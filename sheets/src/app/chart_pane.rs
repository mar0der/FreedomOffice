//! The chart Format pane and the Change Chart Type / Insert Chart, Select
//! Data Source and Move Chart dialogs.

use eframe::egui::{self, Color32, ComboBox, DragValue, RichText, Sense, Stroke, StrokeKind, Vec2};

use super::chart_draw::{draw_chart, series_marker, Elem};
use super::chart_view::subtype_tile;
use super::theme::{self, rgb};
use super::{App, Scope};
use crate::chart::{
    self, Axis, Chart, ChartKind, Custom, Dash, ErrDir, ErrKind, ErrorBars, Grouping, LabelPos, Labels, LegendPos, MarkerKind, SeriesSrc, Subtype,
    TrendKind, Trendline,
};
use crate::model::{col_name, Range, Sheet};

/// Open chart dialog.
pub enum ChartDialog {
    Type(Box<TypeDlg>),
    Data(Box<DataDlg>),
    Move(MoveDlg),
}

pub struct TypeDlg {
    /// The chart as it will look.
    pub work: Chart,
    /// Insert Chart (a new chart) rather than Change Chart Type.
    pub insert: bool,
    /// The Combo page is shown.
    pub combo: bool,
}

pub struct DataDlg {
    work: Chart,
    range_text: String,
    sel: usize,
    edit: Option<SeriesEdit>,
    cats_edit: Option<String>,
    error: Option<String>,
}

struct SeriesEdit {
    /// None = adding a series.
    idx: Option<usize>,
    name: String,
    values: String,
    xs: String,
}

pub struct MoveDlg {
    new_sheet: bool,
    name: String,
    target: usize,
}

/// Series choices on the Combo page: (label, kind, grouping, markers).
const COMBO_CHOICES: [(&str, ChartKind, Grouping, bool); 6] = [
    ("Clustered Column", ChartKind::Column, Grouping::Standard, false),
    ("Stacked Column", ChartKind::Column, Grouping::Stacked, false),
    ("Line", ChartKind::Line, Grouping::Standard, false),
    ("Line with Markers", ChartKind::Line, Grouping::Standard, true),
    ("Area", ChartKind::Area, Grouping::Standard, false),
    ("Stacked Area", ChartKind::Area, Grouping::Stacked, false),
];

const FAMILIES: [(&str, ChartKind); 7] = [
    ("Column", ChartKind::Column),
    ("Line", ChartKind::Line),
    ("Pie", ChartKind::Pie),
    ("Bar", ChartKind::Bar),
    ("Area", ChartKind::Area),
    ("X Y (Scatter)", ChartKind::Scatter),
    ("Radar", ChartKind::Radar),
];

const NUM_FORMATS: [(&str, &str); 11] = [
    ("Linked to source", ""),
    ("General", "General"),
    ("Number", "0"),
    ("Number (0.00)", "0.00"),
    ("Thousands", "#,##0"),
    ("Thousands (0.00)", "#,##0.00"),
    ("Currency", "\"$\"#,##0.00"),
    ("Percentage", "0%"),
    ("Percentage (0.0)", "0.0%"),
    ("Scientific", "0.00E+00"),
    ("Date", "m/d/yyyy"),
];

fn family_of(kind: ChartKind) -> ChartKind {
    if kind == ChartKind::Doughnut { ChartKind::Pie } else { kind }
}

fn combo_choice(ch: &Chart, i: usize) -> usize {
    let kind = ch.series_kind(i);
    let g = ch.series_grouping(i);
    let m = series_marker(ch, i).is_some();
    COMBO_CHOICES.iter().position(|c| c.1 == kind && (c.2 == g || (g == Grouping::Percent && c.2 == Grouping::Stacked)) && (kind != ChartKind::Line || c.3 == m)).unwrap_or(0)
}

/// Applies Combo page choices (one per series) to a chart.
fn set_combo(ch: &mut Chart, choices: &[(usize, bool)]) {
    let Some(&(first, _)) = choices.first() else { return };
    let main = COMBO_CHOICES[first];
    ch.kind = main.1;
    ch.grouping = main.2;
    ch.markers = false;
    ch.lines = false;
    ch.smooth = false;
    (ch.gap, ch.overlap) = if main.1.is_bar() && main.2 != Grouping::Standard { (150, 100) } else { (219, -27) };
    for (i, &(c, sec)) in choices.iter().enumerate() {
        let (_, kind, _, markers) = COMBO_CHOICES[c];
        let f = ch.fmt_mut(i);
        f.kind = (kind != main.1).then_some(kind);
        f.secondary = sec;
        f.marker = if kind == ChartKind::Line { if markers { MarkerKind::Circle } else { MarkerKind::None } } else { MarkerKind::Auto };
    }
}

fn abs_cell(r: u32, c: u32) -> String {
    format!("${}${}", col_name(c), r + 1)
}

fn ref_text(sheet: &str, rg: Range) -> String {
    let body = if rg.is_single() { abs_cell(rg.r1, rg.c1) } else { format!("{}:{}", abs_cell(rg.r1, rg.c1), abs_cell(rg.r2, rg.c2)) };
    format!("={}!{body}", crate::formula::quote_sheet(sheet))
}

/// Parses "=Sheet1!$A$1:$B$4" (the sheet part is optional).
fn parse_ref_text(text: &str, sheet: &str) -> Result<Range, String> {
    let t = text.trim().trim_start_matches('=');
    let (sh, rg) = match t.rfind('!') {
        Some(i) => {
            let s = &t[..i];
            let s = s.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')).map(|x| x.replace("''", "'")).unwrap_or_else(|| s.to_string());
            (s, &t[i + 1..])
        }
        None => (sheet.to_string(), t),
    };
    if !sh.eq_ignore_ascii_case(sheet) {
        return Err(format!("The reference must be on sheet '{sheet}'."));
    }
    Range::parse(rg).ok_or_else(|| "The reference isn't valid.".to_string())
}

