//! The REST API. Every handler authenticates, then checks the caller's role on the item;
//! items the caller can't see answer 404, never 403.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
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
use crate::store::{Grant, Item, Kind, Link, OnConflict, Org, OrgGrant, OrgRole, Principal, Role, Store, StoreError, User};
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
}

impl App {
    pub fn new(mut store: Store, cfg: Config) -> App {
        store.allow_links = cfg.allow_links;
        App { store: Arc::new(Mutex::new(store)), cfg: Arc::new(cfg), limiter: Arc::new(Mutex::new(Limiter::default())), hub: Arc::default() }
    }

    /// After a change that can affect who may see or edit what: re-checks every live session
    /// (they are told about a new role, or closed), before the response goes out.
    async fn changed(&self) {
        let (hub, store) = (self.hub.clone(), self.store.clone());
        let _ = tokio::task::spawn_blocking(move || hub.recheck(&store)).await;
    }

    /// `run` followed by `changed` when the work succeeded.
    async fn run_changing<T: Send + 'static>(&self, f: impl FnOnce(&Store) -> Result<T, ApiError> + Send + 'static) -> Result<T, ApiError> {
        let r = self.run(f).await;
        if r.is_ok() {
            self.changed().await;
        }
        r
    }

    /// Runs blocking store work (SQLite, disk) on the blocking pool, one request at a time.
    async fn run<T: Send + 'static>(&self, f: impl FnOnce(&Store) -> Result<T, ApiError> + Send + 'static) -> Result<T, ApiError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let s = store.lock().unwrap_or_else(|e| e.into_inner());
            f(&s)
        })
        .await
        .map_err(|_| ApiError::internal())?
    }

    fn limited(&self, keys: &[String]) -> bool {
        let mut l = self.limiter.lock().unwrap_or_else(|e| e.into_inner());
        keys.iter().any(|k| l.blocked(k))
    }

    fn fail(&self, keys: &[String]) {
        let mut l = self.limiter.lock().unwrap_or_else(|e| e.into_inner());
        for k in keys {
            l.fail(k);
        }
    }

    fn clear(&self, key: &str) {
        self.limiter.lock().unwrap_or_else(|e| e.into_inner()).map.remove(key);
    }

    fn failures(&self, key: &str) -> usize {
        self.limiter.lock().unwrap_or_else(|e| e.into_inner()).prune(key)
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

/// A username as typed, safe for one log line.
fn loggable(name: &str) -> String {
    name.trim().chars().filter(|c| !c.is_control()).take(64).collect()
}

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
        if self.map.len() > 100_000 {
            self.map.retain(|_, q| q.back().is_some_and(|t| t.elapsed() < LOGIN_WINDOW));
        }
        self.map.entry(key.to_string()).or_default().push_back(Instant::now());
    }
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: Value,
}

impl ApiError {
    fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        ApiError { status, body: json!({ "error": msg.into() }) }
    }

    fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "Not found.")
    }

    fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "Sign in to continue.")
    }

    /// With a machine-readable `code` next to the message.
    fn coded(status: StatusCode, code: &str, msg: impl Into<String>) -> Self {
        ApiError { status, body: json!({ "error": msg.into(), "code": code }) }
    }

    fn forbidden(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, msg)
    }

    fn bad(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, msg)
    }

    fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong on the server.")
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

type ApiResult = Result<Response, ApiError>;

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
        return Err(ApiError::coded(StatusCode::FORBIDDEN, "password_change_required", "Choose a new password to continue (you signed in with a temporary one)."));
    }
    Ok(who)
}

