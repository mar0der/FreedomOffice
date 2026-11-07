//! Layouts and calculations checked against what Excel shows for the same data.

use super::engine::{self, layout};
use super::*;
use crate::model::{Cell, Style, Value, Workbook};

pub(crate) use super::sample::sample;

pub(crate) fn new_pivot(wb: &Workbook) -> PivotTable {
    let n = if wb.sheets[0].get(7, 1).is_some() { 8 } else { 7 };
    let src = format!("Sheet1!$A$1:$E${n}");
    let cache = build_cache(wb, &src, 0).unwrap();
    PivotTable::new("PivotTable1", &src, cache, (2, 0))
}

/// The report as text rows like the Excel recordings: cells joined by " | ", indent as ">n".
fn dump(pt: &PivotTable) -> Vec<String> {
    let o = layout(pt);
    let mut out = Vec::new();
    for r in o.full.r1..=o.full.r2 {
        let mut line = Vec::new();
        for c in o.full.c1..=o.full.c2 {
            let cell = o.cells.iter().find(|x| x.r == r && x.c == c);
            let mut t = cell.map(|x| crate::numfmt::format_value(&x.value, &x.num_fmt).text).unwrap_or_default();
            if let Some(x) = cell {
                if x.indent > 0 {
                    t = format!(">{}{}", x.indent, t);
                }
            }
            line.push(t);
        }
        out.push(line.join(" | "));
    }
    out
}

fn f(pt: &PivotTable, name: &str) -> usize {
    pt.field_index(name).unwrap()
}

#[test]
fn compact_rows_and_columns_match_excel() {
    let wb = sample(false);
    let mut pt = new_pivot(&wb);
    let (region, product, sales, qty) = (f(&pt, "Region"), f(&pt, "Product"), f(&pt, "Sales"), f(&pt, "Qty"));
    pt.add_field(region, Area::Rows, None);
    pt.add_field(sales, Area::Values, None);
    assert_eq!(pt.values[0].name, "Sum of Sales");
    assert_eq!(dump(&pt), ["Row Labels | Sum of Sales", "East | 370", "North | 80", "West | 250", "Grand Total | 700"]);
    let o = layout(&pt);
    assert_eq!(o.range, Range::new(2, 0, 6, 1));

    pt.add_field(product, Area::Rows, None);
    assert_eq!(
        dump(&pt),
        [
            "Row Labels | Sum of Sales",
            "East | 370",
            ">1Apples | 220",
            ">1Pears | 150",
            "North | 80",
            ">1Pears | 80",
            "West | 250",
            ">1Apples | 200",
            ">1Pears | 50",
            "Grand Total | 700"
        ]
    );

    pt.add_field(qty, Area::Values, None);
    assert_eq!(pt.cols, vec![VALUES]);
    assert_eq!(dump(&pt)[0], "Row Labels | Sum of Sales | Sum of Qty");
    assert_eq!(dump(&pt)[1], "East | 370 | 12");
    assert_eq!(dump(&pt).last().unwrap(), "Grand Total | 700 | 22");

    // Product to the columns, before Σ Values -> Excel puts Σ Values first (it was there first).
    pt.add_field(product, Area::Columns, None);
    assert_eq!(pt.cols, vec![VALUES, product]);
    assert_eq!(
        dump(&pt),
        [
            " | Column Labels |  |  |  |  | ",
            " | Sum of Sales |  | Sum of Qty |  | Total Sum of Sales | Total Sum of Qty",
            "Row Labels | Apples | Pears | Apples | Pears |  | ",
            "East | 220 | 150 | 7 | 5 | 370 | 12",
            "North |  | 80 |  | 2 | 80 | 2",
            "West | 200 | 50 | 7 | 1 | 250 | 8",
            "Grand Total | 420 | 280 | 14 | 8 | 700 | 22"
        ]
    );
    pt.form = Form::Outline;
    assert_eq!(dump(&pt)[0], " | Values | Product |  |  |  | ");
    assert_eq!(dump(&pt)[2], "Region | Apples | Pears | Apples | Pears |  | ");
}

