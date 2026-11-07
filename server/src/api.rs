//! The REST API. Every handler authenticates, then checks the caller's role on the item;
//! items the caller can't see answer 404, never 403.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::Config;
use crate::store::{Audit, Grant, Item, Kind, Link, OnConflict, Org, OrgGrant, OrgRole, Principal, Role, Store, StoreError, User};
use crate::util::{self, iso, now};

const JSON_LIMIT: usize = 64 * 1024;
const LOGIN_FAILURES: usize = 10;
const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Failures for a username (within the window) after which each sign-in waits.
const LOGIN_DELAY_AFTER: usize = 3;
/// The wait doubles per further failure up to this many times the first delay.
const LOGIN_DELAY_CAP: u64 = 8;

// ---------------------------------------------------------------------------
// State and errors
// ---------------------------------------------------------------------------

/// The TCP peer address, for plain and TLS listeners alike.
#[derive(Clone, Copy, Debug)]
pub struct Peer(pub SocketAddr);

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, tokio::net::TcpListener>> for Peer {
    fn connect_info(s: axum::serve::IncomingStream<'_, tokio::net::TcpListener>) -> Self {
        Peer(*s.remote_addr())
    }
}

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, crate::tls::TlsListener>> for Peer {
    fn connect_info(s: axum::serve::IncomingStream<'_, crate::tls::TlsListener>) -> Self {
        Peer(*s.remote_addr())
    }
}

#[derive(Clone)]
pub struct App {
    store: Arc<Mutex<Store>>,
    cfg: Arc<Config>,
    limiter: Arc<Mutex<Limiter>>,
    hub: Arc<crate::live::Hub>,
    /// Argon2 runs (about 19 MiB and a core each) at most this many at once.
    argon: Arc<tokio::sync::Semaphore>,
    /// Where uploads are staged while they stream in (`Store::incoming_dir`).
    incoming: Arc<std::path::PathBuf>,
}

impl App {
    pub fn new(mut store: Store, cfg: Config) -> App {
        store.allow_links = cfg.allow_links;
        store.clear_incoming();
        let incoming = Arc::new(store.incoming_dir());
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 16);
        App {
            store: Arc::new(Mutex::new(store)),
            cfg: Arc::new(cfg),
            limiter: Arc::new(Mutex::new(Limiter::default())),
            hub: Arc::default(),
            argon: Arc::new(tokio::sync::Semaphore::new(cores)),
            incoming,
        }
    }

    pub(crate) fn cfg(&self) -> &Config {
        &self.cfg
    }

    /// Background upkeep while the server runs: audit events older than
    /// `store::AUDIT_KEEP_DAYS` are deleted at start and then once a day.
    pub fn start_maintenance(&self) {
        let store = Arc::downgrade(&self.store);
        tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs(24 * 3600));
            loop {
                every.tick().await;
                let Some(store) = store.upgrade() else { break };
                let r = tokio::task::spawn_blocking(move || store.lock().unwrap_or_else(|e| e.into_inner()).prune_audit()).await;
                match r {
                    Ok(Ok(n)) if n > 0 => tracing::info!("pruned {n} audit event(s) older than {} days", crate::store::AUDIT_KEEP_DAYS),
                    Ok(Err(e)) => tracing::error!("couldn't prune the audit log: {e}"),
                    _ => {}
                }
            }
        });
    }

    /// Password hashing or checking (Argon2): never under the store lock, at most one per core
    /// at a time (waiting requests hold no thread and no memory).
    pub(crate) async fn argon<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> Result<T, ApiError> {
        // The permit lives in the blocking task: a client that disconnects drops this future,
        // but the hashing it started keeps its slot until it really ends.
        let permit = self.argon.clone().acquire_owned().await.map_err(|_| ApiError::internal())?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            f()
        })
        .await
        .map_err(|_| ApiError::internal())
    }

    /// Checks a new password against the policy, then hashes it (see `argon`).
    pub(crate) async fn new_password_hash(&self, password: String, username: String) -> Result<String, ApiError> {
        util::check_new_password(&password, &username).map_err(ApiError::bad)?;
        self.argon(move || util::hash_password(&password)).await
    }

    /// After a change that can affect who may see or edit what: re-checks every live session
    /// (they are told about a new role, or closed), before the response goes out.
    pub(crate) async fn changed(&self) {
        let (hub, store) = (self.hub.clone(), self.store.clone());
        let _ = tokio::task::spawn_blocking(move || hub.recheck(&store)).await;
    }

    /// `run` followed by `changed` when the work succeeded.
    pub(crate) async fn run_changing<T: Send + 'static>(&self, f: impl FnOnce(&Store) -> Result<T, ApiError> + Send + 'static) -> Result<T, ApiError> {
        let r = self.run(f).await;
        if r.is_ok() {
            self.changed().await;
        }
        r
    }

    /// Runs blocking store work (SQLite, disk) on the blocking pool, one request at a time.
    /// The client address of the request (see `web::web_gate`) is set on the store for the
    /// audit log while `f` runs.
    pub(crate) async fn run<T: Send + 'static>(&self, f: impl FnOnce(&Store) -> Result<T, ApiError> + Send + 'static) -> Result<T, ApiError> {
        let store = self.store.clone();
        let ip = crate::web::REQUEST_IP.try_with(Clone::clone).unwrap_or_default();
        tokio::task::spawn_blocking(move || {
            let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
            s.request_ip = ip;
            let r = f(&s);
            s.request_ip.clear();
            r
        })
        .await
        .map_err(|_| ApiError::internal())?
    }

    fn limiter(&self) -> MutexGuard<'_, Limiter> {
        self.limiter.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn limited(&self, keys: &[String]) -> bool {
        let mut l = self.limiter();
        keys.iter().any(|k| l.blocked(k))
    }

    fn fail(&self, keys: &[String]) {
        let mut l = self.limiter();
        for k in keys {
            l.fail(k);
        }
    }

    /// Checks the limits and, in the same step, counts this attempt as a failure in advance
    /// (taken back by `succeeded`), so parallel attempts can't all pass the check before any
    /// of them fails. Returns the failures of `keys[1]` before this attempt, or `None` when
    /// limited.
    fn attempt(&self, keys: &[String]) -> Option<usize> {
        let mut l = self.limiter();
        if keys.iter().any(|k| l.blocked(k)) {
            return None;
        }
        let before = keys.get(1).map(|k| l.prune(k)).unwrap_or(0);
        for k in keys {
            l.fail(k);
        }
        Some(before)
    }

    /// The attempt counted by `attempt` succeeded: forget the user's failures and give the
    /// address its provisional failure back.
    fn succeeded(&self, ip_key: &str, user_key: &str) {
        let mut l = self.limiter();
        l.map.remove(user_key);
        l.unfail(ip_key);
    }
}

/// How long a sign-in for a username with `failures` recent failures waits first: nothing up
/// to `LOGIN_DELAY_AFTER`, then `base`, 2x, 4x... up to `LOGIN_DELAY_CAP` x `base`.
fn login_delay(failures: usize, base: Duration) -> Duration {
    if failures < LOGIN_DELAY_AFTER {
        return Duration::ZERO;
    }
    let factor = 1u64.checked_shl((failures - LOGIN_DELAY_AFTER) as u32).unwrap_or(u64::MAX).min(LOGIN_DELAY_CAP);
    base.saturating_mul(factor as u32)
}

/// Keys kept by the limiter at most (each is short: an address, a valid username or a link id).
const LIMITER_KEYS: usize = 100_000;

#[derive(Default)]
struct Limiter {
    map: HashMap<String, VecDeque<Instant>>,
}

impl Limiter {
    fn prune(&mut self, key: &str) -> usize {
        let Some(q) = self.map.get_mut(key) else { return 0 };
        while q.front().is_some_and(|t| t.elapsed() > LOGIN_WINDOW) {
            q.pop_front();
        }
        let n = q.len();
        if n == 0 {
            self.map.remove(key);
        }
        n
    }

    fn blocked(&mut self, key: &str) -> bool {
        self.prune(key) >= LOGIN_FAILURES
    }

    fn fail(&mut self, key: &str) {
        if !self.map.contains_key(key) && self.map.len() >= LIMITER_KEYS {
            self.map.retain(|_, q| q.back().is_some_and(|t| t.elapsed() < LOGIN_WINDOW));
            // Still full (an attack from very many addresses or names): drop the keys whose last
            // failure is oldest, so the map stays bounded.
            if self.map.len() >= LIMITER_KEYS {
                let mut last: Vec<Instant> = self.map.values().filter_map(|q| q.back().copied()).collect();
                last.sort_unstable();
                let cut = last[last.len() / 10];
                self.map.retain(|_, q| q.back().is_some_and(|t| *t > cut));
            }
        }
        let q = self.map.entry(key.to_string()).or_default();
        q.push_back(Instant::now());
        // Only the count within the window matters; never keep more than the limit needs.
        while q.len() > LOGIN_FAILURES * 2 {
            q.pop_front();
        }
    }

    /// Takes back the newest failure of `key` (a provisional one that turned out a success).
    fn unfail(&mut self, key: &str) {
        if let Some(q) = self.map.get_mut(key) {
            q.pop_back();
            if q.is_empty() {
                self.map.remove(key);
            }
        }
    }
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: Value,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        ApiError { status, body: json!({ "error": msg.into() }) }
    }

    pub(crate) fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "Not found.")
    }

    pub(crate) fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "Sign in to continue.")
    }

    /// With a machine-readable `code` next to the message.
    pub(crate) fn coded(status: StatusCode, code: &str, msg: impl Into<String>) -> Self {
        ApiError { status, body: json!({ "error": msg.into(), "code": code }) }
    }

    pub(crate) fn forbidden(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, msg)
    }

    pub(crate) fn bad(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, msg)
    }

    pub(crate) fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong on the server.")
    }

    pub(crate) fn password_change_required() -> Self {
        Self::coded(StatusCode::FORBIDDEN, "password_change_required", "Choose a new password to continue (you signed in with a temporary one).")
    }

    pub(crate) fn status(&self) -> StatusCode {
        self.status
    }

    /// The message for people.
    pub(crate) fn message(&self) -> &str {
        self.body["error"].as_str().unwrap_or("Something went wrong.")
    }

    pub(crate) fn code(&self) -> Option<&str> {
        self.body["code"].as_str()
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound => ApiError::not_found(),
            StoreError::Conflict(m) => ApiError::new(StatusCode::CONFLICT, m),
            StoreError::Invalid(m) => ApiError::bad(m),
            StoreError::Forbidden(m) => ApiError::new(StatusCode::FORBIDDEN, m),
            StoreError::Internal(m) => {
                tracing::error!("{m}");
                ApiError::internal()
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, axum::Json(self.body)).into_response()
    }
}

pub(crate) type ApiResult = Result<Response, ApiError>;

fn ok_json(v: Value) -> ApiResult {
    Ok(axum::Json(v).into_response())
}

fn parse_body<T: DeserializeOwned>(b: &Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(b).map_err(|e| ApiError::bad(format!("Invalid request: {e}")))
}

// ---------------------------------------------------------------------------
// Authentication and access helpers
// ---------------------------------------------------------------------------

fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim().to_string())
}

/// The caller. A user who signed in with a temporary password gets 403 with code
/// `password_change_required` until they chose a new one (`auth_any` allows that).
fn auth(s: &Store, headers: &HeaderMap) -> Result<Principal, ApiError> {
    let who = auth_any(s, headers)?;
    if let Principal::User { user, .. } = &who
        && user.must_change_password
    {
        return Err(ApiError::password_change_required());
    }
    Ok(who)
}

/// The caller, even one who still has to change a temporary password (for `/api/me`,
/// `/api/me/password` and `/api/logout`).
///
/// An `Authorization` header decides alone (a bearer device token or share-link access
/// token). Without one, the browser session cookie of the API (`web`), if `web::web_gate`
/// let it through: 403 `csrf_failed` / `cross_site` when it didn't.
fn auth_any(s: &Store, headers: &HeaderMap) -> Result<Principal, ApiError> {
    if headers.contains_key(header::AUTHORIZATION) {
        let token = bearer(headers).ok_or_else(ApiError::unauthorized)?;
        return s.authenticate(&token)?.ok_or_else(ApiError::unauthorized);
    }
    let secret = crate::web::verdict(headers)?.ok_or_else(ApiError::unauthorized)?;
    let (user, sess) = s.web_session(&secret)?.ok_or_else(ApiError::unauthorized)?;
    Ok(Principal::User { user, sign_in: SignIn::Web(sess.id) })
}

/// What a person signed in with: an API device token, a web admin session (`/admin`) or a
/// browser session of the API (Drive).
pub(crate) use crate::store::SignIn;

/// How a request authenticates a person: the API's bearer device token or browser session
/// (`Bearer`: the request headers), or the web admin session cookie (its secret). The
/// administration flows below take this (or the `User` it gives), so the API and the admin
/// page share every rule; flows that hash a password outside the store lock authenticate
/// again before they store.
#[derive(Clone)]
pub(crate) enum Caller {
    Bearer(HeaderMap),
    Admin(String),
}

impl Caller {
    /// The person, even one who still has to change a temporary password.
    pub(crate) fn person_any(&self, s: &Store) -> Result<(User, SignIn), ApiError> {
        match self {
            Caller::Bearer(h) => match auth_any(s, h)? {
                Principal::User { user, sign_in } => Ok((user, sign_in)),
                Principal::Link { .. } => Err(ApiError::unauthorized()),
            },
            Caller::Admin(secret) => s.admin_session(secret)?.map(|(u, sess)| (u, SignIn::Admin(sess.id))).ok_or_else(ApiError::unauthorized),
        }
    }

    /// The person; 403 `password_change_required` while a temporary password is set.
    pub(crate) fn person(&self, s: &Store) -> Result<User, ApiError> {
        let (u, _) = self.person_any(s)?;
        if u.must_change_password {
            return Err(ApiError::password_change_required());
        }
        Ok(u)
    }
}

/// Endpoints for accounts only (share-link tokens can't use them).
fn user_of(p: &Principal) -> Result<&User, ApiError> {
    match p {
        Principal::User { user, .. } => Ok(user),
        Principal::Link { .. } => Err(ApiError::unauthorized()),
    }
}

/// Who a change is recorded as: a user id or `link:<id>`.
fn actor(p: &Principal) -> String {
    match p {
        Principal::User { user, .. } => user.id.clone(),
        Principal::Link { link } => format!("link:{}", link.id),
    }
}

