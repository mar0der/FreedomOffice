//! Excel Tables in the app: Format as Table, the contextual Table Design tab, table-style
//! painting, the total-row dropdown and growing a table when typing next to it.

use eframe::egui::{self, Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};

use super::grid::Layout;
use super::theme::{self, rgb};
use super::{App, RibbonTab, Scope};
use crate::model::{BorderLine, Cell, Range, Style, Value};
use crate::table::{self, Look, Table, TotalFn};

/// Table Design tab and total-row menu state.
#[derive(Default)]
pub struct TableUi {
    /// Ribbon tab to return to when the active cell leaves the table.
    prev_tab: Option<RibbonTab>,
    /// Name box text and the table it was loaded from.
    name: String,
    name_for: Option<(usize, usize)>,
    /// Resize box text.
    resize: String,
    resize_for: Option<(usize, usize, Range)>,
    /// Open total-row menu: (sheet, table, column offset, screen position).
    total_menu: Option<(usize, usize, u32, Pos2)>,
}

impl App {
    // ------------------------------------------------------------------
    // Lookup
    // ------------------------------------------------------------------

    /// (sheet, table index) of the table holding the active cell.
    pub fn active_table(&self) -> Option<(usize, usize)> {
        let (r, c) = self.active();
        let si = self.wb.active;
        table::at(self.sheet(), r, c).map(|i| (si, i))
    }

    /// Table-style look of a cell of sheet `si`.
    pub fn table_look(&self, si: usize, r: u32, c: u32) -> Option<Look> {
        let sh = &self.wb.sheets[si];
        let i = table::at(sh, r, c)?;
        table::look(&sh.tables[i], r, c)
    }

    // ------------------------------------------------------------------
    // Commands
    // ------------------------------------------------------------------

    /// Format as Table: turns the current region (or selection) into a real table whose first
    /// row holds the headers. `kind`: 0 = Light, 1 = Medium, 2 = Dark.
    pub fn format_as_table(&mut self, accent: u32, kind: u8) {
        if !self.commit_edit(false) {
            return;
        }
        let ai = table::ACCENTS.iter().position(|a| *a == accent).unwrap_or(1);
        let style = table::style_name(kind, ai);
        if let Some((s, i)) = self.active_table() {
            // Inside a table the gallery restyles it, as in Excel.
            self.edit_table(s, i, |t| t.style = style);
            return;
        }
        let rg = if self.sel().is_single() { self.current_region() } else { self.sel_clipped() };
        if rg.rows() < 2 {
            self.status = Some("Select a range with a header row and data to format as a table.".into());
            return;
        }
        let si = self.wb.active;
        if self.sheet().tables.iter().any(|t| t.range.intersects(&rg)) || self.sheet().merges.iter().any(|m| m.intersects(&rg)) {
            self.status = Some("A table can't overlap another table or merged cells.".into());
            return;
        }
        self.push_undo(&[Scope::Sheet(si)]);
        let name = table::unique_name(&self.wb, "Table");
        let mut t = Table::new(&name, rg, &style);
        table::sync_columns_from_cells(self.sheet(), &mut t);
        let sh = &mut self.wb.sheets[si];
        for (i, col) in t.columns.iter().enumerate() {
            let c = rg.c1 + i as u32;
            let style = sh.get(rg.r1, c).map_or(0, |x| x.style);
            sh.set(rg.r1, c, Some(Cell { value: Value::Text(col.name.clone()), formula: None, style }));
        }
        if sh.filter.as_ref().is_some_and(|f| f.range.intersects(&rg)) {
            sh.filter = None;
        }
        sh.tables.push(t);
        self.select_range(rg);
        self.status = None;
        self.changed();
    }

    /// Applies an undoable change to a table and keeps its cells in step.
    fn edit_table(&mut self, s: usize, i: usize, f: impl FnOnce(&mut Table)) {
        self.push_undo(&[Scope::Sheet(s)]);
        let before = self.wb.sheets[s].tables[i].clone();
        f(&mut self.wb.sheets[s].tables[i]);
        let after = self.wb.sheets[s].tables[i].clone();
        self.sync_table_rows(s, i, &before, &after);
        self.changed();
    }

