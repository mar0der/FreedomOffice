//! The scripted multi-client harness: N clients on one item make random concurrent edits
//! (cells, row/column inserts and deletes, presence) with random disconnects and reconnects;
//! at the end every client and a freshly joined one must hold identical cell maps.

use std::time::{Duration, Instant};

use cosync::{CellData, Op, SheetId, Val};

use crate::api::Api;
use crate::live::{Live, Stats};
use crate::rng::Rng;

#[derive(Clone, Debug)]
pub struct Config {
    pub clients: usize,
    /// Actions per client.
    pub edits: usize,
    pub seed: u64,
    /// Pause between a client's actions (the server allows about 30 batches a second).
    pub interval_ms: u64,
    /// Chance (percent) per action that a connected client drops its connection.
    pub disconnect_pct: f64,
    /// Chance (percent) per action of a row/column insert or delete.
    pub structure_pct: f64,
    /// Edits land in the top-left `hot_rows` x `hot_cols` cells so clients collide often.
    pub hot_rows: u32,
    pub hot_cols: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            clients: 4,
            edits: 150,
            seed: 0,
            interval_ms: 40,
            disconnect_pct: 3.0,
            structure_pct: 8.0,
            hot_rows: 12,
            hot_cols: 6,
        }
    }
}

#[derive(Debug)]
pub struct Report {
    pub clients: usize,
    pub seed: u64,
    pub totals: Stats,
    /// Disconnects the harness caused on purpose.
    pub forced_disconnects: u64,
    pub head_seq: u64,
    pub cells: usize,
    pub digest: String,
    pub clients_identical: bool,
    pub joiner_identical: bool,
    pub difference: Option<String>,
    pub seconds: f64,
}

impl Report {
    pub fn converged(&self) -> bool {
        self.clients_identical && self.joiner_identical
    }

    pub fn print(&self) {
        let t = &self.totals;
        println!("soak: {} clients, seed {:#x}", self.clients, self.seed);
        println!(
            "  ops sent {} in {} batches, acks {}, remote batches applied {}",
            t.ops_sent, t.batches_sent, t.acks, t.remote_batches
        );
        println!(
            "  disconnects forced {}, reconnects {}, own batches replayed {}, presence sent {}",
            self.forced_disconnects, t.reconnects, t.replayed_own, t.presence_sent
        );
        println!(
            "  server closes {:?}, snapshot requests ignored {}",
            t.closes, t.snapshot_requests
        );
        println!(
            "  final seq {}, {} cells, digest {}",
            self.head_seq, self.cells, self.digest
        );
        println!(
            "  all clients identical: {}",
            if self.clients_identical { "yes" } else { "NO" }
        );
        println!(
            "  fresh joiner identical: {}",
            if self.joiner_identical { "yes" } else { "NO" }
        );
        if let Some(d) = &self.difference {
            println!("  first difference: {d}");
        }
        println!(
            "  converged: {}   time {:.1}s",
            if self.converged() { "yes" } else { "NO" },
            self.seconds
        );
    }
}

fn add(a: &mut Stats, b: &Stats) {
    a.batches_sent += b.batches_sent;
    a.ops_sent += b.ops_sent;
    a.acks += b.acks;
    a.remote_batches += b.remote_batches;
    a.replayed_own += b.replayed_own;
    a.presence_sent += b.presence_sent;
    a.connects += b.connects;
    a.reconnects += b.reconnects;
    a.closes.extend(b.closes.iter().copied());
    a.snapshot_requests += b.snapshot_requests;
}

fn first_sheet(l: &Live) -> Result<SheetId, String> {
    l.book
        .ordered()
        .first()
        .map(|(i, _)| *i)
        .ok_or_else(|| "The workbook has no sheet.".to_string())
}

