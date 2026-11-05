//! The PivotTable Fields pane and the contextual PivotTable Analyze and Design tabs.

use eframe::egui::{self, Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};

use super::pivot_ui::DragSrc;
use super::ribbon::{big_btn, icon_job};
use super::theme::{self, rgb};
use super::{App, RibbonTab};
use crate::pivot::{self, Area, FieldSource, Form, PivotTable, VALUES};

/// Rectangles of the four areas and their entries, collected while drawing (for drops).
#[derive(Default)]
struct DropZones {
    areas: Vec<(Area, Rect, Vec<Rect>)>,
    list: Option<Rect>,
}

fn area_icon(a: Area) -> &'static str {
    match a {
        Area::Filters => "▼",
        Area::Columns => "⫴",
        Area::Rows => "≡",
        Area::Values => "Σ",
    }
}

/// Label of an area entry.
fn entry_label(pt: &PivotTable, a: Area, idx: usize) -> String {
    match a {
        Area::Values => pt.values.get(idx).map(|d| d.name.clone()).unwrap_or_default(),
        _ => match pt.list(a).get(idx) {
            Some(&VALUES) => "Σ Values".into(),
            Some(&f) => pt.fields.get(f).map(|x| x.display().to_string()).unwrap_or_default(),
            None => String::new(),
        },
    }
}

fn entry_field(pt: &PivotTable, a: Area, idx: usize) -> usize {
    match a {
        Area::Values => pt.values.get(idx).map_or(0, |d| d.field),
        _ => pt.list(a).get(idx).copied().unwrap_or(0),
    }
}

/// Contextual tab label in the tab strip.
fn tab_label(ui: &mut egui::Ui, name: &str, sel: bool) -> egui::Response {
    let font = if sel { theme::ui_bold(13.0) } else { theme::ui_font(13.0) };
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
    resp
}

fn big(ui: &mut egui::Ui, icon: &str, label: &str, tip: &str, checked: bool) -> egui::Response {
    big_btn(ui, icon_job(icon, false, label, 20.0, true, theme::text()), 52.0, checked).on_hover_text(tip)
}

fn small(ui: &mut egui::Ui, text: &str, tip: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).size(12.5)).min_size(Vec2::new(0.0, 22.0))).on_hover_text(tip)
}

/// Miniature of a PivotTable style: header band, a subheading row, body rows and the total row.
fn paint_style_swatch(p: &egui::Painter, rect: Rect, name: &str, hot: bool) {
    let d = pivot::style::def(name);
    let rows = [d.header, d.row_sub[0], d.whole, d.whole, d.grand_row];
    let rh = rect.height() / rows.len() as f32;
    for (k, e) in rows.iter().enumerate() {
        let row = Rect::from_min_size(Pos2::new(rect.left(), rect.top() + k as f32 * rh), Vec2::new(rect.width(), rh));
        p.rect_filled(row, 0.0, e.fill.or(d.whole.fill).map(rgb).unwrap_or(Color32::WHITE));
        if e.bold {
            let c = e.color.map(rgb).unwrap_or(Color32::from_gray(60));
            p.line_segment([Pos2::new(row.left() + 3.0, row.center().y), Pos2::new(row.left() + row.width() * 0.45, row.center().y)], Stroke::new(1.5, c));
        }
        if let Some((_, c)) = e.bottom.or(if k > 0 && k < 4 { d.inner_h.map(|c| (crate::model::BorderLine::Thin, c)) } else { None }) {
            p.line_segment([row.left_bottom(), row.right_bottom()], Stroke::new(1.0, rgb(c)));
        }
        if let Some((_, c)) = e.top {
            p.line_segment([row.left_top(), row.right_top()], Stroke::new(1.0, rgb(c)));
        }
    }
    p.rect_stroke(rect, 0.0, Stroke::new(if hot { 2.0 } else { 1.0 }, if hot { theme::accent() } else { Color32::from_gray(200) }), StrokeKind::Inside);
}

impl App {
    // ------------------------------------------------------------------
    // Ribbon
    // ------------------------------------------------------------------

