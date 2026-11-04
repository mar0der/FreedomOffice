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
    let (mut compared, mut with_spills) = (0, 0);
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
                continue;
            }
            compared += 1;
            if !wb.sheets[0].spill.anchors.is_empty() {
                with_spills += 1;
            }
            assert!(!wb.circular, "seed {seed} step {step}: spurious circular flag");
            let (a, b) = (snapshot(&wb), snapshot(&fresh));
            for (x, y) in a.iter().zip(&b) {
                assert_eq!(x, y, "seed {seed} step {step}: edited {} = {t:?}", cell_name(r, c));
            }
            assert_eq!(wb.sheets[0].spill.anchors, fresh.sheets[0].spill.anchors, "seed {seed} step {step}");
        }
    }
    assert!(compared > 800 && with_spills > 400, "compared {compared}, with spills {with_spills}");
}

// ---------------------------------------------------------------------------
// New functions (expected values from Excel's documentation)
// ---------------------------------------------------------------------------

fn f(src: &str) -> Value {
    let wb = Workbook::new();
    eval_formula(&wb, 0, 0, 0, src)
}

fn fa(src: &str) -> Vec<Vec<Value>> {
    let wb = Workbook::new();
    eval_array(&wb, 0, 0, 0, src)
}

fn t(s: &str) -> Value {
    Value::Text(s.into())
}

#[track_caller]
fn near(src: &str, want: f64, tol: f64) {
    match f(src) {
        Value::Number(x) => assert!((x - want).abs() <= tol, "{src}: got {x}, want {want}"),
        v => panic!("{src}: got {v:?}, want {want}"),
    }
}

#[track_caller]
fn is_err(src: &str, e: ErrorKind) {
    assert_eq!(f(src), Value::Error(e), "{src}");
}

fn nums(rows: &[&[f64]]) -> Vec<Vec<Value>> {
    rows.iter().map(|r| r.iter().map(|x| n(*x)).collect()).collect()
}

const NA: Value = Value::Error(ErrorKind::NA);

