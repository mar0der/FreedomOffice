//! Formula lexer, parser, printer and reference transforms.

use crate::model::{col_name, parse_col, ErrorKind, MAX_COLS, MAX_ROWS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    Cell,
    Area,
    Cols,
    Rows,
    /// Spill-range operator on an anchor cell: `A1#`.
    Spill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellPart {
    pub row: u32,
    pub col: u32,
    pub row_abs: bool,
    pub col_abs: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RefExpr {
    pub sheet: Option<String>,
    pub kind: RefKind,
    pub a: CellPart,
    pub b: CellPart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    pub fn as_str(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Pow => "^",
            BinOp::Concat => "&",
            BinOp::Eq => "=",
            BinOp::Ne => "<>",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(f64),
    Str(String),
    Bool(bool),
    Err(ErrorKind),
    Ref(RefExpr),
    Name(String),
    Neg(Box<Expr>),
    Plus(Box<Expr>),
    Percent(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Func(String, Vec<Expr>),
    Array(Vec<Vec<Expr>>),
    Paren(Box<Expr>),
    /// Omitted argument, e.g. the middle of `IF(A1,,2)`.
    Missing,
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Num(f64, String),
    Str(String),
    Err(ErrorKind),
    Ident(String),
    Sheet(String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
    Colon,
    Semi,
    LBrace,
    RBrace,
}

/// A token with its byte span in the source (used by the editor for colouring).
#[derive(Clone, Debug)]
pub struct Spanned {
    pub tok: Tok,
    pub start: usize,
    pub end: usize,
}

pub fn lex(src: &str) -> Result<Vec<Spanned>, String> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let n = chars.len();
    let at = |i: usize| chars.get(i).map(|c| c.1).unwrap_or('\0');
    let pos = |i: usize| chars.get(i).map(|c| c.0).unwrap_or(src.len());
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = at(i);
        let start = pos(i);
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let tok = match c {
            '"' => {
                let mut s = String::new();
                i += 1;
                loop {
                    if i >= n {
                        return Err("unterminated string".into());
                    }
                    if at(i) == '"' {
                        if at(i + 1) == '"' {
                            s.push('"');
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    s.push(at(i));
                    i += 1;
                }
                Tok::Str(s)
            }
            '\'' => {
                let mut s = String::new();
                i += 1;
                loop {
                    if i >= n {
                        return Err("unterminated sheet name".into());
                    }
                    if at(i) == '\'' {
                        if at(i + 1) == '\'' {
                            s.push('\'');
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    s.push(at(i));
                    i += 1;
                }
                if at(i) != '!' {
                    return Err("expected ! after sheet name".into());
                }
                i += 1;
                Tok::Sheet(s)
            }
            '#' => {
                let mut matched = None;
                for e in ErrorKind::ALL {
                    let es = e.as_str();
                    if src[start..].len() >= es.len() && src[start..start + es.len()].eq_ignore_ascii_case(es) {
                        matched = Some(e);
                        break;
                    }
                }
                match matched {
                    Some(e) => {
                        i += e.as_str().chars().count();
                        Tok::Err(e)
                    }
                    // `A1#`: spill-range operator directly after a cell reference.
                    None if out.last().is_some_and(|t: &Spanned| t.end == start && matches!(t.tok, Tok::Ident(_))) => {
                        i += 1;
                        Tok::Op("#")
                    }
                    None => return Err("unknown error literal".into()),
                }
            }
            '0'..='9' | '.' if c != '.' || at(i + 1).is_ascii_digit() => {
                let s0 = i;
                while at(i).is_ascii_digit() || at(i) == '.' {
                    i += 1;
                }
                if (at(i) == 'e' || at(i) == 'E')
                    && (at(i + 1).is_ascii_digit() || (matches!(at(i + 1), '+' | '-') && at(i + 2).is_ascii_digit()))
                {
                    i += 2;
                    while at(i).is_ascii_digit() {
                        i += 1;
                    }
                }
                let text = &src[pos(s0)..pos(i)];
                let v: f64 = text.parse().map_err(|_| format!("bad number {text}"))?;
                Tok::Num(v, text.to_string())
            }
            c if c.is_alphabetic() || c == '_' || c == '$' || c == '\\' => {
                let s0 = i;
                while {
                    let d = at(i);
                    d.is_alphanumeric() || matches!(d, '_' | '.' | '$' | '\\')
                } {
                    i += 1;
                }
                let text = src[pos(s0)..pos(i)].to_string();
                if at(i) == '!' {
                    i += 1;
                    Tok::Sheet(text)
                } else {
                    Tok::Ident(text)
                }
            }
            '+' | '-' | '*' | '/' | '^' | '&' | '%' | '=' | '@' => {
                i += 1;
                Tok::Op(match c {
                    '@' => "@",
                    '+' => "+",
                    '-' => "-",
                    '*' => "*",
                    '/' => "/",
                    '^' => "^",
                    '&' => "&",
                    '%' => "%",
                    _ => "=",
                })
            }
            '<' => {
                i += 1;
                if at(i) == '=' {
                    i += 1;
                    Tok::Op("<=")
                } else if at(i) == '>' {
                    i += 1;
                    Tok::Op("<>")
                } else {
                    Tok::Op("<")
                }
            }
            '>' => {
                i += 1;
                if at(i) == '=' {
                    i += 1;
                    Tok::Op(">=")
                } else {
                    Tok::Op(">")
                }
            }
            '(' => {
                i += 1;
                Tok::LParen
            }
            ')' => {
                i += 1;
                Tok::RParen
            }
            ',' => {
                i += 1;
                Tok::Comma
            }
            ':' => {
                i += 1;
                Tok::Colon
            }
            ';' => {
                i += 1;
                Tok::Semi
            }
            '{' => {
                i += 1;
                Tok::LBrace
            }
            '}' => {
                i += 1;
                Tok::RBrace
            }
            _ => return Err(format!("unexpected character '{c}'")),
        };
        out.push(Spanned { tok, start, end: pos(i) });
    }
    Ok(out)
}

/// Parses "A1" / "$A$1" (with optional $ markers).
pub fn parse_cell_part(s: &str) -> Option<CellPart> {
    let b = s.as_bytes();
    let mut i = 0;
    let col_abs = b.first() == Some(&b'$');
    if col_abs {
        i += 1;
    }
    let cs = i;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        i += 1;
    }
    let col = parse_col(&s[cs..i])?;
    let row_abs = b.get(i) == Some(&b'$');
    if row_abs {
        i += 1;
    }
    let rs = i;
    if rs >= b.len() || !s[rs..].bytes().all(|c| c.is_ascii_digit()) || b[rs] == b'0' {
        return None;
    }
    let row: u32 = s[rs..].parse().ok()?;
    if row > MAX_ROWS {
        return None;
    }
    Some(CellPart { row: row - 1, col, row_abs, col_abs })
}

fn parse_col_part(s: &str) -> Option<(u32, bool)> {
    let abs = s.starts_with('$');
    let col = parse_col(s.trim_start_matches('$'))?;
    Some((col, abs))
}

fn parse_row_part(s: &str) -> Option<(u32, bool)> {
    let abs = s.starts_with('$');
    let t = s.trim_start_matches('$');
    if t.is_empty() || !t.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let r: u32 = t.parse().ok()?;
    if r == 0 || r > MAX_ROWS {
        return None;
    }
    Some((r - 1, abs))
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser {
    toks: Vec<Tok>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i)
    }
    fn peek_at(&self, k: usize) -> Option<&Tok> {
        self.toks.get(self.i + k)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.i).cloned();
        self.i += 1;
        t
    }
    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(o)) if *o == op)
    }

    fn expr(&mut self) -> Result<Expr, String> {
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Expr, String> {
        let mut l = self.concat()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op("=")) => BinOp::Eq,
                Some(Tok::Op("<>")) => BinOp::Ne,
                Some(Tok::Op("<")) => BinOp::Lt,
                Some(Tok::Op("<=")) => BinOp::Le,
                Some(Tok::Op(">")) => BinOp::Gt,
                Some(Tok::Op(">=")) => BinOp::Ge,
                _ => return Ok(l),
            };
            self.i += 1;
            let r = self.concat()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }

    fn concat(&mut self) -> Result<Expr, String> {
        let mut l = self.additive()?;
        while self.is_op("&") {
            self.i += 1;
            let r = self.additive()?;
            l = Expr::Bin(BinOp::Concat, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn additive(&mut self) -> Result<Expr, String> {
        let mut l = self.multiplicative()?;
        loop {
            let op = if self.is_op("+") {
                BinOp::Add
            } else if self.is_op("-") {
                BinOp::Sub
            } else {
                return Ok(l);
            };
            self.i += 1;
            let r = self.multiplicative()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }

    fn multiplicative(&mut self) -> Result<Expr, String> {
        let mut l = self.power()?;
        loop {
            let op = if self.is_op("*") {
                BinOp::Mul
            } else if self.is_op("/") {
                BinOp::Div
            } else {
                return Ok(l);
            };
            self.i += 1;
            let r = self.power()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }

    fn power(&mut self) -> Result<Expr, String> {
        let mut l = self.unary()?;
        while self.is_op("^") {
            self.i += 1;
            let r = self.unary()?;
            l = Expr::Bin(BinOp::Pow, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.is_op("-") {
            self.i += 1;
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.is_op("+") {
            self.i += 1;
            return Ok(Expr::Plus(Box::new(self.unary()?)));
        }
        if self.is_op("@") {
            // Implicit intersection operator.
            self.i += 1;
            return Ok(Expr::Func("SINGLE".into(), vec![self.unary()?]));
        }
        let mut e = self.primary()?;
        while self.is_op("%") {
            self.i += 1;
            e = Expr::Percent(Box::new(e));
        }
        Ok(e)
    }

    fn reference(&mut self, sheet: Option<String>) -> Result<Expr, String> {
        // cell or area
        match self.peek().cloned() {
            Some(Tok::Ident(id)) => {
                if let Some(a) = parse_cell_part(&id) {
                    self.i += 1;
                    if self.peek() == Some(&Tok::Colon) {
                        if let Some(Tok::Ident(id2)) = self.peek_at(1).cloned() {
                            if let Some(b) = parse_cell_part(&id2) {
                                self.i += 2;
                                return Ok(Expr::Ref(RefExpr { sheet, kind: RefKind::Area, a, b }));
                            }
                        }
                    }
                    if self.is_op("#") {
                        self.i += 1;
                        return Ok(Expr::Ref(RefExpr { sheet, kind: RefKind::Spill, a, b: a }));
                    }
                    return Ok(Expr::Ref(RefExpr { sheet, kind: RefKind::Cell, a, b: a }));
                }
                if let Some((c1, a1)) = parse_col_part(&id) {
                    if self.peek_at(1) == Some(&Tok::Colon) {
                        if let Some(Tok::Ident(id2)) = self.peek_at(2).cloned() {
                            if let Some((c2, a2)) = parse_col_part(&id2) {
                                self.i += 3;
                                return Ok(Expr::Ref(RefExpr {
                                    sheet,
                                    kind: RefKind::Cols,
                                    a: CellPart { row: 0, col: c1, row_abs: false, col_abs: a1 },
                                    b: CellPart { row: MAX_ROWS - 1, col: c2, row_abs: false, col_abs: a2 },
                                }));
                            }
                        }
                    }
                }
                // $1:$3 style rows lex as identifiers when prefixed with '$'
                if let Some((r1, a1)) = parse_row_part(&id) {
                    if self.peek_at(1) == Some(&Tok::Colon) {
                        let second = match self.peek_at(2).cloned() {
                            Some(Tok::Ident(s)) => parse_row_part(&s),
                            Some(Tok::Num(_, s)) => parse_row_part(&s),
                            _ => None,
                        };
                        if let Some((r2, a2)) = second {
                            self.i += 3;
                            return Ok(rows_ref(sheet, r1, a1, r2, a2));
                        }
                    }
                }
                if sheet.is_some() {
                    return Err(format!("bad reference {id}"));
                }
                Err("not a reference".into())
            }
            Some(Tok::Num(_, s)) => {
                if let Some((r1, a1)) = parse_row_part(&s) {
                    if self.peek_at(1) == Some(&Tok::Colon) {
                        let second = match self.peek_at(2).cloned() {
                            Some(Tok::Ident(s2)) => parse_row_part(&s2),
                            Some(Tok::Num(_, s2)) => parse_row_part(&s2),
                            _ => None,
                        };
                        if let Some((r2, a2)) = second {
                            self.i += 3;
                            return Ok(rows_ref(sheet, r1, a1, r2, a2));
                        }
                    }
                }
                Err("not a reference".into())
            }
            Some(Tok::Err(ErrorKind::Ref)) if sheet.is_some() => {
                self.i += 1;
                Ok(Expr::Err(ErrorKind::Ref))
            }
            _ => Err("expected reference".into()),
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        let t = self.peek().cloned().ok_or("unexpected end of formula")?;
        match t {
            Tok::Num(v, _) => {
                if let Ok(r) = self.try_ref(None) {
                    return Ok(r);
                }
                self.i += 1;
                Ok(Expr::Num(v))
            }
            Tok::Str(s) => {
                self.i += 1;
                Ok(Expr::Str(s))
            }
            Tok::Err(e) => {
                self.i += 1;
                Ok(Expr::Err(e))
            }
            Tok::Sheet(name) => {
                self.i += 1;
                self.reference(Some(name))
            }
            Tok::LParen => {
                self.i += 1;
                let e = self.expr()?;
                if self.next() != Some(Tok::RParen) {
                    return Err("missing )".into());
                }
                Ok(Expr::Paren(Box::new(e)))
            }
            Tok::LBrace => {
                self.i += 1;
                let mut rows = vec![vec![]];
                loop {
                    let neg = if self.is_op("-") {
                        self.i += 1;
                        true
                    } else {
                        false
                    };
                    let v = match self.next() {
                        Some(Tok::Num(v, _)) => Expr::Num(if neg { -v } else { v }),
                        Some(Tok::Str(s)) => Expr::Str(s),
                        Some(Tok::Err(e)) => Expr::Err(e),
                        Some(Tok::Ident(s)) if s.eq_ignore_ascii_case("TRUE") => Expr::Bool(true),
                        Some(Tok::Ident(s)) if s.eq_ignore_ascii_case("FALSE") => Expr::Bool(false),
                        _ => return Err("bad array constant".into()),
                    };
                    rows.last_mut().unwrap().push(v);
                    match self.next() {
                        Some(Tok::Comma) => {}
                        Some(Tok::Semi) => rows.push(vec![]),
                        Some(Tok::RBrace) => break,
                        _ => return Err("bad array constant".into()),
                    }
                }
                Ok(Expr::Array(rows))
            }
            Tok::Ident(id) => {
                if self.peek_at(1) == Some(&Tok::LParen) {
                    self.i += 2;
                    let mut name = id.to_ascii_uppercase();
                    for p in ["_XLFN._XLWS.", "_XLFN.", "_XLWS."] {
                        if let Some(rest) = name.strip_prefix(p) {
                            name = rest.to_string();
                        }
                    }
                    let mut args = Vec::new();
                    if self.peek() == Some(&Tok::RParen) {
                        self.i += 1;
                        return Ok(Expr::Func(name, args));
                    }
                    loop {
                        if matches!(self.peek(), Some(Tok::Comma) | Some(Tok::RParen)) {
                            args.push(Expr::Missing);
                        } else {
                            args.push(self.expr()?);
                        }
                        match self.next() {
                            Some(Tok::Comma) => continue,
                            Some(Tok::RParen) => break,
                            _ => return Err(format!("missing ) in {name}")),
                        }
                    }
                    if let (true, [Expr::Ref(r)]) = (name == "ANCHORARRAY", args.as_slice()) {
                        if r.kind == RefKind::Cell {
                            return Ok(Expr::Ref(RefExpr { kind: RefKind::Spill, ..r.clone() }));
                        }
                    }
                    return Ok(Expr::Func(name, args));
                }
                if let Ok(r) = self.try_ref(None) {
                    return Ok(r);
                }
                self.i += 1;
                if id.eq_ignore_ascii_case("TRUE") {
                    return Ok(Expr::Bool(true));
                }
                if id.eq_ignore_ascii_case("FALSE") {
                    return Ok(Expr::Bool(false));
                }
                Ok(Expr::Name(id))
            }
            Tok::Op(o) => Err(format!("unexpected operator {o}")),
            other => Err(format!("unexpected {other:?}")),
        }
    }

    fn try_ref(&mut self, sheet: Option<String>) -> Result<Expr, String> {
        let save = self.i;
        match self.reference(sheet) {
            Ok(e) => Ok(e),
            Err(e) => {
                self.i = save;
                Err(e)
            }
        }
    }
}

fn rows_ref(sheet: Option<String>, r1: u32, a1: bool, r2: u32, a2: bool) -> Expr {
    Expr::Ref(RefExpr {
        sheet,
        kind: RefKind::Rows,
        a: CellPart { row: r1, col: 0, row_abs: a1, col_abs: false },
        b: CellPart { row: r2, col: MAX_COLS - 1, row_abs: a2, col_abs: false },
    })
}

/// Parses formula text (without the leading '=').
pub fn parse(src: &str) -> Result<Expr, String> {
    let toks: Vec<Tok> = lex(src)?.into_iter().map(|s| s.tok).collect();
    if toks.is_empty() {
        return Err("empty formula".into());
    }
    let mut p = Parser { toks, i: 0 };
    let e = p.expr()?;
    if p.i < p.toks.len() {
        return Err(format!("unexpected {:?}", p.toks[p.i]));
    }
    Ok(e)
}

// ---------------------------------------------------------------------------
// Printer
// ---------------------------------------------------------------------------

pub fn quote_sheet(name: &str) -> String {
    let simple = !name.is_empty()
        && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        && !name.chars().next().unwrap().is_ascii_digit()
        && parse_cell_part(name).is_none();
    if simple { name.to_string() } else { format!("'{}'", name.replace('\'', "''")) }
}

fn part_str(p: &CellPart) -> String {
    format!("{}{}{}{}", if p.col_abs { "$" } else { "" }, col_name(p.col), if p.row_abs { "$" } else { "" }, p.row + 1)
}

pub fn ref_str(r: &RefExpr) -> String {
    let prefix = r.sheet.as_ref().map(|s| format!("{}!", quote_sheet(s))).unwrap_or_default();
    let body = match r.kind {
        RefKind::Cell => part_str(&r.a),
        RefKind::Spill => format!("{}#", part_str(&r.a)),
        RefKind::Area => format!("{}:{}", part_str(&r.a), part_str(&r.b)),
        RefKind::Cols => format!(
            "{}{}:{}{}",
            if r.a.col_abs { "$" } else { "" },
            col_name(r.a.col),
            if r.b.col_abs { "$" } else { "" },
            col_name(r.b.col)
        ),
        RefKind::Rows => format!(
            "{}{}:{}{}",
            if r.a.row_abs { "$" } else { "" },
            r.a.row + 1,
            if r.b.row_abs { "$" } else { "" },
            r.b.row + 1
        ),
    };
    format!("{prefix}{body}")
}

pub fn num_str(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 { format!("{}", v as i64) } else { format!("{v}") }
}

pub fn print(e: &Expr) -> String {
    let mut s = String::new();
    write_expr(e, &mut s, &mut None);
    s
}

/// Prints a formula as stored in .xlsx files: `A1#` as `ANCHORARRAY(A1)`, `@x` as
/// `SINGLE(x)` and LET parameters with the `_xlpm.` prefix.
pub fn print_xlsx(e: &Expr) -> String {
    let mut s = String::new();
    write_expr(e, &mut s, &mut Some(Vec::new()));
    s
}

/// `xlsx` holds the LET parameter names in scope when printing for files.
fn write_expr(e: &Expr, out: &mut String, xlsx: &mut Option<Vec<String>>) {
    match e {
        Expr::Num(v) => out.push_str(&num_str(*v)),
        Expr::Str(s) => {
            out.push('"');
            out.push_str(&s.replace('"', "\"\""));
            out.push('"');
        }
        Expr::Bool(b) => out.push_str(if *b { "TRUE" } else { "FALSE" }),
        Expr::Err(e) => out.push_str(e.as_str()),
        Expr::Ref(r) if r.kind == RefKind::Spill && xlsx.is_some() => {
            let cell = RefExpr { kind: RefKind::Cell, ..r.clone() };
            out.push_str(&format!("ANCHORARRAY({})", ref_str(&cell)));
        }
        Expr::Ref(r) => out.push_str(&ref_str(r)),
        Expr::Name(n) => {
            if xlsx.as_ref().is_some_and(|lets| lets.iter().any(|l| l.eq_ignore_ascii_case(n))) {
                out.push_str("_xlpm.");
            }
            out.push_str(n)
        }
        Expr::Neg(e) => {
            out.push('-');
            write_expr(e, out, xlsx);
        }
        Expr::Plus(e) => {
            out.push('+');
            write_expr(e, out, xlsx);
        }
        Expr::Percent(e) => {
            write_expr(e, out, xlsx);
            out.push('%');
        }
        Expr::Bin(op, a, b) => {
            write_expr(a, out, xlsx);
            out.push_str(op.as_str());
            write_expr(b, out, xlsx);
        }
        Expr::Func(name, args) if name == "SINGLE" && args.len() == 1 && xlsx.is_none() => {
            out.push('@');
            write_expr(&args[0], out, xlsx);
        }
        Expr::Func(name, args) => {
            let lets = match xlsx {
                Some(l) if name == "LET" => {
                    let names: Vec<String> = args.iter().step_by(2).filter_map(|a| if let Expr::Name(n) = a { Some(n.clone()) } else { None }).collect();
                    let k = names.len();
                    l.extend(names);
                    k
                }
                _ => 0,
            };
            out.push_str(name);
            out.push('(');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_expr(a, out, xlsx);
            }
            out.push(')');
            if let Some(l) = xlsx {
                l.truncate(l.len() - lets);
            }
        }
        Expr::Array(rows) => {
            out.push('{');
            for (ri, row) in rows.iter().enumerate() {
                if ri > 0 {
                    out.push(';');
                }
                for (ci, v) in row.iter().enumerate() {
                    if ci > 0 {
                        out.push(',');
                    }
                    write_expr(v, out, xlsx);
                }
            }
            out.push('}');
        }
        Expr::Paren(e) => {
            out.push('(');
            write_expr(e, out, xlsx);
            out.push(')');
        }
        Expr::Missing => {}
    }
}

// ---------------------------------------------------------------------------
// Transforms
// ---------------------------------------------------------------------------

/// Visits every reference; the callback returns false to turn it into #REF!.
pub fn map_refs(e: &mut Expr, f: &mut dyn FnMut(&mut RefExpr) -> bool) {
    match e {
        Expr::Ref(r) => {
            if !f(r) {
                *e = Expr::Err(ErrorKind::Ref);
            }
        }
        Expr::Neg(a) | Expr::Plus(a) | Expr::Percent(a) | Expr::Paren(a) => map_refs(a, f),
        Expr::Bin(_, a, b) => {
            map_refs(a, f);
            map_refs(b, f);
        }
        Expr::Func(_, args) => args.iter_mut().for_each(|a| map_refs(a, f)),
        _ => {}
    }
}

/// Adjusts relative references when a formula is copied by (dr, dc).
pub fn shift_relative(e: &mut Expr, dr: i64, dc: i64) {
    map_refs(e, &mut |r| {
        let mut ok = true;
        let mut shift = |p: &mut CellPart, rows: bool, cols: bool| {
            if rows && !p.row_abs {
                let n = p.row as i64 + dr;
                if n < 0 || n >= MAX_ROWS as i64 {
                    ok = false;
                } else {
                    p.row = n as u32;
                }
            }
            if cols && !p.col_abs {
                let n = p.col as i64 + dc;
                if n < 0 || n >= MAX_COLS as i64 {
                    ok = false;
                } else {
                    p.col = n as u32;
                }
            }
        };
        let (rows, cols) = match r.kind {
            RefKind::Cols => (false, true),
            RefKind::Rows => (true, false),
            _ => (true, true),
        };
        shift(&mut r.a, rows, cols);
        shift(&mut r.b, rows, cols);
        ok
    });
}

/// Shifts references after rows (axis_rows=true) or columns are inserted (n>0) or deleted (n<0)
/// at index `at` on the target sheet. `same_sheet` says whether unqualified refs point at it.
pub fn shift_for_structure(e: &mut Expr, target: &str, same_sheet: bool, axis_rows: bool, at: u32, n: i64) {
    map_refs(e, &mut |r| {
        let hits = match &r.sheet {
            Some(s) => s.eq_ignore_ascii_case(target),
            None => same_sheet,
        };
        if !hits {
            return true;
        }
        if axis_rows && r.kind == RefKind::Cols || !axis_rows && r.kind == RefKind::Rows {
            return true;
        }
        let get = |p: &CellPart| if axis_rows { p.row } else { p.col };
        let (mut lo, mut hi) = (get(&r.a) as i64, get(&r.b) as i64);
        let at = at as i64;
        let max = if axis_rows { MAX_ROWS } else { MAX_COLS } as i64;
        if n > 0 {
            if lo >= at {
                lo += n;
            }
            if hi >= at {
                hi = (hi + n).min(max - 1);
            }
            if lo >= max {
                return false;
            }
        } else {
            let del_end = at - n; // exclusive
            let adj = |x: i64| if x >= del_end { x + n } else if x >= at { at } else { x };
            if lo >= at && hi < del_end {
                return false; // fully deleted
            }
            let new_lo = adj(lo);
            let new_hi = if hi >= del_end { hi + n } else if hi >= at { at - 1 } else { hi };
            lo = new_lo;
            hi = new_hi;
            if hi < lo {
                return false;
            }
        }
        if axis_rows {
            r.a.row = lo as u32;
            r.b.row = hi as u32;
        } else {
            r.a.col = lo as u32;
            r.b.col = hi as u32;
        }
        true
    });
}

/// Renames sheet references.
pub fn rename_sheet(e: &mut Expr, old: &str, new: &str) {
    map_refs(e, &mut |r| {
        if let Some(s) = &mut r.sheet {
            if s.eq_ignore_ascii_case(old) {
                *s = new.to_string();
            }
        }
        true
    });
}

/// Moves references that point into `src` (on the same sheet) by (dr, dc) — used by cut & paste.
pub fn move_refs(e: &mut Expr, target: &str, same_sheet: bool, src: crate::model::Range, dr: i64, dc: i64) {
    map_refs(e, &mut |r| {
        let hits = match &r.sheet {
            Some(s) => s.eq_ignore_ascii_case(target),
            None => same_sheet,
        };
        if !hits || matches!(r.kind, RefKind::Cols | RefKind::Rows) {
            return true;
        }
        if src.contains(r.a.row, r.a.col) && src.contains(r.b.row, r.b.col) {
            for p in [&mut r.a, &mut r.b] {
                p.row = (p.row as i64 + dr).clamp(0, MAX_ROWS as i64 - 1) as u32;
                p.col = (p.col as i64 + dc).clamp(0, MAX_COLS as i64 - 1) as u32;
            }
        }
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(s: &str) -> String {
        print(&parse(s).unwrap())
    }

    #[test]
    fn roundtrip() {
        assert_eq!(rt("SUM(A1:B2)+1"), "SUM(A1:B2)+1");
        assert_eq!(rt("'My Sheet'!$A$1*2"), "'My Sheet'!$A$1*2");
        assert_eq!(rt("Sheet2!A:A"), "Sheet2!A:A");
        assert_eq!(rt("SUM(3:5)"), "SUM(3:5)");
        assert_eq!(rt("IF(A1>=10,\"big\",\"small\")"), "IF(A1>=10,\"big\",\"small\")");
        assert_eq!(rt("-2^2"), "-2^2");
        assert_eq!(rt("50%"), "50%");
        assert_eq!(rt("{1,2;3,4}"), "{1,2;3,4}");
        assert_eq!(rt("_xlfn.XLOOKUP(A1,B:B,C:C)"), "XLOOKUP(A1,B:B,C:C)");
        assert_eq!(rt("LOG10(100)"), "LOG10(100)");
        assert_eq!(rt("IF(A1,,2)"), "IF(A1,,2)");
        assert_eq!(rt("myName*2"), "myName*2");
        assert_eq!(rt("SUM(A1#)+Sheet2!$B$3#"), "SUM(A1#)+Sheet2!$B$3#");
        assert_eq!(rt("@A1:A10*2"), "@A1:A10*2");
        assert_eq!(rt("_xlfn.SINGLE(A1:A3)"), "@A1:A3");
        assert!(parse("#").is_err());
        assert_eq!(rt("_xlfn.ANCHORARRAY(B2)*2"), "B2#*2");
        let e = parse("SUM(A1#)+@B1:B3+LET(x,2,x*A1)").unwrap();
        assert_eq!(print_xlsx(&e), "SUM(ANCHORARRAY(A1))+SINGLE(B1:B3)+LET(_xlpm.x,2,_xlpm.x*A1)");
    }

    #[test]
    fn shifting() {
        let mut e = parse("A1+$B$2+C$3+SUM(D1:D4)").unwrap();
        shift_relative(&mut e, 1, 1);
        assert_eq!(print(&e), "B2+$B$2+D$3+SUM(E2:E5)");
        let mut e = parse("A1").unwrap();
        shift_relative(&mut e, -1, 0);
        assert_eq!(print(&e), "#REF!");
    }

    #[test]
    fn structure() {
        let mut e = parse("SUM(A1:A10)+B5").unwrap();
        shift_for_structure(&mut e, "Sheet1", true, true, 2, 3);
        assert_eq!(print(&e), "SUM(A1:A13)+B8");
        let mut e = parse("SUM(A1:A10)+B5").unwrap();
        shift_for_structure(&mut e, "Sheet1", true, true, 4, -1);
        assert_eq!(print(&e), "SUM(A1:A9)+#REF!");
        let mut e = parse("SUM(A1:D1)").unwrap();
        shift_for_structure(&mut e, "Sheet1", true, false, 0, 1);
        assert_eq!(print(&e), "SUM(B1:E1)");
    }
}