/// The caller, even one who still has to change a temporary password (for `/api/me`,
/// `/api/me/password` and `/api/logout`).
fn auth_any(s: &Store, headers: &HeaderMap) -> Result<Principal, ApiError> {
    let token = bearer(headers).ok_or_else(ApiError::unauthorized)?;
    s.authenticate(&token)?.ok_or_else(ApiError::unauthorized)
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

fn user_json(u: &User) -> Value {
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
fn client_ip(cfg: &Config, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    if cfg.trust_proxy && peer.ip().is_loopback() {
        if let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
        {
            return ip;
        }
    }
    peer.ip()
}

fn too_many() -> ApiError {
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
        .fallback(|| async { ApiError::not_found() })
        .layer(DefaultBodyLimit::max(JSON_LIMIT))
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
    let ip_key = format!("ip:{}", client_ip(&app.cfg, peer, &headers));
    let user_key = format!("user:{}", b.username.trim().to_lowercase());
    let keys = [ip_key, user_key.clone()];
    if app.limited(&keys) {
        return Err(too_many());
    }
    // Repeated failures for a username slow every further attempt down (right or wrong, so the
    // wait says nothing about the password).
    let wait = login_delay(app.failures(&user_key), Duration::from_millis(app.cfg.login_delay_ms));
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
    let name = b.username.clone();
    let found = app.run(move |s| Ok(s.user_by_name(&name)?.map(|u| (s.password_hash(&u.id), u)))).await?;
    let pw = b.password;
    let user = tokio::task::spawn_blocking(move || match found {
        Some((Ok(hash), u)) if util::verify_password(&pw, &hash) && !u.disabled => Some(u),
        Some(_) => None,
        None => {
            util::dummy_verify(&pw);
            None
        }
    })
    .await
    .map_err(|_| ApiError::internal())?;
    let Some(user) = user else {
        app.fail(&keys);
        tracing::warn!("failed sign-in for user '{}' from {}", loggable(&b.username), client_ip(&app.cfg, peer, &headers));
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "The username or password is incorrect."));
    };
    app.clear(&user_key);
    let device = b.device;
    app.run(move |s| {
        let token = s.create_token(&user.id, &device)?;
        ok_json(json!({ "token": token, "user": user_json(&user) }))
    })
    .await
}

async fn logout(State(app): State<App>, headers: HeaderMap) -> ApiResult {
    app.run_changing(move |s| {
        let who = auth_any(s, &headers)?;
        if let Principal::User { token_id, .. } = who {
            s.revoke_token(token_id)?;
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
    let (user, token_id, hash) = app
        .run(move |s| {
            let who = auth_any(s, &headers)?;
            let Principal::User { user, token_id } = who else { return Err(ApiError::unauthorized()) };
            let hash = s.password_hash(&user.id)?;
            Ok((user, token_id, hash))
        })
        .await?;
    let key = format!("user:{}", user.username.to_lowercase());
    if app.limited(std::slice::from_ref(&key)) {
        return Err(too_many());
    }
    let old = b.old_password;
    let ok = tokio::task::spawn_blocking(move || util::verify_password(&old, &hash)).await.map_err(|_| ApiError::internal())?;
    if !ok {
        app.fail(std::slice::from_ref(&key));
        tracing::warn!("wrong current password in a password change for user '{}'", user.username);
        return Err(ApiError::coded(StatusCode::FORBIDDEN, "wrong_password", "The current password is incorrect."));
    }
    let new = b.new_password;
    app.run_changing(move |s| {
        if util::verify_password(&new, &s.password_hash(&user.id)?) {
            return Err(ApiError::bad("The new password must be different from the current one."));
        }
        s.set_password(&user.id, &new)?;
        let n = s.revoke_other_tokens(&user.id, token_id)?;
        tracing::info!("user '{}' changed their password ({n} other device(s) signed out)", user.username);
        let fresh = s.user_by_id(&user.id)?.ok_or_else(ApiError::not_found)?;
        ok_json(json!({ "user": user_json(&fresh), "signed_out_devices": n }))
    })
    .await
}

// ---------------------------------------------------------------------------
// Accounts and organisations (administration)
// ---------------------------------------------------------------------------

/// The server administrator (manages accounts and organisations; sees no documents).
fn need_admin(me: &User) -> Result<(), ApiError> {
    if me.is_admin { Ok(()) } else { Err(ApiError::forbidden("Only the server administrator can do that.")) }
}

/// The organisation (by id or name) and the caller's role in it; 404 unless the caller is a
/// member or the server administrator.
fn org_for(s: &Store, me: &User, org: &str) -> Result<(Org, Option<OrgRole>), ApiError> {
    let o = s.find_org(org)?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "There's no such organisation."))?;
    let mine = s.member_role(&o.id, &me.id)?;
    if mine.is_none() && !me.is_admin {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "There's no such organisation."));
    }
    Ok((o, mine))
}

