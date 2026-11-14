//! Streaming native protocol v1/v2 consumption. JSON crosses validated MemoryFile
//! envelopes; pixels are read one frame at a time and never written to disk.
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
    pub source_sha256: Option<String>,
    pub pixels_sha256: Option<String>,
    pub width: u32,
    pub height: u32,
    pub applied_wb: [f64; 3],
    pub wb_source: String,
    pub as_shot_neutral: [f64; 3],
    pub exposure_ev: f64,
    pub duration: Duration,
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
/// Test-only caller of the retired source-path producer. The product graph
/// consumes `PreparedFrame` through `prepare`/`analyze_prepared`/`develop` only.
#[cfg(test)]
pub struct ProduceConfig<'a> {
    pub camera_color_method: crate::color_profile::CameraColorMethod,
    pub logs_dir: &'a Path,
    pub exposure_ev: Option<f64>,
    pub analysis_roi: Option<[f64; 4]>,
    pub echo: bool,
    pub echo_tag: &'a str,
    pub white_balance: crate::cli::WhiteBalance,
    pub converter: Option<&'a str>,
    pub converter_timeout: Option<u64>,
}

/// Versioned prepared-context header. The context is a strict line document
/// written by `dng-linear-frame --prepare`; both processes parse the same
/// bytes and the native consumer revalidates every hash before decoding.
#[allow(dead_code)] // consumed by the graph scheduling integration
pub const PREPARED_CONTEXT_MAGIC: &str = "rawtl-prepared-frame/1";

/// Routing identity carried by a prepared context. `Direct` is a LinearRaw
/// source used as-is; the other two routes have a Converter-produced DNG.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // consumed by the graph scheduling integration
pub enum PreparedRoute {
    Direct,
    Converter,
    VendorRaw,
}
impl PreparedRoute {
    pub fn token(self) -> &'static str {
        match self {
            Self::Direct => "linearraw-sdk",
            Self::Converter => "cfa-converter-linearraw-sdk",
            Self::VendorRaw => "vendor-raw-converter-linearraw-sdk",
        }
    }
    fn from_token(text: &str) -> Option<Self> {
        match text {
            "linearraw-sdk" => Some(Self::Direct),
            "cfa-converter-linearraw-sdk" => Some(Self::Converter),
            "vendor-raw-converter-linearraw-sdk" => Some(Self::VendorRaw),
            _ => None,
        }
    }
    fn generates(self) -> bool {
        self != Self::Direct
    }
}

/// One validated prepared frame. `source` is always the original photograph;
/// `dng` is the LinearRaw artifact the SDK decodes and is never used as the
/// identity for Canon calibration, white balance or capture metadata.
#[derive(Debug, Clone)]
#[allow(dead_code)] // consumed by the graph scheduling integration
pub struct PreparedFrame {
    pub context: PathBuf,
    pub source: PathBuf,
    pub source_name: String,
    pub source_sha256: String,
    pub route: PreparedRoute,
    pub dng: PathBuf,
    pub dng_sha256: String,
    pub generated: bool,
    pub main_ifd_width: u32,
    pub main_ifd_height: u32,
    pub as_shot_neutral: [f64; 3],
    pub capture_time: Option<String>,
}

#[allow(dead_code)] // consumed by the graph scheduling integration
impl PreparedFrame {
    /// Parse and structurally validate a context document. Hash *binding* is
    /// revalidated by the native consumer against the actual files; this side
    /// checks the versioned grammar, absolute identities, route/generated
    /// consistency and file existence so a mis-wired graph fails before a
    /// native process is started.
    pub fn load(context: &Path) -> Result<Self> {
        let bytes = fs::read(context)
            .map_err(|e| failure(format!("cannot read prepared context {}: {e}", context.display())))?;
        let fields = parse_context_fields(&bytes)?;
        let get = |key: &str| -> Result<&str> {
            fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
                .ok_or_else(|| failure(format!("prepared context is missing field {key}")))
        };
        let source_sha256 = hex64(get("source_sha256")?, "source_sha256")?;
        let dng_sha256 = hex64(get("prepared_sha256")?, "prepared_sha256")?;
        let route_text = get("route")?;
        let route = PreparedRoute::from_token(route_text).ok_or_else(|| {
            failure(format!("prepared context has an unknown route {route_text:?}"))
        })?;
        let generated = parse_flag(get("generated")?, "generated")?;
        let main_ifd_width = parse_u32(get("main_ifd_width")?, "main_ifd_width")?;
        let main_ifd_height = parse_u32(get("main_ifd_height")?, "main_ifd_height")?;
        if main_ifd_width == 0 || main_ifd_height == 0 {
            return Err(failure("prepared context geometry must be non-zero"));
        }
        let as_shot_neutral = parse_neutral(get("as_shot_neutral")?)?;
        let has_capture = parse_flag(get("has_capture_time")?, "has_capture_time")?;
        let capture_time = if has_capture {
            let value = get("capture_time")?;
            if value.is_empty() {
                return Err(failure("prepared context capture_time must be non-empty"));
            }
            Some(value.to_string())
        } else {
            if fields.iter().any(|(name, _)| name == "capture_time") {
                return Err(failure(
                    "prepared context carries capture_time with has_capture_time=0",
                ));
            }
            None
        };
        if generated != route.generates() {
            return Err(failure("prepared context generated flag does not match its route"));
        }
        let source_raw = PathBuf::from(get("source")?);
        let dng_raw = PathBuf::from(get("prepared_dng")?);
        if !source_raw.is_absolute() || !dng_raw.is_absolute() {
            return Err(failure("prepared context paths must be absolute"));
        }
        let source = fs::canonicalize(&source_raw).map_err(|e| {
            failure(format!(
                "prepared context original source cannot be resolved: {}: {e}",
                source_raw.display()
            ))
        })?;
        let dng = fs::canonicalize(&dng_raw).map_err(|e| {
            failure(format!(
                "prepared context prepared DNG cannot be resolved: {}: {e}",
                dng_raw.display()
            ))
        })?;
        if route == PreparedRoute::Direct {
            if dng != source {
                return Err(failure(
                    "prepared context for a LinearRaw source must reference the original DNG",
                ));
            }
            if dng_sha256 != source_sha256 {
                return Err(failure(
                    "prepared context prepared hash differs from the original source hash",
                ));
            }
        } else if dng == source {
            return Err(failure(
                "prepared context cannot use the original source as its converted DNG",
            ));
        }
        let source_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| failure("prepared source path has no UTF-8 file name"))?
            .to_string();
        Ok(Self {
            context: context.to_path_buf(),
            source,
            source_name,
            source_sha256,
            route,
            dng,
            dng_sha256,
            generated,
            main_ifd_width,
            main_ifd_height,
            as_shot_neutral,
            capture_time,
        })
    }
}

