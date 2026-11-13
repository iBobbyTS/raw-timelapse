//! Product adapters for the typed graph. Numerical estimation and prepared
//! contexts remain their accepted producers; this module owns scheduling I/O.
use crate::error::{Error, Result, EXIT_FRAME, EXIT_PREVIEW, EXIT_VALIDATION};
use crate::frame::{
    self, FrameInfo, FrameSequence, PrepareSettings, PreparedFrame, PreparedSettings,
};
use crate::{proc, video, EncodedStage, RunContext};
use rawtl_linear_video::deflicker::estimation::ExposurePlan;
use rawtl_linear_video::deflicker::measurement::AnalysisSidecar;
use rawtl_linear_video::deflicker::{
    estimate_exposure, ExposureConfig, ExposureResult, FrameMeasurement,
};
use rawtl_linear_video::graph::{self, Backend, Event, Gain, Graph};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct Output {
    pub sequence: FrameSequence,
    pub stage: EncodedStage,
    pub exposure: Option<ExposureResult>,
}
pub struct Image {
    info: FrameInfo,
    pixels: Arc<[u8]>,
}
struct Runner<'a, 'b> {
    context: &'a RunContext<'b>,
    measurements: Vec<FrameMeasurement>,
    exposure: Option<ExposureResult>,
    reference_wb: Option<[f64; 3]>,
    generated_paths: Vec<std::path::PathBuf>,
    capture_times: Vec<Option<String>>,
    frames: Vec<FrameInfo>,
    encoder: video::GraphEncoder,
    events: File,
    failure: Option<Error>,
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
                format!(
                    "frame {}/{} ({}) failed: {}; no video was produced",
                    id + 1,
                    self.context.sources.len(),
                    self.context.sources[id].name,
                    e.message
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
    fn settings(&self) -> PreparedSettings<'_> {
        PreparedSettings {
            camera_color_method: self.context.options.preview_profile.camera_color_method(),
            logs_dir: self.context.output,
            log_relative_dir: "logs/render",
        }
    }
}
impl Backend for Runner<'_, '_> {
    type Reference = PreparedFrame;
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
    fn prepare(&mut self, id: usize) -> graph::Result<PreparedFrame> {
        let ctx = self.context;
        let opts = ctx.options;
        let scratch = ctx
            .partial_dir
            .join("cache")
            .join(format!("frame_{:06}", id + 1));
        let context = scratch.join("prepared.context");
        let log = ctx
            .output
            .join(format!("logs/prepare/frame_{:06}.log", id + 1));
        self.result(
            fs::create_dir_all(&scratch).map_err(|e| Error::new(EXIT_FRAME, e.to_string())),
        )?;
        let result = frame::prepare(
            &opts.native_tool,
            &ctx.sources[id],
            &PrepareSettings {
                scratch_dir: &scratch,
                context_path: &context,
                log: &log,
                converter: opts.converter.as_deref(),
                converter_timeout: opts.converter_timeout,
            },
        );
        let prepared = self.result(result)?;
        if id == 0 {
            self.reference_wb = Some(prepared.as_shot_neutral);
        }
        self.capture_times.push(prepared.capture_time.clone());
        if prepared.generated {
            self.generated_paths.push(prepared.dng.clone());
        }
        writeln!(
            self.events,
            "cache frame={id} generated={} path={}",
            prepared.generated,
            prepared.dng.display()
        )
        .map_err(|e| e.to_string())?;
        Ok(prepared)
    }
    fn hash<'a>(&self, r: &'a PreparedFrame) -> &'a str {
        &r.source_sha256
    }
    fn measure(&mut self, id: usize, r: &PreparedFrame) -> graph::Result<()> {
        let log = self
            .context
            .output
            .join(format!("logs/analysis/frame_{:06}.log", id + 1));
        let result = frame::analyze_prepared_at(
            &self.context.options.native_tool,
            r,
            self.wb(id),
            self.context.options.deflicker.roi,
            &self.settings(),
            id,
            self.context.sources.len(),
            &log,
        );
        let info = self.frame_result(id, result)?;
        let bytes = frame::analysis_bytes(&info).ok_or("graph: missing measurement")?;
        let sidecar =
            AnalysisSidecar::parse(std::str::from_utf8(bytes).map_err(|e| e.to_string())?)?;
        if sidecar.measurement.source_sha256 != r.source_sha256 {
            return Err("graph: measurement identity mismatch".into());
        }
        self.measurements.push(sidecar.measurement);
        Ok(())
    }
    fn solve(&mut self) -> graph::Result<Vec<Gain>> {
        let cfg = &self.context.options.deflicker;
        let continuity =
            crate::deflicker_run::graph_time_gaps(self.context.sources, &self.capture_times);
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
        let result = estimate_exposure(&self.measurements, &config)?;
        // Quality is durable for this invocation in ExposureResult, never a
        // product quality surface. Consume its accounting before admitting the
        // frozen plan; uncovered pairs are retained as uncovered, no render
        // probe, feedback pass or invented all-clear follows.
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
        self.exposure = Some(result);
        Ok(gains)
    }
    fn develop(&mut self, id: usize, r: &PreparedFrame, ev: f64) -> graph::Result<Image> {
        let log = self
            .context
            .output
            .join(format!("logs/render/frame_{:06}.log", id + 1));
        let mut pixels = None;
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
        let mut info = self.frame_result(id, result)?;
        info.log_relative = format!("logs/render/frame_{:06}.log", id + 1);
        if let Some(first) = self.frames.first() {
            if first.width != info.width || first.height != info.height {
                return Err("graph: frame dimensions changed".into());
            }
            if self
                .context
                .options
                .preview_profile
                .camera_color_method()
                .requires_canon_cr3()
                && first
                    .camera_color
                    .as_ref()
                    .and_then(|c| c.get("camera_model"))
                    != info
                        .camera_color
                        .as_ref()
                        .and_then(|c| c.get("camera_model"))
            {
                return Err("rev-eng Canon Log rejects mixed camera models".into());
            }
        }
        // pixels() checks EOF/hash and reaps native before returning owned data.
        self.check()?;
        Ok(Image {
            info,
            pixels: pixels.ok_or("graph: develop returned no owned pixels")?,
        })
    }
    fn exposure(&mut self, image: &mut Image, ev: f64) -> graph::Result<()> {
        let gain = ev.exp2();
        let mut minimum = f32::INFINITY;
        let mut maximum = f32::NEG_INFINITY;
        for p in Arc::make_mut(&mut image.pixels).chunks_exact_mut(4) {
            let value = (f32::from_le_bytes(p.try_into().unwrap()) as f64 * gain) as f32;
            if !value.is_finite() {
                return Err("graph: exposure overflow".into());
            }
            minimum = minimum.min(value);
            maximum = maximum.max(value);
            p.copy_from_slice(&value.to_le_bytes());
        }
        image.info.pixels_sha256 = Some(crate::digest::sha256(&image.pixels));
        image.info.observed_min = Some(minimum as f64);
        image.info.observed_max = Some(maximum as f64);
        image.info.exposure_ev += ev;
        Ok(())
    }
    fn color(&mut self, mut image: Image) -> graph::Result<Image> {
        let mut pixels = vec![0; image.info.width as usize * image.info.height as usize * 3];
        let counts = self.context.options.preview_profile.convert_f32_frame(
            &image.pixels,
            image.info.width as usize,
            image.info.height as usize,
            self.context.options.log_reference_gain,
            &mut pixels,
        )?;
        image.info.signal_clip_counts = Some(counts);
        image.pixels = pixels.into();
        Ok(image)
    }
    fn begin(&mut self) -> graph::Result<()> {
        self.check()
    }
    fn push(&mut self, _id: usize, image: Image) -> graph::Result<()> {
        let result = self.encoder.push(self.context, &image.info, image.pixels);
        self.result(result)?;
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
            writeln!(self.events, "deleted frame={id} path={}", r.dng.display())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn event(&mut self, event: &Event) -> graph::Result<()> {
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs_f64();
        writeln!(
            self.events,
            "wall={wall:.3} elapsed={:.3} node={} frame={:?} completed={:?}",
            self.context.started.elapsed().as_secs_f64(),
            event.node,
            event.frame_id,
            event.scope
        )
        .map_err(|e| e.to_string())
    }
}
pub fn run(context: &RunContext<'_>) -> Result<Output> {
    let mut graph =
        if let Some(path) = &context.options.graph {
            Graph::parse(&fs::read_to_string(path).map_err(|e| {
                Error::new(EXIT_VALIDATION, format!("graph {}: {e}", path.display()))
            })?)
        } else {
            Ok(Graph::default_graph(context.options.deflicker.enabled()))
        }
        .map_err(|e| Error::new(EXIT_VALIDATION, e))?;
    if !context.options.deflicker.enabled() {
        graph.disable_measurement();
    }
    fs::create_dir_all(context.output.join("logs"))
        .map_err(|e| Error::new(EXIT_FRAME, e.to_string()))?;
    let events = File::create(context.output.join("logs/graph.log"))
        .map_err(|e| Error::new(EXIT_FRAME, e.to_string()))?;
    let mut runner = Runner {
        context,
        measurements: vec![],
        exposure: None,
        reference_wb: None,
        generated_paths: vec![],
        capture_times: vec![],
        frames: vec![],
        encoder: video::GraphEncoder::new(),
        events,
        failure: None,
    };
    let graph_started = Instant::now();
    let result = graph::execute(&graph, context.sources.len(), &mut runner);
    if let Err(message) = result {
        let _ = writeln!(
            runner.events,
            "error: {message}; retained cache root={} partial={}",
            context.partial_dir.join("cache").display(),
            context.partial_dir.join("output.mp4").display()
        );
        if runner.encoder.active.lock().unwrap().cancelled {
            return Err(Error::new(
                EXIT_PREVIEW,
                format!("streaming encoder failed: {message}"),
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
    let validation_started = Instant::now();
    let output = context.partial_dir.join("output.mp4");
    let probe = video::validate_preview(
        &context.options.ffprobe,
        &context.options.ffmpeg,
        &output,
        sequence.width,
        sequence.height,
        sequence.frames.len(),
        context.options.fps,
        context.options.preview_profile,
    )?;
    timings.insert("validate_video".into(), validation_started.elapsed());
    let bytes = video::file_size(&output)
        .ok_or_else(|| Error::new(EXIT_PREVIEW, "graph encoder produced no video"))?;
    let artifact = video::video_envelope_with_gain(
        &output,
        video::PREVIEW_MEDIA_TYPE,
        &probe.codec_name,
        probe.frames,
        "graph streaming encoder",
        context.options.preview_profile,
        context.options.log_reference_gain,
    )
    .map_err(|e| Error::new(EXIT_VALIDATION, e.to_string()))?;
    crate::audit::write_inventory(
        context.output,
        &sequence
            .frames
            .iter()
            .map(|f| f.source_name.clone())
            .collect::<Vec<_>>(),
    )?;
    Ok(Output {
        sequence,
        stage: EncodedStage {
            retain_partial: runner.generated_paths.iter().any(|p| p.exists()),
            probe,
            bytes,
            timings,
            video_artifact: artifact,
        },
        exposure: runner.exposure.take(),
    })
}
