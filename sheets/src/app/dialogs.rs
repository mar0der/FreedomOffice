//! Modal dialogs: Format Cells, Find & Replace, Go To, Sort, Insert Function, etc.

use eframe::egui::{self, Align2, Color32, RichText, Vec2};

use super::theme;
use super::{App, Pending};
use crate::model::{col_name, Border, BorderLine, HAlign, Range, Style, VAlign, Value};
use crate::numfmt;

pub struct FormatCells {
    pub tab: usize,
    pub orig: Style,
    pub style: Style,
    pub category: usize,
    pub decimals: u32,
    pub thousands: bool,
    pub symbol: usize,
    pub custom: String,
    pub sample: Value,
    pub line: BorderLine,
}

pub enum Dialog {
    Message { title: String, text: String },
    SavePrompt,
    FormatCells(Box<FormatCells>),
    Find,
    GoTo { text: String, focus: bool },
    InsertCells { delete: bool, choice: u8 },
    Sort { range: Range, header: bool, keys: Vec<(u32, bool)> },
    ColWidth { text: String },
    RowHeight { text: String },
    InsertFunction { search: String, category: String, selected: Option<&'static str> },
    NameManager { name: String, refers: String, error: Option<String> },
    Zoom { custom: String },
    RemoveDuplicates { range: Range, header: bool, cols: Vec<(u32, bool)> },
    TextToColumns { delim: u8, other: String },
    ConfirmDeleteSheet(usize),
    Stats,
    Shortcuts,
}

impl Dialog {
    pub fn insert_function() -> Self {
        Dialog::InsertFunction { search: String::new(), category: "Most Recently Used".into(), selected: Some("SUM") }
    }
}

const CATEGORIES: &[&str] = &["General", "Number", "Currency", "Accounting", "Date", "Time", "Percentage", "Scientific", "Text", "Custom"];
const SYMBOLS: &[(&str, &str, bool)] = &[("$", "\"$\"", true), ("€", "\"€\"", false), ("£", "\"£\"", true), ("¥", "\"¥\"", true), ("None", "", true)];
const DATE_FORMATS: &[&str] = &["m/d/yyyy", "dddd, mmmm d, yyyy", "m/d/yy", "mm/dd/yy", "d-mmm", "d-mmm-yy", "dd-mmm-yy", "mmm-yy", "mmmm-yy", "mmmm d, yyyy", "yyyy-mm-dd", "dd.mm.yyyy", "m/d/yyyy h:mm"];
const TIME_FORMATS: &[&str] = &["h:mm:ss AM/PM", "h:mm AM/PM", "h:mm", "h:mm:ss", "[h]:mm:ss", "mm:ss.0"];
const CUSTOM_FORMATS: &[&str] = &[
    "General", "0", "0.00", "#,##0", "#,##0.00", "#,##0_);(#,##0)", "#,##0_);[Red](#,##0)", "#,##0.00_);(#,##0.00)", "#,##0.00_);[Red](#,##0.00)",
    "\"$\"#,##0_);(\"$\"#,##0)", "\"$\"#,##0.00_);[Red](\"$\"#,##0.00)", "0%", "0.00%", "0.00E+00", "##0.0E+0", "m/d/yyyy", "d-mmm-yy", "d-mmm", "mmm-yy",
    "h:mm AM/PM", "h:mm:ss AM/PM", "h:mm", "h:mm:ss", "m/d/yyyy h:mm", "mm:ss", "[h]:mm:ss", "@",
];

fn build_format(fc: &FormatCells) -> String {
    let dec = if fc.decimals > 0 { format!(".{}", "0".repeat(fc.decimals as usize)) } else { String::new() };
    match CATEGORIES[fc.category] {
        "General" => String::new(),
        "Number" => format!("{}0{}", if fc.thousands { "#,##" } else { "" }, dec),
        "Currency" => {
            let (_, s, prefix) = SYMBOLS[fc.symbol];
            let body = format!("#,##0{dec}");
            if s.is_empty() {
                body
            } else if prefix {
                format!("{s}{body}")
            } else {
                format!("{body} {s}")
            }
        }
        "Accounting" => {
            let (_, s, _) = SYMBOLS[fc.symbol];
            let q = if fc.decimals > 0 { format!(".{}", "?".repeat(fc.decimals as usize)) } else { String::new() };
            format!("_({s}* #,##0{dec}_);_({s}* \\(#,##0{dec}\\);_({s}* \"-\"{q}_);_(@_)")
        }
        "Percentage" => format!("0{dec}%"),
        "Scientific" => format!("0{dec}E+00"),
        "Text" => "@".into(),
        _ => fc.custom.clone(),
    }
}

fn categorize(fmt: &str) -> (usize, u32, bool) {
    let f = fmt.trim();
    let dec = f.split(['%', 'E', '_', ';']).next().and_then(|p| p.split_once('.')).map(|(_, d)| d.chars().take_while(|c| *c == '0').count() as u32).unwrap_or(0);
    let cat = if f.is_empty() || f.eq_ignore_ascii_case("general") {
        0
    } else if f == "@" {
        8
    } else if f.contains('%') {
        6
    } else if f.contains("E+") {
        7
    } else if numfmt::is_date_format(f) {
        if f.contains('d') || f.contains('y') { 4 } else { 5 }
    } else if f.starts_with("_(") || f.starts_with("_-") {
        3
    } else if f.contains('$') || f.contains('€') || f.contains('£') || f.contains('¥') {
        2
    } else if f.chars().all(|c| "0#,.".contains(c)) {
        1
    } else {
        9
    };
    (cat, dec, f.contains(','))
}

fn ok_cancel(ui: &mut egui::Ui) -> (bool, bool) {
    let mut ok = false;
    let mut cancel = false;
    ui.add_space(6.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.add(egui::Button::new("Cancel").min_size(Vec2::new(76.0, 24.0))).clicked() {
            cancel = true;
        }
        if ui.add(egui::Button::new("OK").min_size(Vec2::new(76.0, 24.0)).fill(theme::ACCENT_LIGHT)).clicked() {
            ok = true;
        }
    });
    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        ok = true;
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        cancel = true;
    }
    (ok, cancel)
}

