//! The workbook as an agent sees it: sheets and their cells, kept up to date by applying
//! `cosync` operations. Styles, notes, merges and other per-sheet features are not tracked
//! (only counted as `ignored`); values, formulas, sheets and row/column structure are.

use std::collections::BTreeMap;
use std::io::Cursor;

use calamine::{Data, Reader, Xlsx, open_workbook_from_rs};
use cosync::{CellData, MAX_COLS, MAX_ROWS, Op, SheetId, Val, map_index};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sheet {
    pub name: String,
    pub pos: f64,
    pub cells: BTreeMap<(u32, u32), CellData>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Book {
    pub sheets: BTreeMap<SheetId, Sheet>,
    /// Operations applied that this model doesn't track (formats, notes, merges, ...).
    pub ignored: u64,
}

/// A1-style column name (0 = A).
pub fn col_name(mut c: u32) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (c % 26) as u8) as char);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s
}

/// "B7" -> (6, 1) as (row, col), both 0-based.
pub fn parse_a1(a: &str) -> Option<(u32, u32)> {
    let a = a.trim().replace('$', "");
    let split = a.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = a.split_at(split);
    if letters.is_empty() || !letters.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let col = letters.to_ascii_uppercase().bytes().fold(0u32, |n, b| n * 26 + (b - b'A') as u32 + 1) - 1;
    let row: u32 = digits.parse::<u32>().ok()?.checked_sub(1)?;
    (row < MAX_ROWS && col < MAX_COLS).then_some((row, col))
}

pub fn a1(r: u32, c: u32) -> String {
    format!("{}{}", col_name(c), r + 1)
}

/// A value as text (what a cell shows, without number formats).
pub fn val_text(v: &Val) -> String {
    match v {
        Val::Empty => String::new(),
        Val::Num(n) => format!("{n}"),
        Val::Text(t) => t.clone(),
        Val::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Val::Err(e) => e.clone(),
    }
}

impl Book {
    /// The workbook of an .xlsx file (values and formula text), with the sheets' live ids from
    /// the server's `X-Live-Sheets` ("id:pos,..."), or by position (1, 2, ...) when there is none.
    pub fn from_xlsx(bytes: &[u8], meta: &str) -> Result<Book, String> {
        let mut xl: Xlsx<_> = open_workbook_from_rs(Cursor::new(bytes)).map_err(|e| format!("Couldn't read the workbook: {e}"))?;
        let names = xl.sheet_names();
        let ids = sheet_ids(meta, names.len());
        let mut book = Book::default();
        for (name, (id, pos)) in names.iter().zip(ids) {
            let mut sheet = Sheet { name: name.clone(), pos, cells: BTreeMap::new() };
            if let Ok(range) = xl.worksheet_range(name) {
                if let Some((r0, c0)) = range.start() {
                    for (r, c, d) in range.used_cells() {
                        let v = match d {
                            Data::Empty => continue,
                            Data::Int(i) => Val::Num(*i as f64),
                            Data::Float(f) => Val::Num(*f),
                            Data::String(s) => Val::Text(s.clone()),
                            Data::Bool(b) => Val::Bool(*b),
                            Data::DateTime(d) => Val::Num(d.as_f64()),
                            Data::DateTimeIso(s) | Data::DurationIso(s) => Val::Text(s.clone()),
                            Data::Error(e) => Val::Err(e.to_string()),
                        };
                        sheet.cells.insert((r0 + r as u32, c0 + c as u32), CellData { v, ..Default::default() });
                    }
                }
            }
            if let Ok(formulas) = xl.worksheet_formula(name) {
                if let Some((r0, c0)) = formulas.start() {
                    for (r, c, f) in formulas.used_cells() {
                        let f = f.trim().trim_start_matches('=');
                        if f.is_empty() {
                            continue;
                        }
                        // Formula cells carry no value on the wire: everyone calculates.
                        sheet.cells.insert((r0 + r as u32, c0 + c as u32), CellData { f: Some(f.to_string()), ..Default::default() });
                    }
                }
            }
            book.sheets.insert(id, sheet);
        }
        Ok(book)
    }

    /// The sheet ids the server should know (the `Hello` message).
    pub fn sheet_ids(&self) -> Vec<SheetId> {
        self.sheets.keys().copied().collect()
    }

    /// Sheets in tab order.
    pub fn ordered(&self) -> Vec<(SheetId, &Sheet)> {
        let mut v: Vec<_> = self.sheets.iter().map(|(i, s)| (*i, s)).collect();
        v.sort_by(|a, b| a.1.pos.total_cmp(&b.1.pos).then(a.0.cmp(&b.0)));
        v
    }

    /// A sheet by name (any case) or by numeric id.
    pub fn find(&self, name_or_id: &str) -> Option<SheetId> {
        self.sheets.iter().find(|(id, s)| s.name.eq_ignore_ascii_case(name_or_id) || id.to_string() == name_or_id).map(|(i, _)| *i)
    }

    pub fn cell(&self, s: SheetId, r: u32, c: u32) -> Option<&CellData> {
        self.sheets.get(&s)?.cells.get(&(r, c))
    }

    pub fn apply_all(&mut self, ops: &[Op]) {
        for op in ops {
            self.apply(op);
        }
    }

