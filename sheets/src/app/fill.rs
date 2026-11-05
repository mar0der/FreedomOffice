//! Drag-fill (fill handle) and the Auto Fill Options button shown after a fill.

use eframe::egui::{self, Pos2, Rect, Stroke, StrokeKind, Vec2};

use super::actions::shift_cell;
use super::theme;
use super::{App, Scope};
use crate::model::{Cell, Range, Value, Workbook};
use crate::numfmt;

const MONTHS_SHORT: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTHS_LONG: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December",
];
const DAYS_SHORT: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const DAYS_LONG: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

/// How a fill extends the source cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FillKind {
    /// Plain drag: a single number copies; dates, names and text with numbers make a series;
    /// several numbers continue their linear trend.
    Auto,
    /// Ctrl+drag: the opposite of Auto (a single number counts up, a series copies).
    Toggle,
    /// Auto Fill Options > Copy Cells.
    Copy,
    /// Auto Fill Options > Fill Series.
    Series,
    /// Auto Fill Options > Fill Formatting Only.
    FormatsOnly,
    /// Auto Fill Options > Fill Without Formatting.
    WithoutFormatting,
}

/// Fill-series detection for drag-fill.
enum Series {
    Copy,
    /// value(k) = base + step * k, k counted from the first source cell.
    Linear { base: f64, step: f64 },
    TextNum { prefix: String, first: i64, step: i64, width: usize },
    Names { list: &'static [&'static str], start: usize, step: i64 },
}

impl Series {
    fn detect(seq: &[Option<Cell>], wb: &Workbook, kind: FillKind) -> Series {
        if matches!(kind, FillKind::Copy | FillKind::FormatsOnly) {
            return Series::Copy;
        }
        let auto = Self::detect_auto(seq, wb);
        let single_number = match seq {
            [Some(c)] => c.formula.is_none() && matches!(c.value, Value::Number(_)),
            _ => false,
        };
        let count_up = |seq: &[Option<Cell>]| match seq.first().and_then(|c| c.as_ref()).map(|c| &c.value) {
            Some(Value::Number(n)) => Series::Linear { base: *n, step: 1.0 },
            _ => Series::Copy,
        };
        match kind {
            FillKind::Series if matches!(auto, Series::Copy) && single_number => count_up(seq),
            FillKind::Toggle if matches!(auto, Series::Copy) && single_number => count_up(seq),
            FillKind::Toggle => Series::Copy,
            _ => auto,
        }
    }

    fn detect_auto(seq: &[Option<Cell>], wb: &Workbook) -> Series {
        if seq.iter().any(|c| c.as_ref().is_some_and(|c| c.formula.is_some())) {
            return Series::Copy;
        }
        let vals: Vec<Value> = seq.iter().map(|c| c.as_ref().map(|c| c.value.clone()).unwrap_or_default()).collect();
        if vals.iter().all(|v| matches!(v, Value::Number(_))) {
            let nums: Vec<f64> = vals.iter().filter_map(|v| v.as_number()).collect();
            if nums.len() >= 2 {
                // Least-squares linear trend (exact for two values), like the familiar AutoFill.
                let n = nums.len() as f64;
                let mx = (n - 1.0) / 2.0;
                let my = nums.iter().sum::<f64>() / n;
                let (mut sxy, mut sxx) = (0.0, 0.0);
                for (i, y) in nums.iter().enumerate() {
                    let dx = i as f64 - mx;
                    sxy += dx * (y - my);
                    sxx += dx * dx;
                }
                let step = sxy / sxx;
                return Series::Linear { base: my - step * mx, step };
            }
            if nums.len() == 1 {
                let fmt = seq[0].as_ref().map(|c| wb.style(c.style).num_fmt.clone()).unwrap_or_default();
                if numfmt::is_date_format(&fmt) {
                    // A date counts in days; a time on its own counts in hours.
                    let lower = fmt.to_ascii_lowercase();
                    let time_only = lower.contains('h') && !lower.contains('d') && !lower.contains('y');
                    return Series::Linear { base: nums[0], step: if time_only { 1.0 / 24.0 } else { 1.0 } };
                }
            }
            return Series::Copy;
        }
        if let Some(texts) = vals.iter().map(|v| if let Value::Text(t) = v { Some(t.clone()) } else { None }).collect::<Option<Vec<_>>>() {
            for list in [&MONTHS_SHORT[..], &MONTHS_LONG[..], &DAYS_SHORT[..], &DAYS_LONG[..]] {
                let idx: Option<Vec<usize>> = texts.iter().map(|t| list.iter().position(|m| m.eq_ignore_ascii_case(t))).collect();
                if let Some(idx) = idx {
                    let step = if idx.len() >= 2 { idx[1] as i64 - idx[0] as i64 } else { 1 };
                    return Series::Names { list, start: idx[0], step };
                }
            }
            // "Item 1", "Q3"
            let split = |t: &str| -> Option<(String, i64, usize)> {
                let digits: String = t.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
                if digits.is_empty() || digits.len() == t.len() {
                    return None;
                }
                Some((t[..t.len() - digits.len()].to_string(), digits.parse().ok()?, digits.len()))
            };
            let parts: Option<Vec<_>> = texts.iter().map(|t| split(t)).collect();
            if let Some(p) = parts {
                if p.iter().all(|x| x.0 == p[0].0) {
                    let step = if p.len() >= 2 { (p[p.len() - 1].1 - p[0].1) / (p.len() as i64 - 1) } else { 1 };
                    return Series::TextNum { prefix: p[0].0.clone(), first: p[0].1, step, width: p[0].2 };
                }
            }
        }
        Series::Copy
    }

