//! Legacy V4 exposure semantics, using f64 and deterministic std-only solvers.
//! All public operations validate dimensions and return errors for malformed data.
pub mod estimation;
pub mod graph;
pub mod measurement;
pub mod pairwise;
pub mod photometry;
mod stats;
pub mod trends;
pub mod validation;
pub type Result<T> = std::result::Result<T, String>;
pub use estimation::{
    estimate_exposure, ExposureConfig, ExposureResult, ExposureStep, FrameEstimate,
};
pub use measurement::FrameMeasurement;
#[cfg(test)]
mod tests;
