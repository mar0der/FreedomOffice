//! Sparklines: Insert > Sparklines, painting in cells and the contextual Sparkline tab.

use eframe::egui::{self, Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use super::dialogs::Dialog;
use super::grid::Layout;
use super::chart_view::big_button;
use super::ribbon::color_grid;
use super::theme::{self, rgb};
use super::whatif_ui::{buttons, ref_box, window, Seen, WhatIfDlg};
use super::{App, RibbonTab, Scope};
use crate::model::Range;
use crate::sparkline::{self as sp, AxisScale, EmptyAs, SparkColors, SparkKind, SparklineGroup};

/// Create Sparklines (also Edit Group Location & Data / Edit Sparkline Data).
pub struct CreateDlg {
    sheet: usize,
    kind: SparkKind,
    data: String,
    pub(super) loc: String,
    /// Editing: the group, and the single sparkline when only its data is edited.
    edit: Option<(usize, Option<usize>)>,
    focus: usize,
    seen: Seen,
    error: Option<String>,
}

/// Sparkline Vertical Axis Settings (custom minimum or maximum).
pub struct AxisDlg {
    max: bool,
    text: String,
}

/// Hidden and Empty Cell Settings.
pub struct HiddenDlg {
    empty: EmptyAs,
    hidden: bool,
}

#[derive(Default)]
pub struct SparkUi {
    pub prev_tab: Option<RibbonTab>,
}

/// Paints one sparkline into `r`.
fn paint_one(p: &egui::Painter, r: Rect, g: &SparklineGroup, vals: &[Option<f64>], range: (f64, f64), zoom: f32) {
    let n = vals.len();
    if n == 0 || r.width() < 4.0 || r.height() < 4.0 {
        return;
    }
    let k = &g.colors;
    let nums: Vec<f64> = vals.iter().flatten().copied().collect();
    if nums.is_empty() {
        return;
    }
    let hi_v = nums.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let lo_v = nums.iter().copied().fold(f64::INFINITY, f64::min);
    let first = vals.iter().position(|v| v.is_some());
    let last = vals.iter().rposition(|v| v.is_some());
    // Special points override the series colour (Excel's precedence: last, first, low, high, negative).
    let point_color = |i: usize, v: f64| -> Option<u32> {
        if g.last && Some(i) == last {
            Some(k.last)
        } else if g.first && Some(i) == first {
            Some(k.first)
        } else if g.low && v == lo_v {
            Some(k.low)
        } else if g.high && v == hi_v {
            Some(k.high)
        } else if g.negative && v < 0.0 {
            Some(k.negative)
        } else {
            None
        }
    };
    let (mut lo, mut hi) = range;
    match g.kind {
        SparkKind::Line => {
            if hi <= lo {
                lo -= 1.0;
                hi += 1.0;
            }
            let x = |i: usize| if n == 1 { r.center().x } else { r.left() + r.width() * i as f32 / (n - 1) as f32 };
            let y = |v: f64| r.bottom() - ((v - lo) / (hi - lo)).clamp(-0.05, 1.05) as f32 * r.height();
            if g.axis && lo < 0.0 && hi > 0.0 {
                p.line_segment([Pos2::new(r.left(), y(0.0)), Pos2::new(r.right(), y(0.0))], Stroke::new(1.0, rgb(k.axis)));
            }
            let w = (g.line_weight as f32 * 96.0 / 72.0 * zoom).max(1.0);
            let mut run: Vec<Pos2> = Vec::new();
            for (i, v) in vals.iter().enumerate() {
                match v {
                    Some(v) => run.push(Pos2::new(x(i), y(*v))),
                    None if g.empty_as == EmptyAs::Span => {}
                    None => {
                        if run.len() > 1 {
                            p.line(std::mem::take(&mut run), Stroke::new(w, rgb(k.series)));
                        }
                        run.clear();
                    }
                }
            }
            if run.len() > 1 {
                p.line(run, Stroke::new(w, rgb(k.series)));
            }
            let rad = (w * 1.3).max(1.8);
            for (i, v) in vals.iter().enumerate() {
                let Some(v) = v else { continue };
                let c = point_color(i, *v).or(g.markers.then_some(k.markers));
                if let Some(c) = c {
                    p.rect_filled(Rect::from_center_size(Pos2::new(x(i), y(*v)), Vec2::splat(rad * 2.0)), 0.0, rgb(c));
                }
            }
        }
        SparkKind::Column | SparkKind::WinLoss => {
            let win = g.kind == SparkKind::WinLoss;
            if win {
                (lo, hi) = (-1.0, 1.0);
            } else {
                lo = lo.min(0.0);
                hi = hi.max(0.0);
                if hi <= lo {
                    hi = lo + 1.0;
                }
            }
            let y = |v: f64| r.bottom() - ((v - lo) / (hi - lo)).clamp(0.0, 1.0) as f32 * r.height();
            let slot = r.width() / n as f32;
            let bw = (slot * 0.75).max(1.0);
            for (i, v) in vals.iter().enumerate() {
                let Some(v) = v else { continue };
                let v = if win { v.signum() * (*v != 0.0) as u8 as f64 } else { *v };
                if v == 0.0 && win {
                    continue;
                }
                let (a, b) = if win { (y(0.0), if v > 0.0 { y(0.9) } else { y(-0.9) }) } else { (y(0.0), y(v)) };
                let cx = r.left() + slot * (i as f32 + 0.5);
                let mut bar = Rect::from_min_max(Pos2::new(cx - bw / 2.0, a.min(b)), Pos2::new(cx + bw / 2.0, a.max(b)));
                if bar.height() < 1.0 {
                    bar.max.y = bar.min.y + 1.0;
                }
                let c = point_color(i, if win { v } else { v }).unwrap_or(k.series);
                p.rect_filled(bar, 0.0, rgb(c));
            }
            if g.axis && (win || (lo < 0.0 && hi > 0.0)) {
                p.line_segment([Pos2::new(r.left(), y(0.0)), Pos2::new(r.right(), y(0.0))], Stroke::new(1.0, rgb(k.axis)));
            }
        }
    }
}

/// Ribbon icon for a sparkline type.
fn paint_kind(p: &egui::Painter, r: Rect, kind: SparkKind) {
    let g = SparklineGroup::new(kind, Vec::new());
    let vals = [Some(2.0), Some(-1.0), Some(3.0), Some(1.5), Some(-2.0), Some(4.0)];
    let inner = r.shrink2(Vec2::new(r.width() * 0.15, r.height() * 0.25));
    p.rect_stroke(r.shrink(2.0), 2.0, Stroke::new(1.0, theme::icon_line()), egui::StrokeKind::Inside);
    paint_one(p, inner, &SparklineGroup { negative: true, ..g }, &vals, (-2.0, 4.0), 1.2);
}

impl App {
    /// Draws the sparklines of the visible cells (called from the grid painter).
    pub(super) fn paint_sparklines(&self, p: &egui::Painter, lay: &Layout) {
        let sh = self.sheet();
        if sh.sparklines.is_empty() {
            return;
        }
        let z = self.zoom;
        let active = sp::at(sh, self.active().0, self.active().1).map(|(g, _)| g);
        for (gi, g) in sh.sparklines.iter().enumerate() {
            for it in &g.items {
                let (Some(&(_, y, h)), Some(&(_, x, w))) = (lay.rows.iter().find(|r| r.0 == it.loc.0), lay.cols.iter().find(|c| c.0 == it.loc.1)) else { continue };
                let cell = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
                let vals = sp::values(&self.wb, g, it);
                let ext = sp::extent(&self.wb, g, &vals);
                let inner = cell.shrink2(Vec2::new(2.0 * z.max(0.5) + 1.0, 2.0 * z.max(0.5) + 1.0));
                paint_one(&p.with_clip_rect(cell.intersect(lay.cells)), inner, g, &vals, ext, z);
                if active == Some(gi) {
                    p.rect_stroke(cell.shrink(1.0), 0.0, Stroke::new(1.0, Color32::from_rgb(0x5B, 0x9B, 0xD5)), egui::StrokeKind::Inside);
                }
            }
        }
    }

    /// Insert > Sparklines group.
    pub(super) fn sparkline_insert_buttons(&mut self, ui: &mut egui::Ui) {
        for (kind, tip) in [
            (SparkKind::Line, "Insert a line chart within a single cell."),
            (SparkKind::Column, "Insert a column chart within a single cell."),
            (SparkKind::WinLoss, "Insert a win/loss chart within a single cell."),
        ] {
            if big_button(ui, kind.label(), &format!("{} Sparkline\n{tip}", kind.label()), |p, r| paint_kind(p, r, kind)).clicked() {
                self.open_create_sparklines(kind);
            }
        }
    }

    pub fn open_create_sparklines(&mut self, kind: SparkKind) {
        self.commit_edit(false);
        let sel = self.sel();
        let (data, loc, focus) = if sel.is_single() { (String::new(), crate::solver::abs(sel), 0) } else { (crate::solver::abs(self.sel_clipped()), String::new(), 1) };
        self.dialog = Some(Dialog::WhatIf(Box::new(WhatIfDlg::Sparklines(CreateDlg {
            sheet: self.wb.active,
            kind,
            data,
            loc,
            edit: None,
            focus,
            seen: self.seen(),
            error: None,
        }))));
    }

    pub(super) fn create_sparklines_ui(&mut self, ctx: &egui::Context, d: &mut CreateDlg) -> bool {
        let field = if d.focus == 0 { &mut d.data } else { &mut d.loc };
        // Data ranges keep their sheet name when picked elsewhere; the location is always here.
        self.pick_ref(&mut d.seen, d.sheet, Some(field));
        let mut result = None;
        let title = match d.edit {
            None => "Create Sparklines",
            Some((_, None)) => "Edit Sparklines",
            Some((_, Some(_))) => "Edit Sparkline Data",
        };
        window(title).show(ctx, |ui| {
            ui.set_min_width(340.0);
            ui.label(RichText::new("Choose the data that you want").strong());
            ui.horizontal(|ui| {
                ui.label("Data Range:");
                ref_box(ui, &mut d.data, 0, &mut d.focus, 220.0);
            });
            if !matches!(d.edit, Some((_, Some(_)))) {
                ui.add_space(6.0);
                ui.label(RichText::new("Choose where you want the sparklines to be placed").strong());
                ui.horizontal(|ui| {
                    ui.label("Location Range:");
                    ref_box(ui, &mut d.loc, 1, &mut d.focus, 200.0);
                });
            }
            if let Some(e) = &d.error {
                ui.colored_label(theme::error_text(), e);
            }
            result = buttons(ui, &["OK", "Cancel"]);
            let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
            if enter {
                result = Some(0);
            } else if esc {
                result = Some(1);
            }
        });
        match result {
            Some(0) => match self.apply_sparklines(d) {
                Ok(()) => false,
                Err(e) => {
                    d.error = Some(e);
                    true
                }
            },
            Some(_) => false,
            None => true,
        }
    }

    fn apply_sparklines(&mut self, d: &CreateDlg) -> Result<(), String> {
        let si = d.sheet;
        let (ds, data) = crate::whatif::parse_range_ref(&self.wb, &d.data).ok_or("Data Range is not valid.")?;
        let data_sheet = self.wb.sheets[ds].name.clone();
        if let Some((g, Some(i))) = d.edit {
            if data.rows() > 1 && data.cols() > 1 {
                return Err("Data Range is not valid. The range must be a single row or column.".into());
            }
            self.push_undo(&[Scope::Sheet(si)]);
            if let Some(it) = self.wb.sheets[si].sparklines.get_mut(g).and_then(|g| g.items.get_mut(i)) {
                it.data = sp::qualified(&data_sheet, data);
            }
            self.dirty = true;
            return Ok(());
        }
        let loc = match crate::whatif::parse_range_ref(&self.wb, &d.loc) {
            Some((s, rg)) if s == si => rg,
            _ => return Err("Location reference is not valid.".into()),
        };
        let items = sp::layout(&data_sheet, data, loc)?;
        self.push_undo(&[Scope::Sheet(si)]);
        let sh = &mut self.wb.sheets[si];
        // A new group replaces the sparklines in its location cells.
        let template = d.edit.and_then(|(g, _)| sh.sparklines.get(g).cloned());
        if let Some((g, _)) = d.edit {
            if g < sh.sparklines.len() {
                sh.sparklines.remove(g);
            }
        }
        for grp in sh.sparklines.iter_mut() {
            grp.items.retain(|it| !items.iter().any(|n| n.loc == it.loc));
        }
        sh.sparklines.retain(|g| !g.items.is_empty());
        let group = match template {
            Some(mut t) => {
                t.items = items;
                t
            }
            None => SparklineGroup::new(d.kind, items),
        };
        sh.sparklines.push(group);
        self.dirty = true;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Contextual tab
    // ------------------------------------------------------------------

    fn active_sparkline(&self) -> Option<(usize, usize)> {
        let (r, c) = self.active();
        sp::at(self.sheet(), r, c)
    }

    pub(super) fn sparkline_tab_button(&mut self, ui: &mut egui::Ui) {
        if self.active_sparkline().is_none() || self.selected_chart().is_some() {
            if self.tab == RibbonTab::Sparkline {
                self.tab = self.sparks.prev_tab.take().unwrap_or(RibbonTab::Home);
            }
            return;
        }
        let sel = self.tab == RibbonTab::Sparkline && !self.ribbon_collapsed;
        let font = if sel { theme::ui_bold(13.0) } else { theme::ui_font(13.0) };
        let name = "Sparkline";
        let g = ui.painter().layout_no_wrap(name.to_string(), font.clone(), theme::accent());
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(g.size().x + 20.0, 28.0), Sense::click());
        if resp.hovered() && !sel {
            ui.painter().rect_filled(rect.shrink(2.0), 3.0, theme::hover());
        }
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, name, font, theme::accent());
        if sel {
            ui.painter().line_segment([Pos2::new(rect.left() + 8.0, rect.bottom() - 2.0), Pos2::new(rect.right() - 8.0, rect.bottom() - 2.0)], Stroke::new(3.0, theme::accent()));
        }
        if resp.clicked() {
            if self.tab != RibbonTab::Sparkline {
                self.sparks.prev_tab = Some(self.tab);
            }
            self.tab = RibbonTab::Sparkline;
            self.ribbon_collapsed = false;
        }
    }

    /// Applies a change to the active sparkline's group.
    fn edit_group(&mut self, f: impl FnOnce(&mut SparklineGroup)) {
        let Some((g, _)) = self.active_sparkline() else { return };
        let si = self.wb.active;
        let mut new = self.wb.sheets[si].sparklines[g].clone();
        f(&mut new);
        if new != self.wb.sheets[si].sparklines[g] {
            self.push_undo(&[Scope::Sheet(si)]);
            self.wb.sheets[si].sparklines[g] = new;
            self.dirty = true;
        }
    }

    /// Groups that have a sparkline in the selection.
    fn selected_groups(&self) -> Vec<usize> {
        let sel = self.sel();
        self.sheet().sparklines.iter().enumerate().filter(|(_, g)| g.items.iter().any(|it| sel.contains(it.loc.0, it.loc.1))).map(|(i, _)| i).collect()
    }

    pub(super) fn sparkline_tab(&mut self, ui: &mut egui::Ui) {
        let Some((gi, ii)) = self.active_sparkline() else { return };
        let g = self.sheet().sparklines[gi].clone();
        self.group(ui, "Sparkline", |app, ui| {
            let r = big_button(ui, "Edit\nData", "Edit Data\nEdit the location and data source of the sparklines.", |p, r| {
                paint_kind(p, r, SparkKind::Line);
                p.line_segment([r.right_bottom() + Vec2::new(-3.0, -1.0), r.right_bottom() + Vec2::new(-9.0, -7.0)], Stroke::new(2.0, theme::accent()));
            });
            egui::Popup::menu(&r).show(|ui| {
                ui.set_min_width(230.0);
                if ui.button("Edit Group Location & Data...").clicked() {
                    app.open_edit_sparklines(gi, None);
                    ui.close();
                }
                if ui.button("Edit Single Sparkline's Data...").clicked() {
                    app.open_edit_sparklines(gi, Some(ii));
                    ui.close();
                }
                ui.separator();
                if ui.button("Hidden & Empty Cells...").clicked() {
                    app.dialog = Some(Dialog::WhatIf(Box::new(WhatIfDlg::SparkHidden(HiddenDlg { empty: g.empty_as, hidden: g.hidden }))));
                    ui.close();
                }
            });
        });
        self.group(ui, "Type", |app, ui| {
            for kind in [SparkKind::Line, SparkKind::Column, SparkKind::WinLoss] {
                let r = big_button(ui, kind.label(), &format!("Change the sparkline group to a {} sparkline.", kind.label()), |p, r| paint_kind(p, r, kind));
                if g.kind == kind {
                    ui.painter().rect_stroke(r.rect, 3.0, Stroke::new(1.0, theme::accent()), egui::StrokeKind::Inside);
                }
                if r.clicked() {
                    app.edit_group(|x| x.kind = kind);
                }
            }
        });
        self.group(ui, "Show", |app, ui| {
            let mut v = [g.high, g.low, g.negative, g.first, g.last, g.markers];
            let labels = ["High Point", "Low Point", "Negative Points", "First Point", "Last Point", "Markers"];
            let mut changed = false;
            for col in 0..2 {
                ui.vertical(|ui| {
                    for k in col * 3..col * 3 + 3 {
                        let enabled = k != 5 || g.kind == SparkKind::Line;
                        changed |= ui.add_enabled(enabled, egui::Checkbox::new(&mut v[k], labels[k])).changed();
                    }
                });
            }
            if changed {
                app.edit_group(|x| [x.high, x.low, x.negative, x.first, x.last, x.markers] = v);
            }
        });
        self.group(ui, "Style", |app, ui| {
            egui::Grid::new("spark_styles").spacing(Vec2::new(2.0, 2.0)).show(ui, |ui| {
                for variant in 0..2 {
                    for accent in 0..6 {
                        let st = SparkColors::styled(accent, variant * 2);
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(34.0, 24.0), Sense::click());
                        let p = ui.painter();
                        p.rect_filled(r, 2.0, Color32::WHITE);
                        p.rect_stroke(r, 2.0, Stroke::new(if resp.hovered() { 2.0 } else { 1.0 }, if resp.hovered() { theme::accent() } else { theme::gallery_edge() }), egui::StrokeKind::Inside);
                        let pts: Vec<Pos2> = [0.6f32, 0.3, 0.5, 0.15, 0.4].iter().enumerate().map(|(i, y)| Pos2::new(r.left() + 4.0 + i as f32 * 6.5, r.top() + 4.0 + y * (r.height() - 8.0))).collect();
                        p.line(pts.clone(), Stroke::new(1.5, rgb(st.series)));
                        p.circle_filled(pts[3], 2.0, rgb(st.high));
                        if resp.on_hover_text(format!("Sparkline Style Accent {}", accent + 1)).clicked() {
                            app.edit_group(|x| x.colors = st);
                        }
                    }
                    ui.end_row();
                }
            });
            ui.vertical(|ui| {
                ui.menu_button("Sparkline Color", |ui| {
                    if let Some(Some(c)) = color_grid(ui, false) {
                        app.edit_group(|x| x.colors.series = c);
                        ui.close();
                    }
                    ui.separator();
                    ui.label("Weight");
                    for w in [0.25, 0.5, 0.75, 1.0, 1.5, 2.25, 3.0, 4.5, 6.0] {
                        if ui.selectable_label((g.line_weight - w).abs() < 1e-9, format!("{w} pt")).clicked() {
                            app.edit_group(|x| x.line_weight = w);
                            ui.close();
                        }
                    }
                });
                ui.menu_button("Marker Color", |ui| {
                    type Pick = fn(&mut SparkColors) -> &mut u32;
                    let items: [(&str, Pick); 6] = [
                        ("Negative Points", |k| &mut k.negative),
                        ("Markers", |k| &mut k.markers),
                        ("High Point", |k| &mut k.high),
                        ("Low Point", |k| &mut k.low),
                        ("First Point", |k| &mut k.first),
                        ("Last Point", |k| &mut k.last),
                    ];
                    for (label, pick) in items {
                        ui.menu_button(label, |ui| {
                            if let Some(Some(c)) = color_grid(ui, false) {
                                app.edit_group(|x| *pick(&mut x.colors) = c);
                                ui.close();
                            }
                        });
                    }
                });
            });
        });
        self.group(ui, "Group", |app, ui| {
            let r = big_button(ui, "Axis", "Axis\nChange the scaling and visibility of the sparkline axes.", |p, r| {
                let st = Stroke::new(1.5, theme::text());
                p.line_segment([r.left_bottom() + Vec2::new(3.0, -3.0), r.right_bottom() + Vec2::new(-3.0, -3.0)], st);
                p.line_segment([r.left_bottom() + Vec2::new(3.0, -3.0), r.left_top() + Vec2::new(3.0, 3.0)], st);
            });
            egui::Popup::menu(&r).show(|ui| {
                ui.set_min_width(260.0);
                ui.label(RichText::new("Horizontal Axis Options").strong());
                if ui.selectable_label(g.date_range.is_none(), "General Axis Type").clicked() {
                    app.edit_group(|x| x.date_range = None);
                    ui.close();
                }
                if ui.selectable_label(g.axis, "Show Axis").clicked() {
                    app.edit_group(|x| x.axis = !x.axis);
                    ui.close();
                }
                if ui.selectable_label(g.rtl, "Plot Data Right-to-Left").clicked() {
                    app.edit_group(|x| x.rtl = !x.rtl);
                    ui.close();
                }
                for (title, max) in [("Vertical Axis Minimum Value Options", false), ("Vertical Axis Maximum Value Options", true)] {
                    ui.separator();
                    ui.label(RichText::new(title).strong());
                    let cur = if max { g.max } else { g.min };
                    let set = move |x: &mut SparklineGroup, v: AxisScale| if max { x.max = v } else { x.min = v };
                    if ui.selectable_label(cur == AxisScale::Individual, "Automatic for Each Sparkline").clicked() {
                        app.edit_group(|x| set(x, AxisScale::Individual));
                        ui.close();
                    }
                    if ui.selectable_label(cur == AxisScale::Group, "Same for All Sparklines").clicked() {
                        app.edit_group(|x| set(x, AxisScale::Group));
                        ui.close();
                    }
                    if ui.selectable_label(matches!(cur, AxisScale::Custom(_)), "Custom Value...").clicked() {
                        let text = match cur {
                            AxisScale::Custom(v) => crate::numfmt::general(v),
                            _ => "0.0".into(),
                        };
                        app.dialog = Some(Dialog::WhatIf(Box::new(WhatIfDlg::SparkAxis(AxisDlg { max, text }))));
                        ui.close();
                    }
                }
            });
            ui.vertical(|ui| {
                if ui.button("⊞ Group").on_hover_text("Group the selected sparklines so they share formatting.").clicked() {
                    app.group_sparklines();
                }
                if ui.button("⊟ Ungroup").on_hover_text("Ungroup the selected sparklines.").clicked() {
                    app.ungroup_sparklines();
                }
                ui.menu_button("Clear", |ui| {
                    if ui.button("Clear Selected Sparklines").clicked() {
                        app.clear_sparklines(false);
                        ui.close();
                    }
                    if ui.button("Clear Selected Sparkline Groups").clicked() {
                        app.clear_sparklines(true);
                        ui.close();
                    }
                });
            });
        });
    }

    fn open_edit_sparklines(&mut self, g: usize, single: Option<usize>) {
        let sh = self.sheet();
        let Some(grp) = sh.sparklines.get(g) else { return };
        let data = match single {
            Some(i) => grp.items.get(i).map(|it| it.data.clone()).unwrap_or_default(),
            None => {
                // The group's data as one range when its rows/columns are contiguous.
                let rs: Vec<(String, Range)> = grp.items.iter().filter_map(|it| sp::split_ref(&it.data)).collect();
                match rs.first() {
                    Some((s, first)) => {
                        let u = rs.iter().fold(*first, |a, (_, r)| a.union(r));
                        sp::qualified(s, u)
                    }
                    None => String::new(),
                }
            }
        };
        let locs = grp.items.iter().fold(None::<Range>, |a, it| {
            let r = Range::cell(it.loc.0, it.loc.1);
            Some(a.map_or(r, |a| a.union(&r)))
        });
        self.dialog = Some(Dialog::WhatIf(Box::new(WhatIfDlg::Sparklines(CreateDlg {
            sheet: self.wb.active,
            kind: grp.kind,
            data,
            loc: locs.map(crate::solver::abs).unwrap_or_default(),
            edit: Some((g, single)),
            focus: 0,
            seen: self.seen(),
            error: None,
        }))));
    }

    fn group_sparklines(&mut self) {
        let Some((active, _)) = self.active_sparkline() else { return };
        let groups = self.selected_groups();
        if groups.len() < 2 {
            return;
        }
        let si = self.wb.active;
        self.push_undo(&[Scope::Sheet(si)]);
        let sh = &mut self.wb.sheets[si];
        let mut merged = sh.sparklines[active].clone();
        merged.items = groups.iter().flat_map(|g| sh.sparklines[*g].items.clone()).collect();
        let mut k = 0;
        sh.sparklines.retain(|_| {
            k += 1;
            !groups.contains(&(k - 1))
        });
        sh.sparklines.push(merged);
        self.dirty = true;
    }

    fn ungroup_sparklines(&mut self) {
        let groups = self.selected_groups();
        if groups.is_empty() {
            return;
        }
        let si = self.wb.active;
        self.push_undo(&[Scope::Sheet(si)]);
        let sh = &mut self.wb.sheets[si];
        let mut out = Vec::new();
        for (i, g) in sh.sparklines.drain(..).enumerate() {
            if groups.contains(&i) {
                for it in g.items.clone() {
                    out.push(SparklineGroup { items: vec![it], ..g.clone() });
                }
            } else {
                out.push(g);
            }
        }
        sh.sparklines = out;
        self.dirty = true;
    }

    fn clear_sparklines(&mut self, whole_groups: bool) {
        let groups = self.selected_groups();
        if groups.is_empty() {
            return;
        }
        let si = self.wb.active;
        let sel = self.sel();
        self.push_undo(&[Scope::Sheet(si)]);
        let sh = &mut self.wb.sheets[si];
        for (i, g) in sh.sparklines.iter_mut().enumerate() {
            if whole_groups && groups.contains(&i) {
                g.items.clear();
            } else {
                g.items.retain(|it| !sel.contains(it.loc.0, it.loc.1));
            }
        }
        sh.sparklines.retain(|g| !g.items.is_empty());
        self.dirty = true;
    }

    pub(super) fn spark_axis_ui(&mut self, ctx: &egui::Context, d: &mut AxisDlg) -> bool {
        let mut result = None;
        window("Sparkline Vertical Axis Settings").show(ctx, |ui| {
            ui.label(format!("Enter the {} value for the vertical axis:", if d.max { "maximum" } else { "minimum" }));
            ui.add(egui::TextEdit::singleline(&mut d.text).desired_width(200.0));
            result = buttons(ui, &["OK", "Cancel"]);
            let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
            if enter {
                result = Some(0);
            } else if esc {
                result = Some(1);
            }
        });
        match result {
            Some(0) => {
                let crate::model::Value::Number(v) = crate::numfmt::parse_input(d.text.trim()).0 else { return true };
                let max = d.max;
                self.edit_group(|x| if max { x.max = AxisScale::Custom(v) } else { x.min = AxisScale::Custom(v) });
                false
            }
            Some(_) => false,
            None => true,
        }
    }

    pub(super) fn spark_hidden_ui(&mut self, ctx: &egui::Context, d: &mut HiddenDlg) -> bool {
        let mut result = None;
        window("Hidden and Empty Cell Settings").show(ctx, |ui| {
            ui.label("Show empty cells as:");
            ui.radio_value(&mut d.empty, EmptyAs::Gap, "Gaps");
            ui.radio_value(&mut d.empty, EmptyAs::Zero, "Zero");
            ui.radio_value(&mut d.empty, EmptyAs::Span, "Connect data points with line");
            ui.add_space(4.0);
            ui.checkbox(&mut d.hidden, "Show data in hidden rows and columns");
            result = buttons(ui, &["OK", "Cancel"]);
            let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
            if enter {
                result = Some(0);
            } else if esc {
                result = Some(1);
            }
        });
        match result {
            Some(0) => {
                let (e, h) = (d.empty, d.hidden);
                self.edit_group(|x| {
                    x.empty_as = e;
                    x.hidden = h;
                });
                false
            }
            Some(_) => false,
            None => true,
        }
    }
}
