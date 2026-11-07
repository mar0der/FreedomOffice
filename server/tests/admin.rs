//! The web admin page (`/admin`) end to end: a real server on 127.0.0.1 with a throwaway data
//! directory. The server's public URL is https (so the cookie is `__Host-fo_admin; Secure`);
//! the tests talk plain http to it and send the cookie and Origin headers themselves, as a
//! browser behind the TLS proxy would.

use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use cosync::ServerMsg;
use freedomoffice_server::config::Config;
use freedomoffice_server::store::Store;
use freedomoffice_server::{spawn, Running};
use serde_json::{json, Value};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// Test-only password for the throwaway accounts below.
const PW: &str = "test-password-1";
const ORIGIN: &str = "https://admin.example.test";
const COOKIE: &str = "__Host-fo_admin";

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
    body: String,
    headers: Vec<(String, String)>,
}

impl Resp {
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
    fn location(&self) -> &str {
        self.header("location").unwrap_or("")
    }
    /// The first `<p class="secret">` (a temporary password or invite link shown once).
    fn secret(&self) -> String {
        let start = self.body.find("<p class=\"secret\">").expect("a secret on the page") + "<p class=\"secret\">".len();
        let end = self.body[start..].find("</p>").unwrap() + start;
        self.body[start..end].to_string()
    }
}

fn start() -> Srv {
    let dir = std::env::temp_dir().join(format!("fo-admin-test-{}", freedomoffice_server::util::new_id()));
    let cfg = Config { data_dir: dir.join("data"), login_delay_ms: 10, public_url: ORIGIN.into(), ..Config::default() };
    {
        let s = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
        s.create_user("alice", "Alice Admin", "alice@example.test", PW, true).unwrap();
        s.create_user("bob", "Bob Builder", "bob@example.test", PW, false).unwrap();
        s.create_user("carol", "Carol C", "", PW, false).unwrap();
        s.create_user("dave", "Dave D", "", PW, false).unwrap();
    }
    let run = spawn(cfg.clone()).unwrap();
    let url = run.url();
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).timeout_global(Some(Duration::from_secs(30))).build().into();
    Srv { run: Some(run), url, dir, cfg, agent }
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
        let body = res.body_mut().read_to_string().unwrap_or_default();
        Resp { status, body, headers }
    }

    fn api(&self, method: &str, path: &str, token: Option<&str>, body: Value) -> Resp {
        let auth = token.map(|t| format!("Bearer {t}"));
        let mut h = vec![("Content-Type", "application/json")];
        if let Some(a) = &auth {
            h.push(("Authorization", a));
        }
        self.send(method, path, &h, Some(body.to_string().into_bytes()))
    }

    fn token(&self, user: &str, password: &str, device: &str) -> String {
        let r = self.api("POST", "/api/login", None, json!({ "username": user, "password": password, "device": device }));
        assert_eq!(r.status, 200, "{}", r.body);
        r.json()["token"].as_str().unwrap().to_string()
    }

    fn org(&self, admin: &str, name: &str) -> String {
        let r = self.api("POST", "/api/orgs", Some(admin), json!({ "name": name }));
        assert_eq!(r.status, 201, "{}", r.body);
        r.json()["id"].as_str().unwrap().to_string()
    }

    fn member(&self, admin: &str, org: &str, user: &str, role: &str) {
        let r = self.api("POST", &format!("/api/orgs/{org}/members"), Some(admin), json!({ "username": user, "role": role }));
        assert_eq!(r.status, 201, "{}", r.body);
    }

    /// Posts the admin sign-in form; the response.
    fn sign_in_form(&self, user: &str, password: &str) -> Resp {
        self.send("POST", "/admin/login", &[("Content-Type", "application/x-www-form-urlencoded"), ("Origin", ORIGIN)], Some(form(&[("username", user), ("password", password)])))
    }

    fn browser(&self, user: &str) -> Browser<'_> {
        self.browser_with(user, PW)
    }

    fn browser_with(&self, user: &str, password: &str) -> Browser<'_> {
        let r = self.sign_in_form(user, password);
        assert_eq!(r.status, 303, "sign-in of {user}: {}", r.body);
        let set = r.header("set-cookie").unwrap();
        let secret = set.strip_prefix(&format!("{COOKIE}=")).unwrap().split(';').next().unwrap().to_string();
        Browser { s: self, cookie: format!("{COOKIE}={secret}"), secret }
    }
}

