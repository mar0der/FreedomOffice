//! Streaming .xlsx/.xlsm reader (zip + quick-xml) that keeps formatting.

use std::collections::HashMap;
use std::io::{Read, Seek};

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::formula;
use crate::model::{
    AutoFilter, Border, BorderLine, Cell, ErrorKind, Formula, HAlign, Range, Sheet, Style, VAlign, Value, Workbook,
    DEFAULT_COL_W, DEFAULT_ROW_H, MAX_COLS, MAX_ROWS,
};
use crate::numfmt;

type XmlReader<'a> = Reader<&'a [u8]>;

pub fn read<R: Read + Seek>(src: R) -> Result<Workbook, String> {
    let mut zip = zip::ZipArchive::new(src).map_err(|e| format!("Not a valid xlsx file: {e}"))?;
    let mut pkg = Package::new(&mut zip);

    // Locate the workbook part through the package relationships.
    let wb_path = pkg
        .text("_rels/.rels")
        .and_then(|x| {
            parse_rels(&x).into_iter().find(|r| r.ty.ends_with("/officeDocument")).map(|r| resolve("", &r.target))
        })
        .unwrap_or_else(|| "xl/workbook.xml".to_string());
    let wb_dir = dir_of(&wb_path);
    let wb_xml = pkg.text(&wb_path).ok_or("The file has no workbook part")?;
    let rels_path = format!("{}_rels/{}.rels", wb_dir, file_of(&wb_path));
    let rels = pkg.text(&rels_path).map(|x| parse_rels(&x)).unwrap_or_default();
    let rel_target = |suffix: &str, default: &str| {
        rels.iter()
            .find(|r| r.ty.ends_with(suffix))
            .map(|r| resolve(&wb_dir, &r.target))
            .unwrap_or_else(|| format!("{wb_dir}{default}"))
    };

    let info = parse_workbook(&wb_xml);
    let theme = pkg.text(&rel_target("/theme", "theme/theme1.xml")).map(|x| parse_theme(&x)).unwrap_or_else(default_theme);
    let shared = pkg.text(&rel_target("/sharedStrings", "sharedStrings.xml")).map(|x| parse_shared_strings(&x)).unwrap_or_default();
    let styles_xml = pkg.text(&rel_target("/styles", "styles.xml"));

    let mut wb = Workbook::empty_with(Vec::new());
    let xf = match &styles_xml {
        Some(x) => parse_styles(x, &theme, &mut wb),
        None => XfMap { ids: vec![0], date: vec![false] },
    };

    let mut active = 0;
    for (i, sh) in info.sheets.iter().enumerate() {
        let rel = rels.iter().find(|r| r.id == sh.rid);
        if rel.is_some_and(|r| !r.ty.ends_with("/worksheet")) {
            continue; // chartsheets, dialog sheets, macro sheets
        }
        let path = match rel {
            Some(r) => resolve(&wb_dir, &r.target),
            None => format!("{wb_dir}worksheets/sheet{}.xml", i + 1),
        };
        if i == info.active {
            active = wb.sheets.len();
        }
        let mut sheet = Sheet::new(&sh.name);
        if let Some(x) = pkg.text(&path) {
            let ctx = SheetCtx { shared: &shared, xf: &xf, theme: &theme, date1904: info.date1904 };
            parse_sheet(&x, &ctx, &mut sheet);
        }
        wb.sheets.push(sheet);
    }
    if wb.sheets.is_empty() {
        wb.sheets.push(Sheet::new("Sheet1"));
    }
    wb.active = active.min(wb.sheets.len() - 1);
    wb.names = info.names;
    Ok(wb)
}

// ---------------------------------------------------------------------------
// Package access
// ---------------------------------------------------------------------------

struct Package<'a, R: Read + Seek> {
    zip: &'a mut zip::ZipArchive<R>,
    /// lowercase name -> actual entry name
    names: HashMap<String, String>,
}

impl<'a, R: Read + Seek> Package<'a, R> {
    fn new(zip: &'a mut zip::ZipArchive<R>) -> Self {
        let names = zip
            .file_names()
            .filter_map(|n| n.ok())
            .map(|n| (n.trim_start_matches('/').to_ascii_lowercase(), n.into_owned()))
            .collect();
        Package { zip, names }
    }

    fn text(&mut self, path: &str) -> Option<String> {
        let key = path.trim_start_matches('/').replace('\\', "/").to_ascii_lowercase();
        let name = self.names.get(&key)?.clone();
        let mut f = self.zip.by_name(&name).ok()?;
        let mut buf = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut buf).ok()?;
        let s = match String::from_utf8(buf) {
            Ok(s) => s,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        };
        Some(s.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(s))
    }
}

fn dir_of(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..=i].to_string(),
        None => String::new(),
    }
}

fn file_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Resolves a relationship target against the directory of the source part.
fn resolve(base_dir: &str, target: &str) -> String {
    let target = target.replace('\\', "/");
    let joined = if let Some(abs) = target.strip_prefix('/') { abs.to_string() } else { format!("{base_dir}{target}") };
    let mut parts: Vec<&str> = Vec::new();
    for p in joined.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(p),
        }
    }
    parts.join("/")
}

struct Rel {
    id: String,
    ty: String,
    target: String,
}

