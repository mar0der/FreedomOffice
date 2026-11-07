use cosync::{CellData, Op, Val};
use rust_xlsxwriter::{Format, Formula, Workbook};

use super::*;

fn xlsx() -> Vec<u8> {
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name("Data").unwrap();
    ws.write_number(0, 0, 2.0).unwrap();
    ws.write_number(1, 0, 3.0).unwrap();
    ws.write_formula(2, 0, Formula::new("=A1*A2")).unwrap();
    ws.write_number_with_format(3, 0, 1234.5, &Format::new().set_num_format("#,##0.00")).unwrap();
    ws.write_string_with_format(4, 0, "big", &Format::new().set_font_size(20)).unwrap();
    wb.save_to_buffer().unwrap()
}

fn texts(w: &Json) -> Vec<(u64, u64, String)> {
    w["cells"].as_array().unwrap().iter().map(|c| (c[0].as_u64().unwrap(), c[1].as_u64().unwrap(), c[2].as_str().unwrap().to_string())).collect()
}

#[test]
fn values_are_calculated_and_formatted() {
    let b = Book::open("a.xlsx", &xlsx(), &LiveInput::default()).unwrap();
    let w = b.window_at(0, 0, 0, 10, 3).unwrap();
    let t = texts(&w);
    assert!(t.contains(&(2, 0, "6".into())), "{t:?}");
    assert!(t.contains(&(3, 0, "1,234.50".into())), "{t:?}");
    let c = b.cell(0, 2, 0).unwrap();
    assert_eq!((c["input"].as_str(), c["formula"].as_bool()), (Some("=A1*A2"), Some(true)));
}

#[test]
fn rows_with_big_text_grow() {
    let b = Book::open("a.xlsx", &xlsx(), &LiveInput::default()).unwrap();
    let m = b.meta();
    let rh = m["sheets"][0]["row_h"].as_array().unwrap();
    let h = rh.iter().find(|x| x[0] == 4).map(|x| x[1].as_f64().unwrap());
    assert!(h.is_some_and(|h| h > 30.0), "{rh:?}");
}

#[test]
fn live_batches_apply_on_top_of_the_file() {
    let mut live = LiveInput::default();
    // The sheet ids of a file without live metadata are 1, 2, ... by position.
    live.batches.push(vec![Op::Content { s: 1, r: 0, c: 0, v: Some(Box::new(CellData { v: Val::Num(10.0), ..Default::default() })), adj: vec![] }]);
    let b = Book::open("a.xlsx", &xlsx(), &live).unwrap();
    let t = texts(&b.window_at(0, 0, 0, 10, 3).unwrap());
    assert!(t.contains(&(0, 0, "10".into())) && t.contains(&(2, 0, "30".into())), "{t:?}");
}

#[test]
fn windows_are_limited() {
    let b = Book::open("a.xlsx", &xlsx(), &LiveInput::default()).unwrap();
    assert!(b.window_at(0, 0, 0, 1000, 1).is_err());
    assert!(b.window_at(0, 0, 0, 999, 100).is_err());
    assert!(b.window_at(0, 5, 0, 1, 1).is_err());
    assert!(b.window_at(0, 0, 0, 2_000_000, 1).is_err());
    assert!(b.window_at(7, 0, 0, 1, 1).is_err());
    assert!(b.window_at(0, 0, 0, 999, 39).is_ok());
}

fn rich_xlsx() -> Vec<u8> {
    use rust_xlsxwriter::{Chart, ChartType, ConditionalFormat2ColorScale, ConditionalFormatCell, ConditionalFormatCellRule, ConditionalFormatDataBar, Note};
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name("Data").unwrap();
    for i in 0..5u32 {
        ws.write_number(i, 0, f64::from(i + 1)).unwrap();
        ws.write_number(i, 1, f64::from(i + 1)).unwrap();
        ws.write_number(i, 2, f64::from(i + 1)).unwrap();
    }
    ws.add_conditional_format(0, 0, 4, 0, &ConditionalFormatCell::new().set_rule(ConditionalFormatCellRule::GreaterThan(3)).set_format(Format::new().set_background_color(0xff0000).set_bold())).unwrap();
    ws.add_conditional_format(0, 1, 4, 1, &ConditionalFormat2ColorScale::new().set_minimum_color(0xffffff).set_maximum_color(0x00ff00)).unwrap();
    ws.add_conditional_format(0, 2, 4, 2, &ConditionalFormatDataBar::new()).unwrap();
    ws.insert_note(0, 4, &Note::new("remember")).unwrap();
    let mut ch = Chart::new(ChartType::Column);
    ch.add_series().set_values("Data!$A$1:$A$5").set_name("Mine");
    ch.title().set_name("Numbers");
    ws.insert_chart(7, 0, &ch).unwrap();
    wb.save_to_buffer().unwrap()
}