fn enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn form(fields: &[(&str, &str)]) -> Vec<u8> {
    fields.iter().map(|(k, v)| format!("{k}={}", enc(v))).collect::<Vec<_>>().join("&").into_bytes()
}

/// A signed-in browser: sends the session cookie and the server's Origin.
struct Browser<'a> {
    s: &'a Srv,
    cookie: String,
    secret: String,
}

impl Browser<'_> {
    fn get(&self, path: &str) -> Resp {
        self.s.send("GET", path, &[("Cookie", &self.cookie)], None)
    }

    /// The CSRF token from the forms of a page.
    fn csrf(&self) -> String {
        let page = self.get("/admin/password");
        let at = page.body.find("name=\"csrf\" value=\"").expect("a form with a CSRF token") + "name=\"csrf\" value=\"".len();
        page.body[at..at + page.body[at..].find('"').unwrap()].to_string()
    }

    fn post_raw(&self, path: &str, headers: &[(&str, &str)], fields: &[(&str, &str)]) -> Resp {
        let mut h = vec![("Cookie", self.cookie.as_str()), ("Content-Type", "application/x-www-form-urlencoded")];
        h.extend_from_slice(headers);
        self.s.send("POST", path, &h, Some(form(fields)))
    }

    /// Posts a form as the page would (with the CSRF token and the right Origin).
    fn post(&self, path: &str, fields: &[(&str, &str)]) -> Resp {
        let csrf = self.csrf();
        let mut f = vec![("csrf", csrf.as_str())];
        f.extend_from_slice(fields);
        self.post_raw(path, &[("Origin", ORIGIN)], &f)
    }
}

#[test]
fn sign_in_gives_a_separate_hardened_session() {
    let s = start();
    // Not signed in: to the sign-in page.
    let r = s.send("GET", "/admin", &[], None);
    assert_eq!(r.status, 303);
    assert_eq!(r.location(), "/admin/login");
    let r = s.send("GET", "/admin/login", &[], None);
    assert_eq!(r.status, 200);
    assert_eq!(
        r.header("content-security-policy"),
        Some("default-src 'none'; style-src 'self'; img-src 'self' data:; form-action 'self'; frame-ancestors 'none'; base-uri 'none'")
    );
    assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(r.header("referrer-policy"), Some("same-origin"));
    assert_eq!(r.header("cache-control"), Some("no-store"));
    assert!(!r.body.contains("<script"), "no scripts at all");
    assert!(!r.body.contains("style="), "no inline styles (the CSP refuses them)");
    let css = s.send("GET", "/admin/style.css", &[], None);
    assert_eq!(css.status, 200);
    assert!(css.header("content-type").unwrap().starts_with("text/css"));
    assert!(css.body.contains("prefers-color-scheme:dark"));

    // A wrong password: the form again, nothing set.
    let r = s.sign_in_form("alice", "wrong-password-123");
    assert_eq!(r.status, 401);
    assert!(r.header("set-cookie").is_none());
    assert!(r.body.contains("The username or password is incorrect."));

    // The cookie and its flags.
    let r = s.sign_in_form("alice", PW);
    assert_eq!(r.status, 303);
    assert_eq!(r.location(), "/admin");
    let set = r.header("set-cookie").unwrap().to_string();
    assert!(set.starts_with(&format!("{COOKIE}=")), "{set}");
    for flag in ["; Path=/", "; HttpOnly", "; Secure", "; SameSite=Strict", "; Max-Age=43200"] {
        assert!(set.contains(flag), "{flag} missing in {set}");
    }
    assert!(!set.contains("Domain="), "__Host- cookies have no Domain");
    let secret = set.strip_prefix(&format!("{COOKIE}=")).unwrap().split(';').next().unwrap().to_string();
    let b = Browser { s: &s, cookie: format!("{COOKIE}={secret}"), secret: secret.clone() };
    let r = b.get("/admin");
    assert_eq!(r.status, 200);
    assert!(r.body.contains("Overview") && r.body.contains("Alice Admin"));
    assert_eq!(r.header("cache-control"), Some("no-store"));

    // Stored hashed; never a bearer token for the API; the cookie of another name is ignored.
    let st = s.store();
    let stored: Vec<u8> = st.conn.query_row("SELECT hash FROM admin_sessions", [], |r| r.get(0)).unwrap();
    assert_eq!(stored, freedomoffice_server::util::token_hash(&secret));
    assert_ne!(stored, secret.as_bytes());
    assert_eq!(s.send("GET", "/api/me", &[("Authorization", &format!("Bearer {secret}"))], None).status, 401);
    assert_eq!(s.send("GET", "/admin", &[("Cookie", &format!("fo_admin={secret}"))], None).status, 303);

    // Signing out ends the session.
    let r = b.post("/admin/logout", &[]);
    assert_eq!(r.status, 303);
    assert_eq!(r.location(), "/admin/login");
    assert!(r.header("set-cookie").unwrap().contains("Max-Age=0"));
    let r = b.get("/admin");
    assert_eq!(r.status, 303);
    assert_eq!(r.location(), "/admin/login");
}

