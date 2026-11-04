//! Formula bar, sheet tabs, status bar, backstage (File) view and popups.

use eframe::egui::{self, Align2, Color32, CursorIcon, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};

use super::dialogs::Dialog;
use super::theme::{self, ic};
use super::{App, Drag, EditMode, Editor, Pending};
use crate::model::{cell_name, Value};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Backstage {
    Home,
    Open,
    SaveAs,
    Print,
    About,
}

impl App {
    pub fn icon(&self, code: char, fallback: &'static str) -> (String, bool) {
        self.fonts.icons.get(code, fallback)
    }

    fn mode_label(&self) -> &'static str {
        match &self.edit {
            None => "Ready",
            Some(e) if e.point.as_ref().is_some_and(|p| p.end == e.cursor) => "Point",
            Some(e) if e.mode == EditMode::Edit => "Edit",
            Some(_) => "Enter",
        }
    }

    // ------------------------------------------------------------------
    // Formula bar
    // ------------------------------------------------------------------

    pub fn formula_bar_ui(&mut self, ui: &mut egui::Ui) {
        let h = 24.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            // Name box
            let sel = self.sel();
            if !self.name_box_editing {
                self.name_box = if matches!(self.drag, Drag::Cells) && !sel.is_single() {
                    format!("{}R x {}C", sel.rows(), sel.cols())
                } else {
                    let (r, c) = self.active();
                    self.wb
                        .names
                        .iter()
                        .find(|(_, f)| crate::model::Range::parse(f.rsplit('!').next().unwrap_or("")) == Some(sel) && sel != crate::model::Range::cell(r, c))
                        .map(|(n, _)| n.clone())
                        .unwrap_or_else(|| cell_name(r, c))
                };
            }
            let nb = egui::TextEdit::singleline(&mut self.name_box)
                .desired_width(96.0)
                .min_size(Vec2::new(96.0, h))
                .vertical_align(egui::Align::Center)
                .font(theme::ui_font(13.0))
                .margin(egui::Margin::symmetric(5, 2));
            let resp = egui::Frame::new().fill(Color32::WHITE).stroke(Stroke::new(1.0, theme::BORDER)).show(ui, |ui| ui.add(nb)).inner;
            if resp.gained_focus() {
                self.name_box_editing = true;
                if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), resp.id) {
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(self.name_box.chars().count()),
                    )));
                    st.store(ui.ctx(), resp.id);
                }
            }
            if resp.lost_focus() {
                self.name_box_editing = false;
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let t = self.name_box.clone();
                    if !self.go_to(&t) {
                        // define a new name for the selection
                        let r = self.sel_abs_ref();
                        if let Err(e) = self.new_name(&t, &r) {
                            self.dialog = Some(Dialog::Message { title: "Name Box".into(), text: format!("Reference isn't valid.\n{e}") });
                        }
                    }
                }
                egui::Id::new("grid");
                ui.memory_mut(|m| m.request_focus(egui::Id::new("grid")));
            }
            ui.add_space(6.0);
            // dots separator
            let (r, _) = ui.allocate_exact_size(Vec2::new(8.0, h), Sense::hover());
            for k in 0..3 {
                ui.painter().circle_filled(Pos2::new(r.center().x, r.top() + 7.0 + k as f32 * 5.0), 1.0, theme::TEXT_DIM);
            }
            ui.add_space(4.0);
            let editing = self.edit.is_some();
            let (x, xi) = self.icon(ic::CLOSE, "✕");
            let (ok, oki) = self.icon(ic::ACCEPT, "✓");
            let f = |s: String, icon: bool| if icon { RichText::new(s).font(theme::icon_font(12.0)) } else { RichText::new(s) };
            if ui.add_enabled(editing, egui::Button::new(f(x, xi)).min_size(Vec2::new(26.0, h)).frame(false)).on_hover_text("Cancel").clicked() {
                self.cancel_edit();
            }
            if ui.add_enabled(editing, egui::Button::new(f(ok, oki)).min_size(Vec2::new(26.0, h)).frame(false)).on_hover_text("Enter").clicked() {
                self.commit_edit(false);
            }
            if ui
                .add(egui::Button::new(RichText::new("fx").italics().font(theme::ui_font(14.0))).min_size(Vec2::new(28.0, h)).frame(false))
                .on_hover_text("Insert Function (Shift+F3)")
                .clicked()
            {
                self.dialog = Some(Dialog::insert_function());
            }
            ui.add_space(4.0);
            self.formula_field(ui, h);
        });
    }

    fn formula_field(&mut self, ui: &mut egui::Ui, h: f32) {
        let w = ui.available_width();
        let lines = self.edit.as_ref().filter(|e| e.in_bar).map(|e| e.text.lines().count().max(1)).unwrap_or(1).min(6) as f32;
        let hh = if lines > 1.0 { lines * 17.0 + 6.0 } else { h };
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, hh), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::WHITE);
        painter.rect_stroke(rect, 0.0, Stroke::new(1.0, theme::BORDER), StrokeKind::Inside);
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Text);
        }
        let font = theme::ui_font(13.5);
        let origin = Pos2::new(rect.left() + 6.0, rect.top() + 4.0);
        let galley = match &self.edit {
            Some(e) => {
                let job = self.editor_job(e, font.clone(), Color32::BLACK, None);
                painter.layout_job(job)
            }
            None => {
                let (r, c) = self.active();
                // A spilled cell shows its anchor's formula in grey.
                let sh = self.sheet();
                let own = sh.get(r, c).filter(|x| !x.value.is_empty() || x.formula.is_some());
                let (t, color) = match own.is_none().then(|| sh.spill_anchor_of(r, c)).flatten().and_then(|(ar, ac)| sh.get(ar, ac)) {
                    Some(anchor) => (anchor.input_text(), Color32::from_gray(150)),
                    None => (sh.get(r, c).map(|c| c.input_text()).unwrap_or_default(), Color32::BLACK),
                };
                let first = t.lines().next().unwrap_or("").to_string();
                let more = t.lines().count() > 1;
                painter.layout_no_wrap(if more { format!("{first} …") } else { first }, font.clone(), color)
            }
        };
        if let Some(e) = &self.edit {
            if e.in_bar {
                self.paint_caret_and_selection(&painter, e, &galley, origin, ui.input(|i| i.time), true);
            }
        }
        painter.galley(origin, galley.clone(), Color32::BLACK);

        let char_at = |p: Pos2, e: &Editor| -> usize {
            let cc = galley.cursor_from_pos(p - origin);
            e.text.char_indices().nth(cc.index).map(|(b, _)| b).unwrap_or(e.text.len())
        };
        if resp.clicked() || resp.drag_started() {
            if self.edit.is_none() {
                self.begin_edit(None, EditMode::Edit);
            }
            let shift = ui.input(|i| i.modifiers.shift);
            if let (Some(p), Some(e)) = (resp.interact_pointer_pos(), &mut self.edit) {
                let galley_now = &galley;
                let _ = galley_now;
                let idx = {
                    let e2: &Editor = e;
                    char_at(p, e2)
                };
                e.in_bar = true;
                e.mode = EditMode::Edit;
                e.move_to(idx, shift);
            }
            ui.memory_mut(|m| m.request_focus(egui::Id::new("grid")));
        } else if resp.dragged() {
            if let (Some(p), Some(e)) = (resp.interact_pointer_pos(), &mut self.edit) {
                let idx = {
                    let e2: &Editor = e;
                    char_at(p, e2)
                };
                e.move_to(idx, true);
            }
        }
        if resp.double_clicked() {
            if let Some(e) = &mut self.edit {
                // select word
                let c = e.cursor;
                let t = &e.text;
                let start = t[..c].rfind(|ch: char| !ch.is_alphanumeric()).map(|i| i + 1).unwrap_or(0);
                let end = t[c..].find(|ch: char| !ch.is_alphanumeric()).map(|i| c + i).unwrap_or(t.len());
                e.anchor = start;
                e.cursor = end;
            }
        }
        if let Some(e) = &self.edit {
            if e.in_bar {
                self.edit_bar_rect = Some(rect);
            }
        }
    }

    // ------------------------------------------------------------------
    // Sheet tabs
    // ------------------------------------------------------------------

    pub fn sheet_tabs_ui(&mut self, ui: &mut egui::Ui) {
        let full = ui.max_rect();
        ui.painter().line_segment([full.left_top(), full.right_top()], Stroke::new(1.0, theme::BORDER));
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(6.0);
            let (l, li) = self.icon(ic::LEFT, "◀");
            let (r, ri) = self.icon(ic::RIGHT, "▶");
            let f = |s: String, icon: bool| if icon { RichText::new(s).font(theme::icon_font(10.0)) } else { RichText::new(s).size(10.0) };
            if ui.add(egui::Button::new(f(l, li)).frame(false).min_size(Vec2::new(22.0, 22.0))).clicked() {
                self.tab_scroll = self.tab_scroll.saturating_sub(1);
            }
            if ui.add(egui::Button::new(f(r, ri)).frame(false).min_size(Vec2::new(22.0, 22.0))).clicked() && self.tab_scroll + 1 < self.wb.sheets.len() {
                self.tab_scroll += 1;
            }
            ui.add_space(6.0);
            self.tab_scroll = self.tab_scroll.min(self.wb.sheets.len().saturating_sub(1));
            if self.wb.active < self.tab_scroll {
                self.tab_scroll = self.wb.active;
            }
            let font = theme::ui_font(12.5);
            let bold = theme::ui_bold(12.5);
            let mut action: Option<(usize, u8)> = None;
            for i in self.tab_scroll..self.wb.sheets.len() {
                if ui.available_width() < 60.0 {
                    if self.wb.active >= i {
                        self.tab_scroll += 1;
                    }
                    break;
                }
                if self.wb.sheets[i].hidden {
                    continue;
                }
                let active = i == self.wb.active;
                if let Some((ri, buf)) = &mut self.rename_tab {
                    if *ri == i {
                        let te = ui.add(egui::TextEdit::singleline(buf).desired_width(110.0).font(font.clone()));
                        te.request_focus();
                        if te.lost_focus() {
                            let (ri, buf) = self.rename_tab.take().unwrap();
                            if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                if let Err(msg) = self.rename_sheet(ri, &buf) {
                                    self.dialog = Some(Dialog::Message { title: "Rename Sheet".into(), text: msg });
                                }
                            }
                            ui.memory_mut(|m| m.request_focus(egui::Id::new("grid")));
                        }
                        continue;
                    }
                }
                let name = self.wb.sheets[i].name.clone();
                let galley = ui.painter().layout_no_wrap(name.clone(), if active { bold.clone() } else { font.clone() }, theme::TEXT);
                let w = galley.size().x + 26.0;
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 26.0), Sense::click_and_drag());
                let p = ui.painter();
                if active {
                    p.rect_filled(rect.shrink2(Vec2::new(0.0, 1.0)), 0.0, Color32::WHITE);
                    p.line_segment(
                        [Pos2::new(rect.left() + 6.0, rect.bottom() - 2.5), Pos2::new(rect.right() - 6.0, rect.bottom() - 2.5)],
                        Stroke::new(3.0, theme::ACCENT),
                    );
                } else if resp.hovered() {
                    p.rect_filled(rect.shrink2(Vec2::new(0.0, 1.0)), 0.0, theme::HOVER);
                }
                if let Some(c) = self.wb.sheets[i].tab_color {
                    p.line_segment([Pos2::new(rect.left() + 2.0, rect.top() + 2.0), Pos2::new(rect.right() - 2.0, rect.top() + 2.0)], Stroke::new(3.0, theme::rgb(c)));
                }
                let col = if active { theme::ACCENT_DARK } else { theme::TEXT_DIM };
                p.text(rect.center(), Align2::CENTER_CENTER, name, if active { bold.clone() } else { font.clone() }, col);
                p.line_segment([Pos2::new(rect.right(), rect.top() + 6.0), Pos2::new(rect.right(), rect.bottom() - 6.0)], Stroke::new(1.0, theme::BORDER));
                if resp.clicked() {
                    action = Some((i, 0));
                }
                if resp.double_clicked() {
                    action = Some((i, 1));
                }
                resp.context_menu(|ui| {
                    ui.set_min_width(160.0);
                    if ui.button("Insert").clicked() {
                        action = Some((i, 2));
                        ui.close();
                    }
                    if ui.button("Delete").clicked() {
                        action = Some((i, 3));
                        ui.close();
                    }
                    if ui.button("Rename").clicked() {
                        action = Some((i, 1));
                        ui.close();
                    }
                    if ui.button("Move Left").clicked() {
                        action = Some((i, 4));
                        ui.close();
                    }
                    if ui.button("Move Right").clicked() {
                        action = Some((i, 5));
                        ui.close();
                    }
                    if ui.button("Duplicate").clicked() {
                        action = Some((i, 6));
                        ui.close();
                    }
                    if ui.button("Hide").clicked() {
                        action = Some((i, 7));
                        ui.close();
                    }
                    let hidden: Vec<(usize, String)> =
                        self.wb.sheets.iter().enumerate().filter(|(_, s)| s.hidden).map(|(k, s)| (k, s.name.clone())).collect();
                    if !hidden.is_empty() {
                        ui.menu_button("Unhide", |ui| {
                            for (k, n) in hidden {
                                if ui.button(n).clicked() {
                                    action = Some((k, 8));
                                    ui.close();
                                }
                            }
                        });
                    }
                    ui.menu_button("Tab Color", |ui| {
                        if let Some(c) = super::ribbon::color_grid(ui, true) {
                            self.push_undo(&[super::Scope::Sheet(i)]);
                            self.wb.sheets[i].tab_color = c;
                            self.dirty = true;
                            ui.close();
                        }
                    });
                });
            }
            ui.add_space(6.0);
            let (plus, pi) = self.icon(ic::ADD, "+");
            let txt = if pi { RichText::new(plus).font(theme::icon_font(12.0)) } else { RichText::new(plus).size(16.0) };
            if ui.add(egui::Button::new(txt).frame(false).min_size(Vec2::new(26.0, 24.0))).on_hover_text("New sheet (Shift+F11)").clicked() {
                self.add_sheet();
            }
            if let Some((i, a)) = action {
                match a {
                    0 => self.switch_sheet(i),
                    1 => {
                        self.commit_edit(false);
                        self.rename_tab = Some((i, self.wb.sheets[i].name.clone()));
                    }
                    2 => {
                        self.wb.active = i;
                        self.add_sheet();
                    }
                    3 => self.dialog = Some(Dialog::ConfirmDeleteSheet(i)),
                    4 => self.move_sheet(i, false),
                    5 => self.move_sheet(i, true),
                    6 => self.duplicate_sheet(i),
                    7 => self.hide_sheet(i),
                    8 => self.unhide_sheet(i),
                    _ => {}
                }
            }
        });
    }

    // ------------------------------------------------------------------
    // Status bar
    // ------------------------------------------------------------------

    fn selection_stats(&mut self) -> Option<(f64, usize, usize, f64)> {
        let key = (self.wb.active, self.sel(), self.generation);
        if let Some((k, v)) = &self.stats_cache {
            if *k == key {
                return *v;
            }
        }
        let sel = self.sel();
        let sh = self.sheet();
        let (mut sum, mut nums, mut count) = (0.0, 0usize, 0usize);
        let mut err = false;
        for (r, _, cell) in sh.iter_range(sel) {
            if sh.hidden_rows.contains(&r) {
                continue;
            }
            match &cell.value {
                Value::Empty => {}
                Value::Number(n) => {
                    sum += n;
                    nums += 1;
                    count += 1;
                }
                Value::Error(_) => {
                    err = true;
                    count += 1;
                }
                _ => count += 1,
            }
        }
        let v = if count >= 2 && !err { Some((if nums > 0 { sum / nums as f64 } else { 0.0 }, count, nums, sum)) } else { None };
        self.stats_cache = Some((key, v));
        v
    }

    pub fn status_bar_ui(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        ui.painter().line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0, theme::BORDER));
        ui.horizontal_centered(|ui| {
            ui.add_space(10.0);
            ui.label(RichText::new(self.mode_label()).size(12.0).color(theme::TEXT_DIM));
            if let Some(s) = &self.status {
                ui.add_space(16.0);
                ui.label(RichText::new(s).size(12.0).color(theme::TEXT_DIM));
            }
            if self.format_painter.is_some() {
                ui.add_space(16.0);
                ui.label(RichText::new("Click a cell or drag to paste the format. Esc to cancel.").size(12.0).color(theme::TEXT_DIM));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                let pct = format!("{}%", (self.zoom * 100.0).round());
                if ui.add(egui::Button::new(RichText::new(pct).size(12.0)).frame(false)).on_hover_text("Zoom").clicked() {
                    self.dialog = Some(Dialog::Zoom { custom: format!("{}", (self.zoom * 100.0).round()) });
                }
                if ui.add(egui::Button::new(RichText::new("+").size(14.0)).frame(false)).on_hover_text("Zoom In").clicked() {
                    self.zoom = ((self.zoom * 10.0).round() / 10.0 + 0.1).min(4.0);
                }
                let mut z = self.zoom * 100.0;
                let slider = egui::Slider::new(&mut z, 10.0..=400.0).show_value(false).logarithmic(true);
                ui.spacing_mut().slider_width = 110.0;
                if ui.add(slider).changed() {
                    self.zoom = (z / 100.0 * 20.0).round() / 20.0;
                }
                if ui.add(egui::Button::new(RichText::new("−").size(14.0)).frame(false)).on_hover_text("Zoom Out").clicked() {
                    self.zoom = ((self.zoom * 10.0).round() / 10.0 - 0.1).max(0.1);
                }
                ui.add_space(14.0);
                if let Some((avg, count, nums, sum)) = self.selection_stats() {
                    let g = crate::numfmt::general;
                    if nums > 0 {
                        ui.label(RichText::new(format!("Sum: {}", g(sum))).size(12.0));
                        ui.add_space(12.0);
                    }
                    ui.label(RichText::new(format!("Count: {count}")).size(12.0));
                    if nums > 0 {
                        ui.add_space(12.0);
                        ui.label(RichText::new(format!("Average: {}", g(avg))).size(12.0));
                    }
                }
            });
        });
    }

    // ------------------------------------------------------------------
    // Backstage (File)
    // ------------------------------------------------------------------

    pub fn backstage_ui(&mut self, ctx: &egui::Context) {
        let page = self.backstage.unwrap_or(Backstage::Home);
        egui::SidePanel::left("backstage_nav")
            .exact_width(210.0)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(theme::ACCENT_DARK).inner_margin(egui::Margin { left: 0, right: 0, top: 10, bottom: 10 }))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let item = |ui: &mut egui::Ui, text: &str, selected: bool| -> bool {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::click());
                    if selected {
                        ui.painter().rect_filled(rect, 0.0, theme::ACCENT);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, 0.0, Color32::from_rgb(0x20, 0x6B, 0x45));
                    }
                    ui.painter().text(Pos2::new(rect.left() + 26.0, rect.center().y), Align2::LEFT_CENTER, text, theme::ui_font(14.0), Color32::WHITE);
                    resp.clicked()
                };
                let (back, bi) = self.icon(ic::BACK, "←");
                let bt = if bi { RichText::new(back).font(theme::icon_font(16.0)) } else { RichText::new(back).size(18.0) };
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    if ui.add(egui::Button::new(bt.color(Color32::WHITE)).frame(false).min_size(Vec2::new(36.0, 36.0))).clicked() {
                        self.backstage = None;
                    }
                });
                ui.add_space(10.0);
                if item(ui, "Home", page == Backstage::Home) {
                    self.backstage = Some(Backstage::Home);
                }
                if item(ui, "New", false) {
                    self.confirm_then(Pending::New);
                    self.backstage = None;
                }
                if item(ui, "Open", page == Backstage::Open) {
                    self.backstage = Some(Backstage::Open);
                }
                ui.add_space(8.0);
                if item(ui, "Save", false) {
                    self.backstage = None;
                    self.save();
                }
                if item(ui, "Save As", page == Backstage::SaveAs) {
                    self.backstage = Some(Backstage::SaveAs);
                }
                if item(ui, "Print", page == Backstage::Print) {
                    self.backstage = Some(Backstage::Print);
                }
                if item(ui, "Close", false) {
                    self.backstage = None;
                    self.confirm_then(Pending::New);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    if item(ui, "Exit", false) {
                        self.backstage = None;
                        self.confirm_then(Pending::Exit);
                    }
                    if item(ui, "About", page == Backstage::About) {
                        self.backstage = Some(Backstage::About);
                    }
                });
            });
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(Color32::WHITE).inner_margin(egui::Margin::same(36))).show(ctx, |ui| {
            match page {
                Backstage::Home => {
                    ui.label(RichText::new("Good to see you").size(28.0).color(theme::TEXT));
                    ui.add_space(18.0);
                    ui.label(RichText::new("New").size(16.0));
                    ui.add_space(6.0);
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(150.0, 120.0), Sense::click());
                    let p = ui.painter();
                    p.rect_filled(rect, 4.0, Color32::WHITE);
                    p.rect_stroke(rect, 4.0, Stroke::new(if resp.hovered() { 2.0 } else { 1.0 }, if resp.hovered() { theme::ACCENT } else { theme::BORDER }), StrokeKind::Inside);
                    for k in 1..5 {
                        let y = rect.top() + k as f32 * 18.0;
                        p.line_segment([Pos2::new(rect.left() + 10.0, y), Pos2::new(rect.right() - 10.0, y)], Stroke::new(1.0, theme::GRID_LINE));
                    }
                    for k in 1..4 {
                        let x = rect.left() + 10.0 + k as f32 * 32.5;
                        p.line_segment([Pos2::new(x, rect.top() + 18.0), Pos2::new(x, rect.top() + 72.0)], Stroke::new(1.0, theme::GRID_LINE));
                    }
                    p.text(Pos2::new(rect.center().x, rect.bottom() - 18.0), Align2::CENTER_CENTER, "Blank workbook", theme::ui_font(12.5), theme::TEXT);
                    if resp.clicked() {
                        self.backstage = None;
                        self.confirm_then(Pending::New);
                    }
                    ui.add_space(24.0);
                    self.recent_list(ui);
                }
                Backstage::Open => {
                    ui.label(RichText::new("Open").size(28.0));
                    ui.add_space(14.0);
                    let (f, fi) = self.icon(ic::FOLDER, "📂");
                    let t = if fi { format!("{f}") } else { f };
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(RichText::new(t).font(if fi { theme::icon_font(20.0) } else { theme::ui_font(20.0) })).min_size(Vec2::new(44.0, 44.0))).clicked()
                            || ui.add(egui::Button::new(RichText::new("Browse").size(15.0)).frame(false)).clicked()
                        {
                            self.backstage = None;
                            self.confirm_then(Pending::Open(None));
                        }
                    });
                    ui.add_space(18.0);
                    self.recent_list(ui);
                }
                Backstage::SaveAs => {
                    ui.label(RichText::new("Save As").size(28.0));
                    ui.add_space(14.0);
                    ui.label("Workbook (*.xlsx), CSV UTF-8 (*.csv), Text (Tab delimited) (*.txt)");
                    ui.add_space(8.0);
                    if ui.add(egui::Button::new(RichText::new("Browse...").size(15.0)).min_size(Vec2::new(120.0, 34.0))).clicked() {
                        self.backstage = None;
                        self.save_as();
                    }
                }
                Backstage::Print => {
                    ui.label(RichText::new("Print").size(28.0));
                    ui.add_space(14.0);
                    let area = if self.print_selection { Some(self.sel_clipped()) } else { None };
                    let pages = crate::pdf::render(&self.wb, self.wb.active, area, self.page_setup)
                        .map(|b| b.windows(12).filter(|w| w == b"/Type /Page ").count())
                        .unwrap_or(0);
                    ui.horizontal(|ui| {
                        let print = egui::Button::new(RichText::new("Print").size(15.0).color(Color32::WHITE)).fill(theme::ACCENT).min_size(Vec2::new(110.0, 40.0));
                        if ui.add(print).on_hover_text("Opens a print-ready PDF in your PDF viewer").clicked() {
                            let path = std::env::temp_dir().join(format!("{} - print.pdf", self.wb.file_name().trim_end_matches(".xlsx")));
                            match crate::pdf::export(&self.wb, self.wb.active, area, self.page_setup, &path) {
                                Ok(_) => {
                                    let _ = std::process::Command::new("cmd").args(["/C", "start", ""]).arg(&path).spawn();
                                    self.status = Some("Opened the print preview PDF - print it from your PDF viewer.".into());
                                    self.backstage = None;
                                }
                                Err(e) => self.dialog = Some(Dialog::Message { title: "Print".into(), text: e }),
                            }
                        }
                        if ui.add(egui::Button::new(RichText::new("Export to PDF...").size(14.0)).min_size(Vec2::new(130.0, 40.0))).clicked() {
                            let name = format!("{}.pdf", self.wb.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Book1".into()));
                            if let Some(p) = rfd::FileDialog::new().set_title("Export to PDF").add_filter("PDF", &["pdf"]).set_file_name(name).save_file() {
                                match crate::pdf::export(&self.wb, self.wb.active, area, self.page_setup, &p) {
                                    Ok(n) => {
                                        self.status = Some(format!("Exported {n} page(s) to {}", p.display()));
                                        self.backstage = None;
                                    }
                                    Err(e) => self.dialog = Some(Dialog::Message { title: "Export".into(), text: e }),
                                }
                            }
                        }
                    });
                    ui.add_space(16.0);
                    ui.label(RichText::new("Settings").size(16.0));
                    ui.add_space(4.0);
                    egui::Grid::new("print_settings").num_columns(2).spacing(Vec2::new(14.0, 8.0)).show(ui, |ui| {
                        ui.label("What to print");
                        egui::ComboBox::from_id_salt("print_what")
                            .width(220.0)
                            .selected_text(if self.print_selection { "Print Selection" } else { "Print Active Sheet" })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.print_selection, false, "Print Active Sheet");
                                ui.selectable_value(&mut self.print_selection, true, "Print Selection");
                            });
                        ui.end_row();
                        ui.label("Orientation");
                        egui::ComboBox::from_id_salt("print_orient")
                            .width(220.0)
                            .selected_text(if self.page_setup.landscape { "Landscape Orientation" } else { "Portrait Orientation" })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.page_setup.landscape, false, "Portrait Orientation");
                                ui.selectable_value(&mut self.page_setup.landscape, true, "Landscape Orientation");
                            });
                        ui.end_row();
                        ui.label("Paper");
                        egui::ComboBox::from_id_salt("print_paper")
                            .width(220.0)
                            .selected_text(if self.page_setup.a4 { "A4 (21 cm x 29.7 cm)" } else { "Letter (8.5\" x 11\")" })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.page_setup.a4, true, "A4 (21 cm x 29.7 cm)");
                                ui.selectable_value(&mut self.page_setup.a4, false, "Letter (8.5\" x 11\")");
                            });
                        ui.end_row();
                        ui.label("Scaling");
                        egui::ComboBox::from_id_salt("print_fit")
                            .width(220.0)
                            .selected_text(if self.page_setup.fit_width { "Fit All Columns on One Page" } else { "No Scaling" })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.page_setup.fit_width, false, "No Scaling");
                                ui.selectable_value(&mut self.page_setup.fit_width, true, "Fit All Columns on One Page");
                            });
                        ui.end_row();
                        ui.label("Gridlines");
                        ui.checkbox(&mut self.page_setup.gridlines, "Print gridlines");
                        ui.end_row();
                    });
                    ui.add_space(10.0);
                    ui.label(RichText::new(if pages == 0 { "Nothing to print".to_string() } else { format!("{pages} page(s)") }).color(theme::TEXT_DIM));
                }
                Backstage::About => {
                    ui.label(RichText::new("FreedomOffice Sheets").size(28.0).color(theme::ACCENT_DARK));
                    ui.add_space(6.0);
                    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                    ui.add_space(10.0);
                    ui.label("A lightweight, open-source spreadsheet written in Rust. Single executable, no installation.");
                    ui.label("Opens .xlsx, .xlsm, .xls, .xlsb, .ods, .csv and .txt files. Saves .xlsx, .csv and .txt.");
                    ui.add_space(10.0);
                    ui.label("MIT License. Not affiliated with or endorsed by any other spreadsheet vendor.");
                    ui.add_space(6.0);
                    ui.hyperlink("https://github.com/mar0der/FreedomOffice");
                }
            }
        });
    }

    fn recent_list(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Recent").size(16.0));
        ui.add_space(6.0);
        if self.recent.is_empty() {
            ui.label(RichText::new("Files you open will appear here.").color(theme::TEXT_DIM));
            return;
        }
        let mut open = None;
        for p in self.recent.clone() {
            let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let dir = p.parent().map(|d| d.display().to_string()).unwrap_or_default();
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width().min(700.0), 44.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(rect, 3.0, theme::HOVER);
            }
            ui.painter().text(Pos2::new(rect.left() + 10.0, rect.top() + 13.0), Align2::LEFT_CENTER, name, theme::ui_font(14.0), theme::TEXT);
            ui.painter().text(Pos2::new(rect.left() + 10.0, rect.top() + 31.0), Align2::LEFT_CENTER, dir, theme::ui_font(11.5), theme::TEXT_DIM);
            if resp.clicked() {
                open = Some(p.clone());
            }
        }
        if let Some(p) = open {
            self.backstage = None;
            self.confirm_then(Pending::Open(Some(p)));
        }
    }

    // ------------------------------------------------------------------
    // Popups: autocomplete, function hints, filter dropdown
    // ------------------------------------------------------------------

    pub fn popups_ui(&mut self, ctx: &egui::Context) {
        let anchor = match &self.edit {
            Some(e) if e.in_bar => self.edit_bar_rect.map(|r| Pos2::new(r.left() + 6.0, r.bottom() + 2.0)),
            Some(_) => self.edit_rect.map(|r| Pos2::new(r.left(), r.bottom() + 2.0)),
            None => None,
        };
        if let Some(pos) = anchor {
            let list = self.autocomplete();
            if !list.is_empty() {
                let sel = self.edit.as_ref().map(|e| e.ac_index.min(list.len() - 1)).unwrap_or(0);
                let mut pick = None;
                egui::Area::new(egui::Id::new("autocomplete")).fixed_pos(pos).order(egui::Order::Foreground).show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(2)).show(ui, |ui| {
                        ui.set_width(230.0);
                        for (i, n) in list.iter().enumerate() {
                            let (rect, resp) = ui.allocate_exact_size(Vec2::new(226.0, 20.0), Sense::click());
                            if i == sel {
                                ui.painter().rect_filled(rect, 2.0, theme::ACCENT_LIGHT);
                            } else if resp.hovered() {
                                ui.painter().rect_filled(rect, 2.0, theme::HOVER);
                            }
                            ui.painter().text(Pos2::new(rect.left() + 6.0, rect.center().y), Align2::LEFT_CENTER, *n, theme::ui_font(12.5), theme::TEXT);
                            if resp.double_clicked() || resp.clicked() {
                                pick = Some(*n);
                            }
                        }
                        if let Some(sig) = crate::formula::signature(list[sel]) {
                            ui.separator();
                            ui.label(RichText::new(format!("{}({})", list[sel], sig)).size(11.0).color(theme::TEXT_DIM));
                        }
                    });
                });
                if let (Some(n), Some(e)) = (pick, &mut self.edit) {
                    e.accept_function(n);
                }
            } else if let Some((name, arg)) = self.edit.as_ref().and_then(|e| e.func_context()) {
                if let Some(sig) = crate::formula::signature(&name) {
                    egui::Area::new(egui::Id::new("func_hint")).fixed_pos(pos).order(egui::Order::Foreground).interactable(false).show(ctx, |ui| {
                        egui::Frame::popup(ui.style()).fill(Color32::from_rgb(0xFF, 0xFF, 0xF4)).inner_margin(egui::Margin::symmetric(6, 3)).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.label(RichText::new(format!("{name}(")).size(12.0));
                                let parts: Vec<&str> = sig.split(", ").collect();
                                for (i, p) in parts.iter().enumerate() {
                                    let is_cur = i == arg.min(parts.len() - 1) || (p.contains("...") && arg >= i);
                                    let t = RichText::new(*p).size(12.0);
                                    ui.label(if is_cur { t.strong() } else { t });
                                    if i + 1 < parts.len() {
                                        ui.label(RichText::new(", ").size(12.0));
                                    }
                                }
                                ui.label(RichText::new(")").size(12.0));
                            });
                        });
                    });
                }
            }
        }
        self.filter_popup_ui(ctx);
        self.dv_popups_ui(ctx);
    }

    fn filter_popup_ui(&mut self, ctx: &egui::Context) {
        let Some(mut fp) = self.filter_popup.take() else { return };
        let mut close = false;
        let mut apply = false;
        let mut sort: Option<bool> = None;
        let resp = egui::Area::new(egui::Id::new("filter_popup")).fixed_pos(fp.pos + Vec2::new(-200.0, 10.0)).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                ui.set_width(230.0);
                if ui.add(egui::Button::new("Sort A to Z").frame(false)).clicked() {
                    sort = Some(false);
                }
                if ui.add(egui::Button::new("Sort Z to A").frame(false)).clicked() {
                    sort = Some(true);
                }
                ui.separator();
                if ui.add(egui::Button::new("Clear Filter From This Column").frame(false)).clicked() {
                    for v in fp.values.iter_mut() {
                        v.1 = true;
                    }
                    apply = true;
                }
                ui.add(egui::TextEdit::singleline(&mut fp.search).hint_text("Search").desired_width(218.0));
                egui::Frame::new().stroke(Stroke::new(1.0, theme::BORDER)).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        ui.set_width(210.0);
                        let mut all = fp.values.iter().all(|v| v.1);
                        if ui.checkbox(&mut all, "(Select All)").changed() {
                            for v in fp.values.iter_mut() {
                                v.1 = all;
                            }
                        }
                        let q = fp.search.to_lowercase();
                        for (v, on) in fp.values.iter_mut() {
                            if !q.is_empty() && !v.to_lowercase().contains(&q) {
                                continue;
                            }
                            let label = if v.is_empty() { "(Blanks)".to_string() } else { v.clone() };
                            ui.checkbox(on, label);
                        }
                    });
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui.button("   OK   ").clicked() {
                            apply = true;
                        }
                    });
                });
            });
        });
        let outside = ctx.input(|i| i.pointer.any_pressed()) && !resp.response.rect.contains(ctx.input(|i| i.pointer.interact_pos().unwrap_or_default()));
        if let Some(desc) = sort {
            let col = fp.col;
            if let Some(f) = self.sheet().filter.clone() {
                let a = self.active();
                self.sheet_mut().view.active = (a.0, col);
                self.sort_range(f.range, true, &[(col, desc)]);
            }
            return;
        }
        if apply {
            let si = self.wb.active;
            let ex: std::collections::HashSet<String> = fp.values.iter().filter(|v| !v.1).map(|v| v.0.clone()).collect();
            self.push_undo(&[super::Scope::Sheet(si)]);
            if let Some(f) = &mut self.wb.sheets[si].filter {
                f.excluded.insert(fp.col, ex);
            }
            self.apply_filter();
            return;
        }
        if close || outside {
            return;
        }
        self.filter_popup = Some(fp);
        let _ = Rect::NOTHING;
    }
}