#[test]
fn fn_dynamic_arrays() {
    assert_eq!(f("=LET(x,2,y,x+1,x*y)"), n(6.0));
    assert_eq!(f("=LET(x,1/0,IFERROR(x,5))"), n(5.0));
    assert_eq!(fa("=TEXTSPLIT(\"a,b;c,d\",\",\",\";\")"), vec![vec![t("a"), t("b")], vec![t("c"), t("d")]]);
    assert_eq!(fa("=TEXTSPLIT(\"a,b;c\",\",\",\";\")"), vec![vec![t("a"), t("b")], vec![t("c"), NA]]);
    assert_eq!(fa("=TEXTSPLIT(\"a,,b\",\",\",,TRUE)"), vec![vec![t("a"), t("b")]]);
    assert_eq!(fa("=TEXTSPLIT(\"1 x 2 X 3\",\" x \",,,1)"), vec![vec![t("1"), t("2"), t("3")]]);
    assert_eq!(fa("=TEXTSPLIT(\"a-b+c\",{\"-\",\"+\"})"), vec![vec![t("a"), t("b"), t("c")]]);
    assert_eq!(fa("=VSTACK({1,2},{3})"), vec![vec![n(1.0), n(2.0)], vec![n(3.0), NA]]);
    assert_eq!(fa("=HSTACK({1;2},{3;4;5})"), vec![vec![n(1.0), n(3.0)], vec![n(2.0), n(4.0)], vec![NA, n(5.0)]]);
    assert_eq!(fa("=TAKE(SEQUENCE(5),2)"), nums(&[&[1.0], &[2.0]]));
    assert_eq!(fa("=TAKE(SEQUENCE(5),-2)"), nums(&[&[4.0], &[5.0]]));
    assert_eq!(fa("=TAKE(SEQUENCE(2,3),,-1)"), nums(&[&[3.0], &[6.0]]));
    assert_eq!(fa("=DROP(SEQUENCE(5),3)"), nums(&[&[4.0], &[5.0]]));
    assert_eq!(fa("=DROP(SEQUENCE(2,3),,2)"), nums(&[&[3.0], &[6.0]]));
    is_err("=DROP(SEQUENCE(2),2)", ErrorKind::Calc);
    assert_eq!(fa("=CHOOSECOLS(SEQUENCE(2,3),3,1)"), nums(&[&[3.0, 1.0], &[6.0, 4.0]]));
    assert_eq!(fa("=CHOOSEROWS(SEQUENCE(3),-1,{1,2})"), nums(&[&[3.0], &[1.0], &[2.0]]));
    is_err("=CHOOSECOLS(SEQUENCE(2,3),4)", ErrorKind::Value);
    assert_eq!(fa("=SORTBY({\"a\";\"b\";\"c\"},{3;1;2})"), vec![vec![t("b")], vec![t("c")], vec![t("a")]]);
    assert_eq!(fa("=SORTBY({\"a\";\"b\";\"c\"},{1;1;2},-1,{3;2;1},1)"), vec![vec![t("c")], vec![t("b")], vec![t("a")]]);
    assert_eq!(fa("=TOCOL({1,2;3,4})"), nums(&[&[1.0], &[2.0], &[3.0], &[4.0]]));
    assert_eq!(fa("=TOCOL({1,2;3,4},0,TRUE)"), nums(&[&[1.0], &[3.0], &[2.0], &[4.0]]));
    assert_eq!(fa("=TOROW({1,#N/A;3,4},2)"), nums(&[&[1.0, 3.0, 4.0]]));
    assert_eq!(fa("=WRAPROWS({1,2,3,4,5},2)"), vec![vec![n(1.0), n(2.0)], vec![n(3.0), n(4.0)], vec![n(5.0), NA]]);
    assert_eq!(fa("=WRAPCOLS({1,2,3,4,5},2,0)"), nums(&[&[1.0, 3.0, 5.0], &[2.0, 4.0, 0.0]]));
    assert_eq!(fa("=EXPAND({1,2},2,3,0)"), nums(&[&[1.0, 2.0, 0.0], &[0.0, 0.0, 0.0]]));
    is_err("=EXPAND({1,2},1,1)", ErrorKind::Value);
    let r = fa("=RANDARRAY(4,3,1,6,TRUE)");
    assert_eq!((r.len(), r[0].len()), (4, 3));
    assert!(r.iter().flatten().all(|v| matches!(v, Value::Number(x) if (1.0..=6.0).contains(x) && x.fract() == 0.0)));
    assert!(fa("=RANDARRAY(3)").iter().flatten().all(|v| matches!(v, Value::Number(x) if (0.0..1.0).contains(x))));
}

