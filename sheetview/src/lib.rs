//! Web Sheets, the data side: turns a workbook (a saved file version, plus the live log of a
//! co-editing session that has not been snapshotted yet) into the compact JSON the browser
//! viewer draws. The workbook is read and calculated by the app's own engine (`freedom_sheets`),
//! so what the browser shows is what FreedomSheets shows: displayed values with number formats,
//! styles, merges, widths and heights, frozen panes, hidden rows and columns, sheet tabs.
//!
//! Nothing here trusts the file: [`Book::open`] refuses archives that would expand beyond
//! [`MAX_UNPACKED_BYTES`] or hold absurdly many parts before the reader sees them, catches
//! panics, and refuses workbooks with more than [`MAX_CELLS`] cells. Time limits and the
//! number of conversions running at once are the caller's job (the server runs [`Book::open`]
//! on a thread of its own with a timeout).
//!
//! Large sheets are sent as windows (`Book::window`), never whole.

use std::collections::HashMap;
use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};

use cosync::Op;
use freedom_sheets::live;
use freedom_sheets::model::{BorderLine, HAlign, Range, Sheet, Style, VAlign, Value, Workbook};
use freedom_sheets::numfmt;
use serde_json::{json, Value as Json};

/// An archive that unpacks to more than this is refused.
pub const MAX_UNPACKED_BYTES: u64 = 120 * 1024 * 1024;
/// Parts in an archive.
pub const MAX_PARTS: usize = 4000;
/// Cells in a workbook (all sheets).
pub const MAX_CELLS: usize = 3_000_000;
/// Cells in one window.
pub const MAX_WINDOW_CELLS: u64 = 40_000;
/// Rows in one window.
pub const MAX_WINDOW_ROWS: u32 = 1000;
/// Displayed text longer than this is cut in a window (the whole text is in [`Book::cell`]).
pub const MAX_DISPLAY_CHARS: usize = 1000;
/// Entries of the lists in the sheet metadata (row heights, hidden rows...).
const MAX_LIST: usize = 100_000;
/// Charts, series and points per chart sent to the viewer.
const MAX_CHARTS: usize = 50;
const MAX_SERIES: usize = 20;
const MAX_POINTS: usize = 500;
/// Pictures described per sheet.
const MAX_IMAGES: usize = 200;

/// Why a file can't be shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// A format the viewer doesn't read (.xls, .ods...). The message says what to do.
    Unsupported(String),
    /// The file is damaged or isn't what its name says.
    Invalid(String),
    /// Too big or too complex to open in the browser.
    TooLarge(String),
}

impl LoadError {
    pub fn message(&self) -> &str {
        match self {
            LoadError::Unsupported(m) | LoadError::Invalid(m) | LoadError::TooLarge(m) => m,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            LoadError::Unsupported(_) => "unsupported",
            LoadError::Invalid(_) => "unreadable",
            LoadError::TooLarge(_) => "too_large",
        }
    }
}

/// The live part of a document's current state: what a joining client would start from.
#[derive(Default, Clone)]
pub struct LiveInput {
    /// The lossless live state stored with the saved version (`GET .../live/state`), if any.
    pub state: Option<Vec<u8>>,
    /// The sheets' live ids and order keys of the saved version ("id:pos,...").
    pub sheets: String,
    /// Committed batches after the saved version, in order.
    pub batches: Vec<Vec<Op>>,
}

/// A workbook ready to be shown.
pub struct Book {
    wb: Workbook,
    cells: usize,
    meta: Json,
}