    /// Writes header names / total-row cells after header or total row were switched.
    fn sync_table_rows(&mut self, s: usize, i: usize, before: &Table, after: &Table) {
        let sh = &mut self.wb.sheets[s];
        let rg = after.range;
        if before.header && !after.header {
            for c in rg.c1..=rg.c2 {
                if let Some(cell) = sh.get_mut(before.range.r1, c) {
                    cell.value = Value::Empty;
                    cell.formula = None;
                }
                sh.tidy(before.range.r1, c);
            }
        }
        if after.header {
            for (k, col) in after.columns.iter().enumerate() {
                let c = rg.c1 + k as u32;
                let style = sh.get(rg.r1, c).map_or(0, |x| x.style);
                sh.set(rg.r1, c, Some(Cell { value: Value::Text(col.name.clone()), formula: None, style }));
            }
        }
        if before.totals && !after.totals {
            for c in rg.c1..=rg.c2 {
                if let Some(cell) = sh.get_mut(before.range.r2, c) {
                    cell.value = Value::Empty;
                    cell.formula = None;
                }
                sh.tidy(before.range.r2, c);
            }
        }
        if after.totals && (!before.totals || before.columns != after.columns) {
            for k in 0..after.columns.len() {
                self.write_total_cell(s, i, k);
            }
        }
    }

    /// Writes the total-row cell of column `k` from its function or label.
    fn write_total_cell(&mut self, s: usize, i: usize, k: usize) {
        let t = self.wb.sheets[s].tables[i].clone();
        let Some(r) = t.totals_row() else { return };
        let c = t.range.c1 + k as u32;
        let col = &t.columns[k];
        if col.total == TotalFn::Custom {
            return;
        }
        let text = match t.total_formula(k) {
            Some(f) => format!("={f}"),
            None => col.total_label.clone(),
        };
        let style = self.wb.sheets[s].get(r, c).map_or(0, |x| x.style);
        let cell = crate::model::cell_from_input(&mut self.wb, &text, style);
        self.wb.sheets[s].set(r, c, cell);
    }

    fn toggle_totals(&mut self, s: usize, i: usize) {
        let t = self.wb.sheets[s].tables[i].clone();
        if t.totals {
            self.edit_table(s, i, |t| {
                t.totals = false;
                t.range.r2 -= 1;
            });
            return;
        }
        if t.range.r2 + 1 >= crate::model::MAX_ROWS {
            return;
        }
        let below = Range::new(t.range.r2 + 1, t.range.c1, t.range.r2 + 1, t.range.c2);
        if self.sheet().iter_range(below).any(|(_, _, c)| !c.value.is_empty() || c.formula.is_some()) {
            self.status = Some("The row below the table isn't empty: clear it to add a total row.".into());
            return;
        }
        let numeric_last = {
            let d = t.data();
            let sh = &self.wb.sheets[s];
            (d.r1..=d.r2).any(|r| matches!(sh.value(r, t.range.c2), Value::Number(_)))
        };
        self.edit_table(s, i, |t| {
            t.totals = true;
            t.range.r2 += 1;
            let n = t.columns.len();
            if n > 1 && t.columns[0].total == TotalFn::None && t.columns[0].total_label.is_empty() {
                t.columns[0].total_label = "Total".into();
            }
            if let Some(last) = t.columns.last_mut() {
                if last.total == TotalFn::None && last.total_label.is_empty() {
                    last.total = if numeric_last { TotalFn::Sum } else { TotalFn::Count };
                }
            }
        });
    }