#[test]
fn fn_information_and_text() {
    let mut wb = book(&[("A1", "1"), ("A2", "#DIV/0!"), ("A3", "5"), ("A4", "3"), ("A5", "=SUBTOTAL(9,A1:A4)"), ("B1", "=A1*2")]);
    wb.sheets[0].hidden_rows.insert(2);
    let g = |src: &str| eval_formula(&wb, 0, 9, 9, src);
    assert_eq!(g("=AGGREGATE(9,6,A1:A4)"), n(9.0));
    assert_eq!(g("=AGGREGATE(9,7,A1:A4)"), n(4.0));
    assert_eq!(g("=AGGREGATE(9,4,A1:A4)"), Value::Error(ErrorKind::Div0));
    assert_eq!(g("=AGGREGATE(14,6,A1:A4,1)"), n(5.0));
    assert_eq!(g("=AGGREGATE(15,6,A1:A4,2)"), n(3.0));
    assert_eq!(g("=AGGREGATE(4,6,A1:A5)"), n(5.0));
    assert_eq!(g("=AGGREGATE(3,6,A1:A5)"), n(3.0));
    assert_eq!(g("=AGGREGATE(12,6,A1:A4)"), n(3.0));
    assert_eq!(g("=AGGREGATE(16,6,A1:A4,0.5)"), n(3.0));
    assert_eq!(g("=ISREF(A1)"), Value::Bool(true));
    assert_eq!(g("=ISREF(1)"), Value::Bool(false));
    assert_eq!(g("=FORMULATEXT(B1)"), t("=A1*2"));
    assert_eq!(g("=FORMULATEXT(A1)"), NA);
    assert_eq!(g("=SHEET()"), n(1.0));
    assert_eq!(g("=SHEETS()"), n(1.0));
    assert_eq!(g("=SHEET(\"Sheet1\")"), n(1.0));
    assert_eq!(f("=ADDRESS(2,3)"), t("$C$2"));
    assert_eq!(f("=ADDRESS(2,3,2)"), t("C$2"));
    assert_eq!(f("=ADDRESS(2,3,2,FALSE)"), t("R2C[3]"));
    assert_eq!(f("=ADDRESS(2,3,1,FALSE,\"[Book1]Sheet1\")"), t("'[Book1]Sheet1'!R2C3"));
    assert_eq!(f("=ADDRESS(2,3,1,FALSE,\"EXCEL SHEET\")"), t("'EXCEL SHEET'!R2C3"));
    assert_eq!(f("=HYPERLINK(\"http://example.com\",\"Click\")"), t("Click"));
    assert_eq!(f("=HYPERLINK(\"http://example.com\")"), t("http://example.com"));
    assert_eq!(f("=NUMBERVALUE(\"2.500,27\",\",\",\".\")"), n(2500.27));
    assert_eq!(f("=NUMBERVALUE(\"3.5%\")"), n(0.035));
    assert_eq!(f("=NUMBERVALUE(\" 1 000 \")"), n(1000.0));
    assert_eq!(f("=NUMBERVALUE(\"\")"), n(0.0));
    assert_eq!(f("=DOLLAR(1234.567,2)"), t("$1,234.57"));
    assert_eq!(f("=DOLLAR(1234.567,-2)"), t("$1,200"));
    assert_eq!(f("=DOLLAR(-1234.567,-2)"), t("($1,200)"));
    assert_eq!(f("=DOLLAR(-0.123,4)"), t("($0.1230)"));
    assert_eq!(f("=DOLLAR(99.888)"), t("$99.89"));
}

#[test]
fn fn_dates() {
    near("=YEARFRAC(DATE(2012,1,1),DATE(2012,7,30))", 0.58055556, 1e-8);
    near("=YEARFRAC(DATE(2012,1,1),DATE(2012,7,30),1)", 0.57650273, 1e-8);
    near("=YEARFRAC(DATE(2012,1,1),DATE(2012,7,30),3)", 0.57808219, 1e-8);
    near("=YEARFRAC(DATE(2023,1,31),DATE(2023,3,31))", 60.0 / 360.0, 1e-12);
    near("=YEARFRAC(DATE(2012,7,30),DATE(2012,1,1),2)", 211.0 / 360.0, 1e-12);
    assert_eq!(f("=DAYS360(DATE(2011,1,30),DATE(2011,12,31))"), n(330.0));
    assert_eq!(f("=DAYS360(DATE(2011,1,1),DATE(2011,1,31))"), n(30.0));
    assert_eq!(f("=DAYS360(DATE(2023,2,28),DATE(2023,3,31))"), n(30.0));
    assert_eq!(f("=DAYS360(DATE(2011,1,1),DATE(2011,1,31),TRUE)"), n(29.0));
    assert_eq!(f("=ISOWEEKNUM(DATE(2012,3,9))"), n(10.0));
    assert_eq!(f("=ISOWEEKNUM(DATE(2021,1,1))"), n(53.0));
    assert_eq!(f("=NETWORKDAYS.INTL(DATE(2006,1,1),DATE(2006,1,31))"), n(22.0));
    assert_eq!(f("=NETWORKDAYS.INTL(DATE(2006,2,28),DATE(2006,1,31))"), n(-21.0));
    assert_eq!(f("=NETWORKDAYS.INTL(DATE(2006,1,1),DATE(2006,2,1),7,{\"2006/1/2\",\"2006/1/16\"})"), n(22.0));
    assert_eq!(f("=NETWORKDAYS.INTL(DATE(2006,1,1),DATE(2006,2,1),\"0010001\",{\"2006/1/2\",\"2006/1/16\"})"), n(20.0));
    is_err("=WORKDAY.INTL(DATE(2012,1,1),30,0)", ErrorKind::Num);
    assert_eq!(f("=WORKDAY.INTL(DATE(2012,1,1),90,11)"), n(41013.0));
    assert_eq!(f("=WORKDAY.INTL(DATE(2012,1,1),30,17)"), n(40944.0));
    assert_eq!(f("=DATEVALUE(\"8/22/2011\")"), n(40777.0));
    assert_eq!(f("=DATEVALUE(\"22-MAY-2011\")"), n(40685.0));
    assert_eq!(f("=DATEVALUE(\"2011/02/23\")"), n(40597.0));
    assert_eq!(f("=DATEVALUE(\"2026-11-04T10:30:00\")"), n(46330.0));
    assert_eq!(f("=DATEVALUE(\"Wednesday, November 4, 2026\")"), n(46330.0));
    assert_eq!(f("=DATEVALUE(\"11/4/2026 10:30 AM\")"), n(46330.0));
    assert_eq!(f("=DATEVALUE(\"2/29/1900\")"), n(60.0));
    is_err("=DATEVALUE(\"2/30/2026\")", ErrorKind::Value);
    is_err("=DATEVALUE(46330)", ErrorKind::Value);
}

