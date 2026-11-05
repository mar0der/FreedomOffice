//! Data tools: the Sort dialog (levels, Sort On, colour and custom-list orders, options),
//! the three-step Convert Text to Columns wizard and the Subtotal dialog.

use eframe::egui::{self, Align2, FontId, Pos2, RichText, Sense, Stroke, StrokeKind, Vec2};

use super::actions::shift_cell;
use super::dialogs::Dialog;
use super::filter_ui::{CustomFilterDlg, Top10Dlg};
use super::theme;
use super::{App, Scope};
use crate::datatools::{
    self, ColFormat, Converted, DateOrder, SortItem, SortKey, SortOn, SortOptions, SortOrder, BUILTIN_LISTS, SUBTOTAL_FUNCS,
};
use crate::filter::{self, ColumnKind};
use crate::model::{cell_from_input, col_name, Cell, Range, Value, MAX_COLS, MAX_ROWS};
use crate::numfmt;

/// Dialogs owned by the data tools (one `Dialog::Data` variant keeps dialogs.rs small).
pub enum DataDlg {
    CustomFilter(CustomFilterDlg),
    Top10(Top10Dlg),
    Sort(SortDlg),
    TextToColumns(T2cDlg),
    Subtotal(SubtotalDlg),
}

pub(super) fn window(title: &str) -> egui::Window<'static> {
    egui::Window::new(title.to_string()).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, -40.0])
}

pub(super) fn button(ui: &mut egui::Ui, text: &str, enabled: bool) -> bool {
    ui.add_enabled(enabled, egui::Button::new(text).min_size(Vec2::new(76.0, 24.0))).clicked()
}

/// Right-aligned OK / Cancel; Enter and Escape answer them.
pub(super) fn ok_cancel(ui: &mut egui::Ui) -> (bool, bool) {
    let mut ok = false;
    let mut cancel = false;
    ui.add_space(8.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.add(egui::Button::new("Cancel").min_size(Vec2::new(76.0, 24.0))).clicked() {
            cancel = true;
        }
        if ui.add(egui::Button::new("OK").min_size(Vec2::new(76.0, 24.0)).fill(theme::accent_light())).clicked() {
            ok = true;
        }
    });
    if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !egui::Popup::is_any_open(ui.ctx()) {
        ok = true;
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        cancel = true;
    }
    (ok, cancel)
}

// ---------------------------------------------------------------------------
// Sort
// ---------------------------------------------------------------------------

pub struct SortDlg {
    range: Range,
    header: bool,
    opts: SortOptions,
    levels: Vec<SortKey>,
    sel: usize,
    options: Option<SortOptions>,
    /// Custom Lists dialog open for a level: (level, chosen list).
    lists: Option<(usize, usize)>,
}

const MAX_LEVELS: usize = 64;

fn color_text(c: Option<u32>, font: bool) -> String {
    match c {
        None if font => "Automatic".into(),
        None => "No Cell Color".into(),
        Some(c) => format!("#{c:06X}"),
    }
}

impl App {
    /// The range a sort works on and whether its first row looks like headers.
    pub(super) fn sort_area(&self) -> (Range, bool) {
        let sh = self.sheet();
        let sel = self.sel();
        if sel.is_single() {
            if let Some(f) = &sh.filter {
                let mut rg = f.range;
                rg.r2 = filter::data_end(sh, rg);
                if rg.contains(sel.r1, sel.c1) {
                    return (rg, true);
                }
            }
        }
        let rg = if sel.is_single() { self.current_region() } else { self.sel_clipped() };
        // header heuristic: first row all text, second row has non-text
        let first_text = (rg.c1..=rg.c2).all(|c| matches!(sh.value(rg.r1, c), Value::Text(_) | Value::Empty))
            && (rg.c1..=rg.c2).any(|c| matches!(sh.value(rg.r1, c), Value::Text(_)));
        let second_other = rg.r2 > rg.r1 && (rg.c1..=rg.c2).any(|c| !matches!(sh.value(rg.r1 + 1, c), Value::Text(_) | Value::Empty));
        let header = (first_text && second_other) || sh.filter.as_ref().is_some_and(|f| f.range.r1 == rg.r1);
        (rg, header)
    }

    /// Sort A to Z / Z to A on the active cell's column.
    pub fn sort_simple(&mut self, desc: bool) {
        if self.pivot_sort(desc) {
            return;
        }
        let (rg, header) = self.sort_area();
        let c = self.active().1.clamp(rg.c1, rg.c2);
        self.sort_range_by(rg, header, &[SortKey::values(c, desc)], SortOptions::default());
    }

    pub fn open_sort_dialog(&mut self) {
        self.commit_edit(false);
        let (rg, header) = self.sort_area();
        if self.protect_blocks(super::Change::Sort(rg)) {
            return;
        }
        let c = self.active().1.clamp(rg.c1, rg.c2);
        self.dialog = Some(Dialog::Data(Box::new(DataDlg::Sort(SortDlg {
            range: rg,
            header,
            opts: SortOptions::default(),
            levels: vec![SortKey::values(c, false)],
            sel: 0,
            options: None,
            lists: None,
        }))));
    }

    fn sort_item(&self, si: usize, r: u32, c: u32) -> SortItem {
        let p = filter::probe(&self.wb, &self.wb.sheets[si], r, c);
        SortItem { value: p.value, fill: p.fill, font: p.font }
    }

    /// Sorts the rows (or, left to right, the columns) of a range by several keys.
    /// Hidden rows/columns (e.g. filtered out) stay where they are.
    pub fn sort_range_by(&mut self, rg: Range, header: bool, keys: &[SortKey], opts: SortOptions) {
        if keys.is_empty() || self.protect_blocks(super::Change::Sort(rg)) {
            return;
        }
        let si = self.wb.active;
        let l2r = opts.left_to_right;
        let (first, last) = if l2r { (rg.c1 + header as u32, rg.c2) } else { (rg.r1 + header as u32, rg.r2) };
        if first >= last {
            return;
        }
        let data = if l2r { Range::new(rg.r1, first, rg.r2, last) } else { Range::new(first, rg.c1, last, rg.c2) };
        let sh = &self.wb.sheets[si];
        let slots: Vec<u32> = (first..=last).filter(|i| if l2r { !sh.hidden_cols.contains(i) } else { !sh.hidden_rows.contains(i) }).collect();
        let items: Vec<Vec<SortItem>> = slots
            .iter()
            .map(|&s| keys.iter().map(|k| if l2r { self.sort_item(si, k.line, s) } else { self.sort_item(si, s, k.line) }).collect())
            .collect();
        let order = datatools::sort_order(&items, keys, opts.case_sensitive);
        if order.iter().enumerate().all(|(i, o)| i == *o) {
            return;
        }
        self.push_undo(&[Scope::Range(si, data)]);
        let sh = &mut self.wb.sheets[si];
        // lift every slot's cells, then put them back in the new order
        let mut lifted: Vec<Vec<(u32, Cell)>> = Vec::new();
        for &s in &slots {
            let line = if l2r { Range::new(rg.r1, s, rg.r2, s) } else { Range::new(s, rg.c1, s, rg.c2) };
            let keys: Vec<(u32, u32)> = sh.iter_range(line).map(|(r, c, _)| (r, c)).collect();
            lifted.push(keys.into_iter().filter_map(|(r, c)| sh.remove(r, c).map(|cell| (if l2r { r } else { c }, cell))).collect());
        }
        let mut moved: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        let mut lifted: Vec<Option<Vec<(u32, Cell)>>> = lifted.into_iter().map(Some).collect();
        for (k, &o) in order.iter().enumerate() {
            let (from, to) = (slots[o], slots[k]);
            moved.insert(from, to);
            for (other, mut cell) in lifted[o].take().unwrap_or_default() {
                let d = to as i64 - from as i64;
                if l2r {
                    shift_cell(&mut cell, 0, d);
                    sh.set(other, to, Some(cell));
                } else {
                    shift_cell(&mut cell, d, 0);
                    sh.set(to, other, Some(cell));
                }
            }
        }
        let remap = |m: &mut std::collections::HashMap<(u32, u32), String>| {
            *m = m
                .drain()
                .map(|((r, c), t)| {
                    if !data.contains(r, c) {
                        ((r, c), t)
                    } else if l2r {
                        ((r, moved.get(&c).copied().unwrap_or(c)), t)
                    } else {
                        ((moved.get(&r).copied().unwrap_or(r), c), t)
                    }
                })
                .collect();
        };
        remap(&mut sh.notes);
        remap(&mut sh.links);
        self.changed();
    }