impl App {
    pub fn open_format_cells(&mut self, tab: usize) {
        self.commit_edit(false);
        let st = self.active_style();
        let (cat, dec, th) = categorize(&st.num_fmt);
        let (r, c) = self.active();
        let sample = self.sheet().value(r, c).clone();
        self.dialog = Some(Dialog::FormatCells(Box::new(FormatCells {
            tab,
            orig: st.clone(),
            custom: if st.num_fmt.is_empty() { "General".into() } else { st.num_fmt.clone() },
            style: st,
            category: cat,
            decimals: if cat == 0 { 2 } else { dec },
            thousands: th,
            symbol: 0,
            sample,
            line: BorderLine::Thin,
        })));
    }

    pub fn open_col_width(&mut self) {
        let c = self.sel().c1;
        let px = self.sheet().raw_col_width(c);
        self.dialog = Some(Dialog::ColWidth { text: format!("{:.2}", ((px - 5.0) / 7.0).max(0.0)) });
    }

    pub fn open_row_height(&mut self) {
        let r = self.sel().r1;
        let px = self.sheet().raw_row_height(r);
        self.dialog = Some(Dialog::RowHeight { text: format!("{:.2}", px * 0.75) });
    }

    pub fn open_remove_duplicates(&mut self) {
        let rg = if self.sel().is_single() { self.current_region() } else { self.sel_clipped() };
        let header = matches!(self.sheet().value(rg.r1, rg.c1), Value::Text(_));
        self.dialog = Some(Dialog::RemoveDuplicates { range: rg, header, cols: (rg.c1..=rg.c2).map(|c| (c, true)).collect() });
    }

    fn col_label(&self, c: u32, header: bool, r: u32) -> String {
        if header {
            let t = numfmt::value_to_text(self.sheet().value(r, c));
            if !t.is_empty() {
                return t;
            }
        }
        format!("Column {}", col_name(c))
    }

