//! Paste Special: the dialog, the floating Paste Options (Ctrl) button, Insert Copied Cells.

use std::collections::HashMap;

use eframe::egui::{self, Align2, Pos2, Rect, RichText, Stroke, Vec2};

use super::actions::{shift_cell, PasteMode};
use super::dialogs::Dialog;
use super::fill::float_button;
use super::grid::Layout;
use super::theme;
use super::{App, Change, Clip, ClipCell, Scope};
use crate::model::{cell_from_input, Border, Cell, ErrorKind, Range, Style, Value, Workbook, MAX_COLS, MAX_ROWS};

/// What Paste Special pastes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PasteWhat {
    #[default]
    All,
    Formulas,
    Values,
    Formats,
    Comments,
    Validation,
    AllSourceTheme,
    AllExceptBorders,
    ColumnWidths,
    FormulasNumFmt,
    ValuesNumFmt,
    AllMergeCf,
    /// Paste Options > Keep Source Column Widths.
    AllWithWidths,
    /// Paste Options > Values & Source Formatting.
    ValuesSourceFormat,
}

impl PasteWhat {
    /// Pastes the cell contents and the source formatting.
    fn is_all(self) -> bool {
        matches!(self, PasteWhat::All | PasteWhat::AllSourceTheme | PasteWhat::AllExceptBorders | PasteWhat::AllMergeCf | PasteWhat::AllWithWidths)
    }
    fn carries_formats(self) -> bool {
        self.is_all() || matches!(self, PasteWhat::Formats | PasteWhat::ValuesSourceFormat)
    }
    fn touches_cells(self) -> bool {
        !matches!(self, PasteWhat::Comments | PasteWhat::Validation | PasteWhat::ColumnWidths)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PasteOp {
    #[default]
    None,
    Add,
    Subtract,
    Multiply,
    Divide,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PasteOpts {
    pub what: PasteWhat,
    pub op: PasteOp,
    pub skip_blanks: bool,
    pub transpose: bool,
    /// Paste Link: formulas pointing at the copied cells.
    pub link: bool,
}

impl PasteOpts {
    pub fn what(what: PasteWhat) -> Self {
        PasteOpts { what, ..Default::default() }
    }
}

impl From<PasteMode> for PasteOpts {
    fn from(m: PasteMode) -> Self {
        match m {
            PasteMode::All => PasteOpts::default(),
            PasteMode::Values => PasteOpts::what(PasteWhat::Values),
            PasteMode::Formulas => PasteOpts::what(PasteWhat::Formulas),
            PasteMode::Formats => PasteOpts::what(PasteWhat::Formats),
            PasteMode::Transpose => PasteOpts { transpose: true, ..Default::default() },
        }
    }
}

/// State of the Paste Special dialog.
#[derive(Default)]
pub struct PasteSpecialDlg {
    pub what: PasteWhat,
    pub op: PasteOp,
    pub skip_blanks: bool,
    pub transpose: bool,
}

/// The Paste Options button left at the bottom-right of a pasted range.
pub struct PasteButton {
    pub sheet: usize,
    pub target: Range,
    pub opts: PasteOpts,
    pub edits: u64,
    pub open: bool,
}

fn number_or_zero(c: &Cell) -> Option<f64> {
    match &c.value {
        Value::Number(n) => Some(*n),
        Value::Empty => Some(0.0),
        _ => None,
    }
}

/// Paste Special operation: combines a source cell with the destination cell's contents.
fn combine(wb: &mut Workbook, dest: Option<&Cell>, src: &Cell, op: PasteOp) -> Cell {
    let blank = Cell::default();
    let dest = dest.unwrap_or(&blank);
    let sym = match op {
        PasteOp::None => return src.clone(),
        PasteOp::Add => "+",
        PasteOp::Subtract => "-",
        PasteOp::Multiply => "*",
        PasteOp::Divide => "/",
    };
    // Text (and other non-numbers) are pasted as they are; text destinations stay.
    if src.formula.is_none() && number_or_zero(src).is_none() {
        return src.clone();
    }
    if dest.formula.is_none() && number_or_zero(dest).is_none() {
        return Cell { style: 0, ..dest.clone() };
    }
    if dest.formula.is_some() || src.formula.is_some() {
        let side = |c: &Cell| match &c.formula {
            Some(f) => format!("({})", f.text),
            None => crate::numfmt::edit_number(number_or_zero(c).unwrap_or(0.0)),
        };
        let text = format!("={}{}{}", side(dest), sym, side(src));
        return cell_from_input(wb, &text, 0).unwrap_or_default();
    }
    let (a, b) = (number_or_zero(dest).unwrap_or(0.0), number_or_zero(src).unwrap_or(0.0));
    let value = match op {
        PasteOp::Add => Value::Number(a + b),
        PasteOp::Subtract => Value::Number(a - b),
        PasteOp::Multiply => Value::Number(a * b),
        _ if b == 0.0 => Value::Error(ErrorKind::Div0),
        _ => Value::Number(a / b),
    };
    Cell { value, formula: None, style: 0 }
}

/// Copies conditional formats (and validations) that touch the copied range to the paste blocks.
fn copy_rules(wb: &mut Workbook, si: usize, clip: &Clip, blocks: &[(u32, u32)], cf: bool, dv: bool, merge_cf: bool) {
    if clip.sheet >= wb.sheets.len() {
        return;
    }
    let src = clip.range;
    let mut new_cf = Vec::new();
    let mut new_dv = Vec::new();
    for &(br, bc) in blocks {
        let dr = br as i64 - src.r1 as i64;
        let dc = bc as i64 - src.c1 as i64;
        let shift = |rg: Range| -> Option<Range> {
            let (r1, c1, r2, c2) = (rg.r1 as i64 + dr, rg.c1 as i64 + dc, rg.r2 as i64 + dr, rg.c2 as i64 + dc);
            if r1 < 0 || c1 < 0 || r2 >= MAX_ROWS as i64 || c2 >= MAX_COLS as i64 {
                return None;
            }
            Some(Range::new(r1 as u32, c1 as u32, r2 as u32, c2 as u32))
        };
        let inter = |rg: &Range| -> Option<Range> {
            rg.intersects(&src).then(|| Range::new(rg.r1.max(src.r1), rg.c1.max(src.c1), rg.r2.min(src.r2), rg.c2.min(src.c2)))
        };
        let sheet = &wb.sheets[clip.sheet];
        if cf {
            for rule in &sheet.cond_formats {
                let parts: Vec<Range> = rule.ranges.iter().filter_map(inter).collect();
                let Some(first) = parts.first().copied() else { continue };
                let ob = rule.base();
                let mut tmp = crate::model::Sheet::new("");
                let mut r = rule.clone();
                r.ranges = parts.iter().filter_map(|p| shift(*p)).collect();
                if r.ranges.is_empty() {
                    continue;
                }
                // Relative references follow the cells, like copied formulas.
                let (sr, sc) = (first.r1 as i64 - ob.0 as i64 + dr, first.c1 as i64 - ob.1 as i64 + dc);
                tmp.cond_formats.push(r);
                crate::condfmt::map_sheet_formulas(&mut tmp, &mut |e| crate::formula::shift_relative(e, sr, sc));
                new_cf.extend(tmp.cond_formats);
            }
        }
        if dv {
            for v in &sheet.validations {
                for p in v.ranges.iter().filter_map(inter) {
                    if let Some(t) = shift(p) {
                        new_dv.push((t, v.clone()));
                    }
                }
            }
        }
    }
    let sh = &mut wb.sheets[si];
    if cf {
        if !merge_cf {
            for &(br, bc) in blocks {
                let t = Range::new(br, bc, (br + src.rows() - 1).min(MAX_ROWS - 1), (bc + src.cols() - 1).min(MAX_COLS - 1));
                for rule in sh.cond_formats.iter_mut() {
                    rule.ranges = crate::condfmt::subtract_all(&rule.ranges, t);
                }
            }
            sh.cond_formats.retain(|r| !r.ranges.is_empty());
        }
        sh.cond_formats.extend(new_cf);
    }
    for (t, v) in new_dv {
        crate::validation::set_for_range(sh, t, Some(v));
    }
}


/// Pastes a copied (not cut) range at `origin`, tiled `tiles` times. Returns the target range.
pub fn paste_cells(wb: &mut Workbook, si: usize, clip: &Clip, origin: (u32, u32), tiles: (u32, u32), o: PasteOpts) -> Range {
    let (r0, c0) = origin;
    let (tr, tc) = tiles;
    let (sh_rows, sh_cols) = (clip.range.rows(), clip.range.cols());
    let (h, w) = if o.transpose { (sh_cols, sh_rows) } else { (sh_rows, sh_cols) };
    let target = Range::new(r0, c0, (r0 + h * tr - 1).min(MAX_ROWS - 1), (c0 + w * tc - 1).min(MAX_COLS - 1));
    let by_pos: HashMap<(u32, u32), &ClipCell> = clip.cells.iter().map(|c| ((c.dr, c.dc), c)).collect();
    let src_name = wb.sheets.get(clip.sheet).map(|s| s.name.clone()).unwrap_or_default();
    let mut blocks = Vec::new();
    for ti in 0..tr {
        for tj in 0..tc {
            let (br, bc) = (r0 + ti * h, c0 + tj * w);
            blocks.push((br, bc));
            for dr0 in 0..sh_rows {
                for dc0 in 0..sh_cols {
                    let (dr, dc) = if o.transpose { (dc0, dr0) } else { (dr0, dc0) };
                    let (r, c) = (br + dr, bc + dc);
                    if r >= MAX_ROWS || c >= MAX_COLS {
                        continue;
                    }
                    let cc = by_pos.get(&(dr0, dc0));
                    let blank = cc.is_none_or(|x| x.cell.value.is_empty() && x.cell.formula.is_none());
                    if o.skip_blanks && blank {
                        continue;
                    }
                    let existing = wb.sheets[si].get(r, c).cloned();
                    let ex_style = existing.as_ref().map_or(0, |e| e.style);
                    if o.link {
                        let (sr, sc) = (clip.range.r1 + dr0, clip.range.c1 + dc0);
                        let a1 = crate::model::cell_name(sr, sc);
                        let reference = if clip.range.is_single() {
                            format!("${}${}", crate::model::col_name(sc), sr + 1)
                        } else {
                            a1
                        };
                        let prefix = if clip.sheet != si { format!("{}!", crate::formula::quote_sheet(&src_name)) } else { String::new() };
                        let cell = cell_from_input(wb, &format!("={prefix}{reference}"), ex_style);
                        wb.sheets[si].set(r, c, cell);
                        continue;
                    }
                    if !o.what.touches_cells() {
                        continue;
                    }
                    let (mut cell, src_style) = cc.map(|x| (x.cell.clone(), x.style.clone())).unwrap_or_default();
                    shift_cell(&mut cell, r as i64 - (clip.range.r1 + dr0) as i64, c as i64 - (clip.range.c1 + dc0) as i64);
                    if o.op != PasteOp::None && o.what != PasteWhat::Formats {
                        cell = combine(wb, existing.as_ref(), &cell, o.op);
                    }
                    let with_num_fmt = |wb: &mut Workbook| {
                        let mut s = wb.style(ex_style).clone();
                        s.num_fmt = src_style.num_fmt.clone();
                        wb.intern_style(s)
                    };
                    let style = match o.what {
                        PasteWhat::AllExceptBorders => wb.intern_style(Style { border: Border::default(), ..src_style.clone() }),
                        PasteWhat::FormulasNumFmt | PasteWhat::ValuesNumFmt => with_num_fmt(wb),
                        w if w.carries_formats() => wb.intern_style(src_style.clone()),
                        _ => ex_style,
                    };
                    let (value, formula) = match o.what {
                        PasteWhat::Formats => existing.map(|e| (e.value, e.formula)).unwrap_or_default(),
                        PasteWhat::Values | PasteWhat::ValuesNumFmt | PasteWhat::ValuesSourceFormat => (cell.value, None),
                        _ => (cell.value, cell.formula),
                    };
                    let out = Cell { value, formula, style };
                    wb.sheets[si].set(r, c, if out.is_blank() { None } else { Some(out) });
                }
            }
        }
    }
    if o.link {
        return target;
    }
    if o.what.is_all() || o.what == PasteWhat::Comments {
        for &(br, bc) in &blocks {
            for (dr, dc, t) in &clip.notes {
                let (dr, dc) = if o.transpose { (*dc, *dr) } else { (*dr, *dc) };
                wb.sheets[si].notes.insert((br + dr, bc + dc), t.clone());
            }
        }
    }
    if !o.transpose {
        let cf = o.what.carries_formats() && o.what != PasteWhat::ValuesSourceFormat;
        let dv = o.what.is_all() || o.what == PasteWhat::Validation;
        if cf || dv {
            copy_rules(wb, si, clip, &blocks, cf, dv, o.what == PasteWhat::AllMergeCf);
        }
        if matches!(o.what, PasteWhat::ColumnWidths | PasteWhat::AllWithWidths) && clip.sheet < wb.sheets.len() {
            let widths: Vec<Option<f32>> = (0..sh_cols).map(|k| wb.sheets[clip.sheet].col_w.get(&(clip.range.c1 + k)).copied()).collect();
            for &(_, bc) in &blocks {
                for (k, wdt) in widths.iter().enumerate() {
                    let col = bc + k as u32;
                    match wdt {
                        Some(x) => wb.sheets[si].col_w.insert(col, *x),
                        None => wb.sheets[si].col_w.remove(&col),
                    };
                }
            }
        }
    }
    target
}

/// Paste Options menu entries: (group, label, shortcut letter, options).
fn paste_option_list() -> Vec<(&'static str, &'static str, char, PasteOpts)> {
    use PasteWhat as W;
    let t = PasteOpts { transpose: true, ..Default::default() };
    let link = PasteOpts { link: true, ..Default::default() };
    vec![
        ("Paste", "Paste", 'P', PasteOpts::default()),
        ("Paste", "Formulas", 'F', PasteOpts::what(W::Formulas)),
        ("Paste", "Formulas & Number Formatting", 'O', PasteOpts::what(W::FormulasNumFmt)),
        ("Paste", "Keep Source Formatting", 'K', PasteOpts::what(W::AllSourceTheme)),
        ("Paste", "No Borders", 'B', PasteOpts::what(W::AllExceptBorders)),
        ("Paste", "Keep Source Column Widths", 'W', PasteOpts::what(W::AllWithWidths)),
        ("Paste", "Transpose", 'T', t),
        ("Paste Values", "Values", 'V', PasteOpts::what(W::Values)),
        ("Paste Values", "Values & Number Formatting", 'A', PasteOpts::what(W::ValuesNumFmt)),
        ("Paste Values", "Values & Source Formatting", 'E', PasteOpts::what(W::ValuesSourceFormat)),
        ("Other Paste Options", "Formatting", 'R', PasteOpts::what(W::Formats)),
        ("Other Paste Options", "Paste Link", 'N', link),
    ]
}

impl App {
    /// Pastes the internal clipboard with the given options (Ctrl+V, Paste Special, Paste Options).
    pub fn paste_special(&mut self, o: PasteOpts) {
        let Some(clip) = self.clip.clone() else { return };
        self.commit_edit(false);
        let si = self.wb.active;
        let (h, w) = if o.transpose { (clip.range.cols(), clip.range.rows()) } else { (clip.range.rows(), clip.range.cols()) };
        // Tile the clip over a larger selection that is a multiple of its size.
        let sel = self.sel();
        let (tr, tc) = if !sel.is_single() && sel.rows() % h == 0 && sel.cols() % w == 0 && !sel.is_full_cols() && !sel.is_full_rows() {
            (sel.rows() / h, sel.cols() / w)
        } else {
            (1, 1)
        };
        let (r0, c0) = if tr * tc > 1 { (sel.r1, sel.c1) } else { self.active() };
        let target_est = Range::new(r0, c0, (r0 + h * tr - 1).min(MAX_ROWS - 1), (c0 + w * tc - 1).min(MAX_COLS - 1));
        if clip.cut {
            // Cut cells can only be moved (the familiar Paste Special is unavailable after Cut).
            self.move_clip(&clip, r0, c0);
            return;
        }
        if self.locked_block(target_est) {
            return;
        }
        // Rules, notes and widths live on the sheet: snapshot the sheet for those pastes.
        let src_has_rules = self.wb.sheets.get(clip.sheet).is_some_and(|s| {
            s.cond_formats.iter().any(|r| r.ranges.iter().any(|x| x.intersects(&clip.range)))
                || s.validations.iter().any(|v| v.ranges.iter().any(|x| x.intersects(&clip.range)))
        });
        let whole_sheet = !o.link && (!clip.notes.is_empty() || src_has_rules || !o.what.touches_cells() || o.what == PasteWhat::AllWithWidths);
        self.push_undo(&[if whole_sheet { Scope::Sheet(si) } else { Scope::Range(si, target_est) }]);
        let target = paste_cells(&mut self.wb, si, &clip, (r0, c0), (tr, tc), o);
        self.select_range(target);
        if whole_sheet {
            self.changed();
        } else {
            self.changed_ranges(si, &[target]);
        }
        self.fill_button = None;
        self.paste_button = Some(PasteButton { sheet: si, target, opts: o, edits: self.edits, open: false });
    }

    /// Paste after Cut: moves the cells, and formulas that pointed at them follow.
    fn move_clip(&mut self, clip: &Clip, r0: u32, c0: u32) {
        let si = self.wb.active;
        let target = Range::new(r0, c0, (r0 + clip.range.rows() - 1).min(MAX_ROWS - 1), (c0 + clip.range.cols() - 1).min(MAX_COLS - 1));
        // Both the cut cells (emptied) and the destination change.
        if self.locked_block(target) || self.protect_blocks_at(clip.sheet, Change::Cells(clip.range)) {
            return;
        }
        self.push_undo(&[Scope::All]);
        let src_sheet = clip.sheet;
        let mut taken = Vec::new();
        {
            let sh = &mut self.wb.sheets[src_sheet];
            let keys: Vec<(u32, u32)> = sh.iter_range(clip.range).map(|(r, c, _)| (r, c)).collect();
            for (r, c) in keys {
                if let Some(cell) = sh.remove(r, c) {
                    taken.push((r - clip.range.r1, c - clip.range.c1, cell));
                }
            }
        }
        let dr = r0 as i64 - clip.range.r1 as i64;
        let dc = c0 as i64 - clip.range.c1 as i64;
        let src_name = self.wb.sheets[src_sheet].name.clone();
        let src = clip.range;
        self.transform_formulas(|e, fsi| crate::formula::move_refs(e, &src_name, fsi == src_sheet, src, dr, dc));
        for (_, _, cell) in taken.iter_mut() {
            if let Some(f) = &mut cell.formula {
                if let Some(ast) = &mut f.ast {
                    crate::formula::move_refs(ast, &src_name, src_sheet == si, src, dr, dc);
                    f.text = crate::formula::print(ast);
                }
            }
        }
        let dest = &mut self.wb.sheets[si];
        let keys: Vec<(u32, u32)> = dest.iter_range(target).map(|(r, c, _)| (r, c)).collect();
        for (r, c) in keys {
            dest.remove(r, c);
        }
        for (dr0, dc0, cell) in taken {
            dest.set(r0 + dr0, c0 + dc0, Some(cell));
        }
        self.wb.sheets[src_sheet].notes.retain(|(r, c), _| !src.contains(*r, *c));
        for (dr0, dc0, t) in &clip.notes {
            self.wb.sheets[si].notes.insert((r0 + dr0, c0 + dc0), t.clone());
        }
        self.clip = None;
        self.paste_button = None;
        self.select_range(target);
        self.changed();
    }

    pub fn open_paste_special(&mut self) {
        self.commit_edit(false);
        if self.clip.is_none() {
            self.status = Some("Copy something first, then use Paste Special.".into());
            return;
        }
        if self.clip.as_ref().is_some_and(|c| c.cut) {
            // After Cut only a plain paste is possible.
            self.paste_special(PasteOpts::default());
            return;
        }
        self.dialog = Some(Dialog::PasteSpecial(Box::default()));
    }

    /// The Paste Special dialog. Returns false when it closes.
    pub fn paste_special_ui(&mut self, ctx: &egui::Context, d: &mut PasteSpecialDlg) -> bool {
        use PasteWhat as W;
        let mut keep = true;
        egui::Window::new("Paste Special").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, -40.0]).show(ctx, |ui| {
            ui.set_min_width(400.0);
            let group = |ui: &mut egui::Ui, title: &str| {
                ui.label(RichText::new(title).strong());
                ui.add(egui::Separator::default().spacing(4.0));
            };
            group(ui, "Paste");
            egui::Grid::new("ps_what").num_columns(2).spacing(Vec2::new(24.0, 4.0)).show(ui, |ui| {
                let left = [(W::All, "All"), (W::Formulas, "Formulas"), (W::Values, "Values"), (W::Formats, "Formats"), (W::Comments, "Comments and Notes"), (W::Validation, "Validation")];
                let right = [
                    (W::AllSourceTheme, "All using Source theme"),
                    (W::AllExceptBorders, "All except borders"),
                    (W::ColumnWidths, "Column widths"),
                    (W::FormulasNumFmt, "Formulas and number formats"),
                    (W::ValuesNumFmt, "Values and number formats"),
                    (W::AllMergeCf, "All merging conditional formats"),
                ];
                for (l, r) in left.iter().zip(right.iter()) {
                    ui.radio_value(&mut d.what, l.0, l.1);
                    ui.radio_value(&mut d.what, r.0, r.1);
                    ui.end_row();
                }
            });
            ui.add_space(6.0);
            group(ui, "Operation");
            egui::Grid::new("ps_op").num_columns(2).spacing(Vec2::new(24.0, 4.0)).show(ui, |ui| {
                ui.radio_value(&mut d.op, PasteOp::None, "None");
                ui.radio_value(&mut d.op, PasteOp::Multiply, "Multiply");
                ui.end_row();
                ui.radio_value(&mut d.op, PasteOp::Add, "Add");
                ui.radio_value(&mut d.op, PasteOp::Divide, "Divide");
                ui.end_row();
                ui.radio_value(&mut d.op, PasteOp::Subtract, "Subtract");
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut d.skip_blanks, "Skip blanks");
                ui.add_space(40.0);
                ui.checkbox(&mut d.transpose, "Transpose");
            });
            ui.add_space(8.0);
            let mut link = false;
            let mut ok = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let mut cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
            ui.horizontal(|ui| {
                // Paste Link needs plain "All" (or "All except borders") without operation/transpose.
                let can_link = matches!(d.what, W::All | W::AllExceptBorders) && d.op == PasteOp::None && !d.skip_blanks && !d.transpose;
                if ui.add_enabled(can_link, egui::Button::new("Paste Link").min_size(Vec2::new(90.0, 24.0))).clicked() {
                    link = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("Cancel").min_size(Vec2::new(76.0, 24.0))).clicked() {
                        cancel = true;
                    }
                    if ui.add(egui::Button::new("OK").min_size(Vec2::new(76.0, 24.0)).fill(theme::accent_light())).clicked() {
                        ok = true;
                    }
                });
            });
            if link {
                self.paste_special(PasteOpts { link: true, ..Default::default() });
                keep = false;
            } else if ok {
                self.paste_special(PasteOpts { what: d.what, op: d.op, skip_blanks: d.skip_blanks, transpose: d.transpose, link: false });
                keep = false;
            } else if cancel {
                keep = false;
            }
        });
        keep
    }

    /// Insert Copied Cells / Insert Cut Cells: makes room at the selection, then pastes.
    pub fn insert_paste_prompt(&mut self) {
        let Some(clip) = &self.clip else { return };
        match clip.whole {
            Some(rows) => self.insert_paste(rows),
            None => self.dialog = Some(Dialog::InsertPaste { down: true }),
        }
    }

    pub fn insert_paste(&mut self, down: bool) {
        let Some(mut clip) = self.clip.clone() else { return };
        self.commit_edit(false);
        let start = self.undo_len();
        let si = self.wb.active;
        let (r0, c0) = self.active();
        let (h, w) = (clip.range.rows(), clip.range.cols());
        let rmax = |r: u32| r.min(MAX_ROWS - 1);
        let cmax = |c: u32| c.min(MAX_COLS - 1);
        let (room, at) = match clip.whole {
            Some(true) => (Range::new(r0, 0, rmax(r0 + h - 1), MAX_COLS - 1), (r0, 0)),
            Some(false) => (Range::new(0, c0, MAX_ROWS - 1, cmax(c0 + w - 1)), (0, c0)),
            None => (Range::new(r0, c0, rmax(r0 + h - 1), cmax(c0 + w - 1)), (r0, c0)),
        };
        let down = clip.whole.unwrap_or(down);
        self.select_range(room);
        self.insert_cells(down);
        if self.undo_len() == start {
            // Refused (it would push data off the sheet).
            self.clip = Some(clip);
            return;
        }
        // The copied range moves too when it sat in the shifted band.
        if clip.sheet == si {
            let src = clip.range;
            if down && src.c1 >= room.c1 && src.c2 <= room.c2 && src.r1 >= room.r1 {
                clip.range = Range { r1: src.r1 + h, r2: rmax(src.r2 + h), ..src };
            } else if !down && src.r1 >= room.r1 && src.r2 <= room.r2 && src.c1 >= room.c1 {
                clip.range = Range { c1: src.c1 + w, c2: cmax(src.c2 + w), ..src };
            }
        }
        self.clip = Some(clip.clone());
        self.select(at.0, at.1, false);
        if clip.cut {
            self.move_clip(&clip, at.0, at.1);
            // Moving whole rows/columns closes the gap they left behind.
            if let (Some(rows), true) = (clip.whole, clip.sheet == si) {
                let src = clip.range;
                let gap = if rows { Range::new(src.r1, 0, src.r2, MAX_COLS - 1) } else { Range::new(0, src.c1, MAX_ROWS - 1, src.c2) };
                self.select_range(gap);
                if rows { self.delete_rows() } else { self.delete_cols() }
                let moved = if rows {
                    let r = if src.r1 < at.0 { at.0 - h } else { at.0 };
                    Range::new(r, 0, rmax(r + h - 1), MAX_COLS - 1)
                } else {
                    let c = if src.c1 < at.1 { at.1 - w } else { at.1 };
                    Range::new(0, c, MAX_ROWS - 1, cmax(c + w - 1))
                };
                self.select_range(moved);
            }
        } else {
            self.paste_special(PasteOpts::default());
        }
        self.squash_undo(start);
    }

    /// "Insert Paste" prompt for Insert Copied/Cut Cells on a partial range.
    pub fn insert_paste_ui(&mut self, ctx: &egui::Context, down: &mut bool) -> bool {
        let mut keep = true;
        egui::Window::new("Insert Paste").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, -40.0]).show(ctx, |ui| {
            ui.set_min_width(200.0);
            ui.label(RichText::new("Insert").strong());
            ui.radio_value(down, false, "Shift cells right");
            ui.radio_value(down, true, "Shift cells down");
            ui.add_space(6.0);
            let mut ok = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let mut cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                cancel |= ui.add(egui::Button::new("Cancel").min_size(Vec2::new(76.0, 24.0))).clicked();
                ok |= ui.add(egui::Button::new("OK").min_size(Vec2::new(76.0, 24.0)).fill(theme::accent_light())).clicked();
            });
            if ok {
                keep = false;
                self.insert_paste(*down);
            } else if cancel {
                keep = false;
            }
        });
        keep
    }

    /// Floating Paste Options button (also opened with Ctrl after a paste).
    pub fn paste_button_ui(&mut self, ui: &mut egui::Ui, lay: &Layout, busy: bool) {
        if self.paste_button.as_ref().is_some_and(|b| b.edits != self.edits || b.sheet >= self.wb.sheets.len() || self.clip.is_none()) {
            self.paste_button = None;
        }
        let Some(b) = &self.paste_button else { return };
        if b.sheet != self.wb.active || busy {
            return;
        }
        let at = self.range_rect(lay, b.target).right_bottom();
        if !lay.cells.contains(at) {
            return;
        }
        let (open, cur) = (b.open, b.opts);
        let (clicked, rect) = float_button(ui, egui::Id::new("paste_options"), at, open, paint_paste_icon);
        self.menu.float_rects.push(rect);
        if clicked {
            self.toggle_paste_menu();
        }
        if open {
            self.paste_menu(ui.ctx(), rect, cur);
        }
    }

    /// Ctrl (pressed alone) after a paste opens the Paste Options menu.
    pub fn toggle_paste_menu(&mut self) -> bool {
        match &mut self.paste_button {
            Some(b) if b.sheet == self.wb.active => {
                b.open = !b.open;
                true
            }
            _ => false,
        }
    }

    pub fn paste_menu_open(&self) -> bool {
        self.paste_button.as_ref().is_some_and(|b| b.open)
    }

    /// Esc after a paste: removes the Paste Options button.
    pub fn dismiss_paste_button(&mut self) -> bool {
        self.paste_button.take().is_some()
    }

    /// A letter typed while the Paste Options menu is open.
    pub fn paste_menu_key(&mut self, ch: char) -> bool {
        if !self.paste_menu_open() {
            return false;
        }
        if let Some((_, _, _, o)) = paste_option_list().into_iter().find(|x| x.2 == ch.to_ascii_uppercase()) {
            self.repaste(o);
        }
        true
    }

    fn repaste(&mut self, o: PasteOpts) {
        if self.paste_button.take().is_none() {
            return;
        }
        let saved = self.sheet().view.clone();
        self.undo();
        // Paste again into the same place (undo restored the original selection).
        self.sheet_mut().view = saved.clone();
        let tgt = saved.sel;
        self.select(tgt.r1, tgt.c1, false);
        self.paste_special(o);
    }

    fn paste_menu(&mut self, ctx: &egui::Context, anchor: Rect, cur: PasteOpts) {
        let mut pick = None;
        let resp = egui::Area::new(egui::Id::new("paste_options_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor.left_bottom() + Vec2::new(0.0, 2.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(230.0);
                    let mut group = "";
                    for (g, label, key, o) in paste_option_list() {
                        if g != group {
                            if !group.is_empty() {
                                ui.add_space(2.0);
                            }
                            ui.label(RichText::new(g).strong().size(12.0));
                            group = g;
                        }
                        let text = format!("{label} ({key})");
                        if ui.selectable_label(o == cur, text).clicked() {
                            pick = Some(o);
                        }
                    }
                });
            });
        if let Some(o) = pick {
            self.repaste(o);
        } else if ctx.input(|i| i.pointer.any_pressed())
            && !ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| resp.response.rect.contains(p) || anchor.contains(p))
        {
            if let Some(b) = &mut self.paste_button {
                b.open = false;
            }
        }
    }

    /// Icon row for the cell menu's "Paste Options:" (Paste, Values, Formulas, Transpose,
    /// Formatting, Paste Link).
    pub fn paste_icon_row(&mut self, ui: &mut egui::Ui) -> bool {
        use PasteWhat as W;
        let enabled = self.clip.is_some();
        let cut = self.clip.as_ref().is_some_and(|c| c.cut);
        let items: [(&str, &str, PasteOpts); 6] = [
            ("", "Paste (P)", PasteOpts::default()),
            ("123", "Values (V)", PasteOpts::what(W::Values)),
            ("fx", "Formulas (F)", PasteOpts::what(W::Formulas)),
            ("⇆", "Transpose (T)", PasteOpts { transpose: true, ..Default::default() }),
            ("%", "Formatting (R)", PasteOpts::what(W::Formats)),
            ("∞", "Paste Link (N)", PasteOpts { link: true, ..Default::default() }),
        ];
        let mut done = false;
        ui.label(RichText::new("Paste Options:").color(theme::text_dim()).size(12.0));
        ui.horizontal(|ui| {
            ui.add_space(18.0);
            for (i, (badge, tip, o)) in items.into_iter().enumerate() {
                let en = enabled && (i == 0 || !cut);
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(28.0, 28.0), egui::Sense::click());
                let resp = if en { resp } else { resp.on_disabled_hover_text(tip) };
                let p = ui.painter();
                if en && resp.hovered() {
                    p.rect_filled(rect, 2.0, theme::hover());
                    p.rect_stroke(rect, 2.0, Stroke::new(1.0, theme::border()), egui::StrokeKind::Inside);
                }
                let col = if en { theme::text() } else { theme::text_dim().gamma_multiply(0.6) };
                let (g, is_icon) = self.fonts.icons.get(theme::ic::PASTE, "📋");
                let font = if is_icon { theme::icon_font(16.0) } else { theme::ui_font(14.0) };
                p.text(rect.center() - Vec2::new(2.0, 2.0), Align2::CENTER_CENTER, g, font, col);
                if !badge.is_empty() {
                    let b = Pos2::new(rect.right() - 3.0, rect.bottom() - 2.0);
                    let galley = p.layout_no_wrap(badge.to_string(), theme::ui_bold(9.0), theme::accent_dark());
                    let br = Rect::from_min_size(b - galley.size(), galley.size()).expand(1.0);
                    p.rect_filled(br, 1.0, theme::window());
                    p.galley(br.min + Vec2::splat(1.0), galley, if en { theme::accent_dark() } else { col });
                }
                if en && resp.on_hover_text(tip).clicked() {
                    self.paste_special(o);
                    done = true;
                }
            }
        });
        done
    }
}

