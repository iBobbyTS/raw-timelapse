//! Per-frame production: invoke the accepted native producer, verify its
//! outputs, and enforce the sequence-frozen white balance.

use crate::error::{Error, Result, EXIT_FRAME};
use crate::json::{self, Json};
use crate::order::SourceFrame;
use crate::proc;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const WB_SOURCE_AS_SHOT: &str = "as-shot";
pub const WB_SOURCE_OVERRIDE: &str = "override";

#[derive(Debug, Clone)]
pub struct FrameInfo {
    pub index: usize,
    pub source_name: String,
    pub source_sha256: Option<String>,
    pub width: u32,
    pub height: u32,
    pub applied_wb: [f64; 3],
    pub wb_source: String,
    pub as_shot_neutral: [f64; 3],
    pub observed_min: Option<f64>,
    pub observed_max: Option<f64>,
    pub quantization: Option<Json>,
    pub png_relative: String,
    pub metadata_relative: String,
    pub log_relative: String,
    pub duration: Duration,
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

pub fn produce(
    native_tool: &Path,
    sources: &[SourceFrame],
    frames_dir: &Path,
    logs_dir: &Path,
    converter: Option<&str>,
    converter_timeout: Option<u64>,
) -> Result<FrameSequence> {
    fs::create_dir_all(frames_dir).map_err(|err| {
        Error::new(
            EXIT_FRAME,
            format!(
                "cannot create frame directory {}: {err}",
                frames_dir.display()
            ),
        )
    })?;
    fs::create_dir_all(logs_dir).map_err(|err| {
        Error::new(
            EXIT_FRAME,
            format!("cannot create log directory {}: {err}", logs_dir.display()),
        )
    })?;

    let total = sources.len();
    let mut infos = Vec::with_capacity(total);
    let mut reference_wb: Option<[f64; 3]> = None;
    let mut width = 0u32;
    let mut height = 0u32;
    let run_start = std::time::Instant::now();

    for (offset, source) in sources.iter().enumerate() {
        let index = offset + 1;
        let stem = format!("frame_{index:06}");
        let png = frames_dir.join(format!("{stem}.png"));
        let metadata = frames_dir.join(format!("{stem}.json"));
        let log_path = logs_dir.join(format!("{stem}.log"));

        let mut args: Vec<String> = vec![
            source.path.display().to_string(),
            png.display().to_string(),
            metadata.display().to_string(),
        ];
        if let Some(reference) = reference_wb {
            args.push("--white-balance".to_string());
            args.push(format_wb(reference));
        }
        if let Some(converter_path) = converter {
            args.push("--converter".to_string());
            args.push(converter_path.to_string());
        }
        if let Some(seconds) = converter_timeout {
            args.push("--converter-timeout".to_string());
            args.push(seconds.to_string());
        }

        let command = proc::display_command(native_tool, &args);
        let captured = proc::run_capture(native_tool, &args, EXIT_FRAME)?;
        let log_text = format!(
            "command: {command}\nexit: {} ({:.3}s)\nstdout:\n{}\nstderr:\n{}\n",
            proc::exit_description(&captured.status),
            captured.duration.as_secs_f64(),
            captured.stdout,
            captured.stderr
        );
        write_log(&log_path, &log_text)?;

        if !captured.status.success() {
            let stderr = captured.stderr.trim();
            let detail = if stderr.is_empty() {
                "(no stderr)".to_string()
            } else {
                proc::tail(stderr, 1200)
            };
            return Err(Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) failed: {}; stderr: {detail}; \
                     frame outputs and transcript retained at {} and {}; no video was produced",
                    source.name,
                    proc::exit_description(&captured.status),
                    frames_dir.display(),
                    log_path.display()
                ),
            ));
        }

        for (path, kind) in [(&png, "PNG"), (&metadata, "metadata JSON")] {
            match fs::metadata(path) {
                Ok(metadata) if metadata.is_file() && metadata.len() > 0 => {}
                Ok(_) => {
                    return Err(Error::new(
                        EXIT_FRAME,
                        format!(
                            "frame {index}/{total} ({}) reported success but {kind} is empty: {}",
                            source.name,
                            path.display()
                        ),
                    ))
                }
                Err(err) => {
                    return Err(Error::new(
                        EXIT_FRAME,
                        format!(
                            "frame {index}/{total} ({}) reported success but {kind} is missing ({}): {err}",
                            source.name,
                            path.display()
                        ),
                    ))
                }
            }
        }

        let text = fs::read_to_string(&metadata).map_err(|err| {
            Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) metadata is unreadable ({}): {err}",
                    source.name,
                    metadata.display()
                ),
            )
        })?;
        let sidecar = json::parse(&text).map_err(|err| {
            Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) metadata is not valid JSON ({}): {err}",
                    source.name,
                    metadata.display()
                ),
            )
        })?;

        let frame_width = sidecar
            .get("width")
            .and_then(Json::as_u32)
            .ok_or_else(|| missing_field(index, total, &source.name, "width"))?;
        let frame_height = sidecar
            .get("height")
            .and_then(Json::as_u32)
            .ok_or_else(|| missing_field(index, total, &source.name, "height"))?;
        if frame_width == 0 || frame_height == 0 {
            return Err(Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) reports an empty image {}x{}",
                    source.name, frame_width, frame_height
                ),
            ));
        }
        if index == 1 {
            width = frame_width;
            height = frame_height;
        } else if frame_width != width || frame_height != height {
            return Err(Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) has dimensions {}x{} but the sequence is {}x{}",
                    source.name, frame_width, frame_height, width, height
                ),
            ));
        }

        let schema_version = sidecar
            .get("schema_version")
            .and_then(Json::as_u64)
            .ok_or_else(|| missing_field(index, total, &source.name, "schema_version"))?;
        if schema_version != 1 {
            return Err(Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) has unsupported metadata schema_version {schema_version}",
                    source.name
                ),
            ));
        }

        let applied_wb = parse_wb(
            &sidecar,
            "applied_white_balance",
            index,
            total,
            &source.name,
        )?;
        let as_shot_neutral = parse_wb(&sidecar, "as_shot_neutral", index, total, &source.name)?;
        let wb_source = sidecar
            .get("white_balance_source")
            .and_then(Json::as_str)
            .ok_or_else(|| missing_field(index, total, &source.name, "white_balance_source"))?
            .to_string();

        if index == 1 {
            if wb_source != WB_SOURCE_AS_SHOT {
                return Err(Error::new(
                    EXIT_FRAME,
                    format!(
                        "frame 1/{total} ({}) should use its as-shot white balance, metadata reports {wb_source:?}",
                        source.name
                    ),
                ));
            }
            reference_wb = Some(applied_wb);
        } else {
            let reference = reference_wb.expect("reference set by first frame");
            if wb_source != WB_SOURCE_OVERRIDE {
                return Err(Error::new(
                    EXIT_FRAME,
                    format!(
                        "frame {index}/{total} ({}) should use the frozen override white balance, metadata reports {wb_source:?}",
                        source.name
                    ),
                ));
            }
            if !wb_close(applied_wb, reference) {
                return Err(Error::new(
                    EXIT_FRAME,
                    format!(
                        "frame {index}/{total} ({}) applied white balance {:?} differs from the frozen reference {:?}",
                        source.name, applied_wb, reference
                    ),
                ));
            }
        }

        let source_field = sidecar
            .get("source")
            .and_then(Json::as_str)
            .ok_or_else(|| missing_field(index, total, &source.name, "source"))?;
        // The native producer resolves the input path (symlinks included), so
        // compare canonical identities rather than the literal argument text.
        let (expected_source, reported_source) =
            canonical_source_pair(source_field, &source.path).map_err(|err| {
                Error::new(
                    EXIT_FRAME,
                    format!(
                        "frame {index}/{total} ({}) metadata source {source_field:?} cannot be resolved against the requested input: {err}",
                        source.name
                    ),
                )
            })?;
        if reported_source != expected_source {
            return Err(Error::new(
                EXIT_FRAME,
                format!(
                    "frame {index}/{total} ({}) metadata source {source_field:?} does not resolve to the requested input {}",
                    source.name,
                    expected_source.display()
                ),
            ));
        }

        let source_sha256 = sidecar
            .get("source_sha256")
            .and_then(Json::as_str)
            .map(str::to_string);
        let observed_min = sidecar.get("observed_scene_min").and_then(Json::as_f64);
        let observed_max = sidecar.get("observed_scene_max").and_then(Json::as_f64);
        let quantization = sidecar.get("quantization").cloned();

        println!(
            "frame {index:>3}/{total} {} -> {} ({:.2}s, WB {})",
            source.name,
            png.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?"),
            captured.duration.as_secs_f64(),
            wb_source
        );

        infos.push(FrameInfo {
            index,
            source_name: source.name.clone(),
            source_sha256,
            width: frame_width,
            height: frame_height,
            applied_wb,
            wb_source,
            as_shot_neutral,
            observed_min,
            observed_max,
            quantization,
            png_relative: format!("frames/{stem}.png"),
            metadata_relative: format!("frames/{stem}.json"),
            log_relative: format!("logs/{stem}.log"),
            duration: captured.duration,
        });
    }

    let reference_wb = reference_wb.expect("at least one frame");
    let reference_frame = infos
        .first()
        .map(|frame| frame.source_name.clone())
        .unwrap_or_default();

    Ok(FrameSequence {
        frames: infos,
        width,
        height,
        reference_wb,
        reference_frame,
        total_duration: run_start.elapsed(),
    })
}

