//! Deflicker node orchestration: the legacy V4 two-pass service semantics
//! mapped onto the current synchronous CLI.
//!
//! Order of operations (enabled path):
//! analysis pass @0 EV -> capture-time continuity -> estimation -> review
//! policy -> gain probe -> application pass(es) with render validation and at
//! most two feedback updates -> encode/validate/roundtrip/preview -> independent
//! encoded-video audit -> exposure report + curve -> manifest -> atomic publish.
//!
//! `--strength 0` uses the historical single pass (no analysis, probe,
//! validation or audit) and records DISABLED frame statuses.

use crate::audit::{self, AuditConfig};
use crate::cli::{DeflickerConfig, Estimator, TemporalQuality};
use crate::error::{
    Error, Result, EXIT_FRAME, EXIT_GAIN_PROBE, EXIT_PUBLISH, EXIT_RENDER_VALIDATION,
};
use crate::fallback::{self, FallbackConfig};
use crate::frame::{self, FrameInfo, FrameSequence, ProduceConfig};
use crate::json::{self, Json};
use crate::{encode_and_validate, publish_run, RunContext};
use rawtl_linear_video::deflicker::measurement::{AnalysisSidecar, FrameMeasurement};
use rawtl_linear_video::deflicker::report::{finite_json, ExposureReport, ReportFrame};
use rawtl_linear_video::deflicker::validation::{
    evaluate_render, feedback_diverged, feedback_update, representative_indices, ValidationSummary,
    MAX_FEEDBACK_UPDATES,
};
use rawtl_linear_video::deflicker::{
    estimate_exposure, ExposureConfig, ExposureResult, FrameEstimate,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

const REPORT_NAME: &str = "exposure_report.json";
const CURVE_NAME: &str = "exposure-curve.csv";

pub fn run(
    options: &crate::cli::Options,
    input: &Path,
    output: &Path,
    started: Instant,
) -> Result<()> {
    let mut state = RunState::new(options, input, output);
    let result = run_with_boundary(options, input, output, started, &mut state);
    if let Err(err) = result {
        // Legacy `service.py` semantics: the report is written from the final
        // state, so a late failure (encoder/validation/preview/publication) can
        // never leave a success run_status next to an error. Statuses already
        // set for an explicit failure class are preserved.
        state.run_status = failure_run_status(&state.run_status, err.code);
        state.error = Some(err.message.clone());
        // The output directory is already established and writable; every
        // failure after that point retains diagnostics. A failure to write the
        // report must not mask the original error.
        if let Err(report_err) = write_reports(options, output, &mut state) {
            eprintln!(
                "warning: could not write failure report {}: {}",
                output.join(REPORT_NAME).display(),
                report_err.message
            );
        } else if state.exposure.is_some() {
            eprintln!(
                "note: failure report written to {}",
                output.join(REPORT_NAME).display()
            );
        }
        return Err(err);
    }
    Ok(())
}

/// Header, required tools and source inventory live inside the report boundary:
/// the output directory already exists and is writable, so exit 20/10 must
/// still leave `exposure_report.json` (with an empty frame list).
fn run_with_boundary(
    options: &crate::cli::Options,
    input: &Path,
    output: &Path,
    started: Instant,
    state: &mut RunState,
) -> Result<()> {
    println!("rawtl-linear-video {}", env!("CARGO_PKG_VERSION"));
    println!("input:  {}", input.display());
    println!("output: {}", output.display());
    println!(
        "fps:    {} ({})",
        options.fps.arg(),
        if options.fps_explicit {
            "CLI override"
        } else {
            crate::cli::LEGACY_FPS_PROVENANCE
        }
    );
    let deflicker = &options.deflicker;
    if deflicker.enabled() {
        println!(
            "deflicker: enabled (strength {}, period {}, brightness {}, estimator {}, \
             ev_limits {}, temporal_quality {})",
            deflicker.strength,
            deflicker.period,
            deflicker.brightness.as_str(),
            deflicker.estimator.as_str(),
            match deflicker.ev_limits {
                Some((low, high)) => format!("{low},{high}"),
                None => "none".to_string(),
            },
            deflicker.temporal_quality.as_str(),
        );
    } else {
        println!("deflicker: disabled (--strength 0: single pass, REQ-011 regression path)");
    }

    crate::proc::require_executable(&options.native_tool, "--dng-linear-frame")?;
    crate::proc::require_executable(&options.ffmpeg, "--ffmpeg")?;
    crate::proc::require_executable(&options.ffprobe, "--ffprobe")?;

    let sources = crate::order::enumerate_dng(input)?;
    println!("found {} DNG frame(s) in numeric order:", sources.len());
    for (index, source) in sources.iter().enumerate() {
        println!("  {:>3}. {}", index + 1, source.name);
    }
    state.set_frame_count(sources.len());

    let context = RunContext {
        options,
        input,
        output,
        sources: &sources,
        partial_dir: output.join(".rawtl-partial"),
        started,
    };
    if options.deflicker.enabled() {
        run_deflicker(&context, state)
    } else {
        run_disabled(&context, state)
    }
}

// ---------------------------------------------------------------------------
// Disabled path (--strength 0): historical single pass, byte-compatible inputs
// ---------------------------------------------------------------------------

fn run_disabled(context: &RunContext<'_>, state: &mut RunState) -> Result<()> {
    let options = context.options;
    let config = ProduceConfig {
        frames_dir: &context.output.join("frames"),
        logs_dir: &context.output.join("logs"),
        analysis_dir: None,
        exposure_ev: None,
        analysis_roi: None,
        no_png: false,
        echo: true,
        echo_tag: "frame ",
        converter: options.converter.as_deref(),
        converter_timeout: options.converter_timeout,
        frames_relative_dir: "frames",
        log_relative_dir: "logs",
    };
    let sequence = frame::produce(&options.native_tool, context.sources, &config)?;
    print_sequence_summary(&sequence);
    state.capture_times = sequence
        .frames
        .iter()
        .map(|frame| frame.capture_time.clone())
        .collect();
    state.filenames = sequence
        .frames
        .iter()
        .map(|frame| frame.source_name.clone())
        .collect();
    state.disabled_exposure(&options.deflicker);
    state.run_status = "OK".to_string();

    let stage = encode_and_validate(context, &sequence)?;
    write_reports(context.options, context.output, state)?;
    publish_run(
        context,
        &sequence,
        stage,
        disabled_manifest_block(state, sequence.frames.len()),
    )
}

// ---------------------------------------------------------------------------
// Enabled path
// ---------------------------------------------------------------------------

fn run_deflicker(context: &RunContext<'_>, state: &mut RunState) -> Result<()> {
    let options = context.options;
    let cfg = &options.deflicker;
    let output = context.output;

    // 1. Analysis pass @0 EV: measurements only, no PNG.
    let baseline_analysis = output.join("analysis/baseline");
    let baseline_meta = output.join("analysis/baseline/meta");
    let baseline_logs = output.join("logs/analysis");
    let analysis_config = ProduceConfig {
        frames_dir: &baseline_meta,
        logs_dir: &baseline_logs,
        analysis_dir: Some(&baseline_analysis),
        exposure_ev: Some(0.0),
        analysis_roi: Some(cfg.roi),
        no_png: true,
        echo: true,
        echo_tag: "analysis ",
        converter: options.converter.as_deref(),
        converter_timeout: options.converter_timeout,
        frames_relative_dir: "analysis/baseline/meta",
        log_relative_dir: "logs/analysis",
    };
    println!("\nanalysis pass (0 EV, tile measurements only)...");
    let baseline = frame::produce(&options.native_tool, context.sources, &analysis_config)?;
    print_sequence_summary(&baseline);
    let n = baseline.frames.len();

    state.filenames = baseline
        .frames
        .iter()
        .map(|frame| frame.source_name.clone())
        .collect();
    state.capture_times = baseline
        .frames
        .iter()
        .map(|frame| frame.capture_time.clone())
        .collect();

    // 2. Capture-time continuity (filename suspicion is metadata only).
    let continuity = continuity(&state.filenames, &state.capture_times);
    state.continuity = continuity.to_json();
    state.source_discontinuities = continuity.discontinuities_json();
    if continuity.coverage == "filename_only" {
        state.warnings.push(
            "SOURCE_METADATA_UNAVAILABLE: capture_time missing; continuity coverage is \
             filename-only and contributes no audit boundaries"
                .to_string(),
        );
    }
    println!(
        "continuity: coverage={} nominal_interval={} time_gaps={} filename_suspicions={}",
        continuity.coverage,
        continuity
            .nominal_interval_s
            .map(|value| format!("{value:.3}s"))
            .unwrap_or_else(|| "UNKNOWN".to_string()),
        continuity.time_gaps.len(),
        continuity.filename_suspicions.len()
    );

    // 3. Parse the frozen baseline measurements.
    let measurements: Vec<FrameMeasurement> = baseline
        .frames
        .iter()
        .map(|frame| parse_measurement(frame, "baseline"))
        .collect::<Result<_>>()?;
    state.baseline = Some(measurements.clone());

    // 4. Estimation (legacy V4 single-sequence semantics).
    let engine_config = ExposureConfig {
        period: cfg.period,
        strength: cfg.strength,
        steps: cfg.steps.clone(),
        brightness: cfg.brightness.as_str().to_string(),
        ev_limits: cfg.ev_limits,
        time_gaps: continuity.time_gap_indices(),
        estimator: cfg.estimator.as_str().to_string(),
        exclude_rois: cfg.exclude_rois.clone(),
        scene_cut_median_ev: 0.20,
        scene_cut_tile_mad_ev: 0.08,
    };
    let exposure = estimate_exposure(&measurements, &engine_config)
        .map_err(|err| Error::new(EXIT_FRAME, format!("exposure estimation failed: {err}")))?;
    println!(
        "estimate: {} frames, period_effective={}, scene_cuts={}, unconverged_trend_tiles={}",
        exposure.frames.len(),
        exposure.period_effective,
        exposure.scene_cuts.len(),
        exposure.trend_unconverged_tiles
    );
    state.exposure = Some(exposure);

    // 5. Fail-open policy: frames without a reliable estimate are filled
    //    from the global-reference curve (the estimator of last resort),
    //    aligned to verified neighbours. Publication is never blocked by an
    //    incomplete estimate; the report records exactly what happened.
    let unestimated = state
        .exposure
        .as_ref()
        .expect("exposure set")
        .frames
        .iter()
        .filter(|frame| frame.applied_ev.is_none())
        .count();
    let mut fallback_engaged = false;
    if unestimated > 0 {
        let boundaries: Vec<usize> = {
            let exposure = state.exposure.as_ref().expect("exposure set");
            let mut set: Vec<usize> = exposure
                .scene_cuts
                .iter()
                .map(|cut| cut.index)
                .chain(exposure.time_gaps.iter().copied())
                .filter(|index| *index > 0 && *index < n)
                .collect();
            set.sort_unstable();
            set.dedup();
            set
        };
        let fallback_config = FallbackConfig {
            period: cfg.period,
            strength: cfg.strength,
            brightness: cfg.brightness.as_str().to_string(),
            ev_limits: cfg.ev_limits,
            steps: cfg.steps.clone(),
        };
        let summary = {
            let exposure = state.exposure.as_mut().expect("exposure set");
            fallback::fill_holes(&measurements, exposure, &boundaries, &fallback_config)
                .map_err(|err| {
                    Error::new(
                        EXIT_FRAME,
                        format!("global-reference fallback failed: {err}"),
                    )
                })?
        };
        println!(
            "review policy: global-reference fallback filled {} of {} unestimated \
             frame(s) ({} at 0 EV)",
            summary.filled, unestimated, summary.rendered_at_zero_ev
        );
        state.warnings.push(format!(
            "GLOBAL_REFERENCE_FALLBACK: {} of {} frame(s) had no reliable pairwise \
             estimate and were corrected from the whole-frame reference curve",
            summary.filled, unestimated
        ));
        if summary.rendered_at_zero_ev > 0 {
            state.warnings.push(format!(
                "GLOBAL_REFERENCE_UNMEASURED: {} fallback frame(s) rendered at 0 EV \
                 (no usable tiles in their continuity segment)",
                summary.rendered_at_zero_ev
            ));
        }
        state.global_reference_fallback = summary.to_json();
        fallback_engaged = true;
    }

    // 6. Gain probe (skipped when disabled, short or without reliable estimates).
    let probe_required = cfg.strength != 0.0
        && n >= 8
        && state
            .exposure
            .as_ref()
            .expect("exposure set")
            .frames
            .iter()
            .any(|frame| frame.applied_ev.is_some());
    let probe = if probe_required {
        println!("gain probe (+/-0.25 EV on representative frames)...");
        run_gain_probe(context, &baseline, &measurements, cfg)?
    } else {
        let reason = if cfg.strength == 0.0 {
            "disabled by --strength 0"
        } else if n < 8 {
            "short sequence (fewer than 8 frames)"
        } else {
            "no reliable exposures"
        };
        Json::obj(vec![
            ("status", Json::Str("SKIPPED".into())),
            ("reason", Json::Str(reason.to_string())),
        ])
    };
    println!(
        "gain probe: {}",
        probe
            .get("status")
            .and_then(Json::as_str)
            .unwrap_or("UNKNOWN")
    );
    let probe_passed = probe.get("passed").and_then(|value| match value {
        Json::Bool(value) => Some(*value),
        _ => None,
    });
    state.gain_probe = probe;
    let mut probe_flagged = false;
    if probe_passed == Some(false) {
        // Fail-open contract: a failed probe is a recorded review flag, not
        // a publication gate. The applied gains may be wrong; the report and
        // manifest carry the probe verdict and reasons verbatim.
        let reasons = state
            .gain_probe
            .get("reasons")
            .cloned()
            .unwrap_or(Json::Null);
        state.warnings.push(format!(
            "GAIN_PROBE_FAILED: RAW exposure gain probe reported {} \
             (published anyway; verify before using the deliverable)",
            json::to_pretty_string(&reasons).trim()
        ));
        probe_flagged = true;
    }

    // 7. Application passes with render validation and at most two feedback updates.
    let must_validate = cfg.strength != 0.0
        && n >= 8
        && state
            .exposure
            .as_ref()
            .expect("exposure set")
            .trusted_indices
            .iter()
            .any(|trusted| *trusted);
    let mut best: Option<BestRound> = None;
    let mut best_score = (f64::INFINITY, f64::INFINITY);
    let mut previous: Option<ValidationSummary> = None;
    let mut feedback_count = 0usize;

    for iteration in 0..=MAX_FEEDBACK_UPDATES {
        println!(
            "\napplication pass {} (iteration {iteration})...",
            iteration + 1
        );
        let round = apply_pass(context, state, iteration)?;
        let (sequence, rendered) = round;
        let exposure = state.exposure.as_mut().expect("exposure set");
        let mut summary = if must_validate {
            Some(
                evaluate_render(exposure, &rendered, iteration).map_err(|err| {
                    Error::new(
                        EXIT_RENDER_VALIDATION,
                        format!("render validation failed: {err}"),
                    )
                })?,
            )
        } else {
            None
        };
        if let Some(candidate) = &summary {
            if candidate.errors.iter().all(|error| error.is_none()) {
                // Nothing observable: never claim verification, never fake a pass.
                state.render_temporal_quality = match cfg.estimator {
                    Estimator::Pairwise => Json::obj(vec![
                        ("status", Json::Str("NOT_VERIFIED_NO_CONSENSUS".into())),
                        (
                            "note",
                            Json::Str(
                                "no frame had observable consensus tiles; render-domain \
                                 validation skipped rather than failed"
                                    .into(),
                            ),
                        ),
                    ]),
                    Estimator::Legacy => Json::obj(vec![
                        ("status", Json::Str("NOT_INDEPENDENT_LEGACY".into())),
                        (
                            "note",
                            Json::Str(
                                "legacy estimator defines no independent render-domain \
                                 targets; encoded-output audit remains the gate"
                                    .into(),
                            ),
                        ),
                    ]),
                };
                summary = None;
            }
        }

        if let Some(candidate) = summary {
            let score = (
                candidate.median_abs_error_ev.unwrap_or(f64::INFINITY),
                candidate.p95_abs_error_ev.unwrap_or(f64::INFINITY),
            );
            state.feedback_history.push(Json::obj(vec![
                ("iteration", Json::Int(iteration as i64)),
                (
                    "median_abs_error_ev",
                    optional_json(candidate.median_abs_error_ev),
                ),
                (
                    "p95_abs_error_ev",
                    optional_json(candidate.p95_abs_error_ev),
                ),
                (
                    "outlier_frames",
                    Json::Array(
                        candidate
                            .outlier_frames
                            .iter()
                            .map(|&index| Json::Int(index as i64))
                            .collect(),
                    ),
                ),
                (
                    "unobservable_frames",
                    Json::Array(
                        candidate
                            .unobservable_frames
                            .iter()
                            .map(|&index| Json::Int(index as i64))
                            .collect(),
                    ),
                ),
                ("passed", Json::Bool(candidate.passed)),
            ]));
            let accepted_better = match &best {
                None => true,
                Some(round) => match &round.summary {
                    None => true,
                    Some(old) => !old.passed,
                },
            };
            let better = score < best_score || (candidate.passed && accepted_better);
            println!(
                "render validation: median={} p95={} passed={} outliers={}",
                format_option(candidate.median_abs_error_ev),
                format_option(candidate.p95_abs_error_ev),
                candidate.passed,
                candidate.outlier_frames.len()
            );
            if better {
                if let Some(old) = best.take() {
                    remove_round(context, old.state.iteration);
                }
                best_score = score;
                best = Some(BestRound::new(
                    round_dir(context, iteration),
                    sequence,
                    snapshot(state, iteration),
                    Some(candidate.clone()),
                ));
            } else {
                remove_round(context, iteration);
            }
            if candidate.passed {
                break;
            }
            if let Some(old) = &previous {
                if feedback_diverged(old, &candidate) {
                    state.warnings.push(
                        "FEEDBACK_DIVERGED: aborted and retained the best candidate".to_string(),
                    );
                    break;
                }
            }
            if iteration >= MAX_FEEDBACK_UPDATES {
                break;
            }
            previous = Some(candidate.clone());
            let exposure = state.exposure.as_mut().expect("exposure set");
            let updated = feedback_update(exposure, &candidate, cfg.ev_limits).map_err(|err| {
                Error::new(
                    EXIT_RENDER_VALIDATION,
                    format!("feedback update failed: {err}"),
                )
            })?;
            if updated == 0 {
                break;
            }
            feedback_count += 1;
            // Legacy assigns the count before any later failure raise, so a
            // TARGET_NOT_REACHED/audit failure report still carries it.
            state.feedback_updates = feedback_count;
        } else {
            // Validation is not applicable (short sequence, disabled targets or
            // legacy estimator): accept this round without claiming a pass.
            if let Some(old) = best.take() {
                remove_round(context, old.state.iteration);
            }
            best = Some(BestRound::new(
                round_dir(context, iteration),
                sequence,
                snapshot(state, iteration),
                None,
            ));
            break;
        }
    }

    let best = best.ok_or_else(|| {
        Error::new(
            EXIT_RENDER_VALIDATION,
            "no accepted render round was produced",
        )
    })?;
    restore(state, best.state.clone());
    let mut targets_not_reached = false;
    if let Some(summary) = &best.summary {
        if !summary.passed {
            // Fail-open contract: keep the best round, flag it, publish.
            state.warnings.push(format!(
                "RENDER_TARGETS_NOT_REACHED: published the best of {} render \
                 round(s) (median abs error {} EV, p95 {} EV; outliers {}, \
                 unobservable {})",
                state.feedback_history.len() + 1,
                format_option(summary.median_abs_error_ev),
                format_option(summary.p95_abs_error_ev),
                summary.outlier_frames.len(),
                summary.unobservable_frames.len()
            ));
            targets_not_reached = true;
        }
    }

    // Adopt the accepted round as the final frame directory.
    let frames_dir = output.join("frames");
    if frames_dir.exists() {
        return Err(Error::new(
            EXIT_PUBLISH,
            format!(
                "refusing to adopt accepted frames: {} already exists",
                frames_dir.display()
            ),
        ));
    }
    fs::rename(&best.dir, &frames_dir).map_err(|err| {
        Error::new(
            EXIT_PUBLISH,
            format!(
                "cannot adopt accepted frames {} -> {}: {err}",
                best.dir.display(),
                frames_dir.display()
            ),
        )
    })?;
    if let Some(analysis_dir) = round_analysis_dir(context, best.state.iteration) {
        let final_dir = output.join("analysis/applied");
        let _ = fs::remove_dir_all(&final_dir);
        let _ = fs::rename(&analysis_dir, &final_dir);
    }
    let applied_logs = context
        .output
        .join(format!("logs/applied-iter{}", best.state.iteration));
    if applied_logs.exists() {
        let final_logs = output.join("logs/applied");
        let _ = fs::remove_dir_all(&final_logs);
        let _ = fs::rename(&applied_logs, &final_logs);
    }
    let final_sequence = best.sequence.clone().remap_output_paths("logs/applied");
    println!(
        "accepted render round {} with {} frame(s)",
        best.state.iteration + 1,
        final_sequence.frames.len()
    );

    // 8. Existing encode / probe / roundtrip / preview path.
    let stage = encode_and_validate(context, &final_sequence)?;

    // 9. Independent audit of the encoded linear master.
    let audit_boundaries: Vec<usize> = {
        let exposure = state.exposure.as_ref().expect("exposure set");
        let mut set: Vec<usize> = exposure
            .scene_cuts
            .iter()
            .map(|cut| cut.index)
            .chain(exposure.time_gaps.iter().copied())
            .filter(|index| *index > 0 && *index < final_sequence.frames.len())
            .collect();
        set.sort_unstable();
        set.dedup();
        set
    };
    match cfg.temporal_quality {
        TemporalQuality::Off => {
            state.encoded_temporal_quality = Json::obj(vec![
                ("status", Json::Str("SKIPPED".into())),
                (
                    "note",
                    Json::Str("temporal audit disabled by --temporal-quality off".into()),
                ),
            ]);
        }
        TemporalQuality::Report => {
            println!("independent encoded-video temporal audit...");
            let labels: Vec<String> = final_sequence
                .frames
                .iter()
                .map(|frame| frame.source_name.clone())
                .collect();
            let audit_config = AuditConfig {
                boundaries: &audit_boundaries,
                frame_labels: &labels,
            };
            match audit::audit_video(
                &options.ffmpeg,
                &options.ffprobe,
                &stage.temp_linear,
                &stage.linear_probe,
                final_sequence.frames.len(),
                &audit_config,
                output,
            ) {
                Ok(outcome) => {
                    println!("temporal audit status: {}", outcome.status);
                    let status = outcome.status.clone();
                    state.encoded_temporal_quality = outcome.summary;
                    if status == "FAIL" {
                        let events = state
                            .encoded_temporal_quality
                            .get("events")
                            .and_then(Json::as_array)
                            .map(|events| events.len())
                            .unwrap_or(0);
                        // Fail-open contract: the audit verdict is recorded and
                        // flagged; publication is never blocked by it.
                        state.warnings.push(format!(
                            "encoded temporal quality FAIL: {events} events >0.05 EV \
                             (published; flagged for review)"
                        ));
                        state.run_status = "COMPLETED_WITH_REVIEW".to_string();
                    }
                }
                Err(err) => {
                    state
                        .warnings
                        .push(format!("TEMPORAL_AUDIT_UNOBSERVABLE: {}", err.message));
                    state.encoded_temporal_quality = Json::obj(vec![
                        ("status", Json::Str("UNOBSERVABLE".into())),
                        ("error", Json::Str(err.message)),
                        ("detector_model", Json::Str(audit::DETECTOR_MODEL.into())),
                        ("events", Json::Array(vec![])),
                        ("segments", Json::Array(vec![])),
                    ]);
                }
            }
        }
    }

    // 10. Run status (never overwrites an explicit failure state).
    if state.run_status == "RUNNING" {
        state.run_status = if fallback_engaged || probe_flagged || targets_not_reached {
            "COMPLETED_WITH_REVIEW"
        } else {
            "OK"
        }
        .to_string();
    }
    state.feedback_updates = feedback_count;

    // 11. Reports, manifest, atomic publication.
    write_reports(context.options, context.output, state)?;
    let block = enabled_manifest_block(state);
    publish_run(context, &final_sequence, stage, block)
}

// ---------------------------------------------------------------------------
// Passes and probe
// ---------------------------------------------------------------------------

fn apply_pass(
    context: &RunContext<'_>,
    state: &mut RunState,
    iteration: usize,
) -> Result<(FrameSequence, Vec<FrameMeasurement>)> {
    let options = context.options;
    let dir = round_dir(context, iteration);
    let analysis_dir = round_analysis_dir(context, iteration).expect("analysis dir");
    let logs_dir = context.output.join(format!("logs/applied-iter{iteration}"));
    let baseline = state
        .baseline
        .clone()
        .ok_or_else(|| Error::new(EXIT_FRAME, "baseline measurements are missing"))?;
    let exposure = state.exposure.as_mut().expect("exposure set");
    let evs: Vec<f64> = exposure
        .frames
        .iter_mut()
        .map(|frame| {
            let ev = frame.applied_ev.unwrap_or(0.0);
            frame.render_ev = Some(ev);
            ev
        })
        .collect();
    let config = ProduceConfig {
        frames_dir: &dir,
        logs_dir: &logs_dir,
        analysis_dir: Some(&analysis_dir),
        exposure_ev: None,
        analysis_roi: Some(options.deflicker.roi),
        no_png: false,
        echo: true,
        echo_tag: "render ",
        converter: options.converter.as_deref(),
        converter_timeout: options.converter_timeout,
        frames_relative_dir: "frames",
        log_relative_dir: "logs/applied",
    };
    let sequence = frame::produce_with_evs(&options.native_tool, context.sources, &evs, &config)?;
    let mut rendered = Vec::with_capacity(sequence.frames.len());
    for (index, frame) in sequence.frames.iter().enumerate() {
        let applied = parse_measurement(frame, "applied")?;
        let tiles = applied.output_clip_fraction.len();
        if tiles == 0 {
            return Err(Error::new(
                EXIT_FRAME,
                format!("applied analysis for frame {} has no tiles", index + 1),
            ));
        }
        let base = baseline
            .get(index)
            .ok_or_else(|| Error::new(EXIT_FRAME, "baseline measurement count mismatch"))?;
        if base.output_clip_fraction.len() != tiles {
            return Err(Error::new(
                EXIT_FRAME,
                format!(
                    "applied analysis tile count {tiles} differs from baseline {} at frame {}",
                    base.output_clip_fraction.len(),
                    index + 1
                ),
            ));
        }
        let increase: f64 = (0..tiles)
            .map(|tile| {
                (applied.output_clip_fraction[tile] - base.output_clip_fraction[tile]).max(0.0)
            })
            .sum::<f64>()
            / tiles as f64;
        if let Some(estimate) = state
            .exposure
            .as_mut()
            .expect("exposure set")
            .frames
            .get_mut(index)
        {
            estimate.new_output_clipping_fraction = Some(increase);
        }
        rendered.push(applied);
    }
    Ok((sequence, rendered))
}

fn run_gain_probe(
    context: &RunContext<'_>,
    baseline: &FrameSequence,
    measurements: &[FrameMeasurement],
    cfg: &DeflickerConfig,
) -> Result<Json> {
    let options = context.options;
    let indices = representative_indices(measurements)
        .map_err(|err| Error::new(EXIT_FRAME, format!("gain probe selection failed: {err}")))?;
    let analysis_dir = context.output.join("analysis/probe");
    let meta_dir = analysis_dir.join("meta");
    let logs_dir = context.output.join("logs/probe");
    let mut errors: Vec<f64> = Vec::new();
    let mut reasons: Vec<String> = Vec::new();
    let mut tested: Vec<usize> = Vec::new();

    for index in indices {
        let base = &measurements[index];
        let k = base.valid.len();
        if !base.valid.iter().any(|valid| *valid) {
            reasons.push(format!("frame {index}: no valid baseline tiles"));
            continue;
        }
        tested.push(index);
        for ev in [-0.25, 0.25] {
            let config = ProduceConfig {
                frames_dir: &meta_dir,
                logs_dir: &logs_dir,
                analysis_dir: Some(&analysis_dir),
                exposure_ev: Some(ev),
                analysis_roi: Some(cfg.roi),
                no_png: true,
                echo: false,
                echo_tag: "probe ",
                converter: options.converter.as_deref(),
                converter_timeout: options.converter_timeout,
                frames_relative_dir: "analysis/probe/meta",
                log_relative_dir: "logs/probe",
            };
            let stem_index = index * 2 + if ev > 0.0 { 2 } else { 1 };
            let info = frame::produce_single(
                &options.native_tool,
                &context.sources[index],
                baseline.reference_wb,
                stem_index,
                &config,
            )
            .map_err(|err| {
                Error::new(
                    EXIT_GAIN_PROBE,
                    format!(
                        "gain probe failed on frame {index} at {ev:+.2} EV: {}",
                        err.message
                    ),
                )
            })?;
            let sidecar = parse_measurement_probe(&info, index)?;
            let rendered = &sidecar.measurement;
            if rendered.valid.len() != k {
                reasons.push(format!(
                    "frame {index} {ev:+.2}: analysis tile count changed"
                ));
                continue;
            }
            let good: Vec<usize> = (0..k)
                .filter(|&tile| {
                    base.valid[tile]
                        && rendered.valid[tile]
                        && rendered.log_luminance[tile].is_finite()
                })
                .collect();
            let required = 8usize.max((k as f64 * 0.25).ceil() as usize);
            if good.len() < required {
                reasons.push(format!(
                    "frame {index} {ev:+.2}: insufficient matching unsaturated tiles"
                ));
                continue;
            }
            for tile in good {
                errors.push((rendered.log_luminance[tile] - base.log_luminance[tile]) - ev);
            }
        }
    }

    if errors.is_empty() {
        reasons.push("no usable gain probes".to_string());
        return Ok(probe_json(false, &tested, None, None, 0, &reasons));
    }
    let mut sorted = errors.clone();
    sorted.sort_by(f64::total_cmp);
    let median = median_sorted(&sorted);
    let mut absolute: Vec<f64> = errors.iter().map(|value| value.abs()).collect();
    absolute.sort_by(f64::total_cmp);
    let p95 = percentile_sorted(&absolute, 95.0);
    let passed = median.abs() <= 0.02 && p95 <= 0.05 && reasons.is_empty();
    if !passed {
        reasons.push(format!(
            "gain response out of tolerance median={median:+.5} P95={p95:.5}"
        ));
    }
    Ok(probe_json(
        passed,
        &tested,
        Some(median),
        Some(p95),
        errors.len(),
        &reasons,
    ))
}

fn probe_json(
    passed: bool,
    tested: &[usize],
    median: Option<f64>,
    p95: Option<f64>,
    observations: usize,
    reasons: &[String],
) -> Json {
    Json::obj(vec![
        (
            "status",
            Json::Str(if passed { "PASS" } else { "FAIL" }.to_string()),
        ),
        ("passed", Json::Bool(passed)),
        (
            "tested_frames",
            Json::Array(
                tested
                    .iter()
                    .map(|&index| Json::Int(index as i64))
                    .collect(),
            ),
        ),
        ("median_error_ev", optional_json(median)),
        ("p95_abs_error_ev", optional_json(p95)),
        ("observations", Json::Int(observations as i64)),
        (
            "reasons",
            Json::Array(reasons.iter().cloned().map(Json::Str).collect()),
        ),
    ])
}

// ---------------------------------------------------------------------------
// Report and manifest payloads
// ---------------------------------------------------------------------------

struct RunState {
    config: Json,
    filenames: Vec<String>,
    capture_times: Vec<Option<String>>,
    exposure: Option<ExposureResult>,
    /// Baseline measurements, kept for per-tile clipping deltas.
    baseline: Option<Vec<FrameMeasurement>>,
    continuity: Json,
    source_discontinuities: Json,
    gain_probe: Json,
    feedback_history: Vec<Json>,
    feedback_updates: usize,
    encoded_temporal_quality: Json,
    render_temporal_quality: Json,
    global_reference_fallback: Json,
    warnings: Vec<String>,
    run_status: String,
    error: Option<String>,
    strength: f64,
}

impl RunState {
    fn new(options: &crate::cli::Options, input: &Path, output: &Path) -> RunState {
        let cfg = &options.deflicker;
        let config = Json::obj(vec![
            ("input", Json::Str(input.display().to_string())),
            ("output", Json::Str(output.display().to_string())),
            ("fps", Json::Str(options.fps.arg())),
            ("frame_count", Json::Int(0)),
            ("strength", Json::Float(cfg.strength)),
            ("period", Json::Int(cfg.period as i64)),
            ("brightness", Json::Str(cfg.brightness.as_str().to_string())),
            ("estimator", Json::Str(cfg.estimator.as_str().to_string())),
            (
                "ev_limits",
                cfg.ev_limits.map_or(Json::Null, |(low, high)| {
                    Json::Array(vec![Json::Float(low), Json::Float(high)])
                }),
            ),
            (
                "roi",
                Json::Array(cfg.roi.iter().map(|value| Json::Float(*value)).collect()),
            ),
            (
                "exclude_rois",
                Json::Array(
                    cfg.exclude_rois
                        .iter()
                        .map(|roi| {
                            Json::Array(roi.iter().map(|value| Json::Float(*value)).collect())
                        })
                        .collect(),
                ),
            ),
            (
                "steps",
                Json::Array(
                    cfg.steps
                        .iter()
                        .map(|step| {
                            Json::obj(vec![
                                ("start_frame", Json::Int(step.start_frame as i64)),
                                ("delta_ev", Json::Float(step.delta_ev)),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "temporal_quality",
                Json::Str(cfg.temporal_quality.as_str().to_string()),
            ),
            (
                "native_tool",
                Json::Str(options.native_tool.display().to_string()),
            ),
            ("ffmpeg", Json::Str(options.ffmpeg.display().to_string())),
            ("ffprobe", Json::Str(options.ffprobe.display().to_string())),
            (
                "converter_override",
                options
                    .converter
                    .as_ref()
                    .map(|path| Json::Str(path.clone()))
                    .unwrap_or(Json::Null),
            ),
            (
                "converter_timeout_override_seconds",
                options
                    .converter_timeout
                    .map(|seconds| Json::Int(seconds as i64))
                    .unwrap_or(Json::Null),
            ),
            (
                "defaults_provenance",
                Json::Str(crate::cli::DEFLICKER_DEFAULTS_PROVENANCE.to_string()),
            ),
        ]);
        RunState {
            config,
            filenames: vec![],
            capture_times: vec![],
            exposure: None,
            baseline: None,
            continuity: Json::Null,
            source_discontinuities: Json::Null,
            gain_probe: Json::Null,
            feedback_history: vec![],
            feedback_updates: 0,
            encoded_temporal_quality: Json::Null,
            global_reference_fallback: Json::Null,
            render_temporal_quality: Json::obj(vec![
                ("status", Json::Str("NOT_INDEPENDENT".into())),
                (
                    "note",
                    Json::Str(
                        "the legacy frozen-target check is self-consistent with whatever \
                         correction was proposed; the encoded-video audit is independent"
                            .into(),
                    ),
                ),
            ]),
            warnings: vec![],
            run_status: "RUNNING".to_string(),
            error: None,
            strength: cfg.strength,
        }
    }

    fn set_frame_count(&mut self, count: usize) {
        if let Json::Object(entries) = &mut self.config {
            if let Some((_, value)) = entries.iter_mut().find(|(key, _)| key == "frame_count") {
                *value = Json::Int(count as i64);
            }
        }
    }

    fn disabled_exposure(&mut self, cfg: &DeflickerConfig) {
        let n = self.filenames.len();
        let frames: Vec<FrameEstimate> = (0..n)
            .map(|index| FrameEstimate {
                index,
                status: "DISABLED".to_string(),
                common_residual_ev: None,
                requested_ev: Some(0.0),
                applied_ev: Some(0.0),
                cumulative_confirmed_step_ev: 0.0,
                valid_tile_count: 0,
                consensus_fraction: None,
                residual_mad_ev: None,
                warnings: vec![],
                render_target_error_ev: None,
                new_output_clipping_fraction: None,
                render_ev: Some(0.0),
                measured_level_ev: None,
                desired_level_ev: None,
                segment_id: 0,
            })
            .collect();
        self.exposure = Some(ExposureResult {
            frames,
            tile_log_luminance: vec![],
            tile_valid: vec![],
            consensus_mask: vec![],
            frozen_target: vec![],
            trusted_indices: vec![],
            trend_unconverged_tiles: 0,
            period_effective: cfg.period.min(n.max(1)),
            scene_cuts: vec![],
            time_gaps: vec![],
            pairwise_summary: Json::Null,
        });
        self.continuity = Json::obj(vec![
            ("coverage", Json::Str("not_executed".into())),
            (
                "note",
                Json::Str("--strength 0 does not execute the analysis pass".into()),
            ),
        ]);
        self.source_discontinuities = Json::Array(vec![]);
        self.gain_probe = Json::obj(vec![
            ("status", Json::Str("SKIPPED".into())),
            ("reason", Json::Str("disabled by --strength 0".into())),
        ]);
        self.encoded_temporal_quality = Json::obj(vec![
            ("status", Json::Str("SKIPPED".into())),
            (
                "note",
                Json::Str("--strength 0 does not execute the independent audit".into()),
            ),
        ]);
    }
}

fn write_reports(options: &crate::cli::Options, output: &Path, state: &mut RunState) -> Result<()> {
    let exposure = match &state.exposure {
        Some(exposure) => exposure,
        None => {
            // Pre-estimation failure: still leave a report describing the run.
            let root = Json::obj(vec![
                ("schema_version", Json::Int(1)),
                ("run_status", Json::Str(state.run_status.clone())),
                (
                    "execution_status",
                    Json::Str(execution_status(&state.run_status).into()),
                ),
                ("config", state.config.clone()),
                ("frames", Json::Array(vec![])),
                (
                    "error",
                    state.error.clone().map(Json::Str).unwrap_or(Json::Null),
                ),
                (
                    "warnings",
                    Json::Array(state.warnings.iter().cloned().map(Json::Str).collect()),
                ),
            ]);
            return write_atomic_json(&output.join(REPORT_NAME), &finite_json(&root)).map_err(
                |err| {
                    Error::new(
                        EXIT_PUBLISH,
                        format!(
                            "cannot write exposure report {}: {err}",
                            output.join(REPORT_NAME).display()
                        ),
                    )
                },
            );
        }
    };

    let mut report = ExposureReport::from_exposure(
        exposure,
        &engine_config(&options.deflicker, &state.continuity),
        &state.filenames,
        &state.capture_times,
    )
    .map_err(|err| Error::new(EXIT_PUBLISH, format!("cannot build exposure report: {err}")))?;
    report.run_status = state.run_status.clone();
    report.source_discontinuities = state.source_discontinuities.clone();
    report.gain_probe = state.gain_probe.clone();
    report.feedback_history = state.feedback_history.clone();
    report.encoded_temporal_quality = state.encoded_temporal_quality.clone();
    report.config = state.config.clone();

    let mut root = report.to_json();
    if let Json::Object(entries) = &mut root {
        entries.push((
            "execution_status".to_string(),
            Json::Str(execution_status(&state.run_status).into()),
        ));
        entries.push((
            "render_temporal_quality".to_string(),
            state.render_temporal_quality.clone(),
        ));
        entries.push((
            "global_reference_fallback".to_string(),
            state.global_reference_fallback.clone(),
        ));
        entries.push(("source_continuity".to_string(), state.continuity.clone()));
        entries.push((
            "feedback_updates".to_string(),
            Json::Int(state.feedback_updates as i64),
        ));
        entries.push((
            "decoder".to_string(),
            Json::obj(vec![
                (
                    "native_tool",
                    Json::Str(options.native_tool.display().to_string()),
                ),
                (
                    "analysis_domain",
                    Json::Str("gained ACEScg AP1/D60 scene-linear FP32".into()),
                ),
                ("saturation_mask_quality", Json::Str("camera_domain".into())),
                (
                    "analysis_projection",
                    Json::Str(
                        "REC.709 weights (0.2126/0.7152/0.0722) over ACEScg linear pixels".into(),
                    ),
                ),
            ]),
        ));
        entries.push((
            "clip_semantics".to_string(),
            Json::Str(
                "new_output_clipping_fraction = sum(max(0, applied.output_clip_fraction - \
                 baseline.output_clip_fraction)) / tiles; delivered-domain series only"
                    .into(),
            ),
        ));
        entries.push((
            "analysis".to_string(),
            Json::obj(vec![
                ("baseline_dir", Json::Str("analysis/baseline".into())),
                (
                    "applied_dir",
                    if state.strength == 0.0 {
                        Json::Null
                    } else {
                        Json::Str("analysis/applied".into())
                    },
                ),
            ]),
        ));
        entries.push((
            "warnings".to_string(),
            Json::Array(state.warnings.iter().cloned().map(Json::Str).collect()),
        ));
        entries.push((
            "error".to_string(),
            state.error.clone().map(Json::Str).unwrap_or(Json::Null),
        ));
    }

    write_atomic_json(&output.join(REPORT_NAME), &finite_json(&root)).map_err(|err| {
        Error::new(
            EXIT_PUBLISH,
            format!(
                "cannot write exposure report {}: {err}",
                output.join(REPORT_NAME).display()
            ),
        )
    })?;
    let csv = curve_csv(&report.frames);
    write_atomic_text(&output.join(CURVE_NAME), &csv).map_err(|err| {
        Error::new(
            EXIT_PUBLISH,
            format!(
                "cannot write exposure curve {}: {err}",
                output.join(CURVE_NAME).display()
            ),
        )
    })?;
    Ok(())
}

fn engine_config(cfg: &DeflickerConfig, continuity: &Json) -> ExposureConfig {
    let mut time_gaps = continuity_time_gaps(continuity);
    time_gaps.sort_unstable();
    time_gaps.dedup();
    ExposureConfig {
        period: cfg.period,
        strength: cfg.strength,
        steps: cfg.steps.clone(),
        brightness: cfg.brightness.as_str().to_string(),
        ev_limits: cfg.ev_limits,
        time_gaps,
        estimator: cfg.estimator.as_str().to_string(),
        exclude_rois: cfg.exclude_rois.clone(),
        scene_cut_median_ev: 0.20,
        scene_cut_tile_mad_ev: 0.08,
    }
}

fn continuity_time_gaps(continuity: &Json) -> Vec<usize> {
    continuity
        .get("time_gaps")
        .and_then(Json::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("index").and_then(Json::as_u64))
                .map(|index| index as usize)
                .collect()
        })
        .unwrap_or_default()
}

fn execution_status(run_status: &str) -> &'static str {
    match run_status {
        "OK" => "COMPLETED",
        "COMPLETED_WITH_REVIEW" => "COMPLETED_WITH_REVIEW",
        _ => "FAILED",
    }
}

fn curve_csv(frames: &[ReportFrame]) -> String {
    const FIELDS: [&str; 18] = [
        "index",
        "filename",
        "segment_id",
        "status",
        "valid_tile_count",
        "consensus_fraction",
        "residual_mad_ev",
        "common_residual_ev",
        "cumulative_confirmed_step_ev",
        "requested_ev",
        "applied_ev",
        "render_ev",
        "gain",
        "render_target_error_ev",
        "new_output_clipping_fraction",
        "measured_level_ev",
        "desired_level_ev",
        "warnings",
    ];
    let mut text = String::new();
    text.push_str(&FIELDS.join(","));
    text.push('\n');
    for frame in frames {
        let estimate = &frame.estimate;
        let row = [
            estimate.index.to_string(),
            csv_cell(&frame.filename),
            estimate.segment_id.to_string(),
            csv_cell(&estimate.status),
            estimate.valid_tile_count.to_string(),
            csv_number(estimate.consensus_fraction),
            csv_number(estimate.residual_mad_ev),
            csv_number(estimate.common_residual_ev),
            csv_number(Some(estimate.cumulative_confirmed_step_ev)),
            csv_number(estimate.requested_ev),
            csv_number(estimate.applied_ev),
            csv_number(estimate.render_ev),
            csv_number(estimate.gain()),
            csv_number(estimate.render_target_error_ev),
            csv_number(estimate.new_output_clipping_fraction),
            csv_number(estimate.measured_level_ev),
            csv_number(estimate.desired_level_ev),
            csv_cell(&estimate.warnings.join(";")),
        ];
        text.push_str(&row.join(","));
        text.push('\n');
    }
    text
}

fn csv_number(value: Option<f64>) -> String {
    match value.filter(|value| value.is_finite()) {
        Some(value) => format!("{value}"),
        None => String::new(),
    }
}

fn csv_cell(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn disabled_manifest_block(state: &RunState, frames: usize) -> Json {
    let _ = state;
    Json::obj(vec![
        ("enabled", Json::Bool(false)),
        ("strength", Json::Float(0.0)),
        ("run_status", Json::Str("OK".into())),
        ("frame_count", Json::Int(frames as i64)),
        (
            "status_counts",
            Json::obj(vec![("DISABLED", Json::Int(frames as i64))]),
        ),
        ("report", Json::Str(REPORT_NAME.into())),
        ("curve", Json::Str(CURVE_NAME.into())),
        (
            "note",
            Json::Str(
                "single pass; no analysis, gain probe, render validation or independent \
                 audit executed (REQ-011 regression path)"
                    .into(),
            ),
        ),
    ])
}

fn enabled_manifest_block(state: &RunState) -> Json {
    let status_counts = state
        .exposure
        .as_ref()
        .map(|exposure| {
            let mut counts = std::collections::BTreeMap::<String, i64>::new();
            for frame in &exposure.frames {
                *counts.entry(frame.status.clone()).or_default() += 1;
            }
            Json::Object(
                counts
                    .into_iter()
                    .map(|(key, value)| (key, Json::Int(value)))
                    .collect(),
            )
        })
        .unwrap_or(Json::Null);
    let audit_status = state
        .encoded_temporal_quality
        .get("status")
        .and_then(Json::as_str)
        .map(|status| Json::Str(status.to_string()))
        .unwrap_or(Json::Null);
    let probe_status = state
        .gain_probe
        .get("status")
        .and_then(Json::as_str)
        .map(|status| Json::Str(status.to_string()))
        .unwrap_or(Json::Null);
    Json::obj(vec![
        ("enabled", Json::Bool(true)),
        ("config", state.config.clone()),
        ("run_status", Json::Str(state.run_status.clone())),
        ("status_counts", status_counts),
        ("gain_probe_status", probe_status),
        ("audit_status", audit_status),
        (
            "render_temporal_quality",
            state.render_temporal_quality.clone(),
        ),
        (
            "global_reference_fallback",
            state.global_reference_fallback.clone(),
        ),
        ("feedback_updates", Json::Int(state.feedback_updates as i64)),
        ("report", Json::Str(REPORT_NAME.into())),
        ("curve", Json::Str(CURVE_NAME.into())),
        (
            "temporal_audit",
            match state
                .encoded_temporal_quality
                .get("status")
                .and_then(Json::as_str)
            {
                Some("SKIPPED") | None => Json::Null,
                _ => Json::Str("temporal_audit.json".into()),
            },
        ),
    ])
}

// ---------------------------------------------------------------------------
// Render-round bookkeeping
// ---------------------------------------------------------------------------

type FrameState = (Option<f64>, Option<f64>, Option<f64>, Option<f64>);

#[derive(Clone)]
struct RoundState {
    iteration: usize,
    frames: Vec<FrameState>,
}

struct BestRound {
    dir: PathBuf,
    sequence: FrameSequence,
    state: RoundState,
    summary: Option<ValidationSummary>,
}

impl BestRound {
    fn new(
        dir: PathBuf,
        sequence: FrameSequence,
        state: RoundState,
        summary: Option<ValidationSummary>,
    ) -> BestRound {
        BestRound {
            dir,
            sequence,
            state,
            summary,
        }
    }
}

fn snapshot(state: &RunState, iteration: usize) -> RoundState {
    let exposure = state.exposure.as_ref().expect("exposure set");
    RoundState {
        iteration,
        frames: exposure
            .frames
            .iter()
            .map(|frame| {
                (
                    frame.applied_ev,
                    frame.render_ev,
                    frame.render_target_error_ev,
                    frame.new_output_clipping_fraction,
                )
            })
            .collect(),
    }
}

fn restore(state: &mut RunState, snapshot: RoundState) {
    let exposure = state.exposure.as_mut().expect("exposure set");
    for (frame, values) in exposure.frames.iter_mut().zip(snapshot.frames) {
        frame.applied_ev = values.0;
        frame.render_ev = values.1;
        frame.render_target_error_ev = values.2;
        frame.new_output_clipping_fraction = values.3;
    }
}

fn round_dir(context: &RunContext<'_>, iteration: usize) -> PathBuf {
    context.partial_dir.join(format!("applied-iter{iteration}"))
}

fn round_analysis_dir(context: &RunContext<'_>, iteration: usize) -> Option<PathBuf> {
    Some(
        context
            .output
            .join(format!("analysis/applied-iter{iteration}")),
    )
}

fn remove_round(context: &RunContext<'_>, iteration: usize) {
    let _ = fs::remove_dir_all(round_dir(context, iteration));
    if let Some(dir) = round_analysis_dir(context, iteration) {
        let _ = fs::remove_dir_all(dir);
    }
    let _ = fs::remove_dir_all(context.output.join(format!("logs/applied-iter{iteration}")));
}

// ---------------------------------------------------------------------------
// Measurement and continuity helpers
// ---------------------------------------------------------------------------

fn parse_measurement(frame: &FrameInfo, pass: &str) -> Result<FrameMeasurement> {
    let path = frame.analysis_path.as_ref().ok_or_else(|| {
        Error::new(
            EXIT_FRAME,
            format!(
                "frame {} ({pass} pass) has no analysis sidecar",
                frame.index
            ),
        )
    })?;
    let text = fs::read_to_string(path).map_err(|err| {
        Error::new(
            EXIT_FRAME,
            format!(
                "cannot read {pass} analysis sidecar {}: {err}",
                path.display()
            ),
        )
    })?;
    let sidecar = AnalysisSidecar::parse(&text).map_err(|err| {
        Error::new(
            EXIT_FRAME,
            format!(
                "frame {} ({pass} pass) analysis sidecar is invalid ({}): {err}",
                frame.index,
                path.display()
            ),
        )
    })?;
    Ok(sidecar.measurement)
}

fn parse_measurement_probe(frame: &FrameInfo, index: usize) -> Result<AnalysisSidecar> {
    let path = frame.analysis_path.as_ref().ok_or_else(|| {
        Error::new(
            EXIT_GAIN_PROBE,
            format!("gain probe measurement for frame {index} has no analysis sidecar"),
        )
    })?;
    let text = fs::read_to_string(path).map_err(|err| {
        Error::new(
            EXIT_GAIN_PROBE,
            format!(
                "cannot read gain probe analysis sidecar {}: {err}",
                path.display()
            ),
        )
    })?;
    AnalysisSidecar::parse(&text).map_err(|err| {
        Error::new(
            EXIT_GAIN_PROBE,
            format!(
                "gain probe analysis sidecar for frame {index} is invalid ({}): {err}",
                path.display()
            ),
        )
    })
}

/// Legacy `continuity`: capture-time gaps from the median nominal interval
/// (gap_factor 1.5, >=3 samples) plus filename-step suspicion. Filename
/// suspicions are metadata only and never become audit boundaries.
struct Continuity {
    nominal_interval_s: Option<f64>,
    time_gaps: Vec<TimeGap>,
    filename_suspicions: Vec<Suspicion>,
    segments: Vec<(usize, usize)>,
    coverage: &'static str,
}

struct TimeGap {
    index: usize,
    gap_s: Option<f64>,
}

struct Suspicion {
    index: usize,
    filename_step: i64,
}

impl Continuity {
    fn time_gap_indices(&self) -> Vec<usize> {
        self.time_gaps.iter().map(|gap| gap.index).collect()
    }

    fn discontinuities_json(&self) -> Json {
        let mut entries: Vec<Json> = self
            .time_gaps
            .iter()
            .map(|gap| {
                Json::obj(vec![
                    ("index", Json::Int(gap.index as i64)),
                    ("kind", Json::Str("time_gap".into())),
                    ("gap_s", gap.gap_s.map_or(Json::Null, Json::Float)),
                ])
            })
            .collect();
        for suspicion in &self.filename_suspicions {
            entries.push(Json::obj(vec![
                ("index", Json::Int(suspicion.index as i64)),
                ("kind", Json::Str("filename_gap_suspicion".into())),
                ("filename_step", Json::Int(suspicion.filename_step)),
            ]));
        }
        Json::Array(entries)
    }

    fn to_json(&self) -> Json {
        Json::obj(vec![
            (
                "nominal_interval_s",
                self.nominal_interval_s.map_or(Json::Null, Json::Float),
            ),
            ("coverage", Json::Str(self.coverage.to_string())),
            (
                "time_gaps",
                Json::Array(
                    self.time_gaps
                        .iter()
                        .map(|gap| {
                            Json::obj(vec![
                                ("index", Json::Int(gap.index as i64)),
                                ("gap_s", gap.gap_s.map_or(Json::Null, Json::Float)),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "filename_suspicions",
                Json::Array(
                    self.filename_suspicions
                        .iter()
                        .map(|suspicion| {
                            Json::obj(vec![
                                ("index", Json::Int(suspicion.index as i64)),
                                ("filename_step", Json::Int(suspicion.filename_step)),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "segments",
                Json::Array(
                    self.segments
                        .iter()
                        .map(|(start, end)| {
                            Json::Array(vec![Json::Int(*start as i64), Json::Int(*end as i64)])
                        })
                        .collect(),
                ),
            ),
            (
                "note",
                Json::Str(
                    "filename-gap suspicion is report metadata only; audit boundaries are \
                     scene cuts union capture-time gaps"
                        .into(),
                ),
            ),
        ])
    }
}

fn continuity(names: &[String], capture_times: &[Option<String>]) -> Continuity {
    let times: Vec<Option<f64>> = capture_times
        .iter()
        .map(|time| time.as_deref().and_then(parse_capture_time))
        .collect();
    let mut deltas: Vec<Option<f64>> = Vec::with_capacity(times.len().saturating_sub(1));
    for index in 1..times.len() {
        deltas.push(match (times[index], times[index - 1]) {
            (Some(current), Some(previous)) => Some(current - previous),
            _ => None,
        });
    }
    let mut positive: Vec<f64> = deltas
        .iter()
        .flatten()
        .copied()
        .filter(|delta| *delta > 0.0)
        .collect();
    positive.sort_by(f64::total_cmp);
    let nominal = if positive.is_empty() {
        None
    } else {
        Some(positive[positive.len() / 2])
    };
    let mut time_gaps = vec![];
    if let Some(nominal) = nominal {
        if positive.len() >= 3 {
            for (offset, delta) in deltas.iter().enumerate() {
                let index = offset + 1;
                let is_gap = match delta {
                    Some(value) => *value <= 0.0 || *value > 1.5 * nominal,
                    None => true,
                };
                if is_gap {
                    time_gaps.push(TimeGap {
                        index,
                        gap_s: *delta,
                    });
                }
            }
        }
    }
    let mut filename_suspicions = vec![];
    for index in 1..names.len() {
        if let (Some(previous), Some(current)) = (
            filename_number(&names[index - 1]),
            filename_number(&names[index]),
        ) {
            let step = current - previous;
            if step != 1 {
                filename_suspicions.push(Suspicion {
                    index,
                    filename_step: step,
                });
            }
        }
    }
    let gap_indices: std::collections::BTreeSet<usize> =
        time_gaps.iter().map(|gap| gap.index).collect();
    let mut segments = vec![];
    let mut start = 0;
    for index in 1..names.len() {
        if gap_indices.contains(&index) {
            segments.push((start, index));
            start = index;
        }
    }
    segments.push((start, names.len()));
    Continuity {
        nominal_interval_s: nominal,
        time_gaps,
        filename_suspicions,
        segments,
        coverage: if positive.is_empty() {
            "filename_only"
        } else {
            "capture_time"
        },
    }
}

fn parse_capture_time(text: &str) -> Option<f64> {
    let text = text.trim();
    let (date, time) = if let Some((date, time)) = text.split_once(' ') {
        (date, time)
    } else if let Some((date, time)) = text.split_once('T') {
        (date, time)
    } else {
        return None;
    };
    let date_parts: Vec<&str> = date
        .split(|ch| ch == ':' || ch == '-' || ch == '/')
        .collect();
    if date_parts.len() != 3 {
        return None;
    }
    let year: i64 = date_parts[0].parse().ok()?;
    let month: i64 = date_parts[1].parse().ok()?;
    let day: i64 = date_parts[2].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let time = time.trim_end_matches('Z');
    let (clock, fraction) = match time.split_once(['.', ',']) {
        Some((clock, fraction)) => (clock, fraction),
        None => (time, ""),
    };
    let time_parts: Vec<&str> = clock.split(':').collect();
    if time_parts.len() < 2 || time_parts.len() > 3 {
        return None;
    }
    let hour: f64 = time_parts[0].parse().ok()?;
    let minute: f64 = time_parts[1].parse().ok()?;
    let second: f64 = if time_parts.len() == 3 {
        time_parts[2].parse().ok()?
    } else {
        0.0
    };
    if !(0.0..60.0).contains(&minute) || !(0.0..61.0).contains(&second) {
        return None;
    }
    let mut seconds = second;
    if !fraction.is_empty() {
        let mut scale = 0.1;
        for ch in fraction.chars() {
            let digit = ch.to_digit(10)? as f64;
            seconds += digit * scale;
            scale *= 0.1;
        }
    }
    let days = days_from_civil(year, month, day);
    Some(days as f64 * 86400.0 + hour * 3600.0 + minute * 60.0 + seconds)
}

/// Howard Hinnant's days_from_civil (proleptic Gregorian, no timezone).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month_index = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * month_index + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Last numeric run in the stem (`(\d+)\D*$`), as legacy `_filename_number`.
fn filename_number(name: &str) -> Option<i64> {
    let stem = match name.rfind('.') {
        Some(index) => &name[..index],
        None => name,
    };
    let bytes = stem.as_bytes();
    let mut end = bytes.len();
    while end > 0 && !bytes[end - 1].is_ascii_digit() {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    let mut start = end;
    while start > 0 && bytes[start - 1].is_ascii_digit() {
        start -= 1;
    }
    stem[start..end].parse().ok()
}

// ---------------------------------------------------------------------------
// Small numeric and I/O helpers
// ---------------------------------------------------------------------------

fn median_sorted(sorted: &[f64]) -> f64 {
    percentile_sorted(sorted, 50.0)
}

fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let position = (sorted.len() - 1) as f64 * p / 100.0;
    let low = position.floor() as usize;
    let high = position.ceil() as usize;
    sorted[low] + (sorted[high] - sorted[low]) * (position - low as f64)
}

fn optional_json(value: Option<f64>) -> Json {
    value
        .filter(|value| value.is_finite())
        .map_or(Json::Null, Json::Float)
}

fn format_option(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite())
        .map(|value| format!("{value:.5}"))
        .unwrap_or_else(|| "none".to_string())
}

/// Statuses that already name a failure class survive the error mapping.
fn failure_run_status(current: &str, code: i32) -> String {
    match current {
        "GAIN_PROBE_FAILED" | "FAILED" => current.to_string(),
        _ => status_for_exit(code).to_string(),
    }
}

fn status_for_exit(code: i32) -> &'static str {
    match code {
        EXIT_GAIN_PROBE => "GAIN_PROBE_FAILED",
        _ => "FAILED",
    }
}

fn print_sequence_summary(sequence: &FrameSequence) {
    println!(
        "frozen white balance: {} from {} (applied to every later frame)",
        frame::format_wb(sequence.reference_wb),
        sequence.reference_frame
    );
    println!(
        "frames complete: {} frames of {}x{} in {:.2}s",
        sequence.frames.len(),
        sequence.width,
        sequence.height,
        sequence.total_duration.as_secs_f64()
    );
}

fn write_atomic_json(path: &Path, value: &Json) -> std::io::Result<()> {
    write_atomic_text(path, &json::to_pretty_string(value))
}

fn write_atomic_text(path: &Path, text: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("report");
    let temp = parent.join(format!(".{name}.tmp-{}", std::process::id()));
    fs::write(&temp, text)?;
    match fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&temp);
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_time_parses_exif_and_iso() {
        let exif = parse_capture_time("2026:11:08 20:09:53").expect("exif");
        let iso = parse_capture_time("2026-11-08T20:09:53").expect("iso");
        assert_eq!(exif, iso);
        assert!(parse_capture_time("not a time").is_none());
        let fractional = parse_capture_time("2026:11:08 20:09:53.5").expect("fraction");
        assert!((fractional - exif - 0.5).abs() < 1e-9);
    }

    #[test]
    fn filename_number_matches_legacy_tail_rule() {
        assert_eq!(filename_number("TIMELAPSE_0709.DNG"), Some(709));
        assert_eq!(filename_number("frame_0012"), Some(12));
        assert_eq!(filename_number("clip 10 v2.dng"), Some(2));
        assert_eq!(filename_number("no-digits.DNG"), None);
    }

    #[test]
    fn continuity_reports_gaps_and_filename_suspicion() {
        let names: Vec<String> = (0..6)
            .map(|index| format!("frame_{:04}.DNG", index * 2))
            .collect();
        let times: Vec<Option<String>> = vec![
            Some("2026:11:08 20:00:00".into()),
            Some("2026:11:08 20:00:10".into()),
            Some("2026:11:08 20:00:20".into()),
            Some("2026:11:08 20:00:30".into()),
            Some("2026:11:08 20:01:30".into()),
            Some("2026:11:08 20:01:40".into()),
        ];
        let continuity = continuity(&names, &times);
        assert_eq!(continuity.coverage, "capture_time");
        assert_eq!(continuity.nominal_interval_s, Some(10.0));
        assert_eq!(continuity.time_gap_indices(), vec![4]);
        assert_eq!(continuity.filename_suspicions.len(), 5);
        assert_eq!(continuity.segments, vec![(0, 4), (4, 6)]);
    }

    #[test]
    fn continuity_degrades_to_filename_only_without_times() {
        let names = vec!["a1.DNG".to_string(), "a2.DNG".to_string()];
        let times: Vec<Option<String>> = vec![None, None];
        let continuity = continuity(&names, &times);
        assert_eq!(continuity.coverage, "filename_only");
        assert!(continuity.time_gaps.is_empty());
        assert!(continuity.filename_suspicions.is_empty());
    }

    #[test]
    fn failure_status_never_keeps_a_success_value() {
        for success in ["RUNNING", "OK", "COMPLETED_WITH_REVIEW"] {
            assert_eq!(failure_run_status(success, 40), "FAILED");
            assert_eq!(failure_run_status(success, 50), "FAILED");
            assert_eq!(failure_run_status(success, 70), "FAILED");
            assert_eq!(failure_run_status(success, 81), "GAIN_PROBE_FAILED");
            assert_eq!(failure_run_status(success, 82), "FAILED");
        }
        for failure in ["GAIN_PROBE_FAILED", "FAILED"] {
            assert_eq!(failure_run_status(failure, 40), failure);
            assert_eq!(failure_run_status(failure, 70), failure);
        }
    }

    #[test]
    fn curve_csv_header_matches_legacy_fields() {
        let frames = vec![ReportFrame {
            estimate: FrameEstimate {
                index: 0,
                status: "OK".into(),
                common_residual_ev: None,
                requested_ev: Some(-0.5),
                applied_ev: Some(-0.5),
                cumulative_confirmed_step_ev: 0.0,
                valid_tile_count: 96,
                consensus_fraction: Some(1.0),
                residual_mad_ev: None,
                warnings: vec!["W1".into(), "W2".into()],
                render_target_error_ev: None,
                new_output_clipping_fraction: None,
                render_ev: Some(-0.5),
                measured_level_ev: None,
                desired_level_ev: None,
                segment_id: 0,
            },
            filename: "frame,1.DNG".into(),
            capture_time: None,
        }];
        let csv = curve_csv(&frames);
        let mut lines = csv.lines();
        assert_eq!(
            lines.next().expect("header"),
            "index,filename,segment_id,status,valid_tile_count,consensus_fraction,\
             residual_mad_ev,common_residual_ev,cumulative_confirmed_step_ev,requested_ev,\
             applied_ev,render_ev,gain,render_target_error_ev,new_output_clipping_fraction,\
             measured_level_ev,desired_level_ev,warnings"
        );
        assert_eq!(
            lines.next().expect("row"),
            "0,\"frame,1.DNG\",0,OK,96,1,,,0,-0.5,-0.5,-0.5,0.7071067811865476,,,,,W1;W2"
        );
    }
}
