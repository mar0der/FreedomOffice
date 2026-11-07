//! Threaded comments in the saved package, the way Excel stores them (DECISIONS.md #28):
//! a legacy comment per commented cell (author `tc={thread id}`, with the text Excel shows
//! older versions), `xl/threadedComments/threadedCommentN.xml` per sheet, and
//! `xl/persons/person.xml`. What the standard format cannot hold -- a second thread on the
//! same cell, a plain note on a cell that has a thread, when a comment was last edited --
//! goes in `xl/freedomoffice/comments.json`, which only this program reads.

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};

use cosync::Thread;
use serde_json::{json, Value};

use crate::model::{cell_name, Sheet, Workbook, MAX_COLS, MAX_ROWS};

pub(crate) const CUSTOM_PART: &str = "xl/freedomoffice/comments.json";
const NS: &str = "http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments";
const REL_TC: &str = "http://schemas.microsoft.com/office/2017/10/relationships/threadedComment";
const REL_PERSON: &str = "http://schemas.microsoft.com/office/2017/10/relationships/person";
const REL_CUSTOM: &str = "https://freedomoffice.app/relationships/comments";

fn esc(s: &str) -> String {
    let clean: String = s.chars().filter(|c| matches!(*c, '\t' | '\n' | '\r') || (*c >= ' ' && *c != '\u{FFFE}' && *c != '\u{FFFF}')).collect();
    clean.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The threads of a sheet that fit the file, by cell; the first of each cell is the threaded
/// comment, the others go in the custom part.
pub(super) fn by_cell(sh: &Sheet) -> BTreeMap<(u32, u32), Vec<&Thread>> {
    let mut out: BTreeMap<(u32, u32), Vec<&Thread>> = BTreeMap::new();
    for t in sh.threads.iter().filter(|t| t.r < MAX_ROWS && t.c < MAX_COLS && !t.comments.is_empty()) {
        out.entry((t.r, t.c)).or_default().push(t);
    }
    out
}

/// The legacy comment of a cell: text and author. Excel shows this to versions that don't know
/// threaded comments; LibreOffice shows it as a note.
pub(super) fn legacy_note(threads: &[&Thread], plain: Option<&str>) -> (String, String) {
    let mut s = String::from(
        "[Threaded comment]\n\nYour version of Excel allows you to read this threaded comment; however, any edits to it will get removed if the file is opened in a newer version of Excel. Learn more: https://go.microsoft.com/fwlink/?linkid=870924\n",
    );
    for (n, t) in threads.iter().enumerate() {
        if n > 0 {
            s.push_str("\nAnother comment thread:\n");
        }
        for (i, c) in t.comments.iter().enumerate() {
            let who = if c.name.is_empty() { c.by.as_str() } else { c.name.as_str() };
            s.push_str(&format!("\n{}{}:\n    {}\n", if i == 0 { "Comment" } else { "Reply" }, if who.is_empty() { String::new() } else { format!(" by {who}") }, c.text.replace('\n', "\n    ")));
        }
    }
    if let Some(p) = plain {
        s.push_str(&format!("\nNote:\n    {}\n", p.replace('\n', "\n    ")));
    }
    (s, format!("tc={}", threads[0].id))
}

/// A GUID derived from text (a person's id is the same wherever they are).
fn guid_of(text: &str) -> String {
    // FNV-1a twice with different seeds: 128 bits, version 4 / variant 1 bits set.
    let h = |seed: u64| text.bytes().fold(seed, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3));
    let (a, b) = (h(0xcbf2_9ce4_8422_2325), h(0x8422_2325_cbf2_9ce4));
    let a = (a & !0xF000) | 0x4000;
    let b = (b & 0x3FFF_FFFF_FFFF_FFFF) | 0x8000_0000_0000_0000;
    format!("{{{:08X}-{:04X}-{:04X}-{:04X}-{:012X}}}", (a >> 32) as u32, (a >> 16) as u16, a as u16, (b >> 48) as u16, b & 0xFFFF_FFFF_FFFF)
}

fn stamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.format("%Y-%m-%dT%H:%M:%S%.3f").to_string()).unwrap_or_else(|| "1970-01-01T00:00:00.000".into())
}

