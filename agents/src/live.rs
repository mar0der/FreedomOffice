//! One live editing session, driven by the caller: connect, `pump` incoming messages, make
//! edits, disconnect and reconnect. The OT bookkeeping is `cosync::client::Session`, the same
//! code the app runs; remote operations are applied to an in-memory [`Book`].
//!
//! Flow (as in the app): download the file (`X-Live-Seq`, `X-Live-Sheets`, `ETag` say where the
//! session starts), open the WebSocket with `since=<seq>&v=<version>&cid=<client>` and the
//! bearer token, send `Hello` with the sheet ids, get `Welcome` and the replay of everything
//! committed since, then exchange `Ops` / `Ack` / `Presence`. After a lost connection the client
//! keeps editing offline and reconnects with `since=<last seq applied>`; its unacknowledged
//! batch is sent again and the server recognises it by (cid, batch id).

use std::collections::BTreeMap;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use cosync::client::{Batch, Session, Step};
use cosync::{CellData, ClientMsg, Op, Peer, Presence, Rect, ServerMsg, SheetId, close};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::api::Api;
use crate::book::Book;

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    /// Batches sent (including re-sends after a reconnect).
    pub batches_sent: u64,
    /// Operations in the batches sent first time (not re-sends).
    pub ops_sent: u64,
    pub acks: u64,
    /// Committed batches from other clients applied.
    pub remote_batches: u64,
    /// Our own batches seen again in a replay after a reconnect (ack was lost).
    pub replayed_own: u64,
    pub presence_sent: u64,
    pub connects: u64,
    /// Connections lost after the first.
    pub reconnects: u64,
    pub closes: Vec<u16>,
    pub snapshot_requests: u64,
}

pub struct Live {
    pub api: Api,
    pub item: String,
    pub cid: String,
    pub agent: bool,
    pub book: Book,
    pub sess: Session,
    /// Version the file was downloaded at (sent on the first join only).
    version: Option<i64>,
    ws: Option<Ws>,
    welcomed: bool,
    caught_up: bool,
    catch_target: u64,
    pub me: Option<Peer>,
    pub peers: BTreeMap<u64, Peer>,
    pub editor: bool,
    pub stats: Stats,
    /// The session can't continue (set with the reason): access removed, view-only, reload needed.
    pub stopped: Option<String>,
    /// Things worth telling the caller (e.g. the base was read from the xlsx only).
    pub notes: Vec<String>,
    pub last_error: Option<String>,
}

fn version_of_etag(etag: &str) -> Option<i64> {
    etag.trim().trim_start_matches("W/").trim_matches('"').strip_prefix('v')?.split('-').next()?.parse().ok()
}

