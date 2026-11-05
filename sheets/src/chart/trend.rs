//! Trendline regression (least squares) and error bar amounts.

use super::TrendKind;

/// A fitted trendline: y = f(x).
#[derive(Clone, Debug, PartialEq)]
pub struct Fit {
    pub kind: TrendKind,
    /// Linear: [b, m]; polynomial: [c0, c1, ..]; exponential: [a, b] (a*e^(bx));
    /// logarithmic: [b, a] (a*ln(x) + b); power: [a, b] (a*x^b).
    pub coef: Vec<f64>,
    pub r2: f64,
}

impl Fit {
    pub fn eval(&self, x: f64) -> f64 {
        let c = &self.coef;
        match self.kind {
            TrendKind::Linear | TrendKind::Polynomial => c.iter().rev().fold(0.0, |acc, k| acc * x + k),
            TrendKind::Exponential => c[0] * (c[1] * x).exp(),
            TrendKind::Logarithmic => c[1] * x.ln() + c[0],
            TrendKind::Power => c[0] * x.powf(c[1]),
            TrendKind::MovingAverage => f64::NAN,
        }
    }

    /// The trendline equation as Excel writes it on the chart ("y = 2.5x + 3.6667").
    pub fn equation(&self) -> String {
        let c = &self.coef;
        match self.kind {
            TrendKind::Linear | TrendKind::Polynomial => {
                let mut s = String::from("y =");
                let mut first = true;
                for p in (0..c.len()).rev() {
                    let k = c[p];
                    if k == 0.0 && !(p == 0 && first) {
                        continue;
                    }
                    let mag = num(k.abs());
                    let var = match p {
                        0 => String::new(),
                        1 => "x".into(),
                        _ => format!("x{}", superscript(p)),
                    };
                    let body = if p > 0 && mag == "1" { var } else { format!("{mag}{var}") };
                    if first {
                        s.push_str(&format!(" {}{body}", if k < 0.0 { "-" } else { "" }));
                    } else {
                        s.push_str(&format!(" {} {body}", if k < 0.0 { "-" } else { "+" }));
                    }
                    first = false;
                }
                s
            }
            TrendKind::Exponential => format!("y = {}e^{}x", num(c[0]), num(c[1])),
            TrendKind::Logarithmic => {
                let sign = if c[0] < 0.0 { "-" } else { "+" };
                format!("y = {}ln(x) {sign} {}", num(c[1]), num(c[0].abs()))
            }
            TrendKind::Power => format!("y = {}x^{}", num(c[0]), num(c[1])),
            TrendKind::MovingAverage => String::new(),
        }
    }
}

fn superscript(p: usize) -> String {
    const SUP: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
    p.to_string().chars().map(|d| SUP[d.to_digit(10).unwrap_or(0) as usize]).collect()
}

