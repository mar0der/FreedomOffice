//! Charts on the grid: painting, selection, moving and resizing, the
//! Insert > Charts buttons and the contextual Chart Design ribbon tab.

use std::f32::consts::{PI, TAU};

use eframe::egui::{self, epaint::Mesh, Align2, Color32, CursorIcon, Event, Key, Painter, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Vec2};

use super::grid::Layout;
use super::theme::{self, rgb};
use super::{App, Drag, RibbonTab, Scope};
use crate::chart::{self, Chart, ChartData, ChartKind, MIN_SIZE, PALETTE};

const TEXT: Color32 = Color32::from_rgb(0x59, 0x59, 0x59);
const GRID: Color32 = Color32::from_rgb(0xD9, 0xD9, 0xD9);
const BORDER: Color32 = Color32::from_rgb(0xBF, 0xBF, 0xBF);
const HANDLE: Color32 = Color32::from_rgb(0x70, 0x70, 0x70);
const BTN_H: f32 = 66.0;
/// Doughnut hole as a fraction of the outer radius.
const HOLE: f32 = 0.5;

/// Chart selection and drag state.
#[derive(Default)]
pub struct ChartUi {
    /// Selected chart: (sheet index, chart index).
    sel: Option<(usize, usize)>,
    drag: Option<ChartDrag>,
    /// Chart title edit box and the chart it belongs to.
    title: String,
    title_for: Option<(usize, usize)>,
    /// The title box holds an edit that has not been applied yet.
    title_dirty: bool,
    /// Ribbon tab to return to when the chart is deselected.
    prev_tab: Option<RibbonTab>,
}

#[derive(Clone, Copy)]
struct ChartDrag {
    idx: usize,
    /// None = move, Some(k) = resize handle k (clockwise from top-left).
    handle: Option<usize>,
    start: Pos2,
    /// Chart rectangle in sheet pixels when the drag began.
    orig: (f32, f32, f32, f32),
    moved: bool,
}

fn series_color(i: usize) -> Color32 {
    rgb(PALETTE[i % PALETTE.len()])
}

fn handles(r: Rect) -> [Pos2; 8] {
    [
        r.left_top(),
        r.center_top(),
        r.right_top(),
        r.right_center(),
        r.right_bottom(),
        r.center_bottom(),
        r.left_bottom(),
        r.left_center(),
    ]
}

fn cursor_for(handle: Option<usize>) -> CursorIcon {
    match handle {
        None => CursorIcon::Move,
        Some(0) | Some(4) => CursorIcon::ResizeNwSe,
        Some(2) | Some(6) => CursorIcon::ResizeNeSw,
        Some(1) | Some(5) => CursorIcon::ResizeVertical,
        Some(_) => CursorIcon::ResizeHorizontal,
    }
}

impl App {
    // ------------------------------------------------------------------
    // Selection and commands
    // ------------------------------------------------------------------

    /// Index of the selected chart on the active sheet.
    pub fn selected_chart(&self) -> Option<usize> {
        let (s, i) = self.chart.sel?;
        (s == self.wb.active && i < self.sheet().charts.len()).then_some(i)
    }

    fn deselect_chart(&mut self) {
        self.commit_chart_title();
        self.chart.sel = None;
        self.chart.drag = None;
    }

    pub fn insert_chart(&mut self, kind: ChartKind) {
        if !self.commit_edit(false) {
            return;
        }
        let si = self.wb.active;
        let sel = self.sel();
        let rg = if sel.is_single() { self.current_region() } else { sel };
        let mut ch = Chart::new(&self.wb, si, rg, kind);
        let data = chart::resolve(&self.wb, &ch);
        if data.series.iter().all(|s| s.values.iter().all(Option::is_none)) {
            self.status = Some("Select a range that contains numbers, then insert a chart.".into());
            return;
        }
        // Cascade over charts already sitting at the same spot.
        while self.sheet().charts.iter().any(|c| c.anchor == ch.anchor && c.offset == ch.offset) {
            ch.offset.0 += 20.0;
            ch.offset.1 += 20.0;
        }
        self.push_undo(&[Scope::Sheet(si)]);
        self.sheet_mut().charts.push(ch);
        self.chart.sel = Some((si, self.sheet().charts.len() - 1));
        if self.tab != RibbonTab::ChartDesign {
            self.chart.prev_tab = Some(self.tab);
        }
        self.tab = RibbonTab::ChartDesign;
        self.status = None;
        self.changed();
    }

    /// Applies an undoable change to the selected chart.
    fn edit_chart(&mut self, f: impl FnOnce(&mut Chart)) {
        if let Some(i) = self.selected_chart() {
            self.edit_chart_at((self.wb.active, i), f);
        }
    }

    fn edit_chart_at(&mut self, (si, i): (usize, usize), f: impl FnOnce(&mut Chart)) {
        let Some(old) = self.wb.sheets.get(si).and_then(|sh| sh.charts.get(i)) else { return };
        let mut ch = old.clone();
        f(&mut ch);
        if &ch == old {
            return;
        }
        self.push_undo(&[Scope::Sheet(si)]);
        self.wb.sheets[si].charts[i] = ch;
        self.changed();
    }

    /// Applies a pending edit from the Chart Title box.
    fn commit_chart_title(&mut self) {
        if !std::mem::take(&mut self.chart.title_dirty) {
            return;
        }
        let Some(key) = self.chart.title_for else { return };
        let t = self.chart.title.trim().to_string();
        self.edit_chart_at(key, |c| c.title = if t.is_empty() { None } else { Some(t) });
    }

