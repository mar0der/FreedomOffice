//! Excel Tables (ListObjects): model, table-style look and structured references.

use crate::formula::Expr;
use crate::model::{BorderLine, ErrorKind, Range, Sheet, Value, Workbook};

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// Total-row function of a table column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TotalFn {
    #[default]
    None,
    Average,
    Count,
    CountNums,
    Max,
    Min,
    Sum,
    StdDev,
    Var,
    /// A formula written by the user (kept as the cell's own formula).
    Custom,
}

impl TotalFn {
    /// Choices of the total-row dropdown, in Excel's order.
    pub const MENU: [TotalFn; 9] = [
        TotalFn::None,
        TotalFn::Average,
        TotalFn::Count,
        TotalFn::CountNums,
        TotalFn::Max,
        TotalFn::Min,
        TotalFn::Sum,
        TotalFn::StdDev,
        TotalFn::Var,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TotalFn::None => "None",
            TotalFn::Average => "Average",
            TotalFn::Count => "Count",
            TotalFn::CountNums => "Count Numbers",
            TotalFn::Max => "Max",
            TotalFn::Min => "Min",
            TotalFn::Sum => "Sum",
            TotalFn::StdDev => "StdDev",
            TotalFn::Var => "Var",
            TotalFn::Custom => "More Functions...",
        }
    }

    /// SUBTOTAL function number (the 10x variants ignore hidden rows, like Excel's total row).
    pub fn code(self) -> Option<u32> {
        Some(match self {
            TotalFn::Average => 101,
            TotalFn::CountNums => 102,
            TotalFn::Count => 103,
            TotalFn::Max => 104,
            TotalFn::Min => 105,
            TotalFn::StdDev => 107,
            TotalFn::Sum => 109,
            TotalFn::Var => 110,
            TotalFn::None | TotalFn::Custom => return None,
        })
    }

    pub fn from_xml(s: &str) -> TotalFn {
        match s {
            "average" => TotalFn::Average,
            "count" => TotalFn::Count,
            "countNums" => TotalFn::CountNums,
            "max" => TotalFn::Max,
            "min" => TotalFn::Min,
            "sum" => TotalFn::Sum,
            "stdDev" => TotalFn::StdDev,
            "var" => TotalFn::Var,
            "custom" => TotalFn::Custom,
            _ => TotalFn::None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct TableColumn {
    pub name: String,
    pub total: TotalFn,
    /// Text shown in the total row instead of a function (e.g. "Total").
    pub total_label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub name: String,
    /// Whole table, including the header and total rows.
    pub range: Range,
    pub header: bool,
    pub totals: bool,
    /// Table style name, e.g. "TableStyleMedium2"; empty = none.
    pub style: String,
    pub banded_rows: bool,
    pub banded_cols: bool,
    pub first_col: bool,
    pub last_col: bool,
    /// Filter buttons in the header row.
    pub filter: bool,
    pub columns: Vec<TableColumn>,
}

impl Table {
    pub fn new(name: &str, range: Range, style: &str) -> Table {
        Table {
            name: name.to_string(),
            range,
            header: true,
            totals: false,
            style: style.to_string(),
            banded_rows: true,
            banded_cols: false,
            first_col: false,
            last_col: false,
            filter: true,
            columns: (0..range.cols()).map(|i| TableColumn { name: format!("Column{}", i + 1), ..Default::default() }).collect(),
        }
    }

    pub fn header_row(&self) -> Option<u32> {
        self.header.then_some(self.range.r1)
    }

    pub fn totals_row(&self) -> Option<u32> {
        self.totals.then_some(self.range.r2)
    }

    /// Data rows (may be empty when the table has only a header).
    pub fn data(&self) -> Range {
        let r1 = self.range.r1 + self.header as u32;
        let r2 = self.range.r2.saturating_sub(self.totals as u32).max(r1);
        Range { r1, r2, ..self.range }
    }

    pub fn col_index(&self, name: &str) -> Option<usize> {
        let n = name.trim();
        self.columns.iter().position(|c| c.name.eq_ignore_ascii_case(n))
    }

    /// Formula of a column's total-row cell (without '=').
    pub fn total_formula(&self, i: usize) -> Option<String> {
        let col = self.columns.get(i)?;
        let code = col.total.code()?;
        Some(format!("SUBTOTAL({code},{}[{}])", self.name, escape_col(&col.name)))
    }

    /// Makes column names unique and non-empty, like Excel does for headers.
    pub fn fix_column_names(&mut self) {
        let mut seen: Vec<String> = Vec::new();
        for (i, c) in self.columns.iter_mut().enumerate() {
            let base = if c.name.trim().is_empty() { format!("Column{}", i + 1) } else { c.name.clone() };
            let mut name = base.clone();
            let mut k = 2;
            while seen.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
                name = format!("{base}{k}");
                k += 1;
            }
            seen.push(name.clone());
            c.name = name;
        }
    }
}

/// Escapes a column name for use inside structured-reference brackets.
pub fn escape_col(name: &str) -> String {
    let mut s = String::new();
    for ch in name.chars() {
        if matches!(ch, '\'' | '[' | ']' | '#') {
            s.push('\'');
        }
        s.push(ch);
    }
    s
}

/// Text of a cell as a table header (numbers become their text).
pub fn header_text(v: &Value) -> String {
    match v {
        Value::Empty => String::new(),
        Value::Number(n) => crate::numfmt::edit_number(*n),
        Value::Text(s) => s.clone(),
        Value::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
        Value::Error(e) => e.as_str().into(),
    }
}

/// A table name that is free in the workbook: "Table1", "Table2", ...
pub fn unique_name(wb: &Workbook, base: &str) -> String {
    let mut i = 1;
    loop {
        let n = format!("{base}{i}");
        if find(wb, &n).is_none() && !wb.names.iter().any(|(k, _)| k.eq_ignore_ascii_case(&n)) {
            return n;
        }
        i += 1;
    }
}

/// Whether a name is valid as a table / defined name.
pub fn valid_name(n: &str) -> bool {
    let mut it = n.chars();
    let Some(first) = it.next() else { return false };
    (first.is_alphabetic() || first == '_' || first == '\\')
        && n.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '\\')
        && crate::model::parse_cell(n).is_none()
        && !n.eq_ignore_ascii_case("r")
        && !n.eq_ignore_ascii_case("c")
        && n.chars().count() <= 255
}

