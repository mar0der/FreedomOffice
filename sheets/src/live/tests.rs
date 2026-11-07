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

// ---------------------------------------------------------------------------
// Protocol 1 mode: separate content/format parts become whole cells
// ---------------------------------------------------------------------------

/// `ops` applied to `base` (as the editor does), and the same state reached from `base` by the
/// whole-cell form `collapse_cells` gives (what a protocol 1 server is sent).
fn collapsed_state_matches(base: &Workbook, ops: &[Op]) -> Result<(), String> {
    let mut a = base.clone();
    let mut st = ApplyState::default();
    apply_ops(&mut a, ops, false, &mut st);
    let whole = collapse_cells(&a, ops);
    if whole.iter().any(|o| matches!(o, Op::Content { .. } | Op::Format { .. })) {
        return Err(format!("parts left in {whole:?}"));
    }
    let mut b = base.clone();
    apply_ops(&mut b, &whole, false, &mut st);
    let d = compare(&a, &b);
    if d.is_empty() { Ok(()) } else { Err(format!("{d:?} for {ops:?} -> {whole:?}")) }
}

fn content(s: cosync::SheetId, r: u32, c: u32, t: &str) -> Op {
    Op::Content { s, r, c, v: Some(Box::new(CellData { v: Val::Text(t.into()), ..Default::default() })), adj: vec![] }
}

fn bold(s: cosync::SheetId, r: u32, c: u32) -> Op {
    Op::Format { s, r, c, st: style_json(&crate::model::Style { bold: true, ..Default::default() }) }
}

#[test]
fn whole_cells_follow_later_row_and_column_changes_in_the_same_list() {
    // Changes made before the server said which protocol it speaks wait as parts, with row and
    // column changes among them; each names its cell as it was at its moment.
    let base = book();
    let s = base.sheets[0].uid;
    let first = [content(s, 3, 0, "x"), Op::Rows { s, at: 1, n: 2 }, content(s, 8, 0, "y"), bold(s, 3, 0), Op::Cols { s, at: 0, n: 1 }, bold(s, 5, 1)];
    collapsed_state_matches(&base, &first).unwrap();
    // A cell written and then deleted with its row writes nothing; one clearing another's content.
    let gone = [content(s, 2, 0, "z"), Op::Rows { s, at: 2, n: -1 }, Op::Rows { s, at: 0, n: 1 }, Op::Content { s, r: 3, c: 0, v: None, adj: vec![] }];
    collapsed_state_matches(&base, &gone).unwrap();
    let out = collapse_cells(&base, &gone);
    assert!(out.iter().filter(|o| matches!(o, Op::Cell { .. })).count() == 1, "{out:?}");
}

