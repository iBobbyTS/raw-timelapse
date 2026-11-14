//! The single run log written beside the deliverable video.
//!
//! Every line after the tool-version header starts with the wall clock and the
//! monotonic elapsed time, so node order, per-frame cost and cache deletion can
//! be read in one place. The log is an operational record: it carries node and
//! frame identities, timings, deletion paths and bounded error summaries, never
//! pixel payloads, matrices, per-frame metadata documents or a deflicker
//! quality verdict.

use crate::error::{Error, Result, EXIT_PUBLISH};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct RunLog {
    file: File,
    path: PathBuf,
    started: Instant,
    #[cfg(test)]
    before_line: Option<Box<dyn FnMut(&str, &mut File)>>,
}

impl RunLog {
    /// Create the log with `create_new`: an existing log is never overwritten.
    pub fn create(path: &Path, version: &str) -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|err| {
                Error::new(
                    EXIT_PUBLISH,
                    format!("cannot create run log {}: {err}", path.display()),
                )
            })?;
        let mut log = Self {
            file,
            path: path.to_path_buf(),
            started: Instant::now(),
            #[cfg(test)]
            before_line: None,
        };
        writeln!(log.file, "rawtl-linear-video {version}").map_err(|err| log_write_error(path, err))?;
        Ok(log)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn elapsed_seconds(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    /// Append one `wall=... elapsed=... <payload>` line.
    pub fn line(&mut self, payload: &str) -> Result<()> {
        #[cfg(test)]
        if let Some(hook) = &mut self.before_line {
            hook(payload, &mut self.file);
        }
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64())
            .unwrap_or(0.0);
        writeln!(
            self.file,
            "wall={wall:.3} elapsed={:.3} {payload}",
            self.elapsed_seconds()
        )
        .map_err(|err| log_write_error(&self.path, err))
    }

    /// Best-effort append used after the video is already published: a log
    /// failure must not turn a published deliverable into a failed run.
    pub fn line_best_effort(&mut self, payload: &str) {
        if let Err(err) = self.line(payload) {
            eprintln!("warning: {}", err.message);
        }
    }

    /// Test-only boundary control; the actual append still uses `File` I/O.
    #[cfg(test)]
    pub(crate) fn before_line(&mut self, hook: impl FnMut(&str, &mut File) + 'static) {
        self.before_line = Some(Box::new(hook));
    }
}

fn log_write_error(path: &Path, err: std::io::Error) -> Error {
    Error::new(
        EXIT_PUBLISH,
        format!("cannot write run log {}: {err}", path.display()),
    )
}

/// Quote one free-text value for the log. Newlines and quotes are escaped so
/// every entry stays a single line.
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_carry_wall_and_elapsed_and_escape_free_text() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-tmp")
            .join(format!("run-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("run.log");
        let mut log = RunLog::create(&path, "9.9.9").unwrap();
        log.line(&format!("kind=note message={}", quote("two\nlines \"q\"")))
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "rawtl-linear-video 9.9.9");
        assert!(lines[1].starts_with("wall="), "{text}");
        assert!(lines[1].contains(" elapsed="), "{text}");
        assert!(lines[1].contains("kind=note"), "{text}");
        assert!(lines[1].contains("\\n") && !lines[1].contains('\n'), "{text}");
        assert_eq!(lines.len(), 2);

        let second = RunLog::create(&path, "9.9.9").err().expect("create_new must refuse");
        assert_eq!(second.code, EXIT_PUBLISH);
        let _ = std::fs::remove_dir_all(&root);
    }
}
