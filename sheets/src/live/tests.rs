//! Tests of the live operations on the workbook model.

use cosync::{CellData, Op, Val};

use super::*;
use crate::model::{cell_from_input, Range, Sheet, Workbook};

fn book() -> Workbook {
    let mut wb = Workbook::empty_with(vec![Sheet::new("Sheet1"), Sheet::new("Data")]);
    assign_ids(&mut wb, "");
    for (r, t) in ["1", "2", "3", "4", "5", "=SUM(A1:A5)", "=Data!B2*2"].iter().enumerate() {
        let c = cell_from_input(&mut wb, t, 0);
        wb.sheets[0].set(r as u32, 0, c);
    }
    wb.recalc();
    wb
}

fn formula_at(wb: &Workbook, s: usize, r: u32, c: u32) -> String {
    wb.sheets[s].get(r, c).and_then(|x| x.formula.as_ref()).map(|f| f.text.clone()).unwrap_or_default()
}

#[test]
fn order_keys_keep_most_sheets_in_place() {
    let p = diff::positions_for_test(&[(1, Some(0.0)), (2, Some(1.0)), (3, Some(2.0))]);
    assert_eq!(p, [0.0, 1.0, 2.0]);
    // Sheet 3 moved to the front: only it gets a new key.
    let p = diff::positions_for_test(&[(3, Some(2.0)), (1, Some(0.0)), (2, Some(1.0))]);
    assert_eq!((p[1], p[2]), (0.0, 1.0));
    assert!(p[0] < 0.0);
    // A new sheet between two others.
    let p = diff::positions_for_test(&[(1, Some(0.0)), (9, None), (2, Some(1.0))]);
    assert!(p[1] > 0.0 && p[1] < 1.0);
    // Ties are fine when ids order them; otherwise everything is renumbered.
    let p = diff::positions_for_test(&[(1, Some(0.0)), (2, Some(0.0))]);
    assert_eq!(p, [0.0, 0.0]);
}

#[test]
fn formulas_transformed_over_structure_are_adjusted_like_the_command_does() {
    // A formula written before someone inserted rows above what it refers to.
    let mut wb = book();
    let s = wb.sheets[0].uid;
    crate::structure::shift_structure(&mut wb, 0, true, 1, 2);
    let op = Op::Cell { s, r: 9, c: 1, v: Some(Box::new(CellData { f: Some("A3+A5".into()), ..Default::default() })), adj: vec![Op::Rows { s, at: 1, n: 2 }] };
    let mut st = ApplyState::default();
    apply_ops(&mut wb, &[op], true, &mut st);
    assert_eq!(formula_at(&wb, 0, 9, 1), "A5+A7");
    // The same formula had it been there: shifted by the command itself.
    assert_eq!(formula_at(&wb, 0, 7, 0), "SUM(A1:A7)");
    // A rename it went through, then a delete of the sheet it refers to.
    let data = wb.sheets[1].uid;
    crate::structure::rename_sheet(&mut wb, 1, "Facts");
    let op = Op::Cell { s, r: 10, c: 1, v: Some(Box::new(CellData { f: Some("Data!A1".into()), ..Default::default() })), adj: vec![Op::Rename { s: data, from: "Data".into(), to: "Facts".into() }] };
    apply_ops(&mut wb, &[op], true, &mut st);
    assert_eq!(formula_at(&wb, 0, 10, 1), "Facts!A1");
}

#[test]
fn features_transformed_over_structure_match_the_command() {
    let mut a = book();
    let s = a.sheets[0].uid;
    let rule = crate::condfmt::CondFormat::new(vec![Range::parse("A2:A6").unwrap()], crate::condfmt::RuleKind::Expression { formula: "A2>2".into() }, Default::default());
    a.sheets[0].cond_formats.push(rule.clone());
    let mut b = a.clone();
    // Path 1: the rule existed when rows were deleted.
    crate::structure::shift_structure(&mut a, 0, true, 0, -1);
    // Path 2: rows deleted first, then the rule arrives having been transformed over that.
    b.sheets[0].cond_formats.clear();
    crate::structure::shift_structure(&mut b, 0, true, 0, -1);
    let op = Op::Feature { s, key: "cond_formats".into(), val: serde_json::to_value(vec![rule]).unwrap(), adj: vec![Op::Rows { s, at: 0, n: -1 }] };
    let mut st = ApplyState::default();
    apply_ops(&mut b, &[op], true, &mut st);
    assert_eq!(a.sheets[0].cond_formats, b.sheets[0].cond_formats);
    assert_eq!(a.sheets[0].cond_formats[0].ranges, vec![Range::parse("A1:A5").unwrap()]);
}

