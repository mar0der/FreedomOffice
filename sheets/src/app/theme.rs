//! Colours, fonts and icon glyphs for the familiar desktop-spreadsheet look.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke};

pub const ACCENT: Color32 = Color32::from_rgb(0x21, 0x73, 0x46);
pub const ACCENT_DARK: Color32 = Color32::from_rgb(0x18, 0x5C, 0x37);
pub const ACCENT_LIGHT: Color32 = Color32::from_rgb(0xD5, 0xEA, 0xDD);
pub const CHROME: Color32 = Color32::from_rgb(0xF3, 0xF3, 0xF3);
pub const RIBBON: Color32 = Color32::from_rgb(0xFA, 0xFA, 0xFA);
pub const BORDER: Color32 = Color32::from_rgb(0xD4, 0xD4, 0xD4);
pub const HEADER_BG: Color32 = Color32::from_rgb(0xF5, 0xF5, 0xF5);
pub const HEADER_SEL_BG: Color32 = Color32::from_rgb(0xDE, 0xDE, 0xDE);
pub const HEADER_FULL_BG: Color32 = Color32::from_rgb(0xC9, 0xE3, 0xD2);
pub const HEADER_TEXT: Color32 = Color32::from_rgb(0x44, 0x44, 0x44);
pub const HEADER_LINE: Color32 = Color32::from_rgb(0xD0, 0xD0, 0xD0);
pub const GRID_LINE: Color32 = Color32::from_rgb(0xDD, 0xDD, 0xDD);
pub const TEXT: Color32 = Color32::from_rgb(0x20, 0x20, 0x20);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x60, 0x60, 0x60);
pub const HOVER: Color32 = Color32::from_rgb(0xE6, 0xE6, 0xE6);
pub const PRESSED: Color32 = Color32::from_rgb(0xD0, 0xD0, 0xD0);
pub const CHECKED: Color32 = Color32::from_rgb(0xC8, 0xD8, 0xCF);

pub fn sel_fill() -> Color32 {
    Color32::from_rgba_unmultiplied(0x10, 0x10, 0x10, 22)
}

/// Colours used to highlight references while editing formulas.
pub const REF_COLORS: [Color32; 8] = [
    Color32::from_rgb(0x41, 0x6F, 0xE6),
    Color32::from_rgb(0xD6, 0x3E, 0x3E),
    Color32::from_rgb(0x8E, 0x4E, 0xC6),
    Color32::from_rgb(0x2E, 0x8B, 0x57),
    Color32::from_rgb(0xC0, 0x6B, 0x16),
    Color32::from_rgb(0x1C, 0x8C, 0xA8),
    Color32::from_rgb(0xB0, 0x3A, 0x8C),
    Color32::from_rgb(0x6B, 0x8E, 0x23),
];

pub fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// Theme palette shown in colour pickers (top row + tints), like the familiar Office grid.
pub const PALETTE_BASE: [u32; 10] =
    [0xFFFFFF, 0x000000, 0xE7E6E6, 0x44546A, 0x4472C4, 0xED7D31, 0xA5A5A5, 0xFFC000, 0x5B9BD5, 0x70AD47];
pub const PALETTE_STANDARD: [u32; 10] =
    [0xC00000, 0xFF0000, 0xFFC000, 0xFFFF00, 0x92D050, 0x00B050, 0x00B0F0, 0x0070C0, 0x002060, 0x7030A0];

pub fn tint(c: u32, t: f32) -> u32 {
    let ch = |v: u32| -> u32 {
        let v = v as f32;
        let out = if t >= 0.0 { v + (255.0 - v) * t } else { v * (1.0 + t) };
        out.round().clamp(0.0, 255.0) as u32
    };
    (ch((c >> 16) & 255) << 16) | (ch((c >> 8) & 255) << 8) | ch(c & 255)
}

// ---------------------------------------------------------------------------
// Icons (Segoe Fluent Icons / Segoe MDL2 Assets ship with Windows 10/11)
// ---------------------------------------------------------------------------

pub struct Icons {
    pub available: bool,
}

