//! Per-sheet page setup: paper, orientation, margins, scaling, print area, titles,
//! headers/footers and manual page breaks (the "Page Setup" settings).

use std::collections::BTreeSet;

use crate::model::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Paper {
    Letter,
    Legal,
    Executive,
    Tabloid,
    A3,
    A4,
    A5,
    B5,
}

impl Paper {
    pub const ALL: [Paper; 8] = [Paper::Letter, Paper::Legal, Paper::Executive, Paper::Tabloid, Paper::A3, Paper::A4, Paper::A5, Paper::B5];

    /// Portrait size in points (1/72 inch).
    pub fn size_pt(self) -> (f32, f32) {
        match self {
            Paper::Letter => (612.0, 792.0),
            Paper::Legal => (612.0, 1008.0),
            Paper::Executive => (522.0, 756.0),
            Paper::Tabloid => (792.0, 1224.0),
            Paper::A3 => (841.89, 1190.55),
            Paper::A4 => (595.28, 841.89),
            Paper::A5 => (419.53, 595.28),
            Paper::B5 => (515.91, 728.5),
        }
    }

    /// Paper code used by the xlsx `paperSize` attribute and the Windows DEVMODE (DMPAPER_*).
    pub fn code(self) -> u16 {
        match self {
            Paper::Letter => 1,
            Paper::Tabloid => 3,
            Paper::Legal => 5,
            Paper::Executive => 7,
            Paper::A3 => 8,
            Paper::A4 => 9,
            Paper::A5 => 11,
            Paper::B5 => 13,
        }
    }

    pub fn from_code(c: u16) -> Option<Paper> {
        Paper::ALL.iter().copied().find(|p| p.code() == c)
    }

    pub fn name(self) -> &'static str {
        match self {
            Paper::Letter => "Letter",
            Paper::Legal => "Legal",
            Paper::Executive => "Executive",
            Paper::Tabloid => "Tabloid",
            Paper::A3 => "A3",
            Paper::A4 => "A4",
            Paper::A5 => "A5",
            Paper::B5 => "B5 (JIS)",
        }
    }

    /// Size caption like the print settings list: "21 cm x 29.7 cm" or "8.5" x 11"".
    pub fn caption(self, metric: bool) -> String {
        let (w, h) = self.size_pt();
        if metric {
            let cm = |p: f32| {
                let v = (p / 72.0 * 2.54 * 100.0).round() / 100.0;
                let s = format!("{v:.2}");
                s.trim_end_matches('0').trim_end_matches('.').to_string()
            };
            format!("{} cm x {} cm", cm(w), cm(h))
        } else {
            let inch = |p: f32| {
                let v = (p / 72.0 * 100.0).round() / 100.0;
                let s = format!("{v:.2}");
                s.trim_end_matches('0').trim_end_matches('.').to_string()
            };
            format!("{}\" x {}\"", inch(w), inch(h))
        }
    }
}

/// Margins in inches.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Margins {
    pub top: f32,
    pub bottom: f32,
    pub left: f32,
    pub right: f32,
    pub header: f32,
    pub footer: f32,
}

impl Margins {
    pub const NORMAL: Margins = Margins { top: 0.75, bottom: 0.75, left: 0.7, right: 0.7, header: 0.3, footer: 0.3 };
    pub const WIDE: Margins = Margins { top: 1.0, bottom: 1.0, left: 1.0, right: 1.0, header: 0.5, footer: 0.5 };
    pub const NARROW: Margins = Margins { top: 0.75, bottom: 0.75, left: 0.25, right: 0.25, header: 0.3, footer: 0.3 };

    pub fn preset(&self) -> Option<&'static str> {
        let near = |a: &Margins, b: &Margins| {
            [(a.top, b.top), (a.bottom, b.bottom), (a.left, b.left), (a.right, b.right), (a.header, b.header), (a.footer, b.footer)]
                .iter()
                .all(|(x, y)| (x - y).abs() < 0.005)
        };
        if near(self, &Margins::NORMAL) {
            Some("Normal")
        } else if near(self, &Margins::WIDE) {
            Some("Wide")
        } else if near(self, &Margins::NARROW) {
            Some("Narrow")
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Scaling {
    /// Adjust to N% of normal size (10..=400).
    Percent(u16),
    /// Fit to N pages wide by M tall; 0 = automatic.
    Fit { wide: u16, tall: u16 },
}

/// Header or footer text in three sections, using the familiar codes
/// (&P page, &N pages, &D date, &T time, &A sheet, &F file, &Z path, && ampersand).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HeaderFooter {
    pub left: String,
    pub center: String,
    pub right: String,
}

impl HeaderFooter {
    pub fn is_empty(&self) -> bool {
        self.left.is_empty() && self.center.is_empty() && self.right.is_empty()
    }

    pub fn center(s: &str) -> Self {
        HeaderFooter { center: s.to_string(), ..Default::default() }
    }

