//! Data > Solver: the Solver Parameters dialog, Add/Change Constraint and Solver Results.

use eframe::egui::{self, RichText, Stroke, Vec2};

use super::dialogs::Dialog;
use super::theme;
use super::whatif_ui::{buttons, message, ref_box, window, Seen, WhatIfDlg};
use super::{App, Scope};
use crate::model::{Cell, Range};
use crate::solver::{abs, Constraint, Goal, Method, Outcome, Rel, SolverModel};

pub struct ConstraintEdit {
    index: Option<usize>,
    lhs: String,
    rel: Rel,
    rhs: String,
    focus: usize,
    error: Option<String>,
}

pub struct SolverDlg {
    sheet: usize,
    objective: String,
    /// 0 Max, 1 Min, 2 Value Of.
    goal: usize,
    value: String,
    vars: String,
    constraints: Vec<Constraint>,
    selected: Option<usize>,
    nonneg: bool,
    method: Method,
    focus: usize,
    seen: Seen,
    edit: Option<ConstraintEdit>,
    error: Option<String>,
}

pub struct ResultsDlg {
    sheet: usize,
    model: SolverModel,
    out: Outcome,
    original: Vec<((u32, u32), Option<Cell>)>,
    keep: bool,
    answer: bool,
    back: bool,
}

impl SolverDlg {
    fn model(&self, app: &App) -> Result<SolverModel, String> {
        let objective = if self.objective.trim().is_empty() {
            None
        } else {
            match crate::whatif::parse_cell_ref(&app.wb, &self.objective) {
                Some((s, r, c)) if s == self.sheet => Some((r, c)),
                _ => return Err("Objective Cell must be a single cell on the active sheet.".into()),
            }
        };
        let goal = match self.goal {
            0 => Goal::Max,
            1 => Goal::Min,
            _ => match crate::numfmt::parse_input(self.value.trim()).0 {
                crate::model::Value::Number(n) => Goal::Value(n),
                _ => return Err("The Value Of box must contain a number.".into()),
            },
        };
        let mut vars = Vec::new();
        for part in self.vars.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            match crate::whatif::parse_range_ref(&app.wb, part) {
                Some((s, rg)) if s == self.sheet => vars.push(rg),
                _ => return Err("Variable Cells must be on the active sheet.".into()),
            }
        }
        Ok(SolverModel { objective, goal, vars, constraints: self.constraints.clone(), nonneg: self.nonneg, method: self.method })
    }
}

impl App {
    pub fn open_solver(&mut self) {
        self.commit_edit(false);
        let si = self.wb.active;
        let m = SolverModel::from_names(&self.wb.sheets[si].names).unwrap_or_default();
        let (r, c) = self.active();
        self.dialog = Some(Dialog::WhatIf(Box::new(WhatIfDlg::Solver(Box::new(SolverDlg {
            sheet: si,
            objective: m.objective.map(|(r, c)| abs(Range::cell(r, c))).unwrap_or_else(|| abs(Range::cell(r, c))),
            goal: match m.goal {
                Goal::Max => 0,
                Goal::Min => 1,
                Goal::Value(_) => 2,
            },
            value: match m.goal {
                Goal::Value(v) => crate::numfmt::general(v),
                _ => "0".into(),
            },
            vars: m.vars.iter().map(|v| abs(*v)).collect::<Vec<_>>().join(","),
            constraints: m.constraints.clone(),
            selected: None,
            nonneg: m.nonneg,
            method: m.method,
            focus: 0,
            seen: self.seen(),
            edit: None,
            error: None,
        })))));
    }

    /// Stores the model in the sheet's solver_* names (as Excel does when the dialog closes).
    fn save_solver_model(&mut self, si: usize, m: &SolverModel) {
        let names = m.to_names(&self.wb.sheets[si].name);
        let sh = &mut self.wb.sheets[si];
        let mut kept: Vec<(String, String)> = sh.names.iter().filter(|(n, _)| !n.to_ascii_lowercase().starts_with("solver_")).cloned().collect();
        kept.extend(names);
        if kept != sh.names {
            sh.names = kept;
            self.dirty = true;
        }
    }

