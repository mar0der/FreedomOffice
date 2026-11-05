//! Additional worksheet functions: dynamic arrays, information, dates, engineering,
//! statistics and finance. Semantics follow Excel's documentation (defaults, error values).

use std::cmp::Ordering;
use std::f64::consts::PI;

use super::eval::{boolean, compare, is_subtotal_cell, num, text, to_num, to_text, Evaluator, Res, ARRAY_MAX, EV};
use super::functions::{aligned_grids, holiday_set, ipmt, need, percentile, pmt, round_half_away, serial, stats, transpose, WORKDAYS_MAX};
use super::parser::{quote_sheet, Expr};
use crate::model::{col_name, ErrorKind as E, Value, MAX_COLS, MAX_ROWS};
use crate::numfmt;

type G = Vec<Vec<Value>>;

/// Most periods the depreciation / cumulative-payment functions step through (#NUM! beyond;
/// a float loop counter past 2^53 would also never advance).
const PERIODS_MAX: f64 = 1_000_000.0;

fn arr(g: G) -> Res {
    Ok(EV::A(g))
}

fn grid_arg(ev: &mut Evaluator, a: &[Expr], i: usize) -> Result<G, E> {
    let v = ev.arg(a, i)?;
    Ok(ev.grid(v))
}

fn dims(g: &G) -> (usize, usize) {
    (g.len(), g.iter().map(|r| r.len()).max().unwrap_or(0))
}

fn flat(g: G) -> Vec<Value> {
    g.into_iter().flatten().collect()
}

fn int_arg(ev: &mut Evaluator, a: &[Expr], i: usize) -> Result<i64, E> {
    Ok(ev.number(a, i)?.trunc() as i64)
}

/// Numbers of one argument (SUM rules).
fn nums(ev: &mut Evaluator, a: &[Expr], i: usize) -> Result<Vec<f64>, E> {
    if i >= a.len() {
        return Err(E::Value);
    }
    ev.numbers(&a[i..i + 1], false)
}

/// A value argument where errors are kept as values (pad_with arguments).
fn value_or_error(ev: &mut Evaluator, a: &[Expr], i: usize, default: Value) -> Value {
    if !ev.has(a, i) {
        return default;
    }
    match ev.value(a, i) {
        Ok(v) => v,
        Err(e) => Value::Error(e),
    }
}

