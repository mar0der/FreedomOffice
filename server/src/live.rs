//! Live co-editing: one WebSocket per participant at `GET /api/items/{id}/live`.
//!
//! The server keeps the authoritative, numbered log of committed batches per document (in
//! memory while anyone is connected, and in SQLite) and never needs a spreadsheet engine:
//! batches are transformed with `cosync`, snapshots of the workbook come from clients. Presence
//! (who is where) is relayed, never stored. Operation contents are never logged.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use cosync::server::{Commit, CommitError, Log};
use cosync::{close, ClientMsg, Peer, ServerMsg, SheetId};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use crate::store::{Item, LiveOp, Principal, Role, Store};
use crate::util::{self, now};

/// Ask for a snapshot after this many committed batches...
const SNAPSHOT_OPS: usize = 200;
/// ...or when the oldest unsnapshotted batch is this old.
const SNAPSHOT_AGE: Duration = Duration::from_secs(120);
const SNAPSHOT_RETRY: Duration = Duration::from_secs(60);
const PING_EVERY: Duration = Duration::from_secs(20);
const IDLE_LIMIT: Duration = Duration::from_secs(60);
const TICKET_LIFE: Duration = Duration::from_secs(60);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

enum Out {
    Text(String),
    Ping,
    Close(u16, String),
}

struct Conn {
    peer: Peer,
    tx: mpsc::UnboundedSender<Out>,
}

/// The live session of one document.
pub struct Doc {
    log: Log,
    conns: Vec<Conn>,
    /// Committed batches since the last snapshot, and when the first of them came.
    unsnapped: usize,
    unsnapped_since: Option<Instant>,
    snap_asked: Option<Instant>,
    /// Round-robin over editors for snapshot requests.
    snap_turn: usize,
}

impl Doc {
    fn send(&self, id: u64, msg: &ServerMsg) {
        if let Some(c) = self.conns.iter().find(|c| c.peer.id == id) {
            let _ = c.tx.send(Out::Text(json(msg)));
        }
    }

    fn broadcast(&self, except: u64, msg: &ServerMsg) {
        let text = json(msg);
        for c in self.conns.iter().filter(|c| c.peer.id != except) {
            let _ = c.tx.send(Out::Text(text.clone()));
        }
    }

    fn close(&self, id: u64, code: u16, reason: &str) {
        if let Some(c) = self.conns.iter().find(|c| c.peer.id == id) {
            let _ = c.tx.send(Out::Close(code, reason.to_string()));
        }
    }

    /// Asks one editor for a snapshot when enough has changed since the last one.
    fn maybe_ask_snapshot(&mut self) {
        if self.unsnapped == 0 {
            return;
        }
        let due = self.unsnapped >= SNAPSHOT_OPS || self.unsnapped_since.is_some_and(|t| t.elapsed() >= SNAPSHOT_AGE);
        if !due || self.snap_asked.is_some_and(|t| t.elapsed() < SNAPSHOT_RETRY) {
            return;
        }
        let editors: Vec<u64> = self.conns.iter().filter(|c| c.peer.editor).map(|c| c.peer.id).collect();
        if editors.is_empty() {
            return;
        }
        self.snap_turn = self.snap_turn.wrapping_add(1);
        let id = editors[self.snap_turn % editors.len()];
        self.send(id, &ServerMsg::Snapshot);
        self.snap_asked = Some(Instant::now());
    }
}

fn json(msg: &ServerMsg) -> String {
    serde_json::to_string(msg).unwrap_or_default()
}

struct Ticket {
    who: Principal,
    item: String,
    expires: Instant,
}

/// Every live document and the single-use connection tickets.
#[derive(Default)]
pub struct Hub {
    docs: Mutex<HashMap<String, Arc<Mutex<Doc>>>>,
    /// Connections per item (a leaf lock: safe to check while holding the store).
    active: Mutex<HashMap<String, usize>>,
    tickets: Mutex<HashMap<Vec<u8>, Ticket>>,
    next_conn: AtomicU64,
    ticker: AtomicBool,
}

impl Hub {
    /// A short-lived single-use ticket for clients that can't send an Authorization header.
    pub fn ticket(&self, who: Principal, item: &str) -> String {
        let token = util::new_token();
        let mut t = lock(&self.tickets);
        t.retain(|_, x| x.expires > Instant::now());
        t.insert(util::token_hash(&token), Ticket { who, item: item.to_string(), expires: Instant::now() + TICKET_LIFE });
        token
    }

    pub fn redeem(&self, token: &str, item: &str) -> Option<Principal> {
        let t = lock(&self.tickets).remove(&util::token_hash(token))?;
        (t.item == item && t.expires > Instant::now()).then_some(t.who)
    }

