//! Writes tables, pictures, sheet protection and rich text with rust_xlsxwriter.

use std::collections::HashMap;

use rust_xlsxwriter::{
    Color, Format, FormatUnderline, Image, ProtectionOptions, Table as XTable, TableColumn as XColumn, TableFunction,
    TableStyle, Worksheet, XlsxError,
};

use crate::model::{legacy_password_hash, RichText, Sheet, Style, MAX_COLS, MAX_ROWS};
use crate::table::{Table, TotalFn};

/// Header cells of the sheet's tables: position -> column name (written as text, as Excel requires).
pub(super) fn header_cells(sh: &Sheet) -> HashMap<(u32, u32), String> {
    let mut out = HashMap::new();
    for t in &sh.tables {
        if let Some(r) = t.header_row() {
            for (i, col) in t.columns.iter().enumerate() {
                out.insert((r, t.range.c1 + i as u32), col.name.clone());
            }
        }
    }
    out
}

/// Tables that can be written: inside the grid, not overlapping each other, at least one data row.
pub(super) fn writable_tables(sh: &Sheet) -> Vec<&Table> {
    let mut done: Vec<&Table> = Vec::new();
    for t in &sh.tables {
        let rg = t.range;
        let min_rows = 1 + t.header as u32 + t.totals as u32;
        if rg.r2 >= MAX_ROWS || rg.c2 >= MAX_COLS || rg.rows() < min_rows.min(2) || done.iter().any(|d| d.range.intersects(&rg)) {
            continue;
        }
        done.push(t);
    }
    done
}

fn function(f: TotalFn) -> TableFunction {
    match f {
        TotalFn::Average => TableFunction::Average,
        TotalFn::Count => TableFunction::Count,
        TotalFn::CountNums => TableFunction::CountNumbers,
        TotalFn::Max => TableFunction::Max,
        TotalFn::Min => TableFunction::Min,
        TotalFn::Sum => TableFunction::Sum,
        TotalFn::StdDev => TableFunction::StdDev,
        TotalFn::Var => TableFunction::Var,
        TotalFn::None | TotalFn::Custom => TableFunction::None,
    }
}

/// Adds the tables. Call before the cells are written: the cells then replace the header and
/// total-row values the writer puts in.
pub(super) fn write_tables(sh: &Sheet, ws: &mut Worksheet) {
    for t in writable_tables(sh) {
        let cols: Vec<XColumn> = t
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut x = XColumn::new().set_header(c.name.clone());
                if t.totals {
                    if !c.total_label.is_empty() {
                        x = x.set_total_label(c.total_label.clone());
                    } else if c.total == TotalFn::Custom {
                        // The cell keeps its own formula; mark the column as custom.
                        let f = sh
                            .get(t.range.r2, t.range.c1 + i as u32)
                            .and_then(|cell| cell.formula.as_ref())
                            .map(|f| f.text.clone())
                            .unwrap_or_default();
                        if !f.is_empty() {
                            x = x.set_total_function(TableFunction::Custom(f.as_str().into()));
                        }
                    } else {
                        x = x.set_total_function(function(c.total));
                    }
                }
                x
            })
            .collect();
        let table = XTable::new()
            .set_name(t.name.clone())
            .set_header_row(t.header)
            .set_total_row(t.totals)
            .set_banded_rows(t.banded_rows)
            .set_banded_columns(t.banded_cols)
            .set_first_column(t.first_col)
            .set_last_column(t.last_col)
            .set_autofilter(t.filter && t.header)
            .set_style(table_style(&t.style))
            .set_columns(&cols);
        let rg = t.range;
        // A table the writer rejects (e.g. an invalid name) is skipped rather than failing the save.
        let _ = ws.add_table(rg.r1, rg.c1 as u16, rg.r2, rg.c2 as u16, &table);
    }
}

