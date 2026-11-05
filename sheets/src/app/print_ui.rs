//! File > Print (printer, copies, settings, live preview), Export to PDF,
//! the Page Setup dialog, the Page Layout ribbon commands and page-break lines.

use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, text::LayoutJob, Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextFormat, Vec2};

use super::dialogs::Dialog;
use super::grid::Layout;
use super::theme::{self, ic};
use super::{App, Scope};
use crate::model::{Range, VAlign};
use crate::page::{hf_presets, metric_locale, HeaderFooter, Margins, PageSetup, Paper, Scaling};
use crate::print::gdi::{self, Duplex, PrintSettings, PrinterInfo};
use crate::print::{self, Align, LayoutOptions, Op, PRect, Page, PrintWhat, TextOp};

pub struct PrintState {
    pub printers: Option<Vec<PrinterInfo>>,
    pub printer: String,
    pub copies: u32,
    pub what: PrintWhat,
    pub ignore_area: bool,
    pub from: u32,
    pub to: u32,
    pub collate: bool,
    pub duplex: Duplex,
    pub devmode: Option<Vec<u8>>,
    pub page: usize,
    pub show_margins: bool,
    pub zoom_to_page: bool,
    cache: Option<(u64, Rc<Vec<Page>>)>,
    pub result: Arc<Mutex<Option<Result<String, String>>>>,
    pub busy: bool,
    /// Sheets that show page-break lines in Normal view (after previewing / page setup).
    pub show_breaks: std::collections::HashSet<usize>,
    breaks_cache: Option<(u64, usize, Vec<u32>, Vec<u32>)>,
}

impl Default for PrintState {
    fn default() -> Self {
        PrintState {
            printers: None,
            printer: String::new(),
            copies: 1,
            what: PrintWhat::ActiveSheet,
            ignore_area: false,
            from: 0,
            to: 0,
            collate: true,
            duplex: Duplex::OneSided,
            devmode: None,
            page: 0,
            show_margins: false,
            zoom_to_page: true,
            cache: None,
            result: Arc::new(Mutex::new(None)),
            busy: false,
            show_breaks: Default::default(),
            breaks_cache: None,
        }
    }
}

fn rgb(c: u32) -> Color32 {
    theme::rgb(c)
}

fn col32(c: Color32) -> Option<u32> {
    (c.a() > 0).then(|| ((c.r() as u32) << 16) | ((c.g() as u32) << 8) | c.b() as u32)
}

/// Converts recorded egui shapes (from the chart painter) into print operations.
fn shape_to_ops(shape: &egui::Shape, out: &mut Vec<Op>) {
    use egui::Shape;
    match shape {
        Shape::Vec(v) => v.iter().for_each(|s| shape_to_ops(s, out)),
        Shape::Rect(rs) => {
            let r = rs.rect;
            if let Some(c) = col32(rs.fill) {
                out.push(Op::Fill { rect: PRect::new(r.left(), r.top(), r.width(), r.height()), color: c });
            }
            if rs.stroke.width > 0.0 {
                if let Some(c) = col32(rs.stroke.color) {
                    let pts = vec![(r.left(), r.top()), (r.right(), r.top()), (r.right(), r.bottom()), (r.left(), r.bottom()), (r.left(), r.top())];
                    out.push(Op::Path { pts, width: rs.stroke.width, color: c });
                }
            }
        }
        Shape::LineSegment { points, stroke } => {
            if let Some(c) = col32(stroke.color) {
                out.push(Op::Line { x1: points[0].x, y1: points[0].y, x2: points[1].x, y2: points[1].y, width: stroke.width, color: c, dashed: false });
            }
        }
        Shape::Path(ps) => {
            let pts: Vec<(f32, f32)> = ps.points.iter().map(|p| (p.x, p.y)).collect();
            if ps.closed {
                if let Some(c) = col32(ps.fill) {
                    out.push(Op::Poly { pts: pts.clone(), color: c });
                }
            }
            if ps.stroke.width > 0.0 {
                if let egui::epaint::ColorMode::Solid(sc) = ps.stroke.color {
                    if let Some(c) = col32(sc) {
                        let mut p = pts;
                        if ps.closed && !p.is_empty() {
                            p.push(p[0]);
                        }
                        out.push(Op::Path { pts: p, width: ps.stroke.width, color: c });
                    }
                }
            }
        }
        Shape::Circle(cs) => {
            let n = 24;
            let pts: Vec<(f32, f32)> = (0..n)
                .map(|i| {
                    let a = i as f32 / n as f32 * std::f32::consts::TAU;
                    (cs.center.x + cs.radius * a.cos(), cs.center.y + cs.radius * a.sin())
                })
                .collect();
            if let Some(c) = col32(cs.fill) {
                out.push(Op::Poly { pts: pts.clone(), color: c });
            }
            if cs.stroke.width > 0.0 {
                if let Some(c) = col32(cs.stroke.color) {
                    let mut p = pts;
                    p.push(p[0]);
                    out.push(Op::Path { pts: p, width: cs.stroke.width, color: c });
                }
            }
        }
        Shape::Text(ts) => {
            let g = &ts.galley;
            let text = g.job.text.clone();
            if text.trim().is_empty() {
                return;
            }
            let fmt = g.job.sections.first().map(|s| s.format.clone()).unwrap_or_default();
            let color = col32(if fmt.color == Color32::PLACEHOLDER { ts.fallback_color } else { fmt.color }).unwrap_or(0);
            let size = g.size();
            // Text turned a quarter counter-clockwise (vertical axis titles) prints rotated.
            let up = (ts.angle + std::f32::consts::FRAC_PI_2).abs() < 0.01;
            let rect = if up { PRect::new(ts.pos.x, ts.pos.y - size.x - 2.0, size.y, size.x + 4.0) } else { PRect::new(ts.pos.x - 2.0, ts.pos.y, size.x + 4.0, size.y) };
            // the chart was drawn in point units, so egui's font size is already in points
            let font = print::FontSpec { family: "Segoe UI".into(), size: fmt.font_id.size, bold: false, italic: false, underline: false, strike: false };
            out.push(Op::Text(TextOp {
                rect,
                clip: PRect::new(rect.x - 2.0, rect.y - 2.0, rect.w + 4.0, rect.h + 4.0),
                text,
                font,
                color,
                halign: if up { Align::Center } else { Align::Left },
                valign: if up { VAlign::Center } else { VAlign::Top },
                wrap: false,
                rotation: if up { 90 } else { 0 },
                number: false,
                indent: 0.0,
            }));
        }
        Shape::Mesh(m) => {
            // Filled areas (pie slices, areas) are strips of quads: print each strip as one
            // polygon so no seams show between the pieces.
            let v = &m.vertices;
            let nq = v.len() / 4;
            let quads = v.len() % 4 == 0
                && m.indices.len() == nq * 6
                && m.indices.chunks_exact(6).enumerate().all(|(q, t)| {
                    let b = q as u32 * 4;
                    t == [b, b + 1, b + 2, b, b + 2, b + 3]
                });
            let at = |i: usize| (v[i].pos.x, v[i].pos.y);
            let near = |a: usize, b: usize| (v[a].pos - v[b].pos).length() < 0.01;
            if quads {
                let mut q = 0;
                while q < nq {
                    let col = v[q * 4].color;
                    let (mut top, mut bottom) = (vec![at(q * 4 + 1), at(q * 4 + 2)], vec![at(q * 4), at(q * 4 + 3)]);
                    let mut k = q + 1;
                    while k < nq && v[k * 4].color == col && near(k * 4 + 1, (k - 1) * 4 + 2) && near(k * 4, (k - 1) * 4 + 3) {
                        top.push(at(k * 4 + 2));
                        bottom.push(at(k * 4 + 3));
                        k += 1;
                    }
                    if let Some(c) = col32(col) {
                        top.extend(bottom.into_iter().rev());
                        out.push(Op::Poly { pts: top, color: c });
                    }
                    q = k;
                }
            } else {
                for t in m.indices.chunks_exact(3) {
                    if let Some(c) = col32(v[t[0] as usize].color) {
                        let pts = vec![at(t[0] as usize), at(t[1] as usize), at(t[2] as usize)];
                        out.push(Op::Poly { pts, color: c });
                    }
                }
            }
        }
        _ => {}
    }
}