/// Org owners and admins (and the server administrator) manage an organisation.
fn need_manage(me: &User, mine: Option<OrgRole>) -> Result<(), ApiError> {
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

fn person_by_name(s: &Store, username: &str) -> Result<User, ApiError> {
    s.user_by_name(username)?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("There's no user named '{}'.", username.trim())))
}

#[derive(Deserialize)]
struct AccountBody {
    username: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    email: String,
    /// Join this organisation (required unless the caller is the server administrator).
    #[serde(default)]
    org: Option<String>,
    #[serde(default)]
    role: Option<String>,
}

/// `POST /api/users`: a new account with a temporary password generated by the server,
/// returned once in this response (never logged or stored in clear); the person must choose
/// a new one when they first sign in. Org owners/admins create accounts into their org.
async fn create_account(State(app): State<App>, headers: HeaderMap, body: Bytes) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?.clone();
        let b: AccountBody = parse_body(&body)?;
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
        let pw = util::temp_password();
        let u = s.create_account(&b.username, &b.display_name, &b.email, &pw, false, true)?;
        if let Some((org, role)) = &org {
            s.set_member(org, &u, *role, &me.id)?;
        }
        tracing::info!("user '{}' created the account '{}'{}", me.username, u.username, org.as_ref().map(|(o, _)| format!(" in {}", o.name)).unwrap_or_default());
        Ok((StatusCode::CREATED, axum::Json(json!({ "user": user_json(&u), "temporary_password": pw }))).into_response())
    })
    .await
}

/// `POST /api/users/{username}/reset-password` (server administrator): a new temporary
/// password (returned once), all devices signed out.
async fn reset_password(State(app): State<App>, headers: HeaderMap, Path(username): Path<String>) -> ApiResult {
    app.run_changing(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        need_admin(me)?;
        let u = person_by_name(s, &username)?;
        let pw = s.set_temp_password(&u.id)?;
        tracing::info!("user '{}' reset the password of '{}'", me.username, u.username);
        ok_json(json!({ "user": user_json(&s.user_by_id(&u.id)?.ok_or_else(ApiError::not_found)?), "temporary_password": pw }))
    })
    .await
}

