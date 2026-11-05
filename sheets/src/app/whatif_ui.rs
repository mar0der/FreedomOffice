//! Data > What-If Analysis (Goal Seek, Scenario Manager, Data Table) and the dialogs of the
//! analysis tools. Reference boxes work like Excel's: while one has been focused last,
//! selecting cells on the sheet types their address into it.

use eframe::egui::{self, Align2, Rect, RichText, Stroke, Vec2};

use super::dialogs::Dialog;
use super::theme;
use super::{App, Scope};
use crate::model::{cell_name, col_name, Cell, Range, Value};
use crate::whatif::{self, CellRef, Scenario};

/// Dialogs of the analysis tools (one `Dialog::WhatIf` variant keeps dialogs.rs small).
pub enum WhatIfDlg {
    GoalSeek(GoalSeekDlg),
    GoalStatus(GoalStatusDlg),
    DataTable(DataTableDlg),
    Scenarios(Box<ScenarioDlg>),
    Solver(Box<super::solver_ui::SolverDlg>),
    SolverResults(Box<super::solver_ui::ResultsDlg>),
    Sparklines(super::sparkline_ui::CreateDlg),
    SparkAxis(super::sparkline_ui::AxisDlg),
    SparkHidden(super::sparkline_ui::HiddenDlg),
}

pub(super) fn window(title: &str) -> egui::Window<'static> {
    egui::Window::new(title.to_string()).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, -60.0])
}

pub(super) fn message(app: &mut App, title: &str, text: &str) {
    app.dialog = Some(Dialog::Message { title: title.into(), text: text.into() });
}

/// Where the sheet selection was when a reference box last looked; changes are typed into
/// the reference box focused last.
#[derive(Clone, Copy, PartialEq)]
pub struct Seen(usize, Range, usize);

impl App {
    pub(super) fn seen(&self) -> Seen {
        Seen(self.wb.active, self.sel(), self.sheet().view.extra.len())
    }

    /// Absolute address of the current selection (all areas), with the sheet name when the
    /// dialog belongs to another sheet.
    pub(super) fn sel_ref_text(&self, home: usize) -> String {
        let v = &self.sheet().view;
        let areas: Vec<Range> = v.extra.iter().copied().chain(std::iter::once(v.sel)).collect();
        let body: Vec<String> = areas.iter().map(|r| crate::solver::abs(*r)).collect();
        let body = body.join(",");
        if self.wb.active == home { body } else { format!("{}!{}", crate::formula::quote_sheet(&self.sheet().name), body) }
    }

    /// Types the selection into `field` when the user selected cells since the last frame.
    pub(super) fn pick_ref(&self, seen: &mut Seen, home: usize, field: Option<&mut String>) {
        let now = self.seen();
        if now != *seen {
            *seen = now;
            if let Some(f) = field {
                *f = self.sel_ref_text(home);
            }
        }
    }
}

/// A reference box; remembers that it was focused last.
pub(super) fn ref_box(ui: &mut egui::Ui, text: &mut String, id: usize, focus: &mut usize, width: f32) -> egui::Response {
    let r = ui.add(egui::TextEdit::singleline(text).desired_width(width));
    if r.has_focus() || r.gained_focus() {
        *focus = id;
    }
    r
}

pub(super) fn buttons(ui: &mut egui::Ui, labels: &[&str]) -> Option<usize> {
    let mut out = None;
    ui.add_space(6.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        for (i, l) in labels.iter().enumerate().rev() {
            let mut b = egui::Button::new(*l).min_size(Vec2::new(76.0, 24.0));
            if i == 0 {
                b = b.fill(theme::accent_light());
            }
            if ui.add(b).clicked() {
                out = Some(i);
            }
        }
    });
    out
}

/// Ribbon icon of the analysis tools: a small grid with a symbol on the highlighted cell.
fn paint_tool(p: &egui::Painter, r: Rect, sym: &str) {
    let g = r.shrink2(Vec2::new(r.width() * 0.18, r.height() * 0.18));
    let line = Stroke::new(1.0, theme::icon_line());
    p.rect_stroke(g, 1.0, line, egui::StrokeKind::Inside);
    for k in 1..3 {
        let x = g.left() + g.width() * k as f32 / 3.0;
        let y = g.top() + g.height() * k as f32 / 3.0;
        p.line_segment([egui::pos2(x, g.top()), egui::pos2(x, g.bottom())], line);
        p.line_segment([egui::pos2(g.left(), y), egui::pos2(g.right(), y)], line);
    }
    let cell = Rect::from_min_size(egui::pos2(g.left() + g.width() * 2.0 / 3.0, g.top() + g.height() * 2.0 / 3.0), g.size() / 3.0);
    p.rect_filled(cell.shrink(0.5), 0.0, theme::accent());
    p.text(r.left_top() + Vec2::new(r.width() * 0.32, r.height() * 0.3), Align2::CENTER_CENTER, sym, theme::ui_bold(14.0), theme::accent_dark());
}

fn keys(ui: &egui::Ui) -> (bool, bool) {
    ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)))
}

// ---------------------------------------------------------------------------
// Goal Seek
// ---------------------------------------------------------------------------