fn extension(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// A sheet name made from a file name (for csv files): no characters Excel forbids, at most 31.
fn sheet_name_of(file: &str) -> String {
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    let clean: String = stem.chars().filter(|c| !matches!(c, '[' | ']' | ':' | '*' | '?' | '/' | '\\') && !c.is_control()).take(31).collect();
    let clean = clean.trim_matches('\'').trim().to_string();
    if clean.is_empty() { "Sheet1".into() } else { clean }
}

/// Looks at an archive's directory before the reader unpacks anything.
fn check_archive(bytes: &[u8]) -> Result<(), LoadError> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| LoadError::Invalid("This file isn't a valid .xlsx workbook (it isn't a zip archive).".into()))?;
    if z.len() > MAX_PARTS {
        return Err(LoadError::TooLarge("This workbook has too many parts to open in the browser.".into()));
    }
    let mut total: u64 = 0;
    for i in 0..z.len() {
        let f = z.by_index_raw(i).map_err(|_| LoadError::Invalid("This workbook's archive is damaged.".into()))?;
        total = total.saturating_add(f.size());
        if total > MAX_UNPACKED_BYTES {
            return Err(LoadError::TooLarge("This workbook is too large to open in the browser. Download it instead.".into()));
        }
    }
    Ok(())
}

impl Book {
    /// A book around a workbook that is already calculated (the live bridge keeps one).
    pub fn from_workbook(wb: Workbook) -> Book {
        let mut b = Book { wb, cells: 0, meta: Json::Null };
        b.refresh();
        b
    }

    /// Recounts the cells and rebuilds the sheet descriptions after the workbook changed.
    pub fn refresh(&mut self) {
        self.cells = self.wb.sheets.iter().map(|s| s.rows.values().map(|r| r.len()).sum::<usize>()).sum();
        let sheets: Vec<Json> = self.wb.sheets.iter().enumerate().map(|(i, s)| sheet_meta(&self.wb, i, s)).collect();
        self.meta = json!({ "sheets": sheets, "active": self.wb.active.min(self.wb.sheets.len().saturating_sub(1)) });
    }

    pub fn workbook_mut(&mut self) -> &mut Workbook {
        &mut self.wb
    }

    pub fn into_workbook(self) -> Workbook {
        self.wb
    }

    /// Reads `bytes` (a file called `name`) and brings it to its current state with `live`.
    pub fn open(name: &str, bytes: &[u8], live: &LiveInput) -> Result<Book, LoadError> {
        let ext = extension(name);
        match ext.as_str() {
            "xlsx" | "xlsm" | "xltx" | "xltm" | "csv" | "tsv" => {}
            "xls" | "xlsb" | "ods" | "xla" | "xlam" => {
                return Err(LoadError::Unsupported(format!("The browser can't open .{ext} files yet. Download it, or open it in FreedomSheets.")));
            }
            "fsheet" => return Err(LoadError::Unsupported("The browser can't open .fsheet files. Download it, or open it in FreedomSheets.".into())),
            _ => return Err(LoadError::Unsupported("This isn't a spreadsheet the browser can open. Download it instead.".into())),
        }
        let r = catch_unwind(AssertUnwindSafe(|| Book::build(name, &ext, bytes, live)));
        match r {
            Ok(r) => r,
            Err(_) => Err(LoadError::Invalid("This file couldn't be read (it is damaged or uses something the reader doesn't handle).".into())),
        }
    }

    fn build(name: &str, ext: &str, bytes: &[u8], live: &LiveInput) -> Result<Book, LoadError> {
        let mut wb = match ext {
            "csv" | "tsv" => freedom_sheets::io::load_delimited_bytes(bytes, &sheet_name_of(name), ext == "tsv"),
            _ => {
                check_archive(bytes)?;
                freedom_sheets::io::load_xlsx_bytes(bytes).map_err(|e| LoadError::Invalid(format!("This workbook couldn't be read: {}", short(&e))))?
            }
        };
        if wb.sheets.is_empty() {
            wb.sheets.push(Sheet::new("Sheet1"));
        }
        // As a joining client does it: the sheets' live ids, the lossless live state, the order.
        live::assign_ids(&mut wb, &live.sheets);
        if live.state.is_some() {
            wb = live::join_workbook(wb, live.state.as_deref());
        }
        live::sort_sheets(&mut wb);
        let cells: usize = wb.sheets.iter().map(|s| s.rows.values().map(|r| r.len()).sum::<usize>()).sum();
        if cells > MAX_CELLS {
            return Err(LoadError::TooLarge(format!("This workbook has {cells} cells; the browser opens up to {MAX_CELLS}. Download it instead.")));
        }
        let mut st = live::ApplyState::default();
        for b in &live.batches {
            live::apply_ops(&mut wb, b, true, &mut st);
        }
        wb.recalc();
        let sheets: Vec<Json> = wb.sheets.iter().enumerate().map(|(i, s)| sheet_meta(&wb, i, s)).collect();
        let meta = json!({ "sheets": sheets, "active": wb.active.min(wb.sheets.len().saturating_sub(1)) });
        Ok(Book { wb, cells, meta })
    }

