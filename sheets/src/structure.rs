//! Structural workbook changes shared by commands and live co-editing: inserting/deleting
//! rows and columns, deleting and renaming sheets, and rewriting formulas everywhere.

use crate::formula::{self, Expr};
use crate::model::{Range, Sheet, Workbook, MAX_COLS, MAX_ROWS};

/// Moves a sheet's cells and row/column-bound data for an insert (n > 0) or delete (n < 0)
/// of rows (`rows`) or columns at `at`.
pub fn shift_sheet(sh: &mut Sheet, rows: bool, at: u32, n: i64) {
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
    if rows {
        sh.row_levels = sh.row_levels.drain().filter_map(|(r, l)| map(r).map(|nr| (nr, l))).collect();
    } else {
        sh.col_levels = sh.col_levels.drain().filter_map(|(c, l)| map(c).map(|nc| (nc, l))).collect();
    }
    sh.links = sh
        .links
        .drain()
        .filter_map(|((r, c), t)| {
            let k = if rows { map(r).map(|nr| (nr, c)) } else { map(c).map(|nc| (r, nc)) };
            k.map(|k| (k, t))
        })
        .collect();
    sh.page.shift(rows, at, n);
    sh.notes = sh
        .notes
        .drain()
        .filter_map(|((r, c), t)| {
            let k = if rows { map(r).map(|nr| (nr, c)) } else { map(c).map(|nc| (r, nc)) };
            k.map(|k| (k, t))
        })
        .collect();
    let key = |(r, c): (u32, u32)| if rows { map(r).map(|nr| (nr, c)) } else { map(c).map(|nc| (r, nc)) };
    sh.note_authors = sh.note_authors.drain().filter_map(|(k, a)| key(k).map(|k| (k, a))).collect();
    sh.rich = sh.rich.drain().filter_map(|(k, t)| key(k).map(|k| (k, t))).collect();
    for p in &mut sh.images {
        if let Some(k) = key(p.anchor) {
            p.anchor = k;
        }
    }
    let fix = |rg: Range| -> Option<Range> {
        let (a, b) = if rows { (rg.r1, rg.r2) } else { (rg.c1, rg.c2) };
        let (na, nb) = if n > 0 {
            let na = if a >= at { a as i64 + n } else { a as i64 };
            if na >= max {
                return None; // pushed off the end of the sheet
            }
            (na as u32, if b >= at { (b as i64 + n).min(max - 1) as u32 } else { b })
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
    let mut tables = std::mem::take(&mut sh.tables);
    tables.retain_mut(|t| match fix(t.range) {
        Some(r) if r.rows() > t.header as u32 + t.totals as u32 => {
            let resized = r.cols() != t.range.cols();
            t.range = r;
            if !rows {
                t.criteria = std::mem::take(&mut t.criteria).into_iter().filter_map(|(c, cf)| map(c).map(|nc| (nc, cf))).collect();
            }
            if resized {
                crate::table::sync_columns_from_cells(sh, t);
            }
            true
        }
        _ => false,
    });
    sh.tables = tables;
    if let Some(f) = &mut sh.filter {
        match fix(f.range) {
            Some(r) => {
                f.range = r;
                if !rows {
                    f.columns = std::mem::take(&mut f.columns).into_iter().filter_map(|(c, cf)| map(c).map(|nc| (nc, cf))).collect();
                }
            }
            None => sh.filter = None,
        }
    }
}

/// Applies a formula transform to every formula in the workbook (cells, conditional formats,
/// validations, sheet and workbook names). The second argument is the sheet index
/// (`usize::MAX` for workbook names).
pub fn transform_formulas(wb: &mut Workbook, mut f: impl FnMut(&mut Expr, usize)) {
    for (si, sh) in wb.sheets.iter_mut().enumerate() {
        crate::condfmt::map_sheet_formulas(sh, &mut |e| f(e, si));
        for (_, t) in sh.names.iter_mut() {
            if let Ok(mut e) = formula::parse(t.trim_start_matches('=')) {
                f(&mut e, si);
                *t = formula::print(&e);
            }
        }
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
    let names = std::mem::take(&mut wb.names);
    wb.names = names
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

/// Inserts (n > 0) or deletes (n < 0) rows/columns of sheet `si`, adjusting everything that
/// refers to them anywhere in the workbook.
pub fn shift_structure(wb: &mut Workbook, si: usize, rows: bool, at: u32, n: i64) {
    shift_sheet(&mut wb.sheets[si], rows, at, n);
    crate::condfmt::shift_sheet_rules(&mut wb.sheets[si], rows, at, n);
    let name = wb.sheets[si].name.clone();
    transform_formulas(wb, |e, fsi| formula::shift_for_structure(e, &name, fsi == si, rows, at, n));
    crate::chart::shift_for_structure(wb, si, rows, at, n);
    crate::pivot::shift_for_structure(&mut wb.sheets[si], rows, at, n);
}

/// What deleting a sheet named `name` does to a formula: references to it become #REF!.
pub fn drop_sheet_refs(e: &mut Expr, name: &str) {
    formula::map_refs(e, &mut |r| !r.sheet.as_ref().is_some_and(|s| s.eq_ignore_ascii_case(name)));
}

/// Removes sheet `i`; formulas that referred to it get #REF!.
pub fn delete_sheet(wb: &mut Workbook, i: usize) {
    let name = wb.sheets[i].name.clone();
    wb.sheets.remove(i);
    transform_formulas(wb, |e, _| drop_sheet_refs(e, &name));
}

/// Renames sheet `i` and every reference to it (formulas, charts, PivotTables).
pub fn rename_sheet(wb: &mut Workbook, i: usize, new: &str) {
    let old = wb.sheets[i].name.clone();
    wb.sheets[i].name = new.to_string();
    let newn = new.to_string();
    transform_formulas(wb, |e, _| formula::rename_sheet(e, &old, &newn));
    crate::chart::rename_sheet(wb, &old, &newn);
    crate::pivot::rename_sheet(wb, &old, &newn);
}
