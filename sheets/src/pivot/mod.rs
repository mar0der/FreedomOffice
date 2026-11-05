//! PivotTables: the report definition, its data cache and the commands that change it.
//! `engine` computes a report and lays it out in cells; `style` gives PivotTable styles
//! their look.

pub mod engine;
pub mod style;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::model::{Range, Value, Workbook};

pub use engine::{Item, Line, LineKind, Output, Role};

/// Stands for the Σ Values pseudo-field in the row or column fields.
pub const VALUES: usize = usize::MAX;

// ---------------------------------------------------------------------------
// Value fields
// ---------------------------------------------------------------------------

/// "Summarize value field by" functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Agg {
    #[default]
    Sum,
    Count,
    Average,
    Max,
    Min,
    Product,
    CountNums,
    StdDev,
    StdDevP,
    Var,
    VarP,
}

impl Agg {
    pub const ALL: [Agg; 11] =
        [Agg::Sum, Agg::Count, Agg::Average, Agg::Max, Agg::Min, Agg::Product, Agg::CountNums, Agg::StdDev, Agg::StdDevP, Agg::Var, Agg::VarP];

    pub fn label(self) -> &'static str {
        match self {
            Agg::Sum => "Sum",
            Agg::Count => "Count",
            Agg::Average => "Average",
            Agg::Max => "Max",
            Agg::Min => "Min",
            Agg::Product => "Product",
            Agg::CountNums => "Count Numbers",
            Agg::StdDev => "StdDev",
            Agg::StdDevP => "StdDevp",
            Agg::Var => "Var",
            Agg::VarP => "Varp",
        }
    }

    pub fn xml(self) -> &'static str {
        match self {
            Agg::Sum => "sum",
            Agg::Count => "count",
            Agg::Average => "average",
            Agg::Max => "max",
            Agg::Min => "min",
            Agg::Product => "product",
            Agg::CountNums => "countNums",
            Agg::StdDev => "stdDev",
            Agg::StdDevP => "stdDevp",
            Agg::Var => "var",
            Agg::VarP => "varp",
        }
    }

    pub fn from_xml(s: &str) -> Agg {
        Agg::ALL.into_iter().find(|a| a.xml().eq_ignore_ascii_case(s)).unwrap_or_default()
    }
}

/// "Show Values As" calculations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ShowAs {
    #[default]
    Normal,
    PctGrand,
    PctCol,
    PctRow,
    PctOf,
    PctParentRow,
    PctParentCol,
    Difference,
    PctDifference,
    RunningTotal,
    PctRunningTotal,
    RankAsc,
    RankDesc,
    Index,
}

