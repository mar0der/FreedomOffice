//! Excel parity tests: LAMBDA family, reference operators, 3-D / external references and the
//! function library additions. Expected values are Excel's (documentation examples where they
//! exist).

use super::{eval_array, eval_formula, parse, print, print_xlsx, recalc};
use crate::model::*;

fn put(wb: &mut Workbook, s: usize, a1: &str, text: &str) {
    let (r, c) = parse_cell(a1).unwrap();
    let cell = cell_from_input(wb, text, 0);
    wb.sheets[s].set(r, c, cell);
}

fn book(cells: &[(&str, &str)]) -> Workbook {
    let mut wb = Workbook::new();
    for (a, t) in cells {
        put(&mut wb, 0, a, t);
    }
    recalc(&mut wb);
    wb
}

fn ev(wb: &Workbook, f: &str) -> Value {
    eval_formula(wb, 0, 50, 10, f)
}

fn arr(wb: &Workbook, f: &str) -> Vec<Vec<Value>> {
    eval_array(wb, 0, 50, 10, f)
}

fn n(v: f64) -> Value {
    Value::Number(v)
}

fn t(s: &str) -> Value {
    Value::Text(s.into())
}

fn err(e: ErrorKind) -> Value {
    Value::Error(e)
}

fn nums(rows: &[&[f64]]) -> Vec<Vec<Value>> {
    rows.iter().map(|r| r.iter().map(|x| n(*x)).collect()).collect()
}

#[track_caller]
fn close(v: Value, want: f64, tol: f64) {
    match v {
        Value::Number(x) => assert!((x - want).abs() <= tol * want.abs().max(1e-300) || (x - want).abs() < 1e-12, "got {x}, want {want}"),
        other => panic!("got {other:?}, want {want}"),
    }
}

// ---------------------------------------------------------------------------
// LAMBDA
// ---------------------------------------------------------------------------

#[test]
fn lambda_calls_and_closures() {
    let mut wb = book(&[("A1", "1"), ("A2", "2"), ("A3", "3"), ("B1", "10"), ("B2", "20"), ("B3", "30")]);
    assert_eq!(ev(&wb, "=LAMBDA(x,x+1)(2)"), n(3.0));
    assert_eq!(ev(&wb, "=LAMBDA(x,y,x*y)(3,4)"), n(12.0));
    assert_eq!(ev(&wb, "=LET(f,LAMBDA(x,y,x*y),f(3,4))"), n(12.0));
    assert_eq!(ev(&wb, "=LET(k,10,f,LAMBDA(x,x+k),f(1))"), n(11.0));
    // lexical scope: `y` is not visible inside the lambda
    assert_eq!(ev(&wb, "=LET(f,LAMBDA(x,x+y),y,5,f(1))"), err(ErrorKind::Name));
    // uncalled lambda, too many arguments, bad parameters
    assert_eq!(ev(&wb, "=LAMBDA(x,x+1)"), err(ErrorKind::Calc));
    assert_eq!(ev(&wb, "=LAMBDA(x,x)(1,2)"), err(ErrorKind::Value));
    assert_eq!(ev(&wb, "=LAMBDA(x,x,1)(1,2)"), err(ErrorKind::Value));
    // errors are passed as values
    assert_eq!(ev(&wb, "=LAMBDA(x,IFERROR(x,\"e\"))(1/0)"), t("e"));
    // references stay references inside the body
    assert_eq!(ev(&wb, "=LAMBDA(r,SUM(r))(A1:A3)"), n(6.0));
    // ISOMITTED and omitted parameters
    let f = "LAMBDA(a,b,IF(ISOMITTED(b),\"no b\",a+b))";
    assert_eq!(ev(&wb, &format!("={f}(1)")), t("no b"));
    assert_eq!(ev(&wb, &format!("={f}(1,)")), t("no b"));
    assert_eq!(ev(&wb, &format!("={f}(1,2)")), n(3.0));
    assert_eq!(ev(&wb, "=LAMBDA(a,b,a+b)(1)"), n(1.0));
    // named lambdas, including recursion
    wb.names.push(("DOUBLE".into(), "=LAMBDA(x,x*2)".into()));
    wb.names.push(("FACTR".into(), "LAMBDA(n,IF(n<=1,1,n*FACTR(n-1)))".into()));
    wb.names.push(("RATE2".into(), "LAMBDA(x,x*Sheet1!$B$1)".into()));
    assert_eq!(ev(&wb, "=DOUBLE(3)"), n(6.0));
    assert_eq!(ev(&wb, "=double(A3)+1"), n(7.0));
    assert_eq!(ev(&wb, "=MAP(A1:A3,DOUBLE)"), n(2.0));
    assert_eq!(ev(&wb, "=NOTDEFINED(1)"), err(ErrorKind::Name));
    // named lambdas recalculate when the cells they read change; recursion runs on the
    // recalculation thread's large stack
    put(&mut wb, 0, "C1", "=RATE2(2)");
    put(&mut wb, 0, "D1", "=FACTR(5)");
    put(&mut wb, 0, "D2", "=FACTR(20)");
    recalc(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 2), &n(20.0));
    assert_eq!(wb.sheets[0].value(0, 3), &n(120.0));
    assert_eq!(wb.sheets[0].value(1, 3), &n(2_432_902_008_176_640_000.0));
    let (r, c) = parse_cell("B1").unwrap();
    let cell = cell_from_input(&mut wb, "7", 0);
    wb.sheets[0].set(r, c, cell);
    super::recalc_cells(&mut wb, &[(0, r, c)]);
    assert_eq!(wb.sheets[0].value(0, 2), &n(14.0));
}

