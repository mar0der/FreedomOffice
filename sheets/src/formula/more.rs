//! More worksheet functions: LOOKUP, CELL / INFO, AREAS, FREQUENCY, database functions,
//! probability distributions and tests, matrices, regular expressions, text conversion and
//! smaller math / engineering helpers.

use std::f64::consts::PI;

use super::eval::{boolean, num, text, to_num, Evaluator, Res, ARRAY_MAX, EV};
use super::ext::{betai, combin, ln_gamma, norm_cdf, norm_pdf, norm_s_inv, t_cdf};
use super::functions::{crit_match, get, lookup_approx, need, parse_criteria, stats, vector, CritOp, Criteria};
use super::parser::{quote_sheet, Expr};
use crate::model::{col_name, ErrorKind as E, Value, MAX_COLS, MAX_ROWS};
use crate::numfmt;

type G = Vec<Vec<Value>>;

fn grid_arg(ev: &mut Evaluator, a: &[Expr], i: usize) -> Result<G, E> {
    match ev.arg(a, i)? {
        EV::M(_) | EV::L(_) => Err(E::Value),
        v => Ok(ev.grid(v)),
    }
}

fn dims(g: &G) -> (usize, usize) {
    (g.len(), g.first().map_or(0, |r| r.len()))
}

/// Numbers of one argument (SUM rules).
fn nums(ev: &mut Evaluator, a: &[Expr], i: usize) -> Result<Vec<f64>, E> {
    if i >= a.len() {
        return Err(E::Value);
    }
    ev.numbers(&a[i..i + 1], false)
}

/// Evaluates `NAME(args)` as another function (aliases and compositions).
fn via(ev: &mut Evaluator, name: &str, args: Vec<Expr>) -> Res {
    super::functions::call(ev, name, &args)
}

fn prob(p: f64) -> Result<f64, E> {
    if (0.0..=1.0).contains(&p) { Ok(p) } else { Err(E::Num) }
}