    /// Value at absolute step index (0 = first source cell).
    fn value(&self, k: i64) -> Option<Value> {
        match self {
            Series::Copy => None,
            Series::Linear { base, step } => Some(Value::Number(base + step * k as f64)),
            Series::TextNum { prefix, first, step, width } => {
                let n = first + step * k;
                let w = if n >= 0 { *width } else { 0 };
                Some(Value::Text(format!("{prefix}{:0w$}", n, w = w)))
            }
            Series::Names { list, start, step } => {
                let i = (*start as i64 + step * k).rem_euclid(list.len() as i64) as usize;
                Some(Value::Text(list[i].to_string()))
            }
        }
    }
}

/// Fills `target` (which contains `src` and extends it in one direction) from the source cells.
pub fn fill_cells(wb: &mut Workbook, si: usize, src: Range, target: Range, kind: FillKind) {
    let vertical = target.rows() != src.rows();
    let (lines, len) = if vertical { (src.c1..=src.c2, src.rows()) } else { (src.r1..=src.r2, src.cols()) };
    let forward = if vertical { target.r2 > src.r2 } else { target.c2 > src.c2 };
    let count = if vertical { target.rows() - src.rows() } else { target.cols() - src.cols() };
    for line in lines {
        let seq: Vec<Option<Cell>> = (0..len)
            .map(|k| {
                let (r, c) = if vertical { (src.r1 + k, line) } else { (line, src.c1 + k) };
                wb.sheets[si].get(r, c).cloned()
            })
            .collect();
        let series = Series::detect(&seq, wb, kind);
        for k in 0..count {
            let step_index = if forward { len as i64 + k as i64 } else { -(k as i64) - 1 };
            let src_idx = step_index.rem_euclid(len as i64) as u32;
            let (r, c) = if vertical {
                (if forward { src.r2 + 1 + k } else { src.r1 - 1 - k }, line)
            } else {
                (line, if forward { src.c2 + 1 + k } else { src.c1 - 1 - k })
            };
            let (sr, sc) = if vertical { (src.r1 + src_idx, line) } else { (line, src.c1 + src_idx) };
            let source = seq[src_idx as usize].clone();
            let existing = wb.sheets[si].get(r, c).cloned();
            let out = if kind == FillKind::FormatsOnly {
                let style = source.as_ref().map_or(0, |s| s.style);
                match existing {
                    Some(mut e) => {
                        e.style = style;
                        Some(e)
                    }
                    None if style != 0 => Some(Cell { style, ..Default::default() }),
                    None => None,
                }
            } else {
                let mut cell = source;
                if let Some(cell) = &mut cell {
                    if cell.formula.is_some() {
                        shift_cell(cell, r as i64 - sr as i64, c as i64 - sc as i64);
                    } else if let Some(v) = series.value(step_index) {
                        cell.value = v;
                    }
                }
                if kind == FillKind::WithoutFormatting {
                    let keep = existing.map_or(0, |e| e.style);
                    match &mut cell {
                        Some(cell) => cell.style = keep,
                        None if keep != 0 => cell = Some(Cell { style: keep, ..Default::default() }),
                        None => {}
                    }
                }
                cell
            };
            wb.sheets[si].set(r, c, out);
        }
    }
}

