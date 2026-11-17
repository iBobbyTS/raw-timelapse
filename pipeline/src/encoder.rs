//! Resolve the encode node's legacy settings and export profiles, and build
//! the FFmpeg commands used for preflight and streaming.
//!
//! Process spawning, hardware probing, preflight execution and output
//! validation live in the binary. The legacy path keeps its planar 10-/12-bit
//! 4:4:4 handoff; export profiles specify their own rawvideo format.

use crate::color_profile::{ColorProfile, PreviewRange, SignalDepth};
use crate::export::{ExportProfile, InputFormat, Selection};
use crate::json::Json;

pub const DEFAULT_ENCODER: &str = "prores_ks";
/// Closed encoder surface. Unknown names are rejected before any RAW work so a
/// typo can never silently select a different encoder or pixel format.
pub const SUPPORTED_ENCODERS: [&str; 3] = ["prores_ks", "libx264", "ffv1"];
pub const CONTAINERS: [&str; 3] = ["mov", "mp4", "matroska"];
/// Input handoff pixel format for the run's signal depth; the single
/// quantization boundary. Use [`SignalDepth::pix_fmt`].
/// Bounded output-option surface: `-i`, a second output, filters, geometry,
/// frame rate, codec/pixel-format selection, colour interpretation and log
/// files are pipeline-owned and therefore not accepted.
pub const OUTPUT_OPTION_KEYS: [&str; 7] = [
    "-preset", "-tune", "-vendor", "-threads", "-g", "-bf", "-level",
];
pub const MAX_OUTPUT_OPTIONS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Mov,
    Mp4,
    Matroska,
}

impl Container {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "mov" => Some(Container::Mov),
            "mp4" => Some(Container::Mp4),
            "matroska" => Some(Container::Matroska),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Container::Mov => "mov",
            Container::Mp4 => "mp4",
            Container::Matroska => "matroska",
        }
    }
    pub fn ffmpeg_name(self) -> &'static str {
        self.as_str()
    }
    pub fn extension(self) -> &'static str {
        match self {
            Container::Mov => "mov",
            Container::Mp4 => "mp4",
            Container::Matroska => "mkv",
        }
    }
    pub fn media_type(self) -> &'static str {
        match self {
            Container::Mov => "video/quicktime",
            Container::Mp4 => "video/mp4",
            Container::Matroska => "video/x-matroska",
        }
    }
    pub fn writes_movflags(self) -> bool {
        matches!(self, Container::Mov | Container::Mp4)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateControl {
    Crf(u32),
    QScale(u32),
    Bitrate(u64),
}

impl RateControl {
    pub fn key(self) -> &'static str {
        match self {
            RateControl::Crf(_) => "crf",
            RateControl::QScale(_) => "qscale",
            RateControl::Bitrate(_) => "bitrate",
        }
    }
    pub fn args(self) -> Vec<String> {
        match self {
            RateControl::Crf(value) => vec!["-crf".into(), value.to_string()],
            // libx264 ignores the generic `-q:v`; `-qp` selects its constant
            // quantization-parameter mode explicitly.
            RateControl::QScale(value) => vec!["-qp".into(), value.to_string()],
            RateControl::Bitrate(value) => vec!["-b:v".into(), value.to_string()],
        }
    }
}

/// One layer of explicit configuration. A field left `None` inherits from the
/// lower-priority layer, so priority is `built-in < graph node < explicit CLI`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Patch {
    pub export: Option<Selection>,
    pub encoder: Option<String>,
    pub pix_fmt: Option<String>,
    pub container: Option<Container>,
    pub profile: Option<String>,
    pub rate: Option<RateControl>,
    /// `None` inherits; `Some` replaces the lower layer's array entirely.
    pub options: Option<Vec<(String, String)>>,
}

impl Patch {
    pub fn is_empty(&self) -> bool {
        self.export.is_none()
            && self.encoder.is_none()
            && self.pix_fmt.is_none()
            && self.container.is_none()
            && self.profile.is_none()
            && self.rate.is_none()
            && self.options.is_none()
    }

    /// Parse the graph node's `encoder` object. Unknown keys are rejected, so
    /// an unsupported intent never degrades into a silently ignored field.
    pub fn from_json(value: &Json) -> Result<Self, String> {
        let Json::Object(entries) = value else {
            return Err("graph: encoder must be an object".into());
        };
        let mut patch = Patch::default();
        let mut rate: Option<RateControl> = None;
        let mut export_name = None;
        let mut input_format = None;
        let mut ffmpeg_args = None;
        let mut seen = std::collections::BTreeSet::new();
        for (key, value) in entries {
            if !seen.insert(key.as_str()) {
                return Err(format!("graph: duplicate encoder field {key}"));
            }
            match key.as_str() {
                "export_profile" => export_name = Some(nonempty_string(value, key)?),
                "input_format" => {
                    input_format = Some(InputFormat::parse(&nonempty_string(value, key)?)?)
                }
                "ffmpeg_args" => ffmpeg_args = Some(string_array(value, key)?),
                "encoder" => patch.encoder = Some(nonempty_string(value, "encoder")?),
                "pix_fmt" => patch.pix_fmt = Some(nonempty_string(value, "pix_fmt")?),
                "container" => {
                    let text = nonempty_string(value, "container")?;
                    patch.container = Some(
                        Container::parse(&text)
                            .ok_or_else(|| format!("graph: unsupported container {text:?}"))?,
                    );
                }
                "profile" => patch.profile = Some(profile_string(value)?),
                "crf" | "qscale" | "bitrate" => {
                    if rate.is_some() {
                        return Err(
                            "graph: encoder accepts at most one of crf, qscale or bitrate".into()
                        );
                    }
                    let number = value
                        .as_u64()
                        .ok_or_else(|| format!("graph: encoder {key} must be an integer"))?;
                    rate = Some(match key.as_str() {
                        "crf" => RateControl::Crf(bounded(number, 0, 63, "crf")? as u32),
                        "qscale" => RateControl::QScale(bounded(number, 0, 63, "qscale")? as u32),
                        _ => RateControl::Bitrate(bounded(number, 1, 1_000_000_000, "bitrate")?),
                    });
                }
                "options" => {
                    patch.options = Some(parse_output_options(&string_array(value, key)?)?);
                }
                other => return Err(format!("graph: unknown encoder field {other}")),
            }
        }
        patch.rate = rate;
        patch.export = match (export_name, input_format, ffmpeg_args) {
            (Some(name), None, None) => Some(Selection::Named(name)),
            (None, Some(format), Some(args)) => {
                Some(Selection::Manual(ExportProfile::manual(format, args)?))
            }
            (None, None, None) => None,
            (Some(_), _, _) => return Err("graph: export_profile already defines input_format and ffmpeg_args".into()),
            _ => return Err("graph: manual export requires both input_format and ffmpeg_args".into()),
        };
        if patch.export.is_some()
            && (patch.encoder.is_some() || patch.pix_fmt.is_some() || patch.profile.is_some()
                || patch.rate.is_some() || patch.options.is_some())
        {
            return Err("graph: export profiles and manual FFmpeg arguments replace legacy encoder options".into());
        }
        Ok(patch)
    }
}

