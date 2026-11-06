//! Cloud documents: Microsoft 365 (OneDrive / SharePoint through Microsoft Graph v1.0) and
//! self-hosted FreedomOffice servers. No UI here; see `app::cloud_ui`.

pub mod auth;
pub mod graph;
pub mod server;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use graph::{CloudDoc, DriveItem, Kind, Permission, Session, Target, UploadError, User};
pub use server::ServerSession;

/// Where a cloud document lives.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Provider {
    #[default]
    Microsoft,
    /// A FreedomOffice server, by base URL (`https://files.example.com`).
    FreedomServer(String),
}

/// A signed-in connection to either provider: the operations Open, Save, Save As and the
/// conflict dialog share. Sharing and version history differ and are called on the session.
#[derive(Clone)]
pub enum Conn {
    Microsoft(Arc<Session>),
    Server(Arc<ServerSession>),
}

impl Conn {
    pub fn provider(&self) -> Provider {
        match self {
            Conn::Microsoft(_) => Provider::Microsoft,
            Conn::Server(s) => s.provider(),
        }
    }

    pub fn list(&self, target: &Target) -> Result<Vec<DriveItem>, String> {
        match self {
            Conn::Microsoft(s) => s.list(target),
            Conn::Server(s) => s.list(target),
        }
    }

    /// Where clicking a row navigates (`None` for files).
    pub fn target_of(&self, item: &DriveItem) -> Option<Target> {
        match self {
            Conn::Microsoft(_) => item.target(),
            Conn::Server(_) => (item.kind != Kind::File).then(|| Target::Folder(item.id.clone())),
        }
    }

    /// The folder reference new files are uploaded into, for a listed place or folder.
    pub fn folder_of(&self, target: &Target) -> Option<String> {
        match (self, target) {
            (Conn::Microsoft(_), t) => t.folder_path(),
            (Conn::Server(_), Target::MyFiles) => Some("root".into()),
            (Conn::Server(_), Target::Folder(id)) => Some(id.clone()),
            (Conn::Server(_), _) => None,
        }
    }

    pub fn open(&self, item: &DriveItem, root: &Path) -> Result<CloudDoc, String> {
        match self {
            Conn::Microsoft(s) => s.open(item, root),
            Conn::Server(s) => s.open(item, root),
        }
    }

    /// Saves over the document; with `etag`, `Conflict` when someone else saved.
    pub fn upload(&self, doc: &CloudDoc, etag: Option<&str>, data: &[u8]) -> Result<DriveItem, UploadError> {
        match self {
            Conn::Microsoft(s) => s.upload(&doc.item_path(), etag, data).map(|i| with_drive(s, i)),
            Conn::Server(s) => s.upload(&doc.item_id, etag, data),
        }
    }

    /// Creates a file in a folder. `behavior`: "fail", "rename" or "replace".
    pub fn upload_new(&self, folder: &str, name: &str, data: &[u8], behavior: &str) -> Result<DriveItem, UploadError> {
        match self {
            Conn::Microsoft(s) => s.upload_new(folder, name, data, behavior).map(|i| with_drive(s, i)),
            Conn::Server(s) => s.upload_new(folder, name, data, behavior),
        }
    }

    /// The document record for an uploaded item.
    pub fn doc_from(&self, item: &DriveItem, local: PathBuf) -> CloudDoc {
        match self {
            Conn::Microsoft(_) => CloudDoc::from_item(item, local),
            Conn::Server(s) => server::doc_from_item(&s.account.url, item, local),
        }
    }

    /// Where downloaded documents are kept locally.
    pub fn cache_root(&self) -> Option<PathBuf> {
        match self {
            Conn::Microsoft(_) => cache_dir(),
            Conn::Server(s) => server::cache_root(&s.account.url),
        }
    }
}

/// Fills in the drive id when a Graph upload answer omits it.
fn with_drive(session: &Session, item: DriveItem) -> DriveItem {
    if !item.drive_id.is_empty() || item.id.is_empty() {
        return item;
    }
    session.item(&format!("/me/drive/items/{}", item.id)).unwrap_or(item)
}

/// "OneDrive" or the server's host name, for status messages and the title bar.
pub fn place_name(p: &Provider) -> String {
    match p {
        Provider::Microsoft => "OneDrive".into(),
        Provider::FreedomServer(url) => server::host_of(url),
    }
}

/// File types offered when browsing OneDrive and SharePoint.
pub const SPREADSHEET_EXTS: &[&str] = &["xlsx", "xlsm", "xls", "xlsb", "ods", "csv"];

pub fn is_spreadsheet(name: &str) -> bool {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    SPREADSHEET_EXTS.contains(&ext.as_str())
}

// ---------------------------------------------------------------------------
// HTTP transport (a trait so tests can fake the network)
// ---------------------------------------------------------------------------

