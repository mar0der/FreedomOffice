//! Built-in worksheet functions.

use std::cmp::Ordering;

use super::eval::{boolean, compare, num, text, to_bool, to_num, to_text, values_equal, Evaluator, Res, EV};
use super::parser::{Expr, RefKind};
use crate::model::{ErrorKind as E, Range, Value};
use crate::numfmt;

/// (name, arguments, category)
pub const FUNCS: &[(&str, &str, &str)] = &[
    ("ABS", "number", "Math & Trig"),
    ("ACOS", "number", "Math & Trig"),
    ("AND", "logical1, [logical2], ...", "Logical"),
    ("ASIN", "number", "Math & Trig"),
    ("ATAN", "number", "Math & Trig"),
    ("ATAN2", "x_num, y_num", "Math & Trig"),
    ("AVERAGE", "number1, [number2], ...", "Statistical"),
    ("AVERAGEA", "value1, [value2], ...", "Statistical"),
    ("AVERAGEIF", "range, criteria, [average_range]", "Statistical"),
    ("AVERAGEIFS", "average_range, criteria_range1, criteria1, ...", "Statistical"),
    ("CEILING", "number, significance", "Math & Trig"),
    ("CEILING.MATH", "number, [significance], [mode]", "Math & Trig"),
    ("CHAR", "number", "Text"),
    ("CHOOSE", "index_num, value1, [value2], ...", "Lookup & Reference"),
    ("CLEAN", "text", "Text"),
    ("CODE", "text", "Text"),
    ("COLUMN", "[reference]", "Lookup & Reference"),
    ("COLUMNS", "array", "Lookup & Reference"),
    ("CONCAT", "text1, [text2], ...", "Text"),
    ("CONCATENATE", "text1, [text2], ...", "Text"),
    ("CORREL", "array1, array2", "Statistical"),
    ("COS", "number", "Math & Trig"),
    ("COUNT", "value1, [value2], ...", "Statistical"),
    ("COUNTA", "value1, [value2], ...", "Statistical"),
    ("COUNTBLANK", "range", "Statistical"),
    ("COUNTIF", "range, criteria", "Statistical"),
    ("COUNTIFS", "criteria_range1, criteria1, ...", "Statistical"),
    ("DATE", "year, month, day", "Date & Time"),
    ("DATEDIF", "start_date, end_date, unit", "Date & Time"),
    ("DATEVALUE", "date_text", "Date & Time"),
    ("DAY", "serial_number", "Date & Time"),
    ("DAYS", "end_date, start_date", "Date & Time"),
    ("DEGREES", "angle", "Math & Trig"),
    ("EDATE", "start_date, months", "Date & Time"),
    ("EOMONTH", "start_date, months", "Date & Time"),
    ("ERROR.TYPE", "error_val", "Information"),
    ("EVEN", "number", "Math & Trig"),
    ("EXACT", "text1, text2", "Text"),
    ("EXP", "number", "Math & Trig"),
    ("FACT", "number", "Math & Trig"),
    ("FALSE", "", "Logical"),
    ("FILTER", "array, include, [if_empty]", "Lookup & Reference"),
    ("FIND", "find_text, within_text, [start_num]", "Text"),
    ("FIXED", "number, [decimals], [no_commas]", "Text"),
    ("FLOOR", "number, significance", "Math & Trig"),
    ("FLOOR.MATH", "number, [significance], [mode]", "Math & Trig"),
    ("FV", "rate, nper, pmt, [pv], [type]", "Financial"),
    ("GCD", "number1, [number2], ...", "Math & Trig"),
    ("HLOOKUP", "lookup_value, table_array, row_index_num, [range_lookup]", "Lookup & Reference"),
    ("HOUR", "serial_number", "Date & Time"),
    ("IF", "logical_test, [value_if_true], [value_if_false]", "Logical"),
    ("IFERROR", "value, value_if_error", "Logical"),
    ("IFNA", "value, value_if_na", "Logical"),
    ("IFS", "logical_test1, value_if_true1, ...", "Logical"),
    ("INDEX", "array, row_num, [column_num]", "Lookup & Reference"),
    ("INDIRECT", "ref_text, [a1]", "Lookup & Reference"),
    ("INT", "number", "Math & Trig"),
    ("IPMT", "rate, per, nper, pv, [fv], [type]", "Financial"),
    ("IRR", "values, [guess]", "Financial"),
    ("ISBLANK", "value", "Information"),
    ("ISERR", "value", "Information"),
    ("ISERROR", "value", "Information"),
    ("ISEVEN", "number", "Information"),
    ("ISFORMULA", "reference", "Information"),
    ("ISLOGICAL", "value", "Information"),
    ("ISNA", "value", "Information"),
    ("ISNONTEXT", "value", "Information"),
    ("ISNUMBER", "value", "Information"),
    ("ISODD", "number", "Information"),
    ("ISTEXT", "value", "Information"),
    ("LARGE", "array, k", "Statistical"),
    ("LCM", "number1, [number2], ...", "Math & Trig"),
    ("LEFT", "text, [num_chars]", "Text"),
    ("LEN", "text", "Text"),
    ("LN", "number", "Math & Trig"),
    ("LOG", "number, [base]", "Math & Trig"),
    ("LOG10", "number", "Math & Trig"),
    ("LOWER", "text", "Text"),
    ("MATCH", "lookup_value, lookup_array, [match_type]", "Lookup & Reference"),
    ("MAX", "number1, [number2], ...", "Statistical"),
    ("MAXA", "value1, [value2], ...", "Statistical"),
    ("MAXIFS", "max_range, criteria_range1, criteria1, ...", "Statistical"),
    ("MEDIAN", "number1, [number2], ...", "Statistical"),
    ("MID", "text, start_num, num_chars", "Text"),
    ("MIN", "number1, [number2], ...", "Statistical"),
    ("MINA", "value1, [value2], ...", "Statistical"),
    ("MINIFS", "min_range, criteria_range1, criteria1, ...", "Statistical"),
    ("MINUTE", "serial_number", "Date & Time"),
    ("MOD", "number, divisor", "Math & Trig"),
    ("MODE", "number1, [number2], ...", "Statistical"),
    ("MODE.SNGL", "number1, [number2], ...", "Statistical"),
    ("MONTH", "serial_number", "Date & Time"),
    ("MROUND", "number, multiple", "Math & Trig"),
    ("N", "value", "Information"),
    ("NA", "", "Information"),
    ("NETWORKDAYS", "start_date, end_date, [holidays]", "Date & Time"),
    ("NOT", "logical", "Logical"),
    ("NOW", "", "Date & Time"),
    ("NPER", "rate, pmt, pv, [fv], [type]", "Financial"),
    ("NPV", "rate, value1, [value2], ...", "Financial"),
    ("ODD", "number", "Math & Trig"),
    ("OFFSET", "reference, rows, cols, [height], [width]", "Lookup & Reference"),
    ("OR", "logical1, [logical2], ...", "Logical"),
    ("PERCENTILE", "array, k", "Statistical"),
    ("PERCENTILE.INC", "array, k", "Statistical"),
    ("PI", "", "Math & Trig"),
    ("PMT", "rate, nper, pv, [fv], [type]", "Financial"),
    ("POWER", "number, power", "Math & Trig"),
    ("PPMT", "rate, per, nper, pv, [fv], [type]", "Financial"),
    ("PRODUCT", "number1, [number2], ...", "Math & Trig"),
    ("PROPER", "text", "Text"),
    ("PV", "rate, nper, pmt, [fv], [type]", "Financial"),
    ("QUARTILE", "array, quart", "Statistical"),
    ("QUARTILE.INC", "array, quart", "Statistical"),
    ("QUOTIENT", "numerator, denominator", "Math & Trig"),
    ("RADIANS", "angle", "Math & Trig"),
    ("RAND", "", "Math & Trig"),
    ("RANDBETWEEN", "bottom, top", "Math & Trig"),
    ("RANK", "number, ref, [order]", "Statistical"),
    ("RANK.EQ", "number, ref, [order]", "Statistical"),
    ("RATE", "nper, pmt, pv, [fv], [type], [guess]", "Financial"),
    ("REPLACE", "old_text, start_num, num_chars, new_text", "Text"),
    ("REPT", "text, number_times", "Text"),
    ("RIGHT", "text, [num_chars]", "Text"),
    ("ROUND", "number, num_digits", "Math & Trig"),
    ("ROUNDDOWN", "number, num_digits", "Math & Trig"),
    ("ROUNDUP", "number, num_digits", "Math & Trig"),
    ("ROW", "[reference]", "Lookup & Reference"),
    ("ROWS", "array", "Lookup & Reference"),
    ("SEARCH", "find_text, within_text, [start_num]", "Text"),
    ("SECOND", "serial_number", "Date & Time"),
    ("SEQUENCE", "rows, [columns], [start], [step]", "Math & Trig"),
    ("SIGN", "number", "Math & Trig"),
    ("SIN", "number", "Math & Trig"),
    ("SMALL", "array, k", "Statistical"),
    ("SORT", "array, [sort_index], [sort_order], [by_col]", "Lookup & Reference"),
    ("SQRT", "number", "Math & Trig"),
    ("STDEV", "number1, [number2], ...", "Statistical"),
    ("STDEV.P", "number1, [number2], ...", "Statistical"),
    ("STDEV.S", "number1, [number2], ...", "Statistical"),
    ("STDEVP", "number1, [number2], ...", "Statistical"),
    ("SUBSTITUTE", "text, old_text, new_text, [instance_num]", "Text"),
    ("SUBTOTAL", "function_num, ref1, [ref2], ...", "Math & Trig"),
    ("SUM", "number1, [number2], ...", "Math & Trig"),
    ("SUMIF", "range, criteria, [sum_range]", "Math & Trig"),
    ("SUMIFS", "sum_range, criteria_range1, criteria1, ...", "Math & Trig"),
    ("SUMPRODUCT", "array1, [array2], ...", "Math & Trig"),
    ("SUMSQ", "number1, [number2], ...", "Math & Trig"),
    ("SWITCH", "expression, value1, result1, ..., [default]", "Logical"),
    ("T", "value", "Text"),
    ("TAN", "number", "Math & Trig"),
    ("TEXT", "value, format_text", "Text"),
    ("TEXTAFTER", "text, delimiter, [instance_num]", "Text"),
    ("TEXTBEFORE", "text, delimiter, [instance_num]", "Text"),
    ("TEXTJOIN", "delimiter, ignore_empty, text1, ...", "Text"),
    ("TIME", "hour, minute, second", "Date & Time"),
    ("TIMEVALUE", "time_text", "Date & Time"),
    ("TODAY", "", "Date & Time"),
    ("TRANSPOSE", "array", "Lookup & Reference"),
    ("TRIM", "text", "Text"),
    ("TRUE", "", "Logical"),
    ("TRUNC", "number, [num_digits]", "Math & Trig"),
    ("TYPE", "value", "Information"),
    ("UNICHAR", "number", "Text"),
    ("UNICODE", "text", "Text"),
    ("UNIQUE", "array, [by_col], [exactly_once]", "Lookup & Reference"),
    ("UPPER", "text", "Text"),
    ("VALUE", "text", "Text"),
    ("VAR", "number1, [number2], ...", "Statistical"),
    ("VAR.P", "number1, [number2], ...", "Statistical"),
    ("VAR.S", "number1, [number2], ...", "Statistical"),
    ("VARP", "number1, [number2], ...", "Statistical"),
    ("VLOOKUP", "lookup_value, table_array, col_index_num, [range_lookup]", "Lookup & Reference"),
    ("WEEKDAY", "serial_number, [return_type]", "Date & Time"),
    ("WEEKNUM", "serial_number, [return_type]", "Date & Time"),
    ("WORKDAY", "start_date, days, [holidays]", "Date & Time"),
    ("XLOOKUP", "lookup_value, lookup_array, return_array, [if_not_found], [match_mode], [search_mode]", "Lookup & Reference"),
    ("XMATCH", "lookup_value, lookup_array, [match_mode], [search_mode]", "Lookup & Reference"),
    ("XOR", "logical1, [logical2], ...", "Logical"),
    ("YEAR", "serial_number", "Date & Time"),
];

