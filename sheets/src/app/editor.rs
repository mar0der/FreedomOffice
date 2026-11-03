//! In-cell / formula-bar text editor state (shared by both views).

use crate::formula::{self, Tok};
use crate::model::{col_name, Range};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditMode {
    /// Started by typing: arrow keys commit and move (or point in formulas).
    Enter,
    /// Started with F2 / double-click: arrow keys move the caret.
    Edit,
}

#[derive(Clone, Debug)]
pub struct PointRef {
    pub start: usize,
    pub end: usize,
    pub anchor: (u32, u32),
    pub cur: (u32, u32),
}

#[derive(Clone, Debug)]
pub struct Editor {
    pub sheet: usize,
    pub row: u32,
    pub col: u32,
    pub text: String,
    pub cursor: usize,
    pub anchor: usize,
    pub mode: EditMode,
    pub in_bar: bool,
    pub point: Option<PointRef>,
    pub ac_index: usize,
    pub ac_hidden: bool,
    /// Sheet being pointed at when it differs from the edited cell's sheet.
    pub point_sheet: Option<usize>,
}

fn prev_boundary(s: &str, i: usize) -> usize {
    s[..i].char_indices().next_back().map(|(p, _)| p).unwrap_or(0)
}

fn next_boundary(s: &str, i: usize) -> usize {
    s[i..].chars().next().map(|c| i + c.len_utf8()).unwrap_or(s.len())
}

impl Editor {
    pub fn new(sheet: usize, row: u32, col: u32, text: String, mode: EditMode) -> Self {
        let n = text.len();
        Editor {
            sheet,
            row,
            col,
            text,
            cursor: n,
            anchor: n,
            mode,
            in_bar: false,
            point: None,
            ac_index: 0,
            ac_hidden: false,
            point_sheet: None,
        }
    }

    pub fn is_formula(&self) -> bool {
        self.text.starts_with('=')
    }

    pub fn selection(&self) -> (usize, usize) {
        (self.cursor.min(self.anchor), self.cursor.max(self.anchor))
    }

    pub fn has_selection(&self) -> bool {
        self.cursor != self.anchor
    }

    pub fn selected_text(&self) -> &str {
        let (a, b) = self.selection();
        &self.text[a..b]
    }

