//! Microsoft Graph v1.0: the signed-in user, OneDrive / SharePoint browsing, download,
//! upload with eTag checks, and sharing.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

use super::{auth, pct_encode, Request, Response, Transport};

pub const GRAPH: &str = "https://graph.microsoft.com/v1.0";
/// Larger files go through an upload session.
pub const SIMPLE_UPLOAD_MAX: usize = 4 * 1024 * 1024;
/// Upload session chunks must be multiples of 320 KiB.
pub const CHUNK_SIZE: usize = 10 * 320 * 1024;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct User {
    pub name: String,
    pub email: String,
}

impl User {
    pub fn initials(&self) -> String {
        let src = if self.name.trim().is_empty() { &self.email } else { &self.name };
        let mut words = src.split(|c: char| c.is_whitespace() || c == '.' || c == '@').filter(|w| !w.is_empty());
        let first = words.next().and_then(|w| w.chars().next());
        let last = if self.name.trim().is_empty() { None } else { words.last().and_then(|w| w.chars().next()) };
        first.into_iter().chain(last).flat_map(|c| c.to_uppercase()).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Folder,
    /// A SharePoint site (its document libraries are listed inside).
    Site,
    /// A document library (drive).
    Library,
}

/// A row in the cloud browser: a file, folder, site or document library.
#[derive(Clone, Debug, PartialEq)]
pub struct DriveItem {
    pub kind: Kind,
    pub id: String,
    pub drive_id: String,
    pub name: String,
    /// ISO 8601 timestamp.
    pub modified: String,
    pub modified_by: String,
    pub web_url: String,
    pub etag: String,
    pub parent_id: String,
    pub size: u64,
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

fn identity_name(v: &Value) -> String {
    ["user", "group", "siteUser", "application"]
        .iter()
        .map(|k| s(&v[k]["displayName"]))
        .find(|n| !n.is_empty())
        .unwrap_or_default()
}

impl DriveItem {
    /// Parses a driveItem. Items shared from another drive (sharedWithMe, recent) carry the real
    /// ids in `remoteItem`.
    pub fn from_json(v: &Value) -> Self {
        let r = if v["remoteItem"].is_object() { &v["remoteItem"] } else { v };
        let pick = |key: &str| {
            let a = s(&r[key]);
            if a.is_empty() { s(&v[key]) } else { a }
        };
        let modified_by = {
            let a = identity_name(&r["lastModifiedBy"]);
            if a.is_empty() { identity_name(&v["lastModifiedBy"]) } else { a }
        };
        let folder = r["folder"].is_object() || v["folder"].is_object() || r["package"]["type"].as_str() == Some("oneNote");
        DriveItem {
            kind: if folder { Kind::Folder } else { Kind::File },
            id: pick("id"),
            drive_id: s(&r["parentReference"]["driveId"]).pipe_or(|| s(&v["parentReference"]["driveId"])),
            name: s(&v["name"]).pipe_or(|| s(&r["name"])),
            modified: pick("lastModifiedDateTime"),
            modified_by,
            web_url: pick("webUrl"),
            etag: pick("eTag"),
            parent_id: s(&r["parentReference"]["id"]),
            size: r["size"].as_u64().or(v["size"].as_u64()).unwrap_or(0),
        }
    }

    pub fn site_from_json(v: &Value) -> Self {
        let name = s(&v["displayName"]).pipe_or(|| s(&v["name"]));
        DriveItem { kind: Kind::Site, id: s(&v["id"]), name, web_url: s(&v["webUrl"]), modified: s(&v["lastModifiedDateTime"]), ..Self::empty() }
    }

    pub fn library_from_json(v: &Value) -> Self {
        DriveItem {
            kind: Kind::Library,
            id: s(&v["id"]),
            drive_id: s(&v["id"]),
            name: s(&v["name"]),
            web_url: s(&v["webUrl"]),
            modified: s(&v["lastModifiedDateTime"]),
            modified_by: identity_name(&v["lastModifiedBy"]),
            ..Self::empty()
        }
    }

    fn empty() -> Self {
        DriveItem {
            kind: Kind::File,
            id: String::new(),
            drive_id: String::new(),
            name: String::new(),
            modified: String::new(),
            modified_by: String::new(),
            web_url: String::new(),
            etag: String::new(),
            parent_id: String::new(),
            size: 0,
        }
    }

