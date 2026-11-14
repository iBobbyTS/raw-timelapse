//! Streaming encoder node, generic output validation and publication.
//!
//! The graph hands this module owned 10-bit 4:4:4 target-signal planes from the
//! colour node. One FFmpeg process receives them as `yuv444p10le`; codec
//! configuration, format adaptation and the single quantization boundary are
//! owned by [`crate::encoder`] and [`crate::color_profile`]. There is no
//! second, 8-bit develop path.

use crate::cli::Fps;
use crate::color_profile::ColorProfile;
use crate::encoder::ResolvedConfig;
use crate::error::{Error, Result, EXIT_PREVIEW, EXIT_PUBLISH, EXIT_VALIDATION};
use crate::json::Json;
use crate::proc;
#[cfg(test)]
use crate::transport::RGB48_FRAME;
use crate::transport::{
    self, DataEnvelope, DiskArtifact, MetadataDoc, Payload, Provenance, VIDEO_ARTIFACT,
};
#[cfg(test)]
use crate::transport::{ArrayDescriptor, ArrayLayout, ByteOrder, DType};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Native stdout supplies scene-linear ACEScg codes.
#[cfg(test)]
const SCENE_LINEAR_SPACE: &str = "ACEScg AP1/D60 scene-linear";
#[cfg(test)]
const SCENE_LINEAR_TRANSFER: &str = "linear";

/// Explicit resize actually applied by the graph, recorded in the run log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeRecord {
    pub source_width: u32,
    pub source_height: u32,
    pub width: u32,
    pub height: u32,
}

impl ResizeRecord {
    pub fn applied(&self) -> bool {
        self.source_width != self.width || self.source_height != self.height
    }
}

/// One tightly packed HWC frame descriptor. Shape coordinates are always
/// `[height, width, channels]`; the declared byte length is the backing length,
/// so a decoder buffer that does not match the declared geometry is rejected at
/// construction time instead of reaching the pixel converter.
#[cfg(test)]
fn rgb_frame_descriptor(
    dtype: DType,
    width: usize,
    height: usize,
    byte_length: usize,
) -> ArrayDescriptor {
    let sample = dtype.byte_width();
    ArrayDescriptor {
        dtype,
        shape: vec![height, width, 3],
        byte_strides: vec![width * 3 * sample, 3 * sample, sample],
        offset: 0,
        byte_length,
        layout: ArrayLayout::HwcInterleaved,
        byte_order: ByteOrder::LittleEndian,
        read_only: true,
    }
}

#[cfg(test)]
fn rgb_metadata(
    width: u32,
    height: u32,
    space: &str,
    transfer: &str,
) -> transport::Result<MetadataDoc> {
    MetadataDoc::new(vec![
        ("source.width".to_string(), Json::Int(width as i64)),
        ("source.height".to_string(), Json::Int(height as i64)),
        ("color.space".to_string(), Json::Str(space.to_string())),
        (
            "color.transfer".to_string(),
            Json::Str(transfer.to_string()),
        ),
    ])
}

/// Wrap one decoded little-endian RGB16 frame as a validated `RGB48_FRAME`.
#[cfg(test)]
fn rgb48_frame_envelope(
    bytes: Arc<[u8]>,
    width: u32,
    height: u32,
    producer: &str,
) -> transport::Result<DataEnvelope> {
    let byte_length = bytes.len();
    let array = transport::NdArray::new(
        bytes,
        rgb_frame_descriptor(DType::U16, width as usize, height as usize, byte_length),
    )?;
    let metadata = rgb_metadata(width, height, SCENE_LINEAR_SPACE, SCENE_LINEAR_TRANSFER)?;
    let provenance = Provenance::new(producer, Vec::new(), Vec::new())?;
    DataEnvelope::new(RGB48_FRAME, Payload::NdArray(array), metadata, provenance)
}

/// Build the typed handoff for an encoded deliverable. `audit` consumes the
/// same envelope, and `publish` renames the referenced file.
#[cfg(test)]
pub fn video_envelope(
    path: &Path,
    media_type: &str,
    codec: &str,
    frame_count: u64,
    producer: &str,
    profile: &ColorProfile,
) -> transport::Result<DataEnvelope> {
    video_envelope_with_gain(path, media_type, codec, frame_count, producer, profile, 1.0)
}

