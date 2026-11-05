//! What-If Analysis: Goal Seek, data tables ({=TABLE(r,c)}) and the Scenario Manager.
//!
//! The algorithms work on a `Workbook` directly (set a cell, recalculate, read a cell) so they
//! can be tested without the UI.

use crate::formula;
use crate::model::{cell_from_input, cell_name, Cell, Range, Sheet, Style, Value, Workbook};

pub type CellRef = (usize, u32, u32);

// ---------------------------------------------------------------------------
// Workbook probing
// ---------------------------------------------------------------------------

/// Puts a number in a cell (keeping its formatting) and recalculates what depends on it.
pub fn set_number(wb: &mut Workbook, at: CellRef, x: f64) {
    let (s, r, c) = at;
    let style = wb.sheets[s].get(r, c).map_or(0, |c| c.style);
    wb.sheets[s].set(r, c, Some(Cell { value: Value::Number(x), formula: None, style }));
    formula::recalc_ranges(wb, &[(s, Range::cell(r, c))]);
}

pub fn number_at(wb: &Workbook, at: CellRef) -> Option<f64> {
    match wb.sheets.get(at.0)?.value(at.1, at.2) {
        Value::Number(n) if n.is_finite() => Some(*n),
        Value::Empty => Some(0.0),
        Value::Bool(b) => Some(*b as u8 as f64),
        _ => None,
    }
}

/// Parses a cell reference typed into a dialog ("B3", "$B$3", "Sheet2!B3", or a defined name).
pub fn parse_cell_ref(wb: &Workbook, text: &str) -> Option<CellRef> {
    let rg = parse_range_ref(wb, text)?;
    rg.1.is_single().then_some((rg.0, rg.1.r1, rg.1.c1))
}

/// Parses a range reference typed into a dialog, resolving defined names; returns (sheet, range).
pub fn parse_range_ref(wb: &Workbook, text: &str) -> Option<(usize, Range)> {
    let t = text.trim().trim_start_matches('=').trim();
    if t.is_empty() {
        return None;
    }
    let (sheet, part) = match t.rsplit_once('!') {
        Some((s, p)) => (wb.sheet_index(&s.trim_matches('\'').replace("''", "'"))?, p),
        None => (wb.active, t),
    };
    if let Some(rg) = Range::parse(part) {
        return Some((sheet, rg));
    }
    if t.contains('!') {
        return None;
    }
    let (_, f) = formula::defined_name(wb, t, wb.active)?;
    let f = f.trim_start_matches('=').to_string();
    if f.eq_ignore_ascii_case(t) {
        return None;
    }
    parse_range_ref(wb, &f)
}

// ---------------------------------------------------------------------------
// Goal Seek
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct GoalSeek {
    pub found: bool,
    /// Value left in the changing cell.
    pub x: f64,
    /// Value of the set cell for `x`.
    pub value: f64,
    pub iterations: usize,
}

/// Changes `changing` until `set` equals `target` (within `max_change`), like Excel's Goal Seek:
/// a few secant steps from the current value, then an outward search for a sign change, then
/// bisection with secant acceleration inside the bracket. At most `max_iter` evaluations; the
/// best value found is left in the changing cell.
pub fn goal_seek(wb: &mut Workbook, set: CellRef, target: f64, changing: CellRef, max_iter: usize, max_change: f64) -> GoalSeek {
    let x0 = number_at(wb, changing).unwrap_or(0.0);
    let mut seek = Seek { wb, set, changing, target, evals: 0, max_iter, best: None, bracket: None };
    let found = seek.run(x0, max_change);
    let (x, y) = seek.best.unwrap_or((x0, f64::NAN));
    let iterations = seek.evals;
    let wb = seek.wb;
    set_number(wb, changing, x);
    let value = number_at(wb, set).unwrap_or(y + target);
    GoalSeek { found: found || y.abs() <= max_change, x, value, iterations }
}