    /// Graph path of this item, e.g. `/drives/{d}/items/{i}`.
    pub fn item_path(&self) -> String {
        if self.drive_id.is_empty() { format!("/me/drive/items/{}", self.id) } else { format!("/drives/{}/items/{}", self.drive_id, self.id) }
    }

    pub fn parent_path(&self) -> Option<String> {
        (!self.parent_id.is_empty() && !self.drive_id.is_empty()).then(|| format!("/drives/{}/items/{}", self.drive_id, self.parent_id))
    }

    /// Where clicking this row navigates (`None` for files).
    pub fn target(&self) -> Option<Target> {
        match self.kind {
            Kind::File => None,
            Kind::Folder => Some(Target::Folder(self.item_path())),
            Kind::Site => Some(Target::Site(self.id.clone())),
            Kind::Library => Some(Target::Folder(format!("/drives/{}/root", self.id))),
        }
    }
}

trait PipeOr {
    fn pipe_or(self, f: impl FnOnce() -> String) -> String;
}

impl PipeOr for String {
    fn pipe_or(self, f: impl FnOnce() -> String) -> String {
        if self.is_empty() { f() } else { self }
    }
}

/// A place or folder listed by the cloud browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Recent,
    MyFiles,
    Shared,
    Sites,
    /// Document libraries of a site.
    Site(String),
    /// Children of a folder, given as a Graph item path (`/drives/{d}/items/{i}`, `/drives/{d}/root`, `/me/drive/root`).
    Folder(String),
}

impl Target {
    /// Graph folder path that new files can be uploaded into.
    pub fn folder_path(&self) -> Option<String> {
        match self {
            Target::MyFiles => Some("/me/drive/root".into()),
            Target::Folder(p) => Some(p.clone()),
            _ => None,
        }
    }
}

/// A document opened from the cloud.
#[derive(Clone, Debug, PartialEq)]
pub struct CloudDoc {
    pub drive_id: String,
    pub item_id: String,
    pub name: String,
    pub etag: String,
    pub web_url: String,
    /// Graph path of the containing folder (for "Save a Copy").
    pub parent: Option<String>,
    /// The local working copy.
    pub local: PathBuf,
    /// Microsoft 365 or a FreedomOffice server.
    pub provider: super::Provider,
}

impl CloudDoc {
    pub fn from_item(item: &DriveItem, local: PathBuf) -> Self {
        CloudDoc {
            drive_id: item.drive_id.clone(),
            item_id: item.id.clone(),
            name: item.name.clone(),
            etag: item.etag.clone(),
            web_url: item.web_url.clone(),
            parent: item.parent_path(),
            local,
            provider: super::Provider::Microsoft,
        }
    }

    pub fn item_path(&self) -> String {
        format!("/drives/{}/items/{}", self.drive_id, self.item_id)
    }

    /// Takes the new eTag (and any rename) after a successful upload.
    pub fn update_from(&mut self, item: &DriveItem) {
        if !item.etag.is_empty() {
            self.etag = item.etag.clone();
        }
        if !item.web_url.is_empty() {
            self.web_url = item.web_url.clone();
        }
        if !item.name.is_empty() {
            self.name = item.name.clone();
        }
    }
}

/// "Book.xlsx" -> "Book (copy).xlsx"
pub fn copy_name(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} (copy).{ext}"),
        _ => format!("{name} (copy)"),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Permission {
    pub id: String,
    pub roles: Vec<String>,
    pub who: String,
    /// E-mail address or link URL.
    pub detail: String,
    pub is_link: bool,
    pub inherited: bool,
}

impl Permission {
    pub fn from_json(v: &Value) -> Self {
        let roles: Vec<String> = v["roles"].as_array().map(|a| a.iter().map(s).collect()).unwrap_or_default();
        let link = &v["link"];
        let is_link = link.is_object();
        let mut who = identity_name(&v["grantedToV2"]).pipe_or(|| identity_name(&v["grantedTo"]));
        let mut detail = s(&v["grantedToV2"]["user"]["email"]).pipe_or(|| s(&v["invitation"]["email"]));
        if is_link {
            who = match link["scope"].as_str() {
                Some("anonymous") => "Anyone with the link".into(),
                Some("organization") => "People in your organization with the link".into(),
                _ => {
                    let names: Vec<String> = v["grantedToIdentitiesV2"]
                        .as_array()
                        .map(|a| a.iter().map(identity_name).filter(|n| !n.is_empty()).collect())
                        .unwrap_or_default();
                    if names.is_empty() { "Specific people with the link".into() } else { format!("Link: {}", names.join(", ")) }
                }
            };
            detail = s(&link["webUrl"]);
        }
        if who.is_empty() {
            who = detail.clone();
        }
        Permission { id: s(&v["id"]), roles, who, detail, is_link, inherited: v["inheritedFrom"].is_object() }
    }

