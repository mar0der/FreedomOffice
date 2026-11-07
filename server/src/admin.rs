//! The web admin page (`/admin`): plain server-rendered HTML forms, no scripts.
//!
//! Sign-in gives a session cookie (`__Host-fo_admin`, see `cookie_name`) that is separate from
//! the API's device tokens: only its SHA-256 is stored (`admin_sessions`), it lasts 12 hours
//! from sign-in, and it never works as a bearer token. Sign-in goes through the API's limiter
//! and Argon2 slots (`api::check_sign_in`).
//!
//! Every POST form carries a CSRF token derived from the session secret (SHA-256 of a label
//! and the secret, so nothing more is stored), checked in constant time; POSTs whose Origin
//! (or, without one, Referer) isn't the server's own are refused before anything else.
//!
//! What a person may see and change is decided by the same functions the API uses
//! (`api::*_as`, `api::org_for`, `api::need_manage`, `api::need_admin`), and every change
//! re-checks the open live sessions (`App::changed`).

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;

use crate::api::{self, html_escape as esc, ApiError, App, Caller, Peer, SignIn};
use crate::config::Config;
use crate::store::{OrgRole, Role, Store, User, ADMIN_SESSION_SECS, INVITE_DAYS};
use crate::util;

pub fn routes() -> Router<App> {
    Router::new()
        .route("/admin", get(overview))
        .route("/admin/style.css", get(stylesheet))
        .route("/admin/login", get(login_page).post(login))
        .route("/admin/logout", post(logout))
        .route("/admin/password", get(password_page).post(change_password))
        .route("/admin/orgs", get(orgs_page).post(create_org))
        .route("/admin/orgs/{org}", get(org_page))
        .route("/admin/orgs/{org}/settings", post(org_settings))
        .route("/admin/orgs/{org}/rename", post(rename_org))
        .route("/admin/orgs/{org}/delete", post(delete_org))
        .route("/admin/orgs/{org}/members", post(add_member))
        .route("/admin/orgs/{org}/members/{username}/role", post(member_role))
        .route("/admin/orgs/{org}/members/{username}/remove", post(remove_member))
        .route("/admin/orgs/{org}/accounts", post(create_org_account))
        .route("/admin/orgs/{org}/invites", post(create_invite))
        .route("/admin/orgs/{org}/invites/{id}/revoke", post(revoke_invite))
        .route("/admin/people", get(people_page).post(create_account))
        .route("/admin/people/{username}", get(person_page))
        .route("/admin/people/{username}/disable", post(disable))
        .route("/admin/people/{username}/enable", post(enable))
        .route("/admin/people/{username}/reset-password", post(reset_password))
        .route("/admin/people/{username}/sign-out", post(sign_out_everywhere))
        .route("/admin/people/{username}/admin", post(set_super_admin))
        .route("/admin/people/{username}/sessions/{kind}/{id}/revoke", post(revoke_sign_in))
}

// ---------------------------------------------------------------------------
// Session cookie, CSRF and origin checks
// ---------------------------------------------------------------------------

/// The server's public address is https: the cookie is `__Host-fo_admin` with `Secure`.
/// Only when the server is reached over plain http (allowed only on loopback or with
/// `allow_insecure_http`, see `Config::check_plain_http`) is it `fo_admin` without `Secure`,
/// because browsers wouldn't send a Secure cookie back over http.
fn secure(cfg: &Config) -> bool {
    !cfg.base_url().starts_with("http://") || !matches!(cfg.tls(), Ok(crate::config::Tls::None))
}

pub fn cookie_name(cfg: &Config) -> &'static str {
    if secure(cfg) { "__Host-fo_admin" } else { "fo_admin" }
}

fn set_cookie(cfg: &Config, value: &str, max_age: i64) -> HeaderValue {
    let flags = if secure(cfg) { "; Secure" } else { "" };
    HeaderValue::from_str(&format!("{}={value}; Path=/; Max-Age={max_age}; HttpOnly; SameSite=Strict{flags}", cookie_name(cfg))).expect("cookie header")
}

/// The session secret from the request's cookies.
fn session_secret(cfg: &Config, headers: &HeaderMap) -> Option<String> {
    let name = cookie_name(cfg);
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty() && v.len() <= 200)
}

/// The CSRF token of a session: derived from its secret, so it needs no storage and can't be
/// worked out without the (HttpOnly) cookie.
fn csrf_for(secret: &str) -> String {
    util::base64url(&util::token_hash(&format!("freedomoffice admin csrf\0{secret}")))
}

fn csrf_ok(secret: &str, body: &[u8]) -> bool {
    let sent = api::form_value(body, "csrf");
    util::ct_eq(sent.as_bytes(), csrf_for(secret).as_bytes())
}

/// "https://host:port" of a URL (lowercase).
fn origin_of(url: &str) -> String {
    let url = url.trim().to_ascii_lowercase();
    match url.split_once("://") {
        Some((scheme, rest)) => format!("{scheme}://{}", rest.split(['/', '?', '#']).next().unwrap_or("")),
        None => url,
    }
}

/// A POST from one of this server's pages: Origin, when sent, must be the public URL's; with
/// no Origin, a Referer must be on it. (Admin pages use `Referrer-Policy: same-origin`, so
/// browsers send the real Origin with their forms; `null` is refused.) With neither header,
/// the CSRF token and SameSite=Strict still protect.
fn same_origin(cfg: &Config, headers: &HeaderMap) -> bool {
    let ours = origin_of(&cfg.base_url());
    if let Some(o) = headers.get(header::ORIGIN) {
        return o.to_str().is_ok_and(|o| origin_of(o) == ours && !o.trim().eq_ignore_ascii_case("null"));
    }
    if let Some(r) = headers.get(header::REFERER) {
        return r.to_str().is_ok_and(|r| origin_of(r) == ours);
    }
    true
}

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

const CSP: &str = "default-src 'none'; style-src 'self'; img-src 'self' data:; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

fn harden(res: &mut Response) {
    let h = res.headers_mut();
    h.insert("content-security-policy", HeaderValue::from_static(CSP));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    // same-origin, not no-referrer: with no-referrer browsers send `Origin: null` on form
    // POSTs, which would defeat the origin check. Nothing is sent to other sites either way.
    h.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

/// Middleware for the whole router: every response under `/admin` gets the admin headers,
/// also errors the handlers never see (a rejected path parameter, 404, 405, body too large).
pub async fn harden_admin(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let path = req.uri().path();
    let admin = path == "/admin" || path.starts_with("/admin/");
    let mut res = next.run(req).await;
    if admin {
        harden(&mut res);
    }
    res
}

fn redirect(to: &str) -> Response {
    let mut res = StatusCode::SEE_OTHER.into_response();
    if let Ok(v) = HeaderValue::from_str(to) {
        res.headers_mut().insert(header::LOCATION, v);
    }
    harden(&mut res);
    res
}

/// The signed-in person on a page: who they are, their session and its CSRF token.
struct Ctx {
    me: User,
    session: i64,
    csrf: String,
}

impl Ctx {
    /// A POST form with the CSRF token. `inner` is trusted HTML (callers escape their values).
    fn form(&self, action: &str, class: &str, inner: &str) -> String {
        format!(
            "<form method=\"post\" action=\"{}\" class=\"{class}\"><input type=\"hidden\" name=\"csrf\" value=\"{}\">{inner}</form>",
            esc(action),
            esc(&self.csrf)
        )
    }

    /// A form that is just one button.
    fn button(&self, action: &str, label: &str, class: &str) -> String {
        self.form(action, "inline", &format!("<button type=\"submit\" class=\"{class}\">{}</button>", esc(label)))
    }

    fn manages_anything(&self, s: &Store) -> Result<bool, ApiError> {
        Ok(self.me.is_admin || s.orgs_of(&self.me.id)?.iter().any(|(_, r)| r.manages()))
    }
}

fn html(status: StatusCode, title: &str, ctx: Option<&Ctx>, nav: &[(&str, &str)], body: &str) -> Response {
    let top = match ctx {
        Some(c) => {
            let links: String = nav.iter().map(|(href, label)| format!("<a href=\"{href}\">{label}</a>")).collect();
            format!(
                "<nav>{links}</nav><div class=\"who\"><span>{}</span>{}</div>",
                esc(&c.me.display_name),
                c.button("/admin/logout", "Sign out", "link")
            )
        }
        None => String::new(),
    };
    let doc = format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex\"><title>{t} - FreedomOffice admin</title><link rel=\"stylesheet\" href=\"/admin/style.css\"></head>\
         <body><header class=\"top\"><a class=\"brand\" href=\"/admin\">FreedomOffice <span>admin</span></a>{top}</header>\
         <main><h1>{t}</h1>{body}</main></body></html>",
        t = esc(title)
    );
    let mut res = (status, doc).into_response();
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    harden(&mut res);
    res
}