    fn delete_chart(&mut self) {
        let Some(i) = self.selected_chart() else { return };
        self.push_undo(&[Scope::Sheet(self.wb.active)]);
        self.sheet_mut().charts.remove(i);
        self.deselect_chart();
        self.changed();
    }

    /// Keyboard while a chart is selected; returns true when the keys were consumed.
    pub(super) fn chart_keys(&mut self, ctx: &egui::Context) -> bool {
        if self.selected_chart().is_none() {
            return false;
        }
        for ev in ctx.input(|i| i.events.clone()) {
            let Event::Key { key, pressed: true, modifiers: m, .. } = ev else { continue };
            let ctrl = m.ctrl || m.command;
            match key {
                Key::Delete | Key::Backspace => self.delete_chart(),
                Key::Escape => self.deselect_chart(),
                Key::Z if ctrl => self.undo(),
                Key::Y if ctrl => self.redo(),
                Key::S if ctrl && m.shift => {
                    self.save_as();
                }
                Key::S if ctrl => {
                    self.save();
                }
                _ => continue,
            }
            break;
        }
        true
    }

    // ------------------------------------------------------------------
    // Geometry and mouse
    // ------------------------------------------------------------------

    /// Screen rectangle of a chart and the clip area it is visible in
    /// (charts anchored in the scrolling area stay out of frozen panes).
    fn chart_screen_rect(&self, lay: &Layout, ch: &Chart) -> (Rect, Rect) {
        let sh = self.sheet();
        let z = self.zoom;
        let (fr, fc) = sh.freeze;
        let (x, y, w, h) = chart::sheet_rect(sh, ch);
        let (sx, cl) = if ch.anchor.1 >= fc {
            (lay.frozen_x + (x - chart::col_x(sh, sh.view.left.max(fc))) * z, lay.frozen_x)
        } else {
            (lay.cells.left() + x * z, lay.cells.left())
        };
        let (sy, ct) = if ch.anchor.0 >= fr {
            (lay.frozen_y + (y - chart::row_y(sh, sh.view.top.max(fr))) * z, lay.frozen_y)
        } else {
            (lay.cells.top() + y * z, lay.cells.top())
        };
        let rect = Rect::from_min_size(Pos2::new(sx, sy), Vec2::new(w * z, h * z));
        (rect, Rect::from_min_max(Pos2::new(cl, ct), lay.cells.max))
    }

    fn chart_hit(&self, lay: &Layout, pos: Pos2) -> Option<(usize, Option<usize>)> {
        if let Some(i) = self.selected_chart() {
            let (r, clip) = self.chart_screen_rect(lay, &self.sheet().charts[i]);
            if clip.contains(pos) {
                if let Some(k) = handles(r).iter().position(|p| (*p - pos).length() <= 6.0) {
                    return Some((i, Some(k)));
                }
            }
        }
        let charts = &self.sheet().charts;
        (0..charts.len()).rev().find_map(|i| {
            let (r, clip) = self.chart_screen_rect(lay, &charts[i]);
            (clip.contains(pos) && r.contains(pos)).then_some((i, None))
        })
    }

    /// Mouse over the grid; returns true when a chart took the event.
    pub(super) fn chart_pointer(&mut self, ui: &egui::Ui, lay: &Layout, pos: Pos2, hovered: bool, pressed: bool, down: bool) -> bool {
        if let Some(mut d) = self.chart.drag {
            if down {
                if !d.moved && (pos - d.start).length() > 3.0 {
                    d.moved = true;
                    self.push_undo(&[Scope::Sheet(self.wb.active)]);
                }
                if d.moved {
                    self.apply_chart_drag(&d, (pos - d.start) / self.zoom);
                    self.dirty = true;
                }
                ui.ctx().set_cursor_icon(cursor_for(d.handle));
                self.chart.drag = Some(d);
            } else {
                self.chart.drag = None;
            }
            return true;
        }
        if !hovered || self.drag != Drag::None || !lay.cells.contains(pos) {
            return false;
        }
        match self.chart_hit(lay, pos) {
            Some((i, handle)) => {
                ui.ctx().set_cursor_icon(cursor_for(handle));
                if pressed {
                    if self.edit.is_some() && !self.commit_edit(false) {
                        return true;
                    }
                    self.filter_popup = None;
                    self.chart.sel = Some((self.wb.active, i));
                    let orig = chart::sheet_rect(self.sheet(), &self.sheet().charts[i]);
                    self.chart.drag = Some(ChartDrag { idx: i, handle, start: pos, orig, moved: false });
                }
                true
            }
            None => {
                if pressed {
                    self.deselect_chart();
                }
                false
            }
        }
    }

    fn apply_chart_drag(&mut self, d: &ChartDrag, delta: Vec2) {
        let (x, y, w, h) = d.orig;
        let (mut l, mut t, mut r, mut b) = (x, y, x + w, y + h);
        match d.handle {
            None => {
                let dx = delta.x.max(-l);
                let dy = delta.y.max(-t);
                l += dx;
                r += dx;
                t += dy;
                b += dy;
            }
            Some(k) => {
                if matches!(k, 0 | 6 | 7) {
                    l = (l + delta.x).min(r - MIN_SIZE).max(0.0);
                }
                if matches!(k, 2..=4) {
                    r = (r + delta.x).max(l + MIN_SIZE);
                }
                if matches!(k, 0..=2) {
                    t = (t + delta.y).min(b - MIN_SIZE).max(0.0);
                }
                if matches!(k, 4..=6) {
                    b = (b + delta.y).max(t + MIN_SIZE);
                }
            }
        }
        let si = self.wb.active;
        let Some(mut ch) = self.wb.sheets[si].charts.get(d.idx).cloned() else { return };
        chart::set_sheet_rect(&self.wb.sheets[si], &mut ch, l, t, r - l, b - t);
        self.wb.sheets[si].charts[d.idx] = ch;
    }