/// Adds the threaded-comment parts to the finished package.
pub(super) fn patch(wb: &Workbook, pkg: Vec<u8>) -> Result<Vec<u8>, String> {
    let cells: Vec<BTreeMap<(u32, u32), Vec<&Thread>>> = wb.sheets.iter().map(by_cell).collect();
    if cells.iter().all(BTreeMap::is_empty) {
        return Ok(pkg);
    }
    // People: one entry per (user, display name).
    let mut people: Vec<(String, String, String)> = Vec::new();
    let mut person = |by: &str, name: &str| -> String {
        if let Some(p) = people.iter().find(|p| p.1 == by && p.2 == name) {
            return p.0.clone();
        }
        let id = guid_of(&format!("{by}\u{0}{name}"));
        people.push((id.clone(), by.to_string(), name.to_string()));
        id
    };
    let mut parts_new: Vec<(String, String)> = Vec::new();
    let mut custom_threads: Vec<Value> = Vec::new();
    let mut custom_notes: Vec<Value> = Vec::new();
    let mut edited: BTreeMap<String, i64> = BTreeMap::new();
    for (si, (sh, by_cell)) in wb.sheets.iter().zip(&cells).enumerate() {
        if by_cell.is_empty() {
            continue;
        }
        let mut xml = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<ThreadedComments xmlns=\"{NS}\">");
        for (&(r, c), list) in by_cell {
            let t = list[0];
            let cell = cell_name(r, c);
            for (i, cm) in t.comments.iter().enumerate() {
                let pid = person(&cm.by, &cm.name);
                xml.push_str(&format!("<threadedComment ref=\"{cell}\" dT=\"{}\" personId=\"{pid}\" id=\"{}\"", stamp(cm.at), esc(&cm.id)));
                if i == 0 {
                    xml.push_str(&format!(" done=\"{}\"", t.done as u8));
                } else {
                    xml.push_str(&format!(" parentId=\"{}\"", esc(&t.id)));
                }
                xml.push_str(&format!("><text>{}</text></threadedComment>", esc(&cm.text)));
                if cm.edited != 0 {
                    edited.insert(cm.id.clone(), cm.edited);
                }
            }
            for extra in &list[1..] {
                custom_threads.push(json!({ "sheet": si, "thread": extra }));
                for cm in &extra.comments {
                    if cm.edited != 0 {
                        edited.insert(cm.id.clone(), cm.edited);
                    }
                }
            }
            if let Some(n) = sh.notes.get(&(r, c)) {
                custom_notes.push(json!({ "sheet": si, "r": r, "c": c, "text": n, "author": sh.note_authors.get(&(r, c)) }));
            }
        }
        xml.push_str("</ThreadedComments>");
        parts_new.push((format!("xl/threadedComments/threadedComment{}.xml", si + 1), xml));
    }
    let mut persons = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<personList xmlns=\"{NS}\">");
    for (id, by, name) in &people {
        let display = if name.is_empty() { by } else { name };
        persons.push_str(&format!("<person displayName=\"{}\" id=\"{id}\" userId=\"{}\" providerId=\"None\"/>", esc(display), esc(by)));
    }
    persons.push_str("</personList>");
    parts_new.push(("xl/persons/person.xml".into(), persons));
    let custom = !custom_threads.is_empty() || !custom_notes.is_empty() || !edited.is_empty();
    if custom {
        let v = json!({ "version": 1, "threads": custom_threads, "notes": custom_notes, "edited": edited });
        parts_new.push((CUSTOM_PART.into(), v.to_string()));
    }

    let mut zin = zip::ZipArchive::new(Cursor::new(pkg)).map_err(|e| e.to_string())?;
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..zin.len() {
        let mut f = zin.by_index(i).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        parts.push((f.name().map_err(|e| e.to_string())?.to_string(), buf));
    }
    let mut insert = |name: &str, before: &str, add: String| {
        if let Some(p) = parts.iter_mut().find(|p| p.0 == name) {
            let xml = String::from_utf8_lossy(&p.1).to_string();
            if let Some(at) = xml.rfind(before) {
                p.1 = format!("{}{add}{}", &xml[..at], &xml[at..]).into_bytes();
            }
        }
    };
    let mut types = String::new();
    for (name, _) in &parts_new {
        let ct = if name.starts_with("xl/persons/") {
            "application/vnd.ms-excel.person+xml"
        } else if name.ends_with(".json") {
            "application/json"
        } else {
            "application/vnd.ms-excel.threadedcomments+xml"
        };
        types.push_str(&format!("<Override PartName=\"/{name}\" ContentType=\"{ct}\"/>"));
    }
    insert("[Content_Types].xml", "</Types>", types);
    let mut rels = format!("<Relationship Id=\"rIdPerson1\" Type=\"{REL_PERSON}\" Target=\"persons/person.xml\"/>");
    if custom {
        rels.push_str(&format!("<Relationship Id=\"rIdFoComments1\" Type=\"{REL_CUSTOM}\" Target=\"freedomoffice/comments.json\"/>"));
    }
    insert("xl/_rels/workbook.xml.rels", "</Relationships>", rels);
    for (si, by_cell) in cells.iter().enumerate() {
        if !by_cell.is_empty() {
            let rel = format!("<Relationship Id=\"rIdTc{0}\" Type=\"{REL_TC}\" Target=\"../threadedComments/threadedComment{0}.xml\"/>", si + 1);
            insert(&format!("xl/worksheets/_rels/sheet{}.xml.rels", si + 1), "</Relationships>", rel);
        }
    }
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in parts.into_iter().chain(parts_new.into_iter().map(|(n, x)| (n, x.into_bytes()))) {
        out.start_file(name, opts).map_err(|e| e.to_string())?;
        out.write_all(&data).map_err(|e| e.to_string())?;
    }
    Ok(out.finish().map_err(|e| e.to_string())?.into_inner())
}
