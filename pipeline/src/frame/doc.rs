//! Prepared-context line document: parsing, validation and encoding.
use super::failure;
use crate::error::Result;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};


/// Versioned prepared-context header. The context is a strict line document
/// written by `dng-linear-frame --prepare`; both processes parse the same
/// bytes and the native consumer revalidates every hash before decoding.
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
    pub lens_valid_area: Option<crate::lens::ValidArea>,
}

#[allow(dead_code)] // consumed by the graph scheduling integration
impl PreparedFrame {
    /// Parse and structurally validate a context document. Hash *binding* is
    /// revalidated by the native consumer against the actual files; this side
    /// checks the versioned grammar, absolute identities, route/generated
    /// consistency and file existence so a mis-wired graph fails before a
    /// native process is started.
    pub fn load(context: &Path) -> Result<Self> {
        Self::load_context(context, false)
    }

    pub fn load_early_measurement(context: &Path) -> Result<Self> {
        Self::load_context(context, true)
    }

    fn load_context(context: &Path, provisional: bool) -> Result<Self> {
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
        validate_conversion(
            &fields, &source_raw, &source_sha256, &dng_raw, &dng_sha256,
            generated && !provisional,
        )?;
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
        let bytes = read_document(path, 1024 * 1024)?;
        if crate::digest::sha256(&bytes)
            != hex64(get("converter_record_sha256")?, "converter_record_sha256")?
        {
            return Err(failure("conversion record hash mismatch"));
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
        let name = get("converter_output_name")?;
        if get("converter_photometric")? != "34892"
            || get("converter_output_sha256")? != dng_hash
            || dng.file_name().and_then(|s| s.to_str()) != Some(name)
            || Path::new(r("directory")?).join(name) != dng
        {
            return Err(failure("conversion record output mismatch"));
        }
        let mut matched = false;
        let mut sources = std::collections::BTreeSet::new();
        let mut names = std::collections::BTreeSet::new();
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
                || !sources.insert(src)
                || !names.insert(output)
            {
                return Err(failure("conversion record source/output identity mismatch"));
            }
            if Path::new(src) == source {
                if sh != source_hash || output != name || oh != dng_hash {
                    return Err(failure(
                        "conversion record does not describe prepared source/output",
                    ));
                }
                matched = true;
            }
        }
        require_fields(&record, &keys)?;
        if !matched {
            return Err(failure(
                "conversion record does not contain prepared source",
            ));
        }
    }
    require_fields(fields, &allowed)
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
