//! Typed run error carrying the process exit code.

use std::fmt;

/// Exit codes are part of the CLI contract (see README).
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_INPUT: i32 = 10;
pub const EXIT_OUTPUT_GUARD: i32 = 11;
pub const EXIT_OUTPUT_NOT_EMPTY: i32 = 12;
pub const EXIT_TOOL: i32 = 20;
pub const EXIT_FRAME: i32 = 30;
pub const EXIT_ENCODE: i32 = 40;
pub const EXIT_VALIDATION: i32 = 50;
pub const EXIT_PREVIEW: i32 = 60;
pub const EXIT_PUBLISH: i32 = 70;
/// Deflicker states keep legacy run_status meaning and get their own codes.
pub const EXIT_REVIEW: i32 = 80;
pub const EXIT_GAIN_PROBE: i32 = 81;
pub const EXIT_RENDER_VALIDATION: i32 = 82;
pub const EXIT_AUDIT: i32 = 83;

#[derive(Debug, Clone)]
pub struct Error {
    pub code: i32,
    pub message: String,
}

impl Error {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Error {}

pub fn io_error(code: i32, context: &str, err: std::io::Error) -> Error {
    Error::new(code, format!("{context}: {err}"))
}
