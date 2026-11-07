//! Web Sheets, structure / selection / formatting through the bridge: a browser session next to
//! a headless `fsheets` client (and a late joiner, who builds the state from the server's log),
//! which must all end up with the same workbook.

#[path = "common/webh.rs"]
mod webh;

use std::time::Duration;

use fsheets::live::Live;
use fsheets::work;
use serde_json::{Value, json};
use webh::*;

struct Env {
    _s: Srv,
    apis: Vec<fsheets::api::Api>,
    item: String,
    br: Browser,
    bob: Live,
}

fn env() -> Env {
    let (s, apis, item) = setup();
    let mut br = Browser::open(&s, &apis[0], &item);
    br.wait_for("hello", |v| v["t"] == "hello");
    let mut bob = Live::open(&apis[1], &item, true).unwrap();
    assert!(bob.wait_synced(Duration::from_secs(10)));
    Env { _s: s, apis, item, br, bob }
}

impl Env {
    /// The browser sends; everything settles.
    fn send(&mut self, v: Value) {
        self.br.seen.clear();
        self.br.send(v);
        self.settle();
    }

    fn settle(&mut self) {
        pump_all(&mut self.br, &mut self.bob, 250);
        let end = std::time::Instant::now() + Duration::from_secs(15);
        while self.br.last_status().as_deref() != Some("saved") || !self.bob.synced() {
            pump_all(&mut self.br, &mut self.bob, 60);
            assert!(std::time::Instant::now() < end, "never settled");
        }
        pump_all(&mut self.br, &mut self.bob, 250);
    }

    /// The browser's rejection message for a request.
    fn rejected(&mut self, v: Value) -> String {
        self.br.seen.clear();
        self.br.send(v);
        let m = self.br.wait_for("rejection", |v| v["t"] == "rejected");
        m["message"].as_str().unwrap().to_string()
    }

    fn edit(&mut self, r: u64, c: u64, t: &str) {
        self.send(json!({ "t": "edit", "sheet": 0, "r": r, "c": c, "text": t }));
    }

    fn bob_text(&mut self, sheet: usize, r: u32, c: u32) -> String {
        work::cell_text(self.bob.workbook(), sheet, r, c)
    }

    fn names(&mut self) -> Vec<String> {
        self.bob.workbook().sheets.iter().map(|s| s.name.clone()).collect()
    }

    /// A joiner who reads the state from the server agrees with the headless client, and the
    /// browser's windows show the same text.
    fn converged(&mut self) {
        pump_all(&mut self.br, &mut self.bob, 300);
        let mut late = Live::open(&self.apis[1], &self.item, true).unwrap();
        assert!(late.wait_synced(Duration::from_secs(10)));
        late.pump(Duration::from_millis(200));
        assert!(late.differences(&self.bob).is_empty(), "{:?}", late.differences(&self.bob));
        let n = self.bob.workbook().sheets.len();
        for sh in 0..n {
            let w = self.br.window(sh as u64, 30, 10);
            for r in 0..31u32 {
                for c in 0..11u32 {
                    let mine = w.get(&(u64::from(r), u64::from(c))).cloned().unwrap_or_default();
                    assert_eq!(mine, work::cell_text(self.bob.workbook(), sh, r, c), "sheet {sh} cell {r},{c}");
                }
            }
        }
    }
}

