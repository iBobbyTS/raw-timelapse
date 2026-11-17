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
    --graph <FILE>            Typed graph JSON (default: prepare P1/B32, solve 8, develop 4)
    --fps <N[/D]>              Frame rate as an integer or rational (default: 30)
    --white-balance <MODE>     first-frame | per-frame (default: first-frame)
                               first-frame freezes frame 1's as-shot neutral for the sequence;
                               per-frame decodes every frame with its own RAW as-shot neutral
    --dng-linear-frame <PATH>  Native frame producer (default: native/build/dng-linear-frame)
    --ffmpeg <PATH>            ffmpeg executable (default: /opt/homebrew/opt/ffmpeg-full/bin/ffmpeg)
    --ffprobe <PATH>           ffprobe executable (default: /opt/homebrew/opt/ffmpeg-full/bin/ffprobe)
    --preview-profile <ID>     Output colour/Log profile (default: srgb-preview)
                               one of: srgb-preview, dji-dlog-dgamut,
                               dji-dlog2-dgamut2,
                               canon-log2-cinema-gamut, sony-slog3-sgamut3,
                               sony-slog3-sgamut3-cine, nikon-nlog-bt2020,
                               canon-log2-cinema-gamut-rev-eng-crm-dev,
                               canon-log3-cinema-gamut-rev-eng-crm-dev
                               Canon names: clog2(white paper), clog2(rev-eng crm-dev),
                               clog3 (rev-eng crm-dev)
    --output-cat <MODE>        profile|bradford|cat02 (default: profile)
                               profile keeps the selected profile's fixed reference
                               adaptation; an explicit different method changes the
                               output pixels and is reported as custom-adaptation
    --camera-color <MODE>      auto|generic|canon-clog3 (default: auto)
    --log-reference-gain <N|auto>
                               Positive finite output gain, all profiles (default: 1); or auto:
                               one sequence-wide gain (D-Log 2 aims at +2 EV, other Logs +3 EV)
                               capped so the scanned sequence peak never clips the Log domain
                               (Log profiles only; costs one extra develop pass over the run)
    --signal-bits <10|12>     Depth of the single YCbCr quantization handoff into the encoder
                               (default: 10; 12 matches ProRes 4444's decode depth, finer codes)
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
    --lens-correction <MODE>   auto | off (default: auto; cannot undo upstream correction)
    --lens-vignette <MODE>     auto | off (default: auto), likewise --lens-distortion / --lens-ca
    --lens-vignette-strength N Exposure-domain strength 0..1 (default: 1)
    --lens-resampling <MODE>   bilinear | quality (default: quality; quality costs more per frame)
    --lens-crop <MODE>         preserve | safe (default: preserve; safe is fixed over video;
                               sequence-safe is an accepted legacy alias of safe)
    --exiftool <PATH>          Read-only Prepare metadata tool (default: /opt/homebrew/bin/exiftool)
    --lens-profile-dir <DIR>   Installed LCP root, repeatable; replaces default Adobe roots
    --converter <PATH>         Adobe DNG Converter override passed to the native tool
    --converter-timeout <SEC>  Total timeout per ADC call, including batches (1-3600 seconds)
    --verify-output           Verify the encoded output in full after encoding: frame-count
                               decode plus a complete decode (default: container metadata only)
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
    <VIDEO_FILE>               deliverable, published by a single rename; checked against
                               container metadata by default (--verify-output adds frame
                               counting and a complete decode)
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

/// Delivery reference gain selection. `Fixed` keeps the historical numeric
/// contract; `Auto` resolves to one sequence-wide gain after the exposure
/// plan exists, before the first colour conversion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReferenceGain {
    Auto,
    Fixed(f64),
}

impl ReferenceGain {
    /// Parse the `--log-reference-gain` value: `auto` or a positive finite
    /// number. Profile-domain rejection happens once the profile is known.
    fn parse(value: &str) -> Result<Self> {
        if value == "auto" {
            return Ok(Self::Auto);
        }
        let gain: f64 = value
            .parse()
            .map_err(|_| Error::new(EXIT_USAGE, "invalid --log-reference-gain"))?;
        if !gain.is_finite() || gain <= 0.0 {
            return Err(Error::new(
                EXIT_USAGE,
                "--log-reference-gain must be positive and finite",
            ));
        }
        Ok(Self::Fixed(gain))
    }

