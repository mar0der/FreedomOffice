//! Best-effort import of charts from a worksheet's drawing part.
//! Charts that cannot be understood are skipped silently.

use std::io::{Read, Seek};

use quick_xml::events::Event;
use quick_xml::Reader;

use super::{attr, dir_of, file_of, local, parse_rels, read_text, resolve, Package};
use crate::chart::{col_x, row_y, Chart, ChartKind, MIN_SIZE};
use crate::model::{Range, Sheet};

const EMU_PER_PX: f32 = 9525.0;

pub(super) fn read<R: Read + Seek>(pkg: &mut Package<'_, R>, sheet_path: &str, sheet: &mut Sheet) {
    let dir = dir_of(sheet_path);
    let Some(rels) = pkg.text(&format!("{dir}_rels/{}.rels", file_of(sheet_path))) else { return };
    for rel in parse_rels(&rels).iter().filter(|r| r.ty.ends_with("/drawing")) {
        let dpath = resolve(&dir, &rel.target);
        let Some(dxml) = pkg.text(&dpath) else { continue };
        let ddir = dir_of(&dpath);
        let drels = pkg.text(&format!("{ddir}_rels/{}.rels", file_of(&dpath))).map(|x| parse_rels(&x)).unwrap_or_default();
        for a in parse_drawing(&dxml) {
            let Some(target) = drels.iter().find(|r| r.id == a.rid) else { continue };
            let Some(cxml) = pkg.text(&resolve(&ddir, &target.target)) else { continue };
            if let Some(ch) = parse_chart(&cxml, &a, sheet) {
                sheet.charts.push(ch);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Drawing anchors
// ---------------------------------------------------------------------------

#[derive(Default, Clone, Copy)]
struct Marker {
    col: u32,
    col_off: f32,
    row: u32,
    row_off: f32,
}

#[derive(Default)]
struct Anchor {
    from: Option<Marker>,
    to: Option<Marker>,
    ext: Option<(f32, f32)>,
    pos: Option<(f32, f32)>,
    rid: String,
}

fn num(s: &str) -> f32 {
    s.trim().parse::<f32>().unwrap_or(0.0)
}

fn parse_drawing(xml: &str) -> Vec<Anchor> {
    let mut out = Vec::new();
    let mut rd = Reader::from_str(xml);
    let mut stack: Vec<String> = Vec::new();
    let mut cur: Option<Anchor> = None;
    loop {
        let ev = rd.read_event();
        match ev {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let is_start = matches!(ev, Ok(Event::Start(_)));
                let name = local(e).to_string();
                let parent = stack.last().map(String::as_str).unwrap_or("");
                let top = parent.ends_with("Anchor");
                match name.as_str() {
                    "twoCellAnchor" | "oneCellAnchor" | "absoluteAnchor" => cur = Some(Anchor::default()),
                    "from" | "to" if top => {
                        if let Some(a) = &mut cur {
                            *(if name == "from" { &mut a.from } else { &mut a.to }) = Some(Marker::default());
                        }
                    }
                    "col" | "colOff" | "row" | "rowOff" if is_start && (parent == "from" || parent == "to") => {
                        let v = num(&read_text(&mut rd, &name));
                        if let Some(m) = cur.as_mut().and_then(|a| if parent == "from" { a.from.as_mut() } else { a.to.as_mut() }) {
                            match name.as_str() {
                                "col" => m.col = v as u32,
                                "colOff" => m.col_off = v / EMU_PER_PX,
                                "row" => m.row = v as u32,
                                _ => m.row_off = v / EMU_PER_PX,
                            }
                        }
                        continue;
                    }
                    "ext" if top => {
                        if let (Some(a), Some(cx), Some(cy)) = (&mut cur, attr(e, "cx"), attr(e, "cy")) {
                            a.ext = Some((num(&cx) / EMU_PER_PX, num(&cy) / EMU_PER_PX));
                        }
                    }
                    "pos" if top => {
                        if let (Some(a), Some(x), Some(y)) = (&mut cur, attr(e, "x"), attr(e, "y")) {
                            a.pos = Some((num(&x) / EMU_PER_PX, num(&y) / EMU_PER_PX));
                        }
                    }
                    "chart" => {
                        if let (Some(a), Some(id)) = (&mut cur, attr(e, "id")) {
                            a.rid = id;
                        }
                    }
                    _ => {}
                }
                if is_start {
                    stack.push(name);
                }
            }
            Ok(Event::End(_)) => {
                let name = stack.pop().unwrap_or_default();
                if name.ends_with("Anchor") {
                    if let Some(a) = cur.take().filter(|a| !a.rid.is_empty()) {
                        out.push(a);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Chart part
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Ser {
    tx: Option<String>,
    cat: Option<String>,
    val: Option<String>,
}

/// Splits `Sheet1!$A$1:$B$2` / `'My Sheet'!A1` into (sheet, range).
fn parse_ref(f: &str) -> Option<(String, Range)> {
    let f = f.trim().trim_start_matches('=');
    if f.starts_with('(') {
        return None;
    }
    let i = f.rfind('!')?;
    let (sh, rg) = (&f[..i], &f[i + 1..]);
    let sh = match sh.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        Some(q) => q.replace("''", "'"),
        None => sh.to_string(),
    };
    Some((sh, Range::parse(rg)?))
}

fn on(v: Option<String>) -> bool {
    v.is_none_or(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

fn parse_chart(xml: &str, anchor: &Anchor, sheet: &Sheet) -> Option<Chart> {
    let mut rd = Reader::from_str(xml);
    let mut stack: Vec<String> = Vec::new();
    let mut kind: Option<ChartKind> = None;
    let mut groups = 0u32;
    let mut series: Vec<Ser> = Vec::new();
    let mut cur: Option<Ser> = None;
    let mut title_depth: Option<usize> = None;
    let (mut has_title, mut title_text, mut title_cache, mut title_ref) = (false, String::new(), String::new(), false);
    let (mut auto_deleted, mut legend, mut labels) = (false, false, false);
    loop {
        let ev = rd.read_event();
        match ev {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let is_start = matches!(ev, Ok(Event::Start(_)));
                let name = local(e).to_string();
                let parent = stack.last().map(String::as_str).unwrap_or("");
                let in_plot = stack.iter().any(|s| s == "plotArea");
                let group_kind = match name.as_str() {
                    "barChart" | "bar3DChart" => Some(ChartKind::Column),
                    "lineChart" | "line3DChart" => Some(ChartKind::Line),
                    "pieChart" | "pie3DChart" | "ofPieChart" => Some(ChartKind::Pie),
                    "doughnutChart" => Some(ChartKind::Doughnut),
                    "areaChart" | "area3DChart" => Some(ChartKind::Area),
                    "scatterChart" => Some(ChartKind::Scatter),
                    _ => None,
                };
                if let Some(k) = group_kind.filter(|_| in_plot && is_start) {
                    groups += 1;
                    if groups == 1 {
                        kind = Some(k);
                    }
                }
                let first_group = groups == 1 && stack.iter().any(|s| s.ends_with("Chart") && s != "chart");
                match name.as_str() {
                    "barDir" if first_group => {
                        if attr(e, "val").as_deref() == Some("bar") {
                            kind = Some(ChartKind::Bar);
                        }
                    }
                    "ser" if first_group && is_start => cur = Some(Ser::default()),
                    "title" if parent == "chart" => {
                        has_title = true;
                        if is_start {
                            title_depth = Some(stack.len());
                        }
                    }
                    "legend" if parent == "chart" => legend = true,
                    "autoTitleDeleted" if parent == "chart" => auto_deleted = on(attr(e, "val")),
                    "showVal" if in_plot => labels |= on(attr(e, "val")),
                    "f" if is_start => {
                        let t = read_text(&mut rd, "f");
                        if title_depth.is_some() {
                            title_ref = true;
                        } else if let Some(s) = &mut cur {
                            let within = |n: &str| stack.iter().rev().take_while(|x| *x != "ser").any(|x| x == n);
                            if within("tx") {
                                s.tx = Some(t);
                            } else if within("cat") || within("xVal") {
                                s.cat = Some(t);
                            } else if within("val") || within("yVal") {
                                s.val = Some(t);
                            }
                        }
                        continue;
                    }
                    "t" if is_start && title_depth.is_some() => {
                        title_text.push_str(&read_text(&mut rd, "t"));
                        continue;
                    }
                    "v" if is_start && title_depth.is_some() => {
                        title_cache.push_str(&read_text(&mut rd, "v"));
                        continue;
                    }
                    _ => {}
                }
                if is_start {
                    stack.push(name);
                }
            }
            Ok(Event::End(_)) => {
                let name = stack.pop().unwrap_or_default();
                if name == "ser" {
                    if let Some(s) = cur.take() {
                        series.push(s);
                    }
                } else if name == "title" && title_depth == Some(stack.len()) {
                    title_depth = None;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    let kind = kind?;
    let (src, first) = parse_ref(series.first()?.val.as_deref()?)?;
    let same = |s: &str| s.eq_ignore_ascii_case(&src);
    let mut range = first;
    let mut cat_rg = None;
    let (mut names, mut cats) = (false, false);
    for s in &series {
        for (f, which) in [(&s.val, 0), (&s.cat, 1), (&s.tx, 2)] {
            if let Some((sh, rg)) = f.as_deref().and_then(parse_ref) {
                if same(&sh) {
                    range = range.union(&rg);
                    match which {
                        1 => {
                            cats = true;
                            cat_rg.get_or_insert(rg);
                        }
                        2 => names = true,
                        _ => {}
                    }
                }
            }
        }
    }
    let by_cols = if first.c1 == first.c2 && first.r2 > first.r1 {
        true
    } else if first.r1 == first.r2 && first.c2 > first.c1 {
        false
    } else {
        cat_rg.is_none_or(|c: Range| c.c1 == c.c2)
    };

    let title = if has_title {
        if !title_text.is_empty() {
            Some(title_text)
        } else if title_ref && !title_cache.is_empty() {
            Some(title_cache)
        } else {
            None
        }
    } else if auto_deleted || series.len() > 1 {
        Some(String::new())
    } else {
        None
    };

    let from = anchor.from.unwrap_or_default();
    let (x, y) = match anchor.pos {
        Some(p) if anchor.from.is_none() => p,
        _ => (col_x(sheet, from.col) + from.col_off, row_y(sheet, from.row) + from.row_off),
    };
    let (w, h) = match (anchor.to, anchor.ext) {
        (Some(to), _) => (col_x(sheet, to.col) + to.col_off - x, row_y(sheet, to.row) + to.row_off - y),
        (None, Some(ext)) => ext,
        _ => (crate::chart::DEFAULT_W, crate::chart::DEFAULT_H),
    };
    let mut ch = Chart {
        kind,
        sheet: src,
        range,
        by_cols,
        names_first: Some(names),
        cats_first: Some(cats),
        title,
        legend,
        data_labels: labels,
        anchor: (from.row, from.col),
        offset: (from.col_off, from.row_off),
        size: (w.max(MIN_SIZE), h.max(MIN_SIZE)),
    };
    if anchor.from.is_none() {
        crate::chart::set_sheet_rect(sheet, &mut ch, x, y, w, h);
    }
    Some(ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs() {
        assert_eq!(parse_ref("Sheet1!$B$2:$B$6"), Some(("Sheet1".into(), Range::new(1, 1, 5, 1))));
        assert_eq!(parse_ref("'It''s data'!A1"), Some(("It's data".into(), Range::cell(0, 0))));
        assert_eq!(parse_ref("(Sheet1!$A$1,Sheet1!$A$3)"), None);
    }
}