/// Draws a chart with the on-screen chart painter and records the result as print operations.
pub(super) fn chart_ops(ctx: &egui::Context, wb: &crate::model::Workbook, si: usize, k: usize, r: PRect) -> Vec<Op> {
    let Some(ch) = wb.sheets.get(si).and_then(|s| s.charts.get(k)) else { return Vec::new() };
    let data = crate::chart::resolve(wb, ch);
    let layer = egui::LayerId::new(egui::Order::Background, egui::Id::new("print_chart_recorder"));
    let start = ctx.graphics_mut(|g| g.entry(layer).next_idx());
    let painter = egui::Painter::new(ctx.clone(), layer, Rect::EVERYTHING);
    let rect = Rect::from_min_size(Pos2::new(r.x, r.y), Vec2::new(r.w, r.h));
    let z = r.w / ch.size.0.max(1.0);
    super::chart_view::draw_chart(&painter, rect, ch, &data, z);
    let shapes: Vec<egui::Shape> = ctx.graphics_mut(|g| {
        let pl = g.entry(layer);
        let out: Vec<egui::Shape> = pl.all_entries().skip(start.0).map(|c| c.shape.clone()).collect();
        let end = pl.next_idx().0;
        for i in start.0..end {
            pl.reset_shape(egui::layers::ShapeIdx(i));
        }
        out
    });
    let mut ops = Vec::new();
    for s in &shapes {
        shape_to_ops(s, &mut ops);
    }
    ops
}

fn two_line(title: &str, sub: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(title, 0.0, TextFormat { font_id: theme::ui_bold(12.5), color: theme::text(), ..Default::default() });
    if !sub.is_empty() {
        job.append(&format!("\n{sub}"), 0.0, TextFormat { font_id: theme::ui_font(11.0), color: theme::text_dim(), ..Default::default() });
    }
    job
}

/// A settings dropdown in the familiar two-line style; returns the picked index.
fn setting(ui: &mut egui::Ui, id: &str, items: &[(String, String)], sel: Option<usize>, current: Option<(String, String)>) -> Option<usize> {
    let mut picked = None;
    let shown = match (sel, current) {
        (Some(i), _) => items[i].clone(),
        (None, Some(c)) => c,
        (None, None) => (String::new(), String::new()),
    };
    egui::ComboBox::from_id_salt(id).width(300.0).height(500.0).selected_text(two_line(&shown.0, &shown.1)).show_ui(ui, |ui| {
        for (i, (t, s)) in items.iter().enumerate() {
            if ui.selectable_label(sel == Some(i), two_line(t, s)).clicked() {
                picked = Some(i);
            }
        }
    });
    picked
}

fn unit(metric: bool, inches: f32) -> String {
    if metric { format!("{:.2} cm", inches * 2.54) } else { format!("{:.2}\"", inches) }
}

fn margins_caption(m: &Margins, metric: bool) -> String {
    format!("Top: {} Bottom: {} Left: {} Right: {}", unit(metric, m.top), unit(metric, m.bottom), unit(metric, m.left), unit(metric, m.right))
}

impl App {
    fn hash_print_key(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.edits.hash(&mut h);
        self.wb.active.hash(&mut h);
        format!("{:?}", self.print.what).hash(&mut h);
        self.print.ignore_area.hash(&mut h);
        format!("{:?}", self.sel_clipped()).hash(&mut h);
        for s in &self.wb.sheets {
            format!("{:?}", s.page).hash(&mut h);
        }
        h.finish()
    }

    /// Laid-out pages for the current print settings (cached).
    pub fn print_pages(&mut self, ctx: &egui::Context) -> Rc<Vec<Page>> {
        let key = self.hash_print_key();
        if let Some((k, p)) = &self.print.cache {
            if *k == key {
                return p.clone();
            }
        }
        let file = self.wb.file_name();
        let path = self.wb.path.as_ref().and_then(|p| p.parent()).map(|d| format!("{}\\", d.display())).unwrap_or_default();
        let opts = LayoutOptions {
            what: self.print.what,
            selection: Some(self.sel_clipped()),
            ignore_print_area: self.print.ignore_area,
            file_name: &file,
            file_path: &path,
        };
        let wb = &self.wb;
        let pages = Rc::new(print::layout(wb, &opts, &|si, k, r| chart_ops(ctx, wb, si, k, r)));
        self.print.cache = Some((key, pages.clone()));
        pages
    }

    /// Pages within the "Pages: from-to" range.
    fn selected_pages(&self, pages: &[Page]) -> Vec<Page> {
        let n = pages.len() as u32;
        let from = if self.print.from == 0 { 1 } else { self.print.from.min(n.max(1)) };
        let to = if self.print.to == 0 { n } else { self.print.to.min(n).max(from) };
        pages.iter().skip(from as usize - 1).take((to + 1 - from) as usize).cloned().collect()
    }