fn string_array(value: &Json, field: &str) -> Result<Vec<String>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("graph: encoder {field} must be an array"))?
        .iter()
        .map(|token| {
            token.as_str().map(str::to_owned)
                .ok_or_else(|| format!("graph: encoder {field} must contain only strings"))
        })
        .collect()
}

fn nonempty_string(value: &Json, field: &str) -> Result<String, String> {
    let text = value
        .as_str()
        .ok_or_else(|| format!("graph: encoder {field} must be a string"))?;
    if text.is_empty() || text.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("graph: invalid encoder {field} {text:?}"));
    }
    Ok(text.to_string())
}

fn profile_string(value: &Json) -> Result<String, String> {
    match value {
        Json::Str(text) if !text.is_empty() => Ok(text.clone()),
        Json::Int(number) => Ok(number.to_string()),
        Json::Float(number) if number.fract() == 0.0 => Ok(format!("{number}")),
        _ => Err("graph: encoder profile must be a string or integer".into()),
    }
}

fn bounded(value: u64, low: u64, high: u64, name: &str) -> Result<u64, String> {
    if (low..=high).contains(&value) {
        Ok(value)
    } else {
        Err(format!(
            "graph: encoder {name} {value} is outside {low}..={high}"
        ))
    }
}

/// Parse the bounded output-option argv array. Each pair is either
/// `["-flag", "value", ...]` or `["-flag=value", ...]`; a bare token is never a
/// second output.
pub fn parse_output_options(tokens: &[String]) -> Result<Vec<(String, String)>, String> {
    if tokens.len() > MAX_OUTPUT_OPTIONS * 2 {
        return Err(format!(
            "encoder options exceed {MAX_OUTPUT_OPTIONS} entries"
        ));
    }
    let mut pairs = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        if !token.starts_with('-') {
            return Err(format!(
                "encoder output option {token:?} is not a flag; options cannot name inputs or outputs"
            ));
        }
        let (key, value) = match token.split_once('=') {
            Some((key, value)) if !value.is_empty() => (key.to_string(), value.to_string()),
            Some(_) => return Err(format!("encoder output option {token:?} has no value")),
            None => {
                let value = tokens.get(index + 1).ok_or_else(|| {
                    format!("encoder output option {token:?} has no value")
                })?;
                if value.starts_with('-') {
                    return Err(format!(
                        "encoder output option {token:?} has no value (next token is a flag)"
                    ));
                }
                index += 1;
                (token.to_string(), value.clone())
            }
        };
        if !OUTPUT_OPTION_KEYS.contains(&key.as_str()) {
            return Err(format!(
                "encoder output option {key:?} is not supported; allowed keys: {}",
                OUTPUT_OPTION_KEYS.join(", ")
            ));
        }
        if value.is_empty()
            || value.len() > 256
            || value.chars().any(|c| c.is_control() || c == '\0')
        {
            return Err(format!("encoder output option {key:?} has an invalid value"));
        }
        if pairs.iter().any(|(existing, _)| existing == &key) {
            return Err(format!("duplicate encoder output option {key:?}"));
        }
        pairs.push((key, value));
        index += 1;
    }
    if pairs.len() > MAX_OUTPUT_OPTIONS {
        return Err(format!(
            "encoder options exceed {MAX_OUTPUT_OPTIONS} entries"
        ));
    }
    Ok(pairs)
}

/// The resolved encoder contract used by preflight, the streaming encoder and
/// output validation.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedConfig {
    pub encoder: String,
    pub pix_fmt: String,
    pub container: Container,
    pub codec_profile: Option<String>,
    pub rate: Option<RateControl>,
    pub options: Vec<(String, String)>,
    export: Option<ExportProfile>,
}

struct Preset {
    pix_fmt: &'static str,
    container: Container,
    profile: Option<&'static str>,
    rate: Option<RateControl>,
}

fn preset(encoder: &str) -> Option<Preset> {
    match encoder {
        "prores_ks" => Some(Preset {
            pix_fmt: "yuv444p10le",
            container: Container::Mov,
            profile: Some("4"),
            rate: None,
        }),
        "libx264" => Some(Preset {
            pix_fmt: "yuv420p",
            container: Container::Mp4,
            profile: Some("high"),
            rate: Some(RateControl::Crf(18)),
        }),
        "ffv1" => Some(Preset {
            pix_fmt: "yuv444p10le",
            container: Container::Matroska,
            profile: None,
            rate: None,
        }),
        _ => None,
    }
}

impl ResolvedConfig {
    pub fn from_export(export: ExportProfile, container: Container) -> Self {
        Self {
            encoder: export.encoder.clone(),
            pix_fmt: export.decoder_or_encoder_format().unwrap_or("auto").into(),
            container,
            codec_profile: export.decoded_profile.map(str::to_owned),
            rate: None,
            options: Vec::new(),
            export: Some(export),
        }
    }

    pub fn export(&self) -> Option<&ExportProfile> {
        self.export.as_ref()
    }

