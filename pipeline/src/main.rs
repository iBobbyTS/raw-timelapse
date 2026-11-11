//! rawtl-linear-video: DNG frame sequence -> 16-bit scene-linear FFV1 video
//! plus a display-referred sRGB or camera-Log review preview.
//!
//! The deflicker node (legacy V4 semantics) is enabled by default; every pass,
//! failure path, report and publication rule lives in the modules below.

mod audit;
mod cli;
mod deflicker_run;
mod fallback;
mod error;
mod frame;
mod guard;
mod manifest;
mod order;
mod proc;
mod video;

// The binary and the library compile the same source files. Re-exporting the
// library modules keeps a single nominal `Json`/`deflicker`/colour-profile type
// across the engine and the process orchestration.
pub use rawtl_linear_video::color_profile;
pub use rawtl_linear_video::deflicker;
pub use rawtl_linear_video::json;

use cli::{Options, Parsed};
use error::{Error, Result, EXIT_PUBLISH, EXIT_TOOL};
use order::SourceFrame;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = match cli::parse(&args) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("error: {}\n\n{}", err.message, cli::USAGE);
            std::process::exit(err.code);
        }
    };
    match parsed {
        Parsed::Help => print!("{}", cli::USAGE),
        Parsed::Version => println!("rawtl-linear-video {}", env!("CARGO_PKG_VERSION")),
        Parsed::Run(options) => {
            if let Err(err) = run(&options) {
                eprintln!("error: {}", err.message);
                std::process::exit(err.code);
            }
        }
    }
}

pub struct RunContext<'a> {
    pub options: &'a Options,
    pub input: &'a Path,
    pub output: &'a Path,
    pub sources: &'a [SourceFrame],
    pub partial_dir: PathBuf,
    pub started: Instant,
}

fn run(options: &Options) -> Result<()> {
    let started = Instant::now();
    let input = guard::canonical_input(&options.input)?;
    let output = guard::canonical_output(&options.output)?;
    guard::ensure_disjoint(&input, &output)?;
    let output = guard::prepare_output(&output)?;
    guard::ensure_output_empty(&output)?;

    // The guarded, writable output directory is established. From here on every
    // failure (including required-executable and source-enumeration failures)
    // must retain a report; guard rejections above stay report-free.
    let partial_dir = output.join(".rawtl-partial");
    let outcome = deflicker_run::run(options, &input, &output, started);
    if outcome.is_err() && partial_dir.exists() {
        eprintln!(
            "note: partial/intermediate artifacts retained at {}; no final deliverable name was published",
            partial_dir.display()
        );
    }
    outcome
}

pub struct EncodedStage {
    pub temp_linear: PathBuf,
    pub temp_preview: PathBuf,
    pub linear_probe: video::ProbeInfo,
    pub roundtrip: video::RoundtripReport,
    pub preview_probe: video::ProbeInfo,
    pub linear_bytes: u64,
    pub preview_bytes: u64,
    pub timings: BTreeMap<String, Duration>,
}

