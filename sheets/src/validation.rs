//! Data validation: allowed values, list sources, input messages and error alerts.
//!
//! Operand formulas are stored as text without '=' and are relative to the top-left cell
//! of the validation's first range. A constant list is stored as a quoted string
//! (`"a,b,c"`), a list from cells as a reference formula (`$A$1:$A$5`).

use crate::condfmt::{eval_at, truthy, CmpOp};
use crate::formula::{self, Expr};
use crate::model::{Range, Sheet, Value, Workbook};
use crate::numfmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Allow {
    #[default]
    Any,
    Whole,
    Decimal,
    List,
    Date,
    Time,
    TextLength,
    Custom,
}

impl Allow {
    pub const ALL: [Allow; 8] =
        [Allow::Any, Allow::Whole, Allow::Decimal, Allow::List, Allow::Date, Allow::Time, Allow::TextLength, Allow::Custom];

    pub fn label(self) -> &'static str {
        match self {
            Allow::Any => "Any value",
            Allow::Whole => "Whole number",
            Allow::Decimal => "Decimal",
            Allow::List => "List",
            Allow::Date => "Date",
            Allow::Time => "Time",
            Allow::TextLength => "Text length",
            Allow::Custom => "Custom",
        }
    }

    pub fn xml(self) -> &'static str {
        match self {
            Allow::Any => "none",
            Allow::Whole => "whole",
            Allow::Decimal => "decimal",
            Allow::List => "list",
            Allow::Date => "date",
            Allow::Time => "time",
            Allow::TextLength => "textLength",
            Allow::Custom => "custom",
        }
    }

    pub fn from_xml(s: &str) -> Allow {
        Allow::ALL.into_iter().find(|a| a.xml() == s).unwrap_or(Allow::Any)
    }

    /// Types that compare against minimum/maximum operands.
    pub fn uses_op(self) -> bool {
        matches!(self, Allow::Whole | Allow::Decimal | Allow::Date | Allow::Time | Allow::TextLength)
    }

    /// Number format used to show constant operands of this type.
    pub fn operand_format(self) -> &'static str {
        match self {
            Allow::Date => "m/d/yyyy",
            Allow::Time => "h:mm:ss",
            _ => "",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ErrorStyle {
    #[default]
    Stop,
    Warning,
    Information,
}

impl ErrorStyle {
    pub const ALL: [ErrorStyle; 3] = [ErrorStyle::Stop, ErrorStyle::Warning, ErrorStyle::Information];

    pub fn label(self) -> &'static str {
        match self {
            ErrorStyle::Stop => "Stop",
            ErrorStyle::Warning => "Warning",
            ErrorStyle::Information => "Information",
        }
    }

    pub fn xml(self) -> &'static str {
        match self {
            ErrorStyle::Stop => "stop",
            ErrorStyle::Warning => "warning",
            ErrorStyle::Information => "information",
        }
    }

    pub fn from_xml(s: &str) -> ErrorStyle {
        ErrorStyle::ALL.into_iter().find(|e| e.xml() == s).unwrap_or_default()
    }
}

pub const DEFAULT_ERROR: &str = "This value doesn't match the data validation restrictions defined for this cell.";

#[derive(Clone, Debug, PartialEq)]
pub struct Validation {
    pub ranges: Vec<Range>,
    pub allow: Allow,
    pub op: CmpOp,
    pub f1: String,
    pub f2: String,
    pub ignore_blank: bool,
    /// Show the in-cell dropdown for lists.
    pub dropdown: bool,
    pub show_input: bool,
    pub input_title: String,
    pub input_msg: String,
    pub show_error: bool,
    pub error_style: ErrorStyle,
    pub error_title: String,
    pub error_msg: String,
}

impl Default for Validation {
    fn default() -> Self {
        Validation {
            ranges: Vec::new(),
            allow: Allow::Any,
            op: CmpOp::Between,
            f1: String::new(),
            f2: String::new(),
            ignore_blank: true,
            dropdown: true,
            show_input: true,
            input_title: String::new(),
            input_msg: String::new(),
            show_error: true,
            error_style: ErrorStyle::Stop,
            error_title: String::new(),
            error_msg: String::new(),
        }
    }
}

fn parse_opt(s: &str) -> Option<Expr> {
    let s = s.trim().trim_start_matches('=');
    if s.is_empty() { None } else { formula::parse(s).ok() }
}

impl Validation {
    pub fn applies(&self, r: u32, c: u32) -> bool {
        self.ranges.iter().any(|rg| rg.contains(r, c))
    }

