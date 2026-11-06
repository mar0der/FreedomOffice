//! Print layout: turns sheets into pages of simple drawing operations (in points,
//! origin at the paper's top-left). The same pages feed the on-screen preview,
//! the Windows printer and PDF export.

pub mod pdf;
#[cfg(windows)]
pub mod gdi;
#[cfg(not(windows))]
#[path = "cups.rs"]
pub mod gdi;

use crate::condfmt::CfFrame;
use crate::model::{BorderLine, HAlign, Range, VAlign, Value, Workbook};
use crate::numfmt;
use crate::page::{render_hf, HfContext, Paper, Scaling};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl PRect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        PRect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    /// Font family name, e.g. "Calibri".
    pub family: String,
    /// Size in points (already scaled for the page).
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextOp {
    /// Box the text is aligned in (the cell).
    pub rect: PRect,
    /// Clip box (wider than `rect` when text overflows into empty neighbours).
    pub clip: PRect,
    pub text: String,
    pub font: FontSpec,
    pub color: u32,
    pub halign: Align,
    pub valign: VAlign,
    pub wrap: bool,
    /// Text rotation in xlsx terms (0, 1-90 ccw, 91-180 cw, 255 vertical).
    pub rotation: u8,
    /// A number: if it doesn't fit, show "####" like the grid does.
    pub number: bool,
    /// Left indent in points.
    pub indent: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Fill { rect: PRect, color: u32 },
    Line { x1: f32, y1: f32, x2: f32, y2: f32, width: f32, color: u32, dashed: bool },
    Text(TextOp),
    /// Filled polygon (charts).
    Poly { pts: Vec<(f32, f32)>, color: u32 },
    /// Open polyline (charts).
    Path { pts: Vec<(f32, f32)>, width: f32, color: u32 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub width: f32,
    pub height: f32,
    pub landscape: bool,
    pub paper: Paper,
    pub ops: Vec<Op>,
    /// Sheet the page comes from (for chart placement and captions).
    pub sheet: usize,
    /// Table area on the page (for "Show Margins" and chart placement).
    pub content: PRect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintWhat {
    ActiveSheet,
    Workbook,
    Selection,
}

/// Charts are drawn by the UI layer; it supplies their operations for a target rectangle.
pub type ChartOps<'a> = dyn Fn(usize, usize, PRect) -> Vec<Op> + 'a;

pub struct LayoutOptions<'a> {
    pub what: PrintWhat,
    pub selection: Option<Range>,
    pub ignore_print_area: bool,
    pub file_name: &'a str,
    pub file_path: &'a str,
}

const PX: f32 = 0.75; // screen pixel -> point
const GRID_COLOR: u32 = 0x8C8C8C;

/// Lays out pages for the requested sheets.
pub fn layout(wb: &Workbook, opts: &LayoutOptions, charts: &ChartOps) -> Vec<Page> {
    let sheets: Vec<usize> = match opts.what {
        PrintWhat::Workbook => (0..wb.sheets.len()).filter(|i| !wb.sheets[*i].hidden).collect(),
        _ => vec![wb.active],
    };
    let mut pages = Vec::new();
    for si in sheets {
        let area = match opts.what {
            PrintWhat::Selection => opts.selection,
            _ => None,
        };
        pages.extend(layout_sheet(wb, si, area, opts.ignore_print_area, charts));
    }
    // headers / footers need the total page count
    let total = pages.len() as u32;
    let mut counters: Vec<u32> = Vec::new();
    for (i, p) in pages.iter_mut().enumerate() {
        let sh = &wb.sheets[p.sheet];
        let first = sh.page.first_page.unwrap_or(1);
        // page numbers restart per sheet like the desktop app does for each sheet's setup
        let n_in_sheet = counters.iter().filter(|s| **s == p.sheet as u32).count() as u32;
        counters.push(p.sheet as u32);
        let page_no = if sh.page.first_page.is_some() { first + n_in_sheet } else { i as u32 + 1 };
        let ctx = HfContext { page: page_no, pages: total, sheet: &sh.name, file: opts.file_name, path: opts.file_path };
        add_header_footer(p, &sh.page, &ctx);
    }
    pages
}

fn add_header_footer(p: &mut Page, ps: &crate::page::PageSetup, ctx: &HfContext) {
    let m = ps.margins;
    let x0 = m.left * 72.0;
    let w = p.width - (m.left + m.right) * 72.0;
    let font = FontSpec { family: "Calibri".into(), size: 11.0, bold: false, italic: false, underline: false, strike: false };
    for (hf, top) in [(&ps.header, true), (&ps.footer, false)] {
        for (text, align) in [(&hf.left, Align::Left), (&hf.center, Align::Center), (&hf.right, Align::Right)] {
            if text.is_empty() {
                continue;
            }
            let t = render_hf(text, ctx);
            let y = if top { m.header * 72.0 } else { p.height - m.footer * 72.0 - 15.0 };
            let rect = PRect::new(x0, y, w, 15.0);
            p.ops.push(Op::Text(TextOp {
                rect,
                clip: PRect::new(0.0, y - 2.0, p.width, 19.0),
                text: t,
                font: font.clone(),
                color: 0,
                halign: align,
                valign: if top { VAlign::Top } else { VAlign::Bottom },
                wrap: false,
                rotation: 0,
                number: false,
                indent: 0.0,
            }));
        }
    }
}

/// Splits sizes into bands that fit `avail`, honouring forced breaks (indices where a band must start).
fn bands(sizes: &[f32], avail: f32, forced: &[bool]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut acc = 0.0;
    for (i, s) in sizes.iter().enumerate() {
        let force = i > start && forced.get(i).copied().unwrap_or(false);
        if (acc + s > avail + 0.01 && i > start) || force {
            out.push((start, i));
            start = i;
            acc = 0.0;
        }
        acc += s;
    }
    if start < sizes.len() {
        out.push((start, sizes.len()));
    }
    out
}

/// The cell range that prints for a sheet (print area, else used range incl. spills and charts).
pub fn print_range(wb: &Workbook, si: usize, ignore_print_area: bool) -> Option<Range> {
    let sh = &wb.sheets[si];
    if !ignore_print_area {
        if let Some(a) = sh.page.print_area {
            return Some(a);
        }
    }
    let mut rg = sh.used_range();
    if let Some((er, ec)) = sh.extent_with_spills() {
        rg = Some(match rg {
            Some(r) => r.union(&Range::new(r.r1, r.c1, er, ec)),
            None => Range::new(0, 0, er, ec),
        });
    }
    for ch in &sh.charts {
        // a chart extends the area to the cells it covers
        let (r, c) = ch.anchor;
        let mut c2 = c;
        let mut w = ch.offset.0 + ch.size.0;
        while w > 0.0 && c2 < crate::model::MAX_COLS - 1 {
            w -= sh.col_width(c2).max(1.0);
            if w > 0.0 {
                c2 += 1;
            }
        }
        let mut r2 = r;
        let mut h = ch.offset.1 + ch.size.1;
        while h > 0.0 && r2 < crate::model::MAX_ROWS - 1 {
            h -= sh.row_height(r2).max(1.0);
            if h > 0.0 {
                r2 += 1;
            }
        }
        let cr = Range::new(r, c, r2, c2);
        rg = Some(match rg {
            Some(x) => x.union(&cr),
            None => cr,
        });
    }
    rg
}

struct Axis {
    /// (index, scaled size in pt)
    items: Vec<(u32, f32)>,
    titles: Vec<(u32, f32)>,
}

fn axis(sh: &crate::model::Sheet, a: u32, b: u32, rows: bool, titles: Option<(u32, u32)>, s: f32) -> Axis {
    let size = |i: u32| if rows { sh.row_height(i) } else { sh.col_width(i) } * PX * s;
    let items = (a..=b).filter(|i| size(*i) > 0.0).map(|i| (i, size(i))).collect();
    let titles = match titles {
        Some((t1, t2)) => (t1..=t2).filter(|i| size(*i) > 0.0).map(|i| (i, size(i))).collect(),
        None => Vec::new(),
    };
    Axis { items, titles }
}

const HDR_W: f32 = 26.0;
const HDR_H: f32 = 15.0;

fn layout_sheet(wb: &Workbook, si: usize, area: Option<Range>, ignore_pa: bool, charts: &ChartOps) -> Vec<Page> {
    let sh = &wb.sheets[si];
    let ps = &sh.page;
    let Some(area) = area.or_else(|| print_range(wb, si, ignore_pa)) else { return Vec::new() };
    // Keep absurd areas (whole columns) inside the data.
    let area = match sh.extent_with_spills() {
        Some((er, ec)) if area.is_full_cols() || area.is_full_rows() => Range {
            r1: area.r1,
            c1: area.c1,
            r2: area.r2.min(er.max(area.r1)),
            c2: area.c2.min(ec.max(area.c1)),
        },
        _ => area,
    };
    if area.rows() as u64 * area.cols() as u64 > 20_000_000 {
        return Vec::new();
    }
    let (pw, ph) = ps.paper_pt();
    let m = ps.margins;
    let (x0, y0) = (m.left * 72.0, m.top * 72.0);
    let aw = (pw - (m.left + m.right) * 72.0).max(36.0);
    let ah = (ph - (m.top + m.bottom) * 72.0).max(36.0);

    // titles only repeat when they lie outside/before the printed band
    let title_rows = ps.title_rows.filter(|(_, b)| *b < area.r2);
    let title_cols = ps.title_cols.filter(|(_, b)| *b < area.c2);

    let compute = |s: f32, honour_breaks: bool| {
        let rows = axis(sh, area.r1, area.r2, true, title_rows, s);
        let cols = axis(sh, area.c1, area.c2, false, title_cols, s);
        let hw = if ps.headings { HDR_W * s } else { 0.0 };
        let hh = if ps.headings { HDR_H * s } else { 0.0 };
        let tw: f32 = cols.titles.iter().filter(|(i, _)| *i < area.c1).map(|x| x.1).sum();
        let th: f32 = rows.titles.iter().filter(|(i, _)| *i < area.r1).map(|x| x.1).sum();
        let fr: Vec<bool> = rows.items.iter().map(|(i, _)| honour_breaks && ps.row_breaks.contains(i)).collect();
        let fc: Vec<bool> = cols.items.iter().map(|(i, _)| honour_breaks && ps.col_breaks.contains(i)).collect();
        // later pages repeat the titles, so they get less room; use the smaller space for all bands
        let full_tw: f32 = cols.titles.iter().map(|x| x.1).sum();
        let full_th: f32 = rows.titles.iter().map(|x| x.1).sum();
        let rb = bands(&rows.items.iter().map(|x| x.1).collect::<Vec<_>>(), ah - hh - full_th.max(th), &fr);
        let cb = bands(&cols.items.iter().map(|x| x.1).collect::<Vec<_>>(), aw - hw - full_tw.max(tw), &fc);
        (rows, cols, rb, cb, hw, hh)
    };

    let (s, (rows, cols, rbands, cbands, hw, hh)) = match ps.scaling {
        Scaling::Percent(p) => {
            let s = (p.clamp(10, 400) as f32) / 100.0;
            (s, compute(s, true))
        }
        Scaling::Fit { wide, tall } => {
            let mut s = 1.0f32;
            loop {
                let r = compute(s, false);
                let ok_w = wide == 0 || r.3.len() <= wide as usize;
                let ok_h = tall == 0 || r.2.len() <= tall as usize;
                if (ok_w && ok_h) || s <= 0.1 {
                    break (s, r);
                }
                // jump close to the target, then refine
                let total_w: f32 = r.1.items.iter().map(|x| x.1).sum::<f32>() + r.4;
                let total_h: f32 = r.0.items.iter().map(|x| x.1).sum::<f32>() + r.5;
                let mut next = s * 0.97;
                if !ok_w && wide > 0 {
                    next = next.min(s * (aw * wide as f32) / total_w.max(1.0));
                }
                if !ok_h && tall > 0 {
                    next = next.min(s * (ah * tall as f32) / total_h.max(1.0));
                }
                s = next.clamp(0.1, s * 0.995);
            }
        }
    };

    let mut order: Vec<(usize, usize)> = Vec::new();
    if ps.over_then_down {
        for r in 0..rbands.len() {
            for c in 0..cbands.len() {
                order.push((r, c));
            }
        }
    } else {
        for c in 0..cbands.len() {
            for r in 0..rbands.len() {
                order.push((r, c));
            }
        }
    }

    let cf = CfFrame::uncached(sh);
    let mut pages = Vec::new();
    for (rbi, cbi) in order {
        let (ra, rb) = rbands[rbi];
        let (ca, cb) = cbands[cbi];
        let band_rows: Vec<(u32, f32)> = rows.items[ra..rb].to_vec();
        let band_cols: Vec<(u32, f32)> = cols.items[ca..cb].to_vec();
        // repeated titles come first unless this band already shows them
        let mut prow: Vec<(u32, f32)> = rows.titles.iter().filter(|(i, _)| *i < band_rows[0].0).copied().collect();
        prow.extend(band_rows);
        let mut pcol: Vec<(u32, f32)> = cols.titles.iter().filter(|(i, _)| *i < band_cols[0].0).copied().collect();
        pcol.extend(band_cols);

        let tw: f32 = pcol.iter().map(|x| x.1).sum::<f32>() + hw;
        let th: f32 = prow.iter().map(|x| x.1).sum::<f32>() + hh;
        let ox = x0 + if ps.center_h { ((aw - tw) / 2.0).max(0.0) } else { 0.0 };
        let oy = y0 + if ps.center_v { ((ah - th) / 2.0).max(0.0) } else { 0.0 };
        let mut xs = Vec::with_capacity(pcol.len() + 1);
        let mut x = ox + hw;
        for (_, w) in &pcol {
            xs.push(x);
            x += w;
        }
        xs.push(x);
        let mut ys = Vec::with_capacity(prow.len() + 1);
        let mut y = oy + hh;
        for (_, h) in &prow {
            ys.push(y);
            y += h;
        }
        ys.push(y);

        let mut ops = Vec::new();
        let col_pos = |c: u32| pcol.iter().position(|(i, _)| *i == c);
        let row_pos = |r: u32| prow.iter().position(|(i, _)| *i == r);
        let font_for = |st: &crate::model::Style, ov: Option<&crate::condfmt::CfOverride>| FontSpec {
            family: st.font.clone().unwrap_or_else(|| "Calibri".into()),
            size: st.font_size() * s,
            bold: st.bold || ov.is_some_and(|o| o.bold),
            italic: st.italic || ov.is_some_and(|o| o.italic),
            underline: st.underline || ov.is_some_and(|o| o.underline),
            strike: st.strike || ov.is_some_and(|o| o.strike),
        };

        // Headings
        if ps.headings {
            let hfont = FontSpec { family: "Calibri".into(), size: 10.0 * s, bold: false, italic: false, underline: false, strike: false };
            for (k, (c, _)) in pcol.iter().enumerate() {
                let r = PRect::new(xs[k], oy, xs[k + 1] - xs[k], hh);
                ops.push(Op::Fill { rect: r, color: 0xF2F2F2 });
                ops.push(Op::Text(TextOp {
                    rect: r,
                    clip: r,
                    text: crate::model::col_name(*c),
                    font: hfont.clone(),
                    color: 0x404040,
                    halign: Align::Center,
                    valign: VAlign::Center,
                    wrap: false,
                    rotation: 0,
                    number: false,
                    indent: 0.0,
                }));
            }
            for (k, (r, _)) in prow.iter().enumerate() {
                let rr = PRect::new(ox, ys[k], hw, ys[k + 1] - ys[k]);
                ops.push(Op::Fill { rect: rr, color: 0xF2F2F2 });
                ops.push(Op::Text(TextOp {
                    rect: rr,
                    clip: rr,
                    text: (r + 1).to_string(),
                    font: hfont.clone(),
                    color: 0x404040,
                    halign: Align::Center,
                    valign: VAlign::Center,
                    wrap: false,
                    rotation: 0,
                    number: false,
                    indent: 0.0,
                }));
            }
            ops.push(Op::Fill { rect: PRect::new(ox, oy, hw, hh), color: 0xE6E6E6 });
        }

        // Cell rectangles (merged anchors span their merge within this page)
        struct CellDraw {
            r: u32,
            c: u32,
            rect: PRect,
            ri: usize,
            ci: usize,
        }
        let mut cells: Vec<CellDraw> = Vec::new();
        for (ri, (r, _)) in prow.iter().enumerate() {
            for (ci, (c, _)) in pcol.iter().enumerate() {
                let (r, c) = (*r, *c);
                let mut rect = PRect::new(xs[ci], ys[ri], xs[ci + 1] - xs[ci], ys[ri + 1] - ys[ri]);
                if let Some(mg) = sh.merge_at(r, c) {
                    // draw the merge once: from its first cell present on this page
                    let first_r = prow.iter().position(|(i, _)| mg.contains(*i, c) && *i >= mg.r1).unwrap_or(ri);
                    let first_c = pcol.iter().position(|(i, _)| mg.contains(r, *i) && *i >= mg.c1).unwrap_or(ci);
                    if first_r != ri || first_c != ci {
                        continue;
                    }
                    let last_r = prow.iter().rposition(|(i, _)| *i <= mg.r2 && *i >= mg.r1).unwrap_or(ri);
                    let last_c = pcol.iter().rposition(|(i, _)| *i <= mg.c2 && *i >= mg.c1).unwrap_or(ci);
                    rect = PRect::new(xs[ci], ys[ri], xs[last_c + 1] - xs[ci], ys[last_r + 1] - ys[ri]);
                    cells.push(CellDraw { r: mg.r1, c: mg.c1, rect, ri, ci });
                    continue;
                }
                cells.push(CellDraw { r, c, rect, ri, ci });
            }
        }

        // Fills (cell fill, conditional fill, data bars)
        for cd in &cells {
            let st = sh.get(cd.r, cd.c).map(|x| wb.style(x.style)).cloned().unwrap_or_default();
            let ov = cf.eval(wb, si, cd.r, cd.c);
            let fill = ov.as_ref().and_then(|o| o.fill).or(st.fill);
            if let Some(f) = fill {
                if !ps.black_white {
                    ops.push(Op::Fill { rect: cd.rect, color: f });
                }
            }
            if let Some(bar) = ov.as_ref().and_then(|o| o.bar) {
                let inner = PRect::new(cd.rect.x + 1.5, cd.rect.y + 1.5, cd.rect.w - 3.0, cd.rect.h - 3.0);
                if inner.w > 0.0 && bar.end > bar.start {
                    let rect = PRect::new(inner.x + inner.w * bar.start, inner.y, inner.w * (bar.end - bar.start), inner.h);
                    let color = if bar.gradient { tint(bar.color, 0.35) } else { bar.color };
                    ops.push(Op::Fill { rect, color });
                }
            }
        }

        // Gridlines
        if ps.gridlines {
            let (gx0, gx1) = (xs[0], *xs.last().unwrap());
            let (gy0, gy1) = (ys[0], *ys.last().unwrap());
            for x in &xs {
                ops.push(Op::Line { x1: *x, y1: gy0, x2: *x, y2: gy1, width: 0.25, color: GRID_COLOR, dashed: false });
            }
            for y in &ys {
                ops.push(Op::Line { x1: gx0, y1: *y, x2: gx1, y2: *y, width: 0.25, color: GRID_COLOR, dashed: false });
            }
        }
        if ps.headings {
            let (gx0, gx1) = (ox, *xs.last().unwrap());
            let (gy0, gy1) = (oy, *ys.last().unwrap());
            for x in std::iter::once(&ox).chain(xs.iter()) {
                ops.push(Op::Line { x1: *x, y1: gy0, x2: *x, y2: if ps.gridlines { gy1 } else { ys[0] }, width: 0.25, color: GRID_COLOR, dashed: false });
            }
            for y in std::iter::once(&oy).chain(ys.iter()) {
                ops.push(Op::Line { x1: gx0, y1: *y, x2: if ps.gridlines { gx1 } else { xs[0] }, y2: *y, width: 0.25, color: GRID_COLOR, dashed: false });
            }
        }

        // Text
        for cd in &cells {
            let Some(cell) = sh.get(cd.r, cd.c) else {
                // spilled values print like ordinary ones
                let v = sh.display_value(cd.r, cd.c);
                if v.is_empty() {
                    continue;
                }
                let f = numfmt::format_value(v, "");
                ops.push(text_op(cd.rect, cd.rect, f.text, font_for(&Default::default(), None), 0, auto_align(v), VAlign::Bottom, false, 0, matches!(v, Value::Number(_)), 0.0));
                continue;
            };
            let st = wb.style(cell.style);
            let v = sh.display_value(cd.r, cd.c);
            let fmtd = numfmt::format_value(v, &st.num_fmt);
            if fmtd.text.is_empty() {
                continue;
            }
            let ov = cf.eval(wb, si, cd.r, cd.c);
            let align = match st.halign {
                HAlign::General => auto_align(v),
                HAlign::Left => Align::Left,
                HAlign::Center => Align::Center,
                HAlign::Right => Align::Right,
            };
            let mut color = ov.as_ref().and_then(|o| o.color).or(fmtd.color).or(st.color).unwrap_or(0);
            if ps.black_white {
                color = 0;
            }
            let is_num = matches!(v, Value::Number(_));
            // overflow into empty neighbours on this page (text only)
            let mut clip = cd.rect;
            if !st.wrap && !is_num && st.rotation == 0 && sh.merge_at(cd.r, cd.c).is_none() {
                let empty = |k: usize| {
                    let c = pcol[k].0;
                    !sh.is_populated(cd.r, c) && sh.merge_at(cd.r, c).is_none()
                };
                if matches!(align, Align::Left | Align::Center) {
                    let mut k = cd.ci + 1;
                    while k < pcol.len() && empty(k) {
                        clip.w = xs[k + 1] - clip.x;
                        k += 1;
                    }
                }
                if matches!(align, Align::Right | Align::Center) && cd.ci > 0 {
                    let mut k = cd.ci;
                    while k > 0 && empty(k - 1) {
                        let nx = xs[k - 1];
                        clip.w += clip.x - nx;
                        clip.x = nx;
                        k -= 1;
                    }
                }
            }
            ops.push(text_op(
                cd.rect,
                clip,
                fmtd.text,
                font_for(st, ov.as_ref()),
                color,
                align,
                st.valign,
                st.wrap,
                st.rotation,
                is_num,
                st.indent as f32 * 7.5 * s,
            ));
        }

        // Borders
        for cd in &cells {
            let Some(cell) = sh.get(cd.r, cd.c) else { continue };
            let b = wb.style(cell.style).border;
            let r = cd.rect;
            for (line, (x1, y1, x2, y2)) in [
                (b.top, (r.x, r.y, r.right(), r.y)),
                (b.bottom, (r.x, r.bottom(), r.right(), r.bottom())),
                (b.left, (r.x, r.y, r.x, r.bottom())),
                (b.right, (r.right(), r.y, r.right(), r.bottom())),
            ] {
                let (w, dashed) = match line {
                    BorderLine::None => continue,
                    BorderLine::Thin => (0.5, false),
                    BorderLine::Dashed | BorderLine::Dotted => (0.5, true),
                    BorderLine::Medium => (1.0, false),
                    BorderLine::Thick => (1.5, false),
                    BorderLine::Double => (1.5, false),
                };
                ops.push(Op::Line { x1, y1, x2, y2, width: w * s.max(0.5), color: 0, dashed });
            }
            let _ = (cd.ri, cd.ci, row_pos(cd.r), col_pos(cd.c));
        }

        // Charts whose top-left lands on this page
        for (k, ch) in sh.charts.iter().enumerate() {
            let (r, c) = ch.anchor;
            if let (Some(ri), Some(ci)) = (row_pos(r), col_pos(c)) {
                let rect = PRect::new(xs[ci] + ch.offset.0 * PX * s, ys[ri] + ch.offset.1 * PX * s, ch.size.0 * PX * s, ch.size.1 * PX * s);
                ops.extend(charts(si, k, rect));
            }
        }

        let content = PRect::new(ox, oy, tw, th);
        pages.push(Page { width: pw, height: ph, landscape: ps.landscape, paper: ps.paper, ops, sheet: si, content });
    }
    pages
}

/// Lightens (t > 0) a 0xRRGGBB colour towards white.
fn tint(c: u32, t: f32) -> u32 {
    let ch = |v: u32| -> u32 { (v as f32 + (255.0 - v as f32) * t).round().clamp(0.0, 255.0) as u32 };
    (ch((c >> 16) & 255) << 16) | (ch((c >> 8) & 255) << 8) | ch(c & 255)
}

fn auto_align(v: &Value) -> Align {
    match v {
        Value::Number(_) => Align::Right,
        Value::Bool(_) | Value::Error(_) => Align::Center,
        _ => Align::Left,
    }
}

#[allow(clippy::too_many_arguments)]
fn text_op(
    rect: PRect,
    clip: PRect,
    text: String,
    font: FontSpec,
    color: u32,
    halign: Align,
    valign: VAlign,
    wrap: bool,
    rotation: u8,
    number: bool,
    indent: f32,
) -> Op {
    Op::Text(TextOp { rect, clip, text, font, color, halign, valign, wrap, rotation, number, indent })
}

/// Row/column indices where automatic or manual page breaks fall (for the dashed lines in Normal view).
pub fn page_breaks(wb: &Workbook, si: usize) -> (Vec<u32>, Vec<u32>) {
    let mut wb2_rows = Vec::new();
    let mut wb2_cols = Vec::new();
    let sh = &wb.sheets[si];
    let ps = &sh.page;
    if let Some(area) = print_range(wb, si, false) {
        let (pw, ph) = ps.paper_pt();
        let m = ps.margins;
        let aw = pw - (m.left + m.right) * 72.0;
        let ah = ph - (m.top + m.bottom) * 72.0;
        let s = match ps.scaling {
            Scaling::Percent(p) => p as f32 / 100.0,
            Scaling::Fit { .. } => 1.0,
        };
        let rows = axis(sh, area.r1, area.r2.min(area.r1 + 200_000), true, None, s);
        let cols = axis(sh, area.c1, area.c2, false, None, s);
        let fr: Vec<bool> = rows.items.iter().map(|(i, _)| ps.row_breaks.contains(i)).collect();
        let fc: Vec<bool> = cols.items.iter().map(|(i, _)| ps.col_breaks.contains(i)).collect();
        for (a, _) in bands(&rows.items.iter().map(|x| x.1).collect::<Vec<_>>(), ah, &fr).iter().skip(1) {
            wb2_rows.push(rows.items[*a].0);
        }
        for (a, _) in bands(&cols.items.iter().map(|x| x.1).collect::<Vec<_>>(), aw, &fc).iter().skip(1) {
            wb2_cols.push(cols.items[*a].0);
        }
    }
    (wb2_rows, wb2_cols)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{cell_from_input, Workbook};

    fn sample(rows: u32, cols: u32) -> Workbook {
        let mut wb = Workbook::new();
        for r in 0..rows {
            for c in 0..cols {
                let cell = cell_from_input(&mut wb, &format!("R{r}C{c}"), 0);
                wb.sheets[0].set(r, c, cell);
            }
        }
        wb
    }

    fn opts() -> LayoutOptions<'static> {
        LayoutOptions { what: PrintWhat::ActiveSheet, selection: None, ignore_print_area: false, file_name: "Book1", file_path: "" }
    }

    #[test]
    fn paginates_down_then_over() {
        let mut wb = sample(120, 12);
        wb.sheets[0].page.paper = Paper::A4;
        let pages = layout(&wb, &opts(), &|_, _, _| Vec::new());
        // 120 rows * 15pt = 1800pt over ~734pt per page -> 3 rows of pages; 12 cols * 48pt = 576pt over ~494pt -> 2
        assert_eq!(pages.len(), 6);
        // first page holds R0C0, second page continues down (R50ish C0)
        let texts = |p: &Page| p.ops.iter().filter_map(|o| if let Op::Text(t) = o { Some(t.text.clone()) } else { None }).collect::<Vec<_>>();
        assert!(texts(&pages[0]).contains(&"R0C0".to_string()));
        assert!(texts(&pages[1]).iter().any(|t| t.ends_with("C0")) && !texts(&pages[1]).contains(&"R0C0".to_string()));
    }

    #[test]
    fn fit_to_one_page_and_titles() {
        let mut wb = sample(120, 12);
        wb.sheets[0].page.scaling = Scaling::Fit { wide: 1, tall: 1 };
        assert_eq!(layout(&wb, &opts(), &|_, _, _| Vec::new()).len(), 1);
        wb.sheets[0].page.scaling = Scaling::Fit { wide: 1, tall: 0 };
        wb.sheets[0].page.title_rows = Some((0, 0));
        let pages = layout(&wb, &opts(), &|_, _, _| Vec::new());
        assert!(pages.len() >= 2);
        for p in &pages {
            assert!(p.ops.iter().any(|o| matches!(o, Op::Text(t) if t.text == "R0C0")), "title row repeats on every page");
        }
    }

    #[test]
    fn manual_breaks_and_header_footer() {
        let mut wb = sample(20, 2);
        wb.sheets[0].page.row_breaks.insert(10);
        wb.sheets[0].page.footer = crate::page::HeaderFooter::center("Page &P of &N");
        let pages = layout(&wb, &opts(), &|_, _, _| Vec::new());
        assert_eq!(pages.len(), 2);
        assert!(pages[1].ops.iter().any(|o| matches!(o, Op::Text(t) if t.text == "Page 2 of 2")));
    }

    #[test]
    fn selection_and_print_area() {
        let mut wb = sample(50, 5);
        let o = LayoutOptions { what: PrintWhat::Selection, selection: Some(Range::new(0, 0, 1, 1)), ..opts() };
        let pages = layout(&wb, &o, &|_, _, _| Vec::new());
        assert_eq!(pages.len(), 1);
        let n = pages[0].ops.iter().filter(|o| matches!(o, Op::Text(_))).count();
        assert_eq!(n, 4);
        wb.sheets[0].page.print_area = Some(Range::new(0, 0, 0, 0));
        let pages = layout(&wb, &opts(), &|_, _, _| Vec::new());
        assert_eq!(pages[0].ops.iter().filter(|o| matches!(o, Op::Text(_))).count(), 1);
    }
}