    /// The xlsx form: "&LLeft&CCenter&RRight".
    pub fn to_xlsx(&self) -> String {
        let mut s = String::new();
        if !self.left.is_empty() {
            s.push_str("&L");
            s.push_str(&self.left);
        }
        if !self.center.is_empty() {
            s.push_str("&C");
            s.push_str(&self.center);
        }
        if !self.right.is_empty() {
            s.push_str("&R");
            s.push_str(&self.right);
        }
        s
    }

    pub fn from_xlsx(s: &str) -> Self {
        let mut hf = HeaderFooter::default();
        let mut section = 1; // text before any &L/&C/&R goes to the center
        let mut cur = String::new();
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        let flush = |hf: &mut HeaderFooter, section: i32, cur: &mut String| {
            let t = std::mem::take(cur);
            match section {
                0 => hf.left.push_str(&t),
                2 => hf.right.push_str(&t),
                _ => hf.center.push_str(&t),
            }
        };
        while i < chars.len() {
            if chars[i] == '&' && i + 1 < chars.len() {
                match chars[i + 1] {
                    'L' | 'C' | 'R' => {
                        flush(&mut hf, section, &mut cur);
                        section = match chars[i + 1] {
                            'L' => 0,
                            'C' => 1,
                            _ => 2,
                        };
                        i += 2;
                        continue;
                    }
                    _ => {
                        cur.push('&');
                        cur.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                }
            }
            cur.push(chars[i]);
            i += 1;
        }
        flush(&mut hf, section, &mut cur);
        hf
    }
}

pub struct HfContext<'a> {
    pub page: u32,
    pub pages: u32,
    pub sheet: &'a str,
    pub file: &'a str,
    pub path: &'a str,
}

/// Replaces header/footer codes with their values and drops formatting codes.
pub fn render_hf(src: &str, ctx: &HfContext) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '&' || i + 1 >= chars.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let n = chars[i + 1];
        i += 2;
        match n {
            '&' => out.push('&'),
            'P' => {
                // &P+1 / &P-1 offsets
                let mut v = ctx.page as i64;
                if i < chars.len() && (chars[i] == '+' || chars[i] == '-') {
                    let sign = if chars[i] == '+' { 1 } else { -1 };
                    let mut j = i + 1;
                    let mut num = 0i64;
                    while j < chars.len() && chars[j].is_ascii_digit() {
                        num = num * 10 + chars[j].to_digit(10).unwrap() as i64;
                        j += 1;
                    }
                    if j > i + 1 {
                        v += sign * num;
                        i = j;
                    }
                }
                out.push_str(&v.to_string());
            }
            'N' => out.push_str(&ctx.pages.to_string()),
            'D' => out.push_str(&chrono::Local::now().format("%-m/%-d/%Y").to_string()),
            'T' => out.push_str(&chrono::Local::now().format("%-I:%M %p").to_string()),
            'A' => out.push_str(ctx.sheet),
            'F' => out.push_str(ctx.file),
            'Z' => out.push_str(ctx.path),
            '"' => {
                // &"Font,Style" - skip to the closing quote
                while i < chars.len() && chars[i] != '"' {
                    i += 1;
                }
                i += 1;
            }
            d if d.is_ascii_digit() => {
                // &12 font size
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            'K' => {
                // &Krrggbb colour
                i = (i + 6).min(chars.len());
            }
            'B' | 'I' | 'U' | 'E' | 'S' | 'X' | 'Y' | 'O' | 'H' | 'G' => {}
            other => {
                out.push('&');
                out.push(other);
            }
        }
    }
    out
}

