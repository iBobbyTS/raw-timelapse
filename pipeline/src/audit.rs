//! Independent temporal audit of the encoded linear master.
//!
//! The audit decodes the actual deliverable with FFmpeg at full precision
//! (RGB48), applies the declared `linear16` analysis transfer, reduces LINEAR
//! light with an area (BOX) filter, and measures paired photometric common-mode
//! on matched 24x18 tiles. It reads the encoded file only and never needs the
//! DNG sources or the estimator's own state.
//!
//! Declared assumptions (see pipeline/README.md):
//! - the master is scene-linear `code / 16384` (no OETF), so the inverse
//!   transfer is `code / 16384` (the legacy audit's linear16 branch);
//! - the delivered-domain saturation proxy is `code >= int(0.965 * 65535)`
//!   (a literal port of the legacy constant, applied to 16-bit codes), which is
//!   deliberately distinct from the analysis sidecar's linear-scale clip series;
//! - area reduction is exact fractional-coverage averaging in f64; legacy used
//!   float32 intermediates, so audit numbers are semantically equal, not
//!   bit-identical.

use crate::error::{Error, Result, EXIT_AUDIT};
use crate::json::{self, Json};
use crate::proc;
use crate::transport::DataEnvelope;
use crate::video::{video_artifact_path, ProbeInfo};
use rawtl_linear_video::deflicker::photometry::{
    analysis_geometry, audit_features, resolve_transfer, tile_statistics, AuditSegment, TileStats,
};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

pub const DETECTOR_MODEL: &str = "fixed-camera paired photometric common-mode v1";
pub const AUDIT_TRANSFER: &str = "linear16";
/// `int(.965 * 65535)` from the legacy audit, applied to delivered codes.
pub const DELIVERED_SATURATION_CODE: u16 = (0.965 * 65535.0) as u16;
const NX: usize = 24;
const NY: usize = 18;

pub struct AuditConfig<'a> {
    /// Zero-based frame indices starting a new source segment.
    pub boundaries: &'a [usize],
    /// Source filename per decoded frame.
    pub frame_labels: &'a [String],
}

pub struct AuditOutcome {
    pub status: String,
    pub summary: Json,
}