    /// Resolve `built-in < graph node < explicit CLI`. Each named field wins
    /// independently; unnamed fields inherit the next lower layer.
    pub fn resolve(
        graph: Option<&Patch>,
        cli: &Patch,
        depth: SignalDepth,
    ) -> Result<Self, String> {
        let graph = graph.unwrap_or(&EMPTY_PATCH);
        if graph.export.is_some() || cli.export.is_some() {
            return Err("resolve the export profile before building the encoder configuration".into());
        }
        // The lower layer's effective encoder includes the built-in default:
        // restating the encoder that is already in effect is not a codec change.
        let lower_encoder = graph
            .encoder
            .clone()
            .unwrap_or_else(|| DEFAULT_ENCODER.to_string());
        let encoder = cli
            .encoder
            .clone()
            .unwrap_or_else(|| lower_encoder.clone());
        let preset = preset(&encoder).ok_or_else(|| {
            format!(
                "unsupported encoder {encoder:?}; supported encoders: {}",
                SUPPORTED_ENCODERS.join(", ")
            )
        })?;
        // When no explicit pixel format was requested, the output format
        // follows the handoff depth for the 4:4:4 presets (prores_ks, ffv1) so
        // a 12-bit handoff is encoded, not scaled back down. libx264 keeps its
        // 8-bit 4:2:0 preset; FFmpeg's adapter converts either depth.
        let depth_default = if depth == SignalDepth::Twelve
            && matches!(encoder.as_str(), "prores_ks" | "ffv1")
        {
            depth.pix_fmt().to_string()
        } else {
            preset.pix_fmt.to_string()
        };
        let pix_fmt = cli
            .pix_fmt
            .clone()
            .or_else(|| graph.pix_fmt.clone())
            .unwrap_or(depth_default);
        let container = cli
            .container
            .or(graph.container)
            .unwrap_or(preset.container);
        // Codec profile and rate control are encoder-scoped. When the CLI names
        // a different encoder, inheriting them from another encoder would either
        // be meaningless or impossible to clear; CLI intent wins instead.
        let encoder_changed_by_cli = cli
            .encoder
            .as_deref()
            .is_some_and(|named| named != lower_encoder);
        let codec_profile = cli
            .profile
            .clone()
            .or_else(|| {
                if encoder_changed_by_cli {
                    None
                } else {
                    graph.profile.clone()
                }
            })
            .or_else(|| preset.profile.map(str::to_string));
        let rate = cli
            .rate
            .or_else(|| {
                if encoder_changed_by_cli {
                    None
                } else {
                    graph.rate
                }
            })
            .or(preset.rate);
        let options = cli
            .options
            .clone()
            .or_else(|| graph.options.clone())
            .unwrap_or_default();
        let config = ResolvedConfig {
            encoder,
            pix_fmt,
            container,
            codec_profile,
            rate,
            options,
            export: None,
        };
        config.validate()?;
        // prores_ks encodes from 10-bit input only (its FFmpeg encoder lists
        // yuv422p10le/yuv444p10le/yuva444p10le regardless of profile), so a
        // 12-bit handoff has nowhere to go. Reject before any RAW work unless
        // the caller explicitly picked a pixel format the encoder accepts.
        if depth == SignalDepth::Twelve
            && config.encoder == "prores_ks"
            && cli.pix_fmt.is_none()
            && graph.pix_fmt.is_none()
        {
            return Err(
                "--signal-bits 12 cannot reach prores_ks (10-bit encoder input); use \
                 --signal-bits 10, or --encoder ffv1 for true 12-bit delivery"
                    .into(),
            );
        }
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        if self.pix_fmt.is_empty()
            || !self
                .pix_fmt
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(format!("invalid pixel format {:?}", self.pix_fmt));
        }
        if has_alpha(&self.pix_fmt) {
            return Err(format!(
                "pixel format {:?} carries alpha; alpha planes are not generated",
                self.pix_fmt
            ));
        }
        match self.encoder.as_str() {
            "prores_ks" => {
                if self.container == Container::Mp4 {
                    return Err(
                        "prores_ks cannot be muxed in MP4; use the MOV container".into(),
                    );
                }
                let profile = self
                    .codec_profile
                    .as_deref()
                    .map(parse_prores_profile)
                    .transpose()?
                    .unwrap_or(4);
                if profile >= 4 && !self.pix_fmt.contains("444") {
                    return Err(format!(
                        "ProRes 4444 profile requires a 4:4:4 pixel format, got {:?}",
                        self.pix_fmt
                    ));
                }
                if self.rate.is_some() {
                    return Err(format!(
                        "ProRes does not accept {} rate control",
                        self.rate.map(RateControl::key).unwrap_or("")
                    ));
                }
            }
            "libx264" => {
                if let Some(profile) = &self.codec_profile {
                    if ![
                        "baseline",
                        "main",
                        "high",
                        "high10",
                        "high422",
                        "high444",
                    ]
                    .contains(&profile.as_str())
                    {
                        return Err(format!(
                            "unsupported H.264 profile {profile:?}; expected baseline, main, \
                             high, high10, high422 or high444"
                        ));
                    }
                }
            }
            "ffv1" => {
                if self.codec_profile.is_some() {
                    return Err("FFV1 has no codec profile; remove the profile setting".into());
                }
                if self.rate.is_some() {
                    return Err("FFV1 is lossless and does not accept rate control".into());
                }
            }
            other => return Err(format!("unsupported encoder {other:?}")),
        }
        validate_options(&self.options)?;
        Ok(())
    }

    pub fn codec_name(&self) -> &'static str {
        if let Some(export) = &self.export {
            return export.codec.unwrap_or("unknown");
        }
        match self.encoder.as_str() {
            "prores_ks" => "prores",
            "libx264" => "h264",
            "ffv1" => "ffv1",
            _ => "unknown",
        }
    }

    /// ffprobe's ProRes profile label for the configured profile value.
    pub fn probe_profile(&self) -> Option<&'static str> {
        if let Some(export) = &self.export {
            return export.decoded_profile;
        }
        if self.encoder != "prores_ks" {
            return None;
        }
        match parse_prores_profile(self.codec_profile.as_deref().unwrap_or("4")).ok()? {
            0 => Some("Proxy"),
            1 => Some("LT"),
            2 => Some("Standard"),
            3 => Some("HQ"),
            4 => Some("4444"),
            5 => Some("XQ"),
            _ => None,
        }
    }

    pub fn preset_label(&self) -> &'static str {
        match self.encoder.as_str() {
            "prores_ks" => "prores-4444",
            "libx264" => "h264",
            "ffv1" => "ffv1-lossless",
            _ => "unknown",
        }
    }

    /// Decoder pixel formats ffprobe may report for this encoder/container.
    pub fn decoder_pix_fmts(&self, range: PreviewRange) -> Vec<&str> {
        if self.export.as_ref().is_some_and(|export| export.decoded_format.is_none()) {
            return vec![];
        }
        match self.encoder.as_str() {
            "prores_ks" => {
                if self.pix_fmt.contains("444") {
                    vec!["yuv444p12le", "yuv444p10le"]
                } else {
                    vec!["yuv422p12le", "yuv422p10le"]
                }
            }
            _ if matches!(self.codec_name(), "h264" | "hevc") => {
                // The encoder preserves the configured depth/subsampling; an
                // 8-bit full-range stream may decode under the legacy yuvj*
                // alias. Keep configured-format validation, not a fixed 4:2:0.
                let mut formats = vec![self.pix_fmt.as_str()];
                if range == PreviewRange::Pc {
                    let alias = match self.pix_fmt.as_str() {
                        "yuv420p" => Some("yuvj420p"),
                        "yuv422p" => Some("yuvj422p"),
                        "yuv444p" => Some("yuvj444p"),
                        _ => None,
                    };
                    if let Some(alias) = alias {
                        formats.push(alias);
                    }
                }
                formats
            }
            // FFV1 is lossless and preserves the configured pixel format.
            _ => vec![self.pix_fmt.as_str()],
        }
    }

    pub fn output_name(&self) -> String {
        format!("output.{}", self.container.extension())
    }

    pub fn media_type(&self) -> &'static str {
        self.container.media_type()
    }

    /// RGB is full range. ProRes and DNxHR use video range to match decoder
    /// conventions, including MOV with legacy `nclc` metadata.
    pub fn effective_range(&self, profile: &ColorProfile) -> PreviewRange {
        if self.export_output_rgb() == Some(true) {
            PreviewRange::Pc
        } else if matches!(self.encoder.as_str(), "prores_ks" | "prores" | "prores_aw" | "dnxhd") {
            PreviewRange::Tv
        } else {
            profile.range
        }
    }

    /// Whether the container/encoder combination writes an explicit range tag.
    pub fn writes_range_tag(&self) -> bool {
        !matches!(self.encoder.as_str(), "prores_ks" | "prores" | "prores_aw" | "dnxhd")
    }

    /// Whether an explicitly selected encoder output format is RGB.
    pub fn export_output_rgb(&self) -> Option<bool> {
        match &self.export {
            None => Some(false),
            Some(export) => export.encoder_format.as_deref().map(|format| {
                ["gbr", "rgb", "bgr", "argb", "abgr", "x2rgb", "x2bgr"]
                    .iter().any(|prefix| format.starts_with(prefix))
            }),
        }
    }

    /// 4:2:0 formats need even width and height; 4:2:2 needs even width. Other
    /// formats (including 4:4:4 ProRes) accept odd dimensions.
    pub fn validate_geometry(&self, width: u32, height: u32) -> Result<(), String> {
        if let Some(export) = &self.export {
            export.input.validate_geometry(width, height)?;
        }
        if width == 0 || height == 0 {
            return Err(format!("invalid output geometry {width}x{height}"));
        }
        let semiplanar_420 = self.export.is_some()
            && ["nv12", "nv21", "p010", "p012", "p016"]
                .iter()
                .any(|format| self.pix_fmt.starts_with(format));
        let semiplanar_422 = self.export.is_some()
            && ["p210", "p212", "p216"]
                .iter()
                .any(|format| self.pix_fmt.starts_with(format));
        let even_width = self.pix_fmt.contains("420")
            || self.pix_fmt.contains("422")
            || self.pix_fmt.contains("411")
            || self.pix_fmt.contains("410")
            || semiplanar_420
            || semiplanar_422;
        let even_height = self.pix_fmt.contains("420") || self.pix_fmt.contains("440") || semiplanar_420;
        if (even_width && width % 2 != 0) || (even_height && height % 2 != 0) {
            return Err(format!(
                "pixel format {:?} cannot encode {}x{}: chroma subsampling requires even \
                 dimensions; no implicit rounding or scaling is applied",
                self.pix_fmt, width, height
            ));
        }
        Ok(())
    }

    /// `setparams` half of the input filter chain, and the pixel-format adapter
    /// when the codec's pixel format is not the 4:4:4 handoff.
    pub fn filter_chain(&self, profile: &ColorProfile, depth: SignalDepth) -> String {
        let range = self.effective_range(profile);
        let matrix = profile.scale_filter_token();
        let tags = format!(
            "setparams=color_primaries={}:color_trc={}:colorspace={}:range={}",
            profile.tag_policy.primaries_token(),
            profile.tag_policy.transfer_token(),
            profile.tag_policy.matrix_token(),
            range.as_str()
        );
        if self.pix_fmt == depth.pix_fmt() {
            tags
        } else {
            format!(
                "scale=in_range={r}:in_color_matrix={m}:out_range={r}:out_color_matrix={m},{tags}",
                r = range.as_str(),
                m = matrix
            )
        }
    }

    /// Complete FFmpeg argument vector for one streaming encode. Every
    /// pipeline-owned parameter (input, geometry, rate, filters, colour, the
    /// single output) is fixed here; configured options are appended verbatim
    /// only after allowlist validation.
    pub fn encoder_args(
        &self,
        profile: &ColorProfile,
        width: u32,
        height: u32,
        frames: usize,
        fps: &str,
        depth: SignalDepth,
        output: &std::path::Path,
    ) -> Vec<String> {
        if self.export.is_some() {
            return self.export_args(profile, (width, height), fps, frames, output, false);
        }
        let range = self.effective_range(profile);
        let mut args = vec![
            "-hide_banner".into(),
            "-v".into(),
            "error".into(),
            "-f".into(),
            "rawvideo".into(),
            "-pixel_format".into(),
            depth.pix_fmt().into(),
            "-video_size".into(),
            format!("{width}x{height}"),
            "-framerate".into(),
            fps.into(),
            "-i".into(),
            "pipe:0".into(),
            "-frames:v".into(),
            frames.to_string(),
            "-vf".into(),
            self.filter_chain(profile, depth),
            "-c:v".into(),
            self.encoder.clone(),
            "-pix_fmt".into(),
            self.pix_fmt.clone(),
        ];
        if let Some(profile_value) = &self.codec_profile {
            args.push("-profile:v".into());
            args.push(profile_value.clone());
        }
        if let Some(rate) = self.rate {
            args.extend(rate.args());
        }
        if self.encoder == "ffv1" {
            // 10-bit 4:4:4 requires FFV1 level 3.
            args.push("-level".into());
            args.push("3".into());
        }
        for (key, value) in &self.options {
            args.push(key.clone());
            args.push(value.clone());
        }
        args.extend(colour_args(profile, range));
        if self.container.writes_movflags() {
            args.push("-movflags".into());
            args.push("+faststart+write_colr".into());
        }
        args.push("-f".into());
        args.push(self.container.ffmpeg_name().into());
        args.push(output.display().to_string());
        args
    }

    /// Encoder/muxer capability arguments for the early preflight, with a
    /// synthetic input instead of the RAW-derived pipe.
    pub fn preflight_args(
        &self,
        profile: &ColorProfile,
        geometry: (u32, u32),
        fps: &str,
        output: &std::path::Path,
    ) -> Vec<String> {
        if self.export.is_some() {
            return self.export_args(profile, geometry, fps, 1, output, true);
        }
        let (width, height) = geometry;
        let range = self.effective_range(profile);
        let mut args = vec![
            "-hide_banner".into(),
            "-nostdin".into(),
            // Warnings expose an encoder pixel-format substitution instead of
            // hiding it; the transcript is ever only surfaced on failure.
            "-v".into(),
            "warning".into(),
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            format!("color=c=black:s={width}x{height}:r={fps}"),
            "-frames:v".into(),
            "1".into(),
            "-pix_fmt".into(),
            self.pix_fmt.clone(),
            "-c:v".into(),
            self.encoder.clone(),
        ];
        if let Some(profile_value) = &self.codec_profile {
            args.push("-profile:v".into());
            args.push(profile_value.clone());
        }
        if let Some(rate) = self.rate {
            args.extend(rate.args());
        }
        if self.encoder == "ffv1" {
            args.push("-level".into());
            args.push("3".into());
        }
        for (key, value) in &self.options {
            args.push(key.clone());
            args.push(value.clone());
        }
        args.extend(colour_args(profile, range));
        if self.container.writes_movflags() {
            args.push("-movflags".into());
            args.push("+faststart+write_colr".into());
        }
        args.push("-f".into());
        args.push(self.container.ffmpeg_name().into());
        args.push(output.display().to_string());
        args
    }

    pub fn summary(&self, profile: &ColorProfile) -> String {
        format!(
            "encoder={} container={} pix_fmt={} profile={} range={}{}",
            self.encoder,
            self.container.as_str(),
            self.pix_fmt,
            self.codec_profile.as_deref().unwrap_or("none"),
            self.effective_range(profile).as_str(),
            self.rate
                .map(|rate| format!(" {}({})", rate.key(), rate.args().join(" ")))
                .unwrap_or_default()
        )
    }

    fn export_args(
        &self,
        profile: &ColorProfile,
        (width, height): (u32, u32),
        fps: &str,
        frames: usize,
        output: &std::path::Path,
        preflight: bool,
    ) -> Vec<String> {
        let export = self.export.as_ref().unwrap();
        let mut args = vec![
            "-hide_banner".into(), "-nostdin".into(), "-v".into(),
            if preflight { "warning" } else { "error" }.into(),
            "-abort_on".into(), "empty_output".into(),
        ];
        args.extend(export.device_args.iter().cloned());
        if preflight {
            args.extend([
                "-f".into(), "lavfi".into(), "-i".into(),
                format!("color=c=black:s={width}x{height}:r={fps},format={}", export.input.name()),
            ]);
        } else {
            args.extend([
                "-f".into(), "rawvideo".into(), "-pixel_format".into(), export.input.name().into(),
                "-video_size".into(), format!("{width}x{height}"),
                "-framerate".into(), fps.into(), "-i".into(), "pipe:0".into(),
            ]);
        }
        args.extend(["-frames:v".into(), frames.to_string(),
            "-vf".into(), self.export_filter(profile, export)]);
        if export.input.is_subsampled() && export.decoder_or_encoder_format() == Some(export.input.name()) {
            args.extend(["-chroma_sample_location".into(), "center".into()]);
        }
        args.extend(export.args.iter().cloned());
        let rgb = self.export_output_rgb();
        if let Some(rgb) = rgb {
            args.extend([
                "-colorspace".into(), if rgb { "rgb" } else { profile.tag_policy.matrix_token() }.into(),
                "-color_range".into(), self.effective_range(profile).as_str().into(),
            ]);
        }
        args.extend([
            "-color_primaries".into(), profile.tag_policy.primaries_token().into(),
            "-color_trc".into(), profile.tag_policy.transfer_token().into(),
        ]);
        if self.container.writes_movflags() {
            args.extend(["-movflags".into(), "+faststart+write_colr".into()]);
        }
        args.extend(["-f".into(), self.container.ffmpeg_name().into(), output.display().to_string()]);
        args
    }

    fn export_filter(&self, profile: &ColorProfile, export: &ExportProfile) -> String {
        let range = self.effective_range(profile).as_str();
        let rgb_input = export.input.is_rgb();
        let input_range = if rgb_input { "pc" } else { range };
        let input_matrix = if rgb_input { "gbr" } else { profile.tag_policy.matrix_token() };
        let mut filter = format!(
            "setparams=color_primaries={}:color_trc={}:colorspace={input_matrix}:range={input_range}",
            profile.tag_policy.primaries_token(), profile.tag_policy.transfer_token(),
        );
        if let Some(hardware) = export.hardware_filter() {
            filter.push(',');
            filter.push_str(&hardware);
        } else if export.encoder_format.as_deref() != Some(export.input.name()) {
            let matrix = profile.scale_filter_token();
            filter.push_str(&format!(
                ",scale=in_range={input_range}:in_color_matrix={matrix}:out_range={range}:out_color_matrix={matrix}",
            ));
        }
        filter
    }
}

