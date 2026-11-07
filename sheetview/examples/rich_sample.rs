//! Writes a workbook with the things the web viewer draws beyond cell text (conditional formats,
//! a note, a chart, a picture, merges, frozen panes) for browser checks:
//! `cargo run -p freedomoffice-sheetview --example rich_sample -- out.xlsx`

use rust_xlsxwriter::{Chart, ChartType, ConditionalFormat2ColorScale, ConditionalFormatCell, ConditionalFormatCellRule, ConditionalFormatDataBar, Format, Image, Note, Workbook};

fn main() {
    let out = std::env::args().nth(1).expect("output path");
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name("Sales").unwrap();
    let head = Format::new().set_bold().set_background_color(0xdde8f5);
    for (c, t) in ["Region", "Q1", "Q2", "Q3", "Q4"].iter().enumerate() {
        ws.write_string_with_format(0, c as u16, *t, &head).unwrap();
    }
    for (r, name) in ["North", "South", "East", "West", "Centre", "Islands"].iter().enumerate() {
        ws.write_string(r as u32 + 1, 0, *name).unwrap();
        for q in 1..=4u16 {
            ws.write_number(r as u32 + 1, q, f64::from(((r as u32 * 37 + u32::from(q) * 53) % 90) + 10)).unwrap();
        }
    }
    ws.set_freeze_panes(1, 1).unwrap();
    ws.set_column_width(0, 14).unwrap();
    ws.add_conditional_format(1, 1, 6, 1, &ConditionalFormatCell::new().set_rule(ConditionalFormatCellRule::GreaterThan(50)).set_format(Format::new().set_background_color(0xffc7ce).set_font_color(0x9c0006))).unwrap();
    ws.add_conditional_format(1, 2, 6, 2, &ConditionalFormat2ColorScale::new().set_minimum_color(0xffffff).set_maximum_color(0x63be7b)).unwrap();
    ws.add_conditional_format(1, 3, 6, 3, &ConditionalFormatDataBar::new()).unwrap();
    ws.insert_note(1, 0, &Note::new("Check the North figures")).unwrap();
    ws.merge_range(9, 0, 9, 3, "Merged heading across four columns", &Format::new().set_bold().set_align(rust_xlsxwriter::FormatAlign::Center)).unwrap();
    let mut ch = Chart::new(ChartType::Column);
    ch.add_series().set_categories("Sales!$A$2:$A$7").set_values("Sales!$B$2:$B$7").set_name("Q1");
    ch.add_series().set_categories("Sales!$A$2:$A$7").set_values("Sales!$C$2:$C$7").set_name("Q2");
    ch.title().set_name("Sales by region");
    ws.insert_chart(11, 0, &ch).unwrap();
    // A small picture: a 40x24 gradient PNG written by hand.
    let png = png_gradient(40, 24);
    ws.insert_image(11, 6, &Image::new_from_buffer(&png).unwrap()).unwrap();
    let ws2 = wb.add_worksheet();
    ws2.set_name("Notes").unwrap();
    ws2.write_string(0, 0, "A second sheet").unwrap();
    wb.save(out).unwrap();
}

/// A PNG with stored (uncompressed) deflate blocks, so no compression crate is needed.
fn png_gradient(w: u32, h: u32) -> Vec<u8> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn adler(data: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for &x in data {
            a = (a + u32::from(x)) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend(data);
        out.extend(&body);
        out.extend(crc(&body).to_be_bytes());
    }
    let mut raw = Vec::new();
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            raw.extend([(x * 255 / w) as u8, (y * 255 / h) as u8, 160]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(60000).peekable();
    while let Some(c) = chunks.next() {
        z.push(u8::from(chunks.peek().is_none()));
        z.extend((c.len() as u16).to_le_bytes());
        z.extend((!(c.len() as u16)).to_le_bytes());
        z.extend(c);
    }
    z.extend(adler(&raw).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}
