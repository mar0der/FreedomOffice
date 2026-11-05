//! .xlsx writer built on rust_xlsxwriter.

mod charts;
mod extras;
mod fidelity;
mod filter;
mod page;

use std::collections::HashMap;
use std::path::Path;

use rust_xlsxwriter::{
    Color, Format, FormatAlign, FormatBorder, FormatPattern, FormatUnderline, Formula as XFormula,
    Workbook as XWorkbook, Worksheet, XlsxError,
};

use crate::formula;
use crate::model::{BorderLine, Cell, HAlign, Range, Sheet, Style, VAlign, Value, Workbook, MAX_COLS, MAX_ROWS};

const MAX_STR: usize = 32_767;

pub fn write(wb: &Workbook, path: &Path) -> Result<(), String> {
    let mut explosions = Vec::new();
    let mut book = build(wb, &mut explosions).map_err(|e| e.to_string())?;
    let bytes = book.save_to_buffer().map_err(|e| format!("Could not save: {e}"))?;
    let bytes = filter::patch(wb, bytes)?;
    let bytes = fidelity::patch_protection(wb, bytes)?;
    let bytes = charts::patch_explosions(bytes, &explosions)?;
    std::fs::write(path, bytes).map_err(|e| format!("Could not save: {e}"))
}

fn build(wb: &Workbook, explosions: &mut Vec<charts::Explosions>) -> Result<XWorkbook, XlsxError> {
    let mut book = XWorkbook::new();
    let mut formats: HashMap<u32, Format> = HashMap::new();
    let mut used: Vec<String> = Vec::new();
    for (i, sh) in wb.sheets.iter().enumerate() {
        let ws = book.add_worksheet();
        ws.set_name(safe_sheet_name(&sh.name, i, &mut used))?;
        write_sheet(wb, sh, ws, &mut formats, explosions)?;
        if i == wb.active {
            ws.set_active(true);
        } else if sh.hidden {
            ws.set_hidden(true);
        }
    }
    for (name, text) in &wb.names {
        // Invalid names are skipped rather than failing the whole save. Parsed definitions
        // are written in file form (`_xlfn.LAMBDA(_xlpm.x,...)` for named lambdas).
        let text = text.trim_start_matches('=');
        let text = formula::parse(text).map(|e| formula::print_xlsx(&e)).unwrap_or_else(|_| text.to_string());
        let _ = book.define_name(name, &format!("={text}"));
    }
    // Sheet-scoped names are written as `Sheet!Name`.
    for (sh, written) in wb.sheets.iter().zip(&used) {
        for (name, text) in &sh.names {
            let _ = book.define_name(format!("{}!{}", formula::quote_sheet(written), name), &format!("={}", text.trim_start_matches('=')));
        }
    }
    Ok(book)
}

fn format_for<'a>(wb: &Workbook, formats: &'a mut HashMap<u32, Format>, id: u32) -> &'a Format {
    formats.entry(id).or_insert_with(|| to_format(wb.style(id)))
}

