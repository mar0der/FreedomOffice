//! Engine tests: dynamic arrays / spilling, lifting and incremental recalculation.

use super::{eval_array, eval_formula, recalc, recalc_cells, recalc_ranges};
use crate::model::*;

fn put(wb: &mut Workbook, s: usize, a1: &str, text: &str) {
    let (r, c) = parse_cell(a1).unwrap();
    let cell = cell_from_input(wb, text, 0);
    wb.sheets[s].set(r, c, cell);
}

/// Sets a cell and recalculates incrementally (like typing it).
fn type_in(wb: &mut Workbook, a1: &str, text: &str) {
    let (r, c) = parse_cell(a1).unwrap();
    let cell = cell_from_input(wb, text, 0);
    wb.sheets[0].set(r, c, cell);
    recalc_cells(wb, &[(0, r, c)]);
}

fn shown(wb: &Workbook, a1: &str) -> Value {
    let (r, c) = parse_cell(a1).unwrap();
    wb.sheets[0].display_value(r, c).clone()
}

fn n(v: f64) -> Value {
    Value::Number(v)
}

fn book(cells: &[(&str, &str)]) -> Workbook {
    let mut wb = Workbook::new();
    for (a, t) in cells {
        put(&mut wb, 0, a, t);
    }
    recalc(&mut wb);
    wb
}

#[test]
fn spill_basics() {
    let wb = book(&[("A1", "1"), ("A2", "2"), ("A3", "3"), ("B1", "=A1:A3*2"), ("C1", "=SEQUENCE(2,3)"), ("H1", "=SUM(B1#)")]);
    assert_eq!(shown(&wb, "B1"), n(2.0));
    assert_eq!(shown(&wb, "B2"), n(4.0));
    assert_eq!(shown(&wb, "B3"), n(6.0));
    assert!(wb.sheets[0].get(1, 1).is_none(), "spilled cells are not real cells");
    assert_eq!(shown(&wb, "E2"), n(6.0));
    assert_eq!(shown(&wb, "H1"), n(12.0));
    assert_eq!(wb.sheets[0].spill_range(0, 1), Some((Range::new(0, 1, 2, 1), false)));
    assert_eq!(wb.sheets[0].spill_anchor_of(2, 1), Some((0, 1)));
    assert!(wb.sheets[0].is_populated(2, 1));
    // other formulas see spilled values, including through ranges and whole columns
    assert_eq!(eval_formula(&wb, 0, 10, 10, "=B3+SUM(B:B)+COUNT(C1:E2)"), n(6.0 + 12.0 + 6.0));
    assert_eq!(eval_formula(&wb, 0, 10, 10, "=ROWS(C1#)*10+COLUMNS(C1#)"), n(23.0));
    assert_eq!(eval_formula(&wb, 0, 10, 10, "=A1#"), Value::Error(ErrorKind::Ref));
}

#[test]
fn spill_blocked_and_unblocked() {
    let mut wb = book(&[("A1", "=SEQUENCE(3)"), ("B1", "=A3")]);
    assert_eq!(shown(&wb, "B1"), n(3.0));
    type_in(&mut wb, "A2", "x");
    assert_eq!(shown(&wb, "A1"), Value::Error(ErrorKind::Spill));
    assert_eq!(shown(&wb, "A3"), Value::Empty);
    assert_eq!(shown(&wb, "B1"), n(0.0));
    assert_eq!(wb.sheets[0].spill_range(0, 0), Some((Range::new(0, 0, 2, 0), true)));
    type_in(&mut wb, "A2", "");
    assert_eq!(shown(&wb, "A1"), n(1.0));
    assert_eq!(shown(&wb, "A3"), n(3.0));
    assert_eq!(shown(&wb, "B1"), n(3.0));
    // beyond the sheet edge
    let wb = book(&[("A1048575", "=SEQUENCE(3)")]);
    assert_eq!(wb.sheets[0].value(1_048_574, 0), &Value::Error(ErrorKind::Spill));
}