pub struct Request {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn new(method: &'static str, url: impl Into<String>) -> Self {
        Request { method, url: url.into(), headers: Vec::new(), body: Vec::new() }
    }

    pub fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    pub fn json(mut self, v: &serde_json::Value) -> Self {
        self.headers.push(("Content-Type", "application/json".into()));
        self.body = v.to_string().into_bytes();
        self
    }

    pub fn form(mut self, pairs: &[(&str, &str)]) -> Self {
        self.headers.push(("Content-Type", "application/x-www-form-urlencoded".into()));
        self.body = form_encode(pairs).into_bytes();
        self
    }

    #[cfg(test)]
    pub fn get_header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    /// Response headers (lower-case names).
    pub headers: Vec<(String, String)>,
}

impl Response {
    #[cfg(test)]
    pub fn new(status: u16, body: Vec<u8>) -> Self {
        Response { status, body, headers: Vec::new() }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json(&self) -> Result<serde_json::Value, String> {
        serde_json::from_slice(&self.body).map_err(|e| format!("Unexpected response from the server ({e})."))
    }

    /// Human-readable error from a Graph (`error.message`) or identity (`error_description`) response.
    pub fn error_message(&self) -> String {
        let v: serde_json::Value = serde_json::from_slice(&self.body).unwrap_or_default();
        let msg = v["error"]["message"].as_str().or(v["error_description"].as_str()).or(v["error"].as_str()).unwrap_or("");
        // Identity errors carry trace and correlation ids after the first line.
        let msg = msg.lines().next().unwrap_or("").trim();
        if msg.is_empty() { format!("The server answered with HTTP {}.", self.status) } else { msg.to_string() }
    }
}

pub trait Transport: Send + Sync {
    fn send(&self, req: Request) -> Result<Response, String>;
}

/// HTTPS through ureq with the Windows TLS stack (schannel) and the system certificate store.
pub struct Http {
    agent: ureq::Agent,
}

impl Http {
    pub fn new() -> Self {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_global(Some(Duration::from_secs(600)))
            .user_agent(concat!("FreedomOfficeSheets/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Http { agent }
    }
}

impl Transport for Http {
    fn send(&self, req: Request) -> Result<Response, String> {
        let mut b = ureq::http::Request::builder().method(req.method).uri(&req.url);
        for (k, v) in &req.headers {
            b = b.header(*k, v);
        }
        let res = if req.body.is_empty() && matches!(req.method, "GET" | "DELETE") {
            self.agent.run(b.body(()).map_err(|e| e.to_string())?)
        } else {
            self.agent.run(b.body(req.body).map_err(|e| e.to_string())?)
        };
        let mut res = res.map_err(net_error)?;
        let status = res.status().as_u16();
        let headers = res
            .headers()
            .iter()
            .filter(|(k, _)| k.as_str().starts_with("x-") || *k == "etag")
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let body = res.body_mut().with_config().limit(2 * 1024 * 1024 * 1024).read_to_vec().map_err(net_error)?;
        Ok(Response { status, body, headers })
    }
}

fn net_error(e: ureq::Error) -> String {
    format!("Couldn't reach the server. Check your internet connection. ({e})")
}

// ---------------------------------------------------------------------------
// Settings and local folders
// ---------------------------------------------------------------------------

/// An absolute directory from an environment variable.
fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute())
}

/// Per-user settings: `%APPDATA%\FreedomOffice`, `~/.config/freedomoffice` (XDG),
/// `~/Library/Application Support/FreedomOffice` (macOS).
pub(crate) fn roaming_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env_dir("APPDATA").map(|a| a.join("FreedomOffice"))
    } else if cfg!(target_os = "macos") {
        env_dir("HOME").map(|h| h.join("Library/Application Support/FreedomOffice"))
    } else {
        env_dir("XDG_CONFIG_HOME").or_else(|| env_dir("HOME").map(|h| h.join(".config"))).map(|d| d.join("freedomoffice"))
    }
}

/// Per-user data that stays on this machine: `%LOCALAPPDATA%\FreedomOffice`,
/// `~/.local/share/freedomoffice` (XDG), `~/Library/Application Support/FreedomOffice` (macOS).
pub(crate) fn local_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env_dir("LOCALAPPDATA").map(|a| a.join("FreedomOffice"))
    } else if cfg!(target_os = "macos") {
        roaming_dir()
    } else {
        env_dir("XDG_DATA_HOME").or_else(|| env_dir("HOME").map(|h| h.join(".local/share"))).map(|d| d.join("freedomoffice"))
    }
}

pub(crate) const NO_SETTINGS_DIR: &str = "The folder for FreedomOffice settings couldn't be found (APPDATA / HOME isn't set).";

