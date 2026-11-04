//! Commands: clipboard, fill, sort, filter, formatting, structure and sheets.

use std::cmp::Ordering;
use std::collections::HashSet;

use eframe::egui;

use super::dialogs::Dialog;
use super::{App, Clip, ClipCell, EditMode, Editor, Scope};
use crate::formula::{self, Expr};
use crate::model::{
    cell_from_input, Border, BorderLine, Cell, Formula, HAlign, Range, Sheet, Style, VAlign, Value, MAX_COLS, MAX_ROWS,
};
use crate::numfmt;

/// A command that F4 / Ctrl+Y can repeat.
#[derive(Clone)]
pub enum Repeat {
    Style(std::rc::Rc<dyn Fn(&mut Style)>),
    InsertRows,
    InsertCols,
    DeleteRows,
    DeleteCols,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PasteMode {
    All,
    Values,
    Formulas,
    Formats,
    Transpose,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BorderKind {
    Bottom,
    Top,
    Left,
    Right,
    None,
    All,
    Outside,
    ThickOutside,
    DoubleBottom,
    ThickBottom,
}

const MONTHS_SHORT: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTHS_LONG: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December",
];
const DAYS_SHORT: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const DAYS_LONG: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

fn reprint(cell: &mut Cell) {
    if let Some(f) = &mut cell.formula {
        if let Some(ast) = &f.ast {
            f.text = formula::print(ast);
        }
    }
}

fn shift_cell(cell: &mut Cell, dr: i64, dc: i64) {
    if dr == 0 && dc == 0 {
        return;
    }
    if let Some(f) = &mut cell.formula {
        if let Some(ast) = &mut f.ast {
            formula::shift_relative(ast, dr, dc);
            f.text = formula::print(ast);
        }
    }
}

impl App {
    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// Selection clipped to the used area (for whole-row/column selections).
    pub fn sel_clipped(&self) -> Range {
        let sel = self.sel();
        let (er, ec) = self.sheet().extent().unwrap_or((0, 0));
        Range { r1: sel.r1, c1: sel.c1, r2: sel.r2.min(er.max(sel.r1)), c2: sel.c2.min(ec.max(sel.c1)) }
    }

    /// All selected areas (Ctrl+click multi-selection), active area last.
    pub fn areas(&self) -> Vec<Range> {
        let v = &self.sheet().view;
        let mut out: Vec<Range> = v.extra.clone();
        out.push(v.sel);
        out
    }

    /// An area clipped to the used part of the sheet (for whole rows/columns).
    pub fn clip_area(&self, a: Range) -> Range {
        let (er, ec) = self.sheet().extent().unwrap_or((0, 0));
        Range { r1: a.r1, c1: a.c1, r2: a.r2.min(er.max(a.r1)), c2: a.c2.min(ec.max(a.c1)) }
    }

    /// F4 / Ctrl+Y: repeats the last formatting or insert/delete command.
    pub fn repeat_last(&mut self) {
        match self.last_action.clone() {
            Some(Repeat::Style(f)) => self.apply_style_rc(f),
            Some(Repeat::InsertRows) => self.insert_rows(),
            Some(Repeat::InsertCols) => self.insert_cols(),
            Some(Repeat::DeleteRows) => self.delete_rows(),
            Some(Repeat::DeleteCols) => self.delete_cols(),
            None => {}
        }
    }

    /// Applies all formula transforms across the workbook.
    pub fn transform_formulas(&mut self, mut f: impl FnMut(&mut Expr, usize)) {
        for (si, sh) in self.wb.sheets.iter_mut().enumerate() {
            crate::condfmt::map_sheet_formulas(sh, &mut |e| f(e, si));
            for row in sh.rows.values_mut() {
                for cell in row.values_mut() {
                    if let Some(fm) = &mut cell.formula {
                        if let Some(ast) = &mut fm.ast {
                            let before = ast.clone();
                            f(ast, si);
                            if *ast != before {
                                fm.text = formula::print(ast);
                            }
                        }
                    }
                }
            }
        }
        let names = std::mem::take(&mut self.wb.names);
        self.wb.names = names
            .into_iter()
            .map(|(n, t)| match formula::parse(t.trim_start_matches('=')) {
                Ok(mut e) => {
                    f(&mut e, usize::MAX);
                    (n, formula::print(&e))
                }
                Err(_) => (n, t),
            })
            .collect();
    }

    /// The contiguous data block around the active cell.
    pub fn current_region(&self) -> Range {
        let sh = self.sheet();
        let (r, c) = self.active();
        let mut rg = Range::cell(r, c);
        loop {
            let mut grown = rg;
            let r1 = rg.r1.saturating_sub(1);
            let c1 = rg.c1.saturating_sub(1);
            let r2 = (rg.r2 + 1).min(MAX_ROWS - 1);
            let c2 = (rg.c2 + 1).min(MAX_COLS - 1);
            if rg.r1 > 0 && (c1..=c2).any(|c| sh.is_populated(r1, c)) {
                grown.r1 = r1;
            }
            if (c1..=c2).any(|c| sh.is_populated(r2, c)) && r2 > rg.r2 {
                grown.r2 = r2;
            }
            if rg.c1 > 0 && (r1..=r2).any(|r| sh.is_populated(r, c1)) {
                grown.c1 = c1;
            }
            if (r1..=r2).any(|r| sh.is_populated(r, c2)) && c2 > rg.c2 {
                grown.c2 = c2;
            }
            if grown == rg {
                return rg;
            }
            rg = grown;
            if rg.rows() as u64 * rg.cols() as u64 > 50_000_000 {
                return rg;
            }
        }
    }

    // ------------------------------------------------------------------
    // Navigation
    // ------------------------------------------------------------------

    pub fn move_active(&mut self, dr: i64, dc: i64, extend: bool) {
        let v = &self.sheet().view;
        let (r, c) = if extend {
            // move the far corner of the selection
            let (ar, ac) = v.anchor;
            let sel = v.sel;
            let fr = if sel.r1 == ar { sel.r2 } else { sel.r1 };
            let fc = if sel.c1 == ac { sel.c2 } else { sel.c1 };
            (fr, fc)
        } else {
            let (r, c) = v.active;
            match self.sheet().merge_at(r, c) {
                Some(m) => (if dr > 0 { m.r2 } else { m.r1 }, if dc > 0 { m.c2 } else { m.c1 }),
                None => (r, c),
            }
        };
        let mut nr = (r as i64 + dr).clamp(0, MAX_ROWS as i64 - 1) as u32;
        let mut nc = (c as i64 + dc).clamp(0, MAX_COLS as i64 - 1) as u32;
        // skip hidden rows/cols
        let sh = self.sheet();
        while dr != 0 && sh.hidden_rows.contains(&nr) && nr > 0 && nr < MAX_ROWS - 1 {
            nr = (nr as i64 + dr.signum()) as u32;
        }
        while dc != 0 && sh.hidden_cols.contains(&nc) && nc > 0 && nc < MAX_COLS - 1 {
            nc = (nc as i64 + dc.signum()) as u32;
        }
        self.select(nr, nc, extend);
    }

    /// Ctrl+Arrow: jump to the edge of the data region.
    pub fn jump(&mut self, dr: i64, dc: i64, extend: bool) {
        let v = &self.sheet().view;
        let (r, c) = if extend {
            let (ar, ac) = v.anchor;
            let sel = v.sel;
            (if sel.r1 == ar { sel.r2 } else { sel.r1 }, if sel.c1 == ac { sel.c2 } else { sel.c1 })
        } else {
            v.active
        };
        let sh = self.sheet();
        let (er, ec) = sh.extent_with_spills().unwrap_or((0, 0));
        let pop =|r: i64, c: i64| r >= 0 && c >= 0 && r < MAX_ROWS as i64 && c < MAX_COLS as i64 && sh.is_populated(r as u32, c as u32);
        let (mut cr, mut cc) = (r as i64, c as i64);
        let max_r = MAX_ROWS as i64 - 1;
        let max_c = MAX_COLS as i64 - 1;
        let at_edge = |r: i64, c: i64| r + dr < 0 || c + dc < 0 || r + dr > max_r || c + dc > max_c;
        if !at_edge(cr, cc) {
            if pop(cr, cc) && pop(cr + dr, cc + dc) {
                while !at_edge(cr, cc) && pop(cr + dr, cc + dc) {
                    cr += dr;
                    cc += dc;
                }
            } else {
                loop {
                    cr += dr;
                    cc += dc;
                    if pop(cr, cc) || at_edge(cr, cc) {
                        break;
                    }
                    // beyond data: go straight to the sheet edge
                    if (dr > 0 && cr > er as i64) || (dc > 0 && cc > ec as i64) {
                        if dr > 0 {
                            cr = max_r;
                        }
                        if dc > 0 {
                            cc = max_c;
                        }
                        break;
                    }
                }
            }
        }
        self.select(cr as u32, cc as u32, extend);
    }

    pub fn page(&mut self, down: bool, extend: bool) {
        let n = self.layout.as_ref().map(|l| l.rows.len().max(2) as i64 - 1).unwrap_or(20);
        let n = if down { n } else { -n };
        let fr = self.sheet().freeze.0;
        let v = &mut self.sheet_mut().view;
        v.top = (v.top as i64 + n).clamp(fr as i64, MAX_ROWS as i64 - 1) as u32;
        self.move_active(n, 0, extend);
    }

    pub fn page_horiz(&mut self, right: bool, extend: bool) {
        let n = self.layout.as_ref().map(|l| l.cols.len().max(2) as i64 - 1).unwrap_or(8);
        let n = if right { n } else { -n };
        let fc = self.sheet().freeze.1;
        let v = &mut self.sheet_mut().view;
        v.left = (v.left as i64 + n).clamp(fc as i64, MAX_COLS as i64 - 1) as u32;
        self.move_active(0, n, extend);
    }

    /// Enter/Tab movement that cycles inside a multi-cell selection.
    pub fn move_in_selection(&mut self, dr: i64, dc: i64) {
        let sel = self.sel();
        if sel.is_single() || self.expand_merges(Range::cell(self.active().0, self.active().1)) == sel {
            if dc != 0 {
                if self.tab_origin.is_none() {
                    self.tab_origin = Some(self.active().1);
                }
            } else if dr > 0 {
                if let Some(c) = self.tab_origin.take() {
                    let r = self.active().0;
                    self.select(r + 1, c, false);
                    return;
                }
            } else {
                self.tab_origin = None;
            }
            self.move_active(dr, dc, false);
            return;
        }
        let (mut r, mut c) = self.active();
        if dr != 0 {
            let nr = r as i64 + dr;
            if nr > sel.r2 as i64 {
                r = sel.r1;
                c = if c >= sel.c2 { sel.c1 } else { c + 1 };
            } else if nr < sel.r1 as i64 {
                r = sel.r2;
                c = if c <= sel.c1 { sel.c2 } else { c - 1 };
            } else {
                r = nr as u32;
            }
        } else {
            let nc = c as i64 + dc;
            if nc > sel.c2 as i64 {
                c = sel.c1;
                r = if r >= sel.r2 { sel.r1 } else { r + 1 };
            } else if nc < sel.c1 as i64 {
                c = sel.c2;
                r = if r <= sel.r1 { sel.r2 } else { r - 1 };
            } else {
                c = nc as u32;
            }
        }
        let v = &mut self.sheet_mut().view;
        v.active = (r, c);
        self.scroll_into_view(r, c);
    }