    pub fn role_label(&self) -> &'static str {
        if self.roles.iter().any(|r| r == "owner") {
            "Owner"
        } else if self.roles.iter().any(|r| r == "write") {
            "Can edit"
        } else {
            "Can view"
        }
    }

    pub fn removable(&self) -> bool {
        !self.inherited && !self.roles.iter().any(|r| r == "owner")
    }
}

#[derive(Debug, PartialEq)]
pub enum UploadError {
    /// HTTP 412: the eTag didn't match (someone else saved).
    Conflict,
    /// HTTP 409: a file with that name already exists.
    Exists,
    Failed(String),
}

impl From<String> for UploadError {
    fn from(e: String) -> Self {
        UploadError::Failed(e)
    }
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UploadError::Conflict => write!(f, "Someone else saved changes to this file."),
            UploadError::Exists => write!(f, "A file with that name already exists."),
            UploadError::Failed(e) => write!(f, "{e}"),
        }
    }
}

fn upload_result(r: Response) -> Result<DriveItem, UploadError> {
    match r.status {
        412 => Err(UploadError::Conflict),
        409 => Err(UploadError::Exists),
        _ if r.ok() => Ok(DriveItem::from_json(&r.json()?)),
        _ => Err(UploadError::Failed(r.error_message())),
    }
}

/// A signed-in connection to Microsoft Graph. Shared between worker threads.
pub struct Session {
    pub client_id: String,
    http: Box<dyn Transport>,
    tokens: Mutex<auth::Tokens>,
    /// Write refreshed refresh tokens to the encrypted cache.
    persist: bool,
}

fn url(path: &str) -> String {
    if path.starts_with("https://") { path.to_string() } else { format!("{GRAPH}{path}") }
}

impl Session {
    pub fn new(client_id: &str, http: Box<dyn Transport>, tokens: auth::Tokens, persist: bool) -> Self {
        Session { client_id: client_id.to_string(), http, tokens: Mutex::new(tokens), persist }
    }

    /// Signs in silently with the cached refresh token.
    pub fn restore(client_id: &str, http: Box<dyn Transport>) -> Result<Option<Self>, String> {
        let Some(rt) = auth::load_refresh_token() else { return Ok(None) };
        let tokens = auth::refresh(http.as_ref(), client_id, &rt)?;
        if tokens.refresh != rt {
            let _ = auth::save_refresh_token(&tokens.refresh);
        }
        Ok(Some(Session::new(client_id, http, tokens, true)))
    }

    /// Interactive sign-in in the browser; caches the refresh token.
    pub fn sign_in(client_id: &str, http: Box<dyn Transport>, cancel: &std::sync::atomic::AtomicBool) -> Result<Self, String> {
        let tokens = auth::sign_in(http.as_ref(), client_id, cancel)?;
        let _ = auth::save_refresh_token(&tokens.refresh);
        Ok(Session::new(client_id, http, tokens, true))
    }

    fn access_token(&self, force: bool) -> Result<String, String> {
        let mut t = self.tokens.lock().map_err(|_| "Sign-in state is unavailable.".to_string())?;
        if force || t.expired(auth::now_secs()) {
            let n = auth::refresh(self.http.as_ref(), &self.client_id, &t.refresh)?;
            if self.persist && n.refresh != t.refresh {
                let _ = auth::save_refresh_token(&n.refresh);
            }
            *t = n;
        }
        Ok(t.access.clone())
    }

    /// Sends an authorized Graph request; refreshes the token and retries once on 401.
    pub fn call(&self, req: Request) -> Result<Response, String> {
        let mut req = req;
        req.url = url(&req.url);
        let auth_req = |r: &Request, token: String| Request {
            method: r.method,
            url: r.url.clone(),
            headers: r.headers.iter().cloned().chain([("Authorization", format!("Bearer {token}"))]).collect(),
            body: r.body.clone(),
        };
        let r = self.http.send(auth_req(&req, self.access_token(false)?))?;
        if r.status == 401 {
            return self.http.send(auth_req(&req, self.access_token(true)?));
        }
        Ok(r)
    }

    pub fn get_json(&self, path: &str) -> Result<Value, String> {
        let r = self.call(Request::new("GET", path))?;
        if !r.ok() {
            return Err(r.error_message());
        }
        r.json()
    }

