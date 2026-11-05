//! Review > Protect Sheet / Unprotect Sheet, the Format Cells Protection tab, the guard that
//! keeps locked cells of a protected sheet unchanged, and Name Manager scopes.

use eframe::egui::{self, Align2, RichText, Vec2};

use super::theme;
use super::{App, Scope};
use crate::model::{legacy_password_hash, Protection, Range, Style};

#[derive(Default)]
pub struct ReviewUi {
    dialog: Option<ProtectDialog>,
    /// Scope for new names in the Name Manager: 0 = workbook, k = sheet k - 1.
    pub name_scope: usize,
}

struct ProtectDialog {
    /// true = Protect Sheet, false = Unprotect Sheet (asks for the password).
    protect: bool,
    password: String,
    confirm: String,
    opts: Protection,
    error: Option<String>,
}

const LOCKED_MSG: &str = "The cell or chart you're trying to change is on a protected sheet. To make a change, unprotect the sheet. You might be requested to enter a password.";

impl App {
    pub fn sheet_protected(&self) -> bool {
        self.sheet().protection.is_some()
    }

    /// True (after telling the user) when the range holds a locked cell of a protected sheet.
    pub fn locked_block(&mut self, rg: Range) -> bool {
        if self.pivot_block(rg) {
            return true;
        }
        let sh = self.sheet();
        if sh.protection.is_none() {
            return false;
        }
        let big = rg.rows() as u64 * rg.cols() as u64 > 100_000;
        let locked = big || (rg.r1..=rg.r2).any(|r| (rg.c1..=rg.c2).any(|c| sh.is_locked(&self.wb, r, c)));
        if locked {
            self.dialog = Some(super::dialogs::Dialog::Message { title: "FreedomOffice Sheets".into(), text: LOCKED_MSG.into() });
        }
        locked
    }

    /// True (after telling the user) when the protected sheet doesn't allow formatting cells.
    pub fn format_block(&mut self) -> bool {
        let allowed = self.sheet().protection.as_ref().is_none_or(|p| p.format_cells);
        if !allowed {
            self.dialog = Some(super::dialogs::Dialog::Message { title: "FreedomOffice Sheets".into(), text: LOCKED_MSG.into() });
        }
        !allowed
    }

    pub fn open_protect_dialog(&mut self) {
        let _ = self.commit_edit(false);
        let protect = !self.sheet_protected();
        self.review.dialog = Some(ProtectDialog {
            protect,
            password: String::new(),
            confirm: String::new(),
            opts: self.sheet().protection.clone().unwrap_or_default(),
            error: None,
        });
    }

    /// Review > Protect group.
    pub(super) fn protect_group(&mut self, ui: &mut egui::Ui) {
        self.group(ui, "Protect", |app, ui| {
            let (label, tip) = if app.sheet_protected() {
                ("Unprotect\nSheet", "Allow changes to the locked cells of this sheet")
            } else {
                ("Protect\nSheet", "Prevent unwanted changes by limiting what can be edited")
            };
            let job = super::ribbon::icon_job("🔒", false, label, 20.0, true, theme::text());
            if super::ribbon::big_btn(ui, job, 60.0, app.sheet_protected()).on_hover_text(tip).clicked() {
                app.open_protect_dialog();
            }
        });
    }

