use super::{
    graph::GraphEdge,
    measurement::compatible,
    stats::{mad, median, percentile},
    FrameMeasurement, Result,
};
use std::collections::{BTreeMap, BTreeSet};
pub const DEFAULT_LAGS: [usize; 4] = [1, 2, 4, 8];
pub(crate) fn quadrants(ids: impl Iterator<Item = usize>, nx: usize, ny: usize) -> usize {
    ids.map(|i| (i % nx >= nx / 2, i / nx >= ny / 2))
        .collect::<BTreeSet<_>>()
        .len()
}
pub fn pair_difference(
    a: &FrameMeasurement,
    b: &FrameMeasurement,
    lag: usize,
    train: Option<&[bool]>,
) -> Result<GraphEdge> {
    a.validate()?;
    b.validate()?;
    let k = a.valid.len();
    if a.centers != b.centers
        || a.grid_nx != b.grid_nx
        || a.grid_ny != b.grid_ny
        || train.is_some_and(|t| t.len() != k)
    {
        return Err("pair shape mismatch".into());
    }
    let mut e = GraphEdge {
        i: 0,
        j: 0,
        delta_ev: f64::NAN,
        weight: 0.,
        support: 0,
        consensus_fraction: 0.,
        spatial_mad_ev: f64::INFINITY,
        quadrants: 0,
        lag,
        rejects: BTreeMap::new(),
    };
    let d: Vec<_> = (0..k)
        .map(|i| b.log_luminance[i] - a.log_luminance[i])
        .collect();
    let usable: Vec<_> = (0..k)
        .map(|i| {
            a.valid[i]
                && b.valid[i]
                && d[i].is_finite()
                && a.log_luminance[i] > 0.002_f64.log2()
                && b.log_luminance[i] > 0.002_f64.log2()
                && a.saturation_fraction[i].max(b.saturation_fraction[i]) < 0.01
        })
        .collect();
    if !usable.iter().any(|v| *v) {
        e.rejects.insert(
            "no_shared_valid_tiles".into(),
            (0..k).filter(|&i| a.valid[i] && b.valid[i]).count(),
        );
        return Ok(e);
    }
    let mut good = usable.clone();
    if let (Some(sa), Some(sb)) = (&a.log_pixel_samples, &b.log_pixel_samples) {
        let mut rejected = 0;
        for i in 0..k {
            let diff: Vec<_> = sa[i].iter().zip(&sb[i]).map(|(x, y)| y - x).collect();
            let shape = mad(&diff, median(&diff));
            let ok = shape < 0.0401;
            if usable[i] && !ok {
                rejected += 1;
            }
            good[i] &= ok;
        }
        if rejected > 0 {
            e.rejects.insert("structure_mad".into(), rejected);
        }
    }
    if let (Some(ca), Some(cb)) = (&a.log_channel_means, &b.log_channel_means) {
        let mut rejected = 0;
        for i in 0..k {
            let cs: Vec<_> = (0..3)
                .map(|c| cb[i][c] - ca[i][c])
                .filter(|v| v.is_finite())
                .collect();
            let ok = !cs.is_empty()
                && cs.iter().copied().fold(f64::NEG_INFINITY, f64::max)
                    - cs.iter().copied().fold(f64::INFINITY, f64::min)
                    <= 0.0801;
            if usable[i] && !ok {
                rejected += 1;
            }
            good[i] &= ok;
        }
        if rejected > 0 {
            e.rejects.insert("colour_change".into(), rejected);
        }
    }
    if let Some(train) = train {
        let mut rejected = 0;
        for i in 0..k {
            if good[i] && !train[i] {
                rejected += 1;
            }
            good[i] &= train[i];
        }
        if rejected > 0 {
            e.rejects
                .insert("outside_reference_support".into(), rejected);
        }
    }
    e.support = good.iter().filter(|v| **v).count();
    if e.support < 24.max(k.div_ceil(4)) {
        e.rejects.insert("insufficient_support".into(), e.support);
        return Ok(e);
    }
    let values: Vec<_> = (0..k).filter(|&i| good[i]).map(|i| d[i]).collect();
    let mut med = median(&values);
    let scale = mad(&values, med);
    for _ in 0..4 {
        let mut total = 0.;
        let mut sum = 0.;
        for &v in &values {
            let w = (0.03 / (v - med).abs().max(1e-8)).min(1.);
            total += w;
            sum += w * v;
        }
        med = sum / total;
    }
    let inliers: Vec<_> = (0..k)
        .filter(|&i| good[i] && (d[i] - med).abs() <= 0.06)
        .collect();
    e.delta_ev = med;
    e.spatial_mad_ev = scale;
    e.consensus_fraction = inliers.len() as f64 / e.support as f64;
    e.quadrants = quadrants(inliers.into_iter(), a.grid_nx, a.grid_ny);
    e.weight = (e.support as f64 / 200.).min(1.) / (0.01_f64.powi(2) + scale.powi(2));
    Ok(e)
}
#[derive(Debug, Clone, Default)]
pub struct EdgeStats {
    pub candidate_edges: usize,
    pub rejected_edges: usize,
    pub reject_reasons: BTreeMap<String, usize>,
    pub by_lag: BTreeMap<usize, (usize, usize)>,
}
pub fn build_edges(
    ms: &[FrameMeasurement],
    boundaries: &[usize],
    train: Option<&[bool]>,
    lags: &[usize],
) -> Result<(Vec<GraphEdge>, EdgeStats)> {
    compatible(ms)?;
    let n = ms.len();
    if lags.contains(&0) {
        return Err("lag must be positive".into());
    }
    let mut active = lags.to_vec();
    if n >= 32 && !active.contains(&16) {
        active.push(16);
        active.sort_unstable();
    }
    let mut stats = EdgeStats::default();
    for &l in &active {
        stats.by_lag.insert(l, (0, 0));
    }
    let mut edges = vec![];
    for lag in active {
        for j in lag..n {
            let i = j - lag;
            if boundaries.iter().any(|&b| i < b && b <= j) {
                continue;
            }
            stats.candidate_edges += 1;
            let mut e = pair_difference(&ms[i], &ms[j], lag, train)?;
            if !e.delta_ev.is_finite() || !e.reliable() {
                stats.rejected_edges += 1;
                if let Some(v) = stats.by_lag.get_mut(&lag) {
                    v.1 += 1;
                }
                for reason in e.rejects.keys() {
                    *stats.reject_reasons.entry(reason.clone()).or_default() += 1;
                }
            } else {
                e.i = i;
                e.j = j;
                edges.push(e);
                if let Some(v) = stats.by_lag.get_mut(&lag) {
                    v.0 += 1;
                }
            }
        }
    }
    Ok((edges, stats))
}
pub fn persistent_background(ms: &[FrameMeasurement], brightness: &[f64]) -> Result<Vec<bool>> {
    persistent_background_with(ms, brightness, 0.8, 0.1)
}
pub fn persistent_background_with(
    ms: &[FrameMeasurement],
    brightness: &[f64],
    min_fraction: f64,
    max_spread: f64,
) -> Result<Vec<bool>> {
    let k = compatible(ms)?;
    let n = ms.len();
    if brightness.len() != n
        || !min_fraction.is_finite()
        || !(0. ..=1.).contains(&min_fraction)
        || !max_spread.is_finite()
        || max_spread < 0.
    {
        return Err("invalid persistent background inputs".into());
    }
    let mut out = vec![false; k];
    for tile in 0..k {
        if ms.iter().filter(|m| m.valid[tile]).count() as f64 / (n as f64) < min_fraction {
            continue;
        }
        let offsets: Vec<_> = ms
            .iter()
            .zip(brightness)
            .filter(|(m, b)| m.valid[tile] && b.is_finite() && m.log_luminance[tile].is_finite())
            .map(|(m, b)| m.log_luminance[tile] - b)
            .collect();
        if offsets.len() < 8.max(n / 2) {
            continue;
        }
        let c = median(&offsets);
        out[tile] = percentile(
            &offsets.iter().map(|v| (v - c).abs()).collect::<Vec<_>>(),
            90.,
        ) <= max_spread;
    }
    Ok(out)
}