/// Colour picker button: returns the new value when the user picked one.
fn color_pick(ui: &mut egui::Ui, id: &str, cur: Option<u32>, auto: u32, auto_label: &str) -> Option<Option<u32>> {
    let mut out = None;
    let shown = cur.unwrap_or(auto);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(46.0, 20.0), Sense::click());
    ui.painter().rect_filled(rect.shrink(3.0), 0.0, rgb(shown));
    ui.painter().rect_stroke(rect, 2.0, Stroke::new(1.0, theme::border()), StrokeKind::Inside);
    let resp = resp.on_hover_text(if cur.is_some() { "Colour" } else { auto_label });
    egui::Popup::menu(&resp).id(egui::Id::new(("chart_color", id))).show(|ui| {
        if ui.selectable_label(cur.is_none(), auto_label).clicked() {
            out = Some(None);
            ui.close();
        }
        ui.label(RichText::new("Theme Colors").size(11.0));
        for t in [0.0, 0.8, 0.6, 0.4, -0.25, -0.5] {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for &c in &theme::PALETTE_BASE {
                    let c = if t == 0.0 { c } else { theme::tint(c, t) };
                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::click());
                    ui.painter().rect_filled(r, 0.0, rgb(c));
                    ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_gray(200)), StrokeKind::Inside);
                    if resp.clicked() {
                        out = Some(Some(c));
                        ui.close();
                    }
                }
            });
        }
        ui.label(RichText::new("Standard Colors").size(11.0));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for &c in &theme::PALETTE_STANDARD {
                let (r, resp) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::click());
                ui.painter().rect_filled(r, 0.0, rgb(c));
                if resp.clicked() {
                    out = Some(Some(c));
                    ui.close();
                }
            }
        });
    });
    out
}

/// "Auto" checkbox with a number box; returns true when changed.
fn opt_num(ui: &mut egui::Ui, label: &str, v: &mut Option<f64>, auto: f64) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut a = v.is_none();
            if ui.checkbox(&mut a, "Auto").changed() {
                *v = if a { None } else { Some(auto) };
                changed = true;
            }
            let mut x = v.unwrap_or(auto);
            let speed = (x.abs() * 0.01).max(0.01);
            if ui.add_enabled(v.is_some(), DragValue::new(&mut x).speed(speed).max_decimals(6)).changed() {
                *v = Some(x);
                changed = true;
            }
        });
    });
    changed
}

fn num_format(ui: &mut egui::Ui, id: &str, fmt: &mut String) {
    ui.horizontal(|ui| {
        ui.label("Category");
        let cur = NUM_FORMATS.iter().find(|(_, f)| f == fmt).map(|(n, _)| *n).unwrap_or("Custom");
        ComboBox::from_id_salt(("chart_numfmt", id)).selected_text(cur).width(150.0).show_ui(ui, |ui| {
            for (n, f) in NUM_FORMATS {
                if ui.selectable_label(fmt == f, n).clicked() {
                    *fmt = f.to_string();
                }
            }
        });
    });
    ui.horizontal(|ui| {
        ui.label("Format Code");
        ui.add(egui::TextEdit::singleline(fmt).desired_width(150.0).hint_text("Linked to source"));
    });
}

fn section(ui: &mut egui::Ui, title: &str, open: bool, body: impl FnOnce(&mut egui::Ui)) {
    egui::CollapsingHeader::new(RichText::new(title).strong()).default_open(open).show(ui, body);
}

fn labels_ui(ui: &mut egui::Ui, id: &str, l: &mut Labels, kind: ChartKind, grouping: Grouping) {
    ui.label("Label Contains");
    ui.checkbox(&mut l.series, "Series Name");
    ui.checkbox(&mut l.category, "Category Name");
    ui.checkbox(&mut l.value, "Value");
    if kind.is_round() {
        ui.checkbox(&mut l.percent, "Percentage");
    }
    let opts = LabelPos::for_kind(kind, grouping);
    if opts.len() > 1 {
        ui.label("Label Position");
        for p in opts {
            ui.radio_value(&mut l.pos, *p, p.label());
        }
    }
    ui.label("Number");
    num_format(ui, id, &mut l.num_fmt);
}

fn axis_ui(ui: &mut egui::Ui, id: &str, ax: &mut Axis, value: bool, horizontal_cat: bool) {
    section(ui, "Axis Options", true, |ui| {
        ui.checkbox(&mut ax.visible, "Show axis");
        if value {
            ui.label(RichText::new("Bounds").size(11.5));
            opt_num(ui, "Minimum", &mut ax.min, 0.0);
            opt_num(ui, "Maximum", &mut ax.max, 100.0);
            ui.label(RichText::new("Units").size(11.5));
            opt_num(ui, "Major", &mut ax.major, 10.0);
            opt_num(ui, "Minor", &mut ax.minor, 2.0);
            ui.horizontal(|ui| {
                let mut on = ax.log_base.is_some();
                if ui.checkbox(&mut on, "Logarithmic scale  Base").changed() {
                    ax.log_base = on.then_some(10.0);
                }
                let mut b = ax.log_base.unwrap_or(10.0);
                if ui.add_enabled(on, DragValue::new(&mut b).range(2.0..=1000.0).speed(0.2)).changed() {
                    ax.log_base = Some(b);
                }
            });
            ui.checkbox(&mut ax.reverse, "Values in reverse order");
        } else {
            ui.checkbox(&mut ax.reverse, if horizontal_cat { "Categories in reverse order" } else { "Categories in reverse order" });
        }
    });
    section(ui, "Axis Title", true, |ui| {
        let mut on = ax.title.is_some();
        if ui.checkbox(&mut on, "Show axis title").changed() {
            ax.title = on.then(String::new);
        }
        if let Some(t) = &mut ax.title {
            ui.add(egui::TextEdit::singleline(t).hint_text("Axis Title").desired_width(220.0));
        }
    });
    section(ui, "Gridlines", true, |ui| {
        ui.checkbox(&mut ax.major_grid, "Major gridlines");
        ui.checkbox(&mut ax.minor_grid, "Minor gridlines");
    });
    if value {
        section(ui, "Number", false, |ui| num_format(ui, id, &mut ax.num_fmt));
    }
}

