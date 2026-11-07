//! Comment threads for agents: list, start, reply to and resolve. The command line
//! (`fsheets comments ...`) and the MCP tools (`list_comments`, `add_comment`,
//! `reply_comment`, `resolve_comment`) are two front ends of these functions. An agent's
//! comments show as an agent in the app: its name ends in "(agent)".

use cosync::Thread;
use freedom_sheets::live::comments::{self, Who};
use freedom_sheets::model::{cell_name, Range};
use serde_json::{Value as Json, json};

use crate::a1::parse_ref;
use crate::live::Live;
use crate::work::find_sheet;

/// `2026-11-07T14:05:09Z` for milliseconds since 1970.
pub fn iso_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn who(live: &Live) -> Result<Who, String> {
    if !live.editor {
        return Err("You have view-only access to this workbook, so you can't write comments.".into());
    }
    if !live.comments {
        return Err("This server can't share comments yet (it needs to be updated).".into());
    }
    let me = live.me.as_ref().ok_or("Not connected to the live session yet.")?;
    let name = if me.name.ends_with("(agent)") { me.name.clone() } else { format!("{} (agent)", me.name) };
    Ok(Who { user: me.user.clone(), name, manager: me.manager })
}

fn thread_json(sheet: &str, t: &Thread) -> Json {
    let comments: Vec<Json> = t
        .comments
        .iter()
        .map(|c| {
            let mut o = json!({ "id": c.id, "author": c.by, "name": c.name, "at": iso_utc(c.at), "text": c.text });
            if c.edited != 0 {
                o["edited"] = json!(iso_utc(c.edited));
            }
            o
        })
        .collect();
    json!({ "id": t.id, "sheet": sheet, "cell": cell_name(t.r, t.c), "resolved": t.done, "comments": comments })
}

/// The comment threads of a sheet (every sheet without `sheet`), by position. `open_only`:
/// leave out resolved threads.
pub fn list(live: &mut Live, sheet: Option<&str>, open_only: bool) -> Result<Json, String> {
    let idx: Vec<usize> = match sheet {
        Some(_) => vec![find_sheet(&live.wb, sheet)?],
        None => (0..live.wb.sheets.len()).collect(),
    };
    let mut out = Vec::new();
    for i in idx {
        let sh = &live.wb.sheets[i];
        let mut threads: Vec<&Thread> = sh.threads.iter().filter(|t| !(open_only && t.done)).collect();
        threads.sort_by_key(|t| (t.r, t.c, t.comments[0].at));
        out.extend(threads.into_iter().map(|t| thread_json(&sh.name, t)));
    }
    Ok(json!({ "count": out.len(), "threads": out }))
}

fn locate(live: &Live, thread: &str) -> Result<(usize, String), String> {
    live.wb
        .sheets
        .iter()
        .enumerate()
        .find(|(_, s)| s.threads.iter().any(|t| t.id == thread))
        .map(|(i, s)| (i, s.name.clone()))
        .ok_or_else(|| format!("There is no comment thread {thread}. Use list_comments for the ids."))
}

/// Starts a thread on a cell (`Sheet!B2`, or B2 with `sheet`).
pub fn add(live: &mut Live, sheet: Option<&str>, cell: &str, text: &str) -> Result<Json, String> {
    let w = who(live)?;
    if text.trim().is_empty() {
        return Err("The comment is empty.".into());
    }
    let p = parse_ref(cell)?;
    if !p.range.is_single() {
        return Err("A comment goes on one cell, like B2.".into());
    }
    let si = find_sheet(&live.wb, p.sheet.as_deref().or(sheet))?;
    let Range { r1: r, c1: c, .. } = p.range;
    let uid = live.wb.sheets[si].uid;
    let op = comments::add_thread(uid, r, c, &w, text.trim());
    let cosync::Op::ThreadAdd { th, .. } = &op else { unreachable!() };
    let id = th.id.clone();
    live.edit(vec![op]);
    Ok(json!({ "thread": id, "sheet": live.wb.sheets[si].name, "cell": cell_name(r, c) }))
}

/// Replies to a thread.
pub fn reply(live: &mut Live, thread: &str, text: &str) -> Result<Json, String> {
    let w = who(live)?;
    if text.trim().is_empty() {
        return Err("The reply is empty.".into());
    }
    let (si, name) = locate(live, thread)?;
    let op = comments::reply(live.wb.sheets[si].uid, thread, &w, text.trim());
    let cosync::Op::CommentAdd { cm, .. } = &op else { unreachable!() };
    let id = cm.id.clone();
    live.edit(vec![op]);
    Ok(json!({ "thread": thread, "comment": id, "sheet": name }))
}

/// Resolves a thread, or (`resolved` false) reopens it.
pub fn resolve(live: &mut Live, thread: &str, resolved: bool) -> Result<Json, String> {
    who(live)?;
    let (si, name) = locate(live, thread)?;
    live.edit(vec![comments::resolve(live.wb.sheets[si].uid, thread, resolved)]);
    Ok(json!({ "thread": thread, "resolved": resolved, "sheet": name }))
}

#[cfg(test)]
mod tests {
    use super::iso_utc;

    #[test]
    fn times_are_iso_utc() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(1_704_067_200_000), "2024-01-01T00:00:00Z");
        assert_eq!(iso_utc(1_709_164_799_999), "2024-02-28T23:59:59Z");
        assert_eq!(iso_utc(1_709_251_200_000), "2024-02-29T00:00:00Z");
        assert_eq!(iso_utc(4_102_444_800_000), "2100-01-01T00:00:00Z");
    }
}