/// An audit event done by the caller (a person, or a share-link holder as `link:<id>`).
fn audit_by(who: &Principal, action: &'static str) -> Audit {
    match who {
        Principal::User { user, .. } => Audit::by(user, action),
        Principal::Link { link } => Audit::anonymous(action, format!("link:{}", link.id)),
    }
}

/// The item and the caller's role, or 404 when the caller can't see it.
fn visible(s: &Store, who: &Principal, id: &str, include_deleted: bool) -> Result<(Item, Role), ApiError> {
    let item = s.item(id)?.ok_or_else(ApiError::not_found)?;
    let role = s.role(who, &item, include_deleted)?.ok_or_else(ApiError::not_found)?;
    Ok((item, role))
}

fn need_edit(role: Role) -> Result<(), ApiError> {
    if role >= Role::Edit { Ok(()) } else { Err(ApiError::new(StatusCode::FORBIDDEN, "You have view-only access to this item.")) }
}

fn display_of(s: &Store, actor: &str) -> String {
    if actor.starts_with("link:") { "Someone with a link".into() } else { s.display_name(actor) }
}

fn username_of(s: &Store, actor: &str) -> String {
    s.user_by_id(actor).ok().flatten().map(|u| u.username).unwrap_or_default()
}

/// A person, or an organisation (for the owner of organisation files).
fn person(u: &User) -> Value {
    match &u.org_id {
        Some(org) => json!({ "username": "", "display_name": u.display_name, "org": org }),
        None => json!({ "username": u.username, "display_name": u.display_name }),
    }
}

fn item_json(s: &Store, it: &Item, role: Role) -> Value {
    let owner = s.user_by_id(&it.owner_id).ok().flatten();
    let mut v = json!({
        "id": it.id,
        "parent": it.parent_id,
        "name": it.name,
        "kind": if it.kind == Kind::Folder { "folder" } else { "file" },
        "size": it.size,
        "version": it.version,
        "etag": if it.kind == Kind::File { it.etag() } else { String::new() },
        "modified": iso(it.modified),
        "modified_by": display_of(s, &it.modified_by),
        "modified_by_username": username_of(s, &it.modified_by),
        "created": iso(it.created),
        "owner": owner.as_ref().map(person),
        "role": role.as_str(),
    });
    // Files in an organisation's folder (the organisation's account owns them).
    if let Some(o) = owner.as_ref().filter(|o| o.org_id.is_some()) {
        v["org"] = json!({ "id": o.org_id, "name": o.display_name });
    }
    if it.deleted != 0 {
        v["deleted"] = json!(iso(it.deleted));
    }
    v
}

fn items_json(s: &Store, who: &Principal, items: &[Item]) -> Result<Value, ApiError> {
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        if let Some(r) = s.role(who, it, it.deleted != 0)? {
            out.push(item_json(s, it, r));
        }
    }
    Ok(json!({ "items": out }))
}

pub(crate) fn user_json(u: &User) -> Value {
    json!({
        "id": u.id,
        "username": u.username,
        "display_name": u.display_name,
        "email": u.email,
        "is_admin": u.is_admin,
        "root": u.root_id,
        "must_change_password": u.must_change_password,
    })
}

fn org_json(s: &Store, o: &Org, mine: Option<OrgRole>) -> Result<Value, ApiError> {
    Ok(json!({
        "id": o.id,
        "name": o.name,
        "root": o.root_id,
        "member_role": o.member_role.as_str(),
        "role": mine.map(OrgRole::as_str),
        "members": s.members(&o.id)?.len(),
        "created": iso(o.created),
    }))
}

/// The client address; behind a loopback reverse proxy, the last X-Forwarded-For entry.
pub(crate) fn client_ip(cfg: &Config, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    if cfg.trust_proxy
        && peer.ip().is_loopback()
        && let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
    {
        return ip;
    }
    peer.ip()
}

pub(crate) fn too_many() -> ApiError {
    ApiError::new(StatusCode::TOO_MANY_REQUESTS, "Too many failed attempts. Try again in 15 minutes.")
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router(app: App) -> Router {
    let upload_limit = DefaultBodyLimit::max(app.cfg.max_upload_bytes());
    Router::new()
        .route("/api/health", get(health))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/web/login", post(crate::web::login))
        .route("/api/web/session", get(crate::web::session))
        .route("/api/web/logout", post(crate::web::logout))
        .route("/api/admin/audit", get(audit_log))
        .route("/api/me", get(me))
        .route("/api/me/password", post(change_password))
        .route("/api/root", get(root))
        .route("/api/shared-with-me", get(shared_with_me))
        .route("/api/recent", get(recent))
        .route("/api/trash", get(trash))
        .route("/api/users", get(users).post(create_account))
        .route("/api/users/{username}/reset-password", post(reset_password))
        .route("/api/users/{username}/disable", post(disable_user))
        .route("/api/users/{username}/enable", post(enable_user))
        .route("/api/users/{username}/admin", post(set_user_admin))
        .route("/api/users/{username}/sign-out", post(sign_out_user))
        .route("/api/users/{username}/sessions", get(user_sessions))
        .route("/api/users/{username}/sessions/{kind}/{id}", axum::routing::delete(end_user_session))
        .route("/api/orgs", get(list_orgs).post(create_org))
        .route("/api/orgs/{org}", get(get_org).patch(patch_org).delete(delete_org))
        .route("/api/orgs/{org}/members", get(org_members).post(add_member))
        .route("/api/orgs/{org}/members/{username}", axum::routing::patch(patch_member).delete(remove_member))
        .route("/api/orgs/{org}/invites", get(list_invites).post(create_invite))
        .route("/api/orgs/{org}/invites/{id}", axum::routing::delete(delete_invite))
        .route("/api/orgs/{org}/storage", get(org_storage))
        .route("/api/admin/storage", get(admin_storage))
        .route("/api/invites/{token}", get(invite_info).post(accept_invite))
        .route("/i/{token}", get(invite_page).post(invite_form))
        .route("/api/folders", post(create_folder))
        .route("/api/files", post(upload_file).layer(upload_limit))
        .route("/api/items/{id}", get(get_item).patch(patch_item).delete(delete_item))
        .route("/api/items/{id}/children", get(children))
        .route("/api/items/{id}/content", get(get_content).put(put_content).layer(upload_limit))
        .route("/api/items/{id}/restore", post(restore_item))
        .route("/api/items/{id}/versions", get(versions))
        .route("/api/items/{id}/versions/{n}/restore", post(restore_version))
        .route("/api/items/{id}/permissions", get(permissions).post(grant).delete(revoke_body))
        .route("/api/items/{id}/permissions/{username}", axum::routing::delete(revoke_path))
        .route("/api/items/{id}/permissions/org/{org}", axum::routing::delete(revoke_org_path))
        .route("/api/items/{id}/links", get(list_links).post(create_link))
        .route("/api/items/{id}/links/{link}", axum::routing::delete(delete_link))
        .route("/api/items/{id}/live", get(live))
        .route("/api/items/{id}/live-ticket", post(live_ticket))
        .route("/api/items/{id}/live/snapshot", post(live_snapshot).layer(DefaultBodyLimit::max(app.cfg.max_upload_bytes().saturating_mul(4))))
        .route("/api/items/{id}/live/state", get(live_state))
        .route("/api/links/{token}", get(open_link).post(open_link_password))
        .route("/l/{token}", get(link_page))
        .route("/l/{token}/download", get(link_download).post(link_download_form))
        .merge(crate::admin::routes())
        .fallback(|| async { ApiError::not_found() })
        .layer(DefaultBodyLimit::max(JSON_LIMIT))
        .layer(middleware::from_fn(crate::admin::harden_admin))
        .layer(middleware::from_fn_with_state(app.clone(), crate::web::web_gate))
        .layer(middleware::from_fn(log_requests))
        .with_state(app)
}

/// Logs method, path (secrets in link paths redacted), status and time. Never bodies or headers.
async fn log_requests(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = redact(req.uri().path());
    let start = Instant::now();
    let res = next.run(req).await;
    tracing::info!("{method} {path} {} {}ms", res.status().as_u16(), start.elapsed().as_millis());
    res
}

fn redact(path: &str) -> String {
    for prefix in ["/api/links/", "/l/", "/api/invites/", "/i/"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            let tail = rest.split_once('/').map(|(_, t)| format!("/{t}")).unwrap_or_default();
            return format!("{prefix}<redacted>{tail}");
        }
    }
    path.to_string()
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

async fn health() -> ApiResult {
    ok_json(json!({ "status": "ok", "service": "freedomoffice-server", "version": env!("CARGO_PKG_VERSION") }))
}

#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
    #[serde(default)]
    device: String,
}

async fn login(State(app): State<App>, ConnectInfo(Peer(peer)): ConnectInfo<Peer>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let b: LoginBody = parse_body(&body)?;
    let ip = client_ip(&app.cfg, peer, &headers);
    let user = check_sign_in(&app, ip, &b.username, b.password, "app").await?;
    let device = b.device;
    app.run(move |s| {
        let token = s.create_token(&user.id, &device)?;
        ok_json(json!({ "token": token, "user": user_json(&user) }))
    })
    .await
}

/// Checks a username and password for a sign-in from `ip` (the API's, the browser session's
/// and the admin page's): the per-address and per-username limits, the delay after repeated
/// failures, Argon2 in its slots. The enabled person, or 401 (429 when limited). The result
/// goes to the audit log (`sign_in` with `via` as detail, or `sign_in.failed` for usernames
/// that could exist; never the password).
pub(crate) async fn check_sign_in(app: &App, ip: IpAddr, username: &str, password: String, via: &'static str) -> Result<User, ApiError> {
    verify_sign_in(app, ip, username, password, false, via).await
}

/// `check_sign_in`, optionally also for a disabled account (the right password for one is
/// still counted as a failure). Lets callers that need the password checked say "disabled"
/// to someone who proved they know it.
async fn verify_sign_in(app: &App, ip: IpAddr, username: &str, password: String, allow_disabled: bool, via: &'static str) -> Result<User, ApiError> {
    let ip_key = format!("ip:{ip}");
    let wrong = || ApiError::new(StatusCode::UNAUTHORIZED, "The username or password is incorrect.");
    let name = username.trim().to_string();
    // Something that can't be a username (or an absurd password) never reaches the limiter's
    // username keys, the database or Argon2, and isn't logged as typed.
    if !util::valid_username(&name) || password.len() > 1024 {
        if app.attempt(std::slice::from_ref(&ip_key)).is_none() {
            return Err(too_many());
        }
        tracing::warn!("failed sign-in with an invalid username from {ip}");
        return Err(wrong());
    }
    let user_key = format!("user:{}", name.to_lowercase());
    let keys = [ip_key.clone(), user_key.clone()];
    // Counted as a failure up front, so parallel attempts are limited too.
    let Some(before) = app.attempt(&keys) else { return Err(too_many()) };
    // Repeated failures for a username slow every further attempt down (right or wrong, so the
    // wait says nothing about the password).
    let wait = login_delay(before, Duration::from_millis(app.cfg.login_delay_ms));
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
    let n = name.clone();
    let found = app.run(move |s| Ok(s.user_by_name(&n)?.map(|u| (s.password_hash(&u.id), u)))).await?;
    let pw = password;
    let user = app
        .argon(move || match found {
            // The password is always checked first, so a disabled account answers as slowly as any other.
            Some((Ok(hash), u)) if util::verify_password(&pw, &hash) && (allow_disabled || !u.disabled) => Some(u),
            Some(_) => None,
            None => {
                util::dummy_verify(&pw);
                None
            }
        })
        .await?;
    let Some(user) = user else {
        tracing::warn!("failed sign-in for user '{name}' from {ip}");
        let (n, ip) = (name.clone(), ip.to_string());
        let _ = app.run(move |s| Ok(s.audit(Audit::anonymous("sign_in.failed", "").target(&n).ip(ip).detail(via))?)).await;
        return Err(wrong());
    };
    if !user.disabled {
        app.succeeded(&ip_key, &user_key);
    }
    let (u, ip) = (user.clone(), ip.to_string());
    let action = if user.disabled { "sign_in.failed" } else { "sign_in" };
    let detail = if user.disabled { format!("{via}, account disabled") } else { via.to_string() };
    let _ = app.run(move |s| Ok(s.audit(Audit::by(&u, action).target(&u.username).ip(ip).detail(detail))?)).await;
    Ok(user)
}