/// (sheet index, table index) of a table by name.
pub fn find(wb: &Workbook, name: &str) -> Option<(usize, usize)> {
    wb.sheets.iter().enumerate().find_map(|(s, sh)| sh.tables.iter().position(|t| t.name.eq_ignore_ascii_case(name)).map(|i| (s, i)))
}

/// Index of the table containing a cell.
pub fn at(sh: &Sheet, r: u32, c: u32) -> Option<usize> {
    sh.tables.iter().position(|t| t.range.contains(r, c))
}

// ---------------------------------------------------------------------------
// Table styles
// ---------------------------------------------------------------------------

/// Accent colours of the default Office theme; index 0 is the black/grey family.
pub const ACCENTS: [u32; 7] = [0x000000, 0x4472C4, 0xED7D31, 0xA5A5A5, 0xFFC000, 0x5B9BD5, 0x70AD47];

/// "TableStyleMedium2" -> (kind 0 light / 1 medium / 2 dark, accent index 0..=6).
pub fn style_parts(name: &str) -> Option<(u8, usize)> {
    let rest = name.strip_prefix("TableStyle")?;
    let (kind, num) = if let Some(n) = rest.strip_prefix("Light") {
        (0, n)
    } else if let Some(n) = rest.strip_prefix("Medium") {
        (1, n)
    } else if let Some(n) = rest.strip_prefix("Dark") {
        (2, n)
    } else {
        return None;
    };
    let n: usize = num.parse().ok().filter(|n| *n >= 1)?;
    Some((kind, (n - 1) % 7))
}

/// Style name of a (kind, accent index) gallery entry.
pub fn style_name(kind: u8, accent: usize) -> String {
    let fam = match kind {
        0 => "Light",
        1 => "Medium",
        _ => "Dark",
    };
    format!("TableStyle{fam}{}", accent.min(6) + 1)
}

fn tint(c: u32, t: f32) -> u32 {
    let ch = |v: u32| -> u32 {
        let v = v as f32;
        let out = if t >= 0.0 { v + (255.0 - v) * t } else { v * (1.0 + t) };
        out.round().clamp(0.0, 255.0) as u32
    };
    (ch((c >> 16) & 255) << 16) | (ch((c >> 8) & 255) << 8) | ch(c & 255)
}

