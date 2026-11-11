//! Command-line parsing for `rawtl-linear-video`.

use crate::color_profile;
use crate::deflicker::ExposureStep;
use crate::error::{Error, Result, EXIT_USAGE};
use std::path::PathBuf;

pub const DEFAULT_NATIVE_TOOL: &str = "native/build/dng-linear-frame";
pub const DEFAULT_FFMPEG: &str = "/opt/homebrew/opt/ffmpeg-full/bin/ffmpeg";
pub const DEFAULT_FFPROBE: &str = "/opt/homebrew/opt/ffmpeg-full/bin/ffprobe";
pub const LEGACY_FPS_PROVENANCE: &str =
    "legacy/python/src/rawtl/cli.py:91 (argparse default Fraction(30))";
pub const DEFLICKER_DEFAULTS_PROVENANCE: &str = "legacy/python/src/rawtl/cli.py:95-114 \
(strength=1.0, period=60, brightness=smooth, estimator=pairwise, \
ev_limits=none, roi=0,0,1,1) amended 2026-11-11: publication gates removed; \
unreliable frames fall back to the global-reference curve; the temporal \
audit records but never blocks (temporal_quality=report)";

pub const USAGE: &str = "\
rawtl-linear-video — DNG frame sequence -> scene-linear ACEScg FFV1 video + colour/Log preview
(default: V4 deflicker enabled with legacy default parameters)

USAGE:
    rawtl-linear-video --input <DIR> --output <DIR> [OPTIONS]

REQUIRED:
    --input <DIR>              Directory of RAW or DNG frames: one file extension per directory;
                               hidden files and .xmp sidecars are skipped (read only, not overlapping output)
    --output <DIR>             Run directory; must be absent or empty

OPTIONS:
    --fps <N[/D]>              Frame rate as an integer or rational (default: 30)
    --white-balance <MODE>     first-frame | per-frame (default: first-frame)
                               first-frame freezes frame 1's as-shot neutral for the sequence;
                               per-frame decodes every frame with its own RAW as-shot neutral
    --dng-linear-frame <PATH>  Native single-frame producer (default: native/build/dng-linear-frame)
    --ffmpeg <PATH>            ffmpeg executable (default: /opt/homebrew/opt/ffmpeg-full/bin/ffmpeg)
    --ffprobe <PATH>           ffprobe executable (default: /opt/homebrew/opt/ffmpeg-full/bin/ffprobe)
    --preview-profile <ID>     Preview colour/Log profile (default: srgb-preview)
                               one of: srgb-preview, dji-dlog-dgamut,
                               canon-log2-cinema-gamut, sony-slog3-sgamut3,
                               sony-slog3-sgamut3-cine, nikon-nlog-bt2020
    --converter <PATH>         Adobe DNG Converter override passed to the native tool
    --converter-timeout <SEC>  Converter timeout override (1-3600 seconds)
    --strength <0..1>          Correction strength (default: 1.0; 0 disables deflicker)
    --period <N>               Smooth-trend period in frames, >= 6 (default: 60)
    --brightness <MODE>        smooth | uniform (default: smooth)
    --estimator <MODE>         pairwise | legacy (default: pairwise)
    --ev-limits <LOW,HIGH>     Clamp applied EV; \"none\" means unbounded (default: none)
    --roi <X,Y,W,H>            Analysis ROI in normalized image coordinates (default: 0,0,1,1)
    --exclude-roi <X0,Y0,X1,Y1>  Exclude tiles from estimation; repeatable
    --step <START:EV>          Confirmed exposure step at 0-based frame START; repeatable
    --temporal-quality <MODE>  report | off (default: report; records, never blocks)
    -h, --help                 Print this help
    --version                  Print version

