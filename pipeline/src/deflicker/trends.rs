use super::Result;
#[derive(Debug, Clone)]
pub struct TrendFit {
    pub trend: Vec<f64>,
    pub converged: bool,
    pub iterations: usize,
}
pub fn lambda_for_period(period: usize) -> Result<f64> {
    if period < 6 {
        return Err("period must be integer >=6".into());
    }
    Ok(1. / (16. * (std::f64::consts::PI / period as f64).sin().powi(4)))
}
pub fn difference_norm_diagonals(n: usize) -> Result<(Vec<f64>, Vec<f64>, Vec<f64>)> {
    if n < 3 {
        return Err("second differences require N>=3".into());
    }
    let mut a = vec![0.; n];
    let mut b = vec![0.; n - 1];
    for i in 0..n - 2 {
        a[i] += 1.;
        a[i + 1] += 4.;
        a[i + 2] += 1.;
        b[i] -= 2.;
        b[i + 1] -= 2.;
    }
    Ok((a, b, vec![1.; n - 2]))
}
/// Bandwidth-two Cholesky of W + lambda D2' D2, O(N) storage and work.
pub fn solve_weighted(values: &[f64], weights: &[f64], lambda: f64) -> Result<Vec<f64>> {
    let n = values.len();
    if weights.len() != n
        || !lambda.is_finite()
        || lambda < 0.
        || weights.iter().any(|w| !w.is_finite() || *w < 0.)
    {
        return Err("invalid weighted trend system".into());
    }
    let (a, b, c) = difference_norm_diagonals(n)?;
    let mut diag = vec![0.; n];
    let mut l1 = vec![0.; n];
    let mut l2 = vec![0.; n];
    let mut rhs = vec![0.; n];
    for i in 0..n {
        if i >= 2 {
            l2[i] = lambda * c[i - 2] / diag[i - 2];
        }
        if i >= 1 {
            l1[i] = (lambda * b[i - 1] - if i >= 2 { l2[i] * l1[i - 1] } else { 0. }) / diag[i - 1];
        }
        let pivot = weights[i] + lambda * a[i] - l1[i] * l1[i] - l2[i] * l2[i];
        if !pivot.is_finite() || pivot <= 0. {
            return Err("trend system not positive definite".into());
        }
        diag[i] = pivot.sqrt();
        // Missing values are zeroed BEFORE multiplying by weight.
        rhs[i] = weights[i] * if values[i].is_finite() { values[i] } else { 0. };
    }
    for i in 0..n {
        rhs[i] = (rhs[i]
            - if i >= 1 { l1[i] * rhs[i - 1] } else { 0. }
            - if i >= 2 { l2[i] * rhs[i - 2] } else { 0. })
            / diag[i];
    }
    for i in (0..n).rev() {
        rhs[i] = (rhs[i]
            - if i + 1 < n {
                l1[i + 1] * rhs[i + 1]
            } else {
                0.
            }
            - if i + 2 < n {
                l2[i + 2] * rhs[i + 2]
            } else {
                0.
            })
            / diag[i];
    }
    if rhs.iter().any(|v| !v.is_finite()) {
        return Err("nonfinite trend solution".into());
    }
    Ok(rhs)
}
pub fn huber_whittaker(values: &[f64], valid: &[bool], period: usize) -> Result<TrendFit> {
    huber_whittaker_with(values, valid, period, 0.1, 100, 1e-5)
}
pub fn huber_whittaker_with(
    y: &[f64],
    q: &[bool],
    period: usize,
    delta: f64,
    max_iter: usize,
    tol: f64,
) -> Result<TrendFit> {
    if y.len() != q.len()
        || period < 6
        || !delta.is_finite()
        || delta <= 0.
        || !tol.is_finite()
        || tol < 0.
        || max_iter == 0
    {
        return Err("invalid trend configuration or dimensions".into());
    }
    if q.iter().filter(|v| **v).count() < 8 || y.iter().zip(q).any(|(v, q)| *q && !v.is_finite()) {
        return Ok(TrendFit {
            trend: vec![f64::NAN; y.len()],
            converged: false,
            iterations: 0,
        });
    }
    let lam = lambda_for_period(period.min(y.len()))?;
    let weights: Vec<_> = q.iter().map(|v| if *v { 1. } else { 0. }).collect();
    let mut z = solve_weighted(y, &weights, lam)?;
    for iteration in 1..=max_iter {
        let w: Vec<_> = y
            .iter()
            .zip(q)
            .zip(&z)
            .map(|((v, q), z)| {
                if *q {
                    (delta / (v - z).abs().max(1e-12)).min(1.)
                } else {
                    0.
                }
            })
            .collect();
        let new = solve_weighted(y, &w, lam)?;
        let change = new
            .iter()
            .zip(&z)
            .map(|(a, b)| (a - b).abs())
            .fold(0., f64::max);
        z = new;
        if change <= tol {
            return Ok(TrendFit {
                trend: z,
                converged: true,
                iterations: iteration,
            });
        }
    }
    Ok(TrendFit {
        trend: z,
        converged: false,
        iterations: max_iter,
    })
}
