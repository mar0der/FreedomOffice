//! Conditional formatting: rules, per-rule statistics and per-cell evaluation.
//!
//! Rules live on the sheet in priority order (index 0 = highest priority). Formulas are
//! stored as text without the leading '=' and are relative to the top-left cell of the
//! rule's first range, the same convention the file format uses.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::formula::{self, Expr};
use crate::model::{Range, Sheet, Value, Workbook, MAX_COLS, MAX_ROWS};

// ---------------------------------------------------------------------------
// Rule model
// ---------------------------------------------------------------------------

/// Comparison operator shared by cell-value rules and data validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CmpOp {
    #[default]
    Between,
    NotBetween,
    Equal,
    NotEqual,
    Greater,
    Less,
    GreaterEq,
    LessEq,
}

impl CmpOp {
    pub const ALL: [CmpOp; 8] = [
        CmpOp::Between,
        CmpOp::NotBetween,
        CmpOp::Equal,
        CmpOp::NotEqual,
        CmpOp::Greater,
        CmpOp::Less,
        CmpOp::GreaterEq,
        CmpOp::LessEq,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CmpOp::Between => "between",
            CmpOp::NotBetween => "not between",
            CmpOp::Equal => "equal to",
            CmpOp::NotEqual => "not equal to",
            CmpOp::Greater => "greater than",
            CmpOp::Less => "less than",
            CmpOp::GreaterEq => "greater than or equal to",
            CmpOp::LessEq => "less than or equal to",
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            CmpOp::Between => "between",
            CmpOp::NotBetween => "not between",
            CmpOp::Equal => "=",
            CmpOp::NotEqual => "<>",
            CmpOp::Greater => ">",
            CmpOp::Less => "<",
            CmpOp::GreaterEq => ">=",
            CmpOp::LessEq => "<=",
        }
    }

    pub fn xml(self) -> &'static str {
        match self {
            CmpOp::Between => "between",
            CmpOp::NotBetween => "notBetween",
            CmpOp::Equal => "equal",
            CmpOp::NotEqual => "notEqual",
            CmpOp::Greater => "greaterThan",
            CmpOp::Less => "lessThan",
            CmpOp::GreaterEq => "greaterThanOrEqual",
            CmpOp::LessEq => "lessThanOrEqual",
        }
    }

    pub fn from_xml(s: &str) -> Option<CmpOp> {
        CmpOp::ALL.into_iter().find(|o| o.xml() == s)
    }

    /// True when the operator takes two operands.
    pub fn two(self) -> bool {
        matches!(self, CmpOp::Between | CmpOp::NotBetween)
    }

    /// Tests `v` against the operand(s) using spreadsheet ordering (numbers < text < logicals).
    pub fn test(self, v: &Value, a: &Value, b: &Value) -> bool {
        let cmp = formula::compare;
        match self {
            CmpOp::Between | CmpOp::NotBetween => {
                let (lo, hi) = if cmp(a, b) == Ordering::Greater { (b, a) } else { (a, b) };
                let inside = cmp(v, lo) != Ordering::Less && cmp(v, hi) != Ordering::Greater;
                inside == (self == CmpOp::Between)
            }
            CmpOp::Equal => cmp(v, a) == Ordering::Equal,
            CmpOp::NotEqual => cmp(v, a) != Ordering::Equal,
            CmpOp::Greater => cmp(v, a) == Ordering::Greater,
            CmpOp::Less => cmp(v, a) == Ordering::Less,
            CmpOp::GreaterEq => cmp(v, a) != Ordering::Less,
            CmpOp::LessEq => cmp(v, a) != Ordering::Greater,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TextOp {
    #[default]
    Contains,
    NotContains,
    BeginsWith,
    EndsWith,
}

impl TextOp {
    pub const ALL: [TextOp; 4] = [TextOp::Contains, TextOp::NotContains, TextOp::BeginsWith, TextOp::EndsWith];

    pub fn label(self) -> &'static str {
        match self {
            TextOp::Contains => "containing",
            TextOp::NotContains => "not containing",
            TextOp::BeginsWith => "beginning with",
            TextOp::EndsWith => "ending with",
        }
    }

    pub fn test(self, hay: &str, needle: &str) -> bool {
        let (h, n) = (hay.to_lowercase(), needle.to_lowercase());
        match self {
            TextOp::Contains => h.contains(&n),
            TextOp::NotContains => !h.contains(&n),
            TextOp::BeginsWith => h.starts_with(&n),
            TextOp::EndsWith => h.ends_with(&n),
        }
    }
}

/// Differential format applied when a rule is true. Flags only switch attributes on.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DiffFormat {
    pub fill: Option<u32>,
    pub color: Option<u32>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Outline border colour.
    pub border: Option<u32>,
}

impl DiffFormat {
    pub fn is_empty(&self) -> bool {
        *self == DiffFormat::default()
    }

