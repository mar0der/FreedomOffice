//! PDF export of laid-out pages (standard Helvetica fonts, no embedding).

use std::fmt::Write as _;
use std::path::Path;

use super::{Align, Op, Page, TextOp};
use crate::model::VAlign;

const HELV: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556, 556, 556, 556,
    556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778,
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556,
    556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

/// Helvetica is wider than the usual sheet fonts; shrink a little so layouts match the screen.
const SIZE_ADJ: f32 = 0.92;

fn text_width(s: &str, size: f32, bold: bool) -> f32 {
    let w: u32 = s.chars().map(|c| if (' '..='~').contains(&c) { HELV[c as usize - 32] as u32 } else { 556 }).sum();
    w as f32 / 1000.0 * size * if bold { 1.06 } else { 1.0 }
}

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
            c if (32..256).contains(&(c as u32)) => c as u32 as u8,
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

/// Greedy word wrap using Helvetica metrics.
fn wrap_lines(text: &str, size: f32, bold: bool, width: f32) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            let cand = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if text_width(&cand, size, bold) <= width || line.is_empty() {
                line = cand;
            } else {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
            }
        }
        out.push(line);
    }
    out
}

fn draw_text(s: &mut Vec<u8>, t: &TextOp, ph: f32) {
    let size = t.font.size * SIZE_ADJ;
    if size <= 0.5 {
        return;
    }
    let font = match (t.font.bold, t.font.italic) {
        (false, false) => "F1",
        (true, false) => "F2",
        (false, true) => "F3",
        (true, true) => "F4",
    };
    let pad = 2.0;
    let (r, g, b) = rgb(t.color);
    let lines: Vec<String> = if t.wrap {
        wrap_lines(&t.text, size, t.font.bold, (t.rect.w - 2.0 * pad - t.indent).max(1.0))
    } else {
        let first = t.text.lines().next().unwrap_or("").to_string();
        if t.number && text_width(&first, size, t.font.bold) > t.rect.w - 2.0 * pad {
            let n = ((t.rect.w - 2.0 * pad) / text_width("#", size, false)).floor().max(1.0) as usize;
            vec!["#".repeat(n)]
        } else {
            vec![first]
        }
    };
    let line_h = size * 1.2;
    let block_h = line_h * lines.len() as f32;
    let clip = t.clip;
    s.extend(format!("q {:.2} {:.2} {:.2} {:.2} re W n {r:.3} {g:.3} {b:.3} rg\n", clip.x, ph - clip.bottom(), clip.w, clip.h).bytes());
    if t.rotation != 0 && t.rotation != 255 && !t.wrap {
        let deg = if t.rotation <= 90 { t.rotation as f32 } else { -((t.rotation - 90) as f32) };
        let a = deg.to_radians();
        let (sn, cs) = a.sin_cos();
        let w = text_width(&lines[0], size, t.font.bold);
        // anchor so the rotated text sits centred horizontally and at the bottom of the cell
        let cx = t.rect.x + t.rect.w / 2.0;
        let bx = cx - (w * cs) / 2.0 + size * 0.35 * sn;
        let by = t.rect.bottom() - pad - (w * sn).max(0.0);
        s.extend(format!("BT /{font} {size:.2} Tf {cs:.4} {sn:.4} {:.4} {cs:.4} {bx:.2} {:.2} Tm ", -sn, ph - by).bytes());
        s.extend(pdf_str(&lines[0]));
        s.extend(b" Tj ET Q\n");
        return;
    }
    let top = match t.valign {
        VAlign::Top => t.rect.y + 1.0,
        VAlign::Center => t.rect.y + (t.rect.h - block_h) / 2.0,
        VAlign::Bottom => t.rect.bottom() - block_h - 1.0,
    };
    for (i, line) in lines.iter().enumerate() {
        let w = text_width(line, size, t.font.bold);
        let x = match t.halign {
            Align::Left => t.rect.x + pad + t.indent,
            Align::Center => t.rect.x + (t.rect.w - w) / 2.0,
            Align::Right => t.rect.right() - pad - w,
        };
        let baseline = top + line_h * i as f32 + size * 0.95;
        s.extend(format!("BT /{font} {size:.2} Tf {x:.2} {:.2} Td ", ph - baseline).bytes());
        s.extend(pdf_str(line));
        s.extend(b" Tj ET\n");
        if t.font.underline || t.font.strike {
            let yy = if t.font.underline { baseline + 1.5 } else { baseline - size * 0.3 };
            s.extend(format!("{r:.3} {g:.3} {b:.3} RG 0.5 w {x:.2} {:.2} m {:.2} {:.2} l S\n", ph - yy, x + w, ph - yy).bytes());
        }
    }
    s.extend(b"Q\n");
}

