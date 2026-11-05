//! Alt key sequences ("KeyTips"): Alt, H, O, I = AutoFit Column Width, etc.

use eframe::egui::{self, Align2, Color32, Pos2, RichText, Vec2};

use super::actions::{BorderKind, PasteMode};
use super::chrome::Backstage;
use super::dialogs::Dialog;
use super::theme;
use super::{App, Pending, RibbonTab};
use crate::model::{HAlign, VAlign};

/// (sequence, description). Sequences follow the familiar ribbon access keys.
pub const KEYTIPS: &[(&str, &str)] = &[
    ("F", "File"),
    ("FP", "Print"),
    ("FA", "Save As"),
    ("FO", "Open"),
    ("FN", "New"),
    ("H", "Home"),
    ("N", "Insert"),
    ("P", "Page Layout"),
    ("M", "Formulas"),
    ("A", "Data"),
    ("R", "Review"),
    ("W", "View"),
    ("Y", "Help"),
    // Home: clipboard
    ("HVP", "Paste"),
    ("HVV", "Paste Values"),
    ("HVF", "Paste Formulas"),
    ("HVR", "Paste Formatting"),
    ("HVT", "Paste Transpose"),
    ("HVS", "Paste Special..."),
    ("HX", "Cut"),
    ("HCC", "Copy"),
    ("HFP", "Format Painter"),
    // Home: font
    ("HFG", "Increase Font Size"),
    ("HFK", "Decrease Font Size"),
    ("H1", "Bold"),
    ("H2", "Italic"),
    ("H3U", "Underline"),
    ("HBO", "Bottom Border"),
    ("HBP", "Top Border"),
    ("HBL", "Left Border"),
    ("HBR", "Right Border"),
    ("HBN", "No Border"),
    ("HBA", "All Borders"),
    ("HBS", "Outside Borders"),
    ("HBT", "Thick Outside Borders"),
    ("HBB", "Bottom Double Border"),
    ("HBH", "Thick Bottom Border"),
    ("HBM", "More Borders..."),
    ("HH", "Fill Color"),
    ("HFC", "Font Color"),
    // Home: alignment
    ("HAT", "Top Align"),
    ("HAM", "Middle Align"),
    ("HAB", "Bottom Align"),
    ("HAL", "Align Left"),
    ("HAC", "Center"),
    ("HAR", "Align Right"),
    ("H5", "Decrease Indent"),
    ("H6", "Increase Indent"),
    ("HW", "Wrap Text"),
    ("HT", "Format as Table"),
    ("HMC", "Merge & Center"),
    ("HMA", "Merge Across"),
    ("HMM", "Merge Cells"),
    ("HMU", "Unmerge Cells"),
    // Home: number
    ("HP", "Percent Style"),
    ("HK", "Comma Style"),
    ("HAN", "Accounting Number Format"),
    ("H0", "Increase Decimal"),
    ("H9", "Decrease Decimal"),
    ("HNM", "More Number Formats..."),
    // Home: cells
    ("HII", "Insert Cells..."),
    ("HIR", "Insert Sheet Rows"),
    ("HIC", "Insert Sheet Columns"),
    ("HIS", "Insert Sheet"),
    ("HDD", "Delete Cells..."),
    ("HDR", "Delete Sheet Rows"),
    ("HDC", "Delete Sheet Columns"),
    ("HDS", "Delete Sheet"),
    ("HOH", "Row Height..."),
    ("HOA", "AutoFit Row Height"),
    ("HOW", "Column Width..."),
    ("HOI", "AutoFit Column Width"),
    ("HOUR", "Hide Rows"),
    ("HOUC", "Hide Columns"),
    ("HOUO", "Unhide Rows"),
    ("HOUL", "Unhide Columns"),
    ("HOR", "Rename Sheet"),
    ("HOE", "Format Cells..."),
    // Home: editing
    ("HUS", "AutoSum"),
    ("HUA", "Average"),
    ("HUC", "Count Numbers"),
    ("HUM", "Max"),
    ("HUI", "Min"),
    ("HUF", "More Functions..."),
    ("HFID", "Fill Down"),
    ("HFIR", "Fill Right"),
    ("HEA", "Clear All"),
    ("HEF", "Clear Formats"),
    ("HEC", "Clear Contents"),
    ("HSS", "Sort A to Z"),
    ("HSO", "Sort Z to A"),
    ("HSU", "Custom Sort..."),
    ("HSF", "Filter"),
    ("HSC", "Clear Filter"),
    ("HFDF", "Find..."),
    ("HFDR", "Replace..."),
    ("HFDG", "Go To..."),
    // Insert
    ("NR", "Insert Sheet Rows"),
    ("NC", "Insert Sheet Columns"),
    ("NS", "Insert Sheet"),
    ("NF", "Insert Function..."),
    ("NI", "Link..."),
    // Formulas
    ("MF", "Insert Function..."),
    ("MUS", "AutoSum"),
    ("MN", "Name Manager"),
    ("MH", "Show Formulas"),
    ("MB", "Calculate Now"),
    // Data
    ("ASA", "Sort A to Z"),
    ("ASD", "Sort Z to A"),
    ("ASS", "Sort..."),
    ("AT", "Filter"),
    ("AC", "Clear Filter"),
    ("AY", "Reapply Filter"),
    ("AE", "Text to Columns..."),
    ("AM", "Remove Duplicates..."),
    ("AGG", "Group"),
    ("AUU", "Ungroup"),
    // Page layout
    ("PVG", "View Gridlines"),
    ("PVH", "View Headings"),
    // View
    ("WFF", "Freeze Panes"),
    ("WFR", "Freeze Top Row"),
    ("WFC", "Freeze First Column"),
    ("WVG", "Gridlines"),
    ("WVF", "Formula Bar"),
    ("WVH", "Headings"),
    ("WQ", "Zoom..."),
    ("WJ", "Zoom 100%"),
    ("WG", "Zoom to Selection"),
    // Review / Help
    ("RS", "Workbook Statistics"),
    ("YK", "Keyboard Shortcuts"),
    // Classic menu sequences that many people still use
    ("ES", "Paste Special..."),
    ("EDR", "Delete Entire Row"),
    ("EDC", "Delete Entire Column"),
    ("OCA", "AutoFit Column Width"),
    ("ORA", "AutoFit Row Height"),
    ("OCW", "Column Width..."),
    ("ORE", "Row Height..."),
    ("IR", "Insert Rows"),
    ("IC", "Insert Columns"),
    ("DS", "Sort..."),
    ("DFF", "Filter"),
];