/// Paste Options icon: a clipboard.
fn paint_paste_icon(p: &egui::Painter, r: Rect) {
    let board = r.shrink2(Vec2::new(1.5, 0.5));
    p.rect_stroke(board, 1.0, Stroke::new(1.2, theme::text_dim()), egui::StrokeKind::Inside);
    let clip = Rect::from_center_size(Pos2::new(board.center().x, board.top() + 1.0), Vec2::new(6.0, 3.0));
    p.rect_filled(clip, 1.0, theme::text_dim());
    for k in 0..3 {
        let y = board.top() + 5.0 + k as f32 * 2.8;
        p.line_segment([Pos2::new(board.left() + 2.5, y), Pos2::new(board.right() - 2.5, y)], Stroke::new(1.0, theme::accent()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Sheet;

    fn setup(vals: &[(u32, u32, &str)]) -> Workbook {
        let mut wb = Workbook::empty_with(vec![Sheet::new("Sheet1"), Sheet::new("Sheet2")]);
        for (r, c, t) in vals {
            let cell = cell_from_input(&mut wb, t, 0);
            wb.sheets[0].set(*r, *c, cell);
        }
        wb.recalc();
        wb
    }

    fn clip_of(wb: &Workbook, si: usize, rg: Range) -> Clip {
        let sh = &wb.sheets[si];
        let cells = sh
            .iter_range(rg)
            .map(|(r, c, cell)| ClipCell { dr: r - rg.r1, dc: c - rg.c1, cell: cell.clone(), style: wb.style(cell.style).clone() })
            .collect();
        Clip { sheet: si, range: rg, cells, text: String::new(), cut: false, notes: Vec::new(), whole: None }
    }

    #[test]
    fn values_formulas_and_operations() {
        let mut wb = setup(&[(0, 0, "2"), (1, 0, "=A1*10"), (0, 2, "5"), (1, 2, "7")]);
        let clip = clip_of(&wb, 0, Range::new(0, 0, 1, 0));
        // Values: the formula's result only.
        paste_cells(&mut wb, 0, &clip, (0, 1), (1, 1), PasteOpts::what(PasteWhat::Values));
        assert_eq!(wb.sheets[0].value(1, 1), &Value::Number(20.0));
        assert!(wb.sheets[0].get(1, 1).unwrap().formula.is_none());
        // Add onto existing numbers in column C.
        let col_b = clip_of(&wb, 0, Range::new(0, 1, 1, 1));
        paste_cells(&mut wb, 0, &col_b, (0, 2), (1, 1), PasteOpts { what: PasteWhat::Values, op: PasteOp::Add, ..Default::default() });
        assert_eq!(wb.sheets[0].value(0, 2), &Value::Number(7.0));
        assert_eq!(wb.sheets[0].value(1, 2), &Value::Number(27.0));
        // Divide by a blank source cell gives #DIV/0!.
        let blank = clip_of(&wb, 0, Range::cell(5, 5));
        paste_cells(&mut wb, 0, &blank, (0, 2), (1, 1), PasteOpts { what: PasteWhat::Values, op: PasteOp::Divide, ..Default::default() });
        assert_eq!(wb.sheets[0].value(0, 2), &Value::Error(ErrorKind::Div0));
    }

    #[test]
    fn skip_blanks_transpose_and_link() {
        let mut wb = setup(&[(0, 0, "1"), (2, 0, "3"), (1, 3, "keep")]);
        let clip = clip_of(&wb, 0, Range::new(0, 0, 2, 0));
        paste_cells(&mut wb, 0, &clip, (1, 2), (1, 1), PasteOpts { transpose: true, skip_blanks: true, ..Default::default() });
        assert_eq!(wb.sheets[0].value(1, 2), &Value::Number(1.0));
        assert_eq!(wb.sheets[0].value(1, 3), &Value::Text("keep".into()));
        assert_eq!(wb.sheets[0].value(1, 4), &Value::Number(3.0));
        // Paste Link from another sheet: relative references for a range.
        paste_cells(&mut wb, 1, &clip, (0, 0), (1, 1), PasteOpts { link: true, ..Default::default() });
        assert_eq!(wb.sheets[1].get(2, 0).unwrap().formula.as_ref().unwrap().text, "Sheet1!A3");
        let one = clip_of(&wb, 0, Range::cell(0, 0));
        paste_cells(&mut wb, 0, &one, (9, 9), (1, 1), PasteOpts { link: true, ..Default::default() });
        assert_eq!(wb.sheets[0].get(9, 9).unwrap().formula.as_ref().unwrap().text, "$A$1");
    }

    #[test]
    fn formats_number_formats_and_widths() {
        let mut wb = setup(&[(0, 0, "$5.00"), (0, 1, "12")]);
        wb.sheets[0].col_w.insert(0, 150.0);
        let clip = clip_of(&wb, 0, Range::cell(0, 0));
        paste_cells(&mut wb, 0, &clip, (0, 1), (1, 1), PasteOpts::what(PasteWhat::Formats));
        assert_eq!(wb.sheets[0].value(0, 1), &Value::Number(12.0));
        assert_eq!(wb.style(wb.sheets[0].get(0, 1).unwrap().style).num_fmt, "\"$\"#,##0.00");
        paste_cells(&mut wb, 0, &clip, (3, 3), (1, 1), PasteOpts::what(PasteWhat::ValuesNumFmt));
        assert_eq!(wb.style(wb.sheets[0].get(3, 3).unwrap().style).num_fmt, "\"$\"#,##0.00");
        paste_cells(&mut wb, 0, &clip, (0, 5), (1, 1), PasteOpts::what(PasteWhat::ColumnWidths));
        assert_eq!(wb.sheets[0].col_w.get(&5), Some(&150.0));
        assert!(wb.sheets[0].get(0, 5).is_none());
    }
}