struct Seek<'a> {
    wb: &'a mut Workbook,
    set: CellRef,
    changing: CellRef,
    target: f64,
    evals: usize,
    max_iter: usize,
    best: Option<(f64, f64)>,
    /// (lo, f(lo), hi, f(hi)) with f(lo), f(hi) of opposite signs.
    bracket: Option<(f64, f64, f64, f64)>,
}

impl Seek<'_> {
    fn f(&mut self, x: f64) -> Option<f64> {
        if !x.is_finite() || self.evals >= self.max_iter {
            return None;
        }
        self.evals += 1;
        set_number(self.wb, self.changing, x);
        let y = number_at(self.wb, self.set).map(|v| v - self.target).filter(|y| y.is_finite())?;
        if self.best.is_none_or(|(_, by)| y.abs() < by.abs()) {
            self.best = Some((x, y));
        }
        Some(y)
    }

    fn note_sign(&mut self, a: (f64, f64), b: (f64, f64)) {
        if a.1.signum() != b.1.signum() && a.1 != 0.0 && b.1 != 0.0 {
            let (lo, hi) = if a.0 <= b.0 { (a, b) } else { (b, a) };
            let narrower = self.bracket.is_none_or(|(l, _, h, _)| hi.0 - lo.0 < h - l);
            if narrower {
                self.bracket = Some((lo.0, lo.1, hi.0, hi.1));
            }
        }
    }

    fn done(&self, tol: f64) -> bool {
        self.best.is_some_and(|(_, y)| y.abs() <= tol)
    }

    fn run(&mut self, x0: f64, tol: f64) -> bool {
        let Some(y0) = self.f(x0) else { return false };
        if self.done(tol) {
            return true;
        }
        let h0 = if x0 == 0.0 { 0.01 } else { x0.abs() * 0.01 };
        // 1. Secant steps while they improve.
        let (mut xa, mut ya) = (x0, y0);
        let (mut xb, mut yb) = match self.f(x0 + h0) {
            Some(y) => (x0 + h0, y),
            None => (x0, y0),
        };
        self.note_sign((xa, ya), (xb, yb));
        for _ in 0..20 {
            if self.done(tol) || self.bracket.is_some() || yb == ya {
                break;
            }
            let x = xb - yb * (xb - xa) / (yb - ya);
            let Some(y) = self.f(x) else { break };
            self.note_sign((xb, yb), (x, y));
            if y.abs() >= yb.abs() && y.abs() >= ya.abs() {
                break;
            }
            (xa, ya, xb, yb) = (xb, yb, x, y);
        }
        // 2. Search outwards from the starting value for a sign change.
        let mut h = h0;
        while self.bracket.is_none() && !self.done(tol) && self.evals + 2 < self.max_iter && h < 1e15 {
            h *= 4.0;
            for x in [x0 + h, x0 - h] {
                if let Some(y) = self.f(x) {
                    self.note_sign((x0, y0), (x, y));
                }
            }
        }
        // 3. Bisection with false-position steps (Illinois) inside the bracket.
        let Some((mut lo, mut flo, mut hi, mut fhi)) = self.bracket else { return self.done(tol) };
        let mut side = 0i8;
        while !self.done(tol) && self.evals < self.max_iter {
            let mut x = hi - fhi * (hi - lo) / (fhi - flo);
            if !(x > lo && x < hi) {
                x = (lo + hi) / 2.0;
            }
            let Some(y) = self.f(x) else {
                // An error value inside the bracket: bisect instead.
                let m = (lo + hi) / 2.0;
                if m == x || self.evals >= self.max_iter {
                    break;
                }
                match self.f(m) {
                    Some(y) if y.signum() == flo.signum() => (lo, flo) = (m, y),
                    Some(y) => (hi, fhi) = (m, y),
                    None => break,
                }
                continue;
            };
            if y.signum() == flo.signum() {
                (lo, flo) = (x, y);
                if side == -1 {
                    fhi /= 2.0;
                }
                side = -1;
            } else {
                (hi, fhi) = (x, y);
                if side == 1 {
                    flo /= 2.0;
                }
                side = 1;
            }
            if hi - lo <= f64::EPSILON * (1.0 + lo.abs().max(hi.abs())) {
                break;
            }
        }
        self.done(tol)
    }
}
// ---------------------------------------------------------------------------
// Data tables
// ---------------------------------------------------------------------------