    pub fn dialogs_ui(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.dialog.take() else { return };
        let mut keep = true;
        let win = |title: &str| egui::Window::new(title).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, -40.0]);
        match &mut d {
            Dialog::Message { title, text } => {
                win(title).show(ctx, |ui| {
                    ui.set_min_width(320.0);
                    ui.add_space(4.0);
                    ui.label(text.as_str());
                    ui.add_space(8.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new("OK").min_size(Vec2::new(76.0, 24.0))).clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape))
                        {
                            keep = false;
                        }
                    });
                });
            }
            Dialog::SavePrompt => {
                win("FreedomOffice Sheets").show(ctx, |ui| {
                    ui.set_min_width(360.0);
                    ui.label(RichText::new(format!("Want to save your changes to '{}'?", self.wb.file_name())).size(15.0));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new("Save").min_size(Vec2::new(90.0, 26.0)).fill(theme::ACCENT_LIGHT)).clicked() {
                            keep = false;
                            if self.save() {
                                if let Some(p) = self.pending.take() {
                                    self.run_pending(p);
                                }
                            } else {
                                self.pending = None;
                            }
                        }
                        if ui.add(egui::Button::new("Don't Save").min_size(Vec2::new(90.0, 26.0))).clicked() {
                            keep = false;
                            self.dirty = false;
                            if let Some(p) = self.pending.take() {
                                self.run_pending(p);
                            }
                        }
                        if ui.add(egui::Button::new("Cancel").min_size(Vec2::new(90.0, 26.0))).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            keep = false;
                            self.pending = None;
                        }
                    });
                });
            }
            Dialog::ConfirmDeleteSheet(i) => {
                let i = *i;
                win("Delete Sheet").show(ctx, |ui| {
                    ui.label(format!("'{}' will be permanently deleted. Do you want to continue?", self.wb.sheets.get(i).map(|s| s.name.as_str()).unwrap_or("")));
                    let (ok, cancel) = ok_cancel(ui);
                    if ok {
                        keep = false;
                        self.delete_sheet(i);
                    } else if cancel {
                        keep = false;
                    }
                });
            }
            Dialog::GoTo { text, focus } => {
                win("Go To").show(ctx, |ui| {
                    ui.set_min_width(300.0);
                    ui.label("Reference:");
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(280.0));
                    if *focus {
                        r.request_focus();
                        *focus = false;
                    }
                    if !self.wb.names.is_empty() {
                        ui.label("Names:");
                        egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                            for (n, _) in &self.wb.names {
                                if ui.selectable_label(false, n).clicked() {
                                    *text = n.clone();
                                }
                            }
                        });
                    }
                    let (ok, cancel) = ok_cancel(ui);
                    if ok {
                        let t = text.clone();
                        if self.go_to(&t) {
                            keep = false;
                        } else {
                            self.status = Some("Reference isn't valid.".into());
                        }
                    } else if cancel {
                        keep = false;
                    }
                });
            }
            Dialog::Find => {
                let title = if self.find.replace { "Find and Replace" } else { "Find" };
                egui::Window::new(title).collapsible(false).resizable(false).default_pos(ctx.content_rect().center() - Vec2::new(200.0, 100.0)).show(ctx, |ui| {
                    ui.set_min_width(400.0);
                    ui.horizontal(|ui| {
                        if ui.selectable_label(!self.find.replace, "Find").clicked() {
                            self.find.replace = false;
                        }
                        if ui.selectable_label(self.find.replace, "Replace").clicked() {
                            self.find.replace = true;
                        }
                    });
                    ui.separator();
                    egui::Grid::new("find_grid").num_columns(2).show(ui, |ui| {
                        ui.label("Find what:");
                        let r = ui.add(egui::TextEdit::singleline(&mut self.find.what).desired_width(280.0));
                        if self.find.focus {
                            r.request_focus();
                            self.find.focus = false;
                        }
                        ui.end_row();
                        if self.find.replace {
                            ui.label("Replace with:");
                            ui.add(egui::TextEdit::singleline(&mut self.find.with).desired_width(280.0));
                            ui.end_row();
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.find.match_case, "Match case");
                        ui.checkbox(&mut self.find.whole, "Match entire cell contents");
                    });
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.find.all_sheets, "Within workbook");
                        ui.checkbox(&mut self.find.in_formulas, "Look in formulas");
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if self.find.replace {
                            if ui.button("Replace All").clicked() {
                                let n = self.replace_all();
                                self.status = Some(format!("All done. We made {n} replacements."));
                            }
                            if ui.button("Replace").clicked() {
                                self.replace_one();
                            }
                        }
                        if ui.button("Find Next").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let back = ui.input(|i| i.modifiers.shift);
                            self.find_next(back);
                        }
                        if ui.button("Close").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            keep = false;
                        }
                    });
                });
            }
            Dialog::InsertCells { delete, choice } => {
                let title = if *delete { "Delete" } else { "Insert" };
                let delete = *delete;
                win(title).show(ctx, |ui| {
                    ui.set_min_width(220.0);
                    ui.label(if delete { "Delete" } else { "Insert" });
                    let opts = if delete {
                        ["Shift cells left", "Shift cells up", "Entire row", "Entire column"]
                    } else {
                        ["Shift cells right", "Shift cells down", "Entire row", "Entire column"]
                    };
                    for (i, o) in opts.iter().enumerate() {
                        ui.radio_value(choice, i as u8, *o);
                    }
                    let (ok, cancel) = ok_cancel(ui);
                    if ok {
                        keep = false;
                        let sel = self.sel();
                        match (*choice, delete) {
                            (0, false) => self.insert_cells(false),
                            (1, false) => self.insert_cells(true),
                            (0, true) => self.delete_cells(false),
                            (1, true) => self.delete_cells(true),
                            (2, _) => {
                                self.sheet_mut().view.sel = Range::new(sel.r1, 0, sel.r2, crate::model::MAX_COLS - 1);
                                if delete { self.delete_rows() } else { self.insert_rows() }
                            }
                            _ => {
                                self.sheet_mut().view.sel = Range::new(0, sel.c1, crate::model::MAX_ROWS - 1, sel.c2);
                                if delete { self.delete_cols() } else { self.insert_cols() }
                            }
                        }
                    } else if cancel {
                        keep = false;
                    }
                });
            }
            Dialog::ColWidth { text } => {
                if let Some(done) = size_dialog(ctx, true, text) {
                    keep = false;
                    if let Some(px) = done {
                        self.set_col_width(px);
                    }
                }
            }
            Dialog::RowHeight { text } => {
                if let Some(done) = size_dialog(ctx, false, text) {
                    keep = false;
                    if let Some(px) = done {
                        self.set_row_height(px);
                    }
                }
            }
            Dialog::Zoom { custom } => {
                win("Zoom").show(ctx, |ui| {
                    ui.label("Magnification");
                    for z in [200, 100, 75, 50, 25] {
                        if ui.radio(custom.trim() == z.to_string(), format!("{z}%")).clicked() {
                            *custom = z.to_string();
                        }
                    }
                    ui.horizontal(|ui| {
                        ui.label("Custom:");
                        ui.add(egui::TextEdit::singleline(custom).desired_width(60.0));
                        ui.label("%");
                    });
                    let (ok, cancel) = ok_cancel(ui);
                    if ok {
                        if let Ok(v) = custom.trim().parse::<f32>() {
                            self.zoom = (v / 100.0).clamp(0.1, 4.0);
                        }
                        keep = false;
                    } else if cancel {
                        keep = false;
                    }
                });
            }
            Dialog::Sort { range, header, keys } => {
                let range = *range;
                win("Sort").show(ctx, |ui| {
                    ui.set_min_width(420.0);
                    ui.checkbox(header, "My data has headers");
                    ui.separator();
                    let hdr = *header;
                    let mut remove = None;
                    for (i, (col, desc)) in keys.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(if i == 0 { "Sort by   " } else { "Then by   " });
                            egui::ComboBox::from_id_salt(("sort_col", i)).width(170.0).selected_text(self.col_label(*col, hdr, range.r1)).show_ui(ui, |ui| {
                                for c in range.c1..=range.c2 {
                                    let l = self.col_label(c, hdr, range.r1);
                                    ui.selectable_value(col, c, l);
                                }
                            });
                            egui::ComboBox::from_id_salt(("sort_ord", i)).width(110.0).selected_text(if *desc { "Largest to Smallest" } else { "Smallest to Largest" }).show_ui(ui, |ui| {
                                ui.selectable_value(desc, false, "Smallest to Largest (A to Z)");
                                ui.selectable_value(desc, true, "Largest to Smallest (Z to A)");
                            });
                            if i > 0 && ui.button("✕").clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove {
                        keys.remove(i);
                    }
                    if keys.len() < 4 && ui.button("+ Add Level").clicked() {
                        keys.push((range.c1, false));
                    }
                    let (ok, cancel) = ok_cancel(ui);
                    if ok {
                        let k = keys.clone();
                        self.sort_range(range, hdr, &k);
                        keep = false;
                    } else if cancel {
                        keep = false;
                    }
                });
            }
            Dialog::RemoveDuplicates { range, header, cols } => {
                let range = *range;
                win("Remove Duplicates").show(ctx, |ui| {
                    ui.set_min_width(320.0);
                    ui.label("To delete duplicate values, select one or more columns that contain duplicates.");
                    ui.checkbox(header, "My data has headers");
                    let hdr = *header;
                    egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                        for (c, on) in cols.iter_mut() {
                            let l = self.col_label(*c, hdr, range.r1);
                            ui.checkbox(on, l);
                        }
                    });
                    let (ok, cancel) = ok_cancel(ui);
                    if ok {
                        let cs: Vec<u32> = cols.iter().filter(|c| c.1).map(|c| c.0).collect();
                        if !cs.is_empty() {
                            let n = self.remove_duplicates(range, hdr, &cs);
                            self.dialog = Some(Dialog::Message {
                                title: "Remove Duplicates".into(),
                                text: format!("{n} duplicate values found and removed."),
                            });
                            return;
                        }
                        keep = false;
                    } else if cancel {
                        keep = false;
                    }
                });
            }
            Dialog::TextToColumns { delim, other } => {
                win("Convert Text to Columns").show(ctx, |ui| {
                    ui.label("Split the selected column by:");
                    for (i, l) in ["Comma", "Semicolon", "Tab", "Space", "Other:"].iter().enumerate() {
                        ui.radio_value(delim, i as u8, *l);
                    }
                    ui.add(egui::TextEdit::singleline(other).desired_width(40.0).char_limit(1));
                    let (ok, cancel) = ok_cancel(ui);
                    if ok {
                        let ch = match *delim {
                            0 => Some(','),
                            1 => Some(';'),
                            2 => Some('\t'),
                            3 => Some(' '),
                            _ => other.chars().next(),
                        };
                        if let Some(ch) = ch {
                            self.text_to_columns(ch);
                        }
                        keep = false;
                    } else if cancel {
                        keep = false;
                    }
                });
            }
            Dialog::InsertFunction { search, category, selected } => {
                win("Insert Function").show(ctx, |ui| {
                    ui.set_min_width(420.0);
                    ui.horizontal(|ui| {
                        ui.label("Search for a function:");
                        ui.add(egui::TextEdit::singleline(search).desired_width(220.0)).request_focus();
                    });
                    ui.horizontal(|ui| {
                        ui.label("Or select a category:");
                        let cats = ["Most Recently Used", "All", "Financial", "Date & Time", "Math & Trig", "Statistical", "Lookup & Reference", "Text", "Logical", "Information"];
                        egui::ComboBox::from_id_salt("fcat").width(200.0).selected_text(category.as_str()).show_ui(ui, |ui| {
                            for c in cats {
                                ui.selectable_value(category, c.to_string(), c);
                            }
                        });
                    });
                    let recent = ["SUM", "AVERAGE", "IF", "VLOOKUP", "XLOOKUP", "COUNT", "MAX", "MIN", "SUMIF", "INDEX", "MATCH", "ROUND"];
                    let q = search.to_uppercase();
                    let list: Vec<&'static str> = crate::formula::function_names()
                        .filter(|(n, c)| {
                            if !q.is_empty() {
                                return n.contains(q.as_str());
                            }
                            match category.as_str() {
                                "All" => true,
                                "Most Recently Used" => recent.contains(n),
                                cat => *c == cat,
                            }
                        })
                        .map(|(n, _)| n)
                        .collect();
                    ui.label("Select a function:");
                    let mut accept = false;
                    egui::Frame::new().stroke(egui::Stroke::new(1.0, theme::BORDER)).show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                            ui.set_width(410.0);
                            for n in &list {
                                let r = ui.selectable_label(*selected == Some(*n), *n);
                                if r.clicked() {
                                    *selected = Some(n);
                                }
                                if r.double_clicked() {
                                    *selected = Some(n);
                                    accept = true;
                                }
                            }
                        });
                    });
                    if let Some(n) = selected {
                        ui.label(RichText::new(format!("{}({})", n, crate::formula::signature(n).unwrap_or(""))).strong());
                    }
                    let (ok, cancel) = ok_cancel(ui);
                    if ok || accept {
                        if let Some(n) = *selected {
                            self.insert_function_text(n);
                        }
                        keep = false;
                    } else if cancel {
                        keep = false;
                    }
                });
            }
            Dialog::NameManager { name, refers, error } => {
                win("Name Manager").show(ctx, |ui| {
                    ui.set_min_width(460.0);
                    let mut del = None;
                    egui::Grid::new("names").striped(true).num_columns(3).show(ui, |ui| {
                        ui.label(RichText::new("Name").strong());
                        ui.label(RichText::new("Refers To").strong());
                        ui.label("");
                        ui.end_row();
                        for (i, (n, f)) in self.wb.names.iter().enumerate() {
                            ui.label(n);
                            ui.label(format!("={f}"));
                            if ui.small_button("Delete").clicked() {
                                del = Some(i);
                            }
                            ui.end_row();
                        }
                    });
                    if let Some(i) = del {
                        self.push_undo(&[super::Scope::All]);
                        self.wb.names.remove(i);
                        self.changed();
                    }
                    ui.separator();
                    ui.label(RichText::new("New name").strong());
                    egui::Grid::new("newname").num_columns(2).show(ui, |ui| {
                        ui.label("Name:");
                        ui.add(egui::TextEdit::singleline(name).desired_width(300.0));
                        ui.end_row();
                        ui.label("Refers to:");
                        ui.add(egui::TextEdit::singleline(refers).desired_width(300.0));
                        ui.end_row();
                    });
                    if let Some(e) = error {
                        ui.colored_label(Color32::from_rgb(0xC0, 0, 0), e.as_str());
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Add").clicked() {
                            let (n, r) = (name.clone(), refers.clone());
                            match self.new_name(&n, &r) {
                                Ok(()) => {
                                    name.clear();
                                    *error = None;
                                }
                                Err(e) => *error = Some(e),
                            }
                        }
                        if ui.button("Close").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            keep = false;
                        }
                    });
                });
            }
            Dialog::Stats => {
                win("Workbook Statistics").show(ctx, |ui| {
                    let sh = self.sheet();
                    let cells: usize = sh.rows.values().map(|r| r.len()).sum();
                    let formulas: usize = sh.rows.values().flat_map(|r| r.values()).filter(|c| c.formula.is_some()).count();
                    let all_cells: usize = self.wb.sheets.iter().map(|s| s.rows.values().map(|r| r.len()).sum::<usize>()).sum();
                    let last = sh.extent().map(|(r, c)| crate::model::cell_name(r, c)).unwrap_or_else(|| "A1".into());
                    egui::Grid::new("stats").num_columns(2).striped(true).show(ui, |ui| {
                        for (k, v) in [
                            ("Current sheet", sh.name.clone()),
                            ("End of sheet", last),
                            ("Cells with data", cells.to_string()),
                            ("Formulas", formulas.to_string()),
                            ("Sheets", self.wb.sheets.len().to_string()),
                            ("Cells with data (workbook)", all_cells.to_string()),
                            ("Defined names", self.wb.names.len().to_string()),
                        ] {
                            ui.label(k);
                            ui.label(v);
                            ui.end_row();
                        }
                    });
                    let (ok, cancel) = ok_cancel(ui);
                    if ok || cancel {
                        keep = false;
                    }
                });
            }
            Dialog::Shortcuts => {
                win("Keyboard Shortcuts").show(ctx, |ui| {
                    egui::ScrollArea::vertical().max_height(460.0).show(ui, |ui| {
                        egui::Grid::new("keys").num_columns(2).striped(true).show(ui, |ui| {
                            for (k, v) in SHORTCUTS {
                                ui.label(RichText::new(*k).strong());
                                ui.label(*v);
                                ui.end_row();
                            }
                        });
                    });
                    let (ok, cancel) = ok_cancel(ui);
                    if ok || cancel {
                        keep = false;
                    }
                });
            }
            Dialog::FormatCells(fc) => {
                win("Format Cells").show(ctx, |ui| {
                    ui.set_min_width(520.0);
                    ui.set_min_height(360.0);
                    ui.horizontal(|ui| {
                        for (i, t) in ["Number", "Alignment", "Font", "Border", "Fill"].iter().enumerate() {
                            if ui.selectable_label(fc.tab == i, *t).clicked() {
                                fc.tab = i;
                            }
                        }
                    });
                    ui.separator();
                    match fc.tab {
                        0 => number_tab(ui, fc),
                        1 => alignment_tab(ui, fc),
                        2 => font_tab(ui, fc),
                        3 => border_tab(ui, fc),
                        _ => fill_tab(ui, fc),
                    }
                    ui.add_space(8.0);
                    let (ok, cancel) = ok_cancel(ui);
                    if ok {
                        fc.style.num_fmt = build_format(fc);
                        if fc.style.num_fmt == "General" {
                            fc.style.num_fmt.clear();
                        }
                        let (o, n) = (fc.orig.clone(), fc.style.clone());
                        self.apply_style(move |s| {
                            if o.num_fmt != n.num_fmt {
                                s.num_fmt = n.num_fmt.clone();
                            }
                            if o.font != n.font {
                                s.font = n.font.clone();
                            }
                            if o.size10 != n.size10 {
                                s.size10 = n.size10;
                            }
                            if o.bold != n.bold {
                                s.bold = n.bold;
                            }
                            if o.italic != n.italic {
                                s.italic = n.italic;
                            }
                            if o.underline != n.underline {
                                s.underline = n.underline;
                            }
                            if o.strike != n.strike {
                                s.strike = n.strike;
                            }
                            if o.color != n.color {
                                s.color = n.color;
                            }
                            if o.fill != n.fill {
                                s.fill = n.fill;
                            }
                            if o.halign != n.halign {
                                s.halign = n.halign;
                            }
                            if o.valign != n.valign {
                                s.valign = n.valign;
                            }
                            if o.wrap != n.wrap {
                                s.wrap = n.wrap;
                            }
                            if o.indent != n.indent {
                                s.indent = n.indent;
                            }
                            if o.border != n.border {
                                s.border = n.border;
                            }
                        });
                        keep = false;
                    } else if cancel {
                        keep = false;
                    }
                });
            }
        }
        if keep && self.dialog.is_none() {
            self.dialog = Some(d);
        }
        if !keep && self.dialog.is_none() {
            ctx.memory_mut(|m| m.request_focus(egui::Id::new("grid")));
        }
        let _ = Pending::New;
    }
}

