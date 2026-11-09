//! Deterministic numeric ordering of DNG source frames.
//!
//! File names are tokenised into alternating non-digit / digit runs. Text runs
//! compare case-insensitively; digit runs compare by numeric value. A name
//! without any digit run cannot be placed in numeric order and is rejected.
//! Two names whose token lists differ only in digit spelling (`a1` vs `a01`)
//! are ambiguous and are also rejected rather than silently ordered.

use crate::error::{Error, Result, EXIT_INPUT};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub enum Token {
    Text(String),
    Number(String),
}

impl PartialEq for Token {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Token {}

impl Ord for Token {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Token::Text(a), Token::Text(b)) => a.cmp(b),
            (Token::Number(a), Token::Number(b)) => compare_numeric_text(a, b),
            (Token::Text(_), Token::Number(_)) => Ordering::Less,
            (Token::Number(_), Token::Text(_)) => Ordering::Greater,
        }
    }
}

impl PartialOrd for Token {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Compare two digit runs by numeric value without integer overflow.
fn compare_numeric_text(a: &str, b: &str) -> Ordering {
    let a = strip_leading_zeros(a);
    let b = strip_leading_zeros(b);
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

fn strip_leading_zeros(text: &str) -> &str {
    let trimmed = text.trim_start_matches('0');
    if trimmed.is_empty() {
        "0"
    } else {
        trimmed
    }
}

#[derive(Debug, Clone)]
pub struct SourceFrame {
    pub path: PathBuf,
    pub name: String,
    pub key: Vec<Token>,
}

pub fn enumerate_dng(dir: &Path) -> Result<Vec<SourceFrame>> {
    let entries = fs::read_dir(dir).map_err(|err| {
        Error::new(
            EXIT_INPUT,
            format!("cannot read input directory {}: {err}", dir.display()),
        )
    })?;

    let mut names: Vec<(String, PathBuf)> = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|err| {
            Error::new(
                EXIT_INPUT,
                format!("cannot read an entry of {}: {err}", dir.display()),
            )
        })?;
        let path = entry.path();
        let is_dng = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.eq_ignore_ascii_case("dng"))
            .unwrap_or(false);
        if !is_dng {
            continue;
        }
        let metadata = fs::metadata(&path).map_err(|err| {
            Error::new(EXIT_INPUT, format!("cannot stat {}: {err}", path.display()))
        })?;
        if !metadata.is_file() {
            continue;
        }
        let name = match entry.file_name().to_str() {
            Some(name) => name.to_string(),
            None => {
                return Err(Error::new(
                    EXIT_INPUT,
                    format!(
                        "DNG file name is not valid UTF-8 and cannot be ordered: {}",
                        path.display()
                    ),
                ))
            }
        };
        names.push((name, path));
    }

    if names.is_empty() {
        return Err(Error::new(
            EXIT_INPUT,
            format!(
                "no .dng files found in {} (extension match is case-insensitive)",
                dir.display()
            ),
        ));
    }

    order_names(names)
}

/// Deterministic ordering over `(display name, path)` pairs.
fn order_names(entries: Vec<(String, PathBuf)>) -> Result<Vec<SourceFrame>> {
    let mut frames = Vec::with_capacity(entries.len());
    let mut seen_lowercase: BTreeMap<String, String> = BTreeMap::new();

    for (name, path) in entries {
        let lowercase = name.to_ascii_lowercase();
        if let Some(previous) = seen_lowercase.insert(lowercase, name.clone()) {
            return Err(Error::new(
                EXIT_INPUT,
                format!("ambiguous frame names differing only by case: {previous:?} and {name:?}"),
            ));
        }
        let key = tokenize(&name);
        if key.iter().all(|token| matches!(token, Token::Text(_))) {
            return Err(Error::new(
                EXIT_INPUT,
                format!("frame name {name:?} has no numeric component; numeric order is undefined"),
            ));
        }
        frames.push(SourceFrame { path, name, key });
    }

    frames.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.name.cmp(&b.name)));

    for pair in frames.windows(2) {
        if pair[0].key == pair[1].key {
            return Err(Error::new(
                EXIT_INPUT,
                format!(
                    "ambiguous numeric order: {:?} and {:?} have equivalent numeric keys",
                    pair[0].name, pair[1].name
                ),
            ));
        }
    }

    Ok(frames)
}

