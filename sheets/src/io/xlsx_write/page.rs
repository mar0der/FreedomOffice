//! Page setup: orientation, paper, margins, scaling, print options, print area/titles,
//! header/footer and manual page breaks. Values the writer rejects are skipped.

use rust_xlsxwriter::Worksheet;

use crate::model::{MAX_COLS, MAX_ROWS};
use crate::page::{Margins, PageSetup, Scaling};

pub(super) fn write(p: &PageSetup, ws: &mut Worksheet) {
    if p.landscape {
        ws.set_landscape();
    }
    // Always explicit: a missing paperSize means Letter, while the default here follows the locale.
    ws.set_paper_size(p.paper.code() as u8);
    if p.margins != Margins::NORMAL {
        let m = p.margins;
        ws.set_margins(m.left as f64, m.right as f64, m.top as f64, m.bottom as f64, m.header as f64, m.footer as f64);
    }
    match p.scaling {
        Scaling::Percent(100) => {}
        Scaling::Percent(n) => {
            ws.set_print_scale(n.clamp(10, 400));
        }
        Scaling::Fit { wide, tall } => {
            ws.set_print_fit_to_pages(wide, tall);
        }
    }
    if p.center_h {
        ws.set_print_center_horizontally(true);
    }
    if p.center_v {
        ws.set_print_center_vertically(true);
    }
    if p.gridlines {
        ws.set_print_gridlines(true);
    }
    if p.headings {
        ws.set_print_headings(true);
    }
    if p.black_white {
        ws.set_print_black_and_white(true);
    }
    if let Some(a) = p.print_area {
        if a.r2 < MAX_ROWS && a.c2 < MAX_COLS {
            let _ = ws.set_print_area(a.r1, a.c1 as u16, a.r2, a.c2 as u16);
        }
    }
    if let Some((a, b)) = p.title_rows {
        let _ = ws.set_repeat_rows(a, b);
    }
    if let Some((a, b)) = p.title_cols {
        if b < MAX_COLS {
            let _ = ws.set_repeat_columns(a as u16, b as u16);
        }
    }
    if !p.header.is_empty() {
        ws.set_header(p.header.to_xlsx());
    }
    if !p.footer.is_empty() {
        ws.set_footer(p.footer.to_xlsx());
    }
    if let Some(n) = p.first_page.filter(|&n| n > 0) {
        ws.set_print_first_page_number(n.min(u16::MAX as u32) as u16);
    }
    if p.over_then_down {
        ws.set_page_order(false);
    }
    if !p.row_breaks.is_empty() {
        let rows: Vec<u32> = p.row_breaks.iter().copied().collect();
        let _ = ws.set_page_breaks(&rows);
    }
    if !p.col_breaks.is_empty() {
        let cols: Vec<u32> = p.col_breaks.iter().copied().collect();
        let _ = ws.set_vertical_page_breaks(&cols);
    }
}