#[test]
fn whole_cells_equal_the_state_for_random_edit_sequences() {
    let base = book();
    let s = base.sheets[0].uid;
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move |n: u32| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as u32
    };
    for round in 0..300 {
        let mut ops = Vec::new();
        for i in 0..1 + next(12) {
            ops.push(match next(10) {
                0 => Op::Rows { s, at: next(8), n: if next(3) == 0 { -1 } else { 1 + next(2) as i32 } },
                1 => Op::Cols { s, at: next(4), n: if next(3) == 0 { -1 } else { 1 } },
                2 | 3 => bold(s, next(8), next(3)),
                4 => Op::Content { s, r: next(8), c: next(3), v: None, adj: vec![] },
                5 => Op::Cell { s, r: next(8), c: next(3), v: Some(Box::new(CellData { v: Val::Num(i as f64), ..Default::default() })), adj: vec![] },
                _ => content(s, next(8), next(3), &format!("t{round}-{i}")),
            });
        }
        if let Err(e) = collapsed_state_matches(&base, &ops) {
            panic!("round {round}: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// A protocol 1 server in-process: clients that only learn at `Welcome` what it speaks
// ---------------------------------------------------------------------------

mod old_server_model {
    use std::collections::VecDeque;

    use cosync::client::{Batch, Session, Step};
    use cosync::server::{Commit, Log};

    use super::*;

    /// What the server sends a client.
    enum To {
        Welcome(u64),
        Ops { seq: u64, cid: String, id: u64, ops: Vec<Op> },
        Ack { id: u64, seq: u64, extra: Vec<(u32, Op)> },
    }

    struct Client {
        wb: Workbook,
        sess: Session,
        cid: String,
        /// Connected: the server has this client's connection.
        online: bool,
        /// A `Welcome` has been seen at some time: the client knows the server only has whole cells.
        legacy: bool,
        caught_up: bool,
        target: u64,
        inbox: VecDeque<To>,
        /// Batches sent and not yet handled by the server.
        outbox: VecDeque<Batch>,
        state: ApplyState,
    }

    impl Client {
        fn new(base: &Workbook, i: usize) -> Client {
            let cid = format!("c{i}");
            let mut sess = Session::new(cid.clone(), 0);
            sess.offline();
            Client { wb: base.clone(), sess, cid, online: false, legacy: false, caught_up: false, target: 0, inbox: VecDeque::new(), outbox: VecDeque::new(), state: ApplyState::default() }
        }

        /// Local change, as the editor makes it: applied, collapsed to whole cells once the
        /// client knows the server is old, then sent or kept.
        fn edit(&mut self, ops: Vec<Op>) {
            apply_ops(&mut self.wb, &ops, false, &mut self.state);
            let ops = if self.legacy { collapse_cells(&self.wb, &ops) } else { ops };
            if let Some(b) = self.sess.local(ops) {
                self.outbox.push_back(b);
            }
        }

        fn stepped(&mut self, step: Step) {
            apply_ops(&mut self.wb, &step.apply, true, &mut self.state);
            if let Some(b) = step.send {
                self.outbox.push_back(b);
            }
        }

        fn caught_up_check(&mut self) {
            if self.online && !self.caught_up && self.sess.seq >= self.target {
                self.caught_up = true;
                if let Some(b) = self.sess.resume() {
                    self.outbox.push_back(b);
                }
            }
        }

        fn receive(&mut self) {
            let Some(m) = self.inbox.pop_front() else { return };
            match m {
                To::Welcome(head) => {
                    self.legacy = true;
                    // (what the app and fsheets do on the old server's Welcome)
                    collapse_pending(&self.wb, self.sess.inflight.as_mut().map(|b| &mut b.ops), &mut self.sess.buffer);
                    self.target = head;
                }
                To::Ops { seq, cid, id, ops } => {
                    let step = self.sess.remote_based(seq, &cid, id, None, ops);
                    self.stepped(step);
                }
                To::Ack { id, seq, extra } => {
                    let step = self.sess.ack(id, seq, extra);
                    self.stepped(step);
                }
            }
            self.caught_up_check();
        }
    }

    struct World {
        log: Log,
        server_wb: Workbook,
        clients: Vec<Client>,
        state: ApplyState,
    }

    impl World {
        fn connect(&mut self, i: usize) {
            let c = &mut self.clients[i];
            if c.online {
                return;
            }
            c.online = true;
            c.caught_up = false;
            c.sess.offline();
            c.inbox.clear();
            c.outbox.clear();
            c.inbox.push_back(To::Welcome(self.log.head()));
            c.target = self.log.head();
            for e in self.log.since(c.sess.seq).expect("kept").to_vec() {
                c.inbox.push_back(To::Ops { seq: e.seq, cid: e.cid, id: e.id, ops: e.ops });
            }
        }

        fn disconnect(&mut self, i: usize, deliver: bool) {
            if deliver {
                self.serve(i);
            }
            let c = &mut self.clients[i];
            c.online = false;
            c.caught_up = false;
            c.sess.offline();
            c.inbox.clear();
            c.outbox.clear();
        }

        /// The server handles one batch of client `i`.
        fn serve(&mut self, i: usize) {
            let Some(b) = self.clients[i].outbox.pop_front() else { return };
            let cid = self.clients[i].cid.clone();
            match self.log.commit(b.base, &cid, b.id, "u", b.ops).expect("commit") {
                Commit::New { seq, ops, extra } => {
                    apply_ops(&mut self.server_wb, &ops, true, &mut self.state);
                    for (j, c) in self.clients.iter_mut().enumerate() {
                        if !c.online {
                            continue;
                        }
                        if j == i {
                            c.inbox.push_back(To::Ack { id: b.id, seq, extra: extra.clone() });
                        } else {
                            c.inbox.push_back(To::Ops { seq, cid: cid.clone(), id: b.id, ops: ops.clone() });
                        }
                    }
                }
                Commit::Duplicate { seq } => {
                    if self.clients[i].online {
                        self.clients[i].inbox.push_back(To::Ack { id: b.id, seq, extra: vec![] });
                    }
                }
            }
        }
    }

    fn random_edit(next: &mut impl FnMut(u32) -> u32, s: cosync::SheetId, tag: &str) -> Op {
        match next(12) {
            0 => Op::Rows { s, at: next(10), n: if next(3) == 0 { -1 } else { 1 + next(2) as i32 } },
            1 => Op::Cols { s, at: next(5), n: if next(3) == 0 { -1 } else { 1 } },
            2..=4 => bold(s, next(10), next(4)),
            5 => Op::Content { s, r: next(10), c: next(4), v: None, adj: vec![] },
            6 => Op::Cell { s, r: next(10), c: next(4), v: Some(Box::new(CellData { v: Val::Num(next(100) as f64), ..Default::default() })), adj: vec![] },
            _ => content(s, next(10), next(4), tag),
        }
    }

    /// Clients make random changes (also before their first `Welcome`, and while disconnected),
    /// lose their connections at random and come back: all must end with the server's state.
    #[test]
    fn clients_of_a_protocol_1_server_converge_with_changes_made_before_its_welcome() {
        let base = book();
        let s = base.sheets[0].uid;
        let mut failures = Vec::new();
        for seed in 1..=150u64 {
            let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut next = move |n: u32| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                (rng >> 11) as u32 % n
            };
            let mut w = World { log: Log::new(0), server_wb: base.clone(), clients: (0..3).map(|i| Client::new(&base, i)).collect(), state: ApplyState::default() };
            for step in 0..200 {
                let i = next(3) as usize;
                match next(100) {
                    0..=34 => {
                        let tag = format!("s{seed}-{step}");
                        let ops = vec![random_edit(&mut next, s, &tag)];
                        w.clients[i].edit(ops);
                    }
                    35..=59 => w.clients[i].receive(),
                    60..=84 => w.serve(i),
                    85..=89 => {
                        let deliver = next(2) == 0;
                        w.disconnect(i, deliver)
                    }
                    _ => w.connect(i),
                }
            }
            // Everybody comes back and everything is handled.
            for _ in 0..10_000 {
                let mut busy = false;
                for i in 0..3 {
                    w.connect(i);
                    busy |= !w.clients[i].inbox.is_empty() || !w.clients[i].outbox.is_empty();
                    w.clients[i].receive();
                    w.serve(i);
                }
                if !busy && w.clients.iter().all(|c| c.caught_up && !c.sess.has_pending() && c.sess.seq == w.log.head()) {
                    break;
                }
            }
            for (i, c) in w.clients.iter().enumerate() {
                let d = compare(&w.server_wb, &c.wb);
                if !d.is_empty() || c.sess.has_pending() {
                    failures.push(format!("seed {seed}: client {i} differs from the server: {:?}", d.first()));
                }
            }
        }
        assert!(failures.is_empty(), "{} runs diverged, e.g. {:?}", failures.len(), &failures[..failures.len().min(3)]);
    }
}