#[test]
fn spill_resizes_and_dependents_follow() {
    let mut wb = book(&[("A1", "3"), ("B1", "=SEQUENCE(A1)"), ("C1", "=SUM(B:B)"), ("D1", "=B4"), ("E1", "=COUNT(B1#)")]);
    assert_eq!(shown(&wb, "C1"), n(6.0));
    assert_eq!(shown(&wb, "D1"), n(0.0));
    type_in(&mut wb, "A1", "5");
    assert_eq!(shown(&wb, "B5"), n(5.0));
    assert_eq!(shown(&wb, "C1"), n(15.0));
    assert_eq!(shown(&wb, "D1"), n(4.0));
    assert_eq!(shown(&wb, "E1"), n(5.0));
    type_in(&mut wb, "A1", "2");
    assert_eq!(shown(&wb, "B3"), Value::Empty);
    assert_eq!(shown(&wb, "C1"), n(3.0));
    assert_eq!(shown(&wb, "D1"), n(0.0));
    assert_eq!(shown(&wb, "E1"), n(2.0));
    // deleting the anchor clears its spill
    type_in(&mut wb, "B1", "");
    assert_eq!(shown(&wb, "B2"), Value::Empty);
    assert_eq!(shown(&wb, "C1"), n(0.0));
}

#[test]
fn chained_spills_and_formula_reading_spill_before_anchor() {
    // C1 is evaluated before the anchor E1 in row-major order and must still see its values.
    let wb = book(&[("A1", "=SUM(E1:E5)"), ("B1", "=E3*10"), ("E1", "=SEQUENCE(5)"), ("F1", "=E1#*2"), ("G1", "=SUM(F1#)")]);
    assert_eq!(shown(&wb, "A1"), n(15.0));
    assert_eq!(shown(&wb, "B1"), n(30.0));
    assert_eq!(shown(&wb, "F5"), n(10.0));
    assert_eq!(shown(&wb, "G1"), n(30.0));
}

#[test]
fn implicit_intersection_and_lifting() {
    let wb = book(&[("A1", "1"), ("A2", "-2"), ("A3", "3"), ("B2", "=@A1:A3*2"), ("C1", "=ABS(A1:A3)"), ("D1", "=IF(A1:A3>0,\"p\",\"n\")")]);
    assert_eq!(shown(&wb, "B2"), n(-4.0));
    assert_eq!(wb.sheets[0].spill_range(1, 1), None);
    assert_eq!(shown(&wb, "C2"), n(2.0));
    assert_eq!(shown(&wb, "D2"), Value::Text("n".into()));
    assert_eq!(shown(&wb, "D3"), Value::Text("p".into()));
    // scalar results stay scalar; ranges passed to aggregate functions are not lifted
    assert_eq!(eval_formula(&wb, 0, 9, 9, "=SUM(ABS(A1:A3))"), n(6.0));
    assert_eq!(eval_array(&wb, 0, 9, 9, "=LEN({\"ab\",\"c\"})"), vec![vec![n(2.0), n(1.0)]]);
    assert_eq!(eval_array(&wb, 0, 9, 9, "=ROW(A2:A4)"), vec![vec![n(2.0)], vec![n(3.0)], vec![n(4.0)]]);
    assert_eq!(eval_array(&wb, 0, 9, 9, "={1,2,3}+{10;20}"), vec![vec![n(11.0), n(12.0), n(13.0)], vec![n(21.0), n(22.0), n(23.0)]]);
    assert_eq!(eval_array(&wb, 0, 9, 9, "={1,2,3}+{10,20}")[0][2], Value::Error(ErrorKind::NA));
}

#[test]
fn incremental_recalc_only_touches_dependents() {
    let mut wb = book(&[("A1", "1"), ("A2", "=A1*2"), ("A3", "=A2+1"), ("B1", "=SUM(A:A)"), ("C1", "5"), ("C2", "=C1*3")]);
    assert_eq!(shown(&wb, "B1"), n(6.0));
    // poison an unrelated formula's stored value: an incremental recalc must not touch it
    wb.sheets[0].get_mut(1, 2).unwrap().value = n(-1.0);
    type_in(&mut wb, "A1", "10");
    assert_eq!(shown(&wb, "A3"), n(21.0));
    assert_eq!(shown(&wb, "B1"), n(51.0));
    assert_eq!(shown(&wb, "C2"), n(-1.0));
    recalc(&mut wb);
    assert_eq!(shown(&wb, "C2"), n(15.0));
}