fn write_log(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text).map_err(|err| {
        Error::new(
            EXIT_FRAME,
            format!("cannot write frame transcript {}: {err}", path.display()),
        )
    })
}

fn canonical_source_pair(reported: &str, expected: &Path) -> std::io::Result<(PathBuf, PathBuf)> {
    Ok((fs::canonicalize(expected)?, fs::canonicalize(reported)?))
}

fn missing_field(index: usize, total: usize, name: &str, field: &str) -> Error {
    Error::new(
        EXIT_FRAME,
        format!("frame {index}/{total} ({name}) metadata is missing or invalid field {field:?}"),
    )
}

fn parse_wb(
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

fn wb_close(a: [f64; 3], b: [f64; 3]) -> bool {
    // The native tool echoes the decimal values it received; the shortest
    // round-tripping decimal representation preserves the exact f64 bits.
    // Requiring exact equality catches any silent per-frame WB drift.
    a.iter().zip(b.iter()).all(|(left, right)| left == right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_white_balance_without_precision_loss() {
        let wb = [0.605080831408776, 1.0, 0.48250460405156537];
        let text = format_wb(wb);
        assert_eq!(text.matches(',').count(), 2);
        let parsed: Vec<f64> = text.split(',').map(|v| v.parse().unwrap()).collect();
        assert_eq!(parsed, wb.to_vec());
    }

    #[test]
    fn white_balance_comparison_is_strict() {
        let reference = [0.605080831408776, 1.0, 0.48250460405156537];
        assert!(wb_close(reference, reference));
        assert!(!wb_close(
            reference,
            [0.605080831408777, 1.0, 0.48250460405156537]
        ));
    }

    #[cfg(unix)]
    #[test]
    fn canonical_source_comparison_resolves_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(format!("frame-source-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");
        let real = dir.join("real.DNG");
        fs::write(&real, b"data").expect("write real file");
        let link = dir.join("link.DNG");
        symlink(&real, &link).expect("create symlink");
        let (expected, reported) =
            canonical_source_pair(link.to_str().expect("utf8"), &real).expect("canonicalize");
        assert_eq!(expected, reported);
        assert_eq!(expected, fs::canonicalize(&real).expect("canonical"));
    }
}
