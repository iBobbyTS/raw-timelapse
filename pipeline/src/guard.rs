//! Input/output path validation and disjointness guards.

use crate::error::{Error, Result, EXIT_INPUT, EXIT_OUTPUT_GUARD, EXIT_OUTPUT_NOT_EMPTY};
use std::fs;
use std::path::{Path, PathBuf};

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

/// Create the run output directory if needed and return its canonical path.
/// The caller must still check emptiness before writing.
pub fn prepare_output(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        let metadata = fs::metadata(path).map_err(|err| {
            Error::new(
                EXIT_OUTPUT_GUARD,
                format!("cannot stat output {}: {err}", path.display()),
            )
        })?;
        if !metadata.is_dir() {
            return Err(Error::new(
                EXIT_OUTPUT_GUARD,
                format!(
                    "output path exists and is not a directory: {}",
                    path.display()
                ),
            ));
        }
    } else {
        fs::create_dir_all(path).map_err(|err| {
            Error::new(
                EXIT_OUTPUT_GUARD,
                format!("cannot create output directory {}: {err}", path.display()),
            )
        })?;
    }
    fs::canonicalize(path).map_err(|err| {
        Error::new(
            EXIT_OUTPUT_GUARD,
            format!("cannot resolve output directory {}: {err}", path.display()),
        )
    })
}

pub fn ensure_output_empty(output: &Path) -> Result<()> {
    let mut entries = fs::read_dir(output).map_err(|err| {
        Error::new(
            EXIT_OUTPUT_GUARD,
            format!("cannot read output directory {}: {err}", output.display()),
        )
    })?;
    if let Some(entry) = entries.next() {
        let entry = entry.map_err(|err| {
            Error::new(
                EXIT_OUTPUT_GUARD,
                format!(
                    "cannot inspect output directory {}: {err}",
                    output.display()
                ),
            )
        })?;
        return Err(Error::new(
            EXIT_OUTPUT_NOT_EMPTY,
            format!(
                "output directory is not empty (found {}); choose a fresh run directory",
                entry.path().display()
            ),
        ));
    }
    Ok(())
}

/// Reject output == input and either directory containing the other, in both
/// directions. Inputs must be canonical.
pub fn ensure_disjoint(input: &Path, output: &Path) -> Result<()> {
    if input == output {
        return Err(Error::new(
            EXIT_OUTPUT_GUARD,
            format!(
                "output directory must differ from the input/material directory ({})",
                input.display()
            ),
        ));
    }
    if output.starts_with(input) {
        return Err(Error::new(
            EXIT_OUTPUT_GUARD,
            format!(
                "output directory {} is inside the input/material directory {}",
                output.display(),
                input.display()
            ),
        ));
    }
    if input.starts_with(output) {
        return Err(Error::new(
            EXIT_OUTPUT_GUARD,
            format!(
                "input/material directory {} is inside the output directory {}",
                input.display(),
                output.display()
            ),
        ));
    }
    Ok(())
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
    fn rejects_overlapping_directories() {
        let root = temp_root("overlap");
        let input = root.join("material");
        let nested = input.join("out");
        fs::create_dir_all(&nested).expect("create nested");
        let input = fs::canonicalize(&input).expect("canonical input");
        let nested = fs::canonicalize(&nested).expect("canonical nested");

        assert!(ensure_disjoint(&input, &input).is_err());
        assert!(ensure_disjoint(&input, &nested).is_err());
        assert!(ensure_disjoint(&nested, &input).is_err());
    }

    #[test]
    fn accepts_separate_directories() {
        let root = temp_root("separate");
        let input = root.join("material");
        let output = root.join("run");
        fs::create_dir_all(&input).expect("create input");
        fs::create_dir_all(&output).expect("create output");
        let input = fs::canonicalize(&input).expect("canonical");
        let output = fs::canonicalize(&output).expect("canonical");
        assert!(ensure_disjoint(&input, &output).is_ok());
    }

    #[test]
    fn rejects_non_empty_output() {
        let root = temp_root("nonempty");
        let output = prepare_output(&root).expect("prepare");
        fs::write(output.join("existing.txt"), b"x").expect("write");
        let error = ensure_output_empty(&output).expect_err("must reject");
        assert_eq!(error.code, EXIT_OUTPUT_NOT_EMPTY);
    }

    #[test]
    fn accepts_empty_output() {
        let root = temp_root("empty");
        let output = prepare_output(&root).expect("prepare");
        assert!(ensure_output_empty(&output).is_ok());
    }
}
