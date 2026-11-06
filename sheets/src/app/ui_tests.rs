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
