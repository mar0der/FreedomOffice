//! Writes conditional formats and data validations with rust_xlsxwriter.

use rust_xlsxwriter::{
    Color, ConditionalFormat2ColorScale, ConditionalFormat3ColorScale, ConditionalFormatAverage,
    ConditionalFormatAverageRule, ConditionalFormatBlank, ConditionalFormatCell, ConditionalFormatCellRule,
    ConditionalFormatDataBar, ConditionalFormatDuplicate, ConditionalFormatError, ConditionalFormatFormula,
    ConditionalFormatText, ConditionalFormatTextRule, ConditionalFormatTop, ConditionalFormatTopRule, DataValidation,
    DataValidationErrorStyle, DataValidationRule, Format, FormatBorder, FormatUnderline,
    Formula as XFormula, Worksheet,
};

use crate::condfmt::{sqref, CmpOp, CondFormat, DiffFormat, RuleKind, TextOp};
use crate::model::{Range, Sheet, MAX_COLS, MAX_ROWS};
use crate::validation::{Allow, ErrorStyle, Validation};

fn valid(r: &Range) -> bool {
    r.r2 < MAX_ROWS && r.c2 < MAX_COLS
}

fn dxf(f: &DiffFormat) -> Format {
    let mut x = Format::new();
    if let Some(c) = f.fill {
        x = x.set_background_color(Color::RGB(c & 0xFFFFFF));
    }
    if let Some(c) = f.color {
        x = x.set_font_color(Color::RGB(c & 0xFFFFFF));
    }
    if f.bold {
        x = x.set_bold();
    }
    if f.italic {
        x = x.set_italic();
    }
    if f.underline {
        x = x.set_underline(FormatUnderline::Single);
    }
    if f.strike {
        x = x.set_font_strikethrough();
    }
    if let Some(c) = f.border {
        x = x.set_border(FormatBorder::Thin).set_border_color(Color::RGB(c & 0xFFFFFF));
    }
    x
}

fn cell_rule(op: CmpOp, f1: &str, f2: &str) -> ConditionalFormatCellRule<XFormula> {
    let a = XFormula::new(if f1.trim().is_empty() { "0" } else { f1 });
    let b = XFormula::new(if f2.trim().is_empty() { "0" } else { f2 });
    match op {
        CmpOp::Between => ConditionalFormatCellRule::Between(a, b),
        CmpOp::NotBetween => ConditionalFormatCellRule::NotBetween(a, b),
        CmpOp::Equal => ConditionalFormatCellRule::EqualTo(a),
        CmpOp::NotEqual => ConditionalFormatCellRule::NotEqualTo(a),
        CmpOp::Greater => ConditionalFormatCellRule::GreaterThan(a),
        CmpOp::Less => ConditionalFormatCellRule::LessThan(a),
        CmpOp::GreaterEq => ConditionalFormatCellRule::GreaterThanOrEqualTo(a),
        CmpOp::LessEq => ConditionalFormatCellRule::LessThanOrEqualTo(a),
    }
}

/// Adds one conditional format over the rule's ranges; unsupported or invalid rules are skipped.
macro_rules! add_cf {
    ($ws:expr, $rule:expr, $first:expr, $cf:expr) => {{
        let mut cf = $cf.set_stop_if_true($rule.stop);
        if $rule.ranges.len() > 1 {
            cf = cf.set_multi_range(sqref(&$rule.ranges));
        }
        let _ = $ws.add_conditional_format($first.r1, $first.c1 as u16, $first.r2, $first.c2 as u16, &cf);
    }};
}

fn write_cf(ws: &mut Worksheet, rule: &CondFormat) {
    if rule.ranges.is_empty() || !rule.ranges.iter().all(valid) {
        return;
    }
    let first = rule.ranges[0];
    let fmt = dxf(&rule.format);
    match &rule.kind {
        RuleKind::CellValue { op, f1, f2 } => {
            add_cf!(ws, rule, first, ConditionalFormatCell::new().set_rule(cell_rule(*op, f1, f2)).set_format(fmt))
        }
        RuleKind::Text { op, text } => {
            let r = match op {
                TextOp::Contains => ConditionalFormatTextRule::Contains(text.clone()),
                TextOp::NotContains => ConditionalFormatTextRule::DoesNotContain(text.clone()),
                TextOp::BeginsWith => ConditionalFormatTextRule::BeginsWith(text.clone()),
                TextOp::EndsWith => ConditionalFormatTextRule::EndsWith(text.clone()),
            };
            add_cf!(ws, rule, first, ConditionalFormatText::new().set_rule(r).set_format(fmt))
        }
        RuleKind::Duplicate { unique } => {
            let mut cf = ConditionalFormatDuplicate::new().set_format(fmt);
            if *unique {
                cf = cf.invert();
            }
            add_cf!(ws, rule, first, cf)
        }
        RuleKind::Top { bottom, percent, rank } => {
            let n = (*rank).clamp(1, 1000) as u16;
            let r = match (bottom, percent) {
                (false, false) => ConditionalFormatTopRule::Top(n),
                (true, false) => ConditionalFormatTopRule::Bottom(n),
                (false, true) => ConditionalFormatTopRule::TopPercent(n.min(100)),
                (true, true) => ConditionalFormatTopRule::BottomPercent(n.min(100)),
            };
            add_cf!(ws, rule, first, ConditionalFormatTop::new().set_rule(r).set_format(fmt))
        }
        RuleKind::Average { below, equal } => {
            let r = match (below, equal) {
                (false, false) => ConditionalFormatAverageRule::AboveAverage,
                (true, false) => ConditionalFormatAverageRule::BelowAverage,
                (false, true) => ConditionalFormatAverageRule::EqualOrAboveAverage,
                (true, true) => ConditionalFormatAverageRule::EqualOrBelowAverage,
            };
            add_cf!(ws, rule, first, ConditionalFormatAverage::new().set_rule(r).set_format(fmt))
        }
        RuleKind::Blanks { not } => {
            let mut cf = ConditionalFormatBlank::new().set_format(fmt);
            if *not {
                cf = cf.invert();
            }
            add_cf!(ws, rule, first, cf)
        }
        RuleKind::Errors { not } => {
            let mut cf = ConditionalFormatError::new().set_format(fmt);
            if *not {
                cf = cf.invert();
            }
            add_cf!(ws, rule, first, cf)
        }
        RuleKind::Expression { formula } => {
            add_cf!(ws, rule, first, ConditionalFormatFormula::new().set_rule(XFormula::new(formula)).set_format(fmt))
        }
        RuleKind::DataBar { color, gradient } => {
            let cf = ConditionalFormatDataBar::new().set_fill_color(Color::RGB(color & 0xFFFFFF)).set_solid_fill(!gradient);
            add_cf!(ws, rule, first, cf)
        }
        RuleKind::ColorScale { colors } => match colors.as_slice() {
            [a, b] => {
                let cf = ConditionalFormat2ColorScale::new()
                    .set_minimum_color(Color::RGB(a & 0xFFFFFF))
                    .set_maximum_color(Color::RGB(b & 0xFFFFFF));
                add_cf!(ws, rule, first, cf)
            }
            [a, m, b, ..] => {
                let cf = ConditionalFormat3ColorScale::new()
                    .set_minimum_color(Color::RGB(a & 0xFFFFFF))
                    .set_midpoint_color(Color::RGB(m & 0xFFFFFF))
                    .set_maximum_color(Color::RGB(b & 0xFFFFFF));
                add_cf!(ws, rule, first, cf)
            }
            _ => {}
        },
    }
}