impl ShowAs {
    pub const ALL: [ShowAs; 14] = [
        ShowAs::Normal,
        ShowAs::PctGrand,
        ShowAs::PctCol,
        ShowAs::PctRow,
        ShowAs::PctOf,
        ShowAs::PctParentRow,
        ShowAs::PctParentCol,
        ShowAs::Difference,
        ShowAs::PctDifference,
        ShowAs::RunningTotal,
        ShowAs::PctRunningTotal,
        ShowAs::RankAsc,
        ShowAs::RankDesc,
        ShowAs::Index,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ShowAs::Normal => "No Calculation",
            ShowAs::PctGrand => "% of Grand Total",
            ShowAs::PctCol => "% of Column Total",
            ShowAs::PctRow => "% of Row Total",
            ShowAs::PctOf => "% Of",
            ShowAs::PctParentRow => "% of Parent Row Total",
            ShowAs::PctParentCol => "% of Parent Column Total",
            ShowAs::Difference => "Difference From",
            ShowAs::PctDifference => "% Difference From",
            ShowAs::RunningTotal => "Running Total In",
            ShowAs::PctRunningTotal => "% Running Total In",
            ShowAs::RankAsc => "Rank Smallest to Largest",
            ShowAs::RankDesc => "Rank Largest to Smallest",
            ShowAs::Index => "Index",
        }
    }

    /// The calculation refers to a base field.
    pub fn needs_field(self) -> bool {
        matches!(
            self,
            ShowAs::PctOf | ShowAs::Difference | ShowAs::PctDifference | ShowAs::RunningTotal | ShowAs::PctRunningTotal | ShowAs::RankAsc | ShowAs::RankDesc
        )
    }

    /// The calculation refers to a base item of the base field.
    pub fn needs_item(self) -> bool {
        matches!(self, ShowAs::PctOf | ShowAs::Difference | ShowAs::PctDifference)
    }

    pub fn is_percent(self) -> bool {
        matches!(
            self,
            ShowAs::PctGrand | ShowAs::PctCol | ShowAs::PctRow | ShowAs::PctOf | ShowAs::PctParentRow | ShowAs::PctParentCol | ShowAs::PctDifference | ShowAs::PctRunningTotal
        )
    }

    /// `showDataAs` attribute (None = only in the 2010 extension).
    pub fn xml(self) -> Option<&'static str> {
        Some(match self {
            ShowAs::Normal => "normal",
            ShowAs::PctGrand => "percentOfTotal",
            ShowAs::PctCol => "percentOfCol",
            ShowAs::PctRow => "percentOfRow",
            ShowAs::PctOf => "percent",
            ShowAs::Difference => "difference",
            ShowAs::PctDifference => "percentDiff",
            ShowAs::RunningTotal => "runTotal",
            ShowAs::Index => "index",
            _ => return None,
        })
    }

    /// `x14:dataField pivotShowAs` value of the 2010 calculations.
    pub fn x14(self) -> Option<&'static str> {
        Some(match self {
            ShowAs::PctParentRow => "percentOfParentRow",
            ShowAs::PctParentCol => "percentOfParentCol",
            ShowAs::PctRunningTotal => "percentOfRunningTotal",
            ShowAs::RankAsc => "rankAscending",
            ShowAs::RankDesc => "rankDescending",
            _ => return None,
        })
    }

    pub fn from_xml(s: &str) -> ShowAs {
        ShowAs::ALL.into_iter().find(|v| v.xml() == Some(s) || v.x14() == Some(s)).unwrap_or_default()
    }
}

/// Base item of "Difference From" and similar calculations.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum BaseItem {
    #[default]
    Prev,
    Next,
    /// An item by its (case-insensitive) label.
    Item(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DataField {
    /// Index into `PivotTable::fields`.
    pub field: usize,
    pub name: String,
    pub agg: Agg,
    pub show_as: ShowAs,
    pub base_field: usize,
    pub base_item: BaseItem,
    /// Number format code; empty = General.
    pub num_fmt: String,
}

// ---------------------------------------------------------------------------
// Fields
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SortOrder {
    #[default]
    Asc,
    Desc,
    /// Data source order.
    Source,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DatePart {
    Years,
    Quarters,
    Months,
    Days,
}

impl DatePart {
    pub const ALL: [DatePart; 4] = [DatePart::Years, DatePart::Quarters, DatePart::Months, DatePart::Days];

    pub fn label(self) -> &'static str {
        match self {
            DatePart::Years => "Years",
            DatePart::Quarters => "Quarters",
            DatePart::Months => "Months",
            DatePart::Days => "Days",
        }
    }

    pub fn xml(self) -> &'static str {
        match self {
            DatePart::Years => "years",
            DatePart::Quarters => "quarters",
            DatePart::Months => "months",
            DatePart::Days => "days",
        }
    }

    pub fn from_xml(s: &str) -> Option<DatePart> {
        DatePart::ALL.into_iter().find(|p| p.xml() == s)
    }
}

/// Where a field's items come from.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldSource {
    /// A column of the cache.
    Column(usize),
    /// A date grouping ("Years (Date)") derived from a cache column.
    Date(usize, DatePart),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PivotField {
    /// Field name (the source column header, or "Years (Date)" for a grouping).
    pub name: String,
    pub source: FieldSource,
    /// Custom name shown in the report; empty = `name`.
    pub caption: String,
    /// Lower-case labels of items hidden by the field's filter.
    pub hidden: BTreeSet<String>,
    pub sort: SortOrder,
    /// Sort by the values of this data field (in `sort` direction) instead of by label.
    pub sort_by: Option<usize>,
    /// Automatic subtotals.
    pub subtotal: bool,
    /// Lower-case labels of collapsed items.
    pub collapsed: BTreeSet<String>,
    /// Report filter: the single item shown (lower-case label), None = all (minus `hidden`).
    pub page: Option<String>,
    /// Numeric grouping (start, end, interval).
    pub num_group: Option<(f64, f64, f64)>,
}

