//! Microsoft identity platform sign-in: OAuth 2.0 authorization code flow with PKCE for a
//! public client, using the system browser and a loopback redirect (`http://localhost:<port>`).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{form_encode, pct_decode, Request, Response, Transport};

/// Work/school and personal accounts.
pub const AUTHORITY: &str = "https://login.microsoftonline.com/common/oauth2/v2.0";
pub const SCOPES: &str = "openid profile offline_access User.Read Files.ReadWrite.All Sites.Read.All";

// ---------------------------------------------------------------------------
// PKCE
// ---------------------------------------------------------------------------

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Result<Self, String> {
        Ok(Self::from_verifier(base64url(&random_bytes(32)?)))
    }

    /// S256 challenge: BASE64URL(SHA256(verifier)).
    pub fn from_verifier(verifier: String) -> Self {
        let challenge = base64url(&sha256(verifier.as_bytes()));
        Pkce { verifier, challenge }
    }
}

/// Base64url without padding (RFC 4648 section 5).
pub fn base64url(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len() * 4 / 3 + 3);
    for ch in data.chunks(3) {
        let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        for i in 0..=ch.len() {
            out.push(A[(n >> (18 - 6 * i)) as usize & 63] as char);
        }
    }
    out
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be,
        0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa,
        0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85,
        0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
        0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f,
        0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

