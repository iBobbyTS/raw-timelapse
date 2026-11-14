//! Product adapters for the typed graph. Numerical estimation and prepared
//! contexts remain their accepted producers; this module owns scheduling I/O,
//! the run-log events and the encoder/validation handoff.
use crate::encoder::ResolvedConfig;
use crate::error::{Error, Result, EXIT_FRAME, EXIT_PREVIEW, EXIT_USAGE, EXIT_VALIDATION};
use crate::frame::{
    self, FrameInfo, FrameSequence, PrepareSettings, PreparedFrame, PreparedSettings,
};
use crate::run_log::{quote, RunLog};
use crate::video::{self, ResizeRecord};
use crate::{proc, EncodedStage, RunContext};
use rawtl_linear_video::deflicker::estimation::ExposurePlan;
use rawtl_linear_video::deflicker::measurement::AnalysisSidecar;
use rawtl_linear_video::deflicker::{estimate_exposure, ExposureConfig, FrameMeasurement};
use rawtl_linear_video::graph::ordered_frames::FrameTask;
use rawtl_linear_video::graph::{self, Backend, Event, Gain, Graph, Scope};
use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

pub struct Output {
    pub sequence: FrameSequence,
    pub stage: EncodedStage,
}

/// One image travelling the stream chain. The colour node replaces scene-linear
/// FP32 with the single 10-bit 4:4:4 encoder handoff; resize and explicit
/// exposure only accept the linear form.
enum Pixels {
    Linear(Arc<[u8]>),
    Target(Vec<u16>),
}

struct Image {
    info: FrameInfo,
    pixels: Pixels,
}

struct Runner<'a, 'b> {
    context: &'a RunContext<'b>,
    log: &'a mut RunLog,
    measurements: Vec<FrameMeasurement>,
    reference_wb: Option<[f64; 3]>,
    capture_times: Vec<Option<String>>,
    frames: Vec<FrameInfo>,
    source_geometry: Option<(u32, u32)>,
    resize_target: Option<(u32, u32)>,
    resize: Option<ResizeRecord>,
    generated_paths: Vec<std::path::PathBuf>,
    encoder: video::GraphEncoder,
    failure: Option<Error>,
}
fn frame_failure(id: usize, total: usize, name: &str, message: &str) -> String {
    format!(
        "frame {}/{} ({}) failed: {}; no video was produced",
        id + 1, total, name, message
    )
}