    /// The preset formats offered by the quick rule dialogs.
    pub fn presets() -> Vec<(&'static str, DiffFormat)> {
        let f = |fill: Option<u32>, color: Option<u32>, border: Option<u32>| DiffFormat { fill, color, border, ..Default::default() };
        vec![
            ("Light Red Fill with Dark Red Text", f(Some(0xFFC7CE), Some(0x9C0006), None)),
            ("Yellow Fill with Dark Yellow Text", f(Some(0xFFEB9C), Some(0x9C5700), None)),
            ("Green Fill with Dark Green Text", f(Some(0xC6EFCE), Some(0x006100), None)),
            ("Light Red Fill", f(Some(0xFFC7CE), None, None)),
            ("Red Text", f(None, Some(0x9C0006), None)),
            ("Red Border", f(None, None, Some(0xFF0000))),
        ]
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RuleKind {
    /// Cell value compared with one or two operand formulas.
    CellValue { op: CmpOp, f1: String, f2: String },
    Text { op: TextOp, text: String },
    Duplicate { unique: bool },
    Top { bottom: bool, percent: bool, rank: u32 },
    Average { below: bool, equal: bool },
    Blanks { not: bool },
    Errors { not: bool },
    /// Formula that must be true, relative to the top-left cell of the first range.
    Expression { formula: String },
    DataBar { color: u32, gradient: bool },
    /// Two (min, max) or three (min, 50th percentile, max) colours.
    ColorScale { colors: Vec<u32> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct CondFormat {
    pub ranges: Vec<Range>,
    pub kind: RuleKind,
    pub format: DiffFormat,
    pub stop: bool,
}

impl CondFormat {
    pub fn new(ranges: Vec<Range>, kind: RuleKind, format: DiffFormat) -> Self {
        CondFormat { ranges, kind, format, stop: false }
    }

    pub fn applies(&self, r: u32, c: u32) -> bool {
        self.ranges.iter().any(|rg| rg.contains(r, c))
    }

    /// Cell that relative references in the rule's formulas are anchored to.
    pub fn base(&self) -> (u32, u32) {
        self.ranges.first().map(|r| (r.r1, r.c1)).unwrap_or((0, 0))
    }

    /// "Applies to" text as shown in the rules manager, e.g. "=$A$1:$A$10,$C:$C".
    pub fn applies_to(&self) -> String {
        let abs = |p: &str| {
            let i = p.find(|c: char| c.is_ascii_digit()).unwrap_or(p.len());
            let (l, d) = p.split_at(i);
            format!("{}{}", if l.is_empty() { String::new() } else { format!("${l}") }, if d.is_empty() { String::new() } else { format!("${d}") })
        };
        let parts: Vec<String> = self.ranges.iter().map(|r| r.to_a1().split(':').map(abs).collect::<Vec<_>>().join(":")).collect();
        format!("={}", parts.join(","))
    }

    /// Visual rules (bars, scales) colour cells by value rather than by a true/false test.
    pub fn is_visual(&self) -> bool {
        matches!(self.kind, RuleKind::DataBar { .. } | RuleKind::ColorScale { .. })
    }

    fn needs_stats(&self) -> bool {
        matches!(
            self.kind,
            RuleKind::Duplicate { .. } | RuleKind::Top { .. } | RuleKind::Average { .. } | RuleKind::DataBar { .. } | RuleKind::ColorScale { .. }
        )
    }

    /// Short description for the rules manager.
    pub fn describe(&self) -> String {
        match &self.kind {
            RuleKind::CellValue { op, f1, f2 } => {
                // Constants read as typed (8, "abc"); anything else as a formula (=$B$1).
                let show = |f: &str| match formula::parse(f) {
                    Ok(Expr::Num(_) | Expr::Str(_)) => f.to_string(),
                    _ => format!("={f}"),
                };
                if op.two() {
                    format!("Cell Value {} {} and {}", op.symbol(), show(f1), show(f2))
                } else {
                    format!("Cell Value {} {}", op.symbol(), show(f1))
                }
            }
            RuleKind::Text { op, text } => format!("Cell Value {} \"{}\"", op.label(), text),
            RuleKind::Duplicate { unique } => if *unique { "Unique Values".into() } else { "Duplicate Values".into() },
            RuleKind::Top { bottom, percent, rank } => {
                format!("{} {}{}", if *bottom { "Bottom" } else { "Top" }, rank, if *percent { "%" } else { "" })
            }
            RuleKind::Average { below, equal } => format!(
                "{}{} Average",
                if *below { "Below" } else { "Above" },
                if *equal { " or Equal to" } else { "" }
            ),
            RuleKind::Blanks { not } => if *not { "No Blanks".into() } else { "Blanks".into() },
            RuleKind::Errors { not } => if *not { "No Errors".into() } else { "Errors".into() },
            RuleKind::Expression { formula } => format!("Formula: ={formula}"),
            RuleKind::DataBar { .. } => "Data Bar".into(),
            RuleKind::ColorScale { colors } => if colors.len() >= 3 { "Graded Color Scale (3)".into() } else { "Graded Color Scale".into() },
        }
    }
}

// ---------------------------------------------------------------------------
// Ranges and structure changes
// ---------------------------------------------------------------------------

/// Space separated list of ranges, e.g. "A1:B5 D2".
pub fn sqref(ranges: &[Range]) -> String {
    ranges.iter().map(|r| r.to_a1()).collect::<Vec<_>>().join(" ")
}

/// Parses "A1:B5 D2" or "=$A$1:$B$5,$D$2"; None when any part is invalid.
pub fn parse_sqref(s: &str) -> Option<Vec<Range>> {
    let s = s.trim().trim_start_matches('=');
    let out: Option<Vec<Range>> = s
        .split([' ', ',', ';'])
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            // Drop a sheet prefix such as Sheet1!A1:B2.
            let p = p.rsplit('!').next().unwrap_or(p);
            Range::parse(p)
        })
        .collect();
    out.filter(|v| !v.is_empty())
}

/// Adjusts a range after rows (rows=true) or columns are inserted (n>0) or deleted (n<0) at `at`.
pub fn shift_range(rg: Range, rows: bool, at: u32, n: i64) -> Option<Range> {
    let max = if rows { MAX_ROWS } else { MAX_COLS } as i64;
    let (a, b) = if rows { (rg.r1, rg.r2) } else { (rg.c1, rg.c2) };
    let (na, nb) = if n > 0 {
        if a as i64 + n >= max && a >= at {
            return None;
        }
        (if a >= at { (a as i64 + n) as u32 } else { a }, if b >= at { (b as i64 + n).min(max - 1) as u32 } else { b })
    } else {
        let end = at as i64 - n;
        let na = if (a as i64) >= end { (a as i64 + n) as u32 } else if a >= at { at } else { a };
        let nb = if (b as i64) >= end { (b as i64 + n) as u32 } else if b >= at { at.checked_sub(1)? } else { b };
        if nb < na {
            return None;
        }
        (na, nb)
    };
    Some(if rows { Range { r1: na, r2: nb, ..rg } } else { Range { c1: na, c2: nb, ..rg } })
}

/// The parts of `rg` that lie outside `cut` (up to four rectangles).
pub fn subtract(rg: Range, cut: Range) -> Vec<Range> {
    if !rg.intersects(&cut) {
        return vec![rg];
    }
    let mut out = Vec::new();
    if rg.r1 < cut.r1 {
        out.push(Range { r2: cut.r1 - 1, ..rg });
    }
    if rg.r2 > cut.r2 {
        out.push(Range { r1: cut.r2 + 1, ..rg });
    }
    let (r1, r2) = (rg.r1.max(cut.r1), rg.r2.min(cut.r2));
    if rg.c1 < cut.c1 {
        out.push(Range { r1, r2, c1: rg.c1, c2: cut.c1 - 1 });
    }
    if rg.c2 > cut.c2 {
        out.push(Range { r1, r2, c1: cut.c2 + 1, c2: rg.c2 });
    }
    out
}

/// Removes `cut` from every range in the list.
pub fn subtract_all(ranges: &[Range], cut: Range) -> Vec<Range> {
    ranges.iter().flat_map(|r| subtract(*r, cut)).collect()
}

fn shift_list(ranges: &mut Vec<Range>, rows: bool, at: u32, n: i64) {
    *ranges = ranges.iter().filter_map(|r| shift_range(*r, rows, at, n)).collect();
}

/// Moves conditional format and validation ranges after a row/column insert or delete.
pub fn shift_sheet_rules(sh: &mut Sheet, rows: bool, at: u32, n: i64) {
    for cf in sh.cond_formats.iter_mut() {
        shift_list(&mut cf.ranges, rows, at, n);
    }
    sh.cond_formats.retain(|cf| !cf.ranges.is_empty());
    for dv in sh.validations.iter_mut() {
        shift_list(&mut dv.ranges, rows, at, n);
    }
    sh.validations.retain(|dv| !dv.ranges.is_empty());
}

/// Applies a formula transform to a stored formula text (no leading '=').
pub fn map_formula_text(text: &mut String, f: &mut dyn FnMut(&mut Expr)) {
    if text.trim().is_empty() {
        return;
    }
    if let Ok(mut e) = formula::parse(text.trim_start_matches('=')) {
        let before = e.clone();
        f(&mut e);
        if e != before {
            *text = formula::print(&e);
        }
    }
}

/// Visits every formula held by the sheet's conditional formats and validations.
pub fn map_sheet_formulas(sh: &mut Sheet, f: &mut dyn FnMut(&mut Expr)) {
    for cf in sh.cond_formats.iter_mut() {
        match &mut cf.kind {
            RuleKind::CellValue { f1, f2, .. } => {
                map_formula_text(f1, f);
                map_formula_text(f2, f);
            }
            RuleKind::Expression { formula } => map_formula_text(formula, f),
            _ => {}
        }
    }
    for dv in sh.validations.iter_mut() {
        map_formula_text(&mut dv.f1, f);
        map_formula_text(&mut dv.f2, f);
    }
}

/// Text typed into a value box (a number, plain text or `=formula`) -> stored formula text.
pub fn operand_from_input(s: &str) -> String {
    let t = s.trim();
    if let Some(f) = t.strip_prefix('=') {
        return f.trim().to_string();
    }
    if t.is_empty() {
        return String::new();
    }
    match crate::numfmt::parse_input(t).0 {
        Value::Number(n) => formula::print(&Expr::Num(n)),
        Value::Bool(b) => formula::print(&Expr::Bool(b)),
        _ => formula::print(&Expr::Str(t.to_string())),
    }
}

/// Stored operand formula -> text for a value box; `fmt` formats plain numbers (dates, times).
pub fn operand_to_input(f: &str, fmt: &str) -> String {
    if f.trim().is_empty() {
        return String::new();
    }
    match formula::parse(f.trim_start_matches('=')) {
        Ok(Expr::Num(n)) => crate::numfmt::format_value(&Value::Number(n), fmt).text,
        Ok(Expr::Neg(e)) if matches!(*e, Expr::Num(_)) => format!("-{}", operand_to_input(&formula::print(&e), fmt)),
        Ok(Expr::Str(s)) => s,
        _ => format!("={}", f.trim_start_matches('=')),
    }
}

// ---------------------------------------------------------------------------
// Statistics
// ---------------------------------------------------------------------------

/// Values gathered once per rule: numeric summary, rank threshold and duplicate counts.
#[derive(Clone, Debug, Default)]
pub struct RuleStats {
    pub count: usize,
    pub min: f64,
    pub max: f64,
    pub mid: f64,
    pub avg: f64,
    /// Top/bottom rule cut-off value.
    pub threshold: f64,
    pub dups: HashMap<String, u32>,
}

/// Key used to compare values for duplicate detection (case-insensitive text).
fn dup_key(v: &Value) -> Option<String> {
    match v {
        Value::Empty | Value::Error(_) => None,
        Value::Number(n) => Some(format!("n{n}")),
        Value::Text(s) => Some(format!("t{}", s.to_lowercase())),
        Value::Bool(b) => Some(format!("b{b}")),
    }
}

/// Inclusive percentile (PERCENTILE.INC) of sorted values.
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let k = (sorted.len() - 1) as f64 * p.clamp(0.0, 1.0);
    let (i, f) = (k.floor() as usize, k.fract());
    if i + 1 < sorted.len() { sorted[i] + (sorted[i + 1] - sorted[i]) * f } else { sorted[i] }
}

pub fn compute_stats(sheet: &Sheet, rule: &CondFormat) -> RuleStats {
    let mut nums = Vec::new();
    let mut st = RuleStats::default();
    let want_dups = matches!(rule.kind, RuleKind::Duplicate { .. });
    let mut seen: HashSet<(u32, u32)> = HashSet::new();
    let (er, ec) = sheet.extent().unwrap_or((0, 0));
    for rg in &rule.ranges {
        let clip = Range { r1: rg.r1, c1: rg.c1, r2: rg.r2.min(er.max(rg.r1)), c2: rg.c2.min(ec.max(rg.c1)) };
        for (r, c, cell) in sheet.iter_range(clip) {
            if rule.ranges.len() > 1 && !seen.insert((r, c)) {
                continue;
            }
            if let Value::Number(n) = cell.value {
                nums.push(n);
            }
            if want_dups {
                if let Some(k) = dup_key(&cell.value) {
                    *st.dups.entry(k).or_insert(0) += 1;
                }
            }
        }
    }
    nums.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    st.count = nums.len();
    if let (Some(&lo), Some(&hi)) = (nums.first(), nums.last()) {
        st.min = lo;
        st.max = hi;
        st.avg = nums.iter().sum::<f64>() / nums.len() as f64;
        st.mid = percentile(&nums, 0.5);
    }
    if let RuleKind::Top { bottom, percent, rank } = rule.kind {
        if !nums.is_empty() {
            let n = if percent { (nums.len() as f64 * rank as f64 / 100.0).floor() as usize } else { rank as usize };
            let n = n.clamp(1, nums.len());
            st.threshold = if bottom { nums[n - 1] } else { nums[nums.len() - n] };
        }
    }
    st
}

// ---------------------------------------------------------------------------
// Colour scales and data bars
// ---------------------------------------------------------------------------

fn lerp_color(a: u32, b: u32, t: f64) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |s: u32| -> u32 {
        let x = ((a >> s) & 0xFF) as f64;
        let y = ((b >> s) & 0xFF) as f64;
        ((x + (y - x) * t).round() as u32).min(255) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// Colour for `v` on a 2- or 3-colour scale (min, [50th percentile], max).
pub fn scale_color(colors: &[u32], st: &RuleStats, v: f64) -> u32 {
    let frac = |lo: f64, hi: f64| if hi > lo { (v - lo) / (hi - lo) } else { 0.0 };
    match colors {
        [a, b] => lerp_color(*a, *b, frac(st.min, st.max)),
        [a, m, b, ..] => {
            if st.max <= st.min {
                *m
            } else if v <= st.mid {
                lerp_color(*a, *m, frac(st.min, st.mid))
            } else {
                lerp_color(*m, *b, frac(st.mid, st.max))
            }
        }
        [a] => *a,
        [] => 0xFFFFFF,
    }
}

/// Horizontal extent of a data bar as fractions (start, end) of the cell width.
/// Positive bars grow right from the axis; negative bars grow left from it.
pub fn bar_extent(st: &RuleStats, v: f64) -> (f32, f32) {
    let lo = st.min.min(0.0);
    let hi = st.max.max(0.0);
    if hi <= lo {
        return (0.0, 0.0);
    }
    let pos = |x: f64| ((x.clamp(lo, hi) - lo) / (hi - lo)) as f32;
    let axis = pos(0.0);
    let p = pos(v);
    if p >= axis { (axis, p) } else { (p, axis) }
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DataBarDraw {
    pub start: f32,
    pub end: f32,
    pub color: u32,
    pub gradient: bool,
    /// Negative bars grow left from the axis (their gradient is mirrored).
    pub negative: bool,
}

/// What the conditional formats change for one cell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CfOverride {
    pub fill: Option<u32>,
    pub color: Option<u32>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub border: Option<u32>,
    pub bar: Option<DataBarDraw>,
}

impl CfOverride {
    fn merge(&mut self, f: &DiffFormat) {
        self.fill = self.fill.or(f.fill);
        self.color = self.color.or(f.color);
        self.border = self.border.or(f.border);
        self.bold |= f.bold;
        self.italic |= f.italic;
        self.underline |= f.underline;
        self.strike |= f.strike;
    }
}

struct Prepared {
    rule: CondFormat,
    base: (u32, u32),
    f1: Option<Expr>,
    f2: Option<Expr>,
    stats: Option<Rc<RuleStats>>,
}

/// The rules of one sheet with parsed formulas and statistics, ready for painting.
#[derive(Default)]
pub struct CfFrame {
    rules: Vec<Prepared>,
}

fn parse_opt(s: &str) -> Option<Expr> {
    let s = s.trim().trim_start_matches('=');
    if s.is_empty() { None } else { formula::parse(s).ok() }
}

/// Formula result read as a condition (errors and text other than TRUE are false).
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Number(n) => *n != 0.0,
        Value::Bool(b) => *b,
        Value::Text(s) => s.eq_ignore_ascii_case("TRUE"),
        Value::Empty | Value::Error(_) => false,
    }
}

