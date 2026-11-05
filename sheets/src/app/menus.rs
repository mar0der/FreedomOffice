//! Right-click menus for cells and row/column headers (also Shift+F10 / the Menu key),
//! the colour sort/filter commands they offer, and Create Names from Selection.

use eframe::egui::{self, Align2, Pos2, Vec2};

use super::actions::shift_cell;
use super::dialogs::Dialog;
use super::theme;
use super::{App, Scope};
use crate::model::{Cell, Range, Sheet, Value, MAX_COLS, MAX_ROWS};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MenuKind {
    #[default]
    Cell,
    Rows,
    Cols,
}

#[derive(Default)]
pub struct MenuState {
    /// Open the context menu here on the next frame (keyboard).
    pub open_at: Option<Pos2>,
    pub kind: MenuKind,
    /// Screen rectangles of the floating option buttons drawn this frame.
    pub float_rects: Vec<egui::Rect>,
}

/// Create Names from Selection: which edge holds the names.
pub struct CreateNamesDlg {
    pub top: bool,
    pub left: bool,
    pub bottom: bool,
    pub right: bool,
}

/// Turns a label into a valid defined name ("Unit price" -> "Unit_price", "2024" -> "_2024").
fn clean_name(label: &str) -> Option<String> {
    let mut s: String = label.trim().chars().map(|c| if c.is_alphanumeric() || c == '_' || c == '.' { c } else { '_' }).collect();
    if s.is_empty() {
        return None;
    }
    if !s.starts_with(|c: char| c.is_alphabetic() || c == '_') || crate::formula::parse_cell_part(&s).is_some() {
        s.insert(0, '_');
    }
    Some(s)
}

/// Names (and the absolute ranges they refer to) created from the labels on the chosen edges.
fn names_from_selection(sh: &Sheet, rg: Range, d: &CreateNamesDlg, label: impl Fn(u32, u32) -> String) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let q = crate::formula::quote_sheet(&sh.name);
    let abs = |r: Range| {
        let a = format!("${}${}", crate::model::col_name(r.c1), r.r1 + 1);
        if r.is_single() { format!("{q}!{a}") } else { format!("{q}!{a}:${}${}", crate::model::col_name(r.c2), r.r2 + 1) }
    };
    // The data block excludes every label row/column that was chosen.
    let r1 = if d.top { rg.r1 + 1 } else { rg.r1 };
    let r2 = if d.bottom { rg.r2.saturating_sub(1) } else { rg.r2 };
    let c1 = if d.left { rg.c1 + 1 } else { rg.c1 };
    let c2 = if d.right { rg.c2.saturating_sub(1) } else { rg.c2 };
    if r1 > r2 || c1 > c2 {
        return out;
    }
    let mut add = |name: String, target: Range| {
        if let Some(n) = clean_name(&name) {
            out.push((n, abs(target)));
        }
    };
    for c in c1..=c2 {
        if d.top {
            add(label(rg.r1, c), Range::new(r1, c, r2, c));
        }
        if d.bottom {
            add(label(rg.r2, c), Range::new(r1, c, r2, c));
        }
    }
    for r in r1..=r2 {
        if d.left {
            add(label(r, rg.c1), Range::new(r, c1, r, c2));
        }
        if d.right {
            add(label(r, rg.c2), Range::new(r, c1, r, c2));
        }
    }
    out
}