/// Column width / row height prompt. Returns Some(result) when closed.
fn size_dialog(ctx: &egui::Context, is_col: bool, text: &mut String) -> Option<Option<f32>> {
    let mut out = None;
    egui::Window::new(if is_col { "Column Width" } else { "Row Height" })
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, -40.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(if is_col { "Column width:" } else { "Row height:" });
                ui.add(egui::TextEdit::singleline(text).desired_width(80.0)).request_focus();
            });
            let (ok, cancel) = ok_cancel(ui);
            if ok {
                out = Some(text.trim().parse::<f32>().ok().map(|v| {
                    if is_col {
                        if v <= 0.0 { 0.0 } else { (v * 7.0 + 5.0).round() }
                    } else {
                        (v / 0.75).round()
                    }
                }));
            } else if cancel {
                out = Some(None);
            }
        });
    out
}

fn number_tab(ui: &mut egui::Ui, fc: &mut FormatCells) {
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.label("Category:");
            egui::Frame::new().stroke(egui::Stroke::new(1.0, theme::BORDER)).show(ui, |ui| {
                ui.set_width(120.0);
                for (i, c) in CATEGORIES.iter().enumerate() {
                    if ui.selectable_label(fc.category == i, *c).clicked() {
                        fc.category = i;
                        if *c == "Custom" && fc.custom.is_empty() {
                            fc.custom = "General".into();
                        }
                    }
                }
            });
        });
        ui.add_space(10.0);
        ui.vertical(|ui| {
            let fmt = if CATEGORIES[fc.category] == "Date" || CATEGORIES[fc.category] == "Time" { fc.custom.clone() } else { build_format(fc) };
            let sample = numfmt::format_value(&fc.sample, &fmt).text;
            ui.group(|ui| {
                ui.set_width(340.0);
                ui.label(RichText::new("Sample").size(11.0).color(theme::TEXT_DIM));
                ui.label(if sample.is_empty() { " ".into() } else { sample });
            });
            match CATEGORIES[fc.category] {
                "General" => {
                    ui.label("General format cells have no specific number format.");
                }
                "Number" | "Currency" | "Accounting" | "Percentage" | "Scientific" => {
                    ui.horizontal(|ui| {
                        ui.label("Decimal places:");
                        ui.add(egui::DragValue::new(&mut fc.decimals).range(0..=30));
                    });
                    if CATEGORIES[fc.category] == "Number" {
                        ui.checkbox(&mut fc.thousands, "Use 1000 Separator (,)");
                    }
                    if matches!(CATEGORIES[fc.category], "Currency" | "Accounting") {
                        ui.horizontal(|ui| {
                            ui.label("Symbol:");
                            egui::ComboBox::from_id_salt("sym").selected_text(SYMBOLS[fc.symbol].0).show_ui(ui, |ui| {
                                for (i, s) in SYMBOLS.iter().enumerate() {
                                    ui.selectable_value(&mut fc.symbol, i, s.0);
                                }
                            });
                        });
                    }
                }
                "Date" | "Time" => {
                    ui.label("Type:");
                    let list = if CATEGORIES[fc.category] == "Date" { DATE_FORMATS } else { TIME_FORMATS };
                    egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                        for f in list {
                            let s = numfmt::format_value(&Value::Number(46330.5625), f).text;
                            if ui.selectable_label(fc.custom == *f, s).clicked() {
                                fc.custom = f.to_string();
                            }
                        }
                    });
                }
                "Text" => {
                    ui.label("Text format cells are treated as text even when a number is in the cell.");
                }
                _ => {
                    ui.label("Type:");
                    ui.add(egui::TextEdit::singleline(&mut fc.custom).desired_width(330.0));
                    egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                        for f in CUSTOM_FORMATS {
                            if ui.selectable_label(fc.custom == *f, *f).clicked() {
                                fc.custom = f.to_string();
                            }
                        }
                    });
                }
            }
        });
    });
    // Date/Time categories store their choice in `custom`.
    if matches!(CATEGORIES[fc.category], "Date" | "Time") && !numfmt::is_date_format(&fc.custom) {
        fc.custom = if CATEGORIES[fc.category] == "Date" { DATE_FORMATS[0].into() } else { TIME_FORMATS[0].into() };
    }
}