/// The navigation for a signed-in person.
fn nav_for(me: &User, manages: bool) -> Vec<(&'static str, &'static str)> {
    let mut v = vec![("/admin", "Overview")];
    if manages {
        v.push(("/admin/orgs", "Organisations"));
    }
    if me.is_admin {
        v.push(("/admin/people", "People"));
    }
    v.push(("/admin/password", "Password"));
    v
}

/// A finished page for a signed-in person.
struct Page {
    status: StatusCode,
    title: String,
    body: String,
}

impl Page {
    fn ok(title: impl Into<String>, body: String) -> Page {
        Page { status: StatusCode::OK, title: title.into(), body }
    }
}

fn render(s: &Store, ctx: &Ctx, p: Page) -> Response {
    let manages = ctx.manages_anything(s).unwrap_or(false);
    html(p.status, &p.title, Some(ctx), &nav_for(&ctx.me, manages), &p.body)
}

fn error_body(e: &ApiError, back: &str) -> String {
    format!("<p class=\"msg error\">{}</p><p><a href=\"{}\">Back</a></p>", esc(e.message()), esc(back))
}

fn error_title(status: StatusCode) -> &'static str {
    match status {
        StatusCode::FORBIDDEN => "Not allowed",
        StatusCode::NOT_FOUND => "Not found",
        StatusCode::CONFLICT => "That can't be done now",
        StatusCode::TOO_MANY_REQUESTS => "Too many attempts",
        _ => "That didn't work",
    }
}

/// A failure before or while authenticating: to the sign-in page, or to the password change
/// a temporary password requires.
fn auth_redirect(e: &ApiError) -> Option<Response> {
    if e.status() == StatusCode::UNAUTHORIZED {
        return Some(redirect("/admin/login"));
    }
    if e.code() == Some("password_change_required") {
        return Some(redirect("/admin/password"));
    }
    None
}

fn plain_error(e: &ApiError, back: &str) -> Response {
    html(e.status(), error_title(e.status()), None, &[], &error_body(e, back))
}

/// The signed-in context for `secret`; with `allow_temp` also for someone who still has to
/// change a temporary password.
fn ctx_for(s: &Store, secret: &str, allow_temp: bool) -> Result<Ctx, ApiError> {
    let (me, sign_in) = Caller::Admin(secret.to_string()).person_any(s)?;
    if me.must_change_password && !allow_temp {
        return Err(ApiError::password_change_required());
    }
    let SignIn::Admin(session) = sign_in else { return Err(ApiError::internal()) };
    Ok(Ctx { me, session, csrf: csrf_for(secret) })
}

/// A GET page for a signed-in person (`allow_temp`: also with a temporary password).
async fn show(app: &App, headers: &HeaderMap, allow_temp: bool, f: impl FnOnce(&Store, &Ctx) -> Result<Page, ApiError> + Send + 'static) -> Response {
    let Some(secret) = session_secret(app.cfg(), headers) else { return redirect("/admin/login") };
    let r = app
        .run(move |s| {
            let ctx = ctx_for(s, &secret, allow_temp)?;
            Ok(match f(s, &ctx) {
                Ok(p) => render(s, &ctx, p),
                Err(e) => {
                    let status = e.status();
                    render(s, &ctx, Page { status, title: error_title(status).into(), body: error_body(&e, "/admin") })
                }
            })
        })
        .await;
    r.unwrap_or_else(|e| auth_redirect(&e).unwrap_or_else(|| plain_error(&e, "/admin")))
}

/// What a POST did: go to a page, or show one (results shown once, such as a temporary
/// password).
enum Done {
    Go(String),
    Show(Page),
}

/// The checks of every POST, in order: same origin (403), a session cookie (else to the
/// sign-in page), the CSRF token (403). The session secret.
fn post_checks(app: &App, headers: &HeaderMap, body: &[u8]) -> Result<String, Box<Response>> {
    if !same_origin(app.cfg(), headers) {
        tracing::warn!("refused an admin form posted from another site");
        return Err(Box::new(plain_error(&ApiError::forbidden("This form was sent from another site."), "/admin")));
    }
    let Some(secret) = session_secret(app.cfg(), headers) else { return Err(Box::new(redirect("/admin/login"))) };
    if !csrf_ok(&secret, body) {
        return Err(Box::new(plain_error(&ApiError::forbidden("This form has expired. Go back, reload the page and try again."), "/admin")));
    }
    Ok(secret)
}

/// A POST that changes something: checks (see `post_checks`), then `f` in the store; on
/// success the live sessions are re-checked before the answer goes out.
async fn act(app: &App, headers: &HeaderMap, body: Bytes, back: String, f: impl FnOnce(&Store, &Ctx, &[u8]) -> Result<Done, ApiError> + Send + 'static) -> Response {
    let secret = match post_checks(app, headers, &body) {
        Ok(s) => s,
        Err(res) => return *res,
    };
    let r = app
        .run(move |s| {
            let ctx = ctx_for(s, &secret, false)?;
            let done = f(s, &ctx, &body);
            let ok = done.is_ok();
            let res = match done {
                Ok(Done::Go(to)) => redirect(&to),
                Ok(Done::Show(p)) => render(s, &ctx, p),
                Err(e) => {
                    let status = e.status();
                    render(s, &ctx, Page { status, title: error_title(status).into(), body: error_body(&e, &back) })
                }
            };
            Ok((res, ok))
        })
        .await;
    match r {
        Ok((res, changed)) => {
            if changed {
                app.changed().await;
            }
            res
        }
        Err(e) => auth_redirect(&e).unwrap_or_else(|| plain_error(&e, "/admin")),
    }
}

/// A path segment from the URL kept to the characters ids and usernames have (for links back).
fn seg(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || ".-_".contains(*c)).take(64).collect()
}

