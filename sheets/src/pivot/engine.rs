//! Computes a PivotTable report from its cache and lays it out in cells, the way Excel
//! does for the compact, outline and tabular forms.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use super::{ikey, BaseItem, DatePart, FieldSource, Form, PivotTable, ShowAs, VALUES};
use crate::model::{Cell, ErrorKind, HAlign, Range, Style, Value, Workbook, MAX_COLS, MAX_ROWS};

// ---------------------------------------------------------------------------
// Items
// ---------------------------------------------------------------------------

/// Sort key of an item: numbers, then text, logicals, errors, and blanks last.
#[derive(Clone, Debug, PartialEq, PartialOrd)]
pub enum Key {
    Num(f64),
    Text(String),
    Bool(bool),
    Err(u8),
    Blank,
}

fn cmp_key(a: &Key, b: &Key) -> Ordering {
    a.partial_cmp(b).unwrap_or(Ordering::Equal)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub label: String,
    /// Value written in the report cell.
    pub value: Value,
    pub key: Key,
}

pub const BLANK: &str = "(blank)";

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn plain_item(v: &Value, fmt: &str) -> Item {
    match v {
        Value::Empty => Item { label: BLANK.into(), value: Value::Text(BLANK.into()), key: Key::Blank },
        Value::Number(n) => {
            let label = if fmt.is_empty() || fmt.eq_ignore_ascii_case("general") { crate::numfmt::general(*n) } else { crate::numfmt::format_value(v, fmt).text };
            Item { label, value: v.clone(), key: Key::Num(*n) }
        }
        Value::Text(s) => Item { label: s.clone(), value: v.clone(), key: Key::Text(s.to_lowercase()) },
        Value::Bool(b) => Item { label: if *b { "TRUE".into() } else { "FALSE".into() }, value: v.clone(), key: Key::Bool(*b) },
        Value::Error(e) => Item { label: e.as_str().into(), value: v.clone(), key: Key::Err(e.code() as u8) },
    }
}

/// Label of a value as a PivotTable item ("(blank)", dates in the field's format).
pub fn label_of(v: &Value, fmt: &str) -> String {
    plain_item(v, fmt).label
}

fn text_item(label: String, order: f64) -> Item {
    Item { value: Value::Text(label.clone()), label, key: Key::Num(order) }
}

/// Label of a numeric group: "0-99" for whole numbers, "0-100" otherwise.
pub fn num_group_label(start: f64, by: f64, k: f64, ints: bool) -> String {
    let a = start + k * by;
    if ints && by.fract() == 0.0 && start.fract() == 0.0 {
        format!("{}-{}", crate::numfmt::general(a), crate::numfmt::general(a + by - 1.0))
    } else {
        format!("{}-{}", crate::numfmt::general(a), crate::numfmt::general(a + by))
    }
}

/// Label of the group above a numeric grouping's end: the end of its last range (">300").
pub fn num_group_above(start: f64, end: f64, by: f64) -> String {
    let n = ((end - start) / by).floor().max(0.0) + 1.0;
    format!(">{}", crate::numfmt::general(start + n * by))
}

/// The item of field `f` for a record (`ints`: the column holds whole numbers only).
pub fn item_of(pt: &PivotTable, f: usize, rec: &[Value], ints: bool) -> Item {
    let field = &pt.fields[f];
    let col = field.column();
    let v = rec.get(col).unwrap_or(&Value::Empty);
    match field.source {
        FieldSource::Column(_) => {
            if let (Some((start, end, by)), Value::Number(n)) = (field.num_group, v) {
                if by > 0.0 {
                    if *n < start {
                        return text_item(format!("<{}", crate::numfmt::general(start)), f64::MIN);
                    }
                    if *n > end {
                        return text_item(num_group_above(start, end, by), f64::MAX);
                    }
                    let k = ((n - start) / by).floor();
                    return text_item(num_group_label(start, by, k, ints), k);
                }
            }
            plain_item(v, pt.cache.formats.get(col).map(|s| s.as_str()).unwrap_or(""))
        }
        FieldSource::Date(_, part) => {
            let Value::Number(n) = v else { return plain_item(v, "") };
            let (y, m, d) = crate::numfmt::serial_to_ymd(*n);
            match part {
                DatePart::Years => text_item(y.to_string(), y as f64),
                DatePart::Quarters => text_item(format!("Qtr{}", (m - 1) / 3 + 1), ((m - 1) / 3 + 1) as f64),
                DatePart::Months => text_item(MONTHS[(m.clamp(1, 12) - 1) as usize].to_string(), m as f64),
                DatePart::Days => text_item(format!("{}-{}", d, MONTHS[(m.clamp(1, 12) - 1) as usize]), (m * 32 + d) as f64),
            }
        }
    }
}

/// Distinct items of a field (in data source order) and each record's item.
pub struct FieldItems {
    pub items: Vec<Item>,
    pub of: Vec<u32>,
}

pub fn field_items(pt: &PivotTable, f: usize) -> FieldItems {
    let mut ix: HashMap<String, u32> = HashMap::new();
    let mut items = Vec::new();
    let mut of = Vec::with_capacity(pt.cache.records.len());
    let col = pt.fields[f].column();
    let ints = pt.fields[f].num_group.is_some() && pt.cache.records.iter().all(|r| !matches!(r.get(col), Some(Value::Number(x)) if x.fract() != 0.0));
    for rec in &pt.cache.records {
        let it = item_of(pt, f, rec, ints);
        let k = ikey(&it.label);
        let id = *ix.entry(k).or_insert_with(|| {
            items.push(it);
            items.len() as u32 - 1
        });
        of.push(id);
    }
    FieldItems { items, of }
}

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct Acc {
    n: u32,
    nums: u32,
    sum: f64,
    prod: f64,
    min: f64,
    max: f64,
    mean: f64,
    m2: f64,
    err: Option<ErrorKind>,
}

