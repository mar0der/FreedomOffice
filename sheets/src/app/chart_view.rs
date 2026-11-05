//! Charts on the grid: selection, moving and resizing, the floating Chart
//! Elements / Chart Styles buttons, the Insert > Charts menus and the
//! contextual Chart Design ribbon tab.

use std::f32::consts::{PI, TAU};

use eframe::egui::{self, epaint::Mesh, Align2, Color32, CursorIcon, Event, Key, Painter, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Vec2};

pub(super) use super::chart_draw::draw_chart;
use super::chart_draw::{arc_band, draw_chart_hits, quad, Elem, Hits};
use super::chart_pane::ChartDialog;
use super::grid::Layout;
use super::theme::{self, rgb};
use super::{App, Drag, RibbonTab, Scope};
use crate::chart::{self, Chart, ChartKind, ErrKind, ErrorBars, Grouping, Labels, Subtype, Trendline, MIN_SIZE, PALETTE};

const HANDLE: Color32 = Color32::from_rgb(0x70, 0x70, 0x70);
const BTN_H: f32 = 66.0;

/// Chart selection and drag state.
#[derive(Default)]
pub struct ChartUi {
    /// Selected chart: (sheet index, chart index).
    pub(super) sel: Option<(usize, usize)>,
    drag: Option<ChartDrag>,
    /// Ribbon tab to return to when the chart is deselected.
    prev_tab: Option<RibbonTab>,
    /// Selected element inside the chart.
    pub(super) elem: Option<Elem>,
    /// The Format pane is open.
    pub(super) pane: bool,
    /// Element areas of the selected chart, from the last paint.
    hits: Hits,
    pub(super) dialog: Option<ChartDialog>,
    /// Time of the last live (pane) edit, to merge rapid edits into one undo step.
    pub(super) live_at: f64,
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
    [r.left_top(), r.center_top(), r.right_top(), r.right_center(), r.right_bottom(), r.center_bottom(), r.left_bottom(), r.left_center()]
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

/// Insert ribbon families: (button label, tooltip, subtype kinds, "More" entry).
const FAMILIES: [(&str, &str, &[ChartKind]); 5] = [
    ("Column", "Insert Column or Bar Chart", &[ChartKind::Column, ChartKind::Bar]),
    ("Line", "Insert Line or Area Chart", &[ChartKind::Line, ChartKind::Area]),
    ("Pie", "Insert Pie or Doughnut Chart", &[ChartKind::Pie, ChartKind::Doughnut]),
    ("Scatter", "Insert Scatter (X, Y) Chart", &[ChartKind::Scatter]),
    ("Radar", "Insert Radar Chart", &[ChartKind::Radar]),
];

fn section_name(k: ChartKind) -> &'static str {
    match k {
        ChartKind::Column => "2-D Column",
        ChartKind::Bar => "2-D Bar",
        ChartKind::Line => "2-D Line",
        ChartKind::Area => "2-D Area",
        ChartKind::Pie => "2-D Pie",
        ChartKind::Doughnut => "Doughnut",
        ChartKind::Scatter => "Scatter",
        ChartKind::Radar => "Radar",
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
        self.chart.sel = None;
        self.chart.drag = None;
        self.chart.elem = None;
        self.chart.hits.clear();
    }

    fn select_chart(&mut self, i: usize) {
        if self.chart.sel != Some((self.wb.active, i)) {
            self.chart.elem = None;
        }
        self.chart.sel = Some((self.wb.active, i));
        if self.tab != RibbonTab::ChartDesign {
            self.chart.prev_tab = Some(self.tab);
        }
        self.tab = RibbonTab::ChartDesign;
    }

    /// The range a new chart is drawn from: the selection, or the data region around the cursor.
    pub(super) fn chart_source_range(&self) -> crate::model::Range {
        let sel = self.sel();
        if sel.is_single() { self.current_region() } else { sel }
    }

    /// Inserts the first (default) subtype of a chart kind.
    pub fn insert_chart(&mut self, kind: ChartKind) {
        if let Some(st) = chart::subtypes_of(kind).next() {
            self.insert_chart_sub(st);
        }
    }

    pub fn insert_chart_sub(&mut self, st: &Subtype) {
        if !self.commit_edit(false) {
            return;
        }
        let si = self.wb.active;
        let ch = Chart::with_subtype(&self.wb, si, self.chart_source_range(), st);
        self.insert_chart_value(ch);
    }

    /// Adds a finished chart to the active sheet and selects it.
    pub(super) fn insert_chart_value(&mut self, mut ch: Chart) {
        if self.protect_blocks(super::Change::Objects) {
            return;
        }
        let si = self.wb.active;
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
        self.select_chart(self.sheet().charts.len() - 1);
        self.status = None;
        self.changed();
    }

    /// Applies an undoable change to the selected chart.
    pub(super) fn edit_chart(&mut self, f: impl FnOnce(&mut Chart)) {
        if let Some(i) = self.selected_chart() {
            self.edit_chart_at((self.wb.active, i), f);
        }
    }

    pub(super) fn edit_chart_at(&mut self, (si, i): (usize, usize), f: impl FnOnce(&mut Chart)) {
        let Some(old) = self.wb.sheets.get(si).and_then(|sh| sh.charts.get(i)) else { return };
        let mut ch = old.clone();
        f(&mut ch);
        if &ch == old || self.protect_blocks_at(si, super::Change::Objects) {
            return;
        }
        self.push_undo(&[Scope::Sheet(si)]);
        self.wb.sheets[si].charts[i] = ch;
        self.changed();
    }

    /// Replaces the selected chart from a live editor (Format pane): edits in quick
    /// succession (typing, dragging a value) share one undo step.
    pub(super) fn set_chart_live(&mut self, now: f64, ch: Chart) {
        let Some(i) = self.selected_chart() else { return };
        let si = self.wb.active;
        if self.wb.sheets[si].charts[i] == ch || self.protect_blocks(super::Change::Objects) {
            return;
        }
        if now - self.chart.live_at > 0.8 {
            self.push_undo(&[Scope::Sheet(si)]);
        }
        self.chart.live_at = now;
        self.wb.sheets[si].charts[i] = ch;
        self.changed();
    }

