//! Page setup: `<pageSetup>`, `<pageMargins>`, `<printOptions>`, `<headerFooter>`, manual
//! page breaks and the `_xlnm.Print_Area` / `_xlnm.Print_Titles` defined names.

use quick_xml::events::Event;
use quick_xml::Reader;

use super::{attr, attr_bool, local, parse_f, read_text, skip};
use crate::model::Range;
use crate::page::{HeaderFooter, PageSetup, Paper, Scaling};

pub(super) fn parse_page_setup(xml: &str, page: &mut PageSetup) {
    let mut rd = Reader::from_str(xml);
    let mut fit_to_page = false;
    let mut fit = (1u16, 1u16);
    let mut scale: Option<u16> = None;
    // Some(true) inside <rowBreaks>, Some(false) inside <colBreaks>.
    let mut row_breaks: Option<bool> = None;
    loop {
        let ev = rd.read_event();
        match ev {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let is_start = matches!(ev, Ok(Event::Start(_)));
                match local(e) {
                    // customSheetViews carry their own page settings; not the sheet's.
                    "sheetData" | "customSheetViews" | "extLst" | "AlternateContent" | "conditionalFormatting"
                    | "dataValidations"
                        if is_start =>
                    {
                        skip(&mut rd, e)
                    }
                    "pageSetUpPr" => fit_to_page = attr_bool(e, "fitToPage").unwrap_or(false),
                    "pageMargins" => {
                        let m = &mut page.margins;
                        let get = |n: &str, d: f32| attr(e, n).and_then(|v| parse_f(&v)).map(|v| v as f32).unwrap_or(d);
                        m.left = get("left", m.left);
                        m.right = get("right", m.right);
                        m.top = get("top", m.top);
                        m.bottom = get("bottom", m.bottom);
                        m.header = get("header", m.header);
                        m.footer = get("footer", m.footer);
                    }
                    "printOptions" => {
                        page.gridlines = attr_bool(e, "gridLines").unwrap_or(false);
                        page.headings = attr_bool(e, "headings").unwrap_or(false);
                        page.center_h = attr_bool(e, "horizontalCentered").unwrap_or(false);
                        page.center_v = attr_bool(e, "verticalCentered").unwrap_or(false);
                    }
                    "pageSetup" => {
                        match attr(e, "orientation").as_deref() {
                            Some("landscape") => page.landscape = true,
                            Some("portrait") => page.landscape = false,
                            _ => {}
                        }
                        if let Some(p) = attr(e, "paperSize").and_then(|v| v.parse().ok()).and_then(Paper::from_code) {
                            page.paper = p;
                        }
                        scale = attr(e, "scale").and_then(|v| v.parse().ok());
                        let dim = |n: &str| attr(e, n).and_then(|v| v.parse::<u16>().ok()).unwrap_or(1);
                        fit = (dim("fitToWidth"), dim("fitToHeight"));
                        page.over_then_down = attr(e, "pageOrder").as_deref() == Some("overThenDown");
                        page.black_white = attr_bool(e, "blackAndWhite").unwrap_or(false);
                        let first = attr(e, "firstPageNumber").and_then(|v| v.parse::<u32>().ok());
                        page.first_page = match attr(e, "useFirstPageNumber").as_deref() {
                            Some("1") | Some("true") => Some(first.unwrap_or(1)),
                            // rust_xlsxwriter (and XlsxWriter) store the number itself here.
                            Some(v) => v.parse::<u32>().ok().filter(|&n| n > 1),
                            None => None,
                        };
                    }
                    "oddHeader" if is_start => page.header = HeaderFooter::from_xlsx(&read_text(&mut rd, "oddHeader")),
                    "oddFooter" if is_start => page.footer = HeaderFooter::from_xlsx(&read_text(&mut rd, "oddFooter")),
                    "rowBreaks" if is_start => row_breaks = Some(true),
                    "colBreaks" if is_start => row_breaks = Some(false),
                    "brk" => {
                        let id = attr(e, "id").and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                        if id > 0 && attr_bool(e, "man").unwrap_or(false) {
                            match row_breaks {
                                Some(true) => {
                                    page.row_breaks.insert(id);
                                }
                                Some(false) => {
                                    page.col_breaks.insert(id);
                                }
                                None => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let n: &str = e.name().0;
                if matches!(n.rsplit(':').next(), Some("rowBreaks") | Some("colBreaks")) {
                    row_breaks = None;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    if fit_to_page {
        page.scaling = Scaling::Fit { wide: fit.0, tall: fit.1 };
    } else if let Some(s) = scale {
        page.scaling = Scaling::Percent(s.clamp(10, 400));
    }
}

/// Applies a workbook-level `_xlnm.Print_Area` / `_xlnm.Print_Titles` defined name.
pub(super) fn apply_defined_name(name: &str, text: &str, page: &mut PageSetup) {
    let areas = split_areas(text);
    if name.eq_ignore_ascii_case("_xlnm.Print_Area") {
        if let Some(rg) = areas.first().and_then(|a| Range::parse(a)) {
            page.print_area = Some(rg);
        }
    } else if name.eq_ignore_ascii_case("_xlnm.Print_Titles") {
        for rg in areas.iter().filter_map(|a| Range::parse(a)) {
            if rg.is_full_rows() && !rg.is_full_cols() {
                page.title_rows = Some((rg.r1, rg.r2));
            } else if rg.is_full_cols() && !rg.is_full_rows() {
                page.title_cols = Some((rg.c1, rg.c2));
            }
        }
    }
}

/// Splits "Sheet1!$A:$B,'It''s, here'!$1:$2" into its references without sheet prefixes.
fn split_areas(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '\'' => {
                quoted = !quoted; // a doubled '' toggles twice
                cur.push(c);
            }
            ',' if !quoted => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts
        .into_iter()
        .map(|p| {
            let p = p.trim();
            // The sheet prefix ends at the last '!' outside quotes.
            let mut quoted = false;
            let mut bang = None;
            for (i, c) in p.char_indices() {
                match c {
                    '\'' => quoted = !quoted,
                    '!' if !quoted => bang = Some(i),
                    _ => {}
                }
            }
            match bang {
                Some(i) => p[i + 1..].to_string(),
                None => p.to_string(),
            }
        })
        .filter(|p| !p.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn areas_split_outside_quotes() {
        assert_eq!(super::split_areas("'It''s, here!'!$1:$2,'It''s, here!'!$A:$B"), ["$1:$2", "$A:$B"]);
        assert_eq!(super::split_areas("Sheet1!$A$1:$D$20"), ["$A$1:$D$20"]);
    }
}