    /// True while someone is connected to the document.
    pub fn connected(&self, item: &str) -> bool {
        lock(&self.active).get(item).is_some_and(|n| *n > 0)
    }

    fn release(&self, item: &str) {
        let mut a = lock(&self.active);
        if let Some(n) = a.get_mut(item) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                a.remove(item);
            }
        }
    }

    /// The document's session, loaded from the store on first use.
    fn doc(&self, s: &Store, item: &Item) -> Result<Arc<Mutex<Doc>>, String> {
        let mut docs = lock(&self.docs);
        if let Some(d) = docs.get(&item.id) {
            return Ok(d.clone());
        }
        let base = s.live_base(item).map_err(|e| e.to_string())?;
        let mut log = Log::new(base.floor);
        let ops = s.live_ops_after(&item.id, base.floor).map_err(|e| e.to_string())?;
        let mut unsnapped = 0;
        for o in ops {
            let parsed: Vec<cosync::Op> = serde_json::from_str(&o.ops).map_err(|e| format!("stored operations: {e}"))?;
            if o.seq > base.seq {
                unsnapped += 1;
            }
            log.entries.push(cosync::server::Entry { seq: o.seq, cid: o.cid, id: o.batch, by: o.actor, ops: parsed });
        }
        for (cid, batch, seq) in s.live_clients(&item.id).map_err(|e| e.to_string())? {
            log.last.insert(cid, (batch, seq));
        }
        let ids = parse_sheets(&base.sheets);
        if !ids.is_empty() {
            log.set_sheets(ids);
            replay_sheets(&mut log, base.seq);
        }
        let doc = Arc::new(Mutex::new(Doc {
            log,
            conns: Vec::new(),
            unsnapped,
            unsnapped_since: (unsnapped > 0).then(Instant::now),
            snap_asked: None,
            snap_turn: 0,
        }));
        docs.insert(item.id.clone(), doc.clone());
        Ok(doc)
    }

    /// A snapshot containing everything up to `seq` was stored; operations up to `floor` are gone.
    pub fn snapshotted(&self, item: &str, seq: u64, floor: u64) {
        let Some(d) = lock(&self.docs).get(item).cloned() else { return };
        let mut d = lock(&d);
        d.log.prune_to(floor);
        d.unsnapped = d.log.entries.iter().filter(|e| e.seq > seq).count();
        d.unsnapped_since = (d.unsnapped > 0).then(Instant::now);
        d.snap_asked = None;
    }

    fn start_ticker(self: &Arc<Self>) {
        if self.ticker.swap(true, Ordering::SeqCst) {
            return;
        }
        let hub = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs(10));
            loop {
                every.tick().await;
                let Some(hub) = hub.upgrade() else { break };
                let docs: Vec<Arc<Mutex<Doc>>> = lock(&hub.docs).values().cloned().collect();
                for d in docs {
                    lock(&d).maybe_ask_snapshot();
                }
            }
        });
    }
}

/// "id:pos,id:pos" -> ids
pub fn parse_sheets(meta: &str) -> Vec<SheetId> {
    meta.split(',').filter_map(|p| p.split(':').next()?.trim().parse().ok()).collect()
}

/// Brings the log's sheet set (as of `from`) up to its head.
fn replay_sheets(log: &mut Log, from: u64) {
    let Some(set) = &mut log.sheets else { return };
    for e in log.entries.iter().filter(|e| e.seq > from) {
        for op in &e.ops {
            match op {
                cosync::Op::AddSheet { s, .. } => {
                    set.insert(*s);
                }
                cosync::Op::DelSheet { s, .. } => {
                    set.remove(s);
                }
                _ => {}
            }
        }
    }
}

/// Who a participant is shown as.
fn peer_of(who: &Principal, role: Role, id: u64) -> Peer {
    let (name, user) = match who {
        Principal::User { user, .. } => (user.display_name.clone(), user.username.clone()),
        Principal::Link { .. } => (if role >= Role::Edit { "Guest (link)".to_string() } else { "Guest viewer (link)".to_string() }, String::new()),
    };
    Peer { id, name, user, color: 0, editor: role >= Role::Edit, presence: None }
}

/// A simple token bucket.
struct Bucket {
    tokens: f64,
    rate: f64,
    burst: f64,
    at: Instant,
}

impl Bucket {
    fn new(rate: f64, burst: f64) -> Self {
        Bucket { tokens: burst, rate, burst, at: Instant::now() }
    }