/// Tab letters shown over the ribbon tab strip when Alt is tapped.
pub const TAB_LETTERS: &[(&str, &str)] =
    &[("File", "F"), ("Home", "H"), ("Insert", "N"), ("Page Layout", "P"), ("Formulas", "M"), ("Data", "A"), ("Review", "R"), ("View", "W"), ("Help", "Y")];

impl App {
    /// Feeds one key into the active KeyTips sequence.
    pub fn keytip_key(&mut self, ctx: &egui::Context, ch: char) {
        let Some(seq) = &mut self.keytips else { return };
        seq.push(ch.to_ascii_uppercase());
        let s = seq.clone();
        // switch ribbon tab on the first letter
        if s.len() == 1 {
            let tab = match s.as_str() {
                "H" => Some(RibbonTab::Home),
                "N" => Some(RibbonTab::Insert),
                "P" => Some(RibbonTab::PageLayout),
                "M" => Some(RibbonTab::Formulas),
                "A" => Some(RibbonTab::Data),
                "R" => Some(RibbonTab::Review),
                "W" => Some(RibbonTab::View),
                "Y" => Some(RibbonTab::Help),
                _ => None,
            };
            if let Some(t) = tab {
                self.tab = t;
                self.ribbon_collapsed = false;
            }
            if s == "F" {
                self.commit_edit(false);
                self.backstage = Some(Backstage::Home);
            }
        }
        let exact = KEYTIPS.iter().any(|(k, _)| *k == s);
        let longer = KEYTIPS.iter().any(|(k, _)| k.len() > s.len() && k.starts_with(&s));
        if exact && !longer {
            self.keytips = None;
            self.run_keytip(ctx, &s);
        } else if !longer {
            self.keytips = None;
            if !exact {
                self.status = Some(format!("No command for Alt, {}", s.chars().map(String::from).collect::<Vec<_>>().join(", ")));
            }
        }
    }

