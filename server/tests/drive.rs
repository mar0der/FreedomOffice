//! What FreedomOffice Drive needs from the server, end to end: browser sessions for the JSON
//! API (cookie, CSRF header, origin checks), the audit log, streamed downloads with Range and
//! streamed uploads. A real server on 127.0.0.1 with a throwaway data directory; its public URL
//! is https (so the cookie is `__Host-fo_web; Secure`) and the tests send the cookie and Origin
//! headers themselves, as a browser behind a TLS proxy would.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use freedomoffice_server::config::Config;
use freedomoffice_server::store::Store;
use freedomoffice_server::{spawn, Running};
use serde_json::{json, Value};
use tungstenite::client::IntoClientRequest;

/// Test-only password for the throwaway accounts below.
const PW: &str = "test-password-1";
const ORIGIN: &str = "https://drive.example.test";
const COOKIE: &str = "__Host-fo_web";

struct Srv {
    run: Option<Running>,
    url: String,
    dir: PathBuf,
    cfg: Config,
    agent: ureq::Agent,
}

impl Drop for Srv {
    fn drop(&mut self) {
        drop(self.run.take());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Resp {
    status: u16,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
}

impl Resp {
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn code(&self) -> String {
        self.json()["code"].as_str().unwrap_or("").to_string()
    }
}

fn start_with(f: impl FnOnce(&mut Config)) -> Srv {
    let dir = std::env::temp_dir().join(format!("fo-drive-test-{}", freedomoffice_server::util::new_id()));
    let mut cfg = Config { data_dir: dir.join("data"), login_delay_ms: 10, public_url: ORIGIN.into(), ..Config::default() };
    f(&mut cfg);
    {
        let s = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
        s.create_user("alice", "Alice Admin", "alice@example.test", PW, true).unwrap();
        s.create_user("bob", "Bob Builder", "bob@example.test", PW, false).unwrap();
        s.create_user("carol", "Carol C", "", PW, false).unwrap();
    }
    let run = spawn(cfg.clone()).unwrap();
    let url = run.url();
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).timeout_global(Some(Duration::from_secs(30))).build().into();
    Srv { run: Some(run), url, dir, cfg, agent }
}

fn start() -> Srv {
    start_with(|_| {})
}

impl Srv {
    fn store(&self) -> Store {
        Store::open(&self.cfg.db_path(), &self.cfg.files_dir(), self.cfg.versions_to_keep).unwrap()
    }

    fn send(&self, method: &str, path: &str, headers: &[(&str, &str)], body: Option<Vec<u8>>) -> Resp {
        let mut b = ureq::http::Request::builder().method(method).uri(format!("{}{path}", self.url));
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let res = match body {
            Some(data) => self.agent.run(b.body(data).unwrap()),
            None => self.agent.run(b.body(()).unwrap()),
        };
        let mut res = res.unwrap();
        let status = res.status().as_u16();
        let headers = res.headers().iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string())).collect();
        let body = res.body_mut().read_to_vec().unwrap_or_default();
        Resp { status, body, headers }
    }

    fn api(&self, method: &str, path: &str, token: &str, body: Value) -> Resp {
        let auth = format!("Bearer {token}");
        self.send(method, path, &[("Content-Type", "application/json"), ("Authorization", &auth)], Some(body.to_string().into_bytes()))
    }

    fn token(&self, user: &str) -> String {
        let r = self.send("POST", "/api/login", &[("Content-Type", "application/json")], Some(json!({ "username": user, "password": PW }).to_string().into_bytes()));
        assert_eq!(r.status, 200, "{}", r.text());
        r.json()["token"].as_str().unwrap().to_string()
    }

    fn web_login_raw(&self, user: &str, password: &str, headers: &[(&str, &str)]) -> Resp {
        let mut h = vec![("Content-Type", "application/json")];
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("origin")) {
            h.push(("Origin", ORIGIN));
        }
        h.extend_from_slice(headers);
        self.send("POST", "/api/web/login", &h, Some(json!({ "username": user, "password": password }).to_string().into_bytes()))
    }

    /// Signs in on the web: the browser with its cookie and CSRF token.
    fn web(&self, user: &str) -> Web<'_> {
        self.web_with(user, PW)
    }

    fn web_with(&self, user: &str, password: &str) -> Web<'_> {
        let r = self.web_login_raw(user, password, &[]);
        assert_eq!(r.status, 200, "web sign-in of {user}: {}", r.text());
        let set = r.header("set-cookie").unwrap().to_string();
        let secret = set.strip_prefix(&format!("{COOKIE}=")).unwrap().split(';').next().unwrap().to_string();
        Web { s: self, cookie: format!("{COOKIE}={secret}"), csrf: r.json()["csrf"].as_str().unwrap().to_string() }
    }

    fn root(&self, token: &str) -> String {
        self.api("GET", "/api/root", token, json!({})).json()["id"].as_str().unwrap().to_string()
    }

    fn upload(&self, token: &str, parent: &str, name: &str, data: &[u8]) -> Resp {
        self.send("POST", &format!("/api/files?parent={parent}&name={}", enc(name)), &[("Authorization", &format!("Bearer {token}"))], Some(data.to_vec()))
    }

    fn audit(&self, token: &str, query: &str) -> Vec<Value> {
        let r = self.api("GET", &format!("/api/admin/audit?{query}"), token, json!({}));
        assert_eq!(r.status, 200, "{}", r.text());
        r.json()["events"].as_array().unwrap().clone()
    }
}

fn enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// A browser signed in on the web (Drive): sends the cookie, and for changes the CSRF header
/// and the server's Origin, as the Drive page does.
struct Web<'a> {
    s: &'a Srv,
    cookie: String,
    csrf: String,
}

