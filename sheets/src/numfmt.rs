//! Number format codes, value display, input parsing and date serial helpers.

use crate::model::Value;

// ---------------------------------------------------------------------------
// Dates (1900 date system, including the historic 1900-02-29 leap-year quirk)
// ---------------------------------------------------------------------------

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Days since 1970-01-01 of serial 0 for serials >= 61 (1899-12-30).
const EPOCH_1899_12_30: i64 = -25569;

/// Date (year, month, day) to serial. Month/day may overflow (DATE(2024,14,1)).
pub fn ymd_to_serial(y: i64, m: i64, d: i64) -> f64 {
    let y = if (0..1900).contains(&y) { y + 1900 } else { y };
    let total_m = y * 12 + (m - 1);
    let (yy, mm) = (total_m.div_euclid(12), total_m.rem_euclid(12) + 1);
    let days = days_from_civil(yy, mm, 1) + (d - 1);
    let serial = days - EPOCH_1899_12_30;
    // Serials before 1900-03-01 are shifted by the fictional 1900-02-29.
    if serial < 61 { (serial - 1) as f64 } else { serial as f64 }
}

/// Serial to (year, month, day).
pub fn serial_to_ymd(serial: f64) -> (i64, i64, i64) {
    let s = serial.floor() as i64;
    if s == 60 {
        return (1900, 2, 29);
    }
    if s == 0 {
        return (1900, 1, 0);
    }
    let s = if s < 60 { s + 1 } else { s };
    civil_from_days(s + EPOCH_1899_12_30)
}

/// Fraction of a day to (hour, minute, second, millis).
pub fn serial_to_hms(serial: f64) -> (i64, i64, i64, i64) {
    let frac = serial - serial.floor();
    let total_ms = (frac * 86_400_000.0).round() as i64;
    let total_ms = total_ms.min(86_399_999);
    (total_ms / 3_600_000, (total_ms / 60_000) % 60, (total_ms / 1000) % 60, total_ms % 1000)
}

pub fn weekday(serial: f64) -> i64 {
    // 1 = Sunday
    let s = serial.floor() as i64;
    (s + 6).rem_euclid(7) + 1
}

pub fn now_serial() -> f64 {
    use chrono::{Datelike, Local, Timelike};
    let n = Local::now();
    ymd_to_serial(n.year() as i64, n.month() as i64, n.day() as i64)
        + (n.hour() as f64 * 3600.0 + n.minute() as f64 * 60.0 + n.second() as f64) / 86400.0
}

pub fn days_in_month(y: i64, m: i64) -> i64 {
    let next = if m == 12 { days_from_civil(y + 1, 1, 1) } else { days_from_civil(y, m + 1, 1) };
    next - days_from_civil(y, m, 1)
}

const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November",
    "December",
];
const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

// ---------------------------------------------------------------------------
// General format
// ---------------------------------------------------------------------------

/// Renders a number the way the "General" format does (about 11 characters).
pub fn general(n: f64) -> String {
    if n == 0.0 {
        return "0".into();
    }
    if !n.is_finite() {
        return "#NUM!".into();
    }
    let a = n.abs();
    if (1e-9..1e11).contains(&a) {
        // up to 10 significant digits
        // Fit in about 11 characters: "0." + 9 decimals, or k integer digits + "." + (10 - k).
        let digits = if a >= 1.0 { 10 - (a.log10().floor() as i32 + 1) } else { 9 };
        let digits = digits.clamp(0, 15) as usize;
        let s = format!("{:.*}", digits, n);
        let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
        if s == "-0" { "0".into() } else { s }
    } else {
        let s = format!("{:.5E}", n);
        // "1.23450E11" -> "1.2345E+11"
        let (mant, exp) = s.split_once('E').unwrap();
        let mant = if mant.contains('.') { mant.trim_end_matches('0').trim_end_matches('.') } else { mant };
        let e: i32 = exp.parse().unwrap_or(0);
        format!("{}E{}{:02}", mant, if e < 0 { '-' } else { '+' }, e.abs())
    }
}