    fn sort_line_label(&self, rg: Range, header: bool, l2r: bool, line: u32) -> String {
        if l2r {
            return format!("Row {}", line + 1);
        }
        if header {
            let t = numfmt::value_to_text(self.sheet().display_value(rg.r1, line));
            if !t.is_empty() {
                return t;
            }
        }
        format!("Column {}", col_name(line))
    }

    fn sort_line_probes(&self, rg: Range, header: bool, l2r: bool, line: u32) -> Vec<filter::Probe> {
        let sh = self.sheet();
        if l2r {
            (rg.c1..=rg.c2).map(|c| filter::probe(&self.wb, sh, line, c)).collect()
        } else {
            (rg.r1 + header as u32..=rg.r2).map(|r| filter::probe(&self.wb, sh, r, line)).collect()
        }
    }

    pub(super) fn sort_dialog_ui(&mut self, ctx: &egui::Context, d: &mut SortDlg) -> bool {
        let mut keep = true;
        let modal = d.options.is_some() || d.lists.is_some();
        let lines: Vec<u32> = if d.opts.left_to_right { (d.range.r1..=d.range.r2).collect() } else { (d.range.c1..=d.range.c2).collect() };
        window("Sort").show(ctx, |ui| {
            ui.set_width(640.0);
            if modal {
                ui.disable();
            }
            ui.horizontal(|ui| {
                if button(ui, "+ Add Level", d.levels.len() < MAX_LEVELS) {
                    let line = lines.iter().copied().find(|l| !d.levels.iter().any(|k| k.line == *l)).unwrap_or(lines[0]);
                    d.levels.insert((d.sel + 1).min(d.levels.len()), SortKey::values(line, false));
                    d.sel = (d.sel + 1).min(d.levels.len() - 1);
                }
                if button(ui, "✕ Delete Level", !d.levels.is_empty()) {
                    d.levels.remove(d.sel);
                    d.sel = d.sel.min(d.levels.len().saturating_sub(1));
                }
                if button(ui, "Copy Level", !d.levels.is_empty() && d.levels.len() < MAX_LEVELS) {
                    let k = d.levels[d.sel].clone();
                    d.levels.insert(d.sel + 1, k);
                    d.sel += 1;
                }
                if ui.add_enabled(d.sel > 0, egui::Button::new("▲").min_size(Vec2::new(26.0, 24.0))).on_hover_text("Move Up").clicked() {
                    d.levels.swap(d.sel, d.sel - 1);
                    d.sel -= 1;
                }
                if ui.add_enabled(d.sel + 1 < d.levels.len(), egui::Button::new("▼").min_size(Vec2::new(26.0, 24.0))).on_hover_text("Move Down").clicked() {
                    d.levels.swap(d.sel, d.sel + 1);
                    d.sel += 1;
                }
                if button(ui, "Options...", true) {
                    d.options = Some(d.opts);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled(!d.opts.left_to_right, egui::Checkbox::new(&mut d.header, "My data has headers"));
                });
            });
            ui.add_space(6.0);
            let line_head = if d.opts.left_to_right { "Row" } else { "Column" };
            let (rg, hdr, l2r) = (d.range, d.header && !d.opts.left_to_right, d.opts.left_to_right);
            egui::Frame::new().fill(theme::window()).stroke(Stroke::new(1.0, theme::border())).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                ui.set_width(628.0);
                ui.horizontal(|ui| {
                    ui.add_space(72.0);
                    ui.add_sized([200.0, 18.0], egui::Label::new(RichText::new(line_head).color(theme::text_dim())));
                    ui.add_sized([150.0, 18.0], egui::Label::new(RichText::new("Sort On").color(theme::text_dim())));
                    ui.add_sized([200.0, 18.0], egui::Label::new(RichText::new("Order").color(theme::text_dim())));
                });
                egui::ScrollArea::vertical().max_height(200.0).min_scrolled_height(160.0).auto_shrink([false, false]).show(ui, |ui| {
                    for i in 0..d.levels.len() {
                        let row = ui.horizontal(|ui| {
                            let lbl = ui.add_sized([68.0, 22.0], egui::Button::selectable(i == d.sel, if i == 0 { "Sort by" } else { "Then by" }));
                            if lbl.clicked() {
                                d.sel = i;
                            }
                            let lvl = &mut d.levels[i];
                            let before = lvl.line;
                            egui::ComboBox::from_id_salt(("sort_line", i)).width(196.0).selected_text(self.sort_line_label(rg, hdr, l2r, lvl.line)).show_ui(ui, |ui| {
                                for &l in &lines {
                                    ui.selectable_value(&mut lvl.line, l, self.sort_line_label(rg, hdr, l2r, l));
                                }
                            });
                            let on_before = lvl.on;
                            egui::ComboBox::from_id_salt(("sort_on", i)).width(146.0).selected_text(lvl.on.label()).show_ui(ui, |ui| {
                                for o in SortOn::ALL {
                                    ui.selectable_value(&mut lvl.on, o, o.label());
                                }
                            });
                            let probes = self.sort_line_probes(rg, hdr, l2r, lvl.line);
                            if lvl.on != on_before || (lvl.line != before && lvl.on != SortOn::Values) {
                                lvl.order = match lvl.on {
                                    SortOn::Values => SortOrder::Asc,
                                    SortOn::CellColor => SortOrder::ColorTop(probes.iter().find_map(|p| p.fill)),
                                    SortOn::FontColor => SortOrder::ColorTop(probes.iter().find_map(|p| p.font)),
                                };
                            }
                            let mut open_lists = false;
                            order_combo(ui, i, lvl, &probes, &mut open_lists);
                            if open_lists {
                                d.lists = Some((i, 0));
                            }
                        });
                        if i == d.sel {
                            ui.painter().rect_stroke(row.response.rect.expand(1.0), 2.0, Stroke::new(1.0, theme::accent()), StrokeKind::Outside);
                        }
                    }
                });
            });
            let (ok, cancel) = if modal { (false, false) } else { ok_cancel(ui) };
            if ok {
                let levels = d.levels.clone();
                self.sort_range_by(d.range, d.header && !d.opts.left_to_right, &levels, d.opts);
                keep = false;
            } else if cancel {
                keep = false;
            }
        });
        if let Some(o) = d.options.as_mut() {
            let mut done = None;
            window("Sort Options").show(ctx, |ui| {
                ui.set_width(240.0);
                ui.checkbox(&mut o.case_sensitive, "Case sensitive");
                ui.add_space(6.0);
                ui.label("Orientation");
                ui.radio_value(&mut o.left_to_right, false, "Sort top to bottom");
                ui.radio_value(&mut o.left_to_right, true, "Sort left to right");
                let (ok, cancel) = ok_cancel(ui);
                if ok {
                    done = Some(true);
                } else if cancel {
                    done = Some(false);
                }
            });
            match done {
                Some(true) => {
                    let o = *o;
                    if o.left_to_right != d.opts.left_to_right {
                        let first = if o.left_to_right { d.range.r1 } else { d.range.c1 };
                        d.levels = vec![SortKey::values(first, false)];
                        d.sel = 0;
                    }
                    d.opts = o;
                    d.options = None;
                }
                Some(false) => d.options = None,
                None => {}
            }
        }
        if let Some((lvl, pick)) = d.lists.as_mut() {
            let mut done = None;
            window("Custom Lists").show(ctx, |ui| {
                ui.set_width(460.0);
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.label("Custom lists:");
                        egui::Frame::new().fill(theme::window()).stroke(Stroke::new(1.0, theme::border())).show(ui, |ui| {
                            ui.set_width(250.0);
                            ui.set_min_height(160.0);
                            for (i, l) in BUILTIN_LISTS.iter().enumerate() {
                                let r = ui.selectable_label(*pick == i, l.join(", "));
                                if r.clicked() {
                                    *pick = i;
                                }
                                if r.double_clicked() {
                                    done = Some(true);
                                }
                            }
                        });
                    });
                    ui.vertical(|ui| {
                        ui.label("List entries:");
                        egui::Frame::new().fill(theme::window()).stroke(Stroke::new(1.0, theme::border())).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                            ui.set_width(180.0);
                            ui.set_min_height(160.0);
                            for e in BUILTIN_LISTS[*pick] {
                                ui.label(*e);
                            }
                        });
                    });
                });
                let (ok, cancel) = ok_cancel(ui);
                if ok {
                    done = Some(true);
                } else if cancel {
                    done = Some(false);
                }
            });
            match done {
                Some(true) => {
                    let list = BUILTIN_LISTS[*pick].iter().map(|s| s.to_string()).collect();
                    if let Some(k) = d.levels.get_mut(*lvl) {
                        k.on = SortOn::Values;
                        k.order = SortOrder::Custom(list, false);
                    }
                    d.lists = None;
                }
                Some(false) => d.lists = None,
                None => {}
            }
        }
        keep
    }
}

