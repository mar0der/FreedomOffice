//! Flash Fill (Ctrl+E): learns a text pattern from examples typed next to the data and fills
//! the rest of the column. The learner searches for a sequence of pieces -- parts of the
//! neighbouring cells (whole cell, n-th piece between delimiters, n-th word, initial, leading /
//! trailing characters, runs of letters or digits), optionally re-cased, and literal text --
//! that reproduces every example.

use super::App;
use crate::model::{Cell, Value, MAX_ROWS};
use crate::numfmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Extract {
    Whole,
    /// Piece `idx` between `delim`s (negative = counted from the end).
    Split(char, i32),
    /// Word `idx` (split on whitespace; negative = from the end).
    Word(i32),
    /// First letter of word `idx`.
    Initial(i32),
    /// Run `idx` of letters or of digits (negative = from the end).
    Run(i32),
    Prefix(usize),
    Suffix(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    Same,
    Proper,
    Upper,
    Lower,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    Lit(String),
    Take { col: usize, ex: Extract, case: Case },
}

/// A learned pattern: concatenation of parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program(Vec<Part>);

fn pick<T: Clone>(v: &[T], idx: i32) -> Option<T> {
    let i = if idx < 0 { v.len() as i64 + idx as i64 } else { idx as i64 };
    if i < 0 || i as usize >= v.len() { None } else { Some(v[i as usize].clone()) }
}

fn runs(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut kind = 0u8;
    for ch in s.chars() {
        let k = if ch.is_alphabetic() { 1 } else if ch.is_ascii_digit() { 2 } else { 0 };
        if k == 0 {
            kind = 0;
            continue;
        }
        if k == kind {
            out.last_mut().unwrap().push(ch);
        } else {
            out.push(ch.to_string());
            kind = k;
        }
    }
    out
}

fn extract(s: &str, ex: Extract) -> Option<String> {
    let v = match ex {
        Extract::Whole => s.to_string(),
        Extract::Split(d, i) => {
            let parts: Vec<&str> = s.split(d).collect();
            if parts.len() < 2 && i != 0 {
                return None;
            }
            pick(&parts, i)?.trim().to_string()
        }
        Extract::Word(i) => pick(&s.split_whitespace().collect::<Vec<_>>(), i)?.to_string(),
        Extract::Initial(i) => pick(&s.split_whitespace().collect::<Vec<_>>(), i)?.chars().next()?.to_string(),
        Extract::Run(i) => pick(&runs(s), i)?,
        Extract::Prefix(n) => {
            if s.chars().count() < n {
                return None;
            }
            s.chars().take(n).collect()
        }
        Extract::Suffix(n) => {
            let len = s.chars().count();
            if len < n {
                return None;
            }
            s.chars().skip(len - n).collect()
        }
    };
    if v.is_empty() { None } else { Some(v) }
}

fn recase(s: &str, case: Case) -> String {
    match case {
        Case::Same => s.to_string(),
        Case::Upper => s.to_uppercase(),
        Case::Lower => s.to_lowercase(),
        Case::Proper => {
            let mut out = String::new();
            let mut start = true;
            for ch in s.chars() {
                if start {
                    out.extend(ch.to_uppercase());
                } else {
                    out.extend(ch.to_lowercase());
                }
                start = !ch.is_alphanumeric();
            }
            out
        }
    }
}

impl Program {
    /// Output for one row of source texts, or None if a piece is missing in that row.
    pub fn apply(&self, row: &[String]) -> Option<String> {
        let mut out = String::new();
        for p in &self.0 {
            match p {
                Part::Lit(t) => out.push_str(t),
                Part::Take { col, ex, case } => out.push_str(&recase(&extract(row.get(*col)?, *ex)?, *case)),
            }
        }
        Some(out)
    }
}

/// Piece indices to try: the first, the last, then the rest from the start and from the end.
fn indices(n: i32) -> Vec<i32> {
    if n <= 0 {
        return Vec::new();
    }
    let mut v = vec![0, -1];
    v.extend(1..n);
    v.extend((2..=n).map(|i| -i));
    v
}

/// Candidate pieces, most general first (whole cells and delimited pieces before positions).
fn candidates(rows: &[&[String]], ncols: usize) -> Vec<(usize, Extract)> {
    let mut out = Vec::new();
    for col in 0..ncols {
        let texts: Vec<&str> = rows.iter().filter_map(|r| r.get(col).map(|s| s.as_str())).collect();
        out.push((col, Extract::Whole));
        let mut delims: Vec<char> = Vec::new();
        for t in &texts {
            for ch in t.chars() {
                if !ch.is_alphanumeric() && !ch.is_whitespace() && !delims.contains(&ch) {
                    delims.push(ch);
                }
            }
        }
        let max_words = texts.iter().map(|t| t.split_whitespace().count()).max().unwrap_or(0) as i32;
        for d in &delims {
            let n = texts.iter().map(|t| t.split(*d).count()).max().unwrap_or(0) as i32;
            for i in indices(n) {
                out.push((col, Extract::Split(*d, i)));
            }
        }
        for i in indices(max_words) {
            out.push((col, Extract::Word(i)));
        }
        let max_runs = texts.iter().map(|t| runs(t).len()).max().unwrap_or(0) as i32;
        for i in indices(max_runs) {
            out.push((col, Extract::Run(i)));
        }
        for i in indices(max_words) {
            out.push((col, Extract::Initial(i)));
        }
        let max_len = texts.iter().map(|t| t.chars().count()).max().unwrap_or(0);
        for n in (1..max_len).rev() {
            out.push((col, Extract::Prefix(n)));
        }
        for n in (1..max_len).rev() {
            out.push((col, Extract::Suffix(n)));
        }
    }
    out
}

struct Search<'a> {
    rows: Vec<&'a [String]>,
    outs: Vec<&'a str>,
    cands: Vec<(usize, Extract)>,
    budget: usize,
}

