//! Command-line parsing for `rawtl-linear-video`.

use crate::error::{Error, Result, EXIT_USAGE};
use std::path::PathBuf;

pub const DEFAULT_NATIVE_TOOL: &str = "native/build/dng-linear-frame";
pub const DEFAULT_FFMPEG: &str = "/opt/homebrew/opt/ffmpeg-full/bin/ffmpeg";
pub const DEFAULT_FFPROBE: &str = "/opt/homebrew/opt/ffmpeg-full/bin/ffprobe";
pub const LEGACY_FPS_PROVENANCE: &str =
    "legacy/python/src/rawtl/cli.py:91 (argparse default Fraction(30))";

pub const USAGE: &str = "\
rawtl-linear-video — DNG frame sequence -> scene-linear ACEScg FFV1 video + sRGB preview

USAGE:
    rawtl-linear-video --input <DIR> --output <DIR> [OPTIONS]

REQUIRED:
    --input <DIR>              Directory of .dng frames (read only, not overlapping output)
    --output <DIR>             Run directory; must be absent or empty

OPTIONS:
    --fps <N[/D]>              Frame rate as an integer or rational (default: 30)
    --dng-linear-frame <PATH>  Native single-frame producer (default: native/build/dng-linear-frame)
    --ffmpeg <PATH>            ffmpeg executable (default: /opt/homebrew/opt/ffmpeg-full/bin/ffmpeg)
    --ffprobe <PATH>           ffprobe executable (default: /opt/homebrew/opt/ffmpeg-full/bin/ffprobe)
    --converter <PATH>         Adobe DNG Converter override passed to the native tool
    --converter-timeout <SEC>  Converter timeout override (1-3600 seconds)
    -h, --help                 Print this help
    --version                  Print version

OUTPUT LAYOUT:
    <output>/frames/frame_%06d.png|json   per-frame RGB16 scene-linear ACEScg frames + metadata
    <output>/logs/                        per-frame and encoder transcripts
    <output>/linear-acescg-16bit-ffv1.mkv FFV1 gbrp16le lossless linear video
    <output>/srgb-preview-h264.mp4        display-referred sRGB/H.264 review preview
    <output>/manifest.json                run provenance and validation record
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fps {
    pub num: u64,
    pub den: u64,
}

impl Fps {
    pub const DEFAULT: Fps = Fps { num: 30, den: 1 };

    pub fn parse(text: &str) -> Result<Fps> {
        let (num, den) = match text.split_once('/') {
            Some((num, den)) => (num, den),
            None => (text, "1"),
        };
        let num: u64 = num.parse().map_err(|_| {
            Error::new(
                EXIT_USAGE,
                format!("invalid --fps value {text:?}: numerator must be a positive integer"),
            )
        })?;
        let den: u64 = den.parse().map_err(|_| {
            Error::new(
                EXIT_USAGE,
                format!("invalid --fps value {text:?}: denominator must be a positive integer"),
            )
        })?;
        if num == 0 || den == 0 {
            return Err(Error::new(
                EXIT_USAGE,
                format!("invalid --fps value {text:?}: frame rate must be positive"),
            ));
        }
        // Match the frozen legacy contract: positive rational <= 1000.
        if (num as u128) > 1000u128 * (den as u128) {
            return Err(Error::new(
                EXIT_USAGE,
                format!("invalid --fps value {text:?}: frame rate must not exceed 1000"),
            ));
        }
        Ok(Fps { num, den })
    }

    pub fn arg(self) -> String {
        format!("{}/{}", self.num, self.den)
    }

    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub input: PathBuf,
    pub output: PathBuf,
    pub fps: Fps,
    pub fps_explicit: bool,
    pub native_tool: PathBuf,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub converter: Option<String>,
    pub converter_timeout: Option<u64>,
}

#[derive(Debug)]
pub enum Parsed {
    Run(Options),
    Help,
    Version,
}

