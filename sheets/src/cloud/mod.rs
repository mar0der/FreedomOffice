//! Microsoft 365 cloud documents: sign-in, OneDrive / SharePoint files and sharing
//! through Microsoft Graph v1.0. No UI here; see `app::cloud_ui`.

pub mod auth;
pub mod graph;

use std::path::{Path, PathBuf};
use std::time::Duration;

pub use graph::{CloudDoc, DriveItem, Kind, Permission, Session, Target, UploadError, User};

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

    pub fn get_header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
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
        let body = res.body_mut().with_config().limit(2 * 1024 * 1024 * 1024).read_to_vec().map_err(net_error)?;
        Ok(Response { status, body })
    }
}

fn net_error(e: ureq::Error) -> String {
    format!("Couldn't reach the server. Check your internet connection. ({e})")
}

// ---------------------------------------------------------------------------
// Settings and local folders
// ---------------------------------------------------------------------------

fn roaming_dir() -> Option<PathBuf> {
    std::env::var("APPDATA").ok().map(|a| PathBuf::from(a).join("FreedomOffice"))
}

fn local_dir() -> Option<PathBuf> {
    std::env::var("LOCALAPPDATA").ok().map(|a| PathBuf::from(a).join("FreedomOffice"))
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
    let path = config_path().ok_or("APPDATA isn't set.")?;
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