/// A What-If data table: `range` holds the {=TABLE()} results; the row above and the column to
/// the left hold the input values and the formula(s).
#[derive(Clone, Debug, PartialEq)]
pub struct DataTable {
    pub range: Range,
    pub row_input: Option<(u32, u32)>,
    pub col_input: Option<(u32, u32)>,
}

impl DataTable {
    /// The formula text shown in every result cell, e.g. `TABLE(B1,A1)` or `TABLE(,A1)`.
    pub fn formula_text(&self) -> String {
        let f = |c: Option<(u32, u32)>| c.map(|(r, c)| cell_name(r, c)).unwrap_or_default();
        format!("TABLE({},{})", f(self.row_input), f(self.col_input))
    }
}

/// For a cell inside a data table: the formula cell to evaluate and the substitutions
/// (input cell, cell holding the value to put in it).
pub fn table_inputs(sheet: &Sheet, r: u32, c: u32) -> Option<((u32, u32), Vec<((u32, u32), (u32, u32))>)> {
    let dt = sheet.data_tables.iter().find(|t| t.range.contains(r, c))?;
    let (r1, c1) = (dt.range.r1.checked_sub(1)?, dt.range.c1.checked_sub(1)?);
    match (dt.row_input, dt.col_input) {
        (Some(ri), Some(ci)) => Some(((r1, c1), vec![(ri, (r1, c)), (ci, (r, c1))])),
        (None, Some(ci)) => Some(((r1, c), vec![(ci, (r, c1))])),
        (Some(ri), None) => Some(((r, c1), vec![(ri, (r1, c))])),
        (None, None) => None,
    }
}

/// Creates a data table over `sel` (formula row/column included). Fills the result cells with
/// {=TABLE()} formulas; the caller recalculates.
pub fn create_data_table(sheet: &mut Sheet, sel: Range, row_input: Option<(u32, u32)>, col_input: Option<(u32, u32)>) -> Result<(), String> {
    if row_input.is_none() && col_input.is_none() {
        return Err("Input cell reference is not valid.".into());
    }
    if sel.rows() < 2 || sel.cols() < 2 {
        return Err("Select the formula and the input values together: at least two rows and two columns.".into());
    }
    let range = Range::new(sel.r1 + 1, sel.c1 + 1, sel.r2, sel.c2);
    for inp in [row_input, col_input].into_iter().flatten() {
        if range.contains(inp.0, inp.1) {
            return Err("Input cell reference is not valid.".into());
        }
    }
    sheet.data_tables.retain(|t| !t.range.intersects(&range));
    let dt = DataTable { range, row_input, col_input };
    let text = dt.formula_text();
    for r in range.r1..=range.r2 {
        for c in range.c1..=range.c2 {
            let style = sheet.get(r, c).map_or(0, |c| c.style);
            sheet.set(r, c, Some(Cell { value: Value::Empty, formula: Some(Box::new(crate::model::Formula::new(&text))), style }));
        }
    }
    sheet.data_tables.push(dt);
    Ok(())
}