    fn delete_selection(&mut self) {
        let (a, b) = self.selection();
        if a != b {
            self.text.replace_range(a..b, "");
            self.cursor = a;
            self.anchor = a;
        }
    }

    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.anchor = self.cursor;
        self.point = None;
        self.ac_hidden = false;
        self.ac_index = 0;
    }

    pub fn backspace(&mut self) {
        self.point = None;
        if self.has_selection() {
            self.delete_selection();
        } else if self.cursor > 0 {
            let p = prev_boundary(&self.text, self.cursor);
            self.text.replace_range(p..self.cursor, "");
            self.cursor = p;
            self.anchor = p;
        }
    }

    pub fn delete(&mut self) {
        self.point = None;
        if self.has_selection() {
            self.delete_selection();
        } else if self.cursor < self.text.len() {
            let n = next_boundary(&self.text, self.cursor);
            self.text.replace_range(self.cursor..n, "");
        }
    }

    fn word_left(&self, i: usize) -> usize {
        let mut i = i;
        while i > 0 && self.text[..i].ends_with(|c: char| !c.is_alphanumeric()) {
            i = prev_boundary(&self.text, i);
        }
        while i > 0 && self.text[..i].ends_with(|c: char| c.is_alphanumeric()) {
            i = prev_boundary(&self.text, i);
        }
        i
    }

    fn word_right(&self, i: usize) -> usize {
        let mut i = i;
        while i < self.text.len() && self.text[i..].starts_with(|c: char| c.is_alphanumeric()) {
            i = next_boundary(&self.text, i);
        }
        while i < self.text.len() && self.text[i..].starts_with(|c: char| !c.is_alphanumeric()) {
            i = next_boundary(&self.text, i);
        }
        i
    }

    pub fn move_to(&mut self, pos: usize, extend: bool) {
        self.cursor = pos.min(self.text.len());
        if !extend {
            self.anchor = self.cursor;
        }
        self.point = None;
    }

    pub fn left(&mut self, extend: bool, word: bool) {
        if !extend && self.has_selection() {
            let (a, _) = self.selection();
            return self.move_to(a, false);
        }
        let p = if word { self.word_left(self.cursor) } else { prev_boundary(&self.text, self.cursor) };
        self.move_to(p, extend);
    }

    pub fn right(&mut self, extend: bool, word: bool) {
        if !extend && self.has_selection() {
            let (_, b) = self.selection();
            return self.move_to(b, false);
        }
        let p = if word { self.word_right(self.cursor) } else { next_boundary(&self.text, self.cursor) };
        self.move_to(p, extend);
    }

    pub fn home(&mut self, extend: bool) {
        let line_start = self.text[..self.cursor].rfind('\n').map(|i| i + 1).unwrap_or(0);
        self.move_to(line_start, extend);
    }

    pub fn end(&mut self, extend: bool) {
        let line_end = self.text[self.cursor..].find('\n').map(|i| self.cursor + i).unwrap_or(self.text.len());
        self.move_to(line_end, extend);
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.text.len();
    }

    /// True if a cell reference can be inserted at the caret by pointing.
    pub fn can_point(&self) -> bool {
        if !self.is_formula() || self.has_selection() {
            return false;
        }
        if let Some(p) = &self.point {
            if p.end == self.cursor {
                return true;
            }
        }
        let before = self.text[..self.cursor].trim_end();
        match before.chars().last() {
            Some(c) => "=(,+-*/^&<>:;".contains(c),
            None => false,
        }
    }

    /// Inserts or replaces the pointed reference.
    pub fn set_point(&mut self, anchor: (u32, u32), cur: (u32, u32), sheet_prefix: Option<&str>) {
        let rg = Range::new(anchor.0, anchor.1, cur.0, cur.1);
        let mut s = String::new();
        if let Some(p) = sheet_prefix {
            s.push_str(&formula::quote_sheet(p));
            s.push('!');
        }
        s.push_str(&rg.to_a1());
        let (start, end) = match &self.point {
            Some(p) if p.end == self.cursor => (p.start, p.end),
            _ => (self.cursor, self.cursor),
        };
        self.text.replace_range(start..end, &s);
        self.cursor = start + s.len();
        self.anchor = self.cursor;
        self.point = Some(PointRef { start, end: self.cursor, anchor, cur });
        self.ac_hidden = true;
    }

    /// Identifier being typed right before the caret (for function autocomplete).
    pub fn func_prefix(&self) -> Option<(usize, String)> {
        if !self.is_formula() || self.has_selection() || self.ac_hidden {
            return None;
        }
        let before = &self.text[..self.cursor];
        let start = before
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_alphanumeric() || *c == '.' || *c == '_')
            .last()
            .map(|(i, _)| i)?;
        let word = &before[start..];
        if word.is_empty() || !word.chars().next().unwrap().is_alphabetic() {
            return None;
        }
        let prev = before[..start].trim_end().chars().last();
        if !matches!(prev, Some(c) if "=(,+-*/^&<>:;".contains(c)) {
            return None;
        }
        // Inside a string literal?
        if before[..start].matches('"').count() % 2 == 1 {
            return None;
        }
        Some((start, word.to_string()))
    }

    pub fn accept_function(&mut self, name: &str) {
        if let Some((start, _)) = self.func_prefix() {
            self.text.replace_range(start..self.cursor, &format!("{name}("));
            self.cursor = start + name.len() + 1;
            self.anchor = self.cursor;
            self.ac_hidden = true;
        }
    }

    /// Innermost function call around the caret and the argument index.
    pub fn func_context(&self) -> Option<(String, usize)> {
        if !self.is_formula() {
            return None;
        }
        let before = &self.text[..self.cursor];
        let mut depth = 0i32;
        let mut arg = 0usize;
        let mut in_str = false;
        let chars: Vec<(usize, char)> = before.char_indices().collect();
        for k in (0..chars.len()).rev() {
            let (i, c) = chars[k];
            if c == '"' {
                in_str = !in_str;
                continue;
            }
            if in_str {
                continue;
            }
            match c {
                ')' => depth += 1,
                ',' if depth == 0 => arg += 1,
                '(' => {
                    if depth == 0 {
                        let name: String = before[..i]
                            .chars()
                            .rev()
                            .take_while(|c| c.is_alphanumeric() || *c == '.' || *c == '_')
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect();
                        if name.is_empty() {
                            return None;
                        }
                        return Some((name.to_uppercase(), arg));
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        None
    }

    /// F4: cycles $ anchoring of the reference at/before the caret.
    pub fn toggle_absolute(&mut self) {
        let Ok(toks) = formula::lex(&self.text[1.min(self.text.len())..]) else { return };
        let off = 1;
        for t in toks {
            let (s, e) = (t.start + off, t.end + off);
            if !(s <= self.cursor && self.cursor <= e) {
                continue;
            }
            if let Tok::Ident(id) = &t.tok {
                let Some(p) = formula::parse_cell_part(id) else { continue };
                let (ra, ca) = match (p.row_abs, p.col_abs) {
                    (false, false) => (true, true),
                    (true, true) => (true, false),
                    (true, false) => (false, true),
                    (false, true) => (false, false),
                };
                let s2 = format!("{}{}{}{}", if ca { "$" } else { "" }, col_name(p.col), if ra { "$" } else { "" }, p.row + 1);
                self.text.replace_range(s..e, &s2);
                self.cursor = s + s2.len();
                self.anchor = self.cursor;
                self.point = None;
                return;
            }
        }
    }

    /// References in the formula with their colour index, for highlighting.
    pub fn references(&self) -> Vec<(usize, usize, Option<String>, Range)> {
        let mut out = Vec::new();
        if !self.is_formula() {
            return out;
        }
        let Ok(toks) = formula::lex(&self.text[1..]) else { return out };
        let mut i = 0;
        while i < toks.len() {
            let (sheet, start, j) = match &toks[i].tok {
                Tok::Sheet(s) => (Some(s.clone()), toks[i].start, i + 1),
                _ => (None, toks[i].start, i),
            };
            // try "A1:B2" then "A1"
            let ident = |k: usize| -> Option<String> {
                match toks.get(k).map(|t| &t.tok) {
                    Some(Tok::Ident(s)) => Some(s.clone()),
                    _ => None,
                }
            };
            if let Some(a) = ident(j).and_then(|s| formula::parse_cell_part(&s)) {
                if matches!(toks.get(j + 1).map(|t| &t.tok), Some(Tok::Colon)) {
                    if let Some(b) = ident(j + 2).and_then(|s| formula::parse_cell_part(&s)) {
                        out.push((start + 1, toks[j + 2].end + 1, sheet, Range::new(a.row, a.col, b.row, b.col)));
                        i = j + 3;
                        continue;
                    }
                }
                let is_call = matches!(toks.get(j + 1).map(|t| &t.tok), Some(Tok::LParen));
                if !is_call {
                    out.push((start + 1, toks[j].end + 1, sheet, Range::cell(a.row, a.col)));
                }
                i = j + 1;
                continue;
            }
            i += 1;
        }
        out
    }
}
