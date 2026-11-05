//! Icon sets: vector drawings of the icons, the Icon Sets gallery menu and the icon editor rows
//! of the formatting rule dialog.

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2};

use super::theme;
use super::App;
use crate::condfmt::RuleKind;
use crate::iconset::{IconDraw, IconSetRule, IconStyle, Threshold, ThresholdKind};

const RED: Color32 = Color32::from_rgb(0xD8, 0x3A, 0x3A);
const YELLOW: Color32 = Color32::from_rgb(0xF2, 0xB8, 0x00);
const GREEN: Color32 = Color32::from_rgb(0x2E, 0x9E, 0x4F);
const GRAY: Color32 = Color32::from_rgb(0x80, 0x80, 0x80);
const BLACK: Color32 = Color32::from_rgb(0x30, 0x30, 0x30);
const PINK: Color32 = Color32::from_rgb(0xF0, 0x9A, 0x9A);
const BLUE: Color32 = Color32::from_rgb(0x3F, 0x6F, 0xC0);
const EMPTY: Color32 = Color32::from_rgb(0xD9, 0xD9, 0xD9);
const GOLD: Color32 = Color32::from_rgb(0xF5, 0xB4, 0x00);

/// Traffic-light colour of icon `idx` in an `n`-icon red/yellow/green set.
fn rag(idx: u8, n: usize) -> Color32 {
    match (n, idx) {
        (_, 0) => RED,
        (3, 1) => YELLOW,
        (3, _) => GREEN,
        (_, i) if (i as usize) + 1 == n => GREEN,
        _ => YELLOW,
    }
}

/// Arrow direction (degrees, 0 = right, 90 = up) of icon `idx` in an `n`-arrow set.
fn arrow_angle(idx: u8, n: usize) -> f32 {
    let steps: &[f32] = match n {
        3 => &[-90.0, 0.0, 90.0],
        4 => &[-90.0, -45.0, 45.0, 90.0],
        _ => &[-90.0, -45.0, 0.0, 45.0, 90.0],
    };
    steps[(idx as usize).min(steps.len() - 1)]
}

fn rot(c: Pos2, v: Vec2, deg: f32) -> Pos2 {
    let (s, k) = (-deg.to_radians()).sin_cos();
    c + Vec2::new(v.x * k - v.y * s, v.x * s + v.y * k)
}

fn arrow(p: &egui::Painter, r: Rect, deg: f32, color: Color32) {
    let c = r.center();
    let h = r.width() / 2.0;
    let shaft = [Vec2::new(-h * 0.85, -h * 0.2), Vec2::new(h * 0.1, -h * 0.2), Vec2::new(h * 0.1, h * 0.2), Vec2::new(-h * 0.85, h * 0.2)];
    p.add(Shape::convex_polygon(shaft.iter().map(|v| rot(c, *v, deg)).collect(), color, Stroke::NONE));
    let head = [Vec2::new(h * 0.9, 0.0), Vec2::new(0.0, -h * 0.65), Vec2::new(0.0, h * 0.65)];
    p.add(Shape::convex_polygon(head.iter().map(|v| rot(c, *v, deg)).collect(), color, Stroke::NONE));
}

fn star(p: &egui::Painter, r: Rect, fill: Color32, outline: Color32) {
    let c = r.center();
    let ro = r.width() / 2.0;
    let ri = ro * 0.42;
    let pt = |k: usize, rad: f32| {
        let a = (90.0 + 36.0 * k as f32).to_radians();
        Pos2::new(c.x + rad * a.cos(), c.y - rad * a.sin())
    };
    let inner: Vec<Pos2> = (0..5).map(|i| pt(2 * i + 1, ri)).collect();
    p.add(Shape::convex_polygon(inner.clone(), fill, Stroke::NONE));
    for i in 0..5 {
        p.add(Shape::convex_polygon(vec![pt(2 * i, ro), inner[(i + 4) % 5], inner[i]], fill, Stroke::NONE));
    }
    let outline_pts: Vec<Pos2> = (0..10).map(|k| pt(k, if k % 2 == 0 { ro } else { ri })).collect();
    p.add(Shape::closed_line(outline_pts, Stroke::new(1.0, outline)));
}

