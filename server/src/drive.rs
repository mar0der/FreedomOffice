//! Serves FreedomOffice Drive, the web app (crate `freedomoffice-drive`), from this origin:
//! `/drive` and `/drive/` answer its page, `/drive/assets/<file>` its script, style and icon.
//! The app talks to the JSON API with the browser session (`web.rs`).
//!
//! Every answer carries a strict CSP (scripts and styles only from this origin, no inline
//! ones, requests only to this origin, no framing), `nosniff`, `Referrer-Policy: same-origin`
//! (not `no-referrer`: the API's origin check needs `Origin` on POSTs) and `X-Frame-Options:
//! DENY`. The page is `no-store` (it names the asset version); an asset requested with the
//! current `?v=` is cacheable for a year, otherwise for 5 minutes.

use axum::extract::{Path, RawQuery};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

use crate::api::App;

/// The Content-Security-Policy of every `/drive` answer.
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

pub fn routes() -> Router<App> {
    Router::new().route("/drive", get(index)).route("/drive/", get(index)).route("/drive/assets/{*path}", get(asset))
}

fn respond(status: StatusCode, body: &'static [u8], content_type: &'static str, cache: &'static str) -> Response {
    let mut res = (status, body).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert("content-security-policy", HeaderValue::from_static(CSP));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
    res
}

async fn index() -> Response {
    let (body, ct) = freedomoffice_drive::asset("index.html").expect("index.html is embedded");
    respond(StatusCode::OK, body, ct, "no-store")
}

async fn asset(Path(path): Path<String>, RawQuery(query): RawQuery) -> Response {
    // The page itself only at /drive (one address, one set of caching rules).
    let found = if path == "index.html" { None } else { freedomoffice_drive::asset(&path) };
    let Some((body, ct)) = found else {
        return respond(StatusCode::NOT_FOUND, b"Not found\n", "text/plain; charset=utf-8", "no-store");
    };
    let current = query.as_deref().is_some_and(|q| q.split('&').any(|kv| kv.strip_prefix("v=") == Some(freedomoffice_drive::version())));
    let cache = if current { "public, max-age=31536000, immutable" } else { "public, max-age=300" };
    respond(StatusCode::OK, body, ct, cache)
}