    fn toggle_header(&mut self, s: usize, i: usize) {
        let t = self.wb.sheets[s].tables[i].clone();
        if t.header {
            if t.data().rows() < 1 || t.range.rows() <= 1 + t.totals as u32 {
                return;
            }
            self.edit_table(s, i, |t| {
                t.header = false;
                t.range.r1 += 1;
            });
        } else {
            if t.range.r1 == 0 {
                return;
            }
            let above = Range::new(t.range.r1 - 1, t.range.c1, t.range.r1 - 1, t.range.c2);
            if self.sheet().iter_range(above).any(|(_, _, c)| !c.value.is_empty() || c.formula.is_some()) {
                self.status = Some("The row above the table isn't empty: clear it to show the header row.".into());
                return;
            }
            self.edit_table(s, i, |t| {
                t.header = true;
                t.range.r1 -= 1;
            });
        }
    }

    pub fn convert_table_to_range(&mut self, s: usize, i: usize) {
        self.push_undo(&[Scope::All]);
        table::unstructure(&mut self.wb, s, i);
        // The table's look stays on the cells, like Excel.
        let t = self.wb.sheets[s].tables[i].clone();
        for r in t.range.r1..=t.range.r2 {
            for c in t.range.c1..=t.range.c2 {
                let Some(l) = table::look(&t, r, c) else { continue };
                let old = self.wb.sheets[s].get(r, c).map_or(0, |x| x.style);
                let mut st = self.wb.style(old).clone();
                bake(&mut st, &l);
                let id = self.wb.intern_style(st);
                if id != old {
                    self.wb.sheets[s].cell_mut(r, c).style = id;
                }
            }
        }
        self.wb.sheets[s].tables.remove(i);
        self.changed();
    }

    fn rename_table_cmd(&mut self, s: usize, i: usize, new: &str) {
        let old = self.wb.sheets[s].tables[i].name.clone();
        let new = new.trim();
        if new == old {
            return;
        }
        let taken = table::find(&self.wb, new).is_some_and(|x| x != (s, i)) || self.wb.names.iter().any(|(n, _)| n.eq_ignore_ascii_case(new));
        if !table::valid_name(new) || taken {
            self.dialog = Some(super::dialogs::Dialog::Message {
                title: "Table Name".into(),
                text: format!("'{new}' can't be used as a table name: it is invalid or already in use."),
            });
            self.tables.name_for = None;
            return;
        }
        self.push_undo(&[Scope::All]);
        table::rename_table(&mut self.wb, &old, new);
        self.changed();
    }

    fn resize_table_cmd(&mut self, s: usize, i: usize, text: &str) {
        let t = self.wb.sheets[s].tables[i].clone();
        let Some(rg) = Range::parse(text.trim().trim_start_matches('=').rsplit('!').next().unwrap_or("")) else {
            self.status = Some("Enter a range such as A1:D10.".into());
            return;
        };
        let min_rows = 1 + t.header as u32 + t.totals as u32;
        let others = self.wb.sheets[s].tables.iter().enumerate().any(|(k, o)| k != i && o.range.intersects(&rg));
        if rg.r1 != t.range.r1 || rg.rows() < min_rows || others || rg.is_full_cols() || rg.is_full_rows() {
            self.dialog = Some(super::dialogs::Dialog::Message {
                title: "Resize Table".into(),
                text: "The headers must remain in the same row, the table needs at least one data row, and it can't overlap another table.".into(),
            });
            return;
        }
        self.push_undo(&[Scope::Sheet(s)]);
        let sh = &mut self.wb.sheets[s];
        let tb = &mut sh.tables[i];
        // Move the total row along with the bottom edge.
        let old_tot = tb.totals_row();
        tb.range = rg;
        let mut tb = tb.clone();
        if tb.header {
            table::sync_columns_from_cells(sh, &mut tb);
        } else {
            tb.columns.resize_with(rg.cols() as usize, Default::default);
            tb.fix_column_names();
        }
        for col in tb.columns.iter_mut() {
            if let Some(old) = t.columns.iter().find(|o| o.name == col.name) {
                col.total = old.total;
                col.total_label = old.total_label.clone();
            }
        }
        sh.tables[i] = tb;
        if let Some(r) = old_tot.filter(|r| *r != rg.r2) {
            for c in t.range.c1..=t.range.c2 {
                if let Some(cell) = self.wb.sheets[s].get_mut(r, c) {
                    cell.value = Value::Empty;
                    cell.formula = None;
                }
                self.wb.sheets[s].tidy(r, c);
            }
        }
        let after = self.wb.sheets[s].tables[i].clone();
        self.sync_table_rows(s, i, &t, &after);
        self.changed();
    }

