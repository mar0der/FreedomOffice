//! Application shell: state, undo, editing and the frame loop.

mod actions;
mod chart_view;
mod chrome;
mod condfmt_ui;
mod dialogs;
mod editor;
mod grid;
mod keys;
mod keytips;
mod ribbon;
mod theme;
mod validation_ui;

use std::path::PathBuf;

use eframe::egui;

use crate::model::{cell_from_input, Cell, Range, Sheet, SheetView, Style, Workbook};
pub use editor::{EditMode, Editor};
use theme::FontManager;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RibbonTab {
    Home,
    Insert,
    PageLayout,
    Formulas,
    Data,
    Review,
    View,
    Help,
    /// Contextual tab shown while a chart is selected.
    ChartDesign,
}

#[derive(Clone)]
pub struct ClipCell {
    pub dr: u32,
    pub dc: u32,
    pub cell: Cell,
    pub style: Style,
}

#[derive(Clone)]
pub struct Clip {
    pub sheet: usize,
    pub range: Range,
    pub cells: Vec<ClipCell>,
    pub text: String,
    pub cut: bool,
    /// Notes inside the copied range: (row offset, col offset, text).
    pub notes: Vec<(u32, u32, String)>,
}

/// Undo snapshot of part of the workbook.
#[derive(Clone)]
pub enum Snap {
    Range { sheet: usize, rg: Range, cells: Vec<(u32, u32, Cell)> },
    Sheet { idx: usize, sheet: Box<Sheet> },
    Sheets { sheets: Vec<Sheet>, active: usize, names: Vec<(String, String)> },
}

#[derive(Clone, Copy)]
pub enum Scope {
    Range(usize, Range),
    Sheet(usize),
    All,
}

pub struct UndoEntry {
    snaps: Vec<Snap>,
    view: (usize, SheetView),
}

#[derive(Default, Clone, Copy, PartialEq)]
pub enum Drag {
    #[default]
    None,
    Cells,
    Rows,
    Cols,
    ResizeCol(u32, f32),
    ResizeRow(u32, f32),
    Fill(Range),
    Point((u32, u32)),
    VScroll(f32, u32),
    HScroll(f32, u32),
    /// Dragging the selection by its border; (row, col) offset of the grabbed cell.
    Move(u32, u32),
}

pub enum Pending {
    New,
    Open(Option<PathBuf>),
    Exit,
}

pub struct FilterPopup {
    pub col: u32,
    pub pos: egui::Pos2,
    pub values: Vec<(String, bool)>,
    pub search: String,
}

#[derive(Default)]
pub struct FindState {
    pub what: String,
    pub with: String,
    pub match_case: bool,
    pub whole: bool,
    pub in_formulas: bool,
    pub all_sheets: bool,
    pub replace: bool,
    pub focus: bool,
}

pub struct App {
    pub wb: Workbook,
    pub fonts: FontManager,
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    pub edit: Option<Editor>,
    pub clip: Option<Clip>,
    pub tab: RibbonTab,
    pub ribbon_collapsed: bool,
    pub backstage: Option<chrome::Backstage>,
    pub dialog: Option<dialogs::Dialog>,
    pub zoom: f32,
    pub show_formulas: bool,
    pub show_formula_bar: bool,
    pub show_headings: bool,
    pub status: Option<String>,
    pub dirty: bool,
    pub name_box: String,
    pub name_box_editing: bool,
    pub drag: Drag,
    pub layout: Option<grid::Layout>,
    pub rename_tab: Option<(usize, String)>,
    pub format_painter: Option<(Vec<(u32, u32, Style)>, u32, u32, bool)>,
    pub filter_popup: Option<FilterPopup>,
    pub title: String,
    pub recent: Vec<PathBuf>,
    pub find: FindState,
    pub pending: Option<Pending>,
    pub wheel_acc: egui::Vec2,
    pub allow_close: bool,
    pub caption_done: bool,
    pub last_font: Option<String>,
    pub ctx: Option<egui::Context>,
    pub edit_rect: Option<egui::Rect>,
    pub edit_bar_rect: Option<egui::Rect>,
    pub tab_scroll: usize,
    pub generation: u64,
    pub stats_cache: Option<((usize, Range, u64), Option<(f64, usize, usize, f64)>)>,
    pub last_font_color: u32,
    pub last_fill: u32,
    /// Column where a Tab-entry run started (Enter returns there).
    pub tab_origin: Option<u32>,
    /// Active Alt key sequence (KeyTips), e.g. "HO".
    pub keytips: Option<String>,
    alt_clean: bool,
    prev_alt: bool,
    pub hover_cell: Option<(u32, u32)>,
    pub show_all_notes: bool,
    pub page_setup: crate::pdf::PageSetup,
    pub print_selection: bool,
    pub chart: chart_view::ChartUi,
    pub cf: condfmt_ui::CfState,
    pub dv: validation_ui::DvState,
}

