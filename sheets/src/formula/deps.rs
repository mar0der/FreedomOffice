//! Dependency tracking for incremental recalculation.
//!
//! Every formula's static precedents (cells and ranges, with defined names expanded) are kept
//! in a per-sheet interval index: per column, the row intervals are sorted by start row with a
//! max-end segment tree, so "which formulas read this cell/range" is answered in
//! O(log n + hits). Ranges wider than `WIDE` columns live in a short list that is scanned.
//! Edits append to a pending list and invalidate old entries by version; the index is rebuilt
//! when enough garbage accumulates.

use std::collections::HashMap;

use super::eval::{FxMap, FxSet, Key};
use super::parser::{Expr, RefKind};
use crate::model::{Range, Workbook};

const WIDE: u32 = 32;
const VOLATILE: &[&str] = &["NOW", "TODAY", "RAND", "RANDBETWEEN", "RANDARRAY", "OFFSET", "INDIRECT"];

#[derive(Clone, Copy)]
struct FInfo {
    ver: u32,
    nprecs: u32,
}

#[derive(Clone, Copy)]
struct Entry {
    sheet: usize,
    rg: Range,
    key: Key,
    ver: u32,
}

/// Row intervals of one column, sorted by start, with a segment tree of max(end)+1.
#[derive(Default)]
struct ColTree {
    r1: Vec<u32>,
    ent: Vec<(u32, Key, u32)>,
    tree: Vec<u32>,
    size: usize,
}

impl ColTree {
    fn build(mut items: Vec<(u32, u32, Key, u32)>) -> Self {
        items.sort_unstable_by_key(|x| x.0);
        let n = items.len();
        let size = n.next_power_of_two().max(1);
        let mut tree = vec![0u32; 2 * size];
        for (i, it) in items.iter().enumerate() {
            tree[size + i] = it.1 + 1;
        }
        for i in (1..size).rev() {
            tree[i] = tree[2 * i].max(tree[2 * i + 1]);
        }
        ColTree { r1: items.iter().map(|x| x.0).collect(), ent: items.iter().map(|x| (x.1, x.2, x.3)).collect(), tree, size }
    }

    /// Calls `f` for every interval intersecting rows [qa, qb] and takes it out of the tree
    /// (logged in `taken`) so a propagation never reports the same entry twice.
    fn take(&mut self, qa: u32, qb: u32, taken: &mut Vec<usize>, f: &mut dyn FnMut(Key, u32)) {
        let p = self.r1.partition_point(|&x| x <= qb);
        if p > 0 {
            self.descend(1, 0, self.size, p, qa, taken, f);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn descend(&mut self, node: usize, lo: usize, hi: usize, p: usize, qa: u32, taken: &mut Vec<usize>, f: &mut dyn FnMut(Key, u32)) {
        if lo >= p || self.tree[node] <= qa {
            return;
        }
        if hi - lo == 1 {
            let (_, k, v) = self.ent[lo];
            f(k, v);
            self.set_leaf(lo, 0);
            taken.push(lo);
            return;
        }
        let mid = (lo + hi) / 2;
        self.descend(2 * node, lo, mid, p, qa, taken, f);
        self.descend(2 * node + 1, mid, hi, p, qa, taken, f);
    }

    fn set_leaf(&mut self, i: usize, v: u32) {
        let mut n = self.size + i;
        self.tree[n] = v;
        while n > 1 {
            n /= 2;
            self.tree[n] = self.tree[2 * n].max(self.tree[2 * n + 1]);
        }
    }

    fn restore(&mut self, taken: &[usize]) {
        for &i in taken {
            self.set_leaf(i, self.ent[i].0 + 1);
        }
    }
}

#[derive(Default)]
struct SheetIndex {
    cols: FxMap<u32, ColTree>,
    wide: Vec<Entry>,
    /// Entries taken out during the current propagation: column -> leaves.
    taken: FxMap<u32, Vec<usize>>,
}

/// Dependency graph of a workbook. Cloning yields an empty (invalid) state that is rebuilt on demand.
#[derive(Default)]
pub struct CalcState {
    valid: bool,
    nsheets: usize,
    formulas: FxMap<Key, FInfo>,
    volatile: FxSet<Key>,
    index: Vec<SheetIndex>,
    pending: Vec<Entry>,
    stale: usize,
    next_ver: u32,
    names: HashMap<String, Option<Expr>>,
}

impl Clone for CalcState {
    fn clone(&self) -> Self {
        CalcState::default()
    }
}

impl std::fmt::Debug for CalcState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CalcState {{ valid: {}, formulas: {} }}", self.valid, self.formulas.len())
    }
}