/// Pictures over the grid.
pub(super) fn write_images(sh: &Sheet, ws: &mut Worksheet) {
    for p in &sh.images {
        if p.anchor.0 >= MAX_ROWS || p.anchor.1 >= MAX_COLS {
            continue;
        }
        let Ok(img) = Image::new_from_buffer(&p.data) else { continue };
        let mut img = img.set_scale_to_size(p.size.0.max(1.0) as f64, p.size.1.max(1.0) as f64, false);
        if !p.descr.is_empty() {
            img = img.set_alt_text(p.descr.clone());
        }
        let (x, y) = (p.offset.0.round().max(0.0) as u32, p.offset.1.round().max(0.0) as u32);
        let _ = ws.insert_image_with_offset(p.anchor.0, p.anchor.1 as u16, &img, x, y);
    }
}

/// A password whose legacy hash equals `hash` (the hash is all the file keeps, and the writer
/// only takes a password). Any such password unlocks the sheet in Excel.
pub(crate) fn password_for_hash(hash: u16) -> Option<String> {
    const CH: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    // Short passwords only reach the low bits of the hash; ten random characters cover all
    // 15 bits about evenly, so a match turns up after ~32k tries on average.
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..4_000_000 {
        let s: String = (0..10)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                CH[(x % CH.len() as u64) as usize] as char
            })
            .collect();
        if legacy_password_hash(&s) == hash {
            return Some(s);
        }
    }
    None
}

pub(super) fn write_protection(sh: &Sheet, ws: &mut Worksheet) {
    let Some(p) = &sh.protection else { return };
    let opts = ProtectionOptions {
        select_locked_cells: p.select_locked,
        select_unlocked_cells: p.select_unlocked,
        format_cells: p.format_cells,
        format_columns: p.format_columns,
        format_rows: p.format_rows,
        insert_columns: p.insert_columns,
        insert_rows: p.insert_rows,
        insert_links: p.insert_links,
        delete_columns: p.delete_columns,
        delete_rows: p.delete_rows,
        sort: p.sort,
        use_autofilter: p.autofilter,
        use_pivot_tables: p.pivot_tables,
        edit_scenarios: p.edit_scenarios,
        edit_objects: p.edit_objects,
        ..Default::default()
    };
    if p.hash != 0 {
        if let Some(pw) = password_for_hash(p.hash) {
            ws.protect_with_password(&pw);
        }
    }
    ws.protect_with_options(&opts);
}

/// Format of one rich-text run on top of the cell's font.
fn run_format(base: &Style, run: &crate::model::RichRun) -> Format {
    let mut f = Format::new();
    if let Some(name) = run.font.as_ref().or(base.font.as_ref()) {
        f = f.set_font_name(name);
    }
    let size = run.size.unwrap_or(base.font_size());
    if (size - crate::model::DEFAULT_FONT_SIZE).abs() > 0.01 {
        f = f.set_font_size(size as f64);
    }
    if run.bold {
        f = f.set_bold();
    }
    if run.italic {
        f = f.set_italic();
    }
    if run.underline {
        f = f.set_underline(FormatUnderline::Single);
    }
    if run.strike {
        f = f.set_font_strikethrough();
    }
    if let Some(c) = run.color.or(base.color) {
        f = f.set_font_color(Color::RGB(c & 0xFFFFFF));
    }
    f
}

/// Writes a rich-text cell; false when the writer rejects it (the caller writes plain text).
pub(super) fn write_rich(ws: &mut Worksheet, r: u32, c: u16, rt: &RichText, base: &Style, cell_fmt: &Format) -> Result<bool, XlsxError> {
    let runs: Vec<(Format, &str)> = rt.runs.iter().filter(|x| !x.text.is_empty()).map(|x| (run_format(base, x), x.text.as_str())).collect();
    if runs.is_empty() {
        return Ok(false);
    }
    let parts: Vec<(&Format, &str)> = runs.iter().map(|(f, t)| (f, *t)).collect();
    Ok(ws.write_rich_string_with_format(r, c, &parts, cell_fmt).is_ok())
}

