//! Conditional Formatting UI: the Home ribbon menu, quick rule dialogs, the rule editor,
//! the rules manager, and the grid painting helpers.

use std::collections::HashMap;
use std::rc::Rc;

use eframe::egui::{self, text::LayoutJob, Align, Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextFormat, Vec2};

use super::dialogs::Dialog;
use super::ribbon::color_grid;
use super::theme::{self, rgb};
use super::{App, Scope};
use crate::condfmt::{
    compute_stats, operand_from_input, operand_to_input, parse_sqref, scale_color, sqref, subtract_all, CfFrame, CfOverride,
    CmpOp, CondFormat, DataBarDraw, DiffFormat, RuleKind, RuleStats, TextOp,
};
use crate::model::{Range, Style};

/// Data bar colours offered in the menu.
const BAR_COLORS: [(&str, u32); 6] = [
    ("Blue", 0x638EC6),
    ("Green", 0x63C384),
    ("Red", 0xFF555A),
    ("Orange", 0xFFB628),
    ("Light Blue", 0x008AEF),
    ("Purple", 0xD6007B),
];

/// Colour scale presets, colours listed from the highest value to the lowest (as named).
const SCALES: [(&str, &[u32]); 12] = [
    ("Green - Yellow - Red", &[0x63BE7B, 0xFFEB84, 0xF8696B]),
    ("Red - Yellow - Green", &[0xF8696B, 0xFFEB84, 0x63BE7B]),
    ("Green - White - Red", &[0x63BE7B, 0xFCFCFF, 0xF8696B]),
    ("Red - White - Green", &[0xF8696B, 0xFCFCFF, 0x63BE7B]),
    ("Blue - White - Red", &[0x5A8AC6, 0xFCFCFF, 0xF8696B]),
    ("Red - White - Blue", &[0xF8696B, 0xFCFCFF, 0x5A8AC6]),
    ("White - Red", &[0xFCFCFF, 0xF8696B]),
    ("Red - White", &[0xF8696B, 0xFCFCFF]),
    ("Green - White", &[0x63BE7B, 0xFCFCFF]),
    ("White - Green", &[0xFCFCFF, 0x63BE7B]),
    ("Green - Yellow", &[0x63BE7B, 0xFFEF9C]),
    ("Yellow - Green", &[0xFFEF9C, 0x63BE7B]),
];

const RULE_TYPES: [&str; 6] = [
    "Format all cells based on their values",
    "Format only cells that contain",
    "Format only top or bottom ranked values",
    "Format only values that are above or below average",
    "Format only unique or duplicate values",
    "Use a formula to determine which cells to format",
];

const CONTAINS: [&str; 6] = ["Cell Value", "Specific Text", "Blanks", "No Blanks", "Errors", "No Errors"];
const AVERAGES: [&str; 4] = ["above", "below", "equal or above", "equal or below"];

/// Statistics cache: per (sheet, rule index) the rule they were computed for.
#[derive(Default)]
pub struct CfState {
    cache: HashMap<(usize, usize), (CondFormat, Rc<RuleStats>)>,
}

impl CfState {
    /// Drops cached statistics; called after every workbook change.
    pub fn invalidate(&mut self) {
        self.cache.clear();
    }
}

// ---------------------------------------------------------------------------
// Dialog state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quick {
    Greater,
    Less,
    Between,
    Equal,
    Text,
    Duplicate,
    Top { bottom: bool, percent: bool },
    Average { below: bool },
}

impl Quick {
    fn title(self) -> &'static str {
        match self {
            Quick::Greater => "Greater Than",
            Quick::Less => "Less Than",
            Quick::Between => "Between",
            Quick::Equal => "Equal To",
            Quick::Text => "Text That Contains",
            Quick::Duplicate => "Duplicate Values",
            Quick::Top { bottom: false, percent: false } => "Top 10 Items",
            Quick::Top { bottom: false, percent: true } => "Top 10%",
            Quick::Top { bottom: true, percent: false } => "Bottom 10 Items",
            Quick::Top { bottom: true, percent: true } => "Bottom 10%",
            Quick::Average { below: false } => "Above Average",
            Quick::Average { below: true } => "Below Average",
        }
    }

    fn prompt(self) -> &'static str {
        match self {
            Quick::Greater => "Format cells that are GREATER THAN:",
            Quick::Less => "Format cells that are LESS THAN:",
            Quick::Between => "Format cells that are BETWEEN:",
            Quick::Equal => "Format cells that are EQUAL TO:",
            Quick::Text => "Format cells that contain the text:",
            Quick::Duplicate => "Format cells that contain:",
            Quick::Top { bottom: false, .. } => "Format cells that rank in the TOP:",
            Quick::Top { bottom: true, .. } => "Format cells that rank in the BOTTOM:",
            Quick::Average { below: false } => "Format cells that are ABOVE AVERAGE:",
            Quick::Average { below: true } => "Format cells that are BELOW AVERAGE:",
        }
    }
}

pub struct QuickRule {
    kind: Quick,
    ranges: Vec<Range>,
    v1: String,
    v2: String,
    rank: u32,
    unique: bool,
    /// Index into the presets; `presets().len()` means the custom format.
    preset: usize,
    custom: DiffFormat,
    fmt: Option<DiffFormat>,
    focus: bool,
}

pub struct RuleEditor {
    title: &'static str,
    ranges: Vec<Range>,
    ty: usize,
    /// 0 = 2-colour scale, 1 = 3-colour scale, 2 = data bar.
    style: usize,
    /// Scale colours: minimum, midpoint, maximum.
    colors: [u32; 3],
    bar_color: u32,
    bar_gradient: bool,
    contains: usize,
    op: CmpOp,
    v1: String,
    v2: String,
    text_op: TextOp,
    text: String,
    bottom: bool,
    rank: u32,
    percent: bool,
    avg: usize,
    unique: bool,
    formula: String,
    format: DiffFormat,
    stop: bool,
    fmt: Option<DiffFormat>,
    error: Option<String>,
}

pub struct Manager {
    rules: Vec<CondFormat>,
    applies: Vec<String>,
    whole_sheet: bool,
    selected: Option<usize>,
    /// Rule being edited: (index in `rules`, None for a new rule).
    editor: Option<(Option<usize>, RuleEditor)>,
    sel: Range,
    error: Option<String>,
}

pub enum CfDialog {
    Quick(QuickRule),
    Rule(RuleEditor),
    Manage(Manager),
}

