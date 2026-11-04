//! Formula evaluator and workbook recalculation.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use super::parser::{BinOp, Expr, RefExpr};
use crate::model::{ErrorKind, Range, Value, Workbook};

/// Result of evaluating an expression: a scalar, a reference, or an in-memory array.
#[derive(Clone, Debug)]
pub enum EV {
    V(Value),
    R(usize, Range),
    A(Vec<Vec<Value>>),
}

pub type Res = Result<EV, ErrorKind>;
pub type VRes = Result<Value, ErrorKind>;

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
        Value::Number(n) => Ok(*n),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Text(s) => {
            let t = s.trim();
            if t.is_empty() {
                return Err(ErrorKind::Value);
            }
            match crate::numfmt::parse_input(t).0 {
                Value::Number(n) => Ok(n),
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
// Evaluator
// ---------------------------------------------------------------------------

type Key = (usize, u32, u32);

pub struct Evaluator<'a> {
    pub wb: &'a Workbook,
    results: HashMap<Key, Value>,
    stack: HashSet<Key>,
    pub circular: bool,
    /// (sheet, row, col) of the formula being evaluated.
    pub cur: Key,
    depth: usize,
    names: HashMap<String, Option<Expr>>,
    rng: u64,
    pub now: f64,
}

impl<'a> Evaluator<'a> {
    pub fn new(wb: &'a Workbook) -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15);
        Evaluator {
            wb,
            results: HashMap::new(),
            stack: HashSet::new(),
            circular: false,
            cur: (wb.active, 0, 0),
            depth: 0,
            names: HashMap::new(),
            rng: seed | 1,
            now: crate::numfmt::now_serial(),
        }
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

    /// Value of a cell, evaluating its formula on demand.
    pub fn cell_value(&mut self, s: usize, r: u32, c: u32) -> Value {
        let wb = self.wb;
        let Some(cell) = wb.sheets.get(s).and_then(|sh| sh.get(r, c)) else { return Value::Empty };
        let Some(f) = &cell.formula else { return cell.value.clone() };
        let key = (s, r, c);
        if let Some(v) = self.results.get(&key) {
            return v.clone();
        }
        if self.stack.contains(&key) || self.depth > 100_000 {
            self.circular = true;
            return Value::Number(0.0);
        }
        self.stack.insert(key);
        let prev = self.cur;
        self.cur = key;
        self.depth += 1;
        let v = match &f.ast {
            Some(e) => {
                let r = self.eval(e);
                self.top_value(r)
            }
            None => Value::Error(ErrorKind::Name),
        };
        self.depth -= 1;
        self.cur = prev;
        self.stack.remove(&key);
        self.results.insert(key, v.clone());
        v
    }

    /// Converts a formula result into the value stored in its cell.
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

    fn implicit(&mut self, s: usize, rg: Range) -> VRes {
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

    /// Clips a range to the populated area of its sheet (whole-column refs etc).
    pub fn clip(&self, s: usize, rg: Range) -> Range {
        match self.wb.sheets.get(s).and_then(|sh| sh.extent()) {
            Some((mr, mc)) => Range { r1: rg.r1, c1: rg.c1, r2: rg.r2.min(mr.max(rg.r1)), c2: rg.c2.min(mc.max(rg.c1)) },
            None => Range { r1: rg.r1, c1: rg.c1, r2: rg.r1, c2: rg.c1 },
        }
    }

    pub fn eval(&mut self, e: &Expr) -> Res {
        match e {
            Expr::Num(n) => Ok(EV::V(Value::Number(*n))),
            Expr::Str(s) => Ok(EV::V(Value::Text(s.clone()))),
            Expr::Bool(b) => Ok(EV::V(Value::Bool(*b))),
            Expr::Err(e) => Err(*e),
            Expr::Missing => Ok(EV::V(Value::Empty)),
            Expr::Ref(r) => {
                let (s, rg) = self.resolve_ref(r)?;
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
            Expr::Bin(op, a, b) => {
                let a = self.eval(a)?;
                let b = self.eval(b)?;
                if self.is_multi(&a) || self.is_multi(&b) {
                    let ga = self.grid(a);
                    let gb = self.grid(b);
                    Ok(EV::A(broadcast(&ga, &gb, |x, y| binop_values(*op, x, y))))
                } else {
                    let x = self.scalar(a)?;
                    let y = self.scalar(b)?;
                    match binop_values(*op, &x, &y) {
                        Value::Error(e) => Err(e),
                        v => Ok(EV::V(v)),
                    }
                }
            }
            Expr::Func(name, args) => super::functions::call(self, name, args),
            Expr::Array(rows) => {
                let mut out = Vec::new();
                for row in rows {
                    let mut r = Vec::new();
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

    fn eval_name(&mut self, n: &str) -> Res {
        let key = n.to_ascii_uppercase();
        if !self.names.contains_key(&key) {
            let parsed = self
                .wb
                .names
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(n))
                .and_then(|(_, f)| super::parse(f.trim_start_matches('=')).ok());
            self.names.insert(key.clone(), parsed);
        }
        match self.names.get(&key).cloned().flatten() {
            Some(e) => {
                if self.depth > 200 {
                    return Err(ErrorKind::Name);
                }
                self.depth += 1;
                let r = self.eval(&e);
                self.depth -= 1;
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
            EV::A(a) => a.first().and_then(|r| r.first()).cloned().unwrap_or(Value::Empty),
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
            EV::A(a) => a,
            EV::R(s, rg) => {
                let rg = self.clip(s, rg);
                let mut out = vec![vec![Value::Empty; rg.cols() as usize]; rg.rows() as usize];
                let wb = self.wb;
                if let Some(sh) = wb.sheets.get(s) {
                    for (r, c, cell) in sh.iter_range(rg) {
                        let v = if cell.formula.is_some() { self.cell_value(s, r, c) } else { cell.value.clone() };
                        out[(r - rg.r1) as usize][(c - rg.c1) as usize] = v;
                    }
                }
                out
            }
        }
    }

    /// Calls `f` for every value in the argument; `from_ref` is true for range/array members.
    pub fn walk(&mut self, v: EV, skip_hidden: bool, f: &mut dyn FnMut(Value, bool) -> Result<(), ErrorKind>) -> Result<(), ErrorKind> {
        match v {
            EV::V(v) => f(v, false),
            EV::A(a) => {
                for row in a {
                    for v in row {
                        f(v, true)?;
                    }
                }
                Ok(())
            }
            EV::R(s, rg) => {
                let wb = self.wb;
                let Some(sh) = wb.sheets.get(s) else { return Err(ErrorKind::Ref) };
                for (r, c, cell) in sh.iter_range(rg) {
                    if skip_hidden && sh.hidden_rows.contains(&r) {
                        continue;
                    }
                    let v = if cell.formula.is_some() { self.cell_value(s, r, c) } else { cell.value.clone() };
                    f(v, true)?;
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
        let mut out = Vec::new();
        for e in args {
            if matches!(e, Expr::Missing) {
                continue;
            }
            let v = self.eval(e)?;
            self.walk(v, skip_hidden, &mut |v, from_ref| {
                match v {
                    Value::Number(n) => out.push(n),
                    Value::Error(e) => return Err(e),
                    Value::Bool(b) if !from_ref => out.push(if b { 1.0 } else { 0.0 }),
                    Value::Text(s) if !from_ref => out.push(to_num(&Value::Text(s))?),
                    _ => {}
                }
                Ok(())
            })?;
        }
        Ok(out)
    }
}

pub fn broadcast(a: &[Vec<Value>], b: &[Vec<Value>], f: impl Fn(&Value, &Value) -> Value) -> Vec<Vec<Value>> {
    let rows = if a.len() == 1 { b.len() } else if b.len() == 1 { a.len() } else { a.len().min(b.len()) };
    let ca = a.first().map_or(0, |r| r.len());
    let cb = b.first().map_or(0, |r| r.len());
    let cols = if ca == 1 { cb } else if cb == 1 { ca } else { ca.min(cb) };
    let get = |g: &[Vec<Value>], r: usize, c: usize| -> Value {
        let rr = if g.len() == 1 { 0 } else { r };
        let row = &g[rr];
        let cc = if row.len() == 1 { 0 } else { c };
        row.get(cc).cloned().unwrap_or(Value::Error(ErrorKind::NA))
    };
    (0..rows).map(|r| (0..cols).map(|c| f(&get(a, r, c), &get(b, r, c))).collect()).collect()
}

/// Recalculates every formula in the workbook.
pub fn recalc(wb: &mut Workbook) {
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
    let (results, circular) = {
        let mut ev = Evaluator::new(wb);
        for &(s, r, c) in &targets {
            ev.cell_value(s, r, c);
        }
        (ev.results, ev.circular)
    };
    for ((s, r, c), v) in results {
        if let Some(cell) = wb.sheets[s].get_mut(r, c) {
            cell.value = v;
        }
    }
    wb.circular = circular;
}

/// Evaluates a standalone formula in the context of a cell (used by tests and the Function dialog).
pub fn eval_formula(wb: &Workbook, sheet: usize, r: u32, c: u32, src: &str) -> Value {
    let Ok(e) = super::parse(src.trim_start_matches('=')) else { return Value::Error(ErrorKind::Name) };
    let mut ev = Evaluator::new(wb);
    ev.cur = (sheet, r, c);
    let res = ev.eval(&e);
    ev.top_value(res)
}

/// Evaluates a parsed expression in the context of a cell (conditional formats, validation).
pub fn eval_expr(wb: &Workbook, sheet: usize, r: u32, c: u32, e: &Expr) -> Value {
    let mut ev = Evaluator::new(wb);
    ev.cur = (sheet, r, c);
    let res = ev.eval(e);
    ev.top_value(res)
}

/// Evaluates a parsed expression to a grid of values, expanding ranges (validation lists).
pub fn eval_grid(wb: &Workbook, sheet: usize, r: u32, c: u32, e: &Expr) -> Vec<Vec<Value>> {
    let mut ev = Evaluator::new(wb);
    ev.cur = (sheet, r, c);
    match ev.eval(e) {
        Ok(v) => ev.grid(v),
        Err(k) => vec![vec![Value::Error(k)]],
    }
}