async fn logout(State(app): State<App>, headers: HeaderMap) -> ApiResult {
    app.run_changing(move |s| {
        let who = auth_any(s, &headers)?;
        match who {
            Principal::User { sign_in: SignIn::Device(id), .. } => s.revoke_token(id)?,
            Principal::User { sign_in: SignIn::Admin(id) | SignIn::Web(id), .. } => s.end_admin_session(id)?,
            Principal::Link { .. } => {}
        }
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

/// The signed-in user, with the organisations they belong to (each with its folder).
async fn me(State(app): State<App>, headers: HeaderMap) -> ApiResult {
    app.run(move |s| {
        let who = auth_any(s, &headers)?;
        let u = user_of(&who)?;
        let mut v = user_json(u);
        let mut orgs = Vec::new();
        for (o, r) in s.orgs_of(&u.id)? {
            orgs.push(org_json(s, &o, Some(r))?);
        }
        v["orgs"] = json!(orgs);
        ok_json(v)
    })
    .await
}

#[derive(Deserialize)]
struct PasswordBody {
    old_password: String,
    new_password: String,
}

/// `POST /api/me/password {old_password, new_password}`: also the way out of a temporary
/// password. Signs the user's other devices out.
async fn change_password(State(app): State<App>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let b: PasswordBody = parse_body(&body)?;
    let (fresh, n) = change_own_password(&app, Caller::Bearer(headers), b.old_password, b.new_password).await?;
    ok_json(json!({ "user": user_json(&fresh), "signed_out_devices": n }))
}

/// A person changes their own password (the API's and the admin page's): the current one is
/// checked (counted for the username's limit), the new one follows the policy. Every other
/// sign-in ends (devices, web admin and web browser sessions; the one in use stays). The updated person and
/// how many sign-ins ended.
pub(crate) async fn change_own_password(app: &App, caller: Caller, old_password: String, new_password: String) -> Result<(User, usize), ApiError> {
    let c = caller.clone();
    let (user, sign_in, hash) = app
        .run(move |s| {
            let (user, sign_in) = c.person_any(s)?;
            let hash = s.password_hash(&user.id)?;
            Ok((user, sign_in, hash))
        })
        .await?;
    let key = format!("user:{}", user.username.to_lowercase());
    if old_password.len() > 1024 {
        return Err(ApiError::coded(StatusCode::FORBIDDEN, "wrong_password", "The current password is incorrect."));
    }
    if app.attempt(std::slice::from_ref(&key)).is_none() {
        return Err(too_many());
    }
    let (old, new, current) = (old_password, new_password.clone(), hash.clone());
    let (ok, same) = app.argon(move || (util::verify_password(&old, &current), util::verify_password(&new, &current))).await?;
    if !ok {
        tracing::warn!("wrong current password in a password change for user '{}'", user.username);
        return Err(ApiError::coded(StatusCode::FORBIDDEN, "wrong_password", "The current password is incorrect."));
    }
    app.limiter().map.remove(&key);
    if same {
        return Err(ApiError::bad("The new password must be different from the current one."));
    }
    let new_hash = app.new_password_hash(new_password, user.username.clone()).await?;
    app.run_changing(move |s| {
        // Still the same password (no other change won the race) and the same sign-in.
        let still = match sign_in {
            SignIn::Device(id) => s.token_exists(id)?,
            SignIn::Admin(id) | SignIn::Web(id) => s.admin_session_exists(id)?,
        };
        if s.password_hash(&user.id)? != hash || !still {
            return Err(ApiError::new(StatusCode::CONFLICT, "Your password was changed meanwhile. Sign in again."));
        }
        s.set_password_hash(&user.id, &new_hash)?;
        // Every other sign-in ends: devices, admin and web browser sessions.
        let n = match sign_in {
            SignIn::Device(id) => s.revoke_other_tokens(&user.id, id)?,
            SignIn::Admin(id) | SignIn::Web(id) => s.revoke_all_but_admin_session(&user.id, id)?,
        };
        tracing::info!("user '{}' changed their password ({n} other sign-in(s) ended)", user.username);
        let was_temp = if user.must_change_password { "replaced a temporary password; " } else { "" };
        s.audit(Audit::by(&user, "password.change").target(&user.username).detail(format!("{was_temp}{n} other sign-in(s) ended")))?;
        let fresh = s.user_by_id(&user.id)?.ok_or_else(ApiError::not_found)?;
        Ok((fresh, n))
    })
    .await
}

// ---------------------------------------------------------------------------
// Accounts and organisations (administration)
// ---------------------------------------------------------------------------

/// The server administrator (manages accounts and organisations; sees no documents).
pub(crate) fn need_admin(me: &User) -> Result<(), ApiError> {
    if me.is_admin { Ok(()) } else { Err(ApiError::forbidden("Only the server administrator can do that.")) }
}

/// The organisation (by id or name) and the caller's role in it; 404 unless the caller is a
/// member or the server administrator.
pub(crate) fn org_for(s: &Store, me: &User, org: &str) -> Result<(Org, Option<OrgRole>), ApiError> {
    let o = s.find_org(org)?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "There's no such organisation."))?;
    let mine = s.member_role(&o.id, &me.id)?;
    if mine.is_none() && !me.is_admin {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "There's no such organisation."));
    }
    Ok((o, mine))
}

/// Org owners and admins (and the server administrator) manage an organisation.
pub(crate) fn need_manage(me: &User, mine: Option<OrgRole>) -> Result<(), ApiError> {
    if me.is_admin || mine.is_some_and(OrgRole::manages) { Ok(()) } else { Err(ApiError::forbidden("Only the organisation's owners and admins can do that.")) }
}

/// Making someone an owner, or changing or removing an owner, takes an owner (or the server
/// administrator); admins manage everyone else.
fn need_owner_for(me: &User, mine: Option<OrgRole>, involved: &[Option<OrgRole>]) -> Result<(), ApiError> {
    if involved.contains(&Some(OrgRole::Owner)) && !me.is_admin && mine != Some(OrgRole::Owner) {
        return Err(ApiError::forbidden("Only an owner of the organisation can add, change or remove owners."));
    }
    Ok(())
}

pub(crate) fn person_by_name(s: &Store, username: &str) -> Result<User, ApiError> {
    s.user_by_name(username)?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "No such user."))
}

#[derive(Deserialize)]
pub(crate) struct AccountBody {
    pub(crate) username: String,
    #[serde(default)]
    pub(crate) display_name: String,
    #[serde(default)]
    pub(crate) email: String,
    /// Join this organisation (required unless the caller is the server administrator).
    #[serde(default)]
    pub(crate) org: Option<String>,
    #[serde(default)]
    pub(crate) role: Option<String>,
}

/// Who may create the account in `b` and into which organisation (checked before the password
/// is hashed and again when the account is stored).
/// (the caller, the organisation and role the account joins, the checked username)
type AccountTarget = (User, Option<(Org, OrgRole)>, String);

fn account_target(s: &Store, caller: &Caller, b: &AccountBody) -> Result<AccountTarget, ApiError> {
    let me = caller.person(s)?;
    let org = match b.org.as_deref().filter(|o| !o.trim().is_empty()) {
        Some(o) => {
            let (org, mine) = org_for(s, &me, o)?;
            need_manage(&me, mine)?;
            let role = OrgRole::parse(b.role.as_deref().unwrap_or("member")).ok_or_else(|| ApiError::bad("role must be member, admin or owner."))?;
            need_owner_for(&me, mine, &[Some(role)])?;
            Some((org, role))
        }
        None => {
            need_admin(&me)?;
            None
        }
    };
    let username = s.check_new_username(&b.username)?;
    Ok((me, org, username))
}

/// `POST /api/users`: a new account with a temporary password generated by the server,
/// returned once in this response (never logged or stored in clear); the person must choose
/// a new one when they first sign in. Org owners/admins create accounts into their org.
async fn create_account(State(app): State<App>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let b: AccountBody = parse_body(&body)?;
    let (u, pw) = create_account_as(&app, Caller::Bearer(headers), b).await?;
    Ok((StatusCode::CREATED, axum::Json(json!({ "user": user_json(&u), "temporary_password": pw }))).into_response())
}

/// Creates the account in `b` with a temporary password (the API's and the admin page's):
/// the account and the password, which the caller shows once.
pub(crate) async fn create_account_as(app: &App, caller: Caller, b: AccountBody) -> Result<(User, String), ApiError> {
    let b = Arc::new(b);
    let (c, bb) = (caller.clone(), b.clone());
    let (_, _, username) = app.run(move |s| account_target(s, &c, &bb)).await?;
    let pw = util::temp_password_for(&username);
    let p = pw.clone();
    let hash = app.argon(move || util::hash_password(&p)).await?;
    let u = app
        .run(move |s| {
            let (me, org, _) = account_target(s, &caller, &b)?;
            let u = s.create_temp_account(&b.username, &b.display_name, &b.email, &hash, org.as_ref().map(|(o, r)| (o, *r, me.id.as_str())))?;
            tracing::info!("user '{}' created the account '{}'{}", me.username, u.username, org.as_ref().map(|(o, _)| format!(" in {}", o.name)).unwrap_or_default());
            let mut a = Audit::by(&me, "account.create").target(&u.username).detail("temporary password");
            if let Some((o, r)) = &org {
                a = a.org(&o.id).detail(format!("temporary password; joins {} as {}", o.name, r.as_str()));
            }
            s.audit(a)?;
            Ok(u)
        })
        .await?;
    Ok((u, pw))
}

/// `POST /api/users/{username}/reset-password` (server administrator): a new temporary
/// password (returned once; accepted policy, SPEC §2), all devices signed out.
async fn reset_password(State(app): State<App>, headers: HeaderMap, Path(username): Path<String>) -> ApiResult {
    let (u, pw) = reset_password_as(&app, Caller::Bearer(headers), username).await?;
    ok_json(json!({ "user": user_json(&u), "temporary_password": pw }))
}

/// The server administrator gives `username` a new temporary password (returned, for showing
/// once) and signs them out everywhere; logged as a warning, never the password.
pub(crate) async fn reset_password_as(app: &App, caller: Caller, username: String) -> Result<(User, String), ApiError> {
    let (c, n) = (caller.clone(), username.clone());
    let target = app
        .run(move |s| {
            let me = c.person(s)?;
            need_admin(&me)?;
            let u = person_by_name(s, &n)?;
            not_self_reset(&me, &u)?;
            Ok(u)
        })
        .await?;
    let pw = util::temp_password_for(&target.username);
    let p = pw.clone();
    let hash = app.argon(move || util::hash_password(&p)).await?;
    let u = app
        .run_changing(move |s| {
            let me = caller.person(s)?;
            need_admin(&me)?;
            let u = person_by_name(s, &username)?;
            not_self_reset(&me, &u)?;
            let n = s.set_temp_password_hash(&u.id, &hash)?;
            tracing::warn!("administrator '{}' reset the password of '{}' ({n} sign-in(s) ended)", me.username, u.username);
            s.audit(Audit::by(&me, "password.reset").target(&u.username).detail(format!("{n} sign-in(s) ended")))?;
            s.user_by_id(&u.id)?.ok_or_else(ApiError::not_found)
        })
        .await?;
    Ok((u, pw))
}

/// A reset signs the person out everywhere, so resetting your own password would lock you out
/// before you saw the new one: change it instead.
fn not_self_reset(me: &User, target: &User) -> Result<(), ApiError> {
    if me.id == target.id {
        return Err(ApiError::bad("Use Change password for your own account."));
    }
    Ok(())
}

/// The server administrator disables (signs out everywhere, ends live sessions) or enables an
/// account; nobody disables themselves.
pub(crate) fn set_disabled_as(s: &Store, me: &User, username: &str, disabled: bool) -> Result<User, ApiError> {
    need_admin(me)?;
    let u = person_by_name(s, username)?;
    if u.id == me.id && disabled {
        return Err(ApiError::bad("You can't disable your own account."));
    }
    s.set_disabled(&u.id, disabled)?;
    tracing::info!("user '{}' {} the account '{}'", me.username, if disabled { "disabled" } else { "enabled" }, u.username);
    s.audit(Audit::by(me, if disabled { "account.disable" } else { "account.enable" }).target(&u.username))?;
    s.user_by_id(&u.id)?.ok_or_else(ApiError::not_found)
}

async fn set_disabled(app: App, headers: HeaderMap, username: String, disabled: bool) -> ApiResult {
    app.run_changing(move |s| {
        let who = auth(s, &headers)?;
        set_disabled_as(s, user_of(&who)?, &username, disabled)?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

/// `POST /api/users/{username}/disable` (server administrator): blocks sign-in, signs out
/// every device and ends live sessions.
async fn disable_user(State(app): State<App>, headers: HeaderMap, Path(username): Path<String>) -> ApiResult {
    set_disabled(app, headers, username, true).await
}

async fn enable_user(State(app): State<App>, headers: HeaderMap, Path(username): Path<String>) -> ApiResult {
    set_disabled(app, headers, username, false).await
}

/// The server administrator makes `username` a server administrator too, or takes that away.
/// The last enabled administrator can't step down.
pub(crate) fn set_super_admin_as(s: &Store, me: &User, username: &str, on: bool) -> Result<User, ApiError> {
    need_admin(me)?;
    let u = person_by_name(s, username)?;
    if !on && u.is_admin && !u.disabled && s.admin_count()? <= 1 {
        return Err(ApiError::new(StatusCode::CONFLICT, "This is the last server administrator. Make someone else an administrator first."));
    }
    s.set_admin(&u.id, on)?;
    tracing::warn!("administrator '{}' {} '{}' {} server administrator", me.username, if on { "made" } else { "removed" }, u.username, if on { "a" } else { "as" });
    s.audit(Audit::by(me, "account.admin").target(&u.username).detail(if on { "made server administrator" } else { "no longer server administrator" }))?;
    s.user_by_id(&u.id)?.ok_or_else(ApiError::not_found)
}

#[derive(Deserialize)]
struct AdminFlagBody {
    is_admin: bool,
}

/// `POST /api/users/{username}/admin {is_admin}` (server administrator).
async fn set_user_admin(State(app): State<App>, headers: HeaderMap, Path(username): Path<String>, body: Bytes) -> ApiResult {
    let b: AdminFlagBody = parse_body(&body)?;
    app.run_changing(move |s| {
        let who = auth(s, &headers)?;
        let u = set_super_admin_as(s, user_of(&who)?, &username, b.is_admin)?;
        ok_json(json!({ "user": user_json(&u) }))
    })
    .await
}

/// A person with their sign-ins: devices (API tokens) and web admin sessions.
pub(crate) type SignIns = (User, Vec<crate::store::DeviceToken>, Vec<crate::store::AdminSession>);

/// The server administrator lists a person's sign-ins.
pub(crate) fn sign_ins_as(s: &Store, me: &User, username: &str) -> Result<SignIns, ApiError> {
    need_admin(me)?;
    let u = person_by_name(s, username)?;
    let (devices, web) = (s.tokens_of(&u.id)?, s.admin_sessions_of(&u.id)?);
    Ok((u, devices, web))
}

/// The server administrator ends one of a person's sign-ins (404 when it isn't theirs).
pub(crate) fn end_sign_in_as(s: &Store, me: &User, username: &str, which: SignIn) -> Result<User, ApiError> {
    need_admin(me)?;
    let u = person_by_name(s, username)?;
    let ended = match which {
        SignIn::Device(id) => s.revoke_token_of(&u.id, id)?,
        SignIn::Admin(id) | SignIn::Web(id) => s.end_admin_session_of(&u.id, id)?,
    };
    if !ended {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "That sign-in has ended already."));
    }
    tracing::info!("administrator '{}' ended a sign-in of '{}'", me.username, u.username);
    s.audit(Audit::by(me, "account.session_end").target(&u.username).detail(match which {
        SignIn::Device(_) => "a device",
        _ => "a browser session",
    }))?;
    Ok(u)
}

/// The server administrator signs a person out of every device and web admin session.
pub(crate) fn sign_out_everywhere_as(s: &Store, me: &User, username: &str) -> Result<(User, usize), ApiError> {
    need_admin(me)?;
    let u = person_by_name(s, username)?;
    let n = s.revoke_user_tokens(&u.id)?;
    tracing::info!("administrator '{}' signed '{}' out everywhere ({n} sign-in(s) ended)", me.username, u.username);
    s.audit(Audit::by(me, "account.sign_out").target(&u.username).detail(format!("{n} sign-in(s) ended")))?;
    Ok((u, n))
}

/// `GET /api/users/{username}/sessions` (server administrator).
async fn user_sessions(State(app): State<App>, headers: HeaderMap, Path(username): Path<String>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let (_, devices, web) = sign_ins_as(s, user_of(&who)?, &username)?;
        let devices: Vec<Value> = devices.iter().map(|t| json!({ "id": t.id, "device": t.device, "created": iso(t.created), "last_used": iso(t.last_used) })).collect();
        let web: Vec<Value> = web.iter().map(|w| json!({ "id": w.id, "kind": w.kind.as_str(), "agent": w.agent, "created": iso(w.created), "last_used": iso(w.last_used), "expires": iso(w.expires) })).collect();
        ok_json(json!({ "devices": devices, "web": web }))
    })
    .await
}

