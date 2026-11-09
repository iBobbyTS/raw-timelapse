//! Subprocess helpers: captured runs and display formatting.

use crate::error::{io_error, Error, Result};
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Captured {
    pub status: std::process::ExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
}

/// Run a child to completion with piped stdout/stderr.
///
/// `spawn_code` is used when the executable cannot be started; a nonzero exit
/// status is returned to the caller for context-specific handling.
pub fn run_capture(program: &Path, args: &[String], spawn_code: i32) -> Result<Captured> {
    let start = Instant::now();
    let output = Command::new(program).args(args).output().map_err(|err| {
        io_error(
            spawn_code,
            &format!("failed to run {} {}", program.display(), args.join(" ")),
            err,
        )
    })?;
    Ok(Captured {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        duration: start.elapsed(),
    })
}

pub fn display_command(program: &Path, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(quote_arg(program.as_os_str()));
    for arg in args {
        parts.push(quote_arg(OsStr::new(arg)));
    }
    parts.join(" ")
}

fn quote_arg(arg: &OsStr) -> String {
    let text = arg.to_string_lossy();
    if text.is_empty()
        || text
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\'' | '\\'))
    {
        format!("{:?}", text)
    } else {
        text.into_owned()
    }
}

/// Last `max_chars` characters of a transcript, prefixed with an ellipsis
/// marker when truncated.
pub fn tail(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let skip = trimmed.chars().count() - max_chars;
    let tail: String = trimmed.chars().skip(skip).collect();
    format!("...{tail}")
}

pub fn exit_description(status: &std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "terminated by signal".to_string(),
    }
}

pub fn require_executable(path: &Path, flag: &str) -> Result<()> {
    let metadata = std::fs::metadata(path).map_err(|err| {
        Error::new(
            crate::error::EXIT_TOOL,
            format!(
                "required executable {flag} not found at {}: {err}",
                path.display()
            ),
        )
    })?;
    if !metadata.is_file() {
        return Err(Error::new(
            crate::error::EXIT_TOOL,
            format!("{flag} is not a regular file: {}", path.display()),
        ));
    }
    Ok(())
}
