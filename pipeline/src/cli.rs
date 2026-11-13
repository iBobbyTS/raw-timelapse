//! Command-line parsing for `rawtl-linear-video`.

use crate::color_profile;
use crate::deflicker::ExposureStep;
use crate::encoder::{self, Container, Patch, RateControl};
use crate::error::{Error, Result, EXIT_USAGE};
use std::path::PathBuf;

pub const DEFAULT_NATIVE_TOOL: &str = "native/build/dng-linear-frame";
pub const DEFAULT_FFMPEG: &str = "/opt/homebrew/opt/ffmpeg-full/bin/ffmpeg";
pub const DEFAULT_FFPROBE: &str = "/opt/homebrew/opt/ffmpeg-full/bin/ffprobe";
pub const LEGACY_FPS_PROVENANCE: &str =
    "legacy/python/src/rawtl/cli.py:91 (argparse default Fraction(30))";
pub const USAGE: &str = "\
rawtl-linear-video — RAW/DNG frame sequence -> one video file and one run log
(default: best-effort deflicker, ProRes 4444 at the source geometry)

USAGE:
    rawtl-linear-video --input <DIR> --output <VIDEO_FILE> [OPTIONS]

REQUIRED:
    --input <DIR>              Directory of RAW or DNG frames: one file extension per directory;
                               hidden files and .xmp sidecars are skipped (read only)
    --output <VIDEO_FILE>      Output video file, e.g. /data/timelapse.mov. Its extension must
                               match the configured container (default .mov). The run log is
                               written beside it as <name>.log. Existing files are never
                               overwritten; choose a new name or remove the old run first.

OPTIONS:
    --graph <FILE>            Typed graph JSON (default: prepared-frame graph)
    --fps <N[/D]>              Frame rate as an integer or rational (default: 30)
    --white-balance <MODE>     first-frame | per-frame (default: first-frame)
                               first-frame freezes frame 1's as-shot neutral for the sequence;
                               per-frame decodes every frame with its own RAW as-shot neutral
    --dng-linear-frame <PATH>  Native single-frame producer (default: native/build/dng-linear-frame)
    --ffmpeg <PATH>            ffmpeg executable (default: /opt/homebrew/opt/ffmpeg-full/bin/ffmpeg)
    --ffprobe <PATH>           ffprobe executable (default: /opt/homebrew/opt/ffmpeg-full/bin/ffprobe)
    --preview-profile <ID>     Output colour/Log profile (default: srgb-preview)
                               one of: srgb-preview, dji-dlog-dgamut,
                               canon-log2-cinema-gamut, sony-slog3-sgamut3,
                               sony-slog3-sgamut3-cine, nikon-nlog-bt2020,
                               canon-log2-cinema-gamut-rev-eng-crm-dev,
                               canon-log3-cinema-gamut-rev-eng-crm-dev
                               Canon names: clog2(white paper), clog2(rev-eng crm-dev),
                               clog3 (rev-eng crm-dev)
    --log-reference-gain <N>   Positive reference scale, rev-eng Canon profiles only (default: 1)
    --encoder <NAME>           prores_ks | libx264 | ffv1 (default: prores_ks)
    --pix-fmt <FMT>            Encoder pixel format (default: yuv444p10le; libx264: yuv420p)
    --container <NAME>         mov | mp4 | matroska (default: mov; libx264: mp4; ffv1: matroska)
    --codec-profile <VALUE>    Encoder profile (ProRes 0-5/4444; H.264 baseline..high444)
    --crf <N>                  Quality for libx264, 0-63 (conflicts with --qscale/--bitrate)
    --qscale <N>               Fixed quantizer, 0-63 (conflicts with --crf/--bitrate)
    --bitrate <BPS>            Target bitrate in bits/s (conflicts with --crf/--qscale)
    --output-option <K=V>      Repeatable bounded encoder option, e.g. -preset=medium
                               allowed: -preset -tune -vendor -threads -g -bf -level
    --resize <WxH>             Explicit scene-linear resize; omit to keep source geometry
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
    -h, --help                 Print this help
    --version                  Print version

