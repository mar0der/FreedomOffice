//! Writes sample workbooks for trying the web viewer: `cargo run -p freedomoffice-sheetview
//! --example sample -- DIR` creates `web-sample.xlsx` (formats, merges, frozen panes, two sheets)
//! and `web-big.xlsx` (60 000 rows) in DIR.

use rust_xlsxwriter::{Format, FormatAlign, FormatBorder, Formula, Workbook};

fn main() {
    let dir = std::env::args().nth(1).expect("usage: sample DIR");
    std::fs::create_dir_all(&dir).expect("create DIR");
    sample(&format!("{dir}/web-sample.xlsx"));
    big(&format!("{dir}/web-big.xlsx"));
}

fn sample(path: &str) {
    let mut wb = Workbook::new();
    let head = Format::new().set_bold().set_background_color(0x217346).set_font_color(0xffffff).set_align(FormatAlign::Center);
    let money = Format::new().set_num_format("#,##0.00");
    let pct = Format::new().set_num_format("0.0%").set_font_color(0xc00000);
    let date = Format::new().set_num_format("d mmm yyyy");
    let boxed = Format::new().set_border(FormatBorder::Medium).set_bold().set_background_color(0xffee99);
    let wrap = Format::new().set_text_wrap();
    let ws = wb.add_worksheet();
    ws.set_name("Budget").unwrap();
    ws.merge_range(0, 0, 0, 4, "Household budget 2026", &Format::new().set_bold().set_font_size(16).set_align(FormatAlign::Center)).unwrap();
    for (c, h) in ["Item", "Jan", "Feb", "Mar", "Total"].iter().enumerate() {
        ws.write_string_with_format(1, c as u16, *h, &head).unwrap();
    }
    let items = ["Rent", "Food", "Transport", "Internet", "Savings", "Fun"];
    for (i, name) in items.iter().enumerate() {
        let r = 2 + i as u32;
        ws.write_string(r, 0, *name).unwrap();
        for c in 1..4u16 {
            ws.write_number_with_format(r, c, 100.0 * (i as f64 + 1.0) + f64::from(c) * 7.5, &money).unwrap();
        }
        ws.write_formula_with_format(r, 4, Formula::new(format!("=SUM(B{0}:D{0})", r + 1)), &money).unwrap();
    }
    ws.write_string(8, 0, "Total").unwrap();
    for c in 1..5u16 {
        let col = (b'A' + c as u8) as char;
        ws.write_formula_with_format(8, c, Formula::new(format!("=SUM({col}3:{col}8)")), &boxed).unwrap();
    }
    ws.write_number_with_format(9, 4, 0.1234, &pct).unwrap();
    ws.write_number_with_format(10, 0, 46000.0, &date).unwrap();
    ws.write_string_with_format(11, 0, "A long note that is wrapped inside its cell so it needs several lines to read", &wrap).unwrap();
    ws.write_string(12, 0, "A long unwrapped text that spills over the empty cells on its right-hand side").unwrap();
    ws.write_boolean(13, 0, true).unwrap();
    ws.write_formula(14, 0, Formula::new("=1/0")).unwrap();
    ws.write_string(15, 0, "Hebrew: \u{5e9}\u{5dc}\u{5d5}\u{5dd}  Arabic: \u{645}\u{631}\u{62d}\u{628}\u{627}  emoji: \u{1f600}").unwrap();
    ws.set_row_height(11, 48.0).unwrap();
    ws.set_column_width(0, 22.0).unwrap();
    ws.set_column_width(5, 3.0).unwrap();
    ws.set_column_hidden(6).unwrap();
    ws.set_freeze_panes(2, 1).unwrap();
    let ws2 = wb.add_worksheet();
    ws2.set_name("Notes").unwrap();
    ws2.write_string(0, 0, "Second sheet").unwrap();
    ws2.write_formula(1, 0, Formula::new("=Budget!E9*2")).unwrap();
    ws2.write_formula(2, 0, Formula::new("=TEXT(TODAY(),\"yyyy\")")).unwrap();
    wb.save(path).unwrap();
}

fn big(path: &str) {
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    for r in 0..60_000u32 {
        ws.write_number(r, 0, f64::from(r + 1)).unwrap();
        ws.write_string(r, 1, format!("Customer {r}")).unwrap();
        ws.write_number(r, 2, f64::from(r % 997) * 1.25).unwrap();
        ws.write_formula(r, 3, Formula::new(format!("=C{}*2", r + 1))).unwrap();
    }
    wb.save(path).unwrap();
}
