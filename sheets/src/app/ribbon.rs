//! The ribbon: tab strip and command groups.

use eframe::egui::{self, text::LayoutJob, Align, Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, TextFormat, Vec2};

use super::actions::{BorderKind, PasteMode};
use super::chrome::Backstage;
use super::dialogs::Dialog;
use super::theme::{self, ic, rgb};
use super::{App, RibbonTab};
use crate::model::{HAlign, VAlign};

const GROUP_H: f32 = 70.0;

pub const NUMBER_FORMATS: &[(&str, &str)] = &[
    ("General", "General"),
    ("Number", "0.00"),
    ("Currency", "\"$\"#,##0.00"),
    ("Accounting", "_(\"$\"* #,##0.00_);_(\"$\"* \\(#,##0.00\\);_(\"$\"* \"-\"??_);_(@_)"),
    ("Short Date", "m/d/yyyy"),
    ("Long Date", "dddd, mmmm d, yyyy"),
    ("Time", "h:mm:ss AM/PM"),
    ("Percentage", "0.00%"),
    ("Scientific", "0.00E+00"),
    ("Text", "@"),
];

const COMMA_STYLE: &str = "_(* #,##0.00_);_(* \\(#,##0.00\\);_(* \"-\"??_);_(@_)";

/// Colour palette grid; returns Some(None) for "automatic / no colour", Some(Some(c)) for a colour.
pub fn color_grid(ui: &mut egui::Ui, allow_none: bool) -> Option<Option<u32>> {
    let mut out = None;
    ui.set_min_width(206.0);
    if allow_none && ui.add(egui::Button::new("No Color / Automatic").frame(false)).clicked() {
        out = Some(None);
    }
    ui.label(RichText::new("Theme Colors").size(11.0).color(theme::TEXT_DIM));
    let sw = |ui: &mut egui::Ui, c: u32| -> bool {
        let (r, resp) = ui.allocate_exact_size(Vec2::splat(17.0), Sense::click());
        ui.painter().rect_filled(r.shrink(1.0), 0.0, rgb(c));
        ui.painter().rect_stroke(
            r.shrink(1.0),
            0.0,
            Stroke::new(1.0, if resp.hovered() { theme::ACCENT } else { Color32::from_gray(200) }),
            egui::StrokeKind::Inside,
        );
        resp.on_hover_text(format!("#{c:06X}")).clicked()
    };
    ui.spacing_mut().item_spacing = Vec2::new(3.0, 1.0);
    ui.horizontal(|ui| {
        for c in theme::PALETTE_BASE {
            if sw(ui, c) {
                out = Some(Some(c));
            }
        }
    });
    ui.add_space(3.0);
    for t in [0.8, 0.6, 0.4, -0.25, -0.5] {
        ui.horizontal(|ui| {
            for c in theme::PALETTE_BASE {
                let base = if c == 0xFFFFFF && t > 0.0 { theme::tint(c, -0.05 - (0.8 - t) * 0.2) } else { c };
                let tc = theme::tint(base, if c == 0x000000 && t < 0.0 { -t } else { t });
                if sw(ui, tc) {
                    out = Some(Some(tc));
                }
            }
        });
    }
    ui.add_space(4.0);
    ui.label(RichText::new("Standard Colors").size(11.0).color(theme::TEXT_DIM));
    ui.horizontal(|ui| {
        for c in theme::PALETTE_STANDARD {
            if sw(ui, c) {
                out = Some(Some(c));
            }
        }
    });
    out
}

fn icon_job(icon: &str, is_icon: bool, label: &str, size: f32, vertical: bool, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let icon_fmt = TextFormat {
        font_id: if is_icon { theme::icon_font(size) } else { theme::ui_font(size) },
        color,
        valign: Align::Center,
        ..Default::default()
    };
    job.append(icon, 0.0, icon_fmt);
    if !label.is_empty() {
        let sep = if vertical { "\n" } else { "  " };
        job.append(
            &format!("{sep}{label}"),
            0.0,
            TextFormat { font_id: theme::ui_font(12.0), color: theme::TEXT, valign: Align::Center, ..Default::default() },
        );
    }
    if vertical {
        job.halign = Align::Center;
        job.wrap.max_width = 62.0;
    }
    job
}

impl App {
    fn ib(&self, code: char, fallback: &'static str, label: &str, size: f32, vertical: bool) -> LayoutJob {
        let (g, is_icon) = self.icon(code, fallback);
        icon_job(&g, is_icon, label, size, vertical, theme::TEXT)
    }

    fn big(&self, ui: &mut egui::Ui, code: char, fallback: &'static str, label: &str, tip: &str) -> egui::Response {
        let job = self.ib(code, fallback, label, 22.0, true);
        ui.add(egui::Button::new(job).min_size(Vec2::new(50.0, GROUP_H - 4.0))).on_hover_text(tip)
    }

    fn small(&self, ui: &mut egui::Ui, code: char, fallback: &'static str, label: &str, tip: &str, checked: bool) -> egui::Response {
        let job = self.ib(code, fallback, label, 14.0, false);
        let mut b = egui::Button::new(job).min_size(Vec2::new(24.0, 22.0));
        if checked {
            b = b.fill(theme::CHECKED);
        }
        ui.add(b).on_hover_text(tip)
    }

    fn text_btn(ui: &mut egui::Ui, text: &str, tip: &str, checked: bool, w: f32) -> egui::Response {
        let mut b = egui::Button::new(RichText::new(text).size(13.0)).min_size(Vec2::new(w, 22.0));
        if checked {
            b = b.fill(theme::CHECKED);
        }
        ui.add(b).on_hover_text(tip)
    }