/// The data table a range touches, if any (edits there are refused, like Excel).
pub fn table_touching(sheet: &Sheet, rg: Range) -> Option<&DataTable> {
    sheet.data_tables.iter().find(|t| t.range.intersects(&rg) && !(rg.r1 <= t.range.r1 && rg.c1 <= t.range.c1 && rg.r2 >= t.range.r2 && rg.c2 >= t.range.c2))
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Scenario {
    pub name: String,
    pub comment: String,
    pub user: String,
    pub locked: bool,
    pub hidden: bool,
    /// Changing cells and the values (as typed) the scenario puts in them.
    pub cells: Vec<((u32, u32), String)>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Scenarios {
    pub list: Vec<Scenario>,
    /// Scenario last selected / shown in the manager.
    pub current: usize,
    pub show: usize,
    /// Result cells chosen for the last summary report.
    pub result_cells: Vec<Range>,
}

/// Shows a scenario: puts its values in the changing cells (replacing formulas) and recalculates.
pub fn show_scenario(wb: &mut Workbook, si: usize, sc: &Scenario) {
    let mut edited = Vec::new();
    for ((r, c), v) in &sc.cells {
        let style = wb.sheets[si].get(*r, *c).map_or(0, |c| c.style);
        let text = v.trim_start_matches('=').to_string();
        let cell = cell_from_input(wb, if text.is_empty() { "" } else { v.as_str() }, style).map(|mut c| {
            if c.formula.is_some() {
                // Scenario values are constants.
                c.formula = None;
                c.value = crate::numfmt::parse_input(&text).0;
            }
            c
        });
        wb.sheets[si].set(*r, *c, cell);
        edited.push((si, Range::cell(*r, *c)));
    }
    formula::recalc_ranges(wb, &edited);
}

/// Text for a scenario value box: the cell's current value as it would be typed.
pub fn value_text(v: &Value) -> String {
    match v {
        Value::Number(n) => crate::numfmt::general(*n),
        other => crate::numfmt::value_to_text(other),
    }
}

/// Builds the "Scenario Summary" sheet: current values, each scenario's changing cells and the
/// result cells it produces. The workbook is left as it was.
pub fn scenario_summary(wb: &mut Workbook, si: usize, results: &[(u32, u32)], name: &str) -> Sheet {
    let list: Vec<Scenario> = wb.sheets[si].scenarios.list.clone();
    let mut changing: Vec<(u32, u32)> = Vec::new();
    for sc in &list {
        for (p, _) in &sc.cells {
            if !changing.contains(p) {
                changing.push(*p);
            }
        }
    }
    // Save the changing cells, collect each scenario's results, then restore.
    let saved: Vec<((u32, u32), Option<Cell>)> = changing.iter().map(|p| (*p, wb.sheets[si].get(p.0, p.1).cloned())).collect();
    let current_vals: Vec<Value> = changing.iter().map(|p| wb.sheets[si].value(p.0, p.1).clone()).collect();
    let current_res: Vec<Value> = results.iter().map(|p| wb.sheets[si].value(p.0, p.1).clone()).collect();
    let mut per: Vec<(Vec<Value>, Vec<Value>)> = Vec::new();
    for sc in &list {
        show_scenario(wb, si, sc);
        let ch = changing.iter().map(|p| wb.sheets[si].value(p.0, p.1).clone()).collect();
        let rs = results.iter().map(|p| wb.sheets[si].value(p.0, p.1).clone()).collect();
        per.push((ch, rs));
    }
    let mut edited = Vec::new();
    for (p, cell) in saved {
        wb.sheets[si].set(p.0, p.1, cell);
        edited.push((si, Range::cell(p.0, p.1)));
    }
    formula::recalc_ranges(wb, &edited);

    let gray = 0xD9D9D9;
    let head = wb.intern_style(Style { bold: true, fill: Some(0x7F7F7F), color: Some(0xFFFFFF), ..Default::default() });
    let title = wb.intern_style(Style { bold: true, fill: Some(gray), ..Default::default() });
    let label = wb.intern_style(Style { fill: Some(gray), ..Default::default() });
    let mut out = Sheet::new(name);
    out.gridlines = false;
    out.col_w.insert(0, 12.0);
    out.col_w.insert(1, 20.0);
    out.col_w.insert(2, 90.0);
    let put = |sh: &mut Sheet, r: u32, c: u32, v: Value, style: u32| sh.set(r, c, Some(Cell { value: v, formula: None, style }));
    let names = |wb: &Workbook, p: (u32, u32)| -> String {
        // A defined name pointing at the cell reads better than its address.
        let addr = format!("${}${}", crate::model::col_name(p.1), p.0 + 1);
        let sheet = &wb.sheets[si].name;
        wb.names
            .iter()
            .chain(wb.sheets[si].names.iter())
            .find(|(_, f)| {
                let f = f.trim_start_matches('=');
                f.eq_ignore_ascii_case(&addr) || f.eq_ignore_ascii_case(&format!("{}!{}", formula::quote_sheet(sheet), addr))
            })
            .map(|(n, _)| n.clone())
            .unwrap_or(addr)
    };
    let fmt_of = |wb: &mut Workbook, p: (u32, u32), extra: Option<u32>| -> u32 {
        let src = wb.sheets[si].get(p.0, p.1).map_or(0, |c| c.style);
        let num_fmt = wb.style(src).num_fmt.clone();
        wb.intern_style(Style { num_fmt, fill: extra, ..Default::default() })
    };
    put(&mut out, 1, 1, Value::Text("Scenario Summary".into()), title);
    for c in 2..(4 + list.len() as u32) {
        put(&mut out, 1, c, Value::Empty, title);
    }
    put(&mut out, 2, 3, Value::Text("Current Values:".into()), head);
    for (i, sc) in list.iter().enumerate() {
        put(&mut out, 2, 4 + i as u32, Value::Text(sc.name.clone()), head);
        out.col_w.insert(4 + i as u32, 84.0);
    }
    out.col_w.insert(3, 96.0);
    let mut r = 3;
    // Comments under the scenario names (Created by ... / Modified by ...).
    if list.iter().any(|s| !s.comment.is_empty()) {
        for (i, sc) in list.iter().enumerate() {
            put(&mut out, r, 4 + i as u32, Value::Text(sc.comment.clone()), label);
        }
        put(&mut out, r, 3, Value::Empty, label);
        r += 1;
    }
    put(&mut out, r, 1, Value::Text("Changing Cells:".into()), title);
    r += 1;
    for (k, p) in changing.iter().enumerate() {
        put(&mut out, r, 2, Value::Text(names(wb, *p)), 0);
        let st = fmt_of(wb, *p, None);
        put(&mut out, r, 3, current_vals[k].clone(), st);
        for (i, (sc, (vals, _))) in list.iter().zip(&per).enumerate() {
            let mine = sc.cells.iter().any(|(q, _)| q == p);
            let st = fmt_of(wb, *p, mine.then_some(gray));
            put(&mut out, r, 4 + i as u32, vals[k].clone(), st);
        }
        r += 1;
    }
    if !results.is_empty() {
        put(&mut out, r, 1, Value::Text("Result Cells:".into()), title);
        r += 1;
        for (k, p) in results.iter().enumerate() {
            put(&mut out, r, 2, Value::Text(names(wb, *p)), 0);
            let st = fmt_of(wb, *p, None);
            put(&mut out, r, 3, current_res[k].clone(), st);
            for (i, (_, res)) in per.iter().enumerate() {
                put(&mut out, r, 4 + i as u32, res[k].clone(), st);
            }
            r += 1;
        }
    }
    for line in [
        "Notes:  Current Values column represents values of changing cells at",
        "time Scenario Summary Report was created.  Changing cells for each",
        "scenario are highlighted in gray.",
    ] {
        put(&mut out, r, 1, Value::Text(line.into()), 0);
        r += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Structure changes
// ---------------------------------------------------------------------------

fn shift_cell(p: (u32, u32), rows: bool, at: u32, n: i64) -> Option<(u32, u32)> {
    crate::condfmt::shift_range(Range::cell(p.0, p.1), rows, at, n).map(|r| (r.r1, r.c1))
}

/// Moves data tables, scenario cells and sparklines after rows/columns are inserted or deleted.
pub fn shift_sheet(sh: &mut Sheet, rows: bool, at: u32, n: i64) {
    sh.data_tables = std::mem::take(&mut sh.data_tables)
        .into_iter()
        .filter_map(|t| {
            let range = crate::condfmt::shift_range(t.range, rows, at, n)?;
            let row_input = match t.row_input {
                Some(p) => Some(shift_cell(p, rows, at, n)?),
                None => None,
            };
            let col_input = match t.col_input {
                Some(p) => Some(shift_cell(p, rows, at, n)?),
                None => None,
            };
            Some(DataTable { range, row_input, col_input })
        })
        .collect();
    for sc in sh.scenarios.list.iter_mut() {
        sc.cells = sc.cells.iter().filter_map(|(p, v)| shift_cell(*p, rows, at, n).map(|q| (q, v.clone()))).collect();
    }
    sh.scenarios.result_cells = sh.scenarios.result_cells.iter().filter_map(|r| crate::condfmt::shift_range(*r, rows, at, n)).collect();
    let name = sh.name.clone();
    for g in sh.sparklines.iter_mut() {
        g.shift(&name, rows, at, n);
    }
    sh.sparklines.retain(|g| !g.items.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Formula;

    fn put(wb: &mut Workbook, a1: &str, text: &str) {
        let (r, c) = crate::model::parse_cell(a1).unwrap();
        let cell = cell_from_input(wb, text, 0);
        wb.sheets[0].set(r, c, cell);
    }

    fn num(wb: &Workbook, a1: &str) -> f64 {
        let (r, c) = crate::model::parse_cell(a1).unwrap();
        number_at(wb, (0, r, c)).unwrap()
    }

    #[test]
    fn goal_seek_converges() {
        // Loan payment: what principal gives a payment of 500?
        let mut wb = Workbook::new();
        put(&mut wb, "A1", "10000");
        put(&mut wb, "A2", "=PMT(5%/12,60,-A1)");
        wb.recalc();
        let res = goal_seek(&mut wb, (0, 1, 0), 500.0, (0, 0, 0), 100, 0.001);
        assert!(res.found, "{res:?}");
        assert!((num(&wb, "A2") - 500.0).abs() <= 0.001);
        assert!((num(&wb, "A1") - 26495.35).abs() < 0.5, "{}", num(&wb, "A1"));
        // A nonlinear model: x^3 - 2x = 20 from x = 0.
        let mut wb = Workbook::new();
        put(&mut wb, "B1", "0");
        put(&mut wb, "B2", "=B1^3-2*B1");
        wb.recalc();
        let res = goal_seek(&mut wb, (0, 1, 1), 20.0, (0, 0, 1), 100, 0.001);
        assert!(res.found, "{res:?}");
        assert!((num(&wb, "B2") - 20.0).abs() <= 0.001);
        // An impossible target is reported but the closest value is kept.
        let mut wb = Workbook::new();
        put(&mut wb, "C1", "3");
        put(&mut wb, "C2", "=C1^2+1");
        wb.recalc();
        let res = goal_seek(&mut wb, (0, 1, 2), -5.0, (0, 0, 2), 100, 0.001);
        assert!(!res.found);
        assert!(num(&wb, "C2") < 2.0, "{}", num(&wb, "C2"));
    }

    #[test]
    fn data_tables_recalculate() {
        let mut wb = Workbook::new();
        // Model: B1 rate, B2 years, B3 = B1 * B2 * 100
        put(&mut wb, "B1", "0.05");
        put(&mut wb, "B2", "10");
        put(&mut wb, "B3", "=B1*B2*100");
        // One-variable (column input B1): rates in A6:A8, formula in B5.
        put(&mut wb, "B5", "=B3");
        put(&mut wb, "A6", "0.01");
        put(&mut wb, "A7", "0.02");
        put(&mut wb, "A8", "0.03");
        create_data_table(&mut wb.sheets[0], Range::parse("A5:B8").unwrap(), None, Some((0, 1))).unwrap();
        // Two-variable: formula in D5, years D6:D7 (column input B2), rates E5:F5 (row input B1).
        put(&mut wb, "D5", "=B3");
        put(&mut wb, "D6", "1");
        put(&mut wb, "D7", "2");
        put(&mut wb, "E5", "0.1");
        put(&mut wb, "F5", "0.2");
        create_data_table(&mut wb.sheets[0], Range::parse("D5:F7").unwrap(), Some((0, 1)), Some((1, 1))).unwrap();
        wb.recalc();
        assert_eq!(wb.sheets[0].get(5, 1).unwrap().input_text(), "{=TABLE(,B1)}");
        assert_eq!(wb.sheets[0].get(5, 4).unwrap().input_text(), "{=TABLE(B1,B2)}");
        assert!((num(&wb, "B6") - 10.0).abs() < 1e-9);
        assert!((num(&wb, "B8") - 30.0).abs() < 1e-9);
        assert!((num(&wb, "E6") - 10.0).abs() < 1e-9);
        assert!((num(&wb, "F7") - 40.0).abs() < 1e-9);
        // The model's own cells are untouched.
        assert!((num(&wb, "B3") - 50.0).abs() < 1e-9);
        // Changing a model input recalculates the tables (incrementally).
        put(&mut wb, "B2", "20");
        formula::recalc_ranges(&mut wb, &[(0, Range::parse("B2").unwrap())]);
        assert!((num(&wb, "B7") - 40.0).abs() < 1e-9, "{}", num(&wb, "B7"));
        // ...and so does changing an input value of the table.
        put(&mut wb, "A6", "0.5");
        formula::recalc_ranges(&mut wb, &[(0, Range::parse("A6").unwrap())]);
        assert!((num(&wb, "B6") - 1000.0).abs() < 1e-9, "{}", num(&wb, "B6"));
        assert!((num(&wb, "E6") - 10.0).abs() < 1e-9);
        // A formula reading the table follows it.
        put(&mut wb, "H1", "=SUM(B6:B8)");
        formula::recalc_ranges(&mut wb, &[(0, Range::parse("H1").unwrap())]);
        assert!((num(&wb, "H1") - (1000.0 + 40.0 + 60.0)).abs() < 1e-9, "{}", num(&wb, "H1"));
        let _ = Formula::new("=1");
    }

    #[test]
    fn scenarios_show_and_summarize() {
        let mut wb = Workbook::new();
        put(&mut wb, "B1", "100");
        put(&mut wb, "B2", "2");
        put(&mut wb, "B3", "=B1*B2");
        wb.recalc();
        wb.sheets[0].scenarios.list = vec![
            Scenario { name: "Low".into(), cells: vec![((0, 1), "10".into()), ((1, 1), "1".into())], ..Default::default() },
            Scenario { name: "High".into(), cells: vec![((0, 1), "1000".into()), ((1, 1), "3".into())], ..Default::default() },
        ];
        let summary = scenario_summary(&mut wb, 0, &[(2, 1)], "Scenario Summary");
        assert!((num(&wb, "B3") - 200.0).abs() < 1e-9, "the workbook is restored");
        let vals: Vec<String> = (0..12).flat_map(|r| (0..6).map(move |c| (r, c))).map(|(r, c)| crate::numfmt::value_to_text(summary.value(r, c))).filter(|s| !s.is_empty()).collect();
        for want in ["Scenario Summary", "Current Values:", "Low", "High", "$B$1", "$B$3", "10", "1000", "3000", "200"] {
            assert!(vals.iter().any(|v| v == want), "{want} missing from {vals:?}");
        }
        let sc = wb.sheets[0].scenarios.list[1].clone();
        show_scenario(&mut wb, 0, &sc);
        assert!((num(&wb, "B3") - 3000.0).abs() < 1e-9);
    }
}