fn page_content(p: &Page) -> Vec<u8> {
    let ph = p.height;
    let mut s: Vec<u8> = Vec::new();
    for op in &p.ops {
        match op {
            Op::Fill { rect, color } => {
                let (r, g, b) = rgb(*color);
                s.extend(format!("{r:.3} {g:.3} {b:.3} rg {:.2} {:.2} {:.2} {:.2} re f\n", rect.x, ph - rect.bottom(), rect.w, rect.h).bytes());
            }
            Op::Line { x1, y1, x2, y2, width, color, dashed } => {
                let (r, g, b) = rgb(*color);
                let dash = if *dashed { "[2 2] 0 d" } else { "[] 0 d" };
                s.extend(format!("{r:.3} {g:.3} {b:.3} RG {width:.2} w {dash} {x1:.2} {:.2} m {x2:.2} {:.2} l S\n", ph - y1, ph - y2).bytes());
            }
            Op::Poly { pts, color } => {
                if pts.len() < 3 {
                    continue;
                }
                let (r, g, b) = rgb(*color);
                s.extend(format!("{r:.3} {g:.3} {b:.3} rg {:.2} {:.2} m ", pts[0].0, ph - pts[0].1).bytes());
                for (x, y) in &pts[1..] {
                    s.extend(format!("{x:.2} {:.2} l ", ph - y).bytes());
                }
                s.extend(b"h f\n");
            }
            Op::Path { pts, width, color } => {
                if pts.len() < 2 {
                    continue;
                }
                let (r, g, b) = rgb(*color);
                s.extend(format!("{r:.3} {g:.3} {b:.3} RG {width:.2} w [] 0 d {:.2} {:.2} m ", pts[0].0, ph - pts[0].1).bytes());
                for (x, y) in &pts[1..] {
                    s.extend(format!("{x:.2} {:.2} l ", ph - y).bytes());
                }
                s.extend(b"S\n");
            }
            Op::Text(t) => draw_text(&mut s, t, ph),
        }
    }
    s
}

pub fn render(pages: &[Page]) -> Vec<u8> {
    let mut pdf: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let obj = |pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, body: &[u8]| {
        offsets.push(pdf.len());
        let id = offsets.len();
        pdf.extend(format!("{id} 0 obj\n").bytes());
        pdf.extend(body);
        pdf.extend(b"\nendobj\n");
    };
    obj(&mut pdf, &mut offsets, b"<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = (0..pages.len()).map(|i| format!("{} 0 R", 7 + i * 2)).collect();
    obj(&mut pdf, &mut offsets, format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()).as_bytes());
    for f in ["Helvetica", "Helvetica-Bold", "Helvetica-Oblique", "Helvetica-BoldOblique"] {
        obj(&mut pdf, &mut offsets, format!("<< /Type /Font /Subtype /Type1 /BaseFont /{f} /Encoding /WinAnsiEncoding >>").as_bytes());
    }
    for (i, p) in pages.iter().enumerate() {
        let content = page_content(p);
        obj(
            &mut pdf,
            &mut offsets,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.2} {:.2}] /Resources << /Font << /F1 3 0 R /F2 4 0 R /F3 5 0 R /F4 6 0 R >> >> /Contents {} 0 R >>",
                p.width,
                p.height,
                8 + i * 2
            )
            .as_bytes(),
        );
        let mut body = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
        body.extend(&content);
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
    pdf
}