#[test]
fn fn_engineering_and_math() {
    near("=CONVERT(1,\"lbm\",\"kg\")", 0.45359237, 1e-12);
    near("=CONVERT(68,\"F\",\"C\")", 20.0, 1e-9);
    near("=CONVERT(100,\"ft\",\"m\")", 30.48, 1e-9);
    near("=CONVERT(1,\"mi\",\"km\")", 1.609344, 1e-12);
    near("=CONVERT(1,\"yr\",\"day\")", 365.25, 1e-9);
    near("=CONVERT(90,\"min\",\"hr\")", 1.5, 1e-12);
    near("=CONVERT(1500,\"ms\",\"s\")", 1.5, 1e-12);
    near("=CONVERT(0,\"C\",\"K\")", 273.15, 1e-9);
    near("=CONVERT(100,\"C\",\"F\")", 212.0, 1e-9);
    near("=CONVERT(1,\"stone\",\"lbm\")", 14.0, 1e-9);
    is_err("=CONVERT(2.5,\"ft\",\"sec\")", ErrorKind::NA);
    is_err("=CONVERT(1,\"xyz\",\"m\")", ErrorKind::NA);
    assert_eq!(f("=BIN2DEC(1100100)"), n(100.0));
    assert_eq!(f("=BIN2DEC(1111111111)"), n(-1.0));
    is_err("=BIN2DEC(102)", ErrorKind::Num);
    assert_eq!(f("=DEC2BIN(9,4)"), t("1001"));
    assert_eq!(f("=DEC2BIN(-100)"), t("1110011100"));
    is_err("=DEC2BIN(9,2)", ErrorKind::Num);
    is_err("=DEC2BIN(512)", ErrorKind::Num);
    assert_eq!(f("=DEC2HEX(100,4)"), t("0064"));
    assert_eq!(f("=DEC2HEX(-54)"), t("FFFFFFFFCA"));
    assert_eq!(f("=HEX2DEC(\"A5\")"), n(165.0));
    assert_eq!(f("=HEX2DEC(\"FFFFFFFF5B\")"), n(-165.0));
    assert_eq!(f("=OCT2DEC(54)"), n(44.0));
    assert_eq!(f("=OCT2DEC(\"7777777533\")"), n(-165.0));
    assert_eq!(f("=DEC2OCT(58,3)"), t("072"));
    assert_eq!(f("=DEC2OCT(-100)"), t("7777777634"));
    assert_eq!(f("=BASE(7,2)"), t("111"));
    assert_eq!(f("=BASE(100,16)"), t("64"));
    assert_eq!(f("=BASE(15,2,10)"), t("0000001111"));
    assert_eq!(f("=DECIMAL(\"FF\",16)"), n(255.0));
    assert_eq!(f("=DECIMAL(111,2)"), n(7.0));
    assert_eq!(f("=DECIMAL(\"zap\",36)"), n(45745.0));
    for (form, want) in [(0, "CDXCIX"), (1, "LDVLIV"), (2, "XDIX"), (3, "VDIV"), (4, "ID")] {
        assert_eq!(f(&format!("=ROMAN(499,{form})")), t(want));
    }
    assert_eq!(f("=ROMAN(2013)"), t("MMXIII"));
    assert_eq!(f("=ROMAN(1999,FALSE)"), t("MIM"));
    assert_eq!(f("=ARABIC(\"LVII\")"), n(57.0));
    assert_eq!(f("=ARABIC(\"mcmxii\")"), n(1912.0));
    assert_eq!(f("=ARABIC(\"\")"), n(0.0));
    is_err("=ARABIC(\"ABC\")", ErrorKind::Value);
    assert_eq!(f("=COMBIN(8,2)"), n(28.0));
    assert_eq!(f("=COMBINA(4,3)"), n(20.0));
    assert_eq!(f("=COMBINA(10,3)"), n(220.0));
    assert_eq!(f("=PERMUT(100,3)"), n(970200.0));
    assert_eq!(f("=PERMUT(3,2)"), n(6.0));
    is_err("=COMBIN(2,3)", ErrorKind::Num);
    assert_eq!(f("=MULTINOMIAL(2,3,4)"), n(1260.0));
    assert_eq!(f("=SUMX2MY2({2,3,9,1,8,7,5},{6,5,11,7,5,4,4})"), n(-55.0));
    assert_eq!(f("=SUMX2PY2({2,3,9,1,8,7,5},{6,5,11,7,5,4,4})"), n(521.0));
    assert_eq!(f("=SUMXMY2({2,3,9,1,8,7,5},{6,5,11,7,5,4,4})"), n(79.0));
    is_err("=SUMXMY2({1,2},{1,2,3})", ErrorKind::NA);
}

