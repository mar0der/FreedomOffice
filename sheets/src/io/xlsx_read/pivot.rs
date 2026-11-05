//! PivotTables from xlsx: each pivot table part and its cache (definition and records) become
//! a report of the model, laid out again once the workbook is loaded.

use std::collections::HashMap;
use std::io::{Read, Seek};

use quick_xml::events::Event;
use quick_xml::Reader;

use super::{attrs, dir_of, file_of, iso_to_serial, local, parse_rels, resolve, Package};
use crate::model::{ErrorKind, Range, Value};
use crate::pivot::{ikey, Agg, BaseItem, Cache, DatePart, FieldSource, Form, PivotField, PivotTable, ShowAs, SortOrder, VALUES};

#[derive(Default, Debug)]
struct El {
    name: String,
    attrs: Vec<(String, String)>,
    kids: Vec<El>,
}

impl El {
    fn child(&self, n: &str) -> Option<&El> {
        self.kids.iter().find(|k| k.name == n)
    }
    fn all<'a>(&'a self, n: &'a str) -> impl Iterator<Item = &'a El> + 'a {
        self.kids.iter().filter(move |k| k.name == n)
    }
    fn attr(&self, n: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str())
    }
    fn flag(&self, n: &str, default: bool) -> bool {
        self.attr(n).map_or(default, |v| v == "1" || v.eq_ignore_ascii_case("true"))
    }
    fn num(&self, n: &str) -> Option<f64> {
        self.attr(n).and_then(|v| v.trim().parse().ok())
    }
    fn deep<'a>(&'a self, n: &str) -> Option<&'a El> {
        self.kids.iter().find_map(|k| if k.name == n { Some(k) } else { k.deep(n) })
    }
}

