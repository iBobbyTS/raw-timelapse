//! Product adapters for the typed graph. Numerical estimation and prepared
//! contexts remain their accepted producers; this module owns scheduling I/O,
//! the run-log events and the encoder/validation handoff.
use crate::encoder::{Container, ResolvedConfig};
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
use rawtl_linear_video::color_profile::AutoReferenceGain;
use rawtl_linear_video::export::{ExportProfile, Selection};
use rawtl_linear_video::graph::{self, Backend, Event, Gain, Graph, Kind, Scope};
use std::collections::BTreeMap;
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;

#[cfg(test)]
#[path = "graph_run_stage_summary_tests.rs"]
mod stage_summary_tests;

pub struct Output {
    pub sequence: FrameSequence,
    pub stage: EncodedStage,
}

/// Resize and exposure consume scene-linear frames. Colour produces either
/// the legacy quantized signal or unquantized RGB for an export profile.
enum Pixels {
    Linear(Arc<[u8]>),
    Target(Vec<u16>),
    ExportRgb(Vec<[f32; 3]>),
}

struct Image {
    info: FrameInfo,
    pixels: Pixels,
    /// Channel samples that the colour node clipped below/above the transfer's
    /// encodable domain. Only the ordered commit accumulates them.
    clips: [usize; 2],
    crop: Option<[f64;4]>,
    crop_record: Option<crate::resample::CropWindow>,
}

struct MeasuredFrame {
    measurement: FrameMeasurement,
    reused: bool,
}

struct Runner<'a, 'b> {
    context: &'a RunContext<'b>,
    log: &'a mut RunLog,
    measurements: Vec<FrameMeasurement>,
    reference_wb: Option<[f64; 3]>,
    early_contexts: Vec<Option<std::path::PathBuf>>,
    capture_times: Vec<Option<String>>,
    frames: Vec<FrameInfo>,
    source_geometry: Option<(u32, u32)>,
    resize_target: Option<(u32, u32)>,
    resize: Option<ResizeRecord>,
    lens_area: Option<[f64;4]>,
    generated_paths: Vec<std::path::PathBuf>,
    exposure_plan_ready: bool,
    gain_scanned: usize,
    encoder: Arc<Mutex<video::GraphEncoder>>,
    encoder_inputs: video::EncoderInputs,
    active: proc::ActiveNative,
    encoder_config: ResolvedConfig,
    develop_reference: Option<FrameInfo>,
    stage_stats: BTreeMap<String, graph::ordered_frames::StageStats>,
    worker_failure: Arc<Mutex<Option<Error>>>,
    failure: Option<Error>,
    prepare_batch_size: usize,
    /// Ordered per-run clipping totals, accumulated only from committed colour
    /// frames. Unit is channel samples (3 per pixel), never pixels.
    clipped_low: usize,
    clipped_high: usize,
    color_channel_samples: usize,
    color_frames: usize,
    /// Resolved delivery reference gain: the numeric CLI value, or the auto
    /// value fixed by `plan_ready` before any colour task is created.
    reference_gain: f64,
    /// Linear stages between develop and colour, replayed by the auto scan.
    linear_chain: Option<Vec<Kind>>,
    /// Worker count for the auto scan; mirrors the graph's develop concurrency.
    scan_workers: usize,
}
fn reuse_measurement(
    path: &std::path::Path,
    prepared: &PreparedFrame,
    wb: Option<[f64; 3]>,
) -> Option<FrameMeasurement> {
    let candidate = PreparedFrame::load_early_measurement(path).ok()?;
    if candidate.source != prepared.source
        || candidate.source_sha256 != prepared.source_sha256
        || candidate.route != prepared.route
        || candidate.dng != prepared.dng
        || candidate.dng_sha256 != prepared.dng_sha256
        || candidate.main_ifd_width != prepared.main_ifd_width
        || candidate.main_ifd_height != prepared.main_ifd_height
        || candidate.as_shot_neutral != prepared.as_shot_neutral
        || candidate.capture_time != prepared.capture_time
    {
        return None;
    }
    let content = fs::read_to_string(path.with_extension("analysis")).ok()?;
    let (recorded_wb, json) = content.split_once('\n')?;
    if recorded_wb != wb.map(frame::format_wb).unwrap_or_default() {
        return None;
    }
    let measurement = AnalysisSidecar::parse(json).ok()?.measurement;
    (measurement.source_sha256 == prepared.source_sha256).then_some(measurement)
}

fn frame_failure(id: usize, total: usize, name: &str, message: &str) -> String {
    format!(
        "frame {}/{} ({}) failed: {}; no video was produced",
        id + 1,
        total,
        name,
        message
    )
}

impl Runner<'_, '_> {
    fn progress(&self, phase: &str) {
        let total = self.context.sources.len() as f64;
        let conversion = if self.generated_paths.is_empty() {
            0.0
        } else {
            40.0
        };
        let deflicker = if !self.context.options.deflicker.enabled() {
            0.0
        } else if conversion > 0.0 {
            10.0
        } else {
            20.0
        };
        let prepared = self.capture_times.len() as f64 / total;
        let measured = if self.exposure_plan_ready {
            1.0
        } else {
            self.measurements.len() as f64 / (total + 1.0)
        };
        let scan_passes = if matches!(
            self.context.output_color.reference_gain,
            crate::cli::ReferenceGain::Auto
        ) {
            1.0
        } else {
            0.0
        };
        let delivered =
            (self.frames.len() + self.gain_scanned) as f64 / (total * (1.0 + scan_passes));
        let percent =
            conversion * prepared + deflicker * measured + (100.0 - conversion - deflicker) * delivered;
        // Keep 100% for a successfully published file, after verification.
        self.log.progress(phase, (percent.floor() as u32).min(99));
    }