#[test]
fn rows_and_columns_insert_delete_and_undo() {
    let mut e = env();
    for (r, t) in ["a", "b", "c", "d"].iter().enumerate() {
        e.edit(r as u64, 0, t);
    }
    e.edit(0, 1, "=A3");
    e.edit(4, 0, "=COUNTA(A1:A4)");
    // Insert two rows above "c": formulas follow their cells; the range grows.
    e.send(json!({ "t": "ins", "sheet": 0, "axis": "r", "at": 2, "n": 2 }));
    assert_eq!(e.bob_text(0, 4, 0), "c");
    assert_eq!(e.bob_text(0, 0, 1), "c", "B1 =A3 now points at the moved cell");
    assert_eq!(e.bob_text(0, 6, 0), "4");
    assert_eq!(e.br.text(4, 0), "c");
    // Delete the inserted rows again, then the first row.
    e.send(json!({ "t": "del", "sheet": 0, "axis": "r", "at": 2, "n": 2 }));
    assert_eq!(e.bob_text(0, 2, 0), "c");
    e.send(json!({ "t": "del", "sheet": 0, "axis": "r", "at": 0, "n": 1 }));
    assert_eq!(e.bob_text(0, 0, 0), "b");
    assert_eq!(e.bob_text(0, 0, 1), "", "the formula cell went with its row");
    // Undo brings the row and its content (the formula too) back.
    e.send(json!({ "t": "undo" }));
    assert_eq!((e.bob_text(0, 0, 0), e.bob_text(0, 1, 0)), ("a".into(), "b".into()));
    assert_eq!(e.bob_text(0, 0, 1), "c");
    // Columns.
    e.send(json!({ "t": "ins", "sheet": 0, "axis": "c", "at": 0, "n": 1 }));
    assert_eq!(e.bob_text(0, 0, 1), "a");
    assert_eq!(e.bob_text(0, 0, 2), "c");
    e.send(json!({ "t": "del", "sheet": 0, "axis": "c", "at": 0, "n": 1 }));
    assert_eq!(e.bob_text(0, 0, 0), "a");
    e.send(json!({ "t": "undo" }));
    assert_eq!(e.bob_text(0, 0, 1), "a");
    e.send(json!({ "t": "redo" }));
    assert_eq!(e.bob_text(0, 0, 0), "a");
    // Limits.
    assert!(e.rejected(json!({ "t": "ins", "sheet": 0, "axis": "r", "at": 0, "n": 5000 })).contains("at most"));
    assert!(e.rejected(json!({ "t": "del", "sheet": 0, "axis": "x", "at": 0, "n": 1 })).contains("Invalid"));
    e.converged();
}

#[test]
fn sheets_add_rename_move_hide_delete() {
    let mut e = env();
    e.edit(0, 0, "one");
    e.send(json!({ "t": "sheet_add" }));
    let f = e.br.wait_for("focus", |v| v["t"] == "focus");
    assert!(f["uid"].as_str().is_some());
    assert_eq!(e.bob.workbook().sheets.len(), 2);
    e.send(json!({ "t": "sheet_add", "name": "Data" }));
    assert!(e.rejected(json!({ "t": "sheet_add", "name": "data" })).contains("already"));
    assert!(e.rejected(json!({ "t": "sheet_rename", "sheet": 0, "name": "a/b" })).contains("can't contain"));
    assert!(e.rejected(json!({ "t": "sheet_rename", "sheet": 0, "name": "x".repeat(40) })).contains("31"));
    e.send(json!({ "t": "sheet_rename", "sheet": 0, "name": "Main" }));
    assert_eq!(e.names(), ["Main", "Sheet2", "Data"]);
    // A formula on another sheet follows the rename.
    e.send(json!({ "t": "edit", "sheet": 2, "r": 0, "c": 0, "text": "=Main!A1" }));
    e.send(json!({ "t": "sheet_rename", "sheet": 0, "name": "Home" }));
    assert_eq!(e.bob_text(2, 0, 0), "one");
    // Move "Data" to the front, then back with undo.
    e.send(json!({ "t": "sheet_move", "sheet": 2, "to": 0 }));
    assert_eq!(e.names(), ["Data", "Home", "Sheet2"]);
    e.send(json!({ "t": "undo" }));
    assert_eq!(e.names(), ["Home", "Sheet2", "Data"]);
    e.send(json!({ "t": "sheet_move", "sheet": 0, "to": 2 }));
    assert_eq!(e.names(), ["Sheet2", "Data", "Home"]);
    // Hide and unhide; the last visible sheet can't be hidden.
    e.send(json!({ "t": "sheet_hide", "sheet": 0, "on": true }));
    assert!(e.bob.workbook().sheets[0].hidden);
    e.send(json!({ "t": "sheet_hide", "sheet": 0, "on": false }));
    assert!(!e.bob.workbook().sheets[0].hidden);
    e.send(json!({ "t": "sheet_color", "sheet": 1, "color": "#ff0000" }));
    assert_eq!(e.bob.workbook().sheets[1].tab_color, Some(0xff0000));
    // Delete a sheet, and undo brings it back with its content.
    e.send(json!({ "t": "sheet_del", "sheet": 2 }));
    assert_eq!(e.names(), ["Sheet2", "Data"]);
    e.send(json!({ "t": "undo" }));
    assert_eq!(e.names(), ["Sheet2", "Data", "Home"]);
    assert_eq!(e.bob_text(2, 0, 0), "one");
    e.send(json!({ "t": "sheet_del", "sheet": 0 }));
    e.send(json!({ "t": "sheet_del", "sheet": 0 }));
    assert!(e.rejected(json!({ "t": "sheet_del", "sheet": 0 })).contains("at least one"));
    e.converged();
}

