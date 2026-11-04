//! Minimal PDF export of a worksheet (used for Print and Export to PDF).
//! Uses the standard Helvetica fonts so no font embedding is needed.

use std::fmt::Write as _;
use std::path::Path;

use crate::model::{BorderLine, HAlign, Range, VAlign, Value, Workbook};
use crate::numfmt;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageSetup {
    pub landscape: bool,
    /// true = A4, false = US Letter
    pub a4: bool,
    pub gridlines: bool,
    pub fit_width: bool,
}

impl Default for PageSetup {
    fn default() -> Self {
        PageSetup { landscape: false, a4: true, gridlines: false, fit_width: false }
    }
}

const HELV: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556, 556, 556, 556,
    556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778,
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556,
    556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

fn text_width(s: &str, size: f32, bold: bool) -> f32 {
    let w: u32 = s.chars().map(|c| if (' '..='~').contains(&c) { HELV[c as usize - 32] as u32 } else { 556 }).sum();
    w as f32 / 1000.0 * size * if bold { 1.06 } else { 1.0 }
}

/// Encodes text as a PDF literal string in WinAnsiEncoding.
fn pdf_str(s: &str) -> Vec<u8> {
    let mut out = vec![b'('];
    for ch in s.chars() {
        let b: u8 = match ch {
            '€' => 0x80,
            '‚' => 0x82,
            '„' => 0x84,
            '…' => 0x85,
            '•' => 0x95,
            '–' => 0x96,
            '—' => 0x97,
            '™' => 0x99,
            '‘' => 0x91,
            '’' => 0x92,
            '“' => 0x93,
            '”' => 0x94,
            c if (c as u32) < 256 && (c as u32) >= 32 => c as u32 as u8,
            '\t' => b' ',
            _ => b'?',
        };
        if b == b'(' || b == b')' || b == b'\\' {
            out.push(b'\\');
        }
        out.push(b);
    }
    out.push(b')');
    out
}

fn rgb(c: u32) -> (f32, f32, f32) {
    (((c >> 16) & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, (c & 255) as f32 / 255.0)
}

/// Splits a run of sizes into pages that fit `avail` (each page at least one item).
fn breaks(sizes: &[f32], avail: f32) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut acc = 0.0;
    for (i, s) in sizes.iter().enumerate() {
        if acc + s > avail && i > start {
            out.push((start, i));
            start = i;
            acc = 0.0;
        }
        acc += s;
    }
    if start < sizes.len() {
        out.push((start, sizes.len()));
    }
    out
}

