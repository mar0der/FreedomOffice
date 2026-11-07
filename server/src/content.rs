//! File bytes over HTTP: downloads streamed from disk (single `Range` requests, safe
//! content types and headers) and uploads streamed to a staged file (size limit enforced
//! while they arrive, never buffered whole).

use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};

use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::api::ApiError;
use crate::util;

/// The content type for a file name, from a short list of well-known extensions; anything
/// else is `application/octet-stream` (which browsers download, never render).
pub fn content_type(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "txt" | "log" | "md" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "json" => "application/json",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "html" | "htm" => "text/html; charset=utf-8",
        "xml" => "application/xml",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "ogg" | "oga" => "audio/ogg",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "zip" => "application/zip",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "xls" => "application/vnd.ms-excel",
        "doc" => "application/msword",
        "ppt" => "application/vnd.ms-powerpoint",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odt" => "application/vnd.oasis.opendocument.text",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        _ => "application/octet-stream",
    }
}

/// The Content-Security-Policy of a download. Everything but PDF (whose viewers refuse to
/// run sandboxed) is sandboxed: a shared HTML or SVG file renders without script, forms or
/// plugins in an opaque origin, never as this server's origin.
fn csp_for(ct: &str) -> &'static str {
    if ct == "application/pdf" {
        "frame-ancestors 'self'"
    } else {
        "sandbox; default-src 'none'; img-src data:; style-src 'unsafe-inline'; media-src 'self'; frame-ancestors 'self'"
    }
}

/// `inline` or `attachment` with the name both as an ASCII fallback (`filename="..."`, other
/// characters replaced by `_`) and in full (`filename*=UTF-8''<percent-encoded>`).
pub fn disposition(name: &str, attachment: bool) -> HeaderValue {
    let ascii: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || " .-_()[]+,".contains(c) { c } else { '_' }).collect();
    let ascii = if ascii.trim().is_empty() { "download".to_string() } else { ascii };
    let encoded: String = name.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    let kind = if attachment { "attachment" } else { "inline" };
    HeaderValue::from_str(&format!("{kind}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")).unwrap_or(HeaderValue::from_static("attachment"))
}

/// What a `Range` header asks of a file of `len` bytes.
#[derive(Debug, PartialEq, Eq)]
pub enum Range {
    /// The whole file (no header, an unsupported or malformed one, several ranges).
    Full,
    /// Bytes `start..=end`.
    Part(u64, u64),
    /// 416.
    Unsatisfiable,
}

/// One `bytes=` range (`a-b`, `a-`, `-n`); anything else means the whole file.
pub fn parse_range(h: Option<&str>, len: u64) -> Range {
    let Some(h) = h else { return Range::Full };
    let Some((unit, spec)) = h.trim().split_once('=') else { return Range::Full };
    if !unit.trim().eq_ignore_ascii_case("bytes") || spec.contains(',') {
        return Range::Full;
    }
    let Some((a, b)) = spec.trim().split_once('-') else { return Range::Full };
    let num = |s: &str| if !s.is_empty() && s.len() <= 19 && s.bytes().all(|c| c.is_ascii_digit()) { s.parse::<u64>().ok() } else { None };
    let (a, b) = (a.trim(), b.trim());
    match (a.is_empty(), b.is_empty()) {
        // "-n": the last n bytes.
        (true, false) => match num(b) {
            Some(0) => Range::Unsatisfiable,
            Some(_) if len == 0 => Range::Unsatisfiable,
            Some(n) => Range::Part(len - n.min(len), len - 1),
            None => Range::Full,
        },
        (false, _) => {
            let Some(start) = num(a) else { return Range::Full };
            let end = if b.is_empty() {
                None
            } else {
                match num(b) {
                    Some(e) if e >= start => Some(e),
                    _ => return Range::Full,
                }
            };
            if start >= len {
                return Range::Unsatisfiable;
            }
            Range::Part(start, end.unwrap_or(len - 1).min(len - 1))
        }
        (true, true) => Range::Full,
    }
}

