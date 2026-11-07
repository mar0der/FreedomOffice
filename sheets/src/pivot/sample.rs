//! Sample data for the pivot tests (shared with the application's tests, so it is not test-only).

use crate::model::{Cell, Style, Value, Workbook};

/// The sample data used to record Excel's results (Region, Product, Date, Sales, Qty).
pub fn sample(blank_row: bool) -> Workbook {
    let mut wb = Workbook::new();
    let date = wb.intern_style(Style { num_fmt: "m/d/yyyy".into(), ..Default::default() });
    let mut rows: Vec<(&str, &str, (i64, i64, i64), f64, f64)> = vec![
        ("East", "Apples", (2023, 1, 15), 100.0, 3.0),
        ("East", "Pears", (2023, 4, 10), 150.0, 5.0),
        ("West", "Apples", (2024, 2, 1), 200.0, 7.0),
        ("West", "Pears", (2024, 7, 21), 50.0, 1.0),
        ("East", "Apples", (2024, 3, 5), 120.0, 4.0),
        ("North", "Pears", (2023, 11, 30), 80.0, 2.0),
    ];
    if blank_row {
        rows.push(("", "Pears", (2023, 12, 30), 10.0, 1.0));
    }
    let sh = &mut wb.sheets[0];
    for (c, h) in ["Region", "Product", "Date", "Sales", "Qty"].iter().enumerate() {
        sh.set(0, c as u32, Some(Cell { value: Value::Text(h.to_string()), ..Default::default() }));
    }
    for (i, (reg, prod, (y, m, d), s, q)) in rows.into_iter().enumerate() {
        let r = i as u32 + 1;
        if !reg.is_empty() {
            sh.set(r, 0, Some(Cell { value: Value::Text(reg.into()), ..Default::default() }));
        }
        sh.set(r, 1, Some(Cell { value: Value::Text(prod.into()), ..Default::default() }));
        sh.set(r, 2, Some(Cell { value: Value::Number(crate::numfmt::ymd_to_serial(y, m, d)), formula: None, style: date }));
        sh.set(r, 3, Some(Cell { value: Value::Number(s), ..Default::default() }));
        sh.set(r, 4, Some(Cell { value: Value::Number(q), ..Default::default() }));
    }
    wb
}