#[test]
fn outline_and_tabular_forms_match_excel() {
    let wb = sample(false);
    let mut pt = new_pivot(&wb);
    let (region, product, sales, qty) = (f(&pt, "Region"), f(&pt, "Product"), f(&pt, "Sales"), f(&pt, "Qty"));
    pt.add_field(region, Area::Rows, None);
    pt.add_field(product, Area::Rows, None);
    pt.add_field(sales, Area::Values, None);
    pt.add_field(qty, Area::Values, None);
    pt.form = Form::Outline;
    assert_eq!(
        dump(&pt),
        [
            "Region | Product | Sum of Sales | Sum of Qty",
            "East |  | 370 | 12",
            " | Apples | 220 | 7",
            " | Pears | 150 | 5",
            "North |  | 80 | 2",
            " | Pears | 80 | 2",
            "West |  | 250 | 8",
            " | Apples | 200 | 7",
            " | Pears | 50 | 1",
            "Grand Total |  | 700 | 22"
        ]
    );
    pt.form = Form::Tabular;
    assert_eq!(
        dump(&pt),
        [
            "Region | Product | Sum of Sales | Sum of Qty",
            "East | Apples | 220 | 7",
            " | Pears | 150 | 5",
            "East Total |  | 370 | 12",
            "North | Pears | 80 | 2",
            "North Total |  | 80 | 2",
            "West | Apples | 200 | 7",
            " | Pears | 50 | 1",
            "West Total |  | 250 | 8",
            "Grand Total |  | 700 | 22"
        ]
    );
    pt.repeat_labels = true;
    assert_eq!(dump(&pt)[2], "East | Pears | 150 | 5");
}

#[test]
fn values_on_rows_match_excel() {
    let wb = sample(false);
    let mut pt = new_pivot(&wb);
    let (region, product, sales, qty) = (f(&pt, "Region"), f(&pt, "Product"), f(&pt, "Sales"), f(&pt, "Qty"));
    pt.add_field(region, Area::Rows, None);
    pt.add_field(product, Area::Rows, None);
    pt.add_field(sales, Area::Values, None);
    pt.add_field(qty, Area::Values, None);
    pt.add_field(VALUES, Area::Rows, None);
    let d = dump(&pt);
    assert_eq!(&d[..9], ["Row Labels | ", "East | ", ">1Apples | ", ">2Sum of Sales | 220", ">2Sum of Qty | 7", ">1Pears | ", ">2Sum of Sales | 150", ">2Sum of Qty | 5", "East Sum of Sales | 370"]);
    assert_eq!(&d[d.len() - 2..], ["Total Sum of Sales | 700", "Total Sum of Qty | 22"]);
    assert_eq!(d.len(), 27);
}

#[test]
fn filters_blanks_collapse_and_subtotals_match_excel() {
    let wb = sample(true);
    let mut pt = new_pivot(&wb);
    let (region, product, sales, qty) = (f(&pt, "Region"), f(&pt, "Product"), f(&pt, "Sales"), f(&pt, "Qty"));
    pt.add_field(sales, Area::Values, None);
    assert_eq!(dump(&pt), ["Sum of Sales", "710"]);
    pt.add_field(product, Area::Columns, None);
    assert_eq!(dump(&pt), [" | Column Labels |  | ", " | Apples | Pears | Grand Total", "Sum of Sales | 420 | 290 | 710"]);
    pt.add_field(region, Area::Rows, None);
    pt.add_field(product, Area::Rows, None);
    let d = dump(&pt);
    assert_eq!(d[9], "(blank) | 10");
    assert_eq!(d[10], ">1Pears | 10");
    pt.fields[region].collapsed.insert("east".into());
    assert_eq!(dump(&pt)[1..3], ["East | 370", "North | 80"]);
    pt.fields[region].collapsed.clear();
    pt.subtotals_top = false;
    assert_eq!(dump(&pt)[1..5], ["East | ", ">1Apples | 220", ">1Pears | 150", "East Total | 370"]);
    pt.subtotals_top = true;
    pt.fields[region].subtotal = false;
    assert_eq!(dump(&pt)[1], "East | ");
    pt.fields[region].subtotal = true;
    pt.col_grand = false;
    assert!(!dump(&pt).iter().any(|l| l.starts_with("Grand Total")));
    pt.col_grand = true;
    pt.blank_rows = true;
    assert_eq!(dump(&pt)[4], " | ");
    pt.blank_rows = false;

    // Report filters sit above the table with a blank row between.
    pt.add_field(qty, Area::Filters, None);
    let o = layout(&pt);
    assert_eq!(o.full.r1, 0);
    assert_eq!(o.range.r1, 2);
    assert_eq!(dump(&pt)[0..2], ["Qty | (All)", " | "]);
    pt.add_field(region, Area::Filters, Some(0));
    let o = layout(&pt);
    assert_eq!((o.full.r1, o.range.r1), (0, 3));
    assert_eq!(dump(&pt)[0..5], ["Region | (All)", "Qty | (All)", " | ", "Row Labels | Sum of Sales", "Apples | 420"]);
    pt.fields[region].page = Some("west".into());
    assert_eq!(dump(&pt)[0], "Region | West");
    assert_eq!(dump(&pt).last().unwrap(), "Grand Total | 250");
    pt.fields[region].page = None;
    pt.fields[region].hidden.insert("west".into());
    assert_eq!(dump(&pt)[0], "Region | (Multiple Items)");
    assert_eq!(dump(&pt).last().unwrap(), "Grand Total | 460");
}

