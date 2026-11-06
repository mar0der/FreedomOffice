//! AutoFilter criteria (what the column dropdowns filter by) and their evaluation.
//!
//! The criteria mirror the xlsx `<filterColumn>` element so files round-trip:
//! a value checklist (`<filters>`), up to two custom conditions (`<customFilters>`),
//! Top 10 (`<top10>`), dynamic filters such as Above Average or This Month
//! (`<dynamicFilter>`) and filter by colour (`<colorFilter>`).

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{Range, Sheet, Value, Workbook};
use crate::numfmt;

/// An AutoFilter range with per-column criteria (keyed by absolute column).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AutoFilter {
    pub range: Range,
    pub columns: BTreeMap<u32, ColumnFilter>,
}

impl AutoFilter {
    pub fn new(range: Range) -> Self {
        AutoFilter { range, columns: BTreeMap::new() }
    }
    pub fn is_filtered(&self, c: u32) -> bool {
        self.columns.contains_key(&c)
    }
}

/// Comparison operator of a custom condition (wildcards in the value give
/// "begins with", "contains" and friends, exactly like the file format).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CustomOp {
    Equal,
    NotEqual,
    Greater,
    GreaterEq,
    Less,
    LessEq,
}

impl CustomOp {
    pub fn xml(self) -> &'static str {
        match self {
            CustomOp::Equal => "equal",
            CustomOp::NotEqual => "notEqual",
            CustomOp::Greater => "greaterThan",
            CustomOp::GreaterEq => "greaterThanOrEqual",
            CustomOp::Less => "lessThan",
            CustomOp::LessEq => "lessThanOrEqual",
        }
    }
    pub fn from_xml(s: &str) -> Option<Self> {
        Some(match s {
            "" | "equal" => CustomOp::Equal,
            "notEqual" => CustomOp::NotEqual,
            "greaterThan" => CustomOp::Greater,
            "greaterThanOrEqual" => CustomOp::GreaterEq,
            "lessThan" => CustomOp::Less,
            "lessThanOrEqual" => CustomOp::LessEq,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Condition {
    pub op: CustomOp,
    pub val: String,
}

/// A checked node of the date tree: a whole year, a month or a single day.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct DateGroup {
    pub year: i32,
    /// 1-12, 0 = the whole year.
    pub month: u8,
    /// 1-31, 0 = the whole month.
    pub day: u8,
}

impl DateGroup {
    pub fn covers(&self, y: i64, m: i64, d: i64) -> bool {
        self.year as i64 == y && (self.month == 0 || self.month as i64 == m) && (self.day == 0 || self.day as i64 == d)
    }
}

/// Dynamic filters: averages and relative date periods.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Dynamic {
    AboveAverage,
    BelowAverage,
    Tomorrow,
    Today,
    Yesterday,
    NextWeek,
    ThisWeek,
    LastWeek,
    NextMonth,
    ThisMonth,
    LastMonth,
    NextQuarter,
    ThisQuarter,
    LastQuarter,
    NextYear,
    ThisYear,
    LastYear,
    YearToDate,
    /// 1-4, any year.
    Quarter(u8),
    /// 1-12, any year.
    Month(u8),
}

const DYN_NAMES: [(Dynamic, &str, &str); 18] = [
    (Dynamic::AboveAverage, "aboveAverage", "Above Average"),
    (Dynamic::BelowAverage, "belowAverage", "Below Average"),
    (Dynamic::Tomorrow, "tomorrow", "Tomorrow"),
    (Dynamic::Today, "today", "Today"),
    (Dynamic::Yesterday, "yesterday", "Yesterday"),
    (Dynamic::NextWeek, "nextWeek", "Next Week"),
    (Dynamic::ThisWeek, "thisWeek", "This Week"),
    (Dynamic::LastWeek, "lastWeek", "Last Week"),
    (Dynamic::NextMonth, "nextMonth", "Next Month"),
    (Dynamic::ThisMonth, "thisMonth", "This Month"),
    (Dynamic::LastMonth, "lastMonth", "Last Month"),
    (Dynamic::NextQuarter, "nextQuarter", "Next Quarter"),
    (Dynamic::ThisQuarter, "thisQuarter", "This Quarter"),
    (Dynamic::LastQuarter, "lastQuarter", "Last Quarter"),
    (Dynamic::NextYear, "nextYear", "Next Year"),
    (Dynamic::ThisYear, "thisYear", "This Year"),
    (Dynamic::LastYear, "lastYear", "Last Year"),
    (Dynamic::YearToDate, "yearToDate", "Year to Date"),
];

pub const MONTH_NAMES: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December",
];