/// Evaluates an anchored formula for cell (r, c) by shifting its relative references.
pub fn eval_at(wb: &Workbook, si: usize, base: (u32, u32), e: &Expr, r: u32, c: u32) -> Value {
    match e {
        Expr::Num(n) => Value::Number(*n),
        Expr::Str(s) => Value::Text(s.clone()),
        Expr::Bool(b) => Value::Bool(*b),
        _ => {
            let mut e = e.clone();
            formula::shift_relative(&mut e, r as i64 - base.0 as i64, c as i64 - base.1 as i64);
            formula::eval_expr(wb, si, r, c, &e)
        }
    }
}

impl CfFrame {
    /// Prepares a sheet's rules; `stats` supplies (possibly cached) statistics for rule i.
    pub fn new(sheet: &Sheet, mut stats: impl FnMut(usize, &CondFormat) -> Rc<RuleStats>) -> Self {
        let rules = sheet
            .cond_formats
            .iter()
            .enumerate()
            .map(|(i, rule)| {
                let (f1, f2) = match &rule.kind {
                    RuleKind::CellValue { f1, f2, .. } => (parse_opt(f1), parse_opt(f2)),
                    RuleKind::Expression { formula } => (parse_opt(formula), None),
                    _ => (None, None),
                };
                Prepared {
                    base: rule.base(),
                    stats: if rule.needs_stats() { Some(stats(i, rule)) } else { None },
                    rule: rule.clone(),
                    f1,
                    f2,
                }
            })
            .collect();
        CfFrame { rules }
    }

