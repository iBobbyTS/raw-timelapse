//! Finite report data only. S03 owns capture metadata, orchestration and writing.
use super::{
    photometry::TemporalSummary, validation::ValidationSummary, ExposureConfig, ExposureResult,
    FrameEstimate, Result,
};
use crate::json::{to_pretty_string, Json};
fn opt(v: Option<f64>) -> Json {
    v.filter(|v| v.is_finite()).map_or(Json::Null, Json::Float)
}
fn strings(v: &[String]) -> Json {
    Json::Array(v.iter().cloned().map(Json::Str).collect())
}
/// Recursively converts nonfinite diagnostics to JSON null, including payloads
/// supplied by S03. No NaN/Infinity tokens can leave this report serializer.
pub fn finite_json(j: &Json) -> Json {
    match j {
        Json::Float(v) if !v.is_finite() => Json::Null,
        Json::Array(v) => Json::Array(v.iter().map(finite_json).collect()),
        Json::Object(v) => {
            Json::Object(v.iter().map(|(k, v)| (k.clone(), finite_json(v))).collect())
        }
        v => v.clone(),
    }
}
#[derive(Debug, Clone)]
pub struct ReportFrame {
    pub estimate: FrameEstimate,
    pub filename: String,
    pub capture_time: Option<String>,
}
impl ReportFrame {
    pub fn to_json(&self) -> Json {
        let f = &self.estimate;
        Json::obj(vec![
            ("index", Json::Int(f.index as i64)),
            ("filename", Json::Str(self.filename.clone())),
            ("status", Json::Str(f.status.clone())),
            ("valid_tile_count", Json::Int(f.valid_tile_count as i64)),
            ("consensus_fraction", opt(f.consensus_fraction)),
            ("residual_mad_ev", opt(f.residual_mad_ev)),
            ("common_residual_ev", opt(f.common_residual_ev)),
            (
                "cumulative_confirmed_step_ev",
                opt(Some(f.cumulative_confirmed_step_ev)),
            ),
            ("requested_ev", opt(f.requested_ev)),
            ("applied_ev", opt(f.applied_ev)),
            ("render_ev", opt(f.render_ev)),
            ("gain", opt(f.gain())),
            ("render_target_error_ev", opt(f.render_target_error_ev)),
            (
                "new_output_clipping_fraction",
                opt(f.new_output_clipping_fraction),
            ),
            ("measured_level_ev", opt(f.measured_level_ev)),
            ("desired_level_ev", opt(f.desired_level_ev)),
            ("segment_id", Json::Int(f.segment_id as i64)),
            ("warnings", strings(&f.warnings)),
            (
                "capture_time",
                self.capture_time.clone().map_or(Json::Null, Json::Str),
            ),
        ])
    }
}
#[derive(Debug, Clone)]
pub struct ExposureReport {
    pub run_status: String,
    pub config: Json,
    pub frames: Vec<ReportFrame>,
    pub pairwise_estimation: Json,
    pub scene_cuts: Json,
    pub source_discontinuities: Json,
    pub gain_probe: Json,
    pub feedback_history: Vec<Json>,
    pub encoded_temporal_quality: Json,
}
impl ExposureReport {
    pub fn from_exposure(
        e: &ExposureResult,
        cfg: &ExposureConfig,
        filenames: &[String],
        capture_times: &[Option<String>],
    ) -> Result<Self> {
        if filenames.len() != e.frames.len() || capture_times.len() != e.frames.len() {
            return Err("report metadata count mismatch".into());
        }
        let config = Json::obj(vec![
            ("period", Json::Int(cfg.period as i64)),
            ("strength", opt(Some(cfg.strength))),
            ("brightness", Json::Str(cfg.brightness.clone())),
            ("estimator", Json::Str(cfg.estimator.clone())),
            (
                "ev_limits",
                cfg.ev_limits.map_or(Json::Null, |(a, b)| {
                    Json::Array(vec![opt(Some(a)), opt(Some(b))])
                }),
            ),
            (
                "steps",
                Json::Array(
                    cfg.steps
                        .iter()
                        .map(|s| {
                            Json::obj(vec![
                                ("start_frame", Json::Int(s.start_frame as i64)),
                                ("delta_ev", opt(Some(s.delta_ev))),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "exclude_rois",
                Json::Array(
                    cfg.exclude_rois
                        .iter()
                        .map(|r| Json::Array(r.iter().map(|&v| opt(Some(v))).collect()))
                        .collect(),
                ),
            ),
        ]);
        Ok(Self {
            run_status: "PENDING".into(),
            config,
            frames: e
                .frames
                .iter()
                .zip(filenames)
                .zip(capture_times)
                .map(|((e, n), c)| ReportFrame {
                    estimate: e.clone(),
                    filename: n.clone(),
                    capture_time: c.clone(),
                })
                .collect(),
            pairwise_estimation: e.pairwise_summary.clone(),
            scene_cuts: Json::Array(
                e.scene_cuts
                    .iter()
                    .map(|c| {
                        Json::obj(vec![
                            ("index", Json::Int(c.index as i64)),
                            ("median_ev", opt(Some(c.median_ev))),
                            ("tile_mad_ev", opt(Some(c.tile_mad_ev))),
                            ("common_tiles", Json::Int(c.common_tiles as i64)),
                        ])
                    })
                    .collect(),
            ),
            source_discontinuities: Json::Null,
            gain_probe: Json::Null,
            feedback_history: vec![],
            encoded_temporal_quality: Json::Null,
        })
    }
    pub fn to_json(&self) -> Json {
        let mut counts = std::collections::BTreeMap::<String, i64>::new();
        for f in &self.frames {
            *counts.entry(f.estimate.status.clone()).or_default() += 1;
        }
        finite_json(&Json::obj(vec![
            ("run_status", Json::Str(self.run_status.clone())),
            ("config", self.config.clone()),
            (
                "frames",
                Json::Array(self.frames.iter().map(ReportFrame::to_json).collect()),
            ),
            ("pairwise_estimation", self.pairwise_estimation.clone()),
            ("scene_cuts", self.scene_cuts.clone()),
            (
                "source_discontinuities",
                self.source_discontinuities.clone(),
            ),
            ("gain_probe", self.gain_probe.clone()),
            (
                "feedback_history",
                Json::Array(self.feedback_history.clone()),
            ),
            (
                "encoded_temporal_quality",
                self.encoded_temporal_quality.clone(),
            ),
            (
                "status_counts",
                Json::Object(counts.into_iter().map(|(k, n)| (k, Json::Int(n))).collect()),
            ),
        ]))
    }
    pub fn serialize(&self) -> String {
        to_pretty_string(&self.to_json())
    }
}
impl ValidationSummary {
    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            (
                "errors",
                Json::Array(self.errors.iter().map(|v| opt(*v)).collect()),
            ),
            ("median_abs_error_ev", opt(self.median_abs_error_ev)),
            ("p95_abs_error_ev", opt(self.p95_abs_error_ev)),
            (
                "outlier_frames",
                Json::Array(
                    self.outlier_frames
                        .iter()
                        .map(|&v| Json::Int(v as i64))
                        .collect(),
                ),
            ),
            (
                "unobservable_frames",
                Json::Array(
                    self.unobservable_frames
                        .iter()
                        .map(|&v| Json::Int(v as i64))
                        .collect(),
                ),
            ),
            ("passed", Json::Bool(self.passed)),
            ("iteration", Json::Int(self.iteration as i64)),
        ])
    }
}
impl TemporalSummary {
    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            ("pairs", Json::Int(self.pairs as i64)),
            ("usable_pairs", Json::Int(self.usable_pairs as i64)),
            ("median_abs_step_ev", opt(self.median_abs_step_ev)),
            ("p95_abs_step_ev", opt(self.p95_abs_step_ev)),
            ("max_abs_step_ev", opt(self.max_abs_step_ev)),
            ("curvature_rms_ev", opt(self.curvature_rms_ev)),
            ("lag2_residual_rms_ev", opt(self.lag2_residual_rms_ev)),
            (
                "steps_over_0_02_ev",
                Json::Int(self.steps_over_0_02_ev as i64),
            ),
            (
                "steps_over_0_05_ev",
                Json::Int(self.steps_over_0_05_ev as i64),
            ),
        ])
    }
}
