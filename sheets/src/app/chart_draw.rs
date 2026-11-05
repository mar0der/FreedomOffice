//! Chart painting: every chart type, axes, legend, labels, trendlines, error
//! bars and data tables. Used for the grid, dialog previews and printing.

use std::f32::consts::{PI, TAU};

use eframe::egui::{epaint::Mesh, epaint::TextShape, Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2};

use super::theme::{self, rgb};
use crate::chart::{self, trend, Axis, Chart, ChartData, ChartKind, Dash, ErrDir, ErrKind, Grouping, LabelPos, Labels, LegendPos, MarkerKind, Scale, TrendKind};

const TEXT: u32 = 0x595959;
const TEXT_ON_DARK: u32 = 0xD9D9D9;
const GRID: u32 = 0xD9D9D9;
const GRID_ON_DARK: u32 = 0x595959;
const MINOR: u32 = 0xF2F2F2;
const BORDER: u32 = 0xD9D9D9;
const ERR: u32 = 0x595959;
/// Points to pixels.
const PT: f32 = 4.0 / 3.0;

/// Parts of a chart that can be selected and formatted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elem {
    ChartArea,
    PlotArea,
    Title,
    Legend,
    /// Category axis (X axis for scatter charts).
    XAxis,
    YAxis,
    Y2Axis,
    XTitle,
    YTitle,
    Y2Title,
    Series(usize),
    DataTable,
    /// All data labels (Format pane only).
    DataLabels,
}

impl Elem {
    pub fn label(self, ch: &Chart, names: &[String]) -> String {
        let (h, v) = if ch.kind == ChartKind::Bar { ("Vertical (Category)", "Horizontal (Value)") } else if ch.kind == ChartKind::Scatter { ("Horizontal (Value)", "Vertical (Value)") } else { ("Horizontal (Category)", "Vertical (Value)") };
        match self {
            Elem::ChartArea => "Chart Area".into(),
            Elem::PlotArea => "Plot Area".into(),
            Elem::Title => "Chart Title".into(),
            Elem::Legend => "Legend".into(),
            Elem::XAxis => format!("{h} Axis"),
            Elem::YAxis => format!("{v} Axis"),
            Elem::Y2Axis => "Secondary Vertical (Value) Axis".into(),
            Elem::XTitle => format!("{h} Axis Title"),
            Elem::YTitle => format!("{v} Axis Title"),
            Elem::Y2Title => "Secondary Vertical Axis Title".into(),
            Elem::Series(i) => format!("Series \"{}\"", names.get(i).map(String::as_str).unwrap_or("")),
            Elem::DataTable => "Data Table".into(),
            Elem::DataLabels => "Data Labels".into(),
        }
    }
}

pub type Hits = Vec<(Rect, Elem)>;

struct D<'a> {
    p: &'a Painter,
    s: f32,
    text: Color32,
    grid: Color32,
    hits: Hits,
}

impl D<'_> {
    fn font(&self, size: f32) -> FontId {
        theme::ui_font(size * self.s)
    }
    fn size(&self, t: &str, size: f32) -> Vec2 {
        self.p.layout_no_wrap(t.to_string(), self.font(size), self.text).size()
    }
    fn text(&self, pos: Pos2, al: Align2, t: &str, size: f32) -> Rect {
        self.p.text(pos, al, t, self.font(size), self.text)
    }
    /// Text rotated a quarter turn counter-clockwise, centred on `c`.
    fn vtext(&self, c: Pos2, t: &str, size: f32) -> Rect {
        let g = self.p.layout_no_wrap(t.to_string(), self.font(size), self.text);
        let sz = g.size();
        let pos = Pos2::new(c.x - sz.y / 2.0, c.y + sz.x / 2.0);
        self.p.add(TextShape::new(pos, g, self.text).with_angle(-PI / 2.0));
        Rect::from_center_size(c, Vec2::new(sz.y, sz.x))
    }
    fn hit(&mut self, r: Rect, e: Elem) {
        self.hits.push((r, e));
    }
}

/// Paints a chart into `rect` at zoom `z`.
pub fn draw_chart(p: &Painter, rect: Rect, ch: &Chart, data: &ChartData, z: f32) {
    draw_chart_hits(p, rect, ch, data, z);
}

/// Paints a chart and returns the screen areas of its elements (later entries on top).
pub fn draw_chart_hits(p: &Painter, rect: Rect, ch: &Chart, data: &ChartData, z: f32) -> Hits {
    let bg = ch.bg.unwrap_or(0xFFFFFF);
    let dark = chart::is_dark(bg);
    let mut d = D {
        p,
        s: z,
        text: rgb(if dark { TEXT_ON_DARK } else { TEXT }),
        grid: rgb(if dark { GRID_ON_DARK } else { GRID }),
        hits: vec![(rect, Elem::ChartArea)],
    };
    let s = z;
    p.rect_filled(rect, 0.0, rgb(bg));
    p.rect_stroke(rect, 0.0, Stroke::new(1.0, rgb(BORDER)), StrokeKind::Inside);
    let mut inner = rect.shrink(10.0 * s);
    if let Some(t) = chart::display_title(ch, data) {
        let r = d.text(Pos2::new(rect.center().x, inner.top()), Align2::CENTER_TOP, &t, 18.7);
        d.hit(r, Elem::Title);
        inner.min.y = r.bottom() + 6.0 * s;
    }
    if ch.legend {
        let entries = legend_entries(ch, data);
        draw_legend(&mut d, &mut inner, &entries, ch.legend_pos);
    }
    if inner.width() < 24.0 || inner.height() < 24.0 || data.series.is_empty() {
        return d.hits;
    }
    match ch.kind {
        ChartKind::Pie | ChartKind::Doughnut => draw_round(&mut d, inner, ch, data),
        ChartKind::Radar => draw_radar(&mut d, inner, ch, data),
        _ => draw_xy(&mut d, inner, ch, data),
    }
    d.hits
}

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

pub fn quad(m: &mut Mesh, a: Pos2, b: Pos2, c: Pos2, e: Pos2, col: Color32) {
    let i = m.vertices.len() as u32;
    for q in [a, b, c, e] {
        m.colored_vertex(q, col);
    }
    m.add_triangle(i, i + 1, i + 2);
    m.add_triangle(i, i + 2, i + 3);
}

/// Filled ring segment (a pie slice when `r0` is zero) between angles a and b.
pub fn arc_band(p: &Painter, c: Pos2, r0: f32, r1: f32, a: f32, b: f32, col: Color32) {
    let steps = (((b - a).abs() / TAU) * 120.0).ceil().max(2.0) as usize;
    let pt = |r: f32, t: f32| c + Vec2::new(t.cos(), t.sin()) * r;
    let mut m = Mesh::default();
    for k in 0..steps {
        let t0 = a + (b - a) * k as f32 / steps as f32;
        let t1 = a + (b - a) * (k + 1) as f32 / steps as f32;
        quad(&mut m, pt(r0, t0), pt(r1, t0), pt(r1, t1), pt(r0, t1), col);
    }
    p.add(Shape::mesh(m));
}

/// Polyline with an optional dash pattern.
fn stroke_path(p: &Painter, pts: &[Pos2], width: f32, col: Color32, dash: Dash) {
    if pts.len() < 2 {
        return;
    }
    let pat = dash.pattern();
    if pat.is_empty() {
        p.add(Shape::line(pts.to_vec(), Stroke::new(width, col)));
        return;
    }
    let lens: Vec<f32> = pat.iter().map(|k| k * width.max(1.0)).collect();
    let (mut k, mut left, mut on) = (0usize, lens[0], true);
    let mut cur: Vec<Pos2> = vec![pts[0]];
    for w in pts.windows(2) {
        let (mut a, b) = (w[0], w[1]);
        let mut seg = (b - a).length();
        while seg > 0.0 {
            let step = left.min(seg);
            let q = a + (b - a).normalized() * step;
            if on {
                cur.push(q);
            }
            seg -= step;
            left -= step;
            a = q;
            if left <= 1e-3 {
                if on && cur.len() > 1 {
                    p.add(Shape::line(std::mem::take(&mut cur), Stroke::new(width, col)));
                }
                cur.clear();
                on = !on;
                k = (k + 1) % lens.len();
                left = lens[k];
                if on {
                    cur.push(a);
                }
            }
        }
    }
    if on && cur.len() > 1 {
        p.add(Shape::line(cur, Stroke::new(width, col)));
    }
}

