//! Whole-window frames run headless: keyboard and text input reach the right place
//! (dialogs, the sheet tab being renamed) and never the grid behind them.

use std::path::PathBuf;

use eframe::egui::{self, Event, Key, Modifiers, Pos2, Rect};

use super::App;
use crate::cloud::{CloudDoc, Provider};

fn input(events: Vec<Event>) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1300.0, 860.0))), events, ..Default::default() }
}

/// One frame of the whole window with these input events.
fn frame(ctx: &egui::Context, app: &mut App, events: Vec<Event>) {
    let _ = ctx.run(input(events), |ctx| app.frame(ctx));
}

fn key(k: Key) -> Vec<Event> {
    vec![
        Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE },
        Event::Key { key: k, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE },
    ]
}

fn typing(s: &str) -> Vec<Event> {
    vec![Event::Text(s.into())]
}

fn app() -> (egui::Context, App) {
    let (ctx, mut app) = super::data_ui::sample_app();
    app.no_cloud_for_test();
    for _ in 0..3 {
        frame(&ctx, &mut app, vec![]);
    }
    (ctx, app)
}

fn a1(app: &App) -> String {
    crate::numfmt::value_to_text(app.sheet().display_value(0, 0))
}

fn server_doc() -> CloudDoc {
    CloudDoc {
        drive_id: String::new(),
        item_id: "doc1".into(),
        name: "Team.xlsx".into(),
        etag: String::new(),
        web_url: String::new(),
        parent: None,
        local: PathBuf::from("Team.xlsx"),
        provider: Provider::FreedomServer("http://127.0.0.1:9".into()),
        live: None,
    }
}

/// Typing with a cloud dialog open never edits the sheet behind it.
fn assert_sheet_untouched(ctx: &egui::Context, app: &mut App, what: &str) {
    assert_cell_untouched(ctx, app, what);
    assert!(!app.dirty, "{what}: the workbook became dirty");
}

fn assert_cell_untouched(ctx: &egui::Context, app: &mut App, what: &str) {
    let before = a1(app);
    for ev in [typing("po-b"), key(Key::Delete), key(Key::Enter), typing("x")] {
        frame(ctx, app, ev);
    }
    assert!(app.edit.is_none(), "{what}: typing started a cell edit");
    assert_eq!(a1(app), before, "{what}: the cell changed");
}

#[test]
fn typing_in_the_share_dialog_stays_out_of_the_sheet() {
    let (ctx, mut app) = app();
    app.select(0, 0, false);
    app.cloud.doc = Some(server_doc());
    app.cloud_open_share();
    frame(&ctx, &mut app, vec![]);
    frame(&ctx, &mut app, typing("po-b"));
    assert_eq!(app.cloud.share.as_ref().map(|s| s.user_query.as_str()), Some("po-b"), "the people search has the focus");
    assert!(app.edit.is_none() && !app.dirty && a1(&app) == "Region");

    // Focus away from the search box (a click on the dialog's background): still no leak.
    ctx.memory_mut(|m| m.stop_text_input());
    assert_sheet_untouched(&ctx, &mut app, "share dialog");
    assert!(app.cloud.share.is_some());
    frame(&ctx, &mut app, key(Key::Escape));
    assert!(app.cloud.share.is_none(), "Escape closes the dialog");
    // The grid gets the keyboard back.
    frame(&ctx, &mut app, vec![]);
    frame(&ctx, &mut app, typing("z"));
    assert!(app.edit.is_some(), "typing reaches the grid again");
}

#[test]
fn typing_in_other_cloud_dialogs_stays_out_of_the_sheet() {
    let (ctx, mut app) = app();
    app.select(0, 0, false);
    app.cloud.doc = Some(server_doc());
    app.cloud.versions = Some(super::cloud_ui::VersionsState { doc: server_doc(), list: Some(Vec::new()), busy: false, error: None });
    assert_sheet_untouched(&ctx, &mut app, "version history");
    app.cloud.versions = None;

    app.cloud.conflict = Some(super::cloud_ui::Conflict::Changed);
    assert_sheet_untouched(&ctx, &mut app, "save conflict");
    app.cloud.conflict = None;

    app.cloud.doc = None;
    app.cloud_open_share();
    assert_sheet_untouched(&ctx, &mut app, "share (not uploaded)");
    app.cloud.share = None;

    // Enter is the dialog's OK (protecting the sheet), the typing is the password.
    app.open_protect_dialog();
    assert_cell_untouched(&ctx, &mut app, "protect sheet");
}

fn click(p: Pos2) -> Vec<Vec<Event>> {
    let b = |pressed| Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    vec![vec![Event::PointerMoved(p)], vec![b(true)], vec![b(false)]]
}