const MAX_UNDO: usize = 100;

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        theme::apply_style(&cc.egui_ctx);
        let fonts = FontManager::new(&cc.egui_ctx);
        #[cfg(windows)]
        set_caption_color(cc);
        let mut app = App {
            wb: Workbook::new(),
            fonts,
            undo: Vec::new(),
            redo: Vec::new(),
            edit: None,
            clip: None,
            tab: RibbonTab::Home,
            ribbon_collapsed: false,
            backstage: None,
            dialog: None,
            zoom: 1.0,
            show_formulas: false,
            show_formula_bar: true,
            show_headings: true,
            status: None,
            dirty: false,
            name_box: String::new(),
            name_box_editing: false,
            drag: Drag::None,
            layout: None,
            rename_tab: None,
            format_painter: None,
            filter_popup: None,
            title: String::new(),
            recent: load_recent(),
            find: FindState::default(),
            pending: None,
            wheel_acc: egui::Vec2::ZERO,
            allow_close: false,
            caption_done: true,
            last_font: None,
            ctx: Some(cc.egui_ctx.clone()),
            edit_rect: None,
            edit_bar_rect: None,
            tab_scroll: 0,
            generation: 0,
            stats_cache: None,
            last_font_color: 0xFF0000,
            last_fill: 0xFFFF00,
            tab_origin: None,
            keytips: None,
            alt_clean: false,
            prev_alt: false,
            hover_cell: None,
            show_all_notes: false,
            page_setup: crate::pdf::PageSetup::default(),
            print_selection: false,
            chart: Default::default(),
            cf: Default::default(),
            dv: Default::default(),
        };
        if let Some(p) = path {
            app.open_path(p);
        }
        app
    }

    // ------------------------------------------------------------------
    // Selection helpers
    // ------------------------------------------------------------------

    pub fn sheet(&self) -> &Sheet {
        self.wb.sheet()
    }
    pub fn sheet_mut(&mut self) -> &mut Sheet {
        self.wb.sheet_mut()
    }
    pub fn sel(&self) -> Range {
        self.sheet().view.sel
    }
    pub fn active(&self) -> (u32, u32) {
        self.sheet().view.active
    }

    /// Expands a range so it fully contains any merged areas it touches.
    pub fn expand_merges(&self, mut rg: Range) -> Range {
        let sh = self.sheet();
        loop {
            let mut grew = false;
            for m in &sh.merges {
                if m.intersects(&rg) {
                    let u = rg.union(m);
                    if u != rg {
                        rg = u;
                        grew = true;
                    }
                }
            }
            if !grew {
                return rg;
            }
        }
    }

    /// Moves the active cell; with `extend` grows the selection from the anchor.
    pub fn select(&mut self, r: u32, c: u32, extend: bool) {
        let r = r.min(crate::model::MAX_ROWS - 1);
        let c = c.min(crate::model::MAX_COLS - 1);
        if extend {
            let (ar, ac) = self.sheet().view.anchor;
            let rg = self.expand_merges(Range::new(ar, ac, r, c));
            let v = &mut self.sheet_mut().view;
            v.sel = rg;
            v.active = v.anchor;
            self.scroll_into_view(r, c);
        } else {
            let (r, c) = match self.sheet().merge_at(r, c) {
                Some(m) => (m.r1, m.c1),
                None => (r, c),
            };
            let rg = self.expand_merges(Range::cell(r, c));
            let v = &mut self.sheet_mut().view;
            v.active = (r, c);
            v.anchor = (r, c);
            v.sel = rg;
            self.scroll_into_view(r, c);
        }
    }

    pub fn select_range(&mut self, rg: Range) {
        let v = &mut self.sheet_mut().view;
        v.sel = rg;
        v.active = (rg.r1, rg.c1);
        v.anchor = (rg.r1, rg.c1);
        self.scroll_into_view(rg.r1, rg.c1);
    }

    pub fn scroll_into_view(&mut self, r: u32, c: u32) {
        let Some(lay) = &self.layout else { return };
        let (avail_h, avail_w) = (lay.avail_h, lay.avail_w);
        let z = self.zoom;
        let sh = &self.wb.sheets[self.wb.active];
        let (fr, fc) = sh.freeze;
        let mut top = sh.view.top.max(fr);
        let mut left = sh.view.left.max(fc);
        if r >= fr {
            if r < top {
                top = r;
            } else {
                // ensure r fits at the bottom
                let mut h = 0.0;
                let mut k = r as i64;
                let mut first = r;
                while k >= fr as i64 {
                    h += sh.row_height(k as u32) * z;
                    if h > avail_h && (k as u32) < r {
                        break;
                    }
                    first = k as u32;
                    k -= 1;
                }
                if first > top {
                    top = first;
                }
            }
        }
        if c >= fc {
            if c < left {
                left = c;
            } else {
                let mut w = 0.0;
                let mut k = c as i64;
                let mut first = c;
                while k >= fc as i64 {
                    w += sh.col_width(k as u32) * z;
                    if w > avail_w && (k as u32) < c {
                        break;
                    }
                    first = k as u32;
                    k -= 1;
                }
                if first > left {
                    left = first;
                }
            }
        }
        let v = &mut self.wb.sheets[self.wb.active].view;
        v.top = top;
        v.left = left;
    }

    // ------------------------------------------------------------------
    // Undo / redo
    // ------------------------------------------------------------------

    fn snapshot(&self, scope: Scope) -> Snap {
        match scope {
            Scope::Range(s, rg) => {
                let cells = self.wb.sheets[s].iter_range(rg).map(|(r, c, cell)| (r, c, cell.clone())).collect();
                Snap::Range { sheet: s, rg, cells }
            }
            Scope::Sheet(i) => Snap::Sheet { idx: i, sheet: Box::new(self.wb.sheets[i].clone()) },
            Scope::All => Snap::Sheets { sheets: self.wb.sheets.clone(), active: self.wb.active, names: self.wb.names.clone() },
        }
    }

    fn snap_scope(s: &Snap) -> Scope {
        match s {
            Snap::Range { sheet, rg, .. } => Scope::Range(*sheet, *rg),
            Snap::Sheet { idx, .. } => Scope::Sheet(*idx),
            Snap::Sheets { .. } => Scope::All,
        }
    }

    fn restore(&mut self, snap: Snap) {
        match snap {
            Snap::Range { sheet, rg, cells } => {
                let sh = &mut self.wb.sheets[sheet];
                let keys: Vec<(u32, u32)> = sh.iter_range(rg).map(|(r, c, _)| (r, c)).collect();
                for (r, c) in keys {
                    sh.remove(r, c);
                }
                for (r, c, cell) in cells {
                    sh.set(r, c, Some(cell));
                }
            }
            Snap::Sheet { idx, sheet } => {
                if idx < self.wb.sheets.len() {
                    self.wb.sheets[idx] = *sheet;
                }
            }
            Snap::Sheets { sheets, active, names } => {
                self.wb.sheets = sheets;
                self.wb.active = active.min(self.wb.sheets.len() - 1);
                self.wb.names = names;
            }
        }
    }

    /// Records the current state of the given scopes before a change.
    pub fn push_undo(&mut self, scopes: &[Scope]) {
        let snaps = scopes.iter().map(|s| self.snapshot(*s)).collect();
        let view = (self.wb.active, self.sheet().view.clone());
        self.undo.push(UndoEntry { snaps, view });
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn apply_undo(&mut self, from_redo: bool) {
        let entry = if from_redo { self.redo.pop() } else { self.undo.pop() };
        let Some(entry) = entry else { return };
        self.cancel_edit();
        let inverse_snaps: Vec<Snap> = entry.snaps.iter().map(|s| self.snapshot_safe(Self::snap_scope(s))).collect();
        let inverse = UndoEntry { snaps: inverse_snaps, view: (self.wb.active, self.sheet().view.clone()) };
        for s in entry.snaps.into_iter().rev() {
            self.restore(s);
        }
        let (si, view) = entry.view;
        if si < self.wb.sheets.len() {
            self.wb.active = si;
            self.wb.sheets[si].view = view;
        }
        if from_redo { self.undo.push(inverse) } else { self.redo.push(inverse) }
        self.changed();
    }

    fn snapshot_safe(&self, scope: Scope) -> Snap {
        match scope {
            Scope::Sheet(i) if i >= self.wb.sheets.len() => self.snapshot(Scope::All),
            Scope::Range(s, _) if s >= self.wb.sheets.len() => self.snapshot(Scope::All),
            s => self.snapshot(s),
        }
    }

    pub fn undo(&mut self) {
        self.apply_undo(false);
    }
    pub fn redo(&mut self) {
        self.apply_undo(true);
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Call after any workbook mutation.
    pub fn changed(&mut self) {
        self.generation += 1;
        self.wb.recalc();
        self.cf.invalidate();
        self.dirty = true;
        if self.wb.circular {
            self.status = Some("Circular reference warning: one or more formulas refer to their own cell.".into());
        }
    }

    // ------------------------------------------------------------------
    // Editing
    // ------------------------------------------------------------------

    pub fn begin_edit(&mut self, initial: Option<String>, mode: EditMode) {
        if self.edit.is_some() {
            return;
        }
        let (r, c) = self.active();
        let text = match initial {
            Some(t) => t,
            None => self.sheet().get(r, c).map(|c| c.input_text()).unwrap_or_default(),
        };
        self.edit = Some(Editor::new(self.wb.active, r, c, text, mode));
        self.clip_clear_if_cut_target();
    }

    fn clip_clear_if_cut_target(&mut self) {}

    pub fn cancel_edit(&mut self) {
        if let Some(e) = self.edit.take() {
            self.wb.active = e.sheet.min(self.wb.sheets.len() - 1);
        }
    }

    /// Commits the editor. Returns false if the formula was rejected.
    pub fn commit_edit(&mut self, fill_selection: bool) -> bool {
        let Some(mut e) = self.edit.take() else { return true };
        self.wb.active = e.sheet;
        let mut text = e.text.clone();
        if text.starts_with('=') && text.len() > 1 {
            // Auto-close parentheses like the familiar autocorrect.
            if crate::formula::parse(&text[1..]).is_err() {
                let open = text.matches('(').count() as i64 - text.matches(')').count() as i64;
                let fixed = format!("{}{}", text, ")".repeat(open.max(0) as usize));
                if open > 0 && crate::formula::parse(&fixed[1..]).is_ok() {
                    text = fixed;
                } else {
                    let msg = match crate::formula::parse(&text[1..]) {
                        Err(m) => m,
                        Ok(_) => String::new(),
                    };
                    self.dialog = Some(dialogs::Dialog::Message {
                        title: "FreedomOffice Sheets".into(),
                        text: format!("There's a problem with this formula.\n\n{msg}"),
                    });
                    e.mode = EditMode::Edit;
                    self.edit = Some(e);
                    return false;
                }
            }
        }
        if !self.dv_check_entry(&e, &text, fill_selection) {
            self.edit = Some(e);
            return false;
        }
        let targets: Vec<(u32, u32)> = if fill_selection {
            let sel = self.sel();
            let n = sel.rows() as u64 * sel.cols() as u64;
            if n > 200_000 {
                vec![(e.row, e.col)]
            } else {
                (sel.r1..=sel.r2).flat_map(|r| (sel.c1..=sel.c2).map(move |c| (r, c))).collect()
            }
        } else {
            vec![(e.row, e.col)]
        };
        let rg = if fill_selection { self.sel() } else { Range::cell(e.row, e.col) };
        self.push_undo(&[Scope::Range(e.sheet, rg)]);
        let base = self.sheet().get(e.row, e.col).cloned();
        for (r, c) in targets {
            let style = self.wb.sheets[e.sheet].get(r, c).map(|c| c.style).unwrap_or(0);
            let mut cell = cell_from_input(&mut self.wb, &text, style);
            if let (Some(cell), true) = (&mut cell, (r, c) != (e.row, e.col)) {
                if let Some(f) = &mut cell.formula {
                    if let Some(ast) = &mut f.ast {
                        crate::formula::shift_relative(ast, r as i64 - e.row as i64, c as i64 - e.col as i64);
                        **f = crate::model::Formula::from_ast(ast.clone());
                    }
                }
            }
            // Wrap-text automatically when the entry has a line break.
            if text.contains('\n') {
                if let Some(cell) = &mut cell {
                    let mut st = self.wb.style(cell.style).clone();
                    if !st.wrap {
                        st.wrap = true;
                        cell.style = self.wb.intern_style(st);
                    }
                }
            }
            self.wb.sheets[e.sheet].set(r, c, cell);
        }
        let _ = base;
        // Grow the row for wrapped multi-line text.
        let lines = text.lines().count().max(1) as f32;
        if lines > 1.0 {
            let sh = &mut self.wb.sheets[e.sheet];
            let need = (crate::model::DEFAULT_ROW_H - 5.0) * lines + 5.0;
            if sh.raw_row_height(e.row) < need {
                sh.row_h.insert(e.row, need);
            }
        }
        self.changed();
        true
    }

    // ------------------------------------------------------------------
    // Files
    // ------------------------------------------------------------------

    pub fn confirm_then(&mut self, p: Pending) {
        if self.dirty {
            self.pending = Some(p);
            self.dialog = Some(dialogs::Dialog::SavePrompt);
        } else {
            self.run_pending(p);
        }
    }

    pub fn run_pending(&mut self, p: Pending) {
        match p {
            Pending::New => {
                self.wb = Workbook::new();
                self.reset_doc();
            }
            Pending::Open(path) => {
                let path = path.or_else(|| {
                    let mut d = rfd::FileDialog::new().set_title("Open");
                    for (label, exts) in crate::io::OPEN_FILTERS {
                        d = d.add_filter(*label, exts);
                    }
                    d.pick_file()
                });
                if let Some(p) = path {
                    self.open_path(p);
                }
            }
            Pending::Exit => {
                self.allow_close = true;
            }
        }
    }

    fn reset_doc(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.edit = None;
        self.clip = None;
        self.dirty = false;
        self.backstage = None;
        self.filter_popup = None;
        self.cf = Default::default();
        self.dv = Default::default();
    }

    pub fn open_path(&mut self, p: PathBuf) {
        match crate::io::load(&p) {
            Ok(mut wb) => {
                wb.path = Some(p.clone());
                if wb.sheets.is_empty() {
                    wb.sheets.push(Sheet::new("Sheet1"));
                }
                wb.active = wb.active.min(wb.sheets.len() - 1);
                wb.recalc();
                self.wb = wb;
                self.reset_doc();
                self.add_recent(p);
                self.status = None;
            }
            Err(e) => {
                self.dialog = Some(dialogs::Dialog::Message {
                    title: "Open".into(),
                    text: format!("Sorry, we couldn't open '{}'.\n\n{e}", p.display()),
                });
            }
        }
    }

    pub fn save(&mut self) -> bool {
        self.commit_edit(false);
        match self.wb.path.clone() {
            Some(p) if is_native_format(&p) => self.save_to(p.clone()),
            _ => self.save_as(),
        }
    }

    pub fn save_as(&mut self) -> bool {
        self.commit_edit(false);
        let mut d = rfd::FileDialog::new().set_title("Save As").set_file_name(self.default_save_name());
        for (label, exts) in crate::io::SAVE_FILTERS {
            d = d.add_filter(*label, exts);
        }
        if let Some(dir) = self.wb.path.as_ref().and_then(|p| p.parent()) {
            d = d.set_directory(dir);
        }
        match d.save_file() {
            Some(p) => {
                let p = if p.extension().is_none() { p.with_extension("xlsx") } else { p };
                self.save_to(p)
            }
            None => false,
        }
    }

    fn default_save_name(&self) -> String {
        let stem = self
            .wb
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Book1".into());
        format!("{stem}.xlsx")
    }

    fn save_to(&mut self, p: PathBuf) -> bool {
        match crate::io::save(&self.wb, &p) {
            Ok(()) => {
                let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                if ext == "xlsx" || ext == "xlsm" {
                    self.wb.path = Some(p.clone());
                    self.dirty = false;
                } else if self.wb.path.is_none() {
                    self.wb.path = Some(p.clone());
                    self.dirty = false;
                }
                if ext == "csv" || ext == "tsv" || ext == "txt" {
                    self.status = Some("Saved the active sheet only (text formats keep one sheet without formatting).".into());
                } else {
                    self.status = Some(format!("Saved {}", p.display()));
                }
                self.add_recent(p);
                true
            }
            Err(e) => {
                self.dialog = Some(dialogs::Dialog::Message { title: "Save".into(), text: format!("Couldn't save the file.\n\n{e}") });
                false
            }
        }
    }

    fn add_recent(&mut self, p: PathBuf) {
        self.recent.retain(|x| x != &p);
        self.recent.insert(0, p);
        self.recent.truncate(15);
        save_recent(&self.recent);
    }

    // ------------------------------------------------------------------
    // Frame
    // ------------------------------------------------------------------

    fn update_title(&mut self, ctx: &egui::Context) {
        let t = format!("{}{} - FreedomOffice Sheets", self.wb.file_name(), if self.dirty { " •" } else { "" });
        if t != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(t.clone()));
            self.title = t;
        }
    }
}

