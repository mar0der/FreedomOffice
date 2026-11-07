//! Threaded comments in live sessions: building the operations (the app and `fsheets` both
//! use these), and setting who made them. The rules for applying them are in
//! `cosync::threads`.

use cosync::{Comment, Op, SheetId, Thread};

use crate::model::Sheet;

/// Who is commenting.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Who {
    /// User name ("" for someone with a link, who can't edit or delete comments).
    pub user: String,
    /// Display name.
    pub name: String,
    /// May delete anyone's comments.
    pub manager: bool,
}

/// A fresh comment id in the GUID form Excel uses: `{8F3A9B7C-1D2E-4F50-A1B2-C3D4E5F60718}`.
pub fn new_guid() -> String {
    use std::hash::{BuildHasher, Hasher};
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let word = || {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.finish()
    };
    let (a, b) = (word(), word());
    // Version 4, variant 1.
    let a = (a & !0xF000) | 0x4000;
    let b = (b & 0x3FFF_FFFF_FFFF_FFFF) | 0x8000_0000_0000_0000;
    format!("{{{:08X}-{:04X}-{:04X}-{:04X}-{:012X}}}", (a >> 32) as u32, (a >> 16) as u16, a as u16, (b >> 48) as u16, b & 0xFFFF_FFFF_FFFF)
}

/// Milliseconds since 1970 (UTC).
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn comment(who: &Who, text: &str) -> Comment {
    Comment { id: new_guid(), by: who.user.clone(), name: who.name.clone(), at: now_ms(), text: text.to_string(), edited: 0 }
}

/// A new thread on cell (`r`, `c`) with `text` as its first comment.
pub fn add_thread(s: SheetId, r: u32, c: u32, who: &Who, text: &str) -> Op {
    let first = comment(who, text);
    Op::ThreadAdd { s, r, c, th: Box::new(Thread { id: first.id.clone(), r, c, done: false, comments: vec![first] }) }
}

pub fn reply(s: SheetId, th: &str, who: &Who, text: &str) -> Op {
    Op::CommentAdd { s, th: th.to_string(), cm: Box::new(comment(who, text)) }
}

pub fn edit(s: SheetId, th: &str, id: &str, who: &Who, text: &str) -> Op {
    Op::CommentEdit { s, th: th.to_string(), id: id.to_string(), text: text.to_string(), at: now_ms(), by: who.user.clone() }
}

/// Deletes a comment; the first comment of a thread takes the whole thread with it.
pub fn delete(s: SheetId, th: &str, id: &str, who: &Who) -> Op {
    Op::CommentDel { s, th: th.to_string(), id: id.to_string(), by: who.user.clone(), adm: who.manager }
}

pub fn resolve(s: SheetId, th: &str, done: bool) -> Op {
    Op::ThreadDone { s, th: th.to_string(), done }
}

/// Whether `who` may edit `cm` (only the author), or delete it (the author, or a manager).
pub fn can_edit(who: &Who, cm: &Comment) -> bool {
    !who.user.is_empty() && cm.by == who.user
}

pub fn can_delete(who: &Who, cm: &Comment) -> bool {
    who.manager || can_edit(who, cm)
}

/// Sets who made the edits and deletes among `ops` (diffs don't know). Additions carry their
/// author already.
pub fn stamp(ops: &mut [Op], who: &Who) {
    for op in ops {
        match op {
            Op::ThreadDel { by, adm, .. } | Op::CommentDel { by, adm, .. } => {
                *by = who.user.clone();
                *adm = who.manager;
            }
            Op::CommentEdit { by, .. } => *by = who.user.clone(),
            _ => {}
        }
    }
}

/// Undoing a delete would put back comments of other people under their names, which the
/// server doesn't allow (a comment is only ever added by its author): leave those out.
pub fn drop_foreign_restores(ops: &mut Vec<Op>, user: &str) {
    ops.retain(|op| match op {
        Op::ThreadAdd { th, .. } => th.comments.iter().all(|c| c.by == user),
        Op::CommentAdd { cm, .. } => cm.by == user,
        _ => true,
    });
}

/// The thread with this id, and the sheet's index.
pub fn find<'a>(sh: &'a Sheet, id: &str) -> Option<&'a Thread> {
    sh.threads.iter().find(|t| t.id == id)
}

/// `rest` starts with `name` followed by the end of the text or something that isn't part of a word.
fn ends_word(rest: &str, name: &str) -> bool {
    rest.starts_with(name) && !rest[name.len()..].chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// Names mentioned in `text` as `@name` (a user name or a display name), longest member name
/// first so "@Ann Lee" is not read as "@Ann". `members`: (user name, display name).
pub fn mentions<'a>(text: &str, members: &'a [(String, String)]) -> Vec<&'a str> {
    let lower = text.to_lowercase();
    let mut found = Vec::new();
    for (i, _) in lower.match_indices('@') {
        let rest = &lower[i + 1..];
        let mut best: Option<(&str, usize)> = None;
        for (user, name) in members {
            for cand in [name.as_str(), user.as_str()] {
                let cl = cand.to_lowercase();
                if !cl.is_empty() && ends_word(rest, &cl) && best.is_none_or(|b| cl.len() > b.1) {
                    best = Some((user.as_str(), cl.len()));
                }
            }
        }
        if let Some((u, _)) = best {
            if !found.contains(&u) {
                found.push(u);
            }
        }
    }
    found
}

/// The `@name` spans of `text` as byte ranges (for highlighting), using the same matching as
/// `mentions`.
pub fn mention_spans(text: &str, members: &[(String, String)]) -> Vec<(usize, usize)> {
    let lower = text.to_lowercase();
    // (Lower-casing can change byte lengths for a few characters; then nothing is highlighted.)
    if lower.len() != text.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut from = 0;
    for (i, _) in lower.match_indices('@') {
        if i < from {
            continue;
        }
        let rest = &lower[i + 1..];
        let best = members.iter().flat_map(|(u, n)| [u, n]).map(|c| c.to_lowercase()).filter(|c| !c.is_empty() && ends_word(rest, c)).map(|c| c.len()).max();
        if let Some(len) = best {
            out.push((i, i + 1 + len));
            from = i + 1 + len;
        }
    }
    out
}