#[test]
fn cells_round_trip_by_content() {
    let mut wb = book();
    let bold = wb.intern_style(crate::model::Style { bold: true, num_fmt: "0.0".into(), ..Default::default() });
    wb.sheets[0].cell_mut(0, 0).style = bold;
    let d = cell_data(&wb, &wb.sheets[0], 0, 0, wb.sheets[0].get(0, 0).unwrap());
    assert_eq!(d.v, Val::Num(1.0));
    assert!(d.st.is_some());
    // Another workbook interns styles in a different order: content still matches.
    let mut other = Workbook::empty_with(vec![Sheet::new("Sheet1")]);
    other.intern_style(crate::model::Style { italic: true, ..Default::default() });
    assign_ids(&mut other, "");
    let op = Op::Cell { s: other.sheets[0].uid, r: 0, c: 0, v: Some(Box::new(d)), adj: vec![] };
    apply_ops(&mut other, &[op], true, &mut ApplyState::default());
    let x = other.sheets[0].get(0, 0).unwrap();
    assert!(other.style(x.style).bold && other.style(x.style).num_fmt == "0.0");
    assert_ne!(x.style, bold);
}

#[test]
fn deleting_the_last_sheet_shows_a_provisional_one_until_the_server_answers() {
    let mut wb = Workbook::empty_with(vec![Sheet::new("Only")]);
    assign_ids(&mut wb, "");
    let mut st = ApplyState::default();
    apply_ops(&mut wb, &[Op::DelSheet { s: 1, name: "Only".into() }], true, &mut st);
    assert_eq!((wb.sheets.len(), wb.sheets[0].uid, st.provisional), (1, cosync::RESCUE_SHEET, true));
    // Someone else's new sheet arrives first: the provisional one goes away.
    apply_ops(&mut wb, &[Op::AddSheet { s: 77, name: "New".into(), pos: 3.0 }], true, &mut st);
    assert_eq!(wb.sheets.iter().map(|s| s.uid).collect::<Vec<_>>(), [77]);
    assert!(!st.provisional);
}

#[test]
fn snapshot_metadata_gives_the_same_ids_everywhere() {
    let mut wb = book();
    wb.sheets[0].uid = 55;
    wb.sheets[0].pos = 2.5;
    let meta = sheets_meta(&wb);
    let mut other = Workbook::empty_with(vec![Sheet::new("Sheet1"), Sheet::new("Data")]);
    assign_ids(&mut other, &meta);
    assert_eq!((other.sheets[0].uid, other.sheets[0].pos), (55, 2.5));
    assert_eq!(other.sheets[1].uid, wb.sheets[1].uid);
    // Without metadata (a version saved outside a live session): by position.
    assign_ids(&mut other, "");
    assert_eq!(other.sheets.iter().map(|s| s.uid).collect::<Vec<_>>(), [1, 2]);
    // Pictures travel as base64.
    let pic = crate::model::Picture { data: std::sync::Arc::new(vec![0x89, b'P', b'N', b'G', 0, 255]), ext: "png".into(), anchor: (1, 1), offset: (0.0, 0.0), size: (10.0, 10.0), name: "p".into(), descr: String::new() };
    let j = serde_json::to_value(vec![pic.clone()]).unwrap();
    assert!(j[0]["data"].is_string());
    let back: Vec<crate::model::Picture> = serde_json::from_value(j).unwrap();
    assert_eq!(back[0], pic);
}