    fn ensure_printers(&mut self) {
        if self.print.printers.is_none() {
            let list = gdi::printers();
            if self.print.printer.is_empty() || !list.iter().any(|p| p.name == self.print.printer) {
                self.print.printer = list.iter().find(|p| p.is_default).or(list.first()).map(|p| p.name.clone()).unwrap_or_default();
            }
            self.print.printers = Some(list);
        }
    }

    /// Updates the active sheet's page setup (undoable) and shows page breaks for it.
    pub fn set_page(&mut self, f: impl FnOnce(&mut PageSetup)) {
        let si = self.wb.active;
        self.push_undo(&[Scope::Sheet(si)]);
        f(&mut self.wb.sheets[si].page);
        self.print.show_breaks.insert(si);
        self.dirty = true;
        self.edits += 1;
    }

    pub fn poll_print(&mut self) {
        let done = self.print.result.lock().ok().and_then(|mut g| g.take());
        if let Some(r) = done {
            self.print.busy = false;
            match r {
                Ok(msg) => self.status = Some(msg),
                Err(e) => self.dialog = Some(Dialog::Message { title: "Print".into(), text: e }),
            }
        }
    }

    fn start_print(&mut self, ctx: &egui::Context) {
        self.ensure_printers();
        if self.print.printer.is_empty() {
            self.dialog = Some(Dialog::Message { title: "Print".into(), text: "No printer is installed. Use Export to save a PDF instead.".into() });
            return;
        }
        let pages = self.print_pages(ctx);
        let pages = self.selected_pages(&pages);
        if pages.is_empty() {
            self.dialog = Some(Dialog::Message { title: "Print".into(), text: "We didn't find anything to print.".into() });
            return;
        }
        let settings = PrintSettings {
            printer: self.print.printer.clone(),
            copies: self.print.copies.clamp(1, 999) as u16,
            collate: self.print.collate,
            duplex: self.print.duplex,
            devmode: self.print.devmode.clone(),
            doc_name: self.wb.file_name(),
            output_file: None,
        };
        let n = pages.len();
        let res = self.print.result.clone();
        self.print.busy = true;
        self.status = Some(format!("Printing {n} page(s) on {}...", settings.printer));
        std::thread::spawn(move || {
            let r = gdi::print(&pages, &settings).map(|_| format!("Sent {n} page(s) to {}.", settings.printer));
            if let Ok(mut g) = res.lock() {
                *g = Some(r);
            }
        });
        self.backstage = None;
    }

    pub fn export_pdf(&mut self, ctx: &egui::Context) {
        let pages = self.print_pages(ctx);
        let name = format!(
            "{}.pdf",
            self.wb.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Book1".into())
        );
        let mut d = rfd::FileDialog::new().set_title("Publish as PDF").add_filter("PDF", &["pdf"]).set_file_name(name);
        if let Some(dir) = self.wb.path.as_ref().and_then(|p| p.parent()) {
            d = d.set_directory(dir);
        }
        if let Some(p) = d.save_file() {
            // Prefer Windows' "Microsoft Print to PDF" (real fonts, embedded); fall back to the built-in writer.
            self.ensure_printers();
            let ms_pdf = self.print.printers.as_ref().and_then(|l| l.iter().find(|x| x.name.eq_ignore_ascii_case("Microsoft Print to PDF")).map(|x| x.name.clone()));
            let via_driver = ms_pdf.and_then(|printer| {
                let settings = PrintSettings {
                    printer,
                    copies: 1,
                    collate: true,
                    duplex: Duplex::OneSided,
                    devmode: None,
                    doc_name: self.wb.file_name(),
                    output_file: Some(p.clone()),
                };
                gdi::print(&pages, &settings).ok().map(|_| pages.len())
            });
            let result = match via_driver {
                Some(n) => Ok(n),
                None => print::pdf::export(&pages, &p),
            };
            match result {
                Ok(n) => {
                    self.status = Some(format!("Exported {n} page(s) to {}", p.display()));
                    self.backstage = None;
                    let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler"]).arg(&p).spawn();
                }
                Err(e) => self.dialog = Some(Dialog::Message { title: "Export".into(), text: e }),
            }
        }
    }

    // ------------------------------------------------------------------
    // File > Print
    // ------------------------------------------------------------------