    pub fn select_all_smart(&mut self) {
        let region = self.current_region();
        let sel = self.sel();
        if !region.is_single() && sel != region {
            self.select_range(region);
            let (r, c) = self.sheet().view.active;
            let _ = (r, c);
        } else {
            let a = self.active();
            self.select_range(Range::new(0, 0, MAX_ROWS - 1, MAX_COLS - 1));
            self.sheet_mut().view.active = a;
        }
    }

    pub fn go_to(&mut self, target: &str) -> bool {
        let t = target.trim();
        let (sheet, refpart) = match t.rsplit_once('!') {
            Some((s, r)) => (Some(s.trim_matches('\'').to_string()), r),
            None => (None, t),
        };
        if let Some(s) = sheet {
            match self.wb.sheet_index(&s) {
                Some(i) => self.wb.active = i,
                None => return false,
            }
        }
        if let Some(rg) = Range::parse(refpart) {
            if rg.is_single() {
                self.select(rg.r1, rg.c1, false);
            } else {
                self.select_range(rg);
            }
            return true;
        }
        // defined name
        if let Some((_, f)) = self.wb.names.iter().find(|(n, _)| n.eq_ignore_ascii_case(refpart)).cloned() {
            return self.go_to(f.trim_start_matches('='));
        }
        false
    }

    // ------------------------------------------------------------------
    // Clipboard
    // ------------------------------------------------------------------

    pub fn copy(&mut self, ctx: &egui::Context, cut: bool) {
        let sel = self.sel_clipped();
        let sh = self.sheet();
        let mut cells = Vec::new();
        let mut text = String::new();
        for r in sel.r1..=sel.r2 {
            if sh.hidden_rows.contains(&r) && !cut {
                continue;
            }
            let mut line = Vec::new();
            for c in sel.c1..=sel.c2 {
                let cell = sh.get(r, c);
                let t = cell
                    .map(|cell| numfmt::format_value(&cell.value, &self.wb.style(cell.style).num_fmt).text)
                    .unwrap_or_default();
                let t = if t.contains(['\t', '\n', '"']) { format!("\"{}\"", t.replace('"', "\"\"")) } else { t };
                line.push(t);
                if let Some(cell) = cell {
                    cells.push(ClipCell { dr: r - sel.r1, dc: c - sel.c1, cell: cell.clone(), style: self.wb.style(cell.style).clone() });
                }
            }
            text.push_str(&line.join("\t"));
            text.push_str("\r\n");
        }
        ctx.copy_text(text.clone());
        let notes = sh.notes.iter().filter(|((r, c), _)| sel.contains(*r, *c)).map(|((r, c), t)| (r - sel.r1, c - sel.c1, t.clone())).collect();
        self.clip = Some(Clip { sheet: self.wb.active, range: sel, cells, text, cut, notes });
    }

    fn normalize(s: &str) -> String {
        s.replace("\r\n", "\n").trim_end_matches('\n').to_string()
    }