    fn set_total_fn(&mut self, s: usize, i: usize, k: u32, f: TotalFn) {
        self.push_undo(&[Scope::Sheet(s)]);
        {
            let col = &mut self.wb.sheets[s].tables[i].columns[k as usize];
            col.total = f;
            col.total_label.clear();
        }
        self.write_total_cell(s, i, k as usize);
        self.changed();
    }

    // ------------------------------------------------------------------
    // Typing in and next to tables
    // ------------------------------------------------------------------

    /// Whether committing a value at (r, c) changes a table (header rename or growth), so
    /// the edit needs a whole-sheet undo step.
    pub fn table_edit_affects(&self, s: usize, r: u32, c: u32) -> bool {
        let sh = &self.wb.sheets[s];
        sh.tables.iter().any(|t| {
            t.header_row() == Some(r) && t.range.contains(r, c) || grows(t, r, c) || t.totals_row() == Some(r) && t.range.contains(r, c)
        })
    }

    /// After a cell was committed: header edits rename the column, entries right below or
    /// right of a table grow it. Returns true when a table changed.
    pub fn table_after_edit(&mut self, s: usize, r: u32, c: u32) -> bool {
        let Some(i) = self.wb.sheets[s].tables.iter().position(|t| t.range.contains(r, c) || grows(t, r, c)) else {
            return false;
        };
        let t = self.wb.sheets[s].tables[i].clone();
        if t.range.contains(r, c) {
            let k = (c - t.range.c1) as usize;
            if t.header_row() == Some(r) {
                let old = t.columns[k].name.clone();
                let mut tb = t.clone();
                tb.columns[k].name = table::header_text(self.wb.sheets[s].value(r, c));
                tb.fix_column_names();
                let new = tb.columns[k].name.clone();
                let style = self.wb.sheets[s].get(r, c).map_or(0, |x| x.style);
                self.wb.sheets[s].set(r, c, Some(Cell { value: Value::Text(new.clone()), formula: None, style }));
                self.wb.sheets[s].tables[i].columns[k].name = new.clone();
                if !old.eq_ignore_ascii_case(&new) {
                    table::rename_column(&mut self.wb, s, i, &old, &new);
                }
                return true;
            }
            if t.totals_row() == Some(r) {
                // A typed total replaces the dropdown function.
                let cell = self.wb.sheets[s].get(r, c).cloned();
                let col = &mut self.wb.sheets[s].tables[i].columns[k];
                match cell {
                    Some(x) if x.formula.is_some() => col.total = TotalFn::Custom,
                    Some(x) => {
                        col.total = TotalFn::None;
                        col.total_label = table::header_text(&x.value);
                    }
                    None => {
                        col.total = TotalFn::None;
                        col.total_label.clear();
                    }
                }
                return true;
            }
            return false;
        }
        if self.wb.sheets[s].get(r, c).is_none_or(|x| x.value.is_empty() && x.formula.is_none()) {
            return false;
        }
        if r == t.range.r2 + 1 {
            // New row: copy calculated-column formulas down from the last data row.
            let last = t.range.r2;
            self.wb.sheets[s].tables[i].range.r2 += 1;
            for cc in t.range.c1..=t.range.c2 {
                if cc == c || self.wb.sheets[s].get(r, cc).is_some() {
                    continue;
                }
                if let Some(prev) = self.wb.sheets[s].get(last, cc).cloned() {
                    if let Some(f) = &prev.formula {
                        let mut f = (**f).clone();
                        if let Some(ast) = &mut f.ast {
                            crate::formula::shift_relative(ast, 1, 0);
                            f = crate::model::Formula::from_ast(ast.clone());
                        }
                        self.wb.sheets[s].set(r, cc, Some(Cell { value: Value::Empty, formula: Some(Box::new(f)), style: prev.style }));
                    }
                }
            }
        } else {
            // New column.
            let tb = &mut self.wb.sheets[s].tables[i];
            tb.range.c2 += 1;
            tb.columns.push(Default::default());
            let mut tb = tb.clone();
            if tb.header {
                let hr = tb.range.r1;
                let typed = table::header_text(self.wb.sheets[s].value(hr, c));
                tb.columns.last_mut().unwrap().name = if typed.is_empty() { format!("Column{}", tb.columns.len()) } else { typed };
                tb.fix_column_names();
                let name = tb.columns.last().unwrap().name.clone();
                let style = self.wb.sheets[s].get(hr, c).map_or(0, |x| x.style);
                self.wb.sheets[s].set(hr, c, Some(Cell { value: Value::Text(name), formula: None, style }));
            } else {
                tb.fix_column_names();
            }
            self.wb.sheets[s].tables[i] = tb;
        }
        true
    }