    pub fn base(&self) -> (u32, u32) {
        self.ranges.first().map(|r| (r.r1, r.c1)).unwrap_or((0, 0))
    }

    /// True when the validation does nothing worth keeping.
    pub fn is_noop(&self) -> bool {
        self.allow == Allow::Any && self.input_title.is_empty() && self.input_msg.is_empty()
    }

    pub fn has_input_message(&self) -> bool {
        self.show_input && !(self.input_title.trim().is_empty() && self.input_msg.trim().is_empty())
    }

    pub fn shows_dropdown(&self) -> bool {
        self.allow == Allow::List && self.dropdown
    }

    /// Items of a list validation as (display text, value), evaluated for cell (r, c).
    pub fn list_items(&self, wb: &Workbook, si: usize, r: u32, c: u32) -> Vec<(String, Value)> {
        let Some(e) = parse_opt(&self.f1) else { return Vec::new() };
        if let Expr::Str(s) = &e {
            return split_list(s).into_iter().map(|t| (t.clone(), numfmt::parse_input(&t).0)).collect();
        }
        // A plain reference keeps each source cell's number format in the display text.
        let mut e = e;
        let base = self.base();
        formula::shift_relative(&mut e, r as i64 - base.0 as i64, c as i64 - base.1 as i64);
        if let Expr::Ref(rf) = &e {
            let s = match &rf.sheet {
                Some(n) => wb.sheet_index(n),
                None => Some(si),
            };
            if let Some(sh) = s.and_then(|s| wb.sheets.get(s)) {
                let rg = Range::new(rf.a.row, rf.a.col, rf.b.row, rf.b.col);
                let (er, ec) = sh.extent().unwrap_or((0, 0));
                let rg = Range { r1: rg.r1, c1: rg.c1, r2: rg.r2.min(er.max(rg.r1)), c2: rg.c2.min(ec.max(rg.c1)) };
                return sh
                    .iter_range(rg)
                    .filter(|(_, _, cell)| !cell.value.is_empty())
                    .map(|(_, _, cell)| (numfmt::format_value(&cell.value, &wb.style(cell.style).num_fmt).text, cell.value.clone()))
                    .collect();
            }
            return Vec::new();
        }
        formula::eval_grid(wb, si, r, c, &e)
            .into_iter()
            .flatten()
            .filter(|v| !v.is_empty())
            .map(|v| (numfmt::value_to_text(&v), v))
            .collect()
    }