    /// Builds a frame computing statistics directly.
    pub fn uncached(sheet: &Sheet) -> Self {
        Self::new(sheet, |_, rule| Rc::new(compute_stats(sheet, rule)))
    }

    /// Combined effect of all rules on cell (r, c) of sheet `si`; None when nothing applies.
    pub fn eval(&self, wb: &Workbook, si: usize, r: u32, c: u32) -> Option<CfOverride> {
        if self.rules.is_empty() {
            return None;
        }
        let sheet = wb.sheets.get(si)?;
        let v = sheet.value(r, c);
        let mut out = CfOverride::default();
        let mut any = false;
        for p in &self.rules {
            if !p.rule.applies(r, c) {
                continue;
            }
            match &p.rule.kind {
                RuleKind::DataBar { color, gradient } => {
                    if let (Value::Number(n), Some(st), None) = (v, &p.stats, out.bar) {
                        let (start, end) = bar_extent(st, *n);
                        let color = if *n < 0.0 { 0xFF0000 } else { *color };
                        out.bar = Some(DataBarDraw { start, end, color, gradient: *gradient, negative: *n < 0.0 });
                        any = true;
                    }
                }
                RuleKind::ColorScale { colors } => {
                    if let (Value::Number(n), Some(st)) = (v, &p.stats) {
                        if out.fill.is_none() {
                            out.fill = Some(scale_color(colors, st, *n));
                        }
                        any = true;
                    }
                }
                _ => {
                    if self.matches(p, wb, si, v, r, c) {
                        out.merge(&p.rule.format);
                        any = true;
                        if p.rule.stop {
                            break;
                        }
                    }
                }
            }
        }
        any.then_some(out)
    }

