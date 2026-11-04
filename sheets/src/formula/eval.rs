//! Formula evaluator and workbook recalculation.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;

use super::parser::{BinOp, Expr, RefExpr, RefKind};
use crate::model::{Cell, ErrorKind, Range, Spill, Value, Workbook, MAX_COLS, MAX_ROWS};

/// Result of evaluating an expression: a scalar, a reference, or an in-memory array.
#[derive(Clone, Debug)]
pub enum EV {
    V(Value),
    R(usize, Range),
    A(Vec<Vec<Value>>),
}

pub type Res = Result<EV, ErrorKind>;
pub type VRes = Result<Value, ErrorKind>;

/// Longest text a formula may produce (characters), as in Excel.
pub const MAX_TEXT: usize = 32_767;
/// Largest in-memory array an operation may build (cells): four times the largest spill, the
/// size the evaluator already materialises for range arguments.
pub const ARRAY_MAX: u64 = 4 * SPILL_MAX;

pub fn num(v: f64) -> Res {
    if v.is_finite() { Ok(EV::V(Value::Number(v))) } else { Err(ErrorKind::Num) }
}
pub fn text(s: impl Into<String>) -> Res {
    Ok(EV::V(Value::Text(s.into())))
}
pub fn boolean(b: bool) -> Res {
    Ok(EV::V(Value::Bool(b)))
}

// ---------------------------------------------------------------------------
// Coercion
// ---------------------------------------------------------------------------

pub fn to_num(v: &Value) -> Result<f64, ErrorKind> {
    match v {
        Value::Empty => Ok(0.0),
        // Spreadsheets have no NaN/infinity; reject them so numeric arguments are always finite.
        Value::Number(n) if !n.is_finite() => Err(ErrorKind::Num),
        Value::Number(n) => Ok(*n),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Text(s) => {
            let t = s.trim();
            if t.is_empty() {
                return Err(ErrorKind::Value);
            }
            match crate::numfmt::parse_input(t).0 {
                Value::Number(n) if n.is_finite() => Ok(n),
                Value::Number(_) => Err(ErrorKind::Num),
                _ => Err(ErrorKind::Value),
            }
        }
        Value::Error(e) => Err(*e),
    }
}

pub fn to_text(v: &Value) -> Result<String, ErrorKind> {
    match v {
        Value::Error(e) => Err(*e),
        v => Ok(crate::numfmt::value_to_text(v)),
    }
}

pub fn to_bool(v: &Value) -> Result<bool, ErrorKind> {
    match v {
        Value::Empty => Ok(false),
        Value::Number(n) => Ok(*n != 0.0),
        Value::Bool(b) => Ok(*b),
        Value::Text(s) => {
            if s.eq_ignore_ascii_case("TRUE") {
                Ok(true)
            } else if s.eq_ignore_ascii_case("FALSE") {
                Ok(false)
            } else {
                Err(ErrorKind::Value)
            }
        }
        Value::Error(e) => Err(*e),
    }
}

fn type_rank(v: &Value) -> u8 {
    match v {
        Value::Number(_) | Value::Empty => 0,
        Value::Text(_) => 1,
        Value::Bool(_) => 2,
        Value::Error(_) => 3,
    }
}

/// Spreadsheet ordering: numbers < text < logicals; text is case-insensitive.
pub fn compare(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Empty, Value::Empty) => Ordering::Equal,
        (Value::Empty, Value::Text(s)) => "".cmp(&s.to_lowercase().as_str()),
        (Value::Text(s), Value::Empty) => s.to_lowercase().as_str().cmp(""),
        (Value::Empty, Value::Bool(b)) => false.cmp(b),
        (Value::Bool(b), Value::Empty) => b.cmp(&false),
        (Value::Empty, Value::Number(n)) => 0f64.partial_cmp(n).unwrap_or(Ordering::Equal),
        (Value::Number(n), Value::Empty) => n.partial_cmp(&0.0).unwrap_or(Ordering::Equal),
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Text(x), Value::Text(y)) => x.to_lowercase().cmp(&y.to_lowercase()),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => type_rank(a).cmp(&type_rank(b)),
    }
}

pub fn values_equal(a: &Value, b: &Value) -> bool {
    compare(a, b) == Ordering::Equal && (type_rank(a) == type_rank(b) || a.is_empty() || b.is_empty())
}

