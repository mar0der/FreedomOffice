//! Reads AutoFilter criteria (`<filterColumn>`) from a worksheet part.

use std::collections::BTreeSet;

use quick_xml::events::Event;
use quick_xml::Reader;

use super::{attr, attr_bool, local, skip};
use crate::condfmt::DiffFormat;
use crate::filter::{ColumnFilter, Condition, CustomOp, DateGroup, Dynamic};
use crate::model::Sheet;

fn is_end(e: &quick_xml::events::BytesEnd, name: &str) -> bool {
    let n: &str = e.name().0;
    n.rsplit(':').next() == Some(name)
}

pub(super) fn parse_autofilter(xml: &str, dxfs: &[DiffFormat], sheet: &mut Sheet) {
    if !xml.contains("filterColumn") {
        return;
    }
    let Some(f) = sheet.filter.as_mut() else { return };
    let c1 = f.range.c1;
    let mut rd = Reader::from_str(xml);
    let mut in_af = false;
    let mut col: Option<u32> = None;
    let mut cur: Option<ColumnFilter> = None;
    // <customFilters and=..>: (and, conditions)
    let mut custom: Option<(bool, Vec<Condition>)> = None;
    loop {
        let ev = rd.read_event();
        match ev {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let is_start = matches!(ev, Ok(Event::Start(_)));
                let n = local(e);
                if !in_af {
                    match n {
                        "sheetData" | "sheetViews" | "cols" if is_start => skip(&mut rd, e),
                        "autoFilter" => in_af = is_start,
                        _ => {}
                    }
                    continue;
                }
                match n {
                    "filterColumn" => {
                        col = attr(e, "colId").and_then(|v| v.parse::<u32>().ok()).map(|i| c1 + i);
                        cur = None;
                        custom = None;
                    }
                    "filters" => {
                        cur = Some(ColumnFilter::Values { items: BTreeSet::new(), blanks: attr_bool(e, "blank").unwrap_or(false), dates: BTreeSet::new() });
                    }
                    "filter" => {
                        if let (Some(ColumnFilter::Values { items, .. }), Some(v)) = (cur.as_mut(), attr(e, "val")) {
                            items.insert(v.to_lowercase());
                        }
                    }
                    "dateGroupItem" => {
                        if let Some(ColumnFilter::Values { dates, .. }) = cur.as_mut() {
                            let num = |k: &str| attr(e, k).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
                            let grouping = attr(e, "dateTimeGrouping").unwrap_or_default();
                            let (month, day) = match grouping.as_str() {
                                "year" => (0, 0),
                                "month" => (num("month"), 0),
                                _ => (num("month"), num("day")),
                            };
                            dates.insert(DateGroup { year: num("year") as i32, month: month.clamp(0, 12) as u8, day: day.clamp(0, 31) as u8 });
                        }
                    }
                    "customFilters" => custom = Some((attr_bool(e, "and").unwrap_or(false), Vec::new())),
                    "customFilter" => {
                        if let Some((_, conds)) = custom.as_mut() {
                            let op = attr(e, "operator").and_then(|o| CustomOp::from_xml(&o)).unwrap_or(CustomOp::Equal);
                            conds.push(Condition { op, val: attr(e, "val").unwrap_or_default() });
                        }
                    }
                    "top10" => {
                        let top = attr_bool(e, "top").unwrap_or(true);
                        let percent = attr_bool(e, "percent").unwrap_or(false);
                        let val = attr(e, "val").and_then(|v| v.parse::<f64>().ok()).unwrap_or(10.0);
                        cur = Some(ColumnFilter::Top10 { top, percent, val });
                    }
                    "dynamicFilter" => {
                        cur = attr(e, "type").and_then(|t| Dynamic::from_xml(&t)).map(ColumnFilter::Dynamic);
                    }
                    "colorFilter" => {
                        let cell = attr_bool(e, "cellColor").unwrap_or(true);
                        let dxf = attr(e, "dxfId").and_then(|v| v.parse::<usize>().ok()).and_then(|i| dxfs.get(i)).cloned().unwrap_or_default();
                        cur = Some(ColumnFilter::Color { font: !cell, color: if cell { dxf.fill } else { dxf.color } });
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                if in_af && is_end(e, "filterColumn") {
                    if let Some((and, mut conds)) = custom.take() {
                        if !conds.is_empty() {
                            let c1 = conds.remove(0);
                            cur = Some(ColumnFilter::Custom { and, c1, c2: conds.into_iter().next() });
                        }
                    }
                    if let (Some(c), Some(cf)) = (col.take(), cur.take()) {
                        f.columns.insert(c, cf);
                    }
                } else if is_end(e, "autoFilter") {
                    break;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}
