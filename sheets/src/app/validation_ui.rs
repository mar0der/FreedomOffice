//! Data Validation UI: the dialog, entry checks with error alerts, the in-cell list
//! dropdown and the input message tooltip.

use eframe::egui::{self, Align, Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};

use super::dialogs::Dialog;
use super::grid::Layout;
use super::theme;
use super::{App, Editor, Scope};
use crate::condfmt::{operand_from_input, operand_to_input, CmpOp};
use crate::model::{cell_from_input, Cell, Range, Value};
use crate::validation::{self, Allow, ErrorStyle, Validation};

#[derive(Default)]
pub struct DvState {
    /// Set while committing an entry the user accepted from a Warning/Information alert.
    bypass: bool,
    popup: Option<DropList>,
}

pub struct DropList {
    si: usize,
    r: u32,
    c: u32,
    items: Vec<(String, Value)>,
    hover: usize,
    fresh: bool,
    scroll: bool,
}

pub struct DvDialog {
    tab: usize,
    v: Validation,
    min: String,
    max: String,
    source: String,
    formula: String,
    range: Range,
    error: Option<String>,
}

pub struct DvAlert {
    style: ErrorStyle,
    title: String,
    msg: String,
    fill: bool,
    fresh: bool,
}

fn operand_labels(allow: Allow, op: CmpOp) -> (&'static str, &'static str) {
    let (lo, hi, one) = match allow {
        Allow::Date => ("Start date:", "End date:", "Date:"),
        Allow::Time => ("Start time:", "End time:", "Time:"),
        Allow::TextLength => ("Minimum:", "Maximum:", "Length:"),
        _ => ("Minimum:", "Maximum:", "Value:"),
    };
    match op {
        CmpOp::Between | CmpOp::NotBetween => (lo, hi),
        CmpOp::Equal | CmpOp::NotEqual => (one, ""),
        CmpOp::Greater | CmpOp::GreaterEq => (lo, ""),
        CmpOp::Less | CmpOp::LessEq => (hi, ""),
    }
}

impl DvDialog {
    fn new(v: Validation, range: Range) -> Self {
        let fmt = v.allow.operand_format();
        DvDialog {
            tab: 0,
            min: if v.allow.uses_op() { operand_to_input(&v.f1, fmt) } else { String::new() },
            max: if v.allow.uses_op() && v.op.two() { operand_to_input(&v.f2, fmt) } else { String::new() },
            source: if v.allow == Allow::List { validation::list_to_input(&v.f1) } else { String::new() },
            formula: if v.allow == Allow::Custom && !v.f1.is_empty() { format!("={}", v.f1) } else { String::new() },
            v,
            range,
            error: None,
        }
    }

    /// Validation built from the dialog fields, or the message to show.
    fn build(&self) -> Result<Validation, String> {
        let mut v = self.v.clone();
        v.f1.clear();
        v.f2.clear();
        let parse = |f: &str| crate::formula::parse(f).map(|_| ()).map_err(|e| format!("There's a problem with this formula.\n\n{e}"));
        match v.allow {
            Allow::Any => {}
            Allow::List => {
                v.f1 = validation::list_from_input(&self.source);
                if v.f1.is_empty() {
                    return Err("You must enter a list source.".into());
                }
                parse(&v.f1)?;
            }
            Allow::Custom => {
                v.f1 = self.formula.trim().trim_start_matches('=').trim().to_string();
                if v.f1.is_empty() {
                    return Err("You must enter a formula.".into());
                }
                parse(&v.f1)?;
            }
            _ => {
                let (l1, l2) = operand_labels(v.allow, v.op);
                v.f1 = operand_from_input(&self.min);
                if v.f1.is_empty() {
                    return Err(format!("The {} value must be entered.", l1.trim_end_matches(':')));
                }
                parse(&v.f1)?;
                if v.op.two() {
                    v.f2 = operand_from_input(&self.max);
                    if v.f2.is_empty() {
                        return Err(format!("The {} value must be entered.", l2.trim_end_matches(':')));
                    }
                    parse(&v.f2)?;
                }
            }
        }
        Ok(v)
    }
}

fn window(title: &str) -> egui::Window<'static> {
    egui::Window::new(title.to_string()).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, -40.0])
}

