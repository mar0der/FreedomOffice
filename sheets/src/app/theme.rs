//! Colours, fonts and icon glyphs for the familiar desktop-spreadsheet look.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke};

// ---------------------------------------------------------------------------
// Light / dark palette. Like desktop Office's default "Use system setting", the
// chrome follows the Windows app mode; the cell area stays white in both.
// ---------------------------------------------------------------------------

static DARK: AtomicBool = AtomicBool::new(false);

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

pub fn set_dark(dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
}

/// Windows app mode, unless FREEDOMOFFICE_THEME=light|dark overrides it (for testing).
pub fn system_dark() -> bool {
    match std::env::var("FREEDOMOFFICE_THEME").as_deref() {
        Ok("dark") => true,
        Ok("light") => false,
        _ => os_dark(),
    }
}

/// Windows Settings > Personalization > Colors > "Choose your default app mode".
#[cfg(windows)]
fn os_dark() -> bool {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let key: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0".encode_utf16().collect();
    let val: Vec<u16> = "AppsUseLightTheme\0".encode_utf16().collect();
    let mut data: u32 = 1;
    let mut size = 4u32;
    let rc = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), &mut data as *mut u32 as *mut _, &mut size)
    };
    rc == 0 && data == 0
}

#[cfg(not(windows))]
fn os_dark() -> bool {
    false
}