/// Built-in header/footer choices shown in the Page Setup dialog.
pub fn hf_presets() -> Vec<(&'static str, HeaderFooter)> {
    vec![
        ("(none)", HeaderFooter::default()),
        ("Page 1", HeaderFooter::center("Page &P")),
        ("Page 1 of ?", HeaderFooter::center("Page &P of &N")),
        ("Sheet1", HeaderFooter::center("&A")),
        ("Book1", HeaderFooter::center("&F")),
        ("Sheet1, Page 1", HeaderFooter { left: String::new(), center: "&A".into(), right: "Page &P".into() }),
        ("Book1, Page 1", HeaderFooter { left: String::new(), center: "&F".into(), right: "Page &P".into() }),
        ("Prepared by, date, Page 1", HeaderFooter { left: String::new(), center: "&D".into(), right: "Page &P".into() }),
        ("Full path", HeaderFooter::center("&Z&F")),
    ]
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PageSetup {
    pub landscape: bool,
    pub paper: Paper,
    pub margins: Margins,
    pub scaling: Scaling,
    pub center_h: bool,
    pub center_v: bool,
    pub gridlines: bool,
    pub headings: bool,
    pub black_white: bool,
    pub print_area: Option<Range>,
    /// Rows / columns repeated on every page (inclusive, 0-based).
    pub title_rows: Option<(u32, u32)>,
    pub title_cols: Option<(u32, u32)>,
    pub header: HeaderFooter,
    pub footer: HeaderFooter,
    pub first_page: Option<u32>,
    /// Page order: false = down, then over (default); true = over, then down.
    pub over_then_down: bool,
    /// Manual page breaks: a break *before* each listed row / column.
    pub row_breaks: BTreeSet<u32>,
    pub col_breaks: BTreeSet<u32>,
}

impl Default for PageSetup {
    fn default() -> Self {
        PageSetup {
            landscape: false,
            paper: default_paper(),
            margins: Margins::NORMAL,
            scaling: Scaling::Percent(100),
            center_h: false,
            center_v: false,
            gridlines: false,
            headings: false,
            black_white: false,
            print_area: None,
            title_rows: None,
            title_cols: None,
            header: HeaderFooter::default(),
            footer: HeaderFooter::default(),
            first_page: None,
            over_then_down: false,
            row_breaks: BTreeSet::new(),
            col_breaks: BTreeSet::new(),
        }
    }
}

impl PageSetup {
    /// Paper size in points for the current orientation.
    pub fn paper_pt(&self) -> (f32, f32) {
        let (w, h) = self.paper.size_pt();
        if self.landscape { (h, w) } else { (w, h) }
    }

    /// Adjusts print area, titles and breaks after rows/columns are inserted (n > 0) or deleted (n < 0).
    pub fn shift(&mut self, rows: bool, at: u32, n: i64) {
        if let Some(a) = self.print_area {
            self.print_area = crate::condfmt::shift_range(a, rows, at, n);
        }
        let shift_span = |span: Option<(u32, u32)>| -> Option<(u32, u32)> {
            let (a, b) = span?;
            let rg = if rows { Range::new(a, 0, b, 0) } else { Range::new(0, a, 0, b) };
            let s = crate::condfmt::shift_range(rg, rows, at, n)?;
            Some(if rows { (s.r1, s.r2) } else { (s.c1, s.c2) })
        };
        if rows {
            self.title_rows = shift_span(self.title_rows);
        } else {
            self.title_cols = shift_span(self.title_cols);
        }
        let breaks = if rows { &mut self.row_breaks } else { &mut self.col_breaks };
        let moved: BTreeSet<u32> = breaks
            .iter()
            .filter_map(|&b| {
                let b = b as i64;
                let at = at as i64;
                if n < 0 && b >= at && b < at - n {
                    None
                } else if b >= at {
                    u32::try_from(b + n).ok()
                } else {
                    Some(b as u32)
                }
            })
            .collect();
        *breaks = moved;
    }
}

/// US Letter where the user's locale measures in inches, A4 elsewhere.
pub fn default_paper() -> Paper {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Globalization::{GetLocaleInfoEx, LOCALE_IMEASURE};
        let mut buf = [0u16; 4];
        // LOCALE_NAME_USER_DEFAULT = null
        let n = unsafe { GetLocaleInfoEx(std::ptr::null(), LOCALE_IMEASURE, buf.as_mut_ptr(), buf.len() as i32) };
        if n > 0 && buf[0] == '1' as u16 {
            return Paper::Letter;
        }
        Paper::A4
    }
    #[cfg(not(windows))]
    {
        Paper::A4
    }
}

/// Whether the user's locale is metric (for paper size captions).
pub fn metric_locale() -> bool {
    default_paper() == Paper::A4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_footer_codes() {
        let hf = HeaderFooter::from_xlsx("&LLeft&CPage &P of &N&R&\"Arial,Bold\"&12&A");
        assert_eq!(hf.left, "Left");
        assert_eq!(hf.center, "Page &P of &N");
        let ctx = HfContext { page: 2, pages: 5, sheet: "Data", file: "Book.xlsx", path: "C:\\" };
        assert_eq!(render_hf(&hf.center, &ctx), "Page 2 of 5");
        assert_eq!(render_hf(&hf.right, &ctx), "Data");
        assert_eq!(HeaderFooter::from_xlsx(&hf.to_xlsx()), hf);
        assert_eq!(render_hf("A && B &P+1", &ctx), "A & B 3");
    }

    #[test]
    fn margins_presets_and_captions() {
        assert_eq!(Margins::NORMAL.preset(), Some("Normal"));
        assert_eq!(Paper::A4.caption(true), "21 cm x 29.7 cm");
        assert_eq!(Paper::Letter.caption(false), "8.5\" x 11\"");
    }

    #[test]
    fn breaks_follow_inserted_rows() {
        let mut p = PageSetup::default();
        p.row_breaks.insert(10);
        p.title_rows = Some((0, 0));
        p.shift(true, 5, 2);
        assert!(p.row_breaks.contains(&12));
        assert_eq!(p.title_rows, Some((0, 0)));
    }
}
