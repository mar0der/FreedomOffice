//! AutoFilter: filter commands, the column dropdown (sort, colour, Text/Number/Date
//! Filters, search and the value checklist with a date tree) and the Custom AutoFilter
//! and Top 10 dialogs.

use std::collections::{BTreeMap, BTreeSet};

use eframe::egui::{self, Align2, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};

use super::data_ui::{self, DataDlg};
use super::dialogs::Dialog;
use super::theme;
use super::{App, Scope};
use crate::datatools::{SortKey, SortOn, SortOptions, SortOrder};
use crate::filter::{self, AutoFilter, ColumnFilter, ColumnKind, Condition, DateGroup, Dynamic, Probe, UiOp, MONTH_NAMES};
use crate::numfmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sub {
    SortColor,
    FilterColor,
    Filters,
}

struct Item {
    text: String,
    checked: bool,
}

struct Month {
    m: u8,
    open: bool,
    days: Vec<(u8, bool)>,
}

struct Year {
    y: i32,
    open: bool,
    months: Vec<Month>,
}

pub struct FilterPopup {
    pub col: u32,
    pub pos: Pos2,
    kind: ColumnKind,
    header: String,
    search: String,
    items: Vec<Item>,
    blanks: Option<bool>,
    years: Vec<Year>,
    fills: Vec<Option<u32>>,
    fonts: Vec<Option<u32>>,
    current: Option<ColumnFilter>,
    sub: Option<Sub>,
    sub_top: f32,
    period: bool,
    period_top: f32,
    fresh: bool,
}

pub struct CustomFilterDlg {
    col: u32,
    header: String,
    kind: ColumnKind,
    op1: UiOp,
    v1: String,
    and: bool,
    op2: Option<UiOp>,
    v2: String,
    values: Vec<String>,
}

pub struct Top10Dlg {
    col: u32,
    top: bool,
    val: f64,
    percent: bool,
}

const POPUP_W: f32 = 268.0;
const SUB_W: f32 = 230.0;

fn menu_row(ui: &mut egui::Ui, icon: &str, label: &str, enabled: bool, arrow: bool, w: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 22.0), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if enabled && resp.hovered() {
        p.rect_filled(rect, 2.0, theme::hover());
    }
    let col = if enabled { theme::text() } else { theme::text_dim() };
    p.text(Pos2::new(rect.left() + 13.0, rect.center().y), Align2::CENTER_CENTER, icon, theme::ui_font(12.0), col);
    p.text(Pos2::new(rect.left() + 30.0, rect.center().y), Align2::LEFT_CENTER, label, theme::ui_font(12.5), col);
    if arrow {
        p.text(Pos2::new(rect.right() - 10.0, rect.center().y), Align2::CENTER_CENTER, "›", theme::ui_font(15.0), col);
    }
    resp
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(RichText::new(text).strong().size(12.0));
    });
}

/// A colour swatch row (No Fill / Automatic for `None`).
fn swatch_row(ui: &mut egui::Ui, color: Option<u32>, font: bool, w: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 22.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 2.0, theme::hover());
    }
    let sw = Rect::from_min_size(Pos2::new(rect.left() + 30.0, rect.top() + 4.0), Vec2::new(90.0, 14.0));
    match (color, font) {
        (Some(c), false) => {
            p.rect_filled(sw, 0.0, theme::rgb(c));
            p.rect_stroke(sw, 0.0, Stroke::new(1.0, theme::border()), StrokeKind::Inside);
        }
        (Some(c), true) => {
            p.text(sw.left_center() + Vec2::new(4.0, 0.0), Align2::LEFT_CENTER, "Abc", theme::ui_bold(12.5), theme::rgb(c));
        }
        (None, _) => {
            p.text(sw.left_center(), Align2::LEFT_CENTER, if font { "Automatic" } else { "No Fill" }, theme::ui_font(12.5), theme::text());
        }
    }
    resp
}

fn date_label(kind: ColumnKind, asc: bool) -> &'static str {
    match (kind, asc) {
        (ColumnKind::Text, true) => "Sort A to Z",
        (ColumnKind::Text, false) => "Sort Z to A",
        (ColumnKind::Number, true) => "Sort Smallest to Largest",
        (ColumnKind::Number, false) => "Sort Largest to Smallest",
        (ColumnKind::Date, true) => "Sort Oldest to Newest",
        (ColumnKind::Date, false) => "Sort Newest to Oldest",
    }
}

impl FilterPopup {
    fn date_matches(q: &str, y: i32, m: u8, d: u8) -> bool {
        q.is_empty() || format!("{y} {} {d}", MONTH_NAMES[(m - 1) as usize]).to_lowercase().contains(q)
    }

    fn item_matches(q: &str, t: &str) -> bool {
        q.is_empty() || t.to_lowercase().contains(q)
    }

    /// Iterates every leaf: (visible under the search, checked).
    fn leaves(&self) -> Vec<(bool, bool)> {
        let q = self.search.to_lowercase();
        let mut out: Vec<(bool, bool)> = self.items.iter().map(|i| (Self::item_matches(&q, &i.text), i.checked)).collect();
        if let Some(b) = self.blanks {
            out.push((q.is_empty() || "(blanks)".contains(&q), b));
        }
        for y in &self.years {
            for m in &y.months {
                for (d, on) in &m.days {
                    out.push((Self::date_matches(&q, y.y, m.m, *d), *on));
                }
            }
        }
        out
    }