OUTPUT LAYOUT:
    <output>/frames/frame_%06d.png|json   per-frame RGB16 scene-linear ACEScg frames + metadata
    <output>/analysis/                    per-pass analysis sidecars (deflicker enabled)
    <output>/logs/                        per-frame, encoder and audit transcripts
    <output>/exposure_report.json         per-frame exposure decisions and run status
    <output>/exposure-curve.csv           legacy write_curve fields, one row per frame
    <output>/temporal_audit.json          independent encoded-video audit (when enabled)
    <output>/pairs.csv, frames.csv        audit pair/frame evidence
    <output>/linear-acescg-16bit-ffv1.mkv FFV1 gbrp16le lossless linear video
    <output>/preview-h264-<transfer>-<gamut>.mp4
                                          display-referred H.264 review preview
                                          (default: preview-h264-srgb-bt709.mp4)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brightness {
    Smooth,
    Uniform,
}

impl Brightness {
    pub fn as_str(self) -> &'static str {
        match self {
            Brightness::Smooth => "smooth",
            Brightness::Uniform => "uniform",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "smooth" => Ok(Brightness::Smooth),
            "uniform" => Ok(Brightness::Uniform),
            other => Err(Error::new(
                EXIT_USAGE,
                format!("invalid --brightness {other:?}: expected smooth or uniform"),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Estimator {
    Pairwise,
    Legacy,
}

impl Estimator {
    pub fn as_str(self) -> &'static str {
        match self {
            Estimator::Pairwise => "pairwise",
            Estimator::Legacy => "legacy",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "pairwise" => Ok(Estimator::Pairwise),
            "legacy" => Ok(Estimator::Legacy),
            other => Err(Error::new(
                EXIT_USAGE,
                format!("invalid --estimator {other:?}: expected pairwise or legacy"),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporalQuality {
    Report,
    Off,
}

impl TemporalQuality {
    pub fn as_str(self) -> &'static str {
        match self {
            TemporalQuality::Report => "report",
            TemporalQuality::Off => "off",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "report" => Ok(TemporalQuality::Report),
            "off" => Ok(TemporalQuality::Off),
            other => Err(Error::new(
                EXIT_USAGE,
                format!(
                    "invalid --temporal-quality {other:?}: expected report or off \
                     (strict was removed: the audit records its verdict but never \
                     blocks publication)"
                ),
            )),
        }
    }
}

/// How the sequence derives its camera neutral.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhiteBalance {
    /// Freeze the first numerically ordered frame's as-shot neutral for every
    /// later frame (the historical, default behaviour).
    FirstFrame,
    /// Decode every frame with its own RAW as-shot neutral.
    PerFrame,
}

impl WhiteBalance {
    pub const DEFAULT: WhiteBalance = WhiteBalance::FirstFrame;

    pub fn as_str(self) -> &'static str {
        match self {
            WhiteBalance::FirstFrame => "first-frame",
            WhiteBalance::PerFrame => "per-frame",
        }
    }

    /// Whether the pipeline passes a frozen `--white-balance` override.
    pub fn freezes_reference(self) -> bool {
        matches!(self, WhiteBalance::FirstFrame)
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "first-frame" => Ok(WhiteBalance::FirstFrame),
            "per-frame" => Ok(WhiteBalance::PerFrame),
            other => Err(Error::new(
                EXIT_USAGE,
                format!("invalid --white-balance {other:?}: expected first-frame or per-frame"),
            )),
        }
    }
}

/// Frozen legacy V4 defaults (REQ-011): the deflicker node is ON by default.
#[derive(Debug, Clone)]
pub struct DeflickerConfig {
    pub strength: f64,
    pub period: usize,
    pub brightness: Brightness,
    pub estimator: Estimator,
    pub ev_limits: Option<(f64, f64)>,
    /// Analysis ROI in [x, y, w, h] normalized coordinates.
    pub roi: [f64; 4],
    /// Exclusion boxes in [x0, y0, x1, y1] normalized coordinates.
    pub exclude_rois: Vec<[f64; 4]>,
    pub steps: Vec<ExposureStep>,
    pub temporal_quality: TemporalQuality,
}

impl Default for DeflickerConfig {
    fn default() -> Self {
        Self {
            strength: 1.0,
            period: 60,
            brightness: Brightness::Smooth,
            estimator: Estimator::Pairwise,
            ev_limits: None,
            roi: [0.0, 0.0, 1.0, 1.0],
            exclude_rois: vec![],
            steps: vec![],
            temporal_quality: TemporalQuality::Report,
        }
    }
}

impl DeflickerConfig {
    pub fn enabled(&self) -> bool {
        self.strength != 0.0
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub input: PathBuf,
    pub output: PathBuf,
    pub fps: Fps,
    pub fps_explicit: bool,
    pub white_balance: WhiteBalance,
    pub native_tool: PathBuf,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub preview_profile: &'static color_profile::ColorProfile,
    pub converter: Option<String>,
    pub converter_timeout: Option<u64>,
    pub deflicker: DeflickerConfig,
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
    let mut white_balance = WhiteBalance::DEFAULT;
    let mut native_tool = PathBuf::from(DEFAULT_NATIVE_TOOL);
    let mut ffmpeg = PathBuf::from(DEFAULT_FFMPEG);
    let mut ffprobe = PathBuf::from(DEFAULT_FFPROBE);
    let mut preview_profile = color_profile::default_profile();
    let mut converter: Option<String> = None;
    let mut converter_timeout: Option<u64> = None;
    let mut deflicker = DeflickerConfig::default();

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
            "--white-balance" => {
                white_balance =
                    WhiteBalance::parse(&take_value(args, &mut index, flag, inline_value)?)?
            }
            "--dng-linear-frame" => {
                native_tool = PathBuf::from(take_value(args, &mut index, flag, inline_value)?)
            }
            "--ffmpeg" => ffmpeg = PathBuf::from(take_value(args, &mut index, flag, inline_value)?),
            "--ffprobe" => {
                ffprobe = PathBuf::from(take_value(args, &mut index, flag, inline_value)?)
            }
            "--preview-profile" => {
                let value = take_value(args, &mut index, flag, inline_value)?;
                preview_profile = color_profile::lookup(&value).ok_or_else(|| {
                    Error::new(
                        EXIT_USAGE,
                        format!(
                            "invalid --preview-profile {value:?}: expected one of {}",
                            color_profile::PROFILES
                                .iter()
                                .map(|profile| profile.id)
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    )
                })?;
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
            "--strength" => {
                deflicker.strength =
                    parse_strength(&take_value(args, &mut index, flag, inline_value)?)?
            }
            "--period" => {
                deflicker.period = parse_period(&take_value(args, &mut index, flag, inline_value)?)?
            }
            "--brightness" => {
                deflicker.brightness =
                    Brightness::parse(&take_value(args, &mut index, flag, inline_value)?)?
            }
            "--estimator" => {
                deflicker.estimator =
                    Estimator::parse(&take_value(args, &mut index, flag, inline_value)?)?
            }
            "--ev-limits" => {
                deflicker.ev_limits =
                    parse_ev_limits(&take_value(args, &mut index, flag, inline_value)?)?
            }
            "--roi" => {
                deflicker.roi =
                    parse_analysis_roi(&take_value(args, &mut index, flag, inline_value)?)?
            }
            "--exclude-roi" => deflicker.exclude_rois.push(parse_exclude_roi(&take_value(
                args,
                &mut index,
                flag,
                inline_value,
            )?)?),
            "--step" => deflicker.steps.push(parse_step(&take_value(
                args,
                &mut index,
                flag,
                inline_value,
            )?)?),
            "--temporal-quality" => {
                deflicker.temporal_quality =
                    TemporalQuality::parse(&take_value(args, &mut index, flag, inline_value)?)?
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
        white_balance,
        native_tool,
        ffmpeg,
        ffprobe,
        preview_profile,
        converter,
        converter_timeout,
        deflicker,
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

fn parse_strength(value: &str) -> Result<f64> {
    let strength: f64 = value.parse().map_err(|_| {
        Error::new(
            EXIT_USAGE,
            format!("invalid --strength {value:?}: expected a finite number"),
        )
    })?;
    if !strength.is_finite() || !(0.0..=1.0).contains(&strength) {
        return Err(Error::new(
            EXIT_USAGE,
            format!("invalid --strength {value:?}: allowed range is [0,1]"),
        ));
    }
    Ok(strength)
}

fn parse_period(value: &str) -> Result<usize> {
    let period: usize = value.parse().map_err(|_| {
        Error::new(
            EXIT_USAGE,
            format!("invalid --period {value:?}: expected an integer >= 6"),
        )
    })?;
    if period < 6 {
        return Err(Error::new(
            EXIT_USAGE,
            format!("invalid --period {value:?}: expected an integer >= 6"),
        ));
    }
    Ok(period)
}

fn parse_four(value: &str, flag: &str) -> Result<[f64; 4]> {
    let parts: Vec<&str> = value.split(',').collect();
    if parts.len() != 4 {
        return Err(Error::new(
            EXIT_USAGE,
            format!("invalid {flag} {value:?}: expected four comma-separated numbers"),
        ));
    }
    let mut out = [0.0; 4];
    for (slot, part) in out.iter_mut().zip(parts) {
        let number: f64 = part.trim().parse().map_err(|_| {
            Error::new(
                EXIT_USAGE,
                format!("invalid {flag} {value:?}: expected four comma-separated numbers"),
            )
        })?;
        if !number.is_finite() {
            return Err(Error::new(
                EXIT_USAGE,
                format!("invalid {flag} {value:?}: all values must be finite"),
            ));
        }
        *slot = number;
    }
    Ok(out)
}

fn parse_analysis_roi(value: &str) -> Result<[f64; 4]> {
    let [x, y, w, h] = parse_four(value, "--roi")?;
    if x < 0.0 || y < 0.0 || w <= 0.0 || h <= 0.0 || x + w > 1.0 || y + h > 1.0 {
        return Err(Error::new(
            EXIT_USAGE,
            format!("invalid --roi {value:?}: expected x,y,w,h within normalized image bounds"),
        ));
    }
    Ok([x, y, w, h])
}

fn parse_exclude_roi(value: &str) -> Result<[f64; 4]> {
    let [x0, y0, x1, y1] = parse_four(value, "--exclude-roi")?;
    if x0 < 0.0 || y0 < 0.0 || x1 > 1.0 || y1 > 1.0 || x0 >= x1 || y0 >= y1 {
        return Err(Error::new(
            EXIT_USAGE,
            format!("invalid --exclude-roi {value:?}: expected x0,y0,x1,y1 within the unit square"),
        ));
    }
    Ok([x0, y0, x1, y1])
}

fn parse_ev_limits(value: &str) -> Result<Option<(f64, f64)>> {
    let text = value.trim().to_ascii_lowercase();
    if text.is_empty() || text == "none" || text == "unbounded" {
        return Ok(None);
    }
    let Some((low, high)) = text.split_once(',') else {
        return Err(Error::new(
            EXIT_USAGE,
            format!("invalid --ev-limits {value:?}: expected \"none\" or LOW,HIGH"),
        ));
    };
    let low: f64 = low.trim().parse().map_err(|_| {
        Error::new(
            EXIT_USAGE,
            format!("invalid --ev-limits {value:?}: LOW must be a finite number"),
        )
    })?;
    let high: f64 = high.trim().parse().map_err(|_| {
        Error::new(
            EXIT_USAGE,
            format!("invalid --ev-limits {value:?}: HIGH must be a finite number"),
        )
    })?;
    if !low.is_finite() || !high.is_finite() || !(low < high) {
        return Err(Error::new(
            EXIT_USAGE,
            format!("invalid --ev-limits {value:?}: expected two finite numbers with LOW < HIGH"),
        ));
    }
    Ok(Some((low, high)))
}

fn parse_step(value: &str) -> Result<ExposureStep> {
    let Some((start, delta)) = value.split_once(':') else {
        return Err(Error::new(
            EXIT_USAGE,
            format!("invalid --step {value:?}: expected START:EV"),
        ));
    };
    let start_frame: usize = start.trim().parse().map_err(|_| {
        Error::new(
            EXIT_USAGE,
            format!("invalid --step {value:?}: START must be a non-negative integer"),
        )
    })?;
    let delta_ev: f64 = delta.trim().parse().map_err(|_| {
        Error::new(
            EXIT_USAGE,
            format!("invalid --step {value:?}: EV must be a finite number"),
        )
    })?;
    if !delta_ev.is_finite() {
        return Err(Error::new(
            EXIT_USAGE,
            format!("invalid --step {value:?}: EV must be a finite number"),
        ));
    }
    Ok(ExposureStep {
        start_frame,
        delta_ev,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color_profile;

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
    fn white_balance_defaults_to_first_frame_and_parses_both_modes() {
        let options = parse_run(&["--input", "in", "--output", "out"]).expect("parse");
        assert_eq!(options.white_balance, WhiteBalance::FirstFrame);
        assert_eq!(WhiteBalance::DEFAULT, WhiteBalance::FirstFrame);
        assert!(options.white_balance.freezes_reference());

        let explicit = parse_run(&[
            "--input",
            "in",
            "--output",
            "out",
            "--white-balance",
            "first-frame",
        ])
        .expect("parse");
        assert_eq!(explicit.white_balance, WhiteBalance::FirstFrame);

        let per_frame = parse_run(&[
            "--input",
            "in",
            "--output",
            "out",
            "--white-balance",
            "per-frame",
        ])
        .expect("parse");
        assert_eq!(per_frame.white_balance, WhiteBalance::PerFrame);
        assert!(!per_frame.white_balance.freezes_reference());

        let inline =
            parse_run(&["--input=in", "--output=out", "--white-balance=per-frame"]).expect("parse");
        assert_eq!(inline.white_balance, WhiteBalance::PerFrame);

        assert_eq!(WhiteBalance::FirstFrame.as_str(), "first-frame");
        assert_eq!(WhiteBalance::PerFrame.as_str(), "per-frame");
        assert!(USAGE.contains("--white-balance"));
        assert!(USAGE.contains("first-frame"));
        assert!(USAGE.contains("per-frame"));
    }

    #[test]
    fn rejects_invalid_white_balance_modes() {
        let base = ["--input", "in", "--output", "out"];
        for extra in [
            vec!["--white-balance", "bogus"],
            vec!["--white-balance", ""],
            vec!["--white-balance="],
            vec!["--white-balance"],
        ] {
            let mut args: Vec<&str> = base.to_vec();
            args.extend(extra.iter());
            let error = parse_run(&args).expect_err("must reject the white balance value");
            assert_eq!(error.code, EXIT_USAGE, "{:?}", error.message);
            assert!(
                error.message.contains("--white-balance"),
                "error must name the flag: {}",
                error.message
            );
        }
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

    #[test]
    fn deflicker_defaults_match_legacy_v4() {
        let options = parse_run(&["--input", "in", "--output", "out"]).expect("parse");
        let d = &options.deflicker;
        assert!(d.enabled());
        assert_eq!(d.strength, 1.0);
        assert_eq!(d.period, 60);
        assert_eq!(d.brightness, Brightness::Smooth);
        assert_eq!(d.estimator, Estimator::Pairwise);
        assert_eq!(d.ev_limits, None);
        assert_eq!(d.roi, [0.0, 0.0, 1.0, 1.0]);
        assert!(d.exclude_rois.is_empty());
        assert!(d.steps.is_empty());
        assert_eq!(d.temporal_quality, TemporalQuality::Report);
        assert!(DEFLICKER_DEFAULTS_PROVENANCE.contains("strength=1.0"));
    }

    #[test]
    fn parses_deflicker_options() {
        let options = parse_run(&[
            "--input",
            "in",
            "--output",
            "out",
            "--strength",
            "0.5",
            "--period",
            "24",
            "--brightness",
            "uniform",
            "--estimator",
            "legacy",
            "--ev-limits",
            "-2,2",
            "--roi",
            "0.1,0.2,0.5,0.6",
            "--exclude-roi",
            "0,0,0.1,0.1",
            "--exclude-roi",
            "0.9,0.9,1,1",
            "--step",
            "10:0.5",
            "--step",
            "20:-0.25",
            "--temporal-quality",
            "off",
        ])
        .expect("parse");
        let d = &options.deflicker;
        assert_eq!(d.strength, 0.5);
        assert_eq!(d.period, 24);
        assert_eq!(d.brightness, Brightness::Uniform);
        assert_eq!(d.estimator, Estimator::Legacy);
        assert_eq!(d.ev_limits, Some((-2.0, 2.0)));
        assert_eq!(d.roi, [0.1, 0.2, 0.5, 0.6]);
        assert_eq!(d.exclude_rois.len(), 2);
        assert_eq!(d.steps.len(), 2);
        assert_eq!(d.steps[0].start_frame, 10);
        assert_eq!(d.steps[0].delta_ev, 0.5);
        assert_eq!(d.steps[1].start_frame, 20);
        assert_eq!(d.temporal_quality, TemporalQuality::Off);
    }

    #[test]
    fn parses_preview_profile_and_defaults_to_srgb() {
        let options = parse_run(&["--input", "in", "--output", "out"]).expect("parse");
        assert_eq!(options.preview_profile.id, "srgb-preview");
        for id in color_profile::PROFILES.iter().map(|profile| profile.id) {
            let options = parse_run(&["--input", "in", "--output", "out", "--preview-profile", id])
                .expect("parse");
            assert_eq!(options.preview_profile.id, id);
        }
        let options = parse_run(&[
            "--input",
            "in",
            "--output",
            "out",
            "--preview-profile=nikon-nlog-bt2020",
        ])
        .expect("parse");
        assert_eq!(options.preview_profile.id, "nikon-nlog-bt2020");
    }

    #[test]
    fn rejects_unknown_or_empty_preview_profile() {
        for value in ["dlog-dgamut", "srgb", "", "srgb-preview "] {
            let args = [
                "--input",
                "in",
                "--output",
                "out",
                "--preview-profile",
                value,
            ];
            let error = parse_run(&args).expect_err("must reject");
            assert_eq!(error.code, EXIT_USAGE);
            assert!(
                error.message.contains("--preview-profile"),
                "{}",
                error.message
            );
        }
        let error = parse_run(&["--input", "in", "--output", "out", "--preview-profile"])
            .expect_err("missing value");
        assert!(error.message.contains("missing value"));
        let error = parse_run(&["--input", "in", "--output", "out", "--preview-profile="])
            .expect_err("empty inline value");
        assert_eq!(error.code, EXIT_USAGE);
    }

    #[test]
    fn rejects_invalid_deflicker_options() {
        let base = ["--input", "in", "--output", "out"];
        for extra in [
            vec!["--strength", "-0.1"],
            vec!["--strength", "1.1"],
            vec!["--strength", "nan"],
            vec!["--period", "5"],
            vec!["--period", "abc"],
            vec!["--brightness", "linear"],
            vec!["--estimator", "graph"],
            vec!["--ev-limits", "2,-2"],
            vec!["--ev-limits", "0"],
            vec!["--roi", "0,0,1.1,1"],
            vec!["--roi", "0.5,0,0.6,1"],
            vec!["--exclude-roi", "0.5,0,0.4,1"],
            vec!["--step", "10"],
            vec!["--step", "-1:0.5"],
            vec!["--temporal-quality", "loose"],
            vec!["--temporal-quality", "strict"],
            vec!["--on-review", "error"],
        ] {
            let mut args: Vec<&str> = base.to_vec();
            args.extend(extra.iter());
            assert!(parse_run(&args).is_err(), "must reject {extra:?}");
        }
        assert!(parse_run(&["--input", "in", "--output", "out", "--ev-limits", "none"]).is_ok());
        assert!(parse_run(&[
            "--input",
            "in",
            "--output",
            "out",
            "--ev-limits",
            "unbounded"
        ])
        .is_ok());
    }
}