impl RuleEditor {
    fn new(ranges: Vec<Range>, ty: usize) -> Self {
        RuleEditor {
            title: "New Formatting Rule",
            ranges,
            ty,
            style: 0,
            colors: [0xF8696B, 0xFFEB84, 0x63BE7B],
            bar_color: 0x638EC6,
            bar_gradient: true,
            contains: 0,
            op: CmpOp::Between,
            v1: String::new(),
            v2: String::new(),
            text_op: TextOp::Contains,
            text: String::new(),
            bottom: false,
            rank: 10,
            percent: false,
            avg: 0,
            unique: false,
            formula: String::new(),
            format: DiffFormat::default(),
            stop: false,
            fmt: None,
            error: None,
        }
    }

    fn from_rule(rule: &CondFormat) -> Self {
        let mut e = RuleEditor::new(rule.ranges.clone(), 1);
        e.title = "Edit Formatting Rule";
        e.format = rule.format.clone();
        e.stop = rule.stop;
        match &rule.kind {
            RuleKind::CellValue { op, f1, f2 } => {
                e.op = *op;
                e.v1 = operand_to_input(f1, "");
                e.v2 = operand_to_input(f2, "");
            }
            RuleKind::Text { op, text } => {
                e.contains = 1;
                e.text_op = *op;
                e.text = text.clone();
            }
            RuleKind::Blanks { not } => e.contains = if *not { 3 } else { 2 },
            RuleKind::Errors { not } => e.contains = if *not { 5 } else { 4 },
            RuleKind::Top { bottom, percent, rank } => {
                e.ty = 2;
                (e.bottom, e.percent, e.rank) = (*bottom, *percent, *rank);
            }
            RuleKind::Average { below, equal } => {
                e.ty = 3;
                e.avg = match (below, equal) {
                    (false, false) => 0,
                    (true, false) => 1,
                    (false, true) => 2,
                    (true, true) => 3,
                };
            }
            RuleKind::Duplicate { unique } => {
                e.ty = 4;
                e.unique = *unique;
            }
            RuleKind::Expression { formula } => {
                e.ty = 5;
                e.formula = format!("={formula}");
            }
            RuleKind::DataBar { color, gradient } => {
                e.ty = 0;
                e.style = 2;
                e.bar_color = *color;
                e.bar_gradient = *gradient;
            }
            RuleKind::ColorScale { colors } => {
                e.ty = 0;
                match colors.as_slice() {
                    [a, b] => {
                        e.style = 0;
                        e.colors = [*a, 0xFFEB84, *b];
                    }
                    [a, m, b, ..] => {
                        e.style = 1;
                        e.colors = [*a, *m, *b];
                    }
                    _ => {}
                }
            }
        }
        e
    }

    fn build(&self) -> Result<CondFormat, String> {
        let kind = match self.ty {
            0 => match self.style {
                0 => RuleKind::ColorScale { colors: vec![self.colors[0], self.colors[2]] },
                1 => RuleKind::ColorScale { colors: self.colors.to_vec() },
                _ => RuleKind::DataBar { color: self.bar_color, gradient: self.bar_gradient },
            },
            1 => match self.contains {
                0 => {
                    let f1 = operand_from_input(&self.v1);
                    let f2 = operand_from_input(&self.v2);
                    if f1.is_empty() || (self.op.two() && f2.is_empty()) {
                        return Err("Enter a value for the rule.".into());
                    }
                    check_formula(&f1)?;
                    if self.op.two() {
                        check_formula(&f2)?;
                    }
                    RuleKind::CellValue { op: self.op, f1, f2: if self.op.two() { f2 } else { String::new() } }
                }
                1 => RuleKind::Text { op: self.text_op, text: self.text.clone() },
                2 => RuleKind::Blanks { not: false },
                3 => RuleKind::Blanks { not: true },
                4 => RuleKind::Errors { not: false },
                _ => RuleKind::Errors { not: true },
            },
            2 => RuleKind::Top { bottom: self.bottom, percent: self.percent, rank: self.rank.clamp(1, if self.percent { 100 } else { 1000 }) },
            3 => RuleKind::Average { below: self.avg % 2 == 1, equal: self.avg >= 2 },
            4 => RuleKind::Duplicate { unique: self.unique },
            _ => {
                let f = self.formula.trim().trim_start_matches('=').trim().to_string();
                if f.is_empty() {
                    return Err("Enter a formula.".into());
                }
                check_formula(&f)?;
                RuleKind::Expression { formula: f }
            }
        };
        let mut rule = CondFormat::new(self.ranges.clone(), kind, self.format.clone());
        rule.stop = self.stop;
        Ok(rule)
    }
}

fn check_formula(f: &str) -> Result<(), String> {
    crate::formula::parse(f).map(|_| ()).map_err(|e| format!("There's a problem with this formula.\n\n{e}"))
}

// ---------------------------------------------------------------------------
// Small widgets
// ---------------------------------------------------------------------------

fn window(title: &str) -> egui::Window<'static> {
    egui::Window::new(title.to_string()).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, -40.0])
}

/// Right-aligned dialog buttons in reading order; the first is the default. Returns the clicked index.
fn buttons(ui: &mut egui::Ui, labels: &[&str]) -> Option<usize> {
    let mut out = None;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            for (i, l) in labels.iter().enumerate().rev() {
                let mut b = egui::Button::new(*l).min_size(Vec2::new(76.0, 24.0));
                if i == 0 {
                    b = b.fill(theme::ACCENT_LIGHT);
                }
                if ui.add(b).clicked() {
                    out = Some(i);
                }
            }
        });
    });
    out
}

fn enter_or_escape(ui: &egui::Ui) -> (bool, bool) {
    ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)))
}

/// Paints the sample text with a differential format.
fn format_preview(ui: &mut egui::Ui, f: &DiffFormat, size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 0.0, f.fill.map(rgb).unwrap_or(Color32::WHITE));
    p.rect_stroke(rect, 0.0, Stroke::new(1.0, f.border.map(rgb).unwrap_or(theme::BORDER)), StrokeKind::Inside);
    let color = f.color.map(rgb).unwrap_or(Color32::BLACK);
    let line = |on: bool| if on { Stroke::new(1.0, color) } else { Stroke::NONE };
    let text = if f.is_empty() { "No Format Set" } else { "AaBbCcYyZz" };
    let font = if f.bold { theme::ui_bold(12.0) } else { theme::ui_font(12.0) };
    let job = LayoutJob::single_section(
        text.into(),
        TextFormat { font_id: font, color, italics: f.italic, underline: line(f.underline), strikethrough: line(f.strike), ..Default::default() },
    );
    let g = p.layout_job(job);
    p.galley(rect.center() - g.size() / 2.0, g, color);
}