#[test]
fn show_values_as_matches_excel() {
    let wb = sample(true);
    let mut pt = new_pivot(&wb);
    let (region, product, sales) = (f(&pt, "Region"), f(&pt, "Product"), f(&pt, "Sales"));
    pt.add_field(region, Area::Rows, None);
    pt.add_field(product, Area::Columns, None);
    pt.add_field(sales, Area::Values, None);
    let body = |pt: &PivotTable| dump(pt)[2..].to_vec();
    pt.values[0].show_as = ShowAs::PctGrand;
    assert_eq!(
        body(&pt),
        [
            "East | 30.99% | 21.13% | 52.11%",
            "North | 0.00% | 11.27% | 11.27%",
            "West | 28.17% | 7.04% | 35.21%",
            "(blank) | 0.00% | 1.41% | 1.41%",
            "Grand Total | 59.15% | 40.85% | 100.00%"
        ]
    );
    pt.values[0].show_as = ShowAs::PctCol;
    assert_eq!(body(&pt)[0], "East | 52.38% | 51.72% | 52.11%");
    pt.values[0].show_as = ShowAs::PctRow;
    assert_eq!(body(&pt)[2], "West | 80.00% | 20.00% | 100.00%");
    pt.values[0].show_as = ShowAs::Difference;
    pt.values[0].base_field = region;
    pt.values[0].base_item = BaseItem::Item("East".into());
    assert_eq!(body(&pt), ["East |  |  | ", "North | -220 | -70 | -290", "West | -20 | -100 | -120", "(blank) | -220 | -140 | -360", "Grand Total |  |  | "]);
    pt.values[0].base_item = BaseItem::Prev;
    assert_eq!(body(&pt), ["East |  |  | ", "North | -220 | -70 | -290", "West | 200 | -30 | 170", "(blank) | -200 | -40 | -240", "Grand Total |  |  | "]);
    pt.values[0].show_as = ShowAs::RunningTotal;
    assert_eq!(body(&pt), ["East | 220 | 150 | 370", "North | 220 | 230 | 450", "West | 420 | 280 | 700", "(blank) | 420 | 290 | 710", "Grand Total |  |  | "]);
    pt.values[0].show_as = ShowAs::RankAsc;
    assert_eq!(body(&pt), ["East | 2 | 4 | 4", "North |  | 3 | 2", "West | 1 | 2 | 3", "(blank) |  | 1 | 1", "Grand Total |  |  | "]);
    pt.values[0].show_as = ShowAs::RankDesc;
    assert_eq!(body(&pt), ["East | 1 | 1 | 1", "North |  | 2 | 3", "West | 2 | 3 | 2", "(blank) |  | 4 | 4", "Grand Total |  |  | "]);
    pt.values[0].show_as = ShowAs::Normal;
    pt.values[0].agg = Agg::Count;
    assert_eq!(body(&pt), ["East | 2 | 1 | 3", "North |  | 1 | 1", "West | 1 | 1 | 2", "(blank) |  | 1 | 1", "Grand Total | 3 | 4 | 7"]);
}