impl PivotField {
    pub fn new(name: &str, source: FieldSource) -> Self {
        PivotField {
            name: name.to_string(),
            source,
            caption: String::new(),
            hidden: BTreeSet::new(),
            sort: SortOrder::Asc,
            sort_by: None,
            subtotal: true,
            collapsed: BTreeSet::new(),
            page: None,
            num_group: None,
        }
    }

    pub fn display(&self) -> &str {
        if self.caption.is_empty() { &self.name } else { &self.caption }
    }

    /// The cache column the field reads.
    pub fn column(&self) -> usize {
        match self.source {
            FieldSource::Column(c) | FieldSource::Date(c, _) => c,
        }
    }
}

/// Lower-case identity of an item label (PivotTable items are case-insensitive).
pub fn ikey(label: &str) -> String {
    label.to_lowercase()
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// Snapshot of the source data taken at the last refresh.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Cache {
    pub names: Vec<String>,
    pub records: Vec<Vec<Value>>,
    /// Number format of each column (from its first data cell).
    pub formats: Vec<String>,
}

impl Cache {
    pub fn is_date(&self, c: usize) -> bool {
        self.formats.get(c).is_some_and(|f| crate::numfmt::is_date_format(f))
    }

    /// All values of a column are numbers (or blank).
    pub fn is_numeric(&self, c: usize) -> bool {
        let mut any = false;
        for r in &self.records {
            match r.get(c) {
                Some(Value::Number(_)) => any = true,
                Some(Value::Empty) | None => {}
                _ => return false,
            }
        }
        any
    }
}

/// Resolves a source reference ("Sheet1!$A$1:$E$7", "A1:E7" on `default_sheet`, a table or a defined name).
pub fn resolve_source(wb: &Workbook, source: &str, default_sheet: usize) -> Option<(usize, Range)> {
    let s = source.trim().trim_start_matches('=');
    if let Some((si, ti)) = crate::table::find(wb, s) {
        let t = &wb.sheets[si].tables[ti];
        return Some((si, Range { r2: t.range.r2 - t.totals as u32, ..t.range }));
    }
    if let Some((_, text)) = wb.names.iter().find(|(n, _)| n.eq_ignore_ascii_case(s)) {
        if !text.eq_ignore_ascii_case(s) {
            return resolve_source(wb, text, default_sheet);
        }
    }
    let (sheet, rg) = match s.rfind('!') {
        Some(i) => {
            let name = s[..i].trim_matches('\'').replace("''", "'");
            (wb.sheet_index(&name)?, &s[i + 1..])
        }
        None => (default_sheet.min(wb.sheets.len().saturating_sub(1)), s),
    };
    let mut rg = Range::parse(rg)?;
    // Whole columns stop at the last used row.
    if rg.is_full_cols() {
        let last = wb.sheets[sheet].extent().map(|e| e.0).unwrap_or(0);
        rg.r2 = last.max(rg.r1 + 1);
    }
    Some((sheet, rg))
}

/// The text of a source reference: `Sheet1!$A$1:$E$7`.
pub fn source_text(wb: &Workbook, si: usize, rg: Range) -> String {
    let p = |r: u32, c: u32| format!("${}${}", crate::model::col_name(c), r + 1);
    format!("{}!{}:{}", crate::formula::quote_sheet(&wb.sheets[si].name), p(rg.r1, rg.c1), p(rg.r2, rg.c2))
}

pub const BAD_FIELD_NAME: &str = "The PivotTable field name is not valid. To create a PivotTable report, you must use data that is organized as a list with labeled columns. If you are changing the name of a PivotTable field, you must type a new name for the field.";