impl CalcState {
    pub fn invalidate(&mut self) {
        *self = CalcState::default();
    }

    pub fn is_valid_for(&self, wb: &Workbook) -> bool {
        self.valid && self.nsheets == wb.sheets.len()
    }

    pub fn volatile(&self) -> impl Iterator<Item = &Key> {
        self.volatile.iter()
    }

    /// Rebuilds the whole graph from the workbook.
    pub fn build(&mut self, wb: &Workbook) {
        self.invalidate();
        self.nsheets = wb.sheets.len();
        let mut entries = Vec::new();
        for (si, sh) in wb.sheets.iter().enumerate() {
            for (r, row) in &sh.rows {
                for (c, cell) in row {
                    if let Some(ast) = cell.formula.as_ref().and_then(|f| f.ast.as_ref()) {
                        self.add(wb, (si, *r, *c), ast, &mut entries);
                    } else if cell.formula.is_some() {
                        self.add(wb, (si, *r, *c), &Expr::Missing, &mut entries);
                    }
                }
            }
        }
        self.index_entries(entries);
        self.valid = true;
    }

    fn index_entries(&mut self, mut entries: Vec<Entry>) {
        entries.extend(self.pending.drain(..));
        let mut per_sheet: Vec<FxMap<u32, Vec<(u32, u32, Key, u32)>>> = (0..self.nsheets).map(|_| FxMap::default()).collect();
        let mut wide: Vec<Vec<Entry>> = (0..self.nsheets).map(|_| Vec::new()).collect();
        for e in entries {
            if self.formulas.get(&e.key).map(|f| f.ver) != Some(e.ver) {
                continue;
            }
            let s = e.sheet;
            if s >= self.nsheets {
                continue;
            }
            if e.rg.cols() > WIDE {
                wide[s].push(e);
            } else {
                for c in e.rg.c1..=e.rg.c2 {
                    per_sheet[s].entry(c).or_default().push((e.rg.r1, e.rg.r2, e.key, e.ver));
                }
            }
        }
        self.index = per_sheet
            .into_iter()
            .zip(wide)
            .map(|(cols, wide)| SheetIndex { cols: cols.into_iter().map(|(c, v)| (c, ColTree::build(v))).collect(), wide, taken: FxMap::default() })
            .collect();
        self.stale = 0;
    }

    fn add(&mut self, wb: &Workbook, key: Key, ast: &Expr, out: &mut Vec<Entry>) {
        let mut precs = Vec::new();
        let mut vol = false;
        collect(ast, wb, key.0, &mut precs, &mut vol, 0, &mut self.names);
        self.next_ver = self.next_ver.wrapping_add(1);
        let ver = self.next_ver;
        self.formulas.insert(key, FInfo { ver, nprecs: precs.len() as u32 });
        if vol {
            self.volatile.insert(key);
        }
        for (s, rg) in precs {
            out.push(Entry { sheet: s, rg, key, ver });
        }
    }

    fn remove(&mut self, key: Key) {
        if let Some(f) = self.formulas.remove(&key) {
            self.stale += f.nprecs as usize;
        }
        self.volatile.remove(&key);
    }

    /// Re-reads the formulas inside the edited ranges.
    pub fn update(&mut self, wb: &Workbook, edited: &[(usize, Range)]) {
        for &(s, rg) in edited {
            let area = rg.rows() as u64 * rg.cols() as u64;
            if area <= 4096 {
                for r in rg.r1..=rg.r2 {
                    for c in rg.c1..=rg.c2 {
                        self.remove((s, r, c));
                    }
                }
            } else {
                let keys: Vec<Key> = self.formulas.keys().filter(|k| k.0 == s && rg.contains(k.1, k.2)).copied().collect();
                for k in keys {
                    self.remove(k);
                }
            }
            let Some(sh) = wb.sheets.get(s) else { continue };
            let mut entries = Vec::new();
            for (r, c, cell) in sh.iter_range(rg) {
                if let Some(f) = &cell.formula {
                    self.add(wb, (s, r, c), f.ast.as_ref().unwrap_or(&Expr::Missing), &mut entries);
                }
            }
            self.pending.extend(entries);
        }
        if self.pending.len() > 256 || self.stale > 4096.max(self.formulas.len() / 4) {
            self.index_entries(Vec::new());
        }
    }