    /// GETs a collection, following `@odata.nextLink` pages.
    pub fn get_list(&self, path: &str) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        let mut next = Some(path.to_string());
        let mut pages = 0;
        while let Some(p) = next.take() {
            let mut v = self.get_json(&p)?;
            if let Some(a) = v["value"].as_array_mut() {
                out.append(a);
            }
            pages += 1;
            next = v["@odata.nextLink"].as_str().filter(|_| pages < 20).map(str::to_string);
        }
        Ok(out)
    }

    pub fn me(&self) -> Result<User, String> {
        let v = self.get_json("/me")?;
        Ok(User { name: s(&v["displayName"]), email: s(&v["mail"]).pipe_or(|| s(&v["userPrincipalName"])) })
    }

    /// Lists a place or folder: folders, sites, libraries and spreadsheet files.
    pub fn list(&self, target: &Target) -> Result<Vec<DriveItem>, String> {
        let mut items: Vec<DriveItem> = match target {
            Target::Recent => self.get_list("/me/drive/recent")?.iter().map(DriveItem::from_json).collect(),
            Target::MyFiles => self.get_list("/me/drive/root/children")?.iter().map(DriveItem::from_json).collect(),
            Target::Shared => self.get_list("/me/drive/sharedWithMe")?.iter().map(DriveItem::from_json).collect(),
            Target::Sites => self.get_list("/me/followedSites")?.iter().map(DriveItem::site_from_json).collect(),
            Target::Site(id) => self.get_list(&format!("/sites/{id}/drives"))?.iter().map(DriveItem::library_from_json).collect(),
            Target::Folder(p) => self.get_list(&format!("{p}/children"))?.iter().map(DriveItem::from_json).collect(),
        };
        Ok(filter_and_sort(&mut items, target))
    }

    pub fn item(&self, item_path: &str) -> Result<DriveItem, String> {
        Ok(DriveItem::from_json(&self.get_json(item_path)?))
    }

    /// Downloads a file's content into `root\<item-id>\<name>` and returns its document record.
    /// Metadata is read first so a save made in between surfaces as a conflict, never a silent overwrite.
    pub fn open(&self, item: &DriveItem, root: &Path) -> Result<CloudDoc, String> {
        let meta = self.item(&item.item_path())?;
        let meta = DriveItem { drive_id: meta.drive_id.clone().pipe_or(|| item.drive_id.clone()), ..meta };
        let local = super::local_copy_path(root, &meta.id, &meta.name);
        let r = self.call(Request::new("GET", format!("{}/content", meta.item_path())))?;
        if !r.ok() {
            return Err(r.error_message());
        }
        if let Some(d) = local.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        std::fs::write(&local, &r.body).map_err(|e| format!("Couldn't write {}: {e}", local.display()))?;
        Ok(CloudDoc::from_item(&meta, local))
    }

    /// Replaces an existing file's content. With `etag`, fails with `Conflict` if someone else saved.
    pub fn upload(&self, item_path: &str, etag: Option<&str>, data: &[u8]) -> Result<DriveItem, UploadError> {
        if data.len() <= SIMPLE_UPLOAD_MAX {
            let mut req = Request::new("PUT", format!("{item_path}/content")).header("Content-Type", "application/octet-stream");
            if let Some(e) = etag {
                req = req.header("If-Match", e);
            }
            req.body = data.to_vec();
            return upload_result(self.call(req)?);
        }
        let mut req = Request::new("POST", format!("{item_path}/createUploadSession")).json(&json!({ "item": { "@microsoft.graph.conflictBehavior": "replace" } }));
        if let Some(e) = etag {
            req = req.header("If-Match", e);
        }
        self.upload_session(req, data)
    }

    /// Creates a file in a folder. `behavior`: "fail", "rename" or "replace".
    pub fn upload_new(&self, folder_path: &str, name: &str, data: &[u8], behavior: &str) -> Result<DriveItem, UploadError> {
        let path = format!("{folder_path}:/{}:", pct_encode(name));
        if data.len() <= SIMPLE_UPLOAD_MAX {
            let mut req = Request::new("PUT", format!("{path}/content?@microsoft.graph.conflictBehavior={behavior}")).header("Content-Type", "application/octet-stream");
            req.body = data.to_vec();
            return upload_result(self.call(req)?);
        }
        let req = Request::new("POST", format!("{path}/createUploadSession")).json(&json!({ "item": { "@microsoft.graph.conflictBehavior": behavior } }));
        self.upload_session(req, data)
    }

    fn upload_session(&self, create: Request, data: &[u8]) -> Result<DriveItem, UploadError> {
        let r = self.call(create)?;
        if !r.ok() {
            return upload_result(r);
        }
        let upload_url = r.json()?["uploadUrl"].as_str().ok_or_else(|| UploadError::Failed("The upload session had no URL.".into()))?.to_string();
        let total = data.len();
        let mut start = 0;
        loop {
            let end = (start + CHUNK_SIZE).min(total);
            // The upload URL is pre-authenticated: no Authorization header.
            let mut req = Request::new("PUT", upload_url.clone()).header("Content-Range", format!("bytes {start}-{}/{total}", end - 1));
            req.body = data[start..end].to_vec();
            let r = self.http.send(req)?;
            if end >= total || matches!(r.status, 200 | 201) {
                return upload_result(r);
            }
            if r.status != 202 {
                let _ = self.http.send(Request::new("DELETE", upload_url.clone()));
                return Err(UploadError::Failed(r.error_message()));
            }
            start = end;
        }
    }

    pub fn invite(&self, item_path: &str, emails: &[String], edit: bool, message: &str, notify: bool) -> Result<(), String> {
        let mut body = json!({
            "recipients": emails.iter().map(|e| json!({ "email": e })).collect::<Vec<_>>(),
            "requireSignIn": true,
            "sendInvitation": notify,
            "roles": [if edit { "write" } else { "read" }],
        });
        if !message.trim().is_empty() {
            body["message"] = json!(message.trim());
        }
        let r = self.call(Request::new("POST", format!("{item_path}/invite")).json(&body))?;
        if r.ok() { Ok(()) } else { Err(r.error_message()) }
    }

    /// Creates (or returns the existing) sharing link; `anyone` = anonymous, else organization.
    pub fn create_link(&self, item_path: &str, anyone: bool, edit: bool) -> Result<String, String> {
        let body = json!({ "type": if edit { "edit" } else { "view" }, "scope": if anyone { "anonymous" } else { "organization" } });
        let r = self.call(Request::new("POST", format!("{item_path}/createLink")).json(&body))?;
        if !r.ok() {
            return Err(r.error_message());
        }
        r.json()?["link"]["webUrl"].as_str().map(str::to_string).ok_or_else(|| "The server didn't return a link.".into())
    }

    pub fn permissions(&self, item_path: &str) -> Result<Vec<Permission>, String> {
        Ok(self.get_list(&format!("{item_path}/permissions"))?.iter().map(Permission::from_json).collect())
    }

    pub fn delete_permission(&self, item_path: &str, id: &str) -> Result<(), String> {
        let r = self.call(Request::new("DELETE", format!("{item_path}/permissions/{id}")))?;
        if r.ok() { Ok(()) } else { Err(r.error_message()) }
    }
}

