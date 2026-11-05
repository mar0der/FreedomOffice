//! Writes sparklines (through rust_xlsxwriter) and patches the finished package with what it
//! cannot write: What-If data tables (`<f t="dataTable">`), scenarios and hidden Solver names.

use std::io::{Cursor, Read, Write};

use rust_xlsxwriter::{ChartEmptyCells, Color, Sparkline as XSparkline, SparklineType, Worksheet};

use crate::model::{cell_name, Range, Sheet, Workbook, MAX_COLS, MAX_ROWS};
use crate::sparkline::{split_ref, AxisScale, EmptyAs, SparkKind, SparklineGroup};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn styled(g: &SparklineGroup) -> XSparkline {
    let c = |v: u32| Color::RGB(v & 0xFFFFFF);
    let k = &g.colors;
    // The colour setters also switch their points on, so the flags come after them.
    let mut s = XSparkline::new()
        .set_type(match g.kind {
            SparkKind::Line => SparklineType::Line,
            SparkKind::Column => SparklineType::Column,
            SparkKind::WinLoss => SparklineType::WinLose,
        })
        .set_sparkline_color(c(k.series))
        .set_negative_points_color(c(k.negative))
        .set_markers_color(c(k.markers))
        .set_first_point_color(c(k.first))
        .set_last_point_color(c(k.last))
        .set_high_point_color(c(k.high))
        .set_low_point_color(c(k.low))
        .show_high_point(g.high)
        .show_low_point(g.low)
        .show_first_point(g.first)
        .show_last_point(g.last)
        .show_negative_points(g.negative)
        .show_markers(g.markers)
        .show_axis(g.axis)
        .show_hidden_data(g.hidden)
        .set_right_to_left(g.rtl)
        .show_empty_cells_as(match g.empty_as {
            EmptyAs::Gap => ChartEmptyCells::Gaps,
            EmptyAs::Zero => ChartEmptyCells::Zero,
            EmptyAs::Span => ChartEmptyCells::Connected,
        });
    if (g.line_weight - 0.75).abs() > 1e-9 {
        s = s.set_line_weight(g.line_weight);
    }
    match g.min {
        AxisScale::Group => s = s.set_group_min(true),
        AxisScale::Custom(v) => s = s.set_custom_min(v),
        AxisScale::Individual => {}
    }
    match g.max {
        AxisScale::Group => s = s.set_group_max(true),
        AxisScale::Custom(v) => s = s.set_custom_max(v),
        AxisScale::Individual => {}
    }
    if let Some((sheet, rg)) = g.date_range.as_deref().and_then(split_ref) {
        s = s.set_date_range((sheet.as_str(), rg.r1, rg.c1 as u16, rg.r2, rg.c2 as u16));
    }
    s
}

fn valid(rg: &Range) -> bool {
    rg.r2 < MAX_ROWS && rg.c2 < MAX_COLS
}

/// A group whose locations form one row or column and whose data rows (or columns) follow
/// each other is written as one sparkline group; anything else is written cell by cell.
pub(super) fn write_sparklines(sh: &Sheet, ws: &mut Worksheet) {
    for g in &sh.sparklines {
        let base = styled(g);
        let mut items: Vec<(u32, u32, String, Range)> = g.items.iter().filter_map(|it| split_ref(&it.data).map(|(s, rg)| (it.loc.0, it.loc.1, s, rg))).filter(|(_, _, _, rg)| valid(rg)).collect();
        items.sort_by_key(|(r, c, _, _)| (*r, *c));
        if items.len() > 1 && write_group(ws, &base, &items) {
            continue;
        }
        for (r, c, sheet, rg) in items {
            if rg.rows() > 1 && rg.cols() > 1 {
                continue;
            }
            let sp = base.clone().set_range((sheet.as_str(), rg.r1, rg.c1 as u16, rg.r2, rg.c2 as u16));
            let _ = ws.add_sparkline(r, c as u16, &sp);
        }
    }
}

