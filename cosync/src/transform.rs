//! Operational transformation of operation sequences.
//!
//! `over(x, y, x_wins)` rewrites `x` so it can be applied after `y` when both were made
//! against the same state. `x_wins` breaks ties (same cell written by both, rows inserted at
//! the same place): the op committed later wins, which is the incoming op on the server and
//! the local (pending) op on a client. The functions satisfy TP1:
//! `apply(apply(S, a), over(b, a, !w)) == apply(apply(S, b), over(a, b, w))`.

use std::collections::HashSet;

use serde_json::Value as Json;

use crate::{CellData, Op, Rect, SheetId, MAX_COLS, MAX_ROWS, MAX_SHEET_NAME};

/// Where index `i` lands after inserting (n > 0) or deleting (n < 0) at `at`; None when it
/// is deleted or pushed off the end (`max`).
pub fn map_index(i: u32, at: u32, n: i32, max: u32) -> Option<u32> {
    let (i, at, n) = (i as i64, at as i64, n as i64);
    if n < 0 && i >= at && i < at - n {
        return None;
    }
    let j = if i >= at { i + n } else { i };
    if j < 0 || j >= max as i64 { None } else { Some(j as u32) }
}

/// A range after inserting/deleting rows (`rows`) or columns, like the app does for merges,
/// tables and filters: inserting inside grows it, deleting shrinks it, None when it is gone.
pub fn fix_range(rg: Rect, rows: bool, at: u32, n: i32) -> Option<Rect> {
    let max = if rows { MAX_ROWS } else { MAX_COLS } as i64;
    let (a, b) = if rows { (rg.r1, rg.r2) } else { (rg.c1, rg.c2) };
    let n = n as i64;
    let (na, nb) = if n > 0 {
        let na = if a >= at { a as i64 + n } else { a as i64 };
        if na >= max {
            return None;
        }
        (na as u32, if b >= at { (b as i64 + n).min(max - 1) as u32 } else { b })
    } else {
        let end = at as i64 - n;
        let na = if (a as i64) >= end { (a as i64 + n) as u32 } else if a >= at { at } else { a };
        let nb = if (b as i64) >= end {
            (b as i64 + n) as u32
        } else if b >= at {
            at.checked_sub(1)?
        } else {
            b
        };
        if nb < na {
            return None;
        }
        (na, nb)
    };
    Some(if rows { Rect { r1: na, r2: nb, ..rg } } else { Rect { c1: na, c2: nb, ..rg } })
}

/// One insert/delete transformed over another on the same axis of the same sheet.
fn axis(at: u32, n: i32, yat: u32, yn: i32, xw: bool, max: u32) -> Option<(u32, i32)> {
    let (at, n, yat, yn) = (at as i64, n as i64, yat as i64, yn as i64);
    let (nat, nn) = if n > 0 && yn > 0 {
        if at < yat || (at == yat && !xw) { (at, n) } else { (at + yn, n) }
    } else if n > 0 {
        // Insert over a delete: an insert inside the deleted block goes with it.
        let k = -yn;
        if at <= yat {
            (at, n)
        } else if at >= yat + k {
            (at - k, n)
        } else {
            return None;
        }
    } else if yn > 0 {
        // Delete over an insert: rows inserted inside the deleted block are deleted too.
        let k = -n;
        if yat <= at {
            (at + yn, n)
        } else if yat >= at + k {
            (at, n)
        } else {
            (at, -(k + yn))
        }
    } else {
        let (k, yk) = (-n, -yn);
        let overlap = ((at + k).min(yat + yk) - at.max(yat)).max(0);
        let nat = if at <= yat {
            at
        } else if at >= yat + yk {
            at - yk
        } else {
            yat
        };
        if k == overlap {
            return None;
        }
        (nat, -(k - overlap))
    };
    if nat >= max as i64 || nn == 0 {
        return None;
    }
    Some((nat as u32, nn.clamp(-(max as i64), max as i64) as i32))
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// The name a sheet gets when someone else claimed its name at the same time.
fn alt_name(name: &str) -> String {
    let suffix = " (2)";
    let keep = MAX_SHEET_NAME - suffix.len();
    format!("{}{suffix}", name.chars().take(keep).collect::<String>())
}

/// What a content operation writes, for last-writer-wins.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Target {
    /// A cell's value or formula, with its rich text.
    CellContent(SheetId, u32, u32),
    /// A cell's style.
    CellFormat(SheetId, u32, u32),
    Note(SheetId, u32, u32),
    Link(SheetId, u32, u32),
    RowH(SheetId, u32),
    ColW(SheetId, u32),
    RowHidden(SheetId, u32),
    ColHidden(SheetId, u32),
    RowLevel(SheetId, u32),
    ColLevel(SheetId, u32),
    Merge(SheetId, Rect),
    Prop(SheetId, String),
    Feature(SheetId, String),
    Book(String),
    Place(SheetId),
}

