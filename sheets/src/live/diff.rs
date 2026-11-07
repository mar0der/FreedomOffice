//! The operations a command made: the state before it (the undo snapshot of its scopes)
//! compared with the state after, plus the row/column inserts and deletes it recorded (those
//! can't be told from a comparison). Produces the inverse operations too, for undo.

use std::collections::{BTreeSet, HashMap, HashSet};

use cosync::{Op, Rect, SheetId};

use super::{apply_struct, cell_data, feature_changed, feature_get, index_of, prop_get, prop_same, ApplyState, FEATURES, PROPS};
use crate::model::{Cell, Range, Sheet, Workbook};

/// The state of a command's scopes before it ran.
pub enum Before {
    Range { s: SheetId, rg: Range, cells: Vec<(u32, u32, Cell)> },
    Sheet(Box<Sheet>),
    All { sheets: Vec<Sheet>, names: Vec<(String, String)> },
}

/// A local change: what to send, and how to undo it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Change {
    pub ops: Vec<Op>,
    pub inverse: Vec<Op>,
}

/// Row/column inserts and deletes recorded during a command: (sheet, rows, at, n).
pub type Structs = [(SheetId, bool, u32, i64)];

pub fn diff(befores: Vec<Before>, structs: &Structs, wb: &mut Workbook) -> Change {
    let mut befores = befores;
    if let Some(i) = befores.iter().position(|b| matches!(b, Before::All { .. })) {
        let Before::All { sheets, names } = befores.swap_remove(i) else { unreachable!() };
        return diff_all(sheets, names, structs, wb);
    }
    debug_assert!(structs.is_empty(), "row/column changes need a whole-workbook undo scope");
    let mut ops: Vec<Op> = structs.iter().map(|&(s, rows, at, n)| struct_op(s, rows, at, n)).collect();
    let mut inverse = Vec::new();
    let whole: HashSet<SheetId> = befores.iter().filter_map(|b| if let Before::Sheet(s) = b { Some(s.uid) } else { None }).collect();
    for b in &befores {
        match b {
            Before::Sheet(x) => {
                if let Some(i) = index_of(wb, x.uid) {
                    sheet_pair(wb, x, wb, &wb.sheets[i], &mut ops, &mut inverse);
                }
            }
            Before::Range { s, rg, cells } if !whole.contains(s) => {
                if let Some(i) = index_of(wb, *s) {
                    range_pair(wb, *rg, cells, &wb.sheets[i], &mut ops, &mut inverse);
                }
            }
            _ => {}
        }
    }
    Change { ops, inverse }
}

fn struct_op(s: SheetId, rows: bool, at: u32, n: i64) -> Op {
    let n = n.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    if rows { Op::Rows { s, at, n } } else { Op::Cols { s, at, n } }
}

