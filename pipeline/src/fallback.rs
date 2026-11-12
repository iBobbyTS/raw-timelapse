//! Global-reference fallback: the fail-open estimator of last resort.
//!
//! When the legacy V4 estimator cannot produce a reliable exposure for a
//! frame (no matched-background evidence), this module fills the hole from a
//! whole-frame reference curve: the per-frame median valid-tile log2 level
//! (the engine's `row_level` semantics), smoothed by the same
//! Huber-Whittaker trend used by the verified path (a segment median for
//! `uniform` intent), minus confirmed steps, split at scene-cut/time-gap
//! boundaries, and offset-aligned to the nearest verified neighbour
//! estimates so post-correction levels stay continuous across the seam.
//!
//! Provenance: the only element borrowed from the reverse-engineered
//! LRTimelapse Visual Deflicker is the concept of a per-frame global
//! brightness reference curve as the estimator of last resort. Measurement
//! input, smoother, EV semantics and application are this system's existing
//! machinery: no rendered previews, no LOESS, no empirical logit calibration,
//! no accuracy deadband, no multipass.

use crate::json::Json;
use rawtl_linear_video::deflicker::estimation::step_vector;
use rawtl_linear_video::deflicker::trends::huber_whittaker;
use rawtl_linear_video::deflicker::{ExposureResult, ExposureStep, FrameMeasurement};

/// Status assigned to frames estimated by this fallback.
pub const STATUS: &str = "GLOBAL_REFERENCE";

#[derive(Debug, Clone)]
pub struct FallbackConfig {
    pub period: usize,
    pub strength: f64,
    pub brightness: String,
    pub ev_limits: Option<(f64, f64)>,
    pub steps: Vec<ExposureStep>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FallbackSummary {
    pub filled: usize,
    pub rendered_at_zero_ev: usize,
    pub hole_ranges: Vec<(usize, usize)>,
    pub linear_reference_segments: usize,
    pub anchor_frames: Vec<usize>,
}

impl FallbackSummary {
    fn empty() -> Self {
        FallbackSummary {
            filled: 0,
            rendered_at_zero_ev: 0,
            hole_ranges: vec![],
            linear_reference_segments: 0,
            anchor_frames: vec![],
        }
    }

    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            (
                "method",
                Json::Str(
                    "whole-frame valid-tile median log2 level -> Huber-Whittaker trend \
                     (uniform intent: segment median) per continuity segment, confirmed \
                     steps removed before smoothing, offset-aligned to verified neighbours"
                        .into(),
                ),
            ),
            (
                "provenance",
                Json::Str(
                    "concept borrowed from the reverse-engineered LRTimelapse Visual \
                     Deflicker; all measurement, smoothing and application machinery is \
                     this system's own"
                        .into(),
                ),
            ),
            ("frames_filled", Json::Int(self.filled as i64)),
            (
                "frames_rendered_at_zero_ev",
                Json::Int(self.rendered_at_zero_ev as i64),
            ),
            (
                "hole_ranges",
                Json::Array(
                    self.hole_ranges
                        .iter()
                        .map(|(a, b)| Json::Array(vec![Json::Int(*a as i64), Json::Int(*b as i64)]))
                        .collect(),
                ),
            ),
            (
                "linear_reference_segments",
                Json::Int(self.linear_reference_segments as i64),
            ),
            (
                "anchor_frames",
                Json::Array(
                    self.anchor_frames
                        .iter()
                        .map(|&t| Json::Int(t as i64))
                        .collect(),
                ),
            ),
        ])
    }
}

/// Median with NumPy linear-interpolation percentile semantics (matches the
/// engine's private `stats::median`).
fn median(values: &[f64]) -> f64 {
    let mut sorted: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return f64::NAN;
    }
    sorted.sort_by(f64::total_cmp);
    let position = (sorted.len() - 1) as f64 * 0.5;
    let low = position.floor() as usize;
    let high = position.ceil() as usize;
    sorted[low] + (sorted[high] - sorted[low]) * (position - low as f64)
}