    fn set_all(&mut self, on: bool) {
        let q = self.search.to_lowercase();
        for i in self.items.iter_mut() {
            if Self::item_matches(&q, &i.text) {
                i.checked = on;
            }
        }
        if self.blanks.is_some() && (q.is_empty() || "(blanks)".contains(&q)) {
            self.blanks = Some(on);
        }
        for y in self.years.iter_mut() {
            for m in y.months.iter_mut() {
                for (d, v) in m.days.iter_mut() {
                    if Self::date_matches(&q, y.y, m.m, *d) {
                        *v = on;
                    }
                }
            }
        }
    }

    /// The criteria the checklist describes (`None` = everything shown).
    fn result(&self) -> Option<ColumnFilter> {
        let q = self.search.to_lowercase();
        let leaves = self.leaves();
        if q.is_empty() && leaves.iter().all(|l| l.1) {
            return None;
        }
        let items: BTreeSet<String> =
            self.items.iter().filter(|i| i.checked && Self::item_matches(&q, &i.text)).map(|i| i.text.to_lowercase()).collect();
        let blanks = self.blanks.unwrap_or(false) && (q.is_empty() || "(blanks)".contains(&q));
        let mut dates = BTreeSet::new();
        for y in &self.years {
            let on = |m: &Month, d: u8, v: bool| v && Self::date_matches(&q, y.y, m.m, d);
            let full_month = |m: &Month| m.days.iter().all(|(d, v)| on(m, *d, *v));
            if y.months.iter().all(full_month) {
                dates.insert(DateGroup { year: y.y, month: 0, day: 0 });
                continue;
            }
            for m in &y.months {
                if full_month(m) {
                    dates.insert(DateGroup { year: y.y, month: m.m, day: 0 });
                } else {
                    for (d, v) in &m.days {
                        if on(m, *d, *v) {
                            dates.insert(DateGroup { year: y.y, month: m.m, day: *d });
                        }
                    }
                }
            }
        }
        Some(ColumnFilter::Values { items, blanks, dates })
    }
}

impl App {
    // ------------------------------------------------------------------
    // Commands
    // ------------------------------------------------------------------

    /// Data > Filter (Ctrl+Shift+L): turns the AutoFilter on for the current region, or off.
    pub fn toggle_filter(&mut self) {
        if self.protect_blocks(super::Change::Any) {
            return;
        }
        let si = self.wb.active;
        self.push_undo(&[Scope::Sheet(si)]);
        if let Some(mut f) = self.sheet().filter.clone() {
            // Show the rows the criteria hid; rows hidden by hand stay hidden.
            f.range.r2 = filter::data_end(self.sheet(), f.range);
            let vis = filter::row_visibility(&self.wb, self.sheet(), &f, numfmt::now_serial());
            let sh = self.sheet_mut();
            sh.filter = None;
            for (r, shown) in vis {
                if !shown {
                    sh.hidden_rows.remove(&r);
                }
            }
            self.status = None;
        } else {
            let rg = if self.sel().is_single() { self.current_region() } else { self.sel_clipped() };
            self.sheet_mut().filter = Some(AutoFilter::new(rg));
        }
        self.changed();
    }

    /// Re-evaluates the criteria and hides the rows that fail (no undo step).
    pub fn refilter(&mut self) {
        let si = self.wb.active;
        let Some(mut f) = self.wb.sheets[si].filter.clone() else { return };
        f.range.r2 = filter::data_end(&self.wb.sheets[si], f.range);
        let vis = filter::row_visibility(&self.wb, &self.wb.sheets[si], &f, numfmt::now_serial());
        let total = vis.len();
        let shown = vis.iter().filter(|v| v.1).count();
        let any = !f.columns.is_empty();
        let sh = &mut self.wb.sheets[si];
        for (r, v) in vis {
            if v {
                sh.hidden_rows.remove(&r);
            } else {
                sh.hidden_rows.insert(r);
            }
        }
        sh.filter = Some(f);
        // SUBTOTAL(101..)/AGGREGATE ignore hidden rows, so totals change with the filter.
        self.changed();
        self.status = if any { Some(format!("{shown} of {total} records found")) } else { None };
    }

    /// Data > Reapply.
    pub fn apply_filter(&mut self) {
        let si = self.wb.active;
        if self.wb.sheets[si].filter.is_none() || self.protect_blocks(super::Change::Filter) {
            return;
        }
        self.push_undo(&[Scope::Sheet(si)]);
        self.refilter();
    }

    /// Data > Clear: removes all criteria, keeping the dropdowns.
    pub fn clear_filter(&mut self) {
        let si = self.wb.active;
        if self.wb.sheets[si].filter.is_none() || self.protect_blocks(super::Change::Filter) {
            return;
        }
        self.push_undo(&[Scope::Sheet(si)]);
        if let Some(f) = &mut self.wb.sheets[si].filter {
            f.columns.clear();
        }
        self.refilter();
    }

    /// Sets (or with `None` clears) the criteria of one column and refilters.
    pub fn set_column_filter(&mut self, col: u32, cf: Option<ColumnFilter>) {
        let si = self.wb.active;
        if self.wb.sheets[si].filter.is_none() || self.protect_blocks(super::Change::Filter) {
            return;
        }
        self.push_undo(&[Scope::Sheet(si)]);
        if let Some(f) = &mut self.wb.sheets[si].filter {
            match cf {
                Some(cf) => {
                    f.columns.insert(col, cf);
                }
                None => {
                    f.columns.remove(&col);
                }
            }
        }
        self.refilter();
    }

    /// Turns the filter on (if needed) and returns the active cell's column inside it.
    fn filter_for_active(&mut self) -> Option<(u32, Probe)> {
        if self.sheet().filter.is_none() {
            self.toggle_filter();
        }
        let (r, c) = self.active();
        let f = self.sheet().filter.as_ref()?;
        if c < f.range.c1 || c > f.range.c2 {
            return None;
        }
        Some((c, filter::probe(&self.wb, self.sheet(), r, c)))
    }