pub const ICON_FAMILY: &str = "icons";

impl Icons {
    /// Returns (glyph, is_icon_font).
    pub fn get(&self, code: char, fallback: &'static str) -> (String, bool) {
        if self.available { (code.to_string(), true) } else { (fallback.to_string(), false) }
    }
}

pub mod ic {
    pub const PASTE: char = '\u{E77F}';
    pub const CUT: char = '\u{E8C6}';
    pub const COPY: char = '\u{E8C8}';
    pub const BRUSH: char = '\u{E771}';
    pub const BOLD: char = '\u{E8DD}';
    pub const ITALIC: char = '\u{E8DB}';
    pub const UNDERLINE: char = '\u{E8DC}';
    pub const FONT_COLOR: char = '\u{E8D3}';
    pub const FILL: char = '\u{E7E6}';
    pub const FONT_UP: char = '\u{E8E8}';
    pub const FONT_DOWN: char = '\u{E8E7}';
    pub const ALIGN_LEFT: char = '\u{E8E4}';
    pub const ALIGN_CENTER: char = '\u{E8E3}';
    pub const ALIGN_RIGHT: char = '\u{E8E2}';
    pub const UNDO: char = '\u{E7A7}';
    pub const REDO: char = '\u{E7A6}';
    pub const SAVE: char = '\u{E74E}';
    pub const SAVE_AS: char = '\u{E792}';
    pub const OPEN: char = '\u{E8E5}';
    pub const FOLDER: char = '\u{E8B7}';
    pub const NEW: char = '\u{E8A5}';
    pub const ADD: char = '\u{E710}';
    pub const DELETE: char = '\u{E74D}';
    pub const FIND: char = '\u{E721}';
    pub const SORT: char = '\u{E8CB}';
    pub const FILTER: char = '\u{E71C}';
    pub const CALC: char = '\u{E8EF}';
    pub const DOWN: char = '\u{E70D}';
    pub const LEFT: char = '\u{E76B}';
    pub const RIGHT: char = '\u{E76C}';
    pub const BACK: char = '\u{E72B}';
    pub const ZOOM: char = '\u{E71E}';
    pub const PIN: char = '\u{E718}';
    pub const CLEAR: char = '\u{E894}';
    pub const INFO: char = '\u{E946}';
    pub const CLOSE: char = '\u{E8BB}';
    pub const ACCEPT: char = '\u{E8FB}';
    pub const TABLE: char = '\u{E80A}';
    pub const SETTINGS: char = '\u{E713}';
    pub const HIDE: char = '\u{ED1A}';
    pub const VIEW: char = '\u{E890}';
    pub const EXIT: char = '\u{E7E8}';
}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

fn font_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(w) = std::env::var("WINDIR") {
        v.push(PathBuf::from(w).join("Fonts"));
    } else {
        v.push(PathBuf::from("C:\\Windows\\Fonts"));
    }
    if let Ok(l) = std::env::var("LOCALAPPDATA") {
        v.push(PathBuf::from(l).join("Microsoft").join("Windows").join("Fonts"));
    }
    v.push(PathBuf::from("/usr/share/fonts/truetype/msttcorefonts"));
    v
}

