//! CLI report boundary around the unified graph runtime. The graph owns all
//! preparation, measurement/solve, rendering, encoding and event-driven cleanup.
//! Legacy report files remain until the CLI/log surface migration.

use crate::cli::{DeflickerConfig, WhiteBalance};
use crate::error::{Error, Result, EXIT_GAIN_PROBE, EXIT_PUBLISH};
use crate::frame::{self, FrameSequence};
use crate::json::{self, Json};
use crate::{publish_run, RunContext};
use rawtl_linear_video::deflicker::measurement::FrameMeasurement;
use rawtl_linear_video::deflicker::report::{finite_json, ExposureReport, ReportFrame};

use rawtl_linear_video::deflicker::{ExposureConfig, ExposureResult, FrameEstimate};
use std::fs;
use std::path::Path;
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
    println!("found {} frame(s) in numeric order:", sources.len());
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
    fs::create_dir_all(&context.partial_dir)
        .map_err(|e| Error::new(EXIT_PUBLISH, e.to_string()))?;
    let result = crate::graph_run::run(&context)?;
    state.filenames = result
        .sequence
        .frames
        .iter()
        .map(|f| f.source_name.clone())
        .collect();
    state.capture_times = result
        .sequence
        .frames
        .iter()
        .map(|f| f.capture_time.clone())
        .collect();
    state.exposure = result.exposure;
    if state.exposure.is_none() {
        state.disabled_exposure(&options.deflicker);
    }
    print_sequence_summary(&result.sequence, options.white_balance);
    state.encoded_temporal_quality = Json::obj(vec![("status", Json::Str("SKIPPED".into()))]);
    state.run_status = "OK".into();
    write_reports(options, output, state)?;
    let block = if options.deflicker.enabled() {
        enabled_manifest_block(state)
    } else {
        disabled_manifest_block(state, result.sequence.frames.len())
    };
    publish_run(&context, &result.sequence, result.stage, block)
}

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
                source_sha256: String::new(),
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
                analysis_highlight_increase_fraction: None,
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
            plan_quality: Default::default(),
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
                ("schema_version", Json::Int(2)),
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
                "analysis_highlight_increase_fraction = sum(max(0, applied.analysis_highlight_fraction - \
                 baseline.analysis_highlight_fraction)) / tiles; analysis-only AP1 threshold occupancy; not final video clipping"
                    .into(),
            ),
        ));
        entries.push((
            "analysis".to_string(),
            Json::obj(vec![(
                "transport",
                Json::Str("MemoryFile; native stdout protocol v1".into()),
            )]),
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
        "analysis_highlight_increase_fraction",
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
            csv_number(estimate.analysis_highlight_increase_fraction),
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

fn print_sequence_summary(sequence: &FrameSequence, white_balance: WhiteBalance) {
    if white_balance.freezes_reference() {
        println!(
            "frozen white balance: {} from {} (applied to every later frame)",
            frame::format_wb(sequence.reference_wb),
            sequence.reference_frame
        );
    } else {
        println!(
            "per-frame white balance: every frame decoded with its own as-shot neutral \
             (no --white-balance override)"
        );
    }
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
                source_sha256: String::new(),
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
                analysis_highlight_increase_fraction: None,
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
             applied_ev,render_ev,gain,render_target_error_ev,analysis_highlight_increase_fraction,\
             measured_level_ev,desired_level_ev,warnings"
        );
        assert_eq!(
            lines.next().expect("row"),
            "0,\"frame,1.DNG\",0,OK,96,1,,,0,-0.5,-0.5,-0.5,0.7071067811865476,,,,,W1;W2"
        );
    }
}

/// Capture-time discontinuities are part of the accepted solver input. Prepared
/// metadata is used by the graph adapter; filenames alone invent no boundary.
pub fn graph_time_gaps(
    sources: &[crate::order::SourceFrame],
    capture_times: &[Option<String>],
) -> Vec<usize> {
    continuity(
        &sources.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
        capture_times,
    )
    .time_gap_indices()
}

// Kept for development audit callers; not part of graph execution.
pub(crate) fn record_audit_failure(output: &Path, err: &Error) -> Result<Json> {
    let summary = audit_failure_summary(err);
    write_atomic_json(&output.join("temporal_audit.json"), &summary)
        .map_err(|e| Error::new(EXIT_PUBLISH, e.to_string()))?;
    Ok(summary)
}

pub(crate) fn audit_failure_summary(err: &Error) -> Json {
    let status = if err.message.starts_with("TEMPORAL_AUDIT_UNOBSERVABLE:") {
        "UNOBSERVABLE"
    } else {
        "ERROR"
    };
    Json::obj(vec![
        ("status", Json::Str(status.into())),
        ("error", Json::Str(err.message.clone())),
        ("detector_model", Json::Str(crate::audit::DETECTOR_MODEL.into())),
        ("events", Json::Array(vec![])),
        ("segments", Json::Array(vec![])),
    ])
}
