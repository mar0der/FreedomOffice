//! Live co-editing: every command's change, found by comparing its undo snapshot with the
//! result, is reproduced exactly by the generated operations (and undone by their inverses).
//! The checks compare full copies of the workbook, so a command that changes something
//! outside its undo scope (or without one) fails here.

use eframe::egui;

use super::paste::{PasteOpts, PasteWhat};
use super::{App, EditMode};
use crate::condfmt::{CondFormat, DiffFormat, RuleKind};
use crate::model::Range;

fn check(app: &mut App, what: &str) {
    let errors = app.live_errors();
    assert!(errors.is_empty(), "{what}: the operations don't reproduce the change:\n{}", errors.join("\n"));
}

fn type_at(app: &mut App, r: u32, c: u32, text: &str) {
    app.select(r, c, false);
    app.begin_edit(Some(text.to_string()), EditMode::Enter);
    assert!(app.commit_edit(false));
}

fn rg(s: &str) -> Range {
    Range::parse(s).unwrap()
}

#[test]
fn colours_follow_the_user_and_agents_look_different() {
    use super::live_ui::{presence_alpha, presence_label, user_color};
    let peer = |user: &str, color: u8, agent: bool| cosync::Peer { id: color as u64, name: user.into(), user: user.into(), color, editor: true, manager: false, agent, presence: None };
    // Same user: same colour whatever the session's slot or window; case doesn't matter.
    assert_eq!(user_color(&peer("ann", 0, false)), user_color(&peer("ann", 5, false)));
    assert_eq!(user_color(&peer("Ann", 1, false)), user_color(&peer("ann", 2, false)));
    // Different users mostly differ (12 colours): the four test accounts are all distinct.
    let cols: std::collections::HashSet<_> = ["test-a", "test-b", "test-c", "test-d"].iter().map(|u| user_color(&peer(u, 0, false))).collect();
    assert!(cols.len() >= 3, "{cols:?}");
    // Agents: their own colour, labelled.
    assert_ne!(user_color(&peer("agent-claude", 0, true)), user_color(&peer("ann", 0, false)));
    assert_eq!(user_color(&peer("agent-claude", 0, true)), user_color(&peer("other-bot", 3, true)));
    assert_eq!(presence_label(&peer("agent-claude", 0, true)), "agent-claude (bot)");
    assert_eq!(presence_label(&peer("ann", 0, false)), "ann");
    // Link guests keep the server's slot.
    assert_ne!(user_color(&peer("", 0, false)), user_color(&peer("", 1, false)));
    // Fade in, stay, dim when idle (never fully gone).
    let s = std::time::Duration::from_secs;
    assert!(presence_alpha(std::time::Duration::ZERO, s(0)) < 0.1);
    assert_eq!(presence_alpha(s(5), s(10)), 1.0);
    assert!(presence_alpha(s(500), s(70)) < 1.0 && presence_alpha(s(500), s(70)) > 0.5);
    assert!((presence_alpha(s(500), s(600)) - 0.45).abs() < 0.01);
}

