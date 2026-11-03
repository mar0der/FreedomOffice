//! Charts: model, series detection from a cell range, axis scaling and
//! keeping chart sources in step with sheet edits.

use crate::model::{Range, Sheet, Value, Workbook, DEFAULT_COL_W, DEFAULT_ROW_H, MAX_COLS, MAX_ROWS};
use crate::numfmt;

/// Default chart size in pixels (5 x 3 inches at 96 dpi).
pub const DEFAULT_W: f32 = 480.0;
pub const DEFAULT_H: f32 = 288.0;
pub const MIN_SIZE: f32 = 40.0;

/// Default office series palette.
pub const PALETTE: [u32; 8] = [0x4472C4, 0xED7D31, 0xA5A5A5, 0xFFC000, 0x5B9BD5, 0x70AD47, 0x264478, 0x9E480E];

/// Points drawn per series are capped to keep painting cheap on huge ranges.
pub const MAX_POINTS: usize = 4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartKind {
    Column,
    Bar,
    Line,
    Pie,
    Doughnut,
    Area,
    Scatter,
}

impl ChartKind {
    pub const ALL: [ChartKind; 7] =
        [ChartKind::Column, ChartKind::Line, ChartKind::Pie, ChartKind::Bar, ChartKind::Area, ChartKind::Scatter, ChartKind::Doughnut];

