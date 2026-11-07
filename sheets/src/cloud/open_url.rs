//! "Open in Sheets" links from the server's web page:
//! `freedomoffice://open?server=<url-encoded base url>&item=<id>`.
//!
//! The link comes from a web page, so it is checked strictly here and only the server address
//! and the item id are ever used. Nothing is sent anywhere by parsing; the app opens the item
//! only with an account already signed in to that server (otherwise it prefills the Add server
//! form on File > Account). Also: registering the `freedomoffice:` scheme on Windows.

/// A checked "Open in Sheets" link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenRequest {
    /// Base URL as `server::normalize_url` gives it (no trailing slash).
    pub server: String,
    /// Server item id (32 lower-case hex digits).
    pub item: String,
}

pub const SCHEME: &str = "freedomoffice";

const MAX_LINK: usize = 4096;
const MAX_SERVER: usize = 2048;

/// The first argument looks like a link rather than a file path.
pub fn is_open_url(arg: &str) -> bool {
    let b = arg.as_bytes();
    b.len() > SCHEME.len() && b[SCHEME.len()] == b':' && b[..SCHEME.len()].eq_ignore_ascii_case(SCHEME.as_bytes())
}

/// Parses and checks a `freedomoffice://open?server=…&item=…` link.
pub fn parse(link: &str) -> Result<OpenRequest, String> {
    const BAD: &str = "This isn't a valid FreedomOffice link.";
    if link.len() > MAX_LINK || link.chars().any(|c| c.is_whitespace() || c.is_control()) || !link.is_ascii() {
        return Err(BAD.into());
    }
    let (scheme, rest) = link.split_once(':').ok_or(BAD)?;
    if !scheme.eq_ignore_ascii_case(SCHEME) {
        return Err(BAD.into());
    }
    let rest = rest.strip_prefix("//").ok_or(BAD)?;
    if rest.contains('#') {
        return Err(BAD.into());
    }
    let (target, query) = rest.split_once('?').ok_or(BAD)?;
    if !(target.eq_ignore_ascii_case("open") || target.eq_ignore_ascii_case("open/")) {
        return Err(BAD.into());
    }
    let (mut server, mut item) = (None, None);
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').ok_or(BAD)?;
        let slot = match k {
            "server" => &mut server,
            "item" => &mut item,
            _ => return Err(BAD.into()),
        };
        if slot.is_some() {
            return Err(BAD.into());
        }
        *slot = Some(pct_decode_strict(v).ok_or(BAD)?);
    }
    let (Some(server), Some(item)) = (server, item) else { return Err(BAD.into()) };
    if !valid_item_id(&item) {
        return Err("The link doesn't name a valid file.".into());
    }
    let server = check_server(&server).ok_or("The link doesn't name a valid FreedomOffice server address.")?;
    Ok(OpenRequest { server, item })
}

/// The server's id format (`util::valid_id` on the server).
pub fn valid_item_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Percent-decoding that refuses bad escapes, non-UTF-8 and control characters. `+` is a space
/// (form encoding), which no valid value contains.
fn pct_decode_strict(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hex = b.get(i + 1..i + 3)?;
                if !hex.iter().all(u8::is_ascii_hexdigit) {
                    return None;
                }
                out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    let s = String::from_utf8(out).ok()?;
    (!s.chars().any(char::is_control)).then_some(s)
}

/// `https://host[:port][/path]`, or `http://localhost|127.0.0.1[:port][/path]`. Returns the
/// normalised base URL (lower-case scheme and host, no trailing slash).
fn check_server(s: &str) -> Option<String> {
    if s.is_empty() || s.len() > MAX_SERVER || !s.is_ascii() {
        return None;
    }
    let (scheme, rest) = s.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    let host = host.to_ascii_lowercase();
    if !valid_host(&host) {
        return None;
    }
    if let Some(p) = port
        && (p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) || !(1..=65535).contains(&p.parse::<u32>().ok()?))
    {
        return None;
    }
    match scheme.as_str() {
        "https" => {}
        "http" if host == "localhost" || host == "127.0.0.1" => {}
        _ => return None,
    }
    // An optional path (a reverse proxy prefix): plain segments only.
    let path = path.trim_end_matches('/');
    if !path.is_empty() {
        let segs = path.strip_prefix('/')?.split('/');
        for seg in segs {
            if seg.is_empty() || seg == "." || seg == ".." || !seg.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b)) {
                return None;
            }
        }
    }
    let authority = match port {
        Some(p) => format!("{host}:{p}"),
        None => host,
    };
    let url = format!("{scheme}://{authority}{path}");
    // Same rules as an address typed into the Add server form.
    super::server::normalize_url(&url).ok().filter(|u| *u == url)
}

