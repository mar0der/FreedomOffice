//! Writes sheet charts as native xlsx charts, plus the pie explosions the
//! writer has no API for (patched into the saved package).

use std::io::{Cursor, Read, Write};

use rust_xlsxwriter::{
    Chart as XChart, ChartAxis, ChartDataLabel, ChartDataLabelPosition, ChartDataTable, ChartErrorBars, ChartErrorBarsDirection,
    ChartErrorBarsType, ChartFormat, ChartLegendPosition, ChartLine, ChartLineDashType, ChartMarker, ChartMarkerType, ChartPoint,
    ChartSeries, ChartSolidFill, ChartTrendline, ChartTrendlineType, ChartType, Worksheet, XlsxError,
};

use crate::chart::{self, Axis, Chart, ChartData, ChartKind, Dash, ErrDir, ErrKind, Grouping, LabelPos, Labels, LegendPos, MarkerKind, TrendKind};
use crate::model::{Range, Sheet, Workbook, MAX_COLS, MAX_ROWS};

/// Explosions of one written chart: (series index, series explosion, [(point, explosion)]).
pub(super) type Explosions = Vec<(usize, u32, Vec<(usize, u32)>)>;

fn chart_type(kind: ChartKind, g: Grouping, ch: &Chart) -> ChartType {
    use ChartType as T;
    match (kind, g) {
        (ChartKind::Column, Grouping::Standard) => T::Column,
        (ChartKind::Column, Grouping::Stacked) => T::ColumnStacked,
        (ChartKind::Column, Grouping::Percent) => T::ColumnPercentStacked,
        (ChartKind::Bar, Grouping::Standard) => T::Bar,
        (ChartKind::Bar, Grouping::Stacked) => T::BarStacked,
        (ChartKind::Bar, Grouping::Percent) => T::BarPercentStacked,
        (ChartKind::Line, Grouping::Standard) => T::Line,
        (ChartKind::Line, Grouping::Stacked) => T::LineStacked,
        (ChartKind::Line, Grouping::Percent) => T::LinePercentStacked,
        (ChartKind::Area, Grouping::Standard) => T::Area,
        (ChartKind::Area, Grouping::Stacked) => T::AreaStacked,
        (ChartKind::Area, Grouping::Percent) => T::AreaPercentStacked,
        (ChartKind::Pie, _) => T::Pie,
        (ChartKind::Doughnut, _) => T::Doughnut,
        (ChartKind::Scatter, _) => match (ch.lines, ch.smooth, ch.markers) {
            (false, _, _) => T::Scatter,
            (true, true, true) => T::ScatterSmoothWithMarkers,
            (true, true, false) => T::ScatterSmooth,
            (true, false, true) => T::ScatterStraightWithMarkers,
            (true, false, false) => T::ScatterStraight,
        },
        (ChartKind::Radar, _) if ch.filled => T::RadarFilled,
        (ChartKind::Radar, _) if ch.markers => T::RadarWithMarkers,
        (ChartKind::Radar, _) => T::Radar,
    }
}

fn rng(sheet: &str, r: Range) -> (&str, u32, u16, u32, u16) {
    (sheet, r.r1, r.c1 as u16, r.r2, r.c2 as u16)
}

fn solid(c: u32) -> ChartSolidFill {
    let mut f = ChartSolidFill::new();
    f.set_color(c);
    f
}

fn dash_type(d: Dash) -> ChartLineDashType {
    match d {
        Dash::Solid => ChartLineDashType::Solid,
        Dash::RoundDot => ChartLineDashType::RoundDot,
        Dash::SquareDot => ChartLineDashType::SquareDot,
        Dash::Dash => ChartLineDashType::Dash,
        Dash::DashDot => ChartLineDashType::DashDot,
        Dash::LongDash => ChartLineDashType::LongDash,
        Dash::LongDashDot => ChartLineDashType::LongDashDot,
    }
}

fn marker_type(m: MarkerKind) -> Option<ChartMarkerType> {
    Some(match m {
        MarkerKind::Circle => ChartMarkerType::Circle,
        MarkerKind::Square => ChartMarkerType::Square,
        MarkerKind::Diamond => ChartMarkerType::Diamond,
        MarkerKind::Triangle => ChartMarkerType::Triangle,
        MarkerKind::X => ChartMarkerType::X,
        MarkerKind::Plus => ChartMarkerType::PlusSign,
        MarkerKind::Star => ChartMarkerType::Star,
        MarkerKind::Dash => ChartMarkerType::ShortDash,
        MarkerKind::Auto | MarkerKind::None => return None,
    })
}

