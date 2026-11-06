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
    let cfg = Config { data_dir: dir.join("data"), ..Config::default() };
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
    wa.send(&ClientMsg::Hello { sheets: vec![1] });
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
    s.send("POST", &format!("/api/items/{id}/live/snapshot?seq=0"), Some(&a), &[], Some(b"snap".to_vec()));
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
    w1.send(&ClientMsg::Hello { sheets: vec![1, 2] });
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
    wa.send(&ClientMsg::Hello { sheets: vec![1] });
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
fn reload_tells_a_client_its_last_batch_was_committed() {
    let s = start();
    let a = s.login("alice");
    let id = s.new_file(&a, "Lost.xlsx");
    let (mut wa, _) = s.join(&a, &id, "client-a");
    wa.send(&ClientMsg::Hello { sheets: vec![1] });
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
}
