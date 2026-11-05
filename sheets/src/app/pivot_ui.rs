//! PivotTables in the app: Insert PivotTable, updating and refreshing reports, the report's
//! look and buttons on the grid, item filters, expand/collapse, Show Details, GETPIVOTDATA
//! pointing and the PivotTable dialogs.

use eframe::egui::{self, Align2, Color32, Pos2, Rect, RichText, Stroke, StrokeKind, Vec2};

use super::data_ui::{ok_cancel, window};
use super::dialogs::Dialog;
use super::grid::Layout;
use super::theme::{self, rgb};
use super::{App, RibbonTab, Scope};
use crate::model::{col_name, BorderLine, Cell, Range, Sheet, Style, Value};
use crate::pivot::engine::{self, LineKind, Role};
use crate::pivot::{self, ikey, Agg, Area, BaseItem, DatePart, FieldSource, PivotTable, ShowAs, VALUES};

pub const PIVOT_MSG: &str = "Cannot change this part of a PivotTable report.";

/// A field being dragged in the field list: the field and the area entry it came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragSrc {
    pub field: usize,
    pub from: Option<(Area, usize)>,
}

#[derive(Default)]
pub struct PivotUi {
    pub(super) prev_tab: Option<RibbonTab>,
    /// Field List switched off from the ribbon or closed.
    pub(super) hide_list: bool,
    pub(super) search: String,
    pub(super) defer: bool,
    /// Deferred layout changes: (sheet, report, edited definition).
    pub(super) pending: Option<(usize, usize, PivotTable)>,
    pub(super) dialog: Option<PDialog>,
    popup: Option<ItemPopup>,
    pub(super) name: String,
    pub(super) name_for: Option<(usize, usize)>,
    pub(super) drag: Option<DragSrc>,
    /// "Generate GetPivotData" switched off.
    pub(super) no_gpd: bool,
}

pub(super) enum PDialog {
    Create { source: String, new_sheet: bool, location: String, error: Option<String> },
    ValueField(ValueDlg),
    FieldSettings { si: usize, pi: usize, f: usize, caption: String, subtotal: bool, sort: pivot::SortOrder },
    Options { si: usize, pi: usize, name: String, empty: String, autofit: bool, row_grand: bool, col_grand: bool, buttons: bool, headers: bool },
    Group { si: usize, pi: usize, f: usize, dates: bool, start: String, end: String, by: String, parts: [bool; 4] },
    Move { si: usize, pi: usize, new_sheet: bool, location: String, error: Option<String> },
    Source { si: usize, pi: usize, source: String, error: Option<String> },
}

pub(super) struct ValueDlg {
    si: usize,
    pi: usize,
    d: usize,
    name: String,
    agg: Agg,
    show_as: ShowAs,
    base_field: usize,
    base_item: BaseItem,
    num_fmt: String,
    tab: u8,
}

/// Item filter dropdown of a field (or the page field list).
struct ItemPopup {
    si: usize,
    pi: usize,
    /// Field being filtered and the fields offered in "Select field".
    f: usize,
    choices: Vec<usize>,
    pos: Pos2,
    search: String,
    /// (label, key, checked)
    items: Vec<(String, String, bool)>,
    page: bool,
    multi: bool,
    sel: Option<String>,
    fresh: bool,
}

/// The line whose expand/collapse button sits in cell (r, c): (row axis, line).
fn button_line(pt: &PivotTable, r: u32, c: u32) -> Option<(bool, usize)> {
    if !pt.show_buttons {
        return None;
    }
    let (rows, li, d) = match pt.out.role(r, c) {
        Role::RowLabel(li, d) => (true, li as usize, d as usize),
        Role::ColLabel(li, d) => (false, li as usize, d as usize),
        _ => return None,
    };
    let l = if rows { pt.out.rows.get(li)? } else { pt.out.cols.get(li)? };
    (l.kind == LineKind::Item && l.children && l.depth == d).then_some((rows, li))
}

/// Indent of a report label: its level (compact form) plus room for the button.
fn label_indent(pt: &PivotTable, r: u32, c: u32) -> u8 {
    pt.out.indent(r, c) + button_line(pt, r, c).is_some() as u8
}

/// The field a filter button stands for: (field, fields offered, page field).
fn filter_target(pt: &PivotTable, role: Role) -> Option<(usize, Vec<usize>, bool)> {
    let real = |v: &[usize]| v.iter().copied().filter(|f| *f != VALUES).collect::<Vec<_>>();
    match role {
        Role::RowHeader => {
            let fs = real(&pt.rows);
            Some((*fs.first()?, fs, false))
        }
        Role::ColHeader => {
            let fs = real(&pt.cols);
            Some((*fs.first()?, fs, false))
        }
        Role::FieldHeader(axis, k) => {
            let f = *if axis == 0 { pt.rows.get(k as usize)? } else { pt.cols.get(k as usize)? };
            (f != VALUES).then(|| (f, vec![f], false))
        }
        Role::PageValue(k) => {
            let f = *pt.filters.get(k as usize)?;
            Some((f, vec![f], true))
        }
        _ => None,
    }
}

fn field_filtered(pt: &PivotTable, f: usize) -> bool {
    let fl = &pt.fields[f];
    !fl.hidden.is_empty() || fl.page.is_some()
}

fn abs_cell(r: u32, c: u32) -> String {
    format!("${}${}", col_name(c), r + 1)
}

impl App {
    // ------------------------------------------------------------------
    // Lookup
    // ------------------------------------------------------------------

    /// (sheet, report) of the PivotTable holding the active cell.
    pub fn active_pivot(&self) -> Option<(usize, usize)> {
        let (r, c) = self.active();
        let si = self.wb.active;
        pivot::at(&self.wb, si, r, c).map(|i| (si, i))
    }

    /// PivotTable style look of a report cell and its indent level.
    pub fn pivot_look(&self, si: usize, r: u32, c: u32) -> Option<(crate::table::Look, u8)> {
        let pt = self.wb.sheets[si].pivots.iter().find(|p| p.out.drawn && p.out.full.contains(r, c))?;
        let l = pivot::style::look(pt, r, c)?;
        Some((l, label_indent(pt, r, c)))
    }

    /// The definition the field list shows: the deferred copy, or the report itself.
    pub(super) fn pane_pivot(&self, si: usize, pi: usize) -> PivotTable {
        match &self.pivots.pending {
            Some((s, i, p)) if *s == si && *i == pi => p.clone(),
            _ => self.wb.sheets[si].pivots[pi].clone(),
        }
    }

    // ------------------------------------------------------------------
    // Commands
    // ------------------------------------------------------------------

    /// Insert > PivotTable: the "PivotTable from table or range" dialog.
    pub fn open_create_pivot(&mut self) {
        if !self.commit_edit(false) {
            return;
        }
        let source = if let Some((s, i)) = self.active_table() {
            let _ = s;
            self.sheet().tables[i].name.clone()
        } else {
            let rg = if self.sel().is_single() { self.current_region() } else { self.sel_clipped() };
            pivot::source_text(&self.wb, self.wb.active, rg)
        };
        self.pivots.dialog = Some(PDialog::Create { source, new_sheet: true, location: String::new(), error: None });
    }

    fn create_pivot(&mut self, source: &str, new_sheet: bool, location: &str) -> Result<(), String> {
        let si0 = self.wb.active;
        let source = source.trim().trim_start_matches('=');
        let cache = pivot::build_cache(&self.wb, source, si0)?;
        let src = if crate::table::find(&self.wb, source).is_some() {
            source.to_string()
        } else {
            let (s, rg) = pivot::resolve_source(&self.wb, source, si0).ok_or("Data source reference is not valid.")?;
            pivot::source_text(&self.wb, s, rg)
        };
        let (target, anchor) = if new_sheet {
            (None, (2u32, 0u32))
        } else {
            let (s, rg) = pivot::resolve_source(&self.wb, location, si0).ok_or("Destination reference is not valid.")?;
            (Some(s), (rg.r1, rg.c1))
        };
        if let Some(s) = target {
            let probe = Range::new(anchor.0, anchor.1, anchor.0 + 17, anchor.1 + 2);
            if self.wb.sheets[s].pivots.iter().any(|p| p.out.full.intersects(&probe)) {
                return Err(engine::OVERLAP.into());
            }
        }
        self.push_undo(&[Scope::All]);
        let si = match target {
            Some(s) => s,
            None => {
                let name = self.wb.unique_sheet_name("Sheet");
                self.wb.sheets.insert(si0, Sheet::new(&name));
                si0
            }
        };
        let name = pivot::unique_name(&self.wb);
        self.wb.sheets[si].pivots.push(PivotTable::new(&name, &src, cache, anchor));
        let pi = self.wb.sheets[si].pivots.len() - 1;
        if let Err(e) = engine::update(&mut self.wb, si, pi) {
            self.undo();
            self.redo.clear();
            return Err(e);
        }
        self.wb.active = si;
        self.select(anchor.0, anchor.1, false);
        self.pivots.hide_list = false;
        self.pivots.pending = None;
        self.pivots.prev_tab = Some(self.tab);
        self.tab = RibbonTab::PivotAnalyze;
        self.changed();
        Ok(())
    }