    /// Context menu Filter > Filter by Selected Cell's Value.
    pub fn filter_by_cell_value(&mut self) {
        let Some((c, p)) = self.filter_for_active() else { return };
        let cf = if p.is_blank() {
            ColumnFilter::Values { items: BTreeSet::new(), blanks: true, dates: BTreeSet::new() }
        } else {
            ColumnFilter::Values { items: [p.text.to_lowercase()].into(), blanks: false, dates: BTreeSet::new() }
        };
        self.set_column_filter(c, Some(cf));
    }

    /// Context menu Filter > Filter by Selected Cell's Color.
    pub fn filter_by_cell_color(&mut self) {
        let Some((c, p)) = self.filter_for_active() else { return };
        self.set_column_filter(c, Some(ColumnFilter::Color { font: false, color: p.fill }));
    }

    /// Context menu Filter > Filter by Selected Cell's Font Color.
    pub fn filter_by_font_color(&mut self) {
        let Some((c, p)) = self.filter_for_active() else { return };
        self.set_column_filter(c, Some(ColumnFilter::Color { font: true, color: p.font }));
    }

    /// Sorts the filter range (or the current region) putting one colour on top.
    pub fn sort_by_color(&mut self, col: u32, font: bool, color: Option<u32>) {
        let (rg, header) = match self.sheet().filter.as_ref() {
            Some(f) if f.range.c1 <= col && col <= f.range.c2 => {
                let mut rg = f.range;
                rg.r2 = filter::data_end(self.sheet(), rg);
                (rg, true)
            }
            _ => self.sort_area(),
        };
        let key = SortKey { line: col, on: if font { SortOn::FontColor } else { SortOn::CellColor }, order: SortOrder::ColorTop(color) };
        self.sort_range_by(rg, header, &[key], SortOptions::default());
    }

    /// Context menu Sort > Put Selected Cell Color On Top.
    pub fn sort_by_cell_color(&mut self) {
        let (r, c) = self.active();
        let fill = filter::probe(&self.wb, self.sheet(), r, c).fill;
        self.sort_by_color(c, false, fill);
    }

    /// Context menu Sort > Put Selected Font Color On Top.
    pub fn sort_by_font_color(&mut self) {
        let (r, c) = self.active();
        let font = filter::probe(&self.wb, self.sheet(), r, c).font;
        self.sort_by_color(c, true, font);
    }

    /// Cells of a filter column: rows passing the other columns' criteria (for the checklist)
    /// or all data rows.
    fn filter_column_probes(&self, col: u32, others_only: bool) -> Vec<Probe> {
        let Some(f) = self.sheet().filter.clone() else { return Vec::new() };
        let sh = self.sheet();
        let mut f = f;
        f.range.r2 = filter::data_end(sh, f.range);
        let rows: Vec<u32> = if others_only {
            let mut g = f.clone();
            g.columns.remove(&col);
            filter::row_visibility(&self.wb, sh, &g, numfmt::now_serial()).into_iter().filter(|v| v.1).map(|v| v.0).collect()
        } else {
            (f.range.r1 + 1..=f.range.r2).collect()
        };
        rows.into_iter().map(|r| filter::probe(&self.wb, sh, r, col)).collect()
    }

    fn filter_header(&self, col: u32) -> String {
        let Some(f) = self.sheet().filter.as_ref() else { return String::new() };
        let t = numfmt::value_to_text(self.sheet().display_value(f.range.r1, col));
        if t.is_empty() { format!("Column {}", crate::model::col_name(col)) } else { t }
    }

    // ------------------------------------------------------------------
    // Dropdown
    // ------------------------------------------------------------------

    pub fn open_filter_popup(&mut self, col: u32, pos: Pos2) {
        let Some(f) = self.sheet().filter.clone() else { return };
        let current = f.columns.get(&col).cloned();
        let all = self.filter_column_probes(col, false);
        let kind = filter::column_kind(&all);
        let listed = self.filter_column_probes(col, true);
        let today = numfmt::now_serial();
        let passes: Vec<bool> = match &current {
            Some(cf) => filter::evaluate(cf, &listed, today),
            None => vec![true; listed.len()],
        };
        // distinct values: numbers first (numerically), then text, logicals, errors
        let mut seen: BTreeMap<String, (Probe, bool)> = BTreeMap::new();
        let mut blanks: Option<bool> = None;
        let mut dates: BTreeMap<i32, BTreeMap<u8, BTreeMap<u8, bool>>> = BTreeMap::new();
        for (p, ok) in listed.iter().zip(passes) {
            if p.is_blank() {
                blanks = Some(blanks.unwrap_or(false) || ok);
            } else if p.date {
                let (y, m, d) = numfmt::serial_to_ymd(p.value.as_number().unwrap_or(0.0));
                let e = dates.entry(y as i32).or_default().entry(m as u8).or_default().entry(d as u8).or_insert(false);
                *e |= ok;
            } else {
                let e = seen.entry(p.text.to_lowercase()).or_insert_with(|| (p.clone(), false));
                e.1 |= ok;
            }
        }
        let mut vals: Vec<(Probe, bool)> = seen.into_values().collect();
        vals.sort_by(|a, b| crate::datatools::value_cmp(&a.0.value, &b.0.value, false, false));
        let items = vals.into_iter().map(|(p, on)| Item { text: p.text, checked: on }).collect();
        let years = dates
            .into_iter()
            .map(|(y, ms)| Year {
                y,
                open: false,
                months: ms.into_iter().map(|(m, ds)| Month { m, open: false, days: ds.into_iter().collect() }).collect(),
            })
            .collect();
        let mut fills: Vec<Option<u32>> = Vec::new();
        let mut fonts: Vec<Option<u32>> = Vec::new();
        for p in &all {
            if !fills.contains(&p.fill) {
                fills.push(p.fill);
            }
            if !fonts.contains(&p.font) {
                fonts.push(p.font);
            }
        }
        self.filter_popup = Some(FilterPopup {
            col,
            pos,
            kind,
            header: self.filter_header(col),
            search: String::new(),
            items,
            blanks,
            years,
            fills,
            fonts,
            current,
            sub: None,
            sub_top: 0.0,
            period: false,
            period_top: 0.0,
            fresh: true,
        });
    }

