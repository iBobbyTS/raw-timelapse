//! Streaming encoder node, generic output validation and publication.
//!
//! The graph hands this module the target colour signal. The streaming node
//! packs it into the selected rawvideo layout and writes it to FFmpeg.

use crate::cli::Fps;
use crate::color_profile::ColorProfile;
use crate::encoder::ResolvedConfig;
use crate::error::{Error, Result, EXIT_PREVIEW, EXIT_PUBLISH};
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
    /// Number of frames decoded by full verification.
    pub frames: Option<u64>,
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
            (
                "nb_read_frames",
                self.frames
                    .map(|frames| crate::json::Json::Int(frames as i64))
                    .unwrap_or(crate::json::Json::Null),
            ),
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

fn inspect_output(ffmpeg: &Path, file: &Path, verify: OutputVerify) -> Result<ProbeInfo> {
    let full = verify == OutputVerify::Full;
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-v", "info", "-nostats", "-xerror",
        "-abort_on", "empty_output", "-progress", "pipe:1", "-i"]
        .into_iter().map(str::to_owned).collect();
    args.push(file.display().to_string());
    args.extend(["-map", "0:v:0"].into_iter().map(str::to_owned));
    if full {
        args.extend(["-fps_mode", "passthrough"].into_iter().map(str::to_owned));
    } else {
        args.extend(["-c:v", "copy", "-frames:v", "1"].into_iter().map(str::to_owned));
    }
    args.extend(["-f", "null", "-"].into_iter().map(str::to_owned));
    let captured = proc::run_capture(ffmpeg, &args, EXIT_PREVIEW)?;
    if !captured.status.success() {
        return Err(Error::new(EXIT_PREVIEW, format!(
            "output verification failed for {}: {}; stderr: {}",
            file.display(), proc::exit_description(&captured.status), proc::tail(&captured.stderr, 1200)
        )));
    }
    let mut probe = parse_video_description(&captured.stderr)
        .ok_or_else(|| Error::new(EXIT_PREVIEW, format!(
            "FFmpeg did not report a video stream for {}: {}",
            file.display(), proc::tail(&captured.stderr, 1200)
        )))?;
    if full {
        probe.frames = captured.stdout.lines()
            .filter_map(|line| line.strip_prefix("frame=")?.trim().parse().ok())
            .last();
    }
    Ok(probe)
}