    pub fn print_page_ui(&mut self, ui: &mut egui::Ui) {
        self.ensure_printers();
        let ctx = ui.ctx().clone();
        let pages = self.print_pages(&ctx);
        let si = self.wb.active;
        self.print.show_breaks.insert(si);
        let metric = metric_locale();
        let full = ui.available_rect_before_wrap();
        let left_w = 330.0;
        let left = Rect::from_min_size(full.min, Vec2::new(left_w, full.height()));
        let right = Rect::from_min_max(Pos2::new(full.left() + left_w + 24.0, full.top() - 36.0), Pos2::new(full.right() + 36.0, full.bottom() + 36.0));

        // ---------------- settings column
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(left).layout(egui::Layout::top_down(egui::Align::Min)));
        let ui2 = &mut child;
        egui::ScrollArea::vertical().id_salt("print_settings").show(ui2, |ui| {
            ui.set_width(left_w - 10.0);
            ui.label(RichText::new("Print").size(28.0));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let (pg, pi) = self.icon('\u{E749}', "🖶");
                let mut job = LayoutJob::default();
                job.append(&pg, 0.0, TextFormat { font_id: if pi { theme::icon_font(28.0) } else { theme::ui_font(28.0) }, color: theme::text(), ..Default::default() });
                job.append("\nPrint", 0.0, TextFormat { font_id: theme::ui_font(13.0), color: theme::text(), ..Default::default() });
                job.halign = egui::Align::Center;
                let btn = super::ribbon::big_btn(ui, job, 76.0, false);
                if btn.on_hover_text("Print (Ctrl+P)").clicked() && !self.print.busy {
                    self.start_print(&ctx);
                }
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.label("Copies:");
                        ui.add(egui::DragValue::new(&mut self.print.copies).range(1..=999));
                    });
                });
            });
            ui.add_space(12.0);
            ui.label(RichText::new("Printer").size(15.0).strong());
            let printers = self.print.printers.clone().unwrap_or_default();
            if printers.is_empty() {
                ui.label(RichText::new("No printers found").color(theme::text_dim()));
            } else {
                let cur = printers.iter().find(|p| p.name == self.print.printer).cloned();
                let shown = cur.as_ref().map(|p| (p.name.clone(), p.status.clone())).unwrap_or_default();
                egui::ComboBox::from_id_salt("printer").width(300.0).selected_text(two_line(&shown.0, &shown.1)).show_ui(ui, |ui| {
                    for p in &printers {
                        let sub = if p.is_default { format!("{} (default)", p.status) } else { p.status.clone() };
                        if ui.selectable_label(p.name == self.print.printer, two_line(&p.name, &sub)).clicked() {
                            self.print.printer = p.name.clone();
                            self.print.devmode = None;
                        }
                    }
                });
                if ui.link("Printer Properties").clicked() {
                    let mut dm = self.print.devmode.clone();
                    if gdi::printer_properties(self.hwnd, &self.print.printer, &mut dm) {
                        self.print.devmode = dm;
                    }
                }
            }
            ui.add_space(12.0);
            ui.label(RichText::new("Settings").size(15.0).strong());
            ui.add_space(2.0);
            // what to print
            let whats = [
                ("Print Active Sheets".to_string(), "Only print the active sheets".to_string()),
                ("Print Entire Workbook".to_string(), "Print the entire workbook".to_string()),
                ("Print Selection".to_string(), "Only print the current selection".to_string()),
            ];
            let wi = match self.print.what {
                PrintWhat::ActiveSheet => 0,
                PrintWhat::Workbook => 1,
                PrintWhat::Selection => 2,
            };
            if let Some(i) = setting(ui, "p_what", &whats, Some(wi), None) {
                self.print.what = [PrintWhat::ActiveSheet, PrintWhat::Workbook, PrintWhat::Selection][i];
                self.print.page = 0;
            }
            ui.checkbox(&mut self.print.ignore_area, "Ignore Print Area");
            ui.horizontal(|ui| {
                ui.label("Pages:");
                ui.add(egui::DragValue::new(&mut self.print.from).range(0..=9999).custom_formatter(|v, _| if v == 0.0 { String::new() } else { format!("{v}") }));
                ui.label("to");
                ui.add(egui::DragValue::new(&mut self.print.to).range(0..=9999).custom_formatter(|v, _| if v == 0.0 { String::new() } else { format!("{v}") }));
            });
            let duplex = [
                ("Print One Sided".to_string(), "Only print on one side of the page".to_string()),
                ("Print on Both Sides".to_string(), "Flip pages on long edge".to_string()),
                ("Print on Both Sides".to_string(), "Flip pages on short edge".to_string()),
            ];
            let di = match self.print.duplex {
                Duplex::OneSided => 0,
                Duplex::LongEdge => 1,
                Duplex::ShortEdge => 2,
            };
            if let Some(i) = setting(ui, "p_duplex", &duplex, Some(di), None) {
                self.print.duplex = [Duplex::OneSided, Duplex::LongEdge, Duplex::ShortEdge][i];
            }
            let coll = [("Collated".to_string(), "1,2,3   1,2,3   1,2,3".to_string()), ("Uncollated".to_string(), "1,1,1   2,2,2   3,3,3".to_string())];
            if let Some(i) = setting(ui, "p_collate", &coll, Some(if self.print.collate { 0 } else { 1 }), None) {
                self.print.collate = i == 0;
            }
            let ps = self.sheet().page.clone();
            let orient = [("Portrait Orientation".to_string(), String::new()), ("Landscape Orientation".to_string(), String::new())];
            if let Some(i) = setting(ui, "p_orient", &orient, Some(ps.landscape as usize), None) {
                self.set_page(|p| p.landscape = i == 1);
            }
            let papers: Vec<(String, String)> = Paper::ALL.iter().map(|p| (p.name().to_string(), p.caption(metric))).collect();
            let pi = Paper::ALL.iter().position(|p| *p == ps.paper);
            if let Some(i) = setting(ui, "p_paper", &papers, pi, None) {
                self.set_page(|p| p.paper = Paper::ALL[i]);
            }
            let margins = [
                ("Normal Margins".to_string(), margins_caption(&Margins::NORMAL, metric)),
                ("Wide Margins".to_string(), margins_caption(&Margins::WIDE, metric)),
                ("Narrow Margins".to_string(), margins_caption(&Margins::NARROW, metric)),
                ("Custom Margins...".to_string(), String::new()),
            ];
            let mi = match ps.margins.preset() {
                Some("Normal") => Some(0),
                Some("Wide") => Some(1),
                Some("Narrow") => Some(2),
                _ => None,
            };
            let custom = ("Custom Margins".to_string(), margins_caption(&ps.margins, metric));
            match setting(ui, "p_margins", &margins, mi, Some(custom)) {
                Some(3) => self.open_page_setup(1),
                Some(i) => self.set_page(|p| p.margins = [Margins::NORMAL, Margins::WIDE, Margins::NARROW][i]),
                None => {}
            }
            let scalings = [
                ("No Scaling".to_string(), "Print sheets at their actual size".to_string()),
                ("Fit Sheet on One Page".to_string(), "Shrink the printout so that it fits on one page".to_string()),
                ("Fit All Columns on One Page".to_string(), "Shrink the printout so that it is one page wide".to_string()),
                ("Fit All Rows on One Page".to_string(), "Shrink the printout so that it is one page high".to_string()),
                ("Custom Scaling Options...".to_string(), String::new()),
            ];
            let si_ = match ps.scaling {
                Scaling::Percent(100) => Some(0),
                Scaling::Fit { wide: 1, tall: 1 } => Some(1),
                Scaling::Fit { wide: 1, tall: 0 } => Some(2),
                Scaling::Fit { wide: 0, tall: 1 } => Some(3),
                _ => None,
            };
            let custom = (
                "Custom Scaling".to_string(),
                match ps.scaling {
                    Scaling::Percent(p) => format!("Print at {p}% of actual size"),
                    Scaling::Fit { wide, tall } => format!("Fit to {} page(s) wide by {} tall", if wide == 0 { "auto".into() } else { wide.to_string() }, if tall == 0 { "auto".into() } else { tall.to_string() }),
                },
            );
            match setting(ui, "p_scale", &scalings, si_, Some(custom)) {
                Some(4) => self.open_page_setup(0),
                Some(i) => {
                    let s = [Scaling::Percent(100), Scaling::Fit { wide: 1, tall: 1 }, Scaling::Fit { wide: 1, tall: 0 }, Scaling::Fit { wide: 0, tall: 1 }][i];
                    self.set_page(|p| p.scaling = s);
                }
                None => {}
            }
            ui.add_space(6.0);
            if ui.link("Page Setup").clicked() {
                self.open_page_setup(0);
            }
            if self.print.busy {
                ui.add_space(8.0);
                ui.label(RichText::new("Sending to the printer...").color(theme::text_dim()));
            }
        });

        // ---------------- preview
        let painter = ui.painter_at(right);
        painter.rect_filled(right, 0.0, Color32::from_rgb(0xE6, 0xE6, 0xE6));
        let nav_h = 34.0;
        let area = Rect::from_min_max(right.min + Vec2::new(20.0, 20.0), Pos2::new(right.right() - 20.0, right.bottom() - nav_h - 8.0));
        let n = pages.len();
        if n == 0 {
            painter.text(area.center(), Align2::CENTER_CENTER, "We didn't find anything to print.", theme::ui_font(15.0), theme::text_dim());
            return;
        }
        self.print.page = self.print.page.min(n - 1);
        let page = &pages[self.print.page];
        let scale = if self.print.zoom_to_page {
            (area.width() / page.width).min(area.height() / page.height)
        } else {
            (96.0 / 72.0f32).min(area.width() / page.width)
        };
        let size = Vec2::new(page.width * scale, page.height * scale);
        let page_rect = Rect::from_center_size(area.center(), size);
        let page_rect = if size.y > area.height() { Rect::from_min_size(Pos2::new(page_rect.left(), area.top()), size) } else { page_rect };
        let prev_clip = painter.with_clip_rect(area.expand(10.0));
        self.paint_page(&prev_clip, page, page_rect);
        if self.print.show_margins {
            let m = self.wb.sheets[page.sheet].page.margins;
            let stroke = Stroke::new(1.0, Color32::from_gray(90));
            for x in [m.left * 72.0, page.width - m.right * 72.0] {
                let xx = page_rect.left() + x * scale;
                prev_clip.extend(egui::Shape::dashed_line(&[Pos2::new(xx, page_rect.top()), Pos2::new(xx, page_rect.bottom())], stroke, 4.0, 3.0));
            }
            for y in [m.top * 72.0, page.height - m.bottom * 72.0, m.header * 72.0, page.height - m.footer * 72.0] {
                let yy = page_rect.top() + y * scale;
                prev_clip.extend(egui::Shape::dashed_line(&[Pos2::new(page_rect.left(), yy), Pos2::new(page_rect.right(), yy)], stroke, 4.0, 3.0));
            }
        }
        // mouse wheel / keys page through the preview
        let resp = ui.interact(area, egui::Id::new("print_preview"), Sense::click());
        if resp.hovered() {
            let dy = ui.input(|i| i.raw_scroll_delta.y);
            if dy < 0.0 && self.print.page + 1 < n {
                self.print.page += 1;
            } else if dy > 0.0 && self.print.page > 0 {
                self.print.page -= 1;
            }
        }
        if resp.clicked() {
            self.print.zoom_to_page = !self.print.zoom_to_page;
        }
        let (pgdn, pgup) = ui.input(|i| (i.key_pressed(egui::Key::PageDown), i.key_pressed(egui::Key::PageUp)));
        if pgdn && self.print.page + 1 < n {
            self.print.page += 1;
        }
        if pgup && self.print.page > 0 {
            self.print.page -= 1;
        }

        // navigation bar
        let nav = Rect::from_min_max(Pos2::new(right.left() + 12.0, right.bottom() - nav_h), Pos2::new(right.right() - 12.0, right.bottom() - 4.0));
        let mut nav_ui = ui.new_child(egui::UiBuilder::new().max_rect(nav).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let (l, li) = self.icon(ic::LEFT, "◀");
        let (r, ri) = self.icon(ic::RIGHT, "▶");
        let f = |s: String, icon: bool| if icon { RichText::new(s).font(theme::icon_font(11.0)) } else { RichText::new(s) };
        if nav_ui.add_enabled(self.print.page > 0, egui::Button::new(f(l, li)).frame(false)).clicked() {
            self.print.page -= 1;
        }
        let mut cur = self.print.page as u32 + 1;
        if nav_ui.add(egui::DragValue::new(&mut cur).range(1..=n as u32)).changed() {
            self.print.page = cur as usize - 1;
        }
        nav_ui.label(format!("of {n}"));
        if nav_ui.add_enabled(self.print.page + 1 < n, egui::Button::new(f(r, ri)).frame(false)).clicked() {
            self.print.page += 1;
        }
        nav_ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let zt = if self.print.zoom_to_page { "Zoom to Page ✓" } else { "Zoom to Page" };
            if ui.button(zt).clicked() {
                self.print.zoom_to_page = !self.print.zoom_to_page;
            }
            let mt = if self.print.show_margins { "Show Margins ✓" } else { "Show Margins" };
            if ui.button(mt).clicked() {
                self.print.show_margins = !self.print.show_margins;
            }
        });
    }

    /// Paints one laid-out page into a screen rectangle (preview).
    pub fn paint_page(&mut self, painter: &egui::Painter, page: &Page, r: Rect) {
        let s = r.width() / page.width;
        painter.rect_filled(r.translate(Vec2::new(3.0, 3.0)), 0.0, Color32::from_black_alpha(40));
        painter.rect_filled(r, 0.0, Color32::WHITE);
        let to = |x: f32, y: f32| Pos2::new(r.left() + x * s, r.top() + y * s);
        let p = painter.with_clip_rect(r.intersect(painter.clip_rect()));
        for op in &page.ops {
            match op {
                Op::Fill { rect, color } => {
                    p.rect_filled(Rect::from_min_max(to(rect.x, rect.y), to(rect.right(), rect.bottom())), 0.0, rgb(*color));
                }
                Op::Line { x1, y1, x2, y2, width, color, dashed } => {
                    let stroke = Stroke::new((width * s).max(0.6), rgb(*color));
                    let a = to(*x1, *y1);
                    let b = to(*x2, *y2);
                    if *dashed {
                        p.extend(egui::Shape::dashed_line(&[a, b], stroke, 3.0 * s.max(0.5), 2.0 * s.max(0.5)));
                    } else {
                        p.line_segment([a, b], stroke);
                    }
                }
                Op::Poly { pts, color } => {
                    let pts: Vec<Pos2> = pts.iter().map(|(x, y)| to(*x, *y)).collect();
                    p.add(egui::Shape::convex_polygon(pts, rgb(*color), Stroke::NONE));
                }
                Op::Path { pts, width, color } => {
                    let pts: Vec<Pos2> = pts.iter().map(|(x, y)| to(*x, *y)).collect();
                    p.add(egui::Shape::line(pts, Stroke::new((width * s).max(0.6), rgb(*color))));
                }
                Op::Text(t) => self.paint_text_op(&p, t, s, r.min),
            }
        }
    }

    fn paint_text_op(&mut self, p: &egui::Painter, t: &TextOp, s: f32, origin: Pos2) {
        // page units are points and `s` is screen pixels per point, so the em size is size * s
        let em = t.font.size * s;
        if em < 1.5 {
            return;
        }
        let fam = self.fonts.cell_family(Some(&t.font.family), t.font.bold, t.font.italic);
        let font = egui::FontId::new(em, fam);
        let color = rgb(t.color);
        let fmt = TextFormat {
            font_id: font.clone(),
            color,
            underline: if t.font.underline { Stroke::new(1.0, color) } else { Stroke::NONE },
            strikethrough: if t.font.strike { Stroke::new(1.0, color) } else { Stroke::NONE },
            ..Default::default()
        };
        let to = |x: f32, y: f32| Pos2::new(origin.x + x * s, origin.y + y * s);
        let rect = Rect::from_min_max(to(t.rect.x, t.rect.y), to(t.rect.right(), t.rect.bottom()));
        let clip = Rect::from_min_max(to(t.clip.x, t.clip.y), to(t.clip.right(), t.clip.bottom()));
        let pad = 2.0 * s;
        let indent = t.indent * s;
        let mut job = LayoutJob::single_section(t.text.clone(), fmt.clone());
        if t.wrap {
            job.wrap.max_width = (rect.width() - 2.0 * pad - indent).max(1.0);
        }
        let mut g = p.layout_job(job);
        if t.number && !t.wrap && g.size().x > rect.width() - 2.0 * pad {
            let hw = p.layout_no_wrap("#".into(), font.clone(), color).size().x.max(0.5);
            let n = ((rect.width() - 2.0 * pad) / hw).floor().max(1.0) as usize;
            g = p.layout_job(LayoutJob::single_section("#".repeat(n), fmt));
        }
        let gs = g.size();
        let cp = p.with_clip_rect(clip.intersect(p.clip_rect()));
        if t.rotation != 0 && t.rotation != 255 {
            let r = t.rotation as f32;
            let deg = if r <= 90.0 { -r } else { r - 90.0 };
            let a = deg.to_radians();
            let (sn, cs) = a.sin_cos();
            let rot = |v: Vec2| Vec2::new(v.x * cs - v.y * sn, v.x * sn + v.y * cs);
            let pts = [rot(Vec2::ZERO), rot(Vec2::new(gs.x, 0.0)), rot(Vec2::new(0.0, gs.y)), rot(gs)];
            let minx = pts.iter().map(|q| q.x).fold(f32::INFINITY, f32::min);
            let maxx = pts.iter().map(|q| q.x).fold(f32::NEG_INFINITY, f32::max);
            let miny = pts.iter().map(|q| q.y).fold(f32::INFINITY, f32::min);
            let maxy = pts.iter().map(|q| q.y).fold(f32::NEG_INFINITY, f32::max);
            let x = match t.halign {
                Align::Left => rect.left() + pad - minx,
                Align::Right => rect.right() - pad - maxx,
                Align::Center => rect.center().x - (minx + maxx) / 2.0,
            };
            let y = match t.valign {
                VAlign::Top => rect.top() + 1.0 - miny,
                VAlign::Center => rect.center().y - (miny + maxy) / 2.0,
                VAlign::Bottom => rect.bottom() - 1.0 - maxy,
            };
            cp.add(egui::epaint::TextShape::new(Pos2::new(x, y), g, color).with_angle(a));
            return;
        }
        let x = match t.halign {
            Align::Left => rect.left() + pad + indent,
            Align::Right => rect.right() - pad - gs.x,
            Align::Center => rect.center().x - gs.x / 2.0,
        };
        let y = match t.valign {
            VAlign::Top => rect.top() + 1.0,
            VAlign::Center => rect.center().y - gs.y / 2.0,
            VAlign::Bottom => rect.bottom() - gs.y - 1.0,
        };
        cp.galley(Pos2::new(x, y), g, color);
    }

    // ------------------------------------------------------------------
    // Page breaks in Normal view
    // ------------------------------------------------------------------

    pub fn paint_page_breaks(&mut self, painter: &egui::Painter, lay: &Layout) {
        let si = self.wb.active;
        if !self.print.show_breaks.contains(&si) {
            return;
        }
        let key = self.edits ^ ((si as u64) << 48);
        let stale = !matches!(&self.print.breaks_cache, Some((k, s, ..)) if *k == key && *s == si);
        if stale {
            let (r, c) = print::page_breaks(&self.wb, si);
            self.print.breaks_cache = Some((key, si, r, c));
        }
        let Some((_, _, rows, cols)) = &self.print.breaks_cache else { return };
        let ps = &self.wb.sheets[si].page;
        for &r in rows {
            if let Some(&(_, y, _)) = lay.rows.iter().find(|x| x.0 == r) {
                let manual = ps.row_breaks.contains(&r);
                let stroke = Stroke::new(if manual { 1.5 } else { 1.0 }, Color32::from_gray(if manual { 40 } else { 110 }));
                painter.extend(egui::Shape::dashed_line(&[Pos2::new(lay.cells.left(), y), Pos2::new(lay.cells.right(), y)], stroke, 5.0, 3.0));
            }
        }
        for &c in cols {
            if let Some(&(_, x, _)) = lay.cols.iter().find(|x| x.0 == c) {
                let manual = ps.col_breaks.contains(&c);
                let stroke = Stroke::new(if manual { 1.5 } else { 1.0 }, Color32::from_gray(if manual { 40 } else { 110 }));
                painter.extend(egui::Shape::dashed_line(&[Pos2::new(x, lay.cells.top()), Pos2::new(x, lay.cells.bottom())], stroke, 5.0, 3.0));
            }
        }
    }

    // ------------------------------------------------------------------
    // Page Layout commands
    // ------------------------------------------------------------------

    pub fn set_print_area(&mut self) {
        let sel = self.sel_clipped();
        self.set_page(|p| p.print_area = Some(sel));
    }

    pub fn clear_print_area(&mut self) {
        self.set_page(|p| p.print_area = None);
    }

    /// Insert Page Break: above the active row and/or left of the active column.
    pub fn insert_page_break(&mut self) {
        let sel = self.sel();
        let (r, c) = self.active();
        self.set_page(|p| {
            if sel.is_full_rows() {
                if sel.r1 > 0 {
                    p.row_breaks.insert(sel.r1);
                }
            } else if sel.is_full_cols() {
                if sel.c1 > 0 {
                    p.col_breaks.insert(sel.c1);
                }
            } else {
                if r > 0 {
                    p.row_breaks.insert(r);
                }
                if c > 0 {
                    p.col_breaks.insert(c);
                }
            }
        });
    }

    pub fn remove_page_break(&mut self) {
        let (r, c) = self.active();
        self.set_page(|p| {
            p.row_breaks.remove(&r);
            p.col_breaks.remove(&c);
        });
    }

    pub fn reset_page_breaks(&mut self) {
        self.set_page(|p| {
            p.row_breaks.clear();
            p.col_breaks.clear();
        });
    }

    pub fn open_page_setup(&mut self, tab: usize) {
        let ps = self.sheet().page.clone();
        let fmt_rows = |t: Option<(u32, u32)>| t.map(|(a, b)| format!("${}:${}", a + 1, b + 1)).unwrap_or_default();
        let fmt_cols = |t: Option<(u32, u32)>| {
            t.map(|(a, b)| format!("${}:${}", crate::model::col_name(a), crate::model::col_name(b))).unwrap_or_default()
        };
        let area = ps
            .print_area
            .map(|a| format!("${}${}:${}${}", crate::model::col_name(a.c1), a.r1 + 1, crate::model::col_name(a.c2), a.r2 + 1))
            .unwrap_or_default();
        self.dialog = Some(Dialog::PageSetup(Box::new(PageSetupDlg {
            tab,
            area,
            rows: fmt_rows(ps.title_rows),
            cols: fmt_cols(ps.title_cols),
            first: ps.first_page.map(|n| n.to_string()).unwrap_or_else(|| "Auto".into()),
            ps,
            error: None,
        })));
    }

    /// The Page Setup dialog; returns false when it closes.
    pub fn page_setup_dialog(&mut self, ctx: &egui::Context, d: &mut PageSetupDlg) -> bool {
        let mut keep = true;
        let metric = metric_locale();
        let mut go_print = false;
        egui::Window::new("Page Setup").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, -30.0]).show(ctx, |ui| {
            ui.set_min_width(520.0);
            ui.horizontal(|ui| {
                for (i, t) in ["Page", "Margins", "Header/Footer", "Sheet"].iter().enumerate() {
                    if ui.selectable_label(d.tab == i, *t).clicked() {
                        d.tab = i;
                    }
                }
            });
            ui.separator();
            ui.set_min_height(330.0);
            match d.tab {
                0 => {
                    ui.label(RichText::new("Orientation").strong());
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut d.ps.landscape, false, "Portrait");
                        ui.radio_value(&mut d.ps.landscape, true, "Landscape");
                    });
                    ui.add_space(8.0);
                    ui.label(RichText::new("Scaling").strong());
                    let (mut pct, mut wide, mut tall) = match d.ps.scaling {
                        Scaling::Percent(p) => (p, 1, 1),
                        Scaling::Fit { wide, tall } => (100, wide, tall),
                    };
                    let mut fit = matches!(d.ps.scaling, Scaling::Fit { .. });
                    ui.horizontal(|ui| {
                        if ui.radio(!fit, "Adjust to:").clicked() {
                            fit = false;
                        }
                        ui.add_enabled(!fit, egui::DragValue::new(&mut pct).range(10..=400).suffix(" %"));
                        ui.label("normal size");
                    });
                    ui.horizontal(|ui| {
                        if ui.radio(fit, "Fit to:").clicked() {
                            fit = true;
                        }
                        let auto = |v: f64| if v == 0.0 { String::new() } else { format!("{v}") };
                        ui.add_enabled(fit, egui::DragValue::new(&mut wide).range(0..=32767).custom_formatter(move |v, _| auto(v)));
                        ui.label("page(s) wide by");
                        ui.add_enabled(fit, egui::DragValue::new(&mut tall).range(0..=32767).custom_formatter(move |v, _| auto(v)));
                        ui.label("tall");
                    });
                    ui.label(RichText::new("Leave a box empty (0) for automatic.").size(11.0).color(theme::text_dim()));
                    d.ps.scaling = if fit { Scaling::Fit { wide, tall } } else { Scaling::Percent(pct) };
                    ui.add_space(8.0);
                    egui::Grid::new("ps_page").num_columns(2).spacing(Vec2::new(10.0, 8.0)).show(ui, |ui| {
                        ui.label("Paper size:");
                        egui::ComboBox::from_id_salt("ps_paper").width(260.0).selected_text(format!("{}  {}", d.ps.paper.name(), d.ps.paper.caption(metric))).show_ui(ui, |ui| {
                            for p in Paper::ALL {
                                ui.selectable_value(&mut d.ps.paper, p, format!("{}  {}", p.name(), p.caption(metric)));
                            }
                        });
                        ui.end_row();
                        ui.label("First page number:");
                        ui.add(egui::TextEdit::singleline(&mut d.first).desired_width(80.0));
                        ui.end_row();
                    });
                }
                1 => {
                    let k = if metric { 2.54 } else { 1.0 };
                    let suffix = if metric { " cm" } else { "\"" };
                    let field = |ui: &mut egui::Ui, label: &str, v: &mut f32| {
                        ui.vertical(|ui| {
                            ui.label(label);
                            let mut x = *v * k;
                            if ui.add(egui::DragValue::new(&mut x).range(0.0..=20.0).speed(0.01).max_decimals(2).suffix(suffix)).changed() {
                                *v = x / k;
                            }
                        });
                    };
                    ui.horizontal(|ui| {
                        ui.add_space(120.0);
                        field(ui, "Top:", &mut d.ps.margins.top);
                        ui.add_space(30.0);
                        field(ui, "Header:", &mut d.ps.margins.header);
                    });
                    ui.horizontal(|ui| {
                        field(ui, "Left:", &mut d.ps.margins.left);
                        let (rect, _) = ui.allocate_exact_size(Vec2::new(150.0, 120.0), Sense::hover());
                        let pr = Rect::from_center_size(rect.center(), Vec2::new(80.0, 104.0));
                        ui.painter().rect_filled(pr, 0.0, Color32::WHITE);
                        ui.painter().rect_stroke(pr, 0.0, Stroke::new(1.0, theme::border()), StrokeKind::Inside);
                        ui.painter().rect_stroke(pr.shrink(12.0), 0.0, Stroke::new(1.0, Color32::from_gray(160)), StrokeKind::Inside);
                        field(ui, "Right:", &mut d.ps.margins.right);
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(120.0);
                        field(ui, "Bottom:", &mut d.ps.margins.bottom);
                        ui.add_space(30.0);
                        field(ui, "Footer:", &mut d.ps.margins.footer);
                    });
                    ui.add_space(8.0);
                    ui.label(RichText::new("Center on page").strong());
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut d.ps.center_h, "Horizontally");
                        ui.checkbox(&mut d.ps.center_v, "Vertically");
                    });
                }
                2 => {
                    let presets = hf_presets();
                    for (label, header) in [("Header:", true), ("Footer:", false)] {
                        let hf = if header { &mut d.ps.header } else { &mut d.ps.footer };
                        let sample = crate::page::HfContext { page: 1, pages: 1, sheet: "Sheet1", file: "Book1", path: "" };
                        // preview box
                        let (rect, _) = ui.allocate_exact_size(Vec2::new(500.0, 34.0), Sense::hover());
                        ui.painter().rect_filled(rect, 0.0, Color32::WHITE);
                        ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, theme::border()), StrokeKind::Inside);
                        let r = |s: &str| crate::page::render_hf(s, &sample);
                        ui.painter().text(rect.left_center() + Vec2::new(8.0, 0.0), Align2::LEFT_CENTER, r(&hf.left), theme::ui_font(11.5), theme::text());
                        ui.painter().text(rect.center(), Align2::CENTER_CENTER, r(&hf.center), theme::ui_font(11.5), theme::text());
                        ui.painter().text(rect.right_center() - Vec2::new(8.0, 0.0), Align2::RIGHT_CENTER, r(&hf.right), theme::ui_font(11.5), theme::text());
                        ui.horizontal(|ui| {
                            ui.label(label);
                            let cur = presets.iter().find(|(_, p)| p == hf).map(|(n, _)| n.to_string()).unwrap_or_else(|| "(custom)".into());
                            egui::ComboBox::from_id_salt(("hf", header)).width(240.0).selected_text(cur).show_ui(ui, |ui| {
                                for (n, p) in &presets {
                                    if ui.selectable_label(p == hf, *n).clicked() {
                                        *hf = p.clone();
                                    }
                                }
                            });
                        });
                        ui.horizontal(|ui| {
                            for (sec, text) in [("Left", &mut hf.left), ("Center", &mut hf.center), ("Right", &mut hf.right)] {
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(format!("{sec} section")).size(11.0).color(theme::text_dim()));
                                    ui.add(egui::TextEdit::singleline(text).desired_width(158.0));
                                });
                            }
                        });
                        ui.add_space(10.0);
                    }
                    ui.label(
                        RichText::new("Codes: &P page number, &N number of pages, &D date, &T time, &A sheet name, &F file name, &Z file path, && ampersand")
                            .size(11.0)
                            .color(theme::text_dim()),
                    );
                    let _ = HeaderFooter::default();
                }
                _ => {
                    egui::Grid::new("ps_sheet").num_columns(2).spacing(Vec2::new(10.0, 8.0)).show(ui, |ui| {
                        ui.label("Print area:");
                        ui.add(egui::TextEdit::singleline(&mut d.area).desired_width(260.0).hint_text("e.g. $A$1:$F$40"));
                        ui.end_row();
                    });
                    ui.add_space(6.0);
                    ui.label(RichText::new("Print titles").strong());
                    egui::Grid::new("ps_titles").num_columns(2).spacing(Vec2::new(10.0, 8.0)).show(ui, |ui| {
                        ui.label("Rows to repeat at top:");
                        ui.add(egui::TextEdit::singleline(&mut d.rows).desired_width(160.0).hint_text("$1:$1"));
                        ui.end_row();
                        ui.label("Columns to repeat at left:");
                        ui.add(egui::TextEdit::singleline(&mut d.cols).desired_width(160.0).hint_text("$A:$A"));
                        ui.end_row();
                    });
                    ui.add_space(6.0);
                    ui.label(RichText::new("Print").strong());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.checkbox(&mut d.ps.gridlines, "Gridlines");
                            ui.checkbox(&mut d.ps.black_white, "Black and white");
                        });
                        ui.vertical(|ui| {
                            ui.checkbox(&mut d.ps.headings, "Row and column headings");
                        });
                    });
                    ui.add_space(6.0);
                    ui.label(RichText::new("Page order").strong());
                    ui.radio_value(&mut d.ps.over_then_down, false, "Down, then over");
                    ui.radio_value(&mut d.ps.over_then_down, true, "Over, then down");
                }
            }
            if let Some(e) = &d.error {
                ui.colored_label(Color32::from_rgb(0xC0, 0, 0), e);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Print...").clicked() {
                    go_print = true;
                }
                if ui.button("Print Preview").clicked() {
                    go_print = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let cancel = ui.add(egui::Button::new("Cancel").min_size(Vec2::new(76.0, 24.0))).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
                    let ok = ui.add(egui::Button::new("OK").min_size(Vec2::new(76.0, 24.0)).fill(theme::accent_light())).clicked();
                    if cancel {
                        keep = false;
                    } else if ok || go_print {
                        match d.validate() {
                            Ok(ps) => {
                                self.set_page(|p| *p = ps);
                                keep = false;
                            }
                            Err(e) => {
                                d.error = Some(e);
                                go_print = false;
                            }
                        }
                    }
                });
            });
        });
        if go_print && !keep {
            self.backstage = Some(super::chrome::Backstage::Print);
        }
        keep
    }
}

