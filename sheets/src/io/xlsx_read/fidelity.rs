//! Tables, threaded comments, rich text and sheet protection from xlsx parts.

use std::collections::HashMap;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use super::{attr, attr_bool, decode_ooxml_escapes, flag, local, parse_color, parse_ref, read_text, skip, Theme, XmlReader, INDEXED};
use crate::model::{PasswordHash, Protection, Range, RichRun, RichText, Sheet};
use crate::table::{Table, TableColumn, TotalFn};

// ---------------------------------------------------------------------------
// Rich text
// ---------------------------------------------------------------------------

/// Reads `<si>` / `<is>` / comment `<text>` content: the plain text and, when it has
/// formatted runs (`<r>`), the runs.
pub(super) fn read_runs(rd: &mut XmlReader, end: &str, theme: &Theme) -> (String, Option<RichText>) {
    let mut runs: Vec<RichRun> = Vec::new();
    let mut plain = String::new();
    let mut cur: Option<RichRun> = None;
    loop {
        match rd.read_event() {
            Ok(Event::Start(e)) => match local(&e) {
                "r" => cur = Some(RichRun::default()),
                "rPr" => {
                    if let Some(r) = &mut cur {
                        read_rpr(rd, theme, r);
                    } else {
                        skip(rd, &e);
                    }
                }
                "t" => {
                    let t = decode_ooxml_escapes(read_text(rd, "t"));
                    match &mut cur {
                        Some(r) => r.text.push_str(&t),
                        None => plain.push_str(&t),
                    }
                }
                "rPh" | "phoneticPr" => skip(rd, &e),
                _ => {}
            },
            Ok(Event::End(e)) => {
                let n: &str = e.name().0;
                match n.rsplit(':').next() {
                    Some("r") => {
                        if let Some(r) = cur.take() {
                            runs.push(r);
                        }
                    }
                    Some(x) if x == end => break,
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    if runs.is_empty() {
        return (plain, None);
    }
    if !plain.is_empty() {
        runs.insert(0, RichRun { text: plain, ..Default::default() });
    }
    let text: String = runs.iter().map(|r| r.text.as_str()).collect();
    (text, Some(RichText { runs }))
}

fn read_rpr(rd: &mut XmlReader, theme: &Theme, r: &mut RichRun) {
    loop {
        match rd.read_event() {
            Ok(Event::Start(c)) | Ok(Event::Empty(c)) => apply_rpr(&c, theme, r),
            Ok(Event::End(c)) if c.name().0.rsplit(':').next() == Some("rPr") => break,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

fn apply_rpr(c: &BytesStart, theme: &Theme, r: &mut RichRun) {
    match local(c) {
        "b" => r.bold = flag(c),
        "i" => r.italic = flag(c),
        "strike" => r.strike = flag(c),
        "u" => r.underline = attr(c, "val").map(|v| v != "none").unwrap_or(true),
        "sz" => r.size = attr(c, "val").and_then(|v| v.trim().parse::<f32>().ok()),
        "rFont" | "name" => r.font = attr(c, "val"),
        "color" => r.color = parse_color(c, theme, &INDEXED),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Sheet protection
// ---------------------------------------------------------------------------

/// `<sheetProtection>`: the attributes say what is *protected* (1) rather than allowed.
pub(super) fn parse_protection(e: &BytesStart) -> Option<Protection> {
    if !attr_bool(e, "sheet").unwrap_or(false) {
        return None;
    }
    let prot = |name: &str, def: bool| attr_bool(e, name).unwrap_or(def);
    // Excel 2013+ writes an iterated SHA-512 hash instead of the legacy `password`.
    let modern = match (attr(e, "algorithmName"), attr(e, "hashValue"), attr(e, "saltValue")) {
        (Some(algorithm), Some(hash), Some(salt)) => Some(PasswordHash {
            algorithm,
            hash,
            salt,
            spin: attr(e, "spinCount").and_then(|v| v.trim().parse().ok()).unwrap_or(0),
        }),
        _ => None,
    };
    Some(Protection {
        hash: attr(e, "password").and_then(|v| u16::from_str_radix(v.trim(), 16).ok()).unwrap_or(0),
        modern,
        select_locked: !prot("selectLockedCells", false),
        select_unlocked: !prot("selectUnlockedCells", false),
        format_cells: !prot("formatCells", true),
        format_columns: !prot("formatColumns", true),
        format_rows: !prot("formatRows", true),
        insert_columns: !prot("insertColumns", true),
        insert_rows: !prot("insertRows", true),
        insert_links: !prot("insertHyperlinks", true),
        delete_columns: !prot("deleteColumns", true),
        delete_rows: !prot("deleteRows", true),
        sort: !prot("sort", true),
        autofilter: !prot("autoFilter", true),
        pivot_tables: !prot("pivotTables", true),
        edit_objects: !prot("objects", false),
        edit_scenarios: !prot("scenarios", false),
    })
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

pub(super) fn parse_table(xml: &str) -> Option<Table> {
    let mut rd = Reader::from_str(xml);
    let mut t: Option<Table> = None;
    loop {
        let ev = rd.read_event();
        match ev {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let is_start = matches!(ev, Ok(Event::Start(_)));
                match local(e) {
                    "table" => {
                        let range = attr(e, "ref").and_then(|v| Range::parse(&v))?;
                        let name = attr(e, "displayName").or_else(|| attr(e, "name")).unwrap_or_else(|| "Table1".into());
                        let mut tb = Table::new(&name, range, "");
                        tb.columns.clear();
                        tb.header = attr(e, "headerRowCount").map(|v| v.trim() != "0").unwrap_or(true);
                        tb.totals = attr(e, "totalsRowCount").is_some_and(|v| v.trim() != "0");
                        tb.filter = false;
                        tb.banded_rows = false;
                        t = Some(tb);
                    }
                    "autoFilter" => {
                        if let Some(tb) = &mut t {
                            tb.filter = true;
                        }
                        if is_start {
                            skip(&mut rd, e);
                        }
                    }
                    "tableColumn" => {
                        let mut col = TableColumn {
                            name: decode_ooxml_escapes(attr(e, "name").unwrap_or_default()),
                            total: TotalFn::from_xml(&attr(e, "totalsRowFunction").unwrap_or_default()),
                            total_label: attr(e, "totalsRowLabel").unwrap_or_default(),
                        };
                        if is_start {
                            // calculatedColumnFormula / totalsRowFormula: the cells carry the formulas.
                            skip(&mut rd, e);
                        }
                        if col.total == TotalFn::Custom && !col.total_label.is_empty() {
                            col.total = TotalFn::None;
                        }
                        if let Some(tb) = &mut t {
                            tb.columns.push(col);
                        }
                    }
                    "tableStyleInfo" => {
                        if let Some(tb) = &mut t {
                            tb.style = attr(e, "name").unwrap_or_default();
                            tb.first_col = attr_bool(e, "showFirstColumn").unwrap_or(false);
                            tb.last_col = attr_bool(e, "showLastColumn").unwrap_or(false);
                            tb.banded_rows = attr_bool(e, "showRowStripes").unwrap_or(false);
                            tb.banded_cols = attr_bool(e, "showColumnStripes").unwrap_or(false);
                        }
                    }
                    "extLst" if is_start => skip(&mut rd, e),
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    let mut t = t?;
    t.columns.resize_with(t.range.cols() as usize, Default::default);
    t.fix_column_names();
    Some(t)
}

// ---------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------

/// Legacy comments (notes) with their authors.
pub(super) fn parse_comments(xml: &str, theme: &Theme, sheet: &mut Sheet) {
    let mut rd = Reader::from_str(xml);
    let mut authors: Vec<String> = Vec::new();
    loop {
        match rd.read_event() {
            Ok(Event::Start(e)) if local(&e) == "author" => authors.push(read_text(&mut rd, "author")),
            Ok(Event::Start(e)) if local(&e) == "comment" => {
                let at = attr(&e, "ref").and_then(|s| parse_ref(&s));
                let author = attr(&e, "authorId").and_then(|v| v.parse::<usize>().ok()).and_then(|i| authors.get(i).cloned());
                let (text, _) = read_runs(&mut rd, "comment", theme);
                if let Some(rc) = at {
                    // Excel puts "Author:\n" at the start of a note's text.
                    let text = match &author {
                        Some(a) if !a.is_empty() => text.strip_prefix(&format!("{a}:\n")).map(str::to_string).unwrap_or(text),
                        _ => text,
                    };
                    sheet.notes.insert(rc, text);
                    if let Some(a) = author.filter(|a| !a.is_empty()) {
                        sheet.note_authors.insert(rc, a);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

/// A person of `xl/persons/person.xml`: display name and user id.
#[derive(Clone, Debug, Default)]
pub(super) struct Person {
    pub name: String,
    pub user: String,
}

/// `xl/persons/person.xml`: person id -> who.
pub(super) fn parse_persons(xml: &str) -> HashMap<String, Person> {
    let mut out = HashMap::new();
    let mut rd = Reader::from_str(xml);
    loop {
        match rd.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if local(&e) == "person" => {
                if let Some(id) = attr(&e, "id") {
                    let name = attr(&e, "displayName").unwrap_or_default();
                    // (Excel's own user ids are account ids; for people without one the name is all there is.)
                    let user = attr(&e, "userId").filter(|u| !u.is_empty()).unwrap_or_else(|| name.clone());
                    out.insert(id, Person { name, user });
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// `2024-01-02T10:30:00.12` (UTC, with or without `Z`) as milliseconds since 1970.
fn parse_stamp(s: &str) -> i64 {
    chrono::NaiveDateTime::parse_from_str(s.trim().trim_end_matches('Z'), "%Y-%m-%dT%H:%M:%S%.f").map(|d| d.and_utc().timestamp_millis()).unwrap_or(0)
}

/// Threaded comments: a thread per first comment, with its replies, on the cell the file puts
/// them on. The legacy comment that stands for a thread in the same file is dropped.
pub(super) fn parse_threaded(xml: &str, persons: &HashMap<String, Person>, sheet: &mut Sheet) {
    struct Tc {
        at: (u32, u32),
        parent: Option<String>,
        done: bool,
        cm: cosync::Comment,
    }
    let mut list: Vec<Tc> = Vec::new();
    let mut rd = Reader::from_str(xml);
    loop {
        match rd.read_event() {
            Ok(Event::Start(e)) if local(&e) == "threadedComment" => {
                let at = attr(&e, "ref").and_then(|s| parse_ref(&s));
                let who = attr(&e, "personId").and_then(|p| persons.get(&p).cloned()).unwrap_or_default();
                let mut cm = cosync::Comment { id: attr(&e, "id").unwrap_or_default(), by: who.user, name: who.name, at: attr(&e, "dT").map_or(0, |d| parse_stamp(&d)), ..Default::default() };
                let (parent, done) = (attr(&e, "parentId"), attr(&e, "done").is_some_and(|d| d == "1" || d.eq_ignore_ascii_case("true")));
                loop {
                    match rd.read_event() {
                        Ok(Event::Start(c)) if local(&c) == "text" => cm.text = decode_ooxml_escapes(read_text(&mut rd, "text")),
                        Ok(Event::End(c)) if c.name().0.rsplit(':').next() == Some("threadedComment") => break,
                        Ok(Event::Eof) | Err(_) => break,
                        _ => {}
                    }
                }
                if let Some(at) = at {
                    list.push(Tc { at, parent, done, cm });
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    for (n, root) in list.iter().enumerate().filter(|(_, c)| c.parent.is_none()) {
        let mut first = root.cm.clone();
        if first.id.is_empty() {
            first.id = format!("{{legacy-{}-{}-{n}}}", root.at.0, root.at.1);
        }
        let mut comments = vec![first.clone()];
        comments.extend(list.iter().filter(|c| c.parent.as_deref() == Some(root.cm.id.as_str()) && !root.cm.id.is_empty()).map(|c| c.cm.clone()));
        let th = cosync::Thread { id: first.id, r: root.at.0, c: root.at.1, done: root.done, comments };
        cosync::apply_thread_op(&mut sheet.threads, &cosync::Op::ThreadAdd { s: 0, r: root.at.0, c: root.at.1, th: Box::new(th) });
        // The legacy comment is only the stand-in Excel keeps for older versions.
        if sheet.note_authors.get(&root.at).is_some_and(|a| a.starts_with("tc=")) || sheet.notes.get(&root.at).is_some_and(|t| t.starts_with("[Threaded comment]")) {
            sheet.notes.remove(&root.at);
            sheet.note_authors.remove(&root.at);
        }
    }
}

/// `xl/freedomoffice/comments.json` (see the writer): second threads on a cell, plain notes on
/// cells that have a thread, and when comments were last edited.
pub(super) fn apply_custom(json: &str, sheets: &mut [Sheet]) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return };
    for t in v["threads"].as_array().into_iter().flatten() {
        let (Some(si), Ok(th)) = (t["sheet"].as_u64(), serde_json::from_value::<cosync::Thread>(t["thread"].clone())) else { continue };
        if let Some(sh) = sheets.get_mut(si as usize) {
            let (r, c) = (th.r, th.c);
            cosync::apply_thread_op(&mut sh.threads, &cosync::Op::ThreadAdd { s: 0, r, c, th: Box::new(th) });
        }
    }
    for n in v["notes"].as_array().into_iter().flatten() {
        let (Some(si), Some(r), Some(c), Some(text)) = (n["sheet"].as_u64(), n["r"].as_u64(), n["c"].as_u64(), n["text"].as_str()) else { continue };
        if let Some(sh) = sheets.get_mut(si as usize) {
            let k = (r as u32, c as u32);
            sh.notes.entry(k).or_insert_with(|| text.to_string());
            if let Some(a) = n["author"].as_str() {
                sh.note_authors.entry(k).or_insert_with(|| a.to_string());
            }
        }
    }
    if let Some(ed) = v["edited"].as_object() {
        for sh in sheets.iter_mut() {
            for cm in sh.threads.iter_mut().flat_map(|t| t.comments.iter_mut()) {
                if let Some(ms) = ed.get(&cm.id).and_then(serde_json::Value::as_i64) {
                    cm.edited = ms;
                }
            }
        }
    }
}