/// Paints a horizontal colour ramp for a scale.
fn scale_preview(p: &egui::Painter, rect: Rect, colors: &[u32]) {
    let st = RuleStats { count: 2, min: 0.0, max: 1.0, mid: 0.5, ..Default::default() };
    let n = 24;
    let w = rect.width() / n as f32;
    for i in 0..n {
        let t = (i as f64 + 0.5) / n as f64;
        let r = Rect::from_min_size(Pos2::new(rect.left() + i as f32 * w, rect.top()), Vec2::new(w + 0.5, rect.height()));
        p.rect_filled(r, 0.0, rgb(scale_color(colors, &st, t)));
    }
}

fn mix(a: u32, b: u32, t: f64) -> u32 {
    let st = RuleStats { count: 2, min: 0.0, max: 1.0, ..Default::default() };
    scale_color(&[a, b], &st, t)
}

/// Draws a data bar inside a cell rectangle.
pub fn paint_data_bar(p: &egui::Painter, cell: Rect, bar: &DataBarDraw) {
    let inner = cell.shrink2(Vec2::new(2.0, 2.0));
    if inner.width() <= 0.0 || inner.height() <= 0.0 || bar.end <= bar.start {
        return;
    }
    let r = Rect::from_min_max(
        Pos2::new(inner.left() + inner.width() * bar.start, inner.top()),
        Pos2::new(inner.left() + inner.width() * bar.end, inner.bottom()),
    );
    let c = rgb(bar.color);
    if bar.gradient {
        let fade = rgb(mix(bar.color, 0xFFFFFF, 0.9));
        let (c1, c2) = if bar.negative { (fade, c) } else { (c, fade) };
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(r.left_top(), c1);
        mesh.colored_vertex(r.right_top(), c2);
        mesh.colored_vertex(r.right_bottom(), c2);
        mesh.colored_vertex(r.left_bottom(), c1);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        p.add(egui::Shape::mesh(mesh));
        p.rect_stroke(r, 0.0, Stroke::new(1.0, c), StrokeKind::Inside);
    } else {
        p.rect_filled(r, 0.0, c);
    }
}

fn bar_preview(p: &egui::Painter, rect: Rect, color: u32, gradient: bool) {
    let bars = [0.45, 0.8, 0.3, 1.0];
    let h = rect.height() / bars.len() as f32;
    for (i, f) in bars.iter().enumerate() {
        let r = Rect::from_min_size(Pos2::new(rect.left(), rect.top() + i as f32 * h), Vec2::new(rect.width(), h));
        paint_data_bar(p, r, &DataBarDraw { start: 0.0, end: *f, color, gradient, negative: false });
    }
}

/// Applies conditional overrides to a cell's style before painting.
pub fn apply_override(style: &mut Style, o: &CfOverride) {
    if o.fill.is_some() {
        style.fill = o.fill;
    }
    if o.color.is_some() {
        style.color = o.color;
    }
    style.bold |= o.bold;
    style.italic |= o.italic;
    style.underline |= o.underline;
    style.strike |= o.strike;
}

fn color_button(ui: &mut egui::Ui, label: &str, c: &mut u32) {
    ui.horizontal(|ui| {
        if !label.is_empty() {
            ui.label(label);
        }
        let (r, _) = ui.allocate_exact_size(Vec2::new(36.0, 16.0), Sense::hover());
        ui.painter().rect_filled(r, 2.0, rgb(*c));
        ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0, theme::BORDER), StrokeKind::Inside);
        ui.menu_button("▾", |ui| {
            if let Some(Some(n)) = color_grid(ui, false) {
                *c = n;
                ui.close();
            }
        });
    });
}

fn opt_color_button(ui: &mut egui::Ui, id: &str, c: &mut Option<u32>) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::new(36.0, 16.0), Sense::hover());
        match c {
            Some(v) => {
                ui.painter().rect_filled(r, 2.0, rgb(*v));
            }
            None => {
                ui.painter().line_segment([r.left_bottom(), r.right_top()], Stroke::new(1.0, Color32::from_gray(150)));
            }
        }
        ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0, theme::BORDER), StrokeKind::Inside);
        ui.push_id(id, |ui| {
            ui.menu_button("▾", |ui| {
                if let Some(n) = color_grid(ui, true) {
                    *c = n;
                    ui.close();
                }
            });
        });
    });
}

/// The compact Format Cells window for a differential format. Returns Some(result) when closed.
fn format_window(ctx: &egui::Context, f: &mut DiffFormat) -> Option<bool> {
    let mut out = None;
    egui::Window::new("Format Cells")
        .id(egui::Id::new("cf_format_cells"))
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_CENTER, [30.0, -10.0])
        .show(ctx, |ui| {
            ui.set_min_width(300.0);
            ui.label(RichText::new("Font").strong());
            ui.horizontal(|ui| {
                ui.checkbox(&mut f.bold, "Bold");
                ui.checkbox(&mut f.italic, "Italic");
                ui.checkbox(&mut f.underline, "Underline");
                ui.checkbox(&mut f.strike, "Strikethrough");
            });
            ui.horizontal(|ui| {
                ui.label("Color:");
                opt_color_button(ui, "cf_font_color", &mut f.color);
            });
            ui.add_space(4.0);
            ui.label(RichText::new("Border").strong());
            ui.horizontal(|ui| {
                let mut on = f.border.is_some();
                if ui.checkbox(&mut on, "Outline").changed() {
                    f.border = if on { Some(0x000000) } else { None };
                }
                if f.border.is_some() {
                    ui.label("Color:");
                    opt_color_button(ui, "cf_border_color", &mut f.border);
                }
            });
            ui.add_space(4.0);
            ui.label(RichText::new("Fill").strong());
            ui.horizontal(|ui| {
                ui.label("Background Color:");
                opt_color_button(ui, "cf_fill_color", &mut f.fill);
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Preview:");
                format_preview(ui, f, Vec2::new(200.0, 30.0));
            });
            ui.horizontal(|ui| {
                if ui.button("Clear").clicked() {
                    *f = DiffFormat::default();
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(egui::Button::new("Cancel").min_size(Vec2::new(76.0, 24.0))).clicked() {
                        out = Some(false);
                    }
                    if ui.add(egui::Button::new("OK").min_size(Vec2::new(76.0, 24.0)).fill(theme::ACCENT_LIGHT)).clicked() {
                        out = Some(true);
                    }
                });
            });
            let (enter, esc) = enter_or_escape(ui);
            if enter {
                out = Some(true);
            } else if esc {
                out = Some(false);
            }
        });
    out
}