fn label_pos(p: LabelPos) -> ChartDataLabelPosition {
    match p {
        LabelPos::Auto => ChartDataLabelPosition::Default,
        LabelPos::Center => ChartDataLabelPosition::Center,
        LabelPos::InsideEnd => ChartDataLabelPosition::InsideEnd,
        LabelPos::InsideBase => ChartDataLabelPosition::InsideBase,
        LabelPos::OutsideEnd => ChartDataLabelPosition::OutsideEnd,
        LabelPos::Left => ChartDataLabelPosition::Left,
        LabelPos::Right => ChartDataLabelPosition::Right,
        LabelPos::Above => ChartDataLabelPosition::Above,
        LabelPos::Below => ChartDataLabelPosition::Below,
        LabelPos::BestFit => ChartDataLabelPosition::BestFit,
    }
}

fn data_label(l: &Labels) -> ChartDataLabel {
    let mut d = ChartDataLabel::new();
    if l.value {
        d.show_value();
    }
    if l.category {
        d.show_category_name();
    }
    if l.series {
        d.show_series_name();
    }
    if l.percent {
        d.show_percentage();
    }
    if l.pos != LabelPos::Auto {
        d.set_position(label_pos(l.pos));
    }
    if !l.num_fmt.is_empty() {
        d.set_num_format(l.num_fmt.as_str());
    }
    d
}

fn set_axis(ax: &mut ChartAxis, a: &Axis, value: bool) {
    if let Some(t) = a.title_text() {
        ax.set_name(t);
    }
    if !a.visible {
        ax.set_hidden(true);
    }
    ax.set_major_gridlines(a.major_grid);
    ax.set_minor_gridlines(a.minor_grid);
    if a.reverse {
        ax.set_reverse();
    }
    if value {
        if let Some(v) = a.min {
            ax.set_min(v);
        }
        if let Some(v) = a.max {
            ax.set_max(v);
        }
        if let Some(v) = a.major {
            ax.set_major_unit(v);
        }
        if let Some(v) = a.minor {
            ax.set_minor_unit(v);
        }
        if let Some(b) = a.log_base {
            ax.set_log_base(b.round().clamp(2.0, 1000.0) as u16);
        }
    }
    if !a.num_fmt.is_empty() {
        ax.set_num_format(a.num_fmt.as_str());
    }
}