    fn run_keytip(&mut self, ctx: &egui::Context, s: &str) {
        self.commit_edit(false);
        match s {
            "FP" => self.backstage = Some(Backstage::Print),
            "FA" => {
                self.save_as();
            }
            "FO" => self.confirm_then(Pending::Open(None)),
            "FN" => self.confirm_then(Pending::New),
            "HVP" => self.paste_internal(PasteMode::All),
            "HVV" => self.paste_internal(PasteMode::Values),
            "HVF" => self.paste_internal(PasteMode::Formulas),
            "HVR" => self.paste_internal(PasteMode::Formats),
            "HVT" => self.paste_internal(PasteMode::Transpose),
            "HVS" | "ES" => self.dialog = Some(Dialog::PasteSpecial { mode: 0, transpose: false }),
            "HX" => self.copy(ctx, true),
            "HCC" => self.copy(ctx, false),
            "HFP" => self.start_format_painter(),
            "HFG" => self.grow_font(true),
            "HFK" => self.grow_font(false),
            "H1" => self.toggle_bold(),
            "H2" => self.toggle_italic(),
            "H3U" => self.toggle_underline(),
            "HBO" => self.borders(BorderKind::Bottom),
            "HBP" => self.borders(BorderKind::Top),
            "HBL" => self.borders(BorderKind::Left),
            "HBR" => self.borders(BorderKind::Right),
            "HBN" => self.borders(BorderKind::None),
            "HBA" => self.borders(BorderKind::All),
            "HBS" => self.borders(BorderKind::Outside),
            "HBT" => self.borders(BorderKind::ThickOutside),
            "HBB" => self.borders(BorderKind::DoubleBottom),
            "HBH" => self.borders(BorderKind::ThickBottom),
            "HBM" => self.open_format_cells(3),
            "HH" => {
                let c = self.last_fill;
                self.apply_style(move |s| s.fill = Some(c));
            }
            "HFC" => {
                let c = self.last_font_color;
                self.apply_style(move |s| s.color = Some(c));
            }
            "HAT" => self.set_valign(VAlign::Top),
            "HAM" => self.set_valign(VAlign::Center),
            "HAB" => self.set_valign(VAlign::Bottom),
            "HAL" => self.set_halign(HAlign::Left),
            "HAC" => self.set_halign(HAlign::Center),
            "HAR" => self.set_halign(HAlign::Right),
            "H5" => self.indent(false),
            "H6" => self.indent(true),
            "HW" => self.toggle_wrap(),
            "HT" => self.format_as_table(0x4472C4, 1),
            "HMC" => self.merge(0),
            "HMA" => self.merge(1),
            "HMM" => self.merge(2),
            "HMU" => self.merge(3),
            "HP" => self.set_num_fmt("0%"),
            "HK" => self.set_num_fmt("_(* #,##0.00_);_(* \\(#,##0.00\\);_(* \"-\"??_);_(@_)"),
            "HAN" => self.set_num_fmt("_(\"$\"* #,##0.00_);_(\"$\"* \\(#,##0.00\\);_(\"$\"* \"-\"??_);_(@_)"),
            "H0" => self.change_decimals(1),
            "H9" => self.change_decimals(-1),
            "HNM" | "HOE" => self.open_format_cells(0),
            "HII" => self.insert_cells_prompt(),
            "HIR" | "NR" | "IR" => self.insert_rows(),
            "HIC" | "NC" | "IC" => self.insert_cols(),
            "HIS" | "NS" => self.add_sheet(),
            "HDD" => self.delete_cells_prompt(),
            "HDR" | "EDR" => self.delete_rows(),
            "HDC" | "EDC" => self.delete_cols(),
            "HDS" => self.dialog = Some(Dialog::ConfirmDeleteSheet(self.wb.active)),
            "HOH" | "ORE" => self.open_row_height(),
            "HOA" | "ORA" => self.autofit_selection_rows(),
            "HOW" | "OCW" => self.open_col_width(),
            "HOI" | "OCA" => self.autofit_selection_cols(),
            "HOUR" => self.hide_rows(true),
            "HOUC" => self.hide_cols(true),
            "HOUO" => self.hide_rows(false),
            "HOUL" => self.hide_cols(false),
            "HOR" => self.rename_tab = Some((self.wb.active, self.sheet().name.clone())),
            "HUS" | "MUS" => self.autosum("SUM"),
            "HUA" => self.autosum("AVERAGE"),
            "HUC" => self.autosum("COUNT"),
            "HUM" => self.autosum("MAX"),
            "HUI" => self.autosum("MIN"),
            "HUF" | "NF" | "MF" => self.dialog = Some(Dialog::insert_function()),
            "NI" => self.open_link_dialog(),
            "HFID" => self.fill_down(),
            "HFIR" => self.fill_right(),
            "HEA" => self.clear(true, true),
            "HEF" => self.clear(false, true),
            "HEC" => self.clear(true, false),
            "HSS" | "ASA" => self.sort_simple(false),
            "HSO" | "ASD" => self.sort_simple(true),
            "HSU" | "ASS" | "DS" => self.open_sort_dialog(),
            "HSF" | "AT" | "DFF" => self.toggle_filter(),
            "HSC" | "AC" => self.clear_filter(),
            "AY" => self.apply_filter(),
            "AE" => self.dialog = Some(Dialog::TextToColumns { delim: 0, other: String::new() }),
            "AM" => self.open_remove_duplicates(),
            "AGG" => self.outline_group_cmd(false),
            "AUU" => self.outline_group_cmd(true),
            "HFDF" => {
                self.find.replace = false;
                self.find.focus = true;
                self.dialog = Some(Dialog::Find);
            }
            "HFDR" => {
                self.find.replace = true;
                self.find.focus = true;
                self.dialog = Some(Dialog::Find);
            }
            "HFDG" => self.dialog = Some(Dialog::GoTo { text: String::new(), focus: true }),
            "MN" => self.dialog = Some(Dialog::NameManager { name: String::new(), refers: self.sel_abs_ref(), error: None }),
            "MH" => self.show_formulas = !self.show_formulas,
            "MB" => self.recalc_now(),
            "PVG" | "WVG" => {
                let g = !self.sheet().gridlines;
                self.sheet_mut().gridlines = g;
                self.dirty = true;
            }
            "PVH" | "WVH" => self.show_headings = !self.show_headings,
            "WVF" => self.show_formula_bar = !self.show_formula_bar,
            "WFF" => self.freeze(0),
            "WFR" => self.freeze(1),
            "WFC" => self.freeze(2),
            "WQ" => self.dialog = Some(Dialog::Zoom { custom: format!("{}", (self.zoom * 100.0).round()) }),
            "WJ" => self.zoom = 1.0,
            "WG" => self.zoom_to_selection(),
            "RS" => self.dialog = Some(Dialog::Stats),
            "YK" => self.dialog = Some(Dialog::Shortcuts),
            _ => {}
        }
        let _ = Pending::New;
    }