    /// Calls `f` for formulas whose precedents intersect `rg` on sheet `s`. Within one
    /// propagation (until `end_propagation`) each indexed precedent is reported only once.
    pub fn dependents(&mut self, s: usize, rg: Range, f: &mut dyn FnMut(Key)) {
        let formulas = &self.formulas;
        let mut hit = |k: Key, v: u32| {
            if formulas.get(&k).map(|x| x.ver) == Some(v) {
                f(k);
            }
        };
        if let Some(ix) = self.index.get_mut(s) {
            let SheetIndex { cols, wide, taken } = ix;
            if (rg.cols() as usize) <= cols.len() {
                for c in rg.c1..=rg.c2 {
                    if let Some(t) = cols.get_mut(&c) {
                        t.take(rg.r1, rg.r2, taken.entry(c).or_default(), &mut hit);
                    }
                }
            } else {
                for (c, t) in cols.iter_mut() {
                    if rg.c1 <= *c && *c <= rg.c2 {
                        t.take(rg.r1, rg.r2, taken.entry(*c).or_default(), &mut hit);
                    }
                }
            }
            for e in wide.iter() {
                if e.rg.intersects(&rg) {
                    hit(e.key, e.ver);
                }
            }
        }
        for e in &self.pending {
            if e.sheet == s && e.rg.intersects(&rg) {
                hit(e.key, e.ver);
            }
        }
    }

    /// Puts back the index entries taken during a propagation.
    pub fn end_propagation(&mut self) {
        for ix in &mut self.index {
            for (c, leaves) in ix.taken.drain() {
                if let Some(t) = ix.cols.get_mut(&c) {
                    t.restore(&leaves);
                }
            }
        }
    }
}

/// Collects static precedents of an expression evaluated on sheet `cur`.
fn collect(
    e: &Expr,
    wb: &Workbook,
    cur: usize,
    out: &mut Vec<(usize, Range)>,
    vol: &mut bool,
    depth: u32,
    names: &mut HashMap<String, Option<Expr>>,
) {
    match e {
        Expr::Ref(r) => {
            let s = match &r.sheet {
                Some(n) => match wb.sheet_index(n) {
                    Some(s) => s,
                    None => return,
                },
                None => cur,
            };
            let rg = if r.kind == RefKind::Spill { Range::cell(r.a.row, r.a.col) } else { Range::new(r.a.row, r.a.col, r.b.row, r.b.col) };
            out.push((s, rg));
        }
        Expr::Name(n) => {
            if depth > 16 {
                return;
            }
            let key = n.to_ascii_uppercase();
            let parsed = names
                .entry(key)
                .or_insert_with(|| {
                    wb.names.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).and_then(|(_, f)| super::parse(f.trim_start_matches('=')).ok())
                })
                .clone();
            if let Some(x) = parsed {
                collect(&x, wb, cur, out, vol, depth + 1, names);
            }
        }
        Expr::Neg(a) | Expr::Plus(a) | Expr::Percent(a) | Expr::Paren(a) => collect(a, wb, cur, out, vol, depth, names),
        Expr::Bin(_, a, b) => {
            collect(a, wb, cur, out, vol, depth, names);
            collect(b, wb, cur, out, vol, depth, names);
        }
        Expr::Func(name, args) => {
            if VOLATILE.contains(&name.as_str()) {
                *vol = true;
            }
            for a in args {
                collect(a, wb, cur, out, vol, depth, names);
            }
        }
        Expr::Array(rows) => {
            for a in rows.iter().flatten() {
                collect(a, wb, cur, out, vol, depth, names);
            }
        }
        _ => {}
    }
}