impl Web<'_> {
    fn get(&self, path: &str) -> Resp {
        self.s.send("GET", path, &[("Cookie", &self.cookie), ("Sec-Fetch-Site", "same-origin")], None)
    }

    fn call(&self, method: &str, path: &str, body: Value) -> Resp {
        self.raw(method, path, &[("X-FO-CSRF", &self.csrf), ("Origin", ORIGIN)], Some(body.to_string().into_bytes()))
    }

    fn raw(&self, method: &str, path: &str, headers: &[(&str, &str)], body: Option<Vec<u8>>) -> Resp {
        let mut h = vec![("Cookie", self.cookie.as_str()), ("Content-Type", "application/json")];
        h.extend_from_slice(headers);
        self.s.send(method, path, &h, body)
    }
}

#[test]
fn web_sign_in_sets_a_hardened_cookie() {
    let s = start();
    let r = s.web_login_raw("bob", PW, &[]);
    assert_eq!(r.status, 200, "{}", r.text());
    let set = r.header("set-cookie").unwrap();
    assert!(set.starts_with(&format!("{COOKIE}=")), "{set}");
    assert!(set.ends_with("; Path=/; Max-Age=43200; HttpOnly; SameSite=Strict; Secure"), "{set}");
    assert_eq!(r.header("cache-control"), Some("no-store"));
    let v = r.json();
    assert_eq!(v["user"]["username"], "bob");
    let csrf = v["csrf"].as_str().unwrap().to_string();
    assert_eq!(csrf.len(), 43);
    let secret = set.strip_prefix(&format!("{COOKIE}=")).unwrap().split(';').next().unwrap().to_string();
    assert!(!r.text().contains(&secret), "the secret is only in the HttpOnly cookie");
    // Only the hash is stored.
    let st = s.store();
    let stored: Vec<Vec<u8>> = st.conn.prepare("SELECT hash FROM admin_sessions WHERE kind = 'web'").unwrap().query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
    assert_eq!(stored, vec![freedomoffice_server::util::token_hash(&secret)]);
    drop(st);

    let w = s.web("bob");
    let r = w.get("/api/web/session");
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["user"]["username"], "bob");
    assert_eq!(r.json()["csrf"].as_str().unwrap(), w.csrf);
    assert_eq!(s.send("GET", "/api/web/session", &[], None).status, 401);
    assert_eq!(s.send("GET", "/api/web/session", &[("Cookie", &format!("{COOKIE}=nonsense"))], None).status, 401);

    // Wrong passwords, another site, not JSON: refused.
    let r = s.web_login_raw("bob", "wrong-password-1", &[]);
    assert_eq!(r.status, 401);
    assert!(r.header("set-cookie").is_none());
    assert_eq!(s.web_login_raw("bob", PW, &[("Origin", "https://evil.test")]).code(), "cross_site");
    assert_eq!(s.web_login_raw("bob", PW, &[("Sec-Fetch-Site", "cross-site")]).status, 403);
    let r = s.send("POST", "/api/web/login", &[("Content-Type", "text/plain"), ("Origin", ORIGIN)], Some(json!({ "username": "bob", "password": PW }).to_string().into_bytes()));
    assert_eq!(r.status, 400, "a cross-site form can post text/plain");
    // The admin cookie and the web cookie don't stand in for each other.
    let admin = s.send("GET", "/admin", &[("Cookie", &format!("__Host-fo_admin={}", w.cookie.split('=').nth(1).unwrap()))], None);
    assert_eq!(admin.status, 303, "a web session doesn't open /admin");
    assert_eq!(s.send("GET", "/admin", &[("Cookie", &w.cookie)], None).status, 303);
}

#[test]
fn the_cookie_works_for_the_api_with_csrf_for_changes() {
    let s = start();
    let w = s.web("bob");
    let me = w.get("/api/me");
    assert_eq!(me.status, 200, "{}", me.text());
    let root = me.json()["root"].as_str().unwrap().to_string();
    assert_eq!(w.get(&format!("/api/items/{root}/children")).status, 200);
    // Changes need the CSRF header ...
    let body = json!({ "parent": root, "name": "Projects" });
    let r = w.raw("POST", "/api/folders", &[("Origin", ORIGIN)], Some(body.to_string().into_bytes()));
    assert_eq!((r.status, r.code()), (403, "csrf_failed".to_string()));
    let r = w.raw("POST", "/api/folders", &[("Origin", ORIGIN), ("X-FO-CSRF", "x".repeat(43).as_str())], Some(body.to_string().into_bytes()));
    assert_eq!((r.status, r.code()), (403, "csrf_failed".to_string()));
    // ... and this server's origin.
    let r = w.raw("POST", "/api/folders", &[("Origin", "https://evil.test"), ("X-FO-CSRF", &w.csrf)], Some(body.to_string().into_bytes()));
    assert_eq!((r.status, r.code()), (403, "cross_site".to_string()));
    let r = w.raw("POST", "/api/folders", &[("Origin", "null"), ("X-FO-CSRF", &w.csrf)], Some(body.to_string().into_bytes()));
    assert_eq!(r.status, 403);
    let r = w.call("POST", "/api/folders", body);
    assert_eq!(r.status, 201, "{}", r.text());
    let folder = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(w.call("PATCH", &format!("/api/items/{folder}"), json!({ "name": "Work" })).status, 200);
    assert_eq!(w.raw("DELETE", &format!("/api/items/{folder}"), &[("Origin", ORIGIN)], None).status, 403);
    // Requests a browser marks as coming from another site (a sibling subdomain included)
    // don't use the session, even reads.
    let r = s.send("GET", "/api/me", &[("Cookie", &w.cookie), ("Sec-Fetch-Site", "same-site")], None);
    assert_eq!((r.status, r.code()), (403, "cross_site".to_string()));
    assert_eq!(s.send("GET", "/api/me", &[("Cookie", &w.cookie), ("Sec-Fetch-Site", "cross-site")], None).status, 403);
    assert_eq!(s.send("GET", "/api/me", &[("Cookie", &w.cookie), ("Sec-Fetch-Site", "none")], None).status, 200);
    // Bearer requests are as before, whatever cookie comes with them.
    let t = s.token("carol");
    let r = s.send("POST", "/api/folders", &[("Authorization", &format!("Bearer {t}")), ("Cookie", &w.cookie), ("Content-Type", "application/json")], Some(json!({ "name": "Mine" }).to_string().into_bytes()));
    assert_eq!(r.status, 201, "{}", r.text());
    assert_eq!(r.json()["owner"]["username"], "carol", "the token decides, not the cookie");
    // An internal header from the client is ignored.
    let r = s.send("GET", "/api/me", &[("X-FO-Internal-Web", &format!("ok {}", w.cookie.split('=').nth(1).unwrap()))], None);
    assert_eq!(r.status, 401);
    // Sign-out ends the session and clears the cookie.
    assert_eq!(w.raw("POST", "/api/web/logout", &[("Origin", ORIGIN)], None).status, 403, "sign-out needs the CSRF header too");
    let r = w.call("POST", "/api/web/logout", json!({}));
    assert_eq!(r.status, 204);
    assert!(r.header("set-cookie").unwrap().starts_with(&format!("{COOKIE}=;")));
    assert_eq!(w.get("/api/me").status, 401);
}