/// Parse the first input video summary. Parenthesized annotations can contain
/// commas, including the pixel range, colour tags and field order.
fn parse_video_description(stderr: &str) -> Option<ProbeInfo> {
    let line = stderr.lines().map(str::trim)
        .take_while(|line| !line.starts_with("Stream mapping:") && !line.starts_with("Output #"))
        .filter(|line| line.starts_with("Stream #0:"))
        .find_map(|line| line.split_once(": Video: ").map(|(_, video)| video))?;
    let mut depth = 0;
    let fields: Vec<_> = line.split(|ch| match ch {
        '(' => { depth += 1; false }
        ')' => { depth -= 1; false }
        ',' => depth == 0,
        _ => false,
    }).map(str::trim).collect();

    let codec = *fields.first()?;
    let codec_name = codec.split_whitespace().next()?.into();
    let codec_profile = codec.split_once('(').and_then(|(_, rest)| rest.split_once(')'))
        .map(|(name, _)| name).filter(|name| !name.contains(" / ")).unwrap_or("unknown");
    let format = *fields.get(1)?;
    let pix_fmt = format.split(['(', ' ']).next()?.to_owned();
    let mut range = if pix_fmt.starts_with("yuvj") { "pc" } else { "unknown" };
    let mut tags = ["unknown"; 3];
    if let Some((_, options)) = format.split_once('(') {
        for option in options.trim_end_matches(')').split(',').map(str::trim) {
            if matches!(option, "tv" | "pc") {
                range = option;
            } else if let [matrix, primaries, transfer] = option.split('/').collect::<Vec<_>>().as_slice() {
                tags = [matrix, primaries, transfer];
            } else if !matches!(option, "progressive" | "top first" | "bottom first" | "unknown" | "chroma location: left" | "") &&
                !option.contains(' ') && !option.contains(':') {
                // FFmpeg collapses three identical tags to one name.
                tags = [option; 3];
            }
        }
    }
    let (width, height) = fields.iter().find_map(|field| {
        let dimensions = field.split_whitespace().next()?;
        let (width, height) = dimensions.split_once('x')?;
        Some((width.parse().ok()?, height.parse().ok()?))
    })?;
    let rate = fields.iter().find_map(|field| field.strip_suffix(" fps")).unwrap_or("unknown");
    Some(ProbeInfo {
        codec_name,
        profile: codec_profile.into(),
        pix_fmt,
        bits_per_raw_sample: None,
        width,
        height,
        frames: None,
        avg_frame_rate: rate.into(),
        color_space: tags[0].into(),
        color_primaries: tags[1].into(),
        color_transfer: tags[2].into(),
        color_range: range.into(),
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

/// Metadata mode reads one encoded packet. Full mode decodes and counts the
/// entire video with the same FFmpeg invocation that describes its stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputVerify {
    Metadata,
    Full,
}

pub fn validate_output(
    ffmpeg: &Path,
    file: &Path,
    contract: &OutputContract<'_>,
    verify: OutputVerify,
) -> Result<ProbeInfo> {
    let config = contract.config;
    let profile = contract.profile;
    let range = config.effective_range(profile);
    if let Err(message) = config.validate_geometry(contract.width, contract.height) {
        return Err(Error::new(EXIT_PREVIEW, message));
    }
    let probe = inspect_output(ffmpeg, file, verify)?;
    if config.codec_name() != "unknown" && probe.codec_name != config.codec_name() {
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
    if !accepted.is_empty() && !accepted.contains(&probe.pix_fmt.as_str()) {
        return Err(Error::new(
            EXIT_PREVIEW,
            format!(
                "output pixel format is {:?}, expected one of {:?} for {}",
                probe.pix_fmt, accepted, config.encoder
            ),
        ));
    }
    if let Some(expected) = config.probe_profile() {
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
    match (verify, probe.frames) {
        (_, Some(observed)) if observed != contract.frames as u64 => {
            return Err(Error::new(
                EXIT_PREVIEW,
                format!(
                    "output has {observed} frames, expected {}",
                    contract.frames
                ),
            ));
        }
        // A count is only observable by decoding in this container.
        (OutputVerify::Full, None) => {
            return Err(Error::new(
                EXIT_PREVIEW,
                "full output verification could not count frames",
            ));
        }
        _ => {}
    }
    if probe.avg_frame_rate != "unknown" && !rate_matches(&probe.avg_frame_rate, contract.fps) {
        return Err(Error::new(
            EXIT_PREVIEW,
            format!(
                "output frame rate is {:?}, expected {}/{}",
                probe.avg_frame_rate, contract.fps.num, contract.fps.den
            ),
        ));
    }

    // Validate every accurate field; unknown Log transfer/primaries stay unknown.
    let rgb = config.export_output_rgb();
    for (field, expected, observed) in [
        (
            "color_space",
            rgb.map(|rgb| if rgb { "gbr" } else { profile.tag_policy.matrix_token() }),
            &probe.color_space,
        ),
        (
            "color_primaries",
            Some(profile.tag_policy.primaries_token()),
            &probe.color_primaries,
        ),
        (
            "color_transfer",
            Some(profile.tag_policy.transfer_token()),
            &probe.color_transfer,
        ),
    ] {
        let Some(expected) = expected else { continue };
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
    // FFmpeg may omit the range from the input summary even for a
    // stream whose decoder defaults to limited range.
    if rgb.is_some() && probe.color_range != "unknown" && !range_matches {
        return Err(Error::new(
            EXIT_PREVIEW,
            format!(
                "output color_range is {:?}, expected {expected_range:?} for {} ({})",
                probe.color_range, profile.id, config.encoder
            ),
        ));
    }

    Ok(probe)
}

pub fn available_hardware_profiles(ffmpeg: &Path) -> Result<Vec<rawtl_linear_video::export::ExportProfile>> {
    rawtl_linear_video::export::hardware_profiles(|args| {
        proc::run_capture(ffmpeg, args, EXIT_USAGE_FOR_PREFLIGHT)
            .map(|result| (result.status.success(), format!("{}\n{}", result.stdout, result.stderr)))
            .map_err(|error| error.message)
    })
    .map_err(|message| Error::new(EXIT_USAGE_FOR_PREFLIGHT, message))
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
) -> Result<()> {
    if let Some((width, height)) = explicit_geometry {
        if let Err(message) = config.validate_geometry(width, height) {
            return Err(Error::new(EXIT_USAGE_FOR_PREFLIGHT, message));
        }
    }
    // Discovery already exercised this variant on the selected FFmpeg/device.
    if config.export().is_some_and(|profile| profile.is_hardware()) {
        return Ok(());
    }
    let declared_formats = if config.export().is_none() {
        supported_encoder_pixel_formats(ffmpeg, &config.encoder)?
    } else {
        None
    };
    if let Some(formats) = declared_formats {
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
    let geometry = explicit_geometry.unwrap_or_else(|| {
        if config.export().is_some() { (640, 480) } else { (32, 32) }
    });
    let mut args = config.preflight_args(profile, geometry, &fps.arg(), Path::new("pipe:1"));
    if let Some(index) = args.iter().rposition(|arg| arg == "-movflags") {
        // MOV needs fragmentation on a non-seekable test pipe. The actual
        // export still uses the normal seekable-file muxer settings.
        args[index + 1] = "+frag_keyframe+empty_moov+write_colr".into();
    }
    let captured = proc::run_capture_errors(ffmpeg, &args, EXIT_PREVIEW)?;
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
        let process = proc::OwnedProcess::spawn_piped_active(
            program,
            args,
            log,
            EXIT_PREVIEW,
            true,
            false,
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
            .note(&format!("pixel bytes sent to FFmpeg: {}", self.bytes));
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
            // FFmpeg prints decimal frame rates to two decimal places.
            (value - expected).abs() <= if observed.contains('/') { 1e-6 * expected.max(1.0) } else { 0.00501 }
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

/// The narrow runtime inputs the encoder node needs, assembled once by the
/// runner. Encode tasks clone this instead of depending on (and cloning) the
/// whole CLI options surface.
#[derive(Clone)]
pub struct EncoderInputs {
    pub profile: ColorProfile,
    pub fps: crate::cli::Fps,
    pub ffmpeg: std::path::PathBuf,
    pub depth: crate::color_profile::SignalDepth,
}

pub enum EncodeFrame<'a> {
    Legacy(&'a [u16]),
    TargetRgb(&'a [[f32; 3]]),
}

/// The streaming encode node packs target colour and writes it to FFmpeg.
pub struct GraphEncoder {
    config: ResolvedConfig,
    frames: usize,
    output_name: String,
    encoder: Option<StreamingEncoder>,
    output: Option<std::path::PathBuf>,

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

            command: None,
            active: Default::default(),
        }
    }

    pub fn begin(&mut self) -> Result<()> {
        proc::check_stop()
    }

    pub fn push(
        &mut self,
        inputs: &EncoderInputs,
        scratch: &Path,
        info: &crate::frame::FrameInfo,
        frame: EncodeFrame<'_>,
    ) -> Result<()> {
        proc::check_stop()?;
        if self.encoder.is_none() {
            if let Err(message) = self.config.validate_geometry(info.width, info.height) {
                return Err(Error::new(EXIT_USAGE_FOR_PREFLIGHT, message));
            }
        }
        use std::borrow::Cow;
        let bytes = match (self.config.export(), frame) {
            (Some(export), EncodeFrame::TargetRgb(rgb)) => Cow::Owned(export.input.pack(
                rgb, info.width, info.height, self.config.effective_range(&inputs.profile),
                inputs.profile.ycbcr_coding().map_err(|err| Error::new(EXIT_PREVIEW, err))?,
            ).map_err(|err| Error::new(EXIT_PREVIEW, err))?),
            (None, EncodeFrame::Legacy(signal)) => {
                let expected = (info.width as usize).checked_mul(info.height as usize)
                    .and_then(|pixels| pixels.checked_mul(3));
                if expected != Some(signal.len()) {
                    return Err(Error::new(EXIT_PREVIEW, format!(
                        "encoder input has {} samples, incompatible with {}x{} 4:4:4 {}-bit signal",
                        signal.len(), info.width, info.height, inputs.depth.token()
                    )));
                }
                signal_bytes(signal)
            }
            _ => return Err(Error::new(EXIT_PREVIEW, "encoder received the wrong target colour representation")),
        };
        if self.encoder.is_none() {
            let output = scratch.join(&self.output_name);
            let args = self.config.encoder_args(
                &inputs.profile,
                info.width,
                info.height,
                self.frames,
                &inputs.fps.arg(),
                inputs.depth,
                &output,
            );
            let key = scratch.join("encode");
            self.command = Some(proc::display_command(&inputs.ffmpeg, &args));
            self.output = Some(output);
            self.encoder = Some(StreamingEncoder::spawn(
                &inputs.ffmpeg,
                &args,
                &key,
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
        // FFmpeg's -frames:v may finish successfully before explicit end().
        // Mark the final transfer before writing; a premature close during the
        // write is still an error (broken pipe), and output validation remains
        // responsible for exact frame coverage after a successful final write.
        if info.index == self.frames {
            encoder
                .final_input
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
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
                    "streaming encoder failed: {}; {}",
                    proc::exit_description(&status),
                    proc::tail(&encoder.process.stderr(), 1200)
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
    pub fn failure_detail(&mut self) -> String {
        let Some(encoder) = &mut self.encoder else { return String::new(); };
        let _ = encoder.finish(true);
        proc::tail(&encoder.process.stderr(), 1200)
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
mod output_verify_tests {
    use super::*;
    use crate::encoder::Patch;

    struct Harness {
        root: std::path::PathBuf,
    }

    impl Harness {
        fn new(name: &str) -> Self {
            let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target/test-tmp")
                .join(format!("verify-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn script(&self, name: &str, lines: &[String]) -> std::path::PathBuf {
            use std::os::unix::fs::PermissionsExt;
            let path = self.root.join(name);
            fs::write(&path, format!("#!/bin/sh\n{}\n", lines.join("\n"))).unwrap();
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).unwrap();
            path
        }

        fn ffmpeg(&self, config: &ResolvedConfig, profile: &ColorProfile, frames: Option<u64>) -> std::path::PathBuf {
            let mut lines = vec![format!(
                "printf '%s\n' \"$@\" > {}",
                self.root.join("ffmpeg.argv").display()
            )];
            let name = config.probe_profile().unwrap_or("unknown");
            let tags = profile.tag_policy;
            let range = config.effective_range(profile).as_str();
            lines.push(format!(
                "echo '  Stream #0:0: Video: {} ({}), {}({}, {}/{}/{}), 64x48, 60 fps' >&2",
                config.codec_name(), name, config.pix_fmt, range,
                tags.matrix_token(), tags.primaries_token(), tags.transfer_token()
            ));
            if let Some(count) = frames {
                lines.push(format!("printf 'frame={count}\nprogress=end\n'"));
            }
            self.script("ffmpeg", &lines)
        }

        fn argv(&self, name: &str) -> Vec<String> {
            fs::read_to_string(self.root.join(name))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn contract<'a>(
        config: &'a ResolvedConfig,
        profile: &'a ColorProfile,
        frames: usize,
    ) -> OutputContract<'a> {
        OutputContract {
            config,
            profile,
            width: 64,
            height: 48,
            frames,
            fps: Fps::DEFAULT,
        }
    }

    fn scene(name: &str, frames: Option<u64>) -> (Harness, ResolvedConfig, &'static ColorProfile) {
        let harness = Harness::new(name);
        let config =
            ResolvedConfig::resolve(None, &Patch::default(), crate::color_profile::SignalDepth::Ten)
                .unwrap();
        let profile = crate::color_profile::default_profile();
        let _ = frames;
        (harness, config, profile)
    }

    #[test]
    fn metadata_mode_reads_one_packet_without_decoding() {
        let (harness, config, profile) = scene("metadata", None);
        let ffmpeg = harness.ffmpeg(&config, profile, None);
        let output = harness.root.join("video.mov");
        let probe = validate_output(&ffmpeg, &output, &contract(&config, profile, 3),
            OutputVerify::Metadata).unwrap();
        assert_eq!(probe.frames, None);
        let argv = harness.argv("ffmpeg.argv");
        assert!(argv.windows(2).any(|pair| pair == ["-c:v", "copy"]));
        assert!(argv.windows(2).any(|pair| pair == ["-frames:v", "1"]));
    }

    #[test]
    fn full_mode_decodes_and_counts_once() {
        let (harness, config, profile) = scene("full", Some(3));
        let ffmpeg = harness.ffmpeg(&config, profile, Some(3));
        let output = harness.root.join("video.mov");
        let probe = validate_output(&ffmpeg, &output, &contract(&config, profile, 3),
            OutputVerify::Full).unwrap();
        assert_eq!(probe.frames, Some(3));
        let argv = harness.argv("ffmpeg.argv");
        assert!(argv.iter().any(|arg| arg == "-xerror"));
        assert!(argv.windows(2).any(|pair| pair == ["-fps_mode", "passthrough"]));
        assert!(!argv.iter().any(|arg| arg == "copy" || arg == "-frames:v"));
    }

    #[test]
    fn full_mode_requires_the_exact_decoded_count() {
        for (count, expected) in [(None, "could not count frames"), (Some(5), "output has 5 frames, expected 3")] {
            let (harness, config, profile) = scene("count", count);
            let ffmpeg = harness.ffmpeg(&config, profile, count);
            let output = harness.root.join("video.mov");
            let error = validate_output(&ffmpeg, &output, &contract(&config, profile, 3),
                OutputVerify::Full).unwrap_err();
            assert_eq!(error.code, EXIT_PREVIEW);
            assert!(error.message.contains(expected), "{}", error.message);
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