/// Catmull-Rom interpolation through the points (smoothed lines).
fn smooth(pts: &[Pos2]) -> Vec<Pos2> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut out = Vec::with_capacity(pts.len() * 10);
    for i in 0..pts.len() - 1 {
        let p0 = if i == 0 { pts[0] } else { pts[i - 1] };
        let (p1, p2) = (pts[i], pts[i + 1]);
        let p3 = if i + 2 < pts.len() { pts[i + 2] } else { p2 };
        for k in 0..10 {
            let t = k as f32 / 10.0;
            let (t2, t3) = (t * t, t * t * t);
            let f = |a: f32, b: f32, c: f32, e: f32| 0.5 * (2.0 * b + (-a + c) * t + (2.0 * a - 5.0 * b + 4.0 * c - e) * t2 + (-a + 3.0 * b - 3.0 * c + e) * t3);
            out.push(Pos2::new(f(p0.x, p1.x, p2.x, p3.x), f(p0.y, p1.y, p2.y, p3.y)));
        }
    }
    out.push(*pts.last().unwrap());
    out
}

pub fn draw_marker(p: &Painter, at: Pos2, kind: MarkerKind, r: f32, fill: Color32, line: Color32) {
    let st = Stroke::new((r * 0.25).clamp(0.75, 1.5), line);
    let poly = |pts: Vec<Pos2>| {
        p.add(Shape::convex_polygon(pts, fill, st));
    };
    match kind {
        MarkerKind::Auto | MarkerKind::Circle => {
            p.circle_filled(at, r, fill);
            p.circle_stroke(at, r, st);
        }
        MarkerKind::None => {}
        MarkerKind::Square => {
            p.rect_filled(Rect::from_center_size(at, Vec2::splat(r * 1.8)), 0.0, fill);
            p.rect_stroke(Rect::from_center_size(at, Vec2::splat(r * 1.8)), 0.0, st, StrokeKind::Middle);
        }
        MarkerKind::Diamond => poly(vec![at + Vec2::new(0.0, -r * 1.2), at + Vec2::new(r * 1.2, 0.0), at + Vec2::new(0.0, r * 1.2), at + Vec2::new(-r * 1.2, 0.0)]),
        MarkerKind::Triangle => poly(vec![at + Vec2::new(0.0, -r * 1.2), at + Vec2::new(r * 1.1, r * 0.8), at + Vec2::new(-r * 1.1, r * 0.8)]),
        MarkerKind::X | MarkerKind::Plus | MarkerKind::Star | MarkerKind::Dash => {
            let s = Stroke::new((r * 0.45).max(1.0), line);
            let d = r * 0.9;
            if matches!(kind, MarkerKind::X | MarkerKind::Star) {
                p.line_segment([at + Vec2::new(-d, -d), at + Vec2::new(d, d)], s);
                p.line_segment([at + Vec2::new(-d, d), at + Vec2::new(d, -d)], s);
            }
            if matches!(kind, MarkerKind::Plus | MarkerKind::Star) {
                p.line_segment([at + Vec2::new(0.0, -d * 1.1), at + Vec2::new(0.0, d * 1.1)], s);
            }
            if matches!(kind, MarkerKind::Plus | MarkerKind::Dash) {
                p.line_segment([at + Vec2::new(-d * 1.1, 0.0), at + Vec2::new(d * 1.1, 0.0)], s);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Series styling
// ---------------------------------------------------------------------------

/// Marker of series `i`, `None` when it has no markers.
pub fn series_marker(ch: &Chart, i: usize) -> Option<MarkerKind> {
    ch.marker(i)
}

/// Whether series `i` is drawn with a connecting line.
fn series_has_line(ch: &Chart, i: usize) -> bool {
    match ch.series_kind(i) {
        ChartKind::Scatter => ch.lines,
        ChartKind::Line | ChartKind::Radar => true,
        _ => false,
    }
}

fn line_width(ch: &Chart, i: usize, s: f32) -> f32 {
    ch.fmt_ref(i).and_then(|f| f.width).unwrap_or(ch.series_kind(i).default_width()) * PT * s
}

fn marker_radius(ch: &Chart, i: usize, s: f32) -> f32 {
    let size = ch.fmt_ref(i).map(|f| f.marker_size).filter(|m| *m > 0).unwrap_or(5) as f32;
    size * PT * s / 2.0
}

enum Swatch {
    Box(Color32, Option<Color32>),
    Line { col: Color32, width: f32, dash: Dash, marker: Option<(MarkerKind, Color32)> },
}

fn legend_entries(ch: &Chart, data: &ChartData) -> Vec<(String, Swatch)> {
    if ch.kind.is_round() {
        let f = ch.fmt(0);
        return data
            .categories
            .iter()
            .enumerate()
            .map(|(i, c)| (c.clone(), Swatch::Box(rgb(f.point(i).and_then(|p| p.fill).unwrap_or_else(|| ch.color(i))), None)))
            .collect();
    }
    let mut out = Vec::new();
    let mut trends = Vec::new();
    for (i, ser) in data.series.iter().enumerate() {
        let f = ch.fmt(i);
        let sw = if matches!(ch.series_kind(i), ChartKind::Column | ChartKind::Bar | ChartKind::Area) || (ch.kind == ChartKind::Radar && ch.filled) {
            Swatch::Box(rgb(ch.series_fill(i)), f.line.map(rgb))
        } else {
            let col = rgb(ch.series_line(i));
            let marker = series_marker(ch, i).map(|m| (m, rgb(ch.series_fill(i))));
            let width = if series_has_line(ch, i) { ch.series_kind(i).default_width() * PT } else { 0.0 };
            Swatch::Line { col, width, dash: f.dash, marker }
        };
        out.push((ser.name.clone(), sw));
        if let Some(t) = &f.trend {
            let name = t.name.clone().unwrap_or_else(|| t.kind.legend(t, &ser.name));
            trends.push((name, Swatch::Line { col: rgb(ch.series_line(i)), width: 1.5 * PT, dash: Dash::RoundDot, marker: None }));
        }
    }
    out.extend(trends);
    out
}

fn draw_swatch(d: &D, sw: &Swatch, x: f32, cy: f32, w: f32) {
    let s = d.s;
    match sw {
        Swatch::Box(c, line) => {
            let r = Rect::from_center_size(Pos2::new(x + w / 2.0, cy), Vec2::splat(7.0 * s));
            d.p.rect_filled(r, 0.0, *c);
            if let Some(l) = line {
                d.p.rect_stroke(r, 0.0, Stroke::new(1.0, *l), StrokeKind::Inside);
            }
        }
        Swatch::Line { col, width, dash, marker } => {
            if *width > 0.0 {
                stroke_path(d.p, &[Pos2::new(x, cy), Pos2::new(x + w, cy)], (width * s).min(3.0 * s), *col, *dash);
            }
            if let Some((m, fill)) = marker {
                draw_marker(d.p, Pos2::new(x + w / 2.0, cy), *m, 3.3 * s, *fill, *col);
            }
        }
    }
}

/// Lays out and paints the legend, shrinking `area` by the space it takes.
fn draw_legend(d: &mut D, area: &mut Rect, entries: &[(String, Swatch)], pos: LegendPos) {
    if entries.is_empty() {
        return;
    }
    let s = d.s;
    let size = 12.0;
    let sw = 14.0 * s;
    let gap = 12.0 * s;
    let line_h = d.size("Ag", size).y + 2.0 * s;
    let items: Vec<(f32, usize)> = entries.iter().take(200).enumerate().map(|(i, (t, _))| (sw + 4.0 * s + d.size(t, size).x, i)).collect();
    let draw_item = |d: &D, i: usize, x: f32, cy: f32| {
        draw_swatch(d, &entries[i].1, x, cy, sw);
        d.text(Pos2::new(x + sw + 4.0 * s, cy), Align2::LEFT_CENTER, &entries[i].0, size);
    };
    match pos {
        LegendPos::Bottom | LegendPos::Top => {
            let mut rows: Vec<Vec<(f32, usize)>> = vec![Vec::new()];
            let mut w = 0.0;
            for it in items {
                if w > 0.0 && w + gap + it.0 > area.width() {
                    rows.push(Vec::new());
                    w = 0.0;
                }
                w += if w > 0.0 { gap } else { 0.0 } + it.0;
                rows.last_mut().unwrap().push(it);
            }
            let rows: Vec<_> = rows.into_iter().take(((area.height() * 0.4) / line_h).max(1.0) as usize).collect();
            let total = rows.len() as f32 * line_h;
            let mut y = if pos == LegendPos::Bottom { area.bottom() - total } else { area.top() };
            let mut bounds = Rect::NOTHING;
            for row in rows {
                let rw: f32 = row.iter().map(|i| i.0).sum::<f32>() + gap * (row.len().saturating_sub(1)) as f32;
                let mut x = area.center().x - rw / 2.0;
                let cy = y + line_h / 2.0;
                for (iw, i) in row {
                    draw_item(d, i, x, cy);
                    bounds = bounds.union(Rect::from_min_size(Pos2::new(x, y), Vec2::new(iw, line_h)));
                    x += iw + gap;
                }
                y += line_h;
            }
            d.hit(bounds, Elem::Legend);
            if pos == LegendPos::Bottom {
                area.max.y -= total + 4.0 * s;
            } else {
                area.min.y += total + 6.0 * s;
            }
        }
        LegendPos::Left | LegendPos::Right | LegendPos::TopRight => {
            let w = items.iter().map(|i| i.0).fold(0.0, f32::max).min(area.width() * 0.4);
            let n = items.len().min((area.height() / line_h).max(1.0) as usize);
            let total = n as f32 * line_h;
            let x = if pos == LegendPos::Left { area.left() } else { area.right() - w };
            let mut y = if pos == LegendPos::TopRight { area.top() } else { area.center().y - total / 2.0 };
            let top = y;
            for (_, i) in items.iter().take(n) {
                draw_item(d, *i, x, y + line_h / 2.0);
                y += line_h;
            }
            d.hit(Rect::from_min_size(Pos2::new(x, top), Vec2::new(w, total)), Elem::Legend);
            if pos == LegendPos::Left {
                area.min.x += w + 8.0 * s;
            } else {
                area.max.x -= w + 8.0 * s;
            }
        }
    }
}

/// Text of a data label.
fn label_text(l: &Labels, v: f64, cat: &str, ser: &str, pct: Option<f64>, fmt: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if l.series {
        parts.push(ser.to_string());
    }
    if l.category {
        parts.push(cat.to_string());
    }
    if l.value {
        parts.push(chart::format_number(v, if l.num_fmt.is_empty() { fmt } else { &l.num_fmt }, 0.0));
    }
    if l.percent {
        if let Some(p) = pct {
            parts.push(chart::format_number(p, "0%", 0.0));
        }
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

// ---------------------------------------------------------------------------
// Pie and doughnut
// ---------------------------------------------------------------------------

fn draw_round(d: &mut D, area: Rect, ch: &Chart, data: &ChartData) {
    let s = d.s;
    let pie = ch.kind == ChartKind::Pie;
    let rings: Vec<usize> = if pie { vec![0] } else { (0..data.series.len()).collect() };
    let fmt0 = ch.fmt(0);
    let max_explode = if pie { fmt0.points.iter().map(|p| p.explode).chain([fmt0.explode]).max().unwrap_or(0) } else { ch.fmt(rings.len().saturating_sub(1)).explode };
    let outside = pie && matches!(ch.series_labels(0).pos, LabelPos::OutsideEnd);
    let margin = if outside { d.size("Ag", 12.0).y * 1.6 } else { 2.0 * s };
    let r = ((area.width().min(area.height()) / 2.0 - margin) / (1.0 + max_explode.min(400) as f32 / 100.0)).max(4.0);
    let c = area.center();
    let hole = if pie { 0.0 } else { r * ch.hole.clamp(10, 90) as f32 / 100.0 };
    let band = (r - hole) / rings.len().max(1) as f32;
    let sep = Stroke::new(1.5 * s, Color32::WHITE);
    let start = -PI / 2.0 + (ch.first_angle % 360) as f32 * PI / 180.0;
    d.hit(Rect::from_center_size(c, Vec2::splat(r * 2.0)), Elem::PlotArea);
    for (k, &si) in rings.iter().enumerate() {
        let ser = &data.series[si];
        let f = ch.fmt(si);
        let labels = ch.series_labels(si).clone();
        let (r0, r1) = (hole + band * k as f32, hole + band * (k + 1) as f32);
        let total: f64 = ser.values.iter().flatten().map(|v| v.abs()).sum();
        if total <= 0.0 {
            continue;
        }
        let mut a = start;
        for (i, v) in ser.values.iter().enumerate() {
            let v = v.map(f64::abs).unwrap_or(0.0);
            if v <= 0.0 {
                continue;
            }
            let b = a + (v / total) as f32 * TAU;
            let mid = (a + b) / 2.0;
            let dir = Vec2::new(mid.cos(), mid.sin());
            let pf = f.point(i);
            let explode = pf.map(|p| p.explode).filter(|e| *e > 0).unwrap_or(f.explode);
            let off = if pie || k + 1 == rings.len() { dir * r * explode.min(400) as f32 / 100.0 } else { Vec2::ZERO };
            let cc = c + off;
            let col = rgb(pf.and_then(|p| p.fill).unwrap_or_else(|| ch.color(i)));
            arc_band(d.p, cc, r0, r1, a, b, col);
            for t in [a, b] {
                let e = Vec2::new(t.cos(), t.sin());
                d.p.line_segment([cc + e * r0, cc + e * r1], sep);
            }
            let rr = (r0 + r1) / 2.0;
            d.hit(Rect::from_center_size(cc + dir * rr, Vec2::splat(band.min(r1 * (b - a)).max(6.0))), Elem::Series(si));
            if labels.any() {
                let pct = v / total;
                if let Some(t) = label_text(&labels, v, data.categories.get(i).map(String::as_str).unwrap_or(""), &ser.name, Some(pct), &data.num_fmt) {
                    let (pos, al) = match labels.pos {
                        _ if !pie => (cc + dir * rr, Align2::CENTER_CENTER),
                        LabelPos::Center => (cc + dir * r1 * 0.5, Align2::CENTER_CENTER),
                        LabelPos::InsideEnd => (cc + dir * r1 * 0.8, Align2::CENTER_CENTER),
                        LabelPos::OutsideEnd => {
                            let al = Align2([
                                if dir.x > 0.3 { eframe::egui::Align::Min } else if dir.x < -0.3 { eframe::egui::Align::Max } else { eframe::egui::Align::Center },
                                if dir.y > 0.3 { eframe::egui::Align::Min } else if dir.y < -0.3 { eframe::egui::Align::Max } else { eframe::egui::Align::Center },
                            ]);
                            (cc + dir * (r1 + 4.0 * s), al)
                        }
                        _ => (cc + dir * r1 * 0.65, Align2::CENTER_CENTER),
                    };
                    d.text(pos, al, &t, 12.0);
                }
            }
            a = b;
        }
        if k > 0 {
            d.p.circle_stroke(c, r0, sep);
        }
    }
}

// ---------------------------------------------------------------------------
// Radar
// ---------------------------------------------------------------------------

fn draw_radar(d: &mut D, area: Rect, ch: &Chart, data: &ChartData) {
    let s = d.s;
    let n = data.points();
    if n < 1 {
        return;
    }
    let vals = data.series.iter().flat_map(|x| x.values.iter().flatten().copied());
    let (vmin, vmax) = vals.fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| (a.min(v), b.max(v)));
    let sc = chart::scale_for(vmin, vmax, &Axis { log_base: None, ..ch.y_axis.clone() }, false);
    let fmt = if ch.y_axis.num_fmt.is_empty() { data.num_fmt.clone() } else { ch.y_axis.num_fmt.clone() };
    let lab_h = d.size("Ag", 12.0).y;
    let cat_w = data.categories.iter().map(|c| d.size(c, 12.0).x).fold(0.0, f32::max).min(area.width() * 0.25);
    let r = ((area.width() / 2.0 - cat_w - 6.0 * s).min(area.height() / 2.0 - lab_h - 4.0 * s)).max(8.0);
    let c = area.center();
    let ang = |i: usize| -PI / 2.0 + TAU * i as f32 / n as f32;
    let at = |i: usize, v: f64| c + Vec2::new(ang(i).cos(), ang(i).sin()) * r * sc.frac(sc.clamp(v)).clamp(0.0, 1.0) as f32;
    d.hit(Rect::from_center_size(c, Vec2::splat(r * 2.0)), Elem::PlotArea);
    let gs = Stroke::new(1.0, d.grid);
    if ch.y_axis.major_grid {
        for t in sc.ticks() {
            let ring: Vec<Pos2> = (0..=n).map(|i| at(i % n, t)).collect();
            d.p.add(Shape::line(ring, gs));
        }
    }
    for i in 0..n {
        d.p.line_segment([c, at(i, sc.hi)], gs);
        let dir = Vec2::new(ang(i).cos(), ang(i).sin());
        let al = Align2([
            if dir.x > 0.2 { eframe::egui::Align::Min } else if dir.x < -0.2 { eframe::egui::Align::Max } else { eframe::egui::Align::Center },
            if dir.y > 0.2 { eframe::egui::Align::Min } else if dir.y < -0.2 { eframe::egui::Align::Max } else { eframe::egui::Align::Center },
        ]);
        if ch.x_axis.visible {
            d.text(c + dir * (r + 5.0 * s), al, data.categories.get(i).map(String::as_str).unwrap_or(""), 12.0);
        }
    }
    if ch.y_axis.visible {
        for t in sc.ticks() {
            let q = at(0, t);
            d.text(q + Vec2::new(-4.0 * s, 0.0), Align2::RIGHT_CENTER, &chart::format_number(t, &fmt, sc.major), 12.0);
        }
    }
    for (k, ser) in data.series.iter().enumerate() {
        let pts: Vec<Pos2> = (0..n).map(|i| at(i, ser.values.get(i).copied().flatten().unwrap_or(sc.lo))).collect();
        let lc = rgb(ch.series_line(k));
        if ch.filled {
            let mut m = Mesh::default();
            let col = rgb(ch.series_fill(k));
            for i in 0..n {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                quad(&mut m, c, a, b, b, col);
            }
            d.p.add(Shape::mesh(m));
        } else {
            let mut ring = pts.clone();
            ring.push(pts[0]);
            stroke_path(d.p, &ring, line_width(ch, k, s), lc, ch.fmt(k).dash);
        }
        if let Some(m) = series_marker(ch, k) {
            for q in &pts {
                draw_marker(d.p, *q, m, marker_radius(ch, k, s), rgb(ch.series_fill(k)), lc);
            }
        }
        for q in &pts {
            d.hit(Rect::from_center_size(*q, Vec2::splat(8.0 * s)), Elem::Series(k));
        }
        let labels = ch.series_labels(k);
        if labels.any() {
            for (i, v) in ser.values.iter().enumerate().take(n) {
                if let Some(v) = v {
                    if let Some(t) = label_text(labels, *v, &data.categories[i], &ser.name, None, &data.num_fmt) {
                        d.text(pts[i] - Vec2::new(0.0, 5.0 * s), Align2::CENTER_BOTTOM, &t, 12.0);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Charts with axes
// ---------------------------------------------------------------------------

/// Drawn extent of one series point: from `base` (None = axis base) to `top`.
#[derive(Clone, Copy, Default)]
struct Span {
    base: Option<f64>,
    top: Option<f64>,
    /// The raw value (labels).
    raw: Option<f64>,
}

/// Stacks the series of each (axis, kind) group with the chart's grouping.
fn stack(ch: &Chart, data: &ChartData) -> Vec<Vec<Span>> {
    let n = data.points();
    let mut out: Vec<Vec<Span>> = data.series.iter().map(|s| (0..n).map(|i| Span { base: None, top: s.values.get(i).copied().flatten(), raw: s.values.get(i).copied().flatten() }).collect()).collect();
    if ch.kind == ChartKind::Scatter {
        return out;
    }
    let mut groups: Vec<((bool, ChartKind), Vec<usize>)> = Vec::new();
    for i in 0..data.series.len() {
        if ch.series_grouping(i) == Grouping::Standard {
            continue;
        }
        let key = (ch.on_secondary(i), ch.series_kind(i));
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => g.1.push(i),
            None => groups.push((key, vec![i])),
        }
    }
    for ((_, kind), members) in groups {
        let percent = ch.grouping == Grouping::Percent;
        for p in 0..n {
            let total: f64 = members.iter().filter_map(|&i| data.series[i].values.get(p).copied().flatten()).map(f64::abs).sum();
            let norm = |v: f64| if percent { if total > 0.0 { v / total } else { 0.0 } } else { v };
            let (mut pos, mut neg) = (0.0, 0.0);
            for &i in &members {
                let v = data.series[i].values.get(p).copied().flatten();
                let sp = &mut out[i][p];
                let nv = norm(v.unwrap_or(0.0));
                if kind.is_bar() {
                    let acc = if nv >= 0.0 { &mut pos } else { &mut neg };
                    sp.base = Some(*acc);
                    *acc += nv;
                    sp.top = v.map(|_| *acc);
                } else {
                    sp.base = Some(pos);
                    pos += nv;
                    sp.top = if v.is_some() || kind == ChartKind::Area { Some(pos) } else { None };
                }
            }
        }
    }
    out
}

/// Error bar extent (low, high) for a value.
fn err_range(eb: &chart::ErrorBars, v: f64, sd: f64, se: f64, mean: f64) -> (f64, f64) {
    let (centre, amt) = match eb.kind {
        ErrKind::Fixed => (v, eb.amount.abs()),
        ErrKind::Percent => (v, v.abs() * eb.amount.abs() / 100.0),
        ErrKind::StdDev => (mean, sd * eb.amount.abs()),
        ErrKind::StdErr => (v, se),
    };
    let lo = if eb.dir == ErrDir::Plus { v } else { centre - amt };
    let hi = if eb.dir == ErrDir::Minus { v } else { centre + amt };
    (lo, hi)
}

fn axis_fmt(ax: &Axis, percent: bool, src: &str) -> String {
    if !ax.num_fmt.is_empty() {
        ax.num_fmt.clone()
    } else if percent {
        "0%".into()
    } else {
        src.to_string()
    }
}

fn draw_xy(d: &mut D, area: Rect, ch: &Chart, data: &ChartData) {
    let s = d.s;
    let font = 12.0;
    let horizontal = ch.kind == ChartKind::Bar;
    let xy = ch.kind == ChartKind::Scatter;
    let n = data.points();
    if n == 0 {
        return;
    }
    let nser = data.series.len();
    let spans = stack(ch, data);
    let secondary = ch.has_secondary(nser);
    let percent = |sec: bool| (0..nser).any(|i| ch.on_secondary(i) == sec && ch.series_grouping(i) == Grouping::Percent);

    // Per-series statistics for error bars.
    let stats: Vec<(f64, f64, f64)> = data
        .series
        .iter()
        .map(|ser| {
            let v: Vec<f64> = ser.values.iter().flatten().copied().collect();
            let (sd, se) = trend::std_dev_err(&v);
            (sd, se, if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 })
        })
        .collect();

    // Value ranges of the two value axes.
    let range_of = |sec: bool| -> (f64, f64) {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in (0..nser).filter(|i| ch.on_secondary(*i) == sec) {
            let eb = ch.fmt_ref(i).and_then(|f| f.err.clone());
            for sp in &spans[i] {
                for v in [sp.top, sp.base].into_iter().flatten() {
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
                if let (Some(eb), Some(v)) = (&eb, sp.top) {
                    let (a, b) = err_range(eb, v, stats[i].0, stats[i].1, stats[i].2);
                    lo = lo.min(a);
                    hi = hi.max(b);
                }
            }
        }
        if lo.is_finite() { (lo, hi) } else { (0.0, 0.0) }
    };
    let (a, b) = range_of(false);
    let ys = chart::scale_for(a, b, &ch.y_axis, percent(false));
    let ys2 = secondary.then(|| {
        let (a, b) = range_of(true);
        chart::scale_for(a, b, &ch.y2_axis, percent(true))
    });
    let yfmt = axis_fmt(&ch.y_axis, percent(false), &data.num_fmt);
    let y2fmt = axis_fmt(&ch.y2_axis, percent(true), &data.num_fmt);
    let tick_labels = |sc: &Scale, f: &str| -> Vec<(f64, String)> { sc.ticks().into_iter().map(|v| (v, chart::format_number(v, f, sc.major))).collect() };
    let vl = if ch.y_axis.visible { tick_labels(&ys, &yfmt) } else { Vec::new() };
    let vl2 = match (&ys2, ch.y2_axis.visible) {
        (Some(sc), true) => tick_labels(sc, &y2fmt),
        _ => Vec::new(),
    };

    // Scatter X scale.
    let xs_of = |k: usize| -> Vec<Option<f64>> {
        match &data.series[k].xs {
            Some(x) => x.clone(),
            None => (0..n).map(|i| data.cat_values.get(i).copied().flatten().or(Some(i as f64 + 1.0))).collect(),
        }
    };
    let xsc = xy.then(|| {
        let all: Vec<f64> = (0..nser).flat_map(xs_of).flatten().collect();
        let a = all.iter().copied().fold(f64::INFINITY, f64::min);
        let b = all.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        chart::scale_for(a, b, &ch.x_axis, false)
    });
    let xfmt = if ch.x_axis.num_fmt.is_empty() { String::new() } else { ch.x_axis.num_fmt.clone() };
    let xl: Vec<(f64, String)> = match (&xsc, ch.x_axis.visible) {
        (Some(sc), true) => tick_labels(sc, &xfmt),
        _ => Vec::new(),
    };

    let line_h = d.size("Ag", font).y;
    let tsize = 13.3;
    let title_h = d.size("Ag", tsize).y + 4.0 * s;
    let maxw = |l: &[(f64, String)]| l.iter().map(|(_, t)| d.size(t, font).x).fold(0.0, f32::max);
    let table = ch.data_table && !xy;
    let cat_w = data.categories.iter().map(|l| d.size(l, font).x).fold(0.0, f32::max);

    // Space around the plot: the axis on each side and its title.
    let (left_axis_w, bottom_axis_h) = if horizontal {
        (if ch.x_axis.visible { cat_w.min(area.width() * 0.4) } else { 0.0 }, if vl.is_empty() { 0.0 } else { line_h })
    } else {
        (maxw(&vl), if table || !ch.x_axis.visible { 0.0 } else { line_h })
    };
    let (left_title, bottom_title) = if horizontal { (ch.x_axis.title_text(), ch.y_axis.title_text()) } else { (ch.y_axis.title_text(), ch.x_axis.title_text()) };
    let right_axis_w = if horizontal { 0.0 } else { maxw(&vl2) };
    let top_axis_h = if horizontal && !vl2.is_empty() { line_h + 4.0 * s } else { 0.0 };
    let right_title = ch.y2_axis.title_text().filter(|_| secondary);
    let table_rows = if table { nser + 1 } else { 0 };
    let row_h = line_h + 4.0 * s;
    let name_col = if table { data.series.iter().map(|x| d.size(&x.name, font).x).fold(0.0, f32::max) + 22.0 * s } else { 0.0 };

    let mut plot = area;
    plot.min.x += left_axis_w + if left_axis_w > 0.0 { 6.0 * s } else { 0.0 } + if left_title.is_some() { title_h } else { 0.0 };
    plot.min.x = plot.min.x.max(area.left() + name_col);
    plot.max.x -= right_axis_w + if right_axis_w > 0.0 { 6.0 * s } else { 4.0 * s } + if right_title.is_some() { title_h } else { 0.0 };
    if horizontal && !vl.is_empty() {
        plot.max.x -= maxw(&vl) / 2.0;
    }
    plot.min.y += line_h / 2.0 + top_axis_h;
    plot.max.y -= bottom_axis_h + 6.0 * s + if bottom_title.is_some() { title_h } else { 0.0 } + table_rows as f32 * row_h;
    if xy && !xl.is_empty() {
        plot.max.x -= maxw(&xl) / 2.0;
    }
    if plot.width() < 8.0 || plot.height() < 8.0 {
        return;
    }
    if let Some(bg) = ch.plot_bg {
        d.p.rect_filled(plot, 0.0, rgb(bg));
    }
    d.hit(plot, Elem::PlotArea);

    // Value position along the value axis.
    let vpos = |sc: &Scale, v: f64| -> f32 {
        let f = sc.frac(v) as f32;
        if horizontal { plot.left() + f * plot.width() } else { plot.bottom() - f * plot.height() }
    };
    let scale_of = |i: usize| -> &Scale { if ch.on_secondary(i) { ys2.as_ref().unwrap_or(&ys) } else { &ys } };
    let xpos = |x: f64| -> f32 { xsc.as_ref().map_or(plot.left(), |sc| plot.left() + sc.frac(x) as f32 * plot.width()) };

    // Category positions.
    let on_ticks = n > 1 && (0..nser).all(|i| ch.series_kind(i) == ChartKind::Area);
    let len = if horizontal { plot.height() } else { plot.width() };
    let slot = len / n as f32;
    let crev = ch.x_axis.reverse;
    // Continuous category coordinate: 0 = first category.
    let cposf = |f: f32| -> f32 {
        let f = if crev { (n - 1) as f32 - f } else { f };
        let along = if on_ticks { f * len / (n - 1) as f32 } else { (f + 0.5) * slot };
        if horizontal { plot.bottom() - along } else { plot.left() + along }
    };
    let cpos = |i: usize| cposf(i as f32);

    let gs = Stroke::new(1.0, d.grid);
    let ms = Stroke::new(1.0, rgb(MINOR));
    let crisp = |v: f32| v.round() + 0.5;
    let vline = |p: &Painter, q: f32, st: Stroke| p.line_segment([Pos2::new(crisp(q), plot.top()), Pos2::new(crisp(q), plot.bottom())], st);
    let hline = |p: &Painter, q: f32, st: Stroke| p.line_segment([Pos2::new(plot.left(), crisp(q)), Pos2::new(plot.right(), crisp(q))], st);
    let along_value = |p: &Painter, q: f32, st: Stroke| if horizontal { vline(p, q, st) } else { hline(p, q, st) };
    let along_cat = |p: &Painter, q: f32, st: Stroke| if horizontal { hline(p, q, st) } else { vline(p, q, st) };

    // Gridlines.
    if ch.y_axis.minor_grid {
        for v in ys.minor_ticks() {
            along_value(d.p, vpos(&ys, v), ms);
        }
    }
    if let (Some(sc), true) = (&ys2, ch.y2_axis.minor_grid) {
        for v in sc.minor_ticks() {
            along_value(d.p, vpos(sc, v), ms);
        }
    }
    if let Some(sc) = &xsc {
        if ch.x_axis.minor_grid {
            for v in sc.minor_ticks() {
                vline(d.p, xpos(v), ms);
            }
        }
        if ch.x_axis.major_grid {
            for v in sc.ticks() {
                vline(d.p, xpos(v), gs);
            }
        }
    } else if ch.x_axis.major_grid || ch.x_axis.minor_grid {
        let k = if on_ticks { n } else { n + 1 };
        for i in 0..k {
            let f = if on_ticks { i as f32 } else { i as f32 - 0.5 };
            along_cat(d.p, cposf(f), gs);
        }
    }
    if ch.y_axis.major_grid {
        for v in ys.ticks() {
            along_value(d.p, vpos(&ys, v), gs);
        }
    }
    if let (Some(sc), true) = (&ys2, ch.y2_axis.major_grid) {
        for v in sc.ticks() {
            along_value(d.p, vpos(sc, v), gs);
        }
    }

    // Value axis labels.
    let mut yr = Rect::NOTHING;
    for (v, l) in &vl {
        let q = crisp(vpos(&ys, *v));
        yr = yr.union(if horizontal {
            d.text(Pos2::new(q, plot.bottom() + 4.0 * s), Align2::CENTER_TOP, l, font)
        } else {
            d.text(Pos2::new(plot.left() - 4.0 * s, q), Align2::RIGHT_CENTER, l, font)
        });
    }
    if yr.is_positive() {
        d.hit(yr, Elem::YAxis);
    }
    if let Some(sc) = &ys2 {
        let mut r2 = Rect::NOTHING;
        for (v, l) in &vl2 {
            let q = crisp(vpos(sc, *v));
            r2 = r2.union(if horizontal {
                d.text(Pos2::new(q, plot.top() - 4.0 * s), Align2::CENTER_BOTTOM, l, font)
            } else {
                d.text(Pos2::new(plot.right() + 4.0 * s, q), Align2::LEFT_CENTER, l, font)
            });
        }
        if r2.is_positive() {
            d.hit(r2, Elem::Y2Axis);
        }
    }

    // Category / X axis labels and the axis line.
    let mut xr = Rect::NOTHING;
    let base_line = ys.base();
    if let Some(sc) = &xsc {
        for (v, l) in &xl {
            xr = xr.union(d.text(Pos2::new(crisp(xpos(*v)), plot.bottom() + 4.0 * s), Align2::CENTER_TOP, l, font));
        }
        let q = crisp(vpos(&ys, base_line));
        d.p.line_segment([Pos2::new(plot.left(), q), Pos2::new(plot.right(), q)], gs);
        let _ = sc;
    } else if horizontal {
        if ch.x_axis.visible {
            let every = ((line_h + 2.0 * s) / slot).ceil().max(1.0) as usize;
            for (i, l) in data.categories.iter().enumerate().step_by(every) {
                xr = xr.union(d.text(Pos2::new(plot.left() - 4.0 * s, cpos(i)), Align2::RIGHT_CENTER, l, font));
            }
        }
        let q = crisp(vpos(&ys, base_line));
        d.p.line_segment([Pos2::new(q, plot.top()), Pos2::new(q, plot.bottom())], gs);
    } else {
        if ch.x_axis.visible && !table {
            let maxw = data.categories.iter().map(|l| d.size(l, font).x).fold(0.0, f32::max);
            let pitch = if on_ticks { plot.width() / (n - 1) as f32 } else { slot };
            let every = ((maxw + 6.0 * s) / pitch).ceil().max(1.0) as usize;
            for (i, l) in data.categories.iter().enumerate().step_by(every) {
                xr = xr.union(d.text(Pos2::new(cpos(i), plot.bottom() + 4.0 * s), Align2::CENTER_TOP, l, font));
            }
        }
        let q = crisp(vpos(&ys, base_line));
        d.p.line_segment([Pos2::new(plot.left(), q), Pos2::new(plot.right(), q)], gs);
    }
    if xr.is_positive() {
        d.hit(xr, Elem::XAxis);
    }

    // Axis titles.
    if let Some(t) = left_title {
        let r = d.vtext(Pos2::new(area.left() + title_h / 2.0, plot.center().y), t, tsize);
        d.hit(r, if horizontal { Elem::XTitle } else { Elem::YTitle });
    }
    if let Some(t) = bottom_title {
        let y = plot.bottom() + bottom_axis_h + 6.0 * s + table_rows as f32 * row_h;
        let r = d.text(Pos2::new(plot.center().x, y), Align2::CENTER_TOP, t, tsize);
        d.hit(r, if horizontal { Elem::YTitle } else { Elem::XTitle });
    }
    if let Some(t) = right_title {
        let r = d.vtext(Pos2::new(area.right() - title_h / 2.0, plot.center().y), t, tsize);
        d.hit(r, Elem::Y2Title);
    }

    // Series, clipped to the plot area.
    let clip = d.p.clip_rect().intersect(plot.expand(6.0 * s));
    let sp = d.p.with_clip_rect(clip);
    let order = |k: ChartKind| -> u8 {
        match k {
            ChartKind::Area => 0,
            ChartKind::Column | ChartKind::Bar => 1,
            _ => 2,
        }
    };
    let mut idx: Vec<usize> = (0..nser).collect();
    idx.sort_by_key(|&i| order(ch.series_kind(i)));

    // Bar layout: per axis, the bar series and their slot index.
    let bar_layout = |i: usize| -> (usize, usize) {
        let sec = ch.on_secondary(i);
        let bars: Vec<usize> = (0..nser).filter(|&k| ch.series_kind(k).is_bar() && ch.on_secondary(k) == sec).collect();
        (bars.iter().position(|&k| k == i).unwrap_or(0), bars.len().max(1))
    };
    let mut labels_to_draw: Vec<(Pos2, Align2, String)> = Vec::new();

    for &k in &idx {
        let ser = &data.series[k];
        let kind = ch.series_kind(k);
        let sc = scale_of(k);
        let f = ch.fmt(k);
        let fill = rgb(ch.series_fill(k));
        let lc = rgb(ch.series_line(k));
        let labels = ch.series_labels(k).clone();
        let grouping = ch.series_grouping(k);
        let base_v = sc.base();
        let lab = |v: f64, i: usize| label_text(&labels, v, data.categories.get(i).map(String::as_str).unwrap_or(""), &ser.name, None, &data.num_fmt);
        let mut hit = Rect::NOTHING;
        match kind {
            ChartKind::Column | ChartKind::Bar => {
                let (pos, nb) = bar_layout(k);
                let stacked = grouping != Grouping::Standard;
                let o = if stacked { 1.0 } else { ch.overlap.clamp(-100, 100) as f32 / 100.0 };
                let g = ch.gap.min(500) as f32 / 100.0;
                let nf = if stacked { 1.0 } else { nb as f32 };
                let pos = if stacked { 0.0 } else { pos as f32 };
                let bw = slot / (nf - o * (nf - 1.0) + g);
                let group = bw * (nf - o * (nf - 1.0));
                for (i, span) in spans[k].iter().enumerate() {
                    let (Some(top), Some(raw)) = (span.top, span.raw) else { continue };
                    let b0 = vpos(sc, sc.clamp(span.base.unwrap_or(base_v)));
                    let b1 = vpos(sc, sc.clamp(top));
                    let c = cpos(i);
                    let r = if horizontal {
                        let y0 = c + group / 2.0 - pos * bw * (1.0 - o) - bw;
                        Rect::from_min_max(Pos2::new(b0.min(b1), y0), Pos2::new(b0.max(b1), y0 + bw))
                    } else {
                        let x0 = c - group / 2.0 + pos * bw * (1.0 - o);
                        Rect::from_min_max(Pos2::new(x0, b0.min(b1)), Pos2::new(x0 + bw, b0.max(b1)))
                    };
                    let pfill = f.point(i).and_then(|p| p.fill).map(rgb).unwrap_or(fill);
                    sp.rect_filled(r, 0.0, pfill);
                    if let Some(l) = f.line {
                        sp.rect_stroke(r, 0.0, Stroke::new(f.width.unwrap_or(0.75) * PT * s, rgb(l)), StrokeKind::Inside);
                    }
                    hit = hit.union(r);
                    if let Some(t) = lab(raw, i) {
                        let up = (top >= span.base.unwrap_or(base_v)) != sc.reverse;
                        let gap = 3.0 * s;
                        let pos = match labels.pos {
                            LabelPos::Auto if stacked => LabelPos::Center,
                            LabelPos::Auto => LabelPos::OutsideEnd,
                            p => p,
                        };
                        let (at, al) = match (horizontal, pos, up) {
                            (_, LabelPos::Center, _) => (r.center(), Align2::CENTER_CENTER),
                            (true, LabelPos::InsideEnd, true) => (r.right_center() - Vec2::new(gap, 0.0), Align2::RIGHT_CENTER),
                            (true, LabelPos::InsideEnd, false) => (r.left_center() + Vec2::new(gap, 0.0), Align2::LEFT_CENTER),
                            (true, LabelPos::InsideBase, true) => (r.left_center() + Vec2::new(gap, 0.0), Align2::LEFT_CENTER),
                            (true, LabelPos::InsideBase, false) => (r.right_center() - Vec2::new(gap, 0.0), Align2::RIGHT_CENTER),
                            (true, _, true) => (r.right_center() + Vec2::new(gap, 0.0), Align2::LEFT_CENTER),
                            (true, _, false) => (r.left_center() - Vec2::new(gap, 0.0), Align2::RIGHT_CENTER),
                            (false, LabelPos::InsideEnd, true) => (r.center_top() + Vec2::new(0.0, gap), Align2::CENTER_TOP),
                            (false, LabelPos::InsideEnd, false) => (r.center_bottom() - Vec2::new(0.0, gap), Align2::CENTER_BOTTOM),
                            (false, LabelPos::InsideBase, true) => (r.center_bottom() - Vec2::new(0.0, gap), Align2::CENTER_BOTTOM),
                            (false, LabelPos::InsideBase, false) => (r.center_top() + Vec2::new(0.0, gap), Align2::CENTER_TOP),
                            (false, _, true) => (r.center_top() - Vec2::new(0.0, gap), Align2::CENTER_BOTTOM),
                            (false, _, false) => (r.center_bottom() + Vec2::new(0.0, gap), Align2::CENTER_TOP),
                        };
                        labels_to_draw.push((at, al, t));
                    }
                }
            }
            ChartKind::Area => {
                let tops: Vec<f32> = spans[k].iter().map(|sp| vpos(sc, sc.clamp(sp.top.unwrap_or(0.0)))).collect();
                let bases: Vec<f32> = spans[k].iter().map(|sp| vpos(sc, sc.clamp(sp.base.unwrap_or(base_v)))).collect();
                let at = |i: usize, y: f32| if horizontal { Pos2::new(y, cpos(i)) } else { Pos2::new(cpos(i), y) };
                let mut m = Mesh::default();
                if tops.len() == 1 {
                    let x = cpos(0);
                    let w = slot * 0.3;
                    quad(&mut m, Pos2::new(x - w, bases[0]), Pos2::new(x - w, tops[0]), Pos2::new(x + w, tops[0]), Pos2::new(x + w, bases[0]), fill);
                }
                for i in 1..tops.len() {
                    let (y0, y1, b0, b1) = (tops[i - 1], tops[i], bases[i - 1], bases[i]);
                    if (y0 - b0) * (y1 - b1) < 0.0 {
                        let t = ((y0 - b0) / ((y0 - b0) - (y1 - b1))).clamp(0.0, 1.0);
                        let xm = cpos(i - 1) + (cpos(i) - cpos(i - 1)) * t;
                        let bm = b0 + (b1 - b0) * t;
                        let pm = if horizontal { Pos2::new(bm, xm) } else { Pos2::new(xm, bm) };
                        quad(&mut m, at(i - 1, b0), at(i - 1, y0), pm, pm, fill);
                        quad(&mut m, pm, pm, at(i, y1), at(i, b1), fill);
                    } else {
                        quad(&mut m, at(i - 1, b0), at(i - 1, y0), at(i, y1), at(i, b1), fill);
                    }
                }
                sp.add(Shape::mesh(m));
                if let Some(l) = f.line {
                    let pts: Vec<Pos2> = (0..tops.len()).map(|i| at(i, tops[i])).collect();
                    stroke_path(&sp, &pts, f.width.unwrap_or(0.75) * PT * s, rgb(l), f.dash);
                }
                for i in 0..tops.len() {
                    hit = hit.union(Rect::from_center_size(at(i, tops[i]), Vec2::splat(6.0 * s)));
                    if let Some(t) = spans[k][i].raw.and_then(|v| lab(v, i)) {
                        labels_to_draw.push((at(i, tops[i]) - Vec2::new(0.0, 3.0 * s), Align2::CENTER_BOTTOM, t));
                    }
                }
            }
            ChartKind::Line | ChartKind::Scatter => {
                let xsk = xs_of(k);
                let pts: Vec<Option<Pos2>> = spans[k]
                    .iter()
                    .enumerate()
                    .map(|(i, sp)| {
                        let v = sp.top?;
                        let y = vpos(sc, v);
                        if xy {
                            let x = xsk.get(i).copied().flatten()?;
                            Some(Pos2::new(xpos(x), y))
                        } else if horizontal {
                            Some(Pos2::new(y, cpos(i)))
                        } else {
                            Some(Pos2::new(cpos(i), y))
                        }
                    })
                    .collect();
                if series_has_line(ch, k) {
                    let w = line_width(ch, k, s);
                    for run in pts.split(Option::is_none) {
                        let run: Vec<Pos2> = run.iter().flatten().copied().collect();
                        let run = if ch.smooth && kind == ChartKind::Scatter || ch.smooth && kind == ChartKind::Line && ch.kind == ChartKind::Line { smooth(&run) } else { run };
                        stroke_path(&sp, &run, w, lc, f.dash);
                    }
                }
                let mk = series_marker(ch, k);
                for (i, q) in pts.iter().enumerate() {
                    let Some(q) = *q else { continue };
                    if let Some(m) = mk {
                        draw_marker(&sp, q, m, marker_radius(ch, k, s), fill, lc);
                    }
                    hit = hit.union(Rect::from_center_size(q, Vec2::splat(8.0 * s)));
                    if let Some(t) = spans[k][i].raw.and_then(|v| lab(v, i)) {
                        let g = 5.0 * s;
                        let (at, al) = match labels.pos {
                            LabelPos::Center => (q, Align2::CENTER_CENTER),
                            LabelPos::Left => (q - Vec2::new(g, 0.0), Align2::RIGHT_CENTER),
                            LabelPos::Above => (q - Vec2::new(0.0, g), Align2::CENTER_BOTTOM),
                            LabelPos::Below => (q + Vec2::new(0.0, g), Align2::CENTER_TOP),
                            _ => (q + Vec2::new(g, 0.0), Align2::LEFT_CENTER),
                        };
                        labels_to_draw.push((at, al, t));
                    }
                }
            }
            _ => {}
        }
        if hit.is_positive() {
            d.hit(hit, Elem::Series(k));
        }

        // Error bars.
        if let Some(eb) = &f.err {
            let st = Stroke::new(PT * 0.75 * s, rgb(ERR));
            let (pos, nb) = if kind.is_bar() { bar_layout(k) } else { (0, 1) };
            let xsk = xs_of(k);
            for (i, span) in spans[k].iter().enumerate() {
                let Some(top) = span.top else { continue };
                let raw = span.raw.unwrap_or(top);
                let (lo, hi) = err_range(eb, raw, stats[k].0, stats[k].1, stats[k].2);
                let (lo, hi) = (lo - raw + top, hi - raw + top);
                let mut c = if xy { xsk.get(i).copied().flatten().map(xpos).unwrap_or(f32::NAN) } else { cpos(i) };
                if kind.is_bar() && grouping == Grouping::Standard && nb > 1 {
                    let o = ch.overlap.clamp(-100, 100) as f32 / 100.0;
                    let g = ch.gap.min(500) as f32 / 100.0;
                    let nf = nb as f32;
                    let bw = slot / (nf - o * (nf - 1.0) + g);
                    let group = bw * (nf - o * (nf - 1.0));
                    c = if horizontal { c + group / 2.0 - pos as f32 * bw * (1.0 - o) - bw / 2.0 } else { c - group / 2.0 + pos as f32 * bw * (1.0 - o) + bw / 2.0 };
                }
                if !c.is_finite() {
                    continue;
                }
                let (a, b) = (vpos(sc, sc.clamp(lo)), vpos(sc, sc.clamp(hi)));
                let cap = 3.5 * s;
                let seg = |p: &Painter, v: f32| if horizontal { p.line_segment([Pos2::new(v, c - cap), Pos2::new(v, c + cap)], st) } else { p.line_segment([Pos2::new(c - cap, v), Pos2::new(c + cap, v)], st) };
                if horizontal {
                    sp.line_segment([Pos2::new(a, c), Pos2::new(b, c)], st);
                } else {
                    sp.line_segment([Pos2::new(c, a), Pos2::new(c, b)], st);
                }
                if eb.cap {
                    if eb.dir != ErrDir::Plus {
                        seg(&sp, a);
                    }
                    if eb.dir != ErrDir::Minus {
                        seg(&sp, b);
                    }
                }
            }
        }

        // Trendline.
        if let Some(t) = &f.trend {
            let pts: Vec<(f64, f64)> = if xy {
                let xsk = xs_of(k);
                ser.values.iter().enumerate().filter_map(|(i, v)| Some((xsk.get(i).copied().flatten()?, (*v)?))).collect()
            } else {
                ser.values.iter().enumerate().filter_map(|(i, v)| Some((i as f64 + 1.0, (*v)?))).collect()
            };
            let to_screen = |x: f64, y: f64| -> Pos2 {
                let v = vpos(sc, y);
                if xy {
                    Pos2::new(xpos(x), v)
                } else if horizontal {
                    Pos2::new(v, cposf(x as f32 - 1.0))
                } else {
                    Pos2::new(cposf(x as f32 - 1.0), v)
                }
            };
            let st_w = 1.5 * PT * s;
            if t.kind == TrendKind::MovingAverage {
                let ma = trend::moving_average(&ser.values, t.period as usize);
                let line: Vec<Pos2> = ma.iter().enumerate().filter_map(|(i, v)| v.map(|v| if xy { to_screen(xs_of(k)[i].unwrap_or(0.0), v) } else { to_screen(i as f64 + 1.0, v) })).collect();
                stroke_path(&sp, &line, st_w, lc, Dash::RoundDot);
            } else {
                let xv: Vec<f64> = pts.iter().map(|p| p.0).collect();
                let yv: Vec<f64> = pts.iter().map(|p| p.1).collect();
                if let Some(fit) = trend::fit(t.kind, t.order, &xv, &yv, t.intercept) {
                    let a = xv.iter().copied().fold(f64::INFINITY, f64::min) - t.backward.max(0.0);
                    let b = xv.iter().copied().fold(f64::NEG_INFINITY, f64::max) + t.forward.max(0.0);
                    let a = if matches!(t.kind, TrendKind::Logarithmic | TrendKind::Power) { a.max(1e-9) } else { a };
                    let line: Vec<Pos2> = (0..=80)
                        .map(|j| a + (b - a) * j as f64 / 80.0)
                        .map(|x| (x, fit.eval(x)))
                        .filter(|(_, y)| y.is_finite())
                        .map(|(x, y)| to_screen(x, y.clamp(sc.lo - (sc.hi - sc.lo) * 10.0, sc.hi + (sc.hi - sc.lo) * 10.0)))
                        .collect();
                    stroke_path(&sp, &line, st_w, lc, Dash::RoundDot);
                    let mut txt = Vec::new();
                    if t.show_eq {
                        txt.push(fit.equation());
                    }
                    if t.show_r2 {
                        txt.push(format!("R² = {}", trend::num(fit.r2)));
                    }
                    if !txt.is_empty() {
                        if let Some(end) = line.last() {
                            let at = Pos2::new(end.x.min(plot.right() - 2.0 * s), end.y.clamp(plot.top() + 2.0 * s, plot.bottom() - 2.0 * s));
                            let g = d.p.layout(txt.join("\n"), d.font(font), d.text, f32::INFINITY);
                            let pos = Pos2::new((at.x - g.size().x).max(plot.left()), (at.y - g.size().y - 4.0 * s).max(plot.top()));
                            d.p.galley(pos, g, d.text);
                        }
                    }
                }
            }
        }
    }
    for (at, al, t) in labels_to_draw {
        d.text(at, al, &t, font);
    }

    // Data table under the plot.
    if table {
        let top = plot.bottom() + 2.0 * s;
        let x0 = area.left();
        let tr = Rect::from_min_max(Pos2::new(x0, top), Pos2::new(plot.right(), top + table_rows as f32 * row_h));
        let ls = Stroke::new(1.0, d.grid);
        for r in 0..=table_rows {
            let y = crisp(top + r as f32 * row_h);
            let from = if r == 0 { plot.left() } else { x0 };
            d.p.line_segment([Pos2::new(from, y), Pos2::new(plot.right(), y)], ls);
        }
        for i in 0..=n {
            let x = crisp(plot.left() + i as f32 * slot);
            d.p.line_segment([Pos2::new(x, top), Pos2::new(x, tr.bottom())], ls);
        }
        d.p.line_segment([Pos2::new(crisp(x0), top + row_h), Pos2::new(crisp(x0), tr.bottom())], ls);
        for i in 0..n {
            let ci = if crev { n - 1 - i } else { i };
            let cx = plot.left() + (i as f32 + 0.5) * slot;
            d.text(Pos2::new(cx, top + row_h / 2.0), Align2::CENTER_CENTER, data.categories.get(ci).map(String::as_str).unwrap_or(""), font);
            for (k, ser) in data.series.iter().enumerate() {
                if let Some(v) = ser.values.get(ci).copied().flatten() {
                    let t = chart::format_number(v, &data.num_fmt, 0.0);
                    d.text(Pos2::new(cx, top + row_h * (k as f32 + 1.5)), Align2::CENTER_CENTER, &t, font);
                }
            }
        }
        for (k, ser) in data.series.iter().enumerate() {
            let cy = top + row_h * (k as f32 + 1.5);
            let sw = if matches!(ch.series_kind(k), ChartKind::Line) {
                Swatch::Line { col: rgb(ch.series_line(k)), width: 2.25 * PT, dash: Dash::Solid, marker: series_marker(ch, k).map(|m| (m, rgb(ch.series_fill(k)))) }
            } else {
                Swatch::Box(rgb(ch.series_fill(k)), None)
            };
            draw_swatch(d, &sw, x0 + 3.0 * s, cy, 14.0 * s);
            d.text(Pos2::new(x0 + 20.0 * s, cy), Align2::LEFT_CENTER, &ser.name, font);
        }
        d.hit(tr, Elem::DataTable);
    }
}
