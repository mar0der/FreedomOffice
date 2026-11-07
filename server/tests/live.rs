//! Live co-editing end to end: a real server on 127.0.0.1, raw WebSocket clients speaking the
//! `cosync` protocol, throwaway accounts and data.

use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use cosync::{CellData, ClientMsg, Op, Presence, Rect, ServerMsg, Val};
use freedomoffice_server::config::Config;
use freedomoffice_server::store::Store;
use freedomoffice_server::{spawn, Running};
use serde_json::{json, Value};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

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
    headers: Vec<(String, String)>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
}

fn start() -> Srv {
    let dir = std::env::temp_dir().join(format!("fo-live-test-{}", freedomoffice_server::util::new_id()));
    let cfg = Config { data_dir: dir.join("data"), login_delay_ms: 10, ..Config::default() };
    {
        let s = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
        s.create_user("alice", "Alice Admin", "", PW, true).unwrap();
        s.create_user("bob", "Bob Builder", "", PW, false).unwrap();
        s.create_user("carol", "Carol C", "", PW, false).unwrap();
    }
    let run = spawn(cfg.clone()).unwrap();
    let url = run.url();
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(30))).build().into();
    Srv { run: Some(run), url, dir, cfg, agent }
}

impl Srv {
    fn send(&self, method: &str, path: &str, token: Option<&str>, headers: &[(&str, &str)], body: Option<Vec<u8>>) -> Resp {
        let mut b = ureq::http::Request::builder().method(method).uri(format!("{}{path}", self.url));
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
        let headers = res.headers().iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string())).collect();
        let body = res.body_mut().with_config().limit(64 * 1024 * 1024).read_to_vec().unwrap();
        Resp { status, body, headers }
    }

    fn json(&self, method: &str, path: &str, token: Option<&str>, body: Value) -> Resp {
        self.send(method, path, token, &[("Content-Type", "application/json")], Some(body.to_string().into_bytes()))
    }

    fn login(&self, user: &str) -> String {
        let r = self.json("POST", "/api/login", None, json!({ "username": user, "password": PW, "device": "test" }));
        assert_eq!(r.status, 200);
        r.json()["token"].as_str().unwrap().to_string()
    }

    fn new_file(&self, token: &str, name: &str) -> String {
        let root = self.send("GET", "/api/root", Some(token), &[], None).json()["id"].as_str().unwrap().to_string();
        let r = self.send("POST", &format!("/api/files?parent={root}&name={name}&conflict=fail"), Some(token), &[], Some(b"version one".to_vec()));
        assert_eq!(r.status, 201);
        r.json()["id"].as_str().unwrap().to_string()
    }

    fn put(&self, token: &str, id: &str, data: &[u8]) -> Resp {
        self.send("PUT", &format!("/api/items/{id}/content"), Some(token), &[], Some(data.to_vec()))
    }

    /// (live seq, sheets meta, version) of the current content.
    fn live_base(&self, token: &str, id: &str) -> (u64, String, i64) {
        let r = self.send("GET", &format!("/api/items/{id}/content"), Some(token), &[], None);
        assert_eq!(r.status, 200);
        let v = self.send("GET", &format!("/api/items/{id}"), Some(token), &[], None).json()["version"].as_i64().unwrap();
        (r.header("x-live-seq").unwrap().parse().unwrap(), r.header("x-live-sheets").unwrap_or("").to_string(), v)
    }

    fn ws(&self, token: Option<&str>, query: &str, id: &str) -> Result<Ws, tungstenite::Error> {
        let url = format!("{}/api/items/{id}/live?{query}", self.url.replace("http://", "ws://"));
        let mut req = url.into_client_request().unwrap();
        if let Some(t) = token {
            req.headers_mut().insert("Authorization", format!("Bearer {t}").parse().unwrap());
        }
        let (ws, _) = tungstenite::connect(req)?;
        if let MaybeTlsStream::Plain(s) = ws.get_ref() {
            s.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        }
        Ok(Ws { ws })
    }

    /// Joins as `token` from the current version.
    fn join(&self, token: &str, id: &str, cid: &str) -> (Ws, u64) {
        let (seq, _, v) = self.live_base(token, id);
        let mut ws = self.ws(Some(token), &format!("since={seq}&v={v}&cid={cid}"), id).unwrap();
        let ServerMsg::Welcome { seq, .. } = ws.recv() else { panic!("no welcome") };
        (ws, seq)
    }
}

struct Ws {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
}

enum Got {
    Msg(ServerMsg),
    Closed(Option<u16>),
}

impl Ws {
    fn send(&mut self, m: &ClientMsg) {
        self.ws.send(Message::Text(serde_json::to_string(m).unwrap().into())).unwrap();
    }

    fn next(&mut self, wait: Duration) -> Option<Got> {
        let end = Instant::now() + wait;
        while Instant::now() < end {
            match self.ws.read() {
                Ok(Message::Text(t)) => return Some(Got::Msg(serde_json::from_str(&t).unwrap())),
                Ok(Message::Close(f)) => return Some(Got::Closed(f.map(|f| f.code.into()))),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(_) => return Some(Got::Closed(None)),
            }
        }
        None
    }