    fn delete_chart(&mut self) {
        let Some(i) = self.selected_chart() else { return };
        if self.protect_blocks(super::Change::Objects) {
            return;
        }
        self.push_undo(&[Scope::Sheet(self.wb.active)]);
        self.sheet_mut().charts.remove(i);
        self.deselect_chart();
        self.chart.pane = false;
        self.changed();
    }

    /// Keyboard while a chart is selected; returns true when the keys were consumed.
    pub(super) fn chart_keys(&mut self, ctx: &egui::Context) -> bool {
        if self.selected_chart().is_none() {
            return false;
        }
        if ctx.wants_keyboard_input() || self.chart.dialog.is_some() {
            return true;
        }
        for ev in ctx.input(|i| i.events.clone()) {
            let Event::Key { key, pressed: true, modifiers: m, .. } = ev else { continue };
            let ctrl = m.ctrl || m.command;
            match key {
                Key::Delete | Key::Backspace => self.delete_chart(),
                Key::Escape if self.chart.elem.is_some_and(|e| e != Elem::ChartArea) => self.chart.elem = Some(Elem::ChartArea),
                Key::Escape => self.deselect_chart(),
                Key::Z if ctrl => self.undo(),
                Key::Y if ctrl => self.redo(),
                Key::Num1 if ctrl => self.chart.pane = true,
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

    /// The smallest element of the selected chart under `pos`.
    fn elem_at(&self, pos: Pos2) -> Option<Elem> {
        self.chart.hits.iter().filter(|(r, _)| r.expand(2.0).contains(pos)).min_by(|a, b| a.0.area().total_cmp(&b.0.area())).map(|(_, e)| *e)
    }

    /// Mouse over the grid; returns true when a chart took the event.
    pub(super) fn chart_pointer(&mut self, ui: &egui::Ui, lay: &Layout, pos: Pos2, hovered: bool, pressed: bool, down: bool) -> bool {
        if let Some(mut d) = self.chart.drag {
            if down {
                if !d.moved && (pos - d.start).length() > 3.0 {
                    if self.protect_blocks(super::Change::Objects) {
                        self.chart.drag = None;
                        return true;
                    }
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
                    let was = self.selected_chart() == Some(i);
                    self.select_chart(i);
                    if handle.is_none() {
                        // Hits are known once the chart has been painted selected.
                        self.chart.elem = if was { self.elem_at(pos).or(Some(Elem::ChartArea)) } else { Some(Elem::ChartArea) };
                        if was && ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
                            self.chart.pane = true;
                        }
                    }
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
                    if let Some(x) = s.xs_ref {
                        outline(x, Color32::from_rgb(0x9B, 0x4F, 0x96));
                    }
                }
                if ch.custom.is_some() {
                    for s in &data.series {
                        outline(s.refr, theme::GRID_ACCENT);
                    }
                } else if let (Some(a), Some(b)) = (data.series.first(), data.series.last()) {
                    outline(a.refr.union(&b.refr), theme::GRID_ACCENT);
                }
            }
        }
        let mut sel_rect = None;
        for i in 0..n {
            let ch = self.sheet().charts[i].clone();
            let (rect, clip) = self.chart_screen_rect(lay, &ch);
            if !clip.intersects(rect.expand(6.0)) {
                continue;
            }
            let data = chart::resolve(&self.wb, &ch);
            let p = ui.painter().with_clip_rect(clip.intersect(rect));
            let hits = draw_chart_hits(&p, rect, &ch, &data, self.zoom);
            if sel == Some(i) {
                self.chart.hits = hits;
                let p = ui.painter().with_clip_rect(clip.intersect(rect.expand(6.0)));
                if let Some(e) = self.chart.elem.filter(|e| *e != Elem::ChartArea) {
                    for (r, _) in self.chart.hits.iter().filter(|(_, x)| *x == e) {
                        p.rect_stroke(r.expand(2.0), 0.0, Stroke::new(1.0, theme::GRID_ACCENT), StrokeKind::Middle);
                    }
                }
                p.rect_stroke(rect, 0.0, Stroke::new(1.0, HANDLE), StrokeKind::Middle);
                for h in handles(rect) {
                    p.circle_filled(h, 4.0, Color32::WHITE);
                    p.circle_stroke(h, 4.0, Stroke::new(1.0, HANDLE));
                }
                if clip.contains(rect.right_top()) || clip.intersects(rect) {
                    sel_rect = Some(rect);
                }
            }
        }
        if let Some(rect) = sel_rect {
            self.chart_side_buttons(ui.ctx(), rect, lay.cells);
        }
    }

    /// The floating Chart Elements (+) and Chart Styles (brush) buttons beside a selected chart.
    fn chart_side_buttons(&mut self, ctx: &egui::Context, rect: Rect, cells: Rect) {
        let x = (rect.right() + 6.0).min(cells.right() - 30.0);
        let y = rect.top().max(cells.top());
        egui::Area::new(egui::Id::new("chart_side_buttons")).fixed_pos(Pos2::new(x, y)).order(egui::Order::Middle).show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            let plus = side_button(ui, "Chart Elements", |p, r| {
                let s = Stroke::new(2.0, Color32::from_gray(80));
                p.line_segment([r.center() - Vec2::new(6.0, 0.0), r.center() + Vec2::new(6.0, 0.0)], s);
                p.line_segment([r.center() - Vec2::new(0.0, 6.0), r.center() + Vec2::new(0.0, 6.0)], s);
            });
            egui::Popup::menu(&plus).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).align(egui::RectAlign::RIGHT_START).show(|ui| {
                self.chart_elements_menu(ui);
            });
            let brush = side_button(ui, "Chart Styles", paint_brush_icon);
            egui::Popup::menu(&brush).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).align(egui::RectAlign::RIGHT_START).show(|ui| {
                self.chart_styles_menu(ui, true);
            });
        });
    }

    /// Chart Elements checklist (the "+" button and Add Chart Element).
    pub(super) fn chart_elements_menu(&mut self, ui: &mut egui::Ui) {
        let Some(i) = self.selected_chart() else { return };
        let ch = self.sheet().charts[i].clone();
        let n = chart::resolve(&self.wb, &ch).series.len();
        ui.set_min_width(190.0);
        ui.label(egui::RichText::new("CHART ELEMENTS").size(11.0).strong());
        ui.add_space(2.0);
        let axes = ch.has_axes();
        let xy = ch.kind == ChartKind::Scatter;
        let radar = ch.kind == ChartKind::Radar;
        let target = match self.chart.elem {
            Some(Elem::Series(k)) if k < n => k,
            _ => 0,
        };
        let mut c = ch.clone();
        let mut more: Option<Elem> = None;
        let mut row = |ui: &mut egui::Ui, label: &str, on: bool, elem: Option<Elem>| -> bool {
            let mut v = on;
            let mut changed = false;
            ui.horizontal(|ui| {
                changed = ui.checkbox(&mut v, label).changed();
                if let Some(e) = elem {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("›").on_hover_text("More Options...").clicked() {
                            more = Some(e);
                        }
                    });
                }
            });
            changed
        };
        if axes && row(ui, "Axes", c.x_axis.visible || c.y_axis.visible, Some(Elem::YAxis)) {
            let on = !(c.x_axis.visible || c.y_axis.visible);
            c.x_axis.visible = on;
            c.y_axis.visible = on;
            c.y2_axis.visible = on;
        }
        if axes && !radar && row(ui, "Axis Titles", c.x_axis.title.is_some() || c.y_axis.title.is_some(), Some(Elem::YTitle)) {
            let on = !(c.x_axis.title.is_some() || c.y_axis.title.is_some());
            c.x_axis.title = on.then(String::new);
            c.y_axis.title = on.then(String::new);
            c.y2_axis.title = (on && c.has_secondary(n)).then(String::new);
        }
        if row(ui, "Chart Title", c.title.as_deref() != Some(""), Some(Elem::Title)) {
            c.title = if c.title.as_deref() == Some("") { None } else { Some(String::new()) };
        }
        if row(ui, "Data Labels", c.any_labels(), Some(Elem::DataLabels)) {
            let on = !c.any_labels();
            c.labels = if on { Labels { value: true, ..Labels::default() } } else { Labels { pos: c.labels.pos, num_fmt: c.labels.num_fmt.clone(), ..Labels::default() } };
            for f in &mut c.series_fmt {
                f.labels = None;
            }
        }
        if axes && !xy && !radar && row(ui, "Data Table", c.data_table, Some(Elem::DataTable)) {
            c.data_table = !c.data_table;
        }
        if axes && !radar && n > 0 && row(ui, "Error Bars", c.series_fmt.iter().any(|f| f.err.is_some()), Some(Elem::Series(target))) {
            let on = !c.series_fmt.iter().any(|f| f.err.is_some());
            for k in 0..n {
                c.fmt_mut(k).err = on.then(|| ErrorBars { kind: ErrKind::StdErr, ..Default::default() });
            }
        }
        if axes && row(ui, "Gridlines", c.y_axis.major_grid || c.x_axis.major_grid, Some(Elem::YAxis)) {
            let on = !(c.y_axis.major_grid || c.x_axis.major_grid);
            c.y_axis.major_grid = on;
            c.x_axis.major_grid = on && xy;
            c.y_axis.minor_grid = false;
            c.x_axis.minor_grid = false;
        }
        if row(ui, "Legend", c.legend, Some(Elem::Legend)) {
            c.legend = !c.legend;
        }
        let trend_ok = axes && !radar && c.grouping == Grouping::Standard;
        if trend_ok && n > 0 && row(ui, "Trendline", c.series_fmt.iter().any(|f| f.trend.is_some()), Some(Elem::Series(target))) {
            if c.series_fmt.iter().any(|f| f.trend.is_some()) {
                for f in &mut c.series_fmt {
                    f.trend = None;
                }
            } else {
                c.fmt_mut(target).trend = Some(Trendline::default());
            }
        }
        if c != ch {
            self.edit_chart(|x| *x = c);
        }
        if let Some(e) = more {
            self.chart.elem = Some(e);
            self.chart.pane = true;
            ui.close();
        }
    }

    /// Chart styles and colour palettes (brush button and Chart Design galleries).
    pub(super) fn chart_styles_menu(&mut self, ui: &mut egui::Ui, both: bool) {
        let Some(i) = self.selected_chart() else { return };
        let ch = self.sheet().charts[i].clone();
        let data = chart::resolve(&self.wb, &ch);
        if both {
            ui.label(egui::RichText::new("STYLE").size(11.0).strong());
        }
        egui::Grid::new("chart_style_grid").spacing(Vec2::splat(6.0)).show(ui, |ui| {
            for k in 0..chart::STYLES.len() {
                let mut c = ch.clone();
                chart::apply_style(&mut c, k);
                c.legend = false;
                let (r, resp) = ui.allocate_exact_size(Vec2::new(96.0, 64.0), Sense::click());
                draw_chart(&ui.painter().with_clip_rect(r), r, &c, &data, 0.32);
                let on = ch.bg == c.bg && ch.plot_bg == c.plot_bg;
                ui.painter().rect_stroke(r, 0.0, Stroke::new(if on { 2.0 } else { 1.0 }, if on { theme::accent() } else { theme::border() }), StrokeKind::Outside);
                if resp.on_hover_text(format!("Style {}", k + 1)).clicked() {
                    self.edit_chart(|x| chart::apply_style(x, k));
                }
                if k % 2 == 1 {
                    ui.end_row();
                }
            }
        });
        if both {
            ui.separator();
            ui.label(egui::RichText::new("COLOR").size(11.0).strong());
            self.chart_palette_list(ui);
        }
    }

    pub(super) fn chart_palette_list(&mut self, ui: &mut egui::Ui) {
        let Some(i) = self.selected_chart() else { return };
        let cur = self.sheet().charts[i].palette;
        for (k, (name, cols)) in chart::PALETTES.iter().enumerate() {
            let (r, resp) = ui.allocate_exact_size(Vec2::new(150.0, 18.0), Sense::click());
            if resp.hovered() || cur as usize == k {
                ui.painter().rect_filled(r, 2.0, if cur as usize == k { theme::checked() } else { theme::hover() });
            }
            for (j, c) in cols.iter().enumerate() {
                ui.painter().rect_filled(Rect::from_min_size(r.min + Vec2::new(4.0 + j as f32 * 24.0, 3.0), Vec2::new(22.0, 12.0)), 0.0, rgb(*c));
            }
            if resp.on_hover_text(*name).clicked() {
                self.edit_chart(|x| {
                    x.palette = k as u8;
                    for f in &mut x.series_fmt {
                        f.fill = None;
                        f.line = None;
                    }
                });
            }
        }
    }

    // ------------------------------------------------------------------
    // Ribbon
    // ------------------------------------------------------------------

    /// Insert > Charts group content.
    pub(super) fn chart_insert_buttons(&mut self, ui: &mut egui::Ui) {
        if big_button(ui, "Recommended\nCharts", "See the charts suited to the selected data", |p, r| paint_kind_icon(p, r, ChartKind::Column)).clicked() {
            self.open_insert_chart_dialog(None);
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(2.0, 2.0);
            let mut row = |ui: &mut egui::Ui, fams: &[usize], combo: bool| {
                ui.horizontal(|ui| {
                    for &f in fams {
                        let (_, tip, kinds) = FAMILIES[f];
                        let resp = small_icon_button(ui, tip, |p, r| paint_kind_icon(p, r, kinds[0]));
                        egui::Popup::menu(&resp).show(|ui| self.chart_family_menu(ui, kinds));
                    }
                    if combo {
                        let resp = small_icon_button(ui, "Insert Combo Chart", |p, r| paint_subtype_icon(p, r, &chart::SUBTYPES[chart::SUBTYPES.len() - 3]));
                        egui::Popup::menu(&resp).show(|ui| {
                            ui.label(egui::RichText::new("Combo").strong());
                            ui.horizontal(|ui| {
                                for st in chart::combo_subtypes() {
                                    if subtype_tile(ui, st, false).clicked() {
                                        self.insert_chart_sub(st);
                                        ui.close();
                                    }
                                }
                            });
                            ui.separator();
                            if ui.button("Create Custom Combo Chart...").clicked() {
                                self.open_insert_chart_dialog(Some(ChartKind::Column));
                                if let Some(ChartDialog::Type(t)) = &mut self.chart.dialog {
                                    t.combo = true;
                                }
                                ui.close();
                            }
                        });
                    }
                });
            };
            row(ui, &[0, 1, 2], false);
            row(ui, &[3, 4], true);
        });
    }

    /// Dropdown with the subtypes of a chart family.
    fn chart_family_menu(&mut self, ui: &mut egui::Ui, kinds: &[ChartKind]) {
        ui.set_min_width(200.0);
        for &k in kinds {
            ui.label(egui::RichText::new(section_name(k)).strong());
            ui.horizontal_wrapped(|ui| {
                ui.set_max_width(170.0);
                for st in chart::subtypes_of(k) {
                    if subtype_tile(ui, st, false).clicked() {
                        self.insert_chart_sub(st);
                        ui.close();
                    }
                }
            });
        }
        ui.separator();
        if ui.button(format!("More {} Charts...", kinds[0].label())).clicked() {
            self.open_insert_chart_dialog(Some(kinds[0]));
            ui.close();
        }
    }

    /// The accent-coloured contextual tab label shown while a chart is selected.
    pub(super) fn chart_tab_button(&mut self, ui: &mut egui::Ui) {
        if self.selected_chart().is_none() {
            if self.chart.sel.is_some() {
                // The chart went away (other sheet, undo): drop the stale selection.
                self.deselect_chart();
            }
            self.chart.pane = false;
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

        self.group(ui, "Chart Layouts", |app, ui| {
            let resp = big_button(ui, "Add Chart\nElement", "Add elements to the chart", paint_add_element_icon);
            egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| app.chart_elements_menu(ui));
            let resp = big_button(ui, "Quick\nLayout", "Change the overall layout of the chart", paint_layout_icon);
            egui::Popup::menu(&resp).show(|ui| {
                let data = chart::resolve(&app.wb, &ch);
                egui::Grid::new("quick_layouts").spacing(Vec2::splat(6.0)).show(ui, |ui| {
                    for k in 1..=11u8 {
                        let mut c = ch.clone();
                        chart::apply_quick_layout(&mut c, k);
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(84.0, 60.0), Sense::click());
                        draw_chart(&ui.painter().with_clip_rect(r), r, &c, &data, 0.28);
                        ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, theme::border()), StrokeKind::Outside);
                        if resp.on_hover_text(format!("Layout {k}")).clicked() {
                            app.edit_chart(|x| chart::apply_quick_layout(x, k));
                            ui.close();
                        }
                        if k % 3 == 0 {
                            ui.end_row();
                        }
                    }
                });
            });
        });
        self.group(ui, "Chart Styles", |app, ui| {
            let resp = big_button(ui, "Change\nColors", "Choose a colour palette for the chart", paint_colors_icon);
            egui::Popup::menu(&resp).show(|ui| app.chart_palette_list(ui));
            let data = chart::resolve(&app.wb, &ch);
            for k in 0..4 {
                let mut c = ch.clone();
                chart::apply_style(&mut c, k);
                c.legend = false;
                let (r, resp) = ui.allocate_exact_size(Vec2::new(62.0, 44.0), Sense::click());
                draw_chart(&ui.painter().with_clip_rect(r), r, &c, &data, 0.2);
                let on = ch.bg == c.bg && ch.plot_bg == c.plot_bg;
                ui.painter().rect_stroke(r, 0.0, Stroke::new(if on { 2.0 } else { 1.0 }, if on { theme::accent() } else { theme::border() }), StrokeKind::Outside);
                if resp.on_hover_text(format!("Style {}", k + 1)).clicked() {
                    app.edit_chart(|x| chart::apply_style(x, k));
                }
            }
            let more = ui.add(egui::Button::new("▾").min_size(Vec2::new(16.0, 44.0))).on_hover_text("More styles");
            egui::Popup::menu(&more).show(|ui| app.chart_styles_menu(ui, false));
        });
        self.group(ui, "Data", |app, ui| {
            if big_button(ui, "Switch\nRow/Column", "Swap the data over the axis", paint_switch_icon).clicked() {
                app.edit_chart(Chart::switch_row_col);
            }
            if big_button(ui, "Select\nData", "Change the data range included in the chart", paint_select_data_icon).clicked() {
                app.open_select_data_dialog();
            }
        });
        self.group(ui, "Type", |app, ui| {
            if big_button(ui, "Change\nChart Type", "Change to a different type of chart", |p, r| paint_kind_icon(p, r, ch.kind)).clicked() {
                app.open_change_type_dialog();
            }
        });
        self.group(ui, "Location", |app, ui| {
            if big_button(ui, "Move\nChart", "Move this chart to another sheet", paint_move_icon).clicked() {
                app.open_move_chart_dialog();
            }
        });
        self.group(ui, "Format", |app, ui| {
            if big_button(ui, "Format\nPane", "Format the selected chart element (Ctrl+1)", paint_format_icon).clicked() {
                app.chart.pane = !app.chart.pane;
            }
            if big_button(ui, "Delete\nChart", "Delete the selected chart (Delete)", paint_delete_icon).clicked() {
                app.delete_chart();
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Ribbon widgets and icons (drawn, no image assets)
// ---------------------------------------------------------------------------

fn side_button(ui: &mut egui::Ui, tip: &str, icon: impl FnOnce(&Painter, Rect)) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
    ui.painter().rect_filled(rect, 2.0, if resp.hovered() { Color32::from_gray(235) } else { Color32::WHITE });
    ui.painter().rect_stroke(rect, 2.0, Stroke::new(1.0, Color32::from_gray(170)), StrokeKind::Inside);
    icon(ui.painter(), rect.shrink(4.0));
    resp.on_hover_text(tip)
}

fn small_icon_button(ui: &mut egui::Ui, tip: &str, icon: impl FnOnce(&Painter, Rect)) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(34.0, 30.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 3.0, theme::hover());
    }
    icon(ui.painter(), Rect::from_min_size(rect.min + Vec2::new(3.0, 5.0), Vec2::splat(20.0)));
    let c = Pos2::new(rect.right() - 5.0, rect.center().y + 2.0);
    ui.painter().add(Shape::convex_polygon(vec![c + Vec2::new(-3.0, -2.0), c + Vec2::new(3.0, -2.0), c + Vec2::new(0.0, 2.0)], theme::text_dim(), Stroke::NONE));
    resp.on_hover_text(tip)
}

