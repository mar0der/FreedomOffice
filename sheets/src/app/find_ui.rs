//! Find and Replace: the dialog with its Options >> panel (Within, Search, Look in,
//! Match case, Match entire cell contents, Format), * ? ~ wildcards, Find All results
//! and Replace All.

use eframe::egui::{self, Align2, RichText, Sense, Stroke, Vec2};

use super::theme;
use super::{App, Scope};
use crate::filter::{wildcard_find, wildcard_match};
use crate::model::{cell_from_input, col_name, Style};
use crate::numfmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LookIn {
    #[default]
    Formulas,
    Values,
    Notes,
}

impl LookIn {
    fn label(self) -> &'static str {
        match self {
            LookIn::Formulas => "Formulas",
            LookIn::Values => "Values",
            LookIn::Notes => "Notes",
        }
    }
}

/// One Find All hit.
#[derive(Clone, Debug)]
pub struct Found {
    pub sheet: usize,
    pub r: u32,
    pub c: u32,
    pub sheet_name: String,
    pub name: String,
    pub value: String,
    pub formula: String,
}

#[derive(Default)]
pub struct FindState {
    pub what: String,
    pub with: String,
    pub match_case: bool,
    pub whole: bool,
    /// Within: Workbook (otherwise Sheet).
    pub all_sheets: bool,
    /// Search: By Columns (otherwise By Rows).
    pub by_cols: bool,
    pub look_in: LookIn,
    pub replace: bool,
    pub focus: bool,
    pub options: bool,
    /// Find format / replace format picked from a cell.
    pub fmt: Option<Style>,
    pub with_fmt: Option<Style>,
    pub results: Vec<Found>,
    pub searched: bool,
    pub sort: Option<(usize, bool)>,
    pub picked: Option<usize>,
    pub message: Option<String>,
}

/// Replaces every match of a wildcard pattern (or the whole text when `whole`).
pub fn replace_matches(text: &str, what: &str, with: &str, case: bool, whole: bool) -> String {
    if what.is_empty() {
        return text.to_string();
    }
    if whole {
        return with.to_string();
    }
    let mut out = String::new();
    let mut i = 0;
    while i < text.len() {
        let Some((a, b)) = wildcard_find(what, text, case, i) else { break };
        out.push_str(&text[i..a]);
        out.push_str(with);
        if b == a {
            break;
        }
        i = b;
    }
    out.push_str(&text[i.min(text.len())..]);
    out
}

fn same_format(a: &Style, b: &Style) -> bool {
    a.fill == b.fill
        && a.color == b.color
        && a.bold == b.bold
        && a.italic == b.italic
        && a.underline == b.underline
        && a.strike == b.strike
        && a.num_fmt == b.num_fmt
}

impl App {
    /// The text Find looks at in a cell (None = nothing there to search).
    fn find_hay(&self, si: usize, r: u32, c: u32) -> Option<String> {
        let sh = &self.wb.sheets[si];
        match self.find.look_in {
            LookIn::Notes => sh.notes.get(&(r, c)).cloned(),
            LookIn::Formulas => sh.get(r, c).map(|x| x.input_text()),
            LookIn::Values => {
                let v = sh.display_value(r, c);
                let st = self.wb.style(sh.get(r, c).map(|x| x.style).unwrap_or(0));
                Some(numfmt::format_value(v, &st.num_fmt).text)
            }
        }
    }

    pub(super) fn cell_matches(&self, si: usize, r: u32, c: u32) -> bool {
        let f = &self.find;
        if let Some(fs) = &f.fmt {
            let st = self.wb.style(self.wb.sheets[si].get(r, c).map(|x| x.style).unwrap_or(0));
            if !same_format(st, fs) {
                return false;
            }
            if f.what.is_empty() {
                return true;
            }
        }
        if f.what.is_empty() {
            return false;
        }
        let Some(hay) = self.find_hay(si, r, c) else { return false };
        if f.whole { wildcard_match(&f.what, &hay, f.match_case) } else { wildcard_find(&f.what, &hay, f.match_case, 0).is_some() }
    }

