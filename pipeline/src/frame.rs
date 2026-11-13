//! Streaming native protocol v1/v2 consumption. JSON crosses validated MemoryFile
//! envelopes; pixels are read one frame at a time and never written to disk.
use crate::cli::WhiteBalance;
use crate::error::{Error, Result, EXIT_FRAME};
use crate::json::{self, Json};
use crate::order::SourceFrame;
use crate::proc::{self, ActiveNative, OwnedProcess};
use crate::transport::{
    self, DataEnvelope, MemoryFile, MetadataDoc, Payload, Provenance, ANALYSIS_SIDECAR,
    FRAME_METADATA,
};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const WB_SOURCE_AS_SHOT: &str = "as-shot";
pub const WB_SOURCE_OVERRIDE: &str = "override";
const JSON_MEDIA_TYPE: &str = "application/json; charset=utf-8";

#[derive(Debug, Clone)]
pub struct FrameInfo {
    pub index: usize,
    pub source_name: String,
    pub camera_color: Option<Json>,
    pub signal_clip_counts: Option<[usize; 2]>,
    pub source_sha256: Option<String>,
    pub pixels_sha256: Option<String>,
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
    pub log_relative: String,
    pub duration: Duration,
    pub metadata_envelope: DataEnvelope,
    pub analysis_envelope: Option<DataEnvelope>,
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
pub struct ProduceConfig<'a> {
    pub camera_color_method: crate::color_profile::CameraColorMethod,
    pub logs_dir: &'a Path,
    pub exposure_ev: Option<f64>,
    pub analysis_roi: Option<[f64; 4]>,
    pub echo: bool,
    pub echo_tag: &'a str,
    pub white_balance: WhiteBalance,
    pub converter: Option<&'a str>,
    pub converter_timeout: Option<u64>,
    pub log_relative_dir: &'a str,
}

fn failure(message: impl Into<String>) -> Error {
    Error::new(EXIT_FRAME, message)
}

/// Exact, ordered record reader. Lengths are checked before allocation. Pixel
/// records use the geometry-derived exact length, JSON has a bounded budget.
pub struct Protocol<R> {
    reader: R,
}
impl<R: Read> Protocol<R> {
    #[cfg(test)]
    pub fn new(reader: R) -> Result<Self> {
        Self::with_version(reader, false)
    }
    pub fn with_version(mut reader: R, f32: bool) -> Result<Self> {
        let mut magic = [0; 8];
        reader
            .read_exact(&mut magic)
            .map_err(|e| failure(format!("short stdout magic: {e}")))?;
        if &magic != if f32 { b"RAWTLST2" } else { b"RAWTLST1" } {
            return Err(failure("invalid stdout magic"));
        }
        Ok(Self { reader })
    }
    pub fn record(&mut self, expected: u8, exact: Option<usize>) -> Result<Arc<[u8]>> {
        let mut header = [0; 12];
        self.reader
            .read_exact(&mut header)
            .map_err(|e| failure(format!("short record header: {e}")))?;
        if header[0] != expected || header[1..4] != [0; 3] {
            return Err(failure(format!(
                "invalid stdout record type/reserved: expected {expected:#04x}"
            )));
        }
        let length = u64::from_le_bytes(header[4..12].try_into().unwrap());
        let length = usize::try_from(length).map_err(|_| failure("record length overflow"))?;
        if let Some(exact) = exact {
            if length != exact {
                return Err(failure(format!(
                    "pixel record length {length} != geometry byte count {exact}"
                )));
            }
        } else if length > 16 * 1024 * 1024 {
            return Err(failure("JSON record exceeds 16 MiB limit"));
        }
        // Allocate the final NdArray backing directly, avoiding a Vec -> Arc
        // copy of a full FP32 frame. Every u8 bit pattern is valid.
        let mut bytes = unsafe { Arc::<[u8]>::new_zeroed_slice(length).assume_init() };
        let mut received = 0;
        while received < length {
            match self
                .reader
                .read(&mut Arc::get_mut(&mut bytes).unwrap()[received..])
            {
                Ok(0) => {
                    return Err(failure(format!(
                        "short record payload: {received} of {length} bytes"
                    )))
                }
                Ok(n) => received += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return Err(failure(format!(
                        "record payload read failed after {received} of {length} bytes: {e}"
                    )))
                }
            }
        }
        Ok(bytes)
    }
    pub fn end(&mut self) -> Result<()> {
        let mut extra = [0];
        match self.reader.read(&mut extra) {
            Ok(0) => Ok(()),
            Ok(_) => Err(failure("trailing stdout records/bytes")),
            Err(e) => Err(failure(format!("cannot read stdout end: {e}"))),
        }
    }
}

