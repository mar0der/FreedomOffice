//! Review > Protect Sheet / Unprotect Sheet, the Format Cells Protection tab, the guard that
//! keeps locked cells of a protected sheet unchanged, and Name Manager scopes.

use eframe::egui::{self, Align2, RichText, Vec2};

use super::theme;
use super::{App, Scope};
use crate::model::{legacy_password_hash, Protection, Range, Sheet, Style, Workbook};

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

/// What a command is about to change on a sheet (see `App::protect_blocks`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Change {
    /// Cell contents in this range: refused when any of its cells is locked.
    Cells(Range),
    /// Cell formatting (needs "Format cells").
    Format,
    /// Row heights, hiding and unhiding rows (needs "Format rows").
    FormatRows,
    /// Column widths, hiding and unhiding columns (needs "Format columns").
    FormatCols,
    InsertRows,
    InsertCols,
    /// Deleting these rows (needs "Delete rows" and no locked cell in them).
    DeleteRows(Range),
    DeleteCols(Range),
    /// Sorting this range (needs "Sort" and no locked cell in it).
    Sort(Range),
    /// A hyperlink on these cells (needs "Insert hyperlinks" and no locked cell).
    Link(Range),
    /// Notes, charts and pictures (needs "Edit objects").
    Objects,
    /// Filtering with an existing AutoFilter (needs "Use AutoFilter").
    Filter,
    /// Changes a protected sheet never allows (tables, subtotals, merging, data validation,
    /// inserting or deleting cells, ...).
    Any,
}

pub const LOCKED_MSG: &str = "The cell or chart you're trying to change is on a protected sheet. To make a change, unprotect the sheet. You might be requested to enter a password.";

impl App {
    pub fn sheet_protected(&self) -> bool {
        self.sheet().protection.is_some()
    }

    /// The one protection guard: true (after telling the user, like Excel) when the protected
    /// active sheet doesn't allow this change. Every command that changes a sheet asks it first.
    pub fn protect_blocks(&mut self, change: Change) -> bool {
        self.protect_blocks_at(self.wb.active, change)
    }

    /// `protect_blocks` for sheet `si`.
    pub fn protect_blocks_at(&mut self, si: usize, change: Change) -> bool {
        let blocked = protection_blocks(&self.wb, si, change);
        if blocked {
            self.dialog = Some(super::dialogs::Dialog::Message { title: "FreedomOffice Sheets".into(), text: LOCKED_MSG.into() });
        }
        blocked
    }

    /// `protect_blocks(Change::Cells(rg))`: the range holds a locked cell of a protected sheet.
    pub fn locked_block(&mut self, rg: Range) -> bool {
        self.protect_blocks(Change::Cells(rg))
    }