#[test]
fn the_live_socket_needs_a_ticket_from_a_browser() {
    let s = start();
    let t = s.token("bob");
    let root = s.root(&t);
    let id = s.upload(&t, &root, "Live.xlsx", b"version one").json()["id"].as_str().unwrap().to_string();
    let w = s.web("bob");
    let ws_url = format!("{}/api/items/{id}/live?since=0&v=1&cid=w", s.url.replace("http://", "ws://"));
    let mut req = ws_url.clone().into_client_request().unwrap();
    req.headers_mut().insert("Cookie", w.cookie.parse().unwrap());
    req.headers_mut().insert("Origin", ORIGIN.parse().unwrap());
    match tungstenite::connect(req) {
        Err(tungstenite::Error::Http(r)) => assert_eq!(r.status().as_u16(), 401),
        other => panic!("the cookie alone must not open the socket: {:?}", other.map(|_| ())),
    }
    assert_eq!(w.raw("POST", &format!("/api/items/{id}/live-ticket"), &[("Origin", ORIGIN)], None).status, 403);
    let r = w.call("POST", &format!("/api/items/{id}/live-ticket"), json!({}));
    assert_eq!(r.status, 200, "{}", r.text());
    let ticket = r.json()["ticket"].as_str().unwrap().to_string();
    let (mut ws, _) = tungstenite::connect(format!("{ws_url}&ticket={ticket}")).unwrap();
    let _ = ws.close(None);
}

#[test]
fn temporary_passwords_and_ending_web_sessions() {
    let s = start();
    let at = s.token("alice");
    let r = s.api("POST", "/api/users", &at, json!({ "username": "dan" }));
    assert_eq!(r.status, 201, "{}", r.text());
    let temp = r.json()["temporary_password"].as_str().unwrap().to_string();
    let w = s.web_with("dan", &temp);
    assert_eq!(w.get("/api/web/session").json()["user"]["must_change_password"], true);
    assert_eq!(w.get("/api/me").status, 200);
    assert_eq!(w.get("/api/root").code(), "password_change_required");
    let other = s.web_with("dan", &temp);
    let r = w.call("POST", "/api/me/password", json!({ "old_password": temp, "new_password": "a-fresh-secret-77" }));
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(w.get("/api/root").status, 200, "this session goes on");
    assert_eq!(other.get("/api/me").status, 401, "the other browser was signed out");

    // A password change from a device ends web sessions too.
    let bw = s.web("bob");
    let bt = s.token("bob");
    assert_eq!(s.api("POST", "/api/me/password", &bt, json!({ "old_password": PW, "new_password": "another-pass-word-9" })).status, 200);
    assert_eq!(bw.get("/api/me").status, 401);
    // Sign out everywhere, disable and reset end them.
    let cw = s.web("carol");
    assert_eq!(s.api("POST", "/api/users/carol/sign-out", &at, json!({})).status, 200);
    assert_eq!(cw.get("/api/me").status, 401);
    let cw = s.web("carol");
    let sessions = s.api("GET", "/api/users/carol/sessions", &at, json!({})).json();
    assert_eq!(sessions["web"][0]["kind"], "web");
    assert_eq!(s.api("POST", "/api/users/carol/disable", &at, json!({})).status, 204);
    assert_eq!(cw.get("/api/me").status, 401);
    assert_eq!(s.api("POST", "/api/users/carol/enable", &at, json!({})).status, 204);
    let cw = s.web("carol");
    assert_eq!(s.api("POST", "/api/users/carol/reset-password", &at, json!({})).status, 200);
    assert_eq!(cw.get("/api/me").status, 401);
    // At most 10 web sessions per person: the oldest end.
    let first = s.web("alice");
    let rest: Vec<Web> = (0..10).map(|_| s.web("alice")).collect();
    assert_eq!(first.get("/api/me").status, 401);
    assert!(rest.iter().all(|w| w.get("/api/me").status == 200));
    // /api/logout with the cookie ends that session.
    assert_eq!(rest[0].call("POST", "/api/logout", json!({})).status, 204);
    assert_eq!(rest[0].get("/api/me").status, 401);
}

