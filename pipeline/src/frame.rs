//! Per-frame production: invoke the accepted native producer, verify its
//! outputs, and enforce the selected white balance mode.
//!
//! One function serves every pass: the deflicker-disabled single pass uses the
//! exact historical argument shape (no optional flags), while deflicker passes
//! add `--exposure-ev`, `--emit-analysis` and optionally `--no-png`.
//!
//! White balance modes: `first-frame` decodes frame 1 with its as-shot neutral
//! and passes that triple to every later frame as `--white-balance`;
//! `per-frame` never passes the flag, so every frame decodes with its own RAW
//! as-shot neutral. In both modes a sidecar that reports `as-shot` must have
//! applied exactly that frame's `as_shot_neutral`.

use crate::cli::WhiteBalance;
use crate::error::{Error, Result, EXIT_FRAME};
use crate::json::{self, Json};
use crate::order::SourceFrame;
use crate::proc;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const WB_SOURCE_AS_SHOT: &str = "as-shot";
pub const WB_SOURCE_OVERRIDE: &str = "override";

#[derive(Debug, Clone)]
pub struct FrameInfo {
    pub index: usize,
    pub source_name: String,
    pub source_sha256: Option<String>,
    pub width: u32,
    pub height: u32,
    pub applied_wb: [f64; 3],
    pub wb_source: String,
    pub as_shot_neutral: [f64; 3],
    pub observed_min: Option<f64>,
    pub observed_max: Option<f64>,
    pub quantization: Option<Json>,
    pub capture_time: Option<String>,
    pub exposure_ev: f64,
    pub analysis_path: Option<PathBuf>,
    pub png_relative: String,
    pub metadata_relative: String,
    pub log_relative: String,
    pub duration: Duration,
}

#[derive(Debug, Clone)]
pub struct FrameSequence {
    pub frames: Vec<FrameInfo>,
    pub width: u32,
    pub height: u32,
    pub reference_wb: [f64; 3],
    pub reference_frame: String,
    pub total_duration: Duration,
}

impl FrameSequence {
    /// Rewrite the recorded relative output paths after a pass directory is
    /// adopted as the final `frames/` directory.
    pub fn remap_output_paths(mut self, log_dir_rel: &str) -> FrameSequence {
        for frame in &mut self.frames {
            let stem = format!("frame_{:06}", frame.index);
            frame.png_relative = format!("frames/{stem}.png");
            frame.metadata_relative = format!("frames/{stem}.json");
            frame.log_relative = format!("{log_dir_rel}/{stem}.log");
        }
        self
    }
}

pub struct ProduceConfig<'a> {
    /// Destination for the PNG and metadata positional paths.
    pub frames_dir: &'a Path,
    /// Destination for per-frame transcripts.
    pub logs_dir: &'a Path,
    /// When set, ask the producer for an analysis sidecar in this directory.
    pub analysis_dir: Option<&'a Path>,
    /// `Some(ev)` emits `--exposure-ev EV`; `None` keeps the historical shape.
    pub exposure_ev: Option<f64>,
    /// `Some(roi)` emits `--analysis-roi X,Y,W,H`.
    pub analysis_roi: Option<[f64; 4]>,
    pub no_png: bool,
    pub echo: bool,
    pub echo_tag: &'a str,
    /// How the sequence derives its camera neutral. `first-frame` passes the
    /// frozen frame-1 triple to later frames; `per-frame` never passes
    /// `--white-balance`.
    pub white_balance: WhiteBalance,
    pub converter: Option<&'a str>,
    pub converter_timeout: Option<u64>,
    /// Recorded relative frame directory (for the manifest).
    pub frames_relative_dir: &'a str,
    /// Recorded relative transcript directory (for the manifest).
    pub log_relative_dir: &'a str,
}

pub fn produce(
    native_tool: &Path,
    sources: &[SourceFrame],
    config: &ProduceConfig<'_>,
) -> Result<FrameSequence> {
    produce_inner(native_tool, sources, None, config)
}

/// Produce one frame per explicit EV; the flag is always emitted so the
/// producer's analysis sidecar names the applied gain.
pub fn produce_with_evs(
    native_tool: &Path,
    sources: &[SourceFrame],
    evs: &[f64],
    config: &ProduceConfig<'_>,
) -> Result<FrameSequence> {
    if evs.len() != sources.len() {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "per-frame EV count {} differs from frame count {}",
                evs.len(),
                sources.len()
            ),
        ));
    }
    if evs.iter().any(|ev| !ev.is_finite()) {
        return Err(Error::new(EXIT_FRAME, "non-finite exposure EV"));
    }
    produce_inner(native_tool, sources, Some(evs), config)
}