/// The targets an operation writes: a whole `Cell` writes two, everything else one or none.
fn targets(op: &Op) -> impl Iterator<Item = Target> {
    let two = match op {
        Op::Cell { s, r, c, .. } => (Some(Target::CellContent(*s, *r, *c)), Some(Target::CellFormat(*s, *r, *c))),
        _ => (target(op), None),
    };
    [two.0, two.1].into_iter().flatten()
}

fn target(op: &Op) -> Option<Target> {
    use Op::*;
    Some(match op {
        Content { s, r, c, .. } => Target::CellContent(*s, *r, *c),
        Format { s, r, c, .. } => Target::CellFormat(*s, *r, *c),
        // (a whole cell has two targets: see `targets`)
        Cell { s, r, c, .. } => Target::CellContent(*s, *r, *c),
        Note { s, r, c, .. } => Target::Note(*s, *r, *c),
        Link { s, r, c, .. } => Target::Link(*s, *r, *c),
        RowH { s, r, .. } => Target::RowH(*s, *r),
        ColW { s, c, .. } => Target::ColW(*s, *c),
        RowHidden { s, r, .. } => Target::RowHidden(*s, *r),
        ColHidden { s, c, .. } => Target::ColHidden(*s, *c),
        RowLevel { s, r, .. } => Target::RowLevel(*s, *r),
        ColLevel { s, c, .. } => Target::ColLevel(*s, *c),
        Merge { s, rg, .. } => Target::Merge(*s, *rg),
        Prop { s, key, .. } => Target::Prop(*s, key.clone()),
        Feature { s, key, .. } => Target::Feature(*s, key.clone()),
        Book { key, .. } => Target::Book(key.clone()),
        Place { s, .. } => Target::Place(*s),
        Rows { .. } | Cols { .. } | AddSheet { .. } | DelSheet { .. } | Rename { .. } => return None,
    })
}

/// `x` rewritten to apply after `y` (both made against the same state).
pub fn over(x: &Op, y: &Op, xw: bool) -> Vec<Op> {
    use Op::*;
    // A deleted sheet takes every change on it with it.
    if let DelSheet { s, .. } = y {
        match x {
            DelSheet { s: xs, .. } if xs == s => return vec![],
            // (The same id is added and deleted only for the rescue sheet: the delete was
            // made after it was there.)
            AddSheet { s: xs, .. } if xs != s => return vec![x.clone()],
            _ if x.sheet() == Some(*s) => return vec![],
            _ => {}
        }
    }
    match x {
        Rows { s, at, n } => match y {
            Rows { s: ys, at: yat, n: yn } if ys == s => axis(*at, *n, *yat, *yn, xw, MAX_ROWS).map(|(at, n)| Rows { s: *s, at, n }).into_iter().collect(),
            _ => vec![x.clone()],
        },
        Cols { s, at, n } => match y {
            Cols { s: ys, at: yat, n: yn } if ys == s => axis(*at, *n, *yat, *yn, xw, MAX_COLS).map(|(at, n)| Cols { s: *s, at, n }).into_iter().collect(),
            _ => vec![x.clone()],
        },
        AddSheet { s, name, .. } => match y {
            AddSheet { s: ys, name: yn, .. } if ys != s && same_name(name, yn) => collide(x, *ys, yn, xw),
            Rename { s: ys, to, .. } if ys != s && same_name(name, to) => collide(x, *ys, to, xw),
            _ => vec![x.clone()],
        },
        DelSheet { .. } => vec![x.clone()],
        Rename { s, from: _, to } => match y {
            Rename { s: ys, to: yto, .. } if ys == s => {
                if xw {
                    vec![Rename { s: *s, from: yto.clone(), to: to.clone() }]
                } else {
                    vec![]
                }
            }
            Rename { s: ys, to: yto, .. } if same_name(to, yto) => collide(x, *ys, yto, xw),
            AddSheet { s: ys, name, .. } if ys != s && same_name(to, name) => collide(x, *ys, name, xw),
            _ => vec![x.clone()],
        },
        _ => content_over(x, y, xw),
    }
}