#[test]
fn web_sign_in_shares_the_limits() {
    let s = start();
    for _ in 0..10 {
        assert_eq!(s.web_login_raw("bob", "wrong-password-1", &[]).status, 401);
    }
    assert_eq!(s.web_login_raw("bob", PW, &[]).status, 429);
    let r = s.send("POST", "/api/login", &[("Content-Type", "application/json")], Some(json!({ "username": "bob", "password": PW }).to_string().into_bytes()));
    assert_eq!(r.status, 429, "the same limiter as /api/login");
    assert_eq!(s.web_login_raw("not a name!", PW, &[]).status, 429, "the address is limited too");
}

#[test]
fn the_audit_log_records_and_scopes_events() {
    let s = start();
    let at = s.token("alice");
    let bt = s.token("bob");
    // Failed sign-ins: valid usernames only, never the password.
    let _ = s.web_login_raw("bob", "secret-guess-123", &[]);
    let _ = s.web_login_raw("bad name<script>", "secret-guess-456", &[]);
    let org = s.api("POST", "/api/orgs", &at, json!({ "name": "Team" })).json()["id"].as_str().unwrap().to_string();
    assert_eq!(s.api("POST", &format!("/api/orgs/{org}/members"), &at, json!({ "username": "bob", "role": "admin" })).status, 201);
    assert_eq!(s.api("PATCH", &format!("/api/orgs/{org}"), &at, json!({ "member_role": "view" })).status, 200);
    let inv = s.api("POST", &format!("/api/orgs/{org}/invites"), &bt, json!({ "email": "eve@example.test" }));
    let inv_url = inv.json()["url"].as_str().unwrap().to_string();
    let team_root = s.api("GET", &format!("/api/orgs/{org}"), &at, json!({})).json()["root"].as_str().unwrap().to_string();
    let f = s.upload(&bt, &team_root, "Plan.txt", b"top secret contents").json()["id"].as_str().unwrap().to_string();
    assert_eq!(s.api("POST", &format!("/api/items/{f}/permissions"), &bt, json!({ "username": "carol", "role": "view" })).status, 200);
    let link = s.api("POST", &format!("/api/items/{f}/links"), &bt, json!({ "role": "view" }));
    let link_url = link.json()["url"].as_str().unwrap().to_string();
    assert_eq!(s.api("PATCH", &format!("/api/items/{f}"), &bt, json!({ "name": "Plan v2.txt" })).status, 200);
    assert_eq!(s.api("DELETE", &format!("/api/items/{f}"), &bt, json!({})).status, 204);
    assert_eq!(s.api("POST", &format!("/api/items/{f}/restore"), &bt, json!({})).status, 200);
    // Bob's own files are not in Team.
    let mine = s.upload(&bt, &s.root(&bt), "Private.txt", b"x").json()["id"].as_str().unwrap().to_string();
    assert!(!mine.is_empty());

    let all = s.audit(&at, "limit=200");
    let actions: Vec<&str> = all.iter().map(|e| e["action"].as_str().unwrap()).collect();
    for a in ["sign_in", "sign_in.failed", "org.create", "member.add", "org.settings", "invite.create", "file.upload", "permission.grant", "link.create", "item.rename", "item.trash", "item.restore"] {
        assert!(actions.contains(&a), "{a} in {actions:?}");
    }
    let failed: Vec<&Value> = all.iter().filter(|e| e["action"] == "sign_in.failed").collect();
    assert_eq!(failed.len(), 1, "only the valid username: {failed:?}");
    assert_eq!(failed[0]["target_user"], "bob");
    assert_eq!(failed[0]["ip"], "127.0.0.1");
    assert!(all.iter().all(|e| e["ip"] == "127.0.0.1"), "every event has the request's address: {all:?}");
    let dump = serde_json::to_string(&all).unwrap();
    for secret in ["secret-guess", PW, "top secret contents", &inv_url, &link_url, inv_url.rsplit('/').next().unwrap(), link_url.rsplit('/').next().unwrap(), &at, &bt] {
        assert!(!dump.contains(secret), "{secret} must not be in the audit log");
    }
    // Filters and paging.
    assert!(s.audit(&at, "action=org").iter().all(|e| e["action"].as_str().unwrap().starts_with("org.")));
    assert!(s.audit(&at, "user=carol").iter().all(|e| e["actor"] == "carol" || e["target_user"] == "carol"));
    let page1 = s.api("GET", "/api/admin/audit?limit=3", &at, json!({})).json();
    assert_eq!(page1["events"].as_array().unwrap().len(), 3);
    let next = page1["next_before"].as_i64().unwrap();
    let page2 = s.audit(&at, &format!("limit=3&before={next}"));
    assert!(page2.iter().all(|e| e["id"].as_i64().unwrap() < next));
    assert_eq!(s.api("GET", "/api/admin/audit?action=DROP%20TABLE", &at, json!({})).status, 400);

    // Bob (an admin of Team) sees Team's events only; Carol nothing.
    let team = s.audit(&bt, "limit=200");
    assert!(!team.is_empty());
    assert!(team.iter().all(|e| e["org"]["id"] == org), "{team:?}");
    assert!(team.iter().any(|e| e["action"] == "file.upload"));
    assert!(!team.iter().any(|e| e["detail"].as_str().unwrap_or("").contains("Private.txt")));
    let ct = s.token("carol");
    assert_eq!(s.api("GET", "/api/admin/audit", &ct, json!({})).status, 403);
    let other = s.api("POST", "/api/orgs", &at, json!({ "name": "Other" })).json()["id"].as_str().unwrap().to_string();
    assert_eq!(s.api("GET", &format!("/api/admin/audit?org={other}"), &bt, json!({})).status, 404);

    // The admin page shows it, escaped, with the admin headers.
    let r = s.send("POST", "/admin/login", &[("Content-Type", "application/x-www-form-urlencoded"), ("Origin", ORIGIN)], Some(format!("username=bob&password={PW}").into_bytes()));
    let cookie = r.header("set-cookie").unwrap().split(';').next().unwrap().to_string();
    assert_eq!(s.api("PATCH", &format!("/api/orgs/{org}"), &at, json!({ "name": "Tom & Jerry's" })).status, 200);
    let page = s.send("GET", "/admin/audit?user=%3Cscript%3E", &[("Cookie", &cookie)], None);
    assert_eq!(page.status, 200, "{}", page.text());
    assert!(page.text().contains("&lt;script&gt;") && !page.text().contains("<script>"));
    assert_eq!(page.header("content-security-policy").map(|c| c.starts_with("default-src 'none'")), Some(true));
    let page = s.send("GET", "/admin/audit", &[("Cookie", &cookie)], None);
    assert!(page.text().contains("Tom &amp; Jerry&#39;s") && page.text().contains("file.upload"));
    assert!(page.text().contains("Audit log"));
}