    pub(super) fn filter_popup_ui(&mut self, ctx: &egui::Context) {
        let Some(mut fp) = self.filter_popup.take() else { return };
        enum Act {
            Sort(bool),
            SortColor(bool, Option<u32>),
            Set(Option<ColumnFilter>),
            Custom(UiOp, Option<UiOp>),
            Top10,
        }
        let mut act: Option<Act> = None;
        let mut close = false;
        let screen = ctx.content_rect();
        let x = (fp.pos.x - 20.0).min(screen.right() - POPUP_W - 20.0).max(screen.left());
        let origin = Pos2::new(x, fp.pos.y + 10.0);
        let colorful = |v: &Vec<Option<u32>>| v.len() > 1;
        let has_colors = colorful(&fp.fills) || colorful(&fp.fonts);
        let mut rects: Vec<Rect> = Vec::new();
        let mut sub_hover: Option<(Sub, f32)> = None;
        let mut any_row_hover = false;

        let main = egui::Area::new(egui::Id::new("filter_popup")).fixed_pos(origin).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                ui.set_width(POPUP_W);
                ui.spacing_mut().item_spacing.y = 0.0;
                let w = POPUP_W;
                if menu_row(ui, "↑", date_label(fp.kind, true), true, false, w).clicked() {
                    act = Some(Act::Sort(false));
                }
                if menu_row(ui, "↓", date_label(fp.kind, false), true, false, w).clicked() {
                    act = Some(Act::Sort(true));
                }
                let r = menu_row(ui, "", "Sort by Color", has_colors, true, w);
                if r.hovered() && has_colors {
                    sub_hover = Some((Sub::SortColor, r.rect.top()));
                }
                any_row_hover |= r.hovered();
                ui.add_space(3.0);
                ui.separator();
                ui.add_space(3.0);
                let clear = format!("Clear Filter From \"{}\"", fp.header);
                let r = menu_row(ui, "⊘", &clear, fp.current.is_some(), false, w);
                if r.clicked() {
                    act = Some(Act::Set(None));
                }
                any_row_hover |= r.hovered();
                let r = menu_row(ui, "", "Filter by Color", has_colors, true, w);
                if r.hovered() && has_colors {
                    sub_hover = Some((Sub::FilterColor, r.rect.top()));
                }
                any_row_hover |= r.hovered();
                let label = match fp.kind {
                    ColumnKind::Text => "Text Filters",
                    ColumnKind::Number => "Number Filters",
                    ColumnKind::Date => "Date Filters",
                };
                let tick = if matches!(fp.current, Some(ColumnFilter::Custom { .. } | ColumnFilter::Top10 { .. } | ColumnFilter::Dynamic(_))) { "✓" } else { "" };
                let r = menu_row(ui, tick, label, true, true, w);
                if r.hovered() {
                    sub_hover = Some((Sub::Filters, r.rect.top()));
                }
                any_row_hover |= r.hovered();
                ui.add_space(4.0);
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    let r = ui.add(egui::TextEdit::singleline(&mut fp.search).hint_text("Search").desired_width(w - 16.0));
                    if fp.fresh {
                        r.request_focus();
                    }
                });
                // checklist
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    egui::Frame::new().fill(theme::window()).stroke(Stroke::new(1.0, theme::border())).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(200.0).min_scrolled_height(200.0).auto_shrink([false, false]).show(ui, |ui| {
                            ui.set_width(w - 26.0);
                            ui.spacing_mut().item_spacing.y = 1.0;
                            checklist(ui, &mut fp);
                        });
                    });
                });
                ui.add_space(4.0);
                let can_ok = fp.leaves().iter().any(|l| l.0 && l.1);
                ui.horizontal(|ui| {
                    ui.add_space(w - 2.0 * 76.0 - 12.0);
                    if ui.add_enabled(can_ok, egui::Button::new("OK").min_size(Vec2::new(70.0, 24.0)).fill(theme::accent_light())).clicked() {
                        act = Some(Act::Set(fp.result()));
                    }
                    if ui.add(egui::Button::new("Cancel").min_size(Vec2::new(70.0, 24.0))).clicked() {
                        close = true;
                    }
                });
                if can_ok && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    act = Some(Act::Set(fp.result()));
                }
            });
        });
        rects.push(main.response.rect);
        if let Some((s, top)) = sub_hover {
            if fp.sub != Some(s) {
                fp.period = false;
            }
            fp.sub = Some(s);
            fp.sub_top = top;
        } else if any_row_hover {
            fp.sub = None;
        }

        // Submenu
        if let Some(sub) = fp.sub {
            let pos = Pos2::new(main.response.rect.right() - 4.0, fp.sub_top - 4.0);
            let mut period_hover: Option<f32> = None;
            let mut other_hover = false;
            let r = egui::Area::new(egui::Id::new("filter_popup_sub")).fixed_pos(pos).order(egui::Order::Foreground).show(ctx, |ui| {
                egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                    ui.set_width(SUB_W);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    match sub {
                        Sub::SortColor | Sub::FilterColor => {
                            let sort = sub == Sub::SortColor;
                            if colorful(&fp.fills) {
                                heading(ui, if sort { "Sort by Cell Color" } else { "Filter by Cell Color" });
                                for c in fp.fills.clone() {
                                    if swatch_row(ui, c, false, SUB_W).clicked() {
                                        act = Some(if sort { Act::SortColor(false, c) } else { Act::Set(Some(ColumnFilter::Color { font: false, color: c })) });
                                    }
                                }
                            }
                            if colorful(&fp.fonts) {
                                heading(ui, if sort { "Sort by Font Color" } else { "Filter by Font Color" });
                                for c in fp.fonts.clone() {
                                    if swatch_row(ui, c, true, SUB_W).clicked() {
                                        act = Some(if sort { Act::SortColor(true, c) } else { Act::Set(Some(ColumnFilter::Color { font: true, color: c })) });
                                    }
                                }
                            }
                        }
                        Sub::Filters => {
                            let cur_dyn = match fp.current {
                                Some(ColumnFilter::Dynamic(d)) => Some(d),
                                _ => None,
                            };
                            let mut item = |ui: &mut egui::Ui, label: &str, a: Act, tick: bool| {
                                let r = menu_row(ui, if tick { "✓" } else { "" }, label, true, false, SUB_W);
                                other_hover |= r.hovered();
                                if r.clicked() {
                                    act = Some(a);
                                }
                            };
                            let dynamic = |ui: &mut egui::Ui, d: Dynamic, item: &mut dyn FnMut(&mut egui::Ui, &str, Act, bool)| {
                                item(ui, &d.label(), Act::Set(Some(ColumnFilter::Dynamic(d))), cur_dyn == Some(d));
                            };
                            match fp.kind {
                                ColumnKind::Text => {
                                    item(ui, "Equals...", Act::Custom(UiOp::Equals, None), false);
                                    item(ui, "Does Not Equal...", Act::Custom(UiOp::NotEquals, None), false);
                                    ui.separator();
                                    item(ui, "Begins With...", Act::Custom(UiOp::BeginsWith, None), false);
                                    item(ui, "Ends With...", Act::Custom(UiOp::EndsWith, None), false);
                                    ui.separator();
                                    item(ui, "Contains...", Act::Custom(UiOp::Contains, None), false);
                                    item(ui, "Does Not Contain...", Act::Custom(UiOp::NotContains, None), false);
                                }
                                ColumnKind::Number => {
                                    item(ui, "Equals...", Act::Custom(UiOp::Equals, None), false);
                                    item(ui, "Does Not Equal...", Act::Custom(UiOp::NotEquals, None), false);
                                    ui.separator();
                                    item(ui, "Greater Than...", Act::Custom(UiOp::Greater, None), false);
                                    item(ui, "Greater Than Or Equal To...", Act::Custom(UiOp::GreaterEq, None), false);
                                    item(ui, "Less Than...", Act::Custom(UiOp::Less, None), false);
                                    item(ui, "Less Than Or Equal To...", Act::Custom(UiOp::LessEq, None), false);
                                    item(ui, "Between...", Act::Custom(UiOp::GreaterEq, Some(UiOp::LessEq)), false);
                                    ui.separator();
                                    let top = matches!(fp.current, Some(ColumnFilter::Top10 { .. }));
                                    item(ui, "Top 10...", Act::Top10, top);
                                    dynamic(ui, Dynamic::AboveAverage, &mut item);
                                    dynamic(ui, Dynamic::BelowAverage, &mut item);
                                }
                                ColumnKind::Date => {
                                    item(ui, "Equals...", Act::Custom(UiOp::Equals, None), false);
                                    ui.separator();
                                    item(ui, "Before...", Act::Custom(UiOp::Less, None), false);
                                    item(ui, "After...", Act::Custom(UiOp::Greater, None), false);
                                    item(ui, "Between...", Act::Custom(UiOp::GreaterEq, Some(UiOp::LessEq)), false);
                                    let groups: [&[Dynamic]; 5] = [
                                        &[Dynamic::Tomorrow, Dynamic::Today, Dynamic::Yesterday],
                                        &[Dynamic::NextWeek, Dynamic::ThisWeek, Dynamic::LastWeek],
                                        &[Dynamic::NextMonth, Dynamic::ThisMonth, Dynamic::LastMonth],
                                        &[Dynamic::NextQuarter, Dynamic::ThisQuarter, Dynamic::LastQuarter],
                                        &[Dynamic::NextYear, Dynamic::ThisYear, Dynamic::LastYear],
                                    ];
                                    for g in groups {
                                        ui.separator();
                                        for d in g {
                                            dynamic(ui, *d, &mut item);
                                        }
                                    }
                                    ui.separator();
                                    dynamic(ui, Dynamic::YearToDate, &mut item);
                                    let tick = matches!(cur_dyn, Some(Dynamic::Quarter(_) | Dynamic::Month(_)));
                                    let r = menu_row(ui, if tick { "✓" } else { "" }, "All Dates in the Period", true, true, SUB_W);
                                    if r.hovered() {
                                        period_hover = Some(r.rect.top());
                                    }
                                }
                            }
                            ui.separator();
                            let custom = matches!(fp.current, Some(ColumnFilter::Custom { .. }));
                            let r = menu_row(ui, if custom { "✓" } else { "" }, "Custom Filter...", true, false, SUB_W);
                            other_hover |= r.hovered();
                            if r.clicked() {
                                act = Some(Act::Custom(UiOp::Equals, None));
                            }
                        }
                    }
                });
            });
            rects.push(r.response.rect);
            if let Some(top) = period_hover {
                fp.period = true;
                fp.period_top = top;
            } else if other_hover {
                fp.period = false;
            }
            if fp.period && sub == Sub::Filters {
                let pos = Pos2::new(r.response.rect.right() - 4.0, fp.period_top - 4.0);
                let cur = fp.current.clone();
                let p = egui::Area::new(egui::Id::new("filter_popup_period")).fixed_pos(pos).order(egui::Order::Foreground).show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                        ui.set_width(150.0);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let all: Vec<Dynamic> = (1..=4).map(Dynamic::Quarter).chain((1..=12).map(Dynamic::Month)).collect();
                        for (i, d) in all.into_iter().enumerate() {
                            if i == 4 {
                                ui.separator();
                            }
                            let tick = cur == Some(ColumnFilter::Dynamic(d));
                            if menu_row(ui, if tick { "✓" } else { "" }, &d.label(), true, false, 150.0).clicked() {
                                act = Some(Act::Set(Some(ColumnFilter::Dynamic(d))));
                            }
                        }
                    });
                });
                rects.push(p.response.rect);
            }
        }

        let press = ctx.input(|i| if i.pointer.any_pressed() { i.pointer.interact_pos() } else { None });
        let outside = !fp.fresh && press.is_some_and(|p| !rects.iter().any(|r| r.contains(p)));
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        fp.fresh = false;
        let col = fp.col;
        match act {
            Some(Act::Sort(desc)) => {
                if let Some(f) = self.sheet().filter.clone() {
                    let mut rg = f.range;
                    rg.r2 = filter::data_end(self.sheet(), rg);
                    let a = self.active();
                    self.sheet_mut().view.active = (a.0, col);
                    self.sort_range_by(rg, true, &[SortKey::values(col, desc)], SortOptions::default());
                }
            }
            Some(Act::SortColor(font, c)) => self.sort_by_color(col, font, c),
            Some(Act::Set(cf)) => self.set_column_filter(col, cf),
            Some(Act::Custom(op1, op2)) => self.open_custom_filter(col, op1, op2),
            Some(Act::Top10) => self.open_top10(col),
            None if close || outside || esc => {}
            None => self.filter_popup = Some(fp),
        }
    }

    // ------------------------------------------------------------------
    // Custom AutoFilter / Top 10 dialogs
    // ------------------------------------------------------------------

    fn open_custom_filter(&mut self, col: u32, op1: UiOp, op2: Option<UiOp>) {
        let probes = self.filter_column_probes(col, false);
        let kind = filter::column_kind(&probes);
        let mut values: Vec<String> = Vec::new();
        let mut sorted = probes.clone();
        sorted.sort_by(|a, b| crate::datatools::value_cmp(&a.value, &b.value, false, false));
        for p in sorted {
            if !p.is_blank() && !values.contains(&p.text) {
                values.push(p.text);
            }
        }
        let mut d = CustomFilterDlg { col, header: self.filter_header(col), kind, op1, v1: String::new(), and: true, op2, v2: String::new(), values };
        // Reopening shows the column's current custom criteria.
        if let Some(Some(ColumnFilter::Custom { and, c1, c2 })) = self.sheet().filter.as_ref().map(|f| f.columns.get(&col).cloned()) {
            if op1 == UiOp::Equals && op2.is_none() {
                d.op1 = filter::ui_op(&c1);
                d.v1 = filter::ui_value(&c1);
                d.and = and;
                d.op2 = c2.as_ref().map(filter::ui_op);
                d.v2 = c2.as_ref().map(filter::ui_value).unwrap_or_default();
            }
        }
        self.dialog = Some(Dialog::Data(Box::new(DataDlg::CustomFilter(d))));
    }

    fn open_top10(&mut self, col: u32) {
        let mut d = Top10Dlg { col, top: true, val: 10.0, percent: false };
        if let Some(Some(ColumnFilter::Top10 { top, percent, val })) = self.sheet().filter.as_ref().map(|f| f.columns.get(&col).cloned()) {
            d = Top10Dlg { col, top, val, percent };
        }
        self.dialog = Some(Dialog::Data(Box::new(DataDlg::Top10(d))));
    }

    pub(super) fn custom_filter_ui(&mut self, ctx: &egui::Context, d: &mut CustomFilterDlg) -> bool {
        let mut keep = true;
        data_ui::window("Custom AutoFilter").show(ctx, |ui| {
            ui.set_width(470.0);
            ui.label("Show rows where:");
            ui.add_space(2.0);
            ui.label(RichText::new(&d.header).strong());
            let date = d.kind == ColumnKind::Date;
            let label = |o: UiOp| if date { o.date_label() } else { o.label() };
            let values = d.values.clone();
            let cond_row = |ui: &mut egui::Ui, id: &str, op: &mut Option<UiOp>, v: &mut String, allow_none: bool| {
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    egui::ComboBox::from_id_salt((id, "op")).width(190.0).selected_text(op.map(label).unwrap_or("")).show_ui(ui, |ui| {
                        if allow_none {
                            ui.selectable_value(op, None, " ");
                        }
                        for o in UiOp::ALL {
                            ui.selectable_value(op, Some(o), label(o));
                        }
                    });
                    ui.add(egui::TextEdit::singleline(v).desired_width(200.0));
                    egui::ComboBox::from_id_salt((id, "val")).width(18.0).selected_text("").show_ui(ui, |ui| {
                        ui.set_min_width(220.0);
                        for t in &values {
                            if ui.selectable_label(v == t, t.as_str()).clicked() {
                                *v = t.clone();
                            }
                        }
                    });
                });
            };
            let mut op1 = Some(d.op1);
            cond_row(ui, "cf1", &mut op1, &mut d.v1, false);
            d.op1 = op1.unwrap_or(UiOp::Equals);
            ui.horizontal(|ui| {
                ui.add_space(40.0);
                ui.radio_value(&mut d.and, true, "And");
                ui.add_space(10.0);
                ui.radio_value(&mut d.and, false, "Or");
            });
            cond_row(ui, "cf2", &mut d.op2, &mut d.v2, true);
            ui.add_space(6.0);
            ui.label("Use ? to represent any single character");
            ui.label("Use * to represent any series of characters");
            let (ok, cancel) = data_ui::ok_cancel(ui);
            if ok {
                let c1: Condition = d.op1.condition(&d.v1);
                let c2 = d.op2.map(|o| o.condition(&d.v2));
                self.set_column_filter(d.col, Some(ColumnFilter::Custom { and: d.and, c1, c2 }));
                keep = false;
            } else if cancel {
                keep = false;
            }
        });
        keep
    }

    pub(super) fn top10_ui(&mut self, ctx: &egui::Context, d: &mut Top10Dlg) -> bool {
        let mut keep = true;
        data_ui::window("Top 10 AutoFilter").show(ctx, |ui| {
            ui.set_width(330.0);
            ui.label("Show");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("t10_top").width(90.0).selected_text(if d.top { "Top" } else { "Bottom" }).show_ui(ui, |ui| {
                    ui.selectable_value(&mut d.top, true, "Top");
                    ui.selectable_value(&mut d.top, false, "Bottom");
                });
                let max = if d.percent { 100.0 } else { 500.0 };
                ui.add(egui::DragValue::new(&mut d.val).range(1.0..=max).speed(1.0).fixed_decimals(0));
                egui::ComboBox::from_id_salt("t10_pct").width(90.0).selected_text(if d.percent { "Percent" } else { "Items" }).show_ui(ui, |ui| {
                    ui.selectable_value(&mut d.percent, false, "Items");
                    ui.selectable_value(&mut d.percent, true, "Percent");
                });
            });
            let (ok, cancel) = data_ui::ok_cancel(ui);
            if ok {
                self.set_column_filter(d.col, Some(ColumnFilter::Top10 { top: d.top, percent: d.percent, val: d.val.max(1.0) }));
                keep = false;
            } else if cancel {
                keep = false;
            }
        });
        keep
    }
}

