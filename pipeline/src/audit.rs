//! Independent audit of the actual encoded deliverable. Profile identity is
//! authoritative in VIDEO_ARTIFACT metadata; stream tags cross-check it. RGB48
//! decoder signals are inverse-transferred before BOX reduction and photometry.

use crate::color_profile::{self, ColorProfile};
use crate::error::{Error, Result, EXIT_AUDIT};
use crate::json::{self, Json};
use crate::proc;
use crate::transport::DataEnvelope;
use crate::video::{video_artifact_path, ProbeInfo};
use rawtl_linear_video::deflicker::photometry::{
    analysis_geometry, audit_features, tile_statistics, AuditSegment, TileStats,
};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

pub const DETECTOR_MODEL: &str = "fixed-camera paired photometric common-mode v1";
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
    let profile = resolve_profile(video, probe)?;
    let video = video_artifact_path(video)
        .ok_or_else(|| audit_error("audit input is not a validated video artifact"))?;
    let width = probe.width as usize;
    let height = probe.height as usize;
    if width == 0 || height == 0 {
        return Err(audit_error("encoded video reports an empty frame geometry"));
    }
    let (aw, ah) = analysis_geometry(width, height).map_err(|err| audit_error(&err))?;
    let transfer = profile.transfer.name().to_string();
    let mut warnings: Vec<String> = Vec::new();
    if count != config.frame_labels.len() {
        return Err(audit_error(&format!(
            "encoded frame count {count} != label count {}",
            config.frame_labels.len()
        )));
    }

    let pts = read_pts(ffprobe, video)?;
    let stats = decode_features(
        ffmpeg, video, width, height, aw, ah, count, output_dir, profile,
    )?;
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
                ("profile", Json::Str(profile.id.into())),
                ("bytes", Json::Int(bytes as i64)),
                (
                    "sha256",
                    Json::Str(
                        "NOT_COMPUTED_STD_ONLY (artifact path, bytes, ffprobe and profile metadata recorded)"
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
                "RGB48 normalized signals -> selected profile inverse transfer -> target-linear luminance -> f64 BOX reduce; no temporal resampling"
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
    profile: &ColorProfile,
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
        luminance(&rgb, &mut luma, profile);
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
    if result.is_err() {
        let _ = child.kill();
    }
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

fn luminance(rgb: &[[u16; 3]], out: &mut [f64], profile: &ColorProfile) {
    let weights = profile.gamut.rgb_to_xyz[1];
    for (value, pixel) in out.iter_mut().zip(rgb) {
        *value = pixel
            .iter()
            .zip(weights)
            .map(|(code, weight)| profile.transfer.inverse(*code as f64 / 65535.0) * weight)
            .sum();
    }
}

/// Unknown tags carry no contradictory information. Explicit tags must match
/// each field of the selected profile, including expected-unspecified Log
/// policies: a bt709 tag cannot silently relabel a vendor Log stream.
fn resolve_profile(video: &DataEnvelope, probe: &ProbeInfo) -> Result<&'static ColorProfile> {
    let id = video.metadata().get("color.profile").and_then(Json::as_str);
    let profile = id.and_then(color_profile::lookup).ok_or_else(|| {
        audit_error("TEMPORAL_AUDIT_UNOBSERVABLE: missing or unresolved profile identity")
    })?;
    for (field, observed, expected) in [
        (
            "color_transfer",
            probe.color_transfer.as_str(),
            profile.tag_policy.transfer_token(),
        ),
        (
            "color_primaries",
            probe.color_primaries.as_str(),
            profile.tag_policy.primaries_token(),
        ),
        (
            "color_space",
            probe.color_space.as_str(),
            profile.tag_policy.matrix_token(),
        ),
        (
            "color_range",
            probe.color_range.as_str(),
            profile.range.as_str(),
        ),
    ] {
        if !matches!(observed, "" | "unknown" | "unspecified" | "reserved") && observed != expected
        {
            return Err(audit_error(&format!("TEMPORAL_AUDIT_ERROR: profile {} conflicts with {field}={observed} (expected {expected})", profile.id)));
        }
    }
    Ok(profile)
}

/// Stable run inventory when audit is disabled or unavailable. Empty pairs
/// mean no measured pairs; the independent audit replaces these on success.
pub fn write_inventory(output: &Path, labels: &[String]) -> Result<()> {
    write_csv(
        &output.join("pairs.csv"),
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
        &[],
    )?;
    write_csv(
        &output.join("frames.csv"),
        &["index", "pts_s", "filename"],
        &labels
            .iter()
            .enumerate()
            .map(|(i, label)| vec![i.to_string(), String::new(), label.clone()])
            .collect::<Vec<_>>(),
    )
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::transport::{DataEnvelope, MetadataDoc};
    use std::os::unix::fs::PermissionsExt;

    fn executable(path: &Path, text: &str) {
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn root(label: &str) -> std::path::PathBuf {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-tmp")
            .join(format!("audit-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }
    fn probe(profile: &ColorProfile, count: usize) -> ProbeInfo {
        ProbeInfo {
            codec_name: "h264".into(),
            pix_fmt: "yuv420p".into(),
            width: 192,
            height: 144,
            frames: count as u64,
            avg_frame_rate: "30/1".into(),
            color_space: profile.tag_policy.matrix_token().into(),
            color_primaries: profile.tag_policy.primaries_token().into(),
            color_transfer: profile.tag_policy.transfer_token().into(),
            color_range: profile.range.as_str().into(),
        }
    }
    #[test]
    fn all_profiles_measure_known_ev_changes_through_the_production_audit() {
        // This is the real audit_video -> decode_features -> luminance path,
        // with controlled decoded RGB48 signals from the actual frame converter.
        for profile in &color_profile::PROFILES {
            let root = root(profile.id);
            let video = root.join("output.mp4");
            fs::write(&video, b"controlled encoded artifact").unwrap();
            let artifact =
                crate::video::video_envelope(&video, "video/mp4", "h264", 26, "test", profile)
                    .unwrap();
            let mut bytes = Vec::new();
            for index in 0..26 {
                let linear = if index % 2 == 0 { 0.18 } else { 0.36 };
                let code = (linear * 16384.0_f64).round() as u16;
                let rgb48: Vec<_> = std::iter::repeat_n(code.to_le_bytes(), 192 * 144 * 3)
                    .flatten()
                    .collect();
                let mut converted = Vec::new();
                profile
                    .convert_frame(&rgb48, 192, 144, &mut converted)
                    .unwrap();
                let decoded: Vec<_> = converted
                    .iter()
                    .flat_map(|v| (*v as u16 * 257).to_le_bytes())
                    .collect();
                // Quantization-bound comparison in target-linear coordinates.
                let target = rawtl_linear_video::preview::mat3_vec(
                    &profile.composed_matrix(),
                    [code as f64 / 16384.0; 3],
                );
                let mut actual = [0.0];
                luminance(
                    &[[
                        u16::from_le_bytes(decoded[0..2].try_into().unwrap()),
                        u16::from_le_bytes(decoded[2..4].try_into().unwrap()),
                        u16::from_le_bytes(decoded[4..6].try_into().unwrap()),
                    ]],
                    &mut actual,
                    profile,
                );
                let weights = profile.gamut.rgb_to_xyz[1];
                let expected: f64 = target.iter().zip(weights).map(|(v, w)| v * w).sum();
                let bound: f64 = converted[..3]
                    .iter()
                    .zip(weights)
                    .map(|(v, w)| {
                        let signal = *v as f64 / 255.0;
                        let lo = profile.transfer.inverse(signal - 0.5 / 255.0);
                        let hi = profile.transfer.inverse(signal + 0.5 / 255.0);
                        (hi - lo).abs() * w.abs()
                    })
                    .sum::<f64>()
                    + 0.0003; // Published D-Log inverse coefficients have finite precision.
                assert!(
                    (actual[0] - expected).abs() <= bound,
                    "{}: {} vs {} (bound {})",
                    profile.id,
                    actual[0],
                    expected,
                    bound
                );
                bytes.extend_from_slice(&decoded);
            }
            fs::write(root.join("decoded.rgb48"), bytes).unwrap();
            let decoder = root.join("decoder");
            executable(
                &decoder,
                &format!(
                    "#!/bin/sh\nexec cat '{}'\n",
                    root.join("decoded.rgb48").display()
                ),
            );
            let prober = root.join("prober");
            executable(&prober, "#!/bin/sh\nprintf '{\"frames\":[]}'\n");
            let labels: Vec<_> = (0..26).map(|i| format!("frame-{i}")).collect();
            let result = audit_video(
                &decoder,
                &prober,
                &artifact,
                &probe(profile, 26),
                26,
                &AuditConfig {
                    boundaries: &[],
                    frame_labels: &labels,
                },
                &root,
            )
            .unwrap();
            assert_eq!(
                result.status, "FAIL",
                "known one-EV changes must be observable: {}",
                profile.id
            );
            let csv = fs::read_to_string(root.join("pairs.csv")).unwrap();
            for (i, row) in csv.lines().skip(1).enumerate() {
                let measured: f64 = row.split(',').nth(4).unwrap().parse().unwrap();
                let expected = if i % 2 == 0 { 1.0 } else { -1.0 };
                assert!(
                    (measured - expected).abs() < 0.06,
                    "{} pair {} measured {} EV",
                    profile.id,
                    i,
                    measured
                );
            }
            let report = fs::read_to_string(root.join("temporal_audit.json")).unwrap();
            assert!(report.contains("NOT_COMPUTED_STD_ONLY"));
            assert!(!report.contains("lossless roundtrip"));
        }
    }

    #[test]
    fn identity_conflicts_and_absence_reach_caller_status_without_events() {
        let root = root("identity-errors");
        let video = root.join("output.mp4");
        fs::write(&video, b"artifact").unwrap();
        for profile in &color_profile::PROFILES {
            let artifact =
                crate::video::video_envelope(&video, "video/mp4", "h264", 2, "test", profile)
                    .unwrap();
            let mut tags = probe(profile, 2);
            // Expected-unspecified tags are legal for every vendor Log profile.
            assert!(resolve_profile(&artifact, &tags).is_ok());
            tags.color_transfer = "bt709".into();
            let missing = DataEnvelope::new(
                artifact.semantic_type(),
                artifact.payload().clone(),
                MetadataDoc::new(vec![
                    ("audit.codec".into(), Json::Str("h264".into())),
                    ("source.frame_count".into(), Json::Int(2)),
                ])
                .unwrap(),
                artifact.provenance().clone(),
            )
            .unwrap();
            let labels = vec!["a".into(), "b".into()];
            for (input, expected) in [(&artifact, "ERROR"), (&missing, "UNOBSERVABLE")] {
                // Neither process exists: resolving identity must precede decode.
                let err = audit_video(
                    Path::new("/missing-decoder"),
                    Path::new("/missing-prober"),
                    input,
                    &tags,
                    2,
                    &AuditConfig {
                        boundaries: &[],
                        frame_labels: &labels,
                    },
                    &root,
                )
                .err()
                .unwrap();
                let summary = crate::deflicker_run::record_audit_failure(&root, &err).unwrap();
                let persisted =
                    json::parse(&fs::read_to_string(root.join("temporal_audit.json")).unwrap())
                        .unwrap();
                assert_eq!(persisted, summary);
                assert_eq!(summary.get("status").and_then(Json::as_str), Some(expected));
                assert!(summary
                    .get("events")
                    .and_then(Json::as_array)
                    .unwrap()
                    .is_empty());
            }
        }
    }
}
