//! A1 notation for agents: "B7", "A1:C5", "A:C", "3:5", optionally with a sheet in front
//! ("Data!A1:B2", "'My sheet'!A1"). Cell parsing itself is the app's (`model::Range`).

use freedom_sheets::model::{self, Range};

/// A reference as an agent writes it: an optional sheet name and the range on it.
#[derive(Clone, Debug, PartialEq)]
pub struct Ref {
    pub sheet: Option<String>,
    pub range: Range,
}

/// Splits "Sheet!A1:B2" / "'It''s'!A1" into the sheet name and the rest.
pub fn split_sheet(s: &str) -> (Option<String>, &str) {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('\'') {
        // Quoted: a doubled quote is a quote.
        let mut name = String::new();
        let mut chars = rest.char_indices();
        while let Some((i, ch)) = chars.next() {
            if ch == '\'' {
                if rest[i + 1..].starts_with('\'') {
                    name.push('\'');
                    chars.next();
                    continue;
                }
                if let Some(tail) = rest[i + 1..].strip_prefix('!') {
                    return (Some(name), tail);
                }
                break;
            }
            name.push(ch);
        }
        return (None, s);
    }
    match s.rsplit_once('!') {
        Some((n, tail)) if !n.is_empty() => (Some(n.to_string()), tail),
        _ => (None, s),
    }
}

/// Parses an agent's reference. Errors say what was wrong.
pub fn parse_ref(s: &str) -> Result<Ref, String> {
    let (sheet, rest) = split_sheet(s);
    let range = Range::parse(rest)
        .ok_or_else(|| format!("'{rest}' isn't a cell range like B7, A1:C5, A:C or 3:5."))?;
    Ok(Ref { sheet, range })
}

pub fn cell_name(r: u32, c: u32) -> String {
    model::cell_name(r, c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_and_ranges() {
        assert_eq!(parse_ref("B7").unwrap().range, Range::cell(6, 1));
        assert_eq!(parse_ref("$c$3").unwrap().range, Range::cell(2, 2));
        assert_eq!(parse_ref("A1:C5").unwrap().range, Range::new(0, 0, 4, 2));
        assert_eq!(parse_ref("C5:A1").unwrap().range, Range::new(0, 0, 4, 2));
        assert_eq!(parse_ref("AB7").unwrap().range, Range::cell(6, 27));
        assert_eq!(parse_ref(" d2 ").unwrap().range, Range::cell(1, 3));
    }

    #[test]
    fn whole_columns_and_rows() {
        let c = parse_ref("A:C").unwrap().range;
        assert!(c.is_full_cols() && c.c1 == 0 && c.c2 == 2);
        let r = parse_ref("3:5").unwrap().range;
        assert!(r.is_full_rows() && r.r1 == 2 && r.r2 == 4);
    }

    #[test]
    fn sheet_prefix() {
        let r = parse_ref("Data!A1:B2").unwrap();
        assert_eq!(r.sheet.as_deref(), Some("Data"));
        assert_eq!(r.range, Range::new(0, 0, 1, 1));
        let r = parse_ref("'My sheet'!C3").unwrap();
        assert_eq!(r.sheet.as_deref(), Some("My sheet"));
        assert_eq!(r.range, Range::cell(2, 2));
        let r = parse_ref("'It''s'!A1").unwrap();
        assert_eq!(r.sheet.as_deref(), Some("It's"));
        assert_eq!(parse_ref("A1").unwrap().sheet, None);
    }

    #[test]
    fn rejects_nonsense() {
        for bad in [
            "", "7", "A", "A0", "A1:", ":B2", "A1:B", "1A", "AAAA1", "A1048577", "A1;B2",
            "'open!A1",
        ] {
            assert!(parse_ref(bad).is_err(), "{bad}");
        }
    }
}