#[test]
fn sizes_hiding_freeze_and_merge() {
    let mut e = env();
    e.edit(0, 0, "x");
    e.edit(0, 1, "y");
    e.send(json!({ "t": "size", "sheet": 0, "axis": "c", "a": 1, "b": 2, "px": 150 }));
    assert_eq!(e.bob.workbook().sheets[0].col_w.get(&2), Some(&150.0));
    e.send(json!({ "t": "size", "sheet": 0, "axis": "r", "a": 3, "b": 3, "px": 60 }));
    assert_eq!(e.bob.workbook().sheets[0].row_h.get(&3), Some(&60.0));
    e.send(json!({ "t": "undo" }));
    assert!(e.bob.workbook().sheets[0].row_h.get(&3).is_none());
    e.send(json!({ "t": "hide", "sheet": 0, "axis": "c", "a": 3, "b": 4, "on": true }));
    assert!(e.bob.workbook().sheets[0].hidden_cols.contains(&4));
    e.send(json!({ "t": "hide", "sheet": 0, "axis": "c", "a": 3, "b": 4, "on": false }));
    assert!(e.bob.workbook().sheets[0].hidden_cols.is_empty());
    e.send(json!({ "t": "hide", "sheet": 0, "axis": "r", "a": 5, "b": 5, "on": true }));
    assert!(e.bob.workbook().sheets[0].hidden_rows.contains(&5));
    e.send(json!({ "t": "freeze", "sheet": 0, "r": 1, "c": 1 }));
    assert_eq!(e.bob.workbook().sheets[0].freeze, (1, 1));
    e.send(json!({ "t": "freeze", "sheet": 0, "r": 0, "c": 0 }));
    assert_eq!(e.bob.workbook().sheets[0].freeze, (0, 0));
    // Merge keeps the upper-left value; undo restores the others.
    e.send(json!({ "t": "merge", "sheet": 0, "r1": 0, "c1": 0, "r2": 1, "c2": 1, "on": true }));
    assert_eq!(e.bob.workbook().sheets[0].merges.len(), 1);
    assert_eq!(e.bob_text(0, 0, 1), "");
    e.send(json!({ "t": "undo" }));
    assert!(e.bob.workbook().sheets[0].merges.is_empty());
    assert_eq!(e.bob_text(0, 0, 1), "y");
    e.send(json!({ "t": "merge", "sheet": 0, "r1": 0, "c1": 0, "r2": 1, "c2": 1, "on": true }));
    e.send(json!({ "t": "merge", "sheet": 0, "r1": 0, "c1": 0, "r2": 0, "c2": 0, "on": false }));
    assert!(e.bob.workbook().sheets[0].merges.is_empty());
    assert!(e.rejected(json!({ "t": "merge", "sheet": 0, "r1": 0, "c1": 0, "r2": 0, "c2": 0, "on": true })).contains("more than one"));
    e.converged();
}

fn clip_of(e: &mut Env, v: Value) -> Value {
    e.br.seen.clear();
    e.br.send(v);
    e.br.wait_for("clip", |v| v["t"] == "clip")
}

