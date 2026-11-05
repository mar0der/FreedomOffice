//! PivotTable styles: the built-in Light, Medium and Dark families on the Office theme
//! colours, and the look of each report cell.

use super::engine::{LineKind, Role};
use super::{PivotTable, VALUES};
use crate::model::BorderLine;
use crate::table::Look;

/// Formatting of one table-style element.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Elem {
    pub fill: Option<u32>,
    pub color: Option<u32>,
    pub bold: bool,
    pub top: Option<(BorderLine, u32)>,
    pub bottom: Option<(BorderLine, u32)>,
}

impl Elem {
    fn apply(&self, l: &mut Look) {
        if self.fill.is_some() {
            l.fill = self.fill;
        }
        if self.color.is_some() {
            l.color = self.color;
        }
        if self.bold {
            l.bold = true;
        }
        if self.top.is_some() {
            l.top = self.top;
        }
        if self.bottom.is_some() {
            l.bottom = self.bottom;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StyleDef {
    pub whole: Elem,
    /// Line under every row of the body.
    pub inner_h: Option<u32>,
    pub header: Elem,
    pub grand_row: Elem,
    pub grand_col: Elem,
    pub first_col: Elem,
    pub row_sub: [Elem; 3],
    pub subtotal_row: [Elem; 3],
    pub row_stripe: Elem,
    pub col_stripe: Elem,
    pub page_label: Elem,
    pub page_value: Elem,
}

/// Shades of a theme accent (index 0 = the black/grey family) as the Office palette has them:
/// 0 = lighter 80%, 1 = lighter 60%, 2 = lighter 40%, 3 = the accent, 4 = darker 25%, 5 = darker 50%.
pub fn shade(ai: usize, level: usize) -> u32 {
    const T: [[u32; 6]; 7] = [
        [0xD9D9D9, 0xBFBFBF, 0xA6A6A6, 0x808080, 0x595959, 0x404040],
        [0xD9E1F2, 0xB4C6E7, 0x8EA9DB, 0x4472C4, 0x305496, 0x203764],
        [0xFCE4D6, 0xF8CBAD, 0xF4B084, 0xED7D31, 0xC65911, 0x833C0C],
        [0xEDEDED, 0xDBDBDB, 0xC9C9C9, 0xA5A5A5, 0x7B7B7B, 0x525252],
        [0xFFF2CC, 0xFFE699, 0xFFD966, 0xFFC000, 0xBF8F00, 0x806000],
        [0xDDEBF7, 0xBDD7EE, 0x9BC2E6, 0x5B9BD5, 0x2F75B5, 0x1F4E78],
        [0xE2EFDA, 0xC6E0B4, 0xA9D08E, 0x70AD47, 0x548235, 0x375623],
    ];
    T[ai.min(6)][level.min(5)]
}

/// "PivotStyleMedium9" -> (family 0..=5, accent 0..=6). Families: Light 1-7, 8-14, 15-21,
/// Medium 1-7, 8-14, Dark 1-7.
pub fn parts(name: &str) -> Option<(usize, usize)> {
    let rest = name.strip_prefix("PivotStyle")?;
    let (base, n) = if let Some(n) = rest.strip_prefix("Light") {
        (0, n)
    } else if let Some(n) = rest.strip_prefix("Medium") {
        (3, n)
    } else if let Some(n) = rest.strip_prefix("Dark") {
        (5, n)
    } else {
        return None;
    };
    let n: usize = n.parse().ok()?;
    if n == 0 {
        return None;
    }
    let fam = base + (n - 1) / 7;
    let max = match base {
        0 => 2,
        3 => 4,
        _ => 5,
    };
    if fam > max {
        return None;
    }
    Some((fam, (n - 1) % 7))
}

/// Name of the style of a family and accent.
pub fn name(fam: usize, ai: usize) -> String {
    let (kind, first) = match fam {
        0..=2 => ("Light", fam * 7),
        3 | 4 => ("Medium", (fam - 3) * 7),
        _ => ("Dark", 0),
    };
    format!("PivotStyle{kind}{}", first + ai + 1)
}

/// Styles offered in the Design gallery, by row.
pub fn gallery() -> Vec<Vec<String>> {
    (0..6).map(|fam| (0..7).map(|ai| name(fam, ai)).collect()).collect()
}

pub fn def(name: &str) -> StyleDef {
    let Some((fam, ai)) = parts(name) else {
        return if name.is_empty() { StyleDef::default() } else { def(super::DEFAULT_STYLE) };
    };
    let s = |l| shade(ai, l);
    let thin = |c| Some((BorderLine::Thin, c));
    let white = Some(0xFFFFFF);
    let mut d = StyleDef::default();
    match fam {
        0 => {
            // Light 1-7: bold header and total rows between accent lines.
            d.header = Elem { bold: true, top: thin(s(3)), bottom: thin(s(3)), ..Default::default() };
            d.grand_row = Elem { bold: true, top: thin(s(3)), bottom: thin(s(3)), ..Default::default() };
            d.row_sub = [Elem { bold: true, ..Default::default() }, Elem { bold: true, ..Default::default() }, Elem::default()];
            d.subtotal_row = [Elem { bold: true, ..Default::default() }, Elem { bold: true, ..Default::default() }, Elem::default()];
            d.page_label = Elem { bold: true, ..Default::default() };
            d.row_stripe = Elem { fill: Some(s(0)), ..Default::default() };
            d.col_stripe = Elem { fill: Some(s(0)), ..Default::default() };
        }
        1 => {
            // Light 8-14: boxed header, tinted first-level rows.
            d.header = Elem { bold: true, top: Some((BorderLine::Medium, s(3))), bottom: Some((BorderLine::Medium, s(3))), ..Default::default() };
            d.grand_row = Elem { bold: true, top: Some((BorderLine::Medium, s(3))), bottom: Some((BorderLine::Medium, s(3))), ..Default::default() };
            d.row_sub = [Elem { fill: Some(s(0)), bold: true, ..Default::default() }, Elem { bold: true, ..Default::default() }, Elem::default()];
            d.subtotal_row = [Elem { fill: Some(s(0)), bold: true, top: thin(s(1)), bottom: thin(s(1)), ..Default::default() }, Elem { bold: true, ..Default::default() }, Elem::default()];
            d.row_stripe = Elem { fill: Some(s(0)), ..Default::default() };
            d.col_stripe = Elem { fill: Some(s(0)), ..Default::default() };
        }
        2 => {
            // Light 15-21 (Light16 is Excel's default): tinted header and total bands.
            d.header = Elem { fill: Some(s(0)), bold: true, bottom: thin(s(2)), ..Default::default() };
            d.grand_row = Elem { fill: Some(s(0)), bold: true, top: thin(s(2)), ..Default::default() };
            d.row_sub = [Elem { bold: true, bottom: thin(s(2)), ..Default::default() }, Elem { bold: true, ..Default::default() }, Elem::default()];
            d.subtotal_row = [Elem { bold: true, top: thin(s(3)), bottom: thin(s(3)), ..Default::default() }, Elem { bold: true, ..Default::default() }, Elem::default()];
            d.page_label = Elem { fill: Some(s(0)), bottom: thin(s(2)), ..Default::default() };
            d.page_value = Elem { fill: Some(s(0)), bottom: thin(s(2)), ..Default::default() };
            d.row_stripe = Elem { fill: Some(0xD9D9D9), ..Default::default() };
            d.col_stripe = Elem { fill: Some(0xD9D9D9), ..Default::default() };
        }
        3 => {
            // Medium 1-7: dark header, accent first-level rows.
            d.inner_h = Some(s(0));
            d.header = Elem { fill: Some(s(4)), color: white, bold: true, ..Default::default() };
            d.grand_row = Elem { bold: true, top: Some((BorderLine::Double, s(4))), ..Default::default() };
            d.row_sub = [Elem { fill: Some(s(2)), color: white, bold: true, ..Default::default() }, Elem { bold: true, ..Default::default() }, Elem::default()];
            d.subtotal_row = [Elem { fill: Some(s(2)), color: white, bold: true, ..Default::default() }, Elem { fill: Some(0xD9D9D9), bold: true, ..Default::default() }, Elem::default()];
            d.page_label = Elem { bold: true, ..Default::default() };
            d.row_stripe = Elem { fill: Some(s(0)), ..Default::default() };
            d.col_stripe = Elem { fill: Some(s(0)), ..Default::default() };
        }
        4 => {
            // Medium 8-14: accent header, tinted first-level rows.
            d.header = Elem { fill: Some(s(3)), color: white, bold: true, ..Default::default() };
            d.grand_row = Elem { bold: true, top: thin(s(4)), bottom: Some((BorderLine::Medium, s(4))), ..Default::default() };
            d.row_sub = [Elem { fill: Some(s(0)), bold: true, ..Default::default() }, Elem { bold: true, ..Default::default() }, Elem::default()];
            d.subtotal_row = [Elem { fill: Some(s(1)), bold: true, ..Default::default() }, Elem { bold: true, ..Default::default() }, Elem::default()];
            d.row_stripe = Elem { fill: Some(s(0)), ..Default::default() };
            d.col_stripe = Elem { fill: Some(s(0)), ..Default::default() };
        }
        _ => {
            // Dark 1-7: tinted body under a dark header and total row.
            d.whole = Elem { fill: Some(s(1)), ..Default::default() };
            d.inner_h = Some(s(0));
            d.header = Elem { fill: Some(s(5)), color: white, bold: true, bottom: thin(s(3)), ..Default::default() };
            d.grand_row = Elem { fill: Some(s(5)), color: white, bold: true, ..Default::default() };
            d.row_sub = [Elem { fill: Some(s(0)), bold: true, top: thin(s(2)), bottom: thin(s(2)), ..Default::default() }, Elem { bold: true, ..Default::default() }, Elem::default()];
            d.subtotal_row = [Elem { bold: true, bottom: thin(s(0)), ..Default::default() }, Elem { bold: true, ..Default::default() }, Elem::default()];
            d.page_label = Elem { fill: Some(s(5)), color: white, bold: true, ..Default::default() };
            d.page_value = Elem { fill: Some(s(5)), color: white, bold: true, ..Default::default() };
            d.row_stripe = Elem { fill: Some(s(0)), ..Default::default() };
            d.col_stripe = Elem { fill: Some(s(0)), ..Default::default() };
        }
    }
    d
}

/// Look of report cell (r, c) of a PivotTable, None outside the report.
pub fn look(pt: &PivotTable, r: u32, c: u32) -> Option<Look> {
    let o = &pt.out;
    let role = o.role(r, c);
    if role == Role::None {
        return None;
    }
    let d = def(&pt.style);
    let mut l = Look::default();
    match role {
        Role::PageLabel(_) => {
            d.page_label.apply(&mut l);
            return Some(l);
        }
        Role::PageValue(_) => {
            d.page_value.apply(&mut l);
            return Some(l);
        }
        _ => {}
    }
    if o.placeholder {
        return None;
    }
    let rg = o.range;
    let in_header = r < rg.r1 + o.header_rows;
    d.whole.apply(&mut l);
    if let Some(c) = d.inner_h {
        if !in_header {
            l.bottom = Some((BorderLine::Thin, c));
        }
    }
    let row_line = if in_header { None } else { o.row_at.get((r - rg.r1 - o.header_rows) as usize).and_then(|i| o.rows.get(*i)) };
    let col_line = if c < rg.c1 + o.label_cols { None } else { o.col_at.get((c - rg.c1 - o.label_cols) as usize).and_then(|i| o.cols.get(*i)) };
    if !in_header {
        let i = r - rg.r1 - o.header_rows;
        if pt.banded_rows && i % 2 == 0 && row_line.is_some_and(|l| l.kind != LineKind::Grand) {
            d.row_stripe.apply(&mut l);
        }
        if pt.banded_cols && c >= rg.c1 + o.label_cols && (c - rg.c1 - o.label_cols) % 2 == 0 && col_line.is_some_and(|l| l.kind != LineKind::Grand) {
            d.col_stripe.apply(&mut l);
        }
    }
    if pt.row_headers && c < rg.c1 + o.label_cols {
        d.first_col.apply(&mut l);
    }
    if col_line.is_some_and(|l| l.kind == LineKind::Grand) {
        d.grand_col.apply(&mut l);
    }
    if let Some(line) = row_line {
        // VALUES-level lines take the look of plain rows.
        let is_values = pt.rows.get(line.depth) == Some(&VALUES);
        if pt.row_headers {
            match line.kind {
                LineKind::Item if line.children && !is_values => d.row_sub[line.depth.min(2)].apply(&mut l),
                LineKind::Subtotal => d.subtotal_row[line.depth.min(2)].apply(&mut l),
                _ => {}
            }
        }
        if line.kind == LineKind::Grand {
            d.grand_row.apply(&mut l);
        }
    }
    if in_header && pt.col_headers {
        d.header.apply(&mut l);
        // Only the last header row carries the bottom line.
        if r + 1 < rg.r1 + o.header_rows {
            l.bottom = None;
        }
    }
    Some(l)
}