fn produce_inner(
    native_tool: &Path,
    sources: &[SourceFrame],
    evs: Option<&[f64]>,
    config: &ProduceConfig<'_>,
) -> Result<FrameSequence> {
    fs::create_dir_all(config.frames_dir).map_err(|err| {
        Error::new(
            EXIT_FRAME,
            format!(
                "cannot create frame directory {}: {err}",
                config.frames_dir.display()
            ),
        )
    })?;
    fs::create_dir_all(config.logs_dir).map_err(|err| {
        Error::new(
            EXIT_FRAME,
            format!(
                "cannot create log directory {}: {err}",
                config.logs_dir.display()
            ),
        )
    })?;
    if let Some(dir) = config.analysis_dir {
        fs::create_dir_all(dir).map_err(|err| {
            Error::new(
                EXIT_FRAME,
                format!("cannot create analysis directory {}: {err}", dir.display()),
            )
        })?;
    }

    let total = sources.len();
    let mut infos = Vec::with_capacity(total);
    let mut reference_wb: Option<[f64; 3]> = None;
    let freezes_reference = config.white_balance.freezes_reference();
    let mut width = 0u32;
    let mut height = 0u32;
    let run_start = std::time::Instant::now();

    for (offset, source) in sources.iter().enumerate() {
        let index = offset + 1;
        let ev = match evs {
            Some(evs) => Some(evs[offset]),
            None => config.exposure_ev,
        };
        // `per-frame` decodes every frame with its own as-shot neutral, so the
        // producer never receives an override in any pass.
        let override_wb = if freezes_reference {
            reference_wb
        } else {
            None
        };
        let info = run_frame(native_tool, source, index, total, override_wb, ev, config)?;
        if index == 1 {
            width = info.width;
            height = info.height;
            reference_wb = Some(info.applied_wb);
        } else {
            if info.width != width || info.height != height {
                return Err(Error::new(
                    EXIT_FRAME,
                    format!(
                        "frame {index}/{total} ({}) has dimensions {}x{} but the sequence is {}x{}",
                        source.name, info.width, info.height, width, height
                    ),
                ));
            }
            // Only the frozen mode requires cross-frame equality; `per-frame`
            // already validated as-shot equality per frame in `run_frame`.
            if freezes_reference {
                let reference = reference_wb.expect("reference set by first frame");
                if info.wb_source != WB_SOURCE_OVERRIDE {
                    return Err(Error::new(
                        EXIT_FRAME,
                        format!(
                            "frame {index}/{total} ({}) should use the frozen override white balance, metadata reports {:?}",
                            source.name, info.wb_source
                        ),
                    ));
                }
                if !wb_close(info.applied_wb, reference) {
                    return Err(Error::new(
                        EXIT_FRAME,
                        format!(
                            "frame {index}/{total} ({}) applied white balance {:?} differs from the frozen reference {:?}",
                            source.name, info.applied_wb, reference
                        ),
                    ));
                }
            }
        }
        if config.echo {
            println!(
                "{}{index:>3}/{total} {} -> {} ({:.2}s, WB {}, EV {:+.3})",
                config.echo_tag,
                source.name,
                Path::new(&info.png_relative)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("?"),
                info.duration.as_secs_f64(),
                info.wb_source,
                info.exposure_ev
            );
        }
        infos.push(info);
    }

    let reference_wb = reference_wb.expect("at least one frame");
    let reference_frame = infos
        .first()
        .map(|frame| frame.source_name.clone())
        .unwrap_or_default();

    Ok(FrameSequence {
        frames: infos,
        width,
        height,
        reference_wb,
        reference_frame,
        total_duration: run_start.elapsed(),
    })
}