impl App {
    // ------------------------------------------------------------------
    // Format pane
    // ------------------------------------------------------------------

    /// The right-hand Format pane for the selected chart element.
    pub(super) fn chart_format_pane(&mut self, ctx: &egui::Context) {
        if !self.chart.pane || self.backstage.is_some() {
            return;
        }
        let Some(i) = self.selected_chart() else {
            self.chart.pane = false;
            return;
        };
        let orig = self.sheet().charts[i].clone();
        let data = chart::resolve(&self.wb, &orig);
        let names: Vec<String> = data.series.iter().map(|s| s.name.clone()).collect();
        let mut c = orig.clone();
        let mut elem = self.chart.elem.unwrap_or(Elem::ChartArea);
        if let Elem::Series(k) = elem {
            if k >= names.len() {
                elem = Elem::ChartArea;
            }
        }
        let mut close = false;
        let now = ctx.input(|i| i.time);
        egui::SidePanel::right("chart_format_pane").resizable(true).default_width(300.0).min_width(260.0).frame(egui::Frame::NONE.fill(theme::window()).inner_margin(10.0)).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Format {}", elem.label(&c, &names).split(" \"").next().unwrap_or(""))).size(16.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("✕").on_hover_text("Close").clicked() {
                        close = true;
                    }
                });
            });
            // Element chooser.
            let mut elems = vec![Elem::ChartArea, Elem::PlotArea, Elem::Title, Elem::Legend];
            if c.has_axes() {
                elems.extend([Elem::XAxis, Elem::YAxis]);
                if c.has_secondary(names.len()) {
                    elems.push(Elem::Y2Axis);
                }
                elems.extend([Elem::XTitle, Elem::YTitle]);
            }
            elems.extend((0..names.len()).map(Elem::Series));
            elems.push(Elem::DataLabels);
            if c.has_axes() && c.kind != ChartKind::Scatter && c.kind != ChartKind::Radar {
                elems.push(Elem::DataTable);
            }
            ComboBox::from_id_salt("chart_pane_elem").selected_text(elem.label(&c, &names)).width(ui.available_width() - 4.0).show_ui(ui, |ui| {
                for e in &elems {
                    if ui.selectable_label(*e == elem, e.label(&c, &names)).clicked() {
                        elem = *e;
                    }
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 5.0;
                self.pane_body(ui, &mut c, elem, &names);
            });
        });
        self.chart.elem = Some(elem);
        if close {
            self.chart.pane = false;
        }
        if c != orig {
            self.set_chart_live(now, c);
        }
    }

    fn pane_body(&mut self, ui: &mut egui::Ui, c: &mut Chart, elem: Elem, names: &[String]) {
        let n = names.len();
        let bar_kind = c.kind == ChartKind::Bar;
        match elem {
            Elem::ChartArea => {
                section(ui, "Fill", true, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Color");
                        if let Some(v) = color_pick(ui, "bg", c.bg, 0xFFFFFF, "Automatic (white)") {
                            c.bg = v;
                        }
                    });
                });
                section(ui, "Colors", true, |ui| {
                    for (k, (name, _)) in chart::PALETTES.iter().enumerate() {
                        if ui.radio(c.palette as usize == k, *name).clicked() {
                            c.palette = k as u8;
                        }
                    }
                });
                section(ui, "Size", false, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Width (px)");
                        ui.add(DragValue::new(&mut c.size.0).range(chart::MIN_SIZE..=4000.0));
                        ui.label("Height (px)");
                        ui.add(DragValue::new(&mut c.size.1).range(chart::MIN_SIZE..=4000.0));
                    });
                });
            }
            Elem::PlotArea => {
                section(ui, "Fill", true, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Color");
                        if let Some(v) = color_pick(ui, "plot", c.plot_bg, c.bg.unwrap_or(0xFFFFFF), "No fill") {
                            c.plot_bg = v;
                        }
                    });
                });
            }
            Elem::Title => {
                section(ui, "Title Options", true, |ui| {
                    let mut on = c.title.as_deref() != Some("");
                    if ui.checkbox(&mut on, "Show chart title").changed() {
                        c.title = if on { None } else { Some(String::new()) };
                    }
                    if on {
                        let mut t = c.title.clone().unwrap_or_default();
                        if ui.add(egui::TextEdit::singleline(&mut t).hint_text("Automatic").desired_width(240.0)).changed() {
                            c.title = if t.is_empty() { None } else { Some(t) };
                        }
                    }
                });
            }
            Elem::Legend => {
                section(ui, "Legend Options", true, |ui| {
                    ui.checkbox(&mut c.legend, "Show legend");
                    ui.label("Legend Position");
                    for p in LegendPos::ALL {
                        ui.radio_value(&mut c.legend_pos, p, p.label());
                    }
                });
            }
            Elem::XAxis | Elem::XTitle => {
                let value = c.kind == ChartKind::Scatter;
                axis_ui(ui, "x", &mut c.x_axis, value, !bar_kind);
            }
            Elem::YAxis | Elem::YTitle => axis_ui(ui, "y", &mut c.y_axis, true, false),
            Elem::Y2Axis | Elem::Y2Title => axis_ui(ui, "y2", &mut c.y2_axis, true, false),
            Elem::DataLabels => {
                section(ui, "Label Options", true, |ui| {
                    let (k, g) = (c.kind, c.grouping);
                    labels_ui(ui, "labels", &mut c.labels, k, g);
                    if c.series_fmt.iter().any(|f| f.labels.is_some()) && ui.button("Apply to all series").clicked() {
                        for f in &mut c.series_fmt {
                            f.labels = None;
                        }
                    }
                });
            }
            Elem::DataTable => {
                section(ui, "Data Table Options", true, |ui| {
                    ui.checkbox(&mut c.data_table, "Show data table");
                });
            }
            Elem::Series(k) if k < n => self.series_pane(ui, c, k),
            Elem::Series(_) => {}
        }
    }

    fn series_pane(&mut self, ui: &mut egui::Ui, c: &mut Chart, k: usize) {
        let kind = c.series_kind(k);
        let round = c.kind.is_round();
        section(ui, "Series Options", true, |ui| {
            if c.has_axes() && c.kind != ChartKind::Radar {
                ui.label("Plot Series On");
                let mut sec = c.fmt(k).secondary;
                ui.horizontal(|ui| {
                    ui.radio_value(&mut sec, false, "Primary Axis");
                    ui.radio_value(&mut sec, true, "Secondary Axis");
                });
                if sec != c.fmt(k).secondary {
                    c.fmt_mut(k).secondary = sec;
                    if sec && c.y2_axis.title.is_none() && c.y_axis.title.is_some() {
                        c.y2_axis.title = Some(String::new());
                    }
                }
            }
            if c.kind.combinable() {
                ui.horizontal(|ui| {
                    ui.label("Chart type");
                    ComboBox::from_id_salt("series_kind").selected_text(kind.label()).show_ui(ui, |ui| {
                        for kk in [ChartKind::Column, ChartKind::Line, ChartKind::Area] {
                            if ui.selectable_label(kk == kind, kk.label()).clicked() {
                                let main = c.kind;
                                c.fmt_mut(k).kind = (kk != main).then_some(kk);
                            }
                        }
                    });
                });
            }
            if kind.is_bar() {
                ui.horizontal(|ui| {
                    ui.label("Series Overlap (%)");
                    ui.add(DragValue::new(&mut c.overlap).range(-100..=100));
                });
                ui.horizontal(|ui| {
                    ui.label("Gap Width (%)");
                    ui.add(DragValue::new(&mut c.gap).range(0..=500));
                });
            }
            if round {
                ui.horizontal(|ui| {
                    ui.label("Angle of first slice (°)");
                    ui.add(DragValue::new(&mut c.first_angle).range(0..=360));
                });
                if c.kind == ChartKind::Doughnut {
                    ui.horizontal(|ui| {
                        ui.label("Doughnut Hole Size (%)");
                        ui.add(DragValue::new(&mut c.hole).range(10..=90));
                    });
                }
                ui.horizontal(|ui| {
                    ui.label("Pie Explosion (%)");
                    ui.add(DragValue::new(&mut c.fmt_mut(k).explode).range(0..=400));
                });
            }
            if matches!(kind, ChartKind::Line | ChartKind::Scatter) {
                ui.checkbox(&mut c.smooth, "Smoothed line");
            }
        });
        if round {
            let cats: Vec<String> = chart::resolve(&self.wb, c).categories;
            section(ui, "Point (slice)", true, |ui| {
                let id = egui::Id::new("chart_pane_point");
                let mut pi: usize = ui.data(|d| d.get_temp(id)).unwrap_or(0).min(cats.len().saturating_sub(1));
                ComboBox::from_id_salt("pane_point").selected_text(cats.get(pi).cloned().unwrap_or_default()).show_ui(ui, |ui| {
                    for (j, name) in cats.iter().enumerate() {
                        ui.selectable_value(&mut pi, j, name);
                    }
                });
                ui.data_mut(|d| d.insert_temp(id, pi));
                ui.horizontal(|ui| {
                    ui.label("Point Explosion (%)");
                    let mut e = c.fmt(k).point(pi).map(|p| p.explode).unwrap_or(0);
                    if ui.add(DragValue::new(&mut e).range(0..=400)).changed() {
                        c.fmt_mut(k).point_mut(pi).explode = e;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Fill");
                    let cur = c.fmt(k).point(pi).and_then(|p| p.fill);
                    if let Some(v) = color_pick(ui, "point", cur, c.color(pi), "Automatic") {
                        c.fmt_mut(k).point_mut(pi).fill = v;
                    }
                });
                c.fmt_mut(k).points.retain(|p| p.fill.is_some() || p.explode > 0);
            });
        }
        if !round {
            section(ui, "Fill & Line", true, |ui| {
                if !matches!(kind, ChartKind::Line | ChartKind::Scatter) || series_marker(c, k).is_some() {
                    ui.horizontal(|ui| {
                        ui.label(if matches!(kind, ChartKind::Line | ChartKind::Scatter) { "Marker fill" } else { "Fill" });
                        let cur = c.fmt(k).fill;
                        if let Some(v) = color_pick(ui, "sfill", cur, c.color(k), "Automatic") {
                            c.fmt_mut(k).fill = v;
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.label(if kind.is_bar() || kind == ChartKind::Area { "Border" } else { "Line" });
                    let cur = c.fmt(k).line;
                    let auto = c.series_line(k);
                    if let Some(v) = color_pick(ui, "sline", cur, auto, "Automatic") {
                        c.fmt_mut(k).line = v;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Width (pt)");
                    let mut w = c.fmt(k).width.unwrap_or(kind.default_width());
                    if ui.add(DragValue::new(&mut w).range(0.25..=20.0).speed(0.05).max_decimals(2)).changed() {
                        c.fmt_mut(k).width = Some(w);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Dash type");
                    let cur = c.fmt(k).dash;
                    ComboBox::from_id_salt("pane_dash").selected_text(cur.label()).show_ui(ui, |ui| {
                        for d in Dash::ALL {
                            if ui.selectable_label(d == cur, d.label()).clicked() {
                                c.fmt_mut(k).dash = d;
                            }
                        }
                    });
                });
                if matches!(kind, ChartKind::Line | ChartKind::Scatter | ChartKind::Radar) {
                    ui.horizontal(|ui| {
                        ui.label("Marker");
                        let cur = c.fmt(k).marker;
                        ComboBox::from_id_salt("pane_marker").selected_text(cur.label()).show_ui(ui, |ui| {
                            for m in MarkerKind::ALL {
                                if ui.selectable_label(m == cur, m.label()).clicked() {
                                    c.fmt_mut(k).marker = m;
                                }
                            }
                        });
                        ui.label("Size");
                        let mut s = c.fmt(k).marker_size.max(if c.fmt(k).marker_size == 0 { 5 } else { 2 });
                        if ui.add(DragValue::new(&mut s).range(2..=72)).changed() {
                            c.fmt_mut(k).marker_size = s;
                        }
                    });
                }
            });
        }
        section(ui, "Data Labels", false, |ui| {
            let mut own = c.fmt(k).labels.is_some();
            if ui.checkbox(&mut own, "Format this series separately").changed() {
                c.fmt_mut(k).labels = own.then(|| c.labels.clone());
            }
            if own {
                let (kk, g) = (kind, c.series_grouping(k));
                if let Some(l) = &mut c.fmt_mut(k).labels {
                    labels_ui(ui, "series_labels", l, kk, g);
                }
            }
        });
        if c.has_axes() && c.kind != ChartKind::Radar {
            section(ui, "Trendline", false, |ui| {
                let mut on = c.fmt(k).trend.is_some();
                if ui.checkbox(&mut on, "Add trendline").changed() {
                    c.fmt_mut(k).trend = on.then(Trendline::default);
                }
                if let Some(t) = &mut c.fmt_mut(k).trend {
                    for tk in TrendKind::ALL {
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut t.kind, tk, tk.label());
                            if tk == TrendKind::Polynomial && t.kind == tk {
                                ui.label("Order");
                                ui.add(DragValue::new(&mut t.order).range(2..=6));
                            }
                            if tk == TrendKind::MovingAverage && t.kind == tk {
                                ui.label("Period");
                                ui.add(DragValue::new(&mut t.period).range(2..=255));
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        ui.label("Forecast forward");
                        ui.add(DragValue::new(&mut t.forward).range(0.0..=1e9).speed(0.1));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Forecast backward");
                        ui.add(DragValue::new(&mut t.backward).range(0.0..=1e9).speed(0.1));
                    });
                    if matches!(t.kind, TrendKind::Linear | TrendKind::Polynomial | TrendKind::Exponential) {
                        let auto = if t.kind == TrendKind::Exponential { 1.0 } else { 0.0 };
                        let mut iv = t.intercept;
                        if opt_num(ui, "Set intercept", &mut iv, auto) {
                            t.intercept = iv;
                        }
                    }
                    if t.kind != TrendKind::MovingAverage {
                        ui.checkbox(&mut t.show_eq, "Display Equation on chart");
                        ui.checkbox(&mut t.show_r2, "Display R-squared value on chart");
                    }
                    let mut custom = t.name.is_some();
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut custom, "Custom name").changed() {
                            t.name = custom.then(String::new);
                        }
                        if let Some(nm) = &mut t.name {
                            ui.text_edit_singleline(nm);
                        }
                    });
                }
            });
            section(ui, "Error Bars", false, |ui| {
                let mut on = c.fmt(k).err.is_some();
                if ui.checkbox(&mut on, "Add error bars").changed() {
                    c.fmt_mut(k).err = on.then(ErrorBars::default);
                }
                if let Some(e) = &mut c.fmt_mut(k).err {
                    ui.label("Direction");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut e.dir, ErrDir::Both, "Both");
                        ui.radio_value(&mut e.dir, ErrDir::Minus, "Minus");
                        ui.radio_value(&mut e.dir, ErrDir::Plus, "Plus");
                    });
                    ui.checkbox(&mut e.cap, "End cap");
                    ui.label("Error Amount");
                    for ek in ErrKind::ALL {
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut e.kind, ek, ek.label());
                            if e.kind == ek && ek != ErrKind::StdErr {
                                ui.add(DragValue::new(&mut e.amount).range(0.0..=1e12).speed(0.1).max_decimals(4));
                            }
                        });
                    }
                }
            });
        }
    }

    // ------------------------------------------------------------------
    // Dialog openers
    // ------------------------------------------------------------------

    pub(super) fn open_change_type_dialog(&mut self) {
        let Some(i) = self.selected_chart() else { return };
        let work = self.sheet().charts[i].clone();
        let n = chart::resolve(&self.wb, &work).series.len();
        let combo = (0..n).any(|k| work.series_kind(k) != work.kind);
        self.chart.dialog = Some(ChartDialog::Type(Box::new(TypeDlg { work, insert: false, combo })));
    }

    pub(super) fn open_insert_chart_dialog(&mut self, kind: Option<ChartKind>) {
        if !self.commit_edit(false) {
            return;
        }
        let si = self.wb.active;
        let rg = self.chart_source_range();
        let st = chart::subtypes_of(kind.unwrap_or(ChartKind::Column)).next().unwrap_or(&chart::SUBTYPES[0]);
        let work = Chart::with_subtype(&self.wb, si, rg, st);
        if chart::resolve(&self.wb, &work).series.iter().all(|s| s.values.iter().all(Option::is_none)) {
            self.status = Some("Select a range that contains numbers, then insert a chart.".into());
            return;
        }
        self.chart.dialog = Some(ChartDialog::Type(Box::new(TypeDlg { work, insert: true, combo: false })));
    }

    pub(super) fn open_select_data_dialog(&mut self) {
        let Some(i) = self.selected_chart() else { return };
        let work = self.sheet().charts[i].clone();
        let range_text = if work.custom.is_some() { String::new() } else { ref_text(&work.sheet, work.range) };
        self.chart.dialog = Some(ChartDialog::Data(Box::new(DataDlg { work, range_text, sel: 0, edit: None, cats_edit: None, error: None })));
    }

    pub(super) fn open_move_chart_dialog(&mut self) {
        if self.selected_chart().is_none() {
            return;
        }
        let name = self.wb.unique_sheet_name("Chart");
        self.chart.dialog = Some(ChartDialog::Move(MoveDlg { new_sheet: false, name, target: self.wb.active }));
    }

    // ------------------------------------------------------------------
    // Dialogs
    // ------------------------------------------------------------------

    pub(super) fn chart_dialogs(&mut self, ctx: &egui::Context) {
        let Some(dlg) = self.chart.dialog.take() else { return };
        let keep = match dlg {
            ChartDialog::Type(mut d) => {
                let r = self.type_dialog(ctx, &mut d);
                r.then_some(ChartDialog::Type(d))
            }
            ChartDialog::Data(mut d) => {
                let r = self.data_dialog(ctx, &mut d);
                r.then_some(ChartDialog::Data(d))
            }
            ChartDialog::Move(mut d) => {
                let r = self.move_dialog(ctx, &mut d);
                r.then_some(ChartDialog::Move(d))
            }
        };
        if self.chart.dialog.is_none() {
            self.chart.dialog = keep;
        }
    }

    /// OK / Cancel row; returns (ok, cancel).
    fn ok_cancel(ui: &mut egui::Ui) -> (bool, bool) {
        let mut res = (false, false);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            res.1 = ui.add(egui::Button::new("Cancel").min_size(Vec2::new(76.0, 24.0))).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
            res.0 = ui.add(egui::Button::new("OK").min_size(Vec2::new(76.0, 24.0)).fill(theme::accent_light())).clicked();
        });
        res
    }

    fn type_dialog(&mut self, ctx: &egui::Context, d: &mut TypeDlg) -> bool {
        let data = chart::resolve(&self.wb, &d.work);
        let n = data.series.len();
        let mut open = true;
        let (mut ok, mut cancel) = (false, false);
        let title = if d.insert { "Insert Chart" } else { "Change Chart Type" };
        egui::Window::new(title).collapsible(false).resizable(false).fixed_size(Vec2::new(760.0, 500.0)).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(130.0);
                    for (name, k) in FAMILIES {
                        let sel = !d.combo && family_of(d.work.kind) == k;
                        if ui.selectable_label(sel, name).clicked() {
                            d.combo = false;
                            if let Some(st) = chart::subtypes_of(k).next() {
                                d.work.apply_subtype(st);
                            }
                        }
                    }
                    if n >= 2 && ui.selectable_label(d.combo, "Combo").clicked() && !d.combo {
                        d.combo = true;
                        let choices: Vec<(usize, bool)> = (0..n).map(|k| if k + 1 == n { (2, false) } else { (0, false) }).collect();
                        set_combo(&mut d.work, &choices);
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    let fam = family_of(d.work.kind);
                    ui.horizontal(|ui| {
                        let list: Vec<&Subtype> = if d.combo {
                            chart::combo_subtypes().collect()
                        } else if fam == ChartKind::Pie {
                            chart::subtypes_of(ChartKind::Pie).chain(chart::subtypes_of(ChartKind::Doughnut)).collect()
                        } else {
                            chart::subtypes_of(fam).collect()
                        };
                        let cur = d.work.subtype().copied();
                        for st in list {
                            if subtype_tile(ui, st, cur.as_ref() == Some(st)).clicked() {
                                if st.combo != 0 {
                                    let choices: Vec<(usize, bool)> = (0..n)
                                        .map(|k| match st.combo {
                                            3 => (if k == 0 { 5 } else { 0 }, false),
                                            c => if k + 1 == n { (2, c == 2) } else { (0, false) },
                                        })
                                        .collect();
                                    set_combo(&mut d.work, &choices);
                                } else {
                                    d.work.apply_subtype(st);
                                }
                            }
                        }
                    });
                    let name = if d.combo { "Custom Combination".to_string() } else { d.work.subtype().map(|s| s.name.to_string()).unwrap_or_default() };
                    ui.label(RichText::new(name).size(14.0));
                    let (r, _) = ui.allocate_exact_size(Vec2::new(590.0, if d.combo { 240.0 } else { 340.0 }), Sense::hover());
                    let mut preview = d.work.clone();
                    preview.size = (r.width(), r.height());
                    draw_chart(&ui.painter().with_clip_rect(r), r, &preview, &data, 1.0);
                    if d.combo {
                        ui.label("Choose the chart type and axis for your data series:");
                        egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                            egui::Grid::new("combo_grid").num_columns(3).spacing(Vec2::new(12.0, 4.0)).show(ui, |ui| {
                                ui.label(RichText::new("Series Name").strong());
                                ui.label(RichText::new("Chart Type").strong());
                                ui.label(RichText::new("Secondary Axis").strong());
                                ui.end_row();
                                let mut choices: Vec<(usize, bool)> = (0..n).map(|k| (combo_choice(&d.work, k), d.work.on_secondary(k))).collect();
                                let before = choices.clone();
                                for (k, s) in data.series.iter().enumerate() {
                                    ui.label(&s.name);
                                    ComboBox::from_id_salt(("combo_kind", k)).selected_text(COMBO_CHOICES[choices[k].0].0).width(170.0).show_ui(ui, |ui| {
                                        for (j, c) in COMBO_CHOICES.iter().enumerate() {
                                            ui.selectable_value(&mut choices[k].0, j, c.0);
                                        }
                                    });
                                    ui.checkbox(&mut choices[k].1, "");
                                    ui.end_row();
                                }
                                if choices != before {
                                    set_combo(&mut d.work, &choices);
                                }
                            });
                        });
                    }
                });
            });
            ui.separator();
            (ok, cancel) = Self::ok_cancel(ui);
        });
        if ok {
            let work = d.work.clone();
            if d.insert {
                self.insert_chart_value(work);
            } else {
                self.edit_chart(|c| *c = work);
            }
            return false;
        }
        open && !cancel
    }

    fn data_dialog(&mut self, ctx: &egui::Context, d: &mut DataDlg) -> bool {
        let data = chart::resolve(&self.wb, &d.work);
        let sheet = d.work.sheet.clone();
        let xy = d.work.kind == ChartKind::Scatter;
        let mut open = true;
        let (mut ok, mut cancel) = (false, false);
        egui::Window::new("Select Data Source").collapsible(false).resizable(false).fixed_size(Vec2::new(620.0, 420.0)).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Chart data range:");
                let resp = ui.add(egui::TextEdit::singleline(&mut d.range_text).desired_width(380.0).hint_text("(series defined individually)"));
                if resp.lost_focus() && !d.range_text.trim().is_empty() {
                    match parse_ref_text(&d.range_text, &sheet) {
                        Ok(rg) => {
                            if let Some(si) = self.wb.sheet_index(&sheet) {
                                let rg = chart::clip_to_data(&self.wb.sheets[si], rg);
                                let fresh = Chart::new(&self.wb, si, rg, d.work.kind);
                                d.work.range = rg;
                                d.work.by_cols = fresh.by_cols;
                                d.work.names_first = None;
                                d.work.cats_first = None;
                                d.work.custom = None;
                                d.range_text = ref_text(&sheet, rg);
                                d.error = None;
                            }
                        }
                        Err(e) => d.error = Some(e),
                    }
                }
            });
            ui.vertical_centered(|ui| {
                if ui.button("Switch Row/Column").clicked() {
                    d.work.switch_row_col();
                    d.range_text = ref_text(&sheet, d.work.range);
                }
            });
            ui.add_space(4.0);
            ui.columns(2, |cols| {
                let ui = &mut cols[0];
                ui.label(RichText::new("Legend Entries (Series)").strong());
                ui.horizontal(|ui| {
                    if ui.button("Add").clicked() {
                        d.edit = Some(SeriesEdit { idx: None, name: String::new(), values: format!("={}!", crate::formula::quote_sheet(&sheet)), xs: String::new() });
                    }
                    if ui.add_enabled(!data.series.is_empty(), egui::Button::new("Edit")).clicked() {
                        let cu = chart::to_custom(&self.wb, &d.work);
                        if let Some(s) = cu.series.get(d.sel) {
                            d.edit = Some(SeriesEdit {
                                idx: Some(d.sel),
                                name: match s.name_cell {
                                    Some((r, c)) => ref_text(&sheet, Range::cell(r, c)),
                                    None => s.name.clone(),
                                },
                                values: s.values.map(|v| ref_text(&sheet, v)).unwrap_or_default(),
                                xs: s.xs.map(|v| ref_text(&sheet, v)).unwrap_or_default(),
                            });
                        }
                    }
                    if ui.add_enabled(data.series.len() > 1, egui::Button::new("Remove")).clicked() {
                        let mut cu = chart::to_custom(&self.wb, &d.work);
                        if d.sel < cu.series.len() {
                            cu.series.remove(d.sel);
                            if d.sel < d.work.series_fmt.len() {
                                d.work.series_fmt.remove(d.sel);
                            }
                            self.set_custom(&mut d.work, cu);
                            d.range_text.clear();
                            d.sel = d.sel.saturating_sub(1);
                        }
                    }
                    if ui.add_enabled(d.sel > 0, egui::Button::new("▲")).on_hover_text("Move Up").clicked() {
                        self.move_series(d, -1);
                    }
                    if ui.add_enabled(d.sel + 1 < data.series.len(), egui::Button::new("▼")).on_hover_text("Move Down").clicked() {
                        self.move_series(d, 1);
                    }
                });
                egui::Frame::NONE.stroke(Stroke::new(1.0, theme::border())).inner_margin(4.0).show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("series_list").max_height(170.0).auto_shrink([false, false]).show(ui, |ui| {
                        for (k, s) in data.series.iter().enumerate() {
                            if ui.selectable_label(k == d.sel, &s.name).clicked() {
                                d.sel = k;
                            }
                        }
                    });
                });
                let ui = &mut cols[1];
                ui.label(RichText::new(if xy { "Horizontal (X) Values" } else { "Horizontal (Category) Axis Labels" }).strong());
                if ui.add_enabled(!xy, egui::Button::new("Edit")).clicked() {
                    d.cats_edit = Some(data.cat_ref.map(|r| ref_text(&sheet, r)).unwrap_or_default());
                }
                egui::Frame::NONE.stroke(Stroke::new(1.0, theme::border())).inner_margin(4.0).show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("cat_list").max_height(170.0).auto_shrink([false, false]).show(ui, |ui| {
                        for c in data.categories.iter().take(500) {
                            ui.label(c);
                        }
                    });
                });
            });
            if let Some(e) = &mut d.cats_edit {
                ui.separator();
                let (mut done, mut drop) = (false, false);
                ui.horizontal(|ui| {
                    ui.label("Axis label range:");
                    ui.add(egui::TextEdit::singleline(e).desired_width(300.0));
                    if ui.button("OK").clicked() {
                        done = true;
                    }
                    if ui.button("Cancel").clicked() {
                        drop = true;
                    }
                });
                if drop {
                    d.cats_edit = None;
                }
                if done {
                    let text = d.cats_edit.take().unwrap_or_default();
                    let cats = if text.trim().trim_start_matches('=').is_empty() { Ok(None) } else { parse_ref_text(&text, &sheet).map(Some) };
                    match cats {
                        Ok(c) => {
                            let mut cu = chart::to_custom(&self.wb, &d.work);
                            cu.cats = c;
                            self.set_custom(&mut d.work, cu);
                            d.range_text.clear();
                            d.error = None;
                        }
                        Err(e) => d.error = Some(e),
                    }
                }
            }
            if d.edit.is_some() {
                ui.separator();
                let mut done = None;
                if let Some(e) = &mut d.edit {
                    ui.label(RichText::new(if e.idx.is_some() { "Edit Series" } else { "Add Series" }).strong());
                    egui::Grid::new("series_edit").num_columns(2).show(ui, |ui| {
                        ui.label("Series name:");
                        ui.add(egui::TextEdit::singleline(&mut e.name).desired_width(300.0).hint_text("=Sheet1!$B$1 or a name"));
                        ui.end_row();
                        if xy {
                            ui.label("Series X values:");
                            ui.add(egui::TextEdit::singleline(&mut e.xs).desired_width(300.0));
                            ui.end_row();
                        }
                        ui.label(if xy { "Series Y values:" } else { "Series values:" });
                        ui.add(egui::TextEdit::singleline(&mut e.values).desired_width(300.0));
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() {
                            done = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            done = Some(false);
                        }
                    });
                }
                match done {
                    Some(true) => {
                        let e = d.edit.take().unwrap();
                        match self.series_from_edit(&e, &sheet) {
                            Ok(src) => {
                                let mut cu = chart::to_custom(&self.wb, &d.work);
                                match e.idx {
                                    Some(k) if k < cu.series.len() => cu.series[k] = src,
                                    _ => {
                                        cu.series.push(src);
                                        d.sel = cu.series.len() - 1;
                                    }
                                }
                                self.set_custom(&mut d.work, cu);
                                d.range_text.clear();
                                d.error = None;
                            }
                            Err(msg) => {
                                d.error = Some(msg);
                                d.edit = Some(e);
                            }
                        }
                    }
                    Some(false) => d.edit = None,
                    None => {}
                }
            }
            if let Some(e) = &d.error {
                ui.colored_label(Color32::from_rgb(0xC4, 0x2B, 0x1C), e);
            }
            ui.separator();
            (ok, cancel) = Self::ok_cancel(ui);
        });
        if ok {
            let work = d.work.clone();
            self.edit_chart(|c| *c = work);
            return false;
        }
        open && !cancel
    }

    fn series_from_edit(&self, e: &SeriesEdit, sheet: &str) -> Result<SeriesSrc, String> {
        let values = parse_ref_text(&e.values, sheet)?;
        let xs = if e.xs.trim().trim_start_matches('=').is_empty() { None } else { Some(parse_ref_text(&e.xs, sheet)?) };
        let (name_cell, name) = if e.name.trim_start().starts_with('=') {
            let r = parse_ref_text(&e.name, sheet)?;
            (Some((r.r1, r.c1)), String::new())
        } else {
            (None, e.name.trim().to_string())
        };
        Ok(SeriesSrc { name_cell, name, values: Some(values), xs })
    }

    fn set_custom(&self, work: &mut Chart, cu: Custom) {
        if let Some(b) = chart::custom_bounds(&cu) {
            work.range = b;
        }
        work.custom = Some(cu);
    }

    fn move_series(&self, d: &mut DataDlg, dir: i32) {
        let mut cu = chart::to_custom(&self.wb, &d.work);
        let j = (d.sel as i32 + dir) as usize;
        if j >= cu.series.len() {
            return;
        }
        cu.series.swap(d.sel, j);
        let need = d.sel.max(j);
        d.work.fmt_mut(need);
        d.work.series_fmt.swap(d.sel, j);
        self.set_custom(&mut d.work, cu);
        d.range_text.clear();
        d.sel = j;
    }

    fn move_dialog(&mut self, ctx: &egui::Context, d: &mut MoveDlg) -> bool {
        let mut open = true;
        let (mut ok, mut cancel) = (false, false);
        let names: Vec<String> = self.wb.sheets.iter().map(|s| s.name.clone()).collect();
        egui::Window::new("Move Chart").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show(ctx, |ui| {
            ui.label("Choose where you want the chart to be placed:");
            egui::Grid::new("move_chart").num_columns(2).spacing(Vec2::new(10.0, 8.0)).show(ui, |ui| {
                ui.radio_value(&mut d.new_sheet, true, "New sheet:");
                ui.add_enabled(d.new_sheet, egui::TextEdit::singleline(&mut d.name).desired_width(220.0));
                ui.end_row();
                ui.radio_value(&mut d.new_sheet, false, "Object in:");
                ui.add_enabled_ui(!d.new_sheet, |ui| {
                    ComboBox::from_id_salt("move_target").selected_text(names.get(d.target).cloned().unwrap_or_default()).width(220.0).show_ui(ui, |ui| {
                        for (k, n) in names.iter().enumerate() {
                            ui.selectable_value(&mut d.target, k, n);
                        }
                    });
                });
                ui.end_row();
            });
            ui.separator();
            (ok, cancel) = Self::ok_cancel(ui);
        });
        if ok {
            self.move_chart_to(d);
            return false;
        }
        open && !cancel
    }

    fn move_chart_to(&mut self, d: &MoveDlg) {
        let Some(i) = self.selected_chart() else { return };
        let from = self.wb.active;
        if !d.new_sheet && d.target == from {
            return;
        }
        let name = d.name.trim().to_string();
        if d.new_sheet && (name.is_empty() || self.wb.sheet_index(&name).is_some()) {
            self.status = Some("Choose a new, unused sheet name for the chart.".into());
            return;
        }
        self.push_undo(&[Scope::All]);
        let mut ch = self.wb.sheets[from].charts.remove(i);
        let to = if d.new_sheet {
            let at = from + 1;
            self.wb.sheets.insert(at, Sheet::new(&name));
            ch.anchor = (0, 0);
            ch.offset = (0.0, 0.0);
            ch.size = (ch.size.0.max(900.0), ch.size.1.max(560.0));
            at
        } else {
            d.target
        };
        self.wb.sheets[to].charts.push(ch);
        self.wb.active = to;
        let k = self.wb.sheets[to].charts.len() - 1;
        self.chart.sel = Some((to, k));
        self.chart.elem = None;
        self.changed();
    }
}
