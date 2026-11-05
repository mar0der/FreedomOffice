//! Best-effort import of charts from a worksheet's drawing part.
//! Charts that cannot be understood are skipped silently.

use std::io::{Read, Seek};

use quick_xml::events::Event;
use quick_xml::Reader;

use super::{attr, attrs, dir_of, file_of, local, parse_rels, read_text, resolve, Package};
use crate::chart::{
    col_x, palette_color, row_y, Axis, Chart, ChartKind, Custom, Dash, ErrDir, ErrKind, ErrorBars, Grouping, LabelPos, Labels, LegendPos,
    MarkerKind, PointFmt, SeriesFmt, SeriesSrc, TrendKind, Trendline, MIN_SIZE,
};
use crate::model::{Range, Sheet};

const EMU_PER_PX: f32 = 9525.0;
const EMU_PER_PT: f32 = 12700.0;

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
// A small element tree for the chart part
// ---------------------------------------------------------------------------

#[derive(Default, Debug)]
struct El {
    name: String,
    attrs: Vec<(String, String)>,
    kids: Vec<El>,
    text: String,
}

impl El {
    fn child(&self, n: &str) -> Option<&El> {
        self.kids.iter().find(|k| k.name == n)
    }
    fn all<'a>(&'a self, n: &'a str) -> impl Iterator<Item = &'a El> + 'a {
        self.kids.iter().filter(move |k| k.name == n)
    }
    fn at(&self, path: &[&str]) -> Option<&El> {
        path.iter().try_fold(self, |e, n| e.child(n))
    }
    fn attr(&self, n: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str())
    }
    fn val(&self) -> Option<&str> {
        self.attr("val")
    }
    /// Value of `<child val=".."/>`.
    fn cval(&self, n: &str) -> Option<&str> {
        self.child(n).and_then(El::val)
    }
    fn cnum(&self, n: &str) -> Option<f64> {
        self.cval(n).and_then(|v| v.trim().parse().ok())
    }
    /// Boolean flag child: present without val, or val 1/true.
    fn flag(&self, n: &str) -> Option<bool> {
        self.child(n).map(|c| c.val().is_none_or(|v| v == "1" || v.eq_ignore_ascii_case("true")))
    }
    /// Text of every descendant element named `n`, concatenated.
    fn deep_text(&self, n: &str, out: &mut String) {
        for k in &self.kids {
            if k.name == n {
                out.push_str(&k.text);
            }
            k.deep_text(n, out);
        }
    }
}