fn field(body: &[u8], key: &str) -> String {
    api::form_value(body, key)
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

fn when(ts: i64) -> String {
    if ts <= 0 {
        return "never".into();
    }
    let iso = util::iso(ts);
    format!("{} {} UTC", &iso[..10], &iso[11..16])
}

fn bytes(n: i64) -> String {
    let n = n.max(0) as f64;
    match n {
        n if n < 1024.0 => format!("{n} B"),
        n if n < 1024.0 * 1024.0 => format!("{:.1} KB", n / 1024.0),
        n if n < 1024.0 * 1024.0 * 1024.0 => format!("{:.1} MB", n / 1048576.0),
        n => format!("{:.2} GB", n / 1073741824.0),
    }
}

/// Percent-encoding for a `mailto:` URL (everything but unreserved characters).
fn pct(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// Fixed messages after a change (`?m=`): never text from the URL.
fn flash(q: &HashMap<String, String>) -> String {
    let m = match q.get("m").map(String::as_str) {
        Some("saved") => "Saved.",
        Some("created") => "The organisation was created.",
        Some("deleted") => "The organisation was deleted.",
        Some("added") => "Added.",
        Some("removed") => "Removed.",
        Some("revoked") => "Revoked.",
        Some("password") => "Your password was changed. Your other sign-ins were ended.",
        Some("disabled") => "The account was disabled and signed out everywhere.",
        Some("enabled") => "The account was enabled.",
        Some("signed-out") => "Signed out of every device.",
        _ => return String::new(),
    };
    format!("<p class=\"msg ok\">{m}</p>")
}

fn role_select(name: &str, current: Option<OrgRole>, may_owner: bool) -> String {
    let mut opts = String::new();
    for r in [OrgRole::Member, OrgRole::Admin, OrgRole::Owner] {
        if r == OrgRole::Owner && !may_owner && current != Some(OrgRole::Owner) {
            continue;
        }
        let sel = if Some(r) == current { " selected" } else { "" };
        opts.push_str(&format!("<option value=\"{0}\"{sel}>{0}</option>", r.as_str()));
    }
    format!("<select name=\"{name}\" aria-label=\"Role\">{opts}</select>")
}

fn stat(label: &str, value: &str) -> String {
    format!("<div class=\"stat\"><span class=\"label\">{}</span><span class=\"value\">{}</span></div>", esc(label), esc(value))
}

fn password_form(ctx: &Ctx, temp: bool) -> String {
    let intro = if temp {
        "<p>You signed in with a temporary password. Choose your own password to continue.</p>"
    } else {
        "<p>Changing your password signs you out of your other devices.</p>"
    };
    let inner = format!(
        "<label>Current password<input type=\"password\" name=\"old_password\" autocomplete=\"current-password\" required></label>\
         <label>New password (at least {min} characters)<input type=\"password\" name=\"new_password\" autocomplete=\"new-password\" minlength=\"{min}\" required></label>\
         <label>New password again<input type=\"password\" name=\"new_password2\" autocomplete=\"new-password\" minlength=\"{min}\" required></label>\
         <button type=\"submit\">Change password</button>",
        min = util::MIN_PASSWORD
    );
    format!("{intro}{}", ctx.form("/admin/password", "stack", &inner))
}

// ---------------------------------------------------------------------------
// Sign-in, sign-out, password
// ---------------------------------------------------------------------------

fn login_html(status: StatusCode, error: &str) -> Response {
    let err = if error.is_empty() { String::new() } else { format!("<p class=\"msg error\">{}</p>", esc(error)) };
    let body = format!(
        "{err}<form method=\"post\" action=\"/admin/login\" class=\"stack narrow\">\
         <label>Username<input name=\"username\" autocomplete=\"username\" autocapitalize=\"none\" spellcheck=\"false\" maxlength=\"64\" required autofocus></label>\
         <label>Password<input type=\"password\" name=\"password\" autocomplete=\"current-password\" required></label>\
         <button type=\"submit\">Sign in</button></form>\
         <p class=\"muted\">For the people who manage this server's organisations and accounts.</p>"
    );
    html(status, "Sign in", None, &[], &body)
}

async fn login_page(State(app): State<App>, headers: HeaderMap) -> Response {
    if let Some(secret) = session_secret(app.cfg(), &headers)
        && app.run(move |s| Ok(s.admin_session(&secret)?.is_some())).await.unwrap_or(false)
    {
        return redirect("/admin");
    }
    login_html(StatusCode::OK, "")
}

/// `POST /admin/login`: the API's sign-in checks (limits, delay, Argon2 slots), then a new
/// session and its cookie.
async fn login(State(app): State<App>, ConnectInfo(Peer(peer)): ConnectInfo<Peer>, headers: HeaderMap, body: Bytes) -> Response {
    if !same_origin(app.cfg(), &headers) {
        tracing::warn!("refused an admin sign-in posted from another site");
        return login_html(StatusCode::FORBIDDEN, "This form was sent from another site.");
    }
    let ip = api::client_ip(app.cfg(), peer, &headers);
    let user = match api::check_sign_in(&app, ip, &field(&body, "username"), field(&body, "password")).await {
        Ok(u) => u,
        Err(e) => return login_html(e.status(), e.message()),
    };
    let agent: String = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let (uid, temp) = (user.id.clone(), user.must_change_password);
    let old = session_secret(app.cfg(), &headers);
    let created = app
        .run(move |s| {
            // The session this browser had (if any) ends: the new cookie replaces it.
            if let Some(old) = old {
                s.end_admin_session_by_secret(&old)?;
            }
            Ok(s.create_admin_session(&uid, &agent)?)
        })
        .await;
    match created {
        Ok(secret) => {
            tracing::info!("user '{}' signed in to the admin page from {ip}", user.username);
            let mut res = redirect(if temp { "/admin/password" } else { "/admin" });
            res.headers_mut().insert(header::SET_COOKIE, set_cookie(app.cfg(), &secret, ADMIN_SESSION_SECS));
            res
        }
        Err(e) => login_html(e.status(), e.message()),
    }
}

fn clear_cookie(app: &App, mut res: Response) -> Response {
    res.headers_mut().insert(header::SET_COOKIE, set_cookie(app.cfg(), "", 0));
    res
}

async fn logout(State(app): State<App>, headers: HeaderMap, body: Bytes) -> Response {
    if !same_origin(app.cfg(), &headers) {
        return plain_error(&ApiError::forbidden("This form was sent from another site."), "/admin");
    }
    let Some(secret) = session_secret(app.cfg(), &headers) else { return clear_cookie(&app, redirect("/admin/login")) };
    if !csrf_ok(&secret, &body) {
        return plain_error(&ApiError::forbidden("This form has expired. Go back, reload the page and try again."), "/admin");
    }
    let _ = app
        .run(move |s| {
            if let Some((_, sess)) = s.admin_session(&secret)? {
                s.end_admin_session(sess.id)?;
            }
            Ok(())
        })
        .await;
    clear_cookie(&app, redirect("/admin/login"))
}

async fn password_page(State(app): State<App>, headers: HeaderMap) -> Response {
    show(&app, &headers, true, |_, ctx| {
        let temp = ctx.me.must_change_password;
        Ok(Page::ok(if temp { "Choose a new password" } else { "Change password" }, password_form(ctx, temp)))
    })
    .await
}

/// `POST /admin/password`: the API's password change (current password checked and counted,
/// the policy for the new one); every other sign-in ends, this session stays.
async fn change_password(State(app): State<App>, headers: HeaderMap, body: Bytes) -> Response {
    let secret = match post_checks(&app, &headers, &body) {
        Ok(s) => s,
        Err(res) => return *res,
    };
    let (old, new, again) = (field(&body, "old_password"), field(&body, "new_password"), field(&body, "new_password2"));
    let result = if new != again {
        Err(ApiError::bad("The two new passwords are different."))
    } else {
        api::change_own_password(&app, Caller::Admin(secret.clone()), old, new).await
    };
    match result {
        Ok(_) => redirect("/admin?m=password"),
        Err(e) => {
            let r = app
                .run(move |s| {
                    let ctx = ctx_for(s, &secret, true)?;
                    let temp = ctx.me.must_change_password;
                    let body = format!("<p class=\"msg error\">{}</p>{}", esc(e.message()), password_form(&ctx, temp));
                    Ok(render(s, &ctx, Page { status: e.status(), title: "Change password".into(), body }))
                })
                .await;
            r.unwrap_or_else(|e| auth_redirect(&e).unwrap_or_else(|| plain_error(&e, "/admin")))
        }
    }
}

async fn stylesheet() -> Response {
    let mut res = STYLE.into_response();
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/css; charset=utf-8"));
    harden(&mut res);
    res
}

// ---------------------------------------------------------------------------
// Overview and organisations
// ---------------------------------------------------------------------------

async fn overview(State(app): State<App>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    show(&app, &headers, false, move |s, ctx| {
        let me = &ctx.me;
        let usage = s.usage()?;
        let used = |id: &str| usage.iter().find(|(o, _)| o == id).map(|(_, u)| u.bytes).unwrap_or(0);
        let managed: Vec<_> = api::visible_orgs(s, me)?.into_iter().filter(|(_, r)| me.is_admin || r.is_some_and(OrgRole::manages)).collect();
        let mut body = flash(&q);
        if !me.is_admin && managed.is_empty() {
            body.push_str("<p class=\"msg\">You don't administer anything on this server. You can change your password here.</p>");
            body.push_str(&password_form(ctx, false));
            return Ok(Page::ok("Overview", body));
        }
        let (people, storage) = if me.is_admin {
            (s.users()?.len(), usage.iter().map(|(_, u)| u.bytes).sum::<i64>())
        } else {
            let mut ids = std::collections::HashSet::new();
            for (o, _) in &managed {
                ids.extend(s.members(&o.id)?.into_iter().map(|m| m.user.id));
            }
            (ids.len(), managed.iter().map(|(o, _)| used(&o.account_id)).sum())
        };
        body.push_str(&format!(
            "<div class=\"stats\">{}{}{}</div>",
            stat("Organisations", &managed.len().to_string()),
            stat("People", &people.to_string()),
            stat("Storage", &bytes(storage))
        ));
        if !me.is_admin {
            body.push_str("<p class=\"muted\">Counts for the organisations you manage.</p>");
        }
        body.push_str("<h2>Organisations</h2>");
        body.push_str(&org_table(s, &managed, &usage)?);
        if me.is_admin {
            body.push_str("<p><a href=\"/admin/people\">People</a></p>");
        }
        Ok(Page::ok("Overview", body))
    })
    .await
}

type UsageList = Vec<(String, crate::store::Usage)>;

fn org_table(s: &Store, orgs: &[(crate::store::Org, Option<OrgRole>)], usage: &UsageList) -> Result<String, ApiError> {
    if orgs.is_empty() {
        return Ok("<p class=\"muted\">No organisations yet.</p>".into());
    }
    let mut rows = String::new();
    for (o, r) in orgs {
        let used = usage.iter().find(|(id, _)| *id == o.account_id).map(|(_, u)| u.bytes).unwrap_or(0);
        rows.push_str(&format!(
            "<tr><td><a href=\"/admin/orgs/{}\">{}</a></td><td>{}</td><td class=\"num\">{}</td><td>{}</td><td class=\"num\">{}</td></tr>",
            esc(&o.id),
            esc(&o.name),
            r.map(OrgRole::as_str).unwrap_or("-"),
            s.members(&o.id)?.len(),
            o.member_role.as_str(),
            bytes(used)
        ));
    }
    Ok(format!(
        "<div class=\"table\"><table><thead><tr><th>Name</th><th>Your role</th><th class=\"num\">Members</th><th>Members can</th><th class=\"num\">Storage</th></tr></thead><tbody>{rows}</tbody></table></div>"
    ))
}

async fn orgs_page(State(app): State<App>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    show(&app, &headers, false, move |s, ctx| {
        let me = &ctx.me;
        let orgs: Vec<_> = api::visible_orgs(s, me)?.into_iter().filter(|(_, r)| me.is_admin || r.is_some_and(OrgRole::manages)).collect();
        let mut body = flash(&q);
        if !me.is_admin && orgs.is_empty() {
            body.push_str("<p class=\"msg\">You don't administer anything on this server.</p>");
            return Ok(Page::ok("Organisations", body));
        }
        body.push_str(&org_table(s, &orgs, &s.usage()?)?);
        if me.is_admin {
            body.push_str("<h2>New organisation</h2>");
            body.push_str(&ctx.form(
                "/admin/orgs",
                "row",
                "<label>Name<input name=\"name\" maxlength=\"100\" required></label><button type=\"submit\">Create</button>",
            ));
        }
        Ok(Page::ok("Organisations", body))
    })
    .await
}

async fn create_org(State(app): State<App>, headers: HeaderMap, body: Bytes) -> Response {
    act(&app, &headers, body, "/admin/orgs".into(), |s, ctx, b| {
        let o = api::create_org_as(s, &ctx.me, &field(b, "name"))?;
        Ok(Done::Go(format!("/admin/orgs/{}?m=created", o.id)))
    })
    .await
}

async fn org_page(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    show(&app, &headers, false, move |s, ctx| {
        let me = &ctx.me;
        let (o, mine) = api::org_for(s, me, &org)?;
        api::need_manage(me, mine)?;
        let may_owner = me.is_admin || mine == Some(OrgRole::Owner);
        let base = format!("/admin/orgs/{}", o.id);
        let use_ = s.usage_of(&o.account_id)?;
        let members = s.members(&o.id)?;
        let mut body = flash(&q);
        body.push_str(&format!(
            "<div class=\"stats\">{}{}{}{}</div>",
            stat("Members", &members.len().to_string()),
            stat("Storage", &bytes(use_.bytes)),
            stat("Files", &use_.files.to_string()),
            stat("Your role", mine.map(OrgRole::as_str).unwrap_or("server administrator"))
        ));

        // Members
        body.push_str("<h2>Members</h2>");
        let mut rows = String::new();
        for m in &members {
            let u = &m.user;
            let touch = may_owner || m.role != OrgRole::Owner;
            let path = format!("{base}/members/{}", u.username);
            let controls = if touch {
                format!(
                    "{}{}",
                    ctx.form(&format!("{path}/role"), "inline", &format!("{}<button type=\"submit\">Change</button>", role_select("role", Some(m.role), may_owner))),
                    ctx.button(&format!("{path}/remove"), if u.id == me.id { "Leave" } else { "Remove" }, "danger")
                )
            } else {
                "<span class=\"muted\">only owners</span>".into()
            };
            let email = if me.is_admin { format!("<td>{}</td>", esc(&u.email)) } else { String::new() };
            let status = if u.disabled { "<span class=\"tag off\">disabled</span>" } else { "" };
            rows.push_str(&format!(
                "<tr><td>{} {status}<div class=\"muted\">{}</div></td>{email}<td>{}</td><td>{}</td><td class=\"actions\">{controls}</td></tr>",
                esc(&u.display_name),
                esc(&u.username),
                m.role.as_str(),
                when(m.created)
            ));
        }
        let email_head = if me.is_admin { "<th>Email</th>" } else { "" };
        body.push_str(&format!(
            "<div class=\"table\"><table><thead><tr><th>Person</th>{email_head}<th>Role</th><th>Since</th><th></th></tr></thead><tbody>{rows}</tbody></table></div>"
        ));

        // Adding people
        body.push_str("<h2>Add people</h2><div class=\"cards\">");
        body.push_str(&format!(
            "<section class=\"card\"><h3>Someone with an account</h3><p class=\"muted\">{}</p>{}</section>",
            if me.is_admin { "Anyone on this server." } else { "People who are already in one of your organisations. Anyone else: send them an invite link; they can accept it with their existing account." },
            ctx.form(
                &format!("{base}/members"),
                "stack",
                &format!("<label>Username<input name=\"username\" maxlength=\"64\" autocapitalize=\"none\" spellcheck=\"false\" required></label><label>Role{}</label><button type=\"submit\">Add</button>", role_select("role", Some(OrgRole::Member), may_owner))
            )
        ));
        body.push_str(&format!(
            "<section class=\"card\"><h3>New account</h3><p class=\"muted\">With a temporary password that you pass on; they choose their own when they first sign in.</p>{}</section>",
            ctx.form(
                &format!("{base}/accounts"),
                "stack",
                &format!(
                    "<label>Username<input name=\"username\" maxlength=\"64\" autocapitalize=\"none\" spellcheck=\"false\" required></label>\
                     <label>Name<input name=\"display_name\" maxlength=\"100\"></label>\
                     <label>Email (optional)<input type=\"email\" name=\"email\" maxlength=\"254\"></label>\
                     <label>Role{}</label><button type=\"submit\">Create account</button>",
                    role_select("role", Some(OrgRole::Member), may_owner)
                )
            )
        ));
        body.push_str(&format!(
            "<section class=\"card\"><h3>Invite link</h3><p class=\"muted\">Works once, for {INVITE_DAYS} days; the person signs in with the account they have, or creates one.</p>{}</section>",
            ctx.form(
                &format!("{base}/invites"),
                "stack",
                &format!("<label>Email (optional)<input type=\"email\" name=\"email\" maxlength=\"254\"></label><label>Role{}</label><button type=\"submit\">Create invite link</button>", role_select("role", Some(OrgRole::Member), may_owner))
            )
        ));
        body.push_str("</div>");

        // Open invites
        let invites = s.invites_of(&o.id)?;
        body.push_str("<h2>Open invites</h2>");
        if invites.is_empty() {
            body.push_str("<p class=\"muted\">None.</p>");
        } else {
            let mut rows = String::new();
            for i in &invites {
                rows.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"actions\">{}</td></tr>",
                    if i.email.is_empty() { "<span class=\"muted\">no email</span>".to_string() } else { esc(&i.email) },
                    i.role.as_str(),
                    esc(&s.display_name(&i.created_by)),
                    when(i.expires),
                    ctx.button(&format!("{base}/invites/{}/revoke", i.id), "Revoke", "danger")
                ));
            }
            body.push_str(&format!(
                "<div class=\"table\"><table><thead><tr><th>Email</th><th>Role</th><th>Made by</th><th>Expires</th><th></th></tr></thead><tbody>{rows}</tbody></table></div>"
            ));
        }

        // Settings
        body.push_str("<h2>Settings</h2>");
        let opt = |r: Role, label: &str| format!("<option value=\"{}\"{}>{label}</option>", r.as_str(), if o.member_role == r { " selected" } else { "" });
        body.push_str(&ctx.form(
            &format!("{base}/settings"),
            "row",
            &format!(
                "<label>Members can<select name=\"member_role\">{}{}</select></label><button type=\"submit\">Save</button>",
                opt(Role::Edit, "edit the organisation's files"),
                opt(Role::View, "only view the organisation's files")
            ),
        ));
        if me.is_admin {
            body.push_str(&ctx.form(
                &format!("{base}/rename"),
                "row",
                &format!("<label>Name<input name=\"name\" value=\"{}\" maxlength=\"100\" required></label><button type=\"submit\">Rename</button>", esc(&o.name)),
            ));
            body.push_str(&ctx.form(
                &format!("{base}/delete"),
                "row",
                "<label class=\"check\"><input type=\"checkbox\" name=\"confirm\" value=\"yes\" required> Delete this organisation (only when its folder is empty)</label><button type=\"submit\" class=\"danger\">Delete</button>",
            ));
        }
        Ok(Page::ok(o.name.clone(), body))
    })
    .await
}