impl Dynamic {
    pub fn xml(self) -> String {
        match self {
            Dynamic::Quarter(q) => format!("Q{q}"),
            Dynamic::Month(m) => format!("M{m}"),
            d => DYN_NAMES.iter().find(|x| x.0 == d).map(|x| x.1.to_string()).unwrap_or_default(),
        }
    }
    pub fn from_xml(s: &str) -> Option<Self> {
        if let Some(d) = DYN_NAMES.iter().find(|x| x.1 == s) {
            return Some(d.0);
        }
        let n: u8 = s.get(1..)?.parse().ok()?;
        match s.as_bytes().first()? {
            b'Q' if (1..=4).contains(&n) => Some(Dynamic::Quarter(n)),
            b'M' if (1..=12).contains(&n) => Some(Dynamic::Month(n)),
            _ => None,
        }
    }
    pub fn label(self) -> String {
        match self {
            Dynamic::Quarter(q) => format!("Quarter {q}"),
            Dynamic::Month(m) => MONTH_NAMES[(m.clamp(1, 12) - 1) as usize].to_string(),
            d => DYN_NAMES.iter().find(|x| x.0 == d).map(|x| x.2.to_string()).unwrap_or_default(),
        }
    }
    pub fn is_average(self) -> bool {
        matches!(self, Dynamic::AboveAverage | Dynamic::BelowAverage)
    }
}

/// Criteria of one filtered column.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ColumnFilter {
    /// Checklist: the displayed texts that stay visible (lower case), blanks, and checked date groups.
    Values { items: BTreeSet<String>, blanks: bool, dates: BTreeSet<DateGroup> },
    /// One or two conditions joined with And / Or.
    Custom { and: bool, c1: Condition, c2: Option<Condition> },
    /// Top / Bottom N items or percent.
    Top10 { top: bool, percent: bool, val: f64 },
    Dynamic(Dynamic),
    /// Cell fill (`font == false`) or font colour; `None` = No Fill / Automatic.
    Color { font: bool, color: Option<u32> },
}

// ---------------------------------------------------------------------------
// Operators as offered in the Custom AutoFilter dialog
// ---------------------------------------------------------------------------

/// The operators listed in the Custom AutoFilter dialog; the text ones are stored
/// as equal / notEqual with `*` wildcards around the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiOp {
    Equals,
    NotEquals,
    Greater,
    GreaterEq,
    Less,
    LessEq,
    BeginsWith,
    NotBeginsWith,
    EndsWith,
    NotEndsWith,
    Contains,
    NotContains,
}

impl UiOp {
    pub const ALL: [UiOp; 12] = [
        UiOp::Equals,
        UiOp::NotEquals,
        UiOp::Greater,
        UiOp::GreaterEq,
        UiOp::Less,
        UiOp::LessEq,
        UiOp::BeginsWith,
        UiOp::NotBeginsWith,
        UiOp::EndsWith,
        UiOp::NotEndsWith,
        UiOp::Contains,
        UiOp::NotContains,
    ];

