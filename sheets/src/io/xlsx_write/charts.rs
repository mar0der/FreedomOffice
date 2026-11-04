//! Writes sheet charts as native xlsx charts.

use rust_xlsxwriter::{
    Chart as XChart, ChartDataLabel, ChartFormat, ChartLegendPosition, ChartMarker, ChartType, Worksheet, XlsxError,
};

use crate::chart::{self, ChartKind};
use crate::model::{Range, Sheet, Workbook, MAX_COLS, MAX_ROWS};

fn chart_type(kind: ChartKind) -> ChartType {
    match kind {
        ChartKind::Column => ChartType::Column,
        ChartKind::Bar => ChartType::Bar,
        ChartKind::Line => ChartType::Line,
        ChartKind::Pie => ChartType::Pie,
        ChartKind::Doughnut => ChartType::Doughnut,
        ChartKind::Area => ChartType::Area,
        ChartKind::Scatter => ChartType::Scatter,
    }
}

fn rng(sheet: &str, r: Range) -> (&str, u32, u16, u32, u16) {
    (sheet, r.r1, r.c1 as u16, r.r2, r.c2 as u16)
}

pub(super) fn write(wb: &Workbook, sh: &Sheet, ws: &mut Worksheet) -> Result<(), XlsxError> {
    // The writer keys charts by position, so charts stacked exactly on top of each
    // other are nudged apart by a pixel instead of being dropped.
    let mut used = std::collections::HashSet::new();
    for ch in &sh.charts {
        let data = chart::resolve(wb, ch);
        if data.series.is_empty() || ch.anchor.0 >= MAX_ROWS || ch.anchor.1 >= MAX_COLS {
            continue;
        }
        let Some(si) = wb.sheet_index(&ch.sheet) else { continue };
        let src = wb.sheets[si].name.as_str();
        let mut xc = XChart::new(chart_type(ch.kind));
        for s in &data.series {
            let ser = xc.add_series();
            ser.set_values(rng(src, s.refr));
            if let Some(cr) = data.cat_ref {
                ser.set_categories(rng(src, cr));
            }
            if let Some((r, c)) = s.name_cell {
                ser.set_name((src, r, c as u16));
            }
            if ch.data_labels {
                ser.set_data_label(ChartDataLabel::new().show_value());
            }
            if ch.kind == ChartKind::Line {
                ser.set_marker(ChartMarker::new().set_automatic());
            }
        }
        match &ch.title {
            Some(t) if t.is_empty() => {
                xc.title().set_hidden();
            }
            Some(t) => {
                xc.title().set_name(t.as_str());
            }
            // An empty title element: the automatic title.
            None => {
                xc.title().set_format(ChartFormat::new().set_no_fill());
            }
        }
        if ch.legend {
            xc.legend().set_position(ChartLegendPosition::Bottom);
        } else {
            xc.legend().set_hidden();
        }
        xc.set_width(ch.size.0.round().max(1.0) as u32).set_height(ch.size.1.round().max(1.0) as u32);
        let (mut x, y) = (ch.offset.0.round().max(0.0) as u32, ch.offset.1.round().max(0.0) as u32);
        while !used.insert((ch.anchor, x, y)) {
            x += 1;
        }
        ws.insert_chart_with_offset(ch.anchor.0, ch.anchor.1 as u16, &xc, x, y)?;
    }
    Ok(())
}