/// A filled pie slice from 12 o'clock clockwise over `frac` of the circle.
fn pie(p: &egui::Painter, c: Pos2, rad: f32, frac: f32, color: Color32) {
    let steps = (48.0 * frac).ceil().max(1.0) as usize;
    for k in 0..steps {
        let a0 = std::f32::consts::TAU * frac * k as f32 / steps as f32;
        let a1 = std::f32::consts::TAU * frac * (k + 1) as f32 / steps as f32;
        let q = |a: f32| Pos2::new(c.x + rad * a.sin(), c.y - rad * a.cos());
        p.add(Shape::convex_polygon(vec![c, q(a0), q(a1)], color, Stroke::NONE));
    }
}

fn flag(p: &egui::Painter, r: Rect, color: Color32) {
    let x = r.left() + r.width() * 0.25;
    p.line_segment([Pos2::new(x, r.top() + 1.0), Pos2::new(x, r.bottom())], Stroke::new(1.4, BLACK));
    let f = Rect::from_min_max(Pos2::new(x + 0.7, r.top() + 1.0), Pos2::new(r.right() - 1.0, r.top() + r.height() * 0.6));
    p.add(Shape::convex_polygon(vec![f.left_top(), Pos2::new(f.right(), f.top() + f.height() * 0.15), Pos2::new(f.right(), f.bottom() - f.height() * 0.1), f.left_bottom()], color, Stroke::NONE));
}

fn symbol(p: &egui::Painter, r: Rect, idx: u8, color: Color32, width: f32) {
    let st = Stroke::new(width, color);
    let c = r.center();
    let h = r.width() / 2.0;
    match idx {
        0 => {
            p.line_segment([c + Vec2::new(-h * 0.55, -h * 0.55), c + Vec2::new(h * 0.55, h * 0.55)], st);
            p.line_segment([c + Vec2::new(-h * 0.55, h * 0.55), c + Vec2::new(h * 0.55, -h * 0.55)], st);
        }
        1 => {
            p.line_segment([c + Vec2::new(0.0, -h * 0.7), c + Vec2::new(0.0, h * 0.2)], st);
            p.circle_filled(c + Vec2::new(0.0, h * 0.62), width * 0.6, color);
        }
        _ => {
            p.line(vec![c + Vec2::new(-h * 0.6, 0.0), c + Vec2::new(-h * 0.15, h * 0.5), c + Vec2::new(h * 0.65, -h * 0.55)], st);
        }
    }
}

