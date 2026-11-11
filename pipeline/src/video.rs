//! Linear video encoding, probe validation, lossless roundtrip and atomic
//! publication, plus the display-referred/camera-Log preview pipeline.

use crate::cli::Fps;
use crate::color_profile::{ColorProfile, TagPolicy};
use crate::error::{Error, Result, EXIT_ENCODE, EXIT_PREVIEW, EXIT_PUBLISH, EXIT_VALIDATION};
use crate::json::Json;
use crate::proc;
use crate::transport::{
    self, ArrayDescriptor, ArrayLayout, ByteOrder, DType, DataEnvelope, DiskArtifact, MetadataDoc,
    NdArray, Payload, Provenance, RGB24_FRAME, RGB48_FRAME, VIDEO_ARTIFACT,
};
use std::fs;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const LINEAR_CODEC: &str = "ffv1";
pub const LINEAR_PIX_FMT: &str = "gbrp16le";
pub const PREVIEW_CODEC: &str = "h264";
pub const PREVIEW_PIX_FMT: &str = "yuv420p";
pub const LINEAR_MEDIA_TYPE: &str = "video/x-matroska";
pub const PREVIEW_MEDIA_TYPE: &str = "video/mp4";

/// The preview pipe decodes the linear master into scene-linear ACEScg codes.
/// RGB24 frames leaving the CPU conversion are described by the profile's
/// transfer and gamut names.
const SCENE_LINEAR_SPACE: &str = "ACEScg AP1/D60 scene-linear";
const SCENE_LINEAR_TRANSFER: &str = "linear";
const RGB48_PRODUCER: &str = "ffmpeg rgb48le decoder";
const RGB24_PRODUCER: &str = "rawtl preview colour conversion";

pub const LINEAR_FINAL_NAME: &str = "linear-acescg-16bit-ffv1.mkv";

pub fn preview_final_name(profile: &ColorProfile) -> &'static str {
    profile.preview_filename
}

/// macOS VideoToolbox hardware H.264 rejects source widths above 4096
/// (`cannot create compression session: -12903`), so the display-referred
/// preview is proportionally downscaled to this width. The FFV1 master is
/// never scaled.
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

fn typed_frame_error(
    err: transport::TransportError,
    stream: &str,
    index: usize,
    count: usize,
) -> Error {
    Error::new(
        EXIT_VALIDATION,
        format!(
            "decoded frame {}/{count} from {stream} is not a valid RGB48 typed frame: {err}",
            index + 1
        ),
    )
}