fn write_group(ws: &mut Worksheet, base: &XSparkline, items: &[(u32, u32, String, Range)]) -> bool {
    let n = items.len() as u32;
    let (r0, c0) = (items[0].0, items[0].1);
    let vertical = items.iter().enumerate().all(|(k, (r, c, _, _))| *c == c0 && *r == r0 + k as u32);
    let horizontal = items.iter().enumerate().all(|(k, (r, c, _, _))| *r == r0 && *c == c0 + k as u32);
    if !vertical && !horizontal {
        return false;
    }
    let sheet = &items[0].2;
    let d0 = items[0].3;
    if items.iter().any(|(_, _, s, _)| s != sheet) {
        return false;
    }
    let by_rows = d0.rows() == 1 && items.iter().enumerate().all(|(k, (_, _, _, rg))| rg.rows() == 1 && rg.c1 == d0.c1 && rg.c2 == d0.c2 && rg.r1 == d0.r1 + k as u32);
    let by_cols = !by_rows && d0.cols() == 1 && items.iter().enumerate().all(|(k, (_, _, _, rg))| rg.cols() == 1 && rg.r1 == d0.r1 && rg.r2 == d0.r2 && rg.c1 == d0.c1 + k as u32);
    let data = if by_rows {
        Range::new(d0.r1, d0.c1, d0.r1 + n - 1, d0.c2)
    } else if by_cols {
        Range::new(d0.r1, d0.c1, d0.r2, d0.c1 + n - 1)
    } else {
        return false;
    };
    // rust_xlsxwriter needs a 2D range and matches the group by its row or column count.
    if data.rows() == 1 || data.cols() == 1 || (by_rows && data.rows() != n) || (by_cols && data.cols() != n) {
        return false;
    }
    let sp = base.clone().set_range((sheet.as_str(), data.r1, data.c1 as u16, data.r2, data.c2 as u16)).set_column_order(by_cols);
    let (r1, c1) = (r0, c0 as u16);
    let (r2, c2) = if vertical { (r0 + n - 1, c0 as u16) } else { (r0, (c0 + n - 1) as u16) };
    ws.add_sparkline_group(r1, c1, r2, c2, &sp).is_ok()
}

/// The `<f>` element of a data table's first cell.
pub fn data_table_f(t: &crate::whatif::DataTable) -> String {
    let p = |c: (u32, u32)| cell_name(c.0, c.1);
    let rf = t.range.to_a1();
    match (t.row_input, t.col_input) {
        (Some(ri), Some(ci)) => format!("<f t=\"dataTable\" ref=\"{rf}\" dt2D=\"1\" dtr=\"1\" r1=\"{}\" r2=\"{}\"/>", p(ri), p(ci)),
        (Some(ri), None) => format!("<f t=\"dataTable\" ref=\"{rf}\" dt2D=\"0\" dtr=\"1\" r1=\"{}\"/>", p(ri)),
        (None, Some(ci)) => format!("<f t=\"dataTable\" ref=\"{rf}\" dt2D=\"0\" dtr=\"0\" r1=\"{}\"/>", p(ci)),
        (None, None) => String::new(),
    }
}

/// Inserts `f` as the first child of cell `r` (`<c r="B5" ...>`).
fn insert_f(xml: &str, at: &str, f: &str) -> Option<String> {
    let needle = format!("<c r=\"{at}\"");
    let mut from = 0;
    let start = loop {
        let i = from + xml[from..].find(&needle)?;
        let next = xml.as_bytes().get(i + needle.len()).copied();
        if matches!(next, Some(b' ') | Some(b'>') | Some(b'/')) {
            break i;
        }
        from = i + needle.len();
    };
    let end = start + xml[start..].find('>')?;
    Some(if xml[..end].ends_with('/') {
        format!("{}>{}</c>{}", &xml[..end - 1], f, &xml[end + 1..])
    } else {
        format!("{}{}{}", &xml[..=end], f, &xml[end + 1..])
    })
}