fn dom(xml: &str) -> Option<El> {
    let mut rd = Reader::from_str(xml);
    let mut stack: Vec<El> = vec![El::default()];
    loop {
        match rd.read_event() {
            Ok(Event::Start(e)) => stack.push(El { name: local(&e).to_string(), attrs: attrs(&e), ..Default::default() }),
            Ok(Event::Empty(e)) => {
                let el = El { name: local(&e).to_string(), attrs: attrs(&e), ..Default::default() };
                stack.last_mut()?.kids.push(el);
            }
            Ok(Event::End(_)) => {
                let el = stack.pop()?;
                stack.last_mut()?.kids.push(el);
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    let mut root = stack.into_iter().next()?;
    root.kids.pop()
}

/// Custom number formats of styles.xml by id.
pub(super) fn num_formats(styles: &str) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    if let Some(root) = dom(styles) {
        if let Some(nf) = root.child("numFmts") {
            for f in nf.all("numFmt") {
                if let (Some(id), Some(code)) = (f.num("numFmtId"), f.attr("formatCode")) {
                    out.insert(id as u32, code.to_string());
                }
            }
        }
    }
    out
}

fn fmt_code(id: Option<f64>, fmts: &HashMap<u32, String>) -> String {
    let Some(id) = id.map(|i| i as u32) else { return String::new() };
    if id == 0 {
        return String::new();
    }
    fmts.get(&id).cloned().or_else(|| crate::numfmt::builtin_format(id).map(|s| s.to_string())).unwrap_or_default()
}

/// A cache item element as a value.
fn item_value(e: &El) -> Value {
    let v = e.attr("v").unwrap_or("");
    match e.name.as_str() {
        "n" => v.trim().parse().map(Value::Number).unwrap_or(Value::Empty),
        "s" => Value::Text(v.to_string()),
        "b" => Value::Bool(v == "1" || v.eq_ignore_ascii_case("true")),
        "e" => ErrorKind::parse(v).map(Value::Error).unwrap_or(Value::Error(ErrorKind::NA)),
        "d" => iso_to_serial(v).map(Value::Number).unwrap_or(Value::Empty),
        _ => Value::Empty,
    }
}

struct CacheField {
    name: String,
    database: bool,
    shared: Vec<Value>,
    fmt: String,
    /// fieldGroup: base field, date part (groupBy), numeric range, group item labels.
    base: Option<usize>,
    part: Option<DatePart>,
    range: Option<(f64, f64, f64)>,
    groups: Vec<String>,
}

fn cache_fields(def: &El, fmts: &HashMap<u32, String>) -> Vec<CacheField> {
    let mut out = Vec::new();
    let Some(cf) = def.child("cacheFields") else { return out };
    for f in cf.all("cacheField") {
        let si = f.child("sharedItems");
        let shared: Vec<Value> = si.map(|s| s.kids.iter().map(item_value).collect()).unwrap_or_default();
        let date = si.is_some_and(|s| s.flag("containsDate", false) && !s.flag("containsNonDate", true)) || shared.iter().any(|v| matches!(v, Value::Number(_))) && si.is_some_and(|s| s.kids.iter().any(|k| k.name == "d"));
        let mut fmt = fmt_code(f.num("numFmtId"), fmts);
        if date && !crate::numfmt::is_date_format(&fmt) {
            fmt = "m/d/yyyy".into();
        }
        let fg = f.child("fieldGroup");
        let rp = fg.and_then(|g| g.child("rangePr"));
        let part = rp.and_then(|r| r.attr("groupBy")).and_then(DatePart::from_xml);
        let range = match (rp, part) {
            (Some(r), None) if r.attr("groupBy").is_none_or(|g| g == "range") => match (r.num("startNum"), r.num("endNum"), r.num("groupInterval")) {
                (Some(a), Some(b), k) => Some((a, b, k.unwrap_or(1.0))),
                _ => None,
            },
            _ => None,
        };
        out.push(CacheField {
            name: f.attr("name").unwrap_or("").to_string(),
            database: f.flag("databaseField", true),
            shared,
            fmt,
            base: fg.and_then(|g| g.num("base")).map(|b| b as usize),
            part,
            range,
            groups: fg.and_then(|g| g.child("groupItems")).map(|gi| gi.kids.iter().map(|k| k.attr("v").unwrap_or("").to_string()).collect()).unwrap_or_default(),
        });
    }
    out
}

fn records(xml: &str, fields: &[CacheField], db: &[usize]) -> Vec<Vec<Value>> {
    let Some(root) = dom(xml) else { return Vec::new() };
    root.all("r")
        .map(|r| {
            r.kids
                .iter()
                .zip(db.iter())
                .map(|(k, &ci)| if k.name == "x" { k.num("v").and_then(|i| fields[ci].shared.get(i as usize).cloned()).unwrap_or_default() } else { item_value(k) })
                .collect()
        })
        .collect()
}

/// Reads one pivot table part (with its cache) of a sheet.
pub(super) fn read<R: Read + Seek>(pkg: &mut Package<'_, R>, table_path: &str, fmts: &HashMap<u32, String>) -> Option<PivotTable> {
    let table = dom(&pkg.text(table_path)?)?;
    let trels = pkg.text(&format!("{}_rels/{}.rels", dir_of(table_path), file_of(table_path))).map(|x| parse_rels(&x)).unwrap_or_default();
    let def_path = trels.iter().find(|r| r.ty.ends_with("/pivotCacheDefinition")).map(|r| resolve(&dir_of(table_path), &r.target))?;
    let def = dom(&pkg.text(&def_path)?)?;
    let crels = pkg.text(&format!("{}_rels/{}.rels", dir_of(&def_path), file_of(&def_path))).map(|x| parse_rels(&x)).unwrap_or_default();
    let fields = cache_fields(&def, fmts);
    let db: Vec<usize> = (0..fields.len()).filter(|&i| fields[i].database).collect();
    let recs = crels
        .iter()
        .find(|r| r.ty.ends_with("/pivotCacheRecords"))
        .and_then(|r| pkg.text(&resolve(&dir_of(&def_path), &r.target)))
        .map(|x| records(&x, &fields, &db))
        .unwrap_or_default();
    let ws = def.child("cacheSource").and_then(|c| c.child("worksheetSource"));
    let source = match ws {
        Some(w) => match (w.attr("name"), w.attr("ref"), w.attr("sheet")) {
            (Some(n), _, _) => n.to_string(),
            (None, Some(r), Some(s)) => format!("{}!{}", crate::formula::quote_sheet(s), abs(r)),
            (None, Some(r), None) => abs(r),
            _ => String::new(),
        },
        None => String::new(),
    };
    let cache = Cache { names: db.iter().map(|&i| fields[i].name.clone()).collect(), records: recs, formats: db.iter().map(|&i| fields[i].fmt.clone()).collect() };
    let loc = table.child("location")?;
    let rg = Range::parse(loc.attr("ref")?)?;
    let mut pt = PivotTable::new(table.attr("name").unwrap_or("PivotTable1"), &source, cache, (rg.r1, rg.c1));
    // Cache field -> model field.
    let mut map: Vec<Option<usize>> = vec![None; fields.len()];
    for (k, &ci) in db.iter().enumerate() {
        map[ci] = Some(k);
    }
    for (ci, f) in fields.iter().enumerate() {
        let col = |b: usize| db.iter().position(|x| *x == b);
        match (f.part, f.base) {
            (Some(part), Some(b)) => {
                if let Some(c) = col(b) {
                    if f.database {
                        // A date column grouped in place (older files): its parts become a field.
                        pt.fields.push(PivotField::new(&format!("{} ({})", part.label(), f.name), FieldSource::Date(c, part)));
                    } else {
                        pt.fields.push(PivotField::new(&f.name, FieldSource::Date(c, part)));
                    }
                    map[ci] = Some(pt.fields.len() - 1);
                }
            }
            (None, _) if f.database => {
                if let (Some(r), Some(k)) = (f.range, map[ci]) {
                    pt.fields[k].num_group = Some(r);
                }
            }
            _ => {}
        }
    }
    let fmap = |x: i64| -> Option<usize> { if x == -2 { Some(VALUES) } else { map.get(x as usize).copied().flatten() } };

    // Item labels of each pivotField's item list (by position), for filters and page items.
    let label_of = |ci: usize, x: usize, pt: &PivotTable| -> Option<String> {
        let f = &fields[ci];
        let mf = map[ci]?;
        if !f.groups.is_empty() {
            return f.groups.get(x).cloned();
        }
        let v = f.shared.get(x)?;
        Some(crate::pivot::engine::label_of(v, pt.cache.formats.get(pt.fields[mf].column()).map(|s| s.as_str()).unwrap_or("")))
    };
    let mut item_lists: Vec<Vec<String>> = vec![Vec::new(); fields.len()];
    let mut first_row: Option<(bool, bool, bool)> = None;
    if let Some(pfs) = table.child("pivotFields") {
        for (ci, pf) in pfs.all("pivotField").enumerate() {
            let Some(mf) = map.get(ci).copied().flatten() else { continue };
            if let Some(items) = pf.child("items") {
                for it in items.all("item") {
                    if it.attr("t").is_some_and(|t| t != "data") {
                        continue;
                    }
                    let Some(x) = it.num("x") else { continue };
                    let Some(label) = label_of(ci, x as usize, &pt) else { continue };
                    let k = ikey(&label);
                    if it.flag("h", false) {
                        pt.fields[mf].hidden.insert(k.clone());
                    }
                    if !it.flag("sd", true) {
                        pt.fields[mf].collapsed.insert(k.clone());
                    }
                    item_lists[ci].push(label);
                }
            }
            let fl = &mut pt.fields[mf];
            if let Some(n) = pf.attr("name") {
                fl.caption = n.to_string();
            }
            fl.subtotal = pf.flag("defaultSubtotal", true);
            fl.sort = match pf.attr("sortType") {
                Some("descending") => SortOrder::Desc,
                Some("ascending") => SortOrder::Asc,
                // Manual order: the item list as saved, which new reports keep sorted.
                _ => SortOrder::Asc,
            };
            if let Some(sc) = pf.child("autoSortScope") {
                if let Some(x) = sc.deep("reference").filter(|r| r.attr("field") == Some("4294967294")).and_then(|r| r.child("x")).and_then(|x| x.num("v")) {
                    fl.sort_by = Some(x as usize);
                }
            }
            if pf.attr("axis") == Some("axisRow") && first_row.is_none() {
                first_row = Some((pf.flag("compact", true), pf.flag("outline", true), pf.flag("subtotalTop", true)));
            }
            if pf.attr("axis") == Some("axisRow") && pf.flag("insertBlankRow", false) {
                pt.blank_rows = true;
            }
            if pf.deep("pivotField").is_some_and(|x| x.flag("fillDownLabels", false)) {
                pt.repeat_labels = true;
            }
        }
    }
    let axis = |tag: &str| -> Vec<usize> {
        table.child(tag).map(|e| e.all("field").filter_map(|f| f.num("x")).filter_map(|x| fmap(x as i64)).collect()).unwrap_or_default()
    };
    pt.rows = axis("rowFields");
    pt.cols = axis("colFields");
    if let Some(pfs) = table.child("pageFields") {
        for p in pfs.all("pageField") {
            let Some(ci) = p.num("fld").map(|x| x as usize) else { continue };
            let Some(mf) = map.get(ci).copied().flatten() else { continue };
            pt.filters.push(mf);
            if let Some(i) = p.num("item") {
                pt.fields[mf].page = item_lists[ci].get(i as usize).map(|l| ikey(l));
            }
        }
    }
    if let Some(dfs) = table.child("dataFields") {
        for d in dfs.all("dataField") {
            let Some(f) = d.num("fld").and_then(|x| fmap(x as i64)).filter(|f| *f != VALUES) else { continue };
            let mut show_as = ShowAs::from_xml(d.attr("showDataAs").unwrap_or("normal"));
            if let Some(s) = d.deep("dataField").and_then(|x| x.attr("pivotShowAs")) {
                show_as = ShowAs::from_xml(s);
            }
            let bf_ci = d.num("baseField").unwrap_or(0.0) as usize;
            let base_field = map.get(bf_ci).copied().flatten().unwrap_or(0);
            let base_item = match d.num("baseItem").map(|x| x as u64).filter(|_| show_as.needs_item()) {
                Some(1048828) => BaseItem::Prev,
                Some(1048829) => BaseItem::Next,
                Some(i) => item_lists.get(bf_ci).and_then(|l| l.get(i as usize)).map(|l| BaseItem::Item(l.clone())).unwrap_or_default(),
                None => BaseItem::Prev,
            };
            let agg = Agg::from_xml(d.attr("subtotal").unwrap_or("sum"));
            let name = d.attr("name").map(|s| s.to_string()).unwrap_or_else(|| pt.data_name(agg, f, None));
            pt.values.push(crate::pivot::DataField { field: f, name, agg, show_as, base_field, base_item, num_fmt: fmt_code(d.num("numFmtId"), fmts) });
        }
    }
    // A single data field never shows Σ Values on an axis.
    pt.fix_values_field();
    let (compact, outline, top) = first_row.unwrap_or((true, true, true));
    pt.form = if compact && table.flag("compact", true) {
        Form::Compact
    } else if outline {
        Form::Outline
    } else {
        Form::Tabular
    };
    pt.subtotals_top = top;
    // rowGrandTotals is the total row at the bottom (Excel's ColumnGrand), colGrandTotals the column.
    pt.col_grand = table.flag("rowGrandTotals", true);
    pt.row_grand = table.flag("colGrandTotals", true);
    pt.show_buttons = table.flag("showDrill", true);
    pt.field_headers = table.flag("showHeaders", true);
    if let Some(c) = table.attr("dataCaption") {
        pt.data_caption = c.to_string();
    }
    if table.flag("showMissing", true) {
        pt.empty_text = table.attr("missingCaption").unwrap_or("").to_string();
    }
    if let Some(si) = table.child("pivotTableStyleInfo") {
        pt.style = si.attr("name").unwrap_or("").to_string();
        pt.row_headers = si.flag("showRowHeaders", true);
        pt.col_headers = si.flag("showColHeaders", true);
        pt.banded_rows = si.flag("showRowStripes", false);
        pt.banded_cols = si.flag("showColStripes", false);
    }
    // The location excludes report filters; the anchor is the table's top-left cell.
    pt.anchor = (rg.r1, rg.c1);
    Some(pt)
}

/// "A1:E7" -> "$A$1:$E$7".
fn abs(r: &str) -> String {
    match Range::parse(r) {
        Some(rg) => {
            let p = |r: u32, c: u32| format!("${}${}", crate::model::col_name(c), r + 1);
            format!("{}:{}", p(rg.r1, rg.c1), p(rg.r2, rg.c2))
        }
        None => r.to_string(),
    }
}