    // ------------------------------------------------------------------
    // Painting
    // ------------------------------------------------------------------

    /// Table-style lines (header underline, total-row double line, row separators).
    pub(super) fn paint_table_borders(&self, p: &egui::Painter, lay: &Layout) {
        let sh = self.sheet();
        for t in &sh.tables {
            let vis = self.range_rect(lay, t.range);
            if !vis.intersects(lay.cells) {
                continue;
            }
            for r in t.range.r1..=t.range.r2 {
                let row = self.range_rect(lay, Range::new(r, t.range.c1, r, t.range.c2));
                if row.height() <= 0.0 || row.bottom() < lay.cells.top() || row.top() > lay.cells.bottom() {
                    continue;
                }
                let Some(l) = table::look(t, r, t.range.c1) else { continue };
                let line = |y: f32, (w, col): (BorderLine, u32)| {
                    let width = match w {
                        BorderLine::Medium => 2.0,
                        BorderLine::Thick => 3.0,
                        _ => 1.0,
                    };
                    let y = y.round() - 0.5;
                    p.line_segment([Pos2::new(row.left(), y), Pos2::new(row.right(), y)], Stroke::new(width, rgb(col)));
                    if w == BorderLine::Double {
                        p.line_segment([Pos2::new(row.left(), y + 2.0), Pos2::new(row.right(), y + 2.0)], Stroke::new(1.0, rgb(col)));
                    }
                };
                if let Some(b) = l.top {
                    line(row.top(), b);
                }
                if let Some(b) = l.bottom {
                    line(row.bottom(), b);
                }
            }
        }
    }

    /// Filter buttons of table headers (when the sheet has no AutoFilter of its own there).
    pub(super) fn paint_table_filter_buttons(&self, p: &egui::Painter, lay: &Layout) {
        for (t, btns) in self.table_filter_buttons(lay) {
            let _ = t;
            for (_, b) in btns {
                p.rect_filled(b, 2.0, Color32::from_gray(250));
                p.rect_stroke(b, 2.0, Stroke::new(1.0, Color32::from_gray(170)), StrokeKind::Inside);
                let c = b.center();
                p.add(egui::Shape::convex_polygon(
                    vec![Pos2::new(c.x - 3.5, c.y - 1.5), Pos2::new(c.x + 3.5, c.y - 1.5), Pos2::new(c.x, c.y + 2.5)],
                    Color32::from_gray(70),
                    Stroke::NONE,
                ));
            }
        }
    }