/// Coefficients shown with up to four decimals, like the default trendline label.
pub fn num(v: f64) -> String {
    if v != 0.0 && v.abs() < 1e-4 {
        return format!("{v:.4E}");
    }
    let s = format!("{v:.4}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s == "-0" { "0".into() } else { s }
}

/// Solves the normal equations of a polynomial least-squares fit of degree `deg`.
fn polyfit(xs: &[f64], ys: &[f64], deg: usize, intercept: Option<f64>) -> Option<Vec<f64>> {
    let n = deg + 1;
    let first = usize::from(intercept.is_some());
    let m = n - first;
    if m == 0 {
        return Some(vec![intercept.unwrap_or(0.0)]);
    }
    let mut a = vec![vec![0.0; m + 1]; m];
    for (&x, &y) in xs.iter().zip(ys) {
        let y = y - intercept.unwrap_or(0.0);
        let pw: Vec<f64> = (0..n).map(|p| x.powi(p as i32)).collect();
        for i in 0..m {
            for j in 0..m {
                a[i][j] += pw[i + first] * pw[j + first];
            }
            a[i][m] += pw[i + first] * y;
        }
    }
    // Gaussian elimination with partial pivoting.
    for col in 0..m {
        let piv = (col..m).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[piv][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, piv);
        for r in 0..m {
            if r != col {
                let f = a[r][col] / a[col][col];
                for k in col..=m {
                    a[r][k] -= f * a[col][k];
                }
            }
        }
    }
    let mut c: Vec<f64> = intercept.into_iter().collect();
    c.extend((0..m).map(|i| a[i][m] / a[i][i]));
    Some(c)
}

fn r_squared(ys: &[f64], fitted: impl Fn(usize) -> f64) -> f64 {
    let mean = ys.iter().sum::<f64>() / ys.len() as f64;
    let ss_tot: f64 = ys.iter().map(|y| (y - mean).powi(2)).sum();
    let ss_res: f64 = ys.iter().enumerate().map(|(i, y)| (y - fitted(i)).powi(2)).sum();
    if ss_tot == 0.0 { 1.0 } else { 1.0 - ss_res / ss_tot }
}

/// Least-squares fit of the given points. Exponential and power fits ignore
/// points with y <= 0, logarithmic and power fits points with x <= 0.
pub fn fit(kind: TrendKind, order: u8, xs: &[f64], ys: &[f64], intercept: Option<f64>) -> Option<Fit> {
    let pts: Vec<(f64, f64)> = xs
        .iter()
        .zip(ys)
        .map(|(x, y)| (*x, *y))
        .filter(|(x, y)| {
            x.is_finite()
                && y.is_finite()
                && match kind {
                    TrendKind::Exponential => *y > 0.0,
                    TrendKind::Logarithmic => *x > 0.0,
                    TrendKind::Power => *x > 0.0 && *y > 0.0,
                    _ => true,
                }
        })
        .collect();
    if pts.len() < 2 {
        return None;
    }
    let (x, y): (Vec<f64>, Vec<f64>) = pts.into_iter().unzip();
    let ln = |v: &[f64]| v.iter().map(|a| a.ln()).collect::<Vec<f64>>();
    let (coef, r2) = match kind {
        TrendKind::Linear | TrendKind::Polynomial => {
            let deg = if kind == TrendKind::Linear { 1 } else { (order as usize).clamp(2, 6).min(x.len() - 1) };
            let c = polyfit(&x, &y, deg, intercept)?;
            let f = Fit { kind, coef: c.clone(), r2: 0.0 };
            let r2 = r_squared(&y, |i| f.eval(x[i]));
            (c, r2)
        }
        TrendKind::Exponential => {
            let ly = ln(&y);
            let c = polyfit(&x, &ly, 1, intercept.filter(|v| *v > 0.0).map(f64::ln))?;
            let r2 = r_squared(&ly, |i| c[0] + c[1] * x[i]);
            (vec![c[0].exp(), c[1]], r2)
        }
        TrendKind::Logarithmic => {
            let lx = ln(&x);
            let c = polyfit(&lx, &y, 1, None)?;
            let r2 = r_squared(&y, |i| c[0] + c[1] * lx[i]);
            (c, r2)
        }
        TrendKind::Power => {
            let (lx, ly) = (ln(&x), ln(&y));
            let c = polyfit(&lx, &ly, 1, None)?;
            let r2 = r_squared(&ly, |i| c[0] + c[1] * lx[i]);
            (vec![c[0].exp(), c[1]], r2)
        }
        TrendKind::MovingAverage => return None,
    };
    coef.iter().all(|c| c.is_finite()).then_some(Fit { kind, coef, r2 })
}

/// Moving average over `period` points; `None` until enough points have been seen.
pub fn moving_average(ys: &[Option<f64>], period: usize) -> Vec<Option<f64>> {
    let period = period.max(2);
    (0..ys.len())
        .map(|i| {
            if i + 1 < period {
                return None;
            }
            let w: Vec<f64> = ys[i + 1 - period..=i].iter().flatten().copied().collect();
            (!w.is_empty()).then(|| w.iter().sum::<f64>() / w.len() as f64)
        })
        .collect()
}

/// Sample standard deviation and standard error of a series.
pub fn std_dev_err(vals: &[f64]) -> (f64, f64) {
    let n = vals.len() as f64;
    if n < 2.0 {
        return (0.0, 0.0);
    }
    let mean = vals.iter().sum::<f64>() / n;
    let ss: f64 = vals.iter().map(|v| (v - mean).powi(2)).sum();
    ((ss / (n - 1.0)).sqrt(), (ss / (n * (n - 1.0))).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_and_polynomial() {
        let xs = [1.0, 2.0, 3.0, 4.0];
        let ys = [3.0, 5.0, 7.0, 9.0];
        let f = fit(TrendKind::Linear, 2, &xs, &ys, None).unwrap();
        assert!((f.coef[1] - 2.0).abs() < 1e-9 && (f.coef[0] - 1.0).abs() < 1e-9);
        assert!((f.r2 - 1.0).abs() < 1e-12);
        assert_eq!(f.equation(), "y = 2x + 1");
        let ys2: Vec<f64> = xs.iter().map(|x| 2.0 * x * x - 3.0 * x + 0.5).collect();
        let p = fit(TrendKind::Polynomial, 2, &xs, &ys2, None).unwrap();
        assert!((p.coef[2] - 2.0).abs() < 1e-9 && (p.coef[1] + 3.0).abs() < 1e-9);
        assert_eq!(p.equation(), "y = 2x² - 3x + 0.5");
        // Fixed intercept.
        let f = fit(TrendKind::Linear, 2, &xs, &ys, Some(0.0)).unwrap();
        assert_eq!(f.coef[0], 0.0);
        assert!((f.coef[1] - 70.0 / 30.0).abs() < 1e-9);
    }

    #[test]
    fn exponential_log_power() {
        let xs = [1.0, 2.0, 3.0, 4.0];
        let ys: Vec<f64> = xs.iter().map(|x: &f64| 2.0 * (0.5 * x).exp()).collect();
        let f = fit(TrendKind::Exponential, 2, &xs, &ys, None).unwrap();
        assert!((f.coef[0] - 2.0).abs() < 1e-9 && (f.coef[1] - 0.5).abs() < 1e-9);
        assert!((f.eval(5.0) - 2.0 * 2.5f64.exp()).abs() < 1e-6);
        let ys: Vec<f64> = xs.iter().map(|x: &f64| 3.0 * x.ln() + 1.0).collect();
        let f = fit(TrendKind::Logarithmic, 2, &xs, &ys, None).unwrap();
        assert!((f.coef[1] - 3.0).abs() < 1e-9 && (f.coef[0] - 1.0).abs() < 1e-9);
        assert_eq!(f.equation(), "y = 3ln(x) + 1");
        let ys: Vec<f64> = xs.iter().map(|x: &f64| 1.5 * x.powf(2.0)).collect();
        let f = fit(TrendKind::Power, 2, &xs, &ys, None).unwrap();
        assert!((f.coef[0] - 1.5).abs() < 1e-9 && (f.coef[1] - 2.0).abs() < 1e-9);
        assert_eq!(moving_average(&[Some(1.0), Some(3.0), None, Some(5.0)], 2), vec![None, Some(2.0), Some(3.0), Some(5.0)]);
        let (sd, se) = std_dev_err(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]);
        assert!((sd - 2.138089935).abs() < 1e-6 && (se - sd / 8f64.sqrt()).abs() < 1e-9);
    }
}