fn order_combo(ui: &mut egui::Ui, i: usize, lvl: &mut SortKey, probes: &[filter::Probe], open_lists: &mut bool) {
    match lvl.on {
        SortOn::Values => {
            let kind = filter::column_kind(probes);
            let (asc, desc) = match kind {
                ColumnKind::Text => ("A to Z", "Z to A"),
                ColumnKind::Number => ("Smallest to Largest", "Largest to Smallest"),
                ColumnKind::Date => ("Oldest to Newest", "Newest to Oldest"),
            };
            let text = match &lvl.order {
                SortOrder::Asc => asc.to_string(),
                SortOrder::Desc => desc.to_string(),
                SortOrder::Custom(l, rev) => {
                    let mut l = l.clone();
                    if *rev {
                        l.reverse();
                    }
                    l.join(", ")
                }
                _ => asc.to_string(),
            };
            egui::ComboBox::from_id_salt(("sort_ord", i)).width(196.0).selected_text(text).show_ui(ui, |ui| {
                ui.selectable_value(&mut lvl.order, SortOrder::Asc, asc);
                ui.selectable_value(&mut lvl.order, SortOrder::Desc, desc);
                if let SortOrder::Custom(l, _) = lvl.order.clone() {
                    let mut r = l.clone();
                    r.reverse();
                    ui.selectable_value(&mut lvl.order, SortOrder::Custom(l.clone(), false), l.join(", "));
                    ui.selectable_value(&mut lvl.order, SortOrder::Custom(l, true), r.join(", "));
                }
                if ui.selectable_label(false, "Custom List...").clicked() {
                    *open_lists = true;
                }
            });
        }
        SortOn::CellColor | SortOn::FontColor => {
            let font = lvl.on == SortOn::FontColor;
            let mut colors: Vec<Option<u32>> = Vec::new();
            for p in probes {
                let c = if font { p.font } else { p.fill };
                if !colors.contains(&c) {
                    colors.push(c);
                }
            }
            let (cur, top) = match &lvl.order {
                SortOrder::ColorTop(c) => (*c, true),
                SortOrder::ColorBottom(c) => (*c, false),
                _ => (None, true),
            };
            let mut pick = cur;
            egui::ComboBox::from_id_salt(("sort_color", i)).width(100.0).selected_text(swatch_text(cur, font)).show_ui(ui, |ui| {
                for c in &colors {
                    ui.selectable_value(&mut pick, *c, swatch_text(*c, font));
                }
            });
            let mut on_top = top;
            egui::ComboBox::from_id_salt(("sort_top", i)).width(86.0).selected_text(if on_top { "On Top" } else { "On Bottom" }).show_ui(ui, |ui| {
                ui.selectable_value(&mut on_top, true, "On Top");
                ui.selectable_value(&mut on_top, false, "On Bottom");
            });
            lvl.order = if on_top { SortOrder::ColorTop(pick) } else { SortOrder::ColorBottom(pick) };
        }
    }
}

fn swatch_text(c: Option<u32>, font: bool) -> RichText {
    match c {
        Some(c) if font => RichText::new("Abc ■").color(theme::rgb(c)).strong(),
        Some(c) => RichText::new("■■■■■").color(theme::rgb(c)),
        None => RichText::new(color_text(None, font)),
    }
}

// ---------------------------------------------------------------------------
// Text to Columns
// ---------------------------------------------------------------------------

pub struct T2cDlg {
    step: u8,
    src: Range,
    lines: Vec<(u32, String)>,
    fixed: bool,
    tab: bool,
    semicolon: bool,
    comma: bool,
    space: bool,
    other: bool,
    other_ch: String,
    consecutive: bool,
    qualifier: Option<char>,
    breaks: Vec<usize>,
    drag: Option<usize>,
    formats: Vec<ColFormat>,
    sel_col: usize,
    date_order: DateOrder,
    dest: String,
}