    fn table_filter_buttons(&self, lay: &Layout) -> Vec<(usize, Vec<(u32, Rect)>)> {
        let sh = self.sheet();
        let mut out = Vec::new();
        for (i, t) in sh.tables.iter().enumerate() {
            let Some(hr) = t.header_row().filter(|_| t.filter) else { continue };
            if sh.filter.as_ref().is_some_and(|f| f.range.r1 == hr && f.range.intersects(&t.range)) {
                continue; // drawn (and handled) as the sheet's AutoFilter
            }
            let mut v = Vec::new();
            for c in t.range.c1..=t.range.c2 {
                let cell = self.range_rect(lay, Range::cell(hr, c));
                if cell.width() <= 0.0 || !lay.cells.intersects(cell) {
                    continue;
                }
                v.push((c, Rect::from_min_max(Pos2::new(cell.right() - 17.0, cell.bottom() - 17.0), Pos2::new(cell.right() - 1.0, cell.bottom() - 1.0))));
            }
            out.push((i, v));
        }
        out
    }

    /// Dropdown arrow of the active total-row cell.
    fn total_arrow_rect(&self, lay: &Layout) -> Option<(usize, u32, Rect)> {
        let (r, c) = self.active();
        let (_, i) = self.active_table()?;
        let t = &self.sheet().tables[i];
        if t.totals_row() != Some(r) {
            return None;
        }
        let cell = self.range_rect(lay, Range::cell(r, c));
        if !lay.cells.contains(cell.center()) || cell.width() <= 0.0 {
            return None;
        }
        let h = cell.height().min(20.0 * self.zoom.max(0.75));
        Some((i, c - t.range.c1, Rect::from_min_size(Pos2::new(cell.right() + 1.0, cell.bottom() - h), Vec2::new(16.0, h))))
    }