/// The kept part of `src` when the fill handle is dragged back inside it to cell (r, c): rows
/// (or columns, whichever more are given up) after the pointer are left out.
pub fn shrink_target(src: Range, r: u32, c: u32) -> Range {
    let rows = src.r2 - r.clamp(src.r1, src.r2);
    let cols = src.c2 - c.clamp(src.c1, src.c2);
    if rows == 0 && cols == 0 {
        src
    } else if rows >= cols {
        Range { r2: src.r2 - rows, ..src }
    } else {
        Range { c2: src.c2 - cols, ..src }
    }
}

/// The cells of `src` a shrinking fill drag leaves out (they are cleared on release).
pub fn shrunk_part(src: Range, keep: Range) -> Option<Range> {
    if keep.r2 < src.r2 && keep.r1 == src.r1 && keep.c1 == src.c1 && keep.c2 == src.c2 {
        Some(Range { r1: keep.r2 + 1, ..src })
    } else if keep.c2 < src.c2 && keep.r1 == src.r1 && keep.c1 == src.c1 && keep.r2 == src.r2 {
        Some(Range { c1: keep.c2 + 1, ..src })
    } else {
        None
    }
}

/// The Auto Fill Options button left at the bottom-right of a filled range.
pub struct FillButton {
    pub sheet: usize,
    pub src: Range,
    pub target: Range,
    pub kind: FillKind,
    /// Workbook edit counter right after the fill: any later edit removes the button.
    pub edits: u64,
    pub open: bool,
}

impl App {
    pub fn autofill(&mut self, src: Range, target: Range) {
        self.autofill_kind(src, target, FillKind::Auto);
    }

    pub fn autofill_kind(&mut self, src: Range, target: Range, kind: FillKind) {
        if self.locked_block(target) {
            return;
        }
        let si = self.wb.active;
        self.push_undo(&[Scope::Range(si, target)]);
        // The option the button shows as chosen: what the first line actually did.
        let shown = match kind {
            FillKind::Auto | FillKind::Toggle => {
                let vertical = target.rows() != src.rows();
                let n = if vertical { src.rows() } else { src.cols() };
                let seq: Vec<Option<Cell>> = (0..n)
                    .map(|k| if vertical { self.wb.sheets[si].get(src.r1 + k, src.c1) } else { self.wb.sheets[si].get(src.r1, src.c1 + k) }.cloned())
                    .collect();
                if matches!(Series::detect(&seq, &self.wb, kind), Series::Copy) { FillKind::Copy } else { FillKind::Series }
            }
            k => k,
        };
        fill_cells(&mut self.wb, si, src, target, kind);
        self.select_range(target);
        self.changed();
        self.paste_button = None;
        self.fill_button = Some(FillButton { sheet: si, src, target, kind: shown, edits: self.edits, open: false });
    }

    /// Fill handle dragged back inside the selection: like Excel, clears the contents (not the
    /// formats) of the cells left out and selects the rest.
    pub fn fill_shrink(&mut self, src: Range, keep: Range) {
        let Some(gone) = shrunk_part(src, keep) else { return };
        if self.locked_block(gone) {
            return;
        }
        let si = self.wb.active;
        self.push_undo(&[Scope::Range(si, gone)]);
        let keys: Vec<(u32, u32)> = self.wb.sheets[si].iter_range(gone).map(|(r, c, _)| (r, c)).collect();
        for (r, c) in keys {
            let sh = &mut self.wb.sheets[si];
            if let Some(cell) = sh.get_mut(r, c) {
                cell.value = Value::Empty;
                cell.formula = None;
            }
            sh.tidy(r, c);
        }
        self.select_range(keep);
        self.changed_ranges(si, &[gone]);
        self.fill_button = None;
    }

