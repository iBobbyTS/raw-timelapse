//! Input/output path validation for the single video and its run log.

use crate::error::{Error, Result, EXIT_INPUT, EXIT_OUTPUT_GUARD, EXIT_OUTPUT_NOT_EMPTY};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn canonical_input(path: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path).map_err(|err| {
        Error::new(
            EXIT_INPUT,
            format!(
                "input directory {} is not accessible: {err}",
                path.display()
            ),
        )
    })?;
    let metadata = fs::metadata(&canonical).map_err(|err| {
        Error::new(
            EXIT_INPUT,
            format!("cannot stat input {}: {err}", canonical.display()),
        )
    })?;
    if !metadata.is_dir() {
        return Err(Error::new(
            EXIT_INPUT,
            format!("input path is not a directory: {}", canonical.display()),
        ));
    }
    Ok(canonical)
}

/// Resolve existing parents (including symlinks) and append missing components
/// without creating anything, so disjointness can be checked before writes.
pub fn canonical_directory(path: &Path) -> Result<PathBuf> {
    let resolve_error = |err| {
        Error::new(
            EXIT_OUTPUT_GUARD,
            format!("cannot resolve output directory {}: {err}", path.display()),
        )
    };
    if path.as_os_str().is_empty() {
        return Err(Error::new(
            EXIT_OUTPUT_GUARD,
            "output path has no parent directory",
        ));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(resolve_error)?.join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        if component == Component::ParentDir {
            resolved.pop();
            continue;
        }
        resolved.push(component.as_os_str());
        match fs::canonicalize(&resolved) {
            Ok(canonical) => {
                if !canonical.is_dir() {
                    return Err(Error::new(
                        EXIT_OUTPUT_GUARD,
                        format!(
                            "output directory component exists and is not a directory: {}",
                            resolved.display()
                        ),
                    ));
                }
                resolved = canonical;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(resolve_error(err)),
        }
    }
    Ok(resolved)
}

/// Resolve one deliverable file path (video or log) against its parent
/// directory without creating it.
pub fn canonical_artifact(path: &Path) -> Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        Error::new(
            EXIT_OUTPUT_GUARD,
            format!("output file path has no file name: {}", path.display()),
        )
    })?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => std::env::current_dir().map_err(|err| {
            Error::new(
                EXIT_OUTPUT_GUARD,
                format!("cannot resolve the current directory: {err}"),
            )
        })?,
    };
    let resolved = canonical_directory(&parent)?.join(file_name);
    if resolved.exists() && !resolved.is_file() {
        return Err(Error::new(
            EXIT_OUTPUT_GUARD,
            format!(
                "output path exists and is not a regular file: {}",
                resolved.display()
            ),
        ));
    }
    Ok(resolved)
}

/// Create the missing parent directories of one deliverable file.
pub fn prepare_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        Error::new(
            EXIT_OUTPUT_GUARD,
            format!("output path has no parent directory: {}", path.display()),
        )
    })?;
    fs::create_dir_all(parent).map_err(|err| {
        Error::new(
            EXIT_OUTPUT_GUARD,
            format!("cannot create output directory {}: {err}", parent.display()),
        )
    })
}

/// Refuse to overwrite an existing deliverable (video or log).
pub fn ensure_absent(artifacts: &[&Path]) -> Result<()> {
    for artifact in artifacts {
        if artifact.exists() {
            return Err(Error::new(
                EXIT_OUTPUT_NOT_EMPTY,
                format!(
                    "refusing to overwrite existing {}; choose a new video file name or \
                     remove the previous run's video and log first",
                    artifact.display()
                ),
            ));
        }
    }
    Ok(())
}

/// Neither deliverable may live inside the read-only material directory.
pub fn ensure_outside_input(input: &Path, artifacts: &[&Path]) -> Result<()> {
    for artifact in artifacts {
        if artifact.starts_with(input) {
            return Err(Error::new(
                EXIT_OUTPUT_GUARD,
                format!(
                    "output {} is inside the input/material directory {}; choose a path \
                     outside the source directory",
                    artifact.display(),
                    input.display()
                ),
            ));
        }
    }
    Ok(())
}

/// A run-specific, hidden scratch directory beside the video: same filesystem
/// as the deliverable (so publication is one rename), never reused between
/// runs, and outside the deliverable names.
pub fn create_scratch(video: &Path) -> Result<PathBuf> {
    let parent = video.parent().ok_or_else(|| {
        Error::new(
            EXIT_OUTPUT_GUARD,
            format!("output path has no parent directory: {}", video.display()),
        )
    })?;
    let stem = video
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("run");
    let safe: String = stem
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let scratch = parent.join(format!(
        ".rawtl-{safe}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&scratch).map_err(|err| {
        Error::new(
            EXIT_OUTPUT_GUARD,
            format!(
                "cannot create run scratch directory {}: {err}",
                scratch.display()
            ),
        )
    })?;
    Ok(scratch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(format!("guard-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        root
    }

    #[test]
    fn artifact_resolution_appends_a_file_to_a_missing_parent() {
        let root = temp_root("artifact");
        let missing = root.join("missing-parent/run/timelapse.mov");
        let resolved = canonical_artifact(&missing).expect("resolve missing parent");
        assert!(!root.join("missing-parent").exists());
        assert!(resolved.ends_with("timelapse.mov"));
        prepare_parent(&resolved).expect("create parent");
        assert!(root.join("missing-parent/run").is_dir());
    }

    #[test]
    fn artifact_inside_input_is_rejected() {
        let root = temp_root("overlap");
        let input = root.join("material");
        fs::create_dir_all(&input).expect("create input");
        let input = canonical_input(&input).expect("canonical input");
        let video = canonical_artifact(&input.join("timelapse.mov")).expect("resolve");
        let log = canonical_artifact(&input.join("timelapse.log")).expect("resolve");
        assert!(ensure_outside_input(&input, &[&video]).is_err());
        assert!(ensure_outside_input(&input, &[&log]).is_err());
        let sibling = canonical_artifact(&root.join("timelapse.mov")).expect("resolve");
        assert!(ensure_outside_input(&input, &[&sibling]).is_ok());
    }

    #[test]
    fn existing_deliverables_are_never_overwritten() {
        let root = temp_root("absent");
        let video = root.join("timelapse.mov");
        fs::write(&video, b"existing").expect("write");
        let error = ensure_absent(&[&video]).expect_err("must refuse");
        assert_eq!(error.code, EXIT_OUTPUT_NOT_EMPTY);
        assert!(error.message.contains("refusing to overwrite"), "{error:?}");
        let log = root.join("timelapse.log");
        assert!(ensure_absent(&[&video, &log]).is_err());
        assert!(ensure_absent(&[&log]).is_ok());
    }

    #[test]
    fn scratch_directories_are_run_specific_and_beside_the_video() {
        let root = temp_root("scratch");
        let video = root.join("run/timelapse.mov");
        prepare_parent(&video).expect("create parent");
        let first = create_scratch(&video).expect("scratch");
        let second = create_scratch(&video).expect("scratch");
        assert_ne!(first, second, "scratch must never be reused");
        assert_eq!(first.parent(), video.parent());
        assert!(first.is_dir() && second.is_dir());
        let name = first.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(".rawtl-timelapse-"), "{name}");
    }
}
