//! AutoFilter criteria in xlsx. rust_xlsxwriter writes the range, the `_FilterDatabase`
//! name and `filterMode`; it has no Top 10, dynamic or colour filters, so the finished
//! package is patched: each sheet's `<autoFilter>` is replaced with the full criteria and
//! the colour filters' differential formats are appended to styles.xml.

use std::io::{Cursor, Read, Write};

use rust_xlsxwriter::{FilterCondition, Worksheet, XlsxError};

use crate::filter::{AutoFilter, ColumnFilter};
use crate::model::{Sheet, Workbook, MAX_COLS, MAX_ROWS};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn argb(c: u32) -> String {
    format!("FF{:06X}", c & 0xFFFFFF)
}

/// Registers the range (and placeholder criteria so `filterMode` is set) with the writer.
pub(super) fn write_range(sh: &Sheet, ws: &mut Worksheet) -> Result<(), XlsxError> {
    let Some(f) = &sh.filter else { return Ok(()) };
    let rg = f.range;
    if rg.r2 >= MAX_ROWS || rg.c2 >= MAX_COLS {
        return Ok(());
    }
    ws.autofilter(rg.r1, rg.c1 as u16, rg.r2, rg.c2 as u16)?;
    if !f.columns.is_empty() {
        // Rows are hidden from the model's own evaluation, not rust_xlsxwriter's.
        ws.filter_automatic_off();
        for &c in f.columns.keys().filter(|c| **c >= rg.c1 && **c <= rg.c2) {
            ws.filter_column(c as u16, &FilterCondition::new().add_list_filter("x"))?;
        }
    }
    Ok(())
}

/// The `<autoFilter>` element; colour filters add `<dxf>` entries numbered from `dxf_base`.
pub fn autofilter_xml(f: &AutoFilter, dxf_base: usize, dxfs: &mut Vec<String>) -> String {
    let mut x = format!("<autoFilter ref=\"{}\"", f.range.to_a1());
    let cols: Vec<_> = f.columns.iter().filter(|(c, _)| **c >= f.range.c1 && **c <= f.range.c2).collect();
    if cols.is_empty() {
        x.push_str("/>");
        return x;
    }
    x.push('>');
    for (&c, cf) in cols {
        x.push_str(&format!("<filterColumn colId=\"{}\">", c - f.range.c1));
        match cf {
            ColumnFilter::Values { items, blanks, dates } => {
                x.push_str(if *blanks { "<filters blank=\"1\">" } else { "<filters>" });
                for v in items {
                    x.push_str(&format!("<filter val=\"{}\"/>", esc(v)));
                }
                for g in dates {
                    let (mut attrs, grouping) = (format!("year=\"{}\"", g.year), if g.month == 0 { "year" } else if g.day == 0 { "month" } else { "day" });
                    if g.month > 0 {
                        attrs.push_str(&format!(" month=\"{}\"", g.month));
                    }
                    if g.day > 0 {
                        attrs.push_str(&format!(" day=\"{}\"", g.day));
                    }
                    x.push_str(&format!("<dateGroupItem {attrs} dateTimeGrouping=\"{grouping}\"/>"));
                }
                x.push_str("</filters>");
            }
            ColumnFilter::Custom { and, c1, c2 } => {
                x.push_str(if *and && c2.is_some() { "<customFilters and=\"1\">" } else { "<customFilters>" });
                for c in std::iter::once(c1).chain(c2.iter()) {
                    let op = crate::filter::CustomOp::xml(c.op);
                    if op == "equal" {
                        x.push_str(&format!("<customFilter val=\"{}\"/>", esc(&c.val)));
                    } else {
                        x.push_str(&format!("<customFilter operator=\"{op}\" val=\"{}\"/>", esc(&c.val)));
                    }
                }
                x.push_str("</customFilters>");
            }
            ColumnFilter::Top10 { top, percent, val } => {
                x.push_str(&format!(
                    "<top10{}{} val=\"{}\"/>",
                    if *top { "" } else { " top=\"0\"" },
                    if *percent { " percent=\"1\"" } else { "" },
                    val
                ));
            }
            ColumnFilter::Dynamic(d) => x.push_str(&format!("<dynamicFilter type=\"{}\"/>", d.xml())),
            ColumnFilter::Color { font, color } => {
                let dxf = match (font, color) {
                    (false, Some(c)) => format!(
                        "<dxf><fill><patternFill patternType=\"solid\"><fgColor rgb=\"{0}\"/><bgColor rgb=\"{0}\"/></patternFill></fill></dxf>",
                        argb(*c)
                    ),
                    (false, None) => "<dxf><fill><patternFill patternType=\"none\"/></fill></dxf>".to_string(),
                    (true, Some(c)) => format!("<dxf><font><color rgb=\"{}\"/></font></dxf>", argb(*c)),
                    (true, None) => "<dxf><font><color auto=\"1\"/></font></dxf>".to_string(),
                };
                let id = dxf_base + dxfs.len();
                dxfs.push(dxf);
                x.push_str(&format!("<colorFilter dxfId=\"{id}\"{}/>", if *font { " cellColor=\"0\"" } else { "" }));
            }
        }
        x.push_str("</filterColumn>");
    }
    x.push_str("</autoFilter>");
    x
}