/// Known font files: [regular, bold, italic, bold italic].
const FONT_FILES: &[(&str, [&str; 4])] = &[
    ("calibri", ["calibri.ttf", "calibrib.ttf", "calibrii.ttf", "calibriz.ttf"]),
    ("aptos", ["Aptos.ttf", "Aptos-Bold.ttf", "Aptos-Italic.ttf", "Aptos-Bold-Italic.ttf"]),
    ("aptos narrow", ["Aptos-Narrow.ttf", "Aptos-Narrow-Bold.ttf", "Aptos-Narrow-Italic.ttf", "Aptos-Narrow-Bold-Italic.ttf"]),
    ("arial", ["arial.ttf", "arialbd.ttf", "ariali.ttf", "arialbi.ttf"]),
    ("arial narrow", ["ARIALN.TTF", "ARIALNB.TTF", "ARIALNI.TTF", "ARIALNBI.TTF"]),
    ("segoe ui", ["segoeui.ttf", "segoeuib.ttf", "segoeuii.ttf", "segoeuiz.ttf"]),
    ("times new roman", ["times.ttf", "timesbd.ttf", "timesi.ttf", "timesbi.ttf"]),
    ("consolas", ["consola.ttf", "consolab.ttf", "consolai.ttf", "consolaz.ttf"]),
    ("courier new", ["cour.ttf", "courbd.ttf", "couri.ttf", "courbi.ttf"]),
    ("verdana", ["verdana.ttf", "verdanab.ttf", "verdanai.ttf", "verdanaz.ttf"]),
    ("tahoma", ["tahoma.ttf", "tahomabd.ttf", "tahoma.ttf", "tahomabd.ttf"]),
    ("georgia", ["georgia.ttf", "georgiab.ttf", "georgiai.ttf", "georgiaz.ttf"]),
    ("cambria", ["cambria.ttc", "cambriab.ttf", "cambriai.ttf", "cambriaz.ttf"]),
    ("candara", ["Candara.ttf", "Candarab.ttf", "Candarai.ttf", "Candaraz.ttf"]),
    ("trebuchet ms", ["trebuc.ttf", "trebucbd.ttf", "trebucit.ttf", "trebucbi.ttf"]),
    ("garamond", ["GARA.TTF", "GARABD.TTF", "GARAIT.TTF", "GARABD.TTF"]),
    ("century gothic", ["GOTHIC.TTF", "GOTHICB.TTF", "GOTHICI.TTF", "GOTHICBI.TTF"]),
    ("comic sans ms", ["comic.ttf", "comicbd.ttf", "comici.ttf", "comicz.ttf"]),
    ("impact", ["impact.ttf", "impact.ttf", "impact.ttf", "impact.ttf"]),
];

/// Fonts offered in the font picker.
pub const FONT_CHOICES: &[&str] = &[
    "Aptos", "Aptos Narrow", "Arial", "Arial Narrow", "Calibri", "Cambria", "Candara", "Century Gothic", "Comic Sans MS",
    "Consolas", "Courier New", "Garamond", "Georgia", "Impact", "Segoe UI", "Tahoma", "Times New Roman", "Trebuchet MS",
    "Verdana",
];

pub const DEFAULT_CELL_FONT: &str = "Calibri";

fn read_font(file: &str) -> Option<Vec<u8>> {
    for d in font_dirs() {
        let p = d.join(file);
        if let Ok(b) = std::fs::read(&p) {
            return Some(b);
        }
        // case-insensitive fallback
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().eq_ignore_ascii_case(file) {
                    if let Ok(b) = std::fs::read(e.path()) {
                        return Some(b);
                    }
                }
            }
        }
    }
    None
}

pub struct FontManager {
    defs: FontDefinitions,
    loaded: HashSet<String>,
    failed: HashSet<String>,
    pending: Vec<(String, String, bool, bool)>,
    cache: HashMap<String, FontFamily>,
    /// Families already active in egui (fonts set via `set_fonts` apply from the next pass).
    usable: HashSet<String>,
    staged: Vec<String>,
    pub icons: Icons,
    ui_fallbacks: Vec<String>,
}