fn alignment_tab(ui: &mut egui::Ui, fc: &mut FormatCells) {
    egui::Grid::new("align").num_columns(2).spacing(Vec2::new(10.0, 8.0)).show(ui, |ui| {
        ui.label("Horizontal:");
        egui::ComboBox::from_id_salt("ha").selected_text(format!("{:?}", fc.style.halign)).show_ui(ui, |ui| {
            for a in [HAlign::General, HAlign::Left, HAlign::Center, HAlign::Right] {
                ui.selectable_value(&mut fc.style.halign, a, format!("{a:?}"));
            }
        });
        ui.end_row();
        ui.label("Vertical:");
        egui::ComboBox::from_id_salt("va").selected_text(format!("{:?}", fc.style.valign)).show_ui(ui, |ui| {
            for a in [VAlign::Top, VAlign::Center, VAlign::Bottom] {
                ui.selectable_value(&mut fc.style.valign, a, format!("{a:?}"));
            }
        });
        ui.end_row();
        ui.label("Indent:");
        ui.add(egui::DragValue::new(&mut fc.style.indent).range(0..=15));
        ui.end_row();
    });
    ui.add_space(8.0);
    ui.label(RichText::new("Text control").strong());
    ui.checkbox(&mut fc.style.wrap, "Wrap text");
}

