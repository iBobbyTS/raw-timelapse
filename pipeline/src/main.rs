//! rawtl-linear-video: DNG frame sequence -> 16-bit scene-linear FFV1 video
//! plus a display-referred sRGB review preview.

mod cli;
mod error;
mod frame;
mod guard;
mod json;
mod manifest;
mod order;
mod preview;
mod proc;
mod video;

use cli::{Options, Parsed};
use error::{Error, Result, EXIT_PUBLISH, EXIT_TOOL};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
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

fn run(options: &Options) -> Result<()> {
    let started = Instant::now();
    let input = guard::canonical_input(&options.input)?;
    let output = guard::prepare_output(&options.output)?;
    guard::ensure_disjoint(&input, &output)?;
    guard::ensure_output_empty(&output)?;

    proc::require_executable(&options.native_tool, "--dng-linear-frame")?;
    proc::require_executable(&options.ffmpeg, "--ffmpeg")?;
    proc::require_executable(&options.ffprobe, "--ffprobe")?;

    println!("rawtl-linear-video {}", env!("CARGO_PKG_VERSION"));
    println!("input:  {}", input.display());
    println!("output: {}", output.display());
    println!(
        "fps:    {} ({})",
        options.fps.arg(),
        if options.fps_explicit {
            "CLI override"
        } else {
            cli::LEGACY_FPS_PROVENANCE
        }
    );

    let sources = order::enumerate_dng(&input)?;
    println!("found {} DNG frame(s) in numeric order:", sources.len());
    for (index, source) in sources.iter().enumerate() {
        println!("  {:>3}. {}", index + 1, source.name);
    }

    let frames_dir = output.join("frames");
    let logs_dir = output.join("logs");
    let sequence = frame::produce(
        &options.native_tool,
        &sources,
        &frames_dir,
        &logs_dir,
        options.converter.as_deref(),
        options.converter_timeout,
    )?;
    println!(
        "frozen white balance: {} from {} (applied to every later frame)",
        frame::format_wb(sequence.reference_wb),
        sequence.reference_frame
    );
    println!(
        "frames complete: {} frames of {}x{} in {:.2}s",
        sequence.frames.len(),
        sequence.width,
        sequence.height,
        sequence.total_duration.as_secs_f64()
    );

    let partial_dir = output.join(".rawtl-partial");
    let outcome = finish(options, &input, &output, &sequence, &partial_dir, started);
    if outcome.is_err() && partial_dir.exists() {
        eprintln!(
            "note: partial/intermediate artifacts retained at {}; no final deliverable name was published",
            partial_dir.display()
        );
    }
    outcome
}

fn finish(
    options: &Options,
    input: &Path,
    output: &Path,
    sequence: &frame::FrameSequence,
    partial_dir: &Path,
    started: Instant,
) -> Result<()> {
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

    println!("rendering display-referred sRGB preview...");
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

    timings.insert("total".to_string(), started.elapsed());

    let manifest_json = manifest::build(&manifest::ManifestInput {
        options,
        input,
        output,
        sequence,
        linear_probe: &linear_probe,
        roundtrip: &roundtrip,
        preview_probe: &preview_probe,
        linear_bytes,
        preview_bytes,
        ffmpeg_version: tool_version(&options.ffmpeg).unwrap_or_else(|| "UNKNOWN".to_string()),
        ffprobe_version: tool_version(&options.ffprobe).unwrap_or_else(|| "UNKNOWN".to_string()),
        timings: &timings,
    });
    manifest::write(&output.join("manifest.json"), &manifest_json)?;

    let final_linear = output.join(video::LINEAR_FINAL_NAME);
    let final_preview = output.join(video::PREVIEW_FINAL_NAME);
    video::publish(&temp_linear, &final_linear, &temp_preview, &final_preview)?;

    match fs::remove_dir_all(partial_dir) {
        Ok(()) => {}
        Err(err) => eprintln!(
            "warning: could not remove partial directory {}: {err}",
            partial_dir.display()
        ),
    }

    println!("\nrun complete in {:.2}s", started.elapsed().as_secs_f64());
    println!("linear video: {}", final_linear.display());
    println!("sRGB preview: {}", final_preview.display());
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