    fn matches(&self, p: &Prepared, wb: &Workbook, si: usize, v: &Value, r: u32, c: u32) -> bool {
        match &p.rule.kind {
            RuleKind::CellValue { op, .. } => {
                if matches!(v, Value::Error(_)) {
                    return false;
                }
                let get = |e: &Option<Expr>| e.as_ref().map(|e| eval_at(wb, si, p.base, e, r, c)).unwrap_or(Value::Empty);
                let a = get(&p.f1);
                let b = if op.two() { get(&p.f2) } else { Value::Empty };
                if matches!(a, Value::Error(_)) || matches!(b, Value::Error(_)) {
                    return false;
                }
                op.test(v, &a, &b)
            }
            RuleKind::Text { op, text } => {
                if matches!(v, Value::Error(_)) {
                    return false;
                }
                op.test(&crate::numfmt::value_to_text(v), text)
            }
            RuleKind::Duplicate { unique } => match (dup_key(v), &p.stats) {
                (Some(k), Some(st)) => {
                    let n = st.dups.get(&k).copied().unwrap_or(0);
                    if *unique { n == 1 } else { n > 1 }
                }
                _ => false,
            },
            RuleKind::Top { bottom, .. } => match (v, &p.stats) {
                (Value::Number(n), Some(st)) if st.count > 0 => {
                    if *bottom { *n <= st.threshold } else { *n >= st.threshold }
                }
                _ => false,
            },
            RuleKind::Average { below, equal } => match (v, &p.stats) {
                (Value::Number(n), Some(st)) if st.count > 0 => match (below, equal) {
                    (false, false) => *n > st.avg,
                    (false, true) => *n >= st.avg,
                    (true, false) => *n < st.avg,
                    (true, true) => *n <= st.avg,
                },
                _ => false,
            },
            RuleKind::Blanks { not } => {
                let blank = match v {
                    Value::Empty => true,
                    Value::Text(s) => s.trim().is_empty(),
                    _ => false,
                };
                blank != *not
            }
            RuleKind::Errors { not } => matches!(v, Value::Error(_)) != *not,
            RuleKind::Expression { .. } => match &p.f1 {
                Some(e) => truthy(&eval_at(wb, si, p.base, e, r, c)),
                None => false,
            },
            RuleKind::DataBar { .. } | RuleKind::ColorScale { .. } => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, Formula, Sheet, Workbook};

    fn book(vals: &[(u32, u32, Value)]) -> Workbook {
        let mut sh = Sheet::new("Sheet1");
        for (r, c, v) in vals {
            sh.set(*r, *c, Some(Cell { value: v.clone(), formula: None, style: 0 }));
        }
        Workbook::empty_with(vec![sh])
    }

    fn col(nums: &[f64]) -> Workbook {
        let v: Vec<(u32, u32, Value)> = nums.iter().enumerate().map(|(i, n)| (i as u32, 0, Value::Number(*n))).collect();
        book(&v)
    }

    fn rg(s: &str) -> Vec<Range> {
        parse_sqref(s).unwrap()
    }

    fn red() -> DiffFormat {
        DiffFormat { fill: Some(0xFF0000), ..Default::default() }
    }

    fn hits(wb: &mut Workbook, rule: CondFormat, rows: u32) -> Vec<u32> {
        wb.sheets[0].cond_formats = vec![rule];
        let frame = CfFrame::uncached(&wb.sheets[0]);
        (0..rows).filter(|r| frame.eval(wb, 0, *r, 0).is_some_and(|o| o.fill.is_some())).collect()
    }

    #[test]
    fn cell_value_rules() {
        let mut wb = col(&[1.0, 5.0, 10.0, 15.0]);
        let gt = |f1: &str| RuleKind::CellValue { op: CmpOp::Greater, f1: f1.into(), f2: String::new() };
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A4"), gt("5"), red()), 4), vec![2, 3]);
        let between = RuleKind::CellValue { op: CmpOp::Between, f1: "10".into(), f2: "4".into() };
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A4"), between, red()), 4), vec![1, 2]);
        let nb = RuleKind::CellValue { op: CmpOp::NotBetween, f1: "4".into(), f2: "10".into() };
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A4"), nb, red()), 4), vec![0, 3]);
        // Operand referring to a cell (absolute) and a relative one (compare with column B).
        wb.sheets[0].set(0, 1, Some(Cell { value: Value::Number(9.0), formula: None, style: 0 }));
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A4"), gt("$B$1"), red()), 4), vec![2, 3]);
        let eq = RuleKind::CellValue { op: CmpOp::Equal, f1: "B1".into(), f2: String::new() };
        // A1=1 vs B1=9 no; A2=5 vs B2 empty(0) no; ...
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A4"), eq, red()), 4), Vec::<u32>::new());
        // Text compared with a quoted string, case-insensitive.
        let mut wb = book(&[(0, 0, Value::Text("Apple".into())), (1, 0, Value::Text("pear".into()))]);
        let eq = RuleKind::CellValue { op: CmpOp::Equal, f1: "\"apple\"".into(), f2: String::new() };
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A2"), eq, red()), 2), vec![0]);
    }

    #[test]
    fn text_rules() {
        let mut wb = book(&[
            (0, 0, Value::Text("Hello World".into())),
            (1, 0, Value::Text("world peace".into())),
            (2, 0, Value::Text("other".into())),
        ]);
        let t = |op, s: &str| RuleKind::Text { op, text: s.into() };
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A3"), t(TextOp::Contains, "WORLD"), red()), 3), vec![0, 1]);
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A3"), t(TextOp::BeginsWith, "world"), red()), 3), vec![1]);
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A3"), t(TextOp::EndsWith, "world"), red()), 3), vec![0]);
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A3"), t(TextOp::NotContains, "world"), red()), 3), vec![2]);
    }

    #[test]
    fn top_bottom_rules() {
        let mut wb = col(&[3.0, 9.0, 1.0, 7.0, 5.0, 2.0, 8.0, 4.0, 6.0, 10.0]);
        let top = |bottom, percent, rank| RuleKind::Top { bottom, percent, rank };
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A10"), top(false, false, 3), red()), 10), vec![1, 6, 9]);
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A10"), top(true, false, 2), red()), 10), vec![2, 5]);
        // 20% of 10 values = 2 items.
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A10"), top(false, true, 20), red()), 10), vec![1, 9]);
        // Rank larger than the data covers everything.
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A10"), top(true, false, 50), red()), 10).len(), 10);
    }

    #[test]
    fn average_rules() {
        let mut wb = col(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let avg = |below, equal| RuleKind::Average { below, equal };
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A5"), avg(false, false), red()), 5), vec![3, 4]);
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A5"), avg(true, false), red()), 5), vec![0, 1]);
        assert_eq!(hits(&mut wb, CondFormat::new(rg("A1:A5"), avg(true, true), red()), 5), vec![0, 1, 2]);
    }

    #[test]
    fn duplicate_and_blank_rules() {
        let mut wb = book(&[
            (0, 0, Value::Text("a".into())),
            (1, 0, Value::Text("A".into())),
            (2, 0, Value::Number(2.0)),
            (3, 0, Value::Number(2.0)),
            (4, 0, Value::Number(3.0)),
            (6, 0, Value::Error(crate::model::ErrorKind::Div0)),
        ]);
        let r = rg("A1:A7");
        assert_eq!(hits(&mut wb, CondFormat::new(r.clone(), RuleKind::Duplicate { unique: false }, red()), 7), vec![0, 1, 2, 3]);
        assert_eq!(hits(&mut wb, CondFormat::new(r.clone(), RuleKind::Duplicate { unique: true }, red()), 7), vec![4]);
        assert_eq!(hits(&mut wb, CondFormat::new(r.clone(), RuleKind::Blanks { not: false }, red()), 7), vec![5]);
        assert_eq!(hits(&mut wb, CondFormat::new(r.clone(), RuleKind::Errors { not: false }, red()), 7), vec![6]);
        assert_eq!(hits(&mut wb, CondFormat::new(r, RuleKind::Errors { not: true }, red()), 7), vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn formula_rules_are_relative_to_the_top_left_cell() {
        // A: values, B: limits. Rule on A2:A4 with formula =A2>B2 anchored at A2.
        let mut wb = book(&[
            (1, 0, Value::Number(5.0)),
            (1, 1, Value::Number(1.0)),
            (2, 0, Value::Number(1.0)),
            (2, 1, Value::Number(5.0)),
            (3, 0, Value::Number(9.0)),
            (3, 1, Value::Number(8.0)),
        ]);
        let rule = CondFormat::new(rg("A2:A4"), RuleKind::Expression { formula: "A2>B2".into() }, red());
        assert_eq!(hits(&mut wb, rule, 5), vec![1, 3]);
        // Mixed reference: whole-row highlighting style ($A2) and a function.
        let rule = CondFormat::new(rg("A2:A4"), RuleKind::Expression { formula: "AND($A2>2,ISNUMBER($B2))".into() }, red());
        assert_eq!(hits(&mut wb, rule, 5), vec![1, 3]);
        // Formula cells in the range are evaluated through their cached values.
        wb.sheets[0].set(4, 0, Some(Cell { value: Value::Number(100.0), formula: Some(Box::new(Formula::new("=50*2"))), style: 0 }));
        let rule = CondFormat::new(rg("A5"), RuleKind::Expression { formula: "A5=100".into() }, red());
        assert_eq!(hits(&mut wb, rule, 6), vec![4]);
    }

    #[test]
    fn priority_and_stop_if_true() {
        let mut wb = col(&[10.0]);
        let gt = |f1: &str| RuleKind::CellValue { op: CmpOp::Greater, f1: f1.into(), f2: String::new() };
        let a = CondFormat::new(rg("A1"), gt("5"), DiffFormat { fill: Some(1), bold: true, ..Default::default() });
        let b = CondFormat::new(rg("A1"), gt("1"), DiffFormat { fill: Some(2), color: Some(3), italic: true, ..Default::default() });
        wb.sheets[0].cond_formats = vec![a.clone(), b.clone()];
        let o = CfFrame::uncached(&wb.sheets[0]).eval(&wb, 0, 0, 0).unwrap();
        assert_eq!((o.fill, o.color, o.bold, o.italic), (Some(1), Some(3), true, true));
        wb.sheets[0].cond_formats = vec![CondFormat { stop: true, ..a }, b];
        let o = CfFrame::uncached(&wb.sheets[0]).eval(&wb, 0, 0, 0).unwrap();
        assert_eq!((o.fill, o.color, o.italic), (Some(1), None, false));
    }

    #[test]
    fn color_scale_interpolation() {
        let st = RuleStats { count: 3, min: 0.0, max: 100.0, mid: 50.0, ..Default::default() };
        assert_eq!(scale_color(&[0x000000, 0xFFFFFF], &st, 0.0), 0x000000);
        assert_eq!(scale_color(&[0x000000, 0xFFFFFF], &st, 100.0), 0xFFFFFF);
        assert_eq!(scale_color(&[0x000000, 0xFFFFFF], &st, 50.0), 0x808080);
        let three = [0xF8696B, 0xFFEB84, 0x63BE7B];
        assert_eq!(scale_color(&three, &st, 0.0), 0xF8696B);
        assert_eq!(scale_color(&three, &st, 50.0), 0xFFEB84);
        assert_eq!(scale_color(&three, &st, 100.0), 0x63BE7B);
        assert_eq!(scale_color(&[0xFF0000, 0x0000FF, 0x00FF00], &st, 25.0), 0x800080);
        // Mid point is the 50th percentile of the data.
        let mut wb = col(&[1.0, 2.0, 3.0, 10.0]);
        wb.sheets[0].cond_formats = vec![CondFormat::new(rg("A1:A4"), RuleKind::ColorScale { colors: three.to_vec() }, DiffFormat::default())];
        let st = compute_stats(&wb.sheets[0], &wb.sheets[0].cond_formats[0]);
        assert_eq!((st.min, st.max, st.mid), (1.0, 10.0, 2.5));
        let frame = CfFrame::uncached(&wb.sheets[0]);
        assert_eq!(frame.eval(&wb, 0, 3, 0).unwrap().fill, Some(0x63BE7B));
        assert_eq!(frame.eval(&wb, 0, 0, 0).unwrap().fill, Some(0xF8696B));
        assert!(frame.eval(&wb, 0, 4, 0).is_none(), "blank cells are not coloured");
    }

    #[test]
    fn data_bar_fraction() {
        let st = RuleStats { count: 3, min: 2.0, max: 10.0, ..Default::default() };
        assert_eq!(bar_extent(&st, 10.0), (0.0, 1.0));
        assert_eq!(bar_extent(&st, 5.0), (0.0, 0.5));
        let st = RuleStats { count: 3, min: -5.0, max: 15.0, ..Default::default() };
        assert_eq!(bar_extent(&st, 15.0), (0.25, 1.0));
        assert_eq!(bar_extent(&st, -5.0), (0.0, 0.25));
        let mut wb = col(&[0.0, 50.0, 100.0]);
        wb.sheets[0].cond_formats = vec![CondFormat::new(rg("A1:A3"), RuleKind::DataBar { color: 0x638EC6, gradient: true }, DiffFormat::default())];
        let frame = CfFrame::uncached(&wb.sheets[0]);
        let bar = frame.eval(&wb, 0, 1, 0).unwrap().bar.unwrap();
        assert_eq!((bar.start, bar.end, bar.color), (0.0, 0.5, 0x638EC6));
    }

    #[test]
    fn structure_adjustment() {
        let r = Range::parse("B2:C5").unwrap();
        assert_eq!(shift_range(r, true, 0, 2), Range::parse("B4:C7"));
        assert_eq!(shift_range(r, true, 3, 1), Range::parse("B2:C6"));
        assert_eq!(shift_range(r, true, 10, 1), Some(r));
        assert_eq!(shift_range(r, true, 1, -2), Range::parse("B2:C3"));
        assert_eq!(shift_range(r, true, 0, -1), Range::parse("B1:C4"));
        assert_eq!(shift_range(r, true, 1, -4), None);
        assert_eq!(shift_range(r, false, 0, -1), Range::parse("A2:B5"));
        assert_eq!(shift_range(r, false, 1, -2), None);
        // Whole columns stay whole when rows are inserted.
        let full = Range::parse("A:A").unwrap();
        assert_eq!(shift_range(full, true, 5, 3), Some(full));

        let mut sh = Sheet::new("S");
        sh.cond_formats.push(CondFormat::new(
            rg("A2:A5"),
            RuleKind::Expression { formula: "$B2>Sheet2!$A$1".into() },
            red(),
        ));
        sh.validations.push(crate::validation::Validation {
            ranges: rg("C1:C4 E5"),
            allow: crate::validation::Allow::List,
            f1: "$D$1:$D$9".into(),
            ..Default::default()
        });
        shift_sheet_rules(&mut sh, true, 0, 1);
        map_sheet_formulas(&mut sh, &mut |e| formula::shift_for_structure(e, "S", true, true, 0, 1));
        assert_eq!(sh.cond_formats[0].ranges, rg("A3:A6"));
        assert_eq!(sh.validations[0].ranges, rg("C2:C5 E6"));
        assert_eq!(sh.validations[0].f1, "$D$2:$D$10");
        shift_sheet_rules(&mut sh, false, 2, -1);
        assert_eq!(sh.validations[0].ranges, rg("D6"));
        assert_eq!(sh.cond_formats[0].kind, RuleKind::Expression { formula: "$B3>Sheet2!$A$1".into() });
        map_sheet_formulas(&mut sh, &mut |e| formula::rename_sheet(e, "Sheet2", "Data"));
        assert_eq!(sh.cond_formats[0].kind, RuleKind::Expression { formula: "$B3>Data!$A$1".into() });
        shift_sheet_rules(&mut sh, true, 0, -10);
        assert!(sh.cond_formats.is_empty());
    }

    #[test]
    fn range_subtraction_and_sqref() {
        let pieces = subtract(Range::parse("A1:C3").unwrap(), Range::parse("B2").unwrap());
        let cells: usize = pieces.iter().map(|r| (r.rows() * r.cols()) as usize).sum();
        assert_eq!(cells, 8);
        assert!(pieces.iter().all(|p| !p.contains(1, 1)));
        assert_eq!(subtract(Range::parse("A1:A3").unwrap(), Range::parse("A1:A9").unwrap()), vec![]);
        assert_eq!(sqref(&rg("$A$1:$B$2,D4")), "A1:B2 D4");
        let cf = CondFormat::new(rg("A1:B2 D4 C:C 3:5"), RuleKind::Blanks { not: false }, DiffFormat::default());
        assert_eq!(cf.applies_to(), "=$A$1:$B$2,$D$4,$C:$C,$3:$5");
        assert_eq!(parse_sqref(&cf.applies_to()), Some(cf.ranges.clone()));
        assert!(parse_sqref("A1:B2 zz").is_none());
    }
}