fn font_tab(ui: &mut egui::Ui, fc: &mut FormatCells) {
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.label("Font:");
            let cur = fc.style.font.clone().unwrap_or_else(|| theme::DEFAULT_CELL_FONT.into());
            egui::Frame::new().stroke(egui::Stroke::new(1.0, theme::BORDER)).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                    ui.set_width(180.0);
                    for f in theme::FONT_CHOICES {
                        if ui.selectable_label(cur.eq_ignore_ascii_case(f), *f).clicked() {
                            fc.style.font = if f.eq_ignore_ascii_case(theme::DEFAULT_CELL_FONT) { None } else { Some(f.to_string()) };
                        }
                    }
                });
            });
        });
        ui.vertical(|ui| {
            ui.label("Font style:");
            for (l, b, i) in [("Regular", false, false), ("Italic", false, true), ("Bold", true, false), ("Bold Italic", true, true)] {
                if ui.selectable_label(fc.style.bold == b && fc.style.italic == i, l).clicked() {
                    fc.style.bold = b;
                    fc.style.italic = i;
                }
            }
        });
        ui.vertical(|ui| {
            ui.label("Size:");
            let mut s = fc.style.font_size();
            if ui.add(egui::DragValue::new(&mut s).range(1.0..=409.0).speed(0.5)).changed() {
                fc.style.set_font_size(s);
            }
        });
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.checkbox(&mut fc.style.underline, "Underline");
        ui.checkbox(&mut fc.style.strike, "Strikethrough");
    });
    ui.horizontal(|ui| {
        ui.label("Color:");
        let c = fc.style.color.unwrap_or(0);
        let (r, _) = ui.allocate_exact_size(Vec2::new(40.0, 16.0), egui::Sense::hover());
        ui.painter().rect_filled(r, 2.0, theme::rgb(c));
        ui.menu_button("Choose", |ui| {
            if let Some(c) = super::ribbon::color_grid(ui, true) {
                fc.style.color = c;
                ui.close();
            }
        });
    });
}

