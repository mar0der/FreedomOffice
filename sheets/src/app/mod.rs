//! Application shell: state, undo, editing and the frame loop.

mod actions;
mod chart_draw;
mod chart_pane;
mod chart_view;
mod chrome;
mod cloud_ui;
mod condfmt_ui;
mod data_ui;
mod dialogs;
mod editor;
mod fill;
mod flash;
mod filter_ui;
mod find_ui;
mod grid;
mod keys;
mod keytips;
mod picture_view;
mod menus;
mod paste;
mod print_ui;
mod review_ui;
mod ribbon;
mod table_view;
mod theme;
mod validation_ui;

use std::path::PathBuf;

use eframe::egui;

use crate::model::{cell_from_input, Cell, Range, Sheet, SheetView, Style, Workbook};
pub use chrome::Backstage;
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
    /// Contextual tab shown while the active cell is in a table.
    TableDesign,
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
    /// Copied whole rows (Some(true)) or whole columns (Some(false)).
    pub whole: Option<bool>,
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

pub use filter_ui::FilterPopup;
pub use find_ui::FindState;
pub use review_ui::Change;

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
    /// Formula bar expanded to three lines (Ctrl+Shift+U).
    pub formula_bar_tall: bool,
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
    pub print: print_ui::PrintState,
    /// Counts workbook edits (cache key for print layout and page breaks).
    pub edits: u64,
    /// Native window handle (for printer dialogs).
    pub hwnd: isize,
    /// When the Windows light/dark app mode was last checked.
    pub theme_checked: f64,
    pub last_press_pos: Option<egui::Pos2>,
    pub pending_link: Option<((u32, u32), String)>,
    /// AutoRecover: where this session's recovery copy is written, and when it was last written.
    pub recovery_path: Option<PathBuf>,
    pub last_autosave: f64,
    /// Last repeatable command (F4 / Ctrl+Y).
    pub last_action: Option<actions::Repeat>,
    /// Sheet tab being dragged to a new position.
    pub tab_drag: Option<usize>,
    pub chart: chart_view::ChartUi,
    pub tables: table_view::TableUi,
    pub pictures: picture_view::PictureUi,
    pub review: review_ui::ReviewUi,
    /// Sheet whose zoom `zoom` currently shows (zoom is kept per sheet, like Excel).
    zoom_for: Option<usize>,
    pub cf: condfmt_ui::CfState,
    pub dv: validation_ui::DvState,
    /// Microsoft 365 sign-in and the open cloud document.
    pub cloud: cloud_ui::CloudState,
    /// Auto Fill Options button after a fill drag.
    pub fill_button: Option<fill::FillButton>,
    /// Paste Options button after a paste.
    pub paste_button: Option<paste::PasteButton>,
    /// Keyboard modes (F8 Extend / Shift+F8 Add / End) and key tracking.
    pub kb: keys::KbState,
    /// Context menu requested from the keyboard (Shift+F10 / Menu key), and which one.
    pub menu: menus::MenuState,
}

