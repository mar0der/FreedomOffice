//! Pictures over the grid: painting, selection, moving and resizing.

use std::collections::HashMap;

use eframe::egui::{self, Color32, CursorIcon, Event, Key, Pos2, Rect, Stroke, StrokeKind, Vec2};

use super::grid::Layout;
use super::{App, Drag, Scope};
use crate::chart::{col_at_x, col_x, row_at_y, row_y};
use crate::model::{Picture, Sheet};

const HANDLE: Color32 = Color32::from_rgb(0x70, 0x70, 0x70);
const MIN: f32 = 8.0;

#[derive(Default)]
pub struct PictureUi {
    /// Selected picture: (sheet index, picture index).
    sel: Option<(usize, usize)>,
    drag: Option<PicDrag>,
    /// Decoded textures keyed by the image data's address (None = could not be decoded).
    textures: HashMap<usize, Option<egui::TextureHandle>>,
}

#[derive(Clone, Copy)]
struct PicDrag {
    idx: usize,
    /// None = move, Some(k) = resize handle k (clockwise from top-left).
    handle: Option<usize>,
    start: Pos2,
    orig: (f32, f32, f32, f32),
    moved: bool,
}

fn handles(r: Rect) -> [Pos2; 8] {
    [r.left_top(), r.center_top(), r.right_top(), r.right_center(), r.right_bottom(), r.center_bottom(), r.left_bottom(), r.left_center()]
}

fn cursor_for(handle: Option<usize>) -> CursorIcon {
    match handle {
        None => CursorIcon::Move,
        Some(0) | Some(4) => CursorIcon::ResizeNwSe,
        Some(2) | Some(6) => CursorIcon::ResizeNeSw,
        Some(1) | Some(5) => CursorIcon::ResizeVertical,
        Some(_) => CursorIcon::ResizeHorizontal,
    }
}

/// Picture rectangle in sheet pixels (unzoomed): (x, y, w, h).
fn sheet_rect(sh: &Sheet, p: &Picture) -> (f32, f32, f32, f32) {
    (col_x(sh, p.anchor.1) + p.offset.0, row_y(sh, p.anchor.0) + p.offset.1, p.size.0, p.size.1)
}

fn set_sheet_rect(sh: &Sheet, p: &mut Picture, x: f32, y: f32, w: f32, h: f32) {
    let (c, ox) = col_at_x(sh, x.max(0.0));
    let (r, oy) = row_at_y(sh, y.max(0.0));
    p.anchor = (r, c);
    p.offset = (ox, oy);
    p.size = (w.max(MIN), h.max(MIN));
}

fn decode(data: &[u8]) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(data).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()))
}

impl App {
    pub fn selected_picture(&self) -> Option<usize> {
        let (s, i) = self.pictures.sel?;
        (s == self.wb.active && i < self.sheet().images.len()).then_some(i)
    }

    fn picture_screen_rect(&self, lay: &Layout, p: &Picture) -> (Rect, Rect) {
        let sh = self.sheet();
        let z = self.zoom;
        let (fr, fc) = sh.freeze;
        let (x, y, w, h) = sheet_rect(sh, p);
        let (sx, cl) = if p.anchor.1 >= fc {
            (lay.frozen_x + (x - col_x(sh, sh.view.left.max(fc))) * z, lay.frozen_x)
        } else {
            (lay.cells.left() + x * z, lay.cells.left())
        };
        let (sy, ct) = if p.anchor.0 >= fr {
            (lay.frozen_y + (y - row_y(sh, sh.view.top.max(fr))) * z, lay.frozen_y)
        } else {
            (lay.cells.top() + y * z, lay.cells.top())
        };
        (Rect::from_min_size(Pos2::new(sx, sy), Vec2::new(w * z, h * z)), Rect::from_min_max(Pos2::new(cl, ct), lay.cells.max))
    }

    fn picture_hit(&self, lay: &Layout, pos: Pos2) -> Option<(usize, Option<usize>)> {
        if let Some(i) = self.selected_picture() {
            let (r, clip) = self.picture_screen_rect(lay, &self.sheet().images[i]);
            if clip.contains(pos) {
                if let Some(k) = handles(r).iter().position(|p| (*p - pos).length() <= 6.0) {
                    return Some((i, Some(k)));
                }
            }
        }
        let pics = &self.sheet().images;
        (0..pics.len()).rev().find_map(|i| {
            let (r, clip) = self.picture_screen_rect(lay, &pics[i]);
            (clip.contains(pos) && r.contains(pos)).then_some((i, None))
        })
    }

    /// Mouse over the grid; returns true when a picture took the event.
    pub(super) fn picture_pointer(&mut self, ui: &egui::Ui, lay: &Layout, pos: Pos2, hovered: bool, pressed: bool, down: bool) -> bool {
        if let Some(mut d) = self.pictures.drag {
            if down {
                if !d.moved && (pos - d.start).length() > 3.0 {
                    d.moved = true;
                    self.push_undo(&[Scope::Sheet(self.wb.active)]);
                }
                if d.moved {
                    self.apply_picture_drag(&d, (pos - d.start) / self.zoom, ui.input(|i| i.modifiers.shift));
                    self.dirty = true;
                }
                ui.ctx().set_cursor_icon(cursor_for(d.handle));
                self.pictures.drag = Some(d);
            } else {
                self.pictures.drag = None;
            }
            return true;
        }
        if !hovered || self.drag != Drag::None || !lay.cells.contains(pos) {
            return false;
        }
        match self.picture_hit(lay, pos) {
            Some((i, handle)) => {
                ui.ctx().set_cursor_icon(cursor_for(handle));
                if pressed {
                    if self.edit.is_some() && !self.commit_edit(false) {
                        return true;
                    }
                    self.filter_popup = None;
                    self.pictures.sel = Some((self.wb.active, i));
                    let orig = sheet_rect(self.sheet(), &self.sheet().images[i]);
                    self.pictures.drag = Some(PicDrag { idx: i, handle, start: pos, orig, moved: false });
                }
                true
            }
            None => {
                if pressed {
                    self.pictures.sel = None;
                }
                false
            }
        }
    }