impl App {
    /// Right-click menu on the grid; picks the cell, row or column menu from where it was opened.
    pub fn context_menu(&mut self, resp: &egui::Response) {
        if resp.secondary_clicked() {
            if let (Some(p), Some(lay)) = (resp.interact_pointer_pos(), self.layout.clone()) {
                let sel = self.sel();
                self.menu.kind = if p.y < lay.cells.top() && p.x >= lay.cells.left() {
                    let c = lay.col_near(p.x);
                    if !(sel.is_full_cols() && sel.c1 <= c && c <= sel.c2) {
                        self.select_whole(Range::new(0, c, MAX_ROWS - 1, c), (self.sheet().view.top, c));
                    }
                    MenuKind::Cols
                } else if p.x < lay.cells.left() && p.y >= lay.cells.top() {
                    let r = lay.row_near(p.y);
                    if !(sel.is_full_rows() && sel.r1 <= r && r <= sel.r2) {
                        self.select_whole(Range::new(r, 0, r, MAX_COLS - 1), (r, self.sheet().view.left));
                    }
                    MenuKind::Rows
                } else {
                    MenuKind::Cell
                };
            }
        }
        if let Some(p) = self.menu.open_at.take() {
            // The context menu anchors where its popup was opened (egui keeps that in memory).
            #[allow(deprecated)]
            resp.ctx.memory_mut(|m| m.open_popup_at(egui::Popup::default_response_id(resp), Some(p)));
        }
        resp.context_menu(|ui| {
            ui.set_min_width(210.0);
            match self.menu.kind {
                MenuKind::Cell => self.cell_menu(ui),
                k => self.header_menu(ui, k == MenuKind::Rows),
            }
        });
    }

    fn select_whole(&mut self, rg: Range, active: (u32, u32)) {
        let v = &mut self.sheet_mut().view;
        v.extra.clear();
        v.sel = rg;
        v.active = active;
        v.anchor = active;
    }

    /// Shift+F10 / Menu key: opens the context menu at the active cell.
    pub fn open_context_menu_kb(&mut self) {
        let sel = self.sel();
        self.menu.kind = if sel.is_full_cols() && !sel.is_full_rows() {
            MenuKind::Cols
        } else if sel.is_full_rows() && !sel.is_full_cols() {
            MenuKind::Rows
        } else {
            MenuKind::Cell
        };
        let Some(lay) = self.layout.clone() else { return };
        let (r, c) = self.active();
        let rect = self.range_rect(&lay, Range::cell(r, c));
        let p = Pos2::new(rect.center().x.clamp(lay.cells.left(), lay.cells.right() - 10.0), rect.bottom().clamp(lay.cells.top(), lay.cells.bottom() - 10.0));
        self.menu.open_at = Some(p);
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
    }

    fn clipboard_items(&mut self, ui: &mut egui::Ui) {
        if ui.button("Cut").clicked() {
            self.copy(ui.ctx(), true);
            ui.close();
        }
        if ui.button("Copy").clicked() {
            self.copy(ui.ctx(), false);
            ui.close();
        }
        if self.paste_icon_row(ui) {
            ui.close();
        }
        let can_special = self.clip.as_ref().is_some_and(|c| !c.cut);
        if ui.add_enabled(can_special, egui::Button::new("Paste Special...")).clicked() {
            self.open_paste_special();
            ui.close();
        }
        ui.separator();
    }

    fn insert_item(&mut self, ui: &mut egui::Ui, header: bool) {
        let label = match &self.clip {
            Some(c) if c.cut => "Insert Cut Cells",
            Some(_) => "Insert Copied Cells",
            None if header => "Insert",
            None => "Insert...",
        };
        if ui.button(label).clicked() {
            if self.clip.is_some() {
                self.insert_paste_prompt();
            } else {
                self.insert_cells_prompt();
            }
            ui.close();
        }
    }

    fn header_menu(&mut self, ui: &mut egui::Ui, rows: bool) {
        self.clipboard_items(ui);
        self.insert_item(ui, true);
        if ui.button("Delete").clicked() {
            if rows { self.delete_rows() } else { self.delete_cols() }
            ui.close();
        }
        if ui.button("Clear Contents").clicked() {
            self.clear(true, false);
            ui.close();
        }
        ui.separator();
        if ui.button("Format Cells...").clicked() {
            self.open_format_cells(0);
            ui.close();
        }
        if ui.button(if rows { "Row Height..." } else { "Column Width..." }).clicked() {
            if rows { self.open_row_height() } else { self.open_col_width() }
            ui.close();
        }
        if ui.button("Hide").clicked() {
            if rows { self.hide_rows(true) } else { self.hide_cols(true) }
            ui.close();
        }
        if ui.button("Unhide").clicked() {
            if rows { self.hide_rows(false) } else { self.hide_cols(false) }
            ui.close();
        }
    }

