//! Solver: optimise an objective cell by changing variable cells subject to constraints.
//!
//! Linear models use a two-phase simplex method (with branch and bound for integer and binary
//! constraints); other models use a penalty method with Nelder-Mead and gradient refinement.
//! The model is kept in the sheet's `solver_*` names, the same names Excel's Solver uses.

use crate::formula;
use crate::model::{col_name, Range, Value, Workbook};
use crate::whatif::number_at;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Goal {
    Max,
    Min,
    Value(f64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rel {
    Le,
    Eq,
    Ge,
    Int,
    Bin,
    Dif,
}

impl Rel {
    pub const ALL: [Rel; 6] = [Rel::Le, Rel::Eq, Rel::Ge, Rel::Int, Rel::Bin, Rel::Dif];

    pub fn symbol(self) -> &'static str {
        match self {
            Rel::Le => "<=",
            Rel::Eq => "=",
            Rel::Ge => ">=",
            Rel::Int => "int",
            Rel::Bin => "bin",
            Rel::Dif => "dif",
        }
    }

    /// Code used in the `solver_rel` names.
    fn code(self) -> u8 {
        match self {
            Rel::Le => 1,
            Rel::Eq => 2,
            Rel::Ge => 3,
            Rel::Int => 4,
            Rel::Bin => 5,
            Rel::Dif => 6,
        }
    }

    fn from_code(n: u8) -> Option<Rel> {
        Rel::ALL.into_iter().find(|r| r.code() == n)
    }

    /// Integer-type constraints have no right-hand side.
    pub fn is_type(self) -> bool {
        matches!(self, Rel::Int | Rel::Bin | Rel::Dif)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Method {
    #[default]
    Grg,
    Simplex,
    Evolutionary,
}

impl Method {
    pub fn label(self) -> &'static str {
        match self {
            Method::Grg => "GRG Nonlinear",
            Method::Simplex => "Simplex LP",
            Method::Evolutionary => "Evolutionary",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Constraint {
    pub lhs: Range,
    pub rel: Rel,
    /// A number, a reference or a formula (without '='); empty for int/bin/dif.
    pub rhs: String,
}

impl Constraint {
    pub fn describe(&self) -> String {
        let l = abs(self.lhs);
        match self.rel {
            Rel::Int => format!("{l} = integer"),
            Rel::Bin => format!("{l} = binary"),
            Rel::Dif => format!("{l} = AllDifferent"),
            r => format!("{l} {} {}", r.symbol(), self.rhs),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SolverModel {
    pub objective: Option<(u32, u32)>,
    pub goal: Goal,
    pub vars: Vec<Range>,
    pub constraints: Vec<Constraint>,
    pub nonneg: bool,
    pub method: Method,
}

impl Default for SolverModel {
    fn default() -> Self {
        SolverModel { objective: None, goal: Goal::Max, vars: Vec::new(), constraints: Vec::new(), nonneg: true, method: Method::Grg }
    }
}

/// `$A$1` / `$A$1:$B$2`.
pub fn abs(rg: Range) -> String {
    let a = |r: u32, c: u32| format!("${}${}", col_name(c), r + 1);
    if rg.is_single() { a(rg.r1, rg.c1) } else { format!("{}:{}", a(rg.r1, rg.c1), a(rg.r2, rg.c2)) }
}

fn strip_sheet(s: &str) -> &str {
    s.trim().trim_start_matches('=').rsplit('!').next().unwrap_or("")
}

impl SolverModel {
    /// Reads the model from a sheet's `solver_*` names (written by Excel or by `to_names`).
    pub fn from_names(names: &[(String, String)]) -> Option<SolverModel> {
        let get = |k: &str| names.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.trim().trim_start_matches('=').trim_matches('"').to_string());
        let num = |k: &str| get(k).and_then(|v| v.parse::<f64>().ok());
        let objective = get("solver_opt").and_then(|v| Range::parse(strip_sheet(&v))).map(|r| (r.r1, r.c1));
        let vars: Vec<Range> = get("solver_adj").map(|v| v.split(',').filter_map(|p| Range::parse(strip_sheet(p))).collect()).unwrap_or_default();
        if objective.is_none() && vars.is_empty() {
            return None;
        }
        let goal = match num("solver_typ").unwrap_or(1.0) as i32 {
            2 => Goal::Min,
            3 => Goal::Value(num("solver_val").unwrap_or(0.0)),
            _ => Goal::Max,
        };
        let mut constraints = Vec::new();
        for i in 1..=(num("solver_num").unwrap_or(0.0) as usize).min(500) {
            let (Some(l), Some(rel)) = (get(&format!("solver_lhs{i}")), num(&format!("solver_rel{i}")).and_then(|n| Rel::from_code(n as u8))) else { continue };
            let Some(lhs) = Range::parse(strip_sheet(&l)) else { continue };
            let rhs = if rel.is_type() { String::new() } else { get(&format!("solver_rhs{i}")).map(|v| strip_sheet(&v).to_string()).unwrap_or_default() };
            constraints.push(Constraint { lhs, rel, rhs });
        }
        let method = match num("solver_eng").unwrap_or(1.0) as i32 {
            2 => Method::Simplex,
            3 => Method::Evolutionary,
            _ => Method::Grg,
        };
        Some(SolverModel { objective, goal, vars, constraints, nonneg: num("solver_neg").unwrap_or(1.0) as i32 == 1, method })
    }

    /// The `solver_*` names describing the model on sheet `sheet`.
    pub fn to_names(&self, sheet: &str) -> Vec<(String, String)> {
        let q = formula::quote_sheet(sheet);
        let r = |rg: Range| format!("{q}!{}", abs(rg));
        let mut out = vec![("solver_ver".to_string(), "3".to_string())];
        if let Some((rr, cc)) = self.objective {
            out.push(("solver_opt".into(), r(Range::cell(rr, cc))));
        }
        let (typ, val) = match self.goal {
            Goal::Max => (1, 0.0),
            Goal::Min => (2, 0.0),
            Goal::Value(v) => (3, v),
        };
        out.push(("solver_typ".into(), typ.to_string()));
        out.push(("solver_val".into(), formula::print(&formula::Expr::Num(val))));
        if !self.vars.is_empty() {
            out.push(("solver_adj".into(), self.vars.iter().map(|v| r(*v)).collect::<Vec<_>>().join(",")));
        }
        out.push(("solver_num".into(), self.constraints.len().to_string()));
        for (i, c) in self.constraints.iter().enumerate() {
            let i = i + 1;
            out.push((format!("solver_lhs{i}"), r(c.lhs)));
            out.push((format!("solver_rel{i}"), c.rel.code().to_string()));
            let rhs = match c.rel {
                Rel::Int => "\"integer\"".to_string(),
                Rel::Bin => "\"binary\"".to_string(),
                Rel::Dif => "\"AllDifferent\"".to_string(),
                _ if Range::parse(&c.rhs).is_some() => format!("{q}!{}", c.rhs),
                _ => c.rhs.clone(),
            };
            out.push((format!("solver_rhs{i}"), rhs));
        }
        out.push(("solver_neg".into(), if self.nonneg { "1" } else { "2" }.into()));
        let eng = match self.method {
            Method::Grg => 1,
            Method::Simplex => 2,
            Method::Evolutionary => 3,
        };
        out.push(("solver_eng".into(), eng.to_string()));
        out.push(("solver_lin".into(), if self.method == Method::Simplex { "1" } else { "2" }.into()));
        out
    }
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Optimal,
    IntegerOptimal,
    Infeasible,
    Unbounded,
    NotLinear,
    Error,
}

impl Status {
    pub fn found(self) -> bool {
        matches!(self, Status::Optimal | Status::IntegerOptimal)
    }
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub status: Status,
    pub message: String,
    pub detail: String,
    /// Variable cells (in order) with original and final values.
    pub cells: Vec<(u32, u32)>,
    pub original: Vec<f64>,
    pub x: Vec<f64>,
    pub original_objective: f64,
    pub objective: f64,
    pub iterations: usize,
}

// ---------------------------------------------------------------------------
// Problem evaluation
// ---------------------------------------------------------------------------

/// One scalar constraint `lhs cell (rel) rhs value`.
#[derive(Clone, Debug)]
struct Row {
    cell: (u32, u32),
    rel: Rel,
    rhs: RhsSrc,
}

#[derive(Clone, Debug)]
enum RhsSrc {
    Const(f64),
    Cell(usize, u32, u32),
    Formula(formula::Expr),
}

struct Problem<'a> {
    wb: &'a mut Workbook,
    si: usize,
    cells: Vec<(u32, u32)>,
    objective: (u32, u32),
    rows: Vec<Row>,
    evals: usize,
}

impl Problem<'_> {
    fn set(&mut self, x: &[f64]) {
        let mut edited = Vec::with_capacity(x.len());
        for (k, (r, c)) in self.cells.iter().enumerate() {
            let style = self.wb.sheets[self.si].get(*r, *c).map_or(0, |c| c.style);
            self.wb.sheets[self.si].set(*r, *c, Some(crate::model::Cell { value: Value::Number(x[k]), formula: None, style }));
            edited.push((self.si, Range::cell(*r, *c)));
        }
        formula::recalc_ranges(self.wb, &edited);
        self.evals += 1;
    }

    fn objective(&self) -> Option<f64> {
        number_at(self.wb, (self.si, self.objective.0, self.objective.1))
    }

    fn rhs(&self, r: &RhsSrc) -> Option<f64> {
        match r {
            RhsSrc::Const(v) => Some(*v),
            RhsSrc::Cell(s, rr, cc) => number_at(self.wb, (*s, *rr, *cc)),
            RhsSrc::Formula(e) => match formula::eval_expr(self.wb, self.si, 0, 0, e) {
                Value::Number(n) => Some(n),
                Value::Bool(b) => Some(b as u8 as f64),
                _ => None,
            },
        }
    }

    /// lhs - rhs of every non-integer row.
    fn gaps(&self) -> Option<Vec<f64>> {
        self.rows.iter().filter(|r| !r.rel.is_type()).map(|r| Some(number_at(self.wb, (self.si, r.cell.0, r.cell.1))? - self.rhs(&r.rhs)?)).collect()
    }

    /// (objective, gaps) at x.
    fn eval(&mut self, x: &[f64]) -> Option<(f64, Vec<f64>)> {
        self.set(x);
        Some((self.objective()?, self.gaps()?))
    }
}

fn violation(rel: Rel, g: f64) -> f64 {
    match rel {
        Rel::Le => g.max(0.0),
        Rel::Ge => (-g).max(0.0),
        Rel::Eq => g.abs(),
        _ => 0.0,
    }
}

const FEAS_TOL: f64 = 1e-6;

// ---------------------------------------------------------------------------
// Solve
// ---------------------------------------------------------------------------

/// Builds the problem, solves it and leaves the solution in the variable cells (the caller
/// decides whether to keep it). Returns the outcome with the original values for restoring.
pub fn solve(wb: &mut Workbook, si: usize, model: &SolverModel) -> Result<Outcome, String> {
    let objective = model.objective.ok_or("Objective Cell must be a single cell on the active sheet.")?;
    let mut cells: Vec<(u32, u32)> = Vec::new();
    for rg in &model.vars {
        if rg.rows() as u64 * rg.cols() as u64 > 2_000 {
            return Err("Too many Variable Cells.".into());
        }
        for r in rg.r1..=rg.r2 {
            for c in rg.c1..=rg.c2 {
                if !cells.contains(&(r, c)) {
                    cells.push((r, c));
                }
            }
        }
    }
    if cells.is_empty() {
        return Err("No Variable Cells were specified.".into());
    }
    for &(r, c) in &cells {
        if wb.sheets[si].get(r, c).is_some_and(|x| x.formula.is_some()) {
            return Err(format!("Variable cell {} contains a formula; Variable Cells must be constants.", abs(Range::cell(r, c))));
        }
    }
    // Expand constraints into scalar rows.
    let mut rows = Vec::new();
    let mut ints: Vec<(usize, bool)> = Vec::new();
    for con in &model.constraints {
        let n = con.lhs.rows() * con.lhs.cols();
        if con.rel.is_type() {
            for r in con.lhs.r1..=con.lhs.r2 {
                for c in con.lhs.c1..=con.lhs.c2 {
                    let Some(k) = cells.iter().position(|p| *p == (r, c)) else {
                        return Err(format!("{} constraints may only refer to Variable Cells.", con.rel.symbol()));
                    };
                    if con.rel == Rel::Dif {
                        return Err("AllDifferent constraints are not supported.".into());
                    }
                    ints.push((k, con.rel == Rel::Bin));
                }
            }
            continue;
        }
        let rhs_text = con.rhs.trim().trim_start_matches('=');
        let rhs_rg = crate::whatif::parse_range_ref(wb, rhs_text).filter(|(_, rg)| !rhs_text.parse::<f64>().is_ok() && (rg.is_single() || rg.rows() * rg.cols() == n));
        let mut k = 0;
        for r in con.lhs.r1..=con.lhs.r2 {
            for c in con.lhs.c1..=con.lhs.c2 {
                let rhs = match (&rhs_rg, rhs_text.parse::<f64>()) {
                    (_, Ok(v)) => RhsSrc::Const(v),
                    (Some((s, rg)), _) if rg.is_single() => RhsSrc::Cell(*s, rg.r1, rg.c1),
                    (Some((s, rg)), _) => RhsSrc::Cell(*s, rg.r1 + k / rg.cols(), rg.c1 + k % rg.cols()),
                    (None, _) => RhsSrc::Formula(formula::parse(rhs_text).map_err(|_| format!("Constraint right-hand side '{}' is not valid.", con.rhs))?),
                };
                rows.push(Row { cell: (r, c), rel: con.rel, rhs });
                k += 1;
            }
        }
    }
    let mut p = Problem { wb, si, cells: cells.clone(), objective, rows, evals: 0 };
    let original: Vec<f64> = cells.iter().map(|(r, c)| number_at(p.wb, (si, *r, *c)).unwrap_or(0.0)).collect();
    let original_objective = p.objective().unwrap_or(0.0);
    let res = match model.method {
        Method::Simplex => solve_lp(&mut p, model, &original, &ints),
        _ => solve_nonlinear(&mut p, model, &original, &ints),
    };
    let (status, x) = res;
    let x = x.unwrap_or_else(|| original.clone());
    p.set(&x);
    let objective_value = p.objective().unwrap_or(f64::NAN);
    let (message, detail) = messages(status, model.method);
    Ok(Outcome { status, message, detail, cells, original, x, original_objective, objective: objective_value, iterations: p.evals })
}

fn messages(status: Status, method: Method) -> (String, String) {
    let (m, d) = match status {
        Status::Optimal => (
            "Solver found a solution.  All Constraints and optimality conditions are satisfied.",
            if method == Method::Simplex {
                "When the Simplex LP engine is used, this means Solver has found a global optimal solution."
            } else {
                "When the GRG engine is used, Solver has found at least a local optimal solution."
            },
        ),
        Status::IntegerOptimal => ("Solver found an integer solution within tolerance.  All Constraints are satisfied.", "Solver found an integer solution within the integer tolerance."),
        Status::Infeasible => ("Solver could not find a feasible solution.", "Solver cannot find a point for which all Constraints are satisfied."),
        Status::Unbounded => ("The Objective Cell values do not converge.", "Solver can make the Objective Cell as large (or small) as it wants."),
        Status::NotLinear => (
            "The linearity conditions required by this LP Solver are not satisfied.",
            "Select the GRG Nonlinear engine in the Solver Parameters dialog to solve this model.",
        ),
        Status::Error => ("Solver encountered an error value in the Objective Cell or a Constraint cell.", "One of the cells in the model became an error value."),
    };
    (m.to_string(), d.to_string())
}

/// Sign so that the solvers always maximise.
fn sense(goal: Goal) -> f64 {
    if goal == Goal::Min { -1.0 } else { 1.0 }
}

// ---------------------------------------------------------------------------
// Linear models
// ---------------------------------------------------------------------------

fn solve_lp(p: &mut Problem, model: &SolverModel, x0: &[f64], ints: &[(usize, bool)]) -> (Status, Option<Vec<f64>>) {
    let n = x0.len();
    let Some((f0, g0)) = p.eval(x0) else { return (Status::Error, None) };
    let mut c = vec![0.0; n];
    let mut a = vec![vec![0.0; n]; g0.len()];
    for j in 0..n {
        let mut x = x0.to_vec();
        x[j] += 1.0;
        let Some((f, g)) = p.eval(&x) else { return (Status::Error, None) };
        c[j] = f - f0;
        for i in 0..g.len() {
            a[i][j] = g[i] - g0[i];
        }
    }
    // Linearity check at another point.
    let t: Vec<f64> = (0..n).map(|j| 0.37 + 0.61 * j as f64).collect();
    let xt: Vec<f64> = x0.iter().zip(&t).map(|(x, d)| x + d).collect();
    let Some((ft, gt)) = p.eval(&xt) else { return (Status::Error, None) };
    let pred = f0 + c.iter().zip(&t).map(|(c, d)| c * d).sum::<f64>();
    let close = |a: f64, b: f64| (a - b).abs() <= 1e-6 * (1.0 + a.abs().max(b.abs()));
    if !close(ft, pred) || gt.iter().enumerate().any(|(i, g)| !close(*g, g0[i] + a[i].iter().zip(&t).map(|(a, d)| a * d).sum::<f64>())) {
        return (Status::NotLinear, None);
    }
    // In absolute terms: f(x) = f0 + c.(x - x0); g_i(x) = g0_i + a_i.(x - x0).
    let mut lp_rows: Vec<(Vec<f64>, Rel, f64)> = Vec::new();
    let scalar_rows: Vec<&Row> = p.rows.iter().filter(|r| !r.rel.is_type()).collect();
    for (i, row) in scalar_rows.iter().enumerate() {
        let b = a[i].iter().zip(x0).map(|(a, x)| a * x).sum::<f64>() - g0[i];
        lp_rows.push((a[i].clone(), row.rel, b));
    }
    let mut obj = c.clone();
    if let Goal::Value(v) = model.goal {
        let b = c.iter().zip(x0).map(|(c, x)| c * x).sum::<f64>() - f0 + v;
        lp_rows.push((c.clone(), Rel::Eq, b));
        obj = vec![0.0; n];
    }
    let s = sense(model.goal);
    let obj: Vec<f64> = obj.iter().map(|v| v * s).collect();
    for &(k, bin) in ints {
        if bin {
            let mut e = vec![0.0; n];
            e[k] = 1.0;
            lp_rows.push((e, Rel::Le, 1.0));
        }
    }
    let int_vars: Vec<usize> = ints.iter().map(|(k, _)| *k).collect();
    match branch_and_bound(&obj, &lp_rows, model.nonneg, &int_vars) {
        Lp::Optimal(x, _) => (if int_vars.is_empty() { Status::Optimal } else { Status::IntegerOptimal }, Some(x)),
        Lp::Infeasible => (Status::Infeasible, None),
        Lp::Unbounded => (Status::Unbounded, None),
    }
}

#[derive(Clone, Debug)]
pub enum Lp {
    Optimal(Vec<f64>, f64),
    Infeasible,
    Unbounded,
}

/// Maximises `c.x` subject to `rows` (a.x rel b); x >= 0 when `nonneg`, otherwise free.
pub fn lp_max(c: &[f64], rows: &[(Vec<f64>, Rel, f64)], nonneg: bool) -> Lp {
    if nonneg {
        return simplex(c, rows);
    }
    // Free variables: x = x+ - x-.
    let n = c.len();
    let c2: Vec<f64> = c.iter().copied().chain(c.iter().map(|v| -v)).collect();
    let rows2: Vec<(Vec<f64>, Rel, f64)> = rows.iter().map(|(a, r, b)| (a.iter().copied().chain(a.iter().map(|v| -v)).collect(), *r, *b)).collect();
    match simplex(&c2, &rows2) {
        Lp::Optimal(x, v) => Lp::Optimal((0..n).map(|j| x[j] - x[n + j]).collect(), v),
        other => other,
    }
}

/// Branch and bound over `ints` on top of `lp_max`.
fn branch_and_bound(c: &[f64], rows: &[(Vec<f64>, Rel, f64)], nonneg: bool, ints: &[usize]) -> Lp {
    let root = lp_max(c, rows, nonneg);
    if ints.is_empty() {
        return root;
    }
    let mut best: Option<(Vec<f64>, f64)> = None;
    let mut stack: Vec<Vec<(Vec<f64>, Rel, f64)>> = vec![Vec::new()];
    let mut nodes = 0;
    let mut unbounded = false;
    while let Some(extra) = stack.pop() {
        nodes += 1;
        if nodes > 20_000 {
            break;
        }
        let all: Vec<(Vec<f64>, Rel, f64)> = rows.iter().cloned().chain(extra.iter().cloned()).collect();
        let (x, v) = match lp_max(c, &all, nonneg) {
            Lp::Optimal(x, v) => (x, v),
            Lp::Infeasible => continue,
            Lp::Unbounded => {
                unbounded = true;
                continue;
            }
        };
        if best.as_ref().is_some_and(|(_, bv)| v <= bv + 1e-9) {
            continue;
        }
        let frac = ints.iter().copied().find(|&k| (x[k] - x[k].round()).abs() > 1e-6);
        match frac {
            None => {
                let mut x = x;
                for &k in ints {
                    x[k] = x[k].round();
                }
                best = Some((x, v));
            }
            Some(k) => {
                let mut e = vec![0.0; c.len()];
                e[k] = 1.0;
                let mut lo = extra.clone();
                lo.push((e.clone(), Rel::Le, x[k].floor()));
                let mut hi = extra;
                hi.push((e, Rel::Ge, x[k].ceil()));
                // Explore the nearer branch first.
                if x[k] - x[k].floor() < 0.5 {
                    stack.push(hi);
                    stack.push(lo);
                } else {
                    stack.push(lo);
                    stack.push(hi);
                }
            }
        }
    }
    match best {
        Some((x, v)) => Lp::Optimal(x, v),
        None if unbounded => Lp::Unbounded,
        None => Lp::Infeasible,
    }
}

/// Two-phase dense simplex for max c.x, rows a.x (<=|=|>=) b, x >= 0 (Bland's rule).
fn simplex(c: &[f64], rows: &[(Vec<f64>, Rel, f64)]) -> Lp {
    const EPS: f64 = 1e-9;
    let n = c.len();
    let m = rows.len();
    // Normalise b >= 0.
    let rows: Vec<(Vec<f64>, Rel, f64)> = rows
        .iter()
        .map(|(a, r, b)| {
            if *b < 0.0 {
                let flipped = match r {
                    Rel::Le => Rel::Ge,
                    Rel::Ge => Rel::Le,
                    o => *o,
                };
                (a.iter().map(|v| -v).collect(), flipped, -b)
            } else {
                (a.clone(), *r, *b)
            }
        })
        .collect();
    let n_slack = rows.iter().filter(|(_, r, _)| matches!(r, Rel::Le | Rel::Ge)).count();
    let n_art = rows.iter().filter(|(_, r, _)| matches!(r, Rel::Ge | Rel::Eq)).count();
    let width = n + n_slack + n_art + 1;
    let rhs = width - 1;
    let mut t = vec![vec![0.0; width]; m];
    let mut basis = vec![0usize; m];
    let (mut s, mut art) = (n, n + n_slack);
    let mut arts = Vec::new();
    for (i, (a, r, b)) in rows.iter().enumerate() {
        t[i][..n].copy_from_slice(&a[..n]);
        t[i][rhs] = *b;
        match r {
            Rel::Le => {
                t[i][s] = 1.0;
                basis[i] = s;
                s += 1;
            }
            Rel::Ge => {
                t[i][s] = -1.0;
                s += 1;
                t[i][art] = 1.0;
                basis[i] = art;
                arts.push(art);
                art += 1;
            }
            _ => {
                t[i][art] = 1.0;
                basis[i] = art;
                arts.push(art);
                art += 1;
            }
        }
    }
    let pivot = |t: &mut Vec<Vec<f64>>, z: &mut Vec<f64>, basis: &mut Vec<usize>, pr: usize, pc: usize| {
        let pv = t[pr][pc];
        for v in t[pr].iter_mut() {
            *v /= pv;
        }
        let prow = t[pr].clone();
        for (i, row) in t.iter_mut().enumerate() {
            if i != pr && row[pc].abs() > 0.0 {
                let f = row[pc];
                for (v, p) in row.iter_mut().zip(&prow) {
                    *v -= f * p;
                }
            }
        }
        let f = z[pc];
        if f != 0.0 {
            for (v, p) in z.iter_mut().zip(&prow) {
                *v -= f * p;
            }
        }
        basis[pr] = pc;
    };
    // Runs the simplex on objective row z (reduced costs; maximise means pick z[j] < 0).
    let run = |t: &mut Vec<Vec<f64>>, z: &mut Vec<f64>, basis: &mut Vec<usize>, allowed: &dyn Fn(usize) -> bool| -> bool {
        for _ in 0..50_000 {
            let Some(pc) = (0..rhs).find(|&j| allowed(j) && z[j] < -EPS) else { return true };
            let mut best: Option<(usize, f64)> = None;
            for i in 0..t.len() {
                if t[i][pc] > EPS {
                    let ratio = t[i][rhs] / t[i][pc];
                    if best.is_none_or(|(bi, br)| ratio < br - EPS || ((ratio - br).abs() <= EPS && basis[i] < basis[bi])) {
                        best = Some((i, ratio));
                    }
                }
            }
            let Some((pr, _)) = best else { return false };
            pivot(t, z, basis, pr, pc);
        }
        true
    };
    // Phase 1: maximise -sum(artificials).
    if !arts.is_empty() {
        let mut z = vec![0.0; width];
        for &a in &arts {
            z[a] = 1.0;
        }
        for i in 0..m {
            if arts.contains(&basis[i]) {
                let row = t[i].clone();
                for (v, r) in z.iter_mut().zip(&row) {
                    *v -= r;
                }
            }
        }
        run(&mut t, &mut z, &mut basis, &|_| true);
        if -z[rhs] > 1e-7 * (1.0 + rows.iter().map(|r| r.2.abs()).fold(0.0, f64::max)) {
            return Lp::Infeasible;
        }
        // Drive remaining artificials out of the basis.
        for i in 0..m {
            if arts.contains(&basis[i]) {
                if let Some(pc) = (0..n + n_slack).find(|&j| t[i][j].abs() > EPS) {
                    let mut dummy = vec![0.0; width];
                    pivot(&mut t, &mut dummy, &mut basis, i, pc);
                }
            }
        }
    }
    // Phase 2.
    let mut z = vec![0.0; width];
    for j in 0..n {
        z[j] = -c[j];
    }
    for i in 0..m {
        let b = basis[i];
        if z[b] != 0.0 {
            let f = z[b];
            let row = t[i].clone();
            for (v, r) in z.iter_mut().zip(&row) {
                *v -= f * r;
            }
        }
    }
    let first_art = n + n_slack;
    if !run(&mut t, &mut z, &mut basis, &|j| j < first_art) {
        return Lp::Unbounded;
    }
    let mut x = vec![0.0; n];
    for i in 0..m {
        if basis[i] < n {
            x[basis[i]] = t[i][rhs];
        }
    }
    let v = c.iter().zip(&x).map(|(c, x)| c * x).sum();
    Lp::Optimal(x, v)
}

// ---------------------------------------------------------------------------
// Nonlinear models
// ---------------------------------------------------------------------------

fn solve_nonlinear(p: &mut Problem, model: &SolverModel, x0: &[f64], ints: &[(usize, bool)]) -> (Status, Option<Vec<f64>>) {
    let n = x0.len();
    let s = sense(model.goal);
    let rels: Vec<Rel> = p.rows.iter().filter(|r| !r.rel.is_type()).map(|r| r.rel).collect();
    let bins: Vec<usize> = ints.iter().filter(|(_, b)| *b).map(|(k, _)| *k).collect();
    let clamp = |x: &mut Vec<f64>| {
        for (k, v) in x.iter_mut().enumerate() {
            if model.nonneg && *v < 0.0 {
                *v = 0.0;
            }
            if bins.contains(&k) {
                *v = v.clamp(0.0, 1.0);
            }
        }
    };
    let Some((f0, _)) = p.eval(x0) else { return (Status::Error, None) };
    let scale = f0.abs().max(1.0);
    // Merit to minimise: -s*f + mu * violations^2 (+ (f - target)^2 for Value Of).
    let merit = |p: &mut Problem, x: &[f64], mu: f64| -> f64 {
        match p.eval(x) {
            Some((f, g)) => {
                let viol: f64 = g.iter().zip(&rels).map(|(g, r)| violation(*r, *g).powi(2)).sum();
                let base = match model.goal {
                    Goal::Value(v) => (f - v).powi(2) * mu,
                    _ => -s * f / scale,
                };
                base + mu * viol
            }
            None => f64::INFINITY,
        }
    };
    let mut x = x0.to_vec();
    clamp(&mut x);
    let mut mu = 10.0;
    let mut prev = f64::NAN;
    let mut stalled = 0;
    for _round in 0..8 {
        x = nelder_mead(p, &x, mu, &merit, &clamp, 400 + 100 * n);
        x = gradient_polish(p, &x, mu, &merit, &clamp, 30);
        let (f, viol) = match p.eval(&x) {
            Some((f, g)) => (f, g.iter().zip(&rels).map(|(g, r)| violation(*r, *g)).fold(0.0, f64::max)),
            None => return (Status::Error, None),
        };
        if viol <= FEAS_TOL && (f - prev).abs() <= 1e-7 * (1.0 + f.abs()) {
            stalled += 1;
            if stalled >= 2 {
                break;
            }
        }
        prev = f;
        mu *= 10.0;
        if p.evals > 20_000 {
            break;
        }
    }
    // Integer variables: round, then try neighbouring integers.
    let int_vars: Vec<usize> = ints.iter().map(|(k, _)| *k).collect();
    if !int_vars.is_empty() {
        for &k in &int_vars {
            x[k] = x[k].round();
        }
        let mut best = merit(p, &x, mu);
        let mut improved = true;
        let mut passes = 0;
        while improved && passes < 50 {
            improved = false;
            passes += 1;
            for &k in &int_vars {
                for d in [-1.0, 1.0] {
                    let mut y = x.clone();
                    y[k] += d;
                    clamp(&mut y);
                    let m = merit(p, &y, mu);
                    if m < best - 1e-12 {
                        best = m;
                        x = y;
                        improved = true;
                    }
                }
            }
        }
    }
    let Some((f, g)) = p.eval(&x) else { return (Status::Error, None) };
    let viol = g.iter().zip(&rels).map(|(g, r)| violation(*r, *g)).fold(0.0, f64::max);
    let target_ok = match model.goal {
        Goal::Value(v) => (f - v).abs() <= 1e-4 * (1.0 + v.abs()),
        _ => true,
    };
    if viol > 1e-4 * (1.0 + g.iter().map(|v| v.abs()).fold(0.0, f64::max)) || !target_ok {
        return (Status::Infeasible, Some(x));
    }
    if !f.is_finite() || f.abs() > 1e30 {
        return (Status::Unbounded, Some(x));
    }
    (if int_vars.is_empty() { Status::Optimal } else { Status::IntegerOptimal }, Some(x))
}

type Merit<'m> = dyn Fn(&mut Problem, &[f64], f64) -> f64 + 'm;

fn nelder_mead(p: &mut Problem, x0: &[f64], mu: f64, merit: &Merit, clamp: &dyn Fn(&mut Vec<f64>), max_evals: usize) -> Vec<f64> {
    let n = x0.len();
    let mut pts: Vec<Vec<f64>> = vec![x0.to_vec()];
    for j in 0..n {
        let mut x = x0.to_vec();
        x[j] += if x[j].abs() > 1e-3 { 0.1 * x[j].abs() } else { 0.5 };
        clamp(&mut x);
        if x == x0 {
            x[j] = x0[j] + 0.5;
        }
        pts.push(x);
    }
    let mut vals: Vec<f64> = pts.iter().map(|x| merit(p, x, mu)).collect();
    let mut evals = n + 1;
    while evals < max_evals {
        let mut idx: Vec<usize> = (0..=n).collect();
        idx.sort_by(|a, b| vals[*a].partial_cmp(&vals[*b]).unwrap_or(std::cmp::Ordering::Equal));
        pts = idx.iter().map(|i| pts[*i].clone()).collect();
        vals = idx.iter().map(|i| vals[*i]).collect();
        let spread = (vals[n] - vals[0]).abs();
        let size = pts.iter().skip(1).map(|x| x.iter().zip(&pts[0]).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max)).fold(0.0, f64::max);
        if spread <= 1e-12 * (1.0 + vals[0].abs()) && size <= 1e-9 * (1.0 + pts[0].iter().map(|v| v.abs()).fold(0.0, f64::max)) {
            break;
        }
        let centroid: Vec<f64> = (0..n).map(|j| pts[..n].iter().map(|x| x[j]).sum::<f64>() / n as f64).collect();
        let along = |t: f64| -> Vec<f64> {
            let mut x: Vec<f64> = (0..n).map(|j| centroid[j] + t * (pts[n][j] - centroid[j])).collect();
            clamp(&mut x);
            x
        };
        let xr = along(-1.0);
        let fr = merit(p, &xr, mu);
        evals += 1;
        if fr < vals[0] {
            let xe = along(-2.0);
            let fe = merit(p, &xe, mu);
            evals += 1;
            if fe < fr {
                pts[n] = xe;
                vals[n] = fe;
            } else {
                pts[n] = xr;
                vals[n] = fr;
            }
        } else if fr < vals[n - 1] {
            pts[n] = xr;
            vals[n] = fr;
        } else {
            let xc = if fr < vals[n] { along(-0.5) } else { along(0.5) };
            let fc = merit(p, &xc, mu);
            evals += 1;
            if fc < vals[n].min(fr) {
                pts[n] = xc;
                vals[n] = fc;
            } else {
                for i in 1..=n {
                    let mut x: Vec<f64> = (0..n).map(|j| pts[0][j] + 0.5 * (pts[i][j] - pts[0][j])).collect();
                    clamp(&mut x);
                    vals[i] = merit(p, &x, mu);
                    pts[i] = x;
                    evals += 1;
                }
            }
        }
    }
    let best = (0..=n).min_by(|a, b| vals[*a].partial_cmp(&vals[*b]).unwrap_or(std::cmp::Ordering::Equal)).unwrap_or(0);
    pts[best].clone()
}

/// A few projected gradient steps with backtracking (central differences).
fn gradient_polish(p: &mut Problem, x0: &[f64], mu: f64, merit: &Merit, clamp: &dyn Fn(&mut Vec<f64>), steps: usize) -> Vec<f64> {
    let n = x0.len();
    let mut x = x0.to_vec();
    let mut fx = merit(p, &x, mu);
    for _ in 0..steps {
        let mut grad = vec![0.0; n];
        for j in 0..n {
            let h = 1e-6 * (1.0 + x[j].abs());
            let mut a = x.clone();
            a[j] += h;
            let mut b = x.clone();
            b[j] -= h;
            grad[j] = (merit(p, &a, mu) - merit(p, &b, mu)) / (2.0 * h);
        }
        let norm = grad.iter().map(|g| g * g).sum::<f64>().sqrt();
        if !norm.is_finite() || norm < 1e-12 {
            break;
        }
        let mut t = 1.0 * (1.0 + x.iter().map(|v| v.abs()).fold(0.0, f64::max)) / norm;
        let mut moved = false;
        for _ in 0..40 {
            let mut y: Vec<f64> = x.iter().zip(&grad).map(|(x, g)| x - t * g).collect();
            clamp(&mut y);
            let fy = merit(p, &y, mu);
            if fy < fx {
                x = y;
                fx = fy;
                moved = true;
                break;
            }
            t *= 0.5;
        }
        if !moved {
            break;
        }
    }
    x
}

// ---------------------------------------------------------------------------
// Answer report
// ---------------------------------------------------------------------------

/// Rows of the Answer Report: (label column values) built after a solve with the solution in place.
pub fn answer_report(wb: &mut Workbook, si: usize, model: &SolverModel, out: &Outcome, name: &str) -> crate::model::Sheet {
    use crate::model::{Cell, Sheet, Style};
    let bold = wb.intern_style(Style { bold: true, ..Default::default() });
    let head = wb.intern_style(Style { bold: true, border: crate::model::Border { bottom: crate::model::BorderLine::Thin, ..Default::default() }, ..Default::default() });
    let mut sh = Sheet::new(name);
    sh.gridlines = false;
    for (c, w) in [(0u32, 20.0f32), (1, 72.0), (2, 120.0), (3, 100.0), (4, 100.0), (5, 90.0), (6, 70.0)] {
        sh.col_w.insert(c, w);
    }
    let put = |sh: &mut Sheet, r: u32, c: u32, v: Value, style: u32| sh.set(r, c, Some(Cell { value: v, formula: None, style }));
    let t = |s: &str| Value::Text(s.to_string());
    let sheet_name = wb.sheets[si].name.clone();
    let label = |wb: &Workbook, r: u32, c: u32| -> String {
        // Excel labels a cell with the nearest text to its left and above.
        let sh = &wb.sheets[si];
        let left = (0..c).rev().find_map(|cc| match sh.value(r, cc) {
            Value::Text(s) => Some(s.clone()),
            _ => None,
        });
        let up = (0..r).rev().find_map(|rr| match sh.value(rr, c) {
            Value::Text(s) => Some(s.clone()),
            _ => None,
        });
        match (left, up) {
            (Some(l), Some(u)) => format!("{l} {u}"),
            (Some(l), None) => l,
            (None, Some(u)) => u,
            _ => String::new(),
        }
    };
    put(&mut sh, 0, 0, t("Microsoft Excel Answer Report"), bold);
    put(&mut sh, 1, 0, t(&format!("Worksheet: [{}]{}", wb.file_name(), sheet_name)), 0);
    put(&mut sh, 2, 0, t(&format!("Report Created: {}", crate::numfmt::format_value(&Value::Number(crate::numfmt::now_serial()), "m/d/yyyy h:mm:ss AM/PM").text)), 0);
    put(&mut sh, 3, 0, t(&format!("Result: {}", out.message)), 0);
    put(&mut sh, 4, 0, t("Solver Engine"), 0);
    put(&mut sh, 5, 1, t(&format!("Engine: {}", model.method.label())), 0);
    put(&mut sh, 6, 1, t(&format!("Solution Time: {} Seconds.", "0.01")), 0);
    put(&mut sh, 7, 1, t(&format!("Iterations: {} Subproblems: 0", out.iterations)), 0);
    let mut r = 9;
    let goal = match model.goal {
        Goal::Max => "Max",
        Goal::Min => "Min",
        Goal::Value(_) => "Value Of",
    };
    put(&mut sh, r, 0, t(&format!("Objective Cell ({goal})")), bold);
    r += 1;
    for (c, h) in ["Cell", "Name", "Original Value", "Final Value"].iter().enumerate() {
        put(&mut sh, r, 1 + c as u32, t(h), head);
    }
    r += 1;
    if let Some((orow, ocol)) = model.objective {
        put(&mut sh, r, 1, t(&abs(Range::cell(orow, ocol))), 0);
        put(&mut sh, r, 2, t(&label(wb, orow, ocol)), 0);
        put(&mut sh, r, 3, Value::Number(out.original_objective), 0);
        put(&mut sh, r, 4, Value::Number(out.objective), 0);
        r += 1;
    }
    r += 1;
    put(&mut sh, r, 0, t("Variable Cells"), bold);
    r += 1;
    for (c, h) in ["Cell", "Name", "Original Value", "Final Value", "Integer"].iter().enumerate() {
        put(&mut sh, r, 1 + c as u32, t(h), head);
    }
    r += 1;
    let int_cells: Vec<(u32, u32)> =
        model.constraints.iter().filter(|c| matches!(c.rel, Rel::Int | Rel::Bin)).flat_map(|c| (c.lhs.r1..=c.lhs.r2).flat_map(move |rr| (c.lhs.c1..=c.lhs.c2).map(move |cc| (rr, cc)))).collect();
    for (k, &(vr, vc)) in out.cells.iter().enumerate() {
        put(&mut sh, r, 1, t(&abs(Range::cell(vr, vc))), 0);
        put(&mut sh, r, 2, t(&label(wb, vr, vc)), 0);
        put(&mut sh, r, 3, Value::Number(out.original[k]), 0);
        put(&mut sh, r, 4, Value::Number(out.x[k]), 0);
        put(&mut sh, r, 5, t(if int_cells.contains(&(vr, vc)) { "Integer" } else { "Contin" }), 0);
        r += 1;
    }
    r += 1;
    put(&mut sh, r, 0, t("Constraints"), bold);
    r += 1;
    for (c, h) in ["Cell", "Name", "Cell Value", "Formula", "Status", "Slack"].iter().enumerate() {
        put(&mut sh, r, 1 + c as u32, t(h), head);
    }
    r += 1;
    for con in &model.constraints {
        for rr in con.lhs.r1..=con.lhs.r2 {
            for cc in con.lhs.c1..=con.lhs.c2 {
                let v = number_at(wb, (si, rr, cc)).unwrap_or(0.0);
                put(&mut sh, r, 1, t(&abs(Range::cell(rr, cc))), 0);
                put(&mut sh, r, 2, t(&label(wb, rr, cc)), 0);
                put(&mut sh, r, 3, Value::Number(v), 0);
                let (formula_text, status, slack) = if con.rel.is_type() {
                    (format!("{}={}", abs(Range::cell(rr, cc)), match con.rel {
                        Rel::Bin => "binary",
                        Rel::Dif => "AllDifferent",
                        _ => "integer",
                    }), "Binding".to_string(), 0.0)
                } else {
                    let rhs = rhs_value(wb, si, con, rr - con.lhs.r1, cc - con.lhs.c1).unwrap_or(0.0);
                    let slack = (rhs - v).abs();
                    let st = if slack <= 1e-6 * (1.0 + rhs.abs()) { "Binding" } else { "Not Binding" };
                    (format!("{}{}{}", abs(Range::cell(rr, cc)), con.rel.symbol(), con.rhs), st.to_string(), if st == "Binding" { 0.0 } else { slack })
                };
                put(&mut sh, r, 4, t(&formula_text), 0);
                put(&mut sh, r, 5, t(&status), 0);
                put(&mut sh, r, 6, Value::Number(slack), 0);
                r += 1;
            }
        }
    }
    sh
}

fn rhs_value(wb: &Workbook, si: usize, con: &Constraint, dr: u32, dc: u32) -> Option<f64> {
    let text = con.rhs.trim().trim_start_matches('=');
    if let Ok(v) = text.parse::<f64>() {
        return Some(v);
    }
    if let Some((s, rg)) = crate::whatif::parse_range_ref(wb, text) {
        let (r, c) = if rg.is_single() { (rg.r1, rg.c1) } else { (rg.r1 + dr, rg.c1 + dc) };
        return number_at(wb, (s, r, c));
    }
    formula::parse(text).ok().and_then(|e| formula::eval_expr(wb, si, 0, 0, &e).as_number())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::cell_from_input;

    fn put(wb: &mut Workbook, a1: &str, text: &str) {
        let (r, c) = crate::model::parse_cell(a1).unwrap();
        let cell = cell_from_input(wb, text, 0);
        wb.sheets[0].set(r, c, cell);
    }

    fn num(wb: &Workbook, a1: &str) -> f64 {
        let (r, c) = crate::model::parse_cell(a1).unwrap();
        number_at(wb, (0, r, c)).unwrap()
    }

    #[test]
    fn simplex_core() {
        // max 3x + 5y; x <= 4; 2y <= 12; 3x + 2y <= 18 -> (2, 6), 36
        let rows = vec![(vec![1.0, 0.0], Rel::Le, 4.0), (vec![0.0, 2.0], Rel::Le, 12.0), (vec![3.0, 2.0], Rel::Le, 18.0)];
        match lp_max(&[3.0, 5.0], &rows, true) {
            Lp::Optimal(x, v) => {
                assert!((x[0] - 2.0).abs() < 1e-9 && (x[1] - 6.0).abs() < 1e-9, "{x:?}");
                assert!((v - 36.0).abs() < 1e-9);
            }
            o => panic!("{o:?}"),
        }
        // >= and = rows (minimise cost): min 2x + 3y; x + y >= 10; x - y = 2 -> (6, 4) cost 24
        let rows = vec![(vec![1.0, 1.0], Rel::Ge, 10.0), (vec![1.0, -1.0], Rel::Eq, 2.0)];
        match lp_max(&[-2.0, -3.0], &rows, true) {
            Lp::Optimal(x, _) => assert!((x[0] - 6.0).abs() < 1e-9 && (x[1] - 4.0).abs() < 1e-9, "{x:?}"),
            o => panic!("{o:?}"),
        }
        assert!(matches!(lp_max(&[1.0], &[(vec![1.0], Rel::Ge, 1.0)], true), Lp::Unbounded));
        assert!(matches!(lp_max(&[1.0], &[(vec![1.0], Rel::Le, 1.0), (vec![1.0], Rel::Ge, 2.0)], true), Lp::Infeasible));
        // Free variable: max -x with x >= -5 (unconstrained sign) -> -5.
        match lp_max(&[-1.0], &[(vec![1.0], Rel::Ge, -5.0)], false) {
            Lp::Optimal(x, _) => assert!((x[0] + 5.0).abs() < 1e-9, "{x:?}"),
            o => panic!("{o:?}"),
        }
    }

    /// The classic product-mix LP built on a worksheet and solved with Simplex LP.
    #[test]
    fn solver_on_a_known_lp() {
        let mut wb = Workbook::new();
        put(&mut wb, "B1", "0"); // x
        put(&mut wb, "B2", "0"); // y
        put(&mut wb, "B4", "=3*B1+5*B2"); // profit
        put(&mut wb, "B5", "=B1");
        put(&mut wb, "B6", "=2*B2");
        put(&mut wb, "B7", "=3*B1+2*B2");
        put(&mut wb, "C7", "18");
        wb.recalc();
        let model = SolverModel {
            objective: Some((3, 1)),
            goal: Goal::Max,
            vars: vec![Range::parse("B1:B2").unwrap()],
            constraints: vec![
                Constraint { lhs: Range::parse("B5").unwrap(), rel: Rel::Le, rhs: "4".into() },
                Constraint { lhs: Range::parse("B6").unwrap(), rel: Rel::Le, rhs: "12".into() },
                Constraint { lhs: Range::parse("B7").unwrap(), rel: Rel::Le, rhs: "$C$7".into() },
            ],
            nonneg: true,
            method: Method::Simplex,
        };
        let out = solve(&mut wb, 0, &model).unwrap();
        assert_eq!(out.status, Status::Optimal, "{}", out.message);
        assert!((num(&wb, "B1") - 2.0).abs() < 1e-6 && (num(&wb, "B2") - 6.0).abs() < 1e-6);
        assert!((num(&wb, "B4") - 36.0).abs() < 1e-6);
        // The same model with GRG Nonlinear reaches the same optimum.
        put(&mut wb, "B1", "0");
        put(&mut wb, "B2", "0");
        wb.recalc();
        let out = solve(&mut wb, 0, &SolverModel { method: Method::Grg, ..model.clone() }).unwrap();
        assert!(out.status.found(), "{}", out.message);
        assert!((num(&wb, "B4") - 36.0).abs() < 1e-2, "{}", num(&wb, "B4"));
        // Integer constraint: max 5x + 4y; 6x + 4y <= 24; x + 2y <= 6; x, y int -> 20 (x=4, y=0) or (x=2,y=2:18)...
        let mut wb = Workbook::new();
        put(&mut wb, "A1", "0");
        put(&mut wb, "A2", "0");
        put(&mut wb, "A3", "=5*A1+4*A2");
        put(&mut wb, "A4", "=6*A1+4*A2");
        put(&mut wb, "A5", "=A1+2*A2");
        wb.recalc();
        let m = SolverModel {
            objective: Some((2, 0)),
            goal: Goal::Max,
            vars: vec![Range::parse("A1:A2").unwrap()],
            constraints: vec![
                Constraint { lhs: Range::parse("A4").unwrap(), rel: Rel::Le, rhs: "24".into() },
                Constraint { lhs: Range::parse("A5").unwrap(), rel: Rel::Le, rhs: "6".into() },
                Constraint { lhs: Range::parse("A1:A2").unwrap(), rel: Rel::Int, rhs: String::new() },
            ],
            nonneg: true,
            method: Method::Simplex,
        };
        let out = solve(&mut wb, 0, &m).unwrap();
        assert_eq!(out.status, Status::IntegerOptimal);
        assert!((num(&wb, "A3") - 20.0).abs() < 1e-6, "{}", num(&wb, "A3"));
        // A nonlinear objective is refused by Simplex LP.
        put(&mut wb, "A3", "=A1*A2");
        wb.recalc();
        let out = solve(&mut wb, 0, &m).unwrap();
        assert_eq!(out.status, Status::NotLinear);
        // Names round trip.
        let names = model.to_names("Sheet1");
        assert_eq!(SolverModel::from_names(&names), Some(model));
    }

    #[test]
    fn nonlinear_minimum() {
        // min (x-3)^2 + (y+1)^2 + 2 with y >= 0 -> x = 3, y = 0, f = 3
        let mut wb = Workbook::new();
        put(&mut wb, "A1", "0");
        put(&mut wb, "A2", "5");
        put(&mut wb, "A3", "=(A1-3)^2+(A2+1)^2+2");
        wb.recalc();
        let m = SolverModel {
            objective: Some((2, 0)),
            goal: Goal::Min,
            vars: vec![Range::parse("A1:A2").unwrap()],
            constraints: vec![],
            nonneg: true,
            method: Method::Grg,
        };
        let out = solve(&mut wb, 0, &m).unwrap();
        assert!(out.status.found(), "{}", out.message);
        assert!((num(&wb, "A3") - 3.0).abs() < 1e-3, "{}", num(&wb, "A3"));
        assert!((num(&wb, "A1") - 3.0).abs() < 1e-2);
    }
}