/// The (Select All) checklist with a Year > Month > Day tree for dates.
fn checklist(ui: &mut egui::Ui, fp: &mut FilterPopup) {
    let q = fp.search.to_lowercase();
    let leaves = fp.leaves();
    let shown: Vec<&(bool, bool)> = leaves.iter().filter(|l| l.0).collect();
    let all = !shown.is_empty() && shown.iter().all(|l| l.1);
    let none = shown.iter().all(|l| !l.1);
    let mut v = all;
    let label = if q.is_empty() { "(Select All)" } else { "(Select All Search Results)" };
    if ui.add(egui::Checkbox::new(&mut v, label).indeterminate(!all && !none)).clicked() {
        fp.set_all(!all);
    }
    for y in fp.years.iter_mut() {
        let vis_months: Vec<usize> =
            (0..y.months.len()).filter(|&i| y.months[i].days.iter().any(|(d, _)| FilterPopup::date_matches(&q, y.y, y.months[i].m, *d))).collect();
        if vis_months.is_empty() {
            continue;
        }
        let open = y.open || !q.is_empty();
        let yd: Vec<bool> = y.months.iter().flat_map(|m| m.days.iter().map(|d| d.1)).collect();
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(if open { "▾" } else { "▸" }).frame(false).small()).clicked() {
                y.open = !y.open;
            }
            let all = yd.iter().all(|b| *b);
            let mut v = all;
            if ui.add(egui::Checkbox::new(&mut v, y.y.to_string()).indeterminate(!all && yd.iter().any(|b| *b))).clicked() {
                for m in y.months.iter_mut() {
                    m.days.iter_mut().for_each(|d| d.1 = !all);
                }
            }
        });
        if !open {
            continue;
        }
        for i in vis_months {
            let m = &mut y.months[i];
            let mopen = m.open || !q.is_empty();
            let all = m.days.iter().all(|d| d.1);
            let some = m.days.iter().any(|d| d.1);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                if ui.add(egui::Button::new(if mopen { "▾" } else { "▸" }).frame(false).small()).clicked() {
                    m.open = !m.open;
                }
                let mut v = all;
                if ui.add(egui::Checkbox::new(&mut v, MONTH_NAMES[(m.m - 1) as usize]).indeterminate(!all && some)).clicked() {
                    m.days.iter_mut().for_each(|d| d.1 = !all);
                }
            });
            if mopen {
                let (yy, mm) = (y.y, m.m);
                for (d, on) in m.days.iter_mut() {
                    if !FilterPopup::date_matches(&q, yy, mm, *d) {
                        continue;
                    }
                    ui.horizontal(|ui| {
                        ui.add_space(48.0);
                        ui.checkbox(on, format!("{d:02}"));
                    });
                }
            }
        }
    }
    for it in fp.items.iter_mut() {
        if FilterPopup::item_matches(&q, &it.text) {
            ui.checkbox(&mut it.checked, it.text.as_str());
        }
    }
    if let Some(b) = fp.blanks.as_mut() {
        if q.is_empty() || "(blanks)".contains(&q) {
            ui.checkbox(b, "(Blanks)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::data_ui::{frames, sample_app};
    use super::*;

    fn hidden(app: &App) -> Vec<u32> {
        let mut v: Vec<u32> = app.sheet().hidden_rows.iter().map(|r| r + 1).collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn dropdown_menus_and_criteria() {
        let (ctx, mut app) = sample_app();
        app.toggle_filter();
        assert_eq!(app.sheet().filter.as_ref().map(|f| f.range), Some(crate::model::Range::new(0, 0, 7, 4)));
        // every column kind, every submenu
        for col in 0..5 {
            for sub in [None, Some(Sub::SortColor), Some(Sub::FilterColor), Some(Sub::Filters)] {
                app.open_filter_popup(col, Pos2::new(200.0, 40.0));
                frames(&ctx, &mut app, 1);
                if let Some(fp) = app.filter_popup.as_mut() {
                    fp.sub = sub;
                    fp.period = sub == Some(Sub::Filters);
                    fp.search = if col == 3 { "nov".into() } else { String::new() };
                }
                frames(&ctx, &mut app, 2);
                assert!(app.filter_popup.is_some(), "popup closed for column {col}");
            }
        }
        let fp = app.filter_popup.take().unwrap();
        assert_eq!(fp.kind, ColumnKind::Text);

        // date tree: keep only November 2026
        app.open_filter_popup(3, Pos2::new(200.0, 40.0));
        let mut fp = app.filter_popup.take().unwrap();
        assert_eq!(fp.kind, ColumnKind::Date);
        assert_eq!(fp.years.iter().map(|y| y.y).collect::<Vec<_>>(), vec![2024, 2025, 2026]);
        for y in fp.years.iter_mut() {
            for m in y.months.iter_mut() {
                let keep = y.y == 2026 && m.m == 11;
                m.days.iter_mut().for_each(|d| d.1 = keep);
            }
        }
        let cf = fp.result().unwrap();
        assert_eq!(cf, ColumnFilter::Values { items: Default::default(), blanks: false, dates: [DateGroup { year: 2026, month: 11, day: 0 }].into() });
        app.set_column_filter(3, Some(cf));
        assert_eq!(hidden(&app), vec![2, 3, 4, 7, 8]);
        assert_eq!(app.status.as_deref(), Some("2 of 7 records found"));

        // the Region checklist lists only values left by the other column's filter
        app.open_filter_popup(0, Pos2::new(200.0, 40.0));
        let fp = app.filter_popup.take().unwrap();
        let names: Vec<&str> = fp.items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(names, ["North", "West"]);

        app.clear_filter();
        assert!(hidden(&app).is_empty());
        app.set_column_filter(2, Some(ColumnFilter::Top10 { top: true, percent: false, val: 3.0 }));
        assert_eq!(hidden(&app), vec![2, 4, 6, 7]);
        app.set_column_filter(2, Some(ColumnFilter::Color { font: false, color: Some(0xFFFF00) }));
        assert_eq!(hidden(&app), vec![2, 4, 6, 7, 8]);
        app.set_column_filter(2, None);
        app.select(1, 0, false);
        app.filter_by_cell_value();
        assert_eq!(hidden(&app), vec![3, 5, 6, 7]);
        app.undo();
        assert!(hidden(&app).is_empty());

        // dialogs
        app.open_custom_filter(1, UiOp::BeginsWith, None);
        frames(&ctx, &mut app, 2);
        app.dialog = None;
        app.open_top10(2);
        frames(&ctx, &mut app, 2);
        assert!(app.dialog.is_some());
        app.dialog = None;
        app.toggle_filter();
        assert!(app.sheet().filter.is_none() && hidden(&app).is_empty());
    }

    #[test]
    fn filter_off_keeps_rows_hidden_by_hand() {
        let (_ctx, mut app) = sample_app();
        app.toggle_filter();
        app.set_column_filter(0, Some(ColumnFilter::Values { items: ["east".to_string()].into(), blanks: false, dates: Default::default() }));
        assert_eq!(hidden(&app), vec![3, 5, 6, 7]);
        app.select(3, 0, false);
        app.hide_rows(true); // row 4 (shown by the filter) hidden by hand
        app.toggle_filter();
        assert!(app.sheet().filter.is_none());
        assert_eq!(hidden(&app), vec![4]);
    }

    #[test]
    fn find_and_replace() {
        let (ctx, mut app) = sample_app();
        app.find.what = "e?e".into();
        app.find.options = true;
        app.find.replace = true;
        app.dialog = Some(Dialog::Find);
        frames(&ctx, &mut app, 2);
        app.find_all();
        let cells: Vec<String> = app.find.results.iter().map(|f| crate::model::cell_name(f.r, f.c)).collect();
        assert_eq!(cells, ["B6"]);
        frames(&ctx, &mut app, 2);
        app.find.whole = true;
        app.find.what = "east".into();
        app.find.with = "Ost".into();
        assert_eq!(app.replace_all(), 3);
        assert_eq!(numfmt::value_to_text(app.sheet().value(1, 0)), "Ost");
        app.find.by_cols = true;
        app.find.whole = false;
        app.find.what = "o".into();
        app.select(0, 0, false);
        assert!(app.find_next(false));
        assert_eq!(app.active(), (1, 0)); // by columns: "Ost" in A2 before "Bob" in B3
        app.find.what = "zzz".into();
        assert!(!app.find_next(false));
        assert!(app.find.message.is_some());
        frames(&ctx, &mut app, 1);
    }
}