#[allow(dead_code)] // consumed by the graph scheduling integration
fn parse_context_fields(bytes: &[u8]) -> Result<Vec<(String, String)>> {
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(failure("prepared context exceeds the size limit"));
    }
    let mut lines = bytes.split(|byte| *byte == b'\n');
    let magic = lines
        .next()
        .ok_or_else(|| failure("prepared context is empty"))?;
    if magic != PREPARED_CONTEXT_MAGIC.as_bytes() {
        return Err(failure("prepared context has an unsupported header"));
    }
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line.ends_with(b"\r") {
            return Err(failure("prepared context must use LF line endings"));
        }
        let equals = line
            .iter()
            .position(|byte| *byte == b'=')
            .filter(|index| *index > 0)
            .ok_or_else(|| failure("prepared context field is malformed"))?;
        let key = std::str::from_utf8(&line[..equals])
            .map_err(|_| failure("prepared context field name is not UTF-8"))?;
        if !key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(failure("prepared context field name is malformed"));
        }
        if fields.iter().any(|(name, _)| name == key) {
            return Err(failure(format!("prepared context repeats field {key}")));
        }
        let value = percent_decode(&line[equals + 1..], key)?;
        fields.push((key.to_string(), value));
    }
    Ok(fields)
}

#[allow(dead_code)] // consumed by the graph scheduling integration
fn percent_decode(value: &[u8], field: &str) -> Result<String> {
    fn digit(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        let byte = value[index];
        if byte == b'%' {
            if index + 2 >= value.len() {
                return Err(failure(format!(
                    "prepared context field {field} has a truncated escape"
                )));
            }
            let hi = digit(value[index + 1]);
            let lo = digit(value[index + 2]);
            match (hi, lo) {
                (Some(hi), Some(lo)) => out.push((hi << 4) | lo),
                _ => {
                    return Err(failure(format!(
                        "prepared context field {field} has an invalid escape"
                    )))
                }
            }
            index += 3;
        } else if byte < 0x20 || byte == 0x7f {
            return Err(failure(format!(
                "prepared context field {field} contains a raw control byte"
            )));
        } else {
            out.push(byte);
            index += 1;
        }
    }
    String::from_utf8(out)
        .map_err(|_| failure(format!("prepared context field {field} is not UTF-8")))
}

#[allow(dead_code)] // consumed by the graph scheduling integration
fn parse_flag(text: &str, field: &str) -> Result<bool> {
    match text {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(failure(format!(
            "prepared context field {field} must be 0 or 1"
        ))),
    }
}

#[allow(dead_code)] // consumed by the graph scheduling integration
fn parse_u32(text: &str, field: &str) -> Result<u32> {
    if text.is_empty() || text.len() > 10 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(failure(format!(
            "prepared context field {field} must be a non-negative integer"
        )));
    }
    text.parse::<u32>().map_err(|_| {
        failure(format!(
            "prepared context field {field} must be a non-negative integer"
        ))
    })
}

#[allow(dead_code)] // consumed by the graph scheduling integration
fn hex64(text: &str, field: &str) -> Result<String> {
    if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(failure(format!(
            "prepared context field {field} must be a 64-digit SHA-256"
        )));
    }
    Ok(text.to_ascii_lowercase())
}

#[allow(dead_code)] // consumed by the graph scheduling integration
fn parse_neutral(text: &str) -> Result<[f64; 3]> {
    let parts: Vec<&str> = text.split(',').collect();
    if parts.len() != 3 {
        return Err(failure("prepared context as_shot_neutral requires three values"));
    }
    let mut out = [0.0f64; 3];
    for (index, part) in parts.iter().enumerate() {
        let value = part
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or_else(|| failure("prepared context as_shot_neutral must be positive finite"))?;
        out[index] = value;
    }
    Ok(out)
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
}
impl NativeStream {
    fn open(tool: &Path, args: &[String], log: &Path, active: ActiveNative) -> Result<Self> {
        let process = OwnedProcess::spawn_active(tool, args, log, EXIT_FRAME, false, active)?;
        let stdout = process.child.lock().unwrap().stdout.take().unwrap();
        let mut stream = Self {
            process,
            protocol: None,
            pixel_bytes: 0,
            expected_digest: None,
            f32: args.first().is_some_and(|a| a == "--stdout-frame-f32"),
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
    }
}

struct NativeRun {
    method: crate::color_profile::CameraColorMethod,
    log: PathBuf,
    analysis_roi: Option<[f64; 4]>,
    #[cfg(test)]
    converter: Option<String>,
    #[cfg(test)]
    converter_timeout: Option<u64>,
}

impl NativeRun {
    #[cfg(test)]
    fn from_produce(config: &ProduceConfig<'_>) -> Self {
        Self {
            method: config.camera_color_method,
            log: config.logs_dir.join("frame_000001.log"),
            converter: config.converter.map(str::to_string),
            converter_timeout: config.converter_timeout,
            analysis_roi: config.analysis_roi,
        }
    }
}

/// Either an original source file or a validated prepared context. Both reach
/// the same native consumer protocol; only the second may not carry Converter
/// options because the conversion already happened during --prepare. The
/// source variant exists only for the retired-producer tests.
#[derive(Clone, Copy)]
enum FrameInput<'a> {
    #[cfg(test)]
    Source(&'a SourceFrame),
    Prepared(&'a PreparedFrame),
}

impl FrameInput<'_> {
    fn name(&self) -> &str {
        match self {
            #[cfg(test)]
            Self::Source(source) => &source.name,
            Self::Prepared(prepared) => &prepared.source_name,
        }
    }
    fn path(&self) -> &Path {
        match self {
            #[cfg(test)]
            Self::Source(source) => &source.path,
            Self::Prepared(prepared) => &prepared.source,
        }
    }
    fn prepared(&self) -> Option<&PreparedFrame> {
        match self {
            #[cfg(test)]
            Self::Source(_) => None,
            Self::Prepared(prepared) => Some(prepared),
        }
    }
}

#[cfg(test)]
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
#[cfg(test)]
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
#[cfg(test)]
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
    let run = NativeRun::from_produce(config);
    let mut frames: Vec<FrameInfo> = Vec::with_capacity(sources.len());
    for (offset, source) in sources.iter().enumerate() {
        let reference = frames
            .first()
            .map(|f| f.applied_wb)
            .filter(|_| config.white_balance.freezes_reference());
        let ev = evs.map(|e| e[offset]).or(config.exposure_ev);
        let info = run_frame(
            tool,
            FrameInput::Source(source),
            offset + 1,
            sources.len(),
            reference,
            ev,
            &run,
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
/// Settings for `dng-linear-frame --prepare`. The caller owns the scratch
/// directory (it must already exist), the context path and its transcript.
pub struct PrepareSettings<'a> {
    pub scratch_dir: &'a Path,
    pub context_path: &'a Path,
    pub log: &'a Path,
    pub converter: Option<&'a str>,
    pub converter_timeout: Option<u64>,
}

/// Prepared-frame consumer settings. No Converter options exist here on
/// purpose: a prepared context already records its conversion provenance.
pub struct PreparedSettings {
    pub camera_color_method: crate::color_profile::CameraColorMethod,
}

/// Run the native prepare operation once for one source and load the resulting
/// context. The returned frame carries the original identity; the prepared DNG
/// stays in the caller's scratch until the caller deletes it.
#[cfg(test)]
pub fn prepare(
    tool: &Path,
    source: &SourceFrame,
    settings: &PrepareSettings<'_>,
) -> Result<PreparedFrame> {
    prepare_active(tool, source, settings, Default::default())
}
pub fn prepare_active(
    tool: &Path,
    source: &SourceFrame,
    settings: &PrepareSettings<'_>,
    active: ActiveNative,
) -> Result<PreparedFrame> {
    let mut args = vec![
        "--prepare".to_string(),
        source.path.display().to_string(),
        "--scratch".into(),
        settings.scratch_dir.display().to_string(),
        "--emit-prepared".into(),
        settings.context_path.display().to_string(),
    ];
    if let Some(path) = settings.converter {
        args.extend(["--converter".into(), path.into()]);
    }
    if let Some(seconds) = settings.converter_timeout {
        args.extend(["--converter-timeout".into(), seconds.to_string()]);
    }
    let captured = proc::run_capture_active(tool, &args, EXIT_FRAME, settings.log, active)?;
    write_prepare_log(settings.log, tool, &args, &captured)?;
    proc::check_stop()?;
    if !captured.status.success() {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "prepared-frame conversion failed for {}: {}; transcript {}: {}",
                source.name,
                proc::exit_description(&captured.status),
                settings.log.display(),
                proc::tail(&captured.stderr, 1200)
            ),
        ));
    }
    let prepared = PreparedFrame::load(settings.context_path)?;
    let expected = fs::canonicalize(&source.path).map_err(|e| {
        failure(format!(
            "requested source {} cannot be resolved: {e}",
            source.path.display()
        ))
    })?;
    if prepared.source != expected {
        return Err(failure(format!(
            "prepared context {} describes {} instead of the requested source {}",
            settings.context_path.display(),
            prepared.source.display(),
            expected.display()
        )));
    }
    Ok(prepared)
}

