//! Keyboard handling: the familiar shortcuts for navigation, editing and formatting.

use eframe::egui::{self, Event, Key, Modifiers};

use super::actions::BorderKind;
use super::dialogs::Dialog;
use super::{App, EditMode, Pending};
use crate::model::{Range, MAX_COLS, MAX_ROWS};

/// Keyboard modes shown on the status bar, and key-state tracking.
#[derive(Default)]
pub struct KbState {
    /// F8: arrows and clicks extend the selection.
    pub extend: bool,
    /// Shift+F8: the next selection is added to the existing ones.
    pub add: bool,
    add_base: Option<Vec<Range>>,
    /// End pressed: the next arrow jumps to the edge of the data like Ctrl+Arrow.
    pub end: bool,
    /// Ctrl+8: outline symbols hidden.
    pub outline_hidden: bool,
    ctrl_clean: bool,
    prev_ctrl: bool,
    menu_key: bool,
}

impl KbState {
    /// Status bar indicators, like "Extend Selection" and "End Mode".
    pub fn status(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.extend {
            v.push("Extend Selection");
        }
        if self.add {
            v.push("Add to Selection");
        }
        if self.end {
            v.push("End Mode");
        }
        v
    }

    fn clear_modes(&mut self) {
        self.extend = false;
        self.add = false;
        self.add_base = None;
        self.end = false;
    }

    /// Mouse modifiers as the selection modes imply (F8 = Shift, Shift+F8 = Ctrl).
    pub fn apply(&self, mut m: Modifiers) -> Modifiers {
        if self.extend {
            m.shift = true;
        }
        if self.add && !m.shift {
            m.ctrl = true;
        }
        m
    }
}

/// The Menu (Apps) key, which egui does not report.
#[cfg(windows)]
fn menu_key_down() -> bool {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetKeyState(vk: i32) -> i16;
    }
    const VK_APPS: i32 = 0x5D;
    unsafe { GetKeyState(VK_APPS) < 0 }
}

