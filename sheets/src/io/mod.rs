//! File import/export: xlsx (native reader + rust_xlsxwriter), xls/xlsb/ods (calamine), csv/tsv/txt.

mod calamine_read;
mod delimited;
mod xlsx_read;
mod xlsx_write;

use std::path::Path;

use crate::model::Workbook;

/// File dialog filters for opening: (label, extensions).
pub const OPEN_FILTERS: &[(&str, &[&str])] = &[
    ("All spreadsheets", &["xlsx", "xlsm", "xltx", "xls", "xlsb", "ods", "csv", "tsv", "txt"]),
    ("Excel Workbook", &["xlsx", "xlsm", "xltx"]),
    ("Excel 97-2003 Workbook", &["xls"]),
    ("Excel Binary Workbook", &["xlsb"]),
    ("OpenDocument Spreadsheet", &["ods"]),
    ("CSV (Comma delimited)", &["csv"]),
    ("Text (Tab delimited)", &["tsv", "txt"]),
];

/// File dialog filters for saving: (label, extensions).
pub const SAVE_FILTERS: &[(&str, &[&str])] = &[
    ("Excel Workbook", &["xlsx"]),
    ("Excel Macro-Enabled Workbook", &["xlsm"]),
    ("CSV (Comma delimited)", &["csv"]),
    ("Text (Tab delimited)", &["tsv", "txt"]),
];