pub fn call(ev: &mut Evaluator, name: &str, a: &[Expr]) -> Res {
    let n = a.len();
    match name {
        // ----- lookup & information -----
        "LOOKUP" => {
            need(a, 2, 3)?;
            let key = ev.value(a, 0)?;
            let lv = grid_arg(ev, a, 1)?;
            let (h, w) = dims(&lv);
            if !ev.has(a, 2) {
                // Array form: search the first row of a wide array, else the first column, and
                // return from the last row / column.
                let wide = w > h;
                let keys: Vec<Value> = if wide { lv[0].clone() } else { lv.iter().map(|r| r[0].clone()).collect() };
                let i = lookup_approx(&keys, &key).ok_or(E::NA)?;
                return Ok(EV::V(if wide { lv[h - 1][i].clone() } else { lv[i][w - 1].clone() }));
            }
            let keys = if h == 1 { lv.into_iter().next().unwrap_or_default() } else { vector(lv) };
            let i = lookup_approx(&keys, &key).ok_or(E::NA)?;
            match ev.arg(a, 2)? {
                EV::R(s, rg) => {
                    // The result vector extends past its end like Excel (same orientation).
                    let (r, c) = if rg.rows() == 1 && rg.cols() > 1 { (rg.r1 as usize, rg.c1 as usize + i) } else { (rg.r1 as usize + i, rg.c1 as usize) };
                    if r >= MAX_ROWS as usize || c >= MAX_COLS as usize {
                        return Err(E::Ref);
                    }
                    Ok(EV::V(ev.cell_value(s, r as u32, c as u32)))
                }
                EV::M(_) | EV::L(_) => Err(E::Value),
                other => {
                    let g = ev.grid(other);
                    let flat: Vec<Value> = g.into_iter().flatten().collect();
                    flat.get(i).cloned().map(EV::V).ok_or(E::NA)
                }
            }
        }
        "AREAS" => {
            need(a, 1, 1)?;
            match ev.arg(a, 0)? {
                EV::R(..) => num(1.0),
                EV::M(m) => num(m.len() as f64),
                _ => Err(E::Value),
            }
        }
        "CELL" => cell_info(ev, a),
        "INFO" => {
            need(a, 1, 1)?;
            let t = ev.string(a, 0)?.to_ascii_lowercase();
            match t.as_str() {
                "directory" => {
                    let dir = ev.wb.path.as_ref().and_then(|p| p.parent()).map(|d| d.to_string_lossy().to_string());
                    text(dir.map(|d| if d.ends_with('\\') || d.ends_with('/') { d } else { format!("{d}\\") }).unwrap_or_default())
                }
                "numfile" => num(ev.wb.sheets.len() as f64),
                "origin" => text("$A:$A$1"),
                "osversion" => text("Windows (64-bit) NT 10.00"),
                "recalc" => text("Automatic"),
                "release" => text("16.0"),
                "system" => text("pcdos"),
                _ => Err(E::Value),
            }
        }
        "FREQUENCY" => {
            need(a, 2, 2)?;
            let data = nums(ev, a, 0)?;
            let bins = nums(ev, a, 1)?;
            let mut sorted: Vec<(f64, usize)> = bins.iter().copied().zip(0..).collect();
            sorted.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
            let mut counts = vec![0.0; bins.len() + 1];
            for x in data {
                // First bin (in ascending order) holding x; beyond the last goes to the extra slot.
                match sorted.iter().find(|(b, _)| x <= *b) {
                    Some((_, i)) => counts[*i] += 1.0,
                    None => counts[bins.len()] += 1.0,
                }
            }
            Ok(EV::A(counts.into_iter().map(|c| vec![Value::Number(c)]).collect()))
        }
        "TRIMMEAN" => {
            need(a, 2, 2)?;
            let mut v = nums(ev, a, 0)?;
            let p = ev.number(a, 1)?;
            if !(0.0..1.0).contains(&p) || v.is_empty() {
                return Err(E::Num);
            }
            v.sort_by(f64::total_cmp);
            let k = ((v.len() as f64 * p) / 2.0).floor() as usize;
            let kept = &v[k..v.len() - k];
            num(kept.iter().sum::<f64>() / kept.len() as f64)
        }
        "PERCENTOF" => {
            need(a, 2, 2)?;
            let part: f64 = nums(ev, a, 0)?.iter().sum();
            let all: f64 = nums(ev, a, 1)?.iter().sum();
            if all == 0.0 { Err(E::Div0) } else { num(part / all) }
        }

        // ----- database functions -----
        "DSUM" | "DCOUNT" | "DCOUNTA" | "DAVERAGE" | "DGET" | "DMAX" | "DMIN" | "DPRODUCT" | "DSTDEV" | "DSTDEVP" | "DVAR" | "DVARP" => {
            database(ev, name, a)
        }

        // ----- text conversion -----
        "ARRAYTOTEXT" | "VALUETOTEXT" => {
            need(a, 1, 2)?;
            let strict = match ev.number_or(a, 1, 0.0)?.trunc() {
                0.0 => false,
                1.0 => true,
                _ => return Err(E::Value),
            };
            let item = |v: &Value| -> String {
                match v {
                    Value::Text(s) if strict => format!("\"{}\"", s.replace('"', "\"\"")),
                    Value::Error(e) => e.as_str().to_string(),
                    v => numfmt::value_to_text(v),
                }
            };
            if name == "VALUETOTEXT" {
                let v = match ev.value(a, 0) {
                    Ok(v) => v,
                    Err(e) => Value::Error(e),
                };
                return text(item(&v));
            }
            let g = grid_arg(ev, a, 0)?;
            let s = if strict {
                let rows: Vec<String> = g.iter().map(|r| r.iter().map(&item).collect::<Vec<_>>().join(",")).collect();
                format!("{{{}}}", rows.join(";"))
            } else {
                g.iter().flatten().map(&item).collect::<Vec<_>>().join(", ")
            };
            if s.chars().count() > super::eval::MAX_TEXT { Err(E::Value) } else { text(s) }
        }
        "ENCODEURL" => {
            need(a, 1, 1)?;
            let s = ev.string(a, 0)?;
            let mut out = String::with_capacity(s.len());
            for b in s.bytes() {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                    out.push(b as char);
                } else {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
            text(out)
        }
        // Byte-counting text functions: identical in single-byte locales.
        "LENB" | "LEFTB" | "RIGHTB" | "MIDB" | "FINDB" | "SEARCHB" | "REPLACEB" => via(ev, &name[..name.len() - 1], a.to_vec()),

        // ----- regular expressions -----
        "REGEXTEST" => {
            need(a, 2, 3)?;
            let s = ev.string(a, 0)?;
            let re = regex(ev, a, 1, 2)?;
            boolean(re.is_match(&s))
        }
        "REGEXEXTRACT" => {
            need(a, 2, 4)?;
            let s = ev.string(a, 0)?;
            let mode = ev.number_or(a, 2, 0.0)?.trunc();
            let re = regex(ev, a, 1, 3)?;
            // A capture group that took no part in the match is blank.
            let cell = |m: Option<regex_lite::Match>| Value::Text(m.map_or(String::new(), |m| m.as_str().to_string()));
            match mode {
                0.0 => match re.find(&s) {
                    Some(m) => text(m.as_str()),
                    None => Err(E::NA),
                },
                1.0 => {
                    let all: Vec<Vec<Value>> = re.find_iter(&s).map(|m| vec![Value::Text(m.as_str().to_string())]).collect();
                    if all.is_empty() { Err(E::NA) } else { Ok(EV::A(all)) }
                }
                2.0 => match re.captures(&s) {
                    Some(c) if c.len() > 1 => Ok(EV::A(vec![(1..c.len()).map(|i| cell(c.get(i))).collect()])),
                    Some(c) => text(c.get(0).map_or("", |m| m.as_str())),
                    None => Err(E::NA),
                },
                _ => Err(E::Value),
            }
        }
        "REGEXREPLACE" => {
            need(a, 3, 5)?;
            let s = ev.string(a, 0)?;
            let rep = ev.string(a, 2)?;
            let occ = ev.number_or(a, 3, 0.0)?.trunc() as i64;
            let re = regex(ev, a, 1, 4)?;
            if occ == 0 {
                return text(re.replace_all(&s, rep.as_str()).into_owned());
            }
            let ms: Vec<regex_lite::Captures> = re.captures_iter(&s).collect();
            let k = if occ > 0 { occ - 1 } else { ms.len() as i64 + occ };
            let Some(c) = (k >= 0).then(|| ms.get(k as usize)).flatten() else { return text(s) };
            let m = c.get(0).expect("whole match");
            let mut out = String::new();
            c.expand(&rep, &mut out);
            text(format!("{}{}{}", &s[..m.start()], out, &s[m.end()..]))
        }

        // ----- statistics -----
        "STDEVA" | "STDEVPA" | "VARA" | "VARPA" => {
            let mut v = Vec::new();
            for i in 0..n {
                let x = ev.arg(a, i)?;
                ev.walk(x, false, &mut |val, from_ref| {
                    match val {
                        Value::Number(x) => v.push(*x),
                        Value::Bool(b) => v.push(if *b { 1.0 } else { 0.0 }),
                        Value::Text(_) if from_ref => v.push(0.0),
                        Value::Text(_) => v.push(to_num(val)?),
                        Value::Error(e) => return Err(*e),
                        Value::Empty => {}
                    }
                    Ok(())
                })?;
            }
            let sample = !name.contains('P');
            let var = stats(&v, sample)?;
            num(if name.starts_with("STDEV") { var.sqrt() } else { var })
        }
        "BINOM.DIST" | "BINOMDIST" => {
            need(a, 4, 4)?;
            let k = ev.number(a, 0)?.trunc();
            let trials = ev.number(a, 1)?.trunc();
            let p = prob(ev.number(a, 2)?)?;
            let cum = ev.boolean(a, 3)?;
            if k < 0.0 || k > trials {
                return Err(E::Num);
            }
            num(if cum { (0..=k as u64).map(|i| binom_pmf(i as f64, trials, p)).sum::<f64>().min(1.0) } else { binom_pmf(k, trials, p) })
        }
        "BINOM.DIST.RANGE" => {
            need(a, 3, 4)?;
            let trials = ev.number(a, 0)?.trunc();
            let p = prob(ev.number(a, 1)?)?;
            let s1 = ev.number(a, 2)?.trunc();
            let s2 = if ev.has(a, 3) { ev.number(a, 3)?.trunc() } else { s1 };
            if trials < 0.0 || s1 < 0.0 || s1 > trials || s2 < s1 || s2 > trials {
                return Err(E::Num);
            }
            num((s1 as u64..=s2 as u64).map(|i| binom_pmf(i as f64, trials, p)).sum())
        }
        "BINOM.INV" | "CRITBINOM" => {
            need(a, 3, 3)?;
            let trials = ev.number(a, 0)?.trunc();
            let p = prob(ev.number(a, 1)?)?;
            let alpha = prob(ev.number(a, 2)?)?;
            if trials < 0.0 {
                return Err(E::Num);
            }
            let mut acc = 0.0;
            for k in 0..=trials as u64 {
                acc += binom_pmf(k as f64, trials, p);
                if acc >= alpha * (1.0 - 1e-15) {
                    return num(k as f64);
                }
            }
            num(trials)
        }
        "POISSON.DIST" | "POISSON" => {
            need(a, 3, 3)?;
            let x = ev.number(a, 0)?.trunc();
            let mean = ev.number(a, 1)?;
            let cum = ev.boolean(a, 2)?;
            if x < 0.0 || mean < 0.0 {
                return Err(E::Num);
            }
            let pmf = |k: f64| if mean == 0.0 { if k == 0.0 { 1.0 } else { 0.0 } } else { (k * mean.ln() - mean - ln_gamma(k + 1.0)).exp() };
            num(if cum { gamma_q(x + 1.0, mean) } else { pmf(x) })
        }
        "EXPON.DIST" | "EXPONDIST" => {
            need(a, 3, 3)?;
            let x = ev.number(a, 0)?;
            let l = ev.number(a, 1)?;
            let cum = ev.boolean(a, 2)?;
            if x < 0.0 || l <= 0.0 {
                return Err(E::Num);
            }
            num(if cum { -(-l * x).exp_m1() } else { l * (-l * x).exp() })
        }
        "GAMMA.DIST" | "GAMMADIST" => {
            need(a, 4, 4)?;
            let (x, al, be) = (ev.number(a, 0)?, ev.number(a, 1)?, ev.number(a, 2)?);
            let cum = ev.boolean(a, 3)?;
            if x < 0.0 || al <= 0.0 || be <= 0.0 {
                return Err(E::Num);
            }
            num(if cum { gamma_p(al, x / be) } else { gamma_pdf(x, al, be) })
        }
        "GAMMA.INV" | "GAMMAINV" => {
            need(a, 3, 3)?;
            let (p, al, be) = (prob(ev.number(a, 0)?)?, ev.number(a, 1)?, ev.number(a, 2)?);
            if al <= 0.0 || be <= 0.0 || p == 1.0 {
                return Err(E::Num);
            }
            num(if p == 0.0 { 0.0 } else { invert(|x| gamma_p(al, x / be), p, 0.0, al * be * 2.0 + 1.0)? })
        }
        "GAMMA" => {
            need(a, 1, 1)?;
            let x = ev.number(a, 0)?;
            if x <= 0.0 && x.fract() == 0.0 {
                return Err(E::Num);
            }
            num(gamma(x))
        }
        "GAMMALN" | "GAMMALN.PRECISE" => {
            need(a, 1, 1)?;
            let x = ev.number(a, 0)?;
            if x <= 0.0 { Err(E::Num) } else { num(ln_gamma(x)) }
        }
        "BETA.DIST" | "BETADIST" => {
            let legacy = name == "BETADIST";
            need(a, if legacy { 3 } else { 4 }, if legacy { 5 } else { 6 })?;
            let (x, al, be) = (ev.number(a, 0)?, ev.number(a, 1)?, ev.number(a, 2)?);
            let (cum, ai) = if legacy { (true, 3) } else { (ev.boolean(a, 3)?, 4) };
            let lo = ev.number_or(a, ai, 0.0)?;
            let hi = ev.number_or(a, ai + 1, 1.0)?;
            if al <= 0.0 || be <= 0.0 || x < lo || x > hi || lo == hi {
                return Err(E::Num);
            }
            let z = (x - lo) / (hi - lo);
            num(if cum {
                betai(al, be, z)
            } else {
                ((al - 1.0) * z.ln() + (be - 1.0) * (1.0 - z).ln() - ln_beta(al, be)).exp() / (hi - lo)
            })
        }
        "BETA.INV" | "BETAINV" => {
            need(a, 3, 5)?;
            let (p, al, be) = (prob(ev.number(a, 0)?)?, ev.number(a, 1)?, ev.number(a, 2)?);
            let lo = ev.number_or(a, 3, 0.0)?;
            let hi = ev.number_or(a, 4, 1.0)?;
            if al <= 0.0 || be <= 0.0 || lo >= hi {
                return Err(E::Num);
            }
            num(lo + (hi - lo) * invert(|z| betai(al, be, z), p, 0.0, 1.0)?)
        }
        "CHISQ.DIST" => {
            need(a, 3, 3)?;
            let (x, df) = (ev.number(a, 0)?, ev.number(a, 1)?.trunc());
            let cum = ev.boolean(a, 2)?;
            if x < 0.0 || !(1.0..=1e10).contains(&df) {
                return Err(E::Num);
            }
            num(if cum { gamma_p(df / 2.0, x / 2.0) } else { gamma_pdf(x, df / 2.0, 2.0) })
        }
        "CHISQ.DIST.RT" | "CHIDIST" => {
            need(a, 2, 2)?;
            let (x, df) = (ev.number(a, 0)?, ev.number(a, 1)?.trunc());
            if x < 0.0 || !(1.0..=1e10).contains(&df) {
                return Err(E::Num);
            }
            num(gamma_q(df / 2.0, x / 2.0))
        }
        "CHISQ.INV" | "CHISQ.INV.RT" | "CHIINV" => {
            need(a, 2, 2)?;
            let (p, df) = (prob(ev.number(a, 0)?)?, ev.number(a, 1)?.trunc());
            if !(1.0..=1e10).contains(&df) {
                return Err(E::Num);
            }
            if name == "CHISQ.INV" {
                if p == 1.0 {
                    return Err(E::Num);
                }
                num(if p == 0.0 { 0.0 } else { invert(|x| gamma_p(df / 2.0, x / 2.0), p, 0.0, df * 2.0 + 2.0)? })
            } else {
                if p == 0.0 {
                    return Err(E::Num);
                }
                num(if p == 1.0 { 0.0 } else { invert(|x| -gamma_q(df / 2.0, x / 2.0), -p, 0.0, df * 2.0 + 2.0)? })
            }
        }
        "CHISQ.TEST" | "CHITEST" => {
            need(a, 2, 2)?;
            let act = grid_arg(ev, a, 0)?;
            let exp = grid_arg(ev, a, 1)?;
            let (r, c) = dims(&act);
            if dims(&exp) != (r, c) {
                return Err(E::NA);
            }
            let mut chi = 0.0;
            for (x, y) in act.iter().flatten().zip(exp.iter().flatten()) {
                if let (Value::Number(x), Value::Number(y)) = (x, y) {
                    if *y == 0.0 {
                        return Err(E::Div0);
                    }
                    chi += (x - y).powi(2) / y;
                }
            }
            let df = if r > 1 && c > 1 { (r - 1) * (c - 1) } else { r * c - 1 };
            if df == 0 {
                return Err(E::NA);
            }
            num(gamma_q(df as f64 / 2.0, chi / 2.0))
        }
        "F.DIST" | "F.DIST.RT" | "FDIST" => {
            let cumulative = name == "F.DIST";
            need(a, if cumulative { 4 } else { 3 }, if cumulative { 4 } else { 3 })?;
            let (x, d1, d2) = (ev.number(a, 0)?, ev.number(a, 1)?.trunc(), ev.number(a, 2)?.trunc());
            if x < 0.0 || d1 < 1.0 || d2 < 1.0 {
                return Err(E::Num);
            }
            if !cumulative {
                return num(betai(d2 / 2.0, d1 / 2.0, d2 / (d2 + d1 * x)));
            }
            if ev.boolean(a, 3)? {
                num(betai(d1 / 2.0, d2 / 2.0, d1 * x / (d1 * x + d2)))
            } else {
                if x == 0.0 {
                    return num(if d1 < 2.0 { f64::INFINITY } else if d1 == 2.0 { 1.0 } else { 0.0 });
                }
                let l = 0.5 * (d1 * (d1 * x).ln() + d2 * d2.ln() - (d1 + d2) * (d1 * x + d2).ln()) - x.ln() - ln_beta(d1 / 2.0, d2 / 2.0);
                num(l.exp())
            }
        }
        "F.INV" | "F.INV.RT" | "FINV" => {
            need(a, 3, 3)?;
            let (p, d1, d2) = (prob(ev.number(a, 0)?)?, ev.number(a, 1)?.trunc(), ev.number(a, 2)?.trunc());
            if d1 < 1.0 || d2 < 1.0 {
                return Err(E::Num);
            }
            let left = name == "F.INV";
            let q = if left { p } else { 1.0 - p };
            if (left && p == 1.0) || (!left && p == 0.0) {
                return Err(E::Num);
            }
            if q == 0.0 {
                return num(0.0);
            }
            // Invert through the beta distribution: x = d2 z / (d1 (1 - z)).
            let z = if left { invert(|z| betai(d1 / 2.0, d2 / 2.0, z), p, 0.0, 1.0)? } else { 1.0 - invert(|z| betai(d2 / 2.0, d1 / 2.0, z), p, 0.0, 1.0)? };
            num(d2 * z / (d1 * (1.0 - z)))
        }
        "F.TEST" | "FTEST" => {
            need(a, 2, 2)?;
            let x = nums(ev, a, 0)?;
            let y = nums(ev, a, 1)?;
            if x.len() < 2 || y.len() < 2 {
                return Err(E::Div0);
            }
            let (vx, vy) = (stats(&x, true)?, stats(&y, true)?);
            if vx == 0.0 || vy == 0.0 {
                return Err(E::Div0);
            }
            let (d1, d2) = ((x.len() - 1) as f64, (y.len() - 1) as f64);
            let f = vx / vy;
            let cdf = betai(d1 / 2.0, d2 / 2.0, d1 * f / (d1 * f + d2));
            num((2.0 * cdf.min(1.0 - cdf)).min(1.0))
        }
        "T.TEST" | "TTEST" => {
            need(a, 4, 4)?;
            let tails = ev.number(a, 2)?.trunc();
            let typ = ev.number(a, 3)?.trunc();
            if !(tails == 1.0 || tails == 2.0) || !(1.0..=3.0).contains(&typ) {
                return Err(E::Num);
            }
            let (t, df) = if typ == 1.0 {
                let g1 = grid_arg(ev, a, 0)?;
                let g2 = grid_arg(ev, a, 1)?;
                let (x, y): (Vec<Value>, Vec<Value>) = (g1.into_iter().flatten().collect(), g2.into_iter().flatten().collect());
                if x.len() != y.len() {
                    return Err(E::NA);
                }
                let d: Vec<f64> = x.iter().zip(&y).filter_map(|p| if let (Value::Number(u), Value::Number(v)) = p { Some(u - v) } else { None }).collect();
                if d.len() < 2 {
                    return Err(E::Div0);
                }
                let m = d.iter().sum::<f64>() / d.len() as f64;
                let s = stats(&d, true)?.sqrt();
                if s == 0.0 {
                    return Err(E::Div0);
                }
                (m / (s / (d.len() as f64).sqrt()), d.len() as f64 - 1.0)
            } else {
                let x = nums(ev, a, 0)?;
                let y = nums(ev, a, 1)?;
                if x.len() < 2 || y.len() < 2 {
                    return Err(E::Div0);
                }
                let (n1, n2) = (x.len() as f64, y.len() as f64);
                let (m1, m2) = (x.iter().sum::<f64>() / n1, y.iter().sum::<f64>() / n2);
                let (v1, v2) = (stats(&x, true)?, stats(&y, true)?);
                if typ == 2.0 {
                    let sp = ((n1 - 1.0) * v1 + (n2 - 1.0) * v2) / (n1 + n2 - 2.0);
                    let se = (sp * (1.0 / n1 + 1.0 / n2)).sqrt();
                    if se == 0.0 {
                        return Err(E::Div0);
                    }
                    ((m1 - m2) / se, n1 + n2 - 2.0)
                } else {
                    let (q1, q2) = (v1 / n1, v2 / n2);
                    if q1 + q2 == 0.0 {
                        return Err(E::Div0);
                    }
                    let df = (q1 + q2).powi(2) / (q1 * q1 / (n1 - 1.0) + q2 * q2 / (n2 - 1.0));
                    ((m1 - m2) / (q1 + q2).sqrt(), df)
                }
            };
            num(tails * (1.0 - t_cdf(t.abs(), df)))
        }
        "Z.TEST" | "ZTEST" => {
            need(a, 2, 3)?;
            let v = nums(ev, a, 0)?;
            let x = ev.number(a, 1)?;
            if v.is_empty() {
                return Err(E::NA);
            }
            let sigma = if ev.has(a, 2) { ev.number(a, 2)? } else { stats(&v, true)?.sqrt() };
            if sigma == 0.0 {
                return Err(E::Div0);
            }
            let m = v.iter().sum::<f64>() / v.len() as f64;
            num(1.0 - norm_cdf((m - x) / (sigma / (v.len() as f64).sqrt())))
        }
        "LOGNORM.DIST" | "LOGNORMDIST" => {
            let legacy = name == "LOGNORMDIST";
            need(a, if legacy { 3 } else { 4 }, if legacy { 3 } else { 4 })?;
            let (x, m, sd) = (ev.number(a, 0)?, ev.number(a, 1)?, ev.number(a, 2)?);
            let cum = if legacy { true } else { ev.boolean(a, 3)? };
            if x <= 0.0 || sd <= 0.0 {
                return Err(E::Num);
            }
            let z = (x.ln() - m) / sd;
            num(if cum { norm_cdf(z) } else { norm_pdf(z) / (x * sd) })
        }
        "LOGNORM.INV" | "LOGINV" => {
            need(a, 3, 3)?;
            let (p, m, sd) = (ev.number(a, 0)?, ev.number(a, 1)?, ev.number(a, 2)?);
            if p <= 0.0 || p >= 1.0 || sd <= 0.0 {
                return Err(E::Num);
            }
            num((m + sd * norm_s_inv(p)).exp())
        }
        "WEIBULL.DIST" | "WEIBULL" => {
            need(a, 4, 4)?;
            let (x, al, be) = (ev.number(a, 0)?, ev.number(a, 1)?, ev.number(a, 2)?);
            let cum = ev.boolean(a, 3)?;
            if x < 0.0 || al <= 0.0 || be <= 0.0 {
                return Err(E::Num);
            }
            let t = (x / be).powf(al);
            num(if cum { -(-t).exp_m1() } else { al / be.powf(al) * x.powf(al - 1.0) * (-t).exp() })
        }
        "HYPGEOM.DIST" | "HYPGEOMDIST" => {
            let legacy = name == "HYPGEOMDIST";
            need(a, if legacy { 4 } else { 5 }, if legacy { 4 } else { 5 })?;
            let k = ev.number(a, 0)?.trunc();
            let ns = ev.number(a, 1)?.trunc();
            let kk = ev.number(a, 2)?.trunc();
            let nn = ev.number(a, 3)?.trunc();
            let cum = if legacy { false } else { ev.boolean(a, 4)? };
            if k < 0.0 || k > ns.min(kk) || k < (ns - nn + kk).max(0.0) || ns <= 0.0 || ns > nn || kk <= 0.0 || kk > nn || nn <= 0.0 {
                return Err(E::Num);
            }
            let pmf = |i: f64| (ln_combin(kk, i) + ln_combin(nn - kk, ns - i) - ln_combin(nn, ns)).exp();
            let lo = (ns - nn + kk).max(0.0);
            num(if cum { (lo as u64..=k as u64).map(|i| pmf(i as f64)).sum::<f64>().min(1.0) } else { pmf(k) })
        }
        "NEGBINOM.DIST" | "NEGBINOMDIST" => {
            let legacy = name == "NEGBINOMDIST";
            need(a, if legacy { 3 } else { 4 }, if legacy { 3 } else { 4 })?;
            let f = ev.number(a, 0)?.trunc();
            let s = ev.number(a, 1)?.trunc();
            let p = ev.number(a, 2)?;
            let cum = if legacy { false } else { ev.boolean(a, 3)? };
            if f < 0.0 || s < 1.0 || p <= 0.0 || p >= 1.0 {
                return Err(E::Num);
            }
            num(if cum { betai(s, f + 1.0, p) } else { (ln_combin(f + s - 1.0, s - 1.0) + s * p.ln() + f * (1.0 - p).ln()).exp() })
        }
        "FISHER" => {
            need(a, 1, 1)?;
            let x = ev.number(a, 0)?;
            if x <= -1.0 || x >= 1.0 { Err(E::Num) } else { num(x.atanh()) }
        }
        "FISHERINV" => {
            need(a, 1, 1)?;
            num(ev.number(a, 0)?.tanh())
        }
        "PHI" => {
            need(a, 1, 1)?;
            num(norm_pdf(ev.number(a, 0)?))
        }
        "GAUSS" => {
            need(a, 1, 1)?;
            num(norm_cdf(ev.number(a, 0)?) - 0.5)
        }
        "ERF" | "ERF.PRECISE" => {
            need(a, 1, if name == "ERF" { 2 } else { 1 })?;
            let lo = ev.number(a, 0)?;
            if ev.has(a, 1) {
                let hi = ev.number(a, 1)?;
                return num(erf(hi) - erf(lo));
            }
            num(erf(lo))
        }
        "ERFC" | "ERFC.PRECISE" => {
            need(a, 1, 1)?;
            num(2.0 * norm_cdf(-ev.number(a, 0)? * std::f64::consts::SQRT_2))
        }
        "PROB" => {
            need(a, 3, 4)?;
            let xs = grid_arg(ev, a, 0)?;
            let ps = grid_arg(ev, a, 1)?;
            if dims(&xs) != dims(&ps) {
                return Err(E::NA);
            }
            let lo = ev.number(a, 2)?;
            let hi = if ev.has(a, 3) { ev.number(a, 3)? } else { lo };
            let (mut total, mut sum) = (0.0, 0.0);
            for (x, p) in xs.iter().flatten().zip(ps.iter().flatten()) {
                if let (Value::Number(x), Value::Number(p)) = (x, p) {
                    if !(0.0..=1.0).contains(p) {
                        return Err(E::Num);
                    }
                    total += p;
                    if (lo..=hi).contains(x) {
                        sum += p;
                    }
                }
            }
            if (total - 1.0).abs() > 1e-7 { Err(E::Num) } else { num(sum) }
        }

        // ----- matrices -----
        "MMULT" => {
            need(a, 2, 2)?;
            let x = matrix(grid_arg(ev, a, 0)?)?;
            let y = matrix(grid_arg(ev, a, 1)?)?;
            let (r, k, c) = (x.len(), x[0].len(), y[0].len());
            if y.len() != k {
                return Err(E::Value);
            }
            if r as u64 * c as u64 > ARRAY_MAX {
                return Err(E::Calc);
            }
            Ok(EV::A((0..r).map(|i| (0..c).map(|j| Value::Number((0..k).map(|t| x[i][t] * y[t][j]).sum())).collect()).collect()))
        }
        "MDETERM" => {
            need(a, 1, 1)?;
            let m = matrix(grid_arg(ev, a, 0)?)?;
            if m.len() != m[0].len() {
                return Err(E::Value);
            }
            num(lu(m).map_or(0.0, |(_, d)| d))
        }
        "MINVERSE" => {
            need(a, 1, 1)?;
            let m = matrix(grid_arg(ev, a, 0)?)?;
            let k = m.len();
            if k != m[0].len() {
                return Err(E::Value);
            }
            let inv = invert_matrix(m).ok_or(E::Num)?;
            Ok(EV::A(inv.into_iter().map(|r| r.into_iter().map(Value::Number).collect()).collect()))
        }
        "MUNIT" => {
            need(a, 1, 1)?;
            let k = ev.number(a, 0)?.trunc();
            if k < 1.0 {
                return Err(E::Value);
            }
            if k * k > ARRAY_MAX as f64 {
                return Err(E::Calc);
            }
            let k = k as usize;
            Ok(EV::A((0..k).map(|i| (0..k).map(|j| Value::Number(if i == j { 1.0 } else { 0.0 })).collect()).collect()))
        }

        // ----- math -----
        "SINH" | "COSH" | "TANH" | "ASINH" | "ACOSH" | "ATANH" | "COT" | "COTH" | "CSC" | "CSCH" | "SEC" | "SECH" | "ACOT" | "ACOTH" | "SQRTPI" => {
            need(a, 1, 1)?;
            let x = ev.number(a, 0)?;
            let inv = |d: f64| if d == 0.0 { Err(E::Div0) } else { Ok(1.0 / d) };
            num(match name {
                "SINH" => x.sinh(),
                "COSH" => x.cosh(),
                "TANH" => x.tanh(),
                "ASINH" => x.asinh(),
                "ACOSH" if x < 1.0 => return Err(E::Num),
                "ACOSH" => x.acosh(),
                "ATANH" if x.abs() >= 1.0 => return Err(E::Num),
                "ATANH" => x.atanh(),
                "COT" => inv(x.tan())?,
                "COTH" => inv(x.tanh())?,
                "CSC" => inv(x.sin())?,
                "CSCH" => inv(x.sinh())?,
                "SEC" => inv(x.cos())?,
                "SECH" => inv(x.cosh())?,
                "ACOT" => PI / 2.0 - x.atan(),
                "ACOTH" if x.abs() <= 1.0 => return Err(E::Num),
                "ACOTH" => 0.5 * ((x + 1.0) / (x - 1.0)).ln(),
                _ if x < 0.0 => return Err(E::Num),
                _ => (x * PI).sqrt(),
            })
        }
        "FACTDOUBLE" => {
            need(a, 1, 1)?;
            let x = ev.number(a, 0)?.trunc();
            if x < -1.0 {
                return Err(E::Num);
            }
            let mut p = 1.0;
            let mut k = x;
            while k > 1.0 {
                p *= k;
                k -= 2.0;
            }
            num(p)
        }
        "CEILING.PRECISE" | "ISO.CEILING" | "FLOOR.PRECISE" => {
            need(a, 1, 2)?;
            let x = ev.number(a, 0)?;
            let s = ev.number_or(a, 1, 1.0)?.abs();
            if s == 0.0 {
                return num(0.0);
            }
            let q = x / s;
            let q = if (q - q.round()).abs() < 1e-12 { q.round() } else { q };
            num(if name == "FLOOR.PRECISE" { q.floor() } else { q.ceil() } * s)
        }
        "SERIESSUM" => {
            need(a, 4, 4)?;
            let (x, n0, m) = (ev.number(a, 0)?, ev.number(a, 1)?, ev.number(a, 2)?);
            let cs = grid_arg(ev, a, 3)?;
            let mut s = 0.0;
            for (i, c) in cs.iter().flatten().enumerate() {
                s += to_num(c).map_err(|_| E::Value)? * x.powf(n0 + i as f64 * m);
            }
            num(s)
        }
        "PERMUTATIONA" => {
            need(a, 2, 2)?;
            let (x, k) = (ev.number(a, 0)?.trunc(), ev.number(a, 1)?.trunc());
            if x < 0.0 || k < 0.0 { Err(E::Num) } else { num(x.powf(k)) }
        }
        "BITAND" | "BITOR" | "BITXOR" | "BITLSHIFT" | "BITRSHIFT" => {
            need(a, 2, 2)?;
            const LIM: f64 = 281_474_976_710_656.0; // 2^48
            let x = ev.number(a, 0)?;
            let y = ev.number(a, 1)?;
            if x < 0.0 || x >= LIM || x.fract() != 0.0 {
                return Err(E::Num);
            }
            let xi = x as u64;
            let r = match name {
                "BITAND" | "BITOR" | "BITXOR" => {
                    if y < 0.0 || y >= LIM || y.fract() != 0.0 {
                        return Err(E::Num);
                    }
                    let yi = y as u64;
                    match name {
                        "BITAND" => xi & yi,
                        "BITOR" => xi | yi,
                        _ => xi ^ yi,
                    }
                }
                _ => {
                    let s = y.trunc();
                    if s.abs() > 53.0 {
                        return Err(E::Num);
                    }
                    let left = (name == "BITLSHIFT") == (s >= 0.0);
                    let s = s.abs() as u32;
                    if left {
                        let v = x * 2f64.powi(s as i32);
                        if v >= LIM {
                            return Err(E::Num);
                        }
                        v as u64
                    } else {
                        xi >> s
                    }
                }
            };
            num(r as f64)
        }
        "DELTA" => {
            need(a, 1, 2)?;
            num(if ev.number(a, 0)? == ev.number_or(a, 1, 0.0)? { 1.0 } else { 0.0 })
        }
        "GESTEP" => {
            need(a, 1, 2)?;
            num(if ev.number(a, 0)? >= ev.number_or(a, 1, 0.0)? { 1.0 } else { 0.0 })
        }
        "BIN2HEX" | "BIN2OCT" | "HEX2BIN" | "HEX2OCT" | "OCT2BIN" | "OCT2HEX" => {
            need(a, 1, 2)?;
            let from = match &name[..3] {
                "BIN" => "BIN2DEC",
                "HEX" => "HEX2DEC",
                _ => "OCT2DEC",
            };
            let to = format!("DEC2{}", &name[4..]);
            let mut args = vec![Expr::Func(from.into(), vec![a[0].clone()])];
            args.extend(a.get(1).cloned());
            via(ev, &to, args)
        }
        "ISPMT" => {
            need(a, 4, 4)?;
            let (r, per, nper, pv) = (ev.number(a, 0)?, ev.number(a, 1)?, ev.number(a, 2)?, ev.number(a, 3)?);
            if nper == 0.0 {
                return Err(E::Div0);
            }
            num(pv * r * (per / nper - 1.0))
        }
        "DOLLARDE" | "DOLLARFR" => {
            need(a, 2, 2)?;
            let x = ev.number(a, 0)?;
            let f = ev.number(a, 1)?.trunc();
            if f < 0.0 {
                return Err(E::Num);
            }
            if f == 0.0 {
                return Err(E::Div0);
            }
            let p = 10f64.powi(f.log10().ceil() as i32);
            let (ip, fp) = (x.trunc(), x - x.trunc());
            num(if name == "DOLLARDE" { ip + fp * p / f } else { ip + fp * f / p })
        }
        _ => super::lambda::call(ev, name, a),
    }
}

// ---------------------------------------------------------------------------
// CELL
// ---------------------------------------------------------------------------

fn cell_info(ev: &mut Evaluator, a: &[Expr]) -> Res {
    need(a, 1, 2)?;
    let what = ev.string(a, 0)?.to_ascii_lowercase();
    let (s, r, c) = if ev.has(a, 1) {
        match ev.arg(a, 1)? {
            EV::R(s, rg) => (s, rg.r1, rg.c1),
            EV::M(m) if !m.is_empty() => (m[0].0, m[0].1.r1, m[0].1.c1),
            _ => return Err(E::Value),
        }
    } else {
        ev.cur
    };
    let wb = ev.wb;
    let sh = wb.sheets.get(s).ok_or(E::Ref)?;
    let cell = sh.get(r, c);
    let fmt = cell.map(|x| wb.style(x.style).num_fmt.clone()).unwrap_or_default();
    match what.as_str() {
        "address" => text(format!("${}${}", col_name(c), r + 1)),
        "col" => num(c as f64 + 1.0),
        "row" => num(r as f64 + 1.0),
        "contents" => match ev.cell_value(s, r, c) {
            Value::Error(e) => Err(e),
            Value::Empty => num(0.0),
            v => Ok(EV::V(v)),
        },
        "type" => text(match ev.cell_value(s, r, c) {
            Value::Empty => "b",
            Value::Text(_) => "l",
            _ => "v",
        }),
        "width" => {
            let px = sh.raw_col_width(c) as f64;
            let chars = ((px - 5.0) / 7.0).max(0.0).trunc();
            Ok(EV::A(vec![vec![Value::Number(chars), Value::Bool(!sh.col_w.contains_key(&c))]]))
        }
        "format" => text(format_code(&fmt)),
        "parentheses" => num(if fmt.split(';').next().is_some_and(|p| p.contains('(')) { 1.0 } else { 0.0 }),
        "color" => num(if fmt.split(';').nth(1).is_some_and(|p| p.contains('[')) { 1.0 } else { 0.0 }),
        "prefix" => text(""),
        "protect" => num(1.0),
        "filename" | "sheetname" => {
            let Some(path) = &wb.path else { return text("") };
            if what == "sheetname" {
                return text(sh.name.clone());
            }
            let dir = path.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
            let sep = if dir.is_empty() || dir.ends_with('\\') { "" } else { "\\" };
            text(format!("{dir}{sep}[{}]{}", wb.file_name(), sh.name))
        }
        _ => {
            let _ = quote_sheet;
            Err(E::Value)
        }
    }
}

/// CELL("format") code of a number format ("G", "F2", ",0", "C2", "P1", "S2", "D1".."D9").
pub(super) fn format_code(fmt: &str) -> String {
    let f = fmt.split(';').next().unwrap_or("").to_ascii_lowercase();
    let neg = fmt.split(';').nth(1);
    let suffix = format!(
        "{}{}",
        if neg.is_some_and(|p| p.contains('[')) { "-" } else { "" },
        if fmt.split(';').next().is_some_and(|p| p.contains('(')) || neg.is_some_and(|p| p.contains('(')) { "()" } else { "" }
    );
    if f.is_empty() || f == "general" {
        return format!("G{suffix}");
    }
    let has = |s: &str| f.contains(s);
    let dec = f.split('.').nth(1).map_or(0, |d| d.chars().take_while(|c| *c == '0').count());
    let code = if has("y") || has("d") || (has("m") && !has("[")) || has("h") || has("s") && !has("e") {
        let date = has("y") || has("d") || (has("m") && !has("h") && !has(":"));
        if date {
            match () {
                _ if has("d") && has("mmm") && has("y") => "D1",
                _ if has("d") && has("mmm") => "D2",
                _ if has("mmm") && has("y") => "D3",
                _ if has("m") && has("d") && has("h") => "D4",
                _ if has("m") && has("d") && has("y") => "D4",
                _ if has("m") && has("d") => "D5",
                _ => "D4",
            }
            .to_string()
        } else {
            let ampm = has("am/pm") || has("a/p");
            match (has("s"), ampm) {
                (true, true) => "D6",
                (false, true) => "D7",
                (true, false) => "D8",
                (false, false) => "D9",
            }
            .to_string()
        }
    } else if has("%") {
        format!("P{dec}")
    } else if has("e+") || has("e-") {
        format!("S{dec}")
    } else if has("$") || has("€") || has("£") || has("[$") {
        format!("C{dec}")
    } else if has(",") {
        format!(",{dec}")
    } else {
        format!("F{dec}")
    };
    format!("{code}{suffix}")
}

// ---------------------------------------------------------------------------
// Database functions
// ---------------------------------------------------------------------------

/// One criteria cell: text without an operator matches values that begin with it (as in
/// Advanced Filter); `=text` matches exactly; other operators work as in COUNTIF.
fn db_crit(v: &Value) -> Criteria {
    match v {
        Value::Text(s) if !s.is_empty() && !s.starts_with(['=', '<', '>']) => match parse_criteria(v) {
            Criteria { op: CritOp::Eq, val: Value::Text(t) } => Criteria { op: CritOp::Eq, val: Value::Text(format!("{t}*")) },
            c => c,
        },
        _ => parse_criteria(v),
    }
}

fn database(ev: &mut Evaluator, name: &str, a: &[Expr]) -> Res {
    need(a, 2, 3)?;
    let db = grid_arg(ev, a, 0)?;
    if db.len() < 1 {
        return Err(E::Value);
    }
    let heads: Vec<String> = db[0].iter().map(|v| numfmt::value_to_text(v).to_lowercase()).collect();
    let field = if ev.has(a, 1) {
        Some(match ev.value(a, 1)? {
            Value::Number(x) => {
                let k = x.trunc() as i64;
                if k < 1 || k as usize > heads.len() {
                    return Err(E::Value);
                }
                k as usize - 1
            }
            Value::Text(t) => heads.iter().position(|h| *h == t.to_lowercase()).ok_or(E::Value)?,
            _ => return Err(E::Value),
        })
    } else {
        None
    };
    if field.is_none() && !matches!(name, "DCOUNT" | "DCOUNTA") {
        return Err(E::Value);
    }
    let crit = if a.len() > 2 { grid_arg(ev, a, 2)? } else { return Err(E::Value) };
    if crit.len() < 2 {
        return Err(E::Value);
    }
    // Criteria rows: (database column, criterion) pairs ANDed; rows ORed.
    let chead: Vec<Option<usize>> = crit[0].iter().map(|h| heads.iter().position(|x| *x == numfmt::value_to_text(h).to_lowercase())).collect();
    let rows: Vec<Vec<(usize, Criteria)>> = crit[1..]
        .iter()
        .map(|row| row.iter().zip(&chead).filter(|(v, _)| !v.is_empty()).filter_map(|(v, h)| h.map(|h| (h, db_crit(v)))).collect())
        .collect();
    let mut picked: Vec<Value> = Vec::new();
    for rec in &db[1..] {
        if rows.iter().any(|conds| conds.iter().all(|(col, c)| crit_match(c, &get(std::slice::from_ref(rec), 0, *col)))) {
            picked.push(field.map_or(Value::Number(0.0), |f| rec.get(f).cloned().unwrap_or_default()));
        }
    }
    let numbers: Vec<f64> = picked.iter().filter_map(|v| if let Value::Number(x) = v { Some(*x) } else { None }).collect();
    match name {
        "DCOUNT" => num(if field.is_none() { picked.len() } else { numbers.len() } as f64),
        "DCOUNTA" => num(picked.iter().filter(|v| !v.is_empty()).count() as f64),
        "DSUM" => num(numbers.iter().sum()),
        "DPRODUCT" => num(if numbers.is_empty() { 0.0 } else { numbers.iter().product() }),
        "DAVERAGE" => {
            if numbers.is_empty() { Err(E::Div0) } else { num(numbers.iter().sum::<f64>() / numbers.len() as f64) }
        }
        "DMAX" => num(numbers.iter().copied().reduce(f64::max).unwrap_or(0.0)),
        "DMIN" => num(numbers.iter().copied().reduce(f64::min).unwrap_or(0.0)),
        "DGET" => match picked.len() {
            0 => Err(E::Value),
            1 => match picked.remove(0) {
                Value::Error(e) => Err(e),
                v => Ok(EV::V(v)),
            },
            _ => Err(E::Num),
        },
        "DSTDEV" => num(stats(&numbers, true)?.sqrt()),
        "DSTDEVP" => num(stats(&numbers, false)?.sqrt()),
        "DVAR" => num(stats(&numbers, true)?),
        _ => num(stats(&numbers, false)?),
    }
}

// ---------------------------------------------------------------------------
// Regular expressions
// ---------------------------------------------------------------------------

/// Pattern argument `i` with the case-sensitivity flag at `ci` (0 sensitive, 1 insensitive).
fn regex(ev: &mut Evaluator, a: &[Expr], i: usize, ci: usize) -> Result<regex_lite::Regex, E> {
    let pat = ev.string(a, i)?;
    let insensitive = match ev.number_or(a, ci, 0.0)?.trunc() {
        0.0 => false,
        1.0 => true,
        _ => return Err(E::Value),
    };
    regex_lite::RegexBuilder::new(&pat).case_insensitive(insensitive).build().map_err(|_| E::Value)
}


// ---------------------------------------------------------------------------
// Distributions
// ---------------------------------------------------------------------------

fn binom_pmf(k: f64, n: f64, p: f64) -> f64 {
    if p == 0.0 {
        return if k == 0.0 { 1.0 } else { 0.0 };
    }
    if p == 1.0 {
        return if k == n { 1.0 } else { 0.0 };
    }
    if n < 1000.0 {
        if let Ok(c) = combin(n, k) {
            return c * p.powf(k) * (1.0 - p).powf(n - k);
        }
    }
    (ln_combin(n, k) + k * p.ln() + (n - k) * (1.0 - p).ln()).exp()
}

fn ln_combin(n: f64, k: f64) -> f64 {
    ln_gamma(n + 1.0) - ln_gamma(k + 1.0) - ln_gamma(n - k + 1.0)
}

fn ln_beta(a: f64, b: f64) -> f64 {
    ln_gamma(a) + ln_gamma(b) - ln_gamma(a + b)
}

fn gamma(x: f64) -> f64 {
    if x < 0.5 {
        PI / ((PI * x).sin() * ln_gamma(1.0 - x).exp())
    } else if x.fract() == 0.0 && x <= 171.0 {
        (1..x as u64).map(|i| i as f64).product()
    } else {
        ln_gamma(x).exp()
    }
}

fn gamma_pdf(x: f64, a: f64, b: f64) -> f64 {
    if x == 0.0 {
        return if a < 1.0 { f64::INFINITY } else if a == 1.0 { 1.0 / b } else { 0.0 };
    }
    ((a - 1.0) * x.ln() - x / b - a * b.ln() - ln_gamma(a)).exp()
}

/// Regularized lower incomplete gamma P(a, x).
fn gamma_p(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        0.0
    } else if x < a + 1.0 {
        gamma_series(a, x)
    } else {
        1.0 - gamma_cf(a, x)
    }
}