/// Draws icon `idx` of `style` (0 = the icon for the lowest values) inside `r`.
pub fn paint_icon(p: &egui::Painter, r: Rect, style: IconStyle, idx: u8) {
    let n = style.count();
    let c = r.center();
    let rad = r.width().min(r.height()) / 2.0;
    match style {
        IconStyle::Arrows3 | IconStyle::Arrows4 | IconStyle::Arrows5 => arrow(p, r, arrow_angle(idx, n), rag(idx, n)),
        IconStyle::ArrowsGray3 | IconStyle::ArrowsGray4 | IconStyle::ArrowsGray5 => arrow(p, r, arrow_angle(idx, n), GRAY),
        IconStyle::Triangles3 => match idx {
            0 => {
                p.add(Shape::convex_polygon(vec![Pos2::new(r.left() + 1.0, c.y - rad * 0.45), Pos2::new(r.right() - 1.0, c.y - rad * 0.45), Pos2::new(c.x, c.y + rad * 0.6)], RED, Stroke::NONE));
            }
            1 => {
                p.rect_filled(Rect::from_center_size(c, Vec2::new(rad * 1.3, rad * 0.45)), 0.0, YELLOW);
            }
            _ => {
                p.add(Shape::convex_polygon(vec![Pos2::new(c.x, c.y - rad * 0.6), Pos2::new(r.right() - 1.0, c.y + rad * 0.45), Pos2::new(r.left() + 1.0, c.y + rad * 0.45)], GREEN, Stroke::NONE));
            }
        },
        IconStyle::TrafficLights3 => {
            p.circle_filled(c, rad * 0.85, rag(idx, n));
        }
        IconStyle::TrafficLightsRim3 => {
            p.rect_filled(r.shrink(0.5), rad * 0.35, BLACK);
            p.circle_filled(c, rad * 0.62, rag(idx, n));
        }
        IconStyle::TrafficLights4 => {
            let col = [BLACK, RED, YELLOW, GREEN][(idx as usize).min(3)];
            p.circle_filled(c, rad * 0.85, col);
        }
        IconStyle::RedToBlack4 => {
            let col = [BLACK, GRAY, PINK, RED][(idx as usize).min(3)];
            p.circle_filled(c, rad * 0.85, col);
        }
        IconStyle::Signs3 => match idx {
            0 => {
                let d = rad * 0.9;
                p.add(Shape::convex_polygon(vec![c + Vec2::new(0.0, -d), c + Vec2::new(d, 0.0), c + Vec2::new(0.0, d), c + Vec2::new(-d, 0.0)], RED, Stroke::NONE));
            }
            1 => {
                p.add(Shape::convex_polygon(vec![Pos2::new(c.x, r.top() + 1.0), Pos2::new(r.right() - 0.5, r.bottom() - 1.5), Pos2::new(r.left() + 0.5, r.bottom() - 1.5)], YELLOW, Stroke::NONE));
            }
            _ => {
                p.circle_filled(c, rad * 0.85, GREEN);
            }
        },
        IconStyle::SymbolsCircled3 => {
            p.circle_filled(c, rad * 0.9, rag(idx, n));
            symbol(p, Rect::from_center_size(c, Vec2::splat(rad * 1.1)), idx, Color32::WHITE, (rad * 0.26).max(1.2));
        }
        IconStyle::Symbols3 => symbol(p, r.shrink(1.0), idx, rag(idx, n), (rad * 0.3).max(1.4)),
        IconStyle::Flags3 => flag(p, r, rag(idx, n)),
        IconStyle::Stars3 => {
            let sr = Rect::from_center_size(c, Vec2::splat(rad * 1.9));
            match idx {
                0 => star(p, sr, Color32::WHITE, GOLD),
                1 => {
                    star(p, sr, Color32::WHITE, GOLD);
                    let left = Rect::from_min_max(sr.min, Pos2::new(c.x, sr.max.y));
                    star(&p.with_clip_rect(left.intersect(p.clip_rect())), sr, GOLD, GOLD);
                }
                _ => star(p, sr, GOLD, GOLD),
            }
        }
        IconStyle::Rating4 | IconStyle::Rating5 => {
            let filled = if style == IconStyle::Rating4 { idx as usize + 1 } else { idx as usize };
            let w = r.width() / 4.0;
            for k in 0..4 {
                let h = r.height() * (0.25 + 0.25 * k as f32);
                let bar = Rect::from_min_max(Pos2::new(r.left() + k as f32 * w + 0.5, r.bottom() - h), Pos2::new(r.left() + (k + 1) as f32 * w - 0.5, r.bottom()));
                p.rect_filled(bar, 0.0, if k < filled { BLUE } else { EMPTY });
            }
        }
        IconStyle::Quarters5 => {
            p.circle_filled(c, rad * 0.85, Color32::WHITE);
            if idx > 0 {
                pie(p, c, rad * 0.85, idx as f32 / 4.0, BLACK);
            }
            p.circle_stroke(c, rad * 0.85, Stroke::new(1.0, BLACK));
        }
        IconStyle::Boxes5 => {
            let s = r.width() / 2.0;
            for k in 0..4 {
                let (x, y) = ((k % 2) as f32, (k / 2) as f32);
                // Filled in reading order from the bottom-left.
                let order = [2, 3, 0, 1][k];
                let b = Rect::from_min_size(Pos2::new(r.left() + x * s, r.top() + y * s), Vec2::splat(s)).shrink(0.8);
                p.rect_filled(b, 0.0, if order < idx as usize { BLUE } else { EMPTY });
            }
        }
    }
}

/// Paints an icon set icon at the left of a cell, as the conditional format shows it.
pub fn paint_cell_icon(p: &egui::Painter, cell: Rect, d: &IconDraw, zoom: f32) {
    let Some((style, idx)) = d.icon else { return };
    let size = (16.0 * zoom).min(cell.height() - 3.0).min(cell.width() - 2.0);
    if size < 4.0 {
        return;
    }
    let r = Rect::from_min_size(Pos2::new(cell.left() + 2.0 * zoom.max(0.5), cell.center().y - size / 2.0), Vec2::splat(size));
    paint_icon(p, r, style, idx);
}