/// Replaces the `<autoFilter>` element of a worksheet part.
fn replace_autofilter(xml: &str, new: &str) -> Option<String> {
    let a = xml.find("<autoFilter")?;
    let tag_end = a + xml[a..].find('>')?;
    let end = if xml[..tag_end].ends_with('/') { tag_end + 1 } else { a + xml[a..].find("</autoFilter>")? + "</autoFilter>".len() };
    Some(format!("{}{}{}", &xml[..a], new, &xml[end..]))
}

/// The worksheet part with the full `<autoFilter>`; its colour filters' dxfs are added to
/// `dxfs` only when the sheet really has an autoFilter to replace.
fn patch_sheet(xml: &str, f: &AutoFilter, base: usize, dxfs: &mut Vec<String>) -> Option<String> {
    let mut mine = Vec::new();
    let new = replace_autofilter(xml, &autofilter_xml(f, base + dxfs.len(), &mut mine))?;
    dxfs.extend(mine);
    Some(new)
}

/// Appends dxf entries to styles.xml; returns the patched text.
fn add_dxfs(styles: &str, dxfs: &[String]) -> Option<String> {
    if dxfs.is_empty() {
        return Some(styles.to_string());
    }
    let a = styles.find("<dxfs")?;
    let tag_end = a + styles[a..].find('>')?;
    let open = &styles[a..=tag_end];
    let count: usize = open.split("count=\"").nth(1).and_then(|r| r.split('"').next()).and_then(|n| n.parse().ok()).unwrap_or(0);
    let body = dxfs.concat();
    let new_open = format!("<dxfs count=\"{}\">", count + dxfs.len());
    Some(if open.ends_with("/>") {
        format!("{}{}{}</dxfs>{}", &styles[..a], new_open, body, &styles[tag_end + 1..])
    } else {
        let close = styles.find("</dxfs>")?;
        format!("{}{}{}{}{}", &styles[..a], new_open, &styles[tag_end + 1..close], body, &styles[close..])
    })
}