/// Produce one frame for an analysis probe. `override_wb` is `Some` only in
/// `first-frame` mode (the frozen triple); `per-frame` passes `None`, so the
/// probe decodes with the frame's own as-shot neutral like every other pass.
/// `stem_index` names the outputs, so consecutive probes never collide.
pub fn produce_single(
    native_tool: &Path,
    source: &SourceFrame,
    override_wb: Option<[f64; 3]>,
    stem_index: usize,
    config: &ProduceConfig<'_>,
) -> Result<FrameInfo> {
    fs::create_dir_all(config.frames_dir).map_err(|err| {
        Error::new(
            EXIT_FRAME,
            format!(
                "cannot create frame directory {}: {err}",
                config.frames_dir.display()
            ),
        )
    })?;
    fs::create_dir_all(config.logs_dir).map_err(|err| {
        Error::new(
            EXIT_FRAME,
            format!(
                "cannot create log directory {}: {err}",
                config.logs_dir.display()
            ),
        )
    })?;
    if let Some(dir) = config.analysis_dir {
        fs::create_dir_all(dir).map_err(|err| {
            Error::new(
                EXIT_FRAME,
                format!("cannot create analysis directory {}: {err}", dir.display()),
            )
        })?;
    }
    run_frame(
        native_tool,
        source,
        stem_index,
        1,
        override_wb,
        config.exposure_ev,
        config,
    )
}