fn write_sheet(wb: &Workbook, sh: &Sheet, ws: &mut Worksheet, formats: &mut HashMap<u32, Format>, explosions: &mut Vec<charts::Explosions>) -> Result<(), XlsxError> {
    for level in 1..=7u8 {
        for (a, b) in Sheet::outline_runs(&sh.row_levels, level) {
            if b < MAX_ROWS {
                let _ = ws.group_rows(a, b);
            }
        }
        for (a, b) in Sheet::outline_runs(&sh.col_levels, level) {
            if b < MAX_COLS {
                let _ = ws.group_columns(a as u16, b as u16);
            }
        }
    }
    let mut cols: Vec<_> = sh.col_w.iter().filter(|(c, _)| **c < MAX_COLS).collect();
    cols.sort_by_key(|(c, _)| **c);
    for (&c, &w) in cols {
        let px = w.round().max(0.0) as u32;
        if px > 0 {
            ws.set_column_width_pixels(c as u16, px)?;
        }
    }
    for &c in sh.hidden_cols.iter().filter(|c| **c < MAX_COLS) {
        ws.set_column_hidden(c as u16)?;
    }
    for (&r, &h) in sh.row_h.iter().filter(|(r, _)| **r < MAX_ROWS) {
        let px = h.round().max(0.0) as u32;
        if px > 0 {
            ws.set_row_height_pixels(r, px)?;
        }
    }
    for &r in sh.hidden_rows.iter().filter(|r| **r < MAX_ROWS) {
        ws.set_row_hidden(r)?;
    }
    let mut notes: Vec<_> = sh.notes.iter().filter(|((r, c), _)| *r < MAX_ROWS && *c < MAX_COLS).collect();
    notes.sort_by_key(|(k, _)| **k);
    for (&(r, c), text) in notes {
        let note = match sh.note_authors.get(&(r, c)) {
            Some(a) if !a.is_empty() => rust_xlsxwriter::Note::new(text).set_author(a.as_str()),
            _ => rust_xlsxwriter::Note::new(text).add_author_prefix(false),
        };
        ws.insert_note(r, c as u16, &note)?;
    }
    // Tables go in before the cells, whose values and formats then replace the defaults.
    fidelity::write_tables(sh, ws);
    let headers = fidelity::header_cells(sh);

    // Merges first: merge_range writes the top-left cell and blank-fills the rest,
    // the real cell contents are written over them below.
    // Overlapping merges are dropped, Excel rejects them.
    let mut done: Vec<Range> = Vec::new();
    for m in &sh.merges {
        if m.is_single() || m.r2 >= MAX_ROWS || m.c2 >= MAX_COLS || done.iter().any(|d| d.intersects(m)) {
            continue;
        }
        done.push(*m);
        let style = sh.get(m.r1, m.c1).map(|c| c.style).unwrap_or(0);
        let fmt = format_for(wb, formats, style).clone();
        ws.merge_range(m.r1, m.c1 as u16, m.r2, m.c2 as u16, "", &fmt)?;
    }

    for (&r, row) in sh.rows.range(..MAX_ROWS) {
        for (&c, cell) in row {
            if c >= MAX_COLS {
                continue;
            }
            let fmt = format_for(wb, formats, cell.style);
            if let Some(name) = headers.get(&(r, c)) {
                ws.write_string_with_format(r, c as u16, name, fmt)?;
                continue;
            }
            if let Some(rt) = sh.rich_at(r, c) {
                if fidelity::write_rich(ws, r, c as u16, rt, wb.style(cell.style), fmt)? {
                    continue;
                }
            }
            let spill = sh.spill_range(r, c).filter(|(_, blocked)| !blocked).map(|(rg, _)| rg);
            match file_cell(sh, r, c, cell) {
                Some(fc) => write_cell(ws, r, c as u16, &fc, fmt, spill)?,
                None => write_cell(ws, r, c as u16, cell, fmt, spill)?,
            }
        }
    }
    // Cached values of dynamic-array spills (not stored as real cells).
    for (&(r, c), (_, v)) in &sh.spill.cells {
        if sh.get(r, c).is_none() && c < MAX_COLS {
            let fmt = format_for(wb, formats, 0);
            write_cell(ws, r, c as u16, &Cell { value: v.clone(), formula: None, style: 0 }, fmt, None)?;
        }
    }

    let mut links: Vec<_> = sh.links.iter().filter(|((r, c), _)| *r < MAX_ROWS && *c < MAX_COLS).collect();
    links.sort_by_key(|(k, _)| **k);
    for (&(r, c), target) in links {
        let (text, style) = match sh.get(r, c) {
            Some(cell) => (crate::numfmt::format_value(&cell.value, &wb.style(cell.style).num_fmt).text, cell.style),
            None => (target.clone(), 0),
        };
        let url = match target.strip_prefix('#') {
            Some(loc) => format!("internal:{loc}"),
            None => target.clone(),
        };
        let fmt = format_for(wb, formats, style).clone();
        let link = rust_xlsxwriter::Url::new(url).set_text(if text.is_empty() { target.clone() } else { text });
        // Links that the writer rejects (unusual schemes, too long) are skipped rather than failing the save.
        let _ = ws.write_url_with_format(r, c as u16, link, &fmt);
    }

    if sh.freeze != (0, 0) {
        ws.set_freeze_panes(sh.freeze.0.min(MAX_ROWS - 1), sh.freeze.1.min(MAX_COLS - 1) as u16)?;
    }
    if !sh.gridlines {
        ws.set_screen_gridlines(false);
    }
    if let Some(c) = sh.tab_color {
        ws.set_tab_color(Color::RGB(c & 0xFFFFFF));
    }
    // A sheet AutoFilter may not overlap a table (tables carry their own).
    if !sh.filter.as_ref().is_some_and(|f| fidelity::writable_tables(sh).iter().any(|t| t.range.intersects(&f.range))) {
        filter::write_range(sh, ws)?;
    }
    if (sh.zoom - 1.0).abs() > 0.004 {
        ws.set_zoom((sh.zoom * 100.0).round().clamp(10.0, 400.0) as u16);
    }
    fidelity::write_protection(sh, ws);
    fidelity::write_images(sh, ws);
    charts::write(wb, sh, ws, explosions)?;
    extras::write_sheet_extras(sh, ws);
    page::write(&sh.page, ws);
    Ok(())
}