#[test]
fn content_and_format_operations_each_leave_the_other_part_alone() {
    let mut wb = book();
    let s = wb.sheets[0].uid;
    let mut st = ApplyState::default();
    let bold = serde_json::to_value(crate::model::Style { bold: true, ..Default::default() }).unwrap();
    // A format on a cell with a value keeps the value; on an empty cell it makes a styled blank.
    apply_ops(&mut wb, &[Op::Format { s, r: 0, c: 0, st: Some(bold.clone()) }, Op::Format { s, r: 20, c: 3, st: Some(bold.clone()) }], true, &mut st);
    let a1 = wb.sheets[0].get(0, 0).unwrap();
    assert!(wb.style(a1.style).bold && a1.value == crate::model::Value::Number(1.0));
    assert!(wb.sheets[0].get(20, 3).is_some_and(|c| c.value.is_empty() && wb.style(c.style).bold));
    // New content keeps the format, a formula too; clearing the content leaves the styled cell.
    let text = |t: &str| Some(Box::new(CellData { v: Val::Text(t.into()), ..Default::default() }));
    apply_ops(&mut wb, &[Op::Content { s, r: 0, c: 0, v: text("typed"), adj: vec![] }, Op::Content { s, r: 20, c: 3, v: Some(Box::new(CellData { f: Some("A1&\"!\"".into()), ..Default::default() })), adj: vec![] }], true, &mut st);
    let a1 = wb.sheets[0].get(0, 0).unwrap();
    assert!(wb.style(a1.style).bold && a1.value == crate::model::Value::Text("typed".into()));
    assert_eq!(formula_at(&wb, 0, 20, 3), "A1&\"!\"");
    assert!(wb.style(wb.sheets[0].get(20, 3).unwrap().style).bold);
    apply_ops(&mut wb, &[Op::Content { s, r: 0, c: 0, v: None, adj: vec![] }], true, &mut st);
    assert!(wb.sheets[0].get(0, 0).is_some_and(|c| c.value.is_empty() && wb.style(c.style).bold));
    // Resetting the format of an otherwise empty cell removes it; a cell with content stays.
    apply_ops(&mut wb, &[Op::Format { s, r: 0, c: 0, st: None }, Op::Format { s, r: 1, c: 0, st: None }], true, &mut st);
    assert!(wb.sheets[0].get(0, 0).is_none());
    assert_eq!(wb.sheets[0].get(1, 0).unwrap().value, crate::model::Value::Number(2.0));
}

// ---------------------------------------------------------------------------
// Threaded comments
// ---------------------------------------------------------------------------

fn who(user: &str, manager: bool) -> comments::Who {
    comments::Who { user: user.into(), name: user.to_uppercase(), manager }
}

/// Makes `ops` on a copy of `wb` and returns what diffing the two says.
fn thread_change(wb: &Workbook, ops: &[Op]) -> (Workbook, Change) {
    let before = Before::Sheet(Box::new(wb.sheets[0].clone()));
    let mut after = wb.clone();
    apply_ops(&mut after, ops, false, &mut ApplyState::default());
    let ch = diff(vec![before], &[], &mut after);
    (after, ch)
}