pub fn function_names() -> impl Iterator<Item = (&'static str, &'static str)> {
    FUNCS.iter().map(|(n, _, c)| (*n, *c))
}

pub fn signature(name: &str) -> Option<&'static str> {
    FUNCS.iter().find(|(n, _, _)| n.eq_ignore_ascii_case(name)).map(|(_, a, _)| *a)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn need(a: &[Expr], min: usize, max: usize) -> Result<(), E> {
    if a.len() < min || a.len() > max { Err(E::Value) } else { Ok(()) }
}

fn round_half_away(x: f64, digits: i32) -> f64 {
    let p = 10f64.powi(digits);
    let y = x * p;
    let frac = y.abs().fract();
    let r = if (frac - 0.5).abs() < 1e-9 { y.trunc() + y.signum() } else { y.round() };
    let out = r / p;
    if out == 0.0 { 0.0 } else { out }
}

fn wild_match(p: &[char], s: &[char]) -> bool {
    // Glob-style with '*', '?', and '~' escape. Case-insensitive.
    let (mut pi, mut si) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while si < s.len() {
        if pi < p.len() {
            let (pc, esc) = if p[pi] == '~' && pi + 1 < p.len() { (p[pi + 1], true) } else { (p[pi], false) };
            let step = if esc { 2 } else { 1 };
            if !esc && pc == '*' {
                star = Some(pi);
                mark = si;
                pi += 1;
                continue;
            }
            if (!esc && pc == '?') || pc.to_lowercase().eq(s[si].to_lowercase()) {
                pi += step;
                si += 1;
                continue;
            }
        }
        if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn has_wild(s: &str) -> bool {
    s.contains(['*', '?', '~'])
}

fn text_match(pattern: &str, s: &str) -> bool {
    if has_wild(pattern) {
        let p: Vec<char> = pattern.chars().collect();
        let sc: Vec<char> = s.chars().collect();
        wild_match(&p, &sc)
    } else {
        pattern.to_lowercase() == s.to_lowercase()
    }
}

#[derive(Debug)]
enum CritOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

struct Criteria {
    op: CritOp,
    val: Value,
}

fn parse_criteria(v: &Value) -> Criteria {
    match v {
        Value::Text(s) => {
            let (op, rest) = if let Some(r) = s.strip_prefix(">=") {
                (CritOp::Ge, r)
            } else if let Some(r) = s.strip_prefix("<=") {
                (CritOp::Le, r)
            } else if let Some(r) = s.strip_prefix("<>") {
                (CritOp::Ne, r)
            } else if let Some(r) = s.strip_prefix('>') {
                (CritOp::Gt, r)
            } else if let Some(r) = s.strip_prefix('<') {
                (CritOp::Lt, r)
            } else if let Some(r) = s.strip_prefix('=') {
                (CritOp::Eq, r)
            } else {
                (CritOp::Eq, s.as_str())
            };
            let val = if rest.is_empty() {
                Value::Empty
            } else if let Some(n) = numfmt::parse_number(rest) {
                Value::Number(n)
            } else if rest.eq_ignore_ascii_case("TRUE") {
                Value::Bool(true)
            } else if rest.eq_ignore_ascii_case("FALSE") {
                Value::Bool(false)
            } else if let Some((n, _)) = numfmt::parse_date(rest) {
                Value::Number(n)
            } else {
                Value::Text(rest.to_string())
            };
            Criteria { op, val }
        }
        v => Criteria { op: CritOp::Eq, val: v.clone() },
    }
}

fn crit_match(c: &Criteria, v: &Value) -> bool {
    match (&c.op, &c.val) {
        (CritOp::Eq, Value::Empty) => matches!(v, Value::Empty) || matches!(v, Value::Text(s) if s.is_empty()),
        (CritOp::Ne, Value::Empty) => !(matches!(v, Value::Empty) || matches!(v, Value::Text(s) if s.is_empty())),
        (CritOp::Eq, Value::Text(p)) => match v {
            Value::Text(s) => text_match(p, s),
            _ => false,
        },
        (CritOp::Ne, Value::Text(p)) => match v {
            Value::Text(s) => !text_match(p, s),
            _ => true,
        },
        (CritOp::Eq, Value::Number(n)) => match v {
            Value::Number(x) => x == n,
            Value::Text(s) => numfmt::parse_number(s) == Some(*n),
            _ => false,
        },
        (CritOp::Ne, Value::Number(n)) => !matches!(v, Value::Number(x) if x == n),
        (CritOp::Eq, val) => values_equal(v, val) && std::mem::discriminant(v) == std::mem::discriminant(val),
        (CritOp::Ne, val) => !values_equal(v, val),
        (op, val) => {
            // ordering comparisons only between same kinds
            let same = matches!((v, val), (Value::Number(_), Value::Number(_)) | (Value::Text(_), Value::Text(_)) | (Value::Bool(_), Value::Bool(_)));
            if !same {
                return false;
            }
            let o = compare(v, val);
            match op {
                CritOp::Lt => o == Ordering::Less,
                CritOp::Le => o != Ordering::Greater,
                CritOp::Gt => o == Ordering::Greater,
                CritOp::Ge => o != Ordering::Less,
                _ => false,
            }
        }
    }
}

fn get(g: &[Vec<Value>], r: usize, c: usize) -> Value {
    g.get(r).and_then(|row| row.get(c)).cloned().unwrap_or(Value::Empty)
}

/// Evaluates a range argument as a grid; for SUMIF-style "same shape as" ranges we
/// re-anchor the second range at its top-left with the given dimensions.
fn grid_shaped(ev: &mut Evaluator, v: EV, rows: usize, cols: usize) -> Vec<Vec<Value>> {
    match v {
        EV::R(s, rg) => {
            let rg2 = Range::new(rg.r1, rg.c1, rg.r1 + rows.max(1) as u32 - 1, rg.c1 + cols.max(1) as u32 - 1);
            ev.grid(EV::R(s, rg2))
        }
        other => ev.grid(other),
    }
}

/// Evaluates (range, criteria) pairs starting at `start` into a mask over a grid shape.
fn criteria_mask(ev: &mut Evaluator, a: &[Expr], start: usize, rows: usize, cols: usize) -> Result<Vec<Vec<bool>>, E> {
    let mut mask = vec![vec![true; cols]; rows];
    let mut i = start;
    while i + 1 < a.len() {
        let rv = ev.eval(&a[i])?;
        let g = grid_shaped(ev, rv, rows, cols);
        let cv = ev.value(a, i + 1)?;
        let crit = parse_criteria(&cv);
        for (r, mrow) in mask.iter_mut().enumerate() {
            for (c, m) in mrow.iter_mut().enumerate() {
                if *m && !crit_match(&crit, &get(&g, r, c)) {
                    *m = false;
                }
            }
        }
        i += 2;
    }
    if i != a.len() {
        return Err(E::Value);
    }
    Ok(mask)
}

/// Flattens a grid into a vector (row-major) and reports whether it was a column.
fn vector(g: Vec<Vec<Value>>) -> Vec<Value> {
    if g.len() == 1 {
        g.into_iter().next().unwrap()
    } else {
        g.into_iter().flat_map(|r| r.into_iter().take(1)).collect()
    }
}

fn lookup_exact(vals: &[Value], key: &Value, wildcard: bool, reverse: bool) -> Option<usize> {
    let idx: Box<dyn Iterator<Item = usize>> = if reverse { Box::new((0..vals.len()).rev()) } else { Box::new(0..vals.len()) };
    for i in idx {
        let v = &vals[i];
        let hit = match (key, v) {
            (Value::Text(k), Value::Text(s)) if wildcard => text_match(k, s),
            (Value::Text(k), Value::Text(s)) => k.to_lowercase() == s.to_lowercase(),
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            _ => false,
        };
        if hit {
            return Some(i);
        }
    }
    None
}

/// Largest value <= key (sorted ascending assumption, linear scan like a binary search would see).
fn lookup_approx(vals: &[Value], key: &Value) -> Option<usize> {
    let mut best = None;
    for (i, v) in vals.iter().enumerate() {
        if v.is_empty() || std::mem::discriminant(v) != std::mem::discriminant(key) {
            continue;
        }
        if compare(v, key) != Ordering::Greater {
            best = Some(i);
        } else {
            break;
        }
    }
    best
}

fn lookup_mode(vals: &[Value], key: &Value, mode: i64, reverse: bool) -> Option<usize> {
    if let Some(i) = lookup_exact(vals, key, mode == 2, reverse) {
        return Some(i);
    }
    if mode == -1 || mode == 1 {
        let mut best: Option<usize> = None;
        for (i, v) in vals.iter().enumerate() {
            if std::mem::discriminant(v) != std::mem::discriminant(key) {
                continue;
            }
            let o = compare(v, key);
            let ok = if mode == -1 { o == Ordering::Less } else { o == Ordering::Greater };
            if ok {
                best = match best {
                    None => Some(i),
                    Some(b) => {
                        let ob = compare(v, &vals[b]);
                        if (mode == -1 && ob == Ordering::Greater) || (mode == 1 && ob == Ordering::Less) { Some(i) } else { Some(b) }
                    }
                };
            }
        }
        return best;
    }
    None
}

fn stats(v: &[f64], sample: bool) -> Result<f64, E> {
    let n = v.len() as f64;
    if n < if sample { 2.0 } else { 1.0 } {
        return Err(E::Div0);
    }
    let mean = v.iter().sum::<f64>() / n;
    let ss: f64 = v.iter().map(|x| (x - mean).powi(2)).sum();
    Ok(ss / if sample { n - 1.0 } else { n })
}

fn percentile(mut v: Vec<f64>, k: f64) -> Result<f64, E> {
    if v.is_empty() || !(0.0..=1.0).contains(&k) {
        return Err(E::Num);
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let pos = k * (v.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    Ok(v[lo] + (v[hi] - v[lo]) * (pos - lo as f64))
}

fn gcd(a: f64, b: f64) -> f64 {
    let (mut a, mut b) = (a.abs().floor(), b.abs().floor());
    while b > 0.0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

fn add_months(serial: f64, months: i64, eom: bool) -> f64 {
    let (y, m, d) = numfmt::serial_to_ymd(serial);
    let total = y * 12 + (m - 1) + months;
    let (ny, nm) = (total.div_euclid(12), total.rem_euclid(12) + 1);
    let dim = numfmt::days_in_month(ny, nm);
    let nd = if eom { dim } else { d.min(dim) };
    numfmt::ymd_to_serial(ny, nm, nd)
}

fn pmt(rate: f64, nper: f64, pv: f64, fv: f64, typ: f64) -> f64 {
    if rate == 0.0 {
        return -(pv + fv) / nper;
    }
    let p = (1.0 + rate).powf(nper);
    -(rate * (pv * p + fv)) / ((1.0 + rate * typ) * (p - 1.0))
}

fn fv(rate: f64, nper: f64, pmt: f64, pv: f64, typ: f64) -> f64 {
    if rate == 0.0 {
        return -(pv + pmt * nper);
    }
    let p = (1.0 + rate).powf(nper);
    -(pv * p + pmt * (1.0 + rate * typ) * (p - 1.0) / rate)
}

fn ipmt(rate: f64, per: f64, nper: f64, pv: f64, fvv: f64, typ: f64) -> f64 {
    let p = pmt(rate, nper, pv, fvv, typ);
    let i = fv(rate, per - 1.0, p, pv, typ) * rate;
    if typ == 1.0 {
        if per == 1.0 { 0.0 } else { i / (1.0 + rate) }
    } else {
        i
    }
}

fn is_weekend(serial: f64) -> bool {
    let w = numfmt::weekday(serial);
    w == 1 || w == 7
}

fn sort_grid(mut g: Vec<Vec<Value>>, col: usize, desc: bool) -> Vec<Vec<Value>> {
    g.sort_by(|a, b| {
        let o = compare(&a.get(col).cloned().unwrap_or_default(), &b.get(col).cloned().unwrap_or_default());
        if desc { o.reverse() } else { o }
    });
    g
}

fn transpose(g: Vec<Vec<Value>>) -> Vec<Vec<Value>> {
    let cols = g.first().map_or(0, |r| r.len());
    (0..cols).map(|c| g.iter().map(|r| r.get(c).cloned().unwrap_or_default()).collect()).collect()
}

fn is_ref(e: &Expr) -> bool {
    matches!(e, Expr::Ref(_))
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

pub fn call(ev: &mut Evaluator, name: &str, a: &[Expr]) -> Res {
    let n = a.len();
    match name {
        // ----- Math -----
        "SUM" => num(ev.numbers(a, false)?.iter().sum()),
        "PRODUCT" => num(ev.numbers(a, false)?.iter().product()),
        "SUMSQ" => num(ev.numbers(a, false)?.iter().map(|x| x * x).sum()),
        "ABS" => num(ev.number(a, 0)?.abs()),
        "SIGN" => {
            let x = ev.number(a, 0)?;
            num(if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 })
        }
        "INT" => num(ev.number(a, 0)?.floor()),
        "TRUNC" => {
            let x = ev.number(a, 0)?;
            let d = ev.number_or(a, 1, 0.0)? as i32;
            let p = 10f64.powi(d);
            num((x * p).trunc() / p)
        }
        "ROUND" => {
            need(a, 2, 2)?;
            num(round_half_away(ev.number(a, 0)?, ev.number(a, 1)? as i32))
        }
        "ROUNDUP" | "ROUNDDOWN" => {
            need(a, 2, 2)?;
            let x = ev.number(a, 0)?;
            let p = 10f64.powi(ev.number(a, 1)? as i32);
            let y = x * p;
            let y = if (y - y.round()).abs() < 1e-9 { y.round() } else { y };
            num(if name == "ROUNDUP" { y.abs().ceil() * y.signum() } else { y.trunc() } / p)
        }
        "MROUND" => {
            let x = ev.number(a, 0)?;
            let m = ev.number(a, 1)?;
            if m == 0.0 {
                return num(0.0);
            }
            if x * m < 0.0 {
                return Err(E::Num);
            }
            num(round_half_away(x / m, 0) * m)
        }
        "CEILING" | "CEILING.MATH" | "FLOOR" | "FLOOR.MATH" => {
            let x = ev.number(a, 0)?;
            let s = ev.number_or(a, 1, 1.0)?;
            if s == 0.0 {
                return num(0.0);
            }
            let up = name.starts_with("CEILING");
            let mode = ev.number_or(a, 2, 0.0)?;
            let q = x / s.abs();
            let r = if name.ends_with(".MATH") && x < 0.0 && mode != 0.0 {
                if up { q.floor() } else { q.ceil() }
            } else if up {
                q.ceil()
            } else {
                q.floor()
            };
            num(r * s.abs())
        }
        "MOD" => {
            let x = ev.number(a, 0)?;
            let d = ev.number(a, 1)?;
            if d == 0.0 {
                return Err(E::Div0);
            }
            num(x - d * (x / d).floor())
        }
        "QUOTIENT" => {
            let x = ev.number(a, 0)?;
            let d = ev.number(a, 1)?;
            if d == 0.0 {
                return Err(E::Div0);
            }
            num((x / d).trunc())
        }
        "POWER" => {
            let x = ev.number(a, 0)?;
            let y = ev.number(a, 1)?;
            num(x.powf(y))
        }
        "SQRT" => {
            let x = ev.number(a, 0)?;
            if x < 0.0 { Err(E::Num) } else { num(x.sqrt()) }
        }
        "EXP" => num(ev.number(a, 0)?.exp()),
        "LN" => {
            let x = ev.number(a, 0)?;
            if x <= 0.0 { Err(E::Num) } else { num(x.ln()) }
        }
        "LOG10" => {
            let x = ev.number(a, 0)?;
            if x <= 0.0 { Err(E::Num) } else { num(x.log10()) }
        }
        "LOG" => {
            let x = ev.number(a, 0)?;
            let b = ev.number_or(a, 1, 10.0)?;
            if x <= 0.0 || b <= 0.0 || b == 1.0 { Err(E::Num) } else { num(x.ln() / b.ln()) }
        }
        "PI" => num(std::f64::consts::PI),
        "RAND" => num(ev.random()),
        "RANDBETWEEN" => {
            let lo = ev.number(a, 0)?.ceil();
            let hi = ev.number(a, 1)?.floor();
            if hi < lo {
                return Err(E::Num);
            }
            let r = ev.random();
            num(lo + (r * (hi - lo + 1.0)).floor())
        }
        "SIN" => num(ev.number(a, 0)?.sin()),
        "COS" => num(ev.number(a, 0)?.cos()),
        "TAN" => num(ev.number(a, 0)?.tan()),
        "ASIN" => num(ev.number(a, 0)?.asin()),
        "ACOS" => num(ev.number(a, 0)?.acos()),
        "ATAN" => num(ev.number(a, 0)?.atan()),
        "ATAN2" => {
            let x = ev.number(a, 0)?;
            let y = ev.number(a, 1)?;
            if x == 0.0 && y == 0.0 { Err(E::Div0) } else { num(y.atan2(x)) }
        }
        "DEGREES" => num(ev.number(a, 0)?.to_degrees()),
        "RADIANS" => num(ev.number(a, 0)?.to_radians()),
        "FACT" => {
            let x = ev.number(a, 0)?.floor();
            if x < 0.0 {
                return Err(E::Num);
            }
            num((1..=x as u64).fold(1.0, |acc, k| acc * k as f64))
        }
        "EVEN" | "ODD" => {
            let x = ev.number(a, 0)?;
            let s = if x < 0.0 { -1.0 } else { 1.0 };
            let mut v = x.abs().ceil();
            let want_even = name == "EVEN";
            if (v % 2.0 == 0.0) != want_even {
                v += 1.0;
            }
            if !want_even && v == 0.0 {
                v = 1.0;
            }
            num(s * v)
        }
        "GCD" => num(ev.numbers(a, false)?.into_iter().fold(0.0, gcd)),
        "LCM" => num(ev.numbers(a, false)?.into_iter().fold(1.0, |acc, x| {
            let x = x.floor();
            if x == 0.0 || acc == 0.0 { 0.0 } else { acc * x / gcd(acc, x) }
        })),
        "SUMPRODUCT" => {
            let mut acc: Option<Vec<Vec<Value>>> = None;
            for i in 0..n {
                let v = ev.arg(a, i)?;
                let g = ev.grid(v);
                acc = Some(match acc {
                    None => g,
                    Some(prev) => {
                        if prev.len() != g.len() || prev.first().map(|r| r.len()) != g.first().map(|r| r.len()) {
                            return Err(E::Value);
                        }
                        super::eval::broadcast(&prev, &g, |x, y| {
                            let fx = if let Value::Number(n) = x { *n } else if let Value::Error(e) = x { return Value::Error(*e) } else { 0.0 };
                            let fy = if let Value::Number(n) = y { *n } else if let Value::Error(e) = y { return Value::Error(*e) } else { 0.0 };
                            Value::Number(fx * fy)
                        })
                    }
                });
            }
            let mut sum = 0.0;
            for row in acc.unwrap_or_default() {
                for v in row {
                    match v {
                        Value::Number(x) => sum += x,
                        Value::Bool(true) if n == 1 => sum += 1.0,
                        Value::Error(e) => return Err(e),
                        _ => {}
                    }
                }
            }
            num(sum)
        }
        "SUBTOTAL" => {
            let f = ev.number(a, 0)? as i64;
            let skip = f > 100;
            let f = f % 100;
            let nums = ev.numbers(&a[1.min(n)..], skip)?;
            subtotal(f, nums)
        }
        "SEQUENCE" => {
            let rows = ev.number(a, 0)? as usize;
            let cols = ev.number_or(a, 1, 1.0)? as usize;
            let start = ev.number_or(a, 2, 1.0)?;
            let step = ev.number_or(a, 3, 1.0)?;
            if rows == 0 || cols == 0 || rows * cols > 1_000_000 {
                return Err(E::Calc);
            }
            Ok(EV::A((0..rows).map(|r| (0..cols).map(|c| Value::Number(start + step * (r * cols + c) as f64)).collect()).collect()))
        }

        // ----- Conditional aggregates -----
        "SUMIF" | "AVERAGEIF" => {
            need(a, 2, 3)?;
            let rv = ev.arg(a, 0)?;
            let g = ev.grid(rv);
            let (rows, cols) = (g.len(), g.first().map_or(0, |r| r.len()));
            let crit = parse_criteria(&ev.value(a, 1)?);
            let sg = if ev.has(a, 2) {
                let sv = ev.arg(a, 2)?;
                grid_shaped(ev, sv, rows, cols)
            } else {
                g.clone()
            };
            let (mut sum, mut cnt) = (0.0, 0.0);
            for r in 0..rows {
                for c in 0..cols {
                    if crit_match(&crit, &get(&g, r, c)) {
                        match get(&sg, r, c) {
                            Value::Number(x) => {
                                sum += x;
                                cnt += 1.0;
                            }
                            Value::Error(e) => return Err(e),
                            _ => {}
                        }
                    }
                }
            }
            if name == "SUMIF" { num(sum) } else if cnt == 0.0 { Err(E::Div0) } else { num(sum / cnt) }
        }
        "SUMIFS" | "AVERAGEIFS" | "MAXIFS" | "MINIFS" => {
            if n < 3 {
                return Err(E::Value);
            }
            let sv = ev.arg(a, 0)?;
            let sg = ev.grid(sv);
            let (rows, cols) = (sg.len(), sg.first().map_or(0, |r| r.len()));
            let mask = criteria_mask(ev, a, 1, rows, cols)?;
            let mut vals = Vec::new();
            for r in 0..rows {
                for c in 0..cols {
                    if mask[r][c] {
                        match get(&sg, r, c) {
                            Value::Number(x) => vals.push(x),
                            Value::Error(e) => return Err(e),
                            _ => {}
                        }
                    }
                }
            }
            match name {
                "SUMIFS" => num(vals.iter().sum()),
                "AVERAGEIFS" => {
                    if vals.is_empty() { Err(E::Div0) } else { num(vals.iter().sum::<f64>() / vals.len() as f64) }
                }
                "MAXIFS" => num(vals.iter().copied().fold(f64::NEG_INFINITY, f64::max).max(if vals.is_empty() { 0.0 } else { f64::NEG_INFINITY })),
                _ => num(if vals.is_empty() { 0.0 } else { vals.iter().copied().fold(f64::INFINITY, f64::min) }),
            }
        }
        "COUNTIF" => {
            need(a, 2, 2)?;
            let rv = ev.arg(a, 0)?;
            let g = ev.grid(rv);
            let crit = parse_criteria(&ev.value(a, 1)?);
            num(g.iter().flatten().filter(|v| crit_match(&crit, v)).count() as f64)
        }
        "COUNTIFS" => {
            if n < 2 {
                return Err(E::Value);
            }
            let rv = ev.arg(a, 0)?;
            let g = ev.grid(rv);
            let (rows, cols) = (g.len(), g.first().map_or(0, |r| r.len()));
            let mask = criteria_mask(ev, a, 0, rows, cols)?;
            num(mask.iter().flatten().filter(|m| **m).count() as f64)
        }

        // ----- Statistics -----
        "AVERAGE" => {
            let v = ev.numbers(a, false)?;
            if v.is_empty() { Err(E::Div0) } else { num(v.iter().sum::<f64>() / v.len() as f64) }
        }
        "AVERAGEA" | "MAXA" | "MINA" => {
            let mut v = Vec::new();
            for i in 0..n {
                let x = ev.arg(a, i)?;
                ev.walk(x, false, &mut |val, _| {
                    match val {
                        Value::Number(x) => v.push(x),
                        Value::Bool(b) => v.push(if b { 1.0 } else { 0.0 }),
                        Value::Text(_) => v.push(0.0),
                        Value::Error(e) => return Err(e),
                        Value::Empty => {}
                    }
                    Ok(())
                })?;
            }
            match name {
                "AVERAGEA" => {
                    if v.is_empty() { Err(E::Div0) } else { num(v.iter().sum::<f64>() / v.len() as f64) }
                }
                "MAXA" => num(v.iter().copied().fold(f64::NEG_INFINITY, f64::max).max(if v.is_empty() { 0.0 } else { f64::NEG_INFINITY })),
                _ => num(if v.is_empty() { 0.0 } else { v.iter().copied().fold(f64::INFINITY, f64::min) }),
            }
        }
        "MAX" => {
            let v = ev.numbers(a, false)?;
            num(if v.is_empty() { 0.0 } else { v.iter().copied().fold(f64::NEG_INFINITY, f64::max) })
        }
        "MIN" => {
            let v = ev.numbers(a, false)?;
            num(if v.is_empty() { 0.0 } else { v.iter().copied().fold(f64::INFINITY, f64::min) })
        }
        "MEDIAN" => percentile(ev.numbers(a, false)?, 0.5).and_then(num),
        "MODE" | "MODE.SNGL" => {
            let v = ev.numbers(a, false)?;
            let mut best = None;
            let mut best_n = 1;
            for (i, x) in v.iter().enumerate() {
                let c = v.iter().filter(|y| *y == x).count();
                if c > best_n || (c == best_n && best.is_none() && c > 1) {
                    best_n = c;
                    best = Some(i);
                }
            }
            match best {
                Some(i) => num(v[i]),
                None => Err(E::NA),
            }
        }
        "STDEV" | "STDEV.S" => num(stats(&ev.numbers(a, false)?, true)?.sqrt()),
        "STDEVP" | "STDEV.P" => num(stats(&ev.numbers(a, false)?, false)?.sqrt()),
        "VAR" | "VAR.S" => num(stats(&ev.numbers(a, false)?, true)?),
        "VARP" | "VAR.P" => num(stats(&ev.numbers(a, false)?, false)?),
        "LARGE" | "SMALL" => {
            let mut v = ev.numbers(&a[..1.min(n)], false)?;
            let k = ev.number(a, 1)? as usize;
            if k == 0 || k > v.len() {
                return Err(E::Num);
            }
            v.sort_by(|x, y| x.partial_cmp(y).unwrap_or(Ordering::Equal));
            if name == "LARGE" { num(v[v.len() - k]) } else { num(v[k - 1]) }
        }
        "PERCENTILE" | "PERCENTILE.INC" => {
            let v = ev.numbers(&a[..1.min(n)], false)?;
            num(percentile(v, ev.number(a, 1)?)?)
        }
        "QUARTILE" | "QUARTILE.INC" => {
            let v = ev.numbers(&a[..1.min(n)], false)?;
            let q = ev.number(a, 1)?.floor();
            if !(0.0..=4.0).contains(&q) {
                return Err(E::Num);
            }
            num(percentile(v, q / 4.0)?)
        }
        "RANK" | "RANK.EQ" => {
            let x = ev.number(a, 0)?;
            let v = ev.numbers(&a[1..2.min(n)], false)?;
            let desc = ev.number_or(a, 2, 0.0)? == 0.0;
            if !v.contains(&x) {
                return Err(E::NA);
            }
            let rank = v.iter().filter(|y| if desc { **y > x } else { **y < x }).count() + 1;
            num(rank as f64)
        }
        "CORREL" => {
            let x = ev.arg(a, 0)?;
            let gx: Vec<Value> = ev.grid(x).into_iter().flatten().collect();
            let y = ev.arg(a, 1)?;
            let gy: Vec<Value> = ev.grid(y).into_iter().flatten().collect();
            let pairs: Vec<(f64, f64)> = gx
                .iter()
                .zip(gy.iter())
                .filter_map(|(a, b)| match (a, b) {
                    (Value::Number(x), Value::Number(y)) => Some((*x, *y)),
                    _ => None,
                })
                .collect();
            let nn = pairs.len() as f64;
            if nn < 2.0 {
                return Err(E::Div0);
            }
            let mx = pairs.iter().map(|p| p.0).sum::<f64>() / nn;
            let my = pairs.iter().map(|p| p.1).sum::<f64>() / nn;
            let cov: f64 = pairs.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
            let sx: f64 = pairs.iter().map(|p| (p.0 - mx).powi(2)).sum::<f64>().sqrt();
            let sy: f64 = pairs.iter().map(|p| (p.1 - my).powi(2)).sum::<f64>().sqrt();
            if sx == 0.0 || sy == 0.0 { Err(E::Div0) } else { num(cov / (sx * sy)) }
        }
        "COUNT" => {
            let mut c = 0.0;
            for i in 0..n {
                let Ok(v) = ev.arg(a, i) else { continue };
                let _ = ev.walk(v, false, &mut |val, from_ref| {
                    match val {
                        Value::Number(_) => c += 1.0,
                        Value::Bool(_) if !from_ref => c += 1.0,
                        Value::Text(s) if !from_ref && numfmt::parse_number(&s).is_some() => c += 1.0,
                        _ => {}
                    }
                    Ok(())
                });
            }
            num(c)
        }
        "COUNTA" => {
            let mut c = 0.0;
            for i in 0..n {
                match ev.arg(a, i) {
                    Ok(v) => {
                        let _ = ev.walk(v, false, &mut |val, _| {
                            if !val.is_empty() {
                                c += 1.0;
                            }
                            Ok(())
                        });
                    }
                    Err(_) => c += 1.0,
                }
            }
            num(c)
        }
        "COUNTBLANK" => {
            let v = ev.arg(a, 0)?;
            let total = match &v {
                EV::R(_, rg) => rg.rows() as f64 * rg.cols() as f64,
                _ => 1.0,
            };
            let mut filled = 0.0;
            ev.walk(v, false, &mut |val, _| {
                if !val.is_empty() && !matches!(&val, Value::Text(s) if s.is_empty()) {
                    filled += 1.0;
                }
                Ok(())
            })?;
            num(total - filled)
        }

        // ----- Logical -----
        "TRUE" => boolean(true),
        "FALSE" => boolean(false),
        "IF" => {
            need(a, 1, 3)?;
            let c = ev.boolean(a, 0)?;
            if c {
                if n >= 2 { ev.eval(&a[1]).map(|v| missing_zero(&a[1], v)) } else { boolean(true) }
            } else if n >= 3 {
                ev.eval(&a[2]).map(|v| missing_zero(&a[2], v))
            } else {
                boolean(false)
            }
        }
        "IFS" => {
            let mut i = 0;
            while i + 1 < n {
                if ev.boolean(a, i)? {
                    return ev.eval(&a[i + 1]);
                }
                i += 2;
            }
            Err(E::NA)
        }
        "IFERROR" | "IFNA" => {
            need(a, 2, 2)?;
            let only_na = name == "IFNA";
            let r = ev.eval(&a[0]);
            let failed = match &r {
                Err(e) => !only_na || *e == E::NA,
                Ok(v) if !ev.is_multi(v) => match ev.scalar(v.clone()) {
                    Err(e) => !only_na || e == E::NA,
                    Ok(_) => false,
                },
                Ok(_) => false,
            };
            if failed { ev.eval(&a[1]) } else { r }
        }
        "AND" | "OR" | "XOR" => {
            let mut vals = Vec::new();
            for i in 0..n {
                let v = ev.arg(a, i)?;
                ev.walk(v, false, &mut |val, from_ref| {
                    match val {
                        Value::Bool(b) => vals.push(b),
                        Value::Number(x) => vals.push(x != 0.0),
                        Value::Error(e) => return Err(e),
                        Value::Text(s) if !from_ref => vals.push(to_bool(&Value::Text(s))?),
                        _ => {}
                    }
                    Ok(())
                })?;
            }
            if vals.is_empty() {
                return Err(E::Value);
            }
            boolean(match name {
                "AND" => vals.iter().all(|b| *b),
                "OR" => vals.iter().any(|b| *b),
                _ => vals.iter().filter(|b| **b).count() % 2 == 1,
            })
        }
        "NOT" => boolean(!ev.boolean(a, 0)?),
        "SWITCH" => {
            if n < 3 {
                return Err(E::Value);
            }
            let key = ev.value(a, 0)?;
            let mut i = 1;
            while i + 1 < n {
                let v = ev.value(a, i)?;
                if values_equal(&key, &v) {
                    return ev.eval(&a[i + 1]);
                }
                i += 2;
            }
            if i < n { ev.eval(&a[i]) } else { Err(E::NA) }
        }

        // ----- Information -----
        "ISBLANK" => Ok(EV::V(Value::Bool(matches!(ev.value(a, 0), Ok(Value::Empty))))),
        "ISNUMBER" => boolean(matches!(ev.value(a, 0), Ok(Value::Number(_)))),
        "ISTEXT" => boolean(matches!(ev.value(a, 0), Ok(Value::Text(_)))),
        "ISNONTEXT" => boolean(!matches!(ev.value(a, 0), Ok(Value::Text(_)))),
        "ISLOGICAL" => boolean(matches!(ev.value(a, 0), Ok(Value::Bool(_)))),
        "ISERROR" => boolean(ev.value(a, 0).is_err()),
        "ISERR" => boolean(matches!(ev.value(a, 0), Err(e) if e != E::NA)),
        "ISNA" => boolean(matches!(ev.value(a, 0), Err(E::NA))),
        "ISEVEN" => boolean((ev.number(a, 0)?.trunc() as i64) % 2 == 0),
        "ISODD" => boolean((ev.number(a, 0)?.trunc() as i64) % 2 != 0),
        "ISFORMULA" => match ev.arg(a, 0)? {
            EV::R(s, rg) => boolean(ev.wb.sheets[s].get(rg.r1, rg.c1).is_some_and(|c| c.formula.is_some())),
            _ => Err(E::Value),
        },
        "NA" => Err(E::NA),
        "ERROR.TYPE" => match ev.value(a, 0) {
            Err(e) => num(e.code()),
            Ok(_) => Err(E::NA),
        },
        "TYPE" => num(match ev.value(a, 0) {
            Ok(Value::Number(_)) | Ok(Value::Empty) => 1.0,
            Ok(Value::Text(_)) => 2.0,
            Ok(Value::Bool(_)) => 4.0,
            Ok(Value::Error(_)) | Err(_) => 16.0,
        }),
        "N" => num(match ev.value(a, 0)? {
            Value::Number(x) => x,
            Value::Bool(b) => if b { 1.0 } else { 0.0 },
            _ => 0.0,
        }),
        "T" => text(match ev.value(a, 0)? {
            Value::Text(s) => s,
            _ => String::new(),
        }),

        // ----- Text -----
        "CONCATENATE" => {
            let mut s = String::new();
            for i in 0..n {
                s.push_str(&ev.string(a, i)?);
            }
            text(s)
        }
        "CONCAT" | "TEXTJOIN" => {
            let (start, delim, ignore) = if name == "TEXTJOIN" { (2, ev.string(a, 0)?, ev.boolean(a, 1)?) } else { (0, String::new(), false) };
            let mut parts = Vec::new();
            for i in start..n {
                let v = ev.arg(a, i)?;
                ev.walk(v, false, &mut |val, _| {
                    let t = to_text(&val)?;
                    if !(ignore && t.is_empty()) {
                        parts.push(t);
                    }
                    Ok(())
                })?;
            }
            text(parts.join(&delim))
        }
        "LEFT" | "RIGHT" => {
            let s: Vec<char> = ev.string(a, 0)?.chars().collect();
            let k = ev.number_or(a, 1, 1.0)?;
            if k < 0.0 {
                return Err(E::Value);
            }
            let k = (k as usize).min(s.len());
            text(if name == "LEFT" { s[..k].iter().collect::<String>() } else { s[s.len() - k..].iter().collect() })
        }
        "MID" => {
            let s: Vec<char> = ev.string(a, 0)?.chars().collect();
            let st = ev.number(a, 1)?;
            let k = ev.number(a, 2)?;
            if st < 1.0 || k < 0.0 {
                return Err(E::Value);
            }
            let st = (st as usize - 1).min(s.len());
            let end = (st + k as usize).min(s.len());
            text(s[st..end].iter().collect::<String>())
        }
        "LEN" => num(ev.string(a, 0)?.chars().count() as f64),
        "UPPER" => text(ev.string(a, 0)?.to_uppercase()),
        "LOWER" => text(ev.string(a, 0)?.to_lowercase()),
        "PROPER" => {
            let s = ev.string(a, 0)?;
            let mut out = String::new();
            let mut prev_alpha = false;
            for ch in s.chars() {
                if prev_alpha { out.extend(ch.to_lowercase()) } else { out.extend(ch.to_uppercase()) }
                prev_alpha = ch.is_alphabetic();
            }
            text(out)
        }
        "TRIM" => text(ev.string(a, 0)?.split(' ').filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")),
        "CLEAN" => text(ev.string(a, 0)?.chars().filter(|c| (*c as u32) >= 32).collect::<String>()),
        "SUBSTITUTE" => {
            let s = ev.string(a, 0)?;
            let old = ev.string(a, 1)?;
            let new = ev.string(a, 2)?;
            if old.is_empty() {
                return text(s);
            }
            if ev.has(a, 3) {
                let inst = ev.number(a, 3)? as usize;
                if inst == 0 {
                    return Err(E::Value);
                }
                match s.match_indices(&old).nth(inst - 1) {
                    Some((i, _)) => text(format!("{}{}{}", &s[..i], new, &s[i + old.len()..])),
                    None => text(s),
                }
            } else {
                text(s.replace(&old, &new))
            }
        }
        "REPLACE" => {
            let s: Vec<char> = ev.string(a, 0)?.chars().collect();
            let st = (ev.number(a, 1)?.max(1.0) as usize - 1).min(s.len());
            let k = ev.number(a, 2)?.max(0.0) as usize;
            let new = ev.string(a, 3)?;
            let end = (st + k).min(s.len());
            text(format!("{}{}{}", s[..st].iter().collect::<String>(), new, s[end..].iter().collect::<String>()))
        }
        "FIND" | "SEARCH" => {
            let f = ev.string(a, 0)?;
            let s = ev.string(a, 1)?;
            let st = ev.number_or(a, 2, 1.0)?;
            let sc: Vec<char> = s.chars().collect();
            if st < 1.0 || st as usize > sc.len() + 1 {
                return Err(E::Value);
            }
            let st = st as usize - 1;
            if name == "FIND" {
                let hay: String = sc[st..].iter().collect();
                return match hay.find(&f) {
                    Some(b) => num((st + hay[..b].chars().count() + 1) as f64),
                    None => Err(E::Value),
                };
            }
            let pc: Vec<char> = f.chars().collect();
            if !has_wild(&f) {
                let hay: String = sc[st..].iter().collect::<String>().to_lowercase();
                return match hay.find(&f.to_lowercase()) {
                    Some(b) => num((st + hay[..b].chars().count() + 1) as f64),
                    None => Err(E::Value),
                };
            }
            for i in st..=sc.len() {
                for j in i..=sc.len() {
                    if wild_match(&pc, &sc[i..j]) {
                        return num((i + 1) as f64);
                    }
                }
            }
            Err(E::Value)
        }
        "REPT" => {
            let s = ev.string(a, 0)?;
            let k = ev.number(a, 1)?;
            if k < 0.0 || s.len() as f64 * k > 32767.0 {
                return Err(E::Value);
            }
            text(s.repeat(k as usize))
        }
        "EXACT" => boolean(ev.string(a, 0)? == ev.string(a, 1)?),
        "CHAR" | "UNICHAR" => {
            let k = ev.number(a, 0)? as u32;
            match char::from_u32(k) {
                Some(c) if k >= 1 => text(c.to_string()),
                _ => Err(E::Value),
            }
        }
        "CODE" | "UNICODE" => match ev.string(a, 0)?.chars().next() {
            Some(c) => num(c as u32 as f64),
            None => Err(E::Value),
        },
        "VALUE" => num(to_num(&Value::Text(ev.string(a, 0)?))?),
        "TEXT" => {
            let mut v = ev.value(a, 0)?;
            if let Value::Text(s) = &v {
                if let Value::Number(x) = numfmt::parse_input(s).0 {
                    v = Value::Number(x);
                }
            }
            let f = ev.string(a, 1)?;
            text(numfmt::format_value(&v, &f).text)
        }
        "FIXED" => {
            let x = ev.number(a, 0)?;
            let d = ev.number_or(a, 1, 2.0)?.max(0.0) as usize;
            let no_commas = ev.bool_or(a, 2, false)?;
            let fmt = format!("{}{}{}", if no_commas { "0" } else { "#,##0" }, if d > 0 { "." } else { "" }, "0".repeat(d));
            text(numfmt::format_value(&Value::Number(round_half_away(x, d as i32)), &fmt).text)
        }
        "TEXTBEFORE" | "TEXTAFTER" => {
            let s = ev.string(a, 0)?;
            let d = ev.string(a, 1)?;
            let inst = ev.number_or(a, 2, 1.0)? as i64;
            if d.is_empty() || inst == 0 {
                return Err(E::Value);
            }
            let idx: Vec<usize> = s.match_indices(&d).map(|(i, _)| i).collect();
            let pick = if inst > 0 { idx.get(inst as usize - 1) } else { idx.len().checked_sub((-inst) as usize).and_then(|k| idx.get(k)) };
            match pick {
                Some(&i) => text(if name == "TEXTBEFORE" { s[..i].to_string() } else { s[i + d.len()..].to_string() }),
                None => Err(E::NA),
            }
        }

        // ----- Date & time -----
        "TODAY" => num(ev.now.floor()),
        "NOW" => num(ev.now),
        "DATE" => {
            let y = ev.number(a, 0)?.trunc() as i64;
            let m = ev.number(a, 1)?.trunc() as i64;
            let d = ev.number(a, 2)?.trunc() as i64;
            let s = numfmt::ymd_to_serial(y, m, d);
            if s < 0.0 { Err(E::Num) } else { num(s) }
        }
        "TIME" => {
            let h = ev.number(a, 0)?;
            let m = ev.number(a, 1)?;
            let s = ev.number(a, 2)?;
            let t = (h * 3600.0 + m * 60.0 + s) / 86400.0;
            if t < 0.0 { Err(E::Num) } else { num(t.fract()) }
        }
        "YEAR" | "MONTH" | "DAY" => {
            let s = ev.number(a, 0)?;
            if s < 0.0 {
                return Err(E::Num);
            }
            let (y, m, d) = numfmt::serial_to_ymd(s);
            num(match name {
                "YEAR" => y,
                "MONTH" => m,
                _ => d,
            } as f64)
        }
        "HOUR" | "MINUTE" | "SECOND" => {
            let s = ev.number(a, 0)?;
            let (h, m, sec, _) = numfmt::serial_to_hms(s);
            num(match name {
                "HOUR" => h,
                "MINUTE" => m,
                _ => sec,
            } as f64)
        }
        "DATEVALUE" => match numfmt::parse_date(&ev.string(a, 0)?) {
            Some((s, _)) => num(s),
            None => Err(E::Value),
        },
        "TIMEVALUE" => match numfmt::parse_time(&ev.string(a, 0)?) {
            Some((s, _)) => num(s),
            None => Err(E::Value),
        },
        "WEEKDAY" => {
            let s = ev.number(a, 0)?;
            let t = ev.number_or(a, 1, 1.0)? as i64;
            let w = numfmt::weekday(s); // 1=Sun
            num(match t {
                1 | 17 => w,
                2 | 11 => (w + 5) % 7 + 1,
                3 => (w + 5) % 7,
                12..=16 => (w - (t - 9)).rem_euclid(7) + 1,
                _ => return Err(E::Num),
            } as f64)
        }
        "WEEKNUM" => {
            let s = ev.number(a, 0)?.floor();
            let t = ev.number_or(a, 1, 1.0)? as i64;
            let (y, _, _) = numfmt::serial_to_ymd(s);
            let jan1 = numfmt::ymd_to_serial(y, 1, 1);
            let start_dow = if t == 2 || t == 11 { 2 } else { 1 };
            let offset = (numfmt::weekday(jan1) - start_dow).rem_euclid(7);
            num(((s - jan1 + offset as f64) / 7.0).floor() + 1.0)
        }
        "EDATE" | "EOMONTH" => {
            let s = ev.number(a, 0)?;
            let m = ev.number(a, 1)?.trunc() as i64;
            num(add_months(s, m, name == "EOMONTH"))
        }
        "DAYS" => num(ev.number(a, 0)?.floor() - ev.number(a, 1)?.floor()),
        "DATEDIF" => {
            let s = ev.number(a, 0)?.floor();
            let e = ev.number(a, 1)?.floor();
            let unit = ev.string(a, 2)?.to_uppercase();
            if s > e {
                return Err(E::Num);
            }
            let (y1, m1, d1) = numfmt::serial_to_ymd(s);
            let (y2, m2, d2) = numfmt::serial_to_ymd(e);
            let mut months = (y2 - y1) * 12 + (m2 - m1);
            if d2 < d1 {
                months -= 1;
            }
            num(match unit.as_str() {
                "Y" => (months / 12) as f64,
                "M" => months as f64,
                "D" => e - s,
                "YM" => (months % 12) as f64,
                "MD" => {
                    if d2 >= d1 {
                        (d2 - d1) as f64
                    } else {
                        let (py, pm) = if m2 == 1 { (y2 - 1, 12) } else { (y2, m2 - 1) };
                        (numfmt::days_in_month(py, pm) - d1 + d2) as f64
                    }
                }
                "YD" => {
                    let anchor = numfmt::ymd_to_serial(if (m2, d2) >= (m1, d1) { y2 } else { y2 - 1 }, m1, d1);
                    e - anchor
                }
                _ => return Err(E::Num),
            })
        }
        "NETWORKDAYS" | "WORKDAY" => {
            let s = ev.number(a, 0)?.floor();
            let mut holidays = Vec::new();
            if ev.has(a, 2) {
                holidays = ev.numbers(&a[2..3], false)?.into_iter().map(f64::floor).collect();
            }
            let is_off = |d: f64| is_weekend(d) || holidays.contains(&d);
            if name == "NETWORKDAYS" {
                let e = ev.number(a, 1)?.floor();
                let (lo, hi, sign) = if s <= e { (s, e, 1.0) } else { (e, s, -1.0) };
                if hi - lo > 1_000_000.0 {
                    return Err(E::Num);
                }
                let mut c = 0.0;
                let mut d = lo;
                while d <= hi {
                    if !is_off(d) {
                        c += 1.0;
                    }
                    d += 1.0;
                }
                num(c * sign)
            } else {
                let days = ev.number(a, 1)?.trunc() as i64;
                let step = if days >= 0 { 1.0 } else { -1.0 };
                let mut left = days.abs();
                let mut d = s;
                while left > 0 {
                    d += step;
                    if !is_off(d) {
                        left -= 1;
                    }
                }
                num(d)
            }
        }

        // ----- Financial -----
        "PMT" => {
            let r = ev.number(a, 0)?;
            let np = ev.number(a, 1)?;
            let pv = ev.number(a, 2)?;
            let f = ev.number_or(a, 3, 0.0)?;
            let t = ev.number_or(a, 4, 0.0)?;
            if np == 0.0 { Err(E::Num) } else { num(pmt(r, np, pv, f, t)) }
        }
        "IPMT" | "PPMT" => {
            let r = ev.number(a, 0)?;
            let per = ev.number(a, 1)?;
            let np = ev.number(a, 2)?;
            let pv = ev.number(a, 3)?;
            let f = ev.number_or(a, 4, 0.0)?;
            let t = ev.number_or(a, 5, 0.0)?;
            if per < 1.0 || per > np {
                return Err(E::Num);
            }
            let i = ipmt(r, per, np, pv, f, t);
            if name == "IPMT" { num(i) } else { num(pmt(r, np, pv, f, t) - i) }
        }
        "FV" => {
            let r = ev.number(a, 0)?;
            let np = ev.number(a, 1)?;
            let p = ev.number(a, 2)?;
            let pv = ev.number_or(a, 3, 0.0)?;
            let t = ev.number_or(a, 4, 0.0)?;
            num(fv(r, np, p, pv, t))
        }
        "PV" => {
            let r = ev.number(a, 0)?;
            let np = ev.number(a, 1)?;
            let p = ev.number(a, 2)?;
            let f = ev.number_or(a, 3, 0.0)?;
            let t = ev.number_or(a, 4, 0.0)?;
            if r == 0.0 {
                return num(-(f + p * np));
            }
            let q = (1.0 + r).powf(np);
            num(-(f + p * (1.0 + r * t) * (q - 1.0) / r) / q)
        }
        "NPER" => {
            let r = ev.number(a, 0)?;
            let p = ev.number(a, 1)?;
            let pv = ev.number(a, 2)?;
            let f = ev.number_or(a, 3, 0.0)?;
            let t = ev.number_or(a, 4, 0.0)?;
            if r == 0.0 {
                if p == 0.0 {
                    return Err(E::Num);
                }
                return num(-(pv + f) / p);
            }
            let z = p * (1.0 + r * t) / r;
            let v = ((z - f) / (pv + z)).ln() / (1.0 + r).ln();
            num(v)
        }
        "NPV" => {
            let r = ev.number(a, 0)?;
            let vals = ev.numbers(&a[1.min(n)..], false)?;
            num(vals.iter().enumerate().map(|(i, v)| v / (1.0 + r).powi(i as i32 + 1)).sum())
        }
        "IRR" => {
            let vals = ev.numbers(&a[..1.min(n)], false)?;
            let mut r = ev.number_or(a, 1, 0.1)?;
            for _ in 0..100 {
                let (mut f, mut df) = (0.0, 0.0);
                for (i, v) in vals.iter().enumerate() {
                    let d = (1.0 + r).powi(i as i32);
                    f += v / d;
                    df -= i as f64 * v / (d * (1.0 + r));
                }
                if df == 0.0 {
                    break;
                }
                let nr = r - f / df;
                if (nr - r).abs() < 1e-10 {
                    return num(nr);
                }
                r = nr;
            }
            Err(E::Num)
        }
        "RATE" => {
            let np = ev.number(a, 0)?;
            let p = ev.number(a, 1)?;
            let pv = ev.number(a, 2)?;
            let f = ev.number_or(a, 3, 0.0)?;
            let t = ev.number_or(a, 4, 0.0)?;
            let mut r = ev.number_or(a, 5, 0.1)?;
            let g = |r: f64| f - fv(r, np, p, pv, t);
            for _ in 0..100 {
                let y = g(r);
                let h = 1e-7;
                let dy = (g(r + h) - y) / h;
                if dy == 0.0 {
                    break;
                }
                let nr = r - y / dy;
                if (nr - r).abs() < 1e-10 {
                    return num(nr);
                }
                r = nr;
            }
            Err(E::Num)
        }

        // ----- Lookup & reference -----
        "ROW" | "COLUMN" => {
            if n == 0 || !ev.has(a, 0) {
                let (_, r, c) = ev.cur;
                return num(if name == "ROW" { r + 1 } else { c + 1 } as f64);
            }
            match ev.arg(a, 0)? {
                EV::R(_, rg) => num(if name == "ROW" { rg.r1 + 1 } else { rg.c1 + 1 } as f64),
                _ => Err(E::Value),
            }
        }
        "ROWS" | "COLUMNS" => match ev.arg(a, 0)? {
            EV::R(_, rg) => num(if name == "ROWS" { rg.rows() } else { rg.cols() } as f64),
            EV::A(g) => num(if name == "ROWS" { g.len() } else { g.first().map_or(0, |r| r.len()) } as f64),
            EV::V(_) => num(1.0),
        },
        "CHOOSE" => {
            let i = ev.number(a, 0)?.trunc() as usize;
            if i < 1 || i >= n {
                return Err(E::Value);
            }
            ev.eval(&a[i])
        }
        "INDEX" => {
            need(a, 2, 4)?;
            let v = ev.arg(a, 0)?;
            let mut r = ev.number_or(a, 1, 0.0)?.trunc() as i64;
            let mut c = ev.number_or(a, 2, 0.0)?.trunc() as i64;
            match v {
                EV::R(s, rg) => {
                    if rg.rows() == 1 && !ev.has(a, 2) {
                        c = r;
                        r = 1;
                    }
                    if r < 0 || c < 0 || r > rg.rows() as i64 || c > rg.cols() as i64 {
                        return Err(E::Ref);
                    }
                    let (r1, r2) = if r == 0 { (rg.r1, rg.r2) } else { (rg.r1 + r as u32 - 1, rg.r1 + r as u32 - 1) };
                    let (c1, c2) = if c == 0 { (rg.c1, rg.c2) } else { (rg.c1 + c as u32 - 1, rg.c1 + c as u32 - 1) };
                    Ok(EV::R(s, Range::new(r1, c1, r2, c2)))
                }
                other => {
                    let g = ev.grid(other);
                    if g.len() == 1 && !ev.has(a, 2) {
                        c = r;
                        r = 1;
                    }
                    let r = if r == 0 { 1 } else { r };
                    let c = if c == 0 { 1 } else { c };
                    g.get(r as usize - 1).and_then(|row| row.get(c as usize - 1)).cloned().map(EV::V).ok_or(E::Ref)
                }
            }
        }
        "OFFSET" => {
            need(a, 3, 5)?;
            let EV::R(s, rg) = ev.arg(a, 0)? else { return Err(E::Value) };
            let dr = ev.number(a, 1)?.trunc() as i64;
            let dc = ev.number(a, 2)?.trunc() as i64;
            let h = if ev.has(a, 3) { ev.number(a, 3)?.trunc() as i64 } else { rg.rows() as i64 };
            let w = if ev.has(a, 4) { ev.number(a, 4)?.trunc() as i64 } else { rg.cols() as i64 };
            let r1 = rg.r1 as i64 + dr;
            let c1 = rg.c1 as i64 + dc;
            if r1 < 0 || c1 < 0 || h < 1 || w < 1 {
                return Err(E::Ref);
            }
            let r2 = r1 + h - 1;
            let c2 = c1 + w - 1;
            if r2 >= crate::model::MAX_ROWS as i64 || c2 >= crate::model::MAX_COLS as i64 {
                return Err(E::Ref);
            }
            Ok(EV::R(s, Range::new(r1 as u32, c1 as u32, r2 as u32, c2 as u32)))
        }
        "INDIRECT" => {
            let t = ev.string(a, 0)?;
            match super::parse(&t) {
                Ok(Expr::Ref(r)) => {
                    let (s, rg) = ev.resolve_ref(&r)?;
                    let _ = RefKind::Cell;
                    Ok(EV::R(s, rg))
                }
                _ => Err(E::Ref),
            }
        }
        "VLOOKUP" | "HLOOKUP" => {
            need(a, 3, 4)?;
            let key = ev.value(a, 0)?;
            let tv = ev.arg(a, 1)?;
            let full = match &tv {
                EV::R(_, rg) => Some(*rg),
                _ => None,
            };
            let mut g = ev.grid(tv);
            if name == "HLOOKUP" {
                g = transpose(g);
            }
            let idx = ev.number(a, 2)?.trunc() as i64;
            let width = match full {
                Some(rg) => (if name == "VLOOKUP" { rg.cols() } else { rg.rows() }) as i64,
                None => g.first().map_or(0, |r| r.len()) as i64,
            };
            if idx < 1 {
                return Err(E::Value);
            }
            if idx > width {
                return Err(E::Ref);
            }
            let approx = ev.bool_or(a, 3, true)?;
            let keys: Vec<Value> = g.iter().map(|r| r.first().cloned().unwrap_or_default()).collect();
            let hit = if approx { lookup_approx(&keys, &key) } else { lookup_exact(&keys, &key, true, false) };
            match hit {
                Some(i) => Ok(EV::V(get(&g, i, idx as usize - 1))),
                None => Err(E::NA),
            }
        }
        "MATCH" => {
            need(a, 2, 3)?;
            let key = ev.value(a, 0)?;
            let v = ev.arg(a, 1)?;
            let vals = vector(ev.grid(v));
            let t = ev.number_or(a, 2, 1.0)? as i64;
            let hit = match t {
                0 => lookup_exact(&vals, &key, true, false),
                1 => lookup_approx(&vals, &key),
                _ => {
                    let mut best = None;
                    for (i, v) in vals.iter().enumerate() {
                        if std::mem::discriminant(v) != std::mem::discriminant(&key) {
                            continue;
                        }
                        if compare(v, &key) != Ordering::Less {
                            best = Some(i);
                        } else {
                            break;
                        }
                    }
                    best
                }
            };
            hit.map(|i| EV::V(Value::Number(i as f64 + 1.0))).ok_or(E::NA)
        }
        "XMATCH" => {
            let key = ev.value(a, 0)?;
            let v = ev.arg(a, 1)?;
            let vals = vector(ev.grid(v));
            let mode = ev.number_or(a, 2, 0.0)? as i64;
            let rev = ev.number_or(a, 3, 1.0)? < 0.0;
            lookup_mode(&vals, &key, mode, rev).map(|i| EV::V(Value::Number(i as f64 + 1.0))).ok_or(E::NA)
        }
        "XLOOKUP" => {
            need(a, 3, 6)?;
            let key = ev.value(a, 0)?;
            let lv = ev.arg(a, 1)?;
            let lg = ev.grid(lv);
            let vertical = lg.len() > 1 || lg.first().map_or(0, |r| r.len()) == 1;
            let vals: Vec<Value> = if vertical { lg.iter().map(|r| r.first().cloned().unwrap_or_default()).collect() } else { lg[0].clone() };
            let mode = ev.number_or(a, 4, 0.0)? as i64;
            let rev = ev.number_or(a, 5, 1.0)? < 0.0;
            match lookup_mode(&vals, &key, mode, rev) {
                Some(i) => match ev.arg(a, 2)? {
                    EV::R(s, rg) => {
                        let i = i as u32;
                        Ok(EV::R(s, if vertical { Range::new(rg.r1 + i, rg.c1, rg.r1 + i, rg.c2) } else { Range::new(rg.r1, rg.c1 + i, rg.r2, rg.c1 + i) }))
                    }
                    other => {
                        let g = ev.grid(other);
                        Ok(EV::A(if vertical { vec![g.get(i).cloned().ok_or(E::NA)?] } else { g.iter().map(|r| vec![r.get(i).cloned().unwrap_or_default()]).collect() }))
                    }
                },
                None => {
                    if ev.has(a, 3) { ev.eval(&a[3]) } else { Err(E::NA) }
                }
            }
        }
        "TRANSPOSE" => {
            let v = ev.arg(a, 0)?;
            let g = ev.grid(v);
            Ok(EV::A(transpose(g)))
        }
        "SORT" => {
            let v = ev.arg(a, 0)?;
            let g = ev.grid(v);
            let idx = ev.number_or(a, 1, 1.0)? as usize;
            let desc = ev.number_or(a, 2, 1.0)? < 0.0;
            let by_col = ev.bool_or(a, 3, false)?;
            if idx < 1 {
                return Err(E::Value);
            }
            let out = if by_col { transpose(sort_grid(transpose(g), idx - 1, desc)) } else { sort_grid(g, idx - 1, desc) };
            Ok(EV::A(out))
        }
        "UNIQUE" => {
            let v = ev.arg(a, 0)?;
            let g = ev.grid(v);
            let once = ev.bool_or(a, 2, false)?;
            let key = |r: &Vec<Value>| r.iter().map(|v| numfmt::value_to_text(v).to_lowercase()).collect::<Vec<_>>().join("\u{1}");
            let mut out: Vec<Vec<Value>> = Vec::new();
            let mut seen: Vec<String> = Vec::new();
            for row in &g {
                let k = key(row);
                if once {
                    if g.iter().filter(|r| key(r) == k).count() == 1 {
                        out.push(row.clone());
                    }
                } else if !seen.contains(&k) {
                    seen.push(k);
                    out.push(row.clone());
                }
            }
            if out.is_empty() { Err(E::Calc) } else { Ok(EV::A(out)) }
        }
        "FILTER" => {
            let v = ev.arg(a, 0)?;
            let g = ev.grid(v);
            let m = ev.arg(a, 1)?;
            let mg = ev.grid(m);
            let mut out = Vec::new();
            for (i, row) in g.iter().enumerate() {
                let keep = match mg.get(i).and_then(|r| r.first()) {
                    Some(v) => to_bool(v).unwrap_or(false),
                    None => false,
                };
                if keep {
                    out.push(row.clone());
                }
            }
            if out.is_empty() {
                if ev.has(a, 2) { ev.eval(&a[2]) } else { Err(E::Calc) }
            } else {
                Ok(EV::A(out))
            }
        }
        _ => {
            let _ = is_ref;
            Err(E::Name)
        }
    }
}

fn missing_zero(e: &Expr, v: EV) -> EV {
    if matches!(e, Expr::Missing) { EV::V(Value::Number(0.0)) } else { v }
}

fn subtotal(f: i64, v: Vec<f64>) -> Res {
    let cnt = v.len() as f64;
    match f {
        1 => if cnt == 0.0 { Err(E::Div0) } else { num(v.iter().sum::<f64>() / cnt) },
        2 | 3 => num(cnt),
        4 => num(if v.is_empty() { 0.0 } else { v.iter().copied().fold(f64::NEG_INFINITY, f64::max) }),
        5 => num(if v.is_empty() { 0.0 } else { v.iter().copied().fold(f64::INFINITY, f64::min) }),
        6 => num(v.iter().product()),
        7 => num(stats(&v, true)?.sqrt()),
        8 => num(stats(&v, false)?.sqrt()),
        9 => num(v.iter().sum()),
        10 => num(stats(&v, true)?),
        11 => num(stats(&v, false)?),
        _ => Err(E::Value),
    }
}

#[cfg(test)]
mod tests {
    use crate::formula::eval_formula;
    use crate::model::*;

    fn wb() -> Workbook {
        let mut wb = Workbook::new();
        let data = [
            ["Name", "Qty", "Price"],
            ["Apple", "3", "1.5"],
            ["Banana", "5", "0.25"],
            ["Cherry", "7", "4"],
            ["apple", "2", "1.5"],
        ];
        for (r, row) in data.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let cell = cell_from_input(&mut wb, v, 0);
                wb.sheets[0].set(r as u32, c as u32, cell);
            }
        }
        let cell = cell_from_input(&mut wb, "=B2*C2", 0);
        wb.sheets[0].set(1, 3, cell);
        wb.recalc();
        wb
    }

    fn ev(wb: &Workbook, f: &str) -> Value {
        eval_formula(wb, 0, 10, 10, f)
    }

    #[test]
    fn basics() {
        let w = wb();
        assert_eq!(ev(&w, "=SUM(B2:B5)"), Value::Number(17.0));
        assert_eq!(ev(&w, "=SUM(B:B)"), Value::Number(17.0));
        assert_eq!(ev(&w, "=AVERAGE(B2:B5)"), Value::Number(4.25));
        assert_eq!(ev(&w, "=D2"), Value::Number(4.5));
        assert_eq!(ev(&w, "=1+2*3"), Value::Number(7.0));
        assert_eq!(ev(&w, "=-2^2"), Value::Number(4.0));
        assert_eq!(ev(&w, "=2^3^2"), Value::Number(64.0));
        assert_eq!(ev(&w, "=10%"), Value::Number(0.1));
        assert_eq!(ev(&w, "=\"a\"&1&TRUE"), Value::Text("a1TRUE".into()));
        assert_eq!(ev(&w, "=1/0"), Value::Error(ErrorKind::Div0));
        assert_eq!(ev(&w, "=A2=\"APPLE\""), Value::Bool(true));
        assert_eq!(ev(&w, "=FOO(1)"), Value::Error(ErrorKind::Name));
    }

    #[test]
    fn lookups() {
        let w = wb();
        assert_eq!(ev(&w, "=VLOOKUP(\"Cherry\",A2:C5,3,FALSE)"), Value::Number(4.0));
        assert_eq!(ev(&w, "=VLOOKUP(\"Ch*\",A2:C5,2,FALSE)"), Value::Number(7.0));
        assert_eq!(ev(&w, "=VLOOKUP(\"Zed\",A2:C5,2,FALSE)"), Value::Error(ErrorKind::NA));
        assert_eq!(ev(&w, "=INDEX(B2:B5,MATCH(\"Banana\",A2:A5,0))"), Value::Number(5.0));
        assert_eq!(ev(&w, "=XLOOKUP(\"Banana\",A2:A5,C2:C5)"), Value::Number(0.25));
        assert_eq!(ev(&w, "=XLOOKUP(\"x\",A2:A5,C2:C5,\"none\")"), Value::Text("none".into()));
        assert_eq!(ev(&w, "=SUM(OFFSET(B2,1,0,2,1))"), Value::Number(12.0));
        assert_eq!(ev(&w, "=SUM(INDIRECT(\"B2:B3\"))"), Value::Number(8.0));
        assert_eq!(ev(&w, "=ROWS(A1:C5)*COLUMNS(A1:C5)"), Value::Number(15.0));
    }

    #[test]
    fn conditional() {
        let w = wb();
        assert_eq!(ev(&w, "=SUMIF(A2:A5,\"apple\",B2:B5)"), Value::Number(5.0));
        assert_eq!(ev(&w, "=COUNTIF(B2:B5,\">3\")"), Value::Number(2.0));
        assert_eq!(ev(&w, "=SUMIFS(B2:B5,A2:A5,\"a*\",C2:C5,1.5)"), Value::Number(5.0));
        assert_eq!(ev(&w, "=COUNTIFS(A2:A5,\"<>Banana\")"), Value::Number(3.0));
        assert_eq!(ev(&w, "=SUMPRODUCT(B2:B5,C2:C5)"), Value::Number(3.0 * 1.5 + 5.0 * 0.25 + 28.0 + 3.0));
        assert_eq!(ev(&w, "=SUMPRODUCT((B2:B5>3)*C2:C5)"), Value::Number(4.25));
        assert_eq!(ev(&w, "=IF(B2>2,\"y\",\"n\")"), Value::Text("y".into()));
        assert_eq!(ev(&w, "=IFERROR(1/0,\"oops\")"), Value::Text("oops".into()));
        assert_eq!(ev(&w, "=AND(TRUE,B2>1)"), Value::Bool(true));
        assert_eq!(ev(&w, "=COUNTA(A1:C5)"), Value::Number(15.0));
        assert_eq!(ev(&w, "=COUNT(A1:C5)"), Value::Number(8.0));
        assert_eq!(ev(&w, "=COUNTBLANK(A1:A7)"), Value::Number(2.0));
    }

    #[test]
    fn text_and_dates() {
        let w = wb();
        assert_eq!(ev(&w, "=LEFT(\"Hello\",2)&RIGHT(\"World\")"), Value::Text("Hed".into()));
        assert_eq!(ev(&w, "=MID(\"spreadsheet\",7,5)"), Value::Text("sheet".into()));
        assert_eq!(ev(&w, "=PROPER(\"hello wORLD\")"), Value::Text("Hello World".into()));
        assert_eq!(ev(&w, "=TRIM(\"  a   b \")"), Value::Text("a b".into()));
        assert_eq!(ev(&w, "=SEARCH(\"s*t\",\"spreadsheet\")"), Value::Number(1.0));
        assert_eq!(ev(&w, "=TEXTJOIN(\", \",TRUE,A2:A4)"), Value::Text("Apple, Banana, Cherry".into()));
        assert_eq!(ev(&w, "=TEXT(1234.5,\"#,##0.00\")"), Value::Text("1,234.50".into()));
        assert_eq!(ev(&w, "=DATE(2026,11,4)"), Value::Number(46330.0));
        assert_eq!(ev(&w, "=YEAR(46330)&\"-\"&MONTH(46330)"), Value::Text("2026-11".into()));
        assert_eq!(ev(&w, "=EOMONTH(DATE(2024,1,15),1)"), Value::Number(crate::numfmt::ymd_to_serial(2024, 2, 29)));
        assert_eq!(ev(&w, "=DATEDIF(DATE(2020,1,1),DATE(2026,11,4),\"Y\")"), Value::Number(6.0));
        assert_eq!(ev(&w, "=WEEKDAY(DATE(2026,11,4))"), Value::Number(4.0));
        assert_eq!(ev(&w, "=ROUND(2.5,0)+ROUND(1.005,2)"), Value::Number(4.01));
        assert_eq!(ev(&w, "=MOD(-3,2)"), Value::Number(1.0));
        assert_eq!(ev(&w, "=ROUND(PMT(0.05/12,360,200000),2)"), Value::Number(-1073.64));
    }

    #[test]
    fn circular() {
        let mut w = Workbook::new();
        let c = cell_from_input(&mut w, "=A2", 0);
        w.sheets[0].set(0, 0, c);
        let c = cell_from_input(&mut w, "=A1", 0);
        w.sheets[0].set(1, 0, c);
        w.recalc();
        assert!(w.circular);
    }
}