/// Numeric pairs of two equally sized arrays (both members numeric); errors propagate.
fn paired(ev: &mut Evaluator, a: &[Expr], i: usize, j: usize) -> Result<Vec<(f64, f64)>, E> {
    need(a, j + 1, usize::MAX)?;
    // Pairs with a blank member are skipped, so same-size ranges need only their data block.
    let (xv, yv) = (ev.arg(a, i)?, ev.arg(a, j)?);
    let mut gs = aligned_grids(ev, vec![xv, yv]).into_iter().map(flat);
    let (x, y) = (gs.next().unwrap_or_default(), gs.next().unwrap_or_default());
    if x.len() != y.len() {
        return Err(E::NA);
    }
    let mut out = Vec::with_capacity(x.len());
    for (p, q) in x.iter().zip(&y) {
        match (p, q) {
            (Value::Error(e), _) | (_, Value::Error(e)) => return Err(*e),
            (Value::Number(u), Value::Number(v)) => out.push((*u, *v)),
            _ => {}
        }
    }
    Ok(out)
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

/// (slope, intercept, Sxx, Sxy, Syy, mean x, mean y) of y on x.
fn linfit(p: &[(f64, f64)]) -> Result<(f64, f64, f64, f64, f64, f64, f64), E> {
    if p.is_empty() {
        return Err(E::Div0);
    }
    let n = p.len() as f64;
    let mx = p.iter().map(|q| q.0).sum::<f64>() / n;
    let my = p.iter().map(|q| q.1).sum::<f64>() / n;
    let sxx: f64 = p.iter().map(|q| (q.0 - mx).powi(2)).sum();
    let sxy: f64 = p.iter().map(|q| (q.0 - mx) * (q.1 - my)).sum();
    let syy: f64 = p.iter().map(|q| (q.1 - my).powi(2)).sum();
    if sxx == 0.0 {
        return Err(E::Div0);
    }
    let m = sxy / sxx;
    Ok((m, my - m * mx, sxx, sxy, syy, mx, my))
}

pub fn call(ev: &mut Evaluator, name: &str, a: &[Expr]) -> Res {
    let n = a.len();
    match name {
        // ----- dynamic arrays -----
        "LET" => {
            if n < 3 || n % 2 == 0 {
                return Err(E::Value);
            }
            let mut bound = 0;
            for i in (0..n - 1).step_by(2) {
                let Expr::Name(var) = &a[i] else {
                    ev.unbind(bound);
                    return Err(E::Name);
                };
                let v = match ev.eval(&a[i + 1]) {
                    Ok(v) => v,
                    Err(e) => EV::V(Value::Error(e)),
                };
                ev.bind(var.clone(), v);
                bound += 1;
            }
            let r = ev.eval(&a[n - 1]);
            ev.unbind(bound);
            r
        }
        "TEXTSPLIT" => textsplit(ev, a),
        "VSTACK" | "HSTACK" => {
            if n == 0 {
                return Err(E::Value);
            }
            let mut gs = Vec::with_capacity(n);
            for i in 0..n {
                let g = grid_arg(ev, a, i)?;
                gs.push(if name == "HSTACK" { transpose(g) } else { g });
            }
            let w = gs.iter().map(|g| dims(g).1).max().unwrap_or(0);
            // Padding to the widest piece multiplies sizes: refuse arrays too large to hold.
            let h: usize = gs.iter().map(|g| g.len()).sum();
            if h as u64 * w as u64 > ARRAY_MAX {
                return Err(E::Calc);
            }
            let mut out: G = Vec::new();
            for g in gs {
                for mut row in g {
                    row.resize(w, Value::Error(E::NA));
                    out.push(row);
                }
            }
            arr(if name == "HSTACK" { transpose(out) } else { out })
        }
        "TAKE" | "DROP" => {
            need(a, 2, 3)?;
            let g = grid_arg(ev, a, 0)?;
            let (h, w) = dims(&g);
            let r = if ev.has(a, 1) { Some(int_arg(ev, a, 1)?) } else { None };
            let c = if ev.has(a, 2) { Some(int_arg(ev, a, 2)?) } else { None };
            let take = name == "TAKE";
            let rows = span(h, r, take);
            let cols = span(w, c, take);
            if rows.is_empty() || cols.is_empty() {
                return Err(E::Calc);
            }
            arr(g[rows].iter().map(|row| row[cols.clone()].to_vec()).collect())
        }
        "CHOOSECOLS" | "CHOOSEROWS" => {
            if n < 2 {
                return Err(E::Value);
            }
            let g = grid_arg(ev, a, 0)?;
            let (h, w) = dims(&g);
            let rows = name == "CHOOSEROWS";
            let len = if rows { h } else { w } as i64;
            let mut idx = Vec::new();
            for i in 1..n {
                for v in flat(grid_arg(ev, a, i)?) {
                    let k = to_num(&v)?.trunc() as i64;
                    let j = if k > 0 { k - 1 } else { len + k };
                    if k == 0 || j < 0 || j >= len {
                        return Err(E::Value);
                    }
                    idx.push(j as usize);
                }
            }
            if idx.len() as u64 * if rows { w } else { h } as u64 > ARRAY_MAX {
                return Err(E::Calc);
            }
            arr(if rows { idx.iter().map(|&j| g[j].clone()).collect() } else { g.iter().map(|row| idx.iter().map(|&j| row[j].clone()).collect()).collect() })
        }
        "SORTBY" => sortby(ev, a),
        "TOCOL" | "TOROW" => {
            need(a, 1, 3)?;
            let g = grid_arg(ev, a, 0)?;
            let ignore = if ev.has(a, 1) { int_arg(ev, a, 1)? } else { 0 };
            if !(0..=3).contains(&ignore) {
                return Err(E::Value);
            }
            let by_col = ev.bool_or(a, 2, false)?;
            let items: Vec<Value> = flat(if by_col { transpose(g) } else { g })
                .into_iter()
                .filter(|v| !((ignore & 1 != 0 && v.is_empty()) || (ignore & 2 != 0 && matches!(v, Value::Error(_)))))
                .collect();
            if items.is_empty() {
                return Err(E::Calc);
            }
            arr(if name == "TOCOL" { items.into_iter().map(|v| vec![v]).collect() } else { vec![items] })
        }
        "WRAPROWS" | "WRAPCOLS" => {
            need(a, 2, 3)?;
            let g = grid_arg(ev, a, 0)?;
            let (h, w) = dims(&g);
            if h > 1 && w > 1 {
                return Err(E::Value);
            }
            let k = int_arg(ev, a, 1)?;
            // Rows of WRAPROWS (columns of WRAPCOLS) are `k` long: at most a sheet's width
            // (height). The result then has at most len + k cells.
            let most = if name == "WRAPROWS" { MAX_COLS } else { MAX_ROWS } as i64;
            if k < 1 || k > most {
                return Err(E::Num);
            }
            let pad = value_or_error(ev, a, 2, Value::Error(E::NA));
            let out: G = flat(g)
                .chunks(k as usize)
                .map(|c| {
                    let mut row = c.to_vec();
                    row.resize(k as usize, pad.clone());
                    row
                })
                .collect();
            arr(if name == "WRAPCOLS" { transpose(out) } else { out })
        }
        "EXPAND" => {
            need(a, 2, 4)?;
            let g = grid_arg(ev, a, 0)?;
            let (h, w) = dims(&g);
            let rows = if ev.has(a, 1) { int_arg(ev, a, 1)? } else { h as i64 };
            let cols = if ev.has(a, 2) { int_arg(ev, a, 2)? } else { w as i64 };
            if rows < h as i64 || cols < w as i64 {
                return Err(E::Value);
            }
            // Bound each side before multiplying (huge counts must not wrap).
            if rows > MAX_ROWS as i64 || cols > MAX_COLS as i64 || rows as u64 * cols as u64 > 1_048_576 {
                return Err(E::Num);
            }
            let pad = value_or_error(ev, a, 3, Value::Error(E::NA));
            let mut out = g;
            for row in out.iter_mut() {
                row.resize(cols as usize, pad.clone());
            }
            out.resize(rows as usize, vec![pad; cols as usize]);
            arr(out)
        }
        "RANDARRAY" => {
            need(a, 0, 5)?;
            let rows = if ev.has(a, 0) { int_arg(ev, a, 0)? } else { 1 };
            let cols = if ev.has(a, 1) { int_arg(ev, a, 1)? } else { 1 };
            let lo = ev.number_or(a, 2, 0.0)?;
            let hi = ev.number_or(a, 3, 1.0)?;
            let whole = ev.bool_or(a, 4, false)?;
            if rows < 1 || cols < 1 || lo > hi || (whole && (lo.fract() != 0.0 || hi.fract() != 0.0)) {
                return Err(E::Value);
            }
            // Bound each side before multiplying (huge counts must not wrap).
            if rows > MAX_ROWS as i64 || cols > MAX_COLS as i64 || rows as u64 * cols as u64 > 1_048_576 {
                return Err(E::Calc);
            }
            let mut out = Vec::with_capacity(rows as usize);
            for _ in 0..rows {
                let mut row = Vec::with_capacity(cols as usize);
                for _ in 0..cols {
                    let r = ev.random();
                    row.push(Value::Number(if whole { lo + (r * (hi - lo + 1.0)).floor() } else { lo + r * (hi - lo) }));
                }
                out.push(row);
            }
            arr(out)
        }

        // ----- information & reference -----
        "AGGREGATE" => aggregate(ev, a),
        "ADDRESS" => {
            need(a, 2, 5)?;
            let r = ev.number(a, 0)?.trunc();
            let c = ev.number(a, 1)?.trunc();
            let abs = if ev.has(a, 2) { int_arg(ev, a, 2)? } else { 1 };
            let a1 = ev.bool_or(a, 3, true)?;
            if r < 1.0 || c < 1.0 || r > MAX_ROWS as f64 || c > MAX_COLS as f64 || !(1..=4).contains(&abs) {
                return Err(E::Value);
            }
            let (ra, ca) = (abs <= 2, abs % 2 == 1);
            let (r, c) = (r as u32, c as u32);
            let body = if a1 {
                format!("{}{}{}{}", if ca { "$" } else { "" }, col_name(c - 1), if ra { "$" } else { "" }, r)
            } else {
                let part = |abs: bool, v: u32| if abs { v.to_string() } else { format!("[{v}]") };
                format!("R{}C{}", part(ra, r), part(ca, c))
            };
            let sheet = if ev.has(a, 4) { ev.string(a, 4)? } else { String::new() };
            text(if sheet.is_empty() { body } else { format!("{}!{body}", quote_sheet(&sheet)) })
        }
        "HYPERLINK" => {
            need(a, 1, 2)?;
            if ev.has(a, 1) {
                ev.value(a, 1).map(EV::V)
            } else {
                text(ev.string(a, 0)?)
            }
        }
        "ISREF" => {
            need(a, 1, 1)?;
            boolean(matches!(ev.eval(&a[0]), Ok(EV::R(..))))
        }
        "SHEET" => {
            need(a, 0, 1)?;
            if !ev.has(a, 0) {
                return num(ev.cur.0 as f64 + 1.0);
            }
            match ev.eval(&a[0])? {
                EV::R(s, _) => num(s as f64 + 1.0),
                v => {
                    let t = to_text(&ev.scalar(v)?)?;
                    ev.wb.sheet_index(&t).map(|i| EV::V(Value::Number(i as f64 + 1.0))).ok_or(E::NA)
                }
            }
        }
        "SHEETS" => {
            need(a, 0, 1)?;
            if !ev.has(a, 0) {
                return num(ev.wb.sheets.len() as f64);
            }
            match ev.eval(&a[0])? {
                EV::R(..) => num(1.0),
                _ => Err(E::Value),
            }
        }
        "FORMULATEXT" => {
            need(a, 1, 1)?;
            match ev.eval(&a[0])? {
                EV::R(s, rg) => match ev.wb.sheets[s].get(rg.r1, rg.c1).and_then(|c| c.formula.as_ref()) {
                    Some(f) => text(format!("={}", f.text)),
                    None => Err(E::NA),
                },
                _ => Err(E::NA),
            }
        }
        "NUMBERVALUE" => {
            need(a, 1, 3)?;
            let t = ev.string(a, 0)?;
            let dec = if ev.has(a, 1) { ev.string(a, 1)?.chars().next().ok_or(E::Value)? } else { '.' };
            let grp = if ev.has(a, 2) { ev.string(a, 2)?.chars().next().ok_or(E::Value)? } else { ',' };
            num(numbervalue(&t, dec, grp)?)
        }
        "DOLLAR" => {
            need(a, 1, 2)?;
            let x = ev.number(a, 0)?;
            let d = ev.number_or(a, 1, 2.0)?.trunc();
            if d > 127.0 {
                return Err(E::Value);
            }
            let r = round_half_away(x, d as i32);
            let body = grouped(r.abs(), d.max(0.0) as usize);
            text(if r < 0.0 { format!("(${body})") } else { format!("${body}") })
        }

        // ----- date & time -----
        "YEARFRAC" => {
            need(a, 2, 3)?;
            let s = serial(ev.number(a, 0)?)?;
            let e = serial(ev.number(a, 1)?)?;
            let b = if ev.has(a, 2) { int_arg(ev, a, 2)? } else { 0 };
            num(yearfrac(s, e, b)?)
        }
        "DAYS360" => {
            need(a, 2, 3)?;
            let s = serial(ev.number(a, 0)?)?.floor();
            let e = serial(ev.number(a, 1)?)?.floor();
            let eu = ev.bool_or(a, 2, false)?;
            let (y1, m1, d1) = numfmt::serial_to_ymd(s);
            let (y2, m2, d2) = numfmt::serial_to_ymd(e);
            let (d1, d2) = if eu {
                (d1.min(30), d2.min(30))
            } else {
                let d1 = if d1 == numfmt::days_in_month(y1, m1) { 30 } else { d1 };
                (d1, if d2 == 31 && d1 >= 30 { 30 } else { d2 })
            };
            num(((y2 - y1) * 360 + (m2 - m1) * 30 + (d2 - d1)) as f64)
        }
        "ISOWEEKNUM" => {
            need(a, 1, 1)?;
            let d = serial(ev.number(a, 0)?)?.floor();
            let iso = (numfmt::weekday(d) + 5) % 7 + 1; // Mon=1..Sun=7
            let thursday = d - iso as f64 + 4.0;
            let (y, _, _) = numfmt::serial_to_ymd(thursday);
            num(((thursday - numfmt::ymd_to_serial(y, 1, 1)) / 7.0).floor() + 1.0)
        }
        "NETWORKDAYS.INTL" | "WORKDAY.INTL" => {
            need(a, 2, 4)?;
            let s = serial(ev.number(a, 0)?)?.floor();
            let wk = if ev.has(a, 2) { Some(ev.value(a, 2)?) } else { None };
            let mask = weekend_mask(wk)?;
            let mut holidays = Vec::new();
            if ev.has(a, 3) {
                for v in flat(grid_arg(ev, a, 3)?) {
                    match v {
                        Value::Empty => {}
                        v => holidays.push(to_num(&v)?),
                    }
                }
            }
            let holidays = holiday_set(holidays);
            let off = |d: f64| mask[((numfmt::weekday(d) + 5) % 7) as usize] || holidays.contains(&(d as i64));
            if name == "NETWORKDAYS.INTL" {
                let e = serial(ev.number(a, 1)?)?.floor();
                let (lo, hi, sign) = if s <= e { (s, e, 1.0) } else { (e, s, -1.0) };
                if hi - lo > WORKDAYS_MAX {
                    return Err(E::Num);
                }
                let mut c = 0.0;
                let mut d = lo;
                while d <= hi {
                    if !off(d) {
                        c += 1.0;
                    }
                    d += 1.0;
                }
                num(c * sign)
            } else {
                // Bounded walk: at most WORKDAYS_MAX working days, stopping if it leaves the
                // date range (the weekend mask always leaves a working day).
                let days = ev.number(a, 1)?.trunc();
                if days.abs() > WORKDAYS_MAX {
                    return Err(E::Num);
                }
                let step = if days >= 0.0 { 1.0 } else { -1.0 };
                let mut left = days.abs();
                let mut d = s;
                while left > 0.0 {
                    d += step;
                    serial(d)?;
                    if !off(d) {
                        left -= 1.0;
                    }
                }
                num(d)
            }
        }

        // ----- engineering -----
        "CONVERT" => {
            need(a, 3, 3)?;
            let x = ev.number(a, 0)?;
            let from = ev.string(a, 1)?;
            let to = ev.string(a, 2)?;
            num(convert(x, &from, &to)?)
        }
        "BIN2DEC" | "OCT2DEC" | "HEX2DEC" => {
            need(a, 1, 1)?;
            let radix = match name {
                "BIN2DEC" => 2,
                "OCT2DEC" => 8,
                _ => 16,
            };
            num(from_radix10(&ev.string(a, 0)?, radix)?)
        }
        "DEC2BIN" | "DEC2OCT" | "DEC2HEX" => {
            need(a, 1, 2)?;
            let (radix, lim) = match name {
                "DEC2BIN" => (2u32, 512.0),
                "DEC2OCT" => (8, 536_870_912.0),
                _ => (16, 549_755_813_888.0),
            };
            let x = ev.number(a, 0)?.trunc();
            if x < -lim || x >= lim {
                return Err(E::Num);
            }
            let full = (radix as u64).pow(10);
            let s = if x < 0.0 { fmt_radix(full - (-x) as u64, radix) } else { fmt_radix(x as u64, radix) };
            if x >= 0.0 && ev.has(a, 1) {
                let p = ev.number(a, 1)?.trunc();
                if p < 0.0 || p > 10.0 || (p as usize) < s.len() {
                    return Err(E::Num);
                }
                return text(format!("{s:0>width$}", width = p as usize));
            }
            text(s)
        }
        "BASE" => {
            need(a, 2, 3)?;
            let x = ev.number(a, 0)?.trunc();
            let radix = ev.number(a, 1)?.trunc();
            let min = ev.number_or(a, 2, 0.0)?.trunc();
            if !(0.0..9007199254740992.0).contains(&x) || !(2.0..=36.0).contains(&radix) || !(0.0..=255.0).contains(&min) {
                return Err(E::Num);
            }
            text(format!("{:0>width$}", fmt_radix(x as u64, radix as u32), width = min as usize))
        }
        "DECIMAL" => {
            need(a, 2, 2)?;
            let t = ev.string(a, 0)?;
            let radix = ev.number(a, 1)?.trunc();
            if !(2.0..=36.0).contains(&radix) || t.len() > 255 {
                return Err(E::Num);
            }
            let mut v = 0.0;
            for ch in t.trim().chars() {
                let d = ch.to_digit(36).filter(|d| (*d as f64) < radix).ok_or(E::Num)?;
                v = v * radix + d as f64;
            }
            num(v)
        }
        "ROMAN" => {
            need(a, 1, 2)?;
            let x = ev.number(a, 0)?.trunc();
            let form = match ev.arg(a, 1)? {
                EV::V(Value::Bool(b)) => if b { 0.0 } else { 4.0 },
                _ => ev.number_or(a, 1, 0.0)?.trunc(),
            };
            if !(0.0..=3999.0).contains(&x) || !(0.0..=4.0).contains(&form) {
                return Err(E::Value);
            }
            text(roman(x as u32, form as u32))
        }
        "ARABIC" => {
            need(a, 1, 1)?;
            num(arabic(&ev.string(a, 0)?)?)
        }

        // ----- math -----
        "COMBIN" | "COMBINA" | "PERMUT" => {
            need(a, 2, 2)?;
            let x = ev.number(a, 0)?.trunc();
            let k = ev.number(a, 1)?.trunc();
            match name {
                "COMBIN" => num(combin(x, k)?),
                "COMBINA" => {
                    if x < 0.0 || k < 0.0 {
                        return Err(E::Num);
                    }
                    if k == 0.0 { num(1.0) } else { num(combin(x + k - 1.0, k)?) }
                }
                _ => {
                    if x < 0.0 || k < 0.0 || x < k {
                        return Err(E::Num);
                    }
                    // Every factor is >= 1, so the product overflows within ~1000 steps for
                    // any huge k: stop there instead of looping k times.
                    let mut acc = 1.0;
                    for i in 0..k as u64 {
                        acc *= x - i as f64;
                        if !acc.is_finite() {
                            return Err(E::Num);
                        }
                    }
                    num(acc)
                }
            }
        }
        "MULTINOMIAL" => {
            let v = ev.numbers(a, false)?;
            let (mut total, mut out) = (0.0, 1.0);
            for x in v {
                let x = x.trunc();
                if x < 0.0 {
                    return Err(E::Num);
                }
                total += x;
                out *= combin(total, x)?;
            }
            num(out)
        }
        "SUMX2MY2" | "SUMX2PY2" | "SUMXMY2" => {
            let p = paired(ev, a, 0, 1)?;
            num(p.iter().map(|(x, y)| match name {
                "SUMX2MY2" => x * x - y * y,
                "SUMX2PY2" => x * x + y * y,
                _ => (x - y).powi(2),
            }).sum())
        }

        // ----- statistics -----
        "GEOMEAN" | "HARMEAN" => {
            let v = ev.numbers(a, false)?;
            if v.is_empty() || v.iter().any(|x| *x <= 0.0) {
                return Err(E::Num);
            }
            if name == "GEOMEAN" { num(mean(&v.iter().map(|x| x.ln()).collect::<Vec<_>>()).exp()) } else { num(v.len() as f64 / v.iter().map(|x| 1.0 / x).sum::<f64>()) }
        }
        "AVEDEV" | "DEVSQ" => {
            let v = ev.numbers(a, false)?;
            if v.is_empty() {
                return Err(E::Num);
            }
            let m = mean(&v);
            if name == "AVEDEV" { num(v.iter().map(|x| (x - m).abs()).sum::<f64>() / v.len() as f64) } else { num(v.iter().map(|x| (x - m).powi(2)).sum()) }
        }
        "KURT" | "SKEW" | "SKEW.P" => {
            let v = ev.numbers(a, false)?;
            let nn = v.len() as f64;
            let m = mean(&v);
            if name == "SKEW.P" {
                if v.is_empty() {
                    return Err(E::Div0);
                }
                let sd = stats(&v, false)?.sqrt();
                if sd == 0.0 {
                    return Err(E::Div0);
                }
                return num(v.iter().map(|x| ((x - m) / sd).powi(3)).sum::<f64>() / nn);
            }
            if nn < if name == "KURT" { 4.0 } else { 3.0 } {
                return Err(E::Div0);
            }
            let sd = stats(&v, true)?.sqrt();
            if sd == 0.0 {
                return Err(E::Div0);
            }
            if name == "KURT" {
                let s4: f64 = v.iter().map(|x| ((x - m) / sd).powi(4)).sum();
                num(nn * (nn + 1.0) / ((nn - 1.0) * (nn - 2.0) * (nn - 3.0)) * s4 - 3.0 * (nn - 1.0).powi(2) / ((nn - 2.0) * (nn - 3.0)))
            } else {
                let s3: f64 = v.iter().map(|x| ((x - m) / sd).powi(3)).sum();
                num(nn / ((nn - 1.0) * (nn - 2.0)) * s3)
            }
        }
        "STANDARDIZE" => {
            need(a, 3, 3)?;
            let x = ev.number(a, 0)?;
            let m = ev.number(a, 1)?;
            let sd = ev.number(a, 2)?;
            if sd <= 0.0 { Err(E::Num) } else { num((x - m) / sd) }
        }
        "NORM.DIST" | "NORMDIST" => {
            need(a, 4, 4)?;
            let x = ev.number(a, 0)?;
            let m = ev.number(a, 1)?;
            let sd = ev.number(a, 2)?;
            let cum = ev.boolean(a, 3)?;
            if sd <= 0.0 {
                return Err(E::Num);
            }
            let z = (x - m) / sd;
            num(if cum { norm_cdf(z) } else { norm_pdf(z) / sd })
        }
        "NORM.S.DIST" | "NORMSDIST" => {
            let z = ev.number(a, 0)?;
            let cum = if name == "NORMSDIST" { need(a, 1, 1)?; true } else { need(a, 2, 2)?; ev.boolean(a, 1)? };
            num(if cum { norm_cdf(z) } else { norm_pdf(z) })
        }
        "NORM.INV" | "NORMINV" => {
            need(a, 3, 3)?;
            let p = ev.number(a, 0)?;
            let m = ev.number(a, 1)?;
            let sd = ev.number(a, 2)?;
            if p <= 0.0 || p >= 1.0 || sd <= 0.0 {
                return Err(E::Num);
            }
            num(m + sd * norm_s_inv(p))
        }
        "NORM.S.INV" | "NORMSINV" => {
            need(a, 1, 1)?;
            let p = ev.number(a, 0)?;
            if p <= 0.0 || p >= 1.0 { Err(E::Num) } else { num(norm_s_inv(p)) }
        }
        "T.DIST" | "T.DIST.2T" | "T.DIST.RT" | "TDIST" => {
            let x = ev.number(a, 0)?;
            let df = ev.number(a, 1)?.trunc();
            if df < 1.0 {
                return Err(E::Num);
            }
            match name {
                "T.DIST" => {
                    need(a, 3, 3)?;
                    num(if ev.boolean(a, 2)? { t_cdf(x, df) } else { t_pdf(x, df) })
                }
                "T.DIST.RT" => num(1.0 - t_cdf(x, df)),
                _ => {
                    let tails = if name == "TDIST" { ev.number(a, 2)?.trunc() } else { 2.0 };
                    if x < 0.0 || !(tails == 1.0 || tails == 2.0) {
                        return Err(E::Num);
                    }
                    num(tails * (1.0 - t_cdf(x, df)))
                }
            }
        }
        "T.INV" | "T.INV.2T" | "TINV" => {
            need(a, 2, 2)?;
            let p = ev.number(a, 0)?;
            let df = ev.number(a, 1)?.trunc();
            if p <= 0.0 || p > 1.0 || df < 1.0 || (name == "T.INV" && p >= 1.0) {
                return Err(E::Num);
            }
            if name == "T.INV" { num(t_inv(p, df)) } else { num(t_inv(1.0 - p / 2.0, df).abs()) }
        }
        "CONFIDENCE.NORM" | "CONFIDENCE" | "CONFIDENCE.T" => {
            need(a, 3, 3)?;
            let alpha = ev.number(a, 0)?;
            let sd = ev.number(a, 1)?;
            let size = ev.number(a, 2)?.trunc();
            if alpha <= 0.0 || alpha >= 1.0 || sd <= 0.0 || size < 1.0 {
                return Err(E::Num);
            }
            if name == "CONFIDENCE.T" {
                if size < 2.0 {
                    return Err(E::Div0);
                }
                return num(t_inv(1.0 - alpha / 2.0, size - 1.0) * sd / size.sqrt());
            }
            num(norm_s_inv(1.0 - alpha / 2.0) * sd / size.sqrt())
        }
        "FORECAST" | "FORECAST.LINEAR" => {
            need(a, 3, 3)?;
            let x = ev.number(a, 0)?;
            let (m, b, ..) = linfit(&paired(ev, a, 2, 1)?)?;
            num(b + m * x)
        }
        "SLOPE" | "INTERCEPT" => {
            need(a, 2, 2)?;
            let (m, b, ..) = linfit(&paired(ev, a, 1, 0)?)?;
            num(if name == "SLOPE" { m } else { b })
        }
        "RSQ" | "PEARSON" => {
            need(a, 2, 2)?;
            let p = paired(ev, a, 1, 0)?;
            let (_, _, sxx, sxy, syy, ..) = linfit(&p)?;
            if syy == 0.0 {
                return Err(E::Div0);
            }
            let r = sxy / (sxx * syy).sqrt();
            num(if name == "RSQ" { r * r } else { r })
        }
        "STEYX" => {
            need(a, 2, 2)?;
            let p = paired(ev, a, 1, 0)?;
            if p.len() < 3 {
                return Err(E::Div0);
            }
            let (_, _, sxx, sxy, syy, ..) = linfit(&p)?;
            num(((syy - sxy * sxy / sxx) / (p.len() as f64 - 2.0)).max(0.0).sqrt())
        }
        "COVARIANCE.P" | "COVARIANCE.S" | "COVAR" => {
            need(a, 2, 2)?;
            let p = paired(ev, a, 0, 1)?;
            let nn = p.len() as f64;
            let sample = name == "COVARIANCE.S";
            if nn < if sample { 2.0 } else { 1.0 } {
                return Err(E::Div0);
            }
            let mx = p.iter().map(|q| q.0).sum::<f64>() / nn;
            let my = p.iter().map(|q| q.1).sum::<f64>() / nn;
            let s: f64 = p.iter().map(|q| (q.0 - mx) * (q.1 - my)).sum();
            num(s / if sample { nn - 1.0 } else { nn })
        }
        "TREND" | "GROWTH" => trend(ev, a, name == "GROWTH"),
        "LINEST" => linest(ev, a),
        "PERCENTRANK" | "PERCENTRANK.INC" | "PERCENTRANK.EXC" => {
            need(a, 2, 3)?;
            let mut v = nums(ev, a, 0)?;
            let x = ev.number(a, 1)?;
            let sig = ev.number_or(a, 2, 3.0)?.trunc();
            if sig < 1.0 || v.is_empty() {
                return Err(E::Num);
            }
            v.sort_by(|p, q| p.partial_cmp(q).unwrap_or(Ordering::Equal));
            let exc = name == "PERCENTRANK.EXC";
            let nn = v.len() as f64;
            let pr = |t: f64| {
                let below = v.iter().filter(|y| **y < t).count() as f64;
                if exc { (below + 1.0) / (nn + 1.0) } else if v.len() == 1 { 1.0 } else { below / (nn - 1.0) }
            };
            if x < v[0] || x > v[v.len() - 1] {
                return Err(E::NA);
            }
            let r = if v.contains(&x) {
                pr(x)
            } else {
                let lo = v.iter().copied().filter(|y| *y < x).fold(f64::NEG_INFINITY, f64::max);
                let hi = v.iter().copied().filter(|y| *y > x).fold(f64::INFINITY, f64::min);
                pr(lo) + (x - lo) / (hi - lo) * (pr(hi) - pr(lo))
            };
            let p = 10f64.powi(sig as i32);
            num(((r * p) + 1e-9).floor() / p)
        }
        "RANK.AVG" => {
            need(a, 2, 3)?;
            let x = ev.number(a, 0)?;
            let v = nums(ev, a, 1)?;
            let asc = ev.number_or(a, 2, 0.0)? != 0.0;
            let ties = v.iter().filter(|y| **y == x).count();
            if ties == 0 {
                return Err(E::NA);
            }
            let before = v.iter().filter(|y| if asc { **y < x } else { **y > x }).count();
            num(before as f64 + (ties as f64 + 1.0) / 2.0)
        }
        "PERCENTILE.EXC" | "QUARTILE.EXC" => {
            need(a, 2, 2)?;
            let v = nums(ev, a, 0)?;
            let k = if name == "QUARTILE.EXC" {
                let q = ev.number(a, 1)?.trunc();
                if !(1.0..=3.0).contains(&q) {
                    return Err(E::Num);
                }
                q / 4.0
            } else {
                ev.number(a, 1)?
            };
            num(percentile_exc(v, k)?)
        }
        "MODE.MULT" => {
            let v = ev.numbers(a, false)?;
            let counts: Vec<usize> = v.iter().map(|x| v.iter().filter(|y| *y == x).count()).collect();
            let best = counts.iter().copied().max().unwrap_or(0);
            if best < 2 {
                return Err(E::NA);
            }
            let mut out: Vec<f64> = Vec::new();
            for (x, c) in v.iter().zip(&counts) {
                if *c == best && !out.contains(x) {
                    out.push(*x);
                }
            }
            arr(out.into_iter().map(|x| vec![Value::Number(x)]).collect())
        }

        // ----- financial -----
        "CUMIPMT" | "CUMPRINC" => {
            need(a, 6, 6)?;
            let r = ev.number(a, 0)?;
            let np = ev.number(a, 1)?.trunc();
            let pv = ev.number(a, 2)?;
            let s = ev.number(a, 3)?.trunc();
            let e = ev.number(a, 4)?.trunc();
            let t = ev.number(a, 5)?.trunc();
            if r <= 0.0 || np <= 0.0 || pv <= 0.0 || s < 1.0 || e < s || e > np || !(t == 0.0 || t == 1.0) || e - s >= PERIODS_MAX {
                return Err(E::Num);
            }
            let p = pmt(r, np, pv, 0.0, t);
            let mut total = 0.0;
            let mut per = s;
            while per <= e {
                let i = ipmt(r, per, np, pv, 0.0, t);
                total += if name == "CUMIPMT" { i } else { p - i };
                per += 1.0;
            }
            num(total)
        }
        "EFFECT" | "NOMINAL" => {
            need(a, 2, 2)?;
            let r = ev.number(a, 0)?;
            let np = ev.number(a, 1)?.trunc();
            if r <= 0.0 || np < 1.0 {
                return Err(E::Num);
            }
            num(if name == "EFFECT" { (1.0 + r / np).powf(np) - 1.0 } else { np * ((1.0 + r).powf(1.0 / np) - 1.0) })
        }
        "SLN" => {
            need(a, 3, 3)?;
            let (c, s, l) = (ev.number(a, 0)?, ev.number(a, 1)?, ev.number(a, 2)?);
            if l == 0.0 { Err(E::Div0) } else { num((c - s) / l) }
        }
        "SYD" => {
            need(a, 4, 4)?;
            let (c, s, l, p) = (ev.number(a, 0)?, ev.number(a, 1)?, ev.number(a, 2)?, ev.number(a, 3)?);
            if l <= 0.0 || p <= 0.0 || p > l {
                return Err(E::Num);
            }
            num((c - s) * (l - p + 1.0) * 2.0 / (l * (l + 1.0)))
        }
        "DB" => {
            need(a, 4, 5)?;
            let (c, s, l) = (ev.number(a, 0)?, ev.number(a, 1)?, ev.number(a, 2)?.trunc());
            let p = ev.number(a, 3)?.trunc();
            let m = ev.number_or(a, 4, 12.0)?.trunc();
            if c < 0.0 || s < 0.0 || l <= 0.0 || p <= 0.0 || !(1.0..=12.0).contains(&m) || p > l + if m < 12.0 { 1.0 } else { 0.0 } || p > PERIODS_MAX {
                return Err(E::Num);
            }
            if c == 0.0 {
                return num(0.0);
            }
            let rate = round_half_away(1.0 - (s / c).powf(1.0 / l), 3);
            let mut total = c * rate * m / 12.0;
            let mut dep = total;
            let mut per = 2.0;
            while per <= p {
                dep = if per == l + 1.0 { (c - total) * rate * (12.0 - m) / 12.0 } else { (c - total) * rate };
                total += dep;
                per += 1.0;
            }
            num(dep)
        }
        "DDB" => {
            need(a, 4, 5)?;
            let (c, s, l, p) = (ev.number(a, 0)?, ev.number(a, 1)?, ev.number(a, 2)?, ev.number(a, 3)?);
            let f = ev.number_or(a, 4, 2.0)?;
            if c < 0.0 || s < 0.0 || l <= 0.0 || p <= 0.0 || p > l || f <= 0.0 || p > PERIODS_MAX {
                return Err(E::Num);
            }
            let (mut total, mut dep) = (0.0, 0.0);
            let mut per = 1.0;
            while per <= p.ceil() {
                dep = ((c - total) * f / l).min(c - s - total).max(0.0);
                total += dep;
                per += 1.0;
            }
            num(dep)
        }
        "XNPV" => {
            need(a, 3, 3)?;
            let r = ev.number(a, 0)?;
            let (vals, dates) = cash_flows(ev, a, 1, 2)?;
            if r <= -1.0 {
                return Err(E::Num);
            }
            num(xnpv(r, &vals, &dates))
        }
        "XIRR" => {
            need(a, 2, 3)?;
            let (vals, dates) = cash_flows(ev, a, 0, 1)?;
            if !vals.iter().any(|v| *v > 0.0) || !vals.iter().any(|v| *v < 0.0) {
                return Err(E::Num);
            }
            let mut r = ev.number_or(a, 2, 0.1)?;
            for _ in 0..200 {
                let f = xnpv(r, &vals, &dates);
                let df: f64 = vals.iter().zip(&dates).map(|(v, d)| {
                    let t = (d - dates[0]) / 365.0;
                    -t * v / (1.0 + r).powf(t + 1.0)
                }).sum();
                if df == 0.0 || !df.is_finite() {
                    break;
                }
                let nr = (r - f / df).max(-0.999_999_999);
                if (nr - r).abs() < 1e-12 {
                    return num(nr);
                }
                r = nr;
            }
            Err(E::Num)
        }
        "MIRR" => {
            need(a, 3, 3)?;
            let v = nums(ev, a, 0)?;
            let fr = ev.number(a, 1)?;
            let rr = ev.number(a, 2)?;
            let nn = v.len() as f64;
            let neg: f64 = v.iter().enumerate().filter(|(_, x)| **x < 0.0).map(|(i, x)| x / (1.0 + fr).powi(i as i32)).sum();
            let pos: f64 = v.iter().enumerate().filter(|(_, x)| **x > 0.0).map(|(i, x)| x * (1.0 + rr).powf(nn - 1.0 - i as f64)).sum();
            if neg == 0.0 || pos == 0.0 || nn < 2.0 {
                return Err(E::Div0);
            }
            num((pos / -neg).powf(1.0 / (nn - 1.0)) - 1.0)
        }
        "FVSCHEDULE" => {
            need(a, 2, 2)?;
            let p = ev.number(a, 0)?;
            let mut out = p;
            for v in flat(grid_arg(ev, a, 1)?) {
                out *= 1.0 + to_num(&v)?;
            }
            num(out)
        }
        "PDURATION" => {
            need(a, 3, 3)?;
            let (r, pv, fv) = (ev.number(a, 0)?, ev.number(a, 1)?, ev.number(a, 2)?);
            if r <= 0.0 || pv <= 0.0 || fv <= 0.0 {
                return Err(E::Num);
            }
            num((fv.ln() - pv.ln()) / (1.0 + r).ln())
        }
        "RRI" => {
            need(a, 3, 3)?;
            let (np, pv, fv) = (ev.number(a, 0)?, ev.number(a, 1)?, ev.number(a, 2)?);
            if np <= 0.0 || pv == 0.0 {
                return Err(E::Num);
            }
            num((fv / pv).powf(1.0 / np) - 1.0)
        }
        _ => super::more::call(ev, name, a),
    }
}

// ---------------------------------------------------------------------------
// Dynamic-array helpers
// ---------------------------------------------------------------------------

/// Index range kept by TAKE (or DROP) for a count `k` (negative = from the end).
fn span(len: usize, k: Option<i64>, take: bool) -> std::ops::Range<usize> {
    let l = len as i64;
    match k {
        None => 0..len,
        Some(k) if take => {
            if k >= 0 { 0..k.min(l) as usize } else { (l + k).max(0) as usize..len }
        }
        Some(k) => {
            if k >= 0 { k.min(l) as usize..len } else { 0..(l + k).max(0) as usize }
        }
    }
}

fn delimiters(ev: &mut Evaluator, a: &[Expr], i: usize) -> Result<Vec<Vec<char>>, E> {
    if !ev.has(a, i) {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for v in flat(grid_arg(ev, a, i)?) {
        let s = to_text(&v)?;
        if !s.is_empty() {
            out.push(s.chars().collect());
        }
    }
    Ok(out)
}

/// Splits on any of the delimiters (longest match wins).
fn split_multi(s: &str, delims: &[Vec<char>], ci: bool) -> Vec<String> {
    let hc: Vec<char> = s.chars().collect();
    let eq = |x: char, y: char| if ci { x.to_lowercase().eq(y.to_lowercase()) } else { x == y };
    let (mut out, mut cur, mut i) = (Vec::new(), String::new(), 0);
    while i < hc.len() {
        let m = delims.iter().filter(|d| i + d.len() <= hc.len() && d.iter().zip(&hc[i..]).all(|(x, y)| eq(*x, *y))).map(|d| d.len()).max();
        match m {
            Some(l) => {
                out.push(std::mem::take(&mut cur));
                i += l;
            }
            None => {
                cur.push(hc[i]);
                i += 1;
            }
        }
    }
    out.push(cur);
    out
}

fn textsplit(ev: &mut Evaluator, a: &[Expr]) -> Res {
    need(a, 2, 6)?;
    let t = ev.string(a, 0)?;
    let cd = delimiters(ev, a, 1)?;
    let rd = delimiters(ev, a, 2)?;
    if cd.is_empty() && rd.is_empty() {
        return Err(E::Value);
    }
    let ignore = ev.bool_or(a, 3, false)?;
    let ci = ev.number_or(a, 4, 0.0)? == 1.0;
    let pad = value_or_error(ev, a, 5, Value::Error(E::NA));
    let rows = if rd.is_empty() { vec![t] } else { split_multi(&t, &rd, ci) };
    let mut g: G = rows
        .into_iter()
        .filter(|r| !(ignore && r.is_empty()))
        .map(|r| {
            let parts = if cd.is_empty() { vec![r] } else { split_multi(&r, &cd, ci) };
            parts.into_iter().filter(|p| !(ignore && p.is_empty())).map(Value::Text).collect::<Vec<_>>()
        })
        .filter(|r| !r.is_empty())
        .collect();
    let w = dims(&g).1;
    // Padding every row to the widest one can square the text length: refuse huge results.
    if w == 0 || g.len() as u64 * w as u64 > ARRAY_MAX {
        return Err(E::Calc);
    }
    for r in g.iter_mut() {
        r.resize(w, pad.clone());
    }
    arr(g)
}

fn sortby(ev: &mut Evaluator, a: &[Expr]) -> Res {
    if a.len() < 2 {
        return Err(E::Value);
    }
    let g = grid_arg(ev, a, 0)?;
    let (h, w) = dims(&g);
    let mut keys: Vec<(Vec<Value>, bool)> = Vec::new();
    let mut by_col: Option<bool> = None;
    let mut i = 1;
    while i < a.len() {
        let kg = grid_arg(ev, a, i)?;
        let (kh, kw) = dims(&kg);
        let desc = if ev.has(a, i + 1) {
            match ev.number(a, i + 1)? {
                o if o == 1.0 => false,
                o if o == -1.0 => true,
                _ => return Err(E::Value),
            }
        } else {
            false
        };
        let cols = if kw == 1 && kh == h {
            false
        } else if kh == 1 && kw == w {
            true
        } else {
            return Err(E::Value);
        };
        if by_col.is_some_and(|b| b != cols) {
            return Err(E::Value);
        }
        by_col = Some(cols);
        keys.push((flat(kg), desc));
        i += 2;
    }
    let cols = by_col.unwrap_or(false);
    let mut order: Vec<usize> = (0..if cols { w } else { h }).collect();
    order.sort_by(|&x, &y| {
        for (k, desc) in &keys {
            let o = compare(&k[x], &k[y]);
            let o = if *desc { o.reverse() } else { o };
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    });
    arr(if cols { g.iter().map(|row| order.iter().map(|&j| row[j].clone()).collect()).collect() } else { order.iter().map(|&j| g[j].clone()).collect() })
}

// ---------------------------------------------------------------------------
// AGGREGATE
// ---------------------------------------------------------------------------

fn aggregate(ev: &mut Evaluator, a: &[Expr]) -> Res {
    if a.len() < 3 {
        return Err(E::Value);
    }
    let f = int_arg(ev, a, 0)?;
    let opt = if ev.has(a, 1) { int_arg(ev, a, 1)? } else { 0 };
    if !(1..=19).contains(&f) || !(0..=7).contains(&opt) {
        return Err(E::Value);
    }
    let skip_hidden = opt % 2 == 1;
    let skip_err = matches!(opt, 2 | 3 | 6 | 7);
    let skip_nested = opt <= 3;
    let refs = if f >= 14 { &a[2..3] } else { &a[2..] };
    let mut vals: Vec<Value> = Vec::new();
    for e in refs {
        if matches!(e, Expr::Missing) {
            continue;
        }
        match ev.eval(e)? {
            EV::R(s, rg) => {
                let rg = ev.clip_extent(s, rg);
                let g = ev.grid(EV::R(s, rg));
                let wb = ev.wb;
                let sh = &wb.sheets[s];
                for (i, row) in g.into_iter().enumerate() {
                    let r = rg.r1 + i as u32;
                    if skip_hidden && sh.hidden_rows.contains(&r) {
                        continue;
                    }
                    for (j, v) in row.into_iter().enumerate() {
                        let nested = skip_nested && sh.get(r, rg.c1 + j as u32).is_some_and(is_subtotal_cell);
                        if !nested {
                            vals.push(v);
                        }
                    }
                }
            }
            other => vals.extend(flat(ev.grid(other))),
        }
    }
    let (mut xs, mut counta) = (Vec::new(), 0.0);
    for v in &vals {
        match v {
            Value::Number(x) => {
                xs.push(*x);
                counta += 1.0;
            }
            Value::Error(e) if !skip_err => return Err(*e),
            Value::Error(_) | Value::Empty => {}
            _ => counta += 1.0,
        }
    }
    let k = if f >= 14 { Some(ev.number(a, 3)?) } else { None };
    let sorted = |mut v: Vec<f64>| {
        v.sort_by(|p, q| p.partial_cmp(q).unwrap_or(Ordering::Equal));
        v
    };
    match f {
        1 => if xs.is_empty() { Err(E::Div0) } else { num(mean(&xs)) },
        2 => num(xs.len() as f64),
        3 => num(counta),
        4 => num(xs.iter().copied().fold(f64::NEG_INFINITY, f64::max).max(if xs.is_empty() { 0.0 } else { f64::NEG_INFINITY })),
        5 => num(if xs.is_empty() { 0.0 } else { xs.iter().copied().fold(f64::INFINITY, f64::min) }),
        6 => num(xs.iter().product()),
        7 => num(stats(&xs, true)?.sqrt()),
        8 => num(stats(&xs, false)?.sqrt()),
        9 => num(xs.iter().sum()),
        10 => num(stats(&xs, true)?),
        11 => num(stats(&xs, false)?),
        12 => num(percentile(xs, 0.5)?),
        13 => {
            let mut best: Option<(f64, usize)> = None;
            for x in &xs {
                let c = xs.iter().filter(|y| *y == x).count();
                if c > 1 && best.is_none_or(|(_, bc)| c > bc) {
                    best = Some((*x, c));
                }
            }
            best.map(|(x, _)| EV::V(Value::Number(x))).ok_or(E::NA)
        }
        14 | 15 => {
            let v = sorted(xs);
            let k = k.unwrap_or(0.0).ceil() as usize;
            if k == 0 || k > v.len() {
                return Err(E::Num);
            }
            num(if f == 14 { v[v.len() - k] } else { v[k - 1] })
        }
        16 => num(percentile(xs, k.unwrap_or(-1.0))?),
        17 => {
            let q = k.unwrap_or(-1.0).trunc();
            if !(0.0..=4.0).contains(&q) {
                return Err(E::Num);
            }
            num(percentile(xs, q / 4.0)?)
        }
        18 => num(percentile_exc(xs, k.unwrap_or(-1.0))?),
        _ => {
            let q = k.unwrap_or(-1.0).trunc();
            if !(1.0..=3.0).contains(&q) {
                return Err(E::Num);
            }
            num(percentile_exc(xs, q / 4.0)?)
        }
    }
}

fn percentile_exc(mut v: Vec<f64>, k: f64) -> Result<f64, E> {
    let nn = v.len() as f64;
    let pos = k * (nn + 1.0);
    if v.is_empty() || k <= 0.0 || k >= 1.0 || pos < 1.0 || pos > nn {
        return Err(E::Num);
    }
    v.sort_by(|p, q| p.partial_cmp(q).unwrap_or(Ordering::Equal));
    let lo = pos.floor() as usize - 1;
    let hi = (lo + 1).min(v.len() - 1);
    Ok(v[lo] + (v[hi] - v[lo]) * pos.fract())
}

// ---------------------------------------------------------------------------
// Text / number helpers
// ---------------------------------------------------------------------------

fn numbervalue(t: &str, dec: char, grp: char) -> Result<f64, E> {
    if dec == grp {
        return Err(E::Value);
    }
    let s: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    if s.is_empty() {
        return Ok(0.0);
    }
    let pct = s.chars().rev().take_while(|c| *c == '%').count();
    let body: String = s.chars().take(s.chars().count() - pct).collect();
    let (int, frac) = match body.split_once(dec) {
        Some((i, f)) => (i.to_string(), Some(f.to_string())),
        None => (body.clone(), None),
    };
    if frac.as_ref().is_some_and(|f| f.contains(dec) || f.contains(grp)) {
        return Err(E::Value);
    }
    let int: String = int.chars().filter(|c| *c != grp).collect();
    let txt = match frac {
        Some(f) => format!("{int}.{f}"),
        None => int,
    };
    if txt.is_empty() || !txt.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')) {
        return Err(E::Value);
    }
    let v: f64 = txt.parse().map_err(|_| E::Value)?;
    Ok(v / 100f64.powi(pct as i32))
}

/// Fixed decimals with thousands separators.
fn grouped(x: f64, d: usize) -> String {
    let s = format!("{x:.d$}");
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i.to_string(), Some(f.to_string())),
        None => (s, None),
    };
    let mut g = String::new();
    for (k, ch) in int.chars().enumerate() {
        if k > 0 && (int.len() - k) % 3 == 0 {
            g.push(',');
        }
        g.push(ch);
    }
    match frac {
        Some(f) => format!("{g}.{f}"),
        None => g,
    }
}