#[test]
fn downloads_stream_with_ranges_and_safe_headers() {
    let s = start();
    let t = s.token("bob");
    let root = s.root(&t);
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let id = s.upload(&t, &root, "Data Set ü.bin", &data).json()["id"].as_str().unwrap().to_string();
    let auth = format!("Bearer {t}");
    let path = format!("/api/items/{id}/content");
    let full = s.send("GET", &path, &[("Authorization", &auth)], None);
    assert_eq!(full.status, 200);
    assert_eq!(full.body, data);
    assert_eq!(full.header("accept-ranges"), Some("bytes"));
    assert_eq!(full.header("content-type"), Some("application/octet-stream"));
    assert_eq!(full.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(full.header("cross-origin-resource-policy"), Some("same-origin"));
    assert_eq!(full.header("content-length"), Some("200000"));
    assert!(full.header("x-live-seq").is_some(), "Sheets' live join headers stay");
    let etag = full.header("etag").unwrap().to_string();
    let part = s.send("GET", &path, &[("Authorization", &auth), ("Range", "bytes=100-199")], None);
    assert_eq!(part.status, 206);
    assert_eq!(part.header("content-range"), Some("bytes 100-199/200000"));
    assert_eq!(part.body, &data[100..200]);
    let tail = s.send("GET", &path, &[("Authorization", &auth), ("Range", "bytes=-10")], None);
    assert_eq!(tail.body, &data[199_990..]);
    let open = s.send("GET", &path, &[("Authorization", &auth), ("Range", "bytes=150000-")], None);
    assert_eq!((open.status, open.body.len()), (206, 50_000));
    let r = s.send("GET", &path, &[("Authorization", &auth), ("Range", "bytes=200000-")], None);
    assert_eq!(r.status, 416);
    assert_eq!(r.header("content-range"), Some("bytes */200000"));
    let r = s.send("GET", &path, &[("Authorization", &auth), ("Range", "bytes=0-1"), ("If-Range", "\"v9-other\"")], None);
    assert_eq!(r.status, 200, "another version: all of it");
    let r = s.send("GET", &path, &[("Authorization", &auth), ("Range", "bytes=0-1"), ("If-Range", &etag)], None);
    assert_eq!(r.status, 206);
    let r = s.send("GET", &format!("{path}?download=1"), &[("Authorization", &auth)], None);
    assert_eq!(r.header("content-disposition"), Some("attachment; filename=\"Data Set _.bin\"; filename*=UTF-8''Data%20Set%20%C3%BC.bin"));
    // HTML and SVG never run as this server's origin.
    let html = s.upload(&t, &root, "page.html", b"<script>alert(1)</script>").json()["id"].as_str().unwrap().to_string();
    let r = s.send("GET", &format!("/api/items/{html}/content"), &[("Authorization", &auth)], None);
    assert_eq!(r.header("content-type"), Some("text/html; charset=utf-8"));
    assert!(r.header("content-security-policy").unwrap().starts_with("sandbox;"));
    let svg = s.upload(&t, &root, "pic.SVG", b"<svg/>").json()["id"].as_str().unwrap().to_string();
    let r = s.send("GET", &format!("/api/items/{svg}/content"), &[("Authorization", &auth)], None);
    assert_eq!(r.header("content-type"), Some("image/svg+xml"));
    assert!(r.header("content-security-policy").unwrap().starts_with("sandbox;"));
    // Older versions too, and the cookie session.
    let w = s.web("bob");
    let r = w.get(&format!("{path}?version=1&download=1"));
    assert_eq!((r.status, r.body.len()), (200, 200_000));
    // Share-link downloads get the same treatment.
    let link = s.api("POST", &format!("/api/items/{html}/links"), &t, json!({ "role": "view" })).json()["url"].as_str().unwrap().to_string();
    let token = link.rsplit('/').next().unwrap();
    let r = s.send("GET", &format!("/l/{token}/download"), &[], None);
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("text/html; charset=utf-8"));
    assert!(r.header("content-disposition").unwrap().starts_with("attachment;"));
    assert!(r.header("content-security-policy").unwrap().starts_with("sandbox;"));
    assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
    let r = s.send("GET", &format!("/l/{token}/download"), &[("Range", "bytes=0-7")], None);
    assert_eq!((r.status, r.body.as_slice()), (206, &b"<script>"[..]));
}

