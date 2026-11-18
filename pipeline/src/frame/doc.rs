//! Prepared-context line document: parsing, validation and encoding.
use super::failure;
use crate::error::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};


/// Versioned prepared-context header. The context is a strict line document
/// written by `dng-linear-frame --prepare`; both processes parse the same
/// bytes. The native consumer checks the captured file versions or verifies the file hashes.
#[allow(dead_code)] // consumed by the graph scheduling integration
pub const PREPARED_CONTEXT_MAGIC: &str = "rawtl-prepared-frame/2";
const EARLY_MEASUREMENT_MAGIC: &str = "rawtl-early-measurement/1";
const PREPARED_CONTEXT_LIMIT: usize = 256 * 1024;

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

#[derive(Debug)]
pub(crate) struct ExportRecord {
    pub id: usize,
    pub key: String,
    bytes: std::sync::Mutex<Option<std::sync::Arc<[u8]>>>,
    pub source_version: String,
    pub dng_version: String,
}

impl ExportRecord {
    pub fn input(&self) -> Result<std::sync::Arc<[u8]>> {
        self.bytes.lock().unwrap().clone().ok_or_else(|| failure("export frame metadata already released"))
    }

    pub fn release(&self) {
        self.bytes.lock().unwrap().take();
    }
}

/// One validated prepared frame. `source` is always the original photograph;
/// `dng` is the LinearRaw artifact the SDK decodes and is never used as the
/// identity for Canon calibration, white balance or capture metadata.
#[derive(Debug, Clone)]
#[allow(dead_code)] // consumed by the graph scheduling integration
pub struct PreparedFrame {
    pub context: PathBuf,
    pub(crate) export: Option<std::sync::Arc<ExportRecord>>,
    pub source: PathBuf,
    pub source_name: String,
    pub source_sha256: String,
    pub route: PreparedRoute,
    pub dng: PathBuf,
    pub dng_sha256: String,
    has_file_versions: bool,
    pub generated: bool,
    pub main_ifd_width: u32,
    pub main_ifd_height: u32,
    pub as_shot_neutral: [f64; 3],
    pub capture_time: Option<String>,
    pub lens_valid_area: Option<crate::lens::ValidArea>,
}

#[allow(dead_code)] // consumed by the graph scheduling integration
impl PreparedFrame {
    /// Validate the document and check source and output associations before
    /// starting a native consumer.
    pub(crate) fn file_check_args(&self, args: &mut Vec<String>) {
        if self.has_file_versions && self.export.is_none() {
            args.extend(["--prepared-file-check".into(), "state".into()]);
        }
    }

    pub fn load(context: &Path) -> Result<Self> {
        Self::load_context(context, false, &mut ConversionRecords::default())
    }

    pub(crate) fn load_export(bytes: Vec<u8>, scratch: &Path) -> Result<Self> {
        let fields = parse_document_fields(&bytes, "rawtl-export-frame/1", PREPARED_CONTEXT_LIMIT)?;
        let get = |key| field_value(&fields, key);
        let id = parse_u32(get("frame_id")?, "frame_id")? as usize;
        let source = PathBuf::from(get("source")?);
        let dng = PathBuf::from(get("prepared_dng")?);
        let route = PreparedRoute::from_token(get("route")?)
            .ok_or_else(|| failure("invalid export frame route"))?;
        let generated = parse_flag(get("generated")?, "generated")?;
        let source_version = get("source_version")?.to_owned();
        let dng_version = get("prepared_version")?.to_owned();
        let main_ifd_width = parse_u32(get("main_ifd_width")?, "main_ifd_width")?;
        let main_ifd_height = parse_u32(get("main_ifd_height")?, "main_ifd_height")?;
        if !source.is_absolute() || !dng.is_absolute() || generated != route.generates()
            || main_ifd_width == 0 || main_ifd_height == 0
            || source_version.is_empty() || dng_version.is_empty()
            || if generated {
                dng == source || dng.parent() != Some(scratch.join("linear").as_path())
            } else {
                dng != source || dng_version != source_version
            }
        {
            return Err(failure("export frame path, state, or geometry is inconsistent"));
        }
        let source_name = source.file_name().and_then(|name| name.to_str())
            .ok_or_else(|| failure("invalid export frame name"))?.to_owned();
        let as_shot_neutral = parse_neutral(get("as_shot_neutral")?)?;
        let capture_time = if parse_flag(get("has_capture_time")?, "has_capture_time")? {
            Some(get("capture_time")?.to_owned())
        } else {
            None
        };
        Ok(Self {
            context: PathBuf::new(),
            export: Some(std::sync::Arc::new(ExportRecord {
                id, key: id.to_string(), bytes: std::sync::Mutex::new(Some(bytes.into())),
                source_version, dng_version,
            })),
            source, source_name, source_sha256: String::new(), route, dng,
            dng_sha256: String::new(), has_file_versions: true, generated,
            main_ifd_width, main_ifd_height, as_shot_neutral, capture_time,
            lens_valid_area: None,
        })
    }
    pub fn load_early_measurement(context: &Path) -> Result<Self> {
        Self::load_context(context, true, &mut ConversionRecords::default())
    }