impl T2cDlg {
    fn delims(&self) -> Vec<char> {
        let mut d = Vec::new();
        if self.tab {
            d.push('\t');
        }
        if self.semicolon {
            d.push(';');
        }
        if self.comma {
            d.push(',');
        }
        if self.space {
            d.push(' ');
        }
        if self.other {
            if let Some(c) = self.other_ch.chars().next() {
                d.push(c);
            }
        }
        d
    }

    pub fn fields(&self) -> Vec<Vec<String>> {
        let delims = self.delims();
        self.lines
            .iter()
            .map(|(_, l)| {
                if self.fixed {
                    datatools::split_fixed(l, &self.breaks)
                } else {
                    datatools::split_delimited(l, &delims, self.consecutive, self.qualifier)
                }
            })
            .collect()
    }

    fn ncols(&self) -> usize {
        self.fields().iter().map(|f| f.len()).max().unwrap_or(1)
    }

    fn sync_formats(&mut self) {
        let n = self.ncols();
        self.formats.resize(n, ColFormat::General);
        self.formats.truncate(n);
        self.sel_col = self.sel_col.min(n.saturating_sub(1));
    }
}

impl App {
    /// Data > Text to Columns.
    pub fn open_text_to_columns(&mut self) {
        self.commit_edit(false);
        let sel = self.sel_clipped();
        if self.locked_block(sel) {
            return;
        }
        if sel.cols() > 1 {
            self.dialog = Some(Dialog::Message {
                title: "Convert Text to Columns Wizard".into(),
                text: "We can only convert one column at a time. Try selecting cells in one column only.".into(),
            });
            return;
        }
        let sh = self.sheet();
        let c = sel.c1;
        let lines: Vec<(u32, String)> = (sel.r1..=sel.r2)
            .filter_map(|r| {
                let cell = sh.get(r, c)?;
                if cell.formula.is_some() || cell.value.is_empty() {
                    return None;
                }
                Some((r, numfmt::format_value(&cell.value, &self.wb.style(cell.style).num_fmt).text))
            })
            .collect();
        if lines.is_empty() {
            self.dialog = Some(Dialog::Message { title: "Convert Text to Columns Wizard".into(), text: "No data was selected to parse.".into() });
            return;
        }
        let refs: Vec<&str> = lines.iter().map(|l| l.1.as_str()).collect();
        let delimited = refs.iter().any(|l| l.contains([',', ';', '\t']));
        let breaks = datatools::suggest_breaks(&refs);
        let mut d = T2cDlg {
            step: 1,
            src: sel,
            fixed: !delimited && !breaks.is_empty(),
            tab: true,
            semicolon: false,
            comma: false,
            space: false,
            other: false,
            other_ch: String::new(),
            consecutive: false,
            qualifier: Some('"'),
            breaks,
            drag: None,
            formats: Vec::new(),
            sel_col: 0,
            date_order: DateOrder::MDY,
            dest: format!("${}${}", col_name(c), sel.r1 + 1),
            lines,
        };
        d.sync_formats();
        self.dialog = Some(Dialog::Data(Box::new(DataDlg::TextToColumns(d))));
    }

    /// Writes the parsed columns; returns false when the destination is invalid.
    pub fn text_to_columns_apply(&mut self, d: &T2cDlg) -> bool {
        let Some((r0, c0)) = crate::model::parse_cell(d.dest.rsplit('!').next().unwrap_or("")) else { return false };
        let fields = d.fields();
        let out_cols = d.formats.iter().filter(|f| **f != ColFormat::Skip).count().max(1) as u32;
        let si = self.wb.active;
        let first = d.lines.first().map(|l| l.0).unwrap_or(d.src.r1);
        let last = d.lines.last().map(|l| l.0).unwrap_or(d.src.r2);
        let r_off = r0 as i64 - first as i64;
        // The converted data must fit on the sheet.
        if last as i64 + r_off >= MAX_ROWS as i64 || c0 as u64 + out_cols as u64 > MAX_COLS as u64 {
            return false;
        }
        let target = Range::new((first as i64 + r_off).max(0) as u32, c0, (last as i64 + r_off).max(0) as u32, c0 + out_cols - 1);
        if self.locked_block(target) {
            return true;
        }
        self.push_undo(&[Scope::Range(si, target.union(&d.src))]);
        for ((r, _), parts) in d.lines.iter().zip(fields) {
            let rr = (*r as i64 + r_off).max(0) as u32;
            let mut cc = c0;
            for (j, part) in parts.iter().enumerate() {
                let fmt = d.formats.get(j).copied().unwrap_or(ColFormat::General);
                let conv = datatools::convert_field(part, fmt);
                if conv == Converted::Skip {
                    continue;
                }
                if cc >= MAX_COLS {
                    break;
                }
                let style = self.wb.sheets[si].get(rr, cc).map(|x| x.style).unwrap_or(0);
                let cell = match conv {
                    Converted::Typed(t) => cell_from_input(&mut self.wb, &t, style),
                    Converted::Value(v, f) => {
                        let mut st = self.wb.style(style).clone();
                        st.num_fmt = f.to_string();
                        let id = self.wb.intern_style(st);
                        Some(Cell { value: v, formula: None, style: id })
                    }
                    Converted::Skip => None,
                };
                self.wb.sheets[si].set(rr, cc, cell);
                cc += 1;
            }
            // a field list shorter than before leaves no stale cells behind in the source row
            if rr == *r && c0 == d.src.c1 && cc == c0 {
                self.wb.sheets[si].set(rr, c0, None);
            }
        }
        self.changed();
        true
    }