    /// `protect_blocks(Change::Format)`: the protected sheet doesn't allow formatting cells.
    pub fn format_block(&mut self) -> bool {
        self.protect_blocks(Change::Format)
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

/// True when sheet `si` is protected and doesn't allow `change`.
pub fn protection_blocks(wb: &Workbook, si: usize, change: Change) -> bool {
    let Some(sh) = wb.sheets.get(si) else { return false };
    let Some(p) = &sh.protection else { return false };
    match change {
        Change::Cells(rg) => range_has_locked(wb, sh, rg),
        Change::Format => !p.format_cells,
        Change::FormatRows => !p.format_rows,
        Change::FormatCols => !p.format_columns,
        Change::InsertRows => !p.insert_rows,
        Change::InsertCols => !p.insert_columns,
        Change::DeleteRows(rg) => !p.delete_rows || range_has_locked(wb, sh, rg),
        Change::DeleteCols(rg) => !p.delete_columns || range_has_locked(wb, sh, rg),
        Change::Sort(rg) => !p.sort || range_has_locked(wb, sh, rg),
        Change::Link(rg) => !p.insert_links || range_has_locked(wb, sh, rg),
        Change::Objects => !p.edit_objects,
        Change::Filter => !p.autofilter,
        Change::Any => true,
    }
}

/// Whether the range holds a locked cell. Only stored cells are visited: a cell that isn't
/// stored has the default (locked) style. Whole rows/columns are checked over the used area,
/// which is where formatting whole rows/columns lands in this model.
fn range_has_locked(wb: &Workbook, sh: &Sheet, rg: Range) -> bool {
    let rg = if rg.is_full_rows() || rg.is_full_cols() {
        let (er, ec) = sh.extent().unwrap_or((0, 0));
        Range { r1: rg.r1, c1: rg.c1, r2: rg.r2.min(er.max(rg.r1)), c2: rg.c2.min(ec.max(rg.c1)) }
    } else {
        rg
    };
    let mut unlocked = 0u64;
    for (_, _, cell) in sh.iter_range(rg) {
        if !wb.style(cell.style).unlocked {
            return true;
        }
        unlocked += 1;
    }
    unlocked < rg.rows() as u64 * rg.cols() as u64 && !wb.style(0).unlocked
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

#[cfg(test)]
mod tests {
    use super::super::data_ui::sample_app;
    use super::super::dialogs::Dialog;
    use super::super::fill::FillKind;
    use super::*;
    use crate::datatools::{SortKey, SortOptions};
    use crate::model::{Value, MAX_COLS, MAX_ROWS};

    /// Everything a blocked command could have changed on the active sheet.
    fn fingerprint(app: &App) -> String {
        let sh = app.sheet();
        let sorted = |s: &std::collections::HashSet<u32>| {
            let mut v: Vec<u32> = s.iter().copied().collect();
            v.sort_unstable();
            v
        };
        let sizes = |m: &std::collections::HashMap<u32, f32>| {
            let mut v: Vec<(u32, u32)> = m.iter().map(|(k, x)| (*k, *x as u32)).collect();
            v.sort_unstable();
            v
        };
        format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{}|{}|{}|{}|{}",
            sh.rows,
            sorted(&sh.hidden_rows),
            sorted(&sh.hidden_cols),
            sizes(&sh.row_h),
            sizes(&sh.col_w),
            sh.merges,
            sh.tables.len(),
            sh.cond_formats.len(),
            sh.notes.len(),
            sh.filter.is_some(),
            sh.row_levels.len()
        )
    }

    fn told_locked(app: &mut App) -> bool {
        matches!(app.dialog.take(), Some(Dialog::Message { text, .. }) if text == LOCKED_MSG)
    }

    fn unlock(app: &mut App, rg: Range) {
        let st = app.wb.intern_style(Style { unlocked: true, ..Default::default() });
        for r in rg.r1..=rg.r2 {
            for c in rg.c1..=rg.c2 {
                app.wb.sheets[0].cell_mut(r, c).style = st;
            }
        }
    }

    #[test]
    fn protected_sheet_refuses_every_change_to_locked_cells() {
        let (ctx, mut app) = sample_app();
        app.wb.sheets[0].protection = Some(Protection::default());
        // A Flash Fill example next to the list.
        app.wb.sheets[0].set(1, 5, Some(crate::model::Cell { value: Value::Text("Ann".into()), ..Default::default() }));
        let before = fingerprint(&app);
        let data = Range::new(0, 0, 7, 4);
        type Cmd = Box<dyn Fn(&mut App, &egui::Context)>;
        let cmds: Vec<(&str, Cmd)> = vec![
            ("fill handle", Box::new(|a, _| a.autofill_kind(Range::cell(1, 2), Range::new(1, 2, 4, 2), FillKind::Auto))),
            (
                "fill down",
                Box::new(|a, _| {
                    a.select_range(Range::new(1, 2, 4, 2));
                    a.fill_down();
                }),
            ),
            (
                "flash fill",
                Box::new(|a, _| {
                    a.select(2, 5, false);
                    a.flash_fill();
                }),
            ),
            (
                "replace all",
                Box::new(|a, _| {
                    a.find.what = "East".into();
                    a.find.with = "Ost".into();
                    assert_eq!(a.replace_all(), 0);
                }),
            ),
            ("sort", Box::new(move |a, _| a.sort_range_by(data, true, &[SortKey::values(2, false)], SortOptions::default()))),
            ("remove duplicates", Box::new(move |a, _| assert_eq!(a.remove_duplicates(data, true, &[0]), 0))),
            ("subtotal", Box::new(move |a, _| a.insert_subtotals(data, 0, 0, &[2], true, true, false))),
            ("format as table", Box::new(|a, _| a.format_as_table(crate::table::ACCENTS[1], 1))),
            ("bold", Box::new(|a, _| a.toggle_bold())),
            ("wrap", Box::new(|a, _| a.toggle_wrap())),
            ("borders", Box::new(|a, _| a.borders(super::super::actions::BorderKind::All))),
            (
                "merge",
                Box::new(|a, _| {
                    a.select_range(Range::new(1, 0, 2, 1));
                    a.merge(2);
                }),
            ),
            ("insert rows", Box::new(|a, _| a.insert_rows())),
            ("delete rows", Box::new(|a, _| a.delete_rows())),
            ("insert columns", Box::new(|a, _| a.insert_cols())),
            ("delete columns", Box::new(|a, _| a.delete_cols())),
            ("insert cells", Box::new(|a, _| a.insert_cells(true))),
            ("row height", Box::new(|a, _| a.set_row_height(40.0))),
            ("column width", Box::new(|a, _| a.set_col_width(120.0))),
            ("hide rows", Box::new(|a, _| a.hide_rows(true))),
            ("hide columns", Box::new(|a, _| a.hide_cols(true))),
            ("group", Box::new(|a, _| a.outline_group_cmd(false))),
            (
                "drag move",
                Box::new(|a, ctx| {
                    a.select_range(Range::new(1, 0, 2, 0));
                    a.drop_selection(ctx, 1, 8, false);
                }),
            ),
            ("clear", Box::new(|a, _| a.clear(true, true))),
            ("note", Box::new(|a, _| a.set_note(1, 1, Some("hi".into())))),
            ("filter", Box::new(|a, _| a.toggle_filter())),
            ("fill shrink", Box::new(|a, _| a.fill_shrink(Range::new(1, 2, 4, 2), Range::new(1, 2, 2, 2)))),
        ];
        for (name, cmd) in cmds {
            app.select(1, 2, false);
            cmd(&mut app, &ctx);
            assert_eq!(fingerprint(&app), before, "{name} changed a protected sheet");
            assert!(told_locked(&mut app), "{name} didn't show the protected-sheet message");
        }
    }

    #[test]
    fn permissions_and_unlocked_cells() {
        let (_ctx, mut app) = sample_app();
        app.wb.sheets[0].protection = Some(Protection { format_rows: true, insert_rows: true, ..Default::default() });
        // Unlocked cells can be filled, the allowed structure changes go through.
        unlock(&mut app, Range::new(1, 2, 7, 2));
        app.autofill_kind(Range::cell(1, 2), Range::new(1, 2, 3, 2), FillKind::Copy);
        assert!(app.dialog.is_none());
        assert_eq!(app.sheet().value(3, 2), &Value::Number(120.0));
        app.select(0, 0, false);
        app.set_row_height(30.0);
        assert!(app.dialog.is_none());
        assert_eq!(app.sheet().row_h.get(&0), Some(&30.0));
        app.select_range(Range::new(9, 0, 9, MAX_COLS - 1));
        app.insert_rows();
        assert!(app.dialog.is_none());
        // Formatting is still refused, and so is a fill reaching a locked cell.
        app.select(1, 2, false);
        app.toggle_bold();
        assert!(told_locked(&mut app));
        let date = app.sheet().value(1, 3).clone();
        app.autofill_kind(Range::cell(1, 2), Range::new(1, 2, 1, 3), FillKind::Copy);
        assert!(told_locked(&mut app));
        assert_eq!(app.sheet().value(1, 3), &date);
    }

    #[test]
    fn deleting_an_unlocked_column_is_checked_without_scanning_a_million_cells() {
        let (_ctx, mut app) = sample_app();
        app.wb.sheets[0].protection = Some(Protection { delete_columns: true, ..Default::default() });
        unlock(&mut app, Range::new(0, 4, 7, 4));
        let t = std::time::Instant::now();
        assert!(!protection_blocks(&app.wb, 0, Change::DeleteCols(Range::new(0, 4, MAX_ROWS - 1, 4))));
        assert!(protection_blocks(&app.wb, 0, Change::DeleteCols(Range::new(0, 3, MAX_ROWS - 1, 3))));
        assert!(protection_blocks(&app.wb, 0, Change::Cells(Range::new(0, 4, 8, 4))));
        assert!(t.elapsed().as_millis() < 500);
        app.select_range(Range::new(0, 4, MAX_ROWS - 1, 4));
        app.delete_cols();
        assert!(app.dialog.is_none());
        assert_eq!(app.sheet().value(0, 4), &Value::Empty);
        assert_eq!(app.sheet().value(0, 3), &Value::Text("Date".into()));
        // Deleting rows isn't allowed at all.
        app.select_range(Range::new(1, 0, 1, MAX_COLS - 1));
        app.delete_rows();
        assert!(told_locked(&mut app));
    }

    #[test]
    fn paste_checks_the_real_target_area() {
        let (ctx, mut app) = sample_app();
        app.wb.sheets[0].protection = Some(Protection::default());
        // Copy two cells; only the active cell of the destination is unlocked.
        unlock(&mut app, Range::cell(1, 7));
        app.select_range(Range::new(1, 0, 2, 0));
        app.copy(&ctx, false);
        app.select(1, 7, false);
        app.paste_special(Default::default());
        assert!(told_locked(&mut app));
        assert_eq!(app.sheet().value(2, 7), &Value::Empty);
        // A fully unlocked destination takes the paste.
        unlock(&mut app, Range::cell(2, 7));
        app.paste_special(Default::default());
        assert!(app.dialog.is_none());
        assert_eq!(app.sheet().value(2, 7), &Value::Text("West".into()));
        // Cut from locked cells can't be moved, even to unlocked ones.
        unlock(&mut app, Range::new(5, 7, 6, 7));
        app.select_range(Range::new(1, 0, 2, 0));
        app.copy(&ctx, true);
        app.select(5, 7, false);
        app.paste_special(Default::default());
        assert!(told_locked(&mut app));
        assert_eq!(app.sheet().value(1, 0), &Value::Text("East".into()));
        assert_eq!(app.sheet().value(5, 7), &Value::Empty);
    }
}