/// Truncates to Excel's UTF-16 length limit for validation titles and messages.
fn limit(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut n = 0;
    for ch in s.chars() {
        n += ch.len_utf16();
        if n > max {
            break;
        }
        out.push(ch);
    }
    out
}

fn dv_rule(v: &Validation) -> DataValidationRule<XFormula> {
    let f = |s: &str| XFormula::new(if s.trim().is_empty() { "0" } else { s });
    let (a, b) = (f(&v.f1), f(&v.f2));
    match v.op {
        CmpOp::Between => DataValidationRule::Between(a, b),
        CmpOp::NotBetween => DataValidationRule::NotBetween(a, b),
        CmpOp::Equal => DataValidationRule::EqualTo(a),
        CmpOp::NotEqual => DataValidationRule::NotEqualTo(a),
        CmpOp::Greater => DataValidationRule::GreaterThan(a),
        CmpOp::Less => DataValidationRule::LessThan(a),
        CmpOp::GreaterEq => DataValidationRule::GreaterThanOrEqualTo(a),
        CmpOp::LessEq => DataValidationRule::LessThanOrEqualTo(a),
    }
}

fn build_dv(v: &Validation) -> Option<DataValidation> {
    let d = DataValidation::new();
    let mut d = match v.allow {
        Allow::Any => d.allow_any_value(),
        Allow::Whole => d.allow_whole_number_formula(dv_rule(v)),
        Allow::Decimal => d.allow_decimal_number_formula(dv_rule(v)),
        Allow::Date => d.allow_date_formula(dv_rule(v)),
        Allow::Time => d.allow_time_formula(dv_rule(v)),
        Allow::TextLength => d.allow_text_length_formula(dv_rule(v)),
        Allow::Custom => d.allow_custom(XFormula::new(&v.f1)),
        Allow::List => match crate::formula::parse(&v.f1) {
            Ok(crate::formula::Expr::Str(s)) => d.allow_list_strings(&crate::validation::split_list(&s)).ok()?,
            _ => d.allow_list_formula(XFormula::new(&v.f1)),
        },
    };
    d = d.ignore_blank(v.ignore_blank).show_dropdown(v.dropdown).show_input_message(v.show_input).show_error_message(v.show_error);
    if !v.input_title.is_empty() {
        d = d.set_input_title(limit(&v.input_title, 32)).ok()?;
    }
    if !v.input_msg.is_empty() {
        d = d.set_input_message(limit(&v.input_msg, 255)).ok()?;
    }
    if !v.error_title.is_empty() {
        d = d.set_error_title(limit(&v.error_title, 32)).ok()?;
    }
    if !v.error_msg.is_empty() {
        d = d.set_error_message(limit(&v.error_msg, 255)).ok()?;
    }
    d = d.set_error_style(match v.error_style {
        ErrorStyle::Stop => DataValidationErrorStyle::Stop,
        ErrorStyle::Warning => DataValidationErrorStyle::Warning,
        ErrorStyle::Information => DataValidationErrorStyle::Information,
    });
    if v.ranges.len() > 1 {
        d = d.set_multi_range(sqref(&v.ranges));
    }
    Some(d)
}

/// Writes the sheet's conditional formats (in priority order) and data validations.
pub(super) fn write_sheet_extras(sh: &Sheet, ws: &mut Worksheet) {
    for rule in &sh.cond_formats {
        write_cf(ws, rule);
    }
    for v in &sh.validations {
        if v.ranges.is_empty() || !v.ranges.iter().all(valid) {
            continue;
        }
        if let Some(d) = build_dv(v) {
            let r = v.ranges[0];
            let _ = ws.add_data_validation(r.r1, r.c1 as u16, r.r2, r.c2 as u16, &d);
        }
    }
}