    pub(super) fn paint_table_extras(&mut self, ui: &egui::Ui, lay: &Layout) {
        let cp = ui.painter().with_clip_rect(lay.cells);
        self.paint_table_filter_buttons(&cp, lay);
        if let Some((_, _, r)) = self.total_arrow_rect(lay) {
            let hover = ui.input(|i| i.pointer.hover_pos()).is_some_and(|h| r.contains(h));
            ui.painter().rect_filled(r, 0.0, if hover { Color32::from_gray(230) } else { Color32::from_gray(246) });
            ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_gray(160)), StrokeKind::Inside);
            let c = r.center();
            ui.painter().add(egui::Shape::convex_polygon(
                vec![Pos2::new(c.x - 3.5, c.y - 1.5), Pos2::new(c.x + 3.5, c.y - 1.5), Pos2::new(c.x, c.y + 2.5)],
                Color32::from_gray(60),
                Stroke::NONE,
            ));
        }
        self.total_menu_ui(ui.ctx());
    }

    fn total_menu_ui(&mut self, ctx: &egui::Context) {
        let Some((s, i, k, pos)) = self.tables.total_menu else { return };
        if s != self.wb.active || i >= self.sheet().tables.len() {
            self.tables.total_menu = None;
            return;
        }
        let cur = self.sheet().tables[i].columns.get(k as usize).map(|c| c.total).unwrap_or_default();
        let mut pick = None;
        let resp = egui::Area::new(egui::Id::new("table_total_menu")).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(130.0);
                for f in TotalFn::MENU {
                    if ui.selectable_label(f == cur, f.label()).clicked() {
                        pick = Some(f);
                    }
                }
            });
        });
        if let Some(f) = pick {
            self.tables.total_menu = None;
            self.set_total_fn(s, i, k, f);
        } else if resp.response.clicked_elsewhere() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.tables.total_menu = None;
        }
    }

    /// Mouse on table controls (total-row arrow, header filter buttons); true when handled.
    pub(super) fn table_pointer(&mut self, lay: &Layout, pos: Pos2, pressed: bool) -> bool {
        if !pressed {
            return false;
        }
        if let Some((i, k, r)) = self.total_arrow_rect(lay) {
            if r.contains(pos) {
                self.tables.total_menu = if self.tables.total_menu.is_some() { None } else { Some((self.wb.active, i, k, r.right_bottom())) };
                return true;
            }
        }
        for (i, btns) in self.table_filter_buttons(lay) {
            if let Some((c, _)) = btns.into_iter().find(|(_, b)| b.contains(pos)) {
                // The table's header acts as the sheet's AutoFilter (the file keeps the table's own).
                let t = self.sheet().tables[i].clone();
                let rg = Range { r2: t.range.r2 - t.totals as u32, ..t.range };
                if self.sheet().filter.as_ref().is_some_and(|f| f.range != rg) {
                    self.status = Some("Clear the sheet's AutoFilter to filter this table.".into());
                    return true;
                }
                if self.sheet().filter.is_none() {
                    self.sheet_mut().filter = Some(crate::model::AutoFilter::new(rg));
                }
                self.open_filter_popup(c, pos);
                return true;
            }
        }
        false
    }

    // ------------------------------------------------------------------
    // Ribbon
    // ------------------------------------------------------------------

    /// The contextual "Table Design" tab label, shown while the active cell is in a table.
    pub(super) fn table_tab_button(&mut self, ui: &mut egui::Ui) {
        if self.active_table().is_none() || self.selected_chart().is_some() {
            if self.tab == RibbonTab::TableDesign {
                self.tab = self.tables.prev_tab.take().unwrap_or(RibbonTab::Home);
            }
            return;
        }
        let sel = self.tab == RibbonTab::TableDesign && !self.ribbon_collapsed;
        let font = if sel { theme::ui_bold(13.0) } else { theme::ui_font(13.0) };
        let name = "Table Design";
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
        if resp.clicked() {
            if self.tab != RibbonTab::TableDesign {
                self.tables.prev_tab = Some(self.tab);
            }
            self.tab = RibbonTab::TableDesign;
            self.ribbon_collapsed = false;
        }
    }

    pub(super) fn table_design_tab(&mut self, ui: &mut egui::Ui) {
        let Some((s, i)) = self.active_table() else { return };
        let t = self.wb.sheets[s].tables[i].clone();
        let name_id = egui::Id::new("table_name_box");
        if self.tables.name_for != Some((s, i)) || !ui.memory(|m| m.has_focus(name_id)) {
            self.tables.name = t.name.clone();
            self.tables.name_for = Some((s, i));
        }
        if self.tables.resize_for != Some((s, i, t.range)) {
            self.tables.resize = abs_range(t.range);
            self.tables.resize_for = Some((s, i, t.range));
        }
        self.group(ui, "Properties", |app, ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Table Name:").size(12.0));
                let r = ui.add(egui::TextEdit::singleline(&mut app.tables.name).id(name_id).desired_width(120.0));
                if r.lost_focus() {
                    let n = app.tables.name.clone();
                    app.rename_table_cmd(s, i, &n);
                }
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut app.tables.resize).desired_width(84.0)).on_hover_text("New table range");
                    if ui.button("Resize").on_hover_text("Resize Table: the headers must stay in the same row").clicked() {
                        let txt = app.tables.resize.clone();
                        app.resize_table_cmd(s, i, &txt);
                    }
                });
            });
        });
        self.group(ui, "Tools", |app, ui| {
            if ui.add(egui::Button::new(RichText::new("Convert\nto Range").size(12.0)).min_size(Vec2::new(64.0, 56.0))).on_hover_text("Convert this table into a normal range of cells").clicked() {
                app.convert_table_to_range(s, i);
            }
        });
        self.group(ui, "Table Style Options", |app, ui| {
            let check = |ui: &mut egui::Ui, label: &str, on: bool| -> bool {
                let mut v = on;
                ui.checkbox(&mut v, RichText::new(label).size(12.0)).changed()
            };
            ui.vertical(|ui| {
                if check(ui, "Header Row", t.header) {
                    app.toggle_header(s, i);
                }
                if check(ui, "Total Row", t.totals) {
                    app.toggle_totals(s, i);
                }
                if check(ui, "Banded Rows", t.banded_rows) {
                    app.edit_table(s, i, |t| t.banded_rows = !t.banded_rows);
                }
            });
            ui.vertical(|ui| {
                if check(ui, "First Column", t.first_col) {
                    app.edit_table(s, i, |t| t.first_col = !t.first_col);
                }
                if check(ui, "Last Column", t.last_col) {
                    app.edit_table(s, i, |t| t.last_col = !t.last_col);
                }
                if check(ui, "Banded Columns", t.banded_cols) {
                    app.edit_table(s, i, |t| t.banded_cols = !t.banded_cols);
                }
            });
            ui.vertical(|ui| {
                if check(ui, "Filter Button", t.filter) {
                    app.edit_table(s, i, |t| t.filter = !t.filter);
                }
            });
        });
        self.group(ui, "Table Styles", |app, ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(2.0, 2.0);
                for kind in 0..3u8 {
                    ui.horizontal(|ui| {
                        for ai in 0..7 {
                            let name = table::style_name(kind, ai);
                            let (rect, resp) = ui.allocate_exact_size(Vec2::new(30.0, 18.0), Sense::click());
                            paint_style_swatch(ui.painter(), rect, &name, t.style == name || resp.hovered());
                            if resp.on_hover_text(&name).clicked() {
                                app.edit_table(s, i, |t| t.style = name.clone());
                            }
                        }
                    });
                }
            });
        });
    }
}