impl Runner<'_, '_> {
    fn result<T>(&mut self, result: Result<T>) -> graph::Result<T> {
        result.map_err(|e| {
            let message = e.message.clone();
            self.failure = Some(e);
            message
        })
    }
    fn frame_result<T>(&mut self, id: usize, result: Result<T>) -> graph::Result<T> {
        let result = result.map_err(|e| {
            Error::new(
                e.code,
                frame_failure(
                    id,
                    self.context.sources.len(),
                    &self.context.sources[id].name,
                    &e.message,
                ),
            )
        });
        self.result(result)
    }
    fn wb(&self, id: usize) -> Option<[f64; 3]> {
        if id > 0 && self.context.options.white_balance.freezes_reference() {
            self.reference_wb
        } else {
            None
        }
    }
    fn transcript(&self, stage: &str, id: usize) -> std::path::PathBuf {
        self.context
            .scratch
            .join("transcripts")
            .join(stage)
            .join(format!("frame_{:06}.log", id + 1))
    }
    fn settings(&self) -> PreparedSettings {
        PreparedSettings {
            camera_color_method: self.context.options.preview_profile.camera_color_method(),
        }
    }
}
impl Backend for Runner<'_, '_> {
    type Reference = PreparedFrame;
    type Measurement = FrameMeasurement;
    type Image = Image;
    fn check(&mut self) -> graph::Result<()> {
        self.result(proc::check_stop())?;
        if self.encoder.active.lock().unwrap().cancelled {
            return self.result(Err(Error::new(
                EXIT_PREVIEW,
                "streaming encoder failed; scheduling cancelled",
            )));
        }
        Ok(())
    }
    fn source_identity(&self, id: usize) -> String {
        self.context.sources[id].path.display().to_string()
    }
    fn cancel(&mut self) {
        proc::cancel(&self.encoder.active);
    }
    fn prepare_task(&mut self, id: usize) -> graph::Result<FrameTask<PreparedFrame>> {
        let scratch = self
            .context
            .scratch
            .join("cache")
            .join(format!("frame_{:06}", id + 1));
        let context = scratch.join("prepared.context");
        let log = self.transcript("prepare", id);
        let source = self.context.sources[id].clone();
        let tool = self.context.options.native_tool.clone();
        let converter = self.context.options.converter.clone();
        let converter_timeout = self.context.options.converter_timeout;
        let active = self.encoder.active.clone();
        Ok(Box::new(move || {
            fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
            frame::prepare_active(
                &tool,
                &source,
                &PrepareSettings {
                    scratch_dir: &scratch,
                    context_path: &context,
                    log: &log,
                    converter: converter.as_deref(),
                    converter_timeout,
                },
                active,
            )
            .map_err(|e| e.message)
        }))
    }
    fn commit_prepare(
        &mut self,
        id: usize,
        prepared: &PreparedFrame,
        elapsed: Duration,
    ) -> graph::Result<()> {
        if id != self.capture_times.len() {
            return Err("graph: prepare commit order mismatch".into());
        }
        if id == 0 {
            self.reference_wb = Some(prepared.as_shot_neutral);
        }
        self.capture_times.push(prepared.capture_time.clone());
        if prepared.generated {
            self.generated_paths.push(prepared.dng.clone());
        }
        let written = self.log.line(&format!(
            "kind=prepare frame={} source={} route={} generated={} cache={} duration={:.3}",
            id + 1,
            quote(&prepared.source_name),
            prepared.route.token(),
            prepared.generated,
            quote(&prepared.dng.display().to_string()),
            elapsed.as_secs_f64()
        ));
        self.result(written)
    }
    fn hash<'a>(&self, r: &'a PreparedFrame) -> &'a str {
        &r.source_sha256
    }
    fn measure_task(
        &mut self,
        id: usize,
        r: &PreparedFrame,
    ) -> graph::Result<FrameTask<FrameMeasurement>> {
        let log = self.transcript("measure", id);
        let tool = self.context.options.native_tool.clone();
        let prepared = r.clone();
        let wb = self.wb(id);
        if id > 0 && self.context.options.white_balance.freezes_reference() && wb.is_none() {
            return Err("graph: reference WB is not committed".into());
        }
        let roi = self.context.options.deflicker.roi;
        let settings = self.settings();
        let total = self.context.sources.len();
        let active = self.encoder.active.clone();
        Ok(Box::new(move || {
            let info = frame::analyze_prepared_at(
                &tool, &prepared, wb, roi, &settings, id, total, &log, active,
            )
            .map_err(|e| frame_failure(id, total, &prepared.source_name, &e.message))?;
            let bytes = frame::analysis_bytes(&info).ok_or("graph: missing measurement")?;
            let sidecar =
                AnalysisSidecar::parse(std::str::from_utf8(bytes).map_err(|e| e.to_string())?)?;
            if sidecar.measurement.source_sha256 != prepared.source_sha256 {
                return Err("graph: measurement identity mismatch".into());
            }
            Ok(sidecar.measurement)
        }))
    }
    fn commit_measure(
        &mut self,
        id: usize,
        r: &PreparedFrame,
        measurement: FrameMeasurement,
        elapsed: Duration,
    ) -> graph::Result<()> {
        if id != self.measurements.len() || measurement.source_sha256 != r.source_sha256 {
            return Err("graph: measurement commit identity/order mismatch".into());
        }
        self.measurements.push(measurement);
        let written = self.log.line(&format!(
            "kind=measure frame={} source={} duration={:.3}",
            id + 1,
            quote(&r.source_name),
            elapsed.as_secs_f64()
        ));
        self.result(written)
    }
    fn solve(&mut self) -> graph::Result<Vec<Gain>> {
        let cfg = &self.context.options.deflicker;
        let continuity = crate::continuity::time_gaps(&self.capture_times);
        let config = ExposureConfig {
            period: cfg.period,
            strength: cfg.strength,
            steps: cfg.steps.clone(),
            brightness: cfg.brightness.as_str().into(),
            ev_limits: cfg.ev_limits,
            time_gaps: continuity,
            estimator: cfg.estimator.as_str().into(),
            exclude_rois: cfg.exclude_rois.clone(),
            ..Default::default()
        };
        let started = Instant::now();
        let result = estimate_exposure(&self.measurements, &config)?;
        // Quality is durable for this invocation in ExposureResult, never a
        // product quality surface. Consume its internal accounting before
        // admitting the frozen plan; uncovered pairs are retained as uncovered,
        // and no render probe, feedback pass or invented all-clear follows.
        let q = &result.plan_quality;
        if q.coverage.eligible != self.measurements.len().saturating_sub(1)
            || q.coverage.evaluated + q.coverage.skipped != q.coverage.eligible
            || q.coverage.evaluated != q.pairs.len()
            || q.common_jump_frames.iter().any(|id| {
                *id == 0
                    || *id >= self.measurements.len()
                    || !q.pairs.iter().any(|p| p.frame_id == *id)
            })
        {
            return Err("graph: inconsistent internal solver coverage".into());
        }
        let plan = ExposurePlan::from_result(&self.measurements, &result)?;
        let gains = plan
            .entries()
            .iter()
            .map(|p| Gain {
                frame_id: p.frame_id,
                source_sha256: p.source_sha256.clone(),
                ev: p.ev,
            })
            .collect();
        let written = self.log.line(&format!(
            "kind=solve frames={} duration={:.3}",
            plan.entries().len(),
            started.elapsed().as_secs_f64()
        ));
        self.result(written)?;
        Ok(gains)
    }
    fn develop(&mut self, id: usize, r: &PreparedFrame, ev: f64) -> graph::Result<Image> {
        let log = self.transcript("develop", id);
        let mut pixels = None;
        let started = Instant::now();
        let result = frame::develop_prepared_at(
            &self.context.options.native_tool,
            r,
            self.wb(id),
            ev,
            &self.settings(),
            id,
            self.context.sources.len(),
            &log,
            self.encoder.active.clone(),
            &mut |info, native| {
                pixels = Some(native.pixels(info.width, info.height)?);
                Ok(())
            },
        );
        let info = self.frame_result(id, result)?;
        if let Some((width, height)) = self.source_geometry {
            if width != info.width || height != info.height {
                return Err("graph: frame dimensions changed".into());
            }
        } else {
            self.source_geometry = Some((info.width, info.height));
            // An explicit resize replaces the default source geometry; only the
            // geometry that actually reaches the codec is validated here.
            let geometry = self.resize_target.unwrap_or((info.width, info.height));
            if let Err(message) = self.encoder.config().validate_geometry(geometry.0, geometry.1) {
                return Err(message);
            }
        }
        if self
            .context
            .options
            .preview_profile
            .camera_color_method()
            .requires_canon_cr3()
            && self.frames.first().is_some_and(|first_frame| {
                first_frame
                    .camera_color
                    .as_ref()
                    .and_then(|c| c.get("camera_model"))
                    != info
                        .camera_color
                        .as_ref()
                        .and_then(|c| c.get("camera_model"))
            })
        {
            return Err("rev-eng Canon Log rejects mixed camera models".into());
        }
        // pixels() checks EOF/hash and reaps native before returning owned data.
        self.check()?;
        let written = self.log.line(&format!(
            "kind=develop frame={} source={} ev={:.9} wb={} wb_source={} as_shot={} \
             source_sha256={} pixels={} pixels_sha256={} duration={:.3}",
            info.index,
            quote(&info.source_name),
            info.exposure_ev,
            frame::format_wb(info.applied_wb),
            info.wb_source,
            frame::format_wb(info.as_shot_neutral),
            info.source_sha256.as_deref().unwrap_or("unknown"),
            pixels.as_ref().map_or(0, |pixels| pixels.len()),
            info.pixels_sha256.as_deref().unwrap_or("unknown"),
            started.elapsed().as_secs_f64()
        ));
        self.result(written)?;
        Ok(Image {
            info,
            pixels: Pixels::Linear(pixels.ok_or("graph: develop returned no owned pixels")?),
        })
    }
    fn exposure(&mut self, image: &mut Image, ev: f64) -> graph::Result<()> {
        let Pixels::Linear(pixels) = &mut image.pixels else {
            return Err("graph: exposure requires scene-linear pixels".into());
        };
        let gain = ev.exp2();
        for p in Arc::make_mut(pixels).chunks_exact_mut(4) {
            let value = (f32::from_le_bytes(p.try_into().unwrap()) as f64 * gain) as f32;
            if !value.is_finite() {
                return Err("graph: exposure overflow".into());
            }
            p.copy_from_slice(&value.to_le_bytes());
        }
        image.info.pixels_sha256 = Some(crate::digest::sha256(pixels));
        image.info.exposure_ev += ev;
        let written = self.log.line(&format!(
            "kind=exposure frame={} ev={:.9} total_ev={:.9}",
            image.info.index, ev, image.info.exposure_ev
        ));
        self.result(written)
    }
    fn resize(&mut self, image: &mut Image, width: u32, height: u32) -> graph::Result<()> {
        let Pixels::Linear(pixels) = &image.pixels else {
            return Err("graph: resize requires scene-linear pixels before colour".into());
        };
        let source = (image.info.width, image.info.height);
        let resized = crate::resample::resize_scene_linear(pixels, source.0, source.1, width, height)
            .map_err(|e| format!("graph: resize: {e}"))?;
        if self.resize.is_none() {
            self.resize = Some(ResizeRecord {
                source_width: source.0,
                source_height: source.1,
                width,
                height,
            });
        }
        let digest = crate::digest::sha256(&resized);
        image.pixels = Pixels::Linear(Arc::from(resized));
        image.info.width = width;
        image.info.height = height;
        image.info.pixels_sha256 = Some(digest);
        Ok(())
    }
    fn color(&mut self, image: Image) -> graph::Result<Image> {
        let Image { info, pixels } = image;
        let Pixels::Linear(pixels) = pixels else {
            return Err("graph: colour conversion requires scene-linear pixels".into());
        };
        let profile = self.context.options.preview_profile;
        let range = self.encoder.config().effective_range(profile);
        let mut signal = Vec::new();
        profile
            .convert_f32_frame_to_yuv10(
                &pixels,
                info.width as usize,
                info.height as usize,
                self.context.options.log_reference_gain,
                range,
                &mut signal,
            )
            .map_err(|e| e.to_string())?;
        Ok(Image {
            info,
            pixels: Pixels::Target(signal),
        })
    }
    fn begin(&mut self) -> graph::Result<()> {
        self.check()?;
        let result = self.encoder.begin();
        self.result(result)
    }
    fn push(&mut self, id: usize, image: Image) -> graph::Result<()> {
        let Pixels::Target(signal) = &image.pixels else {
            return Err("graph: encoder requires the 10-bit target signal".into());
        };
        let result = self.encoder.push(self.context, &image.info, signal);
        self.result(result)?;
        if id == 0 {
            if let Some(command) = self.encoder.command() {
                let written = self
                    .log
                    .line(&format!("kind=encoder.command args={}", quote(command)));
                self.result(written)?;
            }
        }
        self.frames.push(image.info);
        Ok(())
    }
    fn end(&mut self) -> graph::Result<()> {
        let result = self.encoder.end();
        self.result(result)
    }
    fn delete(&mut self, id: usize, r: &PreparedFrame) -> graph::Result<()> {
        if r.generated {
            let result = fs::remove_file(&r.dng).map_err(|e| {
                Error::new(EXIT_FRAME, format!("delete cache {}: {e}", r.dng.display()))
            });
            self.result(result)?;
            let written = self.log.line(&format!(
                "kind=delete frame={} source={} path={}",
                id + 1,
                quote(&r.source_name),
                quote(&r.dng.display().to_string())
            ));
            self.result(written)?;
        }
        Ok(())
    }
    fn event(&mut self, event: &Event) -> graph::Result<()> {
        let frame = match event.frame_id {
            Some(id) => (id + 1).to_string(),
            None => "sequence".to_string(),
        };
        let scope = if event.scope == Scope::Frame {
            "frame"
        } else {
            "sequence"
        };
        let written = self.log.line(&format!(
            "kind=node node={} frame={} scope={}",
            event.node, frame, scope
        ));
        self.result(written)
    }
}