    pub fn label(self) -> &'static str {
        match self {
            ChartKind::Column => "Column",
            ChartKind::Bar => "Bar",
            ChartKind::Line => "Line",
            ChartKind::Pie => "Pie",
            ChartKind::Doughnut => "Doughnut",
            ChartKind::Area => "Area",
            ChartKind::Scatter => "Scatter",
        }
    }

    pub fn is_round(self) -> bool {
        matches!(self, ChartKind::Pie | ChartKind::Doughnut)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chart {
    pub kind: ChartKind,
    /// Name of the sheet holding the data.
    pub sheet: String,
    pub range: Range,
    /// Series run down columns (true) or across rows (false).
    pub by_cols: bool,
    /// Overrides for "first line holds series names" / "first line holds categories";
    /// `None` = detect from the cell contents.
    pub names_first: Option<bool>,
    pub cats_first: Option<bool>,
    /// `None` = automatic title, `Some("")` = no title.
    pub title: Option<String>,
    pub legend: bool,
    pub data_labels: bool,
    /// Top-left cell (row, col) the chart is anchored to.
    pub anchor: (u32, u32),
    /// Offset from the anchor cell's top-left corner, in unzoomed pixels.
    pub offset: (f32, f32),
    /// Width and height in unzoomed pixels.
    pub size: (f32, f32),
}

impl Chart {
    /// A chart over `range` of sheet `si`, with the defaults the desktop spreadsheet picks.
    pub fn new(wb: &Workbook, si: usize, range: Range, kind: ChartKind) -> Chart {
        let sh = &wb.sheets[si];
        let range = clip_to_data(sh, range);
        let mut ch = Chart {
            kind,
            sheet: sh.name.clone(),
            range,
            by_cols: range.rows() > range.cols(),
            names_first: None,
            cats_first: None,
            title: None,
            legend: false,
            data_labels: false,
            anchor: (range.r1, (range.c2 + 2).min(MAX_COLS - 1)),
            offset: (0.0, 0.0),
            size: (DEFAULT_W, DEFAULT_H),
        };
        ch.legend = kind.is_round() || resolve(wb, &ch).series.len() > 1;
        ch
    }

    /// Switch Row/Column: series become categories and vice versa.
    pub fn switch_row_col(&mut self) {
        self.by_cols = !self.by_cols;
        std::mem::swap(&mut self.names_first, &mut self.cats_first);
    }
}

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct SeriesData {
    pub name: String,
    /// Cell holding the series name, if any.
    pub name_cell: Option<(u32, u32)>,
    pub values: Vec<Option<f64>>,
    pub refr: Range,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ChartData {
    pub categories: Vec<String>,
    /// Numeric category values (X values for scatter charts).
    pub cat_values: Vec<Option<f64>>,
    pub cat_ref: Option<Range>,
    pub series: Vec<SeriesData>,
    /// Number format of the first value cell, for axis and data labels.
    pub num_fmt: String,
}

impl ChartData {
    pub fn points(&self) -> usize {
        self.series.iter().map(|s| s.values.len()).max().unwrap_or(0).max(self.categories.len())
    }
}

/// Limits whole-row / whole-column selections to the populated part of the sheet.
pub fn clip_to_data(sh: &Sheet, rg: Range) -> Range {
    let (er, ec) = sh.extent().unwrap_or((0, 0));
    let r2 = rg.r2.min(er.max(rg.r1));
    let c2 = rg.c2.min(ec.max(rg.c1));
    Range::new(rg.r1, rg.c1, r2, c2)
}

fn is_num(v: &Value) -> bool {
    matches!(v, Value::Number(_))
}

/// Whether the first row / first column of the range hold labels, following the
/// familiar rules: a blank top-left cell means both; otherwise a line made only of
/// text (no numbers) is a label line.
pub fn detect_labels(sh: &Sheet, rg: Range) -> (bool, bool) {
    if rg.rows() > 1 && rg.cols() > 1 && sh.value(rg.r1, rg.c1).is_empty() {
        return (true, true);
    }
    let row = rg.rows() > 1 && {
        let from = if rg.cols() > 1 { rg.c1 + 1 } else { rg.c1 };
        (from..=rg.c2).all(|c| !is_num(sh.value(rg.r1, c)))
            && (rg.c1..=rg.c2).any(|c| matches!(sh.value(rg.r1, c), Value::Text(_)))
    };
    let col = rg.cols() > 1 && {
        (rg.r1 + 1..=rg.r2).all(|r| !is_num(sh.value(r, rg.c1)))
            && (rg.r1..=rg.r2).any(|r| matches!(sh.value(r, rg.c1), Value::Text(_)))
    };
    (row, col)
}

fn display(wb: &Workbook, sh: &Sheet, r: u32, c: u32) -> String {
    match sh.get(r, c) {
        Some(cell) => numfmt::format_value(&cell.value, &wb.style(cell.style).num_fmt).text,
        None => String::new(),
    }
}

/// Reads the series, names and categories of a chart from its source cells.
pub fn resolve(wb: &Workbook, ch: &Chart) -> ChartData {
    let Some(si) = wb.sheet_index(&ch.sheet) else { return ChartData::default() };
    let sh = &wb.sheets[si];
    let rg = clip_to_data(sh, ch.range);
    let (label_row, label_col) = detect_labels(sh, rg);
    let (mut names, mut cats) = if ch.by_cols { (label_row, label_col) } else { (label_col, label_row) };
    if let Some(n) = ch.names_first {
        names = n;
    }
    if let Some(c) = ch.cats_first {
        cats = c;
    }
    let (lines, points) = if ch.by_cols { (rg.cols(), rg.rows()) } else { (rg.rows(), rg.cols()) };
    // Scatter: with two or more columns the first holds the X values.
    if ch.kind == ChartKind::Scatter && ch.cats_first.is_none() && lines >= 2 {
        cats = true;
    }
    if cats && lines < 2 {
        cats = false;
    }
    if names && points < 2 {
        names = false;
    }
    let at = |line: u32, point: u32| -> (u32, u32) {
        if ch.by_cols { (rg.r1 + point, rg.c1 + line) } else { (rg.r1 + line, rg.c1 + point) }
    };
    let span = |line: u32, p1: u32, p2: u32| -> Range {
        let (a, b) = (at(line, p1), at(line, p2));
        Range::new(a.0, a.1, b.0, b.1)
    };
    let point0 = u32::from(names);
    let line0 = u32::from(cats);
    let npoints = points.saturating_sub(point0).min(MAX_POINTS as u32);
    let mut data = ChartData::default();
    if npoints == 0 {
        return data;
    }
    let last = point0 + npoints - 1;
    if cats {
        data.cat_ref = Some(span(0, point0, last));
        for p in point0..=last {
            let (r, c) = at(0, p);
            data.categories.push(display(wb, sh, r, c));
            data.cat_values.push(sh.value(r, c).as_number());
        }
    } else {
        data.categories = (1..=npoints).map(|i| i.to_string()).collect();
        data.cat_values = (1..=npoints).map(|i| Some(i as f64)).collect();
    }
    for l in line0..lines {
        let name_cell = names.then(|| at(l, 0));
        let name = match name_cell {
            Some((r, c)) => display(wb, sh, r, c),
            None => format!("Series{}", l - line0 + 1),
        };
        let values = (point0..=last).map(|p| {
            let (r, c) = at(l, p);
            sh.value(r, c).as_number()
        });
        data.series.push(SeriesData { name, name_cell, values: values.collect(), refr: span(l, point0, last) });
    }
    if let Some(s) = data.series.first() {
        if let Some(cell) = sh.get(s.refr.r1, s.refr.c1) {
            data.num_fmt = wb.style(cell.style).num_fmt.clone();
        }
    }
    data
}

/// Title shown on the chart, `None` when hidden.
pub fn display_title(ch: &Chart, data: &ChartData) -> Option<String> {
    match &ch.title {
        Some(t) if t.is_empty() => None,
        Some(t) => Some(t.clone()),
        None => match data.series.as_slice() {
            [s] if s.name_cell.is_some() => Some(s.name.clone()),
            _ => Some("Chart Title".into()),
        },
    }
}

// ---------------------------------------------------------------------------
// Axis scaling
// ---------------------------------------------------------------------------

/// "Nice" value axis: (min, max, major step). Starts at zero for positive data unless
/// the values sit far from it, and leaves a little headroom above the largest value.
pub fn nice_axis(min: f64, max: f64) -> (f64, f64, f64) {
    let (mut min, mut max) = if min.is_finite() && max.is_finite() { (min.min(max), min.max(max)) } else { (0.0, 1.0) };
    if min == max {
        if min == 0.0 {
            max = 1.0;
        } else if min > 0.0 {
            min = 0.0;
        } else {
            max = 0.0;
        }
    }
    if min > 0.0 && (max - min) / max > 1.0 / 6.0 {
        min = 0.0;
    }
    if max < 0.0 && (min - max) / min > 1.0 / 6.0 {
        max = 0.0;
    }
    let raw = (max - min) / 8.0;
    let mag = 10f64.powf(raw.log10().floor());
    let norm = raw / mag;
    let step = mag * if norm <= 1.0 { 1.0 } else if norm <= 2.0 { 2.0 } else if norm <= 5.0 { 5.0 } else { 10.0 };
    let pad = (max - min) * 0.05;
    let lo = if min == 0.0 { 0.0 } else { ((min - if min < 0.0 { pad } else { 0.0 }) / step).floor() * step };
    let hi = if max == 0.0 { 0.0 } else { ((max + if max > 0.0 { pad } else { 0.0 }) / step).ceil() * step };
    (lo, hi.max(lo + step), step)
}

/// Text for an axis tick or data label.
pub fn format_number(v: f64, fmt: &str, step: f64) -> String {
    let general = fmt.trim().is_empty() || fmt.eq_ignore_ascii_case("general");
    if !general {
        return numfmt::format_value(&Value::Number(v), fmt).text;
    }
    let decimals = if step > 0.0 && step < 1.0 { (-step.log10().floor()) as i32 + 1 } else { 0 };
    let p = 10f64.powi(decimals.clamp(0, 10));
    let v = if step > 0.0 { (v * p).round() / p } else { v };
    numfmt::general(if v == 0.0 { 0.0 } else { v })
}

// ---------------------------------------------------------------------------
// Sheet geometry (unzoomed pixels)
// ---------------------------------------------------------------------------

/// Left edge of column `c`, measured from column A.
pub fn col_x(sh: &Sheet, c: u32) -> f32 {
    let mut x = c as f32 * DEFAULT_COL_W;
    for (&k, &w) in &sh.col_w {
        if k < c {
            x += w - DEFAULT_COL_W;
        }
    }
    for &k in &sh.hidden_cols {
        if k < c {
            x -= sh.raw_col_width(k);
        }
    }
    x
}

/// Top edge of row `r`, measured from row 1.
pub fn row_y(sh: &Sheet, r: u32) -> f32 {
    let mut y = r as f32 * DEFAULT_ROW_H;
    for (&k, &h) in &sh.row_h {
        if k < r {
            y += h - DEFAULT_ROW_H;
        }
    }
    for &k in &sh.hidden_rows {
        if k < r {
            y -= sh.raw_row_height(k);
        }
    }
    y
}

/// Column containing sheet x, and the offset into it.
pub fn col_at_x(sh: &Sheet, x: f32) -> (u32, f32) {
    let mut left = 0.0;
    let mut c = 0;
    while c < MAX_COLS - 1 {
        let w = sh.col_width(c);
        if x < left + w {
            break;
        }
        left += w;
        c += 1;
    }
    (c, (x - left).max(0.0))
}

/// Row containing sheet y, and the offset into it.
pub fn row_at_y(sh: &Sheet, y: f32) -> (u32, f32) {
    let mut top = 0.0;
    let mut r = 0;
    while r < MAX_ROWS - 1 {
        let h = sh.row_height(r);
        if y < top + h {
            break;
        }
        top += h;
        r += 1;
    }
    (r, (y - top).max(0.0))
}

/// Chart rectangle in sheet pixels: (x, y, w, h).
pub fn sheet_rect(sh: &Sheet, ch: &Chart) -> (f32, f32, f32, f32) {
    (col_x(sh, ch.anchor.1) + ch.offset.0, row_y(sh, ch.anchor.0) + ch.offset.1, ch.size.0, ch.size.1)
}

/// Places a chart at the given sheet-pixel rectangle.
pub fn set_sheet_rect(sh: &Sheet, ch: &mut Chart, x: f32, y: f32, w: f32, h: f32) {
    let (c, ox) = col_at_x(sh, x.max(0.0));
    let (r, oy) = row_at_y(sh, y.max(0.0));
    ch.anchor = (r, c);
    ch.offset = (ox, oy);
    ch.size = (w.max(MIN_SIZE), h.max(MIN_SIZE));
}

// ---------------------------------------------------------------------------
// Keeping charts in step with sheet edits
// ---------------------------------------------------------------------------

/// Adjusts chart sources and anchors after `n` rows/columns were inserted (n > 0)
/// or deleted (n < 0) at `at` on sheet `si`.
pub fn shift_for_structure(wb: &mut Workbook, si: usize, rows: bool, at: u32, n: i64) {
    let name = wb.sheets[si].name.clone();
    let max = if rows { MAX_ROWS } else { MAX_COLS } as i64;
    let map = |x: u32, end: bool| -> Option<u32> {
        let x = x as i64;
        let at = at as i64;
        let y = if n > 0 {
            if x >= at { x + n } else { x }
        } else if x >= at - n {
            x + n
        } else if x >= at {
            // inside the deleted band: start edges snap to `at`, end edges to just before it
            if end { at - 1 } else { at }
        } else {
            x
        };
        (y >= 0).then_some(y.min(max - 1) as u32)
    };
    for (i, sh) in wb.sheets.iter_mut().enumerate() {
        for ch in &mut sh.charts {
            if ch.sheet.eq_ignore_ascii_case(&name) {
                let rg = ch.range;
                let (a, b) = if rows { (rg.r1, rg.r2) } else { (rg.c1, rg.c2) };
                if let (Some(na), Some(nb)) = (map(a, false), map(b, true)) {
                    if nb >= na {
                        ch.range = if rows { Range { r1: na, r2: nb, ..rg } } else { Range { c1: na, c2: nb, ..rg } };
                    }
                }
            }
            if i == si {
                let a = if rows { ch.anchor.0 } else { ch.anchor.1 };
                let na = map(a, false).unwrap_or(0);
                if rows {
                    ch.anchor.0 = na;
                } else {
                    ch.anchor.1 = na;
                }
            }
        }
    }
}

/// Points charts at a renamed sheet.
pub fn rename_sheet(wb: &mut Workbook, old: &str, new: &str) {
    for sh in &mut wb.sheets {
        for ch in &mut sh.charts {
            if ch.sheet.eq_ignore_ascii_case(old) {
                ch.sheet = new.to_string();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Cell;

    fn put(sh: &mut Sheet, r: u32, c: u32, v: Value) {
        sh.set(r, c, Some(Cell { value: v, ..Default::default() }));
    }

    fn sample() -> Workbook {
        // Month | Sales | Cost
        let mut sh = Sheet::new("Data");
        put(&mut sh, 0, 0, Value::Text("Month".into()));
        put(&mut sh, 0, 1, Value::Text("Sales".into()));
        put(&mut sh, 0, 2, Value::Text("Cost".into()));
        for (i, m) in ["Jan", "Feb", "Mar", "Apr"].iter().enumerate() {
            let r = i as u32 + 1;
            put(&mut sh, r, 0, Value::Text(m.to_string()));
            put(&mut sh, r, 1, Value::Number(10.0 * r as f64));
            put(&mut sh, r, 2, Value::Number(5.0 * r as f64));
        }
        Workbook::empty_with(vec![sh])
    }

    #[test]
    fn nice_axis_steps() {
        assert_eq!(nice_axis(0.0, 100.0), (0.0, 120.0, 20.0));
        assert_eq!(nice_axis(3.0, 37.0), (0.0, 40.0, 5.0));
        let (lo, _, step) = nice_axis(0.0, 0.0);
        assert_eq!((lo, step), (0.0, 0.2));
        let (lo, hi, step) = nice_axis(-12.0, 30.0);
        assert!(lo <= -12.0 && hi >= 30.0 && lo < 0.0);
        assert_eq!(step, 10.0);
        // Values far from zero do not force a zero baseline.
        let (lo, hi, _) = nice_axis(1000.0, 1010.0);
        assert!(lo > 900.0 && lo <= 1000.0 && hi >= 1010.0);
        // Small fractions.
        let (lo, hi, step) = nice_axis(0.0, 0.7);
        assert_eq!((lo, step), (0.0, 0.1));
        assert!((hi - 0.8).abs() < 1e-9);
        assert_eq!(format_number(0.30000000000000004, "", 0.1), "0.3");
    }

    #[test]
    fn series_in_columns_with_labels() {
        let wb = sample();
        let ch = Chart::new(&wb, 0, Range::new(0, 0, 4, 2), ChartKind::Column);
        assert!(ch.by_cols);
        assert!(ch.legend);
        let d = resolve(&wb, &ch);
        assert_eq!(d.categories, vec!["Jan", "Feb", "Mar", "Apr"]);
        assert_eq!(d.cat_ref, Some(Range::new(1, 0, 4, 0)));
        assert_eq!(d.series.len(), 2);
        assert_eq!(d.series[0].name, "Sales");
        assert_eq!(d.series[0].name_cell, Some((0, 1)));
        assert_eq!(d.series[0].refr, Range::new(1, 1, 4, 1));
        assert_eq!(d.series[1].values, vec![Some(5.0), Some(10.0), Some(15.0), Some(20.0)]);
        assert_eq!(display_title(&ch, &d).as_deref(), Some("Chart Title"));
    }

    #[test]
    fn series_in_rows_and_switch() {
        let wb = sample();
        let mut ch = Chart::new(&wb, 0, Range::new(0, 0, 4, 2), ChartKind::Line);
        ch.switch_row_col();
        let d = resolve(&wb, &ch);
        assert_eq!(d.series.len(), 4);
        assert_eq!(d.series[0].name, "Jan");
        assert_eq!(d.categories, vec!["Sales", "Cost"]);
        assert_eq!(d.series[3].values, vec![Some(40.0), Some(20.0)]);
        // Wide range: series run across rows automatically.
        let ch = Chart::new(&wb, 0, Range::new(0, 0, 1, 2), ChartKind::Column);
        assert!(!ch.by_cols);
    }

    #[test]
    fn single_series_title_and_plain_numbers() {
        let wb = sample();
        let ch = Chart::new(&wb, 0, Range::new(0, 1, 4, 1), ChartKind::Pie);
        let d = resolve(&wb, &ch);
        assert_eq!(d.series.len(), 1);
        assert_eq!(d.categories, vec!["1", "2", "3", "4"]);
        assert_eq!(display_title(&ch, &d).as_deref(), Some("Sales"));
        // Numbers only: no labels at all.
        let ch = Chart::new(&wb, 0, Range::new(1, 1, 4, 2), ChartKind::Column);
        let d = resolve(&wb, &ch);
        assert_eq!(d.series.len(), 2);
        assert_eq!(d.series[0].name, "Series1");
        assert!(d.cat_ref.is_none());
        // Scatter takes the first column as X values.
        let ch = Chart::new(&wb, 0, Range::new(1, 1, 4, 2), ChartKind::Scatter);
        let d = resolve(&wb, &ch);
        assert_eq!(d.series.len(), 1);
        assert_eq!(d.cat_values, vec![Some(10.0), Some(20.0), Some(30.0), Some(40.0)]);
    }

    #[test]
    fn structure_changes_move_sources() {
        let mut wb = sample();
        let ch = Chart::new(&wb, 0, Range::new(0, 0, 4, 2), ChartKind::Column);
        wb.sheets[0].charts.push(ch);
        shift_for_structure(&mut wb, 0, true, 0, 2);
        assert_eq!(wb.sheets[0].charts[0].range, Range::new(2, 0, 6, 2));
        assert_eq!(wb.sheets[0].charts[0].anchor.0, 2);
        shift_for_structure(&mut wb, 0, false, 1, -1);
        assert_eq!(wb.sheets[0].charts[0].range, Range::new(2, 0, 6, 1));
        rename_sheet(&mut wb, "data", "Numbers");
        assert_eq!(wb.sheets[0].charts[0].sheet, "Numbers");
    }

    #[test]
    fn geometry_round_trip() {
        let mut sh = Sheet::new("S");
        sh.col_w.insert(1, 100.0);
        sh.hidden_cols.insert(3);
        assert_eq!(col_x(&sh, 2), 164.0);
        assert_eq!(col_x(&sh, 5), 164.0 + 64.0 + 64.0);
        assert_eq!(col_at_x(&sh, 170.0), (2, 6.0));
        assert_eq!(col_at_x(&sh, 230.0), (4, 2.0));
        assert_eq!(row_at_y(&sh, 45.0), (2, 5.0));
    }

    #[test]
    fn xlsx_round_trip() {
        let mut wb = sample();
        let mut ch = Chart::new(&wb, 0, Range::new(0, 0, 4, 2), ChartKind::Bar);
        ch.title = Some("Sales vs Cost".into());
        ch.data_labels = true;
        ch.offset = (10.0, 4.0);
        wb.sheets[0].charts.push(ch.clone());
        let mut pie = Chart::new(&wb, 0, Range::new(0, 1, 4, 1), ChartKind::Pie);
        pie.anchor = (10, 1);
        pie.legend = false;
        wb.sheets[0].charts.push(pie.clone());
        let mut line = Chart::new(&wb, 0, Range::new(0, 0, 4, 2), ChartKind::Line);
        line.switch_row_col();
        line.title = Some(String::new());
        wb.sheets[0].charts.push(line.clone());

        let dir = std::env::temp_dir().join(format!("freedom_sheets_chart_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("chart.xlsx");
        crate::io::save(&wb, &path).unwrap();
        let back = crate::io::load(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let charts = &back.sheets[0].charts;
        assert_eq!(charts.len(), 3);
        let find = |k: ChartKind| charts.iter().find(|c| c.kind == k).unwrap();

        let a = find(ChartKind::Bar);
        assert_eq!(a.kind, ChartKind::Bar);
        assert_eq!(a.sheet, "Data");
        assert_eq!(a.range, ch.range);
        assert!(a.by_cols);
        assert_eq!(a.title.as_deref(), Some("Sales vs Cost"));
        assert!(a.legend && a.data_labels);
        assert_eq!(a.anchor, ch.anchor);
        assert!((a.offset.0 - 10.0).abs() < 1.5 && (a.offset.1 - 4.0).abs() < 1.5);
        assert!((a.size.0 - DEFAULT_W).abs() < 2.0 && (a.size.1 - DEFAULT_H).abs() < 2.0);
        assert_eq!(resolve(&back, a), resolve(&wb, &ch));

        let b = find(ChartKind::Pie);
        assert_eq!(b.kind, ChartKind::Pie);
        assert_eq!(b.range, pie.range);
        assert_eq!(b.title, None);
        assert!(!b.legend);
        assert_eq!(b.anchor, (10, 1));

        let c = find(ChartKind::Line);
        assert_eq!(c.kind, ChartKind::Line);
        assert!(!c.by_cols);
        assert_eq!(c.title.as_deref(), Some(""));
        assert_eq!(resolve(&back, c), resolve(&wb, &line));
    }
}