fn dxf_count(styles: &str) -> usize {
    styles
        .find("<dxfs")
        .and_then(|a| styles[a..].split("count=\"").nth(1))
        .and_then(|r| r.split('"').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Rewrites the package with full filter criteria; returns it unchanged when no sheet has any.
pub(super) fn patch(wb: &Workbook, pkg: Vec<u8>) -> Result<Vec<u8>, String> {
    let filtered: Vec<(usize, &AutoFilter)> = wb
        .sheets
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.filter.as_ref().filter(|f| !f.columns.is_empty() && f.range.r2 < MAX_ROWS && f.range.c2 < MAX_COLS).map(|f| (i, f)))
        .collect();
    // Tables keep their own criteria in their table part, found by name.
    let tables: Vec<(String, AutoFilter)> = wb
        .sheets
        .iter()
        .flat_map(super::fidelity::writable_tables)
        .filter(|t| t.filter && t.header && !t.criteria.is_empty())
        .map(|t| (format!(" displayName=\"{}\"", esc(&t.name)), t.autofilter()))
        .collect();
    if filtered.is_empty() && tables.is_empty() {
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
    let styles_ix = parts.iter().position(|p| p.0 == "xl/styles.xml");
    let base = styles_ix.map(|i| dxf_count(&String::from_utf8_lossy(&parts[i].1))).unwrap_or(0);
    let mut dxfs = Vec::new();
    for (si, f) in filtered {
        let name = format!("xl/worksheets/sheet{}.xml", si + 1);
        if let Some(p) = parts.iter_mut().find(|p| p.0 == name) {
            if let Some(new) = patch_sheet(&String::from_utf8_lossy(&p.1), f, base, &mut dxfs) {
                p.1 = new.into_bytes();
            }
        }
    }
    for p in parts.iter_mut().filter(|p| p.0.starts_with("xl/tables/table") && p.0.ends_with(".xml")) {
        let xml = String::from_utf8_lossy(&p.1).to_string();
        if let Some((_, f)) = tables.iter().find(|(key, _)| xml.contains(key.as_str())) {
            if let Some(new) = patch_sheet(&xml, f, base, &mut dxfs) {
                p.1 = new.into_bytes();
            }
        }
    }
    if let Some(i) = styles_ix {
        let xml = String::from_utf8_lossy(&parts[i].1).to_string();
        if let Some(new) = add_dxfs(&xml, &dxfs) {
            parts[i].1 = new.into_bytes();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{Condition, CustomOp, DateGroup, Dynamic};
    use crate::model::{Cell, Range, Style, Value};

    #[test]
    fn patch_helpers() {
        assert_eq!(replace_autofilter("<a/><autoFilter ref=\"A1:B2\"/><b/>", "<X/>").unwrap(), "<a/><X/><b/>");
        assert_eq!(replace_autofilter("<autoFilter ref=\"A1\"><f/></autoFilter><b/>", "<X/>").unwrap(), "<X/><b/>");
        let s = add_dxfs("<x><dxfs count=\"0\"/></x>", &["<dxf/>".into()]).unwrap();
        assert_eq!(s, "<x><dxfs count=\"1\"><dxf/></dxfs></x>");
        let s = add_dxfs("<x><dxfs count=\"1\"><dxf>a</dxf></dxfs></x>", &["<dxf/>".into()]).unwrap();
        assert_eq!(s, "<x><dxfs count=\"2\"><dxf>a</dxf><dxf/></dxfs></x>");
        assert_eq!(dxf_count(&s), 2);
    }

    /// A table's own AutoFilter criteria survive a save / load (in the table part).
    #[test]
    fn table_filter_round_trip() {
        let mut wb = Workbook::new();
        let yellow = wb.intern_style(Style { fill: Some(0xFFFF00), ..Default::default() });
        let s = &mut wb.sheets[0];
        let put = |s: &mut Sheet, r: u32, c: u32, v: Value, style: u32| s.set(r, c, Some(Cell { value: v, formula: None, style }));
        put(s, 0, 1, Value::Text("Rep".into()), 0);
        put(s, 0, 2, Value::Text("Qty".into()), 0);
        for r in 1..=5u32 {
            put(s, r, 1, Value::Text(format!("r{r}")), if r % 2 == 1 { yellow } else { 0 });
            put(s, r, 2, Value::Number(r as f64), 0);
        }
        let mut t = crate::table::Table::new("Sales", Range::new(0, 1, 5, 2), "TableStyleMedium2");
        t.columns[0].name = "Rep".into();
        t.columns[1].name = "Qty".into();
        t.criteria.insert(2, ColumnFilter::Top10 { top: true, percent: false, val: 3.0 });
        t.criteria.insert(1, ColumnFilter::Color { font: false, color: Some(0xFFFF00) });
        let vis = crate::filter::row_visibility(&wb, &wb.sheets[0], &t.autofilter(), 0.0);
        wb.sheets[0].hidden_rows = vis.iter().filter(|v| !v.1).map(|v| v.0).collect();
        wb.sheets[0].tables.push(t.clone());
        let dir = std::env::temp_dir().join(format!("freedom_sheets_tfilter_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("table_filter.xlsx");
        crate::io::save(&wb, &path).unwrap();
        let back = crate::io::load(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let bt = &back.sheets[0].tables[0];
        assert_eq!(bt.criteria, t.criteria);
        assert!(back.sheets[0].filter.is_none());
        assert_eq!(back.sheets[0].hidden_rows, wb.sheets[0].hidden_rows);
        assert_eq!(back.sheets[0].hidden_rows.len(), 3);
    }

    #[test]
    fn colour_filter_dxfs_only_for_sheets_with_an_autofilter() {
        let mut f = AutoFilter::new(Range::new(0, 0, 5, 1));
        f.columns.insert(1, ColumnFilter::Color { font: false, color: Some(0xFFFF00) });
        let mut dxfs = Vec::new();
        // The writer skipped this sheet's autoFilter: nothing to patch, no orphan dxf.
        assert!(patch_sheet("<worksheet><sheetData/></worksheet>", &f, 3, &mut dxfs).is_none());
        assert!(dxfs.is_empty());
        let xml = patch_sheet("<worksheet><autoFilter ref=\"A1:B6\"/></worksheet>", &f, 3, &mut dxfs).unwrap();
        assert!(xml.contains("<colorFilter dxfId=\"3\"/>"));
        assert_eq!(dxfs.len(), 1);
        let xml = patch_sheet("<worksheet><autoFilter ref=\"A1:B6\"/></worksheet>", &f, 3, &mut dxfs).unwrap();
        assert!(xml.contains("<colorFilter dxfId=\"4\"/>"));
    }

    /// Criteria and hidden rows survive a save / load, and openpyxl (when installed) reads
    /// the same `<filterColumn>` elements Excel would.
    #[test]
    fn filter_round_trip() {
        let mut wb = Workbook::new();
        let yellow = wb.intern_style(Style { fill: Some(0xFFFF00), ..Default::default() });
        let red = wb.intern_style(Style { color: Some(0xFF0000), ..Default::default() });
        let date = wb.intern_style(Style { num_fmt: "m/d/yyyy".into(), ..Default::default() });
        let s = &mut wb.sheets[0];
        let put = |s: &mut Sheet, r: u32, c: u32, v: Value, style: u32| s.set(r, c, Some(Cell { value: v, formula: None, style }));
        for (c, h) in ["Region", "Qty", "Score", "When", "Flag", "Ink"].iter().enumerate() {
            put(s, 0, c as u32, Value::Text(h.to_string()), 0);
        }
        for r in 1..=6u32 {
            put(s, r, 0, Value::Text(if r % 2 == 0 { "East" } else { "West" }.into()), 0);
            put(s, r, 1, Value::Number(r as f64 * 10.0), 0);
            put(s, r, 2, Value::Number(r as f64), 0);
            put(s, r, 3, Value::Number(crate::numfmt::ymd_to_serial(2024, r as i64, 15)), date);
            put(s, r, 4, Value::Text("x".into()), if r < 4 { yellow } else { 0 });
            put(s, r, 5, Value::Text("y".into()), if r > 3 { red } else { 0 });
        }
        let mut f = AutoFilter::new(Range::new(0, 0, 6, 5));
        f.columns.insert(
            0,
            ColumnFilter::Values {
                items: ["east".to_string()].into(),
                blanks: true,
                dates: Default::default(),
            },
        );
        f.columns.insert(
            1,
            ColumnFilter::Custom {
                and: true,
                c1: Condition { op: CustomOp::GreaterEq, val: "20".into() },
                c2: Some(Condition { op: CustomOp::NotEqual, val: "*5*".into() }),
            },
        );
        f.columns.insert(2, ColumnFilter::Top10 { top: false, percent: true, val: 50.0 });
        f.columns.insert(
            3,
            ColumnFilter::Values {
                items: Default::default(),
                blanks: false,
                dates: [DateGroup { year: 2024, month: 2, day: 0 }, DateGroup { year: 2024, month: 4, day: 15 }, DateGroup { year: 2023, month: 0, day: 0 }].into(),
            },
        );
        f.columns.insert(4, ColumnFilter::Color { font: false, color: Some(0xFFFF00) });
        f.columns.insert(5, ColumnFilter::Color { font: true, color: Some(0xFF0000) });
        s.filter = Some(f.clone());
        let vis = crate::filter::row_visibility(&wb, &wb.sheets[0], &f, 0.0);
        for (r, v) in vis {
            if !v {
                wb.sheets[0].hidden_rows.insert(r);
            }
        }
        let hidden = wb.sheets[0].hidden_rows.clone();

        let dir = std::env::temp_dir().join(format!("freedom_sheets_filter_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("filters.xlsx");
        crate::io::save(&wb, &path).unwrap();
        let back = crate::io::load(&path).unwrap();
        let bf = back.sheets[0].filter.clone().unwrap();
        assert_eq!(bf, f);
        assert_eq!(back.sheets[0].hidden_rows, hidden);

        // a dynamic filter on its own
        wb.sheets[0].filter.as_mut().unwrap().columns = [(2, ColumnFilter::Dynamic(Dynamic::AboveAverage)), (3, ColumnFilter::Dynamic(Dynamic::Month(3)))].into();
        let path2 = dir.join("dynamic.xlsx");
        crate::io::save(&wb, &path2).unwrap();
        assert_eq!(crate::io::load(&path2).unwrap().sheets[0].filter, wb.sheets[0].filter);

        let script = r#"
import sys, openpyxl
ws = openpyxl.load_workbook(sys.argv[1]).active
af = ws.auto_filter
assert af.ref == "A1:F7", af.ref
cols = {fc.colId: fc for fc in af.filterColumn}
assert sorted(cols) == [0, 1, 2, 3, 4, 5], sorted(cols)
assert [f for f in cols[0].filters.filter] == ["east"] and cols[0].filters.blank
cf = cols[1].customFilters
assert cf._and and [(c.operator, c.val) for c in cf.customFilter] == [("greaterThanOrEqual", "20"), ("notEqual", "*5*")]
t = cols[2].top10
assert not t.top and t.percent and t.val == 50
g = sorted((d.year, d.month, d.day, d.dateTimeGrouping) for d in cols[3].filters.dateGroupItem)
assert g == [(2023, None, None, "year"), (2024, 2, None, "month"), (2024, 4, 15, "day")], g
assert cols[4].colorFilter.cellColor in (None, True) and cols[5].colorFilter.cellColor is False
hidden = sorted(r for r, d in ws.row_dimensions.items() if d.hidden)
print("OK", hidden)
"#;
        let py = dir.join("check_filters.py");
        std::fs::write(&py, script).unwrap();
        match std::process::Command::new("python").arg(&py).arg(&path).output() {
            Ok(out) if String::from_utf8_lossy(&out.stderr).contains("No module named 'openpyxl'") => {}
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                assert!(out.status.success(), "openpyxl check failed: {}{}", text, String::from_utf8_lossy(&out.stderr));
                let mut want: Vec<u32> = hidden.iter().map(|r| r + 1).collect();
                want.sort_unstable();
                assert_eq!(text.trim(), format!("OK {want:?}"));
            }
            Err(_) => {} // no Python on this machine
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