    // ------------------------------------------------------------------
    // Painting
    // ------------------------------------------------------------------

    pub(super) fn paint_charts(&mut self, ui: &egui::Ui, lay: &Layout) {
        let n = self.sheet().charts.len();
        if n == 0 {
            return;
        }
        let sel = self.selected_chart();
        let si = self.wb.active;
        // Source data outlines of the selected chart.
        if let Some(i) = sel {
            let ch = &self.sheet().charts[i];
            if self.wb.sheet_index(&ch.sheet) == Some(si) {
                let data = chart::resolve(&self.wb, ch);
                let cp = ui.painter().with_clip_rect(lay.cells);
                let outline = |rg: crate::model::Range, col: Color32| {
                    let r = self.range_rect(lay, rg);
                    cp.rect_stroke(r.shrink(0.5), 0.0, Stroke::new(1.5, col), StrokeKind::Middle);
                };
                if let Some(cr) = data.cat_ref {
                    outline(cr, Color32::from_rgb(0x9B, 0x4F, 0x96));
                }
                for s in &data.series {
                    if let Some((r, c)) = s.name_cell {
                        outline(crate::model::Range::cell(r, c), Color32::from_rgb(0xD6, 0x3B, 0x3B));
                    }
                }
                if let (Some(a), Some(b)) = (data.series.first(), data.series.last()) {
                    outline(a.refr.union(&b.refr), Color32::from_rgb(0x2F, 0x6E, 0xBA));
                }
            }
        }
        for i in 0..n {
            let ch = self.sheet().charts[i].clone();
            let (rect, clip) = self.chart_screen_rect(lay, &ch);
            if !clip.intersects(rect.expand(6.0)) {
                continue;
            }
            let data = chart::resolve(&self.wb, &ch);
            let p = ui.painter().with_clip_rect(clip.intersect(rect));
            draw_chart(&p, rect, &ch, &data, self.zoom);
            if sel == Some(i) {
                let p = ui.painter().with_clip_rect(clip.intersect(rect.expand(6.0)));
                p.rect_stroke(rect, 0.0, Stroke::new(1.0, HANDLE), StrokeKind::Middle);
                for h in handles(rect) {
                    p.circle_filled(h, 4.0, Color32::WHITE);
                    p.circle_stroke(h, 4.0, Stroke::new(1.0, HANDLE));
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Ribbon
    // ------------------------------------------------------------------

    /// Insert > Charts group content.
    pub(super) fn chart_insert_buttons(&mut self, ui: &mut egui::Ui) {
        for (kind, label, tip) in [
            (ChartKind::Column, "Column", "Insert Clustered Column Chart"),
            (ChartKind::Line, "Line", "Insert Line Chart with Markers"),
            (ChartKind::Pie, "Pie", "Insert Pie Chart"),
            (ChartKind::Bar, "Bar", "Insert Clustered Bar Chart"),
            (ChartKind::Area, "Area", "Insert Area Chart"),
            (ChartKind::Scatter, "Scatter", "Insert Scatter (X, Y) Chart"),
        ] {
            if kind_button(ui, kind, label, tip).clicked() {
                self.insert_chart(kind);
            }
        }
    }

    /// The accent-coloured contextual tab label shown while a chart is selected.
    pub(super) fn chart_tab_button(&mut self, ui: &mut egui::Ui) {
        if self.selected_chart().is_none() {
            if self.chart.sel.is_some() {
                // The chart went away (other sheet, undo): drop the stale selection.
                self.deselect_chart();
            }
            if self.tab == RibbonTab::ChartDesign {
                self.tab = self.chart.prev_tab.take().unwrap_or(RibbonTab::Home);
            }
            return;
        }
        let sel = self.tab == RibbonTab::ChartDesign && !self.ribbon_collapsed;
        let font = if sel { theme::ui_bold(13.0) } else { theme::ui_font(13.0) };
        let name = "Chart Design";
        let g = ui.painter().layout_no_wrap(name.to_string(), font.clone(), theme::accent());
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(g.size().x + 20.0, 28.0), Sense::click());
        if resp.hovered() && !sel {
            ui.painter().rect_filled(rect.shrink(2.0), 3.0, theme::hover());
        }
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, name, font, theme::accent());
        if sel {
            ui.painter().line_segment(
                [Pos2::new(rect.left() + 8.0, rect.bottom() - 2.0), Pos2::new(rect.right() - 8.0, rect.bottom() - 2.0)],
                Stroke::new(3.0, theme::accent()),
            );
        }
        if resp.clicked() {
            if self.tab != RibbonTab::ChartDesign {
                self.chart.prev_tab = Some(self.tab);
            }
            self.tab = RibbonTab::ChartDesign;
            self.ribbon_collapsed = false;
        }
    }

    pub(super) fn chart_design_tab(&mut self, ui: &mut egui::Ui) {
        let Some(i) = self.selected_chart() else { return };
        let ch = self.sheet().charts[i].clone();
        let key = (self.wb.active, i);
        let title_id = egui::Id::new("chart_title_box");
        if self.chart.title_for != Some(key) {
            self.commit_chart_title();
        }
        if self.chart.title_for != Some(key) || (!self.chart.title_dirty && !ui.memory(|m| m.has_focus(title_id))) {
            self.chart.title = ch.title.clone().unwrap_or_default();
            self.chart.title_for = Some(key);
        }

        self.group(ui, "Type", |app, ui| {
            let resp = big_button(ui, "Change\nChart Type", "Change to a different type of chart", |p, r| paint_kind_icon(p, r, ch.kind));
            egui::Popup::menu(&resp).show(|ui| {
                ui.set_min_width(150.0);
                for k in ChartKind::ALL {
                    let r = ui.selectable_label(k == ch.kind, format!("        {}", k.label()));
                    let icon = Rect::from_center_size(Pos2::new(r.rect.left() + 14.0, r.rect.center().y), Vec2::splat(16.0));
                    paint_kind_icon(ui.painter(), icon, k);
                    if r.clicked() {
                        app.edit_chart(|c| {
                            c.kind = k;
                            if k.is_round() {
                                c.legend = true;
                            }
                        });
                        ui.close();
                    }
                }
            });
        });
        self.group(ui, "Data", |app, ui| {
            if big_button(ui, "Switch\nRow/Column", "Swap the data over the axis", paint_switch_icon).clicked() {
                app.edit_chart(Chart::switch_row_col);
            }
        });
        self.group(ui, "Chart Elements", |app, ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    let shown = ch.title.as_deref() != Some("");
                    if toggle(ui, "Chart Title", shown).clicked() {
                        app.edit_chart(|c| c.title = if shown { Some(String::new()) } else { None });
                    }
                    if toggle(ui, "Legend", ch.legend).clicked() {
                        app.edit_chart(|c| c.legend = !c.legend);
                    }
                    if toggle(ui, "Data Labels", ch.data_labels).clicked() {
                        app.edit_chart(|c| c.data_labels = !c.data_labels);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Title:");
                    let shown = ch.title.as_deref() != Some("");
                    let edit = egui::TextEdit::singleline(&mut app.chart.title).id(title_id).desired_width(190.0).hint_text("Automatic");
                    let resp = ui.add_enabled(shown, edit);
                    if resp.changed() {
                        app.chart.title_dirty = true;
                    }
                    if resp.lost_focus() {
                        app.commit_chart_title();
                    }
                });
            });
        });
        self.group(ui, "Delete", |app, ui| {
            if big_button(ui, "Delete\nChart", "Delete the selected chart (Delete)", paint_delete_icon).clicked() {
                app.delete_chart();
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Ribbon widgets and icons (drawn, no image assets)
// ---------------------------------------------------------------------------

fn toggle(ui: &mut egui::Ui, label: &str, on: bool) -> egui::Response {
    let mut b = egui::Button::new(egui::RichText::new(label).size(12.5)).min_size(Vec2::new(0.0, 24.0));
    if on {
        b = b.fill(theme::checked());
    }
    ui.add(b)
}

fn big_button(ui: &mut egui::Ui, label: &str, tip: &str, icon: impl FnOnce(&Painter, Rect)) -> egui::Response {
    let font = theme::ui_font(12.0);
    let w = label
        .lines()
        .map(|l| ui.painter().layout_no_wrap(l.to_string(), font.clone(), theme::text()).size().x)
        .fold(0.0, f32::max);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new((w + 10.0).max(48.0), BTN_H), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 3.0, theme::hover());
    }
    icon(ui.painter(), Rect::from_center_size(Pos2::new(rect.center().x, rect.top() + 19.0), Vec2::splat(26.0)));
    let mut y = rect.top() + 36.0;
    for l in label.lines() {
        ui.painter().text(Pos2::new(rect.center().x, y), Align2::CENTER_TOP, l, font.clone(), theme::text());
        y += 14.0;
    }
    resp.on_hover_text(tip)
}

fn kind_button(ui: &mut egui::Ui, kind: ChartKind, label: &str, tip: &str) -> egui::Response {
    big_button(ui, label, tip, |p, r| paint_kind_icon(p, r, kind))
}

fn paint_kind_icon(p: &Painter, r: Rect, kind: ChartKind) {
    let (c0, c1, c2) = (series_color(0), series_color(1), series_color(2));
    let at = |fx: f32, fy: f32| Pos2::new(r.left() + fx * r.width(), r.top() + fy * r.height());
    let axis = Stroke::new(1.0, theme::icon_line());
    match kind {
        ChartKind::Column => {
            for (i, (h, c)) in [(0.55, c0), (0.85, c1), (0.4, c2)].iter().enumerate() {
                let x = 0.12 + i as f32 * 0.28;
                p.rect_filled(Rect::from_min_max(at(x, 0.95 - h), at(x + 0.2, 0.95)), 0.0, *c);
            }
            p.line_segment([at(0.0, 0.95), at(1.0, 0.95)], axis);
        }
        ChartKind::Bar => {
            for (i, (w, c)) in [(0.55, c0), (0.85, c1), (0.4, c2)].iter().enumerate() {
                let y = 0.12 + i as f32 * 0.28;
                p.rect_filled(Rect::from_min_max(at(0.05, y), at(0.05 + w, y + 0.2)), 0.0, *c);
            }
            p.line_segment([at(0.05, 0.0), at(0.05, 1.0)], axis);
        }
        ChartKind::Line => {
            p.line_segment([at(0.0, 0.95), at(1.0, 0.95)], axis);
            for (pts, c) in [([0.7, 0.45, 0.55, 0.15], c0), ([0.85, 0.7, 0.8, 0.5], c1)] {
                let v: Vec<Pos2> = pts.iter().enumerate().map(|(i, y)| at(0.08 + i as f32 * 0.28, *y)).collect();
                p.add(Shape::line(v.clone(), Stroke::new(2.0, c)));
                for q in v {
                    p.circle_filled(q, 2.2, c);
                }
            }
        }
        ChartKind::Area => {
            let mut m = Mesh::default();
            let ys = [0.6, 0.3, 0.5, 0.2];
            for i in 0..3 {
                let (x0, x1) = (i as f32 / 3.0, (i + 1) as f32 / 3.0);
                quad(&mut m, at(x0, 0.95), at(x0, ys[i]), at(x1, ys[i + 1]), at(x1, 0.95), c0);
            }
            p.add(Shape::mesh(m));
            p.line_segment([at(0.0, 0.95), at(1.0, 0.95)], axis);
        }
        ChartKind::Scatter => {
            p.line_segment([at(0.05, 0.0), at(0.05, 0.95)], axis);
            p.line_segment([at(0.05, 0.95), at(1.0, 0.95)], axis);
            for (x, y) in [(0.2, 0.7), (0.35, 0.5), (0.5, 0.6), (0.62, 0.3), (0.8, 0.35), (0.9, 0.12)] {
                p.circle_filled(at(x, y), 2.3, c0);
            }
        }
        ChartKind::Pie | ChartKind::Doughnut => {
            let hole = if kind == ChartKind::Doughnut { 0.5 } else { 0.0 };
            let rad = r.width().min(r.height()) / 2.0 - 1.0;
            let fr = [0.45, 0.3, 0.25];
            let mut a = -PI / 2.0;
            for (i, f) in fr.iter().enumerate() {
                let b = a + f * TAU;
                arc_band(p, r.center(), rad * hole, rad, a, b, series_color(i));
                a = b;
            }
        }
    }
}

fn paint_switch_icon(p: &Painter, r: Rect) {
    let s = Stroke::new(2.0, theme::GRID_ACCENT);
    let c = r.center();
    let d = r.width() * 0.35;
    p.line_segment([c + Vec2::new(-d, -d * 0.5), c + Vec2::new(d, -d * 0.5)], s);
    p.line_segment([c + Vec2::new(d, -d * 0.5), c + Vec2::new(d * 0.55, -d * 0.95)], s);
    p.line_segment([c + Vec2::new(-d, d * 0.5), c + Vec2::new(d, d * 0.5)], s);
    p.line_segment([c + Vec2::new(-d, d * 0.5), c + Vec2::new(-d * 0.55, d * 0.95)], s);
}

fn paint_delete_icon(p: &Painter, r: Rect) {
    let s = Stroke::new(2.0, Color32::from_rgb(0xC4, 0x2B, 0x1C));
    let r = r.shrink(6.0);
    p.line_segment([r.left_top(), r.right_bottom()], s);
    p.line_segment([r.right_top(), r.left_bottom()], s);
}

// ---------------------------------------------------------------------------
// Chart drawing
// ---------------------------------------------------------------------------

fn quad(m: &mut Mesh, a: Pos2, b: Pos2, c: Pos2, d: Pos2, col: Color32) {
    let i = m.vertices.len() as u32;
    for q in [a, b, c, d] {
        m.colored_vertex(q, col);
    }
    m.add_triangle(i, i + 1, i + 2);
    m.add_triangle(i, i + 2, i + 3);
}

/// Filled ring segment (a pie slice when `r0` is zero) between angles a and b.
fn arc_band(p: &Painter, c: Pos2, r0: f32, r1: f32, a: f32, b: f32, col: Color32) {
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

fn text_size(p: &Painter, s: &str, size: f32) -> Vec2 {
    p.layout_no_wrap(s.to_string(), theme::ui_font(size), TEXT).size()
}

fn marker(p: &Painter, at: Pos2, s: f32, col: Color32) {
    p.circle_filled(at, 3.3 * s, col);
}

enum Swatch {
    Box,
    Line,
    Dot,
}

pub(super) fn draw_chart(p: &Painter, rect: Rect, ch: &Chart, data: &ChartData, z: f32) {
    let s = z;
    p.rect_filled(rect, 0.0, Color32::WHITE);
    p.rect_stroke(rect, 0.0, Stroke::new(1.0, BORDER), StrokeKind::Inside);
    let mut inner = rect.shrink(10.0 * s);
    if let Some(t) = chart::display_title(ch, data) {
        let g = p.layout_no_wrap(t, theme::ui_font(18.7 * s), TEXT);
        let h = g.size().y;
        p.galley(Pos2::new(rect.center().x - g.size().x / 2.0, inner.top()), g, TEXT);
        inner.min.y += h + 6.0 * s;
    }
    if ch.legend {
        let (entries, swatch): (Vec<(String, Color32)>, Swatch) = if ch.kind.is_round() {
            (data.categories.iter().enumerate().map(|(i, c)| (c.clone(), series_color(i))).collect(), Swatch::Box)
        } else {
            let sw = match ch.kind {
                ChartKind::Line => Swatch::Line,
                ChartKind::Scatter => Swatch::Dot,
                _ => Swatch::Box,
            };
            (data.series.iter().enumerate().map(|(i, x)| (x.name.clone(), series_color(i))).collect(), sw)
        };
        let used = draw_legend(p, inner, &entries, &swatch, s);
        inner.max.y -= used;
    }
    if inner.width() < 24.0 || inner.height() < 24.0 || data.series.is_empty() {
        return;
    }
    if ch.kind.is_round() {
        draw_round(p, inner, ch, data, s);
    } else {
        draw_xy(p, inner, ch, data, s);
    }
}

/// Legend centred at the bottom of `area`; returns the height it used.
fn draw_legend(p: &Painter, area: Rect, entries: &[(String, Color32)], swatch: &Swatch, s: f32) -> f32 {
    if entries.is_empty() {
        return 0.0;
    }
    let size = 12.0 * s;
    let sw = 14.0 * s;
    let gap = 12.0 * s;
    let items: Vec<(f32, &str, Color32)> =
        entries.iter().take(200).map(|(t, c)| (sw + 4.0 * s + text_size(p, t, size).x, t.as_str(), *c)).collect();
    let mut rows: Vec<Vec<(f32, &str, Color32)>> = vec![Vec::new()];
    let mut w = 0.0;
    for it in items {
        if w > 0.0 && w + gap + it.0 > area.width() {
            rows.push(Vec::new());
            w = 0.0;
        }
        w += if w > 0.0 { gap } else { 0.0 } + it.0;
        rows.last_mut().unwrap().push(it);
    }
    let line_h = text_size(p, "Ag", size).y + 2.0 * s;
    let rows: Vec<_> = rows.into_iter().take(((area.height() * 0.4) / line_h).max(1.0) as usize).collect();
    let total = rows.len() as f32 * line_h;
    let mut y = area.bottom() - total;
    for row in rows {
        let rw: f32 = row.iter().map(|i| i.0).sum::<f32>() + gap * (row.len().saturating_sub(1)) as f32;
        let mut x = area.center().x - rw / 2.0;
        let cy = y + line_h / 2.0;
        for (iw, t, c) in row {
            match swatch {
                Swatch::Box => {
                    p.rect_filled(Rect::from_center_size(Pos2::new(x + sw / 2.0, cy), Vec2::splat(7.0 * s)), 0.0, c);
                }
                Swatch::Line => {
                    p.line_segment([Pos2::new(x, cy), Pos2::new(x + sw, cy)], Stroke::new(2.5 * s, c));
                    marker(p, Pos2::new(x + sw / 2.0, cy), s, c);
                }
                Swatch::Dot => marker(p, Pos2::new(x + sw / 2.0, cy), s, c),
            }
            p.text(Pos2::new(x + sw + 4.0 * s, cy), Align2::LEFT_CENTER, t, theme::ui_font(size), TEXT);
            x += iw + gap;
        }
        y += line_h;
    }
    total + 4.0 * s
}

fn draw_round(p: &Painter, area: Rect, ch: &Chart, data: &ChartData, s: f32) {
    let c = area.center();
    let r = (area.width().min(area.height()) / 2.0 - 2.0 * s).max(4.0);
    let font = 12.0 * s;
    let rings: Vec<&chart::SeriesData> = if ch.kind == ChartKind::Pie { data.series.iter().take(1).collect() } else { data.series.iter().collect() };
    let hole = if ch.kind == ChartKind::Doughnut { r * HOLE } else { 0.0 };
    let band = (r - hole) / rings.len().max(1) as f32;
    let sep = Stroke::new(1.5 * s, Color32::WHITE);
    for (k, ser) in rings.iter().enumerate() {
        let (r0, r1) = (hole + band * k as f32, hole + band * (k + 1) as f32);
        let total: f64 = ser.values.iter().flatten().map(|v| v.abs()).sum();
        if total <= 0.0 {
            continue;
        }
        let mut a = -PI / 2.0;
        let mut cuts = Vec::new();
        for (i, v) in ser.values.iter().enumerate() {
            let v = v.map(f64::abs).unwrap_or(0.0);
            if v <= 0.0 {
                continue;
            }
            let b = a + (v / total) as f32 * TAU;
            arc_band(p, c, r0, r1, a, b, series_color(i));
            if ch.data_labels {
                let mid = (a + b) / 2.0;
                let rr = if hole > 0.0 { (r0 + r1) / 2.0 } else { r1 * 0.65 };
                let txt = chart::format_number(v, &data.num_fmt, 0.0);
                p.text(c + Vec2::new(mid.cos(), mid.sin()) * rr, Align2::CENTER_CENTER, txt, theme::ui_font(font), TEXT);
            }
            cuts.push(a);
            a = b;
        }
        if cuts.len() > 1 {
            for t in cuts {
                let d = Vec2::new(t.cos(), t.sin());
                p.line_segment([c + d * r0, c + d * r1], sep);
            }
        }
        if k > 0 {
            p.circle_stroke(c, r0, sep);
        }
    }
}

fn draw_xy(p: &Painter, area: Rect, ch: &Chart, data: &ChartData, s: f32) {
    let font = 12.0 * s;
    let kind = ch.kind;
    let horizontal = kind == ChartKind::Bar;
    let scatter = kind == ChartKind::Scatter;
    let n = data.points();
    if n == 0 {
        return;
    }
    let vals = || data.series.iter().flat_map(|x| x.values.iter().flatten().copied());
    let vmin = vals().fold(f64::INFINITY, f64::min);
    let vmax = vals().fold(f64::NEG_INFINITY, f64::max);
    let (vmin, vmax) = if vmin.is_finite() { (vmin, vmax) } else { (0.0, 0.0) };
    let (lo, hi, step) = chart::nice_axis(vmin, vmax);
    let ticks = |lo: f64, hi: f64, step: f64| -> Vec<f64> {
        let k = ((hi - lo) / step).round().clamp(1.0, 60.0) as usize;
        (0..=k).map(|i| lo + step * i as f64).collect()
    };
    let vt = ticks(lo, hi, step);
    let vlabels: Vec<String> = vt.iter().map(|v| chart::format_number(*v, &data.num_fmt, step)).collect();
    let line_h = text_size(p, "Ag", font).y;
    let vlab_w = vlabels.iter().map(|l| text_size(p, l, font).x).fold(0.0, f32::max);

    // X range for scatter charts.
    let xs: Vec<f64> = (0..n).map(|i| data.cat_values.get(i).copied().flatten().unwrap_or(i as f64 + 1.0)).collect();
    let (xlo, xhi, xstep) = if scatter {
        let a = xs.iter().copied().fold(f64::INFINITY, f64::min);
        let b = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        chart::nice_axis(a, b)
    } else {
        (0.0, 1.0, 1.0)
    };

    let cat_w = if horizontal {
        data.categories.iter().map(|l| text_size(p, l, font).x).fold(0.0, f32::max).min(area.width() * 0.4)
    } else {
        0.0
    };
    let plot = if horizontal {
        Rect::from_min_max(
            Pos2::new(area.left() + cat_w + 6.0 * s, area.top() + 2.0 * s),
            Pos2::new(area.right() - vlab_w / 2.0, area.bottom() - line_h - 6.0 * s),
        )
    } else {
        Rect::from_min_max(
            Pos2::new(area.left() + vlab_w + 6.0 * s, area.top() + line_h / 2.0),
            Pos2::new(area.right() - 4.0 * s, area.bottom() - line_h - 6.0 * s),
        )
    };
    if plot.width() < 8.0 || plot.height() < 8.0 {
        return;
    }
    let span = (hi - lo).max(f64::MIN_POSITIVE);
    // value -> screen coordinate along the value axis
    let vpos = |v: f64| -> f32 {
        let f = ((v - lo) / span) as f32;
        if horizontal { plot.left() + f * plot.width() } else { plot.bottom() - f * plot.height() }
    };
    let xpos = |x: f64| -> f32 { plot.left() + ((x - xlo) / (xhi - xlo).max(f64::MIN_POSITIVE)) as f32 * plot.width() };
    let gs = Stroke::new(1.0, GRID);
    let crisp = |v: f32| v.round() + 0.5;

    // Value gridlines and labels.
    for (v, l) in vt.iter().zip(&vlabels) {
        let q = crisp(vpos(*v));
        if horizontal {
            p.line_segment([Pos2::new(q, plot.top()), Pos2::new(q, plot.bottom())], gs);
            p.text(Pos2::new(q, plot.bottom() + 4.0 * s), Align2::CENTER_TOP, l, theme::ui_font(font), TEXT);
        } else {
            p.line_segment([Pos2::new(plot.left(), q), Pos2::new(plot.right(), q)], gs);
            p.text(Pos2::new(plot.left() - 4.0 * s, q), Align2::RIGHT_CENTER, l, theme::ui_font(font), TEXT);
        }
    }
    let base = vpos(0.0f64.clamp(lo, hi));

    // Category / X axis.
    let slot = if horizontal { plot.height() } else { plot.width() } / n as f32;
    let on_ticks = kind == ChartKind::Area && n > 1;
    let cpos = |i: usize| -> f32 {
        if horizontal {
            plot.bottom() - (i as f32 + 0.5) * slot
        } else if on_ticks {
            plot.left() + i as f32 * plot.width() / (n - 1) as f32
        } else {
            plot.left() + (i as f32 + 0.5) * slot
        }
    };
    if scatter {
        for v in ticks(xlo, xhi, xstep) {
            let q = crisp(xpos(v));
            p.line_segment([Pos2::new(q, plot.top()), Pos2::new(q, plot.bottom())], gs);
            let l = chart::format_number(v, "", xstep);
            p.text(Pos2::new(q, plot.bottom() + 4.0 * s), Align2::CENTER_TOP, l, theme::ui_font(font), TEXT);
        }
    } else if horizontal {
        let every = ((line_h + 2.0 * s) / slot).ceil().max(1.0) as usize;
        for (i, l) in data.categories.iter().enumerate().step_by(every) {
            p.text(Pos2::new(plot.left() - 4.0 * s, cpos(i)), Align2::RIGHT_CENTER, l, theme::ui_font(font), TEXT);
        }
        p.line_segment([Pos2::new(crisp(base), plot.top()), Pos2::new(crisp(base), plot.bottom())], gs);
    } else {
        let maxw = data.categories.iter().map(|l| text_size(p, l, font).x).fold(0.0, f32::max);
        let pitch = if on_ticks { plot.width() / (n - 1) as f32 } else { slot };
        let every = ((maxw + 6.0 * s) / pitch).ceil().max(1.0) as usize;
        for (i, l) in data.categories.iter().enumerate().step_by(every) {
            p.text(Pos2::new(cpos(i), plot.bottom() + 4.0 * s), Align2::CENTER_TOP, l, theme::ui_font(font), TEXT);
        }
        p.line_segment([Pos2::new(plot.left(), crisp(base)), Pos2::new(plot.right(), crisp(base))], gs);
    }

    let nser = data.series.len();
    let label = |v: f64| chart::format_number(v, &data.num_fmt, 0.0);
    match kind {
        ChartKind::Column | ChartKind::Bar => {
            let nf = nser as f32;
            let bw = slot / (nf + 0.27 * (nf - 1.0) + 2.19);
            let group = bw * (nf + 0.27 * (nf - 1.0));
            for (k, ser) in data.series.iter().enumerate() {
                let col = series_color(k);
                for (i, v) in ser.values.iter().enumerate() {
                    let Some(v) = *v else { continue };
                    let along = cpos(i) - group / 2.0 + k as f32 * bw * 1.27;
                    let end = vpos(v.clamp(lo, hi));
                    let r = if horizontal {
                        // first series at the bottom of each cluster
                        let top = cpos(i) + group / 2.0 - (k as f32 + 1.0) * bw - k as f32 * bw * 0.27;
                        Rect::from_min_max(Pos2::new(base.min(end), top), Pos2::new(base.max(end), top + bw))
                    } else {
                        Rect::from_min_max(Pos2::new(along, base.min(end)), Pos2::new(along + bw, base.max(end)))
                    };
                    p.rect_filled(r, 0.0, col);
                    if ch.data_labels {
                        let (pos, al) = match (horizontal, v >= 0.0) {
                            (true, true) => (r.right_center() + Vec2::new(3.0 * s, 0.0), Align2::LEFT_CENTER),
                            (true, false) => (r.left_center() - Vec2::new(3.0 * s, 0.0), Align2::RIGHT_CENTER),
                            (false, true) => (r.center_top() - Vec2::new(0.0, 2.0 * s), Align2::CENTER_BOTTOM),
                            (false, false) => (r.center_bottom() + Vec2::new(0.0, 2.0 * s), Align2::CENTER_TOP),
                        };
                        p.text(pos, al, label(v), theme::ui_font(font), TEXT);
                    }
                }
            }
        }
        ChartKind::Line => {
            for (k, ser) in data.series.iter().enumerate() {
                let col = series_color(k);
                let pts: Vec<Option<Pos2>> =
                    ser.values.iter().enumerate().map(|(i, v)| v.map(|v| Pos2::new(cpos(i), vpos(v)))).collect();
                for run in pts.split(Option::is_none) {
                    let run: Vec<Pos2> = run.iter().flatten().copied().collect();
                    if run.len() > 1 {
                        p.add(Shape::line(run, Stroke::new(3.0 * s, col)));
                    }
                }
                for q in pts.iter().flatten() {
                    marker(p, *q, s, col);
                }
                if ch.data_labels {
                    for (i, v) in ser.values.iter().enumerate() {
                        if let Some(v) = v {
                            p.text(Pos2::new(cpos(i), vpos(*v) - 5.0 * s), Align2::CENTER_BOTTOM, label(*v), theme::ui_font(font), TEXT);
                        }
                    }
                }
            }
        }
        ChartKind::Area => {
            for (k, ser) in data.series.iter().enumerate() {
                let col = series_color(k);
                let ys: Vec<f32> = ser.values.iter().map(|v| vpos(v.unwrap_or(0.0).clamp(lo, hi))).collect();
                let mut m = Mesh::default();
                if ys.len() == 1 {
                    let x = cpos(0);
                    quad(&mut m, Pos2::new(x - slot * 0.3, base), Pos2::new(x - slot * 0.3, ys[0]), Pos2::new(x + slot * 0.3, ys[0]), Pos2::new(x + slot * 0.3, base), col);
                }
                for i in 1..ys.len() {
                    let (x0, x1, y0, y1) = (cpos(i - 1), cpos(i), ys[i - 1], ys[i]);
                    if (y0 - base) * (y1 - base) < 0.0 {
                        // crosses the baseline: split into two triangles
                        let t = (base - y0) / (y1 - y0);
                        let xm = x0 + (x1 - x0) * t;
                        quad(&mut m, Pos2::new(x0, base), Pos2::new(x0, y0), Pos2::new(xm, base), Pos2::new(xm, base), col);
                        quad(&mut m, Pos2::new(xm, base), Pos2::new(xm, base), Pos2::new(x1, y1), Pos2::new(x1, base), col);
                    } else {
                        quad(&mut m, Pos2::new(x0, base), Pos2::new(x0, y0), Pos2::new(x1, y1), Pos2::new(x1, base), col);
                    }
                }
                p.add(Shape::mesh(m));
                if ch.data_labels {
                    for (i, v) in ser.values.iter().enumerate() {
                        if let Some(v) = v {
                            p.text(Pos2::new(cpos(i), vpos(*v) - 3.0 * s), Align2::CENTER_BOTTOM, label(*v), theme::ui_font(font), TEXT);
                        }
                    }
                }
            }
        }
        ChartKind::Scatter => {
            for (k, ser) in data.series.iter().enumerate() {
                let col = series_color(k);
                for (i, v) in ser.values.iter().enumerate() {
                    let Some(v) = *v else { continue };
                    let q = Pos2::new(xpos(xs[i]), vpos(v));
                    marker(p, q, s, col);
                    if ch.data_labels {
                        p.text(q + Vec2::new(5.0 * s, 0.0), Align2::LEFT_CENTER, label(v), theme::ui_font(font), TEXT);
                    }
                }
            }
        }
        ChartKind::Pie | ChartKind::Doughnut => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, Range, Sheet, Value, Workbook};

    /// Paints every chart kind (with legend and data labels) over awkward data:
    /// negatives, gaps, a single point and an empty range must not panic.
    #[test]
    fn draws_all_kinds() {
        let mut sh = Sheet::new("S");
        let text = |t: &str| Some(Cell { value: Value::Text(t.into()), ..Default::default() });
        sh.set(0, 1, text("A"));
        sh.set(0, 2, text("B"));
        let vals = [[Some(1.0), Some(-2.0)], [Some(3.5), Some(4.0)], [None, Some(0.0)], [Some(7.0), Some(2.5)]];
        for (r, row) in vals.iter().enumerate() {
            let r = r as u32 + 1;
            sh.set(r, 0, text(&format!("c{r}")));
            for (c, v) in row.iter().enumerate() {
                if let Some(v) = v {
                    sh.set(r, c as u32 + 1, Some(Cell { value: Value::Number(*v), ..Default::default() }));
                }
            }
        }
        let wb = Workbook::empty_with(vec![sh]);
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for rg in [Range::new(0, 0, 4, 2), Range::new(1, 1, 1, 1), Range::new(10, 10, 12, 12)] {
                    for kind in ChartKind::ALL {
                        let mut ch = Chart::new(&wb, 0, rg, kind);
                        ch.legend = true;
                        ch.data_labels = true;
                        let data = chart::resolve(&wb, &ch);
                        for (w, z) in [(480.0, 1.0), (60.0, 0.5), (900.0, 2.0)] {
                            let rect = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(w, w * 0.6));
                            draw_chart(ui.painter(), rect, &ch, &data, z);
                        }
                    }
                }
            });
        });
    }
}