    /// Corner handles keep the aspect ratio while Shift is held.
    fn apply_picture_drag(&mut self, d: &PicDrag, delta: Vec2, keep_ratio: bool) {
        let (x, y, w, h) = d.orig;
        let (mut l, mut t, mut r, mut b) = (x, y, x + w, y + h);
        match d.handle {
            None => {
                let dx = delta.x.max(-l);
                let dy = delta.y.max(-t);
                l += dx;
                r += dx;
                t += dy;
                b += dy;
            }
            Some(k) => {
                if matches!(k, 0 | 6 | 7) {
                    l = (l + delta.x).min(r - MIN).max(0.0);
                }
                if matches!(k, 2..=4) {
                    r = (r + delta.x).max(l + MIN);
                }
                if matches!(k, 0..=2) {
                    t = (t + delta.y).min(b - MIN).max(0.0);
                }
                if matches!(k, 4..=6) {
                    b = (b + delta.y).max(t + MIN);
                }
                if keep_ratio && matches!(k, 0 | 2 | 4 | 6) && w > 0.0 && h > 0.0 {
                    let nh = (r - l) * h / w;
                    if matches!(k, 0 | 2) {
                        t = b - nh;
                    } else {
                        b = t + nh;
                    }
                }
            }
        }
        let si = self.wb.active;
        let Some(mut p) = self.wb.sheets[si].images.get(d.idx).cloned() else { return };
        set_sheet_rect(&self.wb.sheets[si], &mut p, l, t, r - l, b - t);
        self.wb.sheets[si].images[d.idx] = p;
    }

    pub fn delete_picture(&mut self) {
        let Some(i) = self.selected_picture() else { return };
        self.push_undo(&[Scope::Sheet(self.wb.active)]);
        self.sheet_mut().images.remove(i);
        self.pictures.sel = None;
        self.changed();
    }

    /// Keyboard while a picture is selected; returns true when the keys were consumed.
    pub(super) fn picture_keys(&mut self, ctx: &egui::Context) -> bool {
        if self.selected_picture().is_none() || self.edit.is_some() {
            return false;
        }
        for ev in ctx.input(|i| i.events.clone()) {
            let Event::Key { key, pressed: true, modifiers: m, .. } = ev else { continue };
            let ctrl = m.ctrl || m.command;
            match key {
                Key::Delete | Key::Backspace => self.delete_picture(),
                Key::Escape => self.pictures.sel = None,
                Key::Z if ctrl => self.undo(),
                Key::Y if ctrl => self.redo(),
                Key::S if ctrl => {
                    self.save();
                }
                _ => continue,
            }
            break;
        }
        true
    }

    pub(super) fn paint_pictures(&mut self, ui: &egui::Ui, lay: &Layout) {
        let n = self.sheet().images.len();
        if n == 0 {
            return;
        }
        if self.selected_chart().is_some() {
            self.pictures.sel = None;
        }
        let sel = self.selected_picture();
        for i in 0..n {
            let p = self.sheet().images[i].clone();
            let (rect, clip) = self.picture_screen_rect(lay, &p);
            if !clip.intersects(rect.expand(6.0)) {
                continue;
            }
            let key = std::sync::Arc::as_ptr(&p.data) as usize;
            let tex = self
                .pictures
                .textures
                .entry(key)
                .or_insert_with(|| decode(&p.data).map(|img| ui.ctx().load_texture(format!("picture-{key}"), img, egui::TextureOptions::LINEAR)))
                .clone();
            let painter = ui.painter().with_clip_rect(clip.intersect(rect));
            match tex {
                Some(t) => {
                    painter.image(t.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                }
                None => {
                    painter.rect_filled(rect, 0.0, Color32::from_gray(235));
                    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::from_gray(160)), StrokeKind::Inside);
                    let label = if p.descr.is_empty() { format!("Picture ({})", p.ext) } else { p.descr.clone() };
                    painter.text(rect.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(12.0), Color32::from_gray(90));
                }
            }
            if sel == Some(i) {
                let ph = ui.painter().with_clip_rect(clip.intersect(rect.expand(6.0)));
                ph.rect_stroke(rect, 0.0, Stroke::new(1.0, HANDLE), StrokeKind::Middle);
                for h in handles(rect) {
                    ph.circle_filled(h, 4.0, Color32::WHITE);
                    ph.circle_stroke(h, 4.0, Stroke::new(1.0, HANDLE));
                }
            }
        }
        // Forget textures of pictures that are gone.
        if self.pictures.textures.len() > 64 {
            let live: std::collections::HashSet<usize> =
                self.wb.sheets.iter().flat_map(|s| s.images.iter().map(|p| std::sync::Arc::as_ptr(&p.data) as usize)).collect();
            self.pictures.textures.retain(|k, _| live.contains(k));
        }
    }
}