/// What a table style draws on one cell (under the cell's own formatting).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Look {
    pub fill: Option<u32>,
    pub color: Option<u32>,
    pub bold: bool,
    /// Line drawn along the top edge.
    pub top: Option<(BorderLine, u32)>,
    pub bottom: Option<(BorderLine, u32)>,
}

/// Table-style formatting of cell (r, c) of a table.
pub fn look(t: &Table, r: u32, c: u32) -> Option<Look> {
    if !t.range.contains(r, c) {
        return None;
    }
    let (kind, ai) = style_parts(&t.style)?;
    let accent = ACCENTS[ai];
    let band = if ai == 0 { 0xD9D9D9 } else { tint(accent, 0.8) };
    let mut l = Look::default();
    let is_head = t.header_row() == Some(r);
    let is_total = t.totals_row() == Some(r);
    let data = t.data();
    let ri = r.saturating_sub(data.r1);
    let ci = c - t.range.c1;
    let edge = |w: BorderLine| Some((w, accent));
    match kind {
        0 => {
            if is_head {
                l.bold = true;
                l.bottom = edge(BorderLine::Thin);
            } else if is_total {
                l.bold = true;
                l.top = edge(BorderLine::Double);
            } else {
                if t.banded_rows && ri % 2 == 0 {
                    l.fill = Some(band);
                }
                if t.banded_cols && ci % 2 == 0 {
                    l.fill = Some(band);
                }
            }
            if r == t.range.r1 {
                l.top = edge(BorderLine::Thin);
            }
            if r == t.range.r2 && !is_total {
                l.bottom = edge(BorderLine::Thin);
            }
        }
        1 => {
            if is_head {
                l.bold = true;
                l.fill = Some(accent);
                l.color = Some(0xFFFFFF);
            } else if is_total {
                l.bold = true;
                l.top = edge(BorderLine::Double);
            } else {
                if t.banded_rows && ri % 2 == 0 {
                    l.fill = Some(band);
                }
                if t.banded_cols && ci % 2 == 0 {
                    l.fill = Some(band);
                }
                l.bottom = Some((BorderLine::Thin, tint(accent, 0.4)));
            }
        }
        _ => {
            l.color = Some(0xFFFFFF);
            if is_head {
                l.bold = true;
                l.fill = Some(0x000000);
                l.bottom = Some((BorderLine::Medium, 0xFFFFFF));
            } else if is_total {
                l.bold = true;
                l.fill = Some(tint(accent, -0.5));
                l.top = Some((BorderLine::Double, 0xFFFFFF));
            } else {
                let dark = if ai == 0 { 0x404040 } else { tint(accent, -0.5) };
                let light = if ai == 0 { 0x595959 } else { tint(accent, -0.25) };
                let banded = (t.banded_rows && ri % 2 == 0) || (t.banded_cols && ci % 2 == 0);
                l.fill = Some(if banded { light } else { dark });
            }
        }
    }
    if (t.first_col && c == t.range.c1) || (t.last_col && c == t.range.c2) {
        l.bold = true;
    }
    Some(l)
}

// ---------------------------------------------------------------------------
// Structured references
// ---------------------------------------------------------------------------

/// Whether name text is a structured reference (`Table1[Col]`, `[@Col]`).
pub fn is_structured(text: &str) -> bool {
    text.contains('[')
}

/// Splits `Table1[spec]` into ("Table1", "[spec]").
fn split(text: &str) -> (&str, &str) {
    match text.find('[') {
        Some(i) => (&text[..i], &text[i..]),
        None => (text, ""),
    }
}

/// Removes the `'` escapes of a column name.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\'' {
            if let Some(n) = it.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Splits the inside of a bracket at top-level commas.