    /// Re-does the last fill with another Auto Fill Options choice.
    fn refill(&mut self, kind: FillKind) {
        let Some(b) = self.fill_button.take() else { return };
        self.undo();
        self.autofill_kind(b.src, b.target, kind);
    }

    /// Paints the floating Auto Fill Options / Paste Options buttons and their menus.
    pub fn float_buttons_ui(&mut self, ui: &mut egui::Ui) {
        self.menu.float_rects.clear();
        let Some(lay) = self.layout.clone() else { return };
        let busy = self.edit.is_some() || self.drag != super::Drag::None;
        if self.fill_button.as_ref().is_some_and(|b| b.edits != self.edits || b.sheet >= self.wb.sheets.len()) {
            self.fill_button = None;
        }
        if let Some(b) = &self.fill_button {
            if b.sheet == self.wb.active && !busy {
                let at = self.range_rect(&lay, b.target).right_bottom();
                if lay.cells.contains(at) {
                    let (open, kind) = (b.open, b.kind);
                    let (clicked, rect) = float_button(ui, egui::Id::new("autofill_options"), at, open, paint_fill_icon);
                    self.menu.float_rects.push(rect);
                    if clicked {
                        if let Some(b) = &mut self.fill_button {
                            b.open = !b.open;
                        }
                    }
                    if open {
                        self.fill_menu(ui.ctx(), rect, kind);
                    }
                }
            }
        }
        self.paste_button_ui(ui, &lay, busy);
    }

    fn fill_menu(&mut self, ctx: &egui::Context, anchor: Rect, kind: FillKind) {
        let mut pick = None;
        let mut close = false;
        let resp = egui::Area::new(egui::Id::new("autofill_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor.left_bottom() + Vec2::new(0.0, 2.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(190.0);
                    for (label, k) in [
                        ("Copy Cells", FillKind::Copy),
                        ("Fill Series", FillKind::Series),
                        ("Fill Formatting Only", FillKind::FormatsOnly),
                        ("Fill Without Formatting", FillKind::WithoutFormatting),
                    ] {
                        if ui.radio(kind == k, label).clicked() {
                            pick = Some(k);
                        }
                    }
                    if ui.radio(false, "Flash Fill").clicked() {
                        close = true;
                        pick = None;
                        self.fill_button_flash();
                    }
                });
            });
        if let Some(k) = pick {
            self.refill(k);
        } else if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if let Some(b) = &mut self.fill_button {
                b.open = false;
            }
        } else if ctx.input(|i| i.pointer.any_pressed())
            && !ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| resp.response.rect.contains(p) || anchor.contains(p))
        {
            if let Some(b) = &mut self.fill_button {
                b.open = false;
            }
        }
    }

    /// Auto Fill Options > Flash Fill: fills the dragged cells from the examples above them.
    fn fill_button_flash(&mut self) {
        let Some(b) = self.fill_button.take() else { return };
        self.undo();
        self.select_range(b.target);
        self.flash_fill();
    }
}

/// A small floating option button anchored just outside a range corner. Returns (clicked, rect).
pub fn float_button(ui: &mut egui::Ui, id: egui::Id, corner: Pos2, open: bool, icon: fn(&egui::Painter, Rect)) -> (bool, Rect) {
    let rect = Rect::from_min_size(corner + Vec2::new(3.0, 3.0), Vec2::new(30.0, 20.0));
    let resp = ui.interact(rect, id, egui::Sense::click());
    let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, id));
    let fill = if open || resp.hovered() { theme::hover() } else { theme::window() };
    p.rect_filled(rect, 2.0, fill);
    p.rect_stroke(rect, 2.0, Stroke::new(1.0, theme::border()), StrokeKind::Inside);
    icon(&p, Rect::from_min_size(rect.min + Vec2::new(3.0, 3.0), Vec2::splat(14.0)));
    // drop-down arrow
    let c = Pos2::new(rect.right() - 7.0, rect.center().y + 1.0);
    p.add(egui::Shape::convex_polygon(
        vec![c + Vec2::new(-3.0, -2.0), c + Vec2::new(3.0, -2.0), c + Vec2::new(0.0, 2.0)],
        theme::text(),
        Stroke::NONE,
    ));
    (resp.clicked(), rect)
}