/// Sends a chunked upload over a raw connection (no Content-Length, so the limit must hold
/// while the body streams in); the status code.
fn chunked_upload(s: &Srv, token: &str, path: &str, chunks: usize, chunk: usize) -> u16 {
    let mut c = TcpStream::connect(s.url.trim_start_matches("http://")).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let head = format!("POST {path} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
    c.write_all(head.as_bytes()).unwrap();
    let piece = vec![b'a'; chunk];
    for _ in 0..chunks {
        if c.write_all(format!("{chunk:x}\r\n").as_bytes()).is_err() || c.write_all(&piece).is_err() || c.write_all(b"\r\n").is_err() {
            break;
        }
    }
    let _ = c.write_all(b"0\r\n\r\n");
    let mut out = Vec::new();
    let _ = c.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out);
    text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0)
}

#[test]
fn uploads_stream_to_disk_within_the_limit() {
    let s = start_with(|c| c.max_upload_mb = 1);
    let t = s.token("bob");
    let root = s.root(&t);
    let path = format!("/api/files?parent={root}&name=Big.bin");
    assert_eq!(chunked_upload(&s, &t, &path, 20, 64 * 1024), 413, "1.25 MB without Content-Length");
    assert_eq!(chunked_upload(&s, &t, &path, 8, 64 * 1024), 201);
    let id = s.api("GET", &format!("/api/items/{root}/children"), &t, json!({})).json()["items"][0]["id"].as_str().unwrap().to_string();
    let r = s.send("GET", &format!("/api/items/{id}/content"), &[("Authorization", &format!("Bearer {t}"))], None);
    assert_eq!(r.body, vec![b'a'; 8 * 64 * 1024]);
    assert_eq!(chunked_upload(&s, &t, &format!("/api/items/{id}/content"), 20, 64 * 1024), 405, "PUT only");
    // PUT keeps If-Match: a stale etag is 412 before anything is stored.
    let etag = r.header("etag").unwrap().to_string();
    let auth = format!("Bearer {t}");
    let r = s.send("PUT", &format!("/api/items/{id}/content"), &[("Authorization", &auth), ("If-Match", "\"v0-stale\"")], Some(b"new".to_vec()));
    assert_eq!(r.status, 412);
    let r = s.send("PUT", &format!("/api/items/{id}/content"), &[("Authorization", &auth), ("If-Match", &etag)], Some(b"new".to_vec()));
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["version"], 2);
    // Someone without edit access can't make the server store anything.
    let ct = s.token("carol");
    assert_eq!(s.send("POST", &path, &[("Authorization", &format!("Bearer {ct}"))], Some(vec![0u8; 10])).status, 404);
    assert_eq!(s.send("POST", &format!("/api/files?parent={root}&name=Big.bin&conflict=fail"), &[("Authorization", &auth)], Some(vec![0u8; 10])).status, 409);
    // Nothing is left staged.
    let incoming = s.cfg.data_dir.join("incoming");
    assert!(std::fs::read_dir(&incoming).map(|d| d.count()).unwrap_or(0) == 0, "staged uploads are cleaned up");
    // A restored version is a copy on disk.
    let r = s.api("POST", &format!("/api/items/{id}/versions/1/restore"), &t, json!({}));
    assert_eq!(r.status, 200, "{}", r.text());
    let r = s.send("GET", &format!("/api/items/{id}/content"), &[("Authorization", &auth)], None);
    assert_eq!(r.body.len(), 8 * 64 * 1024);
    assert_eq!(s.audit(&s.token("alice"), "action=item.version_restore").len(), 1);
}

const DRIVE_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

fn assert_drive_headers(r: &Resp, what: &str) {
    assert_eq!(r.header("content-security-policy"), Some(DRIVE_CSP), "{what}");
    assert_eq!(r.header("x-content-type-options"), Some("nosniff"), "{what}");
    assert_eq!(r.header("referrer-policy"), Some("same-origin"), "{what}");
    assert_eq!(r.header("x-frame-options"), Some("DENY"), "{what}");
    assert!(r.header("access-control-allow-origin").is_none(), "{what}");
}

#[test]
fn the_drive_app_is_served_with_strict_headers() {
    let s = start();
    let v = freedomoffice_drive::version();
    for path in ["/drive", "/drive/"] {
        let r = s.send("GET", path, &[], None);
        assert_eq!(r.status, 200, "{path}");
        assert_drive_headers(&r, path);
        assert_eq!(r.header("content-type"), Some("text/html; charset=utf-8"));
        assert_eq!(r.header("cache-control"), Some("no-store"));
        let html = r.text();
        assert!(html.contains(&format!("/drive/assets/app.js?v={v}")), "{html}");
        assert!(!html.contains("{{v}}"));
    }
    for (file, ct) in [("app.js", "text/javascript; charset=utf-8"), ("style.css", "text/css; charset=utf-8"), ("icon.svg", "image/svg+xml")] {
        let r = s.send("GET", &format!("/drive/assets/{file}?v={v}"), &[], None);
        assert_eq!(r.status, 200, "{file}");
        assert_drive_headers(&r, file);
        assert_eq!(r.header("content-type"), Some(ct), "{file}");
        assert_eq!(r.header("cache-control"), Some("public, max-age=31536000, immutable"), "{file}");
        assert_eq!(r.body, freedomoffice_drive::asset(file).unwrap().0, "{file}");
        // Another (older) version, or none: only briefly cached.
        let r = s.send("GET", &format!("/drive/assets/{file}?v=000000000000"), &[], None);
        assert_eq!(r.header("cache-control"), Some("public, max-age=300"), "{file}");
        let r = s.send("GET", &format!("/drive/assets/{file}"), &[], None);
        assert_eq!(r.header("cache-control"), Some("public, max-age=300"), "{file}");
        assert_eq!(s.send("HEAD", &format!("/drive/assets/{file}"), &[], None).status, 200, "{file}");
    }
    for bad in ["/drive/assets/nope.js", "/drive/assets/index.html", "/drive/assets/%2e%2e/Cargo.toml", "/drive/assets/app.js/x", "/drive/assets/APP.JS", "/drive/assets/"] {
        let r = s.send("GET", bad, &[], None);
        assert_eq!(r.status, 404, "{bad}");
    }
    let r = s.send("GET", "/drive/assets/nope.js", &[], None);
    assert_drive_headers(&r, "unknown asset");
    assert_eq!(r.header("cache-control"), Some("no-store"));
    // Not a /drive answer: the API keeps its own headers.
    assert!(s.send("GET", "/api/health", &[], None).header("content-security-policy").is_none());
}