fn scenarios_xml(sh: &Sheet) -> String {
    let s = &sh.scenarios;
    let mut x = format!("<scenarios current=\"{}\" show=\"{}\"", s.current.min(s.list.len().saturating_sub(1)), s.show.min(s.list.len().saturating_sub(1)));
    if !s.result_cells.is_empty() {
        x.push_str(&format!(" sqref=\"{}\"", crate::condfmt::sqref(&s.result_cells)));
    }
    x.push('>');
    for sc in &s.list {
        x.push_str(&format!("<scenario name=\"{}\"", esc(&sc.name)));
        if sc.locked {
            x.push_str(" locked=\"1\"");
        }
        if sc.hidden {
            x.push_str(" hidden=\"1\"");
        }
        x.push_str(&format!(" count=\"{}\"", sc.cells.len()));
        if !sc.user.is_empty() {
            x.push_str(&format!(" user=\"{}\"", esc(&sc.user)));
        }
        if !sc.comment.is_empty() {
            x.push_str(&format!(" comment=\"{}\"", esc(&sc.comment)));
        }
        x.push('>');
        for ((r, c), v) in &sc.cells {
            x.push_str(&format!("<inputCells r=\"{}\" val=\"{}\"/>", cell_name(*r, *c), esc(v)));
        }
        x.push_str("</scenario>");
    }
    x.push_str("</scenarios>");
    x
}

/// Puts `<scenarios>` after sheetData / sheetCalcPr / sheetProtection / protectedRanges.
fn insert_scenarios(xml: &str, sc: &str) -> Option<String> {
    let mut at = None;
    for tag in ["</sheetData>", "<sheetData/>"] {
        if let Some(i) = xml.find(tag) {
            at = Some(i + tag.len());
        }
    }
    let mut at = at?;
    for tag in ["sheetCalcPr", "sheetProtection"] {
        if let Some(i) = xml.find(&format!("<{tag}")) {
            at = i + xml[i..].find('>')? + 1;
        }
    }
    if let Some(i) = xml.find("</protectedRanges>") {
        at = i + "</protectedRanges>".len();
    }
    Some(format!("{}{}{}", &xml[..at], sc, &xml[at..]))
}

/// Data tables whose first result cell still holds its TABLE formula.
fn tables(sh: &Sheet) -> Vec<&crate::whatif::DataTable> {
    sh.data_tables
        .iter()
        .filter(|t| valid(&t.range) && sh.get(t.range.r1, t.range.c1).and_then(|c| c.formula.as_ref()).is_some_and(|f| f.text.starts_with("TABLE(")))
        .collect()
}