/// `x` (adding or renaming a sheet) and `y` both claimed the name `claimed` for different
/// sheets. The winner keeps it; the loser gets "name (2)".
fn collide(x: &Op, other: SheetId, claimed: &str, xw: bool) -> Vec<Op> {
    if xw {
        vec![Op::Rename { s: other, from: claimed.to_string(), to: alt_name(claimed) }, x.clone()]
    } else {
        let mut x = x.clone();
        match &mut x {
            Op::AddSheet { name, .. } => *name = alt_name(name),
            Op::Rename { to, .. } => *to = alt_name(to),
            _ => {}
        }
        vec![x]
    }
}

fn content_over(x: &Op, y: &Op, xw: bool) -> Vec<Op> {
    use Op::*;
    match y {
        Rows { s, at, n } => shift_content(x, *s, true, *at, *n, y),
        Cols { s, at, n } => shift_content(x, *s, false, *at, *n, y),
        Rename { .. } | DelSheet { .. } => {
            let mut x = x.clone();
            push_adj(&mut x, y);
            vec![x]
        }
        AddSheet { .. } => vec![x.clone()],
        _ => {
            if xw {
                return vec![x.clone()];
            }
            let taken: HashSet<Target> = targets(y).collect();
            reduce(x, &taken).into_iter().collect()
        }
    }
}

/// Strips the style from cell data; None when nothing else is left (a cleared content).
fn content_part(d: Option<&CellData>) -> Option<Box<CellData>> {
    d.filter(|d| !d.v.is_empty() || d.f.is_some() || d.rich.is_some()).map(|d| Box::new(CellData { st: None, ..d.clone() }))
}

/// `op` without the writes to the `taken` targets (which a later-committed operation wins):
/// a whole `Cell` keeps the part that is not taken, as a `Content` or `Format`.
fn reduce(op: &Op, taken: &HashSet<Target>) -> Option<Op> {
    if let Op::Cell { s, r, c, v, adj } = op {
        let content = taken.contains(&Target::CellContent(*s, *r, *c));
        let format = taken.contains(&Target::CellFormat(*s, *r, *c));
        return match (content, format) {
            (false, false) => Some(op.clone()),
            (true, true) => None,
            (true, false) => Some(Op::Format { s: *s, r: *r, c: *c, st: v.as_ref().and_then(|d| d.st.clone()) }),
            (false, true) => Some(Op::Content { s: *s, r: *r, c: *c, v: content_part(v.as_deref()), adj: adj.clone() }),
        };
    }
    match target(op) {
        Some(t) if taken.contains(&t) => None,
        _ => Some(op.clone()),
    }
}

/// Which part of a cell a write is to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CellWrite {
    Content,
    Format,
}

/// The single-part writes a cell operation makes (a whole `Cell` makes both), each as a
/// `Content` or `Format` operation; empty for everything else.
pub fn cell_write(op: &Op) -> Vec<(CellWrite, Op)> {
    match op {
        Op::Cell { s, r, c, v, adj } => vec![
            (CellWrite::Content, Op::Content { s: *s, r: *r, c: *c, v: content_part(v.as_deref()), adj: adj.clone() }),
            (CellWrite::Format, Op::Format { s: *s, r: *r, c: *c, st: v.as_ref().and_then(|d| d.st.clone()) }),
        ],
        Op::Content { v, .. } => {
            let mut o = op.clone();
            if let Op::Content { v: nv, .. } = &mut o {
                *nv = content_part(v.as_deref());
            }
            vec![(CellWrite::Content, o)]
        }
        Op::Format { .. } => vec![(CellWrite::Format, op.clone())],
        _ => Vec::new(),
    }
}