/// Renders the area of a sheet to PDF bytes.
pub fn render(wb: &Workbook, sheet: usize, area: Option<Range>, setup: PageSetup) -> Result<Vec<u8>, String> {
    let sh = wb.sheets.get(sheet).ok_or("no sheet")?;
    let area = match area.or_else(|| sh.used_range()) {
        Some(a) => a,
        None => return Err("There's nothing to print on this sheet.".into()),
    };
    let (mut pw, mut ph) = if setup.a4 { (595.0f32, 842.0f32) } else { (612.0, 792.0) };
    if setup.landscape {
        std::mem::swap(&mut pw, &mut ph);
    }
    let (ml, mr, mt, mb) = (50.0f32, 50.0f32, 54.0f32, 54.0f32);
    let avail_w = pw - ml - mr;
    let avail_h = ph - mt - mb - 14.0;
    let px = 0.75f32; // screen px -> pt
    let cols: Vec<u32> = (area.c1..=area.c2).filter(|c| sh.col_width(*c) > 0.0).collect();
    let rows: Vec<u32> = (area.r1..=area.r2).filter(|r| sh.row_height(*r) > 0.0).collect();
    let cw: Vec<f32> = cols.iter().map(|c| sh.col_width(*c) * px).collect();
    let rh: Vec<f32> = rows.iter().map(|r| sh.row_height(*r) * px).collect();
    let total_w: f32 = cw.iter().sum();
    let scale = if setup.fit_width && total_w > avail_w { avail_w / total_w } else { 1.0 };
    let cw: Vec<f32> = cw.iter().map(|w| w * scale).collect();
    let rh: Vec<f32> = rh.iter().map(|h| h * scale).collect();
    let col_pages = breaks(&cw, avail_w);
    let row_pages = breaks(&rh, avail_h);

    // pages go down, then over
    let mut contents: Vec<Vec<u8>> = Vec::new();
    let total_pages = col_pages.len() * row_pages.len();
    for &(c0, c1) in &col_pages {
        for &(r0, r1) in &row_pages {
            let mut s: Vec<u8> = Vec::new();
            let mut xs = vec![ml];
            for w in &cw[c0..c1] {
                let last = *xs.last().unwrap();
                xs.push(last + w);
            }
            let mut ys = vec![ph - mt];
            for h in &rh[r0..r1] {
                let last = *ys.last().unwrap();
                ys.push(last - h);
            }
            // fills
            for (ri, &r) in rows[r0..r1].iter().enumerate() {
                for (ci, &c) in cols[c0..c1].iter().enumerate() {
                    if let Some(cell) = sh.get(r, c) {
                        if let Some(f) = wb.style(cell.style).fill {
                            let (a, b, cc) = rgb(f);
                            s.extend(format!("{a:.3} {b:.3} {cc:.3} rg {:.2} {:.2} {:.2} {:.2} re f\n", xs[ci], ys[ri + 1], xs[ci + 1] - xs[ci], ys[ri] - ys[ri + 1]).bytes());
                        }
                    }
                }
            }
            // gridlines
            if setup.gridlines {
                s.extend(b"0.8 0.8 0.8 RG 0.4 w\n");
                for x in &xs {
                    s.extend(format!("{x:.2} {:.2} m {x:.2} {:.2} l S\n", ys[0], ys[ys.len() - 1]).bytes());
                }
                for y in &ys {
                    s.extend(format!("{:.2} {y:.2} m {:.2} {y:.2} l S\n", xs[0], xs[xs.len() - 1]).bytes());
                }
            }
            // text
            for (ri, &r) in rows[r0..r1].iter().enumerate() {
                for (ci, &c) in cols[c0..c1].iter().enumerate() {
                    let Some(cell) = sh.get(r, c) else { continue };
                    if sh.merge_at(r, c).is_some_and(|m| (m.r1, m.c1) != (r, c)) {
                        continue;
                    }
                    let st = wb.style(cell.style);
                    let f = numfmt::format_value(&cell.value, &st.num_fmt);
                    if f.text.is_empty() {
                        continue;
                    }
                    // merged area extent within this page
                    let (mut x1, mut y1) = (xs[ci + 1], ys[ri + 1]);
                    if let Some(m) = sh.merge_at(r, c) {
                        let cend = cols[c0..c1].iter().rposition(|cc| *cc <= m.c2).unwrap_or(ci);
                        let rend = rows[r0..r1].iter().rposition(|rr| *rr <= m.r2).unwrap_or(ri);
                        x1 = xs[cend + 1];
                        y1 = ys[rend + 1];
                    }
                    let (x0, y0) = (xs[ci], ys[ri]);
                    let size = st.font_size() * 0.92 * scale;
                    let font = match (st.bold, st.italic) {
                        (false, false) => "F1",
                        (true, false) => "F2",
                        (false, true) => "F3",
                        (true, true) => "F4",
                    };
                    let tw = text_width(&f.text, size, st.bold);
                    let pad = 2.0 * scale;
                    let auto = match cell.value {
                        Value::Number(_) => HAlign::Right,
                        Value::Bool(_) | Value::Error(_) => HAlign::Center,
                        _ => HAlign::Left,
                    };
                    let align = if st.halign == HAlign::General { auto } else { st.halign };
                    // text that doesn't fit overflows to the right for left-aligned text only
                    let mut clip_x1 = x1;
                    if align == HAlign::Left && matches!(cell.value, Value::Text(_)) && tw + 2.0 * pad > x1 - x0 {
                        let mut k = ci + 1;
                        while k < c1 - c0 && !sh.is_populated(r, cols[c0 + k]) {
                            clip_x1 = xs[k + 1];
                            if clip_x1 - x0 > tw + 2.0 * pad {
                                break;
                            }
                            k += 1;
                        }
                    }
                    let tx = match align {
                        HAlign::Right => x1 - pad - tw,
                        HAlign::Center => (x0 + x1 - tw) / 2.0,
                        _ => x0 + pad + st.indent as f32 * 7.0 * scale,
                    };
                    let ty = match st.valign {
                        VAlign::Top => y0 - size,
                        VAlign::Center => (y0 + y1) / 2.0 - size * 0.35,
                        VAlign::Bottom => y1 + size * 0.28,
                    };
                    let color = f.color.or(st.color).unwrap_or(0);
                    let (a, b, cc) = rgb(color);
                    s.extend(format!("q {:.2} {:.2} {:.2} {:.2} re W n BT /{font} {size:.2} Tf {a:.3} {b:.3} {cc:.3} rg {tx:.2} {ty:.2} Td ", x0, y1, clip_x1 - x0, y0 - y1).bytes());
                    s.extend(pdf_str(f.text.lines().next().unwrap_or("")));
                    s.extend(b" Tj ET Q\n");
                    if st.underline {
                        s.extend(format!("{a:.3} {b:.3} {cc:.3} RG 0.5 w {tx:.2} {:.2} m {:.2} {:.2} l S\n", ty - 1.5, tx + tw, ty - 1.5).bytes());
                    }
                }
            }
            // borders
            for (ri, &r) in rows[r0..r1].iter().enumerate() {
                for (ci, &c) in cols[c0..c1].iter().enumerate() {
                    let Some(cell) = sh.get(r, c) else { continue };
                    let b = wb.style(cell.style).border;
                    let (x0, x1, y0, y1) = (xs[ci], xs[ci + 1], ys[ri], ys[ri + 1]);
                    for (line, p) in [(b.top, (x0, y0, x1, y0)), (b.bottom, (x0, y1, x1, y1)), (b.left, (x0, y0, x0, y1)), (b.right, (x1, y0, x1, y1))] {
                        let w = match line {
                            BorderLine::None => continue,
                            BorderLine::Medium => 1.0,
                            BorderLine::Thick | BorderLine::Double => 1.5,
                            _ => 0.5,
                        };
                        s.extend(format!("0 0 0 RG {w} w {:.2} {:.2} m {:.2} {:.2} l S\n", p.0, p.1, p.2, p.3).bytes());
                    }
                }
            }
            // footer
            let page_no = contents.len() + 1;
            let footer = format!("Page {page_no} of {total_pages}");
            let fw = text_width(&footer, 9.0, false);
            s.extend(format!("BT /F1 9 Tf 0.3 0.3 0.3 rg {:.2} {:.2} Td ", (pw - fw) / 2.0, mb / 2.0).bytes());
            s.extend(pdf_str(&footer));
            s.extend(b" Tj ET\n");
            contents.push(s);
        }
    }

    // Assemble the PDF
    let mut pdf: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let n_pages = contents.len();
    // object numbers: 1 catalog, 2 pages, 3-6 fonts, then (page, content) pairs
    let mut obj = |pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, body: &[u8]| {
        offsets.push(pdf.len());
        let id = offsets.len();
        pdf.extend(format!("{id} 0 obj\n").bytes());
        pdf.extend(body);
        pdf.extend(b"\nendobj\n");
    };
    obj(&mut pdf, &mut offsets, b"<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = (0..n_pages).map(|i| format!("{} 0 R", 7 + i * 2)).collect();
    obj(&mut pdf, &mut offsets, format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), n_pages).as_bytes());
    for f in ["Helvetica", "Helvetica-Bold", "Helvetica-Oblique", "Helvetica-BoldOblique"] {
        obj(&mut pdf, &mut offsets, format!("<< /Type /Font /Subtype /Type1 /BaseFont /{f} /Encoding /WinAnsiEncoding >>").as_bytes());
    }
    for (i, c) in contents.iter().enumerate() {
        let content_id = 8 + i * 2;
        obj(
            &mut pdf,
            &mut offsets,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw} {ph}] /Resources << /Font << /F1 3 0 R /F2 4 0 R /F3 5 0 R /F4 6 0 R >> >> /Contents {content_id} 0 R >>"
            )
            .as_bytes(),
        );
        let mut body = format!("<< /Length {} >>\nstream\n", c.len()).into_bytes();
        body.extend(c);
        body.extend(b"\nendstream");
        obj(&mut pdf, &mut offsets, &body);
    }
    let xref = pdf.len();
    let mut x = format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1);
    for o in &offsets {
        let _ = writeln!(x, "{o:010} 00000 n ");
    }
    let _ = write!(x, "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len() + 1);
    pdf.extend(x.bytes());
    Ok(pdf)
}