pub struct GoalSeekDlg {
    sheet: usize,
    set: String,
    to: String,
    changing: String,
    focus: usize,
    seen: Seen,
    first: bool,
}

pub struct GoalStatusDlg {
    set: CellRef,
    changing: CellRef,
    target: f64,
    res: whatif::GoalSeek,
    original: Option<Cell>,
    solved: Option<Cell>,
}

// ---------------------------------------------------------------------------
// Data Table
// ---------------------------------------------------------------------------

pub struct DataTableDlg {
    sheet: usize,
    sel: Range,
    row: String,
    col: String,
    focus: usize,
    seen: Seen,
}

// ---------------------------------------------------------------------------
// Scenario Manager
// ---------------------------------------------------------------------------

pub struct ScenarioEdit {
    index: Option<usize>,
    name: String,
    cells: String,
    comment: String,
    locked: bool,
    hidden: bool,
    /// Second step: (cells, values).
    values: Option<(Vec<(u32, u32)>, Vec<String>)>,
    focus: usize,
    error: Option<String>,
}

pub struct ScenarioDlg {
    sheet: usize,
    selected: Option<usize>,
    edit: Option<ScenarioEdit>,
    /// Scenario Summary: (scenario summary = true / pivot = false, result cells).
    summary: Option<(bool, String)>,
    focus: usize,
    seen: Seen,
}

fn user_name() -> String {
    std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "User".into())
}

fn today() -> String {
    crate::numfmt::format_value(&Value::Number(crate::numfmt::now_serial().floor()), "m/d/yyyy").text
}

/// Changing cells typed into the scenario dialog ("$B$1:$B$3,$D$5").
fn parse_cells(app: &App, si: usize, text: &str) -> Result<Vec<(u32, u32)>, String> {
    let mut out = Vec::new();
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (s, rg) = whatif::parse_range_ref(&app.wb, part).ok_or("Reference is not valid.")?;
        if s != si {
            return Err("Changing cells must be on the sheet that contains the scenario.".into());
        }
        for r in rg.r1..=rg.r2 {
            for c in rg.c1..=rg.c2 {
                if !out.contains(&(r, c)) {
                    out.push((r, c));
                }
                if out.len() > 32 {
                    return Err("Too many changing cells. A scenario can have at most 32 changing cells.".into());
                }
            }
        }
    }
    if out.is_empty() {
        return Err("Reference is not valid.".into());
    }
    Ok(out)
}

fn cells_text(cells: &[(u32, u32)]) -> String {
    cells.iter().map(|(r, c)| format!("${}${}", col_name(*c), r + 1)).collect::<Vec<_>>().join(",")
}

impl App {
    // ------------------------------------------------------------------
    // Ribbon
    // ------------------------------------------------------------------

    /// Data > Forecast group: What-If Analysis.
    pub(super) fn forecast_group(&mut self, ui: &mut egui::Ui) {
        self.group(ui, "Forecast", |app, ui| {
            let r = super::chart_view::big_button(ui, "What-If\nAnalysis", "What-If Analysis\nTry out various values for the formulas in the sheet.", |p, r| paint_tool(p, r, "?"));
            egui::Popup::menu(&r).show(|ui| {
                ui.set_min_width(180.0);
                if ui.button("Scenario Manager...").clicked() {
                    app.open_scenarios();
                    ui.close();
                }
                if ui.button("Goal Seek...").clicked() {
                    app.open_goal_seek();
                    ui.close();
                }
                if ui.button("Data Table...").clicked() {
                    app.open_data_table();
                    ui.close();
                }
            });
        });
    }

    /// Data > Analyze group: Solver.
    pub(super) fn analyze_group(&mut self, ui: &mut egui::Ui) {
        self.group(ui, "Analyze", |app, ui| {
            if super::chart_view::big_button(ui, "Solver", "Solver\nFind the best values for the variable cells of a model.", |p, r| paint_tool(p, r, "=")).clicked() {
                app.open_solver();
            }
        });
    }

    pub fn whatif_dialog_ui(&mut self, ctx: &egui::Context, d: &mut WhatIfDlg) -> bool {
        match d {
            WhatIfDlg::GoalSeek(g) => self.goal_seek_ui(ctx, g),
            WhatIfDlg::GoalStatus(g) => self.goal_status_ui(ctx, g),
            WhatIfDlg::DataTable(t) => self.data_table_ui(ctx, t),
            WhatIfDlg::Scenarios(s) => self.scenarios_ui(ctx, s),
            WhatIfDlg::Solver(s) => self.solver_ui(ctx, s),
            WhatIfDlg::SolverResults(r) => self.solver_results_ui(ctx, r),
            WhatIfDlg::Sparklines(s) => self.create_sparklines_ui(ctx, s),
            WhatIfDlg::SparkAxis(a) => self.spark_axis_ui(ctx, a),
            WhatIfDlg::SparkHidden(h) => self.spark_hidden_ui(ctx, h),
        }
    }

    // ------------------------------------------------------------------
    // Goal Seek
    // ------------------------------------------------------------------

