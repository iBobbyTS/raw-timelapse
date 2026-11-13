//! rawtl-linear-video: RAW/DNG frame sequence -> one video file and one log.
//!
//! The graph runtime owns preparation, measurement/solve, rendering, encoding
//! and event-driven cleanup. This module owns the product surface: the single
//! `--output` video file, its sibling run log, the run-specific scratch
//! directory, publication and the failure contract.

mod cli;
mod continuity;
mod digest;
mod error;
mod frame;
mod graph_run;
mod guard;
mod order;
mod proc;
mod resample;
mod run_log;
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
use error::Result;
use order::SourceFrame;
use run_log::{quote, RunLog};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
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
    /// Run-specific scratch root: converted cache and child transcripts.
    pub scratch: &'a Path,
    pub sources: &'a [SourceFrame],
    pub started: Instant,
}

pub struct EncodedStage {
    pub probe: video::ProbeInfo,
    pub bytes: u64,
    pub timings: BTreeMap<String, Duration>,
    pub video_artifact: DataEnvelope,
    pub resize: Option<video::ResizeRecord>,
}

fn run(options: &Options) -> Result<()> {
    let started = Instant::now();
    let input = guard::canonical_input(&options.input)?;
    let mut video = guard::canonical_artifact(&options.video)?;
    let mut log_path = guard::canonical_artifact(&options.log)?;
    guard::ensure_outside_input(&input, &[&video, &log_path])?;
    // Configuration failures must not create a directory, log, scratch or
    // output.
    graph_run::resolve_configuration(options)?;
    guard::prepare_parent(&video)?;
    video = guard::canonical_artifact(&video)?;
    log_path = guard::canonical_artifact(&log_path)?;
    guard::ensure_absent(&[&video, &log_path])?;

    let mut log = RunLog::create(&log_path, env!("CARGO_PKG_VERSION"))?;
    let scratch = match guard::create_scratch(&video) {
        Ok(scratch) => scratch,
        Err(err) => {
            let _ = log.line(&format!(
                "kind=run.error code={} message={}",
                err.code,
                quote(&err.message)
            ));
            return Err(err);
        }
    };
    let outcome = execute(options, &input, &video, &scratch, started, &mut log);
    if let Err(err) = outcome {
        let retained = scratch_has_content(&scratch);
        if retained {
            let _ = log.line(&format!(
                "kind=run.error code={} message={} scratch={} retained=true",
                err.code,
                quote(&err.message),
                quote(&scratch.display().to_string())
            ));
            eprintln!(
                "note: partial/intermediate artifacts retained at {} (converted cache, \
                 transcripts and any partial output); no video was published",
                scratch.display()
            );
        } else {
            let _ = log.line(&format!(
                "kind=run.error code={} message={} retained=false",
                err.code,
                quote(&err.message)
            ));
            let _ = fs::remove_dir_all(&scratch);
        }
        return Err(err);
    }
    Ok(())
}