/// Encode the accepted frame directory, validate the stream, prove the lossless
/// roundtrip and render/validate the preview. Shared by both deflicker paths.
pub fn encode_and_validate(
    context: &RunContext<'_>,
    sequence: &frame::FrameSequence,
) -> Result<EncodedStage> {
    let options = context.options;
    let output = context.output;
    let partial_dir = &context.partial_dir;
    fs::create_dir_all(partial_dir).map_err(|err| {
        Error::new(
            EXIT_PUBLISH,
            format!(
                "cannot create partial artifact directory {}: {err}",
                partial_dir.display()
            ),
        )
    })?;
    let temp_linear = partial_dir.join("linear.mkv");
    let temp_preview = partial_dir.join("preview.mp4");
    let preview_raw = partial_dir.join("preview.rgb");
    let logs_dir = output.join("logs");
    let count = sequence.frames.len();

    let mut timings: BTreeMap<String, Duration> = BTreeMap::new();
    timings.insert("frames".to_string(), sequence.total_duration);

    println!("\nencoding 16-bit FFV1 linear video (gbrp16le, lossless)...");
    let phase = Instant::now();
    video::encode_linear(
        &options.ffmpeg,
        &output.join("frames"),
        count,
        options.fps,
        &temp_linear,
        &logs_dir.join("encode-linear.log"),
    )?;
    timings.insert("encode_linear".to_string(), phase.elapsed());
    println!("encoder finished in {:.2}s", phase.elapsed().as_secs_f64());

    let phase = Instant::now();
    let linear_probe = video::probe_stream(&options.ffprobe, &temp_linear)?;
    video::validate_linear(
        &linear_probe,
        sequence.width,
        sequence.height,
        count,
        options.fps,
    )?;
    timings.insert("validate_linear".to_string(), phase.elapsed());
    println!(
        "ffprobe validation: codec={} pix_fmt={} {}x{} frames={} rate={}",
        linear_probe.codec_name,
        linear_probe.pix_fmt,
        linear_probe.width,
        linear_probe.height,
        linear_probe.frames,
        linear_probe.avg_frame_rate
    );

    println!("verifying lossless roundtrip frame by frame...");
    let phase = Instant::now();
    let roundtrip = video::verify_roundtrip(
        &options.ffmpeg,
        &output.join("frames"),
        &temp_linear,
        sequence.width,
        sequence.height,
        count,
        options.fps,
        &logs_dir.join("roundtrip.log"),
    )?;
    timings.insert("roundtrip".to_string(), phase.elapsed());
    println!(
        "lossless roundtrip: {} frames, {} bytes/frame, byte-identical in {:.2}s",
        roundtrip.frames_compared,
        roundtrip.frame_bytes,
        roundtrip.elapsed.as_secs_f64()
    );

    if options.preview_profile.id == "srgb-preview" {
        println!("rendering display-referred sRGB preview...");
    } else {
        println!(
            "rendering display-referred {} preview...",
            options.preview_profile.id
        );
    }
    let phase = Instant::now();
    video::render_preview(
        &options.ffmpeg,
        &output.join("frames"),
        sequence.width,
        sequence.height,
        count,
        options.fps,
        &preview_raw,
        &logs_dir.join("render-preview.log"),
        options.preview_profile,
    )?;
    timings.insert("render_preview".to_string(), phase.elapsed());

    let phase = Instant::now();
    video::encode_preview(
        &options.ffmpeg,
        &preview_raw,
        sequence.width,
        sequence.height,
        count,
        options.fps,
        &temp_preview,
        &logs_dir.join("encode-preview.log"),
        options.preview_profile,
    )?;
    timings.insert("encode_preview".to_string(), phase.elapsed());

    let phase = Instant::now();
    let preview_probe = video::validate_preview(
        &options.ffprobe,
        &options.ffmpeg,
        &temp_preview,
        sequence.width,
        sequence.height,
        count,
        options.fps,
        options.preview_profile,
    )?;
    timings.insert("validate_preview".to_string(), phase.elapsed());
    println!(
        "preview validation: codec={} pix_fmt={} {}x{} frames={} full decode passed",
        preview_probe.codec_name,
        preview_probe.pix_fmt,
        preview_probe.width,
        preview_probe.height,
        preview_probe.frames
    );

    let linear_bytes = video::file_size(&temp_linear).ok_or_else(|| {
        Error::new(
            EXIT_TOOL,
            format!("validated video disappeared: {}", temp_linear.display()),
        )
    })?;
    let preview_bytes = video::file_size(&temp_preview).ok_or_else(|| {
        Error::new(
            EXIT_TOOL,
            format!("validated preview disappeared: {}", temp_preview.display()),
        )
    })?;

    Ok(EncodedStage {
        temp_linear,
        temp_preview,
        linear_probe,
        roundtrip,
        preview_probe,
        linear_bytes,
        preview_bytes,
        timings,
    })
}

/// Build and write the manifest, then publish both validated deliverables.
pub fn publish_run(
    context: &RunContext<'_>,
    sequence: &frame::FrameSequence,
    mut stage: EncodedStage,
    deflicker: json::Json,
) -> Result<()> {
    let options = context.options;
    let output = context.output;
    stage
        .timings
        .insert("total".to_string(), context.started.elapsed());

    let manifest_json = manifest::build(&manifest::ManifestInput {
        options,
        input: context.input,
        output,
        sequence,
        linear_probe: &stage.linear_probe,
        roundtrip: &stage.roundtrip,
        preview_probe: &stage.preview_probe,
        linear_bytes: stage.linear_bytes,
        preview_bytes: stage.preview_bytes,
        ffmpeg_version: tool_version(&options.ffmpeg).unwrap_or_else(|| "UNKNOWN".to_string()),
        ffprobe_version: tool_version(&options.ffprobe).unwrap_or_else(|| "UNKNOWN".to_string()),
        timings: &stage.timings,
        deflicker,
    });
    manifest::write(&output.join("manifest.json"), &manifest_json)?;

    let final_linear = output.join(video::LINEAR_FINAL_NAME);
    let final_preview = output.join(video::preview_final_name(options.preview_profile));
    video::publish(
        &stage.temp_linear,
        &final_linear,
        &stage.temp_preview,
        &final_preview,
    )?;

    match fs::remove_dir_all(&context.partial_dir) {
        Ok(()) => {}
        Err(err) => eprintln!(
            "warning: could not remove partial directory {}: {err}",
            context.partial_dir.display()
        ),
    }

    println!(
        "\nrun complete in {:.2}s",
        context.started.elapsed().as_secs_f64()
    );
    println!("linear video: {}", final_linear.display());
    if options.preview_profile.id == "srgb-preview" {
        println!("sRGB preview: {}", final_preview.display());
    } else {
        println!(
            "{} preview: {}",
            options.preview_profile.id,
            final_preview.display()
        );
    }
    println!("manifest:     {}", output.join("manifest.json").display());
    Ok(())
}

fn tool_version(program: &Path) -> Option<String> {
    let captured = proc::run_capture(program, &["-version".to_string()], EXIT_TOOL).ok()?;
    if !captured.status.success() {
        return None;
    }
    captured
        .stdout
        .lines()
        .next()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
}
