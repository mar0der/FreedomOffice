//! PivotTables in xlsx. rust_xlsxwriter has no PivotTables, so the finished package is
//! patched: each report gets a pivot cache (definition and records) and a pivot table part,
//! linked from its sheet, the workbook and [Content_Types].xml. The cache asks Excel to
//! refresh on open, so Excel rebuilds the report from the source data it finds there.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};

use crate::model::{Value, Workbook};
use crate::pivot::engine::{field_items, Item, Key};
use crate::pivot::{BaseItem, DatePart, FieldSource, Form, PivotTable, SortOrder, VALUES};

const NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const NS_R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const X14: &str = "http://schemas.microsoft.com/office/spreadsheetml/2009/9/main";

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\n' => o.push_str("&#10;"),
            '\r' => o.push_str("&#13;"),
            '\t' => o.push_str("&#9;"),
            c if (c as u32) < 0x20 => {}
            c => o.push(c),
        }
    }
    o
}

fn num(n: f64) -> String {
    if n.is_finite() { format!("{n}") } else { "0".into() }
}

/// Excel date-time text of a serial date ("2023-01-15T00:00:00").
fn iso(serial: f64) -> String {
    let (y, m, d) = crate::numfmt::serial_to_ymd(serial);
    let secs = ((serial - serial.floor()) * 86400.0).round() as i64;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// A value as a shared or inline cache item.
fn item_xml(v: &Value, date: bool) -> String {
    match v {
        Value::Empty => "<m/>".into(),
        Value::Number(n) if date && *n >= 1.0 => format!("<d v=\"{}\"/>", iso(*n)),
        Value::Number(n) => format!("<n v=\"{}\"/>", num(*n)),
        Value::Text(s) => format!("<s v=\"{}\"/>", esc(s)),
        Value::Bool(b) => format!("<b v=\"{}\"/>", *b as u8),
        Value::Error(e) => format!("<e v=\"{}\"/>", esc(e.as_str())),
    }
}

/// `sharedItems` attributes describing the kinds of values in a column.
fn shared_attrs(vals: &[&Value], date: bool) -> String {
    let (mut s, mut n, mut blank, mut other, mut ints) = (false, false, false, false, true);
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for v in vals {
        match v {
            Value::Empty => blank = true,
            Value::Text(_) => s = true,
            Value::Number(x) => {
                n = true;
                ints &= x.fract() == 0.0;
                lo = lo.min(*x);
                hi = hi.max(*x);
            }
            _ => other = true,
        }
    }
    let mut a = String::new();
    if !s && !blank && !other {
        a.push_str(" containsSemiMixedTypes=\"0\"");
    }
    if date && n && !s && !other {
        a.push_str(" containsNonDate=\"0\" containsDate=\"1\" containsString=\"0\"");
        if blank {
            a.push_str(" containsBlank=\"1\"");
        }
        a.push_str(&format!(" minDate=\"{}\" maxDate=\"{}\"", iso(lo), iso(hi + 1.0)));
        return a;
    }
    if !s {
        a.push_str(" containsString=\"0\"");
    }
    if blank {
        a.push_str(" containsBlank=\"1\"");
    }
    if n && (s || other) {
        a.push_str(" containsMixedTypes=\"1\"");
    }
    if n {
        a.push_str(" containsNumber=\"1\"");
        if ints {
            a.push_str(" containsInteger=\"1\"");
        }
        a.push_str(&format!(" minValue=\"{}\" maxValue=\"{}\"", num(lo), num(hi)));
    }
    a
}

/// Group items of a date part: "<start", the parts, ">end".
fn date_group_items(part: DatePart, lo: f64, hi: f64) -> Vec<String> {
    let fmt = |n: f64| crate::numfmt::format_value(&Value::Number(n), "m/d/yyyy").text;
    let mut v = vec![format!("<{}", fmt(lo))];
    match part {
        DatePart::Years => {
            let (y1, _, _) = crate::numfmt::serial_to_ymd(lo);
            let (y2, _, _) = crate::numfmt::serial_to_ymd(hi);
            v.extend((y1..=y2).map(|y| y.to_string()));
        }
        DatePart::Quarters => v.extend((1..=4).map(|q| format!("Qtr{q}"))),
        DatePart::Months => v.extend(["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].map(String::from)),
        DatePart::Days => {
            const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            const DAYS: [u32; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
            for (m, n) in DAYS.iter().enumerate() {
                v.extend((1..=*n).map(|d| format!("{d}-{}", M[m])));
            }
        }
    }
    v.push(format!(">{}", fmt(hi + 1.0)));
    v
}

/// Group items of a numeric grouping: "<start", the ranges, ">end".
fn num_group_items(pt: &PivotTable, f: usize, (start, end, by): (f64, f64, f64)) -> Vec<String> {
    let col = pt.fields[f].column();
    let ints = pt.cache.records.iter().all(|r| !matches!(r.get(col), Some(Value::Number(x)) if x.fract() != 0.0));
    let mut v = vec![format!("<{}", crate::numfmt::general(start))];
    let n = ((end - start) / by).floor().max(0.0) as usize + 1;
    for k in 0..n.min(10_000) {
        v.push(crate::pivot::engine::num_group_label(start, by, k as f64, ints));
    }
    v.push(crate::pivot::engine::num_group_above(start, end, by));
    v
}

/// What each field writes: the items its pivotField lists (the engine's items) with their
/// index in the shared or group items, and the shared values with each record's index.
struct FieldPlan {
    items: Vec<Item>,
    index: Vec<u32>,
    shared: Option<(Vec<Value>, Vec<u32>)>,
    /// Group item labels of a grouped field (its pivotField lists every one of them).
    groups: Option<Vec<String>>,
}

fn plan(pt: &PivotTable, f: usize, needed: bool) -> FieldPlan {
    let field = &pt.fields[f];
    let fi = field_items(pt, f);
    match (&field.source, field.num_group) {
        (FieldSource::Date(c, part), _) => {
            let nums: Vec<f64> = pt.cache.records.iter().filter_map(|r| r.get(*c).and_then(|v| v.as_number())).collect();
            let (lo, hi) = nums.iter().fold((f64::MAX, f64::MIN), |(a, b), x| (a.min(*x), b.max(*x)));
            let groups = date_group_items(*part, lo, hi);
            let index = fi.items.iter().map(|it| groups.iter().position(|g| g.eq_ignore_ascii_case(&it.label)).unwrap_or(0) as u32).collect();
            FieldPlan { items: fi.items, index, shared: None, groups: Some(groups) }
        }
        (FieldSource::Column(col), Some(g)) => {
            let groups = num_group_items(pt, f, g);
            let index = fi.items.iter().map(|it| groups.iter().position(|x| x.eq_ignore_ascii_case(&it.label)).unwrap_or(0) as u32).collect();
            // The shared items keep the raw values.
            let mut vals: Vec<Value> = Vec::new();
            let mut seen: HashMap<String, u32> = HashMap::new();
            let mut of = Vec::with_capacity(pt.cache.records.len());
            for r in &pt.cache.records {
                let v = r.get(*col).cloned().unwrap_or_default();
                let k = format!("{v:?}");
                let id = *seen.entry(k).or_insert_with(|| {
                    vals.push(v);
                    vals.len() as u32 - 1
                });
                of.push(id);
            }
            FieldPlan { items: fi.items, index, shared: Some((vals, of)), groups: Some(groups) }
        }
        _ => {
            let n = fi.items.len() as u32;
            let shared = needed.then(|| (fi.items.iter().map(|it| if it.key == Key::Blank { Value::Empty } else { it.value.clone() }).collect(), fi.of.clone()));
            FieldPlan { items: fi.items, index: (0..n).collect(), shared, groups: None }
        }
    }
}

/// Display order of a field's items (as the report sorts them).
fn order(pt: &PivotTable, f: usize, items: &[Item]) -> Vec<usize> {
    let mut ix: Vec<usize> = (0..items.len()).collect();
    let field = &pt.fields[f];
    if field.sort == SortOrder::Source && field.num_group.is_none() && matches!(field.source, FieldSource::Column(_)) {
        return ix;
    }
    ix.sort_by(|a, b| {
        let (x, y) = (&items[*a], &items[*b]);
        let blank = (x.key == Key::Blank).cmp(&(y.key == Key::Blank));
        let o = x.key.partial_cmp(&y.key).unwrap_or(std::cmp::Ordering::Equal);
        blank.then(if field.sort == SortOrder::Desc { o.reverse() } else { o })
    });
    ix
}

/// Cache definition and records of a report.
fn cache_parts(wb: &Workbook, pt: &PivotTable, si: usize, plans: &[FieldPlan]) -> (String, String) {
    let c = &pt.cache;
    let ncols = c.names.len();
    let mut x = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<pivotCacheDefinition xmlns=\"{NS}\" xmlns:r=\"{NS_R}\" r:id=\"rId1\" refreshOnLoad=\"1\" refreshedBy=\"FreedomOffice\" refreshedDate=\"{}\" createdVersion=\"8\" refreshedVersion=\"8\" minRefreshableVersion=\"3\" recordCount=\"{}\">",
        num(crate::numfmt::now_serial()),
        c.records.len()
    );
    let src = pt.source.trim();
    if crate::table::find(wb, src).is_some() || wb.names.iter().any(|(n, _)| n.eq_ignore_ascii_case(src)) {
        x.push_str(&format!("<cacheSource type=\"worksheet\"><worksheetSource name=\"{}\"/></cacheSource>", esc(src)));
    } else if let Some((s, rg)) = crate::pivot::resolve_source(wb, src, si) {
        x.push_str(&format!("<cacheSource type=\"worksheet\"><worksheetSource ref=\"{}\" sheet=\"{}\"/></cacheSource>", rg.to_a1(), esc(&wb.sheets[s].name)));
    }
    x.push_str(&format!("<cacheFields count=\"{}\">", pt.fields.len()));
    for (f, field) in pt.fields.iter().enumerate() {
        match &field.source {
            FieldSource::Column(col) => {
                let date = c.is_date(*col);
                let fmt_id = if date { " numFmtId=\"14\"" } else { " numFmtId=\"0\"" };
                x.push_str(&format!("<cacheField name=\"{}\"{fmt_id}>", esc(&field.name)));
                let vals: Vec<&Value> = c.records.iter().map(|r| r.get(*col).unwrap_or(&Value::Empty)).collect();
                let attrs = shared_attrs(&vals, date);
                match &plans[f].shared {
                    Some((vals, _)) => {
                        x.push_str(&format!("<sharedItems{attrs} count=\"{}\">", vals.len()));
                        for v in vals {
                            x.push_str(&item_xml(v, date));
                        }
                        x.push_str("</sharedItems>");
                    }
                    None => {
                        x.push_str(&format!("<sharedItems{attrs}/>"));
                    }
                }
                let parts: Vec<usize> = (0..pt.fields.len()).filter(|&k| matches!(pt.fields[k].source, FieldSource::Date(b, _) if b == *col)).collect();
                if let Some((start, end, by)) = field.num_group {
                    let groups = num_group_items(pt, f, (start, end, by));
                    x.push_str(&format!("<fieldGroup base=\"{f}\"><rangePr autoStart=\"0\" autoEnd=\"0\" startNum=\"{}\" endNum=\"{}\" groupInterval=\"{}\"/><groupItems count=\"{}\">", num(start), num(end), num(by), groups.len()));
                    for g in groups {
                        x.push_str(&format!("<s v=\"{}\"/>", esc(&g)));
                    }
                    x.push_str("</groupItems></fieldGroup>");
                } else if let Some(&par) = parts.iter().min_by_key(|&&k| match pt.fields[k].source {
                    FieldSource::Date(_, p) => p,
                    _ => DatePart::Days,
                }) {
                    // The base field points at its coarsest part, as Excel writes it.
                    x.push_str(&format!("<fieldGroup par=\"{par}\"/>"));
                }
                x.push_str("</cacheField>");
            }
            FieldSource::Date(col, part) => {
                let nums: Vec<f64> = c.records.iter().filter_map(|r| r.get(*col).and_then(|v| v.as_number())).collect();
                let (lo, hi) = nums.iter().fold((f64::MAX, f64::MIN), |(a, b), x| (a.min(*x), b.max(*x)));
                let (lo, hi) = if nums.is_empty() { (1.0, 1.0) } else { (lo, hi) };
                let groups = date_group_items(*part, lo, hi);
                x.push_str(&format!(
                    "<cacheField name=\"{}\" numFmtId=\"0\" databaseField=\"0\"><fieldGroup base=\"{col}\"><rangePr groupBy=\"{}\" startDate=\"{}\" endDate=\"{}\"/><groupItems count=\"{}\">",
                    esc(&field.name),
                    part.xml(),
                    iso(lo),
                    iso(hi + 1.0),
                    groups.len()
                ));
                for g in groups {
                    x.push_str(&format!("<s v=\"{}\"/>", esc(&g)));
                }
                x.push_str("</groupItems></fieldGroup></cacheField>");
            }
        }
    }
    x.push_str("</cacheFields></pivotCacheDefinition>");

    let mut rec = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<pivotCacheRecords xmlns=\"{NS}\" xmlns:r=\"{NS_R}\" count=\"{}\">", c.records.len());
    // Record item index per shared field.
    let ofs: Vec<Option<&Vec<u32>>> = (0..ncols).map(|f| plans[f].shared.as_ref().map(|s| &s.1)).collect();
    for (i, r) in c.records.iter().enumerate() {
        rec.push_str("<r>");
        for col in 0..ncols {
            match &ofs[col] {
                Some(of) => rec.push_str(&format!("<x v=\"{}\"/>", of[i])),
                None => rec.push_str(&item_xml(r.get(col).unwrap_or(&Value::Empty), c.is_date(col))),
            }
        }
        rec.push_str("</r>");
    }
    rec.push_str("</pivotCacheRecords>");
    (x, rec)
}

fn fmt_id(fmt: &str, custom: &HashMap<String, u32>) -> Option<u32> {
    if fmt.is_empty() {
        return Some(0);
    }
    (0..50u32).find(|i| crate::numfmt::builtin_format(*i) == Some(fmt)).or_else(|| custom.get(fmt).copied())
}

/// The pivot table definition.
fn table_part(pt: &PivotTable, cache_id: usize, plans: &[FieldPlan], custom_fmts: &HashMap<String, u32>) -> String {
    let o = &pt.out;
    let compact = pt.form == Form::Compact;
    let mut x = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<pivotTableDefinition xmlns=\"{NS}\" name=\"{}\" cacheId=\"{cache_id}\" applyNumberFormats=\"0\" applyBorderFormats=\"0\" applyFontFormats=\"0\" applyPatternFormats=\"0\" applyAlignmentFormats=\"0\" applyWidthHeightFormats=\"1\" dataCaption=\"{}\" updatedVersion=\"8\" minRefreshableVersion=\"3\" useAutoFormatting=\"1\" itemPrintTitles=\"1\" createdVersion=\"8\" indent=\"0\"",
        esc(&pt.name),
        esc(&pt.data_caption)
    );
    // rowGrandTotals is the total row at the bottom (Excel's ColumnGrand), colGrandTotals the column.
    if !pt.col_grand {
        x.push_str(" rowGrandTotals=\"0\"");
    }
    if !pt.row_grand {
        x.push_str(" colGrandTotals=\"0\"");
    }
    if !pt.show_buttons {
        x.push_str(" showDrill=\"0\"");
    }
    if !pt.field_headers {
        x.push_str(" showHeaders=\"0\"");
    }
    if !pt.empty_text.is_empty() {
        x.push_str(&format!(" missingCaption=\"{}\"", esc(&pt.empty_text)));
    }
    if !compact {
        x.push_str(" compact=\"0\" compactData=\"0\"");
    }
    x.push_str(" outline=\"1\" outlineData=\"1\" multipleFieldFilters=\"0\">");
    let npages = pt.filters.len();
    x.push_str(&format!(
        "<location ref=\"{}\" firstHeaderRow=\"1\" firstDataRow=\"{}\" firstDataCol=\"{}\"",
        o.range.to_a1(),
        o.header_rows.max(1),
        o.label_cols
    ));
    if npages > 0 {
        x.push_str(&format!(" rowPageCount=\"{npages}\" colPageCount=\"1\""));
    }
    x.push_str("/>");
    x.push_str(&format!("<pivotFields count=\"{}\">", pt.fields.len()));
    let mut item_pos: Vec<Vec<usize>> = Vec::new();
    for (f, field) in pt.fields.iter().enumerate() {
        let axis = if pt.rows.contains(&f) {
            Some("axisRow")
        } else if pt.cols.contains(&f) {
            Some("axisCol")
        } else if pt.filters.contains(&f) {
            Some("axisPage")
        } else {
            None
        };
        let p = &plans[f];
        let mut a = String::new();
        if !field.caption.is_empty() {
            a.push_str(&format!(" name=\"{}\"", esc(&field.caption)));
        }
        if let Some(ax) = axis {
            a.push_str(&format!(" axis=\"{ax}\""));
        }
        if pt.values.iter().any(|d| d.field == f) {
            a.push_str(" dataField=\"1\"");
        }
        if !compact {
            a.push_str(" compact=\"0\"");
        }
        if pt.form == Form::Tabular {
            a.push_str(" outline=\"0\"");
        }
        if pt.filters.contains(&f) && !field.hidden.is_empty() {
            a.push_str(" multipleItemSelectionAllowed=\"1\"");
        }
        a.push_str(" showAll=\"0\"");
        if pt.blank_rows && axis == Some("axisRow") {
            a.push_str(" insertBlankRow=\"1\"");
        }
        if (!pt.subtotals_top || pt.form == Form::Tabular) && axis == Some("axisRow") {
            a.push_str(" subtotalTop=\"0\"");
        }
        if !field.subtotal {
            a.push_str(" defaultSubtotal=\"0\"");
        }
        match (field.sort, field.sort_by.is_some() || field.sort != SortOrder::Source) {
            (SortOrder::Desc, _) => a.push_str(" sortType=\"descending\""),
            (SortOrder::Asc, true) => a.push_str(" sortType=\"ascending\""),
            _ => {}
        }
        // The item list as written: every group of a grouped field, else the items in display order.
        let written: Vec<(u32, String)> = match &p.groups {
            Some(g) => {
                let mut v: Vec<(u32, String)> = g.iter().enumerate().map(|(i, l)| (i as u32, l.clone())).collect();
                if field.sort == SortOrder::Desc {
                    v.reverse();
                }
                v
            }
            None => order(pt, f, &p.items).into_iter().map(|i| (p.index[i], p.items[i].label.clone())).collect(),
        };
        let at: HashMap<u32, usize> = written.iter().enumerate().map(|(k, (x, _))| (*x, k)).collect();
        let pos: Vec<usize> = (0..p.items.len()).map(|i| at.get(&p.index[i]).copied().unwrap_or(0)).collect();
        let used = axis.is_some() || p.shared.is_some() || p.groups.is_some();
        if !used {
            x.push_str(&format!("<pivotField{a}/>"));
            item_pos.push(pos);
            continue;
        }
        x.push_str(&format!("<pivotField{a}>"));
        let n = written.len() + field.subtotal as usize;
        x.push_str(&format!("<items count=\"{n}\">"));
        for (xi, label) in &written {
            let k = crate::pivot::ikey(label);
            let mut ia = String::new();
            if field.hidden.contains(&k) {
                ia.push_str(" h=\"1\"");
            }
            if field.collapsed.contains(&k) {
                ia.push_str(" sd=\"0\"");
            }
            x.push_str(&format!("<item x=\"{xi}\"{ia}/>"));
        }
        if field.subtotal {
            x.push_str("<item t=\"default\"/>");
        }
        x.push_str("</items>");
        if let Some(d) = field.sort_by.filter(|d| *d < pt.values.len()) {
            x.push_str(&format!(
                "<autoSortScope><pivotArea dataOnly=\"0\" outline=\"0\" fieldPosition=\"0\"><references count=\"1\"><reference field=\"4294967294\" count=\"1\" selected=\"0\"><x v=\"{d}\"/></reference></references></pivotArea></autoSortScope>"
            ));
        }
        if pt.repeat_labels && axis == Some("axisRow") {
            x.push_str(&format!("<extLst><ext uri=\"{{2946ED86-A175-432a-8AC1-64E0C546D7DE}}\" xmlns:x14=\"{X14}\"><x14:pivotField fillDownLabels=\"1\"/></ext></extLst>"));
        }
        x.push_str("</pivotField>");
        item_pos.push(pos);
    }
    x.push_str("</pivotFields>");
    let fields_xml = |tag: &str, v: &[usize]| -> String {
        if v.is_empty() {
            return String::new();
        }
        let mut s = format!("<{tag} count=\"{}\">", v.len());
        for f in v {
            s.push_str(&format!("<field x=\"{}\"/>", if *f == VALUES { -2 } else { *f as i64 }));
        }
        s.push_str(&format!("</{tag}>"));
        s
    };
    x.push_str(&fields_xml("rowFields", &pt.rows));
    x.push_str(&fields_xml("colFields", &pt.cols));
    if npages > 0 {
        x.push_str(&format!("<pageFields count=\"{npages}\">"));
        for &f in &pt.filters {
            let mut a = format!(" fld=\"{f}\"");
            if let Some(sel) = &pt.fields[f].page {
                if let Some(i) = plans[f].items.iter().position(|it| crate::pivot::ikey(&it.label) == *sel) {
                    a.push_str(&format!(" item=\"{}\"", item_pos[f][i]));
                }
            }
            x.push_str(&format!("<pageField{a} hier=\"-1\"/>"));
        }
        x.push_str("</pageFields>");
    }
    if !pt.values.is_empty() {
        x.push_str(&format!("<dataFields count=\"{}\">", pt.values.len()));
        for d in &pt.values {
            let mut a = format!(" name=\"{}\" fld=\"{}\"", esc(&d.name), d.field);
            if d.agg != crate::pivot::Agg::Sum {
                a.push_str(&format!(" subtotal=\"{}\"", d.agg.xml()));
            }
            if let Some(s) = d.show_as.xml().filter(|s| *s != "normal") {
                a.push_str(&format!(" showDataAs=\"{s}\""));
            }
            let bi = match &d.base_item {
                BaseItem::Prev => 1048828,
                BaseItem::Next => 1048829,
                BaseItem::Item(l) => plans
                    .get(d.base_field)
                    .and_then(|p| p.items.iter().position(|it| crate::pivot::ikey(&it.label) == crate::pivot::ikey(l)))
                    .map_or(0, |i| item_pos[d.base_field][i]),
            };
            let bi = if d.show_as.needs_item() { bi } else { 0 };
            a.push_str(&format!(" baseField=\"{}\" baseItem=\"{bi}\"", if d.show_as.needs_field() { d.base_field } else { 0 }));
            let fmt = if d.num_fmt.is_empty() && d.show_as.is_percent() { "0.00%" } else { d.num_fmt.as_str() };
            if let Some(id) = fmt_id(fmt, custom_fmts).filter(|i| *i != 0) {
                a.push_str(&format!(" numFmtId=\"{id}\""));
            }
            match d.show_as.x14() {
                Some(s) => x.push_str(&format!("<dataField{a}><extLst><ext uri=\"{{E15A36E0-9728-4e99-A89B-3F7291B0FE68}}\" xmlns:x14=\"{X14}\"><x14:dataField pivotShowAs=\"{s}\"/></ext></extLst></dataField>")),
                None => x.push_str(&format!("<dataField{a}/>")),
            }
        }
        x.push_str("</dataFields>");
    }
    x.push_str(&format!(
        "<pivotTableStyleInfo name=\"{}\" showRowHeaders=\"{}\" showColHeaders=\"{}\" showRowStripes=\"{}\" showColStripes=\"{}\" showLastColumn=\"1\"/>",
        esc(&pt.style),
        pt.row_headers as u8,
        pt.col_headers as u8,
        pt.banded_rows as u8,
        pt.banded_cols as u8
    ));
    x.push_str(&format!(
        "<extLst><ext uri=\"{{962EF5D1-5CA2-4c93-8EF4-DBF5C05439D2}}\" xmlns:x14=\"{X14}\"><x14:pivotTableDefinition hideValuesRow=\"1\" xmlns:xm=\"http://schemas.microsoft.com/office/excel/2006/main\"/></ext></extLst>"
    ));
    x.push_str("</pivotTableDefinition>");
    x
}

/// Custom number formats already in styles.xml: code -> id.
fn custom_formats(styles: &str) -> HashMap<String, u32> {
    let mut out = HashMap::new();
    let mut rest = styles;
    while let Some(i) = rest.find("<numFmt ") {
        rest = &rest[i + 8..];
        let end = rest.find("/>").unwrap_or(rest.len());
        let tag = &rest[..end];
        let get = |k: &str| tag.split(&format!("{k}=\"")).nth(1).and_then(|v| v.split('"').next()).map(|v| v.to_string());
        if let (Some(id), Some(code)) = (get("numFmtId").and_then(|v| v.parse().ok()), get("formatCode")) {
            let code = quick_xml::escape::unescape(&code).map(|c| c.into_owned()).unwrap_or(code);
            out.insert(code, id);
        }
    }
    out
}

/// Adds the relationship to a .rels part (creating it when missing).
fn add_rel(parts: &mut Vec<(String, Vec<u8>)>, rels: &str, id: &str, ty: &str, target: &str) {
    let rel = format!("<Relationship Id=\"{id}\" Type=\"{REL}/{ty}\" Target=\"{target}\"/>");
    match parts.iter_mut().find(|p| p.0 == rels) {
        Some(p) => {
            let s = String::from_utf8_lossy(&p.1).to_string();
            if let Some(i) = s.rfind("</Relationships>") {
                p.1 = format!("{}{rel}{}", &s[..i], &s[i..]).into_bytes();
            }
        }
        None => parts.push((
            rels.to_string(),
            format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{rel}</Relationships>").into_bytes(),
        )),
    }
}

/// Adds the PivotTables to a saved package; returns it unchanged when there are none.
pub(super) fn patch(wb: &Workbook, pkg: Vec<u8>) -> Result<Vec<u8>, String> {
    let reports: Vec<(usize, &PivotTable)> = wb.sheets.iter().enumerate().flat_map(|(s, sh)| sh.pivots.iter().filter(|p| p.out.drawn).map(move |p| (s, p))).collect();
    if reports.is_empty() {
        return Ok(pkg);
    }
    let mut zin = zip::ZipArchive::new(Cursor::new(pkg)).map_err(|e| e.to_string())?;
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..zin.len() {
        let mut f = zin.by_index(i).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        let name = f.name().map(|n| n.to_string()).map_err(|e| e.to_string())?;
        parts.push((name, buf));
    }
    let custom = parts.iter().find(|p| p.0 == "xl/styles.xml").map(|p| custom_formats(&String::from_utf8_lossy(&p.1))).unwrap_or_default();
    let mut types = String::new();
    let mut caches = String::new();
    for (k, (si, pt)) in reports.iter().enumerate() {
        let n = k + 1;
        let needed: Vec<bool> = (0..pt.fields.len())
            .map(|f| {
                let fl = &pt.fields[f];
                pt.rows.contains(&f)
                    || pt.cols.contains(&f)
                    || pt.filters.contains(&f)
                    || !fl.hidden.is_empty()
                    || pt.fields.iter().any(|x| matches!(x.source, FieldSource::Date(c, _) if c == f))
                    || pt.values.iter().any(|d| d.base_field == f && d.show_as.needs_item())
            })
            .collect();
        let plans: Vec<FieldPlan> = (0..pt.fields.len()).map(|f| plan(pt, f, needed[f])).collect();
        let (def, rec) = cache_parts(wb, pt, *si, &plans);
        parts.push((format!("xl/pivotCache/pivotCacheDefinition{n}.xml"), def.into_bytes()));
        parts.push((format!("xl/pivotCache/pivotCacheRecords{n}.xml"), rec.into_bytes()));
        add_rel(&mut parts, &format!("xl/pivotCache/_rels/pivotCacheDefinition{n}.xml.rels"), "rId1", "pivotCacheRecords", &format!("pivotCacheRecords{n}.xml"));
        parts.push((format!("xl/pivotTables/pivotTable{n}.xml"), table_part(pt, n, &plans, &custom).into_bytes()));
        add_rel(&mut parts, &format!("xl/pivotTables/_rels/pivotTable{n}.xml.rels"), "rId1", "pivotCacheDefinition", &format!("../pivotCache/pivotCacheDefinition{n}.xml"));
        add_rel(&mut parts, &format!("xl/worksheets/_rels/sheet{}.xml.rels", si + 1), &format!("rIdPvt{n}"), "pivotTable", &format!("../pivotTables/pivotTable{n}.xml"));
        add_rel(&mut parts, "xl/_rels/workbook.xml.rels", &format!("rIdPvc{n}"), "pivotCacheDefinition", &format!("pivotCache/pivotCacheDefinition{n}.xml"));
        caches.push_str(&format!("<pivotCache cacheId=\"{n}\" r:id=\"rIdPvc{n}\"/>"));
        let ct = "application/vnd.openxmlformats-officedocument.spreadsheetml";
        types.push_str(&format!("<Override PartName=\"/xl/pivotCache/pivotCacheDefinition{n}.xml\" ContentType=\"{ct}.pivotCacheDefinition+xml\"/>"));
        types.push_str(&format!("<Override PartName=\"/xl/pivotCache/pivotCacheRecords{n}.xml\" ContentType=\"{ct}.pivotCacheRecords+xml\"/>"));
        types.push_str(&format!("<Override PartName=\"/xl/pivotTables/pivotTable{n}.xml\" ContentType=\"{ct}.pivotTable+xml\"/>"));
    }
    if let Some(p) = parts.iter_mut().find(|p| p.0 == "[Content_Types].xml") {
        let s = String::from_utf8_lossy(&p.1).to_string();
        if let Some(i) = s.rfind("</Types>") {
            p.1 = format!("{}{types}{}", &s[..i], &s[i..]).into_bytes();
        }
    }
    if let Some(p) = parts.iter_mut().find(|p| p.0 == "xl/workbook.xml") {
        let s = String::from_utf8_lossy(&p.1).to_string();
        let block = format!("<pivotCaches>{caches}</pivotCaches>");
        // pivotCaches follows calcPr and comes before any extLst.
        let at = s
            .find("<calcPr")
            .and_then(|i| s[i..].find("/>").map(|j| i + j + 2))
            .or_else(|| s.find("<extLst"))
            .or_else(|| s.rfind("</workbook>"));
        if let Some(i) = at {
            p.1 = format!("{}{block}{}", &s[..i], &s[i..]).into_bytes();
        }
    }
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in parts {
        out.start_file(name, opts).map_err(|e| e.to_string())?;
        out.write_all(&data).map_err(|e| e.to_string())?;
    }
    Ok(out.finish().map_err(|e| e.to_string())?.into_inner())
}
