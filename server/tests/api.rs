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
    let mut cfg = Config { data_dir: dir.join("data"), ..Config::default() };
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
    assert_eq!(s.get(&format!("/api/items/{other}"), &access).status, 404);
    assert_eq!(s.get(&format!("/api/items/{root}/children"), &access).status, 404);
    assert_eq!(s.get("/api/me", &access).status, 401);
    assert_eq!(s.json("POST", &format!("/api/items/{id}/links"), Some(&access), json!({ "role": "edit" })).status, 401);
    // Password links have no direct download.
    assert_eq!(s.send("GET", &format!("/l/{token}/download"), None, &[], None).status, 401);
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
    // Reserved live co-editing route.
    assert_eq!(s.get(&format!("/api/items/{id}/live"), &a).status, 501);
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