/// Whether an entry at (r, c) grows table `t` (directly below it, or directly right of it).
fn grows(t: &Table, r: u32, c: u32) -> bool {
    let below = !t.totals && r == t.range.r2 + 1 && c >= t.range.c1 && c <= t.range.c2;
    let right = c == t.range.c2 + 1 && r >= t.range.r1 && r <= t.range.r2 && c < crate::model::MAX_COLS;
    below || right
}

fn abs_range(rg: Range) -> String {
    let p = |r: u32, c: u32| format!("${}${}", crate::model::col_name(c), r + 1);
    format!("{}:{}", p(rg.r1, rg.c1), p(rg.r2, rg.c2))
}

/// Puts a table-style look into a cell style (Convert to Range keeps the formatting).
fn bake(st: &mut Style, l: &Look) {
    if st.fill.is_none() {
        st.fill = l.fill;
    }
    if st.color.is_none() {
        st.color = l.color.filter(|c| *c != 0);
    }
    if l.bold {
        st.bold = true;
    }
    if let Some((w, _)) = l.top {
        if st.border.top == BorderLine::None {
            st.border.top = w;
        }
    }
    if let Some((w, _)) = l.bottom {
        if st.border.bottom == BorderLine::None {
            st.border.bottom = w;
        }
    }
}

/// Applies a table look under a cell's own formatting (direct formatting wins, like Excel).
pub fn apply_look(st: &mut Style, l: &Look) {
    if st.fill.is_none() {
        st.fill = l.fill;
    }
    if st.color.is_none() {
        st.color = l.color;
    }
    if l.bold {
        st.bold = true;
    }
}

/// Small preview of a table style: header, two body bands.
fn paint_style_swatch(p: &egui::Painter, rect: Rect, name: &str, hot: bool) {
    let mut t = Table::new("x", Range::new(0, 0, 3, 2), name);
    t.banded_rows = true;
    let rh = rect.height() / 4.0;
    for r in 0..4u32 {
        let row = Rect::from_min_size(Pos2::new(rect.left(), rect.top() + r as f32 * rh), Vec2::new(rect.width(), rh));
        let l = table::look(&t, r, 0).unwrap_or_default();
        p.rect_filled(row, 0.0, l.fill.map(rgb).unwrap_or(Color32::WHITE));
        if let Some((_, c)) = l.bottom {
            p.line_segment([row.left_bottom(), row.right_bottom()], Stroke::new(1.0, rgb(c)));
        }
    }
    p.rect_stroke(rect, 0.0, Stroke::new(if hot { 2.0 } else { 1.0 }, if hot { theme::accent() } else { Color32::from_gray(200) }), StrokeKind::Inside);
}