#[test]
fn commands_round_trip_as_operations() {
    let (ctx, mut app) = super::data_ui::sample_app();
    app.live_for_test();
    check(&mut app, "start");

    // Typing values, text, formulas, a multi-line entry (grows the row).
    type_at(&mut app, 10, 0, "hello");
    type_at(&mut app, 10, 1, "42");
    type_at(&mut app, 10, 2, "=SUM(C2:C8)*B11");
    type_at(&mut app, 11, 0, "two\nlines");
    check(&mut app, "typing");
    // Ctrl+Enter fills the selection.
    app.select_range(rg("E12:F14"));
    app.begin_edit(Some("=A1".into()), EditMode::Enter);
    assert!(app.commit_edit(true));
    check(&mut app, "fill selection");

    // Formatting.
    app.select_range(rg("A1:E1"));
    app.toggle_bold();
    app.apply_style(|s| s.fill = Some(0xDDEEFF));
    app.set_num_fmt("0.00");
    check(&mut app, "formatting");
    app.borders(super::actions::BorderKind::All);
    check(&mut app, "borders");

    // Fill down, autofill a series, paste special.
    app.select_range(rg("C2:C5"));
    app.fill_down();
    app.autofill(rg("B11"), rg("B11:B15"));
    check(&mut app, "fill");
    app.select_range(rg("A1:C3"));
    app.copy(&ctx, false);
    app.select(20, 0, false);
    app.paste_special(PasteOpts::what(PasteWhat::Values));
    app.select(25, 0, false);
    app.paste_special(PasteOpts { transpose: true, ..PasteOpts::what(PasteWhat::All) });
    check(&mut app, "paste special");

    // Sort, filter, remove duplicates.
    app.select(1, 2, false);
    app.sort_simple(true);
    check(&mut app, "sort");
    app.select(0, 0, false);
    app.toggle_filter();
    check(&mut app, "filter");
    app.remove_duplicates(rg("A1:B8"), true, &[0]);
    check(&mut app, "remove duplicates");

    // Rows and columns.
    app.select_range(rg("3:4"));
    app.insert_rows();
    check(&mut app, "insert rows");
    app.select_range(rg("2:2"));
    app.delete_rows();
    check(&mut app, "delete rows");
    app.select_range(rg("B:B"));
    app.insert_cols();
    app.select_range(rg("D:D"));
    app.delete_cols();
    check(&mut app, "insert/delete columns");
    app.select_range(rg("B3:C4"));
    app.insert_cells(true);
    app.delete_cells(false);
    check(&mut app, "insert/delete cells");
    app.select_range(rg("A1:C1"));
    app.set_col_width(90.0);
    app.set_row_height(30.0);
    app.select_range(rg("6:7"));
    app.hide_rows(true);
    check(&mut app, "sizes and hidden rows");

    // Merges, notes, links, freeze panes, names.
    app.select_range(rg("H2:J3"));
    app.merge(0);
    check(&mut app, "merge");
    app.set_note(1, 1, Some("A note".into()));
    app.set_link(2, 0, "Site", Some("https://example.com"));
    check(&mut app, "notes and links");
    app.select(1, 1, false);
    app.freeze(0);
    let _ = app.new_name("Total", "Sheet1!$C$2:$C$9");
    check(&mut app, "freeze and names");

    // Conditional formatting, a table, a chart.
    app.select_range(rg("C2:C9"));
    app.cf_add_rule(CondFormat::new(vec![rg("C2:C9")], RuleKind::Expression { formula: "C2>200".into() }, DiffFormat { fill: Some(0xFF0000), ..Default::default() }));
    check(&mut app, "conditional format");
    app.select_range(rg("L1:M4"));
    for (i, t) in ["Name", "Qty", "a", "1", "b", "2", "c", "3"].iter().enumerate() {
        type_at(&mut app, (i / 2) as u32, 11 + (i % 2) as u32, t);
    }
    app.select_range(rg("L1:M4"));
    app.format_as_table(crate::table::ACCENTS[1], 1);
    check(&mut app, "table");
    app.select_range(rg("A1:C5"));
    app.insert_chart(crate::chart::ChartKind::Column);
    check(&mut app, "chart");
    let sh = &app.wb.sheets[0];
    assert_eq!((sh.tables.len(), sh.charts.len(), sh.cond_formats.len()), (1, 1, 1));
    assert!(!sh.merges.is_empty() && sh.filter.is_some() && sh.freeze != (0, 0) && !app.wb.names.is_empty());
    assert!(sh.notes.contains_key(&(1, 1)) && !sh.hidden_rows.is_empty());

    // Clearing.
    app.select_range(rg("A20:C22"));
    app.clear(true, true);
    check(&mut app, "clear");

    // Sheets: add, rename, move, duplicate, hide, delete.
    app.add_sheet();
    check(&mut app, "add sheet");
    let n = app.wb.sheets.len();
    assert!(app.rename_sheet(n - 1, "Data").is_ok());
    check(&mut app, "rename sheet");
    type_at(&mut app, 0, 0, "=Sheet1!A2");
    app.move_sheet_to(n - 1, 0);
    check(&mut app, "move sheet");
    app.duplicate_sheet(0);
    check(&mut app, "duplicate sheet");
    assert!(app.rename_sheet(1, "Copy").is_ok());
    app.hide_sheet(1);
    app.unhide_sheet(1);
    check(&mut app, "hide/unhide sheet");
    app.delete_sheet(1);
    check(&mut app, "delete sheet");
    assert!(app.rename_sheet(app.wb.sheet_index("Sheet1").unwrap(), "Main").is_ok());
    check(&mut app, "rename referenced sheet");

    // Undo and redo go through operations too.
    for _ in 0..6 {
        app.undo();
        check(&mut app, "undo");
    }
    for _ in 0..3 {
        app.redo();
        check(&mut app, "redo");
    }
    assert!(!app.live_pending_ops().is_empty());

    // The lossless state stored with snapshots rebuilds exactly this workbook.
    let data = crate::live::encode_state(&crate::live::state_ops(&app.wb)).unwrap();
    let back = crate::live::from_state(&crate::live::decode_state(&data).unwrap());
    let d = crate::live::compare(&back, &app.wb);
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn undo_restores_deleted_rows_and_sheets() {
    let (_ctx, mut app) = super::data_ui::sample_app();
    app.live_for_test();
    let before = app.wb.clone();
    app.select_range(rg("2:4"));
    app.delete_rows();
    app.add_sheet();
    type_at(&mut app, 0, 0, "x");
    let n = app.wb.sheets.len();
    app.delete_sheet(n - 1);
    check(&mut app, "changes");
    for _ in 0..4 {
        app.undo();
    }
    check(&mut app, "undo");
    assert!(crate::live::compare(&before, &app.wb).is_empty(), "{:?}", crate::live::compare(&before, &app.wb));
    let _ = egui::Context::default();
}

/// Undo steps recorded before a later own insert are in the coordinates from before it:
/// others' changes must reach them through the steps above.
#[test]
fn undo_after_own_insert_and_remote_insert_clears_the_right_cell() {
    let (_ctx, mut app) = super::data_ui::sample_app();
    app.live_for_test();
    app.switch_sheet(0);
    let before = app.wb.clone();
    let at = |wb: &crate::model::Workbook, r: u32, c: u32| crate::numfmt::value_to_text(wb.sheets[0].display_value(r, c));
    type_at(&mut app, 4, 3, "x");
    app.select_range(rg("1:3"));
    app.insert_rows();
    assert_eq!(at(&app.wb, 7, 3), "x");
    // Someone inserts a row above "x" (row 7 now, row 4 before our insert).
    let s = app.wb.sheets[0].uid;
    app.live_remote_for_test(&[cosync::Op::Rows { s, at: 6, n: 1 }]);
    assert_eq!(at(&app.wb, 8, 3), "x");
    app.undo();
    app.undo();
    check(&mut app, "undo");
    assert_eq!(at(&app.wb, 5, 3), at(&before, 4, 3), "the cell typed in gets its old value back");
    assert_eq!(at(&app.wb, 4, 3), at(&before, 3, 3), "the cell above is left alone");
    assert_eq!(at(&app.wb, 3, 3), "", "the row someone else inserted stays");
}

/// Two own edits of a cell, then someone else's: neither undo overwrites their value.
#[test]
fn undo_never_overwrites_someone_elses_later_value() {
    let (_ctx, mut app) = super::data_ui::sample_app();
    app.live_for_test();
    app.switch_sheet(0);
    type_at(&mut app, 0, 0, "mine-1");
    type_at(&mut app, 0, 0, "mine-2");
    let s = app.wb.sheets[0].uid;
    let theirs = cosync::Op::Cell { s, r: 0, c: 0, v: Some(Box::new(cosync::CellData { v: cosync::Val::Text("theirs".into()), ..Default::default() })), adj: vec![] };
    app.live_remote_for_test(&[theirs]);
    app.undo();
    app.undo();
    assert_eq!(crate::numfmt::value_to_text(app.wb.sheets[0].display_value(0, 0)), "theirs");
}

/// Undo is per person: another person's concurrent format of the same cell and edit of another
/// cell survive our undo, and our undo only takes back our own value.
#[test]
fn undo_reverts_only_my_value_and_keeps_others_concurrent_changes() {
    let (_ctx, mut app) = super::data_ui::sample_app();
    app.live_for_test();
    app.switch_sheet(0);
    let at = |app: &App, r: u32, c: u32| crate::numfmt::value_to_text(app.wb.sheets[0].display_value(r, c));
    let old = at(&app, 0, 0);
    type_at(&mut app, 0, 0, "mine");
    let s = app.wb.sheets[0].uid;
    let bold = crate::live::style_json(&crate::model::Style { bold: true, ..Default::default() });
    let other = cosync::Op::Content { s, r: 3, c: 3, v: Some(Box::new(cosync::CellData { v: cosync::Val::Text("theirs".into()), ..Default::default() })), adj: vec![] };
    app.live_remote_for_test(&[cosync::Op::Format { s, r: 0, c: 0, st: bold }, other]);
    app.undo();
    check(&mut app, "undo");
    assert_eq!(at(&app, 0, 0), old, "my value is taken back");
    assert!(app.wb.sheets[0].get(0, 0).is_some_and(|x| app.wb.style(x.style).bold), "their format stays");
    assert_eq!(at(&app, 3, 3), "theirs", "their other edit stays");
}

/// A value and a format change are different operations (so they can't overwrite each other);
/// a command changing both (paste, clear) is one whole cell.
#[test]
fn value_and_format_changes_are_separate_operations() {
    use cosync::Op;
    let (_ctx, mut app) = super::data_ui::sample_app();
    app.live_for_test();
    // The operations the last command produced.
    let last = |app: &mut App, from: &mut usize| -> Vec<Op> {
        app.live_flush();
        let ops = app.live_pending_ops();
        let new = ops[*from..].to_vec();
        *from = ops.len();
        new
    };
    let mut n = 0;
    type_at(&mut app, 30, 0, "plain");
    let ops = last(&mut app, &mut n);
    assert!(matches!(&ops[..], [Op::Content { .. }]), "{ops:?}");
    app.select(30, 0, false);
    app.toggle_bold();
    let ops = last(&mut app, &mut n);
    assert!(matches!(&ops[..], [Op::Format { .. }]), "{ops:?}");
    // Clearing a formatted cell changes both parts: one whole cell.
    app.select(30, 0, false);
    app.clear(true, true);
    let ops = last(&mut app, &mut n);
    assert!(matches!(&ops[..], [Op::Cell { v: None, .. }]), "{ops:?}");
    assert!(app.live_errors().is_empty());
}

/// Against a protocol 1 server (its Welcome has no `proto`) a new app keeps working: it sends only
/// whole `Cell` operations, including what was made before the Welcome arrived.
#[test]
fn against_a_protocol_1_server_only_whole_cells_are_sent() {
    use cosync::Op;
    let (_ctx, mut app) = super::data_ui::sample_app();
    app.live_for_test();
    // Made before the Welcome: still a Content / Format at this point.
    type_at(&mut app, 30, 0, "early");
    app.select(31, 0, false);
    app.toggle_bold();
    app.live_flush();
    assert!(app.live_pending_ops().iter().any(|o| matches!(o, Op::Content { .. } | Op::Format { .. })));
    let you = cosync::Peer { id: 1, name: "Me".into(), user: "me".into(), editor: true, ..Default::default() };
    // (A protocol 1 Welcome has no `proto` field.)
    let welcome: cosync::ServerMsg = serde_json::from_value(serde_json::json!({ "t": "welcome", "seq": 0, "you": you, "peers": [] })).unwrap();
    app.live_event_for_test(crate::live::conn::Event::Msg(welcome));
    let all_cells = |app: &App| {
        let ops = app.live_pending_ops();
        !ops.is_empty() && ops.iter().all(|o| matches!(o, Op::Cell { .. }))
    };
    assert!(all_cells(&app), "{:?}", app.live_pending_ops());
    // And what is made afterwards: value, format, both, clear.
    type_at(&mut app, 32, 0, "later");
    app.select(32, 0, false);
    app.toggle_bold();
    app.apply_style(|s| s.fill = Some(0x112233));
    app.clear(true, false);
    app.select(30, 0, false);
    app.clear(true, true);
    app.live_flush();
    assert!(all_cells(&app), "{:?}", app.live_pending_ops());
    // The whole cell carries the full current state: the bold, filled, now empty cell keeps its format.
    let cells: Vec<Op> = app.live_pending_ops();
    let last32 = cells.iter().rev().find(|o| matches!(o, Op::Cell { r: 32, .. })).unwrap();
    assert!(matches!(last32, Op::Cell { v: Some(d), .. } if d.v.is_empty() && d.st.is_some()));
    assert!(app.live_errors().is_empty());
    // Such a server has no comments either.
    assert!(app.comment_add(1, 1, "hello").is_none());
    assert!(app.status.as_deref().is_some_and(|s| s.contains("can't share comments")), "{:?}", app.status);
}

/// Against the previous server (protocol 2: split cells, no comments) cells are sent as usual, and a
/// comment is refused with a message instead of sending operations that server would reject.
#[test]
fn against_a_protocol_2_server_comments_are_off() {
    use cosync::Op;
    let (_ctx, mut app) = super::data_ui::sample_app();
    app.live_for_test();
    let you = cosync::Peer { id: 1, name: "Me".into(), user: "me".into(), editor: true, ..Default::default() };
    let welcome: cosync::ServerMsg = serde_json::from_value(serde_json::json!({ "t": "welcome", "seq": 0, "you": you, "peers": [], "proto": 2 })).unwrap();
    app.live_event_for_test(crate::live::conn::Event::Msg(welcome));
    type_at(&mut app, 30, 0, "typed");
    app.live_flush();
    assert!(app.live_pending_ops().iter().all(|o| matches!(o, Op::Content { .. })), "{:?}", app.live_pending_ops());
    assert!(app.comment_add(1, 1, "hello").is_none());
    assert!(app.status.as_deref().is_some_and(|s| s.contains("can't share comments")), "{:?}", app.status);
    assert!(app.live_pending_ops().iter().all(|o| !o.is_thread_op()));
    // The same app against a current server can.
    let welcome: cosync::ServerMsg = serde_json::from_value(serde_json::json!({ "t": "welcome", "seq": 0, "you": you, "peers": [], "proto": 3 })).unwrap();
    app.live_event_for_test(crate::live::conn::Event::Msg(welcome));
    assert!(app.comment_add(1, 1, "hello").is_some());
    app.live_flush();
    assert!(app.live_pending_ops().iter().any(|o| matches!(o, Op::ThreadAdd { .. })));
    assert!(app.live_errors().is_empty());
}

/// An editor made a viewer while changes are unsent: the session ends (instead of resending
/// them on every reconnect), the changes stay in the window, and the message bar offers to save
/// a copy. The same whichever way the server says it: the `role` message while connected, the
/// Welcome after reconnecting, or refusing the changes (4403).
#[test]
fn becoming_a_viewer_with_unsent_changes_ends_the_session() {
    use crate::live::conn::Event;
    for how in ["role", "welcome", "refused"] {
        let (_ctx, mut app) = super::data_ui::sample_app();
        app.live_for_test();
        type_at(&mut app, 0, 0, "unsent");
        app.live_flush();
        assert!(!app.live_pending_ops().is_empty());
        match how {
            // Told while connected.
            "role" => app.live_event_for_test(Event::Msg(cosync::ServerMsg::Role { role: "view".into() })),
            "welcome" => {
                // Reconnecting after access was changed: the server says we are a viewer.
                let you = cosync::Peer { id: 1, name: "Me".into(), user: "me".into(), color: 0, editor: false, manager: false, agent: false, presence: None };
                app.live_event_for_test(Event::Msg(cosync::ServerMsg::Welcome { seq: 0, you, peers: vec![], proto: cosync::PROTO }));
            }
            // Sending them was refused.
            _ => app.live_event_for_test(Event::Closed { code: Some(cosync::close::READ_ONLY), http: None, reason: "view-only".into() }),
        }
        let (stopped, retry) = app.live_stopped();
        assert!(stopped.is_some_and(|m| m.contains("view-only")), "the user is told ({how})");
        assert!(!retry, "no reconnect that would resend them ({how})");
        assert!(!app.live_editor() && !app.live_quiet());
        assert!(!app.live_pending_ops().is_empty(), "the changes are kept");
        assert_eq!(crate::numfmt::value_to_text(app.wb.sheets[app.wb.active].display_value(0, 0)), "unsent");
        let n = app.live_notice_for_test().expect("the message bar");
        assert!(n.save_copy && n.text.contains("you can now only view this workbook"), "{n:?} ({how})");
        // Typing is refused now.
        app.select(1, 0, false);
        app.begin_edit(Some("more".into()), EditMode::Enter);
        assert!(app.edit.is_none(), "read-only ({how})");
    }
}

/// Made a viewer while connected with nothing unsent: read-only at once with the message bar,
/// still connected (others' changes keep coming); edit access back: editable again.
#[test]
fn live_role_changes_switch_read_only_and_back() {
    use crate::live::conn::Event;
    let (_ctx, mut app) = super::data_ui::sample_app();
    app.live_for_test();
    let name = app.wb.file_name();
    assert_eq!(app.cloud_autosave_state().0, "AutoSave On");
    app.live_event_for_test(Event::Msg(cosync::ServerMsg::Role { role: "view".into() }));
    assert!(!app.live_editor(), "read-only at once");
    assert_eq!(app.live_notice_for_test(), Some(super::live_ui::Notice { text: format!("{name}: you can now only view this workbook"), save_copy: false }));
    assert_eq!(app.live_stopped(), (None, false), "the session goes on");
    let (label, on, tip) = app.cloud_autosave_state();
    assert!(label == "Read-only" && !on && tip.contains("only view"), "AutoSave doesn't claim to be on: {label} / {tip}");
    assert!(!app.cloud_share_enabled(), "only editors can share (the server refuses viewers)");
    assert_eq!(app.cloud_share_off(), Some("Only editors can share"));
    let before = app.wb.clone();
    app.select_range(rg("2:3"));
    app.delete_rows();
    app.begin_edit(Some("nope".into()), EditMode::Enter);
    assert!(app.edit.is_none());
    app.live_flush();
    assert!(app.live_pending_ops().is_empty() && crate::live::compare(&before, &app.wb).is_empty());

    app.live_event_for_test(Event::Msg(cosync::ServerMsg::Role { role: "edit".into() }));
    assert!(app.live_editor() && app.live_notice_for_test().is_none());
    assert!(app.cloud_share_enabled(), "editors share again");
    type_at(&mut app, 0, 0, "editable again");
    app.live_flush();
    assert!(!app.live_pending_ops().is_empty());
}

/// A cell being typed in, or a drag not recorded yet, when access changes: kept in the window
/// as an unsent change (never thrown away), so the session ends and Save a copy is offered —
/// for view-only and for removed access alike.
#[test]
fn typing_or_dragging_when_access_changes_is_kept() {
    use crate::live::conn::Event;
    let at = |app: &App, r: u32, c: u32| crate::numfmt::value_to_text(app.wb.sheets[app.wb.active].display_value(r, c));
    for ev in [
        Event::Msg(cosync::ServerMsg::Role { role: "view".into() }),
        Event::Msg(cosync::ServerMsg::Revoked { reason: "Your access to this workbook was removed.".into() }),
        Event::Closed { code: None, http: Some(401), reason: "HTTP 401".into() },
    ] {
        let what = format!("{ev:?}");
        let (_ctx, mut app) = super::data_ui::sample_app();
        app.live_for_test();
        assert!(app.live_pending_ops().is_empty());
        app.select(0, 0, false);
        app.begin_edit(Some("half typed".into()), EditMode::Enter);
        app.live_event_for_test(ev);
        assert!(app.edit.is_none() && !app.live_editor(), "{what}");
        assert_eq!(at(&app, 0, 0), "half typed", "the typed text is in the cell ({what})");
        assert!(!app.live_pending_ops().is_empty(), "and counts as unsent ({what})");
        assert!(app.live_stopped().0.is_some() && !app.live_stopped().1, "{what}");
        assert!(app.live_notice_for_test().is_some_and(|n| n.save_copy), "Save a copy is offered ({what})");
    }

    // A drag in progress (recorded, not yet turned into operations) counts too.
    let (_ctx, mut app) = super::data_ui::sample_app();
    app.live_for_test();
    let si = app.wb.active;
    app.live_record(&[super::Scope::Range(si, Range::cell(5, 5))]);
    let c = crate::model::cell_from_input(&mut app.wb, "dragged", 0);
    app.wb.sheets[si].set(5, 5, c);
    app.live_event_for_test(Event::Msg(cosync::ServerMsg::Role { role: "view".into() }));
    assert_eq!(at(&app, 5, 5), "dragged");
    assert!(!app.live_pending_ops().is_empty() && app.live_notice_for_test().is_some_and(|n| n.save_copy));
}

/// Undo and redo change nothing while view-only, and the undo stack stays as it was.
#[test]
fn undo_while_view_only_keeps_the_stack() {
    use crate::live::conn::Event;
    let (_ctx, mut app) = super::data_ui::sample_app();
    app.live_for_test();
    type_at(&mut app, 0, 0, "one");
    type_at(&mut app, 1, 0, "two");
    app.undo();
    assert!(app.live_can_undo(false) && app.live_can_redo());
    let (undo, before) = (app.live_undo_len(), app.wb.clone());
    app.live_event_for_test(Event::Msg(cosync::ServerMsg::Role { role: "view".into() }));
    assert!(!app.live_can_undo(false) && !app.live_can_redo(), "nothing to undo for a viewer");
    for _ in 0..3 {
        app.undo();
        app.redo();
    }
    assert_eq!(app.live_undo_len(), undo, "the stack isn't drained");
    assert!(crate::live::compare(&before, &app.wb).is_empty());
}

/// Signed out while live (401 on reconnecting): read-only with "Sign in again" and Save a copy,
/// instead of piling up edits that can't be sent; AutoSave and Share say it isn't connected.
#[test]
fn signed_out_while_live_is_read_only() {
    use crate::live::conn::Event;
    let (_ctx, mut app) = super::data_ui::sample_app();
    app.live_for_test();
    type_at(&mut app, 0, 0, "before");
    app.live_event_for_test(Event::Closed { code: None, http: Some(401), reason: "HTTP 401".into() });
    let (stopped, retry) = app.live_stopped();
    assert!(stopped.as_deref().is_some_and(|s| s.contains("Sign in again")) && !retry, "{stopped:?}");
    assert!(!app.live_editor() && app.live_notice_for_test().is_some_and(|n| n.save_copy));
    app.select(2, 2, false);
    app.begin_edit(Some("x".into()), EditMode::Enter);
    assert!(app.edit.is_none(), "no more edits");
    let (label, on, _) = app.cloud_autosave_state();
    assert!(label == "Not connected" && !on);
    assert!(!app.cloud_share_enabled(), "Share is off once the session ended");
    assert!(app.cloud_share_off().is_some_and(|t| t.starts_with("Not connected")));
}

/// Access removed (the `revoked` message, or only the 4401 close, or a refused reconnect): the
/// session ends for good, the workbook stays open read-only, and a copy is offered.
#[test]
fn removed_access_ends_the_session_read_only() {
    use crate::live::conn::Event;
    let revoked = Event::Msg(cosync::ServerMsg::Revoked { reason: "This workbook was moved to the trash.".into() });
    let closed = Event::Closed { code: Some(cosync::close::REVOKED), http: None, reason: "Your access to this workbook was removed.".into() };
    let refused = Event::Closed { code: None, http: Some(404), reason: "HTTP 404".into() };
    for (ev, text) in [
        (revoked, "Your access to this file was removed (This workbook was moved to the trash)."),
        (closed, "Your access to this file was removed."),
        (refused, "Your access to this file was removed (The workbook is no longer shared with you, or it was deleted)."),
    ] {
        let (_ctx, mut app) = super::data_ui::sample_app();
        app.live_for_test();
        type_at(&mut app, 0, 0, "mine");
        app.live_event_for_test(ev);
        assert_eq!(app.live_stopped(), (Some(text.to_string()), false), "no reconnect");
        assert_eq!(app.live_notice_for_test(), Some(super::live_ui::Notice { text: text.into(), save_copy: true }));
        assert!(!app.live_editor());
        assert_eq!(crate::numfmt::value_to_text(app.wb.sheets[app.wb.active].display_value(0, 0)), "mine", "the workbook stays");
        app.select(3, 3, false);
        app.begin_edit(Some("x".into()), EditMode::Enter);
        assert!(app.edit.is_none(), "read-only");
        assert_eq!(app.cloud_autosave_state().0, "Not connected");
        assert!(!app.cloud_share_enabled());
        // A later close or reconnect attempt changes nothing.
        app.poll_live();
        assert_eq!(app.live_stopped().1, false);
        // Save a copy: File > Save As.
        app.live_save_copy();
        assert!(app.backstage == Some(super::chrome::Backstage::SaveAs));
    }
}

/// A lost connection is retried after about 2, 4, then 8 s (75-100% of that, so windows don't
/// all come back at once), never longer.
#[test]
fn reconnecting_waits_at_most_8_seconds() {
    use crate::live::conn::Event;
    let (_ctx, mut app) = super::data_ui::sample_app();
    app.live_for_test();
    let mut waits = Vec::new();
    for _ in 0..40 {
        app.live_event_for_test(Event::Closed { code: None, http: None, reason: "Connection lost".into() });
        waits.push(app.live_retry_in().expect("a reconnect is scheduled").as_millis() as u64);
    }
    for (i, w) in waits.iter().enumerate() {
        let full = 1000u64 << (i + 1).min(3);
        assert!(*w <= full && *w + 50 >= full * 3 / 4, "attempt {i}: {w} ms (of {full})");
    }
    assert!(waits[2..].iter().any(|w| *w != waits[2]), "jitter: {waits:?}");
}

#[test]
fn view_only_participants_change_nothing() {
    let (_ctx, mut app) = super::data_ui::sample_app();
    app.live_for_test();
    app.live.as_mut().unwrap().editor = false;
    let before = app.wb.clone();
    app.select(0, 0, false);
    app.begin_edit(Some("nope".into()), EditMode::Enter);
    assert!(app.edit.is_none());
    app.select_range(rg("2:3"));
    app.delete_rows();
    app.toggle_bold();
    app.add_sheet();
    app.live_flush();
    assert!(app.live_pending_ops().is_empty());
    assert!(crate::live::compare(&before, &app.wb).is_empty(), "{:?}", crate::live::compare(&before, &app.wb));
}

/// "Ann joined." and other passing live messages belong to the workbook they were about: a
/// New workbook or another opened file starts with a clean status bar.
#[test]
fn live_messages_are_cleared_by_new_or_open() {
    use crate::live::conn::Event;
    let join = Event::Msg(cosync::ServerMsg::Join { peer: cosync::Peer { id: 7, name: "PO Client B".into(), user: "pob".into(), color: 1, editor: true, manager: false, agent: false, presence: None } });
    let (_ctx, mut app) = super::data_ui::sample_app();
    app.live_for_test();
    app.live_event_for_test(join);
    assert_eq!(app.status.as_deref(), Some("PO Client B joined."));
    app.run_pending(super::Pending::New);
    assert!(app.live.is_none() && app.status.is_none(), "New: {:?}", app.status);

    let dir = super::cloud_ui::TestDir::new("fo-sheets-status");
    std::fs::create_dir_all(&dir.0).unwrap();
    let file = dir.0.join("Other.xlsx");
    crate::io::save(&app.wb, &file).unwrap();
    let (_ctx, mut app) = super::data_ui::sample_app();
    app.live_for_test();
    app.status = Some("PO Client B left.".into());
    app.open_path(file);
    assert!(app.live.is_none() && app.status.is_none(), "Open: {:?}", app.status);
}

// ---------------------------------------------------------------------------
// Against a real server on 127.0.0.1 (throwaway data and accounts)
// ---------------------------------------------------------------------------

mod e2e {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use eframe::egui;
    use freedomoffice_server::config::Config;
    use freedomoffice_server::store::Store;

    use super::{rg, type_at};
    use crate::app::App;
    use crate::model::Range;
    use crate::cloud::{Http, ServerSession, Target, UploadError};

    /// Test-only password for the throwaway accounts.
    const PW: &str = "live-test-password";

    struct Client {
        ctx: egui::Context,
        app: App,
        session: Arc<ServerSession>,
        root: std::path::PathBuf,
    }

    fn client(url: &str, user: &str, dir: &std::path::Path) -> Client {
        let ctx = egui::Context::default();
        let mut app = App::with_context(&ctx, 0, None);
        app.dialog = None;
        let session = Arc::new(ServerSession::login(Box::new(Http::new()), url, user, PW, "test").unwrap());
        app.cloud_for_test(session.clone());
        Client { ctx, app, session, root: dir.join(format!("{user}-{}", crate::model::new_sheet_uid())) }
    }

    fn open(c: &mut Client, id: &str) {
        let item = c.session.item(id).unwrap();
        let doc = c.session.open(&item, &c.root).unwrap();
        assert!(doc.live.is_some(), "the server tells where the live session starts");
        c.app.cloud_opened_for_test(doc);
        assert!(c.app.live.is_some(), "opening a server document joins its live session");
    }

    fn pump(cs: &mut [&mut Client], what: &str, done: impl Fn(&[&mut Client]) -> bool) {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            for c in cs.iter_mut() {
                c.app.poll_live();
            }
            if done(cs) {
                return;
            }
            assert!(Instant::now() < end, "timed out waiting for: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Everyone connected, nothing pending and at the same sequence number.
    fn settle(cs: &mut [&mut Client], what: &str) {
        pump(cs, what, |cs| cs.iter().all(|c| c.app.live_quiet()) && cs.windows(2).all(|w| w[0].app.live_seq() == w[1].app.live_seq()));
    }

    fn assert_same(a: &Client, b: &Client, what: &str) {
        let (x, y) = (crate::live::contents(&a.app.wb), crate::live::contents(&b.app.wb));
        if x != y {
            let mut d: Vec<String> = x.iter().filter(|(k, v)| y.get(*k) != Some(*v)).map(|(k, v)| format!("{k:?}: {v} vs {:?}", y.get(k))).collect();
            d.extend(y.keys().filter(|k| !x.contains_key(*k)).map(|k| format!("{k:?} only in second: {}", y[k])));
            d.sort();
            panic!("{what}: workbooks differ:\n{}", d.join("\n"));
        }
        let diffs = crate::live::compare(&a.app.wb, &b.app.wb);
        assert!(diffs.is_empty(), "{what}: {diffs:#?}");
    }

    fn text(c: &Client, sheet: &str, r: u32, col: u32) -> String {
        let wb = &c.app.wb;
        let sh = &wb.sheets[wb.sheet_index(sheet).unwrap()];
        crate::numfmt::value_to_text(sh.display_value(r, col))
    }

    #[test]
    fn two_people_edit_one_workbook_live() {
        let dir = std::env::temp_dir().join(format!("fo-live-e2e-{}", freedomoffice_server::util::new_id()));
        let cfg = Config { data_dir: dir.join("data"), versions_to_keep: 50, ..Config::default() };
        {
            let st = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
            st.create_user("ann", "Ann Lee", "", PW, false).unwrap();
            st.create_user("ben", "Ben Ode", "", PW, false).unwrap();
            st.create_user("cam", "Cam Viewer", "", PW, false).unwrap();
        }
        let server = freedomoffice_server::spawn(cfg).unwrap();
        let url = server.url();

        // Ann saves a workbook to the server and shares it.
        let mut start = crate::model::Workbook::new();
        for r in 0..8 {
            let c = crate::model::cell_from_input(&mut start, &format!("{}", r + 1), 0);
            start.sheets[0].set(r, 0, c);
        }
        let file = dir.join("start.xlsx");
        std::fs::create_dir_all(&dir).unwrap();
        crate::io::save(&start, &file).unwrap();
        let mut a = client(&url, "ann", &dir);
        let id = a.session.upload_new("root", "Team.xlsx", &std::fs::read(&file).unwrap(), "fail").unwrap().id;
        a.session.grant(&id, "ben", true).unwrap();
        a.session.grant(&id, "cam", false).unwrap();
        let mut b = client(&url, "ben", &dir);
        open(&mut a, &id);
        open(&mut b, &id);
        settle(&mut [&mut a, &mut b], "both joined");
        // (Ben's Join can come after both are caught up.)
        pump(&mut [&mut a, &mut b], "Ann sees Ben", |cs| !cs[0].app.live_peer_names().is_empty());
        assert_eq!(a.app.live_peer_names(), ["Ben Ode"]);
        assert!(a.app.cloud.autosave && a.app.cloud_title_suffix().contains("Saved to"));

        // At the same time: Ann types in A5 while Ben inserts a row above it, and Ben types
        // further down. Neither has seen the other's change.
        type_at(&mut a.app, 4, 0, "alpha");
        b.app.select_range(rg("2:2"));
        b.app.insert_rows();
        type_at(&mut b.app, 9, 0, "=SUM(A1:A9)");
        settle(&mut [&mut a, &mut b], "concurrent insert and typing");
        assert_same(&a, &b, "after a row insert while typing below it");
        assert_eq!(text(&a, "Sheet1", 5, 0), "alpha", "Ann's cell moved down with the inserted row");
        assert_eq!(text(&b, "Sheet1", 9, 0), "31", "formulas recalculate everywhere");

        // Same cell at once: the later commit wins everywhere; others' cursors are shown.
        type_at(&mut a.app, 0, 1, "ann");
        type_at(&mut b.app, 0, 1, "ben");
        settle(&mut [&mut a, &mut b], "same cell");
        assert_same(&a, &b, "same cell");
        assert!(a.app.live.as_ref().unwrap().peers.values().any(|p| p.peer.presence.is_some()));

        // Structure, formatting and sheets from both sides.
        a.app.select_range(rg("C:C"));
        a.app.delete_cols();
        b.app.select_range(rg("A1:B3"));
        b.app.toggle_bold();
        b.app.merge(2);
        a.app.add_sheet();
        let n = a.app.wb.sheets.len();
        assert!(a.app.rename_sheet(n - 1, "Notes").is_ok());
        type_at(&mut a.app, 0, 0, "=Sheet1!A6");
        b.app.select_range(rg("4:5"));
        b.app.delete_rows();
        settle(&mut [&mut a, &mut b], "structure and sheets");
        assert_same(&a, &b, "structure and sheets");

        // Ben renames Sheet1 while Ann moves her new sheet first.
        let i = b.app.wb.sheet_index("Sheet1").unwrap();
        assert!(b.app.rename_sheet(i, "Plan").is_ok());
        let notes = a.app.wb.sheet_index("Notes").unwrap();
        a.app.move_sheet_to(notes, 0);
        settle(&mut [&mut a, &mut b], "rename and move");
        assert_same(&a, &b, "rename and move");
        assert_eq!(a.app.wb.sheets[0].name, "Notes");
        let f = |c: &Client| c.app.wb.sheets[0].get(0, 0).and_then(|x| x.formula.as_ref()).map(|f| f.text.clone());
        assert_eq!(f(&a).as_deref(), Some("Plan!A4"), "references follow the rename and the deleted rows");

        // Per-user undo: Ann undoes her last change; Ben's changes stay.
        a.app.undo();
        settle(&mut [&mut a, &mut b], "undo");
        assert_same(&a, &b, "undo");
        assert_eq!(b.app.wb.sheets.iter().map(|s| s.name.as_str()).next(), Some("Plan"), "undoing Ann's move keeps Ben's rename");

        // Ben's connection drops; both keep editing; he reconnects and everything converges.
        b.app.live_drop_for_test();
        type_at(&mut b.app, 20, 0, "offline edit");
        b.app.select_range(rg("1:1"));
        b.app.insert_rows();
        let plan = a.app.wb.sheet_index("Plan").unwrap();
        a.app.switch_sheet(plan);
        type_at(&mut a.app, 20, 1, "while ben was away");
        pump(&mut [&mut a, &mut b], "ann's change", |cs| cs[0].app.live_quiet());
        assert!(b.app.cloud_title_suffix().contains("Offline"));
        b.app.live_reconnect_for_test();
        settle(&mut [&mut a, &mut b], "reconnect");
        assert_same(&a, &b, "after reconnecting");
        assert_eq!(text(&a, "Plan", 21, 0), "offline edit");

        // A plain (non-live) save is refused while the session is live.
        let doc = b.app.cloud.doc.clone().unwrap();
        match b.session.upload(&doc.item_id, None, b"stale bytes") {
            Err(UploadError::Failed(m)) => assert!(m.contains("edited live"), "{m}"),
            r => panic!("{r:?}"),
        }

        // A viewer joins, sees everything, and can't change anything.
        let mut c = client(&url, "cam", &dir);
        open(&mut c, &id);
        settle(&mut [&mut a, &mut b, &mut c], "viewer joined");
        assert!(!c.app.live_editor());
        assert_same(&a, &c, "viewer's copy");
        c.app.select(0, 0, false);
        c.app.begin_edit(Some("vandal".into()), super::EditMode::Enter);
        assert!(c.app.edit.is_none(), "viewers can't start editing");
        c.app.select_range(rg("1:3"));
        c.app.delete_rows();
        settle(&mut [&mut a, &mut b, &mut c], "viewer tried");
        assert_same(&a, &c, "viewer changed nothing");

        // Snapshot: the server's version holds everything; a late joiner gets the same workbook.
        a.app.live_snapshot();
        let seq = a.app.live_seq();
        pump(&mut [&mut a, &mut b, &mut c], "snapshot", |cs| cs[0].app.live_snap_seq() == seq);
        type_at(&mut b.app, 30, 0, "after the snapshot");
        settle(&mut [&mut a, &mut b, &mut c], "after snapshot");
        let versions = a.session.versions(&id).unwrap();
        assert!(versions.len() >= 2, "the snapshot is a version in the history");
        let mut d = client(&url, "ann", &dir);
        open(&mut d, &id);
        settle(&mut [&mut a, &mut b, &mut c, &mut d], "late joiner");
        assert_same(&a, &d, "snapshot plus replay");
        assert_same(&b, &c, "everyone");
        eprintln!("(repaired, concurrent episodes): ann {:?}, ben {:?}", a.app.live_repaired(), b.app.live_repaired());
        assert!(d.app.wb.sheets.iter().zip(&a.app.wb.sheets).all(|(x, y)| x.uid == y.uid), "sheet ids survive the snapshot");

        // Everyone leaves; leaving saves a version (one for the same state), and plain saves
        // work again.
        drop(c);
        drop(d);
        let before = b.session.versions(&id).unwrap().len();
        a.app.live_leave(true);
        pump(&mut [&mut b], "the others left", |cs| cs[0].app.live_peer_names().is_empty());
        b.app.live_leave(true);
        assert_eq!(b.session.versions(&id).unwrap().len(), before + 1, "leaving stores one version of the latest state");
        std::thread::sleep(Duration::from_millis(300));
        let item = b.session.item(&id).unwrap();
        assert!(b.session.upload(&id, Some(&item.etag), b"plain save").is_ok());
        let _ = (Target::MyFiles, &a.ctx, &b.ctx);
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Everyone closes at once, right after typing (nobody has seen the others leave, the last
    /// change isn't acknowledged yet): the server's file still gets the latest content and
    /// date, for share links and clients that aren't live.
    #[test]
    fn closing_everything_at_once_leaves_the_file_up_to_date() {
        let dir = std::env::temp_dir().join(format!("fo-live-close-{}", freedomoffice_server::util::new_id()));
        let cfg = Config { data_dir: dir.join("data"), ..Config::default() };
        {
            let st = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
            st.create_user("ann", "Ann Lee", "", PW, false).unwrap();
            st.create_user("ben", "Ben Ode", "", PW, false).unwrap();
        }
        let server = freedomoffice_server::spawn(cfg).unwrap();
        let url = server.url();
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("start.xlsx");
        crate::io::save(&crate::model::Workbook::new(), &file).unwrap();
        let mut a = client(&url, "ann", &dir);
        let id = a.session.upload_new("root", "Close.xlsx", &std::fs::read(&file).unwrap(), "fail").unwrap().id;
        a.session.grant(&id, "ben", true).unwrap();
        let mut b = client(&url, "ben", &dir);
        open(&mut a, &id);
        open(&mut b, &id);
        settle(&mut [&mut a, &mut b], "joined");
        type_at(&mut b.app, 1, 0, "ben's");
        settle(&mut [&mut a, &mut b], "ben typed");
        let before = a.session.item(&id).unwrap();
        let versions = a.session.versions(&id).unwrap().len();
        std::thread::sleep(Duration::from_millis(1100));

        type_at(&mut a.app, 0, 0, "last word");
        // Ann closes her window: it stays responsive (each frame returns at once) while her
        // change is acknowledged and the snapshot uploaded, then closes. Ben's app exits.
        let t = Instant::now();
        assert!(a.app.live_close_needs_wait());
        a.app.live_begin_close();
        assert!(a.app.modal_open(), "no editing while closing");
        assert!(a.app.status.as_deref().is_some_and(|s| s.contains("Saving")));
        while !a.app.live_close_done() {
            let f = Instant::now();
            a.app.poll_live();
            a.app.live_poll_close();
            assert!(f.elapsed() < Duration::from_millis(500), "a frame while closing blocked");
            assert!(t.elapsed() < Duration::from_secs(10), "closing doesn't hang");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(a.app.live.is_none());
        b.app.live_leave(true);
        assert!(t.elapsed() < Duration::from_secs(10), "closing doesn't hang");

        let after = a.session.item(&id).unwrap();
        assert_ne!(after.modified, before.modified, "the file's date moved");
        assert_eq!(a.session.versions(&id).unwrap().len(), versions + 1, "one version");
        let doc = a.session.open(&after, &dir.join("check")).unwrap();
        let wb = crate::io::load(&doc.local).unwrap();
        let cell = |r, c| crate::numfmt::value_to_text(wb.sheets[0].display_value(r, c));
        assert_eq!((cell(0, 0), cell(1, 0)), ("last word".to_string(), "ben's".to_string()), "the stored file has every change");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Closing while a cell is still being typed in: the text counts as typed (it reaches the
    /// server's file) instead of being thrown away.
    #[test]
    fn closing_while_typing_keeps_the_typed_text() {
        let dir = std::env::temp_dir().join(format!("fo-live-typing-{}", freedomoffice_server::util::new_id()));
        let cfg = Config { data_dir: dir.join("data"), ..Config::default() };
        {
            let st = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
            st.create_user("ann", "Ann Lee", "", PW, false).unwrap();
        }
        let server = freedomoffice_server::spawn(cfg).unwrap();
        let url = server.url();
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("start.xlsx");
        crate::io::save(&crate::model::Workbook::new(), &file).unwrap();
        let mut a = client(&url, "ann", &dir);
        let id = a.session.upload_new("root", "Typing.xlsx", &std::fs::read(&file).unwrap(), "fail").unwrap().id;
        open(&mut a, &id);
        settle(&mut [&mut a], "joined");

        a.app.select(2, 1, false);
        a.app.begin_edit(Some("half typed".into()), super::EditMode::Enter);
        a.app.live_begin_close();
        assert!(a.app.edit.is_none(), "the entry was committed");
        let t = Instant::now();
        while !a.app.live_close_done() {
            a.app.poll_live();
            a.app.live_poll_close();
            assert!(t.elapsed() < Duration::from_secs(10), "closing doesn't hang");
            std::thread::sleep(Duration::from_millis(5));
        }
        let after = a.session.item(&id).unwrap();
        let doc = a.session.open(&after, &dir.join("check")).unwrap();
        let wb = crate::io::load(&doc.local).unwrap();
        assert_eq!(crate::numfmt::value_to_text(wb.sheets[0].display_value(2, 1)), "half typed");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The connection drops while closing waits for the last change: the window doesn't close
    /// silently (the save prompt says why), and exiting anyway keeps the recovery copy.
    #[test]
    fn closing_with_an_unconfirmed_change_never_loses_it_silently() {
        let dir = std::env::temp_dir().join(format!("fo-live-unconf-{}", freedomoffice_server::util::new_id()));
        let cfg = Config { data_dir: dir.join("data"), ..Config::default() };
        {
            let st = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
            st.create_user("ann", "Ann Lee", "", PW, false).unwrap();
        }
        let server = freedomoffice_server::spawn(cfg).unwrap();
        let url = server.url();
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("start.xlsx");
        crate::io::save(&crate::model::Workbook::new(), &file).unwrap();
        let mut a = client(&url, "ann", &dir);
        let id = a.session.upload_new("root", "Unconf.xlsx", &std::fs::read(&file).unwrap(), "fail").unwrap().id;
        open(&mut a, &id);
        settle(&mut [&mut a], "joined");
        let recovery = dir.join("recovery").join("Recovered.xlsx");
        a.app.recovery_path = Some(recovery.clone());

        type_at(&mut a.app, 0, 0, "unconfirmed");
        assert!(a.app.live_close_needs_wait());
        a.app.live_begin_close();
        // The connection is lost before the acknowledgement is handled.
        a.app.live_drop_for_test();
        for _ in 0..20 {
            a.app.poll_live();
            a.app.live_poll_close();
        }
        assert!(!a.app.live_close_done(), "the window must not close silently");
        assert!(matches!(a.app.dialog, Some(super::super::dialogs::Dialog::SavePrompt)), "the save prompt is shown");
        assert!(a.app.status.as_deref().is_some_and(|s| s.contains("didn't confirm")), "{:?}", a.app.status);
        assert!(!a.app.live_close_needs_wait(), "closing again doesn't wait again");

        // The app exits anyway (Don't Save, or the session ends): the recovery copy stays.
        eframe::App::on_exit(&mut a.app, None);
        let wb = crate::io::load(&recovery).expect("recovery copy kept");
        assert_eq!(crate::numfmt::value_to_text(wb.sheets[0].display_value(0, 0)), "unconfirmed");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The owner changes Ben's access while he is in the workbook: view-only (read-only at once,
    /// still live), edit again, then removed (the session ends, the workbook stays read-only
    /// with "Save a copy").
    #[test]
    fn access_changes_reach_a_live_window_at_once() {
        let dir = std::env::temp_dir().join(format!("fo-live-role-{}", freedomoffice_server::util::new_id()));
        let cfg = Config { data_dir: dir.join("data"), ..Config::default() };
        {
            let st = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
            st.create_user("ann", "Ann Lee", "", PW, false).unwrap();
            st.create_user("ben", "Ben Ode", "", PW, false).unwrap();
        }
        let server = freedomoffice_server::spawn(cfg).unwrap();
        let url = server.url();
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("start.xlsx");
        crate::io::save(&crate::model::Workbook::new(), &file).unwrap();
        let mut a = client(&url, "ann", &dir);
        let id = a.session.upload_new("root", "Roles.xlsx", &std::fs::read(&file).unwrap(), "fail").unwrap().id;
        a.session.grant(&id, "ben", true).unwrap();
        let mut b = client(&url, "ben", &dir);
        open(&mut a, &id);
        open(&mut b, &id);
        settle(&mut [&mut a, &mut b], "joined");
        type_at(&mut b.app, 0, 0, "ben was here");
        settle(&mut [&mut a, &mut b], "ben typed");

        // View only: once the owner's change has answered, Ben's window is told.
        a.session.grant(&id, "ben", false).unwrap();
        pump(&mut [&mut b], "ben is a viewer", |cs| !cs[0].app.live_editor());
        let n = b.app.live_notice_for_test().expect("message bar");
        assert_eq!((n.text.as_str(), n.save_copy), ("Roles.xlsx: you can now only view this workbook", false));
        assert_eq!(b.app.live_stopped(), (None, false));
        assert!(b.app.cloud_title_suffix().contains("Read-only"));
        let _ = b.ctx.run(egui::RawInput::default(), |ctx| b.app.frame(ctx));
        b.app.select(1, 0, false);
        b.app.begin_edit(Some("refused".into()), super::EditMode::Enter);
        assert!(b.app.edit.is_none());
        type_at(&mut a.app, 2, 0, "ann still edits");
        settle(&mut [&mut a, &mut b], "a viewer still sees changes");
        assert_same(&a, &b, "viewer");

        // Edit access back.
        a.session.grant(&id, "ben", true).unwrap();
        pump(&mut [&mut b], "ben edits again", |cs| cs[0].app.live_editor());
        assert!(b.app.live_notice_for_test().is_none());
        type_at(&mut b.app, 3, 0, "editing again");
        settle(&mut [&mut a, &mut b], "ben edits again");
        assert_same(&a, &b, "editor again");

        // Removed: the session ends for good; the workbook stays, read-only; Save offers a copy.
        a.session.remove_permission(&id, "ben").unwrap();
        pump(&mut [&mut b], "ben's access removed", |cs| cs[0].app.live_stopped().0.is_some());
        assert_eq!(b.app.live_stopped(), (Some("Your access to this file was removed.".into()), false));
        assert!(b.app.live_notice_for_test().is_some_and(|n| n.save_copy));
        assert_eq!(text(&b, "Sheet1", 3, 0), "editing again");
        b.app.begin_edit(Some("refused".into()), super::EditMode::Enter);
        assert!(b.app.edit.is_none());
        assert!(!b.app.cloud_save(), "Save can't go to the server any more");
        assert!(b.app.backstage == Some(crate::app::chrome::Backstage::SaveAs), "it offers Save As (a copy)");
        b.app.backstage = None;
        let _ = b.ctx.run(egui::RawInput::default(), |ctx| b.app.frame(ctx));
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Random edits from two people with random message timing (real engine, real server):
    /// both end with the same workbook, and it matches what a late joiner gets.
    #[test]
    fn random_live_edits_converge() {
        let dir = std::env::temp_dir().join(format!("fo-live-rand-{}", freedomoffice_server::util::new_id()));
        let cfg = Config { data_dir: dir.join("data"), ..Config::default() };
        {
            let st = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
            st.create_user("ann", "Ann Lee", "", PW, false).unwrap();
            st.create_user("ben", "Ben Ode", "", PW, false).unwrap();
        }
        let server = freedomoffice_server::spawn(cfg).unwrap();
        let url = server.url();
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("start.xlsx");
        let mut start = crate::model::Workbook::new();
        start.sheets.push(crate::model::Sheet::new("Sheet2"));
        crate::io::save(&start, &file).unwrap();
        let mut a = client(&url, "ann", &dir);
        let id = a.session.upload_new("root", "Rand.xlsx", &std::fs::read(&file).unwrap(), "fail").unwrap().id;
        a.session.grant(&id, "ben", true).unwrap();
        let mut b = client(&url, "ben", &dir);
        open(&mut a, &id);
        open(&mut b, &id);
        settle(&mut [&mut a, &mut b], "joined");
        let mut seed: u64 = std::env::var("FO_LIVE_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x2545_F491_4F6C_DD1D);
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n.max(1)
        };
        for step in 0..240 {
            let c = if next(2) == 0 { &mut a } else { &mut b };
            let app = &mut c.app;
            app.dialog = None;
            let (r, col) = (next(12) as u32, next(6) as u32);
            match next(14) {
                0..=3 => type_at(app, r, col, &format!("v{step}")),
                4 => type_at(app, r, col, &format!("=SUM(A1:B{})+{step}", r + 2)),
                5 => type_at(app, r, col, "=Sheet2!A1&\"x\""),
                6 => {
                    app.select_range(Range::new(r, 0, r + next(2) as u32, crate::model::MAX_COLS - 1));
                    if next(2) == 0 { app.insert_rows() } else { app.delete_rows() }
                }
                7 => {
                    app.select_range(Range::new(0, col, crate::model::MAX_ROWS - 1, col));
                    if next(2) == 0 { app.insert_cols() } else { app.delete_cols() }
                }
                8 => {
                    app.select_range(Range::new(r, col, r + 1, col + 1));
                    app.toggle_bold();
                }
                9 => {
                    app.select_range(Range::new(r, col, r + 1, col + 1));
                    app.merge(if next(2) == 0 { 2 } else { 3 });
                }
                10 => {
                    if next(3) == 0 && app.wb.sheets.len() > 1 {
                        let i = next(app.wb.sheets.len() as u64) as usize;
                        app.delete_sheet(i);
                    } else {
                        app.add_sheet();
                    }
                }
                11 => {
                    let i = next(app.wb.sheets.len() as u64) as usize;
                    let _ = app.rename_sheet(i, &format!("S{step}"));
                }
                12 => {
                    let n = app.wb.sheets.len();
                    app.move_sheet_to(next(n as u64) as usize, next(n as u64 + 1) as usize);
                }
                _ => {
                    let i = next(app.wb.sheets.len() as u64) as usize;
                    app.switch_sheet(i);
                    app.select_range(Range::new(0, col, 0, col));
                    app.set_col_width(40.0 + step as f32);
                }
            }
            // Messages are handled only now and then, so changes often cross on the way.
            if next(4) == 0 {
                std::thread::sleep(Duration::from_millis(next(3)));
                a.app.poll_live();
                b.app.poll_live();
            }
        }
        settle(&mut [&mut a, &mut b], "random edits");
        assert_same(&a, &b, "random edits");
        eprintln!("random: (repaired, concurrent episodes): ann {:?}, ben {:?}", a.app.live_repaired(), b.app.live_repaired());
        a.app.live_snapshot();
        let seq = a.app.live_seq();
        pump(&mut [&mut a, &mut b], "snapshot", |cs| cs[0].app.live_snap_seq() == seq);
        let mut d = client(&url, "ben", &dir);
        open(&mut d, &id);
        settle(&mut [&mut a, &mut b, &mut d], "late joiner");
        assert_same(&a, &d, "late joiner after random edits");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two throwaway accounts sharing one workbook with a few numbers in column A.
    fn two_people(name: &str) -> (freedomoffice_server::Running, std::path::PathBuf, Client, Client, String) {
        let dir = std::env::temp_dir().join(format!("fo-live-{name}-{}", freedomoffice_server::util::new_id()));
        let cfg = Config { data_dir: dir.join("data"), versions_to_keep: 50, ..Config::default() };
        {
            let st = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
            st.create_user("ann", "Ann Lee", "", PW, false).unwrap();
            st.create_user("ben", "Ben Ode", "", PW, false).unwrap();
            st.create_user("cam", "Cam Viewer", "", PW, false).unwrap();
        }
        let server = freedomoffice_server::spawn(cfg).unwrap();
        let url = server.url();
        let mut start = crate::model::Workbook::new();
        for r in 0..8 {
            let c = crate::model::cell_from_input(&mut start, &format!("{}", r + 1), 0);
            start.sheets[0].set(r, 0, c);
        }
        let file = dir.join("start.xlsx");
        std::fs::create_dir_all(&dir).unwrap();
        crate::io::save(&start, &file).unwrap();
        let mut a = client(&url, "ann", &dir);
        let id = a.session.upload_new("root", "Split.xlsx", &std::fs::read(&file).unwrap(), "fail").unwrap().id;
        a.session.grant(&id, "ben", true).unwrap();
        let mut b = client(&url, "ben", &dir);
        open(&mut a, &id);
        open(&mut b, &id);
        settle(&mut [&mut a, &mut b], "both joined");
        pump(&mut [&mut a, &mut b], "each sees the other", |cs| !cs[0].app.live_peer_names().is_empty() && !cs[1].app.live_peer_names().is_empty());
        (server, dir, a, b, id)
    }

    fn is_bold(c: &Client, r: u32, col: u32) -> bool {
        let sh = &c.app.wb.sheets[0];
        sh.get(r, col).is_some_and(|x| c.app.wb.style(x.style).bold)
    }

    /// A window that was offline while someone's snapshot cut the server's log is told to reload
    /// on reconnecting: it takes the latest version and puts its unsent changes on top.
    #[test]
    fn a_reload_request_keeps_the_unsent_changes() {
        let (server, dir, mut a, mut b, _) = two_people("reload");
        b.app.live_drop_for_test();
        type_at(&mut b.app, 6, 2, "typed offline");
        type_at(&mut a.app, 2, 1, "from a");
        pump(&mut [&mut a, &mut b], "a is acknowledged", |cs| cs[0].app.live_quiet());
        a.app.live_snapshot();
        let seq = a.app.live_seq();
        pump(&mut [&mut a, &mut b], "snapshot", |cs| cs[0].app.live_snap_seq() == seq);
        b.app.live_reconnect_for_test();
        settle(&mut [&mut a, &mut b], "reloaded and caught up");
        assert_same(&a, &b, "after the reload");
        for c in [&a, &b] {
            assert_eq!(text(c, "Sheet1", 2, 1), "from a");
            assert_eq!(text(c, "Sheet1", 6, 2), "typed offline");
        }
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A client in protocol 1 mode (as against an old server) and a normal one edit the same
    /// workbook through a real server: both converge; the old-style one sends only whole cells and
    /// is never told "replaced".
    #[test]
    fn a_protocol_1_mode_client_converges_with_a_current_one() {
        use cosync::Op;
        let (server, dir, mut a, mut b, _) = two_people("legacy");
        a.app.live_legacy_for_test();
        type_at(&mut a.app, 2, 1, "legacy value");
        a.app.live_flush();
        assert!(a.app.live_pending_ops().iter().all(|o| matches!(o, Op::Cell { .. })), "{:?}", a.app.live_pending_ops());
        b.app.select(2, 1, false);
        b.app.toggle_bold();
        a.app.select(3, 1, false);
        a.app.toggle_bold();
        // A's whole cell is committed first (B hasn't seen it), then B's value, which wins the
        // content part: without this the two arrive in either order on a loaded machine.
        a.app.live_flush();
        pump(&mut [&mut a], "a's bold is committed", |cs| cs[0].app.live_quiet());
        type_at(&mut b.app, 3, 1, "from b");
        settle(&mut [&mut a, &mut b], "mixed protocol edits");
        assert_same(&a, &b, "mixed protocol edits");
        assert_eq!(text(&a, "Sheet1", 3, 1), "from b");
        // The same cell at once: last commit wins, and the legacy client is told nothing.
        type_at(&mut a.app, 5, 1, "from a");
        type_at(&mut b.app, 5, 1, "from b");
        settle(&mut [&mut a, &mut b], "same cell");
        assert_same(&a, &b, "same cell");
        assert!(a.app.live_replaced_for_test().is_empty());
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A types a value while B makes the same cell bold, at the same time: both survive on both
    /// sides, and nobody is told anything was replaced (they edited different things).
    #[test]
    fn a_value_and_bold_on_one_cell_both_survive() {
        let (server, dir, mut a, mut b, _) = two_people("split");
        b.app.select(2, 1, false);
        type_at(&mut a.app, 2, 1, "typed by Ann");
        b.app.toggle_bold();
        settle(&mut [&mut a, &mut b], "value and bold");
        assert_same(&a, &b, "value and bold");
        for c in [&a, &b] {
            assert_eq!(text(c, "Sheet1", 2, 1), "typed by Ann");
            assert!(is_bold(c, 2, 1), "the cell is bold");
        }
        assert!(a.app.live_replaced_for_test().is_empty() && b.app.live_replaced_for_test().is_empty());
        // The other way round, with a formula, a fill, and then a clear: the content goes, the format stays.
        a.app.select(3, 1, false);
        type_at(&mut b.app, 3, 1, "=SUM(A1:A3)");
        a.app.apply_style(|s| s.fill = Some(0xFFEE00));
        settle(&mut [&mut a, &mut b], "formula and fill");
        assert_same(&a, &b, "formula and fill");
        assert_eq!(text(&a, "Sheet1", 3, 1), "6");
        assert!(a.app.wb.sheets[0].get(3, 1).is_some_and(|x| a.app.wb.style(x.style).fill == Some(0xFFEE00)));
        assert!(a.app.live_replaced_for_test().is_empty() && b.app.live_replaced_for_test().is_empty());
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// "Your change to B3 was replaced by Alice's": only for a concurrent edit of the same part
    /// of the cell, never for one made after seeing the first; and it can be restored.
    #[test]
    fn a_concurrent_value_tells_the_one_who_lost_and_a_later_one_does_not() {
        let (server, dir, mut a, mut b, _) = two_people("replaced");
        // Sequential: Ann types, Ben sees it, then types over it. Nothing is replaced behind anyone's back.
        type_at(&mut a.app, 2, 1, "first");
        settle(&mut [&mut a, &mut b], "sequential 1");
        type_at(&mut b.app, 2, 1, "second");
        settle(&mut [&mut a, &mut b], "sequential 2");
        assert_eq!(text(&a, "Sheet1", 2, 1), "second");
        assert!(a.app.live_replaced_for_test().is_empty(), "{:?}", a.app.live_replaced_for_test());
        assert!(b.app.live_replaced_for_test().is_empty());

        // At the same time: neither has seen the other's. Whoever the server committed last wins; the other is told.
        type_at(&mut a.app, 3, 1, "from Ann");
        type_at(&mut b.app, 3, 1, "from Ben");
        settle(&mut [&mut a, &mut b], "concurrent");
        assert_same(&a, &b, "concurrent");
        let winner = text(&a, "Sheet1", 3, 1);
        let (loser, won, lost_text, winner_name) = if winner == "from Ben" { (&mut a, &b, "from Ann", "Ben Ode") } else { (&mut b, &a, "from Ben", "Ann Lee") };
        assert!(won.app.live_replaced_for_test().is_empty(), "the winner is told nothing");
        let notes = loser.app.live_replaced_for_test();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0].0, format!("Your change to B4 was replaced by {winner_name}'s"));
        assert_eq!(notes[0].1, lost_text, "what was typed is kept");
        // Restoring puts it back as a new change everyone gets.
        loser.app.live_restore_for_test(0);
        assert!(loser.app.live_replaced_for_test().is_empty());
        let mut cs = [&mut a, &mut b];
        settle(&mut cs, "restored");
        assert_same(&a, &b, "restored");
        assert_eq!(text(&a, "Sheet1", 3, 1), lost_text);
        // Formats alone never conflict with values, but two formats do.
        a.app.select(5, 1, false);
        b.app.select(5, 1, false);
        a.app.toggle_bold();
        b.app.apply_style(|s| s.fill = Some(0x00FF00));
        settle(&mut [&mut a, &mut b], "two formats");
        assert_same(&a, &b, "two formats");
        let told = a.app.live_replaced_for_test().len() + b.app.live_replaced_for_test().len();
        assert_eq!(told, 1, "exactly the one whose formatting lost is told");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------------
    // M3: offline, restarts and journals
    // ------------------------------------------------------------------

    use cosync::journal::Journal;

    /// The program dies (nothing is sent, nothing is saved) and starts again with the same
    /// sign-in and data folder.
    fn reopen(c: Client, jdir: &std::path::Path) -> Client {
        let Client { ctx: _, app, session, root } = c;
        drop(app);
        let ctx = egui::Context::default();
        let mut app = App::with_context(&ctx, 0, None);
        app.dialog = None;
        app.cloud_for_test(session.clone());
        app.journal_dir = Some(jdir.to_path_buf());
        Client { ctx, app, session, root }
    }

    /// Like `pump`, with the cloud events (the document opens by itself at the start).
    fn pump_all(cs: &mut [&mut Client], what: &str, mut done: impl FnMut(&mut [&mut Client]) -> bool) {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            for c in cs.iter_mut() {
                c.app.poll_cloud(&c.ctx);
                c.app.poll_live();
            }
            if done(cs) {
                return;
            }
            assert!(Instant::now() < end, "timed out waiting for: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn texts(c: &Client) -> Vec<String> {
        let mut out = Vec::new();
        for sh in &c.app.wb.sheets {
            for (r, row) in &sh.rows {
                for col in row.keys() {
                    let t = crate::numfmt::value_to_text(sh.display_value(*r, *col));
                    if !t.is_empty() {
                        out.push(t);
                    }
                }
            }
        }
        out
    }

    fn count(c: &Client, t: &str) -> usize {
        texts(c).iter().filter(|x| *x == t).count()
    }

    /// Ben's changes made offline survive his app dying; the next start finds the journal,
    /// opens the workbook by itself, sends them once (a second restart finds nothing), says so,
    /// and the journal holds no credential and is private.
    #[test]
    fn unsent_changes_survive_a_restart_and_are_sent_exactly_once() {
        let (server, dir, mut a, mut b, id) = two_people("restart");
        let jdir = dir.join("journal");
        b.app.journal_dir = Some(jdir.clone());
        b.app.live_leave(true);
        open(&mut b, &id);
        settle(&mut [&mut a, &mut b], "ben reopened with a journal");
        assert!(Journal::all(&jdir).is_empty(), "nothing pending, nothing kept");

        b.app.live_drop_for_test();
        type_at(&mut b.app, 6, 2, "off 1");
        type_at(&mut b.app, 7, 2, "off 2");
        b.app.select_range(rg("3:3"));
        b.app.insert_rows();
        b.app.live_flush();
        let kept = Journal::all(&jdir);
        assert_eq!(kept.len(), 1, "the journal is written with every change");
        assert_eq!(kept[0].pending_changes(), 3);
        let file = kept[0].path_in(&jdir);
        let raw = std::fs::read_to_string(&file).unwrap();
        assert!(!raw.contains(b.session.token()), "no token in the journal");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // Ann edits meanwhile; then Ben's app dies.
        type_at(&mut a.app, 0, 4, "ann while away");
        pump(&mut [&mut a], "ann's change is in", |cs| cs[0].app.live_quiet());
        let mut b = reopen(b, &jdir);

        let mut told = false;
        pump_all(&mut [&mut a, &mut b], "ben's earlier changes are in", |cs| {
            told |= cs[1].app.status.as_deref().is_some_and(|s| s.contains("3 changes made offline were saved to the server"));
            cs[1].app.live_quiet() && cs[0].app.live_quiet() && cs[0].app.live_seq() == cs[1].app.live_seq() && told
        });
        assert_same(&a, &b, "after the restart");
        for c in [&a, &b] {
            assert_eq!(text(c, "Sheet1", 7, 2), "off 1", "the row inserted above moved it down by one");
            assert_eq!(text(c, "Sheet1", 8, 2), "off 2");
            assert_eq!(text(c, "Sheet1", 0, 4), "ann while away");
            assert_eq!((text(c, "Sheet1", 2, 0), text(c, "Sheet1", 3, 0)), (String::new(), "3".to_string()), "the row was inserted once");
        }
        assert!(Journal::all(&jdir).is_empty(), "everything acknowledged: no journal");

        // Another restart replays nothing.
        let before = crate::live::contents(&a.app.wb);
        let mut b = reopen(b, &jdir);
        for _ in 0..40 {
            b.app.poll_cloud(&b.ctx);
            b.app.poll_live();
            a.app.poll_live();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(b.app.live.is_none(), "no journal: nothing is opened by itself");
        assert_eq!(crate::live::contents(&a.app.wb), before, "no duplicates");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The batch was committed but its acknowledgement never reached the app that died: the
    /// restart recognises it instead of inserting the row a second time.
    #[test]
    fn a_batch_committed_before_the_crash_is_not_sent_again() {
        let (server, dir, mut a, mut b, id) = two_people("committed");
        let jdir = dir.join("journal");
        b.app.journal_dir = Some(jdir.clone());
        b.app.live_leave(true);
        open(&mut b, &id);
        settle(&mut [&mut a, &mut b], "ben reopened with a journal");
        let seq = a.app.live_seq();
        b.app.select_range(rg("3:3"));
        b.app.insert_rows();
        type_at(&mut b.app, 0, 6, "after the row");
        b.app.live_flush();
        assert!(Journal::all(&jdir).len() == 1 && b.app.live_pending_ops().len() >= 2);
        // Ben's app sends the first batch and dies before reading the answer.
        pump(&mut [&mut a], "ann sees ben's row", |cs| cs[0].app.live_seq() > seq);
        let mut b = reopen(b, &jdir);
        pump_all(&mut [&mut a, &mut b], "resumed", |cs| cs[1].app.live_quiet() && cs[0].app.live_quiet() && cs[0].app.live_seq() == cs[1].app.live_seq() && cs[1].app.live.is_some());
        assert_same(&a, &b, "after the restart");
        for c in [&a, &b] {
            assert_eq!((text(c, "Sheet1", 2, 0), text(c, "Sheet1", 3, 0)), (String::new(), "3".to_string()), "one row inserted, not two");
            assert_eq!(text(c, "Sheet1", 0, 6), "after the row");
        }
        assert!(Journal::all(&jdir).is_empty());
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Access removed while the app was closed: the changes are never dropped; they are in a
    /// copy that opens.
    #[test]
    fn access_lost_while_closed_keeps_the_changes_in_a_copy() {
        let (server, dir, mut a, mut b, id) = two_people("lost");
        let jdir = dir.join("journal");
        b.app.journal_dir = Some(jdir.clone());
        b.app.live_leave(true);
        open(&mut b, &id);
        settle(&mut [&mut a, &mut b], "joined");
        b.app.live_drop_for_test();
        type_at(&mut b.app, 6, 2, "mine 1");
        type_at(&mut b.app, 7, 2, "mine 2");
        b.app.live_flush();
        assert_eq!(Journal::all(&jdir).len(), 1);
        a.session.remove_permission(&id, "ben").unwrap();
        let mut b = reopen(b, &jdir);
        pump_all(&mut [&mut b], "the copy opens", |cs| cs[0].app.dialog.is_some());
        let Some(crate::app::dialogs::Dialog::Message { title, text: msg }) = &b.app.dialog else { panic!("a message, got {:?}", b.app.dialog.is_some()) };
        assert!(title.contains("not saved") && msg.contains("2 changes") && msg.contains("no longer shared"), "{title}: {msg}");
        let path = b.app.wb.path.clone().expect("the copy is open");
        assert!(path.starts_with(&jdir) && path.exists());
        assert!(Journal::all(&jdir).is_empty(), "the changes moved into the copy");
        assert!(b.app.live.is_none());
        assert_eq!((text(&b, "Sheet1", 6, 2), text(&b, "Sheet1", 7, 2)), ("mine 1".to_string(), "mine 2".to_string()));
        let again = crate::io::load(&path).unwrap();
        assert_eq!(crate::numfmt::value_to_text(again.sheets[0].display_value(6, 2)), "mine 1");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Edit access lost while closed: the document opens, shows the changes read-only with
    /// "Save a copy", and keeps them in a copy file too.
    #[test]
    fn view_only_after_a_restart_keeps_the_changes_and_offers_a_copy() {
        let (server, dir, mut a, mut b, id) = two_people("viewer");
        let jdir = dir.join("journal");
        b.app.journal_dir = Some(jdir.clone());
        b.app.live_leave(true);
        open(&mut b, &id);
        settle(&mut [&mut a, &mut b], "joined");
        b.app.live_drop_for_test();
        type_at(&mut b.app, 6, 2, "mine 1");
        b.app.live_flush();
        a.session.grant(&id, "ben", false).unwrap();
        let mut b = reopen(b, &jdir);
        pump_all(&mut [&mut a, &mut b], "ben is told", |cs| cs[1].app.live_notice_for_test().is_some());
        let n = b.app.live_notice_for_test().unwrap();
        assert!(n.save_copy && n.text.contains("weren't saved to the server"), "{}", n.text);
        assert!(n.text.contains("A copy with them was also saved"), "{}", n.text);
        assert_eq!(text(&b, "Sheet1", 6, 2), "mine 1", "still in the window");
        assert!(Journal::all(&jdir).is_empty());
        let kept: Vec<_> = std::fs::read_dir(jdir.join("kept")).unwrap().flatten().collect();
        assert_eq!(kept.len(), 1);
        let copy = crate::io::load(&kept[0].path()).unwrap();
        assert_eq!(crate::numfmt::value_to_text(copy.sheets[0].display_value(6, 2)), "mine 1");
        assert_eq!(text(&a, "Sheet1", 6, 2), "", "the server never got it");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A long time offline: many changes on both sides, structural ones too, a snapshot that
    /// cuts the log; every one of Ben's changes is there once reconnected, and so is Ann's.
    #[test]
    fn a_long_offline_period_with_many_changes_on_both_sides_loses_nothing() {
        let (server, dir, mut a, mut b, _) = two_people("long");
        b.app.live_drop_for_test();
        for i in 0..40u32 {
            match i % 9 {
                4 => {
                    b.app.select_range(rg("36:37"));
                    b.app.insert_rows();
                }
                7 => {
                    b.app.select_range(rg("U:U"));
                    b.app.insert_cols();
                }
                _ => {}
            }
            type_at(&mut b.app, i % 20, 10 + i / 20, &format!("b{i}"));
        }
        b.app.select_range(rg("90:91"));
        b.app.delete_rows();
        b.app.live_flush();
        assert!(b.app.live_waiting() >= 40);
        for i in 0..40u32 {
            match i % 8 {
                3 => {
                    a.app.select_range(rg("1:2"));
                    a.app.insert_rows();
                }
                6 => {
                    a.app.select_range(rg("B:B"));
                    a.app.insert_cols();
                }
                _ => {}
            }
            type_at(&mut a.app, 200 + i * 20, 3, &format!("a{i}"));
            if i % 10 == 9 {
                pump(&mut [&mut a], "a's changes are in", |cs| cs[0].app.live_quiet());
            }
        }
        a.app.select_range(rg("120:122"));
        a.app.delete_rows();
        pump(&mut [&mut a], "a's changes are in", |cs| cs[0].app.live_quiet());
        a.app.live_snapshot();
        let seq = a.app.live_seq();
        pump(&mut [&mut a], "snapshot", |cs| cs[0].app.live_snap_seq() == seq);
        type_at(&mut a.app, 0, 7, "after the snapshot");
        a.app.select_range(rg("1:1"));
        a.app.insert_rows();
        pump(&mut [&mut a], "a's last changes are in", |cs| cs[0].app.live_quiet());
        // The server keeps the log from the snapshot before the latest: a second one cuts it
        // past Ben's base.
        std::thread::sleep(Duration::from_millis(1100));
        a.app.live_snapshot();
        let seq = a.app.live_seq();
        pump(&mut [&mut a], "second snapshot", |cs| cs[0].app.live_snap_seq() == seq);
        b.app.live_reconnect_for_test();
        settle(&mut [&mut a, &mut b], "reloaded and caught up");
        assert!(b.app.live_reloads() >= 1, "the server cut the log: ben reloaded");
        assert_same(&a, &b, "after the long offline period");
        for c in [&a, &b] {
            let missing: Vec<String> = (0..40).flat_map(|i| [format!("a{i}"), format!("b{i}")]).filter(|t| count(c, t) != 1).collect();
            assert!(missing.is_empty(), "missing or doubled: {missing:?}");
            for i in 0..40 {
                assert_eq!(count(c, &format!("b{i}")), 1, "ben's change {i}");
                assert_eq!(count(c, &format!("a{i}")), 1, "ann's change {i}");
            }
            assert_eq!(count(c, "after the snapshot"), 1);
        }
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Offline is said plainly in the status bar, with what is waiting and when the next
    /// attempt is; editing is not blocked; online again, it goes away.
    #[test]
    fn the_offline_state_is_clear_and_editing_goes_on() {
        let (server, dir, mut a, mut b, _) = two_people("offline-ui");
        assert!(b.app.live_offline_text().is_none());
        b.app.live_drop_for_test();
        let t = b.app.live_offline_text().expect("offline text");
        assert!(t.starts_with("Offline \u{2014} changes will be sent when you're back online") && !t.contains("waiting") && t.contains("Reconnecting in"), "{t}");
        type_at(&mut b.app, 6, 2, "typed offline");
        type_at(&mut b.app, 7, 2, "typed offline 2");
        b.app.live_flush();
        let t = b.app.live_offline_text().unwrap();
        assert!(t.contains("(2 waiting)"), "{t}");
        // The window draws it and still takes input.
        let _ = b.ctx.run(egui::RawInput::default(), |ctx| b.app.frame(ctx));
        type_at(&mut b.app, 8, 2, "and another");
        b.app.live_flush();
        assert!(b.app.live_offline_text().unwrap().contains("(3 waiting)"));
        b.app.live_reconnect_for_test();
        settle(&mut [&mut a, &mut b], "back online");
        assert!(b.app.live_offline_text().is_none());
        assert_eq!(text(&a, "Sheet1", 8, 2), "and another");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The save state in words follows the acknowledgements and the journal's count.
    #[test]
    fn the_save_state_says_saving_saved_or_offline_with_a_count() {
        let (server, dir, mut a, mut b, _) = two_people("save-state");
        assert_eq!(b.app.live_save_text().as_deref(), Some("Saved to server"));
        assert!(b.app.cloud_title_suffix().ends_with("Saved to server"), "{}", b.app.cloud_title_suffix());
        type_at(&mut b.app, 6, 2, "x");
        b.app.live_flush();
        assert_eq!(b.app.live_save_text().as_deref(), Some("Saving..."), "sent, not acknowledged yet");
        settle(&mut [&mut a, &mut b], "acknowledged");
        assert_eq!(b.app.live_save_text().as_deref(), Some("Saved to server"));
        b.app.live_drop_for_test();
        assert_eq!(b.app.live_save_text().as_deref(), Some("Offline"));
        type_at(&mut b.app, 7, 2, "y");
        type_at(&mut b.app, 8, 2, "z");
        b.app.live_flush();
        assert_eq!(b.app.live_save_text().as_deref(), Some("Offline (2 waiting)"));
        assert!(b.app.cloud_title_suffix().ends_with("Offline (2 waiting)"));
        // The status bar draws it.
        let _ = b.ctx.run(egui::RawInput::default(), |ctx| b.app.frame(ctx));
        b.app.live_reconnect_for_test();
        settle(&mut [&mut a, &mut b], "back online");
        assert_eq!(b.app.live_save_text().as_deref(), Some("Saved to server"));
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Colours come from the user, the same on both sides and in every session; an agent has
    /// its own style; a person who leaves has no cursor left.
    #[test]
    fn presence_colours_are_stable_and_nobody_is_left_behind() {
        let (server, dir, mut a, mut b, _) = two_people("presence-m4");
        let colour_of = |c: &Client, name: &str| c.app.live.as_ref().unwrap().peers.values().find(|p| p.peer.name == name).map(|p| super::super::live_ui::user_color(&p.peer)).unwrap();
        let ben_seen_by_ann = colour_of(&a, "Ben Ode");
        let ann_seen_by_ben = colour_of(&b, "Ann Lee");
        let own = |c: &Client| super::super::live_ui::user_color(c.app.live.as_ref().unwrap().me.as_ref().unwrap());
        assert_eq!(ben_seen_by_ann, own(&b), "Ben looks the same to Ann as to himself");
        assert_eq!(ann_seen_by_ben, own(&a));
        // Moving shows a cursor on the other side, and a name flag source.
        b.app.select(3, 1, false);
        pump(&mut [&mut a, &mut b], "Ann sees Ben's cursor", |cs| cs[0].app.live.as_ref().unwrap().peers.values().any(|p| p.peer.presence.as_ref().is_some_and(|x| (x.r, x.c) == (3, 1))));
        let _ = a.ctx.run(egui::RawInput::default(), |ctx| a.app.frame(ctx));
        // Ben leaves: Ann has no peer and no cursor for him, and the avatar list is empty.
        b.app.live_leave(true);
        pump(&mut [&mut a, &mut b], "Ann sees Ben leave", |cs| cs[0].app.live_peer_names().is_empty());
        assert!(a.app.live.as_ref().unwrap().peers.is_empty());
        std::thread::sleep(Duration::from_millis(450));
        a.app.poll_live();
        assert!(a.app.live.as_ref().unwrap().gone.is_empty(), "the fade-out is over");
        // A dropped connection clears everyone at once.
        let mut b2 = client(&server.url(), "ben", &dir);
        let id = a.app.cloud.doc.as_ref().unwrap().item_id.clone();
        open(&mut b2, &id);
        pump(&mut [&mut a, &mut b2], "Ann sees Ben again", |cs| !cs[0].app.live_peer_names().is_empty());
        a.app.live_drop_for_test();
        assert!(a.app.live_peer_names().is_empty(), "no stale cursors while disconnected");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Following a person moves the view with them; moving yourself stops it.
    #[test]
    fn following_someone_stops_when_you_move() {
        let (server, dir, mut a, mut b, _) = two_people("follow");
        a.app.live_toggle_follow("ben", "Ben Ode");
        assert_eq!(a.app.live_following().as_deref(), Some("Ben Ode"));
        b.app.select(5, 2, false);
        pump(&mut [&mut a, &mut b], "Ann's view follows Ben", |cs| {
            let sh = &cs[0].app.wb.sheets[cs[0].app.wb.active];
            sh.view.active == (5, 2)
        });
        assert_eq!(a.app.live_following().as_deref(), Some("Ben Ode"));
        a.app.select(1, 1, false);
        a.app.poll_live();
        assert!(a.app.live_following().is_none(), "moving on your own stops following");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Restoring an old version while both are connected: both windows reload the restored file,
    /// and what the other typed but had not sent yet is put back on top of it.
    #[test]
    fn a_restore_with_people_connected_reloads_them_and_keeps_unsent_changes() {
        let (server, dir, mut a, mut b, id) = two_people("restore-live");
        type_at(&mut a.app, 2, 1, "from a");
        settle(&mut [&mut a, &mut b], "a's change everywhere");
        a.app.live_snapshot();
        let seq = a.app.live_seq();
        pump(&mut [&mut a, &mut b], "snapshot", |cs| cs[0].app.live_snap_seq() == seq);
        let versions = a.session.versions(&id).unwrap();
        assert!(versions.len() >= 2, "{versions:?}");
        // Ben has typed something that is still in his window when the restore happens.
        type_at(&mut b.app, 6, 2, "ben unsent");
        a.session.restore_version(&id, 1).unwrap();
        let reloads = (a.app.live_reloads(), b.app.live_reloads());
        pump(&mut [&mut a, &mut b], "both reloaded", |cs| cs[0].app.live_reloads() > reloads.0 && cs[1].app.live_reloads() > reloads.1 && cs.iter().all(|c| c.app.live_quiet()));
        settle(&mut [&mut a, &mut b], "caught up after the restore");
        assert_same(&a, &b, "after the restore");
        for c in [&a, &b] {
            assert_eq!(text(c, "Sheet1", 2, 1), "", "version 1 has no 'from a'");
            assert_eq!(text(c, "Sheet1", 6, 2), "ben unsent", "unsent changes are kept");
        }
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A version can be named from the app's side of the API and the list shows who contributed.
    #[test]
    fn version_names_and_contributors_reach_the_client() {
        let (server, dir, mut a, mut b, id) = two_people("version-names");
        type_at(&mut a.app, 2, 1, "from a");
        type_at(&mut b.app, 3, 1, "from b");
        settle(&mut [&mut a, &mut b], "changes everywhere");
        a.app.live_snapshot();
        let seq = a.app.live_seq();
        pump(&mut [&mut a, &mut b], "snapshot", |cs| cs[0].app.live_snap_seq() == seq);
        let list = a.session.versions(&id).unwrap();
        let mut who = list[0].contributors.clone();
        who.sort();
        assert_eq!(who, ["Ann Lee", "Ben Ode"], "{list:?}");
        assert_eq!(b.session.label_version(&id, list[0].version, "  Budget v1 ").unwrap(), "Budget v1");
        assert_eq!(a.session.versions(&id).unwrap()[0].label, "Budget v1");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn threads(c: &Client) -> Vec<cosync::Thread> {
        c.app.wb.sheets[0].threads.clone()
    }

    /// Comment threads: appear for the others at once, replies, edits, resolving and deleting
    /// follow the rules (own comments; the owner manages), they move with rows, survive a
    /// snapshot, and viewers can read but not write.
    #[test]
    fn comment_threads_are_shared_live_and_follow_the_rules() {
        let (server, dir, mut a, mut b, id) = two_people("comments");
        // Ann is the owner (manages the document), Ben an editor.
        assert!(a.app.live.as_ref().unwrap().me.as_ref().unwrap().manager);
        assert!(!b.app.live.as_ref().unwrap().me.as_ref().unwrap().manager);
        a.session.grant(&id, "cam", false).unwrap();
        let mut c = client(&server.url(), "cam", &dir);

        a.app.select(1, 1, false);
        let th = a.app.comment_add(1, 1, "Is B2 right? @Ben Ode").expect("added");
        super::check(&mut a.app, "adding a thread");
        settle(&mut [&mut a, &mut b], "thread added");
        assert_same(&a, &b, "the thread reaches Ben");
        let t = &threads(&b)[0];
        assert_eq!((t.id.as_str(), t.r, t.c, t.comments[0].by.as_str(), t.comments[0].name.as_str()), (th.as_str(), 1, 1, "ann", "Ann Lee"));

        // Ben replies and resolves; Ann edits hers. A viewer can't write.
        assert!(b.app.comment_reply(&th, "Yes, checked."));
        assert!(b.app.comment_resolve(&th, true));
        assert!(a.app.comment_edit(&th, &th, "Is B2 right? @Ben Ode (edited)"));
        settle(&mut [&mut a, &mut b], "reply, resolve, edit");
        super::check(&mut b.app, "reply and resolve");
        assert_same(&a, &b, "reply, resolve and edit");
        let t = &threads(&a)[0];
        assert!(t.done && t.comments.len() == 2 && t.comments[0].edited > 0, "{t:?}");
        let reply = t.comments[1].id.clone();

        // The rules: Ben can't edit or delete Ann's comment; Ann (owner) can delete Ben's reply.
        assert!(!b.app.comment_edit(&th, &th, "hijacked"));
        assert!(!b.app.comment_delete(&th, &th));
        assert!(b.app.status.as_deref().is_some_and(|s| s.contains("your own")));
        settle(&mut [&mut a, &mut b], "refusals change nothing");
        assert_same(&a, &b, "refused edits");
        assert!(a.app.comment_delete(&th, &reply), "an owner deletes anyone's comment");
        settle(&mut [&mut a, &mut b], "owner deleted a reply");
        assert_eq!(threads(&b)[0].comments.len(), 1);

        // At the same time: Ann inserts a row above the thread, Ben replies and Ann's cell edit lands.
        b.app.comment_reply(&th, "second reply");
        a.app.select_range(rg("1:1"));
        a.app.insert_rows();
        settle(&mut [&mut a, &mut b], "concurrent reply and row insert");
        assert_same(&a, &b, "concurrent reply and row insert");
        assert_eq!((threads(&b)[0].r, threads(&b)[0].comments.len()), (2, 2), "the thread moved down with its cell");

        // A viewer reads the thread and can't write; Ben can't be talked into deleting the row's thread.
        open(&mut c, &id);
        settle(&mut [&mut a, &mut b, &mut c], "viewer joined");
        assert_same(&a, &c, "the viewer sees the thread");
        assert!(!c.app.live_editor());
        assert!(c.app.comment_add(5, 5, "from a viewer").is_none());
        assert!(!c.app.comment_reply(&th, "nope"));
        assert_eq!(threads(&c).len(), 1);

        // Two threads on one cell at once, and a snapshot: a late joiner has exactly the same.
        let (x, y) = (a.app.comment_add(3, 3, "from ann").unwrap(), b.app.comment_add(3, 3, "from ben").unwrap());
        settle(&mut [&mut a, &mut b, &mut c], "two threads on one cell");
        assert_same(&a, &b, "two threads on one cell");
        assert_eq!(threads(&a).len(), 3);
        a.app.live_snapshot();
        let seq = a.app.live_seq();
        pump(&mut [&mut a, &mut b, &mut c], "snapshot", |cs| cs[0].app.live_snap_seq() == seq);
        let mut d = client(&server.url(), "ben", &dir);
        open(&mut d, &id);
        settle(&mut [&mut a, &mut b, &mut c, &mut d], "late joiner");
        assert_same(&a, &d, "snapshot plus replay carries the threads");
        assert!(threads(&d).iter().any(|t| t.id == x) && threads(&d).iter().any(|t| t.id == y));
        // The saved xlsx has them too (what Excel and LibreOffice open).
        let v = a.session.versions(&id).unwrap();
        assert!(v.len() >= 2);
        let path = dir.join("version.xlsx");
        a.session.download_version(&id, v[0].version, &path).unwrap();
        let wb = crate::io::load(&path).unwrap();
        assert_eq!(wb.sheets[0].threads.len(), 3, "threaded comments are in the saved file");
        drop((c, d));
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