fn border_tab(ui: &mut egui::Ui, fc: &mut FormatCells) {
    ui.horizontal(|ui| {
        ui.label("Line style:");
        egui::ComboBox::from_id_salt("bl").selected_text(format!("{:?}", fc.line)).show_ui(ui, |ui| {
            for l in [BorderLine::Thin, BorderLine::Medium, BorderLine::Thick, BorderLine::Double, BorderLine::Dashed, BorderLine::Dotted] {
                ui.selectable_value(&mut fc.line, l, format!("{l:?}"));
            }
        });
    });
    ui.horizontal(|ui| {
        if ui.button("None").clicked() {
            fc.style.border = Border::default();
        }
        if ui.button("Outline").clicked() {
            fc.style.border = Border { left: fc.line, right: fc.line, top: fc.line, bottom: fc.line };
        }
    });
    ui.add_space(6.0);
    let l = fc.line;
    let b = &mut fc.style.border;
    let mut toggle = |ui: &mut egui::Ui, name: &str, v: &mut BorderLine| {
        let mut on = *v != BorderLine::None;
        if ui.checkbox(&mut on, name).changed() {
            *v = if on { l } else { BorderLine::None };
        }
    };
    toggle(ui, "Top", &mut b.top);
    toggle(ui, "Bottom", &mut b.bottom);
    toggle(ui, "Left", &mut b.left);
    toggle(ui, "Right", &mut b.right);
    ui.label(RichText::new("Applies to each selected cell.").size(11.0).color(theme::TEXT_DIM));
}