impl Search<'_> {
    fn go(&mut self, pos: &[usize], parts: &mut Vec<Part>) -> bool {
        if pos.iter().zip(&self.outs).all(|(p, o)| *p == o.len()) {
            return true;
        }
        if pos.iter().zip(&self.outs).any(|(p, o)| *p == o.len()) || parts.len() > 14 || self.budget == 0 {
            return false;
        }
        self.budget -= 1;
        // Pieces of the source that continue every example, longest first (ties keep the
        // more general kind of piece first).
        let mut options: Vec<(usize, Part, Vec<usize>)> = Vec::new();
        for &(col, ex) in &self.cands {
            for case in [Case::Same, Case::Proper, Case::Upper, Case::Lower] {
                let mut next = Vec::with_capacity(pos.len());
                for (i, row) in self.rows.iter().enumerate() {
                    let rest = &self.outs[i][pos[i]..];
                    match row.get(col).and_then(|s| extract(s, ex)).map(|v| recase(&v, case)) {
                        Some(v) if rest.starts_with(&v) => next.push(pos[i] + v.len()),
                        _ => break,
                    }
                }
                if next.len() == pos.len() {
                    let total = next.iter().zip(pos).map(|(n, p)| n - p).sum();
                    options.push((total, Part::Take { col, ex, case }, next));
                    break;
                }
            }
        }
        options.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, part, next) in options {
            parts.push(part);
            if self.go(&next, parts) {
                return true;
            }
            parts.pop();
        }
        // Literal text shared by every example.
        let ch = self.outs[0][pos[0]..].chars().next().unwrap();
        if self.outs.iter().zip(pos).all(|(o, p)| o[*p..].starts_with(ch)) {
            let next: Vec<usize> = pos.iter().map(|p| p + ch.len_utf8()).collect();
            match parts.last_mut() {
                Some(Part::Lit(t)) => {
                    t.push(ch);
                    if self.go(&next, parts) {
                        return true;
                    }
                    if let Some(Part::Lit(t)) = parts.last_mut() {
                        t.pop();
                    }
                }
                _ => {
                    parts.push(Part::Lit(ch.to_string()));
                    if self.go(&next, parts) {
                        return true;
                    }
                    parts.pop();
                }
            }
        }
        false
    }
}

