//! Live co-editing in the workbook model: workbook content as `cosync` operations (cells,
//! styles by content, row/column data, merges, sheet properties, per-sheet feature lists,
//! structural changes), applying operations to a workbook, and finding the operations a
//! command made by comparing the state before it (the undo snapshot) with the state after.
//! No UI here: see `app::live_ui`; the connection is in `conn`.

#[cfg(feature = "ui")]
pub mod conn;
mod diff;

use cosync::{CellData, Op, SheetId, Val, RESCUE_SHEET};
use serde_json::Value as Json;

use crate::formula;
use crate::model::{Cell, ErrorKind, Formula, Range, Sheet, Style, Value, Workbook};

pub use diff::{diff, Before, Change};

// ---------------------------------------------------------------------------
// Values, styles and cells
// ---------------------------------------------------------------------------

pub fn val_of(v: &Value) -> Val {
    match v {
        Value::Empty => Val::Empty,
        Value::Number(n) => Val::Num(*n),
        Value::Text(t) => Val::Text(t.clone()),
        Value::Bool(b) => Val::Bool(*b),
        Value::Error(e) => Val::Err(e.as_str().to_string()),
    }
}

pub fn value_of(v: &Val) -> Value {
    match v {
        Val::Empty => Value::Empty,
        Val::Num(n) if n.is_finite() => Value::Number(*n),
        Val::Num(_) => Value::Error(ErrorKind::Num),
        Val::Text(t) => Value::Text(t.clone()),
        Val::Bool(b) => Value::Bool(*b),
        Val::Err(e) => Value::Error(ErrorKind::parse(e).unwrap_or(ErrorKind::Value)),
    }
}

/// Style content (None for the default style).
pub fn style_json(st: &Style) -> Option<Json> {
    if *st == Style::default() { None } else { serde_json::to_value(st).ok() }
}

/// What a cell holds, as sent: formula text or value, style content, rich text.
pub fn cell_data(wb: &Workbook, sh: &Sheet, r: u32, c: u32, cell: &Cell) -> CellData {
    let (v, f) = match &cell.formula {
        Some(f) => (Val::Empty, Some(f.text.clone())),
        None => (val_of(&cell.value), None),
    };
    CellData { v, f, st: style_json(wb.style(cell.style)), rich: sh.rich_at(r, c).and_then(|rt| serde_json::to_value(rt).ok()) }
}

// ---------------------------------------------------------------------------
// Per-sheet features, properties and workbook lists
// ---------------------------------------------------------------------------

macro_rules! sheet_fields {
    ($keys:ident, $get:ident, $same:ident, $set:ident; $($key:literal => $field:ident),* $(,)?) => {
        pub const $keys: &[&str] = &[$($key),*];

        fn $get(sh: &Sheet, key: &str) -> Option<Json> {
            match key {
                $($key => serde_json::to_value(&sh.$field).ok(),)*
                _ => None,
            }
        }

        fn $same(a: &Sheet, b: &Sheet, key: &str) -> bool {
            match key {
                $($key => a.$field == b.$field,)*
                _ => true,
            }
        }

        fn $set(sh: &mut Sheet, key: &str, v: Json) -> bool {
            match key {
                $($key => match serde_json::from_value(v) {
                    Ok(x) => {
                        sh.$field = x;
                        true
                    }
                    Err(_) => false,
                },)*
                _ => false,
            }
        }
    };
}

sheet_fields!(FEATURES, feature_get, feature_same, feature_set;
    "charts" => charts,
    "cond_formats" => cond_formats,
    "validations" => validations,
    "tables" => tables,
    "images" => images,
    "pivots" => pivots,
    "sparklines" => sparklines,
    "data_tables" => data_tables,
    "filter" => filter,
    "page" => page,
    "names" => names,
    "scenarios" => scenarios,
);

sheet_fields!(PROPS, prop_get, prop_same, prop_set;
    "tab_color" => tab_color,
    "hidden" => hidden,
    "freeze" => freeze,
    "protection" => protection,
);