/// Build the typed handoff for an encoded deliverable. `audit` consumes the
/// same envelope, and `publish` renames the referenced file.
pub fn video_envelope(
    path: &Path,
    media_type: &str,
    codec: &str,
    frame_count: u64,
    producer: &str,
) -> transport::Result<DataEnvelope> {
    let artifact = DiskArtifact::new(path, media_type, file_size(path), None)?;
    let metadata = MetadataDoc::new(vec![
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

pub fn validate_linear(
    probe: &ProbeInfo,
    width: u32,
    height: u32,
    count: usize,
    fps: Fps,
) -> Result<()> {
    if probe.codec_name != LINEAR_CODEC {
        return Err(Error::new(
            EXIT_VALIDATION,
            format!(
                "linear video codec is {:?}, expected {:?}",
                probe.codec_name, LINEAR_CODEC
            ),
        ));
    }
    if probe.pix_fmt != LINEAR_PIX_FMT {
        return Err(Error::new(
            EXIT_VALIDATION,
            format!(
                "linear video pixel format is {:?}, expected {:?} (16-bit lossless RGB)",
                probe.pix_fmt, LINEAR_PIX_FMT
            ),
        ));
    }
    if probe.width != width || probe.height != height {
        return Err(Error::new(
            EXIT_VALIDATION,
            format!(
                "linear video is {}x{}, expected {}x{}",
                probe.width, probe.height, width, height
            ),
        ));
    }
    if probe.frames != count as u64 {
        return Err(Error::new(
            EXIT_VALIDATION,
            format!(
                "linear video has {} frames, expected {}",
                probe.frames, count
            ),
        ));
    }
    if !rate_matches(&probe.avg_frame_rate, fps) {
        return Err(Error::new(
            EXIT_VALIDATION,
            format!(
                "linear video frame rate is {:?}, expected {}/{}",
                probe.avg_frame_rate, fps.num, fps.den
            ),
        ));
    }
    Ok(())
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
        let matches = observed == expected || (expected == "unknown" && observed.is_empty());
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

pub fn encode_linear(
    ffmpeg: &Path,
    frames_dir: &Path,
    count: usize,
    fps: Fps,
    output: &Path,
    log_path: &Path,
) -> Result<()> {
    let pattern = frames_dir.join("frame_%06d.png");
    let args = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-v".to_string(),
        "error".to_string(),
        "-framerate".to_string(),
        fps.arg(),
        "-start_number".to_string(),
        "1".to_string(),
        "-i".to_string(),
        pattern.display().to_string(),
        "-frames:v".to_string(),
        count.to_string(),
        "-c:v".to_string(),
        LINEAR_CODEC.to_string(),
        "-level".to_string(),
        "3".to_string(),
        "-g".to_string(),
        "1".to_string(),
        "-slicecrc".to_string(),
        "1".to_string(),
        "-pix_fmt".to_string(),
        LINEAR_PIX_FMT.to_string(),
        "-map_metadata".to_string(),
        "-1".to_string(),
        "-f".to_string(),
        "matroska".to_string(),
        output.display().to_string(),
    ];
    let command = proc::display_command(ffmpeg, &args);
    let captured = proc::run_capture(ffmpeg, &args, EXIT_ENCODE)?;
    write_log(
        log_path,
        &format!(
            "command: {command}\nexit: {} ({:.3}s)\nstdout:\n{}\nstderr:\n{}\n",
            proc::exit_description(&captured.status),
            captured.duration.as_secs_f64(),
            captured.stdout,
            captured.stderr
        ),
    )?;
    if !captured.status.success() {
        return Err(Error::new(
            EXIT_ENCODE,
            format!(
                "FFV1 encoder failed: {}; stderr: {}; transcript {}",
                proc::exit_description(&captured.status),
                proc::tail(&captured.stderr, 1200),
                log_path.display()
            ),
        ));
    }
    ensure_nonempty(output, EXIT_ENCODE, "encoder produced no video")?;
    Ok(())
}

pub fn render_preview(
    ffmpeg: &Path,
    frames_dir: &Path,
    width: u32,
    height: u32,
    count: usize,
    fps: Fps,
    raw_output: &Path,
    log_path: &Path,
    profile: &ColorProfile,
) -> Result<()> {
    let pattern = frames_dir.join("frame_%06d.png");
    let args = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-v".to_string(),
        "error".to_string(),
        "-framerate".to_string(),
        fps.arg(),
        "-start_number".to_string(),
        "1".to_string(),
        "-i".to_string(),
        pattern.display().to_string(),
        "-frames:v".to_string(),
        count.to_string(),
        "-pix_fmt".to_string(),
        "rgb48le".to_string(),
        "-f".to_string(),
        "rawvideo".to_string(),
        "pipe:1".to_string(),
    ];
    let command = proc::display_command(ffmpeg, &args);
    let start = Instant::now();
    let mut child = spawn_stdout(ffmpeg, &args, EXIT_PREVIEW)?;
    let mut stdout = child.stdout.take().expect("piped stdout");

    let file = fs::File::create(raw_output).map_err(|err| {
        Error::new(
            EXIT_PREVIEW,
            format!(
                "cannot create preview raw frame file {}: {err}",
                raw_output.display()
            ),
        )
    })?;
    let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, file);

    let frame_bytes = (width as usize) * (height as usize) * 6;
    let mut frame = vec![0u8; frame_bytes];
    let mut rgb24 = Vec::with_capacity((width as usize) * (height as usize) * 3);
    let mut result = Ok(());

    for index in 0..count {
        if let Err(err) = read_exact_frame(&mut stdout, &mut frame, "PNG sequence", index, count) {
            result = Err(err);
            break;
        }
        let sent = match rgb48_frame_envelope(
            Arc::from(frame.as_slice()),
            width,
            height,
            RGB48_PRODUCER,
        ) {
            Ok(envelope) => envelope,
            Err(err) => {
                result = Err(Error::new(
                    EXIT_PREVIEW,
                    format!(
                        "preview conversion failed at frame {} ({}): rgb48 input is not a valid typed frame: {err}",
                        index + 1,
                        profile.id
                    ),
                ));
                break;
            }
        };
        if let Err(err) = profile.convert_frame(
            array_bytes(&sent),
            width as usize,
            height as usize,
            &mut rgb24,
        ) {
            result = Err(Error::new(
                EXIT_PREVIEW,
                format!(
                    "preview conversion failed at frame {} ({}): {err}",
                    index + 1,
                    profile.id
                ),
            ));
            break;
        }
        let converted = match rgb24_frame_envelope(
            Arc::from(rgb24.as_slice()),
            width,
            height,
            profile,
            RGB24_PRODUCER,
        ) {
            Ok(envelope) => envelope,
            Err(err) => {
                result = Err(Error::new(
                    EXIT_PREVIEW,
                    format!(
                        "preview conversion failed at frame {} ({}): rgb24 output is not a valid typed frame: {err}",
                        index + 1,
                        profile.id
                    ),
                ));
                break;
            }
        };
        if let Err(err) = writer.write_all(array_bytes(&converted)) {
            result = Err(Error::new(
                EXIT_PREVIEW,
                format!(
                    "cannot write preview raw frame {}: {err}",
                    raw_output.display()
                ),
            ));
            break;
        }
    }

    if result.is_ok() {
        let mut extra = [0u8; 1];
        match stdout.read(&mut extra) {
            Ok(0) => {}
            Ok(_) => {
                result = Err(Error::new(
                    EXIT_PREVIEW,
                    "PNG sequence produced more frames than expected".to_string(),
                ))
            }
            Err(err) => {
                result = Err(Error::new(
                    EXIT_PREVIEW,
                    format!("cannot read PNG sequence decoder output: {err}"),
                ))
            }
        }
    }

    drop(stdout);
    let status = child.wait().map_err(|err| {
        Error::new(
            EXIT_PREVIEW,
            format!("cannot wait for PNG sequence decoder: {err}"),
        )
    });
    let _ = writer.flush();
    let elapsed = start.elapsed();
    let _ = write_log(
        log_path,
        &format!(
            "command: {command}\nframes: {}x{} x{}\nelapsed: {:.3}s\nexit: {}\nnote: linear RGB16 codes decoded by ffmpeg to rgb48le, then converted to the selected preview profile's RGB24 in Rust\n",
            width,
            height,
            count,
            elapsed.as_secs_f64(),
            match &status {
                Ok(status) => proc::exit_description(status),
                Err(err) => format!("wait failed: {err}"),
            }
        ),
    );

    result?;
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            return Err(Error::new(
                EXIT_PREVIEW,
                format!(
                    "PNG sequence decoder failed: {}; transcript {}",
                    proc::exit_description(&status),
                    log_path.display()
                ),
            ))
        }
        Err(err) => return Err(err),
    }
    Ok(())
}