/// Paints the alert icon for an error style.
fn alert_icon(ui: &mut egui::Ui, style: ErrorStyle) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
    let p = ui.painter();
    let c = r.center();
    match style {
        ErrorStyle::Stop => {
            p.circle_filled(c, 15.0, Color32::from_rgb(0xD1, 0x34, 0x38));
            let d = 6.0;
            let s = Stroke::new(2.5, Color32::WHITE);
            p.line_segment([c + Vec2::new(-d, -d), c + Vec2::new(d, d)], s);
            p.line_segment([c + Vec2::new(-d, d), c + Vec2::new(d, -d)], s);
        }
        ErrorStyle::Warning => {
            let pts = vec![c + Vec2::new(0.0, -15.0), c + Vec2::new(16.0, 13.0), c + Vec2::new(-16.0, 13.0)];
            p.add(egui::Shape::convex_polygon(pts, Color32::from_rgb(0xFF, 0xC8, 0x3D), Stroke::new(1.0, Color32::from_rgb(0xB0, 0x80, 0x00))));
            p.text(c + Vec2::new(0.0, 3.0), Align2::CENTER_CENTER, "!", theme::ui_bold(17.0), Color32::BLACK);
        }
        ErrorStyle::Information => {
            p.circle_filled(c, 15.0, Color32::from_rgb(0x1E, 0x6F, 0xD0));
            p.text(c, Align2::CENTER_CENTER, "i", theme::ui_bold(18.0), Color32::WHITE);
        }
    }
}

fn paint_dropdown_button(p: &egui::Painter, r: Rect, hover: bool) {
    p.rect_filled(r, 0.0, if hover { Color32::from_gray(230) } else { Color32::from_gray(246) });
    p.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_gray(160)), StrokeKind::Inside);
    let c = r.center();
    let pts = vec![c + Vec2::new(-4.0, -2.0), c + Vec2::new(4.0, -2.0), c + Vec2::new(0.0, 2.5)];
    p.add(egui::Shape::convex_polygon(pts, Color32::from_gray(60), Stroke::NONE));
}

impl App {
    // ------------------------------------------------------------------
    // Dialog
    // ------------------------------------------------------------------

