//! Live co-editing in the app (FreedomOffice Server documents): joining the document's live
//! session when it is opened, sending what commands change (found by comparing the undo
//! snapshot with the result), applying everyone else's changes, presence like Excel
//! co-authoring (coloured cell borders with name flags, avatars next to Share), per-user
//! undo, snapshots for the server's version history, and reconnecting.

use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cosync::client::{Batch, Session};
use cosync::{ClientMsg, Op, Peer, Presence, Rect as CRect, ServerMsg, SheetId};
use eframe::egui::{self, Align2, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use super::{App, Scope};
use crate::cloud::{CloudDoc, DriveItem, Kind, Provider, ServerSession};
use crate::live::conn::{self, Conn, Event};
use crate::live::{self, ApplyState, Before};
use crate::model::{Range, Workbook};

/// Excel-like colours for other people's cursors.
const COLORS: [u32; 8] = [0x0078D4, 0xC239B3, 0x00A36C, 0xCA5010, 0x8764B8, 0xE3008C, 0x038387, 0x986F0B];

pub fn peer_color(slot: u8) -> Color32 {
    super::theme::rgb(COLORS[slot as usize % COLORS.len()])
}

/// How long a name flag stays up after someone moves.
const FLAG_SECS: f32 = 3.0;
const PRESENCE_EVERY: Duration = Duration::from_millis(100);

/// Someone else in the session.
pub struct Seen {
    pub peer: Peer,
    pub moved: Instant,
}

/// One undoable local change: the operations that reverse it (transformed over everything
/// applied since), and whether someone else changed the same targets since.
struct Undo {
    inverse: Vec<Op>,
    blocked: bool,
}

enum Job {
    Snapshot(u64, Result<DriveItem, String>),
    Reloaded(Result<CloudDoc, String>),
}

pub struct Live {
    pub item: String,
    pub url: String,
    server: Option<Arc<ServerSession>>,
    pub sess: Session,
    conn: Option<Conn>,
    /// The server's sequence number from Welcome, to reach before resending.
    catch_up: Option<u64>,
    /// Connected and caught up.
    pub online: bool,
    state: ApplyState,
    pub editor: bool,
    pub me: Option<Peer>,
    pub peers: BTreeMap<u64, Seen>,
    befores: Vec<Before>,
    structs: Vec<(SheetId, bool, u32, i64)>,
    /// Undo scopes of the change being recorded.
    scopes: Vec<Scope>,
    /// Applying remote or undo operations: don't record them as local changes.
    applying: bool,
    undo: Vec<Undo>,
    redo: Vec<Undo>,
    /// Some(false) while an undo is applied, Some(true) for a redo.
    undoing: Option<bool>,
    /// Inverses of the local changes not yet acknowledged (oldest first) and how many of them
    /// are in the batch in flight: to rebuild the server's state when concurrency starts.
    units: VecDeque<Vec<Op>>,
    sent_units: usize,
    /// The server's state while local changes are pending and others' arrive; adopted when
    /// everything is acknowledged (so every participant ends with exactly the same workbook).
    confirmed: Option<Workbook>,
    confirmed_state: ApplyState,
    last_presence: Option<Presence>,
    presence_at: Instant,
    retry_at: Option<Instant>,
    backoff: u32,
    /// Fatal: don't reconnect (access removed, item gone).
    pub stopped: Option<String>,
    snap_wanted: bool,
    snap_busy: bool,
    pub snap_seq: u64,
    reloading: bool,
    tx: Sender<Job>,
    rx: Receiver<Job>,
    /// Tests: a full copy of the workbook after the last change, to check that the generated
    /// operations reproduce every change exactly (and their inverses undo it).
    pub verify: Option<Workbook>,
    pub verify_errors: Vec<String>,
    /// Tests: times the state adopted at quiescence differed from the transformed one, and
    /// times others' changes arrived while ours were pending.
    pub repaired: usize,
    pub concurrent: usize,
}

impl Live {
    fn new(item: String, url: String, server: Option<Arc<ServerSession>>, seq: u64) -> Self {
        let (tx, rx) = channel();
        Live {
            item,
            url,
            server,
            sess: Session::new(client_id(), seq),
            conn: None,
            catch_up: None,
            online: false,
            state: ApplyState::default(),
            editor: true,
            me: None,
            peers: BTreeMap::new(),
            befores: Vec::new(),
            structs: Vec::new(),
            scopes: Vec::new(),
            applying: false,
            undo: Vec::new(),
            redo: Vec::new(),
            undoing: None,
            units: VecDeque::new(),
            sent_units: 0,
            confirmed: None,
            confirmed_state: ApplyState::default(),
            last_presence: None,
            presence_at: Instant::now() - PRESENCE_EVERY,
            retry_at: None,
            backoff: 0,
            stopped: None,
            snap_wanted: false,
            snap_busy: false,
            snap_seq: seq,
            reloading: false,
            tx,
            rx,
            verify: None,
            verify_errors: Vec::new(),
            repaired: 0,
            concurrent: 0,
        }
    }

    fn send_batch(&mut self, b: Option<Batch>) {
        if let Some(b) = b {
            self.sent_units = self.units.len();
            if let Some(c) = &self.conn {
                c.send(ClientMsg::Ops { base: b.base, id: b.id, ops: b.ops });
            }
        }
    }

    /// Others who can edit (for "last editor leaving").
    fn other_editors(&self) -> usize {
        self.peers.values().filter(|p| p.peer.editor).count()
    }
}

/// A random id for this app instance's batches.
fn client_id() -> String {
    format!("{:016x}", crate::model::new_sheet_uid())
}

fn rect_of(r: Range) -> CRect {
    CRect { r1: r.r1, c1: r.c1, r2: r.r2, c2: r.c2 }
}

fn initials(name: &str) -> String {
    let mut words = name.split(|c: char| c.is_whitespace() || c == '.' || c == '@' || c == '(').filter(|w| !w.is_empty());
    let first = words.next().and_then(|w| w.chars().next());
    let last = words.last().and_then(|w| w.chars().next()).filter(|c| c.is_alphanumeric());
    first.into_iter().chain(last).flat_map(|c| c.to_uppercase()).collect()
}

impl App {
    // ------------------------------------------------------------------
    // Recording local changes
    // ------------------------------------------------------------------

    /// push_undo in a live session: the snapshot becomes the "before" of the next change.
    pub(super) fn live_record(&mut self, scopes: &[Scope]) {
        self.live_flush();
        if let Some(l) = &mut self.live {
            if !l.applying {
                l.scopes = scopes.to_vec();
            }
        }
        self.live_snap_scopes(scopes);
    }

    fn live_snap_scopes(&mut self, scopes: &[Scope]) {
        let befores: Vec<Before> = scopes
            .iter()
            .filter_map(|s| match *s {
                Scope::Range(si, rg) => {
                    let sh = self.wb.sheets.get(si)?;
                    Some(Before::Range { s: sh.uid, rg, cells: sh.iter_range(rg).map(|(r, c, x)| (r, c, x.clone())).collect() })
                }
                Scope::Sheet(i) => self.wb.sheets.get(i).map(|sh| Before::Sheet(Box::new(sh.clone()))),
                Scope::All => Some(Before::All { sheets: self.wb.sheets.clone(), names: self.wb.names.clone() }),
            })
            .collect();
        if let Some(l) = &mut self.live {
            if !l.applying {
                l.befores = befores;
            }
        }
    }

    /// Rows/columns inserted or deleted by a command (they can't be told from a comparison).
    pub(super) fn live_struct(&mut self, si: usize, rows: bool, at: u32, n: i64) {
        let uid = self.wb.sheets.get(si).map(|s| s.uid);
        if let (Some(l), Some(uid)) = (&mut self.live, uid) {
            if !l.applying {
                l.structs.push((uid, rows, at, n));
            }
        }
    }

    /// A mouse drag (resizing, moving a chart) changes the workbook over several frames.
    fn pointer_down(&self) -> bool {
        self.ctx.as_ref().is_some_and(|c| c.input(|i| i.pointer.any_down()))
    }

    /// Turns the change since the last snapshot into operations and sends them.
    pub fn live_flush(&mut self) {
        let Some(l) = &mut self.live else { return };
        if l.applying || (l.befores.is_empty() && l.structs.is_empty()) {
            return;
        }
        let befores = std::mem::take(&mut l.befores);
        let structs = std::mem::take(&mut l.structs);
        let scopes = std::mem::take(&mut l.scopes);
        let ch = live::diff(befores, &structs, &mut self.wb);
        if self.pointer_down() && !scopes.is_empty() {
            // Mid-drag: what follows belongs to the same command; keep recording it.
            self.live_snap_scopes(&scopes);
            if let Some(l) = &mut self.live {
                l.scopes = scopes;
            }
        }
        let l = self.live.as_mut().expect("live");
        if let Some(v) = l.verify.take() {
            let next = self.wb.clone();
            l.verify_errors.extend(verify_change(&v, &ch, &next));
            l.verify = Some(next);
        }
        if ch.ops.is_empty() {
            return;
        }
        if !l.editor {
            // View-only: put it back (commands normally refuse before changing anything).
            l.applying = true;
            let mut st = l.state.clone();
            live::apply_ops(&mut self.wb, &ch.inverse, false, &mut st);
            let l = self.live.as_mut().expect("live");
            l.state = st;
            l.applying = false;
            if let Some(v) = &mut l.verify {
                *v = self.wb.clone();
            }
            self.wb.recalc();
            self.status = Some("You have view-only access to this workbook.".into());
            return;
        }
        // Undo is last-in first-out: by the time an older step is undone, this change is undone
        // too, so stacked steps are transformed over others' changes only.
        let entry = Undo { inverse: ch.inverse.clone(), blocked: false };
        match l.undoing {
            None => {
                l.undo.push(entry);
                l.redo.clear();
            }
            Some(false) => l.redo.push(entry),
            Some(true) => l.undo.push(entry),
        }
        if l.undo.len() > 100 {
            l.undo.remove(0);
        }
        if l.confirmed.is_none() {
            l.units.push_back(ch.inverse);
        }
        let b = l.sess.local(ch.ops);
        l.send_batch(b);
    }

    // ------------------------------------------------------------------
    // Undo / redo (per user)
    // ------------------------------------------------------------------

    pub(super) fn live_can_undo(&self, redo: bool) -> bool {
        self.live.as_ref().is_some_and(|l| if redo { !l.redo.is_empty() } else { !l.undo.is_empty() })
    }

    pub(super) fn live_can_redo(&self) -> bool {
        self.live_can_undo(true)
    }

    pub(super) fn live_undo_len(&self) -> usize {
        self.live.as_ref().map_or(0, |l| l.undo.len())
    }

    /// Merges the undo steps recorded since `start` into one.
    pub(super) fn live_squash(&mut self, start: usize) {
        self.live_flush();
        let Some(l) = &mut self.live else { return };
        if l.undo.len() <= start + 1 {
            return;
        }
        let parts = l.undo.split_off(start);
        let blocked = parts.iter().any(|p| p.blocked);
        let inverse = parts.into_iter().rev().flat_map(|p| p.inverse).collect();
        l.undo.push(Undo { inverse, blocked });
    }

    /// Undoes (or redoes) this user's last change; others' changes stay.
    pub(super) fn live_undo(&mut self, redo: bool) {
        self.live_flush();
        self.cancel_edit();
        let Some(l) = &mut self.live else { return };
        let entry = if redo { l.redo.pop() } else { l.undo.pop() };
        let Some(entry) = entry else { return };
        if entry.blocked {
            self.status = Some(format!("Can't {} that change: someone else has changed the same cells since.", if redo { "redo" } else { "undo" }));
            return;
        }
        let scopes = self.scopes_for(&entry.inverse);
        let active = self.wb.sheets.get(self.wb.active).map(|s| s.uid);
        if let Some(l) = &mut self.live {
            l.undoing = Some(redo);
        }
        self.live_record(&scopes);
        let l = self.live.as_mut().expect("live");
        l.applying = true;
        let mut st = l.state.clone();
        let applied = live::apply_ops(&mut self.wb, &entry.inverse, false, &mut st);
        let l = self.live.as_mut().expect("live");
        l.state = st;
        l.applying = false;
        for op in &applied.structs {
            if let Op::Rows { s, at, n } | Op::Cols { s, at, n } = op {
                l.structs.push((*s, matches!(op, Op::Rows { .. }), *at, *n as i64));
            }
        }
        if let Some(i) = active.and_then(|u| live::index_of(&self.wb, u)) {
            self.wb.active = i;
        }
        self.wb.active = self.wb.active.min(self.wb.sheets.len() - 1);
        self.changed();
        self.live_flush();
        if let Some(l) = &mut self.live {
            l.undoing = None;
        }
        self.after_sheets_changed(applied.sheets, &applied.features);
    }

    /// Undo scopes that cover what `ops` change.
    fn scopes_for(&self, ops: &[Op]) -> Vec<Scope> {
        let mut out = Vec::new();
        let mut cells: BTreeMap<usize, Range> = BTreeMap::new();
        for op in ops {
            if op.is_structural() || matches!(op, Op::Place { .. } | Op::Book { .. }) {
                return vec![Scope::All];
            }
            let Some(i) = op.sheet().and_then(|s| live::index_of(&self.wb, s)) else { continue };
            match op {
                Op::Cell { r, c, .. } => {
                    let rg = Range::cell(*r, *c);
                    cells.entry(i).and_modify(|x| *x = x.union(&rg)).or_insert(rg);
                }
                _ => out.push(Scope::Sheet(i)),
            }
        }
        for (i, rg) in cells {
            if !out.iter().any(|s| matches!(s, Scope::Sheet(j) if *j == i)) {
                out.push(Scope::Range(i, rg));
            }
        }
        out
    }

    // ------------------------------------------------------------------
    // Session lifecycle
    // ------------------------------------------------------------------

    /// After a document was opened (or a new one started): leave the old session, join the new
    /// document's if it is on a FreedomOffice server that supports live co-editing.
    pub(super) fn live_reset(&mut self) {
        self.live_leave(false);
        let Some(doc) = self.cloud.doc.clone() else { return };
        let (Provider::FreedomServer(url), Some(base)) = (&doc.provider, &doc.live) else { return };
        let server = self.cloud.servers.iter().find(|e| e.session.account.url == *url).map(|e| e.session.clone());
        let Some(server) = server else { return };
        live::assign_ids(&mut self.wb, &base.sheets);
        if let Some(data) = base.state.as_ref().and_then(|p| std::fs::read(p).ok()) {
            // The lossless state of that version (the file may not keep everything exactly).
            self.wb = live::join_workbook(std::mem::take(&mut self.wb), Some(&data));
            self.wb.recalc();
        }
        live::sort_sheets(&mut self.wb);
        let mut l = Live::new(doc.item_id.clone(), url.clone(), Some(server), base.seq);
        l.sess.online = false;
        self.live = Some(l);
        self.cloud.autosave = true;
        self.live_connect(Some(base.version));
        self.status = Some(format!("Connecting to {} for live editing...", crate::cloud::server::host_of(url)));
    }

    fn live_connect(&mut self, version: Option<i64>) {
        let ctx = self.ctx.clone();
        let Some(l) = &mut self.live else { return };
        let Some(server) = l.server.clone() else { return };
        let t = conn::Target { url: l.url.clone(), token: server.token().to_string(), item: l.item.clone(), since: l.sess.seq, version, cid: l.sess.cid.clone() };
        l.conn = Some(conn::connect(t, ctx));
        l.catch_up = None;
        l.retry_at = None;
    }

    /// Leaves the session. The last editor uploads a snapshot so the server's version holds
    /// everything (`wait`: block until it is stored, for closing the app).
    pub fn live_leave(&mut self, wait: bool) {
        self.live_flush();
        let Some(l) = self.live.take() else { return };
        let unsaved = l.sess.seq > l.snap_seq;
        if l.editor && l.online && !l.sess.has_pending() && unsaved && l.other_editors() == 0 {
            if let (Some(server), Some(doc)) = (l.server.clone(), self.cloud.doc.clone()) {
                let wb = self.wb.clone();
                let (item, seq) = (l.item.clone(), l.sess.seq);
                let h = std::thread::spawn(move || {
                    let _ = upload_snapshot(&server, &item, seq, &wb, &doc.local);
                });
                if wait {
                    let end = Instant::now() + Duration::from_secs(15);
                    while !h.is_finished() && Instant::now() < end {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
            }
        }
        drop(l);
    }

    /// Frame hook: connection events, snapshots, presence, reconnecting.
    pub fn poll_live(&mut self) {
        if self.live.is_none() {
            return;
        }
        // The document changed under us (Save As elsewhere, closed).
        let same = match (&self.live, &self.cloud.doc) {
            (Some(l), Some(d)) => d.item_id == l.item,
            _ => false,
        };
        if !same {
            self.live_leave(false);
            return;
        }
        if !self.pointer_down() {
            self.live_flush();
        }
        loop {
            let ev = self.live.as_ref().and_then(|l| l.conn.as_ref()).and_then(|c| c.rx.try_recv().ok());
            let Some(ev) = ev else { break };
            self.live_event(ev);
            if self.live.is_none() {
                return;
            }
        }
        loop {
            let job = self.live.as_ref().and_then(|l| l.rx.try_recv().ok());
            let Some(job) = job else { break };
            self.live_job(job);
        }
        let Some(l) = &mut self.live else { return };
        if l.conn.is_none() && l.stopped.is_none() && !l.reloading && l.retry_at.is_some_and(|t| Instant::now() >= t) {
            self.live_connect(None);
        }
        let Some(l) = &mut self.live else { return };
        if l.snap_wanted && !l.snap_busy && l.online && !l.sess.has_pending() && self.edit.is_none() {
            self.live_snapshot();
        }
        self.live_presence();
        if let Some(l) = &self.live {
            self.dirty = l.sess.has_pending();
            if let Some(c) = &self.ctx {
                if l.retry_at.is_some() {
                    c.request_repaint_after(Duration::from_millis(500));
                } else if !l.peers.is_empty() {
                    c.request_repaint_after(Duration::from_millis(1000));
                }
            }
        }
    }

    fn live_event(&mut self, ev: Event) {
        match ev {
            Event::Open => {
                let sheets: Vec<SheetId> = self.wb.sheets.iter().map(|s| s.uid).collect();
                if let Some(l) = &mut self.live {
                    if let Some(c) = &l.conn {
                        c.send(ClientMsg::Hello { sheets });
                    }
                    l.last_presence = None;
                }
            }
            Event::Msg(m) => self.live_msg(m),
            Event::Closed { code, http, reason } => {
                let Some(l) = &mut self.live else { return };
                l.conn = None;
                let was_online = l.online;
                l.online = false;
                l.sess.offline();
                l.peers.clear();
                let fatal = match (code, http) {
                    (_, Some(401)) => Some("Your sign-in has expired. Sign in again (File > Account) to keep editing live.".to_string()),
                    (_, Some(404)) => Some("The workbook is no longer shared with you, or it was deleted.".to_string()),
                    (_, Some(426)) | (_, Some(501)) => Some("This server doesn't support live co-editing.".to_string()),
                    (Some(cosync::close::READ_ONLY), _) => {
                        l.editor = false;
                        None
                    }
                    (Some(cosync::close::INVALID), _) => Some(format!("The server refused a change ({reason}). Reopen the workbook.")),
                    _ => None,
                };
                if let Some(msg) = fatal {
                    l.stopped = Some(msg.clone());
                    self.status = Some(msg);
                    return;
                }
                if l.reloading {
                    return;
                }
                l.backoff = (l.backoff + 1).min(5);
                l.retry_at = Some(Instant::now() + Duration::from_secs(1 << l.backoff.min(5)));
                if was_online {
                    self.status = Some("Reconnecting... You can keep editing; your changes are sent when the connection is back.".into());
                }
            }
        }
    }

    fn live_msg(&mut self, m: ServerMsg) {
        match m {
            ServerMsg::Welcome { seq, you, peers } => {
                let Some(l) = &mut self.live else { return };
                l.editor = you.editor;
                l.me = Some(you);
                l.peers = peers.into_iter().map(|p| (p.id, Seen { peer: p, moved: Instant::now() })).collect();
                l.catch_up = Some(seq);
                l.backoff = 0;
                self.live_caught_up();
            }
            ServerMsg::Ops { seq, cid, id, ops, .. } => {
                self.live_flush();
                let Some(l) = &mut self.live else { return };
                if seq != l.sess.seq + 1 {
                    // A gap: resynchronise.
                    l.conn = None;
                    l.online = false;
                    l.sess.offline();
                    l.retry_at = Some(Instant::now());
                    return;
                }
                if l.sess.has_pending() && l.confirmed.is_none() && !(cid == l.sess.cid && l.sess.inflight.as_ref().is_some_and(|b| b.id == id)) {
                    // Concurrency starts: rebuild the server's state (ours minus pending changes).
                    let mut c = self.wb.clone();
                    let mut st = l.state.clone();
                    for inv in l.units.iter().rev() {
                        live::apply_ops(&mut c, inv, false, &mut st);
                    }
                    l.confirmed = Some(c);
                    l.confirmed_state = st;
                    l.concurrent += 1;
                    l.units.clear();
                    l.sent_units = 0;
                }
                let step = l.sess.remote(seq, &cid, id, ops);
                self.live_step(step);
                self.live_caught_up();
            }
            ServerMsg::Ack { id, seq, extra } => {
                self.live_flush();
                let Some(l) = &mut self.live else { return };
                let step = l.sess.ack(id, seq, extra);
                self.live_step(step);
            }
            ServerMsg::Join { peer } => {
                let Some(l) = &mut self.live else { return };
                let name = peer.name.clone();
                let editor = peer.editor;
                l.peers.insert(peer.id, Seen { peer, moved: Instant::now() });
                self.status = Some(format!("{name} joined{}.", if editor { "" } else { " (viewing)" }));
            }
            ServerMsg::Leave { id } => {
                let Some(l) = &mut self.live else { return };
                if let Some(p) = l.peers.remove(&id) {
                    self.status = Some(format!("{} left.", p.peer.name));
                }
            }
            ServerMsg::Presence { id, p } => {
                let Some(l) = &mut self.live else { return };
                if let Some(s) = l.peers.get_mut(&id) {
                    if s.peer.presence.as_ref().is_none_or(|q| (q.s, q.r, q.c) != (p.s, p.r, p.c)) {
                        s.moved = Instant::now();
                    }
                    s.peer.presence = Some(p);
                }
            }
            ServerMsg::Snapshot => {
                if let Some(l) = &mut self.live {
                    l.snap_wanted = true;
                }
            }
            ServerMsg::Reload => self.live_reload(),
            ServerMsg::Error { message } => self.status = Some(message),
        }
    }

    /// After the replay since our base: resend what is pending.
    fn live_caught_up(&mut self) {
        let Some(l) = &mut self.live else { return };
        let Some(target) = l.catch_up else { return };
        if l.sess.seq < target {
            return;
        }
        l.catch_up = None;
        l.online = true;
        let b = l.sess.resume();
        l.send_batch(b);
        let host = crate::cloud::server::host_of(&l.url);
        let others = l.peers.len();
        let editor = l.editor;
        self.status = Some(match (editor, others) {
            (false, _) => format!("Viewing live on {host}. You have view-only access."),
            (true, 0) => format!("Editing live on {host}. Changes are saved automatically."),
            (true, 1) => format!("Editing live on {host} with 1 other person."),
            (true, n) => format!("Editing live on {host} with {n} other people."),
        });
    }

    fn live_step(&mut self, step: cosync::client::Step) {
        let Some(l) = &mut self.live else { return };
        if let Some(c) = &mut l.confirmed {
            live::apply_ops(c, &step.committed, true, &mut l.confirmed_state);
        }
        if step.own && l.confirmed.is_none() {
            // Our batch is in: forget the inverses it covered.
            for _ in 0..l.sent_units.min(l.units.len()) {
                l.units.pop_front();
            }
            l.sent_units = 0;
        }
        let send = step.send;
        if !step.apply.is_empty() {
            self.live_apply_remote(&step.apply);
        }
        let Some(l) = &mut self.live else { return };
        l.send_batch(send);
        if !l.sess.has_pending() {
            l.units.clear();
            l.sent_units = 0;
            if let Some(c) = l.confirmed.take() {
                l.state = std::mem::take(&mut l.confirmed_state);
                self.live_adopt(c);
            }
        }
    }

    /// Everything is acknowledged: the workbook must equal the server's state. When it doesn't
    /// (ranges that ended differently at an insert/delete boundary), take the server's.
    fn live_adopt(&mut self, mut c: Workbook) {
        let diffs = live::compare(&c, &self.wb);
        if diffs.is_empty() {
            return;
        }
        #[cfg(test)]
        eprintln!("adopting the server's state: {diffs:?}");
        if let Some(l) = &mut self.live {
            l.repaired += 1;
        }
        for sh in &mut c.sheets {
            if let Some(i) = live::index_of(&self.wb, sh.uid) {
                sh.view = self.wb.sheets[i].view.clone();
                sh.zoom = self.wb.sheets[i].zoom;
            }
        }
        let active = self.wb.sheets.get(self.wb.active).map(|s| s.uid);
        c.path = self.wb.path.clone();
        c.active = active.and_then(|u| live::index_of(&c, u)).unwrap_or(0);
        self.wb = c;
        self.wb.recalc();
        self.generation += 1;
        self.edits += 1;
        self.cf.invalidate();
        if let Some(l) = &mut self.live {
            if let Some(v) = &mut l.verify {
                *v = self.wb.clone();
            }
        }
        self.after_sheets_changed(true, &[]);
    }

    /// Applies others' changes (already transformed over ours) to the workbook.
    fn live_apply_remote(&mut self, ops: &[Op]) {
        let Some(l) = &mut self.live else { return };
        for e in l.undo.iter_mut().chain(l.redo.iter_mut()) {
            if cosync::touches(&e.inverse, ops) {
                e.blocked = true;
            }
            e.inverse = cosync::transform(&e.inverse, ops, true).0;
        }
        let active = self.wb.sheets.get(self.wb.active).map(|s| s.uid);
        let edit_sheet = self.edit.as_ref().and_then(|e| self.wb.sheets.get(e.sheet)).map(|s| s.uid);
        l.applying = true;
        let mut st = l.state.clone();
        let applied = live::apply_ops(&mut self.wb, ops, true, &mut st);
        let l = self.live.as_mut().expect("live");
        l.state = st;
        l.applying = false;
        if let Some(v) = &mut l.verify {
            let mut st = ApplyState::default();
            live::apply_ops(v, ops, true, &mut st);
        }
        // Keep the active sheet, the cell being edited, selections and others' cursors in place.
        if let Some(i) = active.and_then(|u| live::index_of(&self.wb, u)) {
            self.wb.active = i;
        }
        self.wb.active = self.wb.active.min(self.wb.sheets.len() - 1);
        for op in &applied.structs {
            self.live_shift_ui(op);
        }
        if let Some(e) = &mut self.edit {
            match edit_sheet.and_then(|u| live::index_of(&self.wb, u)) {
                Some(i) => e.sheet = i,
                None => {
                    self.edit = None;
                    self.status = Some("The sheet you were editing was deleted by someone else.".into());
                }
            }
        }
        if applied.full {
            self.wb.recalc();
        } else if !applied.cells.is_empty() {
            crate::formula::recalc_ranges(&mut self.wb, &applied.cells);
        }
        self.generation += 1;
        self.edits += 1;
        self.cf.invalidate();
        self.after_sheets_changed(applied.sheets, &applied.features);
    }

    /// UI state that refers to sheets or feature lists by index is reset when those change.
    fn after_sheets_changed(&mut self, sheets: bool, features: &[(SheetId, String)]) {
        let touched = |k: &str| sheets || features.iter().any(|(_, f)| f == k);
        if touched("charts") {
            self.chart = Default::default();
        }
        if touched("images") {
            self.pictures = Default::default();
        }
        if touched("tables") {
            self.tables = Default::default();
        }
        if touched("pivots") {
            self.pivots = Default::default();
        }
        if sheets {
            self.rename_tab = None;
            self.tab_drag = None;
            self.zoom_for = None;
            self.filter_popup = None;
            self.tab_scroll = self.tab_scroll.min(self.wb.sheets.len().saturating_sub(1));
        }
    }

    /// Moves views, the editor and others' cursors with rows/columns someone inserted or deleted.
    fn live_shift_ui(&mut self, op: &Op) {
        let (s, rows, at, n) = match op {
            Op::Rows { s, at, n } => (*s, true, *at, *n),
            Op::Cols { s, at, n } => (*s, false, *at, *n),
            _ => return,
        };
        let max = if rows { crate::model::MAX_ROWS } else { crate::model::MAX_COLS };
        let mv = |p: (u32, u32)| -> (u32, u32) {
            let (a, b) = if rows { (p.0, p.1) } else { (p.1, p.0) };
            let a = cosync::map_index(a, at, n, max).unwrap_or(at.min(max - 1));
            if rows { (a, b) } else { (b, a) }
        };
        let Some(i) = live::index_of(&self.wb, s) else { return };
        let v = &mut self.wb.sheets[i].view;
        v.active = mv(v.active);
        v.anchor = mv(v.anchor);
        v.sel = match cosync::fix_range(rect_of(v.sel), rows, at, n) {
            Some(r) => Range::new(r.r1, r.c1, r.r2, r.c2),
            None => Range::cell(v.active.0, v.active.1),
        };
        v.extra.clear();
        if let Some(e) = &mut self.edit {
            if e.sheet == i {
                let p = if rows { cosync::map_index(e.row, at, n, max) } else { cosync::map_index(e.col, at, n, max) };
                match p {
                    Some(p) if rows => e.row = p,
                    Some(p) => e.col = p,
                    None => {
                        self.edit = None;
                        self.status = Some("The cell you were editing was deleted by someone else.".into());
                    }
                }
            }
        }
        if let Some(l) = &mut self.live {
            for p in l.peers.values_mut() {
                if let Some(pr) = &mut p.peer.presence {
                    if pr.s == s {
                        let (r, c) = mv((pr.r, pr.c));
                        pr.r = r;
                        pr.c = c;
                        pr.sel = cosync::fix_range(pr.sel, rows, at, n).unwrap_or(CRect { r1: r, c1: c, r2: r, c2: c });
                    }
                }
            }
        }
    }

    fn live_job(&mut self, job: Job) {
        match job {
            Job::Snapshot(seq, result) => {
                let Some(l) = &mut self.live else { return };
                l.snap_busy = false;
                match result {
                    Ok(item) => {
                        l.snap_seq = l.snap_seq.max(seq);
                        if let Some(d) = &mut self.cloud.doc {
                            d.update_from(&item);
                        }
                    }
                    Err(e) => {
                        l.snap_wanted = true;
                        self.status = Some(format!("Couldn't save a version on the server: {e}"));
                    }
                }
            }
            Job::Reloaded(result) => {
                let Some(l) = &mut self.live else { return };
                l.reloading = false;
                match result {
                    Ok(doc) => self.live_reloaded(doc),
                    Err(e) => {
                        l.retry_at = Some(Instant::now() + Duration::from_secs(5));
                        self.status = Some(format!("Couldn't get the latest version: {e}"));
                    }
                }
            }
        }
    }

    /// Uploads the workbook (which holds everything up to our sequence number) as a version.
    pub fn live_snapshot(&mut self) {
        let Some(l) = &mut self.live else { return };
        let (Some(server), Some(doc)) = (l.server.clone(), self.cloud.doc.clone()) else { return };
        if l.snap_busy || l.sess.has_pending() || !l.online || !l.editor {
            l.snap_wanted = true;
            return;
        }
        l.snap_busy = true;
        l.snap_wanted = false;
        let (item, seq, tx) = (l.item.clone(), l.sess.seq, l.tx.clone());
        let wb = self.wb.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let result = upload_snapshot(&server, &item, seq, &wb, &doc.local);
            let _ = tx.send(Job::Snapshot(seq, result));
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
    }

    /// The server no longer has the changes since our base: get the latest version.
    fn live_reload(&mut self) {
        let Some(l) = &mut self.live else { return };
        let (Some(server), Some(doc)) = (l.server.clone(), self.cloud.doc.clone()) else { return };
        l.conn = None;
        l.online = false;
        l.sess.offline();
        l.reloading = true;
        let tx = l.tx.clone();
        let ctx = self.ctx.clone();
        let root = doc.local.parent().and_then(|p| p.parent()).map(|p| p.to_path_buf());
        self.status = Some("Getting the latest version of the workbook...".into());
        std::thread::spawn(move || {
            let item = DriveItem {
                kind: Kind::File,
                id: doc.item_id.clone(),
                drive_id: String::new(),
                name: doc.name.clone(),
                modified: String::new(),
                modified_by: String::new(),
                web_url: String::new(),
                etag: String::new(),
                parent_id: String::new(),
                size: 0,
            };
            let result = match root {
                Some(root) => server.open(&item, &root),
                None => Err("The local copy's folder is unknown.".into()),
            };
            let _ = tx.send(Job::Reloaded(result));
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
    }

    fn live_reloaded(&mut self, doc: CloudDoc) {
        let Some(base) = doc.live.clone() else { return };
        let mut wb = match crate::io::load(&doc.local) {
            Ok(w) => w,
            Err(e) => {
                self.status = Some(format!("Couldn't open the latest version: {e}"));
                return;
            }
        };
        if wb.sheets.is_empty() {
            wb.sheets.push(crate::model::Sheet::new("Sheet1"));
        }
        live::assign_ids(&mut wb, &base.sheets);
        if let Some(data) = base.state.as_ref().and_then(|p| std::fs::read(p).ok()) {
            wb = live::join_workbook(wb, Some(&data));
        }
        live::sort_sheets(&mut wb);
        for sh in &mut wb.sheets {
            if let Some(i) = live::index_of(&self.wb, sh.uid) {
                sh.view = self.wb.sheets[i].view.clone();
            }
        }
        let active = self.wb.sheets.get(self.wb.active).map(|s| s.uid);
        wb.path = Some(doc.local.clone());
        wb.active = active.and_then(|u| live::index_of(&wb, u)).unwrap_or(0);
        let Some(l) = &mut self.live else { return };
        // Our unacknowledged changes go on top of the latest version.
        let pending = l.sess.rebase_on_reload(base.seq);
        l.confirmed = Some(wb.clone());
        l.confirmed_state = ApplyState::default();
        l.units.clear();
        l.sent_units = 0;
        l.undo.clear();
        l.redo.clear();
        l.snap_seq = base.seq;
        l.applying = true;
        let mut st = ApplyState::default();
        live::apply_ops(&mut wb, &pending, false, &mut st);
        let l = self.live.as_mut().expect("live");
        l.state = st;
        l.applying = false;
        self.wb = wb;
        self.wb.recalc();
        self.generation += 1;
        self.cf.invalidate();
        self.after_sheets_changed(true, &[]);
        if let Some(d) = &mut self.cloud.doc {
            d.etag = doc.etag.clone();
            d.live = doc.live.clone();
        }
        self.live_connect(Some(base.version));
    }

    // ------------------------------------------------------------------
    // Presence
    // ------------------------------------------------------------------

    fn live_presence(&mut self) {
        let Some(sh) = self.wb.sheets.get(self.wb.active) else { return };
        let v = &sh.view;
        let p = Presence { s: sh.uid, r: v.active.0, c: v.active.1, sel: rect_of(v.sel), editing: self.edit.is_some() };
        let Some(l) = &mut self.live else { return };
        if !l.online || l.last_presence.as_ref() == Some(&p) || l.presence_at.elapsed() < PRESENCE_EVERY {
            return;
        }
        if let Some(c) = &l.conn {
            c.send(ClientMsg::Presence { p: p.clone() });
        }
        l.last_presence = Some(p);
        l.presence_at = Instant::now();
    }

    /// Others' selections on the active sheet: coloured borders, the active cell thicker, a
    /// tinted cell while they type, and a name flag for a few seconds after they move (or
    /// while the pointer is over their cell).
    pub(super) fn paint_presence(&self, p: &egui::Painter, lay: &super::grid::Layout, hover: Option<Pos2>) {
        let Some(l) = &self.live else { return };
        let Some(sh) = self.wb.sheets.get(self.wb.active) else { return };
        for seen in l.peers.values() {
            let Some(pr) = &seen.peer.presence else { continue };
            if pr.s != sh.uid {
                continue;
            }
            let col = peer_color(seen.peer.color);
            let sel = Range::new(pr.sel.r1, pr.sel.c1, pr.sel.r2, pr.sel.c2);
            let cell = sh.merge_at(pr.r, pr.c).unwrap_or(Range::cell(pr.r, pr.c));
            let cr = self.range_rect(lay, cell);
            if !sel.is_single() {
                let sr = self.range_rect(lay, sel);
                p.rect_filled(sr, 0.0, col.gamma_multiply(0.06));
                p.rect_stroke(sr.shrink(0.5), 0.0, Stroke::new(1.0, col), StrokeKind::Middle);
            }
            if pr.editing {
                p.rect_filled(cr, 0.0, col.gamma_multiply(0.18));
            }
            p.rect_stroke(cr.shrink(1.0), 0.0, Stroke::new(2.0, col), StrokeKind::Middle);
            let hovered = hover.is_some_and(|h| cr.contains(h));
            if hovered || seen.moved.elapsed().as_secs_f32() < FLAG_SECS || pr.editing {
                let label = if pr.editing { format!("{} (editing)", seen.peer.name) } else { seen.peer.name.clone() };
                let font = super::theme::ui_font(11.0);
                let g = p.layout_no_wrap(label, font, Color32::WHITE);
                let flag = Rect::from_min_size(Pos2::new(cr.right() - 1.0, cr.top() - g.size().y - 3.0), g.size() + Vec2::new(8.0, 3.0));
                p.rect_filled(flag, 2.0, col);
                p.galley(flag.min + Vec2::new(4.0, 1.5), g, Color32::WHITE);
            }
        }
    }

    /// Avatars of the others next to Share; click one to go to their cell.
    pub(super) fn live_avatars_ui(&mut self, ui: &mut egui::Ui) {
        let Some(l) = &self.live else { return };
        // One avatar per person (several windows of the same user count once).
        let mut people: Vec<(String, String, u8, bool, Option<Presence>)> = Vec::new();
        for s in l.peers.values() {
            let key = if s.peer.user.is_empty() { format!("#{}", s.peer.id) } else { s.peer.user.clone() };
            match people.iter_mut().find(|p| p.0 == key) {
                Some(p) => p.3 |= s.peer.editor,
                None => people.push((key, s.peer.name.clone(), s.peer.color, s.peer.editor, s.peer.presence.clone())),
            }
        }
        let status = match (&l.stopped, l.online) {
            (Some(m), _) => Some(m.clone()),
            (None, false) => Some("Reconnecting...".to_string()),
            _ => None,
        };
        let mut jump = None;
        if let Some(st) = status {
            ui.label(egui::RichText::new(if l.stopped.is_some() { "Offline" } else { "Reconnecting..." }).size(12.0).color(super::theme::text_dim())).on_hover_text(st);
        }
        let list: String = std::iter::once(format!("{} (you) — {}", l.me.as_ref().map_or("You".to_string(), |m| m.name.clone()), if l.editor { "Editing" } else { "Viewing" }))
            .chain(people.iter().map(|p| format!("{} — {}", p.1, if p.3 { "Editing" } else { "Viewing" })))
            .collect::<Vec<_>>()
            .join("\n");
        for (i, (_, name, color, _, presence)) in people.iter().enumerate().rev() {
            if i >= 5 {
                continue;
            }
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
            let col = peer_color(*color);
            ui.painter().circle_filled(rect.center(), 12.0, if resp.hovered() { col.gamma_multiply(0.8) } else { col });
            ui.painter().circle_stroke(rect.center(), 12.0, Stroke::new(1.5, super::theme::chrome()));
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, initials(name), super::theme::ui_bold(10.0), Color32::WHITE);
            if resp.on_hover_text(format!("{list}\n\nClick to go to {name}'s location.")).clicked() {
                jump = presence.clone();
            }
        }
        if people.len() > 5 {
            ui.label(egui::RichText::new(format!("+{}", people.len() - 5)).size(12.0)).on_hover_text(list);
        }
        if let Some(p) = jump {
            if let Some(i) = live::index_of(&self.wb, p.s) {
                self.switch_sheet(i);
                self.select(p.r, p.c, false);
            }
        }
    }

    /// View-only participants can't change anything (asked by every command's guard).
    pub(super) fn live_read_only(&mut self) -> bool {
        if self.live.as_ref().is_some_and(|l| !l.editor) {
            self.status = Some("You have view-only access to this workbook.".into());
            return true;
        }
        false
    }

    /// Title bar suffix for live documents.
    pub(super) fn live_title(&self) -> Option<String> {
        let l = self.live.as_ref()?;
        Some(match (&l.stopped, l.online, l.sess.has_pending()) {
            (Some(_), _, _) => " - Not connected".into(),
            (None, false, _) => " - Reconnecting...".into(),
            (None, true, true) => " - Saving...".into(),
            (None, true, false) if !l.editor => " - Read-only".into(),
            (None, true, false) => format!(" - Saved to {}", crate::cloud::server::host_of(&l.url)),
        })
    }
}

/// Saves the workbook (as the version) and its live state, and uploads both.
fn upload_snapshot(server: &ServerSession, item: &str, seq: u64, wb: &Workbook, local: &std::path::Path) -> Result<DriveItem, String> {
    crate::io::save(wb, local)?;
    let data = std::fs::read(local).map_err(|e| e.to_string())?;
    let state = live::encode_state(&live::state_ops(wb))?;
    server.live_snapshot(item, seq, &live::sheets_meta(wb), &data, &state)
}

/// Checks a change against full copies of the workbook before and after it (tests): the
/// operations reproduce the change, and their inverses undo it.
fn verify_change(before: &Workbook, ch: &live::Change, after: &Workbook) -> Vec<String> {
    let mut errors = Vec::new();
    let mut st = ApplyState::default();
    let mut fwd = before.clone();
    live::apply_ops(&mut fwd, &ch.ops, false, &mut st);
    for d in live::compare(&fwd, after) {
        errors.push(format!("forward: {d}"));
    }
    let mut back = after.clone();
    live::apply_ops(&mut back, &ch.inverse, false, &mut st);
    for d in live::compare(&back, before) {
        errors.push(format!("inverse: {d}"));
    }
    errors
}

#[cfg(test)]
impl App {
    /// A live session without a server, recording and checking every change.
    pub(crate) fn live_for_test(&mut self) {
        live::assign_ids(&mut self.wb, "");
        let mut l = Live::new("test".into(), "http://localhost".into(), None, 0);
        l.online = false;
        l.sess.online = false;
        l.verify = Some(self.wb.clone());
        self.live = Some(l);
    }

    pub(crate) fn live_errors(&mut self) -> Vec<String> {
        self.live_flush();
        self.live.as_mut().map(|l| std::mem::take(&mut l.verify_errors)).unwrap_or_default()
    }

    pub(crate) fn live_pending_ops(&self) -> Vec<Op> {
        self.live.as_ref().map(|l| l.sess.pending()).unwrap_or_default()
    }

    /// Connected, caught up and nothing unacknowledged.
    pub(crate) fn live_quiet(&self) -> bool {
        self.live.as_ref().is_some_and(|l| l.online && !l.sess.has_pending() && l.confirmed.is_none())
    }

    pub(crate) fn live_seq(&self) -> u64 {
        self.live.as_ref().map_or(0, |l| l.sess.seq)
    }

    /// The connection drops (the server doesn't know yet); reconnect at once when polled.
    pub(crate) fn live_drop_for_test(&mut self) {
        if let Some(l) = &mut self.live {
            l.conn = None;
            l.online = false;
            l.sess.offline();
            l.peers.clear();
            l.retry_at = Some(Instant::now() + Duration::from_secs(3600));
        }
    }

    pub(crate) fn live_reconnect_for_test(&mut self) {
        if let Some(l) = &mut self.live {
            l.retry_at = Some(Instant::now());
        }
    }

    pub(crate) fn live_snap_seq(&self) -> u64 {
        self.live.as_ref().map_or(0, |l| l.snap_seq)
    }

    pub(crate) fn live_editor(&self) -> bool {
        self.live.as_ref().is_some_and(|l| l.editor)
    }

    pub(crate) fn live_peer_names(&self) -> Vec<String> {
        self.live.as_ref().map(|l| l.peers.values().map(|p| p.peer.name.clone()).collect()).unwrap_or_default()
    }

    pub(crate) fn live_repaired(&self) -> (usize, usize) {
        self.live.as_ref().map_or((0, 0), |l| (l.repaired, l.concurrent))
    }
}
