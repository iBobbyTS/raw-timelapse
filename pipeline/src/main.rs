//! rawtl-linear-video: DNG frame sequence -> one configured video file.
//!
//! The deflicker node (legacy V4 semantics) is enabled by default; every pass,
//! failure path, report and publication rule lives in the modules below.

mod audit;
mod cli;
mod deflicker_run;
mod digest;
mod error;
mod fallback;
mod frame;
mod graph_run;
mod guard;
mod manifest;
mod order;
mod proc;
mod resample;
mod video;

// The binary and the library compile the same source files. Re-exporting the
// library modules keeps a single nominal `Json`/`deflicker`/colour-profile type
// across the engine and the process orchestration.
pub use rawtl_linear_video::color_profile;
pub use rawtl_linear_video::deflicker;
pub use rawtl_linear_video::encoder;
pub use rawtl_linear_video::json;
pub use rawtl_linear_video::transport;

use cli::{Options, Parsed};
use error::{Result, EXIT_TOOL};
use order::SourceFrame;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use transport::DataEnvelope;

fn main() {
    proc::install_stop_handlers();
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
    pub retain_partial: bool,
    pub probe: video::ProbeInfo,
    pub bytes: u64,
    pub timings: BTreeMap<String, Duration>,
    pub video_artifact: DataEnvelope,
    pub encoder: encoder::ResolvedConfig,
    pub resize: Option<video::ResizeRecord>,
}

/// Build and write the manifest, then publish the validated configured video.
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
        encoder: &stage.encoder,
        resize: stage.resize,
    });
    manifest::write(&output.join("manifest.json"), &manifest_json)?;

    proc::check_stop()?;
    let final_video = output.join(stage.encoder.output_name());
    video::publish(&stage.video_artifact, &final_video)?;

    if stage.retain_partial {
        println!(
            "retained graph cache: {}",
            context.partial_dir.join("cache").display()
        );
    } else {
        match fs::remove_dir_all(&context.partial_dir) {
            Ok(()) => {}
            Err(err) => eprintln!(
                "warning: could not remove partial directory {}: {err}",
                context.partial_dir.display()
            ),
        }
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