fn would_block(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(io) if matches!(io.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
}

impl Live {
    /// Downloads the file, reads its workbook as the starting state and connects.
    pub fn open(api: &Api, item: &str, agent: bool) -> Result<Live, String> {
        let mut live = Live::prepare(api, item, agent)?;
        live.connect()?;
        Ok(live)
    }

    /// Like `open`, without connecting.
    pub fn prepare(api: &Api, item: &str, agent: bool) -> Result<Live, String> {
        let r = api.content(item)?;
        let seq: u64 = r.header("x-live-seq").and_then(|v| v.parse().ok()).ok_or("This server doesn't support live co-editing.")?;
        let version = match r.header("etag").and_then(version_of_etag) {
            Some(v) => v,
            None => api.item(item)?.version,
        };
        let book = Book::from_xlsx(&r.body, r.header("x-live-sheets").unwrap_or(""))?;
        let mut notes = Vec::new();
        if r.header("x-live-state").is_some() {
            notes.push("The file was last saved by a live app session: the starting state is read from the .xlsx (values and formulas only, no formats).".to_string());
        }
        let mut cid = [0u8; 6];
        getrandom::fill(&mut cid).map_err(|e| e.to_string())?;
        let cid = format!("{}-{}", if agent { "agent" } else { "client" }, cid.iter().map(|b| format!("{b:02x}")).collect::<String>());
        let mut sess = Session::new(cid.clone(), seq);
        sess.offline();
        Ok(Live {
            api: api.clone(),
            item: item.to_string(),
            cid,
            agent,
            book,
            sess,
            version: Some(version),
            ws: None,
            welcomed: false,
            caught_up: false,
            catch_target: 0,
            me: None,
            peers: BTreeMap::new(),
            editor: true,
            stats: Stats::default(),
            stopped: None,
            notes,
            last_error: None,
        })
    }

    pub fn connected(&self) -> bool {
        self.ws.is_some()
    }

    /// Connected, caught up with the server and nothing waiting for an acknowledgement.
    pub fn synced(&self) -> bool {
        self.ws.is_some() && self.caught_up && !self.sess.has_pending()
    }

    pub fn seq(&self) -> u64 {
        self.sess.seq
    }

    /// (Re)connects from the last sequence number applied.
    pub fn connect(&mut self) -> Result<(), String> {
        if self.ws.is_some() {
            return Ok(());
        }
        if let Some(why) = &self.stopped {
            return Err(why.clone());
        }
        let url = self.api.live_url(&self.item, self.sess.seq, if self.welcomed { None } else { self.version }, &self.cid, self.agent);
        let mut req = url.into_client_request().map_err(|e| e.to_string())?;
        req.headers_mut().insert("Authorization", format!("Bearer {}", self.api.token).parse().map_err(|_| "Invalid sign-in token.".to_string())?);
        let (ws, _) = tungstenite::connect(req).map_err(|e| match e {
            tungstenite::Error::Http(r) => format!("The server refused the live connection (HTTP {})", r.status().as_u16()),
            e => format!("Couldn't open the live connection ({e})"),
        })?;
        match ws.get_ref() {
            MaybeTlsStream::Plain(s) => s.set_read_timeout(Some(Duration::from_millis(20))),
            MaybeTlsStream::NativeTls(s) => s.get_ref().set_read_timeout(Some(Duration::from_millis(20))),
            _ => Ok(()),
        }
        .map_err(|e| e.to_string())?;
        self.ws = Some(ws);
        self.caught_up = false;
        self.sess.offline();
        if self.stats.connects > 0 {
            self.stats.reconnects += 1;
        }
        self.stats.connects += 1;
        self.send(ClientMsg::Hello { sheets: self.book.sheet_ids() });
        Ok(())
    }

    /// Drops the connection (as a network failure would); local edits keep being buffered.
    pub fn disconnect(&mut self) {
        if let Some(mut ws) = self.ws.take() {
            let _ = ws.close(None);
            let _ = ws.flush();
        }
        self.caught_up = false;
        self.sess.offline();
        self.peers.clear();
    }

    fn send(&mut self, m: ClientMsg) {
        let Some(ws) = &mut self.ws else { return };
        let text = serde_json::to_string(&m).unwrap_or_default();
        if let Err(e) = ws.send(Message::Text(text.into())) {
            self.lost(None, format!("{e}"));
        }
    }

    fn send_batch(&mut self, b: Batch, resend: bool) {
        self.stats.batches_sent += 1;
        if !resend {
            self.stats.ops_sent += b.ops.len() as u64;
        }
        self.send(ClientMsg::Ops { base: b.base, id: b.id, ops: b.ops });
    }

    /// The connection ended.
    fn lost(&mut self, code: Option<u16>, reason: String) {
        self.ws = None;
        self.caught_up = false;
        self.sess.offline();
        self.peers.clear();
        self.last_error = Some(reason.clone());
        if let Some(c) = code {
            self.stats.closes.push(c);
            let fatal = match c {
                close::READ_ONLY => Some("You have view-only access to this workbook."),
                close::REVOKED => Some("Your access to this workbook was removed."),
                close::INVALID => Some("The server refused a change as invalid."),
                _ => None,
            };
            if let Some(f) = fatal {
                self.stopped = Some(f.to_string());
            }
        }
    }

    /// Reads and handles messages for up to `wait` (at least one read attempt).
    pub fn pump(&mut self, wait: Duration) {
        let end = Instant::now() + wait;
        loop {
            let Some(ws) = &mut self.ws else { return };
            match ws.read() {
                Ok(Message::Text(t)) => match serde_json::from_str::<ServerMsg>(&t) {
                    Ok(m) => self.handle(m),
                    Err(_) => self.lost(None, "The server sent something unexpected.".into()),
                },
                Ok(Message::Close(f)) => {
                    let _ = ws.flush();
                    self.lost(f.as_ref().map(|f| f.code.into()), f.map(|f| f.reason.to_string()).unwrap_or_default());
                }
                Ok(_) => {}
                Err(e) if would_block(&e) => {}
                Err(e) => self.lost(None, e.to_string()),
            }
            if Instant::now() >= end {
                return;
            }
        }
    }

    /// Pumps until `synced()` or the time is up.
    pub fn wait_synced(&mut self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if self.synced() {
                return true;
            }
            if self.ws.is_none() {
                return false;
            }
            self.pump(Duration::from_millis(20));
        }
        self.synced()
    }

    fn handle(&mut self, m: ServerMsg) {
        match m {
            ServerMsg::Welcome { seq, you, peers } => {
                self.editor = you.editor;
                self.me = Some(you);
                self.peers = peers.into_iter().map(|p| (p.id, p)).collect();
                self.welcomed = true;
                self.catch_target = seq;
                self.check_caught_up();
            }
            ServerMsg::Ops { seq, cid, id, ops, .. } => {
                let step = self.sess.remote(seq, &cid, id, ops);
                self.stepped(step);
                self.check_caught_up();
            }
            ServerMsg::Ack { id, seq, extra } => {
                self.stats.acks += 1;
                let step = self.sess.ack(id, seq, extra);
                self.stepped(step);
            }
            ServerMsg::Join { peer } => {
                self.peers.insert(peer.id, peer);
            }
            ServerMsg::Leave { id } => {
                self.peers.remove(&id);
            }
            ServerMsg::Presence { id, p } => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.presence = Some(p);
                }
            }
            ServerMsg::Snapshot => {
                // Writing a snapshot needs the app's workbook engine (see agents/README.md).
                self.stats.snapshot_requests += 1;
            }
            ServerMsg::Reload => {
                self.stopped = Some("The server no longer has the changes since this copy was downloaded: open the item again.".into());
                self.disconnect();
            }
            ServerMsg::Error { message } => self.last_error = Some(message),
            ServerMsg::Role { role } => self.editor = role == "edit",
            ServerMsg::Revoked { reason } => {
                self.stopped = Some(reason);
                self.disconnect();
            }
        }
    }

    fn stepped(&mut self, step: Step) {
        self.book.apply_all(&step.apply);
        if !step.own {
            self.stats.remote_batches += 1;
        }
        if let Some(b) = step.send {
            self.send_batch(b, false);
        }
    }

    fn check_caught_up(&mut self) {
        if self.welcomed && !self.caught_up && self.ws.is_some() && self.sess.seq >= self.catch_target {
            self.caught_up = true;
            let had = self.sess.inflight.is_some();
            if let Some(b) = self.sess.resume() {
                self.send_batch(b, had);
            }
        }
    }

    /// Local changes: applied to the in-memory workbook now, sent as soon as possible (buffered
    /// while offline).
    pub fn edit(&mut self, ops: Vec<Op>) {
        if ops.is_empty() {
            return;
        }
        self.book.apply_all(&ops);
        if let Some(b) = self.sess.local(ops) {
            self.send_batch(b, false);
        }
    }

    pub fn set_cell(&mut self, s: SheetId, r: u32, c: u32, data: Option<CellData>) {
        self.edit(vec![Op::Cell { s, r, c, v: data.map(Box::new), adj: vec![] }]);
    }

    pub fn send_presence(&mut self, s: SheetId, r: u32, c: u32, editing: bool) {
        self.stats.presence_sent += 1;
        let p = Presence { s, r, c, sel: Rect { r1: r, c1: c, r2: r, c2: c }, editing };
        self.send(ClientMsg::Presence { p });
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.disconnect();
    }
}