fn diff_all(before: Vec<Sheet>, names: Vec<(String, String)>, structs: &Structs, wb: &mut Workbook) -> Change {
    // A copied sheet keeps its source's identity in a plain clone: give it its own.
    let mut seen = HashSet::new();
    for sh in &mut wb.sheets {
        if !seen.insert(sh.uid) {
            sh.uid = crate::model::new_sheet_uid();
            seen.insert(sh.uid);
        }
    }
    let had: HashMap<SheetId, (String, f64)> = before.iter().map(|s| (s.uid, (s.name.clone(), s.pos))).collect();
    let now: HashSet<SheetId> = wb.sheets.iter().map(|s| s.uid).collect();
    // Tab order: keep as many order keys as possible, give the rest keys in between.
    let keys = assign_positions(&wb.sheets.iter().map(|s| (s.uid, had.get(&s.uid).map(|h| h.1))).collect::<Vec<_>>());
    for (sh, k) in wb.sheets.iter_mut().zip(&keys) {
        sh.pos = *k;
    }
    let mut sops = Vec::new();
    for sh in wb.sheets.iter().filter(|s| !had.contains_key(&s.uid)) {
        sops.push(Op::AddSheet { s: sh.uid, name: sh.name.clone(), pos: sh.pos });
    }
    for sh in &wb.sheets {
        if let Some((name, _)) = had.get(&sh.uid) {
            if *name != sh.name {
                sops.push(Op::Rename { s: sh.uid, from: name.clone(), to: sh.name.clone() });
            }
        }
    }
    sops.extend(structs.iter().map(|&(s, rows, at, n)| struct_op(s, rows, at, n)));
    for b in before.iter().filter(|b| !now.contains(&b.uid)) {
        sops.push(Op::DelSheet { s: b.uid, name: b.name.clone() });
    }
    for sh in &wb.sheets {
        if let Some((_, pos)) = had.get(&sh.uid) {
            if pos.to_bits() != sh.pos.to_bits() {
                sops.push(Op::Place { s: sh.uid, pos: sh.pos });
            }
        }
    }
    let original = (!sops.is_empty()).then(|| (before.clone(), names.clone()));
    let mut base = Workbook::empty_with(before);
    base.names = names;
    let mut st = ApplyState::default();
    for op in &sops {
        apply_struct(&mut base, op, false, &mut st);
    }
    let mut ops = sops.clone();
    let mut inverse = Vec::new();
    let mut content_inverse = Vec::new();
    for sh in &wb.sheets {
        if let Some(i) = index_of(&base, sh.uid) {
            sheet_pair(wb, &base.sheets[i], wb, sh, &mut ops, &mut content_inverse);
        }
    }
    if base.names != wb.names {
        ops.push(book_names(&wb.names));
        content_inverse.push(book_names(&base.names));
    }
    match original {
        None => inverse = content_inverse,
        Some((osheets, onames)) => {
            // Undo = the structural changes reversed, then whatever content still differs.
            let sinv = inverse_of_structs(&sops, &osheets);
            let mut back = Workbook::empty_with(wb.sheets.clone());
            back.names = wb.names.clone();
            for op in &sinv {
                apply_struct(&mut back, op, false, &mut st);
            }
            inverse.extend(sinv);
            let mut scratch = Vec::new();
            for o in &osheets {
                if let Some(i) = index_of(&back, o.uid) {
                    sheet_pair(wb, &back.sheets[i], wb, o, &mut inverse, &mut scratch);
                }
            }
            if back.names != onames {
                inverse.push(book_names(&onames));
            }
        }
    }
    Change { ops, inverse }
}

fn book_names(names: &[(String, String)]) -> Op {
    Op::Book { key: "names".into(), val: serde_json::to_value(names).unwrap_or_default(), adj: Vec::new() }
}

/// The structural operations that undo `sops` (in reverse), given the sheets before them.
pub fn inverse_of_structs(sops: &[Op], before: &[Sheet]) -> Vec<Op> {
    let pos_of = |s: SheetId| before.iter().find(|b| b.uid == s).map_or(0.0, |b| b.pos);
    sops.iter()
        .rev()
        .filter_map(|op| match op {
            Op::AddSheet { s, name, .. } => Some(Op::DelSheet { s: *s, name: name.clone() }),
            Op::DelSheet { s, name } => Some(Op::AddSheet { s: *s, name: name.clone(), pos: pos_of(*s) }),
            Op::Rename { s, from, to } => Some(Op::Rename { s: *s, from: to.clone(), to: from.clone() }),
            Op::Rows { s, at, n } => Some(Op::Rows { s: *s, at: *at, n: -n }),
            Op::Cols { s, at, n } => Some(Op::Cols { s: *s, at: *at, n: -n }),
            Op::Place { s, .. } => Some(Op::Place { s: *s, pos: pos_of(*s) }),
            _ => None,
        })
        .collect()
}

