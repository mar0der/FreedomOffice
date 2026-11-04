//! The worksheet grid: layout, painting and mouse interaction.

use eframe::egui::{
    self, text::LayoutJob, Align2, Color32, CursorIcon, FontFamily, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, TextFormat, Vec2,
};

use super::theme::{self, rgb};
use super::{App, Drag, EditMode, FilterPopup, Scope};
use crate::model::{col_name, BorderLine, HAlign, Range, Style, VAlign, Value, MAX_COLS, MAX_ROWS};
use crate::numfmt;

pub const SCROLLBAR: f32 = 14.0;

#[derive(Clone, Debug)]
pub struct Layout {
    pub rect: Rect,
    pub cells: Rect,
    pub hw: f32,
    pub hh: f32,
    /// (row, top y, height) of visible rows.
    pub rows: Vec<(u32, f32, f32)>,
    pub cols: Vec<(u32, f32, f32)>,
    pub avail_h: f32,
    pub avail_w: f32,
    pub frozen_y: f32,
    pub frozen_x: f32,
    pub vbar: Rect,
    pub hbar: Rect,
    pub total_rows: u32,
    pub total_cols: u32,
}

impl Layout {
    pub fn row_at(&self, y: f32) -> Option<u32> {
        self.rows.iter().find(|(_, t, h)| y >= *t && y < t + h).map(|r| r.0)
    }
    pub fn col_at(&self, x: f32) -> Option<u32> {
        self.cols.iter().find(|(_, l, w)| x >= *l && x < l + w).map(|c| c.0)
    }
    /// Row under y, clamped to the nearest visible row (for drags outside the grid).
    pub fn row_near(&self, y: f32) -> u32 {
        if let Some(r) = self.row_at(y) {
            return r;
        }
        if y < self.cells.top() || self.rows.is_empty() {
            self.rows.first().map(|r| r.0).unwrap_or(0)
        } else {
            self.rows.last().map(|r| r.0).unwrap_or(0)
        }
    }
    pub fn col_near(&self, x: f32) -> u32 {
        if let Some(c) = self.col_at(x) {
            return c;
        }
        if x < self.cells.left() || self.cols.is_empty() {
            self.cols.first().map(|c| c.0).unwrap_or(0)
        } else {
            self.cols.last().map(|c| c.0).unwrap_or(0)
        }
    }
    fn row_span(&self, r: u32) -> Option<(f32, f32)> {
        self.rows.iter().find(|x| x.0 == r).map(|x| (x.1, x.1 + x.2))
    }
    fn col_span(&self, c: u32) -> Option<(f32, f32)> {
        self.cols.iter().find(|x| x.0 == c).map(|x| (x.1, x.1 + x.2))
    }

    /// Screen y of the top edge of row r (estimated if off-screen).
    fn y_of(&self, r: u32, top_scroll: u32, fr: u32) -> f32 {
        if let Some((t, _)) = self.row_span(r) {
            return t;
        }
        if r >= fr && r < top_scroll {
            return self.frozen_y - 1.0;
        }
        match self.rows.last() {
            Some(last) if r > last.0 => last.1 + last.2 + 1.0,
            _ => self.cells.top(),
        }
    }
    fn y_end(&self, r: u32, top_scroll: u32, fr: u32) -> f32 {
        if let Some((_, b)) = self.row_span(r) {
            return b;
        }
        if r >= fr && r < top_scroll {
            return self.frozen_y - 1.0;
        }
        match self.rows.last() {
            Some(last) if r > last.0 => self.cells.bottom() + 2.0,
            _ => self.cells.top(),
        }
    }
    fn x_of(&self, c: u32, left_scroll: u32, fc: u32) -> f32 {
        if let Some((l, _)) = self.col_span(c) {
            return l;
        }
        if c >= fc && c < left_scroll {
            return self.frozen_x - 1.0;
        }
        match self.cols.last() {
            Some(last) if c > last.0 => last.1 + last.2 + 1.0,
            _ => self.cells.left(),
        }
    }
    fn x_end(&self, c: u32, left_scroll: u32, fc: u32) -> f32 {
        if let Some((_, r)) = self.col_span(c) {
            return r;
        }
        if c >= fc && c < left_scroll {
            return self.frozen_x - 1.0;
        }
        match self.cols.last() {
            Some(last) if c > last.0 => self.cells.right() + 2.0,
            _ => self.cells.left(),
        }
    }
}

pub fn font_px(pt: f32, zoom: f32) -> f32 {
    pt * 96.0 / 72.0 * zoom
}

/// What the grid shows for a cell: text, colour, alignment.
pub struct Shown {
    pub text: String,
    pub color: Option<Color32>,
    pub align: HAlign,
    pub is_number: bool,
}

pub fn shown(v: &Value, style: &Style, formula: Option<&str>, show_formulas: bool) -> Shown {
    if show_formulas {
        if let Some(f) = formula {
            return Shown { text: format!("={f}"), color: None, align: HAlign::Left, is_number: false };
        }
    }
    let f = numfmt::format_value(v, &style.num_fmt);
    let auto = match v {
        Value::Number(_) => HAlign::Right,
        Value::Bool(_) | Value::Error(_) => HAlign::Center,
        _ => HAlign::Left,
    };
    let align = if style.halign == HAlign::General { auto } else { style.halign };
    Shown { text: f.text, color: f.color.map(rgb), align, is_number: matches!(v, Value::Number(_)) }
}

impl App {
    pub fn cell_font(&mut self, style: &Style) -> FontId {
        let fam: FontFamily = self.fonts.cell_family(style.font.as_deref(), style.bold, style.italic);
        FontId::new(font_px(style.font_size(), self.zoom), fam)
    }

    pub fn compute_layout(&self, rect: Rect) -> Layout {
        let sh = self.sheet();
        let z: f32 = self.zoom;
        let (fr, fc) = sh.freeze;
        let max_row_label = sh.view.top + 60;
        let digits = (max_row_label + 1).to_string().len().max(3) as f32;
        let (hw, hh) = if self.show_headings { ((digits * 7.0 + 12.0) * z.max(0.8), (20.0 * z).max(16.0)) } else { (0.0, 0.0) };
        let vbar = Rect::from_min_max(Pos2::new(rect.right() - SCROLLBAR, rect.top() + hh), Pos2::new(rect.right(), rect.bottom() - SCROLLBAR));
        let hbar = Rect::from_min_max(Pos2::new(rect.left() + hw, rect.bottom() - SCROLLBAR), Pos2::new(rect.right() - SCROLLBAR, rect.bottom()));
        let cells = Rect::from_min_max(Pos2::new(rect.left() + hw, rect.top() + hh), Pos2::new(rect.right() - SCROLLBAR, rect.bottom() - SCROLLBAR));
        let mut rows = Vec::new();
        let mut y = cells.top();
        for r in 0..fr.min(MAX_ROWS) {
            let h = sh.row_height(r) * z;
            if h > 0.0 {
                rows.push((r, y, h));
            }
            y += h;
        }
        let frozen_y = y;
        let mut r = sh.view.top.max(fr);
        while y < cells.bottom() && r < MAX_ROWS {
            let h = sh.row_height(r) * z;
            if h > 0.0 {
                rows.push((r, y, h));
            }
            y += h;
            r += 1;
        }
        let mut cols = Vec::new();
        let mut x = cells.left();
        for c in 0..fc.min(MAX_COLS) {
            let w = sh.col_width(c) * z;
            if w > 0.0 {
                cols.push((c, x, w));
            }
            x += w;
        }
        let frozen_x = x;
        let mut c = sh.view.left.max(fc);
        while x < cells.right() && c < MAX_COLS {
            let w = sh.col_width(c) * z;
            if w > 0.0 {
                cols.push((c, x, w));
            }
            x += w;
            c += 1;
        }
        let (er, ec) = sh.extent().unwrap_or((0, 0));
        let vis_r = rows.len() as u32;
        let vis_c = cols.len() as u32;
        Layout {
            rect,
            cells,
            hw,
            hh,
            rows,
            cols,
            avail_h: cells.bottom() - frozen_y,
            avail_w: cells.right() - frozen_x,
            frozen_y,
            frozen_x,
            vbar,
            hbar,
            total_rows: (er.max(sh.view.active.0).max(sh.view.top + vis_r) + vis_r).min(MAX_ROWS),
            total_cols: (ec.max(sh.view.active.1).max(sh.view.left + vis_c) + vis_c).min(MAX_COLS),
        }
    }