/// Full-precision text for editing (15 significant digits, like the formula bar).
pub fn edit_number(n: f64) -> String {
    if n == 0.0 || !n.is_finite() {
        return general(n);
    }
    let exp = n.abs().log10().floor() as i32;
    if !(-9..15).contains(&exp) {
        let s = format!("{:.14E}", n);
        let (mant, e) = s.split_once('E').unwrap_or((&s, "0"));
        let mant = if mant.contains('.') { mant.trim_end_matches('0').trim_end_matches('.') } else { mant };
        let e: i32 = e.parse().unwrap_or(0);
        return format!("{}E{}{:02}", mant, if e < 0 { '-' } else { '+' }, e.abs());
    }
    let decimals = (14 - exp).clamp(0, 20) as usize;
    let s = format!("{:.*}", decimals, n);
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s == "-0" { "0".into() } else { s }
}

/// Editing text for a date/time-formatted serial (re-enterable by `parse_input`).
pub fn edit_date(n: f64) -> String {
    let has_time = n.fract().abs() > 1e-9;
    if n < 1.0 && n >= 0.0 {
        return format_value(&Value::Number(n), "h:mm:ss AM/PM").text;
    }
    if has_time {
        format_value(&Value::Number(n), "m/d/yyyy h:mm:ss").text
    } else {
        format_value(&Value::Number(n), "m/d/yyyy").text
    }
}

pub fn value_to_text(v: &Value) -> String {
    match v {
        Value::Empty => String::new(),
        Value::Number(n) => general(*n),
        Value::Text(s) => s.clone(),
        Value::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
        Value::Error(e) => e.as_str().into(),
    }
}

// ---------------------------------------------------------------------------
// Input parsing
// ---------------------------------------------------------------------------

/// Parses plain numeric text ("1,234.5", "-3", "1e5"). No currency/percent.
pub fn parse_number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let (neg, t) = if t.starts_with('(') && t.ends_with(')') { (true, &t[1..t.len() - 1]) } else { (false, t) };
    let cleaned: String = t.chars().filter(|&c| c != ',').collect();
    if cleaned.is_empty()
        || !cleaned.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
        || !cleaned.chars().any(|c| c.is_ascii_digit())
    {
        return None;
    }
    // thousands separators must be in proper groups
    if t.contains(',') {
        let int_part = t.trim_start_matches(['-', '+']).split('.').next().unwrap_or("");
        let groups: Vec<&str> = int_part.split(',').collect();
        if groups[0].is_empty() || groups[0].len() > 3 || groups[1..].iter().any(|g| g.len() != 3) {
            return None;
        }
    }
    let v: f64 = cleaned.parse().ok()?;
    if !v.is_finite() {
        return None;
    }
    Some(if neg { -v } else { v })
}

/// Parses a date like 2026-01-31, 1/31/2026, 31.01.2026, 31-Jan-2026, Jan 31 2026.
pub fn parse_date(s: &str) -> Option<(f64, &'static str)> {
    let t = s.trim();
    let parts: Vec<&str> = t.split(['/', '-', '.', ' ', ',']).filter(|p| !p.is_empty()).collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let month_name = |p: &str| -> Option<i64> {
        if p.len() < 3 || p.chars().any(|c| !c.is_ascii_alphabetic()) {
            return None;
        }
        MONTHS.iter().position(|m| m.to_ascii_lowercase().starts_with(&p.to_ascii_lowercase())).map(|i| i as i64 + 1)
    };
    let num = |p: &str| -> Option<i64> { if p.chars().all(|c| c.is_ascii_digit()) { p.parse().ok() } else { None } };
    let this_year = {
        let (y, _, _) = serial_to_ymd(now_serial());
        y
    };
    let fix_year = |y: i64, len: usize| -> i64 {
        if len <= 2 { if y < 30 { 2000 + y } else { 1900 + y } } else { y }
    };
    let (y, m, d, fmt);
    if parts.len() == 3 {
        if let (Some(a), Some(b), Some(c)) = (num(parts[0]), num(parts[1]), num(parts[2])) {
            if parts[0].len() == 4 {
                (y, m, d, fmt) = (a, b, c, "yyyy-mm-dd");
            } else if t.contains('.') {
                (y, m, d, fmt) = (fix_year(c, parts[2].len()), b, a, "dd.mm.yyyy");
            } else {
                (y, m, d, fmt) = (fix_year(c, parts[2].len()), a, b, "m/d/yyyy");
            }
        } else if let (Some(a), Some(mm), Some(c)) = (num(parts[0]), month_name(parts[1]), num(parts[2])) {
            (y, m, d, fmt) = (fix_year(c, parts[2].len()), mm, a, "d-mmm-yy");
        } else if let (Some(mm), Some(b), Some(c)) = (month_name(parts[0]), num(parts[1]), num(parts[2])) {
            (y, m, d, fmt) = (fix_year(c, parts[2].len()), mm, b, "mmmm d, yyyy");
        } else {
            return None;
        }
    } else if let (Some(a), Some(mm)) = (num(parts[0]), month_name(parts[1])) {
        (y, m, d, fmt) = (this_year, mm, a, "d-mmm");
    } else if let (Some(mm), Some(b)) = (month_name(parts[0]), num(parts[1])) {
        if b > 31 {
            (y, m, d, fmt) = (fix_year(b, parts[1].len()), mm, 1, "mmm-yy");
        } else {
            (y, m, d, fmt) = (this_year, mm, b, "d-mmm");
        }
    } else if let (Some(a), Some(b)) = (num(parts[0]), num(parts[1])) {
        if !t.contains('/') || a > 12 || b > 31 || a == 0 || b == 0 {
            return None;
        }
        (y, m, d, fmt) = (this_year, a, b, "d-mmm");
    } else {
        return None;
    }
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) || !(1900..=9999).contains(&y) {
        return None;
    }
    Some((ymd_to_serial(y, m, d), fmt))
}

