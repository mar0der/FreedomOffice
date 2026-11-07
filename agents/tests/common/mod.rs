//! Scenarios that run against any server (the current one in-process, or the real pre-M2 one).
#![allow(dead_code)]

use std::time::Duration;

use cosync::{CellData, Val};
use fsheets::api::Api;
use fsheets::live::Live;

fn text(t: &str) -> Option<CellData> {
    Some(CellData { v: Val::Text(t.into()), ..Default::default() })
}

fn settle(a: &mut Live, b: &mut Live) {
    for _ in 0..200 {
        a.pump(Duration::from_millis(20));
        b.pump(Duration::from_millis(20));
        if a.synced() && b.synced() && a.seq() == b.seq() {
            return;
        }
    }
    panic!("never settled");
}

/// A client downloads the file; before it joins, someone else's snapshot makes a newer version, so
/// the server answers its join with `reload`. It must fetch the latest version, keep what it had
/// written meanwhile, and converge with the others (before, it stopped with an error).
pub fn reload_keeps_pending_changes(api: &Api, item: &str) {
    let mut b = Live::prepare(api, item, true).unwrap();
    let mut a = Live::open(api, item, true).unwrap();
    assert!(a.wait_synced(Duration::from_secs(10)));
    let sheet = a.first_sheet().unwrap();
    a.set_content(sheet, 60, 0, text("from a"));
    assert!(a.wait_synced(Duration::from_secs(10)));
    a.snapshot().expect("a snapshot makes a newer version");
    // b has not joined yet and writes while offline.
    b.set_content(sheet, 61, 0, text("from b, before the reload"));
    b.connect().unwrap();
    assert!(b.wait_synced(Duration::from_secs(15)), "{:?} {:?}", b.stopped, b.last_error);
    assert_eq!(b.stats.reloads, 1);
    settle(&mut a, &mut b);
    assert!(a.differences(&b).is_empty(), "{:?}", a.differences(&b));
    for l in [&a, &b] {
        assert_eq!(l.cell_data(sheet, 60, 0).map(|d| d.v), Some(Val::Text("from a".into())));
        assert_eq!(l.cell_data(sheet, 61, 0).map(|d| d.v), Some(Val::Text("from b, before the reload".into())));
    }
}
