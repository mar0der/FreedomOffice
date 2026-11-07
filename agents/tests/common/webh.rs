#![allow(dead_code)]
//! Shared harness for the web Sheets end-to-end tests (copied from web.rs): Web Sheets live editing end to end: a browser session (the server-side bridge, speaking JSON
//! over `/api/items/{id}/web-live`) next to a headless `fsheets` client in the same live
//! session, on a real server in-process.

use std::net::TcpStream;
use std::time::{Duration, Instant};

use freedomoffice_server::config::Config;
use freedomoffice_server::store::Store;
use freedomoffice_server::{Running, spawn};
use fsheets::api::Api;
use fsheets::live::Live;
use fsheets::work;
use serde_json::{Value, json};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// Test-only password for the throwaway accounts.
pub const PW: &str = "test-password-1";

pub struct Srv {
    pub run: Option<Running>,
    pub dir: std::path::PathBuf,
    pub url: String,
}

impl Drop for Srv {
    fn drop(&mut self) {
        drop(self.run.take());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn start() -> Srv {
    let dir = std::env::temp_dir().join(format!("fo-web-test-{}", freedomoffice_server::util::new_id()));
    let cfg = Config { data_dir: dir.join("data"), login_delay_ms: 10, ..Config::default() };
    {
        let s = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
        for (u, admin) in [("alice", true), ("bob", false), ("carol", false)] {
            s.create_user(u, u, "", PW, admin).unwrap();
        }
    }
    let run = spawn(cfg).unwrap();
    let url = run.url();
    Srv { run: Some(run), dir, url }
}

pub fn share(a: &Api, item: &str, user: &str, role: &str) {
    let r = a.send("POST", &format!("/api/items/{item}/permissions"), &[("Content-Type", "application/json")], Some(json!({ "username": user, "role": role }).to_string().into_bytes())).unwrap();
    assert!(r.ok(), "{}", r.status);
}

/// A browser: asks for a ticket (as the page does after its CSRF-checked POST) and opens the bridge.
pub struct Browser {
    pub ws: WebSocket<MaybeTlsStream<TcpStream>>,
    pub seen: Vec<Value>,
    pub next: u64,
}

impl Browser {
    pub fn open(s: &Srv, api: &Api, item: &str) -> Browser {
        let r = api.send("POST", &format!("/api/items/{item}/live-ticket"), &[], None).unwrap();
        assert!(r.ok(), "{}", r.status);
        let ticket = r.json()["ticket"].as_str().unwrap().to_string();
        let url = format!("ws{}/api/items/{item}/web-live?ticket={ticket}", s.url.strip_prefix("http").unwrap());
        let (ws, _) = tungstenite::connect(url).expect("web-live upgrade");
        if let MaybeTlsStream::Plain(t) = ws.get_ref() {
            t.set_read_timeout(Some(Duration::from_millis(30))).unwrap();
        }
        Browser { ws, seen: Vec::new(), next: 1 }
    }

    pub fn send(&mut self, v: Value) {
        self.ws.send(Message::Text(v.to_string().into())).unwrap();
    }

    /// Reads for up to `wait`; everything is kept in `seen`.
    pub fn pump(&mut self, wait: Duration) {
        let end = Instant::now() + wait;
        while Instant::now() < end {
            match self.ws.read() {
                Ok(Message::Text(t)) => self.seen.push(serde_json::from_str(&t).unwrap()),
                Ok(Message::Close(_)) => return,
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(_) => return,
            }
        }
    }

    pub fn wait_for(&mut self, what: &str, f: impl Fn(&Value) -> bool) -> Value {
        let end = Instant::now() + Duration::from_secs(15);
        let mut from = 0;
        while Instant::now() < end {
            if let Some(v) = self.seen[from..].iter().find(|v| f(v)) {
                return v.clone();
            }
            from = self.seen.len();
            self.pump(Duration::from_millis(60));
        }
        panic!("timed out waiting for {what}; saw {:?}", &self.seen[self.seen.len().saturating_sub(6)..]);
    }

    pub fn last_status(&self) -> Option<String> {
        self.seen.iter().rev().find(|v| v["t"] == "status").map(|v| v["state"].as_str().unwrap().to_string())
    }

    pub fn wait_saved(&mut self) {
        let end = Instant::now() + Duration::from_secs(15);
        while Instant::now() < end {
            self.pump(Duration::from_millis(60));
            if self.last_status().as_deref() == Some("saved") {
                return;
            }
        }
        panic!("never saved; status {:?}", self.last_status());
    }

    /// The text of cells of a window, by (row, col).
    pub fn window(&mut self, sheet: u64, r2: u64, c2: u64) -> std::collections::HashMap<(u64, u64), String> {
        let id = self.next;
        self.next += 1;
        self.send(json!({ "t": "win", "id": id, "sheet": sheet, "r1": 0, "c1": 0, "r2": r2, "c2": c2 }));
        let w = self.wait_for("window", |v| v["t"] == "win" && v["id"] == id);
        w["cells"].as_array().unwrap().iter().map(|c| ((c[0].as_u64().unwrap(), c[1].as_u64().unwrap()), c[2].as_str().unwrap().to_string())).collect()
    }

    pub fn text(&mut self, r: u64, c: u64) -> String {
        self.window(0, 40, 10).get(&(r, c)).cloned().unwrap_or_default()
    }

    pub fn edit(&mut self, r: u64, c: u64, text: &str) {
        self.send(json!({ "t": "edit", "sheet": 0, "r": r, "c": c, "text": text }));
    }
}

pub fn pump_all(b: &mut Browser, l: &mut Live, ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        l.pump(Duration::from_millis(15));
        b.pump(Duration::from_millis(15));
    }
}

pub fn setup() -> (Srv, Vec<Api>, String) {
    let s = start();
    let apis: Vec<Api> = ["alice", "bob", "carol"].iter().map(|u| Api::login(&s.url, u, PW, "test").unwrap()).collect();
    let root = apis[0].root().unwrap();
    let item = work::create_file(&apis[0], &root, "web-live.xlsx", &["Sheet1".into()]).unwrap().id;
    share(&apis[0], &item, "bob", "edit");
    share(&apis[0], &item, "carol", "view");
    (s, apis, item)
}