    fn take(&mut self) -> bool {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.at).as_secs_f64() * self.rate).min(self.burst);
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Everything a connection needs to know about itself.
pub struct Join {
    pub item: Item,
    /// The version the client downloaded (None when resuming a session).
    pub version: Option<i64>,
    pub who: Principal,
    pub role: Role,
    pub actor: String,
    pub since: u64,
    pub cid: String,
}

/// Runs one participant's connection until it closes.
pub async fn run(store: Arc<Mutex<Store>>, hub: Arc<Hub>, socket: WebSocket, j: Join) {
    hub.start_ticker();
    let conn_id = hub.next_conn.fetch_add(1, Ordering::SeqCst) + 1;
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Out>();

    // Load the document and register, all under the document lock so the replay and the live
    // stream join without a gap.
    let item_id = j.item.id.clone();
    let setup = {
        let (store, hub, tx) = (store.clone(), hub.clone(), tx.clone());
        let (item_id, who, role, since, version, cid) = (j.item.id.clone(), j.who.clone(), j.role, j.since, j.version, j.cid.clone());
        tokio::task::spawn_blocking(move || -> Result<Arc<Mutex<Doc>>, String> {
            let (doc, current) = {
                let s = lock(&store);
                // Counted while holding the store, so a plain save can't slip in unnoticed.
                *lock(&hub.active).entry(item_id.clone()).or_default() += 1;
                let item = s.item(&item_id).ok().flatten().ok_or("item gone")?;
                (hub.doc(&s, &item)?, item.version)
            };
            let mut d = lock(&doc);
            let mut peer = peer_of(&who, role, conn_id);
            let replay = if version.is_none_or(|v| v == current) { d.log.since(since).map(<[_]>::to_vec) } else { None };
            let Some(replay) = replay else {
                // The client's copy is older than what the session follows: it must reload. If its
                // last batch was committed (and its ack lost), say so first, or the reload would
                // send it again as a new batch.
                if let Some((id, seq)) = d.log.last_commit(&cid) {
                    let _ = tx.send(Out::Text(json(&ServerMsg::Ack { id, seq, extra: Vec::new() })));
                }
                let _ = tx.send(Out::Text(json(&ServerMsg::Reload)));
                let _ = tx.send(Out::Close(1000, "reload".into()));
                return Ok(doc.clone());
            };
            let used: Vec<u8> = d.conns.iter().map(|c| c.peer.color).collect();
            peer.color = (0..u8::MAX).find(|c| !used.contains(c)).unwrap_or(0);
            let peers: Vec<Peer> = d.conns.iter().map(|c| c.peer.clone()).collect();
            let _ = tx.send(Out::Text(json(&ServerMsg::Welcome { seq: d.log.head(), you: peer.clone(), peers })));
            for e in replay {
                let msg = ServerMsg::Ops { seq: e.seq, cid: e.cid, id: e.id, by: 0, ops: e.ops };
                let _ = tx.send(Out::Text(json(&msg)));
            }
            d.broadcast(conn_id, &ServerMsg::Join { peer: peer.clone() });
            d.conns.push(Conn { peer, tx });
            Ok(doc.clone())
        })
        .await
    };
    let doc = match setup {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => {
            hub.release(&item_id);
            tracing::error!("live: couldn't open item {item_id}: {e}");
            let _ = sink.send(Message::Close(Some(CloseFrame { code: 1011, reason: "Something went wrong on the server.".into() }))).await;
            return;
        }
        Err(_) => {
            hub.release(&item_id);
            return;
        }
    };
    tracing::info!("live: connection {conn_id} joined item {item_id} ({})", if j.role >= Role::Edit { "editor" } else { "viewer" });

    let writer = tokio::spawn(async move {
        while let Some(out) = rx.recv().await {
            let r = match out {
                Out::Text(t) => sink.send(Message::Text(t.into())).await,
                Out::Ping => sink.send(Message::Ping(Vec::new().into())).await,
                Out::Close(code, reason) => {
                    let _ = sink.send(Message::Close(Some(CloseFrame { code, reason: reason.into() }))).await;
                    break;
                }
            };
            if r.is_err() {
                break;
            }
        }
    });

    let mut ops_rate = Bucket::new(30.0, 120.0);
    let mut presence_rate = Bucket::new(15.0, 40.0);
    let mut last_seen = Instant::now();
    let mut heartbeat = tokio::time::interval(PING_EVERY);
    heartbeat.tick().await;
    let editor = j.role >= Role::Edit;
    let fail = |code: u16, reason: &str| {
        let _ = tx.send(Out::Close(code, reason.to_string()));
    };
    loop {
        tokio::select! {
            msg = stream.next() => {
                let Some(Ok(msg)) = msg else { break };
                last_seen = Instant::now();
                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Close(_) => break,
                    Message::Binary(_) => {
                        fail(close::INVALID, "Unexpected message.");
                        break;
                    }
                    _ => continue,
                };
                let Ok(msg) = serde_json::from_str::<ClientMsg>(&text) else {
                    fail(close::INVALID, "Malformed message.");
                    break;
                };
                match msg {
                    ClientMsg::Hello { sheets } => {
                        let mut d = lock(&doc);
                        if d.log.sheets.is_none() && !sheets.is_empty() && sheets.len() <= 10_000 {
                            d.log.set_sheets(sheets);
                            replay_sheets(&mut d.log, j.since);
                        }
                    }
                    ClientMsg::Presence { p } => {
                        if !presence_rate.take() {
                            continue;
                        }
                        let mut d = lock(&doc);
                        if let Some(c) = d.conns.iter_mut().find(|c| c.peer.id == conn_id) {
                            c.peer.presence = Some(p.clone());
                        }
                        d.broadcast(conn_id, &ServerMsg::Presence { id: conn_id, p });
                    }
                    ClientMsg::Ops { base, id, ops } => {
                        if !editor {
                            fail(close::READ_ONLY, "You have view-only access to this workbook.");
                            break;
                        }
                        if !ops_rate.take() {
                            fail(close::RATE, "Too many changes too quickly.");
                            break;
                        }
                        if cosync::validate(&ops).is_err() {
                            tracing::warn!("live: connection {conn_id} sent invalid operations to item {item_id}");
                            fail(close::INVALID, "Invalid changes.");
                            break;
                        }
                        let (store, doc, cid, actor, item) = (store.clone(), doc.clone(), j.cid.clone(), j.actor.clone(), item_id.clone());
                        let ok = tokio::task::spawn_blocking(move || commit(&store, &doc, &item, conn_id, base, &cid, id, &actor, ops)).await.unwrap_or(false);
                        if !ok {
                            break;
                        }
                    }
                }
            }
            _ = heartbeat.tick() => {
                if last_seen.elapsed() > IDLE_LIMIT {
                    fail(close::IDLE, "No response.");
                    break;
                }
                let _ = tx.send(Out::Ping);
            }
        }
    }

    // Leave.
    hub.release(&item_id);
    {
        let mut d = lock(&doc);
        d.conns.retain(|c| c.peer.id != conn_id);
        d.broadcast(conn_id, &ServerMsg::Leave { id: conn_id });
        if d.conns.is_empty() {
            drop(d);
            let mut docs = lock(&hub.docs);
            if docs.get(&item_id).is_some_and(|x| lock(x).conns.is_empty()) {
                docs.remove(&item_id);
            }
        }
    }
    drop(tx);
    let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
    tracing::info!("live: connection {conn_id} left item {item_id}");
}