/// Cryptographically secure random bytes from the operating system.
pub fn random_bytes(n: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; n];
    #[cfg(windows)]
    {
        use windows_sys::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
        let st = unsafe { BCryptGenRandom(std::ptr::null_mut(), buf.as_mut_ptr(), n as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        if st != 0 {
            return Err(format!("The system random number generator failed (0x{st:08X})."));
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).map_err(|e| e.to_string())?;
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Authorization request and token endpoint
// ---------------------------------------------------------------------------

pub fn auth_url(client_id: &str, redirect_uri: &str, challenge: &str, state: &str) -> String {
    format!(
        "{AUTHORITY}/authorize?{}",
        form_encode(&[
            ("client_id", client_id),
            ("response_type", "code"),
            ("redirect_uri", redirect_uri),
            ("response_mode", "query"),
            ("scope", SCOPES),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("prompt", "select_account"),
        ])
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tokens {
    pub access: String,
    pub refresh: String,
    /// Unix time (seconds) when the access token expires.
    pub expires_at: u64,
}

impl Tokens {
    /// True when the access token expires within two minutes.
    pub fn expired(&self, now: u64) -> bool {
        now + 120 >= self.expires_at
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Reads a token endpoint answer; keeps `prev_refresh` when no new refresh token is issued.
pub fn parse_token_response(resp: &Response, prev_refresh: &str, now: u64) -> Result<Tokens, String> {
    if !resp.ok() {
        let v: serde_json::Value = serde_json::from_slice(&resp.body).unwrap_or_default();
        if v["error"].as_str() == Some("invalid_grant") {
            return Err("Your sign-in has expired. Sign in again.".into());
        }
        return Err(resp.error_message());
    }
    let v = resp.json()?;
    let access = v["access_token"].as_str().ok_or("The sign-in answer had no access token.")?.to_string();
    let refresh = v["refresh_token"].as_str().unwrap_or(prev_refresh).to_string();
    let expires_in = v["expires_in"].as_u64().or_else(|| v["expires_in"].as_str().and_then(|s| s.parse().ok())).unwrap_or(3600);
    Ok(Tokens { access, refresh, expires_at: now + expires_in })
}

pub fn exchange_code(http: &dyn Transport, client_id: &str, code: &str, verifier: &str, redirect_uri: &str) -> Result<Tokens, String> {
    let req = Request::new("POST", format!("{AUTHORITY}/token")).form(&[
        ("client_id", client_id),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
        ("scope", SCOPES),
    ]);
    parse_token_response(&http.send(req)?, "", now_secs())
}

pub fn refresh(http: &dyn Transport, client_id: &str, refresh_token: &str) -> Result<Tokens, String> {
    let req = Request::new("POST", format!("{AUTHORITY}/token")).form(&[
        ("client_id", client_id),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("scope", SCOPES),
    ]);
    parse_token_response(&http.send(req)?, refresh_token, now_secs())
}

// ---------------------------------------------------------------------------
// Loopback redirect listener
// ---------------------------------------------------------------------------

/// Reads the redirect's request target (`/?code=...&state=...`). `None` for other paths
/// (e.g. `/favicon.ico`) and for requests without this sign-in's `state`: anything on the
/// machine can reach the loopback port, so those are ignored and the wait goes on
/// (RFC 8252 §8.9). Only an answer carrying the right state can end the sign-in.
pub fn parse_callback(target: &str, state: &str) -> Option<Result<String, String>> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/" || query.is_empty() {
        return None;
    }
    let param = |name: &str| query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| pct_decode(v));
    if param("state").as_deref() != Some(state) {
        return None;
    }
    if let Some(err) = param("error") {
        let desc = param("error_description").unwrap_or_default();
        let desc = desc.lines().next().unwrap_or("").trim().to_string();
        return Some(Err(if err == "access_denied" {
            "Sign-in was cancelled.".into()
        } else if desc.is_empty() {
            format!("Sign-in failed: {err}")
        } else {
            format!("Sign-in failed: {desc}")
        }));
    }
    param("code").filter(|c| !c.is_empty()).map(Ok)
}

pub struct Loopback {
    listeners: Vec<TcpListener>,
    pub port: u16,
}

impl Loopback {
    /// Listens on a random port of 127.0.0.1 (and ::1 when available, since browsers may resolve
    /// `localhost` to either).
    pub fn bind() -> std::io::Result<Self> {
        let v4 = TcpListener::bind(("127.0.0.1", 0))?;
        let port = v4.local_addr()?.port();
        let mut listeners = vec![v4];
        if let Ok(v6) = TcpListener::bind(("::1", port)) {
            listeners.push(v6);
        }
        for l in &listeners {
            l.set_nonblocking(true)?;
        }
        Ok(Loopback { listeners, port })
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://localhost:{}", self.port)
    }

    pub fn wait_for_code(&self, state: &str, cancel: &AtomicBool, timeout: Duration) -> Result<String, String> {
        let start = Instant::now();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("Sign-in was cancelled.".into());
            }
            if start.elapsed() > timeout {
                return Err("Sign-in timed out. Try again.".into());
            }
            for l in &self.listeners {
                match l.accept() {
                    Ok((mut s, _)) => {
                        if let Some(r) = answer(&mut s, state) {
                            return r;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.to_string()),
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn answer(s: &mut TcpStream, state: &str) -> Option<Result<String, String>> {
    let _ = s.set_nonblocking(false);
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 32 * 1024 {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let target = text.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("");
    let result = parse_callback(target, state);
    let (status, body) = match &result {
        None => ("404 Not Found", String::new()),
        Some(Ok(_)) => ("200 OK", page("You're signed in to FreedomOffice Sheets.", "You can close this tab and return to the app.")),
        Some(Err(_)) => ("200 OK", page("Sign-in didn't complete.", "You can close this tab and try again from FreedomOffice Sheets.")),
    };
    let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = s.flush();
    result
}

fn page(title: &str, text: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>FreedomOffice Sheets</title></head>\
         <body style=\"font-family:Segoe UI,sans-serif;margin:60px;color:#202020\"><h2 style=\"color:#217346\">{title}</h2><p>{text}</p></body></html>"
    )
}

/// The whole interactive sign-in: opens the browser, waits for the redirect, redeems the code.
pub fn sign_in(http: &dyn Transport, client_id: &str, cancel: &AtomicBool) -> Result<Tokens, String> {
    let pkce = Pkce::new()?;
    let state = base64url(&random_bytes(16)?);
    let lb = Loopback::bind().map_err(|e| format!("Couldn't start the sign-in listener: {e}"))?;
    let redirect = lb.redirect_uri();
    super::open_in_browser(&auth_url(client_id, &redirect, &pkce.challenge, &state));
    let code = lb.wait_for_code(&state, cancel, Duration::from_secs(300))?;
    exchange_code(http, client_id, &code, &pkce.verifier, &redirect)
}

// ---------------------------------------------------------------------------
// Token cache (refresh token encrypted for the current Windows user with DPAPI)
// ---------------------------------------------------------------------------

pub fn save_refresh_token(token: &str) -> Result<(), String> {
    let path = super::token_path().ok_or("LOCALAPPDATA isn't set.")?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, dpapi::protect(token.as_bytes())?).map_err(|e| e.to_string())
}

pub fn load_refresh_token() -> Option<String> {
    let data = std::fs::read(super::token_path()?).ok()?;
    String::from_utf8(dpapi::unprotect(&data).ok()?).ok().filter(|s| !s.is_empty())
}

pub fn delete_token() {
    if let Some(p) = super::token_path() {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(windows)]
pub(crate) mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};

    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        transform(data, true)
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        transform(data, false)
    }

    fn transform(data: &[u8], encrypt: bool) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        use std::ptr::{null, null_mut};
        let ok = unsafe {
            if encrypt {
                CryptProtectData(&input, null(), null(), null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            } else {
                CryptUnprotectData(&input, null_mut(), null(), null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            }
        };
        if ok == 0 || out.pbData.is_null() {
            return Err("Windows couldn't protect the sign-in token.".into());
        }
        let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
        unsafe { LocalFree(out.pbData as _) };
        Ok(v)
    }
}

#[cfg(not(windows))]
pub(crate) mod dpapi {
    pub fn protect(_: &[u8]) -> Result<Vec<u8>, String> {
        Err("Keeping you signed in is only supported on Windows.".into())
    }

    pub fn unprotect(_: &[u8]) -> Result<Vec<u8>, String> {
        Err("Keeping you signed in is only supported on Windows.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let long = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(hex(&sha256(long)), "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
    }

    #[test]
    fn base64url_without_padding() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn pkce_rfc7636_example() {
        // RFC 7636 appendix B.
        let p = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(p.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let fresh = Pkce::new().unwrap();
        assert_eq!(fresh.verifier.len(), 43);
        assert_ne!(fresh.verifier, Pkce::new().unwrap().verifier);
    }

    #[test]
    fn authorization_url() {
        let u = auth_url("cid", "http://localhost:5050", "chal", "st8");
        assert!(u.starts_with("https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id=cid&response_type=code&"));
        assert!(u.contains("&redirect_uri=http%3A%2F%2Flocalhost%3A5050&"));
        assert!(u.contains("&scope=openid%20profile%20offline_access%20User.Read%20Files.ReadWrite.All%20Sites.Read.All&"));
        assert!(u.contains("&code_challenge=chal&code_challenge_method=S256&state=st8"));
    }

    #[test]
    fn callback_parsing() {
        assert_eq!(parse_callback("/?code=M.C1%2Fx&state=s1", "s1"), Some(Ok("M.C1/x".into())));
        // Answers for another (or no) state are ignored, errors included.
        assert_eq!(parse_callback("/?code=abc&state=other", "s1"), None);
        assert_eq!(parse_callback("/?error=access_denied&state=other", "s1"), None);
        assert_eq!(parse_callback("/?error=access_denied", "s1"), None);
        assert_eq!(parse_callback("/?error=access_denied&error_description=x&state=s1", "s1"), Some(Err("Sign-in was cancelled.".into())));
        assert_eq!(
            parse_callback("/?error=invalid_request&error_description=AADSTS50011%3A+bad+redirect%0D%0ATrace&state=s1", "s1"),
            Some(Err("Sign-in failed: AADSTS50011: bad redirect".into()))
        );
        assert_eq!(parse_callback("/favicon.ico", "s1"), None);
        assert_eq!(parse_callback("/", "s1"), None);
    }

    #[test]
    fn loopback_receives_code() {
        let lb = Loopback::bind().unwrap();
        let port = lb.port;
        let client = std::thread::spawn(move || {
            let get = |target: &str| {
                let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
                s.write_all(format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes()).unwrap();
                let mut out = String::new();
                let _ = s.read_to_string(&mut out);
                out
            };
            // A forged error with the wrong state must not end the sign-in.
            let forged = get("/?error=access_denied&state=evil");
            assert!(forged.starts_with("HTTP/1.1 404"));
            get("/?code=the-code&state=xyz")
        });
        let cancel = AtomicBool::new(false);
        assert_eq!(lb.wait_for_code("xyz", &cancel, Duration::from_secs(10)), Ok("the-code".into()));
        let page = client.join().unwrap();
        assert!(page.starts_with("HTTP/1.1 200 OK"));
        assert!(page.contains("You can close this tab"));
        cancel.store(true, Ordering::Relaxed);
        assert!(lb.wait_for_code("xyz", &cancel, Duration::from_secs(10)).is_err());
    }

    #[test]
    fn token_response() {
        let ok = Response { status: 200, body: br#"{"access_token":"AT","refresh_token":"RT2","expires_in":3599}"#.to_vec() };
        assert_eq!(parse_token_response(&ok, "RT1", 1000), Ok(Tokens { access: "AT".into(), refresh: "RT2".into(), expires_at: 4599 }));
        let keep = Response { status: 200, body: br#"{"access_token":"AT","expires_in":"60"}"#.to_vec() };
        let t = parse_token_response(&keep, "RT1", 1000).unwrap();
        assert_eq!((t.refresh.as_str(), t.expires_at), ("RT1", 1060));
        assert!(t.expired(1000));
        let bad = Response { status: 400, body: br#"{"error":"invalid_grant","error_description":"AADSTS70008: expired"}"#.to_vec() };
        assert_eq!(parse_token_response(&bad, "RT1", 0), Err("Your sign-in has expired. Sign in again.".into()));
        let other = Response { status: 400, body: br#"{"error":"invalid_client","error_description":"AADSTS7000218: needs secret\r\nTrace ID: 1"}"#.to_vec() };
        assert_eq!(parse_token_response(&other, "RT1", 0), Err("AADSTS7000218: needs secret".into()));
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_round_trip() {
        let enc = dpapi::protect(b"refresh-token").unwrap();
        assert_ne!(enc, b"refresh-token");
        assert_eq!(dpapi::unprotect(&enc).unwrap(), b"refresh-token");
    }
}
