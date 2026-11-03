//! .xls / .xlsb / .ods import through calamine (values, formulas, merges; no styles).

use std::collections::HashMap;
use std::path::Path;

use calamine::{Data, Reader, Sheets};

use crate::model::{Cell, ErrorKind, Formula, Range, Sheet, Style, Value, Workbook};
use crate::numfmt;

pub fn read(path: &Path) -> Result<Workbook, String> {
    let mut book = calamine::open_workbook_auto(path).map_err(|e| format!("Could not open {}: {e}", path.display()))?;
    let names = book.sheet_names();
    let mut wb = Workbook::empty_with(Vec::new());
    let mut fmt_styles: HashMap<&'static str, u32> = HashMap::new();

    for name in &names {
        let mut sheet = Sheet::new(name);
        let range = match book.worksheet_range(name) {
            Ok(r) => r,
            Err(_) => {
                wb.sheets.push(sheet);
                continue;
            }
        };
        if let Some((r0, c0)) = range.start() {
            for (r, c, d) in range.used_cells() {
                let (r, c) = (r0 + r as u32, c0 + c as u32);
                let (value, fmt) = convert(d);
                let style = match fmt {
                    Some(f) => *fmt_styles
                        .entry(f)
                        .or_insert_with(|| wb.intern_style(Style { num_fmt: f.to_string(), ..Default::default() })),
                    None => 0,
                };
                sheet.set(r, c, Some(Cell { value, formula: None, style }));
            }
        }
        if let Ok(formulas) = book.worksheet_formula(name) {
            if let Some((r0, c0)) = formulas.start() {
                for (r, c, text) in formulas.used_cells() {
                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    let f = if text.starts_with("of:") || text.starts_with("oooc:") {
                        Formula::new(&odf_to_excel(text))
                    } else {
                        Formula::new(text)
                    };
                    if f.ast.is_none() {
                        continue; // keep just the cached value
                    }
                    sheet.cell_mut(r0 + r as u32, c0 + c as u32).formula = Some(Box::new(f));
                }
            }
        }
        if let Sheets::Xls(x) = &book {
            if let Ok(merges) = x.merge_cells_by_sheet_name(name) {
                for d in merges {
                    let rg = Range::new(d.start.0, d.start.1, d.end.0, d.end.1);
                    if !rg.is_single() {
                        sheet.merges.push(rg);
                    }
                }
            }
        }
        wb.sheets.push(sheet);
    }
    for (n, f) in book.defined_names() {
        let f = f.trim().trim_start_matches('=');
        if !n.is_empty() && !n.starts_with("_xlnm.") && !f.is_empty() && !wb.names.iter().any(|(x, _)| x == n) {
            wb.names.push((n.clone(), f.to_string()));
        }
    }
    if wb.sheets.is_empty() {
        wb.sheets.push(Sheet::new("Sheet1"));
    }
    Ok(wb)
}

fn date_fmt(serial: f64) -> &'static str {
    if serial.fract() == 0.0 {
        "m/d/yyyy"
    } else if serial < 1.0 {
        "h:mm:ss"
    } else {
        "m/d/yyyy h:mm"
    }
}

/// ISO date / datetime / time (as produced by the ODS reader) to a serial.
fn iso_serial(s: &str) -> Option<f64> {
    let (d, t) = match s.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let mut it = d.split('-');
    let y: i64 = it.next()?.trim().parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let dd: i64 = it.next()?.get(0..2)?.parse().ok()?;
    let mut serial = numfmt::ymd_to_serial(y, m, dd);
    if let Some(t) = t {
        let mut p = t.trim_end_matches('Z').split(':');
        let h: f64 = p.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let mi: f64 = p.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let sec: f64 = p.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        serial += (h * 3600.0 + mi * 60.0 + sec) / 86400.0;
    }
    Some(serial)
}