#[test]
fn admin_sign_in_shares_the_api_limiter() {
    let s = start();
    for _ in 0..10 {
        assert_eq!(s.sign_in_form("bob", "wrong-password-123").status, 401);
    }
    // Locked for the right password too, here and in the API.
    let r = s.sign_in_form("bob", PW);
    assert_eq!(r.status, 429);
    assert!(r.body.contains("Too many failed attempts"));
    assert_eq!(s.api("POST", "/api/login", None, json!({ "username": "bob", "password": PW })).status, 429);
    // Text that can't be a username is refused before the limiter and Argon2.
    let r = s.sign_in_form("<script>", PW);
    assert_eq!(r.status, 429, "the address is limited as well by now");
}

#[test]
fn forms_need_the_csrf_token_and_the_same_origin() {
    let s = start();
    let a = s.browser("alice");
    let csrf = a.csrf();
    assert_eq!(csrf.len(), 43);
    // No token, a wrong one, another session's.
    assert_eq!(a.post_raw("/admin/orgs", &[("Origin", ORIGIN)], &[("name", "Team")]).status, 403);
    assert_eq!(a.post_raw("/admin/orgs", &[("Origin", ORIGIN)], &[("csrf", "x"), ("name", "Team")]).status, 403);
    let other = s.browser("alice").csrf();
    assert_ne!(other, csrf);
    assert_eq!(a.post_raw("/admin/orgs", &[("Origin", ORIGIN)], &[("csrf", &other), ("name", "Team")]).status, 403);
    // The right token from another site, or with an opaque origin.
    for origin in ["https://evil.example", "null", "http://admin.example.test"] {
        let r = a.post_raw("/admin/orgs", &[("Origin", origin)], &[("csrf", &csrf), ("name", "Team")]);
        assert_eq!(r.status, 403, "{origin}");
        assert!(r.body.contains("another site"));
    }
    let r = a.post_raw("/admin/orgs", &[("Referer", "https://evil.example/page")], &[("csrf", &csrf), ("name", "Team")]);
    assert_eq!(r.status, 403);
    // Signing in from another site.
    let r = s.send("POST", "/admin/login", &[("Origin", "https://evil.example")], Some(form(&[("username", "alice"), ("password", PW)])));
    assert_eq!(r.status, 403);
    assert!(r.header("set-cookie").is_none());
    // Nothing was created.
    assert!(s.store().orgs().unwrap().is_empty());
    // The page's own form works (Origin or Referer of this server).
    let r = a.post_raw("/admin/orgs", &[("Referer", &format!("{ORIGIN}/admin/orgs"))], &[("csrf", &csrf), ("name", "Team")]);
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(r.location().starts_with("/admin/orgs/") && r.location().ends_with("?m=created"));
    // Without a session a form goes to the sign-in page.
    let r = s.send("POST", "/admin/orgs", &[("Origin", ORIGIN)], Some(form(&[("csrf", &csrf), ("name", "Other")])));
    assert_eq!(r.status, 303);
    assert_eq!(r.location(), "/admin/login");
}

