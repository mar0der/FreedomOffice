//! fsheets against the REAL pre-M2 server (protocol 1: no `proto`, no `base`, only whole-cell
//! operations), not a simulation of it. Slow (builds that server from git), so ignored by default:
//!
//!   cargo test -p freedomsheets-agents --test old_server -- --ignored --nocapture
//!
//! `FO_OLD_SERVER=/path/to/binary` uses a prebuilt server instead; `FO_OLD_REF` (default
//! `b08d351`, the commit before M2) picks the revision to build.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use cosync::{CellData, Val};
use fsheets::api::Api;
use fsheets::live::Live;
use fsheets::{sample, soak};

const PW: &str = "old-server-test-password";

struct OldServer {
    child: Child,
    dir: PathBuf,
    worktree: Option<PathBuf>,
    url: String,
}

impl Drop for OldServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(w) = &self.worktree {
            let _ = Command::new("git").args(["worktree", "remove", "--force"]).arg(w).current_dir(repo()).status();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn start_old() -> OldServer {
    start_at("FO_OLD_SERVER", "FO_OLD_REF", "b08d351")
}

/// The server of the commit before M5 (protocol 2, no comments).
fn start_m4() -> OldServer {
    start_at("FO_M4_SERVER", "FO_M4_REF", "4992888")
}

/// A server built from git revision `default_rev` (or `ref_var`), or the prebuilt binary in `bin_var`.
fn start_at(bin_var: &str, ref_var: &str, default_rev: &str) -> OldServer {
    let dir = std::env::temp_dir().join(format!("fo-old-server-{}", freedomoffice_server::util::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (bin, worktree) = match std::env::var(bin_var) {
        Ok(b) => (PathBuf::from(b), None),
        Err(_) => {
            let rev = std::env::var(ref_var).unwrap_or_else(|_| default_rev.into());
            let w = dir.join("src");
            let ok = Command::new("git").args(["worktree", "add", "--detach"]).arg(&w).arg(&rev).current_dir(repo()).status().unwrap().success();
            assert!(ok, "git worktree add {rev}");
            let ok = Command::new("cargo")
                .args(["build", "--release", "-p", "freedomoffice-server", "--target-dir"])
                .arg(dir.join("target"))
                .current_dir(&w)
                .status()
                .unwrap()
                .success();
            assert!(ok, "building the old server");
            (dir.join("target/release/freedomoffice-server"), Some(w))
        }
    };
    let port = free_port();
    let cfg = dir.join("s.toml");
    std::fs::write(&cfg, format!("listen = \"127.0.0.1:{port}\"\ndata_dir = \"{}\"\n", dir.join("data").display())).unwrap();
    let mut add = Command::new(&bin).arg("--config").arg(&cfg).args(["user", "add", "alice"]).stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
    add.stdin.take().unwrap().write_all(format!("{PW}\n").as_bytes()).unwrap();
    assert!(add.wait().unwrap().success(), "creating the user");
    let child = Command::new(&bin).arg("--config").arg(&cfg).arg("serve").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let url = format!("http://127.0.0.1:{port}");
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end && std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        std::thread::sleep(Duration::from_millis(50));
    }
    OldServer { child, dir, worktree, url }
}

fn workbook(api: &Api, name: &str) -> String {
    let root = api.root().unwrap();
    api.upload_new(&root, name, sample::make("tasks").unwrap()).unwrap().id
}

#[test]
#[ignore = "builds the pre-M2 server from git"]
fn fsheets_works_against_the_real_pre_m2_server() {
    let s = start_old();
    let api = Api::login(&s.url, "alice", PW, "test").unwrap();

    // It joins in protocol 1 mode, edits (value, format, both) and converges: the old server would
    // close the connection on an operation it doesn't know.
    let item = workbook(&api, "Proto1.xlsx");
    let mut a = Live::open(&api, &item, true).unwrap();
    let mut b = Live::open(&api, &item, true).unwrap();
    assert!(a.wait_synced(Duration::from_secs(10)) && b.wait_synced(Duration::from_secs(10)));
    assert!(a.legacy && b.legacy, "the old server's Welcome has no proto");
    let sheet = a.first_sheet().unwrap();
    let style = freedom_sheets::live::style_json(&freedom_sheets::model::Style { bold: true, ..Default::default() });
    a.set_content(sheet, 70, 0, Some(CellData { v: Val::Text("v".into()), ..Default::default() }));
    a.set_format(sheet, 70, 0, style.clone());
    b.set_format(sheet, 71, 0, style);
    b.set_content(sheet, 71, 0, Some(CellData { v: Val::Num(3.0), ..Default::default() }));
    for _ in 0..100 {
        a.pump(Duration::from_millis(20));
        b.pump(Duration::from_millis(20));
        if a.synced() && b.synced() && a.seq() == b.seq() {
            break;
        }
    }
    assert!(a.differences(&b).is_empty(), "{:?}", a.differences(&b));
    assert!(a.cell_data(sheet, 70, 0).is_some_and(|d| d.st.is_some() && d.v == Val::Text("v".into())));
    assert!(a.stopped.is_none() && b.stopped.is_none() && a.stats.closes.is_empty());
    drop((a, b));

    // The reload request after somebody's snapshot (what broke the soak against this server).
    let item = workbook(&api, "Proto1-reload.xlsx");
    common::reload_keeps_pending_changes(&api, &item);

    // The soak, leaving a backlog without snapshot first so that joins race the snapshot.
    let item = workbook(&api, "Proto1-soak.xlsx");
    for seed in [3u64, 9, 17] {
        // (No comment actions: the old server has none, and the same random edits as before are made.)
        let cfg = soak::Config { clients: 3, edits: 120, seed, comment_pct: 0.0, ..Default::default() };
        let report = soak::run(std::slice::from_ref(&api), &item, &cfg).unwrap();
        report.print();
        assert!(report.converged(), "{:?}", report.difference);
    }
}

/// Against the server of the commit before M5 (protocol 2: it knows content/format cells but
/// not comments): fsheets edits as usual, comments are refused with a clear message, and the
/// soak with comment actions turned on converges (the clients leave comments out).
#[test]
#[ignore = "builds the M4 server from git"]
fn fsheets_works_against_the_real_m4_server_without_comments() {
    let s = start_m4();
    let api = Api::login(&s.url, "alice", PW, "test").unwrap();
    let item = workbook(&api, "M4.xlsx");
    let mut a = Live::open(&api, &item, true).unwrap();
    assert!(a.wait_synced(Duration::from_secs(10)));
    assert!(!a.legacy, "protocol 2 splits content and format");
    assert!(!a.comments, "but has no comments");
    let sheet = a.first_sheet().unwrap();
    a.set_content(sheet, 70, 0, Some(CellData { v: Val::Text("v".into()), ..Default::default() }));
    let err = fsheets::comments::add(&mut a, None, "B2", "hello").unwrap_err();
    assert!(err.contains("can't share comments"), "{err}");
    assert!(fsheets::comments::list(&mut a, None, false).unwrap()["count"] == 0);
    assert!(a.wait_synced(Duration::from_secs(10)) && a.stopped.is_none());
    drop(a);
    let cfg = soak::Config { clients: 3, edits: 100, seed: 5, comment_pct: 30.0, ..Default::default() };
    let report = soak::run(std::slice::from_ref(&api), &item, &cfg).unwrap();
    report.print();
    assert!(report.converged() && report.comment_actions == 0, "{:?}", report.difference);
}
