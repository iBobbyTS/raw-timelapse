//! Frame metadata parsing, transport envelopes and white-balance formatting.
use super::{failure, FrameInfo, JSON_MEDIA_TYPE};
use crate::error::{Error, Result, EXIT_FRAME};
use crate::json::{self, Json};
use crate::transport::{
    self, DataEnvelope, MemoryFile, MetadataDoc, Payload, Provenance, ANALYSIS_SIDECAR,
    FRAME_METADATA,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;


pub(crate) fn parse_json(bytes: &[u8]) -> Result<Json> {
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
pub(crate) fn frame_metadata_envelope(
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

pub(crate) fn sidecar_ev_matches(reported: f64, requested: f64) -> bool {
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

pub(crate) fn canonical_source_pair(reported: &str, expected: &Path) -> std::io::Result<(PathBuf, PathBuf)> {
    Ok((fs::canonicalize(expected)?, fs::canonicalize(reported)?))
}

pub(crate) fn missing_field(index: usize, total: usize, name: &str, field: &str) -> Error {
    Error::new(
        EXIT_FRAME,
        format!("frame {index}/{total} ({name}) metadata is missing or invalid field {field:?}"),
    )
}

pub(crate) fn parse_wb(
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

pub(crate) fn format_ev(ev: f64) -> String {
    format!("{ev}")
}

pub(crate) fn format_axis(value: f64) -> String {
    format!("{value}")
}

pub(crate) fn wb_close(a: [f64; 3], b: [f64; 3]) -> bool {
    // The native tool echoes the decimal values it received; the shortest
    // round-tripping decimal representation preserves the exact f64 bits.
    // Requiring exact equality catches any silent per-frame WB drift.
    a.iter().zip(b.iter()).all(|(left, right)| left == right)
}