// ---------------------------------------------------------------------------
// App integration
// ---------------------------------------------------------------------------

impl App {
    /// Rules of a sheet prepared for painting; statistics are cached until the next change.
    pub fn cf_frame(&mut self, si: usize) -> CfFrame {
        let Some(sheet) = self.wb.sheets.get(si) else { return CfFrame::default() };
        if sheet.cond_formats.is_empty() {
            return CfFrame::default();
        }
        let cache = &mut self.cf.cache;
        CfFrame::new(sheet, |i, rule| {
            if let Some((r, st)) = cache.get(&(si, i)) {
                if r == rule {
                    return st.clone();
                }
            }
            let st = Rc::new(compute_stats(sheet, rule));
            cache.insert((si, i), (rule.clone(), st.clone()));
            st
        })
    }

    fn cf_selection(&self) -> Vec<Range> {
        vec![self.sel()]
    }

    /// Numbers in the selection (clipped to the used area), for dialog defaults.
    fn cf_sel_numbers(&self) -> Vec<f64> {
        let sel = self.sel_clipped();
        self.sheet().iter_range(sel).filter_map(|(_, _, c)| c.value.as_number()).take(100_000).collect()
    }

    fn cf_add_rule(&mut self, rule: CondFormat) {
        self.commit_edit(false);
        let si = self.wb.active;
        self.push_undo(&[Scope::Sheet(si)]);
        // New rules take the highest priority, like the familiar behaviour.
        self.wb.sheets[si].cond_formats.insert(0, rule);
        self.changed();
    }

    fn cf_set_rules(&mut self, rules: Vec<CondFormat>) {
        let si = self.wb.active;
        if self.wb.sheets[si].cond_formats == rules {
            return;
        }
        self.push_undo(&[Scope::Sheet(si)]);
        self.wb.sheets[si].cond_formats = rules;
        self.changed();
    }

    pub fn cf_clear(&mut self, whole_sheet: bool) {
        let si = self.wb.active;
        let rules = if whole_sheet {
            Vec::new()
        } else {
            let sel = self.sel();
            let mut v = self.wb.sheets[si].cond_formats.clone();
            for r in v.iter_mut() {
                r.ranges = subtract_all(&r.ranges, sel);
            }
            v.retain(|r| !r.ranges.is_empty());
            v
        };
        self.cf_set_rules(rules);
    }

    pub(super) fn cf_open_quick(&mut self, kind: Quick) {
        self.commit_edit(false);
        let nums = self.cf_sel_numbers();
        let fmt = |n: f64| crate::numfmt::general(n);
        let (mut v1, mut v2) = (String::new(), String::new());
        if !nums.is_empty() {
            let mut s = nums.clone();
            s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            match kind {
                Quick::Greater | Quick::Less => v1 = fmt(crate::condfmt::percentile(&s, 0.5)),
                Quick::Between => {
                    v1 = fmt(crate::condfmt::percentile(&s, 0.25));
                    v2 = fmt(crate::condfmt::percentile(&s, 0.75));
                }
                _ => {}
            }
        }
        if matches!(kind, Quick::Equal | Quick::Text) {
            let (r, c) = self.active();
            v1 = crate::numfmt::value_to_text(self.sheet().value(r, c));
        }
        let rank = if matches!(kind, Quick::Top { .. }) { 10 } else { 0 };
        self.dialog = Some(Dialog::CondFmt(Box::new(CfDialog::Quick(QuickRule {
            kind,
            ranges: self.cf_selection(),
            v1,
            v2,
            rank,
            unique: false,
            preset: 0,
            custom: DiffFormat::default(),
            fmt: None,
            focus: true,
        }))));
    }

    pub(super) fn cf_open_editor(&mut self, ty: usize) {
        self.commit_edit(false);
        self.dialog = Some(Dialog::CondFmt(Box::new(CfDialog::Rule(RuleEditor::new(self.cf_selection(), ty)))));
    }

    pub fn cf_open_manager(&mut self) {
        self.commit_edit(false);
        let rules = self.sheet().cond_formats.clone();
        let applies = rules.iter().map(|r| r.applies_to()).collect();
        self.dialog = Some(Dialog::CondFmt(Box::new(CfDialog::Manage(Manager {
            rules,
            applies,
            whole_sheet: false,
            selected: None,
            editor: None,
            sel: self.sel(),
            error: None,
        }))));
    }

    fn cf_visual(&mut self, kind: RuleKind) {
        let rule = CondFormat::new(self.cf_selection(), kind, DiffFormat::default());
        self.cf_add_rule(rule);
    }

    /// The big "Conditional Formatting" button in the Home > Styles group.
    pub fn cf_ribbon_button(&mut self, ui: &mut egui::Ui) {
        let resp = ribbon_button(ui, Vec2::new(86.0, 66.0), "Conditional\nFormatting");
        egui::Popup::menu(&resp).show(|ui| self.cf_menu(ui));
        let icon = Rect::from_center_size(Pos2::new(resp.rect.center().x, resp.rect.top() + 17.0), Vec2::new(20.0, 18.0));
        paint_cf_icon(ui.painter(), icon);
        resp.on_hover_text("Conditional Formatting\nHighlight interesting cells, emphasize unusual values and visualize data using data bars and color scales.");
    }

