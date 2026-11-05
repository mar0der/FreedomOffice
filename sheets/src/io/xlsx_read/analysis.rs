//! Reads What-If data tables (`<f t="dataTable">`), scenarios, sparkline groups (x14 extension)
//! and icon set rules, including the x14 variants Excel uses for 3 Stars, 3 Triangles, 5 Boxes
//! and custom icons.

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use super::{attr, attr_bool, local, parse_color, read_text, Theme, INDEXED};
use crate::condfmt::{parse_sqref, CondFormat, DiffFormat, RuleKind};
use crate::iconset::{IconSetRule, IconStyle, Threshold, ThresholdKind};
use crate::model::{parse_cell, Range, Sheet};
use crate::sparkline::{AxisScale, EmptyAs, SparkColors, SparkKind, Sparkline, SparklineGroup};
use crate::whatif::{DataTable, Scenario};

fn is_end(e: &quick_xml::events::BytesEnd, name: &str) -> bool {
    let n: &str = e.name().0;
    n.rsplit(':').next() == Some(name)
}

/// (iconSet, showValue, reverse) of an `<iconSet>` element.
pub(super) fn icon_set_attrs(e: &BytesStart) -> (String, bool, bool) {
    (attr(e, "iconSet").unwrap_or_else(|| "3TrafficLights1".into()), attr_bool(e, "showValue").unwrap_or(true), attr_bool(e, "reverse").unwrap_or(false))
}

/// (type, val, gte) of a `<cfvo>` element.
pub(super) fn cfvo_attrs(e: &BytesStart) -> (String, String, bool) {
    (attr(e, "type").unwrap_or_default(), attr(e, "val").unwrap_or_default(), attr_bool(e, "gte").unwrap_or(true))
}

pub(super) fn icon_rule(set: Option<&(String, bool, bool)>, cfvos: &[(String, String, bool)], custom: Vec<Option<(IconStyle, u8)>>) -> Option<IconSetRule> {
    let (name, show, reverse) = set?;
    let style = IconStyle::from_xml(name)?;
    let mut rule = IconSetRule::new(style);
    if cfvos.len() == style.count() {
        rule.thresholds = cfvos
            .iter()
            .map(|(ty, val, gte)| Threshold {
                kind: ThresholdKind::from_xml(ty).unwrap_or_default(),
                value: val.trim().trim_start_matches('=').to_string(),
                gte: *gte,
            })
            .collect();
    }
    rule.icon_only = !show;
    rule.reverse = *reverse;
    if custom.len() == style.count() {
        rule.custom = custom;
    }
    Some(rule)
}

