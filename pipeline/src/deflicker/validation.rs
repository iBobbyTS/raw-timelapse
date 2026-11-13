use super::{
    estimation::limits,
    measurement::compatible,
    stats::{finite, median, need, percentile},
    ExposureResult, FrameMeasurement, Result,
};
#[derive(Debug, Clone)]
pub struct ValidationSummary {
    pub errors: Vec<Option<f64>>,
    pub coverage: Coverage,
    pub median_abs_error_ev: Option<f64>,
    pub p95_abs_error_ev: Option<f64>,
    pub outlier_frames: Vec<usize>,
    pub unobservable_frames: Vec<usize>,
    pub passed: bool,
    pub iteration: usize,
}
/// The service consumer owns the render loop; at most two feedback updates.
pub const MAX_FEEDBACK_UPDATES: usize = 2;
pub fn representative_indices(ms: &[FrameMeasurement]) -> Result<Vec<usize>> {
    if ms.is_empty() {
        return Ok(vec![]);
    }
    compatible(ms)?;
    let n = ms.len();
    let mut base = vec![];
    for t in [0, n / 2, n - 1] {
        if !base.contains(&t) {
            base.push(t);
        }
    }
    let levels: Vec<_> = ms
        .iter()
        .map(|m| {
            median(
                &m.log_luminance
                    .iter()
                    .zip(&m.valid)
                    .filter(|(_, q)| **q)
                    .map(|(v, _)| *v)
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let good: Vec<_> = (0..n).filter(|&t| levels[t].is_finite()).collect();
    if let Some(&first) = good.first() {
        let mut low = first;
        let mut high = first;
        for &t in &good {
            if levels[t] < levels[low] {
                low = t;
            }
            if levels[t] > levels[high] {
                high = t;
            }
        }
        for t in [low, high] {
            if !base.contains(&t) {
                base.push(t);
            }
        }
    }
    Ok(base)
}
pub fn evaluate_render(
    exposure: &mut ExposureResult,
    rendered: &[FrameMeasurement],
    iteration: usize,
) -> Result<ValidationSummary> {
    evaluate_render_with(exposure, rendered, iteration, 0.10)
}
pub fn evaluate_render_with(
    e: &mut ExposureResult,
    rendered: &[FrameMeasurement],
    iteration: usize,
    gate: f64,
) -> Result<ValidationSummary> {
    let n = e.frames.len();
    if rendered.len() != n || e.consensus_mask.len() != n || !gate.is_finite() || gate < 0. {
        return Err("rendered validation frame count mismatch or invalid gate".into());
    }
    let mut summary = ValidationSummary {
        errors: vec![None; n],
        coverage: Coverage {
            eligible: n,
            evaluated: 0,
            skipped: n,
        },
        median_abs_error_ev: None,
        p95_abs_error_ev: None,
        outlier_frames: vec![],
        unobservable_frames: vec![],
        passed: false,
        iteration,
    };
    let mut levels = vec![None; n];
    for t in 0..n {
        let est = &mut e.frames[t];
        let original = &e.consensus_mask[t];
        est.render_target_error_ev = None;
        if est.applied_ev.is_none()
            || ["LIMITED", "REVIEW_REQUIRED"].contains(&est.status.as_str())
            || est.desired_level_ev.is_none()
            || !original.iter().any(|v| *v)
        {
            summary.unobservable_frames.push(t);
            continue;
        }
        let m = &rendered[t];
        if m.valid.len() != original.len() {
            summary.unobservable_frames.push(t);
            continue;
        }
        m.validate()?;
        let values: Vec<_> = (0..original.len())
            .filter(|&i| original[i] && m.valid[i] && m.log_luminance[i].is_finite())
            .map(|i| m.log_luminance[i])
            .collect();
        let initial = original.iter().filter(|v| **v).count();
        if values.len() < need(original.len())
            || values.len() < (0.75 * initial as f64).ceil() as usize
        {
            summary.unobservable_frames.push(t);
            continue;
        }
        levels[t] = Some(median(&values));
    }
    let offsets: Vec<_> = (0..n)
        .filter_map(|t| Some(levels[t]? - e.frames[t].desired_level_ev?))
        .collect();
    if offsets.is_empty() {
        return Ok(summary);
    }
    let offset = median(&offsets);
    for t in 0..n {
        if let (Some(level), Some(desired)) = (levels[t], e.frames[t].desired_level_ev) {
            let error = desired + offset - level;
            if !error.is_finite() {
                return Err("nonfinite render target".into());
            }
            summary.errors[t] = Some(error);
            e.frames[t].render_target_error_ev = Some(error);
            if error.abs() > gate {
                summary.outlier_frames.push(t);
            }
        }
    }
    let abs: Vec<_> = summary.errors.iter().flatten().map(|v| v.abs()).collect();
    summary.coverage.evaluated = abs.len();
    summary.coverage.skipped = n - abs.len();
    summary.median_abs_error_ev = finite(median(&abs));
    summary.p95_abs_error_ev = finite(percentile(&abs, 95.));
    summary.passed = summary.median_abs_error_ev.is_some_and(|v| v <= 0.01)
        && summary.p95_abs_error_ev.is_some_and(|v| v <= 0.03)
        && summary.outlier_frames.is_empty()
        && summary.unobservable_frames.is_empty();
    Ok(summary)
}
pub fn feedback_update(
    e: &mut ExposureResult,
    s: &ValidationSummary,
    ev_limits: Option<(f64, f64)>,
) -> Result<usize> {
    feedback_update_with(e, s, ev_limits, 0.75, 0.25)
}
pub fn feedback_update_with(
    e: &mut ExposureResult,
    s: &ValidationSummary,
    ev_limits: Option<(f64, f64)>,
    damping: f64,
    max_step: f64,
) -> Result<usize> {
    let (lo, hi) = limits(ev_limits)?;
    if !damping.is_finite()
        || damping < 0.
        || !max_step.is_finite()
        || max_step < 0.
        || s.errors.iter().flatten().any(|v| !v.is_finite())
        || e.frames
            .iter()
            .filter_map(|f| f.applied_ev)
            .any(|v| !v.is_finite())
    {
        return Err("invalid feedback inputs".into());
    }
    let mut adjusted = 0;
    for (frame, error) in e.frames.iter_mut().zip(&s.errors) {
        if [
            "LIMITED",
            "REVIEW_REQUIRED",
            "INTERPOLATED_ESTIMATE",
            "DISABLED",
            "SHORT_SEQUENCE_NO_DEFLICKER",
            "GLOBAL_REFERENCE",
        ]
        .contains(&frame.status.as_str())
        {
            continue;
        }
        if let (Some(error), Some(applied)) = (error, frame.applied_ev) {
            let val = (applied + (damping * error).clamp(-max_step, max_step)).clamp(lo, hi);
            if val != applied {
                frame.applied_ev = Some(val);
                adjusted += 1;
            }
        }
    }
    Ok(adjusted)
}
pub fn feedback_diverged(old: &ValidationSummary, new: &ValidationSummary) -> bool {
    if old.median_abs_error_ev.is_none() || new.median_abs_error_ev.is_none() {
        return false;
    }
    [
        (old.median_abs_error_ev, new.median_abs_error_ev),
        (old.p95_abs_error_ev, new.p95_abs_error_ev),
    ]
    .iter()
    .any(|(a, b)| match (a, b) {
        (Some(a), Some(b)) => b - a > (a * 0.10).max(0.002),
        _ => false,
    })
}

/// Internal accounting. Eligible counts potential observations, including those
/// skipped for empty masks, missing targets, clipping or low spatial support.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    pub eligible: usize,
    pub evaluated: usize,
    pub skipped: usize,
}
#[derive(Debug, Clone)]
pub struct PlanPair {
    pub frame_id: usize,
    pub raw_change_ev: f64,
    pub corrected_change_ev: f64,
    pub intended_change_ev: f64,
    pub residual_ev: f64,
}
#[derive(Debug, Clone, Default)]
pub struct PlanQuality {
    pub coverage: Coverage,
    pub pairs: Vec<PlanPair>,
    pub common_jump_frames: Vec<usize>,
}
/// Assess physical local changes before handing gains to a renderer. This is
/// independent of frozen per-frame targets constructed by adding our own EV.
/// Smooth trends/confirmed steps and intentional partial strength are retained.
/// Absence of local evidence remains uncovered, never a zero-error observation.
pub fn evaluate_plan(
    ms: &[FrameMeasurement],
    e: &ExposureResult,
    cfg: &super::ExposureConfig,
) -> Result<PlanQuality> {
    compatible(ms)?;
    if ms.len() != e.frames.len() {
        return Err("plan measurement count mismatch".into());
    }
    let train: Vec<_> = ms[0]
        .centers
        .iter()
        .map(|c| {
            !cfg.exclude_rois
                .iter()
                .any(|r| c[0] >= r[0] && c[0] < r[2] && c[1] >= r[1] && c[1] < r[3])
        })
        .collect();
    let eligible = ms.len().saturating_sub(1);
    let mut quality = PlanQuality {
        coverage: Coverage {
            eligible,
            evaluated: 0,
            skipped: eligible,
        },
        ..Default::default()
    };
    for t in 1..ms.len() {
        let local = super::pairwise::local_common_change(&ms[t - 1], &ms[t], Some(&train))?;
        let (Some(raw), Some(a), Some(b)) = (
            local.delta_ev,
            e.frames[t - 1].applied_ev,
            e.frames[t].applied_ev,
        ) else {
            continue;
        };
        let intended = if cfg.brightness == "uniform" {
            (1. - cfg.strength) * raw
        } else {
            let (Some(da), Some(db)) = (
                e.frames[t - 1].desired_level_ev,
                e.frames[t].desired_level_ev,
            ) else {
                continue;
            };
            let (Some(ma), Some(mb)) = (
                e.frames[t - 1].measured_level_ev,
                e.frames[t].measured_level_ev,
            ) else {
                continue;
            };
            // Desired levels include partial strength. Retain the observed
            // uncorrected part while independently testing the graph's change.
            db - da + (1. - cfg.strength) * (raw - (mb - ma))
        };
        let corrected = raw + b - a;
        let residual = corrected - intended;
        if !residual.is_finite() {
            return Err("nonfinite plan residual".into());
        }
        if residual.abs() > 0.05 {
            quality.common_jump_frames.push(t);
        }
        quality.pairs.push(PlanPair {
            frame_id: t,
            raw_change_ev: raw,
            corrected_change_ev: corrected,
            intended_change_ev: intended,
            residual_ev: residual,
        });
    }
    quality.coverage.evaluated = quality.pairs.len();
    quality.coverage.skipped = eligible - quality.pairs.len();
    Ok(quality)
}