    /// Screen rectangle of a range (clipped estimate for off-screen parts).
    pub fn range_rect(&self, lay: &Layout, rg: Range) -> Rect {
        let sh = self.sheet();
        let (fr, fc) = sh.freeze;
        let (top, left) = (sh.view.top.max(fr), sh.view.left.max(fc));
        Rect::from_min_max(
            Pos2::new(lay.x_of(rg.c1, left, fc), lay.y_of(rg.r1, top, fr)),
            Pos2::new(lay.x_end(rg.c2, left, fc), lay.y_end(rg.r2, top, fr)),
        )
    }

    pub fn grid_ui(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        let id = egui::Id::new("grid");
        let resp = ui.interact(rect, id, Sense::click_and_drag());
        if resp.clicked() || resp.drag_started() || resp.secondary_clicked() {
            resp.request_focus();
        }
        if ui.memory(|m| m.focused().is_none()) && self.dialog.is_none() && self.rename_tab.is_none() && !self.name_box_editing {
            resp.request_focus();
        }
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
            )
        });

        let lay = self.compute_layout(rect);
        self.layout = Some(lay.clone());
        self.grid_input(ui, &resp, &lay);
        let lay = self.compute_layout(rect);
        self.layout = Some(lay.clone());
        self.paint_grid(ui, &lay);
        self.context_menu(&resp);
    }

    // ------------------------------------------------------------------
    // Input
    // ------------------------------------------------------------------

    fn grid_input(&mut self, ui: &mut egui::Ui, resp: &egui::Response, lay: &Layout) {
        let (pressed, released, down, pos, dbl, mods, scroll, secondary) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
                i.pointer.primary_down(),
                i.pointer.interact_pos(),
                i.pointer.button_double_clicked(egui::PointerButton::Primary),
                // Prefer the modifiers recorded with the click itself (robust to fast key-up).
                i.events
                    .iter()
                    .find_map(|e| match e {
                        egui::Event::PointerButton { pressed: true, modifiers, .. } => Some(*modifiers),
                        _ => None,
                    })
                    .unwrap_or(i.modifiers),
                i.raw_scroll_delta,
                i.pointer.secondary_pressed(),
            )
        });
        let hovered = resp.hovered();

        // Mouse wheel
        if hovered && scroll != Vec2::ZERO {
            if mods.ctrl || mods.command {
                let f = if scroll.y > 0.0 { 1.1 } else { 1.0 / 1.1 };
                self.zoom = (self.zoom * f).clamp(0.1, 4.0);
                self.zoom = (self.zoom * 100.0).round() / 100.0;
            } else {
                let (mut dx, mut dy) = (scroll.x, scroll.y);
                if mods.shift && dx == 0.0 {
                    dx = dy;
                    dy = 0.0;
                }
                self.wheel_acc += Vec2::new(dx, dy);
                let step = 50.0 / 3.0;
                let rows = (self.wheel_acc.y / step).trunc();
                let cols = (self.wheel_acc.x / step).trunc();
                self.wheel_acc.y -= rows * step;
                self.wheel_acc.x -= cols * step;
                let (fr, fc) = self.sheet().freeze;
                let v = &mut self.sheet_mut().view;
                if rows != 0.0 {
                    v.top = (v.top as i64 - rows as i64).clamp(fr as i64, MAX_ROWS as i64 - 1) as u32;
                }
                if cols != 0.0 {
                    v.left = (v.left as i64 - cols as i64).clamp(fc as i64, MAX_COLS as i64 - 1) as u32;
                }
            }
        }

        let Some(pos) = pos else { return };
        self.hover_cell = if hovered && lay.cells.contains(pos) && self.drag == Drag::None {
            Some((lay.row_near(pos.y), lay.col_near(pos.x)))
        } else {
            None
        };

        if self.chart_pointer(ui, lay, pos, hovered, pressed, down) {
            return;
        }

        // Cursor icon
        if hovered && self.drag == Drag::None {
            ui.ctx().set_cursor_icon(self.hover_cursor(lay, pos));
        }

        if secondary && hovered {
            if let (Some(r), Some(c)) = (lay.row_at(pos.y), lay.col_at(pos.x)) {
                if !self.sel().contains(r, c) {
                    if self.edit.is_some() && !self.commit_edit(false) {
                        return;
                    }
                    self.select(r, c, false);
                }
            }
        }

        // egui reports double-clicks by timing only; require both clicks on the same spot.
        let dbl = dbl && !mods.shift && !mods.ctrl && self.last_press_pos.is_some_and(|p| p.distance(pos) < 6.0);
        if pressed && hovered {
            self.on_press(lay, pos, mods, dbl);
            self.last_press_pos = Some(pos);
        } else if dbl && hovered {
            self.on_double_click(lay, pos);
        }

        if down && self.drag != Drag::None {
            self.on_drag(lay, pos);
        }
        if released && self.drag != Drag::None {
            self.on_release(lay, pos);
        }
    }

    fn hover_cursor(&self, lay: &Layout, pos: Pos2) -> CursorIcon {
        if lay.vbar.contains(pos) || lay.hbar.contains(pos) {
            return CursorIcon::Default;
        }
        if pos.y < lay.cells.top() && pos.x > lay.cells.left() {
            if self.col_border_at(lay, pos.x).is_some() {
                return CursorIcon::ResizeColumn;
            }
            return CursorIcon::Default;
        }
        if pos.x < lay.cells.left() && pos.y > lay.cells.top() {
            if self.row_border_at(lay, pos.y).is_some() {
                return CursorIcon::ResizeRow;
            }
            return CursorIcon::Default;
        }
        if self.fill_handle_rect(lay).is_some_and(|r| r.expand(2.0).contains(pos)) {
            return CursorIcon::Crosshair;
        }
        if self.filter_button_at(lay, pos).is_some() {
            return CursorIcon::Default;
        }
        if self.on_sel_border(lay, pos) {
            return CursorIcon::Move;
        }
        if let (Some(r), Some(c)) = (lay.row_at(pos.y), lay.col_at(pos.x)) {
            if self.sheet().links.contains_key(&(r, c)) {
                return CursorIcon::PointingHand;
            }
        }
        CursorIcon::Cell
    }

    /// True when the pointer is on the selection outline (drag to move it).
    fn on_sel_border(&self, lay: &Layout, pos: Pos2) -> bool {
        if self.edit.is_some() || !lay.cells.contains(pos) {
            return false;
        }
        let sel = self.sel();
        if sel.is_full_cols() || sel.is_full_rows() {
            return false;
        }
        let r = self.range_rect(lay, sel);
        r.expand(3.0).contains(pos) && !r.shrink(3.0).contains(pos)
    }

    fn col_border_at(&self, lay: &Layout, x: f32) -> Option<u32> {
        lay.cols.iter().find(|(_, l, w)| (x - (l + w)).abs() <= 3.0).map(|c| c.0)
    }
    fn row_border_at(&self, lay: &Layout, y: f32) -> Option<u32> {
        lay.rows.iter().find(|(_, t, h)| (y - (t + h)).abs() <= 2.5).map(|r| r.0)
    }

    pub fn fill_handle_rect(&self, lay: &Layout) -> Option<Rect> {
        if self.edit.is_some() {
            return None;
        }
        let sel = self.sel();
        if sel.is_full_cols() || sel.is_full_rows() {
            return None;
        }
        let r = self.range_rect(lay, sel);
        let p = r.right_bottom();
        if !lay.cells.expand(3.0).contains(p) {
            return None;
        }
        Some(Rect::from_center_size(p, Vec2::splat(6.0)))
    }

    fn filter_button_at(&self, lay: &Layout, pos: Pos2) -> Option<u32> {
        let f = self.sheet().filter.as_ref()?;
        let hr = f.range.r1;
        let (t, b) = lay.row_span(hr)?;
        if pos.y < t || pos.y > b {
            return None;
        }
        for c in f.range.c1..=f.range.c2 {
            if let Some((_, r)) = lay.col_span(c) {
                let btn = Rect::from_min_max(Pos2::new(r - 17.0, b - 17.0), Pos2::new(r - 1.0, b - 1.0));
                if btn.contains(pos) {
                    return Some(c);
                }
            }
        }
        None
    }

    fn on_press(&mut self, lay: &Layout, pos: Pos2, mods: egui::Modifiers, dbl: bool) {
        self.filter_popup = None;
        self.tab_origin = None;
        // Scrollbars
        if lay.vbar.contains(pos) {
            let (thumb, _) = self.vthumb(lay);
            let top = self.sheet().view.top;
            if thumb.contains(pos) {
                self.drag = Drag::VScroll(pos.y, top);
            } else {
                let page = lay.rows.len().max(2) as i64 - 1;
                let dir = if pos.y < thumb.top() { -page } else { page };
                let fr = self.sheet().freeze.0;
                let v = &mut self.sheet_mut().view;
                v.top = (v.top as i64 + dir).clamp(fr as i64, MAX_ROWS as i64 - 1) as u32;
            }
            return;
        }
        if lay.hbar.contains(pos) {
            let (thumb, _) = self.hthumb(lay);
            let left = self.sheet().view.left;
            if thumb.contains(pos) {
                self.drag = Drag::HScroll(pos.x, left);
            } else {
                let page = lay.cols.len().max(2) as i64 - 1;
                let dir = if pos.x < thumb.left() { -page } else { page };
                let fc = self.sheet().freeze.1;
                let v = &mut self.sheet_mut().view;
                v.left = (v.left as i64 + dir).clamp(fc as i64, MAX_COLS as i64 - 1) as u32;
            }
            return;
        }

        // Validation list arrow next to the active cell
        if self.dv_arrow_rect(lay).is_some_and(|r| r.contains(pos)) {
            if self.edit.is_some() && !self.commit_edit(false) {
                return;
            }
            self.dv_open_dropdown();
            return;
        }

        // Filter dropdown buttons
        if let Some(c) = self.filter_button_at(lay, pos) {
            if self.edit.is_some() && !self.commit_edit(false) {
                return;
            }
            self.open_filter_popup(c, pos);
            return;
        }

        // Pointing at cells while editing a formula
        if let Some(e) = &self.edit {
            let in_cells = lay.cells.contains(pos);
            if in_cells && e.can_point() {
                let r = lay.row_near(pos.y);
                let c = lay.col_near(pos.x);
                if !(e.sheet == self.wb.active && e.row == r && e.col == c) {
                    let prefix = if e.sheet != self.wb.active { Some(self.sheet().name.clone()) } else { None };
                    let anchor = if mods.shift { e.point.as_ref().map(|p| p.anchor).unwrap_or((r, c)) } else { (r, c) };
                    if let Some(e) = &mut self.edit {
                        e.set_point(anchor, (r, c), prefix.as_deref());
                    }
                    self.drag = Drag::Point(anchor);
                    return;
                }
            }
            if in_cells {
                let r = lay.row_near(pos.y);
                let c = lay.col_near(pos.x);
                if e.sheet == self.wb.active && e.row == r && e.col == c {
                    return; // clicking inside the cell being edited
                }
            }
            if !self.commit_edit(false) {
                return;
            }
        }

        // Fill handle
        if let Some(fh) = self.fill_handle_rect(lay) {
            if fh.expand(2.0).contains(pos) {
                self.drag = Drag::Fill(self.sel());
                return;
            }
        }
        // Drag the selection by its border
        if self.on_sel_border(lay, pos) {
            let sel = self.sel();
            let r = lay.row_near(pos.y).clamp(sel.r1, sel.r2) - sel.r1;
            let c = lay.col_near(pos.x).clamp(sel.c1, sel.c2) - sel.c1;
            self.drag = Drag::Move(r, c);
            return;
        }

        // Corner: select all
        if pos.x < lay.cells.left() && pos.y < lay.cells.top() {
            self.select_range(Range::new(0, 0, MAX_ROWS - 1, MAX_COLS - 1));
            return;
        }
        // Column header
        if pos.y < lay.cells.top() && pos.x >= lay.cells.left() {
            if let Some(c) = self.col_border_at(lay, pos.x) {
                if dbl {
                    self.autofit_cols(&[c]);
                    return;
                }
                self.push_undo(&[Scope::Sheet(self.wb.active)]);
                let start = lay.col_span(c).map(|s| s.0).unwrap_or(pos.x);
                self.drag = Drag::ResizeCol(c, start);
                return;
            }
            let c = lay.col_near(pos.x);
            if mods.shift {
                let ac = self.sheet().view.anchor.1;
                self.set_sel_cols(ac, c);
            } else {
                let v = &mut self.sheet_mut().view;
                v.anchor = (v.top, c);
                v.active = (v.top.max(0), c);
                v.anchor = v.active;
                self.set_sel_cols(c, c);
            }
            self.drag = Drag::Cols;
            return;
        }
        // Row header
        if pos.x < lay.cells.left() && pos.y >= lay.cells.top() {
            if let Some(r) = self.row_border_at(lay, pos.y) {
                if dbl {
                    self.autofit_rows(&[r]);
                    return;
                }
                self.push_undo(&[Scope::Sheet(self.wb.active)]);
                let start = lay.row_span(r).map(|s| s.0).unwrap_or(pos.y);
                self.drag = Drag::ResizeRow(r, start);
                return;
            }
            let r = lay.row_near(pos.y);
            if mods.shift {
                let ar = self.sheet().view.anchor.0;
                self.set_sel_rows(ar, r);
            } else {
                let v = &mut self.sheet_mut().view;
                v.active = (r, v.left);
                v.anchor = v.active;
                self.set_sel_rows(r, r);
            }
            self.drag = Drag::Rows;
            return;
        }
        if lay.cells.contains(pos) {
            let r = lay.row_near(pos.y);
            let c = lay.col_near(pos.x);
            if self.format_painter.is_some() {
                self.select(r, c, false);
                self.drag = Drag::Cells;
                return;
            }
            if dbl {
                self.select(r, c, false);
                self.begin_edit(None, EditMode::Edit);
                return;
            }
            self.select(r, c, mods.shift);
            self.drag = Drag::Cells;
            if !mods.shift {
                if let Some(t) = self.sheet().links.get(&(r, c)).cloned() {
                    self.pending_link = Some(((r, c), t));
                }
            }
        }
    }

    fn set_sel_cols(&mut self, a: u32, b: u32) {
        let v = &mut self.sheet_mut().view;
        v.sel = Range::new(0, a.min(b), MAX_ROWS - 1, a.max(b));
        if !v.sel.contains(v.active.0, v.active.1) {
            v.active = (v.top, a);
        }
    }

    fn set_sel_rows(&mut self, a: u32, b: u32) {
        let v = &mut self.sheet_mut().view;
        v.sel = Range::new(a.min(b), 0, a.max(b), MAX_COLS - 1);
        if !v.sel.contains(v.active.0, v.active.1) {
            v.active = (a, v.left);
        }
    }

    fn on_double_click(&mut self, lay: &Layout, pos: Pos2) {
        if self.edit.is_some() {
            return;
        }
        if pos.y < lay.cells.top() && pos.x >= lay.cells.left() {
            if let Some(c) = self.col_border_at(lay, pos.x) {
                self.drag = Drag::None;
                if let Some(u) = self.undo.pop() {
                    let _ = u;
                }
                self.autofit_cols(&[c]);
            }
        } else if pos.x < lay.cells.left() && pos.y >= lay.cells.top() {
            if let Some(r) = self.row_border_at(lay, pos.y) {
                self.drag = Drag::None;
                self.autofit_rows(&[r]);
            }
        } else if lay.cells.contains(pos) {
            if self.fill_handle_rect(lay).is_some_and(|r| r.expand(2.0).contains(pos)) {
                self.fill_down_to_data();
                return;
            }
            let r = lay.row_near(pos.y);
            let c = lay.col_near(pos.x);
            self.select(r, c, false);
            self.begin_edit(None, EditMode::Edit);
        }
    }

    fn autoscroll(&mut self, lay: &Layout, pos: Pos2) {
        let (fr, fc) = self.sheet().freeze;
        let v = &mut self.wb.sheets[self.wb.active].view;
        if pos.y > lay.cells.bottom() {
            v.top = (v.top + 1).min(MAX_ROWS - 1);
        } else if pos.y < lay.frozen_y && pos.y >= lay.cells.top() - 40.0 && v.top > fr && pos.y < lay.cells.top() + 2.0 {
            v.top -= 1;
        }
        if pos.x > lay.cells.right() {
            v.left = (v.left + 1).min(MAX_COLS - 1);
        } else if pos.x < lay.cells.left() + 2.0 && v.left > fc {
            v.left -= 1;
        }
    }

    fn on_drag(&mut self, lay: &Layout, pos: Pos2) {
        let outside = !lay.cells.contains(pos);
        match self.drag {
            Drag::Cells => {
                if outside {
                    self.autoscroll(lay, pos);
                }
                let r = lay.row_near(pos.y);
                let c = lay.col_near(pos.x);
                self.select(r, c, true);
            }
            Drag::Point(anchor) => {
                if outside {
                    self.autoscroll(lay, pos);
                }
                let r = lay.row_near(pos.y);
                let c = lay.col_near(pos.x);
                let prefix = match &self.edit {
                    Some(e) if e.sheet != self.wb.active => Some(self.sheet().name.clone()),
                    _ => None,
                };
                if let Some(e) = &mut self.edit {
                    e.set_point(anchor, (r, c), prefix.as_deref());
                }
            }
            Drag::Cols => {
                if outside {
                    self.autoscroll(lay, Pos2::new(pos.x, lay.cells.center().y));
                }
                let c = lay.col_near(pos.x);
                let a = self.sheet().view.anchor.1;
                self.set_sel_cols(a, c);
            }
            Drag::Rows => {
                if outside {
                    self.autoscroll(lay, Pos2::new(lay.cells.center().x, pos.y));
                }
                let r = lay.row_near(pos.y);
                let a = self.sheet().view.anchor.0;
                self.set_sel_rows(a, r);
            }
            Drag::ResizeCol(c, start) => {
                let w = ((pos.x - start) / self.zoom).max(0.0);
                let sel = self.sel();
                let cols: Vec<u32> = if sel.is_full_cols() && sel.c1 <= c && c <= sel.c2 { (sel.c1..=sel.c2).collect() } else { vec![c] };
                let sh = self.sheet_mut();
                for c in cols {
                    if w < 1.0 {
                        sh.hidden_cols.insert(c);
                    } else {
                        sh.hidden_cols.remove(&c);
                        sh.col_w.insert(c, w.round());
                    }
                }
                self.status = Some(format!("Width: {:.2} ({} pixels)", ((w - 5.0) / 7.0).max(0.0), w.round()));
            }
            Drag::ResizeRow(r, start) => {
                let h = ((pos.y - start) / self.zoom).max(0.0);
                let sel = self.sel();
                let rows: Vec<u32> = if sel.is_full_rows() && sel.r1 <= r && r <= sel.r2 { (sel.r1..=sel.r2).collect() } else { vec![r] };
                let sh = self.sheet_mut();
                for r in rows {
                    if h < 1.0 {
                        sh.hidden_rows.insert(r);
                    } else {
                        sh.hidden_rows.remove(&r);
                        sh.row_h.insert(r, h.round());
                    }
                }
                self.status = Some(format!("Height: {:.2} ({} pixels)", h * 0.75, h.round()));
            }
            Drag::Fill(_) | Drag::Move(..) => {
                if outside {
                    self.autoscroll(lay, pos);
                }
            }
            Drag::VScroll(y0, top0) => {
                let track = lay.vbar.height().max(1.0);
                let rows = lay.total_rows.max(1) as f32;
                let d = (pos.y - y0) / track * rows;
                let fr = self.sheet().freeze.0;
                self.sheet_mut().view.top = (top0 as f32 + d).round().clamp(fr as f32, (MAX_ROWS - 1) as f32) as u32;
            }
            Drag::HScroll(x0, left0) => {
                let track = lay.hbar.width().max(1.0);
                let cols = lay.total_cols.max(1) as f32;
                let d = (pos.x - x0) / track * cols;
                let fc = self.sheet().freeze.1;
                self.sheet_mut().view.left = (left0 as f32 + d).round().clamp(fc as f32, (MAX_COLS - 1) as f32) as u32;
            }
            Drag::None => {}
        }
    }

    fn fill_target(&self, lay: &Layout, src: Range, pos: Pos2) -> Range {
        let r = lay.row_near(pos.y);
        let c = lay.col_near(pos.x);
        let down = r as i64 - src.r2 as i64;
        let up = src.r1 as i64 - r as i64;
        let right = c as i64 - src.c2 as i64;
        let left = src.c1 as i64 - c as i64;
        let vert = down.max(up);
        let horiz = right.max(left);
        if vert <= 0 && horiz <= 0 {
            return src;
        }
        if vert >= horiz {
            if down > 0 { Range::new(src.r1, src.c1, r, src.c2) } else { Range::new(r, src.c1, src.r2, src.c2) }
        } else if right > 0 {
            Range::new(src.r1, src.c1, src.r2, c)
        } else {
            Range::new(src.r1, c, src.r2, src.c2)
        }
    }

    fn on_release(&mut self, lay: &Layout, pos: Pos2) {
        let d = self.drag;
        self.drag = Drag::None;
        match d {
            Drag::Fill(src) => {
                let target = self.fill_target(lay, src, pos);
                if target != src {
                    self.autofill(src, target);
                }
            }
            Drag::Cells => {
                if self.format_painter.is_some() {
                    self.apply_format_painter();
                }
                if let Some((cell, t)) = self.pending_link.take() {
                    if self.sel().is_single() && self.active() == cell {
                        self.follow_link(&t);
                    }
                }
            }
            Drag::Move(dr, dc) => {
                let r = lay.row_near(pos.y).saturating_sub(dr);
                let c = lay.col_near(pos.x).saturating_sub(dc);
                let copy = self.ctx.as_ref().is_some_and(|x| x.input(|i| i.modifiers.ctrl));
                if let Some(ctx) = self.ctx.clone() {
                    self.drop_selection(&ctx, r, c, copy);
                }
            }
            Drag::ResizeCol(..) | Drag::ResizeRow(..) => {
                self.dirty = true;
                self.status = None;
            }
            _ => {}
        }
    }

    fn open_filter_popup(&mut self, col: u32, pos: Pos2) {
        let Some(f) = self.sheet().filter.clone() else { return };
        let sh = self.sheet();
        let mut vals: Vec<String> = Vec::new();
        for r in f.range.r1 + 1..=f.range.r2 {
            let t = sh.get(r, col).map(|c| numfmt::format_value(&c.value, &self.wb.style(c.style).num_fmt).text).unwrap_or_default();
            if !vals.contains(&t) {
                vals.push(t);
            }
        }
        vals.sort_by_key(|a| a.to_lowercase());
        let excluded = f.excluded.get(&col).cloned().unwrap_or_default();
        let values = vals
            .into_iter()
            .map(|v| {
                let on = !excluded.contains(&v);
                (v, on)
            })
            .collect();
        self.filter_popup = Some(FilterPopup { col, pos, values, search: String::new() });
    }

    pub fn vthumb(&self, lay: &Layout) -> (Rect, f32) {
        let total = lay.total_rows.max(1) as f32;
        let vis = lay.rows.len().max(1) as f32;
        let track = lay.vbar.shrink2(Vec2::new(2.0, 2.0));
        let h = (track.height() * vis / total).clamp(16.0, track.height());
        let frac = self.sheet().view.top as f32 / total;
        let y = track.top() + (track.height() - h) * (frac / (1.0 - vis / total).max(0.01)).min(1.0);
        (Rect::from_min_size(Pos2::new(track.left(), y), Vec2::new(track.width(), h)), total)
    }

    pub fn hthumb(&self, lay: &Layout) -> (Rect, f32) {
        let total = lay.total_cols.max(1) as f32;
        let vis = lay.cols.len().max(1) as f32;
        let track = lay.hbar.shrink2(Vec2::new(2.0, 2.0));
        let w = (track.width() * vis / total).clamp(16.0, track.width());
        let frac = self.sheet().view.left as f32 / total;
        let x = track.left() + (track.width() - w) * (frac / (1.0 - vis / total).max(0.01)).min(1.0);
        (Rect::from_min_size(Pos2::new(x, track.top()), Vec2::new(w, track.height())), total)
    }

    // ------------------------------------------------------------------
    // Painting
    // ------------------------------------------------------------------

    fn paint_grid(&mut self, ui: &mut egui::Ui, lay: &Layout) {
        let painter = ui.painter_at(lay.rect);
        let cp = painter.with_clip_rect(lay.cells);
        let z = self.zoom;
        let si = self.wb.active;
        let show_grid = self.sheet().gridlines;

        // Gridlines
        if show_grid {
            let gs = Stroke::new(1.0, theme::GRID_LINE);
            for &(_, x, w) in &lay.cols {
                let xx = (x + w).round() - 0.5;
                cp.line_segment([Pos2::new(xx, lay.cells.top()), Pos2::new(xx, lay.cells.bottom())], gs);
            }
            for &(_, y, h) in &lay.rows {
                let yy = (y + h).round() - 0.5;
                cp.line_segment([Pos2::new(lay.cells.left(), yy), Pos2::new(lay.cells.right(), yy)], gs);
            }
        }

        // Collect visible cells (including merge anchors that start off-screen).
        let vis_rows: Vec<(u32, f32, f32)> = lay.rows.clone();
        let vis_cols: Vec<(u32, f32, f32)> = lay.cols.clone();
        let merges: Vec<Range> = self.sheet().merges.clone();
        let editing = self.edit.as_ref().filter(|e| e.sheet == si).map(|e| (e.row, e.col));

        struct Item {
            rect: Rect,
            clip: Rect,
            style: Style,
            shown: Shown,
            fill: Option<Color32>,
            cf: Option<crate::condfmt::CfOverride>,
        }
        let mut items: Vec<Item> = Vec::new();
        let mut merge_done: Vec<Range> = Vec::new();
        let show_formulas = self.show_formulas;
        let cf = self.cf_frame(si);

        for &(r, y, h) in &vis_rows {
            for &(c, x, w) in &vis_cols {
                let mut rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
                let mut anchor = (r, c);
                if let Some(m) = merges.iter().find(|m| m.contains(r, c)) {
                    if merge_done.contains(m) {
                        continue;
                    }
                    merge_done.push(*m);
                    rect = self.range_rect(lay, *m);
                    anchor = (m.r1, m.c1);
                    // white-out interior gridlines
                    cp.rect_filled(rect.shrink(0.5), 0.0, Color32::WHITE);
                }
                let sh = &self.wb.sheets[si];
                let ov = cf.eval(&self.wb, si, anchor.0, anchor.1);
                let Some(cell) = sh.get(anchor.0, anchor.1) else {
                    // Conditional formats can also mark empty cells (e.g. "Blanks").
                    if let Some(o) = ov {
                        let mut style = Style::default();
                        super::condfmt_ui::apply_override(&mut style, &o);
                        let shown = Shown { text: String::new(), color: None, align: HAlign::General, is_number: false };
                        items.push(Item { rect, clip: rect, fill: style.fill.map(rgb), style, shown, cf: Some(o) });
                    }
                    continue;
                };
                let mut style = self.wb.style(cell.style).clone();
                if let Some(o) = &ov {
                    super::condfmt_ui::apply_override(&mut style, o);
                }
                let fill = style.fill.map(rgb);
                if editing == Some(anchor) {
                    if let Some(f) = fill {
                        cp.rect_filled(rect, 0.0, f);
                    }
                    continue;
                }
                let mut s = shown(&cell.value, &style, cell.formula.as_ref().map(|f| f.text.as_str()), show_formulas);
                if ov.as_ref().is_some_and(|o| o.color.is_some()) {
                    s.color = None;
                }
                items.push(Item { rect, clip: rect, style, shown: s, fill, cf: ov });
            }
        }

        // Fills
        for it in &items {
            if let Some(f) = it.fill {
                cp.rect_filled(Rect::from_min_max(it.rect.min - Vec2::splat(0.5), it.rect.max + Vec2::splat(0.5)), 0.0, f);
            }
            if let Some(bar) = it.cf.as_ref().and_then(|o| o.bar) {
                super::condfmt_ui::paint_data_bar(&cp, it.rect, &bar);
            }
        }

        // Text
        let pad = 2.0 * z.max(0.5) + 1.0;
        for it in items.iter_mut() {
            if it.shown.text.is_empty() {
                continue;
            }
            let font = self.cell_font(&it.style);
            let color = it.shown.color.or(it.style.color.map(rgb)).unwrap_or(Color32::BLACK);
            let underline = if it.style.underline { Stroke::new(1.0, color) } else { Stroke::NONE };
            let strike = if it.style.strike { Stroke::new(1.0, color) } else { Stroke::NONE };
            let fmt = TextFormat { font_id: font.clone(), color, underline, strikethrough: strike, ..Default::default() };
            let indent = it.style.indent as f32 * 9.0 * z;
            let avail = (it.rect.width() - 2.0 * pad - indent).max(1.0);
            let mut job = LayoutJob::single_section(it.shown.text.clone(), fmt.clone());
            if it.style.wrap {
                job.wrap.max_width = avail;
            } else {
                job.wrap.max_width = f32::INFINITY;
            }
            let mut galley = painter.layout_job(job);
            // Numbers that don't fit show ####.
            if it.shown.is_number && !it.style.wrap && galley.size().x > avail && !show_formulas {
                let mut t = String::new();
                if it.style.num_fmt.is_empty() || it.style.num_fmt.eq_ignore_ascii_case("general") {
                    // try fewer digits
                    if let Ok(n) = it.shown.text.parse::<f64>() {
                        for d in (0..10).rev() {
                            let s = format!("{:.*}", d, n);
                            let w = painter.layout_no_wrap(s.clone(), font.clone(), color).size().x;
                            if w <= avail && (n.abs() >= 1.0 || d > 0) {
                                t = s;
                                break;
                            }
                        }
                    }
                }
                if t.is_empty() {
                    let hw = painter.layout_no_wrap("#".into(), font.clone(), color).size().x.max(1.0);
                    t = "#".repeat((avail / hw).floor().max(1.0) as usize);
                }
                galley = painter.layout_job(LayoutJob::single_section(t, fmt.clone()));
            }
            let gs = galley.size();
            // Overflow into empty neighbours for text.
            let mut clip = it.rect;
            if !it.style.wrap && !it.shown.is_number && gs.x > avail {
                let sh = &self.wb.sheets[si];
                let row = lay.row_at(it.rect.center().y);
                if let Some(r) = row {
                    let cidx = lay.col_at(it.rect.left() + 1.0);
                    if let Some(c0) = cidx {
                        let need_right = matches!(it.shown.align, HAlign::Left | HAlign::Center);
                        let need_left = matches!(it.shown.align, HAlign::Right | HAlign::Center);
                        if need_right {
                            for &(c, x, w) in vis_cols.iter().filter(|(c, _, _)| *c > c0) {
                                if sh.is_populated(r, c) || merges.iter().any(|m| m.contains(r, c)) || clip.right() - it.rect.left() > gs.x + 2.0 * pad {
                                    break;
                                }
                                let _ = (c, x);
                                clip.max.x = x + w;
                            }
                        }
                        if need_left {
                            for &(c, x, _) in vis_cols.iter().rev().filter(|(c, _, _)| *c < c0) {
                                if sh.is_populated(r, c) || merges.iter().any(|m| m.contains(r, c)) || it.rect.right() - clip.left() > gs.x + 2.0 * pad {
                                    break;
                                }
                                clip.min.x = x;
                            }
                        }
                    }
                }
                if clip != it.rect {
                    let ext = Rect::from_min_max(Pos2::new(clip.left() + 0.5, it.rect.top() + 0.5), Pos2::new(clip.right() - 1.0, it.rect.bottom() - 1.0));
                    let bg = it.fill.unwrap_or(Color32::WHITE);
                    cp.rect_filled(ext, 0.0, bg);
                }
            }
            it.clip = clip;
            if it.style.rotation != 0 {
                let tp = painter.with_clip_rect(it.rect.intersect(lay.cells));
                if it.style.rotation == 255 {
                    let stacked: String = it.shown.text.chars().map(|c| c.to_string()).collect::<Vec<_>>().join("\n");
                    let mut job = LayoutJob::single_section(stacked, fmt.clone());
                    job.halign = egui::Align::Center;
                    let g = painter.layout_job(job);
                    let y = match it.style.valign {
                        VAlign::Top => it.rect.top() + 1.0,
                        VAlign::Center => it.rect.center().y - g.size().y / 2.0,
                        VAlign::Bottom => it.rect.bottom() - g.size().y - 1.0,
                    };
                    tp.galley(Pos2::new(it.rect.center().x.round(), y.round()), g, color);
                } else {
                    let r = it.style.rotation as f32;
                    let deg = if r <= 90.0 { -r } else { r - 90.0 };
                    let a = deg.to_radians();
                    let (s, c) = a.sin_cos();
                    let rot = |v: Vec2| Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c);
                    let pts = [rot(Vec2::ZERO), rot(Vec2::new(gs.x, 0.0)), rot(Vec2::new(0.0, gs.y)), rot(gs)];
                    let minx = pts.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
                    let maxx = pts.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
                    let miny = pts.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
                    let maxy = pts.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
                    let px = match it.shown.align {
                        HAlign::Left | HAlign::General => it.rect.left() + pad - minx,
                        HAlign::Right => it.rect.right() - pad - maxx,
                        HAlign::Center => it.rect.center().x - (minx + maxx) / 2.0,
                    };
                    let py = match it.style.valign {
                        VAlign::Top => it.rect.top() + 1.0 - miny,
                        VAlign::Center => it.rect.center().y - (miny + maxy) / 2.0,
                        VAlign::Bottom => it.rect.bottom() - 1.0 - maxy,
                    };
                    tp.add(egui::epaint::TextShape::new(Pos2::new(px, py), galley, color).with_angle(a));
                }
                continue;
            }
            let x = match it.shown.align {
                HAlign::Left | HAlign::General => it.rect.left() + pad + indent,
                HAlign::Right => it.rect.right() - pad - gs.x - indent,
                HAlign::Center => it.rect.center().x - gs.x / 2.0,
            };
            let y = match it.style.valign {
                VAlign::Top => it.rect.top() + 1.0,
                VAlign::Center => it.rect.center().y - gs.y / 2.0,
                VAlign::Bottom => it.rect.bottom() - gs.y - 1.0,
            };
            let tp = painter.with_clip_rect(clip.intersect(lay.cells));
            tp.galley(Pos2::new(x.round(), y.round()), galley, color);
        }

        // Borders
        let sh = &self.wb.sheets[si];
        for &(r, y, h) in &vis_rows {
            for &(c, x, w) in &vis_cols {
                let Some(cell) = sh.get(r, c) else { continue };
                if cell.style == 0 {
                    continue;
                }
                let b = self.wb.style(cell.style).border;
                if b == Default::default() {
                    continue;
                }
                let mut rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
                if let Some(m) = merges.iter().find(|m| m.r1 == r && m.c1 == c) {
                    rect = self.range_rect(lay, *m);
                }
                draw_border(&cp, rect, b.top, b.bottom, b.left, b.right);
            }
        }
        for it in &items {
            if let Some(c) = it.cf.as_ref().and_then(|o| o.border) {
                cp.rect_stroke(it.rect.translate(Vec2::splat(-0.5)), 0.0, Stroke::new(1.0, rgb(c)), StrokeKind::Middle);
            }
        }

        // Note markers (red triangle in the top-right corner)
        let notes: Vec<(u32, u32)> = self.wb.sheets[si].notes.keys().copied().collect();
        for (r, c) in &notes {
            if let (Some((t, _)), Some((_, rx))) = (lay.row_span(*r), lay.col_span(*c)) {
                let s = 6.0 * z.max(0.7);
                cp.add(egui::Shape::convex_polygon(
                    vec![Pos2::new(rx - s - 1.0, t), Pos2::new(rx - 1.0, t), Pos2::new(rx - 1.0, t + s)],
                    Color32::from_rgb(0xD0, 0x10, 0x10),
                    Stroke::NONE,
                ));
            }
        }

        // Frozen pane separators
        let (fr, fc) = self.sheet().freeze;
        if fr > 0 {
            painter.line_segment([Pos2::new(lay.rect.left(), lay.frozen_y), Pos2::new(lay.cells.right(), lay.frozen_y)], Stroke::new(1.0, Color32::from_gray(140)));
        }
        if fc > 0 {
            painter.line_segment([Pos2::new(lay.frozen_x, lay.rect.top()), Pos2::new(lay.frozen_x, lay.cells.bottom())], Stroke::new(1.0, Color32::from_gray(140)));
        }

        // Clipboard marquee
        if let Some(clip) = &self.clip {
            if clip.sheet == si {
                let r = self.range_rect(lay, clip.range).shrink(0.5);
                let t = ui.input(|i| i.time) as f32;
                dashed_rect(&cp, r, theme::ACCENT, t);
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
            }
        }

        // Selection
        let sel = self.sel();
        let (ar, ac) = self.active();
        let sel_rect = self.range_rect(lay, sel);
        if !sel.is_single() {
            cp.rect_filled(sel_rect, 0.0, theme::sel_fill());
            let act = self.range_rect(lay, self.sheet().merge_at(ar, ac).unwrap_or(Range::cell(ar, ac)));
            let act_cf = cf.eval(&self.wb, si, ar, ac);
            let fill = act_cf
                .as_ref()
                .and_then(|o| o.fill)
                .or_else(|| self.sheet().get(ar, ac).and_then(|c| self.wb.style(c.style).fill))
                .map(rgb)
                .unwrap_or(Color32::WHITE);
            if editing.is_none() {
                cp.rect_filled(act.shrink(0.5), 0.0, fill.gamma_multiply(1.0));
                if let Some(bar) = act_cf.as_ref().and_then(|o| o.bar) {
                    super::condfmt_ui::paint_data_bar(&cp, act, &bar);
                }
                // redraw active cell text above the white-out
                if let Some(cell) = self.wb.sheets[si].get(ar, ac) {
                    let mut style = self.wb.style(cell.style).clone();
                    if let Some(o) = &act_cf {
                        super::condfmt_ui::apply_override(&mut style, o);
                    }
                    let mut s = shown(&cell.value, &style, cell.formula.as_ref().map(|f| f.text.as_str()), show_formulas);
                    if act_cf.as_ref().is_some_and(|o| o.color.is_some()) {
                        s.color = None;
                    }
                    if !s.text.is_empty() {
                        let font = self.cell_font(&style);
                        let color = s.color.or(style.color.map(rgb)).unwrap_or(Color32::BLACK);
                        let g = painter.layout_no_wrap(s.text.clone(), font, color);
                        let x = match s.align {
                            HAlign::Left | HAlign::General => act.left() + pad,
                            HAlign::Right => act.right() - pad - g.size().x,
                            HAlign::Center => act.center().x - g.size().x / 2.0,
                        };
                        let y = match style.valign {
                            VAlign::Top => act.top() + 1.0,
                            VAlign::Center => act.center().y - g.size().y / 2.0,
                            VAlign::Bottom => act.bottom() - g.size().y - 1.0,
                        };
                        painter.with_clip_rect(act.intersect(lay.cells)).galley(Pos2::new(x.round(), y.round()), g, color);
                    }
                }
            }
        }
        let sel_stroke = Stroke::new(2.0, theme::ACCENT);
        cp.rect_stroke(sel_rect.expand(0.0), 0.0, sel_stroke, StrokeKind::Middle);
        if let Some(fh) = self.fill_handle_rect(lay) {
            cp.rect_filled(fh.expand(1.0), 0.0, Color32::WHITE);
            cp.rect_filled(fh, 0.0, theme::ACCENT);
        }
        // Move preview
        if let Drag::Move(dr, dc) = self.drag {
            if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
                let r = lay.row_near(p.y).saturating_sub(dr);
                let c = lay.col_near(p.x).saturating_sub(dc);
                let t = Range::new(r, c, r + sel.rows() - 1, c + sel.cols() - 1);
                let rr = self.range_rect(lay, t);
                cp.rect_stroke(rr, 0.0, Stroke::new(2.0, Color32::from_gray(110)), StrokeKind::Middle);
                ui.ctx().set_cursor_icon(CursorIcon::Move);
            }
        }
        // Fill preview
        if let Drag::Fill(src) = self.drag {
            if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
                let t = self.fill_target(lay, src, p);
                let r = self.range_rect(lay, t);
                dashed_rect(&cp, r, Color32::from_gray(90), 0.0);
            }
        }

        // Formula reference highlights while editing
        if let Some(e) = &self.edit {
            if e.is_formula() {
                for (i, (_, _, sheet, rg)) in e.references().iter().enumerate() {
                    let on_this = match sheet {
                        Some(n) => self.wb.sheet_index(n) == Some(si),
                        None => e.sheet == si,
                    };
                    if !on_this {
                        continue;
                    }
                    let col = theme::REF_COLORS[i % theme::REF_COLORS.len()];
                    let r = self.range_rect(lay, *rg);
                    cp.rect_filled(r, 0.0, col.gamma_multiply(0.08));
                    cp.rect_stroke(r.shrink(0.5), 0.0, Stroke::new(1.5, col), StrokeKind::Middle);
                    for p in [r.left_top(), r.right_top(), r.left_bottom(), r.right_bottom()] {
                        cp.rect_filled(Rect::from_center_size(p, Vec2::splat(4.0)), 0.0, col);
                    }
                }
            }
        }

        // Filter buttons
        if let Some(f) = self.sheet().filter.clone() {
            if let Some((_, b)) = lay.row_span(f.range.r1) {
                for c in f.range.c1..=f.range.c2 {
                    if let Some((_, rx)) = lay.col_span(c) {
                        let btn = Rect::from_min_max(Pos2::new(rx - 17.0, b - 17.0), Pos2::new(rx - 1.0, b - 1.0));
                        let active = f.excluded.get(&c).is_some_and(|s| !s.is_empty());
                        cp.rect_filled(btn, 2.0, Color32::from_gray(250));
                        cp.rect_stroke(btn, 2.0, Stroke::new(1.0, Color32::from_gray(170)), StrokeKind::Inside);
                        let (g, is_icon) = self.fonts.icons.get(if active { theme::ic::FILTER } else { theme::ic::DOWN }, "▾");
                        let font = if is_icon { theme::icon_font(9.0) } else { theme::ui_font(11.0) };
                        cp.text(btn.center(), Align2::CENTER_CENTER, g, font, theme::TEXT);
                    }
                }
            }
        }

        self.paint_charts(ui, lay);
        // Validation list arrow
        self.dv_paint_arrow(&cp, lay, ui.input(|i| i.pointer.hover_pos()));

        // In-cell editor
        self.paint_editor(ui, lay);

        // Note popups (hovered cell, or all when "Show All Notes" is on)
        if self.edit.is_none() && self.drag == Drag::None {
            let shown_notes: Vec<(u32, u32)> =
                if self.show_all_notes { notes.clone() } else { self.hover_cell.filter(|h| notes.contains(h)).into_iter().collect() };
            let lp = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("cell_notes")));
            for (r, c) in shown_notes {
                let (Some((t, _)), Some((_, rx))) = (lay.row_span(r), lay.col_span(c)) else { continue };
                let text = self.wb.sheets[si].notes.get(&(r, c)).cloned().unwrap_or_default();
                let mut job = LayoutJob::single_section(text, TextFormat { font_id: theme::ui_font(12.0), color: Color32::BLACK, ..Default::default() });
                job.wrap.max_width = 200.0;
                let g = lp.layout_job(job);
                let origin = Pos2::new(rx + 14.0, t - 4.0);
                let bx = Rect::from_min_size(origin, g.size() + Vec2::new(12.0, 10.0));
                lp.line_segment([Pos2::new(rx - 1.0, t + 1.0), bx.left_top() + Vec2::new(0.0, 6.0)], Stroke::new(1.0, Color32::from_gray(90)));
                lp.rect_filled(bx, 0.0, Color32::from_rgb(0xFF, 0xFF, 0xE1));
                lp.rect_stroke(bx, 0.0, Stroke::new(1.0, Color32::from_gray(120)), StrokeKind::Inside);
                lp.galley(origin + Vec2::new(6.0, 5.0), g, Color32::BLACK);
            }
        }

        // Headers
        if self.show_headings {
            self.paint_headers(&painter, lay);
        }

        // Scrollbars
        painter.rect_filled(lay.vbar, 0.0, theme::CHROME);
        painter.rect_filled(lay.hbar, 0.0, theme::CHROME);
        painter.rect_filled(Rect::from_min_max(Pos2::new(lay.vbar.left(), lay.hbar.top()), lay.rect.right_bottom()), 0.0, theme::CHROME);
        let (vt, _) = self.vthumb(lay);
        let (ht, _) = self.hthumb(lay);
        let thumb_col = if matches!(self.drag, Drag::VScroll(..)) { Color32::from_gray(120) } else { Color32::from_gray(180) };
        painter.rect_filled(vt.shrink2(Vec2::new(2.0, 0.0)), 4.0, thumb_col);
        let thumb_col = if matches!(self.drag, Drag::HScroll(..)) { Color32::from_gray(120) } else { Color32::from_gray(180) };
        painter.rect_filled(ht.shrink2(Vec2::new(0.0, 2.0)), 4.0, thumb_col);
        let _ = (fr, fc, z);
    }

    fn paint_headers(&mut self, painter: &egui::Painter, lay: &Layout) {
        let sel = self.sel();
        let font = theme::ui_font((11.0_f32 * self.zoom).clamp(7.0, 22.0));
        let line = Stroke::new(1.0, theme::HEADER_LINE);
        let col_hdr = Rect::from_min_max(Pos2::new(lay.cells.left(), lay.rect.top()), Pos2::new(lay.cells.right(), lay.cells.top()));
        let row_hdr = Rect::from_min_max(Pos2::new(lay.rect.left(), lay.cells.top()), Pos2::new(lay.cells.left(), lay.cells.bottom()));
        let cp = painter.with_clip_rect(col_hdr);
        let rp = painter.with_clip_rect(row_hdr);
        cp.rect_filled(col_hdr, 0.0, theme::HEADER_BG);
        rp.rect_filled(row_hdr, 0.0, theme::HEADER_BG);
        let full_cols = sel.is_full_cols();
        let full_rows = sel.is_full_rows();
        for &(c, x, w) in &lay.cols {
            let r = Rect::from_min_size(Pos2::new(x, col_hdr.top()), Vec2::new(w, col_hdr.height()));
            let in_sel = c >= sel.c1 && c <= sel.c2;
            if in_sel {
                cp.rect_filled(r, 0.0, if full_cols { theme::HEADER_FULL_BG } else { theme::HEADER_SEL_BG });
                cp.line_segment([Pos2::new(r.left(), r.bottom() - 1.0), Pos2::new(r.right(), r.bottom() - 1.0)], Stroke::new(2.0, theme::ACCENT));
            }
            let col = if in_sel { theme::ACCENT_DARK } else { theme::HEADER_TEXT };
            cp.text(r.center(), Align2::CENTER_CENTER, col_name(c), font.clone(), col);
            let xx = (x + w).round() - 0.5;
            cp.line_segment([Pos2::new(xx, r.top() + 2.0), Pos2::new(xx, r.bottom())], line);
        }
        for &(rw, y, h) in &lay.rows {
            let r = Rect::from_min_size(Pos2::new(row_hdr.left(), y), Vec2::new(row_hdr.width(), h));
            let in_sel = rw >= sel.r1 && rw <= sel.r2;
            if in_sel {
                rp.rect_filled(r, 0.0, if full_rows { theme::HEADER_FULL_BG } else { theme::HEADER_SEL_BG });
                rp.line_segment([Pos2::new(r.right() - 1.0, r.top()), Pos2::new(r.right() - 1.0, r.bottom())], Stroke::new(2.0, theme::ACCENT));
            }
            let col = if in_sel { theme::ACCENT_DARK } else { theme::HEADER_TEXT };
            if h > 6.0 {
                rp.text(r.center(), Align2::CENTER_CENTER, (rw + 1).to_string(), font.clone(), col);
            }
            let yy = (y + h).round() - 0.5;
            rp.line_segment([Pos2::new(r.left() + 2.0, yy), Pos2::new(r.right(), yy)], line);
        }
        // header borders
        painter.line_segment([Pos2::new(lay.rect.left(), lay.cells.top() - 0.5), Pos2::new(lay.cells.right(), lay.cells.top() - 0.5)], line);
        painter.line_segment([Pos2::new(lay.cells.left() - 0.5, lay.rect.top()), Pos2::new(lay.cells.left() - 0.5, lay.cells.bottom())], line);
        // corner with the select-all triangle
        let corner = Rect::from_min_max(lay.rect.min, Pos2::new(lay.cells.left(), lay.cells.top()));
        painter.rect_filled(corner, 0.0, theme::HEADER_BG);
        let p = corner.right_bottom() - Vec2::new(3.0, 3.0);
        let s = (corner.height() * 0.55).min(corner.width() * 0.55);
        painter.add(egui::Shape::convex_polygon(
            vec![p, p - Vec2::new(0.0, s), p - Vec2::new(s, 0.0)],
            Color32::from_gray(185),
            Stroke::NONE,
        ));
        painter.line_segment([Pos2::new(lay.rect.left(), lay.cells.top() - 0.5), Pos2::new(lay.cells.left(), lay.cells.top() - 0.5)], line);
    }

    fn paint_editor(&mut self, ui: &mut egui::Ui, lay: &Layout) {
        let Some(e) = self.edit.clone() else {
            return;
        };
        if e.sheet != self.wb.active {
            return;
        }
        let si = self.wb.active;
        let anchor_rg = self.sheet().merge_at(e.row, e.col).unwrap_or(Range::cell(e.row, e.col));
        let mut rect = self.range_rect(lay, anchor_rg);
        if !lay.cells.intersects(rect) {
            return;
        }
        let style = self.wb.sheets[si].get(e.row, e.col).map(|c| self.wb.style(c.style).clone()).unwrap_or_default();
        let font = self.cell_font(&style);
        let job = self.editor_job(&e, font.clone(), style.color.map(rgb).unwrap_or(Color32::BLACK), None);
        let painter = ui.painter_at(lay.cells);
        let galley = painter.layout_job(job);
        let pad = 2.0 * self.zoom.max(0.5) + 1.0;
        let need_w = galley.size().x + 2.0 * pad + 6.0;
        if need_w > rect.width() {
            rect.max.x = (rect.left() + need_w).min(lay.cells.right());
        }
        let need_h = galley.size().y + 2.0;
        if need_h > rect.height() {
            rect.max.y = rect.top() + need_h;
        }
        let fill = style.fill.map(rgb).unwrap_or(Color32::WHITE);
        painter.rect_filled(rect, 0.0, fill);
        painter.rect_stroke(rect, 0.0, Stroke::new(2.0, theme::ACCENT), StrokeKind::Middle);
        let text_pos = Pos2::new(rect.left() + pad, rect.bottom() - galley.size().y - 1.0);
        self.paint_caret_and_selection(&painter, &e, &galley, text_pos, ui.input(|i| i.time), !e.in_bar);
        painter.galley(text_pos, galley, Color32::BLACK);
        self.edit_rect = Some(rect);
    }

    /// Text layout for the editor, with formula references coloured.
    pub fn editor_job(&self, e: &super::Editor, font: FontId, base: Color32, wrap: Option<f32>) -> LayoutJob {
        let mut job = LayoutJob::default();
        let refs = e.references();
        let mut pos = 0;
        let fmt = |c: Color32| TextFormat { font_id: font.clone(), color: c, ..Default::default() };
        for (i, (s, en, _, _)) in refs.iter().enumerate() {
            if *s < pos || *en > e.text.len() {
                continue;
            }
            job.append(&e.text[pos..*s], 0.0, fmt(base));
            job.append(&e.text[*s..*en], 0.0, fmt(theme::REF_COLORS[i % theme::REF_COLORS.len()]));
            pos = *en;
        }
        job.append(&e.text[pos..], 0.0, fmt(base));
        if let Some(w) = wrap {
            job.wrap.max_width = w;
        }
        if e.text.is_empty() {
            job.append("", 0.0, fmt(base));
        }
        job
    }

    pub fn paint_caret_and_selection(&self, painter: &egui::Painter, e: &super::Editor, galley: &std::sync::Arc<egui::Galley>, origin: Pos2, time: f64, active: bool) {
        use egui::text::CCursor;
        let ci = |b: usize| e.text[..b.min(e.text.len())].chars().count();
        if e.has_selection() {
            let (a, b) = e.selection();
            let ra = galley.pos_from_cursor(CCursor::new(ci(a)));
            let rb = galley.pos_from_cursor(CCursor::new(ci(b)));
            if (ra.top() - rb.top()).abs() < 1.0 {
                painter.rect_filled(Rect::from_min_max(origin + ra.min.to_vec2(), origin + Vec2::new(rb.min.x, rb.max.y)), 0.0, Color32::from_rgb(0xB5, 0xD3, 0xF5));
            } else {
                painter.rect_filled(
                    Rect::from_min_max(origin + ra.min.to_vec2(), origin + Vec2::new(galley.size().x, ra.max.y)),
                    0.0,
                    Color32::from_rgb(0xB5, 0xD3, 0xF5),
                );
            }
        }
        if active && ((time * 1.9) as i64) % 2 == 0 {
            let r = galley.pos_from_cursor(CCursor::new(ci(e.cursor)));
            let x = (origin.x + r.min.x).round() + 0.5;
            painter.line_segment([Pos2::new(x, origin.y + r.min.y), Pos2::new(x, origin.y + r.max.y)], Stroke::new(1.0, Color32::BLACK));
        }
        painter.ctx().request_repaint_after(std::time::Duration::from_millis(520));
    }

    fn context_menu(&mut self, resp: &egui::Response) {
        resp.context_menu(|ui| {
            ui.set_min_width(190.0);
            let sel = self.sel();
            if ui.button("Cut").clicked() {
                self.copy(ui.ctx(), true);
                ui.close();
            }
            if ui.button("Copy").clicked() {
                self.copy(ui.ctx(), false);
                ui.close();
            }
            if ui.add_enabled(self.clip.is_some(), egui::Button::new("Paste")).clicked() {
                self.paste_internal(super::actions::PasteMode::All);
                ui.close();
            }
            ui.menu_button("Paste Special", |ui| {
                for (label, m) in [
                    ("Values", super::actions::PasteMode::Values),
                    ("Formulas", super::actions::PasteMode::Formulas),
                    ("Formatting", super::actions::PasteMode::Formats),
                    ("Transpose", super::actions::PasteMode::Transpose),
                ] {
                    if ui.add_enabled(self.clip.is_some(), egui::Button::new(label)).clicked() {
                        self.paste_internal(m);
                        ui.close();
                    }
                }
            });
            ui.separator();
            if ui.button("Insert...").clicked() {
                self.insert_cells_prompt();
                ui.close();
            }
            if ui.button("Delete...").clicked() {
                self.delete_cells_prompt();
                ui.close();
            }
            if ui.button("Clear Contents").clicked() {
                self.clear(true, false);
                ui.close();
            }
            ui.separator();
            let (ar, ac) = self.active();
            let has_link = self.sheet().links.contains_key(&(ar, ac));
            if ui.button(if has_link { "Edit Hyperlink..." } else { "Link..." }).clicked() {
                self.open_link_dialog();
                ui.close();
            }
            if has_link {
                if ui.button("Open Hyperlink").clicked() {
                    if let Some(t) = self.sheet().links.get(&(ar, ac)).cloned() {
                        self.follow_link(&t);
                    }
                    ui.close();
                }
                if ui.button("Remove Hyperlink").clicked() {
                    self.set_link(ar, ac, "", None);
                    ui.close();
                }
            }
            let has_note = self.sheet().notes.contains_key(&(ar, ac));
            if ui.button(if has_note { "Edit Note" } else { "New Note" }).clicked() {
                self.edit_note();
                ui.close();
            }
            if has_note && ui.button("Delete Note").clicked() {
                self.delete_notes_in_selection();
                ui.close();
            }
            ui.separator();
            ui.menu_button("Filter", |ui| {
                if ui.button("Filter by Selected Cell's Value").clicked() {
                    self.filter_by_active_value();
                    ui.close();
                }
                if ui.button("Clear Filter").clicked() {
                    self.clear_filter();
                    ui.close();
                }
            });
            ui.menu_button("Sort", |ui| {
                if ui.button("Sort A to Z").clicked() {
                    self.sort_simple(false);
                    ui.close();
                }
                if ui.button("Sort Z to A").clicked() {
                    self.sort_simple(true);
                    ui.close();
                }
                if ui.button("Custom Sort...").clicked() {
                    self.open_sort_dialog();
                    ui.close();
                }
            });
            ui.separator();
            if ui.button("Format Cells...").clicked() {
                self.open_format_cells(0);
                ui.close();
            }
            if sel.is_full_cols() {
                if ui.button("Column Width...").clicked() {
                    self.open_col_width();
                    ui.close();
                }
                if ui.button("Hide").clicked() {
                    self.hide_cols(true);
                    ui.close();
                }
                if ui.button("Unhide").clicked() {
                    self.hide_cols(false);
                    ui.close();
                }
            } else if sel.is_full_rows() {
                if ui.button("Row Height...").clicked() {
                    self.open_row_height();
                    ui.close();
                }
                if ui.button("Hide").clicked() {
                    self.hide_rows(true);
                    ui.close();
                }
                if ui.button("Unhide").clicked() {
                    self.hide_rows(false);
                    ui.close();
                }
            }
        });
    }
}

