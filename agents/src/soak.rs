//! The scripted multi-client harness: N clients on one item make random concurrent edits
//! (cells, row/column inserts and deletes, presence) with random disconnects and reconnects;
//! at the end every client and a freshly joined one must hold identical cell maps.

use std::path::PathBuf;
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
    /// Chance (percent) per action of a format edit (bold, italic, fill) of a cell.
    pub format_pct: f64,
    /// Edits land in the top-left `hot_rows` x `hot_cols` cells so clients collide often.
    pub hot_rows: u32,
    pub hot_cols: u32,
    /// Chance (percent) per action that the client is killed (dropped without leaving, right
    /// after its edit was sent, before any answer was read) and started again from its journal.
    pub restart_pct: f64,
    /// Chance (percent) per action of undoing the client's latest text edit that is still its
    /// own: undo runs like the app's, per person: the cell is found by the unique text the
    /// client typed (wherever others' row/column changes moved it) and is put back only when it
    /// still holds exactly that; when someone else changed it since, the undo is blocked.
    pub undo_pct: f64,
    /// Where the clients keep their journals (a fresh temporary folder, removed afterwards,
    /// when not given).
    pub journal_dir: Option<PathBuf>,
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
            format_pct: 25.0,
            hot_rows: 12,
            hot_cols: 6,
            restart_pct: 2.0,
            undo_pct: 8.0,
            journal_dir: None,
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
    /// Clients killed on purpose and restarted from their journal.
    pub restarts: u64,
    /// Undos applied (the client's own text put back to what it replaced) and undos blocked
    /// because someone else had changed the cell since.
    pub undos: u64,
    pub undos_blocked: u64,
    /// Journal files still there at the end (everything was acknowledged: must be 0).
    pub journals_left: usize,
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
        self.clients_identical && self.joiner_identical && self.journals_left == 0
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
            "  server closes {:?}, snapshot requests {}, snapshots uploaded {}",
            t.closes, t.snapshot_requests, t.snapshots
        );
        println!("  reloads from the latest version: {}", t.reloads);
        println!(
            "  clients killed and restarted from their journal: {} (journals put back: {}, left behind: {})",
            self.restarts, t.journal_restores, self.journals_left
        );
        println!(
            "  undos applied {}, blocked because someone else changed the cell {}",
            self.undos, self.undos_blocked
        );
        println!(
            "  own edits replaced by someone's concurrent edit (told to the editor): {}",
            t.replaced
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

/// A short fingerprint of the synchronised content (the operations that rebuild it).
fn digest(l: &Live) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for op in freedom_sheets::live::state_ops(&l.wb) {
        for b in format!("{op:?}").bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// A value as text (what a cell shows, without number formats).
pub fn val_text(v: &Val) -> String {
    match v {
        Val::Empty => String::new(),
        Val::Num(n) => format!("{n}"),
        Val::Text(t) => t.clone(),
        Val::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Val::Err(e) => e.clone(),
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
    a.snapshots += b.snapshots;
    a.replaced += b.replaced;
    a.reloads += b.reloads;
    a.journal_restores += b.journal_restores;
}

/// A random cell format (a few attributes, so concurrent format edits collide often).
fn random_style(rng: &mut Rng) -> Option<serde_json::Value> {
    let fills = [0xFFEE00u32, 0x66CCFF, 0xFF9999, 0xCCFFCC];
    freedom_sheets::live::style_json(&freedom_sheets::model::Style {
        bold: rng.chance(50.0),
        italic: rng.chance(25.0),
        fill: rng.chance(60.0).then(|| fills[rng.below(fills.len() as u64) as usize]),
        ..Default::default()
    })
}

fn first_sheet(l: &Live) -> Result<SheetId, String> {
    l.first_sheet()
        .ok_or_else(|| "The workbook has no sheet.".to_string())
}

/// One client's script. Returns the session (so the caller can compare it) and the number of
/// disconnects it caused on purpose.
/// What a client's soak run produced.
struct Done {
    live: Live,
    forced: u64,
    restarts: u64,
    undos: u64,
    blocked: u64,
}

/// The text of a cell, if it holds exactly plain text.
fn plain_text(d: &CellData) -> Option<&str> {
    match (&d.v, &d.f) {
        (Val::Text(t), None) => Some(t),
        _ => None,
    }
}

/// Where a client's text is now (others' row/column inserts and deletes move it).
fn find_text(l: &Live, sheet: SheetId, text: &str) -> Option<(u32, u32)> {
    let i = freedom_sheets::live::index_of(&l.wb, sheet)?;
    for (r, row) in &l.wb.sheets[i].rows {
        for c in row.keys() {
            if l.cell_data(sheet, *r, *c).as_ref().and_then(plain_text) == Some(text) {
                return Some((*r, *c));
            }
        }
    }
    None
}

fn client(api: Api, item: String, cfg: Config, idx: usize, jdir: Option<PathBuf>) -> Result<Done, String> {
    let mut rng = Rng::new(cfg.seed ^ (idx as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut live = Live::open_in(&api, &item, true, jdir.as_deref())?;
    let mut restarts = 0u64;
    let (mut undone, mut blocked) = (0u64, 0u64);
    // (the unique text typed, what the cell held before)
    let mut undos: Vec<(String, Option<CellData>)> = Vec::new();
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
        if !undos.is_empty() && rng.chance(cfg.undo_pct) {
            let (text, before) = undos.pop().expect("one");
            match find_text(&live, sheet, &text) {
                Some((r, c)) => {
                    live.set_content(sheet, r, c, before);
                    undone += 1;
                }
                None => blocked += 1,
            }
        } else if roll < cfg.structure_pct {
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
            // Clear the content, reset the format, or clear the whole cell (what protocol 1 did).
            match rng.below(3) {
                0 => live.set_content(sheet, r, c, None),
                1 => live.set_format(sheet, r, c, None),
                _ => live.set_cell(sheet, r, c, None),
            }
        } else if roll < cfg.structure_pct + 16.0 + cfg.format_pct {
            live.set_format(sheet, r, c, random_style(&mut rng));
        } else {
            let v = if rng.chance(50.0) {
                Val::Num(rng.below(1000) as f64)
            } else {
                Val::Text(format!("c{idx}-{i}"))
            };
            let data = CellData {
                v,
                ..Default::default()
            };
            if rng.chance(90.0) {
                if let Val::Text(t) = &data.v {
                    let before = live.cell_data(sheet, r, c).map(|d| CellData { st: None, ..d });
                    undos.push((t.clone(), before));
                    undos.drain(..undos.len().saturating_sub(20));
                }
                live.set_content(sheet, r, c, Some(data));
            } else {
                // Now and then a whole cell with a format, like an older client or a paste.
                live.set_cell(
                    sheet,
                    r,
                    c,
                    Some(CellData {
                        st: random_style(&mut rng),
                        ..data
                    }),
                );
            }
        }
        if jdir.is_some() && live.stopped.is_none() && rng.chance(cfg.restart_pct) {
            // Killed right after the edit went out: its answer is never read. The next
            // start finds the journal, puts the unconfirmed changes back and sends them; a
            // batch the server did commit is recognised, not applied twice.
            let before = live.stats.clone();
            drop(live);
            live = Live::open_in(&api, &item, true, jdir.as_deref())?;
            let mut merged = before;
            add(&mut merged, &live.stats);
            live.stats = merged;
            restarts += 1;
            back_at = None;
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
    Ok(Done { live, forced, restarts, undos: undone, blocked })
}

/// Runs the harness: client `i` signs in with `apis[i % apis.len()]`.
pub fn run(apis: &[Api], item: &str, cfg: &Config) -> Result<Report, String> {
    if apis.is_empty() || cfg.clients == 0 {
        return Err("Need at least one client.".into());
    }
    let started = Instant::now();
    let own_dir = cfg.journal_dir.is_none() && cfg.restart_pct > 0.0;
    let jdir: Option<PathBuf> = match (&cfg.journal_dir, own_dir) {
        (Some(d), _) => Some(d.clone()),
        (None, true) => Some(std::env::temp_dir().join(format!("fsheets-soak-journal-{}-{:x}", std::process::id(), Rng::random_seed()))),
        (None, false) => None,
    };
    let mut handles = Vec::new();
    for i in 0..cfg.clients {
        let (api, item, cfg, jdir) = (apis[i % apis.len()].clone(), item.to_string(), cfg.clone(), jdir.clone());
        handles.push(std::thread::spawn(move || client(api, item, cfg, i, jdir)));
    }
    let mut lives = Vec::new();
    let mut forced = 0;
    let mut restarts = 0;
    let (mut undos, mut undos_blocked) = (0, 0);
    for (i, h) in handles.into_iter().enumerate() {
        let d = h.join().map_err(|_| format!("client {i} panicked"))??;
        lives.push(d.live);
        forced += d.forced;
        restarts += d.restarts;
        undos += d.undos;
        undos_blocked += d.blocked;
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
    let digest = digest(&lives[0]);
    let mut difference = None;
    let mut clients_identical = true;
    for (i, l) in lives.iter().enumerate().skip(1) {
        let diff = lives[0].differences(l);
        if !diff.is_empty() {
            clients_identical = false;
            difference =
                difference.or_else(|| diff.first().map(|d| format!("client 0 vs client {i}: {d}")));
        }
    }
    let joiner_diff = lives[0].differences(&joiner);
    let joiner_identical = joiner_diff.is_empty() && joiner.seq() == head_seq;
    if !joiner_identical {
        difference = difference.or_else(|| {
            joiner_diff
                .first()
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
    // Everything is acknowledged, so no journal may be left.
    let journals_left = jdir.as_deref().map_or(0, |d| cosync::journal::Journal::all(d).len());
    if let (Some(d), true) = (&jdir, own_dir) {
        let _ = std::fs::remove_dir_all(d);
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
        restarts,
        undos,
        undos_blocked,
        journals_left,
        head_seq,
        cells: lives[0].cell_count(),
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
    /// Both edited the same cell at once: A its value, B its format.
    pub value_vs_format: String,
    pub value_survived: bool,
    pub format_survived: bool,
    /// Both typed different values into one cell.
    pub value_vs_value: String,
    /// The one whose value was overwritten was told so (and the other was not).
    pub told_loser: bool,
    /// An edit made after seeing the other's did not report anything.
    pub sequential_quiet: bool,
    /// Both formatted one cell differently: the later format won, and the loser was told.
    pub format_vs_format: String,
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
    let text = |t: &str| {
        Some(CellData {
            v: Val::Text(t.into()),
            ..Default::default()
        })
    };
    let bold = freedom_sheets::live::style_json(&freedom_sheets::model::Style {
        bold: true,
        ..Default::default()
    });
    let fill = freedom_sheets::live::style_json(&freedom_sheets::model::Style {
        fill: Some(0x66CCFF),
        ..Default::default()
    });
    // The two reconnect one after the other: A first, so B's edit is the later commit.
    let reconnect = |a: &mut Live, b: &mut Live| -> Result<(), String> {
        a.connect()?;
        a.wait_synced(Duration::from_secs(10));
        b.connect()?;
        wait(a, b);
        Ok(())
    };
    a.take_replaced();
    b.take_replaced();
    // Value vs format on one cell.
    let (r, c) = (40, 3);
    a.disconnect();
    b.disconnect();
    a.set_content(s, r, c, text("typed by A"));
    b.set_format(s, r, c, bold.clone());
    reconnect(&mut a, &mut b)?;
    let end = a.cell_data(s, r, c).unwrap_or_default();
    let (value_survived, format_survived) =
        (end.v == Val::Text("typed by A".into()), end.st.is_some());
    let same = a.cell_data(s, r, c) == b.cell_data(s, r, c);
    let quiet = a.take_replaced().is_empty() && b.take_replaced().is_empty();
    let value_vs_format = format!(
        "A typed a value, B made it bold, both offline; A reconnected first: final cell {} (clients agree: {same}; anyone told something was replaced: {})",
        serde_json::to_string(&end).unwrap_or_default(),
        !quiet
    );
    // Value vs value.
    let (r2, c2) = (41, 3);
    a.disconnect();
    b.disconnect();
    a.set_content(s, r2, c2, text("from A"));
    b.set_content(s, r2, c2, text("from B"));
    reconnect(&mut a, &mut b)?;
    let end2 = a
        .cell_data(s, r2, c2)
        .map(|d| val_text(&d.v))
        .unwrap_or_default();
    let (told_a, told_b) = (a.take_replaced(), b.take_replaced());
    let told_loser = told_a.len() == 1 && told_b.is_empty() && end2 == "from B";
    let value_vs_value = format!(
        "A and B both typed in one cell, A reconnected first: final value '{end2}' (the later commit wins). A was told: {}; B was told: {}",
        told_a.first().map(|e| e.message()).unwrap_or_else(|| "nothing".into()),
        told_b.first().map(|e| e.message()).unwrap_or_else(|| "nothing".into()),
    );
    // Sequential: B's edit is made after seeing A's, so nobody is told anything.
    let (r4, c4) = (42, 3);
    a.set_content(s, r4, c4, text("first, A"));
    wait(&mut a, &mut b);
    b.set_content(s, r4, c4, text("then B"));
    wait(&mut a, &mut b);
    let sequential_quiet = a.take_replaced().is_empty()
        && b.take_replaced().is_empty()
        && a.cell_data(s, r4, c4) == b.cell_data(s, r4, c4);
    // Format vs format.
    let (r5, c5) = (43, 3);
    a.disconnect();
    b.disconnect();
    a.set_format(s, r5, c5, bold);
    b.set_format(s, r5, c5, fill);
    reconnect(&mut a, &mut b)?;
    let told5 = a.take_replaced();
    let format_vs_format = format!(
        "A made a cell bold, B filled it, both offline; A reconnected first: final format {} (B's, the later); A was told: {}",
        serde_json::to_string(&a.cell_data(s, r5, c5).and_then(|d| d.st)).unwrap_or_default(),
        told5.first().map(|e| e.message()).unwrap_or_else(|| "nothing".into())
    );
    // Row insert above an edit.
    let (r3, c3) = (50, 1);
    a.disconnect();
    b.disconnect();
    a.edit(vec![Op::Rows { s, at: 45, n: 1 }]);
    b.set_content(s, r3, c3, text("below the insert"));
    reconnect(&mut a, &mut b)?;
    let moved = a
        .cell_data(s, r3 + 1, c3)
        .is_some_and(|d| d.v == Val::Text("below the insert".into()))
        && a.cell_data(s, r3, c3).is_none();
    let insert_vs_edit = format!(
        "A inserted a row at 46, B typed in row 51 concurrently: B's text ends up in row 52: {moved}"
    );
    Ok(ProbeReport {
        value_vs_format,
        value_survived,
        format_survived,
        value_vs_value,
        told_loser,
        sequential_quiet,
        format_vs_format,
        insert_vs_edit,
        insert_vs_edit_ok: moved && a.differences(&b).is_empty(),
    })
}