/// `DELETE /api/users/{username}/sessions/{device|web}/{id}` (server administrator).
async fn end_user_session(State(app): State<App>, headers: HeaderMap, Path((username, kind, id)): Path<(String, String, i64)>) -> ApiResult {
    let which = match kind.as_str() {
        "device" => SignIn::Device(id),
        "web" => SignIn::Admin(id),
        _ => return Err(ApiError::not_found()),
    };
    app.run_changing(move |s| {
        let who = auth(s, &headers)?;
        end_sign_in_as(s, user_of(&who)?, &username, which)?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

/// `POST /api/users/{username}/sign-out` (server administrator): every device and web admin
/// session of the person ends (live sessions too).
async fn sign_out_user(State(app): State<App>, headers: HeaderMap, Path(username): Path<String>) -> ApiResult {
    app.run_changing(move |s| {
        let who = auth(s, &headers)?;
        let (_, n) = sign_out_everywhere_as(s, user_of(&who)?, &username)?;
        ok_json(json!({ "signed_out": n }))
    })
    .await
}

/// The organisations `me` sees, with their role in each: their own (the server administrator:
/// all of them).
pub(crate) fn visible_orgs(s: &Store, me: &User) -> Result<Vec<(Org, Option<OrgRole>)>, ApiError> {
    let mine = s.orgs_of(&me.id)?;
    Ok(if me.is_admin {
        s.orgs()?
            .into_iter()
            .map(|o| {
                let r = mine.iter().find(|(m, _)| m.id == o.id).map(|(_, r)| *r);
                (o, r)
            })
            .collect()
    } else {
        mine.into_iter().map(|(o, r)| (o, Some(r))).collect()
    })
}

/// `GET /api/orgs`: the caller's organisations (the server administrator: all of them).
async fn list_orgs(State(app): State<App>, headers: HeaderMap) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let mut list = Vec::new();
        for (o, r) in &visible_orgs(s, me)? {
            list.push(org_json(s, o, *r)?);
        }
        ok_json(json!({ "orgs": list }))
    })
    .await
}

#[derive(Deserialize)]
struct OrgBody {
    name: String,
}

/// The server administrator creates an organisation.
pub(crate) fn create_org_as(s: &Store, me: &User, name: &str) -> Result<Org, ApiError> {
    need_admin(me)?;
    let o = s.create_org(name, &me.id)?;
    tracing::info!("user '{}' created the organisation '{}'", me.username, o.name);
    s.audit(Audit::by(me, "org.create").org(&o.id).detail(o.name.clone()))?;
    Ok(o)
}

async fn create_org(State(app): State<App>, headers: HeaderMap, body: Bytes) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        need_admin(me)?;
        let b: OrgBody = parse_body(&body)?;
        let o = create_org_as(s, me, &b.name)?;
        Ok((StatusCode::CREATED, axum::Json(org_json(s, &o, None)?)).into_response())
    })
    .await
}

fn members_json(s: &Store, org: &Org, with_email: bool) -> Result<Value, ApiError> {
    let list: Vec<Value> = s
        .members(&org.id)?
        .iter()
        .map(|m| {
            let mut v = json!({ "username": m.user.username, "display_name": m.user.display_name, "role": m.role.as_str(), "since": iso(m.created), "disabled": m.user.disabled });
            if with_email {
                v["email"] = json!(m.user.email);
            }
            v
        })
        .collect();
    Ok(json!(list))
}

/// `GET /api/orgs/{org}` (id or name): the organisation and its members.
async fn get_org(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let (o, mine) = org_for(s, me, &org)?;
        let mut v = org_json(s, &o, mine)?;
        v["member_list"] = members_json(s, &o, me.is_admin)?;
        ok_json(v)
    })
    .await
}

#[derive(Deserialize)]
struct PatchOrgBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    member_role: Option<String>,
}

/// Renames an organisation (the server administrator) and/or sets its members' default role
/// ("edit"/"view"; an org owner/admin). The organisation afterwards and the caller's role.
pub(crate) fn patch_org_as(s: &Store, me: &User, org: &str, name: Option<&str>, member_role: Option<&str>) -> Result<(Org, Option<OrgRole>), ApiError> {
    let (o, mine) = org_for(s, me, org)?;
    // Every check before any change: a request that may not do all of it changes nothing.
    let role = match member_role {
        Some(r) => {
            need_manage(me, mine)?;
            Some(Role::parse(r).filter(|r| *r != Role::Owner).ok_or_else(|| ApiError::bad("member_role must be view or edit."))?)
        }
        None => None,
    };
    if name.is_some() {
        need_admin(me)?;
    }
    // The rename can still be refused (taken, invalid name); it goes first, so then nothing
    // has changed either.
    if let Some(n) = name {
        let renamed = s.rename_org(&o, n)?;
        s.audit(Audit::by(me, "org.rename").org(&o.id).detail(format!("{} -> {}", o.name, renamed.name)))?;
    }
    if let Some(r) = role {
        s.set_org_member_role(&o, r)?;
        s.audit(Audit::by(me, "org.settings").org(&o.id).detail(format!("members can {} ({} before)", r.as_str(), o.member_role.as_str())))?;
    }
    Ok((s.org(&o.id)?.ok_or_else(ApiError::not_found)?, mine))
}

/// `PATCH /api/orgs/{org} {name?, member_role?}`: renaming takes the server administrator;
/// the members' default role ("edit"/"view") takes an org owner/admin.
async fn patch_org(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>, body: Bytes) -> ApiResult {
    app.run_changing(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let b: PatchOrgBody = parse_body(&body)?;
        let (o, mine) = patch_org_as(s, me, &org, b.name.as_deref(), b.member_role.as_deref())?;
        ok_json(org_json(s, &o, mine)?)
    })
    .await
}

/// The server administrator deletes an organisation whose folder is empty.
pub(crate) fn delete_org_as(s: &Store, me: &User, org: &str) -> Result<Org, ApiError> {
    need_admin(me)?;
    let (o, _) = org_for(s, me, org)?;
    s.delete_org(&o)?;
    tracing::info!("user '{}' deleted the organisation '{}'", me.username, o.name);
    s.audit(Audit::by(me, "org.delete").org(&o.id).detail(o.name.clone()))?;
    Ok(o)
}

/// `DELETE /api/orgs/{org}` (server administrator): only when its folder is empty.
async fn delete_org(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>) -> ApiResult {
    app.run_changing(move |s| {
        let who = auth(s, &headers)?;
        delete_org_as(s, user_of(&who)?, &org)?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

async fn org_members(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let (o, _) = org_for(s, me, &org)?;
        ok_json(json!({ "members": members_json(s, &o, me.is_admin)? }))
    })
    .await
}

#[derive(Deserialize)]
struct MemberBody {
    #[serde(default)]
    username: String,
    #[serde(default)]
    role: Option<String>,
}

/// Adds a person to an organisation (default role member) or changes their role, with every
/// rule of the API: org owners/admins only, owners for owners, `invite_required` for people
/// an org admin doesn't share an organisation with, the last owner stays. The organisation
/// and whether the person was added (rather than changed).
pub(crate) fn set_member_as(s: &Store, me: &User, org: &str, username: &str, role: Option<&str>) -> Result<(Org, bool), ApiError> {
    let (o, mine) = org_for(s, me, org)?;
    need_manage(me, mine)?;
    let role = OrgRole::parse(role.unwrap_or("member")).ok_or_else(|| ApiError::bad("role must be member, admin or owner."))?;
    let target = person_by_name(s, username)?;
    if target.disabled {
        return Err(ApiError::bad(format!("The user '{}' is disabled.", target.username)));
    }
    let before = s.member_role(&o.id, &target.id)?;
    need_owner_for(me, mine, &[Some(role), before])?;
    // Org admins add directly only people they already share an organisation with; anyone
    // else gets an invite (which they accept themselves). The server administrator adds anyone.
    if before.is_none() && !me.is_admin && !s.shares_org(&me.id, &target.id)? {
        return Err(ApiError::coded(StatusCode::FORBIDDEN, "invite_required", format!("{} isn't in any of your organisations; send them an invite link; they can accept it with their existing account.", target.username)));
    }
    if target.id == me.id && before == Some(OrgRole::Owner) && role != OrgRole::Owner {
        last_owner_check(s, &o)?;
    }
    let added = s.set_member(&o, &target, role, &me.id)?;
    tracing::info!("user '{}' {} '{}' as {} of '{}'", me.username, if added { "added" } else { "set" }, target.username, role.as_str(), o.name);
    let detail = match before {
        Some(b) => format!("{} -> {}", b.as_str(), role.as_str()),
        None => role.as_str().to_string(),
    };
    s.audit(Audit::by(me, if added { "member.add" } else { "member.role" }).org(&o.id).target(&target.username).detail(detail))?;
    Ok((o, added))
}

/// Changes the role of someone who is a member already (404 otherwise); see `set_member_as`.
pub(crate) fn change_member_as(s: &Store, me: &User, org: &str, username: &str, role: &str) -> Result<Org, ApiError> {
    let (o, _) = org_for(s, me, org)?;
    let target = person_by_name(s, username)?;
    if s.member_role(&o.id, &target.id)?.is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, format!("{} isn't a member of {}.", target.username, o.name)));
    }
    Ok(set_member_as(s, me, org, username, Some(role))?.0)
}

/// An organisation keeps at least one owner: its last owner can't leave or step down.
fn last_owner_check(s: &Store, o: &Org) -> Result<(), ApiError> {
    if s.members(&o.id)?.iter().filter(|m| m.role == OrgRole::Owner).count() <= 1 {
        return Err(ApiError::new(StatusCode::CONFLICT, format!("You are the last owner of {}. Make someone else an owner first.", o.name)));
    }
    Ok(())
}

/// `POST /api/orgs/{org}/members {username, role?}`: adds a person (default role member) or
/// changes their role.
async fn add_member(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>, body: Bytes) -> ApiResult {
    app.run_changing(move |s| {
        let b: MemberBody = parse_body(&body)?;
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let (o, added) = set_member_as(s, me, &org, &b.username, b.role.as_deref())?;
        let status = if added { StatusCode::CREATED } else { StatusCode::OK };
        Ok((status, axum::Json(json!({ "members": members_json(s, &o, me.is_admin)? }))).into_response())
    })
    .await
}

/// `PATCH /api/orgs/{org}/members/{username} {role}`
async fn patch_member(State(app): State<App>, headers: HeaderMap, Path((org, username)): Path<(String, String)>, body: Bytes) -> ApiResult {
    app.run_changing(move |s| {
        let b: MemberBody = parse_body(&body)?;
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let role = b.role.as_deref().ok_or_else(|| ApiError::bad("role is required."))?;
        let o = change_member_as(s, me, &org, &username, role)?;
        ok_json(json!({ "members": members_json(s, &o, me.is_admin)? }))
    })
    .await
}

/// Removes a person from an organisation: anyone may leave (not the last owner); removing
/// others takes an org owner/admin (an owner for owners).
pub(crate) fn remove_member_as(s: &Store, me: &User, org: &str, username: &str) -> Result<Org, ApiError> {
    let (o, mine) = org_for(s, me, org)?;
    let target = person_by_name(s, username)?;
    let theirs = s.member_role(&o.id, &target.id)?;
    if target.id != me.id {
        need_manage(me, mine)?;
        need_owner_for(me, mine, &[theirs])?;
    } else if theirs == Some(OrgRole::Owner) {
        last_owner_check(s, &o)?;
    }
    if theirs.is_none() || !s.remove_member(&o.id, &target.id)? {
        return Err(ApiError::new(StatusCode::NOT_FOUND, format!("{} isn't a member of {}.", target.username, o.name)));
    }
    tracing::info!("user '{}' removed '{}' from '{}'", me.username, target.username, o.name);
    s.audit(Audit::by(me, "member.remove").org(&o.id).target(&target.username).detail(if target.id == me.id { "left" } else { "removed" }))?;
    Ok(o)
}

/// `DELETE /api/orgs/{org}/members/{username}`: anyone may leave; removing others takes an
/// org owner/admin. Their access to the org folder ends at once (live sessions included).
async fn remove_member(State(app): State<App>, headers: HeaderMap, Path((org, username)): Path<(String, String)>) -> ApiResult {
    app.run_changing(move |s| {
        let who = auth(s, &headers)?;
        remove_member_as(s, user_of(&who)?, &org, &username)?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

fn invite_json(s: &Store, i: &crate::store::Invite) -> Value {
    json!({ "id": i.id, "email": i.email, "role": i.role.as_str(), "expires": iso(i.expires), "created": iso(i.created), "created_by": s.display_name(&i.created_by) })
}

#[derive(Deserialize)]
struct InviteBody {
    #[serde(default)]
    email: String,
    #[serde(default)]
    role: Option<String>,
}

/// An org owner/admin makes a one-time invite (an owner for owner invites): the invite and its
/// URL, which is shown once (only the token's hash is stored).
pub(crate) fn create_invite_as(s: &Store, me: &User, base_url: &str, org: &str, email: &str, role: Option<&str>) -> Result<(crate::store::Invite, Org, String), ApiError> {
    let (o, mine) = org_for(s, me, org)?;
    need_manage(me, mine)?;
    let role = OrgRole::parse(role.unwrap_or("member")).ok_or_else(|| ApiError::bad("role must be member, admin or owner."))?;
    need_owner_for(me, mine, &[Some(role)])?;
    let (inv, token) = s.create_invite(&o, email, role, &me.id)?;
    tracing::info!("user '{}' created an invite to '{}'", me.username, o.name);
    // The email the invite is for, never the link.
    s.audit(Audit::by(me, "invite.create").org(&o.id).detail(format!("{} as {}", if inv.email.is_empty() { "no email" } else { &inv.email }, role.as_str())))?;
    Ok((inv, o, format!("{base_url}/i/{token}")))
}

/// `POST /api/orgs/{org}/invites {email, role?}`: a one-time link (valid 7 days), returned
/// once in `url` for the admin to send. Only its hash is stored.
async fn create_invite(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>, body: Bytes) -> ApiResult {
    let base = app.cfg.base_url();
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let b: InviteBody = parse_body(&body)?;
        let (inv, _, url) = create_invite_as(s, me, &base, &org, &b.email, b.role.as_deref())?;
        let mut v = invite_json(s, &inv);
        v["url"] = json!(url);
        Ok((StatusCode::CREATED, axum::Json(v)).into_response())
    })
    .await
}

/// An organisation's open invites (org owners/admins).
pub(crate) fn invites_as(s: &Store, me: &User, org: &str) -> Result<Vec<crate::store::Invite>, ApiError> {
    let (o, mine) = org_for(s, me, org)?;
    need_manage(me, mine)?;
    Ok(s.invites_of(&o.id)?)
}

async fn list_invites(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let list: Vec<Value> = invites_as(s, user_of(&who)?, &org)?.iter().map(|i| invite_json(s, i)).collect();
        ok_json(json!({ "invites": list }))
    })
    .await
}