pub struct PageSetupDlg {
    pub tab: usize,
    pub ps: PageSetup,
    pub area: String,
    pub rows: String,
    pub cols: String,
    pub first: String,
    pub error: Option<String>,
}

impl PageSetupDlg {
    /// Applies the text fields; returns the finished setup or a message.
    fn validate(&self) -> Result<PageSetup, String> {
        let mut ps = self.ps.clone();
        let clean = |s: &str| s.trim().trim_start_matches('=').rsplit('!').next().unwrap_or("").to_string();
        let a = clean(&self.area);
        ps.print_area = if a.is_empty() { None } else { Some(Range::parse(&a).ok_or("The print area reference isn't valid.")?) };
        let r = clean(&self.rows);
        ps.title_rows = if r.is_empty() {
            None
        } else {
            let rg = Range::parse(&r).filter(|g| g.is_full_rows()).ok_or("Rows to repeat must be whole rows, like $1:$2.")?;
            Some((rg.r1, rg.r2))
        };
        let c = clean(&self.cols);
        ps.title_cols = if c.is_empty() {
            None
        } else {
            let rg = Range::parse(&c).filter(|g| g.is_full_cols()).ok_or("Columns to repeat must be whole columns, like $A:$B.")?;
            Some((rg.c1, rg.c2))
        };
        let f = self.first.trim();
        ps.first_page = if f.is_empty() || f.eq_ignore_ascii_case("auto") {
            None
        } else {
            Some(f.parse::<u32>().map_err(|_| "First page number must be a number or Auto.")?)
        };
        if ps.margins.header > ps.margins.top || ps.margins.footer > ps.margins.bottom {
            // the familiar warning; still allowed
        }
        Ok(ps)
    }
}