    /// Handles a paste event carrying system clipboard text.
    pub fn paste_text(&mut self, text: &str) {
        if let Some(clip) = &self.clip {
            if Self::normalize(&clip.text) == Self::normalize(text) {
                self.paste_internal(PasteMode::All);
                return;
            }
        }
        self.clip = None;
        let rows = parse_tsv(text);
        if rows.is_empty() {
            return;
        }
        let (r0, c0) = self.active();
        let h = rows.len() as u32;
        let w = rows.iter().map(|r| r.len()).max().unwrap_or(1) as u32;
        let target = Range::new(r0, c0, (r0 + h - 1).min(MAX_ROWS - 1), (c0 + w - 1).min(MAX_COLS - 1));
        self.push_undo(&[Scope::Range(self.wb.active, target)]);
        let si = self.wb.active;
        for (i, row) in rows.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                let (r, c) = (r0 + i as u32, c0 + j as u32);
                if r >= MAX_ROWS || c >= MAX_COLS {
                    continue;
                }
                let style = self.wb.sheets[si].get(r, c).map(|c| c.style).unwrap_or(0);
                let cell = cell_from_input(&mut self.wb, v, style);
                self.wb.sheets[si].set(r, c, cell);
            }
        }
        self.select_range(target);
        self.changed_ranges(si, &[target]);
    }

    pub fn paste_internal(&mut self, mode: PasteMode) {
        let Some(clip) = self.clip.clone() else { return };
        let si = self.wb.active;
        let (r0, c0) = self.active();
        let (h, w) = if mode == PasteMode::Transpose { (clip.range.cols(), clip.range.rows()) } else { (clip.range.rows(), clip.range.cols()) };
        // Tile the clip over a larger selection that is a multiple of its size.
        let sel = self.sel();
        let (tr, tc) = if !sel.is_single() && sel.rows() % h == 0 && sel.cols() % w == 0 && !sel.is_full_cols() && !sel.is_full_rows() {
            (sel.rows() / h, sel.cols() / w)
        } else {
            (1, 1)
        };
        let (r0, c0) = if tr * tc > 1 { (sel.r1, sel.c1) } else { (r0, c0) };
        let target = Range::new(r0, c0, (r0 + h * tr - 1).min(MAX_ROWS - 1), (c0 + w * tc - 1).min(MAX_COLS - 1));

        if clip.cut && mode == PasteMode::All {
            self.push_undo(&[Scope::All]);
            // Move: take the cells out, fix references, put them back at the destination.
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
            self.transform_formulas(|e, fsi| formula::move_refs(e, &src_name, fsi == src_sheet, src, dr, dc));
            for (_, _, cell) in taken.iter_mut() {
                if let Some(f) = &mut cell.formula {
                    if let Some(ast) = &mut f.ast {
                        formula::move_refs(ast, &src_name, src_sheet == si, src, dr, dc);
                        f.text = formula::print(ast);
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
            let src_rg = clip.range;
            self.wb.sheets[src_sheet].notes.retain(|(r, c), _| !src_rg.contains(*r, *c));
            for (dr0, dc0, t) in &clip.notes {
                self.wb.sheets[si].notes.insert((r0 + dr0, c0 + dc0), t.clone());
            }
            self.clip = None;
            self.select_range(target);
            self.changed();
            return;
        }

        if matches!(mode, PasteMode::All | PasteMode::Transpose) && !clip.notes.is_empty() {
            self.push_undo(&[Scope::Sheet(si)]);
            for ti in 0..tr {
                for tj in 0..tc {
                    for (dr, dc, t) in &clip.notes {
                        let (dr, dc) = if mode == PasteMode::Transpose { (*dc, *dr) } else { (*dr, *dc) };
                        self.wb.sheets[si].notes.insert((r0 + ti * h + dr, c0 + tj * w + dc), t.clone());
                    }
                }
            }
        } else {
            self.push_undo(&[Scope::Range(si, target)]);
        }
        for ti in 0..tr {
            for tj in 0..tc {
                let br = r0 + ti * h;
                let bc = c0 + tj * w;
                // clear destination block for full pastes
                if matches!(mode, PasteMode::All | PasteMode::Transpose) {
                    let blk = Range::new(br, bc, (br + h - 1).min(MAX_ROWS - 1), (bc + w - 1).min(MAX_COLS - 1));
                    let keys: Vec<(u32, u32)> = self.wb.sheets[si].iter_range(blk).map(|(r, c, _)| (r, c)).collect();
                    for (r, c) in keys {
                        self.wb.sheets[si].remove(r, c);
                    }
                }
                for cc in &clip.cells {
                    let (dr, dc) = if mode == PasteMode::Transpose { (cc.dc, cc.dr) } else { (cc.dr, cc.dc) };
                    let (r, c) = (br + dr, bc + dc);
                    if r >= MAX_ROWS || c >= MAX_COLS {
                        continue;
                    }
                    let style_id = self.wb.intern_style(cc.style.clone());
                    let existing = self.wb.sheets[si].get(r, c).cloned().unwrap_or_default();
                    let mut cell = cc.cell.clone();
                    cell.style = style_id;
                    shift_cell(&mut cell, r as i64 - (clip.range.r1 + cc.dr) as i64, c as i64 - (clip.range.c1 + cc.dc) as i64);
                    let out = match mode {
                        PasteMode::All | PasteMode::Transpose => cell,
                        PasteMode::Values => Cell { value: cell.value, formula: None, style: existing.style },
                        PasteMode::Formulas => Cell { value: cell.value, formula: cell.formula, style: existing.style },
                        PasteMode::Formats => Cell { value: existing.value, formula: existing.formula, style: style_id },
                    };
                    self.wb.sheets[si].set(r, c, Some(out));
                }
            }
        }
        self.select_range(target);
        self.changed_ranges(si, &[target]);
    }

    // ------------------------------------------------------------------
    // Clear / delete
    // ------------------------------------------------------------------

    pub fn clear(&mut self, contents: bool, formats: bool) {
        let areas = self.areas();
        if areas.len() > 1 {
            // Multi-area: clear each area (one undo step per area keeps this simple and exact).
            let saved = self.sheet().view.clone();
            for a in areas {
                self.sheet_mut().view.sel = a;
                self.sheet_mut().view.extra.clear();
                self.clear(contents, formats);
            }
            self.sheet_mut().view = saved;
            return;
        }
        let sel = self.sel_clipped();
        let si = self.wb.active;
        if formats {
            self.push_undo(&[Scope::Sheet(si)]);
        } else {
            self.push_undo(&[Scope::Range(si, sel)]);
        }
        let keys: Vec<(u32, u32)> = self.wb.sheets[si].iter_range(sel).map(|(r, c, _)| (r, c)).collect();
        for (r, c) in keys {
            let sh = &mut self.wb.sheets[si];
            if let Some(cell) = sh.get_mut(r, c) {
                if contents {
                    cell.value = Value::Empty;
                    cell.formula = None;
                }
                if formats {
                    cell.style = 0;
                }
            }
            sh.tidy(r, c);
        }
        if formats {
            let sh = &mut self.wb.sheets[si];
            sh.merges.retain(|m| !m.intersects(&sel));
            self.changed();
        } else {
            self.changed_ranges(si, &[sel]);
        }
        if contents && formats {
            self.wb.sheets[si].notes.retain(|(r, c), _| !sel.contains(*r, *c));
            self.wb.sheets[si].links.retain(|(r, c), _| !sel.contains(*r, *c));
        }
    }

    // ------------------------------------------------------------------
    // Structure: rows / columns
    // ------------------------------------------------------------------

    fn shift_sheet(sh: &mut Sheet, rows: bool, at: u32, n: i64) {
        let max = if rows { MAX_ROWS } else { MAX_COLS } as i64;
        let map = |x: u32| -> Option<u32> {
            let x = x as i64;
            if n < 0 && x >= at as i64 && x < at as i64 - n {
                return None;
            }
            let y = if x >= at as i64 { x + n } else { x };
            if y < 0 || y >= max { None } else { Some(y as u32) }
        };
        if rows {
            let old = std::mem::take(&mut sh.rows);
            for (r, row) in old {
                if let Some(nr) = map(r) {
                    sh.rows.insert(nr, row);
                }
            }
            sh.row_h = sh.row_h.drain().filter_map(|(r, h)| map(r).map(|nr| (nr, h))).collect();
            sh.hidden_rows = sh.hidden_rows.drain().filter_map(map).collect();
        } else {
            for row in sh.rows.values_mut() {
                let old = std::mem::take(row);
                for (c, cell) in old {
                    if let Some(nc) = map(c) {
                        row.insert(nc, cell);
                    }
                }
            }
            sh.rows.retain(|_, r| !r.is_empty());
            sh.col_w = sh.col_w.drain().filter_map(|(c, w)| map(c).map(|nc| (nc, w))).collect();
            sh.hidden_cols = sh.hidden_cols.drain().filter_map(map).collect();
        }
        sh.links = sh
            .links
            .drain()
            .filter_map(|((r, c), t)| {
                let k = if rows { map(r).map(|nr| (nr, c)) } else { map(c).map(|nc| (r, nc)) };
                k.map(|k| (k, t))
            })
            .collect();
        sh.notes = sh
            .notes
            .drain()
            .filter_map(|((r, c), t)| {
                let k = if rows { map(r).map(|nr| (nr, c)) } else { map(c).map(|nc| (r, nc)) };
                k.map(|k| (k, t))
            })
            .collect();
        let fix = |rg: Range| -> Option<Range> {
            let (a, b) = if rows { (rg.r1, rg.r2) } else { (rg.c1, rg.c2) };
            let (na, nb) = if n > 0 {
                (if a >= at { a + n as u32 } else { a }, if b >= at { (b as i64 + n).min(max - 1) as u32 } else { b })
            } else {
                let end = at as i64 - n;
                let na = if (a as i64) >= end { (a as i64 + n) as u32 } else if a >= at { at } else { a };
                let nb = if (b as i64) >= end { (b as i64 + n) as u32 } else if b >= at { at.checked_sub(1)? } else { b };
                if nb < na {
                    return None;
                }
                (na, nb)
            };
            Some(if rows { Range { r1: na, r2: nb, ..rg } } else { Range { c1: na, c2: nb, ..rg } })
        };
        sh.merges = sh.merges.iter().filter_map(|m| fix(*m)).filter(|m| !m.is_single()).collect();
        if let Some(f) = &mut sh.filter {
            match fix(f.range) {
                Some(r) => f.range = r,
                None => sh.filter = None,
            }
        }
    }

    pub fn insert_rows(&mut self) {
        self.last_action = Some(Repeat::InsertRows);
        let sel = self.sel();
        self.structure(true, sel.r1, sel.rows() as i64);
    }
    pub fn insert_cols(&mut self) {
        self.last_action = Some(Repeat::InsertCols);
        let sel = self.sel();
        self.structure(false, sel.c1, sel.cols() as i64);
    }
    pub fn delete_rows(&mut self) {
        self.last_action = Some(Repeat::DeleteRows);
        let sel = self.sel();
        self.structure(true, sel.r1, -(sel.rows() as i64));
    }
    pub fn delete_cols(&mut self) {
        self.last_action = Some(Repeat::DeleteCols);
        let sel = self.sel();
        self.structure(false, sel.c1, -(sel.cols() as i64));
    }

    fn structure(&mut self, rows: bool, at: u32, n: i64) {
        self.commit_edit(false);
        if n > 0 {
            let limit = if rows { MAX_ROWS } else { MAX_COLS } as i64;
            let last = match self.sheet().extent() {
                Some((er, ec)) => (if rows { er } else { ec }) as i64,
                None => -1,
            };
            if last >= at as i64 && last + n >= limit {
                self.dialog = Some(Dialog::Message {
                    title: "Insert".into(),
                    text: "To prevent possible loss of data, FreedomOffice Sheets can't shift nonblank cells off of the worksheet.\n\nSelect fewer rows or columns to insert, or delete data at the end of the sheet.".into(),
                });
                return;
            }
        }
        self.push_undo(&[Scope::All]);
        let si = self.wb.active;
        Self::shift_sheet(&mut self.wb.sheets[si], rows, at, n);
        crate::condfmt::shift_sheet_rules(&mut self.wb.sheets[si], rows, at, n);
        let name = self.wb.sheets[si].name.clone();
        self.transform_formulas(|e, fsi| formula::shift_for_structure(e, &name, fsi == si, rows, at, n));
        crate::chart::shift_for_structure(&mut self.wb, si, rows, at, n);
        self.clip = None;
        self.changed();
    }

    /// Insert cells shifting right/down within the selection's band.
    pub fn insert_cells(&mut self, shift_down: bool) {
        let sel = self.sel();
        if sel.is_full_rows() {
            return self.insert_rows();
        }
        if sel.is_full_cols() {
            return self.insert_cols();
        }
        self.push_undo(&[Scope::All]);
        let si = self.wb.active;
        let (er, ec) = self.sheet().extent().unwrap_or((0, 0));
        let block = if shift_down { Range::new(sel.r1, sel.c1, er.max(sel.r1), sel.c2) } else { Range::new(sel.r1, sel.c1, sel.r2, ec.max(sel.c1)) };
        let (dr, dc) = if shift_down { (sel.rows() as i64, 0) } else { (0, sel.cols() as i64) };
        self.move_block(si, block, dr, dc);
        self.changed();
    }

    pub fn delete_cells(&mut self, shift_up: bool) {
        let sel = self.sel();
        if sel.is_full_rows() {
            return self.delete_rows();
        }
        if sel.is_full_cols() {
            return self.delete_cols();
        }
        self.push_undo(&[Scope::All]);
        let si = self.wb.active;
        // clear the selection, then pull the rest of the band in
        let keys: Vec<(u32, u32)> = self.wb.sheets[si].iter_range(sel).map(|(r, c, _)| (r, c)).collect();
        for (r, c) in keys {
            self.wb.sheets[si].remove(r, c);
        }
        let (er, ec) = self.sheet().extent().unwrap_or((0, 0));
        if shift_up {
            if er > sel.r2 {
                let block = Range::new(sel.r2 + 1, sel.c1, er, sel.c2);
                self.move_block(si, block, -(sel.rows() as i64), 0);
            }
        } else if ec > sel.c2 {
            let block = Range::new(sel.r1, sel.c2 + 1, sel.r2, ec);
            self.move_block(si, block, 0, -(sel.cols() as i64));
        }
        self.changed();
    }

    fn move_block(&mut self, si: usize, block: Range, dr: i64, dc: i64) {
        let mut taken = Vec::new();
        let keys: Vec<(u32, u32)> = self.wb.sheets[si].iter_range(block).map(|(r, c, _)| (r, c)).collect();
        for (r, c) in keys {
            if let Some(cell) = self.wb.sheets[si].remove(r, c) {
                taken.push((r, c, cell));
            }
        }
        let name = self.wb.sheets[si].name.clone();
        self.transform_formulas(|e, fsi| formula::move_refs(e, &name, fsi == si, block, dr, dc));
        for (r, c, mut cell) in taken {
            if let Some(f) = &mut cell.formula {
                if let Some(ast) = &mut f.ast {
                    formula::move_refs(ast, &name, true, block, dr, dc);
                    f.text = formula::print(ast);
                }
            }
            let nr = r as i64 + dr;
            let nc = c as i64 + dc;
            if nr >= 0 && nc >= 0 && nr < MAX_ROWS as i64 && nc < MAX_COLS as i64 {
                self.wb.sheets[si].set(nr as u32, nc as u32, Some(cell));
            }
        }
    }

    pub fn insert_cells_prompt(&mut self) {
        let sel = self.sel();
        if sel.is_full_rows() {
            self.insert_rows();
        } else if sel.is_full_cols() {
            self.insert_cols();
        } else {
            self.dialog = Some(Dialog::InsertCells { delete: false, choice: 1 });
        }
    }

    pub fn delete_cells_prompt(&mut self) {
        let sel = self.sel();
        if sel.is_full_rows() {
            self.delete_rows();
        } else if sel.is_full_cols() {
            self.delete_cols();
        } else {
            self.dialog = Some(Dialog::InsertCells { delete: true, choice: 1 });
        }
    }

    pub fn hide_rows(&mut self, hide: bool) {
        let sel = self.sel_clipped();
        self.push_undo(&[Scope::Sheet(self.wb.active)]);
        let sel_full = self.sel();
        let sh = self.sheet_mut();
        for r in sel_full.r1..=sel_full.r2.min(sel.r2.max(sel_full.r1) + 1000) {
            if hide { sh.hidden_rows.insert(r) } else { sh.hidden_rows.remove(&r) };
        }
        self.changed();
    }

    pub fn hide_cols(&mut self, hide: bool) {
        self.push_undo(&[Scope::Sheet(self.wb.active)]);
        let sel = self.sel();
        let sh = self.sheet_mut();
        for c in sel.c1..=sel.c2.min(sel.c1 + 16_383) {
            if hide { sh.hidden_cols.insert(c) } else { sh.hidden_cols.remove(&c) };
        }
        self.dirty = true;
    }

    pub fn set_col_width(&mut self, px: f32) {
        self.push_undo(&[Scope::Sheet(self.wb.active)]);
        let sel = self.sel();
        let sh = self.sheet_mut();
        for c in sel.c1..=sel.c2 {
            sh.col_w.insert(c, px.max(0.0));
            if px <= 0.0 {
                sh.hidden_cols.insert(c);
            } else {
                sh.hidden_cols.remove(&c);
            }
        }
        self.dirty = true;
    }

    pub fn set_row_height(&mut self, px: f32) {
        self.push_undo(&[Scope::Sheet(self.wb.active)]);
        let sel = self.sel();
        let (er, _) = self.sheet().extent().unwrap_or((0, 0));
        let r2 = if sel.is_full_cols() { er.max(sel.r1) } else { sel.r2 };
        let sh = self.sheet_mut();
        for r in sel.r1..=r2 {
            sh.row_h.insert(r, px.max(0.0));
            if px <= 0.0 {
                sh.hidden_rows.insert(r);
            } else {
                sh.hidden_rows.remove(&r);
            }
        }
        self.changed();
    }

    fn text_width(&mut self, ctx: &egui::Context, cell: &Cell) -> f32 {
        let style = self.wb.style(cell.style).clone();
        let s = super::grid::shown(&cell.value, &style, cell.formula.as_ref().map(|f| f.text.as_str()), self.show_formulas);
        if s.text.is_empty() {
            return 0.0;
        }
        let z = self.zoom;
        self.zoom = 1.0;
        let font = self.cell_font(&style);
        self.zoom = z;
        let w = ctx.fonts_mut(|f| f.layout_no_wrap(s.text.lines().map(|l| l.to_string()).max_by_key(|l| l.len()).unwrap_or_default(), font, egui::Color32::BLACK).size().x);
        w + style.indent as f32 * 9.0
    }

    pub fn autofit_cols(&mut self, cols: &[u32]) {
        let Some(ctx) = self.ctx.clone() else { return };
        self.push_undo(&[Scope::Sheet(self.wb.active)]);
        for &c in cols {
            let cells: Vec<Cell> = self
                .sheet()
                .rows
                .iter()
                .filter(|(r, _)| !self.sheet().hidden_rows.contains(r))
                .filter_map(|(_, row)| row.get(&c).cloned())
                .collect();
            let mut w: f32 = 0.0;
            for cell in &cells {
                w = w.max(self.text_width(&ctx, cell));
            }
            let sh = self.sheet_mut();
            if w > 0.0 {
                sh.col_w.insert(c, (w + 8.0).ceil());
            } else {
                sh.col_w.remove(&c);
            }
            sh.hidden_cols.remove(&c);
        }
        self.dirty = true;
    }

    pub fn autofit_rows(&mut self, rows: &[u32]) {
        self.push_undo(&[Scope::Sheet(self.wb.active)]);
        self.fit_rows(rows);
    }

    /// Sets row heights to fit their content (wrapped text and line breaks included).
    pub fn fit_rows(&mut self, rows: &[u32]) {
        let ctx = self.ctx.clone();
        for &r in rows {
            let mut h: f32 = crate::model::DEFAULT_ROW_H;
            let cells: Vec<(u32, Cell)> = self.sheet().rows.get(&r).map(|row| row.iter().map(|(c, x)| (*c, x.clone())).collect()).unwrap_or_default();
            for (c, cell) in cells {
                if self.sheet().merge_at(r, c).is_some_and(|m| m.rows() > 1) {
                    continue;
                }
                let st = self.wb.style(cell.style).clone();
                let line_h = st.font_size() * 96.0 / 72.0 * 1.25;
                let mut lines = match &cell.value {
                    Value::Text(t) => t.lines().count().max(1),
                    _ => 1,
                } as f32;
                if st.wrap {
                    if let (Some(ctx), Value::Text(t)) = (&ctx, &cell.value) {
                        let z = self.zoom;
                        self.zoom = 1.0;
                        let font = self.cell_font(&st);
                        self.zoom = z;
                        let width = (self.sheet().col_width(c) - 6.0).max(4.0);
                        let mut job = egui::text::LayoutJob::single_section(
                            t.clone(),
                            egui::TextFormat { font_id: font, ..Default::default() },
                        );
                        job.wrap.max_width = width;
                        let rows_n = ctx.fonts_mut(|f| f.layout_job(job).rows.len()).max(1) as f32;
                        lines = lines.max(rows_n);
                    }
                }
                h = h.max(line_h * lines + 3.0);
            }
            let sh = self.sheet_mut();
            if (h - crate::model::DEFAULT_ROW_H).abs() < 0.5 {
                sh.row_h.remove(&r);
            } else {
                sh.row_h.insert(r, h.ceil());
            }
            sh.hidden_rows.remove(&r);
        }
        self.dirty = true;
    }

    pub fn autofit_selection_cols(&mut self) {
        let sel = self.sel_clipped();
        let cols: Vec<u32> = (sel.c1..=sel.c2).collect();
        self.autofit_cols(&cols);
    }

    pub fn autofit_selection_rows(&mut self) {
        let sel = self.sel_clipped();
        let rows: Vec<u32> = (sel.r1..=sel.r2).collect();
        self.autofit_rows(&rows);
    }

    // ------------------------------------------------------------------
    // Formatting
    // ------------------------------------------------------------------

    /// Applies a style change to every cell of the selection.
    pub fn apply_style(&mut self, f: impl Fn(&mut Style) + 'static) {
        self.apply_style_rc(std::rc::Rc::new(f));
    }

    pub fn apply_style_rc(&mut self, f: std::rc::Rc<dyn Fn(&mut Style)>) {
        self.commit_edit(false);
        self.last_action = Some(Repeat::Style(f.clone()));
        let si = self.wb.active;
        let areas = self.areas();
        let scopes: Vec<Scope> = areas.iter().map(|a| Scope::Range(si, self.clip_area(*a))).collect();
        self.push_undo(&scopes);
        let mut coords: Vec<(u32, u32)> = Vec::new();
        for a in areas {
            let clipped = self.clip_area(a);
            let dense = clipped.rows() as u64 * clipped.cols() as u64 <= 250_000;
            if dense {
                coords.extend((clipped.r1..=clipped.r2).flat_map(|r| (clipped.c1..=clipped.c2).map(move |c| (r, c))));
            } else {
                coords.extend(self.wb.sheets[si].iter_range(a).map(|(r, c, _)| (r, c)));
            }
        }
        coords.sort_unstable();
        coords.dedup();
        let mut cache: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        for (r, c) in coords {
            let old = self.wb.sheets[si].get(r, c).map(|c| c.style).unwrap_or(0);
            let new = match cache.get(&old) {
                Some(n) => *n,
                None => {
                    let mut s = self.wb.style(old).clone();
                    f(&mut s);
                    let n = self.wb.intern_style(s);
                    cache.insert(old, n);
                    n
                }
            };
            if new != old {
                self.wb.sheets[si].cell_mut(r, c).style = new;
                self.wb.sheets[si].tidy(r, c);
            }
        }
        self.dirty = true;
    }

    pub fn active_style(&self) -> Style {
        let (r, c) = self.active();
        self.sheet().get(r, c).map(|c| self.wb.style(c.style).clone()).unwrap_or_default()
    }

    pub fn toggle_bold(&mut self) {
        let v = !self.active_style().bold;
        self.apply_style(move |s| s.bold = v);
    }
    pub fn toggle_italic(&mut self) {
        let v = !self.active_style().italic;
        self.apply_style(move |s| s.italic = v);
    }
    pub fn toggle_underline(&mut self) {
        let v = !self.active_style().underline;
        self.apply_style(move |s| s.underline = v);
    }
    pub fn toggle_strike(&mut self) {
        let v = !self.active_style().strike;
        self.apply_style(move |s| s.strike = v);
    }
    pub fn toggle_wrap(&mut self) {
        let v = !self.active_style().wrap;
        self.apply_style(move |s| s.wrap = v);
        // Like the desktop app, rows grow (or shrink back) to fit wrapped text.
        let sel = self.sel_clipped();
        if sel.rows() <= 5000 {
            let rows: Vec<u32> = (sel.r1..=sel.r2).collect();
            self.fit_rows(&rows);
        }
    }
    pub fn set_halign(&mut self, a: HAlign) {
        let cur = self.active_style().halign;
        let a = if cur == a { HAlign::General } else { a };
        self.apply_style(move |s| s.halign = a);
    }
    pub fn set_valign(&mut self, a: VAlign) {
        self.apply_style(move |s| s.valign = a);
    }
    pub fn set_num_fmt(&mut self, f: &str) {
        let f = f.to_string();
        self.apply_style(move |s| s.num_fmt = if f == "General" { String::new() } else { f.clone() });
    }
    pub fn grow_font(&mut self, up: bool) {
        const SIZES: [f32; 16] = [8.0, 9.0, 10.0, 11.0, 12.0, 14.0, 16.0, 18.0, 20.0, 22.0, 24.0, 26.0, 28.0, 36.0, 48.0, 72.0];
        let cur = self.active_style().font_size();
        let next = if up {
            SIZES.iter().copied().find(|s| *s > cur).unwrap_or(cur + 10.0)
        } else {
            SIZES.iter().rev().copied().find(|s| *s < cur).unwrap_or((cur - 1.0).max(1.0))
        };
        self.apply_style(move |s| s.set_font_size(next));
    }
    pub fn indent(&mut self, more: bool) {
        self.apply_style(move |s| {
            s.indent = if more { (s.indent + 1).min(15) } else { s.indent.saturating_sub(1) };
            if s.halign == HAlign::General || s.halign == HAlign::Center {
                s.halign = HAlign::Left;
            }
        });
    }

    pub fn change_decimals(&mut self, delta: i32) {
        let (r, c) = self.active();
        let st = self.active_style();
        let sample = self.sheet().value(r, c).clone();
        let fmt = adjust_decimals(&st.num_fmt, delta, &sample);
        self.apply_style(move |s| {
            s.num_fmt = if s.num_fmt == st.num_fmt { fmt.clone() } else { adjust_decimals(&s.num_fmt, delta, &Value::Empty) };
        });
    }

    pub fn borders(&mut self, kind: BorderKind) {
        let sel = self.sel_clipped();
        self.commit_edit(false);
        let si = self.wb.active;
        self.push_undo(&[Scope::Range(si, sel)]);
        if sel.rows() as u64 * sel.cols() as u64 > 250_000 {
            return;
        }
        for r in sel.r1..=sel.r2 {
            for c in sel.c1..=sel.c2 {
                let old = self.wb.sheets[si].get(r, c).map(|c| c.style).unwrap_or(0);
                let mut s = self.wb.style(old).clone();
                let b = &mut s.border;
                let (top, bottom, left, right) = (r == sel.r1, r == sel.r2, c == sel.c1, c == sel.c2);
                match kind {
                    BorderKind::None => *b = Border::default(),
                    BorderKind::All => {
                        *b = Border { left: BorderLine::Thin, right: BorderLine::Thin, top: BorderLine::Thin, bottom: BorderLine::Thin }
                    }
                    BorderKind::Outside | BorderKind::ThickOutside => {
                        let l = if kind == BorderKind::Outside { BorderLine::Thin } else { BorderLine::Medium };
                        if top {
                            b.top = l;
                        }
                        if bottom {
                            b.bottom = l;
                        }
                        if left {
                            b.left = l;
                        }
                        if right {
                            b.right = l;
                        }
                    }
                    BorderKind::Bottom if bottom => b.bottom = BorderLine::Thin,
                    BorderKind::Top if top => b.top = BorderLine::Thin,
                    BorderKind::Left if left => b.left = BorderLine::Thin,
                    BorderKind::Right if right => b.right = BorderLine::Thin,
                    BorderKind::DoubleBottom if bottom => b.bottom = BorderLine::Double,
                    BorderKind::ThickBottom if bottom => b.bottom = BorderLine::Medium,
                    _ => {}
                }
                let n = self.wb.intern_style(s);
                self.wb.sheets[si].cell_mut(r, c).style = n;
                self.wb.sheets[si].tidy(r, c);
            }
        }
        self.dirty = true;
    }

    pub fn merge(&mut self, mode: u8) {
        // 0 = merge & center, 1 = merge across, 2 = merge cells, 3 = unmerge
        let sel = self.sel();
        if sel.is_full_cols() || sel.is_full_rows() {
            return;
        }
        let si = self.wb.active;
        self.push_undo(&[Scope::Sheet(si)]);
        let had = self.sheet().merges.iter().any(|m| m.intersects(&sel));
        self.sheet_mut().merges.retain(|m| !m.intersects(&sel));
        if mode == 3 || (mode == 0 && had) {
            self.dirty = true;
            return;
        }
        let ranges: Vec<Range> = if mode == 1 { (sel.r1..=sel.r2).map(|r| Range::new(r, sel.c1, r, sel.c2)).collect() } else { vec![sel] };
        let mut lost = false;
        for m in ranges {
            if m.is_single() {
                continue;
            }
            let keys: Vec<(u32, u32)> = self.wb.sheets[si].iter_range(m).map(|(r, c, _)| (r, c)).collect();
            for (r, c) in keys {
                if (r, c) != (m.r1, m.c1) {
                    if let Some(cell) = self.wb.sheets[si].get_mut(r, c) {
                        if !cell.value.is_empty() || cell.formula.is_some() {
                            lost = true;
                        }
                        cell.value = Value::Empty;
                        cell.formula = None;
                    }
                    self.wb.sheets[si].tidy(r, c);
                }
            }
            self.wb.sheets[si].merges.push(m);
        }
        if mode == 0 {
            let old = self.wb.sheets[si].get(sel.r1, sel.c1).map(|c| c.style).unwrap_or(0);
            let mut s = self.wb.style(old).clone();
            s.halign = HAlign::Center;
            let n = self.wb.intern_style(s);
            self.wb.sheets[si].cell_mut(sel.r1, sel.c1).style = n;
        }
        if lost {
            self.status = Some("Merging cells only keeps the upper-left value and discards other values.".into());
        }
        self.select(sel.r1, sel.c1, false);
        self.changed();
    }

    pub fn start_format_painter(&mut self) {
        let sel = self.sel_clipped();
        let mut styles = Vec::new();
        for r in sel.r1..=sel.r2.min(sel.r1 + 200) {
            for c in sel.c1..=sel.c2.min(sel.c1 + 200) {
                let st = self.sheet().get(r, c).map(|x| self.wb.style(x.style).clone()).unwrap_or_default();
                styles.push((r - sel.r1, c - sel.c1, st));
            }
        }
        self.format_painter = Some((styles, sel.rows().min(201), sel.cols().min(201), false));
    }

    pub fn apply_format_painter(&mut self) {
        let Some((styles, h, w, sticky)) = self.format_painter.clone() else { return };
        let sel = self.sel_clipped();
        let si = self.wb.active;
        let target = if sel.is_single() {
            Range::new(sel.r1, sel.c1, (sel.r1 + h - 1).min(MAX_ROWS - 1), (sel.c1 + w - 1).min(MAX_COLS - 1))
        } else {
            sel
        };
        self.push_undo(&[Scope::Range(si, target)]);
        for r in target.r1..=target.r2 {
            for c in target.c1..=target.c2 {
                let (dr, dc) = ((r - target.r1) % h, (c - target.c1) % w);
                if let Some((_, _, st)) = styles.iter().find(|(a, b, _)| *a == dr && *b == dc) {
                    let id = self.wb.intern_style(st.clone());
                    self.wb.sheets[si].cell_mut(r, c).style = id;
                    self.wb.sheets[si].tidy(r, c);
                }
            }
        }
        if !sticky {
            self.format_painter = None;
        }
        self.select_range(target);
        self.dirty = true;
    }

    // ------------------------------------------------------------------
    // Fill
    // ------------------------------------------------------------------

    pub fn fill_down(&mut self) {
        let sel = self.sel_clipped();
        let src_row = if sel.rows() == 1 { sel.r1.saturating_sub(1) } else { sel.r1 };
        if sel.rows() == 1 && sel.r1 == 0 {
            return;
        }
        let si = self.wb.active;
        self.push_undo(&[Scope::Range(si, sel)]);
        for c in sel.c1..=sel.c2 {
            let src = self.wb.sheets[si].get(src_row, c).cloned();
            for r in sel.r1..=sel.r2 {
                if r == src_row {
                    continue;
                }
                let mut cell = src.clone();
                if let Some(cell) = &mut cell {
                    shift_cell(cell, r as i64 - src_row as i64, 0);
                }
                self.wb.sheets[si].set(r, c, cell);
            }
        }
        self.changed();
    }

    pub fn fill_right(&mut self) {
        let sel = self.sel_clipped();
        let src_col = if sel.cols() == 1 { sel.c1.saturating_sub(1) } else { sel.c1 };
        if sel.cols() == 1 && sel.c1 == 0 {
            return;
        }
        let si = self.wb.active;
        self.push_undo(&[Scope::Range(si, sel)]);
        for r in sel.r1..=sel.r2 {
            let src = self.wb.sheets[si].get(r, src_col).cloned();
            for c in sel.c1..=sel.c2 {
                if c == src_col {
                    continue;
                }
                let mut cell = src.clone();
                if let Some(cell) = &mut cell {
                    shift_cell(cell, 0, c as i64 - src_col as i64);
                }
                self.wb.sheets[si].set(r, c, cell);
            }
        }
        self.changed();
    }

    /// Double-click on the fill handle: fill down as far as the neighbouring column has data.
    pub fn fill_down_to_data(&mut self) {
        let sel = self.sel();
        let sh = self.sheet();
        let probe = |c: u32| -> u32 {
            let mut r = sel.r2;
            while r + 1 < MAX_ROWS && sh.is_populated(r + 1, c) {
                r += 1;
            }
            r
        };
        let mut last = sel.r2;
        if sel.c1 > 0 {
            last = last.max(probe(sel.c1 - 1));
        }
        if last == sel.r2 && sel.c2 + 1 < MAX_COLS {
            last = last.max(probe(sel.c2 + 1));
        }
        if last > sel.r2 {
            self.autofill(sel, Range::new(sel.r1, sel.c1, last, sel.c2));
        }
    }

    pub fn autofill(&mut self, src: Range, target: Range) {
        let si = self.wb.active;
        self.push_undo(&[Scope::Range(si, target)]);
        let vertical = target.rows() != src.rows();
        let (lines, len) = if vertical { (src.c1..=src.c2, src.rows()) } else { (src.r1..=src.r2, src.cols()) };
        let forward = if vertical { target.r2 > src.r2 } else { target.c2 > src.c2 };
        let count = if vertical { target.rows() - src.rows() } else { target.cols() - src.cols() };
        for line in lines {
            let seq: Vec<Option<Cell>> = (0..len)
                .map(|k| {
                    let (r, c) = if vertical { (src.r1 + k, line) } else { (line, src.c1 + k) };
                    self.wb.sheets[si].get(r, c).cloned()
                })
                .collect();
            let series = Series::detect(&seq, &self.wb);
            for k in 0..count {
                let (pos, step_index) = if forward { (len + k, len as i64 + k as i64) } else { (0, -(k as i64) - 1) };
                let _ = pos;
                let src_idx = (step_index.rem_euclid(len as i64)) as u32;
                let (r, c) = if vertical {
                    let r = if forward { src.r2 + 1 + k } else { src.r1 - 1 - k };
                    (r, line)
                } else {
                    let c = if forward { src.c2 + 1 + k } else { src.c1 - 1 - k };
                    (line, c)
                };
                let (sr, sc) = if vertical { (src.r1 + src_idx, line) } else { (line, src.c1 + src_idx) };
                let mut cell = seq[src_idx as usize].clone();
                if let Some(cell) = &mut cell {
                    if cell.formula.is_some() {
                        shift_cell(cell, r as i64 - sr as i64, c as i64 - sc as i64);
                    } else if let Some(v) = series.value(step_index, src_idx as usize) {
                        cell.value = v;
                    }
                }
                self.wb.sheets[si].set(r, c, cell);
            }
        }
        self.select_range(target);
        self.changed();
    }

    // ------------------------------------------------------------------
    // AutoSum and functions
    // ------------------------------------------------------------------

    pub fn autosum(&mut self, func: &str) {
        self.commit_edit(false);
        let sel = self.sel();
        let sh = self.sheet();
        let is_num = |r: u32, c: u32| matches!(sh.value(r, c), Value::Number(_)) || sh.get(r, c).is_some_and(|c| c.formula.is_some());
        if sel.is_single() {
            let (r, c) = self.active();
            // numbers above?
            let mut r1 = r;
            while r1 > 0 && is_num(r1 - 1, c) {
                r1 -= 1;
            }
            let range = if r1 < r {
                Some(Range::new(r1, c, r - 1, c))
            } else {
                let mut c1 = c;
                while c1 > 0 && is_num(r, c1 - 1) {
                    c1 -= 1;
                }
                if c1 < c { Some(Range::new(r, c1, r, c - 1)) } else { None }
            };
            let mut e = Editor::new(self.wb.active, r, c, format!("={func}("), EditMode::Enter);
            if let Some(rg) = range {
                e.set_point((rg.r1, rg.c1), (rg.r2, rg.c2), None);
            }
            let end = e.text.len();
            e.text.push(')');
            e.cursor = end;
            e.anchor = end;
            self.edit = Some(e);
            return;
        }
        // Multi-cell: totals under each column of the selection.
        let si = self.wb.active;
        let last_empty = (sel.c1..=sel.c2).all(|c| !self.sheet().is_populated(sel.r2, c));
        let (data_end, out_row) = if last_empty && sel.rows() > 1 { (sel.r2 - 1, sel.r2) } else { (sel.r2, sel.r2 + 1) };
        self.push_undo(&[Scope::Range(si, Range::new(out_row, sel.c1, out_row, sel.c2))]);
        for c in sel.c1..=sel.c2 {
            let f = format!("={func}({})", Range::new(sel.r1, c, data_end, c).to_a1());
            let style = self.wb.sheets[si].get(out_row, c).map(|x| x.style).unwrap_or(0);
            let cell = cell_from_input(&mut self.wb, &f, style);
            self.wb.sheets[si].set(out_row, c, cell);
        }
        self.changed();
    }

    pub fn insert_function_text(&mut self, name: &str) {
        match &mut self.edit {
            Some(e) => {
                if e.text.is_empty() {
                    e.insert("=");
                }
                e.insert(&format!("{name}("));
            }
            None => {
                self.begin_edit(Some(format!("={name}(")), EditMode::Edit);
            }
        }
        if let Some(e) = &mut self.edit {
            e.ac_hidden = true;
        }
    }

    pub fn insert_now(&mut self, time: bool) {
        let now = numfmt::now_serial();
        let (v, fmt) = if time {
            let t = now - now.floor();
            (numfmt::format_value(&Value::Number(t), "h:mm AM/PM").text, ())
        } else {
            (numfmt::format_value(&Value::Number(now.floor()), "m/d/yyyy").text, ())
        };
        let _ = fmt;
        match &mut self.edit {
            Some(e) => e.insert(&v),
            None => {
                self.begin_edit(Some(v), EditMode::Enter);
            }
        }
    }

    // ------------------------------------------------------------------
    // Sort & filter
    // ------------------------------------------------------------------

    fn sort_area(&self) -> (Range, bool) {
        let sel = self.sel();
        let rg = if sel.is_single() { self.current_region() } else { self.sel_clipped() };
        // header heuristic: first row all text, second row has non-text
        let sh = self.sheet();
        let first_text = (rg.c1..=rg.c2).all(|c| matches!(sh.value(rg.r1, c), Value::Text(_) | Value::Empty))
            && (rg.c1..=rg.c2).any(|c| matches!(sh.value(rg.r1, c), Value::Text(_)));
        let second_other = rg.r2 > rg.r1 && (rg.c1..=rg.c2).any(|c| !matches!(sh.value(rg.r1 + 1, c), Value::Text(_) | Value::Empty));
        let header = (first_text && second_other) || sh.filter.as_ref().is_some_and(|f| f.range.r1 == rg.r1);
        (rg, header)
    }

    pub fn sort_simple(&mut self, desc: bool) {
        let (rg, header) = self.sort_area();
        let c = self.active().1.clamp(rg.c1, rg.c2);
        self.sort_range(rg, header, &[(c, desc)]);
    }

    pub fn open_sort_dialog(&mut self) {
        let (rg, header) = self.sort_area();
        let c = self.active().1.clamp(rg.c1, rg.c2);
        self.dialog = Some(Dialog::Sort { range: rg, header, keys: vec![(c, false)] });
    }

    pub fn sort_range(&mut self, rg: Range, header: bool, keys: &[(u32, bool)]) {
        let si = self.wb.active;
        let r1 = if header { rg.r1 + 1 } else { rg.r1 };
        if r1 >= rg.r2 + 1 {
            return;
        }
        let data = Range::new(r1, rg.c1, rg.r2, rg.c2);
        self.push_undo(&[Scope::Range(si, data)]);
        let sh = &self.wb.sheets[si];
        let mut rows: Vec<(u32, Vec<(u32, Cell)>)> = (data.r1..=data.r2)
            .map(|r| (r, (data.c1..=data.c2).filter_map(|c| sh.get(r, c).map(|x| (c, x.clone()))).collect()))
            .collect();
        let key_val = |row: &Vec<(u32, Cell)>, c: u32| -> Value { row.iter().find(|(cc, _)| *cc == c).map(|(_, x)| x.value.clone()).unwrap_or_default() };
        rows.sort_by(|a, b| {
            for &(c, desc) in keys {
                let (va, vb) = (key_val(&a.1, c), key_val(&b.1, c));
                let o = sort_cmp(&va, &vb, desc);
                if o != Ordering::Equal {
                    return o;
                }
            }
            Ordering::Equal
        });
        let sh = &mut self.wb.sheets[si];
        let old: Vec<(u32, u32)> = sh.iter_range(data).map(|(r, c, _)| (r, c)).collect();
        for (r, c) in old {
            sh.remove(r, c);
        }
        let mut moved: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        for (i, (orig, cells)) in rows.into_iter().enumerate() {
            let nr = data.r1 + i as u32;
            moved.insert(orig, nr);
            for (c, mut cell) in cells {
                shift_cell(&mut cell, nr as i64 - orig as i64, 0);
                sh.set(nr, c, Some(cell));
            }
        }
        let remap = |m: &mut std::collections::HashMap<(u32, u32), String>| {
            *m = m
                .drain()
                .map(|((r, c), t)| if data.contains(r, c) { ((moved.get(&r).copied().unwrap_or(r), c), t) } else { ((r, c), t) })
                .collect();
        };
        remap(&mut sh.notes);
        remap(&mut sh.links);
        self.changed();
    }

    pub fn toggle_filter(&mut self) {
        let si = self.wb.active;
        self.push_undo(&[Scope::Sheet(si)]);
        if self.sheet().filter.is_some() {
            let sh = self.sheet_mut();
            if let Some(f) = sh.filter.take() {
                for r in f.range.r1..=f.range.r2 {
                    sh.hidden_rows.remove(&r);
                }
            }
        } else {
            let rg = if self.sel().is_single() { self.current_region() } else { self.sel_clipped() };
            self.sheet_mut().filter = Some(crate::model::AutoFilter { range: rg, excluded: Default::default() });
        }
        self.changed();
    }

    pub fn apply_filter(&mut self) {
        let si = self.wb.active;
        let Some(mut f) = self.wb.sheets[si].filter.clone() else { return };
        // extend to data below
        let (er, _) = self.wb.sheets[si].extent().unwrap_or((0, 0));
        let sh = &self.wb.sheets[si];
        let mut r2 = f.range.r2;
        while r2 < er && (f.range.c1..=f.range.c2).any(|c| sh.is_populated(r2 + 1, c)) {
            r2 += 1;
        }
        f.range.r2 = r2;
        let mut hide = Vec::new();
        let mut show = Vec::new();
        for r in f.range.r1 + 1..=f.range.r2 {
            let mut visible = true;
            for (c, ex) in &f.excluded {
                if ex.is_empty() {
                    continue;
                }
                let t = sh.get(r, *c).map(|x| numfmt::format_value(&x.value, &self.wb.style(x.style).num_fmt).text).unwrap_or_default();
                if ex.contains(&t) {
                    visible = false;
                    break;
                }
            }
            if visible { show.push(r) } else { hide.push(r) }
        }
        let sh = &mut self.wb.sheets[si];
        for r in show {
            sh.hidden_rows.remove(&r);
        }
        for r in hide {
            sh.hidden_rows.insert(r);
        }
        sh.filter = Some(f);
        // SUBTOTAL(101..)/AGGREGATE ignore hidden rows, so totals change with the filter.
        self.changed();
    }

    pub fn clear_filter(&mut self) {
        let si = self.wb.active;
        if let Some(f) = &mut self.wb.sheets[si].filter {
            f.excluded.clear();
        }
        self.apply_filter();
    }

    pub fn filter_by_active_value(&mut self) {
        if self.sheet().filter.is_none() {
            self.toggle_filter();
        }
        let (r, c) = self.active();
        let want = self.sheet().get(r, c).map(|x| numfmt::format_value(&x.value, &self.wb.style(x.style).num_fmt).text).unwrap_or_default();
        let si = self.wb.active;
        let Some(f) = self.wb.sheets[si].filter.clone() else { return };
        let mut ex = HashSet::new();
        for rr in f.range.r1 + 1..=f.range.r2 {
            let t = self.wb.sheets[si].get(rr, c).map(|x| numfmt::format_value(&x.value, &self.wb.style(x.style).num_fmt).text).unwrap_or_default();
            if t != want {
                ex.insert(t);
            }
        }
        if let Some(f) = &mut self.wb.sheets[si].filter {
            f.excluded.insert(c, ex);
        }
        self.apply_filter();
    }

    pub fn remove_duplicates(&mut self, rg: Range, header: bool, cols: &[u32]) -> usize {
        let si = self.wb.active;
        let r1 = if header { rg.r1 + 1 } else { rg.r1 };
        if r1 > rg.r2 {
            return 0;
        }
        let data = Range::new(r1, rg.c1, rg.r2, rg.c2);
        self.push_undo(&[Scope::Range(si, data)]);
        let sh = &self.wb.sheets[si];
        let mut seen = HashSet::new();
        let mut keep: Vec<Vec<(u32, Cell)>> = Vec::new();
        let mut removed = 0;
        for r in data.r1..=data.r2 {
            let key: Vec<String> = cols.iter().map(|c| numfmt::value_to_text(sh.value(r, *c)).to_lowercase()).collect();
            let cells: Vec<(u32, Cell)> = (data.c1..=data.c2).filter_map(|c| sh.get(r, c).map(|x| (c, x.clone()))).collect();
            if seen.insert(key) {
                keep.push(cells);
            } else {
                removed += 1;
            }
        }
        let sh = &mut self.wb.sheets[si];
        let old: Vec<(u32, u32)> = sh.iter_range(data).map(|(r, c, _)| (r, c)).collect();
        for (r, c) in old {
            sh.remove(r, c);
        }
        for (i, cells) in keep.into_iter().enumerate() {
            for (c, cell) in cells {
                sh.set(data.r1 + i as u32, c, Some(cell));
            }
        }
        self.changed();
        removed
    }

    pub fn text_to_columns(&mut self, delim: char) {
        let sel = self.sel_clipped();
        let si = self.wb.active;
        let c = sel.c1;
        let lines: Vec<(u32, String)> = (sel.r1..=sel.r2)
            .filter_map(|r| match self.wb.sheets[si].value(r, c) {
                Value::Text(t) => Some((r, t.clone())),
                _ => None,
            })
            .collect();
        let max_parts = lines.iter().map(|(_, t)| t.split(delim).count()).max().unwrap_or(1) as u32;
        self.push_undo(&[Scope::Range(si, Range::new(sel.r1, c, sel.r2, (c + max_parts - 1).min(MAX_COLS - 1)))]);
        for (r, t) in lines {
            for (i, part) in t.split(delim).enumerate() {
                let cc = c + i as u32;
                if cc >= MAX_COLS {
                    break;
                }
                let style = self.wb.sheets[si].get(r, cc).map(|x| x.style).unwrap_or(0);
                let cell = cell_from_input(&mut self.wb, part.trim(), style);
                self.wb.sheets[si].set(r, cc, cell);
            }
        }
        self.changed();
    }

    // ------------------------------------------------------------------
    // Find & replace
    // ------------------------------------------------------------------

    fn cell_matches(&self, si: usize, r: u32, c: u32) -> bool {
        let f = &self.find;
        if f.what.is_empty() {
            return false;
        }
        let Some(cell) = self.wb.sheets[si].get(r, c) else { return false };
        let hay = if f.in_formulas { cell.input_text() } else { numfmt::format_value(&cell.value, &self.wb.style(cell.style).num_fmt).text };
        let (h, w) = if f.match_case { (hay, f.what.clone()) } else { (hay.to_lowercase(), f.what.to_lowercase()) };
        if f.whole { h == w } else { h.contains(&w) }
    }

    pub fn find_next(&mut self, backwards: bool) -> bool {
        let si0 = self.wb.active;
        let order: Vec<usize> = if self.find.all_sheets {
            (0..self.wb.sheets.len()).map(|k| (si0 + k) % self.wb.sheets.len()).collect()
        } else {
            vec![si0]
        };
        let (ar, ac) = self.active();
        for (k, &si) in order.iter().enumerate() {
            let mut coords: Vec<(u32, u32)> =
                self.wb.sheets[si].rows.iter().flat_map(|(r, row)| row.keys().map(move |c| (*r, *c))).collect();
            if backwards {
                coords.reverse();
            }
            let start = if k == 0 {
                coords.iter().position(|&(r, c)| if backwards { (r, c) < (ar, ac) } else { (r, c) > (ar, ac) }).unwrap_or(coords.len())
            } else {
                0
            };
            let wrapped: Vec<(u32, u32)> = coords[start..].iter().chain(if order.len() == 1 { coords[..start].iter() } else { [].iter() }).copied().collect();
            for (r, c) in wrapped {
                if self.cell_matches(si, r, c) {
                    self.wb.active = si;
                    self.select(r, c, false);
                    return true;
                }
            }
        }
        self.status = Some("We couldn't find what you were looking for.".into());
        false
    }

    pub fn replace_one(&mut self) {
        let (r, c) = self.active();
        let si = self.wb.active;
        if self.cell_matches(si, r, c) {
            self.replace_at(si, r, c);
            self.changed();
        }
        self.find_next(false);
    }

    fn replace_at(&mut self, si: usize, r: u32, c: u32) {
        let Some(cell) = self.wb.sheets[si].get(r, c).cloned() else { return };
        let text = cell.input_text();
        let new = if self.find.match_case {
            text.replace(&self.find.what, &self.find.with)
        } else {
            replace_ci(&text, &self.find.what, &self.find.with)
        };
        let cell = cell_from_input(&mut self.wb, &new, cell.style);
        self.wb.sheets[si].set(r, c, cell);
    }

    pub fn replace_all(&mut self) -> usize {
        let sheets: Vec<usize> = if self.find.all_sheets { (0..self.wb.sheets.len()).collect() } else { vec![self.wb.active] };
        self.push_undo(&[Scope::All]);
        let mut n = 0;
        let in_formulas = self.find.in_formulas;
        self.find.in_formulas = true;
        for si in sheets {
            let coords: Vec<(u32, u32)> = self.wb.sheets[si].rows.iter().flat_map(|(r, row)| row.keys().map(move |c| (*r, *c))).collect();
            for (r, c) in coords {
                if self.cell_matches(si, r, c) {
                    self.replace_at(si, r, c);
                    n += 1;
                }
            }
        }
        self.find.in_formulas = in_formulas;
        self.changed();
        n
    }

    // ------------------------------------------------------------------
    // View
    // ------------------------------------------------------------------

    pub fn freeze(&mut self, mode: u8) {
        // 0 = at selection / unfreeze toggle, 1 = top row, 2 = first column
        let si = self.wb.active;
        self.push_undo(&[Scope::Sheet(si)]);
        let (r, c) = self.active();
        let top = self.sheet().view.top;
        let left = self.sheet().view.left;
        let sh = self.sheet_mut();
        sh.freeze = match mode {
            0 if sh.freeze != (0, 0) => (0, 0),
            0 => {
                let (fr, fc) = (r.saturating_sub(top) + top, c.saturating_sub(left) + left);
                (if fr == 0 { 0 } else { fr }, if fc == 0 { 0 } else { fc })
            }
            1 => (1, 0),
            2 => (0, 1),
            _ => (0, 0),
        };
        if mode == 1 || mode == 2 {
            sh.view.top = sh.view.top.max(sh.freeze.0);
            sh.view.left = sh.view.left.max(sh.freeze.1);
        }
        if mode == 0 && sh.freeze != (0, 0) {
            // keep the scrolled region starting at the split
            sh.view.top = sh.freeze.0;
            sh.view.left = sh.freeze.1;
            // rows above the original top are kept visible as frozen panes only from row 0
        }
        self.dirty = true;
    }

    // ------------------------------------------------------------------
    // Sheets
    // ------------------------------------------------------------------

    pub fn add_sheet(&mut self) {
        self.commit_edit(false);
        self.push_undo(&[Scope::All]);
        let name = self.wb.unique_sheet_name("Sheet");
        let at = self.wb.active + 1;
        self.wb.sheets.insert(at, Sheet::new(&name));
        self.wb.active = at;
        self.changed();
    }

    pub fn delete_sheet(&mut self, i: usize) {
        if self.wb.sheets.len() <= 1 {
            self.dialog = Some(Dialog::Message { title: "Delete".into(), text: "A workbook must contain at least one visible worksheet.".into() });
            return;
        }
        self.push_undo(&[Scope::All]);
        let name = self.wb.sheets[i].name.clone();
        self.wb.sheets.remove(i);
        self.transform_formulas(|e, _| {
            formula::map_refs(e, &mut |r| !r.sheet.as_ref().is_some_and(|s| s.eq_ignore_ascii_case(&name)));
        });
        if self.wb.active >= self.wb.sheets.len() || self.wb.active > i {
            self.wb.active = self.wb.active.saturating_sub(1).min(self.wb.sheets.len() - 1);
        }
        self.changed();
    }

    pub fn rename_sheet(&mut self, i: usize, new: &str) -> Result<(), String> {
        let new = new.trim();
        if new.is_empty() || new.len() > 31 || new.contains(['[', ']', ':', '*', '?', '/', '\\']) || new.starts_with('\'') || new.ends_with('\'') {
            return Err("That sheet name isn't valid. Names must be 1-31 characters, without : \\ / ? * [ ] or leading/trailing apostrophes.".into());
        }
        if let Some(j) = self.wb.sheet_index(new) {
            if j != i {
                return Err("That name is already taken. Try a different one.".into());
            }
        }
        let old = self.wb.sheets[i].name.clone();
        if old == new {
            return Ok(());
        }
        self.push_undo(&[Scope::All]);
        self.wb.sheets[i].name = new.to_string();
        let newn = new.to_string();
        self.transform_formulas(|e, _| formula::rename_sheet(e, &old, &newn));
        crate::chart::rename_sheet(&mut self.wb, &old, &newn);
        self.changed();
        Ok(())
    }

    pub fn move_sheet(&mut self, i: usize, right: bool) {
        let j = if right { i + 1 } else { i.wrapping_sub(1) };
        if j >= self.wb.sheets.len() {
            return;
        }
        self.push_undo(&[Scope::All]);
        self.wb.sheets.swap(i, j);
        if self.wb.active == i {
            self.wb.active = j;
        } else if self.wb.active == j {
            self.wb.active = i;
        }
        self.dirty = true;
    }

    /// Moves sheet `src` so it lands before position `dst` (0..=len).
    pub fn move_sheet_to(&mut self, src: usize, dst: usize) {
        let n = self.wb.sheets.len();
        if src >= n || dst > n || dst == src || dst == src + 1 {
            return;
        }
        self.push_undo(&[Scope::All]);
        let active_name = self.wb.sheets[self.wb.active].name.clone();
        let sh = self.wb.sheets.remove(src);
        let at = if dst > src { dst - 1 } else { dst };
        self.wb.sheets.insert(at, sh);
        self.wb.active = self.wb.sheet_index(&active_name).unwrap_or(at);
        self.dirty = true;
    }

    pub fn duplicate_sheet(&mut self, i: usize) {
        self.push_undo(&[Scope::All]);
        let mut copy = self.wb.sheets[i].clone();
        let base = copy.name.clone();
        let mut k = 2;
        loop {
            let suffix = format!(" ({k})");
            let n = format!("{}{}", base.chars().take(31 - suffix.chars().count()).collect::<String>(), suffix);
            if self.wb.sheet_index(&n).is_none() {
                copy.name = n;
                break;
            }
            k += 1;
        }
        self.wb.sheets.insert(i + 1, copy);
        self.wb.active = i + 1;
        self.changed();
    }

    pub fn hide_sheet(&mut self, i: usize) {
        let visible = self.wb.sheets.iter().filter(|s| !s.hidden).count();
        if visible <= 1 {
            self.dialog = Some(Dialog::Message { title: "Hide".into(), text: "A workbook must contain at least one visible worksheet.".into() });
            return;
        }
        self.push_undo(&[Scope::All]);
        self.wb.sheets[i].hidden = true;
        if self.wb.active == i {
            let next = (0..self.wb.sheets.len()).map(|k| (i + k) % self.wb.sheets.len()).find(|k| !self.wb.sheets[*k].hidden).unwrap_or(0);
            self.wb.active = next;
        }
        self.dirty = true;
    }

    pub fn unhide_sheet(&mut self, i: usize) {
        self.push_undo(&[Scope::All]);
        self.wb.sheets[i].hidden = false;
        self.wb.active = i;
        self.dirty = true;
    }

    /// Next visible sheet in a direction (Ctrl+PgUp/PgDn).
    pub fn next_visible_sheet(&self, forward: bool) -> usize {
        let n = self.wb.sheets.len();
        let mut i = self.wb.active;
        loop {
            let j = if forward { i + 1 } else { i.wrapping_sub(1) };
            if j >= n {
                return self.wb.active;
            }
            if !self.wb.sheets[j].hidden {
                return j;
            }
            i = j;
        }
    }

    pub fn switch_sheet(&mut self, i: usize) {
        if i >= self.wb.sheets.len() {
            return;
        }
        // While pointing in a formula, switching sheets keeps the editor open.
        let pointing = self.edit.as_ref().is_some_and(|e| e.can_point());
        if !pointing && self.edit.is_some() && !self.commit_edit(false) {
            return;
        }
        self.wb.active = i;
        if let Some(e) = &mut self.edit {
            e.point_sheet = Some(i);
        }
    }

    pub fn new_name(&mut self, name: &str, refers: &str) -> Result<(), String> {
        let n = name.trim();
        if n.is_empty() || !n.chars().next().unwrap().is_alphabetic() && !n.starts_with('_') || n.contains(' ') || formula::parse_cell_part(n).is_some() {
            return Err("The name that you entered is not valid.".into());
        }
        let f = refers.trim().trim_start_matches('=').to_string();
        if formula::parse(&f).is_err() {
            return Err("The formula in 'Refers to' contains an error.".into());
        }
        self.push_undo(&[Scope::All]);
        self.wb.names.retain(|(k, _)| !k.eq_ignore_ascii_case(n));
        self.wb.names.push((n.to_string(), f));
        self.changed();
        Ok(())
    }

    /// Absolute reference text of the current selection, e.g. Sheet1!$A$1:$B$4.
    pub fn sel_abs_ref(&self) -> String {
        let s = self.sel_clipped();
        let a = format!("${}${}", crate::model::col_name(s.c1), s.r1 + 1);
        let b = format!("${}${}", crate::model::col_name(s.c2), s.r2 + 1);
        let body = if s.is_single() { a } else { format!("{a}:{b}") };
        format!("{}!{}", formula::quote_sheet(&self.sheet().name), body)
    }

    pub fn recalc_now(&mut self) {
        self.wb.recalc();
    }

    pub fn update_cell_formula_text(cell: &mut Cell) {
        reprint(cell);
    }

    pub fn set_formula_cell(&mut self, r: u32, c: u32, f: &str) {
        let si = self.wb.active;
        let style = self.wb.sheets[si].get(r, c).map(|x| x.style).unwrap_or(0);
        let cell = Cell { value: Value::Empty, formula: Some(Box::new(Formula::new(f))), style };
        self.wb.sheets[si].set(r, c, Some(cell));
    }
}

impl App {
    // ------------------------------------------------------------------
    // Notes
    // ------------------------------------------------------------------

    pub fn edit_note(&mut self) {
        self.commit_edit(false);
        let (r, c) = self.active();
        let text = self.sheet().notes.get(&(r, c)).cloned().unwrap_or_default();
        self.dialog = Some(Dialog::Note { r, c, text, focus: true });
    }

    pub fn set_note(&mut self, r: u32, c: u32, text: Option<String>) {
        let si = self.wb.active;
        self.push_undo(&[Scope::Sheet(si)]);
        match text {
            Some(t) if !t.trim().is_empty() => {
                self.wb.sheets[si].notes.insert((r, c), t);
            }
            _ => {
                self.wb.sheets[si].notes.remove(&(r, c));
            }
        }
        self.dirty = true;
    }

    pub fn delete_notes_in_selection(&mut self) {
        let sel = self.sel();
        let si = self.wb.active;
        if !self.wb.sheets[si].notes.keys().any(|(r, c)| sel.contains(*r, *c)) {
            return;
        }
        self.push_undo(&[Scope::Sheet(si)]);
        self.wb.sheets[si].notes.retain(|(r, c), _| !sel.contains(*r, *c));
        self.dirty = true;
    }

    // ------------------------------------------------------------------
    // Drag-and-drop move/copy of the selection
    // ------------------------------------------------------------------

    pub fn drop_selection(&mut self, ctx: &egui::Context, r: u32, c: u32, copy: bool) {
        let sel = self.sel();
        if (r, c) == (sel.r1, sel.c1) {
            return;
        }
        self.copy(ctx, !copy);
        self.select(r, c, false);
        self.paste_internal(PasteMode::All);
        if copy {
            self.clip = None;
        }
    }
}

impl App {
    // ------------------------------------------------------------------
    // Hyperlinks
    // ------------------------------------------------------------------

    pub fn open_link_dialog(&mut self) {
        self.commit_edit(false);
        let (r, c) = self.active();
        let sh = self.sheet();
        let text = sh.get(r, c).map(|x| crate::numfmt::value_to_text(&x.value)).unwrap_or_default();
        let address = sh.links.get(&(r, c)).cloned().unwrap_or_default();
        self.dialog = Some(Dialog::Link { r, c, text, address, focus: true });
    }

    pub fn set_link(&mut self, r: u32, c: u32, text: &str, address: Option<&str>) {
        let si = self.wb.active;
        self.push_undo(&[Scope::Sheet(si)]);
        match address.map(str::trim).filter(|a| !a.is_empty()) {
            Some(a) => {
                let a = if a.contains("://") || a.starts_with("mailto:") || a.starts_with('#') {
                    a.to_string()
                } else if a.contains('@') && !a.contains(' ') {
                    format!("mailto:{a}")
                } else if crate::model::Range::parse(a.rsplit('!').next().unwrap_or("")).is_some() {
                    format!("#{a}")
                } else {
                    format!("https://{a}")
                };
                self.wb.sheets[si].links.insert((r, c), a.clone());
                let shown = if text.trim().is_empty() { a.trim_start_matches('#').to_string() } else { text.to_string() };
                let style = self.wb.sheets[si].get(r, c).map(|x| x.style).unwrap_or(0);
                let mut st = self.wb.style(style).clone();
                st.color = Some(0x0563C1);
                st.underline = true;
                let sid = self.wb.intern_style(st);
                let has_formula = self.wb.sheets[si].get(r, c).is_some_and(|x| x.formula.is_some());
                let cell = self.wb.sheets[si].cell_mut(r, c);
                if !has_formula {
                    cell.value = Value::Text(shown);
                }
                cell.style = sid;
            }
            None => {
                self.wb.sheets[si].links.remove(&(r, c));
                let style = self.wb.sheets[si].get(r, c).map(|x| x.style).unwrap_or(0);
                let mut st = self.wb.style(style).clone();
                if st.color == Some(0x0563C1) {
                    st.color = None;
                    st.underline = false;
                }
                let sid = self.wb.intern_style(st);
                if self.wb.sheets[si].get(r, c).is_some() {
                    self.wb.sheets[si].cell_mut(r, c).style = sid;
                }
            }
        }
        self.changed();
    }

    /// Follows a hyperlink: places in the workbook are selected; web and mail links open externally.
    pub fn follow_link(&mut self, target: &str) {
        if let Some(loc) = target.strip_prefix('#') {
            if !self.go_to(loc) {
                self.status = Some(format!("Reference isn't valid: {loc}"));
            }
            return;
        }
        let lower = target.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:") {
            let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", target]).spawn();
            self.status = Some(format!("Opening {target}"));
        } else {
            self.status = Some(format!("Links of this type aren't opened automatically: {target}"));
        }
    }
}

impl App {
    /// Format as Table: banded rows, styled header and AutoFilter on the current region.
    /// `kind`: 0 = Light, 1 = Medium, 2 = Dark.
    pub fn format_as_table(&mut self, accent: u32, kind: u8) {
        self.commit_edit(false);
        let rg = if self.sel().is_single() { self.current_region() } else { self.sel_clipped() };
        if rg.rows() < 2 {
            self.status = Some("Select a range with a header row and data to format as a table.".into());
            return;
        }
        let si = self.wb.active;
        self.push_undo(&[Scope::Sheet(si)]);
        let tint = super::theme::tint;
        let (head_fill, head_color, band, bottom) = match kind {
            0 => (None, Some(accent), Some(tint(accent, 0.8)), BorderLine::Thin),
            1 => (Some(accent), Some(0xFFFFFF), Some(tint(accent, 0.8)), BorderLine::None),
            _ => (Some(0x000000), Some(0xFFFFFF), Some(tint(accent, -0.25)), BorderLine::Medium),
        };
        let dark = kind == 2;
        for r in rg.r1..=rg.r2 {
            for c in rg.c1..=rg.c2 {
                let old = self.wb.sheets[si].get(r, c).map(|x| x.style).unwrap_or(0);
                let mut st = self.wb.style(old).clone();
                if r == rg.r1 {
                    st.bold = true;
                    st.fill = head_fill;
                    st.color = head_color;
                    st.border.bottom = bottom;
                } else {
                    let banded = (r - rg.r1) % 2 == 1;
                    st.fill = if banded { band } else if dark { Some(tint(accent, -0.5)) } else { None };
                    if dark {
                        st.color = Some(0xFFFFFF);
                    }
                    if kind == 0 && r == rg.r2 {
                        st.border.bottom = BorderLine::Thin;
                    }
                }
                let id = self.wb.intern_style(st);
                self.wb.sheets[si].cell_mut(r, c).style = id;
                self.wb.sheets[si].tidy(r, c);
            }
        }
        self.wb.sheets[si].filter = Some(crate::model::AutoFilter { range: rg, excluded: Default::default() });
        self.select_range(Range::new(rg.r1, rg.c1, rg.r2, rg.c2));
        self.dirty = true;
    }
}

/// Case-insensitive match of `needle` at byte `start` of `hay`; returns the end byte.
fn match_ci_at(hay: &str, start: usize, needle: &[char]) -> Option<usize> {
    let mut k = 0;
    let mut end = start;
    for (j, ch) in hay[start..].char_indices() {
        if k == needle.len() {
            break;
        }
        for lc in ch.to_lowercase() {
            if k < needle.len() && lc == needle[k] {
                k += 1;
            } else {
                return None;
            }
        }
        end = start + j + ch.len_utf8();
    }
    (k == needle.len()).then_some(end)
}

fn replace_ci(text: &str, what: &str, with: &str) -> String {
    if what.is_empty() {
        return text.to_string();
    }
    let needle: Vec<char> = what.chars().flat_map(|c| c.to_lowercase()).collect();
    let mut out = String::new();
    let mut i = 0;
    while i < text.len() {
        match match_ci_at(text, i, &needle) {
            Some(end) if end > i => {
                out.push_str(with);
                i = end;
            }
            _ => {
                let ch = text[i..].chars().next().unwrap_or(' ');
                out.push(ch);
                i += ch.len_utf8().max(1);
            }
        }
    }
    out
}

fn sort_cmp(a: &Value, b: &Value, desc: bool) -> Ordering {
    let rank = |v: &Value| match v {
        Value::Number(_) => 0,
        Value::Text(_) => 1,
        Value::Bool(_) => 2,
        Value::Error(_) => 3,
        Value::Empty => 4,
    };
    let (ra, rb) = (rank(a), rank(b));
    if ra == 4 || rb == 4 {
        return ra.cmp(&rb); // blanks always last
    }
    let o = if ra != rb {
        ra.cmp(&rb)
    } else {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
            (Value::Text(x), Value::Text(y)) => x.to_lowercase().cmp(&y.to_lowercase()),
            (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
            _ => Ordering::Equal,
        }
    };
    if desc { o.reverse() } else { o }
}

/// Splits clipboard text (tab separated, quoted fields allowed) into rows.
pub fn parse_tsv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut chars = text.chars().peekable();
    let mut in_q = false;
    let mut at_start = true;
    while let Some(ch) = chars.next() {
        if in_q {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_q = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }
        match ch {
            '"' if at_start => {
                in_q = true;
                at_start = false;
            }
            '\t' => {
                row.push(std::mem::take(&mut field));
                at_start = true;
            }
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                at_start = true;
            }
            c => {
                field.push(c);
                at_start = false;
            }
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// Adds or removes decimal places in a number format code.
pub fn adjust_decimals(fmt: &str, delta: i32, sample: &Value) -> String {
    let f = fmt.trim();
    if f.is_empty() || f.eq_ignore_ascii_case("general") {
        let cur = match sample {
            Value::Number(n) => {
                let g = numfmt::general(*n);
                g.split_once('.').map(|(_, d)| d.trim_end_matches(|c: char| !c.is_ascii_digit()).len()).unwrap_or(0) as i32
            }
            _ => 0,
        };
        let n = (cur + delta).max(0) as usize;
        return if n == 0 { "0".into() } else { format!("0.{}", "0".repeat(n)) };
    }
    let mut out = String::new();
    let chars: Vec<char> = f.chars().collect();
    let mut i = 0;
    let mut in_q = false;
    let mut in_b = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            in_q = !in_q;
        }
        if !in_q && c == '[' {
            in_b = true;
        }
        if !in_q && c == ']' {
            in_b = false;
        }
        if !in_q && !in_b && matches!(c, '0' | '#' | '?') {
            // a digit run
            let mut run = String::new();
            while i < chars.len() && matches!(chars[i], '0' | '#' | '?' | ',' | '.') {
                run.push(chars[i]);
                i += 1;
            }
            if let Some(dot) = run.find('.') {
                let (int, dec) = run.split_at(dot);
                let mut dec: String = dec[1..].to_string();
                if delta > 0 {
                    dec.push('0');
                } else if !dec.is_empty() {
                    dec.pop();
                }
                out.push_str(int);
                if !dec.is_empty() {
                    out.push('.');
                    out.push_str(&dec);
                }
            } else if delta > 0 {
                out.push_str(&run);
                out.push_str(".0");
            } else {
                out.push_str(&run);
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Fill-series detection for drag-fill.
enum Series {
    Copy,
    Linear { first: f64, step: f64 },
    TextNum { prefix: String, first: i64, step: i64, width: usize },
    Names { list: &'static [&'static str], start: usize, step: i64 },
}

impl Series {
    fn detect(seq: &[Option<Cell>], wb: &crate::model::Workbook) -> Series {
        if seq.iter().any(|c| c.as_ref().is_some_and(|c| c.formula.is_some())) {
            return Series::Copy;
        }
        let vals: Vec<Value> = seq.iter().map(|c| c.as_ref().map(|c| c.value.clone()).unwrap_or_default()).collect();
        if vals.iter().all(|v| matches!(v, Value::Number(_))) {
            let nums: Vec<f64> = vals.iter().filter_map(|v| v.as_number()).collect();
            if nums.len() >= 2 {
                return Series::Linear { first: nums[0], step: (nums[nums.len() - 1] - nums[0]) / (nums.len() - 1) as f64 };
            }
            if nums.len() == 1 {
                let is_date = seq[0].as_ref().is_some_and(|c| numfmt::is_date_format(&wb.style(c.style).num_fmt));
                if is_date {
                    return Series::Linear { first: nums[0], step: 1.0 };
                }
            }
            return Series::Copy;
        }
        if let Some(texts) = vals.iter().map(|v| if let Value::Text(t) = v { Some(t.clone()) } else { None }).collect::<Option<Vec<_>>>() {
            for list in [&MONTHS_SHORT[..], &MONTHS_LONG[..], &DAYS_SHORT[..], &DAYS_LONG[..]] {
                let idx: Option<Vec<usize>> = texts.iter().map(|t| list.iter().position(|m| m.eq_ignore_ascii_case(t))).collect();
                if let Some(idx) = idx {
                    let step = if idx.len() >= 2 { idx[1] as i64 - idx[0] as i64 } else { 1 };
                    return Series::Names { list, start: idx[0], step };
                }
            }
            // "Item 1", "Q3"
            let split = |t: &str| -> Option<(String, i64, usize)> {
                let digits: String = t.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
                if digits.is_empty() || digits.len() == t.len() {
                    return None;
                }
                Some((t[..t.len() - digits.len()].to_string(), digits.parse().ok()?, digits.len()))
            };
            let parts: Option<Vec<_>> = texts.iter().map(|t| split(t)).collect();
            if let Some(p) = parts {
                if p.iter().all(|x| x.0 == p[0].0) {
                    let step = if p.len() >= 2 { (p[p.len() - 1].1 - p[0].1) / (p.len() as i64 - 1) } else { 1 };
                    return Series::TextNum { prefix: p[0].0.clone(), first: p[0].1, step, width: p[0].2 };
                }
            }
        }
        Series::Copy
    }

    /// Value at absolute step index (0 = first source cell).
    fn value(&self, k: i64, _src_idx: usize) -> Option<Value> {
        match self {
            Series::Copy => None,
            Series::Linear { first, step } => Some(Value::Number(first + step * k as f64)),
            Series::TextNum { prefix, first, step, width } => {
                let n = first + step * k;
                let w = if n >= 0 { *width } else { 0 };
                Some(Value::Text(format!("{prefix}{:0w$}", n, w = w)))
            }
            Series::Names { list, start, step } => {
                let i = (*start as i64 + step * k).rem_euclid(list.len() as i64) as usize;
                Some(Value::Text(list[i].to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals() {
        assert_eq!(adjust_decimals("0.00", 1, &Value::Empty), "0.000");
        assert_eq!(adjust_decimals("0.0", -1, &Value::Empty), "0");
        assert_eq!(adjust_decimals("#,##0", 1, &Value::Empty), "#,##0.0");
        assert_eq!(adjust_decimals("", 1, &Value::Number(1.5)), "0.00");
        assert_eq!(adjust_decimals("0%", 1, &Value::Empty), "0.0%");
    }

    #[test]
    fn tsv() {
        assert_eq!(parse_tsv("a\tb\r\nc\t\"d\ne\"\r\n"), vec![vec!["a", "b"], vec!["c", "d\ne"]]);
    }
}