    fn cf_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(220.0);
        ui.menu_button("Highlight Cells Rules", |ui| {
            ui.set_min_width(200.0);
            for (label, k) in [
                ("Greater Than...", Quick::Greater),
                ("Less Than...", Quick::Less),
                ("Between...", Quick::Between),
                ("Equal To...", Quick::Equal),
                ("Text that Contains...", Quick::Text),
                ("Duplicate Values...", Quick::Duplicate),
            ] {
                if ui.button(label).clicked() {
                    self.cf_open_quick(k);
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("More Rules...").clicked() {
                self.cf_open_editor(1);
                ui.close();
            }
        });
        ui.menu_button("Top/Bottom Rules", |ui| {
            ui.set_min_width(200.0);
            for (label, k) in [
                ("Top 10 Items...", Quick::Top { bottom: false, percent: false }),
                ("Top 10%...", Quick::Top { bottom: false, percent: true }),
                ("Bottom 10 Items...", Quick::Top { bottom: true, percent: false }),
                ("Bottom 10%...", Quick::Top { bottom: true, percent: true }),
                ("Above Average...", Quick::Average { below: false }),
                ("Below Average...", Quick::Average { below: true }),
            ] {
                if ui.button(label).clicked() {
                    self.cf_open_quick(k);
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("More Rules...").clicked() {
                self.cf_open_editor(2);
                ui.close();
            }
        });
        ui.menu_button("Data Bars", |ui| {
            for (title, gradient) in [("Gradient Fill", true), ("Solid Fill", false)] {
                ui.label(RichText::new(title).size(11.0).color(theme::TEXT_DIM));
                ui.horizontal(|ui| {
                    for (name, color) in BAR_COLORS {
                        let (r, resp) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(r, 2.0, theme::HOVER);
                        }
                        bar_preview(ui.painter(), r.shrink(4.0), color, gradient);
                        let tip = format!("{name} Data Bar ({})", if gradient { "gradient" } else { "solid" });
                        if resp.on_hover_text(tip).clicked() {
                            self.cf_visual(RuleKind::DataBar { color, gradient });
                            ui.close();
                        }
                    }
                });
            }
            ui.separator();
            if ui.button("More Rules...").clicked() {
                let mut e = RuleEditor::new(self.cf_selection(), 0);
                e.style = 2;
                self.dialog = Some(Dialog::CondFmt(Box::new(CfDialog::Rule(e))));
                ui.close();
            }
        });
        ui.menu_button("Color Scales", |ui| {
            for row in SCALES.chunks(4) {
                ui.horizontal(|ui| {
                    for (name, cols) in row {
                        let (r, resp) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(r, 2.0, theme::HOVER);
                        }
                        let inner = r.shrink(5.0);
                        let h = inner.height() / 3.0;
                        // Three bands from top (highest) to bottom (lowest), like a mini column.
                        let low_to_high: Vec<u32> = cols.iter().rev().copied().collect();
                        let st = RuleStats { count: 3, min: 0.0, max: 2.0, mid: 1.0, ..Default::default() };
                        for i in 0..3 {
                            let band = Rect::from_min_size(Pos2::new(inner.left(), inner.top() + i as f32 * h), Vec2::new(inner.width(), h));
                            ui.painter().rect_filled(band, 0.0, rgb(scale_color(&low_to_high, &st, (2 - i) as f64)));
                        }
                        ui.painter().rect_stroke(inner, 0.0, Stroke::new(1.0, Color32::from_gray(200)), StrokeKind::Outside);
                        if resp.on_hover_text(format!("{name} Color Scale")).clicked() {
                            self.cf_visual(RuleKind::ColorScale { colors: low_to_high });
                            ui.close();
                        }
                    }
                });
            }
            ui.separator();
            if ui.button("More Rules...").clicked() {
                let mut e = RuleEditor::new(self.cf_selection(), 0);
                e.style = 1;
                self.dialog = Some(Dialog::CondFmt(Box::new(CfDialog::Rule(e))));
                ui.close();
            }
        });
        ui.separator();
        if ui.button("New Rule...").clicked() {
            self.cf_open_editor(1);
            ui.close();
        }
        ui.menu_button("Clear Rules", |ui| {
            ui.set_min_width(220.0);
            if ui.button("Clear Rules from Selected Cells").clicked() {
                self.cf_clear(false);
                ui.close();
            }
            if ui.button("Clear Rules from Entire Sheet").clicked() {
                self.cf_clear(true);
                ui.close();
            }
        });
        if ui.button("Manage Rules...").clicked() {
            self.cf_open_manager();
            ui.close();
        }
    }

    /// Shows a conditional formatting dialog; returns false when it closed.
    pub fn cf_dialog_ui(&mut self, ctx: &egui::Context, d: &mut CfDialog) -> bool {
        match d {
            CfDialog::Quick(q) => self.cf_quick_ui(ctx, q),
            CfDialog::Rule(e) => match rule_editor_ui(ctx, e) {
                Some(true) => match e.build() {
                    Ok(rule) => {
                        self.cf_add_rule(rule);
                        false
                    }
                    Err(msg) => {
                        e.error = Some(msg);
                        true
                    }
                },
                Some(false) => false,
                None => true,
            },
            CfDialog::Manage(m) => self.cf_manager_ui(ctx, m),
        }
    }

    fn cf_quick_ui(&mut self, ctx: &egui::Context, q: &mut QuickRule) -> bool {
        let presets = DiffFormat::presets();
        let mut result = None;
        let fmt_open = q.fmt.is_some();
        window(q.kind.title()).show(ctx, |ui| {
            ui.set_min_width(420.0);
            ui.label(q.kind.prompt());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                match q.kind {
                    Quick::Greater | Quick::Less | Quick::Equal | Quick::Text => {
                        let r = ui.add(egui::TextEdit::singleline(&mut q.v1).desired_width(180.0));
                        if q.focus {
                            r.request_focus();
                            q.focus = false;
                        }
                    }
                    Quick::Between => {
                        ui.add(egui::TextEdit::singleline(&mut q.v1).desired_width(110.0));
                        ui.label("and");
                        ui.add(egui::TextEdit::singleline(&mut q.v2).desired_width(110.0));
                    }
                    Quick::Duplicate => {
                        egui::ComboBox::from_id_salt("cf_dup").selected_text(if q.unique { "Unique" } else { "Duplicate" }).show_ui(ui, |ui| {
                            ui.selectable_value(&mut q.unique, false, "Duplicate");
                            ui.selectable_value(&mut q.unique, true, "Unique");
                        });
                        ui.label("values");
                    }
                    Quick::Top { percent, .. } => {
                        let max = if percent { 100 } else { 1000 };
                        ui.add(egui::DragValue::new(&mut q.rank).range(1..=max));
                        if percent {
                            ui.label("%");
                        }
                    }
                    Quick::Average { .. } => {
                        ui.label("for the selected range");
                    }
                }
                ui.label("with");
                let label = presets.get(q.preset).map(|p| p.0).unwrap_or("Custom Format...");
                egui::ComboBox::from_id_salt("cf_preset").width(230.0).selected_text(label).show_ui(ui, |ui| {
                    for (i, (name, _)) in presets.iter().enumerate() {
                        ui.selectable_value(&mut q.preset, i, *name);
                    }
                    if ui.selectable_label(q.preset == presets.len(), "Custom Format...").clicked() {
                        q.preset = presets.len();
                        q.fmt = Some(q.custom.clone());
                    }
                });
            });
            if matches!(q.kind, Quick::Greater | Quick::Less | Quick::Between | Quick::Equal) {
                ui.label(RichText::new("Type a number, text, or a reference such as =$A$1.").size(11.0).color(theme::TEXT_DIM));
            }
            ui.add_space(4.0);
            let f = presets.get(q.preset).map(|p| p.1.clone()).unwrap_or_else(|| q.custom.clone());
            ui.horizontal(|ui| {
                ui.label("Preview:");
                format_preview(ui, &f, Vec2::new(200.0, 26.0));
            });
            match buttons(ui, &["OK", "Cancel"]) {
                Some(0) => result = Some(true),
                Some(_) => result = Some(false),
                None => {}
            }
            if !fmt_open {
                let (enter, esc) = enter_or_escape(ui);
                if enter {
                    result = Some(true);
                } else if esc {
                    result = Some(false);
                }
            }
        });
        if let Some(f) = &mut q.fmt {
            match format_window(ctx, f) {
                Some(true) => {
                    q.custom = f.clone();
                    q.fmt = None;
                }
                Some(false) => q.fmt = None,
                None => {}
            }
            return true;
        }
        match result {
            Some(true) => {
                let format = presets.get(q.preset).map(|p| p.1.clone()).unwrap_or_else(|| q.custom.clone());
                let kind = match q.kind {
                    Quick::Greater | Quick::Less | Quick::Equal | Quick::Between => {
                        let op = match q.kind {
                            Quick::Greater => CmpOp::Greater,
                            Quick::Less => CmpOp::Less,
                            Quick::Equal => CmpOp::Equal,
                            _ => CmpOp::Between,
                        };
                        let f1 = operand_from_input(&q.v1);
                        let f2 = if op.two() { operand_from_input(&q.v2) } else { String::new() };
                        if f1.is_empty() || (op.two() && f2.is_empty()) || check_formula(&f1).is_err() || (op.two() && check_formula(&f2).is_err()) {
                            return true;
                        }
                        RuleKind::CellValue { op, f1, f2 }
                    }
                    Quick::Text => RuleKind::Text { op: TextOp::Contains, text: q.v1.clone() },
                    Quick::Duplicate => RuleKind::Duplicate { unique: q.unique },
                    Quick::Top { bottom, percent } => RuleKind::Top { bottom, percent, rank: q.rank.max(1) },
                    Quick::Average { below } => RuleKind::Average { below, equal: false },
                };
                self.cf_add_rule(CondFormat::new(q.ranges.clone(), kind, format));
                false
            }
            Some(false) => false,
            None => true,
        }
    }

    fn cf_manager_ui(&mut self, ctx: &egui::Context, m: &mut Manager) -> bool {
        let mut result: Option<u8> = None; // 0 OK, 1 Cancel, 2 Apply
        let editing = m.editor.is_some();
        window("Conditional Formatting Rules Manager").show(ctx, |ui| {
            ui.set_min_width(640.0);
            ui.horizontal(|ui| {
                ui.label("Show formatting rules for:");
                egui::ComboBox::from_id_salt("cf_scope")
                    .selected_text(if m.whole_sheet { "This Worksheet" } else { "Current Selection" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut m.whole_sheet, false, "Current Selection");
                        ui.selectable_value(&mut m.whole_sheet, true, "This Worksheet");
                    });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("New Rule...").clicked() {
                    let mut e = RuleEditor::new(vec![m.sel], 1);
                    e.title = "New Formatting Rule";
                    m.editor = Some((None, e));
                }
                let sel = m.selected.filter(|i| *i < m.rules.len());
                if ui.add_enabled(sel.is_some(), egui::Button::new("Edit Rule...")).clicked() {
                    if let Some(i) = sel {
                        let mut rule = m.rules[i].clone();
                        if let Some(r) = parse_sqref(&m.applies[i]) {
                            rule.ranges = r;
                        }
                        m.editor = Some((Some(i), RuleEditor::from_rule(&rule)));
                    }
                }
                if ui.add_enabled(sel.is_some(), egui::Button::new("Delete Rule")).clicked() {
                    if let Some(i) = sel {
                        m.rules.remove(i);
                        m.applies.remove(i);
                        m.selected = None;
                    }
                }
                let visible = manager_visible(m);
                let pos = sel.and_then(|i| visible.iter().position(|v| *v == i));
                if ui.add_enabled(pos.is_some_and(|p| p > 0), egui::Button::new("▲")).on_hover_text("Move Up").clicked() {
                    if let Some(p) = pos {
                        let (a, b) = (visible[p - 1], visible[p]);
                        m.rules.swap(a, b);
                        m.applies.swap(a, b);
                        m.selected = Some(a);
                    }
                }
                if ui.add_enabled(pos.is_some_and(|p| p + 1 < visible.len()), egui::Button::new("▼")).on_hover_text("Move Down").clicked() {
                    if let Some(p) = pos {
                        let (a, b) = (visible[p], visible[p + 1]);
                        m.rules.swap(a, b);
                        m.applies.swap(a, b);
                        m.selected = Some(b);
                    }
                }
            });
            ui.add_space(4.0);
            egui::Frame::new().stroke(Stroke::new(1.0, theme::BORDER)).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                ui.set_min_height(200.0);
                ui.set_width(620.0);
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    egui::Grid::new("cf_rules").num_columns(4).striped(true).spacing(Vec2::new(10.0, 4.0)).show(ui, |ui| {
                        ui.label(RichText::new("Rule (applied in order shown)").strong());
                        ui.label(RichText::new("Format").strong());
                        ui.label(RichText::new("Applies to").strong());
                        ui.label(RichText::new("Stop If True").strong());
                        ui.end_row();
                        for i in manager_visible(m) {
                            let rule = &mut m.rules[i];
                            if ui.selectable_label(m.selected == Some(i), rule.describe()).clicked() {
                                m.selected = Some(i);
                            }
                            match &rule.kind {
                                RuleKind::ColorScale { colors } => {
                                    let (r, _) = ui.allocate_exact_size(Vec2::new(120.0, 18.0), Sense::hover());
                                    scale_preview(ui.painter(), r, colors);
                                }
                                RuleKind::DataBar { color, gradient } => {
                                    let (r, _) = ui.allocate_exact_size(Vec2::new(120.0, 18.0), Sense::hover());
                                    paint_data_bar(ui.painter(), r, &DataBarDraw { start: 0.0, end: 0.8, color: *color, gradient: *gradient, negative: false });
                                }
                                _ => format_preview(ui, &rule.format, Vec2::new(120.0, 18.0)),
                            }
                            ui.add_sized([170.0, 20.0], egui::TextEdit::singleline(&mut m.applies[i]));
                            if rule.is_visual() {
                                ui.label("");
                            } else {
                                ui.checkbox(&mut rule.stop, "");
                            }
                            ui.end_row();
                        }
                    });
                });
            });
            if let Some(e) = &m.error {
                ui.colored_label(Color32::from_rgb(0xC0, 0, 0), e);
            }
            match buttons(ui, &["OK", "Cancel", "Apply"]) {
                Some(0) => result = Some(0),
                Some(1) => result = Some(1),
                Some(_) => result = Some(2),
                None => {}
            }
            if !editing {
                let (enter, esc) = enter_or_escape(ui);
                if esc {
                    result = Some(1);
                } else if enter {
                    result = Some(0);
                }
            }
        });

        if let Some((idx, e)) = &mut m.editor {
            match rule_editor_ui(ctx, e) {
                Some(true) => match e.build() {
                    Ok(rule) => {
                        let text = rule.applies_to();
                        match idx {
                            Some(i) if *i < m.rules.len() => {
                                m.rules[*i] = rule;
                                m.applies[*i] = text;
                            }
                            _ => {
                                m.rules.insert(0, rule);
                                m.applies.insert(0, text);
                                m.selected = Some(0);
                            }
                        }
                        m.editor = None;
                    }
                    Err(msg) => e.error = Some(msg),
                },
                Some(false) => m.editor = None,
                None => {}
            }
            return true;
        }

        match result {
            Some(1) => false,
            Some(code) => {
                let mut rules = m.rules.clone();
                for (i, r) in rules.iter_mut().enumerate() {
                    match parse_sqref(&m.applies[i]) {
                        Some(rg) => r.ranges = rg,
                        None => {
                            m.error = Some(format!("The reference '{}' isn't valid.", m.applies[i]));
                            return true;
                        }
                    }
                }
                m.error = None;
                self.cf_set_rules(rules.clone());
                m.rules = rules;
                m.applies = m.rules.iter().map(|r| r.applies_to()).collect();
                code == 2
            }
            None => true,
        }
    }
}

