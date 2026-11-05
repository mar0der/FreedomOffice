//! xlsx round trips of tables, scoped names, rich text, protection, zoom, note authors and pictures.

use std::path::PathBuf;
use std::sync::Arc;

use super::{load, save};
use crate::model::{cell_from_input, legacy_password_hash, Picture, Protection, Range, RichRun, RichText, Sheet, Style, Value, Workbook};
use crate::table::{Table, TotalFn};

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("freedom_sheets_fid_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn put(wb: &mut Workbook, s: usize, r: u32, c: u32, text: &str) {
    let cell = cell_from_input(wb, text, 0);
    wb.sheets[s].set(r, c, cell);
}

fn round_trip(wb: &Workbook, name: &str) -> Workbook {
    let path = temp(name);
    save(wb, &path).unwrap();
    let mut back = load(&path).unwrap();
    back.recalc();
    back
}

/// A 1x1 PNG.
const PNG: [u8; 69] = [
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
    0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63,
    0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

#[test]
fn table_round_trip() {
    let mut wb = Workbook::empty_with(vec![Sheet::new("Data")]);
    for (r, row) in [["Item", "Qty", "Unit Price", "Amount"], ["a", "2", "10", ""], ["b", "3", "20", ""], ["c", "5", "30", ""]].iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            put(&mut wb, 0, r as u32, c as u32, v);
        }
    }
    for r in 1..=3 {
        put(&mut wb, 0, r, 3, "=[@Qty]*[@[Unit Price]]");
    }
    let mut t = Table::new("Sales", Range::new(0, 0, 4, 3), "TableStyleLight9");
    t.totals = true;
    crate::table::sync_columns_from_cells(&wb.sheets[0], &mut t);
    t.columns[0].total_label = "Total".into();
    t.columns[1].total = TotalFn::Sum;
    t.columns[3].total = TotalFn::Average;
    put(&mut wb, 0, 4, 0, "Total");
    let f = format!("={}", t.total_formula(1).unwrap());
    put(&mut wb, 0, 4, 1, &f);
    let f = format!("={}", t.total_formula(3).unwrap());
    put(&mut wb, 0, 4, 3, &f);
    wb.sheets[0].tables.push(t);
    put(&mut wb, 0, 7, 0, "=SUM(Sales[Amount])+Sales[[#Totals],[Qty]]");
    wb.recalc();
    assert_eq!(wb.sheets[0].value(1, 3), &Value::Number(20.0));
    assert_eq!(wb.sheets[0].value(4, 1), &Value::Number(10.0));
    assert_eq!(wb.sheets[0].value(7, 0), &Value::Number(240.0));

    let back = round_trip(&wb, "table.xlsx");
    let s = &back.sheets[0];
    assert_eq!(s.tables.len(), 1);
    let t = &s.tables[0];
    assert_eq!(t.name, "Sales");
    assert_eq!(t.range, Range::new(0, 0, 4, 3));
    assert!(t.header && t.totals && t.filter && t.banded_rows);
    assert_eq!(t.style, "TableStyleLight9");
    let names: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Item", "Qty", "Unit Price", "Amount"]);
    assert_eq!(t.columns[1].total, TotalFn::Sum);
    assert_eq!(t.columns[3].total, TotalFn::Average);
    assert_eq!(t.columns[0].total_label, "Total");
    assert_eq!(s.get(1, 3).unwrap().formula.as_ref().unwrap().text, "Sales[@Qty]*Sales[@[Unit Price]]");
    assert_eq!(s.value(2, 3), &Value::Number(60.0));
    assert_eq!(s.value(4, 3), &Value::Number(76.0 + 2.0 / 3.0));
    assert_eq!(s.value(7, 0), &Value::Number(240.0));
}