/// Order keys for sheets in their new order: existing keys (`Some`) are kept where they are
/// already in order (longest increasing run), the others get keys in between.
fn assign_positions(e: &[(SheetId, Option<f64>)]) -> Vec<f64> {
    let n = e.len();
    let lt = |a: (f64, u64), b: (f64, u64)| a.0 < b.0 || (a.0 == b.0 && a.1 < b.1);
    let idx: Vec<usize> = (0..n).filter(|&i| e[i].1.is_some_and(f64::is_finite)).collect();
    let key = |i: usize| (e[i].1.unwrap_or(0.0), e[i].0);
    let m = idx.len();
    let mut len = vec![1usize; m];
    let mut prev = vec![usize::MAX; m];
    for j in 0..m {
        for k in 0..j {
            if lt(key(idx[k]), key(idx[j])) && len[k] + 1 > len[j] {
                len[j] = len[k] + 1;
                prev[j] = k;
            }
        }
    }
    let mut out: Vec<Option<f64>> = vec![None; n];
    if let Some(mut j) = (0..m).max_by_key(|&j| (len[j], usize::MAX - j)) {
        loop {
            out[idx[j]] = e[idx[j]].1;
            if prev[j] == usize::MAX {
                break;
            }
            j = prev[j];
        }
    }
    let mut i = 0;
    while i < n {
        if out[i].is_some() {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && out[i].is_none() {
            i += 1;
        }
        let cnt = (i - start) as f64;
        let lo = if start > 0 { out[start - 1] } else { None };
        let hi = if i < n { out[i] } else { None };
        let (a, b) = match (lo, hi) {
            (Some(a), Some(b)) => (a, b),
            (Some(a), None) => (a, a + cnt + 1.0),
            (None, Some(b)) => (b - cnt - 1.0, b),
            (None, None) => (-1.0, cnt),
        };
        let step = (b - a) / (cnt + 1.0);
        for k in 0..(i - start) {
            out[start + k] = Some(a + step * (k as f64 + 1.0));
        }
    }
    let res: Vec<f64> = out.into_iter().map(|x| x.unwrap_or(0.0)).collect();
    if (1..n).all(|i| lt((res[i - 1], e[i - 1].0), (res[i], e[i].0))) {
        res
    } else {
        (0..n).map(|i| i as f64).collect()
    }
}

fn data_of(w: &Workbook, sh: &Sheet, r: u32, c: u32) -> Option<Box<cosync::CellData>> {
    sh.get(r, c).filter(|x| !x.is_blank()).map(|x| Box::new(cell_data(w, sh, r, c, x)))
}

/// Every difference between sheet `x` (before, styles of `wx`) and `y` (after, styles of `wy`):
/// forward operations into `fwd`, the reverse into `inv`.
pub(super) fn sheet_pair(wx: &Workbook, x: &Sheet, wy: &Workbook, y: &Sheet, fwd: &mut Vec<Op>, inv: &mut Vec<Op>) {
    let s = y.uid;
    // Cells
    let rows: BTreeSet<u32> = x.rows.keys().chain(y.rows.keys()).copied().collect();
    for r in rows {
        let (rx, ry) = (x.rows.get(&r), y.rows.get(&r));
        let cols: BTreeSet<u32> = rx.into_iter().flat_map(|m| m.keys()).chain(ry.into_iter().flat_map(|m| m.keys())).copied().collect();
        for c in cols {
            let a = x.get(r, c).map(|cell| (x, r, c, cell));
            let b = y.get(r, c).map(|cell| (y, r, c, cell));
            let (content, format) = (same_content(a, b, true), same_format(wx, a, wy, b));
            cell_ops(s, r, c, (content, format), (wx, x), (wy, y), true, fwd, inv);
        }
    }
    // Notes (with their authors) and links
    let keys: BTreeSet<(u32, u32)> = x.notes.keys().chain(y.notes.keys()).chain(x.note_authors.keys()).chain(y.note_authors.keys()).copied().collect();
    for k in keys {
        let (nx, ny) = ((x.notes.get(&k), x.note_authors.get(&k)), (y.notes.get(&k), y.note_authors.get(&k)));
        if nx != ny {
            fwd.push(Op::Note { s, r: k.0, c: k.1, text: ny.0.cloned(), author: ny.1.cloned() });
            inv.push(Op::Note { s, r: k.0, c: k.1, text: nx.0.cloned(), author: nx.1.cloned() });
        }
    }
    let keys: BTreeSet<(u32, u32)> = x.links.keys().chain(y.links.keys()).copied().collect();
    for k in keys {
        if x.links.get(&k) != y.links.get(&k) {
            fwd.push(Op::Link { s, r: k.0, c: k.1, url: y.links.get(&k).cloned() });
            inv.push(Op::Link { s, r: k.0, c: k.1, url: x.links.get(&k).cloned() });
        }
    }
    // Row heights, column widths, hidden rows/columns, outline levels
    let bits = |v: Option<&f32>| v.map(|f| f.to_bits());
    let keys: BTreeSet<u32> = x.row_h.keys().chain(y.row_h.keys()).copied().collect();
    for r in keys {
        if bits(x.row_h.get(&r)) != bits(y.row_h.get(&r)) {
            fwd.push(Op::RowH { s, r, h: y.row_h.get(&r).copied() });
            inv.push(Op::RowH { s, r, h: x.row_h.get(&r).copied() });
        }
    }
    let keys: BTreeSet<u32> = x.col_w.keys().chain(y.col_w.keys()).copied().collect();
    for c in keys {
        if bits(x.col_w.get(&c)) != bits(y.col_w.get(&c)) {
            fwd.push(Op::ColW { s, c, w: y.col_w.get(&c).copied() });
            inv.push(Op::ColW { s, c, w: x.col_w.get(&c).copied() });
        }
    }
    let keys: BTreeSet<u32> = x.hidden_rows.symmetric_difference(&y.hidden_rows).copied().collect();
    for r in keys {
        let on = y.hidden_rows.contains(&r);
        fwd.push(Op::RowHidden { s, r, on });
        inv.push(Op::RowHidden { s, r, on: !on });
    }
    let keys: BTreeSet<u32> = x.hidden_cols.symmetric_difference(&y.hidden_cols).copied().collect();
    for c in keys {
        let on = y.hidden_cols.contains(&c);
        fwd.push(Op::ColHidden { s, c, on });
        inv.push(Op::ColHidden { s, c, on: !on });
    }
    let keys: BTreeSet<u32> = x.row_levels.keys().chain(y.row_levels.keys()).copied().collect();
    for r in keys {
        let (a, b) = (x.row_levels.get(&r).copied().unwrap_or(0), y.row_levels.get(&r).copied().unwrap_or(0));
        if a != b {
            fwd.push(Op::RowLevel { s, r, l: b });
            inv.push(Op::RowLevel { s, r, l: a });
        }
    }
    let keys: BTreeSet<u32> = x.col_levels.keys().chain(y.col_levels.keys()).copied().collect();
    for c in keys {
        let (a, b) = (x.col_levels.get(&c).copied().unwrap_or(0), y.col_levels.get(&c).copied().unwrap_or(0));
        if a != b {
            fwd.push(Op::ColLevel { s, c, l: b });
            inv.push(Op::ColLevel { s, c, l: a });
        }
    }
    // Merges: unmerge first, then merge.
    let rect = |m: &Range| Rect { r1: m.r1, c1: m.c1, r2: m.r2, c2: m.c2 };
    let (mx, my): (Vec<Range>, Vec<Range>) = (x.merges.clone(), y.merges.clone());
    for m in mx.iter().filter(|m| !my.contains(m)) {
        fwd.push(Op::Merge { s, rg: rect(m), on: false });
    }
    for m in my.iter().filter(|m| !mx.contains(m)) {
        inv.push(Op::Merge { s, rg: rect(m), on: false });
    }
    for m in my.iter().filter(|m| !mx.contains(m)) {
        fwd.push(Op::Merge { s, rg: rect(m), on: true });
    }
    for m in mx.iter().filter(|m| !my.contains(m)) {
        inv.push(Op::Merge { s, rg: rect(m), on: true });
    }
    // Sheet properties and feature lists
    for key in PROPS {
        if !prop_same(x, y, key) {
            if let (Some(a), Some(b)) = (prop_get(x, key), prop_get(y, key)) {
                fwd.push(Op::Prop { s, key: key.to_string(), val: b });
                inv.push(Op::Prop { s, key: key.to_string(), val: a });
            }
        }
    }
    for key in FEATURES {
        if let Some(b) = feature_changed(x, y, key) {
            fwd.push(Op::Feature { s, key: key.to_string(), val: b, adj: Vec::new() });
            inv.push(Op::Feature { s, key: key.to_string(), val: feature_get(x, key).unwrap_or_default(), adj: Vec::new() });
        }
    }
}

/// Whether two cells hold the same value or formula (and rich text, when `rich`).
fn same_content(a: Option<(&Sheet, u32, u32, &Cell)>, b: Option<(&Sheet, u32, u32, &Cell)>, rich: bool) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some((sa, ra, ca, x)), Some((sb, rb, cb, y))) => {
            let content = match (&x.formula, &y.formula) {
                (Some(f), Some(g)) => f.text == g.text,
                (None, None) => x.value == y.value,
                _ => false,
            };
            content && (!rich || sa.rich_at(ra, ca) == sb.rich_at(rb, cb))
        }
        // (A cell that is only formatted has no content.)
        (Some((_, _, _, x)), None) | (None, Some((_, _, _, x))) => x.value.is_empty() && x.formula.is_none(),
    }
}