impl FontManager {
    pub fn new(ctx: &egui::Context) -> Self {
        let mut defs = FontDefinitions::default();
        let mut ui_fallbacks = Vec::new();
        let mut add = |defs: &mut FontDefinitions, key: &str, file: &str| -> bool {
            match read_font(file) {
                Some(b) => {
                    defs.font_data.insert(key.to_string(), Arc::new(FontData::from_owned(b)));
                    true
                }
                None => false,
            }
        };
        let mut ui_chain = Vec::new();
        if add(&mut defs, "ui", "segoeui.ttf") {
            ui_chain.push("ui".to_string());
        }
        if add(&mut defs, "ui-symbol", "seguisym.ttf") {
            ui_fallbacks.push("ui-symbol".to_string());
        }
        if add(&mut defs, "ui-emoji", "seguiemj.ttf") {
            ui_fallbacks.push("ui-emoji".to_string());
        }
        let icons = add(&mut defs, ICON_FAMILY, "SegoeIcons.ttf") || add(&mut defs, ICON_FAMILY, "segmdl2.ttf");
        if add(&mut defs, "ui-bold", "segoeuib.ttf") {
            let mut fam = vec!["ui-bold".to_string()];
            fam.extend(ui_fallbacks.iter().cloned());
            defs.families.insert(FontFamily::Name("ui-bold".into()), fam);
        }
        {
            let prop = defs.families.entry(FontFamily::Proportional).or_default();
            for (i, k) in ui_chain.iter().enumerate() {
                prop.insert(i, k.clone());
            }
            for k in &ui_fallbacks {
                prop.push(k.clone());
            }
        }
        if !defs.families.contains_key(&FontFamily::Name("ui-bold".into())) {
            let prop = defs.families[&FontFamily::Proportional].clone();
            defs.families.insert(FontFamily::Name("ui-bold".into()), prop);
        }
        if icons {
            defs.families.insert(FontFamily::Name(ICON_FAMILY.into()), vec![ICON_FAMILY.to_string()]);
        } else {
            let prop = defs.families[&FontFamily::Proportional].clone();
            defs.families.insert(FontFamily::Name(ICON_FAMILY.into()), prop);
        }
        let mut fm = FontManager {
            defs,
            loaded: HashSet::new(),
            failed: HashSet::new(),
            pending: Vec::new(),
            cache: HashMap::new(),
            usable: HashSet::new(),
            staged: Vec::new(),
            icons: Icons { available: icons },
            ui_fallbacks,
        };
        // Preload the default cell font so the first frame looks right.
        for (b, i) in [(false, false), (true, false), (false, true), (true, true)] {
            fm.load_now(DEFAULT_CELL_FONT, b, i);
        }
        ctx.set_fonts(fm.defs.clone());
        fm.usable = fm.loaded.clone();
        fm
    }

    fn key(name: &str, bold: bool, italic: bool) -> String {
        format!("cell:{}:{}{}", name.to_lowercase(), bold as u8, italic as u8)
    }

    fn load_now(&mut self, name: &str, bold: bool, italic: bool) -> bool {
        let key = Self::key(name, bold, italic);
        if self.loaded.contains(&key) {
            return true;
        }
        if self.failed.contains(&key) {
            return false;
        }
        let lname = name.to_lowercase();
        let idx = (bold as usize) | ((italic as usize) << 1);
        let idx = [0, 1, 2, 3][idx];
        let files: Vec<String> = match FONT_FILES.iter().find(|(n, _)| *n == lname) {
            Some((_, f)) => {
                let order = match idx {
                    0 => [0, 0, 0],
                    1 => [1, 0, 0],
                    2 => [2, 0, 0],
                    _ => [3, 1, 0],
                };
                order.iter().map(|&i| f[i].to_string()).collect()
            }
            None => {
                let base = lname.replace(' ', "");
                vec![format!("{base}.ttf"), format!("{name}.ttf")]
            }
        };
        let mut data = None;
        for f in &files {
            if let Some(b) = read_font(f) {
                data = Some(b);
                break;
            }
        }
        let Some(bytes) = data else {
            self.failed.insert(key);
            return false;
        };
        self.defs.font_data.insert(key.clone(), Arc::new(FontData::from_owned(bytes)));
        let mut chain = vec![key.clone()];
        chain.push("ui".to_string());
        chain.extend(self.ui_fallbacks.iter().cloned());
        chain.retain(|k| self.defs.font_data.contains_key(k));
        // egui's bundled fonts as last resort
        for k in self.defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default() {
            if !chain.contains(&k) {
                chain.push(k);
            }
        }
        self.defs.families.insert(FontFamily::Name(key.clone().into()), chain);
        self.loaded.insert(key);
        true
    }