const MAX_UNDO: usize = 100;

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        #[cfg(windows)]
        let hwnd = window_handle(cc);
        #[cfg(not(windows))]
        let hwnd = 0;
        Self::with_context(&cc.egui_ctx, hwnd, path)
    }

    /// Builds the app on an egui context (also used headless by tests).
    pub(crate) fn with_context(egui_ctx: &egui::Context, hwnd: isize, path: Option<PathBuf>) -> Self {
        theme::set_dark(theme::system_dark());
        theme::apply_style(egui_ctx);
        let fonts = FontManager::new(egui_ctx);
        #[cfg(windows)]
        set_caption_color(hwnd);
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
            formula_bar_tall: false,
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
            ctx: Some(egui_ctx.clone()),
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
            print: Default::default(),
            edits: 0,
            hwnd,
            theme_checked: 0.0,
            last_press_pos: None,
            pending_link: None,
            recovery_path: recovery_dir().map(|d| d.join(format!("Recovered-{}.xlsx", std::process::id()))),
            last_autosave: 0.0,
            last_action: None,
            tab_drag: None,
            chart: Default::default(),
            tables: Default::default(),
            pictures: Default::default(),
            review: Default::default(),
            zoom_for: None,
            cf: Default::default(),
            dv: Default::default(),
            cloud: Default::default(),
            fill_button: None,
            paste_button: None,
            kb: Default::default(),
            menu: Default::default(),
        };
        if let Some(p) = path {
            app.open_path(p);
        }
        // Offer files left behind by a session that ended unexpectedly.
        if let Some(dir) = recovery_dir() {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                let files: Vec<PathBuf> = rd
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("xlsx")) && Some(p) != app.recovery_path.as_ref())
                    .collect();
                if !files.is_empty() {
                    app.dialog = Some(dialogs::Dialog::Recover { files });
                }
            }
        }
        app
    }

    /// Writes a recovery copy every few minutes while there are unsaved changes.
    fn autosave(&mut self, ctx: &egui::Context) {
        const INTERVAL: f64 = 120.0;
        let now = ctx.input(|i| i.time);
        if !self.dirty {
            self.last_autosave = now;
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(30));
        if now - self.last_autosave < INTERVAL || self.edit.is_some() {
            return;
        }
        self.last_autosave = now;
        let Some(path) = self.recovery_path.clone() else { return };
        let mut wb = self.wb.clone();
        // Remember where the original lives inside the recovery copy's document name.
        let name = self.wb.file_name();
        std::thread::spawn(move || {
            if let Some(d) = path.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            wb.path = None;
            if crate::io::save(&wb, &path).is_ok() {
                let _ = std::fs::write(path.with_extension("name"), name);
            }
        });
    }

    pub fn remove_recovery(&self) {
        if let Some(p) = &self.recovery_path {
            let _ = std::fs::remove_file(p);
            let _ = std::fs::remove_file(p.with_extension("name"));
        }
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
            self.sheet_mut().view.extra.clear();
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
        v.extra.clear();
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
        self.edits += 1;
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
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Merges every undo step recorded since start into one, so a compound command undoes at once.
    pub fn squash_undo(&mut self, start: usize) {
        if self.undo.len() <= start + 1 {
            return;
        }
        let entries = self.undo.split_off(start);
        let view = entries[0].view.clone();
        let snaps = entries.into_iter().flat_map(|e| e.snaps).collect();
        self.undo.push(UndoEntry { snaps, view });
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Call after any workbook mutation.
    pub fn changed(&mut self) {
        self.generation += 1;
        self.edits += 1;
        self.wb.recalc();
        self.cf.invalidate();
        self.dirty = true;
        if self.wb.circular {
            self.status = Some("Circular reference warning: one or more formulas refer to their own cell.".into());
        }
    }

    /// Like `changed`, after only the contents of these cells changed: recalculates just
    /// their dependents (falls back to a full recalculation when needed).
    pub fn changed_ranges(&mut self, sheet: usize, ranges: &[Range]) {
        let edited: Vec<(usize, Range)> = ranges.iter().map(|r| (sheet, *r)).collect();
        crate::formula::recalc_ranges(&mut self.wb, &edited);
        self.generation += 1;
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
        if self.locked_block(Range::cell(r, c)) {
            return;
        }
        let text = match initial {
            Some(t) => t,
            None => self.edit_text(r, c),
        };
        self.edit = Some(Editor::new(self.wb.active, r, c, text, mode));
        self.clip_clear_if_cut_target();
    }

    fn clip_clear_if_cut_target(&mut self) {}

    /// What the formula bar / editor shows for a cell (dates edit as dates).
    pub fn edit_text(&self, r: u32, c: u32) -> String {
        let Some(cell) = self.sheet().get(r, c) else { return String::new() };
        // Hidden formulas of a protected sheet don't show.
        if cell.formula.is_some() && self.sheet().protection.is_some() && self.wb.style(cell.style).hide_formula {
            return String::new();
        }
        if cell.formula.is_none() {
            if let crate::model::Value::Number(n) = cell.value {
                if crate::numfmt::is_date_format(&self.wb.style(cell.style).num_fmt) && n >= 0.0 {
                    return crate::numfmt::edit_date(n);
                }
            }
        }
        cell.input_text()
    }

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
        if fill_selection && self.locked_block(self.sel()) {
            self.edit = Some(e);
            return false;
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
        let scope = if self.table_edit_affects(e.sheet, e.row, e.col) { Scope::Sheet(e.sheet) } else { Scope::Range(e.sheet, rg) };
        self.push_undo(&[scope]);
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
        if self.table_after_edit(e.sheet, e.row, e.col) {
            self.changed();
        } else {
            self.changed_ranges(e.sheet, &[rg]);
        }
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

    /// Ctrl+W / Ctrl+F4: closes the workbook. With one workbook per window this closes the
    /// window, after the same save prompt as the title-bar close button.
    pub fn close_workbook(&mut self) {
        self.confirm_then(Pending::Exit);
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
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
        self.tables = Default::default();
        self.pictures = Default::default();
        self.zoom_for = None;
        // A cloud document stays attached only when its downloaded copy is what was opened.
        self.cloud.doc = self.cloud.pending_open.take().filter(|d| Some(&d.local) == self.wb.path.as_ref());
        self.cloud.upload_failed = false;
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
        if self.cloud.doc.is_some() {
            return self.cloud_save();
        }
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
                // Saved to this PC: no longer linked to a cloud document.
                self.cloud.doc = None;
                let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                if ext == "xlsx" || ext == "xlsm" {
                    self.wb.path = Some(p.clone());
                    self.dirty = false;
                    self.remove_recovery();
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

    /// Switches between light and dark when the Windows app mode changes, like desktop Office does.
    fn follow_system_theme(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if now - self.theme_checked >= 2.0 {
            self.theme_checked = now;
            let dark = theme::system_dark();
            if dark != theme::is_dark() {
                theme::set_dark(dark);
                theme::apply_style(ctx);
                #[cfg(windows)]
                set_caption_color(self.hwnd);
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(2));
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let t = format!("{}{}{} - FreedomOffice Sheets", self.wb.file_name(), if self.dirty { " •" } else { "" }, self.cloud_title_suffix());
        if t != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(t.clone()));
            self.title = t;
        }
    }
}

fn is_native_format(p: &std::path::Path) -> bool {
    matches!(p.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref(), Some("xlsx") | Some("xlsm"))
}

fn recovery_dir() -> Option<PathBuf> {
    std::env::var("LOCALAPPDATA").ok().map(|a| PathBuf::from(a).join("FreedomOffice").join("AutoRecover"))
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
fn window_handle(cc: &eframe::CreationContext<'_>) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(w)) => w.hwnd.get(),
        _ => 0,
    }
}

/// Title bar colours: the familiar green in light mode, the chrome colour with a dark frame in dark mode.
#[cfg(windows)]
fn set_caption_color(hwnd: isize) {
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    if hwnd == 0 {
        return;
    }
    let dark = theme::is_dark();
    // COLORREF is 0x00BBGGRR
    let c = if dark { theme::chrome() } else { egui::Color32::from_rgb(0x21, 0x73, 0x46) };
    let caption: u32 = (c.b() as u32) << 16 | (c.g() as u32) << 8 | c.r() as u32;
    let text: u32 = 0x00FF_FFFF;
    let immersive_dark: u32 = dark as u32;
    let h = hwnd as windows_sys::Win32::Foundation::HWND;
    unsafe {
        DwmSetWindowAttribute(h, 20, &immersive_dark as *const u32 as *const _, 4);
        DwmSetWindowAttribute(h, 35, &caption as *const u32 as *const _, 4);
        DwmSetWindowAttribute(h, 36, &text as *const u32 as *const _, 4);
    }
}
impl App {
    /// Keeps `zoom` per sheet: stores it on the sheet it belongs to and loads the zoom of a
    /// newly active sheet (or of a newly opened workbook).
    fn sync_sheet_zoom(&mut self) {
        let a = self.wb.active.min(self.wb.sheets.len().saturating_sub(1));
        if self.zoom_for == Some(a) {
            self.wb.sheets[a].zoom = self.zoom;
        } else {
            self.zoom = self.wb.sheets[a].zoom.clamp(0.1, 4.0);
            self.zoom_for = Some(a);
        }
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Normal exit (after the save prompt): the recovery copy is no longer needed.
        self.remove_recovery();
    }

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

        self.follow_system_theme(ctx);
        self.sync_sheet_zoom();
        self.handle_keys(ctx);
        self.update_title(ctx);
        self.poll_print();
        self.poll_cloud(ctx);
        self.autosave(ctx);

        if self.backstage.is_some() {
            self.backstage_ui(ctx);
        } else {
            egui::TopBottomPanel::top("ribbon")
                .frame(egui::Frame::NONE.fill(theme::chrome()))
                .show_separator_line(false)
                .show(ctx, |ui| self.ribbon_ui(ui));
            if self.show_formula_bar {
                egui::TopBottomPanel::top("formula_bar")
                    .frame(egui::Frame::NONE.fill(theme::chrome()).inner_margin(egui::Margin { left: 4, right: 4, top: 4, bottom: 4 }))
                    .show_separator_line(false)
                    .show(ctx, |ui| self.formula_bar_ui(ui));
            }
            egui::TopBottomPanel::bottom("status")
                .exact_height(24.0)
                .frame(egui::Frame::NONE.fill(theme::chrome()))
                .show_separator_line(false)
                .show(ctx, |ui| self.status_bar_ui(ui));
            egui::TopBottomPanel::bottom("tabs")
                .exact_height(28.0)
                .frame(egui::Frame::NONE.fill(theme::chrome()))
                .show_separator_line(false)
                .show(ctx, |ui| self.sheet_tabs_ui(ui));
            self.chart_format_pane(ctx);
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(egui::Color32::WHITE)).show(ctx, |ui| self.grid_ui(ui));
            self.popups_ui(ctx);
            self.keytips_ui(ctx);
        }
        self.dialogs_ui(ctx);
        self.chart_dialogs(ctx);
        self.protect_dialog_ui(ctx);
        self.cloud_dialogs_ui(ctx);
    }
}