/// Configures one series of the chart from the model.
fn fill_series(ser: &mut ChartSeries, ch: &Chart, data: &ChartData, k: usize, src: &str) {
    let s = &data.series[k];
    ser.set_values(rng(src, s.refr));
    if let Some(x) = s.xs_ref {
        ser.set_categories(rng(src, x));
    } else if let Some(cr) = data.cat_ref {
        ser.set_categories(rng(src, cr));
    }
    match s.name_cell {
        Some((r, c)) => {
            ser.set_name((src, r, c as u16));
        }
        None if ch.custom.is_some() => {
            ser.set_name(s.name.as_str());
        }
        None => {}
    }
    let kind = ch.series_kind(k);
    let mut f = ch.fmt(k);
    // Colour schemes other than the default are saved as explicit colours.
    if ch.palette != 0 && !ch.kind.is_round() {
        f.fill = f.fill.or(Some(ch.color(k)));
    }
    let lined = matches!(kind, ChartKind::Line | ChartKind::Scatter | ChartKind::Radar) && !(kind == ChartKind::Radar && ch.filled);
    // Fill and line.
    let mut fmt = ChartFormat::new();
    let mut any = false;
    if lined {
        if kind == ChartKind::Scatter && !ch.lines {
            fmt.set_no_line();
            any = true;
        } else if f.line.is_some() || f.fill.is_some() || f.width.is_some() || f.dash != Dash::Solid {
            let mut ln = ChartLine::new();
            if let Some(c) = f.line.or(f.fill) {
                ln.set_color(c);
            }
            if let Some(w) = f.width {
                ln.set_width(w);
            }
            if f.dash != Dash::Solid {
                ln.set_dash_type(dash_type(f.dash));
            }
            fmt.set_line(&ln);
            any = true;
        }
    } else if !ch.kind.is_round() {
        if let Some(c) = f.fill {
            fmt.set_solid_fill(&solid(c));
            any = true;
        }
        if f.line.is_some() || f.width.is_some() || f.dash != Dash::Solid {
            let mut ln = ChartLine::new();
            if let Some(c) = f.line {
                ln.set_color(c);
            }
            if let Some(w) = f.width {
                ln.set_width(w);
            }
            if f.dash != Dash::Solid {
                ln.set_dash_type(dash_type(f.dash));
            }
            fmt.set_border(&ln);
            any = true;
        }
    }
    if any {
        ser.set_format(&mut fmt);
    }
    // Markers.
    if lined {
        let shown = ch.marker(k);
        let mut m = ChartMarker::new();
        match shown {
            None => {
                m.set_none();
            }
            Some(mk) => {
                match marker_type(mk) {
                    Some(t) => {
                        m.set_type(t);
                    }
                    None => {
                        m.set_automatic();
                    }
                }
                if f.marker_size > 0 {
                    m.set_size(f.marker_size);
                }
                if let Some(c) = f.fill {
                    m.set_format(ChartFormat::new().set_solid_fill(&solid(c)).set_line(&ChartLine::new().set_color(f.line.unwrap_or(c))));
                }
            }
        }
        ser.set_marker(&m);
    }
    if kind == ChartKind::Line && ch.smooth {
        ser.set_smooth(true);
    }
    // Pie point colours (exploded points get an explicit colour so their element exists).
    if ch.kind.is_round() && (!f.points.is_empty() || ch.palette != 0) {
        let n = s.values.len();
        let pts: Vec<ChartPoint> = (0..n)
            .map(|i| match (f.point(i), ch.palette != 0) {
                (Some(p), _) => ChartPoint::new().set_format(ChartFormat::new().set_solid_fill(&solid(p.fill.unwrap_or_else(|| ch.color(i))))),
                (None, true) => ChartPoint::new().set_format(ChartFormat::new().set_solid_fill(&solid(ch.color(i)))),
                (None, false) => ChartPoint::new(),
            })
            .collect();
        ser.set_points(&pts);
    }
    // Labels.
    let l = ch.series_labels(k);
    if l.any() {
        ser.set_data_label(&data_label(l));
    }
    if kind.is_bar() {
        ser.set_gap(ch.gap.min(500));
        ser.set_overlap(ch.overlap.clamp(-100, 100) as i8);
    }
    if ch.on_secondary(k) {
        ser.set_secondary_axis(true);
    }
    // Trendline.
    if let Some(t) = &f.trend {
        let ty = match t.kind {
            TrendKind::Linear => ChartTrendlineType::Linear,
            TrendKind::Exponential => ChartTrendlineType::Exponential,
            TrendKind::Logarithmic => ChartTrendlineType::Logarithmic,
            TrendKind::Polynomial => ChartTrendlineType::Polynomial(t.order.clamp(2, 6)),
            TrendKind::Power => ChartTrendlineType::Power,
            TrendKind::MovingAverage => ChartTrendlineType::MovingAverage(t.period.max(2)),
        };
        let mut tl = ChartTrendline::new();
        tl.set_type(ty);
        if t.forward > 0.0 {
            tl.set_forward_period(t.forward);
        }
        if t.backward > 0.0 {
            tl.set_backward_period(t.backward);
        }
        if let Some(i) = t.intercept {
            tl.set_intercept(i);
        }
        if t.show_eq {
            tl.display_equation(true);
        }
        if t.show_r2 {
            tl.display_r_squared(true);
        }
        if let Some(n) = &t.name {
            tl.set_name(n.as_str());
        }
        ser.set_trendline(&tl);
    }
    // Error bars.
    if let Some(e) = &f.err {
        let ty = match e.kind {
            ErrKind::Fixed => ChartErrorBarsType::FixedValue(e.amount),
            ErrKind::Percent => ChartErrorBarsType::Percentage(e.amount),
            ErrKind::StdDev => ChartErrorBarsType::StandardDeviation(e.amount),
            ErrKind::StdErr => ChartErrorBarsType::StandardError,
        };
        let mut eb = ChartErrorBars::new();
        eb.set_type(ty);
        eb.set_direction(match e.dir {
            ErrDir::Both => ChartErrorBarsDirection::Both,
            ErrDir::Plus => ChartErrorBarsDirection::Plus,
            ErrDir::Minus => ChartErrorBarsDirection::Minus,
        });
        eb.set_end_cap(e.cap);
        if ch.kind == ChartKind::Bar {
            ser.set_x_error_bars(&eb);
        } else {
            ser.set_y_error_bars(&eb);
        }
    }
}

