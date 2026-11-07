//! End-to-end API tests: a real server on 127.0.0.1 with a throwaway data directory and
//! throwaway accounts.

use std::path::PathBuf;
use std::time::Duration;

use freedomoffice_server::config::Config;
use freedomoffice_server::store::Store;
use freedomoffice_server::{spawn, Running};
use serde_json::{json, Value};

/// Test-only password for the throwaway accounts below.
const PW: &str = "test-password-1";

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
    etag: Option<String>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("fo-server-test-{}", freedomoffice_server::util::new_id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn start_with(f: impl FnOnce(&mut Config)) -> Srv {
    let dir = temp_dir();
    let mut cfg = Config { data_dir: dir.join("data"), login_delay_ms: 10, ..Config::default() };
    f(&mut cfg);
    {
        let s = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
        s.create_user("alice", "Alice Admin", "alice@example.test", PW, true).unwrap();
        s.create_user("bob", "Bob Builder", "", PW, false).unwrap();
        s.create_user("carol", "Carol C", "", PW, false).unwrap();
    }
    let run = spawn(cfg.clone()).unwrap();
    let url = run.url();
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(30))).build().into();
    Srv { run: Some(run), url, dir, cfg, agent }
}

fn start() -> Srv {
    start_with(|_| {})
}

impl Srv {
    fn store(&self) -> Store {
        Store::open(&self.cfg.db_path(), &self.cfg.files_dir(), self.cfg.versions_to_keep).unwrap()
    }

    fn send(&self, method: &str, path: &str, token: Option<&str>, headers: &[(&str, &str)], body: Option<Vec<u8>>) -> Resp {
        let url = if path.starts_with("http") { path.to_string() } else { format!("{}{path}", self.url) };
        let mut b = ureq::http::Request::builder().method(method).uri(url);
        if let Some(t) = token {
            b = b.header("Authorization", format!("Bearer {t}"));
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let res = match body {
            Some(data) => self.agent.run(b.body(data).unwrap()),
            None => self.agent.run(b.body(()).unwrap()),
        };
        let mut res = res.unwrap();
        let status = res.status().as_u16();
        let etag = res.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_string);
        let body = res.body_mut().with_config().limit(64 * 1024 * 1024).read_to_vec().unwrap();
        Resp { status, body, etag }
    }

    fn json(&self, method: &str, path: &str, token: Option<&str>, body: Value) -> Resp {
        self.send(method, path, token, &[("Content-Type", "application/json")], Some(body.to_string().into_bytes()))
    }

    fn get(&self, path: &str, token: &str) -> Resp {
        self.send("GET", path, Some(token), &[], None)
    }

    fn login(&self, user: &str) -> String {
        let r = self.json("POST", "/api/login", None, json!({ "username": user, "password": PW, "device": "test" }));
        assert_eq!(r.status, 200, "login {user}: {}", String::from_utf8_lossy(&r.body));
        r.json()["token"].as_str().unwrap().to_string()
    }

    fn root(&self, token: &str) -> String {
        self.get("/api/root", token).json()["id"].as_str().unwrap().to_string()
    }

    fn upload(&self, token: &str, parent: &str, name: &str, data: &[u8]) -> Resp {
        let path = format!("/api/files?parent={parent}&name={}&conflict=fail", enc(name));
        self.send("POST", &path, Some(token), &[("Content-Type", "application/octet-stream")], Some(data.to_vec()))
    }

    fn folder(&self, token: &str, parent: &str, name: &str) -> Resp {
        self.json("POST", "/api/folders", Some(token), json!({ "parent": parent, "name": name }))
    }

    fn put(&self, token: &str, id: &str, if_match: Option<&str>, data: &[u8]) -> Resp {
        let mut h = vec![("Content-Type", "application/octet-stream")];
        if let Some(e) = if_match {
            h.push(("If-Match", e));
        }
        self.send("PUT", &format!("/api/items/{id}/content"), Some(token), &h, Some(data.to_vec()))
    }
}

fn enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn id_of(r: &Resp) -> String {
    r.json()["id"].as_str().unwrap_or_else(|| panic!("no id in {}", String::from_utf8_lossy(&r.body))).to_string()
}

