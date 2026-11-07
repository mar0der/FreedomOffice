//! Live co-editing: one WebSocket per participant at `GET /api/items/{id}/live`.
//!
//! The server keeps the authoritative, numbered log of committed batches per document (in
//! memory while anyone is connected, and in SQLite) and never needs a spreadsheet engine:
//! batches are transformed with `cosync`, snapshots of the workbook come from clients. Presence
//! (who is where) is relayed, never stored. Operation contents are never logged.
//!
//! Access is checked when a connection opens and again whenever something that can change it
//! happens (`Hub::recheck`: shares, organisation membership and roles, disabled accounts,
//! revoked sign-ins, removed links, files moved or trashed), plus every `RECHECK_EVERY` for
//! changes made outside the server process (the admin CLI). A connection whose role drops to
//! view gets `{"t":"role","role":"view"}` and from then on is closed with `close::READ_ONLY`
//! if it sends changes; one raised to edit gets `{"t":"role","role":"edit"}`. A connection
//! that lost access gets `{"t":"revoked","reason":"..."}` and is closed with `close::REVOKED`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use cosync::server::{Commit, CommitError, Log};
use cosync::{close, ClientMsg, Peer, ServerMsg, SheetId};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, Notify};

use crate::store::{Item, LiveOp, Principal, Role, Store};
use crate::util::{self, now};

/// Ask for a snapshot after this many committed batches...
const SNAPSHOT_OPS: usize = 200;
/// ...or when the oldest unsnapshotted batch is this old...
const SNAPSHOT_AGE: Duration = Duration::from_secs(120);
/// ...or when nothing was committed for this long (editing paused: the saved file catches up),
/// at most once per `SNAPSHOT_GAP` (each snapshot is a version in the history).
const SNAPSHOT_IDLE: Duration = Duration::from_secs(15);
const SNAPSHOT_GAP: Duration = Duration::from_secs(60);
/// How often idle and old changes are checked.
const SNAPSHOT_CHECK: Duration = Duration::from_secs(5);
const SNAPSHOT_RETRY: Duration = Duration::from_secs(60);
const PING_EVERY: Duration = Duration::from_secs(20);
const IDLE_LIMIT: Duration = Duration::from_secs(60);
const TICKET_LIFE: Duration = Duration::from_secs(60);
/// Access of every connection is re-checked this often (changes through the API re-check at once).
const RECHECK_EVERY: Duration = Duration::from_secs(30);
/// Messages waiting for a connection (besides its initial replay): a client that doesn't read
/// them is disconnected (it catches up when it reconnects).
const QUEUE: usize = 1024;

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
    tx: mpsc::Sender<Out>,
    /// Told when the queue is full.
    kick: Arc<Notify>,
    /// Who connected and their current role on the document (`peer.editor` follows it).
    who: Principal,
    role: Role,
    /// Access ended (final: a later, older verdict can't give it back).
    revoked: bool,
    /// Told when access ended (the goodbye is queued already).
    end: Arc<Notify>,
}

impl Conn {
    fn push(&self, out: Out) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(out) {
            self.kick.notify_one();
        }
    }
}

/// The live session of one document.
pub struct Doc {
    log: Log,
    conns: Vec<Conn>,
    /// Committed batches since the last snapshot, and when the first of them came.
    unsnapped: usize,
    unsnapped_since: Option<Instant>,
    /// When the last batch was committed, and the last snapshot stored.
    last_commit: Option<Instant>,
    last_snap: Option<Instant>,
    snap_asked: Option<Instant>,
    /// Round-robin over editors for snapshot requests.
    snap_turn: usize,
    /// The file was replaced (restore): no more changes are accepted into this session.
    retired: bool,
}

impl Doc {
    fn send(&self, id: u64, msg: &ServerMsg) {
        if let Some(c) = self.conns.iter().find(|c| c.peer.id == id) {
            c.push(Out::Text(json(msg)));
        }
    }