fn write_prepare_log(
    log: &Path,
    tool: &Path,
    args: &[String],
    captured: &proc::Captured,
) -> Result<()> {
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| Error::new(EXIT_FRAME, format!("cannot create {}: {e}", parent.display())))?;
    }
    let text = format!(
        "command: {}\nexit: {}\nstdout:\n{}\nstderr:\n{}\n",
        proc::display_command(tool, args),
        proc::exit_description(&captured.status),
        captured.stdout,
        captured.stderr
    );
    fs::write(log, text)
        .map_err(|e| Error::new(EXIT_FRAME, format!("cannot write {}: {e}", log.display())))
}

fn prepared_native_run(
    settings: &PreparedSettings,
    analysis_roi: Option<[f64; 4]>,
    log: &Path,
) -> NativeRun {
    NativeRun {
        method: settings.camera_color_method,
        log: log.to_path_buf(),
        analysis_roi,
        #[cfg(test)]
        converter: None,
        #[cfg(test)]
        converter_timeout: None,
    }
}

/// Analysis pass over a prepared context. Invokes no Converter and never
/// deletes the prepared DNG.
#[cfg(test)]
pub fn analyze_prepared(
    tool: &Path,
    prepared: &PreparedFrame,
    override_wb: Option<[f64; 3]>,
    exposure_ev: Option<f64>,
    analysis_roi: Option<[f64; 4]>,
    settings: &PreparedSettings,
    log: &Path,
) -> Result<FrameInfo> {
    let run = prepared_native_run(settings, analysis_roi, log);
    run_frame(
        tool,
        FrameInput::Prepared(prepared),
        1,
        1,
        override_wb,
        exposure_ev,
        &run,
        false,
        Default::default(),
        &mut |_, _| Ok(()),
    )
    .map_err(|error| prepared_consumer_error(error, &log))
}

fn prepared_consumer_error(error: Error, log: &Path) -> Error {
    let detail = fs::read_to_string(log).unwrap_or_default();
    Error::new(
        error.code,
        format!(
            "prepared-frame consumer failed: {}; transcript {}: {}",
            error.message,
            log.display(),
            proc::tail(&detail, 1200)
        ),
    )
}

/// FP32 develop pass over a prepared context. The caller reads pixels through
/// the callback while the native process is still alive.
#[cfg(test)]
pub fn develop_prepared(
    tool: &Path,
    prepared: &PreparedFrame,
    override_wb: Option<[f64; 3]>,
    exposure_ev: Option<f64>,
    settings: &PreparedSettings,
    log: &Path,
    consume: &mut impl FnMut(&FrameInfo, &mut NativeStream) -> Result<()>,
) -> Result<FrameInfo> {
    let run = prepared_native_run(settings, None, log);
    run_frame(
        tool,
        FrameInput::Prepared(prepared),
        1,
        1,
        override_wb,
        exposure_ev,
        &run,
        true,
        Default::default(),
        consume,
    )
    .map_err(|error| prepared_consumer_error(error, &log))
}

/// Scheduler entry points own the frame ID and transcript path.
pub fn analyze_prepared_at(
    tool: &Path,
    prepared: &PreparedFrame,
    wb: Option<[f64; 3]>,
    roi: [f64; 4],
    settings: &PreparedSettings,
    id: usize,
    total: usize,
    log: &Path,
    active: ActiveNative,
) -> Result<FrameInfo> {
    let run = prepared_native_run(settings, Some(roi), log);
    run_frame(
        tool,
        FrameInput::Prepared(prepared),
        id + 1,
        total,
        wb,
        Some(0.),
        &run,
        false,
        active,
        &mut |_, _| Ok(()),
    )
    .map_err(|e| prepared_consumer_error(e, log))
}
pub fn develop_prepared_at(
    tool: &Path,
    prepared: &PreparedFrame,
    wb: Option<[f64; 3]>,
    ev: f64,
    settings: &PreparedSettings,
    id: usize,
    total: usize,
    log: &Path,
    active: ActiveNative,
    consume: &mut impl FnMut(&FrameInfo, &mut NativeStream) -> Result<()>,
) -> Result<FrameInfo> {
    let run = prepared_native_run(settings, None, log);
    run_frame(
        tool,
        FrameInput::Prepared(prepared),
        id + 1,
        total,
        wb,
        Some(ev),
        &run,
        true,
        active,
        consume,
    )
    .map_err(|e| prepared_consumer_error(e, log))
}