async fn org_settings(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>, body: Bytes) -> Response {
    act(&app, &headers, body, format!("/admin/orgs/{}", seg(&org)), move |s, ctx, b| {
        let (o, _) = api::patch_org_as(s, &ctx.me, &org, None, Some(&field(b, "member_role")))?;
        Ok(Done::Go(format!("/admin/orgs/{}?m=saved", o.id)))
    })
    .await
}

async fn rename_org(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>, body: Bytes) -> Response {
    act(&app, &headers, body, format!("/admin/orgs/{}", seg(&org)), move |s, ctx, b| {
        let (o, _) = api::patch_org_as(s, &ctx.me, &org, Some(&field(b, "name")), None)?;
        Ok(Done::Go(format!("/admin/orgs/{}?m=saved", o.id)))
    })
    .await
}

async fn delete_org(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>, body: Bytes) -> Response {
    act(&app, &headers, body, format!("/admin/orgs/{}", seg(&org)), move |s, ctx, b| {
        if field(b, "confirm") != "yes" {
            return Err(ApiError::bad("Tick the box to confirm."));
        }
        api::delete_org_as(s, &ctx.me, &org)?;
        Ok(Done::Go("/admin/orgs?m=deleted".into()))
    })
    .await
}

async fn add_member(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>, body: Bytes) -> Response {
    act(&app, &headers, body, format!("/admin/orgs/{}", seg(&org)), move |s, ctx, b| {
        let role = field(b, "role");
        let (o, added) = api::set_member_as(s, &ctx.me, &org, &field(b, "username"), Some(&role))?;
        Ok(Done::Go(format!("/admin/orgs/{}?m={}", o.id, if added { "added" } else { "saved" })))
    })
    .await
}