async fn set_disabled(app: App, headers: HeaderMap, username: String, disabled: bool) -> ApiResult {
    app.run_changing(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        need_admin(me)?;
        let u = person_by_name(s, &username)?;
        if u.id == me.id && disabled {
            return Err(ApiError::bad("You can't disable your own account."));
        }
        s.set_disabled(&u.id, disabled)?;
        tracing::info!("user '{}' {} the account '{}'", me.username, if disabled { "disabled" } else { "enabled" }, u.username);
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

/// `GET /api/orgs`: the caller's organisations (the server administrator: all of them).
async fn list_orgs(State(app): State<App>, headers: HeaderMap) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let mine = s.orgs_of(&me.id)?;
        let orgs: Vec<(Org, Option<OrgRole>)> = if me.is_admin {
            s.orgs()?.into_iter().map(|o| {
                let r = mine.iter().find(|(m, _)| m.id == o.id).map(|(_, r)| *r);
                (o, r)
            }).collect()
        } else {
            mine.into_iter().map(|(o, r)| (o, Some(r))).collect()
        };
        let mut list = Vec::new();
        for (o, r) in &orgs {
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

async fn create_org(State(app): State<App>, headers: HeaderMap, body: Bytes) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        need_admin(me)?;
        let b: OrgBody = parse_body(&body)?;
        let o = s.create_org(&b.name, &me.id)?;
        tracing::info!("user '{}' created the organisation '{}'", me.username, o.name);
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
        v["member_list"] = members_json(s, &o, need_manage(me, mine).is_ok())?;
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

/// `PATCH /api/orgs/{org} {name?, member_role?}`: renaming takes the server administrator;
/// the members' default role ("edit"/"view") takes an org owner/admin.
async fn patch_org(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>, body: Bytes) -> ApiResult {
    app.run_changing(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let b: PatchOrgBody = parse_body(&body)?;
        let (mut o, mine) = org_for(s, me, &org)?;
        if let Some(r) = &b.member_role {
            need_manage(me, mine)?;
            let r = Role::parse(r).ok_or_else(|| ApiError::bad("member_role must be view or edit."))?;
            s.set_org_member_role(&o, r)?;
        }
        if let Some(n) = &b.name {
            need_admin(me)?;
            o = s.rename_org(&o, n)?;
        }
        let o = s.org(&o.id)?.ok_or_else(ApiError::not_found)?;
        ok_json(org_json(s, &o, mine)?)
    })
    .await
}

/// `DELETE /api/orgs/{org}` (server administrator): only when its folder is empty.
async fn delete_org(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>) -> ApiResult {
    app.run_changing(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        need_admin(me)?;
        let (o, _) = org_for(s, me, &org)?;
        s.delete_org(&o)?;
        tracing::info!("user '{}' deleted the organisation '{}'", me.username, o.name);
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

async fn org_members(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let (o, mine) = org_for(s, me, &org)?;
        ok_json(json!({ "members": members_json(s, &o, need_manage(me, mine).is_ok())? }))
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

fn set_member(s: &Store, headers: &HeaderMap, org: &str, username: &str, role: Option<&str>) -> ApiResult {
    let who = auth(s, headers)?;
    let me = user_of(&who)?;
    let (o, mine) = org_for(s, me, org)?;
    need_manage(me, mine)?;
    let role = OrgRole::parse(role.unwrap_or("member")).ok_or_else(|| ApiError::bad("role must be member, admin or owner."))?;
    let target = person_by_name(s, username)?;
    if target.disabled {
        return Err(ApiError::bad(format!("The user '{}' is disabled.", target.username)));
    }
    let before = s.member_role(&o.id, &target.id)?;
    need_owner_for(me, mine, &[Some(role), before])?;
    let added = s.set_member(&o, &target, role, &me.id)?;
    tracing::info!("user '{}' {} '{}' as {} of '{}'", me.username, if added { "added" } else { "set" }, target.username, role.as_str(), o.name);
    let status = if added { StatusCode::CREATED } else { StatusCode::OK };
    Ok((status, axum::Json(json!({ "members": members_json(s, &o, true)? }))).into_response())
}

/// `POST /api/orgs/{org}/members {username, role?}`: adds a person (default role member) or
/// changes their role.
async fn add_member(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>, body: Bytes) -> ApiResult {
    app.run_changing(move |s| {
        let b: MemberBody = parse_body(&body)?;
        set_member(s, &headers, &org, &b.username, b.role.as_deref())
    })
    .await
}

/// `PATCH /api/orgs/{org}/members/{username} {role}`
async fn patch_member(State(app): State<App>, headers: HeaderMap, Path((org, username)): Path<(String, String)>, body: Bytes) -> ApiResult {
    app.run_changing(move |s| {
        let b: MemberBody = parse_body(&body)?;
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let (o, _) = org_for(s, me, &org)?;
        let target = person_by_name(s, &username)?;
        if s.member_role(&o.id, &target.id)?.is_none() {
            return Err(ApiError::new(StatusCode::NOT_FOUND, format!("{} isn't a member of {}.", target.username, o.name)));
        }
        set_member(s, &headers, &org, &username, Some(b.role.as_deref().ok_or_else(|| ApiError::bad("role is required."))?))
    })
    .await
}

/// `DELETE /api/orgs/{org}/members/{username}`: anyone may leave; removing others takes an
/// org owner/admin. Their access to the org folder ends at once (live sessions included).
async fn remove_member(State(app): State<App>, headers: HeaderMap, Path((org, username)): Path<(String, String)>) -> ApiResult {
    app.run_changing(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let (o, mine) = org_for(s, me, &org)?;
        let target = person_by_name(s, &username)?;
        let theirs = s.member_role(&o.id, &target.id)?;
        if target.id != me.id {
            need_manage(me, mine)?;
            need_owner_for(me, mine, &[theirs])?;
        }
        if theirs.is_none() || !s.remove_member(&o.id, &target.id)? {
            return Err(ApiError::new(StatusCode::NOT_FOUND, format!("{} isn't a member of {}.", target.username, o.name)));
        }
        tracing::info!("user '{}' removed '{}' from '{}'", me.username, target.username, o.name);
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

/// `POST /api/orgs/{org}/invites {email, role?}`: a one-time link (valid 7 days), returned
/// once in `url` for the admin to send. Only its hash is stored.
async fn create_invite(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>, body: Bytes) -> ApiResult {
    let base = app.cfg.base_url();
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let b: InviteBody = parse_body(&body)?;
        let (o, mine) = org_for(s, me, &org)?;
        need_manage(me, mine)?;
        let role = OrgRole::parse(b.role.as_deref().unwrap_or("member")).ok_or_else(|| ApiError::bad("role must be member, admin or owner."))?;
        need_owner_for(me, mine, &[Some(role)])?;
        let (inv, token) = s.create_invite(&o, &b.email, role, &me.id)?;
        tracing::info!("user '{}' created an invite to '{}'", me.username, o.name);
        let mut v = invite_json(s, &inv);
        v["url"] = json!(format!("{base}/i/{token}"));
        Ok((StatusCode::CREATED, axum::Json(v)).into_response())
    })
    .await
}

async fn list_invites(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let (o, mine) = org_for(s, me, &org)?;
        need_manage(me, mine)?;
        let list: Vec<Value> = s.invites_of(&o.id)?.iter().map(|i| invite_json(s, i)).collect();
        ok_json(json!({ "invites": list }))
    })
    .await
}

async fn delete_invite(State(app): State<App>, headers: HeaderMap, Path((org, id)): Path<(String, String)>) -> ApiResult {
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?;
        let (o, mine) = org_for(s, me, &org)?;
        need_manage(me, mine)?;
        match s.invite(&id)? {
            Some(i) if i.org_id == o.id => {
                s.delete_invite(&i.id)?;
                Ok(StatusCode::NO_CONTENT.into_response())
            }
            _ => Err(ApiError::not_found()),
        }
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
// Invites (no account yet)
// ---------------------------------------------------------------------------

/// The invite behind a token, or 404 (counted as a failure for the address).
async fn live_invite(app: &App, ip_key: &str, token: String) -> Result<(crate::store::Invite, Org), ApiError> {
    if app.limited(&[ip_key.to_string()]) {
        return Err(too_many());
    }
    let r = app
        .run(move |s| {
            let inv = s.invite_by_token(&token)?.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "This invite doesn't exist, was used already or has expired."))?;
            let org = s.org(&inv.org_id)?.ok_or_else(ApiError::not_found)?;
            Ok((inv, org))
        })
        .await;
    if r.is_err() {
        app.fail(&[ip_key.to_string()]);
    }
    r
}

/// `GET /api/invites/{token}`: what the invite is for.
async fn invite_info(State(app): State<App>, ConnectInfo(Peer(peer)): ConnectInfo<Peer>, headers: HeaderMap, Path(token): Path<String>) -> ApiResult {
    let ip_key = format!("ip:{}", client_ip(&app.cfg, peer, &headers));
    let (inv, org) = live_invite(&app, &ip_key, token).await?;
    ok_json(json!({ "org": { "id": org.id, "name": org.name }, "email": inv.email, "role": inv.role.as_str(), "expires": iso(inv.expires) }))
}

#[derive(Deserialize)]
struct AcceptBody {
    username: String,
    password: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    device: String,
}

/// Creates the account and membership for an invite; the password is hashed off the async runtime.
async fn accept(app: &App, ip_key: &str, token: String, b: AcceptBody) -> Result<(User, Org), ApiError> {
    live_invite(app, ip_key, token.clone()).await?;
    app.run_changing(move |s| Ok(s.accept_invite(&token, &b.username, &b.display_name, &b.password)?)).await
}

/// `POST /api/invites/{token} {username, password, display_name?, device?}`: creates the
/// account (with the password the person chose) and joins the organisation; signs in.
async fn accept_invite(State(app): State<App>, ConnectInfo(Peer(peer)): ConnectInfo<Peer>, headers: HeaderMap, Path(token): Path<String>, body: Bytes) -> ApiResult {
    let ip_key = format!("ip:{}", client_ip(&app.cfg, peer, &headers));
    let b: AcceptBody = parse_body(&body)?;
    let device = b.device.clone();
    let (user, org) = accept(&app, &ip_key, token, b).await?;
    tracing::info!("'{}' joined '{}' through an invite", user.username, org.name);
    app.run(move |s| {
        let token = s.create_token(&user.id, &device)?;
        Ok((StatusCode::CREATED, axum::Json(json!({ "token": token, "user": user_json(&user), "org": { "id": org.id, "name": org.name } }))).into_response())
    })
    .await
}

fn invite_form_html(token: &str, org: &str, error: &str) -> String {
    let err = if error.is_empty() { String::new() } else { format!("<p style=\"color:#b00020\">{}</p>", html_escape(error)) };
    format!(
        "<p>You were invited to join <b>{}</b> on this FreedomOffice server. Choose a username and a password \
         (at least {} characters) to create your account.</p>{err}\
         <form method=\"post\" action=\"{}\">\
         <p><label>Username<br><input name=\"username\" autocomplete=\"username\" required maxlength=\"64\" autofocus></label></p>\
         <p><label>Your name<br><input name=\"display_name\" autocomplete=\"name\" maxlength=\"100\"></label></p>\
         <p><label>Password<br><input type=\"password\" name=\"password\" autocomplete=\"new-password\" required minlength=\"{}\"></label></p>\
         <p><button type=\"submit\">Create my account</button></p></form>",
        html_escape(org),
        util::MIN_PASSWORD,
        html_escape(token),
        util::MIN_PASSWORD
    )
}

/// `GET /i/{token}`: the invite page a browser shows (a form to choose username and password).
async fn invite_page(State(app): State<App>, ConnectInfo(Peer(peer)): ConnectInfo<Peer>, headers: HeaderMap, Path(token): Path<String>) -> Response {
    let ip_key = format!("ip:{}", client_ip(&app.cfg, peer, &headers));
    match live_invite(&app, &ip_key, token.clone()).await {
        Ok((_, org)) => page("Join FreedomOffice", &invite_form_html(&token, &org.name, "")),
        Err(e) => {
            let mut res = page("FreedomOffice", "<p>This invite doesn't exist, was used already or has expired. Ask for a new one.</p>");
            *res.status_mut() = e.status;
            res
        }
    }
}

/// `POST /i/{token}` from the invite page.
async fn invite_form(State(app): State<App>, ConnectInfo(Peer(peer)): ConnectInfo<Peer>, headers: HeaderMap, Path(token): Path<String>, body: Bytes) -> Response {
    let ip_key = format!("ip:{}", client_ip(&app.cfg, peer, &headers));
    let b = AcceptBody { username: form_value(&body, "username"), password: form_value(&body, "password"), display_name: form_value(&body, "display_name"), device: String::new() };
    match accept(&app, &ip_key, token.clone(), b).await {
        Ok((user, org)) => {
            tracing::info!("'{}' joined '{}' through an invite", user.username, org.name);
            page(
                "Welcome to FreedomOffice",
                &format!(
                    "<p>Your account <b>{}</b> was created and you are now in {}.</p><p>Sign in from FreedomOffice Sheets (File &gt; Account) with your username and password.</p>",
                    html_escape(&user.username),
                    html_escape(&org.name)
                ),
            )
        }
        Err(e) => {
            let msg = e.body["error"].as_str().unwrap_or("Something went wrong.").to_string();
            let org = app.run(move |s| Ok(s.invite_by_token(&token)?.and_then(|i| s.org(&i.org_id).ok().flatten()).map(|o| (o.name, token)))).await.ok().flatten();
            let mut res = match org {
                Some((name, token)) => page("Join FreedomOffice", &invite_form_html(&token, &name, &msg)),
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
        let list: Vec<Value> = s.search_users(q.get("query").map(String::as_str).unwrap_or(""), 21)?.iter().filter(|u| u.id != me.id).take(20).map(person).collect();
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

async fn upload_file(State(app): State<App>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>, body: Bytes) -> ApiResult {
    let hub = app.hub.clone();
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let on_conflict = OnConflict::parse(q.get("conflict").map(String::as_str).unwrap_or("")).ok_or_else(|| ApiError::bad("conflict must be fail, rename or replace."))?;
        let name = q.get("name").cloned().unwrap_or_default();
        let parent = match q.get("parent").filter(|p| !p.is_empty() && p.as_str() != "root") {
            Some(p) => p.clone(),
            None => user_of(&who)?.root_id.clone(),
        };
        let (p, role) = visible(s, &who, &parent, false)?;
        need_edit(role)?;
        if on_conflict == OnConflict::Replace {
            if let Ok(clean) = util::clean_name(&name) {
                if let Some(existing) = s.find_child(&p.id, &clean)? {
                    if live_busy(s, &hub, &existing)? {
                        return Err(locked());
                    }
                }
            }
        }
        let (it, created) = s.create_file(&p.id, &name, &body, &actor(&who), on_conflict)?;
        if let Principal::User { user, .. } = &who {
            s.touch_recent(&user.id, &it.id)?;
        }
        let role = s.role(&who, &it, false)?.unwrap_or(role);
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
        let (data, ver) = s.read_content(&it, version)?;
        if let (Principal::User { user, .. }, None) = (&who, version) {
            s.touch_recent(&user.id, &it.id)?;
        }
        // Where a live session starts from this version (for joining it).
        let live = if version.is_none() { Some((s.live_base(&it)?, s.live_state_blob(&it.id, ver.version)?.is_some())) } else { None };
        let mut res = data.into_response();
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
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
        h.insert(header::ETAG, quoted(&crate::store::etag(ver.version, &ver.hash)));
        h.insert("x-version", HeaderValue::from(ver.version));
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        Ok(res)
    })
    .await
}

/// `"v3-abc"`, `W/"v3-abc"` and `v3-abc` all mean `v3-abc`.
fn norm_etag(s: &str) -> String {
    s.trim().trim_start_matches("W/").trim_matches('"').to_string()
}

async fn put_content(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>, body: Bytes) -> ApiResult {
    let hub = app.hub.clone();
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let (it, role) = visible(s, &who, &id, false)?;
        if it.kind != Kind::File {
            return Err(ApiError::bad("Folders have no content."));
        }
        need_edit(role)?;
        if live_busy(s, &hub, &it)? {
            return Err(locked());
        }
        if let Some(want) = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok()).map(norm_etag) {
            if want != "*" && want != it.etag() {
                let body = json!({
                    "error": "Someone else saved changes to this file.",
                    "etag": it.etag(),
                    "version": it.version,
                    "modified_by": display_of(s, &it.modified_by),
                    "modified": iso(it.modified),
                });
                return Ok((StatusCode::PRECONDITION_FAILED, axum::Json(body)).into_response());
            }
        }
        let saved = s.write_content(&it, &body, &actor(&who))?;
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
            }
            None => {
                let target = s.user_by_name(&b.username)?.filter(|u| !u.disabled).ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("There's no user named '{}'.", b.username.trim())))?;
                s.grant(&it, &target, r, &me.id)?;
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
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn revoke_org(s: &Store, headers: &HeaderMap, id: &str, org: &str) -> ApiResult {
    let who = auth(s, headers)?;
    let (it, role) = visible(s, &who, id, false)?;
    need_edit(role)?;
    // Grants to any organisation can be removed (no membership needed, unlike granting).
    let o = s.find_org(org.trim())?.ok_or_else(ApiError::not_found)?;
    if !s.revoke_org(&it, &o.id)? {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "That organisation has no access granted on this item (it may come from a parent folder)."));
    }
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
    app.run(move |s| {
        let who = auth(s, &headers)?;
        let me = user_of(&who)?.clone();
        let b: LinkBody = parse_body(&body)?;
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
        let (link, token) = s.create_link(&it, r, expires, b.password.as_deref(), &me.id)?;
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
        let ok = tokio::task::spawn_blocking(move || util::verify_password(&password, &hash)).await.map_err(|_| ApiError::internal())?;
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

fn html_escape(s: &str) -> String {
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
    h.insert("content-security-policy", HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'"));
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

fn file_response(s: &Store, l: &Link) -> ApiResult {
    let it = s.item(&l.item_id)?.ok_or_else(ApiError::not_found)?;
    if it.kind != Kind::File {
        return Err(ApiError::bad("Folders can't be downloaded."));
    }
    let (data, ver) = s.read_content(&it, None)?;
    let mut res = data.into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"));
    h.insert(header::ETAG, quoted(&crate::store::etag(ver.version, &ver.hash)));
    let encoded: String = it.name.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename*=UTF-8''{encoded}")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

/// Direct download for links without a password (`GET /l/<token>/download`).
async fn link_download(State(app): State<App>, Path(token): Path<String>) -> ApiResult {
    app.run(move |s| {
        let l = live_link(s, &token)?;
        if l.pw_hash.is_some() {
            return Err(ApiError::new(StatusCode::UNAUTHORIZED, "This link needs a password."));
        }
        file_response(s, &l)
    })
    .await
}

/// `password=...` from an HTML form body.
fn form_value(body: &[u8], key: &str) -> String {
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
        let ok = tokio::task::spawn_blocking(move || util::verify_password(&password, &hash)).await.map_err(|_| ApiError::internal())?;
        if !ok {
            app.fail(&[ip_key, link_key]);
            let mut res = page("FreedomOffice", "<p>The password is incorrect. Go back and try again.</p>");
            *res.status_mut() = StatusCode::UNAUTHORIZED;
            return Ok(res);
        }
    }
    app.run(move |s| file_response(s, &link)).await
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
    let hub = app.hub.clone();
    let ticket = q.get("ticket").cloned();
    let iid = id.clone();
    let (item, who, role) = app
        .run(move |s| {
            let who = match ticket {
                Some(t) if bearer(&headers).is_none() => hub.redeem(&t, &iid).ok_or_else(ApiError::unauthorized)?,
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
    }
}