#[test]
fn scoped_names_round_trip() {
    let mut wb = Workbook::empty_with(vec![Sheet::new("One"), Sheet::new("Two Words")]);
    put(&mut wb, 0, 0, 0, "10");
    put(&mut wb, 1, 0, 0, "20");
    wb.names.push(("Rate".into(), "One!$A$1".into()));
    wb.sheets[1].names.push(("Rate".into(), "'Two Words'!$A$1".into()));
    put(&mut wb, 0, 1, 0, "=Rate");
    put(&mut wb, 1, 1, 0, "=Rate");
    put(&mut wb, 0, 2, 0, "='Two Words'!Rate+1");
    wb.recalc();
    assert_eq!(wb.sheets[0].value(1, 0), &Value::Number(10.0));
    assert_eq!(wb.sheets[1].value(1, 0), &Value::Number(20.0));
    assert_eq!(wb.sheets[0].value(2, 0), &Value::Number(21.0));

    let back = round_trip(&wb, "names.xlsx");
    assert_eq!(back.names, vec![("Rate".to_string(), "One!$A$1".to_string())]);
    assert!(back.sheets[0].names.is_empty());
    assert_eq!(back.sheets[1].names, vec![("Rate".to_string(), "'Two Words'!$A$1".to_string())]);
    assert_eq!(back.sheets[1].value(1, 0), &Value::Number(20.0));
    assert_eq!(back.sheets[0].value(2, 0), &Value::Number(21.0));
}

#[test]
fn rich_text_round_trip() {
    let mut wb = Workbook::new();
    put(&mut wb, 0, 0, 0, "Bold and red");
    let rt = RichText {
        runs: vec![
            RichRun { text: "Bold".into(), bold: true, ..Default::default() },
            RichRun { text: " and ".into(), ..Default::default() },
            RichRun { text: "red".into(), color: Some(0xFF0000), italic: true, size: Some(14.0), ..Default::default() },
        ],
    };
    wb.sheets[0].rich.insert((0, 0), rt);
    let back = round_trip(&wb, "rich.xlsx");
    let rt = back.sheets[0].rich_at(0, 0).expect("runs kept");
    assert_eq!(rt.plain(), "Bold and red");
    assert_eq!(rt.runs.len(), 3);
    assert!(rt.runs[0].bold && !rt.runs[1].bold);
    assert_eq!(rt.runs[2].color, Some(0xFF0000));
    assert!(rt.runs[2].italic);
    assert_eq!(rt.runs[2].size, Some(14.0));
}

/// Excel 2013+ protects with an iterated SHA-512 hash: it must be verified on Unprotect and
/// written back unchanged (a lost hash would leave the sheet protected without a password).
#[test]
fn modern_protection_password_round_trip() {
    // Hash of "Secret1" with salt 00 01 .. 0F and 100000 spins (Python hashlib, same algorithm).
    let m = crate::model::PasswordHash {
        algorithm: "SHA-512".into(),
        hash: "A5/DGG2yRNJKVZSVgt4KbLetHxCMseyalSmU01nIFs4fnVIcy1YVYniahyacT/gB0zEoGnIPmhMJLBs2EXnbcg==".into(),
        salt: "AAECAwQFBgcICQoLDA0ODw==".into(),
        spin: 100_000,
    };
    assert!(m.matches("Secret1"));
    assert!(!m.matches("secret1") && !m.matches(""));
    let mut wb = Workbook::empty_with(vec![Sheet::new("A"), Sheet::new("B")]);
    put(&mut wb, 0, 0, 0, "x");
    wb.sheets[0].protection = Some(Protection { modern: Some(m.clone()), ..Default::default() });
    // A legacy hash with no known password survives too (no brute force on save).
    wb.sheets[1].protection = Some(Protection { hash: 0xCC3D, ..Default::default() });
    let back = round_trip(&wb, "protect_sha512.xlsx");
    let p = back.sheets[0].protection.as_ref().expect("protected");
    assert_eq!(p.modern.as_ref(), Some(&m));
    assert!(p.password_ok("Secret1") && !p.password_ok("") && !p.password_ok("wrong"));
    let q = back.sheets[1].protection.as_ref().expect("protected");
    assert_eq!((q.hash, q.modern.is_none()), (0xCC3D, true));
    assert!(!q.password_ok(""));
}