    fn cell_menu(&mut self, ui: &mut egui::Ui) {
        self.clipboard_items(ui);
        self.insert_item(ui, false);
        if ui.button("Delete...").clicked() {
            self.delete_cells_prompt();
            ui.close();
        }
        if ui.button("Clear Contents").clicked() {
            self.clear(true, false);
            ui.close();
        }
        ui.separator();
        let (ar, ac) = self.active();
        let filter_col = self.sheet().filter.as_ref().filter(|f| f.range.c1 <= ac && ac <= f.range.c2).map(|f| (f.range.r1, f.excluded.get(&ac).is_some_and(|s| !s.is_empty())));
        ui.menu_button("Filter", |ui| {
            let has_filter = self.sheet().filter.is_some();
            let header = filter_col.map(|(hr, _)| crate::numfmt::value_to_text(self.sheet().value(hr, ac))).unwrap_or_default();
            let label = if header.is_empty() { "Clear Filter".to_string() } else { format!("Clear Filter From \"{header}\"") };
            if ui.add_enabled(filter_col.is_some_and(|f| f.1), egui::Button::new(label)).clicked() {
                if let Some(f) = &mut self.sheet_mut().filter {
                    f.excluded.remove(&ac);
                }
                self.apply_filter();
                ui.close();
            }
            if ui.add_enabled(has_filter, egui::Button::new("Reapply")).clicked() {
                self.apply_filter();
                ui.close();
            }
            ui.separator();
            if ui.button("Filter by Selected Cell's Value").clicked() {
                self.filter_by_active_value();
                ui.close();
            }
            if ui.button("Filter by Selected Cell's Color").clicked() {
                self.filter_by_color(false);
                ui.close();
            }
            if ui.button("Filter by Selected Cell's Font Color").clicked() {
                self.filter_by_color(true);
                ui.close();
            }
        });
        let (asc, desc) = match self.sheet().value(ar, ac) {
            Value::Number(_) if crate::numfmt::is_date_format(&self.active_style().num_fmt) => ("Sort Oldest to Newest", "Sort Newest to Oldest"),
            Value::Number(_) => ("Sort Smallest to Largest", "Sort Largest to Smallest"),
            _ => ("Sort A to Z", "Sort Z to A"),
        };
        ui.menu_button("Sort", |ui| {
            if ui.button(asc).clicked() {
                self.sort_simple(false);
                ui.close();
            }
            if ui.button(desc).clicked() {
                self.sort_simple(true);
                ui.close();
            }
            ui.separator();
            if ui.button("Put Selected Cell Color On Top").clicked() {
                self.color_on_top(false);
                ui.close();
            }
            if ui.button("Put Selected Font Color On Top").clicked() {
                self.color_on_top(true);
                ui.close();
            }
            ui.separator();
            if ui.button("Custom Sort...").clicked() {
                self.open_sort_dialog();
                ui.close();
            }
        });
        ui.separator();
        let has_note = self.sheet().notes.contains_key(&(ar, ac));
        if ui.button(if has_note { "Edit Note" } else { "New Note" }).clicked() {
            self.edit_note();
            ui.close();
        }
        if has_note && ui.button("Delete Note").clicked() {
            self.delete_notes_in_selection();
            ui.close();
        }
        ui.separator();
        if ui.button("Format Cells...").clicked() {
            self.open_format_cells(0);
            ui.close();
        }
        if ui.button("Pick From Drop-down List...").clicked() {
            self.dv_open_dropdown();
            ui.close();
        }
        if ui.button("Define Name...").clicked() {
            self.define_name();
            ui.close();
        }
        let has_link = self.sheet().links.contains_key(&(ar, ac));
        ui.menu_button("Link", |ui| {
            if ui.button(if has_link { "Edit Hyperlink..." } else { "Insert Link..." }).clicked() {
                self.open_link_dialog();
                ui.close();
            }
            if ui.add_enabled(has_link, egui::Button::new("Open Hyperlink")).clicked() {
                if let Some(t) = self.sheet().links.get(&(ar, ac)).cloned() {
                    self.follow_link(&t);
                }
                ui.close();
            }
            if ui.add_enabled(has_link, egui::Button::new("Remove Hyperlink")).clicked() {
                self.set_link(ar, ac, "", None);
                ui.close();
            }
        });
    }