#[test]
fn copy_cut_paste_fill() {
    let mut e = env();
    e.edit(0, 0, "2");
    e.edit(1, 0, "5");
    e.edit(0, 1, "=A1*10");
    e.send(json!({ "t": "fmt", "sheet": 0, "r1": 0, "c1": 1, "r2": 0, "c2": 1, "set": { "b": true, "fill": "#ffee99" } }));
    // Copy a cell with a formula and paste it one row down: the relative reference moves, the format comes along.
    let c = clip_of(&mut e, json!({ "t": "copy", "sheet": 0, "r1": 0, "c1": 1, "r2": 0, "c2": 1 }));
    assert_eq!(c["text"], "20\n");
    let id = c["id"].as_u64().unwrap();
    e.send(json!({ "t": "paste", "sheet": 0, "r": 1, "c": 1, "clip": id }));
    assert_eq!(e.bob_text(0, 1, 1), "50");
    {
        let wb = e.bob.workbook();
        assert!(wb.style(wb.sheets[0].get(1, 1).unwrap().style).bold);
    }
    // The same copy pastes again, and tiles into a larger selection.
    e.send(json!({ "t": "paste", "sheet": 0, "r": 3, "c": 1, "r2": 5, "c2": 1, "clip": id }));
    assert_eq!((e.bob_text(0, 3, 1), e.bob_text(0, 5, 1)), ("0".into(), "0".into()));
    // Values only.
    let c = clip_of(&mut e, json!({ "t": "copy", "sheet": 0, "r1": 0, "c1": 0, "r2": 1, "c2": 1 }));
    assert_eq!(c["text"], "2\t20\n5\t50\n");
    e.send(json!({ "t": "paste", "sheet": 0, "r": 8, "c": 0, "clip": c["id"], "what": "values" }));
    assert_eq!(e.bob_text(0, 9, 1), "50");
    assert!(e.bob.workbook().sheets[0].get(9, 1).unwrap().formula.is_none());
    // Undo of a paste puts back what was there.
    e.send(json!({ "t": "undo" }));
    assert_eq!(e.bob_text(0, 9, 1), "");
    // Cut and paste moves the cells; the source is empty.
    let c = clip_of(&mut e, json!({ "t": "copy", "sheet": 0, "r1": 0, "c1": 0, "r2": 1, "c2": 0, "cut": true }));
    e.send(json!({ "t": "paste", "sheet": 0, "r": 0, "c": 3, "clip": c["id"] }));
    assert_eq!((e.bob_text(0, 0, 3), e.bob_text(0, 1, 3), e.bob_text(0, 0, 0)), ("2".into(), "5".into(), "".into()));
    assert!(e.rejected(json!({ "t": "paste", "sheet": 0, "r": 0, "c": 6, "clip": c["id"] })).contains("no longer available"), "a cut can be pasted once");
    // Text from the system clipboard: typed like the keyboard does; formula-looking junk stays text.
    e.send(json!({ "t": "paste", "sheet": 0, "r": 12, "c": 0, "grid": [["x", "1,5", "=1+2"], ["12%", "=SUM(", "a\u{202e}b"]] }));
    assert_eq!((e.bob_text(0, 12, 0), e.bob_text(0, 12, 2), e.bob_text(0, 13, 0)), ("x".into(), "3".into(), "12%".into()));
    assert_eq!(e.bob_text(0, 13, 1), "=SUM(");
    assert_eq!(e.bob_text(0, 13, 2), "ab", "direction overrides are removed");
    // Guards.
    let big: Vec<Vec<String>> = (0..300).map(|_| (0..100).map(|_| "1".to_string()).collect()).collect();
    assert!(e.rejected(json!({ "t": "paste", "sheet": 0, "r": 0, "c": 0, "grid": big })).contains("too much"));
    assert!(e.rejected(json!({ "t": "paste", "sheet": 0, "r": 1_048_570, "c": 0, "grid": [["1"],["1"],["1"],["1"],["1"],["1"],["1"],["1"],["1"]] })).contains("fit"));
    // Fill down / right and the fill handle.
    e.edit(20, 0, "1");
    e.edit(20, 1, "=A21+1");
    e.send(json!({ "t": "fill", "sheet": 0, "r1": 20, "c1": 0, "r2": 23, "c2": 1, "dir": "down" }));
    assert_eq!((e.bob_text(0, 23, 0), e.bob_text(0, 23, 1)), ("1".into(), "2".into()));
    e.send(json!({ "t": "fill", "sheet": 0, "r1": 20, "c1": 1, "r2": 20, "c2": 4, "dir": "right" }));
    assert_eq!(e.bob_text(0, 20, 4), "5", "B21 is =A21+1; filled right, each cell adds one to its left neighbour");
    e.edit(30, 0, "1");
    e.edit(31, 0, "3");
    e.send(json!({ "t": "autofill", "sheet": 0, "r1": 30, "c1": 0, "r2": 31, "c2": 0, "r2n": 35, "c2n": 0 }));
    assert_eq!((e.bob_text(0, 32, 0), e.bob_text(0, 35, 0)), ("5".into(), "11".into()), "a series from two numbers");
    // Clear.
    e.send(json!({ "t": "clear", "sheet": 0, "r1": 0, "c1": 0, "r2": 100, "c2": 100, "clamp": true }));
    assert_eq!(e.bob_text(0, 35, 0), "");
    e.converged();
}