pub fn audit_video(
    ffmpeg: &Path,
    ffprobe: &Path,
    video: &DataEnvelope,
    probe: &ProbeInfo,
    count: usize,
    config: &AuditConfig<'_>,
    output_dir: &Path,
) -> Result<AuditOutcome> {
    // The encoded master reached this stage as a typed `VIDEO_ARTIFACT`
    // handoff. Only a validated disk reference is auditable.
    let video = video_artifact_path(video)
        .ok_or_else(|| audit_error("audit input is not a validated video artifact"))?;
    let width = probe.width as usize;
    let height = probe.height as usize;
    if width == 0 || height == 0 {
        return Err(audit_error("encoded video reports an empty frame geometry"));
    }
    let (aw, ah) = analysis_geometry(width, height).map_err(|err| audit_error(&err))?;
    let (transfer, mut warnings) =
        resolve_transfer(Some(probe.color_transfer.as_str()), Some(AUDIT_TRANSFER))
            .map_err(|err| audit_error(&err))?;
    if matches!(
        probe.color_transfer.as_str(),
        "" | "unknown" | "unspecified" | "reserved"
    ) {
        warnings
            .push("UNKNOWN_TRANSFER_TAG; declared linear16 analysis assumption used".to_string());
    }
    if matches!(
        probe.color_primaries.as_str(),
        "" | "unknown" | "unspecified" | "reserved"
    ) {
        warnings.push(
            "UNKNOWN_PRIMARIES_TAG; BT.709 luminance weights assumed for analysis".to_string(),
        );
    }
    if count != config.frame_labels.len() {
        return Err(audit_error(&format!(
            "encoded frame count {count} != label count {}",
            config.frame_labels.len()
        )));
    }

    let pts = read_pts(ffprobe, video)?;
    let stats = decode_features(ffmpeg, video, width, height, aw, ah, count, output_dir)?;
    if stats.len() != count {
        return Err(audit_error(&format!(
            "decoded frame count {} != expected {count}",
            stats.len()
        )));
    }
    if !pts.is_empty() && pts.len() != stats.len() {
        return Err(audit_error(&format!(
            "decoded frame count {} != PTS count {}",
            stats.len(),
            pts.len()
        )));
    }

    let boundaries: Vec<usize> = config
        .boundaries
        .iter()
        .copied()
        .filter(|index| *index > 0 && *index < count)
        .collect();
    let summary = audit_features(&stats, NX, NY, &boundaries).map_err(|err| audit_error(&err))?;

    let mut pair_rows = Vec::with_capacity(summary.pairs.len());
    let mut frames_rows = Vec::with_capacity(count);
    for index in 0..count {
        frames_rows.push(vec![
            index.to_string(),
            pts.get(index)
                .map(|value| format_seconds(*value))
                .unwrap_or_default(),
            config.frame_labels[index].clone(),
        ]);
    }
    for pair in &summary.pairs {
        pair_rows.push(vec![
            pair.index.to_string(),
            pts.get(pair.index)
                .map(|value| format_seconds(*value))
                .unwrap_or_default(),
            config.frame_labels[pair.index].clone(),
            if pair.boundary {
                "source_discontinuity".to_string()
            } else {
                String::new()
            },
            optional(pair.metric.common_ev),
            optional(pair.metric.spatial_mad_ev),
            pair.metric.valid_tiles.to_string(),
            pair.metric.consensus_tiles.to_string(),
            optional(pair.metric.consensus_fraction),
            pair.metric.quadrants.to_string(),
            pair.metric.quality.clone(),
            optional(pair.metric.local_p90_ev),
            pair.candidate_global_jump.to_string(),
            "NOT_IMPLEMENTED".to_string(),
        ]);
    }

    write_csv(
        &output_dir.join("pairs.csv"),
        &[
            "index",
            "pts_s",
            "filename",
            "boundary",
            "common_ev",
            "spatial_mad_ev",
            "valid_tiles",
            "consensus_tiles",
            "consensus_fraction",
            "quadrants",
            "quality",
            "local_p90_ev",
            "candidate_global_jump",
            "chroma_variation",
        ],
        &pair_rows,
    )?;
    write_csv(
        &output_dir.join("frames.csv"),
        &["index", "pts_s", "filename"],
        &frames_rows,
    )?;

    let segments: Vec<Json> = summary.segments.iter().map(segment_json).collect();
    let events: Vec<Json> = summary
        .pairs
        .iter()
        .filter(|pair| pair.candidate_global_jump)
        .map(|pair| {
            Json::obj(vec![
                ("index", Json::Int(pair.index as i64)),
                (
                    "filename",
                    Json::Str(config.frame_labels[pair.index].clone()),
                ),
                (
                    "pts_s",
                    pts.get(pair.index)
                        .map(|v| Json::Float(*v))
                        .unwrap_or(Json::Null),
                ),
                ("common_ev", optional_json(pair.metric.common_ev)),
                ("local_p90_ev", optional_json(pair.metric.local_p90_ev)),
                (
                    "consensus_fraction",
                    optional_json(pair.metric.consensus_fraction),
                ),
            ])
        })
        .collect();
    let warnings_json = Json::Array(warnings.drain(..).map(Json::Str).collect::<Vec<_>>());
    let boundary_json = Json::Object(
        boundaries
            .iter()
            .map(|index| (index.to_string(), Json::Str("source_discontinuity".into())))
            .collect(),
    );
    let bytes = fs::metadata(video).map(|m| m.len()).unwrap_or(0);
    let full = Json::obj(vec![
        ("detector_model", Json::Str(DETECTOR_MODEL.to_string())),
        (
            "video_identity",
            Json::obj(vec![
                ("path", Json::Str(video.display().to_string())),
                ("bytes", Json::Int(bytes as i64)),
                (
                    "sha256",
                    Json::Str(
                        "NOT_COMPUTED_STD_ONLY (content identity proven by lossless roundtrip)"
                            .into(),
                    ),
                ),
            ]),
        ),
        ("frame_count", Json::Int(stats.len() as i64)),
        (
            "decoded_shape",
            Json::Array(vec![Json::Int(width as i64), Json::Int(height as i64)]),
        ),
        (
            "analysis_shape",
            Json::Array(vec![Json::Int(aw as i64), Json::Int(ah as i64)]),
        ),
        ("transfer_assumed", Json::Str(transfer)),
        (
            "decode_policy",
            Json::Str(
                "RGB48 -> linear16 inverse transfer -> f64 BOX reduce; no temporal resampling"
                    .into(),
            ),
        ),
        (
            "saturation_code_threshold",
            Json::Int(DELIVERED_SATURATION_CODE as i64),
        ),
        (
            "tags",
            Json::obj(vec![
                ("color_space", Json::Str(probe.color_space.clone())),
                ("color_range", Json::Str(probe.color_range.clone())),
                ("color_transfer", Json::Str(probe.color_transfer.clone())),
                ("color_primaries", Json::Str(probe.color_primaries.clone())),
            ]),
        ),
        ("metadata_warnings", warnings_json.clone()),
        ("chroma_variation", Json::Str("NOT_IMPLEMENTED".into())),
        ("status", Json::Str(summary.status.clone())),
        ("boundaries", boundary_json),
        ("segments", Json::Array(segments)),
        ("events", Json::Array(events)),
        ("candidate_count", Json::Int(summary.candidate_count as i64)),
        ("constraints", constraints_json()),
    ]);

    write_json(&output_dir.join("temporal_audit.json"), &full)?;

    let compact_segments: Vec<Json> = summary
        .segments
        .iter()
        .map(|segment| compact_segment(segment))
        .collect();
    let compact = Json::obj(vec![
        ("status", Json::Str(summary.status.clone())),
        ("detector_model", Json::Str(DETECTOR_MODEL.to_string())),
        ("frame_count", Json::Int(stats.len() as i64)),
        ("candidate_count", Json::Int(summary.candidate_count as i64)),
        ("chroma_variation", Json::Str("NOT_IMPLEMENTED".into())),
        ("metadata_warnings", warnings_json),
        ("segments", Json::Array(compact_segments)),
        (
            "events",
            Json::Array(
                summary
                    .pairs
                    .iter()
                    .filter(|pair| pair.candidate_global_jump)
                    .map(|pair| {
                        Json::obj(vec![
                            ("index", Json::Int(pair.index as i64)),
                            (
                                "filename",
                                Json::Str(config.frame_labels[pair.index].clone()),
                            ),
                            ("common_ev", optional_json(pair.metric.common_ev)),
                            ("local_p90_ev", optional_json(pair.metric.local_p90_ev)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ]);

    Ok(AuditOutcome {
        status: summary.status,
        summary: compact,
    })
}

fn decode_features(
    ffmpeg: &Path,
    video: &Path,
    width: usize,
    height: usize,
    aw: usize,
    ah: usize,
    count: usize,
    output_dir: &Path,
) -> Result<Vec<TileStats>> {
    let args = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-v".to_string(),
        "error".to_string(),
        "-threads".to_string(),
        "2".to_string(),
        "-filter_threads".to_string(),
        "1".to_string(),
        "-i".to_string(),
        video.display().to_string(),
        "-map".to_string(),
        "0:v:0".to_string(),
        "-fps_mode".to_string(),
        "passthrough".to_string(),
        "-pix_fmt".to_string(),
        "rgb48le".to_string(),
        "-f".to_string(),
        "rawvideo".to_string(),
        "-".to_string(),
    ];
    let stderr_path = output_dir.join("logs/audit-decode.log");
    if let Some(parent) = stderr_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            Error::new(
                EXIT_AUDIT,
                format!(
                    "cannot create audit log directory {}: {err}",
                    parent.display()
                ),
            )
        })?;
    }
    let stderr_file = fs::File::create(&stderr_path).map_err(|err| {
        Error::new(
            EXIT_AUDIT,
            format!(
                "cannot create audit decode log {}: {err}",
                stderr_path.display()
            ),
        )
    })?;
    let mut child = Command::new(ffmpeg)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .map_err(|err| {
            Error::new(
                EXIT_AUDIT,
                format!("failed to start ffmpeg audit decode: {err}"),
            )
        })?;
    let mut stdout = child.stdout.take().expect("piped audit stdout");

    let frame_values = width * height;
    let frame_bytes = frame_values * 6;
    let mut buffer = vec![0u8; frame_bytes];
    let mut rgb = vec![[0u16; 3]; frame_values];
    let mut luma = vec![0f64; frame_values];
    let mut saturation = vec![0f64; frame_values];
    let x_weights = axis_weights(width, aw);
    let y_weights = axis_weights(height, ah);
    let mut reduced_luma = vec![0f64; aw * ah];
    let mut reduced_sat = vec![0f64; aw * ah];
    let mut tmp_luma = vec![0f64; height * aw];
    let mut tmp_sat = vec![0f64; height * aw];
    let mut stats = Vec::with_capacity(count);

    let mut result = Ok(());
    loop {
        match read_frame(&mut stdout, &mut buffer) {
            Ok(false) => break,
            Ok(true) => {}
            Err(err) => {
                result = Err(err);
                break;
            }
        }
        if stats.len() >= count {
            result = Err(Error::new(
                EXIT_AUDIT,
                format!("encoded video contains more than {count} frames"),
            ));
            break;
        }
        fill_rgb48(&buffer, &mut rgb);
        luminance(&rgb, &mut luma);
        saturation_mask(&rgb, &mut saturation);
        reduce(
            &luma,
            &mut tmp_luma,
            &mut reduced_luma,
            width,
            height,
            aw,
            &x_weights,
            &y_weights,
        );
        reduce(
            &saturation,
            &mut tmp_sat,
            &mut reduced_sat,
            width,
            height,
            aw,
            &x_weights,
            &y_weights,
        );
        match tile_statistics(&reduced_luma, &reduced_sat, aw, ah, NX, NY) {
            Ok(tile) => stats.push(tile),
            Err(err) => {
                result = Err(audit_error(&err));
                break;
            }
        }
    }
    drop(stdout);
    let status = child.wait();
    if result.is_ok() {
        match status {
            Ok(status) if status.success() => {}
            Ok(status) => {
                let detail = fs::read_to_string(&stderr_path).unwrap_or_default();
                result = Err(audit_error(&format!(
                    "FFmpeg audit decode failed: {}; {}",
                    proc::exit_description(&status),
                    proc::tail(&detail, 2000)
                )));
            }
            Err(err) => {
                result = Err(audit_error(&format!(
                    "cannot wait for FFmpeg audit decode: {err}"
                )));
            }
        }
    } else {
        let _ = child.kill();
        let _ = child.wait();
    }
    result?;
    Ok(stats)
}

fn read_frame(reader: &mut impl Read, buffer: &mut [u8]) -> Result<bool> {
    let mut received = 0;
    while received < buffer.len() {
        match reader.read(&mut buffer[received..]) {
            Ok(0) => {
                if received == 0 {
                    return Ok(false);
                }
                return Err(audit_error("truncated decoded frame from FFmpeg"));
            }
            Ok(n) => received += n,
            Err(err) => {
                return Err(audit_error(&format!(
                    "cannot read decoded frame from FFmpeg: {err}"
                )))
            }
        }
    }
    Ok(true)
}

fn fill_rgb48(bytes: &[u8], rgb: &mut [[u16; 3]]) {
    for (pixel, chunk) in rgb.iter_mut().zip(bytes.chunks_exact(6)) {
        pixel[0] = u16::from_le_bytes([chunk[0], chunk[1]]);
        pixel[1] = u16::from_le_bytes([chunk[2], chunk[3]]);
        pixel[2] = u16::from_le_bytes([chunk[4], chunk[5]]);
    }
}

/// Declared linear16 transfer and REC.709 weights, with the transfer applied as
/// `code / 16384` per component before weighting.
fn luminance(rgb: &[[u16; 3]], out: &mut [f64]) {
    const SCALE: f64 = 1.0 / 16384.0;
    for (value, pixel) in out.iter_mut().zip(rgb) {
        *value = (pixel[0] as f64 * SCALE) * 0.2126
            + (pixel[1] as f64 * SCALE) * 0.7152
            + (pixel[2] as f64 * SCALE) * 0.0722;
    }
}

fn saturation_mask(rgb: &[[u16; 3]], out: &mut [f64]) {
    for (value, pixel) in out.iter_mut().zip(rgb) {
        *value = if pixel[0] >= DELIVERED_SATURATION_CODE
            || pixel[1] >= DELIVERED_SATURATION_CODE
            || pixel[2] >= DELIVERED_SATURATION_CODE
        {
            1.0
        } else {
            0.0
        };
    }
}

/// Per-destination-index fractional source coverage, normalized to unit weight.
fn axis_weights(src: usize, dst: usize) -> Vec<(usize, Vec<f64>)> {
    let mut out = Vec::with_capacity(dst);
    let scale = src as f64 / dst as f64;
    for index in 0..dst {
        let lo = index as f64 * scale;
        let hi = (index + 1) as f64 * scale;
        // Floating point can push the last boundary a hair past `src`.
        let start = (lo.floor() as usize).min(src - 1);
        let end = (hi.ceil() as usize).min(src);
        let mut weights = Vec::with_capacity(end.saturating_sub(start));
        for source in start..end {
            let left = (source as f64).max(lo);
            let right = ((source + 1) as f64).min(hi);
            weights.push((right - left).max(0.0));
        }
        let total: f64 = weights.iter().sum();
        if total > 0.0 {
            for weight in &mut weights {
                *weight /= total;
            }
        } else {
            weights = vec![1.0];
        }
        out.push((start, weights));
    }
    out
}

fn reduce(
    source: &[f64],
    tmp: &mut [f64],
    out: &mut [f64],
    width: usize,
    height: usize,
    dst_width: usize,
    x_weights: &[(usize, Vec<f64>)],
    y_weights: &[(usize, Vec<f64>)],
) {
    for y in 0..height {
        let row = &source[y * width..(y + 1) * width];
        let dst_row = &mut tmp[y * dst_width..(y + 1) * dst_width];
        for (x, (start, weights)) in x_weights.iter().enumerate() {
            let mut sum = 0.0;
            for (offset, weight) in weights.iter().enumerate() {
                sum += row[start + offset] * weight;
            }
            dst_row[x] = sum;
        }
    }
    let dst_height = out.len() / dst_width;
    for (y, (start, weights)) in y_weights.iter().enumerate().take(dst_height) {
        let dst_row = &mut out[y * dst_width..(y + 1) * dst_width];
        for value in dst_row.iter_mut() {
            *value = 0.0;
        }
        for (offset, weight) in weights.iter().enumerate() {
            let source_row = &tmp[(start + offset) * dst_width..(start + offset + 1) * dst_width];
            for (value, source) in dst_row.iter_mut().zip(source_row) {
                *value += source * weight;
            }
        }
    }
}

fn read_pts(ffprobe: &Path, video: &Path) -> Result<Vec<f64>> {
    let args = vec![
        "-v".to_string(),
        "error".to_string(),
        "-select_streams".to_string(),
        "v:0".to_string(),
        "-show_frames".to_string(),
        "-show_entries".to_string(),
        "frame=best_effort_timestamp_time".to_string(),
        "-of".to_string(),
        "json".to_string(),
        video.display().to_string(),
    ];
    let captured = proc::run_capture(ffprobe, &args, EXIT_AUDIT)?;
    if !captured.status.success() {
        return Err(audit_error(&format!(
            "ffprobe frame timestamps failed for {}: {}; stderr: {}",
            video.display(),
            proc::exit_description(&captured.status),
            proc::tail(&captured.stderr, 1200)
        )));
    }
    let parsed = json::parse(&captured.stdout)
        .map_err(|err| audit_error(&format!("ffprobe timestamp JSON is invalid: {err}")))?;
    let frames = parsed
        .get("frames")
        .and_then(Json::as_array)
        .ok_or_else(|| audit_error("ffprobe timestamp JSON has no frames array"))?;
    let mut pts = Vec::with_capacity(frames.len());
    for frame in frames {
        if let Some(text) = frame
            .get("best_effort_timestamp_time")
            .and_then(Json::as_str)
        {
            if let Ok(value) = text.parse::<f64>() {
                if value.is_finite() {
                    pts.push(value);
                }
            }
        }
    }
    Ok(pts)
}

fn segment_json(segment: &AuditSegment) -> Json {
    Json::obj(vec![
        ("start", Json::Int(segment.start as i64)),
        ("end_exclusive", Json::Int(segment.end_exclusive as i64)),
        ("frames", Json::Int(segment.frames as i64)),
        ("status", Json::Str(segment.status.clone())),
        ("pairs", Json::Int(segment.summary.pairs as i64)),
        (
            "usable_pairs",
            Json::Int(segment.summary.usable_pairs as i64),
        ),
        (
            "median_abs_step_ev",
            optional_json(segment.summary.median_abs_step_ev),
        ),
        (
            "p95_abs_step_ev",
            optional_json(segment.summary.p95_abs_step_ev),
        ),
        (
            "max_abs_step_ev",
            optional_json(segment.summary.max_abs_step_ev),
        ),
        (
            "curvature_rms_ev",
            optional_json(segment.summary.curvature_rms_ev),
        ),
        (
            "lag2_residual_rms_ev",
            optional_json(segment.summary.lag2_residual_rms_ev),
        ),
        (
            "steps_over_0_02_ev",
            Json::Int(segment.summary.steps_over_0_02_ev as i64),
        ),
        (
            "steps_over_0_05_ev",
            Json::Int(segment.summary.steps_over_0_05_ev as i64),
        ),
        ("roi_metrics", Json::obj(vec![])),
    ])
}

fn compact_segment(segment: &AuditSegment) -> Json {
    Json::obj(vec![
        ("start", Json::Int(segment.start as i64)),
        ("end_exclusive", Json::Int(segment.end_exclusive as i64)),
        ("frames", Json::Int(segment.frames as i64)),
        ("status", Json::Str(segment.status.clone())),
        (
            "usable_pairs",
            Json::Int(segment.summary.usable_pairs as i64),
        ),
        ("pairs", Json::Int(segment.summary.pairs as i64)),
        (
            "median_abs_step_ev",
            optional_json(segment.summary.median_abs_step_ev),
        ),
        (
            "p95_abs_step_ev",
            optional_json(segment.summary.p95_abs_step_ev),
        ),
        (
            "max_abs_step_ev",
            optional_json(segment.summary.max_abs_step_ev),
        ),
        (
            "curvature_rms_ev",
            optional_json(segment.summary.curvature_rms_ev),
        ),
        (
            "steps_over_0_05_ev",
            Json::Int(segment.summary.steps_over_0_05_ev as i64),
        ),
    ])
}

fn constraints_json() -> Json {
    Json::obj(vec![
        ("pair_shape_mad_ev", Json::Float(0.04)),
        ("pair_consensus_ev", Json::Float(0.06)),
        ("minimum_inlier_fraction", Json::Float(0.6)),
        ("minimum_quadrants", Json::Int(3)),
        (
            "candidate_step_ev",
            Json::Float(rawtl_linear_video::deflicker::photometry::CANDIDATE_STEP_EV),
        ),
        (
            "gate_p95_step_ev",
            Json::Float(rawtl_linear_video::deflicker::photometry::GATE_P95_STEP_EV),
        ),
        (
            "gate_curvature_rms_ev",
            Json::Float(rawtl_linear_video::deflicker::photometry::GATE_CURVATURE_RMS_EV),
        ),
        (
            "gate_severe_event_ev",
            Json::Float(rawtl_linear_video::deflicker::photometry::GATE_SEVERE_EVENT_EV),
        ),
    ])
}

fn write_csv(path: &Path, header: &[&str], rows: &[Vec<String>]) -> Result<()> {
    let mut text = String::new();
    text.push_str(&header.join(","));
    text.push('\n');
    for row in rows {
        let cells: Vec<String> = row.iter().map(|cell| csv_cell(cell)).collect();
        text.push_str(&cells.join(","));
        text.push('\n');
    }
    fs::write(path, text).map_err(|err| {
        Error::new(
            EXIT_AUDIT,
            format!("cannot write audit evidence {}: {err}", path.display()),
        )
    })
}

fn csv_cell(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn write_json(path: &Path, value: &Json) -> Result<()> {
    let text = json::to_pretty_string(value);
    let mut file = fs::File::create(path).map_err(|err| {
        Error::new(
            EXIT_AUDIT,
            format!("cannot create audit report {}: {err}", path.display()),
        )
    })?;
    file.write_all(text.as_bytes()).map_err(|err| {
        Error::new(
            EXIT_AUDIT,
            format!("cannot write audit report {}: {err}", path.display()),
        )
    })
}

fn format_seconds(value: f64) -> String {
    format!("{value}")
}

fn optional(value: Option<f64>) -> String {
    value.map(format_seconds).unwrap_or_default()
}

fn optional_json(value: Option<f64>) -> Json {
    value
        .filter(|v| v.is_finite())
        .map_or(Json::Null, Json::Float)
}

fn audit_error(message: &str) -> Error {
    Error::new(EXIT_AUDIT, message.to_string())
}