    /// Number of non-empty cells.
    pub fn cell_count(&self) -> usize {
        self.cells
    }

    pub fn workbook(&self) -> &Workbook {
        &self.wb
    }

    /// The sheets and what the viewer needs to lay each one out (no cells).
    pub fn meta(&self) -> Json {
        self.meta.clone()
    }

    /// [`Book::window`] for a rectangle given as numbers (checked).
    pub fn window_at(&self, sheet: usize, r1: u32, c1: u32, r2: u32, c2: u32) -> Result<Json, String> {
        use freedom_sheets::model::{MAX_COLS, MAX_ROWS};
        if r1 > r2 || c1 > c2 || r2 >= MAX_ROWS || c2 >= MAX_COLS {
            return Err("Invalid window.".into());
        }
        self.window(sheet, Range::new(r1, c1, r2, c2))
    }

    /// The cells of a window of one sheet with their displayed text and styles.
    pub fn window(&self, sheet: usize, rg: Range) -> Result<Json, String> {
        let sh = self.wb.sheets.get(sheet).ok_or("No such sheet.")?;
        if rg.rows() > MAX_WINDOW_ROWS || u64::from(rg.rows()) * u64::from(rg.cols()) > MAX_WINDOW_CELLS {
            return Err(format!("A window is at most {MAX_WINDOW_ROWS} rows and {MAX_WINDOW_CELLS} cells."));
        }
        let mut styles = Styles::default();
        let mut cells: Vec<Json> = Vec::new();
        // Conditional formats of the sheet (statistics are gathered once per window).
        let cf = (!sh.cond_formats.is_empty()).then(|| freedom_sheets::condfmt::CfFrame::uncached(sh));
        let push = |r: u32, c: u32, cells: &mut Vec<Json>, styles: &mut Styles| {
            if let Some(cell) = self.shown(sh, sheet, r, c, styles, cf.as_ref()) {
                cells.push(cell);
            }
        };
        // Cells with something to show: content, spilled values, or a style.
        let mut seen: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
        for (r, c, _) in sh.iter_range(rg) {
            seen.insert((r, c));
            push(r, c, &mut cells, &mut styles);
        }
        if !sh.spill.cells.is_empty() {
            for &(r, c) in sh.spill.cells.keys() {
                if rg.contains(r, c) && seen.insert((r, c)) {
                    push(r, c, &mut cells, &mut styles);
                }
            }
        }
        // Cells with a note show a marker even when they are empty.
        for &(r, c) in sh.notes.keys() {
            if rg.contains(r, c) && seen.insert((r, c)) {
                push(r, c, &mut cells, &mut styles);
            }
        }
        // Merged areas touching the window: their anchor cell comes along even when it lies outside.
        let mut merges: Vec<Json> = Vec::new();
        for m in sh.merges.iter().filter(|m| m.intersects(&rg)) {
            merges.push(json!([m.r1, m.c1, m.r2, m.c2]));
            if !rg.contains(m.r1, m.c1) && seen.insert((m.r1, m.c1)) {
                push(m.r1, m.c1, &mut cells, &mut styles);
            }
            if merges.len() >= 2000 {
                break;
            }
        }
        Ok(json!({ "sheet": sheet, "r1": rg.r1, "r2": rg.r2, "c1": rg.c1, "c2": rg.c2, "styles": styles.list, "cells": cells, "merges": merges }))
    }