    /// Font family for cell text; loads fonts lazily (applied next frame).
    pub fn cell_family(&mut self, name: Option<&str>, bold: bool, italic: bool) -> FontFamily {
        let name = name.unwrap_or(DEFAULT_CELL_FONT);
        let key = Self::key(name, bold, italic);
        if let Some(f) = self.cache.get(&key) {
            return f.clone();
        }
        if self.usable.contains(&key) {
            let f = FontFamily::Name(key.clone().into());
            self.cache.insert(key, f.clone());
            return f;
        }
        if self.loaded.contains(&key) {
            // loaded this frame, active from the next one
        } else if !self.failed.contains(&key) {
            if !self.pending.iter().any(|p| p.0 == key) {
                self.pending.push((key.clone(), name.to_string(), bold, italic));
            }
        } else if !name.eq_ignore_ascii_case(DEFAULT_CELL_FONT) {
            // Remember the fallback for this unavailable font.
            let f = self.cell_family(Some(DEFAULT_CELL_FONT), bold, italic);
            self.cache.insert(key, f.clone());
            return f;
        }
        let dk = Self::key(DEFAULT_CELL_FONT, bold, italic);
        if self.usable.contains(&dk) { FontFamily::Name(dk.into()) } else { FontFamily::Proportional }
    }

    /// Applies lazily loaded fonts. Call once per frame before drawing.
    pub fn flush(&mut self, ctx: &egui::Context) {
        if !self.staged.is_empty() {
            self.usable.extend(self.staged.drain(..));
            self.cache.clear();
        }
        if self.pending.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending);
        let mut changed = false;
        for (key, name, b, i) in pending {
            if self.load_now(&name, b, i) {
                self.staged.push(key);
                changed = true;
            }
        }
        self.cache.clear();
        if changed {
            ctx.set_fonts(self.defs.clone());
            ctx.request_repaint();
        }
    }
}

pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICON_FAMILY.into()))
}

pub fn ui_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn ui_bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("ui-bold".into()))
}

/// Flat, light visuals similar to desktop office ribbons.
pub fn apply_style(ctx: &egui::Context) {
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    let mut style = (*ctx.style()).clone();
    let mut v = egui::Visuals::light();
    v.window_fill = Color32::WHITE;
    v.panel_fill = CHROME;
    v.extreme_bg_color = Color32::WHITE;
    v.faint_bg_color = Color32::from_rgb(0xF7, 0xF7, 0xF7);
    v.selection.bg_fill = ACCENT_LIGHT;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.window_corner_radius = egui::CornerRadius::same(6);
    v.menu_corner_radius = egui::CornerRadius::same(4);
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_shadow = egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(40) };
    v.popup_shadow = egui::Shadow { offset: [0, 3], blur: 10, spread: 0, color: Color32::from_black_alpha(35) };
    let w = &mut v.widgets;
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.inactive.bg_fill = Color32::WHITE;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = egui::CornerRadius::same(3);
    w.hovered.weak_bg_fill = HOVER;
    w.hovered.bg_fill = HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0xC8, 0xC8, 0xC8));
    w.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    w.hovered.corner_radius = egui::CornerRadius::same(3);
    w.hovered.expansion = 0.0;
    w.active.weak_bg_fill = PRESSED;
    w.active.bg_fill = PRESSED;
    w.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0xB0, 0xB0, 0xB0));
    w.active.fg_stroke = Stroke::new(1.0, TEXT);
    w.active.corner_radius = egui::CornerRadius::same(3);
    w.active.expansion = 0.0;
    w.open.weak_bg_fill = PRESSED;
    w.open.bg_fill = PRESSED;
    w.open.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals = v;
    style.spacing.item_spacing = egui::vec2(4.0, 3.0);
    style.spacing.button_padding = egui::vec2(5.0, 2.0);
    style.spacing.interact_size.y = 20.0;
    style.spacing.menu_margin = egui::Margin::same(4);
    style.text_styles = [
        (egui::TextStyle::Small, ui_font(11.0)),
        (egui::TextStyle::Body, ui_font(12.5)),
        (egui::TextStyle::Button, ui_font(12.5)),
        (egui::TextStyle::Heading, ui_font(18.0)),
        (egui::TextStyle::Monospace, FontId::monospace(12.0)),
    ]
    .into();
    ctx.set_style(style);
}