fn dom(xml: &str) -> Option<El> {
    let mut rd = Reader::from_str(xml);
    let mut stack: Vec<El> = vec![El::default()];
    loop {
        match rd.read_event() {
            Ok(Event::Start(e)) => stack.push(El { name: local(&e).to_string(), attrs: attrs(&e), ..Default::default() }),
            Ok(Event::Empty(e)) => {
                let el = El { name: local(&e).to_string(), attrs: attrs(&e), ..Default::default() };
                stack.last_mut()?.kids.push(el);
            }
            Ok(Event::End(_)) => {
                let el = stack.pop()?;
                stack.last_mut()?.kids.push(el);
            }
            Ok(Event::Text(t)) => stack.last_mut()?.text.push_str(&t.xml10_content()),
            Ok(Event::CData(t)) => stack.last_mut()?.text.push_str(&t.xml10_content()),
            Ok(Event::GeneralRef(r)) => {
                let top = stack.last_mut()?;
                if r.is_char_ref() {
                    if let Ok(Some(c)) = r.resolve_char_ref() {
                        top.text.push(c);
                    }
                } else if let Some(s) = quick_xml::escape::resolve_predefined_entity(&r) {
                    top.text.push_str(s);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    let mut root = stack.into_iter().next()?;
    root.kids.pop()
}

// ---------------------------------------------------------------------------
// Chart part
// ---------------------------------------------------------------------------

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

/// Colour of a `solidFill` (or anything holding a colour element).
fn color_of(fill: &El) -> Option<u32> {
    if let Some(c) = fill.child("srgbClr").and_then(El::val) {
        return u32::from_str_radix(c, 16).ok();
    }
    let s = fill.child("schemeClr").and_then(El::val)?;
    Some(match s {
        "accent1" => 0x4472C4,
        "accent2" => 0xED7D31,
        "accent3" => 0xA5A5A5,
        "accent4" => 0xFFC000,
        "accent5" => 0x5B9BD5,
        "accent6" => 0x70AD47,
        "bg1" | "lt1" => 0xFFFFFF,
        "tx1" | "dk1" => 0x000000,
        "bg2" | "lt2" => 0xE7E6E6,
        "tx2" | "dk2" => 0x44546A,
        _ => return None,
    })
}

/// (fill, line colour, line width pt, no line, dash) of an `spPr`.
fn shape_props(sp: Option<&El>) -> (Option<u32>, Option<u32>, Option<f32>, bool, Dash) {
    let Some(sp) = sp else { return (None, None, None, false, Dash::Solid) };
    let fill = sp.child("solidFill").and_then(color_of);
    let ln = sp.child("ln");
    let line = ln.and_then(|l| l.child("solidFill")).and_then(color_of);
    let width = ln.and_then(|l| l.attr("w")).and_then(|w| w.parse::<f32>().ok()).map(|w| w / EMU_PER_PT);
    let no_line = ln.is_some_and(|l| l.child("noFill").is_some());
    let dash = match ln.and_then(|l| l.cval("prstDash")) {
        Some("sysDot") => Dash::RoundDot,
        Some("sysDash") | Some("dot") => Dash::SquareDot,
        Some("dash") => Dash::Dash,
        Some("dashDot") => Dash::DashDot,
        Some("lgDash") => Dash::LongDash,
        Some("lgDashDot") | Some("lgDashDotDot") => Dash::LongDashDot,
        _ => Dash::Solid,
    };
    (fill, line, width, no_line, dash)
}

fn text_of(title: &El) -> Option<String> {
    let mut t = String::new();
    if let Some(rich) = title.at(&["tx", "rich"]) {
        for p in rich.all("p") {
            let mut line = String::new();
            p.deep_text("t", &mut line);
            if !t.is_empty() && !line.is_empty() {
                t.push('\n');
            }
            t.push_str(&line);
        }
    } else if let Some(sr) = title.at(&["tx", "strRef"]) {
        sr.deep_text("v", &mut t);
    }
    (!t.is_empty()).then_some(t)
}

fn labels_of(d: &El) -> Option<Labels> {
    if d.flag("delete") == Some(true) {
        return Some(Labels::default());
    }
    let on = |n: &str| d.flag(n).unwrap_or(false);
    let pos = match d.cval("dLblPos") {
        Some("ctr") => LabelPos::Center,
        Some("inEnd") => LabelPos::InsideEnd,
        Some("inBase") => LabelPos::InsideBase,
        Some("outEnd") => LabelPos::OutsideEnd,
        Some("l") => LabelPos::Left,
        Some("r") => LabelPos::Right,
        Some("t") => LabelPos::Above,
        Some("b") => LabelPos::Below,
        Some("bestFit") => LabelPos::BestFit,
        _ => LabelPos::Auto,
    };
    let num_fmt = d.child("numFmt").filter(|n| n.attr("sourceLinked") != Some("1")).and_then(|n| n.attr("formatCode")).unwrap_or("").to_string();
    let num_fmt = if num_fmt.eq_ignore_ascii_case("general") { String::new() } else { num_fmt };
    Some(Labels { value: on("showVal"), category: on("showCatName"), series: on("showSerName"), percent: on("showPercent"), pos, num_fmt })
}

struct AxisInfo {
    id: String,
    tag: String,
    pos: String,
    axis: Axis,
}

fn axis_of(e: &El) -> AxisInfo {
    let scaling = e.child("scaling");
    let num_fmt = e.child("numFmt").filter(|n| n.attr("sourceLinked") != Some("1")).and_then(|n| n.attr("formatCode")).unwrap_or("");
    let axis = Axis {
        visible: e.flag("delete") != Some(true),
        title: e.child("title").map(|t| text_of(t).unwrap_or_default()),
        min: scaling.and_then(|s| s.cnum("min")),
        max: scaling.and_then(|s| s.cnum("max")),
        major: e.cnum("majorUnit"),
        minor: e.cnum("minorUnit"),
        num_fmt: if num_fmt.eq_ignore_ascii_case("general") { String::new() } else { num_fmt.to_string() },
        log_base: scaling.and_then(|s| s.cnum("logBase")),
        reverse: scaling.and_then(|s| s.cval("orientation")) == Some("maxMin"),
        major_grid: e.child("majorGridlines").is_some(),
        minor_grid: e.child("minorGridlines").is_some(),
    };
    AxisInfo { id: e.cval("axId").unwrap_or("").to_string(), tag: e.name.clone(), pos: e.cval("axPos").unwrap_or("").to_string(), axis }
}

/// One series as read: references plus formatting.
struct Ser {
    name_ref: Option<String>,
    name_lit: Option<String>,
    cat: Option<String>,
    val: Option<String>,
    fmt: SeriesFmt,
    labels: Option<Labels>,
    no_line: bool,
    has_marker: Option<bool>,
    smooth: bool,
}

fn formula(e: Option<&El>) -> Option<String> {
    let e = e?;
    ["numRef", "strRef", "multiLvlStrRef"].iter().find_map(|n| e.at(&[n, "f"])).map(|f| f.text.clone())
}

fn series_of(s: &El, kind: ChartKind) -> Ser {
    let (fill, line, width, no_line, dash) = shape_props(s.child("spPr"));
    let lined = matches!(kind, ChartKind::Line | ChartKind::Scatter | ChartKind::Radar);
    let mut f = SeriesFmt { dash, ..Default::default() };
    if lined {
        f.line = line;
        f.width = width.filter(|w| (w - 2.25).abs() > 0.01);
    } else {
        f.fill = fill;
        f.line = line;
        f.width = width.filter(|_| line.is_some());
    }
    let mut has_marker = None;
    if let Some(m) = s.child("marker") {
        let sym = m.cval("symbol");
        has_marker = Some(sym != Some("none"));
        f.marker = match sym {
            Some("none") => MarkerKind::None,
            Some("circle") => MarkerKind::Circle,
            Some("square") => MarkerKind::Square,
            Some("diamond") => MarkerKind::Diamond,
            Some("triangle") => MarkerKind::Triangle,
            Some("x") => MarkerKind::X,
            Some("plus") => MarkerKind::Plus,
            Some("star") => MarkerKind::Star,
            Some("dash") => MarkerKind::Dash,
            _ => MarkerKind::Auto,
        };
        f.marker_size = m.cnum("size").map(|v| v.clamp(2.0, 72.0) as u8).filter(|v| *v != 5).unwrap_or(0);
        if let (true, Some(c)) = (lined, m.child("spPr").and_then(|sp| sp.child("solidFill")).and_then(color_of)) {
            f.fill = Some(c);
        }
    }
    f.explode = s.cnum("explosion").map(|v| v.max(0.0) as u32).unwrap_or(0);
    for p in s.all("dPt") {
        let idx = p.cnum("idx").unwrap_or(0.0) as u32;
        let explode = p.cnum("explosion").map(|v| v.max(0.0) as u32).unwrap_or(0);
        let fill = p.child("spPr").and_then(|sp| sp.child("solidFill")).and_then(color_of);
        if explode > 0 || fill.is_some() {
            f.points.push(PointFmt { idx, fill, explode });
        }
    }
    if let Some(t) = s.child("trendline") {
        let kind = match t.cval("trendlineType") {
            Some("exp") => TrendKind::Exponential,
            Some("log") => TrendKind::Logarithmic,
            Some("poly") => TrendKind::Polynomial,
            Some("power") => TrendKind::Power,
            Some("movingAvg") => TrendKind::MovingAverage,
            _ => TrendKind::Linear,
        };
        f.trend = Some(Trendline {
            kind,
            order: t.cnum("order").unwrap_or(2.0).clamp(2.0, 6.0) as u8,
            period: t.cnum("period").unwrap_or(2.0).clamp(2.0, 255.0) as u8,
            forward: t.cnum("forward").unwrap_or(0.0),
            backward: t.cnum("backward").unwrap_or(0.0),
            intercept: t.cnum("intercept"),
            show_eq: t.flag("dispEq").unwrap_or(false),
            show_r2: t.flag("dispRSqr").unwrap_or(false),
            name: t.child("name").map(|n| n.text.clone()),
        });
    }
    if let Some(e) = s.all("errBars").find(|e| e.cval("errDir") != Some("x") || kind == ChartKind::Bar) {
        let amount = e.cnum("val").unwrap_or(0.0);
        let kind = match e.cval("errValType") {
            Some("fixedVal") => ErrKind::Fixed,
            Some("stdDev") => ErrKind::StdDev,
            Some("stdErr") => ErrKind::StdErr,
            _ => ErrKind::Percent,
        };
        let dir = match e.cval("errBarType") {
            Some("plus") => ErrDir::Plus,
            Some("minus") => ErrDir::Minus,
            _ => ErrDir::Both,
        };
        f.err = Some(ErrorBars { kind, amount, dir, cap: e.flag("noEndCap") != Some(true) });
    }
    let tx = s.child("tx");
    Ser {
        name_ref: tx.and_then(|t| t.at(&["strRef", "f"])).map(|f| f.text.clone()),
        name_lit: tx.and_then(|t| t.child("v")).map(|v| v.text.clone()),
        cat: formula(s.child("cat").or(s.child("xVal"))),
        val: formula(s.child("val").or(s.child("yVal"))),
        labels: s.child("dLbls").and_then(labels_of),
        fmt: f,
        no_line,
        has_marker,
        smooth: s.flag("smooth").unwrap_or(false),
    }
}

struct Group {
    kind: ChartKind,
    grouping: Grouping,
    secondary: bool,
    series: Vec<Ser>,
}

fn parse_chart(xml: &str, anchor: &Anchor, sheet: &Sheet) -> Option<Chart> {
    let root = dom(xml)?;
    let c = root.child("chart")?;
    let plot = c.child("plotArea")?;
    let axes: Vec<AxisInfo> = plot.kids.iter().filter(|k| matches!(k.name.as_str(), "catAx" | "valAx" | "dateAx" | "serAx")).map(axis_of).collect();

    // Chart groups.
    let mut groups: Vec<Group> = Vec::new();
    let mut first_ids: Vec<String> = Vec::new();
    let mut main_el: Option<&El> = None;
    let mut group_labels: Option<Labels> = None;
    for g in &plot.kids {
        let kind = match g.name.as_str() {
            "barChart" | "bar3DChart" => {
                if g.cval("barDir") == Some("bar") {
                    ChartKind::Bar
                } else {
                    ChartKind::Column
                }
            }
            "lineChart" | "line3DChart" | "stockChart" => ChartKind::Line,
            "pieChart" | "pie3DChart" | "ofPieChart" => ChartKind::Pie,
            "doughnutChart" => ChartKind::Doughnut,
            "areaChart" | "area3DChart" => ChartKind::Area,
            "scatterChart" | "bubbleChart" => ChartKind::Scatter,
            "radarChart" => ChartKind::Radar,
            _ => continue,
        };
        let ids: Vec<String> = g.all("axId").filter_map(|a| a.val().map(str::to_string)).collect();
        if groups.is_empty() {
            first_ids = ids.clone();
            main_el = Some(g);
            group_labels = g.child("dLbls").and_then(labels_of);
        }
        let grouping = match g.cval("grouping") {
            Some("stacked") => Grouping::Stacked,
            Some("percentStacked") => Grouping::Percent,
            _ => Grouping::Standard,
        };
        let secondary = !groups.is_empty() && !ids.is_empty() && ids != first_ids;
        let mut series: Vec<(f64, Ser)> = g.all("ser").map(|s| (s.cnum("order").unwrap_or(0.0), series_of(s, kind))).collect();
        series.sort_by(|a, b| a.0.total_cmp(&b.0));
        groups.push(Group { kind, grouping, secondary, series: series.into_iter().map(|x| x.1).collect() });
    }
    let main = groups.first()?;
    let kind = main.kind;
    let main_el = main_el?;
    let ax_by_id = |id: &str| axes.iter().find(|a| a.id == id);
    // Value and category axes of the primary group.
    let (mut x_axis, mut y_axis) = (Axis::default(), Axis { major_grid: false, ..Default::default() });
    let mut y2_axis = Axis::default();
    let val_of = |ids: &[String]| -> (Option<&AxisInfo>, Option<&AxisInfo>) {
        let infos: Vec<&AxisInfo> = ids.iter().filter_map(|i| ax_by_id(i)).collect();
        let val = if kind == ChartKind::Scatter {
            infos.iter().copied().find(|a| a.tag == "valAx" && matches!(a.pos.as_str(), "l" | "r")).or(infos.get(1).copied())
        } else {
            infos.iter().copied().find(|a| a.tag == "valAx")
        };
        let cat = infos.iter().copied().find(|a| !std::ptr::eq(*a, val.map_or(std::ptr::null(), |v| v as *const _)));
        (cat, val)
    };
    let (cat, val) = val_of(&first_ids);
    if let Some(a) = cat {
        x_axis = a.axis.clone();
    }
    if let Some(a) = val {
        y_axis = a.axis.clone();
    }
    if let Some(g2) = plot.kids.iter().filter(|k| k.name.ends_with("Chart")).find(|g| {
        let ids: Vec<String> = g.all("axId").filter_map(|a| a.val().map(str::to_string)).collect();
        !ids.is_empty() && ids != first_ids
    }) {
        let ids: Vec<String> = g2.all("axId").filter_map(|a| a.val().map(str::to_string)).collect();
        if let (_, Some(v)) = val_of(&ids) {
            y2_axis = v.axis.clone();
        }
    }

    // Flatten the series in group order.
    let mut sers: Vec<(ChartKind, Grouping, bool, Ser)> = Vec::new();
    for g in groups {
        for s in g.series {
            sers.push((g.kind, g.grouping, g.secondary, s));
        }
    }
    let (src, _) = parse_ref(sers.iter().find_map(|s| s.3.val.as_deref())?)?;
    let same = |s: &str| s.eq_ignore_ascii_case(&src);
    sers.retain(|s| s.3.val.as_deref().and_then(parse_ref).is_some_and(|(sh, _)| same(&sh)));
    let refs: Vec<(Range, Option<Range>, Option<(u32, u32)>)> = sers
        .iter()
        .map(|s| {
            let v = parse_ref(s.3.val.as_deref().unwrap_or("")).map(|x| x.1).unwrap_or_default();
            let c = s.3.cat.as_deref().and_then(parse_ref).filter(|(sh, _)| same(sh)).map(|x| x.1);
            let n = s.3.name_ref.as_deref().and_then(parse_ref).filter(|(sh, _)| same(sh)).map(|x| (x.1.r1, x.1.c1));
            (v, c, n)
        })
        .collect();
    let first = refs.first()?.0;
    let mut range = first;
    for (v, c, n) in &refs {
        range = range.union(v);
        if let Some(c) = c {
            range = range.union(c);
        }
        if let Some((r, cc)) = n {
            range = range.union(&Range::cell(*r, *cc));
        }
    }
    let names = refs.iter().any(|r| r.2.is_some());
    let cats = refs.iter().any(|r| r.1.is_some());
    let cat_rg = refs.iter().find_map(|r| r.1);
    let by_cols = if first.c1 == first.c2 && first.r2 > first.r1 {
        true
    } else if first.r1 == first.r2 && first.c2 > first.c1 {
        false
    } else {
        cat_rg.is_none_or(|c: Range| c.c1 == c.c2)
    };
    // Is it the regular block the automatic split would produce?
    let regular = {
        let (n0, c0) = (u32::from(names), u32::from(cats));
        let lines = if by_cols { range.cols() } else { range.rows() };
        lines == c0 + refs.len() as u32
            && sers.iter().all(|s| s.3.name_lit.is_none() || s.3.name_ref.is_some())
            && refs.iter().enumerate().all(|(k, (v, c, n))| {
                let l = c0 + k as u32;
                let (exp_v, exp_c, exp_n) = if by_cols {
                    (Range::new(range.r1 + n0, range.c1 + l, range.r2, range.c1 + l), Range::new(range.r1 + n0, range.c1, range.r2, range.c1), (range.r1, range.c1 + l))
                } else {
                    (Range::new(range.r1 + l, range.c1 + n0, range.r1 + l, range.c2), Range::new(range.r1, range.c1 + n0, range.r1, range.c2), (range.r1 + l, range.c1))
                };
                *v == exp_v && (!cats || *c == Some(exp_c)) && (!names || *n == Some(exp_n))
            })
    };
    let custom = (!regular).then(|| Custom {
        series: sers
            .iter()
            .zip(&refs)
            .map(|(s, (v, c, n))| SeriesSrc {
                name_cell: *n,
                name: if n.is_none() { s.3.name_lit.clone().unwrap_or_default() } else { String::new() },
                values: Some(*v),
                xs: if kind == ChartKind::Scatter { *c } else { None },
            })
            .collect(),
        cats: if kind == ChartKind::Scatter { None } else { cat_rg },
    });

    // Title.
    let title = match c.child("title") {
        Some(t) => text_of(t),
        None if c.flag("autoTitleDeleted") == Some(true) || sers.len() > 1 => Some(String::new()),
        None => None,
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
    let mut ch = Chart::blank(kind, &src, range);
    ch.by_cols = by_cols;
    ch.names_first = Some(names);
    ch.cats_first = Some(cats);
    ch.custom = custom;
    ch.title = title;
    ch.grouping = sers.first().map(|s| s.1).unwrap_or_default();
    ch.x_axis = x_axis;
    ch.y_axis = y_axis;
    ch.y2_axis = y2_axis;
    if let Some(l) = c.child("legend") {
        ch.legend = true;
        ch.legend_pos = match l.cval("legendPos") {
            Some("t") => LegendPos::Top,
            Some("l") => LegendPos::Left,
            Some("r") => LegendPos::Right,
            Some("tr") => LegendPos::TopRight,
            _ => LegendPos::Bottom,
        };
    }
    ch.data_table = plot.child("dTable").is_some();
    ch.bg = root.child("spPr").and_then(|sp| sp.child("solidFill")).and_then(color_of).filter(|c| *c != 0xFFFFFF);
    ch.plot_bg = plot.child("spPr").and_then(|sp| sp.child("solidFill")).and_then(color_of);
    if let Some(g) = main_el.cnum("gapWidth") {
        ch.gap = g.clamp(0.0, 500.0) as u16;
    } else if kind.is_bar() {
        ch.gap = 150;
    }
    if let Some(o) = main_el.cnum("overlap") {
        ch.overlap = o.clamp(-100.0, 100.0) as i16;
    } else if kind.is_bar() {
        ch.overlap = if ch.grouping == Grouping::Standard { 0 } else { 100 };
    }
    if let Some(hs) = main_el.cnum("holeSize") {
        ch.hole = hs.clamp(10.0, 90.0) as u8;
    }
    ch.first_angle = main_el.cnum("firstSliceAng").unwrap_or(0.0).clamp(0.0, 360.0) as u16;
    ch.smooth = main_el.cval("scatterStyle").is_some_and(|s| s.starts_with("smooth")) || sers.iter().any(|s| s.0 == ChartKind::Line && s.3.smooth);
    ch.filled = main_el.cval("radarStyle") == Some("filled");
    match kind {
        ChartKind::Line | ChartKind::Radar => {
            let line_sers: Vec<&Ser> = sers.iter().filter(|s| s.0 == kind).map(|s| &s.3).collect();
            ch.markers = !ch.filled && line_sers.iter().any(|s| s.has_marker.unwrap_or(kind == ChartKind::Line || main_el.cval("radarStyle") == Some("marker")));
        }
        ChartKind::Scatter => {
            ch.lines = sers.iter().any(|s| !s.3.no_line);
            ch.markers = sers.iter().any(|s| s.3.has_marker != Some(false));
        }
        _ => {}
    }

    // Labels: shared ones on the chart, differing ones per series.
    let all: Vec<Option<Labels>> = sers.iter().map(|s| s.3.labels.clone().or(group_labels.clone())).collect();
    let shared = match all.first() {
        Some(Some(l)) if all.iter().all(|x| x.as_ref() == Some(l)) => l.clone(),
        _ => group_labels.unwrap_or_default(),
    };
    ch.labels = shared.clone();
    for (k, (sk, _, sec, s)) in sers.iter().enumerate() {
        let mut f = s.fmt.clone();
        f.kind = (*sk != kind).then_some(*sk);
        f.secondary = *sec;
        if let Some(l) = &all[k] {
            if *l != shared {
                f.labels = Some(l.clone());
            }
        }
        // Explicit colours equal to the automatic ones stay automatic.
        if f.fill == Some(palette_color(0, k)) {
            f.fill = None;
        }
        if f.line == Some(palette_color(0, k)) {
            f.line = None;
        }
        if f.marker == MarkerKind::Circle && ch.markers {
            f.marker = MarkerKind::Auto;
        }
        if f.marker == MarkerKind::None && !ch.markers {
            f.marker = MarkerKind::Auto;
        }
        if kind.is_round() {
            f.points.retain(|p| p.explode > 0 || p.fill.is_some_and(|c| c != palette_color(0, p.idx as usize)));
            for p in &mut f.points {
                if p.fill == Some(palette_color(0, p.idx as usize)) {
                    p.fill = None;
                }
            }
        }
        if f != SeriesFmt::default() {
            *ch.fmt_mut(k) = f;
        }
    }
    ch.anchor = (from.row, from.col);
    ch.offset = (from.col_off, from.row_off);
    ch.size = (w.max(MIN_SIZE), h.max(MIN_SIZE));
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

    #[test]
    fn element_tree() {
        let el = dom("<c:a x=\"1\"><c:b val=\"2\"/><c:t>A &amp; B</c:t></c:a>").unwrap();
        assert_eq!(el.name, "a");
        assert_eq!(el.attr("x"), Some("1"));
        assert_eq!(el.cnum("b"), Some(2.0));
        assert_eq!(el.child("t").unwrap().text, "A & B");
    }
}
