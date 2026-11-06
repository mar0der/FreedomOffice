//! Live co-editing for FreedomOffice: the operations a workbook change is sent as, the
//! operational transformation (OT) that lets concurrent changes converge, and the wire
//! protocol between FreedomOffice Sheets and the document server.
//!
//! Both the server and the app compile this one crate, so they can never disagree on how two
//! concurrent changes combine. Nothing here knows about spreadsheets beyond rows, columns,
//! cells and sheets: cell styles and per-sheet features travel as opaque JSON.
//!
//! The model is classic client/server OT (Jupiter / Google Wave): the server keeps the
//! authoritative log and numbers every committed batch; a client sends one batch at a time
//! against the last sequence number it has seen, and the server transforms it over everything
//! committed since. Concurrent writes to the same target keep the one committed last.

pub mod client;
pub mod server;
mod transform;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

pub use transform::{fix_range, map_index, over, touches, transform};

pub const MAX_ROWS: u32 = 1_048_576;
pub const MAX_COLS: u32 = 16_384;
/// Longest sheet name Excel allows.
pub const MAX_SHEET_NAME: usize = 31;
/// The sheet a workbook gets when concurrent deletes removed every sheet: the server appends
/// `AddSheet` of this id to the batch that deleted the last one. A client that runs out of
/// sheets before that shows a provisional sheet with this id.
pub const RESCUE_SHEET: SheetId = 0x7E5C_0E5C_0E5C_0E5C;

/// Stable identity of a worksheet (survives renames and moves).
pub type SheetId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Rect {
    pub r1: u32,
    pub c1: u32,
    pub r2: u32,
    pub c2: u32,
}

impl Rect {
    pub fn is_single(&self) -> bool {
        self.r1 == self.r2 && self.c1 == self.c2
    }
}

/// A constant cell value.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "k", content = "x")]
pub enum Val {
    #[default]
    Empty,
    Num(f64),
    Text(String),
    Bool(bool),
    /// Error literal such as `#N/A`.
    Err(String),
}

impl Val {
    pub fn is_empty(&self) -> bool {
        matches!(self, Val::Empty)
    }
}

/// What a cell holds. Formula cells carry no value: every participant calculates.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct CellData {
    #[serde(default, skip_serializing_if = "Val::is_empty")]
    pub v: Val,
    /// Formula text without the leading '='.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub f: Option<String>,
    /// Style content (never a workbook-local style index); None = the default style.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub st: Option<Json>,
    /// Rich-text runs of a text cell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rich: Option<Json>,
}

/// One change to a workbook.
///
/// Structural operations (`Rows`, `Cols`, sheet add/delete/rename/order) move other changes
/// around when transformed; content operations are last-writer-wins per target. Content that
/// embeds references (formulas, per-sheet feature lists, defined names) records in `adj` the
/// structural operations it was transformed over; the receiving app adjusts the content by
/// them, exactly as if the content had existed when those operations were applied.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Insert (n > 0) or delete (n < 0) rows starting at `at`.
    Rows { s: SheetId, at: u32, n: i32 },
    /// Insert (n > 0) or delete (n < 0) columns starting at `at`.
    Cols { s: SheetId, at: u32, n: i32 },
    /// A new, empty sheet (content follows as other ops). Tabs are ordered by (`pos`, id),
    /// which is the same whatever order concurrent changes arrive in.
    AddSheet { s: SheetId, name: String, pos: f64 },
    DelSheet { s: SheetId, name: String },
    Rename { s: SheetId, from: String, to: String },
    /// Moves a sheet tab: its new order key.
    Place { s: SheetId, pos: f64 },
    /// Sets a cell (None clears it).
    Cell {
        s: SheetId,
        r: u32,
        c: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        v: Option<Box<CellData>>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        adj: Vec<Op>,
    },
    Note { s: SheetId, r: u32, c: u32, text: Option<String>, author: Option<String> },
    Link { s: SheetId, r: u32, c: u32, url: Option<String> },
    /// Row height in pixels; None = default.
    RowH { s: SheetId, r: u32, h: Option<f32> },
    ColW { s: SheetId, c: u32, w: Option<f32> },
    RowHidden { s: SheetId, r: u32, on: bool },
    ColHidden { s: SheetId, c: u32, on: bool },
    /// Outline level (0 = none).
    RowLevel { s: SheetId, r: u32, l: u8 },
    ColLevel { s: SheetId, c: u32, l: u8 },
    Merge { s: SheetId, rg: Rect, on: bool },
    /// A sheet property: "tab_color", "hidden", "freeze", "gridlines", "protection", ...
    Prop { s: SheetId, key: String, val: Json },
    /// Replaces one per-sheet feature list ("charts", "cond_formats", "tables", ...).
    Feature {
        s: SheetId,
        key: String,
        val: Json,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        adj: Vec<Op>,
    },
    /// Replaces a workbook-level list ("names", ...).
    Book {
        key: String,
        val: Json,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        adj: Vec<Op>,
    },
}