async fn member_role(State(app): State<App>, headers: HeaderMap, Path((org, username)): Path<(String, String)>, body: Bytes) -> Response {
    act(&app, &headers, body, format!("/admin/orgs/{}", seg(&org)), move |s, ctx, b| {
        let o = api::change_member_as(s, &ctx.me, &org, &username, &field(b, "role"))?;
        Ok(Done::Go(format!("/admin/orgs/{}?m=saved", o.id)))
    })
    .await
}

async fn remove_member(State(app): State<App>, headers: HeaderMap, Path((org, username)): Path<(String, String)>, body: Bytes) -> Response {
    act(&app, &headers, body, format!("/admin/orgs/{}", seg(&org)), move |s, ctx, _| {
        let o = api::remove_member_as(s, &ctx.me, &org, &username)?;
        // Whoever left their own organisation may not see it any more.
        let still = api::org_for(s, &ctx.me, &o.id).is_ok_and(|(_, r)| ctx.me.is_admin || r.is_some_and(OrgRole::manages));
        Ok(Done::Go(if still { format!("/admin/orgs/{}?m=removed", o.id) } else { "/admin?m=removed".into() }))
    })
    .await
}

async fn create_invite(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>, body: Bytes) -> Response {
    let base_url = app.cfg().base_url();
    act(&app, &headers, body, format!("/admin/orgs/{}", seg(&org)), move |s, ctx, b| {
        let email = field(b, "email");
        let (inv, o, url) = api::create_invite_as(s, &ctx.me, &base_url, &org, &email, Some(&field(b, "role")))?;
        let subject = format!("Join {} on FreedomOffice", o.name);
        let text = format!(
            "Hello,\n\nYou are invited to join {} on FreedomOffice. Open this link and sign in with your FreedomOffice account, or create one (it works once and expires in {INVITE_DAYS} days):\n\n{url}\n\nThen sign in from FreedomOffice Sheets (File > Account).\n\n{}",
            o.name, ctx.me.display_name
        );
        let mailto = format!("mailto:{}?subject={}&body={}", pct(&inv.email).replace("%40", "@"), pct(&subject), pct(&text));
        let body = format!(
            "<p class=\"msg ok\">An invite to {} as {} was made{}.</p>\
             <p>This link is shown only now. Copy it (click it to select) or open an email with it:</p>\
             <p class=\"secret\">{}</p>\
             <p><a class=\"button\" href=\"{}\">Open an email with the link</a></p>\
             <p class=\"muted\">Anyone with the link can join until it is used or revoked.</p>\
             <p><a href=\"/admin/orgs/{}\">Back to {}</a></p>",
            esc(&o.name),
            inv.role.as_str(),
            if inv.email.is_empty() { String::new() } else { format!(" for {}", esc(&inv.email)) },
            esc(&url),
            esc(&mailto),
            esc(&o.id),
            esc(&o.name)
        );
        Ok(Done::Show(Page::ok("Invite link", body)))
    })
    .await
}

