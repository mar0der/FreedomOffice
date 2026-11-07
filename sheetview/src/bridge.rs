//! Web Sheets, live editing: the headless live client of one browser session.
//!
//! The browser never speaks the `cosync` protocol. The server runs one [`Bridge`] per browser
//! connection: it keeps the workbook (the app's own model and formula engine), the OT session
//! (`cosync::client::Session`, the code the app runs) and the people present, and turns
//! between two vocabularies:
//!
//! * toward the live hub (`ClientMsg` / `ServerMsg`, exactly what the app and `fsheets` send),
//! * toward the browser, small JSON messages: edits in, computed windows, presence, role
//!   changes and the save state out.
//!
//! The bridge does no I/O; the server task moves messages (see `server/src/webbridge.rs`).
//!
//! Browser to bridge (`t`): `meta`, `win {id, sheet, r1, c1, r2, c2}`, `cell {id, sheet, r, c}`,
//! `edit {sheet, r, c, text}`, `clear {sheet, r1, c1, r2, c2}`, `fmt {sheet, r1, c1, r2, c2,
//! set: {b, i, u, x, fill, color, h, nf}}`, `undo`, `redo`, `presence {sheet, r, c, editing}`.
//! Bridge to browser: `meta`, `win`, `cell`, `hello`, `peers`, `changed`, `status`, `role`,
//! `revoked`, `reload`, `rejected`, `error`.

use std::collections::BTreeMap;

use cosync::client::{Batch, Session, Step};
use cosync::{CellData, ClientMsg, Op, Peer, Presence, Rect, ServerMsg};
use freedom_sheets::live::{self, ApplyState};
use freedom_sheets::model::{cell_from_input, Cell, HAlign, Range, Style, Workbook, MAX_COLS, MAX_ROWS};
use serde_json::{json, Value as Json};

use crate::Book;

/// Cells one `clear` or `fmt` may touch.
const MAX_EDIT_CELLS: u64 = 20_000;
/// Longest text a person may enter in a cell.
const MAX_ENTRY: usize = 32_768;
/// Undo steps kept.
const MAX_UNDO: usize = 200;

/// What a step produced.
#[derive(Default)]
pub struct Out {
    /// To the live hub.
    pub server: Vec<ClientMsg>,
    /// To the browser.
    pub browser: Vec<Json>,
}

/// A snapshot to store: the workbook as it is, which holds every change up to `seq`.
pub struct SnapshotJob {
    pub wb: Workbook,
    pub seq: u64,
}

impl SnapshotJob {
    /// (the .xlsx file, the lossless live state, the sheets' ids and order keys); the slow part.
    pub fn encode(&self) -> Result<(Vec<u8>, Vec<u8>, String), String> {
        let data = freedom_sheets::io::xlsx_bytes(&self.wb)?;
        let state = live::encode_state(&live::state_ops(&self.wb))?;
        Ok((data, state, live::sheets_meta(&self.wb)))
    }
}

type Undo = (Vec<Op>, Vec<Op>);

pub struct Bridge {
    book: Book,
    sess: Session,
    st: ApplyState,
    needs_recalc: bool,
    rev: u64,
    editor: bool,
    me: Option<Peer>,
    peers: BTreeMap<u64, Peer>,
    welcomed: bool,
    caught_up: bool,
    catch_target: u64,
    undo: Vec<Undo>,
    redo: Vec<Undo>,
    snapshot_wanted: bool,
    /// The session can't go on (set with the reason shown to the person).
    pub stopped: Option<String>,
}

fn parse_hex(s: &str) -> Option<u32> {
    let h = s.strip_prefix('#')?;
    (h.len() == 6).then(|| u32::from_str_radix(h, 16).ok()).flatten()
}

impl Bridge {
    /// A bridge for a workbook as downloaded at `seq` (see `Live::download` in the agents crate).
    pub fn new(book: Book, seq: u64, cid: String) -> Bridge {
        let mut sess = Session::new(cid, seq);
        sess.offline();
        Bridge {
            book,
            sess,
            st: ApplyState::default(),
            needs_recalc: true,
            rev: 1,
            editor: false,
            me: None,
            peers: BTreeMap::new(),
            welcomed: false,
            caught_up: false,
            catch_target: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            snapshot_wanted: false,
            stopped: None,
        }
    }

    /// The first message to the hub.
    pub fn hello(&self) -> ClientMsg {
        ClientMsg::Hello { sheets: self.book.workbook().sheets.iter().map(|s| s.uid).collect(), proto: cosync::PROTO }
    }

    pub fn editor(&self) -> bool {
        self.editor
    }