/// Parses "13:45", "1:30 PM", "13:45:10".
pub fn parse_time(s: &str) -> Option<(f64, &'static str)> {
    let t = s.trim().to_ascii_uppercase();
    let (body, pm, ampm) = if let Some(b) = t.strip_suffix("PM") {
        (b.trim().to_string(), true, true)
    } else if let Some(b) = t.strip_suffix("AM") {
        (b.trim().to_string(), false, true)
    } else {
        (t.clone(), false, false)
    };
    let parts: Vec<&str> = body.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 || parts.iter().any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit() || c == '.')) {
        return None;
    }
    let mut h: f64 = parts[0].parse().ok()?;
    let m: f64 = parts[1].parse().ok()?;
    let sec: f64 = if parts.len() == 3 { parts[2].parse().ok()? } else { 0.0 };
    if ampm {
        if h > 12.0 {
            return None;
        }
        if pm && h < 12.0 {
            h += 12.0;
        }
        if !pm && h == 12.0 {
            h = 0.0;
        }
    }
    if m >= 60.0 || sec >= 60.0 {
        return None;
    }
    let fmt = match (ampm, parts.len()) {
        (true, 3) => "h:mm:ss AM/PM",
        (true, _) => "h:mm AM/PM",
        (false, 3) => "h:mm:ss",
        (false, _) => "h:mm",
    };
    Some(((h * 3600.0 + m * 60.0 + sec) / 86400.0, fmt))
}

/// Converts typed text into a value plus an implied number format, like the grid does on entry.
pub fn parse_input(text: &str) -> (Value, Option<&'static str>) {
    if let Some(rest) = text.strip_prefix('\'') {
        return (Value::Text(rest.to_string()), None);
    }
    let t = text.trim();
    if t.eq_ignore_ascii_case("TRUE") {
        return (Value::Bool(true), None);
    }
    if t.eq_ignore_ascii_case("FALSE") {
        return (Value::Bool(false), None);
    }
    if let Some(e) = crate::model::ErrorKind::parse(t) {
        return (Value::Error(e), None);
    }
    if let Some(n) = parse_number(t) {
        return (Value::Number(n), if t.contains(',') { Some("#,##0.##") } else { None });
    }
    if let Some(p) = t.strip_suffix('%') {
        if let Some(n) = parse_number(p) {
            return (Value::Number(n / 100.0), Some(if p.contains('.') { "0.00%" } else { "0%" }));
        }
    }
    for (sym, fmt) in [("$", "\"$\"#,##0.00"), ("€", "#,##0.00 \"€\""), ("£", "\"£\"#,##0.00")] {
        let stripped = t.strip_prefix(sym).or_else(|| t.strip_suffix(sym)).map(str::trim);
        let neg_stripped = t.strip_prefix('-').and_then(|x| x.trim().strip_prefix(sym)).map(|x| format!("-{x}"));
        if let Some(n) = stripped.and_then(parse_number).or_else(|| neg_stripped.as_deref().and_then(parse_number)) {
            return (Value::Number(n), Some(fmt));
        }
    }
    if let Some((n, fmt)) = parse_date(t) {
        return (Value::Number(n), Some(fmt));
    }
    if let Some((n, fmt)) = parse_time(t) {
        return (Value::Number(n), Some(fmt));
    }
    // date + time
    if let Some((d, tm)) = t.rsplit_once(' ') {
        if let (Some((dn, _)), Some((tn, _))) = (parse_date(d), parse_time(tm)) {
            return (Value::Number(dn + tn), Some("m/d/yyyy h:mm"));
        }
    }
    (Value::Text(text.to_string()), None)
}