/// Icon set rules found only in `<x14:conditionalFormatting>`: (priority, rule).
pub(super) fn x14_icon_sets(xml: &str) -> Vec<(i64, CondFormat)> {
    let mut out = Vec::new();
    if !xml.contains("x14:iconSet") {
        return out;
    }
    let mut rd = Reader::from_str(xml);
    // Rules of the current x14:conditionalFormatting, waiting for its xm:sqref.
    let mut pending: Vec<(i64, IconSetRule)> = Vec::new();
    let mut cur: Option<(i64, (String, bool, bool), Vec<(String, String, bool)>, Vec<Option<(IconStyle, u8)>>, bool)> = None;
    let mut in_x14 = false;
    loop {
        let ev = rd.read_event();
        match ev {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let start = matches!(ev, Ok(Event::Start(_)));
                let qn: &str = e.name().0;
                match local(e) {
                    "sheetData" if start => {
                        let _ = rd.read_to_end(e.name());
                    }
                    "conditionalFormatting" if qn.starts_with("x14:") => {
                        in_x14 = true;
                        pending.clear();
                    }
                    "cfRule" if in_x14 => {
                        let is_icons = attr(e, "type").as_deref() == Some("iconSet");
                        let priority = attr(e, "priority").and_then(|v| v.parse().ok()).unwrap_or(i64::MAX);
                        cur = is_icons.then(|| (priority, ("3TrafficLights1".to_string(), true, false), Vec::new(), Vec::new(), false));
                    }
                    "iconSet" if in_x14 => {
                        if let Some(c) = cur.as_mut() {
                            c.1 = icon_set_attrs(e);
                            c.4 = attr_bool(e, "custom").unwrap_or(false);
                        }
                    }
                    "cfvo" if in_x14 => {
                        if let Some(c) = cur.as_mut() {
                            let (ty, _, gte) = cfvo_attrs(e);
                            let val = if start { read_text(&mut rd, "cfvo").trim().to_string() } else { attr(e, "val").unwrap_or_default() };
                            c.2.push((ty, val, gte));
                        }
                    }
                    "cfIcon" if in_x14 => {
                        if let Some(c) = cur.as_mut() {
                            let set = attr(e, "iconSet").unwrap_or_default();
                            let id: u8 = attr(e, "iconId").and_then(|v| v.parse().ok()).unwrap_or(0);
                            c.3.push(IconStyle::from_xml(&set).map(|s| (s, id)));
                        }
                    }
                    "sqref" if in_x14 && start => {
                        let sq = read_text(&mut rd, "sqref");
                        if let Some(ranges) = parse_sqref(&sq) {
                            for (p, r) in pending.drain(..) {
                                out.push((p, CondFormat::new(ranges.clone(), RuleKind::IconSet(r), DiffFormat::default())));
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                if is_end(e, "cfRule") {
                    if let Some((p, set, cfvos, icons, custom)) = cur.take() {
                        if let Some(r) = icon_rule(Some(&set), &cfvos, if custom { icons } else { Vec::new() }) {
                            pending.push((p, r));
                        }
                    }
                } else if is_end(e, "conditionalFormatting") {
                    in_x14 = false;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

fn cell_of(s: &str) -> Option<(u32, u32)> {
    parse_cell(s.trim())
}

/// Data tables, scenarios and sparklines of a worksheet part.
pub(super) fn read_sheet(xml: &str, theme: &Theme, sheet: &mut Sheet) {
    let has_dt = xml.contains("dataTable");
    let has_sc = xml.contains("<scenarios");
    let has_sp = xml.contains("sparklineGroup");
    if !has_dt && !has_sc && !has_sp {
        return;
    }
    let mut rd = Reader::from_str(xml);
    let mut cell = String::new();
    let mut tables: Vec<DataTable> = Vec::new();
    let mut group: Option<SparklineGroup> = None;
    let mut item: Option<(String, String)> = None;
    let mut in_sparklines = false;
    loop {
        let ev = rd.read_event();
        match ev {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let start = matches!(ev, Ok(Event::Start(_)));
                match local(e) {
                    "sheetData" if start && !has_dt => {
                        let _ = rd.read_to_end(e.name());
                    }
                    "c" => cell = attr(e, "r").unwrap_or_default(),
                    "f" if attr(e, "t").as_deref() == Some("dataTable") => {
                        let rg = attr(e, "ref").and_then(|r| Range::parse(&r)).or_else(|| cell_of(&cell).map(|(r, c)| Range::cell(r, c)));
                        let two = attr_bool(e, "dt2D").unwrap_or(false);
                        let row = attr_bool(e, "dtr").unwrap_or(false);
                        let r1 = attr(e, "r1").and_then(|v| cell_of(&v));
                        let r2 = attr(e, "r2").and_then(|v| cell_of(&v));
                        if let Some(range) = rg {
                            let (row_input, col_input) = if two { (r1, r2) } else if row { (r1, None) } else { (None, r1) };
                            if row_input.is_some() || col_input.is_some() {
                                tables.push(DataTable { range, row_input, col_input });
                            }
                        }
                    }
                    "scenarios" => {
                        sheet.scenarios.current = attr(e, "current").and_then(|v| v.parse().ok()).unwrap_or(0);
                        sheet.scenarios.show = attr(e, "show").and_then(|v| v.parse().ok()).unwrap_or(0);
                        sheet.scenarios.result_cells = attr(e, "sqref").and_then(|s| parse_sqref(&s)).unwrap_or_default();
                    }
                    "scenario" => sheet.scenarios.list.push(Scenario {
                        name: attr(e, "name").unwrap_or_default(),
                        comment: attr(e, "comment").unwrap_or_default(),
                        user: attr(e, "user").unwrap_or_default(),
                        locked: attr_bool(e, "locked").unwrap_or(false),
                        hidden: attr_bool(e, "hidden").unwrap_or(false),
                        cells: Vec::new(),
                    }),
                    "inputCells" => {
                        if let (Some(sc), Some(p)) = (sheet.scenarios.list.last_mut(), attr(e, "r").and_then(|r| cell_of(&r))) {
                            sc.cells.push((p, attr(e, "val").unwrap_or_default()));
                        }
                    }
                    "sparklineGroup" => {
                        let mut g = SparklineGroup::new(SparkKind::from_xml(&attr(e, "type").unwrap_or_default()), Vec::new());
                        let b = |n: &str| attr_bool(e, n).unwrap_or(false);
                        (g.high, g.low, g.first, g.last, g.negative, g.markers) = (b("high"), b("low"), b("first"), b("last"), b("negative"), b("markers"));
                        g.axis = b("displayXAxis");
                        g.hidden = b("displayHidden");
                        g.rtl = b("rightToLeft");
                        g.empty_as = EmptyAs::from_xml(&attr(e, "displayEmptyCellsAs").unwrap_or_else(|| "zero".into()));
                        g.line_weight = attr(e, "lineWeight").and_then(|v| v.parse().ok()).unwrap_or(0.75);
                        let axis = |kind: &str, manual: &str| match attr(e, kind).as_deref() {
                            Some("group") => AxisScale::Group,
                            Some("custom") => AxisScale::Custom(attr(e, manual).and_then(|v| v.parse().ok()).unwrap_or(0.0)),
                            _ => AxisScale::Individual,
                        };
                        g.min = axis("minAxisType", "manualMin");
                        g.max = axis("maxAxisType", "manualMax");
                        g.colors = SparkColors::default();
                        group = Some(g);
                    }
                    n @ ("colorSeries" | "colorNegative" | "colorAxis" | "colorMarkers" | "colorFirst" | "colorLast" | "colorHigh" | "colorLow") => {
                        if let (Some(g), Some(c)) = (group.as_mut(), parse_color(e, theme, &INDEXED)) {
                            let k = &mut g.colors;
                            *match n {
                                "colorSeries" => &mut k.series,
                                "colorNegative" => &mut k.negative,
                                "colorAxis" => &mut k.axis,
                                "colorMarkers" => &mut k.markers,
                                "colorFirst" => &mut k.first,
                                "colorLast" => &mut k.last,
                                "colorHigh" => &mut k.high,
                                _ => &mut k.low,
                            } = c;
                        }
                    }
                    "sparklines" => in_sparklines = true,
                    "sparkline" => item = Some((String::new(), String::new())),
                    "f" if start && group.is_some() => {
                        let f = read_text(&mut rd, "f").trim().to_string();
                        match item.as_mut() {
                            Some(it) if in_sparklines => it.0 = f,
                            _ => {
                                if let Some(g) = group.as_mut() {
                                    g.date_range = Some(f);
                                }
                            }
                        }
                    }
                    "sqref" if start && item.is_some() => {
                        let s = read_text(&mut rd, "sqref").trim().to_string();
                        if let Some(it) = item.as_mut() {
                            it.1 = s;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                if is_end(e, "sparkline") {
                    if let (Some((data, loc)), Some(g)) = (item.take(), group.as_mut()) {
                        if let Some(p) = cell_of(&loc) {
                            if !data.is_empty() {
                                g.items.push(Sparkline { loc: p, data });
                            }
                        }
                    }
                } else if is_end(e, "sparklines") {
                    in_sparklines = false;
                } else if is_end(e, "sparklineGroup") {
                    if let Some(g) = group.take().filter(|g| !g.items.is_empty()) {
                        sheet.sparklines.push(g);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    // Data table cells get their {=TABLE()} formulas back; the saved results stay as values.
    for t in tables {
        let text = t.formula_text();
        for r in t.range.r1..=t.range.r2 {
            for c in t.range.c1..=t.range.c2 {
                let cur = sheet.get(r, c).cloned().unwrap_or_default();
                sheet.set(r, c, Some(crate::model::Cell { formula: Some(Box::new(crate::model::Formula::new(&text))), ..cur }));
            }
        }
        sheet.data_tables.push(t);
    }
}