#[test]
fn lambda_helpers() {
    let wb = book(&[("A1", "1"), ("A2", "2"), ("A3", "3"), ("B1", "10"), ("B2", "20"), ("B3", "30"), ("C1", "x"), ("C3", "y")]);
    assert_eq!(arr(&wb, "=MAP({1,2,3},LAMBDA(x,x*2))"), nums(&[&[2.0, 4.0, 6.0]]));
    assert_eq!(arr(&wb, "=MAP(A1:A3,B1:B3,LAMBDA(a,b,a+b))"), nums(&[&[11.0], &[22.0], &[33.0]]));
    assert_eq!(arr(&wb, "=MAP(C1:C3,LAMBDA(c,ISBLANK(c)))"), vec![vec![Value::Bool(false)], vec![Value::Bool(true)], vec![Value::Bool(false)]]);
    assert_eq!(arr(&wb, "=MAP({1,2},LAMBDA(x,{1,2}))"), vec![vec![err(ErrorKind::Calc), err(ErrorKind::Calc)]]);
    assert_eq!(ev(&wb, "=REDUCE(0,{1,2,3,4},LAMBDA(a,v,a+v))"), n(10.0));
    assert_eq!(ev(&wb, "=REDUCE(1,A1:A3,LAMBDA(a,v,a*v))"), n(6.0));
    assert_eq!(arr(&wb, "=REDUCE(0,{1,2},LAMBDA(a,v,VSTACK(a,v)))"), nums(&[&[0.0], &[1.0], &[2.0]]));
    assert_eq!(arr(&wb, "=SCAN(0,{1,2,3},LAMBDA(a,v,a+v))"), nums(&[&[1.0, 3.0, 6.0]]));
    assert_eq!(arr(&wb, "=SCAN(\"\",{\"a\",\"b\";\"c\",\"d\"},LAMBDA(a,v,a&v))"), vec![vec![t("a"), t("ab")], vec![t("abc"), t("abcd")]]);
    assert_eq!(arr(&wb, "=BYROW({1,2;3,4},LAMBDA(r,SUM(r)))"), nums(&[&[3.0], &[7.0]]));
    assert_eq!(arr(&wb, "=BYCOL({1,2;3,4},LAMBDA(c,MAX(c)))"), nums(&[&[3.0, 4.0]]));
    assert_eq!(arr(&wb, "=BYROW(A1:B3,SUM)"), nums(&[&[11.0], &[22.0], &[33.0]]));
    assert_eq!(arr(&wb, "=BYCOL(A1:B3,LAMBDA(c,ROWS(c)))"), nums(&[&[3.0, 3.0]]));
    assert_eq!(arr(&wb, "=BYROW({1,2;3,4},LAMBDA(r,r*2))"), vec![vec![err(ErrorKind::Calc)], vec![err(ErrorKind::Calc)]]);
    assert_eq!(arr(&wb, "=MAKEARRAY(2,3,LAMBDA(r,c,r*c))"), nums(&[&[1.0, 2.0, 3.0], &[2.0, 4.0, 6.0]]));
    assert_eq!(ev(&wb, "=MAKEARRAY(0,3,LAMBDA(r,c,r))"), err(ErrorKind::Value));
    assert_eq!(ev(&wb, "=MAP({1,2},5)"), err(ErrorKind::Value));
    // LET + lambdas + helpers together
    assert_eq!(ev(&wb, "=LET(sq,LAMBDA(x,x^2),SUM(MAP(A1:A3,sq)))"), n(14.0));
}

#[test]
fn lambda_spills_and_files() {
    let wb = book(&[("A1", "=MAKEARRAY(2,2,LAMBDA(r,c,r+c))"), ("D1", "=LAMBDA(x,x)")]);
    assert_eq!(wb.sheets[0].display_value(1, 1), &n(4.0));
    assert_eq!(wb.sheets[0].value(0, 3), &err(ErrorKind::Calc));
    // parameters are written with the _xlpm. prefix, including calls of LET-bound lambdas
    let e = parse("LET(f,LAMBDA(x,y,x+y),f(1,2))").unwrap();
    assert_eq!(print(&e), "LET(f,LAMBDA(x,y,x+y),F(1,2))");
    assert_eq!(print_xlsx(&e), "_xlfn.LET(_xlpm.f,_xlfn.LAMBDA(_xlpm.x,_xlpm.y,_xlpm.x+_xlpm.y),_xlpm.F(1,2))");
    // and read back without it
    let back = parse("_xlfn.LET(_xlpm.f,_xlfn.LAMBDA(_xlpm.x,_xlpm.x*2),_xlpm.f(3))").unwrap();
    assert_eq!(print(&back), "LET(f,LAMBDA(x,x*2),F(3))");
    assert_eq!(print(&parse("LAMBDA(x,x+1)(2)").unwrap()), "LAMBDA(x,x+1)(2)");
}