    /// Searchable positions of a sheet in Search order.
    fn find_coords(&self, si: usize) -> Vec<(u32, u32)> {
        let sh = &self.wb.sheets[si];
        let mut v: Vec<(u32, u32)> = if self.find.look_in == LookIn::Notes {
            sh.notes.keys().copied().collect()
        } else {
            let mut v: Vec<(u32, u32)> = sh.rows.iter().flat_map(|(r, row)| row.keys().map(move |c| (*r, *c))).collect();
            if self.find.look_in == LookIn::Values {
                v.extend(sh.spill.cells.keys().copied());
            }
            v
        };
        if self.find.by_cols {
            v.sort_by_key(|&(r, c)| (c, r));
        } else {
            v.sort_unstable();
        }
        v.dedup();
        v
    }

    fn find_sheets(&self) -> Vec<usize> {
        let si0 = self.wb.active;
        if self.find.all_sheets {
            (0..self.wb.sheets.len()).map(|k| (si0 + k) % self.wb.sheets.len()).collect()
        } else {
            vec![si0]
        }
    }

    pub fn find_next(&mut self, backwards: bool) -> bool {
        let order = self.find_sheets();
        let (ar, ac) = self.active();
        let key = |by_cols: bool, (r, c): (u32, u32)| if by_cols { (c, r) } else { (r, c) };
        let by_cols = self.find.by_cols;
        let n = order.len();
        // current sheet after the active cell, other sheets, then wrap round to the current sheet
        for k in 0..=n {
            let si = order[k % n];
            let mut coords = self.find_coords(si);
            if backwards {
                coords.reverse();
            }
            let here = key(by_cols, (ar, ac));
            let list: Vec<(u32, u32)> = match k {
                0 => coords.into_iter().filter(|&p| if backwards { key(by_cols, p) < here } else { key(by_cols, p) > here }).collect(),
                k if k == n => coords.into_iter().filter(|&p| if backwards { key(by_cols, p) >= here } else { key(by_cols, p) <= here }).collect(),
                _ => coords,
            };
            for (r, c) in list {
                if self.cell_matches(si, r, c) {
                    self.wb.active = si;
                    self.select(r, c, false);
                    return true;
                }
            }
        }
        self.find.message = Some("We couldn't find what you were looking for. Click Options for more ways to search.".into());
        false
    }

    pub fn find_all(&mut self) {
        let mut out = Vec::new();
        for si in self.find_sheets() {
            for (r, c) in self.find_coords(si) {
                if !self.cell_matches(si, r, c) {
                    continue;
                }
                let sh = &self.wb.sheets[si];
                let cell = sh.get(r, c);
                let st = self.wb.style(cell.map(|x| x.style).unwrap_or(0));
                let value = numfmt::format_value(sh.display_value(r, c), &st.num_fmt).text;
                let formula = cell.and_then(|x| x.formula.as_ref()).map(|f| format!("={}", f.text)).unwrap_or_default();
                let a1 = format!("${}${}", col_name(c), r + 1);
                let name = self
                    .wb
                    .names
                    .iter()
                    .find(|(_, f)| {
                        let (sn, rf) = f.rsplit_once('!').unwrap_or(("", f.as_str()));
                        rf.replace('$', "") == a1.replace('$', "") && (sn.is_empty() || sn.trim_matches('\'') == sh.name)
                    })
                    .map(|(n, _)| n.clone())
                    .unwrap_or_default();
                out.push(Found { sheet: si, r, c, sheet_name: sh.name.clone(), name, value, formula });
            }
        }
        self.find.results = out;
        self.find.searched = true;
        self.find.sort = None;
        self.find.picked = None;
        if self.find.results.is_empty() {
            self.find.message = Some("We couldn't find what you were looking for. Click Options for more ways to search.".into());
        } else {
            let f = self.find.results[0].clone();
            self.wb.active = f.sheet;
            self.select(f.r, f.c, false);
            self.find.picked = Some(0);
        }
    }

    fn replace_at(&mut self, si: usize, r: u32, c: u32) {
        let Some(cell) = self.wb.sheets[si].get(r, c).cloned() else { return };
        let mut style = cell.style;
        if let Some(wf) = self.find.with_fmt.clone() {
            let mut st = self.wb.style(style).clone();
            st.fill = wf.fill;
            st.color = wf.color;
            st.bold = wf.bold;
            st.italic = wf.italic;
            st.underline = wf.underline;
            st.strike = wf.strike;
            if !wf.num_fmt.is_empty() {
                st.num_fmt = wf.num_fmt.clone();
            }
            style = self.wb.intern_style(st);
        }
        let text = cell.input_text();
        let f = &self.find;
        let new = replace_matches(&text, &f.what, &f.with, f.match_case, f.whole);
        if new == text {
            if style != cell.style {
                self.wb.sheets[si].cell_mut(r, c).style = style;
            }
            return;
        }
        let cell = cell_from_input(&mut self.wb, &new, style);
        self.wb.sheets[si].set(r, c, cell);
    }