    pub fn open_goal_seek(&mut self) {
        self.commit_edit(false);
        let (r, c) = self.active();
        self.dialog = Some(Dialog::WhatIf(Box::new(WhatIfDlg::GoalSeek(GoalSeekDlg {
            sheet: self.wb.active,
            set: format!("${}${}", col_name(c), r + 1),
            to: String::new(),
            changing: String::new(),
            focus: 0,
            seen: self.seen(),
            first: true,
        }))));
    }

    fn goal_seek_ui(&mut self, ctx: &egui::Context, g: &mut GoalSeekDlg) -> bool {
        let field = match g.focus {
            0 => Some(&mut g.set),
            2 => Some(&mut g.changing),
            _ => None,
        };
        self.pick_ref(&mut g.seen, g.sheet, field);
        let mut result = None;
        window("Goal Seek").show(ctx, |ui| {
            egui::Grid::new("goal_seek").num_columns(2).spacing(Vec2::new(8.0, 6.0)).show(ui, |ui| {
                ui.label("Set cell:");
                let r = ref_box(ui, &mut g.set, 0, &mut g.focus, 160.0);
                if g.first {
                    r.request_focus();
                    g.first = false;
                }
                ui.end_row();
                ui.label("To value:");
                let r = ui.add(egui::TextEdit::singleline(&mut g.to).desired_width(160.0));
                if r.gained_focus() {
                    g.focus = 1;
                }
                ui.end_row();
                ui.label("By changing cell:");
                ref_box(ui, &mut g.changing, 2, &mut g.focus, 160.0);
                ui.end_row();
            });
            result = buttons(ui, &["OK", "Cancel"]);
            let (enter, esc) = keys(ui);
            if enter {
                result = Some(0);
            } else if esc {
                result = Some(1);
            }
        });
        match result {
            Some(0) => {
                self.run_goal_seek(g);
                false
            }
            Some(_) => false,
            None => true,
        }
    }

    fn run_goal_seek(&mut self, g: &GoalSeekDlg) {
        let title = "Goal Seek";
        let Some(set) = whatif::parse_cell_ref(&self.wb, &g.set) else {
            return message(self, title, "Reference must be to a single cell.");
        };
        if self.wb.sheets[set.0].get(set.1, set.2).is_none_or(|c| c.formula.is_none()) {
            return message(self, title, "Cell must contain a formula.");
        }
        let target = match crate::numfmt::parse_input(g.to.trim()).0 {
            Value::Number(n) => n,
            _ => return message(self, title, "Invalid number."),
        };
        let Some(changing) = whatif::parse_cell_ref(&self.wb, &g.changing) else {
            return message(self, title, "Reference must be to a single cell.");
        };
        if self.wb.sheets[changing.0].get(changing.1, changing.2).is_some_and(|c| c.formula.is_some()) {
            return message(self, title, "Cell must contain a value.");
        }
        if self.wb.sheets[changing.0].protection.is_some() && self.wb.sheets[changing.0].is_locked(&self.wb, changing.1, changing.2) {
            return message(self, title, "The cell you're trying to change is on a protected sheet.");
        }
        let original = self.wb.sheets[changing.0].get(changing.1, changing.2).cloned();
        let res = whatif::goal_seek(&mut self.wb, set, target, changing, 100, 0.001);
        let solved = self.wb.sheets[changing.0].get(changing.1, changing.2).cloned();
        self.generation += 1;
        self.cf.invalidate();
        self.dialog = Some(Dialog::WhatIf(Box::new(WhatIfDlg::GoalStatus(GoalStatusDlg { set, changing, target, res, original, solved }))));
    }

