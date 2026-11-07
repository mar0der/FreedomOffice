//! What an agent does with an open workbook: look at sheets, read and write ranges, calculate a
//! formula, add sheets, export. The command line and the MCP server are two front ends of these
//! functions, so both behave the same. Everything is the app's own engine (`freedom_sheets`).

use std::path::Path;
use std::time::Duration;

use cosync::{CellData, Op, Rect, SheetId};
use freedom_sheets::formula;
use freedom_sheets::live;
use freedom_sheets::model::{self, Cell, ErrorKind, Range, Value, Workbook};
use serde_json::{Value as Json, json};

use crate::a1::{self, parse_ref};
use crate::api::{Api, Item};
use crate::live::Live;

/// Most cells one read or write may touch (keeps replies and batches a sensible size).
pub const MAX_CELLS: u64 = 50_000;

pub fn item_json(i: &Item) -> Json {
    json!({
        "id": i.id,
        "name": i.name,
        "kind": if i.folder { "folder" } else { "file" },
        "version": i.version,
        "role": i.role,
        "size": i.size,
    })
}

/// The places to start from, or the contents of a folder.
pub fn list_files(api: &Api, folder: Option<&str>) -> Result<Json, String> {
    match folder {
        Some(f) => Ok(json!({ "folder": f, "items": api.children(f)?.iter().map(item_json).collect::<Vec<_>>() })),
        None => {
            let orgs: Vec<Json> = api
                .org_roots()?
                .into_iter()
                .map(|(name, id)| json!({ "name": name, "id": id }))
                .collect();
            let shared: Vec<Json> = api.shared_with_me()?.iter().map(item_json).collect();
            Ok(json!({
                "my_files": { "id": api.root()? },
                "organisation_folders": orgs,
                "shared_with_me": shared,
            }))
        }
    }
}

/// A new, empty workbook as .xlsx bytes, with these sheets (one "Sheet1" when none are named).
pub fn new_workbook(sheets: &[String]) -> Result<Vec<u8>, String> {
    let mut wb = Workbook::new();
    if !sheets.is_empty() {
        wb.sheets.clear();
        for n in sheets {
            check_sheet_name(&wb, n)?;
            wb.sheets.push(model::Sheet::new(n));
        }
    }
    freedom_sheets::io::xlsx_bytes(&wb)
}