#[test]
fn groupby_pivotby() {
    let wb = book(&[
        ("A1", "Region"),
        ("B1", "Sales"),
        ("C1", "Year"),
        ("A2", "East"),
        ("B2", "10"),
        ("C2", "2024"),
        ("A3", "West"),
        ("B3", "5"),
        ("C3", "2025"),
        ("A4", "East"),
        ("B4", "3"),
        ("C4", "2025"),
        ("A5", "North"),
        ("B5", "7"),
        ("C5", "2024"),
        ("A6", "West"),
        ("B6", "1"),
        ("C6", "2024"),
    ]);
    let blank = t("");
    assert_eq!(
        arr(&wb, "=GROUPBY(A2:A6,B2:B6,SUM)"),
        vec![vec![t("East"), n(13.0)], vec![t("North"), n(7.0)], vec![t("West"), n(6.0)], vec![t("Total"), n(26.0)]]
    );
    assert_eq!(arr(&wb, "=GROUPBY(A2:A6,B2:B6,SUM,0,-1)")[0], vec![t("Total"), n(26.0)]);
    // headers detected automatically, no totals, sorted by value descending
    assert_eq!(arr(&wb, "=GROUPBY(A1:A6,B1:B6,SUM,,0,-2)"), vec![vec![t("Region"), t("Sales")], vec![t("East"), n(13.0)], vec![t("North"), n(7.0)], vec![t("West"), n(6.0)]]);
    assert_eq!(arr(&wb, "=GROUPBY(A2:A6,B2:B6,LAMBDA(v,COUNT(v)),0,0)"), vec![vec![t("East"), n(2.0)], vec![t("North"), n(1.0)], vec![t("West"), n(2.0)]]);
    // filter_array
    assert_eq!(arr(&wb, "=GROUPBY(A2:A6,B2:B6,SUM,0,0,,C2:C6=2024)"), vec![vec![t("East"), n(10.0)], vec![t("North"), n(7.0)], vec![t("West"), n(1.0)]]);
    // two row fields with subtotals
    let g = arr(&wb, "=GROUPBY(HSTACK(A2:A6,C2:C6),B2:B6,SUM,0,2)");
    assert_eq!(g.len(), 9);
    assert_eq!(g[0], vec![t("East"), n(2024.0), n(10.0)]);
    assert_eq!(g[1], vec![t("East"), n(2025.0), n(3.0)]);
    assert_eq!(g[2], vec![t("East"), blank.clone(), n(13.0)]);
    assert_eq!(g[4], vec![t("North"), blank.clone(), n(7.0)]);
    assert_eq!(g[8], vec![t("Total"), blank.clone(), n(26.0)]);
    assert_eq!(
        arr(&wb, "=PIVOTBY(A2:A6,C2:C6,B2:B6,SUM)"),
        vec![
            vec![blank.clone(), n(2024.0), n(2025.0), t("Total")],
            vec![t("East"), n(10.0), n(3.0), n(13.0)],
            vec![t("North"), n(7.0), blank.clone(), n(7.0)],
            vec![t("West"), n(1.0), n(5.0), n(6.0)],
            vec![t("Total"), n(18.0), n(8.0), n(26.0)],
        ]
    );
    assert_eq!(arr(&wb, "=PIVOTBY(A2:A6,C2:C6,B2:B6,COUNTA,0,0,,0)").len(), 4);
}

// ---------------------------------------------------------------------------
// Reference operators, 3-D and external references
// ---------------------------------------------------------------------------

#[test]
fn reference_operators() {
    let mut cells = Vec::new();
    let texts: Vec<(String, String)> = (0..4).flat_map(|r| (0..4).map(move |c| (cell_name(r, c), ((r * 4 + c) + 1).to_string()))).collect();
    for (a, v) in &texts {
        cells.push((a.as_str(), v.as_str()));
    }
    let wb = book(&cells);
    // A1:D4 holds 1..16 row by row
    assert_eq!(ev(&wb, "=SUM((A1:A2,C1:C2))"), n(1.0 + 5.0 + 3.0 + 7.0));
    assert_eq!(ev(&wb, "=LARGE((A1:A2,C3:C4),1)"), n(15.0));
    assert_eq!(ev(&wb, "=COUNT((A1,B2,C3))"), n(3.0));
    assert_eq!(ev(&wb, "=AREAS((A1:B2,C3,D4))"), n(3.0));
    assert_eq!(ev(&wb, "=AREAS(A1:D4)"), n(1.0));
    assert_eq!(ev(&wb, "=INDEX((A1:B2,C3:D4),2,2,2)"), n(16.0));
    assert_eq!(ev(&wb, "=INDEX((A1:B2,C3:D4),1,1,3)"), err(ErrorKind::Ref));
    assert_eq!(ev(&wb, "=(A1:A2,C1:C2)"), err(ErrorKind::Value));
    // intersection
    assert_eq!(ev(&wb, "=SUM(A1:C3 B2:D4)"), n(6.0 + 7.0 + 10.0 + 11.0));
    assert_eq!(ev(&wb, "=B1:B4 A3:D3"), n(10.0));
    assert_eq!(ev(&wb, "=A1:A2 C1:C2"), err(ErrorKind::Null));
    // range operator between general references
    assert_eq!(ev(&wb, "=SUM(A1:INDEX(A1:A4,3))"), n(1.0 + 5.0 + 9.0));
    assert_eq!(ev(&wb, "=SUM(INDEX(A1:D4,2,2):INDEX(A1:D4,3,3))"), n(6.0 + 7.0 + 10.0 + 11.0));
    assert_eq!(ev(&wb, "=ROWS(B2:C2:A4)"), n(3.0));
    for f in ["SUM((A1:A3,C1:C3))", "SUM(A1:C3 B2:D4)", "SUM(A1:INDEX(A:A,5))", "A1:B2 B2:C3", "SUM(OFFSET(A1,1,1):D4)"] {
        assert_eq!(print(&parse(f).unwrap()), f);
    }
    // spaces elsewhere are still just spaces
    assert_eq!(print(&parse("SUM( A1 , B1 ) + 1").unwrap()), "SUM(A1,B1)+1");
    assert_eq!(print(&parse("IF(A1 > 1, \"a b\", B1 )").unwrap()), "IF(A1>1,\"a b\",B1)");
}

