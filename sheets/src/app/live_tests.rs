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