/// `count` bytes of `file` from its current position, read in 64 KiB pieces as the client
/// takes them.
fn stream_body(file: std::fs::File, count: u64) -> Body {
    let file = tokio::fs::File::from_std(file);
    let s = futures_util::stream::unfold((file, count), |(mut f, left)| async move {
        if left == 0 {
            return None;
        }
        let mut buf = vec![0u8; left.min(64 * 1024) as usize];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<Bytes, std::io::Error>(Bytes::from(buf)), (f, left - n as u64)))
            }
            Err(e) => Some((Err(e), (f, 0))),
        }
    });
    Body::from_stream(s)
}

/// A file download: the bytes of `file` (`len` long) with safe headers, honouring a single
/// `Range` (206; 416 when outside the file; `If-Range` with another ETag sends it all).
/// `attachment` asks the browser to save rather than show it.
pub fn file_response(req: &HeaderMap, mut file: std::fs::File, len: u64, name: &str, etag: &str, attachment: bool) -> Result<Response, ApiError> {
    let quoted = format!("\"{etag}\"");
    let if_range_ok = req.get(header::IF_RANGE).and_then(|v| v.to_str().ok()).is_none_or(|v| v.trim() == quoted);
    let range = if if_range_ok { parse_range(req.get(header::RANGE).and_then(|v| v.to_str().ok()), len) } else { Range::Full };
    let ct = content_type(name);
    let mut res = match range {
        Range::Unsatisfiable => {
            let mut r = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{len}")) {
                r.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            r
        }
        Range::Full => {
            let mut r = Response::new(stream_body(file, len));
            r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(len));
            r
        }
        Range::Part(start, end) => {
            file.seek(SeekFrom::Start(start)).map_err(|_| ApiError::internal())?;
            let mut r = Response::new(stream_body(file, end - start + 1));
            *r.status_mut() = StatusCode::PARTIAL_CONTENT;
            let h = r.headers_mut();
            h.insert(header::CONTENT_LENGTH, HeaderValue::from(end - start + 1));
            if let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{end}/{len}")) {
                h.insert(header::CONTENT_RANGE, v);
            }
            r
        }
    };
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(header::CONTENT_DISPOSITION, disposition(name, attachment));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("content-security-policy", HeaderValue::from_static(csp_for(ct)));
    // Other sites can't embed these bytes (an <img> or <script> with the visitor's cookie).
    h.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(&quoted) {
        h.insert(header::ETAG, v);
    }
    Ok(res)
}

/// An upload staged on disk: its path (removed when this is dropped, unless it was moved
/// into place), size and SHA-256 (hex).
pub struct Staged {
    pub path: PathBuf,
    pub size: i64,
    pub hash: String,
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn too_large(limit: usize) -> ApiError {
    ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, format!("The file is too large (at most {} MB).", limit / (1024 * 1024)))
}

/// Streams a request body into a new file in `dir` (hashing as it goes), refusing it with 413
/// as soon as it is longer than `limit` (or says so in Content-Length). The file is synced
/// before this returns.
pub async fn stage(dir: &Path, headers: &HeaderMap, body: Body, limits: &UploadLimits) -> Result<Staged, ApiError> {
    let limit = limits.max;
    if headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok()).is_some_and(|n| n > limit as u64) {
        return Err(too_large(limit));
    }
    tokio::fs::create_dir_all(dir).await.map_err(|e| io_error("create the upload folder", e))?;
    // The upload may take up to `limit`; afterwards at least `reserve` must still be free.
    if let Some(free) = free_bytes(dir)
        && !enough_space(free, limit as u64, limits.reserve)
    {
        tracing::error!("refused an upload: only {} MB free on the data disk", free / (1024 * 1024));
        return Err(ApiError::new(StatusCode::INSUFFICIENT_STORAGE, "The server is running out of disk space. Ask your administrator."));
    }
    let mut staged = Staged { path: dir.join(format!("{}.upload", util::new_id())), size: 0, hash: String::new() };
    let mut f = tokio::fs::File::create(&staged.path).await.map_err(|e| io_error("create an upload file", e))?;
    let mut hasher = Sha256::new();
    let mut size: u64 = 0;
    let mut stream = body.into_data_stream();
    loop {
        // A client that stops sending: dropped (the staged file goes with `staged`).
        let Ok(next) = tokio::time::timeout(limits.idle, stream.next()).await else {
            return Err(ApiError::new(StatusCode::REQUEST_TIMEOUT, "The upload stalled and was stopped."));
        };
        let Some(chunk) = next else { break };
        // A client that stops or breaks the connection mid-way: nothing is stored.
        let chunk = chunk.map_err(|_| ApiError::bad("The upload was interrupted."))?;
        size += chunk.len() as u64;
        if size > limit as u64 {
            return Err(too_large(limit));
        }
        hasher.update(&chunk);
        f.write_all(&chunk).await.map_err(|e| io_error("write an upload", e))?;
    }
    f.flush().await.map_err(|e| io_error("write an upload", e))?;
    f.sync_all().await.map_err(|e| io_error("sync an upload", e))?;
    drop(f);
    staged.size = size as i64;
    staged.hash = util::hex(&hasher.finalize());
    Ok(staged)
}