async fn revoke_invite(State(app): State<App>, headers: HeaderMap, Path((org, id)): Path<(String, String)>, body: Bytes) -> Response {
    act(&app, &headers, body, format!("/admin/orgs/{}", seg(&org)), move |s, ctx, _| {
        api::delete_invite_as(s, &ctx.me, &org, &id)?;
        let (o, _) = api::org_for(s, &ctx.me, &org)?;
        Ok(Done::Go(format!("/admin/orgs/{}?m=revoked", o.id)))
    })
    .await
}

/// The page with a new temporary password (shown once, never stored in clear or logged).
fn temp_password_page(user: &User, pw: &str, what: &str, back: &str, back_label: &str) -> Page {
    let body = format!(
        "<p class=\"msg ok\">{}</p>\
         <p>Temporary password for <b>{}</b> ({}). It is shown only now; pass it on privately. They must choose their own password when they sign in.</p>\
         <p class=\"secret\">{}</p><p class=\"muted\">Click the password to select it, then copy it.</p>\
         <p><a href=\"{}\">{}</a></p>",
        esc(what),
        esc(&user.display_name),
        esc(&user.username),
        esc(pw),
        esc(back),
        esc(back_label)
    );
    Page::ok("Temporary password", body)
}

/// The end of a POST that creates or resets a password (`post_checks` passed, giving
/// `secret`): the API flow hashes outside the store lock, then the page is rendered.
async fn with_temp_password(
    app: &App,
    secret: String,
    back: String,
    flow: impl std::future::Future<Output = Result<(User, String), ApiError>>,
    page: impl FnOnce(&User, &str) -> Page + Send + 'static,
) -> Response {
    let r = flow.await;
    let r = app
        .run(move |s| {
            let ctx = ctx_for(s, &secret, false)?;
            Ok(match r {
                Ok((u, pw)) => render(s, &ctx, page(&u, &pw)),
                Err(e) => {
                    let status = e.status();
                    render(s, &ctx, Page { status, title: error_title(status).into(), body: error_body(&e, &back) })
                }
            })
        })
        .await;
    r.unwrap_or_else(|e| auth_redirect(&e).unwrap_or_else(|| plain_error(&e, "/admin")))
}

fn account_body(b: &[u8], org: Option<String>) -> api::AccountBody {
    let role = field(b, "role");
    api::AccountBody {
        username: field(b, "username"),
        display_name: field(b, "display_name"),
        email: field(b, "email"),
        org,
        role: if role.is_empty() { None } else { Some(role) },
    }
}

async fn create_org_account(State(app): State<App>, headers: HeaderMap, Path(org): Path<String>, body: Bytes) -> Response {
    let back = format!("/admin/orgs/{}", seg(&org));
    let secret = match post_checks(&app, &headers, &body) {
        Ok(secret) => secret,
        Err(res) => return *res,
    };
    let caller = Caller::Admin(secret.clone());
    let b = account_body(&body, Some(org));
    let flow = api::create_account_as(&app, caller, b);
    let back2 = back.clone();
    with_temp_password(&app, secret, back, flow, move |u, pw| temp_password_page(u, pw, &format!("The account {} was created.", u.username), &back2, "Back to the organisation")).await
}

// ---------------------------------------------------------------------------
// People (server administrator)
// ---------------------------------------------------------------------------

async fn people_page(State(app): State<App>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    show(&app, &headers, false, move |s, ctx| {
        api::need_admin(&ctx.me)?;
        let usage = s.usage()?;
        let last = s.last_logins()?;
        let mut rows = String::new();
        for u in s.users()? {
            let used = usage.iter().find(|(id, _)| *id == u.id).map(|(_, x)| x.bytes).unwrap_or(0);
            let orgs: Vec<String> = s.orgs_of(&u.id)?.iter().map(|(o, r)| format!("{} <span class=\"muted\">({})</span>", esc(&o.name), r.as_str())).collect();
            let mut tags = String::new();
            if u.is_admin {
                tags.push_str("<span class=\"tag\">server admin</span>");
            }
            tags.push_str(if u.disabled {
                "<span class=\"tag off\">disabled</span>"
            } else if u.must_change_password {
                "<span class=\"tag warn\">temporary password</span>"
            } else {
                "<span class=\"tag on\">active</span>"
            });
            rows.push_str(&format!(
                "<tr><td><a href=\"/admin/people/{}\">{}</a><div class=\"muted\">{}</div></td><td>{tags}</td><td>{}</td><td>{}</td><td class=\"num\">{}</td></tr>",
                esc(&u.username),
                esc(&u.display_name),
                esc(&u.username),
                if orgs.is_empty() { "<span class=\"muted\">none</span>".to_string() } else { orgs.join(", ") },
                when(last.get(&u.id).copied().unwrap_or(0)),
                bytes(used)
            ));
        }
        let mut body = flash(&q);
        body.push_str(&format!(
            "<div class=\"table\"><table><thead><tr><th>Person</th><th>Status</th><th>Organisations</th><th>Last sign-in</th><th class=\"num\">Storage</th></tr></thead><tbody>{rows}</tbody></table></div>"
        ));
        body.push_str("<h2>New account</h2><p class=\"muted\">Without an organisation; add them to one afterwards (or create the account from the organisation's page).</p>");
        body.push_str(&ctx.form(
            "/admin/people",
            "stack narrow",
            "<label>Username<input name=\"username\" maxlength=\"64\" autocapitalize=\"none\" spellcheck=\"false\" required></label>\
             <label>Name<input name=\"display_name\" maxlength=\"100\"></label>\
             <label>Email (optional)<input type=\"email\" name=\"email\" maxlength=\"254\"></label>\
             <button type=\"submit\">Create account</button>",
        ));
        Ok(Page::ok("People", body))
    })
    .await
}