#[test]
fn fn_statistics() {
    near("=GEOMEAN(4,5,8,7,11,4,3)", 5.476986969, 1e-8);
    near("=HARMEAN(4,5,8,7,11,4,3)", 5.028375962, 1e-8);
    near("=AVEDEV(4,5,6,7,5,4,3)", 1.020408163, 1e-8);
    near("=DEVSQ(4,5,8,7,11,4,3)", 48.0, 1e-9);
    near("=KURT(3,4,5,2,3,4,5,6,4,7)", -0.151799637, 1e-8);
    near("=SKEW(3,4,5,2,3,4,5,6,4,7)", 0.359543, 1e-6);
    near("=SKEW.P(3,4,5,2,3,4,5,6,4,7)", 0.303193, 1e-6);
    near("=STANDARDIZE(42,40,1.5)", 1.333333333, 1e-8);
    near("=NORM.DIST(42,40,1.5,TRUE)", 0.9087888, 1e-7);
    near("=NORM.DIST(42,40,1.5,FALSE)", 0.10934005, 1e-8);
    near("=NORM.INV(0.908789,40,1.5)", 42.000002, 1e-6);
    near("=NORM.S.DIST(1.333333,TRUE)", 0.908788726, 1e-9);
    near("=NORM.S.DIST(1.333333,FALSE)", 0.164010148, 1e-9);
    near("=NORM.S.INV(0.908789)", 1.3333347, 1e-7);
    near("=NORM.S.INV(0.001)", -3.090232306, 1e-9);
    near("=NORMSDIST(-5)", 2.866515719e-7, 1e-15);
    near("=T.DIST(60,1,TRUE)", 0.99469533, 1e-8);
    near("=T.DIST(8,3,FALSE)", 0.00073691, 1e-8);
    near("=T.DIST.2T(1.959999998,60)", 0.054644930, 1e-8);
    near("=T.DIST.RT(1.959999998,60)", 0.027322465, 1e-8);
    near("=T.INV(0.75,2)", 0.8164966, 1e-7);
    near("=T.INV.2T(0.546449,60)", 0.606533, 1e-6);
    near("=TDIST(1.959999998,60,2)", 0.054644930, 1e-8);
    near("=CONFIDENCE.NORM(0.05,2.5,50)", 0.692951912, 1e-8);
    near("=CONFIDENCE.T(0.05,1,50)", 0.284196855, 1e-8);
    near("=FORECAST(30,{6,7,9,15,21},{20,28,31,38,40})", 10.607253, 1e-6);
    near("=FORECAST.LINEAR(30,{6,7,9,15,21},{20,28,31,38,40})", 10.607253, 1e-6);
    near("=SLOPE({2,3,9,1,8,7,5},{6,5,11,7,5,4,4})", 0.305556, 1e-6);
    near("=INTERCEPT({2,3,9,1,8},{6,5,11,7,5})", 0.0483871, 1e-7);
    near("=RSQ({2,3,9,1,8,7,5},{6,5,11,7,5,4,4})", 0.05795, 1e-5);
    near("=PEARSON({9,7,5,3,1},{10,6,1,5,3})", 0.699379, 1e-6);
    near("=STEYX({2,3,9,1,8,7,5},{6,5,11,7,5,4,4})", 3.305719, 1e-6);
    near("=COVARIANCE.P({3,2,4,5,6},{9,7,12,15,17})", 5.2, 1e-12);
    near("=COVARIANCE.S({2,4,8},{5,11,12})", 9.666666667, 1e-8);
    is_err("=SLOPE({1,2},{1,2,3})", ErrorKind::NA);
    assert_eq!(fa("=TREND({1,2,3},{1,2,3},{4,5})"), nums(&[&[4.0, 5.0]]));
    let g = fa("=GROWTH({33100,47300,69000,102000,150000,220000},{11,12,13,14,15,16},{17;18})");
    for (v, want) in g.iter().flatten().zip([320196.7184, 468536.0539]) {
        assert!(matches!(v, Value::Number(x) if (x - want).abs() < 1e-3), "{v:?} vs {want}");
    }
    let l = fa("=LINEST({2,3,5,4,6},{1,2,3,4,5},TRUE,TRUE)");
    let want = [[0.9, 1.3], [0.251661148, 0.834664617], [0.81, 0.795822426], [12.78947368, 3.0], [8.1, 1.9]];
    for (row, w) in l.iter().zip(want) {
        for (v, x) in row.iter().zip(w) {
            assert!(matches!(v, Value::Number(y) if (y - x).abs() < 1e-6), "{v:?} vs {x}");
        }
    }
    assert_eq!(fa("=LINEST({1,9,5,7},{0,4,2,3})"), nums(&[&[2.0, 1.0]]));
    for (x, want) in [(2.0, 0.333), (4.0, 0.555), (8.0, 0.666), (5.0, 0.583)] {
        near(&format!("=PERCENTRANK.INC({{13,12,11,8,4,3,2,1,1,1}},{x})"), want, 1e-12);
    }
    near("=PERCENTRANK.EXC({1,2,3,6,6,6,7,8,9},7)", 0.7, 1e-12);
    near("=PERCENTRANK.EXC({1,2,3,6,6,6,7,8,9},5.43)", 0.381, 1e-12);
    near("=PERCENTRANK.EXC({1,2,3,6,6,6,7,8,9},5.43,1)", 0.3, 1e-12);
    is_err("=PERCENTRANK({1,2,3},4)", ErrorKind::NA);
    assert_eq!(f("=RANK.AVG(94,{89,88,92,101,94,97,95})"), n(4.0));
    assert_eq!(f("=RANK.AVG(3.5,{7,3.5,3.5,1,2},1)"), n(3.5));
    near("=PERCENTILE.EXC({1,2,3,6,6,6,7,8,9},0.25)", 2.5, 1e-12);
    is_err("=PERCENTILE.EXC({1,2,3,6,6,6,7,8,9},0.01)", ErrorKind::Num);
    near("=QUARTILE.EXC({6,7,15,36,39,40,41,42,43,47,49},1)", 15.0, 1e-12);
    near("=QUARTILE.EXC({6,7,15,36,39,40,41,42,43,47,49},3)", 43.0, 1e-12);
    assert_eq!(fa("=MODE.MULT({1,2,3,4,3,2,1,2,3,5,6,1})"), nums(&[&[1.0], &[2.0], &[3.0]]));
    is_err("=MODE.MULT({1,2,3})", ErrorKind::NA);
}