#[test]
fn aggregation_functions() {
    let wb = sample(false);
    let mut pt = new_pivot(&wb);
    let (region, sales) = (f(&pt, "Region"), f(&pt, "Sales"));
    pt.add_field(region, Area::Rows, None);
    pt.add_field(sales, Area::Values, None);
    let east = |pt: &PivotTable| dump(pt)[1].clone();
    for (agg, want) in [
        (Agg::Sum, "East | 370"),
        (Agg::Count, "East | 3"),
        (Agg::Average, "East | 123.3333333"),
        (Agg::Max, "East | 150"),
        (Agg::Min, "East | 100"),
        (Agg::Product, "East | 1800000"),
        (Agg::CountNums, "East | 3"),
        (Agg::StdDev, "East | 25.16611478"),
        (Agg::StdDevP, "East | 20.54804668"),
        (Agg::Var, "East | 633.3333333"),
        (Agg::VarP, "East | 422.2222222"),
    ] {
        pt.values[0].agg = agg;
        assert_eq!(east(&pt), want, "{agg:?}");
    }
    pt.values[0].agg = Agg::StdDev;
    assert_eq!(dump(&pt)[2], "North | #DIV/0!");
}

#[test]
fn sorting_and_item_filters() {
    let wb = sample(false);
    let mut pt = new_pivot(&wb);
    let (region, sales) = (f(&pt, "Region"), f(&pt, "Sales"));
    pt.add_field(region, Area::Rows, None);
    pt.add_field(sales, Area::Values, None);
    pt.fields[region].sort = SortOrder::Desc;
    assert_eq!(dump(&pt)[1..4], ["West | 250", "North | 80", "East | 370"]);
    pt.fields[region].sort_by = Some(0);
    assert_eq!(dump(&pt)[1..4], ["East | 370", "West | 250", "North | 80"]);
    pt.fields[region].sort = SortOrder::Asc;
    assert_eq!(dump(&pt)[1..4], ["North | 80", "West | 250", "East | 370"]);
    pt.fields[region].sort_by = None;
    pt.fields[region].sort = SortOrder::Source;
    assert_eq!(dump(&pt)[1..4], ["East | 370", "West | 250", "North | 80"]);
    pt.fields[region].hidden.insert("west".into());
    assert_eq!(dump(&pt)[1..], ["East | 370", "North | 80", "Grand Total | 450"]);
}

#[test]
fn date_and_number_grouping_match_excel() {
    let wb = sample(true);
    let mut pt = new_pivot(&wb);
    let (date, sales) = (f(&pt, "Date"), f(&pt, "Sales"));
    pt.add_field(date, Area::Rows, None);
    pt.add_field(sales, Area::Values, None);
    assert_eq!(dump(&pt)[1], "1/15/2023 | 100");
    pt.group_dates(date, &[DatePart::Months, DatePart::Quarters, DatePart::Years]);
    assert_eq!(pt.rows.len(), 3);
    assert_eq!(pt.fields[pt.rows[0]].name, "Years (Date)");
    assert_eq!(
        dump(&pt),
        [
            "Row Labels | Sum of Sales",
            "2023 | 340",
            ">1Qtr1 | 100",
            ">2Jan | 100",
            ">1Qtr2 | 150",
            ">2Apr | 150",
            ">1Qtr4 | 90",
            ">2Nov | 80",
            ">2Dec | 10",
            "2024 | 370",
            ">1Qtr1 | 320",
            ">2Feb | 200",
            ">2Mar | 120",
            ">1Qtr3 | 50",
            ">2Jul | 50",
            "Grand Total | 710"
        ]
    );
    pt.ungroup(pt.rows[0]);
    assert_eq!(pt.rows, vec![date]);
    assert_eq!(pt.fields.len(), 5);

    let mut pt = new_pivot(&wb);
    pt.add_field(sales, Area::Rows, None);
    pt.add_field(sales, Area::Values, None);
    pt.fields[sales].num_group = Some((0.0, 250.0, 100.0));
    assert_eq!(dump(&pt), ["Row Labels | Sum of Sales", "0-99 | 140", "100-199 | 370", "200-299 | 200", "Grand Total | 710"]);
}