    /// The contextual "PivotTable Analyze" and "Design" tab labels.
    pub(super) fn pivot_tab_buttons(&mut self, ui: &mut egui::Ui) {
        if self.active_pivot().is_none() || self.selected_chart().is_some() {
            if matches!(self.tab, RibbonTab::PivotAnalyze | RibbonTab::PivotDesign) {
                self.tab = self.pivots.prev_tab.take().unwrap_or(RibbonTab::Home);
            }
            return;
        }
        for (t, name) in [(RibbonTab::PivotAnalyze, "PivotTable Analyze"), (RibbonTab::PivotDesign, "Design")] {
            let sel = self.tab == t && !self.ribbon_collapsed;
            if tab_label(ui, name, sel).clicked() {
                if !matches!(self.tab, RibbonTab::PivotAnalyze | RibbonTab::PivotDesign) {
                    self.pivots.prev_tab = Some(self.tab);
                }
                self.tab = t;
                self.ribbon_collapsed = false;
            }
        }
    }

    pub(super) fn pivot_analyze_tab(&mut self, ui: &mut egui::Ui) {
        let Some((si, pi)) = self.active_pivot() else { return };
        let pt = self.wb.sheets[si].pivots[pi].clone();
        let name_id = egui::Id::new("pivot_name_box");
        if self.pivots.name_for != Some((si, pi)) || !ui.memory(|m| m.has_focus(name_id)) {
            self.pivots.name = pt.name.clone();
            self.pivots.name_for = Some((si, pi));
        }
        self.group(ui, "PivotTable", |app, ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("PivotTable Name:").size(12.0));
                let r = ui.add(egui::TextEdit::singleline(&mut app.pivots.name).id(name_id).desired_width(120.0));
                if r.lost_focus() {
                    let n = app.pivots.name.trim().to_string();
                    let taken = app.wb.sheets.iter().enumerate().any(|(s, sh)| sh.pivots.iter().enumerate().any(|(i, p)| (s, i) != (si, pi) && p.name.eq_ignore_ascii_case(&n)));
                    if !n.is_empty() && !taken && n != pt.name {
                        app.pivot_edit(si, pi, |p| p.name = n);
                    }
                }
                let r = small(ui, "⚙ Options ▾", "PivotTable Options");
                egui::Popup::menu(&r).show(|ui| {
                    if ui.button("Options...").clicked() {
                        app.open_pivot_options();
                        ui.close();
                    }
                    let mut gpd = !app.pivots.no_gpd;
                    if ui.checkbox(&mut gpd, "Generate GetPivotData").changed() {
                        app.pivots.no_gpd = !gpd;
                    }
                });
            });
        });
        self.group(ui, "Active Field", |app, ui| {
            let f = app.active_field();
            let d = app.active_data();
            ui.vertical(|ui| {
                ui.label(RichText::new("Active Field:").size(12.0));
                let name = match (d, f) {
                    (Some(d), _) => pt.values.get(d).map(|x| x.name.clone()).unwrap_or_default(),
                    (None, Some(f)) => pt.fields[f].display().to_string(),
                    _ => String::new(),
                };
                ui.add_enabled(false, egui::TextEdit::singleline(&mut name.clone()).desired_width(120.0));
                if ui.add_enabled(f.is_some() || d.is_some(), egui::Button::new(RichText::new("⚙ Field Settings").size(12.5))).clicked() {
                    app.open_field_settings();
                }
            });
            ui.vertical(|ui| {
                if ui.add_enabled(f.is_some(), egui::Button::new(RichText::new("⊞ Expand Field").size(12.5))).on_hover_text("Expand all items of the active field").clicked() {
                    app.expand_field(true);
                }
                if ui.add_enabled(f.is_some(), egui::Button::new(RichText::new("⊟ Collapse Field").size(12.5))).on_hover_text("Collapse all items of the active field").clicked() {
                    app.expand_field(false);
                }
            });
        });
        self.group(ui, "Group", |app, ui| {
            ui.vertical(|ui| {
                if small(ui, "→ Group Selection", "Group the active date or number field").clicked() {
                    app.open_group_dialog();
                }
                if small(ui, "⇠ Ungroup", "Remove the grouping of the active field").clicked() {
                    app.ungroup_active();
                }
                if small(ui, "⧉ Group Field", "Group the active date or number field").clicked() {
                    app.open_group_dialog();
                }
            });
        });
        self.group(ui, "Data", |app, ui| {
            let r = big(ui, "⟳", "Refresh", "Refresh (Alt+F5)\nGet the latest data from the source", false);
            if r.clicked() {
                app.pivot_refresh_active();
            }
            r.context_menu(|ui| {
                if ui.button("Refresh All (Ctrl+Alt+F5)").clicked() {
                    app.pivot_refresh_all();
                    ui.close();
                }
            });
            ui.vertical(|ui| {
                if small(ui, "⟳ Refresh All", "Refresh All (Ctrl+Alt+F5)").clicked() {
                    app.pivot_refresh_all();
                }
                if small(ui, "▤ Change Data Source", "Change the source data for this PivotTable").clicked() {
                    app.open_change_source();
                }
            });
        });
        self.group(ui, "Actions", |app, ui| {
            ui.vertical(|ui| {
                let r = small(ui, "✕ Clear ▾", "Remove fields, formatting and filters");
                egui::Popup::menu(&r).show(|ui| {
                    if ui.button("Clear All").clicked() {
                        app.clear_pivot(false);
                        ui.close();
                    }
                    if ui.button("Clear Filters").clicked() {
                        app.clear_pivot(true);
                        ui.close();
                    }
                });
                let r = small(ui, "⬚ Select ▾", "Select parts of the PivotTable");
                egui::Popup::menu(&r).show(|ui| {
                    for (k, l) in [(3u8, "Labels and Values"), (1, "Values"), (2, "Labels"), (0, "Entire PivotTable")] {
                        if ui.button(l).clicked() {
                            app.select_pivot_part(k);
                            ui.close();
                        }
                    }
                });
                if small(ui, "⇱ Move PivotTable", "Move this PivotTable to another location").clicked() {
                    app.open_move_pivot();
                }
            });
        });
        self.group(ui, "Show", |app, ui| {
            if big(ui, "☰", "Field\nList", "Show or hide the Field List", !app.pivots.hide_list).clicked() {
                app.pivots.hide_list = !app.pivots.hide_list;
            }
            ui.vertical(|ui| {
                if ui.add(egui::Button::selectable(pt.show_buttons, RichText::new("± +/- Buttons").size(12.5))).clicked() {
                    app.pivot_edit(si, pi, |p| p.show_buttons = !p.show_buttons);
                }
                if ui.add(egui::Button::selectable(pt.field_headers, RichText::new("▾ Field Headers").size(12.5))).clicked() {
                    app.pivot_edit(si, pi, |p| p.field_headers = !p.field_headers);
                }
            });
        });
    }

    pub(super) fn pivot_design_tab(&mut self, ui: &mut egui::Ui) {
        let Some((si, pi)) = self.active_pivot() else { return };
        let pt = self.wb.sheets[si].pivots[pi].clone();
        self.group(ui, "Layout", |app, ui| {
            let r = big(ui, "▤", "Subtotals", "Show or hide subtotals", false);
            egui::Popup::menu(&r).show(|ui| {
                let on = pt.rows.iter().chain(pt.cols.iter()).any(|f| *f != VALUES && pt.fields[*f].subtotal);
                if ui.add(egui::Button::selectable(!on, "Do Not Show Subtotals")).clicked() {
                    app.pivot_edit(si, pi, |p| p.fields.iter_mut().for_each(|f| f.subtotal = false));
                    ui.close();
                }
                if ui.add(egui::Button::selectable(on && !pt.subtotals_top, "Show all Subtotals at Bottom of Group")).clicked() {
                    app.pivot_edit(si, pi, |p| {
                        p.fields.iter_mut().for_each(|f| f.subtotal = true);
                        p.subtotals_top = false;
                    });
                    ui.close();
                }
                if ui.add_enabled(pt.form != Form::Tabular, egui::Button::selectable(on && pt.subtotals_top, "Show all Subtotals at Top of Group")).clicked() {
                    app.pivot_edit(si, pi, |p| {
                        p.fields.iter_mut().for_each(|f| f.subtotal = true);
                        p.subtotals_top = true;
                    });
                    ui.close();
                }
            });
            let r = big(ui, "▦", "Grand\nTotals", "Show or hide grand totals", false);
            egui::Popup::menu(&r).show(|ui| {
                for (label, rg, cg) in [
                    ("Off for Rows and Columns", false, false),
                    ("On for Rows and Columns", true, true),
                    ("On for Rows Only", true, false),
                    ("On for Columns Only", false, true),
                ] {
                    if ui.add(egui::Button::selectable(pt.row_grand == rg && pt.col_grand == cg, label)).clicked() {
                        app.pivot_edit(si, pi, |p| {
                            p.row_grand = rg;
                            p.col_grand = cg;
                        });
                        ui.close();
                    }
                }
            });
            let r = big(ui, "▥", "Report\nLayout", "Adjust the report layout", false);
            egui::Popup::menu(&r).show(|ui| {
                for (label, form) in [("Show in Compact Form", Form::Compact), ("Show in Outline Form", Form::Outline), ("Show in Tabular Form", Form::Tabular)] {
                    if ui.add(egui::Button::selectable(pt.form == form, label)).clicked() {
                        app.pivot_edit(si, pi, |p| p.form = form);
                        ui.close();
                    }
                }
                ui.separator();
                if ui.add_enabled(pt.form != Form::Compact, egui::Button::selectable(pt.repeat_labels, "Repeat All Item Labels")).clicked() {
                    app.pivot_edit(si, pi, |p| p.repeat_labels = true);
                    ui.close();
                }
                if ui.add_enabled(pt.form != Form::Compact, egui::Button::selectable(!pt.repeat_labels, "Do Not Repeat Item Labels")).clicked() {
                    app.pivot_edit(si, pi, |p| p.repeat_labels = false);
                    ui.close();
                }
            });
            let r = big(ui, "☰", "Blank\nRows", "Add space between groups", false);
            egui::Popup::menu(&r).show(|ui| {
                if ui.add(egui::Button::selectable(pt.blank_rows, "Insert Blank Line after Each Item")).clicked() {
                    app.pivot_edit(si, pi, |p| p.blank_rows = true);
                    ui.close();
                }
                if ui.add(egui::Button::selectable(!pt.blank_rows, "Remove Blank Line after Each Item")).clicked() {
                    app.pivot_edit(si, pi, |p| p.blank_rows = false);
                    ui.close();
                }
            });
        });
        self.group(ui, "PivotTable Style Options", |app, ui| {
            let mut check = |ui: &mut egui::Ui, label: &str, on: bool, f: fn(&mut PivotTable)| {
                let mut v = on;
                if ui.checkbox(&mut v, RichText::new(label).size(12.0)).changed() {
                    app.pivot_edit(si, pi, f);
                }
            };
            ui.vertical(|ui| {
                check(ui, "Row Headers", pt.row_headers, |p| p.row_headers = !p.row_headers);
                check(ui, "Column Headers", pt.col_headers, |p| p.col_headers = !p.col_headers);
            });
            ui.vertical(|ui| {
                check(ui, "Banded Rows", pt.banded_rows, |p| p.banded_rows = !p.banded_rows);
                check(ui, "Banded Columns", pt.banded_cols, |p| p.banded_cols = !p.banded_cols);
            });
        });
        self.group(ui, "PivotTable Styles", |app, ui| {
            egui::ScrollArea::vertical().id_salt("pivot_styles").max_height(64.0).show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(2.0, 2.0);
                    for row in pivot::style::gallery() {
                        ui.horizontal(|ui| {
                            for name in row {
                                let (rect, resp) = ui.allocate_exact_size(Vec2::new(34.0, 26.0), Sense::click());
                                paint_style_swatch(ui.painter(), rect, &name, pt.style == name || resp.hovered());
                                if resp.on_hover_text(&name).clicked() {
                                    app.pivot_edit(si, pi, |p| p.style = name.clone());
                                }
                            }
                        });
                    }
                });
            });
        });
    }

    // ------------------------------------------------------------------
    // PivotTable Fields pane
    // ------------------------------------------------------------------

    pub(super) fn pivot_field_pane(&mut self, ctx: &egui::Context) {
        if self.backstage.is_some() || self.pivots.hide_list {
            return;
        }
        let Some((si, pi)) = self.active_pivot() else {
            self.pivots.drag = None;
            return;
        };
        if self.pivots.pending.as_ref().is_some_and(|(s, i, _)| (*s, *i) != (si, pi)) {
            self.pivots.pending = None;
        }
        let pt = self.pane_pivot(si, pi);
        let mut zones = DropZones::default();
        let mut close = false;
        egui::SidePanel::right("pivot_field_pane")
            .resizable(true)
            .default_width(300.0)
            .min_width(240.0)
            .frame(egui::Frame::NONE.fill(theme::window()).inner_margin(10.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("PivotTable Fields").size(16.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("✕").on_hover_text("Close").clicked() {
                            close = true;
                        }
                    });
                });
                ui.label(RichText::new("Choose fields to add to report:").size(12.0).color(theme::text_dim()));
                ui.add(egui::TextEdit::singleline(&mut self.pivots.search).hint_text("🔍 Search").desired_width(f32::INFINITY));
                let q = self.pivots.search.to_lowercase();
                let list_h = (ui.available_height() * 0.38).clamp(80.0, 320.0);
                let frame = egui::Frame::NONE.stroke(Stroke::new(1.0, theme::border())).inner_margin(4.0);
                let fr = frame.show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("pivot_field_list").max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
                        for (f, fl) in pt.fields.iter().enumerate() {
                            if !q.is_empty() && !fl.display().to_lowercase().contains(&q) {
                                continue;
                            }
                            ui.horizontal(|ui| {
                                let mut on = pt.is_used(f);
                                if ui.checkbox(&mut on, "").changed() {
                                    self.field_checked(si, pi, &pt, f, on);
                                }
                                let filtered = !fl.hidden.is_empty() || fl.page.is_some();
                                let text = if filtered { format!("{}  ⏷", fl.display()) } else { fl.display().to_string() };
                                let lr = ui.add(egui::Label::new(RichText::new(text).size(13.0)).sense(Sense::click_and_drag()));
                                if lr.drag_started() {
                                    self.pivots.drag = Some(DragSrc { field: f, from: None });
                                }
                                if lr.clicked() {
                                    self.field_checked(si, pi, &pt, f, !pt.is_used(f));
                                }
                                if pt.area_of(f).is_some_and(|a| a != Area::Values) {
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        let r = ui.add(egui::Button::new(RichText::new("▾").size(11.0)).frame(false)).on_hover_text("Filter and sort this field");
                                        if r.clicked() {
                                            let pos = r.rect.left_bottom();
                                            self.open_item_popup(si, pi, f, vec![f], pt.filters.contains(&f), pos);
                                        }
                                    });
                                }
                            });
                        }
                    });
                });
                zones.list = Some(fr.response.rect);
                ui.add_space(6.0);
                ui.label(RichText::new("Drag fields between areas below:").size(12.0).color(theme::text_dim()));
                let area_h = ((ui.available_height() - 60.0) / 2.0 - 26.0).clamp(60.0, 220.0);
                for pair in [[Area::Filters, Area::Columns], [Area::Rows, Area::Values]] {
                    ui.columns(2, |cols| {
                        for (k, a) in pair.into_iter().enumerate() {
                            let ui = &mut cols[k];
                            ui.label(RichText::new(format!("{}  {}", area_icon(a), a.label())).size(12.5));
                            let fr = egui::Frame::NONE.stroke(Stroke::new(1.0, theme::border())).inner_margin(3.0).show(ui, |ui| {
                                ui.set_min_height(area_h);
                                ui.set_width(ui.available_width());
                                let mut rects = Vec::new();
                                let n = if a == Area::Values { pt.values.len() } else { pt.list(a).len() };
                                for idx in 0..n {
                                    rects.push(self.area_entry(ui, si, pi, &pt, a, idx));
                                }
                                rects
                            });
                            zones.areas.push((a, fr.response.rect, fr.inner));
                        }
                    });
                    ui.add_space(4.0);
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let mut defer = self.pivots.defer;
                    if ui.checkbox(&mut defer, "Defer Layout Update").changed() {
                        self.pivots.defer = defer;
                        if !defer {
                            self.apply_pending();
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(self.pivots.defer && self.pivots.pending.is_some(), egui::Button::new("Update")).clicked() {
                            self.apply_pending();
                        }
                    });
                });
            });
        if close {
            self.pivots.hide_list = true;
        }
        self.pane_drag(ctx, si, pi, &pt, &zones);
    }

    /// Checkbox of the field list: text and dates go to Rows, numbers to Values.
    fn field_checked(&mut self, si: usize, pi: usize, pt: &PivotTable, f: usize, on: bool) {
        if !on {
            self.pane_edit(si, pi, |p| p.remove_field(f));
            return;
        }
        let col = pt.fields[f].column();
        let numeric = matches!(pt.fields[f].source, FieldSource::Column(_)) && pt.cache.is_numeric(col) && !pt.cache.is_date(col);
        let area = if numeric { Area::Values } else { Area::Rows };
        self.pane_edit(si, pi, |p| p.add_field(f, area, None));
    }

    /// One entry of an area box; returns its rectangle.
    fn area_entry(&mut self, ui: &mut egui::Ui, si: usize, pi: usize, pt: &PivotTable, a: Area, idx: usize) -> Rect {
        let label = entry_label(pt, a, idx);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::click_and_drag());
        let p = ui.painter();
        let dragging = self.pivots.drag.is_some_and(|d| d.from == Some((a, idx)));
        p.rect_filled(rect.shrink(1.0), 3.0, if resp.hovered() || dragging { theme::hover() } else { theme::ribbon() });
        p.rect_stroke(rect.shrink(1.0), 3.0, Stroke::new(1.0, theme::border()), StrokeKind::Inside);
        let text_rect = Rect::from_min_max(rect.min, Pos2::new(rect.right() - 20.0, rect.bottom()));
        p.with_clip_rect(text_rect).text(Pos2::new(rect.left() + 6.0, rect.center().y), Align2::LEFT_CENTER, &label, theme::ui_font(12.5), theme::text());
        let arrow = Rect::from_min_max(Pos2::new(rect.right() - 20.0, rect.top()), rect.right_bottom());
        super::ribbon::paint_chevron(p, arrow.center(), false, theme::text_dim());
        if resp.drag_started() {
            self.pivots.drag = Some(DragSrc { field: entry_field(pt, a, idx), from: Some((a, idx)) });
        }
        let menu_resp = ui.interact(arrow, ui.id().with(("pivot_entry_arrow", a as u8, idx)), Sense::click());
        let n = if a == Area::Values { pt.values.len() } else { pt.list(a).len() };
        let f = entry_field(pt, a, idx);
        let is_values_field = a != Area::Values && f == VALUES;
        egui::Popup::menu(&menu_resp).show(|ui| {
            ui.set_min_width(200.0);
            let mut mv = |ui: &mut egui::Ui, text: &str, enabled: bool, to: Area, pos: usize| {
                if ui.add_enabled(enabled, egui::Button::new(text)).clicked() {
                    self.pane_edit(si, pi, move |p| p.move_entry(a, idx, to, pos));
                    ui.close();
                }
            };
            mv(ui, "Move Up", idx > 0, a, idx.saturating_sub(1));
            mv(ui, "Move Down", idx + 1 < n, a, idx + 2);
            mv(ui, "Move to Beginning", idx > 0, a, 0);
            mv(ui, "Move to End", idx + 1 < n, a, n);
            ui.separator();
            mv(ui, "Move to Report Filter", a != Area::Filters && !is_values_field, Area::Filters, pt.filters.len());
            mv(ui, "Move to Row Labels", a != Area::Rows, Area::Rows, pt.rows.len());
            mv(ui, "Move to Column Labels", a != Area::Columns, Area::Columns, pt.cols.len());
            mv(ui, "Move to Values", a != Area::Values && !is_values_field, Area::Values, pt.values.len());
            ui.separator();
            if ui.add_enabled(!is_values_field, egui::Button::new("✕ Remove Field")).clicked() {
                self.pane_edit(si, pi, move |p| p.remove_at(a, idx));
                ui.close();
            }
            ui.separator();
            if a == Area::Values {
                if ui.button("Value Field Settings...").clicked() {
                    self.open_value_settings(si, pi, idx);
                    ui.close();
                }
            } else if ui.add_enabled(!is_values_field, egui::Button::new("Field Settings...")).clicked() {
                self.open_field_dialog(si, pi, f);
                ui.close();
            }
        });
        if resp.double_clicked() {
            if a == Area::Values {
                self.open_value_settings(si, pi, idx);
            } else if !is_values_field {
                self.open_field_dialog(si, pi, f);
            }
        }
        rect
    }

    /// Dragging fields between the list and the areas.
    fn pane_drag(&mut self, ctx: &egui::Context, si: usize, pi: usize, pt: &PivotTable, zones: &DropZones) {
        let Some(src) = self.pivots.drag else { return };
        let (pos, released, down) = ctx.input(|i| (i.pointer.latest_pos(), i.pointer.any_released(), i.pointer.any_down()));
        let Some(pos) = pos else { return };
        let target = zones.areas.iter().find(|(_, r, _)| r.contains(pos));
        let insert = target.map(|(a, _, items)| (*a, items.iter().filter(|r| r.center().y < pos.y).count()));
        // The dragged entry, and where it would land.
        let label = match src.from {
            Some((a, i)) => entry_label(pt, a, i),
            None => pt.fields.get(src.field).map(|f| f.display().to_string()).unwrap_or_default(),
        };
        egui::Area::new(egui::Id::new("pivot_drag_ghost")).order(egui::Order::Tooltip).fixed_pos(pos + Vec2::new(10.0, 6.0)).interactable(false).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                let allowed = !(src.field == VALUES && matches!(insert, Some((Area::Filters | Area::Values, _))));
                let mark = if insert.is_none() && src.from.is_some() { "✕ " } else if !allowed { "⊘ " } else { "" };
                ui.label(RichText::new(format!("{mark}{label}")).size(12.5));
            });
        });
        if let Some((a, k)) = insert {
            if let Some((_, r, items)) = zones.areas.iter().find(|(x, _, _)| *x == a) {
                let y = if k < items.len() { items[k].top() } else { items.last().map_or(r.top() + 3.0, |x| x.bottom()) };
                ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("pivot_drop_line")))
                    .line_segment([Pos2::new(r.left() + 3.0, y), Pos2::new(r.right() - 3.0, y)], Stroke::new(2.0, theme::accent()));
            }
        }
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        if released || !down {
            self.pivots.drag = None;
            match (insert, src.from) {
                (Some((to, k)), Some((from, i))) => {
                    if !(src.field == VALUES && matches!(to, Area::Filters | Area::Values)) && !(from == to && (k == i || k == i + 1)) {
                        self.pane_edit(si, pi, move |p| p.move_entry(from, i, to, k));
                    }
                }
                (Some((to, k)), None) => {
                    let f = src.field;
                    self.pane_edit(si, pi, move |p| p.add_field(f, to, Some(k)));
                }
                (None, Some((from, i))) => {
                    // Dropped outside the areas: the field leaves the report, like Excel.
                    if src.field != VALUES {
                        self.pane_edit(si, pi, move |p| p.remove_at(from, i));
                    }
                }
                (None, None) => {}
            }
        }
    }
}