    fn recv(&mut self) -> ServerMsg {
        match self.next(Duration::from_secs(10)) {
            Some(Got::Msg(m)) => m,
            Some(Got::Closed(c)) => panic!("closed ({c:?})"),
            None => panic!("nothing received"),
        }
    }

    /// The next message that isn't presence/join/leave.
    fn recv_data(&mut self) -> ServerMsg {
        loop {
            match self.recv() {
                ServerMsg::Presence { .. } | ServerMsg::Join { .. } | ServerMsg::Leave { .. } => {}
                m => return m,
            }
        }
    }

    fn close_code(&mut self) -> Option<u16> {
        loop {
            match self.next(Duration::from_secs(10)) {
                Some(Got::Closed(c)) => return c,
                Some(Got::Msg(_)) => {}
                None => panic!("not closed"),
            }
        }
    }

    fn quiet(&mut self, ms: u64) -> bool {
        !matches!(self.next(Duration::from_millis(ms)), Some(Got::Msg(ServerMsg::Ops { .. })))
    }
}

fn text(r: u32, c: u32, t: &str) -> Op {
    Op::Cell { s: 1, r, c, v: Some(Box::new(CellData { v: Val::Text(t.into()), ..Default::default() })), adj: vec![] }
}

#[test]
fn two_editors_converge_and_reconnect_replays() {
    let s = start();
    let a = s.login("alice");
    let b = s.login("bob");
    let id = s.new_file(&a, "Live.xlsx");
    assert_eq!(s.json("POST", &format!("/api/items/{id}/permissions"), Some(&a), json!({ "username": "bob", "role": "edit" })).status, 200);
    // Before any live session the version starts at seq 0 with unknown sheet ids.
    assert_eq!(s.live_base(&a, &id), (0, String::new(), 1));

    let (mut wa, seq) = s.join(&a, &id, "client-a");
    assert_eq!(seq, 0);
    wa.send(&ClientMsg::Hello { sheets: vec![1], proto: cosync::PROTO });
    let (mut wb, _) = s.join(&b, &id, "client-b");
    // Alice sees Bob join, with his name and a different colour.
    let ServerMsg::Join { peer } = wa.recv() else { panic!() };
    assert_eq!((peer.name.as_str(), peer.user.as_str(), peer.editor), ("Bob Builder", "bob", true));

    // Concurrent: Alice types in B5 while Bob inserts a row above it, both against seq 0.
    wa.send(&ClientMsg::Ops { base: 0, id: 1, ops: vec![text(4, 1, "alice")] });
    wb.send(&ClientMsg::Ops { base: 0, id: 1, ops: vec![Op::Rows { s: 1, at: 0, n: 1 }] });
    let mut committed = Vec::new();
    for ws in [&mut wa, &mut wb] {
        for _ in 0..2 {
            match ws.recv_data() {
                ServerMsg::Ack { seq, .. } => committed.push(seq),
                ServerMsg::Ops { seq, ops, .. } => {
                    committed.push(seq);
                    // Committed after Bob's insert, Alice's cell moved down a row.
                    if let [Op::Cell { r, .. }] = &ops[..] {
                        assert_eq!(*r, if seq == 2 { 5 } else { 4 });
                    }
                }
                m => panic!("{m:?}"),
            }
        }
    }
    committed.sort();
    assert_eq!(committed, [1, 1, 2, 2]);

    // Presence is relayed to the others, not stored.
    wb.send(&ClientMsg::Presence { p: Presence { s: 1, r: 3, c: 2, sel: Rect { r1: 3, c1: 2, r2: 3, c2: 2 }, editing: true } });
    match wa.recv() {
        ServerMsg::Presence { p, .. } => assert!(p.editing && p.r == 3),
        m => panic!("{m:?}"),
    }

    // Bob drops off and reconnects from seq 1: he gets seq 2 replayed.
    drop(wb);
    let mut wb = s.ws(Some(&b), "since=1&v=1&cid=client-b", &id).unwrap();
    let ServerMsg::Welcome { seq, peers, .. } = wb.recv() else { panic!() };
    assert_eq!(seq, 2);
    assert_eq!(peers.len(), 1);
    let ServerMsg::Ops { seq, cid, .. } = wb.recv_data() else { panic!() };
    assert_eq!(seq, 2);
    assert!(cid == "client-a" || cid == "client-b");
    // Resending an already committed batch is acknowledged, not committed twice.
    wb.send(&ClientMsg::Ops { base: 0, id: 1, ops: vec![Op::Rows { s: 1, at: 0, n: 1 }] });
    match wb.recv_data() {
        ServerMsg::Ack { id: 1, seq, .. } => assert!(seq <= 2),
        m => panic!("{m:?}"),
    }
    assert!(wa.quiet(300));

    // A plain save would overwrite live changes: 423 while connected...
    assert_eq!(s.put(&a, &id, b"old client").status, 423);
    drop(wa);
    drop(wb);
    std::thread::sleep(Duration::from_millis(300));
    // ...and while committed changes aren't in a saved version yet.
    let r = s.put(&a, &id, b"old client");
    assert_eq!(r.status, 423, "{}", String::from_utf8_lossy(&r.body));
    assert!(r.json()["error"].as_str().unwrap().contains("edited live"));
    assert_eq!(s.send("POST", &format!("/api/items/{id}/versions/1/restore"), Some(&a), &[], None).status, 423);

    // The next joiner replays from the version (seq 0) and is asked for a snapshot... it uploads.
    let (mut wc, _) = s.join(&a, &id, "client-c");
    let mut n = 0;
    while let Some(Got::Msg(ServerMsg::Ops { .. })) = wc.next(Duration::from_millis(500)) {
        n += 1;
    }
    assert_eq!(n, 2);
    // The workbook and the client's lossless state in one body, split at the offset.
    let snap = s.send("POST", &format!("/api/items/{id}/live/snapshot?seq=2"), Some(&a), &[("X-Live-Sheets", "1:0"), ("X-Live-State-Offset", "13")], Some(b"snapshot at 2STATE".to_vec()));
    assert_eq!(snap.status, 200, "{}", String::from_utf8_lossy(&snap.body));
    assert_eq!(snap.json()["version"], 2);
    let content = s.send("GET", &format!("/api/items/{id}/content"), Some(&a), &[], None);
    assert_eq!((content.body.as_slice(), content.header("x-live-state")), (&b"snapshot at 2"[..], Some("1")));
    let state = s.send("GET", &format!("/api/items/{id}/live/state?version=2"), Some(&b), &[], None);
    assert_eq!((state.status, state.body.as_slice()), (200, &b"STATE"[..]));
    assert_eq!(s.send("GET", &format!("/api/items/{id}/live/state?version=1"), Some(&a), &[], None).status, 404);
    assert_eq!(s.send("POST", &format!("/api/items/{id}/live/snapshot?seq=2"), Some(&a), &[("X-Live-State-Offset", "99")], Some(b"x".to_vec())).status, 400);
    // An older snapshot is refused.
    assert_eq!(s.send("POST", &format!("/api/items/{id}/live/snapshot?seq=1"), Some(&a), &[], Some(b"x".to_vec())).status, 409);
    drop(wc);
    std::thread::sleep(Duration::from_millis(300));
    // Joining now starts from the snapshot.
    assert_eq!(s.live_base(&a, &id), (2, "1:0".to_string(), 2));
    // Nobody connected, everything saved: plain saves work again.
    assert_eq!(s.put(&a, &id, b"plain save").status, 200);
    // ...and the next live session starts from that version.
    let (_, _, v) = s.live_base(&a, &id);
    assert_eq!(v, 3);
    // Ops are stored only as long as needed (none after the version saved without live).
    let st = Store::open(&s.cfg.db_path(), &s.cfg.files_dir(), 25).unwrap();
    let left: i64 = st.conn.query_row("SELECT COUNT(*) FROM live_ops WHERE item_id = ?1", [&id], |r| r.get(0)).unwrap();
    assert_eq!(left, 0);
}