/// Writes a file only the current user can read (sign-in tokens). On Windows the content
/// is already DPAPI-encrypted; elsewhere the file is created with mode 0600.
pub(crate) fn write_private(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let tmp = path.with_extension("tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).map_err(|e| e.to_string())?;
        f.write_all(data).and_then(|_| f.sync_all()).map_err(|e| e.to_string())?;
        drop(f);
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data).map_err(|e| e.to_string())
    }
}

/// `%APPDATA%\FreedomOffice\cloud.json`
pub fn config_path() -> Option<PathBuf> {
    roaming_dir().map(|d| d.join("cloud.json"))
}

/// `%LOCALAPPDATA%\FreedomOffice\cloud-token.bin`
pub fn token_path() -> Option<PathBuf> {
    local_dir().map(|d| d.join("cloud-token.bin"))
}

/// `%LOCALAPPDATA%\FreedomOffice\Cloud` (local copies of cloud documents).
pub fn cache_dir() -> Option<PathBuf> {
    local_dir().map(|d| d.join("Cloud"))
}

/// The app registration's client ID: env `FREEDOMOFFICE_CLIENT_ID`, else `cloud.json`.
pub fn client_id() -> Option<String> {
    if let Ok(id) = std::env::var("FREEDOMOFFICE_CLIENT_ID") {
        if !id.trim().is_empty() {
            return Some(id.trim().to_string());
        }
    }
    let text = std::fs::read_to_string(config_path()?).ok()?;
    parse_client_id(&text)
}

pub fn parse_client_id(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v["client_id"].as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// A client ID is a GUID like `00000000-0000-0000-0000-000000000000`.
pub fn valid_client_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('-').collect();
    parts.len() == 5
        && parts.iter().zip([8, 4, 4, 4, 12]).all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

pub fn save_client_id(id: &str) -> Result<(), String> {
    let path = config_path().ok_or(NO_SETTINGS_DIR)?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(&serde_json::json!({ "client_id": id.trim() })).unwrap_or_default();
    std::fs::write(&path, text).map_err(|e| e.to_string())
}

/// Where a downloaded cloud item is kept: `Cloud\<item-id>\<name>`.
pub fn local_copy_path(root: &Path, item_id: &str, name: &str) -> PathBuf {
    root.join(safe_file_name(item_id)).join(safe_file_name(name))
}

fn safe_file_name(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    let t = t.trim_matches(|c| c == '.' || c == ' ').to_string();
    if t.is_empty() { "_".into() } else { t }
}

// ---------------------------------------------------------------------------
// Encoding helpers
// ---------------------------------------------------------------------------

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                Ok(v) => {
                    out.push(v);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", pct_encode(k), pct_encode(v))).collect::<Vec<_>>().join("&")
}

/// Opens a web page in the default browser.
pub fn open_in_browser(url: &str) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn private_files_are_user_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("fo-private-{}", std::process::id()));
        let p = dir.join("sub").join("servers.bin");
        write_private(&p, b"one").unwrap();
        write_private(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn settings_dirs_follow_xdg() {
        // Only checks the shape (the environment is shared with other tests).
        let r = roaming_dir().unwrap();
        let l = local_dir().unwrap();
        assert!(r.ends_with("freedomoffice") && l.ends_with("freedomoffice"));
        assert_ne!(r, l);
    }

    #[test]
    fn spreadsheet_filter() {
        assert!(is_spreadsheet("Budget.XLSX"));
        assert!(is_spreadsheet("a.b.csv"));
        assert!(!is_spreadsheet("Notes.docx"));
        assert!(!is_spreadsheet("xlsx"));
    }

    #[test]
    fn percent_encoding_round_trip() {
        assert_eq!(pct_encode("Q1 Report (copy).xlsx"), "Q1%20Report%20%28copy%29.xlsx");
        assert_eq!(pct_decode("a%20b+c%2Fd"), "a b c/d");
        assert_eq!(pct_decode("100%"), "100%");
        assert_eq!(form_encode(&[("a", "1 2"), ("b", "x&y")]), "a=1%202&b=x%26y");
    }

    #[test]
    fn client_id_config() {
        assert_eq!(parse_client_id(r#"{"client_id": " 11111111-2222-3333-4444-555555555555 "}"#).as_deref(), Some("11111111-2222-3333-4444-555555555555"));
        assert_eq!(parse_client_id(r#"{"client_id": ""}"#), None);
        assert_eq!(parse_client_id("not json"), None);
        assert!(valid_client_id("11111111-2222-3333-4444-555555555555"));
        assert!(!valid_client_id("11111111-2222-3333-4444-55555555555"));
        assert!(!valid_client_id("hello"));
    }

    #[test]
    fn local_copy_names_are_safe() {
        let p = local_copy_path(Path::new("C:\\c"), "01AB!12", "a:b?.xlsx");
        assert_eq!(p, Path::new("C:\\c").join("01AB!12").join("a_b_.xlsx"));
    }
}