/// A DNS name or dotted IPv4 address: labels of [a-z0-9-], not starting or ending with "-".
fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.split('.').all(|l| {
            !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// A server base URL compared by scheme, host and port (case-insensitive; default ports
/// dropped) and path (no trailing slash): which signed-in account a link belongs to.
pub fn server_key(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_ascii_lowercase() };
    let scheme = scheme.to_ascii_lowercase();
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let mut authority = authority.to_ascii_lowercase();
    let default_port = if scheme == "https" { ":443" } else if scheme == "http" { ":80" } else { "" };
    if !default_port.is_empty()
        && let Some(a) = authority.strip_suffix(default_port)
    {
        authority = a.to_string();
    }
    format!("{scheme}://{authority}{path}")
}

pub fn same_server(a: &str, b: &str) -> bool {
    server_key(a) == server_key(b)
}

// ---------------------------------------------------------------------------
// Host names as people read them (punycode, RFC 3492)
// ---------------------------------------------------------------------------

/// A server's host name for a confirmation: as read (`xn--` labels decoded) and as sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostName {
    /// Decoded form (same as `raw` when there's nothing to decode or decoding failed).
    pub shown: String,
    /// The ASCII form, with the port.
    pub raw: String,
    /// An `xn--` label couldn't be decoded: the name may imitate another one.
    pub undecodable: bool,
}

/// The host (and port) of a base URL, `xn--` labels decoded.
pub fn host_name(url: &str) -> HostName {
    let raw = super::server::host_of(url).to_ascii_lowercase();
    let (host, port) = match raw.rsplit_once(':') {
        Some((h, p)) if p.bytes().all(|b| b.is_ascii_digit()) => (h, Some(p)),
        _ => (raw.as_str(), None),
    };
    let mut undecodable = false;
    let labels: Vec<String> = host
        .split('.')
        .map(|l| match l.strip_prefix("xn--") {
            Some(enc) => punycode_decode(enc).unwrap_or_else(|| {
                undecodable = true;
                l.to_string()
            }),
            None => l.to_string(),
        })
        .collect();
    let mut shown = if undecodable { host.to_string() } else { labels.join(".") };
    if let Some(p) = port {
        shown = format!("{shown}:{p}");
    }
    HostName { shown, raw, undecodable }
}

/// Decodes the part of an IDNA label after `xn--`. `None` for anything malformed, overflowing,
/// with control characters, or that decodes to plain ASCII (never a valid `xn--` label).
pub fn punycode_decode(input: &str) -> Option<String> {
    const BASE: u32 = 36;
    const TMIN: u32 = 1;
    const TMAX: u32 = 26;
    const SKEW: u32 = 38;
    const DAMP: u32 = 700;
    fn adapt(delta: u32, points: u32, first: bool) -> u32 {
        let mut delta = if first { delta / DAMP } else { delta / 2 };
        delta += delta / points;
        let mut k = 0;
        while delta > ((BASE - TMIN) * TMAX) / 2 {
            delta /= BASE - TMIN;
            k += BASE;
        }
        k + (BASE - TMIN + 1) * delta / (delta + SKEW)
    }
    if input.is_empty() || input.len() > 63 || !input.is_ascii() {
        return None;
    }
    let (basic, digits) = match input.rfind('-') {
        Some(i) => (&input[..i], &input[i + 1..]),
        None => ("", input),
    };
    let mut out: Vec<char> = basic.chars().collect();
    let (mut n, mut i, mut bias) = (128u32, 0u32, 72u32);
    let mut it = digits.bytes().peekable();
    while it.peek().is_some() {
        let old = i;
        let mut w = 1u32;
        let mut k = BASE;
        loop {
            let d = match it.next()? {
                c @ b'a'..=b'z' => (c - b'a') as u32,
                c @ b'A'..=b'Z' => (c - b'A') as u32,
                c @ b'0'..=b'9' => (c - b'0') as u32 + 26,
                _ => return None,
            };
            i = i.checked_add(d.checked_mul(w)?)?;
            let t = if k <= bias { TMIN } else if k >= bias + TMAX { TMAX } else { k - bias };
            if d < t {
                break;
            }
            w = w.checked_mul(BASE - t)?;
            k += BASE;
        }
        let len = out.len() as u32 + 1;
        bias = adapt(i - old, len, old == 0);
        n = n.checked_add(i / len)?;
        i %= len;
        out.insert(i as usize, char::from_u32(n)?);
        i += 1;
    }
    let s: String = out.into_iter().collect();
    (!s.is_ascii() && !s.chars().any(char::is_control)).then_some(s)
}