    /// One cell as the formula bar shows it: the entered text (formula, number as typed), the
    /// displayed text, and where it sits in a merged area. Formulas hidden by sheet protection
    /// are not given out.
    pub fn cell(&self, sheet: usize, r: u32, c: u32) -> Result<Json, String> {
        let sh = self.wb.sheets.get(sheet).ok_or("No such sheet.")?;
        if r >= freedom_sheets::model::MAX_ROWS || c >= freedom_sheets::model::MAX_COLS {
            return Err("No such cell.".into());
        }
        let (ar, ac) = sh.merge_at(r, c).map_or((r, c), |m| (m.r1, m.c1));
        let cell = sh.get(ar, ac);
        let st = self.wb.style(cell.map_or(0, |x| x.style));
        let value = sh.display_value(ar, ac);
        let shown = numfmt::format_value(value, &st.num_fmt).text;
        let hidden = cell.is_some_and(|x| x.formula.is_some()) && sh.protection.is_some() && st.hide_formula;
        let mut input = String::new();
        let mut formula = false;
        if !hidden && let Some(x) = cell {
            formula = x.formula.is_some();
            input = cut(&self.edit_text(x, st), 32_768);
        }
        let mut out = json!({ "r": ar, "c": ac, "input": input, "text": cut(&shown, 32_768), "formula": formula, "hidden_formula": hidden });
        if let Some(note) = sh.notes.get(&(ar, ac)) {
            out["note"] = json!(cut(note, 4000));
        }
        if let Some(link) = sh.links.get(&(ar, ac)) {
            out["link"] = json!(cut(link, 2000));
        }
        Ok(out)
    }

    /// What the formula bar shows (dates as dates).
    fn edit_text(&self, cell: &freedom_sheets::model::Cell, st: &Style) -> String {
        if cell.formula.is_none()
            && let Value::Number(n) = cell.value
            && numfmt::is_date_format(&st.num_fmt)
            && n >= 0.0
        {
            return numfmt::edit_date(n);
        }
        cell.input_text()
    }

    /// The cell at (r, c) as `[r, c, text, kind, style, link, extra]` (kind: n number, s text, b
    /// bool, e error; link 0/1; `extra` only when present: `{bar: [from, to, colour, gradient],
    /// note: 1}`), or None when there is nothing to show. Conditional formats are part of the style.
    fn shown(&self, sh: &Sheet, sheet: usize, r: u32, c: u32, styles: &mut Styles, cf: Option<&freedom_sheets::condfmt::CfFrame>) -> Option<Json> {
        let cell = sh.get(r, c);
        let value = sh.display_value(r, c);
        let style_id = cell.map_or(0, |x| x.style);
        let note = sh.notes.contains_key(&(r, c));
        if value.is_empty() && style_id == 0 && !note {
            return None;
        }
        let st = self.wb.style(style_id);
        let f = numfmt::format_value(value, &st.num_fmt);
        let kind = match value {
            Value::Number(_) => "n",
            Value::Text(_) => "s",
            Value::Bool(_) => "b",
            Value::Error(_) => "e",
            Value::Empty => "",
        };
        let ov = cf.and_then(|cf| cf.eval(&self.wb, sheet, r, c));
        let mut si = 0usize;
        if style_id != 0 || f.color.is_some() || ov.is_some() {
            si = match (&ov, f.color) {
                (None, None) => styles.plain(style_id, || style_json(st)),
                _ => {
                    let mut j = style_json(st);
                    if let Some(col) = f.color {
                        j["c"] = json!(hex(col));
                    }
                    if let Some(o) = &ov {
                        if let Some(x) = o.fill {
                            j["f"] = json!(hex(x));
                        }
                        if let Some(x) = o.color {
                            j["c"] = json!(hex(x));
                        }
                        for (on, k) in [(o.bold, "b"), (o.italic, "i"), (o.underline, "u"), (o.strike, "x")] {
                            if on {
                                j[k] = json!(1);
                            }
                        }
                    }
                    styles.by_json(j)
                }
            };
        }
        let mut text = f.text;
        if let Some((cut_at, _)) = text.char_indices().nth(MAX_DISPLAY_CHARS) {
            text.truncate(cut_at);
        }
        let mut out = json!([r, c, text, kind, si]);
        let mut extra = serde_json::Map::new();
        if let Some(b) = ov.as_ref().and_then(|o| o.bar) {
            extra.insert("bar".into(), json!([b.start, b.end, hex(b.color), u8::from(b.gradient)]));
        }
        if note {
            extra.insert("note".into(), json!(1));
        }
        let link = sh.links.contains_key(&(r, c));
        if link || !extra.is_empty() {
            out.as_array_mut().expect("array").push(json!(u8::from(link)));
        }
        if !extra.is_empty() {
            out.as_array_mut().expect("array").push(Json::Object(extra));
        }
        Some(out)
    }