/// Whether a feature differs (PartialEq first, then the serialized form, which leaves out
/// derived state such as a PivotTable's rendered layout).
fn feature_changed(a: &Sheet, b: &Sheet, key: &str) -> Option<Json> {
    if feature_same(a, b, key) {
        return None;
    }
    let (ja, jb) = (feature_get(a, key), feature_get(b, key));
    if ja == jb { None } else { jb }
}

/// Serde helper: picture bytes as base64 instead of a list of numbers.
pub mod b64 {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::sync::Arc;

    pub fn serialize<S: Serializer>(v: &Arc<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&crate::hash::base64_encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<Vec<u8>>, D::Error> {
        let t = String::deserialize(d)?;
        crate::hash::base64_decode(&t).map(Arc::new).ok_or_else(|| serde::de::Error::custom("invalid base64"))
    }
}

// ---------------------------------------------------------------------------
// Sheet identity and tab order
// ---------------------------------------------------------------------------

pub fn index_of(wb: &Workbook, s: SheetId) -> Option<usize> {
    wb.sheets.iter().position(|x| x.uid == s)
}

/// Orders sheets by (pos, uid) like every participant does, keeping the active sheet.
pub fn sort_sheets(wb: &mut Workbook) {
    let active = wb.sheets.get(wb.active).map(|s| s.uid);
    wb.sheets.sort_by(|a, b| a.pos.total_cmp(&b.pos).then(a.uid.cmp(&b.uid)));
    if let Some(u) = active {
        wb.active = index_of(wb, u).unwrap_or(0);
    }
    wb.active = wb.active.min(wb.sheets.len().saturating_sub(1));
}

/// Gives the sheets of a workbook their live identities: from the snapshot's metadata
/// ("id:pos,..."), or by position when there is none.
pub fn assign_ids(wb: &mut Workbook, meta: &str) {
    let parsed: Vec<(u64, f64)> = meta
        .split(',')
        .filter_map(|p| {
            let (a, b) = p.split_once(':')?;
            Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
        })
        .collect();
    if parsed.len() == wb.sheets.len() && !parsed.is_empty() {
        for (sh, (id, pos)) in wb.sheets.iter_mut().zip(parsed) {
            sh.uid = id;
            sh.pos = if pos.is_finite() { pos } else { 0.0 };
        }
    } else {
        for (i, sh) in wb.sheets.iter_mut().enumerate() {
            sh.uid = i as u64 + 1;
            sh.pos = i as f64;
        }
    }
}

/// "id:pos,id:pos" for a snapshot.
pub fn sheets_meta(wb: &Workbook) -> String {
    wb.sheets.iter().map(|s| format!("{}:{}", s.uid, s.pos)).collect::<Vec<_>>().join(",")
}

// ---------------------------------------------------------------------------
// Applying operations
// ---------------------------------------------------------------------------

/// State kept between applications (the provisional rescue sheet).
#[derive(Default, Debug, Clone)]
pub struct ApplyState {
    /// A rescue sheet is shown because every sheet was deleted and the server's has not come.
    pub provisional: bool,
}

/// What applying operations touched.
#[derive(Default, Debug)]
pub struct Applied {
    /// Cells whose content changed: (sheet index, cell) for incremental recalculation.
    pub cells: Vec<(usize, Range)>,
    /// Anything beyond cell contents changed: recalculate everything.
    pub full: bool,
    /// The sheet list, order or names changed.
    pub sheets: bool,
    /// Feature lists replaced: (sheet id, key).
    pub features: Vec<(SheetId, String)>,
    /// Structural operations applied, in order.
    pub structs: Vec<Op>,
}

/// Applies operations in order. Operations on sheets that don't exist are ignored.
/// `remote`: committed changes from someone else (affects only the provisional rescue sheet).
pub fn apply_ops(wb: &mut Workbook, ops: &[Op], remote: bool, st: &mut ApplyState) -> Applied {
    let mut out = Applied::default();
    for op in ops {
        apply_op(wb, op, remote, st, &mut out);
    }
    out
}

fn apply_op(wb: &mut Workbook, op: &Op, remote: bool, st: &mut ApplyState, out: &mut Applied) {
    if op.is_structural() || matches!(op, Op::Place { .. }) {
        apply_struct(wb, op, remote, st);
        out.full = true;
        out.sheets = true;
        out.structs.push(op.clone());
        return;
    }
    let Some(i) = op.sheet().and_then(|s| index_of(wb, s)).or(if matches!(op, Op::Book { .. }) { Some(usize::MAX) } else { None }) else { return };
    match op {
        Op::Cell { s, r, c, v, adj } => {
            match v {
                None => {
                    wb.sheets[i].remove(*r, *c);
                    wb.sheets[i].rich.remove(&(*r, *c));
                }
                Some(d) => {
                    let cell = cell_from_data(wb, *s, d, adj);
                    let sh = &mut wb.sheets[i];
                    match d.rich.as_ref().and_then(|j| serde_json::from_value(j.clone()).ok()) {
                        Some(rt) => {
                            sh.rich.insert((*r, *c), rt);
                        }
                        None => {
                            sh.rich.remove(&(*r, *c));
                        }
                    }
                    sh.set(*r, *c, Some(cell));
                }
            }
            out.cells.push((i, Range::cell(*r, *c)));
        }
        Op::Note { r, c, text, author, .. } => {
            let sh = &mut wb.sheets[i];
            match text {
                Some(t) => {
                    sh.notes.insert((*r, *c), t.clone());
                }
                None => {
                    sh.notes.remove(&(*r, *c));
                }
            }
            match author {
                Some(a) => {
                    sh.note_authors.insert((*r, *c), a.clone());
                }
                None => {
                    sh.note_authors.remove(&(*r, *c));
                }
            }
        }
        Op::Link { r, c, url, .. } => {
            let sh = &mut wb.sheets[i];
            match url {
                Some(u) => sh.links.insert((*r, *c), u.clone()),
                None => sh.links.remove(&(*r, *c)),
            };
        }
        Op::RowH { r, h, .. } => {
            let sh = &mut wb.sheets[i];
            match h {
                Some(h) => sh.row_h.insert(*r, *h),
                None => sh.row_h.remove(r),
            };
            out.full = true;
        }
        Op::ColW { c, w, .. } => {
            let sh = &mut wb.sheets[i];
            match w {
                Some(w) => sh.col_w.insert(*c, *w),
                None => sh.col_w.remove(c),
            };
            out.full = true;
        }
        Op::RowHidden { r, on, .. } => {
            let sh = &mut wb.sheets[i];
            if *on { sh.hidden_rows.insert(*r) } else { sh.hidden_rows.remove(r) };
            out.full = true;
        }
        Op::ColHidden { c, on, .. } => {
            let sh = &mut wb.sheets[i];
            if *on { sh.hidden_cols.insert(*c) } else { sh.hidden_cols.remove(c) };
            out.full = true;
        }
        Op::RowLevel { r, l, .. } => {
            let sh = &mut wb.sheets[i];
            if *l == 0 { sh.row_levels.remove(r) } else { sh.row_levels.insert(*r, *l) };
        }
        Op::ColLevel { c, l, .. } => {
            let sh = &mut wb.sheets[i];
            if *l == 0 { sh.col_levels.remove(c) } else { sh.col_levels.insert(*c, *l) };
        }
        Op::Merge { rg, on, .. } => {
            let sh = &mut wb.sheets[i];
            let rg = Range::new(rg.r1, rg.c1, rg.r2, rg.c2);
            if *on {
                // Overlapping merges can't be saved; whichever comes first in the server's
                // order stays (everyone adopts that order's result).
                if !sh.merges.iter().any(|m| m.intersects(&rg)) {
                    sh.merges.push(rg);
                }
            } else {
                sh.merges.retain(|m| *m != rg);
            }
            out.full = true;
        }
        Op::Prop { key, val, .. } => {
            prop_set(&mut wb.sheets[i], key, val.clone());
            out.full = true;
        }
        Op::Feature { s, key, val, adj } => {
            let val = if adj.is_empty() { val.clone() } else { adjust_feature(wb, *s, key, val.clone(), adj) };
            if feature_set(&mut wb.sheets[i], key, val) && key == "pivots" {
                for pt in &mut wb.sheets[i].pivots {
                    pt.out = crate::pivot::engine::layout(pt);
                }
            }
            out.features.push((*s, key.clone()));
            out.full = true;
        }
        Op::Book { key, val, adj } => {
            if key == "names" {
                let val = if adj.is_empty() { val.clone() } else { adjust_book(wb, val.clone(), adj) };
                if let Ok(n) = serde_json::from_value(val) {
                    wb.names = n;
                }
            }
            out.full = true;
        }
        _ => {}
    }
}

/// Applies one structural or tab-order operation.
fn apply_struct(wb: &mut Workbook, op: &Op, remote: bool, st: &mut ApplyState) {
    match op {
        Op::Rows { s, at, n } | Op::Cols { s, at, n } => {
            if let Some(i) = index_of(wb, *s) {
                crate::structure::shift_structure(wb, i, matches!(op, Op::Rows { .. }), *at, *n as i64);
            }
        }
        Op::AddSheet { s, name, pos } => {
            if index_of(wb, *s).is_some() {
                if *s == RESCUE_SHEET {
                    st.provisional = false;
                }
                return;
            }
            if st.provisional && remote {
                // Someone added a sheet before our delete of the last one was committed: the
                // server won't create the rescue sheet.
                st.provisional = false;
                if let Some(i) = index_of(wb, RESCUE_SHEET) {
                    wb.sheets.remove(i);
                }
            }
            let mut sh = Sheet::new(name);
            sh.uid = *s;
            sh.pos = *pos;
            wb.sheets.push(sh);
            sort_sheets(wb);
        }
        Op::DelSheet { s, .. } => {
            let Some(i) = index_of(wb, *s) else { return };
            let active = wb.sheets.get(wb.active).map(|x| x.uid);
            crate::structure::delete_sheet(wb, i);
            if wb.sheets.is_empty() {
                let mut sh = Sheet::new("Sheet1");
                sh.uid = RESCUE_SHEET;
                wb.sheets.push(sh);
                st.provisional = true;
            }
            wb.active = active.and_then(|u| index_of(wb, u)).unwrap_or_else(|| i.min(wb.sheets.len() - 1));
        }
        Op::Rename { s, to, .. } => {
            if let Some(i) = index_of(wb, *s) {
                crate::structure::rename_sheet(wb, i, to);
            }
        }
        Op::Place { s, pos } => {
            if let Some(i) = index_of(wb, *s) {
                wb.sheets[i].pos = *pos;
                sort_sheets(wb);
            }
        }
        _ => {}
    }
}

fn cell_from_data(wb: &mut Workbook, host: SheetId, d: &CellData, adj: &[Op]) -> Cell {
    let style = match &d.st {
        Some(j) => match serde_json::from_value::<Style>(j.clone()) {
            Ok(s) => wb.intern_style(s),
            Err(_) => 0,
        },
        None => 0,
    };
    match &d.f {
        Some(f) => {
            let text = if adj.is_empty() { f.clone() } else { adjust_formula(wb, host, f, adj) };
            Cell { value: Value::Empty, formula: Some(Box::new(Formula::new(&text))), style }
        }
        None => Cell { value: value_of(&d.v), formula: None, style },
    }
}

// ---------------------------------------------------------------------------
// Adjusting content for structural changes it was transformed over
// ---------------------------------------------------------------------------

/// Sheet names (by id) as they were before `adj[from..]` was applied, given the current
/// workbook: renames and deletes later in `adj` are undone.
fn names_before(wb: &Workbook, adj: &[Op], from: usize) -> Vec<(SheetId, String)> {
    let mut names: Vec<(SheetId, String)> = wb.sheets.iter().map(|s| (s.uid, s.name.clone())).collect();
    for a in adj[from..].iter().rev() {
        match a {
            Op::Rename { s, from, .. } => match names.iter_mut().find(|(u, _)| u == s) {
                Some(e) => e.1 = from.clone(),
                None => names.push((*s, from.clone())),
            },
            Op::DelSheet { s, name } => {
                if !names.iter().any(|(u, _)| u == s) {
                    names.push((*s, name.clone()));
                }
            }
            _ => {}
        }
    }
    names
}

/// A formula written before the structural changes in `adj` happened elsewhere, adjusted as if
/// it had been in the workbook when they were applied (same functions as the commands use).
fn adjust_formula(wb: &Workbook, host: SheetId, text: &str, adj: &[Op]) -> String {
    let Ok(mut e) = formula::parse(text.trim_start_matches('=')) else { return text.to_string() };
    let orig = e.clone();
    for (i, a) in adj.iter().enumerate() {
        match a {
            Op::Rows { s, at, n } | Op::Cols { s, at, n } => {
                let names = names_before(wb, adj, i);
                let name = names.iter().find(|(u, _)| u == s).map(|(_, n)| n.clone()).unwrap_or_default();
                formula::shift_for_structure(&mut e, &name, host == *s, matches!(a, Op::Rows { .. }), *at, *n as i64);
            }
            Op::Rename { from, to, .. } => formula::rename_sheet(&mut e, from, to),
            Op::DelSheet { name, .. } => crate::structure::drop_sheet_refs(&mut e, name),
            _ => {}
        }
    }
    if e == orig { text.to_string() } else { formula::print(&e) }
}

/// A scratch workbook with empty sheets named as before `adj`, for adjusting a feature list or
/// the defined names with the same structural functions the commands use.
fn scratch(wb: &Workbook, adj: &[Op]) -> Workbook {
    let sheets = names_before(wb, adj, 0)
        .into_iter()
        .map(|(u, n)| {
            let mut s = Sheet::new(&n);
            s.uid = u;
            s
        })
        .collect();
    Workbook::empty_with(sheets)
}

fn adjust_feature(wb: &Workbook, s: SheetId, key: &str, val: Json, adj: &[Op]) -> Json {
    let mut sc = scratch(wb, adj);
    let Some(i) = index_of(&sc, s) else { return val };
    if !feature_set(&mut sc.sheets[i], key, val.clone()) {
        return val;
    }
    let mut st = ApplyState::default();
    for a in adj {
        apply_struct(&mut sc, a, false, &mut st);
    }
    match index_of(&sc, s) {
        Some(i) => feature_get(&sc.sheets[i], key).unwrap_or(val),
        None => val,
    }
}

fn adjust_book(wb: &Workbook, val: Json, adj: &[Op]) -> Json {
    let mut sc = scratch(wb, adj);
    let Ok(names) = serde_json::from_value(val.clone()) else { return val };
    sc.names = names;
    let mut st = ApplyState::default();
    for a in adj {
        apply_struct(&mut sc, a, false, &mut st);
    }
    serde_json::to_value(&sc.names).unwrap_or(val)
}

// ---------------------------------------------------------------------------
// Lossless live state (stored with snapshots for people joining later)
// ---------------------------------------------------------------------------

/// Everything that is synchronised, as operations that rebuild it on an empty workbook.
pub fn state_ops(wb: &Workbook) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut unused = Vec::new();
    for sh in &wb.sheets {
        ops.push(Op::AddSheet { s: sh.uid, name: sh.name.clone(), pos: sh.pos });
        let mut blank = Sheet::new(&sh.name);
        blank.uid = sh.uid;
        blank.pos = sh.pos;
        diff::sheet_pair(wb, &blank, wb, sh, &mut ops, &mut unused);
    }
    if !wb.names.is_empty() {
        ops.push(Op::Book { key: "names".into(), val: serde_json::to_value(&wb.names).unwrap_or_default(), adj: Vec::new() });
    }
    ops
}

