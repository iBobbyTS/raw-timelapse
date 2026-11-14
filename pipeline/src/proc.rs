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
}

/// Run a child to completion with piped stdout/stderr.
///
/// `spawn_code` is used when the executable cannot be started; a nonzero exit
/// status is returned to the caller for context-specific handling.
pub fn run_capture(program: &Path, args: &[String], spawn_code: i32) -> Result<Captured> {
    use std::io::Read;
    use std::process::Stdio;
    check_stop()?;
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            io_error(
                spawn_code,
                &format!("failed to run {}", program.display()),
                err,
            )
        })?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut b = Vec::new();
        stdout.read_to_end(&mut b).map(|_| b)
    });
    let err = std::thread::spawn(move || {
        let mut b = Vec::new();
        stderr.read_to_end(&mut b).map(|_| b)
    });
    let child = std::sync::Arc::new(std::sync::Mutex::new(child));
    let status = wait_owned(&child, false, spawn_code)?;
    let stdout = out
        .join()
        .unwrap()
        .map_err(|e| io_error(spawn_code, "stdout read", e))?;
    let stderr = err
        .join()
        .unwrap()
        .map_err(|e| io_error(spawn_code, "stderr read", e))?;
    Ok(Captured {
        status,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// Captured prepare work participates in the same run cancellation and
/// transcript ownership as streaming measurement/develop processes.
pub fn run_capture_active(
    program: &Path,
    args: &[String],
    code: i32,
    log: &Path,
    active: ActiveNative,
) -> Result<Captured> {
    use std::io::Read;
    let mut process = OwnedProcess::spawn_active(program, args, log, code, false, active)?;
    let mut stdout = process.child.lock().unwrap().stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let status = process.finish(false);
    let stdout = reader
        .join()
        .map_err(|_| Error::new(code, "prepare stdout reader panicked"))?
        .map_err(|e| io_error(code, "prepare stdout read", e))?;
    let status = status?;
    let stderr = std::fs::read_to_string(log).unwrap_or_default();
    Ok(Captured {
        status,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr,
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
        None => format!("terminated by signal ({status})"),
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

/// A child owned by one pipeline stage. Every exit path reaps it and records
/// its actual status; the shared handle lets an encoder failure interrupt a
/// native producer blocked in a pipe read.
pub type ChildHandle = std::sync::Arc<std::sync::Mutex<std::process::Child>>;
#[derive(Default)]
pub struct NativeState {
    pub children: std::collections::BTreeMap<std::path::PathBuf, ChildHandle>,
    pub cancelled: bool,
}
pub type ActiveNative = std::sync::Arc<std::sync::Mutex<NativeState>>;

/// Cancelling a run closes registration before signalling every current owner.
/// Each process monitor also observes this state and owns TERM -> KILL escalation.
pub fn cancel(active: &ActiveNative) {
    let children = {
        let mut state = active.lock().unwrap();
        state.cancelled = true;
        state.children.values().cloned().collect::<Vec<_>>()
    };
    for child in children {
        terminate(&child);
    }
}

pub struct OwnedProcess {
    pub child: ChildHandle,
    log: std::path::PathBuf,
    started: Instant,
    finished: bool,
    code: i32,
    monitor: Option<std::thread::JoinHandle<()>>,
    registration: Option<(ActiveNative, std::path::PathBuf)>,
    stop_monitor: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl OwnedProcess {
    pub fn spawn_active(
        program: &Path,
        args: &[String],
        log: &Path,
        code: i32,
        pipe_input: bool,
        active: ActiveNative,
    ) -> Result<Self> {
        // Spawn and registration are atomic with respect to run cancellation.
        let mut state = active.lock().unwrap();
        if state.cancelled {
            return Err(Error::new(
                code,
                "run cancelled before process registration",
            ));
        }
        if state.children.contains_key(log) {
            return Err(Error::new(code, "duplicate process task identity"));
        }
        let mut process =
            Self::spawn_inner(program, args, log, code, pipe_input, Some(active.clone()))?;
        state
            .children
            .insert(log.to_path_buf(), process.child.clone());
        process.registration = Some((active.clone(), log.to_path_buf()));
        Ok(process)
    }
    pub fn spawn(
        program: &Path,
        args: &[String],
        log: &Path,
        code: i32,
        pipe_input: bool,
    ) -> Result<Self> {
        Self::spawn_inner(program, args, log, code, pipe_input, None)
    }
    fn spawn_inner(
        program: &Path,
        args: &[String],
        log: &Path,
        code: i32,
        pipe_input: bool,
        active: Option<ActiveNative>,
    ) -> Result<Self> {
        use std::io::Write;
        use std::process::Stdio;
        check_stop()?;
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::new(code, e.to_string()))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(log)
            .map_err(|e| Error::new(code, format!("cannot create {}: {e}", log.display())))?;
        writeln!(file, "command: {}\nstderr:", display_command(program, args))
            .map_err(|e| Error::new(code, e.to_string()))?;
        // Append mode keeps stderr and later summaries from overwriting one another.
        let stderr = std::fs::OpenOptions::new()
            .append(true)
            .open(log)
            .map_err(|e| Error::new(code, e.to_string()))?;
        let child = Command::new(program)
            .args(args)
            .stdin(if pipe_input {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(if pipe_input {
                Stdio::null()
            } else {
                Stdio::piped()
            })
            .stderr(stderr)
            .spawn()
            .map_err(|e| Error::new(code, format!("failed to start {}: {e}", program.display())))?;
        let mut process = Self {
            child: std::sync::Arc::new(std::sync::Mutex::new(child)),
            log: log.to_path_buf(),
            started: Instant::now(),
            finished: false,
            code,
            monitor: None,
            registration: None,
            stop_monitor: Default::default(),
        };
        process.note(&format!("pid: {}", process.child.lock().unwrap().id()));
        let child = process.child.clone();
        let stop = process.stop_monitor.clone();
        process.monitor = Some(std::thread::spawn(move || {
            let mut sent = None;
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                if check_stop().is_err()
                    || active.as_ref().is_some_and(|a| a.lock().unwrap().cancelled)
                {
                    if sent.is_none() {
                        terminate(&child);
                        sent = Some(Instant::now());
                    }
                    if sent.is_some_and(|at| at.elapsed() > Duration::from_secs(3)) {
                        let _ = child.lock().unwrap().kill();
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }));
        Ok(process)
    }
    pub fn abort_if_running(&mut self) {
        if !self.finished {
            let _ = self.finish(true);
        }
    }
    pub fn note(&self, text: &str) {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(&self.log) {
            let _ = writeln!(file, "{text}");
        }
    }
    pub fn finish(&mut self, abort: bool) -> Result<std::process::ExitStatus> {
        let status = wait_owned(&self.child, abort, self.code);
        if let Ok(status) = status {
            self.finished = true;
            self.stop_monitor
                .store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(monitor) = self.monitor.take() {
                let _ = monitor.join();
            }
            if let Some((active, key)) = self.registration.take() {
                active.lock().unwrap().children.remove(&key);
            }
            self.note(&format!(
                "exit: {} ({:.3}s)",
                exit_description(&status),
                self.started.elapsed().as_secs_f64()
            ));
        }
        status
    }
}
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.finish(true);
        }
    }
}

// Signal handlers only store an atomic flag. Ordinary Rust code owns signals,
// escalation and wait/reap; no allocation or mutex runs inside a handler.
static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
unsafe extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
    fn kill(pid: i32, sig: i32) -> i32;
}
extern "C" fn stopped(_: i32) {
    STOP.store(true, std::sync::atomic::Ordering::SeqCst);
}
pub fn install_stop_handlers() {
    unsafe {
        signal(2, stopped as *const () as usize);
        signal(15, stopped as *const () as usize);
    }
}
pub fn check_stop() -> Result<()> {
    if STOP.load(std::sync::atomic::Ordering::SeqCst) {
        Err(Error::new(
            130,
            "run cancelled; the run log names the retained scratch directory",
        ))
    } else {
        Ok(())
    }
}
pub fn terminate(child: &ChildHandle) {
    let mut child = child.lock().unwrap();
    if matches!(child.try_wait(), Ok(None)) {
        unsafe {
            kill(child.id() as i32, 15);
        }
    }
}
fn wait_owned(child: &ChildHandle, abort: bool, code: i32) -> Result<std::process::ExitStatus> {
    let mut cancelled = None;
    loop {
        if let Some(status) = child
            .lock()
            .unwrap()
            .try_wait()
            .map_err(|e| Error::new(code, format!("cannot reap child: {e}")))?
        {
            return Ok(status);
        }
        if (abort || check_stop().is_err()) && cancelled.is_none() {
            terminate(child);
            cancelled = Some(Instant::now());
        }
        if cancelled.is_some_and(|at| at.elapsed() > Duration::from_secs(3)) {
            let _ = child.lock().unwrap().kill();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
#[path = "owned_process_cancellation_tests.rs"]
mod owned_process_cancellation_tests;
