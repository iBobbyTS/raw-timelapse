//! Run manifest: provenance and validation record written before publication.

use crate::cli::{Options, LEGACY_FPS_PROVENANCE};
use crate::error::{Error, Result, EXIT_PUBLISH};
use crate::frame::FrameSequence;
use crate::json::{self, Json};
use crate::video::{
    preview_filter, preview_final_name, preview_geometry, ProbeInfo, RoundtripReport,
    LINEAR_FINAL_NAME,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

pub struct ManifestInput<'a> {
    pub options: &'a Options,
    pub input: &'a Path,
    pub output: &'a Path,
    pub sequence: &'a FrameSequence,
    pub linear_probe: &'a ProbeInfo,
    pub roundtrip: &'a RoundtripReport,
    pub preview_probe: &'a ProbeInfo,
    pub linear_bytes: u64,
    pub preview_bytes: u64,
    pub ffmpeg_version: String,
    pub ffprobe_version: String,
    pub timings: &'a BTreeMap<String, Duration>,
    /// Process-owned deflicker block (config, status counts, audit summary).
    pub deflicker: Json,
}

pub fn build(input: &ManifestInput<'_>) -> Json {
    let sequence = input.sequence;
    let preview_scale = preview_geometry(sequence.width, sequence.height);
    let frames: Vec<Json> = sequence
        .frames
        .iter()
        .map(|frame| {
            Json::obj(vec![
                ("index", Json::Int(frame.index as i64)),
                ("source", Json::Str(frame.source_name.clone())),
                (
                    "source_sha256",
                    match &frame.source_sha256 {
                        Some(value) => Json::Str(value.clone()),
                        None => Json::Null,
                    },
                ),
                ("width", Json::Int(frame.width as i64)),
                ("height", Json::Int(frame.height as i64)),
                ("png", Json::Str(frame.png_relative.clone())),
                ("metadata", Json::Str(frame.metadata_relative.clone())),
                ("log", Json::Str(frame.log_relative.clone())),
                ("applied_white_balance", number_triple(frame.applied_wb)),
                ("white_balance_source", Json::Str(frame.wb_source.clone())),
                ("as_shot_neutral", number_triple(frame.as_shot_neutral)),
                ("observed_scene_min", optional_number(frame.observed_min)),
                ("observed_scene_max", optional_number(frame.observed_max)),
                (
                    "quantization",
                    frame.quantization.clone().unwrap_or(Json::Null),
                ),
                ("duration_s", Json::Float(frame.duration.as_secs_f64())),
            ])
        })
        .collect();

    let all_applied_equal = {
        let reference = sequence.reference_wb;
        sequence.frames.iter().all(|frame| {
            frame
                .applied_wb
                .iter()
                .zip(reference.iter())
                .all(|(left, right)| (left - right).abs() <= 1e-12)
        })
    };

    let white_balance = if input.options.white_balance.freezes_reference() {
        Json::obj(vec![
            (
                "strategy",
                Json::Str("sequence-frozen (REQ-008)".to_string()),
            ),
            (
                "reference_frame",
                Json::Str(sequence.reference_frame.clone()),
            ),
            ("reference_source", Json::Str("as-shot".to_string())),
            ("reference", number_triple(sequence.reference_wb)),
            (
                "propagation",
                Json::Str(
                    "first numerically ordered frame decoded with its as-shot neutral; \
                     those three camera-neutral values passed as --white-balance to every \
                     later frame"
                        .to_string(),
                ),
            ),
            ("all_applied_equal", Json::Bool(all_applied_equal)),
            (
                "per_frame_as_shot_neutral",
                Json::Str(
                    "each frame keeps its own source as_shot_neutral for provenance".to_string(),
                ),
            ),
        ])
    } else {
        // No frozen reference exists in per-frame mode: the reference fields
        // are explicitly null instead of naming a frame the pipeline never
        // propagates.
        Json::obj(vec![
            (
                "strategy",
                Json::Str(input.options.white_balance.as_str().to_string()),
            ),
            ("reference_frame", Json::Null),
            ("reference_source", Json::Null),
            ("reference", Json::Null),
            (
                "propagation",
                Json::Str(
                    "every numerically ordered frame decoded with its own as-shot neutral; \
                     no --white-balance override is passed to the native producer in any pass"
                        .to_string(),
                ),
            ),
            ("all_applied_equal", Json::Bool(all_applied_equal)),
            (
                "per_frame_as_shot_neutral",
                Json::Str(
                    "each frame is decoded with its own source as_shot_neutral; its sidecar \
                     must report applied_white_balance equal to that neutral"
                        .to_string(),
                ),
            ),
        ])
    };

    Json::obj(vec![
        (
            "schema_version",
            Json::Int(1),
        ),
        (
            "tool",
            Json::obj(vec![
                (
                    "name",
                    Json::Str("rawtl-linear-video".to_string()),
                ),
                (
                    "version",
                    Json::Str(env!("CARGO_PKG_VERSION").to_string()),
                ),
            ]),
        ),
        (
            "run",
            Json::obj(vec![
                ("input", Json::Str(input.input.display().to_string())),
                ("output", Json::Str(input.output.display().to_string())),
                (
                    "status",
                    Json::Str("validated".to_string()),
                ),
            ]),
        ),
        (
            "environment",
            Json::obj(vec![
                (
                    "dng_linear_frame",
                    Json::Str(input.options.native_tool.display().to_string()),
                ),
                (
                    "ffmpeg",
                    Json::Str(input.options.ffmpeg.display().to_string()),
                ),
                (
                    "ffprobe",
                    Json::Str(input.options.ffprobe.display().to_string()),
                ),
                (
                    "ffmpeg_version",
                    Json::Str(input.ffmpeg_version.clone()),
                ),
                (
                    "ffprobe_version",
                    Json::Str(input.ffprobe_version.clone()),
                ),
                (
                    "converter_override",
                    match &input.options.converter {
                        Some(path) => Json::Str(path.clone()),
                        None => Json::Null,
                    },
                ),
                (
                    "converter_timeout_override_seconds",
                    match input.options.converter_timeout {
                        Some(seconds) => Json::Int(seconds as i64),
                        None => Json::Null,
                    },
                ),
                ("os", Json::Str(std::env::consts::OS.to_string())),
                ("arch", Json::Str(std::env::consts::ARCH.to_string())),
            ]),
        ),
        (
            "frames",
            Json::obj(vec![
                ("count", Json::Int(sequence.frames.len() as i64)),
                ("width", Json::Int(sequence.width as i64)),
                ("height", Json::Int(sequence.height as i64)),
                ("ordering", Json::Str(
                    "numeric order of filename digit runs; ambiguous keys rejected".to_string(),
                )),
                ("entries", Json::Array(frames)),
            ]),
        ),
        (
            "fps",
            Json::obj(vec![
                ("value", Json::Str(input.options.fps.arg())),
                ("decimal", Json::Float(input.options.fps.as_f64())),
                (
                    "provenance",
                    Json::Str(if input.options.fps_explicit {
                        "CLI --fps override".to_string()
                    } else {
                        LEGACY_FPS_PROVENANCE.to_string()
                    }),
                ),
                ("explicit", Json::Bool(input.options.fps_explicit)),
            ]),
        ),
        (
            "white_balance",
            white_balance,
        ),
        (
            "quantization_policy",
            Json::obj(vec![
                ("scale", Json::Int(16384)),
                ("rounding", Json::Str("nearest; ties upward".to_string())),
                ("negative_policy", Json::Str("clip to 0".to_string())),
                (
                    "upper_policy",
                    Json::Str("above 65535/16384 clips to 65535".to_string()),
                ),
                ("transfer", Json::Str("linear; no OETF".to_string())),
                (
                    "authority",
                    Json::Str(
                        "PLAN shared quantization policy; per-frame native JSON records counts"
                            .to_string(),
                    ),
                ),
            ]),
        ),
        (
            "linear_video",
            Json::obj(vec![
                ("file", Json::Str(LINEAR_FINAL_NAME.to_string())),
                ("bytes", Json::Int(input.linear_bytes as i64)),
                ("container", Json::Str("matroska".to_string())),
                ("codec", Json::Str("ffv1".to_string())),
                ("codec_level", Json::Int(3)),
                ("golomb", Json::Int(1)),
                ("slice_crc", Json::Bool(true)),
                ("pix_fmt", Json::Str("gbrp16le".to_string())),
                ("frames", Json::Int(sequence.frames.len() as i64)),
                ("width", Json::Int(sequence.width as i64)),
                ("height", Json::Int(sequence.height as i64)),
                ("fps", Json::Str(input.options.fps.arg())),
                (
                    "encoder_parameters",
                    Json::Array(vec![
                        Json::Str("-c:v ffv1".to_string()),
                        Json::Str("-level 3".to_string()),
                        Json::Str("-g 1".to_string()),
                        Json::Str("-slicecrc 1".to_string()),
                        Json::Str("-pix_fmt gbrp16le".to_string()),
                        Json::Str("-map_metadata -1".to_string()),
                        Json::Str("-f matroska".to_string()),
                        Json::Str("no resize, no colour conversion, no frame-rate filter".to_string()),
                    ]),
                ),
                ("probe", input.linear_probe.to_json()),
                (
                    "roundtrip",
                    Json::obj(vec![
                        (
                            "method",
                            Json::Str(
                                "decode sent PNG sequence and encoded MKV to rgb48le and compare \
                                 every frame byte-for-byte"
                                    .to_string(),
                            ),
                        ),
                        (
                            "frames_compared",
                            Json::Int(input.roundtrip.frames_compared as i64),
                        ),
                        (
                            "frame_bytes",
                            Json::Int(input.roundtrip.frame_bytes as i64),
                        ),
                        (
                            "elapsed_s",
                            Json::Float(input.roundtrip.elapsed.as_secs_f64()),
                        ),
                        ("status", Json::Str("pass".to_string())),
                    ]),
                ),
            ]),
        ),
        (
            "preview",
            {
                let profile = input.options.preview_profile;
                let mut encoder_parameters = vec![
                    Json::Str("-c:v h264_videotoolbox".to_string()),
                    Json::Str("-allow_sw 0".to_string()),
                    Json::Str("-profile:v high".to_string()),
                    Json::Str("-b:v 0 -q:v 65".to_string()),
                    Json::Str("-pix_fmt yuv420p".to_string()),
                    Json::Str(format!("-vf {}", preview_filter(preview_scale, profile))),
                ];
                encoder_parameters.push(Json::Str(format!(
                    "-colorspace {} -color_primaries {} -color_trc {} -color_range {}",
                    profile.tag_policy.matrix_token(), profile.tag_policy.primaries_token(),
                    profile.tag_policy.transfer_token(), profile.range.as_str()
                )));
                if profile.tag_policy != crate::color_profile::TagPolicy::SRGB {
                    encoder_parameters.push(Json::Str("-movflags +faststart+write_colr".to_string()));
                }
                Json::obj(vec![
                    ("file", Json::Str(preview_final_name(profile).to_string())),
                    ("profile", Json::Str(profile.id.to_string())),
                    ("emitted_tags", profile.emitted_tags_json()),
                    ("bytes", Json::Int(input.preview_bytes as i64)),
                    (
                        "purpose",
                        Json::Str(
                            "review preview (display-referred sRGB or camera Log); \
                             not a measurement source"
                                .to_string(),
                        ),
                    ),
                    ("codec", Json::Str("h264".to_string())),
                    ("pix_fmt", Json::Str("yuv420p".to_string())),
                    (
                        "decoder_pix_fmt",
                        Json::Str(input.preview_probe.pix_fmt.clone()),
                    ),
                    (
                        "range",
                        Json::Str(profile.range.as_str().to_string()),
                    ),
                    (
                        "ycbcr_matrix",
                        Json::Str(profile.ycbcr_matrix.to_string()),
                    ),
                    (
                        "scale",
                        Json::obj(vec![
                            ("applied", Json::Bool(preview_scale.scaled)),
                            (
                                "algorithm",
                                if preview_scale.scaled {
                                    Json::Str("lanczos".to_string())
                                } else {
                                    Json::Null
                                },
                            ),
                            (
                                "source",
                                Json::Array(vec![
                                    Json::Int(sequence.width as i64),
                                    Json::Int(sequence.height as i64),
                                ]),
                            ),
                            (
                                "preview",
                                Json::Array(vec![
                                    Json::Int(preview_scale.width as i64),
                                    Json::Int(preview_scale.height as i64),
                                ]),
                            ),
                        ]),
                    ),
                    ("encoder_parameters", Json::Array(encoder_parameters)),
                    ("color_contract", profile.contract_json()),
                    ("probe", input.preview_probe.to_json()),
                    ("full_decode", Json::Str("pass".to_string())),
                ])
            },
        ),
        (
            "publication",
            Json::obj(vec![
                (
                    "method",
                    Json::Str(
                        "encoder output written under .rawtl-partial; ffprobe and lossless roundtrip validation; atomic rename to final names"
                            .to_string(),
                    ),
                ),
                (
                    "final_names",
                    Json::Array(vec![
                        Json::Str(LINEAR_FINAL_NAME.to_string()),
                        Json::Str(
                            preview_final_name(input.options.preview_profile).to_string(),
                        ),
                    ]),
                ),
            ]),
        ),
        ("timings_s", timings_json(input.timings)),
        ("deflicker", input.deflicker.clone()),
    ])
}

pub fn write(path: &Path, value: &Json) -> Result<()> {
    let text = json::to_pretty_string(value);
    std::fs::write(path, text).map_err(|err| {
        Error::new(
            EXIT_PUBLISH,
            format!("cannot write run manifest {}: {err}", path.display()),
        )
    })
}

fn timings_json(timings: &BTreeMap<String, Duration>) -> Json {
    Json::Object(
        timings
            .iter()
            .map(|(key, duration)| (key.clone(), Json::Float(duration.as_secs_f64())))
            .collect(),
    )
}

fn number_triple(values: [f64; 3]) -> Json {
    Json::Array(values.iter().map(|value| Json::Float(*value)).collect())
}

fn optional_number(value: Option<f64>) -> Json {
    match value {
        Some(value) => Json::Float(value),
        None => Json::Null,
    }
}