    pub(crate) fn load_in_sequence(context: &Path, records: &mut ConversionRecords) -> Result<Self> {
        Self::load_context(context, false, records)
    }

    fn load_context(context: &Path, provisional: bool, records: &mut ConversionRecords) -> Result<Self> {
        let bytes = read_document(context, PREPARED_CONTEXT_LIMIT)?;
        let fields = if provisional {
            parse_document_fields(&bytes, EARLY_MEASUREMENT_MAGIC, PREPARED_CONTEXT_LIMIT)?
        } else {
            parse_context_fields(&bytes)?
        };
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
        let has_file_versions = fields.iter().any(|(key, _)| {
            key == "source_version" || key == "prepared_version"
        });
        if has_file_versions && (get("source_version")?.is_empty()
            || get("prepared_version")?.is_empty()
            || (!generated && get("source_version")? != get("prepared_version")?))
        {
            return Err(failure("invalid prepared context file versions"));
        }
        validate_conversion(
            &fields, &source_raw, &source_sha256, &dng_raw, &dng_sha256,
            generated && !provisional, has_file_versions, records,
        )?;
        let source_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| failure("prepared source path has no UTF-8 file name"))?
            .to_string();
        Ok(Self {
            context: context.to_path_buf(),
            export: None,
            source,
            source_name,
            source_sha256,
            route,
            dng,
            dng_sha256,
            has_file_versions,
            generated,
            main_ifd_width,
            main_ifd_height,
            as_shot_neutral,
            capture_time,
            lens_valid_area: None,
        })
    }
}

#[allow(dead_code)] // consumed by the graph scheduling integration
pub(crate) fn parse_context_fields(bytes: &[u8]) -> Result<Vec<(String, String)>> {
    parse_document_fields(bytes, PREPARED_CONTEXT_MAGIC, PREPARED_CONTEXT_LIMIT)
}