#[test]
fn org_admins_see_only_their_orgs() {
    let s = start();
    let a = s.token("alice", PW, "test");
    let team = s.org(&a, "Team");
    let other = s.org(&a, "Other");
    s.member(&a, &team, "bob", "admin");
    s.member(&a, &team, "carol", "member");
    s.member(&a, &other, "carol", "member");
    s.member(&a, &other, "bob", "member");

    let b = s.browser("bob");
    let home = b.get("/admin");
    assert_eq!(home.status, 200);
    assert!(home.body.contains("Team") && !home.body.contains("Other"), "{}", home.body);
    assert!(!home.body.contains("/admin/people"), "no People link for org admins");
    assert_eq!(b.get("/admin/people").status, 403);
    assert_eq!(b.get("/admin/people/carol").status, 403);
    // A member, not an admin, of Other: no admin page for it; not a member at all: not found.
    assert_eq!(b.get(&format!("/admin/orgs/{other}")).status, 403);
    let none = s.org(&a, "Hidden");
    assert_eq!(b.get(&format!("/admin/orgs/{none}")).status, 404);
    let page = b.get(&format!("/admin/orgs/{team}"));
    assert_eq!(page.status, 200);
    assert!(page.body.contains("Carol C"));
    assert!(!page.body.contains("bob@example.test"), "emails are for the server administrator only");
    assert!(!page.body.contains("Delete this organisation") && !page.body.contains("Rename"));
    // Bob can't touch the other org or make anyone an owner.
    assert_eq!(b.post(&format!("/admin/orgs/{other}/settings"), &[("member_role", "view")]).status, 403);
    assert_eq!(b.post(&format!("/admin/orgs/{team}/members/carol/role"), &[("role", "owner")]).status, 403);
    assert_eq!(b.post(&format!("/admin/orgs/{team}/rename"), &[("name", "Mine")]).status, 403);
    assert_eq!(b.post("/admin/orgs", &[("name", "Bobs")]).status, 403);
    assert_eq!(b.post("/admin/people/carol/disable", &[]).status, 403);
    // Same rules as the API: dave shares no org with bob, so he needs an invite.
    let r = b.post(&format!("/admin/orgs/{team}/members"), &[("username", "dave"), ("role", "member")]);
    assert_eq!(r.status, 403);
    assert!(r.body.contains("send them an invite instead"));
    // Within his org he manages: roles and the default role.
    assert_eq!(b.post(&format!("/admin/orgs/{team}/members/carol/role"), &[("role", "admin")]).status, 303);
    assert_eq!(b.post(&format!("/admin/orgs/{team}/settings"), &[("member_role", "view")]).status, 303);
    let st = s.store();
    let o = st.org(&team).unwrap().unwrap();
    assert_eq!(o.member_role.as_str(), "view");

    // Dave administers nothing: a note and his own password form.
    let d = s.browser("dave");
    let home = d.get("/admin");
    assert_eq!(home.status, 200);
    assert!(home.body.contains("You don't administer anything"));
    assert!(home.body.contains("action=\"/admin/password\""));
    assert!(!home.body.contains("/admin/orgs\""));
    assert_eq!(d.get(&format!("/admin/orgs/{team}")).status, 404);
}