fn parse_rels(xml: &str) -> Vec<Rel> {
    let mut out = Vec::new();
    let mut rd = Reader::from_str(xml);
    loop {
        match rd.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if local(&e) == "Relationship" => {
                let mut rel = Rel { id: String::new(), ty: String::new(), target: String::new() };
                for (k, v) in attrs(&e) {
                    match k.as_str() {
                        "Id" => rel.id = v,
                        "Type" => rel.ty = v,
                        "Target" => rel.target = v,
                        _ => {}
                    }
                }
                out.push(rel);
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// XML helpers
// ---------------------------------------------------------------------------

fn local<'b>(e: &'b BytesStart) -> &'b str {
    let n: &str = e.name().0;
    n.rsplit(':').next().unwrap_or(n)
}

fn unescape(raw: &str) -> String {
    if raw.contains('&') {
        quick_xml::escape::unescape(raw).map(|c| c.into_owned()).unwrap_or_else(|_| raw.to_string())
    } else {
        raw.to_string()
    }
}

/// All attributes as (local name, unescaped value).
fn attrs(e: &BytesStart) -> Vec<(String, String)> {
    e.attributes()
        .with_checks(false)
        .filter_map(|a| a.ok())
        .map(|a| {
            let key: &str = a.key.0;
            let key = key.rsplit(':').next().unwrap_or(key).to_string();
            (key, unescape(&a.value))
        })
        .collect()
}

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes().with_checks(false).filter_map(|a| a.ok()).find_map(|a| {
        let key: &str = a.key.0;
        (key.rsplit(':').next() == Some(name)).then(|| unescape(&a.value))
    })
}