/// The workbook `state_ops` describes.
pub fn from_state(ops: &[Op]) -> Workbook {
    let mut wb = Workbook::empty_with(Vec::new());
    apply_ops(&mut wb, ops, false, &mut ApplyState::default());
    if wb.sheets.is_empty() {
        wb.sheets.push(Sheet::new("Sheet1"));
    }
    wb.active = 0;
    wb
}

/// Compressed JSON of the state operations.
pub fn encode_state(ops: &[Op]) -> Result<Vec<u8>, String> {
    use std::io::Write;
    let json = serde_json::to_vec(ops).map_err(|e| e.to_string())?;
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(json.len() > u32::MAX as usize / 2);
    z.start_file("state.json", opts).map_err(|e| e.to_string())?;
    z.write_all(&json).map_err(|e| e.to_string())?;
    Ok(z.finish().map_err(|e| e.to_string())?.into_inner())
}

pub fn decode_state(data: &[u8]) -> Result<Vec<Op>, String> {
    use std::io::Read;
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| e.to_string())?;
    let mut f = z.by_name("state.json").map_err(|e| e.to_string())?;
    let mut json = Vec::new();
    f.read_to_end(&mut json).map_err(|e| e.to_string())?;
    serde_json::from_slice(&json).map_err(|e| e.to_string())
}