fn filter_and_sort(items: &mut Vec<DriveItem>, target: &Target) -> Vec<DriveItem> {
    items.retain(|i| !i.id.is_empty() && (i.kind != Kind::File || super::is_spreadsheet(&i.name)));
    if !matches!(target, Target::Recent | Target::Shared) {
        items.sort_by_key(|i| (i.kind == Kind::File, i.name.to_lowercase()));
    }
    std::mem::take(items)
}

/// "2024-03-05T14:07:00Z" -> "2024-03-05 14:07"
pub fn short_date(iso: &str) -> String {
    match iso.split_once('T') {
        Some((d, t)) => format!("{d} {}", t.get(..5).unwrap_or(t)),
        None => iso.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    type Handler = Box<dyn Fn(&Request) -> Response + Send + Sync>;

    struct Fake {
        log: Mutex<Vec<(String, String, Vec<(&'static str, String)>, usize)>>,
        handler: Handler,
    }

    impl Fake {
        fn new(handler: impl Fn(&Request) -> Response + Send + Sync + 'static) -> Arc<Self> {
            Arc::new(Fake { log: Mutex::new(Vec::new()), handler: Box::new(handler) })
        }

        fn calls(&self) -> Vec<(String, String, Vec<(&'static str, String)>, usize)> {
            self.log.lock().unwrap().clone()
        }
    }

    impl Transport for Arc<Fake> {
        fn send(&self, req: Request) -> Result<Response, String> {
            self.log.lock().unwrap().push((req.method.to_string(), req.url.clone(), req.headers.clone(), req.body.len()));
            Ok((self.handler)(&req))
        }
    }

    fn resp(status: u16, body: &str) -> Response {
        Response { status, body: body.as_bytes().to_vec() }
    }

    fn session(fake: &Arc<Fake>, expires_at: u64) -> Session {
        let t = auth::Tokens { access: "AT".into(), refresh: "RT".into(), expires_at };
        Session::new("cid", Box::new(fake.clone()), t, false)
    }

    const FAR: u64 = u64::MAX / 2;

    #[test]
    fn parses_drive_items() {
        let v: Value = serde_json::from_str(
            r#"{"value":[
              {"id":"F1","name":"Reports","folder":{"childCount":3},"parentReference":{"driveId":"D1","id":"ROOT"},
               "lastModifiedDateTime":"2024-03-05T14:07:00Z","lastModifiedBy":{"user":{"displayName":"Ann Lee"}}},
              {"id":"I2","name":"Budget.xlsx","file":{},"eTag":"\"{ABC},3\"","size":1234,"webUrl":"https://x/Budget.xlsx",
               "parentReference":{"driveId":"D1","id":"F1"},"lastModifiedBy":{"user":{"displayName":"Bob"}}},
              {"id":"I3","name":"Notes.docx","file":{},"parentReference":{"driveId":"D1"}}
            ]}"#,
        )
        .unwrap();
        let mut items: Vec<DriveItem> = v["value"].as_array().unwrap().iter().map(DriveItem::from_json).collect();
        let items = filter_and_sort(&mut items, &Target::MyFiles);
        assert_eq!(items.len(), 2);
        assert_eq!((items[0].kind, items[0].name.as_str(), items[0].modified_by.as_str()), (Kind::Folder, "Reports", "Ann Lee"));
        assert_eq!(items[0].target(), Some(Target::Folder("/drives/D1/items/F1".into())));
        let b = &items[1];
        assert_eq!((b.kind, b.etag.as_str(), b.size, b.drive_id.as_str()), (Kind::File, "\"{ABC},3\"", 1234, "D1"));
        assert_eq!(b.parent_path().as_deref(), Some("/drives/D1/items/F1"));
        assert_eq!(short_date(&items[0].modified), "2024-03-05 14:07");
    }

    #[test]
    fn parses_shared_with_me_remote_items() {
        let v: Value = serde_json::from_str(
            r#"{"id":"LOCAL","name":"Plan.xlsx","remoteItem":{"id":"REMOTE1","name":"Plan.xlsx","file":{},
                "parentReference":{"driveId":"b!remoteDrive","id":"P9"},"webUrl":"https://contoso/Plan.xlsx",
                "lastModifiedDateTime":"2024-01-02T03:04:05Z","lastModifiedBy":{"user":{"displayName":"Cara"}}}}"#,
        )
        .unwrap();
        let i = DriveItem::from_json(&v);
        assert_eq!((i.id.as_str(), i.drive_id.as_str(), i.modified_by.as_str()), ("REMOTE1", "b!remoteDrive", "Cara"));
        assert_eq!(i.item_path(), "/drives/b!remoteDrive/items/REMOTE1");
        assert_eq!(i.web_url, "https://contoso/Plan.xlsx");
    }

    #[test]
    fn parses_sites_and_libraries() {
        let site = DriveItem::site_from_json(&json!({"id":"contoso.sharepoint.com,1,2","displayName":"Finance","webUrl":"https://c/sites/fin"}));
        assert_eq!((site.kind, site.name.as_str()), (Kind::Site, "Finance"));
        assert_eq!(site.target(), Some(Target::Site("contoso.sharepoint.com,1,2".into())));
        let lib = DriveItem::library_from_json(&json!({"id":"b!lib","name":"Documents","driveType":"documentLibrary"}));
        assert_eq!(lib.target(), Some(Target::Folder("/drives/b!lib/root".into())));
        assert_eq!(lib.target().unwrap().folder_path().as_deref(), Some("/drives/b!lib/root"));
        assert_eq!(Target::MyFiles.folder_path().as_deref(), Some("/me/drive/root"));
        assert_eq!(Target::Shared.folder_path(), None);
    }

    #[test]
    fn parses_permissions() {
        let v: Value = serde_json::from_str(
            r#"{"value":[
              {"id":"p0","roles":["owner"],"grantedToV2":{"user":{"displayName":"Me","email":"me@contoso.com"}}},
              {"id":"p1","roles":["write"],"grantedToV2":{"user":{"displayName":"Ann","email":"ann@contoso.com"}}},
              {"id":"p2","roles":["read"],"link":{"scope":"anonymous","type":"view","webUrl":"https://1drv.ms/x"}},
              {"id":"p3","roles":["read"],"invitation":{"email":"guest@example.com"},"inheritedFrom":{"id":"F"}},
              {"id":"p4","roles":["write"],"link":{"scope":"users","type":"edit","webUrl":"https://l"},
               "grantedToIdentitiesV2":[{"user":{"displayName":"Dee"}}]}
            ]}"#,
        )
        .unwrap();
        let p: Vec<Permission> = v["value"].as_array().unwrap().iter().map(Permission::from_json).collect();
        assert_eq!((p[0].role_label(), p[0].removable()), ("Owner", false));
        assert_eq!((p[1].who.as_str(), p[1].detail.as_str(), p[1].role_label(), p[1].removable()), ("Ann", "ann@contoso.com", "Can edit", true));
        assert_eq!((p[2].who.as_str(), p[2].detail.as_str(), p[2].is_link), ("Anyone with the link", "https://1drv.ms/x", true));
        assert_eq!((p[3].who.as_str(), p[3].removable()), ("guest@example.com", false));
        assert_eq!(p[4].who, "Link: Dee");
    }

    #[test]
    fn upload_sends_if_match_and_takes_new_etag() {
        let fake = Fake::new(|r| {
            assert_eq!(r.get_header("If-Match"), Some("\"{E},1\""));
            assert_eq!(r.get_header("Authorization"), Some("Bearer AT"));
            resp(200, r#"{"id":"I","name":"Book.xlsx","eTag":"\"{E},2\"","parentReference":{"driveId":"D"}}"#)
        });
        let s = session(&fake, FAR);
        let mut doc = CloudDoc {
            drive_id: "D".into(),
            item_id: "I".into(),
            name: "Book.xlsx".into(),
            etag: "\"{E},1\"".into(),
            web_url: String::new(),
            parent: None,
            local: PathBuf::new(),
            provider: crate::cloud::Provider::Microsoft,
        };
        let item = s.upload(&doc.item_path(), Some(&doc.etag), b"data").unwrap();
        doc.update_from(&item);
        assert_eq!(doc.etag, "\"{E},2\"");
        let calls = fake.calls();
        assert_eq!((calls[0].0.as_str(), calls[0].1.as_str(), calls[0].3), ("PUT", "https://graph.microsoft.com/v1.0/drives/D/items/I/content", 4));
    }

    #[test]
    fn upload_conflict_on_412() {
        let fake = Fake::new(|_| resp(412, r#"{"error":{"code":"resourceModified","message":"ETag does not match"}}"#));
        let s = session(&fake, FAR);
        assert_eq!(s.upload("/drives/D/items/I", Some("old"), b"x"), Err(UploadError::Conflict));
        // Overwrite: no If-Match header.
        let fake = Fake::new(|r| {
            assert!(r.get_header("If-Match").is_none());
            resp(200, r#"{"id":"I","eTag":"new"}"#)
        });
        assert_eq!(session(&fake, FAR).upload("/drives/D/items/I", None, b"x").unwrap().etag, "new");
    }

    #[test]
    fn save_a_copy_uses_rename_in_parent_folder() {
        let fake = Fake::new(|_| resp(201, r#"{"id":"N","name":"Book (copy).xlsx","eTag":"e1","parentReference":{"driveId":"D","id":"P"}}"#));
        let s = session(&fake, FAR);
        let item = s.upload_new("/drives/D/items/P", &copy_name("Book.xlsx"), b"x", "rename").unwrap();
        assert_eq!(item.name, "Book (copy).xlsx");
        assert_eq!(fake.calls()[0].1, "https://graph.microsoft.com/v1.0/drives/D/items/P:/Book%20%28copy%29.xlsx:/content?@microsoft.graph.conflictBehavior=rename");
        let fake = Fake::new(|_| resp(409, r#"{"error":{"code":"nameAlreadyExists","message":"exists"}}"#));
        assert_eq!(session(&fake, FAR).upload_new("/me/drive/root", "a.xlsx", b"x", "fail"), Err(UploadError::Exists));
        assert_eq!(copy_name("noext"), "noext (copy)");
    }

    #[test]
    fn large_upload_uses_session_chunks() {
        let fake = Fake::new(|r| match (r.method, r.url.as_str()) {
            ("POST", u) if u.ends_with("/createUploadSession") => {
                assert_eq!(r.get_header("If-Match"), Some("e1"));
                resp(200, r#"{"uploadUrl":"https://up.example/session"}"#)
            }
            ("PUT", "https://up.example/session") => {
                assert!(r.get_header("Authorization").is_none());
                if r.get_header("Content-Range").unwrap().ends_with(&format!("-{}/{}", SIMPLE_UPLOAD_MAX + 9, SIMPLE_UPLOAD_MAX + 10)) {
                    resp(200, r#"{"id":"I","eTag":"e2"}"#)
                } else {
                    resp(202, r#"{"nextExpectedRanges":[]}"#)
                }
            }
            _ => resp(500, "{}"),
        });
        let s = session(&fake, FAR);
        let data = vec![7u8; SIMPLE_UPLOAD_MAX + 10];
        assert_eq!(s.upload("/drives/D/items/I", Some("e1"), &data).unwrap().etag, "e2");
        let calls = fake.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[1].2.iter().find(|h| h.0 == "Content-Range").unwrap().1, format!("bytes 0-{}/{}", CHUNK_SIZE - 1, data.len()));
        assert_eq!(calls[1].3 + calls[2].3, data.len());
        // Conflict reported when creating the session.
        let fake = Fake::new(|_| resp(412, "{}"));
        assert_eq!(session(&fake, FAR).upload("/drives/D/items/I", Some("e1"), &data), Err(UploadError::Conflict));
    }

    #[test]
    fn refreshes_expired_token_and_follows_next_link() {
        let fake = Fake::new(|r| {
            if r.url.ends_with("/token") {
                return resp(200, r#"{"access_token":"AT2","refresh_token":"RT2","expires_in":3600}"#);
            }
            assert_eq!(r.get_header("Authorization"), Some("Bearer AT2"));
            if r.url.contains("skiptoken") {
                resp(200, r#"{"value":[{"id":"b","name":"B.csv","file":{}}]}"#)
            } else {
                resp(200, r#"{"value":[{"id":"a","name":"A.xlsx","file":{}}],"@odata.nextLink":"https://graph.microsoft.com/v1.0/me/drive/root/children?$skiptoken=2"}"#)
            }
        });
        let s = session(&fake, 0);
        let items = s.list(&Target::MyFiles).unwrap();
        assert_eq!(items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["A.xlsx", "B.csv"]);
        let calls = fake.calls();
        assert!(calls[0].1.ends_with("/oauth2/v2.0/token"));
        assert_eq!(calls.len(), 3);
    }

    #[test]
    fn sharing_requests() {
        let fake = Fake::new(|r| {
            if r.url.ends_with("/createLink") {
                resp(201, r#"{"link":{"webUrl":"https://1drv.ms/x/abc","type":"view","scope":"anonymous"}}"#)
            } else if r.method == "DELETE" {
                resp(204, "")
            } else {
                resp(200, r#"{"value":[]}"#)
            }
        });
        let s = session(&fake, FAR);
        assert_eq!(s.create_link("/drives/D/items/I", true, false).unwrap(), "https://1drv.ms/x/abc");
        s.invite("/drives/D/items/I", &["a@b.com".into()], true, "", false).unwrap();
        s.delete_permission("/drives/D/items/I", "p1").unwrap();
        let calls = fake.calls();
        assert_eq!(calls[1].1, "https://graph.microsoft.com/v1.0/drives/D/items/I/invite");
        assert_eq!(calls[2].1, "https://graph.microsoft.com/v1.0/drives/D/items/I/permissions/p1");
        let fake = Fake::new(|r| {
            let body: Value = serde_json::from_slice(&r.body).unwrap();
            assert_eq!(body["roles"], json!(["write"]));
            assert_eq!(body["requireSignIn"], json!(true));
            assert_eq!(body["sendInvitation"], json!(true));
            assert_eq!(body["recipients"][0]["email"], json!("a@b.com"));
            assert_eq!(body["message"], json!("hi"));
            resp(200, r#"{"value":[]}"#)
        });
        session(&fake, FAR).invite("/drives/D/items/I", &["a@b.com".into()], true, " hi ", true).unwrap();
    }

    #[test]
    fn user_initials() {
        assert_eq!(User { name: "Peter Petkov".into(), email: String::new() }.initials(), "PP");
        assert_eq!(User { name: "ann".into(), email: String::new() }.initials(), "A");
        assert_eq!(User { name: String::new(), email: "bob.smith@x.com".into() }.initials(), "B");
    }
}