impl Op {
    /// The sheet a sheet-bound operation applies to.
    pub fn sheet(&self) -> Option<SheetId> {
        use Op::*;
        match self {
            Rows { s, .. }
            | Cols { s, .. }
            | AddSheet { s, .. }
            | DelSheet { s, .. }
            | Rename { s, .. }
            | Cell { s, .. }
            | Note { s, .. }
            | Link { s, .. }
            | RowH { s, .. }
            | ColW { s, .. }
            | RowHidden { s, .. }
            | ColHidden { s, .. }
            | RowLevel { s, .. }
            | ColLevel { s, .. }
            | Merge { s, .. }
            | Prop { s, .. }
            | Feature { s, .. }
            | Place { s, .. } => Some(*s),
            Book { .. } => None,
        }
    }

    /// Row/column insert or delete, or a sheet add/delete/rename.
    pub fn is_structural(&self) -> bool {
        matches!(self, Op::Rows { .. } | Op::Cols { .. } | Op::AddSheet { .. } | Op::DelSheet { .. } | Op::Rename { .. })
    }
}

/// Where a participant is: active cell and selection on a sheet.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Presence {
    pub s: SheetId,
    pub r: u32,
    pub c: u32,
    pub sel: Rect,
    /// Typing in the active cell.
    #[serde(default)]
    pub editing: bool,
}

/// A participant of a live session (one per connection).
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Peer {
    /// Connection id, unique within the document's session.
    pub id: u64,
    /// Display name.
    pub name: String,
    /// Username, or "" for someone with a link.
    pub user: String,
    /// Colour slot (the app maps it to a colour).
    pub color: u8,
    pub editor: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<Presence>,
}

/// Messages from the app to the server (WebSocket text frames, JSON).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    /// First message after connecting: the sheet ids of the workbook as loaded.
    Hello { sheets: Vec<SheetId> },
    /// A batch of changes made against the state at `base`.
    Ops { base: u64, id: u64, ops: Vec<Op> },
    Presence { p: Presence },
}

/// Messages from the server to the app.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    /// First message: the server's current sequence number (ops after the client's `since`
    /// follow until the client reaches it), who you are and who else is here.
    Welcome { seq: u64, you: Peer, peers: Vec<Peer> },
    /// A committed batch. `cid`/`id` identify the sender's client and batch (so a client
    /// recognises its own batch when it is replayed after a reconnect).
    Ops { seq: u64, cid: String, id: u64, by: u64, ops: Vec<Op> },
    /// Your batch `id` was committed as `seq` (the committed form is the batch transformed
    /// over what you already received). `extra`: ops the server inserted into it (at these
    /// indices of the committed form), to apply too.
    Ack {
        id: u64,
        seq: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extra: Vec<(u32, Op)>,
    },
    Join { peer: Peer },
    Leave { id: u64 },
    Presence { id: u64, p: Presence },
    /// Please upload a snapshot of the workbook (when you have no unacknowledged changes).
    Snapshot,
    /// The changes since your `since` are no longer kept: download the latest version.
    Reload,
    Error { message: String },
}

/// WebSocket close codes the server uses.
pub mod close {
    /// Malformed or out-of-bounds operations.
    pub const INVALID: u16 = 4400;
    /// A viewer tried to change the workbook.
    pub const READ_ONLY: u16 = 4403;
    /// Too many messages.
    pub const RATE: u16 = 4429;
    /// No heartbeat.
    pub const IDLE: u16 = 4408;
}

