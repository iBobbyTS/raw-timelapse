use super::{
    graph::solve_exposure_graph,
    measurement::compatible,
    pairwise::{build_edges, persistent_background, DEFAULT_LAGS},
    stats::{finite, interpolate, mad, median, need},
    trends::huber_whittaker,
    FrameMeasurement, Result,
};
use crate::json::Json;
#[derive(Debug, Clone)]
pub struct ExposureStep {
    pub start_frame: usize,
    pub delta_ev: f64,
}
#[derive(Debug, Clone)]
pub struct ExposureConfig {
    pub period: usize,
    pub strength: f64,
    pub steps: Vec<ExposureStep>,
    pub brightness: String,
    pub ev_limits: Option<(f64, f64)>,
    pub time_gaps: Vec<usize>,
    pub estimator: String,
    /// Exclusions are [x0,y0,x1,y1], unlike the sidecar's [x,y,w,h] ROI.
    pub exclude_rois: Vec<[f64; 4]>,
    pub scene_cut_median_ev: f64,
    pub scene_cut_tile_mad_ev: f64,
}
impl Default for ExposureConfig {
    fn default() -> Self {
        Self {
            period: 60,
            strength: 1.,
            steps: vec![],
            brightness: "smooth".into(),
            ev_limits: None,
            time_gaps: vec![],
            estimator: "pairwise".into(),
            exclude_rois: vec![],
            scene_cut_median_ev: 0.20,
            scene_cut_tile_mad_ev: 0.08,
        }
    }
}
#[derive(Debug, Clone)]
pub struct FrameEstimate {
    pub index: usize,
    pub status: String,
    pub common_residual_ev: Option<f64>,
    pub requested_ev: Option<f64>,
    pub applied_ev: Option<f64>,
    pub cumulative_confirmed_step_ev: f64,
    pub valid_tile_count: usize,
    pub consensus_fraction: Option<f64>,
    pub residual_mad_ev: Option<f64>,
    pub warnings: Vec<String>,
    pub render_target_error_ev: Option<f64>,
    pub new_output_clipping_fraction: Option<f64>,
    pub render_ev: Option<f64>,
    pub measured_level_ev: Option<f64>,
    pub desired_level_ev: Option<f64>,
    pub segment_id: usize,
}
impl FrameEstimate {
    pub fn gain(&self) -> Option<f64> {
        self.applied_ev.and_then(|e| finite(e.exp2()))
    }
}
#[derive(Debug, Clone)]
pub struct SceneCut {
    pub index: usize,
    pub median_ev: f64,
    pub tile_mad_ev: f64,
    pub common_tiles: usize,
}
#[derive(Debug, Clone)]
pub struct ExposureResult {
    pub frames: Vec<FrameEstimate>,
    pub tile_log_luminance: Vec<Vec<f64>>,
    pub tile_valid: Vec<Vec<bool>>,
    pub consensus_mask: Vec<Vec<bool>>,
    pub frozen_target: Vec<Vec<f64>>,
    pub trusted_indices: Vec<bool>,
    pub trend_unconverged_tiles: usize,
    pub period_effective: usize,
    pub scene_cuts: Vec<SceneCut>,
    pub time_gaps: Vec<usize>,
    pub pairwise_summary: Json,
}
pub fn step_vector(n: usize, steps: &[ExposureStep]) -> Result<Vec<f64>> {
    let mut s = vec![0.; n];
    for step in steps {
        if step.start_frame >= n || !step.delta_ev.is_finite() {
            return Err("invalid confirmed step".into());
        }
        s[step.start_frame] += step.delta_ev;
    }
    let mut total = 0.;
    for v in &mut s {
        total += *v;
        if !total.is_finite() {
            return Err("nonfinite cumulative step".into());
        }
        *v = total;
    }
    Ok(s)
}
pub fn detect_scene_cuts(a: &[Vec<f64>], q: &[Vec<bool>]) -> Result<Vec<SceneCut>> {
    detect_scene_cuts_with(a, q, 0.20, 0.08)
}
pub fn detect_scene_cuts_with(
    a: &[Vec<f64>],
    q: &[Vec<bool>],
    median_ev: f64,
    tile_mad_ev: f64,
) -> Result<Vec<SceneCut>> {
    let k = a.first().map_or(0, Vec::len);
    if a.len() != q.len()
        || a.iter().any(|r| r.len() != k)
        || q.iter().any(|r| r.len() != k)
        || !median_ev.is_finite()
        || !tile_mad_ev.is_finite()
        || median_ev < 0.
        || tile_mad_ev < 0.
    {
        return Err("invalid scene cut inputs".into());
    }
    let mut cuts = vec![];
    for t in 0..a.len().saturating_sub(1) {
        let d: Vec<_> = (0..k)
            .filter(|&i| q[t][i] && q[t + 1][i])
            .map(|i| a[t + 1][i] - a[t][i])
            .collect();
        if d.len() < need(k) {
            continue;
        }
        if d.iter().any(|v| !v.is_finite()) {
            return Err("nonfinite valid photometry".into());
        }
        let med = median(&d);
        let spread = mad(&d, med);
        if med.abs() >= median_ev && spread >= tile_mad_ev {
            cuts.push(SceneCut {
                index: t + 1,
                median_ev: med,
                tile_mad_ev: spread,
                common_tiles: d.len(),
            });
        }
    }
    Ok(cuts)
}
pub(crate) fn limits(limits: Option<(f64, f64)>) -> Result<(f64, f64)> {
    match limits {
        None => Ok((f64::NEG_INFINITY, f64::INFINITY)),
        Some((a, b)) if (a + b).is_finite() && a < b => Ok((a, b)),
        _ => Err("invalid EV limits".into()),
    }
}
struct State {
    consensus: Vec<Vec<bool>>,
    residual: Vec<f64>,
    mad: Vec<f64>,
    fractions: Vec<f64>,
    counts: Vec<usize>,
    reliable: Vec<bool>,
    interpolated: Vec<bool>,
    labels: Vec<String>,
    measured: Vec<f64>,
    desired: Vec<f64>,
}
impl State {
    fn new(n: usize, k: usize) -> Self {
        Self {
            consensus: vec![vec![false; k]; n],
            residual: vec![f64::NAN; n],
            mad: vec![f64::NAN; n],
            fractions: vec![f64::NAN; n],
            counts: vec![0; n],
            reliable: vec![false; n],
            interpolated: vec![false; n],
            labels: vec!["REVIEW_REQUIRED".into(); n],
            measured: vec![f64::NAN; n],
            desired: vec![f64::NAN; n],
        }
    }
}
fn row_level(m: &FrameMeasurement) -> f64 {
    median(
        &m.log_luminance
            .iter()
            .zip(&m.valid)
            .filter(|(_, q)| **q)
            .map(|(x, _)| *x)
            .collect::<Vec<_>>(),
    )
}
fn edge_const(comp: &[usize], start: bool, ms: &[FrameMeasurement], b: &[f64]) -> f64 {
    let member: Vec<_> = comp.iter().copied().filter(|&t| b[t].is_finite()).collect();
    let sel = if start {
        &member[..member.len().min(3)]
    } else {
        &member[member.len().saturating_sub(3)..]
    };
    median(
        &sel.iter()
            .map(|&t| row_level(&ms[t]) - b[t])
            .collect::<Vec<_>>(),
    )
}
fn pairwise_path(
    ms: &[FrameMeasurement],
    s: &[f64],
    cfg: &ExposureConfig,
    st: &mut State,
) -> Result<(Vec<f64>, Json)> {
    let n = ms.len();
    let k = ms[0].valid.len();
    let mut train = vec![true; k];
    for (i, c) in ms[0].centers.iter().enumerate() {
        for r in &cfg.exclude_rois {
            if c[0] >= r[0] && c[0] < r[2] && c[1] >= r[1] && c[1] < r[3] {
                train[i] = false;
            }
        }
    }
    let (edges, stats) = build_edges(ms, &[0], Some(&train), &DEFAULT_LAGS)?;
    let graph = solve_exposure_graph(n, &edges)?;
    let b = &graph.brightness;
    let persistent = persistent_background(ms, b)?;
    let mut bh = b.clone();
    let mut anchors = vec![];
    if graph.components.len() > 1 {
        let mut prev: Option<f64> = None;
        for comp in &graph.components {
            let start = edge_const(comp, true, ms, b);
            if let Some(p) = prev.filter(|v| v.is_finite()) {
                if start.is_finite() {
                    let shift = start - p;
                    for &t in comp {
                        bh[t] += shift;
                    }
                    anchors.push(Json::obj(vec![
                        ("boundary", Json::Int(comp[0] as i64)),
                        ("local_const_before", Json::Float(p)),
                        ("local_const_after", Json::Float(start)),
                        ("applied_shift_ev", Json::Float(-shift)),
                    ]));
                } else {
                    anchors.push(Json::obj(vec![("boundary",Json::Int(comp[0] as i64)),("note",Json::Str("unverifiable island interrupts level chaining; the following component keeps its graph-gauge placement".into()))]));
                }
            }
            prev = Some(edge_const(comp, false, ms, b));
        }
    }
    for (t, m) in ms.iter().enumerate() {
        st.counts[t] = (0..k).filter(|&i| m.valid[i] && persistent[i]).count();
    }
    let mut requested = vec![f64::NAN; n];
    let uniform = cfg.brightness == "uniform";
    let target = median(&bh);
    if uniform {
        let finite_all: Vec<_> = (0..n).filter(|&t| bh[t].is_finite()).collect();
        for &t in &finite_all {
            st.reliable[t] = true;
            for i in 0..k {
                st.consensus[t][i] = ms[t].valid[i] && persistent[i];
            }
            st.residual[t] = bh[t] - target;
            requested[t] = cfg.strength * (target - bh[t]);
            st.labels[t] = "UNIFORM".into();
            st.measured[t] = bh[t];
            st.desired[t] = target;
        }
        if target.is_finite() {
            let global = median(
                &finite_all
                    .iter()
                    .map(|&t| row_level(&ms[t]) - bh[t])
                    .collect::<Vec<_>>(),
            );
            if global.is_finite() {
                for comp in &graph.components {
                    if comp.len() != 1 {
                        continue;
                    }
                    let t = comp[0];
                    if bh[t].is_finite() || !ms[t].valid.iter().any(|q| *q) {
                        continue;
                    }
                    let level = row_level(&ms[t]) - global;
                    if !level.is_finite() {
                        continue;
                    }
                    requested[t] = cfg.strength * (target - level);
                    st.interpolated[t] = true;
                    st.labels[t] = "ABSOLUTE_LEVEL".into();
                    st.consensus[t] = ms[t].valid.clone();
                    st.measured[t] = level;
                    st.desired[t] = target;
                }
            }
        }
    } else {
        let badj: Vec<_> = b.iter().zip(s).map(|(b, s)| b - s).collect();
        let mut trend = vec![f64::NAN; n];
        for comp in &graph.components {
            if comp.len() < 8 {
                continue;
            }
            let vals: Vec<_> = comp.iter().map(|&t| badj[t]).collect();
            let fit = huber_whittaker(
                &vals,
                &vals.iter().map(|v| v.is_finite()).collect::<Vec<_>>(),
                cfg.period,
            )?;
            if fit.converged {
                for (&t, &v) in comp.iter().zip(&fit.trend) {
                    trend[t] = v;
                }
            }
        }
        for t in 0..n {
            if badj[t].is_finite() && trend[t].is_finite() {
                st.reliable[t] = true;
                for i in 0..k {
                    st.consensus[t][i] = ms[t].valid[i] && persistent[i];
                }
                st.residual[t] = badj[t] - trend[t];
                requested[t] = cfg.strength * (trend[t] - badj[t]);
                st.labels[t] = "OK".into();
                st.measured[t] = badj[t];
                st.desired[t] = trend[t];
            }
        }
        for comp in &graph.components {
            let anchor = -median(&comp.iter().map(|&t| requested[t]).collect::<Vec<_>>());
            if anchor.is_finite() {
                for &t in comp {
                    if requested[t].is_finite() {
                        requested[t] += anchor;
                    }
                }
            }
        }
    }
    let good: Vec<_> = (0..n).filter(|&t| requested[t].is_finite()).collect();
    for ab in good.windows(2) {
        let (a, b) = (ab[0], ab[1]);
        let missing = b - a - 1;
        if missing >= 1
            && missing <= if uniform { 3 } else { 7 }
            && (a + 1..b).all(|t| ms[t].valid.iter().filter(|q| **q).count() >= need(k))
        {
            for t in a + 1..b {
                requested[t] = interpolate(t, a, b, requested[a], requested[b]);
                st.interpolated[t] = true;
                st.labels[t] = "INTERPOLATED_ESTIMATE".into();
            }
        }
    }
    let mut summary=Json::obj(vec![
        ("target_provenance",Json::obj(vec![("intent",Json::Str(cfg.brightness.clone())),("reference_scope",Json::Str(if uniform{"global"}else{"trend"}.into())),("defined_before_iteration",Json::Bool(true)),("measured",Json::Str(if uniform{"graph brightness b on persistent matched background"}else{"b minus confirmed steps"}.into())),("desired",Json::Str(if uniform{"constant level (global median of the stitched graph brightness)".into()}else{format!("Huber-Whittaker trend, period={} (lambda=1/(16 sin(pi/P)^4))",cfg.period)}))])),
        ("cross_boundary_anchors",Json::obj(vec![("method",Json::Str("whole-frame all-valid-tile median level const across the boundary (owner whole-sequence intent: perceived overall brightness stays continuous); explicit assumption, not evidence".into())),("entries",Json::Array(anchors))])),
        ("edges",Json::Int(edges.len() as i64)),("candidate_edges",Json::Int(stats.candidate_edges as i64)),("rejected_edges",Json::Int(stats.rejected_edges as i64)),
        ("edges_by_lag",Json::Object(stats.by_lag.iter().map(|(l,(a,b))|(l.to_string(),Json::obj(vec![("kept",Json::Int(*a as i64)),("rejected",Json::Int(*b as i64))]))).collect())),
        ("edge_reject_reasons",Json::Object(stats.reject_reasons.iter().map(|(k,v)|(k.clone(),Json::Int(*v as i64))).collect())),
        ("edge_p95_abs_residual_ev",graph.edge_p95_abs_residual_ev.map_or(Json::Null,Json::Float)),
        ("components",Json::Array(graph.components.iter().map(|c|Json::obj(vec![("frames",Json::Int(c.len() as i64)),("gauge",Json::Int(c[0] as i64))])).collect())),
        ("persistent_tiles",Json::Int(persistent.iter().filter(|q|**q).count() as i64)),("excluded_tiles",Json::Int(train.iter().filter(|q|!**q).count() as i64)),("converged",Json::Bool(graph.converged)),("iterations",Json::Int(graph.iterations as i64)),
        ("linear_solver",Json::Str("deterministic Jacobi PCG; gauge-eliminated SPD; relative residual target 1e-12, verified <=1e-10".into())),
        ("linear_solves",Json::Array(graph.linear_solves.iter().map(|s|Json::obj(vec![("component",Json::Int(s.component as i64)),("irls_round",Json::Int(s.irls_round as i64)),("iterations",Json::Int(s.iterations as i64)),("relative_residual",Json::Float(s.relative_residual)),("converged",Json::Bool(s.converged))])).collect()))]);
    if uniform {
        if let Json::Object(ref mut es) = summary {
            es.push((
                "uniform_target_level_ev".into(),
                finite(target).map_or(Json::Null, Json::Float),
            ));
        }
    }
    Ok((requested, summary))
}
fn robust_segment(
    a: &[Vec<f64>],
    q: &[Vec<bool>],
    centers: &[[f64; 2]],
    period: usize,
    st: &mut State,
) -> Result<usize> {
    let n = a.len();
    let k = centers.len();
    let mut trend = vec![vec![f64::NAN; k]; n];
    let mut failed = 0;
    for col in 0..k {
        let y: Vec<_> = a.iter().map(|r| r[col]).collect();
        let valid: Vec<_> = q.iter().map(|r| r[col]).collect();
        let fit = huber_whittaker(&y, &valid, period)?;
        if fit.converged {
            for t in 0..n {
                trend[t][col] = fit.trend[t];
            }
        } else if valid.iter().filter(|v| **v).count() >= 8 {
            failed += 1;
        }
    }
    for t in 0..n {
        let ids: Vec<_> = (0..k)
            .filter(|&i| q[t][i] && (a[t][i] - trend[t][i]).is_finite())
            .collect();
        st.counts[t] = ids.len();
        if ids.len() < need(k) {
            continue;
        }
        let r: Vec<_> = ids.iter().map(|&i| a[t][i] - trend[t][i]).collect();
        let med = median(&r);
        let spread = mad(&r, med);
        let agree: Vec<_> = ids
            .iter()
            .zip(&r)
            .filter(|(_, v)| (**v - med).abs() <= 0.06)
            .map(|(&i, _)| i)
            .collect();
        let fraction = agree.len() as f64 / ids.len() as f64;
        let quadrants = agree
            .iter()
            .map(|&i| (centers[i][0] >= 0.5, centers[i][1] >= 0.5))
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        st.residual[t] = med;
        st.mad[t] = spread;
        st.fractions[t] = fraction;
        if spread <= 0.06 && fraction >= 0.60 && quadrants >= 3 {
            st.reliable[t] = true;
            st.labels[t] = "OK".into();
            for i in agree {
                st.consensus[t][i] = true;
            }
        } else {
            st.residual[t] = f64::NAN;
        }
    }
    let good: Vec<_> = (0..n).filter(|&t| st.reliable[t]).collect();
    for ab in good.windows(2) {
        let (a, b) = (ab[0], ab[1]);
        if (1..=3).contains(&(b - a - 1)) {
            for t in a + 1..b {
                st.residual[t] = interpolate(t, a, b, st.residual[a], st.residual[b]);
                st.interpolated[t] = true;
                st.labels[t] = "INTERPOLATED_ESTIMATE".into();
            }
        }
    }
    Ok(failed)
}
pub fn estimate_exposure(ms: &[FrameMeasurement], cfg: &ExposureConfig) -> Result<ExposureResult> {
    let k = compatible(ms)?;
    let n = ms.len();
    if k < 8 {
        return Err("insufficient tile grid".into());
    }
    if cfg.period < 6 || !cfg.strength.is_finite() || !(0. ..=1.).contains(&cfg.strength) {
        return Err("invalid period or strength".into());
    }
    if !["smooth", "uniform"].contains(&cfg.brightness.as_str()) {
        return Err("brightness must be smooth or uniform".into());
    }
    if !["pairwise", "legacy"].contains(&cfg.estimator.as_str()) {
        return Err("estimator must be pairwise or legacy".into());
    }
    for r in &cfg.exclude_rois {
        if r.iter().any(|v| !v.is_finite())
            || r[0] < 0.
            || r[1] < 0.
            || r[2] > 1.
            || r[3] > 1.
            || r[0] >= r[2]
            || r[1] >= r[3]
        {
            return Err("invalid exclusion ROI".into());
        }
    }
    let (low, high) = limits(cfg.ev_limits)?;
    let s = step_vector(n, &cfg.steps)?;
    let x: Vec<_> = ms.iter().map(|m| m.log_luminance.clone()).collect();
    let q: Vec<_> = ms.iter().map(|m| m.valid.clone()).collect();
    let mut st = State::new(n, k);
    let mut cuts = vec![];
    let mut summary = Json::obj(vec![]);
    let mut failed = 0;
    let mut pairwise_requested = None;
    if cfg.strength == 0. || (cfg.brightness == "smooth" && n < 8) {
        st.residual.fill(0.);
        st.reliable.fill(true);
        st.labels.fill(
            if n < 8 && cfg.strength != 0. {
                "SHORT_SEQUENCE_NO_DEFLICKER"
            } else {
                "DISABLED"
            }
            .into(),
        );
    } else {
        let a: Vec<Vec<_>> = x
            .iter()
            .zip(&s)
            .map(|(r, s)| r.iter().map(|x| x - s).collect())
            .collect();
        cuts = detect_scene_cuts_with(&a, &q, cfg.scene_cut_median_ev, cfg.scene_cut_tile_mad_ev)?;
        if cfg.estimator == "pairwise" {
            let (req, sum) = pairwise_path(ms, &s, cfg, &mut st)?;
            pairwise_requested = Some(req);
            summary = sum;
        } else if cfg.brightness == "uniform" {
            for t in 0..n {
                st.counts[t] = q[t].iter().filter(|v| **v).count();
                if st.counts[t] >= need(k) {
                    st.residual[t] = median(
                        &a[t]
                            .iter()
                            .zip(&q[t])
                            .filter(|(_, q)| **q)
                            .map(|(x, _)| *x)
                            .collect::<Vec<_>>(),
                    );
                    st.reliable[t] = true;
                    st.consensus[t] = q[t].clone();
                    st.labels[t] = "UNIFORM".into();
                }
            }
        } else {
            failed = robust_segment(&a, &q, &ms[0].centers, cfg.period, &mut st)?;
        }
    }
    let accepted: Vec<_> = (0..n)
        .map(|t| st.reliable[t] || st.interpolated[t])
        .collect();
    let requested = if let Some(r) = pairwise_requested {
        r
    } else {
        let raw: Vec<_> = (0..n)
            .map(|t| {
                if cfg.strength == 0. {
                    0.
                } else if accepted[t] {
                    cfg.strength * (-s[t] - st.residual[t])
                } else {
                    f64::NAN
                }
            })
            .collect();
        let anchor = if st.reliable.iter().any(|q| *q) {
            -median(
                &raw.iter()
                    .zip(&st.reliable)
                    .filter(|(_, q)| **q)
                    .map(|(v, _)| *v)
                    .collect::<Vec<_>>(),
            )
        } else {
            0.
        };
        raw.iter().map(|v| v + anchor).collect()
    };
    let mut frozen = vec![vec![f64::NAN; k]; n];
    let mut frames = vec![];
    for t in 0..n {
        let val = finite(requested[t]);
        let applied = val.map(|v| v.clamp(low, high));
        let mut status = st.labels[t].clone();
        let mut warnings = vec![];
        if cfg.brightness == "smooth"
            && cfg.strength != 0.
            && n < cfg.period.saturating_mul(2)
            && (n < 8 || st.labels[t] != "SHORT_SEQUENCE_NO_DEFLICKER")
        {
            warnings.push("SHORT_CONTEXT".into());
        }
        if failed > 0 {
            warnings.push(format!("TREND_NONCONVERGED_TILES={failed}"));
        }
        if applied.is_some() && val != applied {
            status = "LIMITED".into();
            warnings.push("EXPOSURE_LIMITED".into());
        } else if !accepted[t] {
            status = "REVIEW_REQUIRED".into();
        }
        if st.interpolated[t] {
            warnings.push(
                if st.labels[t] == "ABSOLUTE_LEVEL" {
                    "LONE_FRAME_ABSOLUTE_LEVEL"
                } else {
                    "INTERPOLATED_NOT_VERIFIED"
                }
                .into(),
            );
        }
        if let Some(v) = val {
            for i in 0..k {
                if q[t][i] {
                    frozen[t][i] = x[t][i] + v;
                }
            }
        }
        frames.push(FrameEstimate {
            index: t,
            status,
            common_residual_ev: finite(st.residual[t]),
            requested_ev: val,
            applied_ev: applied,
            cumulative_confirmed_step_ev: s[t],
            valid_tile_count: if n >= 8 && cfg.strength != 0. {
                st.counts[t]
            } else {
                q[t].iter().filter(|q| **q).count()
            },
            consensus_fraction: finite(st.fractions[t]),
            residual_mad_ev: finite(st.mad[t]),
            warnings,
            render_target_error_ev: None,
            new_output_clipping_fraction: None,
            render_ev: None,
            measured_level_ev: finite(st.measured[t]),
            desired_level_ev: finite(st.desired[t]),
            segment_id: 0,
        });
    }
    let mut gaps: Vec<_> = cfg
        .time_gaps
        .iter()
        .copied()
        .filter(|g| *g > 0 && *g < n)
        .collect();
    gaps.sort_unstable();
    gaps.dedup();
    Ok(ExposureResult {
        frames,
        tile_log_luminance: x,
        tile_valid: q,
        consensus_mask: st.consensus,
        frozen_target: frozen,
        trusted_indices: st.reliable,
        trend_unconverged_tiles: failed,
        period_effective: cfg.period.min(n),
        scene_cuts: cuts,
        time_gaps: gaps,
        pairwise_summary: summary,
    })
}