    /// The big "Data Validation" button in the Data > Data Tools group.
    pub fn dv_ribbon_button(&mut self, ui: &mut egui::Ui) {
        let resp = super::condfmt_ui::ribbon_button(ui, Vec2::new(66.0, 66.0), "Data\nValidation");
        let r = Rect::from_center_size(Pos2::new(resp.rect.center().x, resp.rect.top() + 17.0), Vec2::new(22.0, 18.0));
        let p = ui.painter();
        p.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_gray(110)), StrokeKind::Inside);
        p.line_segment([Pos2::new(r.left(), r.top() + 6.0), Pos2::new(r.right(), r.top() + 6.0)], Stroke::new(1.0, Color32::from_gray(110)));
        let g = Stroke::new(2.0, Color32::from_rgb(0x21, 0x8A, 0x4C));
        p.line_segment([Pos2::new(r.left() + 3.0, r.bottom() - 6.0), Pos2::new(r.left() + 6.0, r.bottom() - 3.0)], g);
        p.line_segment([Pos2::new(r.left() + 6.0, r.bottom() - 3.0), Pos2::new(r.left() + 11.0, r.top() + 8.0)], g);
        p.circle_stroke(Pos2::new(r.right() - 5.0, r.bottom() - 5.0), 3.5, Stroke::new(1.5, Color32::from_rgb(0xD1, 0x34, 0x38)));
        if resp.on_hover_text("Data Validation\nPick from a list of rules to limit the type of data that can be entered in a cell.").clicked() {
            self.dv_open_dialog();
        }
    }

    pub fn dv_open_dialog(&mut self) {
        self.commit_edit(false);
        let (r, c) = self.active();
        let v = validation::at(self.sheet(), r, c).cloned().unwrap_or_default();
        self.dialog = Some(Dialog::Validation(Box::new(DvDialog::new(v, self.sel()))));
    }

    /// Shows the Data Validation dialog; returns false when it closed.
    pub fn dv_dialog_ui(&mut self, ctx: &egui::Context, d: &mut DvDialog) -> bool {
        let mut result = None;
        window("Data Validation").show(ctx, |ui| {
            ui.set_min_width(440.0);
            ui.horizontal(|ui| {
                for (i, t) in ["Settings", "Input Message", "Error Alert"].iter().enumerate() {
                    if ui.selectable_label(d.tab == i, *t).clicked() {
                        d.tab = i;
                    }
                }
            });
            ui.separator();
            egui::Frame::new().inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                ui.set_min_height(230.0);
                ui.set_width(420.0);
                match d.tab {
                    0 => settings_tab(ui, d),
                    1 => {
                        ui.checkbox(&mut d.v.show_input, "Show input message when cell is selected");
                        ui.add_space(6.0);
                        ui.label("When cell is selected, show this input message:");
                        ui.label("Title:");
                        ui.add(egui::TextEdit::singleline(&mut d.v.input_title).desired_width(400.0).char_limit(32));
                        ui.label("Input message:");
                        ui.add(egui::TextEdit::multiline(&mut d.v.input_msg).desired_width(400.0).desired_rows(5).char_limit(255));
                    }
                    _ => {
                        ui.checkbox(&mut d.v.show_error, "Show error alert after invalid data is entered");
                        ui.add_space(6.0);
                        ui.label("When user enters invalid data, show this error alert:");
                        ui.horizontal_top(|ui| {
                            ui.vertical(|ui| {
                                ui.label("Style:");
                                egui::ComboBox::from_id_salt("dv_style").selected_text(d.v.error_style.label()).show_ui(ui, |ui| {
                                    for s in ErrorStyle::ALL {
                                        ui.selectable_value(&mut d.v.error_style, s, s.label());
                                    }
                                });
                                ui.add_space(10.0);
                                alert_icon(ui, d.v.error_style);
                            });
                            ui.add_space(10.0);
                            ui.vertical(|ui| {
                                ui.label("Title:");
                                ui.add(egui::TextEdit::singleline(&mut d.v.error_title).desired_width(260.0).char_limit(32));
                                ui.label("Error message:");
                                ui.add(egui::TextEdit::multiline(&mut d.v.error_msg).desired_width(260.0).desired_rows(5).char_limit(255));
                            });
                        });
                    }
                }
            });
            if let Some(e) = &d.error {
                ui.colored_label(theme::error_text(), e);
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new("Clear All").min_size(Vec2::new(76.0, 24.0))).clicked() {
                    *d = DvDialog::new(Validation::default(), d.range);
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(egui::Button::new("Cancel").min_size(Vec2::new(76.0, 24.0))).clicked() {
                        result = Some(false);
                    }
                    if ui.add(egui::Button::new("OK").min_size(Vec2::new(76.0, 24.0)).fill(theme::accent_light())).clicked() {
                        result = Some(true);
                    }
                });
            });
            let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
            if esc {
                result = Some(false);
            } else if enter && d.tab == 0 {
                result = Some(true);
            }
        });
        match result {
            Some(true) => match d.build() {
                Ok(v) => {
                    let si = self.wb.active;
                    self.push_undo(&[Scope::Sheet(si)]);
                    validation::set_for_range(&mut self.wb.sheets[si], d.range, Some(v));
                    self.changed();
                    false
                }
                Err(e) => {
                    d.error = Some(e);
                    d.tab = 0;
                    true
                }
            },
            Some(false) => false,
            None => true,
        }
    }

    // ------------------------------------------------------------------
    // Entry checks
    // ------------------------------------------------------------------

    /// Validates an entry before it is committed. Returns false (and raises the error
    /// alert) when the value breaks the cell's validation.
    pub fn dv_check_entry(&mut self, e: &Editor, text: &str, fill: bool) -> bool {
        if self.dv.bypass {
            return true;
        }
        let (si, r, c) = (e.sheet, e.row, e.col);
        let Some(v) = self.wb.sheets.get(si).and_then(|s| validation::at(s, r, c)).cloned() else { return true };
        if !v.show_error || v.allow == Allow::Any {
            return true;
        }
        let style = self.wb.sheets[si].get(r, c).map(|c| c.style).unwrap_or(0);
        let cell = cell_from_input(&mut self.wb, text, style);
        // Put the entry in place so custom formulas (and formulas of the entry) see it.
        let old = self.wb.sheets[si].remove(r, c);
        if let Some(cell) = &cell {
            self.wb.sheets[si].rows.entry(r).or_default().insert(c, cell.clone());
        }
        let value = match &cell {
            Some(Cell { formula: Some(f), .. }) => match &f.ast {
                Some(ast) => crate::formula::eval_expr(&self.wb, si, r, c, ast),
                None => Value::Error(crate::model::ErrorKind::Name),
            },
            Some(cell) => cell.value.clone(),
            None => Value::Empty,
        };
        let ok = v.check(&self.wb, si, r, c, &value);
        self.wb.sheets[si].remove(r, c);
        if let Some(o) = old {
            self.wb.sheets[si].rows.entry(r).or_default().insert(c, o);
        }
        if ok {
            return true;
        }
        let (title, msg) = v.alert_text();
        self.dialog = Some(Dialog::DvAlert(Box::new(DvAlert { style: v.error_style, title, msg, fill, fresh: true })));
        false
    }

    /// The error alert raised by an invalid entry; returns false when it closed.
    pub fn dv_alert_ui(&mut self, ctx: &egui::Context, a: &mut DvAlert) -> bool {
        // 0 = retry / no (keep editing), 1 = cancel (discard), 2 = accept the entry.
        let mut act: Option<u8> = None;
        window(&a.title).show(ctx, |ui| {
            ui.set_min_width(360.0);
            ui.horizontal_top(|ui| {
                alert_icon(ui, a.style);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.set_max_width(320.0);
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(a.msg.as_str()).wrap());
                    if a.style == ErrorStyle::Warning {
                        ui.add_space(6.0);
                        ui.label("Continue?");
                    }
                });
            });
            ui.add_space(8.0);
            // (label, action); the default button (Enter) follows the familiar dialogs.
            let (labels, default): (&[(&str, u8)], u8) = match a.style {
                ErrorStyle::Stop => (&[("Retry", 0), ("Cancel", 1)], 0),
                ErrorStyle::Warning => (&[("Yes", 2), ("No", 0), ("Cancel", 1)], 0),
                ErrorStyle::Information => (&[("OK", 2), ("Cancel", 1)], 2),
            };
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    for (l, code) in labels.iter().rev() {
                        let mut b = egui::Button::new(*l).min_size(Vec2::new(76.0, 24.0));
                        if *code == default {
                            b = b.fill(theme::accent_light());
                        }
                        if ui.add(b).clicked() {
                            act = Some(*code);
                        }
                    }
                });
            });
            if !a.fresh {
                let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
                if esc {
                    act = Some(1);
                } else if enter {
                    act = Some(default);
                }
            }
        });
        a.fresh = false;
        match act {
            Some(0) => {
                if let Some(e) = &mut self.edit {
                    e.select_all();
                }
                false
            }
            Some(1) => {
                self.cancel_edit();
                false
            }
            Some(_) => {
                self.dv.bypass = true;
                self.commit_edit(a.fill);
                self.dv.bypass = false;
                false
            }
            None => true,
        }
    }

    // ------------------------------------------------------------------
    // In-cell dropdown and input message
    // ------------------------------------------------------------------

    /// Screen rectangle of the list arrow next to the active cell, when it has one.
    pub fn dv_arrow_rect(&self, lay: &Layout) -> Option<Rect> {
        let (r, c) = self.active();
        let sh = self.sheet();
        if !validation::at(sh, r, c)?.shows_dropdown() {
            return None;
        }
        let rg = sh.merge_at(r, c).unwrap_or(Range::cell(r, c));
        let cell = self.range_rect(lay, rg);
        if !lay.cells.contains(cell.center()) || cell.width() <= 0.0 {
            return None;
        }
        let h = cell.height().min(20.0 * self.zoom.max(0.75));
        Some(Rect::from_min_size(Pos2::new(cell.right() + 1.0, cell.bottom() - h), Vec2::new(16.0, h)))
    }

    pub fn dv_paint_arrow(&self, p: &egui::Painter, lay: &Layout, hover: Option<Pos2>) {
        if let Some(r) = self.dv_arrow_rect(lay) {
            paint_dropdown_button(p, r, hover.is_some_and(|h| r.contains(h)) || self.dv.popup.is_some());
        }
    }

    pub fn dv_popup_open(&self) -> bool {
        self.dv.popup.is_some()
    }

    /// Opens the list for the active cell (arrow click or Alt+Down).
    pub fn dv_open_dropdown(&mut self) {
        if self.dv.popup.take().is_some() {
            return;
        }
        let (r, c) = self.active();
        let si = self.wb.active;
        let items = match validation::at(self.sheet(), r, c) {
            Some(v) if v.allow == Allow::List => v.list_items(&self.wb, si, r, c),
            _ => self.pick_list(r, c),
        };
        if items.is_empty() {
            return;
        }
        let cur = crate::numfmt::value_to_text(self.sheet().value(r, c));
        let hover = items.iter().position(|(t, _)| t.eq_ignore_ascii_case(&cur)).unwrap_or(0);
        self.dv.popup = Some(DropList { si, r, c, items, hover, fresh: true, scroll: true });
    }

    /// "Pick From Drop-down List": distinct text entries above/below in the same column.
    fn pick_list(&self, r: u32, c: u32) -> Vec<(String, Value)> {
        let sh = self.sheet();
        let mut seen: Vec<String> = Vec::new();
        for dir in [-1i64, 1] {
            let mut k = r as i64 + dir;
            while k >= 0 && k < crate::model::MAX_ROWS as i64 {
                match sh.value(k as u32, c) {
                    Value::Empty => break,
                    Value::Text(t) => {
                        if !seen.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                            seen.push(t.clone());
                        }
                    }
                    _ => {}
                }
                k += dir;
                if seen.len() > 5000 {
                    break;
                }
            }
        }
        seen.sort_by_key(|a| a.to_lowercase());
        seen.into_iter().map(|t| (t.clone(), Value::Text(t))).collect()
    }

    fn dv_pick(&mut self, si: usize, r: u32, c: u32, text: &str, v: Value) {
        self.push_undo(&[Scope::Range(si, Range::cell(r, c))]);
        let style = self.wb.sheets[si].get(r, c).map(|c| c.style).unwrap_or(0);
        let cell = match v {
            Value::Text(s) => Some(Cell { value: Value::Text(s), formula: None, style }),
            _ => cell_from_input(&mut self.wb, text, style),
        };
        self.wb.sheets[si].set(r, c, cell);
        self.changed();
    }

    /// Draws the list popup and the input message; called with the other popups.
    pub fn dv_popups_ui(&mut self, ctx: &egui::Context) {
        self.dv_dropdown_ui(ctx);
        self.dv_input_message_ui(ctx);
    }

    fn dv_dropdown_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dl) = self.dv.popup.take() else { return };
        if dl.si != self.wb.active || self.active() != (dl.r, dl.c) || self.dialog.is_some() || self.edit.is_some() {
            return;
        }
        let Some(lay) = self.layout.clone() else { return };
        let rg = self.sheet().merge_at(dl.r, dl.c).unwrap_or(Range::cell(dl.r, dl.c));
        let cell = self.range_rect(&lay, rg);
        let width = (cell.width() + 17.0).max(90.0);
        let row_h = 18.0;
        let n = dl.items.len();
        let mut pick = None;
        let mut close = false;
        let (up, down, enter, esc, moved) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Tab),
                i.key_pressed(egui::Key::Escape),
                i.pointer.delta() != Vec2::ZERO,
            )
        });
        if n > 0 && !dl.fresh {
            if up {
                dl.hover = dl.hover.saturating_sub(1);
                dl.scroll = true;
            }
            if down {
                dl.hover = (dl.hover + 1).min(n - 1);
                dl.scroll = true;
            }
            if enter {
                pick = Some(dl.hover);
            }
        }
        if esc {
            close = true;
        }
        let resp = egui::Area::new(egui::Id::new("dv_dropdown"))
            .fixed_pos(Pos2::new(cell.left(), cell.bottom() + 1.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(1)).corner_radius(0).show(ui, |ui| {
                    ui.set_width(width);
                    egui::ScrollArea::vertical().max_height(row_h * 8.0).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        if n == 0 {
                            ui.label(RichText::new("(no items)").color(theme::text_dim()));
                        }
                        for (i, (t, _)) in dl.items.iter().enumerate() {
                            let (rect, resp) = ui.allocate_exact_size(Vec2::new(width - 2.0, row_h), Sense::click());
                            if resp.hovered() && moved {
                                dl.hover = i;
                            }
                            if i == dl.hover {
                                ui.painter().rect_filled(rect, 0.0, theme::accent_light());
                                if dl.scroll {
                                    resp.scroll_to_me(None);
                                }
                            }
                            ui.painter().text(Pos2::new(rect.left() + 4.0, rect.center().y), Align2::LEFT_CENTER, t, theme::ui_font(12.5), theme::text());
                            if resp.clicked() {
                                pick = Some(i);
                            }
                        }
                    });
                });
            });
        dl.scroll = false;
        let ptr = ctx.input(|i| i.pointer.interact_pos());
        let outside = !dl.fresh && ctx.input(|i| i.pointer.any_pressed()) && !ptr.is_some_and(|p| resp.response.rect.contains(p));
        dl.fresh = false;
        if let Some(i) = pick {
            let (t, v) = dl.items[i].clone();
            self.dv_pick(dl.si, dl.r, dl.c, &t, v);
            return;
        }
        if close || outside {
            return;
        }
        self.dv.popup = Some(dl);
    }

    fn dv_input_message_ui(&self, ctx: &egui::Context) {
        if self.dialog.is_some() || self.dv.popup.is_some() || self.filter_popup.is_some() {
            return;
        }
        let (r, c) = self.active();
        let Some(v) = validation::at(self.sheet(), r, c) else { return };
        if !v.has_input_message() {
            return;
        }
        let Some(lay) = &self.layout else { return };
        let rg = self.sheet().merge_at(r, c).unwrap_or(Range::cell(r, c));
        let cell = self.range_rect(lay, rg);
        if !lay.cells.contains(cell.center()) {
            return;
        }
        let pos = Pos2::new(cell.left() + (cell.width() * 0.5).min(40.0), cell.bottom() + 8.0);
        let (title, msg) = (v.input_title.trim(), v.input_msg.trim());
        egui::Area::new(egui::Id::new("dv_input_message")).fixed_pos(pos).order(egui::Order::Foreground).interactable(false).show(ctx, |ui| {
            egui::Frame::new()
                .fill(Color32::from_rgb(0xFF, 0xFF, 0xE1))
                .stroke(Stroke::new(1.0, Color32::from_gray(100)))
                .inner_margin(egui::Margin::symmetric(7, 5))
                .show(ui, |ui| {
                    ui.set_max_width(230.0);
                    if !title.is_empty() {
                        ui.label(RichText::new(title).strong().size(12.0).color(Color32::BLACK));
                    }
                    if !msg.is_empty() {
                        ui.add(egui::Label::new(RichText::new(msg).size(12.0).color(Color32::BLACK)).wrap());
                    }
                });
        });
    }
}

