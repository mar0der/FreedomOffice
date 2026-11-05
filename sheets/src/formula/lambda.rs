//! LAMBDA and the functions built on it: MAP, REDUCE, SCAN, BYROW, BYCOL, MAKEARRAY,
//! ISOMITTED, GROUPBY and PIVOTBY, plus calls of LET-bound and named (defined-name) lambdas.

use std::cmp::Ordering;
use std::rc::Rc;

use super::eval::{boolean, compare, Evaluator, Lambda, Res, ARRAY_MAX, EV};
use super::functions::{element, is_builtin};
use super::parser::Expr;
use crate::model::{ErrorKind as E, Range, Value};

type G = Vec<Vec<Value>>;

/// A function argument: a LAMBDA value or a built-in named directly (`BYROW(A1:C3,SUM)`).
pub(super) enum Callable {
    L(Rc<Lambda>),
    B(String),
}

pub(super) fn callable(ev: &mut Evaluator, e: &Expr) -> Result<Callable, E> {
    if let Expr::Name(n) = e {
        let up = n.to_ascii_uppercase();
        if ev.bound(n).is_none() && is_builtin(&up) && !ev.wb.names.iter().any(|(k, _)| k.eq_ignore_ascii_case(n)) {
            return Ok(Callable::B(up));
        }
    }
    match ev.eval(e)? {
        EV::L(l) => Ok(Callable::L(l)),
        _ => Err(E::Value),
    }
}

pub(super) fn invoke(ev: &mut Evaluator, f: &Callable, args: Vec<EV>) -> Res {
    match f {
        Callable::L(l) => ev.call_lambda(l, args),
        Callable::B(name) => {
            let k = args.len();
            let names: Vec<String> = (0..k).map(|i| format!("\u{2}{i}")).collect();
            let exprs: Vec<Expr> = names.iter().map(|n| Expr::Name(n.clone())).collect();
            for (n, v) in names.into_iter().zip(args) {
                ev.bind(n, v);
            }
            let r = super::functions::call(ev, name, &exprs);
            ev.unbind(k);
            r
        }
    }
}

/// One cell of a helper function's result: arrays (nested arrays) are #CALC!.
fn one(ev: &mut Evaluator, r: Res) -> Value {
    match r {
        Err(e) => Value::Error(e),
        Ok(EV::V(v)) => v,
        Ok(EV::L(_)) => Value::Error(E::Calc),
        Ok(v) if ev.is_multi(&v) => Value::Error(E::Calc),
        Ok(v) => ev.scalar(v).unwrap_or_else(Value::Error),
    }
}

/// An array argument of a helper function (ranges stay references, so cells keep their type).
fn array_arg(ev: &mut Evaluator, e: &Expr) -> Result<EV, E> {
    match ev.eval(e)? {
        EV::M(_) | EV::L(_) => Err(E::Value),
        v => Ok(v),
    }
}

fn dims(v: &EV) -> (usize, usize) {
    match v {
        EV::R(_, rg) => (rg.rows() as usize, rg.cols() as usize),
        EV::A(g) => (g.len(), g.first().map_or(0, |r| r.len())),
        _ => (1, 1),
    }
}

fn check_size(rows: usize, cols: usize) -> Result<(), E> {
    if rows as u64 * cols as u64 > ARRAY_MAX { Err(E::Calc) } else { Ok(()) }
}

/// Row `i` (or column, with `by_col`) of an array argument.
fn slice(v: &EV, i: usize, by_col: bool) -> EV {
    match v {
        EV::R(s, rg) => {
            let k = i as u32;
            EV::R(*s, if by_col { Range::new(rg.r1, rg.c1 + k, rg.r2, rg.c1 + k) } else { Range::new(rg.r1 + k, rg.c1, rg.r1 + k, rg.c2) })
        }
        EV::A(g) => EV::A(if by_col { g.iter().map(|r| vec![r.get(i).cloned().unwrap_or_default()]).collect() } else { vec![g[i].clone()] }),
        other => other.clone(),
    }
}