/// Where a single-part cell write lands and what it writes (for comparing two writes to the
/// same place: equal writes don't conflict).
pub(crate) fn write_key(op: &Op) -> Option<(CellWrite, SheetId, u32, u32, Option<Json>)> {
    match op {
        Op::Content { s, r, c, v, .. } => Some((CellWrite::Content, *s, *r, *c, v.as_ref().and_then(|d| serde_json::to_value(d).ok()))),
        Op::Format { s, r, c, st } => Some((CellWrite::Format, *s, *r, *c, st.clone())),
        _ => None,
    }
}

/// Records a structural op the content of `x` must be adjusted by.
fn push_adj(x: &mut Op, y: &Op) {
    match x {
        Op::Cell { v, adj, .. } | Op::Content { v, adj, .. } if v.as_ref().is_some_and(|d| d.f.is_some()) => adj.push(y.clone()),
        Op::Feature { adj, .. } | Op::Book { adj, .. } => adj.push(y.clone()),
        _ => {}
    }
}

fn shift_content(x: &Op, s: SheetId, rows: bool, at: u32, n: i32, y: &Op) -> Vec<Op> {
    use Op::*;
    let mut x = x.clone();
    let same = x.sheet() == Some(s);
    let max = if rows { MAX_ROWS } else { MAX_COLS };
    let mv = |p: &mut u32| -> bool {
        match map_index(*p, at, n, max) {
            Some(q) => {
                *p = q;
                true
            }
            None => false,
        }
    };
    let keep = match &mut x {
        Cell { r, c, .. } | Content { r, c, .. } | Format { r, c, .. } | Note { r, c, .. } | Link { r, c, .. } => !same || mv(if rows { r } else { c }),
        RowH { r, .. } | RowHidden { r, .. } | RowLevel { r, .. } => !(same && rows) || mv(r),
        ColW { c, .. } | ColHidden { c, .. } | ColLevel { c, .. } => !(same && !rows) || mv(c),
        Merge { rg, .. } => {
            if same {
                match fix_range(*rg, rows, at, n) {
                    Some(f) if !f.is_single() => {
                        *rg = f;
                        true
                    }
                    _ => false,
                }
            } else {
                true
            }
        }
        _ => true,
    };
    if !keep {
        return vec![];
    }
    push_adj(&mut x, y);
    vec![x]
}

/// True when some content operation of `a` writes a target that `b` also writes (undo uses
/// this to refuse undoing a change someone else has changed since, like Excel).
pub fn touches(a: &[Op], b: &[Op]) -> bool {
    let tb: HashSet<Target> = b.iter().flat_map(targets).collect();
    !tb.is_empty() && a.iter().flat_map(targets).any(|t| tb.contains(&t))
}

/// Brings an undo stack (newest step first: each step's ops reverse it, in the state left by
/// undoing the newer steps) over `ops`, someone else's change in the current state. A step is
/// marked blocked when the change writes what it would restore. The change is carried down
/// twice: as it applies below each step (positions), and with its writes winning over the
/// step's, so older steps restoring the same targets are blocked too.
pub fn carry_down<'a>(steps: impl IntoIterator<Item = (&'a mut Vec<Op>, &'a mut bool)>, ops: &[Op]) {
    let (mut ops, mut writes) = (ops.to_vec(), ops.to_vec());
    for (inverse, blocked) in steps {
        if touches(inverse, &writes) {
            *blocked = true;
        }
        writes = transform(&writes, inverse, true).0;
        let (inv, below) = transform(inverse, &ops, true);
        *inverse = inv;
        ops = below;
    }
}

// ---------------------------------------------------------------------------
// Sequences
// ---------------------------------------------------------------------------