fn run_frame(
    tool: &Path,
    input: FrameInput<'_>,
    index: usize,
    total: usize,
    override_wb: Option<[f64; 3]>,
    exposure_ev: Option<f64>,
    run: &NativeRun,
    pixels: bool,
    active: ActiveNative,
    consume: &mut impl FnMut(&FrameInfo, &mut NativeStream) -> Result<()>,
) -> Result<FrameInfo> {
    let started = Instant::now();
    let mut args = vec![if pixels {
        "--stdout-frame-f32"
    } else {
        "--stdout-analysis"
    }
    .to_string()];
    if run.method.requires_canon_cr3()
        && input
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .is_none_or(|e| !e.eq_ignore_ascii_case("cr3"))
    {
        return Err(failure("rev-eng Canon Log requires Canon CR3 photos"));
    }
    match input {
        #[cfg(test)]
        FrameInput::Source(source) => args.push(source.path.display().to_string()),
        FrameInput::Prepared(prepared) => {
            // The context already records the conversion; never pass a
            // Converter option on this path.
            args.push("--prepared".into());
            args.push(prepared.context.display().to_string());
        }
    }
    if let Some(prepared) = input.prepared() {
        #[cfg(test)]
        if run.converter.is_some() || run.converter_timeout.is_some() {
            return Err(failure(
                "a prepared context already carries its Converter provenance",
            ));
        }
        if prepared.route == PreparedRoute::Direct && prepared.generated {
            return Err(failure("prepared context route/generated mismatch"));
        }
    }
    args.extend([
        "--camera-color-method".into(),
        run.method.native_token().into(),
    ]);
    if let Some(wb) = override_wb {
        args.extend(["--white-balance".into(), format_wb(wb)]);
    }
    if let Some(ev) = exposure_ev {
        args.extend(["--exposure-ev".into(), format_ev(ev)]);
    }
    #[cfg(test)]
    if let FrameInput::Source(_) = input {
        if let Some(path) = &run.converter {
            args.extend(["--converter".into(), path.clone()]);
        }
        if let Some(seconds) = run.converter_timeout {
            args.extend(["--converter-timeout".into(), seconds.to_string()]);
        }
    }
    if !pixels {
        if let Some(roi) = run.analysis_roi {
            args.extend([
                "--analysis-roi".into(),
                roi.iter()
                    .map(|v| format_axis(*v))
                    .collect::<Vec<_>>()
                    .join(","),
            ]);
        }
    }
    let mut stream = NativeStream::open(tool, &args, &run.log, active)?;
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

        if run.method.requires_canon_cr3()
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
                != Some(run.method.native_token())
            || !(if run.method.requires_canon_cr3() {
                matches!(
                    effective,
                    Some("dng-camera-to-pcs-forward-matrix" | "dng-camera-to-pcs-inverse-fallback")
                )
            } else {
                effective == Some("legacy-inverse-cm")
            })
            || (run.method.requires_canon_cr3()
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
        .ok_or_else(|| missing_field(index, total, input.name(), "width"))?;
    let frame_height = sidecar
        .get("height")
        .and_then(Json::as_u32)
        .ok_or_else(|| missing_field(index, total, input.name(), "height"))?;
    if frame_width == 0 || frame_height == 0 {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) reports an empty image {}x{}",
                input.name(), frame_width, frame_height
            ),
        ));
    }

    let schema_version = sidecar
        .get("schema_version")
        .and_then(Json::as_u64)
        .ok_or_else(|| missing_field(index, total, input.name(), "schema_version"))?;
    if schema_version != 1 {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) has unsupported metadata schema_version {schema_version}",
                input.name()
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
            .ok_or_else(|| missing_field(index, total, input.name(), "source_sha256"))?
            .to_string(),
    );
    let _metadata_envelope = frame_metadata_envelope(
        file,
        schema_version,
        frame_width,
        frame_height,
        source_sha256.as_deref(),
        index,
        total,
        input.name(),
    )?;
    let applied_wb = parse_wb(
        &sidecar,
        "applied_white_balance",
        index,
        total,
        input.name(),
    )?;
    let as_shot_neutral = parse_wb(&sidecar, "as_shot_neutral", index, total, input.name())?;
    let wb_source = sidecar
        .get("white_balance_source")
        .and_then(Json::as_str)
        .ok_or_else(|| missing_field(index, total, input.name(), "white_balance_source"))?
        .to_string();

    if override_wb.is_none() {
        if wb_source != WB_SOURCE_AS_SHOT {
            return Err(Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) should use its as-shot white balance, metadata reports {wb_source:?}",
                    input.name()
                ),
            ));
        }
    } else if wb_source != WB_SOURCE_OVERRIDE {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) should use the frozen override white balance, metadata reports {wb_source:?}",
                input.name()
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
                input.name(), applied_wb, as_shot_neutral
            ),
        ));
    }

    let source_field = sidecar
        .get("source")
        .and_then(Json::as_str)
        .ok_or_else(|| missing_field(index, total, input.name(), "source"))?;
    // The native producer resolves the input path (symlinks included), so
    // compare canonical identities rather than the literal argument text.
    let (expected_source, reported_source) =
        canonical_source_pair(source_field, input.path()).map_err(|err| {
            Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) metadata source {source_field:?} cannot be resolved against the requested input: {err}",
                    input.name()
                ),
            )
        })?;
    if reported_source != expected_source {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) metadata source {source_field:?} does not resolve to the requested input {}",
                input.name(),
                expected_source.display()
            ),
        ));
    }


    let reported_ev = sidecar
        .get("exposure_ev")
        .and_then(Json::as_f64)
        .ok_or_else(|| missing_field(index, total, input.name(), "exposure_ev"))?;
    if !reported_ev.is_finite() {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) reports a non-finite exposure_ev",
                input.name()
            ),
        ));
    }
    let requested_ev = exposure_ev.unwrap_or(0.0);
    if reported_ev != requested_ev {
        return Err(Error::new(
            EXIT_FRAME,
            format!(
                "frame {index}/{total} ({}) reports exposure_ev {reported_ev} but {requested_ev} was requested",
                input.name()
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
                    input.name()
                ),
            ))
        }
    };

    if sidecar.get("analysis_sidecar") != Some(&Json::Null) {
        return Err(failure("stdout metadata analysis_sidecar must be null"));
    }
    // A prepared consumer proves it decoded the intended cache artifact while
    // keeping the original photograph as the identity authority.
    if let Some(prepared) = input.prepared() {
        let block = sidecar
            .get("prepared")
            .ok_or_else(|| failure("prepared metadata block is missing"))?;
        if block.get("schema_version").and_then(Json::as_u64) != Some(1)
            || block.get("consumed_context") != Some(&Json::Bool(true))
        {
            return Err(failure("prepared metadata block is inconsistent"));
        }
        if block.get("generated") != Some(&Json::Bool(prepared.generated)) {
            return Err(failure(
                "prepared metadata generated flag does not match the context",
            ));
        }
        let dng = block
            .get("dng")
            .and_then(Json::as_str)
            .ok_or_else(|| failure("prepared metadata block has no DNG path"))?;
        let dng_path = fs::canonicalize(dng).map_err(|e| {
            failure(format!(
                "prepared metadata DNG {dng:?} cannot be resolved: {e}"
            ))
        })?;
        if dng_path != prepared.dng
            || block.get("sha256").and_then(Json::as_str)
                != Some(prepared.dng_sha256.as_str())
        {
            return Err(failure(
                "prepared metadata DNG identity does not match the context",
            ));
        }
        if sidecar.get("route").and_then(Json::as_str) != Some(prepared.route.token()) {
            return Err(failure(
                "prepared metadata route does not match the context",
            ));
        }
        if source_sha256.as_deref() != Some(prepared.source_sha256.as_str()) {
            return Err(failure(
                "prepared metadata source_sha256 does not match the context",
            ));
        }
        if !wb_close(as_shot_neutral, prepared.as_shot_neutral) {
            return Err(failure(
                "prepared metadata as_shot_neutral does not match the context",
            ));
        }
        if capture_time.as_ref() != prepared.capture_time.as_ref() {
            return Err(failure(
                "prepared metadata capture_time does not match the context",
            ));
        }
    }
    let mut info = FrameInfo {
        index,
        source_name: input.name().to_string(),
        camera_color: sidecar.get("color").cloned(),
        source_sha256,
        pixels_sha256: None,
        width: frame_width,
        height: frame_height,
        applied_wb,
        wb_source,
        as_shot_neutral,
        exposure_ev: reported_ev,
        duration: Duration::ZERO,
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

#[cfg(test)]
mod prepared_tests {
    use super::*;
    use crate::cli::WhiteBalance;
    use crate::order::SourceFrame;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::sync::OnceLock;

    fn fake_binary() -> PathBuf {
        static BINARY: OnceLock<PathBuf> = OnceLock::new();
        BINARY
            .get_or_init(|| {
                let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("target")
                    .join("fake-native-bin");
                fs::create_dir_all(&directory).expect("create fake native dir");
                let binary = directory.join("fake-native-prepared");
                let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests")
                    .join("common")
                    .join("fake_native_main.rs");
                let output = Command::new("rustc")
                    .args(["--edition", "2021", "-O"])
                    .arg(&source)
                    .arg("-o")
                    .arg(&binary)
                    .output()
                    .expect("run rustc for the fake native producer");
                assert!(
                    output.status.success(),
                    "fake native compilation failed:\n{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                binary
            })
            .clone()
    }

    fn shell_quote(text: &str) -> String {
        format!("'{}'", text.replace('\'', "'\\''"))
    }

    fn work_dir(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(format!("prepared-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn source_frame(dir: &std::path::Path, name: &str) -> SourceFrame {
        let input = dir.join("input");
        fs::create_dir_all(&input).expect("create input dir");
        let path = input.join(name);
        fs::write(&path, b"synthetic raw bytes").expect("write source");
        SourceFrame {
            path,
            name: name.to_string(),
            key: Vec::new(),
        }
    }

    /// Write a fake-producer config and a wrapper that records every argv.
    fn fake_tool(dir: &std::path::Path, frames: &[&str], extra: &str) -> PathBuf {
        let config = dir.join("fake.config");
        let text = format!(
            "width=8\nheight=6\ngrid_nx=4\ngrid_ny=2\n{extra}\n{}\n",
            frames.join("\n")
        );
        fs::write(&config, text).expect("write fake config");
        let argv = dir.join("argv.log");
        let wrapper = dir.join("fake-dng-linear-frame");
        let script = format!(
            "#!/bin/sh\nexport FAKE_NATIVE_CONFIG={}\nprintf '%s\\n' \"$@\" >> {}\nexec {} \"$@\"\n",
            shell_quote(&config.display().to_string()),
            shell_quote(&argv.display().to_string()),
            shell_quote(&fake_binary().display().to_string())
        );
        fs::write(&wrapper, script).expect("write wrapper");
        let mut permissions = fs::metadata(&wrapper).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).expect("chmod wrapper");
        wrapper
    }

    const FRAME_1: &str = "frame=IMG_1.DNG|0.5,1,0.8|0.0|0.0|-|-|flat";
    const FRAME_2: &str = "frame=IMG_2.CR3|0.6,1,0.7|0.2|0.3|2026:11:13 04:05:06|-|flat";

    fn prepared_settings() -> PreparedSettings {
        PreparedSettings {
            camera_color_method: crate::color_profile::CameraColorMethod::LegacyInverseCm,
        }
    }

    fn prepare_one(
        dir: &std::path::Path,
        tool: &Path,
        source: &SourceFrame,
        name: &str,
    ) -> PreparedFrame {
        let scratch = dir.join(format!("scratch-{name}"));
        fs::create_dir_all(&scratch).expect("create scratch");
        prepare(
            tool,
            source,
            &PrepareSettings {
                scratch_dir: &scratch,
                context_path: &dir.join(format!("{name}.ctx")),
                log: &dir.join(format!("{name}-prepare.log")),
                converter: None,
                converter_timeout: None,
            },
        )
        .expect("prepare")
    }

    #[test]
    fn prepared_round_trip_uses_the_context_and_validates_metadata() {
        let dir = work_dir("round-trip");
        let source = source_frame(&dir, "IMG_1.DNG");
        let tool = fake_tool(&dir, &[FRAME_1], "");
        let prepared = prepare_one(&dir, &tool, &source, "direct");
        assert_eq!(prepared.route, PreparedRoute::Direct);
        assert!(!prepared.generated);
        assert_eq!(prepared.source, fs::canonicalize(&source.path).unwrap());
        assert_eq!(prepared.dng, prepared.source);
        assert_eq!(prepared.source_sha256, prepared.dng_sha256);
        assert_eq!(
            (prepared.main_ifd_width, prepared.main_ifd_height),
            (8, 6)
        );
        assert_eq!(prepared.capture_time, None);
        assert_eq!(prepared.as_shot_neutral, [0.5, 1.0, 0.8]);

        let logs = dir.join("logs");
        let settings = prepared_settings();
        let analysis =
            analyze_prepared(&tool, &prepared, None, Some(0.0), None, &settings, &logs.join("frame_000001.log")).expect("analyze");
        assert_eq!(analysis.index, 1);
        assert_eq!(analysis.source_name, "IMG_1.DNG");
        assert_eq!(analysis.applied_wb, prepared.as_shot_neutral);
        assert_eq!(analysis.wb_source, WB_SOURCE_AS_SHOT);
        assert_eq!(
            analysis.source_sha256.as_deref(),
            Some(prepared.source_sha256.as_str())
        );
        assert!(analysis_bytes(&analysis)
            .expect("sidecar")
            .starts_with(b"{\"schema_version\":2"));

        let mut pixel_bytes = 0usize;
        let developed = develop_prepared(&tool, &prepared, None, Some(0.25), &settings, &logs.join("frame_000001.log"), &mut |info, native| {
            let pixels = native.pixels(info.width, info.height)?;
            pixel_bytes = pixels.len();
            assert!(pixels
                .chunks_exact(4)
                .all(|b| f32::from_le_bytes(b.try_into().unwrap()).is_finite()));
            Ok(())
        })
        .expect("develop");
        assert_eq!(pixel_bytes, 8 * 6 * 12);
        assert!(developed.pixels_sha256.is_some());
        assert_eq!(developed.exposure_ev, 0.25);

        let overridden = analyze_prepared(
            &tool,
            &prepared,
            Some([0.7, 1.0, 0.6]),
            None,
            Some([0.0, 0.0, 0.5, 0.5]),
            &settings,
            &logs.join("frame_000002.log"),
        )
        .expect("override analyze");
        assert_eq!(overridden.wb_source, WB_SOURCE_OVERRIDE);
        assert_eq!(overridden.applied_wb, [0.7, 1.0, 0.6]);

        // Every consumer was invoked with --prepared and never with Converter
        // options.
        let argv = fs::read_to_string(dir.join("argv.log")).expect("argv log");
        assert_eq!(argv.lines().filter(|line| *line == "--prepared").count(), 3);
        assert!(!argv
            .lines()
            .any(|line| line == "--converter" || line == "--converter-timeout"));
        assert_eq!(prepared.dng, fs::canonicalize(&source.path).unwrap());
    }

    #[test]
    fn prepared_converter_route_and_canon_method_guard() {
        let dir = work_dir("converter-route");
        let source = source_frame(&dir, "IMG_2.CR3");
        let tool = fake_tool(&dir, &[FRAME_2], "");
        let prepared = prepare_one(&dir, &tool, &source, "vendor");
        assert_eq!(prepared.route, PreparedRoute::VendorRaw);
        assert!(prepared.generated);
        assert_ne!(prepared.dng, prepared.source);
        assert!(prepared.dng.exists());
        assert_eq!(
            prepared.dng,
            fs::canonicalize(dir.join("scratch-vendor/linear/IMG_2.CR3")).unwrap()
        );
        assert_eq!(
            prepared.capture_time.as_deref(),
            Some("2026:11:13 04:05:06")
        );

        let logs = dir.join("canon-logs");
        let settings = PreparedSettings {
            camera_color_method: crate::color_profile::CameraColorMethod::DngCameraToPcs,
        };
        let info = analyze_prepared(&tool, &prepared, None, Some(0.0), None, &settings, &logs.join("frame_000001.log"))
            .expect("canon analyze");
        assert_eq!(
            info.camera_color
                .as_ref()
                .and_then(|color| color.get("camera_model"))
                .and_then(Json::as_str),
            Some("Canon EOS R7")
        );
        develop_prepared(&tool, &prepared, Some([0.6, 1.0, 0.7]), Some(0.0), &settings, &logs.join("frame_000001.log"), &mut |info, native| {
            native.pixels(info.width, info.height)?;
            Ok(())
        })
        .expect("canon develop");
        assert!(prepared.dng.exists(), "consumers must not delete the cache");

        // The release Canon method refuses a prepared LinearRaw DNG before any
        // native process starts.
        let dng_source = source_frame(&dir, "IMG_1.DNG");
        let dng_tool = fake_tool(&dir, &[FRAME_1], "");
        let dng_prepared = prepare_one(&dir, &dng_tool, &dng_source, "dng");
        let error = analyze_prepared(&tool, &dng_prepared, None, None, None, &settings, &logs.join("frame_000001.log"))
            .expect_err("Canon method requires CR3");
        assert!(
            error.message.contains("requires Canon CR3"),
            "{}",
            error.message
        );
    }

    #[test]
    fn prepared_metadata_identity_defects_are_rejected() {
        let dir = work_dir("metadata-defects");
        let source = source_frame(&dir, "IMG_2.CR3");
        let base = fake_tool(&dir, &[FRAME_2], "");
        let prepared = prepare_one(&dir, &base, &source, "base");
        let settings = prepared_settings();
        for (defect, needle) in [
            ("route", "route"),
            ("dng", "DNG identity"),
            ("sha", "DNG identity"),
            ("generated", "generated"),
            ("consumed", "inconsistent"),
            ("missing", "missing"),
        ] {
            let tool = fake_tool(
                &dir,
                &[FRAME_2],
                &format!("prepared_metadata_defect={defect}"),
            );
            let error = analyze_prepared(&tool, &prepared, None, None, None, &settings, &dir.join("defect.log"))
                .expect_err(defect);
            assert!(
                error.message.contains(needle),
                "{defect}: {}",
                error.message
            );
        }
    }

    #[test]
    fn prepared_context_parser_and_prepare_failures_are_explicit() {
        let dir = work_dir("parser");
        let source = source_frame(&dir, "IMG_1.DNG");
        let tool = fake_tool(&dir, &[FRAME_1], "");
        let scratch = dir.join("scratch");
        fs::create_dir_all(&scratch).unwrap();
        let context = dir.join("IMG_1.ctx");
        let settings = PrepareSettings {
            scratch_dir: &scratch,
            context_path: &context,
            log: &dir.join("prepare.log"),
            converter: None,
            converter_timeout: None,
        };
        prepare(&tool, &source, &settings).expect("prepare");
        let valid = fs::read_to_string(&context).expect("context");

        // A missing scratch directory is rejected before any conversion.
        let missing = PrepareSettings {
            scratch_dir: &dir.join("absent"),
            context_path: &dir.join("absent.ctx"),
            log: &dir.join("absent.log"),
            converter: None,
            converter_timeout: None,
        };
        assert!(prepare(&tool, &source, &missing).is_err());

        // The fake fails the conversion and the Rust side surfaces its log.
        let failing_tool = fake_tool(&dir, &[FRAME_1], "prepare_fail=1");
        let error = prepare(&failing_tool, &source, &settings).expect_err("prepare failure");
        assert!(error.message.contains("prepared-frame conversion failed"), "{}", error.message);
        assert!(dir.join("prepare.log").exists());

        // A context that names another source is rejected by prepare().
        let other = source_frame(&dir, "OTHER.DNG");
        let swapped_tool = fake_tool(
            &dir,
            &[FRAME_1],
            &format!(
                "prepared_source_override={}\nprepared_dng_override={}",
                other.path.display(),
                other.path.display()
            ),
        );
        let error = prepare(&swapped_tool, &source, &settings).expect_err("source swap");
        assert!(error.message.contains("instead of the requested source"), "{}", error.message);

        // Structural grammar failures fail closed.
        for (name, text) in [
            ("header", valid.replacen("rawtl-prepared-frame/1", "rawtl-prepared-frame/2", 1)),
            ("missing", valid.replace("route=linearraw-sdk\n", "")),
            ("duplicate", format!("{valid}route=linearraw-sdk\n")),
            ("escape", valid.replace("source=", "source=%zz")),
        ] {
            let path = dir.join(format!("{name}.ctx"));
            fs::write(&path, text).unwrap();
            assert!(PreparedFrame::load(&path).is_err(), "{name}");
        }

        // Relative identities are never resolved from the current directory.
        let relative = valid
            .replace(
                &format!("source={}", source.path.display()),
                "source=input/IMG_1.DNG",
            )
            .replace(
                &format!("prepared_dng={}", source.path.display()),
                "prepared_dng=input/IMG_1.DNG",
            );
        let path = dir.join("relative.ctx");
        fs::write(&path, relative).unwrap();
        assert!(PreparedFrame::load(&path).is_err());

        // Route/generated consistency and the direct-cache rule.
        let mismatched = valid.replace("generated=0", "generated=1");
        let path = dir.join("generated.ctx");
        fs::write(&path, mismatched).unwrap();
        assert!(PreparedFrame::load(&path).is_err());

        // A percent-escaped path round-trips through the parser.
        let odd_dir = dir.join("odd % name");
        fs::create_dir_all(&odd_dir).unwrap();
        let odd = odd_dir.join("odd % frame.dng");
        fs::write(&odd, b"odd").unwrap();
        let canonical = fs::canonicalize(&odd).unwrap();
        let sha = crate::digest::sha256(b"odd");
        let escaped = canonical.display().to_string().replace('%', "%25").replace(' ', "%20");
        let context_text = format!(
            "rawtl-prepared-frame/1\nsource={escaped}\nsource_sha256={sha}\nroute=linearraw-sdk\nprepared_dng={escaped}\nprepared_sha256={sha}\ngenerated=0\nmain_ifd_width=8\nmain_ifd_height=6\nas_shot_neutral=0.5,1,0.8\nhas_capture_time=0\nconverter_used=0\n"
        );
        let path = dir.join("escaped.ctx");
        fs::write(&path, context_text).unwrap();
        let loaded = PreparedFrame::load(&path).expect("escaped context");
        assert_eq!(loaded.dng, canonical);
        assert_eq!(loaded.source, canonical);
    }

    fn converter_wrapper(dir: &std::path::Path, name: &str, counter: &std::path::Path) -> PathBuf {
        let real = std::env::var("RAWTL_REAL_CONVERTER").unwrap_or_else(|_| {
            "/Applications/Adobe DNG Converter.app/Contents/MacOS/Adobe DNG Converter".to_string()
        });
        let path = dir.join(name);
        let script = format!(
            "#!/bin/sh\necho call >> {}\nexec {} \"$@\"\n",
            shell_quote(&counter.display().to_string()),
            shell_quote(&real)
        );
        fs::write(&path, script).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn counter_lines(path: &std::path::Path) -> usize {
        fs::read_to_string(path)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    fn pixel_digest(config: &ProduceConfig<'_>, tool: &Path, sources: &[SourceFrame], evs: &[f64], digests: &mut Vec<String>, infos: &mut Vec<FrameInfo>) {
        let sequence = stream_frames(tool, sources, evs, config, Default::default(), &mut |info, native| {
            let pixels = native.pixels(info.width, info.height)?;
            digests.push(crate::digest::sha256(&pixels));
            Ok(())
        })
        .expect("legacy stream");
        infos.extend(sequence.frames);
    }

    /// Real-material oracle. Opt-in because it needs the built native tool and
    /// staged Canon CR3s:
    /// `RAWTL_NATIVE_TOOL=... RAWTL_REAL_CR3=a.CR3,b.CR3 RAWTL_PREPARED_WORK=dir \
    ///  cargo test --bin rawtl-linear-video -- --ignored --test-threads=1`
    #[test]
    #[ignore = "requires RAWTL_NATIVE_TOOL, RAWTL_REAL_CR3 and RAWTL_PREPARED_WORK"]
    fn prepared_context_on_real_native_frames() {
        let tool = PathBuf::from(
            std::env::var_os("RAWTL_NATIVE_TOOL").expect("RAWTL_NATIVE_TOOL"),
        );
        let materials: Vec<PathBuf> = std::env::var("RAWTL_REAL_CR3")
            .expect("RAWTL_REAL_CR3")
            .split(',')
            .map(PathBuf::from)
            .collect();
        assert!(!materials.is_empty() && materials.len() <= 2);
        let work = PathBuf::from(std::env::var_os("RAWTL_PREPARED_WORK").expect("RAWTL_PREPARED_WORK"));
        fs::create_dir_all(&work).expect("create evidence dir");
        // Only this test's own artifacts are removed; unknown entries stay.
        for entry in fs::read_dir(&work).expect("read evidence dir") {
            let entry = entry.expect("evidence entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let owned = name.starts_with("scratch-")
                || name.ends_with(".ctx")
                || name.ends_with(".log")
                || name.starts_with("converter-")
                || name.ends_with("-calls.txt")
                || name.ends_with("-logs");
            if !owned {
                continue;
            }
            let path = entry.path();
            if entry.file_type().expect("entry type").is_dir() {
                fs::remove_dir_all(&path).expect("remove old evidence dir");
            } else {
                fs::remove_file(&path).expect("remove old evidence file");
            }
        }
        let prepared_counter = work.join("prepared-converter-calls.txt");
        let legacy_counter = work.join("legacy-converter-calls.txt");
        let prepared_converter = converter_wrapper(&work, "converter-prepared", &prepared_counter);
        let legacy_converter = converter_wrapper(&work, "converter-legacy", &legacy_counter);
        let prepared_logs = work.join("prepared-logs");
        let legacy_logs = work.join("legacy-logs");
        let settings = PreparedSettings {
            camera_color_method: crate::color_profile::CameraColorMethod::DngCameraToPcs,
        };
        let sources: Vec<SourceFrame> = materials
            .iter()
            .map(|path| SourceFrame {
                path: path.clone(),
                name: path.file_name().unwrap().to_str().unwrap().to_string(),
                key: Vec::new(),
            })
            .collect();

        // Prepare both frames once; this is the only Converter invocation on
        // the prepared path.
        let mut prepared_frames = Vec::new();
        for (index, source) in sources.iter().enumerate() {
            let scratch = work.join(format!("scratch-{index}"));
            fs::create_dir_all(&scratch).unwrap();
            let frame = prepare(
                &tool,
                source,
                &PrepareSettings {
                    scratch_dir: &scratch,
                    context_path: &work.join(format!("frame-{index}.ctx")),
                    log: &work.join(format!("prepare-{index}.log")),
                    converter: Some(prepared_converter.to_str().unwrap()),
                    converter_timeout: Some(120),
                },
            )
            .expect("real prepare");
            assert_eq!(
                frame.route,
                PreparedRoute::VendorRaw,
                "a CR3 must route through the Converter"
            );
            assert!(frame.generated);
            assert!(frame.dng.exists());
            prepared_frames.push(frame);
        }
        assert_eq!(counter_lines(&prepared_counter), sources.len());

        // Analysis: the first frame uses its as-shot neutral, later frames the
        // frozen first-frame reference (the product's default).
        let first = analyze_prepared(&tool, &prepared_frames[0], None, Some(0.0), None, &settings, &prepared_logs.join("frame_000001.log"))
            .expect("prepare analysis");
        let first_wb = first.applied_wb;
        let legacy_config = ProduceConfig {
            camera_color_method: crate::color_profile::CameraColorMethod::DngCameraToPcs,
            logs_dir: &legacy_logs,
            exposure_ev: None,
            analysis_roi: None,
            echo: false,
            echo_tag: "",
            white_balance: WhiteBalance::FirstFrame,
            converter: Some(legacy_converter.to_str().unwrap()),
            converter_timeout: Some(120),
        };
        let baseline = produce_with_evs(&tool, &sources, &[0.0, 0.0][..sources.len()], &legacy_config)
            .expect("legacy analysis");
        for (index, (prepared, legacy)) in prepared_frames.iter().zip(&baseline.frames).enumerate() {
            let wb = if index == 0 { None } else { Some(first_wb) };
            let info = analyze_prepared(&tool, prepared, wb, Some(0.0), None, &settings, &prepared_logs.join(format!("frame_{index:06}.log")))
                .expect("prepared analysis");
            assert_eq!(info.applied_wb, legacy.applied_wb, "frame {index} WB");
            assert_eq!(info.as_shot_neutral, legacy.as_shot_neutral);
            assert_eq!(info.camera_color, legacy.camera_color, "frame {index} color");
            assert_eq!(info.source_sha256, legacy.source_sha256);
            assert_eq!(
                analysis_bytes(&info),
                analysis_bytes(legacy),
                "frame {index} analysis sidecar bytes"
            );
        }

        // FP32 develop at 0 and a known EV: bitwise digest and metadata equality
        // against the old same-source single-shot path.
        for ev in [0.0f64, 0.25] {
            let mut legacy_digests = Vec::new();
            let mut legacy_infos = Vec::new();
            pixel_digest(
                &legacy_config,
                &tool,
                &sources,
                &[ev; 2][..sources.len()],
                &mut legacy_digests,
                &mut legacy_infos,
            );
            for (index, prepared) in prepared_frames.iter().enumerate() {
                let wb = if index == 0 { None } else { Some(first_wb) };
                let mut digest = String::new();
                let info = develop_prepared(&tool, prepared, wb, Some(ev), &settings, &prepared_logs.join(format!("frame_{index:06}.log")), &mut |info, native| {
                    let pixels = native.pixels(info.width, info.height)?;
                    digest = crate::digest::sha256(&pixels);
                    Ok(())
                })
                .expect("prepared develop");
                assert_eq!(digest, legacy_digests[index], "frame {index} EV {ev} FP32 hash");
                assert_eq!(info.pixels_sha256, legacy_infos[index].pixels_sha256);
                assert_eq!(info.applied_wb, legacy_infos[index].applied_wb);
                assert_eq!(info.as_shot_neutral, legacy_infos[index].as_shot_neutral);
                assert_eq!(info.camera_color, legacy_infos[index].camera_color);
                assert_eq!(info.width, legacy_infos[index].width);
                assert_eq!(info.height, legacy_infos[index].height);
            }
        }
        assert_eq!(counter_lines(&prepared_counter), sources.len());
        for prepared in &prepared_frames {
            assert!(prepared.dng.exists(), "prepared DNG must survive consumers");
        }

        // The caller's deletion is the only owner of the prepared artifact.
        let scratch = work.join("scratch-delete");
        fs::create_dir_all(&scratch).unwrap();
        let deletion = prepare(
            &tool,
            &sources[0],
            &PrepareSettings {
                scratch_dir: &scratch,
                context_path: &work.join("delete.ctx"),
                log: &work.join("prepare-delete.log"),
                converter: Some(prepared_converter.to_str().unwrap()),
                converter_timeout: Some(120),
            },
        )
        .expect("prepare deletion frame");
        fs::remove_file(&deletion.dng).expect("caller deletes prepared DNG");
        let error = analyze_prepared(&tool, &deletion, None, None, None, &settings, &prepared_logs.join("frame_000001.log"))
            .expect_err("deleted cache must fail");
        assert!(error.message.contains("prepared"), "{}", error.message);
        assert_eq!(counter_lines(&prepared_counter), sources.len() + 1);
    }
}