/// The limits of one upload (from the configuration).
pub struct UploadLimits {
    /// Bytes at most (`max_upload_mb`).
    pub max: usize,
    /// No data for this long: stop.
    pub idle: std::time::Duration,
    /// Bytes that must stay free on the data disk after the largest possible upload.
    pub reserve: u64,
}

/// Whether an upload of up to `max` bytes leaves `reserve` free.
pub fn enough_space(free: u64, max: u64, reserve: u64) -> bool {
    free >= max.saturating_add(reserve)
}

/// Free bytes for unprivileged writers on the file system of `dir` (`None` when unknown).
#[cfg(unix)]
fn free_bytes(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `st` a properly sized out-parameter.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

#[cfg(not(unix))]
fn free_bytes(_: &Path) -> Option<u64> {
    None
}

fn io_error(what: &str, e: std::io::Error) -> ApiError {
    tracing::error!("couldn't {what}: {e}");
    ApiError::internal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range(None, 10), Range::Full);
        assert_eq!(parse_range(Some("bytes=0-4"), 10), Range::Part(0, 4));
        assert_eq!(parse_range(Some("bytes=5-"), 10), Range::Part(5, 9));
        assert_eq!(parse_range(Some("bytes=5-100"), 10), Range::Part(5, 9));
        assert_eq!(parse_range(Some("bytes=-3"), 10), Range::Part(7, 9));
        assert_eq!(parse_range(Some("bytes=-30"), 10), Range::Part(0, 9));
        assert_eq!(parse_range(Some("bytes=10-"), 10), Range::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=-0"), 10), Range::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=0-"), 0), Range::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=4-2"), 10), Range::Full, "invalid: ignored");
        assert_eq!(parse_range(Some("bytes=0-1,4-5"), 10), Range::Full, "several ranges: all of it");
        assert_eq!(parse_range(Some("items=0-1"), 10), Range::Full);
        assert_eq!(parse_range(Some("bytes=a-b"), 10), Range::Full);
        assert_eq!(parse_range(Some("bytes=99999999999999999999-"), 10), Range::Full);
    }

    #[test]
    fn disk_space() {
        let gb = 1024 * 1024 * 1024;
        assert!(enough_space(2 * gb, 100 << 20, gb));
        assert!(!enough_space(gb, 100 << 20, gb), "the upload would eat into the reserve");
        assert!(!enough_space(0, 0, 1));
        assert!(enough_space(u64::MAX, u64::MAX, u64::MAX), "no overflow");
        assert!(free_bytes(&std::env::temp_dir()).is_some_and(|f| f > 0));
    }

    #[test]
    fn types_and_names() {
        assert_eq!(content_type("Book.XLSX"), "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet");
        assert_eq!(content_type("page.html"), "text/html; charset=utf-8");
        assert_eq!(content_type("a.exe"), "application/octet-stream");
        assert_eq!(content_type("noext"), "application/octet-stream");
        assert!(csp_for(content_type("x.svg")).starts_with("sandbox"));
        assert!(csp_for(content_type("x.html")).starts_with("sandbox"));
        assert!(!csp_for(content_type("x.pdf")).contains("sandbox"));
        assert_eq!(disposition("Q3 report.xlsx", true).to_str().unwrap(), "attachment; filename=\"Q3 report.xlsx\"; filename*=UTF-8''Q3%20report.xlsx");
        assert_eq!(disposition("Smörgås;\".txt", false).to_str().unwrap(), "inline; filename=\"Sm_rg_s__.txt\"; filename*=UTF-8''Sm%C3%B6rg%C3%A5s%3B%22.txt");
    }
}