#[test]
fn formats_merge_with_concurrent_changes() {
    let mut e = env();
    e.edit(0, 0, "7");
    e.edit(1, 0, "8");
    let fmt = |r1: u64, r2: u64, set: Value| json!({ "t": "fmt", "sheet": 0, "r1": r1, "c1": 0, "r2": r2, "c2": 0, "set": set });
    e.send(fmt(0, 1, json!({ "color": "#ff0000", "fill": "#00ff00", "h": "c", "v": "t", "w": true, "z": 18, "nf": "0.00" })));
    let st = |e: &mut Env, r: u32| {
        let wb = e.bob.workbook();
        wb.style(wb.sheets[0].get(r, 0).unwrap().style).clone()
    };
    let s = st(&mut e, 1);
    assert_eq!((s.color, s.fill, s.wrap, s.size10, s.num_fmt.as_str()), (Some(0xff0000), Some(0x00ff00), true, 180, "0.00"));
    assert_eq!(e.bob_text(0, 0, 0), "7.00");
    // Borders: presets over a range.
    e.send(fmt(0, 1, json!({ "bd": "outer" })));
    let (top, bottom) = (st(&mut e, 0).border, st(&mut e, 1).border);
    assert_eq!((top.top as u8, top.bottom as u8, bottom.bottom as u8, bottom.top as u8), (1, 0, 1, 0));
    e.send(fmt(0, 1, json!({ "bd": "all" })));
    assert_eq!(st(&mut e, 0).border.bottom as u8, 1);
    e.send(fmt(0, 1, json!({ "bd": "none" })));
    assert_eq!(st(&mut e, 0).border, Default::default());
    assert!(e.rejected(fmt(0, 0, json!({ "bd": "wavy" }))).contains("Unknown"));
    assert!(e.rejected(fmt(0, 0, json!({ "z": 500 }))).contains("between"));
    // Each format is its own step in undo.
    e.send(json!({ "t": "undo" }));
    e.send(json!({ "t": "undo" }));
    assert_eq!(st(&mut e, 0).border.top as u8, 1, "back to the outer border");
    e.send(json!({ "t": "undo" }));
    assert_eq!(st(&mut e, 0).border, Default::default());
    // A concurrent change of the same cell's format by another person: one whole style wins, everyone agrees.
    let sheet = e.bob.first_sheet().unwrap();
    let mut other = st(&mut e, 0);
    other.italic = true;
    e.br.send(fmt(0, 0, json!({ "b": true })));
    e.bob.set_format(sheet, 0, 0, freedom_sheets::live::style_json(&other));
    e.settle();
    e.converged();
    // Reset formats.
    e.send(fmt(0, 1, json!({ "reset": true })));
    assert_eq!(st(&mut e, 1), Default::default());
    e.converged();
}

#[test]
fn viewers_can_copy_and_find() {
    let (s, apis, item) = setup();
    let mut br = Browser::open(&s, &apis[0], &item);
    br.wait_for("hello", |v| v["t"] == "hello");
    br.edit(0, 0, "Apple pie");
    br.edit(5, 2, "pineapple");
    br.edit(6, 2, "=1+1");
    br.wait_saved();
    let mut view = Browser::open(&s, &apis[2], &item);
    view.wait_for("hello", |v| v["t"] == "hello");
    view.send(json!({ "t": "copy", "sheet": 0, "r1": 0, "c1": 0, "r2": 0, "c2": 0 }));
    let c = view.wait_for("clip", |v| v["t"] == "clip");
    assert_eq!(c["text"], "Apple pie\n");
    assert_eq!(c["cut"], false, "a viewer can't cut");
    view.send(json!({ "t": "find", "id": 4, "q": "APPLE", "all": true }));
    let f = view.wait_for("found", |v| v["t"] == "found");
    assert_eq!(f["hits"].as_array().unwrap().len(), 2);
    assert_eq!(f["hits"][1], json!([0, 5, 2, "pineapple"]));
    view.send(json!({ "t": "find", "id": 5, "q": "apple", "case": true, "all": true }));
    let f = view.wait_for("found", |v| v["t"] == "found" && v["id"] == 5);
    assert_eq!(f["hits"].as_array().unwrap().len(), 1);
    view.send(json!({ "t": "find", "id": 6, "q": "=1+1", "formulas": true, "whole": true, "all": true }));
    let f = view.wait_for("found", |v| v["t"] == "found" && v["id"] == 6);
    assert_eq!(f["hits"].as_array().unwrap().len(), 1);
}

#[test]
fn a_viewer_cannot_change_structure() {
    let (s, apis, item) = setup();
    let mut view = Browser::open(&s, &apis[2], &item);
    view.wait_for("hello", |v| v["t"] == "hello");
    for m in [json!({ "t": "ins", "sheet": 0, "axis": "r", "at": 0, "n": 1 }), json!({ "t": "sheet_add" }), json!({ "t": "size", "sheet": 0, "axis": "c", "a": 0, "b": 0, "px": 90 }), json!({ "t": "paste", "sheet": 0, "r": 0, "c": 0, "grid": [["x"]] })] {
        view.seen.clear();
        view.send(m);
        view.wait_for("rejection", |v| v["t"] == "rejected");
    }
}