    /// Run-log token: `auto` or the number, matching historical f64 Display.
    pub fn token(&self) -> String {
        match self {
            Self::Auto => "auto".to_string(),
            Self::Fixed(gain) => gain.to_string(),
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
    pub camera_color: color_profile::CameraColor,
    /// Frozen output CAT selection/realization resolved at startup; per-pixel
    /// conversion uses these coefficients.
    pub output_transform: color_profile::OutputTransform,
    pub log_reference_gain: ReferenceGain,
    /// Depth of the single YCbCr quantization handoff into the encoder.
    pub signal_depth: color_profile::SignalDepth,
    pub preview_profile: &'static color_profile::ColorProfile,
    pub converter: Option<String>,
    pub converter_timeout: Option<u64>,
    pub lens: crate::lens::Settings,
    pub deflicker: DeflickerConfig,
    /// Explicit CLI encoder layer; empty when no encoder flag was given.
    pub encoder: Patch,
    /// Explicit CLI resize; `None` keeps the graph's geometry (source by default).
    pub resize: Option<(u32, u32)>,
    /// Opt in to the full output verification pass (frame-count decode plus a
    /// complete decode of the deliverable). Default: container metadata only.
    pub verify_output: bool,
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
    let mut output_cat: Option<color_profile::OutputCatSelection> = None;
    let mut log_reference_gain: Option<ReferenceGain> = None;
    let mut signal_depth = color_profile::SignalDepth::Ten;
    let mut camera_color = None;
    let mut converter: Option<String> = None;
    let mut converter_timeout: Option<u64> = None;
    let mut lens = crate::lens::Settings { mode: crate::lens::Mode::Auto, ..Default::default() };
    let mut deflicker = DeflickerConfig::default();
    let mut encoder = Patch::default();
    let mut output_options: Vec<String> = Vec::new();
    let mut rate: Option<RateControl> = None;
    let mut rate_flag: Option<&'static str> = None;
    let mut resize: Option<(u32, u32)> = None;
    let mut verify_output = false;
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
            "--output-cat" => {
                if output_cat.is_some() {
                    return Err(Error::new(EXIT_USAGE, "duplicate --output-cat"));
                }
                let value = take_value(args, &mut index, flag, inline_value)?;
                output_cat = Some(match value.as_str() {
                    "profile" => color_profile::OutputCatSelection::Profile,
                    other => {
                        let registry = color_profile::CatRegistry::standard();
                        registry
                            .lookup(other)
                            .map(|algorithm| {
                                color_profile::OutputCatSelection::Method(algorithm.token)
                            })
                            .ok_or_else(|| {
                                Error::new(
                                    EXIT_USAGE,
                                    format!(
                                        "invalid --output-cat {value:?}: expected profile or one \
                                         of {}",
                                        registry.tokens()
                                    ),
                                )
                            })?
                    }
                });
            }
            "--camera-color" => {
                if camera_color.is_some() { return Err(Error::new(EXIT_USAGE, "duplicate --camera-color")); }
                let value = take_value(args, &mut index, flag, inline_value)?;
                camera_color = Some(match value.as_str() {
                    "auto" => color_profile::CameraColor::Auto,
                    "generic" => color_profile::CameraColor::Generic,
                    "canon-clog3" => color_profile::CameraColor::CanonClog3,
                    _ => return Err(Error::new(EXIT_USAGE, "invalid --camera-color: expected auto|generic|canon-clog3")),
                });
            }
            "--log-reference-gain" => {
                let value = take_value(args, &mut index, flag, inline_value)?;
                log_reference_gain = Some(ReferenceGain::parse(&value)?);
            }
            "--signal-bits" => {
                let value = take_value(args, &mut index, flag, inline_value)?;
                signal_depth =
                    color_profile::SignalDepth::parse(&value).map_err(|e| Error::new(EXIT_USAGE, e))?;
            }
            "--graph" => {
                graph = Some(PathBuf::from(take_value(
                    args,
                    &mut index,
                    flag,
                    inline_value,
                )?))
            }
            "--lens-vignette" | "--lens-distortion" | "--lens-ca" => {
                let value = take_value(args, &mut index, flag, inline_value)?;
                let enabled = match value.as_str() {"auto"=>true,"off"=>false,_=>return Err(Error::new(EXIT_USAGE,format!("{flag} expects auto or off")))};
                let component = match flag {"--lens-vignette"=>0,"--lens-distortion"=>1,_=>2};
                lens.policy.components[component] = enabled;
            }
            "--lens-vignette-strength" => {
                let value = take_value(args, &mut index, flag, inline_value)?;
                let strength = value.parse::<f64>().map_err(|_|Error::new(EXIT_USAGE,"lens vignette strength must be 0..1"))?;
                if !strength.is_finite() || !(0.0..=1.0).contains(&strength) {return Err(Error::new(EXIT_USAGE,"lens vignette strength must be 0..1"));}
                lens.policy.vignette_strength = strength;
            }
            "--lens-resampling" => {
                let value = take_value(args, &mut index, flag, inline_value)?;
                lens.policy.sampling = match value.as_str() {"bilinear"=>crate::lens::Sampling::Bilinear,"quality"=>crate::lens::Sampling::Quality,_=>return Err(Error::new(EXIT_USAGE,"lens resampling expects bilinear or quality"))};
            }
            "--lens-crop" => {
                let value = take_value(args, &mut index, flag, inline_value)?;
                lens.crop = match value.as_str() {"preserve"=>crate::lens::Crop::Preserve,"safe"|"sequence-safe"=>crate::lens::Crop::Safe,_=>return Err(Error::new(EXIT_USAGE,"lens crop expects preserve or safe (sequence-safe is a legacy alias of safe)"))};
            }
            "--lens-correction" => {
                let value = take_value(args, &mut index, flag, inline_value)?;
                lens.mode = match value.as_str() {
                    "auto" => crate::lens::Mode::Auto, "off" => crate::lens::Mode::Off,
                    _ => return Err(Error::new(EXIT_USAGE, "--lens-correction expects auto or off")),
                };
            }
            "--exiftool" | "--lens-profile-dir" => {
                let value = PathBuf::from(take_value(args, &mut index, flag, inline_value)?);
                if !value.is_absolute() { return Err(Error::new(EXIT_USAGE, format!("{flag} requires an absolute path"))); }
                if flag == "--exiftool" { lens.exiftool = value; } else { lens.profile_roots.push(value); }
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
            "--verify-output" => {
                if inline_value.is_some() {
                    return Err(Error::new(
                        EXIT_USAGE,
                        "--verify-output takes no value",
                    ));
                }
                if verify_output {
                    return Err(Error::new(
                        EXIT_USAGE,
                        "--verify-output may be given only once",
                    ));
                }
                verify_output = true;
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

    if camera_color == Some(color_profile::CameraColor::CanonClog3)
        && preview_profile.transfer != color_profile::Transfer::CanonLog3RevEng {
        return Err(Error::new(EXIT_USAGE, "--camera-color canon-clog3 requires the CLog3 profile"));
    }
    if log_reference_gain == Some(ReferenceGain::Auto)
        && preview_profile.transfer == color_profile::Transfer::Srgb {
        return Err(Error::new(
            EXIT_USAGE,
            "--log-reference-gain auto requires a camera Log profile; srgb-preview has no Log encoding headroom",
        ));
    }
    encoder.rate = rate;
    if !output_options.is_empty() {
        encoder.options = Some(
            encoder::parse_output_options(&output_options)
                .map_err(|message| Error::new(EXIT_USAGE, message))?,
        );
    }
    let output_cat = output_cat.unwrap_or(color_profile::OutputCatSelection::Profile);
    // Resolve, validate and compose the output CAT once, before any RAW work.
    let output_transform = preview_profile
        .resolve_output_cat(output_cat, &color_profile::CatRegistry::standard())
        .map_err(|message| Error::new(EXIT_USAGE, message))?;
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
        output_transform,
        camera_color: camera_color.unwrap_or(color_profile::CameraColor::Auto),
        log_reference_gain: log_reference_gain.unwrap_or(ReferenceGain::Fixed(1.0)),
        signal_depth,
        converter,
        converter_timeout,
        lens,
        deflicker,
        encoder,
        resize,
        verify_output,
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
    fn signal_bits_accepts_ten_and_twelve_and_rejects_others() {
        let base = ["--input", "in", "--output", "out.mov"];
        assert_eq!(
            parse_run(&base).unwrap().signal_depth,
            color_profile::SignalDepth::Ten
        );
        assert_eq!(
            parse_run(&[base.as_slice(), &["--signal-bits", "12"]].concat())
                .unwrap()
                .signal_depth,
            color_profile::SignalDepth::Twelve
        );
        assert_eq!(
            parse_run(&[base.as_slice(), &["--signal-bits", "10"]].concat())
                .unwrap()
                .signal_depth,
            color_profile::SignalDepth::Ten
        );
        for bad in ["9", "16", "twelve", ""] {
            assert!(
                parse_run(&[base.as_slice(), &["--signal-bits", bad]].concat()).is_err(),
                "token {bad:?} must be rejected"
            );
        }
    }
    #[test]
    fn log_reference_gain_accepts_auto_for_log_profiles() {
        let options = parse_run(&[
            "--input", "in", "--output", "out.mov",
            "--preview-profile", "dji-dlog2-dgamut2", "--log-reference-gain", "auto",
        ])
        .expect("parse");
        assert_eq!(options.log_reference_gain, ReferenceGain::Auto);
        assert_eq!(options.log_reference_gain.token(), "auto");
    }

    #[test]
    fn log_reference_gain_keeps_numeric_semantics() {
        let options = parse_run(&[
            "--input", "in", "--output", "out.mov",
            "--preview-profile", "canon-log3-cinema-gamut-rev-eng-crm-dev",
            "--log-reference-gain", "2.5",
        ])
        .expect("parse");
        assert_eq!(options.log_reference_gain, ReferenceGain::Fixed(2.5));
        assert_eq!(options.log_reference_gain.token(), "2.5");
        let options = parse_run(&["--input", "in", "--output", "out.mov"]).expect("parse");
        assert_eq!(options.log_reference_gain, ReferenceGain::Fixed(1.0));
    }

    #[test]
    fn log_reference_gain_auto_is_rejected_for_srgb_preview() {
        let error = parse_run(&[
            "--input", "in", "--output", "out.mov",
            "--preview-profile", "srgb-preview", "--log-reference-gain", "auto",
        ])
        .expect_err("srgb rejects auto");
        assert!(error.message.contains("auto requires a camera Log profile"));
    }

    #[test]
    fn log_reference_gain_auto_rejects_malformed_tokens() {
        for value in ["AUTO", "auto=1", "auto ", "1auto"] {
            let error = parse_run(&[
                "--input", "in", "--output", "out.mov",
                "--preview-profile", "dji-dlog2-dgamut2",
                "--log-reference-gain", value,
            ])
            .expect_err("malformed token");
            assert!(error.message.contains("--log-reference-gain"));
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
    fn lens_policy_defaults_and_explicit_controls() {
        let defaults=parse_run(&["--input","in","--output","out.mov"]).unwrap();
        assert_eq!(defaults.lens.mode,crate::lens::Mode::Auto);
        assert_eq!(defaults.lens.crop,crate::lens::Crop::Preserve);
        assert_eq!(defaults.lens.policy,crate::lens::Policy::default());
        assert_eq!(defaults.lens.policy.sampling,crate::lens::Sampling::Quality);
        let bilinear=parse_run(&["--input","in","--output","out.mov","--lens-resampling","bilinear"]).unwrap();
        assert_eq!(bilinear.lens.policy.sampling,crate::lens::Sampling::Bilinear);
        let options=parse_run(&["--input","in","--output","out.mov","--lens-vignette","off","--lens-distortion","off","--lens-vignette-strength","0.5","--lens-resampling","quality","--lens-crop","sequence-safe"]).unwrap();
        assert_eq!(options.lens.policy.components,[false,false,true]);
        assert_eq!(options.lens.policy.vignette_strength,0.5);
        assert_eq!(options.lens.policy.sampling,crate::lens::Sampling::Quality);
        assert!(options.lens.crop.is_safe());
        for spelling in ["safe", "sequence-safe"] {
            let alias = parse_run(&["--input", "in", "--output", "out.mov", "--lens-crop", spelling]).unwrap();
            assert_eq!(alias.lens.crop, crate::lens::Crop::Safe, "spelling {spelling:?}");
        }
        for (flag,value) in [("--lens-vignette-strength","NaN"),("--lens-vignette-strength","-0.1"),("--lens-vignette-strength","1.1"),("--lens-ca","on"),("--lens-resampling","unknown"),("--lens-crop","per-frame")] {
            assert!(parse_run(&["--input","in","--output","out.mov",flag,value]).is_err());
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
    fn usage_lists_every_registered_profile_id() {
        // T13: the help surface is a hard-coded list; adding a profile must not
        // leave the CLI documentation behind the registry.
        for profile in color_profile::PROFILES.iter() {
            assert!(
                USAGE.contains(profile.id),
                "CLI help is missing registered profile {:?}",
                profile.id
            );
        }
        assert!(USAGE.contains("--output-cat <MODE>        profile|bradford|cat02"));
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
    fn parses_output_cat_default_same_value_and_override() {
        let default = parse_run(&["--input", "in", "--output", "out.mov"]).expect("parse");
        assert_eq!(default.output_transform.requested.token(), "profile");
        assert_eq!(
            default.output_transform.realization,
            color_profile::OutputCatRealization::Reference
        );
        assert_eq!(default.output_transform.method, "bradford");

        let same = parse_run(&[
            "--input", "in", "--output", "out.mov", "--output-cat", "bradford",
        ])
        .expect("parse");
        assert_eq!(same.output_transform.requested.token(), "bradford");
        assert_eq!(
            same.output_transform.realization,
            color_profile::OutputCatRealization::Reference
        );
        assert_eq!(
            same.output_transform.fingerprint,
            default.output_transform.fingerprint,
            "an explicit same-value selection must keep the fixed reference realization"
        );
        assert_eq!(
            same.output_transform.composed_matrix,
            default.output_transform.composed_matrix
        );

        let custom = parse_run(&[
            "--input", "in", "--output", "out.mov",
            "--preview-profile", "nikon-nlog-bt2020", "--output-cat", "cat02",
        ])
        .expect("parse");
        assert_eq!(custom.output_transform.requested.token(), "cat02");
        assert_eq!(custom.output_transform.method, "cat02");
        assert_eq!(
            custom.output_transform.realization,
            color_profile::OutputCatRealization::CustomAdaptation
        );
        assert_ne!(
            custom.output_transform.fingerprint,
            custom.preview_profile.default_output_transform().fingerprint
        );
        // Flag order does not matter: the profile is resolved after parsing.
        let reversed = parse_run(&[
            "--input", "in", "--output", "out.mov",
            "--output-cat", "cat02", "--preview-profile", "nikon-nlog-bt2020",
        ])
        .expect("parse");
        assert_eq!(
            reversed.output_transform.composed_matrix,
            custom.output_transform.composed_matrix
        );
        assert_eq!(reversed.output_transform.target_white_xy, [0.3127, 0.3290]);
    }

    #[test]
    fn rejects_invalid_output_cat_values_and_duplicates() {
        for value in ["", "rec2020", "Profile", "profile ", "bradford-cat", "CAT02"] {
            let args = [
                "--input", "in", "--output", "out.mov", "--output-cat", value,
            ];
            let error = parse_run(&args).expect_err("must reject");
            assert_eq!(error.code, EXIT_USAGE, "{value:?}");
            assert!(error.message.contains("--output-cat"), "{}", error.message);
        }
        let error = parse_run(&["--input", "in", "--output", "out.mov", "--output-cat"])
            .expect_err("missing value");
        assert!(error.message.contains("missing value"));
        let error = parse_run(&[
            "--input", "in", "--output", "out.mov",
            "--output-cat", "profile", "--output-cat", "profile",
        ])
        .expect_err("duplicate");
        assert_eq!(error.code, EXIT_USAGE);
        assert!(error.message.contains("duplicate --output-cat"));
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
    fn full_output_verification_is_opt_in_and_single() {
        let options = parse_run(&["--input", "in", "--output", "out.mov"]).expect("parse");
        assert!(!options.verify_output);
        let options =
            parse_run(&["--input", "in", "--output", "out.mov", "--verify-output"])
                .expect("parse");
        assert!(options.verify_output);
        assert!(USAGE.contains("--verify-output"));
        assert!(
            parse_run(&[
                "--input",
                "in",
                "--output",
                "out.mov",
                "--verify-output",
                "--verify-output"
            ])
            .is_err()
        );
        assert!(
            parse_run(&["--input", "in", "--output", "out.mov", "--verify-output=1"]).is_err()
        );
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