/// Whether two cells have the same format (styles compared by content).
fn same_format(wx: &Workbook, a: Option<(&Sheet, u32, u32, &Cell)>, wy: &Workbook, b: Option<(&Sheet, u32, u32, &Cell)>) -> bool {
    let style = |c: Option<(&Sheet, u32, u32, &Cell)>| c.map_or(0, |c| c.3.style);
    wx.style(style(a)) == wy.style(style(b))
}

/// The value, formula and rich text of a cell (None when it has none).
fn content_of(w: &Workbook, sh: &Sheet, r: u32, c: u32, rich: bool) -> Option<Box<cosync::CellData>> {
    let d = cell_data(w, sh, r, c, sh.get(r, c)?);
    let d = cosync::CellData { st: None, rich: if rich { d.rich } else { None }, ..d };
    (!d.v.is_empty() || d.f.is_some() || d.rich.is_some()).then(|| Box::new(d))
}

/// The style content of a cell (None for the default style or no cell).
fn format_of(w: &Workbook, sh: &Sheet, r: u32, c: u32) -> Option<serde_json::Value> {
    sh.get(r, c).and_then(|x| super::style_json(w.style(x.style)))
}

/// The operation(s) for one cell that differs: `Content` when only its value/formula/rich text
/// changed, `Format` when only its style did, a whole `Cell` when both did (like paste or
/// clear), each with its inverse.
#[allow(clippy::too_many_arguments)]
fn cell_ops(s: SheetId, r: u32, c: u32, same: (bool, bool), x: (&Workbook, &Sheet), y: (&Workbook, &Sheet), rich: bool, fwd: &mut Vec<Op>, inv: &mut Vec<Op>) {
    let adj = Vec::new;
    match same {
        (true, true) => {}
        (false, false) => {
            fwd.push(Op::Cell { s, r, c, v: data_of(y.0, y.1, r, c), adj: adj() });
            inv.push(Op::Cell { s, r, c, v: data_of(x.0, x.1, r, c), adj: adj() });
        }
        (false, true) => {
            fwd.push(Op::Content { s, r, c, v: content_of(y.0, y.1, r, c, rich), adj: adj() });
            inv.push(Op::Content { s, r, c, v: content_of(x.0, x.1, r, c, rich), adj: adj() });
        }
        (true, false) => {
            fwd.push(Op::Format { s, r, c, st: format_of(y.0, y.1, r, c) });
            inv.push(Op::Format { s, r, c, st: format_of(x.0, x.1, r, c) });
        }
    }
}