pub fn export(pages: &[Page], path: &Path) -> Result<usize, String> {
    if pages.is_empty() {
        return Err("There's nothing to print.".into());
    }
    std::fs::write(path, render(pages)).map_err(|e| e.to_string())?;
    Ok(pages.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{cell_from_input, Workbook};
    use crate::print::{layout, LayoutOptions, PrintWhat};

    #[test]
    fn renders_a_valid_pdf() {
        let mut wb = Workbook::new();
        for r in 0..120u32 {
            let cell = cell_from_input(&mut wb, &format!("Row {r} (x)"), 0);
            wb.sheets[0].set(r, 0, cell);
        }
        let o = LayoutOptions { what: PrintWhat::ActiveSheet, selection: None, ignore_print_area: false, file_name: "B", file_path: "" };
        let pages = layout(&wb, &o, &|_, _, _| Vec::new());
        let pdf = String::from_utf8_lossy(&render(&pages)).to_string();
        assert!(pdf.starts_with("%PDF-1.4"));
        assert!(pdf.contains(&format!("/Count {}", pages.len())));
        assert!(pdf.contains("(Row 0 \\(x\\)) Tj"));
        assert!(pdf.trim_end().ends_with("%%EOF"));
    }
}

#[cfg(test)]
mod sample {
    use crate::model::{cell_from_input, BorderLine, Workbook};
    use crate::page::{HeaderFooter, Scaling};
    use crate::print::{layout, LayoutOptions, PrintWhat};

    /// Writes a sample multi-page PDF to the temp dir for visual inspection.
    #[test]
    #[ignore]
    fn print_sample_pdf() {
        let mut wb = Workbook::new();
        let head = ["Region", "Product", "Units", "Price", "Revenue", "Notes"];
        for (c, h) in head.iter().enumerate() {
            let cell = cell_from_input(&mut wb, h, 0);
            wb.sheets[0].set(0, c as u32, cell);
        }
        for r in 1..90u32 {
            let vals = [
                ["North", "South", "East", "West"][r as usize % 4].to_string(),
                format!("Item {r}"),
                format!("{}", r * 7 % 50 + 1),
                format!("{:.2}", (r as f64) * 1.37),
                format!("=C{}*D{}", r + 1, r + 1),
                if r % 9 == 0 { "A longer note that overflows into the next empty cells".into() } else { String::new() },
            ];
            for (c, v) in vals.iter().enumerate() {
                let cell = cell_from_input(&mut wb, v, 0);
                wb.sheets[0].set(r, c as u32, cell);
            }
        }
        let mut st = wb.style(0).clone();
        st.bold = true;
        st.fill = Some(0x4472C4);
        st.color = Some(0xFFFFFF);
        st.border.bottom = BorderLine::Medium;
        let hdr = wb.intern_style(st);
        for c in 0..6 {
            wb.sheets[0].cell_mut(0, c).style = hdr;
        }
        let mut money = wb.style(0).clone();
        money.num_fmt = "\"$\"#,##0.00".into();
        let m = wb.intern_style(money);
        for r in 1..90 {
            wb.sheets[0].cell_mut(r, 4).style = m;
        }
        wb.recalc();
        let p = &mut wb.sheets[0].page;
        p.title_rows = Some((0, 0));
        p.gridlines = true;
        p.headings = true;
        p.scaling = Scaling::Fit { wide: 1, tall: 0 };
        p.header = HeaderFooter { left: "&A".into(), center: "Sales report".into(), right: "&D".into() };
        p.footer = HeaderFooter::center("Page &P of &N");
        let o = LayoutOptions { what: PrintWhat::ActiveSheet, selection: None, ignore_print_area: false, file_name: "Sales.xlsx", file_path: "" };
        let pages = layout(&wb, &o, &|_, _, _| Vec::new());
        let path = std::env::temp_dir().join("freedom_print_sample.pdf");
        super::export(&pages, &path).unwrap();
        println!("{} pages -> {}", pages.len(), path.display());
    }
}