#[test]
fn update_writes_cells_and_getpivotdata_finds_values() {
    let mut wb = sample(false);
    let mut pt = new_pivot(&wb);
    let (region, product, sales) = (f(&pt, "Region"), f(&pt, "Product"), f(&pt, "Sales"));
    pt.add_field(region, Area::Rows, None);
    pt.add_field(product, Area::Columns, None);
    pt.add_field(sales, Area::Values, None);
    pt.anchor = (2, 7);
    wb.sheets[0].pivots.push(pt);
    engine::update(&mut wb, 0, 0).unwrap();
    let sh = &wb.sheets[0];
    assert_eq!(sh.value(2, 7), &Value::Text("Sum of Sales".into()));
    assert_eq!(sh.value(4, 8), &Value::Number(220.0));
    let pairs = [("Region".to_string(), Value::Text("east".into())), ("Product".to_string(), Value::Text("Apples".into()))];
    assert_eq!(engine::get_pivot_data(&wb, 0, 2, 7, "Sales", &pairs), Value::Number(220.0));
    assert_eq!(engine::get_pivot_data(&wb, 0, 2, 7, "Sum of Sales", &pairs[..1]), Value::Number(370.0));
    assert_eq!(engine::get_pivot_data(&wb, 0, 2, 7, "Sum of Sales", &[]), Value::Number(700.0));
    assert_eq!(engine::get_pivot_data(&wb, 0, 2, 7, "Sales", &[("Region".into(), Value::Text("South".into()))]), Value::Error(crate::model::ErrorKind::Ref));
    let pt = &wb.sheets[0].pivots[0];
    let li = pt.out.row_at[0];
    let cj = pt.out.col_at[0];
    assert_eq!(engine::getpivotdata_formula(pt, li, cj, "$H$3").unwrap(), "GETPIVOTDATA(\"Sales\",$H$3,\"Region\",\"East\",\"Product\",\"Apples\")");
    assert_eq!(engine::detail_records(pt, li, cj), vec![0, 4]);

    // A smaller report clears what the larger one left behind.
    wb.sheets[0].pivots[0].cols.clear();
    engine::update(&mut wb, 0, 0).unwrap();
    assert_eq!(wb.sheets[0].value(4, 9), &Value::Empty);
    assert_eq!(wb.sheets[0].value(3, 8), &Value::Number(370.0));
}

#[test]
fn styles_parse_and_look() {
    assert_eq!(style::parts("PivotStyleLight16"), Some((2, 1)));
    assert_eq!(style::parts("PivotStyleMedium9"), Some((4, 1)));
    assert_eq!(style::parts("PivotStyleDark1"), Some((5, 0)));
    assert_eq!(style::name(2, 1), "PivotStyleLight16");
    let mut wb = sample(false);
    let mut pt = new_pivot(&wb);
    let (region, product, sales) = (f(&pt, "Region"), f(&pt, "Product"), f(&pt, "Sales"));
    pt.add_field(region, Area::Rows, None);
    pt.add_field(product, Area::Rows, None);
    pt.add_field(sales, Area::Values, None);
    wb.sheets[0].pivots.push(pt);
    engine::update(&mut wb, 0, 0).unwrap();
    let pt = &wb.sheets[0].pivots[0];
    let head = style::look(pt, 2, 0).unwrap();
    assert_eq!(head.fill, Some(0xD9E1F2));
    assert!(head.bold);
    let east = style::look(pt, 3, 1).unwrap();
    assert!(east.bold && east.fill.is_none());
    let apples = style::look(pt, 4, 0).unwrap();
    assert!(!apples.bold);
    let grand = style::look(pt, pt.out.range.r2, 1).unwrap();
    assert!(grand.bold && grand.fill == Some(0xD9E1F2));
    assert_eq!(pt.out.indent(4, 0), 1);
}

#[test]
fn moving_fields_between_areas() {
    let wb = sample(false);
    let mut pt = new_pivot(&wb);
    let (region, product, sales) = (f(&pt, "Region"), f(&pt, "Product"), f(&pt, "Sales"));
    pt.add_field(region, Area::Rows, None);
    pt.add_field(product, Area::Rows, None);
    pt.move_entry(Area::Rows, 1, Area::Rows, 0);
    assert_eq!(pt.rows, vec![product, region]);
    pt.move_entry(Area::Rows, 0, Area::Columns, 0);
    assert_eq!((pt.rows.clone(), pt.cols.clone()), (vec![region], vec![product]));
    pt.add_field(sales, Area::Values, None);
    pt.add_field(sales, Area::Values, None);
    assert_eq!(pt.values[1].name, "Sum of Sales2");
    assert_eq!(pt.cols, vec![product, VALUES]);
    pt.remove_at(Area::Values, 0);
    assert_eq!(pt.cols, vec![product]);
    pt.clear();
    assert!(layout(&pt).placeholder);
}