// ---------------------------------------------------------------------------
// Format codes
// ---------------------------------------------------------------------------

pub struct Formatted {
    pub text: String,
    pub color: Option<u32>,
}

fn split_sections(fmt: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut in_q = false;
    let mut in_b = false;
    let mut esc = false;
    for ch in fmt.chars() {
        if esc {
            out.last_mut().unwrap().push(ch);
            esc = false;
            continue;
        }
        match ch {
            '\\' => esc = true,
            '"' => in_q = !in_q,
            '[' if !in_q => in_b = true,
            ']' if !in_q => in_b = false,
            ';' if !in_q && !in_b => {
                out.push(String::new());
                continue;
            }
            _ => {}
        }
        if ch == '\\' {
            out.last_mut().unwrap().push(ch);
        } else {
            out.last_mut().unwrap().push(ch);
        }
    }
    out
}

fn color_name(n: &str) -> Option<u32> {
    Some(match n.to_ascii_lowercase().as_str() {
        "red" => 0xFF0000,
        "blue" => 0x0000FF,
        "green" => 0x00A000,
        "black" => 0x000000,
        "white" => 0xFFFFFF,
        "yellow" => 0xFFFF00,
        "magenta" => 0xFF00FF,
        "cyan" => 0x00FFFF,
        _ => return None,
    })
}

#[derive(Debug)]
enum Tok {
    Lit(String),
    Digits(String), // run of 0 # ? , .
    Date(String),   // y m d h s ...
    AmPm(bool),     // true = "AM/PM", false = "A/P"
    Percent,
    Exp(String),    // E+ / E-
    At,
    Skip,           // _x padding
}

fn is_date_fmt(sec: &str) -> bool {
    let mut in_q = false;
    let mut in_b = false;
    let mut prev = ' ';
    for ch in sec.chars() {
        match ch {
            '"' => in_q = !in_q,
            '[' if !in_q => in_b = true,
            ']' if !in_q => in_b = false,
            _ if in_q || in_b || prev == '\\' || prev == '_' => {}
            'y' | 'Y' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' | 'm' | 'M' => return true,
            _ => {}
        }
        prev = ch;
    }
    false
}

fn tokenize(sec: &str) -> (Vec<Tok>, Option<u32>, Option<String>) {
    let mut toks = Vec::new();
    let mut color = None;
    let mut cond = None;
    let ch: Vec<char> = sec.chars().collect();
    let mut i = 0;
    let date = is_date_fmt(sec);
    while i < ch.len() {
        let c = ch[i];
        match c {
            '"' => {
                let mut s = String::new();
                i += 1;
                while i < ch.len() && ch[i] != '"' {
                    s.push(ch[i]);
                    i += 1;
                }
                toks.push(Tok::Lit(s));
            }
            '\\' => {
                if i + 1 < ch.len() {
                    toks.push(Tok::Lit(ch[i + 1].to_string()));
                    i += 1;
                }
            }
            '_' => {
                i += 1;
                toks.push(Tok::Skip);
            }
            '*' => {
                i += 1; // fill character: ignored
            }
            '[' => {
                let mut s = String::new();
                i += 1;
                while i < ch.len() && ch[i] != ']' {
                    s.push(ch[i]);
                    i += 1;
                }
                if let Some(col) = color_name(&s) {
                    color = Some(col);
                } else if let Some(cur) = s.strip_prefix('$') {
                    let sym = cur.split('-').next().unwrap_or("");
                    toks.push(Tok::Lit(sym.to_string()));
                } else if s.starts_with(['<', '>', '=']) {
                    cond = Some(s);
                } else if date && s.chars().all(|c| matches!(c.to_ascii_lowercase(), 'h' | 'm' | 's')) {
                    toks.push(Tok::Date(format!("[{}]", s.to_ascii_lowercase())));
                }
            }
            '@' => toks.push(Tok::At),
            '%' => toks.push(Tok::Percent),
            'E' | 'e' if !date && i + 1 < ch.len() && matches!(ch[i + 1], '+' | '-') => {
                let mut s = String::from(ch[i + 1]);
                i += 2;
                while i < ch.len() && matches!(ch[i], '0' | '#' | '?') {
                    s.push(ch[i]);
                    i += 1;
                }
                toks.push(Tok::Exp(s));
                continue;
            }
            '0' | '#' | '?' | '.' | ',' if !date || (c == '.' || c == '0') => {
                if date {
                    // fractional seconds like ss.00
                    let mut s = String::new();
                    while i < ch.len() && matches!(ch[i], '0' | '.') {
                        s.push(ch[i]);
                        i += 1;
                    }
                    toks.push(Tok::Date(s));
                    continue;
                }
                let mut s = String::new();
                while i < ch.len() && matches!(ch[i], '0' | '#' | '?' | '.' | ',') {
                    s.push(ch[i]);
                    i += 1;
                }
                toks.push(Tok::Digits(s));
                continue;
            }
            'A' | 'a' if date && sec[byte_index(&ch, i)..].to_ascii_uppercase().starts_with("AM/PM") => {
                toks.push(Tok::AmPm(true));
                i += 5;
                continue;
            }
            'A' | 'a' if date && sec[byte_index(&ch, i)..].to_ascii_uppercase().starts_with("A/P") => {
                toks.push(Tok::AmPm(false));
                i += 3;
                continue;
            }
            'y' | 'Y' | 'm' | 'M' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' if date => {
                let lc = c.to_ascii_lowercase();
                let mut s = String::new();
                while i < ch.len() && ch[i].to_ascii_lowercase() == lc {
                    s.push(lc);
                    i += 1;
                }
                toks.push(Tok::Date(s));
                continue;
            }
            _ => toks.push(Tok::Lit(c.to_string())),
        }
        i += 1;
    }
    (toks, color, cond)
}