    /// Applies an undoable change to a report and lays it out again.
    pub fn pivot_edit(&mut self, si: usize, pi: usize, f: impl FnOnce(&mut PivotTable)) -> bool {
        if !self.commit_edit(false) {
            return false;
        }
        if si >= self.wb.sheets.len() || pi >= self.wb.sheets[si].pivots.len() {
            return false;
        }
        self.push_undo(&[Scope::Sheet(si)]);
        f(&mut self.wb.sheets[si].pivots[pi]);
        self.finish_pivot_update(si, pi, None)
    }

    /// Lays out the report after a change already made (undo step recorded by the caller).
    fn finish_pivot_update(&mut self, si: usize, pi: usize, refresh: Option<()>) -> bool {
        let r = if refresh.is_some() { engine::refresh(&mut self.wb, si, pi) } else { engine::update(&mut self.wb, si, pi) };
        match r {
            Ok(()) => {
                self.pivot_autofit(si, pi);
                self.changed();
                true
            }
            Err(e) => {
                self.undo();
                self.redo.clear();
                self.dialog = Some(Dialog::Message { title: "FreedomOffice Sheets".into(), text: e });
                false
            }
        }
    }

    /// Changes from the field list: deferred while "Defer Layout Update" is on.
    pub(super) fn pane_edit(&mut self, si: usize, pi: usize, f: impl FnOnce(&mut PivotTable)) {
        if self.pivots.defer {
            let mut p = self.pane_pivot(si, pi);
            f(&mut p);
            self.pivots.pending = Some((si, pi, p));
        } else {
            self.pivot_edit(si, pi, f);
        }
    }

    /// Defer Layout Update > Update.
    pub(super) fn apply_pending(&mut self) {
        if let Some((si, pi, p)) = self.pivots.pending.take() {
            if si < self.wb.sheets.len() && pi < self.wb.sheets[si].pivots.len() {
                self.pivot_edit(si, pi, move |pt| {
                    let out = std::mem::take(&mut pt.out);
                    *pt = p;
                    pt.out = out;
                });
            }
        }
    }

    /// Fits the report's columns to their contents (Autofit column widths on update).
    fn pivot_autofit(&mut self, si: usize, pi: usize) {
        let Some(ctx) = self.ctx.clone() else { return };
        let pt = &self.wb.sheets[si].pivots[pi];
        if !pt.autofit || pt.out.placeholder {
            return;
        }
        let rg = pt.out.full;
        let mut widths: Vec<(u32, f32)> = Vec::new();
        for c in rg.c1..=rg.c2 {
            let mut w: f32 = 0.0;
            for r in rg.r1..=rg.r2 {
                if matches!(pt.out.role(r, c), Role::PageLabel(_) | Role::PageValue(_)) {
                    continue;
                }
                let sh = &self.wb.sheets[si];
                let Some(cell) = sh.get(r, c) else { continue };
                let text = crate::numfmt::format_value(&cell.value, &self.wb.style(cell.style).num_fmt).text;
                if text.is_empty() {
                    continue;
                }
                let bold = pivot::style::look(pt, r, c).is_some_and(|l| l.bold);
                let tw = ctx.fonts_mut(|f| f.layout_no_wrap(text, egui::FontId::proportional(14.7), Color32::BLACK).size().x) * if bold { 1.07 } else { 1.0 };
                let btn = if matches!(pt.out.role(r, c), Role::RowHeader | Role::ColHeader | Role::FieldHeader(..)) { 18.0 } else { 0.0 };
                w = w.max(tw + label_indent(pt, r, c) as f32 * 9.0 + btn + 10.0);
            }
            if w > 0.0 {
                widths.push((c, w.ceil()));
            }
        }
        let sh = &mut self.wb.sheets[si];
        for (c, w) in widths {
            sh.col_w.insert(c, w);
        }
    }

    /// Refresh (Alt+F5): reads the source again.
    pub fn pivot_refresh_active(&mut self) {
        let Some((si, pi)) = self.active_pivot() else {
            self.status = Some("Select a cell in a PivotTable to refresh it.".into());
            return;
        };
        if !self.commit_edit(false) {
            return;
        }
        self.push_undo(&[Scope::Sheet(si)]);
        self.finish_pivot_update(si, pi, Some(()));
    }

    /// Refresh All (Ctrl+Alt+F5).
    pub fn pivot_refresh_all(&mut self) {
        if !self.commit_edit(false) {
            return;
        }
        let all: Vec<(usize, usize)> = self.wb.sheets.iter().enumerate().flat_map(|(s, sh)| (0..sh.pivots.len()).map(move |i| (s, i))).collect();
        if all.is_empty() {
            return;
        }
        self.push_undo(&[Scope::All]);
        for (si, pi) in all {
            if let Err(e) = engine::refresh(&mut self.wb, si, pi) {
                self.dialog = Some(Dialog::Message { title: "FreedomOffice Sheets".into(), text: e });
                break;
            }
            self.pivot_autofit(si, pi);
        }
        self.changed();
    }

    /// True (after telling the user) when the range covers part of a PivotTable report.
    /// Deleting a whole selected report removes it, like Excel.
    pub fn pivot_block(&mut self, rg: Range) -> bool {
        let si = self.wb.active;
        let hits: Vec<usize> = self.sheet().pivots.iter().enumerate().filter(|(_, p)| p.out.drawn && p.out.full.intersects(&rg)).map(|(i, _)| i).collect();
        if hits.is_empty() {
            return false;
        }
        let delete = self.ctx.as_ref().is_some_and(|c| c.input(|i| i.key_pressed(egui::Key::Delete)));
        let whole = hits.iter().all(|&i| {
            let f = self.sheet().pivots[i].out.full;
            rg.contains(f.r1, f.c1) && rg.contains(f.r2, f.c2)
        });
        if delete && whole {
            self.push_undo(&[Scope::Sheet(si)]);
            for &i in hits.iter().rev() {
                engine::delete(&mut self.wb, si, i);
            }
            self.changed();
            return true;
        }
        self.dialog = Some(Dialog::Message { title: "FreedomOffice Sheets".into(), text: PIVOT_MSG.into() });
        true
    }

    /// The GETPIVOTDATA call for a value cell clicked while typing a formula.
    pub fn pivot_point_text(&self, r: u32, c: u32) -> Option<String> {
        if self.pivots.no_gpd {
            return None;
        }
        let si = self.wb.active;
        let pi = pivot::at(&self.wb, si, r, c)?;
        let pt = &self.wb.sheets[si].pivots[pi];
        let Role::Value(li, cj) = pt.out.role(r, c) else { return None };
        let e = self.edit.as_ref()?;
        let mut anchor = abs_cell(pt.out.range.r1, pt.out.range.c1);
        if e.sheet != si {
            anchor = format!("{}!{}", crate::formula::quote_sheet(&self.wb.sheets[si].name), anchor);
        }
        engine::getpivotdata_formula(pt, li as usize, cj as usize, &anchor)
    }

    /// Expands or collapses an item (by its line).
    fn toggle_item(&mut self, si: usize, pi: usize, rows: bool, li: usize) {
        let pt = &self.wb.sheets[si].pivots[pi];
        let line = if rows { pt.out.rows.get(li) } else { pt.out.cols.get(li) };
        let Some((f, it)) = line.and_then(|l| l.path.last()).cloned() else { return };
        let k = ikey(&it.label);
        self.pivot_edit(si, pi, |pt| {
            let set = &mut pt.fields[f].collapsed;
            if !set.remove(&k) {
                set.insert(k);
            }
        });
    }

    /// Expand Field / Collapse Field: every item of the active field.
    pub(super) fn expand_field(&mut self, expand: bool) {
        let Some((si, pi)) = self.active_pivot() else { return };
        let Some(f) = self.active_field() else { return };
        let fi = engine::field_items(&self.wb.sheets[si].pivots[pi], f);
        self.pivot_edit(si, pi, |pt| {
            if expand {
                pt.fields[f].collapsed.clear();
            } else {
                pt.fields[f].collapsed = fi.items.iter().map(|i| ikey(&i.label)).collect();
            }
        });
    }