pub fn call(ev: &mut Evaluator, name: &str, a: &[Expr]) -> Res {
    let n = a.len();
    match name {
        "LAMBDA" => {
            let Some((body, params)) = a.split_last() else { return Err(E::Value) };
            let mut names: Vec<String> = Vec::with_capacity(params.len());
            for p in params {
                let Expr::Name(p) = p else { return Err(E::Value) };
                if names.iter().any(|q| q.eq_ignore_ascii_case(p)) {
                    return Err(E::Value);
                }
                names.push(p.clone());
            }
            Ok(ev.make_lambda(names, body))
        }
        "ISOMITTED" => {
            if n != 1 {
                return Err(E::Value);
            }
            boolean(matches!(&a[0], Expr::Name(x) if ev.is_omitted(x)))
        }
        "MAP" => {
            if n < 2 {
                return Err(E::Value);
            }
            let f = callable(ev, &a[n - 1])?;
            let mut arrs = Vec::with_capacity(n - 1);
            for e in &a[..n - 1] {
                arrs.push(array_arg(ev, e)?);
            }
            let rows = arrs.iter().map(|v| dims(v).0).max().unwrap_or(1);
            let cols = arrs.iter().map(|v| dims(v).1).max().unwrap_or(1);
            check_size(rows, cols)?;
            let mut out = Vec::with_capacity(rows);
            for r in 0..rows {
                let mut row = Vec::with_capacity(cols);
                for c in 0..cols {
                    let args = arrs.iter().map(|v| element(v, r, c)).collect();
                    let res = invoke(ev, &f, args);
                    row.push(one(ev, res));
                }
                out.push(row);
            }
            Ok(EV::A(out))
        }
        "REDUCE" | "SCAN" => {
            if n != 3 {
                return Err(E::Value);
            }
            let mut acc = match &a[0] {
                Expr::Missing => EV::A(Vec::new()),
                e => ev.eval(e).unwrap_or_else(|k| EV::V(Value::Error(k))),
            };
            let arr = array_arg(ev, &a[1])?;
            let f = callable(ev, &a[2])?;
            let (rows, cols) = dims(&arr);
            let mut out = Vec::with_capacity(if name == "SCAN" { rows } else { 0 });
            for r in 0..rows {
                let mut row = Vec::with_capacity(cols);
                for c in 0..cols {
                    acc = invoke(ev, &f, vec![acc, element(&arr, r, c)]).unwrap_or_else(|k| EV::V(Value::Error(k)));
                    if name == "SCAN" {
                        row.push(one(ev, Ok(acc.clone())));
                    }
                }
                if name == "SCAN" {
                    out.push(row);
                }
            }
            match (name, acc) {
                ("SCAN", _) => Ok(EV::A(out)),
                (_, EV::V(Value::Error(k))) => Err(k),
                (_, acc) if matches!(&acc, EV::A(g) if g.is_empty()) => Ok(EV::V(Value::Empty)),
                (_, acc) => Ok(acc),
            }
        }
        "BYROW" | "BYCOL" => {
            if n != 2 {
                return Err(E::Value);
            }
            let arr = array_arg(ev, &a[0])?;
            let f = callable(ev, &a[1])?;
            let by_col = name == "BYCOL";
            let (rows, cols) = dims(&arr);
            let k = if by_col { cols } else { rows };
            let mut vals = Vec::with_capacity(k);
            for i in 0..k {
                let res = invoke(ev, &f, vec![slice(&arr, i, by_col)]);
                vals.push(one(ev, res));
            }
            Ok(EV::A(if by_col { vec![vals] } else { vals.into_iter().map(|v| vec![v]).collect() }))
        }
        "MAKEARRAY" => {
            if n != 3 {
                return Err(E::Value);
            }
            let rows = ev.number(a, 0)?.trunc();
            let cols = ev.number(a, 1)?.trunc();
            if rows < 1.0 || cols < 1.0 {
                return Err(E::Value);
            }
            if rows * cols > ARRAY_MAX as f64 {
                return Err(E::Calc);
            }
            let f = callable(ev, &a[2])?;
            let mut out = Vec::with_capacity(rows as usize);
            for r in 0..rows as usize {
                let mut row = Vec::with_capacity(cols as usize);
                for c in 0..cols as usize {
                    let res = invoke(ev, &f, vec![EV::V(Value::Number(r as f64 + 1.0)), EV::V(Value::Number(c as f64 + 1.0))]);
                    row.push(one(ev, res));
                }
                out.push(row);
            }
            Ok(EV::A(out))
        }
        "GROUPBY" => groupby(ev, a),
        "PIVOTBY" => pivotby(ev, a),
        _ => named(ev, name, a),
    }
}

/// `f(args)` where `f` is a LET variable or a defined name holding a LAMBDA.
fn named(ev: &mut Evaluator, name: &str, a: &[Expr]) -> Res {
    let known = ev.bound(name).is_some() || ev.wb.names.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
    if !known {
        return Err(E::Name);
    }
    match ev.eval_name(name)? {
        EV::L(l) => {
            let args = ev.lambda_args(a);
            ev.call_lambda(&l, args)
        }
        _ => Err(E::Value),
    }
}