#[test]
fn incremental_circular_detection() {
    let mut wb = book(&[("A1", "=B1+1"), ("B1", "1")]);
    assert!(!wb.circular);
    type_in(&mut wb, "B1", "=A1");
    assert!(wb.circular);
    type_in(&mut wb, "B1", "2");
    assert!(!wb.circular);
    assert_eq!(shown(&wb, "A1"), n(3.0));
}

#[test]
fn volatile_and_names_in_incremental() {
    let mut wb = book(&[("A1", "1"), ("B1", "=INDIRECT(\"A\"&C1)"), ("C1", "1"), ("A5", "7")]);
    wb.names.push(("rate".into(), "Sheet1!$D$1".into()));
    put(&mut wb, 0, "D1", "0.5");
    put(&mut wb, 0, "E1", "=rate*100");
    recalc(&mut wb);
    assert_eq!(shown(&wb, "E1"), n(50.0));
    type_in(&mut wb, "D1", "0.25");
    assert_eq!(shown(&wb, "E1"), n(25.0));
    type_in(&mut wb, "C1", "5");
    assert_eq!(shown(&wb, "B1"), n(7.0));
    type_in(&mut wb, "A5", "8");
    assert_eq!(shown(&wb, "B1"), n(8.0));
}

#[test]
fn deep_chain_does_not_overflow() {
    // Upward chain: A1 depends on A2 ... the last row; evaluated recursively.
    let mut wb = Workbook::new();
    let n_rows = 20_000;
    for r in 0..n_rows {
        let t = if r == n_rows - 1 { "1".to_string() } else { format!("=A{}+1", r + 2) };
        let cell = cell_from_input(&mut wb, &t, 0);
        wb.sheets[0].set(r, 0, cell);
    }
    recalc(&mut wb);
    assert!(!wb.circular);
    assert_eq!(wb.sheets[0].value(0, 0), &n(n_rows as f64));
    type_in(&mut wb, &format!("A{n_rows}"), "2");
    assert_eq!(wb.sheets[0].value(0, 0), &n(n_rows as f64 + 1.0));
}

#[test]
fn paste_range_incremental() {
    let mut wb = book(&[("A1", "1"), ("A2", "2"), ("B1", "=SUM(A1:A3)")]);
    for (a, t) in [("A2", "5"), ("A3", "7")] {
        put(&mut wb, 0, a, t);
    }
    recalc_ranges(&mut wb, &[(0, Range::new(1, 0, 2, 0))]);
    assert_eq!(shown(&wb, "B1"), n(13.0));
}

// ---------------------------------------------------------------------------
// Differential test: incremental recalculation must match a fresh full recalculation.
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const ROWS: u64 = 24;

fn pick(rng: &mut Rng, cols: &[&str], max_row: u64) -> String {
    format!("{}{}", cols[rng.below(cols.len() as u64) as usize], rng.below(max_row.max(1)) + 1)
}