/// A clickable subtype icon (Insert menus and the chart type dialogs).
pub(super) fn subtype_tile(ui: &mut egui::Ui, st: &Subtype, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 3.0, theme::checked());
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 3.0, theme::hover());
    }
    if selected || resp.hovered() {
        ui.painter().rect_stroke(rect, 3.0, Stroke::new(1.0, theme::accent()), StrokeKind::Inside);
    }
    ui.painter().rect_filled(rect.shrink(6.0), 0.0, Color32::WHITE);
    paint_subtype_icon(ui.painter(), rect.shrink(9.0), st);
    resp.on_hover_text(st.name)
}

fn big_button(ui: &mut egui::Ui, label: &str, tip: &str, icon: impl FnOnce(&Painter, Rect)) -> egui::Response {
    let font = theme::ui_font(12.0);
    let w = label.lines().map(|l| ui.painter().layout_no_wrap(l.to_string(), font.clone(), theme::text()).size().x).fold(0.0, f32::max);
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

pub(super) fn paint_kind_icon(p: &Painter, r: Rect, kind: ChartKind) {
    if let Some(st) = chart::subtypes_of(kind).next() {
        paint_subtype_icon(p, r, st);
    }
}

/// Miniature of a chart subtype.
pub(super) fn paint_subtype_icon(p: &Painter, r: Rect, st: &Subtype) {
    let (c0, c1, c2) = (series_color(0), series_color(1), series_color(2));
    let at = |fx: f32, fy: f32| Pos2::new(r.left() + fx * r.width(), r.top() + fy * r.height());
    let axis = Stroke::new(1.0, theme::icon_line());
    let a = [0.5, 0.8, 0.35];
    let b = [0.3, 0.45, 0.6];
    let lw = (r.width() / 14.0).clamp(1.2, 2.5);
    let dot = (r.width() / 12.0).clamp(1.5, 3.0);
    if st.combo != 0 {
        p.line_segment([at(0.0, 0.95), at(1.0, 0.95)], axis);
        if st.combo == 3 {
            let mut m = Mesh::default();
            quad(&mut m, at(0.0, 0.95), at(0.0, 0.5), at(1.0, 0.3), at(1.0, 0.95), c0);
            p.add(Shape::mesh(m));
            for (i, h) in [0.45, 0.6, 0.35].iter().enumerate() {
                let x = 0.12 + i as f32 * 0.3;
                p.rect_filled(Rect::from_min_max(at(x, 0.95 - h), at(x + 0.16, 0.95)), 0.0, c1);
            }
        } else {
            for (i, h) in [0.45, 0.7, 0.35].iter().enumerate() {
                let x = 0.1 + i as f32 * 0.3;
                p.rect_filled(Rect::from_min_max(at(x, 0.95 - h), at(x + 0.2, 0.95)), 0.0, c0);
            }
            let v: Vec<Pos2> = [0.6, 0.3, 0.15].iter().enumerate().map(|(i, y)| at(0.2 + i as f32 * 0.3, *y)).collect();
            p.add(Shape::line(v, Stroke::new(lw, c1)));
            if st.combo == 2 {
                p.line_segment([at(1.0, 0.0), at(1.0, 0.95)], axis);
            }
        }
        return;
    }
    let stacked = st.grouping == Grouping::Stacked;
    let pct = st.grouping == Grouping::Percent;
    match st.kind {
        ChartKind::Column | ChartKind::Bar => {
            let hor = st.kind == ChartKind::Bar;
            for i in 0..3 {
                let (va, vb) = if pct { (a[i] / (a[i] + b[i]), 1.0 - a[i] / (a[i] + b[i])) } else if stacked { (a[i] * 0.55, b[i] * 0.55) } else { (a[i], b[i]) };
                let segs: Vec<(f32, f32, f32, Color32)> = if stacked || pct {
                    let x = 0.12 + i as f32 * 0.3;
                    vec![(x, 0.0, va, c0), (x, va, vb, c1)]
                } else {
                    let x = 0.06 + i as f32 * 0.32;
                    vec![(x, 0.0, va, c0), (x + 0.13, 0.0, vb, c1)]
                };
                for (x, from, len, col) in segs {
                    let w = if stacked || pct { 0.2 } else { 0.13 };
                    let rr = if hor {
                        Rect::from_min_max(at(0.05 + from * 0.92, 1.0 - x - w), at(0.05 + (from + len) * 0.92, 1.0 - x))
                    } else {
                        Rect::from_min_max(at(x, 0.95 - (from + len) * 0.92), at(x + w, 0.95 - from * 0.92))
                    };
                    p.rect_filled(rr, 0.0, col);
                }
            }
            if hor {
                p.line_segment([at(0.05, 0.0), at(0.05, 1.0)], axis);
            } else {
                p.line_segment([at(0.0, 0.95), at(1.0, 0.95)], axis);
            }
        }
        ChartKind::Line => {
            p.line_segment([at(0.0, 0.95), at(1.0, 0.95)], axis);
            let s1 = [0.4, 0.25, 0.3, 0.15];
            let s2 = [0.2, 0.35, 0.25, 0.4];
            let lines: [Vec<f32>; 2] = if pct {
                [s1.iter().zip(&s2).map(|(x, y)| x / (x + y)).collect(), vec![1.0; 4]]
            } else if stacked {
                [s1.to_vec(), s1.iter().zip(&s2).map(|(x, y)| x + y).collect()]
            } else {
                [vec![0.3, 0.55, 0.45, 0.85], vec![0.15, 0.3, 0.2, 0.5]]
            };
            for (vals, c) in lines.iter().zip([c0, c1]) {
                let v: Vec<Pos2> = vals.iter().enumerate().map(|(i, y)| at(0.08 + i as f32 * 0.28, 0.92 - y * 0.85)).collect();
                p.add(Shape::line(v.clone(), Stroke::new(lw, c)));
                if st.markers {
                    for q in v {
                        p.circle_filled(q, dot, c);
                    }
                }
            }
        }
        ChartKind::Area => {
            let s1 = [0.35, 0.55, 0.4, 0.6];
            let s2 = [0.3, 0.2, 0.35, 0.25];
            let (bot, top): ([f32; 4], [f32; 4]) = if pct {
                let f: Vec<f32> = s1.iter().zip(&s2).map(|(x, y)| x / (x + y)).collect();
                ([f[0], f[1], f[2], f[3]], [1.0; 4])
            } else if stacked {
                (s1, [s1[0] + s2[0], s1[1] + s2[1], s1[2] + s2[2], s1[3] + s2[3]])
            } else {
                ([0.4, 0.75, 0.5, 0.85], [0.25, 0.45, 0.3, 0.4])
            };
            let mut m = Mesh::default();
            let y = |v: f32| 0.95 - v * 0.9;
            let (back, front, base2): (&[f32; 4], &[f32; 4], bool) = if stacked || pct { (&top, &bot, true) } else { (&bot, &top, false) };
            for i in 0..3 {
                let (x0, x1) = (i as f32 / 3.0, (i + 1) as f32 / 3.0);
                quad(&mut m, at(x0, 0.95), at(x0, y(back[i])), at(x1, y(back[i + 1])), at(x1, 0.95), if base2 { c1 } else { c0 });
            }
            for i in 0..3 {
                let (x0, x1) = (i as f32 / 3.0, (i + 1) as f32 / 3.0);
                quad(&mut m, at(x0, 0.95), at(x0, y(front[i])), at(x1, y(front[i + 1])), at(x1, 0.95), if base2 { c0 } else { c1 });
            }
            p.add(Shape::mesh(m));
            p.line_segment([at(0.0, 0.95), at(1.0, 0.95)], axis);
        }
        ChartKind::Scatter => {
            p.line_segment([at(0.05, 0.0), at(0.05, 0.95)], axis);
            p.line_segment([at(0.05, 0.95), at(1.0, 0.95)], axis);
            let pts = [(0.15, 0.75), (0.35, 0.45), (0.55, 0.55), (0.75, 0.25), (0.92, 0.12)];
            let v: Vec<Pos2> = pts.iter().map(|(x, y)| at(*x, *y)).collect();
            if st.lines {
                let line = if st.smooth {
                    let mut out = Vec::new();
                    for w in v.windows(2) {
                        for k in 0..6 {
                            let t = k as f32 / 6.0;
                            let e = (1.0 - (t * PI).cos()) / 2.0;
                            out.push(Pos2::new(w[0].x + (w[1].x - w[0].x) * t, w[0].y + (w[1].y - w[0].y) * e));
                        }
                    }
                    out.push(*v.last().unwrap());
                    out
                } else {
                    v.clone()
                };
                p.add(Shape::line(line, Stroke::new(lw * 0.8, c0)));
            }
            if st.markers || !st.lines {
                for q in v {
                    p.circle_filled(q, dot, c0);
                }
            }
        }
        ChartKind::Radar => {
            let c = r.center();
            let rad = r.width().min(r.height()) / 2.0;
            let pt = |i: usize, f: f32| c + Vec2::new((-PI / 2.0 + TAU * i as f32 / 5.0).cos(), (-PI / 2.0 + TAU * i as f32 / 5.0).sin()) * rad * f;
            let web: Vec<Pos2> = (0..=5).map(|i| pt(i % 5, 1.0)).collect();
            p.add(Shape::line(web, Stroke::new(1.0, Color32::from_gray(170))));
            let vals = [0.8, 0.5, 0.9, 0.4, 0.6];
            let poly: Vec<Pos2> = (0..5).map(|i| pt(i, vals[i])).collect();
            if st.filled {
                p.add(Shape::convex_polygon(poly, c0, Stroke::NONE));
            } else {
                let mut ring = poly.clone();
                ring.push(poly[0]);
                p.add(Shape::line(ring, Stroke::new(lw * 0.8, c0)));
                if st.markers {
                    for q in poly {
                        p.circle_filled(q, dot * 0.8, c0);
                    }
                }
            }
        }
        ChartKind::Pie | ChartKind::Doughnut => {
            let hole = if st.kind == ChartKind::Doughnut { 0.5 } else { 0.0 };
            let rad = r.width().min(r.height()) / 2.0 - 1.0;
            let fr = [0.45, 0.3, 0.25];
            let mut a = -PI / 2.0;
            for (i, f) in fr.iter().enumerate() {
                let b = a + f * TAU;
                arc_band(p, r.center(), rad * hole, rad, a, b, [c0, c1, c2][i]);
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

fn paint_select_data_icon(p: &Painter, r: Rect) {
    let g = Stroke::new(1.0, Color32::from_gray(140));
    let t = r.shrink(3.0);
    p.rect_stroke(t, 0.0, g, StrokeKind::Inside);
    for k in 1..3 {
        let y = t.top() + t.height() * k as f32 / 3.0;
        p.line_segment([Pos2::new(t.left(), y), Pos2::new(t.right(), y)], g);
        let x = t.left() + t.width() * k as f32 / 3.0;
        p.line_segment([Pos2::new(x, t.top()), Pos2::new(x, t.bottom())], g);
    }
    p.rect_stroke(Rect::from_min_max(Pos2::new(t.left() + t.width() / 3.0, t.top() + t.height() / 3.0), t.max), 0.0, Stroke::new(2.0, theme::GRID_ACCENT), StrokeKind::Inside);
}

fn paint_add_element_icon(p: &Painter, r: Rect) {
    paint_kind_icon(p, r.shrink(3.0), ChartKind::Column);
    let c = r.right_top() + Vec2::new(-4.0, 4.0);
    p.circle_filled(c, 5.0, theme::GRID_ACCENT);
    p.line_segment([c - Vec2::new(3.0, 0.0), c + Vec2::new(3.0, 0.0)], Stroke::new(1.5, Color32::WHITE));
    p.line_segment([c - Vec2::new(0.0, 3.0), c + Vec2::new(0.0, 3.0)], Stroke::new(1.5, Color32::WHITE));
}

fn paint_layout_icon(p: &Painter, r: Rect) {
    let t = r.shrink(2.0);
    p.rect_stroke(t, 0.0, Stroke::new(1.0, Color32::from_gray(140)), StrokeKind::Inside);
    p.rect_filled(Rect::from_min_size(t.min + Vec2::new(4.0, 3.0), Vec2::new(t.width() - 8.0, 3.0)), 0.0, Color32::from_gray(150));
    for (i, h) in [0.4, 0.65, 0.3].iter().enumerate() {
        let x = t.left() + 5.0 + i as f32 * 6.0;
        p.rect_filled(Rect::from_min_max(Pos2::new(x, t.bottom() - 3.0 - h * 14.0), Pos2::new(x + 4.0, t.bottom() - 3.0)), 0.0, series_color(i));
    }
}

fn paint_colors_icon(p: &Painter, r: Rect) {
    for i in 0..4 {
        let rr = Rect::from_min_size(r.min + Vec2::new(2.0 + (i % 2) as f32 * 11.0, 2.0 + (i / 2) as f32 * 11.0), Vec2::splat(10.0));
        p.rect_filled(rr, 1.0, series_color(i));
    }
}

fn paint_brush_icon(p: &Painter, r: Rect) {
    let s = Stroke::new(2.0, Color32::from_gray(80));
    p.line_segment([r.right_top() + Vec2::new(-3.0, 2.0), r.center() + Vec2::new(1.0, 1.0)], s);
    p.circle_filled(r.center() + Vec2::new(-2.0, 3.0), 4.0, theme::GRID_ACCENT);
}

fn paint_move_icon(p: &Painter, r: Rect) {
    let s = Stroke::new(1.0, Color32::from_gray(140));
    let a = Rect::from_min_size(r.min + Vec2::new(1.0, 4.0), Vec2::new(14.0, 12.0));
    p.rect_stroke(a, 0.0, s, StrokeKind::Inside);
    paint_kind_icon(p, Rect::from_min_size(r.min + Vec2::new(10.0, 10.0), Vec2::splat(14.0)), ChartKind::Column);
}

fn paint_format_icon(p: &Painter, r: Rect) {
    let t = r.shrink(3.0);
    p.rect_stroke(t, 0.0, Stroke::new(1.0, Color32::from_gray(140)), StrokeKind::Inside);
    let pane = Rect::from_min_max(Pos2::new(t.right() - 8.0, t.top()), t.max);
    p.rect_filled(pane, 0.0, theme::GRID_ACCENT);
}

fn paint_delete_icon(p: &Painter, r: Rect) {
    let s = Stroke::new(2.0, Color32::from_rgb(0xC4, 0x2B, 0x1C));
    let r = r.shrink(6.0);
    p.line_segment([r.left_top(), r.right_bottom()], s);
    p.line_segment([r.right_top(), r.left_bottom()], s);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chart::{Dash, ErrDir, LabelPos, LegendPos, MarkerKind, TrendKind};
    use crate::model::{Cell, Range, Sheet, Value, Workbook};

    /// Paints every chart subtype with every element switched on over awkward data:
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
                    for (k, st) in chart::SUBTYPES.iter().enumerate() {
                        let mut ch = Chart::with_subtype(&wb, 0, rg, st);
                        ch.legend = true;
                        ch.legend_pos = LegendPos::ALL[k % 5];
                        ch.labels = Labels { value: true, category: k % 2 == 0, percent: true, pos: LabelPos::ALL[k % 10], ..Default::default() };
                        ch.data_table = true;
                        ch.x_axis.title = Some(String::new());
                        ch.y_axis.title = Some("Values".into());
                        ch.y_axis.minor_grid = true;
                        ch.x_axis.major_grid = true;
                        ch.y_axis.log_base = (k % 3 == 0).then_some(10.0);
                        ch.y_axis.reverse = k % 4 == 0;
                        ch.x_axis.reverse = k % 5 == 0;
                        ch.y_axis.min = (k % 7 == 0).then_some(-1.0);
                        let f = ch.fmt_mut(0);
                        f.trend = Some(Trendline { kind: TrendKind::ALL[k % 6], show_eq: true, show_r2: true, forward: 1.0, ..Default::default() });
                        f.err = Some(ErrorBars { kind: ErrKind::ALL[k % 4], amount: 10.0, dir: ErrDir::Both, cap: true });
                        f.marker = MarkerKind::ALL[k % 10];
                        f.dash = Dash::ALL[k % 7];
                        f.explode = 20;
                        f.secondary = k % 2 == 1;
                        let data = chart::resolve(&wb, &ch);
                        for (w, z) in [(480.0, 1.0), (60.0, 0.5), (900.0, 2.0)] {
                            let rect = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(w, w * 0.6));
                            let hits = draw_chart_hits(ui.painter(), rect, &ch, &data, z);
                            assert_eq!(hits[0].1, Elem::ChartArea);
                        }
                        paint_subtype_icon(ui.painter(), Rect::from_min_size(Pos2::ZERO, Vec2::splat(30.0)), st);
                    }
                }
            });
        });
    }

    /// Prints the workbook in FS_GALLERY to FS_GALLERY_PDF through the print pipeline
    /// (charts recorded from the screen painter), for checking chart output by eye.
    #[test]
    #[ignore]
    fn gallery_pdf() {
        let src = std::env::var("FS_GALLERY").expect("FS_GALLERY");
        let out = std::env::var("FS_GALLERY_PDF").expect("FS_GALLERY_PDF");
        let wb = crate::io::load(std::path::Path::new(&src)).unwrap();
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let opts = crate::print::LayoutOptions { what: crate::print::PrintWhat::ActiveSheet, selection: None, ignore_print_area: true, file_name: "", file_path: "" };
            let pages = crate::print::layout(&wb, &opts, &|si, k, r| super::super::print_ui::chart_ops(ctx, &wb, si, k, r));
            crate::print::pdf::export(&pages, std::path::Path::new(&out)).unwrap();
        });
    }

    /// Runs the Chart Design tab, Insert menus, Format pane (every element) and every chart
    /// dialog for each chart subtype over a few frames: nothing may panic.
    #[test]
    fn chart_ui_smoke() {
        let ctx = egui::Context::default();
        let mut app = App::with_context(&ctx, 0, None);
        let mut sh = Sheet::new("S");
        let text = |t: &str| Some(Cell { value: Value::Text(t.into()), ..Default::default() });
        sh.set(0, 1, text("A"));
        sh.set(0, 2, text("B"));
        for r in 1..5u32 {
            sh.set(r, 0, text(&format!("c{r}")));
            sh.set(r, 1, Some(Cell { value: Value::Number(r as f64 * 3.0), ..Default::default() }));
            sh.set(r, 2, Some(Cell { value: Value::Number(10.0 - r as f64), ..Default::default() }));
        }
        let mut wb = Workbook::empty_with(vec![sh]);
        let rg = Range::new(0, 0, 4, 2);
        for st in chart::SUBTYPES {
            let ch = Chart::with_subtype(&wb, 0, rg, st);
            wb.sheets[0].charts.push(ch);
        }
        app.wb = wb;
        app.sheet_mut().view.sel = rg;
        let elems = [
            Elem::ChartArea,
            Elem::PlotArea,
            Elem::Title,
            Elem::Legend,
            Elem::XAxis,
            Elem::YAxis,
            Elem::Y2Axis,
            Elem::XTitle,
            Elem::Series(0),
            Elem::Series(1),
            Elem::DataLabels,
            Elem::DataTable,
        ];
        let n = chart::SUBTYPES.len();
        for frame in 0..n * 2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                app.chart.sel = Some((0, frame % n));
                app.chart.elem = Some(elems[frame % elems.len()]);
                app.chart.pane = true;
                match frame % 4 {
                    0 => app.open_change_type_dialog(),
                    1 => app.open_select_data_dialog(),
                    2 => app.open_move_chart_dialog(),
                    _ => app.open_insert_chart_dialog(Some(ChartKind::Line)),
                }
                if let Some(ChartDialog::Type(t)) = &mut app.chart.dialog {
                    t.combo = frame % 8 == 0;
                }
                app.chart_format_pane(ctx);
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        app.chart_design_tab(ui);
                        app.chart_insert_buttons(ui);
                    });
                    app.chart_elements_menu(ui);
                    app.chart_styles_menu(ui, true);
                });
                app.chart_dialogs(ctx);
            });
        }
        assert_eq!(app.sheet().charts.len(), n);
    }
}