/// A copy of a formula cell with its structured references in file form (`Table1[[#This Row],[Col]]`),
/// or None when nothing needs rewriting.
fn file_cell(sh: &Sheet, r: u32, c: u32, cell: &Cell) -> Option<Cell> {
    let f = cell.formula.as_ref()?;
    if !f.text.contains('[') {
        return None;
    }
    let mut ast = f.ast.clone()?;
    let table = crate::table::at(sh, r, c).map(|i| sh.tables[i].name.clone());
    crate::table::visit_names(&mut ast, &mut |n| {
        if crate::table::is_structured(n) {
            *n = crate::table::file_form(n, table.as_deref());
        }
    });
    Some(Cell { value: cell.value.clone(), formula: Some(Box::new(crate::model::Formula::from_ast(ast))), style: cell.style })
}

fn result_text(v: &Value) -> String {
    match v {
        Value::Empty => String::new(),
        Value::Number(n) => format!("{n}"),
        Value::Text(s) => s.clone(),
        Value::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
        Value::Error(e) => e.as_str().into(),
    }
}

fn write_cell(ws: &mut Worksheet, r: u32, c: u16, cell: &Cell, fmt: &Format, spill: Option<Range>) -> Result<(), XlsxError> {
    if let Some(f) = &cell.formula {
        // Normalized text (upper-case function names) so Excel and the future-function
        // escaping in rust_xlsxwriter both recognise it.
        let text = match &f.ast {
            Some(ast) => formula::print_xlsx(ast),
            None => f.text.clone(),
        };
        let xf = XFormula::new(&text).set_result(result_text(&cell.value));
        match spill {
            Some(rg) => ws.write_dynamic_array_formula_with_format(rg.r1, rg.c1 as u16, rg.r2, rg.c2 as u16, xf, fmt)?,
            None => ws.write_formula_with_format(r, c, xf, fmt)?,
        };
        return Ok(());
    }
    match &cell.value {
        Value::Empty => {
            ws.write_blank(r, c, fmt)?;
        }
        Value::Number(n) => {
            ws.write_number_with_format(r, c, if n.is_finite() { *n } else { 0.0 }, fmt)?;
        }
        Value::Text(s) => {
            let s = if s.len() > MAX_STR {
                let mut end = MAX_STR;
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                &s[..end]
            } else {
                s.as_str()
            };
            ws.write_string_with_format(r, c, s, fmt)?;
        }
        Value::Bool(b) => {
            ws.write_boolean_with_format(r, c, *b, fmt)?;
        }
        Value::Error(e) => {
            // No plain error cell type in rust_xlsxwriter: store as an error-literal formula,
            // which the reader turns back into a plain error value.
            let xf = XFormula::new(e.as_str()).set_result(e.as_str());
            ws.write_formula_with_format(r, c, xf, fmt)?;
        }
    }
    Ok(())
}