/// A workbook with a few reports exercising most of what the file format carries.
pub(crate) fn rich_workbook() -> Workbook {
    let mut wb = sample(true);
    let base = new_pivot(&wb);
    let (region, product, date, sales, qty) = (f(&base, "Region"), f(&base, "Product"), f(&base, "Date"), f(&base, "Sales"), f(&base, "Qty"));
    // 1: compact, rows Region > Product, columns Σ Values, a report filter and item filters.
    let mut a = base.clone();
    a.add_field(region, Area::Rows, None);
    a.add_field(product, Area::Rows, None);
    a.add_field(sales, Area::Values, None);
    a.add_field(qty, Area::Values, None);
    a.values[1].agg = Agg::Average;
    a.values[1].name = "Average of Qty".into();
    a.values[1].num_fmt = "0.00".into();
    a.add_field(date, Area::Filters, None);
    a.fields[region].hidden.insert("north".into());
    a.fields[region].collapsed.insert("west".into());
    a.anchor = (3, 7);
    // 2: tabular, columns Product, % of grand total, sorted descending by value, no column grand totals.
    let mut b = base.clone();
    b.name = "PivotTable2".into();
    b.add_field(region, Area::Rows, None);
    b.add_field(product, Area::Columns, None);
    b.add_field(sales, Area::Values, None);
    b.values[0].show_as = ShowAs::PctGrand;
    b.values[0].num_fmt = "0.00%".into();
    b.fields[region].sort = SortOrder::Desc;
    b.fields[region].sort_by = Some(0);
    b.form = Form::Tabular;
    b.col_grand = false;
    b.style = "PivotStyleMedium9".into();
    b.anchor = (20, 7);
    // 3: dates grouped by years and months, counts.
    let mut c = base.clone();
    c.name = "PivotTable3".into();
    c.add_field(date, Area::Rows, None);
    c.group_dates(date, &[DatePart::Years, DatePart::Months]);
    c.add_field(qty, Area::Values, None);
    c.values[0].agg = Agg::Count;
    c.values[0].name = "Count of Qty".into();
    c.anchor = (2, 14);
    // 4: numbers grouped, outline form.
    let mut d = base;
    d.name = "PivotTable4".into();
    d.add_field(sales, Area::Rows, None);
    d.fields[sales].num_group = Some((0.0, 250.0, 100.0));
    d.add_field(product, Area::Rows, None);
    d.add_field(qty, Area::Values, None);
    d.form = Form::Outline;
    d.anchor = (2, 20);
    let mut sh = crate::model::Sheet::new("Report");
    sh.pivots = vec![a, b, c, d];
    wb.sheets.push(sh);
    for i in 0..4 {
        engine::update(&mut wb, 1, i).unwrap();
    }
    wb
}