/// Regularized upper incomplete gamma Q(a, x) = 1 - P(a, x).
fn gamma_q(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        1.0
    } else if x < a + 1.0 {
        1.0 - gamma_series(a, x)
    } else {
        gamma_cf(a, x)
    }
}

fn gamma_series(a: f64, x: f64) -> f64 {
    let (mut ap, mut del) = (a, 1.0 / a);
    let mut sum = del;
    for _ in 0..10_000 {
        ap += 1.0;
        del *= x / ap;
        sum += del;
        if del.abs() < sum.abs() * 1e-17 {
            break;
        }
    }
    sum * (-x + a * x.ln() - ln_gamma(a)).exp()
}

fn gamma_cf(a: f64, x: f64) -> f64 {
    const TINY: f64 = 1e-300;
    let mut b = x + 1.0 - a;
    let mut c = 1.0 / TINY;
    let mut d = 1.0 / b;
    let mut h = d;
    for i in 1..10_000 {
        let an = -(i as f64) * (i as f64 - a);
        b += 2.0;
        d = an * d + b;
        if d.abs() < TINY {
            d = TINY;
        }
        c = b + an / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < 1e-17 {
            break;
        }
    }
    (-x + a * x.ln() - ln_gamma(a)).exp() * h
}

fn erf(x: f64) -> f64 {
    2.0 * norm_cdf(x * std::f64::consts::SQRT_2) - 1.0
}