fn colour_args(profile: &ColorProfile, range: PreviewRange) -> Vec<String> {
    vec![
        "-colorspace".into(),
        profile.tag_policy.matrix_token().into(),
        "-color_primaries".into(),
        profile.tag_policy.primaries_token().into(),
        "-color_trc".into(),
        profile.tag_policy.transfer_token().into(),
        "-color_range".into(),
        range.as_str().into(),
    ]
}

fn validate_options(options: &[(String, String)]) -> Result<(), String> {
    if options.len() > MAX_OUTPUT_OPTIONS {
        return Err(format!(
            "encoder options exceed {MAX_OUTPUT_OPTIONS} entries"
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for (key, value) in options {
        if !OUTPUT_OPTION_KEYS.contains(&key.as_str()) {
            return Err(format!(
                "encoder output option {key:?} is not supported; allowed keys: {}",
                OUTPUT_OPTION_KEYS.join(", ")
            ));
        }
        if !seen.insert(key.as_str()) {
            return Err(format!("duplicate encoder output option {key:?}"));
        }
        if value.is_empty()
            || value.len() > 256
            || value.chars().any(|c| c.is_control() || c == '\0')
        {
            return Err(format!("encoder output option {key:?} has an invalid value"));
        }
    }
    Ok(())
}

fn has_alpha(pix_fmt: &str) -> bool {
    ["yuva", "rgba", "bgra", "argb", "abgr", "gbrap", "ya8", "ya16"]
        .iter()
        .any(|token| pix_fmt.contains(token))
}

fn parse_prores_profile(value: &str) -> Result<u32, String> {
    match value {
        "proxy" => Ok(0),
        "lt" => Ok(1),
        "standard" => Ok(2),
        "hq" => Ok(3),
        "4444" => Ok(4),
        "4444xq" => Ok(5),
        other => other
            .parse::<u32>()
            .ok()
            .filter(|profile| *profile <= 5)
            .ok_or_else(|| {
                format!(
                    "unsupported ProRes profile {value:?}; expected 0..=5, proxy, lt, standard, \
                     hq, 4444 or 4444xq"
                )
            }),
    }
}

static EMPTY_PATCH: Patch = Patch {
    export: None,
    encoder: None,
    pix_fmt: None,
    container: None,
    profile: None,
    rate: None,
    options: None,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color_profile::lookup;
    use std::path::Path;

    #[test]
    fn twelve_bit_handoff_threads_the_depth_through_the_ffmpeg_argv() {
        let cli = Patch {
            encoder: Some("ffv1".into()),
            ..Default::default()
        };
        let config = ResolvedConfig::resolve(None, &cli, SignalDepth::Twelve).unwrap();
        // The output format follows the handoff depth without an explicit
        // --pix-fmt, so the argv feeds 12-bit words straight into the encoder.
        assert_eq!(config.pix_fmt, "yuv444p12le");
        let profile = lookup("dji-dlog2-dgamut2").unwrap();
        let args = config.encoder_args(profile, 16, 12, 1, "30/1", SignalDepth::Twelve, Path::new("out.mkv"));
        let input_format = args
            .iter()
            .zip(args.iter().skip(1))
            .find(|(key, _)| key.as_str() == "-pixel_format")
            .map(|(_, value)| value.as_str())
            .expect("rawvideo input format");
        assert_eq!(input_format, "yuv444p12le");
        let output_format = args
            .iter()
            .zip(args.iter().skip(1))
            .find(|(key, _)| key.as_str() == "-pix_fmt")
            .map(|(_, value)| value.as_str())
            .expect("encoder output format");
        assert_eq!(output_format, "yuv444p12le");
        // Matching depths mean no scale adapter: the filter chain only tags.
        let vf = args
            .iter()
            .zip(args.iter().skip(1))
            .find(|(key, _)| key.as_str() == "-vf")
            .map(|(_, value)| value.as_str())
            .expect("filter chain");
        assert!(!vf.contains("scale="), "unexpected adapter: {vf}");
        assert!(vf.contains("setparams="), "missing colour tags: {vf}");
    }

    #[test]
    fn signal_bits_twelve_is_rejected_for_prores_unless_a_format_is_explicit() {
        let error = ResolvedConfig::resolve(None, &Patch::default(), SignalDepth::Twelve).unwrap_err();
        assert!(
            error.contains("--signal-bits 12 cannot reach prores_ks"),
            "{error}"
        );
        // An explicit encoder-accepted format keeps the combination legal: the
        // scale adapter then downconverts the 12-bit handoff at the encoder input.
        let cli = Patch {
            pix_fmt: Some("yuv444p10le".into()),
            ..Default::default()
        };
        let config = ResolvedConfig::resolve(None, &cli, SignalDepth::Twelve).unwrap();
        assert_eq!(config.pix_fmt, "yuv444p10le");
        let profile = lookup("srgb-preview").unwrap();
        let vf = config.filter_chain(profile, SignalDepth::Twelve);
        assert!(vf.starts_with("scale="), "expected an adapter: {vf}");
    }

    fn resolve(graph: &Patch, cli: &Patch) -> ResolvedConfig {
        ResolvedConfig::resolve(Some(graph), cli, SignalDepth::Ten).expect("resolve")
    }

    #[test]
    fn built_in_default_is_prores_4444_mov_at_the_single_10_bit_handoff() {
        let config = resolve(&Patch::default(), &Patch::default());
        assert_eq!(config.encoder, "prores_ks");
        assert_eq!(config.pix_fmt, "yuv444p10le");
        assert_eq!(config.container, Container::Mov);
        assert_eq!(config.codec_profile.as_deref(), Some("4"));
        assert_eq!(config.rate, None);
        assert_eq!(config.output_name(), "output.mov");
        assert_eq!(config.media_type(), "video/quicktime");
        assert_eq!(config.codec_name(), "prores");
        let profile = lookup("srgb-preview").unwrap();
        assert_eq!(config.effective_range(profile), PreviewRange::Tv);
        assert!(!config.writes_range_tag());
        assert!(config
            .decoder_pix_fmts(PreviewRange::Tv)
            .contains(&"yuv444p12le"));
    }

    #[test]
    fn explicit_cli_fields_override_the_graph_node_per_field() {
        let graph = Patch {
            export: None,
            encoder: Some("libx264".into()),
            pix_fmt: Some("yuv420p".into()),
            container: Some(Container::Mp4),
            profile: Some("high".into()),
            rate: Some(RateControl::Crf(20)),
            options: Some(vec![("-preset".into(), "slow".into())]),
        };
        let config = ResolvedConfig::resolve(Some(&graph), &Patch::default(), SignalDepth::Ten).unwrap();
        assert_eq!(config.encoder, "libx264");
        assert_eq!(config.rate, Some(RateControl::Crf(20)));
        assert_eq!(config.output_name(), "output.mp4");

        let cli = Patch {
            encoder: Some("ffv1".into()),
            container: Some(Container::Matroska),
            rate: None,
            ..Default::default()
        };
        // CLI names encoder/container; the graph still supplies the pixel
        // format and options that the CLI did not name.
        let merged = ResolvedConfig::resolve(Some(&graph), &cli, SignalDepth::Ten).unwrap();
        assert_eq!(merged.encoder, "ffv1");
        assert_eq!(merged.container, Container::Matroska);
        assert_eq!(merged.pix_fmt, "yuv420p");
        assert_eq!(merged.options.len(), 1);
        assert_eq!(merged.output_name(), "output.mkv");
    }

    #[test]
    fn cli_options_replace_graph_options_and_encoder_defaults_fill_unset_fields() {
        let graph = Patch {
            options: Some(vec![("-preset".into(), "slow".into())]),
            ..Default::default()
        };
        let cli = Patch {
            encoder: Some("libx264".into()),
            options: Some(vec![("-preset".into(), "medium".into())]),
            ..Default::default()
        };
        let config = ResolvedConfig::resolve(Some(&graph), &cli, SignalDepth::Ten).unwrap();
        assert_eq!(config.pix_fmt, "yuv420p");
        assert_eq!(config.container, Container::Mp4);
        assert_eq!(config.codec_profile.as_deref(), Some("high"));
        assert_eq!(config.rate, Some(RateControl::Crf(18)));
        assert_eq!(config.options, vec![("-preset".into(), "medium".into())]);
    }

    #[test]
    fn rejects_unknown_or_conflicting_configuration() {
        for (graph, cli, marker) in [
            (
                Patch::default(),
                Patch {
                    encoder: Some("h264_videotoolbox".into()),
                    ..Default::default()
                },
                "unsupported encoder",
            ),
            (
                Patch::default(),
                Patch {
                    pix_fmt: Some("yuva444p10le".into()),
                    ..Default::default()
                },
                "alpha",
            ),
            (
                Patch::default(),
                Patch {
                    container: Some(Container::Mp4),
                    ..Default::default()
                },
                "MP4",
            ),
            (
                Patch::default(),
                Patch {
                    rate: Some(RateControl::Crf(20)),
                    ..Default::default()
                },
                "ProRes does not accept",
            ),
            (
                Patch::default(),
                Patch {
                    pix_fmt: Some("yuv422p10le".into()),
                    ..Default::default()
                },
                "requires a 4:4:4",
            ),
            (
                Patch {
                    encoder: Some("ffv1".into()),
                    rate: Some(RateControl::Bitrate(1)),
                    ..Default::default()
                },
                Patch::default(),
                "lossless",
            ),
        ] {
            let error = ResolvedConfig::resolve(Some(&graph), &cli, SignalDepth::Ten).unwrap_err();
            assert!(error.contains(marker), "{error}");
        }
    }

    #[test]
    fn output_options_are_a_bounded_allowlist_of_pairs() {
        assert_eq!(
            parse_output_options(&["-preset".into(), "medium".into(), "-g=12".into()]).unwrap(),
            vec![
                ("-preset".into(), "medium".into()),
                ("-g".into(), "12".into())
            ]
        );
        for tokens in [
            vec!["-i".to_string(), "in".to_string()],
            vec!["-vf".to_string(), "scale=2:2".to_string()],
            vec!["-c:v".to_string(), "libx264".to_string()],
            vec!["-pix_fmt".to_string(), "yuv420p".to_string()],
            vec!["output.mov".to_string()],
            vec!["-preset".to_string()],
            vec!["-preset".to_string(), "-tune".to_string()],
            vec!["-preset".to_string(), "a".to_string(), "extra".to_string()],
            vec!["-preset".to_string(), "a".to_string(), "-preset".to_string(), "b".to_string()],
            vec!["-map".to_string(), "0".to_string()],
        ] {
            assert!(parse_output_options(&tokens).is_err(), "{tokens:?}");
        }
    }

    #[test]
    fn graph_encoder_object_rejects_unknown_and_duplicate_intent() {
        let parse = |text: &str| {
            let value = crate::json::parse(text).unwrap();
            Patch::from_json(&value)
        };
        let patch = parse(
            r#"{"encoder":"libx264","pix_fmt":"yuv420p","container":"mp4","profile":"high","crf":22,"options":["-preset","medium"]}"#,
        )
        .unwrap();
        assert_eq!(patch.encoder.as_deref(), Some("libx264"));
        assert_eq!(patch.rate, Some(RateControl::Crf(22)));
        assert_eq!(patch.options.unwrap()[0].1, "medium");
        for bad in [
            r#"{"encoder":"prores_ks","scale":2}"#,
            r#"{"crf":18,"bitrate":1000000}"#,
            r#"{"container":"avi"}"#,
            r#"{"crf":64}"#,
            r#"{"options":["-i","in"]}"#,
            r#"{"encoder":"prores_ks","encoder":"ffv1"}"#,
            r#"{"crf":"high"}"#,
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn h264_decoder_formats_follow_the_configured_pixel_format() {
        let cli = Patch {
            encoder: Some("libx264".into()),
            pix_fmt: Some("yuv444p10le".into()),
            profile: Some("high444".into()),
            ..Default::default()
        };
        let config = ResolvedConfig::resolve(None, &cli, SignalDepth::Ten).unwrap();
        assert_eq!(config.decoder_pix_fmts(PreviewRange::Tv), vec!["yuv444p10le"]);

        let cli = Patch {
            encoder: Some("libx264".into()),
            pix_fmt: Some("yuv420p".into()),
            ..Default::default()
        };
        let config = ResolvedConfig::resolve(None, &cli, SignalDepth::Ten).unwrap();
        assert_eq!(config.decoder_pix_fmts(PreviewRange::Tv), vec!["yuv420p"]);
        assert_eq!(
            config.decoder_pix_fmts(PreviewRange::Pc),
            vec!["yuv420p", "yuvj420p"]
        );

        let cli = Patch {
            encoder: Some("libx264".into()),
            pix_fmt: Some("yuv422p10le".into()),
            profile: Some("high422".into()),
            ..Default::default()
        };
        let config = ResolvedConfig::resolve(None, &cli, SignalDepth::Ten).unwrap();
        assert_eq!(config.decoder_pix_fmts(PreviewRange::Pc), vec!["yuv422p10le"]);
    }

    #[test]
    fn naming_the_effective_encoder_keeps_graph_scoped_fields() {
        let graph = Patch {
            profile: Some("5".into()),
            ..Default::default()
        };
        let cli = Patch {
            encoder: Some("prores_ks".into()),
            ..Default::default()
        };
        let config = ResolvedConfig::resolve(Some(&graph), &cli, SignalDepth::Ten).unwrap();
        assert_eq!(config.encoder, "prores_ks");
        assert_eq!(config.codec_profile.as_deref(), Some("5"));

        let graph = Patch {
            profile: Some("hq".into()),
            pix_fmt: Some("yuv422p10le".into()),
            ..Default::default()
        };
        for cli in [
            Patch {
                encoder: Some("prores_ks".into()),
                ..Default::default()
            },
            Patch::default(),
        ] {
            let config = ResolvedConfig::resolve(Some(&graph), &cli, SignalDepth::Ten).unwrap();
            assert_eq!(config.codec_profile.as_deref(), Some("hq"));
            assert_eq!(config.pix_fmt, "yuv422p10le");
        }

        // A real codec change still drops the inherited encoder-scoped fields.
        let cli = Patch {
            encoder: Some("ffv1".into()),
            ..Default::default()
        };
        let config = ResolvedConfig::resolve(Some(&graph), &cli, SignalDepth::Ten).unwrap();
        assert_eq!(config.codec_profile, None);
        assert_eq!(config.encoder, "ffv1");
    }

    #[test]
    fn fixed_quantizer_uses_libx264s_constant_qp_control() {
        assert_eq!(
            RateControl::QScale(20).args(),
            vec!["-qp".to_string(), "20".to_string()]
        );
    }

    #[test]
    fn geometry_validation_is_per_pixel_format_and_never_rounds() {
        let prores = resolve(&Patch::default(), &Patch::default());
        assert!(prores.validate_geometry(9, 7).is_ok());
        assert!(prores.validate_geometry(0, 7).is_err());

        let h264 = Patch {
            encoder: Some("libx264".into()),
            ..Default::default()
        };
        let h264 = resolve(&h264, &Patch::default());
        assert!(h264.validate_geometry(8, 6).is_ok());
        let error = h264.validate_geometry(9, 6).unwrap_err();
        assert!(error.contains("even") && error.contains("9x6"), "{error}");
        assert!(h264.validate_geometry(8, 5).is_err());

        let ffv1 = Patch {
            encoder: Some("ffv1".into()),
            ..Default::default()
        };
        let ffv1 = resolve(&ffv1, &Patch::default());
        assert!(ffv1.validate_geometry(9, 7).is_ok());
    }

    #[test]
    fn encoder_arguments_fix_input_geometry_colour_and_one_output() {
        let config = resolve(&Patch::default(), &Patch::default());
        let profile = lookup("clog3").unwrap();
        let args = config.encoder_args(profile, 6984, 4660, 3, "30/1", SignalDepth::Ten, Path::new("out.mov"));
        assert_eq!(args.iter().filter(|arg| *arg == "-i").count(), 1);
        assert_eq!(args.iter().filter(|arg| *arg == "pipe:0").count(), 1);
        assert!(args.contains(&"rawvideo".into()));
        assert!(args.contains(&"yuv444p10le".into()));
        assert!(args.contains(&"6984x4660".into()));
        assert!(args.contains(&"prores_ks".into()));
        assert!(args.contains(&"-profile:v".into()));
        assert!(args.contains(&"4".into()));
        assert_eq!(
            args.iter()
                .position(|arg| arg == "-pix_fmt")
                .map(|index| args[index + 1].as_str()),
            Some("yuv444p10le")
        );
        for forbidden in ["-r", "-s", "-filter:v", "-logfile", "-filter_complex"] {
            assert!(!args.contains(&forbidden.to_string()), "{forbidden}");
        }
        // One output: the last argument, preceded by the configured muxer.
        assert_eq!(args.last().map(String::as_str), Some("out.mov"));
        assert_eq!(args[args.len() - 2], "mov");
        let filter = config.filter_chain(profile, SignalDepth::Ten);
        assert!(filter.starts_with("setparams="), "{filter}");
        assert!(filter.contains("colorspace=bt2020nc"));
        assert!(filter.contains("range=tv"));
        // Colour tags are accurate: Cinema Gamut/CLog3 are not labelled sRGB.
        let tags = colour_args(profile, config.effective_range(profile));
        assert!(tags.contains(&"unknown".into()));
        assert!(!tags.contains(&"bt709".into()));
    }

    #[test]
    fn lower_bit_depth_codecs_subsample_from_the_same_handoff() {
        let config = ResolvedConfig::resolve(
            None,
            &Patch {
                encoder: Some("libx264".into()),
                ..Default::default()
            },
            SignalDepth::Ten,
        )
        .unwrap();
        let profile = lookup("sony-slog3-sgamut3").unwrap();
        let filter = config.filter_chain(profile, SignalDepth::Ten);
        assert!(filter.starts_with("scale=in_range=pc:in_color_matrix=bt2020:out_range=pc:out_color_matrix=bt2020,"));
        assert!(filter.contains("range=pc"));
        let args = config.encoder_args(profile, 16, 12, 1, "30/1", SignalDepth::Ten, Path::new("out.mp4"));
        assert!(args.contains(&"yuv420p".into()));
        assert!(args.contains(&"-crf".into()));
        assert_eq!(config.effective_range(profile), PreviewRange::Pc);
        assert!(config.writes_range_tag());
    }

    #[test]
    fn preflight_uses_a_synthetic_input_and_removes_no_pipeline_argument() {
        let config = resolve(&Patch::default(), &Patch::default());
        let profile = lookup("srgb-preview").unwrap();
        let args = config.preflight_args(profile, (64, 48), "24/1", Path::new("preflight.mov"));
        assert!(args.contains(&"lavfi".into()));
        assert!(args.contains(&"color=c=black:s=64x48:r=24/1".into()));
        assert!(args.contains(&"prores_ks".into()));
        assert_eq!(args.last().map(String::as_str), Some("preflight.mov"));
    }
}