fn settings_tab(ui: &mut egui::Ui, d: &mut DvDialog) {
    ui.label(RichText::new("Validation criteria").strong());
    ui.add_space(4.0);
    egui::Grid::new("dv_settings").num_columns(2).spacing(Vec2::new(10.0, 8.0)).show(ui, |ui| {
        ui.label("Allow:");
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("dv_allow").width(170.0).selected_text(d.v.allow.label()).show_ui(ui, |ui| {
                for a in Allow::ALL {
                    if ui.selectable_value(&mut d.v.allow, a, a.label()).clicked() {
                        d.error = None;
                    }
                }
            });
            if d.v.allow != Allow::Any {
                ui.checkbox(&mut d.v.ignore_blank, "Ignore blank");
            }
        });
        ui.end_row();
        if d.v.allow.uses_op() {
            ui.label("Data:");
            egui::ComboBox::from_id_salt("dv_op").width(170.0).selected_text(d.v.op.label()).show_ui(ui, |ui| {
                for op in CmpOp::ALL {
                    ui.selectable_value(&mut d.v.op, op, op.label());
                }
            });
            ui.end_row();
            let (l1, l2) = operand_labels(d.v.allow, d.v.op);
            ui.label(l1);
            ui.add(egui::TextEdit::singleline(&mut d.min).desired_width(240.0));
            ui.end_row();
            if d.v.op.two() {
                ui.label(l2);
                ui.add(egui::TextEdit::singleline(&mut d.max).desired_width(240.0));
                ui.end_row();
            }
        }
        match d.v.allow {
            Allow::List => {
                ui.label("Source:");
                ui.add(egui::TextEdit::singleline(&mut d.source).desired_width(240.0).hint_text("Yes,No  or  =$A$1:$A$5"));
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut d.v.dropdown, "In-cell dropdown");
                ui.end_row();
            }
            Allow::Custom => {
                ui.label("Formula:");
                ui.add(egui::TextEdit::singleline(&mut d.formula).desired_width(240.0).hint_text("=ISNUMBER(A1)"));
                ui.end_row();
            }
            _ => {}
        }
    });
    ui.add_space(8.0);
    let hint = match d.v.allow {
        Allow::Date => "Enter dates like 1/31/2025, or a formula such as =TODAY().",
        Allow::Time => "Enter times like 9:00 or 17:30, or a formula.",
        Allow::List => "Separate items with commas, or refer to cells with =$A$1:$A$5.",
        Allow::Custom => "The formula must be TRUE for the entry to be accepted.",
        Allow::Any => "Any value can be entered.",
        _ => "Enter a number, or a formula such as =$B$1.",
    };
    ui.label(RichText::new(hint).size(11.0).color(theme::text_dim()));
}
