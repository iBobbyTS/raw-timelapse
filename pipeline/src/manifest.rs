//! Run manifest: provenance and validation record written before publication.

use crate::cli::{Options, LEGACY_FPS_PROVENANCE};
use crate::encoder::ResolvedConfig;
use crate::error::{Error, Result, EXIT_PUBLISH};
use crate::frame::FrameSequence;
use crate::json::{self, Json};
use crate::video::{ProbeInfo, ResizeRecord};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

pub struct ManifestInput<'a> {
    pub options: &'a Options,
    pub input: &'a Path,
    pub output: &'a Path,
    pub sequence: &'a FrameSequence,
    pub probe: &'a ProbeInfo,
    pub bytes: u64,
    pub ffmpeg_version: String,
    pub ffprobe_version: String,
    pub timings: &'a BTreeMap<String, Duration>,
    /// Process-owned deflicker block (config, status counts, audit summary).
    pub deflicker: Json,
    pub encoder: &'a ResolvedConfig,
    pub resize: Option<ResizeRecord>,
}

pub fn build(input: &ManifestInput<'_>) -> Json {
    let sequence = input.sequence;
    let profile = input.options.preview_profile;
    let config = input.encoder;
    let range = config.effective_range(profile);
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
                (
                    "pixels_sha256",
                    frame
                        .pixels_sha256
                        .clone()
                        .map(Json::Str)
                        .unwrap_or(Json::Null),
                ),
                ("applied_ev", Json::Float(frame.exposure_ev)),
                (
                    "final_signal_clipping",
                    frame
                        .signal_clip_counts
                        .map(|counts| {
                            Json::obj(vec![
                                ("below_domain_samples", Json::Int(counts[0] as i64)),
                                ("above_domain_samples", Json::Int(counts[1] as i64)),
                            ])
                        })
                        .unwrap_or(Json::Null),
                ),
                (
                    "camera_color",
                    frame.camera_color.clone().unwrap_or(Json::Null),
                ),
                ("log", Json::Str(frame.log_relative.clone())),
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
            Json::Int(2),
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
                ("linear_dtype", Json::Str("signed f32 LE AP1/D60".into())),
                ("linear_clipping", Json::Str("none after camera-to-AP1; upstream limits remain".into())),
                (
                    "encoder_input",
                    Json::Str("planar yuv444p10le target signal (single quantization boundary)".into()),
                ),
                ("final_denominator", Json::Int(1023)),
                ("reference_gain", Json::Float(input.options.log_reference_gain)),
                ("reference_status", Json::Str(if profile.is_rev_eng() { "fixed-canon-normal-white; user gain is additional" } else { "not-applicable" }.into())),
                ("calibration_id", if profile.is_rev_eng() { Json::Str("canon-normal-white-fixed-v1".into()) } else { Json::Null }),
            ])
        ),
        (
            "video",
            {
                let encoder_arguments: Vec<Json> = config
                    .encoder_args(
                        profile,
                        sequence.width,
                        sequence.height,
                        sequence.frames.len(),
                        &input.options.fps.arg(),
                        Path::new("<output>"),
                    )
                    .into_iter()
                    .map(Json::Str)
                    .collect();
                Json::obj(vec![
                    ("file", Json::Str(config.output_name())),
                    ("profile", Json::Str(profile.id.to_string())),
                    ("emitted_tags", profile.emitted_tags_json(range)),
                    ("bytes", Json::Int(input.bytes as i64)),
                    (
                        "purpose",
                        Json::Str(
                            "encoded deliverable (display-referred sRGB or camera Log); temporal audit uses the selected profile inverse transfer"
                                .to_string(),
                        ),
                    ),
                    ("encoder", Json::Str(config.encoder.clone())),
                    ("container", Json::Str(config.container.as_str().to_string())),
                    ("codec", Json::Str(input.probe.codec_name.clone())),
                    (
                        "codec_profile",
                        config
                            .codec_profile
                            .clone()
                            .map(Json::Str)
                            .unwrap_or(Json::Null),
                    ),
                    ("width", Json::Int(input.probe.width as i64)),
                    ("height", Json::Int(input.probe.height as i64)),
                    ("frames", Json::Int(input.probe.frames as i64)),
                    ("duration_s", Json::Float(input.probe.frames as f64 / input.options.fps.as_f64())),
                    ("pix_fmt", Json::Str(config.pix_fmt.clone())),
                    (
                        "decoder_pix_fmt",
                        Json::Str(input.probe.pix_fmt.clone()),
                    ),
                    (
                        "bits_per_raw_sample",
                        input
                            .probe
                            .bits_per_raw_sample
                            .map(|bits| Json::Int(bits as i64))
                            .unwrap_or(Json::Null),
                    ),
                    (
                        "range",
                        Json::Str(range.as_str().to_string()),
                    ),
                    (
                        "range_tag",
                        Json::Str(if config.writes_range_tag() {
                            "written by the container".to_string()
                        } else {
                            "not expressible by MOV nclc; ProRes decoders assume limited range"
                                .to_string()
                        }),
                    ),
                    (
                        "ycbcr_matrix",
                        Json::Str(profile.ycbcr_matrix.to_string()),
                    ),
                    (
                        "scale",
                        match input.resize {
                            Some(record) => Json::obj(vec![
                                ("applied", Json::Bool(record.applied())),
                                (
                                    "algorithm",
                                    Json::Str("lanczos3 scene-linear FP32".to_string()),
                                ),
                                (
                                    "source",
                                    Json::Array(vec![
                                        Json::Int(record.source_width as i64),
                                        Json::Int(record.source_height as i64),
                                    ]),
                                ),
                                (
                                    "output",
                                    Json::Array(vec![
                                        Json::Int(record.width as i64),
                                        Json::Int(record.height as i64),
                                    ]),
                                ),
                            ]),
                            None => Json::obj(vec![
                                ("applied", Json::Bool(false)),
                                ("algorithm", Json::Null),
                                (
                                    "source",
                                    Json::Array(vec![
                                        Json::Int(sequence.width as i64),
                                        Json::Int(sequence.height as i64),
                                    ]),
                                ),
                                (
                                    "output",
                                    Json::Array(vec![
                                        Json::Int(sequence.width as i64),
                                        Json::Int(sequence.height as i64),
                                    ]),
                                ),
                            ]),
                        },
                    ),
                    ("encoder_parameters", Json::Array(encoder_arguments)),
                    ("color_contract", profile.contract_json()),
                    ("probe", input.probe.to_json()),
                    ("full_decode", Json::Str("pass".to_string())),
                ])
            },
        ),
        (
            "publication",
            Json::obj(vec![
                (
                    "method",
                    Json::Str(format!(
                        "streamed encoder output under .rawtl-partial; ffprobe and full decode validation; single rename to {}",
                        config.output_name()
                    )),
                ),
                (
                    "final_names",
                    Json::Array(vec![Json::Str(config.output_name())]),
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