/// "PT12H30M15S" to a fraction of a day.
fn iso_duration(s: &str) -> Option<f64> {
    let rest = s.strip_prefix('-').unwrap_or(s).strip_prefix("PT")?;
    let mut total = 0.0;
    let mut num = String::new();
    for ch in rest.chars() {
        match ch {
            'H' | 'M' | 'S' => {
                let v: f64 = num.parse().ok()?;
                total += v * match ch {
                    'H' => 3600.0,
                    'M' => 60.0,
                    _ => 1.0,
                };
                num.clear();
            }
            _ => num.push(ch),
        }
    }
    let v = total / 86400.0;
    Some(if s.starts_with('-') { -v } else { v })
}

fn convert(d: &Data) -> (Value, Option<&'static str>) {
    match d {
        Data::Empty => (Value::Empty, None),
        Data::Int(i) => (Value::Number(*i as f64), None),
        Data::Float(f) => (Value::Number(*f), None),
        Data::String(s) => (Value::Text(s.clone()), None),
        Data::Bool(b) => (Value::Bool(*b), None),
        Data::DateTime(dt) => {
            let v = dt.as_f64();
            (Value::Number(v), Some(if dt.is_duration() { "[h]:mm:ss" } else { date_fmt(v) }))
        }
        Data::DateTimeIso(s) => match iso_serial(s) {
            Some(v) => (Value::Number(v), Some(date_fmt(v))),
            None => (Value::Text(s.clone()), None),
        },
        Data::DurationIso(s) => match iso_duration(s) {
            Some(v) => (Value::Number(v), Some("[h]:mm:ss")),
            None => (Value::Text(s.clone()), None),
        },
        Data::Error(e) => (Value::Error(ErrorKind::parse(&e.to_string()).unwrap_or(ErrorKind::NA)), None),
    }
}

/// Converts OpenFormula text (`of:=SUM([.A1:.B2];['My Sheet'.C3])`) to Excel syntax.
fn odf_to_excel(src: &str) -> String {
    let body = src.split_once(':').map(|(_, b)| b).unwrap_or(src);
    let body = body.strip_prefix('=').unwrap_or(body);
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                out.push('"');
                for c in chars.by_ref() {
                    out.push(c);
                    if c == '"' {
                        break; // a doubled quote simply re-enters this branch
                    }
                }
            }
            '[' => {
                let mut inner = String::new();
                let mut in_q = false;
                for c in chars.by_ref() {
                    match c {
                        '\'' => {
                            in_q = !in_q;
                            inner.push(c);
                        }
                        ']' if !in_q => break,
                        _ => inner.push(c),
                    }
                }
                out.push_str(&odf_ref(&inner));
            }
            ';' => out.push(','),
            _ => out.push(ch),
        }
    }
    out
}

/// `.A1`, `Sheet1.A1:.B2`, `$'My Sheet'.$A$1` -> `A1`, `Sheet1!A1:B2`, `'My Sheet'!$A$1`.
fn odf_ref(r: &str) -> String {
    let split = |part: &str| -> (String, String) {
        let part = part.strip_prefix('$').unwrap_or(part);
        let mut in_q = false;
        let mut dot = None;
        for (i, c) in part.char_indices() {
            match c {
                '\'' => in_q = !in_q,
                '.' if !in_q => dot = Some(i),
                _ => {}
            }
        }
        match dot {
            Some(i) => (part[..i].to_string(), part[i + 1..].to_string()),
            None => (String::new(), part.to_string()),
        }
    };
    let mut parts = Vec::new();
    let mut in_q = false;
    let mut start = 0;
    for (i, c) in r.char_indices() {
        match c {
            '\'' => in_q = !in_q,
            ':' if !in_q => {
                parts.push(&r[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&r[start..]);
    let (sheet, first) = split(parts[0]);
    let mut out = if sheet.is_empty() { first } else { format!("{sheet}!{first}") };
    for p in &parts[1..] {
        out.push(':');
        out.push_str(&split(p).1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odf_formulas() {
        assert_eq!(odf_to_excel("of:=SUM([.A1:.B2];[.C3])"), "SUM(A1:B2,C3)");
        assert_eq!(odf_to_excel("of:=['My Sheet'.$A$1]&\"a;b\""), "'My Sheet'!$A$1&\"a;b\"");
        assert_eq!(odf_to_excel("of:=[$Sheet1.A1:.A3]*2"), "Sheet1!A1:A3*2");
    }
}