#[test]
fn me_tells_the_drive_app_its_storage_and_limits() {
    let s = start_with(|c| {
        c.max_upload_mb = 7;
        c.allow_links = false;
    });
    let w = s.web("bob");
    let root = w.get("/api/me").json()["root"].as_str().unwrap().to_string();
    let token = s.token("bob");
    assert_eq!(s.upload(&token, &root, "a.txt", b"hello").status, 201);
    let me = w.get("/api/me").json();
    assert_eq!(me["server"], json!({ "max_upload_mb": 7, "allow_links": false }));
    assert_eq!(me["storage"]["files"], 1);
    assert_eq!(me["storage"]["bytes"], 5);
}

// ---------------------------------------------------------------------------
// Review fixes: audit flooding, typed passwords, limits, logout, upload limits
// ---------------------------------------------------------------------------

#[test]
fn repeated_events_are_counted_not_stored_one_by_one() {
    let s = start();
    let at = s.token("alice");
    let bt = s.token("bob");
    let root = s.root(&bt);
    let id = s.upload(&bt, &root, "Loop.txt", b"x").json()["id"].as_str().unwrap().to_string();
    for i in 0..60 {
        let name = format!("{}{i}.txt", "n".repeat(230));
        assert_eq!(s.api("PATCH", &format!("/api/items/{id}"), &bt, json!({ "name": name })).status, 200);
    }
    let renames = s.audit(&at, "action=item.rename");
    assert_eq!(renames.len(), 1, "one row a minute per person and item");
    assert_eq!(renames[0]["count"], 60);
    assert!(renames[0]["detail"].as_str().unwrap().chars().count() <= 200, "detail is capped");
    assert!(renames[0]["detail"].as_str().unwrap().starts_with("Loop.txt -> "), "the first rename");
    let last = renames[0]["last_detail"].as_str().unwrap();
    assert!(last.starts_with("nnn") && last.chars().count() <= 200, "and the last one: {last}");
    // New versions: counted the same way.
    let auth = format!("Bearer {bt}");
    for _ in 0..3 {
        assert_eq!(s.send("PUT", &format!("/api/items/{id}/content"), &[("Authorization", &auth)], Some(b"v".to_vec())).status, 200);
    }
    let versions = s.audit(&at, "action=file.version");
    assert_eq!((versions.len(), versions[0]["count"].as_i64()), (1, Some(3)));
    // New folders are recorded, counted per person and parent folder.
    for i in 0..5 {
        assert_eq!(s.api("POST", "/api/folders", &bt, json!({ "name": format!("Archive {i}") })).status, 201);
    }
    let folders = s.audit(&at, "action=folder.create");
    assert_eq!((folders.len(), folders[0]["count"].as_i64()), (1, Some(5)));
    assert!(folders[0]["last_detail"].as_str().unwrap().starts_with("Archive 4"));
    // The admin page shows the count and the last detail.
    let r = s.send("POST", "/admin/login", &[("Content-Type", "application/x-www-form-urlencoded"), ("Origin", ORIGIN)], Some(format!("username=alice&password={PW}").into_bytes()));
    let cookie = r.header("set-cookie").unwrap().split(';').next().unwrap().to_string();
    let page = s.send("GET", "/admin/audit?action=folder.create", &[("Cookie", &cookie)], None).text();
    assert!(page.contains("x5 until") && page.contains("last: Archive 4"), "{page}");
}

#[test]
fn failed_sign_ins_never_keep_unknown_names() {
    let s = start();
    let at = s.token("alice");
    // Someone types their password into the username field, then random names follow.
    let _ = s.web_login_raw("MySecretPass99", "whatever-123", &[]);
    for i in 0..8 {
        let _ = s.web_login_raw(&format!("nobody{i}"), "whatever-123", &[]);
    }
    let _ = s.web_login_raw("bob", "wrong-password-1", &[]);
    let failed = s.audit(&at, "action=sign_in.failed");
    let dump = serde_json::to_string(&failed).unwrap();
    assert!(!dump.contains("MySecretPass99") && !dump.contains("nobody"), "{dump}");
    let unknown: Vec<&Value> = failed.iter().filter(|e| e["target_user"] == "").collect();
    assert_eq!(unknown.len(), 1, "one row for every unknown name from this address: {failed:?}");
    assert_eq!(unknown[0]["count"], 9, "(the address limit stops the 11th attempt before anything is recorded)");
    assert!(unknown[0]["detail"].as_str().unwrap().contains("unknown username"));
    assert!(failed.iter().any(|e| e["target_user"] == "bob" && e["count"] == 1));
}