#[test]
fn health_login_logout() {
    let s = start();
    let r = s.send("GET", "/api/health", None, &[], None);
    assert_eq!((r.status, r.json()["status"].as_str()), (200, Some("ok")));
    assert_eq!(s.send("GET", "/api/me", None, &[], None).status, 401);
    assert_eq!(s.send("GET", "/api/me", Some("not-a-token"), &[], None).status, 401);
    let bad = s.json("POST", "/api/login", None, json!({ "username": "alice", "password": "wrong-password", "device": "x" }));
    assert_eq!(bad.status, 401);
    let unknown = s.json("POST", "/api/login", None, json!({ "username": "nobody", "password": PW, "device": "x" }));
    assert_eq!(unknown.status, 401);
    assert_eq!(bad.json()["error"], unknown.json()["error"]);
    // Usernames are case-insensitive.
    let r = s.json("POST", "/api/login", None, json!({ "username": "ALICE", "password": PW, "device": "Laptop" }));
    assert_eq!(r.status, 200);
    let v = r.json();
    let token = v["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 43);
    assert_eq!(v["user"]["username"], "alice");
    let me = s.get("/api/me", &token).json();
    assert_eq!((me["display_name"].as_str(), me["is_admin"].as_bool()), (Some("Alice Admin"), Some(true)));
    assert!(me.get("pw_hash").is_none());
    // Tokens are stored hashed.
    let st = s.store();
    let stored: Vec<u8> = st.conn.query_row("SELECT hash FROM tokens LIMIT 1", [], |r| r.get(0)).unwrap();
    assert_eq!(stored.len(), 32);
    assert_ne!(stored, token.as_bytes());
    assert_eq!(s.send("POST", "/api/logout", Some(&token), &[], None).status, 204);
    assert_eq!(s.get("/api/me", &token).status, 401);
    assert_eq!(s.get("/api/items/0123/live", &s.login("bob")).status, 404);
    assert_eq!(s.send("GET", "/nope", None, &[], None).status, 404);
}

#[test]
fn login_rate_limit() {
    let s = start();
    for _ in 0..10 {
        let r = s.json("POST", "/api/login", None, json!({ "username": "bob", "password": "wrong-password", "device": "x" }));
        assert_eq!(r.status, 401);
    }
    // Locked out now, even with the right password.
    let r = s.json("POST", "/api/login", None, json!({ "username": "bob", "password": PW, "device": "x" }));
    assert_eq!(r.status, 429);
    // The address is limited too.
    let r = s.json("POST", "/api/login", None, json!({ "username": "carol", "password": PW, "device": "x" }));
    assert_eq!(r.status, 429);
}

#[test]
fn login_rate_limit_per_username_behind_proxy() {
    let s = start();
    for i in 0..10 {
        let ip = format!("203.0.113.{i}");
        let r = s.send(
            "POST",
            "/api/login",
            None,
            &[("Content-Type", "application/json"), ("X-Forwarded-For", &ip)],
            Some(json!({ "username": "carol", "password": "wrong-password" }).to_string().into_bytes()),
        );
        assert_eq!(r.status, 401);
    }
    let r = s.send("POST", "/api/login", None, &[("Content-Type", "application/json"), ("X-Forwarded-For", "198.51.100.7")], Some(json!({ "username": "carol", "password": PW }).to_string().into_bytes()));
    assert_eq!(r.status, 429, "username limit");
    let r = s.send("POST", "/api/login", None, &[("Content-Type", "application/json"), ("X-Forwarded-For", "198.51.100.7")], Some(json!({ "username": "bob", "password": PW }).to_string().into_bytes()));
    assert_eq!(r.status, 200, "other users from other addresses still sign in");
}

#[test]
fn token_revoke_and_disable() {
    let s = start();
    let t1 = s.login("bob");
    let t2 = s.login("bob");
    assert_eq!(s.get("/api/me", &t1).status, 200);
    let st = s.store();
    let bob = st.user_by_name("bob").unwrap().unwrap();
    assert_eq!(st.tokens_of(&bob.id).unwrap().len(), 2);
    assert_eq!(st.revoke_user_tokens(&bob.id).unwrap(), 2);
    assert_eq!(s.get("/api/me", &t1).status, 401);
    assert_eq!(s.get("/api/me", &t2).status, 401);
    let t3 = s.login("bob");
    st.set_disabled(&bob.id, true).unwrap();
    assert_eq!(s.get("/api/me", &t3).status, 401);
    let r = s.json("POST", "/api/login", None, json!({ "username": "bob", "password": PW }));
    assert_eq!(r.status, 401);
    st.set_disabled(&bob.id, false).unwrap();
    st.set_password(&bob.id, "another-pass-2").unwrap();
    let r = s.json("POST", "/api/login", None, json!({ "username": "bob", "password": "another-pass-2" }));
    assert_eq!(r.status, 200);
}

#[test]
fn folder_and_file_crud() {
    let s = start();
    let a = s.login("alice");
    let root = s.root(&a);
    assert_eq!(s.get("/api/root", &a).json()["name"], "My files");
    let f = s.folder(&a, &root, "Reports");
    assert_eq!(f.status, 201);
    let folder = id_of(&f);
    assert_eq!(s.folder(&a, &root, "reports").status, 409, "names are case-insensitive");
    let up = s.upload(&a, &folder, "Budget 2026.xlsx", b"one");
    assert_eq!(up.status, 201);
    let file = up.json();
    let id = file["id"].as_str().unwrap().to_string();
    assert_eq!((file["version"].as_i64(), file["size"].as_i64(), file["role"].as_str()), (Some(1), Some(3), Some("owner")));
    assert!(file["etag"].as_str().unwrap().starts_with("v1-"));
    assert_eq!(file["modified_by"], "Alice Admin");
    // Name conflicts: fail, rename, replace.
    assert_eq!(s.upload(&a, &folder, "budget 2026.xlsx", b"x").status, 409);
    let renamed = s.send("POST", &format!("/api/files?parent={folder}&name=Budget%202026.xlsx&conflict=rename"), Some(&a), &[], Some(b"two".to_vec()));
    assert_eq!((renamed.status, renamed.json()["name"].as_str()), (201, Some("Budget 2026 (2).xlsx")));
    let replaced = s.send("POST", &format!("/api/files?parent={folder}&name=Budget%202026.xlsx&conflict=replace"), Some(&a), &[], Some(b"three".to_vec()));
    assert_eq!((replaced.status, replaced.json()["id"].as_str(), replaced.json()["version"].as_i64()), (200, Some(id.as_str()), Some(2)));
    // Listing: folders first, then files by name.
    s.folder(&a, &folder, "Archive");
    let kids = s.get(&format!("/api/items/{folder}/children"), &a).json();
    let names: Vec<&str> = kids["items"].as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Archive", "Budget 2026 (2).xlsx", "Budget 2026.xlsx"]);
    // Content.
    let c = s.get(&format!("/api/items/{id}/content"), &a);
    assert_eq!((c.status, c.body.as_slice()), (200, &b"three"[..]));
    assert_eq!(c.etag.as_deref().map(|e| e.trim_matches('"').to_string()), Some(s.get(&format!("/api/items/{id}"), &a).json()["etag"].as_str().unwrap().to_string()));
    // Rename and move.
    let r = s.json("PATCH", &format!("/api/items/{id}"), Some(&a), json!({ "name": "Plan.xlsx" }));
    assert_eq!((r.status, r.json()["name"].as_str()), (200, Some("Plan.xlsx")));
    let r = s.json("PATCH", &format!("/api/items/{id}"), Some(&a), json!({ "parent": root }));
    assert_eq!((r.status, r.json()["parent"].as_str()), (200, Some(root.as_str())));
    let r = s.json("PATCH", &format!("/api/items/{folder}"), Some(&a), json!({ "parent": folder }));
    assert_eq!(r.status, 400, "a folder can't move into itself");
    assert_eq!(s.json("PATCH", &format!("/api/items/{root}"), Some(&a), json!({ "name": "x" })).status, 400);
    // Recent.
    let recent = s.get("/api/recent", &a).json();
    assert_eq!(recent["items"][0]["id"].as_str(), Some(id.as_str()));
    // Trash and restore.
    assert_eq!(s.send("DELETE", &format!("/api/items/{folder}"), Some(&a), &[], None).status, 204);
    assert_eq!(s.get(&format!("/api/items/{folder}"), &a).status, 404);
    let inside = id_of(&s.upload(&a, &root, "Keep.xlsx", b"k"));
    let trash = s.get("/api/trash", &a).json();
    assert_eq!(trash["items"].as_array().unwrap().len(), 1);
    let r = s.send("POST", &format!("/api/items/{folder}/restore"), Some(&a), &[], None);
    assert_eq!((r.status, r.json()["name"].as_str()), (200, Some("Reports")));
    assert_eq!(s.get(&format!("/api/items/{folder}/children"), &a).json()["items"].as_array().unwrap().len(), 2);
    assert_eq!(s.get(&format!("/api/items/{inside}"), &a).status, 200);
}

#[test]
fn save_conflict_412() {
    let s = start();
    let a = s.login("alice");
    let root = s.root(&a);
    let id = id_of(&s.upload(&a, &root, "Book.xlsx", b"v1"));
    let etag1 = s.get(&format!("/api/items/{id}"), &a).json()["etag"].as_str().unwrap().to_string();
    // First writer wins (quoted and bare forms are both accepted).
    let r = s.put(&a, &id, Some(&format!("\"{etag1}\"")), b"v2");
    assert_eq!(r.status, 200);
    let etag2 = r.json()["etag"].as_str().unwrap().to_string();
    assert!(etag2.starts_with("v2-"));
    assert_eq!(r.json()["version"].as_i64(), Some(2));
    // Second writer with the old etag is refused and told who saved.
    let r = s.put(&a, &id, Some(&etag1), b"v2-other");
    assert_eq!(r.status, 412);
    let v = r.json();
    assert_eq!((v["etag"].as_str(), v["modified_by"].as_str(), v["version"].as_i64()), (Some(etag2.as_str()), Some("Alice Admin"), Some(2)));
    assert!(v["modified"].as_str().unwrap().ends_with('Z'));
    assert_eq!(s.get(&format!("/api/items/{id}/content"), &a).body, b"v2");
    // Overwrite: no If-Match.
    let r = s.put(&a, &id, None, b"v3");
    assert_eq!((r.status, r.json()["version"].as_i64()), (200, Some(3)));
    // Same content still makes a new version, and the etag changes.
    let r = s.put(&a, &id, Some(r.json()["etag"].as_str().unwrap()), b"v3");
    assert_eq!(r.json()["version"].as_i64(), Some(4));
}

#[test]
fn version_history_and_restore() {
    let s = start_with(|c| c.versions_to_keep = 3);
    let a = s.login("alice");
    let root = s.root(&a);
    let id = id_of(&s.upload(&a, &root, "History.xlsx", b"content-1"));
    for i in 2..=6 {
        assert_eq!(s.put(&a, &id, None, format!("content-{i}").as_bytes()).status, 200);
    }
    let v = s.get(&format!("/api/items/{id}/versions"), &a).json();
    let nums: Vec<i64> = v["versions"].as_array().unwrap().iter().map(|x| x["version"].as_i64().unwrap()).collect();
    // Newest three plus the first of the day.
    assert_eq!(nums, [6, 5, 4, 1]);
    assert_eq!(v["versions"][0]["current"], true);
    let files: Vec<String> = std::fs::read_dir(s.cfg.files_dir().join(&id)).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    assert_eq!(files.len(), 4, "pruned versions are deleted from disk: {files:?}");
    let old = s.get(&format!("/api/items/{id}/content?version=1"), &a);
    assert_eq!((old.status, old.body.as_slice()), (200, &b"content-1"[..]));
    assert!(old.etag.unwrap().contains("v1-"));
    assert_eq!(s.get(&format!("/api/items/{id}/content?version=2"), &a).status, 404);
    let r = s.send("POST", &format!("/api/items/{id}/versions/1/restore"), Some(&a), &[], None);
    assert_eq!((r.status, r.json()["version"].as_i64()), (200, Some(7)));
    assert_eq!(s.get(&format!("/api/items/{id}/content"), &a).body, b"content-1");
    assert_eq!(s.send("POST", &format!("/api/items/{id}/versions/99/restore"), Some(&a), &[], None).status, 404);
}

#[test]
fn permissions_and_hiding() {
    let s = start();
    let (a, b, c) = (s.login("alice"), s.login("bob"), s.login("carol"));
    let root = s.root(&a);
    let id = id_of(&s.upload(&a, &root, "Private.xlsx", b"secret"));
    // Invisible items answer 404, never 403.
    for path in [format!("/api/items/{id}"), format!("/api/items/{id}/content"), format!("/api/items/{id}/versions"), format!("/api/items/{id}/permissions"), format!("/api/items/{root}/children")] {
        assert_eq!(s.get(&path, &b).status, 404, "{path}");
    }
    assert_eq!(s.put(&b, &id, None, b"x").status, 404);
    assert_eq!(s.send("DELETE", &format!("/api/items/{id}"), Some(&b), &[], None).status, 404);
    assert_eq!(s.upload(&b, &root, "Intruder.xlsx", b"x").status, 404);
    // View access.
    let r = s.json("POST", &format!("/api/items/{id}/permissions"), Some(&a), json!({ "username": "bob", "role": "view" }));
    assert_eq!(r.status, 200);
    assert_eq!(s.get(&format!("/api/items/{id}/content"), &b).body, b"secret");
    assert_eq!(s.get(&format!("/api/items/{id}"), &b).json()["role"], "view");
    assert_eq!(s.put(&b, &id, None, b"x").status, 403);
    assert_eq!(s.json("PATCH", &format!("/api/items/{id}"), Some(&b), json!({ "name": "x.xlsx" })).status, 403);
    assert_eq!(s.json("POST", &format!("/api/items/{id}/permissions"), Some(&b), json!({ "username": "carol", "role": "view" })).status, 403);
    assert_eq!(s.json("POST", &format!("/api/items/{id}/links"), Some(&b), json!({ "role": "view" })).status, 403);
    let shared = s.get("/api/shared-with-me", &b).json();
    assert_eq!(shared["items"][0]["id"].as_str(), Some(id.as_str()));
    assert_eq!(shared["items"][0]["owner"]["username"], "alice");
    assert_eq!(s.get(&format!("/api/items/{id}"), &c).status, 404);
    // Inherited edit access from a folder.
    let team = id_of(&s.folder(&a, &root, "Team"));
    let plan = id_of(&s.upload(&a, &team, "Plan.xlsx", b"p1"));
    s.json("POST", &format!("/api/items/{team}/permissions"), Some(&a), json!({ "username": "carol", "role": "edit" }));
    assert_eq!(s.get(&format!("/api/items/{plan}"), &c).json()["role"], "edit");
    assert_eq!(s.put(&c, &plan, None, b"p2").status, 200);
    assert_eq!(s.get(&format!("/api/items/{plan}"), &a).json()["modified_by"], "Carol C");
    let up = s.upload(&c, &team, "Carol.xlsx", b"c");
    assert_eq!(up.status, 201);
    assert_eq!(up.json()["owner"]["username"], "alice", "files in a shared folder belong to the folder owner");
    let perms = s.get(&format!("/api/items/{plan}/permissions"), &a).json();
    assert_eq!(perms["owner"]["username"], "alice");
    let p = &perms["permissions"][0];
    assert_eq!((p["username"].as_str(), p["role"].as_str(), p["inherited"].as_bool(), p["inherited_from"]["name"].as_str()), (Some("carol"), Some("edit"), Some(true), Some("Team")));
    // An editor can share further, but not move items into their own files.
    let carol_root = s.root(&c);
    assert_eq!(s.json("PATCH", &format!("/api/items/{plan}"), Some(&c), json!({ "parent": carol_root })).status, 400);
    assert_eq!(s.json("POST", &format!("/api/items/{plan}/permissions"), Some(&c), json!({ "username": "bob", "role": "view" })).status, 200);
    assert_eq!(s.get(&format!("/api/items/{plan}"), &b).json()["role"], "view");
    // Granting to unknown users or the owner fails; bad roles too.
    assert_eq!(s.json("POST", &format!("/api/items/{id}/permissions"), Some(&a), json!({ "username": "nobody", "role": "view" })).status, 404);
    assert_eq!(s.json("POST", &format!("/api/items/{id}/permissions"), Some(&a), json!({ "username": "alice", "role": "view" })).status, 400);
    assert_eq!(s.json("POST", &format!("/api/items/{id}/permissions"), Some(&a), json!({ "username": "bob", "role": "owner" })).status, 400);
    // Revoking hides the item again; inherited grants can't be removed on the child.
    assert_eq!(s.send("DELETE", &format!("/api/items/{id}/permissions/bob"), Some(&a), &[], None).status, 204);
    assert_eq!(s.get(&format!("/api/items/{id}"), &b).status, 404);
    assert_eq!(s.send("DELETE", &format!("/api/items/{plan}/permissions/carol"), Some(&a), &[], None).status, 404);
    let r = s.send("DELETE", &format!("/api/items/{team}/permissions"), Some(&a), &[("Content-Type", "application/json")], Some(json!({ "username": "carol" }).to_string().into_bytes()));
    assert_eq!(r.status, 204);
    assert_eq!(s.get(&format!("/api/items/{plan}"), &c).status, 404);
    // Bob can leave what was shared with him.
    assert_eq!(s.send("DELETE", &format!("/api/items/{plan}/permissions/bob"), Some(&b), &[], None).status, 204);
    assert_eq!(s.get(&format!("/api/items/{plan}"), &b).status, 404);
    // Trash hides shared items from others.
    s.json("POST", &format!("/api/items/{id}/permissions"), Some(&a), json!({ "username": "bob", "role": "edit" }));
    s.send("DELETE", &format!("/api/items/{id}"), Some(&a), &[], None);
    assert_eq!(s.get(&format!("/api/items/{id}"), &b).status, 404);
    assert!(s.get("/api/shared-with-me", &b).json()["items"].as_array().unwrap().is_empty());
}

#[test]
fn user_search() {
    let s = start();
    let a = s.login("alice");
    let r = s.get("/api/users?query=bu", &a).json();
    assert_eq!(r["users"], json!([{ "username": "bob", "display_name": "Bob Builder" }]));
    let all = s.get("/api/users?query=", &a).json();
    let names: Vec<&str> = all["users"].as_array().unwrap().iter().map(|u| u["username"].as_str().unwrap()).collect();
    assert_eq!(names, ["bob", "carol"], "the caller isn't listed");
    assert_eq!(s.send("GET", "/api/users?query=b", None, &[], None).status, 401);
}

#[test]
fn share_links() {
    let s = start();
    let a = s.login("alice");
    let root = s.root(&a);
    let id = id_of(&s.upload(&a, &root, "Shared Book.xlsx", b"link-data"));
    let other = id_of(&s.upload(&a, &root, "Other.xlsx", b"other"));
    // View link with a password and an expiry.
    let r = s.json("POST", &format!("/api/items/{id}/links"), Some(&a), json!({ "role": "view", "expires_days": 7, "password": "link-pass-1" }));
    assert_eq!(r.status, 201);
    let v = r.json();
    let url = v["url"].as_str().unwrap().to_string();
    assert!(url.starts_with(&format!("{}/l/", s.url)));
    assert_eq!((v["has_password"].as_bool(), v["role"].as_str()), (Some(true), Some("view")));
    assert!(v["expires"].as_str().is_some());
    let token = url.rsplit('/').next().unwrap().to_string();
    let r = s.send("GET", &format!("/api/links/{token}"), None, &[], None);
    assert_eq!((r.status, r.json()["password_required"].as_bool()), (401, Some(true)));
    let r = s.json("POST", &format!("/api/links/{token}"), None, json!({ "password": "wrong" }));
    assert_eq!(r.status, 401);
    let r = s.json("POST", &format!("/api/links/{token}"), None, json!({ "password": "link-pass-1" }));
    assert_eq!(r.status, 200);
    let v = r.json();
    assert_eq!((v["item"]["name"].as_str(), v["role"].as_str()), (Some("Shared Book.xlsx"), Some("view")));
    let access = v["access_token"].as_str().unwrap().to_string();
    // The access token reads this item only.
    assert_eq!(s.get(&format!("/api/items/{id}/content"), &access).body, b"link-data");
    assert_eq!(s.put(&access, &id, None, b"x").status, 403);
    assert_eq!(s.get(&format!("/api/items/{id}/content?version=1"), &access).status, 404, "no history through links");
    assert_eq!(s.get(&format!("/api/items/{other}"), &access).status, 404);
    assert_eq!(s.get(&format!("/api/items/{root}/children"), &access).status, 404);
    assert_eq!(s.get("/api/me", &access).status, 401);
    assert_eq!(s.json("POST", &format!("/api/items/{id}/links"), Some(&access), json!({ "role": "edit" })).status, 401);
    // Password links have no direct download, only the password form.
    assert_eq!(s.send("GET", &format!("/l/{token}/download"), None, &[], None).status, 401);
    let page = String::from_utf8(s.send("GET", &format!("/l/{token}"), None, &[], None).body).unwrap();
    assert!(page.contains(&format!("action=\"{token}/download\"")) && page.contains("type=\"password\""));
    let form = |pw: &str| s.send("POST", &format!("/l/{token}/download"), None, &[("Content-Type", "application/x-www-form-urlencoded")], Some(format!("password={pw}").into_bytes()));
    assert_eq!(form("nope").status, 401);
    let dl = form("link%2Dpass-1");
    assert_eq!((dl.status, dl.body.as_slice()), (200, &b"link-data"[..]));
    // Edit link without a password.
    let edit = s.json("POST", &format!("/api/items/{id}/links"), Some(&a), json!({ "role": "edit" })).json();
    let etoken = edit["url"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    let r = s.send("GET", &format!("/api/links/{etoken}"), None, &[], None);
    assert_eq!(r.status, 200);
    let eaccess = r.json()["access_token"].as_str().unwrap().to_string();
    let r = s.put(&eaccess, &id, None, b"edited-by-link");
    assert_eq!((r.status, r.json()["modified_by"].as_str()), (200, Some("Someone with a link")));
    let page = s.send("GET", &format!("/l/{etoken}"), None, &[], None);
    assert_eq!(page.status, 200);
    assert!(String::from_utf8_lossy(&page.body).contains("Shared Book.xlsx"));
    let dl = s.send("GET", &format!("/l/{etoken}/download"), None, &[], None);
    assert_eq!((dl.status, dl.body.as_slice()), (200, &b"edited-by-link"[..]));
    // Listing hides the secret; deleting a link kills its access tokens.
    let links = s.get(&format!("/api/items/{id}/links"), &a).json();
    assert_eq!(links["links"].as_array().unwrap().len(), 2);
    assert!(links["links"][0].get("url").is_none());
    let edit_id = edit["id"].as_str().unwrap();
    assert_eq!(s.send("DELETE", &format!("/api/items/{id}/links/{edit_id}"), Some(&a), &[], None).status, 204);
    assert_eq!(s.get(&format!("/api/items/{id}/content"), &eaccess).status, 401);
    assert_eq!(s.send("GET", &format!("/api/links/{etoken}"), None, &[], None).status, 404);
    assert_eq!(s.send("GET", &format!("/l/{etoken}"), None, &[], None).status, 404);
    // Expiry.
    let st = s.store();
    st.conn.execute("UPDATE links SET expires = 1", []).unwrap();
    assert_eq!(s.json("POST", &format!("/api/links/{token}"), None, json!({ "password": "link-pass-1" })).status, 404);
    assert_eq!(s.get(&format!("/api/items/{id}/content"), &access).status, 401);
    assert_eq!(s.send("GET", "/api/links/made-up-token", None, &[], None).status, 404);
    // Trashing the item disables its links.
    let r = s.json("POST", &format!("/api/items/{other}/links"), Some(&a), json!({ "role": "view" })).json();
    let otoken = r["url"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    s.send("DELETE", &format!("/api/items/{other}"), Some(&a), &[], None);
    assert_eq!(s.send("GET", &format!("/api/links/{otoken}"), None, &[], None).status, 404);
    // Links are stored hashed.
    let n: i64 = st.conn.query_row("SELECT COUNT(*) FROM links WHERE token_hash = ?1", [otoken.as_bytes()], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn link_password_rate_limit() {
    let s = start();
    let a = s.login("alice");
    let root = s.root(&a);
    let id = id_of(&s.upload(&a, &root, "L.xlsx", b"l"));
    let url = s.json("POST", &format!("/api/items/{id}/links"), Some(&a), json!({ "password": "link-pass-1" })).json()["url"].as_str().unwrap().to_string();
    let token = url.rsplit('/').next().unwrap();
    for _ in 0..10 {
        assert_eq!(s.json("POST", &format!("/api/links/{token}"), None, json!({ "password": "guess" })).status, 401);
    }
    assert_eq!(s.json("POST", &format!("/api/links/{token}"), None, json!({ "password": "link-pass-1" })).status, 429);
}

#[test]
fn name_validation() {
    let s = start();
    let a = s.login("alice");
    let root = s.root(&a);
    for bad in ["a/b.xlsx", "a\\b.xlsx", "a:b", "a*b", "what?", "say \"hi\"", "<x>", "a|b", "tab\there", "", "   ", "..", &"n".repeat(256)] {
        assert_eq!(s.folder(&a, &root, bad).status, 400, "folder {bad:?}");
        assert_eq!(s.upload(&a, &root, bad, b"x").status, 400, "file {bad:?}");
    }
    let ok = s.upload(&a, &root, "  Trimmed.xlsx  ", b"x");
    assert_eq!((ok.status, ok.json()["name"].as_str()), (201, Some("Trimmed.xlsx")));
    let id = id_of(&ok);
    assert_eq!(s.json("PATCH", &format!("/api/items/{id}"), Some(&a), json!({ "name": "x/y" })).status, 400);
    assert_eq!(s.upload(&a, &root, &"n".repeat(255), b"x").status, 201);
    assert_eq!(s.send("POST", "/api/files?name=x.xlsx&conflict=maybe", Some(&a), &[], Some(b"x".to_vec())).status, 400);
    assert_eq!(s.json("POST", "/api/folders", Some(&a), json!({ "nope": 1 })).status, 400);
    assert_eq!(s.send("POST", "/api/folders", Some(&a), &[], Some(b"{not json".to_vec())).status, 400);
}

#[test]
fn path_safety_and_limits() {
    let s = start_with(|c| c.max_upload_mb = 1);
    let a = s.login("alice");
    let root = s.root(&a);
    // Ids are random hex, never paths.
    for bad in ["..%2F..%2Fetc%2Fpasswd", "%2e%2e", "..", "ABCDEF0123456789ABCDEF0123456789", "x"] {
        assert_eq!(s.get(&format!("/api/items/{bad}"), &a).status, 404, "{bad}");
        assert_eq!(s.get(&format!("/api/items/{bad}/content"), &a).status, 404, "{bad}");
    }
    assert_eq!(s.send("POST", "/api/files?parent=..%2F..&name=x.xlsx", Some(&a), &[], Some(b"x".to_vec())).status, 404);
    let r = s.upload(&a, &root, "..\\..\\evil.xlsx", b"x");
    assert_eq!(r.status, 400);
    let id = id_of(&s.upload(&a, &root, "Safe.xlsx", b"safe"));
    // Bytes live under files/<id>/<version>.xlsx whatever the name is.
    assert!(s.cfg.files_dir().join(&id).join("1.xlsx").is_file());
    let entries: Vec<_> = std::fs::read_dir(s.cfg.files_dir()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    assert!(entries.iter().all(|e| e.len() == 32 && e.chars().all(|c| c.is_ascii_hexdigit())), "{entries:?}");
    // Upload size limit.
    let big = vec![0u8; 1024 * 1024 + 1];
    assert_eq!(s.upload(&a, &root, "Big.xlsx", &big).status, 413);
    assert_eq!(s.put(&a, &id, None, &big).status, 413);
    assert_eq!(s.upload(&a, &root, "Fits.xlsx", &big[..1024 * 1024]).status, 201);
    // JSON bodies are small.
    let huge = json!({ "parent": root, "name": "x".repeat(100 * 1024) });
    assert_eq!(s.json("POST", "/api/folders", Some(&a), huge).status, 413);
    // The live co-editing route needs a WebSocket upgrade.
    assert_eq!(s.get(&format!("/api/items/{id}/live"), &a).status, 426);
}

#[test]
fn backup_copies_database_and_files() {
    let s = start();
    let a = s.login("alice");
    let root = s.root(&a);
    let id = id_of(&s.upload(&a, &root, "B.xlsx", b"backup-me"));
    s.put(&a, &id, None, b"backup-me-2");
    let dest = s.dir.join("backup");
    let (n, _) = s.store().backup(&dest).unwrap();
    assert_eq!(n, 2);
    assert_eq!(std::fs::read(dest.join("files").join(&id).join("2.xlsx")).unwrap(), b"backup-me-2");
    let copy = Store::open(&dest.join("freedomoffice.db"), &dest.join("files"), 25).unwrap();
    assert!(copy.user_by_name("alice").unwrap().is_some());
    assert_eq!(copy.item(&id).unwrap().unwrap().version, 2);
    drop(copy);
    // A second run copies only new versions.
    assert_eq!(s.store().backup(&dest).unwrap().0, 0);
}

// ---------------------------------------------------------------------------
// Organisations, accounts, invites (SPEC.md §2, §3, §5)
// ---------------------------------------------------------------------------

impl Srv {
    fn login_with(&self, user: &str, pw: &str) -> Resp {
        self.json("POST", "/api/login", None, json!({ "username": user, "password": pw, "device": "test" }))
    }

    /// Alice (server administrator) creates an organisation; returns its id and root folder.
    fn org(&self, a: &str, name: &str) -> (String, String) {
        let r = self.json("POST", "/api/orgs", Some(a), json!({ "name": name }));
        assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
        (r.json()["id"].as_str().unwrap().to_string(), r.json()["root"].as_str().unwrap().to_string())
    }

    fn add_member(&self, token: &str, org: &str, user: &str, role: &str) -> u16 {
        self.json("POST", &format!("/api/orgs/{org}/members"), Some(token), json!({ "username": user, "role": role })).status
    }
}

#[test]
fn organisations_members_and_roles() {
    let s = start();
    let (a, b, c) = (s.login("alice"), s.login("bob"), s.login("carol"));
    assert_eq!(s.json("POST", "/api/orgs", Some(&b), json!({ "name": "Nope" })).status, 403, "only the server administrator");
    let (org, root) = s.org(&a, "Team");
    assert_eq!(s.json("POST", "/api/orgs", Some(&a), json!({ "name": "team" })).status, 409);
    // Not members yet: invisible.
    assert_eq!(s.get(&format!("/api/orgs/{org}"), &b).status, 404);
    assert_eq!(s.get(&format!("/api/items/{root}"), &b).status, 404);
    assert_eq!(s.add_member(&b, &org, "carol", "member"), 404);
    assert_eq!(s.add_member(&a, &org, "bob", "admin"), 201);
    let r = s.json("POST", &format!("/api/orgs/{org}/members"), Some(&b), json!({ "username": "carol" }));
    assert_eq!((r.status, r.json()["code"].as_str()), (403, Some("invite_required")), "org admins can't pull in strangers");
    assert_eq!(s.add_member(&a, &org, "carol", "member"), 201, "the server administrator can");
    assert_eq!(s.add_member(&b, &org, "carol", "owner"), 403, "only owners make owners");
    assert_eq!(s.add_member(&c, &org, "alice", "member"), 403, "members don't manage");
    // The org folder shows up for members.
    let me = s.get("/api/me", &c).json();
    assert_eq!((me["orgs"][0]["name"].as_str(), me["orgs"][0]["root"].as_str(), me["orgs"][0]["role"].as_str()), (Some("Team"), Some(root.as_str()), Some("member")));
    assert_eq!(s.get("/api/orgs", &c).json()["orgs"].as_array().unwrap().len(), 1);
    // Members edit by default; files belong to the org.
    let f = s.upload(&c, &root, "Plan.xlsx", b"p1");
    assert_eq!(f.status, 201);
    assert_eq!(f.json()["org"]["name"], "Team");
    assert_eq!(f.json()["owner"]["display_name"], "Team");
    let plan = id_of(&f);
    assert_eq!(s.get(&format!("/api/items/{plan}"), &b).json()["role"], "owner", "org admins own the org's files");
    assert_eq!(s.get(&format!("/api/items/{plan}"), &a).status, 404, "the server administrator sees no documents");
    // A view-only default.
    assert_eq!(s.json("PATCH", &format!("/api/orgs/{org}"), Some(&c), json!({ "member_role": "view" })).status, 403);
    assert_eq!(s.json("PATCH", &format!("/api/orgs/{org}"), Some(&b), json!({ "member_role": "view" })).status, 200);
    assert_eq!(s.get(&format!("/api/items/{plan}"), &c).json()["role"], "view");
    assert_eq!(s.put(&c, &plan, None, b"p2").status, 403);
    // Member list (emails only for managers), role change, rename (server admin only).
    let m = s.get(&format!("/api/orgs/{org}/members"), &c).json();
    assert_eq!(m["members"].as_array().unwrap().len(), 2);
    assert!(m["members"][0].get("email").is_none());
    assert_eq!(s.json("PATCH", &format!("/api/orgs/{org}/members/carol"), Some(&b), json!({ "role": "admin" })).status, 200);
    assert_eq!(s.get(&format!("/api/items/{plan}"), &c).json()["role"], "owner");
    assert_eq!(s.json("PATCH", &format!("/api/orgs/{org}"), Some(&b), json!({ "name": "Crew" })).status, 403);
    assert_eq!(s.json("PATCH", &format!("/api/orgs/{org}"), Some(&a), json!({ "name": "Crew" })).json()["name"], "Crew");
    assert_eq!(s.get(&format!("/api/items/{root}"), &c).json()["name"], "Crew");
    // Leaving / removing ends access.
    assert_eq!(s.send("DELETE", "/api/orgs/Crew/members/carol", Some(&c), &[], None).status, 204, "anyone may leave");
    assert_eq!(s.get(&format!("/api/items/{plan}"), &c).status, 404);
    assert_eq!(s.get(&format!("/api/orgs/{org}"), &c).status, 404);
    // Storage use.
    let st = s.get(&format!("/api/orgs/{org}/storage"), &b).json();
    assert_eq!((st["files"].as_i64(), st["bytes"].as_i64()), (Some(1), Some(2)));
    assert_eq!(s.get("/api/admin/storage", &b).status, 403);
    let all = s.get("/api/admin/storage", &a).json();
    assert_eq!(all["orgs"][0]["bytes"].as_i64(), Some(2));
    assert!(all["users"].as_array().unwrap().iter().any(|u| u["username"] == "carol"));
    // Delete: only an empty org.
    assert_eq!(s.send("DELETE", &format!("/api/orgs/{org}"), Some(&b), &[], None).status, 403);
    assert_eq!(s.send("DELETE", &format!("/api/orgs/{org}"), Some(&a), &[], None).status, 409);
    assert_eq!(s.send("DELETE", &format!("/api/items/{plan}"), Some(&b), &[], None).status, 204);
    assert_eq!(s.get("/api/trash", &b).json()["items"][0]["id"].as_str(), Some(plan.as_str()), "org admins see the org's trash");
    assert_eq!(s.send("DELETE", &format!("/api/orgs/{org}"), Some(&a), &[], None).status, 204);
    assert!(s.get("/api/me", &b).json()["orgs"].as_array().unwrap().is_empty());
    // Organisation accounts never show up as people.
    let names: Vec<String> = s.get("/api/users?query=", &a).json()["users"].as_array().unwrap().iter().map(|u| u["username"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["bob", "carol"]);
}

#[test]
fn sharing_with_a_whole_organisation() {
    let s = start();
    let (a, b, c) = (s.login("alice"), s.login("bob"), s.login("carol"));
    let (org, _) = s.org(&a, "Family");
    assert_eq!(s.add_member(&a, &org, "carol", "member"), 201);
    let root = s.root(&b);
    let id = id_of(&s.upload(&b, &root, "Budget.xlsx", b"b"));
    // Bob isn't in the org: he can't share with it (nor find out it exists).
    let r = s.json("POST", &format!("/api/items/{id}/permissions"), Some(&b), json!({ "org": "Family", "role": "view" }));
    assert_eq!(r.status, 404);
    assert_eq!(s.add_member(&a, &org, "bob", "member"), 201);
    let r = s.json("POST", &format!("/api/items/{id}/permissions"), Some(&b), json!({ "org": "Family", "role": "view" }));
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(r.json()["orgs"][0]["org"]["name"], "Family");
    assert_eq!(s.get(&format!("/api/items/{id}"), &c).json()["role"], "view");
    assert_eq!(s.get("/api/shared-with-me", &c).json()["items"][0]["id"].as_str(), Some(id.as_str()));
    let perms = s.get(&format!("/api/items/{id}/permissions"), &b).json();
    assert_eq!((perms["orgs"][0]["role"].as_str(), perms["permissions"].as_array().unwrap().len()), (Some("view"), 0));
    // By id works too, and raises the role.
    s.json("POST", &format!("/api/items/{id}/permissions"), Some(&b), json!({ "org": org, "role": "edit" }));
    assert_eq!(s.get(&format!("/api/items/{id}"), &c).json()["role"], "edit");
    // Leaving the org ends the access that came with it.
    assert_eq!(s.send("DELETE", &format!("/api/orgs/{org}/members/carol"), Some(&a), &[], None).status, 204);
    assert_eq!(s.get(&format!("/api/items/{id}"), &c).status, 404);
    assert_eq!(s.add_member(&a, &org, "carol", "member"), 201);
    assert_eq!(s.send("DELETE", &format!("/api/items/{id}/permissions/org/Family"), Some(&b), &[], None).status, 204);
    assert_eq!(s.get(&format!("/api/items/{id}"), &c).status, 404);
    assert_eq!(s.send("DELETE", &format!("/api/items/{id}/permissions/org/Family"), Some(&b), &[], None).status, 404);
}

#[test]
fn accounts_with_temporary_passwords() {
    let s = start();
    let (a, b) = (s.login("alice"), s.login("bob"));
    assert_eq!(s.json("POST", "/api/users", Some(&b), json!({ "username": "dave" })).status, 403, "people need an org they manage");
    let r = s.json("POST", "/api/users", Some(&a), json!({ "username": "dave", "display_name": "Dave D", "email": "dave@example.test" }));
    assert_eq!(r.status, 201);
    let temp = r.json()["temporary_password"].as_str().unwrap().to_string();
    assert!(temp.len() >= 16);
    assert_eq!(r.json()["user"]["must_change_password"], true);
    assert_eq!(s.json("POST", "/api/users", Some(&a), json!({ "username": "dave" })).status, 409);
    // Signed in with it, only the password change (and me, logout) works.
    let l = s.login_with("dave", &temp);
    assert_eq!(l.status, 200);
    assert_eq!(l.json()["user"]["must_change_password"], true);
    let d = l.json()["token"].as_str().unwrap().to_string();
    let r = s.get("/api/root", &d);
    assert_eq!((r.status, r.json()["code"].as_str()), (403, Some("password_change_required")));
    assert_eq!(s.get("/api/me", &d).status, 200);
    let change = |old: &str, new: &str| s.json("POST", "/api/me/password", Some(&d), json!({ "old_password": old, "new_password": new }));
    assert_eq!(change("wrong-password-x", "a-much-better-passphrase").status, 403);
    assert_eq!(change(&temp, "password1234").status, 400, "common");
    assert_eq!(change(&temp, "dave-is-my-name-ok").status, 400, "contains the username");
    assert_eq!(change(&temp, "short").status, 400);
    let r = change(&temp, "a-much-better-passphrase");
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["user"]["must_change_password"], false);
    assert_eq!(s.get("/api/root", &d).status, 200, "the same device keeps working");
    assert_eq!(s.login_with("dave", &temp).status, 401);
    // An org admin creates accounts into the org.
    let (org, root) = s.org(&a, "Club");
    assert_eq!(s.add_member(&a, &org, "bob", "admin"), 201);
    let r = s.json("POST", "/api/users", Some(&b), json!({ "username": "erin", "org": "Club", "role": "member" }));
    assert_eq!(r.status, 201);
    let temp = r.json()["temporary_password"].as_str().unwrap().to_string();
    assert_eq!(s.json("POST", "/api/users", Some(&b), json!({ "username": "fred", "org": "Club", "role": "owner" })).status, 403);
    let e = s.login_with("erin", &temp).json()["token"].as_str().unwrap().to_string();
    s.json("POST", "/api/me/password", Some(&e), json!({ "old_password": temp, "new_password": "my-own-passphrase-77" }));
    assert_eq!(s.get(&format!("/api/items/{root}"), &e).json()["role"], "edit");
    // Reset by the server administrator: a new temporary password, devices signed out.
    assert_eq!(s.send("POST", "/api/users/erin/reset-password", Some(&b), &[], None).status, 403);
    let r = s.send("POST", "/api/users/erin/reset-password", Some(&a), &[], None);
    assert_eq!(r.status, 200);
    assert_eq!(s.get("/api/me", &e).status, 401);
    let temp2 = r.json()["temporary_password"].as_str().unwrap().to_string();
    assert_eq!(s.login_with("erin", &temp2).json()["user"]["must_change_password"], true);
    // Disable / enable.
    assert_eq!(s.send("POST", "/api/users/alice/disable", Some(&a), &[], None).status, 400);
    assert_eq!(s.send("POST", "/api/users/erin/disable", Some(&a), &[], None).status, 204);
    assert_eq!(s.login_with("erin", &temp2).status, 401);
    assert_eq!(s.send("POST", "/api/users/erin/enable", Some(&a), &[], None).status, 204);
    assert_eq!(s.login_with("erin", &temp2).status, 200);
}

#[test]
fn invites_create_an_account_once() {
    let s = start();
    let (a, b, c) = (s.login("alice"), s.login("bob"), s.login("carol"));
    let (org, root) = s.org(&a, "Choir");
    assert_eq!(s.add_member(&a, &org, "bob", "admin"), 201);
    assert_eq!(s.add_member(&a, &org, "carol", "member"), 201);
    assert_eq!(s.json("POST", &format!("/api/orgs/{org}/invites"), Some(&c), json!({ "email": "x@example.test" })).status, 403);
    let r = s.json("POST", &format!("/api/orgs/{org}/invites"), Some(&b), json!({ "email": "gina@example.test" }));
    assert_eq!(r.status, 201);
    let url = r.json()["url"].as_str().unwrap().to_string();
    let token = url.rsplit('/').next().unwrap().to_string();
    assert!(url.contains("/i/"));
    assert_eq!(s.get(&format!("/api/orgs/{org}/invites"), &b).json()["invites"].as_array().unwrap().len(), 1);
    let info = s.send("GET", &format!("/api/invites/{token}"), None, &[], None).json();
    assert_eq!((info["org"]["name"].as_str(), info["email"].as_str()), (Some("Choir"), Some("gina@example.test")));
    // The browser page has the form.
    let page = s.send("GET", &format!("/i/{token}"), None, &[], None);
    assert_eq!(page.status, 200);
    assert!(String::from_utf8_lossy(&page.body).contains("Choir"));
    // A weak password doesn't use the invite up.
    let accept = |user: &str, pw: &str| s.json("POST", &format!("/api/invites/{token}"), None, json!({ "username": user, "password": pw, "display_name": "Gina" }));
    assert_eq!(accept("gina", "gina-password").status, 400);
    let r = accept("gina", "sings-in-the-choir-7");
    assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
    let g = r.json()["token"].as_str().unwrap().to_string();
    assert_eq!(s.get(&format!("/api/items/{root}"), &g).json()["role"], "edit");
    assert_eq!(s.get("/api/me", &g).json()["email"], "gina@example.test");
    assert_eq!(accept("gina2", "sings-in-the-choir-7").status, 404, "single use");
    assert_eq!(s.send("GET", &format!("/i/{token}"), None, &[], None).status, 404);
    assert!(s.get(&format!("/api/orgs/{org}/invites"), &b).json()["invites"].as_array().unwrap().is_empty());
    // The HTML form works too, and invites can be withdrawn.
    let r = s.json("POST", &format!("/api/orgs/{org}/invites"), Some(&b), json!({}));
    let t2 = r.json()["url"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    let form = s.send("POST", &format!("/i/{t2}"), None, &[("Content-Type", "application/x-www-form-urlencoded")], Some(b"username=hank&display_name=Hank+H&password=sings-bass-loudly-9".to_vec()));
    assert_eq!(form.status, 200);
    assert_eq!(s.login_with("hank", "sings-bass-loudly-9").status, 200);
    let id3 = s.json("POST", &format!("/api/orgs/{org}/invites"), Some(&b), json!({})).json()["id"].as_str().unwrap().to_string();
    assert_eq!(s.send("DELETE", &format!("/api/orgs/{org}/invites/{id3}"), Some(&b), &[], None).status, 204);
    assert_eq!(s.send("GET", "/api/invites/not-a-real-token", None, &[], None).status, 404);
}

#[test]
fn invites_can_be_accepted_by_existing_accounts() {
    let s = start_with(|c| c.login_delay_ms = 300);
    let (a, b) = (s.login("alice"), s.login("bob"));
    let (org, root) = s.org(&a, "PO Test");
    assert_eq!(s.add_member(&a, &org, "bob", "owner"), 201);
    assert_eq!(s.add_member(&a, &org, "carol", "member"), 201);
    // Carol is removed; Bob can't add her back directly (they share no organisation now).
    assert_eq!(s.send("DELETE", &format!("/api/orgs/{org}/members/carol"), Some(&b), &[], None).status, 204);
    let r = s.json("POST", &format!("/api/orgs/{org}/members"), Some(&b), json!({ "username": "carol" }));
    assert_eq!((r.status, r.json()["code"].as_str()), (403, Some("invite_required")));
    assert!(r.json()["error"].as_str().unwrap().contains("they can accept it with their existing account"));
    let invite = |email: &str, role: &str| {
        let r = s.json("POST", &format!("/api/orgs/{org}/invites"), Some(&b), json!({ "email": email, "role": role }));
        assert_eq!(r.status, 201);
        r.json()["url"].as_str().unwrap().rsplit('/').next().unwrap().to_string()
    };
    let token = invite("carol.c@example.test", "admin");
    let info = s.send("GET", &format!("/api/invites/{token}"), None, &[], None).json();
    assert_eq!(info["accept_with"], json!(["new_account", "existing_account"]));
    let page = String::from_utf8_lossy(&s.send("GET", &format!("/i/{token}"), None, &[], None).body).to_string();
    assert!(page.contains("I already have an account") && page.contains("Create an account") && page.contains("carol.c@example.test"));
    let join = |t: &str, user: &str, pw: &str| s.json("POST", &format!("/api/invites/{t}"), None, json!({ "existing": true, "username": user, "password": pw, "device": "test" }));
    let open_invites = || s.get(&format!("/api/orgs/{org}/invites"), &b).json()["invites"].as_array().unwrap().len();

    // Bob is a member already: a friendly answer; the invite isn't used up.
    let r = join(&token, "bob", PW);
    assert_eq!((r.status, r.json()["already_member"].as_bool()), (200, Some(true)), "{}", String::from_utf8_lossy(&r.body));
    assert!(r.json()["message"].as_str().unwrap().contains("already a member of PO Test"));
    assert_eq!(open_invites(), 1);

    // Wrong passwords are refused and count for the username like sign-ins do: after three,
    // the next sign-in (even the right one) waits.
    for _ in 0..3 {
        assert_eq!(join(&token, "carol", "not-carols-password").status, 401);
    }
    assert_eq!(join(&token, "c a r o l", PW).status, 401, "not a username");
    let t = std::time::Instant::now();
    assert_eq!(s.login_with("carol", PW).status, 200);
    assert!(t.elapsed() >= Duration::from_millis(300), "wrong invite passwords count toward the sign-in limiter");
    assert_eq!(open_invites(), 1, "failed attempts don't use the invite");

    // The right password joins her with the invite's role (the email didn't have to match).
    let r = join(&token, "carol", PW);
    assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!((r.json()["role"].as_str(), r.json()["already_member"].as_bool()), (Some("admin"), Some(false)));
    let c = r.json()["token"].as_str().unwrap().to_string();
    assert_eq!(s.get(&format!("/api/items/{root}"), &c).json()["role"], "owner");
    assert!(s.get("/api/me", &c).json()["orgs"].as_array().unwrap().iter().any(|o| o["id"] == org.as_str()));
    assert_eq!(s.get("/api/me", &c).json()["email"], "", "her own email stays");
    // Used once.
    assert_eq!(join(&token, "carol", PW).status, 404);
    assert_eq!(open_invites(), 0);

    // A disabled account is refused (clearly, but only with the right password).
    s.store().create_user("dave", "Dave", "", PW, false).unwrap();
    assert_eq!(s.send("POST", "/api/users/dave/disable", Some(&a), &[], None).status, 204);
    let t2 = invite("", "member");
    assert_eq!(join(&t2, "dave", "not-daves-password").status, 401);
    let r = join(&t2, "dave", PW);
    assert_eq!(r.status, 403);
    assert!(r.json()["error"].as_str().unwrap().contains("disabled"));
    // So is one with a temporary password.
    let r = s.json("POST", "/api/users", Some(&a), json!({ "username": "erin" }));
    let temp = r.json()["temporary_password"].as_str().unwrap().to_string();
    let r = join(&t2, "erin", &temp);
    assert_eq!((r.status, r.json()["code"].as_str()), (403, Some("password_change_required")));
    assert_eq!(open_invites(), 1, "refusals don't use the invite");

    // The browser form does the same.
    let form = |t: &str, body: &str| s.send("POST", &format!("/i/{t}"), None, &[("Content-Type", "application/x-www-form-urlencoded")], Some(body.as_bytes().to_vec()));
    let r = form(&t2, "existing=1&username=bob&password=wrong-password-xx");
    assert_eq!(r.status, 401);
    assert!(String::from_utf8_lossy(&r.body).contains("I already have an account"), "the form again, with the error");
    let r = form(&t2, &format!("existing=1&username=bob&password={PW}"));
    assert_eq!(r.status, 200);
    assert!(String::from_utf8_lossy(&r.body).contains("already a member"));
    assert_eq!(s.send("DELETE", &format!("/api/orgs/{org}/members/carol"), Some(&b), &[], None).status, 204);
    let r = form(&t2, &format!("existing=1&username=carol&password={PW}"));
    assert_eq!(r.status, 200);
    assert!(String::from_utf8_lossy(&r.body).contains("is now in PO Test"));
    assert_eq!(form(&t2, &format!("existing=1&username=carol&password={PW}")).status, 404);

    // An expired invite can't be used.
    let t3 = invite("", "member");
    let db = rusqlite::Connection::open(s.cfg.db_path()).unwrap();
    db.execute("UPDATE invites SET expires = 1 WHERE used = 0", []).unwrap();
    assert_eq!(s.send("DELETE", &format!("/api/orgs/{org}/members/carol"), Some(&b), &[], None).status, 204);
    assert_eq!(join(&t3, "carol", PW).status, 404);
    // (Every refusal above counted for this address too: it is now at the 10-failure limit.)
    assert_eq!(s.send("GET", &format!("/i/{t3}"), None, &[], None).status, 429);
}

#[test]
fn share_links_can_be_switched_off() {
    let on = start();
    let a = on.login("alice");
    let root = on.root(&a);
    let id = id_of(&on.upload(&a, &root, "L.xlsx", b"l"));
    let url = on.json("POST", &format!("/api/items/{id}/links"), Some(&a), json!({ "role": "view" })).json()["url"].as_str().unwrap().to_string();
    let token = url.rsplit('/').next().unwrap().to_string();
    let access = on.send("GET", &format!("/api/links/{token}"), None, &[], None).json()["access_token"].as_str().unwrap().to_string();
    assert_eq!(on.get(&format!("/api/items/{id}"), &access).status, 200);
    // The same data with allow_links = false.
    let mut s = on;
    drop(s.run.take());
    s.cfg.allow_links = false;
    let run = freedomoffice_server::spawn(s.cfg.clone()).unwrap();
    s.url = run.url();
    s.run = Some(run);
    assert_eq!(s.json("POST", &format!("/api/items/{id}/links"), Some(&a), json!({ "role": "view" })).status, 403);
    assert_eq!(s.send("GET", &format!("/api/links/{token}"), None, &[], None).status, 404);
    assert_eq!(s.send("GET", &format!("/l/{token}"), None, &[], None).status, 404);
    assert_eq!(s.get(&format!("/api/items/{id}"), &access).status, 401, "link access tokens stop working");
}

#[test]
fn repeated_failures_slow_sign_in_down() {
    let s = start_with(|c| c.login_delay_ms = 300);
    let wrong = || s.login_with("bob", "wrong-password-x").status;
    for _ in 0..3 {
        let t = std::time::Instant::now();
        assert_eq!(wrong(), 401);
        assert!(t.elapsed() < Duration::from_millis(300), "no delay for the first failures");
    }
    let t = std::time::Instant::now();
    assert_eq!(s.login_with("bob", PW).status, 200);
    assert!(t.elapsed() >= Duration::from_millis(300), "the 4th attempt waits (even with the right password)");
    // Success clears the count.
    let t = std::time::Instant::now();
    assert_eq!(wrong(), 401);
    assert!(t.elapsed() < Duration::from_millis(300));
    // Other usernames aren't slowed.
    let t = std::time::Instant::now();
    s.login("carol");
    assert!(t.elapsed() < Duration::from_millis(300));
}

#[test]
fn new_passwords_follow_the_policy() {
    let s = start();
    let st = s.store();
    for bad in ["short-one", "password12345", "bobby-rocks-2024", "zzzzzzzzzzzzzzz"] {
        assert!(st.create_user("bobby", "", "", bad, false).is_err(), "{bad}");
    }
    let bob = st.user_by_name("bob").unwrap().unwrap();
    assert!(st.set_password(&bob.id, "qwertyuiop123").is_err());
}

#[test]
fn parallel_wrong_sign_ins_are_limited() {
    let s = start();
    let codes: Vec<u16> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..40).map(|_| sc.spawn(|| s.login_with("bob", "wrong-password-x").status)).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let wrong = codes.iter().filter(|c| **c == 401).count();
    let limited = codes.iter().filter(|c| **c == 429).count();
    assert!(wrong <= 10, "at most 10 attempts reach the password check: {codes:?}");
    assert_eq!(wrong + limited, 40);
    assert_eq!(s.login_with("bob", PW).status, 429, "locked even with the right password");
}

#[test]
fn junk_usernames_are_refused_before_the_limiter() {
    let s = start();
    for name in ["x".repeat(5000), "bob\nfake log line".to_string(), String::new()] {
        assert_eq!(s.login_with(&name, "whatever-password").status, 401);
    }
    assert_eq!(s.login_with("bob", PW).status, 200, "junk doesn't count against real users");
}

#[test]
fn org_admins_reach_only_people_they_know() {
    let s = start();
    let (a, b, c) = (s.login("alice"), s.login("bob"), s.login("carol"));
    let (one, _) = s.org(&a, "One");
    let (two, _) = s.org(&a, "Two");
    assert_eq!(s.add_member(&a, &one, "bob", "owner"), 201);
    assert_eq!(s.add_member(&a, &two, "bob", "admin"), 201);
    // Search: strangers aren't listed, only found by their exact username.
    let names = |tok: &str, q: &str| -> Vec<String> { s.get(&format!("/api/users?query={q}"), tok).json()["users"].as_array().unwrap().iter().map(|u| u["username"].as_str().unwrap().to_string()).collect() };
    assert!(names(&b, "").is_empty());
    assert!(names(&b, "car").is_empty());
    assert_eq!(names(&b, "carol"), ["carol"]);
    assert_eq!(names(&a, ""), ["bob", "carol"], "the server administrator finds everyone");
    // Carol joins One (through the administrator): now Bob knows her and may add her to Two.
    assert_eq!(s.add_member(&a, &one, "carol", "member"), 201);
    assert_eq!(names(&b, "car"), ["carol"]);
    let r = s.json("POST", &format!("/api/orgs/{two}/members"), Some(&b), json!({ "username": "carol" }));
    assert_eq!(r.status, 201);
    assert!(r.json()["members"].as_array().unwrap().iter().all(|m| m.get("email").is_none()), "no emails for org admins");
    assert!(s.get(&format!("/api/orgs/{two}/members"), &a).json()["members"][0].get("email").is_some(), "the server administrator sees them");
    // The last owner can't leave or step down; with a second owner they can.
    assert_eq!(s.send("DELETE", &format!("/api/orgs/{one}/members/bob"), Some(&b), &[], None).status, 409);
    assert_eq!(s.json("PATCH", &format!("/api/orgs/{one}/members/bob"), Some(&b), json!({ "role": "member" })).status, 409);
    assert_eq!(s.add_member(&b, &one, "carol", "owner"), 200);
    assert_eq!(s.send("DELETE", &format!("/api/orgs/{one}/members/bob"), Some(&b), &[], None).status, 204);
    drop(c);
}

#[test]
fn revoking_an_unknown_org_and_a_missing_grant_look_the_same() {
    let s = start();
    let a = s.login("alice");
    let root = s.root(&a);
    let id = id_of(&s.upload(&a, &root, "R.xlsx", b"r"));
    s.org(&a, "Secret Club");
    let x = s.send("DELETE", &format!("/api/items/{id}/permissions/org/Secret%20Club"), Some(&a), &[], None);
    let y = s.send("DELETE", &format!("/api/items/{id}/permissions/org/No%20Such%20Org"), Some(&a), &[], None);
    assert_eq!((x.status, y.status), (404, 404));
    assert_eq!(x.body, y.body);
}
