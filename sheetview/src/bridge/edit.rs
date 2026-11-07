//! Web Sheets, live editing: the structural, clipboard, fill and find messages of the bridge.
//!
//! Every message that changes the workbook becomes ordinary `cosync` operations (`Rows`,
//! `Cols`, `AddSheet`, `Rename`, `Place`, `ColW`, `RowH`, `RowHidden`, `ColHidden`, `Merge`,
//! `Prop`, `Content`, `Format`), so they merge with what desktop users do at the same time by
//! the rules of the protocol, and each comes with the operations that undo it.
//!
//! Browser to bridge (`t`; `sheet` is the sheet's index, or `suid` its live id as a string):
//! `ins`/`del {axis: "r"|"c", at, n}`, `sheet_add {name?}`, `sheet_rename {name}`, `sheet_del`,
//! `sheet_move {to}`, `sheet_hide {on}`, `sheet_color {color}`, `size {axis, a, b, px}`,
//! `hide {axis, a, b, on}`, `freeze {r, c}`, `merge {r1..c2, on}`, `copy {r1..c2, cut}`,
//! `paste {r, c, r2?, c2?, clip?, grid?, what?}`, `fill {r1..c2, dir}`, `autofill {r1..c2,
//! r2n, c2n}`, `find {q, case, whole, all, formulas}`.

use std::time::{Duration, Instant};

use cosync::{CellData, Op, Rect, Val};
use freedom_sheets::formula;
use freedom_sheets::live;
use freedom_sheets::model::{new_sheet_uid, BorderLine, Range, Style, MAX_COLS, MAX_ROWS};
use serde_json::{json, Value as Json};

use super::{Bridge, Out, Undo, MAX_EDIT_CELLS, MAX_ENTRY};

/// Rows or columns one `ins`/`del` may touch.
const MAX_SPAN: u64 = 1000;
/// Cells restored by undoing a delete (beyond this, undo only puts the rows back).
const MAX_RESTORE: usize = 200_000;
/// Sheets a workbook may hold.
const MAX_SHEETS: usize = 255;
/// Largest text a paste may bring in, in bytes.
const MAX_PASTE_BYTES: usize = 4 * 1024 * 1024;
/// Hits a search reports.
const MAX_HITS: usize = 500;
/// Cells a search looks at.
const MAX_SCAN: usize = 600_000;

/// A copied area: the cells (content, format, value) as they were when copied.
pub struct Clip {
    pub id: u64,
    pub sheet: u64,
    pub r1: u32,
    pub c1: u32,
    pub h: u32,
    pub w: u32,
    pub cut: bool,
    pub cells: Vec<Vec<(Option<Box<CellData>>, Style, Val)>>,
}

fn num(v: &Json, k: &str) -> Option<u64> {
    v.get(k).and_then(Json::as_u64)
}

fn rows_axis(v: &Json) -> Result<bool, String> {
    match v.get("axis").and_then(Json::as_str) {
        Some("r") => Ok(true),
        Some("c") => Ok(false),
        _ => Err("Invalid request.".into()),
    }
}

impl Bridge {
    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// What a cell holds and how it looks now.
    fn pair_of(&self, i: usize, r: u32, c: u32) -> (Option<Box<CellData>>, Style) {
        (self.content_of(i, r, c), self.style_of(i, r, c))
    }

    /// Writes new content and/or style for a cell, recording what undoes it. Only differences
    /// produce operations.
    pub(super) fn put(&self, i: usize, r: u32, c: u32, content: Option<Option<Box<CellData>>>, style: Option<&Style>, fwd: &mut Vec<Op>, inv: &mut Vec<Op>) {
        let uid = self.uid(i);
        if let Some(new) = content {
            let old = self.content_of(i, r, c);
            if old != new {
                fwd.push(Op::Content { s: uid, r, c, v: new, adj: vec![] });
                inv.push(Op::Content { s: uid, r, c, v: old, adj: vec![] });
            }
        }
        if let Some(st) = style {
            let old = self.style_of(i, r, c);
            if old != *st {
                fwd.push(Op::Format { s: uid, r, c, st: live::style_json(st) });
                inv.push(Op::Format { s: uid, r, c, st: live::style_json(&old) });
            }
        }
    }

    fn span(v: &Json, k1: &str, k2: &str, max: u64) -> Result<(u32, u32), String> {
        let (Some(a), Some(b)) = (num(v, k1), num(v, k2)) else { return Err("Invalid range.".into()) };
        if a > b || b >= max || b - a >= 20_000 {
            return Err("Invalid range.".into());
        }
        Ok((a as u32, b as u32))
    }

