//! Transform pairs (TP1 on a small document model) and a randomized convergence test of the
//! whole client/server protocol.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::json;

use crate::client::Session;
use crate::server::{Commit, Log};
use crate::*;

// ---------------------------------------------------------------------------
// A tiny document model that applies operations the way the app does.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Sh {
    id: SheetId,
    pos: u64,
    name: String,
    cells: BTreeMap<(u32, u32), String>,
    formulas: BTreeMap<(u32, u32), String>,
    merges: BTreeSet<(u32, u32, u32, u32)>,
    row_h: BTreeMap<u32, u32>,
    col_w: BTreeMap<u32, u32>,
    hidden_rows: BTreeSet<u32>,
    notes: BTreeMap<(u32, u32), String>,
    feats: BTreeMap<String, serde_json::Value>,
    props: BTreeMap<String, serde_json::Value>,
}

impl Sh {
    fn new(id: SheetId, name: &str) -> Self {
        Sh {
            id,
            pos: 0,
            name: name.into(),
            cells: BTreeMap::new(),
            formulas: BTreeMap::new(),
            merges: BTreeSet::new(),
            row_h: BTreeMap::new(),
            col_w: BTreeMap::new(),
            hidden_rows: BTreeSet::new(),
            notes: BTreeMap::new(),
            feats: BTreeMap::new(),
            props: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Doc {
    sheets: Vec<Sh>,
    book: BTreeMap<String, serde_json::Value>,
    /// A provisional rescue sheet is shown (all sheets deleted, server not heard yet).
    provisional: bool,
}

fn rect_t(r: Rect) -> (u32, u32, u32, u32) {
    (r.r1, r.c1, r.r2, r.c2)
}

fn t_rect(t: (u32, u32, u32, u32)) -> Rect {
    Rect { r1: t.0, c1: t.1, r2: t.2, c2: t.3 }
}

impl Doc {
    fn new() -> Self {
        let mut b = Sh::new(2, "Sheet2");
        b.pos = 1f64.to_bits();
        Doc { sheets: vec![Sh::new(1, "Sheet1"), b], book: BTreeMap::new(), provisional: false }
    }

    fn sh(&mut self, s: SheetId) -> Option<&mut Sh> {
        self.sheets.iter_mut().find(|x| x.id == s)
    }

    fn shift(&mut self, s: SheetId, rows: bool, at: u32, n: i32) {
        let Some(sh) = self.sh(s) else { return };
        let max = if rows { MAX_ROWS } else { MAX_COLS };
        let mp = |k: (u32, u32)| -> Option<(u32, u32)> {
            if rows { map_index(k.0, at, n, max).map(|r| (r, k.1)) } else { map_index(k.1, at, n, max).map(|c| (k.0, c)) }
        };
        sh.cells = std::mem::take(&mut sh.cells).into_iter().filter_map(|(k, v)| mp(k).map(|k| (k, v))).collect();
        sh.formulas = std::mem::take(&mut sh.formulas).into_iter().filter_map(|(k, v)| mp(k).map(|k| (k, v))).collect();
        sh.notes = std::mem::take(&mut sh.notes).into_iter().filter_map(|(k, v)| mp(k).map(|k| (k, v))).collect();
        sh.merges = std::mem::take(&mut sh.merges).into_iter().filter_map(|m| fix_range(t_rect(m), rows, at, n)).filter(|m| !m.is_single()).map(rect_t).collect();
        if rows {
            sh.row_h = std::mem::take(&mut sh.row_h).into_iter().filter_map(|(r, h)| map_index(r, at, n, max).map(|r| (r, h))).collect();
            sh.hidden_rows = std::mem::take(&mut sh.hidden_rows).into_iter().filter_map(|r| map_index(r, at, n, max)).collect();
        } else {
            sh.col_w = std::mem::take(&mut sh.col_w).into_iter().filter_map(|(c, w)| map_index(c, at, n, max).map(|c| (c, w))).collect();
        }
    }

    fn apply(&mut self, op: &Op) {
        self.apply_from(op, true);
    }

    /// `remote`: a committed change from the server (as opposed to a local edit).
    fn apply_from(&mut self, op: &Op, remote: bool) {
        use Op::*;
        match op {
            Rows { s, at, n } => self.shift(*s, true, *at, *n),
            Cols { s, at, n } => self.shift(*s, false, *at, *n),
            AddSheet { s, name, pos } => {
                if *s == RESCUE_SHEET && self.sheets.iter().any(|x| x.id == RESCUE_SHEET) {
                    self.provisional = false;
                    return;
                }
                if self.provisional && remote {
                    // Someone added a sheet before our delete was committed: the server won't
                    // create the rescue sheet.
                    self.provisional = false;
                    self.sheets.retain(|x| x.id != RESCUE_SHEET);
                }
                let mut sh = Sh::new(*s, name);
                sh.pos = pos.to_bits();
                self.sheets.push(sh);
                self.sort();
            }
            Place { s, pos } => {
                if let Some(sh) = self.sh(*s) {
                    sh.pos = pos.to_bits();
                }
                self.sort();
            }
            DelSheet { s, .. } => {
                self.sheets.retain(|x| x.id != *s);
                if self.sheets.is_empty() {
                    self.sheets.push(Sh::new(RESCUE_SHEET, "Sheet1"));
                    self.provisional = true;
                }
            }
            Rename { s, to, .. } => {
                if let Some(sh) = self.sh(*s) {
                    sh.name = to.clone();
                }
            }
            Cell { s, r, c, v, .. } => {
                let Some(sh) = self.sh(*s) else { return };
                sh.cells.remove(&(*r, *c));
                sh.formulas.remove(&(*r, *c));
                if let Some(d) = v {
                    match (&d.f, &d.v) {
                        (Some(f), _) => {
                            sh.formulas.insert((*r, *c), f.clone());
                        }
                        (None, Val::Text(t)) => {
                            sh.cells.insert((*r, *c), t.clone());
                        }
                        _ => {}
                    }
                }
            }
            Note { s, r, c, text, .. } => {
                let Some(sh) = self.sh(*s) else { return };
                match text {
                    Some(t) => sh.notes.insert((*r, *c), t.clone()),
                    None => sh.notes.remove(&(*r, *c)),
                };
            }
            RowH { s, r, h } => {
                let Some(sh) = self.sh(*s) else { return };
                match h {
                    Some(h) => sh.row_h.insert(*r, h.to_bits()),
                    None => sh.row_h.remove(r),
                };
            }
            ColW { s, c, w } => {
                let Some(sh) = self.sh(*s) else { return };
                match w {
                    Some(w) => sh.col_w.insert(*c, w.to_bits()),
                    None => sh.col_w.remove(c),
                };
            }
            RowHidden { s, r, on } => {
                let Some(sh) = self.sh(*s) else { return };
                if *on {
                    sh.hidden_rows.insert(*r);
                } else {
                    sh.hidden_rows.remove(r);
                }
            }
            Merge { s, rg, on } => {
                let Some(sh) = self.sh(*s) else { return };
                if *on {
                    sh.merges.insert(rect_t(*rg));
                } else {
                    sh.merges.remove(&rect_t(*rg));
                }
            }
            Prop { s, key, val } => {
                if let Some(sh) = self.sh(*s) {
                    sh.props.insert(key.clone(), val.clone());
                }
            }
            Feature { s, key, val, .. } => {
                if let Some(sh) = self.sh(*s) {
                    sh.feats.insert(key.clone(), val.clone());
                }
            }
            Book { key, val, .. } => {
                self.book.insert(key.clone(), val.clone());
            }
            Link { .. } | ColHidden { .. } | RowLevel { .. } | ColLevel { .. } => {}
        }
    }

    fn sort(&mut self) {
        self.sheets.sort_by(|a, b| f64::from_bits(a.pos).total_cmp(&f64::from_bits(b.pos)).then(a.id.cmp(&b.id)));
    }

    fn apply_all(&mut self, ops: &[Op]) {
        for op in ops {
            self.apply(op);
        }
    }
}

fn text(s: SheetId, r: u32, c: u32, t: &str) -> Op {
    Op::Cell { s, r, c, v: Some(Box::new(CellData { v: Val::Text(t.into()), ..Default::default() })), adj: vec![] }
}

fn formula(s: SheetId, r: u32, c: u32, f: &str) -> Op {
    Op::Cell { s, r, c, v: Some(Box::new(CellData { f: Some(f.into()), ..Default::default() })), adj: vec![] }
}

fn rows(s: SheetId, at: u32, n: i32) -> Op {
    Op::Rows { s, at, n }
}

fn cols(s: SheetId, at: u32, n: i32) -> Op {
    Op::Cols { s, at, n }
}

/// Checks TP1 for `a` (later, winning) and `b` on `doc`; returns the converged document.
fn tp1(doc: &Doc, a: &[Op], b: &[Op]) -> Doc {
    let (a2, b2) = transform(a, b, true);
    let mut x = doc.clone();
    x.apply_all(b);
    x.apply_all(&a2);
    let mut y = doc.clone();
    y.apply_all(a);
    y.apply_all(&b2);
    assert_eq!(x, y, "TP1 failed for\n a={a:?}\n b={b:?}\n a'={a2:?}\n b'={b2:?}");
    // The mirror call agrees.
    let (b3, a3) = transform(b, a, false);
    assert_eq!((a3, b3), (a2, b2));
    x
}

fn base() -> Doc {
    let mut d = Doc::new();
    for r in 0..10 {
        d.apply(&text(1, r, 0, &format!("A{}", r + 1)));
        d.apply(&text(1, r, 1, &format!("B{}", r + 1)));
    }
    d
}

fn cell(d: &Doc, s: SheetId, r: u32, c: u32) -> Option<&str> {
    d.sheets.iter().find(|x| x.id == s)?.cells.get(&(r, c)).map(String::as_str)
}

// ---------------------------------------------------------------------------
// Pairs
// ---------------------------------------------------------------------------

#[test]
fn insert_rows_shifts_cells_below() {
    let d = tp1(&base(), &[text(1, 5, 0, "x")], &[rows(1, 2, 3)]);
    assert_eq!(cell(&d, 1, 8, 0), Some("x"));
    assert_eq!(cell(&d, 1, 1, 0), Some("A2"));
    let d = tp1(&base(), &[text(1, 1, 0, "above")], &[rows(1, 2, 3)]);
    assert_eq!(cell(&d, 1, 1, 0), Some("above"));
    // Exactly at the insert point: the cell moves down with its row.
    let d = tp1(&base(), &[text(1, 2, 0, "at")], &[rows(1, 2, 1)]);
    assert_eq!((cell(&d, 1, 3, 0), cell(&d, 1, 2, 0)), (Some("at"), None));
    // Other sheet: untouched.
    let d = tp1(&base(), &[text(2, 5, 0, "x")], &[rows(1, 0, 3)]);
    assert_eq!(cell(&d, 2, 5, 0), Some("x"));
}

#[test]
fn delete_rows_drops_cells_inside_and_shifts_below() {
    let d = tp1(&base(), &[text(1, 3, 0, "gone")], &[rows(1, 2, -3)]);
    assert!(!d.sheets[0].cells.values().any(|v| v == "gone"));
    let d = tp1(&base(), &[text(1, 7, 0, "x")], &[rows(1, 2, -3)]);
    assert_eq!(cell(&d, 1, 4, 0), Some("x"));
    // Both orders: the delete is the winner.
    let d = tp1(&base(), &[rows(1, 2, -3)], &[text(1, 3, 1, "gone")]);
    assert!(!d.sheets[0].cells.values().any(|v| v == "gone"));
}

#[test]
fn columns_like_rows() {
    let d = tp1(&base(), &[text(1, 0, 1, "x")], &[cols(1, 0, 2)]);
    assert_eq!(cell(&d, 1, 0, 3), Some("x"));
    let d = tp1(&base(), &[text(1, 0, 1, "x")], &[cols(1, 1, -1)]);
    assert!(!d.sheets[0].cells.values().any(|v| v == "x"));
    // Rows and columns commute.
    tp1(&base(), &[rows(1, 1, 2)], &[cols(1, 0, -1)]);
    tp1(&base(), &[cols(1, 1, 2), text(1, 4, 4, "z")], &[rows(1, 0, -2), text(1, 4, 4, "y")]);
}

#[test]
fn insert_vs_insert() {
    for (a, b) in [((2, 1), (2, 2)), ((1, 1), (5, 2)), ((5, 2), (1, 1))] {
        let d = tp1(&base(), &[rows(1, a.0, a.1), text(1, a.0, 3, "a")], &[rows(1, b.0, b.1), text(1, b.0, 4, "b")]);
        assert_eq!(d.sheets[0].cells.values().filter(|v| *v == "a" || *v == "b").count(), 2);
        assert_eq!(d.sheets[0].cells.len(), 22);
    }
    // Same place: the later (winning) insert goes below.
    let (a2, b2) = transform(&[rows(1, 2, 1)], &[rows(1, 2, 1)], true);
    assert_eq!((a2, b2), (vec![rows(1, 3, 1)], vec![rows(1, 2, 1)]));
}

#[test]
fn insert_vs_delete() {
    // Insert before, after and inside a deleted block.
    tp1(&base(), &[rows(1, 1, 2)], &[rows(1, 3, -2)]);
    tp1(&base(), &[rows(1, 6, 2)], &[rows(1, 3, -2)]);
    let d = tp1(&base(), &[rows(1, 4, 2), text(1, 4, 0, "new")], &[rows(1, 3, -3)]);
    // Rows inserted inside a block someone deleted are deleted with it.
    assert!(!d.sheets[0].cells.values().any(|v| v == "new"));
    assert_eq!(cell(&d, 1, 3, 0), Some("A7"));
    // Insert exactly at the start / end of the deleted block survives.
    let d = tp1(&base(), &[rows(1, 3, 1), text(1, 3, 0, "s")], &[rows(1, 3, -2)]);
    assert_eq!(cell(&d, 1, 3, 0), Some("s"));
    let d = tp1(&base(), &[rows(1, 5, 1), text(1, 5, 0, "e")], &[rows(1, 3, -2)]);
    assert_eq!(cell(&d, 1, 3, 0), Some("e"));
}

#[test]
fn delete_vs_delete() {
    for (a, b) in [((2, 3), (3, 3)), ((2, 2), (6, 2)), ((1, 6), (3, 2)), ((3, 2), (1, 6)), ((4, 2), (4, 2))] {
        let d = tp1(&base(), &[rows(1, a.0, -a.1)], &[rows(1, b.0, -b.1)]);
        let gone: BTreeSet<u32> = (a.0..a.0 + a.1 as u32).chain(b.0..b.0 + b.1 as u32).collect();
        assert_eq!(d.sheets[0].cells.len(), 20 - 2 * gone.len(), "{a:?} {b:?}");
    }
    assert_eq!(transform(&[rows(1, 4, -2)], &[rows(1, 4, -2)], true), (vec![], vec![]));
}

#[test]
fn merges_follow_rows_and_columns() {
    let m = |r1, c1, r2, c2, on| Op::Merge { s: 1, rg: Rect { r1, c1, r2, c2 }, on };
    // Inserting inside a merge grows it; before it moves it; deleting shrinks or removes it.
    let d = tp1(&base(), &[m(2, 0, 4, 1, true)], &[rows(1, 3, 2)]);
    assert!(d.sheets[0].merges.contains(&(2, 0, 6, 1)));
    let d = tp1(&base(), &[m(2, 0, 4, 1, true)], &[rows(1, 0, 1)]);
    assert!(d.sheets[0].merges.contains(&(3, 0, 5, 1)));
    let d = tp1(&base(), &[m(2, 0, 4, 1, true)], &[rows(1, 3, -1)]);
    assert!(d.sheets[0].merges.contains(&(2, 0, 3, 1)));
    let d = tp1(&base(), &[m(2, 0, 3, 0, true)], &[rows(1, 2, -1)]);
    assert!(d.sheets[0].merges.is_empty());
    let d = tp1(&base(), &[m(0, 0, 0, 3, true)], &[cols(1, 1, -2)]);
    assert!(d.sheets[0].merges.contains(&(0, 0, 0, 1)));
    // Unmerge of a merge that moved.
    let mut start = base();
    start.apply(&m(5, 0, 6, 1, true));
    let d = tp1(&start, &[m(5, 0, 6, 1, false)], &[rows(1, 0, 2)]);
    assert!(d.sheets[0].merges.is_empty());
    // Same range merged and unmerged at once: the later one wins.
    let d = tp1(&start, &[m(5, 0, 6, 1, false)], &[m(5, 0, 6, 1, true)]);
    assert!(d.sheets[0].merges.is_empty());
}

#[test]
fn row_and_column_sizes() {
    let h = |r, v: f32| Op::RowH { s: 1, r, h: Some(v) };
    let w = |c, v: f32| Op::ColW { s: 1, c, w: Some(v) };
    let d = tp1(&base(), &[h(4, 30.0), w(2, 99.0)], &[rows(1, 0, 2), cols(1, 3, 1)]);
    assert_eq!(d.sheets[0].row_h.get(&6), Some(&30.0f32.to_bits()));
    assert_eq!(d.sheets[0].col_w.get(&2), Some(&99.0f32.to_bits()));
    let d = tp1(&base(), &[h(4, 30.0), Op::RowHidden { s: 1, r: 5, on: true }], &[rows(1, 4, -2)]);
    assert!(d.sheets[0].row_h.is_empty() && d.sheets[0].hidden_rows.is_empty());
    let d = tp1(&base(), &[h(4, 30.0)], &[h(4, 40.0)]);
    assert_eq!(d.sheets[0].row_h.get(&4), Some(&30.0f32.to_bits()));
}

#[test]
fn same_cell_last_commit_wins() {
    let d = tp1(&base(), &[text(1, 0, 0, "mine")], &[text(1, 0, 0, "theirs")]);
    assert_eq!(cell(&d, 1, 0, 0), Some("mine"));
    let d = tp1(&base(), &[Op::Cell { s: 1, r: 0, c: 0, v: None, adj: vec![] }], &[text(1, 0, 0, "theirs")]);
    assert_eq!(cell(&d, 1, 0, 0), None);
    let note = |t: &str| Op::Note { s: 1, r: 1, c: 1, text: Some(t.into()), author: None };
    let d = tp1(&base(), &[note("a")], &[note("b")]);
    assert_eq!(d.sheets[0].notes.get(&(1, 1)).map(String::as_str), Some("a"));
    let p = |v| Op::Prop { s: 1, key: "tab_color".into(), val: json!(v) };
    let d = tp1(&base(), &[p(1)], &[p(2)]);
    assert_eq!(d.sheets[0].props["tab_color"], json!(1));
    // A batch: only the conflicting target loses.
    let d = tp1(&base(), &[text(1, 0, 0, "a1"), text(1, 0, 1, "b1")], &[text(1, 0, 1, "zz"), text(1, 0, 2, "c1")]);
    assert_eq!((cell(&d, 1, 0, 0), cell(&d, 1, 0, 1), cell(&d, 1, 0, 2)), (Some("a1"), Some("b1"), Some("c1")));
}

#[test]
fn formulas_and_features_record_adjustments() {
    let (a2, _) = transform(&[formula(1, 5, 2, "SUM(A1:A9)")], &[rows(1, 0, 1), Op::Rename { s: 2, from: "Sheet2".into(), to: "Data".into() }], true);
    match &a2[0] {
        Op::Cell { r, adj, .. } => assert_eq!((*r, adj.len()), (6, 2)),
        o => panic!("{o:?}"),
    }
    // Plain values need no adjustment.
    let (a2, _) = transform(&[text(1, 5, 2, "x")], &[rows(1, 0, 1)], true);
    assert!(matches!(&a2[0], Op::Cell { adj, .. } if adj.is_empty()));
    // Features and defined names adjust for structure anywhere (they may refer to any sheet).
    let f = Op::Feature { s: 1, key: "charts".into(), val: json!([1]), adj: vec![] };
    let b = Op::Book { key: "names".into(), val: json!([["x", "Sheet2!A1"]]), adj: vec![] };
    let (a2, _) = transform(&[f.clone(), b], &[cols(2, 0, 1), Op::DelSheet { s: 3, name: "Gone".into() }], true);
    assert!(a2.iter().all(|o| matches!(o, Op::Feature { adj, .. } | Op::Book { adj, .. } if adj.len() == 2)));
    tp1(&base(), &[formula(1, 5, 2, "A1"), f.clone()], &[rows(1, 0, 1), cols(1, 0, -1)]);
    // Feature lists: later replace wins.
    let g = Op::Feature { s: 1, key: "charts".into(), val: json!([2]), adj: vec![] };
    let d = tp1(&base(), &[g], &[f]);
    assert_eq!(d.sheets[0].feats["charts"], json!([2]));
}

#[test]
fn deleted_sheet_takes_its_changes() {
    let del = Op::DelSheet { s: 2, name: "Sheet2".into() };
    let d = tp1(&base(), &[text(2, 0, 0, "x"), rows(2, 0, 1), Op::Rename { s: 2, from: "Sheet2".into(), to: "Y".into() }], std::slice::from_ref(&del));
    assert_eq!(d.sheets.len(), 1);
    assert_eq!(transform(&[del.clone()], std::slice::from_ref(&del), true), (vec![], vec![]));
    // Deleting every sheet at once leaves the same rescue sheet everywhere.
    let d = tp1(&base(), &[Op::DelSheet { s: 1, name: "Sheet1".into() }], &[del]);
    assert_eq!(d.sheets.iter().map(|s| s.id).collect::<Vec<_>>(), [RESCUE_SHEET]);
}

#[test]
fn sheet_names_and_order() {
    let ren = |s, from: &str, to: &str| Op::Rename { s, from: from.into(), to: to.into() };
    // Same sheet renamed twice: the later name wins.
    let d = tp1(&base(), &[ren(1, "Sheet1", "A")], &[ren(1, "Sheet1", "B")]);
    assert_eq!(d.sheets[0].name, "A");
    // Two sheets given the same name: the later keeps it, the other becomes "name (2)".
    let d = tp1(&base(), &[ren(1, "Sheet1", "Data")], &[ren(2, "Sheet2", "data")]);
    assert_eq!((d.sheets[0].name.as_str(), d.sheets[1].name.as_str()), ("Data", "data (2)"));
    let add = |s, n: &str| Op::AddSheet { s, name: n.into(), pos: 5.0 };
    let d = tp1(&base(), &[add(10, "New")], &[add(11, "New")]);
    let names: Vec<&str> = d.sheets.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"New") && names.contains(&"New (2)"), "{names:?}");
    let d = tp1(&base(), &[add(10, "Data")], &[ren(2, "Sheet2", "Data")]);
    assert_eq!(d.sheets.iter().find(|s| s.id == 2).unwrap().name, "Data (2)");
    // Long names stay within 31 characters.
    let long = "A".repeat(31);
    let (_, b2) = transform(&[ren(1, "Sheet1", &long)], &[ren(2, "Sheet2", &long)], true);
    assert!(matches!(&b2[0], Op::Rename { to, .. } if to.chars().count() == 31));
    // Tab order: the later move of a sheet wins; equal keys order by id.
    let place = |s, pos| Op::Place { s, pos };
    let d = tp1(&base(), &[place(1, 2.0)], &[place(1, 0.5)]);
    assert_eq!(d.sheets[0].id, 2);
    let d = tp1(&base(), &[place(1, 2.0)], &[Op::AddSheet { s: 7, name: "S7".into(), pos: 0.5 }]);
    assert_eq!(d.sheets.iter().map(|s| s.id).collect::<Vec<_>>(), [7, 2, 1]);
    let d = tp1(&base(), &[add(9, "S9")], &[add(8, "S8")]);
    assert_eq!(d.sheets.iter().map(|s| s.id).collect::<Vec<_>>(), [1, 2, 8, 9]);
    tp1(&base(), &[place(2, -1.0)], &[Op::DelSheet { s: 2, name: "Sheet2".into() }]);
}

#[test]
fn validation_rejects_garbage() {
    assert!(validate(&[text(1, MAX_ROWS, 0, "x")]).is_err());
    assert!(validate(&[text(1, 0, MAX_COLS, "x")]).is_err());
    assert!(validate(&[rows(1, 0, 0)]).is_err());
    assert!(validate(&[Op::AddSheet { s: 1, name: "x".repeat(40), pos: 0.0 }]).is_err());
    assert!(validate(&[Op::Place { s: 1, pos: f64::INFINITY }]).is_err());
    assert!(validate(&[Op::Merge { s: 1, rg: Rect { r1: 3, c1: 0, r2: 1, c2: 0 }, on: true }]).is_err());
    assert!(validate(&[Op::RowH { s: 1, r: 0, h: Some(f32::NAN) }]).is_err());
    let bad_adj = Op::Cell { s: 1, r: 0, c: 0, v: None, adj: vec![text(1, 0, 0, "x")] };
    assert!(validate(&[bad_adj]).is_err());
    assert!(validate(&[text(1, 5, 5, "ok"), rows(1, 0, -3), Op::Cell { s: 1, r: 0, c: 0, v: None, adj: vec![rows(1, 0, 1)] }]).is_ok());
    // Wire format round trip.
    let msg = ClientMsg::Ops { base: 3, id: 9, ops: vec![formula(1, 2, 3, "A1+1"), rows(1, 4, -2)] };
    let s = serde_json::to_string(&msg).unwrap();
    assert_eq!(serde_json::from_str::<ClientMsg>(&s).unwrap(), msg);
    assert!(serde_json::from_str::<ClientMsg>(r#"{"t":"ops","base":1}"#).is_err());
}

#[test]
fn server_log_commits_dedups_and_prunes() {
    let mut log = Log::new(0);
    assert_eq!(log.commit(0, "a", 1, "u", vec![text(1, 3, 0, "a")]), Ok(Commit::New { seq: 1, ops: vec![text(1, 3, 0, "a")], extra: vec![] }));
    // B made its change against seq 0: transformed over A's.
    match log.commit(0, "b", 1, "u", vec![rows(1, 0, 1), text(1, 3, 0, "b")]).unwrap() {
        Commit::New { seq, ops, .. } => assert_eq!((seq, ops), (2, vec![rows(1, 0, 1), text(1, 3, 0, "b")])),
        c => panic!("{c:?}"),
    }
    assert_eq!(log.commit(1, "a", 1, "u", vec![]), Ok(Commit::Duplicate { seq: 1 }));
    assert_eq!(log.commit(9, "a", 2, "u", vec![]), Err(server::CommitError::Invalid));
    log.prune_to(1);
    assert_eq!(log.commit(0, "c", 1, "u", vec![]), Err(server::CommitError::TooOld));
    assert_eq!(log.since(1).unwrap().len(), 1);
    assert!(log.since(0).is_none());
    // The batch that deletes the last sheet gets a rescue sheet appended.
    let mut log = Log::new(0);
    log.set_sheets([1, 2]);
    log.commit(0, "a", 1, "u", vec![Op::DelSheet { s: 1, name: "Sheet1".into() }]).unwrap();
    match log.commit(0, "b", 1, "u", vec![Op::DelSheet { s: 2, name: "Sheet2".into() }]).unwrap() {
        Commit::New { extra, .. } => assert!(matches!(&extra[..], [(1, Op::AddSheet { s: RESCUE_SHEET, .. })])),
        c => panic!("{c:?}"),
    }
}

// ---------------------------------------------------------------------------
// Randomized convergence: clients editing concurrently against an in-memory server.
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

enum In {
    Ops { seq: u64, cid: String, id: u64, ops: Vec<Op> },
    Ack { id: u64, seq: u64, extra: Vec<(u32, Op)> },
    CaughtUp,
}

struct Sim {
    doc: Doc,
    /// The server's state as far as this client knows it (committed batches only).
    confirmed: Doc,
    sess: Session,
    out: VecDeque<client::Batch>,
    inbox: VecDeque<In>,
    online: bool,
    counter: u64,
}

const NAMES: [&str; 5] = ["Alpha", "Beta", "Gamma", "Delta", "Plan"];

fn random_batch(rng: &mut Rng, doc: &Doc, me: usize, counter: &mut u64) -> Vec<Op> {
    let sh = &doc.sheets[rng.below(doc.sheets.len() as u64) as usize];
    let s = sh.id;
    *counter += 1;
    let tag = format!("c{me}-{counter}");
    let r = rng.below(12) as u32;
    let c = rng.below(6) as u32;
    match rng.below(17) {
        0..=4 => vec![text(s, r, c, &tag)],
        5 => vec![formula(s, r, c, &format!("A{}", r + 1))],
        6 => {
            let n = 1 + rng.below(3) as i32;
            if rng.chance(50) { vec![rows(s, r, n), text(s, r, c, &tag)] } else { vec![rows(s, r, n)] }
        }
        7 => vec![rows(s, r, -(1 + rng.below(3) as i32))],
        8 => {
            let n = 1 + rng.below(2) as i32;
            if rng.chance(50) { vec![cols(s, c, n), text(s, r, c, &tag)] } else { vec![cols(s, c, -n)] }
        }
        9 => {
            let rg = Rect { r1: r, c1: c, r2: r + 1 + rng.below(2) as u32, c2: c + rng.below(2) as u32 };
            match sh.merges.iter().next() {
                Some(m) if rng.chance(40) => vec![Op::Merge { s, rg: t_rect(*m), on: false }],
                _ => vec![Op::Merge { s, rg, on: true }],
            }
        }
        10 => vec![Op::RowH { s, r, h: Some(20.0 + rng.below(30) as f32) }, Op::RowHidden { s, r: r + 1, on: rng.chance(50) }],
        11 => {
            let id = rng.next() | 1 << 40;
            let name = format!("S{me}x{counter}");
            let pos = rng.below(8) as f64 / 2.0;
            vec![Op::AddSheet { s: id, name, pos }, text(id, 0, 0, &tag)]
        }
        12 if doc.sheets.len() > 1 => vec![Op::DelSheet { s, name: sh.name.clone() }],
        13 => {
            let to = NAMES[rng.below(NAMES.len() as u64) as usize];
            if doc.sheets.iter().any(|x| x.name.eq_ignore_ascii_case(to)) {
                vec![]
            } else {
                vec![Op::Rename { s, from: sh.name.clone(), to: to.into() }]
            }
        }
        14 => {
            vec![Op::Place { s, pos: rng.below(8) as f64 / 2.0 }]
        }
        15 => vec![Op::Feature { s, key: "charts".into(), val: json!(tag), adj: vec![] }, Op::Book { key: "names".into(), val: json!(tag), adj: vec![] }],
        _ => vec![Op::Note { s, r, c, text: Some(tag), author: None }, Op::Cell { s, r: r + 2, c, v: None, adj: vec![] }],
    }
}

fn doc_diff(a: &Doc, b: &Doc) -> String {
    let mut out = String::new();
    if a.book != b.book {
        out += &format!("book {:?} vs {:?}
", a.book, b.book);
    }
    let ids = |d: &Doc| d.sheets.iter().map(|s| (s.id, s.name.clone())).collect::<Vec<_>>();
    if ids(a) != ids(b) {
        out += &format!("sheets {:?}
    vs {:?}
", ids(a), ids(b));
    }
    for x in &a.sheets {
        if let Some(y) = b.sheets.iter().find(|y| y.id == x.id) {
            if x != y {
                out += &format!("sheet {}:
 {x:?}
 {y:?}
", x.name);
            }
        }
    }
    out
}

fn run_sim(seed: u64, clients: usize, steps: usize) {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let initial = base();
    let mut log = Log::new(0);
    log.set_sheets([1, 2]);
    let mut sims: Vec<Sim> = (0..clients)
        .map(|i| Sim { doc: initial.clone(), confirmed: initial.clone(), sess: Session::new(format!("client{i}"), 0), out: VecDeque::new(), inbox: VecDeque::new(), online: true, counter: 0 })
        .collect();
    let deliver_to_server = |log: &mut Log, sims: &mut Vec<Sim>, i: usize| {
        let Some(b) = sims[i].out.pop_front() else { return };
        let cid = sims[i].sess.cid.clone();
        if std::env::var("COSYNC_TRACE").is_ok() {
            eprintln!("server receives from {cid} base {}: {:?}", b.base, b.ops);
        }
        match log.commit(b.base, &cid, b.id, "u", b.ops).expect("commit") {
            Commit::New { seq, ops, extra } => {
                if std::env::var("COSYNC_TRACE").is_ok() {
                    eprintln!("server commits seq {seq} from {cid}: {ops:?}");
                }
                for (j, s) in sims.iter_mut().enumerate() {
                    if !s.online {
                        continue;
                    }
                    if j == i {
                        s.inbox.push_back(In::Ack { id: b.id, seq, extra: extra.clone() });
                    } else {
                        s.inbox.push_back(In::Ops { seq, cid: cid.clone(), id: b.id, ops: ops.clone() });
                    }
                }
            }
            Commit::Duplicate { seq } => sims[i].inbox.push_back(In::Ack { id: b.id, seq, extra: vec![] }),
        }
    };
    let trace = std::env::var("COSYNC_TRACE").is_ok();
    let handle = |s: &mut Sim, m: In| match m {
        In::Ack { id, seq, extra } => {
            if trace {
                eprintln!("{} ack {id} seq {seq} extra {extra:?}", s.sess.cid);
            }
            let st = s.sess.ack(id, seq, extra);
            s.confirmed.apply_all(&st.committed);
            s.doc.apply_all(&st.apply);
            if let Some(b) = st.send {
                s.out.push_back(b);
            }
        }
        In::Ops { seq, cid, id, ops } => {
            assert_eq!(seq, s.sess.seq + 1, "gap");
            let st = s.sess.remote(seq, &cid, id, ops);
            if trace {
                eprintln!("{} applies seq {seq} from {cid}: {:?}", s.sess.cid, st.apply);
            }
            s.confirmed.apply_all(&st.committed);
            s.doc.apply_all(&st.apply);
            if trace {
                eprintln!("  {} merges {:?}", s.sess.cid, s.doc.sheets.iter().map(|s| (s.id, s.merges.clone())).filter(|x| !x.1.is_empty()).collect::<Vec<_>>());
            }
            if let Some(b) = st.send {
                s.out.push_back(b);
            }
        }
        In::CaughtUp => {
            if let Some(b) = s.sess.resume() {
                s.out.push_back(b);
            }
        }
    };
    // With nothing pending, the visible state must equal the confirmed one. Point-like
    // content (cells, notes, sizes, sheets) is transformed exactly; ranges (merges) can end
    // differently at insert/delete boundaries, so the app then adopts the confirmed state.
    let settle = |s: &mut Sim| {
        if s.sess.has_pending() {
            return;
        }
        let strip = |d: &Doc| {
            let mut d = d.clone();
            d.sheets.iter_mut().for_each(|x| x.merges.clear());
            d.provisional = false;
            d
        };
        assert!(strip(&s.doc) == strip(&s.confirmed), "{} differs from its confirmed state (seed {seed}):\n{}", s.sess.cid, doc_diff(&s.doc, &s.confirmed));
        s.doc = s.confirmed.clone();
    };
    for _ in 0..steps {
        let i = rng.below(clients as u64) as usize;
        match rng.below(10) {
            0..=3 => {
                let s = &mut sims[i];
                let ops = random_batch(&mut rng, &s.doc, i, &mut s.counter);
                if ops.is_empty() {
                    continue;
                }
                if trace {
                    eprintln!("client{i} local {ops:?}");
                }
                for op in &ops {
                    s.doc.apply_from(op, false);
                }
                if let Some(b) = s.sess.local(ops) {
                    s.out.push_back(b);
                }
            }
            4..=6 => {
                if sims[i].online {
                    deliver_to_server(&mut log, &mut sims, i);
                }
            }
            7 | 8 => {
                if let Some(m) = sims[i].inbox.pop_front() {
                    handle(&mut sims[i], m);
                    settle(&mut sims[i]);
                }
            }
            _ => {
                if sims[i].online && rng.chance(30) {
                    // Connection lost: messages in transit to it are gone; its outgoing batch
                    // may or may not have reached the server.
                    if rng.chance(50) {
                        deliver_to_server(&mut log, &mut sims, i);
                    }
                    let s = &mut sims[i];
                    s.out.clear();
                    s.inbox.clear();
                    s.online = false;
                    s.sess.offline();
                } else if !sims[i].online {
                    let s = &mut sims[i];
                    s.online = true;
                    for e in log.since(s.sess.seq).unwrap() {
                        s.inbox.push_back(In::Ops { seq: e.seq, cid: e.cid.clone(), id: e.id, ops: e.ops.clone() });
                    }
                    s.inbox.push_back(In::CaughtUp);
                }
            }
        }
    }
    // Reconnect everyone and drain.
    for s in sims.iter_mut().filter(|s| !s.online) {
        s.online = true;
        for e in log.since(s.sess.seq).unwrap() {
            s.inbox.push_back(In::Ops { seq: e.seq, cid: e.cid.clone(), id: e.id, ops: e.ops.clone() });
        }
        s.inbox.push_back(In::CaughtUp);
    }
    loop {
        let mut progress = false;
        for i in 0..clients {
            while let Some(m) = sims[i].inbox.pop_front() {
                handle(&mut sims[i], m);
                settle(&mut sims[i]);
                progress = true;
            }
            if !sims[i].out.is_empty() {
                deliver_to_server(&mut log, &mut sims, i);
                progress = true;
            }
        }
        if !progress {
            break;
        }
    }
    let mut replay = initial.clone();
    for e in &log.entries {
        replay.apply_all(&e.ops);
        if trace {
            eprintln!("replay seq {} merges {:?}", e.seq, replay.sheets.iter().map(|s| (s.id, s.merges.clone())).filter(|x| !x.1.is_empty()).collect::<Vec<_>>());
        }
    }
    for (i, s) in sims.iter().enumerate() {
        assert!(!s.sess.has_pending(), "client {i} still has pending changes (seed {seed})");
        assert!(!s.doc.provisional, "client {i} still shows a provisional sheet (seed {seed})");
        assert!(s.doc == replay, "client {i} diverged from the server (seed {seed}):
{}", doc_diff(&s.doc, &replay));
    }
}

#[test]
fn random_concurrent_edits_converge() {
    if let Ok(v) = std::env::var("COSYNC_SEED") {
        let mut it = v.split(',').map(|x| x.parse::<u64>().unwrap());
        let (seed, n, steps) = (it.next().unwrap(), it.next().unwrap_or(3), it.next().unwrap_or(250));
        run_sim(seed, n as usize, steps as usize);
        return;
    }
    for seed in 1..=400 {
        run_sim(seed, 3, 250);
    }
    for seed in 1000..1040 {
        run_sim(seed, 5, 600);
    }
}

/// A batch committed whose ack was lost, followed by pruning past the client's base: the
/// server tells the client (an ack of its last committed batch before Reload), so the
/// reload doesn't send it again as a new batch.
#[test]
fn reload_after_a_lost_ack_does_not_recommit() {
    let mut log = Log::new(0);
    let mut a = Session::new("a", 0);
    let b = a.local(vec![rows(1, 0, 1)]).unwrap();
    assert!(matches!(log.commit(b.base, "a", b.id, "u", b.ops.clone()).unwrap(), Commit::New { .. }));
    a.offline(); // ack lost
    a.local(vec![text(1, 5, 0, "later")]);
    // Others keep editing; two snapshots prune past a's base.
    for i in 0..3 {
        log.commit(log.head(), "o", i + 1, "u", vec![text(1, 20, 0, "o")]).unwrap();
    }
    log.prune_to(2);
    assert!(log.since(a.seq).is_none(), "the server answers Reload");
    // Even resent as it was, the pruned batch is recognised.
    assert_eq!(log.commit(b.base, "a", b.id, "u", b.ops.clone()), Ok(Commit::Duplicate { seq: 1 }));
    // What the server sends before Reload.
    let (id, seq) = log.last_commit("a").unwrap();
    a.ack(id, seq, Vec::new());
    let reapply = a.rebase_on_reload(log.head());
    assert_eq!(reapply, vec![text(1, 5, 0, "later")], "only what wasn't committed is re-applied");
    let b2 = a.resume().unwrap();
    assert!(!b2.ops.iter().any(|o| matches!(o, Op::Rows { .. })), "re-sent: {:?}", b2.ops);
    assert!(matches!(log.commit(b2.base, "a", b2.id, "u", b2.ops).unwrap(), Commit::New { seq: 5, .. }));
}