fn attr_bool(e: &BytesStart, name: &str) -> Option<bool> {
    attr(e, name).map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Boolean flag element such as `<b/>` or `<b val="0"/>`.
fn flag(e: &BytesStart) -> bool {
    attr_bool(e, "val").unwrap_or(true)
}

/// Collects the text content of the current element up to its end tag.
fn read_text(rd: &mut XmlReader, end: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    loop {
        match rd.read_event() {
            Ok(Event::Text(t)) => out.push_str(&t.xml10_content()),
            Ok(Event::CData(t)) => out.push_str(&t.xml10_content()),
            Ok(Event::GeneralRef(r)) => {
                if r.is_char_ref() {
                    if let Ok(Some(c)) = r.resolve_char_ref() {
                        out.push(c);
                    }
                } else if let Some(s) = quick_xml::escape::resolve_predefined_entity(&r) {
                    out.push_str(s);
                }
            }
            Ok(Event::Start(e)) => {
                if local(&e) == end {
                    depth += 1;
                }
            }
            Ok(Event::End(e)) => {
                let n: &str = e.name().0;
                if n.rsplit(':').next() == Some(end) {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

fn skip(rd: &mut XmlReader, e: &BytesStart) {
    let _ = rd.read_to_end(e.name());
}

/// Decodes OOXML `_xHHHH_` escapes.
fn decode_ooxml_escapes(s: String) -> String {
    if !s.contains("_x") {
        return s;
    }
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if b[i] == b'_' && i + 7 <= s.len() && b[i + 1] == b'x' && b[i + 6] == b'_' {
            if let Ok(code) = u32::from_str_radix(&s[i + 2..i + 6], 16) {
                if let Some(c) = char::from_u32(code) {
                    out.push(c);
                    i += 7;
                    continue;
                }
            }
        }
        let c = s[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Fast "AB12" -> (row, col), 0-based.
fn parse_ref(s: &str) -> Option<(u32, u32)> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut col: u32 = 0;
    while i < b.len() && (b[i].is_ascii_alphabetic() || b[i] == b'$') {
        if b[i] != b'$' {
            col = col * 26 + (b[i].to_ascii_uppercase() - b'A' + 1) as u32;
        }
        i += 1;
    }
    let mut row: u32 = 0;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'$') {
        if b[i] != b'$' {
            row = row.checked_mul(10)?.checked_add((b[i] - b'0') as u32)?;
        }
        i += 1;
    }
    if i != b.len() || col == 0 || row == 0 || col > MAX_COLS || row > MAX_ROWS {
        return None;
    }
    Some((row - 1, col - 1))
}

fn parse_f(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

// ---------------------------------------------------------------------------
// Workbook part
// ---------------------------------------------------------------------------

struct SheetEntry {
    name: String,
    rid: String,
}

#[derive(Default)]
struct WorkbookInfo {
    sheets: Vec<SheetEntry>,
    names: Vec<(String, String)>,
    active: usize,
    date1904: bool,
}

fn parse_workbook(xml: &str) -> WorkbookInfo {
    let mut info = WorkbookInfo::default();
    let mut rd = Reader::from_str(xml);
    loop {
        match rd.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match local(&e) {
                "sheet" => {
                    let name = attr(&e, "name").unwrap_or_else(|| format!("Sheet{}", info.sheets.len() + 1));
                    let rid = attr(&e, "id").unwrap_or_default();
                    info.sheets.push(SheetEntry { name, rid });
                }
                "workbookView" => {
                    if let Some(t) = attr(&e, "activeTab").and_then(|v| v.parse().ok()) {
                        info.active = t;
                    }
                }
                "workbookPr" => info.date1904 = attr_bool(&e, "date1904").unwrap_or(false),
                "definedName" => {
                    let name = attr(&e, "name").unwrap_or_default();
                    let text = read_text(&mut rd, "definedName");
                    let text = strip_future_prefixes(text.trim().trim_start_matches('='));
                    if !name.is_empty()
                        && !name.starts_with("_xlnm.")
                        && !text.is_empty()
                        && !info.names.iter().any(|(n, _)| n.eq_ignore_ascii_case(&name))
                    {
                        info.names.push((name, text));
                    }
                }
                "extLst" | "AlternateContent" => skip(&mut rd, &e),
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    info
}

/// Removes `_xlfn.` / `_xlws.` / `_xlpm.` markers from formula text.
fn strip_future_prefixes(s: &str) -> String {
    if !s.contains("_xl") {
        return s.to_string();
    }
    s.replace("_xlfn.", "").replace("_xlws.", "").replace("_xlpm.", "")
}

// ---------------------------------------------------------------------------
// Shared strings
// ---------------------------------------------------------------------------

fn parse_shared_strings(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rd = Reader::from_str(xml);
    loop {
        match rd.read_event() {
            Ok(Event::Start(e)) if local(&e) == "si" => out.push(read_rich_text(&mut rd, "si")),
            Ok(Event::Empty(e)) if local(&e) == "si" => out.push(String::new()),
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// Reads `<si>` / `<is>` content: plain `<t>` or rich-text runs, skipping phonetic runs.
fn read_rich_text(rd: &mut XmlReader, end: &str) -> String {
    let mut s = String::new();
    loop {
        match rd.read_event() {
            Ok(Event::Start(e)) => match local(&e) {
                "t" => s.push_str(&read_text(rd, "t")),
                "rPh" | "phoneticPr" => skip(rd, &e),
                _ => {}
            },
            Ok(Event::End(e)) => {
                let n: &str = e.name().0;
                if n.rsplit(':').next() == Some(end) {
                    break;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    decode_ooxml_escapes(s)
}

// ---------------------------------------------------------------------------
// Colors and theme
// ---------------------------------------------------------------------------

/// Theme colors in the order used by `theme="n"` attributes:
/// lt1, dk1, lt2, dk2, accent1..6, hlink, folHlink.
pub(crate) struct Theme {
    colors: Vec<u32>,
}

fn default_theme() -> Theme {
    Theme {
        colors: vec![
            0xFFFFFF, 0x000000, 0xE7E6E6, 0x44546A, 0x4472C4, 0xED7D31, 0xA5A5A5, 0xFFC000, 0x5B9BD5, 0x70AD47,
            0x0563C1, 0x954F72,
        ],
    }
}

fn parse_theme(xml: &str) -> Theme {
    let order = [
        "dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6", "hlink",
        "folHlink",
    ];
    let mut file: HashMap<&str, u32> = HashMap::new();
    let mut rd = Reader::from_str(xml);
    let mut in_scheme = false;
    let mut current: Option<&str> = None;
    loop {
        match rd.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let n = local(&e);
                if n == "clrScheme" {
                    in_scheme = true;
                } else if in_scheme {
                    if let Some(k) = order.iter().find(|k| **k == n) {
                        current = Some(k);
                    } else if let Some(k) = current {
                        let val = match n {
                            "srgbClr" => attr(&e, "val"),
                            "sysClr" => attr(&e, "lastClr").or_else(|| {
                                attr(&e, "val").map(|v| if v == "window" { "FFFFFF".into() } else { "000000".into() })
                            }),
                            _ => None,
                        };
                        if let Some(c) = val.and_then(|v| u32::from_str_radix(v.trim(), 16).ok()) {
                            file.entry(k).or_insert(c & 0xFFFFFF);
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                let n: &str = e.name().0;
                if n.rsplit(':').next() == Some("clrScheme") {
                    break;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    let def = default_theme();
    let get = |k: &str, i: usize| file.get(k).copied().unwrap_or(def.colors[i]);
    let mut colors = vec![get("lt1", 0), get("dk1", 1), get("lt2", 2), get("dk2", 3)];
    for (i, k) in order.iter().enumerate().skip(4) {
        colors.push(get(k, i));
    }
    Theme { colors }
}

const INDEXED: [u32; 64] = [
    0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x000000, 0xFFFFFF, 0xFF0000,
    0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x800000, 0x008000, 0x000080, 0x808000, 0x800080, 0x008080,
    0xC0C0C0, 0x808080, 0x9999FF, 0x993366, 0xFFFFCC, 0xCCFFFF, 0x660066, 0xFF8080, 0x0066CC, 0xCCCCFF, 0x000080,
    0xFF00FF, 0xFFFF00, 0x00FFFF, 0x800080, 0x800000, 0x008080, 0x0000FF, 0x00CCFF, 0xCCFFFF, 0xCCFFCC, 0xFFFF99,
    0x99CCFF, 0xFF99CC, 0xCC99FF, 0xFFCC99, 0x3366FF, 0x33CCCC, 0x99CC00, 0xFFCC00, 0xFF9900, 0xFF6600, 0x666699,
    0x969696, 0x003366, 0x339966, 0x003300, 0x333300, 0x993300, 0x993366, 0x333399, 0x333333,
];

/// Applies an OOXML tint (-1..1) to an RGB color by scaling HLS luminance, using the
/// integer HLS model (HLSMAX = 240) that Excel uses so results match its palette.
pub(crate) fn apply_tint(rgb: u32, tint: f64) -> u32 {
    const HLS: i64 = 240;
    const RGB: i64 = 255;
    if tint == 0.0 {
        return rgb;
    }
    let (r, g, b) = (((rgb >> 16) & 0xFF) as i64, ((rgb >> 8) & 0xFF) as i64, (rgb & 0xFF) as i64);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = ((max + min) * HLS + RGB) / (2 * RGB);
    let mut h = 0i64;
    let s = if max == min {
        0
    } else {
        let d = max - min;
        let s = if l <= HLS / 2 {
            (d * HLS + (max + min) / 2) / (max + min)
        } else {
            (d * HLS + (2 * RGB - max - min) / 2) / (2 * RGB - max - min)
        };
        let delta = |c: i64| ((max - c) * (HLS / 6) + d / 2) / d;
        h = if r == max {
            delta(b) - delta(g)
        } else if g == max {
            HLS / 3 + delta(r) - delta(b)
        } else {
            2 * HLS / 3 + delta(g) - delta(r)
        };
        h = h.rem_euclid(HLS);
        s
    };
    let lf = l as f64;
    let l = if tint < 0.0 { lf * (1.0 + tint) } else { lf * (1.0 - tint) + HLS as f64 * tint };
    let l = (l.trunc() as i64).clamp(0, HLS);
    let (r, g, b) = if s == 0 {
        let v = (l * RGB + HLS / 2) / HLS;
        (v, v, v)
    } else {
        let m2 = if l <= HLS / 2 { (l * (HLS + s) + HLS / 2) / HLS } else { l + s - (l * s + HLS / 2) / HLS };
        let m1 = 2 * l - m2;
        let hue = |mut t: i64| {
            t = t.rem_euclid(HLS);
            let v = if t < HLS / 6 {
                m1 + ((m2 - m1) * t + HLS / 12) / (HLS / 6)
            } else if t < HLS / 2 {
                m2
            } else if t < HLS * 2 / 3 {
                m1 + ((m2 - m1) * (HLS * 2 / 3 - t) + HLS / 12) / (HLS / 6)
            } else {
                m1
            };
            (v * RGB + HLS / 2) / HLS
        };
        (hue(h + HLS / 3), hue(h), hue(h - HLS / 3))
    };
    let c = |v: i64| v.clamp(0, 255) as u32;
    (c(r) << 16) | (c(g) << 8) | c(b)
}

/// Resolves a CT_Color element (`rgb`, `theme`+`tint`, `indexed`, `auto`).
fn parse_color(e: &BytesStart, theme: &Theme, palette: &[u32]) -> Option<u32> {
    let mut rgb = None;
    let mut th = None;
    let mut idx = None;
    let mut tint = 0.0;
    for (k, v) in attrs(e) {
        match k.as_str() {
            "rgb" => {
                let v = v.trim();
                let v = if v.len() > 6 { &v[v.len() - 6..] } else { v };
                rgb = u32::from_str_radix(v, 16).ok();
            }
            "theme" => th = v.parse::<usize>().ok(),
            "indexed" => idx = v.parse::<usize>().ok(),
            "tint" => tint = v.parse().unwrap_or(0.0),
            "auto" if v == "1" || v == "true" => return None,
            _ => {}
        }
    }
    let base = rgb.or_else(|| th.and_then(|t| theme.colors.get(t).copied())).or_else(|| idx.and_then(|i| palette.get(i).copied()))?;
    Some(apply_tint(base, tint))
}

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------

/// xf index -> (model style id, is date format).
pub(crate) struct XfMap {
    ids: Vec<u32>,
    date: Vec<bool>,
}

#[derive(Default, Clone)]
struct FontRec {
    name: Option<String>,
    size: f64,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    color: Option<u32>,
}

fn border_line(style: &str) -> BorderLine {
    match style {
        "" | "none" => BorderLine::None,
        "thin" | "hair" => BorderLine::Thin,
        "medium" => BorderLine::Medium,
        "thick" => BorderLine::Thick,
        "double" => BorderLine::Double,
        "dotted" => BorderLine::Dotted,
        _ => BorderLine::Dashed, // dashed, mediumDashed, dashDot, ...
    }
}

fn parse_styles(xml: &str, theme: &Theme, wb: &mut Workbook) -> XfMap {
    // Custom indexed palette first (rare, but it changes every indexed color).
    let mut palette: Vec<u32> = INDEXED.to_vec();
    {
        let mut custom = Vec::new();
        let mut rd = Reader::from_str(xml);
        let mut in_idx = false;
        loop {
            match rd.read_event() {
                Ok(Event::Start(e)) if local(&e) == "indexedColors" => in_idx = true,
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) if in_idx && local(&e) == "rgbColor" => {
                    let v = attr(&e, "rgb").unwrap_or_default();
                    let v = if v.len() > 6 { v[v.len() - 6..].to_string() } else { v };
                    custom.push(u32::from_str_radix(&v, 16).unwrap_or(0));
                }
                Ok(Event::End(e)) if e.name().0.ends_with("indexedColors") => break,
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
        for (i, c) in custom.into_iter().enumerate().take(64) {
            palette[i] = c;
        }
    }

    let mut num_fmts: HashMap<u32, String> = HashMap::new();
    let mut fonts: Vec<FontRec> = Vec::new();
    let mut fills: Vec<Option<u32>> = Vec::new();
    let mut borders: Vec<Border> = Vec::new();
    let mut map = XfMap { ids: Vec::new(), date: Vec::new() };

    let mut rd = Reader::from_str(xml);
    let mut section = "";
    loop {
        let ev = rd.read_event();
        match ev {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let is_start = matches!(ev, Ok(Event::Start(_)));
                match local(e) {
                    "numFmt" => {
                        if let (Some(id), Some(code)) = (attr(e, "numFmtId").and_then(|v| v.parse().ok()), attr(e, "formatCode")) {
                            num_fmts.insert(id, code);
                        }
                    }
                    "fonts" if is_start => section = "fonts",
                    "fills" if is_start => section = "fills",
                    "borders" if is_start => section = "borders",
                    "cellXfs" if is_start => section = "cellXfs",
                    "cellStyleXfs" | "dxfs" | "extLst" | "colors" | "cellStyles" | "tableStyles" if is_start => {
                        skip(&mut rd, e)
                    }
                    "font" if section == "fonts" => {
                        let mut f = FontRec { size: 11.0, ..Default::default() };
                        if is_start {
                            loop {
                                match rd.read_event() {
                                    Ok(Event::Start(c)) | Ok(Event::Empty(c)) => match local(&c) {
                                        "b" => f.bold = flag(&c),
                                        "i" => f.italic = flag(&c),
                                        "strike" => f.strike = flag(&c),
                                        "u" => f.underline = attr(&c, "val").map(|v| v != "none").unwrap_or(true),
                                        "sz" => f.size = attr(&c, "val").and_then(|v| parse_f(&v)).unwrap_or(11.0),
                                        "name" | "rFont" => f.name = attr(&c, "val"),
                                        "color" => f.color = parse_color(&c, theme, &palette),
                                        _ => {}
                                    },
                                    Ok(Event::End(c)) if c.name().0.rsplit(':').next() == Some("font") => break,
                                    Ok(Event::Eof) | Err(_) => break,
                                    _ => {}
                                }
                            }
                        }
                        fonts.push(f);
                    }
                    "fill" if section == "fills" => {
                        let mut fill = None;
                        if is_start {
                            let mut pattern = String::from("none");
                            let mut fg = None;
                            let mut bg = None;
                            let mut gradient = None;
                            loop {
                                match rd.read_event() {
                                    Ok(Event::Start(c)) | Ok(Event::Empty(c)) => match local(&c) {
                                        "patternFill" => pattern = attr(&c, "patternType").unwrap_or_else(|| "none".into()),
                                        "fgColor" => fg = parse_color(&c, theme, &palette),
                                        "bgColor" => bg = parse_color(&c, theme, &palette),
                                        "color" if gradient.is_none() => gradient = parse_color(&c, theme, &palette),
                                        _ => {}
                                    },
                                    Ok(Event::End(c)) if c.name().0.rsplit(':').next() == Some("fill") => break,
                                    Ok(Event::Eof) | Err(_) => break,
                                    _ => {}
                                }
                            }
                            fill = match pattern.as_str() {
                                "none" => gradient,
                                "solid" => fg.or(bg),
                                _ => fg.or(bg),
                            };
                        }
                        fills.push(fill);
                    }
                    "border" if section == "borders" => {
                        let mut b = Border::default();
                        if is_start {
                            loop {
                                match rd.read_event() {
                                    Ok(Event::Start(c)) | Ok(Event::Empty(c)) => {
                                        let line = border_line(&attr(&c, "style").unwrap_or_default());
                                        match local(&c) {
                                            "left" | "start" => b.left = line,
                                            "right" | "end" => b.right = line,
                                            "top" => b.top = line,
                                            "bottom" => b.bottom = line,
                                            _ => {}
                                        }
                                    }
                                    Ok(Event::End(c)) if c.name().0.rsplit(':').next() == Some("border") => break,
                                    Ok(Event::Eof) | Err(_) => break,
                                    _ => {}
                                }
                            }
                        }
                        borders.push(b);
                    }
                    "xf" if section == "cellXfs" => {
                        let mut st = Style::default();
                        let mut fmt_id = 0u32;
                        for (k, v) in attrs(e) {
                            match k.as_str() {
                                "numFmtId" => fmt_id = v.parse().unwrap_or(0),
                                "fontId" => {
                                    if let Some(f) = v.parse::<usize>().ok().and_then(|i| fonts.get(i)) {
                                        st.font = f.name.clone().filter(|n| !n.eq_ignore_ascii_case("Calibri"));
                                        if (f.size - 11.0).abs() > 0.01 {
                                            st.set_font_size(f.size as f32);
                                        }
                                        st.bold = f.bold;
                                        st.italic = f.italic;
                                        st.underline = f.underline;
                                        st.strike = f.strike;
                                        st.color = f.color.filter(|&c| c != 0x000000);
                                    }
                                }
                                "fillId" => st.fill = v.parse::<usize>().ok().and_then(|i| fills.get(i).copied().flatten()),
                                "borderId" => {
                                    st.border = v.parse::<usize>().ok().and_then(|i| borders.get(i).copied()).unwrap_or_default()
                                }
                                _ => {}
                            }
                        }
                        let code = num_fmts.get(&fmt_id).cloned().or_else(|| numfmt::builtin_format(fmt_id).map(str::to_string));
                        let code = match code {
                            Some(c) if !c.eq_ignore_ascii_case("general") => c,
                            Some(_) => String::new(),
                            // Locale-specific built-in date formats.
                            None if (27..=36).contains(&fmt_id) || (50..=58).contains(&fmt_id) => "m/d/yyyy".into(),
                            None => String::new(),
                        };
                        st.num_fmt = code;
                        if is_start {
                            loop {
                                match rd.read_event() {
                                    Ok(Event::Start(c)) | Ok(Event::Empty(c)) if local(&c) == "alignment" => {
                                        for (k, v) in attrs(&c) {
                                            match k.as_str() {
                                                "horizontal" => {
                                                    st.halign = match v.as_str() {
                                                        "left" | "justify" | "distributed" | "fill" => HAlign::Left,
                                                        "center" | "centerContinuous" => HAlign::Center,
                                                        "right" => HAlign::Right,
                                                        _ => HAlign::General,
                                                    }
                                                }
                                                "vertical" => {
                                                    st.valign = match v.as_str() {
                                                        "top" => VAlign::Top,
                                                        "center" | "justify" | "distributed" => VAlign::Center,
                                                        _ => VAlign::Bottom,
                                                    }
                                                }
                                                "wrapText" => st.wrap = v == "1" || v == "true",
                                                "indent" => st.indent = v.parse::<u32>().unwrap_or(0).min(250) as u8,
                                                _ => {}
                                            }
                                        }
                                    }
                                    Ok(Event::End(c)) if c.name().0.rsplit(':').next() == Some("xf") => break,
                                    Ok(Event::Eof) | Err(_) => break,
                                    _ => {}
                                }
                            }
                        }
                        map.date.push(numfmt::is_date_format(&st.num_fmt));
                        map.ids.push(wb.intern_style(st));
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let n: &str = e.name().0;
                if matches!(n.rsplit(':').next(), Some("fonts" | "fills" | "borders" | "cellXfs")) {
                    section = "";
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    if map.ids.is_empty() {
        map.ids.push(0);
        map.date.push(false);
    }
    map
}

// ---------------------------------------------------------------------------
// Worksheets
// ---------------------------------------------------------------------------

struct SheetCtx<'a> {
    shared: &'a [String],
    xf: &'a XfMap,
    theme: &'a Theme,
    date1904: bool,
}

/// Column width in characters (as stored in the file) to pixels for Calibri 11 (max digit width 7).
fn width_to_px(w: f64) -> f32 {
    (((256.0 * w + (128.0f64 / 7.0).trunc()) / 256.0) * 7.0).trunc().max(0.0) as f32
}

/// ISO 8601 "yyyy-mm-dd[Thh:mm:ss]" to a date serial.
fn iso_to_serial(s: &str) -> Option<f64> {
    let (d, t) = match s.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let mut it = d.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let dd: i64 = it.next()?.parse().ok()?;
    let mut serial = numfmt::ymd_to_serial(y, m, dd);
    if let Some(t) = t {
        let t = t.trim_end_matches('Z');
        let mut p = t.split(':');
        let h: f64 = p.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let mi: f64 = p.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let sec: f64 = p.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        serial += (h * 3600.0 + mi * 60.0 + sec) / 86400.0;
    }
    Some(serial)
}

#[derive(Default)]
struct CellBuf {
    row: u32,
    col: u32,
    style: usize,
    t: String,
    v: Option<String>,
    is: Option<String>,
    f_text: Option<String>,
    f_type: String,
    f_si: Option<String>,
    f_ref: Option<String>,
}

struct SharedMaster {
    row: u32,
    col: u32,
    ast: Option<formula::Expr>,
    text: String,
}

fn parse_sheet(xml: &str, ctx: &SheetCtx, sheet: &mut Sheet) {
    let mut rd = Reader::from_str(xml);
    let mut shared_f: HashMap<String, SharedMaster> = HashMap::new();
    let mut row: u32 = 0;
    let mut next_row: u32 = 0;
    let mut next_col: u32 = 0;
    let mut in_sheet_view = false;
    let mut seen_view = false;
    loop {
        let ev = rd.read_event();
        match ev {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let is_start = matches!(ev, Ok(Event::Start(_)));
                match local(e) {
                    "row" => {
                        let mut r = next_row;
                        let mut ht = None;
                        let mut hidden = false;
                        for (k, v) in attrs(e) {
                            match k.as_str() {
                                "r" => {
                                    if let Ok(n) = v.parse::<u32>() {
                                        if n >= 1 && n <= MAX_ROWS {
                                            r = n - 1;
                                        }
                                    }
                                }
                                "ht" => ht = parse_f(&v),
                                "hidden" => hidden = v == "1" || v == "true",
                                _ => {}
                            }
                        }
                        if let Some(pt) = ht {
                            let px = (pt * 96.0 / 72.0) as f32;
                            if (px - DEFAULT_ROW_H).abs() > 0.01 && px > 0.0 {
                                sheet.row_h.insert(r, px);
                            }
                        }
                        if hidden {
                            sheet.hidden_rows.insert(r);
                        }
                        row = r;
                        next_row = r + 1;
                        next_col = 0;
                    }
                    "c" => {
                        let mut cb = CellBuf { row, col: next_col, ..Default::default() };
                        for (k, v) in attrs(e) {
                            match k.as_str() {
                                "r" => {
                                    if let Some((r, c)) = parse_ref(&v) {
                                        cb.row = r;
                                        cb.col = c;
                                    }
                                }
                                "s" => cb.style = v.parse().unwrap_or(0),
                                "t" => cb.t = v,
                                _ => {}
                            }
                        }
                        next_col = cb.col + 1;
                        if is_start {
                            loop {
                                match rd.read_event() {
                                    Ok(Event::Start(c)) => match local(&c) {
                                        "v" => cb.v = Some(read_text(&mut rd, "v")),
                                        "f" => {
                                            for (k, v) in attrs(&c) {
                                                match k.as_str() {
                                                    "t" => cb.f_type = v,
                                                    "si" => cb.f_si = Some(v),
                                                    "ref" => cb.f_ref = Some(v),
                                                    _ => {}
                                                }
                                            }
                                            cb.f_text = Some(read_text(&mut rd, "f"));
                                        }
                                        "is" => cb.is = Some(read_rich_text(&mut rd, "is")),
                                        _ => skip(&mut rd, &c),
                                    },
                                    Ok(Event::Empty(c)) => {
                                        if local(&c) == "f" {
                                            for (k, v) in attrs(&c) {
                                                match k.as_str() {
                                                    "t" => cb.f_type = v,
                                                    "si" => cb.f_si = Some(v),
                                                    "ref" => cb.f_ref = Some(v),
                                                    _ => {}
                                                }
                                            }
                                            cb.f_text = Some(String::new());
                                        }
                                    }
                                    Ok(Event::End(_)) => break,
                                    Ok(Event::Eof) | Err(_) => break,
                                    _ => {}
                                }
                            }
                        }
                        finish_cell(cb, ctx, &mut shared_f, sheet);
                    }
                    "sheetView" => {
                        in_sheet_view = !seen_view && is_start;
                        if !seen_view {
                            if attr_bool(e, "showGridLines") == Some(false) {
                                sheet.gridlines = false;
                            }
                        }
                        seen_view = true;
                    }
                    "pane" if in_sheet_view => {
                        let state = attr(e, "state").unwrap_or_default();
                        if state == "frozen" || state == "frozenSplit" {
                            let xs = attr(e, "xSplit").and_then(|v| parse_f(&v)).unwrap_or(0.0);
                            let ys = attr(e, "ySplit").and_then(|v| parse_f(&v)).unwrap_or(0.0);
                            sheet.freeze = (ys.max(0.0) as u32, xs.max(0.0) as u32);
                        }
                    }
                    "selection" if in_sheet_view => {
                        if let Some((r, c)) = attr(e, "activeCell").and_then(|v| parse_ref(&v)) {
                            sheet.view.active = (r, c);
                            sheet.view.anchor = (r, c);
                            sheet.view.sel = Range::cell(r, c);
                        }
                    }
                    "tabColor" => sheet.tab_color = parse_color(e, ctx.theme, &INDEXED),
                    "col" => {
                        let min = attr(e, "min").and_then(|v| v.parse::<u32>().ok()).unwrap_or(1).max(1);
                        let max = attr(e, "max").and_then(|v| v.parse::<u32>().ok()).unwrap_or(min).clamp(min, MAX_COLS);
                        let hidden = attr_bool(e, "hidden").unwrap_or(false);
                        let px = attr(e, "width").and_then(|v| parse_f(&v)).map(width_to_px);
                        for c in (min - 1)..max {
                            if hidden {
                                sheet.hidden_cols.insert(c);
                            }
                            if let Some(px) = px {
                                if px > 0.0 && (px - DEFAULT_COL_W).abs() > 0.01 {
                                    sheet.col_w.insert(c, px);
                                }
                            }
                        }
                    }
                    "mergeCell" => {
                        if let Some(rg) = attr(e, "ref").and_then(|v| Range::parse(&v)) {
                            if !rg.is_single() {
                                sheet.merges.push(rg);
                            }
                        }
                    }
                    "autoFilter" => {
                        if let Some(rg) = attr(e, "ref").and_then(|v| Range::parse(&v)) {
                            sheet.filter = Some(AutoFilter { range: rg, excluded: HashMap::new() });
                        }
                        if is_start {
                            skip(&mut rd, e);
                        }
                    }
                    "extLst" | "AlternateContent" | "conditionalFormatting" | "dataValidations" | "hyperlinks"
                        if is_start =>
                    {
                        skip(&mut rd, e)
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let n: &str = e.name().0;
                if n.rsplit(':').next() == Some("sheetView") {
                    in_sheet_view = false;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    clear_array_results(sheet);
}

/// A formula from a file; `ANCHORARRAY(A1)` / `SINGLE(x)` are shown as `A1#` / `@x` like Excel.
fn file_formula(text: &str) -> Formula {
    let f = Formula::new(text);
    match f.ast {
        Some(ast) if text.contains("ANCHORARRAY(") || text.contains("SINGLE(") => Formula::from_ast(ast),
        _ => f,
    }
}

/// Array formulas are saved with their results as plain cells inside the formula's range;
/// drop those so the formula spills them again (they are kept as recalculation hints).
fn clear_array_results(sheet: &mut Sheet) {
    let anchors: Vec<((u32, u32), Range)> = sheet.spill.anchors.iter().map(|(a, (rg, _))| (*a, *rg)).collect();
    for (a, rg) in anchors {
        let keys: Vec<(u32, u32)> = sheet.iter_range(rg).filter(|(r, c, cell)| (*r, *c) != a && cell.formula.is_none()).map(|(r, c, _)| (r, c)).collect();
        for (r, c) in keys {
            if let Some(cell) = sheet.get_mut(r, c) {
                cell.value = Value::Empty;
            }
            sheet.tidy(r, c);
        }
    }
}

fn finish_cell(cb: CellBuf, ctx: &SheetCtx, shared_f: &mut HashMap<String, SharedMaster>, sheet: &mut Sheet) {
    let style = ctx.xf.ids.get(cb.style).copied().unwrap_or(0);
    let is_date = ctx.xf.date.get(cb.style).copied().unwrap_or(false);
    let mut value = match cb.t.as_str() {
        "s" => cb
            .v
            .as_deref()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .and_then(|i| ctx.shared.get(i))
            .map(|s| Value::Text(s.clone()))
            .unwrap_or(Value::Empty),
        "inlineStr" => cb.is.clone().or_else(|| cb.v.clone()).map(Value::Text).unwrap_or(Value::Empty),
        "str" => cb.v.clone().map(|v| Value::Text(decode_ooxml_escapes(v))).unwrap_or(Value::Empty),
        "b" => cb.v.as_deref().map(|v| Value::Bool(v.trim() == "1" || v.trim().eq_ignore_ascii_case("true"))).unwrap_or(Value::Empty),
        "e" => cb
            .v
            .as_deref()
            .map(|v| ErrorKind::parse(v).map(Value::Error).unwrap_or_else(|| Value::Text(v.to_string())))
            .unwrap_or(Value::Empty),
        "d" => cb.v.as_deref().and_then(iso_to_serial).map(Value::Number).unwrap_or(Value::Empty),
        _ => match cb.v.as_deref().and_then(parse_f) {
            Some(n) => Value::Number(n),
            None => match cb.is.clone() {
                Some(s) => Value::Text(s),
                None => Value::Empty,
            },
        },
    };
    if ctx.date1904 && is_date {
        if let Value::Number(n) = &mut value {
            *n += 1462.0;
        }
    }

    let mut f: Option<Formula> = None;
    if let Some(text) = cb.f_text {
        let text = text.trim();
        match cb.f_type.as_str() {
            "shared" => {
                let si = cb.f_si.unwrap_or_default();
                if !text.is_empty() {
                    let text = strip_future_prefixes(text);
                    let ast = formula::parse(&text).ok();
                    f = Some(file_formula(&text));
                    shared_f.insert(si, SharedMaster { row: cb.row, col: cb.col, ast, text });
                } else if let Some(m) = shared_f.get(&si) {
                    f = Some(match &m.ast {
                        Some(ast) => {
                            let mut a = ast.clone();
                            formula::shift_relative(&mut a, cb.row as i64 - m.row as i64, cb.col as i64 - m.col as i64);
                            Formula::from_ast(a)
                        }
                        None => Formula::new(&m.text),
                    });
                }
            }
            "dataTable" => {}
            "array" if !text.is_empty() => {
                let text = strip_future_prefixes(text);
                if let Some(rg) = cb.f_ref.as_deref().and_then(Range::parse).filter(|rg| !rg.is_single() && rg.r1 == cb.row && rg.c1 == cb.col) {
                    sheet.spill.anchors.insert((cb.row, cb.col), (rg, false));
                }
                f = Some(file_formula(&text));
            }
            _ => {
                if !text.is_empty() {
                    let text = strip_future_prefixes(text);
                    // A formula that is just an error literal is how errors are stored on save.
                    match ErrorKind::parse(&text) {
                        Some(e) if !text.contains('(') => value = Value::Error(e),
                        _ => f = Some(file_formula(&text)),
                    }
                }
            }
        }
    }
    let cell = Cell { value, formula: f.map(Box::new), style };
    sheet.set(cb.row, cb.col, Some(cell));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tint_conversion() {
        // Excel: "White, Background 1, Darker 15%"
        assert_eq!(apply_tint(0xFFFFFF, -0.1499984740745262), 0xD9D9D9);
        // Excel: "Blue, Accent 1, Lighter 80%" -> D9E1F2
        let c = apply_tint(0x4472C4, 0.7999816888943144);
        let (r, g, b) = ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF);
        assert!(r.abs_diff(0xD9) <= 1 && g.abs_diff(0xE1) <= 1 && b.abs_diff(0xF2) <= 1, "{c:06X}");
        // "Black, Text 1, Lighter 50%" -> 7F7F7F
        let c = apply_tint(0x000000, 0.4999847407452621);
        assert!((c & 0xFF).abs_diff(0x7F) <= 1, "{c:06X}");
        assert_eq!(apply_tint(0x123456, 0.0), 0x123456);
    }

    #[test]
    fn theme_index_order() {
        let xml = r#"<a:theme xmlns:a="x"><a:themeElements><a:clrScheme name="Office">
            <a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1>
            <a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>
            <a:dk2><a:srgbClr val="1F497D"/></a:dk2><a:lt2><a:srgbClr val="EEECE1"/></a:lt2>
            <a:accent1><a:srgbClr val="4F81BD"/></a:accent1></a:clrScheme></a:themeElements></a:theme>"#;
        let t = parse_theme(xml);
        assert_eq!(t.colors[0], 0xFFFFFF);
        assert_eq!(t.colors[1], 0x000000);
        assert_eq!(t.colors[2], 0xEEECE1);
        assert_eq!(t.colors[3], 0x1F497D);
        assert_eq!(t.colors[4], 0x4F81BD);
    }

    #[test]
    fn refs_and_widths() {
        assert_eq!(parse_ref("A1"), Some((0, 0)));
        assert_eq!(parse_ref("AB12"), Some((11, 27)));
        assert_eq!(parse_ref("A0"), None);
        assert_eq!(width_to_px(9.140625), 64.0);
        assert_eq!(resolve("xl/", "worksheets/sheet1.xml"), "xl/worksheets/sheet1.xml");
        assert_eq!(resolve("xl/", "/xl/worksheets/sheet1.xml"), "xl/worksheets/sheet1.xml");
        assert_eq!(resolve("xl/worksheets/", "../drawings/d.xml"), "xl/drawings/d.xml");
        assert_eq!(decode_ooxml_escapes("a_x000D_b".into()), "a\rb");
    }
}
