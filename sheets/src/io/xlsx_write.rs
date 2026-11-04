//! .xlsx writer built on rust_xlsxwriter.

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
    let mut book = build(wb).map_err(|e| e.to_string())?;
    book.save(path).map_err(|e| format!("Could not save: {e}"))
}

fn build(wb: &Workbook) -> Result<XWorkbook, XlsxError> {
    let mut book = XWorkbook::new();
    let mut formats: HashMap<u32, Format> = HashMap::new();
    for (i, sh) in wb.sheets.iter().enumerate() {
        let ws = book.add_worksheet();
        ws.set_name(&sh.name)?;
        write_sheet(wb, sh, ws, &mut formats)?;
        if i == wb.active {
            ws.set_active(true);
        }
    }
    for (name, text) in &wb.names {
        // Invalid names are skipped rather than failing the whole save.
        let _ = book.define_name(name, &format!("={}", text.trim_start_matches('=')));
    }
    Ok(book)
}

fn format_for<'a>(wb: &Workbook, formats: &'a mut HashMap<u32, Format>, id: u32) -> &'a Format {
    formats.entry(id).or_insert_with(|| to_format(wb.style(id)))
}

fn write_sheet(wb: &Workbook, sh: &Sheet, ws: &mut Worksheet, formats: &mut HashMap<u32, Format>) -> Result<(), XlsxError> {
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
        ws.insert_note(r, c as u16, &rust_xlsxwriter::Note::new(text).add_author_prefix(false))?;
    }

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

    for (&r, row) in &sh.rows {
        for (&c, cell) in row {
            if c >= MAX_COLS {
                continue;
            }
            let fmt = format_for(wb, formats, cell.style);
            write_cell(ws, r, c as u16, cell, fmt)?;
        }
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
    if let Some(f) = &sh.filter {
        let rg = f.range;
        if rg.r2 < MAX_ROWS && rg.c2 < MAX_COLS {
            ws.autofilter(rg.r1, rg.c1 as u16, rg.r2, rg.c2 as u16)?;
        }
    }
    Ok(())
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

fn write_cell(ws: &mut Worksheet, r: u32, c: u16, cell: &Cell, fmt: &Format) -> Result<(), XlsxError> {
    if let Some(f) = &cell.formula {
        // Normalized text (upper-case function names) so Excel and the future-function
        // escaping in rust_xlsxwriter both recognise it.
        let text = match &f.ast {
            Some(ast) => formula::print(ast),
            None => f.text.clone(),
        };
        let xf = XFormula::new(&text).set_result(result_text(&cell.value));
        ws.write_formula_with_format(r, c, xf, fmt)?;
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
    if !s.num_fmt.is_empty() && !s.num_fmt.eq_ignore_ascii_case("general") {
        f = f.set_num_format(&s.num_fmt);
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