// ---------------------------------------------------------------------------
// Windows: HKCU\Software\Classes\freedomoffice
// ---------------------------------------------------------------------------

/// The `shell\open\command` this exe registers.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn handler_command(exe: &str) -> String {
    format!("\"{exe}\" \"%1\"")
}

/// The program a `shell\open\command` runs (quoted or up to the first space).
#[cfg_attr(not(windows), allow(dead_code))]
fn command_program(cmd: &str) -> &str {
    let cmd = cmd.trim();
    if let Some(r) = cmd.strip_prefix('"') {
        r.split('"').next().unwrap_or("")
    } else {
        cmd.split_whitespace().next().unwrap_or("")
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
fn same_path(a: &str, b: &str) -> bool {
    let n = |s: &str| s.trim().trim_matches('"').replace('/', "\\").to_lowercase();
    !a.trim().is_empty() && n(a) == n(b)
}

/// The programs a `shell\open\command` may run: the quoted path, or for an unquoted command
/// every prefix ending at a space, with and without ".exe" (Windows tries them in turn), up to
/// the first argument (`%1`, `"%1"`).
#[cfg_attr(not(windows), allow(dead_code))]
fn command_candidates(cmd: &str) -> Vec<String> {
    let cmd = cmd.trim();
    if cmd.starts_with('"') {
        return vec![command_program(cmd).to_string()];
    }
    let mut out = Vec::new();
    let mut end = 0;
    for (n, tok) in cmd.split(' ').enumerate() {
        if n > 0 && (tok.starts_with('%') || tok.starts_with('"')) {
            break;
        }
        end += usize::from(n > 0) + tok.len();
        if tok.is_empty() {
            continue;
        }
        let p = &cmd[..end];
        out.push(p.to_string());
        if !p.to_ascii_lowercase().ends_with(".exe") {
            out.push(format!("{p}.exe"));
        }
    }
    out
}

/// `C:\...`, `C:/...` or `\\server\...` (relative names are looked up on PATH).
#[cfg_attr(not(windows), allow(dead_code))]
fn absolute_windows_path(p: &str) -> bool {
    let b = p.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')) || p.starts_with("\\\\")
}

/// The registered program is gone: every program the command may run is an absolute path
/// (no environment variables) that `exists` says isn't there.
#[cfg_attr(not(windows), allow(dead_code))]
fn handler_missing(cmd: &str, exists: &dyn Fn(&str) -> bool) -> bool {
    let c = command_candidates(cmd);
    !c.is_empty() && c.iter().all(|p| !p.is_empty() && !p.contains('%') && absolute_windows_path(p) && !exists(p))
}

/// Whether to (re)write the scheme registration: only when no handler is registered, the
/// registered one is this same program (moved files re-register), or the registered program no
/// longer exists (uninstalled). Another installed handler is never taken over. `existing`:
/// `None` when the key is missing, else its command (`None` when it has none or it can't be
/// read). `exists`: whether a file is there (the disk on Windows; a stand-in in tests).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn should_register(existing: Option<Option<&str>>, exe: &str, exists: &dyn Fn(&str) -> bool) -> bool {
    match existing {
        None => true,
        Some(Some(cmd)) => same_path(command_program(cmd), exe) || handler_missing(cmd, exists),
        Some(None) => false,
    }
}

/// Registers `freedomoffice:` links for the current user (cheap; done at every start).
#[cfg(windows)]
pub fn register_windows_handler() {
    use windows_sys::Win32::System::Registry::{RegCloseKey, RegGetValueW, RegOpenKeyExW, RegSetKeyValueW, HKEY, HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, KEY_READ, REG_SZ, RRF_RT_REG_SZ};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(exe) = exe.to_str().map(str::to_string) else { return };
    if exe.contains('"') {
        return;
    }
    // Read the merged view (HKCR): a machine-wide handler of another program counts too.
    let existing = unsafe {
        let mut hk: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CLASSES_ROOT, wide(SCHEME).as_ptr(), 0, KEY_READ, &mut hk) != 0 {
            None
        } else {
            RegCloseKey(hk);
            let sub = wide(&format!("{SCHEME}\\shell\\open\\command"));
            let mut size = 0u32;
            let rc = RegGetValueW(HKEY_CLASSES_ROOT, sub.as_ptr(), std::ptr::null(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut size);
            if rc != 0 || size < 2 || size > 64 * 1024 {
                Some(None)
            } else {
                let mut buf = vec![0u16; (size as usize).div_ceil(2)];
                let rc = RegGetValueW(HKEY_CLASSES_ROOT, sub.as_ptr(), std::ptr::null(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut size);
                if rc != 0 {
                    Some(None)
                } else {
                    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                    Some(Some(String::from_utf16_lossy(&buf[..end])))
                }
            }
        }
    };
    if !should_register(existing.as_ref().map(|o| o.as_deref()), &exe, &|p| std::path::Path::new(p).is_file()) {
        return;
    }
    let base = format!("Software\\Classes\\{SCHEME}");
    let values = [
        (base.clone(), None, "URL:FreedomOffice".to_string()),
        (base.clone(), Some("URL Protocol"), String::new()),
        (format!("{base}\\DefaultIcon"), None, format!("\"{exe}\",0")),
        (format!("{base}\\shell\\open\\command"), None, handler_command(&exe)),
    ];
    for (key, name, data) in values {
        let key = wide(&key);
        let name = name.map(wide);
        let data = wide(&data);
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789abcdef0123456789abcdef";

    fn link(server: &str, item: &str) -> String {
        format!("freedomoffice://open?server={}&item={item}", crate::cloud::pct_encode(server))
    }

    #[test]
    fn good_links() {
        let ok = |l: &str| parse(l).unwrap_or_else(|e| panic!("{l}: {e}"));
        assert_eq!(ok(&link("https://files.example.com", ID)), OpenRequest { server: "https://files.example.com".into(), item: ID.into() });
        assert_eq!(ok(&link("https://Files.Example.COM/", ID)).server, "https://files.example.com");
        assert_eq!(ok(&link("https://files.example.com:8443/office/", ID)).server, "https://files.example.com:8443/office");
        assert_eq!(ok(&link("http://127.0.0.1:8080", ID)).server, "http://127.0.0.1:8080");
        assert_eq!(ok(&link("http://localhost", ID)).server, "http://localhost");
        assert_eq!(ok(&link("HTTPS://10.0.0.5", ID)).server, "https://10.0.0.5");
        // Scheme and target are case-insensitive; "open/" and either parameter order work.
        assert_eq!(ok(&format!("FreedomOffice://OPEN/?item={ID}&server=https%3A%2F%2Ffiles.example.com")).server, "https://files.example.com");
        // Unencoded (but valid) server address.
        assert_eq!(ok(&format!("freedomoffice://open?server=https://f.example.com/a&item={ID}")).server, "https://f.example.com/a");
        assert!(is_open_url("FREEDOMOFFICE://open") && is_open_url("freedomoffice:x"));
        assert!(!is_open_url("freedomoffic\u{e9}:x") && !is_open_url("freedomoffice") &&!is_open_url("C:\\x.xlsx") && !is_open_url("freedomofficex:"));
    }

    #[test]
    fn bad_links() {
        let long = format!("https://{}.example.com", "a".repeat(2100));
        let bad = [
            String::new(),
            "javascript:alert(1)".into(),
            "file:///etc/passwd".into(),
            format!("https://open?server=https%3A%2F%2Fa.com&item={ID}"),
            format!("freedomoffice:open?server=https%3A%2F%2Fa.com&item={ID}"),
            format!("freedomoffice://close?server=https%3A%2F%2Fa.com&item={ID}"),
            format!("freedomoffice://open/x?server=https%3A%2F%2Fa.com&item={ID}"),
            format!("freedomoffice://evil.com/open?server=https%3A%2F%2Fa.com&item={ID}"),
            "freedomoffice://open".into(),
            "freedomoffice://open?".into(),
            format!("freedomoffice://open?item={ID}"),
            "freedomoffice://open?server=https%3A%2F%2Fa.com".into(),
            format!("{}&x=1", link("https://a.com", ID)),
            format!("{}&", link("https://a.com", ID)),
            format!("{}&item={ID}", link("https://a.com", ID)),
            format!("{}&server=https%3A%2F%2Fb.com", link("https://a.com", ID)),
            format!("{}#frag", link("https://a.com", ID)),
            format!("freedomoffice://open?Server=https%3A%2F%2Fa.com&item={ID}"),
            format!("freedomoffice://open?server&item={ID}"),
            format!(" {}", link("https://a.com", ID)),
            format!("{}\n", link("https://a.com", ID)),
            // Item ids.
            link("https://a.com", ""),
            link("https://a.com", &ID.to_uppercase()),
            link("https://a.com", &ID[1..]),
            link("https://a.com", &format!("{ID}0")),
            link("https://a.com", "0123456789abcdef0123456789abcdeg"),
            link("https://a.com", "..%2F..%2F..%2Fetc%2Fpasswd0000000"),
            link("https://a.com", &crate::cloud::pct_encode(&format!("{}\n", &ID[1..]))),
            // Servers.
            link("http://evil.com", ID),
            link("http://127.0.0.2", ID),
            link("http://localhost.evil.com", ID),
            link("ftp://a.com", ID),
            link("javascript:alert(1)", ID),
            link("file:///etc/passwd", ID),
            link("a.com", ID),
            link("https://", ID),
            link("https://user:pw@a.com", ID),
            link("https://a.com@evil.com", ID),
            link("https://a.com/?x=1", ID),
            link("https://a.com/#x", ID),
            link("https://a.com/a b", ID),
            link("https://a.com\n", ID),
            link("https://a.com/x\r\nHost: evil", ID),
            link("https://[::1]", ID),
            link("http://[::1]:8080", ID),
            link("https://a.com:0", ID),
            link("https://a.com:65536", ID),
            link("https://a.com:", ID),
            link("https://a.com:80:80", ID),
            link("https://a.com:8a", ID),
            link("https://-a.com", ID),
            link("https://a..com", ID),
            link("https://a_b.com", ID),
            link("https://a.com/../x", ID),
            link("https://a.com/x//y", ID),
            link("https://a.com/%2e%2e", ID),
            link("https://a.com\\evil", ID),
            link("https://ex\u{e4}mple.com", ID),
            link(&long, ID),
            // Escapes.
            format!("freedomoffice://open?server=https%3A%2F%2Fa.com%&item={ID}"),
            format!("freedomoffice://open?server=https%3A%2F%2Fa.com%zz&item={ID}"),
            format!("freedomoffice://open?server=https%3A%2F%2Fa.com%2&item={ID}"),
            format!("freedomoffice://open?server=https%3A%2F%2Fa.com%FF&item={ID}"),
            format!("freedomoffice://open?server=https%3A%2F%2Fa.com%0A&item={ID}"),
            format!("freedomoffice://open?server=https%3A%2F%2Fa.com%00&item={ID}"),
            format!("freedomoffice://open?server=https%3A%2F%2Fa.com+x&item={ID}"),
            format!("freedomoffice://open?server=https%253A%252F%252Fa.com&item={ID}"),
            format!("{}{}", link("https://a.com", ID), "x".repeat(MAX_LINK)),
        ];
        for l in &bad {
            assert!(parse(l).is_err(), "accepted: {l:?}");
        }
    }

    #[test]
    fn servers_match_by_scheme_host_and_port() {
        assert!(same_server("https://Files.Example.com/", "https://files.example.com"));
        assert!(same_server("https://files.example.com:443", "https://files.example.com"));
        assert!(same_server("HTTP://LOCALHOST:80/", "http://localhost"));
        assert!(same_server("https://a.com/Office/", "https://A.COM/Office"));
        assert!(!same_server("https://a.com/office", "https://a.com/Office"));
        assert!(!same_server("https://a.com:8443", "https://a.com"));
        assert!(!same_server("http://localhost:8080", "https://localhost:8080"));
        assert!(!same_server("https://a.com", "https://a.com.evil.com"));
    }

    #[test]
    fn windows_registration_never_takes_over_another_handler() {
        let exe = r"C:\Program Files\FreedomOffice\FreedomSheets.exe";
        let all = |_: &str| true;
        assert!(should_register(None, exe, &all), "nothing registered");
        assert!(should_register(Some(Some(&handler_command(exe))), exe, &all), "ours");
        assert!(should_register(Some(Some(r#""c:/program files/freedomoffice/freedomsheets.exe" "%1""#)), exe, &all), "ours, other spelling");
        assert!(should_register(Some(Some(r"C:\Tools\FreedomSheets.exe %1")), r"c:\tools\freedomsheets.exe", &all), "unquoted");
        assert!(!should_register(Some(Some(r#""C:\Other\FreedomSheets.exe" "%1""#)), exe, &all), "another copy");
        assert!(!should_register(Some(Some(r#""C:\Program Files\Evil\handler.exe" "%1""#)), exe, &all));
        assert!(!should_register(Some(Some(r"C:\Program")), exe, &all), "unquoted prefix isn't ours");
        assert!(!should_register(Some(Some("")), exe, &all));
        assert!(!should_register(Some(None), exe, &all), "key without a readable command");
        assert_eq!(handler_command(exe), r#""C:\Program Files\FreedomOffice\FreedomSheets.exe" "%1""#);
    }

    #[test]
    fn punycode_labels_are_decoded() {
        assert_eq!(punycode_decode("mnchen-3ya").as_deref(), Some("münchen"));
        assert_eq!(punycode_decode("bcher-kva").as_deref(), Some("bücher"));
        assert_eq!(punycode_decode("tda").as_deref(), Some("ü"));
        assert_eq!(punycode_decode("a-b--3ra").as_deref(), Some("a-b-ü"));
        assert_eq!(punycode_decode("80ak6aa92e").as_deref(), Some("аррӏе"), "Cyrillic look-alike of apple");
        assert_eq!(punycode_decode("ihqwcrb4cv8a8dqg056pqjye").as_deref(), Some("他们为什么不说中文"), "RFC 3492 sample");
        for bad in ["", "abc-", "a", "99999999a", "999999999999999999a", "99999999999", "mnchen-3y!", "-", "mnchen-3ya\u{e9}"] {
            assert_eq!(punycode_decode(bad), None, "{bad:?}");
        }
        let h = host_name("https://xn--80ak6aa92e.com:8443");
        assert_eq!((h.shown.as_str(), h.raw.as_str(), h.undecodable), ("аррӏе.com:8443", "xn--80ak6aa92e.com:8443", false));
        let h = host_name("https://files.example.com/office");
        assert_eq!((h.shown.as_str(), h.raw.as_str(), h.undecodable), ("files.example.com", "files.example.com", false));
        let h = host_name("https://xn--99999999a.example.com");
        assert_eq!((h.shown.as_str(), h.undecodable), ("xn--99999999a.example.com", true));
    }

    #[test]
    fn windows_registration_replaces_a_handler_that_is_gone() {
        let exe = r"C:\Users\ann\FreedomSheets.exe";
        let none = |_: &str| false;
        let only = |there: &'static str| move |p: &str| p.eq_ignore_ascii_case(there);
        // The registered program was uninstalled (quoted, unquoted, UNC).
        assert!(should_register(Some(Some(r#""C:\Old\FreedomSheets.exe" "%1""#)), exe, &none));
        assert!(should_register(Some(Some(r"C:\Old\Sheets.exe %1")), exe, &none));
        assert!(should_register(Some(Some(r#""\\nas\apps\handler.exe" "%1""#)), exe, &none));
        // Still installed: kept.
        assert!(!should_register(Some(Some(r#""C:\Old\FreedomSheets.exe" "%1""#)), exe, &only(r"C:\Old\FreedomSheets.exe")));
        // Unquoted with spaces: Windows also tries "C:\Program Files\Other\h.exe"; it's there.
        assert!(!should_register(Some(Some(r"C:\Program Files\Other\h.exe %1")), exe, &only(r"C:\Program Files\Other\h.exe")));
        assert!(!should_register(Some(Some(r"C:\Program Files\Other\h %1")), exe, &only(r"C:\Program Files\Other\h.exe")));
        // Can't tell: names looked up on PATH, environment variables, empty or unreadable.
        assert!(!should_register(Some(Some("rundll32 url.dll,FileProtocolHandler %1")), exe, &none));
        assert!(!should_register(Some(Some(r#""%ProgramFiles%\Other\h.exe" "%1""#)), exe, &none));
        assert!(!should_register(Some(Some(r#""" "%1""#)), exe, &none));
        assert!(!should_register(Some(Some("")), exe, &none));
        assert!(!should_register(Some(None), exe, &none));
    }
}