    /// Define Name...: a new name for the selection, suggested from a label next to it.
    pub fn define_name(&mut self) {
        let (r, c) = self.active();
        let sh = self.sheet();
        let label = |r: u32, c: u32| match sh.value(r, c) {
            Value::Text(t) => clean_name(t),
            _ => None,
        };
        let name = label(r, c)
            .filter(|_| !self.sel().is_single())
            .or_else(|| if c > 0 { label(r, c - 1) } else { None })
            .or_else(|| if r > 0 { label(r - 1, c) } else { None })
            .unwrap_or_default();
        self.dialog = Some(Dialog::NameManager { name, refers: self.sel_abs_ref(), error: None });
    }

    /// Cell colour of a cell (fill or font) as shown, for the colour sort/filter commands.
    fn cell_color(&self, r: u32, c: u32, font: bool) -> Option<u32> {
        let st = self.wb.style(self.sheet().get(r, c).map_or(0, |x| x.style));
        if font { st.color } else { st.fill }
    }

    /// Sort > Put Selected Cell (Font) Color On Top: rows whose cell in the active column has
    /// that colour move to the top, keeping their order.
    pub fn color_on_top(&mut self, font: bool) {
        let (rg, header) = self.sort_area();
        let (ar, ac) = self.active();
        let want = self.cell_color(ar, ac, font);
        let r1 = if header { rg.r1 + 1 } else { rg.r1 };
        if r1 > rg.r2 {
            return;
        }
        let data = Range::new(r1, rg.c1, rg.r2, rg.c2);
        let mut order: Vec<u32> = (data.r1..=data.r2).collect();
        order.sort_by_key(|&r| self.cell_color(r, ac, font) != want);
        let si = self.wb.active;
        self.push_undo(&[Scope::Range(si, data)]);
        reorder_rows(&mut self.wb.sheets[si], data, &order);
        self.changed();
    }

    /// Filter > Filter by Selected Cell's (Font) Color: hides rows of a different colour.
    pub fn filter_by_color(&mut self, font: bool) {
        if self.sheet().filter.is_none() {
            self.toggle_filter();
        }
        let (ar, ac) = self.active();
        let want = self.cell_color(ar, ac, font);
        let Some(f) = self.sheet().filter.clone() else { return };
        let hide: Vec<u32> = (f.range.r1 + 1..=f.range.r2).filter(|&r| self.cell_color(r, ac, font) != want).collect();
        let si = self.wb.active;
        self.push_undo(&[Scope::Sheet(si)]);
        let sh = self.sheet_mut();
        for r in hide {
            sh.hidden_rows.insert(r);
        }
        self.changed();
    }

    /// Ctrl+Shift+F3: Create Names from Selection.
    pub fn open_create_names(&mut self) {
        self.commit_edit(false);
        let rg = self.sel_clipped();
        let sh = self.sheet();
        let text = |r: u32, c: u32| matches!(sh.value(r, c), Value::Text(_));
        // Like the familiar dialog, guess the label edges from where the text is.
        let top = rg.rows() > 1 && (rg.c1..=rg.c2).filter(|&c| c > rg.c1 || rg.cols() == 1).all(|c| text(rg.r1, c));
        let left = rg.cols() > 1 && (rg.r1..=rg.r2).filter(|&r| r > rg.r1 || rg.rows() == 1).all(|r| text(r, rg.c1));
        self.dialog = Some(Dialog::CreateNames(Box::new(CreateNamesDlg { top, left, bottom: false, right: false })));
    }