macro_rules! palette {
    ($($(#[$m:meta])* $name:ident = $light:literal, $dark:literal;)*) => {
        $($(#[$m])* pub fn $name() -> Color32 {
            rgb(if is_dark() { $dark } else { $light })
        })*
    };
}

palette! {
    /// Accent for chrome (tab underline, links, focused outlines).
    accent = 0x217346, 0x60BD82;
    /// Accent used for text and strong fills.
    accent_dark = 0x185C37, 0x6FCB91;
    /// Soft accent for selected list items and default buttons.
    accent_light = 0xD5EADD, 0x2B4F3A;
    /// Window background behind the ribbon, formula bar, sheet tabs and status bar.
    chrome = 0xF3F3F3, 0x121A1E;
    /// Ribbon body.
    ribbon = 0xFAFAFA, 0x292929;
    /// Popups, dialogs, backstage pages and input fields.
    window = 0xFFFFFF, 0x292929;
    border = 0xD4D4D4, 0x5C5C5C;
    header_bg = 0xF5F5F5, 0x2B2B2B;
    header_sel_bg = 0xDEDEDE, 0x424242;
    header_full_bg = 0xC9E3D2, 0x2C5A3E;
    header_text = 0x444444, 0xD0D0D0;
    header_line = 0xD0D0D0, 0x474747;
    text = 0x202020, 0xFFFFFF;
    text_dim = 0x606060, 0xADADAD;
    hover = 0xE6E6E6, 0x3D3D3D;
    pressed = 0xD0D0D0, 0x4F4F4F;
    checked = 0xC8D8CF, 0x4A4A4A;
    /// Outline of hovered or pressed ribbon buttons.
    button_edge = 0xC8C8C8, 0x8A8A8A;
    /// Outline of the name box and formula field.
    field_edge = 0xD4D4D4, 0x666666;
    /// Vertical line between ribbon groups.
    separator = 0xD4D4D4, 0x666666;
    /// Frame around the cells of a ribbon gallery (Cell Styles).
    gallery_edge = 0xC6C6C6, 0xADADAD;
    /// File backstage navigation pane.
    backstage_nav = 0x185C37, 0x1F3D2B;
    backstage_sel = 0x217346, 0x2F5E42;
    backstage_hover = 0x206B45, 0x2A4F3A;
    /// Formula autocomplete / argument tooltip.
    tooltip = 0xFFFFF4, 0x333333;
    /// Validation and parse errors shown in dialogs.
    error_text = 0xC00000, 0xFF7B7B;
    /// Inset panel inside a dialog (rule description box).
    inset = 0xF8F8F8, 0x262626;
    /// Outlines of small painted ribbon icons.
    icon_line = 0x787878, 0xADADAD;
    /// Scrollbar thumb, idle and dragged.
    scroll_thumb = 0xB4B4B4, 0x6B6B6B;
    scroll_thumb_active = 0x787878, 0x9E9E9E;
    /// Gaps beside the headers at the scrollbars (white sheet in light mode, window colour in dark).
    grid_gutter = 0xFFFFFF, 0x121A1E;
    /// Select-all triangle in the header corner.
    select_all = 0xB9B9B9, 0x6E6E6E;
    /// KeyTips help panel and its key badges.
    keytip_panel = 0xFBFBFB, 0x292929;
    keytip_key = 0xFFF4C2, 0x5C4E1E;
    /// Background around the pages in the print preview.
    preview_bg = 0xE6E6E6, 0x1F1F1F;
}

/// Accent drawn on the (always white) cell area: selection border, fill handle, copy marquee.
pub const GRID_ACCENT: Color32 = Color32::from_rgb(0x21, 0x73, 0x46);
pub const GRID_LINE: Color32 = Color32::from_rgb(0xDD, 0xDD, 0xDD);
/// Text drawn on white previews of paper or cells (stays dark in dark mode).
pub const PAPER_TEXT: Color32 = Color32::from_rgb(0x20, 0x20, 0x20);

pub fn grid_line() -> Color32 {
    GRID_LINE
}

pub fn sel_fill() -> Color32 {
    Color32::from_rgba_unmultiplied(0x10, 0x10, 0x10, 22)
}

/// Grey wash over the cells a fill-handle drag back into the selection will clear
/// (drawn on the always-white cell area).
pub fn fill_shrink() -> Color32 {
    Color32::from_rgba_unmultiplied(0x60, 0x60, 0x60, 90)
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

/// Resolves a fontconfig pattern (e.g. "Calibri:bold") to font bytes and face index.
/// fontconfig applies its own metric-compatible aliases (Calibri -> Carlito, Arial ->
/// Liberation Sans, ...) and always answers with its best match.
#[cfg(not(windows))]
fn fc_match(pattern: &str) -> Option<(Vec<u8>, u32)> {
    let out = std::process::Command::new("fc-match").args(["-f", "%{file}|%{index}", pattern]).output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    let (file, index) = s.trim().rsplit_once('|')?;
    // Colour bitmap fonts (Noto Color Emoji) can't be rasterised by egui.
    if file.is_empty() || file.to_lowercase().contains("color") {
        return None;
    }
    Some((std::fs::read(file).ok()?, index.parse().unwrap_or(0)))
}

#[cfg(windows)]
fn fc_match(_pattern: &str) -> Option<(Vec<u8>, u32)> {
    None
}

fn font_data((bytes, index): (Vec<u8>, u32)) -> Arc<FontData> {
    let mut d = FontData::from_owned(bytes);
    d.index = index;
    Arc::new(d)
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
        let add = |defs: &mut FontDefinitions, key: &str, file: &str| -> bool {
            match read_font(file) {
                Some(b) => {
                    defs.font_data.insert(key.to_string(), Arc::new(FontData::from_owned(b)));
                    true
                }
                None => false,
            }
        };
        // Not on Windows: take the font from fontconfig when the Windows file is missing.
        let add_or = |defs: &mut FontDefinitions, key: &str, file: &str, pattern: &str| -> bool {
            add(defs, key, file)
                || match fc_match(pattern) {
                    Some(f) if !cfg!(windows) => {
                        defs.font_data.insert(key.to_string(), font_data(f));
                        true
                    }
                    _ => false,
                }
        };
        let mut ui_chain = Vec::new();
        if add_or(&mut defs, "ui", "segoeui.ttf", "sans-serif") {
            ui_chain.push("ui".to_string());
        }
        if add(&mut defs, "ui-symbol", "seguisym.ttf") {
            ui_fallbacks.push("ui-symbol".to_string());
        }
        if add(&mut defs, "ui-emoji", "seguiemj.ttf") {
            ui_fallbacks.push("ui-emoji".to_string());
        }
        // Linux/macOS: broad symbol coverage for the icon fallbacks (arrows, ✂, ⧉, ⫷ ...).
        if !cfg!(windows) {
            for (i, pat) in ["DejaVu Sans", "Noto Sans Symbols2", "Noto Sans Symbols", "Noto Sans Math"].iter().enumerate() {
                let key = format!("ui-symbol-{i}");
                if let Some(f) = fc_match(pat).filter(|_| {
                    // fc-match always answers; keep only the family that was asked for.
                    std::process::Command::new("fc-list")
                        .arg(pat)
                        .output()
                        .map(|o| !o.stdout.is_empty())
                        .unwrap_or(false)
                }) {
                    defs.font_data.insert(key.clone(), font_data(f));
                    ui_fallbacks.push(key);
                }
            }
        }
        let icons = add(&mut defs, ICON_FAMILY, "SegoeIcons.ttf") || add(&mut defs, ICON_FAMILY, "segmdl2.ttf");
        if add_or(&mut defs, "ui-bold", "segoeuib.ttf", "sans-serif:bold") {
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
        let data = data.map(|b| (b, 0)).or_else(|| {
            let style = match (bold, italic) {
                (false, false) => "",
                (true, false) => ":bold",
                (false, true) => ":italic",
                (true, true) => ":bold:italic",
            };
            fc_match(&format!("{name}{style}"))
        });
        let Some(data) = data else {
            self.failed.insert(key);
            return false;
        };
        self.defs.font_data.insert(key.clone(), font_data(data));
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

/// Flat visuals similar to desktop office ribbons, in the current light or dark palette.
pub fn apply_style(ctx: &egui::Context) {
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    let dark = is_dark();
    let mut style = (*ctx.style()).clone();
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.window_fill = window();
    v.panel_fill = chrome();
    v.extreme_bg_color = window();
    v.faint_bg_color = if dark { rgb(0x262626) } else { rgb(0xF7F7F7) };
    v.code_bg_color = v.faint_bg_color;
    v.selection.bg_fill = accent_light();
    v.selection.stroke = Stroke::new(1.0, accent());
    v.hyperlink_color = accent();
    v.window_corner_radius = egui::CornerRadius::same(6);
    v.menu_corner_radius = egui::CornerRadius::same(4);
    v.window_stroke = Stroke::new(1.0, border());
    let shadow = if dark { 110 } else { 40 };
    v.window_shadow = egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(shadow) };
    v.popup_shadow = egui::Shadow { offset: [0, 3], blur: 10, spread: 0, color: Color32::from_black_alpha(shadow - 5) };
    let w = &mut v.widgets;
    w.noninteractive.fg_stroke = Stroke::new(1.0, text());
    w.noninteractive.bg_stroke = Stroke::new(1.0, border());
    w.noninteractive.bg_fill = window();
    w.noninteractive.weak_bg_fill = window();
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.inactive.bg_fill = if dark { rgb(0x333333) } else { Color32::WHITE };
    // visible outlines for checkboxes, number boxes, combo boxes and dialog buttons
    w.inactive.bg_stroke = Stroke::new(1.0, if dark { rgb(0x707070) } else { Color32::from_gray(170) });
    w.inactive.fg_stroke = Stroke::new(1.0, text());
    w.inactive.corner_radius = egui::CornerRadius::same(3);
    w.hovered.weak_bg_fill = hover();
    w.hovered.bg_fill = hover();
    w.hovered.bg_stroke = Stroke::new(1.0, button_edge());
    w.hovered.fg_stroke = Stroke::new(1.0, text());
    w.hovered.corner_radius = egui::CornerRadius::same(3);
    w.hovered.expansion = 0.0;
    w.active.weak_bg_fill = pressed();
    w.active.bg_fill = pressed();
    w.active.bg_stroke = Stroke::new(1.0, if dark { rgb(0xA0A0A0) } else { rgb(0xB0B0B0) });
    w.active.fg_stroke = Stroke::new(1.0, text());
    w.active.corner_radius = egui::CornerRadius::same(3);
    w.active.expansion = 0.0;
    w.open.weak_bg_fill = pressed();
    w.open.bg_fill = pressed();
    w.open.fg_stroke = Stroke::new(1.0, text());
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