fn binop_values(op: BinOp, a: &Value, b: &Value) -> Value {
    if let Value::Error(e) = a {
        return Value::Error(*e);
    }
    if let Value::Error(e) = b {
        return Value::Error(*e);
    }
    let arith = |f: fn(f64, f64) -> Result<f64, ErrorKind>| -> Value {
        match (to_num(a), to_num(b)) {
            (Ok(x), Ok(y)) => match f(x, y) {
                Ok(r) if r.is_finite() => Value::Number(r),
                Ok(_) => Value::Error(ErrorKind::Num),
                Err(e) => Value::Error(e),
            },
            (Err(e), _) | (_, Err(e)) => Value::Error(e),
        }
    };
    match op {
        BinOp::Add => arith(|x, y| Ok(x + y)),
        BinOp::Sub => arith(|x, y| Ok(x - y)),
        BinOp::Mul => arith(|x, y| Ok(x * y)),
        BinOp::Div => arith(|x, y| if y == 0.0 { Err(ErrorKind::Div0) } else { Ok(x / y) }),
        BinOp::Pow => arith(|x, y| {
            if x == 0.0 && y == 0.0 {
                Err(ErrorKind::Num)
            } else if x == 0.0 && y < 0.0 {
                Err(ErrorKind::Div0)
            } else {
                Ok(x.powf(y))
            }
        }),
        BinOp::Concat => match (to_text(a), to_text(b)) {
            // Excel's text limit; also stops chains of `A1&A1` from doubling without bound.
            (Ok(x), Ok(y)) if x.chars().count() + y.chars().count() > MAX_TEXT => Value::Error(ErrorKind::Value),
            (Ok(x), Ok(y)) => Value::Text(x + &y),
            (Err(e), _) | (_, Err(e)) => Value::Error(e),
        },
        _ => {
            let ord = compare(a, b);
            let eq = values_equal(a, b);
            Value::Bool(match op {
                BinOp::Eq => eq,
                BinOp::Ne => !eq,
                BinOp::Lt => ord == Ordering::Less && !eq,
                BinOp::Le => ord != Ordering::Greater || eq,
                BinOp::Gt => ord == Ordering::Greater && !eq,
                BinOp::Ge => ord != Ordering::Less || eq,
                _ => unreachable!(),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Fast hashing for cell keys
// ---------------------------------------------------------------------------

/// FxHash-style hasher: much faster than SipHash for small integer keys.
#[derive(Default, Clone, Copy)]
pub struct FxHasher {
    hash: u64,
}

const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    #[inline]
    fn add(&mut self, w: u64) {
        self.hash = (self.hash.rotate_left(5) ^ w).wrapping_mul(FX_SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut b = [0u8; 8];
            b[..chunk.len()].copy_from_slice(chunk);
            self.add(u64::from_le_bytes(b));
        }
    }
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(i as u64);
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(i as u64);
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

pub type FxBuild = BuildHasherDefault<FxHasher>;
pub type FxMap<K, V> = HashMap<K, V, FxBuild>;
pub type FxSet<K> = HashSet<K, FxBuild>;

// ---------------------------------------------------------------------------
// Evaluator
// ---------------------------------------------------------------------------

pub type Key = (usize, u32, u32);

/// Largest dynamic array that may spill (cells).
const SPILL_MAX: u64 = 1_048_576;

/// Which formula cells are (re)computed; all others report their stored value.
pub enum Mode {
    /// Full recalculation: every formula is evaluated.
    All,
    /// Incremental: only these cells are evaluated.
    Dirty(FxSet<Key>),
    /// Use stored values for every formula (ad-hoc evaluation after a recalc).
    #[allow(dead_code)]
    Stored,
}

/// A dynamic-array result of an anchor cell evaluated in this pass.
pub struct SpillRes {
    pub range: Range,
    pub blocked: bool,
    pub vals: Vec<Vec<Value>>,
}

/// Normalised exact-match key for lookup indexes.
#[derive(Clone, PartialEq, Eq, Hash)]
enum LKey {
    Num(u64),
    Text(String),
    Bool(bool),
}

fn lkey(v: &Value) -> Option<LKey> {
    match v {
        Value::Number(n) => Some(LKey::Num((n + 0.0).to_bits())),
        Value::Text(s) => Some(LKey::Text(s.to_lowercase())),
        Value::Bool(b) => Some(LKey::Bool(*b)),
        _ => None,
    }
}

/// A lookup vector (first column/row of a lookup range) with lazily built hash indexes.
pub struct LookupCol {
    pub vals: Vec<Value>,
    first: RefCell<Option<FxMap<LKey, usize>>>,
    last: RefCell<Option<FxMap<LKey, usize>>>,
}

impl LookupCol {
    pub fn new(vals: Vec<Value>) -> Self {
        LookupCol { vals, first: RefCell::new(None), last: RefCell::new(None) }
    }

    /// Exact (case-insensitive) match without wildcards; first or last occurrence.
    pub fn find(&self, key: &Value, reverse: bool) -> Option<usize> {
        let k = lkey(key)?;
        let cell = if reverse { &self.last } else { &self.first };
        let mut idx = cell.borrow_mut();
        let map = idx.get_or_insert_with(|| {
            let mut m = FxMap::with_capacity_and_hasher(self.vals.len(), Default::default());
            for (i, v) in self.vals.iter().enumerate() {
                if let Some(k) = lkey(v) {
                    if reverse {
                        m.insert(k, i);
                    } else {
                        m.entry(k).or_insert(i);
                    }
                }
            }
            m
        });
        map.get(&k).copied()
    }
}

pub struct Evaluator<'a> {
    pub wb: &'a Workbook,
    mode: Mode,
    results: FxMap<Key, Value>,
    stack: FxSet<Key>,
    pub circular: bool,
    /// (sheet, row, col) of the formula being evaluated.
    pub cur: Key,
    depth: usize,
    name_depth: u32,
    names: FxMap<String, Option<Rc<Expr>>>,
    /// LET variables and lifted-argument bindings (innermost last).
    scopes: Vec<(String, EV)>,
    rng: u64,
    pub now: f64,
    /// Dynamic arrays computed in this pass, by anchor.
    pub spills: FxMap<Key, SpillRes>,
    /// Cells claimed by spills of this pass -> anchor.
    claimed: FxMap<Key, (u32, u32)>,
    /// Per sheet: column -> (r1, r2, anchor) of known spill ranges (previous + this pass).
    spill_cols: Vec<BTreeMap<u32, Vec<(u32, u32, (u32, u32))>>>,
    extents: Vec<Option<Option<(u32, u32)>>>,
    lookups: FxMap<(usize, Range), Rc<LookupCol>>,
    stack_base: usize,
    stack_budget: usize,
}

#[inline(never)]
fn stack_pos() -> usize {
    let marker = 0u8;
    std::hint::black_box(&marker) as *const u8 as usize
}

impl<'a> Evaluator<'a> {
    pub fn new(wb: &'a Workbook, mode: Mode) -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15);
        let spill_cols = wb
            .sheets
            .iter()
            .map(|sh| {
                let mut m: BTreeMap<u32, Vec<(u32, u32, (u32, u32))>> = BTreeMap::new();
                for (a, (rg, blocked)) in &sh.spill.anchors {
                    if !blocked {
                        for c in rg.c1..=rg.c2 {
                            m.entry(c).or_default().push((rg.r1, rg.r2, *a));
                        }
                    }
                }
                m
            })
            .collect();
        let cap = match &mode {
            Mode::Dirty(d) => d.len(),
            _ => 0,
        };
        Evaluator {
            wb,
            mode,
            results: FxMap::with_capacity_and_hasher(cap, Default::default()),
            stack: FxSet::default(),
            circular: false,
            cur: (wb.active, 0, 0),
            depth: 0,
            name_depth: 0,
            names: FxMap::default(),
            scopes: Vec::new(),
            rng: seed | 1,
            now: crate::numfmt::now_serial(),
            spills: FxMap::default(),
            claimed: FxMap::default(),
            spill_cols,
            extents: vec![None; wb.sheets.len()],
            lookups: FxMap::default(),
            stack_base: stack_pos(),
            stack_budget: 512 * 1024,
        }
    }

    fn reserve(&mut self, n: usize) {
        self.results.reserve(n);
    }

    pub fn random(&mut self) -> f64 {
        // xorshift64*
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        (x.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    #[inline]
    fn is_dirty(&self, key: &Key) -> bool {
        match &self.mode {
            Mode::All => true,
            Mode::Dirty(d) => d.contains(key),
            Mode::Stored => false,
        }
    }

    fn too_deep(&self) -> bool {
        self.depth > 1_000_000 || stack_pos().abs_diff(self.stack_base) > self.stack_budget
    }

    /// Value of a cell, evaluating its formula on demand; empty cells report spilled values.
    pub fn cell_value(&mut self, s: usize, r: u32, c: u32) -> Value {
        let wb = self.wb;
        let Some(sh) = wb.sheets.get(s) else { return Value::Empty };
        match sh.get(r, c) {
            Some(cell) if cell.formula.is_some() => {
                let key = (s, r, c);
                if !self.is_dirty(&key) {
                    return cell.value.clone();
                }
                if let Some(v) = self.results.get(&key) {
                    return v.clone();
                }
                self.eval_cell(key, cell.formula.as_ref().and_then(|f| f.ast.as_ref()))
            }
            Some(cell) if !cell.value.is_empty() => cell.value.clone(),
            _ => self.spilled_value(s, r, c),
        }
    }

    fn eval_cell(&mut self, key: Key, ast: Option<&Expr>) -> Value {
        if self.stack.contains(&key) || self.too_deep() {
            self.circular = true;
            return Value::Number(0.0);
        }
        self.stack.insert(key);
        let prev = self.cur;
        let scopes = std::mem::take(&mut self.scopes);
        self.cur = key;
        self.depth += 1;
        let v = match ast {
            Some(e) => {
                let r = self.eval(e);
                self.finish_cell(key, r)
            }
            None => Value::Error(ErrorKind::Name),
        };
        self.depth -= 1;
        self.cur = prev;
        self.scopes = scopes;
        self.stack.remove(&key);
        self.results.insert(key, v.clone());
        v
    }

    /// Value of an empty cell: whatever a dynamic array spills into it.
    fn spilled_value(&mut self, s: usize, r: u32, c: u32) -> Value {
        if let Some(v) = self.claimed_value(s, r, c) {
            return v;
        }
        let wb = self.wb;
        let Some((a, v)) = wb.sheets[s].spill.cells.get(&(r, c)) else { return Value::Empty };
        let akey = (s, a.0, a.1);
        if !self.is_dirty(&akey) {
            return v.clone();
        }
        self.cell_value(s, a.0, a.1);
        self.claimed_value(s, r, c).unwrap_or(Value::Empty)
    }

    fn claimed_value(&self, s: usize, r: u32, c: u32) -> Option<Value> {
        let a = self.claimed.get(&(s, r, c))?;
        let sp = self.spills.get(&(s, a.0, a.1))?;
        sp.vals.get((r - sp.range.r1) as usize)?.get((c - sp.range.c1) as usize).cloned()
    }

    /// Converts a formula result into the value stored in its cell, spilling arrays.
    fn finish_cell(&mut self, key: Key, r: Res) -> Value {
        match r {
            Ok(EV::A(a)) if a.len() > 1 || a.first().is_some_and(|row| row.len() > 1) => self.spill(key, a),
            Ok(EV::R(s, rg)) if !rg.is_single() => {
                let rg = self.clip(s, rg);
                if rg.rows() as u64 * rg.cols() as u64 > SPILL_MAX {
                    return self.spill_blocked_result(key, rg);
                }
                let g = self.grid(EV::R(s, rg));
                self.spill(key, g)
            }
            other => self.top_value(other),
        }
    }

    fn spill_blocked_result(&mut self, key: Key, rg: Range) -> Value {
        let (_, r, c) = key;
        let range = Range::new(r, c, (r + rg.rows() - 1).min(MAX_ROWS - 1), (c + rg.cols() - 1).min(MAX_COLS - 1));
        self.spills.insert(key, SpillRes { range, blocked: true, vals: Vec::new() });
        Value::Error(ErrorKind::Spill)
    }

    fn spill(&mut self, key: Key, mut a: Vec<Vec<Value>>) -> Value {
        let (s, r, c) = key;
        let h = a.len();
        let w = a.iter().map(|row| row.len()).max().unwrap_or(0);
        if h == 0 || w == 0 {
            return Value::Error(ErrorKind::Calc);
        }
        for row in a.iter_mut() {
            row.resize(w, Value::Error(ErrorKind::NA));
            for v in row.iter_mut() {
                if v.is_empty() {
                    *v = Value::Number(0.0);
                }
            }
        }
        if h == 1 && w == 1 {
            return a[0][0].clone();
        }
        let (r2, c2) = (r as u64 + h as u64 - 1, c as u64 + w as u64 - 1);
        let range = Range::new(r, c, r2.min(MAX_ROWS as u64 - 1) as u32, c2.min(MAX_COLS as u64 - 1) as u32);
        if r2 >= MAX_ROWS as u64 || c2 >= MAX_COLS as u64 || (h as u64 * w as u64) > SPILL_MAX || self.is_blocked(s, (r, c), range) {
            self.spills.insert(key, SpillRes { range, blocked: true, vals: Vec::new() });
            return Value::Error(ErrorKind::Spill);
        }
        for rr in range.r1..=range.r2 {
            for cc in range.c1..=range.c2 {
                if (rr, cc) != (r, c) {
                    self.claimed.insert((s, rr, cc), (r, c));
                }
            }
        }
        for cc in range.c1..=range.c2 {
            self.spill_cols[s].entry(cc).or_default().push((range.r1, range.r2, (r, c)));
        }
        if let Some(Some(e)) = &mut self.extents[s] {
            *e = (e.0.max(range.r2), e.1.max(range.c2));
        } else if let Some(None) = self.extents[s] {
            self.extents[s] = Some(Some((range.r2, range.c2)));
        }
        let first = a[0][0].clone();
        self.spills.insert(key, SpillRes { range, blocked: false, vals: a });
        first
    }

    /// Whether a spill range is obstructed by data, merged cells or another spill.
    fn is_blocked(&self, s: usize, anchor: (u32, u32), rg: Range) -> bool {
        let sh = &self.wb.sheets[s];
        if sh.iter_range(rg).any(|(r, c, cell)| (r, c) != anchor && (!cell.value.is_empty() || cell.formula.is_some())) {
            return true;
        }
        if sh.merges.iter().any(|m| m.intersects(&rg)) {
            return true;
        }
        let check_old = !matches!(self.mode, Mode::All) && !sh.spill.cells.is_empty();
        for r in rg.r1..=rg.r2 {
            for c in rg.c1..=rg.c2 {
                if (r, c) == anchor {
                    continue;
                }
                if self.claimed.get(&(s, r, c)).is_some_and(|a| *a != anchor) {
                    return true;
                }
                if check_old {
                    if let Some((a, _)) = sh.spill.cells.get(&(r, c)) {
                        if *a != anchor && !self.is_dirty(&(s, a.0, a.1)) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// Converts a formula result into a single value (ad-hoc evaluation; no spilling).
    pub fn top_value(&mut self, r: Res) -> Value {
        match r {
            Err(e) => Value::Error(e),
            Ok(EV::V(Value::Empty)) => Value::Number(0.0),
            Ok(EV::V(v)) => v,
            Ok(EV::R(s, rg)) => {
                let v = if rg.is_single() {
                    self.cell_value(s, rg.r1, rg.c1)
                } else {
                    match self.implicit(s, rg) {
                        Ok(v) => v,
                        Err(_) => self.cell_value(s, rg.r1, rg.c1),
                    }
                };
                if v.is_empty() { Value::Number(0.0) } else { v }
            }
            Ok(EV::A(a)) => {
                let v = a.first().and_then(|r| r.first()).cloned().unwrap_or(Value::Empty);
                if v.is_empty() { Value::Number(0.0) } else { v }
            }
        }
    }

    /// Implicit intersection of a multi-cell range with the current cell's row/column.
    pub fn implicit(&mut self, s: usize, rg: Range) -> VRes {
        let (_, r, c) = self.cur;
        if rg.c1 == rg.c2 && rg.r1 <= r && r <= rg.r2 {
            return Ok(self.cell_value(s, r, rg.c1));
        }
        if rg.r1 == rg.r2 && rg.c1 <= c && c <= rg.c2 {
            return Ok(self.cell_value(s, rg.r1, c));
        }
        Err(ErrorKind::Value)
    }

    pub fn resolve_ref(&self, r: &RefExpr) -> Result<(usize, Range), ErrorKind> {
        let s = match &r.sheet {
            Some(n) => self.wb.sheet_index(n).ok_or(ErrorKind::Ref)?,
            None => self.cur.0,
        };
        Ok((s, Range::new(r.a.row, r.a.col, r.b.row, r.b.col)))
    }

    /// (max_row, max_col) of a sheet including spilled values.
    pub fn extent(&mut self, s: usize) -> Option<(u32, u32)> {
        if s >= self.extents.len() {
            return None;
        }
        if let Some(e) = self.extents[s] {
            return e;
        }
        let e = self.wb.sheets[s].extent_with_spills();
        self.extents[s] = Some(e);
        e
    }

    /// Clips a range to the populated area of its sheet.
    pub fn clip_extent(&mut self, s: usize, rg: Range) -> Range {
        match self.extent(s) {
            Some((mr, mc)) => Range { r1: rg.r1, c1: rg.c1, r2: rg.r2.min(mr.max(rg.r1)), c2: rg.c2.min(mc.max(rg.c1)) },
            None => Range { r1: rg.r1, c1: rg.c1, r2: rg.r1, c2: rg.c1 },
        }
    }

    /// Clips whole-column / whole-row references to the populated area.
    pub fn clip(&mut self, s: usize, rg: Range) -> Range {
        if rg.is_full_cols() || rg.is_full_rows() { self.clip_extent(s, rg) } else { rg }
    }

    /// Current spill range of an anchor cell, if it spills.
    pub fn spill_range_of(&mut self, s: usize, r: u32, c: u32) -> Option<Range> {
        let wb = self.wb;
        let sh = wb.sheets.get(s)?;
        sh.get(r, c)?.formula.as_ref()?;
        let key = (s, r, c);
        if self.is_dirty(&key) {
            self.cell_value(s, r, c);
            self.spills.get(&key).filter(|sp| !sp.blocked).map(|sp| sp.range)
        } else {
            sh.spill.anchors.get(&(r, c)).filter(|(_, b)| !b).map(|(rg, _)| *rg)
        }
    }

    fn has_spills(&self, s: usize, rg: Range) -> bool {
        self.spill_cols.get(s).is_some_and(|m| m.range(rg.c1..=rg.c2).next().is_some())
    }

    /// Writes spilled values intersecting `rg` into `out` (indexed relative to rg).
    fn overlay_spills(&mut self, s: usize, rg: Range, out: &mut [Vec<Value>]) {
        for (r, c, v) in self.spilled_cells(s, rg) {
            let slot = &mut out[(r - rg.r1) as usize][(c - rg.c1) as usize];
            if slot.is_empty() {
                *slot = v;
            }
        }
    }

    /// Spilled (non-anchor) values inside `rg`, in row-major order; the first anchor wins
    /// where stale spill areas overlap.
    fn spilled_cells(&mut self, s: usize, rg: Range) -> Vec<(u32, u32, Value)> {
        let mut out = Vec::new();
        let mut cands: Vec<(u32, u32)> = Vec::new();
        for (_, ents) in self.spill_cols[s].range(rg.c1..=rg.c2) {
            for &(r1, r2, a) in ents {
                if r1 <= rg.r2 && r2 >= rg.r1 {
                    cands.push(a);
                }
            }
        }
        cands.sort_unstable();
        cands.dedup();
        let wb = self.wb;
        let sh = &wb.sheets[s];
        for a in cands {
            let key = (s, a.0, a.1);
            let Some(sr) = self.spill_range_of(s, a.0, a.1) else { continue };
            if !sr.intersects(&rg) {
                continue;
            }
            let i = Range { r1: sr.r1.max(rg.r1), c1: sr.c1.max(rg.c1), r2: sr.r2.min(rg.r2), c2: sr.c2.min(rg.c2) };
            let fresh = self.is_dirty(&key);
            for r in i.r1..=i.r2 {
                for c in i.c1..=i.c2 {
                    if (r, c) == a {
                        continue;
                    }
                    let v = if fresh {
                        let sp = &self.spills[&key];
                        sp.vals[(r - sp.range.r1) as usize][(c - sp.range.c1) as usize].clone()
                    } else {
                        match sh.spill.cells.get(&(r, c)) {
                            Some((_, v)) => v.clone(),
                            None => continue,
                        }
                    };
                    out.push((r, c, v));
                }
            }
        }
        // Stable sort: equal positions keep anchor order, then the first one is kept.
        out.sort_by_key(|x| (x.0, x.1));
        out.dedup_by_key(|x| (x.0, x.1));
        out
    }

    pub fn eval(&mut self, e: &Expr) -> Res {
        // Nested expressions recurse; stop before the stack runs out (the main thread's stack
        // is small, and names can re-enter evaluation).
        if matches!(e, Expr::Func(..) | Expr::Bin(..) | Expr::Neg(_) | Expr::Plus(_) | Expr::Percent(_) | Expr::Paren(_) | Expr::Array(_))
            && self.too_deep()
        {
            self.circular = true;
            return Err(ErrorKind::Calc);
        }
        match e {
            Expr::Num(n) => Ok(EV::V(Value::Number(*n))),
            Expr::Str(s) => Ok(EV::V(Value::Text(s.clone()))),
            Expr::Bool(b) => Ok(EV::V(Value::Bool(*b))),
            Expr::Err(e) => Err(*e),
            Expr::Missing => Ok(EV::V(Value::Empty)),
            Expr::Ref(r) => {
                let (s, rg) = self.resolve_ref(r)?;
                if r.kind == RefKind::Spill {
                    return match self.spill_range_of(s, rg.r1, rg.c1) {
                        Some(sr) => Ok(EV::R(s, sr)),
                        None => Err(ErrorKind::Ref),
                    };
                }
                Ok(EV::R(s, rg))
            }
            Expr::Name(n) => self.eval_name(n),
            Expr::Paren(e) => self.eval(e),
            Expr::Plus(e) => self.eval(e),
            Expr::Neg(e) => {
                let v = self.eval(e)?;
                self.map_unary(v, |x| binop_values(BinOp::Sub, &Value::Number(0.0), x))
            }
            Expr::Percent(e) => {
                let v = self.eval(e)?;
                self.map_unary(v, |x| binop_values(BinOp::Div, x, &Value::Number(100.0)))
            }
            Expr::Bin(op, a, b) if !matches!(**a, Expr::Bin(..)) => {
                let a = self.eval(a)?;
                let b = self.eval(b)?;
                self.binop(*op, a, b)
            }
            Expr::Bin(..) => {
                // Operator chains (`A1+A2+...+An`) nest to the left: walk the spine iteratively.
                let mut spine = Vec::new();
                let mut cur = e;
                while let Expr::Bin(op, a, b) = cur {
                    spine.push((*op, &**b));
                    cur = a;
                }
                let mut acc = self.eval(cur)?;
                for (op, b) in spine.into_iter().rev() {
                    let b = self.eval(b)?;
                    acc = self.binop(op, acc, b)?;
                }
                Ok(acc)
            }
            Expr::Func(name, args) => super::functions::call(self, name, args),
            Expr::Array(rows) => {
                let mut out = Vec::with_capacity(rows.len());
                for row in rows {
                    let mut r = Vec::with_capacity(row.len());
                    for e in row {
                        r.push(match self.eval(e) {
                            Ok(EV::V(v)) => v,
                            Ok(_) => Value::Error(ErrorKind::Value),
                            Err(e) => Value::Error(e),
                        });
                    }
                    out.push(r);
                }
                Ok(EV::A(out))
            }
        }
    }

    fn binop(&mut self, op: BinOp, a: EV, b: EV) -> Res {
        if self.is_multi(&a) || self.is_multi(&b) {
            let ga = self.grid(a);
            let gb = self.grid(b);
            // A column against a row multiplies sizes: refuse arrays too large to hold.
            let (rows, cols) = broadcast_dims(&ga, &gb);
            if rows as u64 * cols as u64 > ARRAY_MAX {
                return Err(ErrorKind::Calc);
            }
            Ok(EV::A(broadcast(&ga, &gb, |x, y| binop_values(op, x, y))))
        } else {
            let x = self.scalar(a)?;
            let y = self.scalar(b)?;
            match binop_values(op, &x, &y) {
                Value::Error(e) => Err(e),
                v => Ok(EV::V(v)),
            }
        }
    }

    /// Binds a LET variable / lifted argument for the duration of a call.
    pub fn bind(&mut self, name: String, v: EV) {
        self.scopes.push((name, v));
    }

    pub fn unbind(&mut self, n: usize) {
        let keep = self.scopes.len().saturating_sub(n);
        self.scopes.truncate(keep);
    }

    fn eval_name(&mut self, n: &str) -> Res {
        if let Some((_, v)) = self.scopes.iter().rev().find(|(k, _)| k.eq_ignore_ascii_case(n)) {
            return match v {
                EV::V(Value::Error(e)) => Err(*e),
                v => Ok(v.clone()),
            };
        }
        let key = n.to_ascii_uppercase();
        let parsed = match self.names.get(&key) {
            Some(p) => p.clone(),
            None => {
                let p = self
                    .wb
                    .names
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(n))
                    .and_then(|(_, f)| super::parse(f.trim_start_matches('=')).ok())
                    .map(Rc::new);
                self.names.insert(key, p.clone());
                p
            }
        };
        match parsed {
            Some(e) => {
                if self.name_depth > 200 || self.too_deep() {
                    return Err(ErrorKind::Name);
                }
                self.name_depth += 1;
                let r = self.eval(&e);
                self.name_depth -= 1;
                r
            }
            None => Err(ErrorKind::Name),
        }
    }

    fn map_unary(&mut self, v: EV, f: impl Fn(&Value) -> Value) -> Res {
        if self.is_multi(&v) {
            let g = self.grid(v);
            Ok(EV::A(g.iter().map(|r| r.iter().map(&f).collect()).collect()))
        } else {
            match f(&self.scalar(v)?) {
                Value::Error(e) => Err(e),
                v => Ok(EV::V(v)),
            }
        }
    }

    pub fn is_multi(&self, v: &EV) -> bool {
        match v {
            EV::V(_) => false,
            EV::R(_, rg) => !rg.is_single(),
            EV::A(a) => a.len() > 1 || a.first().is_some_and(|r| r.len() > 1),
        }
    }

    /// Reduces an evaluation result to a single value (implicit intersection for ranges).
    pub fn scalar(&mut self, v: EV) -> VRes {
        let v = match v {
            EV::V(v) => v,
            EV::R(s, rg) => {
                if rg.is_single() {
                    self.cell_value(s, rg.r1, rg.c1)
                } else {
                    self.implicit(s, rg)?
                }
            }
            EV::A(a) => a.into_iter().next().and_then(|r| r.into_iter().next()).unwrap_or(Value::Empty),
        };
        match v {
            Value::Error(e) => Err(e),
            v => Ok(v),
        }
    }

    /// Materializes any result as a 2-D grid of values.
    pub fn grid(&mut self, v: EV) -> Vec<Vec<Value>> {
        match v {
            EV::V(v) => vec![vec![v]],
            EV::A(mut a) => {
                // Callers index grids as rectangles: pad any ragged rows.
                let w = a.iter().map(|r| r.len()).max().unwrap_or(0);
                if a.iter().any(|r| r.len() != w) {
                    for r in a.iter_mut() {
                        r.resize(w, Value::Error(ErrorKind::NA));
                    }
                }
                a
            }
            EV::R(s, rg) => {
                let mut rg = self.clip(s, rg);
                if rg.rows() as u64 * rg.cols() as u64 > 4 * SPILL_MAX {
                    rg = self.clip_extent(s, rg);
                }
                self.grid_of(s, rg)
            }
        }
    }

    /// Like `grid`, but ranges are clipped to the used area: for aggregates whose result does
    /// not depend on trailing blank cells (keeps `SUMIF($A$1:$A$100000,...)` cheap).
    pub fn grid_clipped(&mut self, v: EV) -> Vec<Vec<Value>> {
        match v {
            EV::R(s, rg) => {
                let rg = self.clip_extent(s, rg);
                self.grid_of(s, rg)
            }
            other => self.grid(other),
        }
    }

    fn grid_of(&mut self, s: usize, rg: Range) -> Vec<Vec<Value>> {
        let mut out = vec![vec![Value::Empty; rg.cols() as usize]; rg.rows() as usize];
        let wb = self.wb;
        if let Some(sh) = wb.sheets.get(s) {
            for (r, c, cell) in sh.iter_range(rg) {
                let v = if cell.formula.is_some() { self.cell_value(s, r, c) } else { cell.value.clone() };
                out[(r - rg.r1) as usize][(c - rg.c1) as usize] = v;
            }
            if self.has_spills(s, rg) {
                self.overlay_spills(s, rg, &mut out);
            }
        }
        out
    }

    /// Values of a one-dimensional lookup range (cached per pass, with hash indexes).
    pub fn lookup_col(&mut self, s: usize, rg: Range) -> Rc<LookupCol> {
        let rg = self.clip_extent(s, rg);
        if let Some(c) = self.lookups.get(&(s, rg)) {
            return c.clone();
        }
        let g = self.grid_of(s, rg);
        let vals: Vec<Value> = if rg.rows() == 1 { g.into_iter().next().unwrap_or_default() } else { g.into_iter().map(|r| r.into_iter().next().unwrap_or_default()).collect() };
        let col = Rc::new(LookupCol::new(vals));
        self.lookups.insert((s, rg), col.clone());
        col
    }

    /// Calls `f` for every value in the argument; `from_ref` is true for range/array members.
    pub fn walk(&mut self, v: EV, skip_hidden: bool, f: &mut dyn FnMut(&Value, bool) -> Result<(), ErrorKind>) -> Result<(), ErrorKind> {
        self.walk_cells(v, skip_hidden, false, f)
    }

    /// Like `walk`; with `skip_nested`, cells holding SUBTOTAL/AGGREGATE formulas are left out.
    /// Ranges are visited sparsely (populated and spilled cells only, row-major), so their
    /// size does not matter.
    pub fn walk_cells(
        &mut self,
        v: EV,
        skip_hidden: bool,
        skip_nested: bool,
        f: &mut dyn FnMut(&Value, bool) -> Result<(), ErrorKind>,
    ) -> Result<(), ErrorKind> {
        match v {
            EV::V(v) => f(&v, false),
            EV::A(a) => {
                for row in &a {
                    for v in row {
                        f(v, true)?;
                    }
                }
                Ok(())
            }
            EV::R(s, rg) => {
                let wb = self.wb;
                let Some(sh) = wb.sheets.get(s) else { return Err(ErrorKind::Ref) };
                let hidden = |r: u32| skip_hidden && sh.hidden_rows.contains(&r);
                let spilled = if self.has_spills(s, rg) { self.spilled_cells(s, rg) } else { Vec::new() };
                let mut spilled = spilled.into_iter().peekable();
                for (r, c, cell) in sh.iter_range(rg) {
                    // Spilled values before this cell, then the one at it (shown only if empty).
                    while let Some((sr, _, v)) = spilled.next_if(|x| (x.0, x.1) < (r, c)) {
                        if !hidden(sr) {
                            f(&v, true)?;
                        }
                    }
                    let here = spilled.next_if(|x| (x.0, x.1) == (r, c)).map(|x| x.2);
                    if hidden(r) {
                        continue;
                    }
                    if cell.formula.is_some() {
                        if skip_nested && is_subtotal_cell(cell) {
                            continue;
                        }
                        let v = self.cell_value(s, r, c);
                        f(&v, true)?;
                    } else {
                        match here {
                            Some(v) if cell.value.is_empty() => f(&v, true)?,
                            _ => f(&cell.value, true)?,
                        }
                    }
                }
                for (sr, _, v) in spilled {
                    if !hidden(sr) {
                        f(&v, true)?;
                    }
                }
                Ok(())
            }
        }
    }

    // ----- argument helpers -----

    pub fn arg(&mut self, args: &[Expr], i: usize) -> Res {
        match args.get(i) {
            Some(e) => self.eval(e),
            None => Ok(EV::V(Value::Empty)),
        }
    }
    pub fn value(&mut self, args: &[Expr], i: usize) -> VRes {
        let v = self.arg(args, i)?;
        self.scalar(v)
    }
    pub fn has(&self, args: &[Expr], i: usize) -> bool {
        args.get(i).is_some_and(|e| !matches!(e, Expr::Missing))
    }
    pub fn number(&mut self, args: &[Expr], i: usize) -> Result<f64, ErrorKind> {
        let v = self.value(args, i)?;
        to_num(&v)
    }
    pub fn number_or(&mut self, args: &[Expr], i: usize, d: f64) -> Result<f64, ErrorKind> {
        if self.has(args, i) { self.number(args, i) } else { Ok(d) }
    }
    pub fn string(&mut self, args: &[Expr], i: usize) -> Result<String, ErrorKind> {
        let v = self.value(args, i)?;
        to_text(&v)
    }
    pub fn boolean(&mut self, args: &[Expr], i: usize) -> Result<bool, ErrorKind> {
        let v = self.value(args, i)?;
        to_bool(&v)
    }
    pub fn bool_or(&mut self, args: &[Expr], i: usize, d: bool) -> Result<bool, ErrorKind> {
        if self.has(args, i) { self.boolean(args, i) } else { Ok(d) }
    }

    /// Numbers from all arguments using SUM-style rules (text/logicals in ranges ignored).
    pub fn numbers(&mut self, args: &[Expr], skip_hidden: bool) -> Result<Vec<f64>, ErrorKind> {
        self.numbers_opts(args, skip_hidden, false)
    }

    /// `numbers`, optionally leaving out cells that hold SUBTOTAL/AGGREGATE formulas.
    pub fn numbers_opts(&mut self, args: &[Expr], skip_hidden: bool, skip_nested: bool) -> Result<Vec<f64>, ErrorKind> {
        let mut out = Vec::new();
        for e in args {
            if matches!(e, Expr::Missing) {
                continue;
            }
            let v = self.eval(e)?;
            self.walk_cells(v, skip_hidden, skip_nested, &mut |v, from_ref| {
                match v {
                    Value::Number(n) => out.push(*n),
                    Value::Error(e) => return Err(*e),
                    Value::Bool(b) if !from_ref => out.push(if *b { 1.0 } else { 0.0 }),
                    Value::Text(s) if !from_ref => out.push(to_num(&Value::Text(s.clone()))?),
                    _ => {}
                }
                Ok(())
            })?;
        }
        Ok(out)
    }
}

/// Whether a cell holds a SUBTOTAL or AGGREGATE formula; both functions ignore such cells in
/// their ranges to avoid double counting, like Excel.
pub fn is_subtotal_cell(cell: &Cell) -> bool {
    cell.formula.as_ref().is_some_and(|f| {
        let t = f.text.to_ascii_uppercase();
        t.contains("SUBTOTAL(") || t.contains("AGGREGATE(")
    })
}

/// Result size of `broadcast(a, b)`.
pub fn broadcast_dims(a: &[Vec<Value>], b: &[Vec<Value>]) -> (usize, usize) {
    let rows = if a.len() == 1 { b.len() } else if b.len() == 1 { a.len() } else { a.len().max(b.len()) };
    let ca = a.first().map_or(0, |r| r.len());
    let cb = b.first().map_or(0, |r| r.len());
    let cols = if ca == 1 { cb } else if cb == 1 { ca } else { ca.max(cb) };
    (rows, cols)
}

/// Element-wise operation with Excel's array broadcasting (single rows/columns expand,
/// mismatched sizes pad with #N/A).
pub fn broadcast(a: &[Vec<Value>], b: &[Vec<Value>], f: impl Fn(&Value, &Value) -> Value) -> Vec<Vec<Value>> {
    let (rows, cols) = broadcast_dims(a, b);
    let get = |g: &[Vec<Value>], r: usize, c: usize| -> Value {
        let rr = if g.len() == 1 { 0 } else { r };
        let Some(row) = g.get(rr) else { return Value::Error(ErrorKind::NA) };
        let cc = if row.len() == 1 { 0 } else { c };
        row.get(cc).cloned().unwrap_or(Value::Error(ErrorKind::NA))
    };
    (0..rows).map(|r| (0..cols).map(|c| f(&get(a, r, c), &get(b, r, c))).collect()).collect()
}

// ---------------------------------------------------------------------------
// Recalculation drivers
// ---------------------------------------------------------------------------

/// Stack reserved for recalculation threads (address space only; committed on use).
const BIG_STACK: usize = 1 << 30;
const STACK_MARGIN: usize = 8 << 20;
/// Edits larger than this many cells recalculate everything.
const INCREMENTAL_MAX_CELLS: u64 = 50_000;

struct PassOut {
    results: Vec<(Key, Value)>,
    spills: FxMap<Key, SpillRes>,
    circular: bool,
}

/// Runs `f` on a thread with a large stack so deep reference chains cannot overflow.
fn on_big_stack<R: Send, F: FnOnce(usize) -> R + Send>(f: F) -> R {
    let slot = std::sync::Mutex::new(Some(f));
    let take = || slot.lock().ok().and_then(|mut g| g.take());
    std::thread::scope(|sc| {
        let spawned = std::thread::Builder::new()
            .name("recalc".into())
            .stack_size(BIG_STACK)
            .spawn_scoped(sc, || take().map(|f| f(BIG_STACK - STACK_MARGIN)));
        let out = match spawned {
            Ok(h) => match h.join() {
                Ok(r) => r,
                Err(e) => std::panic::resume_unwind(e),
            },
            Err(_) => None,
        };
        match out {
            Some(r) => r,
            None => (take().expect("recalc closure"))(512 * 1024),
        }
    })
}

fn run_pass(wb: &Workbook, mode: Mode, targets: &[Key]) -> PassOut {
    on_big_stack(|budget| {
        let mut ev = Evaluator::new(wb, mode);
        ev.stack_base = stack_pos();
        ev.stack_budget = budget;
        ev.reserve(targets.len());
        for &(s, r, c) in targets {
            ev.cell_value(s, r, c);
        }
        PassOut { results: ev.results.into_iter().collect(), spills: ev.spills, circular: ev.circular }
    })
}

fn write_results(wb: &mut Workbook, results: Vec<(Key, Value)>) {
    for ((s, r, c), v) in results {
        if let Some(cell) = wb.sheets[s].get_mut(r, c) {
            if cell.formula.is_some() {
                cell.value = v;
            }
        }
    }
}

fn add_spill(sp: &mut Spill, anchor: (u32, u32), res: SpillRes) {
    sp.anchors.insert(anchor, (res.range, res.blocked));
    if res.blocked {
        return;
    }
    for (i, row) in res.vals.into_iter().enumerate() {
        for (j, v) in row.into_iter().enumerate() {
            let pos = (res.range.r1 + i as u32, res.range.c1 + j as u32);
            if pos != anchor {
                sp.cells.insert(pos, (anchor, v));
            }
        }
    }
}

fn remove_spill(sp: &mut Spill, anchor: (u32, u32)) {
    if let Some((rg, blocked)) = sp.anchors.remove(&anchor) {
        if !blocked {
            for r in rg.r1..=rg.r2 {
                for c in rg.c1..=rg.c2 {
                    if sp.cells.get(&(r, c)).is_some_and(|(a, _)| *a == anchor) {
                        sp.cells.remove(&(r, c));
                    }
                }
            }
        }
    }
}

/// Recalculates every formula in the workbook (loads, structural changes).
pub fn recalc(wb: &mut Workbook) {
    wb.calc.invalidate();
    let mut targets = Vec::new();
    for (si, s) in wb.sheets.iter().enumerate() {
        for (r, row) in &s.rows {
            for (c, cell) in row {
                if cell.formula.is_some() {
                    targets.push((si, *r, *c));
                }
            }
        }
    }
    let had_spills = wb.sheets.iter().any(|s| !s.spill.anchors.is_empty());
    if targets.is_empty() && !had_spills {
        wb.circular = false;
        return;
    }
    for _pass in 0..4 {
        let out = run_pass(wb, Mode::All, &targets);
        write_results(wb, out.results);
        let mut fresh: Vec<Spill> = wb.sheets.iter().map(|_| Spill::default()).collect();
        for ((s, r, c), res) in out.spills {
            add_spill(&mut fresh[s], (r, c), res);
        }
        let mut stable = true;
        for (sh, sp) in wb.sheets.iter_mut().zip(fresh) {
            if sh.spill.anchors != sp.anchors {
                stable = false;
            }
            sh.spill = sp;
        }
        wb.circular = out.circular;
        if stable {
            break;
        }
    }
}

/// Recalculates only what depends on the given cells (contents edited in place).
#[cfg(test)]
pub fn recalc_cells(wb: &mut Workbook, cells: &[(usize, u32, u32)]) {
    let ranges: Vec<(usize, Range)> = cells.iter().map(|&(s, r, c)| (s, Range::cell(r, c))).collect();
    recalc_ranges(wb, &ranges);
}

/// Recalculates only what depends on the given ranges, whose cell contents were edited.
/// Falls back to a full recalculation whenever the incremental state cannot be trusted.
pub fn recalc_ranges(wb: &mut Workbook, edited: &[(usize, Range)]) {
    let area: u64 = edited.iter().map(|(_, rg)| rg.rows() as u64 * rg.cols() as u64).sum();
    if wb.circular || area > INCREMENTAL_MAX_CELLS || edited.iter().any(|(s, _)| *s >= wb.sheets.len()) {
        recalc(wb);
        return;
    }
    let mut calc = std::mem::take(&mut wb.calc);
    if calc.is_valid_for(wb) {
        calc.update(wb, edited);
    } else {
        calc.build(wb);
    }
    let mut regions: Vec<(usize, Range)> = Vec::new();
    let mut converged = false;
    for round in 0..4 {
        let first = round == 0;
        let dirty = dirty_set(wb, &mut calc, if first { edited } else { &[] }, &regions, first);
        let mut targets: Vec<Key> = dirty.iter().copied().collect();
        targets.sort_unstable();
        let out = run_pass(wb, Mode::Dirty(dirty), &targets);
        write_results(wb, out.results);
        wb.circular = out.circular;
        // Apply spill changes of the anchors evaluated in this round.
        regions.clear();
        let mut new_spills: Vec<Vec<((u32, u32), SpillRes)>> = wb.sheets.iter().map(|_| Vec::new()).collect();
        for ((s, r, c), res) in out.spills {
            new_spills[s].push(((r, c), res));
        }
        for (s, news) in new_spills.into_iter().enumerate() {
            let sp = &mut wb.sheets[s].spill;
            let evaluated: Vec<(u32, u32)> = targets.iter().filter(|k| k.0 == s).map(|k| (k.1, k.2)).collect();
            let mut olds = Vec::new();
            for a in &evaluated {
                if let Some(o) = sp.anchors.get(a).copied() {
                    olds.push((*a, o));
                }
            }
            for (a, o) in &olds {
                let now = news.iter().find(|(na, _)| na == a).map(|(_, r)| (r.range, r.blocked));
                if now != Some(*o) {
                    regions.push((s, o.0));
                }
                remove_spill(sp, *a);
            }
            for (a, res) in news {
                if olds.iter().find(|(oa, _)| *oa == a).map(|(_, o)| *o) != Some((res.range, res.blocked)) {
                    regions.push((s, res.range));
                }
                add_spill(sp, a, res);
            }
        }
        if regions.is_empty() || wb.circular {
            converged = true;
            break;
        }
    }
    wb.calc = calc;
    if !converged {
        recalc(wb);
    }
}

/// Formula cells to recompute after `edited` cells changed and `regions` changed spill values.
fn dirty_set(wb: &Workbook, calc: &mut super::deps::CalcState, edited: &[(usize, Range)], regions: &[(usize, Range)], volatile: bool) -> FxSet<Key> {
    let mut dirty: FxSet<Key> = FxSet::default();
    let mut queue: Vec<(usize, Range)> = Vec::new();
    for &(s, rg) in edited {
        let sh = &wb.sheets[s];
        for r in rg.r1..=rg.r2 {
            for c in rg.c1..=rg.c2 {
                dirty.insert((s, r, c));
            }
        }
        for (a, (srg, blocked)) in &sh.spill.anchors {
            if rg.contains(a.0, a.1) {
                if !blocked {
                    queue.push((s, *srg));
                }
            }
        }
        queue.push((s, rg));
    }
    for &(s, rg) in edited.iter().chain(regions) {
        // Anchors whose (possibly blocked) spill area overlaps a change must re-spill.
        for (a, (srg, blocked)) in &wb.sheets[s].spill.anchors {
            if srg.intersects(&rg) && dirty.insert((s, a.0, a.1)) {
                queue.push((s, Range::cell(a.0, a.1)));
                if !blocked {
                    queue.push((s, *srg));
                }
            }
        }
        queue.push((s, rg));
    }
    if volatile {
        for k in calc.volatile() {
            if dirty.insert(*k) {
                queue.push((k.0, Range::cell(k.1, k.2)));
            }
        }
    }
    while let Some((s, rg)) = queue.pop() {
        calc.dependents(s, rg, &mut |k| {
            if dirty.insert(k) {
                queue.push((k.0, Range::cell(k.1, k.2)));
                if let Some((srg, false)) = wb.sheets[k.0].spill.anchors.get(&(k.1, k.2)) {
                    queue.push((k.0, *srg));
                }
            }
        });
    }
    calc.end_propagation();
    dirty
}

/// Evaluates a standalone formula in the context of a cell (used by tests).
#[cfg(test)]
pub fn eval_formula(wb: &Workbook, sheet: usize, r: u32, c: u32, src: &str) -> Value {
    let Ok(e) = super::parse(src.trim_start_matches('=')) else { return Value::Error(ErrorKind::Name) };
    let mut ev = Evaluator::new(wb, Mode::Stored);
    ev.cur = (sheet, r, c);
    let res = ev.eval(&e);
    ev.top_value(res)
}

/// Evaluates a parsed expression in the context of a cell (conditional formats, validation).
pub fn eval_expr(wb: &Workbook, sheet: usize, r: u32, c: u32, e: &Expr) -> Value {
    let mut ev = Evaluator::new(wb, Mode::Stored);
    ev.cur = (sheet, r, c);
    let res = ev.eval(e);
    ev.top_value(res)
}

/// Evaluates a parsed expression to a grid of values, expanding ranges (validation lists).
pub fn eval_grid(wb: &Workbook, sheet: usize, r: u32, c: u32, e: &Expr) -> Vec<Vec<Value>> {
    let mut ev = Evaluator::new(wb, Mode::Stored);
    ev.cur = (sheet, r, c);
    match ev.eval(e) {
        Ok(v) => ev.grid(v),
        Err(k) => vec![vec![Value::Error(k)]],
    }
}

/// Evaluates a standalone formula and returns its full (array) result.
#[cfg(test)]
pub fn eval_array(wb: &Workbook, sheet: usize, r: u32, c: u32, src: &str) -> Vec<Vec<Value>> {
    let Ok(e) = super::parse(src.trim_start_matches('=')) else { return vec![vec![Value::Error(ErrorKind::Name)]] };
    let mut ev = Evaluator::new(wb, Mode::Stored);
    ev.cur = (sheet, r, c);
    match ev.eval(&e) {
        Err(e) => vec![vec![Value::Error(e)]],
        Ok(v) => ev.grid(v),
    }
}