fn byte_index(ch: &[char], i: usize) -> usize {
    ch[..i].iter().map(|c| c.len_utf8()).sum()
}

fn group_thousands(int: &str) -> String {
    let bytes: Vec<char> = int.chars().collect();
    let mut out = String::new();
    for (i, c) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*c);
    }
    out
}

fn format_number_section(n: f64, toks: &[Tok], auto_minus: bool) -> String {
    let digits: String = toks.iter().filter_map(|t| if let Tok::Digits(d) = t { Some(d.as_str()) } else { None }).collect();
    let percent = toks.iter().filter(|t| matches!(t, Tok::Percent)).count();
    let exp = toks.iter().find_map(|t| if let Tok::Exp(e) = t { Some(e.clone()) } else { None });
    let mut v = n * 100f64.powi(percent as i32);
    // trailing commas scale by 1000
    let mut trailing = 0;
    {
        let t = digits.trim_end_matches(|c| c != ',' && c != '0' && c != '#' && c != '?');
        for c in t.chars().rev() {
            if c == ',' { trailing += 1 } else { break }
        }
    }
    v /= 1000f64.powi(trailing);
    let core = digits.trim_end_matches(',');
    let (int_pat, dec_pat) = match core.split_once('.') {
        Some((a, b)) => (a.to_string(), b.replace(',', "")),
        None => (core.to_string(), String::new()),
    };
    let thousands = int_pat.contains(',');
    let int_min = int_pat.chars().filter(|&c| c == '0').count();
    let dec_req = dec_pat.chars().filter(|&c| c == '0').count();
    let dec_max = dec_pat.chars().filter(|&c| matches!(c, '0' | '#' | '?')).count();
    let neg = v < 0.0;
    let a = v.abs();

    let body = if let Some(e) = exp {
        let exp_digits = e.chars().skip(1).count().max(1);
        let (mant, ex) = if a == 0.0 {
            (0.0, 0)
        } else {
            let ex = a.log10().floor() as i32;
            (a / 10f64.powi(ex), ex)
        };
        let mut m = format!("{:.*}", dec_max, mant);
        let mut ex = ex;
        if m.starts_with("10") {
            m = format!("{:.*}", dec_max, mant / 10.0);
            ex += 1;
        }
        let sign = if ex < 0 { "-" } else if e.starts_with('+') { "+" } else { "" };
        format!("{}E{}{:0width$}", m, sign, ex.abs(), width = exp_digits)
    } else {
        let s = format!("{:.*}", dec_max, a);
        let (ip, dp) = match s.split_once('.') {
            Some((x, y)) => (x.to_string(), y.to_string()),
            None => (s.clone(), String::new()),
        };
        let mut dp = dp;
        while dp.len() > dec_req && dp.ends_with('0') {
            dp.pop();
        }
        let mut ip = ip.trim_start_matches('0').to_string();
        while ip.len() < int_min {
            ip.insert(0, '0');
        }
        if thousands {
            ip = group_thousands(&ip);
        }
        if dec_pat.is_empty() && !core.contains('.') {
            ip
        } else if dp.is_empty() && dec_req == 0 && dec_max > 0 {
            format!("{ip}.")
        } else {
            format!("{ip}.{dp}")
        }
    };

    let mut out = String::new();
    let mut placed = false;
    let is_zero_result = body.chars().all(|c| !c.is_ascii_digit() || c == '0');
    if neg && auto_minus && !is_zero_result {
        out.push('-');
    }
    for t in toks {
        match t {
            Tok::Lit(s) => out.push_str(s),
            Tok::Digits(_) | Tok::Exp(_) => {
                if !placed {
                    out.push_str(&body);
                    placed = true;
                }
            }
            Tok::Percent => out.push('%'),
            Tok::Skip => out.push(' '),
            Tok::At | Tok::Date(_) | Tok::AmPm(_) => {}
        }
    }
    if !placed {
        // format has no digit placeholders, e.g. "-" for zero
    }
    out
}

