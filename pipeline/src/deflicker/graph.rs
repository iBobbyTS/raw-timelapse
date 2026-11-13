use super::{
    stats::{finite, median, percentile},
    Result,
};
use std::collections::BTreeMap;
#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub i: usize,
    pub j: usize,
    pub delta_ev: f64,
    pub weight: f64,
    pub support: usize,
    pub consensus_fraction: f64,
    pub spatial_mad_ev: f64,
    pub quadrants: usize,
    pub lag: usize,
    pub rejects: BTreeMap<String, usize>,
}
impl GraphEdge {
    pub fn reliable(&self) -> bool {
        self.support >= 24
            && self.consensus_fraction >= 0.6
            && self.spatial_mad_ev <= 0.06
            && self.quadrants >= 3
    }
}
#[derive(Debug, Clone)]
pub struct LinearSolveRecord {
    pub component: usize,
    pub irls_round: usize,
    pub iterations: usize,
    pub relative_residual: f64,
    pub converged: bool,
}
#[derive(Debug, Clone)]
pub struct GraphResult {
    pub brightness: Vec<f64>,
    pub component: Vec<isize>,
    pub edge_residuals: Vec<f64>,
    pub edge_p95_abs_residual_ev: Option<f64>,
    pub components: Vec<Vec<usize>>,
    pub converged: bool,
    pub iterations: usize,
    pub linear_solves: Vec<LinearSolveRecord>,
}
/// Deterministic Jacobi-preconditioned CG on gauge-eliminated SPD normal equations.
/// Matrix-vector products use only edges. Relative L2 residual <= 1e-12;
/// at most 4*N+64 iterations. Every inner solve is recorded, including failures.
pub fn solve_exposure_graph(n: usize, edges: &[GraphEdge]) -> Result<GraphResult> {
    solve_graph_with(n, edges, 0.02, 8)
}
pub fn solve_graph_with(
    n: usize,
    edges: &[GraphEdge],
    delta: f64,
    max_iterations: usize,
) -> Result<GraphResult> {
    if !delta.is_finite() || delta <= 0. || max_iterations == 0 {
        return Err("invalid graph solver configuration".into());
    }
    for e in edges {
        if e.i >= n
            || e.j >= n
            || e.i == e.j
            || !e.delta_ev.is_finite()
            || !e.weight.is_finite()
            || e.weight <= 0.
        {
            return Err("invalid graph edge".into());
        }
    }
    let mut r = GraphResult {
        brightness: vec![f64::NAN; n],
        component: vec![-1; n],
        edge_residuals: vec![],
        edge_p95_abs_residual_ev: None,
        components: vec![],
        converged: true,
        iterations: 0,
        linear_solves: vec![],
    };
    let mut adj = vec![vec![]; n];
    for e in edges {
        adj[e.i].push(e.j);
        adj[e.j].push(e.i);
    }
    for start in 0..n {
        if r.component[start] >= 0 {
            continue;
        }
        let id = r.components.len();
        let mut stack = vec![start];
        let mut comp = vec![];
        r.component[start] = id as isize;
        while let Some(t) = stack.pop() {
            comp.push(t);
            for &u in &adj[t] {
                if r.component[u] < 0 {
                    r.component[u] = id as isize;
                    stack.push(u);
                }
            }
        }
        comp.sort_unstable();
        r.components.push(comp);
    }
    let norm = median(&edges.iter().map(|e| e.weight).collect::<Vec<_>>()).max(1e-12);
    for (id, comp) in r.components.iter().enumerate() {
        if comp.len() < 2 {
            continue;
        }
        let gauge = comp[0];
        let free = &comp[1..];
        let mut index = vec![usize::MAX; n];
        for (i, &t) in free.iter().enumerate() {
            index[t] = i;
        }
        let es: Vec<_> = edges
            .iter()
            .filter(|e| r.component[e.i] == id as isize)
            .collect();
        let mut robust = vec![1.; es.len()];
        let mut previous: Option<Vec<f64>> = None;
        for round in 1..=max_iterations {
            r.iterations = r.iterations.max(round);
            let mut diag = vec![0.; free.len()];
            let mut rhs = vec![0.; free.len()];
            let weights: Vec<_> = es
                .iter()
                .zip(&robust)
                .map(|(e, h)| e.weight / norm * h)
                .collect();
            for (e, &w) in es.iter().zip(&weights) {
                for (t, sgn) in [(e.i, -1.), (e.j, 1.)] {
                    if t != gauge {
                        let p = index[t];
                        diag[p] += w;
                        rhs[p] += sgn * w * e.delta_ev;
                    }
                }
            }
            let mul = |x: &[f64]| {
                let mut out = vec![0.; free.len()];
                for (e, &w) in es.iter().zip(&weights) {
                    let a = if e.i == gauge { 0. } else { x[index[e.i]] };
                    let b = if e.j == gauge { 0. } else { x[index[e.j]] };
                    let v = w * (b - a);
                    if e.i != gauge {
                        out[index[e.i]] -= v;
                    }
                    if e.j != gauge {
                        out[index[e.j]] += v;
                    }
                }
                out
            };
            let mut x = previous.clone().unwrap_or_else(|| vec![0.; free.len()]);
            let ax = mul(&x);
            let mut residual: Vec<_> = rhs.iter().zip(ax).map(|(b, a)| b - a).collect();
            let rhsnorm = dot(&rhs, &rhs).sqrt().max(1e-30);
            let mut z: Vec<_> = residual.iter().zip(&diag).map(|(v, d)| v / d).collect();
            let mut p = z.clone();
            let mut rz = dot(&residual, &z);
            let mut it = 0;
            let cap = free.len().saturating_mul(4).saturating_add(64);
            while dot(&residual, &residual).sqrt() > 1e-12 * rhsnorm && it < cap {
                let ap = mul(&p);
                let denom = dot(&p, &ap);
                if !denom.is_finite() || denom <= 0. {
                    break;
                }
                let alpha = rz / denom;
                for k in 0..x.len() {
                    x[k] += alpha * p[k];
                    residual[k] -= alpha * ap[k];
                }
                z = residual.iter().zip(&diag).map(|(v, d)| v / d).collect();
                let next = dot(&residual, &z);
                let beta = next / rz;
                rz = next;
                for k in 0..p.len() {
                    p[k] = z[k] + beta * p[k];
                }
                it += 1;
            }
            // Recompute the true residual; recursive residuals may drift.
            let ax = mul(&x);
            let rel = rhs
                .iter()
                .zip(ax)
                .map(|(b, a)| (b - a).powi(2))
                .sum::<f64>()
                .sqrt()
                / rhsnorm;
            let converged = x.iter().all(|v| v.is_finite()) && rel <= 1e-10;
            r.linear_solves.push(LinearSolveRecord {
                component: id,
                irls_round: round,
                iterations: it,
                relative_residual: rel,
                converged,
            });
            r.converged &= converged;
            r.brightness[gauge] = 0.;
            for (&t, &v) in free.iter().zip(&x) {
                r.brightness[t] = v;
            }
            if !converged {
                break;
            }
            if previous
                .as_ref()
                .is_some_and(|v| v.iter().zip(&x).all(|(a, b)| (a - b).abs() < 1e-10))
            {
                break;
            }
            previous = Some(x);
            for (k, e) in es.iter().enumerate() {
                let residual = (r.brightness[e.j] - r.brightness[e.i] - e.delta_ev).abs();
                robust[k] = (delta / residual.max(1e-10)).min(1.);
            }
        }
    }
    r.edge_residuals = edges
        .iter()
        .map(|e| r.brightness[e.j] - r.brightness[e.i] - e.delta_ev)
        .collect();
    r.edge_p95_abs_residual_ev = finite(percentile(
        &r.edge_residuals.iter().map(|v| v.abs()).collect::<Vec<_>>(),
        95.,
    ));
    Ok(r)
}
fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