    /// Evaluates an operand for cell (r, c) as a number.
    fn operand(&self, wb: &Workbook, si: usize, f: &str, r: u32, c: u32) -> Option<f64> {
        let e = parse_opt(f)?;
        match eval_at(wb, si, self.base(), &e, r, c) {
            Value::Number(n) => Some(n),
            Value::Text(s) => match numfmt::parse_input(&s).0 {
                Value::Number(n) => Some(n),
                _ => None,
            },
            Value::Bool(b) => Some(if b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    fn compare(&self, wb: &Workbook, si: usize, r: u32, c: u32, x: f64) -> bool {
        let Some(a) = self.operand(wb, si, &self.f1, r, c) else { return false };
        let b = if self.op.two() {
            match self.operand(wb, si, &self.f2, r, c) {
                Some(b) => b,
                None => return false,
            }
        } else {
            0.0
        };
        self.op.test(&Value::Number(x), &Value::Number(a), &Value::Number(b))
    }

    /// True when `v` is acceptable in cell (r, c). For Custom rules the new value must
    /// already be in the workbook, since the formula reads it from the cell.
    pub fn check(&self, wb: &Workbook, si: usize, r: u32, c: u32, v: &Value) -> bool {
        let blank = match v {
            Value::Empty => true,
            Value::Text(s) => s.is_empty(),
            _ => false,
        };
        if self.allow == Allow::Any {
            return true;
        }
        if blank {
            return self.ignore_blank;
        }
        match self.allow {
            Allow::Any => true,
            Allow::Whole | Allow::Decimal | Allow::Date | Allow::Time => {
                let Value::Number(n) = v else { return false };
                if self.allow == Allow::Whole && n.fract() != 0.0 {
                    return false;
                }
                if matches!(self.allow, Allow::Date | Allow::Time) && *n < 0.0 {
                    return false;
                }
                self.compare(wb, si, r, c, *n)
            }
            Allow::TextLength => {
                let len = numfmt::value_to_text(v).chars().count();
                self.compare(wb, si, r, c, len as f64)
            }
            Allow::List => {
                let text = numfmt::value_to_text(v);
                self.list_items(wb, si, r, c).iter().any(|(shown, item)| match (v, item) {
                    (Value::Number(a), Value::Number(b)) => (a - b).abs() < 1e-9,
                    _ => shown.eq_ignore_ascii_case(&text) || numfmt::value_to_text(item).eq_ignore_ascii_case(&text),
                })
            }
            Allow::Custom => match parse_opt(&self.f1) {
                Some(e) => truthy(&eval_at(wb, si, self.base(), &e, r, c)),
                None => true,
            },
        }
    }

    /// Message, title for the error alert.
    pub fn alert_text(&self) -> (String, String) {
        let title = if self.error_title.trim().is_empty() { "FreedomOffice Sheets".to_string() } else { self.error_title.clone() };
        let msg = if self.error_msg.trim().is_empty() { DEFAULT_ERROR.to_string() } else { self.error_msg.clone() };
        (title, msg)
    }
}

/// Splits a constant list ("a, b,c") into trimmed items.
pub fn split_list(s: &str) -> Vec<String> {
    s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

/// List source typed in the dialog ("a,b,c" or "=$A$1:$A$5") -> stored formula text.
pub fn list_from_input(s: &str) -> String {
    let t = s.trim();
    match t.strip_prefix('=') {
        Some(f) => f.trim().to_string(),
        None if t.is_empty() => String::new(),
        None => formula::print(&Expr::Str(t.to_string())),
    }
}

/// Stored list formula -> text for the Source box.
pub fn list_to_input(f: &str) -> String {
    match parse_opt(f) {
        Some(Expr::Str(s)) => s,
        Some(_) => format!("={}", f.trim_start_matches('=')),
        None => String::new(),
    }
}

/// The validation that applies to a cell.
pub fn at(sheet: &Sheet, r: u32, c: u32) -> Option<&Validation> {
    sheet.validations.iter().rev().find(|v| v.applies(r, c))
}

/// Replaces the validation of `rg` with `v` (None clears it).
pub fn set_for_range(sheet: &mut Sheet, rg: Range, v: Option<Validation>) {
    for dv in sheet.validations.iter_mut() {
        dv.ranges = crate::condfmt::subtract_all(&dv.ranges, rg);
    }
    sheet.validations.retain(|dv| !dv.ranges.is_empty());
    if let Some(mut v) = v {
        if !v.is_noop() {
            v.ranges = vec![rg];
            sheet.validations.push(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, Sheet, Workbook};

    fn wb() -> Workbook {
        let mut sh = Sheet::new("Sheet1");
        for (i, t) in ["Red", "Green", "Blue"].iter().enumerate() {
            sh.set(i as u32, 4, Some(Cell { value: Value::Text(t.to_string()), formula: None, style: 0 }));
        }
        sh.set(0, 5, Some(Cell { value: Value::Number(10.0), formula: None, style: 0 }));
        Workbook::empty_with(vec![sh, Sheet::new("Lists")])
    }

    fn dv(allow: Allow, op: CmpOp, f1: &str, f2: &str) -> Validation {
        Validation { ranges: vec![Range::parse("A1:A10").unwrap()], allow, op, f1: f1.into(), f2: f2.into(), ..Default::default() }
    }

    fn ok(v: &Validation, val: Value) -> bool {
        v.check(&wb(), 0, 0, 0, &val)
    }

    fn input(s: &str) -> Value {
        numfmt::parse_input(s).0
    }

    #[test]
    fn whole_and_decimal() {
        let v = dv(Allow::Whole, CmpOp::Between, "1", "10");
        assert!(ok(&v, Value::Number(5.0)));
        assert!(!ok(&v, Value::Number(5.5)));
        assert!(!ok(&v, Value::Number(11.0)));
        assert!(!ok(&v, Value::Text("abc".into())));
        assert!(ok(&v, Value::Empty), "blank allowed with ignore blank");
        assert!(!ok(&Validation { ignore_blank: false, ..v.clone() }, Value::Empty));
        let v = dv(Allow::Decimal, CmpOp::Greater, "$F$1", "");
        assert!(ok(&v, Value::Number(10.5)));
        assert!(!ok(&v, Value::Number(10.0)));
        let v = dv(Allow::Decimal, CmpOp::NotBetween, "0", "1");
        assert!(ok(&v, Value::Number(-0.5)));
        assert!(!ok(&v, Value::Number(0.5)));
    }

    #[test]
    fn date_time_and_length() {
        let v = dv(Allow::Date, CmpOp::GreaterEq, &crate::condfmt::operand_from_input("1/1/2024"), "");
        assert!(ok(&v, input("3/15/2024")));
        assert!(!ok(&v, input("12/31/2023")));
        assert!(!ok(&v, Value::Text("soon".into())));
        assert_eq!(crate::condfmt::operand_to_input(&v.f1, Allow::Date.operand_format()), "1/1/2024");
        let v = dv(Allow::Time, CmpOp::Less, &crate::condfmt::operand_from_input("12:00"), "");
        assert!(ok(&v, input("9:30")));
        assert!(!ok(&v, input("13:00")));
        let v = dv(Allow::TextLength, CmpOp::LessEq, "3", "");
        assert!(ok(&v, Value::Text("abc".into())));
        assert!(!ok(&v, Value::Text("abcd".into())));
        assert!(ok(&v, Value::Number(123.0)));
    }

    #[test]
    fn lists_from_constants_and_ranges() {
        assert_eq!(list_from_input("Yes, No ,Maybe"), "\"Yes, No ,Maybe\"");
        assert_eq!(list_to_input("\"Yes, No ,Maybe\""), "Yes, No ,Maybe");
        assert_eq!(list_from_input("=$E$1:$E$3"), "$E$1:$E$3");
        assert_eq!(list_to_input("$E$1:$E$3"), "=$E$1:$E$3");

        let book = wb();
        let v = dv(Allow::List, CmpOp::Between, &list_from_input("Yes, No ,Maybe"), "");
        let items: Vec<String> = v.list_items(&book, 0, 0, 0).into_iter().map(|i| i.0).collect();
        assert_eq!(items, vec!["Yes", "No", "Maybe"]);
        assert!(ok(&v, Value::Text("no".into())));
        assert!(!ok(&v, Value::Text("Never".into())));

        let v = dv(Allow::List, CmpOp::Between, "$E$1:$E$5", "");
        let items: Vec<String> = v.list_items(&book, 0, 3, 0).into_iter().map(|i| i.0).collect();
        assert_eq!(items, vec!["Red", "Green", "Blue"]);
        assert!(ok(&v, Value::Text("Blue".into())));
        assert!(!ok(&v, Value::Text("Pink".into())));

        let nums = dv(Allow::List, CmpOp::Between, &list_from_input("1,2,3"), "");
        assert!(ok(&nums, Value::Number(2.0)));
        assert!(!ok(&nums, Value::Number(4.0)));

        // Lists on another sheet.
        let mut book = wb();
        book.sheets[1].set(0, 0, Some(Cell { value: Value::Text("Alpha".into()), formula: None, style: 0 }));
        let v = dv(Allow::List, CmpOp::Between, "Lists!$A$1:$A$9", "");
        assert_eq!(v.list_items(&book, 0, 0, 0).len(), 1);
    }

    #[test]
    fn custom_formula() {
        let mut book = wb();
        // =A1>F1 must hold (F1 = 10); the value is read from the cell itself.
        let v = dv(Allow::Custom, CmpOp::Between, "A1>$F$1", "");
        book.sheets[0].set(0, 0, Some(Cell { value: Value::Number(20.0), formula: None, style: 0 }));
        assert!(v.check(&book, 0, 0, 0, &Value::Number(20.0)));
        book.sheets[0].set(1, 0, Some(Cell { value: Value::Number(5.0), formula: None, style: 0 }));
        assert!(!v.check(&book, 0, 1, 0, &Value::Number(5.0)), "relative reference moves to A2");
        assert!(dv(Allow::Any, CmpOp::Between, "", "").check(&book, 0, 0, 0, &Value::Text("x".into())));
    }

    #[test]
    fn replace_and_messages() {
        let mut sh = Sheet::new("S");
        set_for_range(&mut sh, Range::parse("A1:A10").unwrap(), Some(dv(Allow::Whole, CmpOp::Greater, "0", "")));
        set_for_range(&mut sh, Range::parse("A5").unwrap(), Some(dv(Allow::Decimal, CmpOp::Greater, "0", "")));
        assert_eq!(at(&sh, 4, 0).unwrap().allow, Allow::Decimal);
        assert_eq!(at(&sh, 3, 0).unwrap().allow, Allow::Whole);
        assert_eq!(at(&sh, 9, 0).unwrap().allow, Allow::Whole);
        set_for_range(&mut sh, Range::parse("A1:A10").unwrap(), None);
        assert!(sh.validations.is_empty());
        let v = Validation::default();
        assert_eq!(v.alert_text().1, DEFAULT_ERROR);
    }
}