/// Reads the source range into a cache. The first row holds the field names.
pub fn build_cache(wb: &Workbook, source: &str, default_sheet: usize) -> Result<Cache, String> {
    let (si, rg) = resolve_source(wb, source, default_sheet).ok_or_else(|| "Data source reference is not valid.".to_string())?;
    let sh = &wb.sheets[si];
    if rg.rows() < 1 || rg.cols() > 16_384 {
        return Err("Data source reference is not valid.".into());
    }
    let mut names = Vec::new();
    for c in rg.c1..=rg.c2 {
        let v = sh.display_value(rg.r1, c);
        let n = match v {
            Value::Empty => return Err(BAD_FIELD_NAME.into()),
            v => crate::numfmt::format_value(v, &wb.style(sh.get(rg.r1, c).map_or(0, |x| x.style)).num_fmt).text,
        };
        // Duplicate headers get a number, like Excel ("Sales2").
        let mut name = n.clone();
        let mut k = 2;
        while names.iter().any(|x: &String| x.eq_ignore_ascii_case(&name)) {
            name = format!("{n}{k}");
            k += 1;
        }
        names.push(name);
    }
    let mut formats = vec![String::new(); names.len()];
    let mut records = Vec::with_capacity(rg.rows() as usize);
    for r in rg.r1 + 1..=rg.r2 {
        let mut rec = Vec::with_capacity(names.len());
        for (k, c) in (rg.c1..=rg.c2).enumerate() {
            let v = sh.display_value(r, c).clone();
            if formats[k].is_empty() && !v.is_empty() {
                formats[k] = wb.style(sh.get(r, c).map_or(0, |x| x.style)).num_fmt.clone();
                if formats[k].is_empty() {
                    formats[k] = "General".into();
                }
            }
            rec.push(v);
        }
        records.push(rec);
    }
    for f in formats.iter_mut() {
        if f == "General" {
            f.clear();
        }
    }
    Ok(Cache { names, records, formats })
}

// ---------------------------------------------------------------------------
// The PivotTable
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Form {
    #[default]
    Compact,
    Outline,
    Tabular,
}

/// The four areas of the field list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    Filters,
    Columns,
    Rows,
    Values,
}

impl Area {
    pub const ALL: [Area; 4] = [Area::Filters, Area::Columns, Area::Rows, Area::Values];

