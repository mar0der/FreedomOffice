//! One live editing session, driven by the caller: connect, `pump` incoming messages, make
//! edits, disconnect and reconnect. The OT bookkeeping is `cosync::client::Session`, the same
//! code the app runs; remote operations are applied to an in-memory `Workbook` of the app's own
//! model (formats, merges, formulas and all), exactly as the app applies them.
//!
//! Flow (as in the app): download the file (`X-Live-Seq`, `X-Live-Sheets`, `ETag` say where the
//! session starts), open the WebSocket with `since=<seq>&v=<version>&cid=<client>` and the
//! bearer token, send `Hello` with the sheet ids, get `Welcome` and the replay of everything
//! committed since, then exchange `Ops` / `Ack` / `Presence`. After a lost connection the client
//! keeps editing offline and reconnects with `since=<last seq applied>`; its unacknowledged
//! batch is sent again and the server recognises it by (cid, batch id). The server's `Snapshot`
//! request is answered like the app does: the workbook as .xlsx plus its lossless live state go
//! to `/live/snapshot` once everything is acknowledged, which becomes a version.

use std::collections::BTreeMap;

use serde_json::{Value as Json, json};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use cosync::client::{Batch, Replaced, Session, Step};
use cosync::{CellData, CellWrite, ClientMsg, Op, Peer, Presence, Rect, ServerMsg, SheetId, close};
use freedom_sheets::live::{self, ApplyState};
use freedom_sheets::model::{Sheet, Workbook};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::api::Api;

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
    /// Snapshots uploaded (each normally becomes a version).
    pub snapshots: u64,
    /// Own edits that someone else's concurrent edit replaced.
    pub replaced: u64,
    /// Times the server asked this client to start again from the file's latest version.
    pub reloads: u64,
}

/// One of this client's own cell edits that a concurrent edit by someone else replaced (the
/// same event the app shows as "Your change to B3 was replaced by Alice's").
#[derive(Clone, Debug, PartialEq)]
pub struct ReplacedEvent {
    pub sheet: String,
    /// A1 name of the cell.
    pub cell: String,
    /// True when it was the cell's format, not its value or formula.
    pub format: bool,
    /// What this client had entered (formula, value, or "formatting").
    pub yours: String,
    /// Display name of who replaced it.
    pub by: String,
}

impl ReplacedEvent {
    pub fn message(&self) -> String {
        let cell = format!("{}!{}", self.sheet, self.cell);
        let whose = if self.by.is_empty() { "someone else's".to_string() } else { format!("{}'s", self.by) };
        if self.format {
            format!("Your formatting of {cell} was replaced by {whose}.")
        } else {
            format!("Your change to {cell} ({}) was replaced by {whose}.", self.yours)
        }
    }

    pub fn to_json(&self) -> Json {
        json!({
            "sheet": self.sheet, "cell": self.cell, "part": if self.format { "format" } else { "value" },
            "yours": self.yours, "replaced_by": self.by, "message": self.message(),
        })
    }
}

pub struct Live {
    pub api: Api,
    pub item: String,
    pub cid: String,
    pub agent: bool,
    /// The workbook (read it through [`Live::workbook`] to have formulas calculated).
    pub wb: Workbook,
    apply_state: ApplyState,
    needs_recalc: bool,
    pub sess: Session,
    /// Version the file was downloaded at (sent on the first join only).
    version: Option<i64>,
    ws: Option<Ws>,
    welcomed: bool,
    caught_up: bool,
    catch_target: u64,
    pub me: Option<Peer>,
    /// The presence last sent (the server doesn't echo it back to us).
    pub my_presence: Option<Presence>,
    pub peers: BTreeMap<u64, Peer>,
    pub editor: bool,
    pub stats: Stats,
    /// The session can't continue (set with the reason): access removed, view-only, reload needed.
    pub stopped: Option<String>,
    /// Things worth telling the caller (e.g. the base was read from the xlsx only).
    pub notes: Vec<String>,
    pub last_error: Option<String>,
    /// The server asked for a snapshot and it hasn't been uploaded yet.
    snapshot_wanted: bool,
    /// Local changes made since the last snapshot was uploaded.
    unsaved: bool,
    /// The last version uploaded as a snapshot.
    pub last_snapshot: Option<i64>,
    /// Own edits that were replaced and not yet reported (see [`Live::take_replaced`]).
    replaced: Vec<ReplacedEvent>,
    /// The server speaks protocol 1 (its Welcome has no `proto`): only whole `Cell` operations
    /// are sent, without `made`, and nothing is reported as replaced.
    pub legacy: bool,
    /// Set by a reload: the connection was dropped on purpose and should be reopened.
    reconnect: bool,
}