fn format_date_section(n: f64, toks: &[Tok]) -> String {
    if !(0.0..2_958_466.0).contains(&n) {
        return "#".repeat(8);
    }
    let (y, mo, d) = serial_to_ymd(n);
    let (mut h, mi, s, ms) = serial_to_hms(n);
    let has_ampm = toks.iter().any(|t| matches!(t, Tok::AmPm(_)));
    let pm = h >= 12;
    if has_ampm {
        h %= 12;
        if h == 0 {
            h = 12;
        }
    }
    // Decide whether an 'm' token is minutes: after h or before s.
    let mut out = String::new();
    for (i, t) in toks.iter().enumerate() {
        match t {
            Tok::Lit(s) => out.push_str(s),
            Tok::Skip => out.push(' '),
            Tok::AmPm(full) => {
                out.push_str(match (full, pm) {
                    (true, true) => "PM",
                    (true, false) => "AM",
                    (false, true) => "P",
                    (false, false) => "A",
                });
            }
            Tok::Date(code) => {
                let is_minute = code.starts_with('m') && code.len() <= 2 && {
                    let prev_h = toks[..i].iter().rev().find_map(|t| if let Tok::Date(c) = t { Some(c.clone()) } else { None });
                    let next_s = toks[i + 1..].iter().find_map(|t| if let Tok::Date(c) = t { Some(c.clone()) } else { None });
                    prev_h.is_some_and(|c| c.starts_with('h') || c.starts_with("[h")) || next_s.is_some_and(|c| c.starts_with('s'))
                };
                let piece = match code.as_str() {
                    "yy" | "y" => format!("{:02}", y % 100),
                    c if c.starts_with('y') => format!("{y}"),
                    "m" if is_minute => format!("{mi}"),
                    "mm" if is_minute => format!("{mi:02}"),
                    "m" => format!("{mo}"),
                    "mm" => format!("{mo:02}"),
                    "mmm" => MONTHS[(mo - 1) as usize][..3].to_string(),
                    "mmmmm" => MONTHS[(mo - 1) as usize][..1].to_string(),
                    c if c.starts_with("mmmm") => MONTHS[(mo - 1) as usize].to_string(),
                    "d" => format!("{d}"),
                    "dd" => format!("{d:02}"),
                    "ddd" => DAYS[(weekday(n) - 1) as usize][..3].to_string(),
                    c if c.starts_with("dddd") => DAYS[(weekday(n) - 1) as usize].to_string(),
                    "h" => format!("{h}"),
                    c if c.starts_with("hh") => format!("{h:02}"),
                    "s" => format!("{s}"),
                    c if c.starts_with("ss") => format!("{s:02}"),
                    "[h]" | "[hh]" => format!("{}", (n * 24.0).floor() as i64),
                    "[m]" | "[mm]" => format!("{}", (n * 1440.0).floor() as i64),
                    "[s]" | "[ss]" => format!("{}", (n * 86400.0).round() as i64),
                    c if c.starts_with('.') => {
                        let places = c.len() - 1;
                        let frac = format!("{:03}", ms);
                        format!(".{}", &frac[..places.min(3)])
                    }
                    c => c.to_string(),
                };
                out.push_str(&piece);
            }
            _ => {}
        }
    }
    out
}

