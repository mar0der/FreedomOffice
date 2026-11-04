//! Reads conditional formats (`<conditionalFormatting>` + `<dxfs>`) and data validations.
//! Rule types that the model does not support are skipped silently.

use std::collections::HashMap;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use super::{attr, attr_bool, flag, local, parse_color, read_text, skip, strip_future_prefixes, Theme, XmlReader, INDEXED};
use crate::condfmt::{parse_sqref, CmpOp, CondFormat, DiffFormat, RuleKind, TextOp};
use crate::model::Sheet;
use crate::validation::{Allow, ErrorStyle, Validation};

fn is_end(e: &quick_xml::events::BytesEnd, name: &str) -> bool {
    let n: &str = e.name().0;
    n.rsplit(':').next() == Some(name)
}

/// Differential formats from styles.xml, by dxfId.
pub(super) fn parse_dxfs(xml: &str, theme: &Theme) -> Vec<DiffFormat> {
    let mut out = Vec::new();
    let mut rd = Reader::from_str(xml);
    let mut in_dxfs = false;
    let mut cur: Option<DiffFormat> = None;
    let mut section = "";
    let mut border_side = false;
    loop {
        match rd.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let n = local(&e);
                match n {
                    "dxfs" => in_dxfs = true,
                    "cellXfs" | "cellStyleXfs" | "fonts" | "fills" | "borders" | "extLst" if !in_dxfs => skip(&mut rd, &e),
                    "dxf" if in_dxfs => {
                        cur = Some(DiffFormat::default());
                        section = "";
                    }
                    "font" | "fill" | "border" if cur.is_some() => section = n_static(n),
                    _ => {
                        let Some(d) = cur.as_mut() else { continue };
                        match (section, n) {
                            ("font", "b") => d.bold = flag(&e),
                            ("font", "i") => d.italic = flag(&e),
                            ("font", "strike") => d.strike = flag(&e),
                            ("font", "u") => d.underline = attr(&e, "val").map(|v| v != "none").unwrap_or(true),
                            ("font", "color") => d.color = parse_color(&e, theme, &INDEXED),
                            ("fill", "bgColor") => d.fill = parse_color(&e, theme, &INDEXED).or(d.fill),
                            ("fill", "fgColor") if d.fill.is_none() => d.fill = parse_color(&e, theme, &INDEXED),
                            ("border", "left" | "right" | "top" | "bottom" | "start" | "end") => {
                                border_side = attr(&e, "style").is_some_and(|s| s != "none");
                                if border_side && d.border.is_none() {
                                    d.border = Some(0x000000);
                                }
                            }
                            ("border", "color") if border_side => {
                                if let Some(c) = parse_color(&e, theme, &INDEXED) {
                                    d.border = Some(c);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                if is_end(&e, "dxf") {
                    if let Some(d) = cur.take() {
                        out.push(d);
                    }
                } else if is_end(&e, "dxfs") {
                    break;
                } else if is_end(&e, "font") || is_end(&e, "fill") || is_end(&e, "border") {
                    section = "";
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

fn n_static(n: &str) -> &'static str {
    match n {
        "font" => "font",
        "fill" => "fill",
        _ => "border",
    }
}

fn clean_formula(s: &str) -> String {
    strip_future_prefixes(s.trim().trim_start_matches('='))
}

/// A cfRule read from the sheet, before priorities and data-bar extensions are resolved.
struct RawRule {
    priority: i64,
    rule: CondFormat,
    ext_id: Option<String>,
}

fn parse_cf_rule(rd: &mut XmlReader, e: &BytesStart, is_start: bool, ranges: &[crate::model::Range], dxfs: &[DiffFormat], theme: &Theme) -> Option<RawRule> {
    let ty = attr(e, "type").unwrap_or_default();
    let mut formulas: Vec<String> = Vec::new();
    let mut colors: Vec<u32> = Vec::new();
    let mut ext_id = None;
    if is_start {
        loop {
            match rd.read_event() {
                Ok(Event::Start(c)) => match local(&c) {
                    "formula" => formulas.push(clean_formula(&read_text(rd, "formula"))),
                    "id" => ext_id = Some(read_text(rd, "id").trim().to_string()),
                    "color" => {
                        colors.extend(parse_color(&c, theme, &INDEXED));
                        skip(rd, &c);
                    }
                    _ => {}
                },
                Ok(Event::Empty(c)) if local(&c) == "color" => colors.extend(parse_color(&c, theme, &INDEXED)),
                Ok(Event::End(c)) if is_end(&c, "cfRule") => break,
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }
    let flag_of = |name: &str, def: bool| attr_bool(e, name).unwrap_or(def);
    let text = attr(e, "text").unwrap_or_default();
    let kind = match ty.as_str() {
        "cellIs" => RuleKind::CellValue {
            op: CmpOp::from_xml(&attr(e, "operator").unwrap_or_default())?,
            f1: formulas.first().cloned()?,
            f2: formulas.get(1).cloned().unwrap_or_default(),
        },
        "containsText" => RuleKind::Text { op: TextOp::Contains, text },
        "notContainsText" => RuleKind::Text { op: TextOp::NotContains, text },
        "beginsWith" => RuleKind::Text { op: TextOp::BeginsWith, text },
        "endsWith" => RuleKind::Text { op: TextOp::EndsWith, text },
        "duplicateValues" => RuleKind::Duplicate { unique: false },
        "uniqueValues" => RuleKind::Duplicate { unique: true },
        "top10" => RuleKind::Top {
            bottom: flag_of("bottom", false),
            percent: flag_of("percent", false),
            rank: attr(e, "rank").and_then(|v| v.parse().ok()).unwrap_or(10),
        },
        "aboveAverage" => {
            if attr(e, "stdDev").and_then(|v| v.parse::<i32>().ok()).unwrap_or(0) != 0 {
                return None;
            }
            RuleKind::Average { below: !flag_of("aboveAverage", true), equal: flag_of("equalAverage", false) }
        }
        "containsBlanks" => RuleKind::Blanks { not: false },
        "notContainsBlanks" => RuleKind::Blanks { not: true },
        "containsErrors" => RuleKind::Errors { not: false },
        "notContainsErrors" => RuleKind::Errors { not: true },
        "expression" => RuleKind::Expression { formula: formulas.first().cloned()? },
        "dataBar" => RuleKind::DataBar { color: colors.first().copied().unwrap_or(0x638EC6), gradient: true },
        "colorScale" if colors.len() >= 2 => RuleKind::ColorScale { colors: colors.into_iter().take(3).collect() },
        _ => return None,
    };
    let format = attr(e, "dxfId").and_then(|v| v.parse::<usize>().ok()).and_then(|i| dxfs.get(i).cloned()).unwrap_or_default();
    let mut rule = CondFormat::new(ranges.to_vec(), kind, format);
    rule.stop = flag_of("stopIfTrue", false);
    let priority = attr(e, "priority").and_then(|v| v.parse().ok()).unwrap_or(i64::MAX);
    Some(RawRule { priority, rule, ext_id })
}

fn parse_dv(rd: &mut XmlReader, e: &BytesStart, is_start: bool) -> Option<Validation> {
    let get = |n: &str| attr(e, n).unwrap_or_default();
    let on = |n: &str| attr_bool(e, n).unwrap_or(false);
    let mut v = Validation {
        allow: Allow::from_xml(&get("type")),
        op: CmpOp::from_xml(&get("operator")).unwrap_or_default(),
        ignore_blank: on("allowBlank"),
        // showDropDown="1" hides the in-cell arrow.
        dropdown: !on("showDropDown"),
        show_input: on("showInputMessage"),
        input_title: get("promptTitle"),
        input_msg: get("prompt"),
        show_error: on("showErrorMessage"),
        error_style: ErrorStyle::from_xml(&get("errorStyle")),
        error_title: get("errorTitle"),
        error_msg: get("error"),
        ..Default::default()
    };
    let mut sq = get("sqref");
    if is_start {
        loop {
            match rd.read_event() {
                Ok(Event::Start(c)) => match local(&c) {
                    "formula1" => v.f1 = clean_formula(&read_text(rd, "formula1")),
                    "formula2" => v.f2 = clean_formula(&read_text(rd, "formula2")),
                    "sqref" => sq = read_text(rd, "sqref"),
                    _ => {}
                },
                Ok(Event::End(c)) if is_end(&c, "dataValidation") => break,
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }
    v.ranges = parse_sqref(&sq)?;
    Some(v)
}

/// Reads conditional formats and validations from a worksheet part.
pub(super) fn parse_sheet_extras(xml: &str, theme: &Theme, dxfs: &[DiffFormat], sheet: &mut Sheet) {
    if !xml.contains("conditionalFormatting") && !xml.contains("dataValidation") {
        return;
    }
    let mut rd = Reader::from_str(xml);
    let mut rules: Vec<RawRule> = Vec::new();
    let mut gradients: HashMap<String, bool> = HashMap::new();
    loop {
        let ev = rd.read_event();
        match ev {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let is_start = matches!(ev, Ok(Event::Start(_)));
                match local(e) {
                    "sheetData" | "sheetViews" | "cols" | "mergeCells" | "autoFilter" | "hyperlinks" if is_start => skip(&mut rd, e),
                    "conditionalFormatting" if is_start => {
                        let ranges = attr(e, "sqref").and_then(|s| parse_sqref(&s)).unwrap_or_default();
                        loop {
                            match rd.read_event() {
                                Ok(Event::Start(c)) if local(&c) == "cfRule" => {
                                    if let Some(r) = parse_cf_rule(&mut rd, &c, true, &ranges, dxfs, theme) {
                                        rules.push(r);
                                    }
                                }
                                Ok(Event::Empty(c)) if local(&c) == "cfRule" => {
                                    if let Some(r) = parse_cf_rule(&mut rd, &c, false, &ranges, dxfs, theme) {
                                        rules.push(r);
                                    }
                                }
                                Ok(Event::End(c)) if is_end(&c, "conditionalFormatting") => break,
                                Ok(Event::Eof) | Err(_) => break,
                                _ => {}
                            }
                        }
                    }
                    "dataValidation" => {
                        if let Some(v) = parse_dv(&mut rd, e, is_start) {
                            sheet.validations.push(v);
                        }
                    }
                    "extLst" if is_start => {
                        // Excel 2010 extensions: data bar gradients and cross-sheet list validations.
                        let mut cur_id = String::new();
                        loop {
                            let ev = rd.read_event();
                            match ev {
                                Ok(Event::Start(ref c)) | Ok(Event::Empty(ref c)) => {
                                    let st = matches!(ev, Ok(Event::Start(_)));
                                    match local(c) {
                                        "cfRule" => cur_id = attr(c, "id").unwrap_or_default(),
                                        "dataBar" => {
                                            if let Some(g) = attr_bool(c, "gradient") {
                                                gradients.insert(cur_id.clone(), g);
                                            }
                                        }
                                        "dataValidation" => {
                                            if let Some(v) = parse_dv(&mut rd, c, st) {
                                                sheet.validations.push(v);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                Ok(Event::End(c)) if is_end(&c, "extLst") => break,
                                Ok(Event::Eof) | Err(_) => break,
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    rules.sort_by_key(|r| r.priority);
    for mut r in rules {
        if let (RuleKind::DataBar { gradient, .. }, Some(id)) = (&mut r.rule.kind, &r.ext_id) {
            if let Some(g) = gradients.get(id) {
                *gradient = *g;
            }
        }
        if !r.rule.ranges.is_empty() {
            sheet.cond_formats.push(r.rule);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::condfmt::{parse_sqref, CmpOp, CondFormat, DiffFormat, RuleKind, TextOp};
    use crate::model::{Cell, Sheet, Value, Workbook};
    use crate::validation::{Allow, ErrorStyle, Validation};

    #[test]
    fn conditional_formats_and_validations_round_trip() {
        let mut wb = Workbook::empty_with(vec![Sheet::new("Data"), Sheet::new("Lists")]);
        let rg = |s: &str| parse_sqref(s).unwrap();
        let bad = DiffFormat { fill: Some(0xFFC7CE), color: Some(0x9C0006), ..Default::default() };
        let fancy = DiffFormat { bold: true, italic: true, underline: true, strike: true, border: Some(0xFF0000), ..Default::default() };
        let mut rules = vec![
            CondFormat::new(rg("A1:A10"), RuleKind::CellValue { op: CmpOp::Greater, f1: "5".into(), f2: String::new() }, bad.clone()),
            CondFormat::new(rg("A1:A10 C1:C10"), RuleKind::CellValue { op: CmpOp::Between, f1: "$B$1".into(), f2: "\"zz\"".into() }, fancy.clone()),
            CondFormat::new(rg("B1:B10"), RuleKind::Text { op: TextOp::Contains, text: "abc".into() }, bad.clone()),
            CondFormat::new(rg("B1:B10"), RuleKind::Text { op: TextOp::EndsWith, text: "z".into() }, bad.clone()),
            CondFormat::new(rg("D1:D10"), RuleKind::Duplicate { unique: false }, bad.clone()),
            CondFormat::new(rg("D1:D10"), RuleKind::Duplicate { unique: true }, fancy.clone()),
            CondFormat::new(rg("E1:E10"), RuleKind::Top { bottom: true, percent: true, rank: 15 }, bad.clone()),
            CondFormat::new(rg("E1:E10"), RuleKind::Average { below: true, equal: true }, bad.clone()),
            CondFormat::new(rg("F1:F10"), RuleKind::Blanks { not: true }, bad.clone()),
            CondFormat::new(rg("F1:F10"), RuleKind::Errors { not: false }, bad.clone()),
            CondFormat::new(rg("G2:H10"), RuleKind::Expression { formula: "AND($G2>1,H2<>\"x\")".into() }, bad.clone()),
            CondFormat::new(rg("I1:I10"), RuleKind::DataBar { color: 0x63BE7B, gradient: false }, DiffFormat::default()),
            CondFormat::new(rg("J1:J10"), RuleKind::DataBar { color: 0x638EC6, gradient: true }, DiffFormat::default()),
            CondFormat::new(rg("K1:K10"), RuleKind::ColorScale { colors: vec![0xF8696B, 0xFFEB84, 0x63BE7B] }, DiffFormat::default()),
            CondFormat::new(rg("L1:L10"), RuleKind::ColorScale { colors: vec![0xFFFFFF, 0x63BE7B] }, DiffFormat::default()),
        ];
        rules[0].stop = true;
        wb.sheets[0].cond_formats = rules.clone();
        let vals = vec![
            Validation {
                ranges: rg("A1:A20"),
                allow: Allow::List,
                f1: "\"Red,Green,Blue\"".into(),
                input_title: "Colour".into(),
                input_msg: "Pick one".into(),
                error_style: ErrorStyle::Warning,
                error_title: "Oops".into(),
                error_msg: "Not a colour".into(),
                ..Default::default()
            },
            Validation { ranges: rg("B1:B5 D1:D5"), allow: Allow::Whole, op: CmpOp::Between, f1: "1".into(), f2: "$C$1".into(), ..Default::default() },
            Validation { ranges: rg("C2"), allow: Allow::List, f1: "Lists!$A$1:$A$3".into(), dropdown: false, ignore_blank: false, ..Default::default() },
            Validation { ranges: rg("E1:E5"), allow: Allow::Custom, f1: "ISNUMBER(E1)".into(), error_style: ErrorStyle::Information, ..Default::default() },
            Validation { ranges: rg("F1"), allow: Allow::TextLength, op: CmpOp::LessEq, f1: "10".into(), show_error: false, ..Default::default() },
            Validation { ranges: rg("G1"), allow: Allow::Date, op: CmpOp::Greater, f1: "45292".into(), ..Default::default() },
            Validation { ranges: rg("H1"), allow: Allow::Time, op: CmpOp::Less, f1: "0.5".into(), ..Default::default() },
            Validation { ranges: rg("I1"), allow: Allow::Decimal, op: CmpOp::NotEqual, f1: "0".into(), ..Default::default() },
        ];
        wb.sheets[0].validations = vals.clone();
        wb.sheets[0].set(0, 0, Some(Cell { value: Value::Number(1.0), ..Default::default() }));

        let dir = std::env::temp_dir().join(format!("freedom_sheets_cfdv_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cf_dv.xlsx");
        crate::io::save(&wb, &path).unwrap();
        let back = crate::io::load(&path).unwrap();
        let s = &back.sheets[0];
        assert_eq!(s.cond_formats.len(), rules.len());
        for (a, b) in s.cond_formats.iter().zip(&rules) {
            assert_eq!(a, b);
        }
        assert_eq!(s.validations.len(), vals.len());
        for (a, b) in s.validations.iter().zip(&vals) {
            assert_eq!(a, b);
        }
        assert!(back.sheets[1].cond_formats.is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