    /// Caught up with the hub and nothing waiting for an acknowledgement.
    pub fn synced(&self) -> bool {
        self.caught_up && !self.sess.has_pending()
    }

    pub fn seq(&self) -> u64 {
        self.sess.seq
    }

    pub fn workbook(&self) -> &Workbook {
        self.book.workbook()
    }

    /// The server asked for a snapshot and the session can give one now.
    pub fn snapshot_job(&mut self) -> Option<SnapshotJob> {
        if !(self.snapshot_wanted && self.editor && self.synced()) {
            return None;
        }
        self.snapshot_wanted = false;
        self.ready();
        Some(SnapshotJob { wb: self.book.workbook().clone(), seq: self.sess.seq })
    }

    /// The snapshot was stored (or failed): ask again when the server does.
    pub fn snapshot_wanted(&self) -> bool {
        self.snapshot_wanted
    }

    fn ready(&mut self) {
        if self.needs_recalc {
            self.book.workbook_mut().recalc();
            self.needs_recalc = false;
        }
    }

    fn changed(&mut self, out: &mut Out, sheets: bool) {
        self.rev += 1;
        out.browser.push(json!({ "t": "changed", "rev": self.rev, "sheets": sheets }));
    }

    fn status(&self, out: &mut Out) {
        let state = if !self.caught_up {
            "offline"
        } else if self.sess.has_pending() {
            "saving"
        } else {
            "saved"
        };
        out.browser.push(json!({ "t": "status", "state": state }));
    }

    fn sheet_index(&self, v: &Json) -> Option<usize> {
        let i = v.get("sheet")?.as_u64()? as usize;
        (i < self.book.workbook().sheets.len()).then_some(i)
    }

    fn uid(&self, i: usize) -> u64 {
        self.book.workbook().sheets[i].uid
    }

    // ------------------------------------------------------------------
    // From the hub
    // ------------------------------------------------------------------