pub fn parse(args: &[String]) -> Result<Parsed> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut fps = Fps::DEFAULT;
    let mut fps_explicit = false;
    let mut native_tool = PathBuf::from(DEFAULT_NATIVE_TOOL);
    let mut ffmpeg = PathBuf::from(DEFAULT_FFMPEG);
    let mut ffprobe = PathBuf::from(DEFAULT_FFPROBE);
    let mut converter: Option<String> = None;
    let mut converter_timeout: Option<u64> = None;

    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "-h" || arg == "--help" {
            return Ok(Parsed::Help);
        }
        if arg == "--version" {
            return Ok(Parsed::Version);
        }
        let (flag, inline_value) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (arg.as_str(), None),
        };
        match flag {
            "--input" => {
                input = Some(PathBuf::from(take_value(
                    args,
                    &mut index,
                    flag,
                    inline_value,
                )?))
            }
            "--output" => {
                output = Some(PathBuf::from(take_value(
                    args,
                    &mut index,
                    flag,
                    inline_value,
                )?))
            }
            "--fps" => {
                fps = Fps::parse(&take_value(args, &mut index, flag, inline_value)?)?;
                fps_explicit = true;
            }
            "--dng-linear-frame" => {
                native_tool = PathBuf::from(take_value(args, &mut index, flag, inline_value)?)
            }
            "--ffmpeg" => ffmpeg = PathBuf::from(take_value(args, &mut index, flag, inline_value)?),
            "--ffprobe" => {
                ffprobe = PathBuf::from(take_value(args, &mut index, flag, inline_value)?)
            }
            "--converter" => {
                let value = take_value(args, &mut index, flag, inline_value)?;
                if !value.starts_with('/') {
                    return Err(Error::new(
                        EXIT_USAGE,
                        format!("--converter must be an absolute executable path, got {value:?}"),
                    ));
                }
                converter = Some(value);
            }
            "--converter-timeout" => {
                let value = take_value(args, &mut index, flag, inline_value)?;
                let seconds: u64 = value.parse().map_err(|_| {
                    Error::new(
                        EXIT_USAGE,
                        format!("invalid --converter-timeout {value:?}: expected seconds"),
                    )
                })?;
                if !(1..=3600).contains(&seconds) {
                    return Err(Error::new(
                        EXIT_USAGE,
                        format!(
                            "invalid --converter-timeout {value:?}: allowed range is 1-3600 seconds"
                        ),
                    ));
                }
                converter_timeout = Some(seconds);
            }
            other => {
                return Err(Error::new(
                    EXIT_USAGE,
                    format!("unknown argument {other:?}"),
                ))
            }
        }
        index += 1;
    }

    let input = input.ok_or_else(|| Error::new(EXIT_USAGE, "--input <DIR> is required"))?;
    let output = output.ok_or_else(|| Error::new(EXIT_USAGE, "--output <DIR> is required"))?;

    Ok(Parsed::Run(Options {
        input,
        output,
        fps,
        fps_explicit,
        native_tool,
        ffmpeg,
        ffprobe,
        converter,
        converter_timeout,
    }))
}

fn take_value(
    args: &[String],
    index: &mut usize,
    flag: &str,
    inline_value: Option<&str>,
) -> Result<String> {
    if let Some(value) = inline_value {
        return Ok(value.to_string());
    }
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| Error::new(EXIT_USAGE, format!("missing value for {flag}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_run(args: &[&str]) -> Result<Options> {
        let owned: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        match parse(&owned)? {
            Parsed::Run(options) => Ok(options),
            Parsed::Help | Parsed::Version => Err(Error::new(EXIT_USAGE, "unexpected mode")),
        }
    }

    #[test]
    fn default_fps_is_legacy_thirty() {
        let options = parse_run(&["--input", "in", "--output", "out"]).expect("parse");
        assert_eq!(options.fps, Fps::DEFAULT);
        assert_eq!(options.fps.arg(), "30/1");
        assert!(!options.fps_explicit);
        assert!(LEGACY_FPS_PROVENANCE.contains("Fraction(30)"));
    }

    #[test]
    fn parses_rational_fps_and_equals_form() {
        let options =
            parse_run(&["--input=in", "--output=out", "--fps=30000/1001"]).expect("parse");
        assert_eq!(
            options.fps,
            Fps {
                num: 30000,
                den: 1001
            }
        );
        assert_eq!(options.fps.arg(), "30000/1001");
        assert!(options.fps_explicit);
    }

    #[test]
    fn rejects_invalid_fps() {
        for value in ["0", "-30", "30/0", "1001", "abc"] {
            let args = ["--input", "in", "--output", "out", "--fps", value];
            assert!(parse_run(&args).is_err(), "must reject fps {value}");
        }
        assert!(parse_run(&["--input", "in", "--output", "out", "--fps", "1000"]).is_ok());
    }

    #[test]
    fn requires_input_and_output() {
        assert!(parse_run(&[]).is_err());
        assert!(parse_run(&["--input", "in"]).is_err());
        assert!(parse_run(&["--output", "out"]).is_err());
    }

    #[test]
    fn validates_converter_override() {
        assert!(parse_run(&[
            "--input",
            "in",
            "--output",
            "out",
            "--converter",
            "relative/tool"
        ])
        .is_err());
        assert!(parse_run(&[
            "--input",
            "in",
            "--output",
            "out",
            "--converter",
            "/Applications/Adobe DNG Converter.app/Contents/MacOS/Adobe DNG Converter"
        ])
        .is_ok());
        assert!(parse_run(&[
            "--input",
            "in",
            "--output",
            "out",
            "--converter-timeout",
            "0"
        ])
        .is_err());
        assert!(parse_run(&[
            "--input",
            "in",
            "--output",
            "out",
            "--converter-timeout",
            "120"
        ])
        .is_ok());
    }
}
