//! Browser sessions for the JSON API (FreedomOffice Drive at `/drive`).
//!
//! `POST /api/web/login` signs in exactly like `/api/login` (`api::check_sign_in`: invalid
//! usernames refused first, the attempt counted up front per address and username, the delay,
//! Argon2 in its slots) and sets the cookie `__Host-fo_web` (HttpOnly, Secure, SameSite=Strict,
//! Path=/, 12 h absolute). Only the SHA-256 of its secret is stored, in `admin_sessions` with
//! `kind = 'web'`; a web session never opens `/admin` and an admin session never opens the API.
//!
//! Every `/api/*` endpoint accepts the cookie instead of `Authorization: Bearer` (a request with
//! an Authorization header is judged by it alone, as before). With the cookie, requests other
//! than GET/HEAD must carry `X-FO-CSRF: <csrf>` (from the sign-in or `GET /api/web/session`,
//! compared in constant time) and come from this server's origin; a request a browser marks
//! `Sec-Fetch-Site: cross-site` or `same-site` is refused whatever its method. `web_gate` (a
//! router-wide middleware) does those checks and hands the verdict to `api::auth_any` in an
//! internal header that clients can't set (it is removed from every incoming request).

use std::net::IpAddr;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::api::{self, ApiError, ApiResult, App, Peer};
use crate::config::Config;
use crate::store::{SessionKind, ADMIN_SESSION_SECS};
use crate::util;

/// Internal: `ok <secret>` or `bad <code>`, set only by `web_gate`.
pub(crate) const VERDICT: &str = "x-fo-internal-web";

/// The request header carrying the session's CSRF token.
pub const CSRF_HEADER: &str = "x-fo-csrf";

tokio::task_local! {
    /// The client address of the request being served (for the audit log).
    pub(crate) static REQUEST_IP: String;
}

/// The cookie's name: `__Host-fo_web`, or `fo_web` on a plain-http loopback setup (as for the
/// admin cookie, see `admin::secure`).
pub fn cookie_name(cfg: &Config) -> &'static str {
    if crate::admin::secure(cfg) { "__Host-fo_web" } else { "fo_web" }
}

fn set_cookie(cfg: &Config, value: &str, max_age: i64) -> HeaderValue {
    let flags = if crate::admin::secure(cfg) { "; Secure" } else { "" };
    HeaderValue::from_str(&format!("{}={value}; Path=/; Max-Age={max_age}; HttpOnly; SameSite=Strict{flags}", cookie_name(cfg))).expect("cookie header")
}

/// A Set-Cookie that removes the web session cookie.
pub(crate) fn clear_cookie(cfg: &Config) -> HeaderValue {
    set_cookie(cfg, "", 0)
}

fn session_secret(cfg: &Config, headers: &HeaderMap) -> Option<String> {
    crate::admin::cookie_value(headers, cookie_name(cfg))
}

/// The CSRF token of a web session: derived from its secret (nothing stored; unknowable
/// without the HttpOnly cookie), with a label of its own so it never equals the admin one.
pub fn csrf_for(secret: &str) -> String {
    util::base64url(&util::token_hash(&format!("freedomoffice web csrf\0{secret}")))
}

fn safe_method(m: &Method) -> bool {
    m == Method::GET || m == Method::HEAD
}

/// Why a cookie request is refused before it reaches a handler, or `None` when it may use the
/// session.
fn refusal(cfg: &Config, method: &Method, headers: &HeaderMap, secret: &str) -> Option<&'static str> {
    // Browsers say where a request comes from; only this origin (or the person, typing or
    // following a bookmark: "none") may use the session. Sibling subdomains are "same-site".
    if let Some(site) = headers.get("sec-fetch-site") {
        let site = site.to_str().unwrap_or("");
        if !site.eq_ignore_ascii_case("same-origin") && !site.eq_ignore_ascii_case("none") {
            return Some("cross_site");
        }
    }
    if !safe_method(method) {
        if !crate::admin::same_origin(cfg, headers) {
            return Some("cross_site");
        }
        let sent = headers.get(CSRF_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
        if !util::ct_eq(sent.as_bytes(), csrf_for(secret).as_bytes()) {
            return Some("csrf_failed");
        }
    }
    None
}