fn fill_tab(ui: &mut egui::Ui, fc: &mut FormatCells) {
    ui.label("Background Color:");
    if let Some(c) = super::ribbon::color_grid(ui, true) {
        fc.style.fill = c;
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Sample:");
        let (r, _) = ui.allocate_exact_size(Vec2::new(160.0, 30.0), egui::Sense::hover());
        ui.painter().rect_filled(r, 0.0, fc.style.fill.map(theme::rgb).unwrap_or(Color32::WHITE));
        ui.painter().rect_stroke(r, 0.0, egui::Stroke::new(1.0, theme::BORDER), egui::StrokeKind::Inside);
    });
}

const SHORTCUTS: &[(&str, &str)] = &[
    ("Arrow keys", "Move one cell"),
    ("Ctrl+Arrow", "Jump to edge of data region"),
    ("Shift+Arrow / Ctrl+Shift+Arrow", "Extend selection"),
    ("Home / Ctrl+Home / Ctrl+End", "Row start / A1 / last used cell"),
    ("PgUp / PgDn, Alt+PgUp / Alt+PgDn", "Scroll a screen up/down, left/right"),
    ("Ctrl+PgUp / Ctrl+PgDn", "Previous / next sheet"),
    ("Enter / Shift+Enter, Tab / Shift+Tab", "Commit and move (cycles inside selection)"),
    ("F2", "Edit cell / toggle Enter-Edit mode"),
    ("Alt+Enter", "New line in cell"),
    ("Ctrl+Enter", "Fill selection with entry"),
    ("F4 (while editing)", "Toggle absolute/relative reference"),
    ("Esc", "Cancel edit / clear copy marquee"),
    ("Ctrl+C / Ctrl+X / Ctrl+V", "Copy / Cut / Paste"),
    ("Ctrl+Z / Ctrl+Y", "Undo / Redo"),
    ("Ctrl+B / I / U / 5", "Bold / Italic / Underline / Strikethrough"),
    ("Ctrl+1", "Format Cells"),
    ("Ctrl+Shift+$ % # @ ! ^ ~", "Currency, Percent, Date, Time, Number, Scientific, General"),
    ("Ctrl+Shift+& / Ctrl+Shift+_", "Outline border / remove borders"),
    ("Ctrl+D / Ctrl+R", "Fill down / right"),
    ("Ctrl+; / Ctrl+Shift+;", "Insert date / time"),
    ("Alt+=", "AutoSum"),
    ("Shift+F3", "Insert Function"),
    ("Ctrl+A", "Select region / all"),
    ("Ctrl+Space / Shift+Space", "Select column / row"),
    ("Ctrl+- / Ctrl++", "Delete / Insert cells"),
    ("Ctrl+9 / Ctrl+0 (+Shift)", "Hide (unhide) rows / columns"),
    ("Ctrl+F / Ctrl+H / Ctrl+G, F5", "Find / Replace / Go To"),
    ("Ctrl+Shift+L", "Toggle AutoFilter"),
    ("Ctrl+`", "Show formulas"),
    ("F9", "Calculate now"),
    ("Shift+F11", "Insert sheet"),
    ("Ctrl+S / F12 / Ctrl+O / Ctrl+N", "Save / Save As / Open / New"),
    ("Ctrl+Mouse wheel", "Zoom"),
];