fn run_frame(
    native_tool: &Path,
    source: &SourceFrame,
    index: usize,
    total: usize,
    override_wb: Option<[f64; 3]>,
    exposure_ev: Option<f64>,
    config: &ProduceConfig<'_>,
) -> Result<FrameInfo> {
    let stem = format!("frame_{index:06}");
    let png = config.frames_dir.join(format!("{stem}.png"));
    let metadata = config.frames_dir.join(format!("{stem}.json"));
    let log_path = config.logs_dir.join(format!("{stem}.log"));
    let analysis = config
        .analysis_dir
        .map(|dir| dir.join(format!("{stem}.analysis.json")));

    let mut args: Vec<String> = vec![
        source.path.display().to_string(),
        png.display().to_string(),
        metadata.display().to_string(),
    ];
    if let Some(reference) = override_wb {
        args.push("--white-balance".to_string());
        args.push(format_wb(reference));
    }
    if let Some(converter_path) = config.converter {
        args.push("--converter".to_string());
        args.push(converter_path.to_string());
    }
    if let Some(seconds) = config.converter_timeout {
        args.push("--converter-timeout".to_string());
        args.push(seconds.to_string());
    }
    if let Some(ev) = exposure_ev {
        args.push("--exposure-ev".to_string());
        args.push(format_ev(ev));
    }
    if let Some(path) = &analysis {
        args.push("--emit-analysis".to_string());
        args.push(path.display().to_string());
    }
    if let Some(roi) = config.analysis_roi {
        args.push("--analysis-roi".to_string());
        args.push(format!(
            "{},{},{},{}",
            format_axis(roi[0]),
            format_axis(roi[1]),
            format_axis(roi[2]),
            format_axis(roi[3])
        ));
    }
    if config.no_png {
        args.push("--no-png".to_string());
    }

    let command = proc::display_command(native_tool, &args);
    let captured = proc::run_capture(native_tool, &args, EXIT_FRAME)?;
    let log_text = format!(
        "command: {command}\nexit: {} ({:.3}s)\nstdout:\n{}\nstderr:\n{}\n",
        proc::exit_description(&captured.status),
        captured.duration.as_secs_f64(),
        captured.stdout,
        captured.stderr
    );
    write_log(&log_path, &log_text)?;

    if !captured.status.success() {
        let stderr = captured.stderr.trim();
        let detail = if stderr.is_empty() {
            "(no stderr)".to_string()
        } else {
            proc::tail(stderr, 1200)
        };
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) failed: {}; stderr: {detail}; \
                 frame outputs and transcript retained at {} and {}; no video was produced",
                source.name,
                proc::exit_description(&captured.status),
                config.frames_dir.display(),
                log_path.display()
            ),
        ));
    }

    let mut required: Vec<(&Path, &str)> = vec![(&metadata, "metadata JSON")];
    if !config.no_png {
        required.push((&png, "PNG"));
    }
    if let Some(path) = &analysis {
        required.push((path, "analysis sidecar"));
    }
    for (path, kind) in required {
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_file() && metadata.len() > 0 => {}
            Ok(_) => {
                return Err(Error::new(
                    EXIT_FRAME,
                    format!(
                        "frame {index}/{total} ({}) reported success but {kind} is empty: {}",
                        source.name,
                        path.display()
                    ),
                ))
            }
            Err(err) => {
                return Err(Error::new(
                    EXIT_FRAME,
                    format!(
                    "frame {index}/{total} ({}) reported success but {kind} is missing ({}): {err}",
                    source.name,
                    path.display()
                ),
                ))
            }
        }
    }

    let text = fs::read_to_string(&metadata).map_err(|err| {
        Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) metadata is unreadable ({}): {err}",
                source.name,
                metadata.display()
            ),
        )
    })?;
    let sidecar = json::parse(&text).map_err(|err| {
        Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) metadata is not valid JSON ({}): {err}",
                source.name,
                metadata.display()
            ),
        )
    })?;

    let frame_width = sidecar
        .get("width")
        .and_then(Json::as_u32)
        .ok_or_else(|| missing_field(index, total, &source.name, "width"))?;
    let frame_height = sidecar
        .get("height")
        .and_then(Json::as_u32)
        .ok_or_else(|| missing_field(index, total, &source.name, "height"))?;
    if frame_width == 0 || frame_height == 0 {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) reports an empty image {}x{}",
                source.name, frame_width, frame_height
            ),
        ));
    }

    let schema_version = sidecar
        .get("schema_version")
        .and_then(Json::as_u64)
        .ok_or_else(|| missing_field(index, total, &source.name, "schema_version"))?;
    if schema_version != 1 {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) has unsupported metadata schema_version {schema_version}",
                source.name
            ),
        ));
    }

    let applied_wb = parse_wb(
        &sidecar,
        "applied_white_balance",
        index,
        total,
        &source.name,
    )?;
    let as_shot_neutral = parse_wb(&sidecar, "as_shot_neutral", index, total, &source.name)?;
    let wb_source = sidecar
        .get("white_balance_source")
        .and_then(Json::as_str)
        .ok_or_else(|| missing_field(index, total, &source.name, "white_balance_source"))?
        .to_string();

    if override_wb.is_none() {
        if wb_source != WB_SOURCE_AS_SHOT {
            return Err(Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) should use its as-shot white balance, metadata reports {wb_source:?}",
                    source.name
                ),
            ));
        }
    } else if wb_source != WB_SOURCE_OVERRIDE {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) should use the frozen override white balance, metadata reports {wb_source:?}",
                source.name
            ),
        ));
    }

    // A producer that reports `as-shot` must have applied exactly that frame's
    // as_shot_neutral. This covers the first frame of `first-frame` mode, every
    // frame of `per-frame` mode and every probe: a lying sidecar can never
    // smuggle a different neutral into the sequence or into the frozen
    // reference.
    if wb_source == WB_SOURCE_AS_SHOT && !wb_close(applied_wb, as_shot_neutral) {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) reports as-shot white balance but applied {:?} differs from its own as_shot_neutral {:?}",
                source.name, applied_wb, as_shot_neutral
            ),
        ));
    }

    let source_field = sidecar
        .get("source")
        .and_then(Json::as_str)
        .ok_or_else(|| missing_field(index, total, &source.name, "source"))?;
    // The native producer resolves the input path (symlinks included), so
    // compare canonical identities rather than the literal argument text.
    let (expected_source, reported_source) =
        canonical_source_pair(source_field, &source.path).map_err(|err| {
            Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) metadata source {source_field:?} cannot be resolved against the requested input: {err}",
                    source.name
                ),
            )
        })?;
    if reported_source != expected_source {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) metadata source {source_field:?} does not resolve to the requested input {}",
                source.name,
                expected_source.display()
            ),
        ));
    }

    let source_sha256 = sidecar
        .get("source_sha256")
        .and_then(Json::as_str)
        .map(str::to_string);
    let observed_min = sidecar.get("observed_scene_min").and_then(Json::as_f64);
    let observed_max = sidecar.get("observed_scene_max").and_then(Json::as_f64);
    let quantization = sidecar.get("quantization").cloned();

    let reported_ev = sidecar
        .get("exposure_ev")
        .and_then(Json::as_f64)
        .ok_or_else(|| missing_field(index, total, &source.name, "exposure_ev"))?;
    if !reported_ev.is_finite() {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) reports a non-finite exposure_ev",
                source.name
            ),
        ));
    }
    let requested_ev = exposure_ev.unwrap_or(0.0);
    if reported_ev != requested_ev {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) reports exposure_ev {reported_ev} but {requested_ev} was requested",
                source.name
            ),
        ));
    }

    let capture_time = match sidecar.get("capture_time") {
        None | Some(Json::Null) => None,
        Some(Json::Str(text)) if !text.is_empty() => Some(text.clone()),
        Some(_) => {
            return Err(Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) capture_time must be a non-empty string or null",
                    source.name
                ),
            ))
        }
    };

    if let Some(path) = &analysis {
        let reported = sidecar
            .get("analysis_sidecar")
            .and_then(Json::as_str)
            .ok_or_else(|| missing_field(index, total, &source.name, "analysis_sidecar"))?;
        if Path::new(reported) != path {
            return Err(Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) reports analysis_sidecar {reported:?} but {} was requested",
                    source.name,
                    path.display()
                ),
            ));
        }
        verify_analysis_sidecar(
            path,
            &source.name,
            index,
            total,
            &source_sha256,
            requested_ev,
        )?;
    }

    Ok(FrameInfo {
        index,
        source_name: source.name.clone(),
        source_sha256,
        width: frame_width,
        height: frame_height,
        applied_wb,
        wb_source,
        as_shot_neutral,
        observed_min,
        observed_max,
        quantization,
        capture_time,
        exposure_ev: reported_ev,
        analysis_path: analysis,
        png_relative: format!("{}/frame_{:06}.png", config.frames_relative_dir, index),
        metadata_relative: format!("{}/frame_{:06}.json", config.frames_relative_dir, index),
        log_relative: format!("{}/{stem}.log", config.log_relative_dir),
        duration: captured.duration,
    })
}