pub(super) fn reference_tile_valid(m: &FrameMeasurement, i: usize) -> bool {
    m.valid[i] && m.saturation_fraction[i] < 0.01 && m.log_luminance[i] > 0.002_f64.log2()
}

/// Local shared-tile evidence for component placement and independent plan
/// assessment. This deliberately measures luminance change, not exposure cause:
/// spatial coherence is required, but fine structure/chroma need not remain
/// fixed over the entire sequence. It never establishes persistent background.
#[derive(Debug, Clone)]
pub struct LocalCommonChange {
    pub delta_ev: Option<f64>,
    pub support: usize,
    pub consensus_fraction: f64,
    pub spatial_mad_ev: Option<f64>,
    pub quadrants: usize,
}
pub fn local_common_change(
    a: &FrameMeasurement,
    b: &FrameMeasurement,
    train: Option<&[bool]>,
) -> Result<LocalCommonChange> {
    a.validate()?;
    b.validate()?;
    if a.centers != b.centers || a.grid_nx != b.grid_nx || a.grid_ny != b.grid_ny {
        return Err("local reference shape mismatch".into());
    }
    let k = a.valid.len();
    if train.is_some_and(|q| q.len() != k) {
        return Err("local reference mask shape".into());
    }
    let ids: Vec<_> = (0..k)
        .filter(|&i| {
            reference_tile_valid(a, i) && reference_tile_valid(b, i) && train.is_none_or(|q| q[i])
        })
        .collect();
    let d: Vec<_> = ids
        .iter()
        .map(|&i| b.log_luminance[i] - a.log_luminance[i])
        .collect();
    let center = median(&d);
    let spread = mad(&d, center);
    let inliers: Vec<_> = ids
        .iter()
        .zip(&d)
        .filter(|(_, d)| (**d - center).abs() <= 0.06)
        .map(|(i, _)| *i)
        .collect();
    let fraction = if ids.is_empty() {
        0.
    } else {
        inliers.len() as f64 / ids.len() as f64
    };
    let quadrants = quadrants(inliers.into_iter(), a.grid_nx, a.grid_ny);
    Ok(LocalCommonChange {
        delta_ev: (ids.len() >= 24.max(k.div_ceil(4))
            && fraction >= 0.6
            && spread <= 0.06
            && quadrants >= 3)
            .then_some(center)
            .filter(|v| v.is_finite()),
        support: ids.len(),
        consensus_fraction: fraction,
        spatial_mad_ev: spread.is_finite().then_some(spread),
        quadrants,
    })
}