/// Limits the server enforces on what clients send.
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_OPS_PER_BATCH: usize = 1_000_000;
const MAX_TEXT: usize = 1 << 20;
const MAX_ADJ: usize = 10_000;

/// Checks a batch from a client: bounds, sizes and shape. Never echoes contents.
pub fn validate(ops: &[Op]) -> Result<(), String> {
    if ops.len() > MAX_OPS_PER_BATCH {
        return Err("too many operations in one batch".into());
    }
    for op in ops {
        validate_op(op, false)?;
    }
    Ok(())
}

fn validate_op(op: &Op, in_adj: bool) -> Result<(), String> {
    use Op::*;
    let row = |r: u32| if r < MAX_ROWS { Ok(()) } else { Err("row out of range".to_string()) };
    let col = |c: u32| if c < MAX_COLS { Ok(()) } else { Err("column out of range".to_string()) };
    let name = |n: &str| if !n.trim().is_empty() && n.chars().count() <= MAX_SHEET_NAME { Ok(()) } else { Err("invalid sheet name".to_string()) };
    let text = |t: &Option<String>| if t.as_ref().is_none_or(|t| t.len() <= MAX_TEXT) { Ok(()) } else { Err("text too long".to_string()) };
    let adj_ok = |adj: &[Op]| -> Result<(), String> {
        if adj.len() > MAX_ADJ {
            return Err("too many adjustments".into());
        }
        adj.iter().try_for_each(|a| validate_op(a, true))
    };
    if in_adj && !op.is_structural() {
        return Err("invalid adjustment".into());
    }
    match op {
        Rows { at, n, .. } => {
            row(*at)?;
            if *n == 0 || n.unsigned_abs() > MAX_ROWS {
                return Err("invalid row count".into());
            }
        }
        Cols { at, n, .. } => {
            col(*at)?;
            if *n == 0 || n.unsigned_abs() > MAX_COLS {
                return Err("invalid column count".into());
            }
        }
        AddSheet { name: n, pos, .. } => {
            name(n)?;
            if !pos.is_finite() {
                return Err("invalid sheet position".into());
            }
        }
        DelSheet { name: n, .. } => name(n)?,
        Rename { from, to, .. } => {
            name(from)?;
            name(to)?;
        }
        Place { pos, .. } => {
            if !pos.is_finite() {
                return Err("invalid sheet position".into());
            }
        }
        Cell { r, c, v, adj, .. } => {
            row(*r)?;
            col(*c)?;
            if let Some(d) = v {
                if d.f.as_ref().is_some_and(|f| f.len() > 64 * 1024) {
                    return Err("formula too long".into());
                }
                if let Val::Text(t) | Val::Err(t) = &d.v {
                    if t.len() > MAX_TEXT {
                        return Err("text too long".into());
                    }
                }
            }
            adj_ok(adj)?;
        }
        Note { r, c, text: t, author, .. } => {
            row(*r)?;
            col(*c)?;
            text(t)?;
            text(author)?;
        }
        Link { r, c, url, .. } => {
            row(*r)?;
            col(*c)?;
            text(url)?;
        }
        RowH { r, h, .. } => {
            row(*r)?;
            if h.is_some_and(|h| !h.is_finite() || !(0.0..=10_000.0).contains(&h)) {
                return Err("invalid row height".into());
            }
        }
        ColW { c, w, .. } => {
            col(*c)?;
            if w.is_some_and(|w| !w.is_finite() || !(0.0..=100_000.0).contains(&w)) {
                return Err("invalid column width".into());
            }
        }
        RowHidden { r, .. } | RowLevel { r, .. } => row(*r)?,
        ColHidden { c, .. } | ColLevel { c, .. } => col(*c)?,
        Merge { rg, .. } => {
            row(rg.r2)?;
            col(rg.c2)?;
            if rg.r1 > rg.r2 || rg.c1 > rg.c2 {
                return Err("invalid range".into());
            }
        }
        Prop { key, .. } => {
            if key.len() > 64 {
                return Err("invalid property".into());
            }
        }
        Feature { key, adj, .. } | Book { key, adj, .. } => {
            if key.len() > 64 {
                return Err("invalid feature".into());
            }
            adj_ok(adj)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