/// One client's script. Returns the session (so the caller can compare it) and the number of
/// disconnects it caused on purpose.
fn client(api: Api, item: String, cfg: Config, idx: usize) -> Result<(Live, u64), String> {
    let mut rng = Rng::new(cfg.seed ^ (idx as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut live = Live::open(&api, &item, true)?;
    let sheet = first_sheet(&live)?;
    let pause = Duration::from_millis(cfg.interval_ms);
    let mut forced = 0u64;
    let mut back_at: Option<Instant> = None;
    for i in 0..cfg.edits {
        if let Some(why) = &live.stopped {
            return Err(format!("client {idx} stopped: {why}"));
        }
        if let Some(t) = back_at {
            if Instant::now() >= t && live.connect().is_ok() {
                back_at = None;
            }
        } else if !live.connected() {
            // The server closed us (e.g. a rate limit): come back like the app does.
            let _ = live.connect();
        } else if rng.chance(cfg.disconnect_pct) {
            live.disconnect();
            forced += 1;
            back_at = Some(Instant::now() + Duration::from_millis(100 + rng.below(500)));
        }
        let roll = rng.below(1000) as f64 / 10.0;
        let (r, c) = (
            rng.below(cfg.hot_rows as u64) as u32,
            rng.below(cfg.hot_cols as u64) as u32,
        );
        if roll < cfg.structure_pct {
            let n = if rng.chance(70.0) {
                1 + rng.below(2) as i32
            } else {
                -1
            };
            if rng.chance(70.0) {
                live.edit(vec![Op::Rows { s: sheet, at: r, n }]);
            } else {
                live.edit(vec![Op::Cols { s: sheet, at: c, n }]);
            }
        } else if roll < cfg.structure_pct + 8.0 {
            live.send_presence(sheet, r, c, rng.chance(50.0));
        } else if roll < cfg.structure_pct + 16.0 {
            live.set_cell(sheet, r, c, None);
        } else {
            let v = if rng.chance(50.0) {
                Val::Num(rng.below(1000) as f64)
            } else {
                Val::Text(format!("c{idx}-{i}"))
            };
            live.set_cell(
                sheet,
                r,
                c,
                Some(CellData {
                    v,
                    ..Default::default()
                }),
            );
        }
        if live.connected() {
            live.pump(pause);
        } else {
            std::thread::sleep(pause);
        }
    }
    // Back online and everything acknowledged.
    let end = Instant::now() + Duration::from_secs(30);
    while !live.synced() && Instant::now() < end {
        if live.stopped.is_some() {
            return Err(format!(
                "client {idx} stopped: {}",
                live.stopped.clone().unwrap_or_default()
            ));
        }
        if !live.connected() {
            let _ = live.connect();
            std::thread::sleep(Duration::from_millis(50));
        }
        live.pump(Duration::from_millis(20));
    }
    Ok((live, forced))
}

/// Runs the harness: client `i` signs in with `apis[i % apis.len()]`.
pub fn run(apis: &[Api], item: &str, cfg: &Config) -> Result<Report, String> {
    if apis.is_empty() || cfg.clients == 0 {
        return Err("Need at least one client.".into());
    }
    let started = Instant::now();
    let mut handles = Vec::new();
    for i in 0..cfg.clients {
        let (api, item, cfg) = (apis[i % apis.len()].clone(), item.to_string(), cfg.clone());
        handles.push(std::thread::spawn(move || client(api, item, cfg, i)));
    }
    let mut lives = Vec::new();
    let mut forced = 0;
    for (i, h) in handles.into_iter().enumerate() {
        let (l, f) = h.join().map_err(|_| format!("client {i} panicked"))??;
        lives.push(l);
        forced += f;
    }
    // Settle: everyone has the server's last sequence number and nothing is pending.
    let end = Instant::now() + Duration::from_secs(30);
    let mut stable = 0;
    while stable < 4 && Instant::now() < end {
        for l in &mut lives {
            if !l.connected() {
                let _ = l.connect();
            }
            l.pump(Duration::from_millis(15));
        }
        let top = lives.iter().map(|l| l.seq()).max().unwrap_or(0);
        if lives.iter().all(|l| l.synced() && l.seq() == top) {
            stable += 1
        } else {
            stable = 0
        }
    }
    // A client that joins now replays the whole log from the file's version.
    let mut joiner = Live::open(&apis[0], item, true)?;
    joiner.wait_synced(Duration::from_secs(30));
    let head_seq = lives.iter().map(|l| l.seq()).max().unwrap_or(0);
    let digest = lives[0].book.digest();
    let mut difference = None;
    let mut clients_identical = true;
    for (i, l) in lives.iter().enumerate().skip(1) {
        if l.book != lives[0].book {
            clients_identical = false;
            difference = difference.or_else(|| {
                lives[0]
                    .book
                    .first_difference(&l.book)
                    .map(|d| format!("client 0 vs client {i}: {d}"))
            });
        }
    }
    let joiner_identical = joiner.book == lives[0].book && joiner.seq() == head_seq;
    if !joiner_identical {
        difference = difference.or_else(|| {
            lives[0]
                .book
                .first_difference(&joiner.book)
                .map(|d| {
                    format!(
                        "client 0 vs fresh joiner (seq {} vs {head_seq}): {d}",
                        joiner.seq()
                    )
                })
                .or_else(|| {
                    Some(format!(
                        "fresh joiner at seq {} vs {head_seq}",
                        joiner.seq()
                    ))
                })
        });
    }
    let mut totals = Stats::default();
    for l in &lives {
        add(&mut totals, &l.stats);
    }
    Ok(Report {
        clients: cfg.clients,
        seed: cfg.seed,
        totals,
        forced_disconnects: forced,
        head_seq,
        cells: lives[0].book.cell_count(),
        digest,
        clients_identical,
        joiner_identical,
        difference,
        seconds: started.elapsed().as_secs_f64(),
    })
}

/// What two people editing one cell at the same time end up with.
#[derive(Debug)]
pub struct ProbeReport {
    /// Both edited the same cell: A its value, B its format (each sent the whole cell).
    pub value_vs_format: String,
    pub value_survived: bool,
    pub format_survived: bool,
    /// Both typed different values into one cell.
    pub value_vs_value: String,
    /// A inserted a row above the cell B was editing.
    pub insert_vs_edit: String,
    pub insert_vs_edit_ok: bool,
}

fn settle(a: &mut Live, b: &mut Live) {
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        a.pump(Duration::from_millis(20));
        b.pump(Duration::from_millis(20));
        if a.synced() && b.synced() && a.seq() == b.seq() {
            return;
        }
    }
}

/// Two clients edit the same cell while both are offline, then reconnect (A first).
pub fn probe_conflicts(api_a: &Api, api_b: &Api, item: &str) -> Result<ProbeReport, String> {
    let mut a = Live::open(api_a, item, true)?;
    let mut b = Live::open(api_b, item, true)?;
    let s = first_sheet(&a)?;
    let wait = |a: &mut Live, b: &mut Live| settle(a, b);
    wait(&mut a, &mut b);
    // Value vs format on one cell.
    let (r, c) = (40, 3);
    a.disconnect();
    b.disconnect();
    a.set_cell(
        s,
        r,
        c,
        Some(CellData {
            v: Val::Text("typed by A".into()),
            ..Default::default()
        }),
    );
    b.set_cell(
        s,
        r,
        c,
        Some(CellData {
            st: Some(serde_json::json!({"bold": true})),
            ..Default::default()
        }),
    );
    a.connect()?;
    a.wait_synced(Duration::from_secs(10));
    b.connect()?;
    wait(&mut a, &mut b);
    let end = a.book.cell(s, r, c).cloned().unwrap_or_default();
    let (value_survived, format_survived) =
        (end.v == Val::Text("typed by A".into()), end.st.is_some());
    let same = a.book.cell(s, r, c) == b.book.cell(s, r, c);
    let value_vs_format = format!(
        "A typed a value, B made it bold, both offline; A reconnected first: final cell {} (clients agree: {same})",
        serde_json::to_string(&end).unwrap_or_default()
    );
    // Value vs value.
    let (r2, c2) = (41, 3);
    a.disconnect();
    b.disconnect();
    a.set_cell(
        s,
        r2,
        c2,
        Some(CellData {
            v: Val::Text("from A".into()),
            ..Default::default()
        }),
    );
    b.set_cell(
        s,
        r2,
        c2,
        Some(CellData {
            v: Val::Text("from B".into()),
            ..Default::default()
        }),
    );
    a.connect()?;
    a.wait_synced(Duration::from_secs(10));
    b.connect()?;
    wait(&mut a, &mut b);
    let end2 = a
        .book
        .cell(s, r2, c2)
        .map(|d| crate::book::val_text(&d.v))
        .unwrap_or_default();
    let value_vs_value = format!(
        "A and B both typed in one cell, A reconnected first: final value '{end2}' (the later commit wins; B is not told that A's edit was replaced, A is not told that its edit was overwritten)"
    );
    // Row insert above an edit.
    let (r3, c3) = (50, 1);
    a.disconnect();
    b.disconnect();
    a.edit(vec![Op::Rows { s, at: 45, n: 1 }]);
    b.set_cell(
        s,
        r3,
        c3,
        Some(CellData {
            v: Val::Text("below the insert".into()),
            ..Default::default()
        }),
    );
    a.connect()?;
    a.wait_synced(Duration::from_secs(10));
    b.connect()?;
    wait(&mut a, &mut b);
    let moved = a
        .book
        .cell(s, r3 + 1, c3)
        .is_some_and(|d| d.v == Val::Text("below the insert".into()))
        && a.book.cell(s, r3, c3).is_none();
    let insert_vs_edit = format!(
        "A inserted a row at 46, B typed in row 51 concurrently: B's text ends up in row 52: {moved}"
    );
    Ok(ProbeReport {
        value_vs_format,
        value_survived,
        format_survived,
        value_vs_value,
        insert_vs_edit,
        insert_vs_edit_ok: moved && a.book == b.book,
    })
}