// ---------------------------------------------------------------------------
// GROUPBY / PIVOTBY
// ---------------------------------------------------------------------------

fn tag(v: &Value) -> u8 {
    match v {
        Value::Empty => 0,
        Value::Number(_) => 1,
        Value::Text(_) => 2,
        Value::Bool(_) => 3,
        Value::Error(_) => 4,
    }
}

/// Key order: Excel's sort order, with values of different types never equal.
fn kcmp(a: &[Value], b: &[Value]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        let o = compare(x, y).then(tag(x).cmp(&tag(y)));
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

/// Blank output cell (shown empty, not 0).
fn blank() -> Value {
    Value::Text(String::new())
}

fn opt_int(ev: &mut Evaluator, a: &[Expr], i: usize) -> Result<Option<i64>, E> {
    if ev.has(a, i) { Ok(Some(ev.number(a, i)?.trunc() as i64)) } else { Ok(None) }
}

/// Sort keys (1-based output columns, negative = descending).
fn sort_keys(ev: &mut Evaluator, a: &[Expr], i: usize) -> Result<Vec<i64>, E> {
    if !ev.has(a, i) {
        return Ok(Vec::new());
    }
    let v = ev.arg(a, i)?;
    let mut out = Vec::new();
    for v in ev.grid(v).into_iter().flatten() {
        let k = super::eval::to_num(&v)?.trunc() as i64;
        if k == 0 {
            return Err(E::Value);
        }
        out.push(k);
    }
    Ok(out)
}

/// Field headers: (data has a header row, headers are shown).
fn header_mode(mode: Option<i64>, values: &G) -> Result<(bool, bool), E> {
    match mode {
        Some(0) => Ok((false, false)),
        Some(1) => Ok((true, false)),
        Some(2) => Ok((false, false)),
        Some(3) => Ok((true, true)),
        Some(_) => Err(E::Value),
        None => {
            // Automatic: a text first row above a numeric second row is a header.
            let auto = values.len() > 1
                && values[0].iter().all(|v| matches!(v, Value::Text(_)))
                && values[1].iter().any(|v| matches!(v, Value::Number(_)));
            Ok((auto, auto))
        }
    }
}

/// Aggregates `vals[rows][j]` with the user's function.
fn aggregate(ev: &mut Evaluator, f: &Callable, vals: &G, rows: &[usize], j: usize) -> Value {
    let col: G = rows.iter().map(|&i| vec![vals[i].get(j).cloned().unwrap_or_default()]).collect();
    let res = invoke(ev, f, vec![EV::A(col)]);
    match one(ev, res) {
        Value::Empty => blank(),
        v => v,
    }
}

/// Distinct keys (sorted ascending) with the data rows of each.
fn groups(keys: &G, rows: &[usize]) -> Vec<(Vec<Value>, Vec<usize>)> {
    let mut idx = rows.to_vec();
    idx.sort_by(|&x, &y| kcmp(&keys[x], &keys[y]));
    let mut out: Vec<(Vec<Value>, Vec<usize>)> = Vec::new();
    for i in idx {
        match out.last_mut() {
            Some((k, members)) if kcmp(k, &keys[i]) == Ordering::Equal => members.push(i),
            _ => out.push((keys[i].clone(), vec![i])),
        }
    }
    out
}

/// Common inputs: data rows kept by the filter, and the header rows split off.
struct Fields {
    rows: Vec<usize>,
    head: Option<(Vec<Value>, Vec<Value>, Vec<Value>)>,
}

fn prepare(ev: &mut Evaluator, rf: &mut G, cf: Option<&mut G>, vals: &mut G, hdr: (bool, bool), filter: Option<G>) -> Result<Fields, E> {
    let n = vals.len();
    if rf.len() != n || cf.as_ref().is_some_and(|c| c.len() != n) || n == 0 {
        return Err(E::Value);
    }
    let _ = ev;
    let mut head = None;
    let mut cf = cf;
    if hdr.0 {
        let h_rf = rf.remove(0);
        let h_cf = cf.as_mut().map(|c| c.remove(0)).unwrap_or_default();
        let h_v = vals.remove(0);
        head = Some((h_rf, h_cf, h_v));
    }
    let start = usize::from(hdr.0);
    let mut rows = Vec::with_capacity(vals.len());
    for i in 0..vals.len() {
        if let Some(f) = &filter {
            let keep = f.get(i + start).and_then(|r| r.first());
            match keep {
                Some(Value::Error(e)) => return Err(*e),
                Some(v) if super::eval::to_bool(v).unwrap_or(false) => {}
                _ => continue,
            }
        }
        rows.push(i);
    }
    if !hdr.1 {
        head = None;
    }
    Ok(Fields { rows, head })
}

fn grid_arg(ev: &mut Evaluator, a: &[Expr], i: usize) -> Result<G, E> {
    let v = array_arg(ev, &a[i])?;
    Ok(ev.grid(v))
}

fn filter_arg(ev: &mut Evaluator, a: &[Expr], i: usize) -> Result<Option<G>, E> {
    if ev.has(a, i) { Ok(Some(grid_arg(ev, a, i)?)) } else { Ok(None) }
}

/// Orders leaf rows by sort keys over (key columns, value columns).
fn sort_rows(rows: &mut [(Vec<Value>, Vec<Value>)], keys: &[i64]) {
    if keys.is_empty() {
        return;
    }
    rows.sort_by(|x, y| {
        for &k in keys {
            let i = k.unsigned_abs() as usize - 1;
            let pick = |r: &(Vec<Value>, Vec<Value>)| -> Value {
                if i < r.0.len() { r.0[i].clone() } else { r.1.get(i - r.0.len()).cloned().unwrap_or_default() }
            };
            let o = kcmp(&[pick(x)], &[pick(y)]);
            let o = if k < 0 { o.reverse() } else { o };
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    });
}

fn groupby(ev: &mut Evaluator, a: &[Expr]) -> Res {
    if a.len() < 3 || a.len() > 8 {
        return Err(E::Value);
    }
    let mut rf = grid_arg(ev, a, 0)?;
    let mut vals = grid_arg(ev, a, 1)?;
    let f = callable(ev, &a[2])?;
    let hdr = header_mode(opt_int(ev, a, 3)?, &vals)?;
    let depth = opt_int(ev, a, 4)?.unwrap_or(1);
    let sort = sort_keys(ev, a, 5)?;
    let filter = filter_arg(ev, a, 6)?;
    let fx = prepare(ev, &mut rf, None, &mut vals, hdr, filter)?;
    let (k, m) = (rf.first().map_or(0, |r| r.len()), vals.first().map_or(0, |r| r.len()));
    if sort.iter().any(|s| s.unsigned_abs() as usize > k + m) || depth.abs() > k as i64 + 1 {
        return Err(E::Value);
    }
    let agg_rows = |ev: &mut Evaluator, rows: &[usize]| -> Vec<Value> { (0..m).map(|j| aggregate(ev, &f, &vals, rows, j)).collect() };
    // Leaves, then subtotals for key prefixes (levels 1..|depth|-1) and the grand total.
    let mut leaves: Vec<(Vec<Value>, Vec<Value>, Vec<usize>)> = Vec::new();
    for (key, rows) in groups(&rf, &fx.rows) {
        let v = agg_rows(ev, &rows);
        leaves.push((key, v, rows));
    }
    let mut order: Vec<(Vec<Value>, Vec<Value>)> = leaves.iter().map(|(k, v, _)| (k.clone(), v.clone())).collect();
    sort_rows(&mut order, &sort);
    let sub_levels = (depth.unsigned_abs() as usize).saturating_sub(1).min(k.saturating_sub(1));
    let mut out: G = Vec::new();
    if let Some((h_rf, _, h_v)) = &fx.head {
        out.push(h_rf.iter().chain(h_v.iter()).cloned().collect());
    }
    let top = depth < 0;
    let total = |ev: &mut Evaluator| -> Vec<Value> {
        let mut row = vec![Value::Text("Total".into())];
        row.resize(k, blank());
        row.extend(agg_rows(ev, &fx.rows));
        row
    };
    if top && depth != 0 {
        out.push(total(ev));
    }
    if sub_levels == 0 {
        out.extend(order.into_iter().map(|(key, v)| key.into_iter().chain(v).collect()));
    } else {
        // Keep level-1 groups together, in the order their first member sorts.
        let mut firsts: Vec<Vec<Value>> = Vec::new();
        for (key, _) in &order {
            if !firsts.iter().any(|p| kcmp(p, &key[..1]) == Ordering::Equal) {
                firsts.push(key[..1].to_vec());
            }
        }
        for p in firsts {
            let members: Vec<&(Vec<Value>, Vec<Value>)> = order.iter().filter(|(key, _)| kcmp(&key[..1], &p) == Ordering::Equal).collect();
            let rows: Vec<usize> = leaves.iter().filter(|(key, _, _)| kcmp(&key[..1], &p) == Ordering::Equal).flat_map(|(_, _, r)| r.iter().copied()).collect();
            let mut sub = p.clone();
            sub.resize(k, blank());
            sub.extend(agg_rows(ev, &rows));
            if top {
                out.push(sub.clone());
            }
            out.extend(members.into_iter().map(|(key, v)| key.iter().chain(v.iter()).cloned().collect()));
            if !top {
                out.push(sub);
            }
        }
    }
    if !top && depth != 0 {
        out.push(total(ev));
    }
    if out.is_empty() { Err(E::Calc) } else { Ok(EV::A(out)) }
}

fn pivotby(ev: &mut Evaluator, a: &[Expr]) -> Res {
    if a.len() < 4 || a.len() > 11 {
        return Err(E::Value);
    }
    let mut rf = grid_arg(ev, a, 0)?;
    let mut cf = grid_arg(ev, a, 1)?;
    let mut vals = grid_arg(ev, a, 2)?;
    let f = callable(ev, &a[3])?;
    let hdr = header_mode(opt_int(ev, a, 4)?, &vals)?;
    let row_depth = opt_int(ev, a, 5)?.unwrap_or(1);
    let row_sort = sort_keys(ev, a, 6)?;
    let col_depth = opt_int(ev, a, 7)?.unwrap_or(1);
    let col_sort = sort_keys(ev, a, 8)?;
    let filter = filter_arg(ev, a, 9)?;
    let fx = prepare(ev, &mut rf, Some(&mut cf), &mut vals, hdr, filter)?;
    let kr = rf.first().map_or(0, |r| r.len());
    let kc = cf.first().map_or(0, |r| r.len());
    let m = vals.first().map_or(0, |r| r.len()).max(1);
    let mut rkeys = groups(&rf, &fx.rows);
    let mut ckeys = groups(&cf, &fx.rows);
    // Sorting by a key column (descending when negative).
    let by_key = |keys: &mut Vec<(Vec<Value>, Vec<usize>)>, sort: &[i64]| {
        if let Some(&s) = sort.first() {
            let i = (s.unsigned_abs() as usize - 1).min(keys.first().map_or(0, |k| k.0.len().saturating_sub(1)));
            keys.sort_by(|x, y| {
                let o = kcmp(&x.0[i..i + 1], &y.0[i..i + 1]);
                if s < 0 { o.reverse() } else { o }
            });
        }
    };
    by_key(&mut rkeys, &row_sort);
    by_key(&mut ckeys, &col_sort);
    let width = kr + ckeys.len() * m + if col_depth != 0 { m } else { 0 };
    check_size(rkeys.len() + kc + 2, width)?;
    let mut out: G = Vec::new();
    for level in 0..kc {
        let mut row = vec![blank(); kr];
        if level + 1 == kc {
            if let Some((h_rf, _, _)) = &fx.head {
                row = h_rf.clone();
            }
        }
        for (key, _) in &ckeys {
            for _ in 0..m {
                row.push(key[level].clone());
            }
        }
        if col_depth != 0 {
            for _ in 0..m {
                row.push(if level == 0 { Value::Text("Total".into()) } else { blank() });
            }
        }
        out.push(row);
    }
    if m > 1 || kc == 0 {
        if let Some((h_rf, _, h_v)) = &fx.head {
            let mut row = if kc == 0 { h_rf.clone() } else { vec![blank(); kr] };
            for _ in 0..ckeys.len() + usize::from(col_depth != 0) {
                row.extend(h_v.iter().cloned());
            }
            out.push(row);
        }
    }
    let line = |ev: &mut Evaluator, label: Vec<Value>, rows: &[usize]| -> Vec<Value> {
        let mut row = label;
        for (_, crow) in &ckeys {
            let both: Vec<usize> = rows.iter().copied().filter(|r| crow.contains(r)).collect();
            for j in 0..m {
                row.push(if both.is_empty() { blank() } else { aggregate(ev, &f, &vals, &both, j) });
            }
        }
        if col_depth != 0 {
            for j in 0..m {
                row.push(aggregate(ev, &f, &vals, rows, j));
            }
        }
        row
    };
    let mut body = Vec::with_capacity(rkeys.len());
    for (key, rows) in &rkeys {
        body.push(line(ev, key.clone(), rows));
    }
    let total = if row_depth != 0 {
        let mut label = vec![Value::Text("Total".into())];
        label.resize(kr, blank());
        Some(line(ev, label, &fx.rows))
    } else {
        None
    };
    if row_depth < 0 {
        out.extend(total.clone());
    }
    out.extend(body);
    if row_depth > 0 {
        out.extend(total);
    }
    if out.is_empty() { Err(E::Calc) } else { Ok(EV::A(out)) }
}