    pub(super) fn protect_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.review.dialog.take() else { return };
        let mut keep = true;
        let title = if d.protect { "Protect Sheet" } else { "Unprotect Sheet" };
        egui::Window::new(title).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, -40.0]).show(ctx, |ui| {
            ui.set_min_width(320.0);
            ui.label(if d.protect { "Password to unprotect sheet (optional):" } else { "Password:" });
            ui.add(egui::TextEdit::singleline(&mut d.password).password(true).desired_width(300.0));
            if d.protect {
                if !d.password.is_empty() {
                    ui.label("Reenter password to proceed:");
                    ui.add(egui::TextEdit::singleline(&mut d.confirm).password(true).desired_width(300.0));
                }
                ui.add_space(4.0);
                ui.label(RichText::new("Allow all users of this worksheet to:").strong());
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                    let o = &mut d.opts;
                    for (label, v) in [
                        ("Select locked cells", &mut o.select_locked),
                        ("Select unlocked cells", &mut o.select_unlocked),
                        ("Format cells", &mut o.format_cells),
                        ("Format columns", &mut o.format_columns),
                        ("Format rows", &mut o.format_rows),
                        ("Insert columns", &mut o.insert_columns),
                        ("Insert rows", &mut o.insert_rows),
                        ("Insert hyperlinks", &mut o.insert_links),
                        ("Delete columns", &mut o.delete_columns),
                        ("Delete rows", &mut o.delete_rows),
                        ("Sort", &mut o.sort),
                        ("Use AutoFilter", &mut o.autofilter),
                        ("Use PivotTable & PivotChart", &mut o.pivot_tables),
                        ("Edit objects", &mut o.edit_objects),
                        ("Edit scenarios", &mut o.edit_scenarios),
                    ] {
                        ui.checkbox(v, label);
                    }
                });
            }
            if let Some(e) = &d.error {
                ui.colored_label(egui::Color32::from_rgb(0xC0, 0, 0), e.as_str());
            }
            ui.add_space(6.0);
            let mut ok = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let mut cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                cancel |= ui.add(egui::Button::new("Cancel").min_size(Vec2::new(76.0, 24.0))).clicked();
                ok |= ui.add(egui::Button::new("OK").min_size(Vec2::new(76.0, 24.0)).fill(theme::accent_light())).clicked();
            });
            if cancel {
                keep = false;
            } else if ok {
                let si = self.wb.active;
                if d.protect {
                    if !d.password.is_empty() && d.password != d.confirm {
                        d.error = Some("Confirmation password is not identical.".into());
                        return;
                    }
                    let mut p = d.opts.clone();
                    p.hash = legacy_password_hash(&d.password);
                    self.push_undo(&[Scope::Sheet(si)]);
                    self.wb.sheets[si].protection = Some(p);
                    self.dirty = true;
                    keep = false;
                } else {
                    let hash = self.wb.sheets[si].protection.as_ref().map_or(0, |p| p.hash);
                    if hash != 0 && legacy_password_hash(&d.password) != hash {
                        d.error = Some("The password you supplied is not correct.".into());
                        return;
                    }
                    self.push_undo(&[Scope::Sheet(si)]);
                    self.wb.sheets[si].protection = None;
                    self.dirty = true;
                    keep = false;
                }
            }
        });
        if keep {
            self.review.dialog = Some(d);
        }
    }

    /// Name Manager list with scopes; returns nothing, deletes through undo.
    pub(super) fn name_list_ui(&mut self, ui: &mut egui::Ui) {
        let mut del: Option<(Option<usize>, usize)> = None;
        egui::Grid::new("names").striped(true).num_columns(4).show(ui, |ui| {
            ui.label(RichText::new("Name").strong());
            ui.label(RichText::new("Refers To").strong());
            ui.label(RichText::new("Scope").strong());
            ui.label("");
            ui.end_row();
            for (i, (n, f)) in self.wb.names.iter().enumerate() {
                ui.label(n);
                ui.label(format!("={f}"));
                ui.label("Workbook");
                if ui.small_button("Delete").clicked() {
                    del = Some((None, i));
                }
                ui.end_row();
            }
            for (s, sh) in self.wb.sheets.iter().enumerate() {
                for (i, (n, f)) in sh.names.iter().enumerate() {
                    ui.label(n);
                    ui.label(format!("={f}"));
                    ui.label(&sh.name);
                    if ui.small_button("Delete").clicked() {
                        del = Some((Some(s), i));
                    }
                    ui.end_row();
                }
            }
        });
        if let Some((scope, i)) = del {
            self.push_undo(&[Scope::All]);
            match scope {
                None => {
                    self.wb.names.remove(i);
                }
                Some(s) => {
                    self.wb.sheets[s].names.remove(i);
                }
            }
            self.changed();
        }
    }

    /// Scope picker for a new name.
    pub(super) fn name_scope_ui(&mut self, ui: &mut egui::Ui) {
        let n = self.wb.sheets.len();
        if self.review.name_scope > n {
            self.review.name_scope = 0;
        }
        let label = |k: usize| if k == 0 { "Workbook".to_string() } else { self.wb.sheets[k - 1].name.clone() };
        let cur = label(self.review.name_scope);
        let mut pick = None;
        egui::ComboBox::from_id_salt("name_scope").selected_text(cur).width(300.0).show_ui(ui, |ui| {
            for k in 0..=n {
                if ui.selectable_label(k == self.review.name_scope, label(k)).clicked() {
                    pick = Some(k);
                }
            }
        });
        if let Some(k) = pick {
            self.review.name_scope = k;
        }
    }

    /// Adds a name in the chosen scope (sheet-scoped names live on their sheet).
    pub fn new_scoped_name(&mut self, name: &str, refers: &str) -> Result<(), String> {
        let scope = self.review.name_scope;
        if scope == 0 || scope > self.wb.sheets.len() {
            return self.new_name(name, refers);
        }
        let s = scope - 1;
        let n = name.trim();
        if !crate::table::valid_name(n) {
            return Err("The name that you entered is not valid.".into());
        }
        let f = refers.trim().trim_start_matches('=').to_string();
        if crate::formula::parse(&f).is_err() {
            return Err("The formula in 'Refers to' contains an error.".into());
        }
        self.push_undo(&[Scope::All]);
        let names = &mut self.wb.sheets[s].names;
        names.retain(|(k, _)| !k.eq_ignore_ascii_case(n));
        names.push((n.to_string(), f));
        self.changed();
        Ok(())
    }
}

/// Format Cells > Protection.
pub(super) fn protection_tab(ui: &mut egui::Ui, st: &mut Style) {
    let mut locked = !st.unlocked;
    if ui.checkbox(&mut locked, "Locked").changed() {
        st.unlocked = !locked;
    }
    ui.checkbox(&mut st.hide_formula, "Hidden");
    ui.add_space(8.0);
    ui.label(
        RichText::new("Locking cells or hiding formulas has no effect until you protect the worksheet (Review tab, Protect group, Protect Sheet button).")
            .size(12.0)
            .color(theme::text_dim()),
    );
}