/// The workbook from a downloaded copy: the live state when there is one (lossless), with
/// what isn't synchronised (zoom, selection) taken from the saved file.
pub fn join_workbook(file: Workbook, state: Option<&[u8]>) -> Workbook {
    let Some(ops) = state.and_then(|d| decode_state(d).ok()) else { return file };
    let mut wb = from_state(&ops);
    for (i, sh) in wb.sheets.iter_mut().enumerate() {
        if let Some(f) = file.sheets.get(i) {
            // Per-person view settings, like Excel's co-authoring.
            sh.view = f.view.clone();
            sh.zoom = f.zoom;
            sh.gridlines = f.gridlines;
        }
    }
    wb.active = file.active.min(wb.sheets.len() - 1);
    wb.path = file.path;
    wb
}

// ---------------------------------------------------------------------------
// Comparing workbooks (verification and tests)
// ---------------------------------------------------------------------------

/// Differences between two workbooks in everything that is synchronised (empty when equal).
pub fn compare(a: &Workbook, b: &Workbook) -> Vec<String> {
    let mut out = Vec::new();
    let ids = |w: &Workbook| w.sheets.iter().map(|s| (s.uid, s.name.clone())).collect::<Vec<_>>();
    if ids(a) != ids(b) {
        out.push(format!("sheets {:?} vs {:?}", ids(a), ids(b)));
        return out;
    }
    if a.names != b.names {
        out.push(format!("names {:?} vs {:?}", a.names, b.names));
    }
    for (x, y) in a.sheets.iter().zip(&b.sheets) {
        let mut fwd = Vec::new();
        let mut inv = Vec::new();
        // Styles are compared by content, so each side resolves its own indices.
        diff::sheet_pair(a, x, b, y, &mut fwd, &mut inv);
        for op in fwd.into_iter().take(5) {
            out.push(format!("{}: {}", x.name, describe(&op)));
        }
    }
    out
}

/// A short description of an op (for test failures; never sent anywhere).
pub fn describe(op: &Op) -> String {
    let s = format!("{op:?}");
    if s.len() > 300 { format!("{}...", &s[..300]) } else { s }
}

/// Cell contents by position of every sheet (for convergence tests).
pub fn contents(wb: &Workbook) -> std::collections::HashMap<(String, u32, u32), String> {
    let mut out = std::collections::HashMap::new();
    for sh in &wb.sheets {
        for (r, row) in &sh.rows {
            for (c, cell) in row {
                let d = cell_data(wb, sh, *r, *c, cell);
                out.insert((sh.name.clone(), *r, *c), serde_json::to_string(&d).unwrap_or_default());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