pub fn export(wb: &Workbook, sheet: usize, area: Option<Range>, setup: PageSetup, path: &Path) -> Result<usize, String> {
    let bytes = render(wb, sheet, area, setup)?;
    let pages = bytes.windows(12).filter(|w| w == b"/Type /Page ").count();
    std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{cell_from_input, Workbook};

    #[test]
    fn renders_pages() {
        let mut wb = Workbook::new();
        for r in 0..120u32 {
            for c in 0..3u32 {
                let cell = cell_from_input(&mut wb, &format!("R{r}C{c} (x)"), 0);
                wb.sheets[0].set(r, c, cell);
            }
        }
        let pdf = render(&wb, 0, None, PageSetup::default()).unwrap();
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.starts_with("%PDF-1.4"));
        assert!(s.contains("/Count 3"), "120 rows of 15pt should need 3 A4 pages");
        assert!(s.contains("(R0C0 \\(x\\)) Tj"));
        assert!(s.trim_end().ends_with("%%EOF"));
    }

    #[test]
    fn page_breaks() {
        assert_eq!(breaks(&[10.0, 10.0, 10.0], 25.0), vec![(0, 2), (2, 3)]);
        assert_eq!(breaks(&[30.0], 25.0), vec![(0, 1)]);
    }
}

#[cfg(test)]
mod sample {
    use super::*;
    use crate::model::{cell_from_input, Workbook};