/// Revokes an open invite of an organisation (org owners/admins; 404 for another org's).
pub(crate) fn delete_invite_as(s: &Store, me: &User, org: &str, id: &str) -> Result<(), ApiError> {
    let (o, mine) = org_for(s, me, org)?;
    need_manage(me, mine)?;
    match s.invite(id)? {
        Some(i) if i.org_id == o.id => {
            s.delete_invite(&i.id)?;
            tracing::info!("user '{}' revoked an invite to '{}'", me.username, o.name);
            s.audit(Audit::by(me, "invite.revoke").org(&o.id).detail(if i.email.is_empty() { "no email".to_string() } else { i.email.clone() }))?;
            Ok(())
        }
        _ => Err(ApiError::not_found()),
    }
}

async fn delete_invite(State(app): State<App>, headers: HeaderMap, Path((org, id)): Path<(String, String)>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        delete_invite_as(s, user_of(&who)?, &org, &id)?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

fn usage_json(u: &crate::store::Usage) -> Value {
    json!({ "files": u.files, "versions": u.versions, "bytes": u.bytes })
}

/// `GET /api/orgs/{org}/storage` (org owners/admins): what the org's files take, every kept
/// version and the trash included.
async fn org_storage(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let (o, mine) = org_for(s, me, &org)?;
        need_manage(me, mine)?;
        let mut v = usage_json(&s.usage_of(&o.account_id)?);
        v["org"] = json!({ "id": o.id, "name": o.name });
        ok_json(v)
    })
    .await
}

/// `GET /api/admin/storage` (server administrator): storage per person and per organisation.
async fn admin_storage(State(app): State<App>, headers: HeaderMap) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        need_admin(user_of(&who)?)?;
        let usage = s.usage()?;
        let of = |id: &str| usage.iter().find(|(o, _)| o == id).map(|(_, u)| u.clone()).unwrap_or_default();
        let users: Vec<Value> = s
            .users()?
            .iter()
            .map(|u| {
                let mut v = usage_json(&of(&u.id));
                v["username"] = json!(u.username);
                v["display_name"] = json!(u.display_name);
                v
            })
            .collect();
        let orgs: Vec<Value> = s
            .orgs()?
            .iter()
            .map(|o| {
                let mut v = usage_json(&of(&o.account_id));
                v["id"] = json!(o.id);
                v["name"] = json!(o.name);
                v
            })
            .collect();
        let total: i64 = usage.iter().map(|(_, u)| u.bytes).sum();
        ok_json(json!({ "users": users, "orgs": orgs, "bytes": total }))
    })
    .await
}

// ---------------------------------------------------------------------------
// Audit log
// ---------------------------------------------------------------------------

/// Audit filters as they come from a URL (`before`, `limit`, `org`, `user`, `action`).
#[derive(Clone, Debug, Default)]
pub(crate) struct AuditFilter {
    pub(crate) before: Option<i64>,
    pub(crate) limit: usize,
    pub(crate) org: String,
    pub(crate) user: String,
    pub(crate) action: String,
}

impl AuditFilter {
    pub(crate) fn from_query(q: &HashMap<String, String>) -> Result<AuditFilter, ApiError> {
        let get = |k: &str| q.get(k).map(|v| v.trim().to_string()).unwrap_or_default();
        let before = match get("before").as_str() {
            "" => None,
            v => Some(v.parse::<i64>().map_err(|_| ApiError::bad("before must be a number."))?),
        };
        let limit = match get("limit").as_str() {
            "" => 50,
            v => v.parse::<usize>().map_err(|_| ApiError::bad("limit must be a number."))?.clamp(1, 200),
        };
        let action = get("action");
        if action.len() > 64 || !action.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c == '.') {
            return Err(ApiError::bad("action may contain only a-z, '_' and '.'."));
        }
        Ok(AuditFilter { before, limit, org: get("org"), user: get("user"), action })
    }
}

/// The audit events `me` may see with filter `f`, and the organisations they may filter by:
/// the server administrator sees everything; an org owner/admin the events of the
/// organisations they manage; anyone else gets 403. An organisation in the filter that `me`
/// doesn't manage is 404, like elsewhere.
pub(crate) fn audit_as(s: &Store, me: &User, f: &AuditFilter) -> Result<(Vec<crate::store::AuditEvent>, Vec<Org>), ApiError> {
    let managed: Vec<Org> = visible_orgs(s, me)?.into_iter().filter(|(_, r)| me.is_admin || r.is_some_and(OrgRole::manages)).map(|(o, _)| o).collect();
    if !me.is_admin && managed.is_empty() {
        return Err(ApiError::forbidden("Only the server administrator and organisation owners and admins can see the audit log."));
    }
    let org = match f.org.as_str() {
        "" => None,
        o => {
            let (org, mine) = org_for(s, me, o)?;
            need_manage(me, mine)?;
            Some(org.id)
        }
    };
    let q = crate::store::AuditQuery {
        before: f.before,
        limit: f.limit,
        within_orgs: if me.is_admin { None } else { Some(managed.iter().map(|o| o.id.clone()).collect()) },
        org,
        user: Some(f.user.clone()).filter(|u| !u.is_empty()),
        action: Some(f.action.clone()).filter(|a| !a.is_empty()),
    };
    Ok((s.audit_events(&q)?, managed))
}

pub(crate) fn audit_json(s: &Store, e: &crate::store::AuditEvent) -> Value {
    let org = e.org_id.as_deref().and_then(|o| s.org(o).ok().flatten());
    json!({
        "id": e.id,
        "at": iso(e.at),
        "actor": e.actor_name,
        "actor_id": e.actor_id,
        "ip": e.ip,
        "action": e.action,
        "org": e.org_id.as_ref().map(|id| json!({ "id": id, "name": org.as_ref().map(|o| o.name.clone()) })),
        "item": e.item_id,
        "target_user": e.target_user,
        "detail": e.detail,
    })
}

/// `GET /api/admin/audit?before=&limit=&org=&user=&action=`: newest first; `next_before` pages on.
async fn audit_log(State(app): State<App>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let f = AuditFilter::from_query(&q)?;
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let (events, _) = audit_as(s, user_of(&who)?, &f)?;
        let next = if events.len() == f.limit { events.last().map(|e| e.id) } else { None };
        let list: Vec<Value> = events.iter().map(|e| audit_json(s, e)).collect();
        ok_json(json!({ "events": list, "next_before": next }))
    })
    .await
}

// ---------------------------------------------------------------------------
// Invites (no account yet)
// ---------------------------------------------------------------------------

/// The invite behind a token, or 404 (counted as a failure for the address, up front so
/// parallel guesses are limited too).
async fn live_invite(app: &App, ip_key: &str, token: String) -> Result<(crate::store::Invite, Org), ApiError> {
    if app.attempt(&[ip_key.to_string()]).is_none() {
        return Err(too_many());
    }
    let r = app
        .run(move |s| {
            let inv = s.invite_by_token(&token)?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "This invite doesn't exist, was used already or has expired."))?;
            let org = s.org(&inv.org_id)?.ok_or_else(ApiError::not_found)?;
            Ok((inv, org))
        })
        .await;
    if r.is_ok() {
        app.limiter().unfail(ip_key);
    }
    r
}

/// `GET /api/invites/{token}`: what the invite is for.
async fn invite_info(State(app): State<App>, ConnectInfo(Peer(peer)): ConnectInfo<Peer>, headers: HeaderMap, Path(token): Path<String>) -> ApiResult {
    let ip_key = format!("ip:{}", client_ip(&app.cfg, peer, &headers));
    let (inv, org) = live_invite(&app, &ip_key, token).await?;
    ok_json(json!({
        "org": { "id": org.id, "name": org.name },
        "email": inv.email,
        "role": inv.role.as_str(),
        "expires": iso(inv.expires),
        "accept_with": ["new_account", "existing_account"],
    }))
}

#[derive(Deserialize)]
struct AcceptBody {
    username: String,
    password: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    device: String,
    /// Accept with an account that exists already (`username` and `password` are its
    /// sign-in) instead of creating one.
    #[serde(default)]
    existing: bool,
}

/// What accepting an invite did.
enum Accepted {
    /// A new account was made.
    Created(User, Org),
    /// An existing account was added, or (`AlreadyMember`) was in the organisation already.
    Existing(crate::store::InviteAccepted),
}

impl Accepted {
    fn user_org(&self) -> (&User, &Org) {
        use crate::store::InviteAccepted as A;
        match self {
            Accepted::Created(u, o) | Accepted::Existing(A::Joined(u, o, _) | A::AlreadyMember(u, o, _)) => (u, o),
        }
    }

    fn already_member(&self) -> bool {
        matches!(self, Accepted::Existing(crate::store::InviteAccepted::AlreadyMember(..)))
    }
}

/// Accepts an invite either way (see `accept` and `accept_existing`) and logs what happened.
async fn accept_any(app: &App, ip: IpAddr, token: String, b: AcceptBody) -> Result<Accepted, ApiError> {
    let ip_key = format!("ip:{ip}");
    let r = if b.existing {
        Accepted::Existing(accept_existing(app, ip, &ip_key, token, &b.username, b.password).await?)
    } else {
        let (u, o) = accept(app, &ip_key, token, b).await?;
        Accepted::Created(u, o)
    };
    let (u, o) = r.user_org();
    match &r {
        Accepted::Created(..) => tracing::info!("'{}' joined '{}' through an invite", u.username, o.name),
        _ if r.already_member() => tracing::info!("'{}' opened an invite to '{}' but is a member already", u.username, o.name),
        _ => tracing::info!("'{}' joined '{}' through an invite (existing account)", u.username, o.name),
    }
    if !r.already_member() {
        let detail = match &r {
            Accepted::Created(..) => "new account".to_string(),
            Accepted::Existing(crate::store::InviteAccepted::Joined(_, _, role)) => format!("existing account, as {}", role.as_str()),
            Accepted::Existing(_) => "existing account".to_string(),
        };
        let a = Audit::by(u, "invite.accept").org(&o.id).target(&u.username).ip(ip.to_string()).detail(detail);
        let _ = app.run(move |s| Ok(s.audit(a)?)).await;
    }
    Ok(r)
}

/// Adds an existing account to the invite's organisation. The sign-in is checked exactly as
/// `/api/login` checks it (invalid usernames refused first, the attempt counted up front in
/// the limiter, the delay, Argon2 in its slots); a disabled account or one with a temporary
/// password gets a clear refusal, but only after its password was right.
async fn accept_existing(app: &App, ip: IpAddr, ip_key: &str, token: String, username: &str, password: String) -> Result<crate::store::InviteAccepted, ApiError> {
    live_invite(app, ip_key, token.clone()).await?;
    let user = verify_sign_in(app, ip, username, password, true, "invite").await?;
    if user.disabled {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "This account is disabled, so it can't join an organisation. Ask your administrator."));
    }
    if user.must_change_password {
        return Err(ApiError::coded(
            StatusCode::FORBIDDEN,
            "password_change_required",
            "This account still has a temporary password. Sign in from FreedomOffice Sheets or the web admin and choose your own password first, then open this link again.",
        ));
    }
    let id = user.id;
    app.run_changing(move |s| Ok(s.accept_invite_existing(&token, &id)?)).await
}

/// Creates the account and membership for an invite. The username and password are checked
/// first; the password is hashed outside the store lock (see `App::argon`).
async fn accept(app: &App, ip_key: &str, token: String, b: AcceptBody) -> Result<(User, Org), ApiError> {
    live_invite(app, ip_key, token.clone()).await?;
    let name = b.username.clone();
    let username = app.run(move |s| Ok(s.check_new_username(&name)?)).await?;
    let hash = app.new_password_hash(b.password, username.clone()).await?;
    app.run_changing(move |s| Ok(s.accept_invite(&token, &username, &b.display_name, &hash)?)).await
}

/// `POST /api/invites/{token} {username, password, display_name?, device?, existing?}`:
/// creates the account (with the password the person chose), or with `existing: true` checks
/// the sign-in of an account that exists already, and joins the organisation; signs in.
/// 201 when someone joined; 200 with `already_member: true` (invite not used) otherwise.
async fn accept_invite(State(app): State<App>, ConnectInfo(Peer(peer)): ConnectInfo<Peer>, headers: HeaderMap, Path(token): Path<String>, body: Bytes) -> ApiResult {
    let ip = client_ip(&app.cfg, peer, &headers);
    let b: AcceptBody = parse_body(&body)?;
    let device = b.device.clone();
    let r = accept_any(&app, ip, token, b).await?;
    let already = r.already_member();
    let (user, org) = r.user_org();
    let (user, org) = (user.clone(), org.clone());
    let role = match &r {
        Accepted::Created(..) => None,
        Accepted::Existing(crate::store::InviteAccepted::Joined(_, _, role) | crate::store::InviteAccepted::AlreadyMember(_, _, role)) => Some(role.as_str()),
    };
    app.run(move |s| {
        let token = s.create_token(&user.id, &device)?;
        let mut v = json!({ "token": token, "user": user_json(&user), "org": { "id": org.id, "name": org.name }, "already_member": already });
        if let Some(role) = role {
            v["role"] = json!(role);
        }
        if already {
            v["message"] = json!(format!("You're already a member of {}.", org.name));
        }
        Ok((if already { StatusCode::OK } else { StatusCode::CREATED }, axum::Json(v)).into_response())
    })
    .await
}