/// Creates a workbook file in a folder.
pub fn create_file(api: &Api, folder: &str, name: &str, sheets: &[String]) -> Result<Item, String> {
    let name = if name.to_ascii_lowercase().ends_with(".xlsx") {
        name.to_string()
    } else {
        format!("{name}.xlsx")
    };
    api.upload_new(folder, &name, new_workbook(sheets)?)
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// A value as JSON: numbers, text and booleans as themselves, errors as their text ("#DIV/0!"),
/// nothing as null.
pub fn value_json(v: &Value) -> Json {
    match v {
        Value::Empty => Json::Null,
        Value::Number(n) => serde_json::Number::from_f64(*n)
            .map(Json::Number)
            .unwrap_or_else(|| json!(ErrorKind::Num.as_str())),
        Value::Text(t) => json!(t),
        Value::Bool(b) => json!(b),
        Value::Error(e) => json!(e.as_str()),
    }
}

fn display(wb: &Workbook, r: u32, c: u32, sh: usize) -> String {
    let s = &wb.sheets[sh];
    let style = s.get(r, c).map_or(0, |x| x.style);
    freedom_sheets::numfmt::format_value(s.display_value(r, c), &wb.style(style).num_fmt).text
}

/// A cell's text as a tab separated line shows it.
pub fn cell_text(wb: &Workbook, sheet: usize, r: u32, c: u32) -> String {
    display(wb, r, c, sheet)
}

// ---------------------------------------------------------------------------
// Sheets
// ---------------------------------------------------------------------------

/// The index of a sheet by name (any case) or numeric id; the first sheet when none is named.
pub fn find_sheet(wb: &Workbook, name: Option<&str>) -> Result<usize, String> {
    let Some(n) = name.map(str::trim).filter(|n| !n.is_empty()) else {
        return if wb.sheets.is_empty() { Err("The workbook has no sheet.".into()) } else { Ok(0) };
    };
    wb.sheets
        .iter()
        .position(|s| s.name.eq_ignore_ascii_case(n) || s.uid.to_string() == n)
        .ok_or_else(|| {
            format!(
                "No sheet called '{n}'. Sheets: {}.",
                wb.sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
            )
        })
}

pub fn list_sheets(live: &mut Live) -> Json {
    let wb = live.workbook();
    let list: Vec<Json> = wb
        .sheets
        .iter()
        .map(|s| {
            let used = s.used_range();
            json!({
                "name": s.name,
                "id": s.uid,
                "used_range": used.map(|r| r.to_a1()),
                "rows": used.map_or(0, |r| r.r2 + 1),
                "columns": used.map_or(0, |r| r.c2 + 1),
            })
        })
        .collect();
    json!({ "sheets": list })
}

fn check_sheet_name(wb: &Workbook, name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 31 || n.chars().any(|c| "[]:*?/\\".contains(c)) || n.starts_with('\'') || n.ends_with('\'') {
        return Err("A sheet name has 1 to 31 characters and none of [ ] : * ? / \\".into());
    }
    if wb.sheet_index(n).is_some() {
        return Err(format!("There is already a sheet called '{n}'."));
    }
    Ok(())
}

pub fn add_sheet(live: &mut Live, name: &str) -> Result<Json, String> {
    need_editor(live)?;
    check_sheet_name(&live.wb, name)?;
    let name = name.trim().to_string();
    let uid = model::new_sheet_uid();
    let pos = live.wb.sheets.iter().map(|s| s.pos).fold(-1.0, f64::max) + 1.0;
    live.edit(vec![Op::AddSheet { s: uid, name: name.clone(), pos }]);
    Ok(json!({ "added": name, "id": uid }))
}

fn need_editor(live: &Live) -> Result<(), String> {
    if live.editor { Ok(()) } else { Err("You have view-only access to this workbook.".into()) }
}

// ---------------------------------------------------------------------------
// Ranges
// ---------------------------------------------------------------------------

/// Resolves "Sheet!A1:B2" (or the `sheet` argument) and a range to (sheet index, range). No
/// range means the sheet's used range. Whole-column and whole-row ranges are cut to it.
fn resolve(wb: &Workbook, sheet: Option<&str>, range: Option<&str>) -> Result<(usize, Range), String> {
    let parsed = range.filter(|r| !r.trim().is_empty()).map(parse_ref).transpose()?;
    let name = parsed.as_ref().and_then(|p| p.sheet.as_deref()).or(sheet);
    let si = find_sheet(wb, name)?;
    let used = wb.sheets[si].used_range();
    let rg = match parsed {
        Some(p) => {
            let mut rg = p.range;
            let u = used.unwrap_or(Range::cell(0, 0));
            if rg.is_full_cols() {
                rg.r2 = u.r2;
            }
            if rg.is_full_rows() {
                rg.c2 = u.c2;
            }
            rg
        }
        None => used.ok_or("The sheet is empty.")?,
    };
    if u64::from(rg.rows()) * u64::from(rg.cols()) > MAX_CELLS {
        return Err(format!("{} is more than {MAX_CELLS} cells; use a smaller range.", rg.to_a1()));
    }
    Ok((si, rg))
}

/// Tells the other people in the session where this agent is working.
pub fn show_presence(live: &mut Live, sheet: SheetId, rg: Range, editing: bool) {
    if live.connected() {
        let sel = Rect { r1: rg.r1, c1: rg.c1, r2: rg.r2, c2: rg.c2 };
        live.send_selection(sheet, rg.r1, rg.c1, sel, editing);
    }
}

/// Values (calculated), formulas and, on request, the text as formatted, of a range.
pub fn read_range(live: &mut Live, sheet: Option<&str>, range: Option<&str>, with_display: bool) -> Result<Json, String> {
    let (si, rg) = resolve(live.workbook(), sheet, range)?;
    let uid = live.wb.sheets[si].uid;
    show_presence(live, uid, rg, false);
    let wb = live.workbook();
    let sh = &wb.sheets[si];
    let (mut values, mut formulas, mut shown) = (Vec::new(), Vec::new(), Vec::new());
    for r in rg.r1..=rg.r2 {
        let (mut v, mut f, mut d) = (Vec::new(), Vec::new(), Vec::new());
        for c in rg.c1..=rg.c2 {
            v.push(value_json(sh.display_value(r, c)));
            f.push(match sh.get(r, c).and_then(|x| x.formula.as_ref()) {
                Some(x) => json!(format!("={}", x.text)),
                None => Json::Null,
            });
            if with_display {
                d.push(json!(display(wb, r, c, si)));
            }
        }
        values.push(v);
        formulas.push(f);
        shown.push(d);
    }
    let mut out = json!({ "sheet": sh.name, "range": rg.to_a1(), "values": values, "formulas": formulas });
    if with_display {
        out["display"] = json!(shown);
    }
    Ok(out)
}

/// The cell a JSON value makes: strings are read like typing them in the app ("=..." is a
/// formula, "12" a number, "'12" text), numbers and booleans are taken as they are, null clears.
/// The cell keeps its existing format.
fn cell_from_json(wb: &mut Workbook, v: &Json, style: u32, at: (u32, u32)) -> Result<Option<Cell>, String> {
    let name = || a1::cell_name(at.0, at.1);
    Ok(match v {
        Json::Null => model::cell_from_input(wb, "", style),
        Json::Bool(b) => Some(Cell { value: Value::Bool(*b), formula: None, style }),
        Json::Number(n) => Some(Cell {
            value: Value::Number(n.as_f64().filter(|f| f.is_finite()).ok_or_else(|| format!("{}: not a usable number", name()))?),
            formula: None,
            style,
        }),
        Json::String(s) => {
            if let Some(f) = s.strip_prefix('=').filter(|f| !f.trim().is_empty())
                && let Err(e) = formula::parse(f)
            {
                return Err(format!("{}: the formula {s} can't be read ({e})", name()));
            }
            model::cell_from_input(wb, s, style)
        }
        _ => return Err(format!("{}: a cell is a string, number, boolean or null", name())),
    })
}

/// Writes a grid of values with its top-left cell at the start of `range` (a single cell, or a
/// range of exactly the grid's size). Nothing is written when any entry is invalid. Returns the
/// calculated values of what was written.
pub fn write_range(live: &mut Live, sheet: Option<&str>, range: &str, grid: &[Vec<Json>]) -> Result<Json, String> {
    need_editor(live)?;
    let p = parse_ref(range)?;
    let name = p.sheet.as_deref().or(sheet);
    let si = find_sheet(&live.wb, name)?;
    let rows = grid.len() as u32;
    let cols = grid.iter().map(Vec::len).max().unwrap_or(0) as u32;
    if rows == 0 || cols == 0 {
        return Err("values is empty.".into());
    }
    if u64::from(rows) * u64::from(cols) > MAX_CELLS {
        return Err(format!("More than {MAX_CELLS} cells; write in smaller pieces."));
    }
    let at = p.range;
    if !at.is_single() && (at.rows() != rows || at.cols() != cols) {
        return Err(format!("{} is {}x{} cells but values is {rows}x{cols}.", at.to_a1(), at.rows(), at.cols()));
    }
    let (r0, c0) = (at.r1, at.c1);
    if u64::from(r0) + u64::from(rows) > u64::from(model::MAX_ROWS) || u64::from(c0) + u64::from(cols) > u64::from(model::MAX_COLS) {
        return Err("That runs off the end of the sheet.".into());
    }
    let uid = live.wb.sheets[si].uid;
    // Build every cell first: one bad entry stops the whole write.
    let mut ops = Vec::new();
    for (i, row) in grid.iter().enumerate() {
        for (j, v) in row.iter().enumerate() {
            let (r, c) = (r0 + i as u32, c0 + j as u32);
            let style = live.wb.sheets[si].get(r, c).map_or(0, |x| x.style);
            let cell = cell_from_json(&mut live.wb, v, style, (r, c))?;
            let new = cell.map(|cell| {
                let sh = &live.wb.sheets[si];
                let mut d: CellData = live::cell_data(&live.wb, sh, r, c, &cell);
                d.rich = None;
                Box::new(d)
            });
            // Unchanged cells make no operations.
            if new.as_deref() != live.cell_data(uid, r, c).as_ref() {
                ops.push(Op::Cell { s: uid, r, c, v: new, adj: vec![] });
            }
        }
    }
    let changed = ops.len();
    let rg = Range::new(r0, c0, r0 + rows - 1, c0 + cols - 1);
    live.edit(ops);
    show_presence(live, uid, rg, false);
    let wb = live.workbook();
    let sh = &wb.sheets[si];
    let values: Vec<Vec<Json>> = (rg.r1..=rg.r2)
        .map(|r| (rg.c1..=rg.c2).map(|c| value_json(sh.display_value(r, c))).collect())
        .collect();
    Ok(json!({ "sheet": sh.name, "range": rg.to_a1(), "cells_changed": changed, "values": values }))
}

/// Calculates a formula against the workbook, as if typed in A1 of a sheet. Nothing is changed.
pub fn evaluate(live: &mut Live, sheet: Option<&str>, src: &str) -> Result<Json, String> {
    let si = find_sheet(live.workbook(), sheet)?;
    let text = src.trim().trim_start_matches('=');
    let ast = formula::parse(text).map_err(|e| format!("The formula can't be read: {e}"))?;
    let wb = live.workbook();
    let v = formula::eval_expr(wb, si, 0, 0, &ast);
    let shown = freedom_sheets::numfmt::format_value(&v, "").text;
    Ok(json!({ "formula": format!("={text}"), "sheet": wb.sheets[si].name, "value": value_json(&v), "display": shown }))
}

/// Writes the workbook to a local file: .xlsx, or .csv/.tsv of one sheet. Existing files are
/// kept unless `overwrite`.
pub fn export(live: &mut Live, path: &Path, sheet: Option<&str>, overwrite: bool) -> Result<Json, String> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if !matches!(ext.as_str(), "xlsx" | "csv" | "tsv") {
        return Err("The file name must end in .xlsx, .csv or .tsv.".into());
    }
    if path.exists() && !overwrite {
        return Err(format!("{} exists; pass overwrite to replace it.", path.display()));
    }
    let si = find_sheet(live.workbook(), sheet)?;
    let mut wb = live.workbook().clone();
    wb.active = si;
    // A save reads only what is saved: no link back to the file, no live bookkeeping.
    wb.path = None;
    freedom_sheets::io::save(&wb, path)?;
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok(json!({
        "path": path.display().to_string(),
        "format": ext,
        "sheet": if ext == "xlsx" { Json::Null } else { json!(wb.sheets[si].name) },
        "bytes": size,
    }))
}