impl Acc {
    fn add(&mut self, v: &Value) {
        match v {
            Value::Empty => {}
            Value::Number(x) => {
                let x = *x;
                self.n += 1;
                self.nums += 1;
                if self.nums == 1 {
                    self.prod = x;
                    self.min = x;
                    self.max = x;
                } else {
                    self.prod *= x;
                    self.min = self.min.min(x);
                    self.max = self.max.max(x);
                }
                self.sum += x;
                let d = x - self.mean;
                self.mean += d / self.nums as f64;
                self.m2 += d * (x - self.mean);
            }
            Value::Error(e) => {
                self.n += 1;
                self.err.get_or_insert(*e);
            }
            _ => self.n += 1,
        }
    }

    fn result(&self, agg: super::Agg) -> Value {
        use super::Agg::*;
        let n = self.nums as f64;
        if let (Some(e), false) = (self.err, matches!(agg, Count | CountNums)) {
            return Value::Error(e);
        }
        let div0 = Value::Error(ErrorKind::Div0);
        match agg {
            Sum => Value::Number(self.sum),
            Count => Value::Number(self.n as f64),
            CountNums => Value::Number(n),
            Average => if self.nums == 0 { div0 } else { Value::Number(self.sum / n) },
            Max => Value::Number(if self.nums == 0 { 0.0 } else { self.max }),
            Min => Value::Number(if self.nums == 0 { 0.0 } else { self.min }),
            Product => Value::Number(if self.nums == 0 { 0.0 } else { self.prod }),
            StdDev => if self.nums < 2 { div0 } else { Value::Number((self.m2 / (n - 1.0)).sqrt()) },
            StdDevP => if self.nums == 0 { div0 } else { Value::Number((self.m2 / n).sqrt()) },
            Var => if self.nums < 2 { div0 } else { Value::Number(self.m2 / (n - 1.0)) },
            VarP => if self.nums == 0 { div0 } else { Value::Number(self.m2 / n) },
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Node {
    item: u32,
    kids: Vec<Node>,
}

impl Node {
    fn find(&self, path: &[u32]) -> Option<&Node> {
        match path.split_first() {
            None => Some(self),
            Some((h, t)) => self.kids.iter().find(|k| k.item == *h)?.find(t),
        }
    }
}

fn build_tree(paths: &mut [Vec<u32>], depth: usize) -> Vec<Node> {
    let mut out: Vec<Node> = Vec::new();
    let mut i = 0;
    while i < paths.len() {
        if paths[i].len() <= depth {
            i += 1;
            continue;
        }
        let item = paths[i][depth];
        let mut j = i;
        while j < paths.len() && paths[j].len() > depth && paths[j][depth] == item {
            j += 1;
        }
        let kids = build_tree(&mut paths[i..j], depth + 1);
        out.push(Node { item, kids });
        i = j;
    }
    out
}

/// One computation of a PivotTable from its cache.
pub struct Calc<'a> {
    pt: &'a PivotTable,
    pub items: HashMap<usize, FieldItems>,
    /// Real (non-Σ) row and column fields.
    pub rf: Vec<usize>,
    pub cf: Vec<usize>,
    /// Records passing the filters.
    pub recs: Vec<usize>,
    agg: HashMap<(Vec<u32>, Vec<u32>), Vec<Acc>>,
    rtree: Node,
    ctree: Node,
}

impl<'a> Calc<'a> {
    pub fn new(pt: &'a PivotTable) -> Self {
        let nf = pt.fields.len();
        let rf: Vec<usize> = pt.rows.iter().copied().filter(|f| *f != VALUES && *f < nf).collect();
        let cf: Vec<usize> = pt.cols.iter().copied().filter(|f| *f != VALUES && *f < nf).collect();
        let mut used: Vec<usize> = rf.iter().chain(cf.iter()).chain(pt.filters.iter()).copied().filter(|f| *f < nf).collect();
        used.sort_unstable();
        used.dedup();
        let mut items = HashMap::new();
        for &f in &used {
            items.insert(f, field_items(pt, f));
        }
        let recs: Vec<usize> = (0..pt.cache.records.len())
            .filter(|&i| {
                used.iter().all(|&f| {
                    let fi = &items[&f];
                    let k = ikey(&fi.items[fi.of[i] as usize].label);
                    let field = &pt.fields[f];
                    if pt.filters.contains(&f) {
                        if let Some(p) = &field.page {
                            return *p == k;
                        }
                    }
                    !field.hidden.contains(&k)
                })
            })
            .collect();
        let nd = pt.values.len();
        let mut agg: HashMap<(Vec<u32>, Vec<u32>), Vec<Acc>> = HashMap::new();
        let mut rpaths: HashSet<Vec<u32>> = HashSet::new();
        let mut cpaths: HashSet<Vec<u32>> = HashSet::new();
        let empty = Value::Empty;
        for &i in &recs {
            let rk: Vec<u32> = rf.iter().map(|f| items[f].of[i]).collect();
            let ck: Vec<u32> = cf.iter().map(|f| items[f].of[i]).collect();
            let rec = &pt.cache.records[i];
            for a in 0..=rk.len() {
                for b in 0..=ck.len() {
                    let accs = agg.entry((rk[..a].to_vec(), ck[..b].to_vec())).or_insert_with(|| vec![Acc::default(); nd]);
                    for (d, df) in pt.values.iter().enumerate() {
                        let col = pt.fields.get(df.field).map(|f| f.column()).unwrap_or(usize::MAX);
                        accs[d].add(rec.get(col).unwrap_or(&empty));
                    }
                }
            }
            rpaths.insert(rk);
            cpaths.insert(ck);
        }
        let mut rp: Vec<Vec<u32>> = rpaths.into_iter().collect();
        rp.sort();
        let mut cp: Vec<Vec<u32>> = cpaths.into_iter().collect();
        cp.sort();
        let mut c = Calc {
            pt,
            items,
            rf,
            cf,
            recs,
            agg,
            rtree: Node { item: 0, kids: build_tree(&mut rp, 0) },
            ctree: Node { item: 0, kids: build_tree(&mut cp, 0) },
        };
        let mut rt = std::mem::take(&mut c.rtree);
        c.sort_tree(&mut rt, true, &mut Vec::new());
        c.rtree = rt;
        let mut ct = std::mem::take(&mut c.ctree);
        c.sort_tree(&mut ct, false, &mut Vec::new());
        c.ctree = ct;
        c
    }

    fn item(&self, f: usize, id: u32) -> &Item {
        &self.items[&f].items[id as usize]
    }

    /// Orders the children of every node the way the field asks.
    fn sort_tree(&self, node: &mut Node, rows: bool, prefix: &mut Vec<u32>) {
        let level = prefix.len();
        let fields = if rows { &self.rf } else { &self.cf };
        if level >= fields.len() {
            return;
        }
        let f = fields[level];
        let field = &self.pt.fields[f];
        let desc = field.sort == super::SortOrder::Desc;
        let by = field.sort_by.filter(|d| *d < self.pt.values.len());
        let val = |id: u32| -> f64 {
            let Some(d) = by else { return 0.0 };
            let mut k = prefix.clone();
            k.push(id);
            let v = if rows { self.raw(&k, &[], d) } else { self.raw(&[], &k, d) };
            match v {
                Some(Value::Number(n)) => n,
                _ => f64::NEG_INFINITY,
            }
        };
        let mut keyed: Vec<(f64, Node)> = std::mem::take(&mut node.kids).into_iter().map(|n| (val(n.item), n)).collect();
        keyed.sort_by(|(va, a), (vb, b)| {
            let (ia, ib) = (self.item(f, a.item), self.item(f, b.item));
            // Blanks stay last.
            let blank = (ia.key == Key::Blank).cmp(&(ib.key == Key::Blank));
            if blank != Ordering::Equal {
                return blank;
            }
            let o = if by.is_some() {
                va.partial_cmp(vb).unwrap_or(Ordering::Equal).then_with(|| cmp_key(&ia.key, &ib.key))
            } else if field.sort == super::SortOrder::Source {
                a.item.cmp(&b.item)
            } else {
                cmp_key(&ia.key, &ib.key)
            };
            if desc { o.reverse() } else { o }
        });
        node.kids = keyed.into_iter().map(|(_, n)| n).collect();
        for k in node.kids.iter_mut() {
            prefix.push(k.item);
            self.sort_tree(k, rows, prefix);
            prefix.pop();
        }
    }

    fn raw(&self, rk: &[u32], ck: &[u32], d: usize) -> Option<Value> {
        let accs = self.agg.get(&(rk.to_vec(), ck.to_vec()))?;
        Some(accs.get(d)?.result(self.pt.values[d].agg))
    }

    fn num(&self, rk: &[u32], ck: &[u32], d: usize) -> Option<Result<f64, ErrorKind>> {
        match self.raw(rk, ck, d)? {
            Value::Number(n) => Some(Ok(n)),
            Value::Error(e) => Some(Err(e)),
            _ => None,
        }
    }

    /// Items of the field at `level` under `prefix`, in display order.
    fn siblings(&self, rows: bool, prefix: &[u32]) -> Vec<u32> {
        let t = if rows { &self.rtree } else { &self.ctree };
        t.find(prefix).map(|n| n.kids.iter().map(|k| k.item).collect()).unwrap_or_default()
    }

    /// The value shown for a cell, after the data field's "Show Values As" calculation.
    pub fn shown(&self, rk: &[u32], ck: &[u32], d: usize) -> Value {
        let df = &self.pt.values[d];
        let v = self.raw(rk, ck, d);
        let ratio = |t: Option<Value>| -> Value {
            match (v.clone(), t) {
                (Some(Value::Error(e)), _) | (_, Some(Value::Error(e))) => Value::Error(e),
                (None, Some(_)) => Value::Number(0.0),
                (Some(Value::Number(a)), Some(Value::Number(b))) => {
                    if b == 0.0 { Value::Error(ErrorKind::Div0) } else { Value::Number(a / b) }
                }
                _ => Value::Empty,
            }
        };
        match df.show_as {
            ShowAs::Normal => v.unwrap_or(Value::Empty),
            ShowAs::PctGrand => ratio(self.raw(&[], &[], d)),
            ShowAs::PctCol => ratio(self.raw(&[], ck, d)),
            ShowAs::PctRow => ratio(self.raw(rk, &[], d)),
            ShowAs::PctParentRow => {
                if rk.is_empty() { ratio(v.clone()) } else { ratio(self.raw(&rk[..rk.len() - 1], ck, d)) }
            }
            ShowAs::PctParentCol => {
                if ck.is_empty() { ratio(v.clone()) } else { ratio(self.raw(rk, &ck[..ck.len() - 1], d)) }
            }
            ShowAs::Index => {
                let n = |x: Option<Value>| match x {
                    Some(Value::Number(n)) => Some(n),
                    _ => None,
                };
                match (n(v.clone()), n(self.raw(&[], &[], d)), n(self.raw(rk, &[], d)), n(self.raw(&[], ck, d))) {
                    (Some(a), Some(g), Some(r), Some(c)) if r != 0.0 && c != 0.0 => Value::Number(a * g / (r * c)),
                    (None, ..) => Value::Empty,
                    _ => Value::Error(ErrorKind::Div0),
                }
            }
            _ => self.base_calc(rk, ck, d, v),
        }
    }

    fn base_calc(&self, rk: &[u32], ck: &[u32], d: usize, v: Option<Value>) -> Value {
        let df = &self.pt.values[d];
        let bf = df.base_field;
        let (rows, k) = match (self.rf.iter().position(|f| *f == bf), self.cf.iter().position(|f| *f == bf)) {
            (Some(k), _) => (true, k),
            (None, Some(k)) => (false, k),
            _ => return Value::Error(ErrorKind::NA),
        };
        let key = if rows { rk } else { ck };
        if key.len() <= k {
            return Value::Empty;
        }
        let sibs = self.siblings(rows, &key[..k]);
        if sibs.is_empty() {
            return Value::Empty;
        }
        let cur = key[k];
        let pos = sibs.iter().position(|x| *x == cur).unwrap_or(0);
        let with = |item: u32| -> Option<Result<f64, ErrorKind>> {
            let mut kk = key.to_vec();
            kk[k] = item;
            if rows { self.num(&kk, ck, d) } else { self.num(rk, &kk, d) }
        };
        let cur_v: Option<Result<f64, ErrorKind>> = match &v {
            Some(Value::Number(n)) => Some(Ok(*n)),
            Some(Value::Error(e)) => Some(Err(*e)),
            _ => None,
        };
        match df.show_as {
            ShowAs::Difference | ShowAs::PctDifference | ShowAs::PctOf => {
                let base = match &df.base_item {
                    BaseItem::Prev => if pos == 0 { None } else { sibs.get(pos - 1).copied() },
                    BaseItem::Next => sibs.get(pos + 1).copied(),
                    BaseItem::Item(label) => {
                        let fi = &self.items[&bf];
                        match fi.items.iter().position(|it| ikey(&it.label) == ikey(label)) {
                            Some(i) => Some(i as u32),
                            None => return Value::Error(ErrorKind::NA),
                        }
                    }
                };
                let Some(base) = base else {
                    return if df.show_as == ShowAs::PctOf { Value::Error(ErrorKind::NA) } else { Value::Empty };
                };
                if base == cur {
                    return if df.show_as == ShowAs::PctOf && cur_v.is_some() { Value::Number(1.0) } else { Value::Empty };
                }
                let b = with(base);
                if cur_v.is_none() && b.is_none() {
                    return Value::Empty;
                }
                let (a, b) = match (cur_v.unwrap_or(Ok(0.0)), b.unwrap_or(Ok(0.0))) {
                    (Ok(a), Ok(b)) => (a, b),
                    (Err(e), _) | (_, Err(e)) => return Value::Error(e),
                };
                match df.show_as {
                    ShowAs::Difference => Value::Number(a - b),
                    ShowAs::PctDifference => if b == 0.0 { Value::Error(ErrorKind::Null) } else { Value::Number((a - b) / b) },
                    _ => {
                        if cur_v.is_none() {
                            Value::Empty
                        } else if b == 0.0 {
                            Value::Error(ErrorKind::Div0)
                        } else {
                            Value::Number(a / b)
                        }
                    }
                }
            }
            ShowAs::RunningTotal | ShowAs::PctRunningTotal => {
                let mut run = 0.0;
                for s in &sibs[..=pos.min(sibs.len().saturating_sub(1))] {
                    match with(*s) {
                        Some(Ok(x)) => run += x,
                        Some(Err(e)) => return Value::Error(e),
                        None => {}
                    }
                }
                if df.show_as == ShowAs::RunningTotal {
                    return Value::Number(run);
                }
                let total: f64 = sibs.iter().filter_map(|s| with(*s).and_then(|x| x.ok())).sum();
                if total == 0.0 { Value::Error(ErrorKind::Div0) } else { Value::Number(run / total) }
            }
            ShowAs::RankAsc | ShowAs::RankDesc => {
                let Some(Ok(a)) = cur_v else { return Value::Empty };
                let others: Vec<f64> = sibs.iter().filter_map(|s| with(*s).and_then(|x| x.ok())).collect();
                let better = others.iter().filter(|x| if df.show_as == ShowAs::RankAsc { **x < a } else { **x > a }).count();
                Value::Number(better as f64 + 1.0)
            }
            _ => v.unwrap_or(Value::Empty),
        }
    }

    /// Lines of an axis (rows or columns), Excel's order.
    fn lines(&self, rows: bool) -> Vec<Line> {
        let pt = self.pt;
        let fields: Vec<usize> = if rows { pt.rows.clone() } else { pt.cols.clone() };
        let fields: Vec<usize> = fields.into_iter().filter(|f| *f == VALUES || *f < pt.fields.len()).collect();
        let mut out = Vec::new();
        if fields.is_empty() {
            if rows || !pt.values.is_empty() {
                out.push(Line { kind: LineKind::Total, values: true, ..Default::default() });
            }
            return out;
        }
        let tree = if rows { &self.rtree } else { &self.ctree };
        self.walk(rows, &fields, 0, tree, &mut Vec::new(), None, &mut out);
        let real = fields.iter().any(|f| *f != VALUES);
        let grand = if rows { pt.col_grand } else { pt.row_grand };
        if grand && real {
            if fields.contains(&VALUES) {
                for d in 0..pt.values.len() {
                    out.push(Line { kind: LineKind::Grand, data: Some(d), values: true, ..Default::default() });
                }
            } else {
                out.push(Line { kind: LineKind::Grand, values: true, ..Default::default() });
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(&self, rows: bool, fields: &[usize], level: usize, node: &Node, ids: &mut Vec<u32>, data: Option<usize>, out: &mut Vec<Line>) {
        if level >= fields.len() {
            return;
        }
        let pt = self.pt;
        let f = fields[level];
        let last = level + 1 == fields.len();
        let path = |ids: &[u32]| -> Vec<(usize, Item)> {
            let rf = if rows { &self.rf } else { &self.cf };
            ids.iter().enumerate().map(|(i, id)| (rf[i], self.item(rf[i], *id).clone())).collect()
        };
        if f == VALUES {
            for d in 0..pt.values.len() {
                out.push(Line { kind: LineKind::Item, path: path(ids), ids: ids.clone(), data: Some(d), depth: level, values: last, children: !last, collapsed: false });
                if !last {
                    self.walk(rows, fields, level + 1, node, ids, Some(d), out);
                }
            }
            return;
        }
        let field = &pt.fields[f];
        let values_below = fields[level + 1..].contains(&VALUES);
        let real_below = fields[level + 1..].iter().any(|x| *x != VALUES);
        for kid in &node.kids {
            ids.push(kid.item);
            let label = ikey(&self.item(f, kid.item).label);
            let has_kids = !last;
            let collapsed = has_kids && real_below && field.collapsed.contains(&label);
            let top_sub = rows && has_kids && !collapsed && field.subtotal && pt.subtotals_top && pt.form != Form::Tabular && !values_below;
            let values = if values_below { false } else { !has_kids || collapsed || top_sub };
            out.push(Line { kind: LineKind::Item, path: path(ids), ids: ids.clone(), data, depth: level, values, children: has_kids && real_below, collapsed });
            if has_kids && !collapsed {
                self.walk(rows, fields, level + 1, kid, ids, data, out);
            }
            if has_kids && real_below {
                if values_below {
                    if field.subtotal || collapsed {
                        for d in 0..pt.values.len() {
                            out.push(Line { kind: LineKind::Subtotal, path: path(ids), ids: ids.clone(), data: Some(d), depth: level, values: true, ..Default::default() });
                        }
                    }
                } else if field.subtotal && !top_sub && !collapsed {
                    out.push(Line { kind: LineKind::Subtotal, path: path(ids), ids: ids.clone(), data, depth: level, values: true, ..Default::default() });
                }
            }
            if rows && pt.blank_rows && has_kids {
                out.push(Line { kind: LineKind::Blank, path: path(ids), ids: ids.clone(), data, depth: level, ..Default::default() });
            }
            ids.pop();
        }
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LineKind {
    #[default]
    Item,
    Subtotal,
    Grand,
    Blank,
    /// The single line of an axis without fields.
    Total,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Line {
    pub kind: LineKind,
    /// (field, item) of the line's real fields, outer first.
    pub path: Vec<(usize, Item)>,
    pub ids: Vec<u32>,
    /// Data field of the line (Σ Values on this axis).
    pub data: Option<usize>,
    /// Axis level of the line's label.
    pub depth: usize,
    /// The line shows values.
    pub values: bool,
    /// The item has child items (expand/collapse button).
    pub children: bool,
    pub collapsed: bool,
}

/// What a report cell is, for styling and for clicks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Role {
    #[default]
    None,
    PageLabel(u16),
    PageValue(u16),
    /// Data caption in the top-left corner.
    Caption,
    /// "Column Labels" / "Row Labels" (compact form).
    ColHeader,
    RowHeader,
    /// Field caption of the outline and tabular forms: (0 rows / 1 columns, position).
    FieldHeader(u8, u16),
    /// Column label: (column line, axis level).
    ColLabel(u32, u16),
    /// Row label: (row line, axis level).
    RowLabel(u32, u16),
    /// Value: (row line, column line).
    Value(u32, u32),
    /// Empty cell inside the report.
    Body,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct OutCell {
    pub r: u32,
    pub c: u32,
    pub value: Value,
    pub num_fmt: String,
    /// Row labels are left aligned.
    pub left: bool,
    /// Indent level (compact form).
    pub indent: u8,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Output {
    /// The table (Excel's TableRange1) and the whole report with its filters (TableRange2).
    pub range: Range,
    pub full: Range,
    pub header_rows: u32,
    pub label_cols: u32,
    /// All row / column lines and, per displayed row / column, the line shown there.
    pub rows: Vec<Line>,
    pub cols: Vec<Line>,
    pub row_at: Vec<usize>,
    pub col_at: Vec<usize>,
    pub cells: Vec<OutCell>,
    /// Role of every cell of `full`, row-major.
    pub roles: Vec<Role>,
    /// Report without fields: the placeholder box is shown.
    pub placeholder: bool,
    /// Set once the report has been laid out.
    pub drawn: bool,
}

impl Output {
    pub fn role(&self, r: u32, c: u32) -> Role {
        if !self.drawn || !self.full.contains(r, c) {
            return Role::None;
        }
        let w = self.full.cols() as usize;
        self.roles.get((r - self.full.r1) as usize * w + (c - self.full.c1) as usize).copied().unwrap_or_default()
    }

    /// Indent level of a compact-form row label.
    pub fn indent(&self, r: u32, c: u32) -> u8 {
        match self.role(r, c) {
            Role::RowLabel(li, d) if self.label_cols == 1 => match self.rows.get(li as usize) {
                Some(l) if matches!(l.kind, LineKind::Item | LineKind::Subtotal) => d.min(15) as u8,
                _ => 0,
            },
            _ => 0,
        }
    }
}

fn data_name(pt: &PivotTable, d: Option<usize>) -> String {
    d.and_then(|d| pt.values.get(d)).map(|v| v.name.clone()).unwrap_or_default()
}

/// Text of a line's label (the item, "East Total", "Grand Total", ...).
pub fn line_label(pt: &PivotTable, axis: &[usize], l: &Line) -> String {
    let item = l.path.last().map(|(_, it)| it.label.clone()).unwrap_or_default();
    let values_below = axis.get(l.depth + 1..).is_some_and(|rest| rest.contains(&VALUES));
    match l.kind {
        LineKind::Item => {
            if axis.get(l.depth) == Some(&VALUES) {
                data_name(pt, l.data)
            } else {
                item
            }
        }
        LineKind::Subtotal => {
            if values_below {
                format!("{item} {}", data_name(pt, l.data))
            } else {
                format!("{item} Total")
            }
        }
        LineKind::Grand => {
            if axis.contains(&VALUES) {
                format!("Total {}", data_name(pt, l.data))
            } else {
                "Grand Total".into()
            }
        }
        LineKind::Total => {
            if pt.values.len() == 1 { pt.values[0].name.clone() } else { String::new() }
        }
        LineKind::Blank => String::new(),
    }
}

/// Value written for a line's label.
fn label_value(pt: &PivotTable, axis: &[usize], l: &Line) -> Value {
    if l.kind == LineKind::Item && axis.get(l.depth) != Some(&VALUES) {
        if let Some((_, it)) = l.path.last() {
            return it.value.clone();
        }
    }
    let s = line_label(pt, axis, l);
    if s.is_empty() { Value::Empty } else { Value::Text(s) }
}

fn label_fmt(pt: &PivotTable, axis: &[usize], l: &Line) -> String {
    if l.kind == LineKind::Item && axis.get(l.depth) != Some(&VALUES) {
        if let Some((f, it)) = l.path.last() {
            if matches!(it.value, Value::Number(_)) && pt.fields[*f].num_group.is_none() {
                if let FieldSource::Column(c) = pt.fields[*f].source {
                    return pt.cache.formats.get(c).cloned().unwrap_or_default();
                }
            }
        }
    }
    String::new()
}

fn field_caption(pt: &PivotTable, f: usize) -> String {
    if f == VALUES { pt.data_caption.clone() } else { pt.fields[f].display().to_string() }
}

/// Page field value text: the selected item, "(All)" or "(Multiple Items)".
pub fn page_text(pt: &PivotTable, f: usize) -> String {
    let field = &pt.fields[f];
    if let Some(p) = &field.page {
        let fi = field_items(pt, f);
        return fi.items.iter().find(|it| ikey(&it.label) == *p).map(|it| it.label.clone()).unwrap_or_else(|| p.clone());
    }
    if field.hidden.is_empty() { "(All)".into() } else { "(Multiple Items)".into() }
}

/// Lays out the report at its anchor (moved down when the report filters need room).
pub fn layout(pt: &PivotTable) -> Output {
    let mut out = Output { drawn: true, ..Default::default() };
    let npages = pt.filters.len() as u32;
    let top = if npages > 0 { pt.anchor.0.max(npages + 1) } else { pt.anchor.0 };
    let (ar, ac) = (top, pt.anchor.1);
    let mut cells: Vec<OutCell> = Vec::new();
    let mut roles: HashMap<(u32, u32), Role> = HashMap::new();
    let put = |cells: &mut Vec<OutCell>, roles: &mut HashMap<(u32, u32), Role>, r: u32, c: u32, v: Value, fmt: String, left: bool, indent: u8, role: Role| {
        if r < MAX_ROWS && c < MAX_COLS {
            roles.insert((r, c), role);
            if !v.is_empty() {
                cells.push(OutCell { r, c, value: v, num_fmt: fmt, left, indent });
            }
        }
    };
    for (k, &f) in pt.filters.iter().enumerate() {
        if f >= pt.fields.len() {
            continue;
        }
        let r = ar - npages - 1 + k as u32;
        put(&mut cells, &mut roles, r, ac, Value::Text(pt.fields[f].display().to_string()), String::new(), false, 0, Role::PageLabel(k as u16));
        put(&mut cells, &mut roles, r, ac + 1, Value::Text(page_text(pt, f)), String::new(), true, 0, Role::PageValue(k as u16));
    }
    if pt.rows.is_empty() && pt.cols.is_empty() && pt.values.is_empty() {
        out.placeholder = true;
        out.range = Range::new(ar, ac, (ar + 17).min(MAX_ROWS - 1), (ac + 2).min(MAX_COLS - 1));
    } else {
        let calc = Calc::new(pt);
        let rows_ax: Vec<usize> = pt.rows.iter().copied().filter(|f| *f == VALUES || *f < pt.fields.len()).collect();
        let cols_ax: Vec<usize> = pt.cols.iter().copied().filter(|f| *f == VALUES || *f < pt.fields.len()).collect();
        out.rows = calc.lines(true);
        out.cols = calc.lines(false);
        let nd = pt.values.len();
        let real_cols = !calc.cf.is_empty();
        let hr: u32 = if real_cols { 1 + cols_ax.len() as u32 } else { 1 };
        let compact = pt.form == Form::Compact;
        let lc: u32 = if !rows_ax.is_empty() {
            if compact { 1 } else { rows_ax.len() as u32 }
        } else if real_cols {
            1
        } else {
            0
        };
        out.header_rows = hr;
        out.label_cols = lc;
        // Columns: parent items only label the header above their first column.
        let mut pending: Vec<usize> = Vec::new();
        let data_c0 = ac + lc;
        for (li, l) in out.cols.iter().enumerate() {
            if !l.values {
                pending.push(li);
                continue;
            }
            let j = out.col_at.len() as u32;
            out.col_at.push(li);
            let c = data_c0 + j;
            if real_cols {
                for &p in &pending {
                    let pl = &out.cols[p];
                    let v = label_value(pt, &cols_ax, pl);
                    put(&mut cells, &mut roles, ar + 1 + pl.depth as u32, c, v, label_fmt(pt, &cols_ax, pl), false, 0, Role::ColLabel(p as u32, pl.depth as u16));
                }
                let depth = if l.kind == LineKind::Grand { 0 } else { l.depth };
                put(&mut cells, &mut roles, ar + 1 + depth as u32, c, label_value(pt, &cols_ax, l), label_fmt(pt, &cols_ax, l), false, 0, Role::ColLabel(li as u32, depth as u16));
            } else {
                let name = data_name(pt, l.data.or(if nd == 1 { Some(0) } else { None }));
                put(&mut cells, &mut roles, ar, c, if name.is_empty() { Value::Empty } else { Value::Text(name) }, String::new(), false, 0, Role::ColLabel(li as u32, 0));
            }
            pending.clear();
        }
        // Header captions.
        if real_cols {
            if !rows_ax.is_empty() && nd == 1 {
                put(&mut cells, &mut roles, ar, ac, Value::Text(pt.values[0].name.clone()), String::new(), false, 0, Role::Caption);
            }
            if pt.field_headers {
                if compact {
                    put(&mut cells, &mut roles, ar, data_c0, Value::Text("Column Labels".into()), String::new(), false, 0, Role::ColHeader);
                } else {
                    for (k, &f) in cols_ax.iter().enumerate() {
                        put(&mut cells, &mut roles, ar, data_c0 + k as u32, Value::Text(field_caption(pt, f)), String::new(), false, 0, Role::FieldHeader(1, k as u16));
                    }
                }
            }
        }
        if pt.field_headers && !rows_ax.is_empty() {
            if compact {
                put(&mut cells, &mut roles, ar + hr - 1, ac, Value::Text("Row Labels".into()), String::new(), false, 0, Role::RowHeader);
            } else {
                for (k, &f) in rows_ax.iter().enumerate() {
                    put(&mut cells, &mut roles, ar + hr - 1, ac + k as u32, Value::Text(field_caption(pt, f)), String::new(), false, 0, Role::FieldHeader(0, k as u16));
                }
            }
        }
        // Rows.
        let tabular = pt.form == Form::Tabular;
        let repeat = pt.repeat_labels && !compact;
        let mut pending: Vec<usize> = Vec::new();
        let mut parents: Vec<usize> = Vec::new();
        let indent_of = |l: &Line| -> u8 {
            if !compact || l.kind == LineKind::Grand || l.kind == LineKind::Total {
                return 0;
            }
            l.depth.min(15) as u8
        };
        for (li, l) in out.rows.iter().enumerate() {
            if l.kind == LineKind::Item && l.children && !l.collapsed {
                parents.truncate(l.depth);
                parents.push(li);
            }
            if tabular && l.kind == LineKind::Item && l.children && !l.collapsed {
                pending.push(li);
                continue;
            }
            let i = out.row_at.len() as u32;
            out.row_at.push(li);
            let r = ar + hr + i;
            if lc == 0 {
                pending.clear();
                continue;
            }
            for &p in &pending {
                let pl = &out.rows[p];
                put(&mut cells, &mut roles, r, ac + pl.depth as u32, label_value(pt, &rows_ax, pl), label_fmt(pt, &rows_ax, pl), true, 0, Role::RowLabel(p as u32, pl.depth as u16));
            }
            pending.clear();
            if l.kind == LineKind::Blank {
                continue;
            }
            if repeat && l.kind == LineKind::Item {
                for &p in parents.iter().filter(|p| out.rows[**p].depth < l.depth) {
                    let pl = &out.rows[p];
                    if !roles.contains_key(&(r, ac + pl.depth as u32)) {
                        put(&mut cells, &mut roles, r, ac + pl.depth as u32, label_value(pt, &rows_ax, pl), label_fmt(pt, &rows_ax, pl), true, 0, Role::RowLabel(p as u32, pl.depth as u16));
                    }
                }
            }
            let depth = if matches!(l.kind, LineKind::Grand | LineKind::Total) { 0 } else { l.depth };
            let c = if compact { ac } else { ac + depth as u32 };
            put(&mut cells, &mut roles, r, c, label_value(pt, &rows_ax, l), label_fmt(pt, &rows_ax, l), true, indent_of(l), Role::RowLabel(li as u32, depth as u16));
        }
        // Values.
        if nd > 0 {
            for (i, &li) in out.row_at.iter().enumerate() {
                let l = &out.rows[li];
                if !l.values {
                    continue;
                }
                for (j, &cj) in out.col_at.iter().enumerate() {
                    let cl = &out.cols[cj];
                    if !cl.values {
                        continue;
                    }
                    let d = l.data.or(cl.data).unwrap_or(0).min(nd - 1);
                    let df = &pt.values[d];
                    let mut v = calc.shown(&l.ids, &cl.ids, d);
                    if v.is_empty() && !pt.empty_text.is_empty() {
                        v = Value::Text(pt.empty_text.clone());
                    }
                    let fmt = if df.num_fmt.is_empty() && df.show_as.is_percent() { "0.00%".to_string() } else { df.num_fmt.clone() };
                    put(&mut cells, &mut roles, ar + hr + i as u32, data_c0 + j as u32, v, fmt, false, 0, Role::Value(li as u32, cj as u32));
                }
            }
        }
        let h = hr + out.row_at.len() as u32;
        let w = (lc + out.col_at.len() as u32).max(1);
        out.range = Range::new(ar, ac, (ar + h - 1).min(MAX_ROWS - 1), (ac + w - 1).min(MAX_COLS - 1));
    }
    out.full = if npages > 0 {
        Range::new(ar - npages - 1, ac, out.range.r2, out.range.c2.max(ac + 1).min(MAX_COLS - 1))
    } else {
        out.range
    };
    let w = out.full.cols() as usize;
    out.roles = vec![Role::None; out.full.rows() as usize * w];
    for r in out.range.r1..=out.range.r2 {
        for c in out.range.c1..=out.range.c2 {
            out.roles[(r - out.full.r1) as usize * w + (c - out.full.c1) as usize] = Role::Body;
        }
    }
    for ((r, c), role) in roles {
        if out.full.contains(r, c) {
            out.roles[(r - out.full.r1) as usize * w + (c - out.full.c1) as usize] = role;
        }
    }
    cells.sort_by_key(|x| (x.r, x.c));
    out.cells = cells;
    out
}

// ---------------------------------------------------------------------------
// Writing into the sheet
// ---------------------------------------------------------------------------

pub const OVERLAP: &str = "A PivotTable report cannot overlap another PivotTable report.";

/// Lays out PivotTable `pi` of sheet `si` and writes its cells, replacing the previous report.
pub fn update(wb: &mut Workbook, si: usize, pi: usize) -> Result<(), String> {
    let out = layout(&wb.sheets[si].pivots[pi]);
    if wb.sheets[si].pivots.iter().enumerate().any(|(k, o)| k != pi && o.out.drawn && o.out.full.intersects(&out.full)) {
        return Err(OVERLAP.into());
    }
    let old = wb.sheets[si].pivots[pi].out.clone();
    if old.drawn {
        let sh = &mut wb.sheets[si];
        let keys: Vec<(u32, u32)> = sh.iter_range(old.full).map(|(r, c, _)| (r, c)).collect();
        for (r, c) in keys {
            sh.remove(r, c);
        }
    }
    let mut ids: HashMap<(String, bool), u32> = HashMap::new();
    for oc in &out.cells {
        let key = (oc.num_fmt.clone(), oc.left);
        let style = match ids.get(&key) {
            Some(&s) => s,
            None => {
                let s = wb.intern_style(Style { num_fmt: oc.num_fmt.clone(), halign: if oc.left { HAlign::Left } else { HAlign::General }, ..Default::default() });
                ids.insert(key, s);
                s
            }
        };
        wb.sheets[si].set(oc.r, oc.c, Some(Cell { value: oc.value.clone(), formula: None, style }));
    }
    let pt = &mut wb.sheets[si].pivots[pi];
    pt.anchor = (out.range.r1, out.range.c1);
    pt.out = out;
    Ok(())
}

/// Rebuilds the cache from the source and updates the report.
pub fn refresh(wb: &mut Workbook, si: usize, pi: usize) -> Result<(), String> {
    let src = wb.sheets[si].pivots[pi].source.clone();
    let cache = super::build_cache(wb, &src, si)?;
    wb.sheets[si].pivots[pi].set_cache(cache);
    update(wb, si, pi)
}

/// Removes a report and its cells.
pub fn delete(wb: &mut Workbook, si: usize, pi: usize) {
    let full = wb.sheets[si].pivots[pi].out.full;
    let drawn = wb.sheets[si].pivots[pi].out.drawn;
    let sh = &mut wb.sheets[si];
    if drawn {
        let keys: Vec<(u32, u32)> = sh.iter_range(full).map(|(r, c, _)| (r, c)).collect();
        for (r, c) in keys {
            sh.remove(r, c);
        }
    }
    sh.pivots.remove(pi);
}

// ---------------------------------------------------------------------------
// Details, GETPIVOTDATA
// ---------------------------------------------------------------------------

/// Records behind a value cell (Show Details): (row line, column line) of the last layout.
pub fn detail_records(pt: &PivotTable, li: usize, cj: usize) -> Vec<usize> {
    let calc = Calc::new(pt);
    let mut want: Vec<(usize, u32)> = Vec::new();
    if let Some(l) = pt.out.rows.get(li) {
        want.extend(l.ids.iter().enumerate().map(|(i, id)| (calc.rf[i], *id)));
    }
    if let Some(l) = pt.out.cols.get(cj) {
        want.extend(l.ids.iter().enumerate().map(|(i, id)| (calc.cf[i], *id)));
    }
    calc.recs.iter().copied().filter(|&i| want.iter().all(|(f, id)| calc.items[f].of[i] == *id)).collect()
}

fn item_matches(it: &Item, v: &Value) -> bool {
    match v {
        Value::Text(s) => ikey(&it.label) == ikey(s.trim()) || matches!((&it.value, crate::numfmt::parse_number(s)), (Value::Number(a), Some(b)) if *a == b),
        Value::Number(n) => match &it.value {
            Value::Number(a) => a == n,
            _ => it.label == crate::numfmt::general(*n),
        },
        Value::Bool(b) => it.value == Value::Bool(*b),
        Value::Error(e) => it.value == Value::Error(*e),
        Value::Empty => it.key == Key::Blank,
    }
}

/// GETPIVOTDATA over the report covering (r, c) of sheet `si`.
pub fn get_pivot_data(wb: &Workbook, si: usize, r: u32, c: u32, data: &str, pairs: &[(String, Value)]) -> Value {
    let refe = Value::Error(ErrorKind::Ref);
    let Some(pi) = super::at(wb, si, r, c) else { return refe };
    let pt = &wb.sheets[si].pivots[pi];
    let o = &pt.out;
    let data = data.trim();
    let d = pt
        .values
        .iter()
        .position(|v| v.name.eq_ignore_ascii_case(data))
        .or_else(|| pt.values.iter().position(|v| pt.fields.get(v.field).is_some_and(|f| f.name.eq_ignore_ascii_case(data) || f.display().eq_ignore_ascii_case(data))));
    let Some(d) = d else { return refe };
    // Resolve the field names; report filters must match their current selection.
    let mut want: Vec<(usize, &Value)> = Vec::new();
    for (name, v) in pairs {
        let Some(f) = pt.field_index(name.trim()) else { return refe };
        if pt.filters.contains(&f) {
            let sel = page_text(pt, f);
            if !item_matches(&Item { label: sel.clone(), value: Value::Text(sel), key: Key::Blank }, v) {
                return refe;
            }
            continue;
        }
        want.push((f, v));
    }
    for (i, &li) in o.row_at.iter().enumerate() {
        let l = &o.rows[li];
        if !l.values {
            continue;
        }
        for (j, &cj) in o.col_at.iter().enumerate() {
            let cl = &o.cols[cj];
            if !cl.values || l.data.or(cl.data).unwrap_or(0) != d {
                continue;
            }
            let path: Vec<&(usize, Item)> = l.path.iter().chain(cl.path.iter()).collect();
            if path.len() != want.len() {
                continue;
            }
            if want.iter().all(|(f, v)| path.iter().any(|(pf, it)| pf == f && item_matches(it, v))) {
                let (rr, cc) = (o.range.r1 + o.header_rows + i as u32, o.range.c1 + o.label_cols + j as u32);
                return match wb.sheets[si].value(rr, cc) {
                    Value::Empty => Value::Number(0.0),
                    v => v.clone(),
                };
            }
        }
    }
    refe
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// The GETPIVOTDATA formula Excel generates when a value cell is clicked while typing a formula.
pub fn getpivotdata_formula(pt: &PivotTable, li: usize, cj: usize, anchor: &str) -> Option<String> {
    let o = &pt.out;
    let (l, cl) = (o.rows.get(li)?, o.cols.get(cj)?);
    let d = l.data.or(cl.data).unwrap_or(0);
    let df = pt.values.get(d)?;
    let fname = &pt.fields.get(df.field)?.name;
    let default = df.name.eq_ignore_ascii_case(&format!("{} of {}", df.agg.label(), fname));
    let mut s = format!("GETPIVOTDATA({},{}", quote(if default { fname } else { &df.name }), anchor);
    for (f, it) in l.path.iter().chain(cl.path.iter()) {
        s.push(',');
        s.push_str(&quote(pt.fields[*f].display()));
        s.push(',');
        match &it.value {
            Value::Number(n) => {
                let fmt = pt.cache.formats.get(pt.fields[*f].column()).cloned().unwrap_or_default();
                if crate::numfmt::is_date_format(&fmt) && matches!(pt.fields[*f].source, FieldSource::Column(_)) {
                    let (y, m, dd) = crate::numfmt::serial_to_ymd(*n);
                    s.push_str(&format!("DATE({y},{m},{dd})"));
                } else {
                    s.push_str(&crate::numfmt::general(*n));
                }
            }
            Value::Bool(b) => s.push_str(if *b { "TRUE" } else { "FALSE" }),
            _ => s.push_str(&quote(&it.label)),
        }
    }
    s.push(')');
    Some(s)
}