#[test]
fn fn_financial() {
    near("=CUMIPMT(0.09/12,360,125000,13,24,0)", -11135.23213, 1e-5);
    near("=CUMIPMT(0.09/12,360,125000,1,1,0)", -937.5, 1e-9);
    near("=CUMPRINC(0.09/12,360,125000,13,24,0)", -934.1071234, 1e-6);
    near("=CUMPRINC(0.09/12,360,125000,1,1,0)", -68.27827118, 1e-7);
    is_err("=CUMIPMT(0.09/12,360,125000,0,1,0)", ErrorKind::Num);
    near("=EFFECT(0.0525,4)", 0.053542667, 1e-9);
    near("=NOMINAL(0.053543,4)", 0.052500319, 1e-9);
    near("=SLN(30000,7500,10)", 2250.0, 1e-9);
    near("=SYD(30000,7500,10,1)", 4090.909091, 1e-6);
    near("=SYD(30000,7500,10,10)", 409.0909091, 1e-6);
    near("=DB(1000000,100000,6,1,7)", 186083.33, 0.01);
    near("=DB(1000000,100000,6,2,7)", 259639.42, 0.01);
    near("=DB(1000000,100000,6,7,7)", 15845.10, 0.01);
    near("=DDB(2400,300,3650,1)", 1.32, 0.005);
    near("=DDB(2400,300,120,1,2)", 40.0, 1e-9);
    near("=DDB(2400,300,10,1,2)", 480.0, 1e-9);
    near("=DDB(2400,300,10,2,1.5)", 306.0, 1e-9);
    near("=DDB(2400,300,10,10)", 22.12, 0.005);
    near("=XNPV(0.09,{-10000,2750,4250,3250,2750},{39448,39508,39751,39859,39904})", 2086.647602, 1e-5);
    near("=XIRR({-10000,2750,4250,3250,2750},{39448,39508,39751,39859,39904})", 0.373362535, 1e-8);
    near("=MIRR({-120000,39000,30000,21000,37000,46000},0.1,0.12)", 0.126094, 1e-6);
    near("=MIRR({-120000,39000,30000,21000},0.1,0.12)", -0.048044655, 1e-8);
    near("=MIRR({-120000,39000,30000,21000,37000,46000},0.1,0.14)", 0.134759111, 1e-8);
    near("=FVSCHEDULE(1,{0.09,0.11,0.1})", 1.33089, 1e-12);
    near("=PDURATION(0.025,2000,2200)", 3.859866163, 1e-8);
    near("=PDURATION(0.025/12,1000,1200)", 87.60547642, 1e-7);
    near("=RRI(96,10000,11000)", 0.000993307, 1e-9);
}