fn top_level_items(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start, mut esc) = (0i32, 0usize, false);
    for (i, c) in s.char_indices() {
        if esc {
            esc = false;
            continue;
        }
        match c {
            '\'' => esc = true,
            '[' => depth += 1,
            ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

/// Strips one level of surrounding brackets.
fn unbracket(s: &str) -> &str {
    let s = s.trim();
    if s.starts_with('[') && s.ends_with(']') && s.len() >= 2 { &s[1..s.len() - 1] } else { s }
}

#[derive(Default)]
struct Spec {
    all: bool,
    data: bool,
    headers: bool,
    totals: bool,
    this_row: bool,
    cols: Option<(String, String)>,
}

fn parse_spec(spec: &str) -> Result<Spec, ErrorKind> {
    let mut out = Spec::default();
    let inner = unbracket(spec);
    let inner = inner.trim();
    if inner.is_empty() {
        return Ok(out);
    }
    let (inner, at) = match inner.strip_prefix('@') {
        Some(rest) => (rest.trim(), true),
        None => (inner, false),
    };
    out.this_row = at;
    if inner.is_empty() {
        return Ok(out);
    }
    let items: Vec<&str> = if inner.starts_with('[') { top_level_items(inner) } else { vec![inner] };
    for it in items {
        if it.is_empty() {
            continue;
        }
        // Column range `[A]:[B]` (only when the item is made of bracketed parts).
        let col_range = if it.starts_with('[') {
            let parts = top_level_colon(it);
            if parts.len() == 2 { Some((unbracket(parts[0]).to_string(), unbracket(parts[1]).to_string())) } else { None }
        } else {
            None
        };
        if let Some((a, b)) = col_range {
            out.cols = Some((unescape(&a), unescape(&b)));
            continue;
        }
        let body = unbracket(it);
        match body.to_ascii_lowercase().as_str() {
            "#all" => out.all = true,
            "#data" => out.data = true,
            "#headers" => out.headers = true,
            "#totals" => out.totals = true,
            "#this row" => out.this_row = true,
            _ if body.starts_with('#') => return Err(ErrorKind::Ref),
            _ => {
                let n = unescape(body);
                out.cols = Some((n.clone(), n));
            }
        }
    }
    Ok(out)
}

fn top_level_colon(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start, mut esc) = (0i32, 0usize, false);
    for (i, c) in s.char_indices() {
        if esc {
            esc = false;
            continue;
        }
        match c {
            '\'' => esc = true,
            '[' => depth += 1,
            ']' => depth -= 1,
            ':' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// Resolves a structured reference (or a bare table name) to (sheet index, range).
/// `cur` is the formula's (sheet, row, col); a row of `None` widens `#This Row` to all data rows
/// (used for dependency tracking).
pub fn resolve(wb: &Workbook, text: &str, cur_sheet: usize, cur_cell: Option<(u32, u32)>) -> Result<(usize, Range), ErrorKind> {
    let (tname, spec) = split(text.trim());
    let (s, t) = if tname.is_empty() {
        let sh = wb.sheets.get(cur_sheet).ok_or(ErrorKind::Ref)?;
        let (r, c) = cur_cell.ok_or(ErrorKind::Ref)?;
        let i = at(sh, r, c).ok_or(ErrorKind::Ref)?;
        (cur_sheet, &sh.tables[i])
    } else {
        let (s, i) = find(wb, tname).ok_or(ErrorKind::Name)?;
        (s, &wb.sheets[s].tables[i])
    };
    let sp = parse_spec(spec)?;
    let (c1, c2) = match &sp.cols {
        Some((a, b)) => {
            let a = t.col_index(a).ok_or(ErrorKind::Ref)? as u32;
            let b = t.col_index(b).ok_or(ErrorKind::Ref)? as u32;
            (t.range.c1 + a.min(b), t.range.c1 + a.max(b))
        }
        None => (t.range.c1, t.range.c2),
    };
    let data = t.data();
    let (r1, r2) = if sp.this_row {
        match cur_cell {
            Some((r, _)) if s == cur_sheet && r >= data.r1 && r <= data.r2 => (r, r),
            Some(_) => return Err(ErrorKind::Value),
            None => (data.r1, data.r2),
        }
    } else if sp.all {
        (t.range.r1, t.range.r2)
    } else {
        let mut lo = u32::MAX;
        let mut hi = 0;
        let mut add = |a: u32, b: u32| {
            lo = lo.min(a);
            hi = hi.max(b);
        };
        let data_rows = sp.data || !(sp.headers || sp.totals);
        if sp.headers {
            let r = t.header_row().ok_or(ErrorKind::Ref)?;
            add(r, r);
        }
        if data_rows {
            add(data.r1, data.r2);
        }
        if sp.totals {
            let r = t.totals_row().ok_or(ErrorKind::Ref)?;
            add(r, r);
        }
        (lo, hi)
    };
    Ok((s, Range::new(r1, c1, r2, c2)))
}

// ---------------------------------------------------------------------------
// Rewriting formulas (rename table / column, qualify for files)
// ---------------------------------------------------------------------------

/// Visits every name node of an expression.
pub fn visit_names(e: &mut Expr, f: &mut dyn FnMut(&mut String)) {
    match e {
        Expr::Name(n) => f(n),
        Expr::Neg(a) | Expr::Plus(a) | Expr::Percent(a) | Expr::Paren(a) => visit_names(a, f),
        Expr::Bin(_, a, b) => {
            visit_names(a, f);
            visit_names(b, f);
        }
        Expr::Func(_, args) => args.iter_mut().for_each(|a| visit_names(a, f)),
        Expr::Array(rows) => rows.iter_mut().flatten().for_each(|a| visit_names(a, f)),
        _ => {}
    }
}

/// Renames the table part of a structured reference / bare table name.
pub fn rename_table_in(text: &str, old: &str, new: &str) -> Option<String> {
    let (t, spec) = split(text);
    t.eq_ignore_ascii_case(old).then(|| format!("{new}{spec}"))
}

/// Replaces column `old` by `new` inside a structured-reference spec.
pub fn rename_column_in(spec: &str, old: &str, new: &str) -> String {
    // Walk innermost bracket groups and compare their (unescaped) content.
    let chars: Vec<char> = spec.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            // Find the matching ']' of an innermost group (no nested '[').
            let mut j = i + 1;
            let mut esc = false;
            let mut nested = false;
            while j < chars.len() {
                if esc {
                    esc = false;
                } else if chars[j] == '\'' {
                    esc = true;
                } else if chars[j] == '[' {
                    nested = true;
                    break;
                } else if chars[j] == ']' {
                    break;
                }
                j += 1;
            }
            if !nested && j < chars.len() {
                let content: String = chars[i + 1..j].iter().collect();
                let (at, body) = match content.strip_prefix('@') {
                    Some(b) => ("@", b.to_string()),
                    None => ("", content.clone()),
                };
                out.push('[');
                if !body.starts_with('#') && unescape(&body).trim().eq_ignore_ascii_case(old) {
                    out.push_str(at);
                    out.push_str(&escape_col(new));
                } else {
                    out.push_str(&content);
                }
                out.push(']');
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Rewrites a structured reference into the form stored in files: always qualified with the
/// table name and `@` spelled `[#This Row],`.
pub fn file_form(text: &str, table: Option<&str>) -> String {
    let (t, spec) = split(text);
    let t = if t.is_empty() { table.unwrap_or("") } else { t };
    if t.is_empty() {
        // Outside any table: keep the text as typed.
        return text.to_string();
    }
    let inner = unbracket(spec).trim();
    let spec = match inner.strip_prefix('@') {
        Some(rest) => {
            let rest = rest.trim();
            if rest.is_empty() {
                "[#This Row]".to_string()
            } else if rest.starts_with('[') {
                format!("[[#This Row],{rest}]")
            } else {
                format!("[[#This Row],[{rest}]]")
            }
        }
        None => spec.to_string(),
    };
    format!("{t}{spec}")
}

/// Turns the file form `Table1[[#This Row],[Col]]` into the familiar `Table1[@Col]`.
pub fn display_form(text: &str) -> String {
    let (t, spec) = split(text);
    let inner = unbracket(spec);
    let items = if inner.starts_with('[') { top_level_items(inner) } else { return text.to_string() };
    if items.first().is_some_and(|f| unbracket(f).eq_ignore_ascii_case("#This Row")) {
        let rest: Vec<&str> = items[1..].to_vec();
        return match rest.as_slice() {
            [] => format!("{t}[@]"),
            [one] if !one.contains(':') && !needs_brackets(unbracket(one)) => format!("{t}[@{}]", unbracket(one)),
            _ => format!("{t}[@{}]", rest.join(",")),
        };
    }
    text.to_string()
}

fn needs_brackets(col: &str) -> bool {
    col.chars().any(|c| matches!(c, ' ' | '\'' | '#' | '[' | ']' | ',' | ':' | '@'))
}

/// Applies `f` to every formula AST of the workbook with its (sheet, row, col).
pub fn for_each_formula(wb: &mut Workbook, f: &mut dyn FnMut(&mut Expr, usize, u32, u32) -> bool) {
    for s in 0..wb.sheets.len() {
        for (&r, row) in wb.sheets[s].rows.iter_mut() {
            for (&c, cell) in row.iter_mut() {
                if let Some(fm) = &mut cell.formula {
                    if let Some(ast) = &mut fm.ast {
                        if f(ast, s, r, c) {
                            fm.text = crate::formula::print(ast);
                        }
                    }
                }
            }
        }
    }
}

/// Renames a table and every structured reference to it.
pub fn rename_table(wb: &mut Workbook, old: &str, new: &str) {
    if let Some((s, i)) = find(wb, old) {
        wb.sheets[s].tables[i].name = new.to_string();
    }
    for_each_formula(wb, &mut |e, _, _, _| {
        let mut changed = false;
        visit_names(e, &mut |n| {
            if let Some(x) = rename_table_in(n, old, new) {
                *n = x;
                changed = true;
            }
        });
        changed
    });
    let globals = wb.names.iter_mut().map(|(_, f)| f);
    let locals: Vec<&mut String> = wb.sheets.iter_mut().flat_map(|sh| sh.names.iter_mut().map(|(_, f)| f)).collect();
    for f in globals.chain(locals) {
        if let Ok(mut e) = crate::formula::parse(f) {
            let mut changed = false;
            visit_names(&mut e, &mut |n| {
                if let Some(x) = rename_table_in(n, old, new) {
                    *n = x;
                    changed = true;
                }
            });
            if changed {
                *f = crate::formula::print(&e);
            }
        }
    }
}

/// Renames a column of table (sheet `s`, index `ti`) and the structured references to it.
pub fn rename_column(wb: &mut Workbook, s: usize, ti: usize, old: &str, new: &str) {
    let t = wb.sheets[s].tables[ti].clone();
    for_each_formula(wb, &mut |e, fs, r, c| {
        let inside = fs == s && t.range.contains(r, c);
        let mut changed = false;
        visit_names(e, &mut |n| {
            let (tn, spec) = split(n);
            let ours = if tn.is_empty() { inside && !spec.is_empty() } else { tn.eq_ignore_ascii_case(&t.name) };
            if ours && !spec.is_empty() {
                let x = format!("{tn}{}", rename_column_in(spec, old, new));
                if x != *n {
                    *n = x;
                    changed = true;
                }
            }
        });
        changed
    });
}

/// Replaces the structured references to a table by plain cell references (Convert to Range).
pub fn unstructure(wb: &mut Workbook, s: usize, ti: usize) {
    let t = wb.sheets[s].tables[ti].clone();
    let sheet_name = wb.sheets[s].name.clone();
    let snapshot = wb.clone();
    for_each_formula(wb, &mut |e, fs, r, c| {
        let mut changed = false;
        replace_names(e, &mut |n| {
            let (tn, spec) = split(n);
            let ours = if tn.is_empty() { fs == s && t.range.contains(r, c) && !spec.is_empty() } else { tn.eq_ignore_ascii_case(&t.name) };
            if !ours {
                return None;
            }
            let (_, rg) = resolve(&snapshot, n, fs, Some((r, c))).ok()?;
            changed = true;
            let abs = |r: u32, c: u32| format!("${}${}", crate::model::col_name(c), r + 1);
            let body = if rg.is_single() { abs(rg.r1, rg.c1) } else { format!("{}:{}", abs(rg.r1, rg.c1), abs(rg.r2, rg.c2)) };
            let prefix = if fs == s { String::new() } else { format!("{}!", crate::formula::quote_sheet(&sheet_name)) };
            crate::formula::parse(&format!("{prefix}{body}")).ok()
        });
        changed
    });
}

/// Replaces name nodes for which `f` returns an expression.
fn replace_names(e: &mut Expr, f: &mut dyn FnMut(&str) -> Option<Expr>) {
    match e {
        Expr::Name(n) => {
            if let Some(x) = f(n) {
                *e = x;
            }
        }
        Expr::Neg(a) | Expr::Plus(a) | Expr::Percent(a) | Expr::Paren(a) => replace_names(a, f),
        Expr::Bin(_, a, b) => {
            replace_names(a, f);
            replace_names(b, f);
        }
        Expr::Func(_, args) => args.iter_mut().for_each(|a| replace_names(a, f)),
        _ => {}
    }
}

/// Header texts of a table read from its header row (or Column1.. without a header).
pub fn sync_columns_from_cells(sh: &Sheet, t: &mut Table) {
    let n = t.range.cols() as usize;
    t.columns.resize_with(n, Default::default);
    if let Some(hr) = t.header_row() {
        for (i, col) in t.columns.iter_mut().enumerate() {
            let txt = header_text(sh.value(hr, t.range.c1 + i as u32));
            col.name = txt;
        }
    }
    t.fix_column_names();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{cell_from_input, Sheet};

    const DEFAULT_STYLE: &str = "TableStyleMedium2";

    fn book() -> Workbook {
        let mut wb = Workbook::empty_with(vec![Sheet::new("Sheet1")]);
        for (r, row) in [["Item", "Qty", "Price"], ["a", "2", "10"], ["b", "3", "20"], ["c", "5", "30"]].iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let cell = cell_from_input(&mut wb, v, 0);
                wb.sheets[0].set(r as u32 + 1, c as u32 + 1, cell);
            }
        }
        let mut t = Table::new("Sales", Range::new(1, 1, 5, 3), DEFAULT_STYLE);
        t.totals = true;
        sync_columns_from_cells(&wb.sheets[0], &mut t);
        t.columns[1].total = TotalFn::Sum;
        wb.sheets[0].tables.push(t);
        wb
    }

    #[test]
    fn structured_ranges() {
        let wb = book();
        let r = |s: &str, cell: Option<(u32, u32)>| resolve(&wb, s, 0, cell).map(|x| x.1.to_a1());
        assert_eq!(r("Sales[Qty]", None), Ok("C3:C5".into()));
        assert_eq!(r("Sales", None), Ok("B3:D5".into()));
        assert_eq!(r("Sales[]", None), Ok("B3:D5".into()));
        assert_eq!(r("Sales[#All]", None), Ok("B2:D6".into()));
        assert_eq!(r("Sales[#Headers]", None), Ok("B2:D2".into()));
        assert_eq!(r("Sales[#Totals]", None), Ok("B6:D6".into()));
        assert_eq!(r("Sales[[#Headers],[Price]]", None), Ok("D2".into()));
        assert_eq!(r("Sales[[#Headers],[#Data],[Qty]]", None), Ok("C2:C5".into()));
        assert_eq!(r("Sales[[Qty]:[Price]]", None), Ok("C3:D5".into()));
        assert_eq!(r("[@Price]", Some((3, 3))), Ok("D4".into()));
        assert_eq!(r("Sales[[#This Row],[Qty]]", Some((4, 5))), Ok("C5".into()));
        assert_eq!(r("[Qty]", Some((5, 2))), Ok("C3:C5".into()));
        assert!(r("Sales[Nope]", None).is_err());
        assert!(r("[@Price]", Some((1, 4))).is_err());
    }

    #[test]
    fn rewriting() {
        assert_eq!(rename_column_in("[[#Headers],[Qty]]", "qty", "Units"), "[[#Headers],[Units]]");
        assert_eq!(rename_column_in("[@Qty]", "Qty", "Unit Count"), "[@Unit Count]");
        assert_eq!(rename_column_in("[@[Qty]]", "Qty", "U"), "[@[U]]");
        assert_eq!(file_form("[@Qty]", Some("Sales")), "Sales[[#This Row],[Qty]]");
        assert_eq!(file_form("Sales[@[Unit Price]]", None), "Sales[[#This Row],[Unit Price]]");
        assert_eq!(display_form("Sales[[#This Row],[Qty]]"), "Sales[@Qty]");
        assert_eq!(display_form("Sales[[#This Row],[Unit Price]]"), "Sales[@[Unit Price]]");
        assert_eq!(style_parts("TableStyleMedium2"), Some((1, 1)));
        assert_eq!(style_name(1, 1), "TableStyleMedium2");
    }

    #[test]
    fn rename_updates_formulas() {
        let mut wb = book();
        let f = cell_from_input(&mut wb, "=SUM(Sales[Qty])+Sales[[#Totals],[Qty]]", 0);
        wb.sheets[0].set(10, 0, f);
        let f = cell_from_input(&mut wb, "=[@Qty]*[@Price]", 0);
        wb.sheets[0].set(2, 3, f);
        rename_column(&mut wb, 0, 0, "Qty", "Units");
        rename_table(&mut wb, "Sales", "Orders");
        assert_eq!(wb.sheets[0].get(10, 0).unwrap().formula.as_ref().unwrap().text, "SUM(Orders[Units])+Orders[[#Totals],[Units]]");
        assert_eq!(wb.sheets[0].get(2, 3).unwrap().formula.as_ref().unwrap().text, "[@Units]*[@Price]");
    }
}