#[test]
fn protection_and_view_round_trip() {
    let mut wb = Workbook::empty_with(vec![Sheet::new("A"), Sheet::new("B")]);
    put(&mut wb, 0, 0, 0, "locked");
    put(&mut wb, 0, 1, 0, "=1+1");
    let open = wb.intern_style(Style { unlocked: true, ..Default::default() });
    let hidden = wb.intern_style(Style { hide_formula: true, ..Default::default() });
    wb.sheets[0].cell_mut(0, 1).style = open;
    wb.sheets[0].cell_mut(1, 0).style = hidden;
    wb.sheets[0].protection = Some(Protection { hash: legacy_password_hash("secret"), format_cells: true, ..Default::default() });
    wb.sheets[0].zoom = 1.5;
    wb.sheets[1].zoom = 0.75;
    wb.sheets[0].notes.insert((0, 0), "Check this".into());
    wb.sheets[0].note_authors.insert((0, 0), "Ann Smith".into());
    wb.sheets[0].notes.insert((2, 2), "Anonymous".into());

    let back = round_trip(&wb, "protect.xlsx");
    let s = &back.sheets[0];
    let p = s.protection.as_ref().expect("protected");
    assert_eq!(p.hash, legacy_password_hash("secret"));
    assert!(p.format_cells && !p.format_rows && p.select_locked);
    assert!(back.style(s.get(0, 1).unwrap().style).unlocked);
    assert!(!back.style(s.get(0, 0).unwrap().style).unlocked);
    assert!(back.style(s.get(1, 0).unwrap().style).hide_formula);
    assert!(s.is_locked(&back, 0, 0) && !s.is_locked(&back, 0, 1));
    assert!(back.sheets[1].protection.is_none());
    assert_eq!(s.zoom, 1.5);
    assert_eq!(back.sheets[1].zoom, 0.75);
    assert_eq!(s.notes.get(&(0, 0)).map(String::as_str), Some("Check this"));
    assert_eq!(s.note_authors.get(&(0, 0)).map(String::as_str), Some("Ann Smith"));
    assert_eq!(s.notes.get(&(2, 2)).map(String::as_str), Some("Anonymous"));
}

#[test]
fn picture_round_trip() {
    let mut wb = Workbook::new();
    wb.sheets[0].images.push(Picture {
        data: Arc::new(PNG.to_vec()),
        ext: "png".into(),
        anchor: (2, 1),
        offset: (5.0, 4.0),
        size: (120.0, 80.0),
        name: "Picture 1".into(),
        descr: "Logo".into(),
    });
    let back = round_trip(&wb, "picture.xlsx");
    let p = &back.sheets[0].images[0];
    assert_eq!(back.sheets[0].images.len(), 1);
    assert_eq!(p.ext, "png");
    assert_eq!(p.data.as_slice(), PNG.as_slice());
    assert_eq!(p.anchor, (2, 1));
    assert!((p.offset.0 - 5.0).abs() < 1.0 && (p.offset.1 - 4.0).abs() < 1.0, "{:?}", p.offset);
    assert!((p.size.0 - 120.0).abs() < 1.5 && (p.size.1 - 80.0).abs() < 1.5, "{:?}", p.size);
    assert_eq!(p.descr, "Logo");
}

