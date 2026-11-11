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
use crate::error::{Error, Result, EXIT_FRAME, EXIT_PUBLISH};
use crate::json::{self, Json};
use crate::order::SourceFrame;
use crate::proc;
use crate::transport::{
    self, DataEnvelope, DiskArtifact, MemoryFile, MetadataDoc, Payload, Provenance,
    ANALYSIS_SIDECAR, FRAME_METADATA, FRAME_PNG,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const WB_SOURCE_AS_SHOT: &str = "as-shot";
pub const WB_SOURCE_OVERRIDE: &str = "override";

/// Media types used when a stage hands a file artifact to the next stage.
/// The transport contract only requires a nonempty type; these name the actual
/// encoding of each produced file.
const JSON_MEDIA_TYPE: &str = "application/json; charset=utf-8";
const PNG_MEDIA_TYPE: &str = "image/png";
/// Every produced PNG is scene-linear ACEScg (AP1 primaries, D60 white) with
/// no OETF, independently of how the native producer encodes it on disk.
const PNG_COLOR_SPACE: &str = "ACEScg AP1/D60 scene-linear";
const PNG_COLOR_TRANSFER: &str = "linear";

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
    /// Typed references handed to later stages. The PNG is absent for
    /// `--no-png` measurement passes; the analysis sidecar is absent unless it
    /// was requested. The metadata envelope holds the validated sidecar bytes.
    pub png_artifact: Option<DiskArtifact>,
    pub png_envelope: Option<DataEnvelope>,
    pub metadata_artifact: DiskArtifact,
    pub metadata_envelope: DataEnvelope,
    pub analysis_artifact: Option<DiskArtifact>,
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
    /// Rewrite the recorded relative output paths and re-point the typed disk
    /// references after a pass directory is adopted as the final `frames/`
    /// directory. A move is not an in-place conversion: the producer
    /// references are immutable, so adoption builds new references/envelopes at
    /// the adopted absolute paths while keeping the same semantic type,
    /// metadata and payload kind; the producer envelope becomes their lineage
    /// parent.
    pub fn adopt_output_locations(
        mut self,
        frames_dir: &Path,
        analysis_dir: Option<&Path>,
        log_dir_rel: &str,
    ) -> Result<FrameSequence> {
        for frame in &mut self.frames {
            let stem = format!("frame_{:06}", frame.index);
            let png = frames_dir.join(format!("{stem}.png"));
            let metadata = frames_dir.join(format!("{stem}.json"));
            frame.png_relative = format!("frames/{stem}.png");
            frame.metadata_relative = format!("frames/{stem}.json");
            frame.log_relative = format!("{log_dir_rel}/{stem}.log");

            if let Some(artifact) = frame.png_artifact.take() {
                let moved = relocate_artifact(&artifact, &png)?;
                frame.png_envelope = match frame.png_envelope.take() {
                    Some(envelope) => Some(repoint_envelope(
                        &envelope,
                        Payload::DiskArtifact(moved.clone()),
                    )?),
                    None => None,
                };
                frame.png_artifact = Some(moved);
            }
            frame.metadata_artifact = relocate_artifact(&frame.metadata_artifact, &metadata)?;

            match (&frame.analysis_artifact, analysis_dir) {
                (Some(artifact), Some(dir)) => {
                    let path = dir.join(format!("{stem}.analysis.json"));
                    frame.analysis_artifact = Some(relocate_artifact(artifact, &path)?);
                    frame.analysis_path = Some(path);
                }
                (None, _) => frame.analysis_path = None,
                (Some(artifact), None) => {
                    return Err(Error::new(
                        EXIT_PUBLISH,
                        format!(
                            "cannot adopt analysis sidecar reference {}: no adopted analysis directory",
                            artifact.path().display()
                        ),
                    ))
                }
            }
        }
        Ok(self)
    }
}

