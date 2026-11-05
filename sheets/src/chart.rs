//! Charts: model, series detection from a cell range, axis scaling and
//! keeping chart sources in step with sheet edits.

use crate::model::{Range, Sheet, Value, Workbook, DEFAULT_COL_W, DEFAULT_ROW_H, MAX_COLS, MAX_ROWS};
use crate::numfmt;

pub mod trend;

/// Default chart size in pixels (5 x 3 inches at 96 dpi).
pub const DEFAULT_W: f32 = 480.0;
pub const DEFAULT_H: f32 = 288.0;
pub const MIN_SIZE: f32 = 40.0;

/// Default office series palette.
pub const PALETTE: [u32; 6] = PALETTES[0].1;

/// Points drawn per series are capped to keep painting cheap on huge ranges.
pub const MAX_POINTS: usize = 4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartKind {
    Column,
    Bar,
    Line,
    Pie,
    Doughnut,
    Area,
    Scatter,
    Radar,
}

impl ChartKind {
    pub const ALL: [ChartKind; 8] = [
        ChartKind::Column,
        ChartKind::Line,
        ChartKind::Pie,
        ChartKind::Bar,
        ChartKind::Area,
        ChartKind::Scatter,
        ChartKind::Radar,
        ChartKind::Doughnut,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ChartKind::Column => "Column",
            ChartKind::Bar => "Bar",
            ChartKind::Line => "Line",
            ChartKind::Pie => "Pie",
            ChartKind::Doughnut => "Doughnut",
            ChartKind::Area => "Area",
            ChartKind::Scatter => "X Y (Scatter)",
            ChartKind::Radar => "Radar",
        }
    }

    pub fn is_round(self) -> bool {
        matches!(self, ChartKind::Pie | ChartKind::Doughnut)
    }

    /// Kinds that can be mixed in one combo chart.
    pub fn combinable(self) -> bool {
        matches!(self, ChartKind::Column | ChartKind::Line | ChartKind::Area)
    }

    /// Bars and columns: series take a share of each category slot.
    pub fn is_bar(self) -> bool {
        matches!(self, ChartKind::Column | ChartKind::Bar)
    }

    pub fn can_stack(self) -> bool {
        matches!(self, ChartKind::Column | ChartKind::Bar | ChartKind::Line | ChartKind::Area)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Grouping {
    #[default]
    Standard,
    Stacked,
    Percent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LegendPos {
    #[default]
    Bottom,
    Top,
    Left,
    Right,
    TopRight,
}

impl LegendPos {
    pub const ALL: [LegendPos; 5] = [LegendPos::Right, LegendPos::Top, LegendPos::Left, LegendPos::Bottom, LegendPos::TopRight];
    pub fn label(self) -> &'static str {
        match self {
            LegendPos::Bottom => "Bottom",
            LegendPos::Top => "Top",
            LegendPos::Left => "Left",
            LegendPos::Right => "Right",
            LegendPos::TopRight => "Top Right",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LabelPos {
    /// The chart type's default (outside end for clustered columns, centre for stacked, right for lines, best fit for pies).
    #[default]
    Auto,
    Center,
    InsideEnd,
    InsideBase,
    OutsideEnd,
    Left,
    Right,
    Above,
    Below,
    BestFit,
}

impl LabelPos {
    pub const ALL: [LabelPos; 10] = [
        LabelPos::Auto,
        LabelPos::Center,
        LabelPos::InsideEnd,
        LabelPos::InsideBase,
        LabelPos::OutsideEnd,
        LabelPos::Left,
        LabelPos::Right,
        LabelPos::Above,
        LabelPos::Below,
        LabelPos::BestFit,
    ];
    pub fn label(self) -> &'static str {
        match self {
            LabelPos::Auto => "Default",
            LabelPos::Center => "Center",
            LabelPos::InsideEnd => "Inside End",
            LabelPos::InsideBase => "Inside Base",
            LabelPos::OutsideEnd => "Outside End",
            LabelPos::Left => "Left",
            LabelPos::Right => "Right",
            LabelPos::Above => "Above",
            LabelPos::Below => "Below",
            LabelPos::BestFit => "Best Fit",
        }
    }
    /// Positions offered for a chart kind, like the Format Data Labels pane.
    pub fn for_kind(kind: ChartKind, grouping: Grouping) -> &'static [LabelPos] {
        use LabelPos::*;
        match kind {
            ChartKind::Column | ChartKind::Bar if grouping == Grouping::Standard => &[Auto, Center, InsideEnd, InsideBase, OutsideEnd],
            ChartKind::Column | ChartKind::Bar => &[Auto, Center, InsideEnd, InsideBase],
            ChartKind::Line | ChartKind::Scatter => &[Auto, Center, Left, Right, Above, Below],
            ChartKind::Pie => &[Auto, Center, InsideEnd, OutsideEnd, BestFit],
            _ => &[Auto],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Dash {
    #[default]
    Solid,
    RoundDot,
    SquareDot,
    Dash,
    DashDot,
    LongDash,
    LongDashDot,
}

impl Dash {
    pub const ALL: [Dash; 7] = [Dash::Solid, Dash::RoundDot, Dash::SquareDot, Dash::Dash, Dash::DashDot, Dash::LongDash, Dash::LongDashDot];
    pub fn label(self) -> &'static str {
        match self {
            Dash::Solid => "Solid",
            Dash::RoundDot => "Round Dot",
            Dash::SquareDot => "Square Dot",
            Dash::Dash => "Dash",
            Dash::DashDot => "Dash Dot",
            Dash::LongDash => "Long Dash",
            Dash::LongDashDot => "Long Dash Dot",
        }
    }
    /// On/off run lengths in multiples of the line width (empty = solid).
    pub fn pattern(self) -> &'static [f32] {
        match self {
            Dash::Solid => &[],
            Dash::RoundDot => &[1.0, 1.0],
            Dash::SquareDot => &[1.0, 1.0],
            Dash::Dash => &[4.0, 3.0],
            Dash::DashDot => &[4.0, 3.0, 1.0, 3.0],
            Dash::LongDash => &[8.0, 3.0],
            Dash::LongDashDot => &[8.0, 3.0, 1.0, 3.0],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MarkerKind {
    /// Follows the chart type (circles on "with markers" charts, none otherwise).
    #[default]
    Auto,
    None,
    Circle,
    Square,
    Diamond,
    Triangle,
    X,
    Plus,
    Star,
    Dash,
}

impl MarkerKind {
    pub const ALL: [MarkerKind; 10] = [
        MarkerKind::Auto,
        MarkerKind::None,
        MarkerKind::Circle,
        MarkerKind::Square,
        MarkerKind::Diamond,
        MarkerKind::Triangle,
        MarkerKind::X,
        MarkerKind::Plus,
        MarkerKind::Star,
        MarkerKind::Dash,
    ];
    pub fn label(self) -> &'static str {
        match self {
            MarkerKind::Auto => "Automatic",
            MarkerKind::None => "None",
            MarkerKind::Circle => "Circle",
            MarkerKind::Square => "Square",
            MarkerKind::Diamond => "Diamond",
            MarkerKind::Triangle => "Triangle",
            MarkerKind::X => "X",
            MarkerKind::Plus => "Plus",
            MarkerKind::Star => "Star",
            MarkerKind::Dash => "Dash",
        }
    }
}

/// What data labels show and where.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Labels {
    pub value: bool,
    pub category: bool,
    pub series: bool,
    pub percent: bool,
    pub pos: LabelPos,
    /// Empty = linked to the source cells.
    pub num_fmt: String,
}

impl Labels {
    pub fn values() -> Labels {
        Labels { value: true, ..Default::default() }
    }
    pub fn any(&self) -> bool {
        self.value || self.category || self.series || self.percent
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Axis {
    pub visible: bool,
    /// `None` = no axis title, `Some("")` = the default "Axis Title".
    pub title: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub major: Option<f64>,
    pub minor: Option<f64>,
    /// Empty = linked to the source cells.
    pub num_fmt: String,
    pub log_base: Option<f64>,
    /// Values (or categories) in reverse order.
    pub reverse: bool,
    pub major_grid: bool,
    pub minor_grid: bool,
}

impl Default for Axis {
    fn default() -> Self {
        Axis {
            visible: true,
            title: None,
            min: None,
            max: None,
            major: None,
            minor: None,
            num_fmt: String::new(),
            log_base: None,
            reverse: false,
            major_grid: false,
            minor_grid: false,
        }
    }
}

impl Axis {
    pub fn title_text(&self) -> Option<&str> {
        self.title.as_deref().map(|t| if t.is_empty() { "Axis Title" } else { t })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TrendKind {
    #[default]
    Linear,
    Exponential,
    Logarithmic,
    Polynomial,
    Power,
    MovingAverage,
}

impl TrendKind {
    pub const ALL: [TrendKind; 6] =
        [TrendKind::Exponential, TrendKind::Linear, TrendKind::Logarithmic, TrendKind::Polynomial, TrendKind::Power, TrendKind::MovingAverage];
    pub fn label(self) -> &'static str {
        match self {
            TrendKind::Linear => "Linear",
            TrendKind::Exponential => "Exponential",
            TrendKind::Logarithmic => "Logarithmic",
            TrendKind::Polynomial => "Polynomial",
            TrendKind::Power => "Power",
            TrendKind::MovingAverage => "Moving Average",
        }
    }
    /// Legend prefix ("Linear (Sales)", "2 per. Mov. Avg. (Sales)").
    pub fn legend(self, t: &Trendline, series: &str) -> String {
        match self {
            TrendKind::Exponential => format!("Expon. ({series})"),
            TrendKind::Logarithmic => format!("Log. ({series})"),
            TrendKind::Polynomial => format!("Poly. ({series})"),
            TrendKind::MovingAverage => format!("{} per. Mov. Avg. ({series})", t.period),
            _ => format!("{} ({series})", self.label()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trendline {
    pub kind: TrendKind,
    /// Polynomial order (2-6).
    pub order: u8,
    /// Moving average period (2+).
    pub period: u8,
    pub forward: f64,
    pub backward: f64,
    pub intercept: Option<f64>,
    pub show_eq: bool,
    pub show_r2: bool,
    pub name: Option<String>,
}

impl Default for Trendline {
    fn default() -> Self {
        Trendline { kind: TrendKind::Linear, order: 2, period: 2, forward: 0.0, backward: 0.0, intercept: None, show_eq: false, show_r2: false, name: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ErrKind {
    Fixed,
    #[default]
    Percent,
    StdDev,
    StdErr,
}

impl ErrKind {
    pub const ALL: [ErrKind; 4] = [ErrKind::Fixed, ErrKind::Percent, ErrKind::StdDev, ErrKind::StdErr];
    pub fn label(self) -> &'static str {
        match self {
            ErrKind::Fixed => "Fixed value",
            ErrKind::Percent => "Percentage",
            ErrKind::StdDev => "Standard deviation(s)",
            ErrKind::StdErr => "Standard Error",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ErrDir {
    #[default]
    Both,
    Plus,
    Minus,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ErrorBars {
    pub kind: ErrKind,
    /// Fixed value, percentage or number of standard deviations.
    pub amount: f64,
    pub dir: ErrDir,
    pub cap: bool,
}

impl Default for ErrorBars {
    fn default() -> Self {
        ErrorBars { kind: ErrKind::Percent, amount: 5.0, dir: ErrDir::Both, cap: true }
    }
}

/// Formatting of one data point (pie slices).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct PointFmt {
    pub idx: u32,
    pub fill: Option<u32>,
    /// Distance from the centre in percent of the radius.
    pub explode: u32,
}

/// Per-series formatting; `None`/default values follow the chart.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SeriesFmt {
    /// Combo charts: this series is drawn as another kind.
    pub kind: Option<ChartKind>,
    pub secondary: bool,
    pub fill: Option<u32>,
    pub line: Option<u32>,
    /// Line width in points.
    pub width: Option<f32>,
    pub dash: Dash,
    pub marker: MarkerKind,
    /// Marker size in points (0 = automatic, 5).
    pub marker_size: u8,
    pub labels: Option<Labels>,
    pub explode: u32,
    pub points: Vec<PointFmt>,
    pub trend: Option<Trendline>,
    pub err: Option<ErrorBars>,
}

impl SeriesFmt {
    pub fn point(&self, i: usize) -> Option<&PointFmt> {
        self.points.iter().find(|p| p.idx as usize == i)
    }
    pub fn point_mut(&mut self, i: usize) -> &mut PointFmt {
        match self.points.iter().position(|p| p.idx as usize == i) {
            Some(k) => &mut self.points[k],
            None => {
                self.points.push(PointFmt { idx: i as u32, ..Default::default() });
                self.points.last_mut().unwrap()
            }
        }
    }
}

/// An explicitly defined series (Select Data), on the chart's source sheet.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SeriesSrc {
    pub name_cell: Option<(u32, u32)>,
    /// Name typed in when there is no name cell.
    pub name: String,
    pub values: Option<Range>,
    /// Scatter X values of this series.
    pub xs: Option<Range>,
}

/// Explicit series list replacing the automatic split of `Chart::range`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Custom {
    pub series: Vec<SeriesSrc>,
    pub cats: Option<Range>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chart {
    pub kind: ChartKind,
    /// Name of the sheet holding the data.
    pub sheet: String,
    pub range: Range,
    /// Series run down columns (true) or across rows (false).
    pub by_cols: bool,
    /// Overrides for "first line holds series names" / "first line holds categories";
    /// `None` = detect from the cell contents.
    pub names_first: Option<bool>,
    pub cats_first: Option<bool>,
    /// Explicit series (Select Data); `None` = series come from `range`.
    pub custom: Option<Custom>,
    /// `None` = automatic title, `Some("")` = no title.
    pub title: Option<String>,
    pub legend: bool,
    pub legend_pos: LegendPos,
    pub labels: Labels,
    pub data_table: bool,
    pub grouping: Grouping,
    /// Line / scatter / radar: show markers.
    pub markers: bool,
    /// Scatter: connect the points.
    pub lines: bool,
    pub smooth: bool,
    /// Radar: filled.
    pub filled: bool,
    /// Category axis (X values for scatter charts).
    pub x_axis: Axis,
    pub y_axis: Axis,
    /// Secondary value axis.
    pub y2_axis: Axis,
    pub series_fmt: Vec<SeriesFmt>,
    /// Colour scheme (index into `PALETTES`).
    pub palette: u8,
    /// Chart area and plot area fills (`None` = white / none).
    pub bg: Option<u32>,
    pub plot_bg: Option<u32>,
    /// Bar/column gap width and overlap in percent.
    pub gap: u16,
    pub overlap: i16,
    /// Doughnut hole size in percent.
    pub hole: u8,
    /// Angle of the first pie slice in degrees.
    pub first_angle: u16,
    /// Top-left cell (row, col) the chart is anchored to.
    pub anchor: (u32, u32),
    /// Offset from the anchor cell's top-left corner, in unzoomed pixels.
    pub offset: (f32, f32),
    /// Width and height in unzoomed pixels.
    pub size: (f32, f32),
}

/// One entry of the Insert Chart menus: a chart kind with its variant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Subtype {
    pub kind: ChartKind,
    pub grouping: Grouping,
    pub markers: bool,
    pub lines: bool,
    pub smooth: bool,
    pub filled: bool,
    /// Combo presets: 0 = none, 1 = column + line, 2 = column + line on secondary axis, 3 = stacked area + column.
    pub combo: u8,
    pub name: &'static str,
}

const fn sub(kind: ChartKind, grouping: Grouping, markers: bool, name: &'static str) -> Subtype {
    Subtype { kind, grouping, markers, lines: false, smooth: false, filled: false, combo: 0, name }
}

pub const SUBTYPES: &[Subtype] = &[
    sub(ChartKind::Column, Grouping::Standard, false, "Clustered Column"),
    sub(ChartKind::Column, Grouping::Stacked, false, "Stacked Column"),
    sub(ChartKind::Column, Grouping::Percent, false, "100% Stacked Column"),
    sub(ChartKind::Bar, Grouping::Standard, false, "Clustered Bar"),
    sub(ChartKind::Bar, Grouping::Stacked, false, "Stacked Bar"),
    sub(ChartKind::Bar, Grouping::Percent, false, "100% Stacked Bar"),
    sub(ChartKind::Line, Grouping::Standard, false, "Line"),
    sub(ChartKind::Line, Grouping::Stacked, false, "Stacked Line"),
    sub(ChartKind::Line, Grouping::Percent, false, "100% Stacked Line"),
    sub(ChartKind::Line, Grouping::Standard, true, "Line with Markers"),
    sub(ChartKind::Line, Grouping::Stacked, true, "Stacked Line with Markers"),
    sub(ChartKind::Line, Grouping::Percent, true, "100% Stacked Line with Markers"),
    sub(ChartKind::Area, Grouping::Standard, false, "Area"),
    sub(ChartKind::Area, Grouping::Stacked, false, "Stacked Area"),
    sub(ChartKind::Area, Grouping::Percent, false, "100% Stacked Area"),
    sub(ChartKind::Pie, Grouping::Standard, false, "Pie"),
    sub(ChartKind::Doughnut, Grouping::Standard, false, "Doughnut"),
    sub(ChartKind::Scatter, Grouping::Standard, true, "Scatter"),
    Subtype { lines: true, smooth: true, ..sub(ChartKind::Scatter, Grouping::Standard, true, "Scatter with Smooth Lines and Markers") },
    Subtype { lines: true, smooth: true, ..sub(ChartKind::Scatter, Grouping::Standard, false, "Scatter with Smooth Lines") },
    Subtype { lines: true, ..sub(ChartKind::Scatter, Grouping::Standard, true, "Scatter with Straight Lines and Markers") },
    Subtype { lines: true, ..sub(ChartKind::Scatter, Grouping::Standard, false, "Scatter with Straight Lines") },
    sub(ChartKind::Radar, Grouping::Standard, false, "Radar"),
    sub(ChartKind::Radar, Grouping::Standard, true, "Radar with Markers"),
    Subtype { filled: true, ..sub(ChartKind::Radar, Grouping::Standard, false, "Filled Radar") },
    Subtype { combo: 1, ..sub(ChartKind::Column, Grouping::Standard, false, "Clustered Column - Line") },
    Subtype { combo: 2, ..sub(ChartKind::Column, Grouping::Standard, false, "Clustered Column - Line on Secondary Axis") },
    Subtype { combo: 3, ..sub(ChartKind::Column, Grouping::Standard, false, "Stacked Area - Clustered Column") },
];

/// Subtypes of one kind (pie and doughnut share a family, as do column and bar in menus).
pub fn subtypes_of(kind: ChartKind) -> impl Iterator<Item = &'static Subtype> {
    SUBTYPES.iter().filter(move |s| s.kind == kind && s.combo == 0)
}

pub fn combo_subtypes() -> impl Iterator<Item = &'static Subtype> {
    SUBTYPES.iter().filter(|s| s.combo != 0)
}

impl Chart {
    /// A chart over `range` of sheet `si`, with the defaults the desktop spreadsheet picks.
    pub fn new(wb: &Workbook, si: usize, range: Range, kind: ChartKind) -> Chart {
        let sh = &wb.sheets[si];
        let range = clip_to_data(sh, range);
        let mut ch = Chart::blank(kind, &sh.name, range);
        ch.by_cols = range.rows() > range.cols();
        ch.anchor = (range.r1, (range.c2 + 2).min(MAX_COLS - 1));
        ch.markers = matches!(kind, ChartKind::Line | ChartKind::Scatter);
        ch.legend = kind.is_round() || resolve(wb, &ch).series.len() > 1;
        ch
    }

    /// A chart with every option at its default and no data detection.
    pub fn blank(kind: ChartKind, sheet: &str, range: Range) -> Chart {
        let xy = kind == ChartKind::Scatter;
        Chart {
            kind,
            sheet: sheet.to_string(),
            range,
            by_cols: true,
            names_first: None,
            cats_first: None,
            custom: None,
            title: None,
            legend: false,
            legend_pos: LegendPos::Bottom,
            labels: Labels::default(),
            data_table: false,
            grouping: Grouping::Standard,
            markers: false,
            lines: false,
            smooth: false,
            filled: false,
            x_axis: Axis { major_grid: xy, ..Default::default() },
            y_axis: Axis { major_grid: true, ..Default::default() },
            y2_axis: Axis::default(),
            series_fmt: Vec::new(),
            palette: 0,
            bg: None,
            plot_bg: None,
            gap: 219,
            overlap: -27,
            hole: 75,
            first_angle: 0,
            anchor: (0, 0),
            offset: (0.0, 0.0),
            size: (DEFAULT_W, DEFAULT_H),
        }
    }

    /// A new chart of the given menu subtype.
    pub fn with_subtype(wb: &Workbook, si: usize, range: Range, st: &Subtype) -> Chart {
        let mut ch = Chart::new(wb, si, range, st.kind);
        ch.apply_subtype(st);
        let n = resolve(wb, &ch).series.len();
        match st.combo {
            1 | 2 if n >= 2 => {
                let f = ch.fmt_mut(n - 1);
                f.kind = Some(ChartKind::Line);
                f.secondary = st.combo == 2;
            }
            3 if n >= 2 => {
                ch.fmt_mut(0).kind = Some(ChartKind::Area);
            }
            _ => {}
        }
        if st.combo != 0 {
            ch.legend = true;
        }
        ch
    }

    /// Changes the chart type, keeping data and formatting.
    pub fn apply_subtype(&mut self, st: &Subtype) {
        let was_xy = self.kind == ChartKind::Scatter;
        self.kind = st.kind;
        self.grouping = if st.kind.can_stack() { st.grouping } else { Grouping::Standard };
        self.markers = st.markers;
        self.lines = st.lines;
        self.smooth = st.smooth;
        self.filled = st.filled;
        let stacked_bars = st.kind.is_bar() && self.grouping != Grouping::Standard;
        (self.gap, self.overlap) = if stacked_bars { (150, 100) } else { (219, -27) };
        if st.kind.is_round() {
            self.legend = true;
        }
        if (st.kind == ChartKind::Scatter) != was_xy {
            self.x_axis.major_grid = st.kind == ChartKind::Scatter;
        }
        if st.combo == 0 {
            for f in &mut self.series_fmt {
                f.kind = None;
                if !st.kind.combinable() && st.kind != ChartKind::Bar {
                    f.secondary = false;
                }
            }
        }
    }

    /// The subtype the chart currently matches, if any.
    pub fn subtype(&self) -> Option<&'static Subtype> {
        SUBTYPES.iter().find(|s| {
            s.combo == 0
                && s.kind == self.kind
                && s.grouping == self.grouping
                && (s.markers == self.markers || !matches!(s.kind, ChartKind::Line | ChartKind::Scatter | ChartKind::Radar))
                && (s.lines == self.lines || s.kind != ChartKind::Scatter)
                && (s.smooth == self.smooth || s.kind != ChartKind::Scatter)
                && s.filled == (self.filled && s.kind == ChartKind::Radar)
        })
    }

    /// Switch Row/Column: series become categories and vice versa.
    pub fn switch_row_col(&mut self) {
        self.custom = None;
        self.by_cols = !self.by_cols;
        std::mem::swap(&mut self.names_first, &mut self.cats_first);
    }

    pub fn fmt(&self, i: usize) -> SeriesFmt {
        self.series_fmt.get(i).cloned().unwrap_or_default()
    }

    pub fn fmt_ref(&self, i: usize) -> Option<&SeriesFmt> {
        self.series_fmt.get(i)
    }

    pub fn fmt_mut(&mut self, i: usize) -> &mut SeriesFmt {
        if self.series_fmt.len() <= i {
            self.series_fmt.resize(i + 1, SeriesFmt::default());
        }
        &mut self.series_fmt[i]
    }

    /// The kind series `i` is drawn as.
    pub fn series_kind(&self, i: usize) -> ChartKind {
        match self.series_fmt.get(i).and_then(|f| f.kind) {
            Some(k) if self.kind.combinable() && k.combinable() => k,
            _ => self.kind,
        }
    }

    /// Series `i` uses the secondary value axis.
    pub fn on_secondary(&self, i: usize) -> bool {
        !self.kind.is_round() && self.kind != ChartKind::Radar && self.series_fmt.get(i).is_some_and(|f| f.secondary)
    }

    /// Stacking of series `i`: the chart's grouping applies to series of the chart's own kind.
    pub fn series_grouping(&self, i: usize) -> Grouping {
        if self.series_kind(i) == self.kind { self.grouping } else { Grouping::Standard }
    }

    /// Data labels of series `i`.
    pub fn series_labels(&self, i: usize) -> &Labels {
        self.series_fmt.get(i).and_then(|f| f.labels.as_ref()).unwrap_or(&self.labels)
    }

    pub fn any_labels(&self) -> bool {
        self.labels.any() || self.series_fmt.iter().any(|f| f.labels.as_ref().is_some_and(Labels::any))
    }

    pub fn has_secondary(&self, nseries: usize) -> bool {
        (0..nseries).any(|i| self.on_secondary(i))
    }

    /// Colour of series `i` (or of point `i` on pie charts).
    pub fn color(&self, i: usize) -> u32 {
        palette_color(self.palette, i)
    }

    /// Fill of series `i`, honouring its format.
    pub fn series_fill(&self, i: usize) -> u32 {
        self.series_fmt.get(i).and_then(|f| f.fill).unwrap_or_else(|| self.color(i))
    }

    /// Line colour of series `i`.
    pub fn series_line(&self, i: usize) -> u32 {
        self.series_fmt.get(i).and_then(|f| f.line.or(if self.series_kind(i) == ChartKind::Area { None } else { f.fill })).unwrap_or_else(|| self.color(i))
    }

    /// Whether the chart is drawn with axes at all.
    pub fn has_axes(&self) -> bool {
        !self.kind.is_round()
    }
}

// ---------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------

/// Change Colors palettes: (name, base colours). Colours past the end repeat darker.
pub const PALETTES: [(&str, [u32; 6]); 10] = [
    ("Colorful Palette 1", [0x4472C4, 0xED7D31, 0xA5A5A5, 0xFFC000, 0x5B9BD5, 0x70AD47]),
    ("Colorful Palette 2", [0xED7D31, 0xFFC000, 0x70AD47, 0x9E480E, 0x997300, 0x43682B]),
    ("Colorful Palette 3", [0xA5A5A5, 0x5B9BD5, 0x4472C4, 0x636363, 0x255E91, 0x264478]),
    ("Colorful Palette 4", [0x70AD47, 0x5B9BD5, 0xFFC000, 0xA5A5A5, 0xED7D31, 0x4472C4]),
    ("Monochromatic Palette 1", [0x4472C4, 0x264478, 0x698ED0, 0x335AA1, 0x8FAADC, 0x203864]),
    ("Monochromatic Palette 2", [0xED7D31, 0x9E480E, 0xF19E66, 0xC55A11, 0xF4B183, 0x843C0C]),
    ("Monochromatic Palette 3", [0xA5A5A5, 0x636363, 0xB7B7B7, 0x7B7B7B, 0xC9C9C9, 0x525252]),
    ("Monochromatic Palette 4", [0xFFC000, 0x997300, 0xFFCD33, 0xBF9000, 0xFFD966, 0x7F6000]),
    ("Monochromatic Palette 5", [0x5B9BD5, 0x255E91, 0x7CAFDD, 0x2E75B6, 0x9DC3E6, 0x1F4E79]),
    ("Monochromatic Palette 6", [0x70AD47, 0x43682B, 0x8CC168, 0x548235, 0xA9D18E, 0x385723]),
];

/// The i-th series colour of a palette; later cycles are shaded darker, then lighter.
pub fn palette_color(palette: u8, i: usize) -> u32 {
    let base = PALETTES[(palette as usize) % PALETTES.len()].1;
    let c = base[i % 6];
    match (i / 6) % 4 {
        0 => c,
        1 => shade(c, 0.6),
        2 => shade(c, 1.4),
        _ => shade(c, 0.8),
    }
}

fn shade(c: u32, f: f32) -> u32 {
    let ch = |s: u32| {
        let v = ((c >> s) & 0xFF) as f32;
        let v = if f <= 1.0 { v * f } else { v + (255.0 - v) * (f - 1.0) };
        (v.round().clamp(0.0, 255.0) as u32) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// Quick Layout presets (Layout 1-11), applied to a chart.
pub fn apply_quick_layout(ch: &mut Chart, n: u8) {
    let axes = ch.has_axes();
    let (title, legend, pos, labels, xt, yt, table, ygrid) = match n {
        1 => (true, true, LegendPos::Right, false, false, false, false, true),
        2 => (true, true, LegendPos::Top, true, false, false, false, false),
        3 => (true, true, LegendPos::Bottom, false, false, false, false, true),
        4 => (false, true, LegendPos::Bottom, true, false, false, false, false),
        5 => (true, false, LegendPos::Bottom, false, false, true, true, true),
        6 => (true, false, LegendPos::Bottom, false, false, true, false, true),
        7 => (false, true, LegendPos::Right, false, true, true, false, true),
        8 => (true, false, LegendPos::Bottom, false, true, true, false, false),
        9 => (true, true, LegendPos::Right, false, true, true, false, true),
        10 => (true, true, LegendPos::Right, true, false, false, false, true),
        _ => (false, true, LegendPos::TopRight, false, false, false, false, false),
    };
    ch.title = if title { if ch.title.as_deref() == Some("") { None } else { ch.title.clone() } } else { Some(String::new()) };
    ch.legend = legend;
    ch.legend_pos = pos;
    ch.labels = if labels { Labels { value: true, percent: false, ..ch.labels.clone() } } else { Labels { pos: ch.labels.pos, ..Default::default() } };
    if ch.kind.is_round() && labels && n == 2 {
        ch.labels.percent = true;
        ch.labels.value = false;
    }
    if axes {
        ch.x_axis.title = if xt { ch.x_axis.title.clone().or(Some(String::new())) } else { None };
        ch.y_axis.title = if yt { ch.y_axis.title.clone().or(Some(String::new())) } else { None };
        ch.data_table = table && ch.kind != ChartKind::Scatter && ch.kind != ChartKind::Radar;
        ch.y_axis.major_grid = ygrid;
        ch.y_axis.visible = !(n == 2 || n == 4);
        if n == 10 && ch.kind.is_bar() {
            ch.gap = 0;
        }
    }
}

/// Chart Styles gallery: (chart area fill, plot area fill, gridlines, labels).
pub const STYLES: [(Option<u32>, Option<u32>, bool, bool); 8] = [
    (None, None, true, false),
    (None, None, false, true),
    (None, Some(0xF2F2F2), true, false),
    (None, None, false, false),
    (Some(0xF2F2F2), None, true, false),
    (Some(0xD9E1F2), Some(0xFFFFFF), true, false),
    (Some(0x404040), None, true, false),
    (Some(0x262626), Some(0x404040), false, true),
];

pub fn apply_style(ch: &mut Chart, k: usize) {
    let (bg, plot, grid, labels) = STYLES[k % STYLES.len()];
    ch.bg = bg;
    ch.plot_bg = plot;
    if ch.has_axes() {
        ch.y_axis.major_grid = grid;
    }
    ch.labels = if labels { Labels { value: !ch.kind.is_round(), percent: ch.kind.is_round(), ..ch.labels.clone() } } else { Labels { pos: ch.labels.pos, num_fmt: ch.labels.num_fmt.clone(), ..Default::default() } };
}

/// Whether a colour is dark enough to need light text on top.
pub fn is_dark(c: u32) -> bool {
    let (r, g, b) = (((c >> 16) & 0xFF) as f32, ((c >> 8) & 0xFF) as f32, (c & 0xFF) as f32);
    0.299 * r + 0.587 * g + 0.114 * b < 128.0
}

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct SeriesData {
    pub name: String,
    /// Cell holding the series name, if any.
    pub name_cell: Option<(u32, u32)>,
    pub values: Vec<Option<f64>>,
    pub refr: Range,
    /// Scatter X values of this series when they differ from the shared categories.
    pub xs: Option<Vec<Option<f64>>>,
    pub xs_ref: Option<Range>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ChartData {
    pub categories: Vec<String>,
    /// Numeric category values (X values for scatter charts).
    pub cat_values: Vec<Option<f64>>,
    pub cat_ref: Option<Range>,
    pub series: Vec<SeriesData>,
    /// Number format of the first value cell, for axis and data labels.
    pub num_fmt: String,
}

impl ChartData {
    pub fn points(&self) -> usize {
        self.series.iter().map(|s| s.values.len()).max().unwrap_or(0).max(self.categories.len())
    }
}

/// Limits whole-row / whole-column selections to the populated part of the sheet.
pub fn clip_to_data(sh: &Sheet, rg: Range) -> Range {
    let (er, ec) = sh.extent().unwrap_or((0, 0));
    let r2 = rg.r2.min(er.max(rg.r1));
    let c2 = rg.c2.min(ec.max(rg.c1));
    Range::new(rg.r1, rg.c1, r2, c2)
}

fn is_num(v: &Value) -> bool {
    matches!(v, Value::Number(_))
}

/// Whether the first row / first column of the range hold labels, following the
/// familiar rules: a blank top-left cell means both; otherwise a line made only of
/// text (no numbers) is a label line.
pub fn detect_labels(sh: &Sheet, rg: Range) -> (bool, bool) {
    if rg.rows() > 1 && rg.cols() > 1 && sh.value(rg.r1, rg.c1).is_empty() {
        return (true, true);
    }
    let row = rg.rows() > 1 && {
        let from = if rg.cols() > 1 { rg.c1 + 1 } else { rg.c1 };
        (from..=rg.c2).all(|c| !is_num(sh.value(rg.r1, c)))
            && (rg.c1..=rg.c2).any(|c| matches!(sh.value(rg.r1, c), Value::Text(_)))
    };
    let col = rg.cols() > 1 && {
        (rg.r1 + 1..=rg.r2).all(|r| !is_num(sh.value(r, rg.c1)))
            && (rg.r1..=rg.r2).any(|r| matches!(sh.value(r, rg.c1), Value::Text(_)))
    };
    (row, col)
}

fn display(wb: &Workbook, sh: &Sheet, r: u32, c: u32) -> String {
    match sh.get(r, c) {
        Some(cell) => numfmt::format_value(&cell.value, &wb.style(cell.style).num_fmt).text,
        None => String::new(),
    }
}

/// Reads the series, names and categories of a chart from its source cells.
pub fn resolve(wb: &Workbook, ch: &Chart) -> ChartData {
    let Some(si) = wb.sheet_index(&ch.sheet) else { return ChartData::default() };
    let sh = &wb.sheets[si];
    if let Some(cu) = &ch.custom {
        return resolve_custom(wb, sh, cu);
    }
    let rg = clip_to_data(sh, ch.range);
    let (label_row, label_col) = detect_labels(sh, rg);
    let (mut names, mut cats) = if ch.by_cols { (label_row, label_col) } else { (label_col, label_row) };
    if let Some(n) = ch.names_first {
        names = n;
    }
    if let Some(c) = ch.cats_first {
        cats = c;
    }
    let (lines, points) = if ch.by_cols { (rg.cols(), rg.rows()) } else { (rg.rows(), rg.cols()) };
    // Scatter: with two or more columns the first holds the X values.
    if ch.kind == ChartKind::Scatter && ch.cats_first.is_none() && lines >= 2 {
        cats = true;
    }
    if cats && lines < 2 {
        cats = false;
    }
    if names && points < 2 {
        names = false;
    }
    let at = |line: u32, point: u32| -> (u32, u32) {
        if ch.by_cols { (rg.r1 + point, rg.c1 + line) } else { (rg.r1 + line, rg.c1 + point) }
    };
    let span = |line: u32, p1: u32, p2: u32| -> Range {
        let (a, b) = (at(line, p1), at(line, p2));
        Range::new(a.0, a.1, b.0, b.1)
    };
    let point0 = u32::from(names);
    let line0 = u32::from(cats);
    let npoints = points.saturating_sub(point0).min(MAX_POINTS as u32);
    let mut data = ChartData::default();
    if npoints == 0 {
        return data;
    }
    let last = point0 + npoints - 1;
    if cats {
        data.cat_ref = Some(span(0, point0, last));
        for p in point0..=last {
            let (r, c) = at(0, p);
            data.categories.push(display(wb, sh, r, c));
            data.cat_values.push(sh.value(r, c).as_number());
        }
    } else {
        data.categories = (1..=npoints).map(|i| i.to_string()).collect();
        data.cat_values = (1..=npoints).map(|i| Some(i as f64)).collect();
    }
    for l in line0..lines {
        let name_cell = names.then(|| at(l, 0));
        let name = match name_cell {
            Some((r, c)) => display(wb, sh, r, c),
            None => format!("Series{}", l - line0 + 1),
        };
        let values = (point0..=last).map(|p| {
            let (r, c) = at(l, p);
            sh.value(r, c).as_number()
        });
        data.series.push(SeriesData { name, name_cell, values: values.collect(), refr: span(l, point0, last), xs: None, xs_ref: None });
    }
    if let Some(s) = data.series.first() {
        if let Some(cell) = sh.get(s.refr.r1, s.refr.c1) {
            data.num_fmt = wb.style(cell.style).num_fmt.clone();
        }
    }
    data
}

/// Cells of a range in reading order, capped at `MAX_POINTS`.
fn cells_of(rg: Range) -> impl Iterator<Item = (u32, u32)> {
    (rg.r1..=rg.r2).flat_map(move |r| (rg.c1..=rg.c2).map(move |c| (r, c))).take(MAX_POINTS)
}

fn resolve_custom(wb: &Workbook, sh: &Sheet, cu: &Custom) -> ChartData {
    let mut data = ChartData::default();
    let nums = |rg: Range| -> Vec<Option<f64>> { cells_of(clip_to_data(sh, rg)).map(|(r, c)| sh.value(r, c).as_number()).collect() };
    for (k, s) in cu.series.iter().enumerate() {
        let name = match s.name_cell {
            Some((r, c)) => display(wb, sh, r, c),
            None if !s.name.is_empty() => s.name.clone(),
            None => format!("Series{}", k + 1),
        };
        let refr = s.values.map(|v| clip_to_data(sh, v)).unwrap_or_default();
        let values = s.values.map(nums).unwrap_or_default();
        let xs = s.xs.map(nums);
        data.series.push(SeriesData { name, name_cell: s.name_cell, values, refr, xs, xs_ref: s.xs });
    }
    let n = data.series.iter().map(|s| s.values.len()).max().unwrap_or(0);
    match cu.cats {
        Some(rg) => {
            let rg = clip_to_data(sh, rg);
            data.cat_ref = Some(rg);
            for (r, c) in cells_of(rg) {
                data.categories.push(display(wb, sh, r, c));
                data.cat_values.push(sh.value(r, c).as_number());
            }
        }
        None => {
            data.categories = (1..=n).map(|i| i.to_string()).collect();
            data.cat_values = (1..=n).map(|i| Some(i as f64)).collect();
        }
    }
    if let Some(s) = data.series.iter().find(|s| s.values.iter().any(Option::is_some)) {
        if let Some(cell) = sh.get(s.refr.r1, s.refr.c1) {
            data.num_fmt = wb.style(cell.style).num_fmt.clone();
        }
    }
    data
}

/// The explicit series list equivalent to what the chart shows now (Select Data starts from it).
pub fn to_custom(wb: &Workbook, ch: &Chart) -> Custom {
    if let Some(c) = &ch.custom {
        return c.clone();
    }
    let data = resolve(wb, ch);
    let xy = ch.kind == ChartKind::Scatter;
    Custom {
        series: data
            .series
            .iter()
            .map(|s| SeriesSrc {
                name_cell: s.name_cell,
                name: if s.name_cell.is_some() { String::new() } else { s.name.clone() },
                values: Some(s.refr),
                xs: if xy { data.cat_ref } else { None },
            })
            .collect(),
        cats: if xy { None } else { data.cat_ref },
    }
}

/// Bounding range of every source cell of a custom series list.
pub fn custom_bounds(cu: &Custom) -> Option<Range> {
    let mut all = cu.series.iter().flat_map(|s| [s.values, s.xs, s.name_cell.map(|(r, c)| Range::cell(r, c))]).flatten().chain(cu.cats);
    let first = all.next()?;
    Some(all.fold(first, |a, b| a.union(&b)))
}

/// Title shown on the chart, `None` when hidden.
pub fn display_title(ch: &Chart, data: &ChartData) -> Option<String> {
    match &ch.title {
        Some(t) if t.is_empty() => None,
        Some(t) => Some(t.clone()),
        None => match data.series.as_slice() {
            [s] if s.name_cell.is_some() => Some(s.name.clone()),
            _ => Some("Chart Title".into()),
        },
    }
}

// ---------------------------------------------------------------------------
// Axis scaling
// ---------------------------------------------------------------------------

/// "Nice" value axis: (min, max, major step). Starts at zero for positive data unless
/// the values sit far from it, and leaves a little headroom above the largest value.
pub fn nice_axis(min: f64, max: f64) -> (f64, f64, f64) {
    let (mut min, mut max) = if min.is_finite() && max.is_finite() { (min.min(max), min.max(max)) } else { (0.0, 1.0) };
    if min == max {
        if min == 0.0 {
            max = 1.0;
        } else if min > 0.0 {
            min = 0.0;
        } else {
            max = 0.0;
        }
    }
    if min > 0.0 && (max - min) / max > 1.0 / 6.0 {
        min = 0.0;
    }
    if max < 0.0 && (min - max) / min > 1.0 / 6.0 {
        max = 0.0;
    }
    let step = nice_step(max - min);
    let pad = (max - min) * 0.05;
    let lo = if min == 0.0 { 0.0 } else { ((min - if min < 0.0 { pad } else { 0.0 }) / step).floor() * step };
    let hi = if max == 0.0 { 0.0 } else { ((max + if max > 0.0 { pad } else { 0.0 }) / step).ceil() * step };
    (lo, hi.max(lo + step), step)
}

/// A round major unit splitting `span` into roughly eight steps.
pub fn nice_step(span: f64) -> f64 {
    let raw = (span.abs() / 8.0).max(f64::MIN_POSITIVE);
    let mag = 10f64.powf(raw.log10().floor());
    let norm = raw / mag;
    mag * if norm <= 1.0 { 1.0 } else if norm <= 2.0 { 2.0 } else if norm <= 5.0 { 5.0 } else { 10.0 }
}

/// A value axis: bounds, units and direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scale {
    pub lo: f64,
    pub hi: f64,
    pub major: f64,
    pub minor: f64,
    pub log: Option<f64>,
    pub reverse: bool,
}

impl Scale {
    /// Position of `v` along the axis, 0 at the start and 1 at the end.
    pub fn frac(&self, v: f64) -> f64 {
        let f = match self.log {
            Some(_) => {
                let (a, b) = (self.lo.ln(), self.hi.ln());
                (v.max(self.lo * 1e-6).ln() - a) / (b - a).max(f64::MIN_POSITIVE)
            }
            None => (v - self.lo) / (self.hi - self.lo).max(f64::MIN_POSITIVE),
        };
        if self.reverse { 1.0 - f } else { f }
    }

    /// Where bars and areas start: zero, or the nearest bound.
    pub fn base(&self) -> f64 {
        match self.log {
            Some(_) => self.lo,
            None => 0.0f64.clamp(self.lo, self.hi),
        }
    }

    pub fn clamp(&self, v: f64) -> f64 {
        v.clamp(self.lo, self.hi)
    }

    pub fn ticks(&self) -> Vec<f64> {
        match self.log {
            Some(b) => {
                let mut v = Vec::new();
                let mut x = self.lo;
                while x <= self.hi * (1.0 + 1e-9) && v.len() < 60 {
                    v.push(x);
                    x *= b;
                }
                v
            }
            None => {
                let k = ((self.hi - self.lo) / self.major + 1e-9).floor().clamp(1.0, 200.0) as usize;
                (0..=k).map(|i| self.lo + self.major * i as f64).collect()
            }
        }
    }

    pub fn minor_ticks(&self) -> Vec<f64> {
        match self.log {
            Some(b) => {
                let mut v = Vec::new();
                let mut x = self.lo;
                while x < self.hi && v.len() < 400 {
                    for k in 2..(b.round() as i64).clamp(2, 10) {
                        v.push(x * k as f64);
                    }
                    x *= b;
                }
                v
            }
            None => {
                let k = ((self.hi - self.lo) / self.minor + 1e-9).floor().clamp(1.0, 400.0) as usize;
                (0..=k).map(|i| self.lo + self.minor * i as f64).collect()
            }
        }
    }
}

/// The value axis for data spanning `min..max`, honouring the axis options.
pub fn scale_for(min: f64, max: f64, ax: &Axis, percent: bool) -> Scale {
    if let Some(b) = ax.log_base.filter(|b| *b >= 2.0) {
        let lmin = if min > 0.0 && min.is_finite() { min } else { 1.0 };
        let lmax = if max > 0.0 && max.is_finite() { max } else { lmin };
        let lg = |v: f64| v.ln() / b.ln();
        let mut lo = ax.min.filter(|v| *v > 0.0).unwrap_or_else(|| b.powf(lg(lmin).floor()));
        let mut hi = ax.max.filter(|v| *v > 0.0).unwrap_or_else(|| b.powf(lg(lmax).ceil()));
        if hi <= lo {
            hi = lo * b;
        }
        if lo <= 0.0 {
            lo = 1.0;
        }
        return Scale { lo, hi, major: b, minor: b, log: Some(b), reverse: ax.reverse };
    }
    let (mut lo, mut hi, mut step) = if percent {
        (if min < -1e-12 { -1.0 } else { 0.0 }, if max > 1e-12 || min >= 0.0 { 1.0 } else { 0.0 }, 0.1)
    } else {
        nice_axis(min, max)
    };
    if let Some(m) = ax.min {
        lo = m;
    }
    if let Some(m) = ax.max {
        hi = m;
    }
    if hi <= lo {
        hi = lo + step.max(1.0);
    }
    if (ax.min.is_some() || ax.max.is_some()) && !percent {
        step = nice_step(hi - lo);
    }
    if let Some(m) = ax.major.filter(|m| *m > 0.0) {
        step = m;
    }
    // Keep the tick count sane.
    while (hi - lo) / step > 200.0 {
        step *= 10.0;
    }
    let minor = ax.minor.filter(|m| *m > 0.0).unwrap_or(step / 5.0);
    Scale { lo, hi, major: step, minor, log: None, reverse: ax.reverse }
}

/// Text for an axis tick or data label.
pub fn format_number(v: f64, fmt: &str, step: f64) -> String {
    let general = fmt.trim().is_empty() || fmt.eq_ignore_ascii_case("general");
    if !general {
        return numfmt::format_value(&Value::Number(v), fmt).text;
    }
    let decimals = if step > 0.0 && step < 1.0 { (-step.log10().floor()) as i32 + 1 } else { 0 };
    let p = 10f64.powi(decimals.clamp(0, 10));
    let v = if step > 0.0 { (v * p).round() / p } else { v };
    numfmt::general(if v == 0.0 { 0.0 } else { v })
}

// ---------------------------------------------------------------------------
// Sheet geometry (unzoomed pixels)
// ---------------------------------------------------------------------------

/// Left edge of column `c`, measured from column A.
pub fn col_x(sh: &Sheet, c: u32) -> f32 {
    let mut x = c as f32 * DEFAULT_COL_W;
    for (&k, &w) in &sh.col_w {
        if k < c {
            x += w - DEFAULT_COL_W;
        }
    }
    for &k in &sh.hidden_cols {
        if k < c {
            x -= sh.raw_col_width(k);
        }
    }
    x
}

/// Top edge of row `r`, measured from row 1.
pub fn row_y(sh: &Sheet, r: u32) -> f32 {
    let mut y = r as f32 * DEFAULT_ROW_H;
    for (&k, &h) in &sh.row_h {
        if k < r {
            y += h - DEFAULT_ROW_H;
        }
    }
    for &k in &sh.hidden_rows {
        if k < r {
            y -= sh.raw_row_height(k);
        }
    }
    y
}

/// Column containing sheet x, and the offset into it.
pub fn col_at_x(sh: &Sheet, x: f32) -> (u32, f32) {
    let mut left = 0.0;
    let mut c = 0;
    while c < MAX_COLS - 1 {
        let w = sh.col_width(c);
        if x < left + w {
            break;
        }
        left += w;
        c += 1;
    }
    (c, (x - left).max(0.0))
}

/// Row containing sheet y, and the offset into it.
pub fn row_at_y(sh: &Sheet, y: f32) -> (u32, f32) {
    let mut top = 0.0;
    let mut r = 0;
    while r < MAX_ROWS - 1 {
        let h = sh.row_height(r);
        if y < top + h {
            break;
        }
        top += h;
        r += 1;
    }
    (r, (y - top).max(0.0))
}

/// Chart rectangle in sheet pixels: (x, y, w, h).
pub fn sheet_rect(sh: &Sheet, ch: &Chart) -> (f32, f32, f32, f32) {
    (col_x(sh, ch.anchor.1) + ch.offset.0, row_y(sh, ch.anchor.0) + ch.offset.1, ch.size.0, ch.size.1)
}

/// Places a chart at the given sheet-pixel rectangle.
pub fn set_sheet_rect(sh: &Sheet, ch: &mut Chart, x: f32, y: f32, w: f32, h: f32) {
    let (c, ox) = col_at_x(sh, x.max(0.0));
    let (r, oy) = row_at_y(sh, y.max(0.0));
    ch.anchor = (r, c);
    ch.offset = (ox, oy);
    ch.size = (w.max(MIN_SIZE), h.max(MIN_SIZE));
}

// ---------------------------------------------------------------------------
// Keeping charts in step with sheet edits
// ---------------------------------------------------------------------------

/// Adjusts chart sources and anchors after `n` rows/columns were inserted (n > 0)
/// or deleted (n < 0) at `at` on sheet `si`.
pub fn shift_for_structure(wb: &mut Workbook, si: usize, rows: bool, at: u32, n: i64) {
    let name = wb.sheets[si].name.clone();
    let max = if rows { MAX_ROWS } else { MAX_COLS } as i64;
    let map = |x: u32, end: bool| -> Option<u32> {
        let x = x as i64;
        let at = at as i64;
        let y = if n > 0 {
            if x >= at { x + n } else { x }
        } else if x >= at - n {
            x + n
        } else if x >= at {
            // inside the deleted band: start edges snap to `at`, end edges to just before it
            if end { at - 1 } else { at }
        } else {
            x
        };
        (y >= 0).then_some(y.min(max - 1) as u32)
    };
    for (i, sh) in wb.sheets.iter_mut().enumerate() {
        for ch in &mut sh.charts {
            if ch.sheet.eq_ignore_ascii_case(&name) {
                let shift = |rg: Range| -> Range {
                    let (a, b) = if rows { (rg.r1, rg.r2) } else { (rg.c1, rg.c2) };
                    match (map(a, false), map(b, true)) {
                        (Some(na), Some(nb)) if nb >= na => {
                            if rows { Range { r1: na, r2: nb, ..rg } } else { Range { c1: na, c2: nb, ..rg } }
                        }
                        _ => rg,
                    }
                };
                ch.range = shift(ch.range);
                if let Some(cu) = &mut ch.custom {
                    cu.cats = cu.cats.map(shift);
                    for s in &mut cu.series {
                        s.values = s.values.map(shift);
                        s.xs = s.xs.map(shift);
                        s.name_cell = s.name_cell.map(|(r, c)| {
                            let g = shift(Range::cell(r, c));
                            (g.r1, g.c1)
                        });
                    }
                }
            }
            if i == si {
                let a = if rows { ch.anchor.0 } else { ch.anchor.1 };
                let na = map(a, false).unwrap_or(0);
                if rows {
                    ch.anchor.0 = na;
                } else {
                    ch.anchor.1 = na;
                }
            }
        }
    }
}

/// Points charts at a renamed sheet.
pub fn rename_sheet(wb: &mut Workbook, old: &str, new: &str) {
    for sh in &mut wb.sheets {
        for ch in &mut sh.charts {
            if ch.sheet.eq_ignore_ascii_case(old) {
                ch.sheet = new.to_string();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Cell;

    fn put(sh: &mut Sheet, r: u32, c: u32, v: Value) {
        sh.set(r, c, Some(Cell { value: v, ..Default::default() }));
    }

    fn sample() -> Workbook {
        // Month | Sales | Cost
        let mut sh = Sheet::new("Data");
        put(&mut sh, 0, 0, Value::Text("Month".into()));
        put(&mut sh, 0, 1, Value::Text("Sales".into()));
        put(&mut sh, 0, 2, Value::Text("Cost".into()));
        for (i, m) in ["Jan", "Feb", "Mar", "Apr"].iter().enumerate() {
            let r = i as u32 + 1;
            put(&mut sh, r, 0, Value::Text(m.to_string()));
            put(&mut sh, r, 1, Value::Number(10.0 * r as f64));
            put(&mut sh, r, 2, Value::Number(5.0 * r as f64));
        }
        Workbook::empty_with(vec![sh])
    }

    #[test]
    fn nice_axis_steps() {
        assert_eq!(nice_axis(0.0, 100.0), (0.0, 120.0, 20.0));
        assert_eq!(nice_axis(3.0, 37.0), (0.0, 40.0, 5.0));
        let (lo, _, step) = nice_axis(0.0, 0.0);
        assert_eq!((lo, step), (0.0, 0.2));
        let (lo, hi, step) = nice_axis(-12.0, 30.0);
        assert!(lo <= -12.0 && hi >= 30.0 && lo < 0.0);
        assert_eq!(step, 10.0);
        // Values far from zero do not force a zero baseline.
        let (lo, hi, _) = nice_axis(1000.0, 1010.0);
        assert!(lo > 900.0 && lo <= 1000.0 && hi >= 1010.0);
        // Small fractions.
        let (lo, hi, step) = nice_axis(0.0, 0.7);
        assert_eq!((lo, step), (0.0, 0.1));
        assert!((hi - 0.8).abs() < 1e-9);
        assert_eq!(format_number(0.30000000000000004, "", 0.1), "0.3");
    }

    #[test]
    fn series_in_columns_with_labels() {
        let wb = sample();
        let ch = Chart::new(&wb, 0, Range::new(0, 0, 4, 2), ChartKind::Column);
        assert!(ch.by_cols);
        assert!(ch.legend);
        let d = resolve(&wb, &ch);
        assert_eq!(d.categories, vec!["Jan", "Feb", "Mar", "Apr"]);
        assert_eq!(d.cat_ref, Some(Range::new(1, 0, 4, 0)));
        assert_eq!(d.series.len(), 2);
        assert_eq!(d.series[0].name, "Sales");
        assert_eq!(d.series[0].name_cell, Some((0, 1)));
        assert_eq!(d.series[0].refr, Range::new(1, 1, 4, 1));
        assert_eq!(d.series[1].values, vec![Some(5.0), Some(10.0), Some(15.0), Some(20.0)]);
        assert_eq!(display_title(&ch, &d).as_deref(), Some("Chart Title"));
    }

    #[test]
    fn series_in_rows_and_switch() {
        let wb = sample();
        let mut ch = Chart::new(&wb, 0, Range::new(0, 0, 4, 2), ChartKind::Line);
        ch.switch_row_col();
        let d = resolve(&wb, &ch);
        assert_eq!(d.series.len(), 4);
        assert_eq!(d.series[0].name, "Jan");
        assert_eq!(d.categories, vec!["Sales", "Cost"]);
        assert_eq!(d.series[3].values, vec![Some(40.0), Some(20.0)]);
        // Wide range: series run across rows automatically.
        let ch = Chart::new(&wb, 0, Range::new(0, 0, 1, 2), ChartKind::Column);
        assert!(!ch.by_cols);
    }

    #[test]
    fn single_series_title_and_plain_numbers() {
        let wb = sample();
        let ch = Chart::new(&wb, 0, Range::new(0, 1, 4, 1), ChartKind::Pie);
        let d = resolve(&wb, &ch);
        assert_eq!(d.series.len(), 1);
        assert_eq!(d.categories, vec!["1", "2", "3", "4"]);
        assert_eq!(display_title(&ch, &d).as_deref(), Some("Sales"));
        // Numbers only: no labels at all.
        let ch = Chart::new(&wb, 0, Range::new(1, 1, 4, 2), ChartKind::Column);
        let d = resolve(&wb, &ch);
        assert_eq!(d.series.len(), 2);
        assert_eq!(d.series[0].name, "Series1");
        assert!(d.cat_ref.is_none());
        // Scatter takes the first column as X values.
        let ch = Chart::new(&wb, 0, Range::new(1, 1, 4, 2), ChartKind::Scatter);
        let d = resolve(&wb, &ch);
        assert_eq!(d.series.len(), 1);
        assert_eq!(d.cat_values, vec![Some(10.0), Some(20.0), Some(30.0), Some(40.0)]);
    }

    #[test]
    fn structure_changes_move_sources() {
        let mut wb = sample();
        let ch = Chart::new(&wb, 0, Range::new(0, 0, 4, 2), ChartKind::Column);
        wb.sheets[0].charts.push(ch);
        shift_for_structure(&mut wb, 0, true, 0, 2);
        assert_eq!(wb.sheets[0].charts[0].range, Range::new(2, 0, 6, 2));
        assert_eq!(wb.sheets[0].charts[0].anchor.0, 2);
        shift_for_structure(&mut wb, 0, false, 1, -1);
        assert_eq!(wb.sheets[0].charts[0].range, Range::new(2, 0, 6, 1));
        rename_sheet(&mut wb, "data", "Numbers");
        assert_eq!(wb.sheets[0].charts[0].sheet, "Numbers");
    }

    #[test]
    fn geometry_round_trip() {
        let mut sh = Sheet::new("S");
        sh.col_w.insert(1, 100.0);
        sh.hidden_cols.insert(3);
        assert_eq!(col_x(&sh, 2), 164.0);
        assert_eq!(col_x(&sh, 5), 164.0 + 64.0 + 64.0);
        assert_eq!(col_at_x(&sh, 170.0), (2, 6.0));
        assert_eq!(col_at_x(&sh, 230.0), (4, 2.0));
        assert_eq!(row_at_y(&sh, 45.0), (2, 5.0));
    }

    #[test]
    fn xlsx_round_trip() {
        let mut wb = sample();
        let mut ch = Chart::new(&wb, 0, Range::new(0, 0, 4, 2), ChartKind::Bar);
        ch.title = Some("Sales vs Cost".into());
        ch.labels = Labels::values();
        ch.offset = (10.0, 4.0);
        wb.sheets[0].charts.push(ch.clone());
        let mut pie = Chart::new(&wb, 0, Range::new(0, 1, 4, 1), ChartKind::Pie);
        pie.anchor = (10, 1);
        pie.legend = false;
        wb.sheets[0].charts.push(pie.clone());
        let mut line = Chart::new(&wb, 0, Range::new(0, 0, 4, 2), ChartKind::Line);
        line.switch_row_col();
        line.title = Some(String::new());
        wb.sheets[0].charts.push(line.clone());

        let dir = std::env::temp_dir().join(format!("freedom_sheets_chart_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("chart.xlsx");
        crate::io::save(&wb, &path).unwrap();
        let back = crate::io::load(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let charts = &back.sheets[0].charts;
        assert_eq!(charts.len(), 3);
        let find = |k: ChartKind| charts.iter().find(|c| c.kind == k).unwrap();

        let a = find(ChartKind::Bar);
        assert_eq!(a.kind, ChartKind::Bar);
        assert_eq!(a.sheet, "Data");
        assert_eq!(a.range, ch.range);
        assert!(a.by_cols);
        assert_eq!(a.title.as_deref(), Some("Sales vs Cost"));
        assert!(a.legend && a.labels.value);
        assert_eq!(a.anchor, ch.anchor);
        assert!((a.offset.0 - 10.0).abs() < 1.5 && (a.offset.1 - 4.0).abs() < 1.5);
        assert!((a.size.0 - DEFAULT_W).abs() < 2.0 && (a.size.1 - DEFAULT_H).abs() < 2.0);
        assert_eq!(resolve(&back, a), resolve(&wb, &ch));

        let b = find(ChartKind::Pie);
        assert_eq!(b.kind, ChartKind::Pie);
        assert_eq!(b.range, pie.range);
        assert_eq!(b.title, None);
        assert!(!b.legend);
        assert_eq!(b.anchor, (10, 1));

        let c = find(ChartKind::Line);
        assert_eq!(c.kind, ChartKind::Line);
        assert!(!c.by_cols);
        assert_eq!(c.title.as_deref(), Some(""));
        assert_eq!(resolve(&back, c), resolve(&wb, &line));
    }
}