/// Cheap integrity checks for the analysis sidecar. Full measurement parsing is
/// the engine's responsibility; here we prove the artifact belongs to this
/// frame and this gain request.
fn verify_analysis_sidecar(
    path: &Path,
    name: &str,
    index: usize,
    total: usize,
    source_sha256: &Option<String>,
    requested_ev: f64,
) -> Result<()> {
    let text = fs::read_to_string(path).map_err(|err| {
        Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({name}) analysis sidecar is unreadable ({}): {err}",
                path.display()
            ),
        )
    })?;
    let sidecar = json::parse(&text).map_err(|err| {
        Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({name}) analysis sidecar is not valid JSON ({}): {err}",
                path.display()
            ),
        )
    })?;
    if sidecar.get("schema_version").and_then(Json::as_u64) != Some(1)
        || sidecar.get("kind").and_then(Json::as_str) != Some("rawtl-analysis-sidecar")
    {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({name}) analysis sidecar has an unsupported schema ({}): {}",
                path.display(),
                proc::tail(&text, 200)
            ),
        ));
    }
    let sidecar_sha = sidecar.get("source_sha256").and_then(Json::as_str);
    match (source_sha256, sidecar_sha) {
        (Some(expected), Some(actual)) if expected == actual => {}
        _ => {
            return Err(Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({name}) analysis sidecar source_sha256 does not match the frame metadata"
                ),
            ))
        }
    }
    let sidecar_ev = sidecar
        .get("analysis")
        .and_then(|analysis| analysis.get("exposure_ev"))
        .and_then(Json::as_f64);
    match sidecar_ev {
        Some(reported) if sidecar_ev_matches(reported, requested_ev) => {}
        _ => {
            return Err(Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({name}) analysis sidecar exposure_ev is {sidecar_ev:?} but {requested_ev} was requested"
                ),
            ))
        }
    }
    Ok(())
}

/// The analysis sidecar is serialized by the accepted producer with 10
/// significant decimal digits, so a fractional EV does not round-trip through
/// f64 exactly. Accept the sidecar value only when it is the exact requested
/// value, the producer's 10-significant-digit representation of it, or within
/// half a decimal ulp of that representation (decimal rounding-mode ties at the
/// last digit). A materially different gain (>= 1e-9 here) is still rejected.
/// The native metadata JSON keeps the full-precision exact check.
fn sidecar_ev_matches(reported: f64, requested: f64) -> bool {
    if reported == requested {
        return true;
    }
    if !reported.is_finite() || !requested.is_finite() {
        return false;
    }
    // Exact 10-significant-digit formatting, as parsed back from the sidecar.
    if let Ok(quantized) = format!("{requested:.9e}").parse::<f64>() {
        if reported == quantized {
            return true;
        }
    }
    let magnitude = requested.abs();
    if magnitude == 0.0 {
        return false;
    }
    // Half a decimal ulp at 10 significant digits. The 1e-12 relative guard
    // only absorbs log10/powf rounding, not real gain differences.
    let exponent = magnitude.log10().floor();
    let ulp = 10f64.powf(exponent - 9.0);
    (reported - requested).abs() <= 0.5 * ulp * (1.0 + 1e-12)
}

fn write_log(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text).map_err(|err| {
        Error::new(
            EXIT_FRAME,
            format!("cannot write frame transcript {}: {err}", path.display()),
        )
    })
}

fn canonical_source_pair(reported: &str, expected: &Path) -> std::io::Result<(PathBuf, PathBuf)> {
    Ok((fs::canonicalize(expected)?, fs::canonicalize(reported)?))
}