fn ext(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// Loads a workbook, choosing the reader by file extension. Does not recalculate.
pub fn load(path: &Path) -> Result<Workbook, String> {
    let mut wb = match ext(path).as_str() {
        "xlsx" | "xlsm" | "xltx" | "xltm" => {
            let f = std::fs::File::open(path).map_err(|e| format!("Could not open {}: {e}", path.display()))?;
            xlsx_read::read(std::io::BufReader::new(f))?
        }
        "xls" | "xlsb" | "ods" | "xla" | "xlam" => calamine_read::read(path)?,
        "csv" => delimited::read(path, b',', false)?,
        "tsv" => delimited::read(path, b'\t', true)?,
        "txt" => delimited::read(path, b'\t', false)?,
        other => return Err(format!("Unsupported file type: .{other}")),
    };
    wb.path = Some(path.to_path_buf());
    Ok(wb)
}

/// Saves a workbook, choosing the writer by file extension (.xlsm is written as plain xlsx content).
pub fn save(wb: &Workbook, path: &Path) -> Result<(), String> {
    match ext(path).as_str() {
        "xlsx" | "xlsm" => xlsx_write::write(wb, path),
        "csv" => delimited::write(wb, path, b','),
        "tsv" | "txt" => delimited::write(wb, path, b'\t'),
        other => Err(format!("Saving as .{other} is not supported; use .xlsx, .csv or .tsv")),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;

    use super::*;
    use crate::model::{
        Border, BorderLine, Cell, ErrorKind, Formula, HAlign, Range, Sheet, Style, VAlign, Value, Workbook,
    };

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("freedom_sheets_io_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn fcell(text: &str, value: Value) -> Cell {
        Cell { value, formula: Some(Box::new(Formula::new(text))), style: 0 }
    }

    #[test]
    fn xlsx_dynamic_arrays() {
        let mut wb = Workbook::new();
        wb.sheets[0].set(0, 0, Some(fcell("SEQUENCE(3)", Value::Empty)));
        wb.sheets[0].set(0, 1, Some(fcell("SUM(A1#)+@C1:C2", Value::Empty)));
        wb.sheets[0].set(0, 2, Some(fcell("LET(x,2,x*10)", Value::Empty)));
        wb.recalc();
        assert_eq!(wb.sheets[0].display_value(2, 0), &Value::Number(3.0));
        assert_eq!(wb.sheets[0].value(0, 1), &Value::Number(26.0));
        let path = temp("dynamic_arrays.xlsx");
        save(&wb, &path).unwrap();
        let mut back = load(&path).unwrap();
        let s = &back.sheets[0];
        assert_eq!(s.get(0, 1).unwrap().formula.as_ref().unwrap().text, "SUM(A1#)+@C1:C2");
        assert_eq!(s.get(0, 2).unwrap().formula.as_ref().unwrap().text, "LET(x,2,x*10)");
        // cached spill results are dropped so the formula can spill again
        assert!(s.get(1, 0).is_none() && s.get(2, 0).is_none());
        back.recalc();
        assert_eq!(back.sheets[0].display_value(2, 0), &Value::Number(3.0));
        assert_eq!(back.sheets[0].value(0, 1), &Value::Number(26.0));
        assert_eq!(back.sheets[0].value(0, 2), &Value::Number(20.0));
    }

    #[test]
    fn xlsx_round_trip() {
        let mut wb = Workbook::empty_with(vec![Sheet::new("Data"), Sheet::new("My Sheet")]);
        let fancy = wb.intern_style(Style {
            bold: true,
            fill: Some(0xFFFF00),
            color: Some(0xC00000),
            font: Some("Arial".into()),
            size10: 140,
            halign: HAlign::Center,
            valign: VAlign::Top,
            wrap: true,
            num_fmt: "#,##0.00".into(),
            border: Border { left: BorderLine::Thin, right: BorderLine::Medium, top: BorderLine::Double, bottom: BorderLine::Dashed },
            ..Default::default()
        });
        let date = wb.intern_style(Style { num_fmt: "m/d/yyyy".into(), ..Default::default() });
        {
            let s = &mut wb.sheets[0];
            s.set(0, 0, Some(Cell { value: Value::Text("Name & <Co>".into()), ..Default::default() }));
            s.set(0, 1, Some(Cell { value: Value::Number(1234.5), style: fancy, ..Default::default() }));
            s.set(1, 0, Some(Cell { value: Value::Bool(true), ..Default::default() }));
            s.set(1, 1, Some(Cell { value: Value::Error(ErrorKind::NA), ..Default::default() }));
            s.set(2, 0, Some(Cell { value: Value::Number(46330.0), style: date, ..Default::default() }));
            s.set(2, 1, Some(fcell("B1*2", Value::Number(2469.0))));
            s.set(3, 1, Some(fcell("XLOOKUP(\"x\",'My Sheet'!A1:A3,'My Sheet'!B1:B3)", Value::Number(7.0))));
            s.set(4, 0, Some(Cell { value: Value::Text("merged".into()), ..Default::default() }));
            s.merges.push(Range::new(4, 0, 5, 2));
            s.col_w.insert(0, 100.0);
            s.row_h.insert(1, 40.0);
            s.hidden_cols.insert(3);
            s.freeze = (1, 1);
            s.gridlines = false;
            s.tab_color = Some(0x00B050);
            s.filter = Some(crate::model::AutoFilter { range: Range::new(0, 0, 3, 1), ..Default::default() });
        }
        {
            let s = &mut wb.sheets[1];
            s.set(0, 0, Some(Cell { value: Value::Text("x".into()), ..Default::default() }));
            s.set(0, 1, Some(Cell { value: Value::Number(7.0), ..Default::default() }));
            s.set(1, 1, Some(fcell("SUM(Data!B1:B3)+1", Value::Number(3704.5))));
        }
        wb.names.push(("Rate".into(), "Data!$B$1".into()));
        wb.active = 1;

        let path = temp("round_trip.xlsx");
        save(&wb, &path).unwrap();
        let back = load(&path).unwrap();

        assert_eq!(back.path.as_deref(), Some(path.as_path()));
        assert_eq!(back.sheets.len(), 2);
        assert_eq!(back.sheets[0].name, "Data");
        assert_eq!(back.sheets[1].name, "My Sheet");
        assert_eq!(back.active, 1);
        assert_eq!(back.names, vec![("Rate".to_string(), "Data!$B$1".to_string())]);

        let s = &back.sheets[0];
        assert_eq!(s.value(0, 0), &Value::Text("Name & <Co>".into()));
        assert_eq!(s.value(0, 1), &Value::Number(1234.5));
        assert_eq!(s.value(1, 0), &Value::Bool(true));
        assert_eq!(s.value(1, 1), &Value::Error(ErrorKind::NA));
        assert!(s.get(1, 1).unwrap().formula.is_none());
        assert_eq!(s.value(2, 0), &Value::Number(46330.0));
        assert_eq!(back.style(s.get(2, 0).unwrap().style).num_fmt, "m/d/yyyy");
        let f = |r, c| s.get(r, c).unwrap().formula.as_ref().unwrap().text.clone();
        assert_eq!(f(2, 1), "B1*2");
        assert_eq!(s.value(2, 1), &Value::Number(2469.0));
        assert_eq!(f(3, 1), "XLOOKUP(\"x\",'My Sheet'!A1:A3,'My Sheet'!B1:B3)");
        assert_eq!(back.sheets[1].get(1, 1).unwrap().formula.as_ref().unwrap().text, "SUM(Data!B1:B3)+1");

        let st = back.style(s.get(0, 1).unwrap().style);
        assert_eq!(st, wb.style(fancy), "style survived");

        assert_eq!(s.value(4, 0), &Value::Text("merged".into()));
        assert_eq!(s.merges, vec![Range::new(4, 0, 5, 2)]);
        assert_eq!(s.col_w.get(&0), Some(&100.0));
        assert_eq!(s.row_h.get(&1), Some(&40.0));
        assert!(s.hidden_cols.contains(&3));
        assert_eq!(s.freeze, (1, 1));
        assert!(!s.gridlines);
        assert_eq!(s.tab_color, Some(0x00B050));
        assert_eq!(s.filter.as_ref().map(|f| f.range), Some(Range::new(0, 0, 3, 1)));
        assert!(back.sheets[1].gridlines);
        let _ = std::fs::remove_file(&path);
    }

    fn write_zip(path: &Path, parts: &[(&str, &str)]) {
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, body) in parts {
            z.start_file(*name, opts).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn xlsx_shared_formulas_and_theme() {
        let ct = r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/></Types>"#;
        let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let wbx = r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="R&amp;D" sheetId="1" r:id="rId1"/></sheets><definedNames><definedName name="_xlnm._FilterDatabase" localSheetId="0" hidden="1">'R&amp;D'!$A$1:$B$3</definedName><definedName name="Total">'R&amp;D'!$B$1:$B$3</definedName></definedNames></workbook>"#;
        let wbrels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/></Relationships>"#;
        let styles = r#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="2"><font><sz val="11"/><name val="Calibri"/></font><font><b/><sz val="11"/><color theme="0"/><name val="Calibri"/></font></fonts><fills count="3"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill><fill><patternFill patternType="solid"><fgColor theme="0" tint="-0.1499984740745262"/><bgColor indexed="64"/></patternFill></fill></fills><borders count="1"><border><left/><right/><top/><bottom/></border></borders><cellXfs count="2"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/><xf numFmtId="10" fontId="1" fillId="2" borderId="0" applyFill="1"><alignment horizontal="right"/></xf></cellXfs></styleSheet>"#;
        let sst = r#"<?xml version="1.0"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="2" uniqueCount="2"><si><t>plain</t></si><si><r><rPr><b/></rPr><t xml:space="preserve">rich </t></r><r><t>text</t></r><rPh><t>PHONETIC</t></rPh></si></sst>"#;
        let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetPr><tabColor theme="4"/></sheetPr><sheetViews><sheetView workbookViewId="0"><pane ySplit="1" topLeftCell="A2" activePane="bottomLeft" state="frozen"/></sheetView></sheetViews><cols><col min="2" max="3" width="20.7109375" customWidth="1"/></cols><sheetData>
            <row r="1" ht="30" customHeight="1"><c r="A1"><v>1</v></c><c r="B1" s="1"><f t="shared" ref="B1:B3" si="0">A1*2</f><v>2</v></c><c r="C1" t="s"><v>1</v></c></row>
            <row r="2"><c r="A2"><v>2</v></c><c r="B2" s="1"><f t="shared" si="0"/><v>4</v></c><c r="C2" t="inlineStr"><is><t>in &amp; line</t></is></c></row>
            <row><c><v>3</v></c><c s="1"><f t="shared" si="0"/><v>6</v></c><c t="e"><v>#DIV/0!</v></c><c t="b"><v>1</v></c><c t="str"><f>_xlfn.CONCAT("a","b")</f><v>ab</v></c></row>
            </sheetData><autoFilter ref="A1:B3"/><mergeCells count="1"><mergeCell ref="D1:E1"/></mergeCells></worksheet>"#;
        let path = temp("shared.xlsx");
        write_zip(
            &path,
            &[
                ("[Content_Types].xml", ct),
                ("_rels/.rels", rels),
                ("xl/workbook.xml", wbx),
                ("xl/_rels/workbook.xml.rels", wbrels),
                ("xl/styles.xml", styles),
                ("xl/sharedStrings.xml", sst),
                ("xl/worksheets/sheet1.xml", sheet),
            ],
        );
        let wb = load(&path).unwrap();
        let s = &wb.sheets[0];
        assert_eq!(s.name, "R&D");
        let f = |r, c| s.get(r, c).unwrap().formula.as_ref().unwrap().text.clone();
        assert_eq!(f(0, 1), "A1*2");
        assert_eq!(f(1, 1), "A2*2");
        assert_eq!(f(2, 1), "A3*2");
        assert_eq!(s.value(2, 1), &Value::Number(6.0));
        assert_eq!(s.value(2, 0), &Value::Number(3.0));
        assert_eq!(s.value(0, 2), &Value::Text("rich text".into()));
        assert_eq!(s.value(1, 2), &Value::Text("in & line".into()));
        assert_eq!(s.value(2, 2), &Value::Error(ErrorKind::Div0));
        assert_eq!(s.value(2, 3), &Value::Bool(true));
        assert_eq!(f(2, 4), "CONCAT(\"a\",\"b\")");
        assert_eq!(s.value(2, 4), &Value::Text("ab".into()));

        let st = wb.style(s.get(0, 1).unwrap().style);
        assert!(st.bold);
        assert_eq!(st.fill, Some(0xD9D9D9));
        assert_eq!(st.color, Some(0xFFFFFF));
        assert_eq!(st.num_fmt, "0.00%");
        assert_eq!(st.halign, HAlign::Right);

        assert_eq!(s.freeze, (1, 0));
        assert_eq!(s.tab_color, Some(0x4472C4));
        assert_eq!(s.col_w.get(&1), Some(&145.0));
        assert_eq!(s.col_w.get(&2), Some(&145.0));
        assert_eq!(s.row_h.get(&0), Some(&40.0));
        assert_eq!(s.merges, vec![Range::new(0, 3, 0, 4)]);
        assert_eq!(s.filter.as_ref().map(|f| f.range), Some(Range::new(0, 0, 2, 1)));
        assert_eq!(wb.names, vec![("Total".to_string(), "'R&D'!$B$1:$B$3".to_string())]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn csv_semicolon_newline_and_injection() {
        let path = temp("data.csv");
        std::fs::write(&path, "\u{feff}name;amount;note\r\n\"multi\nline\";1,5;=1+2\r\nb;12;\"x;y\"\r\n").unwrap();
        let wb = load(&path).unwrap();
        let s = &wb.sheets[0];
        assert_eq!(s.name, "data");
        assert_eq!(s.value(0, 0), &Value::Text("name".into()));
        assert_eq!(s.value(1, 0), &Value::Text("multi\nline".into()));
        assert_eq!(s.value(1, 2), &Value::Text("=1+2".into()));
        assert!(s.get(1, 2).unwrap().formula.is_none());
        assert_eq!(s.value(2, 1), &Value::Number(12.0));
        assert_eq!(s.value(2, 2), &Value::Text("x;y".into()));

        let mut wb = wb;
        let pct = wb.intern_style(Style { num_fmt: "0.0%".into(), ..Default::default() });
        wb.sheets[0].set(3, 1, Some(Cell { value: Value::Number(0.25), style: pct, ..Default::default() }));
        let out = temp("out.csv");
        save(&wb, &out).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert_eq!(
            text,
            "\u{feff}name,amount,note\r\n\"multi\nline\",\"1,5\",=1+2\r\nb,12,x;y\r\n,25.0%,\r\n"
        );

        let tsv = temp("out.tsv");
        save(&wb, &tsv).unwrap();
        let text = std::fs::read_to_string(&tsv).unwrap();
        assert!(text.starts_with("\u{feff}name\tamount\tnote\r\n"));
        let back = load(&tsv).unwrap();
        assert_eq!(back.sheets[0].value(2, 2), &Value::Text("x;y".into()));
        for p in [path, out, tsv] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn csv_latin1_fallback() {
        let path = temp("latin.csv");
        std::fs::write(&path, b"caf\xe9,\x80 5\r\n").unwrap();
        let wb = load(&path).unwrap();
        assert_eq!(wb.sheets[0].value(0, 0), &Value::Text("caf\u{e9}".into()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ods_via_calamine() {
        let manifest = r#"<?xml version="1.0" encoding="UTF-8"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>"#;
        let content = r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" office:version="1.2"><office:body><office:spreadsheet><table:table table:name="Sheet A"><table:table-row><table:table-cell office:value-type="float" office:value="2"><text:p>2</text:p></table:table-cell><table:table-cell table:formula="of:=[.A1]*3" office:value-type="float" office:value="6"><text:p>6</text:p></table:table-cell><table:table-cell office:value-type="date" office:date-value="2026-11-04"><text:p>11/04/26</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>hi</text:p></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#;
        let path = temp("t.ods");
        write_zip(
            &path,
            &[
                ("mimetype", "application/vnd.oasis.opendocument.spreadsheet"),
                ("META-INF/manifest.xml", manifest),
                ("content.xml", content),
            ],
        );
        let wb = load(&path).unwrap();
        let s = &wb.sheets[0];
        assert_eq!(s.name, "Sheet A");
        assert_eq!(s.value(0, 0), &Value::Number(2.0));
        assert_eq!(s.value(0, 1), &Value::Number(6.0));
        assert_eq!(s.get(0, 1).unwrap().formula.as_ref().unwrap().text, "A1*3");
        assert_eq!(s.value(0, 2), &Value::Number(46330.0));
        assert_eq!(wb.style(s.get(0, 2).unwrap().style).num_fmt, "m/d/yyyy");
        assert_eq!(s.value(0, 3), &Value::Text("hi".into()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unsupported_extension() {
        assert!(load(Path::new("x.pdf")).is_err());
        assert!(save(&Workbook::new(), Path::new("x.ods")).is_err());
    }
}
