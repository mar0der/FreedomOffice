//! Threaded comments (protocol 3): a thread is anchored to a cell, has a first comment and
//! replies, and can be resolved. Plain notes (`Op::Note`) are separate and stay as they are.
//!
//! The rules live here, in one place, so every participant (the app, `fsheets`, the tests)
//! applies the operations identically:
//!
//! * Threads, and replies inside a thread, are kept sorted by (time, id), not by arrival, so
//!   concurrent additions end in the same order everywhere. The first comment of a thread
//!   stays first.
//! * Operations address threads and comments by id, so they need no transformation over
//!   row/column changes; only the anchor of `ThreadAdd` moves (`shift_threads`, and
//!   `transform` for an addition still in flight). An operation on a missing thread or
//!   comment does nothing, which is how a deleted row, sheet, thread or comment takes
//!   concurrent replies, edits and resolves with it.
//! * Authorship is enforced when applying: an edit needs `by` = the comment's author, a delete
//!   needs the same or `adm`. The server sets `by` to the authenticated user and `adm` to
//!   whether that user manages the document, and refuses a batch whose values differ.
//! * Edits and resolves are last-writer-wins per comment / thread (`transform`).

use serde::{Deserialize, Serialize};

use crate::{map_index, Op, MAX_COLS, MAX_ROWS};

/// Longest comment text (Excel's limit for a comment).
pub const MAX_COMMENT_TEXT: usize = 32_767;
/// Most comments one thread keeps; further replies are ignored.
pub const MAX_THREAD_COMMENTS: usize = 1000;
const MAX_ID: usize = 80;
const MAX_NAME: usize = 256;

/// One comment of a thread.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Comment {
    /// Unique (a GUID like `{8F3A...}` so it is the same in the xlsx file).
    pub id: String,
    /// The author's user name; "" for someone with a link (who can't edit or delete).
    pub by: String,
    /// The author's display name when written.
    #[serde(default)]
    pub name: String,
    /// When written: milliseconds since 1970 (UTC).
    pub at: i64,
    pub text: String,
    /// When the text was last edited (0: never).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub edited: i64,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// A comment thread on a cell. `comments[0]` is the first comment.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    /// Anchor (row, column); `ThreadAdd` sets them.
    #[serde(default)]
    pub r: u32,
    #[serde(default)]
    pub c: u32,
    /// Resolved.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub done: bool,
    pub comments: Vec<Comment>,
}

impl Thread {
    /// The first comment's (time, id): the order of threads.
    fn key(&self) -> (i64, &str) {
        self.comments.first().map_or((0, ""), |c| (c.at, c.id.as_str()))
    }

    /// The thread with its replies in canonical order, duplicates and excess removed.
    fn normalized(mut self) -> Option<Thread> {
        if self.comments.is_empty() {
            return None;
        }
        let first = self.comments.remove(0);
        self.comments.sort_by(|a, b| (a.at, &a.id).cmp(&(b.at, &b.id)));
        self.comments.insert(0, first);
        let mut seen = std::collections::HashSet::new();
        self.comments.retain(|c| seen.insert(c.id.clone()));
        self.comments.truncate(MAX_THREAD_COMMENTS);
        Some(self)
    }
}

/// Applies a comment operation to the threads of a sheet (other operations are ignored).
/// Returns whether anything changed.
pub fn apply_thread_op(list: &mut Vec<Thread>, op: &Op) -> bool {
    match op {
        Op::ThreadAdd { r, c, th, .. } => {
            if list.iter().any(|t| t.id == th.id) {
                return false;
            }
            let Some(mut t) = (**th).clone().normalized() else { return false };
            (t.r, t.c) = (*r, *c);
            let at = list.partition_point(|x| x.key() < t.key());
            list.insert(at, t);
            true
        }
        Op::ThreadDel { th, by, adm, .. } => {
            let Some(i) = list.iter().position(|t| t.id == *th) else { return false };
            if !allowed(list[i].comments.first(), by, *adm) {
                return false;
            }
            list.remove(i);
            true
        }
        Op::CommentAdd { th, cm, .. } => {
            let Some(t) = list.iter_mut().find(|t| t.id == *th) else { return false };
            if t.comments.len() >= MAX_THREAD_COMMENTS || t.comments.iter().any(|c| c.id == cm.id) {
                return false;
            }
            let at = 1 + t.comments[1..].partition_point(|x| (x.at, &x.id) < (cm.at, &cm.id));
            t.comments.insert(at, (**cm).clone());
            true
        }
        Op::CommentEdit { th, id, text, at, by, .. } => {
            let Some(cm) = list.iter_mut().find(|t| t.id == *th).and_then(|t| t.comments.iter_mut().find(|c| c.id == *id)) else { return false };
            if by.is_empty() || cm.by != *by || cm.text == *text {
                return false;
            }
            cm.text = text.clone();
            cm.edited = *at;
            true
        }
        Op::CommentDel { th, id, by, adm, .. } => {
            let Some(i) = list.iter().position(|t| t.id == *th) else { return false };
            let Some(j) = list[i].comments.iter().position(|c| c.id == *id) else { return false };
            if !allowed(list[i].comments.get(j), by, *adm) {
                return false;
            }
            if j == 0 {
                list.remove(i);
            } else {
                list[i].comments.remove(j);
            }
            true
        }
        Op::ThreadDone { th, done, .. } => match list.iter_mut().find(|t| t.id == *th) {
            Some(t) if t.done != *done => {
                t.done = *done;
                true
            }
            _ => false,
        },
        _ => false,
    }
}

fn allowed(cm: Option<&Comment>, by: &str, adm: bool) -> bool {
    cm.is_some_and(|c| adm || (!by.is_empty() && c.by == by))
}

/// Moves the anchors of `list` for rows (`rows`) or columns inserted (n > 0) or deleted
/// (n < 0) at `at`; threads on deleted rows/columns are removed.
pub fn shift_threads(list: &mut Vec<Thread>, rows: bool, at: u32, n: i32) {
    let max = if rows { MAX_ROWS } else { MAX_COLS };
    list.retain_mut(|t| {
        let p = if rows { &mut t.r } else { &mut t.c };
        match map_index(*p, at, n, max) {
            Some(q) => {
                *p = q;
                true
            }
            None => false,
        }
    });
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

pub(crate) fn validate_id(id: &str) -> Result<(), String> {
    if !id.is_empty() && id.len() <= MAX_ID { Ok(()) } else { Err("invalid comment id".into()) }
}

pub(crate) fn validate_short(s: &str) -> Result<(), String> {
    if s.len() <= MAX_NAME { Ok(()) } else { Err("invalid comment author".into()) }
}

pub(crate) fn validate_text(t: &str) -> Result<(), String> {
    if t.chars().count() <= MAX_COMMENT_TEXT { Ok(()) } else { Err("comment too long".into()) }
}

pub(crate) fn validate_comment(c: &Comment) -> Result<(), String> {
    validate_id(&c.id)?;
    validate_short(&c.by)?;
    validate_short(&c.name)?;
    validate_text(&c.text)
}

pub(crate) fn validate_thread(t: &Thread) -> Result<(), String> {
    validate_id(&t.id)?;
    if t.comments.is_empty() || t.comments.len() > MAX_THREAD_COMMENTS {
        return Err("invalid thread".into());
    }
    t.comments.iter().try_for_each(validate_comment)
}