    /// Writes a sample PDF to the temp dir for manual inspection.
    #[test]
    #[ignore]
    fn export_sample() {
        let mut wb = Workbook::new();
        let rows = [
            ["Region", "Product", "Units", "Price", "Revenue"],
            ["North", "Apples", "120", "1.5", "=C2*D2"],
            ["South", "Pears (organic)", "80", "2.25", "=C3*D3"],
            ["East", "Cherries", "200", "4", "=C4*D4"],
            ["Total", "", "=SUM(C2:C4)", "", "=SUM(E2:E4)"],
        ];
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let cell = cell_from_input(&mut wb, v, 0);
                wb.sheets[0].set(r as u32, c as u32, cell);
            }
        }
        let mut st = wb.style(0).clone();
        st.bold = true;
        st.fill = Some(0x4472C4);
        st.color = Some(0xFFFFFF);
        let hdr = wb.intern_style(st);
        for c in 0..5 {
            wb.sheets[0].cell_mut(0, c).style = hdr;
        }
        let mut money = wb.style(0).clone();
        money.num_fmt = "\"$\"#,##0.00".into();
        money.border.top = BorderLine::Thin;
        let m = wb.intern_style(money);
        for r in 1..5 {
            wb.sheets[0].cell_mut(r, 4).style = m;
        }
        wb.sheets[0].col_w.insert(1, 120.0);
        wb.recalc();
        let p = std::env::temp_dir().join("freedom_sample.pdf");
        let n = export(&wb, 0, None, PageSetup { gridlines: true, ..Default::default() }, &p).unwrap();
        println!("{} pages -> {}", n, p.display());
    }
}