fn execute(
    options: &Options,
    input: &Path,
    video: &Path,
    scratch: &Path,
    started: Instant,
    log: &mut RunLog,
) -> Result<()> {
    log.line(&format!(
        "kind=run.start input={} video={} log={} scratch={}",
        quote(&input.display().to_string()),
        quote(&video.display().to_string()),
        quote(&log.path().display().to_string()),
        quote(&scratch.display().to_string())
    ))?;
    println!("rawtl-linear-video {}", env!("CARGO_PKG_VERSION"));
    println!("input:   {}", input.display());
    println!("video:   {}", video.display());
    println!("log:     {}", log.path().display());
    println!("scratch: {}", scratch.display());
    println!(
        "fps:     {} ({})",
        options.fps.arg(),
        if options.fps_explicit {
            "CLI override"
        } else {
            cli::LEGACY_FPS_PROVENANCE
        }
    );
    let deflicker = &options.deflicker;
    if deflicker.enabled() {
        println!(
            "deflicker: enabled (strength {}, period {}, brightness {}, estimator {}, \
             ev_limits {})",
            deflicker.strength,
            deflicker.period,
            deflicker.brightness.as_str(),
            deflicker.estimator.as_str(),
            deflicker.ev_limits_text(),
        );
    } else {
        println!("deflicker: disabled (--strength 0: single pass, EV 0)");
    }
    log.line(&format!(
        "kind=settings fps={} white_balance={} strength={} period={} brightness={} \
         estimator={} ev_limits={} roi={} exclude_rois={} steps={} reference_gain={}",
        options.fps.arg(),
        options.white_balance.as_str(),
        deflicker.strength,
        deflicker.period,
        deflicker.brightness.as_str(),
        deflicker.estimator.as_str(),
        deflicker.ev_limits_text(),
        deflicker
            .roi
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(","),
        deflicker.exclude_rois.len(),
        deflicker.steps.len(),
        options.log_reference_gain
    ))?;

    proc::require_executable(&options.native_tool, "--dng-linear-frame")?;
    proc::require_executable(&options.ffmpeg, "--ffmpeg")?;
    proc::require_executable(&options.ffprobe, "--ffprobe")?;

    let sources = order::enumerate_dng(input)?;
    log.line(&format!(
        "kind=inventory frames={} first={} last={}",
        sources.len(),
        quote(&sources.first().map_or_else(String::new, |s| s.name.clone())),
        quote(&sources.last().map_or_else(String::new, |s| s.name.clone()))
    ))?;
    println!("found {} frame(s) in numeric order:", sources.len());
    for (index, source) in sources.iter().enumerate() {
        println!("  {:>3}. {}", index + 1, source.name);
    }

    let context = RunContext {
        options,
        scratch,
        sources: &sources,
        started,
    };
    let result = graph_run::run(&context, log)?;
    print_sequence_summary(&result.sequence, options.white_balance);

    let stage = &result.stage;
    log.line(&format!(
        "kind=video path={} bytes={} codec={} profile={} width={} height={} frames={} \
         pix_fmt={} range={} resize={}",
        quote(&video.display().to_string()),
        stage.bytes,
        stage.probe.codec_name,
        stage.probe.profile,
        stage.probe.width,
        stage.probe.height,
        stage.probe.frames,
        stage.probe.pix_fmt,
        stage.probe.color_range,
        match stage.resize {
            Some(record) => format!("{}x{}", record.width, record.height),
            None => "none".to_string(),
        }
    ))?;
    log.line(&format!(
        "kind=timing preflight={:.3} graph={:.3} validate={:.3}",
        stage
            .timings
            .get("preflight")
            .map_or(0.0, |value| value.as_secs_f64()),
        stage
            .timings
            .get("graph")
            .map_or(0.0, |value| value.as_secs_f64()),
        stage
            .timings
            .get("validate_video")
            .map_or(0.0, |value| value.as_secs_f64())
    ))?;

    proc::check_stop()?;
    let publish_started = Instant::now();
    video::publish(&stage.video_artifact, video)?;
    let published = publish_started.elapsed();
    let total = started.elapsed();

    log.line_best_effort(&format!(
        "kind=publish path={} duration={:.3}",
        quote(&video.display().to_string()),
        published.as_secs_f64()
    ));
    log.line_best_effort(&format!(
        "kind=timing.publish duration={:.3} total={:.3}",
        published.as_secs_f64(),
        total.as_secs_f64()
    ));
    if let Err(err) = fs::remove_dir_all(scratch) {
        eprintln!(
            "warning: could not remove run scratch {}: {err}",
            scratch.display()
        );
        log.line_best_effort(&format!(
            "kind=warning message={}",
            quote(&format!("could not remove run scratch: {err}"))
        ));
    }
    log.line_best_effort("kind=run.complete");
    println!("\nrun complete in {:.2}s", total.as_secs_f64());
    println!("video: {}", video.display());
    println!("log:   {}", log.path().display());
    Ok(())
}

fn scratch_has_content(scratch: &Path) -> bool {
    fs::read_dir(scratch)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

fn print_sequence_summary(sequence: &frame::FrameSequence, white_balance: cli::WhiteBalance) {
    if white_balance.freezes_reference() {
        println!(
            "frozen white balance: {} from {} (applied to every later frame)",
            frame::format_wb(sequence.reference_wb),
            sequence.reference_frame
        );
    } else {
        println!(
            "per-frame white balance: every frame decoded with its own as-shot neutral \
             (no --white-balance override)"
        );
    }
    println!(
        "frames complete: {} frames of {}x{} in {:.2}s",
        sequence.frames.len(),
        sequence.width,
        sequence.height,
        sequence.total_duration.as_secs_f64()
    );
}