fn fmt_radix(mut v: u64, radix: u32) -> String {
    const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    if v == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while v > 0 {
        out.push(DIGITS[(v % radix as u64) as usize]);
        v /= radix as u64;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Parses up to 10 digits; a 10-digit value with the top bit set is two's complement.
pub(super) fn from_radix10(s: &str, radix: u32) -> Result<f64, E> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(0.0);
    }
    if s.len() > 10 {
        return Err(E::Num);
    }
    let v = u64::from_str_radix(s, radix).map_err(|_| E::Num)?;
    let full = (radix as u64).pow(10);
    Ok(if s.len() == 10 && v >= full / 2 { v as f64 - full as f64 } else { v as f64 })
}

fn roman(mut val: u32, form: u32) -> String {
    const CH: [char; 7] = ['M', 'D', 'C', 'L', 'X', 'V', 'I'];
    const VA: [u32; 7] = [1000, 500, 100, 50, 10, 5, 1];
    let max = 6;
    let mut out = String::new();
    for i in 0..=max / 2 {
        let mut idx = 2 * i;
        let digit = val / VA[idx];
        if digit % 5 == 4 {
            let idx2 = if digit == 4 { idx - 1 } else { idx - 2 };
            let mut steps = 0;
            while steps < form && idx < max {
                steps += 1;
                if VA[idx2] - VA[idx + 1] <= val {
                    idx += 1;
                } else {
                    steps = form;
                }
            }
            out.push(CH[idx]);
            out.push(CH[idx2]);
            val = val + VA[idx] - VA[idx2];
        } else {
            if digit > 4 {
                out.push(CH[idx - 1]);
            }
            for _ in 0..digit % 5 {
                out.push(CH[idx]);
            }
            val %= VA[idx];
        }
    }
    out
}

