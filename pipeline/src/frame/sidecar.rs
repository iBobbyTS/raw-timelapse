//! Frame metadata parsing, transport envelopes and white-balance formatting.
use super::{failure, JSON_MEDIA_TYPE};
#[cfg(test)]
use super::FrameInfo;
use crate::deflicker::{measurement::AnalysisSidecar, FrameMeasurement};
use crate::error::{Error, Result, EXIT_FRAME};
use crate::json::{self, Json};
use crate::transport::{
    DataEnvelope, MemoryFile, MetadataDoc, Payload, Provenance, ANALYSIS_SIDECAR,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;


pub(crate) fn parse_json(bytes: &[u8]) -> Result<Json> {
    let text =
        std::str::from_utf8(bytes).map_err(|e| failure(format!("invalid JSON UTF-8: {e}")))?;
    json::parse(text).map_err(|e| failure(format!("invalid JSON: {e}")))
}
pub(crate) fn parse_analysis(bytes: &[u8], expected: Option<&str>, ev: f64)
    -> Result<FrameMeasurement>
{
    let sidecar = parse_json(bytes)?;
    let hash = sidecar
        .get("source_sha256")
        .and_then(Json::as_str)
        .ok_or_else(|| {
            failure("analysis sidecar source_sha256 does not match the frame metadata")
        })?;
    if hash != expected.unwrap_or_default() {
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
    let measurement = AnalysisSidecar::from_json(&sidecar).map_err(failure)?.measurement;
    Ok(measurement)
}

pub fn analysis_sidecar_envelope(
    bytes: Arc<[u8]>,
    expected: Option<&str>,
    ev: f64,
) -> Result<(DataEnvelope, FrameMeasurement)> {
    let measurement = parse_analysis(&bytes, expected, ev)?;
    let hash = expected.ok_or_else(|| failure("missing analysis source hash"))?;
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
    let envelope = DataEnvelope::new(
        ANALYSIS_SIDECAR,
        Payload::MemoryFile(file),
        metadata,
        provenance,
    )
    .map_err(|e| failure(e.to_string()))?;
    Ok((envelope, measurement))
}
#[cfg(test)]
pub fn analysis_bytes(frame: &FrameInfo) -> Option<&[u8]> {
    match frame.analysis_envelope.as_ref()?.payload() {
        Payload::MemoryFile(file) => Some(file.bytes()),
        _ => None,
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