// ---------------------------------------------------------------------------
// Session upkeep
// ---------------------------------------------------------------------------

/// Waits until the server has every change, then uploads a snapshot when there are changes no
/// snapshot covers (as the app does when it leaves). Returns the file's version if one was made.
pub fn settle(live: &mut Live) -> Result<Option<i64>, String> {
    if !live.wait_synced(Duration::from_secs(20)) {
        return Err(live
            .stopped
            .clone()
            .or_else(|| live.last_error.clone())
            .unwrap_or_else(|| "The server didn't confirm the changes.".into()));
    }
    if live.unsaved() && live.editor { live.snapshot().map(Some) } else { Ok(None) }
}

/// Who is in the session, with the range each is working on.
pub fn peers(live: &Live) -> Json {
    let name_of = |s: SheetId| live.wb.sheets.iter().find(|x| x.uid == s).map(|x| x.name.clone());
    let one = |p: &cosync::Peer, you: bool| {
        let at = p.presence.as_ref().map(|pr| {
            let rg = Range::new(pr.sel.r1, pr.sel.c1, pr.sel.r2, pr.sel.c2);
            json!({ "sheet": name_of(pr.s), "range": rg.to_a1(), "editing": pr.editing })
        });
        json!({ "name": p.name, "user": p.user, "agent": p.agent, "editor": p.editor, "you": you, "working_on": at })
    };
    let mut list: Vec<Json> = live.me.iter().map(|p| one(p, true)).collect();
    list.extend(live.peers.values().filter(|p| Some(p.id) != live.me.as_ref().map(|m| m.id)).map(|p| one(p, false)));
    json!({ "people": list })
}

/// The version list of a file, newest first.
pub fn versions(api: &Api, item: &str) -> Result<Json, String> {
    let mut v = api.versions(item)?;
    v.sort_by_key(|x| std::cmp::Reverse(x["version"].as_i64().unwrap_or(0)));
    Ok(json!({ "versions": v }))
}
