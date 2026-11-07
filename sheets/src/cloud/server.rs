//! FreedomOffice Server (self-hosted document server): sign-in with a device token, browsing,
//! open and save with If-Match, sharing with server users, share links and version history.
//! The password is sent once to the server to get a device token; only the token is kept,
//! encrypted for the Windows user (`%LOCALAPPDATA%\FreedomOffice\servers.bin`); on Linux and
//! macOS in a file only the user can read (`~/.local/share/freedomoffice/servers.bin`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{json, Value};

use super::{pct_encode, CloudDoc, DriveItem, Kind, Permission, Provider, Request, Response, Target, Transport, UploadError};

/// A signed-in server account (the token is a device token, never the password).
#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub url: String,
    pub username: String,
    pub display_name: String,
    pub token: String,
}

/// An organisation the signed-in person belongs to (from `/api/me`).
#[derive(Clone, Debug, PartialEq)]
pub struct OrgInfo {
    pub id: String,
    pub name: String,
    /// The organisation's root folder.
    pub root: String,
    /// The person's role in it: "owner", "admin" or "member".
    pub role: String,
}

impl OrgInfo {
    /// Owners and admins manage the organisation (on the server's admin page).
    pub fn manages(&self) -> bool {
        matches!(self.role.as_str(), "owner" | "admin")
    }
}

/// The signed-in person as the server sees them (`/api/me`).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Me {
    pub username: String,
    pub display_name: String,
    pub is_admin: bool,
    /// Signed in with a temporary password: every other request answers
    /// `password_change_required` until a new one is chosen.
    pub must_change_password: bool,
    pub orgs: Vec<OrgInfo>,
}