    fn group(&mut self, ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut Self, &mut egui::Ui)) {
        let resp = ui.vertical(|ui| {
            ui.set_height(GROUP_H + 18.0);
            let w = ui.available_width();
            ui.allocate_ui_with_layout(Vec2::new(w, GROUP_H), egui::Layout::left_to_right(Align::Center), |ui| {
                ui.set_height(GROUP_H);
                add(self, ui);
            });
        });
        let r = resp.response.rect;
        ui.painter().text(Pos2::new(r.center().x, r.bottom() - 8.0), Align2::CENTER_CENTER, label, theme::ui_font(11.0), theme::TEXT_DIM);
        ui.add_space(6.0);
        let x = ui.cursor().left();
        ui.painter().line_segment([Pos2::new(x, r.top() + 6.0), Pos2::new(x, r.bottom() - 4.0)], Stroke::new(1.0, theme::BORDER));
        ui.add_space(7.0);
    }

    fn color_split(&mut self, ui: &mut egui::Ui, fill: bool) {
        let (code, fb, tip) = if fill { (ic::FILL, "▧", "Fill Color") } else { (ic::FONT_COLOR, "A", "Font Color") };
        let cur = if fill { self.last_fill } else { self.last_font_color };
        let job = self.ib(code, fb, "", 14.0, false);
        let r = ui.add(egui::Button::new(job).min_size(Vec2::new(24.0, 22.0))).on_hover_text(tip);
        let bar = Rect::from_min_size(Pos2::new(r.rect.left() + 5.0, r.rect.bottom() - 5.0), Vec2::new(r.rect.width() - 10.0, 3.0));
        ui.painter().rect_filled(bar, 0.0, rgb(cur));
        if r.clicked() {
            if fill { self.apply_style(move |s| s.fill = Some(cur)) } else { self.apply_style(move |s| s.color = Some(cur)) }
        }
        let (dn, di) = self.icon(ic::DOWN, "▾");
        let t = if di { RichText::new(dn).font(theme::icon_font(8.0)) } else { RichText::new(dn).size(10.0) };
        ui.menu_button(t, |ui| {
            if let Some(c) = color_grid(ui, true) {
                if fill {
                    if let Some(c) = c {
                        self.last_fill = c;
                    }
                    self.apply_style(move |s| s.fill = c);
                } else {
                    if let Some(c) = c {
                        self.last_font_color = c;
                    }
                    self.apply_style(move |s| s.color = c);
                }
                ui.close();
            }
        });
    }

    pub fn ribbon_ui(&mut self, ui: &mut egui::Ui) {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        // Tab strip
        let strip = ui.horizontal(|ui| {
            ui.set_height(30.0);
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.add_space(6.0);
            // Quick access toolbar
            let (s, si) = self.icon(ic::SAVE, "💾");
            let (u, ui_) = self.icon(ic::UNDO, "↶");
            let (rd, ri) = self.icon(ic::REDO, "↷");
            let mk = |g: String, i: bool| if i { RichText::new(g).font(theme::icon_font(13.0)) } else { RichText::new(g).size(14.0) };
            if ui.add(egui::Button::new(mk(s, si)).frame(false).min_size(Vec2::new(26.0, 26.0))).on_hover_text("Save (Ctrl+S)").clicked() {
                self.save();
            }
            if ui.add_enabled(self.can_undo(), egui::Button::new(mk(u, ui_)).frame(false).min_size(Vec2::new(26.0, 26.0))).on_hover_text("Undo (Ctrl+Z)").clicked() {
                self.undo();
            }
            if ui.add_enabled(self.can_redo(), egui::Button::new(mk(rd, ri)).frame(false).min_size(Vec2::new(26.0, 26.0))).on_hover_text("Redo (Ctrl+Y)").clicked() {
                self.redo();
            }
            ui.add_space(10.0);
            // File tab
            let file = egui::Button::new(RichText::new("File").size(13.0).color(Color32::WHITE)).fill(theme::ACCENT).min_size(Vec2::new(52.0, 26.0));
            if ui.add(file).clicked() {
                self.commit_edit(false);
                self.backstage = Some(Backstage::Home);
            }
            ui.add_space(4.0);
            for (t, name) in [
                (RibbonTab::Home, "Home"),
                (RibbonTab::Insert, "Insert"),
                (RibbonTab::PageLayout, "Page Layout"),
                (RibbonTab::Formulas, "Formulas"),
                (RibbonTab::Data, "Data"),
                (RibbonTab::Review, "Review"),
                (RibbonTab::View, "View"),
                (RibbonTab::Help, "Help"),
            ] {
                let sel = self.tab == t && !self.ribbon_collapsed;
                let font = if sel { theme::ui_bold(13.0) } else { theme::ui_font(13.0) };
                let g = ui.painter().layout_no_wrap(name.to_string(), font.clone(), theme::TEXT);
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(g.size().x + 20.0, 28.0), Sense::click());
                if resp.hovered() && !sel {
                    ui.painter().rect_filled(rect.shrink(2.0), 3.0, theme::HOVER);
                }
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, name, font, if sel { theme::ACCENT_DARK } else { theme::TEXT });
                if sel {
                    ui.painter().line_segment(
                        [Pos2::new(rect.left() + 8.0, rect.bottom() - 2.0), Pos2::new(rect.right() - 8.0, rect.bottom() - 2.0)],
                        Stroke::new(3.0, theme::ACCENT),
                    );
                }
                if resp.clicked() {
                    if self.tab == t && !self.ribbon_collapsed {
                        // single click on active tab does nothing; double-click collapses
                    }
                    self.tab = t;
                    self.ribbon_collapsed = false;
                }
                if resp.double_clicked() {
                    self.ribbon_collapsed = !self.ribbon_collapsed;
                }
            }
        });
        let _ = strip;
        if self.ribbon_collapsed {
            return;
        }
        let frame = egui::Frame::new()
            .fill(theme::RIBBON)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(6)
            .inner_margin(egui::Margin { left: 6, right: 6, top: 2, bottom: 0 })
            .outer_margin(egui::Margin { left: 6, right: 6, top: 0, bottom: 4 });
        frame.show(ui, |ui| {
            egui::ScrollArea::horizontal().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(2.0, 2.0);
                    match self.tab {
                        RibbonTab::Home => self.home_tab(ui),
                        RibbonTab::Insert => self.insert_tab(ui),
                        RibbonTab::PageLayout => self.page_layout_tab(ui),
                        RibbonTab::Formulas => self.formulas_tab(ui),
                        RibbonTab::Data => self.data_tab(ui),
                        RibbonTab::Review => self.review_tab(ui),
                        RibbonTab::View => self.view_tab(ui),
                        RibbonTab::Help => self.help_tab(ui),
                    }
                });
            });
        });
    }

    fn home_tab(&mut self, ui: &mut egui::Ui) {
        let st = self.active_style();
        self.group(ui, "Clipboard", |app, ui| {
            let job = app.ib(ic::PASTE, "📋", "Paste", 22.0, true);
            ui.menu_button(job, |ui| {
                for (label, m) in [
                    ("Paste", PasteMode::All),
                    ("Values", PasteMode::Values),
                    ("Formulas", PasteMode::Formulas),
                    ("Formatting", PasteMode::Formats),
                    ("Transpose", PasteMode::Transpose),
                ] {
                    if ui.add_enabled(app.clip.is_some(), egui::Button::new(label)).clicked() {
                        app.paste_internal(m);
                        ui.close();
                    }
                }
            });
            ui.vertical(|ui| {
                if app.small(ui, ic::CUT, "✂", "", "Cut (Ctrl+X)", false).clicked() {
                    let ctx = ui.ctx().clone();
                    app.copy(&ctx, true);
                }
                if app.small(ui, ic::COPY, "⧉", "", "Copy (Ctrl+C)", false).clicked() {
                    let ctx = ui.ctx().clone();
                    app.copy(&ctx, false);
                }
                let fp = app.format_painter.is_some();
                if app.small(ui, ic::BRUSH, "🖌", "", "Format Painter", fp).clicked() {
                    if fp { app.format_painter = None } else { app.start_format_painter() }
                }
            });
        });
        self.group(ui, "Font", |app, ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    let cur_font = st.font.clone().unwrap_or_else(|| theme::DEFAULT_CELL_FONT.to_string());
                    egui::ComboBox::from_id_salt("font_name").width(130.0).selected_text(&cur_font).height(400.0).show_ui(ui, |ui| {
                        for f in theme::FONT_CHOICES {
                            if ui.selectable_label(cur_font.eq_ignore_ascii_case(f), *f).clicked() {
                                let v = if f.eq_ignore_ascii_case(theme::DEFAULT_CELL_FONT) { None } else { Some(f.to_string()) };
                                app.apply_style(move |s| s.font = v.clone());
                            }
                        }
                    });
                    let size = st.font_size();
                    let size_txt = if size.fract() == 0.0 { format!("{}", size as i32) } else { format!("{size}") };
                    egui::ComboBox::from_id_salt("font_size").width(44.0).selected_text(size_txt).height(400.0).show_ui(ui, |ui| {
                        for s in [8.0, 9.0, 10.0, 10.5, 11.0, 12.0, 14.0, 16.0, 18.0, 20.0, 22.0, 24.0, 26.0, 28.0, 36.0, 48.0, 72.0] {
                            if ui.selectable_label(size == s, format!("{s}")).clicked() {
                                app.apply_style(move |st| st.set_font_size(s));
                            }
                        }
                    });
                    if app.small(ui, ic::FONT_UP, "A▲", "", "Increase Font Size (Ctrl+Shift+>)", false).clicked() {
                        app.grow_font(true);
                    }
                    if app.small(ui, ic::FONT_DOWN, "A▼", "", "Decrease Font Size (Ctrl+Shift+<)", false).clicked() {
                        app.grow_font(false);
                    }
                });
                ui.horizontal(|ui| {
                    if app.small(ui, ic::BOLD, "B", "", "Bold (Ctrl+B)", st.bold).clicked() {
                        app.toggle_bold();
                    }
                    if app.small(ui, ic::ITALIC, "I", "", "Italic (Ctrl+I)", st.italic).clicked() {
                        app.toggle_italic();
                    }
                    if app.small(ui, ic::UNDERLINE, "U", "", "Underline (Ctrl+U)", st.underline).clicked() {
                        app.toggle_underline();
                    }
                    if Self::text_btn(ui, "ab", "Strikethrough (Ctrl+5)", st.strike, 24.0).clicked() {
                        app.toggle_strike();
                    }
                    ui.add_space(4.0);
                    ui.menu_button(RichText::new("▦").size(15.0), |ui| {
                        ui.set_min_width(190.0);
                        for (label, k) in [
                            ("Bottom Border", BorderKind::Bottom),
                            ("Top Border", BorderKind::Top),
                            ("Left Border", BorderKind::Left),
                            ("Right Border", BorderKind::Right),
                            ("No Border", BorderKind::None),
                            ("All Borders", BorderKind::All),
                            ("Outside Borders", BorderKind::Outside),
                            ("Thick Outside Borders", BorderKind::ThickOutside),
                            ("Bottom Double Border", BorderKind::DoubleBottom),
                            ("Thick Bottom Border", BorderKind::ThickBottom),
                        ] {
                            if ui.button(label).clicked() {
                                app.borders(k);
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("More Borders...").clicked() {
                            app.open_format_cells(3);
                            ui.close();
                        }
                    })
                    .response
                    .on_hover_text("Borders");
                    ui.add_space(2.0);
                    app.color_split(ui, true);
                    app.color_split(ui, false);
                });
            });
        });
        self.group(ui, "Alignment", |app, ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    for (t, v, tip) in [("⭱", VAlign::Top, "Top Align"), ("☰", VAlign::Center, "Middle Align"), ("⭳", VAlign::Bottom, "Bottom Align")] {
                        if Self::text_btn(ui, t, tip, st.valign == v, 24.0).clicked() {
                            app.set_valign(v);
                        }
                    }
                    ui.add_space(4.0);
                    if Self::text_btn(ui, "↵ Wrap Text", "Wrap Text", st.wrap, 90.0).clicked() {
                        app.toggle_wrap();
                    }
                });
                ui.horizontal(|ui| {
                    if app.small(ui, ic::ALIGN_LEFT, "⫷", "", "Align Left", st.halign == HAlign::Left).clicked() {
                        app.set_halign(HAlign::Left);
                    }
                    if app.small(ui, ic::ALIGN_CENTER, "≡", "", "Center", st.halign == HAlign::Center).clicked() {
                        app.set_halign(HAlign::Center);
                    }
                    if app.small(ui, ic::ALIGN_RIGHT, "⫸", "", "Align Right", st.halign == HAlign::Right).clicked() {
                        app.set_halign(HAlign::Right);
                    }
                    ui.add_space(4.0);
                    if Self::text_btn(ui, "⇤", "Decrease Indent", false, 24.0).clicked() {
                        app.indent(false);
                    }
                    if Self::text_btn(ui, "⇥", "Increase Indent", false, 24.0).clicked() {
                        app.indent(true);
                    }
                    ui.add_space(4.0);
                    if Self::text_btn(ui, "⇔ Merge & Center", "Merge & Center", false, 0.0).clicked() {
                        app.merge(0);
                    }
                    ui.menu_button("▾", |ui| {
                        for (l, m) in [("Merge & Center", 0u8), ("Merge Across", 1), ("Merge Cells", 2), ("Unmerge Cells", 3)] {
                            if ui.button(l).clicked() {
                                app.merge(m);
                                ui.close();
                            }
                        }
                    });
                });
            });
        });
        self.group(ui, "Number", |app, ui| {
            ui.vertical(|ui| {
                let cur = NUMBER_FORMATS
                    .iter()
                    .find(|(_, f)| (st.num_fmt.is_empty() && *f == "General") || st.num_fmt == *f)
                    .map(|(n, _)| n.to_string())
                    .unwrap_or_else(|| if st.num_fmt.is_empty() { "General".into() } else { "Custom".into() });
                egui::ComboBox::from_id_salt("num_fmt").width(130.0).selected_text(cur).show_ui(ui, |ui| {
                    let (r, c) = app.active();
                    let v = app.sheet().value(r, c).clone();
                    for (n, f) in NUMBER_FORMATS {
                        let sample = crate::numfmt::format_value(&v, f).text;
                        let label = if sample.is_empty() { n.to_string() } else { format!("{n}    {sample}") };
                        if ui.selectable_label(false, label).clicked() {
                            app.set_num_fmt(f);
                        }
                    }
                    ui.separator();
                    if ui.selectable_label(false, "More Number Formats...").clicked() {
                        app.open_format_cells(0);
                    }
                });
                ui.horizontal(|ui| {
                    ui.menu_button(RichText::new("$").size(14.0), |ui| {
                        for (l, f) in [
                            ("$ English (United States)", NUMBER_FORMATS[3].1),
                            ("€ Euro", "_-* #,##0.00 \"€\"_-;-* #,##0.00 \"€\"_-;_-* \"-\"?? \"€\"_-;_-@_-"),
                            ("£ English (United Kingdom)", "_-\"£\"* #,##0.00_-;-\"£\"* #,##0.00_-;_-\"£\"* \"-\"??_-;_-@_-"),
                            ("¥ Japanese", "_-\"¥\"* #,##0_-;-\"¥\"* #,##0_-;_-\"¥\"* \"-\"_-;_-@_-"),
                            ("CHF Swiss", "_-\"CHF\"* #,##0.00_-;-\"CHF\"* #,##0.00_-;_-\"CHF\"* \"-\"??_-;_-@_-"),
                        ] {
                            if ui.button(l).clicked() {
                                app.set_num_fmt(f);
                                ui.close();
                            }
                        }
                    })
                    .response
                    .on_hover_text("Accounting Number Format");
                    if Self::text_btn(ui, "%", "Percent Style (Ctrl+Shift+%)", false, 24.0).clicked() {
                        app.set_num_fmt("0%");
                    }
                    if Self::text_btn(ui, ",", "Comma Style", false, 24.0).clicked() {
                        app.set_num_fmt(COMMA_STYLE);
                    }
                    if Self::text_btn(ui, "←.0", "Increase Decimal", false, 30.0).clicked() {
                        app.change_decimals(1);
                    }
                    if Self::text_btn(ui, ".00→", "Decrease Decimal", false, 30.0).clicked() {
                        app.change_decimals(-1);
                    }
                });
            });
        });
        self.group(ui, "Styles", |app, ui| {
            let job = app.ib(ic::TABLE, "▤", "Cell\nStyles", 20.0, true);
            ui.menu_button(job, |ui| {
                ui.set_min_width(170.0);
                type F = fn(&mut crate::model::Style);
                let presets: [(&str, F); 10] = [
                    ("Normal", |s| *s = Default::default()),
                    ("Good", |s| {
                        s.fill = Some(0xC6EFCE);
                        s.color = Some(0x006100);
                    }),
                    ("Bad", |s| {
                        s.fill = Some(0xFFC7CE);
                        s.color = Some(0x9C0006);
                    }),
                    ("Neutral", |s| {
                        s.fill = Some(0xFFEB9C);
                        s.color = Some(0x9C5700);
                    }),
                    ("Title", |s| {
                        s.set_font_size(18.0);
                        s.color = Some(0x44546A);
                        s.font = Some("Calibri Light".into());
                    }),
                    ("Heading 1", |s| {
                        s.set_font_size(15.0);
                        s.bold = true;
                        s.color = Some(0x44546A);
                        s.border.bottom = crate::model::BorderLine::Thick;
                    }),
                    ("Heading 2", |s| {
                        s.set_font_size(13.0);
                        s.bold = true;
                        s.color = Some(0x44546A);
                        s.border.bottom = crate::model::BorderLine::Medium;
                    }),
                    ("Total", |s| {
                        s.bold = true;
                        s.border.top = crate::model::BorderLine::Thin;
                        s.border.bottom = crate::model::BorderLine::Double;
                    }),
                    ("Note", |s| {
                        s.fill = Some(0xFFFFCC);
                        s.border = crate::model::Border {
                            left: crate::model::BorderLine::Thin,
                            right: crate::model::BorderLine::Thin,
                            top: crate::model::BorderLine::Thin,
                            bottom: crate::model::BorderLine::Thin,
                        };
                    }),
                    ("Accent 1", |s| {
                        s.fill = Some(0x4472C4);
                        s.color = Some(0xFFFFFF);
                    }),
                ];
                for (name, f) in presets {
                    if ui.button(name).clicked() {
                        app.apply_style(f);
                        ui.close();
                    }
                }
            });
        });
        self.group(ui, "Cells", |app, ui| {
            ui.vertical(|ui| {
                let job = app.ib(ic::ADD, "+", "Insert", 13.0, false);
                ui.menu_button(job, |ui| {
                    ui.set_min_width(180.0);
                    if ui.button("Insert Cells...").clicked() {
                        app.insert_cells_prompt();
                        ui.close();
                    }
                    if ui.button("Insert Sheet Rows").clicked() {
                        app.insert_rows();
                        ui.close();
                    }
                    if ui.button("Insert Sheet Columns").clicked() {
                        app.insert_cols();
                        ui.close();
                    }
                    if ui.button("Insert Sheet").clicked() {
                        app.add_sheet();
                        ui.close();
                    }
                });
                let job = app.ib(ic::DELETE, "−", "Delete", 13.0, false);
                ui.menu_button(job, |ui| {
                    ui.set_min_width(180.0);
                    if ui.button("Delete Cells...").clicked() {
                        app.delete_cells_prompt();
                        ui.close();
                    }
                    if ui.button("Delete Sheet Rows").clicked() {
                        app.delete_rows();
                        ui.close();
                    }
                    if ui.button("Delete Sheet Columns").clicked() {
                        app.delete_cols();
                        ui.close();
                    }
                    if ui.button("Delete Sheet").clicked() {
                        app.dialog = Some(Dialog::ConfirmDeleteSheet(app.wb.active));
                        ui.close();
                    }
                });
                let job = app.ib(ic::SETTINGS, "⚙", "Format", 13.0, false);
                ui.menu_button(job, |ui| app.format_menu(ui));
            });
        });
        self.group(ui, "Editing", |app, ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    if Self::text_btn(ui, "Σ AutoSum", "AutoSum (Alt+=)", false, 0.0).clicked() {
                        app.autosum("SUM");
                    }
                    ui.menu_button("▾", |ui| app.autosum_menu(ui));
                });
                ui.menu_button("⬇ Fill", |ui| {
                    if ui.button("Down (Ctrl+D)").clicked() {
                        app.fill_down();
                        ui.close();
                    }
                    if ui.button("Right (Ctrl+R)").clicked() {
                        app.fill_right();
                        ui.close();
                    }
                });
                let job = app.ib(ic::CLEAR, "⌫", "Clear", 13.0, false);
                ui.menu_button(job, |ui| {
                    ui.set_min_width(150.0);
                    if ui.button("Clear All").clicked() {
                        app.clear(true, true);
                        ui.close();
                    }
                    if ui.button("Clear Formats").clicked() {
                        app.clear(false, true);
                        ui.close();
                    }
                    if ui.button("Clear Contents").clicked() {
                        app.clear(true, false);
                        ui.close();
                    }
                });
            });
            let job = app.ib(ic::SORT, "⇅", "Sort &\nFilter", 20.0, true);
            ui.menu_button(job, |ui| app.sort_menu(ui));
            let job = app.ib(ic::FIND, "🔍", "Find &\nSelect", 20.0, true);
            ui.menu_button(job, |ui| {
                ui.set_min_width(160.0);
                if ui.button("Find... (Ctrl+F)").clicked() {
                    app.find.replace = false;
                    app.find.focus = true;
                    app.dialog = Some(Dialog::Find);
                    ui.close();
                }
                if ui.button("Replace... (Ctrl+H)").clicked() {
                    app.find.replace = true;
                    app.find.focus = true;
                    app.dialog = Some(Dialog::Find);
                    ui.close();
                }
                if ui.button("Go To... (Ctrl+G)").clicked() {
                    app.dialog = Some(Dialog::GoTo { text: String::new(), focus: true });
                    ui.close();
                }
            });
        });
    }

    fn format_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(200.0);
        ui.label(RichText::new("Cell Size").strong());
        if ui.button("Row Height...").clicked() {
            self.open_row_height();
            ui.close();
        }
        if ui.button("AutoFit Row Height").clicked() {
            self.autofit_selection_rows();
            ui.close();
        }
        if ui.button("Column Width...").clicked() {
            self.open_col_width();
            ui.close();
        }
        if ui.button("AutoFit Column Width").clicked() {
            self.autofit_selection_cols();
            ui.close();
        }
        ui.separator();
        ui.label(RichText::new("Visibility").strong());
        ui.menu_button("Hide & Unhide", |ui| {
            if ui.button("Hide Rows").clicked() {
                self.hide_rows(true);
                ui.close();
            }
            if ui.button("Hide Columns").clicked() {
                self.hide_cols(true);
                ui.close();
            }
            if ui.button("Unhide Rows").clicked() {
                self.hide_rows(false);
                ui.close();
            }
            if ui.button("Unhide Columns").clicked() {
                self.hide_cols(false);
                ui.close();
            }
        });
        ui.separator();
        ui.label(RichText::new("Organize Sheets").strong());
        if ui.button("Rename Sheet").clicked() {
            self.rename_tab = Some((self.wb.active, self.sheet().name.clone()));
            ui.close();
        }
        ui.menu_button("Tab Color", |ui| {
            if let Some(c) = color_grid(ui, true) {
                let i = self.wb.active;
                self.push_undo(&[super::Scope::Sheet(i)]);
                self.wb.sheets[i].tab_color = c;
                self.dirty = true;
                ui.close();
            }
        });
        ui.separator();
        if ui.button("Format Cells... (Ctrl+1)").clicked() {
            self.open_format_cells(0);
            ui.close();
        }
    }

    fn autosum_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(150.0);
        for (l, f) in [("Sum", "SUM"), ("Average", "AVERAGE"), ("Count Numbers", "COUNT"), ("Max", "MAX"), ("Min", "MIN")] {
            if ui.button(l).clicked() {
                self.autosum(f);
                ui.close();
            }
        }
        ui.separator();
        if ui.button("More Functions...").clicked() {
            self.dialog = Some(Dialog::insert_function());
            ui.close();
        }
    }

    fn sort_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(170.0);
        if ui.button("Sort A to Z").clicked() {
            self.sort_simple(false);
            ui.close();
        }
        if ui.button("Sort Z to A").clicked() {
            self.sort_simple(true);
            ui.close();
        }
        if ui.button("Custom Sort...").clicked() {
            self.open_sort_dialog();
            ui.close();
        }
        ui.separator();
        let on = self.sheet().filter.is_some();
        if ui.button(if on { "✓ Filter (Ctrl+Shift+L)" } else { "Filter (Ctrl+Shift+L)" }).clicked() {
            self.toggle_filter();
            ui.close();
        }
        if ui.add_enabled(on, egui::Button::new("Clear")).clicked() {
            self.clear_filter();
            ui.close();
        }
        if ui.add_enabled(on, egui::Button::new("Reapply")).clicked() {
            self.apply_filter();
            ui.close();
        }
    }

    fn insert_tab(&mut self, ui: &mut egui::Ui) {
        self.group(ui, "Cells", |app, ui| {
            if app.big(ui, ic::ADD, "+", "Rows", "Insert Sheet Rows").clicked() {
                app.insert_rows();
            }
            if app.big(ui, ic::ADD, "+", "Columns", "Insert Sheet Columns").clicked() {
                app.insert_cols();
            }
            if app.big(ui, ic::NEW, "▤", "Sheet", "Insert Sheet (Shift+F11)").clicked() {
                app.add_sheet();
            }
        });
        self.group(ui, "Functions", |app, ui| {
            let job = icon_job("fx", false, "Function", 20.0, true, theme::TEXT);
            if ui.add(egui::Button::new(job).min_size(Vec2::new(54.0, GROUP_H - 4.0))).on_hover_text("Insert Function (Shift+F3)").clicked() {
                app.dialog = Some(Dialog::insert_function());
            }
        });
        self.group(ui, "Date & Time", |app, ui| {
            ui.vertical(|ui| {
                if ui.button("📅 Today's Date (Ctrl+;)").clicked() {
                    app.insert_now(false);
                }
                if ui.button("🕒 Current Time (Ctrl+Shift+;)").clicked() {
                    app.insert_now(true);
                }
            });
        });
        self.group(ui, "Symbols", |app, ui| {
            let job = icon_job("Ω", false, "Symbol", 22.0, true, theme::TEXT);
            ui.menu_button(job, |ui| {
                ui.set_min_width(240.0);
                let syms = "©®™€£¥¢°±×÷≤≥≠≈∞√πΣµΩαβγΔ→←↑↓↔✓✗•…§¶†‰½¼¾²³";
                egui::Grid::new("syms").spacing(Vec2::new(2.0, 2.0)).show(ui, |ui| {
                    for (i, ch) in syms.chars().enumerate() {
                        if ui.add(egui::Button::new(RichText::new(ch.to_string()).size(16.0)).min_size(Vec2::splat(26.0))).clicked() {
                            match &mut app.edit {
                                Some(e) => e.insert(&ch.to_string()),
                                None => {
                                    app.begin_edit(None, super::EditMode::Edit);
                                    if let Some(e) = &mut app.edit {
                                        e.insert(&ch.to_string());
                                    }
                                }
                            }
                            ui.close();
                        }
                        if i % 8 == 7 {
                            ui.end_row();
                        }
                    }
                });
            });
        });
    }

    fn page_layout_tab(&mut self, ui: &mut egui::Ui) {
        self.group(ui, "Sheet Options", |app, ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Gridlines").strong());
                let mut g = app.sheet().gridlines;
                if ui.checkbox(&mut g, "View").changed() {
                    app.sheet_mut().gridlines = g;
                    app.dirty = true;
                }
            });
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("Headings").strong());
                ui.checkbox(&mut app.show_headings, "View");
            });
        });
        self.group(ui, "Sheet", |app, ui| {
            ui.vertical(|ui| {
                ui.label("Tab color");
                ui.menu_button("Choose...", |ui| {
                    if let Some(c) = color_grid(ui, true) {
                        let i = app.wb.active;
                        app.push_undo(&[super::Scope::Sheet(i)]);
                        app.wb.sheets[i].tab_color = c;
                        app.dirty = true;
                        ui.close();
                    }
                });
            });
        });
    }

    fn formulas_tab(&mut self, ui: &mut egui::Ui) {
        self.group(ui, "Function Library", |app, ui| {
            let job = icon_job("fx", false, "Insert\nFunction", 20.0, true, theme::TEXT);
            if ui.add(egui::Button::new(job).min_size(Vec2::new(54.0, GROUP_H - 4.0))).clicked() {
                app.dialog = Some(Dialog::insert_function());
            }
            let job = icon_job("Σ", false, "AutoSum", 22.0, true, theme::TEXT);
            ui.menu_button(job, |ui| app.autosum_menu(ui));
            for (cat, label) in [
                ("Financial", "Financial"),
                ("Logical", "Logical"),
                ("Text", "Text"),
                ("Date & Time", "Date &\nTime"),
                ("Lookup & Reference", "Lookup &\nReference"),
                ("Math & Trig", "Math &\nTrig"),
                ("Statistical", "More\nFunctions"),
            ] {
                let job = icon_job("▤", false, label, 18.0, true, theme::TEXT);
                ui.menu_button(job, |ui| {
                    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                        ui.set_min_width(170.0);
                        let cats: &[&str] = if cat == "Statistical" { &["Statistical", "Information"] } else { &[cat] };
                        for c in cats {
                            if cats.len() > 1 {
                                ui.label(RichText::new(*c).strong());
                            }
                            for (n, fc) in crate::formula::function_names() {
                                if fc == *c && ui.button(n).on_hover_text(crate::formula::signature(n).unwrap_or("")).clicked() {
                                    app.insert_function_text(n);
                                    ui.close();
                                }
                            }
                        }
                    });
                });
            }
        });
        self.group(ui, "Defined Names", |app, ui| {
            let job = icon_job("🏷", false, "Name\nManager", 18.0, true, theme::TEXT);
            if ui.add(egui::Button::new(job).min_size(Vec2::new(60.0, GROUP_H - 4.0))).on_hover_text("Name Manager (Ctrl+F3)").clicked() {
                app.dialog = Some(Dialog::NameManager { name: String::new(), refers: app.sel_abs_ref(), error: None });
            }
        });
        self.group(ui, "Formula Auditing", |app, ui| {
            if Self::text_btn(ui, "Show Formulas", "Show Formulas (Ctrl+`)", app.show_formulas, 0.0).clicked() {
                app.show_formulas = !app.show_formulas;
            }
        });
        self.group(ui, "Calculation", |app, ui| {
            let (c, ci) = app.icon(ic::CALC, "🖩");
            let job = icon_job(&c, ci, "Calculate\nNow", 20.0, true, theme::TEXT);
            if ui.add(egui::Button::new(job).min_size(Vec2::new(60.0, GROUP_H - 4.0))).on_hover_text("Calculate Now (F9)").clicked() {
                app.recalc_now();
            }
        });
    }

    fn data_tab(&mut self, ui: &mut egui::Ui) {
        self.group(ui, "Get & Transform Data", |app, ui| {
            if app.big(ui, ic::OPEN, "📄", "From\nText/CSV", "Open a text or CSV file").clicked() {
                app.confirm_then(super::Pending::Open(None));
            }
        });
        self.group(ui, "Sort & Filter", |app, ui| {
            ui.vertical(|ui| {
                if Self::text_btn(ui, "A→Z", "Sort A to Z", false, 40.0).clicked() {
                    app.sort_simple(false);
                }
                if Self::text_btn(ui, "Z→A", "Sort Z to A", false, 40.0).clicked() {
                    app.sort_simple(true);
                }
            });
            if app.big(ui, ic::SORT, "⇅", "Sort", "Custom Sort").clicked() {
                app.open_sort_dialog();
            }
            let on = app.sheet().filter.is_some();
            let job = app.ib(ic::FILTER, "⏷", "Filter", 22.0, true);
            let mut b = egui::Button::new(job).min_size(Vec2::new(50.0, GROUP_H - 4.0));
            if on {
                b = b.fill(theme::CHECKED);
            }
            if ui.add(b).on_hover_text("Filter (Ctrl+Shift+L)").clicked() {
                app.toggle_filter();
            }
            ui.vertical(|ui| {
                if ui.add_enabled(on, egui::Button::new("Clear")).clicked() {
                    app.clear_filter();
                }
                if ui.add_enabled(on, egui::Button::new("Reapply")).clicked() {
                    app.apply_filter();
                }
            });
        });
        self.group(ui, "Data Tools", |app, ui| {
            let job = icon_job("⫼", false, "Text to\nColumns", 20.0, true, theme::TEXT);
            if ui.add(egui::Button::new(job).min_size(Vec2::new(60.0, GROUP_H - 4.0))).clicked() {
                app.dialog = Some(Dialog::TextToColumns { delim: 0, other: String::new() });
            }
            let job = icon_job("⧉", false, "Remove\nDuplicates", 20.0, true, theme::TEXT);
            if ui.add(egui::Button::new(job).min_size(Vec2::new(66.0, GROUP_H - 4.0))).clicked() {
                app.open_remove_duplicates();
            }
        });
    }

    fn review_tab(&mut self, ui: &mut egui::Ui) {
        self.group(ui, "Proofing", |app, ui| {
            let job = icon_job("📊", false, "Workbook\nStatistics", 20.0, true, theme::TEXT);
            if ui.add(egui::Button::new(job).min_size(Vec2::new(66.0, GROUP_H - 4.0))).clicked() {
                app.dialog = Some(Dialog::Stats);
            }
        });
    }

    fn view_tab(&mut self, ui: &mut egui::Ui) {
        self.group(ui, "Show", |app, ui| {
            ui.vertical(|ui| {
                let mut g = app.sheet().gridlines;
                if ui.checkbox(&mut g, "Gridlines").changed() {
                    app.sheet_mut().gridlines = g;
                    app.dirty = true;
                }
                ui.checkbox(&mut app.show_formula_bar, "Formula Bar");
                ui.checkbox(&mut app.show_headings, "Headings");
            });
        });
        self.group(ui, "Zoom", |app, ui| {
            if app.big(ui, ic::ZOOM, "🔍", "Zoom", "Zoom").clicked() {
                app.dialog = Some(Dialog::Zoom { custom: format!("{}", (app.zoom * 100.0).round()) });
            }
            let job = icon_job("100", false, "100%", 16.0, true, theme::TEXT);
            if ui.add(egui::Button::new(job).min_size(Vec2::new(50.0, GROUP_H - 4.0))).clicked() {
                app.zoom = 1.0;
            }
            let job = icon_job("⛶", false, "Zoom to\nSelection", 20.0, true, theme::TEXT);
            if ui.add(egui::Button::new(job).min_size(Vec2::new(62.0, GROUP_H - 4.0))).clicked() {
                app.zoom_to_selection();
            }
        });
        self.group(ui, "Window", |app, ui| {
            let job = app.ib(ic::PIN, "❄", "Freeze\nPanes", 20.0, true);
            ui.menu_button(job, |ui| {
                ui.set_min_width(200.0);
                let frozen = app.sheet().freeze != (0, 0);
                if ui.button(if frozen { "Unfreeze Panes" } else { "Freeze Panes" }).clicked() {
                    app.freeze(0);
                    ui.close();
                }
                if ui.button("Freeze Top Row").clicked() {
                    app.freeze(1);
                    ui.close();
                }
                if ui.button("Freeze First Column").clicked() {
                    app.freeze(2);
                    ui.close();
                }
            });
        });
    }

    fn help_tab(&mut self, ui: &mut egui::Ui) {
        self.group(ui, "Help", |app, ui| {
            if app.big(ui, ic::INFO, "ℹ", "About", "About FreedomOffice Sheets").clicked() {
                app.backstage = Some(Backstage::About);
            }
            let job = icon_job("⌨", false, "Keyboard\nShortcuts", 20.0, true, theme::TEXT);
            if ui.add(egui::Button::new(job).min_size(Vec2::new(66.0, GROUP_H - 4.0))).clicked() {
                app.dialog = Some(Dialog::Shortcuts);
            }
        });
    }

    pub fn zoom_to_selection(&mut self) {
        let Some(lay) = &self.layout else { return };
        let sel = self.sel_clipped();
        let sh = self.sheet();
        let w: f32 = (sel.c1..=sel.c2.min(sel.c1 + 200)).map(|c| sh.col_width(c)).sum();
        let h: f32 = (sel.r1..=sel.r2.min(sel.r1 + 500)).map(|r| sh.row_height(r)).sum();
        let z = (lay.cells.width() / w.max(1.0)).min(lay.cells.height() / h.max(1.0)).clamp(0.1, 4.0);
        self.zoom = (z * 20.0).floor() / 20.0;
        let v = &mut self.sheet_mut().view;
        v.top = sel.r1;
        v.left = sel.c1;
    }
}