fn line_for(b: BorderLine) -> Option<(f32, bool)> {
    match b {
        BorderLine::None => None,
        BorderLine::Thin => Some((1.0, false)),
        BorderLine::Dashed | BorderLine::Dotted => Some((1.0, true)),
        BorderLine::Medium => Some((2.0, false)),
        BorderLine::Thick => Some((3.0, false)),
        BorderLine::Double => Some((3.0, false)),
    }
}

fn draw_border(p: &egui::Painter, r: Rect, top: BorderLine, bottom: BorderLine, left: BorderLine, right: BorderLine) {
    let col = Color32::BLACK;
    let seg = |a: Pos2, b: Pos2, l: BorderLine| {
        if let Some((w, dashed)) = line_for(l) {
            if dashed {
                p.extend(egui::Shape::dashed_line(&[a, b], Stroke::new(w, col), 3.0, 2.0));
            } else if l == BorderLine::Double {
                let n = if a.y == b.y { Vec2::new(0.0, 1.0) } else { Vec2::new(1.0, 0.0) };
                p.line_segment([a - n, b - n], Stroke::new(1.0, col));
                p.line_segment([a + n, b + n], Stroke::new(1.0, col));
            } else {
                p.line_segment([a, b], Stroke::new(w, col));
            }
        }
    };
    let (l, rr, t, b) = (r.left() - 0.5, r.right() - 0.5, r.top() - 0.5, r.bottom() - 0.5);
    seg(Pos2::new(l, t), Pos2::new(rr, t), top);
    seg(Pos2::new(l, b), Pos2::new(rr, b), bottom);
    seg(Pos2::new(l, t), Pos2::new(l, b), left);
    seg(Pos2::new(rr, t), Pos2::new(rr, b), right);
}

fn dashed_rect(p: &egui::Painter, r: Rect, col: Color32, t: f32) {
    let offset = (t * 12.0) % 8.0;
    let pts = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    let shapes = egui::Shape::dashed_line_with_offset(&pts, Stroke::new(2.0, col), &[4.0], &[4.0], offset);
    p.extend(shapes);
}