/// Router-wide: removes the internal header from every request, keeps the client address for
/// the audit log, and for `/api/*` requests that come with the web cookie and no
/// Authorization header records whether the cookie may be used (see the module docs).
pub async fn web_gate(State(app): State<App>, mut req: Request, next: Next) -> Response {
    req.headers_mut().remove(VERDICT);
    let ip = req.extensions().get::<ConnectInfo<Peer>>().map(|c| api::client_ip(app.cfg(), c.0 .0, req.headers()).to_string()).unwrap_or_default();
    let path = req.uri().path();
    let api_path = path.starts_with("/api/") && path != "/api/web/login";
    if api_path
        && !req.headers().contains_key(header::AUTHORIZATION)
        && let Some(secret) = session_secret(app.cfg(), req.headers())
    {
        let verdict = match refusal(app.cfg(), req.method(), req.headers(), &secret) {
            None => format!("ok {secret}"),
            Some(code) => format!("bad {code}"),
        };
        if let Ok(v) = HeaderValue::from_str(&verdict) {
            req.headers_mut().insert(VERDICT, v);
        }
    }
    REQUEST_IP.scope(ip, next.run(req)).await
}

/// What `web_gate` decided for this request: `Ok(Some(secret))` to use the session,
/// `Ok(None)` when there is no cookie, `Err` (403) when it was refused.
pub(crate) fn verdict(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let Some(v) = headers.get(VERDICT).and_then(|v| v.to_str().ok()) else { return Ok(None) };
    match v.split_once(' ') {
        Some(("ok", secret)) => Ok(Some(secret.to_string())),
        Some(("bad", "csrf_failed")) => Err(ApiError::coded(StatusCode::FORBIDDEN, "csrf_failed", "This request is missing its X-FO-CSRF token, or it is wrong. Reload the page.")),
        _ => Err(ApiError::coded(StatusCode::FORBIDDEN, "cross_site", "Requests from other sites can't use your sign-in.")),
    }
}

#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
}

fn session_json(user: &crate::store::User, secret: &str) -> serde_json::Value {
    json!({ "user": api::user_json(user), "csrf": csrf_for(secret) })
}

/// `POST /api/web/login {username, password}` → `{user, csrf}` and the session cookie.
pub(crate) async fn login(State(app): State<App>, ConnectInfo(Peer(peer)): ConnectInfo<Peer>, headers: HeaderMap, body: Bytes) -> ApiResult {
    // A form on another site can post text/plain, but not application/json without asking
    // first (CORS, which this server never allows); and the origin must be ours.
    let json_body = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/json"));
    if !crate::admin::same_origin(app.cfg(), &headers) || headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()).is_some_and(|s| !s.eq_ignore_ascii_case("same-origin") && !s.eq_ignore_ascii_case("none")) {
        tracing::warn!("refused a web sign-in sent from another site");
        return Err(ApiError::coded(StatusCode::FORBIDDEN, "cross_site", "Sign in from this server's own pages."));
    }
    if !json_body {
        return Err(ApiError::bad("Send the sign-in as application/json."));
    }
    let b: LoginBody = serde_json::from_slice(&body).map_err(|e| ApiError::bad(format!("Invalid request: {e}")))?;
    let ip: IpAddr = api::client_ip(app.cfg(), peer, &headers);
    let user = api::check_sign_in(&app, ip, &b.username, b.password, "web").await?;
    let agent: String = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let old = session_secret(app.cfg(), &headers);
    let u = user.clone();
    let secret = app
        .run(move |s| {
            // This browser's previous web session (if any) ends: the new cookie replaces it.
            if let Some(old) = old {
                s.end_session_by_secret(&old, SessionKind::Web)?;
            }
            Ok(s.create_session(&u.id, &agent, SessionKind::Web)?)
        })
        .await?;
    tracing::info!("user '{}' signed in on the web from {ip}", user.username);
    let mut res = axum::Json(session_json(&user, &secret)).into_response();
    res.headers_mut().insert(header::SET_COOKIE, set_cookie(app.cfg(), &secret, ADMIN_SESSION_SECS));
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