/// Transforms, stores and distributes one batch. False when the connection must close.
#[allow(clippy::too_many_arguments)]
fn commit(store: &Mutex<Store>, doc: &Mutex<Doc>, item: &str, conn: u64, base: u64, cid: &str, id: u64, actor: &str, ops: Vec<cosync::Op>) -> bool {
    let mut d = lock(doc);
    let (sheets_before, last_before) = (d.log.sheets.clone(), d.log.last_commit(cid));
    match d.log.commit(base, cid, id, actor, ops) {
        Ok(Commit::New { seq, ops, extra }) => {
            let rec = LiveOp { seq, cid: cid.to_string(), batch: id, actor: actor.to_string(), ops: serde_json::to_string(&ops).unwrap_or_default(), at: now() };
            if let Err(e) = lock(store).add_live_op(item, &rec) {
                tracing::error!("live: couldn't store a batch for item {item}: {e}");
                d.log.entries.pop();
                d.log.sheets = sheets_before;
                match last_before {
                    Some(l) => d.log.last.insert(cid.to_string(), l),
                    None => d.log.last.remove(cid),
                };
                d.close(conn, 1011, "The server couldn't save your change.");
                return false;
            }
            d.send(conn, &ServerMsg::Ack { id, seq, extra });
            d.broadcast(conn, &ServerMsg::Ops { seq, cid: cid.to_string(), id, by: conn, ops });
            d.unsnapped += 1;
            if d.unsnapped_since.is_none() {
                d.unsnapped_since = Some(Instant::now());
            }
            d.maybe_ask_snapshot();
            true
        }
        Ok(Commit::Duplicate { seq }) => {
            d.send(conn, &ServerMsg::Ack { id, seq, extra: Vec::new() });
            true
        }
        Err(CommitError::TooOld) => {
            d.send(conn, &ServerMsg::Reload);
            true
        }
        Err(CommitError::Invalid) => {
            d.close(conn, close::INVALID, "Invalid base.");
            false
        }
    }
}