fn arabic(s: &str) -> Result<f64, E> {
    let t = s.trim();
    if t.len() > 255 {
        return Err(E::Value);
    }
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t),
    };
    let vals: Vec<f64> = t
        .chars()
        .map(|c| match c.to_ascii_uppercase() {
            'I' => Ok(1.0),
            'V' => Ok(5.0),
            'X' => Ok(10.0),
            'L' => Ok(50.0),
            'C' => Ok(100.0),
            'D' => Ok(500.0),
            'M' => Ok(1000.0),
            _ => Err(E::Value),
        })
        .collect::<Result<_, _>>()?;
    let mut total = 0.0;
    for (i, v) in vals.iter().enumerate() {
        if vals.get(i + 1).is_some_and(|n| n > v) { total -= v } else { total += v }
    }
    Ok(if neg { -total } else { total })
}

pub(super) fn combin(n: f64, k: f64) -> Result<f64, E> {
    let (n, k) = (n.trunc(), k.trunc());
    if n < 0.0 || k < 0.0 || n < k {
        return Err(E::Num);
    }
    let k = k.min(n - k);
    let mut r = 1.0;
    // With k <= n/2 every factor is >= 1 and C(n, i) >= 2^i, so a huge k overflows within
    // ~1000 steps: stop there instead of looping k times.
    for i in 0..k as u64 {
        r = r * (n - i as f64) / (i as f64 + 1.0);
        if !r.is_finite() {
            return Err(E::Num);
        }
    }
    Ok(r.round())
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

fn yearfrac(s: f64, e: f64, basis: i64) -> Result<f64, E> {
    let (s, e) = if s <= e { (s.floor(), e.floor()) } else { (e.floor(), s.floor()) };
    let (y1, m1, d1) = numfmt::serial_to_ymd(s);
    let (y2, m2, d2) = numfmt::serial_to_ymd(e);
    let leap = |y: i64| numfmt::days_in_month(y, 2) == 29;
    let d360 = |d1: i64, d2: i64| ((y2 - y1) * 360 + (m2 - m1) * 30 + (d2 - d1)) as f64 / 360.0;
    match basis {
        0 => {
            let last_feb = |y: i64, m: i64, d: i64| m == 2 && d == numfmt::days_in_month(y, 2);
            let (mut a, mut b) = (d1, d2);
            if last_feb(y1, m1, d1) && last_feb(y2, m2, d2) {
                b = 30;
            }
            if last_feb(y1, m1, d1) {
                a = 30;
            }
            if b == 31 && a >= 30 {
                b = 30;
            }
            if a == 31 {
                a = 30;
            }
            Ok(d360(a, b))
        }
        1 => {
            let within_year = y1 == y2 || (y2 == y1 + 1 && (m1 > m2 || (m1 == m2 && d1 >= d2)));
            if within_year {
                let feb29 = |y: i64| leap(y) && { let f = numfmt::ymd_to_serial(y, 2, 29); s <= f && f <= e };
                let denom = if (y1 == y2 && leap(y1)) || feb29(y1) || feb29(y2) { 366.0 } else { 365.0 };
                Ok((e - s) / denom)
            } else {
                let years = (y2 - y1 + 1) as f64;
                let days = numfmt::ymd_to_serial(y2 + 1, 1, 1) - numfmt::ymd_to_serial(y1, 1, 1);
                Ok((e - s) / (days / years))
            }
        }
        2 => Ok((e - s) / 360.0),
        3 => Ok((e - s) / 365.0),
        4 => Ok(d360(d1.min(30), d2.min(30))),
        _ => Err(E::Num),
    }
}

/// Weekend days (Monday first) for NETWORKDAYS.INTL / WORKDAY.INTL.
fn weekend_mask(v: Option<Value>) -> Result<[bool; 7], E> {
    let mut m = [false; 7];
    match v {
        None | Some(Value::Empty) => {
            m[5] = true;
            m[6] = true;
        }
        Some(Value::Text(s)) => {
            if s.len() != 7 || !s.chars().all(|c| c == '0' || c == '1') || s == "1111111" {
                return Err(E::Value);
            }
            for (i, c) in s.chars().enumerate() {
                m[i] = c == '1';
            }
        }
        Some(v) => {
            let k = to_num(&v)?.trunc() as i64;
            match k {
                1..=7 => {
                    m[((k + 4) % 7) as usize] = true;
                    m[((k + 5) % 7) as usize] = true;
                }
                11..=17 => m[((k - 12).rem_euclid(7)) as usize] = true,
                _ => return Err(E::Num),
            }
        }
    }
    Ok(m)
}

// ---------------------------------------------------------------------------
// CONVERT
// ---------------------------------------------------------------------------

/// (unit, category, factor to the base unit, accepts metric prefixes). Category 3 = temperature.
const UNITS: &[(&str, u8, f64, bool)] = &[
    // length (m)
    ("m", 0, 1.0, true),
    ("mi", 0, 1609.344, false),
    ("Nmi", 0, 1852.0, false),
    ("in", 0, 0.0254, false),
    ("ft", 0, 0.3048, false),
    ("yd", 0, 0.9144, false),
    ("ang", 0, 1e-10, true),
    ("ell", 0, 1.143, false),
    ("ly", 0, 9.4607304725808e15, true),
    ("parsec", 0, 3.085_677_581_281_5e16, true),
    ("pc", 0, 3.085_677_581_281_5e16, true),
    ("Pica", 0, 0.0254 / 72.0, false),
    ("Picapt", 0, 0.0254 / 72.0, false),
    ("pica", 0, 0.0254 / 6.0, false),
    ("survey_mi", 0, 1609.347_218_694_437, false),
    // mass (g)
    ("g", 1, 1.0, true),
    ("sg", 1, 14593.902_937_206_364, false),
    ("lbm", 1, 453.59237, false),
    ("u", 1, 1.660_538_782e-24, true),
    ("ozm", 1, 28.349_523_125, false),
    ("grain", 1, 0.064_798_91, false),
    ("cwt", 1, 45359.237, false),
    ("shweight", 1, 45359.237, false),
    ("uk_cwt", 1, 50802.34544, false),
    ("lcwt", 1, 50802.34544, false),
    ("hweight", 1, 50802.34544, false),
    ("stone", 1, 6350.29318, false),
    ("ton", 1, 907_184.74, false),
    ("uk_ton", 1, 1_016_046.9088, false),
    ("LTON", 1, 1_016_046.9088, false),
    ("brton", 1, 1_016_046.9088, false),
    // time (s)
    ("yr", 2, 31_557_600.0, false),
    ("day", 2, 86400.0, false),
    ("d", 2, 86400.0, false),
    ("hr", 2, 3600.0, false),
    ("mn", 2, 60.0, false),
    ("min", 2, 60.0, false),
    ("sec", 2, 1.0, true),
    ("s", 2, 1.0, true),
    // temperature
    ("C", 3, 0.0, false),
    ("cel", 3, 0.0, false),
    ("F", 3, 0.0, false),
    ("fah", 3, 0.0, false),
    ("K", 3, 0.0, true),
    ("kel", 3, 0.0, true),
    ("Rank", 3, 0.0, false),
    ("Reau", 3, 0.0, false),
];

const PREFIXES: &[(&str, f64)] = &[
    ("da", 1e1),
    ("Y", 1e24),
    ("Z", 1e21),
    ("E", 1e18),
    ("P", 1e15),
    ("T", 1e12),
    ("G", 1e9),
    ("M", 1e6),
    ("k", 1e3),
    ("h", 1e2),
    ("e", 1e1),
    ("d", 1e-1),
    ("c", 1e-2),
    ("m", 1e-3),
    ("u", 1e-6),
    ("n", 1e-9),
    ("p", 1e-12),
    ("f", 1e-15),
    ("a", 1e-18),
    ("z", 1e-21),
    ("y", 1e-24),
];

/// (category, factor, unit name) of a unit, resolving metric prefixes.
fn unit(u: &str) -> Option<(u8, f64, &'static str)> {
    if let Some(&(name, cat, f, _)) = UNITS.iter().find(|x| x.0 == u) {
        return Some((cat, f, name));
    }
    for (p, pf) in PREFIXES {
        if let Some(rest) = u.strip_prefix(p) {
            if let Some(&(name, cat, f, true)) = UNITS.iter().find(|x| x.0 == rest) {
                return Some((cat, f * pf, name));
            }
        }
    }
    None
}

fn convert(x: f64, from: &str, to: &str) -> Result<f64, E> {
    let (cf, ff, nf) = unit(from).ok_or(E::NA)?;
    let (ct, ft, nt) = unit(to).ok_or(E::NA)?;
    if cf != ct {
        return Err(E::NA);
    }
    if cf != 3 {
        return Ok(x * ff / ft);
    }
    // Temperature: via kelvin (prefixed kelvin scales linearly).
    let kelvin = match nf {
        "C" | "cel" => x + 273.15,
        "F" | "fah" => (x - 32.0) * 5.0 / 9.0 + 273.15,
        "Rank" => x * 5.0 / 9.0,
        "Reau" => x * 1.25 + 273.15,
        _ => x * if ff == 0.0 { 1.0 } else { ff },
    };
    Ok(match nt {
        "C" | "cel" => kelvin - 273.15,
        "F" | "fah" => (kelvin - 273.15) * 9.0 / 5.0 + 32.0,
        "Rank" => kelvin * 9.0 / 5.0,
        "Reau" => (kelvin - 273.15) * 0.8,
        _ => kelvin / if ft == 0.0 { 1.0 } else { ft },
    })
}

// ---------------------------------------------------------------------------
// Distributions
// ---------------------------------------------------------------------------

/// Standard normal CDF (W. J. Cody / Hart, double precision).
pub fn norm_cdf(x: f64) -> f64 {
    let z = x.abs();
    let c = if z > 37.0 {
        0.0
    } else {
        let e = (-z * z / 2.0).exp();
        if z < 7.071_067_811_865_47 {
            let mut n = 3.526_249_659_989_11e-2 * z + 0.700_383_064_443_688;
            n = n * z + 6.373_962_203_531_65;
            n = n * z + 33.912_866_078_383;
            n = n * z + 112.079_291_497_871;
            n = n * z + 221.213_596_169_931;
            n = n * z + 220.206_867_912_376;
            let mut d = 8.838_834_764_831_84e-2 * z + 1.755_667_163_182_64;
            d = d * z + 16.064_177_579_207;
            d = d * z + 86.780_732_202_946_1;
            d = d * z + 296.564_248_779_674;
            d = d * z + 637.333_633_378_831;
            d = d * z + 793.826_512_519_948;
            d = d * z + 440.413_735_824_752;
            e * n / d
        } else {
            let mut b = z + 0.65;
            b = z + 4.0 / b;
            b = z + 3.0 / b;
            b = z + 2.0 / b;
            b = z + 1.0 / b;
            e / b / 2.506_628_274_631
        }
    };
    if x > 0.0 { 1.0 - c } else { c }
}

pub(super) fn norm_pdf(z: f64) -> f64 {
    (-z * z / 2.0).exp() / (2.0 * PI).sqrt()
}

/// Inverse standard normal CDF (Acklam's approximation refined with Halley steps).
pub fn norm_s_inv(p: f64) -> f64 {
    const A: [f64; 6] = [-3.969_683_028_665_376e1, 2.209_460_984_245_205e2, -2.759_285_104_469_687e2, 1.383_577_518_672_69e2, -3.066_479_806_614_716e1, 2.506_628_277_459_239];
    const B: [f64; 5] = [-5.447_609_879_822_406e1, 1.615_858_368_580_409e2, -1.556_989_798_598_866e2, 6.680_131_188_771_972e1, -1.328_068_155_288_572e1];
    const C: [f64; 6] = [-7.784_894_002_430_293e-3, -3.223_964_580_411_365e-1, -2.400_758_277_161_838, -2.549_732_539_343_734, 4.374_664_141_464_968, 2.938_163_982_698_783];
    const D: [f64; 4] = [7.784_695_709_041_462e-3, 3.224_671_290_700_398e-1, 2.445_134_137_142_996, 3.754_408_661_907_416];
    let lo = 0.02425;
    let tail = |q: f64| (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5]) / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0);
    let mut x = if p < lo {
        tail((-2.0 * p.ln()).sqrt())
    } else if p <= 1.0 - lo {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        -tail((-2.0 * (1.0 - p).ln()).sqrt())
    };
    for _ in 0..2 {
        let e = norm_cdf(x) - p;
        let u = e * (2.0 * PI).sqrt() * (x * x / 2.0).exp();
        x -= u / (1.0 + x * u / 2.0);
    }
    x
}