fn check_output_extension(options: &crate::cli::Options, config: &ResolvedConfig) -> Result<()> {
    let extension = options
        .video
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    if !extension.eq_ignore_ascii_case(config.container.extension()) {
        return Err(Error::new(
            EXIT_USAGE,
            format!(
                "--output {} ends in .{extension}, but the configured container {} writes .{}; \
                 rename the output file or select a matching --container/--encoder",
                options.video.display(),
                config.container.as_str(),
                config.container.extension()
            ),
        ));
    }
    Ok(())
}

/// Resolve the graph and encoder configuration without touching the output
/// location. Configuration errors are therefore rejected before the run log,
/// the scratch directory or any RAW work exists.
pub fn resolve_configuration(options: &crate::cli::Options) -> Result<(Graph, ResolvedConfig)> {
    let mut graph = if let Some(path) = &options.graph {
        Graph::parse(&fs::read_to_string(path).map_err(|e| {
            Error::new(EXIT_VALIDATION, format!("graph {}: {e}", path.display()))
        })?)
    } else {
        Ok(Graph::default_graph(options.deflicker.enabled()))
    }
    .map_err(|e| Error::new(EXIT_VALIDATION, e))?;
    if !options.deflicker.enabled() {
        graph.disable_measurement();
    }
    if let Some((width, height)) = options.resize {
        graph
            .apply_resize(width, height)
            .map_err(|e| Error::new(EXIT_VALIDATION, e))?;
    }
    let config =
        ResolvedConfig::resolve(graph.encoder_patch(), &options.encoder)
            .map_err(|e| Error::new(EXIT_VALIDATION, e))?;
    check_output_extension(options, &config)?;
    Ok((graph, config))
}