fn invite_form_html(token: &str, org: &str, email: &str, error: &str) -> String {
    let err = if error.is_empty() { String::new() } else { format!("<p style=\"color:#b00020\">{}</p>", html_escape(error)) };
    let to = if email.is_empty() { String::new() } else { format!(" This invite was sent to <b>{}</b>.", html_escape(email)) };
    let action = html_escape(token);
    format!(
        "<p>You were invited to join <b>{}</b> on this FreedomOffice server.{to}</p>{err}\
         <h3>I already have an account</h3>\
         <p>Sign in with your username and password to add your account to {}.</p>\
         <form method=\"post\" action=\"{action}\"><input type=\"hidden\" name=\"existing\" value=\"1\">\
         <p><label>Username<br><input name=\"username\" autocomplete=\"username\" required maxlength=\"64\"></label></p>\
         <p><label>Password<br><input type=\"password\" name=\"password\" autocomplete=\"current-password\" required></label></p>\
         <p><button type=\"submit\">Join with my account</button></p></form>\
         <h3>Create an account</h3>\
         <p>Choose a username and a password (at least {} characters).</p>\
         <form method=\"post\" action=\"{action}\">\
         <p><label>Username<br><input name=\"username\" autocomplete=\"username\" required maxlength=\"64\"></label></p>\
         <p><label>Your name<br><input name=\"display_name\" autocomplete=\"name\" maxlength=\"100\"></label></p>\
         <p><label>Password<br><input type=\"password\" name=\"password\" autocomplete=\"new-password\" required minlength=\"{}\"></label></p>\
         <p><button type=\"submit\">Create my account</button></p></form>",
        html_escape(org),
        html_escape(org),
        util::MIN_PASSWORD,
        util::MIN_PASSWORD
    )
}

/// `GET /i/{token}`: the invite page a browser shows (sign in with an existing account, or
/// choose a username and password for a new one).
async fn invite_page(State(app): State<App>, ConnectInfo(Peer(peer)): ConnectInfo<Peer>, headers: HeaderMap, Path(token): Path<String>) -> Response {
    let ip_key = format!("ip:{}", client_ip(&app.cfg, peer, &headers));
    match live_invite(&app, &ip_key, token.clone()).await {
        Ok((inv, org)) => page("Join FreedomOffice", &invite_form_html(&token, &org.name, &inv.email, "")),
        Err(e) => {
            let mut res = page("FreedomOffice", "<p>This invite doesn't exist, was used already or has expired. Ask for a new one.</p>");
            *res.status_mut() = e.status;
            res
        }
    }
}

/// `POST /i/{token}` from the invite page (either form; `existing=1` for the sign-in one).
async fn invite_form(State(app): State<App>, ConnectInfo(Peer(peer)): ConnectInfo<Peer>, headers: HeaderMap, Path(token): Path<String>, body: Bytes) -> Response {
    let ip = client_ip(&app.cfg, peer, &headers);
    let b = AcceptBody {
        username: form_value(&body, "username"),
        password: form_value(&body, "password"),
        display_name: form_value(&body, "display_name"),
        device: String::new(),
        existing: form_value(&body, "existing") == "1",
    };
    match accept_any(&app, ip, token.clone(), b).await {
        Ok(r) => {
            let (user, org) = r.user_org();
            let (who, org) = (html_escape(&user.username), html_escape(&org.name));
            let text = match r {
                Accepted::Created(..) => format!("<p>Your account <b>{who}</b> was created and you are now in {org}.</p>"),
                _ if r.already_member() => format!("<p>Your account <b>{who}</b> is already a member of {org}; nothing changed.</p>"),
                _ => format!("<p>Your account <b>{who}</b> is now in {org}.</p>"),
            };
            page("Welcome to FreedomOffice", &format!("{text}<p>Sign in from FreedomOffice Sheets (File &gt; Account) with your username and password.</p>"))
        }
        Err(e) => {
            let msg = e.body["error"].as_str().unwrap_or("Something went wrong.").to_string();
            let org = app.run(move |s| Ok(s.invite_by_token(&token)?.and_then(|i| s.org(&i.org_id).ok().flatten().map(|o| (o.name, i.email))).map(|(n, e)| (n, e, token)))).await.ok().flatten();
            let mut res = match org {
                Some((name, email, token)) => page("Join FreedomOffice", &invite_form_html(&token, &name, &email, &msg)),
                None => page("FreedomOffice", &format!("<p>{}</p>", html_escape(&msg))),
            };
            *res.status_mut() = e.status;
            res
        }
    }
}

async fn users(State(app): State<App>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let list: Vec<Value> = s.search_users_for(me, q.get("query").map(String::as_str).unwrap_or(""), 21)?.iter().filter(|u| u.id != me.id).take(20).map(person).collect();
        ok_json(json!({ "users": list }))
    })
    .await
}

// ---------------------------------------------------------------------------
// Browsing
// ---------------------------------------------------------------------------

async fn root(State(app): State<App>, headers: HeaderMap) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let root = user_of(&who)?.root_id.clone();
        let (it, role) = visible(s, &who, &root, false)?;
        ok_json(item_json(s, &it, role))
    })
    .await
}

async fn get_item(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let (it, role) = visible(s, &who, &id, false)?;
        ok_json(item_json(s, &it, role))
    })
    .await
}

async fn children(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let (it, _) = visible(s, &who, &id, false)?;
        if it.kind != Kind::Folder {
            return Err(ApiError::bad("That isn't a folder."));
        }
        ok_json(items_json(s, &who, &s.children(&it.id)?)?)
    })
    .await
}

async fn shared_with_me(State(app): State<App>, headers: HeaderMap) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        user_of(&who)?;
        ok_json(items_json(s, &who, &s.shared_with(&who)?)?)
    })
    .await
}

async fn recent(State(app): State<App>, headers: HeaderMap) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        user_of(&who)?;
        ok_json(items_json(s, &who, &s.recent(&who, 50)?)?)
    })
    .await
}

async fn trash(State(app): State<App>, headers: HeaderMap) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let items = s.trash_of(&user_of(&who)?.id)?;
        ok_json(items_json(s, &who, &items)?)
    })
    .await
}

// ---------------------------------------------------------------------------
// Creating, reading and saving
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct FolderBody {
    #[serde(default)]
    parent: Option<String>,
    name: String,
}

async fn create_folder(State(app): State<App>, headers: HeaderMap, body: Bytes) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let b: FolderBody = parse_body(&body)?;
        let parent = match b.parent.filter(|p| !p.is_empty()) {
            Some(p) => p,
            None => user_of(&who)?.root_id.clone(),
        };
        let (p, role) = visible(s, &who, &parent, false)?;
        need_edit(role)?;
        let it = s.create_folder(&p.id, &b.name, &actor(&who))?;
        let role = s.role(&who, &it, false)?.unwrap_or(role);
        Ok((StatusCode::CREATED, axum::Json(item_json(s, &it, role))).into_response())
    })
    .await
}

/// Who uploads into which folder, under which name: checked before the body is read (so
/// nobody without edit access can make the server store anything) and again when it is stored.
struct UploadTarget {
    who: Principal,
    parent: Item,
    role: Role,
    on_conflict: OnConflict,
    name: String,
}

fn upload_target(s: &Store, hub: &crate::live::Hub, headers: &HeaderMap, q: &HashMap<String, String>) -> Result<UploadTarget, ApiError> {
    let who = auth(s, headers)?;
    let on_conflict = OnConflict::parse(q.get("conflict").map(String::as_str).unwrap_or("")).ok_or_else(|| ApiError::bad("conflict must be fail, rename or replace."))?;
    let name = q.get("name").cloned().unwrap_or_default();
    let parent = match q.get("parent").filter(|p| !p.is_empty() && p.as_str() != "root") {
        Some(p) => p.clone(),
        None => user_of(&who)?.root_id.clone(),
    };
    let (p, role) = visible(s, &who, &parent, false)?;
    need_edit(role)?;
    if p.kind != Kind::Folder {
        return Err(ApiError::bad("Files can only be uploaded into folders."));
    }
    let clean = util::clean_name(&name).map_err(ApiError::bad)?;
    match (s.find_child(&p.id, &clean)?, on_conflict) {
        (Some(_), OnConflict::Fail) => return Err(ApiError::new(StatusCode::CONFLICT, format!("An item named '{clean}' already exists here."))),
        (Some(existing), OnConflict::Replace) if live_busy(s, hub, &existing)? => return Err(locked()),
        _ => {}
    }
    Ok(UploadTarget { who, parent: p, role, on_conflict, name })
}

/// `POST /api/files?parent=&name=&conflict=fail|rename|replace` with the bytes as the body
/// (streamed to disk; 413 past `max_upload_mb`). 201 for a new file, 200 for a new version
/// of an existing one (`replace`).
async fn upload_file(State(app): State<App>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>, body: axum::body::Body) -> ApiResult {
    let (hub, h, qq) = (app.hub.clone(), headers.clone(), q.clone());
    app.run(move |s| upload_target(s, &hub, &h, &qq).map(|_| ())).await?;
    let staged = crate::content::stage(&app.incoming, &headers, body, app.cfg.max_upload_bytes()).await?;
    let hub = app.hub.clone();
    app.run(move |s| {
        let t = upload_target(s, &hub, &headers, &q)?;
        let blob = crate::store::Blob::Staged { path: &staged.path, size: staged.size, hash: &staged.hash };
        let (it, created) = s.create_file_from(&t.parent.id, &t.name, blob, &actor(&t.who), t.on_conflict)?;
        drop(staged);
        if created {
            s.audit_item(audit_by(&t.who, "file.upload").detail(format!("{} ({} bytes) in {}", it.name, it.size, t.parent.name)), &it)?;
        }
        if let Principal::User { user, .. } = &t.who {
            s.touch_recent(&user.id, &it.id)?;
        }
        let role = s.role(&t.who, &it, false)?.unwrap_or(t.role);
        let status = if created { StatusCode::CREATED } else { StatusCode::OK };
        Ok((status, axum::Json(item_json(s, &it, role))).into_response())
    })
    .await
}

fn quoted(etag: &str) -> HeaderValue {
    HeaderValue::from_str(&format!("\"{etag}\"")).unwrap_or(HeaderValue::from_static("\"\""))
}

async fn get_content(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let (it, _) = visible(s, &who, &id, false)?;
        if it.kind != Kind::File {
            return Err(ApiError::bad("Folders have no content."));
        }
        let version = match q.get("version") {
            Some(v) => Some(v.parse::<i64>().map_err(|_| ApiError::bad("version must be a number."))?),
            None => None,
        };
        // Share links give the current version only; the history is for people with access.
        if version.is_some() && matches!(who, Principal::Link { .. }) {
            return Err(ApiError::not_found());
        }
        let (file, len, ver) = s.open_content(&it, version)?;
        if let (Principal::User { user, .. }, None) = (&who, version) {
            s.touch_recent(&user.id, &it.id)?;
        }
        // Where a live session starts from this version (for joining it).
        let live = if version.is_none() { Some((s.live_base(&it)?, s.live_state_blob(&it.id, ver.version)?.is_some())) } else { None };
        // Streamed from disk; one Range; `?download=1` for "save as".
        let download = q.get("download").is_some_and(|v| v == "1");
        let mut res = crate::content::file_response(&headers, file, len, &it.name, &crate::store::etag(ver.version, &ver.hash), download)?;
        let h = res.headers_mut();
        if let Some((l, state)) = live {
            h.insert("x-live-seq", HeaderValue::from(l.seq));
            if let Ok(v) = HeaderValue::from_str(&l.sheets) {
                h.insert("x-live-sheets", v);
            }
            if state {
                h.insert("x-live-state", HeaderValue::from_static("1"));
            }
        }
        h.insert("x-version", HeaderValue::from(ver.version));
        Ok(res)
    })
    .await
}

/// `"v3-abc"`, `W/"v3-abc"` and `v3-abc` all mean `v3-abc`.
fn norm_etag(s: &str) -> String {
    s.trim().trim_start_matches("W/").trim_matches('"').to_string()
}

/// The caller, the file and their role; or the 412 answer to send.
type PutTarget = Result<(Principal, Item, Role), Box<Response>>;

/// Whether the caller may save new content into `id` now: `Ok(Err(412 response))` when
/// `If-Match` names another version. Checked before the body is read and again before it is
/// stored (someone may have saved meanwhile).
fn put_target(s: &Store, hub: &crate::live::Hub, headers: &HeaderMap, id: &str) -> Result<PutTarget, ApiError> {
    let who = auth(s, headers)?;
    let (it, role) = visible(s, &who, id, false)?;
    if it.kind != Kind::File {
        return Err(ApiError::bad("Folders have no content."));
    }
    need_edit(role)?;
    if live_busy(s, hub, &it)? {
        return Err(locked());
    }
    if let Some(want) = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok()).map(norm_etag)
        && want != "*"
        && want != it.etag()
    {
        let body = json!({
            "error": "Someone else saved changes to this file.",
            "etag": it.etag(),
            "version": it.version,
            "modified_by": display_of(s, &it.modified_by),
            "modified": iso(it.modified),
        });
        return Ok(Err(Box::new((StatusCode::PRECONDITION_FAILED, axum::Json(body)).into_response())));
    }
    Ok(Ok((who, it, role)))
}

/// `PUT /api/items/{id}/content` (optional `If-Match: "<etag>"`): the body, streamed to disk
/// (413 past `max_upload_mb`), becomes the next version.
async fn put_content(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>, body: axum::body::Body) -> ApiResult {
    let (hub, h, i) = (app.hub.clone(), headers.clone(), id.clone());
    if let Err(res) = app.run(move |s| put_target(s, &hub, &h, &i).map(|r| r.map(|_| ()))).await? {
        return Ok(*res);
    }
    let staged = crate::content::stage(&app.incoming, &headers, body, app.cfg.max_upload_bytes()).await?;
    let hub = app.hub.clone();
    app.run(move |s| {
        let (who, it, role) = match put_target(s, &hub, &headers, &id)? {
            Ok(t) => t,
            Err(res) => return Ok(*res),
        };
        let saved = s.write_content_from(&it, crate::store::Blob::Staged { path: &staged.path, size: staged.size, hash: &staged.hash }, &actor(&who))?;
        drop(staged);
        if let Principal::User { user, .. } = &who {
            s.touch_recent(&user.id, &saved.id)?;
        }
        let mut res = axum::Json(item_json(s, &saved, role)).into_response();
        res.headers_mut().insert(header::ETAG, quoted(&saved.etag()));
        Ok(res)
    })
    .await
}