pub(super) fn ln_gamma(x: f64) -> f64 {
    const G: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        return (PI / (PI * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let t = x + 7.5;
    let mut s = G[0];
    for (i, g) in G.iter().enumerate().skip(1) {
        s += g / (x + i as f64);
    }
    0.5 * (2.0 * PI).ln() + (x + 0.5) * t.ln() - t + s.ln()
}

/// Continued fraction for the incomplete beta function (modified Lentz).
fn betacf(a: f64, b: f64, x: f64) -> f64 {
    const TINY: f64 = 1e-300;
    let (qab, qap, qam) = (a + b, a + 1.0, a - 1.0);
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..=500 {
        let m = m as f64;
        let m2 = 2.0 * m;
        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < 1e-16 {
            break;
        }
    }
    h
}

/// Regularized incomplete beta function I_x(a, b).
pub(super) fn betai(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let bt = (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    if x < (a + 1.0) / (a + b + 2.0) { bt * betacf(a, b, x) / a } else { 1.0 - bt * betacf(b, a, 1.0 - x) / b }
}

pub(super) fn t_cdf(t: f64, df: f64) -> f64 {
    let ib = betai(df / 2.0, 0.5, df / (df + t * t));
    if t > 0.0 { 1.0 - 0.5 * ib } else { 0.5 * ib }
}

fn t_pdf(t: f64, df: f64) -> f64 {
    (ln_gamma((df + 1.0) / 2.0) - ln_gamma(df / 2.0)).exp() / (df * PI).sqrt() * (1.0 + t * t / df).powf(-(df + 1.0) / 2.0)
}

/// Left-tailed inverse of Student's t (bisection on the CDF).
fn t_inv(p: f64, df: f64) -> f64 {
    let (mut lo, mut hi) = (-1.0, 1.0);
    while t_cdf(lo, df) > p && lo > -1e10 {
        lo *= 2.0;
    }
    while t_cdf(hi, df) < p && hi < 1e10 {
        hi *= 2.0;
    }
    for _ in 0..200 {
        let mid = (lo + hi) / 2.0;
        if t_cdf(mid, df) < p {
            lo = mid;
        } else {
            hi = mid;
        }
        if hi - lo <= 1e-15 * mid.abs().max(1e-300) {
            break;
        }
    }
    (lo + hi) / 2.0
}

// ---------------------------------------------------------------------------
// Regression
// ---------------------------------------------------------------------------

/// known_y (and optional known_x) as numbers, with the shape of known_x for TREND's default.
fn known_xy(ev: &mut Evaluator, a: &[Expr]) -> Result<(Vec<f64>, Vec<f64>), E> {
    let ys: Vec<f64> = flat(grid_arg(ev, a, 0)?).iter().map(to_num).collect::<Result<_, _>>()?;
    let xs: Vec<f64> = if ev.has(a, 1) {
        let g = grid_arg(ev, a, 1)?;
        let (h, w) = dims(&g);
        if h > 1 && w > 1 && h * w != ys.len() {
            return Err(E::Ref);
        }
        flat(g).iter().map(to_num).collect::<Result<_, _>>()?
    } else {
        (1..=ys.len()).map(|i| i as f64).collect()
    };
    if xs.len() != ys.len() || ys.is_empty() {
        return Err(E::Ref);
    }
    Ok((xs, ys))
}

/// Least-squares line (slope, intercept), optionally forced through the origin.
fn fit(xs: &[f64], ys: &[f64], konst: bool) -> Result<(f64, f64), E> {
    if konst {
        let p: Vec<(f64, f64)> = xs.iter().copied().zip(ys.iter().copied()).collect();
        let (m, b, ..) = linfit(&p)?;
        Ok((m, b))
    } else {
        let sxx: f64 = xs.iter().map(|x| x * x).sum();
        if sxx == 0.0 {
            return Err(E::Div0);
        }
        Ok((xs.iter().zip(ys).map(|(x, y)| x * y).sum::<f64>() / sxx, 0.0))
    }
}

fn trend(ev: &mut Evaluator, a: &[Expr], growth: bool) -> Res {
    need(a, 1, 4)?;
    let (xs, ys) = known_xy(ev, a)?;
    let konst = ev.bool_or(a, 3, true)?;
    let ys: Vec<f64> = if growth {
        if ys.iter().any(|y| *y <= 0.0) {
            return Err(E::Num);
        }
        ys.iter().map(|y| y.ln()).collect()
    } else {
        ys
    };
    let (m, b) = fit(&xs, &ys, konst)?;
    let new_x: G = if ev.has(a, 2) {
        grid_arg(ev, a, 2)?
    } else if ev.has(a, 1) {
        grid_arg(ev, a, 1)?
    } else {
        xs.iter().map(|x| vec![Value::Number(*x)]).collect()
    };
    let out: G = new_x
        .iter()
        .map(|row| {
            row.iter()
                .map(|v| match to_num(v) {
                    Ok(x) => {
                        let y = b + m * x;
                        Value::Number(if growth { y.exp() } else { y })
                    }
                    Err(e) => Value::Error(e),
                })
                .collect()
        })
        .collect();
    arr(out)
}

fn linest(ev: &mut Evaluator, a: &[Expr]) -> Res {
    need(a, 1, 4)?;
    if ev.has(a, 1) {
        let g = grid_arg(ev, a, 1)?;
        let (h, w) = dims(&g);
        let yn = flat(grid_arg(ev, a, 0)?).len();
        if h > 1 && w > 1 && h * w != yn {
            return Err(E::Value); // multiple regression is not supported
        }
    }
    let (xs, ys) = known_xy(ev, a)?;
    let konst = ev.bool_or(a, 2, true)?;
    let full = ev.bool_or(a, 3, false)?;
    let (m, b) = fit(&xs, &ys, konst)?;
    if !full {
        return arr(vec![vec![Value::Number(m), Value::Number(b)]]);
    }
    let nn = ys.len() as f64;
    let df = nn - if konst { 2.0 } else { 1.0 };
    let pred: Vec<f64> = xs.iter().map(|x| b + m * x).collect();
    let ss_resid: f64 = ys.iter().zip(&pred).map(|(y, p)| (y - p).powi(2)).sum();
    let my = mean(&ys);
    let ss_reg: f64 = if konst { pred.iter().map(|p| (p - my).powi(2)).sum() } else { pred.iter().map(|p| p * p).sum() };
    let ss_tot = ss_reg + ss_resid;
    let r2 = if ss_tot == 0.0 { 1.0 } else { ss_reg / ss_tot };
    let v = |x: f64| if x.is_finite() { Value::Number(x) } else { Value::Error(E::Num) };
    let div = |x: f64, d: f64| if d == 0.0 { Value::Error(E::Div0) } else { v(x / d) };
    let se_y = if df > 0.0 { (ss_resid / df).sqrt() } else { f64::NAN };
    let sxx: f64 = if konst {
        let mx = mean(&xs);
        xs.iter().map(|x| (x - mx).powi(2)).sum()
    } else {
        xs.iter().map(|x| x * x).sum()
    };
    let se_m = div(se_y, sxx.sqrt());
    let se_b = if konst { v(se_y * (1.0 / nn + mean(&xs).powi(2) / sxx).sqrt()) } else { Value::Error(E::NA) };
    let f = if df > 0.0 { div(ss_reg, ss_resid / df) } else { Value::Error(E::Num) };
    arr(vec![
        vec![v(m), v(b)],
        vec![se_m, se_b],
        vec![v(r2), if df > 0.0 { v(se_y) } else { Value::Error(E::Num) }],
        vec![f, v(df)],
        vec![v(ss_reg), v(ss_resid)],
    ])
}

// ---------------------------------------------------------------------------
// Cash flows
// ---------------------------------------------------------------------------

fn cash_flows(ev: &mut Evaluator, a: &[Expr], vi: usize, di: usize) -> Result<(Vec<f64>, Vec<f64>), E> {
    let vals: Vec<f64> = flat(grid_arg(ev, a, vi)?).iter().map(to_num).collect::<Result<_, _>>()?;
    let dates: Vec<f64> = flat(grid_arg(ev, a, di)?).iter().map(|v| to_num(v).map(f64::trunc)).collect::<Result<_, _>>()?;
    if vals.len() != dates.len() || vals.is_empty() || dates.iter().any(|d| *d < dates[0] || *d < 0.0) {
        return Err(E::Num);
    }
    Ok((vals, dates))
}

fn xnpv(r: f64, vals: &[f64], dates: &[f64]) -> f64 {
    vals.iter().zip(dates).map(|(v, d)| v / (1.0 + r).powf((d - dates[0]) / 365.0)).sum()
}