fn cond_matches(cond: &str, n: f64) -> bool {
    let (op, rest) = if let Some(r) = cond.strip_prefix(">=") {
        (">=", r)
    } else if let Some(r) = cond.strip_prefix("<=") {
        ("<=", r)
    } else if let Some(r) = cond.strip_prefix("<>") {
        ("<>", r)
    } else if let Some(r) = cond.strip_prefix('>') {
        (">", r)
    } else if let Some(r) = cond.strip_prefix('<') {
        ("<", r)
    } else if let Some(r) = cond.strip_prefix('=') {
        ("=", r)
    } else {
        return false;
    };
    let Ok(v) = rest.trim().parse::<f64>() else { return false };
    match op {
        ">=" => n >= v,
        "<=" => n <= v,
        "<>" => n != v,
        ">" => n > v,
        "<" => n < v,
        _ => n == v,
    }
}

/// Formats a value with a number format code.
pub fn format_value(v: &Value, fmt: &str) -> Formatted {
    let plain = |s: String| Formatted { text: s, color: None };
    let fmt = fmt.trim();
    let is_general = fmt.is_empty() || fmt.eq_ignore_ascii_case("general");
    match v {
        Value::Empty => plain(String::new()),
        Value::Bool(b) => plain(if *b { "TRUE".into() } else { "FALSE".into() }),
        Value::Error(e) => plain(e.as_str().into()),
        Value::Text(s) => {
            if is_general {
                return plain(s.clone());
            }
            let secs = split_sections(fmt);
            if secs.len() >= 4 || (secs.len() == 1 && secs[0].contains('@')) {
                let sec = if secs.len() >= 4 { &secs[3] } else { &secs[0] };
                let (toks, color, _) = tokenize(sec);
                let mut out = String::new();
                for t in toks {
                    match t {
                        Tok::Lit(l) => out.push_str(&l),
                        Tok::At => out.push_str(s),
                        Tok::Skip => out.push(' '),
                        _ => {}
                    }
                }
                Formatted { text: out, color }
            } else {
                plain(s.clone())
            }
        }
        Value::Number(n) => {
            let n = *n;
            if is_general {
                return plain(general(n));
            }
            if fmt == "@" {
                return plain(general(n));
            }
            let secs = split_sections(fmt);
            let parsed: Vec<_> = secs.iter().map(|s| tokenize(s)).collect();
            // choose section
            let has_cond = parsed.iter().any(|p| p.2.is_some());
            let (idx, auto_minus) = if has_cond {
                let mut chosen = None;
                for (i, p) in parsed.iter().enumerate() {
                    if let Some(c) = &p.2 {
                        if cond_matches(c, n) {
                            chosen = Some(i);
                            break;
                        }
                    }
                }
                match chosen {
                    Some(i) => (i, false),
                    None => (parsed.iter().position(|p| p.2.is_none()).unwrap_or(0), true),
                }
            } else {
                match parsed.len() {
                    1 => (0, true),
                    2 => if n < 0.0 { (1, false) } else { (0, true) },
                    _ => if n < 0.0 { (1, false) } else if n == 0.0 { (2, true) } else { (0, true) },
                }
            };
            let (toks, color, _) = &parsed[idx.min(parsed.len() - 1)];
            let sec = &secs[idx.min(secs.len() - 1)];
            if sec.eq_ignore_ascii_case("general") {
                return Formatted { text: general(if auto_minus { n } else { n.abs() }), color: *color };
            }
            let text = if is_date_fmt(sec) {
                format_date_section(n, toks)
            } else {
                format_number_section(if auto_minus { n } else { n.abs() }, toks, true)
            };
            Formatted { text, color: *color }
        }
    }
}

/// True if the format code renders dates or times.
pub fn is_date_format(fmt: &str) -> bool {
    let f = fmt.trim();
    !f.is_empty() && !f.eq_ignore_ascii_case("general") && split_sections(f).first().is_some_and(|s| is_date_fmt(s))
}