// ---------------------------------------------------------------------------
// Rename, move, trash, versions
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct PatchBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    parent: Option<String>,
}

async fn patch_item(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>, body: Bytes) -> ApiResult {
    // A move can change who has access (grants on the old and new parent folders).
    app.run_changing(move |s| {
        let who = auth(s, &headers)?;
        let b: PatchBody = parse_body(&body)?;
        let (it, role) = visible(s, &who, &id, false)?;
        need_edit(role)?;
        let parent = match b.parent.filter(|p| !p.is_empty()) {
            Some(p) => {
                let (pi, prole) = visible(s, &who, &p, false)?;
                need_edit(prole)?;
                Some(pi)
            }
            None => None,
        };
        let updated = s.update_item(&it, b.name.as_deref(), parent.as_ref(), &actor(&who))?;
        if updated.name != it.name {
            s.audit_item(audit_by(&who, "item.rename").detail(format!("{} -> {}", it.name, updated.name)), &updated)?;
        }
        if updated.parent_id != it.parent_id {
            let name_of = |id: &Option<String>| id.as_deref().and_then(|i| s.item(i).ok().flatten()).map(|p| p.name).unwrap_or_default();
            s.audit_item(audit_by(&who, "item.move").detail(format!("{}: {} -> {}", updated.name, name_of(&it.parent_id), name_of(&updated.parent_id))), &updated)?;
        }
        let role = s.role(&who, &updated, false)?.unwrap_or(role);
        ok_json(item_json(s, &updated, role))
    })
    .await
}

async fn delete_item(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    app.run_changing(move |s| {
        let who = auth(s, &headers)?;
        user_of(&who)?;
        let (it, role) = visible(s, &who, &id, false)?;
        need_edit(role)?;
        s.trash(&it, &actor(&who))?;
        s.audit_item(audit_by(&who, "item.trash").detail(it.name.clone()), &it)?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

async fn restore_item(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        user_of(&who)?;
        let (it, role) = visible(s, &who, &id, true)?;
        need_edit(role)?;
        let restored = s.restore(&it)?;
        if it.deleted != 0 {
            s.audit_item(audit_by(&who, "item.restore").detail(restored.name.clone()), &restored)?;
        }
        let role = s.role(&who, &restored, false)?.unwrap_or(role);
        ok_json(item_json(s, &restored, role))
    })
    .await
}

async fn versions(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        user_of(&who)?;
        let (it, _) = visible(s, &who, &id, false)?;
        let list: Vec<Value> = s
            .versions(&it.id)?
            .iter()
            .map(|v| {
                json!({
                    "version": v.version,
                    "size": v.size,
                    "etag": crate::store::etag(v.version, &v.hash),
                    "modified": iso(v.modified),
                    "modified_by": display_of(s, &v.modified_by),
                    "current": v.version == it.version,
                })
            })
            .collect();
        ok_json(json!({ "versions": list }))
    })
    .await
}

async fn restore_version(State(app): State<App>, headers: HeaderMap, Path((id, n)): Path<(String, i64)>) -> ApiResult {
    let hub = app.hub.clone();
    app.run(move |s| {
        let who = auth(s, &headers)?;
        user_of(&who)?;
        let (it, role) = visible(s, &who, &id, false)?;
        need_edit(role)?;
        if live_busy(s, &hub, &it)? {
            return Err(locked());
        }
        let saved = s.restore_version(&it, n, &actor(&who))?;
        s.audit_item(audit_by(&who, "item.version_restore").detail(format!("{}: version {n} saved as version {}", saved.name, saved.version)), &saved)?;
        ok_json(item_json(s, &saved, role))
    })
    .await
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

fn grant_json(g: &Grant) -> Value {
    json!({
        "username": g.user.username,
        "display_name": g.user.display_name,
        "role": g.role.as_str(),
        "inherited": g.inherited_from.is_some(),
        "inherited_from": g.inherited_from.as_ref().map(|f| json!({ "id": f.id, "name": f.name })),
    })
}

fn org_grant_json(g: &OrgGrant) -> Value {
    json!({
        "org": { "id": g.org.id, "name": g.org.name },
        "role": g.role.as_str(),
        "inherited": g.inherited_from.is_some(),
        "inherited_from": g.inherited_from.as_ref().map(|f| json!({ "id": f.id, "name": f.name })),
    })
}

/// `permissions`: grants to people; `orgs`: grants to whole organisations.
fn permissions_json(s: &Store, it: &Item) -> Result<Value, ApiError> {
    let list: Vec<Value> = s.grants(it)?.iter().map(grant_json).collect();
    let orgs: Vec<Value> = s.org_grants(it)?.iter().map(org_grant_json).collect();
    Ok(json!({ "permissions": list, "orgs": orgs }))
}

async fn permissions(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        user_of(&who)?;
        let (it, role) = visible(s, &who, &id, false)?;
        let owner = s.user_by_id(&it.owner_id)?.map(|u| person(&u));
        let mut v = permissions_json(s, &it)?;
        v["owner"] = json!(owner);
        v["role"] = json!(role.as_str());
        ok_json(v)
    })
    .await
}

/// `{"username": ..., "role": ...}` for a person, or `{"org": "<name or id>", "role": ...}`
/// for everyone in an organisation.
#[derive(Deserialize)]
struct GrantBody {
    #[serde(default)]
    username: String,
    #[serde(default)]
    org: Option<String>,
    #[serde(default)]
    role: String,
}

/// An organisation the caller may share with: one they belong to (organisations aren't
/// listed to outsiders, so they can't be probed by name), or any for the server administrator.
fn org_to_share_with(s: &Store, me: &User, org: &str) -> Result<Org, ApiError> {
    let not_found = || ApiError::new(StatusCode::NOT_FOUND, format!("You aren't in an organisation named '{}'.", org.trim()));
    let o = s.find_org(org.trim())?.ok_or_else(not_found)?;
    if !me.is_admin && s.member_role(&o.id, &me.id)?.is_none() {
        return Err(not_found());
    }
    Ok(o)
}

async fn grant(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>, body: Bytes) -> ApiResult {
    app.run_changing(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?.clone();
        let b: GrantBody = parse_body(&body)?;
        let (it, role) = visible(s, &who, &id, false)?;
        need_edit(role)?;
        let r = Role::parse(&b.role).ok_or_else(|| ApiError::bad("role must be view or edit."))?;
        match b.org.as_deref().filter(|o| !o.trim().is_empty()) {
            Some(org) => {
                let o = org_to_share_with(s, &me, org)?;
                s.grant_org(&it, &o, r, &me.id)?;
                s.audit_item(Audit::by(&me, "permission.grant").detail(format!("{}: everyone in {} can {}", it.name, o.name, r.as_str())), &it)?;
            }
            None => {
                let target = s.user_by_name(&b.username)?.filter(|u| !u.disabled).ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("There's no user named '{}'.", b.username.trim())))?;
                s.grant(&it, &target, r, &me.id)?;
                s.audit_item(Audit::by(&me, "permission.grant").target(&target.username).detail(format!("{}: {}", it.name, r.as_str())), &it)?;
            }
        }
        ok_json(permissions_json(s, &it)?)
    })
    .await
}

fn revoke_user(s: &Store, headers: &HeaderMap, id: &str, username: &str) -> ApiResult {
    let who = auth(s, headers)?;
    let me = user_of(&who)?.clone();
    let (it, role) = visible(s, &who, id, false)?;
    let target = s.user_by_name(username)?.ok_or_else(ApiError::not_found)?;
    // Anyone may leave something shared with them; removing others needs edit access.
    if target.id != me.id {
        need_edit(role)?;
    }
    if !s.revoke(&it, &target.id)? {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "That person has no access granted on this item (it may come from a parent folder)."));
    }
    s.audit_item(Audit::by(&me, "permission.revoke").target(&target.username).detail(it.name.clone()), &it)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn revoke_org(s: &Store, headers: &HeaderMap, id: &str, org: &str) -> ApiResult {
    let who = auth(s, headers)?;
    let (it, role) = visible(s, &who, id, false)?;
    need_edit(role)?;
    // Grants to any organisation can be removed (no membership needed, unlike granting). An
    // unknown name answers exactly like a known one without a grant here, so names can't be probed.
    let none = || ApiError::new(StatusCode::NOT_FOUND, "That organisation has no access granted on this item (it may come from a parent folder).");
    let o = s.find_org(org.trim())?.ok_or_else(none)?;
    if !s.revoke_org(&it, &o.id)? {
        return Err(none());
    }
    s.audit_item(audit_by(&who, "permission.revoke").detail(format!("{}: everyone in {}", it.name, o.name)), &it)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn revoke_path(State(app): State<App>, headers: HeaderMap, Path((id, username)): Path<(String, String)>) -> ApiResult {
    app.run_changing(move |s| revoke_user(s, &headers, &id, &username)).await
}

/// `DELETE /api/items/{id}/permissions/org/{org}`
async fn revoke_org_path(State(app): State<App>, headers: HeaderMap, Path((id, org)): Path<(String, String)>) -> ApiResult {
    app.run_changing(move |s| revoke_org(s, &headers, &id, &org)).await
}

async fn revoke_body(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>, body: Bytes) -> ApiResult {
    app.run_changing(move |s| {
        let b: GrantBody = parse_body(&body)?;
        match b.org.as_deref().filter(|o| !o.trim().is_empty()) {
            Some(org) => revoke_org(s, &headers, &id, org),
            None => revoke_user(s, &headers, &id, &b.username),
        }
    })
    .await
}

// ---------------------------------------------------------------------------
// Share links
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct LinkBody {
    #[serde(default)]
    role: String,
    #[serde(default)]
    expires_days: Option<u32>,
    #[serde(default)]
    password: Option<String>,
}

fn link_json(s: &Store, l: &Link) -> Value {
    json!({
        "id": l.id,
        "role": l.role.as_str(),
        "expires": l.expires.map(iso),
        "expired": l.expires.is_some_and(|e| e <= now()),
        "has_password": l.pw_hash.is_some(),
        "created": iso(l.created),
        "created_by": s.display_name(&l.created_by),
    })
}

async fn create_link(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>, body: Bytes) -> ApiResult {
    let base = app.cfg.base_url();
    let b: LinkBody = parse_body(&body)?;
    // The link's password is hashed before the store is locked (only for signed-in callers,
    // so nobody else can make the server hash for them).
    let h = headers.clone();
    app.run(move |s| user_of(&auth(s, &h)?).map(|_| ())).await?;
    let pw_hash = match b.password.clone().filter(|p| !p.is_empty()) {
        Some(p) if p.len() > 1024 => return Err(ApiError::bad("The password is too long.")),
        Some(p) => Some(app.argon(move || util::hash_password(&p)).await?),
        None => None,
    };
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?.clone();
        let (it, role) = visible(s, &who, &id, false)?;
        need_edit(role)?;
        if !s.allow_links {
            return Err(ApiError::coded(StatusCode::FORBIDDEN, "links_disabled", "Share links are switched off on this server."));
        }
        let r = Role::parse(if b.role.is_empty() { "view" } else { &b.role }).ok_or_else(|| ApiError::bad("role must be view or edit."))?;
        let expires = match b.expires_days {
            Some(0) | None => None,
            Some(d) if d <= 3650 => Some(now() + d as i64 * 86400),
            Some(_) => return Err(ApiError::bad("expires_days must be 3650 or less.")),
        };
        let (link, token) = s.create_link(&it, r, expires, pw_hash.as_deref(), &me.id)?;
        let extra = format!("{}{}", if link.pw_hash.is_some() { ", password" } else { "" }, link.expires.map(|e| format!(", expires {}", iso(e))).unwrap_or_default());
        s.audit_item(Audit::by(&me, "link.create").detail(format!("{}: {} link {}{extra}", it.name, r.as_str(), link.id)), &it)?;
        let mut v = link_json(s, &link);
        v["url"] = json!(format!("{base}/l/{token}"));
        Ok((StatusCode::CREATED, axum::Json(v)).into_response())
    })
    .await
}

async fn list_links(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        user_of(&who)?;
        let (it, role) = visible(s, &who, &id, false)?;
        need_edit(role)?;
        let list: Vec<Value> = s.links_of(&it.id)?.iter().map(|l| link_json(s, l)).collect();
        ok_json(json!({ "links": list }))
    })
    .await
}

async fn delete_link(State(app): State<App>, headers: HeaderMap, Path((id, link)): Path<(String, String)>) -> ApiResult {
    app.run_changing(move |s| {
        let who = auth(s, &headers)?;
        user_of(&who)?;
        let (it, role) = visible(s, &who, &id, false)?;
        need_edit(role)?;
        match s.link(&link)? {
            Some(l) if l.item_id == it.id => {
                s.delete_link(&l.id)?;
                s.audit_item(audit_by(&who, "link.delete").detail(format!("{}: link {}", it.name, l.id)), &it)?;
                Ok(StatusCode::NO_CONTENT.into_response())
            }
            _ => Err(ApiError::not_found()),
        }
    })
    .await
}

#[derive(Deserialize, Default)]
struct LinkPassword {
    #[serde(default)]
    password: String,
}

async fn open_link(State(app): State<App>, ConnectInfo(Peer(peer)): ConnectInfo<Peer>, headers: HeaderMap, Path(token): Path<String>) -> ApiResult {
    resolve_link(app, peer, headers, token, String::new()).await
}

async fn open_link_password(State(app): State<App>, ConnectInfo(Peer(peer)): ConnectInfo<Peer>, headers: HeaderMap, Path(token): Path<String>, body: Bytes) -> ApiResult {
    let b: LinkPassword = if body.is_empty() { LinkPassword::default() } else { parse_body(&body)? };
    resolve_link(app, peer, headers, token, b.password).await
}

