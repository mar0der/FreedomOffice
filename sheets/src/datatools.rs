//! Data tools without UI: sort keys and comparison, custom lists, Text to Columns
//! parsing and subtotal grouping.

use std::cmp::Ordering;

use crate::model::Value;
use crate::numfmt;

// ---------------------------------------------------------------------------
// Sorting
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortOn {
    Values,
    CellColor,
    FontColor,
}

impl SortOn {
    pub const ALL: [SortOn; 3] = [SortOn::Values, SortOn::CellColor, SortOn::FontColor];
    pub fn label(self) -> &'static str {
        match self {
            SortOn::Values => "Cell Values",
            SortOn::CellColor => "Cell Color",
            SortOn::FontColor => "Font Color",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SortOrder {
    Asc,
    Desc,
    /// Cells with this colour first / last (`None` = No Fill / Automatic).
    ColorTop(Option<u32>),
    ColorBottom(Option<u32>),
    /// Custom list order; `true` = reversed.
    Custom(Vec<String>, bool),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SortKey {
    /// Column (top to bottom) or row (left to right) holding the key.
    pub line: u32,
    pub on: SortOn,
    pub order: SortOrder,
}

impl SortKey {
    pub fn values(line: u32, desc: bool) -> Self {
        SortKey { line, on: SortOn::Values, order: if desc { SortOrder::Desc } else { SortOrder::Asc } }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SortOptions {
    pub case_sensitive: bool,
    pub left_to_right: bool,
}

/// One key cell as the sort sees it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SortItem {
    pub value: Value,
    pub fill: Option<u32>,
    pub font: Option<u32>,
}

/// Built-in custom lists (Excel's Custom Lists dialog).
pub const BUILTIN_LISTS: [&[&str]; 4] = [
    &["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"],
    &["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"],
    &["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"],
    &["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"],
];

/// Ascending value order: numbers, text, logicals, errors; blanks always last.
pub fn value_cmp(a: &Value, b: &Value, desc: bool, case: bool) -> Ordering {
    let rank = |v: &Value| match v {
        Value::Number(_) => 0,
        Value::Text(t) if t.is_empty() => 4,
        Value::Text(_) => 1,
        Value::Bool(_) => 2,
        Value::Error(_) => 3,
        Value::Empty => 4,
    };
    let (ra, rb) = (rank(a), rank(b));
    if ra == 4 || rb == 4 {
        return ra.cmp(&rb);
    }
    let o = if ra != rb {
        ra.cmp(&rb)
    } else {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
            (Value::Text(x), Value::Text(y)) => {
                let o = x.to_lowercase().cmp(&y.to_lowercase());
                if o == Ordering::Equal && case {
                    // lower case sorts before upper case, like Excel
                    y.cmp(x)
                } else {
                    o
                }
            }
            (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
            _ => Ordering::Equal,
        }
    };
    if desc { o.reverse() } else { o }
}

fn list_pos(list: &[String], v: &Value) -> Option<usize> {
    let t = numfmt::value_to_text(v);
    list.iter().position(|x| x.eq_ignore_ascii_case(t.trim()))
}

pub fn compare(a: &SortItem, b: &SortItem, key: &SortKey, case: bool) -> Ordering {
    let color = |x: &SortItem| if key.on == SortOn::FontColor { x.font } else { x.fill };
    match &key.order {
        SortOrder::Asc | SortOrder::Desc if key.on == SortOn::Values => value_cmp(&a.value, &b.value, key.order == SortOrder::Desc, case),
        SortOrder::Asc | SortOrder::Desc => Ordering::Equal,
        SortOrder::ColorTop(c) => (color(b) == *c).cmp(&(color(a) == *c)),
        SortOrder::ColorBottom(c) => (color(a) == *c).cmp(&(color(b) == *c)),
        SortOrder::Custom(list, desc) => {
            let (pa, pb) = (list_pos(list, &a.value), list_pos(list, &b.value));
            match (pa, pb) {
                (Some(x), Some(y)) => if *desc { y.cmp(&x) } else { x.cmp(&y) },
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => value_cmp(&a.value, &b.value, *desc, case),
            }
        }
    }
}

/// Stable order of `items` (each a key-cell list in key order) by the sort keys.
pub fn sort_order(items: &[Vec<SortItem>], keys: &[SortKey], case: bool) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..items.len()).collect();
    idx.sort_by(|&x, &y| {
        for (k, key) in keys.iter().enumerate() {
            let o = compare(&items[x][k], &items[y][k], key, case);
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    });
    idx
}

// ---------------------------------------------------------------------------
// Text to Columns
// ---------------------------------------------------------------------------

/// Splits a line on any of the delimiters, honouring a text qualifier ("a, b" stays one field).
pub fn split_delimited(line: &str, delims: &[char], consecutive: bool, qualifier: Option<char>) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut chars = line.chars().peekable();
    let mut at_start = true;
    let mut quoted = false;
    let mut last_was_delim = false;
    while let Some(c) = chars.next() {
        if quoted {
            if Some(c) == qualifier {
                if chars.peek() == Some(&c) {
                    field.push(c);
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        if delims.contains(&c) {
            if !(consecutive && last_was_delim) {
                out.push(std::mem::take(&mut field));
            }
            last_was_delim = true;
            at_start = true;
            continue;
        }
        last_was_delim = false;
        if at_start && Some(c) == qualifier {
            quoted = true;
            at_start = false;
            continue;
        }
        at_start = false;
        field.push(c);
    }
    if !(consecutive && last_was_delim) || out.is_empty() {
        out.push(field);
    }
    out
}

/// Splits a line at character positions (sorted break columns).
pub fn split_fixed(line: &str, breaks: &[usize]) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut prev = 0;
    for &b in breaks.iter().chain(std::iter::once(&usize::MAX)) {
        let end = b.min(chars.len());
        let start = prev.min(chars.len());
        out.push(chars[start..end.max(start)].iter().collect());
        prev = b;
    }
    out
}

/// Suggested break columns for fixed-width data: where a column of spaces ends.
pub fn suggest_breaks(lines: &[&str]) -> Vec<usize> {
    let rows: Vec<Vec<char>> = lines.iter().map(|l| l.chars().collect()).collect();
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let blank = |p: usize| rows.iter().all(|r| r.get(p).is_none_or(|c| *c == ' '));
    (1..width).filter(|&p| blank(p - 1) && !blank(p) && (0..p - 1).any(|q| !blank(q))).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateOrder {
    MDY,
    DMY,
    YMD,
    MYD,
    DYM,
    YDM,
}

impl DateOrder {
    pub const ALL: [DateOrder; 6] = [DateOrder::MDY, DateOrder::DMY, DateOrder::YMD, DateOrder::MYD, DateOrder::DYM, DateOrder::YDM];
    pub fn label(self) -> &'static str {
        match self {
            DateOrder::MDY => "MDY",
            DateOrder::DMY => "DMY",
            DateOrder::YMD => "YMD",
            DateOrder::MYD => "MYD",
            DateOrder::DYM => "DYM",
            DateOrder::YDM => "YDM",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColFormat {
    General,
    Text,
    Date(DateOrder),
    Skip,
}

impl ColFormat {
    pub fn label(self) -> &'static str {
        match self {
            ColFormat::General => "General",
            ColFormat::Text => "Text",
            ColFormat::Date(o) => o.label(),
            ColFormat::Skip => "Skip column",
        }
    }
}

/// Parses a date written in the given day/month/year order ("31.12.24", "2024-1-5", "5 Jan 2024").
pub fn parse_ordered_date(s: &str, order: DateOrder) -> Option<f64> {
    let parts: Vec<&str> = s.trim().split(['/', '-', '.', ' ', ',']).filter(|p| !p.is_empty()).collect();
    if parts.len() != 3 {
        return None;
    }
    let month = |p: &str| -> Option<i64> {
        if let Ok(n) = p.parse::<i64>() {
            return Some(n);
        }
        let l = p.to_ascii_lowercase();
        if l.len() < 3 {
            return None;
        }
        crate::filter::MONTH_NAMES.iter().position(|m| m.to_ascii_lowercase().starts_with(&l)).map(|i| i as i64 + 1)
    };
    let num = |p: &str| p.parse::<i64>().ok();
    let (yi, mi, di) = match order {
        DateOrder::MDY => (2, 0, 1),
        DateOrder::DMY => (2, 1, 0),
        DateOrder::YMD => (0, 1, 2),
        DateOrder::MYD => (1, 0, 2),
        DateOrder::DYM => (1, 2, 0),
        DateOrder::YDM => (0, 2, 1),
    };
    let (mut y, m, d) = (num(parts[yi])?, month(parts[mi])?, num(parts[di])?);
    if parts[yi].len() <= 2 {
        y += if y < 30 { 2000 } else { 1900 };
    }
    if !(1..=12).contains(&m) || d < 1 || d > numfmt::days_in_month(y, m) || !(1900..=9999).contains(&y) {
        return None;
    }
    Some(numfmt::ymd_to_serial(y, m, d))
}

/// Converted field: `None` = column skipped; `Some((text, None))` = enter like typing;
/// `Some((value, fmt))` = a typed value with a number format.
#[derive(Clone, Debug, PartialEq)]
pub enum Converted {
    Skip,
    Typed(String),
    Value(Value, &'static str),
}

pub fn convert_field(field: &str, fmt: ColFormat) -> Converted {
    match fmt {
        ColFormat::Skip => Converted::Skip,
        ColFormat::Text => Converted::Value(if field.is_empty() { Value::Empty } else { Value::Text(field.to_string()) }, "@"),
        ColFormat::General => Converted::Typed(field.trim().to_string()),
        ColFormat::Date(o) => match parse_ordered_date(field, o) {
            Some(serial) => Converted::Value(Value::Number(serial), "m/d/yyyy"),
            None => Converted::Typed(field.trim().to_string()),
        },
    }
}

// ---------------------------------------------------------------------------
// Subtotals
// ---------------------------------------------------------------------------

/// Subtotal functions: (dialog label, SUBTOTAL function number, label suffix).
pub const SUBTOTAL_FUNCS: [(&str, u8, &str); 11] = [
    ("Sum", 9, "Total"),
    ("Count", 3, "Count"),
    ("Average", 1, "Average"),
    ("Max", 4, "Max"),
    ("Min", 5, "Min"),
    ("Product", 6, "Product"),
    ("Count Numbers", 2, "Count"),
    ("StdDev", 7, "StdDev"),
    ("StdDevp", 8, "StdDevp"),
    ("Var", 10, "Var"),
    ("Varp", 11, "Varp"),
];

/// Runs of equal (case-insensitive) consecutive keys: (first index, last index).
pub fn groups(keys: &[String]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        match out.last_mut() {
            Some((s, e)) if keys[*s].to_lowercase() == k.to_lowercase() => *e = i,
            _ => out.push((i, i)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn delimited() {
        assert_eq!(split_delimited("a,b,,c", &[','], false, Some('"')), s(&["a", "b", "", "c"]));
        assert_eq!(split_delimited("a,b,,c", &[','], true, Some('"')), s(&["a", "b", "c"]));
        assert_eq!(split_delimited("\"x, y\",z", &[','], false, Some('"')), s(&["x, y", "z"]));
        assert_eq!(split_delimited("\"say \"\"hi\"\"\";2", &[';'], false, Some('"')), s(&["say \"hi\"", "2"]));
        assert_eq!(split_delimited("'a b' c", &[' '], false, Some('\'')), s(&["a b", "c"]));
        assert_eq!(split_delimited("\"x, y\",z", &[','], false, None), s(&["\"x", " y\"", "z"]));
        assert_eq!(split_delimited("a\tb;c", &['\t', ';'], false, None), s(&["a", "b", "c"]));
        assert_eq!(split_delimited("a  b ", &[' '], true, None), s(&["a", "b"]));
        assert_eq!(split_delimited("", &[','], false, None), s(&[""]));
    }

    #[test]
    fn fixed_width() {
        assert_eq!(split_fixed("John  42 NY", &[6, 9]), s(&["John  ", "42 ", "NY"]));
        assert_eq!(split_fixed("ab", &[6, 9]), s(&["ab", "", ""]));
        assert_eq!(suggest_breaks(&["John  42 NY", "Al    7  LA"]), vec![6, 9]);
    }

    #[test]
    fn dates_and_formats() {
        assert_eq!(parse_ordered_date("31.12.2024", DateOrder::DMY), Some(numfmt::ymd_to_serial(2024, 12, 31)));
        assert_eq!(parse_ordered_date("12/31/24", DateOrder::MDY), Some(numfmt::ymd_to_serial(2024, 12, 31)));
        assert_eq!(parse_ordered_date("2024-01-05", DateOrder::YMD), Some(numfmt::ymd_to_serial(2024, 1, 5)));
        assert_eq!(parse_ordered_date("5 Jan 2024", DateOrder::DMY), Some(numfmt::ymd_to_serial(2024, 1, 5)));
        assert_eq!(parse_ordered_date("31/12/2024", DateOrder::MDY), None);
        assert_eq!(convert_field("007", ColFormat::Text), Converted::Value(Value::Text("007".into()), "@"));
        assert_eq!(convert_field(" 42 ", ColFormat::General), Converted::Typed("42".into()));
        assert_eq!(convert_field("x", ColFormat::Skip), Converted::Skip);
        assert_eq!(convert_field("1.2.2025", ColFormat::Date(DateOrder::DMY)), Converted::Value(Value::Number(numfmt::ymd_to_serial(2025, 2, 1)), "m/d/yyyy"));
    }

    #[test]
    fn sorting() {
        let it = |v: Value| SortItem { value: v, ..Default::default() };
        let items = vec![
            vec![it(Value::Text("b".into()))],
            vec![it(Value::Empty)],
            vec![it(Value::Number(3.0))],
            vec![it(Value::Text("A".into()))],
            vec![it(Value::Text("a".into()))],
        ];
        assert_eq!(sort_order(&items, &[SortKey::values(0, false)], false), vec![2, 3, 4, 0, 1]);
        assert_eq!(sort_order(&items, &[SortKey::values(0, false)], true), vec![2, 4, 3, 0, 1]);
        assert_eq!(sort_order(&items, &[SortKey::values(0, true)], false), vec![0, 3, 4, 2, 1]);

        let day = |d: &str| vec![it(Value::Text(d.into()))];
        let items = vec![day("Wed"), day("Sun"), day("zzz"), day("Mon")];
        let list: Vec<String> = BUILTIN_LISTS[0].iter().map(|x| x.to_string()).collect();
        let key = SortKey { line: 0, on: SortOn::Values, order: SortOrder::Custom(list.clone(), false) };
        assert_eq!(sort_order(&items, &[key], false), vec![1, 3, 0, 2]);
        let key = SortKey { line: 0, on: SortOn::Values, order: SortOrder::Custom(list, true) };
        assert_eq!(sort_order(&items, &[key], false), vec![0, 3, 1, 2]);

        let col = |f: Option<u32>| vec![SortItem { fill: f, ..Default::default() }];
        let items = vec![col(None), col(Some(1)), col(Some(2)), col(Some(1))];
        let key = SortKey { line: 0, on: SortOn::CellColor, order: SortOrder::ColorTop(Some(1)) };
        assert_eq!(sort_order(&items, &[key], false), vec![1, 3, 0, 2]);
        let key = SortKey { line: 0, on: SortOn::CellColor, order: SortOrder::ColorBottom(Some(1)) };
        assert_eq!(sort_order(&items, &[key], false), vec![0, 2, 1, 3]);
    }

    #[test]
    fn subtotal_groups() {
        assert_eq!(groups(&s(&["a", "A", "b", "c", "c"])), vec![(0, 1), (2, 2), (3, 4)]);
    }
}