#[test]
fn ipv6_neighbours_share_one_sign_in_limit() {
    let s = start();
    let login = |ip: &str, user: &str| {
        s.send("POST", "/api/login", &[("Content-Type", "application/json"), ("X-Forwarded-For", ip)], Some(json!({ "username": user, "password": "wrong-password-1" }).to_string().into_bytes())).status
    };
    for i in 1..=10 {
        assert_eq!(login(&format!("2001:db8:1:2::{i:x}"), &format!("guess{i}")), 401);
    }
    assert_eq!(login("2001:db8:1:2:ffff::1", "guess99"), 429, "the same /64");
    assert_eq!(login("2001:db8:1:3::1", "guess99"), 401, "another /64");
}

#[test]
fn logout_clears_the_cookie_and_sign_outs_are_audited() {
    let s = start();
    let at = s.token("alice");
    let w = s.web("bob");
    let r = w.call("POST", "/api/logout", json!({}));
    assert_eq!(r.status, 204);
    assert!(r.header("set-cookie").unwrap().starts_with(&format!("{COOKIE}=;")), "the cookie is cleared");
    let t = s.token("carol");
    let r = s.api("POST", "/api/logout", &t, json!({}));
    assert_eq!(r.status, 204);
    assert!(r.header("set-cookie").is_none(), "no cookie for a device");
    // The admin page's sign-out too.
    let r = s.send("POST", "/admin/login", &[("Content-Type", "application/x-www-form-urlencoded"), ("Origin", ORIGIN)], Some(format!("username=alice&password={PW}").into_bytes()));
    let cookie = r.header("set-cookie").unwrap().split(';').next().unwrap().to_string();
    let page = s.send("GET", "/admin/password", &[("Cookie", &cookie)], None).text();
    let at_csrf = page.find("name=\"csrf\" value=\"").unwrap() + "name=\"csrf\" value=\"".len();
    let csrf = page[at_csrf..at_csrf + page[at_csrf..].find('"').unwrap()].to_string();
    let r = s.send("POST", "/admin/logout", &[("Cookie", &cookie), ("Origin", ORIGIN), ("Content-Type", "application/x-www-form-urlencoded")], Some(format!("csrf={csrf}").into_bytes()));
    assert_eq!(r.status, 303);
    let outs: Vec<(String, String)> = s.audit(&at, "action=sign_out").iter().map(|e| (e["target_user"].as_str().unwrap().to_string(), e["detail"].as_str().unwrap().to_string())).collect();
    for want in [("bob", "web"), ("carol", "app"), ("alice", "admin")] {
        assert!(outs.contains(&(want.0.to_string(), want.1.to_string())), "{want:?} in {outs:?}");
    }
}

/// Starts a chunked upload and sends one chunk, leaving the connection open.
fn open_upload(s: &Srv, token: &str, path: &str) -> TcpStream {
    let mut c = TcpStream::connect(s.url.trim_start_matches("http://")).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let head = format!("POST {path} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nabcd\r\n");
    c.write_all(head.as_bytes()).unwrap();
    c
}

fn status_of(mut c: TcpStream) -> u16 {
    let mut out = Vec::new();
    let _ = c.read_to_end(&mut out);
    String::from_utf8_lossy(&out).split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0)
}

#[test]
fn uploads_are_limited_per_person_stall_and_disk() {
    let s = start_with(|c| c.upload_idle_ms = 1500);
    let t = s.token("bob");
    let root = s.root(&t);
    let open: Vec<TcpStream> = (0..4).map(|i| open_upload(&s, &t, &format!("/api/files?parent={root}&name=Slow{i}.bin"))).collect();
    std::thread::sleep(Duration::from_millis(300));
    let r = s.upload(&t, &root, "Fifth.bin", b"x");
    assert_eq!(r.status, 429, "{}", r.text());
    let ct = s.token("carol");
    assert_eq!(s.upload(&ct, &s.root(&ct), "Other.bin", b"x").status, 201, "per person");
    // The stalled uploads are dropped and leave nothing behind.
    for c in open {
        assert_eq!(status_of(c), 408);
    }
    let incoming = s.cfg.data_dir.join("incoming");
    assert_eq!(std::fs::read_dir(&incoming).map(|d| d.count()).unwrap_or(0), 0);
    assert_eq!(s.upload(&t, &root, "Fifth.bin", b"x").status, 201, "the slots came back");
    drop(s);
    // Too little disk space would be left: 507 before anything is stored.
    let s = start_with(|c| c.upload_reserve_mb = u64::MAX / (2 << 20));
    let t = s.token("bob");
    let r = s.upload(&t, &s.root(&t), "Full.bin", b"x");
    assert_eq!(r.status, 507, "{}", r.text());
}

#[test]
fn the_cli_erases_audit_events() {
    let s = start();
    let at = s.token("alice");
    let _ = s.web_login_raw("bob", "wrong-password-1", &[]);
    assert!(s.audit(&at, "").len() >= 2);
    let cfg_file = s.dir.join("server.toml");
    std::fs::write(&cfg_file, format!("data_dir = {:?}\n", s.cfg.data_dir.to_str().unwrap())).unwrap();
    let bin = env!("CARGO_BIN_EXE_freedomoffice-server");
    let out = std::process::Command::new(bin).args(["--config", cfg_file.to_str().unwrap(), "audit-prune", "--before", "2999-01-01", "--user", "bob"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let left = s.audit(&at, "limit=200");
    assert!(left.iter().all(|e| e["actor"] != "bob" && e["target_user"] != "bob"), "{left:?}");
    assert!(left.iter().any(|e| e["action"] == "audit.erase"));
    let bad = std::process::Command::new(bin).args(["--config", cfg_file.to_str().unwrap(), "audit-prune", "--before", "2026-02-30"]).output().unwrap();
    assert!(!bad.status.success());
}