    /// The n-th picture of a sheet: (extension, bytes), only for formats a browser shows.
    pub fn image(&self, sheet: usize, n: usize) -> Option<(String, std::sync::Arc<Vec<u8>>)> {
        let p = self.wb.sheets.get(sheet)?.images.get(n)?;
        let ext = match freedom_sheets::model::Picture::sniff(&p.data)? {
            "jpeg" => "jpeg",
            "png" => "png",
            "gif" => "gif",
            "bmp" => "bmp",
            _ => return None,
        };
        Some((ext.to_string(), p.data.clone()))
    }

    /// The charts of a sheet with the data they show (resolved by the app's own engine), for the
    /// viewer to draw.
    pub fn charts(&self, sheet: usize) -> Result<Json, String> {
        use freedom_sheets::chart::{self, ChartKind, Grouping};
        let sh = self.wb.sheets.get(sheet).ok_or("No such sheet.")?;
        let mut out = Vec::new();
        for ch in sh.charts.iter().take(MAX_CHARTS) {
            let data = chart::resolve(&self.wb, ch);
            let kind = match ch.kind {
                ChartKind::Column => "column",
                ChartKind::Bar => "bar",
                ChartKind::Line => "line",
                ChartKind::Pie => "pie",
                ChartKind::Doughnut => "doughnut",
                ChartKind::Area => "area",
                ChartKind::Scatter => "scatter",
                ChartKind::Radar => "radar",
            };
            let grouping = match ch.grouping {
                Grouping::Standard => "standard",
                Grouping::Stacked => "stacked",
                Grouping::Percent => "percent",
            };
            let series: Vec<Json> = data
                .series
                .iter()
                .enumerate()
                .take(MAX_SERIES)
                .map(|(i, s)| {
                    let colors: Vec<String> = if ch.kind.is_round() { (0..s.values.len().min(MAX_POINTS)).map(|k| hex(ch.color(k))).collect() } else { Vec::new() };
                    json!({
                        "name": cut(&s.name, 60),
                        "color": hex(ch.series_fill(i)),
                        "line": hex(ch.series_line(i)),
                        "values": s.values.iter().take(MAX_POINTS).collect::<Vec<_>>(),
                        "xs": s.xs.as_ref().map(|x| x.iter().take(MAX_POINTS).collect::<Vec<_>>()),
                        "colors": colors,
                    })
                })
                .collect();
            let title = chart::display_title(ch, &data).map(|t| cut(&t, 120));
            out.push(json!({
                "kind": kind, "grouping": grouping, "title": title, "legend": ch.legend,
                "r": ch.anchor.0, "c": ch.anchor.1, "dx": ch.offset.0, "dy": ch.offset.1, "w": ch.size.0, "h": ch.size.1,
                "cats": data.categories.iter().take(MAX_POINTS).map(|c| cut(c, 40)).collect::<Vec<_>>(),
                "xs": data.cat_values.iter().take(MAX_POINTS).collect::<Vec<_>>(),
                "series": series,
            }));
        }
        Ok(json!(out))
    }