/// Cells of a range snapshot against the sheet now.
fn range_pair(wb: &Workbook, rg: Range, before: &[(u32, u32, Cell)], sh: &Sheet, fwd: &mut Vec<Op>, inv: &mut Vec<Op>) {
    let s = sh.uid;
    let old: HashMap<(u32, u32), &Cell> = before.iter().map(|(r, c, x)| ((*r, *c), x)).collect();
    let mut keys: BTreeSet<(u32, u32)> = old.keys().copied().collect();
    keys.extend(sh.iter_range(rg).map(|(r, c, _)| (r, c)));
    for (r, c) in keys {
        let a = old.get(&(r, c)).copied();
        let b = sh.get(r, c);
        let content = match (a, b) {
            (None, None) => true,
            (Some(x), Some(y)) => match (&x.formula, &y.formula) {
                (Some(f), Some(g)) => f.text == g.text,
                (None, None) => x.value == y.value,
                _ => false,
            },
            (Some(x), None) | (None, Some(x)) => x.value.is_empty() && x.formula.is_none(),
        };
        let format = wb.style(a.map_or(0, |x| x.style)) == wb.style(b.map_or(0, |x| x.style));
        if content && format {
            continue;
        }
        // What the cell held before (without rich text, which the snapshot doesn't keep).
        let old = |with_style: bool| {
            a.filter(|x| !x.is_blank()).map(|x| {
                let (v, f) = match &x.formula {
                    Some(f) => (cosync::Val::Empty, Some(f.text.clone())),
                    None => (super::val_of(&x.value), None),
                };
                Box::new(cosync::CellData { v, f, st: if with_style { super::style_json(wb.style(x.style)) } else { None }, rich: None })
            })
        };
        let adj = Vec::new;
        match (content, format) {
            (false, false) => {
                fwd.push(Op::Cell { s, r, c, v: data_of(wb, sh, r, c), adj: adj() });
                inv.push(Op::Cell { s, r, c, v: old(true), adj: adj() });
            }
            (false, true) => {
                fwd.push(Op::Content { s, r, c, v: content_of(wb, sh, r, c, true), adj: adj() });
                inv.push(Op::Content { s, r, c, v: old(false).filter(|d| !d.v.is_empty() || d.f.is_some()), adj: adj() });
            }
            _ => {
                fwd.push(Op::Format { s, r, c, st: format_of(wb, sh, r, c) });
                inv.push(Op::Format { s, r, c, st: a.and_then(|x| super::style_json(wb.style(x.style))) });
            }
        }
    }
}

#[cfg(test)]
pub(super) fn positions_for_test(e: &[(SheetId, Option<f64>)]) -> Vec<f64> {
    assign_positions(e)
}