pub fn encode_preview(
    ffmpeg: &Path,
    raw_input: &Path,
    width: u32,
    height: u32,
    count: usize,
    fps: Fps,
    output: &Path,
    log_path: &Path,
    profile: &ColorProfile,
) -> Result<()> {
    let geometry = preview_geometry(width, height);
    let mut args = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
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
        raw_input.display().to_string(),
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

    let command = proc::display_command(ffmpeg, &args);
    let captured = proc::run_capture(ffmpeg, &args, EXIT_PREVIEW)?;
    // Keep the unscaled sRGB log byte-identical; scaled runs and non-sRGB
    // profiles record the applied geometry/profile alongside the source size.
    let scale_note = if geometry.scaled {
        format!(
            "\npreview scale: {}x{} -> {}x{} (lanczos; VideoToolbox max width {})",
            width, height, geometry.width, geometry.height, PREVIEW_MAX_WIDTH
        )
    } else {
        String::new()
    };
    let profile_note = if profile.id == "srgb-preview" {
        String::new()
    } else {
        format!(
            "\npreview profile: {} (range {}, ycbcr matrix {}, per-field colour tags)",
            profile.id,
            profile.range.as_str(),
            profile.ycbcr_matrix
        )
    };
    write_log(
        log_path,
        &format!(
            "command: {command}\ninput: {}x{} rgb24 x{} @ {}{}{}\nexit: {} ({:.3}s)\nstdout:\n{}\nstderr:\n{}\n",
            width,
            height,
            count,
            fps.arg(),
            scale_note,
            profile_note,
            proc::exit_description(&captured.status),
            captured.duration.as_secs_f64(),
            captured.stdout,
            captured.stderr
        ),
    )?;
    if !captured.status.success() {
        let label = if profile.id == "srgb-preview" {
            "sRGB preview encoder failed".to_string()
        } else {
            format!("{} preview encoder failed", profile.id)
        };
        return Err(Error::new(
            EXIT_PREVIEW,
            format!(
                "{label}: {}; stderr: {}; transcript {}",
                proc::exit_description(&captured.status),
                proc::tail(&captured.stderr, 1200),
                log_path.display()
            ),
        ));
    }
    ensure_nonempty(output, EXIT_PREVIEW, "preview encoder produced no video")?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct RoundtripReport {
    pub frames_compared: usize,
    pub frame_bytes: usize,
    pub elapsed: Duration,
}

pub fn verify_roundtrip(
    ffmpeg: &Path,
    frames_dir: &Path,
    video: &Path,
    width: u32,
    height: u32,
    count: usize,
    fps: Fps,
    log_path: &Path,
) -> Result<RoundtripReport> {
    let pattern = frames_dir.join("frame_%06d.png");
    let png_args = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-v".to_string(),
        "error".to_string(),
        "-framerate".to_string(),
        fps.arg(),
        "-start_number".to_string(),
        "1".to_string(),
        "-i".to_string(),
        pattern.display().to_string(),
        "-frames:v".to_string(),
        count.to_string(),
        "-pix_fmt".to_string(),
        "rgb48le".to_string(),
        "-f".to_string(),
        "rawvideo".to_string(),
        "pipe:1".to_string(),
    ];
    let video_args = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-v".to_string(),
        "error".to_string(),
        "-i".to_string(),
        video.display().to_string(),
        "-pix_fmt".to_string(),
        "rgb48le".to_string(),
        "-f".to_string(),
        "rawvideo".to_string(),
        "pipe:1".to_string(),
    ];
    let png_command = proc::display_command(ffmpeg, &png_args);
    let video_command = proc::display_command(ffmpeg, &video_args);
    let start = Instant::now();

    let mut png_child = spawn_stdout(ffmpeg, &png_args, EXIT_VALIDATION)?;
    let mut video_child = match spawn_stdout(ffmpeg, &video_args, EXIT_VALIDATION) {
        Ok(child) => child,
        Err(err) => {
            kill_and_wait(&mut png_child);
            return Err(err);
        }
    };
    let mut png_stdout = png_child.stdout.take().expect("piped stdout");
    let mut video_stdout = video_child.stdout.take().expect("piped stdout");

    let frame_bytes = (width as usize) * (height as usize) * 6;
    let mut sent = vec![0u8; frame_bytes];
    let mut encoded = vec![0u8; frame_bytes];
    let mut failure: Option<Error> = None;

    for index in 0..count {
        if let Err(err) = read_exact_frame(&mut png_stdout, &mut sent, "PNG sequence", index, count)
        {
            failure = Some(err);
            break;
        }
        if let Err(err) =
            read_exact_frame(&mut video_stdout, &mut encoded, "FFV1 video", index, count)
        {
            failure = Some(err);
            break;
        }
        // Both decoded streams pass through the same typed RGB48 construction
        // and semantic validation before the byte comparison.
        let sent_frame =
            match rgb48_frame_envelope(Arc::from(sent.as_slice()), width, height, RGB48_PRODUCER) {
                Ok(envelope) => envelope,
                Err(err) => {
                    failure = Some(typed_frame_error(err, "PNG sequence", index, count));
                    break;
                }
            };
        let encoded_frame = match rgb48_frame_envelope(
            Arc::from(encoded.as_slice()),
            width,
            height,
            RGB48_PRODUCER,
        ) {
            Ok(envelope) => envelope,
            Err(err) => {
                failure = Some(typed_frame_error(err, "FFV1 video", index, count));
                break;
            }
        };
        let sent_bytes = array_bytes(&sent_frame);
        let encoded_bytes = array_bytes(&encoded_frame);
        if sent_bytes != encoded_bytes {
            failure = Some(describe_mismatch(index, sent_bytes, encoded_bytes));
            break;
        }
    }

    if failure.is_none() {
        for (stream, reader) in [
            ("PNG sequence", &mut png_stdout),
            ("FFV1 video", &mut video_stdout),
        ] {
            let mut extra = [0u8; 1];
            match reader.read(&mut extra) {
                Ok(0) => {}
                Ok(_) => {
                    failure = Some(Error::new(
                        EXIT_VALIDATION,
                        format!("{stream} produced more than {count} frames"),
                    ));
                    break;
                }
                Err(err) => {
                    failure = Some(Error::new(
                        EXIT_VALIDATION,
                        format!("cannot finish reading {stream} decoder output: {err}"),
                    ));
                    break;
                }
            }
        }
    }

    drop(png_stdout);
    drop(video_stdout);
    if failure.is_some() {
        kill_and_wait(&mut png_child);
        kill_and_wait(&mut video_child);
    }
    let png_status = png_child.wait();
    let video_status = video_child.wait();
    let elapsed = start.elapsed();

    let mut summary = format!(
        "command_png: {png_command}\ncommand_video: {video_command}\nframes: {}\nframe_bytes: {frame_bytes}\n",
        count
    );
    summary.push_str(&format!(
        "png_exit: {}\nvideo_exit: {}\n",
        match &png_status {
            Ok(status) => proc::exit_description(status),
            Err(err) => format!("wait failed: {err}"),
        },
        match &video_status {
            Ok(status) => proc::exit_description(status),
            Err(err) => format!("wait failed: {err}"),
        }
    ));
    let result_line = match &failure {
        Some(err) => format!("result: FAIL ({})\n", err.message),
        None => "result: PASS (byte-identical rgb48le frames)\n".to_string(),
    };
    summary.push_str(&result_line);
    write_log(log_path, &summary)?;

    if let Some(err) = failure {
        return Err(err);
    }
    match png_status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            return Err(Error::new(
                EXIT_VALIDATION,
                format!(
                    "PNG sequence decoder failed: {}; transcript {}",
                    proc::exit_description(&status),
                    log_path.display()
                ),
            ))
        }
        Err(err) => {
            return Err(Error::new(
                EXIT_VALIDATION,
                format!("cannot wait for PNG sequence decoder: {err}"),
            ))
        }
    }
    match video_status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            return Err(Error::new(
                EXIT_VALIDATION,
                format!(
                    "FFV1 decoder failed: {}; transcript {}",
                    proc::exit_description(&status),
                    log_path.display()
                ),
            ))
        }
        Err(err) => {
            return Err(Error::new(
                EXIT_VALIDATION,
                format!("cannot wait for FFV1 decoder: {err}"),
            ))
        }
    }
    Ok(RoundtripReport {
        frames_compared: count,
        frame_bytes,
        elapsed,
    })
}