pub fn video_envelope_with_gain(
    path: &Path,
    media_type: &str,
    codec: &str,
    frame_count: u64,
    producer: &str,
    profile: &ColorProfile,
    reference_gain: f64,
) -> transport::Result<DataEnvelope> {
    let artifact = DiskArtifact::new(path, media_type, file_size(path), None)?;
    let metadata = MetadataDoc::new(vec![
        ("color.profile".into(), Json::Str(profile.id.into())),
        (
            "color.display_name".into(),
            Json::Str(profile.display_name().into()),
        ),
        ("color.reference_gain".into(), Json::Float(reference_gain)),
        (
            "color.reference_status".into(),
            Json::Str("unverified".into()),
        ),
        (
            "color.transfer".into(),
            Json::Str(profile.transfer.name().into()),
        ),
        ("color.space".into(), Json::Str(profile.gamut.name.into())),
        ("audit.codec".to_string(), Json::Str(codec.to_string())),
        (
            "source.frame_count".to_string(),
            Json::Int(frame_count as i64),
        ),
    ])?;
    let provenance = Provenance::new(producer, Vec::new(), Vec::new())?;
    DataEnvelope::new(
        VIDEO_ARTIFACT,
        Payload::DiskArtifact(artifact),
        metadata,
        provenance,
    )
}