    fn check_sheet_name(&self, name: &str, except: Option<usize>) -> Result<String, String> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err("A sheet needs a name.".into());
        }
        if name.chars().count() > cosync::MAX_SHEET_NAME {
            return Err("Sheet names have at most 31 characters.".into());
        }
        if name.chars().any(|c| matches!(c, '[' | ']' | ':' | '*' | '?' | '/' | '\\') || c.is_control()) {
            return Err("A sheet name can't contain [ ] : * ? / \\".into());
        }
        if name.starts_with('\'') || name.ends_with('\'') {
            return Err("A sheet name can't start or end with an apostrophe.".into());
        }
        let wb = self.book.workbook();
        if wb.sheets.iter().enumerate().any(|(k, s)| Some(k) != except && s.name.eq_ignore_ascii_case(&name)) {
            return Err("There is already a sheet with that name.".into());
        }
        Ok(name)
    }

    fn visible_sheets(&self) -> usize {
        self.book.workbook().sheets.iter().filter(|s| !s.hidden).count()
    }

    // ------------------------------------------------------------------
    // Rows and columns
    // ------------------------------------------------------------------

    pub(super) fn make_insert(&mut self, v: &Json, delete: bool) -> Result<Undo, String> {
        let i = self.sheet_index(v).ok_or("No such sheet.")?;
        let rows = rows_axis(v)?;
        let max = u64::from(if rows { MAX_ROWS } else { MAX_COLS });
        let (Some(at), Some(n)) = (num(v, "at"), num(v, "n")) else { return Err("Invalid request.".into()) };
        if n == 0 || n > MAX_SPAN || at >= max {
            return Err(format!("You can change at most {MAX_SPAN} rows or columns at once."));
        }
        let (at, n) = (at as u32, n as u32);
        let uid = self.uid(i);
        let mk = |k: i32| if rows { Op::Rows { s: uid, at, n: k } } else { Op::Cols { s: uid, at, n: k } };
        if !delete {
            return Ok((vec![mk(-(n as i32))], vec![mk(n as i32)]));
        }
        if u64::from(at) + u64::from(n) > max {
            return Err("Invalid range.".into());
        }
        // Undo puts the rows back and the cells, formats and sizes that were in them.
        let mut inv = vec![mk(n as i32)];
        let end = at + n - 1;
        let sh = &self.book.workbook().sheets[i];
        let mut cells: Vec<(u32, u32)> = Vec::new();
        if rows {
            for (r, row) in sh.rows.range(at..=end) {
                cells.extend(row.keys().map(|c| (*r, *c)));
            }
        } else {
            for (r, row) in &sh.rows {
                cells.extend(row.range(at..=end).map(|(c, _)| (*r, *c)));
            }
        }
        if cells.len() <= MAX_RESTORE {
            for (r, c) in cells {
                let (content, st) = self.pair_of(i, r, c);
                if content.is_some() {
                    inv.push(Op::Content { s: uid, r, c, v: content, adj: vec![] });
                }
                if let Some(j) = live::style_json(&st) {
                    inv.push(Op::Format { s: uid, r, c, st: Some(j) });
                }
            }
        }
        for k in at..=end {
            if rows {
                if let Some(h) = sh.row_h.get(&k) {
                    inv.push(Op::RowH { s: uid, r: k, h: Some(*h) });
                }
                if sh.hidden_rows.contains(&k) {
                    inv.push(Op::RowHidden { s: uid, r: k, on: true });
                }
            } else {
                if let Some(w) = sh.col_w.get(&k) {
                    inv.push(Op::ColW { s: uid, c: k, w: Some(*w) });
                }
                if sh.hidden_cols.contains(&k) {
                    inv.push(Op::ColHidden { s: uid, c: k, on: true });
                }
            }
        }
        Ok((inv, vec![mk(-(n as i32))]))
    }

    /// Width or height of rows/columns `a..=b` (`px` null = back to the default).
    pub(super) fn make_size(&mut self, v: &Json) -> Result<Undo, String> {
        let i = self.sheet_index(v).ok_or("No such sheet.")?;
        let rows = rows_axis(v)?;
        let (a, b) = Self::span(v, "a", "b", u64::from(if rows { MAX_ROWS } else { MAX_COLS }))?;
        let px: Option<f32> = match v.get("px") {
            None | Some(Json::Null) => None,
            Some(p) => {
                let p = p.as_f64().filter(|p| p.is_finite()).ok_or("Invalid size.")?;
                Some(p.clamp(if rows { 4.0 } else { 8.0 }, if rows { 600.0 } else { 800.0 }) as f32)
            }
        };
        let uid = self.uid(i);
        let sh = &self.book.workbook().sheets[i];
        let (mut fwd, mut inv) = (Vec::new(), Vec::new());
        for k in a..=b {
            if rows {
                let old = sh.row_h.get(&k).copied();
                if old != px {
                    fwd.push(Op::RowH { s: uid, r: k, h: px });
                    inv.push(Op::RowH { s: uid, r: k, h: old });
                }
            } else {
                let old = sh.col_w.get(&k).copied();
                if old != px {
                    fwd.push(Op::ColW { s: uid, c: k, w: px });
                    inv.push(Op::ColW { s: uid, c: k, w: old });
                }
            }
        }
        if fwd.is_empty() {
            return Err("Nothing to change.".into());
        }
        Ok((inv, fwd))
    }

    pub(super) fn make_hide(&mut self, v: &Json) -> Result<Undo, String> {
        let i = self.sheet_index(v).ok_or("No such sheet.")?;
        let rows = rows_axis(v)?;
        let (a, b) = Self::span(v, "a", "b", u64::from(if rows { MAX_ROWS } else { MAX_COLS }))?;
        let on = v.get("on").and_then(Json::as_bool).ok_or("Invalid request.")?;
        let uid = self.uid(i);
        let sh = &self.book.workbook().sheets[i];
        let (mut fwd, mut inv) = (Vec::new(), Vec::new());
        for k in a..=b {
            let was = if rows { sh.hidden_rows.contains(&k) } else { sh.hidden_cols.contains(&k) };
            if was != on {
                if rows {
                    fwd.push(Op::RowHidden { s: uid, r: k, on });
                    inv.push(Op::RowHidden { s: uid, r: k, on: was });
                } else {
                    fwd.push(Op::ColHidden { s: uid, c: k, on });
                    inv.push(Op::ColHidden { s: uid, c: k, on: was });
                }
            }
        }
        if fwd.is_empty() {
            return Err("Nothing to change.".into());
        }
        Ok((inv, fwd))
    }

    pub(super) fn make_freeze(&mut self, v: &Json) -> Result<Undo, String> {
        let i = self.sheet_index(v).ok_or("No such sheet.")?;
        let (Some(r), Some(c)) = (num(v, "r"), num(v, "c")) else { return Err("Invalid request.".into()) };
        if r > 200 || c > 50 {
            return Err("You can freeze at most 200 rows and 50 columns.".into());
        }
        let uid = self.uid(i);
        let old = self.book.workbook().sheets[i].freeze;
        if old == (r as u32, c as u32) {
            return Err("Nothing to change.".into());
        }
        let prop = |f: (u32, u32)| Op::Prop { s: uid, key: "freeze".into(), val: json!([f.0, f.1]) };
        Ok((vec![prop(old)], vec![prop((r as u32, c as u32))]))
    }

    pub(super) fn make_merge(&mut self, v: &Json) -> Result<Undo, String> {
        let (i, rg) = self.rect(v)?;
        let on = v.get("on").and_then(Json::as_bool).ok_or("Invalid request.")?;
        let uid = self.uid(i);
        let r = Rect { r1: rg.r1, c1: rg.c1, r2: rg.r2, c2: rg.c2 };
        let sh = &self.book.workbook().sheets[i];
        let (mut fwd, mut inv) = (Vec::new(), Vec::new());
        if on {
            if rg.r1 == rg.r2 && rg.c1 == rg.c2 {
                return Err("Select more than one cell to merge.".into());
            }
            // Merged areas can't overlap: whatever lies under the new one is merged away first.
            for m in sh.merges.iter().filter(|m| m.intersects(&rg)) {
                let mr = Rect { r1: m.r1, c1: m.c1, r2: m.r2, c2: m.c2 };
                fwd.push(Op::Merge { s: uid, rg: mr, on: false });
                inv.push(Op::Merge { s: uid, rg: mr, on: true });
            }
            // Only the upper-left value is kept, as in Excel.
            let cells: Vec<(u32, u32)> = sh.iter_range(rg).map(|(r, c, _)| (r, c)).filter(|&(r, c)| (r, c) != (rg.r1, rg.c1)).collect();
            for (r, c) in cells {
                if let Some(old) = self.content_of(i, r, c) {
                    fwd.push(Op::Content { s: uid, r, c, v: None, adj: vec![] });
                    inv.push(Op::Content { s: uid, r, c, v: Some(old), adj: vec![] });
                }
            }
            fwd.push(Op::Merge { s: uid, rg: r, on: true });
            inv.push(Op::Merge { s: uid, rg: r, on: false });
        } else {
            for m in sh.merges.iter().filter(|m| m.intersects(&rg)) {
                let mr = Rect { r1: m.r1, c1: m.c1, r2: m.r2, c2: m.c2 };
                fwd.push(Op::Merge { s: uid, rg: mr, on: false });
                inv.push(Op::Merge { s: uid, rg: mr, on: true });
            }
            if fwd.is_empty() {
                return Err("Nothing is merged there.".into());
            }
        }
        // Undo runs the steps backwards: the new area unmerged first, then content and old merges back.
        inv.reverse();
        Ok((inv, fwd))
    }

    // ------------------------------------------------------------------
    // Sheets
    // ------------------------------------------------------------------

    pub(super) fn make_sheet_add(&mut self, v: &Json) -> Result<Undo, String> {
        let wb = self.book.workbook();
        if wb.sheets.len() >= MAX_SHEETS {
            return Err("A workbook can have at most 255 sheets.".into());
        }
        let name = match v.get("name").and_then(Json::as_str).map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => self.check_sheet_name(n, None)?,
            None => wb.unique_sheet_name("Sheet"),
        };
        let pos = wb.sheets.iter().map(|s| s.pos).fold(0.0_f64, f64::max) + 1.0;
        let s = new_sheet_uid();
        self.extra.push(json!({ "t": "focus", "uid": s.to_string() }));
        Ok((vec![Op::DelSheet { s, name: name.clone() }], vec![Op::AddSheet { s, name, pos }]))
    }

    pub(super) fn make_sheet_rename(&mut self, v: &Json) -> Result<Undo, String> {
        let i = self.sheet_index(v).ok_or("No such sheet.")?;
        let to = self.check_sheet_name(v.get("name").and_then(Json::as_str).ok_or("A sheet needs a name.")?, Some(i))?;
        let from = self.book.workbook().sheets[i].name.clone();
        if from == to {
            return Err("Nothing to change.".into());
        }
        let s = self.uid(i);
        Ok((vec![Op::Rename { s, from: to.clone(), to: from.clone() }], vec![Op::Rename { s, from, to }]))
    }

    pub(super) fn make_sheet_del(&mut self, v: &Json) -> Result<Undo, String> {
        let i = self.sheet_index(v).ok_or("No such sheet.")?;
        let wb = self.book.workbook();
        let sh = &wb.sheets[i];
        if wb.sheets.len() <= 1 || (!sh.hidden && self.visible_sheets() <= 1) {
            return Err("A workbook needs at least one visible sheet.".into());
        }
        // Undo brings the sheet back with everything on it (formulas elsewhere that pointed at it stay #REF!).
        let mut tmp = wb.clone();
        tmp.sheets = vec![sh.clone()];
        let inv = live::state_ops(&tmp);
        let fwd = vec![Op::DelSheet { s: sh.uid, name: sh.name.clone() }];
        Ok((inv, fwd))
    }

    pub(super) fn make_sheet_move(&mut self, v: &Json) -> Result<Undo, String> {
        let i = self.sheet_index(v).ok_or("No such sheet.")?;
        let to = num(v, "to").ok_or("Invalid request.")? as usize;
        let wb = self.book.workbook();
        let n = wb.sheets.len();
        if to >= n || to == i {
            return Err("Nothing to change.".into());
        }
        // The tab order keys of the others, in order; the moved sheet gets a key between its new neighbours.
        let others: Vec<f64> = wb.sheets.iter().enumerate().filter(|(k, _)| *k != i).map(|(_, s)| s.pos).collect();
        let pos = if to == 0 {
            others[0] - 1.0
        } else if to >= others.len() {
            others[others.len() - 1] + 1.0
        } else {
            (others[to - 1] + others[to]) / 2.0
        };
        if !(pos.is_finite()) || (to > 0 && to < others.len() && (pos <= others[to - 1] || pos >= others[to])) {
            return Err("The tabs can't be reordered here.".into());
        }
        let s = self.uid(i);
        let old = wb.sheets[i].pos;
        Ok((vec![Op::Place { s, pos: old }], vec![Op::Place { s, pos }]))
    }

    pub(super) fn make_sheet_hide(&mut self, v: &Json) -> Result<Undo, String> {
        let i = self.sheet_index(v).ok_or("No such sheet.")?;
        let on = v.get("on").and_then(Json::as_bool).ok_or("Invalid request.")?;
        let wb = self.book.workbook();
        if wb.sheets[i].hidden == on {
            return Err("Nothing to change.".into());
        }
        if on && self.visible_sheets() <= 1 {
            return Err("A workbook needs at least one visible sheet.".into());
        }
        let s = self.uid(i);
        let prop = |h: bool| Op::Prop { s, key: "hidden".into(), val: json!(h) };
        Ok((vec![prop(!on)], vec![prop(on)]))
    }

    pub(super) fn make_sheet_color(&mut self, v: &Json) -> Result<Undo, String> {
        let i = self.sheet_index(v).ok_or("No such sheet.")?;
        let new = match v.get("color") {
            None | Some(Json::Null) => None,
            Some(c) => Some(super::parse_hex(c.as_str().unwrap_or("")).ok_or("Invalid colour.")?),
        };
        let old = self.book.workbook().sheets[i].tab_color;
        if old == new {
            return Err("Nothing to change.".into());
        }
        let s = self.uid(i);
        let prop = |c: Option<u32>| Op::Prop { s, key: "tab_color".into(), val: json!(c) };
        Ok((vec![prop(old)], vec![prop(new)]))
    }

    // ------------------------------------------------------------------
    // Clipboard and fill
    // ------------------------------------------------------------------

    /// A cell's displayed text.
    fn shown_at(&self, i: usize, r: u32, c: u32) -> String {
        let wb = self.book.workbook();
        let sh = &wb.sheets[i];
        freedom_sheets::numfmt::format_value(sh.display_value(r, c), &wb.style(sh.get(r, c).map_or(0, |x| x.style)).num_fmt).text
    }

    /// Takes a copy of a range: the page keeps it (formats and formulas) and gets the text for the system clipboard.
    pub(super) fn copy(&mut self, v: &Json, out: &mut Out) {
        let (i, rg) = match self.rect(v) {
            Ok(x) => x,
            Err(e) => return out.browser.push(json!({ "t": "error", "message": e, "id": v.get("id").cloned().unwrap_or(Json::Null) })),
        };
        self.ready();
        let cut = v.get("cut").and_then(Json::as_bool).unwrap_or(false) && self.editor;
        let mut text = String::new();
        let mut cells = Vec::new();
        for r in rg.r1..=rg.r2 {
            let mut row = Vec::new();
            for c in rg.c1..=rg.c2 {
                let (content, st) = self.pair_of(i, r, c);
                let val = live::val_of(self.book.workbook().sheets[i].display_value(r, c));
                row.push((content, st, val));
                if c > rg.c1 {
                    text.push('\t');
                }
                let t = self.shown_at(i, r, c);
                if t.contains(['\t', '\n', '\r', '"']) {
                    text.push('"');
                    text.push_str(&t.replace('"', "\"\""));
                    text.push('"');
                } else {
                    text.push_str(&t);
                }
            }
            cells.push(row);
            text.push('\n');
        }
        self.clip_seq += 1;
        let id = self.clip_seq;
        self.clip = Some(Clip { id, sheet: self.uid(i), r1: rg.r1, c1: rg.c1, h: rg.rows(), w: rg.cols(), cut, cells });
        out.browser.push(json!({ "t": "clip", "clip": id, "id": v.get("id").cloned().unwrap_or(Json::Null), "cut": cut, "text": text, "rows": rg.rows(), "cols": rg.cols() }));
    }

    /// A formula moved by (dr, dc) as a copy would be (references that would leave the sheet become #REF!).
    fn shifted(f: &str, dr: i64, dc: i64) -> String {
        if dr == 0 && dc == 0 {
            return f.to_string();
        }
        match formula::parse(f.trim_start_matches('=')) {
            Ok(mut e) => {
                formula::shift_relative(&mut e, dr, dc);
                formula::print(&e)
            }
            Err(_) => f.to_string(),
        }
    }

    fn moved_content(d: &Option<Box<CellData>>, dr: i64, dc: i64) -> Option<Box<CellData>> {
        d.as_ref().map(|d| {
            let mut d = (**d).clone();
            if let Some(f) = &d.f {
                d.f = Some(Self::shifted(f, dr, dc));
            }
            Box::new(d)
        })
    }

    pub(super) fn make_paste(&mut self, v: &Json) -> Result<Undo, String> {
        let i = self.sheet_index(v).ok_or("No such sheet.")?;
        let g = |k: &str| num(v, k).filter(|x| *x < u64::from(MAX_ROWS));
        let (Some(r0), Some(c0)) = (g("r"), g("c")) else { return Err("Invalid cell.".into()) };
        let what = v.get("what").and_then(Json::as_str).unwrap_or("all");
        if !matches!(what, "all" | "values" | "formats") {
            return Err("Invalid request.".into());
        }
        let uid = self.uid(i);
        let (mut fwd, mut inv) = (Vec::new(), Vec::new());
        if let Some(cid) = num(v, "clip") {
            let clip = self.clip.take().filter(|c| c.id == cid).ok_or("That copy is no longer available. Copy again.")?;
            let (h, w) = (u64::from(clip.h), u64::from(clip.w));
            // A target that is a whole multiple of the copied area is filled with repeats.
            let (mut th, mut tw) = (h, w);
            if let (Some(r2), Some(c2)) = (g("r2"), g("c2")) {
                let (sh, sw) = (r2.saturating_sub(r0) + 1, c2.saturating_sub(c0) + 1);
                if sh % h == 0 && sw % w == 0 {
                    (th, tw) = (sh, sw);
                }
            }
            if r0 + th > u64::from(MAX_ROWS) || c0 + tw > u64::from(MAX_COLS) {
                self.clip = Some(clip);
                return Err("That doesn't fit on the sheet.".into());
            }
            if th * tw > MAX_EDIT_CELLS {
                self.clip = Some(clip);
                return Err("That is too many cells at once.".into());
            }
            let same_sheet = clip.sheet == uid;
            for dr in 0..th {
                for dc in 0..tw {
                    let (r, c) = (r0 as u32 + dr as u32, c0 as u32 + dc as u32);
                    let (content, st, val) = &clip.cells[(dr % h) as usize][(dc % w) as usize];
                    let (sr, sc) = (clip.r1 + (dr % h) as u32, clip.c1 + (dc % w) as u32);
                    let new_content = match what {
                        "formats" => None,
                        "values" => Some(match val {
                            Val::Empty => None,
                            x => Some(Box::new(CellData { v: x.clone(), ..Default::default() })),
                        }),
                        // A cut keeps its formulas as they are; a copy moves relative references.
                        _ if clip.cut && same_sheet => Some(content.clone()),
                        _ => Some(Self::moved_content(content, i64::from(r) - i64::from(sr), i64::from(c) - i64::from(sc))),
                    };
                    let new_style = if what == "values" { None } else { Some(st) };
                    self.put(i, r, c, new_content, new_style, &mut fwd, &mut inv);
                }
            }
            if clip.cut {
                // The source empties (except where the paste landed on it).
                if let Some(si) = live_index(self, clip.sheet) {
                    let dest = Range::new(r0 as u32, c0 as u32, (r0 + th - 1) as u32, (c0 + tw - 1) as u32);
                    for r in clip.r1..clip.r1 + clip.h {
                        for c in clip.c1..clip.c1 + clip.w {
                            if si == i && dest.contains(r, c) {
                                continue;
                            }
                            self.put(si, r, c, Some(None), Some(&Style::default()), &mut fwd, &mut inv);
                        }
                    }
                }
            } else {
                self.clip = Some(clip);
            }
        } else {
            // Text from the system clipboard, already split into cells by the page.
            let grid = v.get("grid").and_then(Json::as_array).ok_or("Nothing to paste.")?;
            let mut bytes = 0usize;
            let mut cellsn = 0u64;
            for row in grid {
                let row = row.as_array().ok_or("Invalid paste.")?;
                cellsn += row.len() as u64;
                for t in row {
                    bytes += t.as_str().ok_or("Invalid paste.")?.len();
                }
                if bytes > MAX_PASTE_BYTES || cellsn > MAX_EDIT_CELLS {
                    return Err("That is too much to paste at once.".into());
                }
            }
            if grid.is_empty() {
                return Err("Nothing to paste.".into());
            }
            let width = grid.iter().map(|r| r.as_array().map_or(0, Vec::len)).max().unwrap_or(0) as u64;
            if r0 + grid.len() as u64 > u64::from(MAX_ROWS) || c0 + width > u64::from(MAX_COLS) {
                return Err("That doesn't fit on the sheet.".into());
            }
            for (dr, row) in grid.iter().enumerate() {
                for (dc, t) in row.as_array().into_iter().flatten().enumerate() {
                    let t = clean(t.as_str().unwrap_or(""));
                    let (r, c) = (r0 as u32 + dr as u32, c0 as u32 + dc as u32);
                    let (content, st) = self.entry(i, r, c, &t, true)?;
                    self.put(i, r, c, Some(content), Some(&st), &mut fwd, &mut inv);
                }
            }
        }
        if fwd.is_empty() {
            return Err("Nothing changed.".into());
        }
        inv.reverse();
        Ok((inv, fwd))
    }

    pub(super) fn make_fill(&mut self, v: &Json) -> Result<Undo, String> {
        let (i, rg) = self.rect(v)?;
        let down = match v.get("dir").and_then(Json::as_str) {
            Some("down") => true,
            Some("right") => false,
            _ => return Err("Invalid request.".into()),
        };
        if (down && rg.r1 == rg.r2) || (!down && rg.c1 == rg.c2) {
            return Err("Select the cells to fill, including the one to copy from.".into());
        }
        let (mut fwd, mut inv) = (Vec::new(), Vec::new());
        if down {
            for c in rg.c1..=rg.c2 {
                let (content, st) = self.pair_of(i, rg.r1, c);
                for r in rg.r1 + 1..=rg.r2 {
                    let moved = Self::moved_content(&content, i64::from(r - rg.r1), 0);
                    self.put(i, r, c, Some(moved), Some(&st), &mut fwd, &mut inv);
                }
            }
        } else {
            for r in rg.r1..=rg.r2 {
                let (content, st) = self.pair_of(i, r, rg.c1);
                for c in rg.c1 + 1..=rg.c2 {
                    let moved = Self::moved_content(&content, 0, i64::from(c - rg.c1));
                    self.put(i, r, c, Some(moved), Some(&st), &mut fwd, &mut inv);
                }
            }
        }
        if fwd.is_empty() {
            return Err("Nothing changed.".into());
        }
        inv.reverse();
        Ok((inv, fwd))
    }

    /// Drag the fill handle: the area `r1..c2` is repeated into `r2n..c2n` (which contains it), as a
    /// linear series when it holds two or more plain numbers in a line.
    pub(super) fn make_autofill(&mut self, v: &Json) -> Result<Undo, String> {
        let (i, src) = self.rect(v)?;
        let g = |k: &str| num(v, k).filter(|x| *x < u64::from(MAX_ROWS)).map(|x| x as u32);
        let (Some(er), Some(ec)) = (g("r2n"), g("c2n")) else { return Err("Invalid range.".into()) };
        let down = er > src.r2 && ec <= src.c2;
        let right = ec > src.c2 && er <= src.r2;
        if !(down || right) {
            return Err("Drag the handle below or to the right of the selection.".into());
        }
        if (u64::from(er.max(src.r2) - src.r1 + 1)) * u64::from(ec.max(src.c2) - src.c1 + 1) > MAX_EDIT_CELLS {
            return Err("That is too many cells at once.".into());
        }
        let (mut fwd, mut inv) = (Vec::new(), Vec::new());
        let (h, w) = (src.rows(), src.cols());
        let (rows, cols): (Vec<u32>, Vec<u32>) = if down { ((src.r2 + 1..=er).collect(), (src.c1..=src.c2).collect()) } else { ((src.r1..=src.r2).collect(), (src.c2 + 1..=ec).collect()) };
        if down {
            for c in &cols {
                let col: Vec<(Option<Box<CellData>>, Style)> = (src.r1..=src.r2).map(|r| self.pair_of(i, r, *c)).collect();
                let series = linear(&col);
                for r in &rows {
                    let k = (*r - src.r1) as usize;
                    let (content, st) = &col[k % h as usize];
                    let new = match &series {
                        Some((start, step)) => Some(Box::new(CellData { v: Val::Num(start + step * k as f64), ..Default::default() })),
                        None => Self::moved_content(content, i64::from(*r) - i64::from(src.r1 + (k as u32 % h)), 0),
                    };
                    self.put(i, *r, *c, Some(new), Some(st), &mut fwd, &mut inv);
                }
            }
        } else {
            for r in &rows {
                let row: Vec<(Option<Box<CellData>>, Style)> = (src.c1..=src.c2).map(|c| self.pair_of(i, *r, c)).collect();
                let series = linear(&row);
                for c in &cols {
                    let k = (*c - src.c1) as usize;
                    let (content, st) = &row[k % w as usize];
                    let new = match &series {
                        Some((start, step)) => Some(Box::new(CellData { v: Val::Num(start + step * k as f64), ..Default::default() })),
                        None => Self::moved_content(content, 0, i64::from(*c) - i64::from(src.c1 + (k as u32 % w))),
                    };
                    self.put(i, *r, *c, Some(new), Some(st), &mut fwd, &mut inv);
                }
            }
        }
        if fwd.is_empty() {
            return Err("Nothing changed.".into());
        }
        inv.reverse();
        Ok((inv, fwd))
    }

    // ------------------------------------------------------------------
    // Cells as typed
    // ------------------------------------------------------------------

    /// What typing `text` into a cell makes of it: the content and the style (a typed date or
    /// percentage brings its format along when the cell has none).
    pub(super) fn entry(&mut self, i: usize, r: u32, c: u32, text: &str, lenient: bool) -> Result<(Option<Box<CellData>>, Style), String> {
        if text.len() > MAX_ENTRY {
            return Err("That text is too long for a cell.".into());
        }
        let text = text.to_string();
        if let Some(f) = text.strip_prefix('=').filter(|f| !f.trim().is_empty())
            && let Err(e) = formula::parse(f)
        {
            if !lenient {
                return Err(format!("That formula can't be read ({e})."));
            }
            // Pasted text that only looks like a formula stays text.
            let wb = self.book.workbook();
            let st = wb.style(wb.sheets[i].get(r, c).map_or(0, |x| x.style)).clone();
            return Ok((Some(Box::new(CellData { v: Val::Text(text), ..Default::default() })), st));
        }
        let base_style = self.book.workbook().sheets[i].get(r, c).map_or(0, |x| x.style);
        let new = freedom_sheets::model::cell_from_input(self.book.workbook_mut(), &text, base_style);
        let wb = self.book.workbook();
        Ok(match &new {
            Some(cell) => {
                let d = live::cell_data(wb, &wb.sheets[i], r, c, cell);
                let content = (!d.v.is_empty() || d.f.is_some()).then(|| Box::new(CellData { st: None, rich: None, ..d }));
                (content, wb.style(cell.style).clone())
            }
            None => (None, self.style_of(i, r, c)),
        })
    }

    // ------------------------------------------------------------------
    // Formats
    // ------------------------------------------------------------------

    /// Border presets over a range: none, all, outer, thick_outer, top, bottom, left, right.
    pub(super) fn apply_border(preset: &str, rg: Range, r: u32, c: u32, st: &mut Style) -> Result<(), String> {
        let (top, bottom, left, right) = (r == rg.r1, r == rg.r2, c == rg.c1, c == rg.c2);
        let t = BorderLine::Thin;
        let b = &mut st.border;
        match preset {
            "none" => *b = Default::default(),
            "all" => {
                b.left = t;
                b.right = t;
                b.top = t;
                b.bottom = t;
            }
            "outer" | "thick_outer" => {
                let k = if preset == "outer" { t } else { BorderLine::Thick };
                if left {
                    b.left = k;
                }
                if right {
                    b.right = k;
                }
                if top {
                    b.top = k;
                }
                if bottom {
                    b.bottom = k;
                }
            }
            "top" if top => b.top = t,
            "bottom" if bottom => b.bottom = t,
            "left" if left => b.left = t,
            "right" if right => b.right = t,
            "top" | "bottom" | "left" | "right" => {}
            _ => return Err("Unknown border.".into()),
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Find
    // ------------------------------------------------------------------

    pub(super) fn find(&mut self, v: &Json, out: &mut Out) {
        let q = v.get("q").and_then(Json::as_str).unwrap_or("");
        let id = v.get("id").cloned().unwrap_or(Json::Null);
        if q.is_empty() || q.len() > 200 {
            return out.browser.push(json!({ "t": "found", "id": id, "hits": [], "more": false }));
        }
        let case = v.get("case").and_then(Json::as_bool).unwrap_or(false);
        let whole = v.get("whole").and_then(Json::as_bool).unwrap_or(false);
        let formulas = v.get("formulas").and_then(Json::as_bool).unwrap_or(false);
        let all = v.get("all").and_then(Json::as_bool).unwrap_or(true);
        let only = self.sheet_index(v);
        self.ready();
        let needle = if case { q.to_string() } else { q.to_lowercase() };
        let wb = self.book.workbook();
        let (mut hits, mut more, mut scanned) = (Vec::new(), false, 0usize);
        let deadline = Instant::now() + Duration::from_millis(1500);
        'sheets: for (i, sh) in wb.sheets.iter().enumerate() {
            if (!all && Some(i) != only) || (all && sh.hidden) {
                continue;
            }
            for (r, row) in &sh.rows {
                for (c, cell) in row {
                    scanned += 1;
                    if scanned > MAX_SCAN || (scanned % 4096 == 0 && Instant::now() > deadline) {
                        more = true;
                        break 'sheets;
                    }
                    if cell.value.is_empty() && cell.formula.is_none() {
                        continue;
                    }
                    let mut text = freedom_sheets::numfmt::format_value(sh.display_value(*r, *c), &wb.style(cell.style).num_fmt).text;
                    let mut ok = test(&mut text, &needle, case, whole);
                    if !ok && formulas && cell.formula.is_some() {
                        let mut f = cell.input_text();
                        ok = test(&mut f, &needle, case, whole);
                    }
                    if ok {
                        if hits.len() >= MAX_HITS {
                            more = true;
                            break 'sheets;
                        }
                        let shown: String = text.chars().take(80).collect();
                        hits.push(json!([i, r, c, shown]));
                    }
                }
            }
        }
        out.browser.push(json!({ "t": "found", "id": id, "hits": hits, "more": more }));
    }
}

fn test(text: &mut String, needle: &str, case: bool, whole: bool) -> bool {
    if !case {
        *text = text.to_lowercase();
    }
    if whole { text.as_str() == needle } else { text.contains(needle) }
}

fn live_index(b: &Bridge, uid: u64) -> Option<usize> {
    b.book.workbook().sheets.iter().position(|s| s.uid == uid)
}

/// Pasted text without control characters (tabs and line breaks inside a cell become spaces) or text-direction overrides.
fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{0}'))
        .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
        .collect()
}

/// (first value, step) when the cells are two or more plain numbers with a constant step.
fn linear(cells: &[(Option<Box<CellData>>, Style)]) -> Option<(f64, f64)> {
    if cells.len() < 2 {
        return None;
    }
    let nums: Vec<f64> = cells
        .iter()
        .map(|(c, _)| match c.as_deref() {
            Some(CellData { v: Val::Num(n), f: None, .. }) => Some(*n),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let step = nums[1] - nums[0];
    nums.windows(2).all(|w| ((w[1] - w[0]) - step).abs() < 1e-9).then_some((nums[0], step))
}