impl Me {
    pub fn from_json(v: &Value) -> Self {
        let orgs = v["orgs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|o| OrgInfo { id: s(&o["id"]), name: s(&o["name"]), root: s(&o["root"]), role: s(&o["role"]) })
                    .filter(|o| !o.id.is_empty() && !o.root.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Me {
            username: s(&v["username"]),
            display_name: s(&v["display_name"]),
            is_admin: v["is_admin"].as_bool().unwrap_or(false),
            must_change_password: v["must_change_password"].as_bool().unwrap_or(false),
            orgs,
        }
    }

    /// May use the server's admin page: the server administrator, or an owner or admin of an
    /// organisation.
    pub fn can_manage(&self) -> bool {
        self.is_admin || self.orgs.iter().any(OrgInfo::manages)
    }
}

/// The 403 code of requests made before a temporary password was replaced.
pub const PASSWORD_CHANGE_REQUIRED: &str = "password_change_required";

/// A version in a file's history.
#[derive(Clone, Debug, PartialEq)]
pub struct VersionInfo {
    pub version: i64,
    pub size: u64,
    pub modified: String,
    pub modified_by: String,
    pub current: bool,
    /// A name given to the version ("" = none); named versions are never pruned.
    pub label: String,
    /// People whose live changes are in this version (live snapshots).
    pub contributors: Vec<String>,
}

/// Checks a server address typed by the user: https:// only (http:// for localhost testing).
/// Returns the base URL without a trailing slash.
pub fn normalize_url(input: &str) -> Result<String, String> {
    let t = input.trim();
    if t.is_empty() {
        return Err("Enter the server address, for example https://files.example.com".into());
    }
    let (scheme, rest) = match t.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => ("https".to_string(), t),
    };
    let rest = rest.trim_end_matches('/');
    if rest.contains(['?', '#', ' ', '\\', '@']) {
        return Err("That doesn't look like a server address.".into());
    }
    let host = host_part(rest);
    if host.is_empty() {
        return Err("That doesn't look like a server address.".into());
    }
    match scheme.as_str() {
        "https" => Ok(format!("https://{rest}")),
        "http" if is_local(&host) => Ok(format!("http://{rest}")),
        "http" => Err("For your security, FreedomOffice Sheets connects to servers only over HTTPS (https://). \
                       Unencrypted http:// is allowed only for localhost."
            .into()),
        _ => Err("The server address must start with https://".into()),
    }
}

fn host_part(rest: &str) -> String {
    let authority = rest.split('/').next().unwrap_or("");
    let host = if let Some(v6) = authority.strip_prefix('[') { format!("[{}", v6.split(']').next().unwrap_or("")) + "]" } else { authority.split(':').next().unwrap_or("").to_string() };
    host.to_ascii_lowercase()
}

fn is_local(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

/// "https://files.example.com/x" -> "files.example.com"; ports are kept ("localhost:8080").
pub fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.split('/').next().unwrap_or(rest).to_string()
}

/// Local folder for documents of a server: `Server\<host>` (":" becomes "_").
pub fn cache_root(url: &str) -> Option<PathBuf> {
    super::local_dir().map(|d| d.join("Server").join(super::safe_file_name(&host_of(url).replace(':', "_"))))
}

pub fn accounts_path() -> Option<PathBuf> {
    super::local_dir().map(|d| d.join("servers.bin"))
}

/// How this device is listed among the account's sign-ins: "FreedomOffice Sheets on <computer>".
pub fn device_name() -> String {
    let pc = computer_name().unwrap_or_else(|| if cfg!(windows) { "this PC".into() } else { "this computer".into() });
    format!("FreedomOffice Sheets on {pc}")
}

/// The computer's name: COMPUTERNAME on Windows; elsewhere the host name (/etc/hostname, the
/// kernel's host name, HOSTNAME, then `hostname`, as on macOS).
fn computer_name() -> Option<String> {
    let clean = |s: &str| {
        let t: String = s.trim().chars().filter(|c| !c.is_control()).take(64).collect();
        (!t.is_empty()).then_some(t)
    };
    if cfg!(windows) {
        return std::env::var("COMPUTERNAME").ok().and_then(|s| clean(&s));
    }
    ["/etc/hostname", "/proc/sys/kernel/hostname"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok().and_then(|s| clean(s.lines().next().unwrap_or(""))))
        .or_else(|| std::env::var("HOSTNAME").ok().and_then(|s| clean(&s)))
        .or_else(|| std::process::Command::new("hostname").output().ok().and_then(|o| clean(&String::from_utf8_lossy(&o.stdout))))
}

fn accounts_json(accounts: &[Account]) -> String {
    let list: Vec<Value> = accounts.iter().map(|a| json!({ "url": a.url, "username": a.username, "display_name": a.display_name, "token": a.token })).collect();
    Value::Array(list).to_string()
}

fn parse_accounts(text: &str) -> Vec<Account> {
    let v: Value = serde_json::from_str(text).unwrap_or_default();
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|x| Account { url: s(&x["url"]), username: s(&x["username"]), display_name: s(&x["display_name"]), token: s(&x["token"]) })
                .filter(|a| !a.url.is_empty() && !a.token.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Writes the accounts (with their device tokens) for the current user only (DPAPI on Windows).
pub fn save_accounts_to(path: &Path, accounts: &[Account]) -> Result<(), String> {
    if accounts.is_empty() {
        let _ = std::fs::remove_file(path);
        return Ok(());
    }
    super::write_private(path, &super::auth::dpapi::protect(accounts_json(accounts).as_bytes())?)
}

pub fn load_accounts_from(path: &Path) -> Vec<Account> {
    let Ok(data) = std::fs::read(path) else { return Vec::new() };
    let Ok(plain) = super::auth::dpapi::unprotect(&data) else { return Vec::new() };
    parse_accounts(&String::from_utf8_lossy(&plain))
}

pub fn save_accounts(accounts: &[Account]) -> Result<(), String> {
    save_accounts_to(&accounts_path().ok_or(super::NO_SETTINGS_DIR)?, accounts)
}

pub fn load_accounts() -> Vec<Account> {
    accounts_path().map(|p| load_accounts_from(&p)).unwrap_or_default()
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// A server item as a browser row.
pub fn item_from_json(v: &Value) -> DriveItem {
    DriveItem {
        kind: if v["kind"].as_str() == Some("folder") { Kind::Folder } else { Kind::File },
        id: s(&v["id"]),
        drive_id: String::new(),
        name: s(&v["name"]),
        modified: s(&v["modified"]),
        modified_by: s(&v["modified_by"]),
        web_url: String::new(),
        etag: s(&v["etag"]),
        parent_id: s(&v["parent"]),
        size: v["size"].as_u64().unwrap_or(0),
    }
}

/// The document record for a server file.
pub fn doc_from_item(url: &str, item: &DriveItem, local: PathBuf) -> CloudDoc {
    CloudDoc {
        drive_id: String::new(),
        item_id: item.id.clone(),
        name: item.name.clone(),
        etag: item.etag.clone(),
        web_url: String::new(),
        parent: (!item.parent_id.is_empty()).then(|| item.parent_id.clone()),
        local,
        provider: Provider::FreedomServer(url.to_string()),
        live: None,
    }
}

/// "v12-abc..." -> 12
pub fn version_of_etag(etag: &str) -> Option<i64> {
    etag.trim_matches('"').strip_prefix('v')?.split('-').next()?.parse().ok()
}

/// The `code` of a server error (`{"error": "...", "code": "..."}`), or "".
fn error_code(r: &Response) -> String {
    serde_json::from_slice::<Value>(&r.body).ok().map(|v| s(&v["code"])).unwrap_or_default()
}

fn error_text(r: &Response, host: &str) -> String {
    match r.status {
        401 => format!("Your sign-in to {host} has expired or was revoked. Sign in again (File > Account)."),
        404 => "The item wasn't found. It may have been deleted, or it's no longer shared with you.".into(),
        413 => "The file is larger than the server accepts.".into(),
        429 => "Too many attempts. Try again in 15 minutes.".into(),
        _ => r.error_message(),
    }
}

/// What asking for a document's details found.
#[derive(Debug)]
pub enum Probe {
    Found(DriveItem),
    /// Deleted, or no longer shared with this account.
    Missing,
    Denied,
    /// A network or server problem: nothing is known about the document.
    Failed(String),
}

/// A signed-in connection to a FreedomOffice server. Shared between worker threads.
pub struct ServerSession {
    pub account: Account,
    http: Box<dyn Transport>,
    /// The server asked for a new password (a request answered `password_change_required`, or
    /// `/api/me` / the sign-in said so). The UI shows "Choose a new password" while it is set.
    needs_password: AtomicBool,
}

impl ServerSession {
    pub fn new(account: Account, http: Box<dyn Transport>) -> Self {
        ServerSession { account, http, needs_password: AtomicBool::new(false) }
    }

    /// The server wants a new password before anything else works.
    pub fn needs_password_change(&self) -> bool {
        self.needs_password.load(Ordering::SeqCst)
    }

    /// Forgets the request (the dialog was closed); the next refused request sets it again.
    pub fn dismiss_password_change(&self) {
        self.needs_password.store(false, Ordering::SeqCst);
    }

    /// Signs in with a username and password (sent only to this server) and gets a device token.
    pub fn login(http: Box<dyn Transport>, url: &str, username: &str, password: &str, device: &str) -> Result<Self, String> {
        let url = normalize_url(url)?;
        let req = Request::new("POST", format!("{url}/api/login")).json(&json!({ "username": username.trim(), "password": password, "device": device }));
        let r = http.send(req)?;
        if r.status == 401 {
            return Err("The username or password is incorrect.".into());
        }
        if !r.ok() {
            return Err(error_text(&r, &host_of(&url)));
        }
        let v = r.json()?;
        let token = s(&v["token"]);
        if token.is_empty() || v["user"]["username"].as_str().is_none() {
            return Err("That address didn't answer like a FreedomOffice server.".into());
        }
        let account = Account { url, username: s(&v["user"]["username"]), display_name: s(&v["user"]["display_name"]), token };
        let session = ServerSession::new(account, http);
        session.needs_password.store(v["user"]["must_change_password"].as_bool().unwrap_or(false), Ordering::SeqCst);
        Ok(session)
    }

    pub fn host(&self) -> String {
        host_of(&self.account.url)
    }

    pub fn provider(&self) -> Provider {
        Provider::FreedomServer(self.account.url.clone())
    }

    fn call(&self, mut req: Request) -> Result<Response, String> {
        req.url = format!("{}{}", self.account.url, req.url);
        req.headers.push(("Authorization", format!("Bearer {}", self.account.token)));
        let r = self.http.send(req)?;
        if r.status == 403 && error_code(&r) == PASSWORD_CHANGE_REQUIRED {
            self.needs_password.store(true, Ordering::SeqCst);
        }
        Ok(r)
    }

    fn json_ok(&self, req: Request) -> Result<Value, String> {
        let r = self.call(req)?;
        if !r.ok() {
            return Err(error_text(&r, &self.host()));
        }
        if r.body.is_empty() { Ok(Value::Null) } else { r.json() }
    }

    fn get(&self, path: &str) -> Result<Value, String> {
        self.json_ok(Request::new("GET", path))
    }

    /// Checks the device token; returns the display name (tests).
    #[cfg(test)]
    pub fn me(&self) -> Result<String, String> {
        Ok(self.me_info()?.display_name)
    }

    /// Who is signed in, with their organisations (works with a temporary password too).
    pub fn me_info(&self) -> Result<Me, String> {
        let me = Me::from_json(&self.get("/api/me")?);
        self.needs_password.store(me.must_change_password, Ordering::SeqCst);
        Ok(me)
    }

    /// Replaces the password (also the way out of a temporary one). The server signs the
    /// account's other devices out; this one stays signed in. Returns how many were signed out.
    /// The passwords go only into the request body: never logged, never kept.
    pub fn change_password(&self, old: &str, new: &str) -> Result<u64, String> {
        let r = self.call(Request::new("POST", "/api/me/password").json(&json!({ "old_password": old, "new_password": new })))?;
        if r.ok() {
            self.needs_password.store(false, Ordering::SeqCst);
            return Ok(r.json().ok().and_then(|v| v["signed_out_devices"].as_u64()).unwrap_or(0));
        }
        Err(match (r.status, error_code(&r).as_str()) {
            (403, "wrong_password") => "The current password is incorrect.".into(),
            // The password policy (too short, too common, contains the username...) in the
            // server's words.
            (400, _) | (409, _) => r.error_message(),
            _ => error_text(&r, &self.host()),
        })
    }

    /// Revokes this device's token on the server.
    pub fn logout(&self) -> Result<(), String> {
        self.json_ok(Request::new("POST", "/api/logout")).map(|_| ())
    }

    fn items(&self, path: &str) -> Result<Vec<DriveItem>, String> {
        Ok(self.get(path)?["items"].as_array().map(|a| a.iter().map(item_from_json).collect()).unwrap_or_default())
    }

    /// Lists My files (root), Shared with me, Recent or a folder (`Target::Folder(id)`).
    pub fn list(&self, target: &Target) -> Result<Vec<DriveItem>, String> {
        let mut items = match target {
            Target::MyFiles => {
                let root = s(&self.get("/api/root")?["id"]);
                self.items(&format!("/api/items/{root}/children"))?
            }
            Target::Recent => self.items("/api/recent")?,
            Target::Shared => self.items("/api/shared-with-me")?,
            Target::Folder(id) => self.items(&format!("/api/items/{}/children", pct_encode(id)))?,
            Target::Sites | Target::Site(_) => Vec::new(),
        };
        items.retain(|i| !i.id.is_empty() && (i.kind != Kind::File || super::is_spreadsheet(&i.name)));
        Ok(items)
    }

    pub fn item(&self, id: &str) -> Result<DriveItem, String> {
        Ok(item_from_json(&self.get(&format!("/api/items/{}", pct_encode(id)))?))
    }

    /// Whether the document can still be opened by this account (answers "no longer shared" and
    /// "deleted" differently from a network problem).
    pub fn probe_item(&self, id: &str) -> Probe {
        match self.call(Request::new("GET", format!("/api/items/{}", pct_encode(id)))) {
            Err(e) => Probe::Failed(e),
            Ok(r) if r.ok() => match r.json() {
                Ok(v) => Probe::Found(item_from_json(&v)),
                Err(e) => Probe::Failed(e),
            },
            Ok(r) if r.status == 404 => Probe::Missing,
            Ok(r) if r.status == 403 && error_code(&r) != PASSWORD_CHANGE_REQUIRED => Probe::Denied,
            Ok(r) => Probe::Failed(error_text(&r, &self.host())),
        }
    }

    /// Downloads a file into `root\<item-id>\<name>`. Metadata is read first so a save made in
    /// between surfaces as a conflict on the next save, never a silent overwrite.
    pub fn open(&self, item: &DriveItem, root: &Path) -> Result<CloudDoc, String> {
        let meta = self.item(&item.id)?;
        let local = super::local_copy_path(root, &meta.id, &meta.name);
        let r = self.call(Request::new("GET", format!("/api/items/{}/content", pct_encode(&meta.id))))?;
        if !r.ok() {
            return Err(error_text(&r, &self.host()));
        }
        if let Some(d) = local.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        std::fs::write(&local, &r.body).map_err(|e| format!("Couldn't write {}: {e}", local.display()))?;
        let mut doc = doc_from_item(&self.account.url, &meta, local);
        // The live session this copy can join: the version downloaded and the sequence number
        // it contains (servers without live co-editing send neither).
        let version = r.header("etag").and_then(version_of_etag).or_else(|| version_of_etag(&meta.etag));
        if let (Some(seq), Some(version)) = (r.header("x-live-seq").and_then(|v| v.parse().ok()), version) {
            let mut state = None;
            if r.header("x-live-state").is_some() {
                let s = self.call(Request::new("GET", format!("/api/items/{}/live/state?version={version}", pct_encode(&meta.id))))?;
                if s.ok() {
                    let path = doc.local.with_file_name(format!("{}.live", doc.local.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()));
                    std::fs::write(&path, &s.body).map_err(|e| format!("Couldn't write {}: {e}", path.display()))?;
                    state = Some(path);
                }
            }
            doc.live = Some(super::graph::LiveBase { version, seq, sheets: r.header("x-live-sheets").unwrap_or("").to_string(), state });
            if let Some(e) = r.header("etag") {
                doc.etag = e.trim_matches('"').to_string();
            }
        }
        Ok(doc)
    }

    /// Uploads a live client's copy that contains every change up to `seq` as a new version,
    /// with its lossless live state (for people joining later).
    pub fn live_snapshot(&self, id: &str, seq: u64, sheets: &str, data: &[u8], state: &[u8]) -> Result<DriveItem, String> {
        let mut req = Request::new("POST", format!("/api/items/{}/live/snapshot?seq={seq}", pct_encode(id)))
            .header("Content-Type", "application/octet-stream")
            .header("X-Live-Sheets", sheets)
            .header("X-Live-Proto", cosync::PROTO.to_string())
            .header("X-Live-State-Offset", data.len().to_string());
        req.body = [data, state].concat();
        let r = self.call(req)?;
        if !r.ok() {
            return Err(error_text(&r, &self.host()));
        }
        Ok(item_from_json(&r.json()?))
    }

    /// The people who can be mentioned in comments on a document: (user name, display name) of
    /// the members of the organisation it belongs to (none for a personal file).
    pub fn document_members(&self, id: &str) -> Result<Vec<(String, String)>, String> {
        let item = self.get(&format!("/api/items/{}", pct_encode(id)))?;
        let Some(org) = item["org"]["id"].as_str() else { return Ok(Vec::new()) };
        let o = self.get(&format!("/api/orgs/{}", pct_encode(org)))?;
        Ok(o["member_list"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| !m["disabled"].as_bool().unwrap_or(false))
            .filter_map(|m| {
                let user = m["username"].as_str()?.to_string();
                let name = m["display_name"].as_str().filter(|n| !n.is_empty()).unwrap_or(&user).to_string();
                Some((user, name))
            })
            .collect())
    }

    /// The device token (for the live connection's Authorization header).
    pub fn token(&self) -> &str {
        &self.account.token
    }

    fn upload_result(&self, r: Response) -> Result<DriveItem, UploadError> {
        match r.status {
            412 => Err(UploadError::Conflict),
            409 => Err(UploadError::Exists),
            _ if r.ok() => Ok(item_from_json(&r.json()?)),
            _ => Err(UploadError::Failed(error_text(&r, &self.host()))),
        }
    }

    /// Saves new content; with `etag`, fails with `Conflict` if someone else saved in between.
    pub fn upload(&self, id: &str, etag: Option<&str>, data: &[u8]) -> Result<DriveItem, UploadError> {
        let mut req = Request::new("PUT", format!("/api/items/{}/content", pct_encode(id))).header("Content-Type", "application/octet-stream");
        if let Some(e) = etag {
            req = req.header("If-Match", format!("\"{e}\""));
        }
        req.body = data.to_vec();
        let r = self.call(req)?;
        self.upload_result(r)
    }

    /// Creates a file in a folder (`root` = My files). `behavior`: "fail", "rename" or "replace".
    pub fn upload_new(&self, folder: &str, name: &str, data: &[u8], behavior: &str) -> Result<DriveItem, UploadError> {
        let mut req = Request::new("POST", format!("/api/files?parent={}&name={}&conflict={behavior}", pct_encode(folder), pct_encode(name))).header("Content-Type", "application/octet-stream");
        req.body = data.to_vec();
        let r = self.call(req)?;
        self.upload_result(r)
    }

    pub fn create_folder(&self, parent: &str, name: &str) -> Result<DriveItem, String> {
        let parent = if parent == "root" { Value::Null } else { json!(parent) };
        let r = self.call(Request::new("POST", "/api/folders").json(&json!({ "parent": parent, "name": name })))?;
        match r.status {
            409 => Err(format!("A file or folder named '{}' already exists here.", name.trim())),
            _ if r.ok() => Ok(item_from_json(&r.json()?)),
            _ => Err(error_text(&r, &self.host())),
        }
    }

    /// Server users matching `query`: (username, display name).
    pub fn search_users(&self, query: &str) -> Result<Vec<(String, String)>, String> {
        let v = self.get(&format!("/api/users?query={}", pct_encode(query.trim())))?;
        Ok(v["users"].as_array().map(|a| a.iter().map(|u| (s(&u["username"]), s(&u["display_name"]))).collect()).unwrap_or_default())
    }

    pub fn grant(&self, id: &str, username: &str, edit: bool) -> Result<(), String> {
        let body = json!({ "username": username, "role": if edit { "edit" } else { "view" } });
        self.json_ok(Request::new("POST", format!("/api/items/{}/permissions", pct_encode(id))).json(&body)).map(|_| ())
    }

    /// Shares with everyone in an organisation the person belongs to (`org`: its id or name).
    pub fn grant_org(&self, id: &str, org: &str, edit: bool) -> Result<(), String> {
        let body = json!({ "org": org, "role": if edit { "edit" } else { "view" } });
        self.json_ok(Request::new("POST", format!("/api/items/{}/permissions", pct_encode(id))).json(&body)).map(|_| ())
    }

    /// Owner, people and organisations (direct and inherited from folders) and share links, as
    /// share-dialog rows. People rows have the username as id, organisation rows `org:<id>`,
    /// link rows `link:<id>`.
    pub fn permissions(&self, id: &str) -> Result<Vec<Permission>, String> {
        let v = self.get(&format!("/api/items/{}/permissions", pct_encode(id)))?;
        let mut out = Vec::new();
        if v["owner"].is_object() {
            out.push(Permission {
                id: s(&v["owner"]["username"]),
                roles: vec!["owner".into()],
                who: s(&v["owner"]["display_name"]),
                detail: s(&v["owner"]["username"]),
                is_link: false,
                inherited: false,
            });
        }
        for p in v["permissions"].as_array().into_iter().flatten() {
            out.push(Permission {
                id: s(&p["username"]),
                roles: vec![if p["role"].as_str() == Some("edit") { "write".into() } else { "read".into() }],
                who: s(&p["display_name"]),
                detail: s(&p["username"]),
                is_link: false,
                inherited: p["inherited"].as_bool().unwrap_or(false),
            });
        }
        for g in v["orgs"].as_array().into_iter().flatten() {
            out.push(Permission {
                id: format!("org:{}", s(&g["org"]["id"])),
                roles: vec![if g["role"].as_str() == Some("edit") { "write".into() } else { "read".into() }],
                who: format!("Everyone in {}", s(&g["org"]["name"])),
                detail: "Organisation".into(),
                is_link: false,
                inherited: g["inherited"].as_bool().unwrap_or(false),
            });
        }
        // Links are listed for people who can edit; viewers get 403 and just see people.
        let r = self.call(Request::new("GET", format!("/api/items/{}/links", pct_encode(id))))?;
        if r.ok() {
            for l in r.json()?["links"].as_array().into_iter().flatten() {
                let mut detail = match l["expires"].as_str() {
                    Some(e) if l["expired"].as_bool() == Some(true) => format!("Expired {}", super::graph::short_date(e)),
                    Some(e) => format!("Expires {}", super::graph::short_date(e)),
                    None => "Doesn't expire".into(),
                };
                if l["has_password"].as_bool() == Some(true) {
                    detail.push_str(" · password");
                }
                out.push(Permission {
                    id: format!("link:{}", s(&l["id"])),
                    roles: vec![if l["role"].as_str() == Some("edit") { "write".into() } else { "read".into() }],
                    who: "Anyone with the link".into(),
                    detail,
                    is_link: true,
                    inherited: false,
                });
            }
        }
        Ok(out)
    }

    /// Removes a person (`username`), an organisation (`org:<id>`) or a link (`link:<id>`)
    /// from an item (usernames can't contain ':').
    pub fn remove_permission(&self, id: &str, perm_id: &str) -> Result<(), String> {
        let path = if let Some(link) = perm_id.strip_prefix("link:") {
            format!("/api/items/{}/links/{}", pct_encode(id), pct_encode(link))
        } else if let Some(org) = perm_id.strip_prefix("org:") {
            format!("/api/items/{}/permissions/org/{}", pct_encode(id), pct_encode(org))
        } else {
            format!("/api/items/{}/permissions/{}", pct_encode(id), pct_encode(perm_id))
        };
        self.json_ok(Request::new("DELETE", path)).map(|_| ())
    }

    /// Creates a share link and returns its URL (shown only now; the server keeps a hash).
    pub fn create_link(&self, id: &str, edit: bool, expires_days: Option<u32>, password: &str) -> Result<String, String> {
        let mut body = json!({ "role": if edit { "edit" } else { "view" } });
        if let Some(d) = expires_days.filter(|d| *d > 0) {
            body["expires_days"] = json!(d);
        }
        if !password.is_empty() {
            body["password"] = json!(password);
        }
        let v = self.json_ok(Request::new("POST", format!("/api/items/{}/links", pct_encode(id))).json(&body))?;
        v["url"].as_str().map(str::to_string).ok_or_else(|| "The server didn't return a link.".into())
    }

    pub fn versions(&self, id: &str) -> Result<Vec<VersionInfo>, String> {
        let v = self.get(&format!("/api/items/{}/versions", pct_encode(id)))?;
        Ok(v["versions"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| VersionInfo {
                        version: x["version"].as_i64().unwrap_or(0),
                        size: x["size"].as_u64().unwrap_or(0),
                        modified: s(&x["modified"]),
                        modified_by: s(&x["modified_by"]),
                        current: x["current"].as_bool().unwrap_or(false),
                        label: s(&x["label"]),
                        contributors: x["contributors"].as_array().map(|c| c.iter().map(s).collect()).unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Names a version (empty clears the name); returns the name as stored.
    pub fn label_version(&self, id: &str, version: i64, label: &str) -> Result<String, String> {
        let v = self.json_ok(Request::new("PUT", format!("/api/items/{}/versions/{version}", pct_encode(id))).json(&json!({ "label": label })))?;
        Ok(s(&v["label"]))
    }

    /// Makes an old version the current one (as a new version).
    pub fn restore_version(&self, id: &str, version: i64) -> Result<DriveItem, String> {
        Ok(item_from_json(&self.json_ok(Request::new("POST", format!("/api/items/{}/versions/{version}/restore", pct_encode(id))))?))
    }

    /// Downloads an old version to `dest`.
    pub fn download_version(&self, id: &str, version: i64, dest: &Path) -> Result<(), String> {
        let r = self.call(Request::new("GET", format!("/api/items/{}/content?version={version}", pct_encode(id))))?;
        if !r.ok() {
            return Err(error_text(&r, &self.host()));
        }
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        std::fs::write(dest, &r.body).map_err(|e| format!("Couldn't write {}: {e}", dest.display()))
    }
}

/// "Book.xlsx", 3 -> "Book (version 3).xlsx"
pub fn version_copy_name(name: &str, version: i64) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} (version {version}).{ext}"),
        _ => format!("{name} (version {version})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses() {
        assert_eq!(normalize_url("files.example.com").unwrap(), "https://files.example.com");
        assert_eq!(normalize_url(" HTTPS://files.example.com/ ").unwrap(), "https://files.example.com");
        assert_eq!(normalize_url("https://example.com:8443/office").unwrap(), "https://example.com:8443/office");
        assert_eq!(normalize_url("http://localhost:8080").unwrap(), "http://localhost:8080");
        assert_eq!(normalize_url("http://127.0.0.1:9/").unwrap(), "http://127.0.0.1:9");
        assert_eq!(normalize_url("http://[::1]:9").unwrap(), "http://[::1]:9");
        let e = normalize_url("http://files.example.com").unwrap_err();
        assert!(e.contains("HTTPS"), "{e}");
        assert!(normalize_url("http://localhost.evil.com").is_err());
        assert!(normalize_url("http://127.0.0.1.nip.io").is_err());
        assert!(normalize_url("ftp://x").is_err());
        assert!(normalize_url("").is_err());
        assert!(normalize_url("https://user@host").is_err());
        assert_eq!(host_of("https://files.example.com/office"), "files.example.com");
        assert_eq!(host_of("http://localhost:8080"), "localhost:8080");
        assert_eq!(version_copy_name("Book.xlsx", 3), "Book (version 3).xlsx");
    }

    #[test]
    fn device_names_say_which_computer() {
        let d = device_name();
        assert!(d.starts_with("FreedomOffice Sheets on ") && d.len() > "FreedomOffice Sheets on ".len(), "{d}");
        #[cfg(target_os = "linux")]
        if let Ok(h) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
            assert!(!d.ends_with("this PC") && !d.ends_with("this computer"), "{d}");
            let file = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
            assert!(d.ends_with(h.trim()) || d.ends_with(file.trim()), "{d} vs {h}");
        }
    }

    #[test]
    fn accounts_round_trip_text() {
        let a = vec![Account { url: "https://a.example".into(), username: "ann".into(), display_name: "Ann".into(), token: "t1".into() }];
        assert_eq!(parse_accounts(&accounts_json(&a)), a);
        assert!(parse_accounts("garbage").is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn accounts_file_is_encrypted() {
        let dir = std::env::temp_dir().join(format!("fo-accounts-{}", std::process::id()));
        let path = dir.join("servers.bin");
        let a = vec![Account { url: "https://a.example".into(), username: "ann".into(), display_name: "Ann".into(), token: "device-token-value".into() }];
        save_accounts_to(&path, &a).unwrap();
        let raw = std::fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("device-token-value"));
        assert_eq!(load_accounts_from(&path), a);
        save_accounts_to(&path, &[]).unwrap();
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parses_server_items() {
        let v: Value = serde_json::from_str(
            r#"{"id":"0123456789abcdef0123456789abcdef","parent":"fedcba9876543210fedcba9876543210","name":"Plan.xlsx","kind":"file",
                "size":12,"version":3,"etag":"v3-abc","modified":"2026-11-05T10:00:00Z","modified_by":"Ann Lee","role":"edit"}"#,
        )
        .unwrap();
        let i = item_from_json(&v);
        assert_eq!((i.kind, i.etag.as_str(), i.modified_by.as_str(), i.size), (Kind::File, "v3-abc", "Ann Lee", 12));
        let d = doc_from_item("https://f.example", &i, PathBuf::from("x.xlsx"));
        assert_eq!(d.parent.as_deref(), Some("fedcba9876543210fedcba9876543210"));
        assert_eq!(d.provider, Provider::FreedomServer("https://f.example".into()));
    }
}

/// The real client functions against a real server on 127.0.0.1 (throwaway data and accounts).
#[cfg(test)]
mod e2e {
    use super::*;
    use crate::cloud::{graph, Http};
    use freedomoffice_server::config::Config;
    use freedomoffice_server::store::Store;

    /// Test-only password for the throwaway accounts.
    const PW: &str = "e2e-test-password";

    #[test]
    fn client_against_local_server() {
        let dir = std::env::temp_dir().join(format!("fo-sheets-e2e-{}", freedomoffice_server::util::new_id()));
        let cfg = Config { data_dir: dir.join("data"), versions_to_keep: 10, ..Config::default() };
        {
            let st = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
            let ann = st.create_user("ann", "Ann Lee", "", PW, false).unwrap();
            let ben = st.create_user("ben", "Ben Ode", "", PW, false).unwrap();
            // People find each other in the share dialog when they share an organisation.
            let org = st.create_org("Team", "test").unwrap();
            for u in [&ann, &ben] {
                st.set_member(&org, u, freedomoffice_server::store::OrgRole::Member, "test").unwrap();
            }
        }
        let server = freedomoffice_server::spawn(cfg).unwrap();
        let url = server.url();
        let local = dir.join("local");

        // Sign-in.
        assert_eq!(ServerSession::login(Box::new(Http::new()), &url, "ann", "wrong", "test").err().as_deref(), Some("The username or password is incorrect."));
        let ann = ServerSession::login(Box::new(Http::new()), &url, "ann", PW, &device_name()).unwrap();
        assert_eq!((ann.account.username.as_str(), ann.account.display_name.as_str()), ("ann", "Ann Lee"));
        assert_eq!(ann.me().unwrap(), "Ann Lee");
        let ben = ServerSession::login(Box::new(Http::new()), &url, "ben", PW, "test").unwrap();

        // Save As into My files, then a folder.
        assert!(ann.list(&Target::MyFiles).unwrap().is_empty());
        let folder = ann.create_folder("root", "Finance").unwrap();
        assert_eq!(folder.kind, Kind::Folder);
        assert!(ann.create_folder("root", "finance").unwrap_err().contains("already exists"));
        let created = ann.upload_new(&folder.id, "Budget.xlsx", b"version one", "fail").unwrap();
        assert_eq!(ann.upload_new(&folder.id, "Budget.xlsx", b"x", "fail"), Err(UploadError::Exists));
        let names: Vec<String> = ann.list(&Target::MyFiles).unwrap().into_iter().map(|i| i.name).collect();
        assert_eq!(names, ["Finance"]);
        let files = ann.list(&Target::Folder(folder.id.clone())).unwrap();
        assert_eq!((files.len(), files[0].modified_by.as_str()), (1, "Ann Lee"));

        // Open downloads to <root>\<id>\<name>.
        let mut doc = ann.open(&created, &local).unwrap();
        assert_eq!(doc.local, local.join(&created.id).join("Budget.xlsx"));
        assert_eq!(std::fs::read(&doc.local).unwrap(), b"version one");
        assert_eq!(doc.provider, Provider::FreedomServer(url.clone()));
        assert_eq!(doc.parent.as_deref(), Some(folder.id.as_str()));
        let conn = crate::cloud::Conn::Server(std::sync::Arc::new(ServerSession::new(ann.account.clone(), Box::new(Http::new()))));

        // Save back with If-Match through the provider-neutral connection.
        let saved = conn.upload(&doc, Some(&doc.etag), b"version two").unwrap();
        doc.update_from(&saved);
        assert!(doc.etag.starts_with("v2-"));

        // Shared editing: Ben can't see it until it's shared; then his save makes Ann's stale.
        assert!(ben.list(&Target::Shared).unwrap().is_empty());
        assert!(ben.item(&created.id).is_err());
        ann.grant(&folder.id, "ben", true).unwrap();
        let shared = ben.list(&Target::Shared).unwrap();
        assert_eq!(shared[0].name, "Finance");
        let ben_doc = ben.open(&ben.list(&Target::Folder(folder.id.clone())).unwrap()[0], &dir.join("ben")).unwrap();
        ben.upload(&ben_doc.item_id, Some(&ben_doc.etag), b"ben's version").unwrap();
        assert_eq!(conn.upload(&doc, Some(&doc.etag), b"ann's version"), Err(UploadError::Conflict));

        // Conflict dialog: Save a Copy next to the original, or Overwrite.
        let copy = conn.upload_new(doc.parent.as_deref().unwrap(), &graph::copy_name(&doc.name), b"ann's version", "rename").unwrap();
        assert_eq!(copy.name, "Budget (copy).xlsx");
        let over = conn.upload(&doc, None, b"ann's version").unwrap();
        assert!(over.etag.starts_with("v4-"));

        // Sharing dialog data.
        assert_eq!(ann.search_users("be").unwrap(), [("ben".to_string(), "Ben Ode".to_string())]);
        let url_link = ann.create_link(&created.id, false, Some(7), "link-password").unwrap();
        assert!(url_link.starts_with(&format!("{url}/l/")));
        let perms = ann.permissions(&created.id).unwrap();
        let rows: Vec<(&str, &str, bool)> = perms.iter().map(|p| (p.who.as_str(), p.role_label(), p.removable())).collect();
        assert_eq!(rows, [("Ann Lee", "Owner", false), ("Ben Ode", "Can edit", false), ("Anyone with the link", "Can view", true)]);
        assert!(perms[2].detail.starts_with("Expires ") && perms[2].detail.ends_with("password"));
        ann.remove_permission(&created.id, &perms[2].id).unwrap();
        assert_eq!(ann.permissions(&created.id).unwrap().len(), 2);
        ann.remove_permission(&folder.id, "ben").unwrap();
        assert!(ben.item(&created.id).is_err());

        // Version history: list, open a copy, restore.
        let versions = ann.versions(&created.id).unwrap();
        assert_eq!(versions.iter().map(|v| v.version).collect::<Vec<_>>(), [4, 3, 2, 1]);
        assert!(versions[0].current);
        assert_eq!(versions[1].modified_by, "Ben Ode");
        let old = local.join("v1.xlsx");
        ann.download_version(&created.id, 1, &old).unwrap();
        assert_eq!(std::fs::read(&old).unwrap(), b"version one");
        let restored = ann.restore_version(&created.id, 1).unwrap();
        assert!(restored.etag.starts_with("v5-"));
        assert_eq!(std::fs::read(ann.open(&restored, &local).unwrap().local).unwrap(), b"version one");

        // Recent, sign-out revokes the device token.
        assert!(ann.list(&Target::Recent).unwrap().iter().any(|i| i.id == created.id));
        ann.logout().unwrap();
        assert!(ann.me().unwrap_err().contains("expired or was revoked"));
        drop(server);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Organisation folders, sharing with a whole organisation, and temporary passwords.
    #[test]
    fn organisations_and_passwords() {
        use freedomoffice_server::store::OrgRole;
        let dir = std::env::temp_dir().join(format!("fo-sheets-orgs-{}", freedomoffice_server::util::new_id()));
        let cfg = Config { data_dir: dir.join("data"), ..Config::default() };
        let (org_id, org_root) = {
            let st = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
            let ann = st.create_user("ann", "Ann Lee", "", PW, false).unwrap();
            let ben = st.create_user("ben", "Ben Ode", "", PW, false).unwrap();
            st.create_user("cam", "Cam Out", "", PW, false).unwrap();
            // Dan got a temporary password from an admin.
            let dan = st.create_account("dan", "Dan New", "", "temporary-password-20", false, true).unwrap();
            let org = st.create_org("Lab Team", "test").unwrap();
            st.set_member(&org, &ann, OrgRole::Owner, "test").unwrap();
            st.set_member(&org, &ben, OrgRole::Member, "test").unwrap();
            st.set_member(&org, &dan, OrgRole::Member, "test").unwrap();
            (org.id.clone(), org.root_id.clone())
        };
        let server = freedomoffice_server::spawn(cfg).unwrap();
        let url = server.url();
        let login = |u: &str, pw: &str| ServerSession::login(Box::new(Http::new()), &url, u, pw, "test").unwrap();
        let (ann, ben, cam) = (login("ann", PW), login("ben", PW), login("cam", PW));

        // /api/me lists the organisations with their folders and the person's role.
        let me = ann.me_info().unwrap();
        assert_eq!(me.orgs, [OrgInfo { id: org_id.clone(), name: "Lab Team".into(), root: org_root.clone(), role: "owner".into() }]);
        assert!(me.can_manage() && !me.must_change_password);
        assert!(!ben.me_info().unwrap().can_manage(), "members don't manage");
        assert!(cam.me_info().unwrap().orgs.is_empty());

        // Saving into and opening from the organisation's folder.
        let target = Target::Folder(org_root.clone());
        assert!(ann.list(&target).unwrap().is_empty());
        let conn = crate::cloud::Conn::Server(std::sync::Arc::new(ServerSession::new(ann.account.clone(), Box::new(Http::new()))));
        assert_eq!(conn.folder_of(&target).as_deref(), Some(org_root.as_str()), "Save As can save there");
        let f = conn.upload_new(&org_root, "Lab.xlsx", b"lab data", "fail").unwrap();
        let listed = ben.list(&target).unwrap();
        assert_eq!(listed.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["Lab.xlsx"], "members see it");
        let doc = ben.open(&listed[0], &dir.join("ben")).unwrap();
        assert_eq!((std::fs::read(&doc.local).unwrap(), doc.parent.as_deref()), (b"lab data".to_vec(), Some(org_root.as_str())));
        assert!(cam.item(&f.id).is_err() && cam.list(&target).is_err(), "outsiders don't");

        // Sharing a personal file with the whole organisation, as view; then removing it.
        let mine = ann.upload_new("root", "Mine.xlsx", b"mine", "fail").unwrap();
        assert!(ben.item(&mine.id).is_err());
        ann.grant_org(&mine.id, &org_id, false).unwrap();
        assert_eq!(ben.item(&mine.id).unwrap().name, "Mine.xlsx");
        assert!(ben.upload(&mine.id, None, b"ben's").is_err(), "view only");
        let perms = ann.permissions(&mine.id).unwrap();
        let org_row = perms.iter().find(|p| p.id == format!("org:{org_id}")).expect("the org grant is listed");
        assert_eq!((org_row.who.as_str(), org_row.role_label(), org_row.removable()), ("Everyone in Lab Team", "Can view", true));
        ann.grant_org(&mine.id, "Lab Team", true).unwrap();
        assert!(ann.permissions(&mine.id).unwrap().iter().any(|p| p.id == org_row.id && p.role_label() == "Can edit"), "by name too, now edit");
        assert!(cam.grant_org(&cam.upload_new("root", "C.xlsx", b"c", "fail").unwrap().id, &org_id, true).is_err(), "only members share with an org");
        ann.remove_permission(&mine.id, &org_row.id).unwrap();
        assert!(ben.item(&mine.id).is_err());
        assert!(!ann.permissions(&mine.id).unwrap().iter().any(|p| p.id.starts_with("org:")));

        // People search: people sharing an organisation, plus exact usernames.
        assert_eq!(ann.search_users("be").unwrap(), [("ben".to_string(), "Ben Ode".to_string())]);
        assert!(ann.search_users("ca").unwrap().is_empty());
        assert_eq!(ann.search_users("cam").unwrap(), [("cam".to_string(), "Cam Out".to_string())]);

        // A temporary password: the sign-in says so, every request is refused until it changes.
        let dan = login("dan", "temporary-password-20");
        assert!(dan.needs_password_change());
        dan.dismiss_password_change();
        let e = dan.list(&Target::MyFiles).unwrap_err();
        assert!(e.contains("new password"), "{e}");
        assert!(dan.needs_password_change(), "a refused request asks for a new password");
        assert!(dan.me_info().unwrap().must_change_password);
        assert_eq!(dan.change_password("wrong-password-123", "a-much-better-passphrase-77").unwrap_err(), "The current password is incorrect.");
        let weak = dan.change_password("temporary-password-20", "short").unwrap_err();
        assert!(weak.contains("12"), "the server's policy message is shown: {weak}");
        let common = dan.change_password("temporary-password-20", "Password2024!").unwrap_err();
        assert!(!common.is_empty() && common != weak, "{common}");
        assert!(dan.needs_password_change());
        dan.change_password("temporary-password-20", "a-much-better-passphrase-77").unwrap();
        assert!(!dan.needs_password_change());
        assert!(dan.list(&Target::MyFiles).is_ok(), "this device stays signed in");
        assert!(!dan.me_info().unwrap().must_change_password);
        assert!(ServerSession::login(Box::new(Http::new()), &url, "dan", "a-much-better-passphrase-77", "test").is_ok());
        drop(server);
        let _ = std::fs::remove_dir_all(dir);
    }
}
