//! Core workbook data model: values, styles, cells, sheets and addressing helpers.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use crate::formula::{self, Expr};

pub const MAX_ROWS: u32 = 1_048_576;
pub const MAX_COLS: u32 = 16_384;
pub const DEFAULT_COL_W: f32 = 64.0;
pub const DEFAULT_ROW_H: f32 = 20.0;
pub const DEFAULT_FONT_SIZE: f32 = 11.0;

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    Null,
    Div0,
    Value,
    Ref,
    Name,
    Num,
    NA,
    Calc,
    Spill,
}

impl ErrorKind {
    pub const ALL: [ErrorKind; 9] = [
        ErrorKind::Null,
        ErrorKind::Div0,
        ErrorKind::Value,
        ErrorKind::Ref,
        ErrorKind::Name,
        ErrorKind::Num,
        ErrorKind::NA,
        ErrorKind::Calc,
        ErrorKind::Spill,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Null => "#NULL!",
            ErrorKind::Div0 => "#DIV/0!",
            ErrorKind::Value => "#VALUE!",
            ErrorKind::Ref => "#REF!",
            ErrorKind::Name => "#NAME?",
            ErrorKind::Num => "#NUM!",
            ErrorKind::NA => "#N/A",
            ErrorKind::Calc => "#CALC!",
            ErrorKind::Spill => "#SPILL!",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|e| e.as_str().eq_ignore_ascii_case(s.trim()))
    }

    /// Code used by ERROR.TYPE.
    pub fn code(self) -> f64 {
        match self {
            ErrorKind::Null => 1.0,
            ErrorKind::Div0 => 2.0,
            ErrorKind::Value => 3.0,
            ErrorKind::Ref => 4.0,
            ErrorKind::Name => 5.0,
            ErrorKind::Num => 6.0,
            ErrorKind::NA => 7.0,
            ErrorKind::Calc => 14.0,
            ErrorKind::Spill => 9.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub enum Value {
    #[default]
    Empty,
    Number(f64),
    Text(String),
    Bool(bool),
    Error(ErrorKind),
}

impl Value {
    pub fn is_empty(&self) -> bool {
        matches!(self, Value::Empty)
    }
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Addressing
// ---------------------------------------------------------------------------

/// 0-based column index to letters (0 -> "A").
pub fn col_name(mut c: u32) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

/// Column letters to 0-based index.
pub fn parse_col(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 3 {
        return None;
    }
    let mut n: u32 = 0;
    for ch in s.chars() {
        if !ch.is_ascii_alphabetic() {
            return None;
        }
        n = n * 26 + (ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1);
    }
    if n == 0 || n > MAX_COLS {
        return None;
    }
    Some(n - 1)
}

pub fn cell_name(r: u32, c: u32) -> String {
    format!("{}{}", col_name(c), r + 1)
}

/// Parses "A1" / "$A$1" into 0-based (row, col).
pub fn parse_cell(s: &str) -> Option<(u32, u32)> {
    let s = s.trim().replace('$', "");
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = s.split_at(split);
    let col = parse_col(letters)?;
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let row: u32 = digits.parse().ok()?;
    if row == 0 || row > MAX_ROWS {
        return None;
    }
    Some((row - 1, col))
}

/// A normalized rectangular cell range (inclusive, 0-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Range {
    pub r1: u32,
    pub c1: u32,
    pub r2: u32,
    pub c2: u32,
}

impl Range {
    pub fn new(ra: u32, ca: u32, rb: u32, cb: u32) -> Self {
        Range { r1: ra.min(rb), c1: ca.min(cb), r2: ra.max(rb), c2: ca.max(cb) }
    }
    pub fn cell(r: u32, c: u32) -> Self {
        Range { r1: r, c1: c, r2: r, c2: c }
    }
    pub fn contains(&self, r: u32, c: u32) -> bool {
        r >= self.r1 && r <= self.r2 && c >= self.c1 && c <= self.c2
    }
    pub fn rows(&self) -> u32 {
        self.r2 - self.r1 + 1
    }
    pub fn cols(&self) -> u32 {
        self.c2 - self.c1 + 1
    }
    pub fn is_single(&self) -> bool {
        self.r1 == self.r2 && self.c1 == self.c2
    }
    pub fn intersects(&self, o: &Range) -> bool {
        self.r1 <= o.r2 && o.r1 <= self.r2 && self.c1 <= o.c2 && o.c1 <= self.c2
    }
    pub fn union(&self, o: &Range) -> Range {
        Range { r1: self.r1.min(o.r1), c1: self.c1.min(o.c1), r2: self.r2.max(o.r2), c2: self.c2.max(o.c2) }
    }
    pub fn is_full_cols(&self) -> bool {
        self.r1 == 0 && self.r2 == MAX_ROWS - 1
    }
    pub fn is_full_rows(&self) -> bool {
        self.c1 == 0 && self.c2 == MAX_COLS - 1
    }
    pub fn to_a1(&self) -> String {
        if self.is_full_cols() && !self.is_full_rows() {
            return format!("{}:{}", col_name(self.c1), col_name(self.c2));
        }
        if self.is_full_rows() && !self.is_full_cols() {
            return format!("{}:{}", self.r1 + 1, self.r2 + 1);
        }
        if self.is_single() {
            cell_name(self.r1, self.c1)
        } else {
            format!("{}:{}", cell_name(self.r1, self.c1), cell_name(self.r2, self.c2))
        }
    }
    /// Parses "A1", "A1:B2", "A:C", "3:5".
    pub fn parse(s: &str) -> Option<Range> {
        let s = s.trim();
        if let Some((a, b)) = s.split_once(':') {
            if let (Some((r1, c1)), Some((r2, c2))) = (parse_cell(a), parse_cell(b)) {
                return Some(Range::new(r1, c1, r2, c2));
            }
            let (a, b) = (a.replace('$', ""), b.replace('$', ""));
            if let (Some(c1), Some(c2)) = (parse_col(&a), parse_col(&b)) {
                return Some(Range::new(0, c1, MAX_ROWS - 1, c2));
            }
            if let (Ok(r1), Ok(r2)) = (a.parse::<u32>(), b.parse::<u32>()) {
                if r1 >= 1 && r2 >= 1 && r1 <= MAX_ROWS && r2 <= MAX_ROWS {
                    return Some(Range::new(r1 - 1, 0, r2 - 1, MAX_COLS - 1));
                }
            }
            None
        } else {
            parse_cell(s).map(|(r, c)| Range::cell(r, c))
        }
    }
}

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum HAlign {
    #[default]
    General,
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum VAlign {
    Top,
    Center,
    #[default]
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum BorderLine {
    #[default]
    None,
    Thin,
    Medium,
    Thick,
    Double,
    Dashed,
    Dotted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Border {
    pub left: BorderLine,
    pub right: BorderLine,
    pub top: BorderLine,
    pub bottom: BorderLine,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Style {
    /// Font family name, `None` = default (Calibri).
    pub font: Option<String>,
    /// Font size in tenths of a point, 0 = default (11 pt).
    pub size10: u16,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// 0xRRGGBB
    pub color: Option<u32>,
    pub fill: Option<u32>,
    pub halign: HAlign,
    pub valign: VAlign,
    pub wrap: bool,
    pub indent: u8,
    /// Text rotation in xlsx terms: 0 none, 1-90 counterclockwise degrees,
    /// 91-180 = clockwise (value - 90) degrees, 255 = vertical stacked text.
    pub rotation: u8,
    /// Number format code; empty = "General".
    pub num_fmt: String,
    pub border: Border,
    /// Protection: the cell can be edited while the sheet is protected (Excel's default is locked).
    pub unlocked: bool,
    /// Protection: the formula is hidden while the sheet is protected.
    pub hide_formula: bool,
}

impl Style {
    pub fn font_size(&self) -> f32 {
        if self.size10 == 0 { DEFAULT_FONT_SIZE } else { self.size10 as f32 / 10.0 }
    }
    pub fn set_font_size(&mut self, pt: f32) {
        self.size10 = (pt * 10.0).round().clamp(10.0, 4090.0) as u16;
    }
}

// ---------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Formula {
    /// Formula text without the leading '='.
    pub text: String,
    pub ast: Option<Expr>,
}

impl Formula {
    pub fn new(text: &str) -> Self {
        let text = text.trim_start_matches('=').to_string();
        let ast = formula::parse(&text).ok();
        Formula { text, ast }
    }
    pub fn from_ast(ast: Expr) -> Self {
        Formula { text: formula::print(&ast), ast: Some(ast) }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Cell {
    pub value: Value,
    pub formula: Option<Box<Formula>>,
    pub style: u32,
}

impl Cell {
    pub fn is_blank(&self) -> bool {
        self.value.is_empty() && self.formula.is_none() && self.style == 0
    }
    /// Text shown in the formula bar / when editing.
    pub fn input_text(&self) -> String {
        if let Some(f) = &self.formula {
            return format!("={}", f.text);
        }
        match &self.value {
            Value::Empty => String::new(),
            Value::Number(n) => crate::numfmt::edit_number(*n),
            Value::Text(s) => {
                // Text that would otherwise be read as a number/formula keeps its apostrophe.
                if s.starts_with('=') || crate::numfmt::parse_number(s).is_some() {
                    format!("'{s}")
                } else {
                    s.clone()
                }
            }
            Value::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
            Value::Error(e) => e.as_str().into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Sheets
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct SheetView {
    pub active: (u32, u32),
    pub anchor: (u32, u32),
    pub sel: Range,
    pub top: u32,
    pub left: u32,
    /// Additional areas of a Ctrl+click multi-area selection.
    pub extra: Vec<Range>,
}

pub use crate::filter::AutoFilter;

#[derive(Clone, Debug)]
pub struct Sheet {
    pub name: String,
    pub rows: BTreeMap<u32, BTreeMap<u32, Cell>>,
    pub col_w: HashMap<u32, f32>,
    pub row_h: HashMap<u32, f32>,
    pub hidden_rows: HashSet<u32>,
    pub hidden_cols: HashSet<u32>,
    pub merges: Vec<Range>,
    /// Frozen (rows, cols).
    pub freeze: (u32, u32),
    pub filter: Option<AutoFilter>,
    pub gridlines: bool,
    pub tab_color: Option<u32>,
    pub hidden: bool,
    /// Page setup used for printing.
    pub page: crate::page::PageSetup,
    pub view: SheetView,
    /// Cell notes (comments) keyed by (row, col).
    pub notes: HashMap<(u32, u32), String>,
    /// Hyperlinks: URL, or "#Sheet!A1" for a place in this workbook.
    pub links: HashMap<(u32, u32), String>,
    /// Outline (group) levels 1..=7 for rows and columns.
    pub row_levels: HashMap<u32, u8>,
    pub col_levels: HashMap<u32, u8>,
    pub charts: Vec<crate::chart::Chart>,
    /// Conditional formatting rules, highest priority first.
    pub cond_formats: Vec<crate::condfmt::CondFormat>,
    pub validations: Vec<crate::validation::Validation>,
    /// Dynamic-array spill overlay, rebuilt by recalculation.
    pub spill: Spill,
    /// Sheet-scoped defined names: (name, formula text without '=').
    pub names: Vec<(String, String)>,
    /// Excel Tables (ListObjects) on this sheet.
    pub tables: Vec<crate::table::Table>,
    /// Rich-text runs of text cells; used only while the cell still holds the same text.
    pub rich: HashMap<(u32, u32), RichText>,
    /// Sheet protection (None = unprotected).
    pub protection: Option<Protection>,
    /// Zoom factor of this sheet's view (1.0 = 100%).
    pub zoom: f32,
    /// Authors of cell notes (threaded comments are kept as notes with their author).
    pub note_authors: HashMap<(u32, u32), String>,
    /// Pictures floating over the cells.
    pub images: Vec<Picture>,
}

/// One run of a rich-text cell.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct RichRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// 0xRRGGBB, None = the cell's colour.
    pub color: Option<u32>,
    /// Size in points, None = the cell's size.
    pub size: Option<f32>,
    pub font: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct RichText {
    pub runs: Vec<RichRun>,
}

impl RichText {
    pub fn plain(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

/// Sheet protection settings (what users may still do, and the legacy password hash).
#[derive(Clone, Debug, PartialEq)]
pub struct Protection {
    /// Excel's legacy 16-bit password hash, 0 = no password.
    pub hash: u16,
    /// The iterated, salted password hash Excel 2013+ writes instead (kept as read).
    pub modern: Option<PasswordHash>,
    pub select_locked: bool,
    pub select_unlocked: bool,
    pub format_cells: bool,
    pub format_columns: bool,
    pub format_rows: bool,
    pub insert_columns: bool,
    pub insert_rows: bool,
    pub insert_links: bool,
    pub delete_columns: bool,
    pub delete_rows: bool,
    pub sort: bool,
    pub autofilter: bool,
    pub pivot_tables: bool,
    pub edit_objects: bool,
    pub edit_scenarios: bool,
}

impl Default for Protection {
    fn default() -> Self {
        Protection {
            hash: 0,
            modern: None,
            select_locked: true,
            select_unlocked: true,
            format_cells: false,
            format_columns: false,
            format_rows: false,
            insert_columns: false,
            insert_rows: false,
            insert_links: false,
            delete_columns: false,
            delete_rows: false,
            sort: false,
            autofilter: false,
            pivot_tables: false,
            edit_objects: false,
            edit_scenarios: false,
        }
    }
}

/// `algorithmName`, `hashValue`, `saltValue` and `spinCount` of `sheetProtection`.
#[derive(Clone, Debug, PartialEq)]
pub struct PasswordHash {
    pub algorithm: String,
    /// Base64 of the final hash.
    pub hash: String,
    /// Base64 of the salt.
    pub salt: String,
    pub spin: u32,
}

impl PasswordHash {
    /// Excel's check: H0 = SHA-512(salt + UTF-16LE password), then `spin` rounds of
    /// H = SHA-512(H + round number as 4 little-endian bytes). Only SHA-512 (what Excel
    /// writes) is supported; any other algorithm never matches, so the sheet stays protected.
    pub fn matches(&self, pw: &str) -> bool {
        if !self.algorithm.eq_ignore_ascii_case("SHA-512") {
            return false;
        }
        let (Some(salt), Some(want)) = (crate::hash::base64_decode(&self.salt), crate::hash::base64_decode(&self.hash)) else { return false };
        let mut data = salt;
        data.extend(pw.encode_utf16().flat_map(u16::to_le_bytes));
        let mut h = crate::hash::sha512(&data);
        let mut buf = [0u8; 68];
        for i in 0..self.spin {
            buf[..64].copy_from_slice(&h);
            buf[64..].copy_from_slice(&i.to_le_bytes());
            h = crate::hash::sha512(&buf);
        }
        h[..] == want[..]
    }
}

impl Protection {
    /// Whether `pw` unprotects the sheet (any password does when none was set).
    pub fn password_ok(&self, pw: &str) -> bool {
        match &self.modern {
            Some(m) => m.matches(pw),
            None => self.hash == 0 || legacy_password_hash(pw) == self.hash,
        }
    }
}

/// Excel's legacy sheet/workbook password hash (the `password` attribute of `sheetProtection`).
pub fn legacy_password_hash(pw: &str) -> u16 {
    if pw.is_empty() {
        return 0;
    }
    let bytes: Vec<u8> = pw.chars().map(|c| (c as u32 & 0xFF) as u8).collect();
    let mut h: u16 = 0;
    for &b in bytes.iter().rev() {
        h = ((h >> 14) & 1) | ((h << 1) & 0x7FFF);
        h ^= b as u16;
    }
    h = ((h >> 14) & 1) | ((h << 1) & 0x7FFF);
    h ^= bytes.len() as u16;
    h ^ 0xCE4B
}

/// A picture placed over the grid, anchored at a cell plus a pixel offset (at 100% zoom).
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    /// Encoded image bytes (PNG, JPEG, GIF, BMP).
    pub data: std::sync::Arc<Vec<u8>>,
    /// Lower-case file extension of the encoded data.
    pub ext: String,
    pub anchor: (u32, u32),
    pub offset: (f32, f32),
    /// Displayed size in pixels at 100% zoom.
    pub size: (f32, f32),
    pub name: String,
    pub descr: String,
}

impl Picture {
    /// Image format from the file signature.
    pub fn sniff(data: &[u8]) -> Option<&'static str> {
        if data.starts_with(&[0x89, b'P', b'N', b'G']) {
            Some("png")
        } else if data.starts_with(&[0xFF, 0xD8]) {
            Some("jpeg")
        } else if data.starts_with(b"GIF8") {
            Some("gif")
        } else if data.starts_with(b"BM") {
            Some("bmp")
        } else {
            None
        }
    }
}

/// Values spilled by dynamic-array formulas. Spilled cells are not stored as real cells.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spill {
    /// Spilled (non-anchor) cells: position -> (anchor, value).
    pub cells: HashMap<(u32, u32), ((u32, u32), Value)>,
    /// Anchor -> (intended spill range, blocked). Blocked anchors show #SPILL!.
    pub anchors: HashMap<(u32, u32), (Range, bool)>,
}

impl Sheet {
    pub fn new(name: &str) -> Self {
        Sheet {
            name: name.to_string(),
            rows: BTreeMap::new(),
            col_w: HashMap::new(),
            row_h: HashMap::new(),
            hidden_rows: HashSet::new(),
            hidden_cols: HashSet::new(),
            merges: Vec::new(),
            freeze: (0, 0),
            filter: None,
            gridlines: true,
            tab_color: None,
            hidden: false,
            page: crate::page::PageSetup::default(),
            view: SheetView::default(),
            notes: HashMap::new(),
            links: HashMap::new(),
            row_levels: HashMap::new(),
            col_levels: HashMap::new(),
            charts: Vec::new(),
            cond_formats: Vec::new(),
            validations: Vec::new(),
            spill: Spill::default(),
            names: Vec::new(),
            tables: Vec::new(),
            rich: HashMap::new(),
            protection: None,
            zoom: 1.0,
            note_authors: HashMap::new(),
            images: Vec::new(),
        }
    }

    /// Rich-text runs of a cell, if it still holds the text they were made for.
    pub fn rich_at(&self, r: u32, c: u32) -> Option<&RichText> {
        let rt = self.rich.get(&(r, c))?;
        match self.get(r, c) {
            Some(cell) if cell.formula.is_none() && matches!(&cell.value, Value::Text(t) if *t == rt.plain()) => Some(rt),
            _ => None,
        }
    }

    /// True when the sheet is protected and the cell is locked.
    #[cfg(test)]
    pub fn is_locked(&self, wb_styles: &Workbook, r: u32, c: u32) -> bool {
        self.protection.is_some() && !wb_styles.style(self.get(r, c).map_or(0, |x| x.style)).unlocked
    }

    pub fn get(&self, r: u32, c: u32) -> Option<&Cell> {
        self.rows.get(&r).and_then(|row| row.get(&c))
    }

    pub fn get_mut(&mut self, r: u32, c: u32) -> Option<&mut Cell> {
        self.rows.get_mut(&r).and_then(|row| row.get_mut(&c))
    }

    pub fn value(&self, r: u32, c: u32) -> &Value {
        static EMPTY: Value = Value::Empty;
        self.get(r, c).map(|c| &c.value).unwrap_or(&EMPTY)
    }

    pub fn cell_mut(&mut self, r: u32, c: u32) -> &mut Cell {
        self.rows.entry(r).or_default().entry(c).or_default()
    }

    /// Stores a cell, removing it when blank.
    pub fn set(&mut self, r: u32, c: u32, cell: Option<Cell>) {
        match cell {
            Some(cell) if !cell.is_blank() => {
                self.rows.entry(r).or_default().insert(c, cell);
            }
            _ => {
                self.remove(r, c);
            }
        }
    }

    pub fn remove(&mut self, r: u32, c: u32) -> Option<Cell> {
        let row = self.rows.get_mut(&r)?;
        let out = row.remove(&c);
        if row.is_empty() {
            self.rows.remove(&r);
        }
        out
    }

    /// Removes blank cells left behind by edits in the given row.
    pub fn tidy(&mut self, r: u32, c: u32) {
        if self.get(r, c).is_some_and(|c| c.is_blank()) {
            self.remove(r, c);
        }
    }

    /// (max_row, max_col) of populated cells.
    pub fn extent(&self) -> Option<(u32, u32)> {
        let max_r = *self.rows.keys().next_back()?;
        let max_c = self.rows.values().filter_map(|r| r.keys().next_back()).max().copied()?;
        Some((max_r, max_c))
    }

    pub fn used_range(&self) -> Option<Range> {
        let min_r = *self.rows.keys().next()?;
        let min_c = self.rows.values().filter_map(|r| r.keys().next()).min().copied()?;
        let (max_r, max_c) = self.extent()?;
        Some(Range::new(min_r, min_c, max_r, max_c))
    }

    pub fn raw_col_width(&self, c: u32) -> f32 {
        self.col_w.get(&c).copied().unwrap_or(DEFAULT_COL_W)
    }
    pub fn raw_row_height(&self, r: u32) -> f32 {
        self.row_h.get(&r).copied().unwrap_or(DEFAULT_ROW_H)
    }
    pub fn col_width(&self, c: u32) -> f32 {
        if self.hidden_cols.contains(&c) { 0.0 } else { self.raw_col_width(c) }
    }
    pub fn row_height(&self, r: u32) -> f32 {
        if self.hidden_rows.contains(&r) { 0.0 } else { self.raw_row_height(r) }
    }

    /// Maximal runs of consecutive indices whose outline level is at least `level`.
    pub fn outline_runs(levels: &HashMap<u32, u8>, level: u8) -> Vec<(u32, u32)> {
        let mut keys: Vec<u32> = levels.iter().filter(|(_, l)| **l >= level).map(|(k, _)| *k).collect();
        keys.sort_unstable();
        let mut out: Vec<(u32, u32)> = Vec::new();
        for k in keys {
            match out.last_mut() {
                Some((_, b)) if *b + 1 == k => *b = k,
                _ => out.push((k, k)),
            }
        }
        out
    }

    pub fn merge_at(&self, r: u32, c: u32) -> Option<Range> {
        self.merges.iter().copied().find(|m| m.contains(r, c))
    }

    /// Iterates populated cells inside a range in row-major order.
    pub fn iter_range(&self, rg: Range) -> impl Iterator<Item = (u32, u32, &Cell)> {
        self.rows
            .range(rg.r1..=rg.r2)
            .flat_map(move |(r, row)| row.range(rg.c1..=rg.c2).map(move |(c, cell)| (*r, *c, cell)))
    }

    pub fn is_populated(&self, r: u32, c: u32) -> bool {
        self.get(r, c).is_some_and(|c| !c.value.is_empty() || c.formula.is_some()) || self.spill.cells.contains_key(&(r, c))
    }

    /// The value shown in a cell: its own value, or the value spilled into it.
    pub fn display_value(&self, r: u32, c: u32) -> &Value {
        match self.get(r, c) {
            Some(cell) if !cell.value.is_empty() || cell.formula.is_some() => &cell.value,
            _ => self.spill.cells.get(&(r, c)).map(|(_, v)| v).unwrap_or(self.value(r, c)),
        }
    }

    /// Anchor of the dynamic array spilling into (r, c), if any.
    pub fn spill_anchor_of(&self, r: u32, c: u32) -> Option<(u32, u32)> {
        self.spill.cells.get(&(r, c)).map(|(a, _)| *a)
    }

    /// Spill range of an anchor cell (also returned for a blocked #SPILL! anchor).
    pub fn spill_range(&self, r: u32, c: u32) -> Option<(Range, bool)> {
        self.spill.anchors.get(&(r, c)).copied()
    }

    /// (max_row, max_col) of populated cells including spilled values.
    pub fn extent_with_spills(&self) -> Option<(u32, u32)> {
        let mut out = self.extent();
        for (rg, blocked) in self.spill.anchors.values() {
            if !blocked {
                let (r, c) = out.unwrap_or((0, 0));
                out = Some((r.max(rg.r2), c.max(rg.c2)));
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Workbook
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    pub styles: Vec<Style>,
    style_ix: HashMap<Style, u32>,
    /// Defined names: (name, formula text without '=').
    pub names: Vec<(String, String)>,
    pub active: usize,
    pub path: Option<PathBuf>,
    pub circular: bool,
    /// Dependency graph for incremental recalculation (rebuilt on demand).
    pub calc: formula::CalcState,
}

impl Default for Workbook {
    fn default() -> Self {
        Self::new()
    }
}

impl Workbook {
    pub fn new() -> Self {
        Self::empty_with(vec![Sheet::new("Sheet1")])
    }

    pub fn empty_with(sheets: Vec<Sheet>) -> Self {
        let mut wb = Workbook {
            sheets,
            styles: Vec::new(),
            style_ix: HashMap::new(),
            names: Vec::new(),
            active: 0,
            path: None,
            circular: false,
            calc: Default::default(),
        };
        wb.intern_style(Style::default());
        wb
    }

    pub fn intern_style(&mut self, s: Style) -> u32 {
        if let Some(&i) = self.style_ix.get(&s) {
            return i;
        }
        let i = self.styles.len() as u32;
        self.style_ix.insert(s.clone(), i);
        self.styles.push(s);
        i
    }

    pub fn style(&self, id: u32) -> &Style {
        self.styles.get(id as usize).unwrap_or(&self.styles[0])
    }

    pub fn sheet(&self) -> &Sheet {
        &self.sheets[self.active]
    }
    pub fn sheet_mut(&mut self) -> &mut Sheet {
        &mut self.sheets[self.active]
    }

    pub fn sheet_index(&self, name: &str) -> Option<usize> {
        self.sheets.iter().position(|s| s.name.eq_ignore_ascii_case(name))
    }

    pub fn unique_sheet_name(&self, base: &str) -> String {
        let mut i = self.sheets.len() + 1;
        loop {
            let n = format!("{base}{i}");
            if self.sheet_index(&n).is_none() {
                return n;
            }
            i += 1;
        }
    }

    pub fn recalc(&mut self) {
        formula::recalc(self);
    }

    pub fn file_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Book1".to_string())
    }
}

/// Builds a cell from what the user typed, interning any implied number format.
pub fn cell_from_input(wb: &mut Workbook, text: &str, base_style: u32) -> Option<Cell> {
    if text.is_empty() {
        return if base_style == 0 { None } else { Some(Cell { style: base_style, ..Default::default() }) };
    }
    if text.starts_with('=') && text.len() > 1 {
        let f = Formula::new(text);
        return Some(Cell { value: Value::Empty, formula: Some(Box::new(f)), style: base_style });
    }
    let (value, fmt) = crate::numfmt::parse_input(text);
    let mut style = base_style;
    if let Some(fmt) = fmt {
        let cur = wb.style(base_style);
        if cur.num_fmt.is_empty() {
            let mut s = cur.clone();
            s.num_fmt = fmt.to_string();
            style = wb.intern_style(s);
        }
    }
    Some(Cell { value, formula: None, style })
}