pub struct NativeStream {
    process: OwnedProcess,
    protocol: Option<Protocol<std::process::ChildStdout>>,
    pixel_bytes: usize,
    f32: bool,
    expected_digest: Option<String>,
    active: ActiveNative,
}
impl NativeStream {
    fn open(tool: &Path, args: &[String], log: &Path, active: ActiveNative) -> Result<Self> {
        let mut registration = active.lock().unwrap();
        if registration.cancelled {
            return Err(Error::new(
                crate::error::EXIT_PREVIEW,
                "encoder failed; native frame dispatch cancelled",
            ));
        }
        let process = OwnedProcess::spawn(tool, args, log, EXIT_FRAME, false)?;
        registration.child = Some(process.child.clone());
        drop(registration);
        let stdout = process.child.lock().unwrap().stdout.take().unwrap();
        let mut stream = Self {
            process,
            protocol: None,
            pixel_bytes: 0,
            expected_digest: None,
            f32: args.first().is_some_and(|a| a == "--stdout-frame-f32"),
            active,
        };
        stream.protocol = Some(Protocol::with_version(stdout, stream.f32)?);
        Ok(stream)
    }
    pub fn pixels(&mut self, width: u32, height: u32) -> Result<Arc<[u8]>> {
        let length = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(if self.f32 { 12 } else { 6 }))
            .ok_or_else(|| failure("pixel geometry overflow"))?;
        self.process
            .note(&format!("pixel bytes expected: {length}"));
        let pixels = self
            .protocol
            .as_mut()
            .unwrap()
            .record(4, Some(length))
            .map_err(|e| {
                self.process
                    .note(&format!("pixel record failed: {}", e.message));
                e
            })?;
        if self.f32
            && pixels
                .chunks_exact(4)
                .any(|b| !f32::from_le_bytes(b.try_into().unwrap()).is_finite())
        {
            return Err(failure("nonfinite FP32 pixel"));
        }
        if let Some(expected) = &self.expected_digest {
            if crate::digest::sha256(&pixels) != expected.to_ascii_lowercase() {
                return Err(failure("FP32 pixels_sha256 mismatch"));
            }
        }
        self.pixel_bytes = pixels.len();
        self.process
            .note(&format!("pixel bytes: {}", self.pixel_bytes));
        // A complete payload is insufficient: reject trailing bytes and a late
        // nonzero native exit before an encoder write can block on backpressure.
        self.finish()?;
        Ok(pixels)
    }
    fn json(&mut self, kind: u8) -> Result<Arc<[u8]>> {
        let bytes = self.protocol.as_mut().unwrap().record(kind, None)?;
        let text =
            std::str::from_utf8(&bytes).map_err(|e| failure(format!("invalid UTF-8 JSON: {e}")))?;
        // JSON summaries only: pixel payloads never enter transcripts.
        self.process.note(&format!("JSON {kind:#04x}: {text}"));
        Ok(bytes)
    }
    fn finish(&mut self) -> Result<()> {
        if self.protocol.is_none() {
            return Ok(());
        }
        if self.pixel_bytes == 0 {
            self.process.note("pixel bytes: 0 (analysis only)");
        }
        self.protocol.as_mut().unwrap().end()?;
        self.protocol.take();
        let status = self.process.finish(false)?;
        self.active.lock().unwrap().child = None;
        if !status.success() {
            return Err(failure(format!(
                "native frame failed: {}",
                proc::exit_description(&status)
            )));
        }
        Ok(())
    }
}
impl Drop for NativeStream {
    fn drop(&mut self) {
        self.protocol.take();
        // Reap before unregistering, including protocol/conversion/write failures.
        // OwnedProcess records the real exit status on drop.
        self.process.abort_if_running();
        self.active.lock().unwrap().child = None;
    }
}

