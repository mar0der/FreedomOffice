//! FreedomOffice Drive: the web app served by freedomoffice-server at `/drive`.
//!
//! Plain static files (`drive/web/`), embedded at build time: no framework, no build step, no
//! external scripts, fonts or CDNs, no inline scripts or styles (the server's CSP forbids
//! them). `index.html` refers to the other files as `/drive/assets/<file>?v=<version>`, where
//! the version is a hash of their contents, so the server can let browsers cache them for good.

use std::sync::OnceLock;

const INDEX: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLE: &str = include_str!("../web/style.css");
const ICON: &str = include_str!("../web/icon.svg");
const SHEET: &str = include_str!("../web/sheet.html");
const SHEET_JS: &str = include_str!("../web/sheet.js");
const SHEET_CSS: &str = include_str!("../web/sheet.css");

/// The files under `/drive/assets/`, with their content types (index.html is served at `/drive`).
const ASSETS: &[(&str, &str, &str)] = &[
    ("app.js", APP_JS, "text/javascript; charset=utf-8"),
    ("style.css", STYLE, "text/css; charset=utf-8"),
    ("icon.svg", ICON, "image/svg+xml"),
    ("sheet.js", SHEET_JS, "text/javascript; charset=utf-8"),
    ("sheet.css", SHEET_CSS, "text/css; charset=utf-8"),
];

/// A short hash of every asset (FNV-1a, 64 bit): changes whenever any of them does.
pub fn version() -> &'static str {
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for (name, body, _) in ASSETS {
            for b in name.bytes().chain([0]).chain(body.bytes()).chain([0]) {
                h ^= b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        format!("{h:016x}")[..12].to_string()
    })
}

/// `index.html` with the asset version filled in.
fn index() -> &'static [u8] {
    static I: OnceLock<String> = OnceLock::new();
    I.get_or_init(|| INDEX.replace("{{v}}", version())).as_bytes()
}

/// `sheet.html` (web Sheets, served at `/drive/sheet/<id>`) with the asset version filled in.
fn sheet_page() -> &'static [u8] {
    static I: OnceLock<String> = OnceLock::new();
    I.get_or_init(|| SHEET.replace("{{v}}", version())).as_bytes()
}

/// The pages: Drive and web Sheets. They are not served under `/drive/assets/`.
pub const PAGES: &[&str] = &["index.html", "sheet.html"];

/// The embedded file at `path` ("index.html", "sheet.html", "app.js", "style.css", "sheet.js",
/// "sheet.css", "icon.svg") and its content type, or `None`.
pub fn asset(path: &str) -> Option<(&'static [u8], &'static str)> {
    if path == "index.html" {
        return Some((index(), "text/html; charset=utf-8"));
    }
    if path == "sheet.html" {
        return Some((sheet_page(), "text/html; charset=utf-8"));
    }
    ASSETS.iter().find(|(n, _, _)| *n == path).map(|(_, body, ct)| (body.as_bytes(), *ct))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assets_are_there() {
        for p in ["index.html", "sheet.html", "app.js", "style.css", "sheet.js", "sheet.css", "icon.svg"] {
            let (body, ct) = asset(p).unwrap_or_else(|| panic!("{p}"));
            assert!(!body.is_empty() && !ct.is_empty(), "{p}");
        }
        assert!(asset("../Cargo.toml").is_none());
        assert!(asset("").is_none());
        assert!(asset("APP.JS").is_none());
        let index = std::str::from_utf8(asset("index.html").unwrap().0).unwrap();
        assert!(!index.contains("{{v}}"));
        assert!(index.contains(&format!("/drive/assets/app.js?v={}", version())));
        assert!(index.contains(&format!("/drive/assets/style.css?v={}", version())));
        assert_eq!(version().len(), 12);
    }

    /// Every `<tag ...>` in an HTML text (crude, but the file is ours and simple).
    fn tags(html: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = html;
        while let Some(i) = rest.find('<') {
            let after = &rest[i + 1..];
            let end = after.find('>').expect("unclosed tag");
            out.push(after[..end].to_string());
            rest = &after[end + 1..];
        }
        out
    }

    /// The CSP (`script-src 'self'; style-src 'self'`) would block these anyway; this keeps
    /// them out of the source so nothing silently breaks or tempts a looser policy.
    #[test]
    fn html_has_no_inline_script_or_style() {
        for t in tags(INDEX).into_iter().chain(tags(SHEET)) {
            let lower = t.to_ascii_lowercase();
            if lower.starts_with("script") {
                assert!(lower.contains(" src="), "inline <script> in index.html: <{t}>");
            }
            assert!(!lower.starts_with("style"), "<style> element in index.html");
            for w in lower.split(char::is_whitespace).skip(1) {
                assert!(!w.starts_with("style="), "style attribute in index.html: <{t}>");
                assert!(!(w.starts_with("on") && w.contains('=')), "event handler attribute in index.html: <{t}>");
            }
            assert!(!lower.contains("javascript:"), "javascript: URL in index.html: <{t}>");
            // Resources and links: only this server's own paths (or in-page hashes).
            for attr in ["src=\"", "href=\""] {
                if let Some(i) = lower.find(attr) {
                    let v = &lower[i + attr.len()..];
                    assert!(v.starts_with("/drive") || v.starts_with("/admin") || v.starts_with('#'), "external reference in index.html: <{t}>");
                }
            }
        }
    }

    #[test]
    fn js_builds_the_page_without_html_strings() {
        // Data is always rendered with textContent / createTextNode, never parsed as HTML.
        for bad in [
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
            "eval(",
            "new Function",
            "setAttribute(\"on",
            "setAttribute('on",
            ".style.",
            "style=",
            "javascript:",
            "localStorage",
            "createContextualFragment",
            "DOMParser",
            "srcdoc",
        ] {
            assert!(!APP_JS.contains(bad), "app.js contains `{bad}`");
        }
        // sheet.js draws on a canvas and sizes two elements through CSSOM (allowed by the CSP,
        // unlike style attributes), so `.style.` is fine there; the rest applies.
        for bad in ["innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "eval(", "new Function", "setAttribute(\"on", "setAttribute('on", "setAttribute('style", "setAttribute(\"style", "style=", "javascript:", "localStorage", "sessionStorage", "createContextualFragment", "DOMParser", "srcdoc", "XMLHttpRequest", "importScripts"] {
            assert!(!SHEET_JS.contains(bad), "sheet.js contains `{bad}`");
        }
        for (n, line) in SHEET_JS.lines().enumerate() {
            for bad in ["http://", "https://", "//cdn", "fonts.googleapis"] {
                assert!(!line.contains(bad), "sheet.js line {} refers to another origin: {line}", n + 1);
            }
        }
        // No other origins: requests, links and resources are relative to this server (the
        // one marked exception is the FreedomSheets download page, a plain link).
        for (n, line) in APP_JS.lines().enumerate() {
            for bad in ["http://", "https://", "//cdn", "fonts.googleapis"] {
                assert!(!line.contains(bad) || line.contains("EXTERNAL-LINK-OK"), "app.js line {} refers to another origin: {line}", n + 1);
            }
        }
        for bad in ["@import", "url(http", "url(//", "expression("] {
            assert!(!STYLE.contains(bad), "style.css contains `{bad}`");
        }
    }

    #[test]
    fn the_icon_is_plain_svg() {
        let lower = ICON.to_ascii_lowercase();
        for bad in ["<script", "onload", "onerror", "href=", "<foreignobject", "<style"] {
            assert!(!lower.contains(bad), "icon.svg contains `{bad}`");
        }
    }
}