/// Random cell content. Columns form layers so the workbook never has cycles:
/// A,B -> earlier rows of A,B; C -> A,B; F (spills) -> A,B,C; D,E -> A,B,C,F and earlier D,E.
fn random_entry(rng: &mut Rng, r: u32, c: u32) -> String {
    let r = r as u64;
    if rng.below(5) == 0 {
        return if rng.below(3) == 0 { String::new() } else { format!("{}", rng.below(10)) };
    }
    match c {
        0 | 1 => {
            if r == 0 {
                return format!("{}", rng.below(10));
            }
            match rng.below(3) {
                0 => format!("={}+{}", pick(rng, &["A", "B"], r), pick(rng, &["A", "B"], r)),
                1 => format!("=SUM(A1:B{r})"),
                _ => format!("{}", rng.below(10)),
            }
        }
        2 => match rng.below(4) {
            0 => format!("={}*2+{}", pick(rng, &["A", "B"], ROWS), pick(rng, &["A", "B"], ROWS)),
            1 => format!("=IF({}>4,{},1)", pick(rng, &["A", "B"], ROWS), pick(rng, &["A", "B"], ROWS)),
            2 => format!("=VLOOKUP({},A1:B{ROWS},2,FALSE)", rng.below(10)),
            _ => {
                let a = rng.below(ROWS) + 1;
                format!("=SUM(A{a}:B{})", a + rng.below(5))
            }
        },
        5 => {
            // Spill anchors live at rows 1, 6, 11, ... of column F and never overlap each other.
            if r % 5 != 0 {
                return if rng.below(3) == 0 { format!("{}", rng.below(9)) } else { String::new() };
            }
            let r0 = rng.below(ROWS - 4) + 1;
            match rng.below(6) {
                0 => format!("=SEQUENCE({})", rng.below(4) + 1),
                1 => format!("=A{r0}:A{}*2", r0 + rng.below(4)),
                2 => format!("=FILTER(B{r0}:B{},B{r0}:B{}>3,\"none\")", r0 + 3, r0 + 3),
                3 => format!("=SORT(C{r0}:C{})", r0 + 2),
                4 => format!("={}", pick(rng, &["A", "B", "C"], ROWS)),
                _ => String::new(),
            }
        }
        _ => {
            let lower = |rng: &mut Rng| pick(rng, &["A", "B", "C", "F"], ROWS);
            match rng.below(7) {
                0 => format!("={}+{}", lower(rng), lower(rng)),
                1 => format!("=SUM(F{}#)", 5 * rng.below(ROWS / 5) + 1),
                2 => "=COUNT(F:F)+SUM(F:F)".into(),
                3 => format!("=IF({}>4,{},{})", lower(rng), lower(rng), rng.below(5)),
                4 if r > 0 => format!("={}+1", pick(rng, &["D", "E"], r)),
                5 => format!("=IFERROR(F{}*2,-1)", rng.below(ROWS) + 1),
                _ => format!("=SUM(C1:C{0})+SUM(F1:F{0})", rng.below(ROWS) + 1),
            }
        }
    }
}

fn snapshot(wb: &Workbook) -> Vec<(u32, u32, String)> {
    let mut out = Vec::new();
    for r in 0..ROWS as u32 + 2 {
        for c in 0..6 {
            let v = wb.sheets[0].display_value(r, c);
            let t = match v {
                Value::Number(x) => format!("{:.9}", x),
                v => format!("{v:?}"),
            };
            out.push((r, c, t));
        }
    }
    out
}

#[test]
fn incremental_matches_full_recalc() {
    let (mut compared, mut skipped, mut with_spills) = (0, 0, 0);
    for seed in 1..=6u64 {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ (seed * 0x1234_5678));
        let mut wb = Workbook::new();
        for r in 0..ROWS as u32 {
            for c in 0..6 {
                let t = random_entry(&mut rng, r, c);
                let cell = cell_from_input(&mut wb, &t, 0);
                wb.sheets[0].set(r, c, cell);
            }
        }
        recalc(&mut wb);
        for step in 0..150 {
            let (r, c) = (rng.below(ROWS) as u32, rng.below(6) as u32);
            let t = random_entry(&mut rng, r, c);
            let cell = cell_from_input(&mut wb, &t, 0);
            wb.sheets[0].set(r, c, cell);
            recalc_cells(&mut wb, &[(0, r, c)]);

            let mut fresh = wb.clone();
            fresh.sheets[0].spill = Spill::default();
            recalc(&mut fresh);
            if fresh.circular {
                skipped += 1;
                continue;
            }
            compared += 1;
            if !wb.sheets[0].spill.anchors.is_empty() { with_spills += 1; }
            assert!(!wb.circular, "seed {seed} step {step}: spurious circular flag");
            let (a, b) = (snapshot(&wb), snapshot(&fresh));
            for (x, y) in a.iter().zip(&b) {
                assert_eq!(x, y, "seed {seed} step {step}: edited {} = {t:?}", cell_name(r, c));
            }
            assert_eq!(wb.sheets[0].spill.anchors, fresh.sheets[0].spill.anchors, "seed {seed} step {step}");
        }
    }
    eprintln!("compared {compared} skipped {skipped} with_spills {with_spills}");
}