    fn result<T>(&mut self, result: Result<T>) -> graph::Result<T> {
        result.map_err(|e| {
            let message = e.message.clone();
            self.failure = Some(e);
            message
        })
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
            lens: self.context.options.lens.clone(),
            camera_recipe: self.context.camera_recipe,
            preview_edge: None,
        }
    }
    fn scan_task(
        &self,
        ev: f64,
        id: usize,
        r: &PreparedFrame,
    ) -> graph::Result<FrameTask<(f64, f64, u128)>> {
        let develop = self.develop_image_task("scan", id, r, ev)?;
        let chain = self
            .linear_chain
            .clone()
            .ok_or("graph: auto reference gain scan requires the linear chain")?;
        let matrix = self.context.output_color.transform.composed_matrix;
        Ok(Box::new(move || {
            let mut image = develop()?;
            for kind in &chain {
                apply_linear_transform(*kind, &mut image)?;
            }
            apply_color_crop(&mut image)?;
            let Pixels::Linear(pixels) = &image.pixels else {
                return Err("graph: scan requires scene-linear pixels".into());
            };
            let mut max = f64::NEG_INFINITY;
            let mut min = f64::INFINITY;
            for src in pixels.chunks_exact(12) {
                let mut ap1 = [0.0f64; 3];
                for channel in 0..3 {
                    ap1[channel] =
                        f32::from_le_bytes(src[channel * 4..channel * 4 + 4].try_into().unwrap())
                            as f64;
                    if !ap1[channel].is_finite() {
                        return Err("nonfinite FP32 sample".into());
                    }
                }
                let target = rawtl_linear_video::chromatic_adaptation::mat3_vec(&matrix, ap1);
                if !target.iter().all(|value| value.is_finite()) {
                    return Err("nonfinite target color".into());
                }
                max = max.max(target[0]).max(target[1]).max(target[2]);
                min = min.min(target[0]).min(target[1]).min(target[2]);
            }
            let count = (pixels.len() / 4) as u128;
            Ok((max, min, count))
        }))
    }

    /// Build one native develop task for `stage`: "develop" is the delivery
    /// path, "scan" the auto-reference-gain pre-pass over identical inputs
    /// (only the transcript location differs, never the pixels).
    fn develop_image_task(
        &self,
        stage: &str,
        id: usize,
        r: &PreparedFrame,
        ev: f64,
    ) -> graph::Result<FrameTask<Image>> {
        let log = self.transcript(stage, id);
        let tool = self.context.options.native_tool.clone();
        let prepared = r.clone();
        let wb = self.wb(id);
        let settings = self.settings();
        let crop=if self.context.options.lens.crop.is_safe() {
            if self.capture_times.len()!=self.context.sources.len() {return Err("graph: sequence crop requires the complete prepared inventory".into());}
            Some(self.lens_area.ok_or("graph: missing common sequence lens area")?)
        } else {None};
        let total = self.context.sources.len();
        let active = self.active.clone();
        if id > 0 && self.context.options.white_balance.freezes_reference() && wb.is_none() {
            return Err("graph: reference WB is not committed".into());
        }
        Ok(Box::new(move || {
            let mut pixels = None;
            let info = frame::develop_prepared_at(
                &tool,
                &prepared,
                wb,
                ev,
                &settings,
                id,
                total,
                &log,
                active,
                &mut |info, native| {
                    if let Some(area)=prepared.lens_valid_area {
                        if area.bounds!=[0.,0.,1.,1.]&&(area.width!=info.width||area.height!=info.height) {return Err(Error::new(EXIT_FRAME,"lens valid-area geometry differs from delivery"));}
                    }
                    pixels = Some(native.pixels(info.width, info.height)?);
                    Ok(())
                },
            )
            .map_err(|e| frame_failure(id, total, &prepared.source_name, &e.message))?;
            Ok(Image {
                info,
                pixels: Pixels::Linear(pixels.ok_or("graph: develop returned no owned pixels")?),
                clips: [0, 0],
                crop,
                crop_record: None,
            })
        }))
    }
}
impl Backend for Runner<'_, '_> {
    type Reference = PreparedFrame;
    type Measurement = MeasuredFrame;
    type Image = Image;
    fn check(&mut self) -> graph::Result<()> {
        self.result(proc::check_stop())?;
        if self.active.lock().unwrap().cancelled {
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
        proc::cancel(&self.active);
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
        let lens = self.context.options.lens.clone();
        let active = self.active.clone();
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
                    lens,
                },
                active,
            )
            .map_err(|e| e.message)
        }))
    }
    fn prepare_batch_task(
        &mut self,
        ids: &[usize],
    ) -> graph::Result<FrameTask<graph::PreparedBatch<PreparedFrame>>> {
        let scratch = self
            .context
            .scratch
            .join("cache")
            .join(format!("batch_{:06}", ids[0] + 1));
        let sources: Vec<_> = ids
            .iter()
            .map(|id| (*id, self.context.sources[*id].clone()))
            .collect();
        let streaming = self.context.options.deflicker.enabled();
        if streaming {
            for &id in ids {
                self.early_contexts[id] =
                    Some(scratch.join("early").join(format!("frame_{id}.context")));
            }
        }
        let tool = self.context.options.native_tool.clone();
        let converter = self.context.options.converter.clone();
        let converter_timeout = self.context.options.converter_timeout;
        let lens = self.context.options.lens.clone();
        let active = self.active.clone();
        Ok(Box::new(move || {
            fs::create_dir_all(scratch.parent().unwrap()).map_err(|e| e.to_string())?;
            // create_dir, rather than create_dir_all, prevents accidental reuse.
            fs::create_dir(&scratch).map_err(|e| format!("private batch scratch: {e}"))?;
            let items: Vec<_> = sources
                .iter()
                .map(|(id, source)| frame::PrepareBatchItem {
                    frame_id: *id,
                    source,
                })
                .collect();
            let prepare = if streaming {
                frame::prepare_batch_with_early_measurements
            } else {
                frame::prepare_batch_active
            };
            let prepared = prepare(
                &tool,
                &items,
                &frame::PrepareBatchSettings {
                    scratch_dir: &scratch,
                    converter: converter.as_deref(),
                    converter_timeout,
                    lens,
                },
                active,
            )
            .map_err(|e| e.message)?;
            let diagnostics = batch_diagnostics(&scratch.join("batch-native.log"))?;
            let frames = prepared
                .into_iter()
                .map(|(frame_id, value)| {
                    let started = Instant::now();
                    let source = sources
                        .iter()
                        .find(|(id, _)| *id == frame_id)
                        .unwrap()
                        .1
                        .path
                        .display()
                        .to_string();
                    graph::PreparedReference {
                        frame_id,
                        source,
                        value,
                        elapsed: started.elapsed(),
                    }
                })
                .collect();
            Ok(graph::PreparedBatch {
                frames,
                diagnostics,
            })
        }))
    }
    fn commit_prepare_batch(
        &mut self,
        batch_id: usize,
        ids: &[usize],
        elapsed: Duration,
        diagnostics: &str,
    ) -> graph::Result<()> {
        let written = self.log.line(&format!("kind=prepare.batch batch={} frame_first={} frame_last={} count={} mode=batch {} total_wall={:.6}", batch_id + 1, ids[0] + 1, ids[ids.len()-1] + 1, ids.len(), diagnostics, elapsed.as_secs_f64()));
        self.result(written)
    }
    fn commit_prepare(
        &mut self,
        id: usize,
        prepared: &PreparedFrame,
        elapsed: Duration,
    ) -> graph::Result<()> {
        self.check()?; // A completed worker cannot revive a cancelled run.
        let materialize_started = Instant::now();
        if id != self.capture_times.len() {
            return Err("graph: prepare commit order mismatch".into());
        }
        if id == 0 {
            self.reference_wb = Some(prepared.as_shot_neutral);
        }
        if self.context.options.lens.crop.is_safe() {
            let area=prepared.lens_valid_area.ok_or("graph: prepared frame has no certified lens area")?;
            self.lens_area=Some(crate::lens::intersect(self.lens_area.unwrap_or([0.,0.,1.,1.]),area.bounds).map_err(|e|e.message)?);
        }
        self.capture_times.push(prepared.capture_time.clone());
        if prepared.generated {
            self.generated_paths.push(prepared.dng.clone());
        }
        if !self.generated_paths.is_empty() {
            self.progress("convert");
        }
        let written = self.log.line(&format!(
            "kind=prepare frame={} source={} route={} generated={} cache={} mode={} duration_scope={} duration={:.6}",
            id + 1,
            quote(&prepared.source_name),
            prepared.route.token(),
            prepared.generated,
            quote(&prepared.dng.display().to_string()),
            if self.prepare_batch_size > 1 { "batch" } else { "single" },
            if self.prepare_batch_size > 1 { "reference_materialization_excludes_shared_native" } else { "single_frame_wall" },
            (elapsed + if self.prepare_batch_size > 1 { materialize_started.elapsed() } else { Duration::ZERO }).as_secs_f64()
        ));
        self.result(written)
    }
    fn hash<'a>(&self, r: &'a PreparedFrame) -> &'a str {
        &r.source_sha256
    }
    fn early_measure_task(&mut self, id: usize) -> graph::Result<Option<FrameTask<()>>> {
        let Some(candidate) = self.early_contexts[id].as_ref() else {
            return Ok(None);
        };
        let Ok(prepared) = PreparedFrame::load_early_measurement(candidate) else {
            return Ok(None);
        };
        if fs::canonicalize(&self.context.sources[id].path).ok().as_ref() != Some(&prepared.source) {
            return Ok(None);
        }
        let wb = if id > 0 && self.context.options.white_balance.freezes_reference() {
            let reference = self.reference_wb.or_else(|| {
                let path = self.early_contexts[0].as_ref()?;
                Some(PreparedFrame::load_early_measurement(path).ok()?.as_shot_neutral)
            });
            let Some(reference) = reference else {
                return Ok(None);
            };
            Some(reference)
        } else {
            None
        };
        let log = self.transcript("measure-early", id);
        let tool = self.context.options.native_tool.clone();
        let output = candidate.with_extension("analysis");
        let roi = self.context.options.deflicker.roi;
        let settings = self.settings();
        let total = self.context.sources.len();
        let active = self.active.clone();
        let written = self.log.line(&format!(
            "kind=measure.early frame={} source={}",
            id + 1,
            quote(&prepared.source_name)
        ));
        self.result(written)?;
        Ok(Some(Box::new(move || {
            if let Ok(info) = frame::analyze_prepared_at(
                &tool, &prepared, wb, roi, &settings, id, total, &log, active, true,
            ) {
                if let Some(bytes) = frame::analysis_bytes(&info) {
                    let mut output_bytes = wb.map(frame::format_wb).unwrap_or_default().into_bytes();
                    output_bytes.push(b'\n');
                    output_bytes.extend_from_slice(bytes);
                    // An early failure is retried through the sealed input.
                    let _ = fs::write(output, output_bytes);
                }
            }
            Ok(())
        })))
    }
    fn measure_task(
        &mut self,
        id: usize,
        r: &PreparedFrame,
    ) -> graph::Result<FrameTask<MeasuredFrame>> {
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
        let active = self.active.clone();
        let early = self.early_contexts[id].clone();
        Ok(Box::new(move || {
            if let Some(measurement) = early
                .as_deref()
                .and_then(|path| reuse_measurement(path, &prepared, wb))
            {
                return Ok(MeasuredFrame {
                    measurement,
                    reused: true,
                });
            }
            let info = frame::analyze_prepared_at(
                &tool, &prepared, wb, roi, &settings, id, total, &log, active, false,
            )
            .map_err(|e| frame_failure(id, total, &prepared.source_name, &e.message))?;
            let bytes = frame::analysis_bytes(&info).ok_or("graph: missing measurement")?;
            let sidecar =
                AnalysisSidecar::parse(std::str::from_utf8(bytes).map_err(|e| e.to_string())?)?;
            if sidecar.measurement.source_sha256 != prepared.source_sha256 {
                return Err("graph: measurement identity mismatch".into());
            }
            Ok(MeasuredFrame {
                measurement: sidecar.measurement,
                reused: false,
            })
        }))
    }
    fn commit_measure(
        &mut self,
        id: usize,
        r: &PreparedFrame,
        measured: MeasuredFrame,
        elapsed: Duration,
    ) -> graph::Result<()> {
        let MeasuredFrame { measurement, reused } = measured;
        if id != self.measurements.len() || measurement.source_sha256 != r.source_sha256 {
            return Err("graph: measurement commit identity/order mismatch".into());
        }
        self.measurements.push(measurement);
        self.progress("deflicker");
        let written = self.log.line(&format!(
            "kind=measure frame={} source={} duration={:.3} reused_early={}",
            id + 1,
            quote(&r.source_name),
            elapsed.as_secs_f64(),
            reused
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
        self.exposure_plan_ready = true;
        self.progress("deflicker");
        Ok(gains)
    }
    fn plan_ready(&mut self, plan: &[Gain], refs: &[PreparedFrame]) -> graph::Result<()> {
        if !matches!(
            self.context.output_color.reference_gain,
            crate::cli::ReferenceGain::Auto
        ) {
            return Ok(());
        }
        let started = Instant::now();
        let profile = self.context.output_color.profile;
        let total = refs.len();
        let workers = self.scan_workers.min(total.max(1));
        let next = AtomicUsize::new(0);
        let abort = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel::<(usize, std::result::Result<(f64, f64, u128), String>)>();
        // Build every scan task on the coordinator first: the boxed tasks are
        // Send + 'static, so workers never borrow the (non-Sync) runner; the
        // develop guards already ran during prepare/measure commits.
        let mut tasks: Vec<Option<FrameTask<(f64, f64, u128)>>> = Vec::with_capacity(total);
        for id in 0..total {
            tasks.push(Some(self.scan_task(plan[id].ev, id, &refs[id])?));
        }
        let queue = Mutex::new(tasks);
        let mut max_target = f64::NEG_INFINITY;
        let mut min_target = f64::INFINITY;
        let mut samples: u128 = 0;
        std::thread::scope(|scope| -> graph::Result<()> {
            let next = &next;
            let abort = &abort;
            let queue = &queue;
            for _ in 0..workers {
                let tx = tx.clone();
                scope.spawn(move || loop {
                    if abort.load(Ordering::Relaxed) {
                        break;
                    }
                    let id = next.fetch_add(1, Ordering::Relaxed);
                    if id >= total {
                        break;
                    }
                    let task = queue.lock().unwrap()[id]
                        .take()
                        .expect("graph: auto reference gain scan task taken twice");
                    let result = task().map_err(|e| e.to_string());
                    if result.is_err() {
                        abort.store(true, Ordering::Relaxed);
                    }
                    if tx.send((id, result)).is_err() {
                        break;
                    }
                });
            }
            drop(tx);
            for (id, result) in rx {
                let (max, min, count) = result.map_err(|e| {
                    format!("graph: auto reference gain scan failed on frame {}: {e}", id + 1)
                })?;
                max_target = max_target.max(max);
                min_target = min_target.min(min);
                samples += count;
                self.gain_scanned += 1;
                self.progress("exposure");
            }
            Ok(())
        })?;
        let decision = profile.auto_reference_gain(max_target, min_target);
        let gain = decision.gain;
        if !gain.is_finite() || gain <= 0.0 {
            return Err("graph: auto reference gain produced a nonpositive gain".into());
        }
        self.reference_gain = gain;
        let AutoReferenceGain {
            desired,
            safe_positive,
            safe_negative,
            limited_by,
            ..
        } = decision;
        let written = self.log.line(&format!(
            "kind=reference_gain requested=auto profile={} frames={} channel_samples={} \
             desired={:.9} desired_ev={:.2} safe_positive={:.9} safe_negative={:.9} \
             max_target_linear={:.9} min_target_linear={:.9} applied={:.9} applied_ev={:+.2} \
             limited_by={} duration={:.3}",
            profile.id,
            total,
            samples,
            desired,
            desired.log2(),
            safe_positive,
            safe_negative,
            max_target,
            min_target,
            gain,
            gain.log2(),
            limited_by,
            started.elapsed().as_secs_f64(),
        ));
        self.result(written)
    }

    fn develop_task(
        &mut self,
        id: usize,
        r: &PreparedFrame,
        ev: f64,
    ) -> graph::Result<FrameTask<Image>> {
        self.develop_image_task("develop", id, r, ev)
    }

    fn image_task(
        &mut self,
        kind: Kind,
        _: usize,
        mut image: Image,
    ) -> graph::Result<FrameTask<Image>> {
        let profile = self.context.output_color.profile;
        let output_transform = self.context.output_color.transform.clone();
        let range = self.encoder_config.effective_range(profile);
        let reference_gain = self.reference_gain;
        if !reference_gain.is_finite() || reference_gain <= 0.0 {
            return Err("graph: reference gain was not resolved before colour".into());
        }
        let depth = self.encoder_inputs.depth;
        let export = self.encoder_config.export().is_some();
        Ok(Box::new(move || {
            image.crop_record=None;
            match kind {
                Kind::Color => {
                    apply_color_crop(&mut image)?;
                    let Pixels::Linear(pixels) = &image.pixels else {
                        return Err("graph: colour conversion requires scene-linear pixels".into());
                    };
                    let width = image.info.width as usize;
                    let height = image.info.height as usize;
                    if export {
                        let (signal, clips) = profile.convert_f32_frame_to_target_rgb(
                            &output_transform, pixels, width, height, reference_gain,
                        )?;
                        image.clips = clips;
                        image.pixels = Pixels::ExportRgb(signal);
                    } else {
                        let mut signal = Vec::new();
                        image.clips = profile.convert_f32_frame_to_yuv(
                            &output_transform,
                            pixels,
                            width,
                            height,
                            reference_gain,
                            range,
                            depth,
                            &mut signal,
                        )?;
                        image.pixels = Pixels::Target(signal);
                    }
                }
                _ => apply_linear_transform(kind, &mut image)?,
            }
            Ok(image)
        }))
    }
    fn commit_image(
        &mut self,
        kind: Kind,
        id: usize,
        image: &Image,
        elapsed: Duration,
    ) -> graph::Result<()> {
        let info = &image.info;
        if info.index != id + 1 || info.source_sha256.as_deref().is_none() {
            return Err("graph: image identity mismatch".into());
        }
        if let Some(window)=image.crop_record {
            let written=self.log.line(&format!("kind=lens.crop frame={} fixed_sequence=true window={:.9},{:.9},{:.9},{:.9} output={}x{} scale={:.9},{:.9}",info.index,window.left,window.top,window.right,window.bottom,info.width,info.height,f64::from(info.width)/(window.right-window.left),f64::from(info.height)/(window.bottom-window.top)));
            self.result(written)?;
        }
        if kind == Kind::Develop {
            if let Some((width, height)) = self.source_geometry {
                if width != info.width || height != info.height {
                    return Err("graph: frame dimensions changed".into());
                }
            } else {
                self.source_geometry = Some((info.width, info.height));
                // An explicit resize replaces the default source geometry; only the
                // geometry that actually reaches the codec is validated here.
                let geometry = self.resize_target.unwrap_or((info.width, info.height));
                if let Err(message) = self
                    .encoder_config
                    .validate_geometry(geometry.0, geometry.1)
                {
                    return Err(message);
                }
            }
            if self.context.camera_recipe
                .requires_canon_cr3()
                && self.develop_reference.as_ref().is_some_and(|first_frame| {
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
                return Err("Canon CR3 development rejects mixed camera models".into());
            }

            if id == 0 {
                self.develop_reference = Some(info.clone());
                let c = info.camera_color.as_ref().expect("validated color contract");
                let text = |key| c.get(key).and_then(crate::json::Json::as_str).unwrap_or("unknown");
                let written = self.log.line(&format!(
                    "kind=camera.colour color_contract_version=2 algorithm_revision=4 recipe_requested={} recipe_effective={} profile_fingerprint={} camera_model={} hue_sat_map={} look_table={} tone_curve={} baseline_exposure={} calibration={} calibration_status={} input_cat=bradford-d50-d60",
                    text("recipe_requested"), text("recipe_effective"), quote(text("profile_fingerprint")), quote(text("camera_model")),
                    text("hue_sat_map_status"), text("look_table_status"), text("tone_curve_status"), text("baseline_exposure_status"), text("camera_calibration_effective"),
                    if self.context.camera_recipe.calibration() != "none" { "calibrated" } else { "uncalibrated" }
                ));
                self.result(written)?;
            }
            if let Some(lens) = &info.lens_correction {
                let written = self.log.line(&format!("kind=lens frame={} report={}", info.index, crate::json::to_pretty_string(lens).replace('\n', "")));
                self.result(written)?;
            }
            if let Some(recovery) = &info.highlight_recovery {
                let text = |key| recovery.get(key).and_then(crate::json::Json::as_str).unwrap_or("unknown");
                let count = |key| recovery.get(key).and_then(crate::json::Json::as_u64).unwrap_or(0);
                let written = self.log.line(&format!(
                    "kind=highlight frame={} revision=1 status={} affected={} partial={} full={} spatial={} fallback={} evidence={}",
                    info.index, text("status"), count("affected_pixels"), count("partial_pixels"),
                    count("full_pixels"), count("spatial_pixels"), count("fallback_pixels"), quote(text("evidence"))));
                self.result(written)?;
            }
            let written=self.log.line(&format!(
                "kind=develop frame={} source={} ev={:.9} wb={} wb_source={} as_shot={} source_sha256={} pixels={} duration={:.3}",
                info.index,quote(&info.source_name),info.exposure_ev,frame::format_wb(info.applied_wb),info.wb_source,frame::format_wb(info.as_shot_neutral),info.source_sha256.as_deref().unwrap_or("unknown"),
                match &image.pixels {Pixels::Linear(p)=>p.len(),_=>0},elapsed.as_secs_f64()));
            self.result(written)?;
        } else {
            if let Kind::Resize { width, height } = kind {
                if self.resize.is_none() {
                    let (sw, sh) = self
                        .source_geometry
                        .ok_or("graph: missing source geometry")?;
                    self.resize = Some(ResizeRecord {
                        source_width: sw,
                        source_height: sh,
                        width,
                        height,
                    });
                }
            }
            if let Kind::Exposure(ev) = kind {
                let written = self.log.line(&format!(
                    "kind=exposure frame={} ev={:.9} total_ev={:.9}",
                    info.index, ev, info.exposure_ev
                ));
                self.result(written)?;
            }
            let token = match kind {
                Kind::Identity => "identity",
                Kind::Exposure(_) => "exposure",
                Kind::Resize { .. } => "resize",
                Kind::Color => "color",
                _ => unreachable!("only image transformations reach this commit"),
            };
            if kind == Kind::Color {
                // Ordered commit: one accumulation per delivered frame. Worker
                // threads never share these counters, so concurrency cannot
                // duplicate or drop a frame's samples.
                let samples = (info.width as usize)
                    .checked_mul(info.height as usize)
                    .and_then(|pixels| pixels.checked_mul(3))
                    .ok_or("graph: colour geometry overflow")?;
                self.clipped_low += image.clips[0];
                self.clipped_high += image.clips[1];
                self.color_channel_samples += samples;
                self.color_frames += 1;
            }
            let written = self.log.line(&format!(
                "kind=image node_kind={token} frame={} duration={:.3}",
                info.index,
                elapsed.as_secs_f64()
            ));
            self.result(written)?;
        }
        Ok(())
    }
    fn encode_task(&mut self, _: usize, image: Image) -> graph::Result<FrameTask<Image>> {
        let encoder = self.encoder.clone();
        let inputs = self.encoder_inputs.clone();
        let scratch = self.context.scratch.to_path_buf();
        let failure = self.worker_failure.clone();
        Ok(Box::new(move || {
            let frame = match &image.pixels {
                Pixels::Target(signal) => video::EncodeFrame::Legacy(signal),
                Pixels::ExportRgb(signal) => video::EncodeFrame::TargetRgb(signal),
                _ => return Err("graph: encoder requires the target color signal".into()),
            };
            let mut encoder = encoder.lock().map_err(|_| "encoder writer panicked")?;
            let result = encoder
                .begin()
                .and_then(|_| encoder.push(&inputs, &scratch, &image.info, frame));
            if let Err(e) = result {
                let message = e.message.clone();
                *failure.lock().unwrap() = Some(e);
                return Err(message);
            }
            Ok(image)
        }))
    }
    fn commit_encode(&mut self, id: usize, image: Image, elapsed: Duration) -> graph::Result<()> {
        if id != self.frames.len() {
            return Err("graph: encode commit order mismatch".into());
        }
        if id == 0 {
            let command = self.encoder.lock().unwrap().command().map(str::to_owned);
            if let Some(command) = command {
                let written = self
                    .log
                    .line(&format!("kind=encoder.command args={}", quote(&command)));
                self.result(written)?;
            }
        }
        let written = self.log.line(&format!(
            "kind=encode frame={} duration={:.3}",
            id + 1,
            elapsed.as_secs_f64()
        ));
        self.result(written)?;
        self.frames.push(image.info);
        self.progress("encode");
        Ok(())
    }
    fn activity(
        &mut self,
        node: &str,
        stats: graph::ordered_frames::StageStats,
    ) -> graph::Result<()> {
        self.stage_stats.insert(node.into(), stats);
        Ok(())
    }
    fn end(&mut self) -> graph::Result<()> {
        let result = self.encoder.lock().unwrap().end();
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

/// Native diagnostics are a timing observation, separate from the success
/// protocol. Missing diagnostics from a custom tool are reported as unknown.
/// Apply one linear image transformation in place. Shared by the delivery
/// image chain and the auto-reference-gain scan so both see identical
/// pixels for the same frame and exposure plan.
fn apply_linear_transform(kind: Kind, image: &mut Image) -> graph::Result<()> {
    match kind {
        Kind::Identity => (),
        Kind::Exposure(ev) => {
            let Pixels::Linear(pixels) = &mut image.pixels else {
                return Err("graph: exposure requires scene-linear pixels".into());
            };
            let gain = ev.exp2();
            for p in Arc::make_mut(pixels).chunks_exact_mut(4) {
                let value =
                    (f32::from_le_bytes(p.try_into().unwrap()) as f64 * gain) as f32;
                if !value.is_finite() {
                    return Err("graph: exposure overflow".into());
                }
                p.copy_from_slice(&value.to_le_bytes());
            }
            image.info.exposure_ev += ev;
        }
        Kind::Resize { width, height } => {
            let Pixels::Linear(pixels) = &image.pixels else {
                return Err(
                    "graph: resize requires scene-linear pixels before colour".into()
                );
            };
            let bounds=image.crop.take();
            if let Some(area)=bounds {image.crop_record=Some(crate::resample::crop_window(image.info.width,image.info.height,width,height,area)?);}
            let resized = crate::resample::resize_scene_linear_region(
                pixels,
                image.info.width,
                image.info.height,
                width,
                height,
                bounds,
            )
            .map_err(|e| format!("graph: resize: {e}"))?;
            image.pixels = Pixels::Linear(Arc::from(resized));
            image.info.width = width;
            image.info.height = height;
        }
        _ => return Err("graph: unsupported linear image task".into()),
    }
    Ok(())
}

/// Lens safe-crop applied at the colour boundary. The auto-reference-gain
/// scan replays this so the scanned extremes match the converter input.
fn apply_color_crop(image: &mut Image) -> graph::Result<()> {
    if let Some(area)=image.crop.take() {
        let Pixels::Linear(pixels)=&image.pixels else {return Err("graph: lens crop requires linear pixels".into());};
        image.crop_record=Some(crate::resample::crop_window(image.info.width,image.info.height,image.info.width,image.info.height,area)?);
        let cropped=crate::resample::resize_scene_linear_region(pixels,image.info.width,image.info.height,image.info.width,image.info.height,Some(area))?;
        image.pixels=Pixels::Linear(Arc::from(cropped));
    }
    Ok(())
}

fn batch_diagnostics(path: &std::path::Path) -> graph::Result<String> {
    let text = fs::read_to_string(path).map_err(|e| format!("batch transcript: {e}"))?;
    let mut calls = 0usize;
    let mut conversion = 0.0f64;
    let mut verification = 0.0f64;
    let mut metadata = None;
    let number = |line: &str, key: &str| -> Option<f64> {
        line.split_whitespace()
            .find_map(|word| word.strip_prefix(key))?
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && *n >= 0.)
    };
    for line in text.lines() {
        if line.starts_with("converter batch count=") {
            calls += 1;
            conversion +=
                number(line, "process_s=").ok_or("graph: malformed native conversion timing")?;
            verification += number(line, "output_verification_s=")
                .ok_or("graph: malformed native verification timing")?;
        }
        if line.starts_with("prepare batch count=") {
            metadata = number(line, "metadata_verification_s=");
            if number(line, "adc_calls=") != Some(calls as f64) {
                return Err("graph: native ADC call diagnostics disagree".into());
            }
        }
    }
    Ok(match metadata {
        Some(metadata) => format!(
            "adc_calls={calls} conversion_wall={conversion:.6} verification_wall={:.6}",
            verification + metadata
        ),
        None => "adc_calls=unknown conversion_wall=unknown verification_wall=unknown".into(),
    })
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
                 rename the output file or select a profile with a matching container",
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
    let (graph, config) = resolve_processing(options)?;
    check_output_extension(options, &config)?;
    Ok((graph, config))
}

pub(crate) fn resolve_processing(options: &crate::cli::Options) -> Result<(Graph, ResolvedConfig)> {
    let mut graph =
        if let Some(path) = &options.graph {
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
    let patch = graph.encoder_patch();
    let selection = options
        .encoder
        .export
        .as_ref()
        .or_else(|| patch.and_then(|patch| patch.export.as_ref()));
    let config = match selection {
        Some(selection) => {
            let export = match selection {
                Selection::Manual(profile) => profile.clone(),
                Selection::Named(name) => match ExportProfile::software(name) {
                    Some(profile) => profile,
                    None => video::available_hardware_profiles(&options.ffmpeg)?
                        .into_iter().find(|profile| profile.name == *name)
                        .ok_or_else(|| Error::new(
                            EXIT_VALIDATION, format!("unknown or unavailable export profile {name:?}"),
                        ))?,
                },
            };
            let container = options.encoder.container
                .or_else(|| patch.and_then(|patch| patch.container))
                .unwrap_or(Container::Mov);
            ResolvedConfig::from_export(export, container)
        }
        None => ResolvedConfig::resolve(patch, &options.encoder, options.signal_depth)
            .map_err(|e| Error::new(EXIT_VALIDATION, e))?,
    };
    Ok((graph, config))
}

pub fn run(
    context: &RunContext<'_>,
    log: &mut RunLog,
    (graph, config): (Graph, ResolvedConfig),
) -> Result<Output> {
    let explicit_resize = graph.resize_geometry();
    let profile = context.output_color.profile;
    let range = config.effective_range(profile);
    log.line(&format!(
        "kind=graph source={} nodes={} concurrency={} prepare_batch_size={} prepare_capacity_unit={}",
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
            .join(","),
        graph.prepare_batch_size(),
        if graph.prepare_batch_size() > 1 { "batch_tasks" } else { "frames" }
    ))?;
    if let Some(export) = config.export() {
        log.line(&format!("kind=export profile={} input_format={}", quote(&export.name), export.input.name()))?;
    }
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
    log.line(&format!(
        "kind=colour profile={} camera_color={} matrix={} primaries={} transfer={} \
         reference_gain={} reference_status=unverified effective_range={} \
         camera_color_requested={} recipe_effective={} profile_correction={} camera_calibration={} \
         input_cat=bradford-d50-d60 calibration_status={} {}",
        profile.id,
        context.camera_recipe.native_token(),
        profile.tag_policy.matrix_token(),
        profile.tag_policy.primaries_token(),
        profile.tag_policy.transfer_token(),
        context.output_color.reference_gain.token(),
        range.as_str(),
        context.options.camera_color.token(), context.camera_recipe.token(), crate::camera::CAMERA_PROFILE_CORRECTION, context.camera_recipe.calibration(),
        if context.camera_recipe.calibration() != "none" { "pending-native-validation" } else { "uncalibrated" },
        context.output_color.transform.log_fields()
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
    // The develop->colour replayability constraint exists only for the auto
    // reference-gain scan. A fixed-gain run accepts any validated graph shape
    // and never pays the chain walk.
    let linear_chain = if matches!(
        context.output_color.reference_gain,
        crate::cli::ReferenceGain::Auto
    ) {
        Some(
            graph
                .linear_chain_to_color()
                .map_err(|e| Error::new(EXIT_VALIDATION, e))?,
        )
    } else {
        None
    };
    let scan_workers = graph.develop_concurrency();
    let active: proc::ActiveNative = Default::default();
    let mut runner = Runner {
        context,
        log,
        measurements: vec![],
        reference_wb: None,
        early_contexts: vec![None; context.sources.len()],
        capture_times: vec![],
        frames: vec![],
        source_geometry: None,
        resize_target: explicit_resize,
        resize: None,
        lens_area: None,
        generated_paths: vec![],
        exposure_plan_ready: false,
        gain_scanned: 0,
        encoder: {
            let mut encoder =
                video::GraphEncoder::new(config.clone(), context.sources.len(), &output_name);
            encoder.active = active.clone();
            Arc::new(Mutex::new(encoder))
        },
        encoder_inputs: video::EncoderInputs {
            profile: *profile,
            fps: context.options.fps,
            ffmpeg: context.options.ffmpeg.clone(),
            depth: context.options.signal_depth,
        },
        active,
        encoder_config: config.clone(),
        develop_reference: None,
        stage_stats: BTreeMap::new(),
        worker_failure: Default::default(),
        failure: None,
        prepare_batch_size: graph.prepare_batch_size(),
        clipped_low: 0,
        clipped_high: 0,
        color_channel_samples: 0,
        color_frames: 0,
        reference_gain: match context.output_color.reference_gain {
            crate::cli::ReferenceGain::Fixed(gain) => gain,
            crate::cli::ReferenceGain::Auto => f64::NAN,
        },
        linear_chain,
        scan_workers,
    };
    let graph_started = Instant::now();
    let result = graph::execute(&graph, context.sources.len(), &mut runner);
    for (node, stats) in &runner.stage_stats {
        let written = runner.log.line(&format!("kind=stage node={} concurrency={} accepted={} committed={} delivered={} peak_in_flight={} capacity_unit={}",node,stats.capacity,stats.accepted,stats.committed,stats.delivered,stats.peak,if graph.prepare_batch_size() > 1 && node == graph.prepare_node_id() { "batch_tasks" } else { "frames" }));
        // Diagnostics must not replace the graph's selected error or STOP.
        // With a successful graph, a summary append failure remains fatal.
        if result.is_ok() {
            written?;
        }
    }
    if let Err(message) = result {
        let _ = runner.log.line(&format!(
            "kind=node.error message={} cache_root={}",
            quote(&message),
            quote(&context.scratch.join("cache").display().to_string())
        ));
        proc::check_stop()?;
        // Cancellation can make the encoder observer report failure during
        // cleanup. Choose the already observed root before adding diagnostics.
        let worker_failure = runner
            .worker_failure
            .lock()
            .unwrap()
            .take()
            .filter(|e| message.ends_with(&e.message));
        let mut error = runner
            .failure
            .take()
            .or(worker_failure)
            .unwrap_or_else(|| Error::new(EXIT_FRAME, message));
        if error.code == EXIT_PREVIEW {
            error.message = format!(
                "streaming encoder failed: {}; encoder transcript: {}",
                error.message,
                runner.encoder.lock().unwrap().failure_detail()
            );
        }
        return Err(error);
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
    runner.log.progress("verify", 99);
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
        if context.options.verify_output {
            video::OutputVerify::Full
        } else {
            video::OutputVerify::Metadata
        },
    )?;
    timings.insert("validate_video".into(), validation_started.elapsed());
    // Delivered clipping statistics: written only after the graph completed and
    // the deliverable passed its output check. Frames that failed or never
    // committed are excluded because only ordered colour commits reached these
    // counters; the unit is channel samples (three per pixel), never pixels.
    let clip_frames = runner.color_frames;
    let clip_low = runner.clipped_low;
    let clip_high = runner.clipped_high;
    let clip_samples = runner.color_channel_samples;
    runner.log.line(&format!(
        "kind=clip frames={} clipped_low={} clipped_high={} clipped_total={} \
         total_channel_samples={} unit=channel_samples",
        clip_frames,
        clip_low,
        clip_high,
        clip_low + clip_high,
        clip_samples
    ))?;
    let bytes = video::file_size(&output)
        .ok_or_else(|| Error::new(EXIT_PREVIEW, "graph encoder produced no video"))?;
    let artifact = video::video_envelope_with_gain(
        &output,
        config.media_type(),
        &probe.codec_name,
        probe.frames.unwrap_or(sequence.frames.len() as u64),
        "graph streaming encoder",
        profile,
        runner.reference_gain,
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
            verify: if context.options.verify_output {
                "full"
            } else {
                "metadata"
            },
            retained_cache,
        },
    })
}