    fn goal_status_ui(&mut self, ctx: &egui::Context, g: &mut GoalStatusDlg) -> bool {
        let mut result = None;
        let addr = cell_name(g.set.1, g.set.2);
        let fmt = self.wb.sheets[g.set.0].get(g.set.1, g.set.2).map(|c| self.wb.style(c.style).num_fmt.clone()).unwrap_or_default();
        let current = crate::numfmt::format_value(self.wb.sheets[g.set.0].value(g.set.1, g.set.2), &fmt).text;
        window("Goal Seek Status").show(ctx, |ui| {
            ui.set_min_width(300.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(format!("Goal Seeking with Cell {addr}"));
                    ui.label(if g.res.found { "found a solution." } else { "may not have found a solution." });
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    ui.vertical(|ui| {
                        ui.add_enabled(false, egui::Button::new("Step").min_size(Vec2::new(70.0, 22.0)));
                        ui.add_enabled(false, egui::Button::new("Pause").min_size(Vec2::new(70.0, 22.0)));
                    });
                });
            });
            ui.add_space(6.0);
            egui::Grid::new("goal_status").num_columns(2).spacing(Vec2::new(16.0, 4.0)).show(ui, |ui| {
                ui.label("Target value:");
                ui.label(crate::numfmt::general(g.target));
                ui.end_row();
                ui.label("Current value:");
                ui.label(current);
                ui.end_row();
            });
            result = buttons(ui, &["OK", "Cancel"]);
            let (enter, esc) = keys(ui);
            if enter {
                result = Some(0);
            } else if esc {
                result = Some(1);
            }
        });
        let (s, r, c) = g.changing;
        match result {
            Some(0) => {
                // Keep: record the original for undo, then put the solution back.
                self.wb.sheets[s].set(r, c, g.original.clone());
                self.push_undo(&[Scope::Range(s, Range::cell(r, c))]);
                self.wb.sheets[s].set(r, c, g.solved.clone());
                self.changed_ranges(s, &[Range::cell(r, c)]);
                self.cf.invalidate();
                false
            }
            Some(_) => {
                self.wb.sheets[s].set(r, c, g.original.clone());
                self.changed_ranges(s, &[Range::cell(r, c)]);
                self.cf.invalidate();
                false
            }
            None => true,
        }
    }

    // ------------------------------------------------------------------
    // Data Table
    // ------------------------------------------------------------------

    pub fn open_data_table(&mut self) {
        self.commit_edit(false);
        let sel = self.sel();
        if sel.rows() < 2 || sel.cols() < 2 {
            return message(self, "Data Table", "Select the data table range first: the formula and the input values in the first row and column, with the cells for the results.");
        }
        self.dialog = Some(Dialog::WhatIf(Box::new(WhatIfDlg::DataTable(DataTableDlg {
            sheet: self.wb.active,
            sel,
            row: String::new(),
            col: String::new(),
            focus: 0,
            seen: self.seen(),
        }))));
    }

    fn data_table_ui(&mut self, ctx: &egui::Context, t: &mut DataTableDlg) -> bool {
        let field = if t.focus == 0 { &mut t.row } else { &mut t.col };
        self.pick_ref(&mut t.seen, t.sheet, Some(field));
        let mut result = None;
        window("Data Table").show(ctx, |ui| {
            egui::Grid::new("data_table").num_columns(2).spacing(Vec2::new(8.0, 6.0)).show(ui, |ui| {
                ui.label("Row input cell:");
                ref_box(ui, &mut t.row, 0, &mut t.focus, 150.0);
                ui.end_row();
                ui.label("Column input cell:");
                ref_box(ui, &mut t.col, 1, &mut t.focus, 150.0);
                ui.end_row();
            });
            result = buttons(ui, &["OK", "Cancel"]);
            let (enter, esc) = keys(ui);
            if enter {
                result = Some(0);
            } else if esc {
                result = Some(1);
            }
        });
        match result {
            Some(0) => {
                let parse = |app: &App, s: &str| -> Result<Option<(u32, u32)>, ()> {
                    if s.trim().is_empty() {
                        return Ok(None);
                    }
                    match whatif::parse_cell_ref(&app.wb, s) {
                        Some((sh, r, c)) if sh == t.sheet => Ok(Some((r, c))),
                        _ => Err(()),
                    }
                };
                let (Ok(row), Ok(col)) = (parse(self, &t.row), parse(self, &t.col)) else {
                    message(self, "Data Table", "Input cell reference is not valid.");
                    return false;
                };
                // Restore the selection the dialog was opened for (picking input cells moved it).
                self.wb.active = t.sheet;
                self.select_range(t.sel);
                let si = t.sheet;
                if self.locked_block(t.sel) {
                    return false;
                }
                self.push_undo(&[Scope::Sheet(si)]);
                match whatif::create_data_table(&mut self.wb.sheets[si], t.sel, row, col) {
                    Ok(()) => self.changed(),
                    Err(e) => {
                        self.undo();
                        message(self, "Data Table", &e);
                    }
                }
                false
            }
            Some(_) => false,
            None => true,
        }
    }

    /// Edits inside a data table are refused, like Excel ("Cannot change part of a data table.").
    pub fn data_table_blocks(&mut self, si: usize, rg: Range) -> bool {
        if self.wb.sheets.get(si).and_then(|sh| whatif::table_touching(sh, rg)).is_some() {
            message(self, "FreedomOffice Sheets", "Cannot change part of a data table.");
            return true;
        }
        false
    }

    // ------------------------------------------------------------------
    // Scenario Manager
    // ------------------------------------------------------------------

    pub fn open_scenarios(&mut self) {
        self.commit_edit(false);
        let si = self.wb.active;
        let n = self.wb.sheets[si].scenarios.list.len();
        self.dialog = Some(Dialog::WhatIf(Box::new(WhatIfDlg::Scenarios(Box::new(ScenarioDlg {
            sheet: si,
            selected: (n > 0).then_some(self.wb.sheets[si].scenarios.current.min(n.saturating_sub(1))),
            edit: None,
            summary: None,
            focus: 0,
            seen: self.seen(),
        })))));
    }

    fn set_scenarios(&mut self, si: usize, f: impl FnOnce(&mut crate::whatif::Scenarios)) {
        self.push_undo(&[Scope::Sheet(si)]);
        f(&mut self.wb.sheets[si].scenarios);
        self.dirty = true;
    }

    fn scenarios_ui(&mut self, ctx: &egui::Context, d: &mut ScenarioDlg) -> bool {
        let si = d.sheet.min(self.wb.sheets.len() - 1);
        if let Some(e) = &mut d.edit {
            if e.values.is_none() {
                self.pick_ref(&mut d.seen, si, Some(&mut e.cells));
            }
            return self.scenario_edit_ui(ctx, d, si);
        }
        if let Some((_, res)) = &mut d.summary {
            self.pick_ref(&mut d.seen, si, Some(res));
            return self.scenario_summary_ui(ctx, d, si);
        }
        d.seen = self.seen();
        let list = self.wb.sheets[si].scenarios.list.clone();
        let mut close = false;
        window("Scenario Manager").show(ctx, |ui| {
            ui.set_min_width(440.0);
            ui.label("Scenarios:");
            ui.horizontal(|ui| {
                egui::Frame::new().stroke(Stroke::new(1.0, theme::border())).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                    ui.set_width(300.0);
                    ui.set_min_height(140.0);
                    if list.is_empty() {
                        ui.label(RichText::new("No Scenarios defined. Choose Add to add scenarios.").color(theme::text_dim()));
                    }
                    egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                        for (i, sc) in list.iter().enumerate() {
                            let r = ui.selectable_label(d.selected == Some(i), &sc.name);
                            if r.clicked() {
                                d.selected = Some(i);
                            }
                            if r.double_clicked() {
                                let sc = sc.clone();
                                self.set_scenarios(si, |s| s.current = i);
                                self.show_scenario_now(si, &sc);
                            }
                        }
                    });
                });
                ui.vertical(|ui| {
                    let b = |ui: &mut egui::Ui, t: &str, on: bool| ui.add_enabled(on, egui::Button::new(t).min_size(Vec2::new(96.0, 24.0))).clicked();
                    if b(ui, "Add...", true) {
                        let (r, c) = self.active();
                        let text = self.sel_ref_text(si);
                        d.edit = Some(ScenarioEdit {
                            index: None,
                            name: String::new(),
                            cells: if text.is_empty() { format!("${}${}", col_name(c), r + 1) } else { text },
                            comment: format!("Created by {} on {}", user_name(), today()),
                            locked: true,
                            hidden: false,
                            values: None,
                            focus: 1,
                            error: None,
                        });
                    }
                    let sel = d.selected.filter(|i| *i < list.len());
                    if b(ui, "Delete", sel.is_some()) {
                        if let Some(i) = sel {
                            self.set_scenarios(si, |s| {
                                s.list.remove(i);
                                s.current = s.current.min(s.list.len().saturating_sub(1));
                            });
                            d.selected = if list.len() > 1 { Some(i.min(list.len() - 2)) } else { None };
                        }
                    }
                    if b(ui, "Edit...", sel.is_some()) {
                        if let Some(i) = sel {
                            let sc = &list[i];
                            d.edit = Some(ScenarioEdit {
                                index: Some(i),
                                name: sc.name.clone(),
                                cells: cells_text(&sc.cells.iter().map(|(p, _)| *p).collect::<Vec<_>>()),
                                comment: format!("{}\r\nModified by {} on {}", sc.comment, user_name(), today()).trim_start().to_string(),
                                locked: sc.locked,
                                hidden: sc.hidden,
                                values: None,
                                focus: 1,
                                error: None,
                            });
                        }
                    }
                    b(ui, "Merge...", false);
                    if b(ui, "Summary...", !list.is_empty()) {
                        let rc = &self.wb.sheets[si].scenarios.result_cells;
                        let text = if rc.is_empty() { self.sel_ref_text(si) } else { rc.iter().map(|r| crate::solver::abs(*r)).collect::<Vec<_>>().join(",") };
                        d.summary = Some((true, text));
                        d.focus = 0;
                    }
                });
            });
            ui.add_space(6.0);
            let sel = d.selected.and_then(|i| list.get(i));
            egui::Grid::new("sc_info").num_columns(2).spacing(Vec2::new(8.0, 4.0)).show(ui, |ui| {
                ui.label("Changing cells:");
                ui.label(sel.map(|s| cells_text(&s.cells.iter().map(|(p, _)| *p).collect::<Vec<_>>())).unwrap_or_default());
                ui.end_row();
                ui.label("Comment:");
                ui.label(sel.map(|s| s.comment.clone()).unwrap_or_default());
                ui.end_row();
            });
            match buttons(ui, &["Show", "Close"]) {
                Some(0) => {
                    if let Some(i) = d.selected.filter(|i| *i < list.len()) {
                        let sc = list[i].clone();
                        self.set_scenarios(si, |s| s.current = i);
                        self.show_scenario_now(si, &sc);
                    }
                }
                Some(_) => close = true,
                None => {}
            }
            if keys(ui).1 {
                close = true;
            }
        });
        !close
    }

    fn show_scenario_now(&mut self, si: usize, sc: &Scenario) {
        whatif::show_scenario(&mut self.wb, si, sc);
        self.generation += 1;
        self.cf.invalidate();
        self.dirty = true;
    }

    fn scenario_edit_ui(&mut self, ctx: &egui::Context, d: &mut ScenarioDlg, si: usize) -> bool {
        let Some(e) = d.edit.as_mut() else { return true };
        let mut result = None;
        if let Some((cells, vals)) = &mut e.values {
            window("Scenario Values").show(ctx, |ui| {
                ui.set_min_width(300.0);
                ui.label("Enter values for each of the changing cells.");
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    egui::Grid::new("sc_vals").num_columns(2).spacing(Vec2::new(8.0, 4.0)).show(ui, |ui| {
                        for (k, (r, c)) in cells.iter().enumerate() {
                            ui.label(format!("{}:  ${}${}", k + 1, col_name(*c), r + 1));
                            ui.add(egui::TextEdit::singleline(&mut vals[k]).desired_width(140.0));
                            ui.end_row();
                        }
                    });
                });
                if let Some(err) = &e.error {
                    ui.colored_label(theme::error_text(), err);
                }
                result = buttons(ui, &["OK", "Cancel", "Add"]).map(|i| i + 10);
                let (enter, esc) = keys(ui);
                if enter {
                    result = Some(10);
                } else if esc {
                    result = Some(11);
                }
            });
        } else {
            let title = if e.index.is_some() { "Edit Scenario" } else { "Add Scenario" };
            window(title).show(ctx, |ui| {
                ui.set_min_width(380.0);
                ui.label("Scenario name:");
                let r = ui.add(egui::TextEdit::singleline(&mut e.name).desired_width(360.0));
                if r.gained_focus() {
                    e.focus = 0;
                }
                ui.label("Changing cells:");
                let mut f = e.focus;
                ref_box(ui, &mut e.cells, 1, &mut f, 360.0);
                e.focus = f;
                ui.label(RichText::new("Ctrl+click cells to select non-adjacent changing cells.").size(11.5).color(theme::text_dim()));
                ui.label("Comment:");
                ui.add(egui::TextEdit::multiline(&mut e.comment).desired_width(360.0).desired_rows(3));
                ui.add_space(4.0);
                ui.label(RichText::new("Protection").strong());
                ui.checkbox(&mut e.locked, "Prevent changes");
                ui.checkbox(&mut e.hidden, "Hide");
                if let Some(err) = &e.error {
                    ui.colored_label(theme::error_text(), err);
                }
                result = buttons(ui, &["OK", "Cancel"]);
                if keys(ui).1 {
                    result = Some(1);
                }
            });
        }
        match result {
            Some(0) => {
                // Names must be unique on the sheet.
                let name = e.name.trim().to_string();
                let list = &self.wb.sheets[si].scenarios.list;
                if name.is_empty() {
                    e.error = Some("Enter a name for the scenario.".into());
                } else if list.iter().enumerate().any(|(i, s)| s.name.eq_ignore_ascii_case(&name) && Some(i) != e.index) {
                    e.error = Some("Scenario name already exists. Choose another name.".into());
                } else {
                    match parse_cells(self, si, &e.cells) {
                        Ok(cells) => {
                            let old = e.index.and_then(|i| list.get(i));
                            let vals = cells
                                .iter()
                                .map(|p| {
                                    old.and_then(|s| s.cells.iter().find(|(q, _)| q == p).map(|(_, v)| v.clone()))
                                        .unwrap_or_else(|| whatif::value_text(self.wb.sheets[si].value(p.0, p.1)))
                                })
                                .collect();
                            e.error = None;
                            e.values = Some((cells, vals));
                        }
                        Err(msg) => e.error = Some(msg),
                    }
                }
                true
            }
            Some(1) | Some(11) => {
                d.edit = None;
                true
            }
            Some(10) | Some(12) => {
                let Some((cells, vals)) = e.values.clone() else { return true };
                let sc = Scenario {
                    name: e.name.trim().to_string(),
                    comment: e.comment.clone(),
                    user: user_name(),
                    locked: e.locked,
                    hidden: e.hidden,
                    cells: cells.into_iter().zip(vals.into_iter().map(|v| v.trim().to_string())).collect(),
                };
                let idx = e.index;
                let add_more = result == Some(12);
                self.set_scenarios(si, |s| match idx {
                    Some(i) if i < s.list.len() => s.list[i] = sc,
                    _ => s.list.push(sc),
                });
                let n = self.wb.sheets[si].scenarios.list.len();
                d.selected = Some(idx.unwrap_or(n - 1));
                if add_more {
                    // "Add" in Scenario Values starts another scenario with the same cells.
                    let cells = e.cells.clone();
                    *e = ScenarioEdit {
                        index: None,
                        name: String::new(),
                        cells,
                        comment: format!("Created by {} on {}", user_name(), today()),
                        locked: true,
                        hidden: false,
                        values: None,
                        focus: 0,
                        error: None,
                    };
                } else {
                    d.edit = None;
                }
                true
            }
            _ => true,
        }
    }

    fn scenario_summary_ui(&mut self, ctx: &egui::Context, d: &mut ScenarioDlg, si: usize) -> bool {
        let Some((summary, res)) = d.summary.as_mut() else { return true };
        let mut result = None;
        window("Scenario Summary").show(ctx, |ui| {
            ui.set_min_width(300.0);
            ui.label(RichText::new("Report type").strong());
            ui.radio_value(summary, true, "Scenario summary");
            ui.add_enabled(false, egui::RadioButton::new(!*summary, "Scenario PivotTable report"));
            ui.label("Result cells:");
            let mut f = 0;
            ref_box(ui, res, 0, &mut f, 280.0);
            result = buttons(ui, &["OK", "Cancel"]);
            let (enter, esc) = keys(ui);
            if enter {
                result = Some(0);
            } else if esc {
                result = Some(1);
            }
        });
        match result {
            Some(0) => {
                let mut cells = Vec::new();
                let mut ranges = Vec::new();
                for part in res.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    match whatif::parse_range_ref(&self.wb, part) {
                        Some((s, rg)) if s == si => {
                            ranges.push(rg);
                            for r in rg.r1..=rg.r2.min(rg.r1 + 200) {
                                for c in rg.c1..=rg.c2.min(rg.c1 + 50) {
                                    cells.push((r, c));
                                }
                            }
                        }
                        _ => {
                            message(self, "Scenario Summary", "Result cells reference is not valid.");
                            return false;
                        }
                    }
                }
                self.push_undo(&[Scope::All]);
                self.wb.sheets[si].scenarios.result_cells = ranges;
                let mut name = "Scenario Summary".to_string();
                let mut k = 2;
                while self.wb.sheet_index(&name).is_some() {
                    name = format!("Scenario Summary {k}");
                    k += 1;
                }
                let sheet = whatif::scenario_summary(&mut self.wb, si, &cells, &name);
                self.wb.sheets.insert(si, sheet);
                self.wb.active = si;
                self.changed();
                false
            }
            Some(_) => {
                d.summary = None;
                true
            }
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{cell_from_input, parse_cell};

    fn put(app: &mut App, a1: &str, text: &str) {
        let (r, c) = parse_cell(a1).unwrap();
        let cell = cell_from_input(&mut app.wb, text, 0);
        app.wb.sheets[0].set(r, c, cell);
    }

    fn num(app: &App, a1: &str) -> f64 {
        let (r, c) = parse_cell(a1).unwrap();
        app.wb.sheets[0].value(r, c).as_number().unwrap_or(f64::NAN)
    }

    fn key(ctx: &egui::Context, app: &mut App, k: egui::Key) {
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
        let _ = ctx.run(input, |ctx| app.dialogs_ui(ctx));
        super::super::data_ui::frames(ctx, app, 1);
    }

    /// Paints the grid and every ribbon tab (incl. the Sparkline tab) with icon sets and sparklines.
    fn paint(ctx: &egui::Context, app: &mut App) {
        for tab in [RibbonTabRef::Data, RibbonTabRef::Insert, RibbonTabRef::Spark, RibbonTabRef::Home] {
            app.tab = tab.tab();
            let _ = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1400.0, 900.0))), ..Default::default() }, |ctx| {
                egui::TopBottomPanel::top("ribbon").show(ctx, |ui| app.ribbon_ui(ui));
                egui::CentralPanel::default().show(ctx, |ui| app.grid_ui(ui));
                app.dialogs_ui(ctx);
            });
        }
    }

    #[derive(Clone, Copy)]
    enum RibbonTabRef {
        Data,
        Insert,
        Spark,
        Home,
    }

    impl RibbonTabRef {
        fn tab(self) -> super::super::RibbonTab {
            match self {
                RibbonTabRef::Data => super::super::RibbonTab::Data,
                RibbonTabRef::Insert => super::super::RibbonTab::Insert,
                RibbonTabRef::Spark => super::super::RibbonTab::Sparkline,
                RibbonTabRef::Home => super::super::RibbonTab::Home,
            }
        }
    }

    #[test]
    fn what_if_dialogs_headless() {
        let ctx = egui::Context::default();
        let mut app = App::with_context(&ctx, 0, None);
        app.dialog = None;
        put(&mut app, "A1", "10000");
        put(&mut app, "A2", "=PMT(5%/12,60,-A1)");
        for r in 1..=5 {
            for c in 2..7 {
                put(&mut app, &format!("{}{}", col_name(c), r), &format!("{}", (r as i64 * 5 + c as i64 * 3) % 9 - 3));
            }
        }
        app.wb.recalc();
        // Goal Seek: OK keeps the solution, undo brings the original back.
        app.open_goal_seek();
        if let Some(Dialog::WhatIf(w)) = &mut app.dialog {
            if let WhatIfDlg::GoalSeek(g) = w.as_mut() {
                g.set = "A2".into();
                g.to = "500".into();
                g.changing = "$A$1".into();
                g.focus = 1;
            }
        }
        key(&ctx, &mut app, egui::Key::Enter);
        assert!(matches!(&app.dialog, Some(Dialog::WhatIf(w)) if matches!(w.as_ref(), WhatIfDlg::GoalStatus(_))));
        assert!((num(&app, "A2") - 500.0).abs() < 0.001);
        key(&ctx, &mut app, egui::Key::Enter);
        assert!(app.dialog.is_none());
        assert!((num(&app, "A2") - 500.0).abs() < 0.001);
        app.undo();
        assert_eq!(num(&app, "A1"), 10000.0);
        // Data Table over G1:H4 with column input A1.
        put(&mut app, "H1", "=A2");
        put(&mut app, "G2", "20000");
        put(&mut app, "G3", "30000");
        app.select_range(Range::parse("G1:H3").unwrap());
        app.open_data_table();
        if let Some(Dialog::WhatIf(w)) = &mut app.dialog {
            if let WhatIfDlg::DataTable(t) = w.as_mut() {
                t.col = "A1".into();
            }
        }
        key(&ctx, &mut app, egui::Key::Enter);
        assert!(app.dialog.is_none(), "data table dialog closed");
        assert!((num(&app, "H3") - 3.0 * num(&app, "A2")).abs() < 1e-6, "{} {}", num(&app, "H3"), num(&app, "A2"));
        // Editing inside it is refused.
        app.select(2, 7, false);
        app.begin_edit(Some("5".into()), super::super::EditMode::Enter);
        assert!(!app.commit_edit(false));
        app.cancel_edit();
        app.dialog = None;
        // Sparklines from C1:G5 into I1:I5.
        app.select_range(Range::parse("C1:G5").unwrap());
        app.open_create_sparklines(crate::sparkline::SparkKind::Column);
        if let Some(Dialog::WhatIf(w)) = &mut app.dialog {
            if let WhatIfDlg::Sparklines(s) = w.as_mut() {
                s.loc = "$I$1:$I$5".into();
            }
        }
        key(&ctx, &mut app, egui::Key::Enter);
        assert!(app.dialog.is_none());
        assert_eq!(app.wb.sheets[0].sparklines.len(), 1);
        assert_eq!(app.wb.sheets[0].sparklines[0].items.len(), 5);
        app.wb.sheets[0].sparklines[0].high = true;
        let mut line = app.wb.sheets[0].sparklines[0].clone();
        line.kind = crate::sparkline::SparkKind::Line;
        line.markers = true;
        line.items = vec![crate::sparkline::Sparkline { loc: (6, 8), data: "Sheet1!C1:G1".into() }];
        let mut win = line.clone();
        win.kind = crate::sparkline::SparkKind::WinLoss;
        win.items = vec![crate::sparkline::Sparkline { loc: (7, 8), data: "Sheet1!C2:G2".into() }];
        app.wb.sheets[0].sparklines.extend([line, win]);
        // Every icon set painted.
        for (k, style) in crate::iconset::IconStyle::ALL.into_iter().enumerate() {
            let rg = Range::new(0, 2 + (k as u32 % 5), 4, 2 + (k as u32 % 5));
            let rule = crate::condfmt::CondFormat::new(vec![rg], crate::condfmt::RuleKind::IconSet(crate::iconset::IconSetRule::new(style)), Default::default());
            app.wb.sheets[0].cond_formats.push(rule);
        }
        app.changed();
        app.select(0, 8, false);
        paint(&ctx, &mut app);
        // Solver on a small LP.
        put(&mut app, "K1", "0");
        put(&mut app, "K2", "0");
        put(&mut app, "K3", "=3*K1+5*K2");
        put(&mut app, "K4", "=3*K1+2*K2");
        app.wb.recalc();
        app.open_solver();
        if let Some(Dialog::WhatIf(w)) = &mut app.dialog {
            if let WhatIfDlg::Solver(s) = w.as_mut() {
                s.objective = "$K$3".into();
                s.vars = "$K$1:$K$2".into();
                s.method = crate::solver::Method::Simplex;
                s.constraints = vec![
                    crate::solver::Constraint { lhs: Range::parse("K4").unwrap(), rel: crate::solver::Rel::Le, rhs: "18".into() },
                    crate::solver::Constraint { lhs: Range::parse("K1").unwrap(), rel: crate::solver::Rel::Le, rhs: "4".into() },
                    crate::solver::Constraint { lhs: Range::parse("K2").unwrap(), rel: crate::solver::Rel::Le, rhs: "6".into() },
                ];
            }
        }
        paint(&ctx, &mut app);
        let si = app.wb.active;
        let m = match &app.dialog {
            Some(Dialog::WhatIf(w)) => match w.as_ref() {
                WhatIfDlg::Solver(s) => s.model_for_test(&app),
                _ => panic!(),
            },
            _ => panic!(),
        };
        app.dialog = None;
        app.run_solver(si, m);
        if let Some(Dialog::WhatIf(w)) = &mut app.dialog {
            if let WhatIfDlg::SolverResults(r) = w.as_mut() {
                r.answer = true;
            }
        }
        key(&ctx, &mut app, egui::Key::Enter);
        assert!(app.dialog.is_none());
        assert_eq!(app.wb.sheets[app.wb.active].name, "Answer Report 1");
        let model = app.wb.sheet_index("Sheet1").unwrap();
        assert!((app.wb.sheets[model].value(2, 10).as_number().unwrap() - 36.0).abs() < 1e-6);
        // Scenario Manager renders with and without scenarios.
        app.wb.active = model;
        app.open_scenarios();
        paint(&ctx, &mut app);
        app.wb.sheets[model].scenarios.list.push(Scenario { name: "One".into(), cells: vec![((0, 10), "1".into())], ..Default::default() });
        app.open_scenarios();
        paint(&ctx, &mut app);
    }
}