    pub(super) fn t2c_ui(&mut self, ctx: &egui::Context, d: &mut T2cDlg) -> bool {
        let mut keep = true;
        let title = format!("Convert Text to Columns Wizard - Step {} of 3", d.step);
        egui::Window::new(title).id(egui::Id::new("t2c_wizard")).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, -30.0]).show(ctx, |ui| {
            ui.set_width(600.0);
            match d.step {
                1 => {
                    ui.label(format!(
                        "The Text Wizard has determined that your data is {}.",
                        if d.fixed { "Fixed Width" } else { "Delimited" }
                    ));
                    ui.label("If this is correct, choose Next, or choose the data type that best describes your data.");
                    ui.add_space(6.0);
                    ui.group(|ui| {
                        ui.set_width(580.0);
                        ui.label(RichText::new("Original data type").strong());
                        ui.label("Choose the file type that best describes your data:");
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut d.fixed, false, "Delimited");
                            ui.add_space(40.0);
                            ui.label(RichText::new("- Characters such as commas or tabs separate each field.").color(theme::text_dim()));
                        });
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut d.fixed, true, "Fixed width");
                            ui.add_space(34.0);
                            ui.label(RichText::new("- Fields are aligned in columns with spaces between each field.").color(theme::text_dim()));
                        });
                    });
                    ui.add_space(6.0);
                    ui.label("Preview of selected data:");
                    raw_preview(ui, d);
                }
                2 if !d.fixed => {
                    ui.label("This screen lets you set the delimiters your data contains. You can see how your text is affected in the preview below.");
                    ui.add_space(6.0);
                    ui.horizontal_top(|ui| {
                        ui.group(|ui| {
                            ui.set_width(170.0);
                            ui.label(RichText::new("Delimiters").strong());
                            ui.checkbox(&mut d.tab, "Tab");
                            ui.checkbox(&mut d.semicolon, "Semicolon");
                            ui.checkbox(&mut d.comma, "Comma");
                            ui.checkbox(&mut d.space, "Space");
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut d.other, "Other:");
                                if ui.add(egui::TextEdit::singleline(&mut d.other_ch).desired_width(24.0).char_limit(1)).changed() {
                                    d.other = !d.other_ch.is_empty();
                                }
                            });
                        });
                        ui.vertical(|ui| {
                            ui.add_space(30.0);
                            ui.checkbox(&mut d.consecutive, "Treat consecutive delimiters as one");
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                ui.label("Text qualifier:");
                                let txt = match d.qualifier {
                                    Some('"') => "\"",
                                    Some('\'') => "'",
                                    _ => "{none}",
                                };
                                egui::ComboBox::from_id_salt("t2c_q").width(70.0).selected_text(txt).show_ui(ui, |ui| {
                                    ui.selectable_value(&mut d.qualifier, Some('"'), "\"");
                                    ui.selectable_value(&mut d.qualifier, Some('\''), "'");
                                    ui.selectable_value(&mut d.qualifier, None, "{none}");
                                });
                            });
                        });
                    });
                    d.sync_formats();
                    ui.add_space(6.0);
                    ui.label("Data preview");
                    grid_preview(ui, d, false);
                }
                2 => {
                    ui.label("This screen lets you set field widths (column breaks).");
                    ui.label("Lines with arrows signify a column break.");
                    ui.add_space(4.0);
                    ui.label("To CREATE a break line, click at the desired position.");
                    ui.label("To DELETE a break line, double click on the line.");
                    ui.label("To MOVE a break line, click and drag it.");
                    ui.add_space(6.0);
                    ui.label("Data preview");
                    ruler_preview(ui, d);
                    d.sync_formats();
                }
                _ => {
                    ui.label("This screen lets you select each column and set the Data Format.");
                    ui.add_space(6.0);
                    ui.horizontal_top(|ui| {
                        ui.group(|ui| {
                            ui.set_width(220.0);
                            ui.label(RichText::new("Column data format").strong());
                            let cur = d.formats.get(d.sel_col).copied().unwrap_or(ColFormat::General);
                            let mut pick = cur;
                            ui.radio_value(&mut pick, ColFormat::General, "General");
                            ui.radio_value(&mut pick, ColFormat::Text, "Text");
                            ui.horizontal(|ui| {
                                if ui.radio(matches!(pick, ColFormat::Date(_)), "Date:").clicked() {
                                    pick = ColFormat::Date(d.date_order);
                                }
                                egui::ComboBox::from_id_salt("t2c_dmy").width(70.0).selected_text(d.date_order.label()).show_ui(ui, |ui| {
                                    for o in DateOrder::ALL {
                                        if ui.selectable_value(&mut d.date_order, o, o.label()).clicked() {
                                            pick = ColFormat::Date(o);
                                        }
                                    }
                                });
                            });
                            ui.radio_value(&mut pick, ColFormat::Skip, "Do not import column (skip)");
                            if pick != cur {
                                if let Some(f) = d.formats.get_mut(d.sel_col) {
                                    *f = pick;
                                }
                            }
                        });
                        ui.vertical(|ui| {
                            ui.add_space(20.0);
                            ui.label(RichText::new("'General' converts numeric values to numbers, date values to dates, and all remaining values to text.").color(theme::text_dim()));
                        });
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label("Destination:");
                        ui.add(egui::TextEdit::singleline(&mut d.dest).desired_width(200.0));
                    });
                    ui.add_space(6.0);
                    ui.label("Data preview");
                    grid_preview(ui, d, true);
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let finish = ui.add(egui::Button::new("Finish").min_size(Vec2::new(76.0, 24.0)).fill(theme::accent_light())).clicked();
                    if button(ui, "Next >", d.step < 3) {
                        d.step += 1;
                        d.sync_formats();
                    }
                    if button(ui, "< Back", d.step > 1) {
                        d.step -= 1;
                    }
                    if button(ui, "Cancel", true) || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        keep = false;
                    }
                    if finish {
                        d.sync_formats();
                        if self.text_to_columns_apply(d) {
                            keep = false;
                        } else {
                            self.status = Some("Destination reference is not valid.".into());
                        }
                    }
                });
            });
        });
        keep
    }
}

fn mono() -> FontId {
    FontId::monospace(12.0)
}

/// Step 1: the raw lines with their row numbers.
fn raw_preview(ui: &mut egui::Ui, d: &T2cDlg) {
    preview_frame(ui, 130.0, |ui| {
        for (r, l) in d.lines.iter().take(200) {
            ui.label(RichText::new(format!("{:<4}{}", r + 1, l.replace('\t', "→"))).font(mono()));
        }
    });
}

fn preview_frame(ui: &mut egui::Ui, h: f32, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().fill(theme::window()).stroke(Stroke::new(1.0, theme::border())).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
        ui.set_width(586.0);
        egui::ScrollArea::both().max_height(h).min_scrolled_height(h).auto_shrink([false, false]).show(ui, add);
    });
}

/// Steps 2 (delimited) and 3: parsed fields in columns; step 3 also picks a column.
fn grid_preview(ui: &mut egui::Ui, d: &mut T2cDlg, pick: bool) {
    let fields = d.fields();
    let n = d.ncols();
    let widths: Vec<usize> = (0..n)
        .map(|j| fields.iter().map(|f| f.get(j).map(|s| s.chars().count()).unwrap_or(0)).max().unwrap_or(0).clamp(4, 40))
        .collect();
    let cw = ui.fonts_mut(|f| f.glyph_width(&mono(), 'M'));
    let mut clicked = None;
    preview_frame(ui, 130.0, |ui| {
        ui.spacing_mut().item_spacing = Vec2::new(0.0, 0.0);
        if pick {
            ui.horizontal(|ui| {
                for (j, w) in widths.iter().enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(*w as f32 * cw + 10.0, 18.0), Sense::click());
                    let sel = j == d.sel_col;
                    ui.painter().rect_filled(rect, 0.0, if sel { theme::accent() } else { theme::header_bg() });
                    ui.painter().text(
                        rect.left_center() + Vec2::new(3.0, 0.0),
                        Align2::LEFT_CENTER,
                        d.formats.get(j).map(|f| f.label()).unwrap_or("General"),
                        mono(),
                        if sel { theme::window() } else { theme::text() },
                    );
                    if resp.clicked() {
                        clicked = Some(j);
                    }
                }
            });
        }
        for row in fields.iter().take(200) {
            ui.horizontal(|ui| {
                for (j, w) in widths.iter().enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(*w as f32 * cw + 10.0, 16.0), Sense::click());
                    let sel = pick && j == d.sel_col;
                    if sel {
                        ui.painter().rect_filled(rect, 0.0, theme::accent());
                    }
                    let skip = pick && d.formats.get(j) == Some(&ColFormat::Skip);
                    let text: String = row.get(j).map(|s| s.chars().take(40).collect()).unwrap_or_default();
                    let col = if sel { theme::window() } else if skip { theme::text_dim() } else { theme::text() };
                    ui.painter().text(rect.left_center() + Vec2::new(3.0, 0.0), Align2::LEFT_CENTER, text, mono(), col);
                    ui.painter().line_segment([rect.right_top(), rect.right_bottom()], Stroke::new(1.0, theme::border()));
                    if resp.clicked() {
                        clicked = Some(j);
                    }
                }
            });
        }
    });
    if let Some(j) = clicked {
        d.sel_col = j;
    }
}