    /// The field of the active report cell (Active Field).
    pub(super) fn active_field(&self) -> Option<usize> {
        let (si, pi) = self.active_pivot()?;
        let pt = &self.wb.sheets[si].pivots[pi];
        let (r, c) = self.active();
        let role = pt.out.role(r, c);
        match role {
            Role::RowLabel(li, d) => {
                let f = *pt.rows.get(d as usize)?;
                let _ = li;
                (f != VALUES).then_some(f)
            }
            Role::ColLabel(li, d) => {
                let l = pt.out.cols.get(li as usize)?;
                let f = *pt.cols.get(if l.kind == LineKind::Grand { 0 } else { d as usize })?;
                (f != VALUES).then_some(f)
            }
            Role::PageLabel(k) | Role::PageValue(k) => pt.filters.get(k as usize).copied(),
            r => filter_target(pt, r).map(|t| t.0),
        }
    }

    /// The data field of the active report cell.
    pub(super) fn active_data(&self) -> Option<usize> {
        let (si, pi) = self.active_pivot()?;
        let pt = &self.wb.sheets[si].pivots[pi];
        let (r, c) = self.active();
        let o = &pt.out;
        match o.role(r, c) {
            Role::Value(li, cj) => Some(o.rows[li as usize].data.or(o.cols[cj as usize].data).unwrap_or(0)),
            Role::Caption => Some(0),
            Role::ColLabel(li, _) if o.cols.get(li as usize).is_some_and(|l| l.data.is_some() || l.kind == LineKind::Total) => o.cols[li as usize].data.or(Some(0)),
            Role::RowLabel(li, d) if pt.rows.get(d as usize) == Some(&VALUES) => o.rows[li as usize].data,
            _ => None,
        }
        .filter(|d| *d < pt.values.len())
    }

    /// Field Settings for the active field (or Value Field Settings on a value).
    pub(super) fn open_field_settings(&mut self) {
        let Some((si, pi)) = self.active_pivot() else { return };
        if let Some(d) = self.active_data() {
            self.open_value_settings(si, pi, d);
        } else if let Some(f) = self.active_field() {
            self.open_field_dialog(si, pi, f);
        }
    }

    pub(super) fn open_value_settings(&mut self, si: usize, pi: usize, d: usize) {
        let pt = self.pane_pivot(si, pi);
        let Some(df) = pt.values.get(d) else { return };
        let base_field = if df.show_as.needs_field() { df.base_field } else { pt.rows.iter().chain(pt.cols.iter()).copied().find(|f| *f != VALUES).unwrap_or(0) };
        self.pivots.dialog = Some(PDialog::ValueField(ValueDlg {
            si,
            pi,
            d,
            name: df.name.clone(),
            agg: df.agg,
            show_as: df.show_as,
            base_field,
            base_item: df.base_item.clone(),
            num_fmt: df.num_fmt.clone(),
            tab: if df.show_as == ShowAs::Normal { 0 } else { 1 },
        }));
    }

    pub(super) fn open_field_dialog(&mut self, si: usize, pi: usize, f: usize) {
        let pt = self.pane_pivot(si, pi);
        let fl = &pt.fields[f];
        self.pivots.dialog = Some(PDialog::FieldSettings { si, pi, f, caption: fl.display().to_string(), subtotal: fl.subtotal, sort: fl.sort });
    }

    pub(super) fn open_pivot_options(&mut self) {
        let Some((si, pi)) = self.active_pivot() else { return };
        let pt = &self.wb.sheets[si].pivots[pi];
        self.pivots.dialog = Some(PDialog::Options {
            si,
            pi,
            name: pt.name.clone(),
            empty: pt.empty_text.clone(),
            autofit: pt.autofit,
            row_grand: pt.row_grand,
            col_grand: pt.col_grand,
            buttons: pt.show_buttons,
            headers: pt.field_headers,
        });
    }

    /// Group Selection / Group Field: the Grouping dialog for a date or number field.
    pub(super) fn open_group_dialog(&mut self) {
        let Some((si, pi)) = self.active_pivot() else { return };
        let Some(f) = self.active_field() else {
            self.status = Some("Select an item of a row or column field to group.".into());
            return;
        };
        let pt = &self.wb.sheets[si].pivots[pi];
        let col = pt.fields[f].column();
        let nums: Vec<f64> = pt.cache.records.iter().filter_map(|r| r.get(col).and_then(|v| v.as_number())).collect();
        let only_nums = pt.cache.is_numeric(col);
        if nums.is_empty() || !only_nums {
            self.dialog = Some(Dialog::Message { title: "FreedomOffice Sheets".into(), text: "Cannot group that selection.".into() });
            return;
        }
        let (lo, hi) = nums.iter().fold((f64::MAX, f64::MIN), |(a, b), x| (a.min(*x), b.max(*x)));
        let dates = pt.cache.is_date(col) || matches!(pt.fields[f].source, FieldSource::Date(..));
        let base = pt.fields.iter().position(|x| x.source == FieldSource::Column(col)).unwrap_or(f);
        let (start, end, by, parts) = if dates {
            let have: Vec<DatePart> = pt.fields.iter().filter_map(|x| match x.source {
                FieldSource::Date(c, p) if c == col => Some(p),
                _ => None,
            }).collect();
            let parts = [have.contains(&DatePart::Days), have.contains(&DatePart::Months) || have.is_empty(), have.contains(&DatePart::Quarters), have.contains(&DatePart::Years)];
            (crate::numfmt::format_value(&Value::Number(lo), "m/d/yyyy").text, crate::numfmt::format_value(&Value::Number(hi + 1.0), "m/d/yyyy").text, String::new(), parts)
        } else {
            let (s, e, b) = pt.fields[f].num_group.unwrap_or((lo.floor(), hi.ceil(), 10.0));
            (crate::numfmt::general(s), crate::numfmt::general(e), crate::numfmt::general(b), [false; 4])
        };
        self.pivots.dialog = Some(PDialog::Group { si, pi, f: base, dates, start, end, by, parts });
    }

    /// Ungroup the active field.
    pub(super) fn ungroup_active(&mut self) {
        let Some((si, pi)) = self.active_pivot() else { return };
        let Some(f) = self.active_field() else { return };
        let pt = &self.wb.sheets[si].pivots[pi];
        let grouped = pt.fields[f].num_group.is_some() || pt.fields.iter().any(|x| matches!(x.source, FieldSource::Date(c, _) if c == pt.fields[f].column()));
        if !grouped {
            self.dialog = Some(Dialog::Message { title: "FreedomOffice Sheets".into(), text: "Cannot ungroup that selection.".into() });
            return;
        }
        self.pivot_edit(si, pi, |pt| pt.ungroup(f));
    }

    /// Show Details: the records behind a value on a new sheet, as a table.
    fn show_details(&mut self, si: usize, pi: usize, li: usize, cj: usize) {
        let pt = self.wb.sheets[si].pivots[pi].clone();
        let recs = engine::detail_records(&pt, li, cj);
        self.push_undo(&[Scope::All]);
        let name = self.wb.unique_sheet_name("Sheet");
        let mut sh = Sheet::new(&name);
        let ncols = pt.cache.names.len() as u32;
        for (k, n) in pt.cache.names.iter().enumerate() {
            sh.set(0, k as u32, Some(Cell { value: Value::Text(n.clone()), ..Default::default() }));
        }
        let styles: Vec<u32> = pt.cache.formats.iter().map(|f| if f.is_empty() { 0 } else { self.wb.intern_style(Style { num_fmt: f.clone(), ..Default::default() }) }).collect();
        for (i, &ri) in recs.iter().enumerate() {
            for (k, v) in pt.cache.records[ri].iter().enumerate() {
                let style = styles.get(k).copied().unwrap_or(0);
                if !v.is_empty() || style != 0 {
                    sh.set(i as u32 + 1, k as u32, Some(Cell { value: v.clone(), formula: None, style }));
                }
            }
        }
        if ncols > 0 {
            let rg = Range::new(0, 0, recs.len().max(1) as u32, ncols - 1);
            let tname = crate::table::unique_name(&self.wb, "Table");
            let mut t = crate::table::Table::new(&tname, rg, "TableStyleMedium2");
            crate::table::sync_columns_from_cells(&sh, &mut t);
            sh.tables.push(t);
            for c in 0..ncols {
                let w = (0..=recs.len().min(500) as u32)
                    .filter_map(|r| sh.get(r, c).map(|x| crate::numfmt::format_value(&x.value, &self.wb.style(x.style).num_fmt).text.chars().count()))
                    .max()
                    .unwrap_or(0);
                sh.col_w.insert(c, (w as f32 * 7.5 + 28.0).max(64.0));
            }
        }
        let at = self.wb.active;
        self.wb.sheets.insert(at, sh);
        self.wb.active = at;
        self.select(0, 0, false);
        self.changed();
    }