pub fn run(context: &RunContext<'_>, log: &mut RunLog) -> Result<Output> {
    let (graph, config) = resolve_configuration(context.options)?;
    let explicit_resize = graph.resize_geometry();
    let range = config.effective_range(context.options.preview_profile);
    log.line(&format!(
        "kind=graph source={} nodes={} concurrency={}",
        match &context.options.graph {
            Some(path) => quote(&path.display().to_string()),
            None => quote("built-in"),
        },
        graph.node_ids().join(","),
        graph
            .concurrency_config()
            .iter()
            .map(|(id, n)| format!("{id}:{n}"))
            .collect::<Vec<_>>()
            .join(",")
    ))?;
    log.line(&format!(
        "kind=encoder encoder={} container={} pix_fmt={} profile={} range={} geometry={}",
        config.encoder,
        config.container.as_str(),
        config.pix_fmt,
        config.codec_profile.as_deref().unwrap_or("default"),
        range.as_str(),
        match explicit_resize {
            Some((width, height)) => format!("{width}x{height}"),
            None => "source".to_string(),
        }
    ))?;
    let profile = context.options.preview_profile;
    log.line(&format!(
        "kind=colour profile={} camera_color={} matrix={} primaries={} transfer={} \
         reference_gain={}",
        profile.id,
        profile.camera_color_method().native_token(),
        profile.tag_policy.matrix_token(),
        profile.tag_policy.primaries_token(),
        profile.tag_policy.transfer_token(),
        context.options.log_reference_gain
    ))?;

    let preflight_started = Instant::now();
    video::preflight(
        &config,
        profile,
        &context.options.ffmpeg,
        explicit_resize,
        context.options.fps,
        context.scratch,
    )?;
    let preflight_elapsed = preflight_started.elapsed();
    let output_name = context
        .options
        .video
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::new(EXIT_USAGE, "--output has no usable file name"))?
        .to_string();
    let mut runner = Runner {
        context,
        log,
        measurements: vec![],
        reference_wb: None,
        capture_times: vec![],
        frames: vec![],
        source_geometry: None,
        resize_target: explicit_resize,
        resize: None,
        generated_paths: vec![],
        encoder: video::GraphEncoder::new(config.clone(), context.sources.len(), &output_name),
        failure: None,
    };
    let graph_started = Instant::now();
    let result = graph::execute(&graph, context.sources.len(), &mut runner);
    if let Err(message) = result {
        let _ = runner.log.line(&format!(
            "kind=node.error message={} cache_root={}",
            quote(&message),
            quote(&context.scratch.join("cache").display().to_string())
        ));
        proc::check_stop()?;
        if runner.encoder.failed() {
            return Err(Error::new(
                EXIT_PREVIEW,
                format!(
                    "streaming encoder failed: {message}; encoder transcript: {}",
                    runner.encoder.failure_detail()
                ),
            ));
        }
        return Err(runner
            .failure
            .take()
            .unwrap_or_else(|| Error::new(EXIT_FRAME, message)));
    }
    proc::check_stop()?;
    let first = runner
        .frames
        .first()
        .ok_or_else(|| Error::new(EXIT_FRAME, "graph produced no frames"))?;
    let sequence = FrameSequence {
        width: first.width,
        height: first.height,
        reference_wb: first.applied_wb,
        reference_frame: first.source_name.clone(),
        total_duration: runner.frames.iter().map(|f| f.duration).sum(),
        frames: std::mem::take(&mut runner.frames),
    };
    let mut timings = BTreeMap::new();
    timings.insert("graph".into(), graph_started.elapsed());
    timings.insert("preflight".into(), preflight_elapsed);
    let validation_started = Instant::now();
    let output = context.scratch.join(&output_name);
    let probe = video::validate_output(
        &context.options.ffprobe,
        &context.options.ffmpeg,
        &output,
        &video::OutputContract {
            config: &config,
            profile,
            width: sequence.width,
            height: sequence.height,
            frames: sequence.frames.len(),
            fps: context.options.fps,
        },
    )?;
    timings.insert("validate_video".into(), validation_started.elapsed());
    let bytes = video::file_size(&output)
        .ok_or_else(|| Error::new(EXIT_PREVIEW, "graph encoder produced no video"))?;
    let artifact = video::video_envelope_with_gain(
        &output,
        config.media_type(),
        &probe.codec_name,
        probe.frames,
        "graph streaming encoder",
        profile,
        context.options.log_reference_gain,
    )
    .map_err(|e| Error::new(EXIT_VALIDATION, e.to_string()))?;
    // The graph owns cache deletion. Files that a custom graph did not delete
    // are retained (and named by the success log); the default graph deletes
    // every generated cache DNG, leaving this list empty.
    let mut retained_cache: Vec<std::path::PathBuf> = runner
        .generated_paths
        .iter()
        .filter(|path| fs::symlink_metadata(path).is_ok())
        .cloned()
        .collect();
    retained_cache.sort();
    retained_cache.dedup();
    Ok(Output {
        sequence,
        stage: EncodedStage {
            probe,
            bytes,
            timings,
            video_artifact: artifact,
            resize: runner.resize,
            retained_cache,
        },
    })
}