#[test]
fn super_admin_flows() {
    let s = start();
    let a = s.browser("alice");
    // Create an organisation.
    let r = a.post("/admin/orgs", &[("name", "Family")]);
    assert_eq!(r.status, 303);
    let org = r.location().trim_start_matches("/admin/orgs/").split('?').next().unwrap().to_string();
    let page = a.get(&format!("/admin/orgs/{org}?m=created"));
    assert!(page.body.contains("The organisation was created."));

    // Create an account: the temporary password is shown once and must be changed.
    let r = a.post(&format!("/admin/orgs/{org}/accounts"), &[("username", "erin"), ("display_name", "Erin E"), ("email", ""), ("role", "member")]);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.header("cache-control"), Some("no-store"));
    let temp = r.secret();
    assert_eq!(temp.len(), 24);
    assert!(!a.get(&format!("/admin/orgs/{org}")).body.contains(&temp), "shown only once");
    let e = s.browser_with("erin", &temp);
    let r = e.get("/admin");
    assert_eq!(r.status, 303);
    assert_eq!(r.location(), "/admin/password", "a temporary password goes to the change first");
    assert_eq!(e.get("/admin/orgs").location(), "/admin/password");
    let r = e.get("/admin/password");
    assert!(r.body.contains("temporary password"));
    let new_pw = "own-secret-words-9";
    let r = e.post("/admin/password", &[("old_password", &temp), ("new_password", new_pw), ("new_password2", "different-one-123")]);
    assert_eq!(r.status, 400);
    let r = e.post("/admin/password", &[("old_password", &temp), ("new_password", new_pw), ("new_password2", new_pw)]);
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(r.location(), "/admin?m=password");
    assert_eq!(e.get("/admin").status, 200, "the session that changed it stays");
    assert_eq!(s.api("POST", "/api/login", None, json!({ "username": "erin", "password": temp })).status, 401, "the temporary password worked once");
    s.token("erin", new_pw, "laptop");

    // Invite link: shown once with a mailto link, works once, can be revoked.
    let r = a.post(&format!("/admin/orgs/{org}/invites"), &[("email", "frank@example.test"), ("role", "member")]);
    assert_eq!(r.status, 200, "{}", r.body);
    let url = r.secret();
    assert!(url.starts_with(&format!("{ORIGIN}/i/")), "{url}");
    assert!(r.body.contains("href=\"mailto:frank@example.test?subject=Join%20Family%20on%20FreedomOffice&amp;body="), "{}", r.body);
    let token = url.rsplit('/').next().unwrap().to_string();
    assert!(r.body.contains(&enc(&url)), "the link is in the email text");
    let page = a.get(&format!("/admin/orgs/{org}"));
    assert!(page.body.contains("frank@example.test") && !page.body.contains(&token), "listed without the link");
    let r = s.send("POST", &format!("/i/{token}"), &[("Content-Type", "application/x-www-form-urlencoded")], Some(form(&[("username", "frank"), ("password", "silver-words-77x")])));
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(a.get(&format!("/admin/orgs/{org}")).body.contains("frank"));
    let r = s.send("POST", &format!("/i/{token}"), &[("Content-Type", "application/x-www-form-urlencoded")], Some(form(&[("username", "frank2"), ("password", "silver-words-77x")])));
    assert_eq!(r.status, 404, "used up");
    let r = a.post(&format!("/admin/orgs/{org}/invites"), &[("email", ""), ("role", "member")]);
    let token2 = r.secret().rsplit('/').next().unwrap().to_string();
    let inv = s.store().invites_of(&org).unwrap();
    assert_eq!(inv.len(), 1);
    assert_eq!(a.post(&format!("/admin/orgs/{org}/invites/{}/revoke", inv[0].id), &[]).status, 303);
    assert_eq!(s.send("GET", &format!("/i/{token2}"), &[], None).status, 404);

    // Members: role and removal.
    assert_eq!(a.post(&format!("/admin/orgs/{org}/members"), &[("username", "carol"), ("role", "admin")]).status, 303);
    assert_eq!(a.post(&format!("/admin/orgs/{org}/members/carol/role"), &[("role", "owner")]).status, 303);
    assert_eq!(a.post(&format!("/admin/orgs/{org}/members/carol/remove"), &[]).status, 303);
    // Delete only when empty: put a file in the folder first.
    let f = s.token("frank", "silver-words-77x", "test");
    let root = s.store().org(&org).unwrap().unwrap().root_id;
    let up = s.send("POST", &format!("/api/files?parent={root}&name=Plan.xlsx&conflict=fail"), &[("Authorization", &format!("Bearer {f}"))], Some(b"plan".to_vec()));
    assert_eq!(up.status, 201);
    let r = a.post(&format!("/admin/orgs/{org}/delete"), &[("confirm", "yes")]);
    assert_eq!(r.status, 409);
    assert!(r.body.contains("isn&#39;t empty"), "{}", r.body);
    let page = a.get("/admin");
    assert!(page.body.contains("<span class=\"label\">Storage</span><span class=\"value\">4 B</span>"), "{}", page.body);

    // People: the list, reset (once, signs out), sign-out everywhere.
    let people = a.get("/admin/people");
    assert_eq!(people.status, 200);
    assert!(people.body.contains("Erin E") && people.body.contains("Family (member)") || people.body.contains("Family <span class=\"muted\">(member)</span>"));
    let r = a.post("/admin/people/erin/reset-password", &[]);
    assert_eq!(r.status, 200, "{}", r.body);
    let temp2 = r.secret();
    assert_ne!(temp2, temp);
    assert_eq!(e.get("/admin").status, 303, "a reset ends the admin session too");
    assert_eq!(s.api("POST", "/api/login", None, json!({ "username": "erin", "password": new_pw })).status, 401);
    let t = s.token("erin", &temp2, "phone");
    assert_eq!(s.api("GET", "/api/root", Some(&t), json!({})).status, 403, "the reset password must be changed");
    assert_eq!(a.post("/admin/people/erin/sign-out", &[]).status, 303);
    assert_eq!(s.api("GET", "/api/me", Some(&t), json!({})).status, 401);

    // The last server administrator can't step down.
    let r = a.post("/admin/people/alice/admin", &[("on", "0")]);
    assert_eq!(r.status, 409);
    assert!(r.body.contains("last server administrator"));
    assert_eq!(a.post("/admin/people/bob/admin", &[("on", "1")]).status, 303);
    assert!(s.browser("bob").get("/admin/people").status == 200);
    let r = a.post("/admin/people/alice/admin", &[("on", "0")]);
    assert_eq!(r.status, 303);
    assert_eq!(a.get("/admin/people").status, 403, "alice is no longer a server administrator");
}