    pub fn create_names_ui(&mut self, ctx: &egui::Context, d: &mut CreateNamesDlg) -> bool {
        let mut keep = true;
        egui::Window::new("Create Names from Selection").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, -40.0]).show(ctx, |ui| {
            ui.set_min_width(260.0);
            ui.label("Create names from values in the:");
            ui.checkbox(&mut d.top, "Top row");
            ui.checkbox(&mut d.left, "Left column");
            ui.checkbox(&mut d.bottom, "Bottom row");
            ui.checkbox(&mut d.right, "Right column");
            ui.add_space(6.0);
            let mut ok = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let mut cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                cancel |= ui.add(egui::Button::new("Cancel").min_size(Vec2::new(76.0, 24.0))).clicked();
                ok |= ui.add(egui::Button::new("OK").min_size(Vec2::new(76.0, 24.0)).fill(theme::accent_light())).clicked();
            });
            if ok {
                keep = false;
                let rg = self.sel_clipped();
                let sh = self.sheet();
                let names = names_from_selection(sh, rg, d, |r, c| crate::numfmt::value_to_text(sh.value(r, c)));
                if names.is_empty() {
                    self.status = Some("Select a range with labels on the chosen edges.".into());
                } else {
                    let start = self.undo_len();
                    for (n, f) in &names {
                        let _ = self.new_name(n, f);
                    }
                    self.squash_undo(start);
                    self.status = Some(format!("Created {} name{}.", names.len(), if names.len() == 1 { "" } else { "s" }));
                }
            } else if cancel {
                keep = false;
            }
        });
        keep
    }
}

/// Rewrites the rows of `data` in the given order of source rows.
fn reorder_rows(sh: &mut Sheet, data: Range, order: &[u32]) {
    let rows: Vec<Vec<(u32, Cell)>> = order.iter().map(|&r| (data.c1..=data.c2).filter_map(|c| sh.get(r, c).map(|x| (c, x.clone()))).collect()).collect();
    let old: Vec<(u32, u32)> = sh.iter_range(data).map(|(r, c, _)| (r, c)).collect();
    for (r, c) in old {
        sh.remove(r, c);
    }
    let mut moved = std::collections::HashMap::new();
    for (i, cells) in rows.into_iter().enumerate() {
        let nr = data.r1 + i as u32;
        let orig = order[i];
        moved.insert(orig, nr);
        for (c, mut cell) in cells {
            shift_cell(&mut cell, nr as i64 - orig as i64, 0);
            sh.set(nr, c, Some(cell));
        }
    }
    let remap = |m: &mut std::collections::HashMap<(u32, u32), String>| {
        *m = m.drain().map(|((r, c), t)| if data.contains(r, c) { ((moved.get(&r).copied().unwrap_or(r), c), t) } else { ((r, c), t) }).collect();
    };
    remap(&mut sh.notes);
    remap(&mut sh.links);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_top_row_and_left_column() {
        let sh = Sheet::new("Data");
        let labels = |r: u32, c: u32| -> String {
            match (r, c) {
                (0, 1) => "Unit price".into(),
                (0, 2) => "Qty".into(),
                (1, 0) => "Apples".into(),
                (2, 0) => "2024".into(),
                _ => String::new(),
            }
        };
        let d = CreateNamesDlg { top: true, left: false, bottom: false, right: false };
        let n = names_from_selection(&sh, Range::new(0, 1, 2, 2), &d, labels);
        assert_eq!(n, vec![("Unit_price".to_string(), "Data!$B$2:$B$3".to_string()), ("Qty".to_string(), "Data!$C$2:$C$3".to_string())]);
        let d = CreateNamesDlg { top: true, left: true, bottom: false, right: false };
        let n = names_from_selection(&sh, Range::new(0, 0, 2, 2), &d, labels);
        assert!(n.contains(&("Apples".to_string(), "Data!$B$2:$C$2".to_string())));
        assert!(n.contains(&("_2024".to_string(), "Data!$B$3:$C$3".to_string())));
        assert!(n.contains(&("Unit_price".to_string(), "Data!$B$2:$B$3".to_string())));
    }

    #[test]
    fn reorder_moves_cells_and_notes() {
        let mut sh = Sheet::new("S");
        for r in 0..3 {
            sh.set(r, 0, Some(Cell { value: Value::Number(r as f64), ..Default::default() }));
        }
        sh.notes.insert((2, 0), "n".into());
        reorder_rows(&mut sh, Range::new(0, 0, 2, 0), &[2, 0, 1]);
        assert_eq!(sh.value(0, 0), &Value::Number(2.0));
        assert_eq!(sh.value(1, 0), &Value::Number(0.0));
        assert_eq!(sh.notes.get(&(0, 0)).map(String::as_str), Some("n"));
    }
}