/// Resolve the file behind a validated video handoff. `None` means the caller
/// did not receive a `VIDEO_ARTIFACT` disk reference.
pub fn video_artifact_path(envelope: &DataEnvelope) -> Option<&Path> {
    match (envelope.semantic_type(), envelope.payload()) {
        (VIDEO_ARTIFACT, Payload::DiskArtifact(artifact)) => Some(artifact.path()),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct ProbeInfo {
    pub codec_name: String,
    pub profile: String,
    pub pix_fmt: String,
    pub bits_per_raw_sample: Option<u32>,
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub avg_frame_rate: String,
    pub color_space: String,
    pub color_primaries: String,
    pub color_transfer: String,
    pub color_range: String,
}

impl ProbeInfo {
    pub fn to_json(&self) -> crate::json::Json {
        crate::json::Json::obj(vec![
            (
                "codec_name",
                crate::json::Json::Str(self.codec_name.clone()),
            ),
            ("profile", crate::json::Json::Str(self.profile.clone())),
            ("pix_fmt", crate::json::Json::Str(self.pix_fmt.clone())),
            (
                "bits_per_raw_sample",
                self.bits_per_raw_sample
                    .map(|bits| crate::json::Json::Int(bits as i64))
                    .unwrap_or(crate::json::Json::Null),
            ),
            ("width", crate::json::Json::Int(self.width as i64)),
            ("height", crate::json::Json::Int(self.height as i64)),
            ("nb_read_frames", crate::json::Json::Int(self.frames as i64)),
            (
                "avg_frame_rate",
                crate::json::Json::Str(self.avg_frame_rate.clone()),
            ),
            (
                "color_space",
                crate::json::Json::Str(self.color_space.clone()),
            ),
            (
                "color_primaries",
                crate::json::Json::Str(self.color_primaries.clone()),
            ),
            (
                "color_transfer",
                crate::json::Json::Str(self.color_transfer.clone()),
            ),
            (
                "color_range",
                crate::json::Json::Str(self.color_range.clone()),
            ),
        ])
    }
}

pub fn probe_stream(ffprobe: &Path, file: &Path) -> Result<ProbeInfo> {
    let args = vec![
        "-v".to_string(),
        "error".to_string(),
        "-select_streams".to_string(),
        "v:0".to_string(),
        "-count_frames".to_string(),
        "-show_entries".to_string(),
        "stream=codec_name,profile,pix_fmt,bits_per_raw_sample,width,height,nb_read_frames,avg_frame_rate,color_space,color_primaries,color_transfer,color_range".to_string(),
        "-of".to_string(),
        "default=noprint_wrappers=1".to_string(),
        file.display().to_string(),
    ];
    let captured = proc::run_capture(ffprobe, &args, EXIT_VALIDATION)?;
    if !captured.status.success() {
        return Err(Error::new(
            EXIT_VALIDATION,
            format!(
                "ffprobe failed for {}: {}; stderr: {}",
                file.display(),
                proc::exit_description(&captured.status),
                proc::tail(&captured.stderr, 1200)
            ),
        ));
    }
    let mut fields = std::collections::BTreeMap::new();
    for line in captured.stdout.lines() {
        if let Some((key, value)) = line.split_once('=') {
            fields.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    let required = |key: &str| -> Result<String> {
        fields.get(key).cloned().ok_or_else(|| {
            Error::new(
                EXIT_VALIDATION,
                format!(
                    "ffprobe output for {} has no {key} field; output: {}",
                    file.display(),
                    proc::tail(&captured.stdout, 600)
                ),
            )
        })
    };
    let width: u32 = required("width")?.parse().map_err(|_| {
        Error::new(
            EXIT_VALIDATION,
            format!(
                "ffprobe returned a non-integer width for {}",
                file.display()
            ),
        )
    })?;
    let height: u32 = required("height")?.parse().map_err(|_| {
        Error::new(
            EXIT_VALIDATION,
            format!(
                "ffprobe returned a non-integer height for {}",
                file.display()
            ),
        )
    })?;
    let frames_text = fields
        .get("nb_read_frames")
        .or_else(|| fields.get("nb_frames"))
        .cloned()
        .ok_or_else(|| {
            Error::new(
                EXIT_VALIDATION,
                format!(
                    "ffprobe output for {} has no frame count field; output: {}",
                    file.display(),
                    proc::tail(&captured.stdout, 600)
                ),
            )
        })?;
    let frames: u64 = frames_text.parse().map_err(|_| {
        Error::new(
            EXIT_VALIDATION,
            format!(
                "ffprobe returned a non-integer frame count {frames_text:?} for {}",
                file.display()
            ),
        )
    })?;
    let color = |key: &str| {
        fields
            .get(key)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string())
    };
    let bits_per_raw_sample = fields
        .get("bits_per_raw_sample")
        .and_then(|text| text.parse::<u32>().ok())
        .filter(|bits| *bits > 0);
    Ok(ProbeInfo {
        codec_name: required("codec_name")?,
        profile: color("profile"),
        pix_fmt: required("pix_fmt")?,
        bits_per_raw_sample,
        width,
        height,
        frames,
        avg_frame_rate: required("avg_frame_rate")?,
        color_space: color("color_space"),
        color_primaries: color("color_primaries"),
        color_transfer: color("color_transfer"),
        color_range: color("color_range"),
    })
}

/// Everything the generic output check needs to know about the configured
/// deliverable. No H.264-specific syntax is inspected.
pub struct OutputContract<'a> {
    pub config: &'a ResolvedConfig,
    pub profile: &'a ColorProfile,
    pub width: u32,
    pub height: u32,
    pub frames: usize,
    pub fps: Fps,
}

/// Validate the encoded file generically: codec/profile/pixel format/geometry/
/// frame count/rate, the profile's per-field colour tags, the range the
/// container can express, and a complete decode.
pub fn validate_output(
    ffprobe: &Path,
    ffmpeg: &Path,
    file: &Path,
    contract: &OutputContract<'_>,
) -> Result<ProbeInfo> {
    let config = contract.config;
    let profile = contract.profile;
    let range = config.effective_range(profile);
    if let Err(message) = config.validate_geometry(contract.width, contract.height) {
        return Err(Error::new(EXIT_PREVIEW, message));
    }
    let probe = probe_stream(ffprobe, file)?;
    if probe.codec_name != config.codec_name() {
        return Err(Error::new(
            EXIT_PREVIEW,
            format!(
                "output codec is {:?}, expected {:?} ({})",
                probe.codec_name,
                config.codec_name(),
                config.encoder
            ),
        ));
    }
    let accepted = config.decoder_pix_fmts(range);
    if !accepted.contains(&probe.pix_fmt.as_str()) {
        return Err(Error::new(
            EXIT_PREVIEW,
            format!(
                "output pixel format is {:?}, expected one of {:?} for {}",
                probe.pix_fmt, accepted, config.encoder
            ),
        ));
    }
    if config.encoder == "prores_ks" {
        // ffprobe reports ProRes profiles by name; the configured value may be
        // the numeric profile or the name.
        let expected = config.probe_profile().ok_or_else(|| {
            Error::new(
                EXIT_PREVIEW,
                format!("invalid ProRes profile {:?}", config.codec_profile),
            )
        })?;
        if !probe.profile.eq_ignore_ascii_case(expected) {
            return Err(Error::new(
                EXIT_PREVIEW,
                format!(
                    "output codec profile is {:?}, expected {expected:?}",
                    probe.profile
                ),
            ));
        }
    }
    if probe.width != contract.width || probe.height != contract.height {
        return Err(Error::new(
            EXIT_PREVIEW,
            format!(
                "output is {}x{}, expected {}x{}",
                probe.width, probe.height, contract.width, contract.height
            ),
        ));
    }
    if probe.frames != contract.frames as u64 {
        return Err(Error::new(
            EXIT_PREVIEW,
            format!(
                "output has {} frames, expected {}",
                probe.frames, contract.frames
            ),
        ));
    }
    if !rate_matches(&probe.avg_frame_rate, contract.fps) {
        return Err(Error::new(
            EXIT_PREVIEW,
            format!(
                "output frame rate is {:?}, expected {}/{}",
                probe.avg_frame_rate, contract.fps.num, contract.fps.den
            ),
        ));
    }

    // Validate every accurate field; unknown Log transfer/primaries stay unknown.
    for (field, expected, observed) in [
        (
            "color_space",
            profile.tag_policy.matrix_token(),
            &probe.color_space,
        ),
        (
            "color_primaries",
            profile.tag_policy.primaries_token(),
            &probe.color_primaries,
        ),
        (
            "color_transfer",
            profile.tag_policy.transfer_token(),
            &probe.color_transfer,
        ),
    ] {
        let matches = observed == expected
            || (expected == "unknown"
                && matches!(
                    observed.as_str(),
                    "" | "unknown" | "unspecified" | "reserved"
                ));
        if !matches {
            return Err(Error::new(EXIT_PREVIEW, format!(
                "output {field} is {observed:?}, expected {expected:?} for {} (per-field colour contract)", profile.id
            )));
        }
    }
    let expected_range = range.as_str();
    let range_matches = probe.color_range == expected_range
        || (!config.writes_range_tag()
            && matches!(probe.color_range.as_str(), "" | "unknown" | "unspecified"));
    if !range_matches {
        return Err(Error::new(
            EXIT_PREVIEW,
            format!(
                "output color_range is {:?}, expected {expected_range:?} for {} ({})",
                probe.color_range, profile.id, config.encoder
            ),
        ));
    }

    // Full decode proves the file is actually playable, not merely indexable.
    let args = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-v".to_string(),
        "error".to_string(),
        "-i".to_string(),
        file.display().to_string(),
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ];
    let captured = proc::run_capture(ffmpeg, &args, EXIT_PREVIEW)?;
    if !captured.status.success() {
        return Err(Error::new(
            EXIT_PREVIEW,
            format!(
                "output failed a full decode: {}; stderr: {}",
                proc::exit_description(&captured.status),
                proc::tail(&captured.stderr, 1200)
            ),
        ));
    }
    Ok(probe)
}

/// Ask FFmpeg which pixel formats an encoder declares. This is the encoder's
/// own capability list (`AVCodec.pix_fmts`), not a decoder bit-depth label, so
/// it can establish exact input-format support before any RAW work.
fn supported_encoder_pixel_formats(ffmpeg: &Path, encoder: &str) -> Result<Option<Vec<String>>> {
    let args = vec![
        "-hide_banner".to_string(),
        "-h".to_string(),
        format!("encoder={encoder}"),
    ];
    let captured = proc::run_capture(ffmpeg, &args, EXIT_USAGE_FOR_PREFLIGHT)?;
    let text = format!("{}\n{}", captured.stdout, captured.stderr);
    let formats = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("Supported pixel formats:"))
        .map(|rest| {
            rest.split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        });
    if formats.is_none() && !captured.status.success() {
        return Err(Error::new(
            EXIT_USAGE_FOR_PREFLIGHT,
            format!(
                "cannot query encoder {encoder:?}: {}; output: {}",
                proc::exit_description(&captured.status),
                proc::tail(&text, 600)
            ),
        ));
    }
    Ok(formats)
}

