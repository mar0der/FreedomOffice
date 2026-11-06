//! Sparklines: small line, column and win/loss charts drawn inside cells.

use crate::model::{Range, Sheet, Value, Workbook};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SparkKind {
    #[default]
    Line,
    Column,
    WinLoss,
}

impl SparkKind {
    pub fn from_xml(s: &str) -> SparkKind {
        match s {
            "column" => SparkKind::Column,
            "stacked" => SparkKind::WinLoss,
            _ => SparkKind::Line,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SparkKind::Line => "Line",
            SparkKind::Column => "Column",
            SparkKind::WinLoss => "Win/Loss",
        }
    }
}

/// How empty cells are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum EmptyAs {
    #[default]
    Gap,
    Zero,
    /// Connect data points with a line.
    Span,
}

impl EmptyAs {
    pub fn from_xml(s: &str) -> EmptyAs {
        match s {
            "zero" => EmptyAs::Zero,
            "span" => EmptyAs::Span,
            _ => EmptyAs::Gap,
        }
    }
}

/// Vertical axis minimum / maximum option.
#[derive(Clone, Copy, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub enum AxisScale {
    /// Automatic for each sparkline.
    #[default]
    Individual,
    /// Same for all sparklines of the group.
    Group,
    Custom(f64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SparkColors {
    pub series: u32,
    pub negative: u32,
    pub axis: u32,
    pub markers: u32,
    pub first: u32,
    pub last: u32,
    pub high: u32,
    pub low: u32,
}

/// Office theme accents 1-6.
pub const ACCENTS: [u32; 6] = [0x4472C4, 0xED7D31, 0xA5A5A5, 0xFFC000, 0x5B9BD5, 0x70AD47];

impl Default for SparkColors {
    /// Excel's default: Accent 1, Darker 50%, with an Accent 2 negative colour.
    fn default() -> Self {
        SparkColors::styled(0, 0)
    }
}

impl SparkColors {
    /// The Style gallery: `accent` 0-5, `variant` 0 (darker 50%), 1 (darker 25%), 2 (accent),
    /// 3 (lighter 40%).
    pub fn styled(accent: usize, variant: usize) -> SparkColors {
        let a = ACCENTS[accent % 6];
        let tint = |t: f64| crate::io::tint(a, t);
        let series = match variant {
            0 => tint(-0.499984740745262),
            1 => tint(-0.249977111117893),
            2 => a,
            _ => tint(0.39997558519241921),
        };
        let other = ACCENTS[(accent + 1) % 6];
        SparkColors {
            series,
            negative: other,
            axis: 0x000000,
            markers: series,
            first: tint(0.39997558519241921),
            last: tint(0.39997558519241921),
            high: a,
            low: a,
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sparkline {
    /// Cell the sparkline is drawn in.
    pub loc: (u32, u32),
    /// Data range text, sheet-qualified (e.g. `Sheet1!A2:E2`).
    pub data: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SparklineGroup {
    pub kind: SparkKind,
    pub items: Vec<Sparkline>,
    pub colors: SparkColors,
    pub high: bool,
    pub low: bool,
    pub first: bool,
    pub last: bool,
    pub negative: bool,
    pub markers: bool,
    /// Show the horizontal axis.
    pub axis: bool,
    pub empty_as: EmptyAs,
    /// Show data in hidden rows and columns.
    pub hidden: bool,
    pub rtl: bool,
    /// Line weight in points.
    pub line_weight: f64,
    pub min: AxisScale,
    pub max: AxisScale,
    /// Date axis range (sheet-qualified), if any.
    pub date_range: Option<String>,
}

impl SparklineGroup {
    pub fn new(kind: SparkKind, items: Vec<Sparkline>) -> Self {
        SparklineGroup {
            kind,
            items,
            colors: SparkColors::default(),
            high: false,
            low: false,
            first: false,
            last: false,
            negative: false,
            markers: false,
            axis: false,
            empty_as: EmptyAs::Gap,
            hidden: false,
            rtl: false,
            line_weight: 0.75,
            min: AxisScale::Individual,
            max: AxisScale::Individual,
            date_range: None,
        }
    }

    /// Moves locations and same-sheet data ranges after a row/column insert or delete.
    pub fn shift(&mut self, sheet_name: &str, rows: bool, at: u32, n: i64) {
        let shift_text = |text: &str| -> Option<String> {
            let (sheet, rg) = split_ref(text)?;
            if !sheet.eq_ignore_ascii_case(sheet_name) {
                return Some(text.to_string());
            }
            let rg = crate::condfmt::shift_range(rg, rows, at, n)?;
            Some(qualified(&sheet, rg))
        };
        self.items = self
            .items
            .iter()
            .filter_map(|it| {
                let loc = crate::condfmt::shift_range(Range::cell(it.loc.0, it.loc.1), rows, at, n)?;
                Some(Sparkline { loc: (loc.r1, loc.c1), data: shift_text(&it.data)? })
            })
            .collect();
        if let Some(d) = &self.date_range {
            self.date_range = shift_text(d);
        }
    }
}

/// `Sheet1!A1:E1` / `'My Sheet'!$A$1:$E$1` -> (sheet name, range).
pub fn split_ref(text: &str) -> Option<(String, Range)> {
    let t = text.trim().trim_start_matches('=');
    let (s, r) = t.rsplit_once('!')?;
    let s = s.trim();
    let s = if s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2 { s[1..s.len() - 1].replace("''", "'") } else { s.to_string() };
    Some((s, Range::parse(r)?))
}

/// The text form of a sheet-qualified range.
pub fn qualified(sheet: &str, rg: Range) -> String {
    format!("{}!{}", crate::formula::quote_sheet(sheet), rg.to_a1())
}

/// The sparkline drawn in a cell: (group index, item index).
pub fn at(sheet: &Sheet, r: u32, c: u32) -> Option<(usize, usize)> {
    sheet.sparklines.iter().enumerate().find_map(|(g, grp)| grp.items.iter().position(|it| it.loc == (r, c)).map(|i| (g, i)))
}

/// Data values of a sparkline in plotting order; None marks an empty (or non-numeric) point.
pub fn values(wb: &Workbook, grp: &SparklineGroup, it: &Sparkline) -> Vec<Option<f64>> {
    let Some((name, rg)) = split_ref(&it.data) else { return Vec::new() };
    let Some(sh) = wb.sheet_index(&name).map(|i| &wb.sheets[i]) else { return Vec::new() };
    let horizontal = rg.rows() == 1 && rg.cols() > 1;
    let n = if horizontal { rg.cols() } else { rg.rows() * rg.cols() };
    let mut out = Vec::with_capacity(n.min(10_000) as usize);
    for k in 0..n.min(10_000) {
        let (r, c) = if horizontal { (rg.r1, rg.c1 + k) } else if rg.cols() == 1 { (rg.r1 + k, rg.c1) } else { (rg.r1 + k / rg.cols(), rg.c1 + k % rg.cols()) };
        if !grp.hidden && (sh.hidden_rows.contains(&r) || sh.hidden_cols.contains(&c)) {
            continue;
        }
        out.push(match sh.display_value(r, c) {
            Value::Number(x) if x.is_finite() => Some(*x),
            Value::Bool(b) => Some(*b as u8 as f64),
            _ => {
                if grp.empty_as == EmptyAs::Zero {
                    Some(0.0)
                } else {
                    None
                }
            }
        });
    }
    if grp.rtl {
        out.reverse();
    }
    out
}

/// Vertical extent (min, max) of one sparkline, honouring the group's axis options.
pub fn extent(wb: &Workbook, grp: &SparklineGroup, vals: &[Option<f64>]) -> (f64, f64) {
    let own = |v: &[Option<f64>]| -> (f64, f64) {
        let it = v.iter().flatten();
        let lo = it.clone().fold(f64::INFINITY, |a, b| a.min(*b));
        let hi = it.fold(f64::NEG_INFINITY, |a, b| a.max(*b));
        (lo, hi)
    };
    let group = || -> (f64, f64) {
        grp.items.iter().map(|it| own(&values(wb, grp, it))).fold((f64::INFINITY, f64::NEG_INFINITY), |a, b| (a.0.min(b.0), a.1.max(b.1)))
    };
    let (mut lo, mut hi) = own(vals);
    let need_group = matches!(grp.min, AxisScale::Group) || matches!(grp.max, AxisScale::Group);
    let g = if need_group { group() } else { (lo, hi) };
    match grp.min {
        AxisScale::Group => lo = g.0,
        AxisScale::Custom(v) => lo = v,
        AxisScale::Individual => {}
    }
    match grp.max {
        AxisScale::Group => hi = g.1,
        AxisScale::Custom(v) => hi = v,
        AxisScale::Individual => {}
    }
    (lo, hi)
}

/// Builds the sparklines for Insert > Sparklines: one per location cell, each plotting a row or
/// column of `data`. Returns an error message like Excel's when the shapes don't fit.
pub fn layout(sheet_name: &str, data: Range, loc: Range) -> Result<Vec<Sparkline>, String> {
    if loc.rows() > 1 && loc.cols() > 1 {
        return Err("Location reference is not valid because the cells are not all in the same column or same row. Select cells that are all in a single row or single column.".into());
    }
    let n = loc.rows().max(loc.cols());
    let cells: Vec<(u32, u32)> = (0..n).map(|k| if loc.cols() == 1 { (loc.r1 + k, loc.c1) } else { (loc.r1, loc.c1 + k) }).collect();
    if n == 1 {
        if data.rows() > 1 && data.cols() > 1 {
            return Err("Data Range is not valid. The range must be a single row or column for a single location cell.".into());
        }
        return Ok(vec![Sparkline { loc: cells[0], data: qualified(sheet_name, data) }]);
    }
    // Several locations: one row (or column) of data per location; a vertical location
    // prefers rows, a horizontal one prefers columns.
    let by_rows = if loc.cols() == 1 { data.rows() == n } else { data.cols() != n && data.rows() == n };
    if !by_rows && data.cols() != n {
        return Err("The reference for the location or data range is not valid.".into());
    }
    Ok(cells
        .into_iter()
        .enumerate()
        .map(|(k, p)| {
            let k = k as u32;
            let rg = if by_rows { Range::new(data.r1 + k, data.c1, data.r1 + k, data.c2) } else { Range::new(data.r1, data.c1 + k, data.r2, data.c1 + k) };
            Sparkline { loc: p, data: qualified(sheet_name, rg) }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_rows_and_columns() {
        let data = Range::parse("A2:E4").unwrap();
        let v = layout("Sheet1", data, Range::parse("F2:F4").unwrap()).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v[1], Sparkline { loc: (2, 5), data: "Sheet1!A3:E3".into() });
        let v = layout("My Data", Range::parse("A1:C5").unwrap(), Range::parse("A7:C7").unwrap()).unwrap();
        assert_eq!(v[2].data, "'My Data'!C1:C5");
        assert!(layout("S", data, Range::parse("F2:G4").unwrap()).is_err());
        assert!(layout("S", data, Range::parse("F2").unwrap()).is_err());
        assert_eq!(split_ref("'My Data'!$C$1:$C$5"), Some(("My Data".into(), Range::parse("C1:C5").unwrap())));
        let mut g = SparklineGroup::new(SparkKind::Line, v);
        g.shift("My Data", true, 0, 2);
        assert_eq!(g.items[0].loc, (8, 0));
        assert_eq!(g.items[0].data, "'My Data'!A3:A7");
    }
}