    pub fn on_server(&mut self, m: ServerMsg) -> Out {
        let mut out = Out::default();
        match m {
            ServerMsg::Welcome { seq, you, peers, .. } => {
                self.editor = you.editor;
                self.me = Some(you.clone());
                self.peers = peers.into_iter().map(|p| (p.id, p)).collect();
                self.welcomed = true;
                self.catch_target = seq;
                out.browser.push(json!({ "t": "hello", "role": if self.editor { "edit" } else { "view" }, "you": self.peer_json(&you) }));
                self.check_caught_up(&mut out);
                self.peers_out(&mut out);
            }
            ServerMsg::Ops { seq, cid, id, base, ops, .. } => {
                let step = self.sess.remote_based(seq, &cid, id, base, ops);
                self.stepped(step, &mut out);
                self.check_caught_up(&mut out);
            }
            ServerMsg::Ack { id, seq, extra } => {
                let step = self.sess.ack(id, seq, extra);
                self.stepped(step, &mut out);
            }
            ServerMsg::Join { peer } => {
                self.peers.insert(peer.id, peer);
                self.peers_out(&mut out);
            }
            ServerMsg::Leave { id } => {
                self.peers.remove(&id);
                self.peers_out(&mut out);
            }
            ServerMsg::Presence { id, p } => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.presence = Some(p);
                }
                self.peers_out(&mut out);
            }
            ServerMsg::Snapshot => self.snapshot_wanted = true,
            ServerMsg::Reload => {
                // The log no longer reaches our base: the browser reconnects from the latest version.
                out.browser.push(json!({ "t": "reload" }));
                self.stopped = Some("reload".into());
            }
            ServerMsg::Error { message } => out.browser.push(json!({ "t": "error", "message": message })),
            ServerMsg::Role { role } => {
                self.editor = role == "edit";
                if let Some(me) = &mut self.me {
                    me.editor = self.editor;
                }
                out.browser.push(json!({ "t": "role", "role": role }));
            }
            ServerMsg::Revoked { reason } => {
                out.browser.push(json!({ "t": "revoked", "reason": reason }));
                self.stopped = Some(reason);
            }
        }
        out
    }

    /// The hub connection ended.
    pub fn lost(&mut self) -> Out {
        let mut out = Out::default();
        self.caught_up = false;
        self.sess.offline();
        self.peers.clear();
        self.status(&mut out);
        self.peers_out(&mut out);
        out
    }

    fn check_caught_up(&mut self, out: &mut Out) {
        if self.welcomed && !self.caught_up && self.sess.seq >= self.catch_target {
            self.caught_up = true;
            if let Some(b) = self.sess.resume() {
                out.server.push(batch_msg(b));
            }
            self.status(out);
            self.changed(out, false);
        }
    }

    fn stepped(&mut self, step: Step, out: &mut Out) {
        if !step.apply.is_empty() {
            let sheets = self.apply(&step.apply, true);
            self.changed(out, sheets);
        }
        if let Some(b) = step.send {
            out.server.push(batch_msg(b));
        }
        self.status(out);
    }

    /// Applies operations to the workbook; true when the sheet list changed.
    fn apply(&mut self, ops: &[Op], remote: bool) -> bool {
        let a = live::apply_ops(self.book.workbook_mut(), ops, remote, &mut self.st);
        if a.full || !a.cells.is_empty() || a.sheets || !a.features.is_empty() {
            self.needs_recalc = true;
        }
        let structural = a.sheets || a.full || !a.structs.is_empty() || !a.features.is_empty();
        if structural {
            self.book.refresh();
        }
        structural
    }

    // ------------------------------------------------------------------
    // From the browser
    // ------------------------------------------------------------------

    pub fn on_browser(&mut self, v: &Json) -> Out {
        let mut out = Out::default();
        let t = v.get("t").and_then(Json::as_str).unwrap_or("");
        let id = v.get("id").cloned().unwrap_or(Json::Null);
        match t {
            "meta" => {
                if self.needs_recalc {
                    self.ready();
                }
                self.book.refresh();
                let mut m = self.book.meta();
                m["t"] = json!("meta");
                m["rev"] = json!(self.rev);
                m["role"] = json!(if self.editor { "edit" } else { "view" });
                m["cells"] = json!(self.book.cell_count());
                out.browser.push(m);
            }
            "win" => {
                self.ready();
                let g = |k: &str| v.get(k).and_then(Json::as_u64).map(|x| x.min(u64::from(u32::MAX)) as u32);
                let (Some(s), Some(r1), Some(c1), Some(r2), Some(c2)) = (v.get("sheet").and_then(Json::as_u64), g("r1"), g("c1"), g("r2"), g("c2")) else {
                    out.browser.push(json!({ "t": "error", "message": "Invalid window." }));
                    return out;
                };
                match self.book.window_at(s as usize, r1, c1, r2, c2) {
                    Ok(mut w) => {
                        w["t"] = json!("win");
                        w["id"] = id;
                        w["rev"] = json!(self.rev);
                        out.browser.push(w);
                    }
                    Err(e) => out.browser.push(json!({ "t": "error", "message": e, "id": id })),
                }
            }
            "cell" => {
                self.ready();
                let g = |k: &str| v.get(k).and_then(Json::as_u64).map(|x| x.min(u64::from(u32::MAX)) as u32);
                let (Some(s), Some(r), Some(c)) = (v.get("sheet").and_then(Json::as_u64), g("r"), g("c")) else { return out };
                if let Ok(mut c) = self.book.cell(s as usize, r, c) {
                    c["t"] = json!("cell");
                    c["id"] = id;
                    c["rev"] = json!(self.rev);
                    out.browser.push(c);
                }
            }
            "presence" => {
                if let (Some(i), Some(r), Some(c)) = (self.sheet_index(v), v.get("r").and_then(Json::as_u64), v.get("c").and_then(Json::as_u64)) {
                    let (r, c) = (r.min(u64::from(MAX_ROWS - 1)) as u32, c.min(u64::from(MAX_COLS - 1)) as u32);
                    let editing = v.get("editing").and_then(Json::as_bool).unwrap_or(false);
                    let p = Presence { s: self.uid(i), r, c, sel: Rect { r1: r, c1: c, r2: r, c2: c }, editing };
                    if let Some(me) = &mut self.me {
                        me.presence = Some(p.clone());
                    }
                    out.server.push(ClientMsg::Presence { p });
                }
            }
            "edit" | "clear" | "fmt" | "undo" | "redo" => self.change(t, v, &mut out),
            _ => out.browser.push(json!({ "t": "error", "message": "Unknown message." })),
        }
        out
    }

    fn reject(&self, out: &mut Out, msg: &str) {
        out.browser.push(json!({ "t": "rejected", "message": msg }));
    }

    fn change(&mut self, t: &str, v: &Json, out: &mut Out) {
        if !self.editor {
            return self.reject(out, "You have view-only access to this workbook.");
        }
        let made: Result<Undo, String> = match t {
            "edit" => self.make_edit(v),
            "clear" => self.make_clear(v),
            "fmt" => self.make_fmt(v),
            "undo" => match self.undo.pop() {
                Some(u) => {
                    let ops = u.0.clone();
                    self.redo.push(u);
                    self.run_local(ops, None, out);
                    return;
                }
                None => return self.reject(out, "Nothing to undo."),
            },
            _ => match self.redo.pop() {
                Some(u) => {
                    let ops = u.1.clone();
                    self.undo.push(u);
                    self.run_local(ops, None, out);
                    return;
                }
                None => return self.reject(out, "Nothing to redo."),
            },
        };
        match made {
            Ok((inverse, forward)) => {
                self.redo.clear();
                self.run_local(forward.clone(), Some((inverse, forward)), out);
            }
            Err(e) => self.reject(out, &e),
        }
    }

    /// Applies our own change, sends it, and remembers how to undo it.
    fn run_local(&mut self, ops: Vec<Op>, record: Option<Undo>, out: &mut Out) {
        if ops.is_empty() {
            return;
        }
        let sheets = self.apply(&ops, false);
        if let Some(u) = record {
            self.undo.push(u);
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
            }
        }
        if let Some(b) = self.sess.local(ops) {
            out.server.push(batch_msg(b));
        }
        self.changed(out, sheets);
        self.status(out);
    }

    fn rect(&self, v: &Json) -> Result<(usize, Range), String> {
        let i = self.sheet_index(v).ok_or("No such sheet.")?;
        let g = |k: &str| v.get(k).and_then(Json::as_u64).ok_or_else(|| "Invalid range.".to_string());
        let (r1, c1, r2, c2) = (g("r1")?, g("c1")?, g("r2")?, g("c2")?);
        if r2 >= u64::from(MAX_ROWS) || c2 >= u64::from(MAX_COLS) || r1 > r2 || c1 > c2 {
            return Err("Invalid range.".into());
        }
        if (r2 - r1 + 1) * (c2 - c1 + 1) > MAX_EDIT_CELLS {
            return Err("That is too many cells at once.".into());
        }
        Ok((i, Range::new(r1 as u32, c1 as u32, r2 as u32, c2 as u32)))
    }

    /// A cell's content as it travels (no format), None when empty.
    fn content_of(&self, i: usize, r: u32, c: u32) -> Option<Box<CellData>> {
        let wb = self.book.workbook();
        let sh = &wb.sheets[i];
        let cell = sh.get(r, c)?;
        let d = live::cell_data(wb, sh, r, c, cell);
        (!d.v.is_empty() || d.f.is_some() || d.rich.is_some()).then(|| Box::new(CellData { st: None, ..d }))
    }

    fn style_of(&self, i: usize, r: u32, c: u32) -> Style {
        let wb = self.book.workbook();
        wb.style(wb.sheets[i].get(r, c).map_or(0, |x| x.style)).clone()
    }

    fn make_edit(&mut self, v: &Json) -> Result<Undo, String> {
        let i = self.sheet_index(v).ok_or("No such sheet.")?;
        let g = |k: &str| v.get(k).and_then(Json::as_u64).filter(|x| *x < 1 << 31).map(|x| x as u32);
        let (Some(r), Some(c)) = (g("r"), g("c")) else { return Err("Invalid cell.".into()) };
        if r >= MAX_ROWS || c >= MAX_COLS {
            return Err("Invalid cell.".into());
        }
        let text = v.get("text").and_then(Json::as_str).ok_or("Nothing to enter.")?;
        if text.len() > MAX_ENTRY {
            return Err("That text is too long for a cell.".into());
        }
        if let Some(f) = text.strip_prefix('=').filter(|f| !f.trim().is_empty())
            && let Err(e) = freedom_sheets::formula::parse(f)
        {
            return Err(format!("That formula can't be read ({e})."));
        }
        let uid = self.uid(i);
        let old_content = self.content_of(i, r, c);
        let old_style = self.style_of(i, r, c);
        let base_style = self.book.workbook().sheets[i].get(r, c).map_or(0, |x| x.style);
        let new: Option<Cell> = cell_from_input(self.book.workbook_mut(), text, base_style);
        let wb = self.book.workbook();
        let (new_content, new_style) = match &new {
            Some(cell) => {
                let d = live::cell_data(wb, &wb.sheets[i], r, c, cell);
                let content = (!d.v.is_empty() || d.f.is_some()).then(|| Box::new(CellData { st: None, rich: None, ..d }));
                (content, wb.style(cell.style).clone())
            }
            None => (None, old_style.clone()),
        };
        let mut fwd = vec![Op::Content { s: uid, r, c, v: new_content, adj: vec![] }];
        let mut inv = vec![Op::Content { s: uid, r, c, v: old_content, adj: vec![] }];
        if new_style != old_style {
            fwd.push(Op::Format { s: uid, r, c, st: live::style_json(&new_style) });
            inv.push(Op::Format { s: uid, r, c, st: live::style_json(&old_style) });
        }
        Ok((inv, fwd))
    }

    fn make_clear(&mut self, v: &Json) -> Result<Undo, String> {
        let (i, rg) = self.rect(v)?;
        let uid = self.uid(i);
        let mut fwd = Vec::new();
        let mut inv = Vec::new();
        let cells: Vec<(u32, u32)> = self.book.workbook().sheets[i].iter_range(rg).map(|(r, c, _)| (r, c)).collect();
        for (r, c) in cells {
            if let Some(old) = self.content_of(i, r, c) {
                fwd.push(Op::Content { s: uid, r, c, v: None, adj: vec![] });
                inv.push(Op::Content { s: uid, r, c, v: Some(old), adj: vec![] });
            }
        }
        if fwd.is_empty() {
            return Err("Nothing to clear.".into());
        }
        Ok((inv, fwd))
    }

    fn make_fmt(&mut self, v: &Json) -> Result<Undo, String> {
        let (i, rg) = self.rect(v)?;
        let uid = self.uid(i);
        let set = v.get("set").and_then(Json::as_object).ok_or("Nothing to change.")?;
        let mut fwd = Vec::new();
        let mut inv = Vec::new();
        for r in rg.r1..=rg.r2 {
            for c in rg.c1..=rg.c2 {
                let old = self.style_of(i, r, c);
                let mut st = old.clone();
                for (k, val) in set {
                    match k.as_str() {
                        "b" => st.bold = val.as_bool().ok_or("Invalid format.")?,
                        "i" => st.italic = val.as_bool().ok_or("Invalid format.")?,
                        "u" => st.underline = val.as_bool().ok_or("Invalid format.")?,
                        "x" => st.strike = val.as_bool().ok_or("Invalid format.")?,
                        "fill" => st.fill = if val.is_null() { None } else { Some(parse_hex(val.as_str().unwrap_or("")).ok_or("Invalid colour.")?) },
                        "color" => st.color = if val.is_null() { None } else { Some(parse_hex(val.as_str().unwrap_or("")).ok_or("Invalid colour.")?) },
                        "h" => {
                            st.halign = match val.as_str() {
                                Some("l") => HAlign::Left,
                                Some("c") => HAlign::Center,
                                Some("r") => HAlign::Right,
                                Some("g") => HAlign::General,
                                _ => return Err("Invalid alignment.".into()),
                            }
                        }
                        "nf" => {
                            let f = val.as_str().ok_or("Invalid number format.")?;
                            if f.len() > 100 {
                                return Err("That number format is too long.".into());
                            }
                            st.num_fmt = f.to_string();
                        }
                        _ => return Err("Unknown format.".into()),
                    }
                }
                if st != old {
                    fwd.push(Op::Format { s: uid, r, c, st: live::style_json(&st) });
                    inv.push(Op::Format { s: uid, r, c, st: live::style_json(&old) });
                }
            }
        }
        if fwd.is_empty() {
            return Err("Nothing to change.".into());
        }
        Ok((inv, fwd))
    }

    // ------------------------------------------------------------------
    // People
    // ------------------------------------------------------------------

    fn peer_json(&self, p: &Peer) -> Json {
        let wb = self.book.workbook();
        let pres = p.presence.as_ref().map(|x| {
            let sheet = live::index_of(wb, x.s).map_or(-1, |i| i as i64);
            json!({ "sheet": sheet, "r": x.r, "c": x.c, "sel": [x.sel.r1, x.sel.c1, x.sel.r2, x.sel.c2], "editing": x.editing })
        });
        json!({ "id": p.id, "name": p.name, "user": p.user, "color": p.color, "editor": p.editor, "agent": p.agent, "p": pres })
    }

    fn peers_out(&self, out: &mut Out) {
        let list: Vec<Json> = self.peers.values().map(|p| self.peer_json(p)).collect();
        out.browser.push(json!({ "t": "peers", "peers": list }));
    }
}

fn batch_msg(b: Batch) -> ClientMsg {
    ClientMsg::Ops { base: b.base, id: b.id, made: (b.made < b.base).then_some(b.made), ops: b.ops }
}

/// Used by tests: the text a cell shows.
pub fn shown(b: &Bridge, sheet: usize, r: u32, c: u32) -> String {
    let wb = b.workbook();
    let sh = &wb.sheets[sheet];
    let st = wb.style(sh.get(r, c).map_or(0, |x| x.style));
    freedom_sheets::numfmt::format_value(sh.display_value(r, c), &st.num_fmt).text
}