    pub(super) fn solver_ui(&mut self, ctx: &egui::Context, d: &mut SolverDlg) -> bool {
        if let Some(e) = &mut d.edit {
            let field = if e.focus == 0 { &mut e.lhs } else { &mut e.rhs };
            self.pick_ref(&mut d.seen, d.sheet, Some(field));
            return self.constraint_ui(ctx, d);
        }
        let field = match d.focus {
            0 => Some(&mut d.objective),
            1 => Some(&mut d.vars),
            _ => None,
        };
        self.pick_ref(&mut d.seen, d.sheet, field);
        let mut action: Option<&str> = None;
        window("Solver Parameters").show(ctx, |ui| {
            ui.set_min_width(520.0);
            ui.horizontal(|ui| {
                ui.label("Set Objective:");
                ref_box(ui, &mut d.objective, 0, &mut d.focus, 200.0);
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("To:");
                ui.radio_value(&mut d.goal, 0, "Max");
                ui.radio_value(&mut d.goal, 1, "Min");
                ui.radio_value(&mut d.goal, 2, "Value Of:");
                ui.add_enabled(d.goal == 2, egui::TextEdit::singleline(&mut d.value).desired_width(90.0));
            });
            ui.add_space(4.0);
            ui.label("By Changing Variable Cells:");
            ref_box(ui, &mut d.vars, 1, &mut d.focus, 500.0);
            ui.add_space(4.0);
            ui.label("Subject to the Constraints:");
            ui.horizontal(|ui| {
                egui::Frame::new().stroke(Stroke::new(1.0, theme::border())).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                    ui.set_width(390.0);
                    ui.set_min_height(130.0);
                    egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                        for (i, c) in d.constraints.iter().enumerate() {
                            let r = ui.selectable_label(d.selected == Some(i), c.describe());
                            if r.clicked() {
                                d.selected = Some(i);
                            }
                            if r.double_clicked() {
                                action = Some("change");
                            }
                        }
                    });
                });
                ui.vertical(|ui| {
                    let b = |ui: &mut egui::Ui, t: &str, on: bool| ui.add_enabled(on, egui::Button::new(t).min_size(Vec2::new(90.0, 24.0))).clicked();
                    if b(ui, "Add", true) {
                        action = Some("add");
                    }
                    let sel = d.selected.is_some_and(|i| i < d.constraints.len());
                    if b(ui, "Change", sel) {
                        action = Some("change");
                    }
                    if b(ui, "Delete", sel) {
                        action = Some("delete");
                    }
                    ui.add_space(8.0);
                    if b(ui, "Reset All", true) {
                        action = Some("reset");
                    }
                });
            });
            ui.add_space(4.0);
            ui.checkbox(&mut d.nonneg, "Make Unconstrained Variables Non-Negative");
            ui.horizontal(|ui| {
                ui.label("Select a Solving Method:");
                egui::ComboBox::from_id_salt("solver_method").width(160.0).selected_text(d.method.label()).show_ui(ui, |ui| {
                    for m in [Method::Grg, Method::Simplex, Method::Evolutionary] {
                        ui.selectable_value(&mut d.method, m, m.label());
                    }
                });
            });
            egui::Frame::new().fill(theme::inset()).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                ui.set_width(500.0);
                ui.label(RichText::new("Solving Method").strong());
                ui.label(
                    RichText::new(
                        "Select the GRG Nonlinear engine for Solver Problems that are smooth nonlinear. Select the LP Simplex engine for linear Solver Problems, and select the Evolutionary engine for Solver problems that are non-smooth.",
                    )
                    .size(11.5),
                );
            });
            if let Some(err) = &d.error {
                ui.colored_label(theme::error_text(), err);
            }
            match buttons(ui, &["Solve", "Close"]) {
                Some(0) => action = Some("solve"),
                Some(_) => action = Some("close"),
                None => {}
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                action = Some("close");
            }
        });
        match action {
            Some("add") => {
                d.edit = Some(ConstraintEdit { index: None, lhs: String::new(), rel: Rel::Le, rhs: String::new(), focus: 0, error: None });
                true
            }
            Some("change") => {
                if let Some(i) = d.selected.filter(|i| *i < d.constraints.len()) {
                    let c = &d.constraints[i];
                    d.edit = Some(ConstraintEdit { index: Some(i), lhs: abs(c.lhs), rel: c.rel, rhs: c.rhs.clone(), focus: 0, error: None });
                }
                true
            }
            Some("delete") => {
                if let Some(i) = d.selected.filter(|i| *i < d.constraints.len()) {
                    d.constraints.remove(i);
                    d.selected = None;
                }
                true
            }
            Some("reset") => {
                d.constraints.clear();
                d.objective.clear();
                d.vars.clear();
                d.goal = 0;
                d.nonneg = true;
                d.method = Method::Grg;
                true
            }
            Some("close") => {
                if let Ok(m) = d.model(self) {
                    if m.objective.is_some() || !m.vars.is_empty() {
                        self.push_undo(&[Scope::Sheet(d.sheet)]);
                        self.save_solver_model(d.sheet, &m);
                    }
                }
                false
            }
            Some("solve") => {
                let m = match d.model(self) {
                    Ok(m) => m,
                    Err(e) => {
                        d.error = Some(e);
                        return true;
                    }
                };
                self.run_solver(d.sheet, m);
                false
            }
            _ => true,
        }
    }

    fn constraint_ui(&mut self, ctx: &egui::Context, d: &mut SolverDlg) -> bool {
        let Some(e) = d.edit.as_mut() else { return true };
        let mut result = None;
        let title = if e.index.is_some() { "Change Constraint" } else { "Add Constraint" };
        window(title).show(ctx, |ui| {
            egui::Grid::new("solver_con").num_columns(3).spacing(Vec2::new(8.0, 4.0)).show(ui, |ui| {
                ui.label("Cell Reference:");
                ui.label("");
                ui.label("Constraint:");
                ui.end_row();
                ref_box(ui, &mut e.lhs, 0, &mut e.focus, 150.0);
                egui::ComboBox::from_id_salt("solver_rel").width(56.0).selected_text(e.rel.symbol()).show_ui(ui, |ui| {
                    for r in Rel::ALL {
                        ui.selectable_value(&mut e.rel, r, r.symbol());
                    }
                });
                match e.rel {
                    Rel::Int => {
                        ui.label("integer");
                    }
                    Rel::Bin => {
                        ui.label("binary");
                    }
                    Rel::Dif => {
                        ui.label("AllDifferent");
                    }
                    _ => {
                        ref_box(ui, &mut e.rhs, 1, &mut e.focus, 150.0);
                    }
                }
                ui.end_row();
            });
            if let Some(err) = &e.error {
                ui.colored_label(theme::error_text(), err);
            }
            result = buttons(ui, &["OK", "Add", "Cancel"]);
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                result = Some(2);
            }
        });
        match result {
            Some(2) => {
                d.edit = None;
                true
            }
            Some(k) => {
                let lhs = match crate::whatif::parse_range_ref(&self.wb, &e.lhs) {
                    Some((s, rg)) if s == d.sheet => rg,
                    _ => {
                        e.error = Some("Cell Reference is not valid.".into());
                        return true;
                    }
                };
                let rhs = e.rhs.trim().trim_start_matches('=').to_string();
                if !e.rel.is_type() && (rhs.is_empty() || crate::formula::parse(&rhs).is_err()) {
                    e.error = Some("Constraint must be a number, simple reference, or formula with a numeric value.".into());
                    return true;
                }
                let c = Constraint { lhs, rel: e.rel, rhs: if e.rel.is_type() { String::new() } else { rhs } };
                match e.index {
                    Some(i) if i < d.constraints.len() => d.constraints[i] = c,
                    _ => d.constraints.push(c),
                }
                if k == 1 {
                    *e = ConstraintEdit { index: None, lhs: String::new(), rel: Rel::Le, rhs: String::new(), focus: 0, error: None };
                } else {
                    d.edit = None;
                }
                true
            }
            None => true,
        }
    }

    fn run_solver(&mut self, si: usize, m: SolverModel) {
        // The undo step covers the model names and the variable cells.
        self.push_undo(&[Scope::Sheet(si)]);
        self.save_solver_model(si, &m);
        let mut original = Vec::new();
        for rg in &m.vars {
            for r in rg.r1..=rg.r2.min(rg.r1 + 2000) {
                for c in rg.c1..=rg.c2.min(rg.c1 + 2000) {
                    original.push(((r, c), self.wb.sheets[si].get(r, c).cloned()));
                }
            }
        }
        match crate::solver::solve(&mut self.wb, si, &m) {
            Ok(out) => {
                self.generation += 1;
                self.cf.invalidate();
                self.dialog = Some(Dialog::WhatIf(Box::new(WhatIfDlg::SolverResults(Box::new(ResultsDlg {
                    sheet: si,
                    model: m,
                    keep: out.status.found(),
                    out,
                    original,
                    answer: false,
                    back: false,
                })))));
            }
            Err(e) => message(self, "Solver", &e),
        }
    }

    pub(super) fn solver_results_ui(&mut self, ctx: &egui::Context, d: &mut ResultsDlg) -> bool {
        let mut result = None;
        window("Solver Results").show(ctx, |ui| {
            ui.set_min_width(480.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(300.0);
                    ui.label(RichText::new(&d.out.message).strong());
                    ui.add_space(8.0);
                    ui.radio_value(&mut d.keep, true, "Keep Solver Solution");
                    ui.radio_value(&mut d.keep, false, "Restore Original Values");
                    ui.add_space(8.0);
                    ui.checkbox(&mut d.back, "Return to Solver Parameters Dialog");
                });
                ui.vertical(|ui| {
                    ui.label("Reports");
                    egui::Frame::new().stroke(Stroke::new(1.0, theme::border())).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                        ui.set_width(150.0);
                        ui.set_min_height(70.0);
                        let ok = d.out.status.found();
                        if ui.add_enabled(ok, egui::Button::selectable(d.answer, "Answer")).clicked() {
                            d.answer = !d.answer;
                        }
                    });
                });
            });
            ui.add_space(6.0);
            ui.label(RichText::new(&d.out.detail).size(11.5).color(theme::text_dim()));
            result = buttons(ui, &["OK", "Cancel"]);
            let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
            if enter {
                result = Some(0);
            } else if esc {
                result = Some(1);
            }
        });
        let Some(k) = result else { return true };
        let si = d.sheet;
        let restore = k == 1 || !d.keep;
        // The report is built while the solution is in the cells.
        let report = (k == 0 && d.answer && d.out.status.found()).then(|| {
            let mut n = 1;
            while self.wb.sheet_index(&format!("Answer Report {n}")).is_some() {
                n += 1;
            }
            crate::solver::answer_report(&mut self.wb, si, &d.model, &d.out, &format!("Answer Report {n}"))
        });
        if restore {
            let mut edited = Vec::new();
            for (p, cell) in d.original.drain(..) {
                self.wb.sheets[si].set(p.0, p.1, cell);
                edited.push((si, Range::cell(p.0, p.1)));
            }
            crate::formula::recalc_ranges(&mut self.wb, &edited);
        }
        if let Some(rep) = report {
            self.push_undo(&[Scope::All]);
            self.wb.sheets.insert(si, rep);
            self.wb.active = si;
        }
        self.changed();
        if k == 0 && d.back {
            self.wb.active = si.min(self.wb.sheets.len() - 1);
            self.open_solver();
            return false;
        }
        false
    }
}