pub(crate) fn parse_document_fields(bytes: &[u8], header: &str, limit: usize) -> Result<Vec<(String, String)>> {
    if !bytes.ends_with(b"\n") {
        return Err(failure("prepared document is truncated (missing final LF)"));
    }
    if bytes.len() > limit {
        return Err(failure("prepared context exceeds the size limit"));
    }
    let mut lines = bytes.split(|byte| *byte == b'\n');
    let magic = lines
        .next()
        .ok_or_else(|| failure("prepared context is empty"))?;
    if magic != header.as_bytes() {
        return Err(failure("prepared context has an unsupported header"));
    }
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut lines = lines.peekable();
    while let Some(line) = lines.next() {
        if line.is_empty() {
            if lines.peek().is_none() { break; }
            return Err(failure("prepared document contains an empty row"));
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
        if !seen.insert(key) {
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
    if out.contains(&0) { return Err(failure("prepared document contains NUL")); }
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
pub(crate) fn parse_u32(text: &str, field: &str) -> Result<u32> {
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
pub(crate) fn hex64(text: &str, field: &str) -> Result<String> {
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

pub(crate) fn read_document(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = fs::File::open(path)
        .map_err(|e| failure(format!("cannot read {}: {e}", path.display())))?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| failure(format!("cannot read {}: {e}", path.display())))?;
    if bytes.len() > limit {
        return Err(failure("prepared document exceeds size limit"));
    }
    Ok(bytes)
}
pub(crate) fn field_value<'a>(fields: &'a [(String, String)], key: &str) -> Result<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| failure(format!("prepared document missing field {key}")))
}
pub(crate) fn require_fields(fields: &[(String, String)], allowed: &[String]) -> Result<()> {
    if fields.len() != allowed.len() || fields.iter().any(|(k, _)| !allowed.contains(k)) {
        return Err(failure("prepared document has missing or unknown fields"));
    }
    Ok(())
}
fn validate_conversion(
    fields: &[(String, String)],
    source: &Path,
    source_hash: &str,
    dng: &Path,
    dng_hash: &str,
    generated: bool,
    has_file_versions: bool,
    records: &mut ConversionRecords,
) -> Result<()> {
    let get = |key: &str| field_value(fields, key);
    let mut allowed: Vec<String> = [
        "source",
        "source_sha256",
        "route",
        "prepared_dng",
        "prepared_sha256",
        "generated",
        "main_ifd_width",
        "main_ifd_height",
        "as_shot_neutral",
        "has_capture_time",
        "converter_used",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if has_file_versions {
        allowed.extend(["source_version".into(), "prepared_version".into()]);
    }
    if fields.iter().any(|(key, _)| key == "lens_calibration") {
        if !get("lens_calibration")?.starts_with("rawtl-lens-calibration/2 ") {
            return Err(failure("unsupported prepared lens calibration; run Prepare again"));
        }
        allowed.push("lens_calibration".into());
    }
    if get("has_capture_time")? == "1" {
        allowed.push("capture_time".into());
    }
    if parse_flag(get("converter_used")?, "converter_used")? != generated {
        return Err(failure(
            "prepared context converter_used disagrees with generated",
        ));
    }
    if generated {
        allowed.extend(
            [
                "converter_record",
                "converter_record_sha256",
                "converter_output_name",
                "converter_output_sha256",
                "converter_photometric",
                "converter_cleanup",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        let path = Path::new(get("converter_record")?);
        if !path.is_absolute() {
            return Err(failure("conversion record path must be absolute"));
        }
        let digest = hex64(get("converter_record_sha256")?, "converter_record_sha256")?;
        let record = records.load(path, &digest)?;
        let name = get("converter_output_name")?;
        if get("converter_photometric")? != "34892"
            || get("converter_output_sha256")? != dng_hash
            || dng.file_name().and_then(|s| s.to_str()) != Some(name)
            || record.directory.join(name) != dng
        {
            return Err(failure("conversion record output mismatch"));
        }
        let Some(output) = record.outputs.get(source) else {
            return Err(failure("conversion record does not contain prepared source"));
        };
        if output.source_hash != source_hash || output.name != name || output.output_hash != dng_hash {
            return Err(failure("conversion record does not describe prepared source/output"));
        }
    }
    require_fields(fields, &allowed)
}
/// Caches validated conversion provenance by record path during Prepare.
#[derive(Default)]
pub(crate) struct ConversionRecords {
    by_path: BTreeMap<PathBuf, ConversionRecord>,
}

struct ConversionRecord {
    digest: String,
    directory: PathBuf,
    outputs: BTreeMap<PathBuf, ConversionOutput>,
}

struct ConversionOutput {
    source_hash: String,
    name: String,
    output_hash: String,
}

impl ConversionRecords {
    fn load(&mut self, path: &Path, digest: &str) -> Result<&ConversionRecord> {
        use std::collections::btree_map::Entry;
        match self.by_path.entry(path.to_path_buf()) {
            Entry::Occupied(entry) => {
                let record = entry.into_mut();
                if record.digest != digest {
                    return Err(failure("conversion record hash mismatch"));
                }
                Ok(record)
            }
            Entry::Vacant(entry) => Ok(entry.insert(ConversionRecord::read(path, digest)?)),
        }
    }
}

impl ConversionRecord {
    fn read_sequence(bytes: &[u8], digest: &str) -> Result<Self> {
        let fields = parse_document_fields(bytes, "rawtl-sequence-conversion/1", 256 * 1024)?;
        let get = |name| field_value(&fields, name);
        let keys = [
            "executable", "version", "directory", "working_directory", "invocation", "argument",
            "source", "source_sha256", "output_name", "output_sha256",
        ].map(String::from);
        require_fields(&fields, &keys)?;
        let directory = PathBuf::from(get("directory")?);
        let scratch = directory.parent().ok_or_else(|| failure("missing Converter scratch"))?;
        let cwd = Path::new(get("working_directory")?);
        let source = PathBuf::from(get("source")?);
        let argument = Path::new(get("argument")?);
        let name = get("output_name")?;
        let valid_input = if cwd.as_os_str().is_empty() {
            argument == source
        } else {
            cwd == scratch.join("converter-input")
                && argument.parent() == Some(Path::new("."))
                && fs::canonicalize(cwd.join(argument)).ok().as_ref() == Some(&source)
        };
        if !directory.is_absolute()
            || !Path::new(get("executable")?).is_absolute()
            || !source.is_absolute()
            || Path::new(get("invocation")?) != scratch.join("converter.argv")
            || !valid_input
            || argument.file_stem().and_then(|stem| stem.to_str())
                .is_none_or(|stem| format!("{stem}.dng") != name)
        {
            return Err(failure("sequence conversion record invocation mismatch"));
        }
        let output = ConversionOutput {
            source_hash: hex64(get("source_sha256")?, "source_sha256")?,
            name: name.into(),
            output_hash: hex64(get("output_sha256")?, "output_sha256")?,
        };
        Ok(Self {
            digest: digest.into(),
            directory,
            outputs: BTreeMap::from([(source, output)]),
        })
    }

    fn read(path: &Path, digest: &str) -> Result<Self> {
        let bytes = read_document(path, 1024 * 1024)?;
        if crate::digest::sha256(&bytes) != digest {
            return Err(failure("conversion record hash mismatch"));
        }
        if bytes.starts_with(b"rawtl-sequence-conversion/1\n") {
            return Self::read_sequence(&bytes, digest);
        }
        let record = parse_document_fields(&bytes, "rawtl-conversion/1", 1024 * 1024)?;
        let r = |key: &str| field_value(&record, key);
        let count = parse_u32(r("count")?, "count")? as usize;
        let argc = parse_u32(r("argc")?, "argc")? as usize;
        if count == 0 || count > 64 || argc != count + 6 {
            return Err(failure("conversion record count/argc out of range"));
        }
        let mut keys: Vec<String> = [
            "executable",
            "version",
            "argc",
            "exit_code",
            "transcript",
            "directory",
            "count",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut args = Vec::new();
        for i in 0..argc {
            let key = format!("arg{i}");
            args.push(r(&key)?);
            keys.push(key);
        }
        if args
            .iter()
            .map(|s| s.len() + 1 + std::mem::size_of::<usize>())
            .sum::<usize>()
            > 65536
            || args[..6] != [r("executable")?, "-c", "-l", "-d", r("directory")?, "-mp"]
            || r("exit_code")? != "0"
            || r("transcript")?.len() > 65792
            || !Path::new(r("directory")?).is_absolute()
        {
            return Err(failure("conversion record invocation mismatch"));
        }
        let mut outputs = BTreeMap::new();
        let mut names = BTreeSet::new();
        for i in 0..count {
            for key in ["source", "source_sha256", "output_name", "output_sha256"] {
                keys.push(format!("item{i}_{key}"));
            }
            let src = r(&format!("item{i}_source"))?;
            let sh = hex64(r(&format!("item{i}_source_sha256"))?, "source_sha256")?;
            let output = r(&format!("item{i}_output_name"))?;
            let oh = hex64(r(&format!("item{i}_output_sha256"))?, "output_sha256")?;
            if !Path::new(src).is_absolute()
                || args[i + 6] != src
                || Path::new(src)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| format!("{s}.dng"))
                    .as_deref()
                    != Some(output)
                || !names.insert(output)
            {
                return Err(failure("conversion record source/output identity mismatch"));
            }
            if outputs.insert(PathBuf::from(src), ConversionOutput {
                source_hash: sh,
                name: output.into(),
                output_hash: oh,
            }).is_some() {
                return Err(failure("conversion record repeats a source"));
            }
        }
        require_fields(&record, &keys)?;
        Ok(Self {
            digest: digest.into(),
            directory: PathBuf::from(r("directory")?),
            outputs,
        })
    }
}

pub(crate) fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c == '%' || c.is_ascii_control() {
            out.push_str(&format!("%{:02x}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}