fn relocate_artifact(artifact: &DiskArtifact, path: &Path) -> Result<DiskArtifact> {
    DiskArtifact::new(
        path,
        artifact.media_type(),
        artifact.size(),
        artifact.hash().map(str::to_string),
    )
    .map_err(|err| {
        Error::new(
            EXIT_PUBLISH,
            format!(
                "cannot re-point adopted artifact {} -> {}: {err}",
                artifact.path().display(),
                path.display()
            ),
        )
    })
}

/// Rebuild one envelope around a re-pointed payload. The new envelope keeps
/// the semantic type and immutable metadata verbatim and records the producer
/// envelope as an additional lineage parent, mirroring `DataEnvelope::patch`.
fn repoint_envelope(envelope: &DataEnvelope, payload: Payload) -> Result<DataEnvelope> {
    let mut parents = envelope.provenance().parent_value_ids().to_vec();
    if !parents.iter().any(|id| id == envelope.value_id()) {
        parents.push(envelope.value_id().to_string());
    }
    let provenance = Provenance::new(
        envelope.provenance().producer(),
        envelope.provenance().source_hashes().to_vec(),
        parents,
    )
    .map_err(|err| {
        Error::new(
            EXIT_PUBLISH,
            format!("cannot re-point adopted envelope provenance: {err}"),
        )
    })?;
    DataEnvelope::new(
        envelope.semantic_type().to_string(),
        payload,
        envelope.metadata().clone(),
        provenance,
    )
    .map_err(|err| {
        Error::new(
            EXIT_PUBLISH,
            format!("cannot re-point adopted envelope: {err}"),
        )
    })
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

    // The metadata sidecar is produced on disk, but its content is handed to
    // this stage as a typed envelope: reference -> load -> MemoryFile ->
    // semantic validation, and only then the historical field parsing.
    let metadata_artifact =
        DiskArtifact::new(&metadata, JSON_MEDIA_TYPE, None, None).map_err(|err| {
            Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) metadata reference is invalid ({}): {err}",
                    source.name,
                    metadata.display()
                ),
            )
        })?;
    let loaded = load_json_sidecar(&metadata_artifact).map_err(|err| match err {
        SidecarLoadError::Read(err) => Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) metadata is unreadable ({}): {err}",
                source.name,
                metadata.display()
            ),
        ),
        SidecarLoadError::InvalidJson(err) => Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) metadata is not valid JSON ({}): {err}",
                source.name,
                metadata.display()
            ),
        ),
        SidecarLoadError::Semantic(err) => Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) metadata is unreadable ({}): {err}",
                source.name,
                metadata.display()
            ),
        ),
    })?;
    let sidecar = loaded.json;

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

    let source_sha256 = sidecar
        .get("source_sha256")
        .and_then(Json::as_str)
        .map(str::to_string);
    let metadata_envelope = frame_metadata_envelope(
        loaded.file,
        schema_version,
        frame_width,
        frame_height,
        source_sha256.as_deref(),
        index,
        total,
        &source.name,
    )?;
    let (png_artifact, png_envelope) = if config.no_png {
        (None, None)
    } else {
        let artifact = DiskArtifact::new(&png, PNG_MEDIA_TYPE, None, None).map_err(|err| {
            Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) PNG reference is invalid ({}): {err}",
                    source.name,
                    png.display()
                ),
            )
        })?;
        let envelope = frame_png_envelope(
            &artifact,
            frame_width,
            frame_height,
            index,
            total,
            &source.name,
        )?;
        (Some(artifact), Some(envelope))
    };

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

    let analysis_artifact = match &analysis {
        Some(path) => {
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
            let artifact =
                DiskArtifact::new(path, JSON_MEDIA_TYPE, None, None).map_err(|err| {
                    Error::new(
                        EXIT_FRAME,
                        format!(
                            "frame {index}/{total} ({}) analysis sidecar reference is invalid ({}): {err}",
                            source.name,
                            path.display()
                        ),
                    )
                })?;
            verify_analysis_sidecar(
                &artifact,
                &source.name,
                index,
                total,
                &source_sha256,
                requested_ev,
            )?;
            Some(artifact)
        }
        None => None,
    };

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
        png_artifact,
        png_envelope,
        metadata_artifact,
        metadata_envelope,
        analysis_artifact,
    })
}