impl App {    pub fn handle_keys(&mut self, ctx: &egui::Context) {
        self.ctx = Some(ctx.clone());
        if self.backstage.is_some() {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.backstage = None;
            }
            return;
        }
        if self.dialog.is_some() || self.rename_tab.is_some() || self.name_box_editing {
            return;
        }
        if self.filter_popup.is_some() {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.filter_popup = None;
            }
            return;
        }
        if self.dv_popup_open() {
            return; // the validation list handles its own keys
        }
        let grid_id = egui::Id::new("grid");
        let focused = ctx.memory(|m| m.focused());
        if focused.is_some() && focused != Some(grid_id) {
            return;
        }
        // Tapping Alt alone toggles KeyTips (Alt, H, O, I ...).
        let (alt, ctrl_down, any_input) = ctx.input(|i| {
            (
                i.modifiers.alt,
                i.modifiers.ctrl,
                i.events.iter().any(|e| matches!(e, Event::Key { pressed: true, .. } | Event::PointerButton { .. } | Event::Text(_))),
            )
        });
        if alt && !self.prev_alt {
            self.alt_clean = !ctrl_down;
        }
        if any_input && alt {
            self.alt_clean = false;
        }
        if !alt && self.prev_alt && self.alt_clean && self.edit.is_none() {
            self.keytips = if self.keytips.is_some() { None } else { Some(String::new()) };
        }
        self.prev_alt = alt;
        if self.keytips.is_some() {
            let events = ctx.input(|i| i.events.clone());
            for ev in events {
                match ev {
                    Event::Key { key: Key::Escape, pressed: true, .. } => {
                        if let Some(s) = &mut self.keytips {
                            if s.pop().is_none() {
                                self.keytips = None;
                            }
                        }
                    }
                    Event::Key { key, pressed: true, modifiers, .. } if !modifiers.ctrl => match key_char(key) {
                        Some(ch) => self.keytip_key(ctx, ch),
                        None => self.keytips = None,
                    },
                    Event::PointerButton { pressed: true, .. } => self.keytips = None,
                    _ => {}
                }
                if self.keytips.is_none() {
                    break;
                }
            }
            return;
        }
        if self.chart_keys(ctx) {
            return;
        }
        #[cfg(windows)]
        {
            let down = menu_key_down();
            if down && !self.kb.menu_key && self.edit.is_none() {
                self.open_context_menu_kb();
            }
            self.kb.menu_key = down;
        }
        // Ctrl pressed and released on its own opens the Paste Options menu after a paste.
        let (ctrl_now, other) = ctx.input(|i| {
            (i.modifiers.ctrl, i.events.iter().any(|e| !matches!(e, Event::PointerMoved(_) | Event::MouseMoved(_))))
        });
        if ctrl_now && !self.kb.prev_ctrl {
            self.kb.ctrl_clean = true;
        }
        if ctrl_now && other {
            self.kb.ctrl_clean = false;
        }
        if !ctrl_now && self.kb.prev_ctrl && self.kb.ctrl_clean && self.edit.is_none() {
            self.toggle_paste_menu();
        }
        self.kb.prev_ctrl = ctrl_now;
        let events = ctx.input(|i| i.events.clone());
        // A letter that picked a Paste Options entry must not also start typing.
        let mut eat_text = false;
        for ev in events {
            if self.dialog.is_some() || self.backstage.is_some() {
                break;
            }
            match ev {
                Event::Copy => match &mut self.edit {
                    Some(e) => {
                        if e.has_selection() {
                            ctx.copy_text(e.selected_text().to_string());
                        }
                    }
                    None => self.copy(ctx, false),
                },
                Event::Cut => match &mut self.edit {
                    Some(e) => {
                        if e.has_selection() {
                            ctx.copy_text(e.selected_text().to_string());
                            e.backspace();
                        }
                    }
                    None => self.copy(ctx, true),
                },
                Event::Paste(_) if self.edit.is_none() && ctx.input(|i| i.modifiers.alt) => {
                    self.open_paste_special();
                }
                Event::Paste(s) => match &mut self.edit {
                    Some(e) => e.insert(s.trim_end_matches(['\r', '\n'])),
                    None => self.paste_text(&s),
                },
                Event::Text(t) => {
                    let m = ctx.input(|i| i.modifiers);
                    if ((m.ctrl || m.command) && !m.alt) || (m.alt && !m.ctrl) {
                        continue;
                    }
                    if eat_text || t.chars().any(|c| c.is_control()) {
                        continue;
                    }
                    self.on_text(&t);
                }
                Event::Key { key: Key::Escape, pressed: true, .. } if self.paste_menu_open() => {
                    self.dismiss_paste_button();
                }
                Event::Key { key, pressed: true, modifiers, .. } if self.paste_menu_open() && !modifiers.ctrl && key_char(key).is_some() => {
                    self.paste_menu_key(key_char(key).unwrap());
                    eat_text = true;
                }
                Event::Key { key, pressed: true, modifiers, .. } => self.on_key(ctx, key, modifiers),
                _ => {}
            }
        }
    }

    fn on_text(&mut self, t: &str) {
        self.format_painter = None;
        match &mut self.edit {
            Some(e) => e.insert(t),
            None => {
                self.begin_edit(Some(String::new()), EditMode::Enter);
                if let Some(e) = &mut self.edit {
                    e.insert(t);
                }
            }
        }
        self.autocomplete_value();
    }

    /// Cell AutoComplete: completes text from matching entries in the same column.
    fn autocomplete_value(&mut self) {
        let Some(e) = &self.edit else { return };
        if e.is_formula() || e.in_bar || e.cursor != e.text.len() || e.text.is_empty() || e.text.contains('\n') {
            return;
        }
        let typed = e.text.clone();
        if typed.chars().all(|c| c.is_ascii_digit() || ".,-+ ".contains(c)) {
            return;
        }
        let lower = typed.to_lowercase();
        let sh = &self.wb.sheets[e.sheet];
        let (row, col) = (e.row, e.col);
        let mut found: Option<String> = None;
        for dir in [-1i64, 1] {
            let mut r = row as i64 + dir;
            let mut n = 0;
            while r >= 0 && r < MAX_ROWS as i64 && n < 20_000 {
                match sh.value(r as u32, col) {
                    crate::model::Value::Text(t) => {
                        if t.chars().count() > typed.chars().count() && t.to_lowercase().starts_with(&lower) {
                            match &found {
                                None => found = Some(t.clone()),
                                Some(f) if f.to_lowercase() != t.to_lowercase() => return,
                                _ => {}
                            }
                        }
                    }
                    crate::model::Value::Empty => break,
                    _ => {}
                }
                r += dir;
                n += 1;
            }
        }
        if let (Some(f), Some(e)) = (found, &mut self.edit) {
            let n = typed.chars().count();
            let split = f.char_indices().nth(n).map(|(b, _)| b).unwrap_or(f.len());
            e.text = f.clone();
            e.cursor = e.text.len();
            e.anchor = split;
        }
    }

    fn autocomplete_list(&self) -> Vec<&'static str> {
        let Some(e) = &self.edit else { return vec![] };
        let Some((_, prefix)) = e.func_prefix() else { return vec![] };
        let p = prefix.to_uppercase();
        crate::formula::function_names().map(|(n, _)| n).filter(|n| n.starts_with(&p)).take(12).collect()
    }

    pub fn autocomplete(&self) -> Vec<&'static str> {
        self.autocomplete_list()
    }

    fn point_move(&mut self, dr: i64, dc: i64, shift: bool) {
        let active_sheet = self.wb.active;
        let Some(e) = &self.edit else { return };
        let other_sheet = e.sheet != active_sheet;
        let base = match &e.point {
            Some(p) if p.end == e.cursor => p.cur,
            _ => {
                if other_sheet {
                    self.active()
                } else {
                    (e.row, e.col)
                }
            }
        };
        let anchor_existing = e.point.as_ref().filter(|p| p.end == e.cursor).map(|p| p.anchor);
        let nr = (base.0 as i64 + dr).clamp(0, MAX_ROWS as i64 - 1) as u32;
        let nc = (base.1 as i64 + dc).clamp(0, MAX_COLS as i64 - 1) as u32;
        let anchor = if shift { anchor_existing.unwrap_or(base) } else { (nr, nc) };
        let prefix = if other_sheet { Some(self.sheet().name.clone()) } else { None };
        if let Some(e) = &mut self.edit {
            e.set_point(anchor, (nr, nc), prefix.as_deref());
        }
        self.scroll_into_view(nr, nc);
    }

    fn on_key(&mut self, ctx: &egui::Context, key: Key, m: Modifiers) {
        let ctrl = m.ctrl || m.command;
        if self.edit.is_some() {
            self.on_key_editing(ctx, key, m);
            return;
        }
        let sel = self.sel();
        // End mode lasts for one key; F8 Extend mode makes movement extend the selection.
        let end_mode = std::mem::take(&mut self.kb.end);
        let extend = m.shift || self.kb.extend;
        match key {
            Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight => {
                if m.alt && m.shift && matches!(key, Key::ArrowRight | Key::ArrowLeft) {
                    self.outline_group_cmd(key == Key::ArrowLeft);
                    return;
                }
                if m.alt {
                    if key == Key::ArrowDown {
                        self.dv_open_dropdown();
                    }
                    return;
                }
                let (dr, dc) = match key {
                    Key::ArrowUp => (-1, 0),
                    Key::ArrowDown => (1, 0),
                    Key::ArrowLeft => (0, -1),
                    _ => (0, 1),
                };
                self.tab_origin = None;
                if ctrl || end_mode { self.jump(dr, dc, extend) } else { self.move_active(dr, dc, extend) }
                // Add to Selection: earlier areas stay selected while the new one moves.
                if let (true, Some(base)) = (self.kb.add, self.kb.add_base.clone()) {
                    let a = self.sel();
                    self.sheet_mut().view.extra = base.into_iter().filter(|x| *x != a).collect();
                }
            }
            Key::Enter => {
                if m.alt {
                    self.begin_edit(None, EditMode::Edit);
                } else if !ctrl {
                    self.move_in_selection(if m.shift { -1 } else { 1 }, 0);
                }
            }
            Key::Tab => self.move_in_selection(0, if m.shift { -1 } else { 1 }),
            Key::Home => {
                let (fr, fc) = self.sheet().freeze;
                if end_mode {
                    // End, Home: the last used cell, like Ctrl+End.
                    let (er, ec) = self.sheet().extent().unwrap_or((0, 0));
                    self.select(er, ec, extend);
                } else if ctrl {
                    self.select(fr, fc, extend);
                } else {
                    let r = self.active().0;
                    self.select(r, 0, extend);
                }
            }
            Key::End if ctrl => {
                let (er, ec) = self.sheet().extent().unwrap_or((0, 0));
                self.select(er, ec, m.shift);
            }
            Key::End if !m.alt => self.kb.end = !end_mode,
            Key::F8 if !ctrl && !m.alt => {
                if m.shift {
                    let on = !self.kb.add;
                    self.kb.clear_modes();
                    self.kb.add = on;
                    self.kb.add_base = on.then(|| self.areas());
                } else {
                    let on = !self.kb.extend;
                    self.kb.clear_modes();
                    self.kb.extend = on;
                }
            }
            Key::F10 if m.shift && !ctrl => self.open_context_menu_kb(),
            Key::F7 if !m.shift && !ctrl && !m.alt => {
                self.dialog = Some(Dialog::Message { title: "Spelling".into(), text: "Spelling isn't available in FreedomOffice Sheets yet.".into() });
            }
            // Alt+F1: a default (clustered column) chart of the selection on this sheet.
            Key::F1 if m.alt => self.insert_chart(crate::chart::ChartKind::Column),
            Key::PageDown | Key::PageUp => {
                let down = key == Key::PageDown;
                if ctrl {
                    let j = self.next_visible_sheet(down);
                    self.switch_sheet(j);
                } else if m.alt {
                    self.page_horiz(down, m.shift);
                } else {
                    self.page(down, m.shift);
                }
            }
            Key::F2 if ctrl => {
                self.commit_edit(false);
                self.backstage = Some(super::chrome::Backstage::Print);
            }
            Key::F2 if !m.shift => self.begin_edit(None, EditMode::Edit),
            Key::F2 => self.edit_note(),
            Key::F3 if m.shift && ctrl => self.open_create_names(),
            Key::F3 if m.shift => self.dialog = Some(Dialog::insert_function()),
            Key::F3 if ctrl => self.dialog = Some(Dialog::NameManager { name: String::new(), refers: self.sel_abs_ref(), error: None }),
            Key::F5 => self.dialog = Some(Dialog::GoTo { text: String::new(), focus: true }),
            Key::F4 if !ctrl => self.repeat_last(),
            Key::Backspace if ctrl => {
                let (r, c) = self.active();
                self.scroll_into_view(r, c);
            }
            Key::F9 => self.recalc_now(),
            Key::F11 if m.shift => self.add_sheet(),
            Key::F12 if ctrl => self.confirm_then(Pending::Open(None)),
            Key::F12 => {
                self.save_as();
            }
            Key::Delete => self.clear(true, false),
            Key::Backspace => {
                self.begin_edit(Some(String::new()), EditMode::Enter);
            }
            Key::Escape => {
                self.kb.clear_modes();
                self.dismiss_paste_button();
                self.clip = None;
                self.format_painter = None;
                self.status = None;
            }
            Key::Space if ctrl && m.shift => self.select_range(Range::new(0, 0, MAX_ROWS - 1, MAX_COLS - 1)),
            Key::Space if ctrl => {
                let a = self.active();
                let v = &mut self.sheet_mut().view;
                v.sel = Range::new(0, sel.c1, MAX_ROWS - 1, sel.c2);
                v.active = a;
            }
            Key::Space if m.shift => {
                let a = self.active();
                let v = &mut self.sheet_mut().view;
                v.sel = Range::new(sel.r1, 0, sel.r2, MAX_COLS - 1);
                v.active = a;
            }
            Key::Equals if m.alt => self.autosum("SUM"),
            k if m.alt && !ctrl && key_char(k).is_some() => {
                self.keytips = Some(String::new());
                self.keytip_key(ctx, key_char(k).unwrap());
            }
            _ if ctrl => self.on_ctrl_key(ctx, key, m),
            _ => {}
        }
    }

    fn on_ctrl_key(&mut self, _ctx: &egui::Context, key: Key, m: Modifiers) {
        let shift = m.shift;
        match key {
            Key::Z => self.undo(),
            Key::Y => {
                if self.can_redo() {
                    self.redo()
                } else {
                    self.repeat_last()
                }
            }
            Key::E => self.flash_fill(),
            Key::Quote => self.copy_from_above(shift),
            Key::B => self.toggle_bold(),
            Key::I => self.toggle_italic(),
            Key::U => self.toggle_underline(),
            Key::A => self.select_all_smart(),
            Key::S if shift => {
                self.save_as();
            }
            Key::S => {
                self.save();
            }
            Key::O => self.confirm_then(Pending::Open(None)),
            Key::N => self.confirm_then(Pending::New),
            // Closes the workbook (single-document window: close it, after the save prompt).
            Key::W | Key::F4 => self.close_workbook(),
            Key::F if shift => self.open_format_cells(2),
            Key::F => {
                self.find.replace = false;
                self.find.focus = true;
                self.dialog = Some(Dialog::Find);
            }
            Key::H => {
                self.find.replace = true;
                self.find.focus = true;
                self.dialog = Some(Dialog::Find);
            }
            Key::G => self.dialog = Some(Dialog::GoTo { text: String::new(), focus: true }),
            Key::D => self.fill_down(),
            Key::OpenBracket => self.select_precedents(),
            Key::CloseBracket => self.select_dependents(),
            Key::R => self.fill_right(),
            Key::L if shift => self.toggle_filter(),
            Key::T | Key::L => self.format_as_table(0x4472C4, 1),
            Key::P => {
                self.commit_edit(false);
                self.backstage = Some(super::chrome::Backstage::Print);
            }
            Key::K => self.open_link_dialog(),
            Key::Semicolon | Key::Colon => self.insert_now(shift || key == Key::Colon),
            Key::Backtick if shift => self.set_num_fmt("General"),
            Key::Backtick => self.show_formulas = !self.show_formulas,
            Key::Minus if shift => self.borders(BorderKind::None),
            Key::Minus => self.delete_cells_prompt(),
            Key::Plus => self.insert_cells_prompt(),
            Key::Equals if shift => self.insert_cells_prompt(),
            Key::Num1 if shift => self.set_num_fmt("#,##0.00"),
            Key::Num1 => self.open_format_cells(0),
            Key::Num2 if shift => self.set_num_fmt("h:mm AM/PM"),
            Key::Num2 => self.toggle_bold(),
            Key::Num3 if shift => self.set_num_fmt("d-mmm-yy"),
            Key::Num3 => self.toggle_italic(),
            Key::Num4 if shift => self.set_num_fmt("\"$\"#,##0.00_);(\"$\"#,##0.00)"),
            Key::Num4 => self.toggle_underline(),
            Key::Num5 if shift => self.set_num_fmt("0%"),
            Key::Num5 => self.toggle_strike(),
            Key::Num6 if shift => self.set_num_fmt("0.00E+00"),
            Key::Num7 if shift => self.borders(BorderKind::Outside),
            Key::Num8 if shift => {
                let rg = self.current_region();
                self.select_range(rg);
            }
            Key::Num8 => self.kb.outline_hidden = !self.kb.outline_hidden,
            Key::Num9 => self.hide_rows(!shift),
            Key::Num0 => self.hide_cols(!shift),
            _ => {}
        }
    }

    /// Ctrl+' copies the formula from the cell above (references unchanged) into the editor;
    /// Ctrl+Shift+" copies the value shown in the cell above.
    fn copy_from_above(&mut self, value: bool) {
        let (r, c) = self.active();
        if r == 0 {
            return;
        }
        let text = if value {
            let sh = self.sheet();
            let style = sh.get(r - 1, c).map_or(0, |x| x.style);
            crate::numfmt::format_value(sh.display_value(r - 1, c), &self.wb.style(style).num_fmt).text
        } else {
            self.edit_text(r - 1, c)
        };
        self.begin_edit(Some(text), EditMode::Edit);
    }

    fn on_key_editing(&mut self, ctx: &egui::Context, key: Key, m: Modifiers) {
        let ctrl = m.ctrl || m.command;
        let ac = self.autocomplete_list();
        let ac_open = !ac.is_empty();
        let Some(e) = &self.edit else { return };
        let (mode, in_bar, can_point, ac_index) = (e.mode, e.in_bar, e.can_point(), e.ac_index);
        macro_rules! ed {
            ($f:expr) => {
                if let Some(e) = &mut self.edit {
                    #[allow(clippy::redundant_closure_call)]
                    ($f)(e);
                }
            };
        }
        match key {
            Key::Enter => {
                if m.alt {
                    ed!(|e: &mut super::Editor| e.insert("\n"));
                    return;
                }
                if self.commit_edit(ctrl) && !ctrl {
                    self.move_in_selection(if m.shift { -1 } else { 1 }, 0);
                }
            }
            Key::Tab => {
                if ac_open && !m.shift {
                    let name = ac[ac_index.min(ac.len() - 1)];
                    ed!(|e: &mut super::Editor| e.accept_function(name));
                    return;
                }
                if self.commit_edit(false) {
                    self.move_in_selection(0, if m.shift { -1 } else { 1 });
                }
            }
            Key::Escape => {
                if ac_open {
                    ed!(|e: &mut super::Editor| e.ac_hidden = true);
                } else {
                    self.cancel_edit();
                }
            }
            Key::ArrowUp | Key::ArrowDown if ac_open && !m.shift => {
                let n = ac.len();
                let down = key == Key::ArrowDown;
                ed!(|e: &mut super::Editor| e.ac_index = if down { (e.ac_index + 1) % n } else { (e.ac_index + n - 1) % n });
            }
            Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight => {
                let (dr, dc) = match key {
                    Key::ArrowUp => (-1, 0),
                    Key::ArrowDown => (1, 0),
                    Key::ArrowLeft => (0, -1),
                    _ => (0, 1),
                };
                if mode == EditMode::Enter && !in_bar {
                    if can_point {
                        self.point_move(dr, dc, m.shift);
                    } else if self.commit_edit(false) {
                        self.move_active(dr, dc, false);
                    }
                } else {
                    ed!(|e: &mut super::Editor| match key {
                        Key::ArrowLeft => e.left(m.shift, ctrl),
                        Key::ArrowRight => e.right(m.shift, ctrl),
                        Key::ArrowUp => e.home(m.shift),
                        _ => e.end(m.shift),
                    });
                }
            }
            Key::Home => ed!(|e: &mut super::Editor| if ctrl { e.move_to(0, m.shift) } else { e.home(m.shift) }),
            Key::End => ed!(|e: &mut super::Editor| if ctrl {
                let n = e.text.len();
                e.move_to(n, m.shift)
            } else {
                e.end(m.shift)
            }),
            Key::Backspace => ed!(|e: &mut super::Editor| e.backspace()),
            Key::Delete => ed!(|e: &mut super::Editor| e.delete()),
            Key::F2 if m.shift => {
                if self.commit_edit(false) {
                    self.edit_note();
                }
            }
            Key::F2 => ed!(|e: &mut super::Editor| e.mode = if e.mode == EditMode::Enter { EditMode::Edit } else { EditMode::Enter }),
            Key::F4 => ed!(|e: &mut super::Editor| e.toggle_absolute()),
            Key::A if ctrl => ed!(|e: &mut super::Editor| e.select_all()),
            Key::Semicolon | Key::Colon if ctrl => self.insert_now(m.shift || key == Key::Colon),
            Key::PageDown | Key::PageUp if ctrl => {
                let j = self.next_visible_sheet(key == Key::PageDown);
                self.switch_sheet(j);
            }
            // Undoes the last typing step and stays in the editor.
            Key::Z if ctrl => ed!(|e: &mut super::Editor| {
                e.undo();
            }),
            _ => {}
        }
        let _ = ctx;
    }
}

/// Letter or digit for a key, used by KeyTips.
fn key_char(k: Key) -> Option<char> {
    let n = k.name();
    let mut it = n.chars();
    match (it.next(), it.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => Some(c.to_ascii_uppercase()),
        _ => None,
    }
}
