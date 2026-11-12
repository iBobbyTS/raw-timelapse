//! Streaming RGB frame conversion, H.264 encoding, validation and publication.

use crate::cli::Fps;
use crate::color_profile::{ColorProfile, TagPolicy};
use crate::error::{Error, Result, EXIT_PREVIEW, EXIT_PUBLISH, EXIT_VALIDATION};
use crate::json::Json;
use crate::proc;
#[cfg(test)]
use crate::transport::RGB48_FRAME;
use crate::transport::{
    self, ArrayDescriptor, ArrayLayout, ByteOrder, DType, DataEnvelope, DiskArtifact, MetadataDoc,
    NdArray, Payload, Provenance, AP1_F32_FRAME, RGB24_FRAME, VIDEO_ARTIFACT,
};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub const PREVIEW_CODEC: &str = "h264";
pub const PREVIEW_PIX_FMT: &str = "yuv420p";
pub const PREVIEW_MEDIA_TYPE: &str = "video/mp4";

/// Native stdout supplies scene-linear ACEScg codes.
/// RGB24 frames leaving the CPU conversion are described by the profile's
/// transfer and gamut names.
const SCENE_LINEAR_SPACE: &str = "ACEScg AP1/D60 scene-linear";
const SCENE_LINEAR_TRANSFER: &str = "linear";
const RGB24_PRODUCER: &str = "rawtl preview colour conversion";

pub const FINAL_NAME: &str = "output.mp4";

/// macOS VideoToolbox hardware H.264 rejects source widths above 4096
/// (`cannot create compression session: -12903`), so the display-referred
/// output is proportionally downscaled to this width.
pub const PREVIEW_MAX_WIDTH: u32 = 4096;

/// Applied preview geometry for one source frame size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewGeometry {
    pub width: u32,
    pub height: u32,
    pub scaled: bool,
}

/// Proportional preview downscale policy: widths at or below
/// [`PREVIEW_MAX_WIDTH`] are unchanged. Wider sources scale to that width; the
/// height is rounded to the nearest integer and raised to the nearest even
/// value (yuv420p chroma alignment), with a minimum of 2.
pub fn preview_geometry(width: u32, height: u32) -> PreviewGeometry {
    if width <= PREVIEW_MAX_WIDTH {
        return PreviewGeometry {
            width,
            height,
            scaled: false,
        };
    }
    let numerator = u64::from(height) * u64::from(PREVIEW_MAX_WIDTH);
    let mut scaled_height = ((numerator + u64::from(width) / 2) / u64::from(width)) as u32;
    scaled_height = scaled_height.max(2);
    if scaled_height % 2 == 1 {
        scaled_height += 1;
    }
    PreviewGeometry {
        width: PREVIEW_MAX_WIDTH,
        height: scaled_height,
        scaled: true,
    }
}

/// The `-vf` value used by the preview encoder. The unscaled chain is the
/// historical one; a scaled preview prepends RGB lanczos resampling before the
/// profile's colour-matrix conversion.
pub fn preview_filter(geometry: PreviewGeometry, profile: &ColorProfile) -> String {
    let convert = profile.color_filter();
    if geometry.scaled {
        format!(
            "scale={}:{}:flags=lanczos,{convert}",
            geometry.width, geometry.height
        )
    } else {
        convert
    }
}

/// One tightly packed HWC frame descriptor. Shape coordinates are always
/// `[height, width, channels]`; the declared byte length is the backing length,
/// so a decoder buffer that does not match the declared geometry is rejected at
/// construction time instead of reaching the pixel converter.
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
    let array = NdArray::new(
        bytes,
        rgb_frame_descriptor(DType::U16, width as usize, height as usize, byte_length),
    )?;
    let metadata = rgb_metadata(width, height, SCENE_LINEAR_SPACE, SCENE_LINEAR_TRANSFER)?;
    let provenance = Provenance::new(producer, Vec::new(), Vec::new())?;
    DataEnvelope::new(RGB48_FRAME, Payload::NdArray(array), metadata, provenance)
}

fn f32_frame_envelope(
    bytes: Arc<[u8]>,
    width: u32,
    height: u32,
) -> transport::Result<DataEnvelope> {
    let length = bytes.len();
    let array = NdArray::new(
        bytes,
        rgb_frame_descriptor(DType::F32, width as usize, height as usize, length),
    )?;
    DataEnvelope::new(
        AP1_F32_FRAME,
        Payload::NdArray(array),
        rgb_metadata(width, height, SCENE_LINEAR_SPACE, SCENE_LINEAR_TRANSFER)?,
        Provenance::new("dng-linear-frame FP32 stdout", vec![], vec![])?,
    )
}