/// Cheap integrity checks for the analysis sidecar. Full measurement parsing is
/// the engine's responsibility; here we prove the artifact belongs to this
/// frame and this gain request. The content is first handed over as a typed
/// `ANALYSIS_SIDECAR` envelope (loaded bytes plus metadata whose source hash is
/// bound to the frame's provenance); the historical value checks then run on
/// the same bytes.
fn verify_analysis_sidecar(
    artifact: &DiskArtifact,
    name: &str,
    index: usize,
    total: usize,
    source_sha256: &Option<String>,
    requested_ev: f64,
) -> Result<()> {
    let path = artifact.path();
    let loaded = analysis_sidecar_envelope(artifact, source_sha256.as_deref()).map_err(|err| {
        match err {
            SidecarLoadError::Read(err) => Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({name}) analysis sidecar is unreadable ({}): {err}",
                    path.display()
                ),
            ),
            SidecarLoadError::InvalidJson(err) => Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({name}) analysis sidecar is not valid JSON ({}): {err}",
                    path.display()
                ),
            ),
            SidecarLoadError::Semantic(transport::TransportError::MissingMetadata(field))
            | SidecarLoadError::Semantic(transport::TransportError::InvalidMetadataValue(field))
                if field == "source.sha256" =>
            {
                analysis_sidecar_hash_mismatch(index, total, name)
            }
            SidecarLoadError::Semantic(transport::TransportError::SourceHashMismatch(_)) => {
                analysis_sidecar_hash_mismatch(index, total, name)
            }
            SidecarLoadError::Semantic(transport::TransportError::MissingMetadata(_))
            | SidecarLoadError::Semantic(transport::TransportError::InvalidMetadataValue(_)) => {
                let text = fs::read_to_string(path).unwrap_or_default();
                Error::new(
                    EXIT_FRAME,
                    format!(
                        "frame {index}/{total} ({name}) analysis sidecar has an unsupported schema ({}): {}",
                        path.display(),
                        proc::tail(&text, 200)
                    ),
                )
            }
            SidecarLoadError::Semantic(err) => Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({name}) analysis sidecar violates the transport contract ({}): {err}",
                    path.display()
                ),
            ),
        }
    })?;
    let sidecar = &loaded.json;
    if sidecar.get("schema_version").and_then(Json::as_u64) != Some(1)
        || sidecar.get("kind").and_then(Json::as_str) != Some("rawtl-analysis-sidecar")
    {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({name}) analysis sidecar has an unsupported schema ({}): {}",
                path.display(),
                proc::tail(&loaded.text, 200)
            ),
        ));
    }
    let sidecar_sha = sidecar.get("source_sha256").and_then(Json::as_str);
    match (source_sha256, sidecar_sha) {
        (Some(expected), Some(actual)) if expected == actual => {}
        _ => return Err(analysis_sidecar_hash_mismatch(index, total, name)),
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

fn analysis_sidecar_hash_mismatch(index: usize, total: usize, name: &str) -> Error {
    Error::new(
        EXIT_FRAME,
        format!(
            "frame {index}/{total} ({name}) analysis sidecar source_sha256 does not match the frame metadata"
        ),
    )
}

/// Failure while turning a disk reference into validated in-memory sidecar
/// content. Read/parse failures keep the historical per-call messages; the
/// semantic variant is a transport-contract rejection that did not exist
/// before the typed handoff.
#[derive(Debug)]
pub(crate) enum SidecarLoadError {
    Read(std::io::Error),
    InvalidJson(String),
    Semantic(transport::TransportError),
}

struct LoadedSidecar {
    file: MemoryFile,
    text: String,
    json: Json,
}

fn load_json_sidecar(
    artifact: &DiskArtifact,
) -> std::result::Result<LoadedSidecar, SidecarLoadError> {
    let file = transport::load(artifact).map_err(|err| match err {
        transport::TransportError::Io { message, .. } => {
            SidecarLoadError::Read(std::io::Error::other(message))
        }
        other => SidecarLoadError::Semantic(other),
    })?;
    let text = std::str::from_utf8(file.bytes())
        .map_err(|err| {
            SidecarLoadError::Read(std::io::Error::new(std::io::ErrorKind::InvalidData, err))
        })?
        .to_string();
    let json = json::parse(&text).map_err(SidecarLoadError::InvalidJson)?;
    Ok(LoadedSidecar { file, text, json })
}

/// Validate the native metadata sidecar as a typed `FRAME_METADATA` envelope.
/// The envelope metadata carries the structural facts the sidecar promises and
/// the source hash, which must also be present in provenance.
fn frame_metadata_envelope(
    file: MemoryFile,
    schema_version: u64,
    width: u32,
    height: u32,
    source_sha256: Option<&str>,
    index: usize,
    total: usize,
    name: &str,
) -> Result<DataEnvelope> {
    let mut fields = vec![
        (
            "source.schema_version".to_string(),
            Json::Int(schema_version as i64),
        ),
        ("source.width".to_string(), Json::Int(width as i64)),
        ("source.height".to_string(), Json::Int(height as i64)),
    ];
    if let Some(hash) = source_sha256 {
        fields.push(("source.sha256".to_string(), Json::Str(hash.to_string())));
    }
    let metadata =
        MetadataDoc::new(fields).map_err(|err| metadata_contract_error(err, index, total, name))?;
    let provenance = Provenance::new(
        "dng-linear-frame",
        source_sha256.map(str::to_string).into_iter().collect(),
        Vec::new(),
    )
    .map_err(|err| metadata_contract_error(err, index, total, name))?;
    DataEnvelope::new(
        FRAME_METADATA,
        Payload::MemoryFile(file),
        metadata,
        provenance,
    )
    .map_err(|err| metadata_contract_error(err, index, total, name))
}

/// Validate the produced PNG as a typed `FRAME_PNG` disk reference.
fn frame_png_envelope(
    artifact: &DiskArtifact,
    width: u32,
    height: u32,
    index: usize,
    total: usize,
    name: &str,
) -> Result<DataEnvelope> {
    let metadata = MetadataDoc::new(vec![
        ("source.width".to_string(), Json::Int(width as i64)),
        ("source.height".to_string(), Json::Int(height as i64)),
        (
            "color.space".to_string(),
            Json::Str(PNG_COLOR_SPACE.to_string()),
        ),
        (
            "color.transfer".to_string(),
            Json::Str(PNG_COLOR_TRANSFER.to_string()),
        ),
    ])
    .map_err(|err| metadata_contract_error(err, index, total, name))?;
    let provenance = Provenance::new("dng-linear-frame", Vec::new(), Vec::new())
        .map_err(|err| metadata_contract_error(err, index, total, name))?;
    DataEnvelope::new(
        FRAME_PNG,
        Payload::DiskArtifact(artifact.clone()),
        metadata,
        provenance,
    )
    .map_err(|err| metadata_contract_error(err, index, total, name))
}

fn metadata_contract_error(
    err: transport::TransportError,
    index: usize,
    total: usize,
    name: &str,
) -> Error {
    match err {
        transport::TransportError::MissingMetadata(field)
        | transport::TransportError::InvalidMetadataValue(field) => {
            let field = match field.as_str() {
                "source.sha256" => "source_sha256".to_string(),
                other => other.strip_prefix("source.").unwrap_or(other).to_string(),
            };
            missing_field(index, total, name, &field)
        }
        transport::TransportError::SourceHashMismatch(hash) => Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({name}) metadata source_sha256 {hash:?} is not bound to the processed source"
            ),
        ),
        other => Error::new(
            EXIT_FRAME,
            format!("frame {index}/{total} ({name}) metadata violates the transport contract: {other}"),
        ),
    }
}