    /// Double-click in a report: Show Details on values, expand/collapse on items, settings on captions.
    /// Returns true when the click was the report's.
    pub fn pivot_double_click(&mut self, r: u32, c: u32) -> bool {
        let si = self.wb.active;
        let Some(pi) = pivot::at(&self.wb, si, r, c) else { return false };
        let pt = self.wb.sheets[si].pivots[pi].clone();
        let role = pt.out.role(r, c);
        self.select(r, c, false);
        match role {
            Role::Value(li, cj) => self.show_details(si, pi, li as usize, cj as usize),
            Role::RowLabel(li, d) | Role::ColLabel(li, d) => {
                let rows = matches!(role, Role::RowLabel(..));
                let line = if rows { &pt.out.rows[li as usize] } else { &pt.out.cols[li as usize] };
                if line.kind == LineKind::Item && line.children && line.depth == d as usize {
                    self.toggle_item(si, pi, rows, li as usize);
                } else if let Some(dd) = self.active_data() {
                    self.open_value_settings(si, pi, dd);
                }
            }
            Role::Caption => self.open_value_settings(si, pi, 0),
            Role::FieldHeader(..) | Role::RowHeader | Role::ColHeader => {
                if let Some((f, _, _)) = filter_target(&pt, role) {
                    self.open_field_dialog(si, pi, f);
                }
            }
            _ => {}
        }
        true
    }

    // ------------------------------------------------------------------
    // Grid: buttons and painting
    // ------------------------------------------------------------------

    fn cell_rect(&self, lay: &Layout, r: u32, c: u32) -> Option<Rect> {
        let &(_, t, h) = lay.rows.iter().find(|x| x.0 == r)?;
        let &(_, l, w) = lay.cols.iter().find(|x| x.0 == c)?;
        Some(Rect::from_min_size(Pos2::new(l, t), Vec2::new(w, h)))
    }

    /// Expand/collapse button of a label cell.
    fn button_rect(&self, pt: &PivotTable, cell: Rect, r: u32, c: u32) -> Rect {
        let z = self.zoom;
        let s = (9.0 * z).round().max(7.0);
        let x = cell.left() + 3.0 * z + pt.out.indent(r, c) as f32 * 9.0 * z;
        Rect::from_min_size(Pos2::new(x.round() + 0.5, (cell.center().y - s / 2.0).round() + 0.5), Vec2::splat(s - 1.0))
    }

    fn filter_rect(cell: Rect) -> Rect {
        Rect::from_min_max(Pos2::new(cell.right() - 17.0, cell.bottom() - 17.0), Pos2::new(cell.right() - 1.0, cell.bottom() - 1.0))
    }

    /// Lines, buttons and the empty-report placeholder.
    pub(super) fn paint_pivot_extras(&mut self, ui: &egui::Ui, lay: &Layout) {
        let si = self.wb.active;
        let cp = ui.painter().with_clip_rect(lay.cells);
        let z = self.zoom;
        let hover = ui.input(|i| i.pointer.hover_pos());
        for pt in &self.wb.sheets[si].pivots {
            if !pt.out.drawn {
                continue;
            }
            let full = pt.out.full;
            if !self.range_rect(lay, full).intersects(lay.cells) {
                continue;
            }
            if pt.out.placeholder {
                let rr = self.range_rect(lay, pt.out.range);
                cp.rect_filled(rr, 0.0, Color32::from_gray(245));
                cp.rect_stroke(rr.shrink(0.5), 0.0, Stroke::new(1.0, Color32::from_gray(150)), StrokeKind::Inside);
                let font = egui::FontId::proportional((13.0 * z).max(8.0));
                cp.text(Pos2::new(rr.center().x, rr.top() + rr.height() * 0.3), Align2::CENTER_CENTER, &pt.name, egui::FontId::proportional((15.0 * z).max(8.0)), Color32::from_gray(60));
                let g = cp.layout(
                    "To build a report, choose fields from the PivotTable Field List".into(),
                    font,
                    Color32::from_gray(80),
                    (rr.width() - 16.0).max(20.0),
                );
                let pos = Pos2::new(rr.center().x - g.size().x / 2.0, rr.top() + rr.height() * 0.42);
                cp.galley(pos, g, Color32::from_gray(80));
            }
            for &(r, _, _) in lay.rows.iter().filter(|(r, _, _)| full.r1 <= *r && *r <= full.r2) {
                for &(c, _, _) in lay.cols.iter().filter(|(c, _, _)| full.c1 <= *c && *c <= full.c2) {
                    let Some(cell) = self.cell_rect(lay, r, c) else { continue };
                    if let Some(l) = pivot::style::look(pt, r, c) {
                        let line = |y: f32, (w, col): (BorderLine, u32)| {
                            let width = match w {
                                BorderLine::Medium => 2.0,
                                BorderLine::Thick => 3.0,
                                _ => 1.0,
                            };
                            let y = y.round() - 0.5;
                            cp.line_segment([Pos2::new(cell.left(), y), Pos2::new(cell.right(), y)], Stroke::new(width, rgb(col)));
                            if w == BorderLine::Double {
                                cp.line_segment([Pos2::new(cell.left(), y + 2.0), Pos2::new(cell.right(), y + 2.0)], Stroke::new(1.0, rgb(col)));
                            }
                        };
                        if let Some(b) = l.top {
                            line(cell.top() + 0.5, b);
                        }
                        if let Some(b) = l.bottom {
                            line(cell.bottom(), b);
                        }
                    }
                    if let Some((rows, li)) = button_line(pt, r, c) {
                        let b = self.button_rect(pt, cell, r, c);
                        let l = if rows { &pt.out.rows[li] } else { &pt.out.cols[li] };
                        cp.rect_filled(b, 0.0, Color32::WHITE);
                        cp.rect_stroke(b, 0.0, Stroke::new(1.0, Color32::from_gray(128)), StrokeKind::Inside);
                        let cc = b.center();
                        let k = (b.width() * 0.28).max(2.0);
                        let st = Stroke::new(1.0, Color32::from_gray(40));
                        cp.line_segment([Pos2::new(cc.x - k, cc.y), Pos2::new(cc.x + k + 0.5, cc.y)], st);
                        if l.collapsed {
                            cp.line_segment([Pos2::new(cc.x, cc.y - k), Pos2::new(cc.x, cc.y + k + 0.5)], st);
                        }
                    }
                    let role = pt.out.role(r, c);
                    if pt.field_headers || matches!(role, Role::PageValue(_)) {
                        if let Some((f, _, _)) = filter_target(pt, role) {
                            let b = Self::filter_rect(cell);
                            let hot = hover.is_some_and(|h| b.contains(h));
                            cp.rect_filled(b, 2.0, if hot { Color32::from_gray(232) } else { Color32::from_gray(250) });
                            cp.rect_stroke(b, 2.0, Stroke::new(1.0, Color32::from_gray(170)), StrokeKind::Inside);
                            let filtered = field_filtered(pt, f);
                            let (g, is_icon) = self.fonts.icons.get(if filtered { theme::ic::FILTER } else { theme::ic::DOWN }, "▾");
                            let font = if is_icon { theme::icon_font(9.0) } else { theme::ui_font(11.0) };
                            cp.text(b.center(), Align2::CENTER_CENTER, g, font, Color32::from_gray(50));
                        }
                    }
                }
            }
        }
        self.item_popup_ui(ui.ctx());
    }

    /// Clicks on report buttons; true when handled.
    pub(super) fn pivot_pointer(&mut self, lay: &Layout, pos: Pos2, pressed: bool) -> bool {
        if !pressed || !lay.cells.contains(pos) {
            return false;
        }
        let (Some(r), Some(c)) = (lay.row_at(pos.y), lay.col_at(pos.x)) else { return false };
        let si = self.wb.active;
        let Some(pi) = pivot::at(&self.wb, si, r, c) else { return false };
        let pt = &self.wb.sheets[si].pivots[pi];
        let Some(cell) = self.cell_rect(lay, r, c) else { return false };
        if let Some((rows, li)) = button_line(pt, r, c) {
            if self.button_rect(pt, cell, r, c).expand(2.0).contains(pos) {
                if self.edit.is_some() && !self.commit_edit(false) {
                    return true;
                }
                self.select(r, c, false);
                self.toggle_item(si, pi, rows, li);
                return true;
            }
        }
        let role = pt.out.role(r, c);
        if pt.field_headers || matches!(role, Role::PageValue(_)) {
            if let Some((f, choices, page)) = filter_target(pt, role) {
                if Self::filter_rect(cell).contains(pos) {
                    if self.edit.is_some() && !self.commit_edit(false) {
                        return true;
                    }
                    self.select(r, c, false);
                    self.open_item_popup(si, pi, f, choices, page, Pos2::new(cell.left(), cell.bottom()));
                    return true;
                }
            }
        }
        false
    }