async fn create_account(State(app): State<App>, headers: HeaderMap, body: Bytes) -> Response {
    let secret = match post_checks(&app, &headers, &body) {
        Ok(secret) => secret,
        Err(res) => return *res,
    };
    let caller = Caller::Admin(secret.clone());
    let b = account_body(&body, None);
    let flow = api::create_account_as(&app, caller, b);
    with_temp_password(&app, secret, "/admin/people".into(), flow, |u, pw| {
        let back = format!("/admin/people/{}", u.username);
        temp_password_page(u, pw, &format!("The account {} was created.", u.username), &back, "Go to the account")
    })
    .await
}

async fn person_page(State(app): State<App>, headers: HeaderMap, Path(username): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    show(&app, &headers, false, move |s, ctx| {
        let (u, devices, web) = api::sign_ins_as(s, &ctx.me, &username)?;
        let base = format!("/admin/people/{}", u.username);
        let used = s.usage_of(&u.id)?;
        let last = s.last_logins()?.get(&u.id).copied().unwrap_or(0);
        let mut body = flash(&q);
        let status = if u.disabled {
            "disabled"
        } else if u.must_change_password {
            "temporary password"
        } else {
            "active"
        };
        body.push_str(&format!(
            "<dl class=\"facts\"><dt>Username</dt><dd>{}</dd><dt>Email</dt><dd>{}</dd><dt>Status</dt><dd>{status}{}</dd><dt>Created</dt><dd>{}</dd><dt>Last sign-in</dt><dd>{}</dd><dt>Storage</dt><dd>{} in {} file(s)</dd></dl>",
            esc(&u.username),
            if u.email.is_empty() { "-".to_string() } else { esc(&u.email) },
            if u.is_admin { ", server administrator" } else { "" },
            when(u.created),
            when(last),
            bytes(used.bytes),
            used.files
        ));
        let orgs = s.orgs_of(&u.id)?;
        body.push_str("<h2>Organisations</h2>");
        if orgs.is_empty() {
            body.push_str("<p class=\"muted\">None.</p>");
        } else {
            let items: String = orgs.iter().map(|(o, r)| format!("<li><a href=\"/admin/orgs/{}\">{}</a> <span class=\"muted\">{}</span></li>", esc(&o.id), esc(&o.name), r.as_str())).collect();
            body.push_str(&format!("<ul>{items}</ul>"));
        }

        body.push_str("<h2>Account</h2><div class=\"buttons\">");
        if u.disabled {
            body.push_str(&ctx.button(&format!("{base}/enable"), "Enable", ""));
        } else if u.id != ctx.me.id {
            body.push_str(&ctx.button(&format!("{base}/disable"), "Disable and sign out", "danger"));
        }
        // Your own password: Change password (a reset would sign you out before you saw it).
        if u.id != ctx.me.id {
            body.push_str(&ctx.button(&format!("{base}/reset-password"), "Reset password", ""));
        }
        body.push_str(&ctx.button(&format!("{base}/sign-out"), "Sign out of all devices", ""));
        let admin_label = if u.is_admin { "Remove server administrator" } else { "Make server administrator" };
        body.push_str(&ctx.form(&format!("{base}/admin"), "inline", &format!("<input type=\"hidden\" name=\"on\" value=\"{}\"><button type=\"submit\">{admin_label}</button>", if u.is_admin { "0" } else { "1" })));
        body.push_str("</div><p class=\"muted\">Resetting the password shows you a new temporary password once and signs them out everywhere.</p>");

        body.push_str("<h2>Active sign-ins</h2>");
        if devices.is_empty() && web.is_empty() {
            body.push_str("<p class=\"muted\">None.</p>");
        } else {
            let mut rows = String::new();
            for d in &devices {
                rows.push_str(&format!(
                    "<tr><td>{}</td><td>app</td><td>{}</td><td>{}</td><td class=\"actions\">{}</td></tr>",
                    esc(&d.device),
                    when(d.created),
                    when(d.last_used),
                    ctx.button(&format!("{base}/sessions/device/{}/revoke", d.id), "Sign out", "danger")
                ));
            }
            for w in &web {
                let this = if w.id == ctx.session { " <span class=\"tag\">this browser</span>" } else { "" };
                rows.push_str(&format!(
                    "<tr><td>{}{this}</td><td>admin page</td><td>{}</td><td>{}</td><td class=\"actions\">{}</td></tr>",
                    if w.agent.is_empty() { "Browser".to_string() } else { esc(&w.agent) },
                    when(w.created),
                    when(w.last_used),
                    ctx.button(&format!("{base}/sessions/web/{}/revoke", w.id), "Sign out", "danger")
                ));
            }
            body.push_str(&format!(
                "<div class=\"table\"><table><thead><tr><th>Device</th><th>Kind</th><th>Signed in</th><th>Last used</th><th></th></tr></thead><tbody>{rows}</tbody></table></div>"
            ));
        }
        Ok(Page::ok(u.display_name.clone(), body))
    })
    .await
}

async fn disable(State(app): State<App>, headers: HeaderMap, Path(username): Path<String>, body: Bytes) -> Response {
    act(&app, &headers, body, format!("/admin/people/{}", seg(&username)), move |s, ctx, _| {
        let u = api::set_disabled_as(s, &ctx.me, &username, true)?;
        Ok(Done::Go(format!("/admin/people/{}?m=disabled", u.username)))
    })
    .await
}

async fn enable(State(app): State<App>, headers: HeaderMap, Path(username): Path<String>, body: Bytes) -> Response {
    act(&app, &headers, body, format!("/admin/people/{}", seg(&username)), move |s, ctx, _| {
        let u = api::set_disabled_as(s, &ctx.me, &username, false)?;
        Ok(Done::Go(format!("/admin/people/{}?m=enabled", u.username)))
    })
    .await
}

async fn reset_password(State(app): State<App>, headers: HeaderMap, Path(username): Path<String>, body: Bytes) -> Response {
    let back = format!("/admin/people/{}", seg(&username));
    let secret = match post_checks(&app, &headers, &body) {
        Ok(secret) => secret,
        Err(res) => return *res,
    };
    let caller = Caller::Admin(secret.clone());
    let flow = api::reset_password_as(&app, caller, username);
    let back2 = back.clone();
    with_temp_password(&app, secret, back, flow, move |u, pw| temp_password_page(u, pw, "The password was reset and every sign-in ended.", &back2, "Back to the account")).await
}

async fn sign_out_everywhere(State(app): State<App>, headers: HeaderMap, Path(username): Path<String>, body: Bytes) -> Response {
    act(&app, &headers, body, format!("/admin/people/{}", seg(&username)), move |s, ctx, _| {
        let (u, _) = api::sign_out_everywhere_as(s, &ctx.me, &username)?;
        Ok(Done::Go(format!("/admin/people/{}?m=signed-out", u.username)))
    })
    .await
}

async fn set_super_admin(State(app): State<App>, headers: HeaderMap, Path(username): Path<String>, body: Bytes) -> Response {
    act(&app, &headers, body, format!("/admin/people/{}", seg(&username)), move |s, ctx, b| {
        let u = api::set_super_admin_as(s, &ctx.me, &username, field(b, "on") == "1")?;
        Ok(Done::Go(if u.id == ctx.me.id && !u.is_admin { "/admin?m=saved".into() } else { format!("/admin/people/{}?m=saved", u.username) }))
    })
    .await
}