/// The icons of a set side by side, highest first (as the gallery shows them).
pub fn paint_set(p: &egui::Painter, r: Rect, style: IconStyle) {
    let n = style.count();
    let s = r.height();
    for k in 0..n {
        let ir = Rect::from_min_size(Pos2::new(r.left() + k as f32 * (s + 2.0), r.top()), Vec2::splat(s));
        paint_icon(p, ir, style, (n - 1 - k) as u8);
    }
}

/// An icon choice button for the rule editor; returns the new icon when one was picked.
fn icon_picker(ui: &mut egui::Ui, id: usize, cur: Option<(IconStyle, u8)>) -> Option<Option<(IconStyle, u8)>> {
    let mut out = None;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(46.0, 22.0), Sense::click());
    ui.painter().rect_stroke(rect, 2.0, Stroke::new(1.0, theme::field_edge()), egui::StrokeKind::Inside);
    if let Some((s, i)) = cur {
        paint_icon(ui.painter(), Rect::from_min_size(rect.min + Vec2::new(5.0, 3.0), Vec2::splat(16.0)), s, i);
    }
    super::ribbon::paint_chevron(ui.painter(), Pos2::new(rect.right() - 10.0, rect.center().y), false, theme::text());
    let resp = resp.on_hover_text("Icon");
    egui::Popup::menu(&resp).id(egui::Id::new(("cf_icon_pick", id))).show(|ui| {
        egui::Grid::new(("cf_icon_grid", id)).spacing(Vec2::new(2.0, 2.0)).show(ui, |ui| {
            for style in IconStyle::ALL {
                for k in (0..style.count()).rev() {
                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(r, 2.0, theme::hover());
                    }
                    paint_icon(ui.painter(), r.shrink(3.0), style, k as u8);
                    if resp.clicked() {
                        out = Some(Some((style, k as u8)));
                        ui.close();
                    }
                }
                ui.end_row();
            }
        });
        if ui.button("No Cell Icon").clicked() {
            out = Some(None);
            ui.close();
        }
    });
    out
}

