//! Exact-length native record protocol and owned process lifetime.
#[cfg(test)]
use super::ProduceConfig;
use super::failure;
use crate::error::{Result, EXIT_FRAME};
use crate::proc::{self, ActiveNative, OwnedProcess};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;


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
}
impl NativeStream {
    pub(crate) fn open(tool: &Path, args: &[String], log: &Path, active: ActiveNative) -> Result<Self> {
        let process = OwnedProcess::spawn_active(tool, args, log, EXIT_FRAME, false, active)?;
        let stdout = process.child.lock().unwrap().stdout.take().unwrap();
        let mut stream = Self {
            process,
            protocol: None,
            pixel_bytes: 0,
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
        self.pixel_bytes = pixels.len();
        self.process
            .note(&format!("pixel bytes: {}", self.pixel_bytes));
        // A complete payload is insufficient: reject trailing bytes and a late
        // nonzero native exit before an encoder write can block on backpressure.
        self.finish()?;
        Ok(pixels)
    }
    pub(crate) fn json(&mut self, kind: u8) -> Result<Arc<[u8]>> {
        let bytes = self.protocol.as_mut().unwrap().record(kind, None)?;
        self.process.note(&format!("JSON {kind:#04x}: {} bytes", bytes.len()));
        Ok(bytes)
    }
    pub(crate) fn finish(&mut self) -> Result<()> {
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

pub(crate) struct NativeRun {
    pub(crate) method: crate::camera::CameraRecipe,
    pub(crate) lens: crate::lens::Settings,
    pub(crate) log: PathBuf,
    pub(crate) analysis_roi: Option<[f64; 4]>,
    pub(crate) preview_edge: Option<u32>,
    #[cfg(test)]
    pub(crate) converter: Option<String>,
    #[cfg(test)]
    pub(crate) converter_timeout: Option<u64>,
}

impl NativeRun {
    #[cfg(test)]
    pub(crate) fn from_produce(config: &ProduceConfig<'_>) -> Self {
        Self {
            method: config.camera_recipe,
            lens: crate::lens::Settings::default(),
            log: config.logs_dir.join("frame_000001.log"),
            converter: config.converter.map(str::to_string),
            converter_timeout: config.converter_timeout,
            analysis_roi: config.analysis_roi,
            preview_edge: None,
        }
    }
}