/// Indices of the manager rules shown for the chosen scope.
fn manager_visible(m: &Manager) -> Vec<usize> {
    (0..m.rules.len())
        .filter(|&i| m.whole_sheet || parse_sqref(&m.applies[i]).unwrap_or_else(|| m.rules[i].ranges.clone()).iter().any(|r| r.intersects(&m.sel)))
        .collect()
}

/// The New / Edit Formatting Rule window. Returns Some(ok) when closed.
fn rule_editor_ui(ctx: &egui::Context, e: &mut RuleEditor) -> Option<bool> {
    let mut out = None;
    let fmt_open = e.fmt.is_some();
    egui::Window::new(e.title)
        .id(egui::Id::new("cf_rule_editor"))
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_CENTER, [0.0, -30.0])
        .show(ctx, |ui| {
            ui.set_min_width(460.0);
            ui.label("Select a Rule Type:");
            egui::Frame::new().stroke(Stroke::new(1.0, theme::BORDER)).inner_margin(egui::Margin::same(3)).show(ui, |ui| {
                ui.set_width(450.0);
                for (i, t) in RULE_TYPES.iter().enumerate() {
                    if ui.selectable_label(e.ty == i, format!("► {t}")).clicked() {
                        e.ty = i;
                        e.error = None;
                    }
                }
            });
            ui.add_space(6.0);
            ui.label("Edit the Rule Description:");
            egui::Frame::new().fill(Color32::from_gray(248)).stroke(Stroke::new(1.0, theme::BORDER)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                ui.set_width(440.0);
                rule_description(ui, e);
            });
            if let Some(err) = &e.error {
                ui.colored_label(Color32::from_rgb(0xC0, 0, 0), err);
            }
            match buttons(ui, &["OK", "Cancel"]) {
                Some(0) => out = Some(true),
                Some(_) => out = Some(false),
                None => {}
            }
            if !fmt_open {
                let (enter, esc) = enter_or_escape(ui);
                if enter {
                    out = Some(true);
                } else if esc {
                    out = Some(false);
                }
            }
        });
    if let Some(f) = &mut e.fmt {
        match format_window(ctx, f) {
            Some(true) => {
                e.format = f.clone();
                e.fmt = None;
            }
            Some(false) => e.fmt = None,
            None => {}
        }
        return None;
    }
    out
}