/// A run of a sequence: one structural op, or content ops (which only conflict on equal
/// targets, so a whole run is transformed at once with a hash set).
enum Seg {
    Heavy(Op),
    Light(Vec<Op>),
}

fn segments(ops: Vec<Op>) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::new();
    for op in ops {
        if op.is_structural() {
            out.push(Seg::Heavy(op));
        } else if let Some(Seg::Light(v)) = out.last_mut() {
            v.push(op);
        } else {
            out.push(Seg::Light(vec![op]));
        }
    }
    out
}

fn flatten(segs: Vec<Seg>) -> Vec<Op> {
    let mut out = Vec::new();
    for s in segs {
        match s {
            Seg::Heavy(op) => out.push(op),
            Seg::Light(v) => out.extend(v),
        }
    }
    out
}

fn heavies(ops: Vec<Op>) -> Vec<Seg> {
    ops.into_iter().map(Seg::Heavy).collect()
}

fn light(ops: Vec<Op>) -> Vec<Seg> {
    if ops.is_empty() { Vec::new() } else { vec![Seg::Light(ops)] }
}

/// Both sides of one segment pair.
fn seg_pair(x: &Seg, y: &Seg, xw: bool) -> (Vec<Seg>, Vec<Seg>) {
    match (x, y) {
        (Seg::Heavy(a), Seg::Heavy(b)) => (heavies(over(a, b, xw)), heavies(over(b, a, !xw))),
        // Content never moves a structural op.
        (Seg::Heavy(a), Seg::Light(bs)) => (vec![Seg::Heavy(a.clone())], light(bs.iter().flat_map(|b| over(b, a, !xw)).collect())),
        (Seg::Light(xs), Seg::Heavy(b)) => (light(xs.iter().flat_map(|x| over(x, b, xw)).collect()), vec![Seg::Heavy(b.clone())]),
        (Seg::Light(xs), Seg::Light(ys)) => {
            let loser = |v: &[Op], winners: &[Op]| -> Vec<Op> {
                let taken: HashSet<_> = winners.iter().flat_map(targets).collect();
                v.iter().filter_map(|o| reduce(o, &taken)).collect()
            };
            if xw { (light(xs.clone()), light(loser(ys, xs))) } else { (light(loser(xs, ys)), light(ys.clone())) }
        }
    }
}

fn tr_segs(a: Vec<Seg>, b: Vec<Seg>, aw: bool) -> (Vec<Seg>, Vec<Seg>) {
    let mut b = b;
    let mut out = Vec::new();
    for x in a {
        let (xs, nb) = tr_one(x, b, aw);
        out.extend(xs);
        b = nb;
    }
    (out, b)
}

/// One segment over a sequence (and the sequence over it).
fn tr_one(x: Seg, b: Vec<Seg>, aw: bool) -> (Vec<Seg>, Vec<Seg>) {
    let mut xs = vec![x];
    let mut bout = Vec::new();
    for y in b {
        let mut ys = vec![y];
        let mut nxs = Vec::new();
        for xi in xs {
            if ys.len() == 1 {
                let (xo, yo) = seg_pair(&xi, &ys[0], aw);
                nxs.extend(xo);
                ys = yo;
            } else {
                // Rare: an op expanded (sheet name collisions); recurse on the short list.
                let (xo, yo) = tr_segs(vec![xi], ys, aw);
                nxs.extend(xo);
                ys = yo;
            }
        }
        xs = nxs;
        bout.extend(ys);
    }
    (xs, bout)
}

/// Transforms two sequences made against the same state: returns (a', b') with
/// `S·a·b' == S·b·a'`. `a_wins` breaks ties in favour of `a`.
pub fn transform(a: &[Op], b: &[Op], a_wins: bool) -> (Vec<Op>, Vec<Op>) {
    if a.is_empty() || b.is_empty() {
        return (a.to_vec(), b.to_vec());
    }
    let (x, y) = tr_segs(segments(a.to_vec()), segments(b.to_vec()), a_wins);
    (flatten(x), flatten(y))
}