fn missing_field(index: usize, total: usize, name: &str, field: &str) -> Error {
    Error::new(
        EXIT_FRAME,
        format!("frame {index}/{total} ({name}) metadata is missing or invalid field {field:?}"),
    )
}

fn parse_wb(
    sidecar: &Json,
    field: &str,
    index: usize,
    total: usize,
    name: &str,
) -> Result<[f64; 3]> {
    let value = sidecar
        .get(field)
        .and_then(Json::as_array)
        .ok_or_else(|| missing_field(index, total, name, field))?;
    if value.len() != 3 {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({name}) field {field:?} must contain three camera-neutral values"
            ),
        ));
    }
    let mut out = [0.0f64; 3];
    for (channel, item) in value.iter().enumerate() {
        let number = item
            .as_f64()
            .filter(|value| value.is_finite() && *value > 0.0);
        match number {
            Some(number) => out[channel] = number,
            None => {
                return Err(Error::new(
                    EXIT_FRAME,
                    format!(
                        "frame {index}/{total} ({name}) field {field:?} channel {channel} is not a finite positive number"
                    ),
                ))
            }
        }
    }
    Ok(out)
}

pub fn format_wb(wb: [f64; 3]) -> String {
    format!("{},{},{}", wb[0], wb[1], wb[2])
}

fn format_ev(ev: f64) -> String {
    format!("{ev}")
}

fn format_axis(value: f64) -> String {
    format!("{value}")
}

fn wb_close(a: [f64; 3], b: [f64; 3]) -> bool {
    // The native tool echoes the decimal values it received; the shortest
    // round-tripping decimal representation preserves the exact f64 bits.
    // Requiring exact equality catches any silent per-frame WB drift.
    a.iter().zip(b.iter()).all(|(left, right)| left == right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_white_balance_without_precision_loss() {
        let wb = [0.605080831408776, 1.0, 0.48250460405156537];
        let text = format_wb(wb);
        assert_eq!(text.matches(',').count(), 2);
        let parsed: Vec<f64> = text.split(',').map(|v| v.parse().unwrap()).collect();
        assert_eq!(parsed, wb.to_vec());
    }

    #[test]
    fn white_balance_comparison_is_strict() {
        let reference = [0.605080831408776, 1.0, 0.48250460405156537];
        assert!(wb_close(reference, reference));
        assert!(!wb_close(
            reference,
            [0.605080831408777, 1.0, 0.48250460405156537]
        ));
    }

    #[test]
    fn sidecar_ev_accepts_only_producer_precision() {
        let requested = 0.0920620700999999f64;
        let producer: f64 = "0.0920620701".parse().expect("producer decimal");
        assert_ne!(producer, requested);
        assert!(sidecar_ev_matches(producer, requested));
        assert!(sidecar_ev_matches(requested, requested));
        assert!(sidecar_ev_matches(-producer, -requested));
        assert!(sidecar_ev_matches(0.0, 0.0));
        assert!(!sidecar_ev_matches(0.0, requested));
        // Materially wrong gains are still rejected (well beyond half ulp).
        assert!(!sidecar_ev_matches(requested + 1e-6, requested));
        assert!(!sidecar_ev_matches(requested + 1e-9, requested));
        assert!(!sidecar_ev_matches(requested + 1e-10, requested));
        // 10 significant digits at this magnitude -> decimal step 1e-11,
        // so the half-ulp bound is 5e-12.
        assert!(sidecar_ev_matches(requested + 4e-12, requested));
    }

    #[test]
    fn formats_ev_and_axes_round_trip() {
        for ev in [0.0, 0.25, -0.25, 0.1, 1.0 / 3.0] {
            assert_eq!(format_ev(ev).parse::<f64>().unwrap(), ev);
            assert_eq!(format_axis(ev).parse::<f64>().unwrap(), ev);
        }
    }

    #[cfg(unix)]
    #[test]
    fn canonical_source_comparison_resolves_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(format!("frame-source-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");
        let real = dir.join("real.DNG");
        fs::write(&real, b"data").expect("write real file");
        let link = dir.join("link.DNG");
        symlink(&real, &link).expect("create symlink");
        let (expected, reported) =
            canonical_source_pair(link.to_str().expect("utf8"), &real).expect("canonicalize");
        assert_eq!(expected, reported);
        assert_eq!(expected, fs::canonicalize(&real).expect("canonical"));
    }
}