    pub fn replace_one(&mut self) {
        let (r, c) = self.active();
        let si = self.wb.active;
        let look = self.find.look_in;
        self.find.look_in = LookIn::Formulas;
        if self.cell_matches(si, r, c) {
            self.push_undo(&[Scope::Range(si, crate::model::Range::cell(r, c))]);
            self.replace_at(si, r, c);
            self.changed();
        }
        self.find_next(false);
        self.find.look_in = look;
    }

    /// Replace All; returns the number of cells changed.
    pub fn replace_all(&mut self) -> usize {
        let look = self.find.look_in;
        self.find.look_in = LookIn::Formulas;
        let mut hits = Vec::new();
        for si in self.find_sheets() {
            for (r, c) in self.find_coords(si) {
                if self.cell_matches(si, r, c) {
                    hits.push((si, r, c));
                }
            }
        }
        if !hits.is_empty() {
            self.push_undo(&[Scope::All]);
            for &(si, r, c) in &hits {
                self.replace_at(si, r, c);
            }
            self.changed();
        }
        self.find.look_in = look;
        hits.len()
    }

    fn format_button(&mut self, ui: &mut egui::Ui, replace: bool) {
        let cur = if replace { self.find.with_fmt.clone() } else { self.find.fmt.clone() };
        let (w, h) = (110.0, 22.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
        let p = ui.painter();
        match &cur {
            None => {
                p.rect_stroke(rect, 2.0, Stroke::new(1.0, theme::border()), egui::StrokeKind::Inside);
                p.text(rect.center(), Align2::CENTER_CENTER, "No Format Set", theme::ui_font(12.0), theme::text_dim());
            }
            Some(st) => {
                p.rect_filled(rect, 2.0, st.fill.map(theme::rgb).unwrap_or_else(theme::window));
                p.rect_stroke(rect, 2.0, Stroke::new(1.0, theme::border()), egui::StrokeKind::Inside);
                let font = if st.bold { theme::ui_bold(12.5) } else { theme::ui_font(12.5) };
                p.text(rect.center(), Align2::CENTER_CENTER, "Preview*", font, st.color.map(theme::rgb).unwrap_or_else(theme::text));
            }
        }
        ui.menu_button("Format... ▾", |ui| {
            if ui.button("Choose Format From Cell").clicked() {
                let (r, c) = self.active();
                let st = self.wb.style(self.sheet().get(r, c).map(|x| x.style).unwrap_or(0)).clone();
                if replace {
                    self.find.with_fmt = Some(st);
                } else {
                    self.find.fmt = Some(st);
                }
                ui.close();
            }
            let label = if replace { "Clear Replace Format" } else { "Clear Find Format" };
            if ui.add_enabled(cur.is_some(), egui::Button::new(label)).clicked() {
                if replace {
                    self.find.with_fmt = None;
                } else {
                    self.find.fmt = None;
                }
                ui.close();
            }
        });
    }

    pub(super) fn find_dialog_ui(&mut self, ctx: &egui::Context) -> bool {
        let mut keep = true;
        let modal = self.find.message.is_some();
        egui::Window::new("Find and Replace")
            .id(egui::Id::new("find_replace"))
            .collapsible(false)
            .resizable(false)
            .default_pos(ctx.content_rect().center() - Vec2::new(260.0, 160.0))
            .show(ctx, |ui| {
                ui.set_width(500.0);
                if modal {
                    ui.disable();
                }
                ui.horizontal(|ui| {
                    if ui.selectable_label(!self.find.replace, " Find ").clicked() {
                        self.find.replace = false;
                    }
                    if ui.selectable_label(self.find.replace, " Replace ").clicked() {
                        self.find.replace = true;
                    }
                });
                ui.separator();
                let opts = self.find.options;
                egui::Grid::new("find_grid").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Find what:");
                    let r = ui.add(egui::TextEdit::singleline(&mut self.find.what).desired_width(if opts { 240.0 } else { 390.0 }));
                    if self.find.focus {
                        r.request_focus();
                        self.find.focus = false;
                    }
                    if opts {
                        ui.horizontal(|ui| self.format_button(ui, false));
                    }
                    ui.end_row();
                    if self.find.replace {
                        ui.label("Replace with:");
                        ui.add(egui::TextEdit::singleline(&mut self.find.with).desired_width(if opts { 240.0 } else { 390.0 }));
                        if opts {
                            ui.horizontal(|ui| self.format_button(ui, true));
                        }
                        ui.end_row();
                    }
                });
                if opts {
                    ui.add_space(4.0);
                    ui.horizontal_top(|ui| {
                        egui::Grid::new("find_opts").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                            ui.label("Within:");
                            egui::ComboBox::from_id_salt("find_within").width(100.0).selected_text(if self.find.all_sheets { "Workbook" } else { "Sheet" }).show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.find.all_sheets, false, "Sheet");
                                ui.selectable_value(&mut self.find.all_sheets, true, "Workbook");
                            });
                            ui.end_row();
                            ui.label("Search:");
                            egui::ComboBox::from_id_salt("find_by").width(100.0).selected_text(if self.find.by_cols { "By Columns" } else { "By Rows" }).show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.find.by_cols, false, "By Rows");
                                ui.selectable_value(&mut self.find.by_cols, true, "By Columns");
                            });
                            ui.end_row();
                            ui.label("Look in:");
                            let replace = self.find.replace;
                            if replace {
                                self.find.look_in = LookIn::Formulas;
                            }
                            egui::ComboBox::from_id_salt("find_look").width(100.0).selected_text(self.find.look_in.label()).show_ui(ui, |ui| {
                                for l in [LookIn::Formulas, LookIn::Values, LookIn::Notes] {
                                    if !replace || l == LookIn::Formulas {
                                        ui.selectable_value(&mut self.find.look_in, l, l.label());
                                    }
                                }
                            });
                            ui.end_row();
                        });
                        ui.add_space(16.0);
                        ui.vertical(|ui| {
                            ui.checkbox(&mut self.find.match_case, "Match case");
                            ui.checkbox(&mut self.find.whole, "Match entire cell contents");
                        });
                    });
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(if opts { "Options <<" } else { "Options >>" }).clicked() {
                        self.find.options = !opts;
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let b = |ui: &mut egui::Ui, t: &str| ui.add(egui::Button::new(t).min_size(Vec2::new(84.0, 24.0))).clicked();
                    if self.find.replace {
                        if b(ui, "Replace All") {
                            let n = self.replace_all();
                            self.find.message = Some(if n == 0 {
                                "We couldn't find anything to replace. Click Options for more ways to search.".into()
                            } else {
                                format!("All done. We made {n} replacement{}.", if n == 1 { "" } else { "s" })
                            });
                        }
                        if b(ui, "Replace") {
                            self.replace_one();
                        }
                    }
                    if b(ui, "Find All") {
                        self.find_all();
                    }
                    let enter = !modal && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if b(ui, "Find Next") || enter {
                        let back = ui.input(|i| i.modifiers.shift);
                        self.find_next(back);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if b(ui, "Close") || (!modal && ui.input(|i| i.key_pressed(egui::Key::Escape))) {
                            keep = false;
                        }
                    });
                });
                if self.find.searched {
                    ui.add_space(6.0);
                    self.find_results_ui(ui);
                }
            });
        if let Some(msg) = self.find.message.clone() {
            egui::Window::new("FreedomOffice Sheets")
                .id(egui::Id::new("find_message"))
                .collapsible(false)
                .resizable(false)
                .order(egui::Order::Foreground)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_min_width(300.0);
                    ui.label(msg);
                    ui.add_space(8.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new("OK").min_size(Vec2::new(76.0, 24.0)).fill(theme::accent_light())).clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape))
                        {
                            self.find.message = None;
                        }
                    });
                });
        }
        keep
    }

    fn find_results_ui(&mut self, ui: &mut egui::Ui) {
        const HEADS: [&str; 6] = ["Book", "Sheet", "Name", "Cell", "Value", "Formula"];
        const W: [f32; 6] = [80.0, 70.0, 60.0, 50.0, 110.0, 120.0];
        let book = self.wb.file_name();
        let mut click_head = None;
        let mut pick = None;
        egui::Frame::new().fill(theme::window()).stroke(Stroke::new(1.0, theme::border())).inner_margin(egui::Margin::same(2)).show(ui, |ui| {
            ui.set_width(496.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (i, h) in HEADS.iter().enumerate() {
                    let arrow = match self.find.sort {
                        Some((k, desc)) if k == i => if desc { " ▼" } else { " ▲" },
                        _ => "",
                    };
                    let r = ui.add_sized([W[i], 20.0], egui::Button::new(RichText::new(format!("{h}{arrow}")).size(12.0)).frame(true));
                    if r.clicked() {
                        click_head = Some(i);
                    }
                }
            });
            egui::ScrollArea::vertical().max_height(140.0).auto_shrink([false, true]).show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(0.0, 0.0);
                for (k, f) in self.find.results.iter().enumerate() {
                    let cols = [book.clone(), f.sheet_name.clone(), f.name.clone(), format!("${}${}", col_name(f.c), f.r + 1), f.value.clone(), f.formula.clone()];
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(W.iter().sum(), 18.0), Sense::click());
                    let p = ui.painter();
                    if self.find.picked == Some(k) {
                        p.rect_filled(rect, 0.0, theme::accent_light());
                    } else if resp.hovered() {
                        p.rect_filled(rect, 0.0, theme::hover());
                    }
                    let mut x = rect.left() + 4.0;
                    for (i, t) in cols.iter().enumerate() {
                        let clip = egui::Rect::from_min_max(egui::Pos2::new(x, rect.top()), egui::Pos2::new(x + W[i] - 6.0, rect.bottom()));
                        p.with_clip_rect(clip).text(egui::Pos2::new(x, rect.center().y), Align2::LEFT_CENTER, t, theme::ui_font(12.0), theme::text());
                        x += W[i];
                    }
                    if resp.clicked() {
                        pick = Some(k);
                    }
                }
            });
        });
        let n = self.find.results.len();
        ui.label(format!("{n} cell(s) found"));
        if let Some(i) = click_head {
            let desc = matches!(self.find.sort, Some((k, d)) if k == i && !d);
            self.find.sort = Some((i, desc));
            let key = |f: &Found| -> (String, f64) {
                match i {
                    0 => (String::new(), 0.0),
                    1 => (f.sheet_name.to_lowercase(), f.sheet as f64),
                    2 => (f.name.to_lowercase(), 0.0),
                    3 => (String::new(), f.c as f64 * 2_000_000.0 + f.r as f64),
                    4 => match numfmt::parse_number(&f.value) {
                        Some(v) => (String::new(), v),
                        None => (f.value.to_lowercase(), f64::MAX),
                    },
                    _ => (f.formula.to_lowercase(), 0.0),
                }
            };
            self.find.results.sort_by(|a, b| {
                let (ka, kb) = (key(a), key(b));
                let o = ka.1.total_cmp(&kb.1).then(ka.0.cmp(&kb.0));
                if desc { o.reverse() } else { o }
            });
            self.find.picked = None;
        }
        if let Some(k) = pick {
            let f = self.find.results[k].clone();
            if f.sheet < self.wb.sheets.len() {
                self.wb.active = f.sheet;
                self.select(f.r, f.c, false);
            }
            self.find.picked = Some(k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::replace_matches;

    #[test]
    fn replace_with_wildcards() {
        assert_eq!(replace_matches("cat hat bat", "?at", "X", false, false), "X X X");
        assert_eq!(replace_matches("Total: 12", "total", "Sum", false, false), "Sum: 12");
        assert_eq!(replace_matches("Total: 12", "total", "Sum", true, false), "Total: 12");
        assert_eq!(replace_matches("a*b", "~*", "x", false, false), "axb");
        assert_eq!(replace_matches("abc", "b*", "Z", false, false), "aZ");
        assert_eq!(replace_matches("abc", "abc", "Z", false, true), "Z");
        assert_eq!(replace_matches("=SUM(A1:A3)", "a1", "B1", false, false), "=SUM(B1:A3)");
    }
}