CONFIGURATION PRIORITY:
    built-in default < graph encode-node configuration < explicit CLI flags
    Unsupported encoders/pixel formats/options and conflicting geometry or
    colour parameters are rejected before RAW preparation; no encoder or
    pixel format is silently substituted.

OUTPUT SURFACE:
    <VIDEO_FILE>               validated deliverable, published by a single rename
    <name>.log                 one run log: wall-clock + monotonic elapsed, node/frame
                               events, per-frame timings, cache-deletion events and
                               bounded error summaries (Converter/FFmpeg stderr tails)
    No logs directory, manifest, CSV or audit JSON is produced. A normal run leaves
    exactly the video and its log beside any files that already existed there.

RESOLUTION:
    The default keeps the source geometry; ProRes 4444 needs odd/even-any geometry
    and no implicit 4096 cap applies. Pass --resize to resample explicitly.

SCRATCH AND FAILURE:
    A run-specific scratch directory (<video dir>/.rawtl-<name>-<pid>-<nonce>) holds
    the converted LinearRaw DNG cache and child-process transcripts. The graph owns
    deletion: the scratch is removed after a successful publication when its delete
    nodes consumed every generated cache file; a custom graph that leaves generated
    DNGs keeps the scratch and the log names the retained cache. On failure or stop
    the scratch is retained and named in the log together with any partial output.
    Converting and caching a long full-resolution sequence needs disk space for the
    cache peak. A failed run must be re-run from the source: there is no checkpoint,
    resume or automatic recovery, and an earlier cache is never picked up again.

MIGRATION:
    --output used to name a run directory. Pass the video file instead; the log is
    <name>.log beside it. --temporal-quality (report|off) belonged to the removed
    quality-report workflow and is rejected; a run never reports a deflicker verdict.
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
        }
    }
}

impl DeflickerConfig {
    pub fn enabled(&self) -> bool {
        self.strength != 0.0
    }