/// Learns a program reproducing every (source row, output) example.
pub fn learn(examples: &[(Vec<String>, String)]) -> Option<Program> {
    if examples.is_empty() {
        return None;
    }
    let ncols = examples.iter().map(|(r, _)| r.len()).max().unwrap_or(0);
    let rows: Vec<&[String]> = examples.iter().map(|(r, _)| r.as_slice()).collect();
    let cands = candidates(&rows, ncols);
    let mut s = Search { rows, outs: examples.iter().map(|(_, o)| o.as_str()).collect(), cands, budget: 20_000 };
    let mut parts = Vec::new();
    if s.go(&vec![0; examples.len()], &mut parts) {
        // An all-literal program just repeats the example: that is not a pattern.
        if parts.iter().all(|p| matches!(p, Part::Lit(_))) {
            return None;
        }
        Some(Program(parts))
    } else {
        None
    }
}

impl App {
    /// Ctrl+E / Data > Flash Fill for the active cell's column.
    pub fn flash_fill(&mut self) {
        self.commit_edit(false);
        let si = self.wb.active;
        let (r0, c) = self.active();
        let sh = &self.wb.sheets[si];
        let text = |r: u32, c: u32| -> String {
            sh.get(r, c).map(|x| numfmt::format_value(&x.value, &self.wb.style(x.style).num_fmt).text).unwrap_or_default()
        };
        let side = |r: u32| (c > 0 && sh.is_populated(r, c - 1)) || sh.is_populated(r, c + 1);
        let (mut top, mut bottom) = (r0, r0);
        while top > 0 && side(top - 1) {
            top -= 1;
        }
        while bottom + 1 < MAX_ROWS && side(bottom + 1) {
            bottom += 1;
        }
        // Source columns: the contiguous data to the left and to the right of the column.
        let used = |cc: u32| (top..=bottom).any(|r| sh.is_populated(r, cc));
        let mut cols = Vec::new();
        let mut cc = c;
        while cc > 0 && used(cc - 1) {
            cc -= 1;
            cols.push(cc);
        }
        cols.reverse();
        let mut cc = c + 1;
        while cc < crate::model::MAX_COLS && used(cc) {
            cols.push(cc);
            cc += 1;
        }
        let rows: Vec<(u32, Vec<String>, String)> =
            (top..=bottom).map(|r| (r, cols.iter().map(|&k| text(r, k)).collect(), text(r, c))).collect();
        let examples: Vec<(Vec<String>, String)> = rows.iter().filter(|(_, _, o)| !o.is_empty()).map(|(_, s, o)| (s.clone(), o.clone())).collect();
        let all_text = rows.iter().filter(|(_, _, o)| !o.is_empty()).all(|(r, _, _)| matches!(sh.value(*r, c), Value::Text(_)));
        // The first row may be a header that is not part of the pattern.
        let prog = learn(&examples).or_else(|| if examples.len() > 1 && !rows[0].2.is_empty() { learn(&examples[1..]) } else { None });
        let Some(prog) = prog.filter(|_| !cols.is_empty()) else {
            self.dialog = Some(super::dialogs::Dialog::Message {
                title: "FreedomOffice Sheets".into(),
                text: "Flash Fill didn't find a pattern in the data next to the selection.\n\nType a couple of examples of the result you want in the column, keep the active cell in that column, and try Flash Fill again.".into(),
            });
            return;
        };
        let fills: Vec<(u32, String)> =
            rows.iter().filter(|(_, _, o)| o.is_empty()).filter_map(|(r, s, _)| prog.apply(s).filter(|v| !v.is_empty()).map(|v| (*r, v))).collect();
        if fills.is_empty() {
            return;
        }
        let rg = crate::model::Range::new(top, c, bottom, c);
        if fills.iter().any(|(r, _)| self.locked_block(crate::model::Range::cell(*r, c))) {
            return;
        }
        self.push_undo(&[super::Scope::Range(si, rg)]);
        for (r, v) in &fills {
            let style = self.wb.sheets[si].get(*r, c).map_or(0, |x| x.style);
            let cell = if all_text || v.starts_with('=') {
                Some(Cell { value: Value::Text(v.clone()), formula: None, style })
            } else {
                crate::model::cell_from_input(&mut self.wb, v, style)
            };
            self.wb.sheets[si].set(*r, c, cell);
        }
        self.select_range(rg);
        self.changed_ranges(si, &[rg]);
        self.status = Some(format!("Flash Fill Changed Cells: {}", fills.len()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(src: &[&str], out: &str) -> (Vec<String>, String) {
        (src.iter().map(|s| s.to_string()).collect(), out.to_string())
    }

    fn run(p: &Program, src: &[&str]) -> Option<String> {
        p.apply(&src.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn first_and_last_names() {
        let p = learn(&[ex(&["John Smith"], "John")]).unwrap();
        assert_eq!(run(&p, &["Mary Jones"]).as_deref(), Some("Mary"));
        let p = learn(&[ex(&["John Smith"], "Smith")]).unwrap();
        assert_eq!(run(&p, &["Mary Ann Jones"]).as_deref(), Some("Jones"));
    }

    #[test]
    fn email_user_and_domain() {
        let p = learn(&[ex(&["jane.doe@contoso.com"], "jane.doe")]).unwrap();
        assert_eq!(run(&p, &["bob@fabrikam.org"]).as_deref(), Some("bob"));
        let p = learn(&[ex(&["jane.doe@contoso.com"], "contoso.com")]).unwrap();
        assert_eq!(run(&p, &["bob@fabrikam.org"]).as_deref(), Some("fabrikam.org"));
    }

    #[test]
    fn case_changes_and_reordering() {
        let p = learn(&[ex(&["john smith"], "Smith, J.")]).unwrap();
        assert_eq!(run(&p, &["mary jones"]).as_deref(), Some("Jones, M."));
        let p = learn(&[ex(&["acme corp"], "ACME CORP")]).unwrap();
        assert_eq!(run(&p, &["big co"]).as_deref(), Some("BIG CO"));
    }

    #[test]
    fn concatenates_columns() {
        let p = learn(&[ex(&["John", "Smith"], "John Smith")]).unwrap();
        assert_eq!(run(&p, &["Mary", "Jones"]).as_deref(), Some("Mary Jones"));
        let p = learn(&[ex(&["John", "Smith"], "jsmith"), ex(&["Ann", "Lee"], "alee")]).unwrap();
        assert_eq!(run(&p, &["Mary", "Jones"]).as_deref(), Some("mjones"));
    }

    #[test]
    fn codes_and_numbers() {
        let p = learn(&[ex(&["SKU-1234-RED"], "1234")]).unwrap();
        assert_eq!(run(&p, &["SKU-98-BLUE"]).as_deref(), Some("98"));
        let p = learn(&[ex(&["Order12345"], "12345")]).unwrap();
        assert_eq!(run(&p, &["Order77"]).as_deref(), Some("77"));
    }

    #[test]
    fn several_examples_disambiguate() {
        // One example could mean "first 3 characters" or "first word"; two settle it.
        let p = learn(&[ex(&["Bob Smith"], "Bob"), ex(&["Alice Jones"], "Alice")]).unwrap();
        assert_eq!(run(&p, &["Christopher Lee"]).as_deref(), Some("Christopher"));
    }

    #[test]
    fn no_pattern() {
        assert!(learn(&[ex(&["abc"], "xyz")]).is_none());
        assert!(learn(&[]).is_none());
    }
}