/// Inverse of an increasing function `f` on [lo, ∞) at `p` (bracket expansion + bisection).
fn invert(f: impl Fn(f64) -> f64, p: f64, lo: f64, hi: f64) -> Result<f64, E> {
    let (mut lo, mut hi) = (lo, hi.max(lo + 1e-300));
    let mut guard = 0;
    while f(hi) < p {
        lo = hi;
        hi *= 2.0;
        guard += 1;
        if guard > 2000 || !hi.is_finite() {
            return Err(E::Num);
        }
    }
    for _ in 0..300 {
        let mid = 0.5 * (lo + hi);
        if mid <= lo || mid >= hi {
            break;
        }
        if f(mid) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok(0.5 * (lo + hi))
}

// ---------------------------------------------------------------------------
// Matrices
// ---------------------------------------------------------------------------

fn matrix(g: G) -> Result<Vec<Vec<f64>>, E> {
    if g.is_empty() || g[0].is_empty() {
        return Err(E::Value);
    }
    g.into_iter()
        .map(|r| {
            r.into_iter()
                .map(|v| match v {
                    Value::Number(x) => Ok(x),
                    Value::Error(e) => Err(e),
                    _ => Err(E::Value),
                })
                .collect()
        })
        .collect()
}

/// LU decomposition with partial pivoting: (factors, determinant); None when singular.
fn lu(mut m: Vec<Vec<f64>>) -> Option<(Vec<Vec<f64>>, f64)> {
    let k = m.len();
    let mut det = 1.0;
    for i in 0..k {
        let p = (i..k).max_by(|&x, &y| m[x][i].abs().total_cmp(&m[y][i].abs()))?;
        if m[p][i] == 0.0 {
            return None;
        }
        if p != i {
            m.swap(p, i);
            det = -det;
        }
        det *= m[i][i];
        for r in i + 1..k {
            let f = m[r][i] / m[i][i];
            for c in i..k {
                m[r][c] -= f * m[i][c];
            }
        }
    }
    Some((m, det))
}

fn invert_matrix(mut m: Vec<Vec<f64>>) -> Option<Vec<Vec<f64>>> {
    let k = m.len();
    let mut inv: Vec<Vec<f64>> = (0..k).map(|i| (0..k).map(|j| if i == j { 1.0 } else { 0.0 }).collect()).collect();
    for i in 0..k {
        let p = (i..k).max_by(|&x, &y| m[x][i].abs().total_cmp(&m[y][i].abs()))?;
        if m[p][i].abs() < 1e-300 {
            return None;
        }
        m.swap(p, i);
        inv.swap(p, i);
        let d = m[i][i];
        for c in 0..k {
            m[i][c] /= d;
            inv[i][c] /= d;
        }
        for r in 0..k {
            if r != i {
                let f = m[r][i];
                if f != 0.0 {
                    for c in 0..k {
                        m[r][c] -= f * m[i][c];
                        inv[r][c] -= f * inv[i][c];
                    }
                }
            }
        }
    }
    Some(inv)
}
