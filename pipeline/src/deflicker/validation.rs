use super::{
    estimation::limits,
    measurement::compatible,
    stats::{finite, median, need, percentile},
    ExposureResult, FrameMeasurement, Result,
};
#[derive(Debug, Clone)]
pub struct ValidationSummary {
    pub errors: Vec<Option<f64>>,
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