    pub fn ev_limits_text(&self) -> String {
        match self.ev_limits {
            Some((low, high)) => format!("{low},{high}"),
            None => "none".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub graph: Option<PathBuf>,
    pub input: PathBuf,
    /// The single deliverable video file named by `--output`.
    pub video: PathBuf,
    /// The run log beside it: same file stem, `.log` extension.
    pub log: PathBuf,
    pub fps: Fps,
    pub fps_explicit: bool,
    pub white_balance: WhiteBalance,
    pub native_tool: PathBuf,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub log_reference_gain: f64,
    pub preview_profile: &'static color_profile::ColorProfile,
    pub converter: Option<String>,
    pub converter_timeout: Option<u64>,
    pub deflicker: DeflickerConfig,
    /// Explicit CLI encoder layer; empty when no encoder flag was given.
    pub encoder: Patch,
    /// Explicit CLI resize; `None` keeps the graph's geometry (source by default).
    pub resize: Option<(u32, u32)>,
}

#[derive(Debug)]
pub enum Parsed {
    Run(Options),
    Help,
    Version,
}

pub fn parse(args: &[String]) -> Result<Parsed> {
    let mut graph = None;
    let mut input: Option<PathBuf> = None;
    let mut output: Option<(PathBuf, PathBuf)> = None;
    let mut fps = Fps::DEFAULT;
    let mut fps_explicit = false;
    let mut white_balance = WhiteBalance::DEFAULT;
    let mut native_tool = PathBuf::from(DEFAULT_NATIVE_TOOL);
    let mut ffmpeg = PathBuf::from(DEFAULT_FFMPEG);
    let mut ffprobe = PathBuf::from(DEFAULT_FFPROBE);
    let mut preview_profile = color_profile::default_profile();
    let mut log_reference_gain: Option<f64> = None;
    let mut converter: Option<String> = None;
    let mut converter_timeout: Option<u64> = None;
    let mut deflicker = DeflickerConfig::default();
    let mut encoder = Patch::default();
    let mut output_options: Vec<String> = Vec::new();
    let mut rate: Option<RateControl> = None;
    let mut rate_flag: Option<&'static str> = None;
    let mut resize: Option<(u32, u32)> = None;
    let mut seen_input = false;
    let mut seen_output = false;

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
                if seen_input {
                    return Err(Error::new(EXIT_USAGE, "--input may be given only once"));
                }
                seen_input = true;
                input = Some(PathBuf::from(take_value(
                    args,
                    &mut index,
                    flag,
                    inline_value,
                )?))
            }
            "--output" => {
                if seen_output {
                    return Err(Error::new(EXIT_USAGE, "--output may be given only once"));
                }
                seen_output = true;
                let (video, log) = parse_video_target(&take_value(
                    args,
                    &mut index,
                    flag,
                    inline_value,
                )?)?;
                output = Some((video, log));
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
            "--log-reference-gain" => {
                let value = take_value(args, &mut index, flag, inline_value)?;
                let gain: f64 = value
                    .parse()
                    .map_err(|_| Error::new(EXIT_USAGE, "invalid --log-reference-gain"))?;
                if !gain.is_finite() || gain <= 0.0 {
                    return Err(Error::new(
                        EXIT_USAGE,
                        "--log-reference-gain must be positive and finite",
                    ));
                }
                log_reference_gain = Some(gain);
            }
            "--graph" => {
                graph = Some(PathBuf::from(take_value(
                    args,
                    &mut index,
                    flag,
                    inline_value,
                )?))
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
                let value = take_value(args, &mut index, flag, inline_value)?;
                return Err(Error::new(
                    EXIT_USAGE,
                    format!(
                        "--temporal-quality {value:?} was removed: a run no longer produces a \
                         quality report or gate. Operational events and errors are recorded in \
                         the single run log; remove the flag"
                    ),
                ));
            }
            "--on-review" => {
                let value = take_value(args, &mut index, flag, inline_value)?;
                return Err(Error::new(
                    EXIT_USAGE,
                    format!(
                        "--on-review {value:?} belonged to the removed quality-gate workflow; \
                         estimate completeness never blocks a run. Remove the flag"
                    ),
                ));
            }
            "--save-frames" | "--overwrite" => {
                return Err(Error::new(
                    EXIT_USAGE,
                    format!(
                        "{flag} was removed: intermediate frames stay in run scratch and are \
                         deleted after a successful run, and an existing video or log is never \
                         overwritten"
                    ),
                ));
            }
            "--exclude-rois" => {
                let _ = take_value(args, &mut index, flag, inline_value)?;
                return Err(Error::new(
                    EXIT_USAGE,
                    "--exclude-rois (file form) was removed; repeat --exclude-roi X0,Y0,X1,Y1",
                ));
            }
            "--steps" => {
                let _ = take_value(args, &mut index, flag, inline_value)?;
                return Err(Error::new(
                    EXIT_USAGE,
                    "--steps (file form) was removed; repeat --step START:EV",
                ));
            }
            "--encoder" => {
                let value = take_value(args, &mut index, flag, inline_value)?;
                if !encoder::SUPPORTED_ENCODERS.contains(&value.as_str()) {
                    return Err(Error::new(
                        EXIT_USAGE,
                        format!(
                            "invalid --encoder {value:?}: expected one of {}",
                            encoder::SUPPORTED_ENCODERS.join(", ")
                        ),
                    ));
                }
                encoder.encoder = Some(value);
            }
            "--pix-fmt" => {
                encoder.pix_fmt = Some(take_value(args, &mut index, flag, inline_value)?)
            }
            "--container" => {
                let value = take_value(args, &mut index, flag, inline_value)?;
                encoder.container = Some(Container::parse(&value).ok_or_else(|| {
                    Error::new(
                        EXIT_USAGE,
                        format!(
                            "invalid --container {value:?}: expected one of {}",
                            encoder::CONTAINERS.join(", ")
                        ),
                    )
                })?);
            }
            "--codec-profile" => {
                encoder.profile = Some(take_value(args, &mut index, flag, inline_value)?)
            }
            "--crf" => {
                let value = parse_rate(&take_value(args, &mut index, flag, inline_value)?, 0, 63)?;
                set_rate(&mut rate, &mut rate_flag, RateControl::Crf(value as u32), "--crf")?;
            }
            "--qscale" => {
                let value = parse_rate(&take_value(args, &mut index, flag, inline_value)?, 0, 63)?;
                set_rate(
                    &mut rate,
                    &mut rate_flag,
                    RateControl::QScale(value as u32),
                    "--qscale",
                )?;
            }
            "--bitrate" => {
                let value =
                    parse_rate(&take_value(args, &mut index, flag, inline_value)?, 1, 1_000_000_000)?;
                set_rate(
                    &mut rate,
                    &mut rate_flag,
                    RateControl::Bitrate(value),
                    "--bitrate",
                )?;
            }
            "--output-option" => {
                let value = take_value(args, &mut index, flag, inline_value)?;
                output_options.push(value);
            }
            "--resize" => {
                resize = Some(parse_resize(&take_value(args, &mut index, flag, inline_value)?)?)
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
    let (video, log) =
        output.ok_or_else(|| Error::new(EXIT_USAGE, "--output <VIDEO_FILE> is required"))?;

    if log_reference_gain.is_some() && !preview_profile.is_rev_eng() {
        return Err(Error::new(
            EXIT_USAGE,
            "--log-reference-gain requires a rev-eng Canon profile (canon-log2-cinema-gamut-rev-eng-crm-dev or canon-log3-cinema-gamut-rev-eng-crm-dev)",
        ));
    }
    encoder.rate = rate;
    if !output_options.is_empty() {
        encoder.options = Some(
            encoder::parse_output_options(&output_options)
                .map_err(|message| Error::new(EXIT_USAGE, message))?,
        );
    }
    Ok(Parsed::Run(Options {
        graph,
        input,
        video,
        log,
        fps,
        fps_explicit,
        white_balance,
        native_tool,
        ffmpeg,
        ffprobe,
        preview_profile,
        log_reference_gain: log_reference_gain.unwrap_or(1.0),
        converter,
        converter_timeout,
        deflicker,
        encoder,
        resize,
    }))
}

/// Resolve the single `--output` value into the deliverable video file and the
/// run log beside it. The old run-directory form is a migration error, never a
/// guessed file name.
pub fn parse_video_target(value: &str) -> Result<(PathBuf, PathBuf)> {
    let path = PathBuf::from(value);
    if value.ends_with('/') || path.is_dir() || path.file_name().is_none() {
        return Err(Error::new(
            EXIT_USAGE,
            format!(
                "--output {value:?} names a directory; this version writes one video file. \
                 Pass the video path instead, e.g. {}/timelapse.mov; the run log is written \
                 beside it as timelapse.log",
                value.trim_end_matches('/')
            ),
        ));
    }
    let extension = path.extension().and_then(|extension| extension.to_str());
    if extension.is_none_or(str::is_empty) {
        return Err(Error::new(
            EXIT_USAGE,
            format!(
                "--output {value:?} must name the video file including its container \
                 extension, e.g. {value}.mov; the run log is written beside it as {value}.log"
            ),
        ));
    }
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| {
            Error::new(
                EXIT_USAGE,
                format!("--output {value:?} has no usable file name for its run log"),
            )
        })?;
    let log = path.with_file_name(format!("{stem}.log"));
    Ok((path, log))
}

fn parse_rate(value: &str, min: u64, max: u64) -> Result<u64> {
    let number: u64 = value.parse().map_err(|_| {
        Error::new(
            EXIT_USAGE,
            format!("invalid encoder rate value {value:?}: expected an integer {min}..={max}"),
        )
    })?;
    if !(min..=max).contains(&number) {
        return Err(Error::new(
            EXIT_USAGE,
            format!("invalid encoder rate value {value:?}: expected an integer {min}..={max}"),
        ));
    }
    Ok(number)
}

fn set_rate(
    slot: &mut Option<RateControl>,
    flag_slot: &mut Option<&'static str>,
    value: RateControl,
    flag: &'static str,
) -> Result<()> {
    if let Some(previous) = flag_slot {
        return Err(Error::new(
            EXIT_USAGE,
            format!("{flag} conflicts with {previous}; choose one rate control"),
        ));
    }
    *slot = Some(value);
    *flag_slot = Some(flag);
    Ok(())
}

fn parse_resize(value: &str) -> Result<(u32, u32)> {
    let (width, height) = value
        .split_once('x')
        .or_else(|| value.split_once('X'))
        .ok_or_else(|| {
            Error::new(
                EXIT_USAGE,
                format!("invalid --resize {value:?}: expected WxH, e.g. 4096x2734"),
            )
        })?;
    let width: u32 = width.trim().parse().map_err(|_| {
        Error::new(
            EXIT_USAGE,
            format!("invalid --resize {value:?}: width must be a positive integer"),
        )
    })?;
    let height: u32 = height.trim().parse().map_err(|_| {
        Error::new(
            EXIT_USAGE,
            format!("invalid --resize {value:?}: height must be a positive integer"),
        )
    })?;
    if width == 0 || height == 0 {
        return Err(Error::new(
            EXIT_USAGE,
            format!("invalid --resize {value:?}: dimensions must be positive"),
        ));
    }
    Ok((width, height))
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
        let options = parse_run(&["--input", "in", "--output", "out.mov"]).expect("parse");
        assert_eq!(options.fps, Fps::DEFAULT);
        assert_eq!(options.fps.arg(), "30/1");
        assert!(!options.fps_explicit);
        assert!(LEGACY_FPS_PROVENANCE.contains("Fraction(30)"));
    }

    #[test]
    fn parses_rational_fps_and_equals_form() {
        let options =
            parse_run(&["--input=in", "--output=out.mov", "--fps=30000/1001"]).expect("parse");
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
            let args = ["--input", "in", "--output", "out.mov", "--fps", value];
            assert!(parse_run(&args).is_err(), "must reject fps {value}");
        }
        assert!(parse_run(&["--input", "in", "--output", "out.mov", "--fps", "1000"]).is_ok());
    }