/// Wrap one converted preview frame as a validated `RGB24_FRAME`.
fn rgb24_frame_envelope(
    bytes: Arc<[u8]>,
    width: u32,
    height: u32,
    profile: &ColorProfile,
    producer: &str,
) -> transport::Result<DataEnvelope> {
    let byte_length = bytes.len();
    let array = NdArray::new(
        bytes,
        rgb_frame_descriptor(DType::U8, width as usize, height as usize, byte_length),
    )?;
    let metadata = rgb_metadata(width, height, profile.gamut.name, profile.transfer.name())?;
    let provenance = Provenance::new(producer, Vec::new(), Vec::new())?;
    DataEnvelope::new(RGB24_FRAME, Payload::NdArray(array), metadata, provenance)
}

fn array_bytes(envelope: &DataEnvelope) -> &[u8] {
    match envelope.payload() {
        Payload::NdArray(array) => array.bytes(),
        _ => unreachable!("RGB frame envelopes always carry an NdArray payload"),
    }
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
    pub pix_fmt: String,
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
            ("pix_fmt", crate::json::Json::Str(self.pix_fmt.clone())),
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
        "stream=codec_name,pix_fmt,width,height,nb_read_frames,avg_frame_rate,color_space,color_primaries,color_transfer,color_range".to_string(),
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
    Ok(ProbeInfo {
        codec_name: required("codec_name")?,
        pix_fmt: required("pix_fmt")?,
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

pub fn validate_preview(
    ffprobe: &Path,
    ffmpeg: &Path,
    file: &Path,
    width: u32,
    height: u32,
    count: usize,
    fps: Fps,
    profile: &ColorProfile,
) -> Result<ProbeInfo> {
    let expected = preview_geometry(width, height);
    let probe = probe_stream(ffprobe, file)?;
    if probe.codec_name != PREVIEW_CODEC {
        return Err(Error::new(
            EXIT_PREVIEW,
            format!(
                "preview codec is {:?}, expected {:?}",
                probe.codec_name, PREVIEW_CODEC
            ),
        ));
    }
    let accepted_pix_fmts = profile.range.accepted_decoder_pix_fmts();
    if !accepted_pix_fmts.contains(&probe.pix_fmt.as_str()) {
        return Err(Error::new(
            EXIT_PREVIEW,
            format!(
                "preview pixel format is {:?}, expected one of {:?} for {} range ({})",
                probe.pix_fmt,
                accepted_pix_fmts,
                profile.range.as_str(),
                profile.id
            ),
        ));
    }
    if probe.width != expected.width || probe.height != expected.height {
        return Err(Error::new(
            EXIT_PREVIEW,
            format!(
                "preview is {}x{}, expected {}x{}",
                probe.width, probe.height, expected.width, expected.height
            ),
        ));
    }
    if probe.frames != count as u64 {
        return Err(Error::new(
            EXIT_PREVIEW,
            format!("preview has {} frames, expected {}", probe.frames, count),
        ));
    }
    if !rate_matches(&probe.avg_frame_rate, fps) {
        return Err(Error::new(
            EXIT_PREVIEW,
            format!(
                "preview frame rate is {:?}, expected {}/{}",
                probe.avg_frame_rate, fps.num, fps.den
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
        ("color_range", profile.range.as_str(), &probe.color_range),
    ] {
        let matches = observed == expected
            || (expected == "unknown"
                && matches!(
                    observed.as_str(),
                    "" | "unknown" | "unspecified" | "reserved"
                ));
        if !matches {
            return Err(Error::new(EXIT_PREVIEW, format!(
                "preview {field} is {observed:?}, expected {expected:?} for {} (per-field colour contract)", profile.id
            )));
        }
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
                "preview failed a full decode: {}; stderr: {}",
                proc::exit_description(&captured.status),
                proc::tail(&captured.stderr, 1200)
            ),
        ));
    }
    Ok(probe)
}

/// Start with the first frame's validated metadata/descriptor, then retain only
/// one FP32 and one RGB24 backing allocation per frame. The encoder owns no
/// input file; closing stdin is the end-of-sequence signal.
pub fn encode_stream(
    context: &crate::RunContext<'_>,
    evs: &[f64],
    output: &Path,
) -> Result<crate::frame::FrameSequence> {
    let options = context.options;
    let logs = context.output.join("logs/encode");
    let config = crate::frame::ProduceConfig {
        logs_dir: &logs,
        exposure_ev: None,
        analysis_roi: None,
        echo: true,
        echo_tag: "encode ",
        white_balance: options.white_balance,
        camera_color_method: options.preview_profile.camera_color_method(),
        converter: options.converter.as_deref(),
        converter_timeout: options.converter_timeout,
        log_relative_dir: "logs/encode",
    };
    let active: proc::ActiveNative = Default::default();
    let mut encoder: Option<StreamingEncoder> = None;
    let mut clipping = Vec::new();
    let result = crate::frame::stream_frames(
        &options.native_tool,
        context.sources,
        evs,
        &config,
        active.clone(),
        &mut |info, native| {
            if encoder.is_none() {
                let args = encoder_args(
                    info.width,
                    info.height,
                    context.sources.len(),
                    options.fps,
                    output,
                    options.preview_profile,
                );
                encoder = Some(StreamingEncoder::spawn(
                    &options.ffmpeg,
                    &args,
                    &context.output.join("logs/encode.log"),
                    active.clone(),
                )?);
            }
            let encoder = encoder.as_mut().unwrap();
            if encoder.failed.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(Error::new(
                    EXIT_PREVIEW,
                    "encoder failed while native frame was active",
                ));
            }
            let pixels = native.pixels(info.width, info.height)?;
            let sent = f32_frame_envelope(pixels, info.width, info.height).map_err(|e| {
                Error::new(
                    crate::error::EXIT_FRAME,
                    format!("invalid scene-linear frame: {e}"),
                )
            })?;
            // The output backing is initialized and unique until envelope creation.
            let mut rgb24 = unsafe {
                Arc::<[u8]>::new_zeroed_slice(info.width as usize * info.height as usize * 3)
                    .assume_init()
            };
            let conversion = options
                .preview_profile
                .convert_f32_frame(
                    array_bytes(&sent),
                    info.width as usize,
                    info.height as usize,
                    options.log_reference_gain,
                    &mut rgb24,
                )
                .map(|counts| clipping.push(counts));
            conversion.map_err(|e| Error::new(EXIT_PREVIEW, format!("colour conversion: {e}")))?;
            let converted = rgb24_frame_envelope(
                rgb24,
                info.width,
                info.height,
                options.preview_profile,
                RGB24_PRODUCER,
            )
            .map_err(|e| Error::new(EXIT_PREVIEW, format!("invalid RGB24 frame: {e}")))?;
            encoder
                .input
                .as_mut()
                .unwrap()
                .write_all(array_bytes(&converted))
                .map_err(|e| Error::new(EXIT_PREVIEW, format!("encoder stdin failed: {e}")))?;
            encoder.bytes += array_bytes(&converted).len();
            Ok(())
        },
    );
    if let Some(encoder) = &mut encoder {
        let counterpart_failed = encoder.failed.load(std::sync::atomic::Ordering::SeqCst);
        let status = encoder.finish(result.is_err())?;
        if counterpart_failed
            || result
                .as_ref()
                .err()
                .is_some_and(|e| e.code == EXIT_PREVIEW)
        {
            return Err(Error::new(
                EXIT_PREVIEW,
                format!(
                    "streaming encoder failed: {}; {}",
                    proc::exit_description(&status),
                    result
                        .as_ref()
                        .err()
                        .map(|e| e.message.as_str())
                        .unwrap_or("see logs/encode.log")
                ),
            ));
        }
        if result.is_ok() && !status.success() {
            return Err(Error::new(
                EXIT_PREVIEW,
                format!(
                    "streaming encoder failed: {}",
                    proc::exit_description(&status)
                ),
            ));
        }
    }
    let mut sequence = result?;
    for (frame, counts) in sequence.frames.iter_mut().zip(clipping) {
        frame.signal_clip_counts = Some(counts);
    }
    ensure_nonempty(output, EXIT_PREVIEW, "streaming encoder produced no video")?;
    Ok(sequence)
}

fn encoder_args(
    width: u32,
    height: u32,
    count: usize,
    fps: Fps,
    output: &Path,
    profile: &ColorProfile,
) -> Vec<String> {
    let geometry = preview_geometry(width, height);
    let mut args = vec![
        "-hide_banner".to_string(),
        "-v".to_string(),
        "error".to_string(),
        "-f".to_string(),
        "rawvideo".to_string(),
        "-pixel_format".to_string(),
        "rgb24".to_string(),
        "-video_size".to_string(),
        format!("{width}x{height}"),
        "-framerate".to_string(),
        fps.arg(),
        "-i".to_string(),
        "pipe:0".to_string(),
        "-frames:v".to_string(),
        count.to_string(),
        "-vf".to_string(),
        preview_filter(geometry, profile),
        "-c:v".to_string(),
        "h264_videotoolbox".to_string(),
        "-allow_sw".to_string(),
        "0".to_string(),
        "-profile:v".to_string(),
        "high".to_string(),
        "-b:v".to_string(),
        "0".to_string(),
        "-q:v".to_string(),
        "65".to_string(),
        "-pix_fmt".to_string(),
        PREVIEW_PIX_FMT.to_string(),
    ];
    if profile.tag_policy == TagPolicy::SRGB {
        // The historical sRGB argument sequence, kept byte-identical.
        for (flag, value) in [
            ("-colorspace", "bt709"),
            ("-color_primaries", "bt709"),
            ("-color_trc", "iec61966-2-1"),
            ("-color_range", profile.range.as_str()),
        ] {
            args.push(flag.to_string());
            args.push(value.to_string());
        }
    } else {
        // VideoToolbox on the supported build propagates these fields to SPS
        // VUI. Partial colour information needs write_colr for MP4 nclx.
        for (flag, value) in [
            ("-colorspace", profile.tag_policy.matrix_token()),
            ("-color_primaries", profile.tag_policy.primaries_token()),
            ("-color_trc", profile.tag_policy.transfer_token()),
            ("-color_range", profile.range.as_str()),
        ] {
            args.push(flag.to_string());
            args.push(value.to_string());
        }
    }
    let movflags = if profile.tag_policy == TagPolicy::SRGB {
        "+faststart"
    } else {
        "+faststart+write_colr"
    };
    for (flag, value) in [("-movflags", movflags), ("-f", "mp4")] {
        args.push(flag.to_string());
        args.push(value.to_string());
    }
    args.push(output.display().to_string());

    args
}

struct StreamingEncoder {
    process: proc::OwnedProcess,
    input: Option<std::process::ChildStdin>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
        let process = proc::OwnedProcess::spawn(program, args, log, EXIT_PREVIEW, true)?;
        let input = process.child.lock().unwrap().stdin.take();
        let child = process.child.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let (stop_watch, failed_watch) = (stop.clone(), failed.clone());
        let watcher = std::thread::spawn(move || {
            while !stop_watch.load(Ordering::SeqCst) {
                let exited = child.lock().unwrap().try_wait();
                if let Ok(Some(status)) = exited {
                    if !status.success() {
                        failed_watch.store(true, Ordering::SeqCst);
                        let mut state = active.lock().unwrap();
                        state.cancelled = true;
                        if let Some(native) = state.child.as_ref() {
                            let _ = native.lock().unwrap().kill();
                        }
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
            watcher: Some(watcher),
            bytes: 0,
        })
    }
    fn finish(&mut self, abort: bool) -> Result<std::process::ExitStatus> {
        self.input.take();
        // Stop the observer before our intentional cancellation, so a native
        // protocol error keeps its primary family 30 rather than becoming 60.
        if abort {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        if abort {
            if let Some(w) = self.watcher.take() {
                let _ = w.join();
            }
        }
        let status = self.process.finish(abort);
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(w) = self.watcher.take() {
            let _ = w.join();
        }
        self.process
            .note(&format!("pixel bytes (RGB24): {}", self.bytes));
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
    if destination.exists() {
        return Err(Error::new(
            EXIT_PUBLISH,
            format!(
                "refusing to overwrite existing deliverable {}",
                destination.display()
            ),
        ));
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
    fn preview_geometry_keeps_material_at_or_below_the_limit() {
        for (width, height) in [(4096, 2304), (4000, 2256), (8, 6), (1, 1)] {
            assert_eq!(
                preview_geometry(width, height),
                PreviewGeometry {
                    width,
                    height,
                    scaled: false
                }
            );
        }
    }

    #[test]
    fn preview_geometry_scales_wider_material_proportionally() {
        // The two 25 MP real-material sources (CR3 ADC output and DxO DNG).
        assert_eq!(
            preview_geometry(6984, 4660),
            PreviewGeometry {
                width: 4096,
                height: 2734,
                scaled: true
            }
        );
        assert_eq!(
            preview_geometry(6960, 4640),
            PreviewGeometry {
                width: 4096,
                height: 2732,
                scaled: true
            }
        );
        // Aspect ratio is preserved within one pixel and the width is the cap.
        for (width, height) in [(5000u32, 3000u32), (12000, 8000), (8192, 8192)] {
            let geometry = preview_geometry(width, height);
            assert!(geometry.scaled);
            assert_eq!(geometry.width, PREVIEW_MAX_WIDTH);
            let expected = f64::from(height) * f64::from(PREVIEW_MAX_WIDTH) / f64::from(width);
            assert!((f64::from(geometry.height) - expected).abs() <= 1.5);
        }
    }

    #[test]
    fn preview_geometry_aligns_chroma_and_clamps_tiny_heights() {
        // Nearest integer is 999 (odd), raised to 1000 for yuv420p.
        assert_eq!(
            preview_geometry(4100, 1000),
            PreviewGeometry {
                width: 4096,
                height: 1000,
                scaled: true
            }
        );
        // A degenerate aspect still yields an even, non-zero height.
        assert_eq!(
            preview_geometry(20000, 1),
            PreviewGeometry {
                width: 4096,
                height: 2,
                scaled: true
            }
        );
        for (width, height) in [(6984u32, 4660u32), (4100, 1000), (5000, 3000), (20000, 1)] {
            assert_eq!(preview_geometry(width, height).height % 2, 0);
        }
    }

    #[test]
    fn preview_filter_only_prepends_lanczos_when_scaled() {
        let srgb = crate::color_profile::lookup("srgb-preview").expect("srgb");
        assert_eq!(
            preview_filter(preview_geometry(8, 6), srgb),
            "scale=out_color_matrix=bt709:out_range=tv,\
             setparams=color_primaries=bt709:color_trc=iec61966-2-1:colorspace=bt709:range=tv"
        );
        assert_eq!(
            preview_filter(preview_geometry(5000, 3000), srgb),
            format!("scale=4096:2458:flags=lanczos,{}", srgb.color_filter())
        );
    }

    #[test]
    fn vendor_preview_filter_writes_known_fields() {
        let profile = crate::color_profile::lookup("dji-dlog-dgamut").expect("dji");
        assert_eq!(
            preview_filter(preview_geometry(8, 6), profile),
            "scale=out_color_matrix=bt2020:out_range=pc,\
             setparams=color_primaries=unknown:color_trc=unknown:colorspace=bt2020nc:range=pc"
        );
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

    #[test]
    fn rgb24_typed_frame_carries_the_profile_colour_metadata() {
        let profile = crate::color_profile::lookup("srgb-preview").expect("srgb");
        let bytes: Arc<[u8]> = vec![0u8; 8 * 6 * 3].into();
        let envelope =
            rgb24_frame_envelope(bytes, 8, 6, profile, RGB24_PRODUCER).expect("valid RGB24 frame");
        assert_eq!(envelope.semantic_type(), RGB24_FRAME);
        assert_eq!(envelope.transport_kind(), transport::TransportKind::NdArray);
        assert_eq!(
            envelope
                .metadata()
                .get("color.space")
                .and_then(Json::as_str),
            Some(profile.gamut.name)
        );
        assert_eq!(
            envelope
                .metadata()
                .get("color.transfer")
                .and_then(Json::as_str),
            Some(profile.transfer.name())
        );
        match envelope.payload() {
            Payload::NdArray(array) => {
                assert_eq!(array.shape(), &[6, 8, 3]);
                assert_eq!(array.byte_strides(), &[24, 3, 1]);
                assert_eq!(array.dtype(), DType::U8);
            }
            other => panic!("unexpected payload {other:?}"),
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
        let source = root.join("temporary.mp4");
        let destination = root.join("output.mp4");
        fs::write(&source, b"validated").unwrap();
        fs::write(&destination, b"existing").unwrap();
        let envelope = video_envelope(
            &source,
            "video/mp4",
            "h264",
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
}