    /// The whole sheet as CSV text (displayed values), for the download button.
    pub fn csv(&self, sheet: usize) -> Result<String, String> {
        let sh = self.wb.sheets.get(sheet).ok_or("No such sheet.")?;
        let mut out = String::new();
        let Some((mr, mc)) = sh.extent_with_spills() else { return Ok(out) };
        for r in 0..=mr.min(1_048_575) {
            let line: Vec<String> = (0..=mc)
                .map(|c| {
                    let v = sh.display_value(r, c);
                    let st = self.wb.style(sh.get(r, c).map_or(0, |x| x.style));
                    csv_field(&numfmt::format_value(v, &st.num_fmt).text)
                })
                .collect();
            out.push_str(&line.join(","));
            out.push('\n');
        }
        Ok(out)
    }
}

/// The styles of one window: distinct style objects, referred to by 1-based index.
#[derive(Default)]
struct Styles {
    list: Vec<Json>,
    ids: HashMap<u32, usize>,
    jsons: HashMap<String, usize>,
}

impl Styles {
    fn plain(&mut self, id: u32, make: impl FnOnce() -> Json) -> usize {
        if let Some(i) = self.ids.get(&id) {
            return *i;
        }
        self.list.push(make());
        self.ids.insert(id, self.list.len());
        self.list.len()
    }

    fn by_json(&mut self, j: Json) -> usize {
        let key = j.to_string();
        if let Some(i) = self.jsons.get(&key) {
            return *i;
        }
        self.list.push(j);
        self.jsons.insert(key, self.list.len());
        self.list.len()
    }
}

/// A CSV field; text that a spreadsheet would run as a formula gets a leading apostrophe.
fn csv_field(s: &str) -> String {
    let danger = matches!(s.chars().next(), Some('=' | '+' | '-' | '@' | '\t' | '\r'));
    let s = if danger && s.parse::<f64>().is_err() { format!("'{s}") } else { s.to_string() };
    if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s }
}

fn short(e: &str) -> String {
    cut(e, 200)
}