#[test]
fn three_d_references() {
    let mut wb = Workbook::empty_with(vec![Sheet::new("Jan"), Sheet::new("Feb"), Sheet::new("Mar"), Sheet::new("Total Sheet")]);
    for (s, v) in [(0, "1"), (1, "2"), (2, "4")] {
        put(&mut wb, s, "B2", v);
        put(&mut wb, s, "B3", "10");
    }
    put(&mut wb, 3, "A1", "=SUM(Jan:Mar!B2)");
    put(&mut wb, 3, "A2", "=SUM('Jan:Feb'!B2:B3)");
    put(&mut wb, 3, "A3", "=AVERAGE(Feb:Mar!B2)");
    put(&mut wb, 3, "A4", "=COUNT(Jan:Mar!B2:B3)");
    put(&mut wb, 3, "A5", "=Jan:Mar!B2");
    put(&mut wb, 3, "A6", "=SUM(Jan:Nope!B2)");
    recalc(&mut wb);
    let v = |r: u32| wb.sheets[3].value(r, 0).clone();
    assert_eq!(v(0), n(7.0));
    assert_eq!(v(1), n(23.0));
    assert_eq!(v(2), n(3.0));
    assert_eq!(v(3), n(6.0));
    assert_eq!(v(4), err(ErrorKind::Value));
    assert_eq!(v(5), err(ErrorKind::Ref));
    // incremental recalculation follows edits on any sheet of the span
    put(&mut wb, 1, "B2", "100");
    super::recalc_ranges(&mut wb, &[(1, Range::cell(1, 1))]);
    assert_eq!(wb.sheets[3].value(0, 0), &n(105.0));
    // printing and renames
    assert_eq!(print(&parse("SUM(Jan:Mar!B2)").unwrap()), "SUM(Jan:Mar!B2)");
    assert_eq!(print(&parse("SUM('Jan:Total Sheet'!B2)").unwrap()), "SUM('Jan:Total Sheet'!B2)");
    let mut e = parse("SUM(Jan:Mar!B2)+Mar!A1").unwrap();
    super::rename_sheet(&mut e, "Mar", "March 2026");
    assert_eq!(print(&e), "SUM('Jan:March 2026'!B2)+'March 2026'!A1");
    // a sheet-qualified range is not a 3-D reference
    assert_eq!(print(&parse("SUM(Jan!A1:Jan!B2)").unwrap()), "SUM(Jan!A1:Jan!B2)");
}

#[test]
fn external_references() {
    let wb = book(&[("A1", "1")]);
    for f in [
        "[Book.xlsx]Sheet1!A1",
        "SUM([Budget.xlsx]Data!B2:B9)",
        "SUM('[Budget 2026.xlsx]Data'!B2:B9)",
        "[1]Sheet1!$A$1+1",
        "'C:\\path\\[Book.xlsx]Sheet1'!A1",
        "'[My Book.xlsx]My Sheet'!A1*2",
    ] {
        let e = parse(f).unwrap_or_else(|m| panic!("{f}: {m}"));
        assert_eq!(print(&e), f);
        assert_eq!(print_xlsx(&e), f);
        assert_eq!(ev(&wb, &format!("={f}")), err(ErrorKind::Ref), "{f}");
    }
}

