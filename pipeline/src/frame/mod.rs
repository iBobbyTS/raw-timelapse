//! Streaming native protocol v1/v2 consumption. JSON crosses validated MemoryFile
//! envelopes; pixels are read one frame at a time and never written to disk.
//!
//! Structure: `doc` decodes the prepared-context line document, `stream` owns the
//! native record protocol and process lifetime, `orchestrate` drives one frame or
//! batch through prepare/analyze/develop, and `sidecar` parses and formats frame
//! metadata envelopes.

mod contract;
mod doc;
mod orchestrate;
mod sidecar;
mod stream;

pub use doc::PreparedFrame;
#[cfg(test)]
pub use doc::PreparedRoute;
pub use orchestrate::{
    analyze_prepared_at, develop_prepared_at, prepare_active, prepare_batch_active,
    PrepareBatchItem, PrepareBatchSettings, PrepareSettings, PreparedSettings,
};
pub use sidecar::{analysis_bytes, format_wb};
#[cfg(test)]
pub use stream::Protocol;

#[cfg(test)]
pub use orchestrate::{analyze_prepared, develop_prepared, prepare};
#[cfg(test)]
pub use orchestrate::{produce_with_evs, stream_frames};
pub(crate) use doc::percent_encode;
#[cfg(test)]
pub(crate) use doc::{field_value, parse_context_fields, parse_document_fields, read_document};
#[cfg(test)]
pub(crate) use sidecar::{
    canonical_source_pair, format_axis, format_ev, sidecar_ev_matches, wb_close,
};

use crate::error::{Error, EXIT_FRAME};
use crate::json::Json;
use crate::transport::DataEnvelope;
#[cfg(test)]
use std::path::Path;
use std::time::Duration;

pub const WB_SOURCE_AS_SHOT: &str = "as-shot";
pub const WB_SOURCE_OVERRIDE: &str = "override";
pub(crate) const JSON_MEDIA_TYPE: &str = "application/json; charset=utf-8";

#[derive(Debug, Clone)]
pub struct FrameInfo {
    pub index: usize,
    pub source_name: String,
    pub camera_color: Option<Json>,
    pub highlight_recovery: Option<Json>,
    pub lens_correction: Option<Json>,
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
    pub camera_recipe: crate::color_profile::CameraRecipe,
    pub logs_dir: &'a Path,
    pub exposure_ev: Option<f64>,
    pub analysis_roi: Option<[f64; 4]>,
    pub echo: bool,
    pub echo_tag: &'a str,
    pub white_balance: crate::cli::WhiteBalance,
    pub converter: Option<&'a str>,
    pub converter_timeout: Option<u64>,
}

pub(crate) fn failure(message: impl Into<String>) -> Error {
    Error::new(EXIT_FRAME, message)
}


#[cfg(test)]
mod tests;
#[cfg(test)]
mod protocol_tests;
#[cfg(test)]
mod prepared_tests;
#[cfg(test)]
mod batch_tests;