async fn revoke_sign_in(State(app): State<App>, headers: HeaderMap, Path((username, kind, id)): Path<(String, String, i64)>, body: Bytes) -> Response {
    act(&app, &headers, body, format!("/admin/people/{}", seg(&username)), move |s, ctx, _| {
        let which = match kind.as_str() {
            "device" => SignIn::Device(id),
            "web" => SignIn::Admin(id),
            _ => return Err(ApiError::not_found()),
        };
        let u = api::end_sign_in_as(s, &ctx.me, &username, which)?;
        Ok(Done::Go(format!("/admin/people/{}?m=revoked", u.username)))
    })
    .await
}

// ---------------------------------------------------------------------------
// Style (served from /admin/style.css: the CSP allows no inline styles)
// ---------------------------------------------------------------------------

const STYLE: &str = r#":root{--bg:#f6f7f6;--fg:#1d2320;--muted:#5f6b65;--card:#fff;--line:#d9dfdb;--accent:#217346;--accent-fg:#fff;--danger:#b3261e;--ok-bg:#e6f4ea;--err-bg:#fbe9e7;--warn:#8a5a00;color-scheme:light dark}
@media (prefers-color-scheme:dark){:root{--bg:#141816;--fg:#e4e9e6;--muted:#9aa7a0;--card:#1d2320;--line:#323b36;--accent:#4caf7a;--accent-fg:#0d1a12;--danger:#f28b82;--ok-bg:#1f3527;--err-bg:#3d1f1c;--warn:#f0c060}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.5 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}
a{color:var(--accent)}
.top{display:flex;flex-wrap:wrap;align-items:center;gap:.5rem 1.5rem;padding:.75rem 1.25rem;background:var(--card);border-bottom:1px solid var(--line)}
.brand{font-weight:700;text-decoration:none;color:var(--fg)}.brand span{color:var(--accent);font-weight:600}
nav{display:flex;flex-wrap:wrap;gap:.25rem 1rem;flex:1}nav a{text-decoration:none}nav a:hover{text-decoration:underline}
.who{display:flex;align-items:center;gap:.75rem;color:var(--muted)}
main{max-width:1100px;margin:0 auto;padding:1.25rem}
h1{font-size:1.6rem;margin:.5rem 0 1rem}h2{font-size:1.2rem;margin:2rem 0 .75rem}h3{font-size:1rem;margin:0 0 .25rem}
.muted{color:var(--muted);font-size:.9rem}
.msg{padding:.6rem .9rem;border-radius:6px;background:var(--card);border:1px solid var(--line)}.msg.ok{background:var(--ok-bg)}.msg.error{background:var(--err-bg);border-color:var(--danger)}
.stats{display:grid;grid-template-columns:repeat(auto-fit,minmax(160px,1fr));gap:.75rem}
.stat{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:.75rem 1rem}.stat .label{display:block;color:var(--muted);font-size:.85rem}.stat .value{font-size:1.5rem;font-weight:600}
.table{overflow-x:auto;background:var(--card);border:1px solid var(--line);border-radius:8px}
table{border-collapse:collapse;width:100%}th,td{text-align:left;padding:.55rem .75rem;border-bottom:1px solid var(--line);vertical-align:top}tbody tr:last-child td{border-bottom:0}th{font-size:.85rem;color:var(--muted);font-weight:600}
.num{text-align:right;white-space:nowrap}.actions{white-space:nowrap;text-align:right}
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(260px,1fr));gap:.75rem}.card{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:1rem}
form.stack{display:flex;flex-direction:column;gap:.6rem}form.narrow{max-width:420px}
form.row{display:flex;flex-wrap:wrap;align-items:flex-end;gap:.6rem;margin:.5rem 0}
form.inline{display:inline-flex;gap:.35rem;align-items:center;margin:.1rem .2rem}
label{display:flex;flex-direction:column;gap:.2rem;font-size:.9rem}label.check{flex-direction:row;align-items:center;gap:.5rem}
input,select,button,.button{font:inherit;padding:.45rem .6rem;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--fg)}
input:focus,select:focus,button:focus{outline:2px solid var(--accent);outline-offset:1px}
button,.button{background:var(--accent);color:var(--accent-fg);border-color:var(--accent);cursor:pointer;font-weight:600;text-decoration:none;display:inline-block}
button.danger{background:transparent;color:var(--danger);border-color:var(--danger)}
button.link{background:none;border:0;color:var(--accent);padding:0;font-weight:400;text-decoration:underline}
.buttons{display:flex;flex-wrap:wrap;gap:.25rem}
.tag{display:inline-block;font-size:.75rem;padding:0 .45rem;border-radius:999px;border:1px solid var(--line);margin-right:.25rem}.tag.on{color:var(--accent)}.tag.off{color:var(--danger)}.tag.warn{color:var(--warn)}
.secret{font:1.15rem/1.4 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;background:var(--card);border:1px dashed var(--accent);border-radius:6px;padding:.75rem 1rem;user-select:all;-webkit-user-select:all;word-break:break-all}
dl.facts{display:grid;grid-template-columns:max-content 1fr;gap:.3rem 1rem;background:var(--card);border:1px solid var(--line);border-radius:8px;padding:1rem;margin:0}dl.facts dt{color:var(--muted)}dl.facts dd{margin:0}
@media (max-width:600px){main{padding:.75rem}.top{padding:.6rem .75rem}th,td{padding:.45rem .5rem}.actions{white-space:normal}}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins() {
        assert_eq!(origin_of("https://Files.Example.com/fo/"), "https://files.example.com");
        assert_eq!(origin_of("http://127.0.0.1:8080"), "http://127.0.0.1:8080");
        let cfg = Config { public_url: "https://files.example.com".into(), ..Config::default() };
        let mut h = HeaderMap::new();
        assert!(same_origin(&cfg, &h), "no header: CSRF token and SameSite protect");
        h.insert(header::ORIGIN, HeaderValue::from_static("https://files.example.com"));
        assert!(same_origin(&cfg, &h));
        h.insert(header::ORIGIN, HeaderValue::from_static("null"));
        assert!(!same_origin(&cfg, &h));
        h.insert(header::ORIGIN, HeaderValue::from_static("https://files.example.com.evil.test"));
        assert!(!same_origin(&cfg, &h));
        let mut h = HeaderMap::new();
        h.insert(header::REFERER, HeaderValue::from_static("https://evil.test/https://files.example.com/"));
        assert!(!same_origin(&cfg, &h));
    }

    #[test]
    fn cookies_and_csrf() {
        let cfg = Config { public_url: "https://files.example.com".into(), ..Config::default() };
        assert_eq!(cookie_name(&cfg), "__Host-fo_admin");
        let v = set_cookie(&cfg, "abc", 60);
        assert_eq!(v.to_str().unwrap(), "__Host-fo_admin=abc; Path=/; Max-Age=60; HttpOnly; SameSite=Strict; Secure");
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("a=1; __Host-fo_admin=secret; b=2"));
        assert_eq!(session_secret(&cfg, &h).as_deref(), Some("secret"));
        let dev = Config { public_url: "http://127.0.0.1:8080".into(), ..Config::default() };
        assert_eq!(cookie_name(&dev), "fo_admin");
        assert_eq!(session_secret(&dev, &h), None);
        assert_ne!(csrf_for("a"), csrf_for("b"));
        assert!(csrf_ok("a", format!("x=1&csrf={}", csrf_for("a")).as_bytes()));
        assert!(!csrf_ok("a", b"csrf="));
        assert!(!csrf_ok("a", b""));
    }
}