/// The Icon Sets part of the "Format all cells based on their values" rule description.
pub fn icon_rule_ui(ui: &mut egui::Ui, ic: &mut IconSetRule) {
    ui.horizontal(|ui| {
        ui.label("Icon Style:");
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(150.0, 22.0), Sense::click());
        ui.painter().rect_stroke(rect, 2.0, Stroke::new(1.0, theme::field_edge()), egui::StrokeKind::Inside);
        if ic.custom.is_empty() {
            paint_set(ui.painter(), Rect::from_min_size(rect.min + Vec2::new(6.0, 4.0), Vec2::splat(14.0)), ic.style);
        } else {
            ui.painter().text(rect.left_center() + Vec2::new(6.0, 0.0), egui::Align2::LEFT_CENTER, "Custom", theme::ui_font(12.0), theme::text());
        }
        super::ribbon::paint_chevron(ui.painter(), Pos2::new(rect.right() - 10.0, rect.center().y), false, theme::text());
        egui::Popup::menu(&resp.on_hover_text(ic.style.label())).id(egui::Id::new("cf_icon_style")).show(|ui| {
            for style in IconStyle::ALL {
                let (r, resp) = ui.allocate_exact_size(Vec2::new(150.0, 22.0), Sense::click());
                if resp.hovered() || style == ic.style {
                    ui.painter().rect_filled(r, 2.0, theme::hover());
                }
                paint_set(ui.painter(), Rect::from_min_size(r.min + Vec2::new(6.0, 4.0), Vec2::splat(14.0)), style);
                if resp.on_hover_text(style.label()).clicked() {
                    let keep = (ic.reverse, ic.icon_only);
                    *ic = IconSetRule::new(style);
                    (ic.reverse, ic.icon_only) = keep;
                    ui.close();
                }
            }
        });
        if ui.button("Reverse Icon Order").clicked() {
            if ic.custom.is_empty() {
                ic.reverse = !ic.reverse;
            } else {
                ic.custom.reverse();
            }
        }
        ui.checkbox(&mut ic.icon_only, "Show Icon Only");
    });
    ui.add_space(4.0);
    ui.label("Display each icon according to these rules:");
    let n = ic.style.count();
    egui::Grid::new("cf_icon_rules").num_columns(5).spacing(Vec2::new(6.0, 4.0)).show(ui, |ui| {
        ui.label(RichText::new("Icon").strong());
        ui.label("");
        ui.label("");
        ui.label(RichText::new("Value").strong());
        ui.label(RichText::new("Type").strong());
        ui.end_row();
        for pos in (0..n).rev() {
            let cur = ic.icon_at(pos);
            if let Some(new) = icon_picker(ui, pos, cur) {
                if ic.custom.is_empty() {
                    ic.custom = (0..n).map(|k| ic.icon_at(k)).collect();
                    ic.reverse = false;
                }
                ic.custom[pos] = new;
                // Back to the plain set when the picks match it again.
                if (0..n).all(|k| ic.custom[k] == Some((ic.style, k as u8))) {
                    ic.custom.clear();
                }
            }
            if pos == 0 {
                let t = &ic.thresholds[1.min(n - 1)];
                ui.label(format!("when {} {}", if t.gte { "<" } else { "<=" }, t.value));
                ui.end_row();
                continue;
            }
            let t = &mut ic.thresholds[pos];
            ui.label("when value is");
            egui::ComboBox::from_id_salt(("cf_icon_op", pos)).width(40.0).selected_text(if t.gte { ">=" } else { ">" }).show_ui(ui, |ui| {
                ui.selectable_value(&mut t.gte, true, ">=");
                ui.selectable_value(&mut t.gte, false, ">");
            });
            ui.add(egui::TextEdit::singleline(&mut t.value).desired_width(90.0));
            let before = t.kind;
            egui::ComboBox::from_id_salt(("cf_icon_kind", pos)).width(90.0).selected_text(t.kind.label()).show_ui(ui, |ui| {
                for k in ThresholdKind::ALL {
                    ui.selectable_value(&mut t.kind, k, k.label());
                }
            });
            if before != t.kind && matches!(t.kind, ThresholdKind::Percent | ThresholdKind::Percentile) && t.value.parse::<f64>().is_err() {
                *t = Threshold { kind: t.kind, ..Threshold::percent(((pos * 100) / n) as f64) };
            }
            ui.end_row();
        }
    });
}

/// Checks the thresholds the way Excel does before accepting the rule.
pub fn check_icon_rule(ic: &IconSetRule) -> Result<(), String> {
    for t in ic.thresholds.iter().skip(1) {
        let v = t.value.trim().trim_start_matches('=');
        match t.kind {
            ThresholdKind::Percent | ThresholdKind::Percentile => match v.parse::<f64>() {
                Ok(x) if (0.0..=100.0).contains(&x) => {}
                _ => return Err(format!("The value '{}' is not a valid {}. Enter a number between 0 and 100.", t.value, t.kind.label().to_lowercase())),
            },
            _ => {
                if v.is_empty() || crate::formula::parse(v).is_err() {
                    return Err(format!("The value '{}' is not a valid number, date, time or formula.", t.value));
                }
            }
        }
    }
    Ok(())
}

impl App {
    /// Home > Conditional Formatting > Icon Sets gallery.
    pub(super) fn cf_icon_menu(&mut self, ui: &mut egui::Ui) {
        for (title, styles) in IconStyle::groups() {
            ui.label(RichText::new(title).size(11.0).color(theme::text_dim()));
            egui::Grid::new(("cf_icon_group", title)).spacing(Vec2::new(4.0, 2.0)).show(ui, |ui| {
                for (i, style) in styles.iter().enumerate() {
                    let w = style.count() as f32 * 18.0 + 10.0;
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 24.0), Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(r, 2.0, theme::hover());
                    }
                    paint_set(ui.painter(), Rect::from_min_size(r.min + Vec2::new(5.0, 4.0), Vec2::splat(16.0)), *style);
                    if resp.on_hover_text(style.label()).clicked() {
                        self.cf_visual(RuleKind::IconSet(IconSetRule::new(*style)));
                        ui.close();
                    }
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
        }
        ui.separator();
        if ui.button("More Rules...").clicked() {
            self.cf_open_icon_editor();
            ui.close();
        }
    }
}