fn cut(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

fn hex(c: u32) -> String {
    format!("#{:06x}", c & 0xff_ffff)
}

fn line_code(l: BorderLine) -> u8 {
    match l {
        BorderLine::None => 0,
        BorderLine::Thin => 1,
        BorderLine::Medium => 2,
        BorderLine::Thick => 3,
        BorderLine::Double => 4,
        BorderLine::Dashed => 5,
        BorderLine::Dotted => 6,
    }
}

/// A style as the viewer reads it; only what differs from the default is present.
/// `b` bold, `i` italic, `u` underline, `x` strike, `c` text colour, `f` fill, `h` l/c/r,
/// `v` t/c, `w` wrap, `z` size in points, `n` indent, `d` borders [left,right,top,bottom]
/// (0 none, 1 thin, 2 medium, 3 thick, 4 double, 5 dashed, 6 dotted).
fn style_json(st: &Style) -> Json {
    let mut j = serde_json::Map::new();
    if st.bold {
        j.insert("b".into(), json!(1));
    }
    if st.italic {
        j.insert("i".into(), json!(1));
    }
    if st.underline {
        j.insert("u".into(), json!(1));
    }
    if st.strike {
        j.insert("x".into(), json!(1));
    }
    if let Some(c) = st.color {
        j.insert("c".into(), json!(hex(c)));
    }
    if let Some(c) = st.fill {
        j.insert("f".into(), json!(hex(c)));
    }
    match st.halign {
        HAlign::General => {}
        HAlign::Left => {
            j.insert("h".into(), json!("l"));
        }
        HAlign::Center => {
            j.insert("h".into(), json!("c"));
        }
        HAlign::Right => {
            j.insert("h".into(), json!("r"));
        }
    }
    match st.valign {
        VAlign::Bottom => {}
        VAlign::Top => {
            j.insert("v".into(), json!("t"));
        }
        VAlign::Center => {
            j.insert("v".into(), json!("c"));
        }
    }
    if st.wrap {
        j.insert("w".into(), json!(1));
    }
    if st.size10 != 0 && st.size10 != 110 {
        j.insert("z".into(), json!(f64::from(st.size10) / 10.0));
    }
    if st.indent > 0 {
        j.insert("n".into(), json!(st.indent));
    }
    let b = &st.border;
    let d = [line_code(b.left), line_code(b.right), line_code(b.top), line_code(b.bottom)];
    if d.iter().any(|x| *x != 0) {
        j.insert("d".into(), json!(d));
    }
    Json::Object(j)
}

/// Runs of consecutive indices as [first, last] pairs.
fn runs(mut v: Vec<u32>) -> Vec<Json> {
    v.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for k in v {
        match out.last_mut() {
            Some((_, b)) if *b + 1 == k => *b = k,
            _ => out.push((k, k)),
        }
    }
    out.into_iter().take(MAX_LIST).map(|(a, b)| json!([a, b])).collect()
}

/// Rows whose height Excel would grow to fit larger text (no custom height set): (row, px).
fn auto_heights(wb: &Workbook, s: &Sheet) -> Vec<(u32, f32)> {
    let mut out = Vec::new();
    for (r, row) in &s.rows {
        if s.row_h.contains_key(r) {
            continue;
        }
        let biggest = row.values().map(|c| wb.style(c.style).size10).max().unwrap_or(0);
        if biggest > 110 {
            // 11 pt is the 20 px default row.
            out.push((*r, (f32::from(biggest) / 10.0 * 20.0 / 11.0).ceil()));
        }
        if out.len() >= MAX_LIST {
            break;
        }
    }
    out
}

fn sheet_meta(wb: &Workbook, i: usize, s: &Sheet) -> Json {
    let (mut rows, mut cols) = s.extent_with_spills().map_or((0, 0), |(r, c)| (r + 1, c + 1));
    for m in &s.merges {
        rows = rows.max(m.r2 + 1);
        cols = cols.max(m.c2 + 1);
    }
    let mut col_w: Vec<(u32, f32)> = s.col_w.iter().map(|(c, w)| (*c, *w)).collect();
    col_w.sort_unstable_by_key(|x| x.0);
    let mut row_h: Vec<(u32, f32)> = s.row_h.iter().map(|(r, h)| (*r, *h)).collect();
    row_h.extend(auto_heights(wb, s));
    row_h.sort_unstable_by_key(|x| x.0);
    let merges: Vec<Json> = s.merges.iter().take(20_000).map(|m| json!([m.r1, m.c1, m.r2, m.c2])).collect();
    json!({
        "i": i,
        "uid": s.uid.to_string(),
        "name": s.name,
        "hidden": s.hidden,
        "tab": s.tab_color.map(hex),
        "rows": rows,
        "cols": cols,
        "freeze": [s.freeze.0, s.freeze.1],
        "col_w": col_w.into_iter().take(MAX_LIST).map(|(c, w)| json!([c, w])).collect::<Vec<_>>(),
        "row_h": row_h.into_iter().take(MAX_LIST).map(|(r, h)| json!([r, h])).collect::<Vec<_>>(),
        "default_col_w": freedom_sheets::model::DEFAULT_COL_W,
        "default_row_h": freedom_sheets::model::DEFAULT_ROW_H,
        "hidden_rows": runs(s.hidden_rows.iter().copied().collect()),
        "hidden_cols": runs(s.hidden_cols.iter().copied().collect()),
        "merges": merges,
        "gridlines": s.gridlines,
        "protected": s.protection.is_some(),
        "charts": s.charts.len(),
        "images": s.images.iter().take(MAX_IMAGES).enumerate().map(|(n, p)| json!({ "n": n, "r": p.anchor.0, "c": p.anchor.1, "dx": p.offset.0, "dy": p.offset.1, "w": p.size.0, "h": p.size.1, "name": cut(&p.name, 80), "descr": cut(&p.descr, 200) })).collect::<Vec<_>>(),
    })
}

pub mod bridge;

#[cfg(test)]
mod tests;