fn describe_mismatch(index: usize, sent: &[u8], encoded: &[u8]) -> Error {
    let diff = sent
        .iter()
        .zip(encoded.iter())
        .position(|(left, right)| left != right)
        .unwrap_or(0);
    let sample = diff / 2;
    let channel = sample % 3;
    let pixel = sample / 3;
    let sent_value = if diff + 1 < sent.len() {
        u16::from_le_bytes([sent[diff], sent[diff + 1]])
    } else {
        0
    };
    let encoded_value = if diff + 1 < encoded.len() {
        u16::from_le_bytes([encoded[diff], encoded[diff + 1]])
    } else {
        0
    };
    Error::new(
        EXIT_VALIDATION,
        format!(
            "lossless roundtrip mismatch at frame {} (0-based {index}), byte {}: \
             sent pixel {} channel {} = {sent_value} but decoded video = {encoded_value}",
            index + 1,
            diff,
            pixel,
            channel
        ),
    )
}

fn read_exact_frame(
    reader: &mut impl Read,
    buffer: &mut [u8],
    stream: &str,
    index: usize,
    count: usize,
) -> Result<()> {
    reader.read_exact(buffer).map_err(|err| {
        Error::new(
            EXIT_VALIDATION,
            format!(
                "cannot read frame {}/{count} from {stream}: {err}",
                index + 1
            ),
        )
    })
}