    #[test]
    fn white_balance_defaults_to_first_frame_and_parses_both_modes() {
        let options = parse_run(&["--input", "in", "--output", "out.mov"]).expect("parse");
        assert_eq!(options.white_balance, WhiteBalance::FirstFrame);
        assert_eq!(WhiteBalance::DEFAULT, WhiteBalance::FirstFrame);
        assert!(options.white_balance.freezes_reference());

        let explicit = parse_run(&[
            "--input",
            "in",
            "--output",
            "out.mov",
            "--white-balance",
            "first-frame",
        ])
        .expect("parse");
        assert_eq!(explicit.white_balance, WhiteBalance::FirstFrame);

        let per_frame = parse_run(&[
            "--input",
            "in",
            "--output",
            "out.mov",
            "--white-balance",
            "per-frame",
        ])
        .expect("parse");
        assert_eq!(per_frame.white_balance, WhiteBalance::PerFrame);
        assert!(!per_frame.white_balance.freezes_reference());

        let inline =
            parse_run(&["--input=in", "--output=out.mov", "--white-balance=per-frame"]).expect("parse");
        assert_eq!(inline.white_balance, WhiteBalance::PerFrame);

        assert_eq!(WhiteBalance::FirstFrame.as_str(), "first-frame");
        assert_eq!(WhiteBalance::PerFrame.as_str(), "per-frame");
        assert!(USAGE.contains("--white-balance"));
        assert!(USAGE.contains("first-frame"));
        assert!(USAGE.contains("per-frame"));
    }

