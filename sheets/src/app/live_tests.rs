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

/// An editor made a viewer while changes are unsent: the session ends (instead of resending
/// them on every reconnect) and the changes stay in the window.
#[test]
fn becoming_a_viewer_with_unsent_changes_ends_the_session() {
    use crate::live::conn::Event;
    for welcome in [false, true] {
        let (_ctx, mut app) = super::data_ui::sample_app();
        app.live_for_test();
        type_at(&mut app, 0, 0, "unsent");
        app.live_flush();
        assert!(!app.live_pending_ops().is_empty());
        if welcome {
            // Reconnecting after access was changed: the server says we are a viewer.
            let you = cosync::Peer { id: 1, name: "Me".into(), user: "me".into(), color: 0, editor: false, presence: None };
            app.live_event_for_test(Event::Msg(cosync::ServerMsg::Welcome { seq: 0, you, peers: vec![] }));
        } else {
            // Sending them was refused.
            app.live_event_for_test(Event::Closed { code: Some(cosync::close::READ_ONLY), http: None, reason: "view-only".into() });
        }
        let (stopped, retry) = app.live_stopped();
        assert!(stopped.is_some_and(|m| m.contains("view-only")), "the user is told (welcome: {welcome})");
        assert!(!retry, "no reconnect that would resend them (welcome: {welcome})");
        assert!(!app.live_editor() && !app.live_quiet());
        assert!(!app.live_pending_ops().is_empty(), "the changes are kept");
        assert_eq!(crate::numfmt::value_to_text(app.wb.sheets[app.wb.active].display_value(0, 0)), "unsent");
    }
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
        assert!(b.app.cloud_title_suffix().contains("Reconnecting"));
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
}