    fn broadcast(&self, except: u64, msg: &ServerMsg) {
        let text = json(msg);
        for c in self.conns.iter().filter(|c| c.peer.id != except) {
            c.push(Out::Text(text.clone()));
        }
    }

    /// Whether the connection may send changes (its current role, not the one it joined with).
    fn is_editor(&self, id: u64) -> bool {
        self.conns.iter().any(|c| c.peer.id == id && c.peer.editor)
    }

    fn close(&self, id: u64, code: u16, reason: &str) {
        if let Some(c) = self.conns.iter().find(|c| c.peer.id == id) {
            c.push(Out::Close(code, reason.to_string()));
        }
    }

    /// Asks one editor for a snapshot when enough has changed since the last one.
    fn maybe_ask_snapshot(&mut self) {
        if self.unsnapped == 0 {
            return;
        }
        let due = self.unsnapped >= SNAPSHOT_OPS
            || self.unsnapped_since.is_some_and(|t| t.elapsed() >= SNAPSHOT_AGE)
            || (self.last_commit.is_some_and(|t| t.elapsed() >= SNAPSHOT_IDLE) && self.last_snap.is_none_or(|t| t.elapsed() >= SNAPSHOT_GAP));
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
    /// Held for a whole `recheck` and while a connection checks its access and registers, so
    /// verdicts are applied in the order they were computed and none misses a new connection.
    /// Lock order: `recheck_lock` before the store, the document map or a document; never
    /// taken while holding any of them.
    recheck_lock: Mutex<()>,
    /// Bumped (under the document map lock) whenever a document's session is dropped, so a
    /// session loaded from the store meanwhile is known to be stale.
    dropped: AtomicU64,
}

/// What a connection may do now.
#[derive(Debug, PartialEq)]
enum Verdict {
    Role(Role),
    Revoked(&'static str),
}

fn verdict(s: &Store, item_id: &str, who: &Principal) -> Result<Verdict, crate::store::StoreError> {
    let who = match s.recheck_principal(who)? {
        Ok(p) => p,
        Err(reason) => return Ok(Verdict::Revoked(reason)),
    };
    let Some(item) = s.item(item_id)? else { return Ok(Verdict::Revoked("This workbook was deleted.")) };
    Ok(match s.role(&who, &item, false)? {
        Some(r) => Verdict::Role(r),
        None if s.role(&who, &item, true)?.is_some() => Verdict::Revoked("This workbook was moved to the trash."),
        None => Verdict::Revoked("Your access to this workbook was removed."),
    })
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

    /// The document's session, loaded from the store on first use. The store and the
    /// document map are never held together (a connection leaving holds the map, then a
    /// document; a commit holds a document, then the store).
    fn doc(&self, store: &Mutex<Store>, item_id: &str) -> Result<Arc<Mutex<Doc>>, String> {
        loop {
            let seen = self.dropped.load(Ordering::SeqCst);
            if let Some(d) = lock(&self.docs).get(item_id) {
                return Ok(d.clone());
            }
            let loaded = {
                let s = lock(store);
                let item = s.item(item_id).ok().flatten().ok_or("item gone")?;
                Self::load(&s, &item)?
            };
            let mut docs = lock(&self.docs);
            if let Some(d) = docs.get(item_id) {
                return Ok(d.clone());
            }
            if self.dropped.load(Ordering::SeqCst) != seen {
                // A session was dropped while we read the store: it may have committed batches
                // after our read. Load again.
                continue;
            }
            let doc = Arc::new(Mutex::new(loaded));
            docs.insert(item_id.to_string(), doc.clone());
            return Ok(doc);
        }
    }

    fn load(s: &Store, item: &Item) -> Result<Doc, String> {
        let base = s.live_base(item).map_err(|e| e.to_string())?;
        let mut log = Log::new(base.floor);
        let ops = s.live_ops_after(&item.id, base.floor).map_err(|e| e.to_string())?;
        let mut unsnapped = 0;
        for o in ops {
            let parsed: Vec<cosync::Op> = serde_json::from_str(&o.ops).map_err(|e| format!("stored operations: {e}"))?;
            if o.seq > base.seq {
                unsnapped += 1;
            }
            log.entries.push(cosync::server::Entry { seq: o.seq, cid: o.cid, id: o.batch, by: o.actor, base: None, ops: parsed });
        }
        for (actor, cid, batch, seq) in s.live_clients(&item.id).map_err(|e| e.to_string())? {
            log.last.insert((actor, cid), (batch, seq));
        }
        let ids = parse_sheets(&base.sheets);
        if !ids.is_empty() {
            log.set_sheets(ids);
            replay_sheets(&mut log, base.seq);
        }
        Ok(Doc {
            log,
            conns: Vec::new(),
            unsnapped,
            unsnapped_since: (unsnapped > 0).then(Instant::now),
            last_commit: (unsnapped > 0).then(Instant::now),
            last_snap: None,
            snap_asked: None,
            snap_turn: 0,
            retired: false,
        })
    }

    /// The file was replaced by a restored version: the session is dropped and every
    /// connection is told to reload (clients keep their unsent changes and rebase them).
    pub fn retire(&self, item: &str) {
        let doc = {
            let mut docs = lock(&self.docs);
            self.dropped.fetch_add(1, Ordering::SeqCst);
            docs.remove(item)
        };
        let Some(doc) = doc else { return };
        let mut d = lock(&doc);
        d.retired = true;
        for c in &d.conns {
            c.push(Out::Text(json(&ServerMsg::Reload)));
            c.push(Out::Close(1000, "reload".into()));
        }
    }

    /// A snapshot containing everything up to `seq` was stored; operations up to `floor` are gone.
    pub fn snapshotted(&self, item: &str, seq: u64, floor: u64) {
        let Some(d) = lock(&self.docs).get(item).cloned() else { return };
        let mut d = lock(&d);
        d.log.prune_to(floor);
        d.unsnapped = d.log.entries.iter().filter(|e| e.seq > seq).count();
        d.unsnapped_since = (d.unsnapped > 0).then(Instant::now);
        d.last_snap = Some(Instant::now());
        d.snap_asked = None;
    }

    /// Re-evaluates the access of every live connection and tells those whose access changed
    /// (see the module comment). Call after anything that can change who may see or edit what.
    /// Blocking (takes the store); no lock is held while taking another.
    pub fn recheck(&self, store: &Mutex<Store>) {
        let _serial = lock(&self.recheck_lock);
        let docs: Vec<(String, Arc<Mutex<Doc>>)> = lock(&self.docs).iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let mut sessions: Vec<(usize, u64, Principal)> = Vec::new();
        for (i, (_, d)) in docs.iter().enumerate() {
            sessions.extend(lock(d).conns.iter().filter(|c| !c.revoked).map(|c| (i, c.peer.id, c.who.clone())));
        }
        if sessions.is_empty() {
            return;
        }
        let verdicts: Vec<(usize, u64, Verdict)> = {
            let s = lock(store);
            sessions
                .into_iter()
                .filter_map(|(i, id, who)| match verdict(&s, &docs[i].0, &who) {
                    Ok(v) => Some((i, id, v)),
                    Err(e) => {
                        // Keep the connection as it is; the next check tries again.
                        tracing::error!("live: couldn't re-check access to item {}: {e}", docs[i].0);
                        None
                    }
                })
                .collect()
        };
        for (i, id, v) in verdicts {
            let (item, doc) = &docs[i];
            let mut d = lock(doc);
            let Some(c) = d.conns.iter_mut().find(|c| c.peer.id == id && !c.revoked) else { continue };
            match v {
                Verdict::Role(r) => {
                    c.role = r;
                    let editor = r >= Role::Edit;
                    if editor != c.peer.editor {
                        c.peer.editor = editor;
                        let role = if editor { "edit" } else { "view" };
                        c.push(Out::Text(json(&ServerMsg::Role { role: role.into() })));
                        tracing::info!("live: connection {id} to item {item} is now a{}", if editor { "n editor" } else { " viewer" });
                    }
                }
                Verdict::Revoked(reason) => {
                    c.revoked = true;
                    c.peer.editor = false;
                    c.push(Out::Text(json(&ServerMsg::Revoked { reason: reason.into() })));
                    c.push(Out::Close(close::REVOKED, reason.into()));
                    c.end.notify_one();
                    tracing::info!("live: connection {id} to item {item} lost access; closing it");
                }
            }
        }
    }

    fn start_ticker(self: &Arc<Self>, store: Arc<Mutex<Store>>) {
        if self.ticker.swap(true, Ordering::SeqCst) {
            return;
        }
        let hub = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut every = tokio::time::interval(SNAPSHOT_CHECK);
            let mut last_check = Instant::now();
            loop {
                every.tick().await;
                let Some(hub) = hub.upgrade() else { break };
                let docs: Vec<Arc<Mutex<Doc>>> = lock(&hub.docs).values().cloned().collect();
                for d in docs {
                    lock(&d).maybe_ask_snapshot();
                }
                if last_check.elapsed() >= RECHECK_EVERY {
                    last_check = Instant::now();
                    let store = store.clone();
                    let _ = tokio::task::spawn_blocking(move || hub.recheck(&store)).await;
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
fn peer_of(who: &Principal, role: Role, id: u64, agent: bool) -> Peer {
    let (name, user) = match who {
        Principal::User { user, .. } => (user.display_name.clone(), user.username.clone()),
        Principal::Link { .. } => (if role >= Role::Edit { "Guest (link)".to_string() } else { "Guest viewer (link)".to_string() }, String::new()),
    };
    Peer { id, name, user, color: 0, editor: role >= Role::Edit, agent, presence: None }
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
    /// The client says it is a headless agent (`agent=1`); shown to others in presence.
    pub agent: bool,
}

/// Runs one participant's connection until it closes.
pub async fn run(store: Arc<Mutex<Store>>, hub: Arc<Hub>, socket: WebSocket, j: Join) {
    hub.start_ticker(store.clone());
    let conn_id = hub.next_conn.fetch_add(1, Ordering::SeqCst) + 1;
    let (mut sink, mut stream) = socket.split();
    let kick = Arc::new(Notify::new());
    let end = Arc::new(Notify::new());

    // Load the document and register, all under the document lock so the replay and the live
    // stream join without a gap.
    let item_id = j.item.id.clone();
    let setup = {
        let (store, hub, kick, end) = (store.clone(), hub.clone(), kick.clone(), end.clone());
        let (item_id, who, since, version, cid, actor, agent) = (j.item.id.clone(), j.who.clone(), j.since, j.version, j.cid.clone(), j.actor.clone(), j.agent);
        // The last field: registered (false: only the queued farewell goes out, nothing is read).
        type Setup = (Arc<Mutex<Doc>>, mpsc::Sender<Out>, mpsc::Receiver<Out>, Role, bool);
        tokio::task::spawn_blocking(move || -> Result<Setup, String> {
            // No re-check runs from here until the connection is registered: the access checked
            // now is the one every later re-check starts from.
            let _serial = lock(&hub.recheck_lock);
            let (current, access) = {
                let s = lock(&store);
                // Counted while holding the store, so a plain save can't slip in unnoticed.
                *lock(&hub.active).entry(item_id.clone()).or_default() += 1;
                let item = s.item(&item_id).ok().flatten().ok_or("item gone")?;
                (item.version, verdict(&s, &item_id, &who).map_err(|e| e.to_string())?)
            };
            let doc = hub.doc(&store, &item_id)?;
            let role = match access {
                Verdict::Role(r) => r,
                Verdict::Revoked(reason) => {
                    // Access ended between the HTTP check and now.
                    let (tx, rx) = mpsc::channel(2);
                    let _ = tx.try_send(Out::Text(json(&ServerMsg::Revoked { reason: reason.into() })));
                    let _ = tx.try_send(Out::Close(close::REVOKED, reason.into()));
                    return Ok((doc, tx, rx, Role::View, false));
                }
            };
            let mut d = lock(&doc);
            let mut peer = peer_of(&who, role, conn_id, agent);
            let replay = if version.is_none_or(|v| v == current) { d.log.since(since).map(<[_]>::to_vec) } else { None };
            let Some(replay) = replay else {
                // The client's copy is older than what the session follows: it must reload. If its
                // last batch was committed (and its ack lost), say so first, or the reload would
                // send it again as a new batch.
                let (tx, rx) = mpsc::channel(3);
                if let Some((id, seq)) = d.log.last_commit(&cid, &actor) {
                    let _ = tx.try_send(Out::Text(json(&ServerMsg::Ack { id, seq, extra: Vec::new() })));
                }
                let _ = tx.try_send(Out::Text(json(&ServerMsg::Reload)));
                let _ = tx.try_send(Out::Close(1000, "reload".into()));
                return Ok((doc.clone(), tx, rx, role, false));
            };
            let (tx, rx) = mpsc::channel(QUEUE + 1 + replay.len());
            let used: Vec<u8> = d.conns.iter().map(|c| c.peer.color).collect();
            peer.color = (0..u8::MAX).find(|c| !used.contains(c)).unwrap_or(0);
            let peers: Vec<Peer> = d.conns.iter().map(|c| c.peer.clone()).collect();
            let _ = tx.try_send(Out::Text(json(&ServerMsg::Welcome { seq: d.log.head(), you: peer.clone(), peers, proto: cosync::PROTO })));
            for e in replay {
                let msg = ServerMsg::Ops { seq: e.seq, cid: e.cid, id: e.id, by: 0, base: e.base, ops: e.ops };
                let _ = tx.try_send(Out::Text(json(&msg)));
            }
            d.broadcast(conn_id, &ServerMsg::Join { peer: peer.clone() });
            d.conns.push(Conn { peer, tx: tx.clone(), kick, who, role, revoked: false, end });
            Ok((doc.clone(), tx, rx, role, true))
        })
        .await
    };
    let (doc, tx, mut rx, role, registered) = match setup {
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
    tracing::info!("live: connection {conn_id} joined item {item_id} ({})", if role >= Role::Edit { "editor" } else { "viewer" });

    let mut writer = tokio::spawn(async move {
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
    let fail = |code: u16, reason: &str| {
        let _ = tx.try_send(Out::Close(code, reason.to_string()));
    };
    let mut kicked = false;
    loop {
        // Not registered (access ended, or must reload): ignore whatever the client sends.
        if !registered {
            break;
        }
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
                    ClientMsg::Hello { sheets, proto } => {
                        if proto < cosync::PROTO {
                            // A protocol 1 client would apply the split cell operations as whole
                            // cells and diverge: ask for an update instead (nothing it sent is lost).
                            let _ = tx.try_send(Out::Text(json(&ServerMsg::Error { message: "This version of FreedomOffice Sheets is too old to edit this workbook live. Please update the app.".into() })));
                            fail(close::UPGRADE, "Update FreedomOffice Sheets.");
                            break;
                        }
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
                    ClientMsg::Ops { base, id, made, ops } => {
                        // The role may have changed since the connection opened (`Hub::recheck`);
                        // `commit` checks again under the document lock.
                        if !lock(&doc).is_editor(conn_id) {
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
                        let ok = tokio::task::spawn_blocking(move || commit(&store, &doc, &item, conn_id, (base, made.unwrap_or(base)), &cid, id, &actor, ops)).await.unwrap_or(false);
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
                if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(Out::Ping) {
                    kicked = true;
                    break;
                }
            }
            _ = kick.notified() => {
                kicked = true;
                break;
            }
            _ = end.notified() => break,
        }
    }
    if kicked {
        tracing::warn!("live: connection {conn_id} to item {item_id} doesn't read its messages; disconnecting it");
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
                hub.dropped.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    drop(tx);
    // A client that doesn't read is cut off at once; otherwise what is queued goes out first.
    if kicked || tokio::time::timeout(Duration::from_secs(5), &mut writer).await.is_err() {
        writer.abort();
    }
    tracing::info!("live: connection {conn_id} left item {item_id}");
}

/// Transforms, stores and distributes one batch. False when the connection must close.
#[allow(clippy::too_many_arguments)]
fn commit(store: &Mutex<Store>, doc: &Mutex<Doc>, item: &str, conn: u64, (base, made): (u64, u64), cid: &str, id: u64, actor: &str, ops: Vec<cosync::Op>) -> bool {
    let mut d = lock(doc);
    if d.retired {
        d.send(conn, &ServerMsg::Reload);
        return true;
    }
    if !d.is_editor(conn) {
        d.close(conn, close::READ_ONLY, "You have view-only access to this workbook.");
        return false;
    }
    let (sheets_before, last_before) = (d.log.sheets.clone(), d.log.last_commit(cid, actor));
    match d.log.commit_made(base, made, cid, id, actor, ops) {
        Ok(Commit::New { seq, ops, extra }) => {
            let rec = LiveOp { seq, cid: cid.to_string(), batch: id, actor: actor.to_string(), ops: serde_json::to_string(&ops).unwrap_or_default(), at: now() };
            if let Err(e) = lock(store).add_live_op(item, &rec) {
                tracing::error!("live: couldn't store a batch for item {item}: {e}");
                d.log.entries.pop();
                d.log.sheets = sheets_before;
                match last_before {
                    Some(l) => d.log.last.insert((actor.to_string(), cid.to_string()), l),
                    None => d.log.last.remove(&(actor.to_string(), cid.to_string())),
                };
                d.close(conn, 1011, "The server couldn't save your change.");
                return false;
            }
            d.send(conn, &ServerMsg::Ack { id, seq, extra });
            d.broadcast(conn, &ServerMsg::Ops { seq, cid: cid.to_string(), id, by: conn, base: Some(made.min(base)), ops });
            d.unsnapped += 1;
            if d.unsnapped_since.is_none() {
                d.unsnapped_since = Some(Instant::now());
            }
            d.last_commit = Some(Instant::now());
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

#[cfg(test)]
mod tests {
    use futures_util::FutureExt;

    use super::*;

    fn link() -> Principal {
        Principal::Link { link: crate::store::Link { id: String::new(), item_id: String::new(), role: Role::Edit, expires: None, pw_hash: None, created: 0, created_by: String::new() } }
    }

    /// Once a connection lost access, no later re-check (e.g. a stale one) gives it back.
    #[test]
    fn revoked_is_final() {
        let dir = std::env::temp_dir().join(format!("fo-live-unit-{}", util::new_id()));
        let store = Store::open(&dir.join("db"), &dir.join("files"), 5).unwrap();
        let u = store.create_user("ann", "", "", "test-password-1", false).unwrap();
        let (item, _) = store.create_file(&u.root_id, "A.xlsx", b"a", &u.id, crate::store::OnConflict::Fail).unwrap();
        let (link, _) = store.create_link(&item, Role::Edit, None, None, &u.id).unwrap();
        let store = Mutex::new(store);
        let hub = Hub::default();
        let (tx, mut rx) = mpsc::channel(16);
        let peer = Peer { id: 1, name: String::new(), user: String::new(), color: 0, editor: false, agent: false, presence: None };
        let mut d = Doc { log: Log::new(0), conns: vec![], unsnapped: 0, unsnapped_since: None, last_commit: None, last_snap: None, snap_asked: None, snap_turn: 0, retired: false };
        d.conns.push(Conn { peer, tx, kick: Arc::new(Notify::new()), who: Principal::Link { link }, role: Role::View, revoked: true, end: Arc::new(Notify::new()) });
        let doc = Arc::new(Mutex::new(d));
        lock(&hub.docs).insert(item.id.clone(), doc.clone());
        hub.recheck(&store);
        assert!(rx.try_recv().is_err(), "a revoked connection isn't given edit back");
        lock(&doc).conns[0].revoked = false;
        hub.recheck(&store);
        assert!(matches!(rx.try_recv(), Ok(Out::Text(t)) if t.contains("\"role\"")), "a live one is");
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A participant that doesn't read its messages is disconnected when its queue is full,
    /// instead of the server queueing for it without bound.
    #[test]
    fn a_connection_that_does_not_read_is_kicked() {
        let peer = |id| Peer { id, name: String::new(), user: String::new(), color: 0, editor: true, agent: false, presence: None };
        let (tx, _rx) = mpsc::channel(2);
        let kick = Arc::new(Notify::new());
        let d = Doc { log: Log::new(0), conns: vec![Conn { peer: peer(1), tx, kick: kick.clone(), who: link(), role: Role::Edit, revoked: false, end: Arc::new(Notify::new()) }], unsnapped: 0, unsnapped_since: None, last_commit: None, last_snap: None, snap_asked: None, snap_turn: 0, retired: false };
        for _ in 0..2 {
            d.broadcast(9, &ServerMsg::Leave { id: 9 });
        }
        assert!(kick.notified().now_or_never().is_none(), "not while there is room");
        d.broadcast(9, &ServerMsg::Leave { id: 9 });
        assert!(kick.notified().now_or_never().is_some(), "kicked when full");
    }

    /// A pause in editing gets the saved file up to date soon (not only after many changes or
    /// minutes), but idle snapshots don't flood the version history.
    #[test]
    fn a_pause_in_editing_asks_for_a_snapshot() {
        let peer = |id| Peer { id, name: String::new(), user: String::new(), color: 0, editor: true, agent: false, presence: None };
        let (tx, mut rx) = mpsc::channel(16);
        let ago = |s: u64| Instant::now().checked_sub(Duration::from_secs(s));
        let mut d = Doc { log: Log::new(0), conns: vec![Conn { peer: peer(1), tx, kick: Arc::new(Notify::new()), who: link(), role: Role::Edit, revoked: false, end: Arc::new(Notify::new()) }], unsnapped: 1, unsnapped_since: ago(1), last_commit: ago(1), last_snap: None, snap_asked: None, snap_turn: 0, retired: false };
        let asked = |rx: &mut mpsc::Receiver<Out>| {
            let mut n = 0;
            while let Ok(o) = rx.try_recv() {
                if matches!(o, Out::Text(t) if t == json(&ServerMsg::Snapshot)) {
                    n += 1;
                }
            }
            n
        };
        d.maybe_ask_snapshot();
        assert_eq!(asked(&mut rx), 0, "still typing");
        d.last_commit = ago(SNAPSHOT_IDLE.as_secs() + 1);
        d.unsnapped_since = d.last_commit;
        d.maybe_ask_snapshot();
        assert_eq!(asked(&mut rx), 1, "idle: asked");
        d.maybe_ask_snapshot();
        assert_eq!(asked(&mut rx), 0, "not again while the request is pending");

        // Shortly after a snapshot, a pause doesn't ask again; later it does.
        d.snap_asked = None;
        d.last_snap = ago(5);
        d.maybe_ask_snapshot();
        assert_eq!(asked(&mut rx), 0, "a snapshot was stored a moment ago");
        d.last_snap = ago(SNAPSHOT_GAP.as_secs() + 1);
        d.maybe_ask_snapshot();
        assert_eq!(asked(&mut rx), 1);

        // Nothing unsnapshotted: nothing to ask.
        d.snap_asked = None;
        d.unsnapped = 0;
        d.maybe_ask_snapshot();
        assert_eq!(asked(&mut rx), 0);
    }
}
