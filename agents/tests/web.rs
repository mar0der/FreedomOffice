//! Web Sheets live editing end to end: a browser session (the server-side bridge, speaking JSON
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
const PW: &str = "test-password-1";

struct Srv {
    run: Option<Running>,
    dir: std::path::PathBuf,
    url: String,
}

impl Drop for Srv {
    fn drop(&mut self) {
        drop(self.run.take());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start() -> Srv {
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

fn share(a: &Api, item: &str, user: &str, role: &str) {
    let r = a.send("POST", &format!("/api/items/{item}/permissions"), &[("Content-Type", "application/json")], Some(json!({ "username": user, "role": role }).to_string().into_bytes())).unwrap();
    assert!(r.ok(), "{}", r.status);
}

/// A browser: asks for a ticket (as the page does after its CSRF-checked POST) and opens the bridge.
struct Browser {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    seen: Vec<Value>,
    next: u64,
}

impl Browser {
    fn open(s: &Srv, api: &Api, item: &str) -> Browser {
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

    fn send(&mut self, v: Value) {
        self.ws.send(Message::Text(v.to_string().into())).unwrap();
    }

    /// Reads for up to `wait`; everything is kept in `seen`.
    fn pump(&mut self, wait: Duration) {
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

    fn wait_for(&mut self, what: &str, f: impl Fn(&Value) -> bool) -> Value {
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

    fn last_status(&self) -> Option<String> {
        self.seen.iter().rev().find(|v| v["t"] == "status").map(|v| v["state"].as_str().unwrap().to_string())
    }

    fn wait_saved(&mut self) {
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
    fn window(&mut self, sheet: u64, r2: u64, c2: u64) -> std::collections::HashMap<(u64, u64), String> {
        let id = self.next;
        self.next += 1;
        self.send(json!({ "t": "win", "id": id, "sheet": sheet, "r1": 0, "c1": 0, "r2": r2, "c2": c2 }));
        let w = self.wait_for("window", |v| v["t"] == "win" && v["id"] == id);
        w["cells"].as_array().unwrap().iter().map(|c| ((c[0].as_u64().unwrap(), c[1].as_u64().unwrap()), c[2].as_str().unwrap().to_string())).collect()
    }

    fn text(&mut self, r: u64, c: u64) -> String {
        self.window(0, 40, 10).get(&(r, c)).cloned().unwrap_or_default()
    }

    fn edit(&mut self, r: u64, c: u64, text: &str) {
        self.send(json!({ "t": "edit", "sheet": 0, "r": r, "c": c, "text": text }));
    }
}

fn pump_all(b: &mut Browser, l: &mut Live, ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        l.pump(Duration::from_millis(15));
        b.pump(Duration::from_millis(15));
    }
}

fn setup() -> (Srv, Vec<Api>, String) {
    let s = start();
    let apis: Vec<Api> = ["alice", "bob", "carol"].iter().map(|u| Api::login(&s.url, u, PW, "test").unwrap()).collect();
    let root = apis[0].root().unwrap();
    let item = work::create_file(&apis[0], &root, "web-live.xlsx", &["Sheet1".into()]).unwrap().id;
    share(&apis[0], &item, "bob", "edit");
    share(&apis[0], &item, "carol", "view");
    (s, apis, item)
}

#[test]
fn a_browser_and_a_headless_client_edit_the_same_workbook() {
    let (s, apis, item) = setup();
    let mut br = Browser::open(&s, &apis[0], &item);
    let hello = br.wait_for("hello", |v| v["t"] == "hello");
    assert_eq!((hello["role"].as_str(), hello["you"]["name"].as_str()), (Some("edit"), Some("alice")));
    let mut bob = Live::open(&apis[1], &item, true).unwrap();
    assert!(bob.wait_synced(Duration::from_secs(10)));
    let sheet = bob.first_sheet().unwrap();

    // Browser edit -> the headless client sees it, formulas calculated by both.
    br.edit(0, 0, "10");
    br.edit(1, 0, "=A1*3");
    br.wait_saved();
    pump_all(&mut br, &mut bob, 300);
    assert_eq!(work::cell_text(bob.workbook(), 0, 0, 0), "10");
    assert_eq!(work::cell_text(bob.workbook(), 0, 1, 0), "30");
    assert_eq!(br.text(1, 0), "30");
    // The formula bar's content for a formula cell.
    br.send(json!({ "t": "cell", "id": 99, "sheet": 0, "r": 1, "c": 0 }));
    let c = br.wait_for("cell", |v| v["t"] == "cell" && v["id"] == 99);
    assert_eq!((c["input"].as_str(), c["formula"].as_bool()), (Some("=A1*3"), Some(true)));

    // The headless client edits -> the browser is told and shows it.
    bob.set_content(sheet, 0, 0, Some(cosync::CellData { v: cosync::Val::Num(7.0), ..Default::default() }));
    bob.send_presence(sheet, 4, 2, false);
    pump_all(&mut br, &mut bob, 400);
    assert!(br.seen.iter().any(|v| v["t"] == "changed"));
    assert_eq!(br.text(0, 0), "7");
    assert_eq!(br.text(1, 0), "21", "the browser recalculates what depends on a remote change");
    // Presence: the other person, flagged as an agent, with the cell they are on.
    let peers = br.seen.iter().rev().find(|v| v["t"] == "peers" && v["peers"].as_array().is_some_and(|p| p.iter().any(|x| x["p"]["r"] == 4))).expect("bob's presence").clone();
    let bobp = peers["peers"].as_array().unwrap().iter().find(|p| p["name"] == "bob").unwrap();
    assert_eq!((bobp["agent"].as_bool(), bobp["p"]["c"].as_u64(), bobp["p"]["sheet"].as_i64()), (Some(true), Some(2), Some(0)));

    // Typed values keep their implied format; rejected input is told, nothing is sent.
    br.edit(2, 0, "12%");
    br.edit(3, 0, "=SUM(");
    br.wait_for("rejection", |v| v["t"] == "rejected");
    br.wait_saved();
    pump_all(&mut br, &mut bob, 300);
    assert_eq!(br.text(2, 0), "12%");
    assert_eq!(work::cell_text(bob.workbook(), 0, 2, 0), "12%");

    // Undo / redo, per person: the browser's own last change only.
    bob.set_content(sheet, 9, 9, Some(cosync::CellData { v: cosync::Val::Text("bob's".into()), ..Default::default() }));
    br.send(json!({ "t": "undo" }));
    br.wait_saved();
    pump_all(&mut br, &mut bob, 300);
    assert_eq!(br.text(2, 0), "", "undo removed the browser's last edit");
    assert_eq!(br.text(9, 9), "bob's", "and left bob's edit alone");
    br.send(json!({ "t": "redo" }));
    br.wait_saved();
    pump_all(&mut br, &mut bob, 300);
    assert_eq!(work::cell_text(bob.workbook(), 0, 2, 0), "12%");

    // Basic formatting and clearing.
    br.send(json!({ "t": "fmt", "sheet": 0, "r1": 0, "c1": 0, "r2": 0, "c2": 0, "set": { "b": true, "fill": "#ffee99", "nf": "0.00" } }));
    br.wait_saved();
    pump_all(&mut br, &mut bob, 300);
    assert_eq!(work::cell_text(bob.workbook(), 0, 0, 0), "7.00");
    br.send(json!({ "t": "clear", "sheet": 0, "r1": 0, "c1": 0, "r2": 1, "c2": 0 }));
    br.wait_saved();
    pump_all(&mut br, &mut bob, 300);
    assert_eq!(work::cell_text(bob.workbook(), 0, 0, 0), "");

    // The browser's changes reach the file: a snapshot asked of an editor becomes a version.
    let before = apis[0].versions(&item).unwrap().len();
    for i in 0..210u64 {
        br.edit(20 + i % 5, i % 7, &format!("{i}"));
    }
    br.wait_saved();
    let end = Instant::now() + Duration::from_secs(20);
    while apis[0].versions(&item).unwrap().len() == before && Instant::now() < end {
        pump_all(&mut br, &mut bob, 200);
    }
    assert!(apis[0].versions(&item).unwrap().len() > before, "a snapshot became a version");
    let mut late = Live::open(&apis[1], &item, true).unwrap();
    assert!(late.wait_synced(Duration::from_secs(10)));
    pump_all(&mut br, &mut bob, 300);
    late.pump(Duration::from_millis(200));
    assert!(late.differences(&bob).is_empty(), "{:?}", late.differences(&bob));
}

#[test]
fn viewers_cannot_edit_and_role_changes_apply_live() {
    let (s, apis, item) = setup();
    let mut view = Browser::open(&s, &apis[2], &item);
    let hello = view.wait_for("hello", |v| v["t"] == "hello");
    assert_eq!(hello["role"], "view");
    view.edit(0, 0, "nope");
    view.wait_for("rejection", |v| v["t"] == "rejected");
    let mut bob = Live::open(&apis[1], &item, true).unwrap();
    assert!(bob.wait_synced(Duration::from_secs(10)));
    let sheet = bob.first_sheet().unwrap();
    bob.set_content(sheet, 0, 0, Some(cosync::CellData { v: cosync::Val::Text("hello".into()), ..Default::default() }));
    pump_all(&mut view, &mut bob, 400);
    assert_eq!(view.text(0, 0), "hello", "a viewer sees live changes");
    // Raised to edit while connected: the browser is told and can write.
    share(&apis[0], &item, "carol", "edit");
    view.wait_for("role edit", |v| v["t"] == "role" && v["role"] == "edit");
    view.edit(1, 0, "now allowed");
    view.wait_saved();
    pump_all(&mut view, &mut bob, 300);
    assert_eq!(work::cell_text(bob.workbook(), 0, 1, 0), "now allowed");
    // Lowered again: edits are refused at once.
    share(&apis[0], &item, "carol", "view");
    view.wait_for("role view", |v| v["t"] == "role" && v["role"] == "view");
    view.edit(2, 0, "refused");
    view.wait_for("rejection", |v| v["t"] == "rejected");
    // Access removed: told, and the connection ends.
    let r = apis[0].send("DELETE", &format!("/api/items/{item}/permissions/carol"), &[], None).unwrap();
    assert!(r.ok());
    let v = view.wait_for("revoked", |v| v["t"] == "revoked");
    assert!(v["reason"].as_str().is_some());
    // Without a ticket (the cookie alone) nobody gets in.
    let url = format!("ws{}/api/items/{item}/web-live", s.url.strip_prefix("http").unwrap());
    assert!(tungstenite::connect(url).is_err());
    // A used ticket is dead.
    let r = apis[0].send("POST", &format!("/api/items/{item}/live-ticket"), &[], None).unwrap();
    let t = r.json()["ticket"].as_str().unwrap().to_string();
    let url = format!("ws{}/api/items/{item}/web-live?ticket={t}", s.url.strip_prefix("http").unwrap());
    assert!(tungstenite::connect(url.clone()).is_ok());
    assert!(tungstenite::connect(url).is_err());
}

/// Both sides write at random, to the same cells too, with the connection of the headless
/// client dropped now and then; everything converges.
#[test]
fn a_soak_of_concurrent_edits_converges() {
    let (s, apis, item) = setup();
    let mut br = Browser::open(&s, &apis[0], &item);
    br.wait_for("hello", |v| v["t"] == "hello");
    let mut bob = Live::open(&apis[1], &item, true).unwrap();
    assert!(bob.wait_synced(Duration::from_secs(10)));
    let sheet = bob.first_sheet().unwrap();
    let mut x: u64 = 0x2545_f491_4f6c_dd1d;
    let mut rnd = |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n
    };
    for i in 0..400u64 {
        let (r, c) = (rnd(8), rnd(4));
        match rnd(5) {
            0 => br.edit(r, c, &format!("{}", rnd(1000))),
            1 => br.edit(r, c, &format!("=A1+{}", rnd(9))),
            2 => br.edit(r, c, &format!("w{i}")),
            3 => bob.set_content(sheet, r as u32, c as u32, Some(cosync::CellData { v: cosync::Val::Num(rnd(500) as f64), ..Default::default() })),
            _ => bob.set_content(sheet, r as u32, c as u32, None),
        }
        if i % 100 == 99 {
            bob.disconnect();
            pump_all(&mut br, &mut bob, 50);
            bob.connect().unwrap();
        }
        pump_all(&mut br, &mut bob, 5);
    }
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        pump_all(&mut br, &mut bob, 100);
        if bob.synced() && br.last_status().as_deref() == Some("saved") {
            break;
        }
        assert!(Instant::now() < end, "never settled");
    }
    pump_all(&mut br, &mut bob, 500);
    let w = br.window(0, 9, 5);
    for r in 0..10u32 {
        for c in 0..6u32 {
            let mine = w.get(&(u64::from(r), u64::from(c))).cloned().unwrap_or_default();
            let theirs = work::cell_text(bob.workbook(), 0, r, c);
            assert_eq!(mine, theirs, "cell {r},{c}");
        }
    }
    // A late joiner agrees too.
    let mut late = Live::open(&apis[1], &item, true).unwrap();
    assert!(late.wait_synced(Duration::from_secs(10)));
    assert!(late.differences(&bob).is_empty(), "{:?}", late.differences(&bob));
}
