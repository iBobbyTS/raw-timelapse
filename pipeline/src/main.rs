//! rawtl-linear-video: DNG frame sequence -> streaming H.264 output.mp4.
//!
//! The deflicker node (legacy V4 semantics) is enabled by default; every pass,
//! failure path, report and publication rule lives in the modules below.

mod audit;
mod cli;
mod deflicker_run;
mod error;
mod fallback;
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
pub use rawtl_linear_video::transport;

use cli::{Options, Parsed};
use error::{Error, Result, EXIT_PREVIEW, EXIT_TOOL, EXIT_VALIDATION};
use order::SourceFrame;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use transport::DataEnvelope;

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
    pub probe: video::ProbeInfo,
    pub bytes: u64,
    pub timings: BTreeMap<String, Duration>,
    pub video_artifact: DataEnvelope,
}

pub fn encode_and_validate(
    context: &RunContext<'_>,
    evs: &[f64],
) -> Result<(frame::FrameSequence, EncodedStage)> {
    let options = context.options;
    let output = context.partial_dir.join("output.mp4");
    let mut timings = BTreeMap::new();
    let phase = Instant::now();
    let sequence = video::encode_stream(context, evs, &output)?;
    timings.insert("encode".into(), phase.elapsed());
    let phase = Instant::now();
    let probe = video::validate_preview(
        &options.ffprobe,
        &options.ffmpeg,
        &output,
        sequence.width,
        sequence.height,
        sequence.frames.len(),
        options.fps,
        options.preview_profile,
    )?;
    timings.insert("validate_video".into(), phase.elapsed());
    audit::write_inventory(
        context.output,
        &sequence
            .frames
            .iter()
            .map(|f| f.source_name.clone())
            .collect::<Vec<_>>(),
    )?;
    let bytes = video::file_size(&output)
        .ok_or_else(|| Error::new(EXIT_PREVIEW, "validated video disappeared"))?;
    let video_artifact = video::video_envelope(
        &output,
        video::PREVIEW_MEDIA_TYPE,
        &probe.codec_name,
        probe.frames,
        "ffmpeg h264 streaming encoder",
        options.preview_profile,
    )
    .map_err(|e| Error::new(EXIT_VALIDATION, e.to_string()))?;
    Ok((
        sequence,
        EncodedStage {
            probe,
            bytes,
            timings,
            video_artifact,
        },
    ))
}

/// Build and write the manifest, then publish the validated output.mp4.
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
        probe: &stage.probe,
        bytes: stage.bytes,
        ffmpeg_version: tool_version(&options.ffmpeg).unwrap_or_else(|| "UNKNOWN".to_string()),
        ffprobe_version: tool_version(&options.ffprobe).unwrap_or_else(|| "UNKNOWN".to_string()),
        timings: &stage.timings,
        deflicker,
    });
    manifest::write(&output.join("manifest.json"), &manifest_json)?;

    let final_video = output.join(video::FINAL_NAME);
    video::publish(&stage.video_artifact, &final_video)?;

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
    println!("video:        {}", final_video.display());
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