/// Load and validate an analysis sidecar as a typed `ANALYSIS_SIDECAR`
/// envelope. The sidecar's declared source hash must be bound to the source
/// hash of the frame that requested it. Callers parse the returned text with
/// the existing measurement parser.
pub(crate) fn analysis_sidecar_envelope(
    artifact: &DiskArtifact,
    expected_source_hash: Option<&str>,
) -> std::result::Result<LoadedAnalysisSidecar, SidecarLoadError> {
    let loaded = load_json_sidecar(artifact)?;
    let mut fields = Vec::new();
    let schema_version = loaded
        .json
        .get("schema_version")
        .and_then(Json::as_u64)
        .ok_or_else(|| {
            SidecarLoadError::Semantic(transport::TransportError::MissingMetadata(
                "source.schema_version".into(),
            ))
        })?;
    fields.push((
        "source.schema_version".to_string(),
        Json::Int(schema_version as i64),
    ));
    let kind = loaded
        .json
        .get("kind")
        .and_then(Json::as_str)
        .ok_or_else(|| {
            SidecarLoadError::Semantic(transport::TransportError::MissingMetadata(
                "source.kind".into(),
            ))
        })?;
    fields.push(("source.kind".to_string(), Json::Str(kind.to_string())));
    let hash = loaded
        .json
        .get("source_sha256")
        .and_then(Json::as_str)
        .ok_or_else(|| {
            SidecarLoadError::Semantic(transport::TransportError::MissingMetadata(
                "source.sha256".into(),
            ))
        })?;
    fields.push(("source.sha256".to_string(), Json::Str(hash.to_string())));
    let metadata = MetadataDoc::new(fields).map_err(SidecarLoadError::Semantic)?;
    let provenance = Provenance::new(
        "dng-linear-frame",
        expected_source_hash
            .map(str::to_string)
            .into_iter()
            .collect(),
        Vec::new(),
    )
    .map_err(SidecarLoadError::Semantic)?;
    let envelope = DataEnvelope::new(
        ANALYSIS_SIDECAR,
        Payload::MemoryFile(loaded.file),
        metadata,
        provenance,
    )
    .map_err(SidecarLoadError::Semantic)?;
    Ok(LoadedAnalysisSidecar {
        envelope,
        text: loaded.text,
        json: loaded.json,
    })
}