#[test]
fn sign_ins_are_listed_and_revoked() {
    let s = start();
    let a = s.browser("alice");
    let phone = s.token("bob", PW, "Bob's phone");
    let _laptop = s.token("bob", PW, "Bob's laptop");
    let bw = s.browser("bob");
    let page = a.get("/admin/people/bob");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("Bob&#39;s phone") && page.body.contains("Bob&#39;s laptop") && page.body.contains("admin page"));
    let st = s.store();
    let bob = st.user_by_name("bob").unwrap().unwrap();
    let id = st.tokens_of(&bob.id).unwrap().into_iter().find(|t| t.device == "Bob's phone").unwrap().id;
    assert_eq!(a.post(&format!("/admin/people/bob/sessions/device/{id}/revoke"), &[]).status, 303);
    assert_eq!(s.api("GET", "/api/me", Some(&phone), json!({})).status, 401);
    assert_eq!(a.post(&format!("/admin/people/bob/sessions/device/{id}/revoke"), &[]).status, 404, "gone already");
    // A session of somebody else can't be ended through bob's page.
    let alice = st.user_by_name("alice").unwrap().unwrap();
    let mine = st.admin_sessions_of(&alice.id).unwrap()[0].id;
    assert_eq!(a.post(&format!("/admin/people/bob/sessions/web/{mine}/revoke"), &[]).status, 404);
    let web = st.admin_sessions_of(&bob.id).unwrap()[0].id;
    assert_eq!(a.post(&format!("/admin/people/bob/sessions/web/{web}/revoke"), &[]).status, 303);
    assert_eq!(bw.get("/admin").status, 303);
    // The same through the API.
    let at = s.token("alice", PW, "test");
    let r = s.api("GET", "/api/users/bob/sessions", Some(&at), json!({}));
    assert_eq!(r.json()["devices"].as_array().unwrap().len(), 1);
    assert_eq!(s.api("GET", "/api/users/bob/sessions", Some(&phone), json!({})).status, 401);
    // A password change through the API ends the other web sessions as well.
    let bw = s.browser("bob");
    let bt = s.token("bob", PW, "test");
    let r = s.api("POST", "/api/me/password", Some(&bt), json!({ "old_password": PW, "new_password": "a-fresh-secret-44" }));
    assert_eq!(r.status, 200);
    assert_eq!(bw.get("/admin").status, 303);
    let _ = bw.secret;
}

