//! The soak harness against a real server started in-process (127.0.0.1, random port, temp data).

use freedomoffice_server::config::Config;
use freedomoffice_server::store::Store;
use freedomoffice_server::{Running, spawn};
mod common;

use fsheets::api::Api;
use fsheets::live::Live;
use fsheets::{sample, soak};

/// Test-only password for the throwaway accounts.
const PW: &str = "test-password-1";

struct Srv {
    run: Option<Running>,
    dir: std::path::PathBuf,
    url: String,
}

impl Drop for Srv {
    fn drop(&mut self) {
        drop(self.run.take());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start() -> Srv {
    let dir = std::env::temp_dir().join(format!(
        "fo-agents-test-{}",
        freedomoffice_server::util::new_id()
    ));
    let cfg = Config {
        data_dir: dir.join("data"),
        login_delay_ms: 10,
        ..Config::default()
    };
    {
        let s = Store::open(&cfg.db_path(), &cfg.files_dir(), cfg.versions_to_keep).unwrap();
        for (u, admin) in [("alice", true), ("bob", false), ("carol", false)] {
            s.create_user(u, u, "", PW, admin).unwrap();
        }
    }
    let run = spawn(cfg).unwrap();
    let url = run.url();
    Srv {
        run: Some(run),
        dir,
        url,
    }
}

/// A shared workbook (alice's, shared with bob and carol); returns each person's API and the item.
fn shared_workbook(s: &Srv, sample_name: &str) -> (Vec<Api>, String) {
    let apis: Vec<Api> = ["alice", "bob", "carol"]
        .iter()
        .map(|u| Api::login(&s.url, u, PW, "test").unwrap())
        .collect();
    let root = apis[0].root().unwrap();
    let item = apis[0]
        .upload_new(&root, "Soak.xlsx", sample::make(sample_name).unwrap())
        .unwrap();
    for u in ["bob", "carol"] {
        let r = apis[0]
            .send(
                "POST",
                &format!("/api/items/{}/permissions", item.id),
                &[("Content-Type", "application/json")],
                Some(format!(r#"{{"username":"{u}","role":"edit"}}"#).into_bytes()),
            )
            .unwrap();
        assert!(r.ok(), "{}", r.status);
    }
    (apis, item.id)
}

#[test]
fn three_clients_converge_with_disconnects() {
    let s = start();
    let (apis, item) = shared_workbook(&s, "budget");
    let cfg = soak::Config {
        clients: 3,
        edits: 80,
        seed: 0x5eed,
        interval_ms: 25,
        disconnect_pct: 4.0,
        ..Default::default()
    };
    let report = soak::run(&apis, &item, &cfg).unwrap();
    report.print();
    assert!(report.converged(), "{:?}", report.difference);
    assert!(report.totals.ops_sent > 100, "{report:?}");
    assert!(
        report.forced_disconnects > 0 && report.totals.reconnects > 0,
        "no reconnects happened: {report:?}"
    );
    assert!(report.head_seq > 0);
}

/// Past 200 batches the server asks an editor for a snapshot; the clients answer it, the
/// file gets a new version, and a client joining afterwards (file + live state + the log since)
/// holds exactly what the others hold.
#[test]
fn clients_answer_the_servers_snapshot_requests() {
    let s = start();
    let (apis, item) = shared_workbook(&s, "budget");
    let before = apis[0].versions(&item).unwrap().len();
    let cfg = soak::Config {
        clients: 3,
        edits: 120,
        seed: 0x5eed,
        interval_ms: 40,
        disconnect_pct: 2.0,
        ..Default::default()
    };
    let report = soak::run(&apis, &item, &cfg).unwrap();
    report.print();
    assert!(report.converged(), "{:?}", report.difference);
    assert!(report.totals.batches_sent > 200, "{report:?}");
    assert!(
        report.totals.snapshot_requests > 0,
        "the server never asked: {report:?}"
    );
    assert!(report.totals.snapshots > 0, "nobody answered: {report:?}");
    assert!(apis[0].versions(&item).unwrap().len() > before);
}

#[test]
fn concurrent_edits_of_one_cell() {
    let s = start();
    let (apis, item) = shared_workbook(&s, "tasks");
    let p = soak::probe_conflicts(&apis[0], &apis[1], &item).unwrap();
    println!("{p:#?}");
    // A cell's content and format are separate targets: both survive.
    assert!(p.value_survived && p.format_survived, "{}", p.value_vs_format);
    assert!(!p.value_vs_format.contains("told something was replaced: true"));
    // Two values: the later commit wins and the loser, and only the loser, is told.
    assert!(p.told_loser, "{}", p.value_vs_value);
    // An edit made after seeing the other's reports nothing.
    assert!(p.sequential_quiet);
    assert!(p.format_vs_format.contains("A was told: Your formatting of"), "{}", p.format_vs_format);
    assert!(p.insert_vs_edit_ok, "{}", p.insert_vs_edit);
}

#[test]
fn agents_are_marked_and_see_each_other() {
    let s = start();
    let (apis, item) = shared_workbook(&s, "grades");
    let mut a = Live::open(&apis[0], &item, true).unwrap();
    let mut b = Live::open(&apis[1], &item, false).unwrap();
    a.pump(std::time::Duration::from_millis(200));
    b.pump(std::time::Duration::from_millis(200));
    assert!(a.peers.values().any(|p| p.user == "bob" && !p.agent));
    assert!(b.peers.values().any(|p| p.user == "alice" && p.agent));
    // The starting state comes from the xlsx and an edit reaches the other client.
    let sheet = a.first_sheet().unwrap();
    assert_eq!(
        a.cell_data(sheet, 0, 0).map(|c| c.v),
        Some(cosync::Val::Text("Student".into()))
    );
    a.set_cell(
        sheet,
        20,
        0,
        Some(cosync::CellData {
            v: cosync::Val::Text("hi".into()),
            ..Default::default()
        }),
    );
    assert!(a.wait_synced(std::time::Duration::from_secs(5)));
    b.pump(std::time::Duration::from_millis(300));
    assert_eq!(b.cell_data(sheet, 20, 0), a.cell_data(sheet, 20, 0));
}

/// Against a protocol 1 server (Welcome without `proto`) fsheets keeps working: only whole `Cell`
/// operations are sent. Simulated by switching the mode on a client of the current server (a reconnect would reset it).
#[test]
fn protocol_1_mode_sends_only_whole_cells_and_converges() {
    use cosync::{CellData, Op, Val};
    let s = start();
    let (apis, item) = shared_workbook(&s, "tasks");
    let mut a = Live::open(&apis[0], &item, true).unwrap();
    let mut b = Live::open(&apis[1], &item, true).unwrap();
    assert!(a.wait_synced(std::time::Duration::from_secs(5)));
    a.legacy = true;
    let sheet = a.first_sheet().unwrap();
    // Value, format and both: whole cells in the batch in flight and in the buffer behind it.
    a.set_content(sheet, 30, 0, Some(CellData { v: Val::Text("v".into()), ..Default::default() }));
    a.set_format(sheet, 31, 0, freedom_sheets::live::style_json(&freedom_sheets::model::Style { bold: true, ..Default::default() }));
    a.set_content(sheet, 31, 0, Some(CellData { v: Val::Num(5.0), ..Default::default() }));
    let pending = a.sess.pending();
    assert_eq!(pending.len(), 3);
    assert!(pending.iter().all(|o| matches!(o, Op::Cell { .. })), "{pending:?}");
    // (The second content edit carries the cell's whole state, format included.)
    assert!(matches!(&pending[2], Op::Cell { v: Some(d), .. } if d.st.is_some() && d.v == Val::Num(5.0)));
    // B formats the cell A typed in, at the same time.
    b.set_format(sheet, 30, 0, freedom_sheets::live::style_json(&freedom_sheets::model::Style { italic: true, ..Default::default() }));
    for _ in 0..100 {
        a.pump(std::time::Duration::from_millis(20));
        b.pump(std::time::Duration::from_millis(20));
        if a.synced() && b.synced() && a.seq() == b.seq() {
            break;
        }
    }
    assert!(a.synced() && b.synced());
    assert!(a.differences(&b).is_empty(), "{:?}", a.differences(&b));
    assert!(a.take_replaced().is_empty());
}

#[test]
fn a_reload_request_is_followed_and_pending_changes_survive() {
    let s = start();
    let (apis, item) = shared_workbook(&s, "tasks");
    common::reload_keeps_pending_changes(&apis[0], &item);
}