/// Built-in number format ids used by the xlsx format.
pub fn builtin_format(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        5 => "\"$\"#,##0_);(\"$\"#,##0)",
        6 => "\"$\"#,##0_);[Red](\"$\"#,##0)",
        7 => "\"$\"#,##0.00_);(\"$\"#,##0.00)",
        8 => "\"$\"#,##0.00_);[Red](\"$\"#,##0.00)",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        12 => "# ?/?",
        13 => "# ??/??",
        14 => "m/d/yyyy",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        22 => "m/d/yyyy h:mm",
        37 => "#,##0_);(#,##0)",
        38 => "#,##0_);[Red](#,##0)",
        39 => "#,##0.00_);(#,##0.00)",
        40 => "#,##0.00_);[Red](#,##0.00)",
        41 => "_(* #,##0_);_(* (#,##0);_(* \"-\"_);_(@_)",
        42 => "_(\"$\"* #,##0_);_(\"$\"* (#,##0);_(\"$\"* \"-\"_);_(@_)",
        43 => "_(* #,##0.00_);_(* (#,##0.00);_(* \"-\"??_);_(@_)",
        44 => "_(\"$\"* #,##0.00_);_(\"$\"* (#,##0.00);_(\"$\"* \"-\"??_);_(@_)",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mm:ss.0",
        48 => "##0.0E+0",
        49 => "@",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_roundtrip() {
        assert_eq!(ymd_to_serial(1900, 1, 1), 1.0);
        assert_eq!(ymd_to_serial(1900, 3, 1), 61.0);
        assert_eq!(ymd_to_serial(2026, 11, 4), 46330.0);
        assert_eq!(serial_to_ymd(46330.0), (2026, 11, 4));
        assert_eq!(serial_to_ymd(60.0), (1900, 2, 29));
        assert_eq!(serial_to_ymd(1.0), (1900, 1, 1));
    }

    #[test]
    fn general_format() {
        assert_eq!(general(1.0), "1");
        assert_eq!(general(0.1 + 0.2), "0.3");
        assert_eq!(general(123456789012.0), "1.23457E+11");
        assert_eq!(general(-2.5), "-2.5");
        assert_eq!(general(1.0 / 3.0), "0.333333333");
    }

    #[test]
    fn number_formats() {
        let f = |n: f64, c: &str| format_value(&Value::Number(n), c).text;
        assert_eq!(f(1234.567, "#,##0.00"), "1,234.57");
        assert_eq!(f(0.256, "0%"), "26%");
        assert_eq!(f(0.256, "0.00%"), "25.60%");
        assert_eq!(f(-5.0, "0.00"), "-5.00");
        assert_eq!(f(-5.0, "0.00;(0.00)"), "(5.00)");
        assert_eq!(f(12345.0, "0.00E+00"), "1.23E+04");
        assert_eq!(f(46330.0, "m/d/yyyy"), "11/4/2026");
        assert_eq!(f(46330.0, "dd.mm.yyyy"), "04.11.2026");
        assert_eq!(f(46330.5, "h:mm AM/PM"), "12:00 PM");
        assert_eq!(f(46330.0, "d-mmm-yy"), "4-Nov-26");
        assert_eq!(f(1234.5, "\"$\"#,##0.00"), "$1,234.50");
        assert_eq!(f(0.0, "_(* #,##0.00_);_(* (#,##0.00);_(* \"-\"??_);_(@_)").trim(), "-");
        assert_eq!(f(1.5, "[h]:mm"), "36:00");
    }

    #[test]
    fn edit_text_round_trips() {
        for n in [123456789012.0, 1.0 / 3.0, 0.1 + 0.2, 1e20, 1.5e-12, -42.125, 46330.0] {
            let t = edit_number(n);
            let back = match parse_input(&t).0 {
                Value::Number(x) => x,
                v => panic!("{t} -> {v:?}"),
            };
            assert!((back - n).abs() <= n.abs() * 1e-14, "{n} -> {t} -> {back}");
        }
        assert_eq!(edit_number(123456789012.0), "123456789012");
        assert_eq!(edit_number(1.0 / 3.0), "0.333333333333333");
        assert_eq!(parse_input(&edit_date(46330.0)).0, Value::Number(46330.0));
        assert_eq!(parse_input(&edit_date(46330.5)).0, Value::Number(46330.5));
    }

    #[test]
    fn input_parsing() {
        assert_eq!(parse_input("12").0, Value::Number(12.0));
        assert_eq!(parse_input("1,234.5").0, Value::Number(1234.5));
        assert_eq!(parse_input("50%").0, Value::Number(0.5));
        assert_eq!(parse_input("$12.50").0, Value::Number(12.5));
        assert_eq!(parse_input("2026-11-04").0, Value::Number(46330.0));
        assert_eq!(parse_input("11/4/2026").0, Value::Number(46330.0));
        assert_eq!(parse_input("'123").0, Value::Text("123".into()));
        assert_eq!(parse_input("hello").0, Value::Text("hello".into()));
        assert_eq!(parse_input("1,2").0, Value::Text("1,2".into()));
    }
}