#[test]
fn unparsed_formulas_survive_save_and_load() {
    // Syntax the engine does not understand (structured references, external names) is kept
    // verbatim: not rewritten, not turned into text.
    let texts = ["Table1[Sales]*2", "SUM(Table1[[#This Row],[Qty]:[Price]])", "[Book.xlsx]!Name+1", "[@Qty]*2", "SUM([Budget 2026.xlsx]Data!B2:B9)"];
    let mut wb = Workbook::new();
    for (i, f) in texts.iter().enumerate() {
        let cell = cell_from_input(&mut wb, &format!("={f}"), 0);
        wb.sheets[0].set(i as u32, 0, cell);
        let cell = wb.sheets[0].get(i as u32, 0).unwrap();
        let fm = cell.formula.as_ref().expect("still a formula");
        assert!(fm.ast.is_none(), "{f} should not parse");
        assert_eq!(fm.text, *f);
    }
    recalc(&mut wb);
    let path = std::env::temp_dir().join(format!("freedom_formula_verbatim_{}.xlsx", std::process::id()));
    crate::io::save(&wb, &path).unwrap();
    let back = crate::io::load(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    for (i, f) in texts.iter().enumerate() {
        let cell = back.sheets[0].get(i as u32, 0).expect("cell kept");
        assert_eq!(cell.formula.as_ref().map(|x| x.text.as_str()), Some(*f));
    }
}

#[test]
fn lambdas_and_new_functions_round_trip_through_xlsx() {
    let mut wb = Workbook::new();
    wb.names.push(("DOUBLE".into(), "LAMBDA(x,x*2)".into()));
    for (a, f) in [
        ("A1", "1"),
        ("A2", "2"),
        ("B1", "=DOUBLE(4)"),
        ("C1", "=GROUPBY(A1:A2,A1:A2,SUM,0,0)"),
        ("E1", "=REGEXTEST(\"a1\",\"\\d\")"),
        ("F1", "=SUM((A1,A2))+SUM(A1:A2 A2)"),
    ] {
        put(&mut wb, 0, a, f);
    }
    recalc(&mut wb);
    let path = std::env::temp_dir().join(format!("freedom_formula_lambda_{}.xlsx", std::process::id()));
    crate::io::save(&wb, &path).unwrap();
    // the file holds Excel's prefixed forms
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut xml = String::new();
    for part in ["xl/workbook.xml", "xl/worksheets/sheet1.xml"] {
        std::io::Read::read_to_string(&mut zip.by_name(part).unwrap(), &mut xml).unwrap();
    }
    assert!(xml.contains("_xlfn.LAMBDA(_xlpm.x,_xlpm.x*2)"), "{xml}");
    assert!(xml.contains("_xlfn.GROUPBY(A1:A2,A1:A2,_xleta.SUM,0,0)"), "{xml}");
    assert!(xml.contains("_xlfn.REGEXTEST("), "{xml}");
    let mut back = crate::io::load(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    recalc(&mut back);
    let s = &back.sheets[0];
    assert_eq!(s.value(0, 1), &n(8.0));
    assert_eq!(s.get(0, 2).and_then(|c| c.formula.as_ref()).map(|f| f.text.as_str()), Some("GROUPBY(A1:A2,A1:A2,SUM,0,0)"));
    assert_eq!(s.display_value(1, 3), &n(2.0));
    assert_eq!(s.value(0, 4), &Value::Bool(true));
    assert_eq!(s.value(0, 5), &n(5.0));
}

// ---------------------------------------------------------------------------
// Lookup, information, database
// ---------------------------------------------------------------------------

#[test]
fn lookup_cell_info() {
    let wb = book(&[
        ("A1", "4.14"),
        ("A2", "4.19"),
        ("A3", "5.17"),
        ("A4", "5.77"),
        ("A5", "6.39"),
        ("B1", "red"),
        ("B2", "orange"),
        ("B3", "yellow"),
        ("B4", "green"),
        ("B5", "blue"),
        ("D1", "abc"),
    ]);
    // Excel documentation examples (vector form)
    assert_eq!(ev(&wb, "=LOOKUP(4.19,A1:A5,B1:B5)"), t("orange"));
    assert_eq!(ev(&wb, "=LOOKUP(5.75,A1:A5,B1:B5)"), t("yellow"));
    assert_eq!(ev(&wb, "=LOOKUP(7.66,A1:A5,B1:B5)"), t("blue"));
    assert_eq!(ev(&wb, "=LOOKUP(0,A1:A5,B1:B5)"), err(ErrorKind::NA));
    // array form
    assert_eq!(ev(&wb, "=LOOKUP(\"C\",{\"a\",\"b\",\"c\",\"d\";1,2,3,4})"), n(3.0));
    assert_eq!(ev(&wb, "=LOOKUP(\"bump\",{\"a\",1;\"b\",2;\"c\",3})"), n(2.0));
    assert_eq!(ev(&wb, "=LOOKUP(5.2,A1:B5)"), t("yellow"));
    assert_eq!(ev(&wb, "=CELL(\"address\",B3)"), t("$B$3"));
    assert_eq!(ev(&wb, "=CELL(\"row\",C7)+CELL(\"col\",C7)"), n(10.0));
    assert_eq!(ev(&wb, "=CELL(\"contents\",B2)"), t("orange"));
    assert_eq!(ev(&wb, "=CELL(\"type\",B2)&CELL(\"type\",A2)&CELL(\"type\",Z9)"), t("lvb"));
    assert_eq!(ev(&wb, "=CELL(\"format\",A1)"), t("G"));
    assert_eq!(arr(&wb, "=CELL(\"width\",A1)"), vec![vec![n(8.0), Value::Bool(true)]]);
    assert_eq!(ev(&wb, "=CELL(\"filename\",A1)"), t(""));
    assert_eq!(ev(&wb, "=CELL(\"bogus\",A1)"), err(ErrorKind::Value));
    assert_eq!(ev(&wb, "=INFO(\"numfile\")"), n(1.0));
    assert_eq!(ev(&wb, "=INFO(\"recalc\")"), t("Automatic"));
    assert_eq!(super::more::format_code("0.00"), "F2");
    assert_eq!(super::more::format_code("#,##0"), ",0");
    assert_eq!(super::more::format_code("$#,##0.00_);($#,##0.00)"), "C2()");
    assert_eq!(super::more::format_code("0%"), "P0");
    assert_eq!(super::more::format_code("0.00E+00"), "S2");
    assert_eq!(super::more::format_code("d-mmm-yy"), "D1");
    assert_eq!(super::more::format_code("m/d/yyyy"), "D4");
    assert_eq!(super::more::format_code("h:mm AM/PM"), "D7");
    assert_eq!(super::more::format_code("h:mm:ss"), "D8");
}

#[test]
fn frequency_trimmean() {
    let wb = book(&[]);
    assert_eq!(arr(&wb, "=FREQUENCY({79,85,78,85,50,81,95,88,97},{70,79,89})"), nums(&[&[1.0], &[2.0], &[4.0], &[2.0]]));
    assert_eq!(arr(&wb, "=FREQUENCY({1,2,3},{2,1})"), nums(&[&[1.0], &[1.0], &[1.0]]));
    close(ev(&wb, "=TRIMMEAN({4,5,6,7,2,3,4,5,1,2,3},0.2)"), 3.777_777_777_777_78, 1e-12);
    assert_eq!(ev(&wb, "=TRIMMEAN({1,2},1)"), err(ErrorKind::Num));
    assert_eq!(ev(&wb, "=PERCENTOF({1,3},{1,3,4,2})"), n(0.4));
}

#[test]
fn database_functions() {
    // Excel's documentation sample
    let rows = [
        ["Tree", "Height", "Age", "Yield", "Profit", "Height"],
        ["=\"=Apple\"", ">10", "", "", "", "<16"],
        ["=\"=Pear\"", "", "", "", "", ""],
        ["Tree", "Height", "Age", "Yield", "Profit", ""],
        ["Apple", "18", "20", "14", "105", ""],
        ["Pear", "12", "12", "10", "96", ""],
        ["Cherry", "13", "14", "9", "105", ""],
        ["Apple", "14", "15", "10", "75", ""],
        ["Pear", "9", "8", "8", "76.8", ""],
        ["Apple", "8", "9", "6", "45", ""],
    ];
    let mut cells = Vec::new();
    let names: Vec<String> = (0..10).flat_map(|r| (0..6).map(move |c| cell_name(r, c))).collect();
    for (i, a) in names.iter().enumerate() {
        let v = rows[i / 6][i % 6];
        if !v.is_empty() {
            cells.push((a.as_str(), v));
        }
    }
    let wb = book(&cells);
    assert_eq!(ev(&wb, "=DCOUNT(A4:E10,\"Age\",A1:F2)"), n(1.0));
    assert_eq!(ev(&wb, "=DCOUNTA(A4:E10,\"Profit\",A1:F2)"), n(1.0));
    assert_eq!(ev(&wb, "=DMAX(A4:E10,\"Profit\",A1:A3)"), n(105.0));
    assert_eq!(ev(&wb, "=DMIN(A4:E10,\"Profit\",A1:B2)"), n(75.0));
    assert_eq!(ev(&wb, "=DSUM(A4:E10,\"Profit\",A1:A2)"), n(225.0));
    assert_eq!(ev(&wb, "=DSUM(A4:E10,\"Profit\",A1:F2)"), n(75.0));
    assert_eq!(ev(&wb, "=DPRODUCT(A4:E10,\"Yield\",A1:F2)"), n(10.0));
    close(ev(&wb, "=DAVERAGE(A4:E10,\"Yield\",A1:B2)"), 12.0, 1e-12);
    close(ev(&wb, "=DAVERAGE(A4:E10,3,A4:E10)"), 13.0, 1e-12);
    close(ev(&wb, "=DSTDEV(A4:E10,\"Yield\",A1:A3)"), 2.966_479_394_838_26, 1e-12);
    close(ev(&wb, "=DSTDEVP(A4:E10,\"Yield\",A1:A3)"), 2.653_299_832_284_32, 1e-12);
    close(ev(&wb, "=DVAR(A4:E10,\"Yield\",A1:A3)"), 8.8, 1e-12);
    close(ev(&wb, "=DVARP(A4:E10,\"Yield\",A1:A3)"), 7.04, 1e-12);
    assert_eq!(ev(&wb, "=DGET(A4:E10,\"Yield\",A1:A3)"), err(ErrorKind::Num));
    assert_eq!(ev(&wb, "=DGET(A4:E10,\"Yield\",A1:F2)"), n(10.0));
    assert_eq!(ev(&wb, "=DSUM(A4:E10,\"Nope\",A1:A2)"), err(ErrorKind::Value));
    // text without an operator matches values that begin with it
    let wb2 = book(&[("A1", "Name"), ("A2", "Apple"), ("A3", "Apples"), ("A4", "Pineapple"), ("B1", "Name"), ("B2", "App")]);
    assert_eq!(ev(&wb2, "=DCOUNTA(A1:A4,1,B1:B2)"), n(2.0));
    assert_eq!(ev(&wb2, "=DCOUNTA(A1:A4,,B1:B2)"), n(2.0));
}

// ---------------------------------------------------------------------------
// Statistics and distributions (Excel documentation examples)
// ---------------------------------------------------------------------------

#[test]
fn distributions() {
    let wb = book(&[]);
    let c = |f: &str, want: f64, tol: f64| close(ev(&wb, f), want, tol);
    c("=BINOM.DIST(6,10,0.5,FALSE)", 0.205_078_125, 1e-12);
    c("=BINOM.DIST(6,10,0.5,TRUE)", 0.828_125, 1e-12);
    c("=BINOMDIST(6,10,0.5,FALSE)", 0.205_078_125, 1e-12);
    assert_eq!(ev(&wb, "=BINOM.INV(6,0.5,0.75)"), n(4.0));
    assert_eq!(ev(&wb, "=CRITBINOM(6,0.5,0.75)"), n(4.0));
    c("=BINOM.DIST.RANGE(60,0.75,48)", 0.083_974_967, 1e-7);
    c("=BINOM.DIST.RANGE(60,0.75,45,50)", 0.523_629_793, 1e-7);
    c("=POISSON.DIST(2,5,TRUE)", 0.124_652_019_483_081, 1e-12);
    c("=POISSON.DIST(2,5,FALSE)", 0.084_224_337_488_568, 1e-12);
    c("=EXPON.DIST(0.2,10,TRUE)", 0.864_664_716_763_387, 1e-12);
    c("=EXPON.DIST(0.2,10,FALSE)", 1.353_352_832_366_13, 1e-12);
    c("=GAMMA.DIST(10.00001131,9,2,FALSE)", 0.032_639, 1e-4);
    c("=GAMMA.DIST(10.00001131,9,2,TRUE)", 0.068_094, 1e-4);
    c("=GAMMA.INV(0.068094,9,2)", 10.000_011_2, 1e-6);
    c("=GAMMA(2.5)", 1.329_340_388_179_14, 1e-12);
    c("=GAMMA(-3.75)", 0.267_866_128_861_417, 1e-10);
    assert_eq!(ev(&wb, "=GAMMA(0)"), err(ErrorKind::Num));
    c("=GAMMALN(4)", 1.791_759_469_228_06, 1e-12);
    c("=BETA.DIST(2,8,10,TRUE,1,3)", 0.685_470_581_054_688, 1e-10);
    c("=BETA.DIST(2,8,10,FALSE,1,3)", 1.483_764_648_437_5, 1e-10);
    c("=BETADIST(2,8,10,1,3)", 0.685_470_581_054_688, 1e-10);
    c("=BETA.INV(0.685470581,8,10,1,3)", 2.0, 1e-8);
    c("=CHISQ.DIST(0.5,1,TRUE)", 0.520_499_877_813_047, 1e-10);
    c("=CHISQ.DIST(2,3,FALSE)", 0.207_553_748_710_297, 1e-10);
    c("=CHISQ.DIST.RT(18.307,10)", 0.050_000_589_8, 1e-6);
    c("=CHIDIST(18.307,10)", 0.050_000_589_8, 1e-6);
    c("=CHISQ.INV(0.93,1)", 3.283_020_286_759_54, 1e-9);
    c("=CHISQ.INV(0.6,2)", 1.832_581_463_748_31, 1e-9);
    c("=CHISQ.INV.RT(0.050001,10)", 18.306_973_464_394_3, 1e-8);
    c("=F.DIST(15.2069,6,4,TRUE)", 0.990_000_043_2, 1e-7);
    c("=F.DIST(15.2069,6,4,FALSE)", 0.001_223_791_709_2, 1e-6);
    c("=F.DIST.RT(15.2068649,6,4)", 0.01, 1e-6);
    c("=F.INV(0.01,6,4)", 0.109_309_914_1, 1e-7);
    c("=F.INV.RT(0.01,6,4)", 15.206_864_870_947_7, 1e-8);
    c("=LOGNORM.DIST(4,3.5,1.2,TRUE)", 0.039_083_556_3, 1e-7);
    c("=LOGNORM.DIST(4,3.5,1.2,FALSE)", 0.017_617_597_0, 1e-7);
    c("=LOGNORM.INV(0.039084,3.5,1.2)", 4.000_025_191_6, 1e-8);
    c("=WEIBULL.DIST(105,20,100,TRUE)", 0.929_581_390_3, 1e-8);
    c("=WEIBULL.DIST(105,20,100,FALSE)", 0.035_588_864_3, 1e-8);
    c("=HYPGEOM.DIST(1,4,8,20,TRUE)", 0.465_428_276_573_788, 1e-10);
    c("=HYPGEOM.DIST(1,4,8,20,FALSE)", 0.363_261_093_911_249, 1e-10);
    c("=NEGBINOM.DIST(10,5,0.25,TRUE)", 0.313_514_1, 2e-7);
    c("=NEGBINOM.DIST(10,5,0.25,FALSE)", 0.055_048_660_6, 1e-8);
    c("=FISHER(0.75)", 0.972_955_074_527_657, 1e-12);
    c("=FISHERINV(0.972955)", 0.749_999_967_9, 1e-8);
    c("=PHI(0.75)", 0.301_137_432_154_804, 1e-12);
    c("=GAUSS(2)", 0.477_249_868_051_821, 1e-12);
    c("=ERF(0.745)", 0.707_928_92, 1e-8);
    c("=ERF(0,1)", 0.842_700_792_949_715, 1e-12);
    c("=ERFC(1)", 0.157_299_207_050_285, 1e-10);
    // array lifting of the new scalar functions
    assert_eq!(arr(&wb, "=ROUND(EXPON.DIST({0,1},1,TRUE),4)"), nums(&[&[0.0, 0.6321]]));
}

#[test]
fn statistical_tests() {
    let wb = book(&[
        ("A1", "3"),
        ("A2", "4"),
        ("A3", "5"),
        ("A4", "8"),
        ("A5", "9"),
        ("A6", "1"),
        ("A7", "2"),
        ("A8", "4"),
        ("A9", "5"),
        ("B1", "6"),
        ("B2", "19"),
        ("B3", "3"),
        ("B4", "2"),
        ("B5", "14"),
        ("B6", "4"),
        ("B7", "5"),
        ("B8", "17"),
        ("B9", "1"),
    ]);
    let c = |f: &str, want: f64, tol: f64| close(ev(&wb, f), want, tol);
    c("=T.TEST(A1:A9,B1:B9,2,1)", 0.196_015_785_3, 1e-8);
    c("=TTEST(A1:A9,B1:B9,2,1)", 0.196_015_785_3, 1e-8);
    // t = 1.3622 with 16 (pooled) and 10.255 (Welch) degrees of freedom
    c("=T.TEST(A1:A9,B1:B9,2,2)", 0.192, 5e-3);
    c("=T.TEST(A1:A9,B1:B9,1,3)", 0.101, 5e-3);
    c("=F.TEST({6,7,9,15,21},{20,28,31,38,40})", 0.648_317_846_786_174, 1e-9);
    c("=Z.TEST({3,6,7,8,6,5,4,2,1,9},4)", 0.090_574_196_7, 1e-8);
    c("=Z.TEST({3,6,7,8,6,5,4,2,1,9},6)", 0.863_043_389_7, 1e-8);
    c("=CHISQ.TEST({58,35;11,25;10,23},{45.35,47.65;17.56,18.44;16.09,16.91})", 0.000_308_192_5, 1e-5);
    c("=PROB({0,1,2,3},{0.2,0.3,0.1,0.4},2)", 0.1, 1e-12);
    c("=PROB({0,1,2,3},{0.2,0.3,0.1,0.4},1,3)", 0.8, 1e-12);
    c("=STDEVA(1,TRUE,\"3\")", 2.0 / 3f64.sqrt(), 1e-12);
    c("=VARPA({1,2,3})", 2.0 / 3.0, 1e-12);
}

// ---------------------------------------------------------------------------
// Math, engineering, matrices, text
// ---------------------------------------------------------------------------

#[test]
fn math_and_engineering() {
    let wb = book(&[]);
    assert_eq!(arr(&wb, "=MMULT({1,3;7,2},{2,0;0,2})"), nums(&[&[2.0, 6.0], &[14.0, 4.0]]));
    assert_eq!(ev(&wb, "=MMULT({1,2},{1,2})"), err(ErrorKind::Value));
    close(ev(&wb, "=MDETERM({1,3,8,5;1,3,6,1;1,1,1,0;7,3,10,2})"), 88.0, 1e-12);
    assert_eq!(arr(&wb, "=MINVERSE({4,-1;2,0})"), nums(&[&[0.0, 0.5], &[-1.0, 2.0]]));
    assert_eq!(ev(&wb, "=MINVERSE({1,2;2,4})"), err(ErrorKind::Num));
    assert_eq!(arr(&wb, "=MUNIT(2)"), nums(&[&[1.0, 0.0], &[0.0, 1.0]]));
    close(ev(&wb, "=SINH(1)+COSH(1)"), std::f64::consts::E, 1e-12);
    assert_eq!(ev(&wb, "=COT(0)"), err(ErrorKind::Div0));
    assert_eq!(ev(&wb, "=ACOSH(0.5)"), err(ErrorKind::Num));
    close(ev(&wb, "=ACOT(2)"), 0.463_647_609, 1e-8);
    assert_eq!(ev(&wb, "=FACTDOUBLE(7)"), n(105.0));
    assert_eq!(ev(&wb, "=FACTDOUBLE(-1)"), n(1.0));
    assert_eq!(ev(&wb, "=CEILING.PRECISE(-4.3,2)"), n(-4.0));
    assert_eq!(ev(&wb, "=ISO.CEILING(4.3)"), n(5.0));
    assert_eq!(ev(&wb, "=FLOOR.PRECISE(-3.2,-1)"), n(-4.0));
    assert_eq!(ev(&wb, "=PERMUTATIONA(3,2)"), n(9.0));
    assert_eq!(ev(&wb, "=BITAND(1,5)+BITOR(23,10)*100"), n(3101.0));
    assert_eq!(ev(&wb, "=BITXOR(5,3)"), n(6.0));
    assert_eq!(ev(&wb, "=BITLSHIFT(4,2)"), n(16.0));
    assert_eq!(ev(&wb, "=BITRSHIFT(13,2)"), n(3.0));
    assert_eq!(ev(&wb, "=BITAND(-1,1)"), err(ErrorKind::Num));
    assert_eq!(ev(&wb, "=DELTA(5,4)+GESTEP(5,4)*10"), n(10.0));
    assert_eq!(ev(&wb, "=BIN2HEX(11111011,4)"), t("00FB"));
    assert_eq!(ev(&wb, "=HEX2OCT(\"F\",3)"), t("017"));
    assert_eq!(ev(&wb, "=OCT2BIN(3,3)"), t("011"));
    close(ev(&wb, "=ISPMT(0.1/12,1,36,8000000)"), -64_814.814_814_814_8, 1e-12);
    close(ev(&wb, "=DOLLARDE(1.02,16)"), 1.125, 1e-12);
    close(ev(&wb, "=DOLLARFR(1.125,16)"), 1.02, 1e-12);
    close(ev(&wb, "=SERIESSUM(2,0,1,{1,1,1})"), 7.0, 1e-12);
    close(ev(&wb, "=SQRTPI(1)"), std::f64::consts::PI.sqrt(), 1e-12);
}

#[test]
fn text_and_regex() {
    let wb = book(&[("A1", "abc123def45")]);
    assert_eq!(ev(&wb, "=ARRAYTOTEXT({1,\"a\";TRUE,2})"), t("1, a, TRUE, 2"));
    assert_eq!(ev(&wb, "=ARRAYTOTEXT({1,\"a\";TRUE,2},1)"), t("{1,\"a\";TRUE,2}"));
    assert_eq!(ev(&wb, "=VALUETOTEXT(\"abc\",1)"), t("\"abc\""));
    assert_eq!(ev(&wb, "=VALUETOTEXT(12.5)"), t("12.5"));
    assert_eq!(ev(&wb, "=VALUETOTEXT(NA())"), t("#N/A"));
    assert_eq!(ev(&wb, "=ENCODEURL(\"a b&c\")"), t("a%20b%26c"));
    assert_eq!(ev(&wb, "=LENB(\"abc\")&LEFTB(\"abc\",2)"), t("3ab"));
    assert_eq!(ev(&wb, "=REGEXTEST(A1,\"\\d+\")"), Value::Bool(true));
    assert_eq!(ev(&wb, "=REGEXTEST(A1,\"^ABC\")"), Value::Bool(false));
    assert_eq!(ev(&wb, "=REGEXTEST(A1,\"^ABC\",1)"), Value::Bool(true));
    assert_eq!(ev(&wb, "=REGEXEXTRACT(A1,\"[0-9]+\")"), t("123"));
    assert_eq!(arr(&wb, "=REGEXEXTRACT(A1,\"[0-9]+\",1)"), vec![vec![t("123")], vec![t("45")]]);
    assert_eq!(arr(&wb, "=REGEXEXTRACT(A1,\"([a-z]+)(\\d+)\",2)"), vec![vec![t("abc"), t("123")]]);
    assert_eq!(ev(&wb, "=REGEXEXTRACT(A1,\"x\")"), err(ErrorKind::NA));
    assert_eq!(ev(&wb, "=REGEXREPLACE(A1,\"\\d+\",\"#\")"), t("abc#def#"));
    assert_eq!(ev(&wb, "=REGEXREPLACE(A1,\"\\d+\",\"#\",2)"), t("abc123def#"));
    assert_eq!(ev(&wb, "=REGEXREPLACE(A1,\"\\d+\",\"#\",-2)"), t("abc#def45"));
    assert_eq!(ev(&wb, "=REGEXREPLACE(\"John Smith\",\"(\\w+) (\\w+)\",\"$2, $1\")"), t("Smith, John"));
    assert_eq!(ev(&wb, "=REGEXTEST(A1,\"(\")"), err(ErrorKind::Value));
    assert_eq!(arr(&wb, "=REGEXTEST({\"a1\",\"b\"},\"\\d\")"), vec![vec![Value::Bool(true), Value::Bool(false)]]);
}

#[test]
fn function_tables_are_sorted() {
    let names: Vec<&str> = super::functions::FUNCS.iter().map(|f| f.0).collect();
    assert!(names.windows(2).all(|w| w[0] < w[1]), "FUNCS must stay sorted (binary search)");
    for f in ["LAMBDA", "MAP", "LOOKUP", "DSUM", "REGEXTEST", "GROUPBY", "STDEV", "VAR", "PERCENTILE", "QUARTILE", "RANK", "NORMDIST", "CONCATENATE"] {
        assert!(super::functions::is_builtin(f), "{f}");
    }
    super::functions::check_lift_tables();
}