/// Whether a chart is written (it has data and a valid position).
fn writable(wb: &Workbook, ch: &Chart) -> Option<ChartData> {
    let data = chart::resolve(wb, ch);
    (!data.series.is_empty() && ch.anchor.0 < MAX_ROWS && ch.anchor.1 < MAX_COLS && wb.sheet_index(&ch.sheet).is_some()).then_some(data)
}

fn build(wb: &Workbook, ch: &Chart, data: &ChartData) -> XChart {
    let src = wb.sheets[wb.sheet_index(&ch.sheet).unwrap_or(0)].name.as_str();
    let n = data.series.len();
    let main = ch.kind;
    let mut xc = XChart::new(chart_type(main, ch.grouping, ch));
    // Combo: series of another kind go to one combined chart.
    let other = (0..n).map(|k| ch.series_kind(k)).find(|k| *k != main);
    let mut combined = other.map(|k| XChart::new(chart_type(k, Grouping::Standard, ch)));
    for k in 0..n {
        let target = match (&mut combined, ch.series_kind(k) != main) {
            (Some(c), true) => c,
            _ => &mut xc,
        };
        let ser = target.add_series();
        fill_series(ser, ch, data, k, src);
    }
    if let Some(c) = &combined {
        xc.combine(c);
    }
    match &ch.title {
        Some(t) if t.is_empty() => {
            xc.title().set_hidden();
        }
        Some(t) => {
            xc.title().set_name(t.as_str());
        }
        // An empty title element: the automatic title.
        None => {
            xc.title().set_format(ChartFormat::new().set_no_fill());
        }
    }
    if ch.legend {
        xc.legend().set_position(match ch.legend_pos {
            LegendPos::Bottom => ChartLegendPosition::Bottom,
            LegendPos::Top => ChartLegendPosition::Top,
            LegendPos::Left => ChartLegendPosition::Left,
            LegendPos::Right => ChartLegendPosition::Right,
            LegendPos::TopRight => ChartLegendPosition::TopRight,
        });
    } else {
        xc.legend().set_hidden();
    }
    if ch.has_axes() {
        // For bar charts the writer's X axis is the (horizontal) value axis.
        let bar = main == ChartKind::Bar;
        let xy = main == ChartKind::Scatter;
        if bar {
            set_axis(xc.x_axis(), &ch.y_axis, true);
            set_axis(xc.y_axis(), &ch.x_axis, false);
        } else {
            set_axis(xc.x_axis(), &ch.x_axis, xy);
            set_axis(xc.y_axis(), &ch.y_axis, true);
        }
        if ch.has_secondary(n) {
            if bar {
                set_axis(xc.x2_axis(), &ch.y2_axis, true);
            } else {
                set_axis(xc.y2_axis(), &ch.y2_axis, true);
            }
        }
        if ch.data_table && !xy && main != ChartKind::Radar {
            xc.set_data_table(&ChartDataTable::new());
        }
    }
    if main == ChartKind::Doughnut {
        xc.set_hole_size(ch.hole.clamp(10, 90));
    }
    if main.is_round() && ch.first_angle > 0 {
        xc.set_rotation(ch.first_angle.min(360));
    }
    if let Some(bg) = ch.bg {
        xc.chart_area().set_format(ChartFormat::new().set_solid_fill(&solid(bg)));
    }
    if let Some(bg) = ch.plot_bg {
        xc.plot_area().set_format(ChartFormat::new().set_solid_fill(&solid(bg)));
    }
    xc.set_width(ch.size.0.round().max(1.0) as u32).set_height(ch.size.1.round().max(1.0) as u32);
    xc
}