fn tokenize(name: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut text = String::new();
    let mut digits = String::new();
    let mut in_digits = false;

    for ch in name.chars() {
        if ch.is_ascii_digit() {
            if !in_digits {
                if !text.is_empty() {
                    tokens.push(Token::Text(text.to_ascii_lowercase()));
                    text.clear();
                }
                in_digits = true;
            }
            digits.push(ch);
        } else {
            if in_digits {
                tokens.push(Token::Number(digits.clone()));
                digits.clear();
                in_digits = false;
            }
            text.push(ch);
        }
    }
    if in_digits {
        tokens.push(Token::Number(digits));
    } else if !text.is_empty() {
        tokens.push(Token::Text(text.to_ascii_lowercase()));
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn temp_root(label: &str) -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(format!("order-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        root
    }

    fn touch(dir: &Path, name: &str) {
        File::create(dir.join(name)).expect("create fixture file");
    }

    #[test]
    fn sorts_shuffled_names_numerically() {
        let dir = temp_root("shuffled");
        for name in [
            "TIMELAPSE_0710.DNG",
            "TIMELAPSE_0709.DNG",
            "TIMELAPSE_0747.DNG",
            "TIMELAPSE_0711.dng",
            "notes.txt",
        ] {
            touch(&dir, name);
        }
        let frames = enumerate_dng(&dir).expect("enumerate");
        let names: Vec<&str> = frames.iter().map(|frame| frame.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "TIMELAPSE_0709.DNG",
                "TIMELAPSE_0710.DNG",
                "TIMELAPSE_0711.dng",
                "TIMELAPSE_0747.DNG"
            ]
        );
    }

    #[test]
    fn rejects_names_without_a_numeric_component() {
        let error = order_names(vec![
            ("TIMELAPSE_0001.DNG".to_string(), PathBuf::from("a")),
            ("snapshot.DNG".to_string(), PathBuf::from("b")),
        ])
        .expect_err("must reject");
        assert_eq!(error.code, EXIT_INPUT);
        assert!(error.message.contains("no numeric component"), "{error}");
    }

    #[test]
    fn rejects_equivalent_numeric_keys() {
        let error = order_names(vec![
            ("IMG_1.DNG".to_string(), PathBuf::from("a")),
            ("IMG_01.DNG".to_string(), PathBuf::from("b")),
        ])
        .expect_err("must reject");
        assert_eq!(error.code, EXIT_INPUT);
        assert!(error.message.contains("ambiguous numeric order"), "{error}");
    }

    #[test]
    fn rejects_case_only_duplicates() {
        let error = order_names(vec![
            ("IMG_0001.DNG".to_string(), PathBuf::from("a")),
            ("img_0001.dng".to_string(), PathBuf::from("b")),
        ])
        .expect_err("must reject");
        assert_eq!(error.code, EXIT_INPUT);
        assert!(error.message.contains("differing only by case"), "{error}");
    }

    #[test]
    fn sorts_in_memory_names_across_digit_boundaries() {
        let frames = order_names(vec![
            ("clip_10.DNG".to_string(), PathBuf::from("10")),
            ("clip_2.DNG".to_string(), PathBuf::from("2")),
            ("clip_1.DNG".to_string(), PathBuf::from("1")),
            ("clip_100.DNG".to_string(), PathBuf::from("100")),
        ])
        .expect("order");
        let names: Vec<&str> = frames.iter().map(|frame| frame.name.as_str()).collect();
        assert_eq!(
            names,
            ["clip_1.DNG", "clip_2.DNG", "clip_10.DNG", "clip_100.DNG"]
        );
    }

    #[test]
    fn rejects_empty_directory() {
        let dir = temp_root("empty");
        let error = enumerate_dng(&dir).expect_err("must reject");
        assert_eq!(error.code, EXIT_INPUT);
    }

    #[test]
    fn compares_large_numbers_without_overflow() {
        assert_eq!(
            compare_numeric_text("9999999999999999999999", "10000000000000000000000"),
            Ordering::Less
        );
        assert_eq!(compare_numeric_text("007", "7"), Ordering::Equal);
    }
}