fn rule_description(ui: &mut egui::Ui, e: &mut RuleEditor) {
    match e.ty {
        0 => {
            ui.label(RichText::new("Format all cells based on their values:").strong());
            ui.horizontal(|ui| {
                ui.label("Format Style:");
                let names = ["2-Color Scale", "3-Color Scale", "Data Bar"];
                egui::ComboBox::from_id_salt("cf_style").selected_text(names[e.style.min(2)]).show_ui(ui, |ui| {
                    for (i, n) in names.iter().enumerate() {
                        ui.selectable_value(&mut e.style, i, *n);
                    }
                });
            });
            ui.add_space(4.0);
            if e.style < 2 {
                ui.horizontal(|ui| {
                    ui.push_id("min", |ui| color_button(ui, "Minimum (lowest value):", &mut e.colors[0]));
                });
                if e.style == 1 {
                    ui.horizontal(|ui| {
                        ui.push_id("mid", |ui| color_button(ui, "Midpoint (50th percentile):", &mut e.colors[1]));
                    });
                }
                ui.horizontal(|ui| {
                    ui.push_id("max", |ui| color_button(ui, "Maximum (highest value):", &mut e.colors[2]));
                });
                ui.horizontal(|ui| {
                    ui.label("Preview:");
                    let (r, _) = ui.allocate_exact_size(Vec2::new(260.0, 18.0), Sense::hover());
                    let cols: Vec<u32> = if e.style == 0 { vec![e.colors[0], e.colors[2]] } else { e.colors.to_vec() };
                    scale_preview(ui.painter(), r, &cols);
                });
            } else {
                ui.horizontal(|ui| {
                    ui.label("Fill:");
                    egui::ComboBox::from_id_salt("cf_bar_fill").selected_text(if e.bar_gradient { "Gradient Fill" } else { "Solid Fill" }).show_ui(ui, |ui| {
                        ui.selectable_value(&mut e.bar_gradient, true, "Gradient Fill");
                        ui.selectable_value(&mut e.bar_gradient, false, "Solid Fill");
                    });
                    ui.push_id("bar", |ui| color_button(ui, "Color:", &mut e.bar_color));
                });
                ui.horizontal(|ui| {
                    ui.label("Preview:");
                    let (r, _) = ui.allocate_exact_size(Vec2::new(160.0, 20.0), Sense::hover());
                    paint_data_bar(ui.painter(), r, &DataBarDraw { start: 0.0, end: 0.8, color: e.bar_color, gradient: e.bar_gradient, negative: false });
                });
            }
            return;
        }
        1 => {
            ui.label(RichText::new("Format only cells with:").strong());
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("cf_contains").selected_text(CONTAINS[e.contains.min(5)]).show_ui(ui, |ui| {
                    for (i, n) in CONTAINS.iter().enumerate() {
                        ui.selectable_value(&mut e.contains, i, *n);
                    }
                });
                match e.contains {
                    0 => {
                        egui::ComboBox::from_id_salt("cf_op").width(150.0).selected_text(e.op.label()).show_ui(ui, |ui| {
                            for op in CmpOp::ALL {
                                ui.selectable_value(&mut e.op, op, op.label());
                            }
                        });
                    }
                    1 => {
                        egui::ComboBox::from_id_salt("cf_textop").selected_text(e.text_op.label()).show_ui(ui, |ui| {
                            for op in TextOp::ALL {
                                ui.selectable_value(&mut e.text_op, op, op.label());
                            }
                        });
                    }
                    _ => {}
                }
            });
            match e.contains {
                0 => {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut e.v1).desired_width(150.0));
                        if e.op.two() {
                            ui.label("and");
                            ui.add(egui::TextEdit::singleline(&mut e.v2).desired_width(150.0));
                        }
                    });
                }
                1 => {
                    ui.add(egui::TextEdit::singleline(&mut e.text).desired_width(250.0));
                }
                _ => {}
            }
        }
        2 => {
            ui.label(RichText::new("Format values that rank in the:").strong());
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("cf_topbottom").selected_text(if e.bottom { "Bottom" } else { "Top" }).show_ui(ui, |ui| {
                    ui.selectable_value(&mut e.bottom, false, "Top");
                    ui.selectable_value(&mut e.bottom, true, "Bottom");
                });
                ui.add(egui::DragValue::new(&mut e.rank).range(1..=1000));
                ui.checkbox(&mut e.percent, "% of the selected range");
            });
        }
        3 => {
            ui.label(RichText::new("Format values that are:").strong());
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("cf_avg").selected_text(AVERAGES[e.avg.min(3)]).show_ui(ui, |ui| {
                    for (i, n) in AVERAGES.iter().enumerate() {
                        ui.selectable_value(&mut e.avg, i, *n);
                    }
                });
                ui.label("the average for the selected range");
            });
        }
        4 => {
            ui.label(RichText::new("Format all:").strong());
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("cf_unique").selected_text(if e.unique { "unique" } else { "duplicate" }).show_ui(ui, |ui| {
                    ui.selectable_value(&mut e.unique, false, "duplicate");
                    ui.selectable_value(&mut e.unique, true, "unique");
                });
                ui.label("values in the selected range");
            });
        }
        _ => {
            ui.label(RichText::new("Format values where this formula is true:").strong());
            ui.add(egui::TextEdit::singleline(&mut e.formula).desired_width(420.0).hint_text("=$A1>100"));
            let base = e.ranges.first().map(|r| crate::model::cell_name(r.r1, r.c1)).unwrap_or_else(|| "A1".into());
            ui.label(RichText::new(format!("Relative references are relative to {base}, the top-left cell of {}.", sqref(&e.ranges))).size(11.0).color(theme::TEXT_DIM));
        }
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Preview:");
        format_preview(ui, &e.format, Vec2::new(220.0, 26.0));
        if ui.button("Format...").clicked() {
            e.fmt = Some(e.format.clone());
        }
    });
}