fn border(b: BorderLine) -> FormatBorder {
    match b {
        BorderLine::None => FormatBorder::None,
        BorderLine::Thin => FormatBorder::Thin,
        BorderLine::Medium => FormatBorder::Medium,
        BorderLine::Thick => FormatBorder::Thick,
        BorderLine::Double => FormatBorder::Double,
        BorderLine::Dashed => FormatBorder::Dashed,
        BorderLine::Dotted => FormatBorder::Dotted,
    }
}

fn to_format(s: &Style) -> Format {
    let mut f = Format::new();
    if let Some(name) = &s.font {
        f = f.set_font_name(name);
    }
    if s.size10 != 0 {
        f = f.set_font_size(s.size10 as f64 / 10.0);
    }
    if s.bold {
        f = f.set_bold();
    }
    if s.italic {
        f = f.set_italic();
    }
    if s.underline {
        f = f.set_underline(FormatUnderline::Single);
    }
    if s.strike {
        f = f.set_font_strikethrough();
    }
    if let Some(c) = s.color {
        f = f.set_font_color(Color::RGB(c & 0xFFFFFF));
    }
    if let Some(c) = s.fill {
        f = f.set_pattern(FormatPattern::Solid).set_background_color(Color::RGB(c & 0xFFFFFF));
    }
    match s.halign {
        HAlign::General => {}
        HAlign::Left => f = f.set_align(FormatAlign::Left),
        HAlign::Center => f = f.set_align(FormatAlign::Center),
        HAlign::Right => f = f.set_align(FormatAlign::Right),
    }
    match s.valign {
        VAlign::Bottom => {}
        VAlign::Top => f = f.set_align(FormatAlign::Top),
        VAlign::Center => f = f.set_align(FormatAlign::VerticalCenter),
    }
    if s.wrap {
        f = f.set_text_wrap();
    }
    if s.indent > 0 {
        f = f.set_indent(s.indent);
    }
    match s.rotation {
        0 => {}
        r @ 1..=90 => f = f.set_rotation(r as i16),
        r @ 91..=180 => f = f.set_rotation(-((r - 90) as i16)),
        _ => f = f.set_rotation(270),
    }
    if !s.num_fmt.is_empty() && !s.num_fmt.eq_ignore_ascii_case("general") {
        f = f.set_num_format(&s.num_fmt);
    }
    if s.unlocked {
        f = f.set_unlocked();
    }
    if s.hide_formula {
        f = f.set_hidden();
    }
    let b = s.border;
    if b.left != BorderLine::None {
        f = f.set_border_left(border(b.left));
    }
    if b.right != BorderLine::None {
        f = f.set_border_right(border(b.right));
    }
    if b.top != BorderLine::None {
        f = f.set_border_top(border(b.top));
    }
    if b.bottom != BorderLine::None {
        f = f.set_border_bottom(border(b.bottom));
    }
    f
}

/// A sheet name the file format accepts: no []:*?/\\, at most 31 chars, unique (case-insensitive).
fn safe_sheet_name(name: &str, index: usize, used: &mut Vec<String>) -> String {
    let mut base: String = name.chars().map(|c| if "[]:*?/\\".contains(c) { '_' } else { c }).collect();
    base = base.trim_matches('\'').to_string();
    if base.trim().is_empty() {
        base = format!("Sheet{}", index + 1);
    }
    let cut = |s: &str, n: usize| s.chars().take(n).collect::<String>();
    let mut candidate = cut(&base, 31);
    let mut k = 2;
    while used.iter().any(|u| u.eq_ignore_ascii_case(&candidate)) {
        let suffix = format!(" ({k})");
        candidate = format!("{}{}", cut(&base, 31 - suffix.chars().count()), suffix);
        k += 1;
    }
    used.push(candidate.clone());
    candidate
}

#[cfg(test)]
mod name_tests {
    #[test]
    fn sheet_names_are_made_valid() {
        let mut used = Vec::new();
        assert_eq!(super::safe_sheet_name("Q1/Q2 [draft]", 0, &mut used), "Q1_Q2 _draft_");
        assert_eq!(super::safe_sheet_name("A very long sheet name that exceeds the limit", 1, &mut used).chars().count(), 31);
        assert_eq!(super::safe_sheet_name("q1_q2 _draft_", 2, &mut used), "q1_q2 _draft_ (2)");
    }
}