/// `GET /api/web/session` → `{user, csrf}`, or 401 without a (valid) web session. Also while
/// a temporary password is set (`user.must_change_password`).
pub(crate) async fn session(State(app): State<App>, headers: HeaderMap) -> ApiResult {
    let secret = verdict(&headers)?.ok_or_else(ApiError::unauthorized)?;
    app.run(move |s| {
        let (user, _) = s.web_session(&secret)?.ok_or_else(ApiError::unauthorized)?;
        let mut res = axum::Json(session_json(&user, &secret)).into_response();
        res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        Ok(res)
    })
    .await
}

/// `POST /api/web/logout` (with `X-FO-CSRF`): ends the session and clears the cookie. 204,
/// also when there was no session.
pub(crate) async fn logout(State(app): State<App>, headers: HeaderMap) -> ApiResult {
    if let Some(secret) = verdict(&headers)? {
        app.run_changing(move |s| {
            if let Some((u, sess)) = s.web_session(&secret)? {
                s.end_admin_session(sess.id)?;
                s.audit(crate::store::Audit::by(&u, "sign_out").target(&u.username).detail("web"))?;
            }
            Ok(())
        })
        .await?;
    }
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut().insert(header::SET_COOKIE, clear_cookie(app.cfg()));
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_and_csrf() {
        let cfg = Config { public_url: "https://files.example.com".into(), ..Config::default() };
        assert_eq!(set_cookie(&cfg, "abc", 60).to_str().unwrap(), "__Host-fo_web=abc; Path=/; Max-Age=60; HttpOnly; SameSite=Strict; Secure");
        let dev = Config { public_url: "http://127.0.0.1:8080".into(), ..Config::default() };
        assert_eq!(cookie_name(&dev), "fo_web");
        assert_ne!(csrf_for("a"), crate::admin::csrf_for("a"), "not the admin page's token");
    }

    #[test]
    fn refusals() {
        let cfg = Config { public_url: "https://files.example.com".into(), ..Config::default() };
        let mut h = HeaderMap::new();
        assert_eq!(refusal(&cfg, &Method::GET, &h, "s"), None);
        assert_eq!(refusal(&cfg, &Method::POST, &h, "s"), Some("csrf_failed"));
        h.insert(CSRF_HEADER, HeaderValue::from_str(&csrf_for("s")).unwrap());
        assert_eq!(refusal(&cfg, &Method::POST, &h, "s"), None);
        assert_eq!(refusal(&cfg, &Method::DELETE, &h, "other"), Some("csrf_failed"));
        h.insert(header::ORIGIN, HeaderValue::from_static("https://evil.test"));
        assert_eq!(refusal(&cfg, &Method::POST, &h, "s"), Some("cross_site"));
        h.insert(header::ORIGIN, HeaderValue::from_static("https://files.example.com"));
        assert_eq!(refusal(&cfg, &Method::PATCH, &h, "s"), None);
        h.insert("sec-fetch-site", HeaderValue::from_static("same-site"));
        assert_eq!(refusal(&cfg, &Method::GET, &h, "s"), Some("cross_site"), "a sibling subdomain");
        h.insert("sec-fetch-site", HeaderValue::from_static("none"));
        assert_eq!(refusal(&cfg, &Method::GET, &h, "s"), None, "typed or bookmarked");
    }
}