    pub fn apply(&mut self, op: &Op) {
        match op {
            Op::Cell { s, r, c, v, .. } => {
                if let Some(sh) = self.sheets.get_mut(s) {
                    match v {
                        Some(d) => {
                            sh.cells.insert((*r, *c), (**d).clone());
                        }
                        None => {
                            sh.cells.remove(&(*r, *c));
                        }
                    }
                }
            }
            Op::Rows { s, at, n } => {
                if let Some(sh) = self.sheets.get_mut(s) {
                    sh.cells = std::mem::take(&mut sh.cells).into_iter().filter_map(|((r, c), d)| Some(((map_index(r, *at, *n, MAX_ROWS)?, c), d))).collect();
                }
            }
            Op::Cols { s, at, n } => {
                if let Some(sh) = self.sheets.get_mut(s) {
                    sh.cells = std::mem::take(&mut sh.cells).into_iter().filter_map(|((r, c), d)| Some(((r, map_index(c, *at, *n, MAX_COLS)?), d))).collect();
                }
            }
            Op::AddSheet { s, name, pos } => {
                self.sheets.entry(*s).or_insert_with(|| Sheet { name: name.clone(), pos: *pos, cells: BTreeMap::new() });
            }
            Op::DelSheet { s, .. } => {
                self.sheets.remove(s);
            }
            Op::Rename { s, to, .. } => {
                if let Some(sh) = self.sheets.get_mut(s) {
                    sh.name = to.clone();
                }
            }
            Op::Place { s, pos } => {
                if let Some(sh) = self.sheets.get_mut(s) {
                    sh.pos = *pos;
                }
            }
            _ => self.ignored += 1,
        }
    }

    /// A short fingerprint of everything tracked (sheets, names, order, cells).
    pub fn digest(&self) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut feed = |b: &[u8]| {
            for x in b {
                h ^= *x as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        for (id, sh) in &self.sheets {
            feed(format!("S{id}|{}|{}", sh.name, sh.pos).as_bytes());
            for ((r, c), d) in &sh.cells {
                feed(format!("C{r},{c}={}", serde_json::to_string(d).unwrap_or_default()).as_bytes());
            }
        }
        format!("{h:016x}")
    }

    pub fn cell_count(&self) -> usize {
        self.sheets.values().map(|s| s.cells.len()).sum()
    }

    /// The first difference between two books, for reports.
    pub fn first_difference(&self, other: &Book) -> Option<String> {
        for (id, a) in &self.sheets {
            let Some(b) = other.sheets.get(id) else { return Some(format!("sheet {id} ({}) is missing on the other side", a.name)) };
            if a.name != b.name || a.pos != b.pos {
                return Some(format!("sheet {id}: '{}'@{} vs '{}'@{}", a.name, a.pos, b.name, b.pos));
            }
            for (k, d) in &a.cells {
                if b.cells.get(k) != Some(d) {
                    return Some(format!("sheet {id} {}: {:?} vs {:?}", a1(k.0, k.1), d, b.cells.get(k)));
                }
            }
            if let Some(k) = b.cells.keys().find(|k| !a.cells.contains_key(k)) {
                return Some(format!("sheet {id} {}: missing vs {:?}", a1(k.0, k.1), b.cells[k]));
            }
        }
        other.sheets.keys().find(|id| !self.sheets.contains_key(id)).map(|id| format!("sheet {id} is missing on this side"))
    }
}

/// (id, pos) per sheet: from "id:pos,..." when it matches the sheet count, else by position.
fn sheet_ids(meta: &str, n: usize) -> Vec<(SheetId, f64)> {
    let parsed: Vec<(SheetId, f64)> = meta
        .split(',')
        .filter_map(|p| {
            let (a, b) = p.split_once(':')?;
            Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
        })
        .collect();
    if parsed.len() == n && n > 0 { parsed } else { (0..n).map(|i| (i as u64 + 1, i as f64)).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: SheetId, r: u32, c: u32, t: &str) -> Op {
        Op::Cell { s, r, c, v: Some(Box::new(CellData { v: Val::Text(t.into()), ..Default::default() })), adj: vec![] }
    }

    #[test]
    fn a1_round_trip() {
        assert_eq!(a1(0, 0), "A1");
        assert_eq!(a1(6, 27), "AB7");
        assert_eq!(parse_a1("AB7"), Some((6, 27)));
        assert_eq!(parse_a1("$c$3"), Some((2, 2)));
        assert_eq!(parse_a1("7"), None);
    }

    #[test]
    fn rows_and_cols_move_and_delete_cells() {
        let mut b = Book::default();
        b.apply(&Op::AddSheet { s: 1, name: "S".into(), pos: 0.0 });
        b.apply_all(&[text(1, 2, 2, "x"), text(1, 3, 0, "y")]);
        b.apply(&Op::Rows { s: 1, at: 0, n: 2 });
        assert!(b.cell(1, 4, 2).is_some() && b.cell(1, 5, 0).is_some());
        b.apply(&Op::Rows { s: 1, at: 4, n: -1 });
        assert!(b.cell(1, 4, 2).is_none() && b.cell(1, 4, 0).is_some());
        b.apply(&Op::Cols { s: 1, at: 0, n: 1 });
        assert!(b.cell(1, 4, 1).is_some());
    }
}
