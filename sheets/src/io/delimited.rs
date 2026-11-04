//! Delimited text (.csv / .tsv / .txt) import and export.

use std::path::Path;

use crate::model::{cell_from_input, Cell, Sheet, Value, Workbook};
use crate::numfmt;

/// Reads a delimited text file. `default_delim` is used when sniffing finds nothing;
/// `force` skips sniffing entirely.
pub fn read(path: &Path, default_delim: u8, force: bool) -> Result<Workbook, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let name = sheet_name_from(path);
    Ok(read_bytes(&bytes, &name, default_delim, force))
}

pub fn read_bytes(bytes: &[u8], sheet_name: &str, default_delim: u8, force: bool) -> Workbook {
    let text = decode(bytes);
    let delim = if force { default_delim } else { sniff(&text).unwrap_or(default_delim) };
    let mut wb = Workbook::empty_with(vec![Sheet::new(sheet_name)]);
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delim)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut rec = csv::StringRecord::new();
    let mut r: u32 = 0;
    while let Ok(true) = rdr.read_record(&mut rec) {
        for (c, field) in rec.iter().enumerate() {
            if field.is_empty() {
                continue;
            }
            let cell = if field.starts_with('=') || field.starts_with('\'') {
                // Never evaluate formulas from text files (CSV injection), keep apostrophes literal.
                Some(Cell { value: Value::Text(field.to_string()), ..Default::default() })
            } else {
                cell_from_input(&mut wb, field, 0)
            };
            wb.sheets[0].set(r, c as u32, cell);
        }
        r += 1;
        if r >= crate::model::MAX_ROWS {
            break;
        }
    }
    wb
}

fn sheet_name_from(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let clean: String =
        stem.chars().filter(|c| !matches!(c, '[' | ']' | ':' | '*' | '?' | '/' | '\\')).take(31).collect();
    let clean = clean.trim_matches('\'').trim().to_string();
    if clean.is_empty() { "Sheet1".into() } else { clean }
}

/// UTF-8 (with or without BOM), UTF-16 with BOM, else Windows-1252.
fn decode(bytes: &[u8]) -> String {
    if let Some(b) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(b).into_owned();
    }
    if let Some(b) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let u: Vec<u16> = b.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
        return String::from_utf16_lossy(&u);
    }
    if let Some(b) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let u: Vec<u16> = b.chunks_exact(2).map(|p| u16::from_be_bytes([p[0], p[1]])).collect();
        return String::from_utf16_lossy(&u);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| cp1252(b)).collect(),
    }
}

fn cp1252(b: u8) -> char {
    const HI: [u16; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x8D,
        0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A,
        0x0153, 0x9D, 0x017E, 0x0178,
    ];
    if (0x80..0xA0).contains(&b) {
        char::from_u32(HI[(b - 0x80) as usize] as u32).unwrap_or('?')
    } else {
        b as char
    }
}

/// Picks the delimiter (comma, semicolon, tab) that occurs most consistently on the first lines.
fn sniff(text: &str) -> Option<u8> {
    let cands = [b',', b';', b'\t'];
    let mut counts: Vec<[usize; 3]> = Vec::new();
    let mut cur = [0usize; 3];
    let mut in_q = false;
    for &b in text.as_bytes().iter().take(64 * 1024) {
        match b {
            b'"' => in_q = !in_q,
            b'\n' if !in_q => {
                counts.push(cur);
                cur = [0; 3];
                if counts.len() >= 20 {
                    break;
                }
            }
            _ if !in_q => {
                if let Some(i) = cands.iter().position(|c| *c == b) {
                    cur[i] += 1;
                }
            }
            _ => {}
        }
    }
    if cur.iter().any(|&n| n > 0) && counts.len() < 20 {
        counts.push(cur);
    }
    let mut best: Option<(usize, usize)> = None; // (score, idx)
    for i in 0..3 {
        let lines_with = counts.iter().filter(|l| l[i] > 0).count();
        if lines_with == 0 {
            continue;
        }
        let first = counts.first().map(|l| l[i]).unwrap_or(0);
        let consistent = counts.iter().filter(|l| l[i] == first).count();
        let total: usize = counts.iter().map(|l| l[i]).sum();
        let score = consistent * 1000 + lines_with * 100 + total.min(99);
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, i));
        }
    }
    best.map(|(_, i)| cands[i])
}

/// Writes the active sheet's displayed values as delimited text (UTF-8 BOM, CRLF).
pub fn write(wb: &Workbook, path: &Path, delim: u8) -> Result<(), String> {
    std::fs::write(path, to_text(wb, delim)).map_err(|e| format!("Could not save: {e}"))
}

pub fn to_text(wb: &Workbook, delim: u8) -> String {
    let sh = &wb.sheets[wb.active.min(wb.sheets.len().saturating_sub(1))];
    let mut out = String::from("\u{feff}");
    let Some((max_r, max_c)) = sh.extent_with_spills() else {
        return out;
    };
    let d = delim as char;
    for r in 0..=max_r {
        let row = sh.rows.get(&r);
        for c in 0..=max_c {
            if c > 0 {
                out.push(d);
            }
            // Spilled dynamic-array values are exported like ordinary values.
            let cell = row.and_then(|row| row.get(&c));
            let value = sh.display_value(r, c);
            if cell.is_some() || !value.is_empty() {
                let text = numfmt::format_value(value, &wb.style(cell.map_or(0, |c| c.style)).num_fmt).text;
                push_field(&mut out, &text, d);
            }
        }
        out.push_str("\r\n");
    }
    out
}

fn push_field(out: &mut String, s: &str, d: char) {
    if s.contains(d) || s.contains('"') || s.contains('\n') || s.contains('\r') {
        out.push('"');
        out.push_str(&s.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(s);
    }
}