/// Starts renaming the active tab (as double-click, Format > Rename Sheet, Alt H O R do).
fn start_rename(ctx: &egui::Context, app: &mut App) {
    app.rename_tab = Some((app.wb.active, app.sheet().name.clone()));
    frame(ctx, app, vec![]);
    frame(ctx, app, vec![]);
}

fn renaming(app: &App) -> Option<String> {
    app.rename_tab.as_ref().map(|(_, b)| b.clone())
}

#[test]
fn renaming_a_tab_ends_with_enter_escape_or_a_click_elsewhere() {
    let (ctx, mut app) = app();

    // The old name is selected, so typing replaces it; Enter commits; the typing goes into
    // the tab, not a cell.
    start_rename(&ctx, &mut app);
    frame(&ctx, &mut app, typing("Data"));
    assert_eq!(renaming(&app).as_deref(), Some("Data"));
    assert!(app.edit.is_none());
    frame(&ctx, &mut app, key(Key::Enter));
    frame(&ctx, &mut app, vec![]);
    assert!(app.rename_tab.is_none(), "Enter ends the rename");
    assert_eq!(app.sheet().name, "Data");
    // ... and the grid has the keyboard again.
    frame(&ctx, &mut app, typing("7"));
    assert!(app.edit.is_some(), "typing after the rename edits a cell");
    app.cancel_edit();

    // Escape cancels.
    start_rename(&ctx, &mut app);
    frame(&ctx, &mut app, typing("Nope"));
    frame(&ctx, &mut app, key(Key::Escape));
    frame(&ctx, &mut app, vec![]);
    assert!(app.rename_tab.is_none(), "Escape ends the rename");
    assert_eq!(app.sheet().name, "Data", "Escape keeps the old name");

    // A click on the grid commits.
    start_rename(&ctx, &mut app);
    frame(&ctx, &mut app, key(Key::End));
    frame(&ctx, &mut app, typing("Z"));
    for ev in click(Pos2::new(400.0, 400.0)) {
        frame(&ctx, &mut app, ev);
    }
    frame(&ctx, &mut app, vec![]);
    assert!(app.rename_tab.is_none(), "clicking elsewhere ends the rename");
    assert_eq!(app.sheet().name, "DataZ");
}

#[test]
fn renaming_a_tab_in_a_live_session_sends_the_rename_and_survives_others_edits() {
    use cosync::Op;
    let (ctx, mut app) = app();
    app.cloud.doc = Some(CloudDoc { item_id: "test".into(), ..server_doc() });
    app.live_for_test();
    let s = app.wb.sheets[0].uid;
    let pos = app.wb.sheets[0].pos;
    start_rename(&ctx, &mut app);
    frame(&ctx, &mut app, typing("Pl"));
    // Others insert a row, then add a sheet before this tab, move it after, delete it, while
    // the box is open: it stays, with the focus and what was typed.
    let other = 0x5eed;
    let remote: [Vec<Op>; 4] = [
        vec![Op::Rows { s, at: 3, n: 1 }],
        vec![Op::AddSheet { s: other, name: "Front".into(), pos: pos - 1.0 }],
        vec![Op::Place { s: other, pos: pos + 1.0 }, Op::Place { s: other, pos: pos - 2.0 }],
        vec![Op::DelSheet { s: other, name: "Front".into() }],
    ];
    for (k, ops) in remote.iter().enumerate() {
        app.live_remote_for_test(ops);
        frame(&ctx, &mut app, vec![]);
        frame(&ctx, &mut app, vec![]);
        assert_eq!(renaming(&app).as_deref(), Some("Pl"), "still renaming after remote change {k}");
        if k == 1 {
            assert_eq!(app.wb.sheets[0].name, "Front", "the new sheet's tab is before the renamed one");
        }
    }
    frame(&ctx, &mut app, typing("an"));
    frame(&ctx, &mut app, key(Key::Enter));
    assert!(app.rename_tab.is_none());
    assert_eq!(app.wb.sheets[0].name, "Plan");
    assert!(app.live.is_some());
    assert!(app.live_errors().is_empty(), "the rename's operations reproduce it");
    assert!(!app.live_pending_ops().is_empty(), "the rename goes to the server like any edit");

    // The sheet being renamed is deleted by someone else: the rename ends, with a note.
    app.add_sheet();
    let keep = app.wb.sheets[1].uid;
    app.switch_sheet(0);
    start_rename(&ctx, &mut app);
    frame(&ctx, &mut app, typing("Gone"));
    app.live_remote_for_test(&[Op::DelSheet { s, name: "Plan".into() }]);
    frame(&ctx, &mut app, vec![]);
    assert!(app.rename_tab.is_none());
    assert!(app.status.as_deref().is_some_and(|t| t.contains("deleted the sheet 'Plan'")), "{:?}", app.status);
    assert_eq!(app.wb.sheets.iter().map(|x| x.uid).collect::<Vec<_>>(), [keep]);
}