pub fn produce(
    tool: &Path,
    sources: &[SourceFrame],
    config: &ProduceConfig<'_>,
) -> Result<FrameSequence> {
    produce_inner(
        tool,
        sources,
        None,
        config,
        false,
        Default::default(),
        &mut |_, _| Ok(()),
    )
}
pub fn produce_with_evs(
    tool: &Path,
    sources: &[SourceFrame],
    evs: &[f64],
    config: &ProduceConfig<'_>,
) -> Result<FrameSequence> {
    produce_inner(
        tool,
        sources,
        Some(evs),
        config,
        false,
        Default::default(),
        &mut |_, _| Ok(()),
    )
}
pub fn stream_frames(
    tool: &Path,
    sources: &[SourceFrame],
    evs: &[f64],
    config: &ProduceConfig<'_>,
    active: ActiveNative,
    consume: &mut impl FnMut(&FrameInfo, &mut NativeStream) -> Result<()>,
) -> Result<FrameSequence> {
    produce_inner(tool, sources, Some(evs), config, true, active, consume)
}
fn produce_inner(
    tool: &Path,
    sources: &[SourceFrame],
    evs: Option<&[f64]>,
    config: &ProduceConfig<'_>,
    pixels: bool,
    active: ActiveNative,
    consume: &mut impl FnMut(&FrameInfo, &mut NativeStream) -> Result<()>,
) -> Result<FrameSequence> {
    if let Some(evs) = evs {
        if evs.len() != sources.len() || evs.iter().any(|v| !v.is_finite()) {
            return Err(failure("invalid per-frame EV vector"));
        }
    }
    let started = Instant::now();
    let mut frames: Vec<FrameInfo> = Vec::with_capacity(sources.len());
    for (offset, source) in sources.iter().enumerate() {
        let reference = frames
            .first()
            .map(|f| f.applied_wb)
            .filter(|_| config.white_balance.freezes_reference());
        let ev = evs.map(|e| e[offset]).or(config.exposure_ev);
        let info = run_frame(
            tool,
            source,
            offset + 1,
            sources.len(),
            reference,
            ev,
            config,
            pixels,
            active.clone(),
            &mut |info, stream| {
                if let Some(first) = frames.first() {
                    if config.camera_color_method.requires_canon_cr3()
                        && info
                            .camera_color
                            .as_ref()
                            .and_then(|c| c.get("camera_model"))
                            != first
                                .camera_color
                                .as_ref()
                                .and_then(|c| c.get("camera_model"))
                    {
                        return Err(failure("rev-eng Canon Log rejects mixed camera models"));
                    }
                    if info.width != first.width || info.height != first.height {
                        return Err(failure(format!(
                            "frame {} ({}) has dimensions {}x{} but the sequence is {}x{}",
                            info.index,
                            info.source_name,
                            info.width,
                            info.height,
                            first.width,
                            first.height
                        )));
                    }
                    if config.white_balance.freezes_reference()
                        && !wb_close(info.applied_wb, first.applied_wb)
                    {
                        return Err(failure(
                            "applied white balance differs from the frozen reference",
                        ));
                    }
                }
                consume(info, stream)
            },
        )
        .map_err(|e| {
            let log = config.logs_dir.join(format!("frame_{:06}.log", offset + 1));
            let detail = fs::read_to_string(&log).unwrap_or_default();
            Error::new(
                e.code,
                format!(
                    "frame {}/{} ({}) failed: {}; transcript {}: {}; no video was produced",
                    offset + 1,
                    sources.len(),
                    source.name,
                    e.message,
                    log.display(),
                    proc::tail(&detail, 1200)
                ),
            )
        })?;
        if config.echo {
            println!(
                "{}{}/{} {} (WB {}, EV {:+.3})",
                config.echo_tag,
                info.index,
                sources.len(),
                info.source_name,
                info.wb_source,
                info.exposure_ev
            );
        }
        frames.push(info);
    }
    let first = frames
        .first()
        .ok_or_else(|| failure("empty frame sequence"))?;
    Ok(FrameSequence {
        width: first.width,
        height: first.height,
        reference_wb: first.applied_wb,
        reference_frame: first.source_name.clone(),
        frames,
        total_duration: started.elapsed(),
    })
}
pub fn produce_single(
    tool: &Path,
    source: &SourceFrame,
    override_wb: Option<[f64; 3]>,
    index: usize,
    config: &ProduceConfig<'_>,
) -> Result<FrameInfo> {
    run_frame(
        tool,
        source,
        index,
        1,
        override_wb,
        config.exposure_ev,
        config,
        false,
        Default::default(),
        &mut |_, _| Ok(()),
    )
}
fn run_frame(
    tool: &Path,
    source: &SourceFrame,
    index: usize,
    total: usize,
    override_wb: Option<[f64; 3]>,
    exposure_ev: Option<f64>,
    config: &ProduceConfig<'_>,
    pixels: bool,
    active: ActiveNative,
    consume: &mut impl FnMut(&FrameInfo, &mut NativeStream) -> Result<()>,
) -> Result<FrameInfo> {
    let started = Instant::now();
    let mut args = vec![
        if pixels {
            "--stdout-frame-f32"
        } else {
            "--stdout-analysis"
        }
        .to_string(),
        source.path.display().to_string(),
    ];
    if config.camera_color_method.requires_canon_cr3() {
        if source
            .path
            .extension()
            .and_then(|e| e.to_str())
            .is_none_or(|e| !e.eq_ignore_ascii_case("cr3"))
        {
            return Err(failure("rev-eng Canon Log requires Canon CR3 photos"));
        }
    }
    args.extend([
        "--camera-color-method".into(),
        config.camera_color_method.native_token().into(),
    ]);
    if let Some(wb) = override_wb {
        args.extend(["--white-balance".into(), format_wb(wb)]);
    }
    if let Some(ev) = exposure_ev {
        args.extend(["--exposure-ev".into(), format_ev(ev)]);
    }
    if let Some(path) = config.converter {
        args.extend(["--converter".into(), path.into()]);
    }
    if let Some(seconds) = config.converter_timeout {
        args.extend(["--converter-timeout".into(), seconds.to_string()]);
    }
    if !pixels {
        if let Some(roi) = config.analysis_roi {
            args.extend([
                "--analysis-roi".into(),
                roi.iter()
                    .map(|v| format_axis(*v))
                    .collect::<Vec<_>>()
                    .join(","),
            ]);
        }
    }
    let mut stream = NativeStream::open(
        tool,
        &args,
        &config.logs_dir.join(format!("frame_{index:06}.log")),
        active,
    )?;
    let bytes = stream.json(1)?;
    let sidecar = parse_json(&bytes)?;
    {
        let color = sidecar
            .get("color")
            .ok_or_else(|| failure("missing camera color contract"))?;
        let effective = color.get("method_effective").and_then(Json::as_str);
        let valid_matrix = |key| {
            color.get(key).and_then(Json::as_array).is_some_and(|rows| {
                rows.len() == 3
                    && rows.iter().all(|r| {
                        r.as_array().is_some_and(|r| {
                            r.len() == 3 && r.iter().all(|x| x.as_f64().is_some_and(f64::is_finite))
                        })
                    })
            })
        };
        if !valid_matrix("camera_to_xyz_d50")
            || !valid_matrix("camera_to_acescg")
            || color
                .get("profile_fingerprint")
                .and_then(Json::as_str)
                .is_none_or(str::is_empty)
        {
            return Err(failure("missing/invalid camera matrix/profile fingerprint"));
        }

        if config.camera_color_method.requires_canon_cr3()
            && (color.get("algorithm_revision").and_then(Json::as_u64) != Some(3)
                || !valid_fixed_calibration(color)
                || !matches!(
                    (
                        color.get("hue_sat_map_present"),
                        color.get("hue_sat_map_applied")
                    ),
                    (Some(Json::Bool(true)), Some(Json::Bool(true)))
                        | (Some(Json::Bool(false)), Some(Json::Bool(false)))
                ))
        {
            return Err(failure(
                "incomplete camera profile calibration; rebuild matched native/Rust tools",
            ));
        }
        if sidecar.get("color_contract_version").and_then(Json::as_u64) != Some(1)
            || color.get("method_requested").and_then(Json::as_str)
                != Some(config.camera_color_method.native_token())
            || !(if config.camera_color_method.requires_canon_cr3() {
                matches!(
                    effective,
                    Some("dng-camera-to-pcs-forward-matrix" | "dng-camera-to-pcs-inverse-fallback")
                )
            } else {
                effective == Some("legacy-inverse-cm")
            })
            || (config.camera_color_method.requires_canon_cr3()
                && !color
                    .get("camera_model")
                    .and_then(Json::as_str)
                    .is_some_and(|s| s.starts_with("Canon ")))
        {
            return Err(failure("invalid camera color contract"));
        }
    }
    let file =
        MemoryFile::new(bytes.into(), JSON_MEDIA_TYPE).map_err(|e| failure(e.to_string()))?;
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

    if sidecar
        .get("source_sha256")
        .and_then(Json::as_str)
        .is_some_and(|s| !s.is_empty())
        && !sidecar
            .get("source_sha256")
            .and_then(Json::as_str)
            .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(failure("invalid source_sha256 for FP32 frame"));
    }
    let source_sha256 = Some(
        sidecar
            .get("source_sha256")
            .and_then(Json::as_str)
            .filter(|hash| !hash.is_empty())
            .ok_or_else(|| missing_field(index, total, &source.name, "source_sha256"))?
            .to_string(),
    );
    let metadata_envelope = frame_metadata_envelope(
        file,
        schema_version,
        frame_width,
        frame_height,
        source_sha256.as_deref(),
        index,
        total,
        &source.name,
    )?;
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

    if sidecar.get("analysis_sidecar") != Some(&Json::Null) {
        return Err(failure("stdout metadata analysis_sidecar must be null"));
    }
    let mut info = FrameInfo {
        index,
        source_name: source.name.clone(),
        camera_color: sidecar.get("color").cloned(),
        signal_clip_counts: None,
        source_sha256,
        pixels_sha256: None,
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
        log_relative: format!("{}/frame_{index:06}.log", config.log_relative_dir),
        duration: Duration::ZERO,
        metadata_envelope,
        analysis_envelope: None,
    };
    if pixels {
        let descriptor = parse_json(&stream.json(3)?)?;
        if sidecar.get("linear_pixel_format").and_then(Json::as_str) != Some("rgbf32le") {
            return Err(failure("metadata linear_pixel_format must be rgbf32le"));
        }
        if descriptor.get("width").and_then(Json::as_u32) != Some(info.width)
            || descriptor.get("height").and_then(Json::as_u32) != Some(info.height)
            || descriptor.get("pixel_format").and_then(Json::as_str) != Some("rgbf32le")
        {
            return Err(failure("frame descriptor geometry/pixel_format mismatch"));
        }
        let digest = descriptor
            .get("pixels_sha256")
            .and_then(Json::as_str)
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| failure("invalid pixels_sha256"))?;
        info.pixels_sha256 = Some(digest.into());
        {
            let expected_bytes = u64::from(info.width)
                .checked_mul(u64::from(info.height))
                .and_then(|n| n.checked_mul(12))
                .ok_or_else(|| failure("FP32 geometry overflow"))?;
            if descriptor.get("space").and_then(Json::as_str) != Some("AP1/D60")
                || descriptor.get("transfer").and_then(Json::as_str) != Some("linear")
                || descriptor
                    .get("color_contract_version")
                    .and_then(Json::as_u64)
                    != Some(1)
                || descriptor.get("byte_length").and_then(Json::as_u64) != Some(expected_bytes)
                || expected_bytes > 2_400_000_000
            {
                return Err(failure("invalid FP32 descriptor contract/allocation bound"));
            }
            stream.expected_digest = Some(digest.into());
        }
    } else {
        let bytes = stream.json(2)?;
        let envelope =
            analysis_sidecar_envelope(bytes, info.source_sha256.as_deref(), info.exposure_ev)?;
        info.analysis_envelope = Some(envelope);
    }
    consume(&info, &mut stream)?;
    stream.finish()?;
    info.duration = started.elapsed();
    Ok(info)
}
fn parse_json(bytes: &[u8]) -> Result<Json> {
    let text =
        std::str::from_utf8(bytes).map_err(|e| failure(format!("invalid JSON UTF-8: {e}")))?;
    json::parse(text).map_err(|e| failure(format!("invalid JSON: {e}")))
}
pub fn analysis_sidecar_envelope(
    bytes: Arc<[u8]>,
    expected: Option<&str>,
    ev: f64,
) -> Result<DataEnvelope> {
    let sidecar = parse_json(&bytes)?;
    if sidecar.get("schema_version").and_then(Json::as_u64) != Some(2)
        || sidecar.get("kind").and_then(Json::as_str) != Some("rawtl-analysis-sidecar")
    {
        return Err(failure("analysis sidecar has an unsupported schema"));
    }
    let hash = sidecar
        .get("source_sha256")
        .and_then(Json::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            failure("analysis sidecar source_sha256 does not match the frame metadata")
        })?;
    if Some(hash) != expected {
        return Err(failure(
            "analysis sidecar source_sha256 does not match the frame metadata",
        ));
    }
    let reported = sidecar
        .get("analysis")
        .and_then(|j| j.get("exposure_ev"))
        .and_then(Json::as_f64);
    if !reported.is_some_and(|v| sidecar_ev_matches(v, ev)) {
        return Err(failure("analysis sidecar exposure_ev mismatch"));
    }
    let metadata = MetadataDoc::new(vec![
        ("source.schema_version".into(), Json::Int(2)),
        (
            "source.kind".into(),
            Json::Str("rawtl-analysis-sidecar".into()),
        ),
        ("source.sha256".into(), Json::Str(hash.into())),
    ])
    .map_err(|e| failure(e.to_string()))?;
    let provenance = Provenance::new(
        "dng-linear-frame",
        expected.map(str::to_string).into_iter().collect(),
        Vec::new(),
    )
    .map_err(|e| failure(e.to_string()))?;
    let file =
        MemoryFile::new(bytes.into(), JSON_MEDIA_TYPE).map_err(|e| failure(e.to_string()))?;
    DataEnvelope::new(
        ANALYSIS_SIDECAR,
        Payload::MemoryFile(file),
        metadata,
        provenance,
    )
    .map_err(|e| failure(e.to_string()))
}
pub fn analysis_bytes(frame: &FrameInfo) -> Option<&[u8]> {
    match frame.analysis_envelope.as_ref()?.payload() {
        Payload::MemoryFile(file) => Some(file.bytes()),
        _ => None,
    }
}
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
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    use std::io::Cursor;
    fn record(kind: u8, reserved: [u8; 3], length: u64, payload: &[u8]) -> Vec<u8> {
        let mut bytes = b"RAWTLST1".to_vec();
        bytes.push(kind);
        bytes.extend(reserved);
        bytes.extend(length.to_le_bytes());
        bytes.extend(payload);
        bytes
    }
    #[test]
    fn versions_are_selected_by_strategy_not_guessed_from_payload() {
        assert!(Protocol::with_version(Cursor::new(b"RAWTLST1"), true).is_err());
        assert!(Protocol::with_version(Cursor::new(b"RAWTLST2"), false).is_err());
        let mut p = Protocol::with_version(Cursor::new(b"RAWTLST2"), true).unwrap();
        p.end().unwrap();
    }
    #[test]
    fn rejects_wrong_order_reserved_lengths_truncation_and_trailing_data() {
        for (bytes, expected, exact) in [
            (record(3, [0; 3], 2, b"{}"), 1, None),
            (record(1, [0, 1, 0], 2, b"{}"), 1, None),
            (record(4, [0; 3], 12, &[0; 12]), 4, Some(6)),
            (record(4, [0; 3], 6, &[0; 5]), 4, Some(6)),
            (record(1, [0; 3], u64::MAX, b""), 1, None),
        ] {
            assert!(Protocol::new(Cursor::new(bytes))
                .unwrap()
                .record(expected, exact)
                .is_err());
        }
        let mut bytes = record(1, [0; 3], 2, b"{}");
        bytes.push(0);
        let mut parser = Protocol::new(Cursor::new(bytes)).unwrap();
        parser.record(1, None).unwrap();
        assert!(parser.end().is_err());
        let mut parser = Protocol::new(Cursor::new(record(4, [0; 3], 6, &[0; 6]))).unwrap();
        assert_eq!(&*parser.record(4, Some(6)).unwrap(), &[0; 6]);
        parser.end().unwrap();
    }
}