/// Auto Fill Options icon: a little grid of cells with the bottom row filled.
fn paint_fill_icon(p: &egui::Painter, r: Rect) {
    let s = r.width() / 3.0;
    for i in 0..3 {
        for j in 0..3 {
            let cell = Rect::from_min_size(r.min + Vec2::new(j as f32 * s, i as f32 * s), Vec2::splat(s));
            if i == 2 {
                p.rect_filled(cell.shrink(0.5), 0.0, theme::accent());
            }
            p.rect_stroke(cell, 0.0, Stroke::new(1.0, theme::text_dim()), StrokeKind::Inside);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{cell_from_input, Sheet};

    fn wb_with(vals: &[(u32, u32, &str)]) -> Workbook {
        let mut wb = Workbook::empty_with(vec![Sheet::new("Sheet1")]);
        for (r, c, t) in vals {
            let cell = cell_from_input(&mut wb, t, 0);
            wb.sheets[0].set(*r, *c, cell);
        }
        wb
    }

    fn col(wb: &Workbook, rows: std::ops::RangeInclusive<u32>) -> Vec<Value> {
        rows.map(|r| wb.sheets[0].value(r, 0).clone()).collect()
    }

    fn nums(v: &[f64]) -> Vec<Value> {
        v.iter().map(|n| Value::Number(*n)).collect()
    }

    #[test]
    fn single_number_copies_and_ctrl_counts_up() {
        let mut wb = wb_with(&[(0, 0, "5")]);
        fill_cells(&mut wb, 0, Range::cell(0, 0), Range::new(0, 0, 3, 0), FillKind::Auto);
        assert_eq!(col(&wb, 0..=3), nums(&[5.0, 5.0, 5.0, 5.0]));
        let mut wb = wb_with(&[(0, 0, "5")]);
        fill_cells(&mut wb, 0, Range::cell(0, 0), Range::new(0, 0, 3, 0), FillKind::Toggle);
        assert_eq!(col(&wb, 0..=3), nums(&[5.0, 6.0, 7.0, 8.0]));
        let mut wb = wb_with(&[(0, 0, "5")]);
        fill_cells(&mut wb, 0, Range::cell(0, 0), Range::new(0, 0, 2, 0), FillKind::Series);
        assert_eq!(col(&wb, 0..=2), nums(&[5.0, 6.0, 7.0]));
    }

    #[test]
    fn dates_and_text_numbers_count_and_ctrl_copies() {
        let mut wb = wb_with(&[(0, 0, "11/4/2026")]);
        fill_cells(&mut wb, 0, Range::cell(0, 0), Range::new(0, 0, 2, 0), FillKind::Auto);
        assert_eq!(col(&wb, 0..=2), nums(&[46330.0, 46331.0, 46332.0]));
        let mut wb = wb_with(&[(0, 0, "11/4/2026")]);
        fill_cells(&mut wb, 0, Range::cell(0, 0), Range::new(0, 0, 2, 0), FillKind::Toggle);
        assert_eq!(col(&wb, 0..=2), nums(&[46330.0, 46330.0, 46330.0]));
        let mut wb = wb_with(&[(0, 0, "Item 9")]);
        fill_cells(&mut wb, 0, Range::cell(0, 0), Range::new(0, 0, 2, 0), FillKind::Auto);
        assert_eq!(col(&wb, 0..=2), vec![Value::Text("Item 9".into()), Value::Text("Item 10".into()), Value::Text("Item 11".into())]);
        let mut wb = wb_with(&[(0, 0, "Item 9")]);
        fill_cells(&mut wb, 0, Range::cell(0, 0), Range::new(0, 0, 1, 0), FillKind::Toggle);
        assert_eq!(col(&wb, 0..=1), vec![Value::Text("Item 9".into()), Value::Text("Item 9".into())]);
    }

    #[test]
    fn two_or_more_numbers_continue_the_linear_trend() {
        let mut wb = wb_with(&[(0, 0, "1"), (1, 0, "3")]);
        fill_cells(&mut wb, 0, Range::new(0, 0, 1, 0), Range::new(0, 0, 4, 0), FillKind::Auto);
        assert_eq!(col(&wb, 0..=4), nums(&[1.0, 3.0, 5.0, 7.0, 9.0]));
        // Best-fit line through 1, 2, 4: the source cells stay, new cells follow the trend.
        let mut wb = wb_with(&[(0, 0, "1"), (1, 0, "2"), (2, 0, "4")]);
        fill_cells(&mut wb, 0, Range::new(0, 0, 2, 0), Range::new(0, 0, 4, 0), FillKind::Auto);
        let v: Vec<f64> = col(&wb, 0..=4).iter().map(|v| v.as_number().unwrap()).collect();
        assert_eq!(&v[..3], &[1.0, 2.0, 4.0]);
        assert!((v[3] - 16.0 / 3.0).abs() < 1e-9 && (v[4] - 41.0 / 6.0).abs() < 1e-9, "{v:?}");
        // Ctrl+drag of a numeric series repeats it instead.
        let mut wb = wb_with(&[(0, 0, "1"), (1, 0, "3")]);
        fill_cells(&mut wb, 0, Range::new(0, 0, 1, 0), Range::new(0, 0, 3, 0), FillKind::Toggle);
        assert_eq!(col(&wb, 0..=3), nums(&[1.0, 3.0, 1.0, 3.0]));
    }

    #[test]
    fn formatting_only_and_without_formatting() {
        let mut wb = wb_with(&[(0, 0, "$5.00"), (1, 0, "x")]);
        let money = wb.sheets[0].get(0, 0).unwrap().style;
        assert_ne!(money, 0);
        fill_cells(&mut wb, 0, Range::cell(0, 0), Range::new(0, 0, 1, 0), FillKind::FormatsOnly);
        assert_eq!(wb.sheets[0].value(1, 0), &Value::Text("x".into()));
        assert_eq!(wb.sheets[0].get(1, 0).unwrap().style, money);
        let mut wb = wb_with(&[(0, 0, "$5.00")]);
        fill_cells(&mut wb, 0, Range::cell(0, 0), Range::new(0, 0, 1, 0), FillKind::WithoutFormatting);
        assert_eq!(wb.sheets[0].value(1, 0), &Value::Number(5.0));
        assert_eq!(wb.sheets[0].get(1, 0).unwrap().style, 0);
    }

    #[test]
    fn dragging_the_handle_back_inside_clears_the_cells_left_out() {
        let src = Range::new(1, 2, 6, 3);
        // Pointer back on row 3 (index 2): rows 4-7 are given up.
        assert_eq!(shrink_target(src, 2, 3), Range::new(1, 2, 2, 3));
        assert_eq!(shrunk_part(src, Range::new(1, 2, 2, 3)), Some(Range::new(3, 2, 6, 3)));
        // Pointer in the first column on the last row: the second column is given up.
        assert_eq!(shrink_target(src, 6, 2), Range::new(1, 2, 6, 2));
        assert_eq!(shrink_target(src, 6, 3), src);
        assert_eq!(shrunk_part(src, src), None);

        let (_ctx, mut app) = super::super::data_ui::sample_app();
        let date_style = app.sheet().get(5, 3).unwrap().style;
        app.fill_shrink(src, Range::new(1, 2, 2, 3));
        assert_eq!(app.sel(), Range::new(1, 2, 2, 3));
        assert_eq!(app.sheet().value(2, 2), &Value::Number(340.0));
        for r in 3..=6 {
            assert_eq!(app.sheet().value(r, 2), &Value::Empty);
            assert_eq!(app.sheet().value(r, 3), &Value::Empty);
        }
        // Contents go, formats stay; one undo brings the values back.
        assert_eq!(app.sheet().get(5, 3).map(|c| c.style), Some(date_style));
        assert_eq!(app.sheet().value(7, 2), &Value::Number(500.0));
        app.undo();
        assert_eq!(app.sheet().value(4, 2), &Value::Number(410.0));
    }
}