#[test]
fn conditional_formats_notes_and_charts_are_described() {
    let b = Book::open("a.xlsx", &rich_xlsx(), &LiveInput::default()).unwrap();
    let w = b.window_at(0, 0, 0, 10, 5).unwrap();
    let styles = w["styles"].as_array().unwrap();
    let cell = |r: u64, c: u64| w["cells"].as_array().unwrap().iter().find(|x| x[0] == r && x[1] == c).cloned().unwrap();
    let style_of = |r: u64, c: u64| {
        let si = cell(r, c)[4].as_u64().unwrap();
        if si == 0 { Json::Null } else { styles[si as usize - 1].clone() }
    };
    // "greater than 3": fill and bold on 4 and 5 only.
    assert_eq!(style_of(3, 0)["f"], "#ff0000");
    assert_eq!(style_of(3, 0)["b"], 1);
    assert!(style_of(1, 0).is_null());
    // The colour scale gives different fills; the data bar travels per cell.
    assert_ne!(style_of(0, 1)["f"], style_of(4, 1)["f"]);
    assert_eq!(style_of(4, 1)["f"], "#00ff00");
    let bar = &cell(4, 2)[6]["bar"];
    assert!(bar[1].as_f64().unwrap() > bar[0].as_f64().unwrap(), "{bar}");
    // A note marks its (empty) cell.
    assert_eq!(cell(0, 4)[6]["note"], 1);
    // The chart with its data.
    let ch = b.charts(0).unwrap();
    assert_eq!((ch[0]["kind"].as_str(), ch[0]["title"].as_str(), ch[0]["series"][0]["name"].as_str()), (Some("column"), Some("Numbers"), Some("Mine")));
    assert_eq!(ch[0]["series"][0]["values"], json!([1.0, 2.0, 3.0, 4.0, 5.0]));
    assert_eq!(b.meta()["sheets"][0]["charts"], 1);
}

#[test]
fn csv_files_read_as_text_and_never_as_formulas() {
    let b = Book::open("pe\u{202e}ople.csv", b"a,b\n=1+1,\"x,y\"\n", &LiveInput::default()).unwrap();
    let t = texts(&b.window_at(0, 0, 0, 5, 5).unwrap());
    assert!(t.contains(&(1, 0, "=1+1".into())) && t.contains(&(1, 1, "x,y".into())), "{t:?}");
    // The sheet's name has no direction control... it comes from the file name, minus bad characters.
    assert!(b.meta()["sheets"][0]["name"].as_str().unwrap().len() <= 31 * 4);
    assert_eq!(b.csv(0).unwrap(), "a,b\n'=1+1,\"x,y\"\n");
}

#[test]
fn hostile_input_is_an_error_not_a_crash() {
    for bytes in [Vec::new(), b"PK\x03\x04".to_vec(), vec![0xff; 100], xlsx()[..200].to_vec()] {
        let r = Book::open("x.xlsx", &bytes, &LiveInput::default());
        assert!(matches!(r, Err(LoadError::Invalid(_))), "{:?}", r.err());
    }
    for name in ["x.xls", "x.ods", "x.xlsb", "x.bin", "x"] {
        assert!(matches!(Book::open(name, b"data", &LiveInput::default()), Err(LoadError::Unsupported(_))), "{name}");
    }
}

#[test]
fn control_characters_and_long_text_are_cut() {
    let long = "x".repeat(5000);
    let csv = format!("{long}\n");
    let b = Book::open("a.csv", csv.as_bytes(), &LiveInput::default()).unwrap();
    let t = texts(&b.window_at(0, 0, 0, 1, 1).unwrap());
    assert_eq!(t[0].2.len(), MAX_DISPLAY_CHARS);
    // The formula bar gets the whole text (up to the cap).
    assert_eq!(b.cell(0, 0, 0).unwrap()["text"].as_str().unwrap().len(), 5000);
}