    pub fn label(self) -> &'static str {
        match self {
            Area::Filters => "Filters",
            Area::Columns => "Columns",
            Area::Rows => "Rows",
            Area::Values => "Values",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PivotTable {
    pub name: String,
    /// Source reference text (see `resolve_source`).
    pub source: String,
    pub cache: Arc<Cache>,
    pub fields: Vec<PivotField>,
    /// Row / column fields (indices into `fields`, or `VALUES`), outer first.
    pub rows: Vec<usize>,
    pub cols: Vec<usize>,
    /// Report filter fields, top first.
    pub filters: Vec<usize>,
    pub values: Vec<DataField>,
    /// Top-left cell of the report body (below the report filters).
    pub anchor: (u32, u32),
    pub form: Form,
    pub subtotals_top: bool,
    /// Grand totals for rows (the column at the right) and for columns (the row at the bottom).
    pub row_grand: bool,
    pub col_grand: bool,
    pub blank_rows: bool,
    pub repeat_labels: bool,
    pub style: String,
    pub row_headers: bool,
    pub col_headers: bool,
    pub banded_rows: bool,
    pub banded_cols: bool,
    /// Expand/collapse buttons.
    pub show_buttons: bool,
    /// Field captions and filter buttons ("Row Labels", "Column Labels").
    pub field_headers: bool,
    pub data_caption: String,
    /// Text shown in empty value cells.
    pub empty_text: String,
    /// Fit column widths after each update.
    pub autofit: bool,
    /// Last rendered layout (rebuilt by `engine::update`).
    pub out: Output,
}

pub const DEFAULT_STYLE: &str = "PivotStyleLight16";

impl PivotTable {
    pub fn new(name: &str, source: &str, cache: Cache, anchor: (u32, u32)) -> Self {
        let fields = cache.names.iter().enumerate().map(|(i, n)| PivotField::new(n, FieldSource::Column(i))).collect();
        PivotTable {
            name: name.to_string(),
            source: source.to_string(),
            cache: Arc::new(cache),
            fields,
            rows: Vec::new(),
            cols: Vec::new(),
            filters: Vec::new(),
            values: Vec::new(),
            anchor,
            form: Form::Compact,
            subtotals_top: true,
            row_grand: true,
            col_grand: true,
            blank_rows: false,
            repeat_labels: false,
            style: DEFAULT_STYLE.into(),
            row_headers: true,
            col_headers: true,
            banded_rows: false,
            banded_cols: false,
            show_buttons: true,
            field_headers: true,
            data_caption: "Values".into(),
            empty_text: String::new(),
            autofit: true,
            out: Output::default(),
        }
    }

    pub fn field_index(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.name.eq_ignore_ascii_case(name)).or_else(|| self.fields.iter().position(|f| f.display().eq_ignore_ascii_case(name)))
    }

    /// The area holding field `f` (Values counts when any data field uses it).
    pub fn area_of(&self, f: usize) -> Option<Area> {
        if self.rows.contains(&f) {
            Some(Area::Rows)
        } else if self.cols.contains(&f) {
            Some(Area::Columns)
        } else if self.filters.contains(&f) {
            Some(Area::Filters)
        } else if self.values.iter().any(|d| d.field == f) {
            Some(Area::Values)
        } else {
            None
        }
    }

    /// Whether a field is in the report (any area).
    pub fn is_used(&self, f: usize) -> bool {
        self.area_of(f).is_some()
    }

    fn list_mut(&mut self, a: Area) -> Option<&mut Vec<usize>> {
        match a {
            Area::Rows => Some(&mut self.rows),
            Area::Columns => Some(&mut self.cols),
            Area::Filters => Some(&mut self.filters),
            Area::Values => None,
        }
    }

    pub fn list(&self, a: Area) -> Vec<usize> {
        match a {
            Area::Rows => self.rows.clone(),
            Area::Columns => self.cols.clone(),
            Area::Filters => self.filters.clone(),
            Area::Values => (0..self.values.len()).collect(),
        }
    }

    /// Default data field name: "Sum of Sales" (with a number when taken, like Excel).
    pub fn data_name(&self, agg: Agg, f: usize, skip: Option<usize>) -> String {
        let base = format!("{} of {}", agg.label(), self.fields[f].name);
        let taken = |n: &str| self.values.iter().enumerate().any(|(i, d)| Some(i) != skip && d.name.eq_ignore_ascii_case(n)) || self.fields.iter().any(|x| x.name.eq_ignore_ascii_case(n));
        if !taken(&base) {
            return base;
        }
        (2..).map(|k| format!("{base}{k}")).find(|n| !taken(n)).unwrap()
    }

    /// Adds field `f` to an area at `pos` (end when None). Row, column and filter fields
    /// appear once, so the field leaves its old area; Values adds another data field.
    pub fn add_field(&mut self, f: usize, area: Area, pos: Option<usize>) {
        if f == VALUES {
            self.rows.retain(|x| *x != VALUES);
            self.cols.retain(|x| *x != VALUES);
            if let Some(list) = self.list_mut(area) {
                let p = pos.unwrap_or(list.len()).min(list.len());
                list.insert(p, VALUES);
            }
            return;
        }
        if area == Area::Values {
            let numeric = self.cache.is_numeric(self.fields[f].column()) && matches!(self.fields[f].source, FieldSource::Column(_));
            let agg = if numeric { Agg::Sum } else { Agg::Count };
            let name = self.data_name(agg, f, None);
            let d = DataField { field: f, name, agg, show_as: ShowAs::Normal, base_field: 0, base_item: BaseItem::Prev, num_fmt: String::new() };
            let p = pos.unwrap_or(self.values.len()).min(self.values.len());
            self.values.insert(p, d);
            self.fix_values_field();
            return;
        }
        // A field can't be both a row/column/filter field and keep its old axis place.
        let mut p = pos;
        for a in [Area::Rows, Area::Columns, Area::Filters] {
            if let Some(list) = self.list_mut(a) {
                if let Some(i) = list.iter().position(|x| *x == f) {
                    list.remove(i);
                    if a == area {
                        if let Some(q) = p.as_mut() {
                            if *q > i {
                                *q -= 1;
                            }
                        }
                    }
                }
            }
        }
        if area == Area::Filters {
            self.fields[f].collapsed.clear();
        } else {
            self.fields[f].page = None;
        }
        let list = self.list_mut(area).unwrap();
        let p = p.unwrap_or(list.len()).min(list.len());
        list.insert(p, f);
    }

    /// Removes the entry at `idx` of an area.
    pub fn remove_at(&mut self, area: Area, idx: usize) {
        match area {
            Area::Values => {
                if idx < self.values.len() {
                    self.values.remove(idx);
                    for f in self.fields.iter_mut() {
                        if let Some(s) = f.sort_by {
                            f.sort_by = if s == idx { None } else if s > idx { Some(s - 1) } else { Some(s) };
                        }
                    }
                }
                self.fix_values_field();
            }
            a => {
                let list = self.list_mut(a).unwrap();
                if idx < list.len() {
                    let f = list.remove(idx);
                    if f != VALUES {
                        self.fields[f].page = None;
                    }
                }
            }
        }
    }

    /// Removes a field from every area.
    pub fn remove_field(&mut self, f: usize) {
        self.rows.retain(|x| *x != f);
        self.cols.retain(|x| *x != f);
        self.filters.retain(|x| *x != f);
        while let Some(i) = self.values.iter().position(|d| d.field == f) {
            self.remove_at(Area::Values, i);
        }
        if let Some(pf) = self.fields.get_mut(f) {
            pf.page = None;
        }
    }

    /// Moves the entry at `idx` of `from` to `to` at `pos`.
    pub fn move_entry(&mut self, from: Area, idx: usize, to: Area, pos: usize) {
        if from == Area::Values {
            if to == Area::Values {
                if idx < self.values.len() {
                    let d = self.values.remove(idx);
                    let p = if pos > idx { pos - 1 } else { pos }.min(self.values.len());
                    self.values.insert(p, d);
                }
                return;
            }
            let Some(d) = self.values.get(idx).cloned() else { return };
            self.remove_at(Area::Values, idx);
            self.add_field(d.field, to, Some(pos));
            return;
        }
        let list = self.list(from);
        let Some(&f) = list.get(idx) else { return };
        if f == VALUES && to == Area::Filters || f == VALUES && to == Area::Values {
            return;
        }
        if to == Area::Values {
            self.add_field(f, Area::Values, Some(pos));
            return;
        }
        let pos = if from == to && pos > idx { pos } else { pos };
        self.add_field(f, to, Some(pos));
    }

    /// Keeps the Σ Values field in the rows or columns exactly when there are two or more data fields.
    pub fn fix_values_field(&mut self) {
        let has = self.rows.contains(&VALUES) || self.cols.contains(&VALUES);
        if self.values.len() >= 2 && !has {
            self.cols.push(VALUES);
        } else if self.values.len() < 2 && has {
            self.rows.retain(|x| *x != VALUES);
            self.cols.retain(|x| *x != VALUES);
        }
    }

    /// Removes every field and filter (Clear All).
    pub fn clear(&mut self) {
        self.rows.clear();
        self.cols.clear();
        self.filters.clear();
        self.values.clear();
        for f in self.fields.iter_mut() {
            f.hidden.clear();
            f.collapsed.clear();
            f.page = None;
            f.sort_by = None;
        }
    }

    /// Clears every item filter (Clear Filters).
    pub fn clear_filters(&mut self) {
        for f in self.fields.iter_mut() {
            f.hidden.clear();
            f.page = None;
        }
    }

    /// Installs a new cache (Refresh / Change Data Source), keeping fields that still exist by name.
    pub fn set_cache(&mut self, cache: Cache) {
        let old = std::mem::take(&mut self.fields);
        let mut map: Vec<Option<usize>> = vec![None; old.len()];
        let mut fields: Vec<PivotField> = Vec::new();
        for (i, n) in cache.names.iter().enumerate() {
            let pf = match old.iter().position(|f| matches!(f.source, FieldSource::Column(_)) && f.name.eq_ignore_ascii_case(n)) {
                Some(k) => {
                    map[k] = Some(i);
                    let mut f = old[k].clone();
                    f.source = FieldSource::Column(i);
                    f.name = n.clone();
                    f
                }
                None => PivotField::new(n, FieldSource::Column(i)),
            };
            fields.push(pf);
        }
        // Date groupings follow their base column.
        for (k, f) in old.iter().enumerate() {
            if let FieldSource::Date(base, part) = f.source {
                let old_base = old.iter().position(|x| x.source == FieldSource::Column(base));
                if let Some(nb) = old_base.and_then(|b| map[b]) {
                    let mut f = f.clone();
                    f.source = FieldSource::Date(nb, part);
                    map[k] = Some(fields.len());
                    fields.push(f);
                }
            }
        }
        let remap = |v: &mut Vec<usize>| {
            *v = v.iter().filter_map(|&x| if x == VALUES { Some(VALUES) } else { map.get(x).copied().flatten() }).collect();
        };
        remap(&mut self.rows);
        remap(&mut self.cols);
        remap(&mut self.filters);
        let mut values = Vec::new();
        for mut d in std::mem::take(&mut self.values) {
            if let Some(nf) = map.get(d.field).copied().flatten() {
                d.field = nf;
                d.base_field = map.get(d.base_field).copied().flatten().unwrap_or(0);
                values.push(d);
            }
        }
        self.values = values;
        self.fields = fields;
        self.cache = Arc::new(cache);
        self.fix_values_field();
    }

    /// Groups date field `f` into the given parts: one new field per part ("Years (Date)"),
    /// placed where the field was.
    pub fn group_dates(&mut self, f: usize, parts: &[DatePart]) {
        let FieldSource::Column(col) = self.fields[f].source else { return };
        self.ungroup(f);
        let mut parts = parts.to_vec();
        parts.sort();
        parts.dedup();
        if parts.is_empty() {
            return;
        }
        let base_name = self.fields[f].name.clone();
        let mut new_ids = Vec::new();
        for p in &parts {
            let name = format!("{} ({})", p.label(), base_name);
            new_ids.push(self.fields.len());
            self.fields.push(PivotField::new(&name, FieldSource::Date(col, *p)));
        }
        for list in [&mut self.rows, &mut self.cols] {
            if let Some(i) = list.iter().position(|x| *x == f) {
                list.splice(i..=i, new_ids.iter().copied());
            }
        }
        if let Some(i) = self.filters.iter().position(|x| *x == f) {
            self.filters.splice(i..=i, new_ids.iter().copied());
        }
    }

    /// Removes the date grouping of a field (given the base field or any of its parts) or its numeric grouping.
    pub fn ungroup(&mut self, f: usize) {
        if f >= self.fields.len() {
            return;
        }
        self.fields[f].num_group = None;
        let col = self.fields[f].column();
        let base = self.fields.iter().position(|x| x.source == FieldSource::Column(col));
        let derived: Vec<usize> = (0..self.fields.len()).filter(|&i| matches!(self.fields[i].source, FieldSource::Date(c, _) if c == col)).collect();
        if derived.is_empty() {
            return;
        }
        // The base field takes the place of the first part.
        if let Some(b) = base {
            for list in [&mut self.rows, &mut self.cols, &mut self.filters] {
                if let Some(i) = list.iter().position(|x| derived.contains(x)) {
                    if !list.contains(&b) {
                        list[i] = b;
                    }
                }
            }
        }
        for &d in derived.iter().rev() {
            self.drop_field(d);
        }
    }

    /// Deletes field `d` from `fields`, renumbering references.
    fn drop_field(&mut self, d: usize) {
        self.remove_field(d);
        self.fields.remove(d);
        let fix = |x: &mut usize| {
            if *x != VALUES && *x > d {
                *x -= 1;
            }
        };
        self.rows.iter_mut().for_each(fix);
        self.cols.iter_mut().for_each(fix);
        self.filters.iter_mut().for_each(fix);
        for v in self.values.iter_mut() {
            fix(&mut v.field);
            if v.base_field == d {
                v.base_field = 0;
            } else {
                fix(&mut v.base_field);
            }
        }
    }

    /// The table range (without report filters) of the last update.
    pub fn range(&self) -> Range {
        self.out.range
    }

    /// The whole report including report filters.
    pub fn full_range(&self) -> Range {
        self.out.full
    }
}

/// A name for a new PivotTable that no other PivotTable of the workbook uses.
pub fn unique_name(wb: &Workbook) -> String {
    (1..)
        .map(|i| format!("PivotTable{i}"))
        .find(|n| !wb.sheets.iter().any(|s| s.pivots.iter().any(|p| p.name.eq_ignore_ascii_case(n))))
        .unwrap()
}

/// (sheet, pivot index) of the PivotTable whose report covers a cell.
pub fn at(wb: &Workbook, si: usize, r: u32, c: u32) -> Option<usize> {
    wb.sheets.get(si)?.pivots.iter().position(|p| p.out.full.contains(r, c))
}
