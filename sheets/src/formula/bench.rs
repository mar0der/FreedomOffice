//! Recalculation benchmark (ignored by default).
//!
//! Run with: `cargo test --release -p freedom-sheets bench_recalc -- --ignored --nocapture`

use std::time::Instant;

use crate::model::{cell_from_input, Workbook};

const N: u32 = 25_000;

/// 200k cells: 100k values (A, B) + 100k formulas (C..F) plus a few whole-column aggregates.
pub fn build() -> Workbook {
    let mut wb = Workbook::new();
    let rows = 2 * N;
    for r in 0..rows {
        let a = cell_from_input(&mut wb, &format!("{}", (r * 7) % 1000), 0);
        wb.sheets[0].set(r, 0, a);
        let b = cell_from_input(&mut wb, &format!("k{r}"), 0);
        wb.sheets[0].set(r, 1, b);
    }
    for r in 0..N {
        let i = r + 1;
        // C: running chain
        let c = if r == 0 { "=A1".to_string() } else { format!("=C{}+A{i}", i - 1) };
        // D: windowed SUM
        let d = format!("=SUM(A{i}:A{})", i + 9);
        // E: exact-match VLOOKUP into the key/value table
        let e = format!("=VLOOKUP(\"k\"&MOD({i}*13,{rows}),$B$1:$C${rows},2,FALSE)");
        // F: simple arithmetic on the others
        let f = format!("=IF(E{i}>D{i},E{i}-D{i},D{i}*2)");
        for (col, text) in [(2, c), (3, d), (4, e), (5, f)] {
            let cell = cell_from_input(&mut wb, &text, 0);
            wb.sheets[0].set(r, col, cell);
        }
    }
    // Chain continues in C for rows N..2N so the lookup table is fully formula-backed.
    for r in N..rows {
        let i = r + 1;
        let cell = cell_from_input(&mut wb, &format!("=C{}+A{i}", i - 1), 0);
        wb.sheets[0].set(r, 2, cell);
    }
    for (r, text) in ["=SUM(A:A)", "=COUNTIF(B:B,\"k1*\")", "=SUMIF(A:A,\">500\")", "=MAX(C:C)"].iter().enumerate() {
        let cell = cell_from_input(&mut wb, text, 0);
        wb.sheets[0].set(r as u32, 7, cell);
    }
    wb
}

#[test]
#[ignore]
fn bench_recalc() {
    let t = Instant::now();
    let mut wb = build();
    let formulas: usize = wb.sheets[0].rows.values().map(|r| r.values().filter(|c| c.formula.is_some()).count()).sum();
    let cells: usize = wb.sheets[0].rows.values().map(|r| r.len()).sum();
    println!("built {cells} cells / {formulas} formulas in {:?}", t.elapsed());

    let t = Instant::now();
    wb.recalc();
    println!("full recalc #1: {:?}", t.elapsed());
    let t = Instant::now();
    wb.recalc();
    println!("full recalc #2: {:?}", t.elapsed());
    let total = crate::formula::eval_formula(&wb, 0, 0, 10, "=SUM(C:F)");
    println!("checksum SUM(C:F) = {total:?}");

    let mut edit = |label: &str, a1: &str, text: &str| {
        let (r, c) = crate::model::parse_cell(a1).unwrap();
        let cell = cell_from_input(&mut wb, text, 0);
        wb.sheets[0].set(r, c, cell);
        let t = Instant::now();
        crate::formula::recalc_cells(&mut wb, &[(0, r, c)]);
        println!("incremental edit {label:<38} {:?}", t.elapsed());
    };
    edit("Z1 (no dependents, builds graph)", "Z1", "1");
    edit("Z2 (no dependents)", "Z2", "1");
    edit("D7 formula (one dependent)", "D7", "=SUM(A7:A20)");
    edit("A49990 (chain tail + all lookups)", &format!("A{}", 2 * N - 10), "5");
    edit("A1 (everything)", "A1", "3");
    let mut fresh = wb.clone();
    fresh.recalc();
    let a = crate::formula::eval_formula(&wb, 0, 0, 10, "=SUM(C:H)");
    let b = crate::formula::eval_formula(&fresh, 0, 0, 10, "=SUM(C:H)");
    println!("incremental checksum {a:?} vs full {b:?}");
    assert_eq!(a, b);
}