fn version_of_etag(etag: &str) -> Option<i64> {
    etag.trim()
        .trim_start_matches("W/")
        .trim_matches('"')
        .strip_prefix('v')?
        .split('-')
        .next()?
        .parse()
        .ok()
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

    /// The latest version of the file as the app joins from it: the workbook (with the lossless
    /// live state when the version has one), the sequence number the session starts at and the
    /// version.
    fn download(api: &Api, item: &str) -> Result<(Workbook, u64, i64), String> {
        let r = api.content(item)?;
        let seq: u64 = r
            .header("x-live-seq")
            .and_then(|v| v.parse().ok())
            .ok_or("This server doesn't support live co-editing.")?;
        let version = match r.header("etag").and_then(version_of_etag) {
            Some(v) => v,
            None => api.item(item)?.version,
        };
        // As the app joins: the file, the sheets' live ids and, when the version has one, its
        // lossless live state.
        let mut wb = freedom_sheets::io::load_xlsx_bytes(&r.body)?;
        if wb.sheets.is_empty() {
            wb.sheets.push(Sheet::new("Sheet1"));
        }
        live::assign_ids(&mut wb, r.header("x-live-sheets").unwrap_or(""));
        if r.header("x-live-state").is_some() {
            let state = api.live_state(item, version)?;
            wb = live::join_workbook(wb, Some(&state));
        }
        live::sort_sheets(&mut wb);
        wb.recalc();
        Ok((wb, seq, version))
    }

    /// Like `open`, without connecting.
    pub fn prepare(api: &Api, item: &str, agent: bool) -> Result<Live, String> {
        let (wb, seq, version) = Live::download(api, item)?;
        let notes = Vec::new();
        let mut cid = [0u8; 6];
        getrandom::fill(&mut cid).map_err(|e| e.to_string())?;
        let cid = format!(
            "{}-{}",
            if agent { "agent" } else { "client" },
            cid.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let mut sess = Session::new(cid.clone(), seq);
        sess.offline();
        Ok(Live {
            api: api.clone(),
            item: item.to_string(),
            cid,
            agent,
            wb,
            apply_state: ApplyState::default(),
            needs_recalc: false,
            sess,
            version: Some(version),
            ws: None,
            welcomed: false,
            caught_up: false,
            catch_target: 0,
            me: None,
            my_presence: None,
            peers: BTreeMap::new(),
            editor: true,
            stats: Stats::default(),
            stopped: None,
            notes,
            last_error: None,
            snapshot_wanted: false,
            unsaved: false,
            last_snapshot: None,
            replaced: Vec::new(),
            legacy: false,
            reconnect: false,
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
        let url = self.api.live_url(
            &self.item,
            self.sess.seq,
            if self.welcomed { None } else { self.version },
            &self.cid,
            self.agent,
        );
        let mut req = url.into_client_request().map_err(|e| e.to_string())?;
        req.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", self.api.token)
                .parse()
                .map_err(|_| "Invalid sign-in token.".to_string())?,
        );
        let (ws, _) = tungstenite::connect(req).map_err(|e| match e {
            tungstenite::Error::Http(r) => format!(
                "The server refused the live connection (HTTP {})",
                r.status().as_u16()
            ),
            e => format!("Couldn't open the live connection ({e})"),
        })?;
        match ws.get_ref() {
            MaybeTlsStream::Plain(s) => s.set_read_timeout(Some(Duration::from_millis(20))),
            MaybeTlsStream::NativeTls(s) => s
                .get_ref()
                .set_read_timeout(Some(Duration::from_millis(20))),
            _ => Ok(()),
        }
        .map_err(|e| e.to_string())?;
        self.ws = Some(ws);
        self.reconnect = false;
        self.caught_up = false;
        self.sess.offline();
        if self.stats.connects > 0 {
            self.stats.reconnects += 1;
        }
        self.stats.connects += 1;
        self.send(ClientMsg::Hello {
            sheets: self.wb.sheets.iter().map(|s| s.uid).collect(),
            proto: cosync::PROTO,
        });
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
        self.send(ClientMsg::Ops {
            base: b.base,
            id: b.id,
            made: (b.made < b.base && !self.legacy).then_some(b.made),
            ops: b.ops,
        });
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
                close::UPGRADE => Some("This version of fsheets is too old for the server."),
                _ => None,
            };
            if let Some(f) = fatal {
                self.stopped = Some(f.to_string());
            }
        }
    }

    /// Reads and handles messages for up to `wait` (at least one read attempt).
    pub fn pump(&mut self, wait: Duration) {
        self.pump_messages(wait);
        self.answer_snapshot_request();
    }

    fn pump_messages(&mut self, wait: Duration) {
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
                    self.lost(
                        f.as_ref().map(|f| f.code.into()),
                        f.map(|f| f.reason.to_string()).unwrap_or_default(),
                    );
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
                // After a reload the connection is ours to reopen.
                if self.reconnect && self.stopped.is_none() && self.connect().is_ok() {
                    continue;
                }
                return false;
            }
            self.pump(Duration::from_millis(20));
        }
        self.synced()
    }

    fn handle(&mut self, m: ServerMsg) {
        match m {
            ServerMsg::Welcome { seq, you, peers, proto } => {
                // An older server only knows whole cells: send only those (also what was made
                // before this Welcome and is still waiting).
                self.legacy = proto < cosync::PROTO;
                if self.legacy {
                    if let Some(b) = &mut self.sess.inflight {
                        b.ops = live::collapse_cells(&self.wb, &b.ops);
                    }
                    self.sess.buffer = live::collapse_cells(&self.wb, &self.sess.buffer);
                }
                self.editor = you.editor;
                self.me = Some(you);
                self.peers = peers.into_iter().map(|p| (p.id, p)).collect();
                self.welcomed = true;
                self.catch_target = seq;
                self.check_caught_up();
            }
            ServerMsg::Ops {
                seq,
                cid,
                id,
                by,
                base,
                ops,
            } => {
                let step = self.sess.remote_based(seq, &cid, id, base, ops);
                let who = self.peers.get(&by).map(|p| p.name.clone()).unwrap_or_default();
                self.note_replaced(&step.replaced, &who);
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
                self.stats.snapshot_requests += 1;
                self.snapshot_wanted = true;
            }
            ServerMsg::Reload => {
                // The file got a newer version (someone's snapshot) or the log was cut short
                // while we were away: start again from the latest version, with our
                // unacknowledged changes on top, as the app does.
                self.disconnect();
                if let Err(e) = self.reload() {
                    self.stopped = Some(format!("Couldn't get the latest version after the server asked for a reload: {e}"));
                }
            }
            ServerMsg::Error { message } => self.last_error = Some(message),
            ServerMsg::Role { role } => self.editor = role == "edit",
            ServerMsg::Revoked { reason } => {
                self.stopped = Some(reason);
                self.disconnect();
            }
        }
    }

    /// Replaces the workbook by the latest version of the file and puts the changes the server
    /// has not acknowledged on top of it; the next `connect` joins from there.
    fn reload(&mut self) -> Result<(), String> {
        let (mut wb, seq, version) = Live::download(&self.api, &self.item)?;
        let pending = self.sess.rebase_on_reload(seq);
        live::apply_ops(&mut wb, &pending, false, &mut self.apply_state);
        self.wb = wb;
        self.needs_recalc = true;
        self.version = Some(version);
        self.welcomed = false;
        self.reconnect = true;
        self.stats.reloads += 1;
        Ok(())
    }

    fn note_replaced(&mut self, replaced: &[Replaced], who: &str) {
        if self.legacy {
            return;
        }
        for r in replaced {
            let sheet = live::index_of(&self.wb, r.s).map(|i| self.wb.sheets[i].name.clone()).unwrap_or_default();
            self.stats.replaced += 1;
            self.replaced.push(ReplacedEvent {
                sheet,
                cell: freedom_sheets::model::cell_name(r.r, r.c),
                format: r.part == CellWrite::Format,
                yours: live::entered_text(&r.mine),
                by: who.to_string(),
            });
        }
    }

    /// The own edits that others' concurrent edits replaced since the last call.
    pub fn take_replaced(&mut self) -> Vec<ReplacedEvent> {
        std::mem::take(&mut self.replaced)
    }

    fn stepped(&mut self, step: Step) {
        self.apply(&step.apply, true);
        if !step.own {
            self.stats.remote_batches += 1;
        }
        if let Some(b) = step.send {
            self.send_batch(b, false);
        }
    }

    fn check_caught_up(&mut self) {
        if self.welcomed
            && !self.caught_up
            && self.ws.is_some()
            && self.sess.seq >= self.catch_target
        {
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
        self.apply(&ops, false);
        self.unsaved = true;
        let ops = if self.legacy { live::collapse_cells(&self.wb, &ops) } else { ops };
        if let Some(b) = self.sess.local(ops) {
            self.send_batch(b, false);
        }
    }

    fn apply(&mut self, ops: &[Op], remote: bool) {
        let applied = live::apply_ops(&mut self.wb, ops, remote, &mut self.apply_state);
        if applied.full || !applied.cells.is_empty() {
            self.needs_recalc = true;
        }
    }

    /// The workbook with every formula calculated.
    pub fn workbook(&mut self) -> &Workbook {
        if self.needs_recalc {
            self.wb.recalc();
            self.needs_recalc = false;
        }
        &self.wb
    }

    /// The first sheet in tab order.
    pub fn first_sheet(&self) -> Option<SheetId> {
        self.wb.sheets.first().map(|s| s.uid)
    }

    /// What a cell holds as it travels (value, formula text, style content).
    pub fn cell_data(&self, s: SheetId, r: u32, c: u32) -> Option<CellData> {
        let i = live::index_of(&self.wb, s)?;
        let sh = &self.wb.sheets[i];
        sh.get(r, c)
            .map(|cell| live::cell_data(&self.wb, sh, r, c, cell))
    }

    /// Number of non-empty cells.
    pub fn cell_count(&self) -> usize {
        self.wb
            .sheets
            .iter()
            .map(|s| s.rows.values().map(|r| r.len()).sum::<usize>())
            .sum()
    }

    /// Differences from another session's workbook in everything that is synchronised.
    pub fn differences(&self, other: &Live) -> Vec<String> {
        live::compare(&self.wb, &other.wb)
    }

    /// Sets a cell's value or formula (and rich text); its format stays.
    pub fn set_content(&mut self, s: SheetId, r: u32, c: u32, data: Option<CellData>) {
        let v = data
            .map(|d| CellData { st: None, ..d })
            .filter(|d| !d.v.is_empty() || d.f.is_some() || d.rich.is_some())
            .map(Box::new);
        self.edit(vec![Op::Content { s, r, c, v, adj: vec![] }]);
    }

    /// Sets a cell's format (style content, None = default); its content stays.
    pub fn set_format(&mut self, s: SheetId, r: u32, c: u32, st: Option<Json>) {
        self.edit(vec![Op::Format { s, r, c, st }]);
    }

    /// Sets the whole cell, content and format together (what protocol 1 clients did for
    /// every change).
    pub fn set_cell(&mut self, s: SheetId, r: u32, c: u32, data: Option<CellData>) {
        self.edit(vec![Op::Cell {
            s,
            r,
            c,
            v: data.map(Box::new),
            adj: vec![],
        }]);
    }

    pub fn send_presence(&mut self, s: SheetId, r: u32, c: u32, editing: bool) {
        let sel = Rect {
            r1: r,
            c1: c,
            r2: r,
            c2: c,
        };
        self.send_selection(s, r, c, sel, editing);
    }

    /// Presence with a selected range (the cell `r`,`c` is the active one inside it).
    pub fn send_selection(&mut self, s: SheetId, r: u32, c: u32, sel: Rect, editing: bool) {
        self.stats.presence_sent += 1;
        let p = Presence {
            s,
            r,
            c,
            sel,
            editing,
        };
        self.my_presence = Some(p.clone());
        self.send(ClientMsg::Presence { p });
    }

    /// Whether the session holds changes no snapshot has covered yet.
    pub fn unsaved(&self) -> bool {
        self.unsaved
    }

    /// Uploads the workbook and its live state as a snapshot (a version of the file). Needs the
    /// connection to be caught up with nothing waiting for an acknowledgement, because the
    /// snapshot claims to hold exactly the changes up to the sequence number it names. Returns
    /// the version the file has afterwards.
    pub fn snapshot(&mut self) -> Result<i64, String> {
        if !self.editor {
            return Err("You have view-only access to this workbook.".into());
        }
        if !self.synced() {
            return Err("The session isn't caught up with the server yet.".into());
        }
        let seq = self.sess.seq;
        self.workbook();
        let data = freedom_sheets::io::xlsx_bytes(&self.wb)?;
        let state = live::encode_state(&live::state_ops(&self.wb))?;
        let item =
            self.api
                .live_snapshot(&self.item, seq, &live::sheets_meta(&self.wb), &data, &state)?;
        self.snapshot_wanted = false;
        self.unsaved = false;
        self.stats.snapshots += 1;
        self.last_snapshot = Some(item.version);
        Ok(item.version)
    }

    fn answer_snapshot_request(&mut self) {
        if self.snapshot_wanted
            && self.synced()
            && self.editor
            && let Err(e) = self.snapshot()
        {
            self.last_error = Some(format!("Snapshot failed: {e}"));
            // Ask again when the server does, rather than at every pump.
            self.snapshot_wanted = false;
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.disconnect();
    }
}