#[test]
fn comment_changes_are_found_by_the_diff_and_undone_by_the_inverse() {
    let wb = book();
    let (s, ann, ben) = (wb.sheets[0].uid, who("ann", false), who("ben", false));
    let add = comments::add_thread(s, 2, 1, &ann, "Is this right?");
    let Op::ThreadAdd { th, .. } = &add else { panic!() };
    let id = th.id.clone();
    assert!(id.starts_with('{') && id.ends_with('}') && id.len() == 38, "{id}");
    let (wb1, ch) = thread_change(&wb, &[add]);
    assert!(matches!(&ch.ops[..], [Op::ThreadAdd { r: 2, c: 1, .. }]));
    assert!(matches!(&ch.inverse[..], [Op::ThreadDel { .. }]));
    // The operations reproduce the change on another copy, the inverse undoes it.
    let mut other = wb.clone();
    apply_ops(&mut other, &ch.ops, true, &mut ApplyState::default());
    assert_eq!(compare(&other, &wb1), Vec::<String>::new());
    let mut inv = ch.inverse.clone();
    comments::stamp(&mut inv, &ann);
    apply_ops(&mut other, &inv, false, &mut ApplyState::default());
    assert_eq!(compare(&other, &wb), Vec::<String>::new());
    // Reply, edit, resolve in one change.
    let mut wb2 = wb1.clone();
    let reply = comments::reply(s, &id, &ben, "Yes.");
    let Op::CommentAdd { cm, .. } = &reply else { panic!() };
    let rid = cm.id.clone();
    apply_ops(&mut wb2, &[reply, comments::resolve(s, &id, true), comments::edit(s, &id, &id, &ann, "Is this right? (A3)")], false, &mut ApplyState::default());
    let t = &wb2.sheets[0].threads[0];
    assert!(t.done && t.comments.len() == 2 && t.comments[0].text.ends_with("(A3)") && t.comments[0].edited > 0);
    let (_, ch) = thread_change(&wb1, &[comments::reply(s, &id, &ben, "x")]);
    assert!(matches!(&ch.ops[..], [Op::CommentAdd { .. }]) && matches!(&ch.inverse[..], [Op::CommentDel { .. }]));
    // Someone else's edit or delete is refused by the rule, a manager's delete is not.
    let mut wb3 = wb2.clone();
    apply_ops(&mut wb3, &[comments::edit(s, &id, &rid, &ann, "hijack"), comments::delete(s, &id, &rid, &ann)], false, &mut ApplyState::default());
    assert_eq!(wb3.sheets[0].threads[0].comments[1].text, "Yes.");
    apply_ops(&mut wb3, &[comments::delete(s, &id, &rid, &who("admin", true))], false, &mut ApplyState::default());
    assert_eq!(wb3.sheets[0].threads[0].comments.len(), 1);
}

#[test]
fn threads_are_part_of_the_lossless_state_and_follow_rows() {
    let mut wb = book();
    let (s, ann) = (wb.sheets[0].uid, who("ann", false));
    apply_ops(&mut wb, &[comments::add_thread(s, 4, 0, &ann, "five"), comments::add_thread(s, 4, 0, &who("ben", false), "same cell")], false, &mut ApplyState::default());
    assert_eq!(wb.sheets[0].threads.len(), 2);
    let rebuilt = from_state(&state_ops(&wb));
    assert_eq!(compare(&wb, &rebuilt), Vec::<String>::new());
    assert_eq!(rebuilt.sheets[0].threads, wb.sheets[0].threads);
    // Rows inserted above move them (by the structural op, as the app applies it), deleting the row removes them.
    apply_ops(&mut wb, &[Op::Rows { s, at: 0, n: 2 }], false, &mut ApplyState::default());
    assert!(wb.sheets[0].threads.iter().all(|t| (t.r, t.c) == (6, 0)));
    apply_ops(&mut wb, &[Op::Rows { s, at: 6, n: -1 }], false, &mut ApplyState::default());
    assert!(wb.sheets[0].threads.is_empty());
}

#[test]
fn restoring_someone_else_s_comments_is_left_out_of_an_undo() {
    let mut ops = vec![comments::add_thread(1, 0, 0, &who("ann", false), "mine"), comments::reply(1, "{T}", &who("ben", false), "theirs")];
    comments::drop_foreign_restores(&mut ops, "ann");
    assert_eq!(ops.len(), 1);
}

#[test]
fn mentions_are_matched_by_user_or_display_name() {
    let members = vec![("ann".to_string(), "Ann Lee".to_string()), ("anna".to_string(), "Anna K".to_string()), ("bob".to_string(), "Bob".to_string())];
    assert_eq!(comments::mentions("hi @Ann Lee and @bob, also @annabelle @anna", &members), ["ann", "bob", "anna"]);
    let text = "hi @Ann Lee, ok";
    assert_eq!(comments::mention_spans(text, &members), [(3, 11)]);
}