/// Patches data tables, scenarios and Solver names into the package; unchanged when none exist.
pub(super) fn patch(wb: &Workbook, pkg: Vec<u8>) -> Result<Vec<u8>, String> {
    let solver = wb.sheets.iter().any(|s| s.names.iter().any(|(n, _)| n.to_ascii_lowercase().starts_with("solver_")));
    let sheets: Vec<usize> = (0..wb.sheets.len()).filter(|&i| !tables(&wb.sheets[i]).is_empty() || !wb.sheets[i].scenarios.list.is_empty()).collect();
    if sheets.is_empty() && !solver {
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
    for si in sheets {
        let sh = &wb.sheets[si];
        let name = format!("xl/worksheets/sheet{}.xml", si + 1);
        let Some(p) = parts.iter_mut().find(|p| p.0 == name) else { continue };
        let mut xml = String::from_utf8_lossy(&p.1).to_string();
        for t in tables(sh) {
            if let Some(new) = insert_f(&xml, &cell_name(t.range.r1, t.range.c1), &data_table_f(t)) {
                xml = new;
            }
        }
        if !sh.scenarios.list.is_empty() {
            if let Some(new) = insert_scenarios(&xml, &scenarios_xml(sh)) {
                xml = new;
            }
        }
        p.1 = xml.into_bytes();
    }
    if solver {
        if let Some(p) = parts.iter_mut().find(|p| p.0 == "xl/workbook.xml") {
            let xml = String::from_utf8_lossy(&p.1).to_string();
            p.1 = xml.replace("<definedName name=\"solver_", "<definedName hidden=\"1\" name=\"solver_").into_bytes();
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

    /// Icon sets (plain, x14 and custom), sparkline groups, data tables, scenarios and Solver
    /// names survive a save and load.
    #[test]
    fn analysis_round_trip() {
        use crate::condfmt::{parse_sqref, CondFormat, DiffFormat, RuleKind};
        use crate::iconset::{IconSetRule, IconStyle, Threshold, ThresholdKind};
        use crate::model::{cell_from_input, Workbook};
        use crate::sparkline::{SparkColors, Sparkline};
        use crate::whatif::Scenario;

        let mut wb = Workbook::new();
        let put = |wb: &mut Workbook, a1: &str, text: &str| {
            let (r, c) = crate::model::parse_cell(a1).unwrap();
            let cell = cell_from_input(wb, text, 0);
            wb.sheets[0].set(r, c, cell);
        };
        for r in 1..=4 {
            for c in 0..5 {
                put(&mut wb, &format!("{}{}", crate::model::col_name(c), r), &format!("{}", (r * 7 + c * 3) % 11));
            }
        }
        let rg = |s: &str| parse_sqref(s).unwrap();
        let mut custom = IconSetRule::new(IconStyle::Arrows3);
        custom.thresholds[1] = Threshold { kind: ThresholdKind::Percentile, value: "40".into(), gte: false };
        custom.thresholds[2] = Threshold { kind: ThresholdKind::Number, value: "8".into(), gte: true };
        let mut picks = IconSetRule::new(IconStyle::TrafficLights3);
        picks.custom = vec![Some((IconStyle::Flags3, 0)), None, Some((IconStyle::Stars3, 2))];
        let mut formula = IconSetRule::new(IconStyle::Quarters5);
        formula.thresholds[4] = Threshold { kind: ThresholdKind::Formula, value: "$A$1*2".into(), gte: true };
        let rules = vec![
            CondFormat::new(rg("A1:A4"), RuleKind::IconSet(custom), DiffFormat::default()),
            CondFormat::new(rg("B1:B4"), RuleKind::IconSet(IconSetRule::new(IconStyle::Stars3)), DiffFormat::default()),
            CondFormat::new(rg("C1:C4"), RuleKind::IconSet(IconSetRule { reverse: true, icon_only: true, ..IconSetRule::new(IconStyle::TrafficLights4) }), DiffFormat::default()),
            CondFormat::new(rg("D1:D4"), RuleKind::IconSet(IconSetRule::new(IconStyle::Boxes5)), DiffFormat::default()),
            CondFormat::new(rg("E1:E4"), RuleKind::IconSet(picks), DiffFormat::default()),
            CondFormat::new(rg("F1:F4"), RuleKind::IconSet(formula), DiffFormat::default()),
        ];
        wb.sheets[0].cond_formats = rules.clone();
        // A regular group (one row of data per location) and a single column sparkline.
        let mut g = SparklineGroup::new(SparkKind::Line, (0..4).map(|k| Sparkline { loc: (k, 7), data: format!("Sheet1!A{0}:E{0}", k + 1) }).collect());
        (g.markers, g.high, g.low, g.axis, g.first) = (true, true, true, true, true);
        g.colors = SparkColors::styled(3, 1);
        g.min = AxisScale::Custom(-1.5);
        g.max = AxisScale::Group;
        g.empty_as = EmptyAs::Span;
        g.line_weight = 1.5;
        let mut col = SparklineGroup::new(SparkKind::Column, vec![Sparkline { loc: (5, 0), data: "Sheet1!A1:A4".into() }]);
        col.negative = true;
        col.rtl = true;
        let win = SparklineGroup::new(SparkKind::WinLoss, vec![Sparkline { loc: (6, 0), data: "Sheet1!B1:B4".into() }]);
        wb.sheets[0].sparklines = vec![g.clone(), col.clone(), win.clone()];
        // Data tables: one-variable (column input), one-variable (row input), two-variable.
        put(&mut wb, "J1", "2");
        put(&mut wb, "J2", "3");
        put(&mut wb, "J3", "=J1*J2");
        put(&mut wb, "K10", "=J3");
        put(&mut wb, "J11", "1");
        put(&mut wb, "J12", "2");
        crate::whatif::create_data_table(&mut wb.sheets[0], Range::parse("J10:K12").unwrap(), None, Some((0, 9))).unwrap();
        put(&mut wb, "M10", "=J3");
        put(&mut wb, "N9", "5");
        put(&mut wb, "O9", "6");
        crate::whatif::create_data_table(&mut wb.sheets[0], Range::parse("M9:O10").unwrap(), Some((1, 9)), None).unwrap();
        put(&mut wb, "Q9", "=J3");
        put(&mut wb, "R9", "10");
        put(&mut wb, "S9", "20");
        put(&mut wb, "Q10", "1");
        put(&mut wb, "Q11", "2");
        crate::whatif::create_data_table(&mut wb.sheets[0], Range::parse("Q9:S11").unwrap(), Some((0, 9)), Some((1, 9))).unwrap();
        wb.recalc();
        assert_eq!(wb.sheets[0].value(11, 10), &crate::model::Value::Number(6.0));
        assert_eq!(wb.sheets[0].value(10, 18), &crate::model::Value::Number(40.0));
        wb.sheets[0].scenarios.list = vec![
            Scenario { name: "Best \"case\"".into(), comment: "Created by me".into(), user: "me".into(), locked: true, hidden: false, cells: vec![((0, 9), "5".into()), ((1, 9), "7".into())] },
            Scenario { name: "Worst".into(), hidden: true, cells: vec![((0, 9), "1".into())], ..Default::default() },
        ];
        wb.sheets[0].scenarios.result_cells = vec![Range::parse("J3").unwrap()];
        let model = crate::solver::SolverModel { objective: Some((2, 9)), vars: vec![Range::parse("J1:J2").unwrap()], ..Default::default() };
        wb.sheets[0].names.extend(model.to_names("Sheet1"));

        let dir = std::env::temp_dir().join(format!("freedom_sheets_analysis_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("analysis.xlsx");
        crate::io::save(&wb, &path).unwrap();
        let mut back = crate::io::load(&path).unwrap();
        let s = &back.sheets[0];
        assert_eq!(s.cond_formats.len(), rules.len());
        for (a, b) in s.cond_formats.iter().zip(&rules) {
            assert_eq!(a, b);
        }
        assert_eq!(s.sparklines.len(), 3, "{:#?}", s.sparklines);
        let find = |k: SparkKind| s.sparklines.iter().find(|x| x.kind == k).unwrap().clone();
        let mut bg = find(SparkKind::Line);
        bg.items.sort_by_key(|i| i.loc);
        assert_eq!(bg, g);
        assert_eq!(find(SparkKind::Column), col);
        assert_eq!(find(SparkKind::WinLoss), win);
        let sorted = |v: &[crate::whatif::DataTable]| {
            let mut v = v.to_vec();
            v.sort_by_key(|t| (t.range.r1, t.range.c1));
            v
        };
        assert_eq!(sorted(&s.data_tables), sorted(&wb.sheets[0].data_tables));
        assert_eq!(s.get(10, 10).unwrap().input_text(), "{=TABLE(,J1)}");
        assert_eq!(s.get(10, 18).unwrap().input_text(), "{=TABLE(J1,J2)}");
        assert_eq!(s.scenarios, wb.sheets[0].scenarios);
        assert_eq!(crate::solver::SolverModel::from_names(&s.names), Some(model));
        back.recalc();
        assert_eq!(back.sheets[0].value(11, 10), &crate::model::Value::Number(6.0));
        assert_eq!(back.sheets[0].value(9, 14), &crate::model::Value::Number(12.0));
        assert_eq!(back.sheets[0].value(10, 18), &crate::model::Value::Number(40.0));
        let xml = {
            let mut z = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
            let mut s = String::new();
            z.by_name("xl/workbook.xml").unwrap().read_to_string(&mut s).unwrap();
            s
        };
        assert!(xml.contains("hidden=\"1\" name=\"solver_opt\""), "{xml}");
        // FS_KEEP_ANALYSIS keeps the file for checking it in Excel.
        if std::env::var_os("FS_KEEP_ANALYSIS").is_none() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn patch_helpers() {
        assert_eq!(insert_f("<c r=\"B10\"><v>1</v></c><c r=\"B1\" s=\"2\"><v>3</v></c>", "B1", "<f/>").unwrap(), "<c r=\"B10\"><v>1</v></c><c r=\"B1\" s=\"2\"><f/><v>3</v></c>");
        assert_eq!(insert_f("<c r=\"B1\" s=\"2\"/>", "B1", "<f/>").unwrap(), "<c r=\"B1\" s=\"2\"><f/></c>");
        assert_eq!(insert_scenarios("<sheetData/><sheetProtection a=\"1\"/><x/>", "<S/>").unwrap(), "<sheetData/><sheetProtection a=\"1\"/><S/><x/>");
        assert_eq!(insert_scenarios("<sheetData></sheetData><x/>", "<S/>").unwrap(), "<sheetData></sheetData><S/><x/>");
    }
}