#[test]
fn user_text_is_escaped() {
    let s = start();
    let a = s.browser("alice");
    // An organisation can't even be named <script>.
    let r = a.post("/admin/orgs", &[("name", "<script>")]);
    assert_eq!(r.status, 400);
    assert!(!r.body.contains("<script>"));
    let at = s.token("alice", PW, "test");
    let org = s.org(&at, "Tom & Jerry's");
    let r = s.api("POST", "/api/users", Some(&at), json!({ "username": "mallory", "display_name": "<script>alert(1)</script>", "email": "\"><img src=x onerror=alert(2)>@x.test", "org": org }));
    assert_eq!(r.status, 201, "{}", r.body);
    let temp = r.json()["temporary_password"].as_str().unwrap().to_string();
    let t = s.token("mallory", &temp, "<img src=x onerror=alert(3)>");
    s.api("POST", "/api/me/password", Some(&t), json!({ "old_password": temp, "new_password": "plain-secret-words-1" }));
    s.token("mallory", "plain-secret-words-1", "<b>bold</b>");
    for path in ["/admin", "/admin/orgs", &format!("/admin/orgs/{org}"), "/admin/people", "/admin/people/mallory"] {
        let r = a.get(path);
        assert_eq!(r.status, 200, "{path}");
        for raw in ["<script>alert", "<img src=x", "<b>bold", "Tom & Jerry's"] {
            assert!(!r.body.contains(raw), "{raw} unescaped on {path}");
        }
    }
    assert!(a.get("/admin/people").body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(a.get("/admin/people/mallory").body.contains("&lt;b&gt;bold&lt;/b&gt;"));
    assert!(a.get(&format!("/admin/orgs/{org}")).body.contains("Tom &amp; Jerry&#39;s"));
    // Unknown users in the URL are escaped too.
    let r = a.get("/admin/people/%3Cscript%3E");
    assert_eq!(r.status, 404);
    assert!(r.body.contains("&lt;script&gt;") && !r.body.contains("<script>"));
}

// ---------------------------------------------------------------------------
// Live session revoked when the admin page disables the account
// ---------------------------------------------------------------------------

struct Ws(WebSocket<MaybeTlsStream<TcpStream>>);

impl Ws {
    fn next(&mut self, wait: Duration) -> Option<Result<ServerMsg, Option<u16>>> {
        let end = Instant::now() + wait;
        while Instant::now() < end {
            match self.0.read() {
                Ok(Message::Text(t)) => return Some(Ok(serde_json::from_str(&t).unwrap())),
                Ok(Message::Close(f)) => return Some(Err(f.map(|f| f.code.into()))),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(_) => return Some(Err(None)),
            }
        }
        None
    }
}

#[test]
fn disabling_from_the_admin_page_ends_live_sessions() {
    let s = start();
    let at = s.token("alice", PW, "test");
    let bt = s.token("bob", PW, "test");
    let root = s.api("GET", "/api/root", Some(&at), json!({})).json()["id"].as_str().unwrap().to_string();
    let r = s.send("POST", &format!("/api/files?parent={root}&name=Live.xlsx&conflict=fail"), &[("Authorization", &format!("Bearer {at}"))], Some(b"version one".to_vec()));
    let id = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(s.api("POST", &format!("/api/items/{id}/permissions"), Some(&at), json!({ "username": "bob", "role": "edit" })).status, 200);
    let content = s.send("GET", &format!("/api/items/{id}/content"), &[("Authorization", &format!("Bearer {bt}"))], None);
    let seq = content.header("x-live-seq").unwrap().to_string();
    let url = format!("{}/api/items/{id}/live?since={seq}&v=1&cid=b", s.url.replace("http://", "ws://"));
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert("Authorization", format!("Bearer {bt}").parse().unwrap());
    let (ws, _) = tungstenite::connect(req).unwrap();
    if let MaybeTlsStream::Plain(t) = ws.get_ref() {
        t.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    }
    let mut ws = Ws(ws);
    assert!(matches!(ws.next(Duration::from_secs(10)), Some(Ok(ServerMsg::Welcome { .. }))));

    let a = s.browser("alice");
    let bw = s.browser("bob");
    let r = a.post("/admin/people/bob/disable", &[]);
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(r.location(), "/admin/people/bob?m=disabled");
    // By the time the answer arrived the live session was told.
    let reason = loop {
        match ws.next(Duration::from_secs(10)) {
            Some(Ok(ServerMsg::Revoked { reason })) => break reason,
            Some(Ok(_)) => {}
            other => panic!("expected revoked, got {:?}", other.map(|r| r.err())),
        }
    };
    assert_eq!(reason, "Your account was disabled.");
    assert_eq!(s.api("GET", "/api/me", Some(&bt), json!({})).status, 401);
    assert_eq!(bw.get("/admin").status, 303, "and the admin session");
    assert_eq!(s.sign_in_form("bob", PW).status, 401);
    // Nobody disables themselves; enabling works.
    assert_eq!(a.post("/admin/people/alice/disable", &[]).status, 400);
    assert_eq!(a.post("/admin/people/bob/enable", &[]).status, 303);
    s.token("bob", PW, "test");
}