#[test]
fn viewers_receive_but_cannot_edit() {
    let s = start();
    let a = s.login("alice");
    let c = s.login("carol");
    let id = s.new_file(&a, "View.xlsx");
    s.json("POST", &format!("/api/items/{id}/permissions"), Some(&a), json!({ "username": "carol", "role": "view" }));
    let (mut wa, _) = s.join(&a, &id, "a");
    let (mut wc, _) = s.join(&c, &id, "c");
    let ServerMsg::Join { peer } = wa.recv() else { panic!() };
    assert!(!peer.editor);
    wa.send(&ClientMsg::Ops { base: 0, id: 1, ops: vec![text(0, 0, "hello")] });
    let ServerMsg::Ops { ops, .. } = wc.recv_data() else { panic!() };
    assert_eq!(ops, vec![text(0, 0, "hello")]);
    // A viewer that sends changes is disconnected.
    wc.send(&ClientMsg::Ops { base: 1, id: 1, ops: vec![text(0, 0, "nope")] });
    assert_eq!(wc.close_code(), Some(cosync::close::READ_ONLY));
    assert!(wa.quiet(300));
    // Viewers can't upload snapshots either.
    assert_eq!(s.send("POST", &format!("/api/items/{id}/live/snapshot?seq=1"), Some(&c), &[], Some(b"x".to_vec())).status, 403);

    // Share links: a view link joins as a viewer, an edit link as an editor.
    for (role, editor) in [("view", false), ("edit", true)] {
        let link = s.json("POST", &format!("/api/items/{id}/links"), Some(&a), json!({ "role": role })).json();
        let token = link["url"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
        let access = s.send("GET", &format!("/api/links/{token}"), None, &[], None).json()["access_token"].as_str().unwrap().to_string();
        let mut wl = s.ws(Some(&access), "since=1&v=1&cid=link", &id).unwrap();
        let ServerMsg::Welcome { you, .. } = wl.recv() else { panic!() };
        assert_eq!(you.editor, editor);
        assert!(you.name.starts_with("Guest"));
    }
}

#[test]
fn tickets_garbage_and_limits() {
    let s = start();
    let a = s.login("alice");
    let b = s.login("bob");
    let id = s.new_file(&a, "T.xlsx");
    // Plain GET without the WebSocket upgrade.
    assert_eq!(s.send("GET", &format!("/api/items/{id}/live"), Some(&a), &[], None).status, 426);
    // No access: 404 before any upgrade; no auth: 401.
    assert!(matches!(s.ws(Some(&b), "since=0&v=1&cid=x", &id), Err(tungstenite::Error::Http(r)) if r.status() == 404));
    assert!(matches!(s.ws(None, "since=0&v=1&cid=x", &id), Err(tungstenite::Error::Http(r)) if r.status() == 401));

    // Single-use ticket for clients that can't send headers.
    let t = s.send("POST", &format!("/api/items/{id}/live-ticket"), Some(&a), &[], None).json()["ticket"].as_str().unwrap().to_string();
    let mut w = s.ws(None, &format!("since=0&v=1&cid=t&ticket={t}"), &id).unwrap();
    assert!(matches!(w.recv(), ServerMsg::Welcome { .. }));
    assert!(matches!(s.ws(None, &format!("since=0&v=1&cid=t&ticket={t}"), &id), Err(tungstenite::Error::Http(r)) if r.status() == 401));

    // Out-of-bounds operations close the connection.
    w.send(&ClientMsg::Ops { base: 0, id: 1, ops: vec![text(cosync::MAX_ROWS, 0, "x")] });
    assert_eq!(w.close_code(), Some(cosync::close::INVALID));
    // So does garbage.
    let mut w = s.ws(Some(&a), "since=0&v=1&cid=g", &id).unwrap();
    w.recv();
    w.ws.send(Message::Text("{\"t\":\"ops\",\"base\":\"x\"}".into())).unwrap();
    assert_eq!(w.close_code(), Some(cosync::close::INVALID));
    // A base from the future too.
    let mut w = s.ws(Some(&a), "since=0&v=1&cid=f", &id).unwrap();
    w.recv();
    w.send(&ClientMsg::Ops { base: 99, id: 1, ops: vec![text(0, 0, "x")] });
    assert_eq!(w.close_code(), Some(cosync::close::INVALID));
    // Flooding trips the rate limit.
    let mut w = s.ws(Some(&a), "since=0&v=1&cid=r", &id).unwrap();
    w.recv();
    let mut code = None;
    for i in 0..400 {
        if w.ws.send(Message::Text(serde_json::to_string(&ClientMsg::Ops { base: 0, id: i + 1, ops: vec![] }).unwrap().into())).is_err() {
            break;
        }
    }
    while let Some(g) = w.next(Duration::from_secs(5)) {
        if let Got::Closed(c) = g {
            code = c;
            break;
        }
    }
    assert_eq!(code, Some(cosync::close::RATE));
    // A stale copy (older version) is told to reload.
    let ServerMsg::Welcome { seq: head, .. } = s.ws(Some(&a), "since=0&v=1&cid=h", &id).unwrap().recv() else { panic!("no welcome") };
    assert_eq!(s.send("POST", &format!("/api/items/{id}/live/snapshot?seq={head}"), Some(&a), &[], Some(b"snap".to_vec())).status, 200);
    std::thread::sleep(Duration::from_millis(300));
    let mut w = s.ws(Some(&a), "since=0&v=1&cid=old", &id).unwrap();
    assert_eq!(w.recv(), ServerMsg::Reload);
}

#[test]
fn deleting_every_sheet_concurrently_leaves_a_rescue_sheet() {
    let s = start();
    let a = s.login("alice");
    let id = s.new_file(&a, "R.xlsx");
    let (mut w1, _) = s.join(&a, &id, "one");
    w1.send(&ClientMsg::Hello { sheets: vec![1, 2], proto: cosync::PROTO });
    let (mut w2, _) = s.join(&a, &id, "two");
    w1.send(&ClientMsg::Ops { base: 0, id: 1, ops: vec![Op::DelSheet { s: 1, name: "Sheet1".into() }] });
    assert!(matches!(w1.recv_data(), ServerMsg::Ack { extra, .. } if extra.is_empty()));
    w2.send(&ClientMsg::Ops { base: 0, id: 1, ops: vec![Op::DelSheet { s: 2, name: "Sheet2".into() }] });
    loop {
        match w2.recv_data() {
            ServerMsg::Ops { .. } => {}
            ServerMsg::Ack { extra, .. } => {
                assert!(matches!(&extra[..], [(1, Op::AddSheet { s: cosync::RESCUE_SHEET, .. })]));
                break;
            }
            m => panic!("{m:?}"),
        }
    }
}

#[test]
fn reconnecting_after_a_plain_save_reloads() {
    let s = start();
    let a = s.login("alice");
    let id = s.new_file(&a, "Stale.xlsx");
    let (mut wa, _) = s.join(&a, &id, "client-a");
    wa.send(&ClientMsg::Hello { sheets: vec![1], proto: cosync::PROTO });
    wa.send(&ClientMsg::Ops { base: 0, id: 1, ops: vec![text(0, 0, "x")] });
    assert!(matches!(wa.recv_data(), ServerMsg::Ack { seq: 1, .. }));
    // The client snapshots everything, then its connection drops (laptop sleeps).
    assert_eq!(s.send("POST", &format!("/api/items/{id}/live/snapshot?seq=1"), Some(&a), &[("X-Live-Sheets", "1:0")], Some(b"snap".to_vec())).status, 200);
    drop(wa);
    std::thread::sleep(Duration::from_millis(300));
    // Someone replaces the content while nobody is connected (allowed: nothing unsaved).
    assert_eq!(s.put(&a, &id, b"completely different workbook").status, 200);
    // The sleeping client reconnects the way the app does: since only, no v.
    let mut ws = s.ws(Some(&a), "since=1&cid=client-a", &id).unwrap();
    loop {
        match ws.recv() {
            // (Its last batch was committed.)
            ServerMsg::Ack { id: 1, seq: 1, .. } => {}
            ServerMsg::Reload => break,
            m => panic!("stale client resumed on a replaced workbook: {m:?}"),
        }
    }
}

#[test]
fn share_links_get_the_live_state_of_the_current_version_only() {
    let s = start();
    let a = s.login("alice");
    let id = s.new_file(&a, "Linked.xlsx");
    let (mut wa, _) = s.join(&a, &id, "client-a");
    wa.send(&ClientMsg::Hello { sheets: vec![1], proto: cosync::PROTO });
    for (seq, state) in [(1u64, "old state"), (2, "new state")] {
        wa.send(&ClientMsg::Ops { base: seq - 1, id: seq, ops: vec![text(0, 0, state)] });
        while !matches!(wa.recv_data(), ServerMsg::Ack { .. }) {}
        let mut body = b"snap".to_vec();
        body.extend_from_slice(state.as_bytes());
        assert_eq!(s.send("POST", &format!("/api/items/{id}/live/snapshot?seq={seq}"), Some(&a), &[("X-Live-State-Offset", "4")], Some(body)).status, 200);
    }
    let link = s.json("POST", &format!("/api/items/{id}/links"), Some(&a), json!({ "role": "edit" })).json();
    let token = link["url"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    let access = s.send("GET", &format!("/api/links/{token}"), None, &[], None).json()["access_token"].as_str().unwrap().to_string();
    let state = |who: &str, v: i64| s.send("GET", &format!("/api/items/{id}/live/state?version={v}"), Some(who), &[], None);
    assert_eq!(state(&access, 3).body, b"new state");
    assert_eq!(state(&access, 2).status, 404, "no history through links");
    assert_eq!(state(&a, 2).body, b"old state");
}

#[test]
fn reload_tells_a_client_its_last_batch_was_committed() {
    let s = start();
    let a = s.login("alice");
    let id = s.new_file(&a, "Lost.xlsx");
    let (mut wa, _) = s.join(&a, &id, "client-a");
    wa.send(&ClientMsg::Hello { sheets: vec![1], proto: cosync::PROTO });
    wa.send(&ClientMsg::Ops { base: 0, id: 7, ops: vec![text(0, 0, "a")] });
    // Its ack is lost: the client never reads it.
    let (mut wb, _) = s.join(&a, &id, "client-b");
    for i in 0..3 {
        wb.send(&ClientMsg::Ops { base: i + 1, id: i + 1, ops: vec![text(5, 0, "b")] });
        while !matches!(wb.recv_data(), ServerMsg::Ack { .. }) {}
    }
    // Two snapshots prune past the client's base; then everyone leaves (the session is
    // loaded from the database again).
    for seq in [2, 4] {
        assert_eq!(s.send("POST", &format!("/api/items/{id}/live/snapshot?seq={seq}"), Some(&a), &[("X-Live-Sheets", "1:0")], Some(b"snap".to_vec())).status, 200);
    }
    drop((wa, wb));
    std::thread::sleep(Duration::from_millis(300));
    let mut ws = s.ws(Some(&a), "since=0&cid=client-a", &id).unwrap();
    assert_eq!(ws.recv(), ServerMsg::Ack { id: 7, seq: 1, extra: Vec::new() });
    assert_eq!(ws.recv(), ServerMsg::Reload);
    // Someone else using that client id learns nothing (and can't make it drop its batch).
    let b = s.login("bob");
    assert_eq!(s.json("POST", &format!("/api/items/{id}/permissions"), Some(&a), json!({ "username": "bob", "role": "edit" })).status, 200);
    let mut ws = s.ws(Some(&b), "since=0&cid=client-a", &id).unwrap();
    assert_eq!(ws.recv(), ServerMsg::Reload);
}

/// Several editors leaving at once each upload a snapshot of the same state: one version.
/// A snapshot after the last one stores the newer content (and date) as the current version.
#[test]
fn snapshots_of_the_same_state_make_one_version() {
    let s = start();
    let a = s.login("alice");
    let b = s.login("bob");
    let id = s.new_file(&a, "Twice.xlsx");
    assert_eq!(s.json("POST", &format!("/api/items/{id}/permissions"), Some(&a), json!({ "username": "bob", "role": "edit" })).status, 200);
    let (mut wa, _) = s.join(&a, &id, "client-a");
    wa.send(&ClientMsg::Hello { sheets: vec![1], proto: cosync::PROTO });
    wa.send(&ClientMsg::Ops { base: 0, id: 1, ops: vec![text(0, 0, "x")] });
    assert!(matches!(wa.recv_data(), ServerMsg::Ack { seq: 1, .. }));
    let before = s.send("GET", &format!("/api/items/{id}"), Some(&a), &[], None).json();
    std::thread::sleep(Duration::from_millis(1100));
    let snap = |who: &str, body: &[u8]| s.send("POST", &format!("/api/items/{id}/live/snapshot?seq=1"), Some(who), &[("X-Live-Sheets", "1:0")], Some(body.to_vec()));
    let first = snap(&a, b"alice's copy");
    assert_eq!(first.status, 200);
    assert_eq!(first.json()["version"], 2);
    assert_ne!(first.json()["modified"], before["modified"], "the file's date moves with its content");
    let second = snap(&b, b"bob's copy");
    assert_eq!(second.status, 200, "{}", String::from_utf8_lossy(&second.body));
    assert_eq!(second.json()["version"], 2, "no second version for the same state");
    let content = s.send("GET", &format!("/api/items/{id}/content"), Some(&a), &[], None);
    assert_eq!(content.body, b"alice's copy");
    let versions = s.send("GET", &format!("/api/items/{id}/versions"), Some(&a), &[], None).json();
    assert_eq!(versions["versions"].as_array().map(|v| v.len()), Some(2));
}

// ---------------------------------------------------------------------------
// Access changes while connected (SPEC.md §3 "Live changes")
// ---------------------------------------------------------------------------

impl Ws {
    /// The next role/revoked message (skipping presence, joins and leaves).
    fn recv_access(&mut self) -> ServerMsg {
        match self.recv_data() {
            m @ (ServerMsg::Role { .. } | ServerMsg::Revoked { .. }) => m,
            m => panic!("expected role or revoked, got {m:?}"),
        }
    }
}

impl Srv {
    fn share(&self, owner: &str, id: &str, user: &str, role: &str) {
        let r = self.json("POST", &format!("/api/items/{id}/permissions"), Some(owner), json!({ "username": user, "role": role }));
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    }
}

#[test]
fn a_lowered_role_takes_effect_in_open_sessions() {
    let s = start();
    let (a, b) = (s.login("alice"), s.login("bob"));
    let id = s.new_file(&a, "Live.xlsx");
    s.share(&a, &id, "bob", "edit");
    let (mut wa, _) = s.join(&a, &id, "a");
    let (mut wb, _) = s.join(&b, &id, "b");
    wb.send(&ClientMsg::Ops { base: 0, id: 1, ops: vec![text(0, 0, "bob was here")] });
    let ServerMsg::Ack { seq: 1, .. } = wb.recv_data() else { panic!() };
    let ServerMsg::Ops { .. } = wa.recv_data() else { panic!() };
    // Down to view: told at once; the owner isn't.
    s.share(&a, &id, "bob", "view");
    assert_eq!(wb.recv_access(), ServerMsg::Role { role: "view".into() });
    assert!(wa.quiet(300));
    // Back to edit: changes are accepted again.
    s.share(&a, &id, "bob", "edit");
    assert_eq!(wb.recv_access(), ServerMsg::Role { role: "edit".into() });
    wb.send(&ClientMsg::Ops { base: 1, id: 2, ops: vec![text(0, 1, "again")] });
    let ServerMsg::Ack { seq: 2, .. } = wb.recv_data() else { panic!() };
    // View again; a change sent anyway closes the connection and isn't committed.
    s.share(&a, &id, "bob", "view");
    assert_eq!(wb.recv_access(), ServerMsg::Role { role: "view".into() });
    wb.send(&ClientMsg::Ops { base: 2, id: 3, ops: vec![text(0, 2, "nope")] });
    assert_eq!(wb.close_code(), Some(cosync::close::READ_ONLY));
    let ServerMsg::Ops { seq: 2, .. } = wa.recv_data() else { panic!() };
    assert!(wa.quiet(300), "the refused change never reaches others");
}

#[test]
fn removed_access_ends_open_sessions() {
    let s = start();
    let (a, b, c) = (s.login("alice"), s.login("bob"), s.login("carol"));
    let id = s.new_file(&a, "Gone.xlsx");
    s.share(&a, &id, "bob", "edit");
    s.share(&a, &id, "carol", "view");
    let (mut wa, _) = s.join(&a, &id, "a");
    let (mut wb, _) = s.join(&b, &id, "b");
    let (mut wc, _) = s.join(&c, &id, "c");
    assert_eq!(s.send("DELETE", &format!("/api/items/{id}/permissions/bob"), Some(&a), &[], None).status, 204);
    let ServerMsg::Revoked { reason } = wb.recv_access() else { panic!() };
    assert_eq!(reason, "Your access to this workbook was removed.");
    assert_eq!(wb.close_code(), Some(cosync::close::REVOKED));
    // The others stay; Bob can't come back.
    while !matches!(wa.recv(), ServerMsg::Leave { .. }) {}
    assert!(wc.quiet(200));
    assert!(s.ws(Some(&b), "since=0&v=1&cid=b", &id).is_err());
    // Signing out (token revoked) ends that device's sessions.
    assert_eq!(s.send("POST", "/api/logout", Some(&c), &[], None).status, 204);
    let ServerMsg::Revoked { reason } = wc.recv_access() else { panic!() };
    assert_eq!(reason, "You were signed out.");
    assert_eq!(wc.close_code(), Some(cosync::close::REVOKED));
    // Disabling an account ends its sessions.
    let c2 = s.login("carol");
    let (mut wc, _) = s.join(&c2, &id, "c2");
    assert_eq!(s.send("POST", "/api/users/carol/disable", Some(&a), &[], None).status, 204);
    let ServerMsg::Revoked { reason } = wc.recv_access() else { panic!() };
    assert_eq!(reason, "Your account was disabled.");
    // Moving the workbook to the trash ends every session on it, the owner's too.
    let b2 = s.login("bob");
    s.share(&a, &id, "bob", "edit");
    let (mut wb, _) = s.join(&b2, &id, "b2");
    assert_eq!(s.send("DELETE", &format!("/api/items/{id}"), Some(&a), &[], None).status, 204);
    let ServerMsg::Revoked { reason } = wb.recv_access() else { panic!() };
    assert_eq!(reason, "This workbook was moved to the trash.");
    let ServerMsg::Revoked { .. } = wa.recv_access() else { panic!() };
}

#[test]
fn leaving_an_organisation_ends_its_sessions() {
    let s = start();
    let (a, b, c) = (s.login("alice"), s.login("bob"), s.login("carol"));
    let r = s.json("POST", "/api/orgs", Some(&a), json!({ "name": "Team" })).json();
    let (org, root) = (r["id"].as_str().unwrap().to_string(), r["root"].as_str().unwrap().to_string());
    for (u, role) in [("bob", "admin"), ("carol", "member")] {
        assert_eq!(s.json("POST", &format!("/api/orgs/{org}/members"), Some(&a), json!({ "username": u, "role": role })).status, 201);
    }
    let r = s.send("POST", &format!("/api/files?parent={root}&name=Org.xlsx&conflict=fail"), Some(&b), &[], Some(b"org file".to_vec()));
    assert_eq!(r.status, 201);
    let id = r.json()["id"].as_str().unwrap().to_string();
    let (mut wb, _) = s.join(&b, &id, "b");
    let (mut wc, _) = s.join(&c, &id, "c");
    // The members' default role drops to view.
    assert_eq!(s.json("PATCH", &format!("/api/orgs/{org}"), Some(&b), json!({ "member_role": "view" })).status, 200);
    assert_eq!(wc.recv_access(), ServerMsg::Role { role: "view".into() });
    // Promoted to admin: editor again.
    assert_eq!(s.json("PATCH", &format!("/api/orgs/{org}/members/carol"), Some(&b), json!({ "role": "admin" })).status, 200);
    assert_eq!(wc.recv_access(), ServerMsg::Role { role: "edit".into() });
    // Removed from the org: the session ends.
    assert_eq!(s.send("DELETE", &format!("/api/orgs/{org}/members/carol"), Some(&b), &[], None).status, 204);
    let ServerMsg::Revoked { reason } = wc.recv_access() else { panic!() };
    assert_eq!(reason, "Your access to this workbook was removed.");
    assert_eq!(wc.close_code(), Some(cosync::close::REVOKED));
    // Bob (admin) is unaffected and still edits.
    wb.send(&ClientMsg::Ops { base: 0, id: 1, ops: vec![text(0, 0, "still here")] });
    loop {
        match wb.recv_data() {
            ServerMsg::Ack { seq: 1, .. } => break,
            ServerMsg::Role { .. } | ServerMsg::Revoked { .. } => panic!("bob's access didn't change"),
            _ => {}
        }
    }
}

#[test]
fn removing_a_share_link_ends_its_sessions() {
    let s = start();
    let a = s.login("alice");
    let id = s.new_file(&a, "Linked.xlsx");
    let link = s.json("POST", &format!("/api/items/{id}/links"), Some(&a), json!({ "role": "edit" })).json();
    let (link_id, token) = (link["id"].as_str().unwrap().to_string(), link["url"].as_str().unwrap().rsplit('/').next().unwrap().to_string());
    let access = s.send("GET", &format!("/api/links/{token}"), None, &[], None).json()["access_token"].as_str().unwrap().to_string();
    let mut wl = s.ws(Some(&access), "since=0&v=1&cid=link", &id).unwrap();
    let ServerMsg::Welcome { .. } = wl.recv() else { panic!() };
    assert_eq!(s.send("DELETE", &format!("/api/items/{id}/links/{link_id}"), Some(&a), &[], None).status, 204);
    assert_eq!(wl.recv_access(), ServerMsg::Revoked { reason: "The share link was removed.".into() });
    assert_eq!(wl.close_code(), Some(cosync::close::REVOKED));
}

#[test]
fn a_ticket_does_not_outlive_the_account() {
    let s = start();
    let (a, b) = (s.login("alice"), s.login("bob"));
    let id = s.new_file(&a, "Ticket.xlsx");
    s.share(&a, &id, "bob", "edit");
    let ticket = |tok: &str| s.send("POST", &format!("/api/items/{id}/live-ticket"), Some(tok), &[], None).json()["ticket"].as_str().unwrap().to_string();
    // Disabled after the ticket was issued.
    let t = ticket(&b);
    assert_eq!(s.send("POST", "/api/users/bob/disable", Some(&a), &[], None).status, 204);
    assert!(matches!(s.ws(None, &format!("since=0&v=1&cid=t&ticket={t}"), &id), Err(tungstenite::Error::Http(r)) if r.status() == 401));
    // Signed out after the ticket was issued.
    assert_eq!(s.send("POST", "/api/users/bob/enable", Some(&a), &[], None).status, 204);
    let b = s.login("bob");
    let t = ticket(&b);
    assert_eq!(s.send("POST", "/api/logout", Some(&b), &[], None).status, 204);
    assert!(matches!(s.ws(None, &format!("since=0&v=1&cid=t&ticket={t}"), &id), Err(tungstenite::Error::Http(r)) if r.status() == 401));
}

/// Joins, leaves, commits and re-checks all at once: nothing deadlocks and every editor ends
/// with every batch.
#[test]
fn concurrent_joins_leaves_commits_and_rechecks_finish() {
    let s = start();
    let (a, b) = (s.login("alice"), s.login("bob"));
    let id = s.new_file(&a, "Busy.xlsx");
    s.share(&a, &id, "bob", "edit");
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|sc| {
        // Someone keeps changing Bob's access (each change re-checks every session).
        sc.spawn(|| {
            let mut i = 0;
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                s.share(&a, &id, "bob", if i % 2 == 0 { "edit" } else { "view" });
                i += 1;
            }
            s.share(&a, &id, "bob", "edit");
        });
        // Alice commits all along.
        sc.spawn(|| {
            let (mut w, mut seq) = s.join(&a, &id, "committer");
            for i in 0..30u32 {
                w.send(&ClientMsg::Ops { base: seq, id: u64::from(i) + 1, ops: vec![text(1, i, "x")] });
                loop {
                    match w.recv_data() {
                        ServerMsg::Ack { seq: q, .. } => {
                            seq = seq.max(q);
                            break;
                        }
                        ServerMsg::Ops { seq: q, .. } => seq = q,
                        _ => {}
                    }
                }
            }
        });
        let workers: Vec<_> = (0..4)
            .map(|n| {
                let (s, a, id) = (&s, &a, &id);
                sc.spawn(move || {
                    for round in 0..5 {
                        let (seq, _, v) = s.live_base(a, id);
                        let Ok(mut w) = s.ws(Some(a), &format!("since={seq}&v={v}&cid=w{n}r{round}"), id) else { continue };
                        if !matches!(w.next(Duration::from_secs(10)), Some(Got::Msg(ServerMsg::Welcome { .. })) | Some(Got::Msg(ServerMsg::Reload))) {
                            panic!("no welcome");
                        }
                        drop(w);
                    }
                })
            })
            .collect();
        for w in workers {
            w.join().unwrap();
        }
        done.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    // Still serving.
    let (mut wa, seq) = s.join(&a, &id, "final");
    wa.send(&ClientMsg::Ops { base: seq, id: 1, ops: vec![text(0, 0, "ok")] });
    while !matches!(wa.recv_data(), ServerMsg::Ack { .. }) {}
    drop(b);
}

#[test]
fn value_and_format_of_one_cell_both_survive_and_old_clients_are_asked_to_update() {
    let s = start();
    let a = s.login("alice");
    let b = s.login("bob");
    let id = s.new_file(&a, "Split.xlsx");
    assert_eq!(s.json("POST", &format!("/api/items/{id}/permissions"), Some(&a), json!({ "username": "bob", "role": "edit" })).status, 200);
    // The server says which protocol it speaks.
    let mut peek = s.ws(Some(&a), "since=0&cid=client-peek", &id).unwrap();
    assert!(matches!(peek.recv(), ServerMsg::Welcome { proto, .. } if proto == cosync::PROTO));
    drop(peek);
    let (mut wa, _) = s.join(&a, &id, "client-a");
    wa.send(&ClientMsg::Hello { sheets: vec![1], proto: cosync::PROTO });
    let (mut wb, _) = s.join(&b, &id, "client-b");
    wb.send(&ClientMsg::Hello { sheets: vec![1], proto: cosync::PROTO });

    // Alice types a value while Bob makes the same cell bold, both against seq 0.
    let value = Op::Content { s: 1, r: 0, c: 0, v: Some(Box::new(CellData { v: Val::Text("typed".into()), ..Default::default() })), adj: vec![] };
    let bold = Op::Format { s: 1, r: 0, c: 0, st: Some(json!({ "bold": true })) };
    wa.send(&ClientMsg::Ops { base: 0, id: 1, ops: vec![value.clone()] });
    wb.send(&ClientMsg::Ops { base: 0, id: 1, ops: vec![bold.clone()] });
    let mut relayed = Vec::new();
    for ws in [&mut wa, &mut wb] {
        for _ in 0..2 {
            match ws.recv_data() {
                ServerMsg::Ack { .. } => {}
                ServerMsg::Ops { base, ops, .. } => {
                    // Neither was changed by the other; the relay says what the sender had seen.
                    assert_eq!(base, Some(0));
                    relayed.extend(ops);
                }
                m => panic!("{m:?}"),
            }
        }
    }
    assert!(relayed.contains(&value) && relayed.contains(&bold), "{relayed:?}");

    // A client that predates the protocol versions is told to update and disconnected.
    let (mut old, _) = s.join(&b, &id, "client-old");
    old.ws.send(Message::Text(r#"{"t":"hello","sheets":[1]}"#.to_string().into())).unwrap();
    let mut told = false;
    loop {
        match old.next(Duration::from_secs(10)) {
            Some(Got::Msg(ServerMsg::Error { message })) => told = message.contains("update"),
            Some(Got::Msg(_)) => {}
            Some(Got::Closed(code)) => {
                assert_eq!(code, Some(cosync::close::UPGRADE));
                break;
            }
            None => panic!("not closed"),
        }
    }
    assert!(told);
}