#[test]
fn fn_conditional_aggregates_with_dates() {
    let wb = book(&[
        ("A1", "1/10/2024"),
        ("A2", "1/15/2024"),
        ("A3", "1/20/2024"),
        ("A4", "2/1/2024"),
        ("A5", "2024-02-10"),
        ("B1", "10"),
        ("B2", "20"),
        ("B3", "30"),
        ("B4", "40"),
        ("B5", "50"),
    ]);
    let g = |src: &str| eval_formula(&wb, 0, 9, 9, src);
    assert_eq!(g("=SUMIFS(B1:B5,A1:A5,\">=\"&DATE(2024,1,15),A1:A5,\"<\"&DATE(2024,2,1))"), n(50.0));
    assert_eq!(g("=SUMIFS(B1:B5,A1:A5,\">=1/15/2024\",A1:A5,\"<2/1/2024\")"), n(50.0));
    assert_eq!(g("=COUNTIFS(A1:A5,\">=2024-01-20\")"), n(3.0));
    assert_eq!(g("=AVERAGEIFS(B1:B5,A1:A5,\"<=\"&DATE(2024,1,20))"), n(20.0));
    assert_eq!(g("=COUNTIF(A:A,DATE(2024,1,15))"), n(1.0));
    assert_eq!(g("=SUMIF(A1:A5,\"1/20/2024\",B1:B5)"), n(30.0));
    assert_eq!(g("=COUNTIFS(A1:A5,\">\"&A2,B1:B5,\"<50\")"), n(2.0));
    // criteria arrays are lifted: one result per criterion
    assert_eq!(eval_array(&wb, 0, 9, 9, "=COUNTIF(B1:B5,{\">20\",\"<20\"})"), nums(&[&[3.0, 1.0]]));
}