/// FFmpeg's own substitution diagnostics. These are treated as configuration
/// errors: the requested pixel format must be the encoded one.
fn substituted_pixel_format(stderr: &str) -> bool {
    let text = stderr.to_ascii_lowercase();
    [
        "auto-selecting format",
        "incompatible pixel format",
        "incompatible pix_fmt",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

/// Early capability check before any RAW preparation. The encoder's declared
/// pixel formats, profile, rate control, options and container are checked, then
/// exercised by a one-frame synthetic encode into the partial directory; the
/// file is removed immediately. An unsupported pixel format (or FFmpeg
/// substitution) is fatal here, before any RAW processing.
pub fn preflight(
    config: &ResolvedConfig,
    profile: &ColorProfile,
    ffmpeg: &Path,
    explicit_geometry: Option<(u32, u32)>,
    fps: Fps,
    scratch: &Path,
) -> Result<()> {
    if let Some((width, height)) = explicit_geometry {
        if let Err(message) = config.validate_geometry(width, height) {
            return Err(Error::new(EXIT_USAGE_FOR_PREFLIGHT, message));
        }
    }
    if let Some(formats) = supported_encoder_pixel_formats(ffmpeg, &config.encoder)? {
        if !formats.iter().any(|format| format == &config.pix_fmt) {
            return Err(Error::new(
                EXIT_USAGE_FOR_PREFLIGHT,
                format!(
                    "encoder {} does not support pixel format {:?}; supported: {}",
                    config.encoder,
                    config.pix_fmt,
                    formats.join(", ")
                ),
            ));
        }
    }
    let geometry = explicit_geometry.unwrap_or((32, 32));
    let output = scratch.join(format!("preflight.{}", config.container.extension()));
    let _ = fs::remove_file(&output);
    let args = config.preflight_args(profile, geometry, &fps.arg(), &output);
    let captured = proc::run_capture(ffmpeg, &args, EXIT_PREVIEW)?;
    let _ = fs::remove_file(&output);
    if !captured.status.success() {
        return Err(Error::new(
            EXIT_PREVIEW,
            format!(
                "encoder preflight failed for {}: {}; stderr: {}",
                config.summary(profile),
                proc::exit_description(&captured.status),
                proc::tail(&captured.stderr, 1200)
            ),
        ));
    }
    if substituted_pixel_format(&captured.stderr) {
        return Err(Error::new(
            EXIT_USAGE_FOR_PREFLIGHT,
            format!(
                "encoder preflight substituted the requested pixel format {:?} for {}; stderr: {}",
                config.pix_fmt,
                config.encoder,
                proc::tail(&captured.stderr, 1200)
            ),
        ));
    }
    Ok(())
}

/// Configuration/validation family for run-boundary encoder rejections.
const EXIT_USAGE_FOR_PREFLIGHT: i32 = crate::error::EXIT_VALIDATION;

struct StreamingEncoder {
    process: proc::OwnedProcess,
    input: Option<std::process::ChildStdin>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    final_input: std::sync::Arc<std::sync::atomic::AtomicBool>,
    watcher: Option<std::thread::JoinHandle<()>>,
    bytes: usize,
}
impl StreamingEncoder {
    fn spawn(
        program: &Path,
        args: &[String],
        log: &Path,
        active: proc::ActiveNative,
    ) -> Result<Self> {
        use std::sync::atomic::{AtomicBool, Ordering};
        let process = proc::OwnedProcess::spawn_active(
            program,
            args,
            log,
            EXIT_PREVIEW,
            true,
            active.clone(),
        )?;
        let input = process.child.lock().unwrap().stdin.take();
        let child = process.child.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let final_input = Arc::new(AtomicBool::new(false));
        let final_input_watch = final_input.clone();
        let (stop_watch, failed_watch) = (stop.clone(), failed.clone());
        let watcher = std::thread::spawn(move || {
            while !stop_watch.load(Ordering::SeqCst) {
                let exited = child.lock().unwrap().try_wait();
                if let Ok(Some(status)) = exited {
                    if !stop_watch.load(Ordering::SeqCst)
                        && (!status.success() || !final_input_watch.load(Ordering::SeqCst))
                    {
                        failed_watch.store(true, Ordering::SeqCst);
                        proc::cancel(&active);
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        Ok(Self {
            process,
            input,
            stop,
            failed,
            final_input,
            watcher: Some(watcher),
            bytes: 0,
        })
    }
    fn finish(&mut self, abort: bool) -> Result<std::process::ExitStatus> {
        // Stop observing before intentional EOF/cancellation. The observer
        // distinguishes premature exit from successful final input completion.
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(w) = self.watcher.take() {
            let _ = w.join();
        }
        self.input.take();
        let status = self.process.finish(abort);
        self.process
            .note(&format!("pixel bytes (yuv444p10le): {}", self.bytes));
        status
    }
}
impl Drop for StreamingEncoder {
    fn drop(&mut self) {
        if self.watcher.is_some() {
            let _ = self.finish(true);
        }
    }
}

/// Publish the sole validated deliverable with one rename, refusing overwrite.
pub fn publish(video: &DataEnvelope, destination: &Path) -> Result<()> {
    let source = video_artifact_path(video)
        .ok_or_else(|| Error::new(EXIT_PUBLISH, "invalid video artifact handoff"))?;
    match fs::symlink_metadata(destination) {
        Ok(_) => {
            return Err(Error::new(
                EXIT_PUBLISH,
                format!(
                    "refusing to overwrite existing deliverable {}",
                    destination.display()
                ),
            ));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(Error::new(
                EXIT_PUBLISH,
                format!(
                    "cannot inspect publication destination {}: {err}",
                    destination.display()
                ),
            ));
        }
    }
    fs::rename(source, destination).map_err(|e| {
        Error::new(
            EXIT_PUBLISH,
            format!(
                "cannot publish {} -> {}: {e}",
                source.display(),
                destination.display()
            ),
        )
    })
}

fn ensure_nonempty(path: &Path, code: i32, context: &str) -> Result<()> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 => Ok(()),
        Ok(metadata) if metadata.is_file() => Err(Error::new(
            code,
            format!("{context}: {} is empty", path.display()),
        )),
        Ok(_) => Err(Error::new(
            code,
            format!("{context}: {} is not a regular file", path.display()),
        )),
        Err(err) => Err(Error::new(
            code,
            format!("{context}: {} is missing: {err}", path.display()),
        )),
    }
}

fn rate_matches(observed: &str, fps: Fps) -> bool {
    let parsed = match observed.split_once('/') {
        Some((num, den)) => match (num.trim().parse::<f64>(), den.trim().parse::<f64>()) {
            (Ok(num), Ok(den)) if den > 0.0 => Some(num / den),
            _ => None,
        },
        None => observed.trim().parse::<f64>().ok(),
    };
    match parsed {
        Some(value) if value.is_finite() => {
            let expected = fps.as_f64();
            (value - expected).abs() <= 1e-6 * expected.max(1.0)
        }
        _ => false,
    }
}

pub fn file_size(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|metadata| metadata.len())
}

/// Little-endian byte view of the 10-bit samples. The native architecture is
/// little-endian, so the frame is written without a full-frame copy; other
/// targets fall back to an explicit conversion.
fn signal_bytes(signal: &[u16]) -> std::borrow::Cow<'_, [u8]> {
    #[cfg(target_endian = "little")]
    {
        // SAFETY: every sample is a plain integer, the view covers exactly the
        // same initialized memory, and `u8` has no stronger alignment.
        std::borrow::Cow::Borrowed(unsafe {
            std::slice::from_raw_parts(signal.as_ptr().cast::<u8>(), signal.len() * 2)
        })
    }
    #[cfg(not(target_endian = "little"))]
    {
        std::borrow::Cow::Owned(
            signal
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect(),
        )
    }
}

/// The product encoder node. It accepts owned 10-bit 4:4:4 planes from the
/// colour node and never re-enters source decoding.
pub struct GraphEncoder {
    config: ResolvedConfig,
    frames: usize,
    output_name: String,
    encoder: Option<StreamingEncoder>,
    output: Option<std::path::PathBuf>,
    transcript: Option<std::path::PathBuf>,
    command: Option<String>,
    pub active: proc::ActiveNative,
}
impl GraphEncoder {
    pub fn new(config: ResolvedConfig, frames: usize, output_name: &str) -> Self {
        Self {
            config,
            frames,
            output_name: output_name.to_string(),
            encoder: None,
            output: None,
            transcript: None,
            command: None,
            active: Default::default(),
        }
    }

    pub fn begin(&mut self) -> Result<()> {
        proc::check_stop()
    }

    pub fn push(
        &mut self,
        options: &crate::cli::Options,
        scratch: &Path,
        info: &crate::frame::FrameInfo,
        signal: &[u16],
    ) -> Result<()> {
        proc::check_stop()?;
        let expected = info.width as usize * info.height as usize * 3;
        if signal.len() != expected {
            return Err(Error::new(
                EXIT_PREVIEW,
                format!(
                    "encoder input has {} samples, expected {expected} for {}x{} 4:4:4 10-bit",
                    signal.len(),
                    info.width,
                    info.height
                ),
            ));
        }
        if self.encoder.is_none() {
            if let Err(message) = self.config.validate_geometry(info.width, info.height) {
                return Err(Error::new(EXIT_USAGE_FOR_PREFLIGHT, message));
            }
            let output = scratch.join(&self.output_name);
            let args = self.config.encoder_args(
                options.preview_profile,
                info.width,
                info.height,
                self.frames,
                &options.fps.arg(),
                &output,
            );
            let transcript = scratch.join("transcripts/encode.log");
            self.command = Some(proc::display_command(&options.ffmpeg, &args));
            self.output = Some(output);
            self.transcript = Some(transcript.clone());
            self.encoder = Some(StreamingEncoder::spawn(
                &options.ffmpeg,
                &args,
                &transcript,
                self.active.clone(),
            )?);
        }
        let encoder = self.encoder.as_mut().unwrap();
        if encoder.failed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(Error::new(
                EXIT_PREVIEW,
                "encoder failed while a graph image was active",
            ));
        }
        // `yuv444p10le` is 16-bit little-endian words with the code in the low
        // ten bits; the buffer is produced by the colour node.
        // FFmpeg's -frames:v may finish successfully before explicit end().
        // Mark the final transfer before writing; a premature close during the
        // write is still an error (broken pipe), and output validation remains
        // responsible for exact frame coverage after a successful final write.
        if info.index == self.frames {
            encoder
                .final_input
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let bytes = signal_bytes(signal);
        encoder
            .input
            .as_mut()
            .unwrap()
            .write_all(&bytes)
            .map_err(|e| Error::new(EXIT_PREVIEW, format!("encoder stdin failed: {e}")))?;
        encoder.bytes += bytes.len();
        Ok(())
    }

    pub fn end(&mut self) -> Result<()> {
        proc::check_stop()?;
        let encoder = self
            .encoder
            .as_mut()
            .ok_or_else(|| Error::new(EXIT_PREVIEW, "empty encoder input"))?;
        let failed = encoder.failed.load(std::sync::atomic::Ordering::SeqCst);
        let status = encoder.finish(false)?;
        if failed || !status.success() {
            return Err(Error::new(
                EXIT_PREVIEW,
                format!(
                    "streaming encoder failed: {}; transcript {}: {}",
                    proc::exit_description(&status),
                    self.transcript.as_ref().map_or_else(
                        || "unavailable".to_string(),
                        |path| path.display().to_string()
                    ),
                    self.transcript_tail()
                ),
            ));
        }
        ensure_nonempty(
            self.output
                .as_ref()
                .ok_or_else(|| Error::new(EXIT_PREVIEW, "empty encoder input"))?,
            EXIT_PREVIEW,
            "streaming encoder produced no video",
        )?;
        Ok(())
    }

    /// The argv of the spawned encoder, recorded once in the run log.
    pub fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }

    /// Bounded encoder transcript tail for the single run log when the encoder
    /// is the failing node.
    pub fn failure_detail(&self) -> String {
        self.transcript_tail()
    }

    /// Bounded tail of the FFmpeg transcript, so the single run log carries the
    /// necessary encoder error without a second log file.
    fn transcript_tail(&self) -> String {
        let Some(path) = &self.transcript else {
            return String::new();
        };
        match fs::read_to_string(path) {
            Ok(text) => proc::tail(&text, 1200),
            Err(err) => format!("unreadable: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_rate_comparison_accepts_rational_and_decimal() {
        assert!(rate_matches("30/1", Fps { num: 30, den: 1 }));
        assert!(rate_matches(
            "30000/1001",
            Fps {
                num: 30000,
                den: 1001
            }
        ));
        assert!(rate_matches("60/2", Fps { num: 30, den: 1 }));
        assert!(rate_matches("30", Fps { num: 30, den: 1 }));
        assert!(!rate_matches("24/1", Fps { num: 30, den: 1 }));
        assert!(!rate_matches("N/A", Fps { num: 30, den: 1 }));
    }

    #[test]
    fn rgb48_typed_frame_rejects_a_buffer_that_does_not_match_the_geometry() {
        let width = 8u32;
        let height = 6u32;
        let shortened: Arc<[u8]> = vec![0u8; (width * height * 6 - 2) as usize].into();
        match rgb48_frame_envelope(shortened, width, height, "test") {
            Err(transport::TransportError::ByteLengthMismatch { expected, actual }) => {
                assert_eq!(expected, (width * height * 6) as usize);
                assert_eq!(actual, expected - 2);
            }
            other => panic!("unexpected constructor outcome: {other:?}"),
        }
    }
}

#[cfg(all(test, unix))]
mod publication_tests {
    use super::*;
    #[test]
    fn single_video_publication_refuses_overwrite_and_retains_validated_source() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-tmp")
            .join(format!("publish-stream-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source = root.join("temporary.mov");
        let destination = root.join("output.mov");
        fs::write(&source, b"validated").unwrap();
        fs::write(&destination, b"existing").unwrap();
        let envelope = video_envelope(
            &source,
            "video/quicktime",
            "prores",
            1,
            "test",
            crate::color_profile::default_profile(),
        )
        .unwrap();
        let err = publish(&envelope, &destination).unwrap_err();
        assert_eq!(err.code, EXIT_PUBLISH);
        assert_eq!(fs::read(&destination).unwrap(), b"existing");
        assert_eq!(fs::read(&source).unwrap(), b"validated");
        fs::remove_file(&destination).unwrap();
        publish(&envelope, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"validated");
        assert!(!source.exists());
    }

    #[test]
    fn publication_refuses_a_dangling_destination_symlink() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-tmp")
            .join(format!("publish-dangling-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source = root.join("temporary.mov");
        let destination = root.join("output.mov");
        fs::write(&source, b"validated").unwrap();
        let link_target = std::path::PathBuf::from("missing-user-target.mov");
        std::os::unix::fs::symlink(&link_target, &destination).unwrap();
        let envelope = video_envelope(
            &source,
            "video/quicktime",
            "prores",
            1,
            "test",
            crate::color_profile::default_profile(),
        )
        .unwrap();
        let err = publish(&envelope, &destination).unwrap_err();
        assert_eq!(err.code, EXIT_PUBLISH);
        let metadata = fs::symlink_metadata(&destination).expect("link metadata");
        assert!(metadata.file_type().is_symlink(), "the link was replaced");
        assert_eq!(fs::read_link(&destination).unwrap(), link_target);
        assert_eq!(fs::read(&source).unwrap(), b"validated");
        let _ = fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod signal_byte_tests {
    use super::*;

    #[test]
    fn ten_bit_samples_use_little_endian_words() {
        let samples = [64u16, 512, 940, 1023];
        let bytes = signal_bytes(&samples);
        let expected: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        assert_eq!(&bytes[..], &expected[..]);
        assert_eq!(bytes.len(), samples.len() * 2);
    }
}

#[cfg(test)]
#[path = "encoder_cancels_producers_tests.rs"]
mod encoder_cancels_producers_tests;