fn spawn_stdout(program: &Path, args: &[String], code: i32) -> Result<Child> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|err| {
            Error::new(
                code,
                format!(
                    "failed to start {} {}: {err}",
                    program.display(),
                    args.join(" ")
                ),
            )
        })
}

fn kill_and_wait(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Publish both validated deliverables. The temporary names are taken from the
/// already validated `VIDEO_ARTIFACT` handoffs; the final names are unchanged.
pub fn publish(
    temp_linear: &DataEnvelope,
    final_linear: &Path,
    temp_preview: &DataEnvelope,
    final_preview: &Path,
) -> Result<()> {
    let temp_linear = video_artifact_path(temp_linear).ok_or_else(|| {
        Error::new(
            EXIT_PUBLISH,
            "linear video handoff is not a validated video artifact",
        )
    })?;
    let temp_preview = video_artifact_path(temp_preview).ok_or_else(|| {
        Error::new(
            EXIT_PUBLISH,
            "preview video handoff is not a validated video artifact",
        )
    })?;
    for final_path in [final_linear, final_preview] {
        if final_path.exists() {
            return Err(Error::new(
                EXIT_PUBLISH,
                format!(
                    "refusing to overwrite existing deliverable {}",
                    final_path.display()
                ),
            ));
        }
    }
    if let Err(err) = fs::rename(temp_linear, final_linear) {
        return Err(Error::new(
            EXIT_PUBLISH,
            format!(
                "cannot publish {} -> {}: {err}; validated video remains at {}",
                temp_linear.display(),
                final_linear.display(),
                temp_linear.display()
            ),
        ));
    }
    if let Err(err) = fs::rename(temp_preview, final_preview) {
        let rollback = fs::rename(final_linear, temp_linear);
        return Err(Error::new(
            EXIT_PUBLISH,
            format!(
                "cannot publish preview {} -> {}: {err}; linear video rollback: {rollback:?}; \
                 no final deliverables were added",
                temp_preview.display(),
                final_preview.display()
            ),
        ));
    }
    Ok(())
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

fn write_log(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text).map_err(|err| {
        Error::new(
            EXIT_VALIDATION,
            format!("cannot write transcript {}: {err}", path.display()),
        )
    })
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
    fn linear_probe_validation_rejects_wrong_pixel_format() {
        let probe = ProbeInfo {
            codec_name: "ffv1".to_string(),
            pix_fmt: "yuv420p".to_string(),
            width: 8,
            height: 6,
            frames: 3,
            avg_frame_rate: "30/1".to_string(),
            color_space: "unknown".to_string(),
            color_primaries: "unknown".to_string(),
            color_transfer: "unknown".to_string(),
            color_range: "unknown".to_string(),
        };
        let error =
            validate_linear(&probe, 8, 6, 3, Fps { num: 30, den: 1 }).expect_err("must reject");
        assert!(error.message.contains("gbrp16le"), "{error}");
    }

    #[test]
    fn linear_probe_validation_accepts_matching_stream() {
        let probe = ProbeInfo {
            codec_name: "ffv1".to_string(),
            pix_fmt: "gbrp16le".to_string(),
            width: 8,
            height: 6,
            frames: 3,
            avg_frame_rate: "30/1".to_string(),
            color_space: "unknown".to_string(),
            color_primaries: "unknown".to_string(),
            color_transfer: "unknown".to_string(),
            color_range: "unknown".to_string(),
        };
        assert!(validate_linear(&probe, 8, 6, 3, Fps { num: 30, den: 1 }).is_ok());
    }

    #[test]
    fn mismatch_description_points_at_the_first_difference() {
        let sent = [1u8, 0, 2, 0, 3, 0];
        let encoded = [1u8, 0, 2, 0, 9, 0];
        let error = describe_mismatch(2, &sent, &encoded);
        assert!(error.message.contains("frame 3"));
        assert!(error.message.contains("byte 4"));
        assert!(error.message.contains("3 but decoded video = 9"));
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

    #[cfg(unix)]
    #[test]
    fn render_preview_converts_through_validated_typed_frames() {
        use crate::color_profile::lookup;
        use std::os::unix::fs::PermissionsExt;

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(format!("preview-typed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");
        let raw = dir.join("preview.rgb");
        let log = dir.join("render.log");
        let count = 2usize;
        let frame_bytes = 8 * 6 * 6;
        let script = dir.join("fake-ffmpeg.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\ni=0\nwhile [ $i -lt {count} ]; do head -c {frame_bytes} /dev/zero; i=$((i+1)); done\nexit 0\n"
            ),
        )
        .expect("write fake decoder");
        let mut permissions = fs::metadata(&script).expect("stat").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("chmod");

        let profile = lookup("srgb-preview").expect("srgb");
        render_preview(
            &script,
            &dir,
            8,
            6,
            count,
            Fps { num: 30, den: 1 },
            &raw,
            &log,
            profile,
        )
        .expect("render preview");

        let mut expected = Vec::new();
        profile
            .convert_frame(&vec![0u8; frame_bytes], 8, 6, &mut expected)
            .expect("reference conversion");
        let actual = fs::read(&raw).expect("read raw preview");
        assert_eq!(actual.len(), expected.len() * count);
        for frame in actual.chunks_exact(expected.len()) {
            assert_eq!(frame, expected.as_slice());
        }
    }
}