    /// Draws the KeyTips hint panel (available next keys) under the ribbon.
    pub fn keytips_ui(&mut self, ctx: &egui::Context) {
        let Some(seq) = self.keytips.clone() else { return };
        let mut next: Vec<(String, &str)> = KEYTIPS
            .iter()
            .filter(|(k, _)| k.len() > seq.len() && k.starts_with(&seq))
            .map(|(k, l)| (k[seq.len()..].to_string(), *l))
            .collect();
        if seq.is_empty() {
            next.retain(|(k, _)| k.len() == 1);
        }
        let title = if seq.is_empty() {
            "Key Tips: press a letter".to_string()
        } else {
            format!("Alt, {}", seq.chars().map(String::from).collect::<Vec<_>>().join(", "))
        };
        egui::Area::new(egui::Id::new("keytips"))
            .anchor(Align2::LEFT_TOP, Vec2::new(16.0, 132.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).fill(Color32::from_rgb(0xFB, 0xFB, 0xFB)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                    ui.label(RichText::new(title).strong().color(theme::accent_dark()));
                    ui.add_space(4.0);
                    egui::Grid::new("kt").num_columns(4).spacing(Vec2::new(10.0, 2.0)).show(ui, |ui| {
                        for (i, (k, l)) in next.iter().take(60).enumerate() {
                            ui.label(RichText::new(k.as_str()).monospace().strong().background_color(Color32::from_rgb(0xFF, 0xF4, 0xC2)));
                            ui.label(RichText::new(*l).size(12.0));
                            if i % 2 == 1 {
                                ui.end_row();
                            }
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(RichText::new("Esc to go back").size(11.0).color(theme::text_dim()));
                });
            });
    }

    /// Draws the letter badges over the ribbon tabs while KeyTips are active.
    pub fn keytip_badge(&self, ui: &egui::Ui, center_bottom: Pos2, name: &str) {
        if self.keytips.as_deref() != Some("") {
            return;
        }
        if let Some((_, l)) = TAB_LETTERS.iter().find(|(n, _)| *n == name) {
            let r = egui::Rect::from_center_size(center_bottom + Vec2::new(0.0, 6.0), Vec2::new(16.0, 16.0));
            ui.painter().rect_filled(r, 2.0, Color32::from_rgb(0x40, 0x40, 0x40));
            ui.painter().text(r.center(), Align2::CENTER_CENTER, *l, theme::ui_bold(11.0), Color32::WHITE);
        }
    }
}
