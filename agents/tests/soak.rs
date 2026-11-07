//! The soak harness against a real server started in-process (127.0.0.1, random port, temp data).

use freedomoffice_server::config::Config;
use freedomoffice_server::store::Store;
use freedomoffice_server::{Running, spawn};
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

#[test]
fn concurrent_edits_of_one_cell() {
    let s = start();
    let (apis, item) = shared_workbook(&s, "tasks");
    let p = soak::probe_conflicts(&apis[0], &apis[1], &item).unwrap();
    println!("{p:#?}");
    // Last writer wins per cell: the value and the format are one target, so one is lost.
    assert!(p.value_survived != p.format_survived);
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
    let sheet = a.book.ordered()[0].0;
    assert_eq!(
        a.book.cell(sheet, 0, 0).map(|c| c.v.clone()),
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
    assert_eq!(b.book.cell(sheet, 20, 0), a.book.cell(sheet, 20, 0));
}