// Matched native/Rust contract: no silent fallback to sensor-relative exposure.
fn valid_fixed_calibration(color: &Json) -> bool {
    let Some(c) = color.get("exposure_calibration") else { return false };
    let number = |k| c.get(k).and_then(Json::as_f64).filter(|v| v.is_finite());
    let (Some(max), Some(black), Some(white), Some(dw), Some(db), Some(gain), Some(scale), Some(normal)) =
        (number("adc_maximum"), number("black"), number("normal_white"), number("dng_white"),
         number("dng_black"), number("native_scene_gain"), number("storage_scale"), number("normal_white_gain"))
        else { return false };
    let close = |a: f64,b: f64| (a-b).abs() <= 1e-9 * b.abs().max(1.0);
    c.get("applied") == Some(&Json::Bool(true))
        && c.get("method").and_then(Json::as_str) == Some("canon-normal-white-fixed-v1")
        && c.get("auto_tone_normalization") == Some(&Json::Bool(false))
        && c.get("baseline_exposure_applied") == Some(&Json::Bool(false))
        && number("scene_to_reflection") == Some(0.9)
        && number("fixed_scene_gain") == Some(1.8)
        && max >= 255.0 && max <= 65535.0 && (max + 1.0).fract() == 0.0
        && ((max as u64)+1).is_power_of_two()
        && black >= 0.0 && black < white && white <= max && white > max/2.0
        && dw == 65535.0 && db >= 0.0 && (db-black*scale).abs() <= scale+1e-6
        && close(scale, (dw+1.0)/(max+1.0))
        && close(normal, (dw-db)/(scale*(white-black))) && close(gain, 1.8*normal)
}