    pub fn label(self) -> &'static str {
        match self {
            UiOp::Equals => "equals",
            UiOp::NotEquals => "does not equal",
            UiOp::Greater => "is greater than",
            UiOp::GreaterEq => "is greater than or equal to",
            UiOp::Less => "is less than",
            UiOp::LessEq => "is less than or equal to",
            UiOp::BeginsWith => "begins with",
            UiOp::NotBeginsWith => "does not begin with",
            UiOp::EndsWith => "ends with",
            UiOp::NotEndsWith => "does not end with",
            UiOp::Contains => "contains",
            UiOp::NotContains => "does not contain",
        }
    }

    /// Date columns say "is after" / "is before".
    pub fn date_label(self) -> &'static str {
        match self {
            UiOp::Greater => "is after",
            UiOp::GreaterEq => "is after or equal to",
            UiOp::Less => "is before",
            UiOp::LessEq => "is before or equal to",
            o => o.label(),
        }
    }

    pub fn condition(self, val: &str) -> Condition {
        let (op, v) = match self {
            UiOp::Equals => (CustomOp::Equal, val.to_string()),
            UiOp::NotEquals => (CustomOp::NotEqual, val.to_string()),
            UiOp::Greater => (CustomOp::Greater, val.to_string()),
            UiOp::GreaterEq => (CustomOp::GreaterEq, val.to_string()),
            UiOp::Less => (CustomOp::Less, val.to_string()),
            UiOp::LessEq => (CustomOp::LessEq, val.to_string()),
            UiOp::BeginsWith => (CustomOp::Equal, format!("{val}*")),
            UiOp::NotBeginsWith => (CustomOp::NotEqual, format!("{val}*")),
            UiOp::EndsWith => (CustomOp::Equal, format!("*{val}")),
            UiOp::NotEndsWith => (CustomOp::NotEqual, format!("*{val}")),
            UiOp::Contains => (CustomOp::Equal, format!("*{val}*")),
            UiOp::NotContains => (CustomOp::NotEqual, format!("*{val}*")),
        };
        Condition { op, val: v }
    }
}

fn unescaped_star_end(s: &str) -> bool {
    s.ends_with('*') && !s.ends_with("~*")
}

/// The dialog operator for a stored condition.
pub fn ui_op(c: &Condition) -> UiOp {
    let v = &c.val;
    let starts = v.starts_with('*') && v.len() > 1;
    let ends = unescaped_star_end(v) && v.len() > 1;
    let eq = c.op == CustomOp::Equal;
    match c.op {
        CustomOp::Equal | CustomOp::NotEqual => match (starts, ends) {
            (true, true) if v.len() > 2 => if eq { UiOp::Contains } else { UiOp::NotContains },
            (false, true) => if eq { UiOp::BeginsWith } else { UiOp::NotBeginsWith },
            (true, false) => if eq { UiOp::EndsWith } else { UiOp::NotEndsWith },
            _ => if eq { UiOp::Equals } else { UiOp::NotEquals },
        },
        CustomOp::Greater => UiOp::Greater,
        CustomOp::GreaterEq => UiOp::GreaterEq,
        CustomOp::Less => UiOp::Less,
        CustomOp::LessEq => UiOp::LessEq,
    }
}