pub(crate) struct LoadedAnalysisSidecar {
    envelope: DataEnvelope,
    text: String,
    json: Json,
}

impl LoadedAnalysisSidecar {
    /// The validated in-memory bytes this envelope handed to the consumer.
    pub fn bytes(&self) -> &[u8] {
        match self.envelope.payload() {
            Payload::MemoryFile(file) => file.bytes(),
            _ => unreachable!("analysis sidecar envelopes carry a MemoryFile payload"),
        }
    }
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

    #[cfg(unix)]
    struct HandoffFixture {
        root: PathBuf,
        frames_dir: PathBuf,
        analysis_dir: PathBuf,
        analysis_text: String,
        sequence: FrameSequence,
    }

    /// Produce one frame with a real metadata/analysis sidecar and a PNG through
    /// a fake native producer, returning the producer-layout artifacts.
    #[cfg(unix)]
    fn produce_handoff_fixture(label: &str) -> HandoffFixture {
        use std::os::unix::fs::PermissionsExt;

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(format!("{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let input_dir = root.join("input");
        let frames_dir = root.join("frames");
        let logs_dir = root.join("logs");
        let analysis_dir = root.join("analysis");
        fs::create_dir_all(&input_dir).expect("create input dir");
        let source_path = input_dir.join("IMG_0001.DNG");
        fs::write(&source_path, b"placeholder").expect("write source frame");
        let png_path = frames_dir.join("frame_000001.png");
        let metadata_path = frames_dir.join("frame_000001.json");
        let analysis_path = analysis_dir.join("frame_000001.analysis.json");

        let metadata = format!(
            "{{\"schema_version\":1,\"source\":\"{}\",\"source_sha256\":\"fakehash\",\"width\":8,\"height\":6,\
\"applied_white_balance\":[0.5,1,0.8],\"white_balance_source\":\"as-shot\",\
\"as_shot_neutral\":[0.5,1,0.8],\"observed_scene_min\":0,\"observed_scene_max\":1,\
\"exposure_ev\":0,\"capture_time\":null,\"analysis_sidecar\":\"{}\"}}\n",
            source_path.display(),
            analysis_path.display()
        );
        let analysis = "{\"schema_version\":1,\"kind\":\"rawtl-analysis-sidecar\",\
\"source_sha256\":\"fakehash\",\"analysis\":{\"exposure_ev\":0.0},\"tiles\":[]}\n";
        let script = format!(
            "#!/bin/sh\nprintf 'PNG' > '{}'\nprintf '%s' '{}' > '{}'\nprintf '%s' '{}' > '{}'\n",
            png_path.display(),
            metadata.replace('\'', "'\\''"),
            metadata_path.display(),
            analysis,
            analysis_path.display()
        );
        let tool = root.join("fake-native.sh");
        fs::write(&tool, script).expect("write fake producer");
        let mut permissions = fs::metadata(&tool).expect("stat producer").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&tool, permissions).expect("chmod producer");

        let sources = crate::order::enumerate_dng(&input_dir).expect("enumerate sources");
        assert_eq!(sources.len(), 1);
        let config = ProduceConfig {
            frames_dir: &frames_dir,
            logs_dir: &logs_dir,
            analysis_dir: Some(&analysis_dir),
            exposure_ev: None,
            analysis_roi: None,
            no_png: false,
            echo: false,
            echo_tag: "test ",
            white_balance: WhiteBalance::FirstFrame,
            converter: None,
            converter_timeout: None,
            frames_relative_dir: "frames",
            log_relative_dir: "logs",
        };
        let sequence = produce(&tool, &sources, &config).expect("produce one frame");
        HandoffFixture {
            root,
            frames_dir,
            analysis_dir,
            analysis_text: analysis.to_string(),
            sequence,
        }
    }

    #[cfg(unix)]
    #[test]
    fn produce_returns_validated_handoff_envelopes() {
        use crate::transport::{TransportKind, ANALYSIS_SIDECAR, FRAME_METADATA, FRAME_PNG};

        let fixture = produce_handoff_fixture("frame-handoff");
        let frame = &fixture.sequence.frames[0];

        assert_eq!(
            frame.metadata_artifact.path(),
            fixture.frames_dir.join("frame_000001.json")
        );
        assert_eq!(frame.metadata_envelope.semantic_type(), FRAME_METADATA);
        assert_eq!(
            frame.metadata_envelope.transport_kind(),
            TransportKind::MemoryFile
        );
        match frame.metadata_envelope.payload() {
            Payload::MemoryFile(file) => {
                assert!(std::str::from_utf8(file.bytes())
                    .expect("metadata bytes")
                    .contains("\"schema_version\":1"))
            }
            other => panic!("unexpected metadata payload {other:?}"),
        }

        let png_artifact = frame.png_artifact.as_ref().expect("png artifact");
        assert_eq!(png_artifact.path(), fixture.frames_dir.join("frame_000001.png"));
        assert_eq!(png_artifact.media_type(), "image/png");
        let png_envelope = frame.png_envelope.as_ref().expect("png envelope");
        assert_eq!(png_envelope.semantic_type(), FRAME_PNG);
        assert_eq!(png_envelope.transport_kind(), TransportKind::DiskArtifact);

        let analysis_artifact = frame.analysis_artifact.as_ref().expect("analysis artifact");
        assert_eq!(
            analysis_artifact.path(),
            fixture.analysis_dir.join("frame_000001.analysis.json")
        );
        let loaded = analysis_sidecar_envelope(analysis_artifact, frame.source_sha256.as_deref())
            .expect("validated analysis envelope");
        assert_eq!(loaded.envelope.semantic_type(), ANALYSIS_SIDECAR);
        assert!(std::str::from_utf8(loaded.bytes())
            .expect("analysis bytes")
            .contains("\"kind\":\"rawtl-analysis-sidecar\""));

        // The provenance binding rejects a sidecar whose hash does not match.
        match analysis_sidecar_envelope(analysis_artifact, Some("different-hash")) {
            Err(SidecarLoadError::Semantic(transport::TransportError::SourceHashMismatch(_))) => {}
            Err(other) => panic!("unexpected mismatch error: {other:?}"),
            Ok(_) => panic!("mismatched source hash was accepted"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn adopted_sequence_repoints_every_typed_reference() {
        use crate::transport::{TransportKind, FRAME_PNG};

        let fixture = produce_handoff_fixture("frame-adoption");
        let original_metadata =
            fs::read(fixture.frames_dir.join("frame_000001.json")).expect("producer metadata");
        let original_analysis = fs::read(fixture.analysis_dir.join("frame_000001.analysis.json"))
            .expect("producer analysis sidecar");
        let before_png = fixture.sequence.frames[0]
            .png_envelope
            .clone()
            .expect("producer png envelope");

        // The adoption flow moves the producer round and its analysis directory
        // to the final names before the sequence is remapped.
        let adopted_frames = fixture.root.join("output/frames");
        let adopted_analysis = fixture.root.join("output/analysis/applied");
        fs::create_dir_all(adopted_frames.parent().expect("frames parent"))
            .expect("create output dir");
        fs::create_dir_all(adopted_analysis.parent().expect("analysis parent"))
            .expect("create analysis parent");
        fs::rename(&fixture.frames_dir, &adopted_frames).expect("adopt frames");
        fs::rename(&fixture.analysis_dir, &adopted_analysis).expect("adopt analysis");

        let adopted = fixture
            .sequence
            .adopt_output_locations(&adopted_frames, Some(&adopted_analysis), "logs/applied")
            .expect("adopt output locations");
        let frame = &adopted.frames[0];

        let png_path = adopted_frames.join("frame_000001.png");
        let metadata_path = adopted_frames.join("frame_000001.json");
        let analysis_path = adopted_analysis.join("frame_000001.analysis.json");

        assert_eq!(frame.png_relative, "frames/frame_000001.png");
        assert_eq!(frame.metadata_relative, "frames/frame_000001.json");
        assert_eq!(frame.log_relative, "logs/applied/frame_000001.log");

        let png_artifact = frame.png_artifact.as_ref().expect("adopted png artifact");
        assert_eq!(png_artifact.path(), png_path);
        assert!(png_artifact.path().is_file(), "adopted PNG must exist");
        assert_eq!(png_artifact.media_type(), "image/png");

        let png_envelope = frame.png_envelope.as_ref().expect("adopted png envelope");
        assert_eq!(png_envelope.semantic_type(), FRAME_PNG);
        assert_eq!(png_envelope.transport_kind(), TransportKind::DiskArtifact);
        match png_envelope.payload() {
            Payload::DiskArtifact(artifact) => {
                assert_eq!(artifact.path(), png_path);
                assert!(artifact.path().is_file(), "envelope payload must exist");
            }
            other => panic!("unexpected png payload {other:?}"),
        }
        assert_eq!(png_envelope.metadata(), before_png.metadata());
        assert!(png_envelope
            .provenance()
            .parent_value_ids()
            .iter()
            .any(|id| id == before_png.value_id()));
        assert_ne!(png_envelope.value_id(), before_png.value_id());

        assert_eq!(frame.metadata_artifact.path(), metadata_path);
        assert!(metadata_path.is_file(), "adopted metadata must exist");
        assert_eq!(
            fs::read(&metadata_path).expect("read metadata"),
            original_metadata
        );

        let analysis_artifact = frame.analysis_artifact.as_ref().expect("analysis artifact");
        assert_eq!(analysis_artifact.path(), analysis_path);
        assert!(analysis_path.is_file(), "adopted analysis sidecar must exist");
        assert_eq!(frame.analysis_path.as_deref(), Some(analysis_path.as_path()));

        let loaded = transport::load(analysis_artifact).expect("adopted analysis artifact loads");
        assert_eq!(loaded.bytes(), original_analysis.as_slice());
        let envelope = analysis_sidecar_envelope(analysis_artifact, frame.source_sha256.as_deref())
            .expect("adopted analysis envelope");
        assert_eq!(
            std::str::from_utf8(envelope.bytes()).expect("analysis bytes"),
            fixture.analysis_text
        );
    }
}