fn is_native_format(p: &std::path::Path) -> bool {
    matches!(p.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref(), Some("xlsx") | Some("xlsm"))
}

fn recent_file() -> Option<PathBuf> {
    std::env::var("APPDATA").ok().map(|a| PathBuf::from(a).join("FreedomOffice").join("sheets-recent.txt"))
}

fn load_recent() -> Vec<PathBuf> {
    recent_file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).collect())
        .unwrap_or_default()
}

fn save_recent(v: &[PathBuf]) {
    if let Some(f) = recent_file() {
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let s: Vec<String> = v.iter().map(|p| p.display().to_string()).collect();
        let _ = std::fs::write(f, s.join("\n"));
    }
}

#[cfg(windows)]
fn set_caption_color(cc: &eframe::CreationContext<'_>) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    let Ok(h) = cc.window_handle() else { return };
    if let RawWindowHandle::Win32(w) = h.as_raw() {
        let hwnd = w.hwnd.get() as windows_sys::Win32::Foundation::HWND;
        // COLORREF is 0x00BBGGRR
        let caption: u32 = 0x0046_7321;
        let text: u32 = 0x00FF_FFFF;
        unsafe {
            DwmSetWindowAttribute(hwnd, 35, &caption as *const u32 as *const _, 4);
            DwmSetWindowAttribute(hwnd, 36, &text as *const u32 as *const _, 4);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.fonts.flush(ctx);
        self.edit_rect = None;
        self.edit_bar_rect = None;
        self.generation += 1;

        // Close handling with unsaved-changes prompt.
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            if self.dirty {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if self.dialog.is_none() {
                    self.pending = Some(Pending::Exit);
                    self.dialog = Some(dialogs::Dialog::SavePrompt);
                }
            }
        }
        if self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        self.handle_keys(ctx);
        self.update_title(ctx);

        if self.backstage.is_some() {
            self.backstage_ui(ctx);
        } else {
            egui::TopBottomPanel::top("ribbon")
                .frame(egui::Frame::NONE.fill(theme::CHROME))
                .show_separator_line(false)
                .show(ctx, |ui| self.ribbon_ui(ui));
            if self.show_formula_bar {
                egui::TopBottomPanel::top("formula_bar")
                    .frame(egui::Frame::NONE.fill(theme::CHROME).inner_margin(egui::Margin { left: 4, right: 4, top: 4, bottom: 4 }))
                    .show_separator_line(false)
                    .show(ctx, |ui| self.formula_bar_ui(ui));
            }
            egui::TopBottomPanel::bottom("status")
                .exact_height(24.0)
                .frame(egui::Frame::NONE.fill(theme::CHROME))
                .show_separator_line(false)
                .show(ctx, |ui| self.status_bar_ui(ui));
            egui::TopBottomPanel::bottom("tabs")
                .exact_height(28.0)
                .frame(egui::Frame::NONE.fill(theme::CHROME))
                .show_separator_line(false)
                .show(ctx, |ui| self.sheet_tabs_ui(ui));
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(egui::Color32::WHITE)).show(ctx, |ui| self.grid_ui(ui));
            self.popups_ui(ctx);
            self.keytips_ui(ctx);
        }
        self.dialogs_ui(ctx);
    }
}