    #[test]
    fn rejects_invalid_white_balance_modes() {
        let base = ["--input", "in", "--output", "out.mov"];
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
        assert!(parse_run(&["--output", "out.mov"]).is_err());
    }

    #[test]
    fn output_names_one_video_file_and_a_sibling_log() {
        let options = parse_run(&["--input", "in", "--output", "/data/timelapse.mov"])
            .expect("parse");
        assert_eq!(options.video, PathBuf::from("/data/timelapse.mov"));
        assert_eq!(options.log, PathBuf::from("/data/timelapse.log"));

        let relative = parse_run(&["--input", "in", "--output=clip.v2.mkv"]).expect("parse");
        assert_eq!(relative.video, PathBuf::from("clip.v2.mkv"));
        assert_eq!(relative.log, PathBuf::from("clip.v2.log"));
    }

    #[test]
    fn old_run_directory_output_is_an_explicit_migration_error() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-tmp")
            .join(format!("cli-output-migration-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create migration dir");
        let directory = root.to_str().expect("utf8");
        let with_slash = format!("{directory}/");
        for value in [directory, with_slash.as_str()] {
            let error = parse_run(&["--input", "in", "--output", value]).expect_err("directory");
            assert_eq!(error.code, EXIT_USAGE);
            assert!(error.message.contains("names a directory"), "{}", error.message);
            assert!(error.message.contains("timelapse.mov"), "{}", error.message);
        }
        for value in ["run", "out"] {
            let error = parse_run(&["--input", "in", "--output", value]).expect_err("usage");
            assert_eq!(error.code, EXIT_USAGE);
            assert!(error.message.contains("--output"), "{}", error.message);
            assert!(
                error.message.contains("extension") || error.message.contains("video file"),
                "{}",
                error.message
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn removed_quality_workflow_flags_are_rejected_with_migration_text() {
        let base = ["--input", "in", "--output", "out.mov"];
        for (extra, marker) in [
            (vec!["--temporal-quality", "off"], "temporal-quality"),
            (vec!["--temporal-quality=report"], "temporal-quality"),
            (vec!["--on-review", "error"], "on-review"),
            (vec!["--save-frames"], "save-frames"),
            (vec!["--overwrite"], "overwrite"),
            (vec!["--steps", "steps.json"], "--step"),
            (vec!["--exclude-rois", "rois.json"], "--exclude-roi"),
        ] {
            let mut args: Vec<&str> = base.to_vec();
            args.extend(extra.iter());
            let error = parse_run(&args).expect_err("removed flag");
            assert_eq!(error.code, EXIT_USAGE, "{extra:?}: {}", error.message);
            assert!(
                error.message.contains(marker),
                "{extra:?}: {}",
                error.message
            );
        }
    }

    #[test]
    fn validates_converter_override() {
        assert!(parse_run(&[
            "--input",
            "in",
            "--output",
            "out.mov",
            "--converter",
            "relative/tool"
        ])
        .is_err());
        assert!(parse_run(&[
            "--input",
            "in",
            "--output",
            "out.mov",
            "--converter",
            "/Applications/Adobe DNG Converter.app/Contents/MacOS/Adobe DNG Converter"
        ])
        .is_ok());
        assert!(parse_run(&[
            "--input",
            "in",
            "--output",
            "out.mov",
            "--converter-timeout",
            "0"
        ])
        .is_err());
        assert!(parse_run(&[
            "--input",
            "in",
            "--output",
            "out.mov",
            "--converter-timeout",
            "120"
        ])
        .is_ok());
    }

    #[test]
    fn deflicker_defaults_match_legacy_v4() {
        let options = parse_run(&["--input", "in", "--output", "out.mov"]).expect("parse");
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
    }

    #[test]
    fn parses_deflicker_options() {
        let options = parse_run(&[
            "--input",
            "in",
            "--output",
            "out.mov",
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
    }

    #[test]
    fn parses_preview_profile_and_defaults_to_srgb() {
        let options = parse_run(&["--input", "in", "--output", "out.mov"]).expect("parse");
        assert_eq!(options.preview_profile.id, "srgb-preview");
        for id in color_profile::PROFILES.iter().map(|profile| profile.id) {
            let options = parse_run(&["--input", "in", "--output", "out.mov", "--preview-profile", id])
                .expect("parse");
            assert_eq!(options.preview_profile.id, id);
        }
        let options = parse_run(&[
            "--input",
            "in",
            "--output",
            "out.mov",
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
                "out.mov",
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
        let error = parse_run(&["--input", "in", "--output", "out.mov", "--preview-profile"])
            .expect_err("missing value");
        assert!(error.message.contains("missing value"));
        let error = parse_run(&["--input", "in", "--output", "out.mov", "--preview-profile="])
            .expect_err("empty inline value");
        assert_eq!(error.code, EXIT_USAGE);
    }

    #[test]
    fn encoder_flags_form_the_explicit_cli_layer() {
        let options = parse_run(&[
            "--input",
            "in",
            "--output",
            "out.mov",
            "--encoder",
            "libx264",
            "--pix-fmt",
            "yuv420p",
            "--container",
            "mp4",
            "--codec-profile",
            "high",
            "--crf",
            "20",
            "--output-option=-preset=medium",
            "--output-option=-g=12",
            "--resize",
            "4096x2734",
        ])
        .expect("parse");
        assert_eq!(options.encoder.encoder.as_deref(), Some("libx264"));
        assert_eq!(options.encoder.pix_fmt.as_deref(), Some("yuv420p"));
        assert_eq!(options.encoder.container, Some(Container::Mp4));
        assert_eq!(options.encoder.profile.as_deref(), Some("high"));
        assert_eq!(options.encoder.rate, Some(RateControl::Crf(20)));
        assert_eq!(
            options.encoder.options,
            Some(vec![
                ("-preset".into(), "medium".into()),
                ("-g".into(), "12".into())
            ])
        );
        assert_eq!(options.resize, Some((4096, 2734)));
        assert!(USAGE.contains("--encoder"));
        assert!(USAGE.contains("--resize"));
        assert!(USAGE.contains("--output <VIDEO_FILE>"));
    }

    #[test]
    fn defaults_leave_the_cli_layer_empty_for_graph_inheritance() {
        let options = parse_run(&["--input", "in", "--output", "out.mov"]).expect("parse");
        assert!(options.encoder.is_empty());
        assert_eq!(options.resize, None);
    }

    #[test]
    fn rejects_conflicting_or_invalid_encoder_flags() {
        let base = ["--input", "in", "--output", "out.mov"];
        for extra in [
            vec!["--encoder", "h264_videotoolbox"],
            vec!["--container", "avi"],
            vec!["--crf", "18", "--bitrate", "1000"],
            vec!["--crf", "-1"],
            vec!["--qscale", "64"],
            vec!["--bitrate", "0"],
            vec!["--output-option=-i=in.mov"],
            vec!["--output-option=-vf=scale=2:2"],
            vec!["--output-option=output.mov"],
            vec!["--output-option=-preset"],
            vec!["--resize", "0x10"],
            vec!["--resize", "4096"],
            vec!["--resize", "4096x"],
        ] {
            let mut args: Vec<&str> = base.to_vec();
            args.extend(extra.iter());
            let error = parse_run(&args).expect_err("must reject");
            assert_eq!(error.code, EXIT_USAGE, "{extra:?}: {}", error.message);
        }
        let error = parse_run(&["--input", "in", "--output", "out.mov", "--crf", "18", "--qscale", "2"])
            .expect_err("conflict");
        assert!(error.message.contains("conflicts"), "{}", error.message);
    }

    #[test]
    fn duplicate_input_or_output_is_rejected() {
        let error = parse_run(&["--input", "a", "--input", "b", "--output", "out.mov"])
            .expect_err("duplicate input");
        assert!(error.message.contains("--input"));
        let error = parse_run(&["--input", "in", "--output", "a.mov", "--output", "b.mov"])
            .expect_err("duplicate output");
        assert!(error.message.contains("--output"));
    }

    #[test]
    fn rejects_invalid_deflicker_options() {
        let base = ["--input", "in", "--output", "out.mov"];
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
        assert!(parse_run(&["--input", "in", "--output", "out.mov", "--ev-limits", "none"]).is_ok());
        assert!(parse_run(&[
            "--input",
            "in",
            "--output",
            "out.mov",
            "--ev-limits",
            "unbounded"
        ])
        .is_ok());
    }
}