/// The value as typed in the dialog (wildcards added by the operator removed).
pub fn ui_value(c: &Condition) -> String {
    let v = c.val.as_str();
    match ui_op(c) {
        UiOp::Contains | UiOp::NotContains => v[1..v.len() - 1].to_string(),
        UiOp::BeginsWith | UiOp::NotBeginsWith => v[..v.len() - 1].to_string(),
        UiOp::EndsWith | UiOp::NotEndsWith => v[1..].to_string(),
        _ => v.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Wildcards (* ? ~), shared with Find & Replace
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Pat {
    Star,
    One,
    Ch(char),
}

fn compile(pattern: &str, case: bool) -> Vec<Pat> {
    let mut out = Vec::new();
    let mut it = pattern.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '~' if matches!(it.peek(), Some('*' | '?' | '~')) => {
                let n = it.next().unwrap_or('~');
                out.push(Pat::Ch(if case { n } else { lower(n) }));
            }
            '*' => {
                if out.last() != Some(&Pat::Star) {
                    out.push(Pat::Star);
                }
            }
            '?' => out.push(Pat::One),
            c => out.push(Pat::Ch(if case { c } else { lower(c) })),
        }
    }
    out
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn prep(text: &str, case: bool) -> Vec<char> {
    text.chars().map(|c| if case { c } else { lower(c) }).collect()
}

/// Longest end (in chars) of a match of `p` starting at `st`.
fn longest_end(p: &[Pat], s: &[char], st: usize) -> Option<usize> {
    let n = s.len();
    let mut cur = vec![false; n + 1];
    cur[st] = true;
    for tok in p {
        let mut next = vec![false; n + 1];
        match tok {
            Pat::Star => {
                if let Some(first) = cur.iter().position(|b| *b) {
                    next[first..].iter_mut().for_each(|b| *b = true);
                }
            }
            Pat::One => (0..n).filter(|&i| cur[i]).for_each(|i| next[i + 1] = true),
            Pat::Ch(c) => (0..n).filter(|&i| cur[i] && s[i] == *c).for_each(|i| next[i + 1] = true),
        }
        cur = next;
    }
    cur.iter().rposition(|b| *b)
}

fn full_match(p: &[Pat], s: &[char]) -> bool {
    // Iterative glob match with backtracking on the last star.
    let (mut pi, mut si) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while si < s.len() {
        match p.get(pi) {
            Some(Pat::Star) => {
                star = Some(pi);
                mark = si;
                pi += 1;
            }
            Some(Pat::One) => {
                pi += 1;
                si += 1;
            }
            Some(Pat::Ch(c)) if *c == s[si] => {
                pi += 1;
                si += 1;
            }
            _ => match star {
                Some(st) => {
                    pi = st + 1;
                    mark += 1;
                    si = mark;
                }
                None => return false,
            },
        }
    }
    while p.get(pi) == Some(&Pat::Star) {
        pi += 1;
    }
    pi == p.len()
}

/// Whole-text match of a pattern with `*`, `?` and `~` escapes.
pub fn wildcard_match(pattern: &str, text: &str, case: bool) -> bool {
    full_match(&compile(pattern, case), &prep(text, case))
}

/// True if the text contains unescaped wildcards.
pub fn has_wildcards(pattern: &str) -> bool {
    compile(pattern, true).iter().any(|p| !matches!(p, Pat::Ch(_)))
}

/// First match of a pattern inside `text`: (start, end) byte offsets.
/// Leftmost start, longest match (a trailing `*` runs to the end of the text).
pub fn wildcard_find(pattern: &str, text: &str, case: bool, from: usize) -> Option<(usize, usize)> {
    let p = compile(pattern, case);
    if p.is_empty() {
        return None;
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let s: Vec<char> = chars.iter().map(|(_, c)| if case { *c } else { lower(*c) }).collect();
    let byte = |i: usize| chars.get(i).map(|(b, _)| *b).unwrap_or(text.len());
    let start_char = chars.iter().position(|(b, _)| *b >= from).unwrap_or(chars.len());
    for st in start_char..=s.len() {
        if let Some(e) = longest_end(&p, &s, st) {
            if e > st || p.iter().all(|x| *x == Pat::Star) {
                return Some((byte(st), byte(e)));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// What the criteria need to know about one cell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probe {
    pub value: Value,
    /// Displayed (formatted) text.
    pub text: String,
    /// Number shown with a date format.
    pub date: bool,
    pub fill: Option<u32>,
    pub font: Option<u32>,
}

impl Probe {
    pub fn is_blank(&self) -> bool {
        match &self.value {
            Value::Empty => true,
            Value::Text(t) => t.is_empty(),
            _ => false,
        }
    }
}

pub fn probe(wb: &Workbook, sh: &Sheet, r: u32, c: u32) -> Probe {
    let value = sh.display_value(r, c).clone();
    let style = wb.style(sh.get(r, c).map(|x| x.style).unwrap_or(0));
    let text = numfmt::format_value(&value, &style.num_fmt).text;
    let date = matches!(value, Value::Number(_)) && numfmt::is_date_format(&style.num_fmt);
    Probe { value, text, date, fill: style.fill, font: style.color }
}

/// Kind of data in a column, which decides between Text / Number / Date Filters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnKind {
    Text,
    Number,
    Date,
}

pub fn column_kind(cells: &[Probe]) -> ColumnKind {
    let (mut t, mut n, mut d) = (0, 0, 0);
    for p in cells {
        match &p.value {
            Value::Number(_) if p.date => d += 1,
            Value::Number(_) => n += 1,
            Value::Text(s) if !s.is_empty() => t += 1,
            Value::Bool(_) | Value::Error(_) => t += 1,
            _ => {}
        }
    }
    if d > 0 && d >= n && d >= t {
        ColumnKind::Date
    } else if n > 0 && n >= t {
        ColumnKind::Number
    } else {
        ColumnKind::Text
    }
}

fn day_range(dynamic: Dynamic, today: f64) -> Option<(f64, f64)> {
    let t = today.floor();
    let (y, m, _) = numfmt::serial_to_ymd(t);
    let ymd = numfmt::ymd_to_serial;
    let week = t - (numfmt::weekday(t) - 1) as f64;
    let q0 = (m - 1) / 3 * 3 + 1;
    Some(match dynamic {
        Dynamic::Tomorrow => (t + 1.0, t + 2.0),
        Dynamic::Today => (t, t + 1.0),
        Dynamic::Yesterday => (t - 1.0, t),
        Dynamic::NextWeek => (week + 7.0, week + 14.0),
        Dynamic::ThisWeek => (week, week + 7.0),
        Dynamic::LastWeek => (week - 7.0, week),
        Dynamic::NextMonth => (ymd(y, m + 1, 1), ymd(y, m + 2, 1)),
        Dynamic::ThisMonth => (ymd(y, m, 1), ymd(y, m + 1, 1)),
        Dynamic::LastMonth => (ymd(y, m - 1, 1), ymd(y, m, 1)),
        Dynamic::NextQuarter => (ymd(y, q0 + 3, 1), ymd(y, q0 + 6, 1)),
        Dynamic::ThisQuarter => (ymd(y, q0, 1), ymd(y, q0 + 3, 1)),
        Dynamic::LastQuarter => (ymd(y, q0 - 3, 1), ymd(y, q0, 1)),
        Dynamic::NextYear => (ymd(y + 1, 1, 1), ymd(y + 2, 1, 1)),
        Dynamic::ThisYear => (ymd(y, 1, 1), ymd(y + 1, 1, 1)),
        Dynamic::LastYear => (ymd(y - 1, 1, 1), ymd(y, 1, 1)),
        Dynamic::YearToDate => (ymd(y, 1, 1), t + 1.0),
        _ => return None,
    })
}

/// Parses a condition value as a number or date, like typing it into a cell.
fn cond_number(val: &str) -> Option<f64> {
    match numfmt::parse_input(val.trim()).0 {
        Value::Number(n) => Some(n),
        _ => None,
    }
}

fn cond_matches(c: &Condition, p: &Probe) -> bool {
    use std::cmp::Ordering;
    let val = c.val.as_str();
    let blank = p.is_blank();
    // "equals" with an empty value finds blanks; "does not equal" nothing finds non-blanks.
    if val.is_empty() {
        return match c.op {
            CustomOp::Equal => blank,
            CustomOp::NotEqual => !blank,
            _ => false,
        };
    }
    let ord: Option<Ordering> = match (&p.value, cond_number(val)) {
        (Value::Number(n), Some(v)) if !has_wildcards(val) => {
            // compare what is shown, so 1.23456 shown as 1.23 equals "1.23"
            if matches!(c.op, CustomOp::Equal | CustomOp::NotEqual) && p.text.eq_ignore_ascii_case(val.trim()) {
                Some(Ordering::Equal)
            } else {
                n.partial_cmp(&v)
            }
        }
        _ => None,
    };
    match c.op {
        CustomOp::Equal | CustomOp::NotEqual => {
            let eq = match ord {
                Some(o) => o == Ordering::Equal,
                None => !blank && wildcard_match(val, &p.text, false),
            };
            eq == (c.op == CustomOp::Equal)
        }
        op => {
            let o = match ord {
                Some(o) => o,
                None => {
                    // text compares with text only
                    if blank || matches!(p.value, Value::Number(_)) || cond_number(val).is_some() {
                        return false;
                    }
                    p.text.to_lowercase().cmp(&val.to_lowercase())
                }
            };
            match op {
                CustomOp::Greater => o == Ordering::Greater,
                CustomOp::GreaterEq => o != Ordering::Less,
                CustomOp::Less => o == Ordering::Less,
                _ => o != Ordering::Greater,
            }
        }
    }
}

/// Which cells of a column pass the criteria. `today` is a date serial.
pub fn evaluate(f: &ColumnFilter, cells: &[Probe], today: f64) -> Vec<bool> {
    let nums: Vec<f64> = cells.iter().filter_map(|p| p.value.as_number()).collect();
    match f {
        ColumnFilter::Values { items, blanks, dates } => cells
            .iter()
            .map(|p| {
                if p.is_blank() {
                    return *blanks;
                }
                if p.date {
                    if let Some(n) = p.value.as_number() {
                        let (y, m, d) = numfmt::serial_to_ymd(n);
                        if dates.iter().any(|g| g.covers(y, m, d)) {
                            return true;
                        }
                    }
                }
                items.contains(&p.text.to_lowercase())
            })
            .collect(),
        ColumnFilter::Custom { and, c1, c2 } => cells
            .iter()
            .map(|p| {
                let a = cond_matches(c1, p);
                match c2 {
                    None => a,
                    Some(c2) if *and => a && cond_matches(c2, p),
                    Some(c2) => a || cond_matches(c2, p),
                }
            })
            .collect(),
        ColumnFilter::Top10 { top, percent, val } => {
            let mut sorted = nums.clone();
            sorted.sort_by(|a, b| if *top { b.total_cmp(a) } else { a.total_cmp(b) });
            let count = if *percent { (sorted.len() as f64 * val / 100.0).floor() as usize } else { val.floor().max(0.0) as usize };
            let count = count.clamp(1, sorted.len().max(1));
            let threshold = sorted.get(count - 1).copied();
            cells
                .iter()
                .map(|p| match (p.value.as_number(), threshold) {
                    (Some(n), Some(t)) => if *top { n >= t } else { n <= t },
                    _ => false,
                })
                .collect()
        }
        ColumnFilter::Dynamic(d) if d.is_average() => {
            let avg = if nums.is_empty() { 0.0 } else { nums.iter().sum::<f64>() / nums.len() as f64 };
            cells
                .iter()
                .map(|p| p.value.as_number().is_some_and(|n| if *d == Dynamic::AboveAverage { n > avg } else { n < avg }))
                .collect()
        }
        ColumnFilter::Dynamic(d) => cells
            .iter()
            .map(|p| {
                let Some(n) = p.value.as_number() else { return false };
                match *d {
                    Dynamic::Quarter(q) => (numfmt::serial_to_ymd(n).1 - 1) / 3 + 1 == q as i64,
                    Dynamic::Month(m) => numfmt::serial_to_ymd(n).1 == m as i64,
                    d => day_range(d, today).is_some_and(|(a, b)| n >= a && n < b),
                }
            })
            .collect(),
        ColumnFilter::Color { font, color } => cells.iter().map(|p| if *font { p.font == *color } else { p.fill == *color }).collect(),
    }
}

/// Last row of the filter's data: the range grows to take in rows typed directly below it.
pub fn data_end(sh: &Sheet, rg: Range) -> u32 {
    let er = sh.extent_with_spills().map(|e| e.0).unwrap_or(0);
    let mut r2 = rg.r2;
    while r2 < er && (rg.c1..=rg.c2).any(|c| sh.is_populated(r2 + 1, c)) {
        r2 += 1;
    }
    r2
}

/// Evaluates every criteria column; returns the data rows and whether each is visible.
pub fn row_visibility(wb: &Workbook, sh: &Sheet, f: &AutoFilter, today: f64) -> Vec<(u32, bool)> {
    let rg = f.range;
    if rg.r2 <= rg.r1 {
        return Vec::new();
    }
    let rows: Vec<u32> = (rg.r1 + 1..=rg.r2).collect();
    let mut vis = vec![true; rows.len()];
    for (&c, cf) in &f.columns {
        if c < rg.c1 || c > rg.c2 {
            continue;
        }
        let probes: Vec<Probe> = rows.iter().map(|&r| probe(wb, sh, r, c)).collect();
        for (v, ok) in vis.iter_mut().zip(evaluate(cf, &probes, today)) {
            *v &= ok;
        }
    }
    rows.into_iter().zip(vis).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Probe {
        Probe { value: if s.is_empty() { Value::Empty } else { Value::Text(s.into()) }, text: s.into(), ..Default::default() }
    }
    fn n(x: f64) -> Probe {
        Probe { value: Value::Number(x), text: numfmt::general(x), ..Default::default() }
    }
    fn d(y: i64, m: i64, dd: i64) -> Probe {
        let s = numfmt::ymd_to_serial(y, m, dd);
        Probe { value: Value::Number(s), text: format!("{m}/{dd}/{y}"), date: true, ..Default::default() }
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match("a*c", "ABC", false));
        assert!(wildcard_match("a?c", "abc", false));
        assert!(!wildcard_match("a?c", "abbc", false));
        assert!(wildcard_match("*", "", false));
        assert!(wildcard_match("~*x", "*x", false));
        assert!(!wildcard_match("~*x", "ax", false));
        assert!(!wildcard_match("a*c", "ABC", true));
        assert!(wildcard_match("*ab*ab", "xxabyyab", false));
        assert!(has_wildcards("a*"));
        assert!(!has_wildcards("a~*"));
        assert_eq!(wildcard_find("b?d", "abcde", false, 0), Some((1, 4)));
        assert_eq!(wildcard_find("b*", "abcde", false, 0), Some((1, 5)));
        assert_eq!(wildcard_find("x", "abc", false, 0), None);
        assert_eq!(wildcard_find("a", "aXa", false, 1), Some((2, 3)));
    }

    #[test]
    fn ui_ops_round_trip() {
        for op in UiOp::ALL {
            let c = op.condition("abc");
            assert_eq!(ui_op(&c), op, "{op:?}");
            assert_eq!(ui_value(&c), "abc");
        }
    }

    #[test]
    fn values_and_blanks() {
        let cells = vec![t("East"), t("West"), t(""), t("north")];
        let f = ColumnFilter::Values { items: ["east".to_string(), "north".to_string()].into(), blanks: false, dates: Default::default() };
        assert_eq!(evaluate(&f, &cells, 0.0), vec![true, false, false, true]);
        let f = ColumnFilter::Values { items: Default::default(), blanks: true, dates: Default::default() };
        assert_eq!(evaluate(&f, &cells, 0.0), vec![false, false, true, false]);
    }

    #[test]
    fn date_groups() {
        let cells = vec![d(2024, 1, 5), d(2024, 2, 9), d(2025, 2, 9)];
        let g = |year, month, day| DateGroup { year, month, day };
        let f = ColumnFilter::Values { items: Default::default(), blanks: false, dates: [g(2024, 2, 0), g(2025, 0, 0)].into() };
        assert_eq!(evaluate(&f, &cells, 0.0), vec![false, true, true]);
        let f = ColumnFilter::Values { items: Default::default(), blanks: false, dates: [g(2024, 1, 5)].into() };
        assert_eq!(evaluate(&f, &cells, 0.0), vec![true, false, false]);
    }

    #[test]
    fn custom_text_and_numbers() {
        let cells = vec![t("Apple"), t("Banana"), t("apricot"), n(5.0), n(12.0), t("")];
        let ev = |c1: Condition, c2: Option<Condition>, and: bool| evaluate(&ColumnFilter::Custom { and, c1, c2 }, &cells, 0.0);
        assert_eq!(ev(UiOp::BeginsWith.condition("ap"), None, true), vec![true, false, true, false, false, false]);
        assert_eq!(ev(UiOp::Contains.condition("an"), None, true), vec![false, true, false, false, false, false]);
        assert_eq!(ev(UiOp::NotContains.condition("p"), None, true), vec![false, true, false, true, true, true]);
        assert_eq!(ev(UiOp::EndsWith.condition("A"), None, true), vec![false, true, false, false, false, false]);
        assert_eq!(ev(UiOp::Greater.condition("6"), None, true), vec![false, false, false, false, true, false]);
        // Between 4 and 10
        assert_eq!(
            ev(UiOp::GreaterEq.condition("4"), Some(UiOp::LessEq.condition("10")), true),
            vec![false, false, false, true, false, false]
        );
        // Or
        assert_eq!(
            ev(UiOp::Equals.condition("apple"), Some(UiOp::Equals.condition("12")), false),
            vec![true, false, false, false, true, false]
        );
        assert_eq!(ev(UiOp::Equals.condition("a?ple"), None, true), vec![true, false, false, false, false, false]);
        assert_eq!(ev(UiOp::Greater.condition("b"), None, true), vec![false, true, false, false, false, false]);
        assert_eq!(ev(UiOp::Equals.condition(""), None, true), vec![false, false, false, false, false, true]);
        assert_eq!(ev(UiOp::NotEquals.condition(""), None, true), vec![true, true, true, true, true, false]);
    }

    #[test]
    fn top10_and_average() {
        let cells: Vec<Probe> = [1.0, 7.0, 3.0, 9.0, 5.0].iter().map(|x| n(*x)).chain([t("x")]).collect();
        let f = ColumnFilter::Top10 { top: true, percent: false, val: 2.0 };
        assert_eq!(evaluate(&f, &cells, 0.0), vec![false, true, false, true, false, false]);
        let f = ColumnFilter::Top10 { top: false, percent: true, val: 40.0 };
        assert_eq!(evaluate(&f, &cells, 0.0), vec![true, false, true, false, false, false]);
        let f = ColumnFilter::Dynamic(Dynamic::AboveAverage);
        assert_eq!(evaluate(&f, &cells, 0.0), vec![false, true, false, true, false, false]);
        let f = ColumnFilter::Dynamic(Dynamic::BelowAverage);
        assert_eq!(evaluate(&f, &cells, 0.0), vec![true, false, true, false, false, false]);
    }

    #[test]
    fn date_periods() {
        // Wednesday 2026-11-04
        let today = numfmt::ymd_to_serial(2026, 11, 4);
        let cells = vec![d(2026, 11, 4), d(2026, 11, 3), d(2026, 11, 1), d(2026, 10, 31), d(2026, 1, 1), d(2025, 12, 31), d(2026, 12, 1)];
        let ev = |k| evaluate(&ColumnFilter::Dynamic(k), &cells, today);
        assert_eq!(ev(Dynamic::Today), vec![true, false, false, false, false, false, false]);
        assert_eq!(ev(Dynamic::Yesterday), vec![false, true, false, false, false, false, false]);
        assert_eq!(ev(Dynamic::ThisWeek), vec![true, true, true, false, false, false, false]);
        assert_eq!(ev(Dynamic::LastWeek), vec![false, false, false, true, false, false, false]);
        assert_eq!(ev(Dynamic::ThisMonth), vec![true, true, true, false, false, false, false]);
        assert_eq!(ev(Dynamic::LastMonth), vec![false, false, false, true, false, false, false]);
        assert_eq!(ev(Dynamic::NextMonth), vec![false, false, false, false, false, false, true]);
        assert_eq!(ev(Dynamic::ThisQuarter), vec![true, true, true, true, false, false, true]);
        assert_eq!(ev(Dynamic::LastYear), vec![false, false, false, false, false, true, false]);
        assert_eq!(ev(Dynamic::YearToDate), vec![true, true, true, true, true, false, false]);
        assert_eq!(ev(Dynamic::Quarter(1)), vec![false, false, false, false, true, false, false]);
        assert_eq!(ev(Dynamic::Month(12)), vec![false, false, false, false, false, true, true]);
        for k in [Dynamic::ThisWeek, Dynamic::YearToDate, Dynamic::Quarter(3), Dynamic::Month(11)] {
            assert_eq!(Dynamic::from_xml(&k.xml()), Some(k));
        }
    }

    #[test]
    fn colors() {
        let mut a = t("a");
        a.fill = Some(0xFFFF00);
        let mut b = t("b");
        b.font = Some(0xFF0000);
        let cells = vec![a, b, t("c")];
        assert_eq!(evaluate(&ColumnFilter::Color { font: false, color: Some(0xFFFF00) }, &cells, 0.0), vec![true, false, false]);
        assert_eq!(evaluate(&ColumnFilter::Color { font: false, color: None }, &cells, 0.0), vec![false, true, true]);
        assert_eq!(evaluate(&ColumnFilter::Color { font: true, color: Some(0xFF0000) }, &cells, 0.0), vec![false, true, false]);
    }

    #[test]
    fn kinds() {
        assert_eq!(column_kind(&[t("a"), n(1.0), n(2.0)]), ColumnKind::Number);
        assert_eq!(column_kind(&[t("a"), t("b"), n(2.0)]), ColumnKind::Text);
        assert_eq!(column_kind(&[d(2024, 1, 1), t("b")]), ColumnKind::Date);
    }
}