#[test]
fn threaded_comments_become_notes() {
    use std::io::Write;
    // Minimal package: a sheet with a threaded comment and one reply.
    let path = temp("threaded.xlsx");
    let f = std::fs::File::create(&path).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let opt = zip::write::SimpleFileOptions::default();
    let mut add = |name: &str, body: &str| {
        z.start_file(name, opt).unwrap();
        z.write_all(body.as_bytes()).unwrap();
    };
    add("[Content_Types].xml", r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"#);
    add("_rels/.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#);
    add("xl/workbook.xml", r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>"#);
    add("xl/_rels/workbook.xml.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId9" Type="http://schemas.microsoft.com/office/2017/10/relationships/person" Target="persons/person.xml"/></Relationships>"#);
    add("xl/persons/person.xml", r#"<personList xmlns="http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments"><person displayName="Ann" id="{P1}"/><person displayName="Bob" id="{P2}"/></personList>"#);
    add("xl/worksheets/sheet1.xml", r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></worksheet>"#);
    add("xl/worksheets/_rels/sheet1.xml.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="../comments1.xml"/><Relationship Id="rId2" Type="http://schemas.microsoft.com/office/2017/10/relationships/threadedComment" Target="../threadedComments/threadedComment1.xml"/></Relationships>"#);
    add("xl/comments1.xml", r#"<comments xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><authors><author>tc={T1}</author></authors><commentList><comment ref="B2" authorId="0"><text><t>[Threaded comment] placeholder</t></text></comment></commentList></comments>"#);
    add("xl/threadedComments/threadedComment1.xml", r#"<ThreadedComments xmlns="http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments"><threadedComment ref="B2" dT="2024-01-01T00:00:00" personId="{P1}" id="{T1}"><text>Is this right?</text></threadedComment><threadedComment ref="B2" dT="2024-01-02T00:00:00" personId="{P2}" id="{T2}" parentId="{T1}"><text>Yes.</text></threadedComment></ThreadedComments>"#);
    z.finish().unwrap();

    let wb = load(&path).unwrap();
    let s = &wb.sheets[0];
    assert_eq!(s.notes.get(&(1, 1)).map(String::as_str), Some("Is this right?\n\nBob:\nYes."));
    assert_eq!(s.note_authors.get(&(1, 1)).map(String::as_str), Some("Ann"));
    // Saved back as a note with its author.
    let back = round_trip(&wb, "threaded_back.xlsx");
    assert_eq!(back.sheets[0].notes.get(&(1, 1)), s.notes.get(&(1, 1)));
    assert_eq!(back.sheets[0].note_authors.get(&(1, 1)).map(String::as_str), Some("Ann"));
}

#[test]
fn default_table_style_is_known() {
    assert_eq!(crate::table::style_parts("TableStyleMedium2"), Some((1, 1)));
}

/// Loads `FS_IN`, prints what was read and saves it to `FS_OUT` (manual check against real files).
#[test]
#[ignore]
fn resave_file() {
    let (Ok(src), Ok(dst)) = (std::env::var("FS_IN"), std::env::var("FS_OUT")) else { return };
    let mut wb = load(std::path::Path::new(&src)).unwrap();
    wb.recalc();
    for s in &wb.sheets {
        println!("sheet {} zoom={} names={:?} protected={:?}", s.name, s.zoom, s.names, s.protection.as_ref().map(|p| p.hash));
        for t in &s.tables {
            println!("  table {} {} style={} totals={} cols={:?}", t.name, t.range.to_a1(), t.style, t.totals, t.columns);
        }
        for p in &s.images {
            println!("  picture {} at {:?}+{:?} size {:?} ({} bytes)", p.ext, p.anchor, p.offset, p.size, p.data.len());
        }
        for (k, rt) in &s.rich {
            println!("  rich {k:?} {:?}", rt.runs);
        }
        for (k, n) in &s.notes {
            println!("  note {k:?} {n:?} by {:?}", s.note_authors.get(k));
        }
        for (r, row) in &s.rows {
            for (c, cell) in row {
                let f = cell.formula.as_ref().map(|f| f.text.as_str());
                println!("  {} = {:?} {f:?} unlocked={}", crate::model::cell_name(*r, *c), cell.value, wb.style(cell.style).unlocked);
            }
        }
    }
    println!("global names {:?}", wb.names);
    save(&wb, std::path::Path::new(&dst)).unwrap();
}