/// Checks a link (and its password) and hands out a short-lived access token for its item.
async fn resolve_link(app: App, peer: SocketAddr, headers: HeaderMap, token: String, password: String) -> ApiResult {
    let ip_key = format!("ip:{}", client_ip(&app.cfg, peer, &headers));
    if app.limited(std::slice::from_ref(&ip_key)) {
        return Err(too_many());
    }
    let tk = token.clone();
    let link = app.run(move |s| live_link(s, &tk)).await;
    let link = match link {
        Ok(l) => l,
        Err(e) => {
            app.fail(std::slice::from_ref(&ip_key));
            return Err(e);
        }
    };
    let link_key = format!("link:{}", link.id);
    if app.limited(std::slice::from_ref(&link_key)) {
        return Err(too_many());
    }
    if let Some(hash) = link.pw_hash.clone() {
        if password.is_empty() {
            let body = json!({ "error": "This link needs a password.", "password_required": true });
            return Ok((StatusCode::UNAUTHORIZED, axum::Json(body)).into_response());
        }
        let ok = app.argon(move || util::verify_password(&password, &hash)).await?;
        if !ok {
            app.fail(&[ip_key, link_key]);
            let body = json!({ "error": "The password is incorrect.", "password_required": true });
            return Ok((StatusCode::UNAUTHORIZED, axum::Json(body)).into_response());
        }
    }
    let hours = app.cfg.link_access_hours;
    app.run(move |s| {
        let it = s.item(&link.item_id)?.ok_or_else(ApiError::not_found)?;
        let who = Principal::Link { link: link.clone() };
        let role = s.role(&who, &it, false)?.ok_or_else(ApiError::not_found)?;
        let (access, expires) = s.link_access_token(&link, hours)?;
        ok_json(json!({ "item": item_json(s, &it, role), "role": role.as_str(), "access_token": access, "expires": iso(expires) }))
    })
    .await
}

fn live_link(s: &Store, token: &str) -> Result<Link, ApiError> {
    if !s.allow_links {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "This link doesn't exist or was removed."));
    }
    let link = s.link_by_token(token)?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "This link doesn't exist or was removed."))?;
    if link.expires.is_some_and(|e| e <= now()) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "This link has expired."));
    }
    let it = s.item(&link.item_id)?.ok_or_else(ApiError::not_found)?;
    if s.role(&Principal::Link { link: link.clone() }, &it, false)?.is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "This link doesn't exist or was removed."));
    }
    Ok(link)
}

pub(crate) fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

fn page(title: &str, body: &str) -> Response {
    let html = format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><meta name=\"robots\" content=\"noindex\">\
         <title>{t}</title></head><body style=\"font-family:Segoe UI,sans-serif;margin:60px auto;max-width:640px;color:#202020\">\
         <h2 style=\"color:#217346\">{t}</h2>{body}</body></html>",
        t = html_escape(title)
    );
    let mut res = (StatusCode::OK, html).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    h.insert("content-security-policy", HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// What a browser shows for a share link: the file name and a download (with the password
/// form for protected links).
async fn link_page(State(app): State<App>, Path(token): Path<String>) -> Response {
    let tk = token.clone();
    let r = app
        .run(move |s| {
            let l = live_link(s, &tk)?;
            let it = s.item(&l.item_id)?.ok_or_else(ApiError::not_found)?;
            Ok((l, it))
        })
        .await;
    match r {
        Ok((l, it)) => {
            let mut body = format!(
                "<p>'{}' was shared with you ({}).</p>",
                html_escape(&it.name),
                if l.role == Role::Edit { "anyone with the link can edit" } else { "anyone with the link can view" }
            );
            let action = format!("{}/download", html_escape(&token));
            if it.kind != Kind::File {
                body.push_str("<p>Folders can't be downloaded from a link.</p>");
            } else if l.pw_hash.is_some() {
                body.push_str(&format!(
                    "<form method=\"post\" action=\"{action}\"><p>This link is protected by a password.</p>                     <p><input type=\"password\" name=\"password\" autocomplete=\"current-password\" autofocus> <button type=\"submit\">Download</button></p></form>"
                ));
            } else {
                body.push_str(&format!("<p><a href=\"{action}\">Download a copy</a></p>"));
            }
            body.push_str("<p style=\"color:#666\">Open the downloaded workbook with FreedomOffice Sheets or any spreadsheet program.</p>");
            page("FreedomOffice", &body)
        }
        Err(_) => {
            let mut res = page("FreedomOffice", "<p>This link doesn't exist, has expired or was removed.</p>");
            *res.status_mut() = StatusCode::NOT_FOUND;
            res
        }
    }
}

/// A share link's file as a download (always `attachment`; the same content type, sandbox
/// and nosniff rules as `/api/items/{id}/content`, streamed, with Range).
fn file_response(s: &Store, l: &Link, req: &HeaderMap) -> ApiResult {
    let it = s.item(&l.item_id)?.ok_or_else(ApiError::not_found)?;
    if it.kind != Kind::File {
        return Err(ApiError::bad("Folders can't be downloaded."));
    }
    let (file, len, ver) = s.open_content(&it, None)?;
    crate::content::file_response(req, file, len, &it.name, &crate::store::etag(ver.version, &ver.hash), true)
}

/// Direct download for links without a password (`GET /l/<token>/download`).
async fn link_download(State(app): State<App>, headers: HeaderMap, Path(token): Path<String>) -> ApiResult {
    app.run(move |s| {
        let l = live_link(s, &token)?;
        if l.pw_hash.is_some() {
            return Err(ApiError::new(StatusCode::UNAUTHORIZED, "This link needs a password."));
        }
        file_response(s, &l, &headers)
    })
    .await
}

/// `password=...` from an HTML form body.
pub(crate) fn form_value(body: &[u8], key: &str) -> String {
    let text = String::from_utf8_lossy(body);
    for pair in text.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == key {
            let b = v.as_bytes();
            let mut out = Vec::with_capacity(b.len());
            let mut i = 0;
            while i < b.len() {
                match b[i] {
                    b'+' => out.push(b' '),
                    b'%' if i + 2 < b.len() => match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                        Ok(x) => {
                            out.push(x);
                            i += 2;
                        }
                        Err(_) => out.push(b'%'),
                    },
                    c => out.push(c),
                }
                i += 1;
            }
            return String::from_utf8_lossy(&out).into_owned();
        }
    }
    String::new()
}

/// Download through the password form (`POST /l/<token>/download`).
async fn link_download_form(State(app): State<App>, ConnectInfo(Peer(peer)): ConnectInfo<Peer>, headers: HeaderMap, Path(token): Path<String>, body: Bytes) -> ApiResult {
    let password = form_value(&body, "password");
    let ip_key = format!("ip:{}", client_ip(&app.cfg, peer, &headers));
    if app.limited(std::slice::from_ref(&ip_key)) {
        return Err(too_many());
    }
    let tk = token.clone();
    let link = match app.run(move |s| live_link(s, &tk)).await {
        Ok(l) => l,
        Err(e) => {
            app.fail(std::slice::from_ref(&ip_key));
            return Err(e);
        }
    };
    let link_key = format!("link:{}", link.id);
    if app.limited(std::slice::from_ref(&link_key)) {
        return Err(too_many());
    }
    if let Some(hash) = link.pw_hash.clone() {
        let ok = app.argon(move || util::verify_password(&password, &hash)).await?;
        if !ok {
            app.fail(&[ip_key, link_key]);
            let mut res = page("FreedomOffice", "<p>The password is incorrect. Go back and try again.</p>");
            *res.status_mut() = StatusCode::UNAUTHORIZED;
            return Ok(res);
        }
    }
    app.run(move |s| file_response(s, &link, &headers)).await
}

// ---------------------------------------------------------------------------
// Live co-editing
// ---------------------------------------------------------------------------

fn locked() -> ApiError {
    ApiError::new(StatusCode::LOCKED, "This workbook is being edited live. Open it again to join the live session; saving a separate copy would overwrite other people's changes.")
}

/// Someone is connected, or committed live changes aren't in a saved version yet.
fn live_busy(s: &Store, hub: &crate::live::Hub, it: &Item) -> Result<bool, ApiError> {
    if hub.connected(&it.id) {
        return Ok(true);
    }
    Ok(match s.live_state(&it.id)? {
        Some(st) if st.version == it.version => s.live_head(&it.id)? > st.seq,
        _ => false,
    })
}

/// `GET /api/items/{id}/live?since=<seq>[&v=<version>]&cid=<client>[&ticket=<t>]`: the
/// co-editing WebSocket. Editors may send changes; viewers only receive.
async fn live(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    ws: Result<axum::extract::ws::WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> ApiResult {
    // A browser opens WebSockets from any page with its cookies (no CORS for them): the web
    // session cookie never authenticates this route; a browser gets a ticket first (`POST
    // .../live-ticket`, which needs the CSRF header).
    let mut headers = headers;
    headers.remove(crate::web::VERDICT);
    let hub = app.hub.clone();
    let ticket = q.get("ticket").cloned();
    let iid = id.clone();
    let (item, who, role) = app
        .run(move |s| {
            let who = match ticket {
                // A ticket carries who asked for it then: check the account, device token or
                // link again now (disabled, signed out, temporary password since).
                Some(t) if bearer(&headers).is_none() => {
                    let issued = hub.redeem(&t, &iid).ok_or_else(ApiError::unauthorized)?;
                    s.recheck_principal(&issued)?.map_err(|_| ApiError::unauthorized())?
                }
                _ => auth(s, &headers)?,
            };
            let (it, role) = visible(s, &who, &iid, false)?;
            if it.kind != Kind::File {
                return Err(ApiError::bad("Folders can't be edited live."));
            }
            Ok((it, who, role))
        })
        .await?;
    let Ok(ws) = ws else {
        return Err(ApiError::new(StatusCode::UPGRADE_REQUIRED, "Live co-editing needs a WebSocket connection."));
    };
    let num = |k: &str| q.get(k).and_then(|v| v.parse::<i64>().ok());
    // `v`: the version a first join downloaded (a reconnect continues from `since` alone).
    let (Some(since), version) = (num("since"), num("v")) else {
        return Err(ApiError::bad("since is required."));
    };
    let cid: String = q.get("cid").map(|c| c.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(64).collect()).unwrap_or_default();
    if cid.is_empty() || since < 0 {
        return Err(ApiError::bad("cid is required."));
    }
    let join = crate::live::Join { actor: actor(&who), item, version, who, role, since: since as u64, cid };
    let (store, hub) = (app.store.clone(), app.hub.clone());
    Ok(ws.max_message_size(cosync::MAX_MESSAGE_BYTES).max_frame_size(cosync::MAX_MESSAGE_BYTES).on_upgrade(move |socket| crate::live::run(store, hub, socket, join)))
}

/// `GET /api/items/{id}/live/state?version=<v>`: the live state stored with a version.
async fn live_state(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let version: i64 = q.get("version").and_then(|v| v.parse().ok()).ok_or_else(|| ApiError::bad("version is required."))?;
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let (it, _) = visible(s, &who, &id, false)?;
        // Share links give the current version only (as for the content).
        if version != it.version && matches!(who, Principal::Link { .. }) {
            return Err(ApiError::not_found());
        }
        let data = s.live_state_blob(&it.id, version)?.ok_or_else(ApiError::not_found)?;
        let mut res = data.into_response();
        res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
        res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        Ok(res)
    })
    .await
}

/// A single-use ticket (valid one minute) for connecting without an Authorization header.
async fn live_ticket(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    let hub = app.hub.clone();
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let (it, _) = visible(s, &who, &id, false)?;
        let t = hub.ticket(who, &it.id);
        ok_json(json!({ "ticket": t, "expires_in": 60 }))
    })
    .await
}

/// `POST /api/items/{id}/live/snapshot?seq=<n>` with the workbook: a live client's copy that
/// contains every change up to `n`, stored as a new version. `X-Live-Sheets` carries the
/// sheet ids and order keys; with `X-Live-State-Offset: <k>` the first k bytes are the
/// workbook and the rest the client's lossless live state (kept with the version).
async fn live_snapshot(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>, body: Bytes) -> ApiResult {
    let seq: u64 = q.get("seq").and_then(|v| v.parse().ok()).ok_or_else(|| ApiError::bad("seq is required."))?;
    let sheets: String = headers.get("x-live-sheets").and_then(|v| v.to_str().ok()).unwrap_or("").chars().take(64 * 1024).collect();
    let split = match headers.get("x-live-state-offset").and_then(|v| v.to_str().ok()) {
        Some(v) => v.parse::<usize>().ok().filter(|k| *k <= body.len()).ok_or_else(|| ApiError::bad("Invalid X-Live-State-Offset."))?,
        None => body.len(),
    };
    if split == 0 || split > app.cfg.max_upload_bytes() {
        return Err(ApiError::new(if split == 0 { StatusCode::BAD_REQUEST } else { StatusCode::PAYLOAD_TOO_LARGE }, "The snapshot is empty or too large."));
    }
    let iid = id.clone();
    let (saved, st, role) = app
        .run(move |s| {
            let who = auth(s, &headers)?;
            let (it, role) = visible(s, &who, &iid, false)?;
            need_edit(role)?;
            let (saved, st) = s.live_snapshot(&it, &body[..split], &body[split..], &actor(&who), seq, &sheets)?;
            Ok((saved, st, role))
        })
        .await?;
    app.hub.snapshotted(&id, st.seq, st.floor);
    let s = app.store.clone();
    let v = tokio::task::spawn_blocking(move || {
        let s = s.lock().unwrap_or_else(|e| e.into_inner());
        item_json(&s, &saved, role)
    })
    .await
    .map_err(|_| ApiError::internal())?;
    ok_json(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_link_tokens() {
        assert_eq!(redact("/api/links/SECRET"), "/api/links/<redacted>");
        assert_eq!(redact("/l/SECRET/download"), "/l/<redacted>/download");
        assert_eq!(redact("/api/items/abc"), "/api/items/abc");
        assert_eq!(redact("/api/invites/SECRET"), "/api/invites/<redacted>");
        assert_eq!(redact("/i/SECRET"), "/i/<redacted>");
    }

    #[test]
    fn etag_forms() {
        assert_eq!(norm_etag("\"v3-abc\""), "v3-abc");
        assert_eq!(norm_etag("W/\"v3-abc\""), "v3-abc");
        assert_eq!(norm_etag(" v3-abc "), "v3-abc");
    }

    #[test]
    fn limiter_blocks_after_ten() {
        let mut l = Limiter::default();
        for _ in 0..9 {
            l.fail("k");
        }
        assert!(!l.blocked("k"));
        l.fail("k");
        assert!(l.blocked("k"));
        assert!(!l.blocked("other"));
        l.unfail("k");
        assert!(!l.blocked("k"));
    }

    #[test]
    fn limiter_stays_bounded() {
        let mut l = Limiter::default();
        for i in 0..LIMITER_KEYS + 5000 {
            l.fail(&format!("ip:{i}"));
        }
        assert!(l.map.len() <= LIMITER_KEYS);
        for _ in 0..100 {
            l.fail("k");
        }
        assert!(l.map["k"].len() <= LOGIN_FAILURES * 2);
        assert!(l.blocked("k"));
    }
}