/// A big ribbon button with a painted icon area on top and a two-line caption.
pub fn ribbon_button(ui: &mut egui::Ui, size: Vec2, text: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    if resp.is_pointer_button_down_on() {
        p.rect_filled(rect, 3.0, theme::PRESSED);
    } else if resp.hovered() {
        p.rect_filled(rect, 3.0, theme::HOVER);
    }
    for (i, line) in text.lines().enumerate() {
        let pos = Pos2::new(rect.center().x, rect.top() + 38.0 + i as f32 * 15.0);
        p.text(pos, Align2::CENTER_CENTER, line, theme::ui_font(12.0), theme::TEXT);
    }
    resp
}

/// Mini icon for the ribbon button: a table with highlighted cells and a bar.
fn paint_cf_icon(p: &egui::Painter, r: Rect) {
    let grid = Color32::from_gray(120);
    let cw = r.width() / 3.0;
    let ch = r.height() / 3.0;
    let fills = [[None, Some(0xF8696B), None], [Some(0xFFEB84), None, Some(0x63BE7B)], [None, Some(0x638EC6), None]];
    for (i, row) in fills.iter().enumerate() {
        for (j, f) in row.iter().enumerate() {
            let cell = Rect::from_min_size(Pos2::new(r.left() + j as f32 * cw, r.top() + i as f32 * ch), Vec2::new(cw, ch));
            if let Some(c) = f {
                p.rect_filled(cell.shrink(0.5), 0.0, rgb(*c));
            }
        }
    }
    for k in 0..=3 {
        let x = r.left() + k as f32 * cw;
        let y = r.top() + k as f32 * ch;
        p.line_segment([Pos2::new(x, r.top()), Pos2::new(x, r.bottom())], Stroke::new(1.0, grid));
        p.line_segment([Pos2::new(r.left(), y), Pos2::new(r.right(), y)], Stroke::new(1.0, grid));
    }
}