#[test]
fn xlsx_round_trip_keeps_reports() {
    let wb = rich_workbook();
    let dir = std::env::temp_dir().join(format!("freedom_sheets_pivot_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pivots.xlsx");
    crate::io::save(&wb, &path).unwrap();
    let back = crate::io::load(&path).unwrap();
    let (x, y) = (&wb.sheets[1], &back.sheets[1]);
    assert_eq!(y.pivots.len(), 4);
    for (p, q) in x.pivots.iter().zip(&y.pivots) {
        assert_eq!(p.name, q.name);
        assert_eq!(p.source, q.source);
        assert_eq!(p.rows, q.rows, "{}", p.name);
        assert_eq!(p.cols, q.cols, "{}", p.name);
        assert_eq!(p.filters, q.filters, "{}", p.name);
        assert_eq!(p.values, q.values, "{}", p.name);
        assert_eq!(p.form, q.form, "{}", p.name);
        assert_eq!(p.style, q.style);
        assert_eq!((p.row_grand, p.col_grand), (q.row_grand, q.col_grand));
        for (fa, fb) in p.fields.iter().zip(&q.fields) {
            assert_eq!(fa.name, fb.name);
            assert_eq!(fa.source, fb.source);
            assert_eq!(fa.hidden, fb.hidden, "{} {}", p.name, fa.name);
            assert_eq!(fa.collapsed, fb.collapsed, "{} {}", p.name, fa.name);
            assert_eq!(fa.num_group, fb.num_group);
            assert_eq!(fa.sort_by, fb.sort_by, "{} {}", p.name, fa.name);
        }
        assert_eq!(p.out.range, q.out.range, "{}", p.name);
        assert_eq!(p.out.cells, q.out.cells, "{}", p.name);
        assert_eq!(*p.cache, *q.cache);
    }
}

/// Writes the sample reports to FS_PIVOT_XLSX (for opening the file in Excel).
#[test]
#[ignore]
fn export_for_excel() {
    let path = std::env::var("FS_PIVOT_XLSX").expect("FS_PIVOT_XLSX");
    let mut wb = rich_workbook();
    // FS_PIVOT_ONLY="0,2" keeps only those reports.
    if let Ok(only) = std::env::var("FS_PIVOT_ONLY") {
        let keep: Vec<usize> = only.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        let all = std::mem::take(&mut wb.sheets[1].pivots);
        for (i, p) in all.into_iter().enumerate() {
            if keep.contains(&i) {
                wb.sheets[1].pivots.push(p);
            } else {
                let full = p.out.full;
                let keys: Vec<(u32, u32)> = wb.sheets[1].iter_range(full).map(|(r, c, _)| (r, c)).collect();
                for (r, c) in keys {
                    wb.sheets[1].remove(r, c);
                }
            }
        }
    }
    crate::io::save(&wb, std::path::Path::new(&path)).unwrap();
}

#[test]
fn getpivotdata_formula_evaluates() {
    let mut wb = rich_workbook();
    let f = |t: &str| Some(Cell { formula: Some(Box::new(crate::model::Formula::new(t))), ..Default::default() });
    let sh = &mut wb.sheets[1];
    sh.set(0, 0, f("=GETPIVOTDATA(\"Sales\",$H$4,\"Region\",\"East\")"));
    sh.set(1, 0, f("=GETPIVOTDATA(\"Sum of Sales\",Report!$H$4)"));
    sh.set(1, 1, f("=GETPIVOTDATA(\"Sales\",$H$4,\"Region\",\"North\")"));
    wb.recalc();
    assert_eq!(wb.sheets[1].value(0, 0), &Value::Number(370.0));
    // North is filtered out: 710 - 80.
    assert_eq!(wb.sheets[1].value(1, 0), &Value::Number(630.0));
    assert_eq!(wb.sheets[1].value(1, 1), &Value::Error(crate::model::ErrorKind::Ref));
}

/// Prints the sample reports as text (compare with what Excel shows for the exported file).
#[test]
#[ignore]
fn print_pivot_samples() {
    for p in &rich_workbook().sheets[1].pivots {
        println!("== {} {}", p.name, p.out.range.to_a1());
        for l in dump(p) {
            println!("{l}");
        }
    }
}

/// Prints the reports of the workbook FS_PIVOT_IN (for files saved by Excel).
#[test]
#[ignore]
fn print_pivot_file() {
    let path = std::env::var("FS_PIVOT_IN").expect("FS_PIVOT_IN");
    let wb = crate::io::load(std::path::Path::new(&path)).unwrap();
    for sh in &wb.sheets {
        for p in &sh.pivots {
            println!("== {} {} {} rows={:?} cols={:?} filters={:?}", sh.name, p.name, p.out.range.to_a1(), p.rows, p.cols, p.filters);
            for l in dump(p) {
                println!("{l}");
            }
        }
    }
}

#[test]
fn sheet_rename_and_row_insert_follow_reports() {
    let mut wb = rich_workbook();
    super::rename_sheet(&mut wb, "Sheet1", "My Data");
    assert_eq!(wb.sheets[1].pivots[0].source, "'My Data'!$A$1:$E$8");
    wb.sheets[0].name = "My Data".into();
    assert!(engine::refresh(&mut wb, 1, 0).is_ok());
    let before = wb.sheets[1].pivots[0].out.full;
    super::shift_for_structure(&mut wb.sheets[1], true, 0, 2);
    let p = &wb.sheets[1].pivots[0];
    assert_eq!((p.out.full.r1, p.out.full.r2), (before.r1 + 2, before.r2 + 2));
    assert_eq!(p.anchor.0, p.out.range.r1);
}