    // ------------------------------------------------------------------
    // Item filter dropdown
    // ------------------------------------------------------------------

    pub(super) fn open_item_popup(&mut self, si: usize, pi: usize, f: usize, choices: Vec<usize>, page: bool, pos: Pos2) {
        let pt = self.pane_pivot(si, pi);
        let fi = engine::field_items(&pt, f);
        let mut items: Vec<engine::Item> = fi.items;
        items.sort_by(|a, b| {
            let blank = (a.key == engine::Key::Blank).cmp(&(b.key == engine::Key::Blank));
            blank.then_with(|| a.key.partial_cmp(&b.key).unwrap_or(std::cmp::Ordering::Equal))
        });
        let fl = &pt.fields[f];
        let list = items.into_iter().map(|it| {
            let k = ikey(&it.label);
            let on = !fl.hidden.contains(&k) && fl.page.as_ref().is_none_or(|p| *p == k);
            (it.label, k, on)
        });
        self.pivots.popup = Some(ItemPopup {
            si,
            pi,
            f,
            choices,
            pos,
            search: String::new(),
            items: list.collect(),
            page,
            multi: page && !fl.hidden.is_empty(),
            sel: fl.page.clone(),
            fresh: true,
        });
    }

    fn item_popup_ui(&mut self, ctx: &egui::Context) {
        let Some(mut p) = self.pivots.popup.take() else { return };
        if p.si >= self.wb.sheets.len() || p.pi >= self.wb.sheets[p.si].pivots.len() {
            return;
        }
        let pt = self.pane_pivot(p.si, p.pi);
        let mut close = false;
        let mut apply: Option<Box<dyn FnOnce(&mut PivotTable)>> = None;
        let mut switch: Option<usize> = None;
        let resp = egui::Area::new(egui::Id::new("pivot_item_popup")).order(egui::Order::Foreground).fixed_pos(p.pos).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(250.0);
                let f = p.f;
                if !p.page {
                    if p.choices.len() > 1 {
                        ui.horizontal(|ui| {
                            ui.label("Select field:");
                            egui::ComboBox::from_id_salt("pivot_popup_field").selected_text(pt.fields[f].display()).width(140.0).show_ui(ui, |ui| {
                                for &c in &p.choices {
                                    if ui.selectable_label(c == f, pt.fields[c].display()).clicked() {
                                        switch = Some(c);
                                    }
                                }
                            });
                        });
                        ui.separator();
                    }
                    let fl = &pt.fields[f];
                    let flat = |ui: &mut egui::Ui, text: &str, on: bool| ui.add(egui::Button::selectable(on, text).min_size(Vec2::new(240.0, 22.0))).clicked();
                    if flat(ui, "↑ Sort A to Z", fl.sort == pivot::SortOrder::Asc && fl.sort_by.is_none()) {
                        apply = Some(Box::new(move |pt: &mut PivotTable| {
                            pt.fields[f].sort = pivot::SortOrder::Asc;
                            pt.fields[f].sort_by = None;
                        }));
                    }
                    if flat(ui, "↓ Sort Z to A", fl.sort == pivot::SortOrder::Desc && fl.sort_by.is_none()) {
                        apply = Some(Box::new(move |pt: &mut PivotTable| {
                            pt.fields[f].sort = pivot::SortOrder::Desc;
                            pt.fields[f].sort_by = None;
                        }));
                    }
                    for (d, df) in pt.values.iter().enumerate() {
                        for (desc, label) in [(false, "Smallest to Largest"), (true, "Largest to Smallest")] {
                            let on = fl.sort_by == Some(d) && (fl.sort == pivot::SortOrder::Desc) == desc;
                            if flat(ui, &format!("  Sort by {}: {label}", df.name), on) {
                                apply = Some(Box::new(move |pt: &mut PivotTable| {
                                    pt.fields[f].sort = if desc { pivot::SortOrder::Desc } else { pivot::SortOrder::Asc };
                                    pt.fields[f].sort_by = Some(d);
                                }));
                            }
                        }
                    }
                    if flat(ui, "  Data Source Order", fl.sort == pivot::SortOrder::Source && fl.sort_by.is_none()) {
                        apply = Some(Box::new(move |pt: &mut PivotTable| {
                            pt.fields[f].sort = pivot::SortOrder::Source;
                            pt.fields[f].sort_by = None;
                        }));
                    }
                    let filtered = field_filtered(&pt, f);
                    if ui.add_enabled(filtered, egui::Button::new(format!("Clear Filter From \"{}\"", fl.display())).frame(false).min_size(Vec2::new(240.0, 22.0))).clicked() {
                        apply = Some(Box::new(move |pt: &mut PivotTable| {
                            pt.fields[f].hidden.clear();
                            pt.fields[f].page = None;
                        }));
                    }
                    ui.separator();
                }
                let sr = ui.add(egui::TextEdit::singleline(&mut p.search).hint_text("Search").desired_width(240.0));
                if p.fresh {
                    sr.request_focus();
                    p.fresh = false;
                }
                let q = p.search.to_lowercase();
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    if p.page && !p.multi {
                        if ui.selectable_label(p.sel.is_none(), "(All)").clicked() {
                            p.sel = None;
                        }
                        for (label, k, _) in p.items.iter().filter(|(l, _, _)| q.is_empty() || l.to_lowercase().contains(&q)) {
                            if ui.selectable_label(p.sel.as_deref() == Some(k.as_str()), label).clicked() {
                                p.sel = Some(k.clone());
                            }
                        }
                    } else {
                        let shown: Vec<usize> = (0..p.items.len()).filter(|i| q.is_empty() || p.items[*i].0.to_lowercase().contains(&q)).collect();
                        let all = shown.iter().all(|i| p.items[*i].2);
                        let mut a = all;
                        if ui.checkbox(&mut a, if q.is_empty() { "(Select All)" } else { "(Select All Search Results)" }).changed() {
                            for i in &shown {
                                p.items[*i].2 = a;
                            }
                        }
                        for i in shown {
                            let (label, _, on) = &mut p.items[i];
                            ui.checkbox(on, label.as_str());
                        }
                    }
                });
                if p.page {
                    ui.separator();
                    if ui.checkbox(&mut p.multi, "Select Multiple Items").changed() && p.multi {
                        let sel = p.sel.clone();
                        for it in p.items.iter_mut() {
                            it.2 = sel.as_ref().is_none_or(|s| *s == it.1);
                        }
                    }
                }
                ui.add_space(4.0);
                let any = p.page && !p.multi || p.items.iter().any(|i| i.2);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("Cancel").min_size(Vec2::new(70.0, 24.0))).clicked() {
                        close = true;
                    }
                    if ui.add_enabled(any, egui::Button::new("OK").min_size(Vec2::new(70.0, 24.0)).fill(theme::accent_light())).clicked() {
                        let hidden: std::collections::BTreeSet<String> = p.items.iter().filter(|i| !i.2).map(|i| i.1.clone()).collect();
                        let (page, multi, sel) = (p.page, p.multi, p.sel.clone());
                        apply = Some(Box::new(move |pt: &mut PivotTable| {
                            if page && !multi {
                                pt.fields[f].page = sel;
                                pt.fields[f].hidden.clear();
                            } else {
                                pt.fields[f].page = None;
                                pt.fields[f].hidden = hidden;
                            }
                        }));
                    }
                });
            });
        });
        if let Some(c) = switch {
            let (si, pi, choices, pos) = (p.si, p.pi, p.choices.clone(), p.pos);
            self.open_item_popup(si, pi, c, choices, false, pos);
            return;
        }
        if let Some(a) = apply {
            let (si, pi) = (p.si, p.pi);
            self.pane_edit(si, pi, a);
            return;
        }
        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) || resp.response.clicked_elsewhere() {
            return;
        }
        self.pivots.popup = Some(p);
    }

    // ------------------------------------------------------------------
    // Dialogs
    // ------------------------------------------------------------------

    /// The selection as an absolute reference with its sheet ("Sheet1!$A$1:$E$7").
    fn sel_ref(&self) -> String {
        let rg = self.sel_clipped();
        pivot::source_text(&self.wb, self.wb.active, rg)
    }

    pub(super) fn pivot_dialogs(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.pivots.dialog.take() else { return };
        let mut keep = true;
        match &mut d {
            PDialog::Create { source, new_sheet, location, error } => {
                window("Create PivotTable").show(ctx, |ui| {
                    ui.set_min_width(380.0);
                    ui.label(RichText::new("Select a table or range").strong());
                    ui.horizontal(|ui| {
                        ui.label("Table/Range:");
                        ui.add(egui::TextEdit::singleline(source).desired_width(220.0));
                        if ui.button("↥").on_hover_text("Use the selected cells").clicked() {
                            *source = self.sel_ref();
                        }
                    });
                    ui.add_space(6.0);
                    ui.label(RichText::new("Choose where you want the PivotTable to be placed").strong());
                    ui.radio_value(new_sheet, true, "New Worksheet");
                    ui.radio_value(new_sheet, false, "Existing Worksheet");
                    ui.add_enabled_ui(!*new_sheet, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Location:");
                            ui.add(egui::TextEdit::singleline(location).desired_width(220.0));
                            if ui.button("↥").on_hover_text("Use the active cell").clicked() {
                                let (r, c) = self.active();
                                *location = format!("{}!{}", crate::formula::quote_sheet(&self.sheet().name), abs_cell(r, c));
                            }
                        });
                    });
                    if let Some(e) = error {
                        ui.colored_label(theme::error_text(), e.as_str());
                    }
                    let (ok, cancel) = ok_cancel(ui);
                    if cancel {
                        keep = false;
                    } else if ok {
                        if !*new_sheet && location.trim().is_empty() {
                            *error = Some("Destination reference is not valid.".into());
                            return;
                        }
                        let (s, n, l) = (source.clone(), *new_sheet, location.clone());
                        match self.create_pivot(&s, n, &l) {
                            Ok(()) => keep = false,
                            Err(e) => *error = Some(e),
                        }
                    }
                });
            }
            PDialog::ValueField(v) => {
                let pt = self.pane_pivot(v.si, v.pi);
                window("Value Field Settings").show(ctx, |ui| {
                    ui.set_min_width(360.0);
                    let src = pt.values.get(v.d).and_then(|df| pt.fields.get(df.field)).map(|f| f.name.clone()).unwrap_or_default();
                    ui.label(format!("Source Name: {src}"));
                    ui.horizontal(|ui| {
                        ui.label("Custom Name:");
                        ui.add(egui::TextEdit::singleline(&mut v.name).desired_width(240.0));
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut v.tab, 0, "Summarize Values By");
                        ui.selectable_value(&mut v.tab, 1, "Show Values As");
                    });
                    ui.separator();
                    if v.tab == 0 {
                        ui.label("Summarize value field by");
                        ui.label(RichText::new("Choose the type of calculation that you want to use to summarize data from the selected field").size(11.5).color(theme::text_dim()));
                        egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                            for a in Agg::ALL {
                                if ui.selectable_label(v.agg == a, a.label()).clicked() && v.agg != a {
                                    // A default name follows the function, like Excel.
                                    let old = format!("{} of {}", v.agg.label(), src);
                                    if v.name.starts_with(&old) {
                                        v.name = format!("{} of {}{}", a.label(), src, &v.name[old.len()..]);
                                    }
                                    v.agg = a;
                                }
                            }
                        });
                    } else {
                        ui.label("Show values as");
                        egui::ComboBox::from_id_salt("pivot_show_as").selected_text(v.show_as.label()).width(240.0).show_ui(ui, |ui| {
                            for s in ShowAs::ALL {
                                ui.selectable_value(&mut v.show_as, s, s.label());
                            }
                        });
                        ui.add_enabled_ui(v.show_as.needs_field(), |ui| {
                            ui.columns(2, |cols| {
                                cols[0].label("Base field:");
                                egui::ScrollArea::vertical().id_salt("pv_bf").max_height(120.0).show(&mut cols[0], |ui| {
                                    for (i, f) in pt.fields.iter().enumerate() {
                                        if ui.selectable_label(v.base_field == i, f.display()).clicked() {
                                            v.base_field = i;
                                        }
                                    }
                                });
                                cols[1].label("Base item:");
                                cols[1].add_enabled_ui(v.show_as.needs_item(), |ui| {
                                    egui::ScrollArea::vertical().id_salt("pv_bi").max_height(120.0).show(ui, |ui| {
                                        if ui.selectable_label(v.base_item == BaseItem::Prev, "(previous)").clicked() {
                                            v.base_item = BaseItem::Prev;
                                        }
                                        if ui.selectable_label(v.base_item == BaseItem::Next, "(next)").clicked() {
                                            v.base_item = BaseItem::Next;
                                        }
                                        if v.base_field < pt.fields.len() {
                                            for it in engine::field_items(&pt, v.base_field).items {
                                                let on = matches!(&v.base_item, BaseItem::Item(s) if ikey(s) == ikey(&it.label));
                                                if ui.selectable_label(on, &it.label).clicked() {
                                                    v.base_item = BaseItem::Item(it.label.clone());
                                                }
                                            }
                                        }
                                    });
                                });
                            });
                        });
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("Number Format:");
                        let cur = super::ribbon::NUMBER_FORMATS
                            .iter()
                            .find(|(_, f)| (v.num_fmt.is_empty() && *f == "General") || v.num_fmt == *f)
                            .map(|(n, _)| n.to_string())
                            .unwrap_or_else(|| "Custom".into());
                        egui::ComboBox::from_id_salt("pivot_numfmt").selected_text(cur).width(120.0).show_ui(ui, |ui| {
                            for (n, f) in super::ribbon::NUMBER_FORMATS.iter().chain([("Number (1,000)", "#,##0")].iter()) {
                                if ui.selectable_label(false, *n).clicked() {
                                    v.num_fmt = if *f == "General" { String::new() } else { f.to_string() };
                                }
                            }
                        });
                        ui.add(egui::TextEdit::singleline(&mut v.num_fmt).hint_text("General").desired_width(110.0));
                    });
                    let (ok, cancel) = ok_cancel(ui);
                    if cancel {
                        keep = false;
                    } else if ok {
                        let (d, name) = (v.d, v.name.trim().to_string());
                        if name.is_empty() || pt.values.iter().enumerate().any(|(i, x)| i != d && x.name.eq_ignore_ascii_case(&name)) || pt.fields.iter().any(|f| f.name.eq_ignore_ascii_case(&name)) {
                            self.dialog = Some(Dialog::Message { title: "FreedomOffice Sheets".into(), text: "A PivotTable field name must be unique and not empty.".into() });
                            return;
                        }
                        // Percentages get a percent format, like Excel.
                        let was = pt.values.get(d).map(|x| x.show_as.is_percent()).unwrap_or(false);
                        if v.show_as.is_percent() && !was && v.num_fmt.is_empty() {
                            v.num_fmt = "0.00%".into();
                        } else if !v.show_as.is_percent() && was && v.num_fmt == "0.00%" {
                            v.num_fmt.clear();
                        }
                        let (agg, show_as, bf, bi, fmt) = (v.agg, v.show_as, v.base_field, v.base_item.clone(), v.num_fmt.clone());
                        let (si, pi) = (v.si, v.pi);
                        self.pane_edit(si, pi, move |pt| {
                            if let Some(df) = pt.values.get_mut(d) {
                                df.name = name;
                                df.agg = agg;
                                df.show_as = show_as;
                                df.base_field = bf;
                                df.base_item = bi;
                                df.num_fmt = fmt;
                            }
                        });
                        keep = false;
                    }
                });
            }
            PDialog::FieldSettings { si, pi, f, caption, subtotal, sort } => {
                let pt = self.pane_pivot(*si, *pi);
                window("Field Settings").show(ctx, |ui| {
                    ui.set_min_width(320.0);
                    ui.label(format!("Source Name: {}", pt.fields[*f].name));
                    ui.horizontal(|ui| {
                        ui.label("Custom Name:");
                        ui.add(egui::TextEdit::singleline(caption).desired_width(200.0));
                    });
                    ui.add_space(4.0);
                    ui.label(RichText::new("Subtotals").strong());
                    ui.radio_value(subtotal, true, "Automatic");
                    ui.radio_value(subtotal, false, "None");
                    ui.add_space(4.0);
                    ui.label(RichText::new("Sort").strong());
                    ui.radio_value(sort, pivot::SortOrder::Asc, "Ascending (A to Z)");
                    ui.radio_value(sort, pivot::SortOrder::Desc, "Descending (Z to A)");
                    ui.radio_value(sort, pivot::SortOrder::Source, "Data source order");
                    let (ok, cancel) = ok_cancel(ui);
                    if cancel {
                        keep = false;
                    } else if ok {
                        let (f, cap, sub, so) = (*f, caption.trim().to_string(), *subtotal, *sort);
                        let base = pt.fields[f].name.clone();
                        self.pane_edit(*si, *pi, move |pt| {
                            let fl = &mut pt.fields[f];
                            fl.caption = if cap == base { String::new() } else { cap };
                            fl.subtotal = sub;
                            if fl.sort != so {
                                fl.sort = so;
                                fl.sort_by = None;
                            }
                        });
                        keep = false;
                    }
                });
            }
            PDialog::Options { si, pi, name, empty, autofit, row_grand, col_grand, buttons, headers } => {
                window("PivotTable Options").show(ctx, |ui| {
                    ui.set_min_width(340.0);
                    ui.horizontal(|ui| {
                        ui.label("PivotTable Name:");
                        ui.add(egui::TextEdit::singleline(name).desired_width(180.0));
                    });
                    ui.add_space(4.0);
                    ui.label(RichText::new("Layout & Format").strong());
                    ui.horizontal(|ui| {
                        ui.label("For empty cells show:");
                        ui.add(egui::TextEdit::singleline(empty).desired_width(100.0));
                    });
                    ui.checkbox(autofit, "Autofit column widths on update");
                    ui.label(RichText::new("Totals & Filters").strong());
                    ui.checkbox(row_grand, "Show grand totals for rows");
                    ui.checkbox(col_grand, "Show grand totals for columns");
                    ui.label(RichText::new("Display").strong());
                    ui.checkbox(buttons, "Show expand/collapse buttons");
                    ui.checkbox(headers, "Display field captions and filter drop downs");
                    let (ok, cancel) = ok_cancel(ui);
                    if cancel {
                        keep = false;
                    } else if ok {
                        let n = name.trim().to_string();
                        let taken = self.wb.sheets.iter().enumerate().any(|(s, sh)| sh.pivots.iter().enumerate().any(|(i, p)| (s, i) != (*si, *pi) && p.name.eq_ignore_ascii_case(&n)));
                        if n.is_empty() || taken {
                            self.dialog = Some(Dialog::Message { title: "FreedomOffice Sheets".into(), text: "A PivotTable report with that name already exists, or the name isn't valid.".into() });
                            return;
                        }
                        let (e, a, rg, cg, b, h) = (empty.clone(), *autofit, *row_grand, *col_grand, *buttons, *headers);
                        self.pivot_edit(*si, *pi, move |pt| {
                            pt.name = n;
                            pt.empty_text = e;
                            pt.autofit = a;
                            pt.row_grand = rg;
                            pt.col_grand = cg;
                            pt.show_buttons = b;
                            pt.field_headers = h;
                        });
                        keep = false;
                    }
                });
            }
            PDialog::Group { si, pi, f, dates, start, end, by, parts } => {
                window("Grouping").show(ctx, |ui| {
                    ui.set_min_width(280.0);
                    ui.label(RichText::new("Auto").strong());
                    ui.horizontal(|ui| {
                        ui.label("Starting at:");
                        ui.add_enabled(!*dates, egui::TextEdit::singleline(start).desired_width(140.0));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Ending at:  ");
                        ui.add_enabled(!*dates, egui::TextEdit::singleline(end).desired_width(140.0));
                    });
                    ui.label(RichText::new("By").strong());
                    if *dates {
                        for (k, p) in [DatePart::Days, DatePart::Months, DatePart::Quarters, DatePart::Years].iter().enumerate() {
                            ui.toggle_value(&mut parts[k], p.label());
                        }
                    } else {
                        ui.add(egui::TextEdit::singleline(by).desired_width(140.0));
                    }
                    let (ok, cancel) = ok_cancel(ui);
                    if cancel {
                        keep = false;
                    } else if ok {
                        let f = *f;
                        if *dates {
                            let sel: Vec<DatePart> = [DatePart::Days, DatePart::Months, DatePart::Quarters, DatePart::Years].into_iter().zip(parts.iter()).filter(|(_, on)| **on).map(|(p, _)| p).collect();
                            if sel.is_empty() {
                                return;
                            }
                            self.pivot_edit(*si, *pi, move |pt| pt.group_dates(f, &sel));
                        } else {
                            let n = |s: &str| crate::numfmt::parse_number(s.trim());
                            match (n(start), n(end), n(by)) {
                                (Some(a), Some(b), Some(k)) if b > a && k > 0.0 => {
                                    self.pivot_edit(*si, *pi, move |pt| pt.fields[f].num_group = Some((a, b, k)));
                                }
                                _ => {
                                    self.dialog = Some(Dialog::Message { title: "FreedomOffice Sheets".into(), text: "The ending value must be greater than the starting value, and By must be positive.".into() });
                                    return;
                                }
                            }
                        }
                        keep = false;
                    }
                });
            }
            PDialog::Move { si, pi, new_sheet, location, error } => {
                window("Move PivotTable").show(ctx, |ui| {
                    ui.set_min_width(340.0);
                    ui.label("Choose where you want the PivotTable to be placed");
                    ui.radio_value(new_sheet, true, "New Worksheet");
                    ui.radio_value(new_sheet, false, "Existing Worksheet");
                    ui.add_enabled_ui(!*new_sheet, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Location:");
                            ui.add(egui::TextEdit::singleline(location).desired_width(200.0));
                        });
                    });
                    if let Some(e) = error {
                        ui.colored_label(theme::error_text(), e.as_str());
                    }
                    let (ok, cancel) = ok_cancel(ui);
                    if cancel {
                        keep = false;
                    } else if ok {
                        match self.move_pivot(*si, *pi, *new_sheet, &location.clone()) {
                            Ok(()) => keep = false,
                            Err(e) => *error = Some(e),
                        }
                    }
                });
            }
            PDialog::Source { si, pi, source, error } => {
                window("Change PivotTable Data Source").show(ctx, |ui| {
                    ui.set_min_width(340.0);
                    ui.label("Choose the data that you want to analyze");
                    ui.horizontal(|ui| {
                        ui.label("Table/Range:");
                        ui.add(egui::TextEdit::singleline(source).desired_width(220.0));
                        if ui.button("↥").on_hover_text("Use the selected cells").clicked() {
                            *source = self.sel_ref();
                        }
                    });
                    if let Some(e) = error {
                        ui.colored_label(theme::error_text(), e.as_str());
                    }
                    let (ok, cancel) = ok_cancel(ui);
                    if cancel {
                        keep = false;
                    } else if ok {
                        let (si, pi) = (*si, *pi);
                        let src = source.trim().trim_start_matches('=').to_string();
                        match pivot::build_cache(&self.wb, &src, si) {
                            Err(e) => *error = Some(e),
                            Ok(_) => {
                                let src = match crate::table::find(&self.wb, &src) {
                                    Some(_) => src,
                                    None => match pivot::resolve_source(&self.wb, &src, si) {
                                        Some((s, rg)) => pivot::source_text(&self.wb, s, rg),
                                        None => src,
                                    },
                                };
                                self.push_undo(&[Scope::Sheet(si)]);
                                self.wb.sheets[si].pivots[pi].source = src;
                                self.finish_pivot_update(si, pi, Some(()));
                                keep = false;
                            }
                        }
                    }
                });
            }
        }
        if keep && self.pivots.dialog.is_none() {
            self.pivots.dialog = Some(d);
        }
    }

    pub(super) fn open_move_pivot(&mut self) {
        let Some((si, pi)) = self.active_pivot() else { return };
        let r = self.wb.sheets[si].pivots[pi].out.full;
        let location = format!("{}!{}", crate::formula::quote_sheet(&self.wb.sheets[si].name), abs_cell(r.r1, r.c1));
        self.pivots.dialog = Some(PDialog::Move { si, pi, new_sheet: false, location, error: None });
    }

    pub(super) fn open_change_source(&mut self) {
        let Some((si, pi)) = self.active_pivot() else { return };
        let source = self.wb.sheets[si].pivots[pi].source.clone();
        self.pivots.dialog = Some(PDialog::Source { si, pi, source, error: None });
    }

    fn move_pivot(&mut self, si: usize, pi: usize, new_sheet: bool, location: &str) -> Result<(), String> {
        let (ts, cell) = if new_sheet {
            (None, (2u32, 0u32))
        } else {
            let (s, rg) = pivot::resolve_source(&self.wb, location, si).ok_or("Destination reference is not valid.")?;
            (Some(s), (rg.r1, rg.c1))
        };
        self.push_undo(&[Scope::All]);
        let mut pt = self.wb.sheets[si].pivots[pi].clone();
        engine::delete(&mut self.wb, si, pi);
        let npages = pt.filters.len() as u32;
        let ts = match ts {
            Some(s) => s,
            None => {
                let name = self.wb.unique_sheet_name("Sheet");
                self.wb.sheets.insert(si, Sheet::new(&name));
                si
            }
        };
        // The location is the report's top-left cell, report filters included.
        pt.anchor = (cell.0 + if npages > 0 { npages + 1 } else { 0 }, cell.1);
        pt.out = Default::default();
        self.wb.sheets[ts].pivots.push(pt);
        let ni = self.wb.sheets[ts].pivots.len() - 1;
        if let Err(e) = engine::update(&mut self.wb, ts, ni) {
            self.undo();
            self.redo.clear();
            return Err(e);
        }
        self.pivot_autofit(ts, ni);
        self.wb.active = ts;
        let a = self.wb.sheets[ts].pivots[ni].out.range;
        self.select(a.r1, a.c1, false);
        self.changed();
        Ok(())
    }

    /// Sort A to Z / Z to A inside a report: a label sorts its field, a value sorts the
    /// innermost row field by that value. Returns true when the active cell is in a report.
    pub fn pivot_sort(&mut self, desc: bool) -> bool {
        let Some((si, pi)) = self.active_pivot() else { return false };
        let order = if desc { pivot::SortOrder::Desc } else { pivot::SortOrder::Asc };
        if let Some(d) = self.active_data().filter(|_| matches!(self.wb.sheets[si].pivots[pi].out.role(self.active().0, self.active().1), Role::Value(..))) {
            let pt = &self.wb.sheets[si].pivots[pi];
            if let Some(&f) = pt.rows.iter().rev().find(|f| **f != VALUES) {
                self.pivot_edit(si, pi, |p| {
                    p.fields[f].sort = order;
                    p.fields[f].sort_by = Some(d);
                });
            }
        } else if let Some(f) = self.active_field() {
            self.pivot_edit(si, pi, |p| {
                p.fields[f].sort = order;
                p.fields[f].sort_by = None;
            });
        }
        true
    }

    /// Select > Entire PivotTable / Values / Labels.
    pub(super) fn select_pivot_part(&mut self, part: u8) {
        let Some((si, pi)) = self.active_pivot() else { return };
        let o = self.wb.sheets[si].pivots[pi].out.clone();
        let rg = match part {
            0 => o.full,
            1 if o.label_cols < o.range.cols() && o.header_rows < o.range.rows() => Range::new(o.range.r1 + o.header_rows, o.range.c1 + o.label_cols, o.range.r2, o.range.c2),
            2 if o.label_cols > 0 => Range::new(o.range.r1, o.range.c1, o.range.r2, o.range.c1 + o.label_cols - 1),
            _ => o.range,
        };
        self.select_range(rg);
    }

    /// Clear All / Clear Filters.
    pub(super) fn clear_pivot(&mut self, filters_only: bool) {
        let Some((si, pi)) = self.active_pivot() else { return };
        self.pivots.pending = None;
        self.pivot_edit(si, pi, |pt| if filters_only { pt.clear_filters() } else { pt.clear() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ctx: &egui::Context, app: &mut App) {
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 1000.0))), ..Default::default() };
        let _ = ctx.run(input, |ctx| {
            egui::TopBottomPanel::top("ribbon").show(ctx, |ui| app.ribbon_ui(ui));
            app.pivot_field_pane(ctx);
            egui::CentralPanel::default().show(ctx, |ui| app.grid_ui(ui));
            app.pivot_dialogs(ctx);
            app.dialogs_ui(ctx);
        });
    }

    /// Creates a report through the commands the UI uses, draws every tab, pane, popup and
    /// dialog, and checks Show Details, GETPIVOTDATA pointing, protection, refresh and undo.
    #[test]
    fn pivot_ui_smoke() {
        let ctx = egui::Context::default();
        let mut app = App::with_context(&ctx, 0, None);
        app.wb = crate::pivot::tests::sample(true);
        app.select(1, 1, false);
        app.open_create_pivot();
        let Some(PDialog::Create { source, .. }) = &app.pivots.dialog else { panic!("no dialog") };
        assert_eq!(source, "Sheet1!$A$1:$E$8");
        let source = source.clone();
        app.pivots.dialog = None;
        app.create_pivot(&source, true, "").unwrap();
        assert_eq!(app.wb.sheets.len(), 2);
        assert_eq!(app.wb.active, 0);
        assert_eq!(app.tab, RibbonTab::PivotAnalyze);
        let (si, pi) = app.active_pivot().unwrap();
        assert!(app.sheet().pivots[pi].out.placeholder);
        frame(&ctx, &mut app);
        app.pane_edit(si, pi, |p| p.add_field(0, Area::Rows, None));
        app.pane_edit(si, pi, |p| p.add_field(1, Area::Columns, None));
        app.pane_edit(si, pi, |p| p.add_field(3, Area::Values, None));
        app.pane_edit(si, pi, |p| p.add_field(4, Area::Filters, None));
        let pt = app.sheet().pivots[pi].clone();
        assert_eq!(pt.out.range.r1, 2);
        assert_eq!(app.sheet().value(2, 0), &Value::Text("Sum of Sales".into()));
        // Defer Layout Update keeps the report until Update.
        app.pivots.defer = true;
        app.pane_edit(si, pi, |p| p.add_field(2, Area::Rows, None));
        assert_eq!(app.sheet().pivots[pi].rows.len(), 1);
        app.apply_pending();
        assert_eq!(app.sheet().pivots[pi].rows.len(), 2);
        app.pivots.defer = false;
        app.undo();
        assert_eq!(app.sheet().pivots[pi].rows.len(), 1);
        for (k, tab) in [RibbonTab::PivotAnalyze, RibbonTab::PivotDesign, RibbonTab::PivotAnalyze, RibbonTab::PivotDesign, RibbonTab::Insert, RibbonTab::Home].into_iter().enumerate() {
            let rg = app.sheet().pivots[pi].out.range;
            app.select(rg.r1 + 2, rg.c1 + 1, false);
            app.tab = tab;
            match k {
                0 => app.open_field_settings(),
                1 => app.open_pivot_options(),
                2 => app.open_move_pivot(),
                3 => app.open_change_source(),
                4 => {
                    app.select(rg.r1 + 2, rg.c1, false);
                    app.open_field_settings();
                }
                _ => {
                    let pos = Pos2::new(100.0, 200.0);
                    app.open_item_popup(si, pi, 0, vec![0], false, pos);
                }
            }
            frame(&ctx, &mut app);
            frame(&ctx, &mut app);
            app.pivots.dialog = None;
        }
        app.select(rg_value(&app, pi).0, rg_value(&app, pi).1, false);
        app.open_group_dialog();
        frame(&ctx, &mut app);
        app.pivots.dialog = None;
        // Report cells can't be typed over.
        let (vr, vc) = rg_value(&app, pi);
        assert!(app.locked_block(Range::cell(vr, vc)));
        app.dialog = None;
        // GETPIVOTDATA when a value is clicked while typing a formula.
        app.select(0, 8, false);
        app.begin_edit(Some("=".into()), super::super::EditMode::Enter);
        let t = app.pivot_point_text(vr, vc).unwrap();
        assert!(t.starts_with("GETPIVOTDATA(\"Sales\",$A$3,\"Region\",\"East\""), "{t}");
        app.cancel_edit();
        // Double-clicking a value shows its records on a new sheet.
        let sheets = app.wb.sheets.len();
        assert!(app.pivot_double_click(vr, vc));
        assert_eq!(app.wb.sheets.len(), sheets + 1);
        assert_eq!(app.sheet().tables.len(), 1);
        assert_eq!(app.sheet().value(0, 0), &Value::Text("Region".into()));
        app.undo();
        assert_eq!(app.wb.sheets.len(), sheets);
        // Collapse an item with its button, then refresh after a source change.
        let li = app.sheet().pivots[pi].out.row_at[0];
        app.toggle_item(si, pi, true, li);
        let src = app.wb.sheet_index("Sheet1").unwrap();
        app.wb.sheets[src].set(1, 3, Some(Cell { value: Value::Number(1000.0), ..Default::default() }));
        app.select(vr, vc, false);
        app.pivot_refresh_active();
        let total = app.sheet().pivots[pi].out.range;
        assert_eq!(app.sheet().value(total.r2, total.c2), &Value::Number(1610.0));
        app.pivot_refresh_all();
        app.clear_pivot(false);
        assert!(app.sheet().pivots[pi].out.placeholder);
        frame(&ctx, &mut app);
    }

    /// First value cell of a report.
    fn rg_value(app: &App, pi: usize) -> (u32, u32) {
        let o = &app.sheet().pivots[pi].out;
        (o.range.r1 + o.header_rows, o.range.c1 + o.label_cols)
    }
}