fn table_style(name: &str) -> TableStyle {
    match name {
        "TableStyleLight1" => TableStyle::Light1,
        "TableStyleLight2" => TableStyle::Light2,
        "TableStyleLight3" => TableStyle::Light3,
        "TableStyleLight4" => TableStyle::Light4,
        "TableStyleLight5" => TableStyle::Light5,
        "TableStyleLight6" => TableStyle::Light6,
        "TableStyleLight7" => TableStyle::Light7,
        "TableStyleLight8" => TableStyle::Light8,
        "TableStyleLight9" => TableStyle::Light9,
        "TableStyleLight10" => TableStyle::Light10,
        "TableStyleLight11" => TableStyle::Light11,
        "TableStyleLight12" => TableStyle::Light12,
        "TableStyleLight13" => TableStyle::Light13,
        "TableStyleLight14" => TableStyle::Light14,
        "TableStyleLight15" => TableStyle::Light15,
        "TableStyleLight16" => TableStyle::Light16,
        "TableStyleLight17" => TableStyle::Light17,
        "TableStyleLight18" => TableStyle::Light18,
        "TableStyleLight19" => TableStyle::Light19,
        "TableStyleLight20" => TableStyle::Light20,
        "TableStyleLight21" => TableStyle::Light21,
        "TableStyleMedium1" => TableStyle::Medium1,
        "TableStyleMedium2" => TableStyle::Medium2,
        "TableStyleMedium3" => TableStyle::Medium3,
        "TableStyleMedium4" => TableStyle::Medium4,
        "TableStyleMedium5" => TableStyle::Medium5,
        "TableStyleMedium6" => TableStyle::Medium6,
        "TableStyleMedium7" => TableStyle::Medium7,
        "TableStyleMedium8" => TableStyle::Medium8,
        "TableStyleMedium9" => TableStyle::Medium9,
        "TableStyleMedium10" => TableStyle::Medium10,
        "TableStyleMedium11" => TableStyle::Medium11,
        "TableStyleMedium12" => TableStyle::Medium12,
        "TableStyleMedium13" => TableStyle::Medium13,
        "TableStyleMedium14" => TableStyle::Medium14,
        "TableStyleMedium15" => TableStyle::Medium15,
        "TableStyleMedium16" => TableStyle::Medium16,
        "TableStyleMedium17" => TableStyle::Medium17,
        "TableStyleMedium18" => TableStyle::Medium18,
        "TableStyleMedium19" => TableStyle::Medium19,
        "TableStyleMedium20" => TableStyle::Medium20,
        "TableStyleMedium21" => TableStyle::Medium21,
        "TableStyleMedium22" => TableStyle::Medium22,
        "TableStyleMedium23" => TableStyle::Medium23,
        "TableStyleMedium24" => TableStyle::Medium24,
        "TableStyleMedium25" => TableStyle::Medium25,
        "TableStyleMedium26" => TableStyle::Medium26,
        "TableStyleMedium27" => TableStyle::Medium27,
        "TableStyleMedium28" => TableStyle::Medium28,
        "TableStyleDark1" => TableStyle::Dark1,
        "TableStyleDark2" => TableStyle::Dark2,
        "TableStyleDark3" => TableStyle::Dark3,
        "TableStyleDark4" => TableStyle::Dark4,
        "TableStyleDark5" => TableStyle::Dark5,
        "TableStyleDark6" => TableStyle::Dark6,
        "TableStyleDark7" => TableStyle::Dark7,
        "TableStyleDark8" => TableStyle::Dark8,
        "TableStyleDark9" => TableStyle::Dark9,
        "TableStyleDark10" => TableStyle::Dark10,
        "TableStyleDark11" => TableStyle::Dark11,
        _ => TableStyle::None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn password_preimage() {
        for pw in ["secret", "Excel2024", "x"] {
            let h = crate::model::legacy_password_hash(pw);
            let found = super::password_for_hash(h).unwrap();
            assert_eq!(crate::model::legacy_password_hash(&found), h);
        }
        // Excel's documented example: "test" hashes to CBEB.
        assert_eq!(crate::model::legacy_password_hash("test"), 0xCBEB);
    }
}