/// Explosions of a chart, empty when it has none.
fn explosions(ch: &Chart, data: &ChartData) -> Explosions {
    if !ch.kind.is_round() {
        return Vec::new();
    }
    (0..data.series.len())
        .filter_map(|k| {
            let f = ch.fmt_ref(k)?;
            let pts: Vec<(usize, u32)> = f.points.iter().filter(|p| p.explode > 0).map(|p| (p.idx as usize, p.explode)).collect();
            (f.explode > 0 || !pts.is_empty()).then_some((k, f.explode, pts))
        })
        .collect()
}

/// Writes the charts of a sheet; appends their explosions in the writer's chart order.
pub(super) fn write(wb: &Workbook, sh: &Sheet, ws: &mut Worksheet, out: &mut Vec<Explosions>) -> Result<(), XlsxError> {
    // The writer keys charts by position, so charts stacked exactly on top of each
    // other are nudged apart by a pixel instead of being dropped.
    let mut used = std::collections::HashSet::new();
    let mut order: Vec<((u32, u16, u32, u32), Explosions)> = Vec::new();
    for ch in &sh.charts {
        let Some(data) = writable(wb, ch) else { continue };
        let xc = build(wb, ch, &data);
        let (mut x, y) = (ch.offset.0.round().max(0.0) as u32, ch.offset.1.round().max(0.0) as u32);
        while !used.insert((ch.anchor, x, y)) {
            x += 1;
        }
        ws.insert_chart_with_offset(ch.anchor.0, ch.anchor.1 as u16, &xc, x, y)?;
        order.push(((ch.anchor.0, ch.anchor.1 as u16, x, y), explosions(ch, &data)));
    }
    order.sort_by_key(|(k, _)| *k);
    out.extend(order.into_iter().map(|(_, e)| e));
    Ok(())
}

/// Inserts `<c:explosion>` elements into the saved chart parts (chartN.xml, in writer order).
pub(super) fn patch_explosions(buf: Vec<u8>, ex: &[Explosions]) -> Result<Vec<u8>, String> {
    if ex.iter().all(Vec::is_empty) {
        return Ok(buf);
    }
    let mut zin = zip::ZipArchive::new(Cursor::new(buf)).map_err(|e| e.to_string())?;
    let mut zout = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..zin.len() {
        let mut f = zin.by_index(i).map_err(|e| e.to_string())?;
        let name = f.name().map_err(|e| e.to_string())?.to_string();
        let idx = name.strip_prefix("xl/charts/chart").and_then(|s| s.strip_suffix(".xml")).and_then(|s| s.parse::<usize>().ok());
        match idx.and_then(|k| ex.get(k.wrapping_sub(1))).filter(|e| !e.is_empty()) {
            Some(e) => {
                let mut xml = String::new();
                f.read_to_string(&mut xml).map_err(|e| e.to_string())?;
                let xml = patch_chart_xml(&xml, e);
                let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
                zout.start_file(name, opts).map_err(|e| e.to_string())?;
                zout.write_all(xml.as_bytes()).map_err(|e| e.to_string())?;
            }
            None => zout.raw_copy_file(f).map_err(|e| e.to_string())?,
        }
    }
    Ok(zout.finish().map_err(|e| e.to_string())?.into_inner())
}

fn patch_chart_xml(xml: &str, ex: &Explosions) -> String {
    let mut out = String::with_capacity(xml.len() + 64);
    let mut rest = xml;
    let mut k = 0usize;
    while let Some(start) = rest.find("<c:ser>") {
        let Some(len) = rest[start..].find("</c:ser>") else { break };
        let end = start + len + "</c:ser>".len();
        out.push_str(&rest[..start]);
        let mut ser = rest[start..end].to_string();
        if let Some((_, e, pts)) = ex.iter().find(|(s, _, _)| *s == k) {
            for (p, v) in pts {
                let tag = format!("<c:dPt><c:idx val=\"{p}\"/>");
                if let Some(at) = ser.find(&tag) {
                    ser.insert_str(at + tag.len(), &format!("<c:explosion val=\"{v}\"/>"));
                }
            }
            if *e > 0 {
                let at = ["<c:dPt>", "<c:dLbls>", "<c:cat>", "<c:val>"].iter().filter_map(|t| ser.find(t)).min();
                if let Some(at) = at {
                    ser.insert_str(at, &format!("<c:explosion val=\"{e}\"/>"));
                }
            }
        }
        out.push_str(&ser);
        rest = &rest[end..];
        k += 1;
    }
    out.push_str(rest);
    out
}