/// The engine's `row_level` semantics: median log2 luminance over valid tiles.
fn row_level(m: &FrameMeasurement) -> f64 {
    median(
        &m.log_luminance
            .iter()
            .zip(&m.valid)
            .filter(|(_, valid)| **valid)
            .map(|(value, _)| *value)
            .collect::<Vec<_>>(),
    )
}

fn interpolate(t: usize, a: usize, b: usize, x: f64, y: f64) -> f64 {
    x + (y - x) * (t - a) as f64 / (b - a) as f64
}

/// Fill every frame whose `applied_ev` is `None` (the estimator's honest
/// "no reliable estimate" marker) from the global reference curve. Never
/// fails open on its own account: frames in segments without any usable
/// measurement render at 0 EV with an explicit warning.
pub fn fill_holes(
    measurements: &[FrameMeasurement],
    exposure: &mut ExposureResult,
    boundaries: &[usize],
    config: &FallbackConfig,
) -> Result<FallbackSummary, String> {
    let n = exposure.frames.len();
    if measurements.len() != n {
        return Err("fallback measurement count mismatch".into());
    }
    if config.period < 6
        || !config.strength.is_finite()
        || !(0.0..=1.0).contains(&config.strength)
        || !["smooth", "uniform"].contains(&config.brightness.as_str())
    {
        return Err("invalid fallback configuration".into());
    }
    if let Some((low, high)) = config.ev_limits {
        if !low.is_finite() || !high.is_finite() || low >= high {
            return Err("invalid fallback EV limits".into());
        }
    }
    let mut summary = FallbackSummary::empty();
    if !exposure.frames.iter().any(|frame| frame.applied_ev.is_none()) {
        return Ok(summary);
    }
    let steps = step_vector(n, &config.steps)?;

    // Maximal hole runs over the whole sequence (report metadata).
    let mut t = 0;
    while t < n {
        if exposure.frames[t].applied_ev.is_none() {
            let start = t;
            while t < n && exposure.frames[t].applied_ev.is_none() {
                t += 1;
            }
            summary.hole_ranges.push((start, t - 1));
        } else {
            t += 1;
        }
    }

    // Raw whole-frame levels.
    let raw: Vec<f64> = measurements.iter().map(row_level).collect();

    // Segment starts: 0 plus interior boundaries.
    let mut starts: Vec<usize> = vec![0];
    for &boundary in boundaries {
        if boundary > 0 && boundary < n && !starts.contains(&boundary) {
            starts.push(boundary);
        }
    }
    starts.sort_unstable();
    starts.dedup();

    for (position, &seg_start) in starts.iter().enumerate() {
        let seg_end = starts.get(position + 1).copied().unwrap_or(n);
        let has_hole = (seg_start..seg_end).any(|t| exposure.frames[t].applied_ev.is_none());
        if !has_hole {
            continue;
        }

        // Fill missing levels inside the segment by linear interpolation
        // between measured frames, extended constantly at the edges. A
        // segment without a single measured frame keeps NaN levels.
        let mut level = raw[seg_start..seg_end].to_vec();
        let measured: Vec<usize> = (seg_start..seg_end)
            .filter(|&t| level[t - seg_start].is_finite())
            .collect();
        if !measured.is_empty() {
            for t in seg_start..seg_end {
                if level[t - seg_start].is_finite() {
                    continue;
                }
                let value = if t < measured[0] {
                    level[measured[0] - seg_start]
                } else if t > *measured.last().expect("nonempty") {
                    level[*measured.last().expect("nonempty") - seg_start]
                } else {
                    let left = *measured
                        .iter()
                        .filter(|&&m| m < t)
                        .max()
                        .expect("measured below t");
                    let right = *measured
                        .iter()
                        .filter(|&&m| m > t)
                        .min()
                        .expect("measured above t");
                    interpolate(
                        t,
                        left,
                        right,
                        level[left - seg_start],
                        level[right - seg_start],
                    )
                };
                level[t - seg_start] = value;
            }
        }

        // Reference curve in confirmed-step-removed space.
        let adjusted: Vec<f64> = (seg_start..seg_end)
            .map(|t| level[t - seg_start] - steps[t])
            .collect();
        let valid: Vec<bool> = adjusted.iter().map(|v| v.is_finite()).collect();
        let trend: Option<Vec<f64>> = if config.brightness == "uniform" {
            let target = median(&adjusted);
            target.is_finite().then(|| vec![target; seg_end - seg_start])
        } else {
            let fit = huber_whittaker(&adjusted, &valid, config.period)?;
            if fit.converged {
                Some(fit.trend)
            } else {
                None
            }
        };
        let trend = match trend {
            Some(trend) => trend,
            None => {
                // Degraded reference for short or nonconverged segments: a
                // linear ramp between the first and last measured adjusted
                // level (mirrors the engine's own interpolation semantics).
                summary.linear_reference_segments += 1;
                match (measured.first(), measured.last()) {
                    (Some(&first), Some(&last)) if first != last => {
                        let slope = (adjusted[last - seg_start] - adjusted[first - seg_start])
                            / (last - first) as f64;
                        (seg_start..seg_end)
                            .map(|t| adjusted[first - seg_start] + slope * (t - first) as f64)
                            .collect()
                    }
                    (Some(&only), _) | (_, Some(&only)) => {
                        vec![adjusted[only - seg_start]; seg_end - seg_start]
                    }
                    (_, _) => vec![f64::NAN; seg_end - seg_start],
                }
            }
        };

        // Verified anchors inside this segment: engine-estimated frames with
        // a known post-correction level (measured + applied).
        let anchors: Vec<usize> = (seg_start..seg_end)
            .filter(|&t| {
                let frame = &exposure.frames[t];
                frame.applied_ev.is_some()
                    && frame.measured_level_ev.is_some()
                    && frame.status != STATUS
                    && level[t - seg_start].is_finite()
                    && trend[t - seg_start].is_finite()
            })
            .collect();
        let anchor_delta = |t: usize| -> Option<f64> {
            let frame = &exposure.frames[t];
            let correction = config.strength * (trend[t - seg_start] - adjusted[t - seg_start]);
            let unaligned_post = level[t - seg_start] + correction;
            let verified_post = frame.measured_level_ev? + frame.applied_ev?;
            (verified_post - unaligned_post).is_finite().then_some(verified_post - unaligned_post)
        };
        for &anchor in &anchors {
            if !summary.anchor_frames.contains(&anchor) {
                summary.anchor_frames.push(anchor);
            }
        }
        // Materialise the anchor offsets once per segment so the immutable
        // borrow of `exposure` ends before any frame is written.
        let anchor_data: Vec<(usize, f64)> = anchors
            .iter()
            .filter_map(|&anchor| anchor_delta(anchor).map(|delta| (anchor, delta)))
            .collect();

        // Fill this segment's hole runs.
        let mut t = seg_start;
        while t < seg_end {
            if exposure.frames[t].applied_ev.is_some() {
                t += 1;
                continue;
            }
            let run_start = t;
            while t < seg_end && exposure.frames[t].applied_ev.is_none() {
                t += 1;
            }
            let run_end = t - 1;
            let left = anchor_data
                .iter()
                .filter(|(anchor, _)| *anchor < run_start)
                .next_back()
                .copied();
            let right = anchor_data
                .iter()
                .filter(|(anchor, _)| *anchor > run_end)
                .next()
                .copied();
            let deltas: Vec<f64> = (run_start..=run_end)
                .map(|frame_index| match (left, right) {
                    (Some((a, delta_a)), Some((b, delta_b))) => {
                        interpolate(frame_index, a, b, delta_a, delta_b)
                    }
                    (Some((_, delta_a)), None) => delta_a,
                    (None, Some((_, delta_b))) => delta_b,
                    (None, None) => 0.0,
                })
                .collect();
            for frame_index in run_start..=run_end {
                let curve = trend[frame_index - seg_start];
                let frame = &mut exposure.frames[frame_index];
                if !curve.is_finite() || !level[frame_index - seg_start].is_finite() {
                    // No usable measurement anywhere in the segment: the
                    // fail-open contract still publishes, honestly at 0 EV.
                    frame.applied_ev = Some(0.0);
                    frame.requested_ev = Some(0.0);
                    frame.status = STATUS.to_string();
                    frame.warnings.push(
                        "GLOBAL_REFERENCE_UNMEASURED: no usable tiles in this continuity \
                         segment; rendered at 0 EV"
                            .into(),
                    );
                    summary.filled += 1;
                    summary.rendered_at_zero_ev += 1;
                    continue;
                }
                let correction =
                    config.strength * (curve - adjusted[frame_index - seg_start]);
                let delta = deltas[frame_index - run_start];
                let requested = correction + delta;
                let applied = match config.ev_limits {
                    Some((low, high)) => requested.clamp(low, high),
                    None => requested,
                };
                frame.applied_ev = Some(applied);
                frame.requested_ev = Some(requested);
                frame.measured_level_ev = Some(level[frame_index - seg_start]);
                frame.desired_level_ev = Some(curve + delta);
                frame.status = STATUS.to_string();
                frame.warnings.push(
                    "GLOBAL_REFERENCE_FALLBACK: no reliable pairwise estimate; \
                     whole-frame reference curve applied"
                        .into(),
                );
                if applied != requested {
                    frame.warnings.push("EXPOSURE_LIMITED".into());
                }
                summary.filled += 1;
            }
            // Frozen targets follow the engine convention for filled frames.
            for frame_index in run_start..=run_end {
                let applied = exposure.frames[frame_index].applied_ev.unwrap_or(0.0);
                if let (Some(values), Some(valid_row), Some(frozen)) = (
                    exposure.tile_log_luminance.get(frame_index),
                    exposure.tile_valid.get(frame_index),
                    exposure.frozen_target.get_mut(frame_index),
                ) {
                    for tile in 0..frozen.len().min(values.len().min(valid_row.len())) {
                        if valid_row[tile] {
                            frozen[tile] = values[tile] + applied;
                        }
                    }
                }
            }
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtl_linear_video::deflicker::FrameEstimate;

    fn measurement(levels: &[f64]) -> FrameMeasurement {
        let k = levels.len();
        FrameMeasurement {
            source_sha256: "test".into(),
            log_luminance: levels.to_vec(),
            valid: vec![true; k],
            centers: (0..k)
                .map(|tile| [(tile % 12) as f64 / 12.0, (tile / 12) as f64 / 8.0])
                .collect(),
            saturation_fraction: vec![0.0; k],
            output_clip_fraction: vec![0.0; k],
            width: 1024,
            height: 768,
            grid_nx: 12,
            grid_ny: 8,
            saturation_mask_quality: "camera_domain".into(),
            log_pixel_samples: None,
            log_channel_means: None,
        }
    }

    fn estimate(index: usize, applied: Option<f64>, measured: Option<f64>) -> FrameEstimate {
        FrameEstimate {
            index,
            status: if applied.is_some() { "OK".into() } else { "REVIEW_REQUIRED".into() },
            common_residual_ev: None,
            requested_ev: applied,
            applied_ev: applied,
            cumulative_confirmed_step_ev: 0.0,
            valid_tile_count: 96,
            consensus_fraction: None,
            residual_mad_ev: None,
            warnings: vec![],
            render_target_error_ev: None,
            analysis_highlight_increase_fraction: None,
            render_ev: None,
            measured_level_ev: measured,
            desired_level_ev: measured,
            segment_id: 0,
        }
    }

    fn result(frames: Vec<FrameEstimate>, k: usize) -> ExposureResult {
        let n = frames.len();
        let any_trusted = frames.iter().any(|f| f.applied_ev.is_some());
        ExposureResult {
            frames,
            tile_log_luminance: vec![vec![0.0; k]; n],
            tile_valid: vec![vec![true; k]; n],
            consensus_mask: vec![vec![false; k]; n],
            frozen_target: vec![vec![f64::NAN; k]; n],
            trusted_indices: vec![any_trusted; n.max(1)],
            trend_unconverged_tiles: 0,
            period_effective: 60,
            scene_cuts: vec![],
            time_gaps: vec![],
            pairwise_summary: Json::Null,
        }
    }

    fn config() -> FallbackConfig {
        FallbackConfig {
            period: 60,
            strength: 1.0,
            brightness: "smooth".into(),
            ev_limits: None,
            steps: vec![],
        }
    }

    fn flat_measurement(value: f64) -> FrameMeasurement {
        measurement(&[value; 96])
    }

    #[test]
    fn no_holes_is_a_noop() {
        let frames: Vec<_> = (0..10)
            .map(|t| estimate(t, Some(0.0), Some(-2.0)))
            .collect();
        let mut exposure = result(frames, 96);
        let measurements: Vec<_> = (0..10).map(|_| flat_measurement(-2.0)).collect();
        let summary =
            fill_holes(&measurements, &mut exposure, &[], &config()).expect("fill");
        assert_eq!(summary, FallbackSummary::empty());
        assert!(exposure.frames.iter().all(|f| f.status == "OK"));
    }

    #[test]
    fn whole_sequence_hole_tracks_the_reference_curve() {
        // Sinusoidal flicker around a constant scene level.
        let n = 40;
        let levels: Vec<f64> = (0..n)
            .map(|t| -2.0 + 0.3 * (t as f64 * 0.7).sin())
            .collect();
        let measurements: Vec<_> = levels.iter().map(|v| flat_measurement(*v)).collect();
        let frames: Vec<_> = (0..n).map(|t| estimate(t, None, None)).collect();
        let mut exposure = result(frames, 96);
        let summary = fill_holes(&measurements, &mut exposure, &[], &config())
            .expect("fill");
        assert_eq!(summary.filled, n);
        assert_eq!(summary.rendered_at_zero_ev, 0);
        assert!(summary.anchor_frames.is_empty());
        let spread_before: f64 = {
            let mean = levels.iter().sum::<f64>() / n as f64;
            (levels.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64).sqrt()
        };
        let posts: Vec<f64> = exposure
            .frames
            .iter()
            .map(|f| f.measured_level_ev.expect("measured") + f.applied_ev.expect("applied"))
            .collect();
        let mean = posts.iter().sum::<f64>() / n as f64;
        let spread_after =
            (posts.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64).sqrt();
        assert!(
            spread_after < 0.25 * spread_before,
            "flicker not removed: before {spread_before:.4} after {spread_after:.4}"
        );
        assert!(exposure.frames.iter().all(|f| f.status == STATUS));
        assert!(exposure
            .frames
            .iter()
            .all(|f| f.warnings.iter().any(|w| w.starts_with("GLOBAL_REFERENCE_FALLBACK"))));
        assert!(exposure
            .frames
            .iter()
            .all(|f| f.applied_ev.is_some_and(|v| v.is_finite())));
    }

    #[test]
    fn middle_hole_aligns_to_verified_neighbours() {
        // Verified anchors live in a biased level space (+5 EV) with zero
        // corrections; the fallback must continue that post-correction level.
        let n = 30;
        let levels: Vec<f64> = (0..n).map(|t| -2.0 + 0.02 * t as f64).collect();
        let measurements: Vec<_> = levels.iter().map(|v| flat_measurement(*v)).collect();
        let frames: Vec<_> = (0..n)
            .map(|t| {
                if (10..20).contains(&t) {
                    estimate(t, None, None)
                } else {
                    // measured level carries a constant +5 EV space bias
                    estimate(t, Some(0.0), Some(levels[t] + 5.0))
                }
            })
            .collect();
        let mut exposure = result(frames, 96);
        let summary = fill_holes(&measurements, &mut exposure, &[], &config())
            .expect("fill");
        assert_eq!(summary.filled, 10);
        assert_eq!(summary.anchor_frames.len() >= 2, true);
        for t in 10..20 {
            let frame = &exposure.frames[t];
            assert_eq!(frame.status, STATUS);
            let applied = frame.applied_ev.expect("applied");
            assert!(
                (applied - 5.0).abs() < 0.05,
                "frame {t}: alignment delta {applied:.4} must track the +5 EV anchor bias"
            );
        }
        // Seam continuity of post-correction levels.
        let post = |t: usize| {
            exposure.frames[t].measured_level_ev.expect("m") + exposure.frames[t].applied_ev.expect("a")
        };
        for seam in [9usize, 10usize, 18usize, 19usize] {
            assert!(
                (post(seam + 1) - post(seam)).abs() < 0.06,
                "seam {seam}->{} jumped by {:.4}",
                seam + 1,
                post(seam + 1) - post(seam)
            );
        }
    }

    #[test]
    fn unmeasured_frames_inside_a_hole_get_interpolated_levels() {
        let n = 24;
        let mut measurements: Vec<_> = (0..n)
            .map(|t| flat_measurement(-2.0 + 0.01 * t as f64))
            .collect();
        // Frames 11..13 have no valid tiles at all.
        for frame in measurements.iter_mut().take(14).skip(11) {
            frame.valid = vec![false; 96];
        }
        let frames: Vec<_> = (0..n)
            .map(|t| if (8..16).contains(&t) { estimate(t, None, None) } else { estimate(t, Some(0.0), Some(-2.0)) })
            .collect();
        let mut exposure = result(frames, 96);
        let summary = fill_holes(&measurements, &mut exposure, &[], &config())
            .expect("fill");
        assert_eq!(summary.filled, 8);
        assert_eq!(summary.rendered_at_zero_ev, 0);
        for t in 8..16 {
            let frame = &exposure.frames[t];
            assert!(frame.applied_ev.is_some_and(|v| v.is_finite()), "frame {t}");
            assert!(frame.measured_level_ev.is_some(), "frame {t} level");
        }
    }

    #[test]
    fn segment_without_measurements_renders_at_zero_ev() {
        let n = 16;
        let mut measurements: Vec<_> = (0..n).map(|_| flat_measurement(-2.0)).collect();
        for frame in measurements.iter_mut() {
            frame.valid = vec![false; 96];
        }
        let frames: Vec<_> = (0..n).map(|t| estimate(t, None, None)).collect();
        let mut exposure = result(frames, 96);
        let summary = fill_holes(&measurements, &mut exposure, &[], &config())
            .expect("fill");
        assert_eq!(summary.filled, n);
        assert_eq!(summary.rendered_at_zero_ev, n);
        assert!(exposure.frames.iter().all(|f| f.applied_ev == Some(0.0)));
        assert!(exposure.frames.iter().all(|f| f.status == STATUS));
        assert!(exposure
            .frames
            .iter()
            .all(|f| f.warnings.iter().any(|w| w.starts_with("GLOBAL_REFERENCE_UNMEASURED"))));
    }

    #[test]
    fn short_segment_degrades_to_linear_reference() {
        // A time-gap boundary at 18 splits a 20-frame hole so the second
        // segment has 2 frames: below the Whittaker minimum.
        let n = 20;
        let levels: Vec<f64> = (0..n).map(|t| -2.0 + 0.05 * t as f64).collect();
        let measurements: Vec<_> = levels.iter().map(|v| flat_measurement(*v)).collect();
        let frames: Vec<_> = (0..n).map(|t| estimate(t, None, None)).collect();
        let mut exposure = result(frames, 96);
        let summary =
            fill_holes(&measurements, &mut exposure, &[18], &config()).expect("fill");
        assert_eq!(summary.filled, n);
        assert!(summary.linear_reference_segments >= 1);
        assert!(exposure.frames.iter().all(|f| f.applied_ev.is_some_and(|v| v.is_finite())));
    }

    #[test]
    fn confirmed_steps_are_preserved_not_flattened() {
        // Raw levels jump +1 EV at frame 10; the confirmed step says that
        // jump is intended exposure, so the fallback must not correct it.
        let n = 30;
        let levels: Vec<f64> = (0..n)
            .map(|t| -2.0 + if t >= 10 { 1.0 } else { 0.0 } + 0.1 * (t as f64 * 0.9).sin())
            .collect();
        let measurements: Vec<_> = levels.iter().map(|v| flat_measurement(*v)).collect();
        let frames: Vec<_> = (0..n).map(|t| estimate(t, None, None)).collect();
        let mut exposure = result(frames, 96);
        let config = FallbackConfig {
            steps: vec![ExposureStep { start_frame: 10, delta_ev: 1.0 }],
            ..config()
        };
        fill_holes(&measurements, &mut exposure, &[], &config).expect("fill");
        for (t, frame) in exposure.frames.iter().enumerate() {
            let applied = frame.applied_ev.expect("applied");
            assert!(
                applied.abs() < 0.2,
                "frame {t}: confirmed step was flattened (applied {applied:+.4})"
            );
        }
    }

    #[test]
    fn ev_limits_clamp_the_fallback_correction() {
        let n = 20;
        let levels: Vec<f64> = (0..n).map(|t| -2.0 + 0.1 * t as f64).collect();
        let measurements: Vec<_> = levels.iter().map(|v| flat_measurement(*v)).collect();
        let frames: Vec<_> = (0..n)
            .map(|t| if t == 0 { estimate(0, Some(0.0), Some(levels[0])) } else { estimate(t, None, None) })
            .collect();
        let mut exposure = result(frames, 96);
        let config = FallbackConfig {
            ev_limits: Some((-0.2, 0.2)),
            ..config()
        };
        fill_holes(&measurements, &mut exposure, &[], &config).expect("fill");
        for frame in &exposure.frames[1..] {
            let applied = frame.applied_ev.expect("applied");
            assert!((-0.2..=0.2).contains(&applied));
            let requested = frame.requested_ev.expect("requested");
            if requested != applied {
                assert!(frame.warnings.iter().any(|w| w == "EXPOSURE_LIMITED"));
            }
        }
    }

    #[test]
    fn strength_scales_the_correction_and_uniform_targets_the_median() {
        let n = 24;
        let levels: Vec<f64> = (0..n).map(|t| -2.0 + 0.04 * (t as f64 - 11.0)).collect();
        let measurements: Vec<_> = levels.iter().map(|v| flat_measurement(*v)).collect();
        let frames: Vec<_> = (0..n).map(|t| estimate(t, None, None)).collect();
        let mut exposure = result(frames, 96);
        let config = FallbackConfig {
            strength: 0.5,
            brightness: "uniform".into(),
            ..config()
        };
        fill_holes(&measurements, &mut exposure, &[], &config).expect("fill");
        let target = median(&levels);
        for (t, frame) in exposure.frames.iter().enumerate() {
            let applied = frame.applied_ev.expect("applied");
            let expected = 0.5 * (target - levels[t]);
            assert!(
                (applied - expected).abs() < 1e-9,
                "frame {t}: applied {applied:.5} != half-strength median pull {expected:.5}"
            );
        }
    }
}