/// Step 2 (fixed width): ruler with break lines over the data.
fn ruler_preview(ui: &mut egui::Ui, d: &mut T2cDlg) {
    let cw = ui.fonts_mut(|f| f.glyph_width(&mono(), 'M'));
    let width = d.lines.iter().map(|l| l.1.chars().count()).max().unwrap_or(0).max(60) + 10;
    let lh = 16.0;
    let rows = d.lines.len().min(200);
    egui::Frame::new().fill(theme::window()).stroke(Stroke::new(1.0, theme::border())).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
        ui.set_width(586.0);
        egui::ScrollArea::both().max_height(170.0).min_scrolled_height(170.0).auto_shrink([false, false]).show(ui, |ui| {
            let size = Vec2::new(width as f32 * cw + 8.0, 22.0 + rows as f32 * lh);
            let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
            let p = ui.painter_at(rect);
            let x0 = rect.left() + 4.0;
            let pos_x = |k: usize| x0 + k as f32 * cw;
            // ruler
            for k in 0..=width {
                let x = pos_x(k);
                let h = if k % 10 == 0 { 8.0 } else if k % 5 == 0 { 5.0 } else { 3.0 };
                p.line_segment([Pos2::new(x, rect.top() + 18.0 - h), Pos2::new(x, rect.top() + 18.0)], Stroke::new(1.0, theme::text_dim()));
                if k % 10 == 0 {
                    p.text(Pos2::new(x + 2.0, rect.top() + 1.0), Align2::LEFT_TOP, k.to_string(), FontId::proportional(9.0), theme::text_dim());
                }
            }
            p.line_segment([Pos2::new(rect.left(), rect.top() + 18.0), Pos2::new(rect.right(), rect.top() + 18.0)], Stroke::new(1.0, theme::text_dim()));
            for (i, (_, l)) in d.lines.iter().take(rows).enumerate() {
                p.text(Pos2::new(x0, rect.top() + 22.0 + i as f32 * lh), Align2::LEFT_TOP, l, mono(), theme::text());
            }
            for &b in &d.breaks {
                let x = pos_x(b);
                p.line_segment([Pos2::new(x, rect.top() + 18.0), Pos2::new(x, rect.bottom())], Stroke::new(1.0, theme::text()));
                let tri = vec![Pos2::new(x, rect.top() + 18.0), Pos2::new(x - 4.0, rect.top() + 12.0), Pos2::new(x + 4.0, rect.top() + 12.0)];
                p.add(egui::Shape::convex_polygon(tri, theme::text(), Stroke::NONE));
            }
            let at = |pos: Pos2| -> usize { (((pos.x - x0) / cw).round().max(1.0) as usize).min(width) };
            let near = |pos: Pos2, breaks: &Vec<usize>| breaks.iter().position(|b| (pos_x(*b) - pos.x).abs() <= 4.0);
            if let Some(pos) = resp.interact_pointer_pos() {
                if resp.double_clicked() {
                    if let Some(i) = near(pos, &d.breaks) {
                        d.breaks.remove(i);
                    }
                } else if resp.drag_started() {
                    d.drag = near(pos, &d.breaks);
                } else if resp.dragged() {
                    if let Some(i) = d.drag {
                        let k = at(pos);
                        if !d.breaks.iter().enumerate().any(|(j, b)| j != i && *b == k) {
                            d.breaks[i] = k;
                        }
                    }
                } else if resp.clicked() && near(pos, &d.breaks).is_none() {
                    let k = at(pos);
                    if !d.breaks.contains(&k) {
                        d.breaks.push(k);
                    }
                }
            }
            if resp.drag_stopped() {
                d.drag = None;
            }
            d.breaks.sort_unstable();
            d.breaks.dedup();
            if resp.hovered() {
                ui.ctx().set_cursor_icon(if d.drag.is_some() { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::Default });
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Subtotal
// ---------------------------------------------------------------------------

pub struct SubtotalDlg {
    range: Range,
    at_change: u32,
    func: usize,
    add_to: Vec<(u32, bool)>,
    replace: bool,
    page_breaks: bool,
    below: bool,
}

impl App {
    /// Data > Subtotal.
    pub fn open_subtotal(&mut self) {
        self.commit_edit(false);
        if self.protect_blocks(super::Change::Any) {
            return;
        }
        let (rg, _) = self.sort_area();
        if rg.rows() < 2 {
            self.dialog = Some(Dialog::Message {
                title: "Subtotal".into(),
                text: "The command could not be completed. Select a cell in a list with a header row and try again.".into(),
            });
            return;
        }
        let last = rg.c2;
        self.dialog = Some(Dialog::Data(Box::new(DataDlg::Subtotal(SubtotalDlg {
            range: rg,
            at_change: rg.c1,
            func: 0,
            add_to: (rg.c1..=rg.c2).map(|c| (c, c == last)).collect(),
            replace: true,
            page_breaks: false,
            below: true,
        }))));
    }

    fn header_label(&self, rg: Range, c: u32) -> String {
        let t = numfmt::value_to_text(self.sheet().display_value(rg.r1, c));
        if t.is_empty() { format!("Column {}", col_name(c)) } else { t }
    }

    /// Removes rows holding SUBTOTAL formulas inside the list and its outline; returns the new range.
    pub fn remove_subtotals(&mut self, rg: Range) -> Range {
        let si = self.wb.active;
        let mut rg = rg;
        let mut r = rg.r2;
        while r > rg.r1 {
            let is_total = (rg.c1..=rg.c2).any(|c| {
                self.wb.sheets[si].get(r, c).and_then(|x| x.formula.as_ref()).is_some_and(|f| f.text.to_ascii_uppercase().starts_with("SUBTOTAL("))
            });
            if is_total {
                self.shift_rows_no_undo(r, -1);
                rg.r2 -= 1;
            }
            r -= 1;
        }
        let sh = &mut self.wb.sheets[si];
        for r in rg.r1..=rg.r2 + 1 {
            sh.row_levels.remove(&r);
            sh.hidden_rows.remove(&r);
        }
        sh.page.row_breaks.retain(|r| *r <= rg.r1 || *r > rg.r2 + 1);
        rg
    }

    /// Inserts a subtotal row at each change in `at_change` plus a grand total, and outlines them
    /// (detail rows at level 2, subtotal rows at level 1).
    pub fn insert_subtotals(&mut self, rg: Range, at_change: u32, func: usize, cols: &[u32], replace: bool, below: bool, page_breaks: bool) {
        if self.protect_blocks(super::Change::Any) {
            return;
        }
        let si = self.wb.active;
        self.push_undo(&[Scope::All]);
        let rg = if replace { self.remove_subtotals(rg) } else { rg };
        let (_, num, suffix) = SUBTOTAL_FUNCS[func.min(SUBTOTAL_FUNCS.len() - 1)];
        let keys: Vec<String> = (rg.r1 + 1..=rg.r2).map(|r| numfmt::value_to_text(self.wb.sheets[si].display_value(r, at_change))).collect();
        let groups = datatools::groups(&keys);
        let bold = {
            let mut st = self.wb.style(0).clone();
            st.bold = true;
            self.wb.intern_style(st)
        };
        let put = |app: &mut App, r: u32, c: u32, text: &str, label: bool| {
            let style = app.wb.sheets[si].get(r, c).map(|x| x.style).unwrap_or(0);
            let style = if label && style == 0 { bold } else { style };
            let cell = cell_from_input(&mut app.wb, text, style);
            app.wb.sheets[si].set(r, c, cell);
        };
        let write_total = |app: &mut App, total: u32, a: u32, b: u32, label: &str| {
            put(app, total, at_change, label, true);
            for &c in cols {
                let n = col_name(c);
                put(app, total, c, &format!("=SUBTOTAL({num},{n}{}:{n}{})", a + 1, b + 1), false);
            }
        };
        // bottom-up, so the rows of groups not yet handled keep their numbers
        for (k, (s, e)) in groups.iter().enumerate().rev() {
            let (s, e) = (rg.r1 + 1 + *s as u32, rg.r1 + 1 + *e as u32);
            let key = &keys[(s - rg.r1 - 1) as usize];
            let label = format!("{key} {suffix}");
            let (total, a, b) = if below { (e + 1, s, e) } else { (s, s + 1, e + 1) };
            self.shift_rows_no_undo(total, 1);
            write_total(self, total, a, b, &label);
            let sh = &mut self.wb.sheets[si];
            for r in a..=b {
                sh.row_levels.insert(r, 2);
            }
            sh.row_levels.insert(total, 1);
            // Page break between groups: each group (with its total) starts a page.
            if page_breaks && k > 0 {
                sh.page.row_breaks.insert(if below { a } else { total });
            }
        }
        let last = rg.r2 + groups.len() as u32;
        let grand_label = if suffix == "Total" { "Grand Total".to_string() } else { format!("Grand {suffix}") };
        let end = if below {
            let g = last + 1;
            self.shift_rows_no_undo(g, 1);
            write_total(self, g, rg.r1 + 1, last, &grand_label);
            g
        } else {
            let g = rg.r1 + 1;
            self.shift_rows_no_undo(g, 1);
            write_total(self, g, g + 1, last + 1, &grand_label);
            last + 1
        };
        if let Some(f) = &mut self.wb.sheets[si].filter {
            if f.range.r1 == rg.r1 {
                f.range.r2 = end;
            }
        }
        self.changed();
    }

    /// Data > Subtotal > Remove All.
    pub fn remove_all_subtotals(&mut self, rg: Range) {
        if self.protect_blocks(super::Change::Any) {
            return;
        }
        self.push_undo(&[Scope::All]);
        self.remove_subtotals(rg);
        self.changed();
    }

    pub(super) fn subtotal_ui(&mut self, ctx: &egui::Context, d: &mut SubtotalDlg) -> bool {
        let mut keep = true;
        window("Subtotal").show(ctx, |ui| {
            ui.set_width(300.0);
            let rg = d.range;
            ui.label("At each change in:");
            egui::ComboBox::from_id_salt("st_at").width(290.0).selected_text(self.header_label(rg, d.at_change)).show_ui(ui, |ui| {
                for c in rg.c1..=rg.c2 {
                    ui.selectable_value(&mut d.at_change, c, self.header_label(rg, c));
                }
            });
            ui.label("Use function:");
            egui::ComboBox::from_id_salt("st_fn").width(290.0).selected_text(SUBTOTAL_FUNCS[d.func].0).show_ui(ui, |ui| {
                for (i, f) in SUBTOTAL_FUNCS.iter().enumerate() {
                    ui.selectable_value(&mut d.func, i, f.0);
                }
            });
            ui.label("Add subtotal to:");
            egui::Frame::new().fill(theme::window()).stroke(Stroke::new(1.0, theme::border())).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(110.0).min_scrolled_height(110.0).auto_shrink([false, false]).show(ui, |ui| {
                    ui.set_width(282.0);
                    for (c, on) in d.add_to.iter_mut() {
                        ui.checkbox(on, self.header_label(rg, *c));
                    }
                });
            });
            ui.add_space(4.0);
            ui.checkbox(&mut d.replace, "Replace current subtotals");
            ui.checkbox(&mut d.page_breaks, "Page break between groups");
            ui.checkbox(&mut d.below, "Summary below data");
            ui.add_space(6.0);
            let mut remove = false;
            ui.horizontal(|ui| {
                remove = button(ui, "Remove All", true);
            });
            let (ok, cancel) = ok_cancel(ui);
            if remove {
                self.remove_all_subtotals(rg);
                keep = false;
            } else if ok {
                let cols: Vec<u32> = d.add_to.iter().filter(|c| c.1).map(|c| c.0).collect();
                if cols.is_empty() {
                    self.status = Some("Select at least one column to add a subtotal to.".into());
                } else {
                    self.insert_subtotals(rg, d.at_change, d.func, &cols, d.replace, d.below, d.page_breaks);
                    keep = false;
                }
            } else if cancel {
                keep = false;
            }
        });
        keep
    }

    /// Dispatches the data-tool dialogs; returns false when the dialog closed.
    pub(super) fn data_dialog_ui(&mut self, ctx: &egui::Context, d: &mut DataDlg) -> bool {
        match d {
            DataDlg::CustomFilter(x) => self.custom_filter_ui(ctx, x),
            DataDlg::Top10(x) => self.top10_ui(ctx, x),
            DataDlg::Sort(x) => self.sort_dialog_ui(ctx, x),
            DataDlg::TextToColumns(x) => self.t2c_ui(ctx, x),
            DataDlg::Subtotal(x) => self.subtotal_ui(ctx, x),
        }
    }
}

/// Headless app with a small list: Region | Rep | Amount | Date | Notes (rows 2-8).
#[cfg(test)]
pub(super) fn sample_app() -> (egui::Context, App) {
    let ctx = egui::Context::default();
    let mut app = App::with_context(&ctx, 0, None);
    app.dialog = None;
    let date = app.wb.intern_style(crate::model::Style { num_fmt: "m/d/yyyy".into(), ..Default::default() });
    let yellow = app.wb.intern_style(crate::model::Style { fill: Some(0xFFFF00), ..Default::default() });
    let rows = [
        ("East", "Ann", 120.0, (2025, 1, 5), "a,b,c"),
        ("West", "Bob", 340.0, (2025, 2, 11), "x,y"),
        ("East", "Cid", 90.0, (2026, 3, 2), "one,two,three"),
        ("North", "Dee", 410.0, (2026, 11, 3), "p,q"),
        ("West", "Eve", 220.0, (2026, 11, 4), "m"),
        ("South", "Fay", 75.0, (2024, 7, 19), "k,l"),
        ("East", "Gus", 500.0, (2026, 10, 30), "z"),
    ];
    let sh = &mut app.wb.sheets[0];
    let text = |s: &str| Some(Cell { value: Value::Text(s.into()), ..Default::default() });
    for (c, h) in ["Region", "Rep", "Amount", "Date", "Notes"].iter().enumerate() {
        sh.set(0, c as u32, text(h));
    }
    for (i, (reg, rep, amt, (y, m, d), notes)) in rows.iter().enumerate() {
        let r = i as u32 + 1;
        sh.set(r, 0, text(reg));
        sh.set(r, 1, text(rep));
        let style = if r == 2 || r == 4 { yellow } else { 0 };
        sh.set(r, 2, Some(Cell { value: Value::Number(*amt), formula: None, style }));
        sh.set(r, 3, Some(Cell { value: Value::Number(numfmt::ymd_to_serial(*y, *m, *d)), formula: None, style: date }));
        sh.set(r, 4, text(notes));
    }
    app.select(1, 0, false);
    (ctx, app)
}

/// Runs a few headless frames of the dialogs and popups.
#[cfg(test)]
pub(super) fn frames(ctx: &egui::Context, app: &mut App, n: usize) {
    for _ in 0..n {
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            app.dialogs_ui(ctx);
            app.popups_ui(ctx);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_cell;

    fn txt(app: &App, a1: &str) -> String {
        let (r, c) = parse_cell(a1).unwrap();
        numfmt::value_to_text(app.sheet().display_value(r, c))
    }

    #[test]
    fn sort_dialog_and_sorting() {
        let (ctx, mut app) = sample_app();
        app.open_sort_dialog();
        frames(&ctx, &mut app, 2);
        assert!(matches!(&app.dialog, Some(Dialog::Data(d)) if matches!(**d, DataDlg::Sort(_))));
        if let Some(Dialog::Data(d)) = &mut app.dialog {
            if let DataDlg::Sort(s) = &mut **d {
                assert!(s.header);
                s.levels.push(SortKey { line: 2, on: SortOn::CellColor, order: SortOrder::ColorTop(Some(0xFFFF00)) });
                s.options = Some(s.opts);
                s.lists = Some((0, 3));
            }
        }
        frames(&ctx, &mut app, 2);
        // region A to Z, then yellow amounts on top within a region
        let rg = Range::new(0, 0, 7, 4);
        let keys = [SortKey::values(0, false), SortKey { line: 2, on: SortOn::CellColor, order: SortOrder::ColorTop(Some(0xFFFF00)) }];
        app.sort_range_by(rg, true, &keys, SortOptions::default());
        let col: Vec<String> = (2..=8).map(|r| txt(&app, &format!("B{r}"))).collect();
        assert_eq!(col, ["Ann", "Cid", "Gus", "Dee", "Fay", "Bob", "Eve"]);
        // left to right by row 2 (keeps column contents together)
        app.sort_range_by(Range::new(0, 1, 7, 2), false, &[SortKey::values(1, false)], SortOptions { left_to_right: true, ..Default::default() });
        assert_eq!(txt(&app, "B1"), "Amount");
        assert_eq!(txt(&app, "C1"), "Rep");
        app.undo();
        assert_eq!(txt(&app, "B1"), "Rep");
    }

    #[test]
    fn text_to_columns_wizard() {
        let (ctx, mut app) = sample_app();
        app.select_range(Range::new(1, 4, 7, 4));
        app.open_text_to_columns();
        for step in 1..=3 {
            if let Some(Dialog::Data(d)) = &mut app.dialog {
                if let DataDlg::TextToColumns(t) = &mut **d {
                    t.step = step;
                    t.fixed = false;
                    t.tab = false;
                    t.comma = true;
                }
            }
            frames(&ctx, &mut app, 2);
        }
        if let Some(Dialog::Data(d)) = &mut app.dialog {
            if let DataDlg::TextToColumns(t) = &mut **d {
                t.fixed = true;
                t.step = 2;
                t.breaks = vec![2, 4];
            }
        }
        frames(&ctx, &mut app, 2);
        let Some(Dialog::Data(d)) = app.dialog.take() else { panic!("wizard closed") };
        let DataDlg::TextToColumns(mut t) = *d else { panic!("not the wizard") };
        t.fixed = false;
        t.comma = true;
        t.tab = false;
        t.sync_formats();
        assert_eq!(t.formats.len(), 3);
        t.formats[1] = ColFormat::Skip;
        t.dest = "G2".into();
        assert!(app.text_to_columns_apply(&t));
        assert_eq!(txt(&app, "G4"), "one");
        assert_eq!(txt(&app, "H4"), "three");
        assert_eq!(txt(&app, "E4"), "one,two,three");
    }

    #[test]
    fn text_to_columns_destination_must_fit_on_the_sheet() {
        let (_ctx, mut app) = sample_app();
        app.select_range(Range::new(1, 4, 7, 4));
        app.open_text_to_columns();
        let Some(Dialog::Data(d)) = app.dialog.take() else { panic!("no wizard") };
        let DataDlg::TextToColumns(mut t) = *d else { panic!("not the wizard") };
        t.fixed = false;
        t.comma = true;
        t.tab = false;
        t.sync_formats();
        let cells = app.sheet().rows.values().map(|r| r.len()).sum::<usize>();
        // Seven lines starting two rows above the bottom, three columns starting at XFC.
        for dest in ["A1048575", "XFC2"] {
            t.dest = dest.into();
            assert!(!app.text_to_columns_apply(&t), "{dest} accepted");
            assert_eq!(app.sheet().rows.values().map(|r| r.len()).sum::<usize>(), cells);
        }
        t.dest = "A1048570".into();
        assert!(app.text_to_columns_apply(&t));
        assert_eq!(txt(&app, "A1048576"), "z");
        assert_eq!(txt(&app, "C1048572"), "three");
    }

    #[test]
    fn subtotals_with_outline() {
        let (ctx, mut app) = sample_app();
        app.sort_simple(false);
        app.open_subtotal();
        frames(&ctx, &mut app, 2);
        app.dialog = None;
        app.insert_subtotals(Range::new(0, 0, 7, 4), 0, 0, &[2], true, true, true);
        // East x3, North, South, West x2 -> 4 subtotal rows + grand total
        assert_eq!(txt(&app, "A5"), "East Total");
        assert_eq!(txt(&app, "C5"), "710");
        assert_eq!(txt(&app, "A13"), "Grand Total");
        assert_eq!(txt(&app, "C13"), "1755");
        assert_eq!(app.sheet().row_levels.get(&4), Some(&1));
        assert_eq!(app.sheet().row_levels.get(&1), Some(&2));
        // replacing keeps one set of subtotals
        assert_eq!(app.sheet().page.row_breaks.iter().copied().collect::<Vec<_>>(), vec![5, 7, 9]);
        app.insert_subtotals(Range::new(0, 0, 12, 4), 0, 1, &[2], true, true, false);
        assert_eq!(txt(&app, "A5"), "East Count");
        assert_eq!(txt(&app, "A13"), "Grand Count");
        assert_eq!(txt(&app, "C13"), "7");
        app.remove_all_subtotals(Range::new(0, 0, 12, 4));
        assert_eq!(txt(&app, "A9"), "");
        assert!(app.sheet().row_levels.is_empty());
    }
}
