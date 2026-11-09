use super::Result;
use crate::json::{self, Json};
#[derive(Debug, Clone)]
pub struct FrameMeasurement {
    pub source_sha256: String,
    pub log_luminance: Vec<f64>,
    pub valid: Vec<bool>,
    pub centers: Vec<[f64; 2]>,
    pub saturation_fraction: Vec<f64>,
    pub output_clip_fraction: Vec<f64>,
    pub width: usize,
    pub height: usize,
    pub grid_nx: usize,
    pub grid_ny: usize,
    pub saturation_mask_quality: String,
    /// Ragged samples; missing tail entries behave like legacy NaN padding.
    pub log_pixel_samples: Option<Vec<Vec<f64>>>,
    pub log_channel_means: Option<Vec<[f64; 3]>>,
}
impl FrameMeasurement {
    pub fn validate(&self) -> Result<()> {
        let k = self.log_luminance.len();
        if self.grid_nx == 0
            || self.grid_ny == 0
            || self.grid_nx.checked_mul(self.grid_ny) != Some(k)
            || self.valid.len() != k
            || self.centers.len() != k
            || self.saturation_fraction.len() != k
            || self.output_clip_fraction.len() != k
            || self.width == 0
            || self.height == 0
            || self
                .log_pixel_samples
                .as_ref()
                .is_some_and(|s| s.len() != k)
            || self
                .log_channel_means
                .as_ref()
                .is_some_and(|s| s.len() != k)
        {
            return Err("measurement shape mismatch".into());
        }
        for i in 0..k {
            if self.valid[i] && !self.log_luminance[i].is_finite() {
                return Err("nonfinite valid photometry".into());
            }
            if self.centers[i]
                .iter()
                .any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
                || [self.saturation_fraction[i], self.output_clip_fraction[i]]
                    .iter()
                    .any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
            {
                return Err("invalid measurement geometry or occupancy".into());
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct AnalysisSidecar {
    pub measurement: FrameMeasurement,
    pub roi: [f64; 4],
    pub roi_pixel_bounds: [usize; 4],
    pub luma_weights: [f64; 3],
    pub quantum_linear: f64,
    pub floor: f64,
    pub exposure_ev: f64,
    pub analysis_domain: String,
    pub output_clip_quality: String,
}
fn field<'a>(j: &'a Json, k: &str) -> Result<&'a Json> {
    j.get(k).ok_or_else(|| format!("missing {k}"))
}
fn num(j: &Json) -> Result<f64> {
    j.as_f64()
        .filter(|v| v.is_finite())
        .ok_or_else(|| "expected finite number".into())
}
fn uint(j: &Json) -> Result<usize> {
    j.as_u64()
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| "expected unsigned integer".into())
}
fn string(j: &Json) -> Result<String> {
    j.as_str()
        .map(str::to_owned)
        .ok_or_else(|| "expected string".into())
}
fn arr(j: &Json) -> Result<&[Json]> {
    j.as_array().ok_or_else(|| "expected array".into())
}
fn numbers<const N: usize>(j: &Json) -> Result<[f64; N]> {
    let a = arr(j)?;
    if a.len() != N {
        return Err("array length mismatch".into());
    }
    let mut out = [0.; N];
    for (o, v) in out.iter_mut().zip(a) {
        *o = num(v)?;
    }
    Ok(out)
}
impl AnalysisSidecar {
    pub fn parse(text: &str) -> Result<Self> {
        Self::from_json(&json::parse(text)?)
    }
    pub fn from_json(j: &Json) -> Result<Self> {
        if field(j, "schema_version")?.as_u64() != Some(1)
            || field(j, "kind")?.as_str() != Some("rawtl-analysis-sidecar")
        {
            return Err("unsupported analysis schema".into());
        }
        let a = field(j, "analysis")?;
        let mut m = FrameMeasurement {
            source_sha256: string(field(j, "source_sha256")?)?,
            log_luminance: vec![],
            valid: vec![],
            centers: vec![],
            saturation_fraction: vec![],
            output_clip_fraction: vec![],
            width: uint(field(a, "downsampled_width")?)?,
            height: uint(field(a, "downsampled_height")?)?,
            grid_nx: uint(field(a, "grid_nx")?)?,
            grid_ny: uint(field(a, "grid_ny")?)?,
            saturation_mask_quality: string(field(a, "saturation_mask_quality")?)?,
            log_pixel_samples: Some(vec![]),
            log_channel_means: Some(vec![]),
        };
        let mut samples = vec![];
        let mut channels = vec![];
        for t in arr(field(j, "tiles")?)? {
            let v = match field(t, "valid")? {
                Json::Bool(b) => *b,
                _ => return Err("expected valid bool".into()),
            };
            let l = field(t, "log_luminance")?;
            if v == matches!(l, Json::Null) {
                return Err("log_luminance must be null iff invalid".into());
            }
            m.valid.push(v);
            m.log_luminance.push(if v { num(l)? } else { f64::NAN });
            m.centers.push(numbers(field(t, "center")?)?);
            m.saturation_fraction
                .push(num(field(t, "saturation_fraction")?)?);
            m.output_clip_fraction
                .push(num(field(t, "output_clip_fraction")?)?);
            samples.push(
                arr(field(t, "structure_samples")?)?
                    .iter()
                    .map(num)
                    .collect::<Result<Vec<_>>>()?,
            );
            let c = arr(field(t, "channel_log_means")?)?;
            if c.len() != 3 {
                return Err("channel count mismatch".into());
            }
            let mut cs = [f64::NAN; 3];
            for i in 0..3 {
                if !matches!(c[i], Json::Null) {
                    cs[i] = num(&c[i])?;
                }
            }
            channels.push(cs);
        }
        m.log_pixel_samples = Some(samples);
        m.log_channel_means = Some(channels);
        m.validate()?;
        let r = arr(field(a, "roi_pixel_bounds")?)?;
        if r.len() != 4 {
            return Err("ROI bounds shape".into());
        }
        let mut bounds = [0; 4];
        for i in 0..4 {
            bounds[i] = uint(&r[i])?;
        }
        let out = Self {
            measurement: m,
            roi: numbers(field(a, "roi")?)?,
            roi_pixel_bounds: bounds,
            luma_weights: numbers(field(a, "luma_weights")?)?,
            quantum_linear: num(field(a, "quantum_linear")?)?,
            floor: num(field(a, "floor")?)?,
            exposure_ev: num(field(a, "exposure_ev")?)?,
            analysis_domain: string(field(a, "analysis_domain")?)?,
            output_clip_quality: string(field(a, "output_clip_quality")?)?,
        };
        if out.quantum_linear <= 0.
            || out.floor <= 0.
            || out.roi[0] < 0.
            || out.roi[1] < 0.
            || out.roi[2] <= 0.
            || out.roi[3] <= 0.
            || out.roi[0] + out.roi[2] > 1.
            || out.roi[1] + out.roi[3] > 1.
            || bounds[0] >= bounds[2]
            || bounds[1] >= bounds[3]
            || bounds[2] > out.measurement.width
            || bounds[3] > out.measurement.height
        {
            return Err("invalid analysis geometry".into());
        }
        Ok(out)
    }
}
pub(crate) fn compatible(ms: &[FrameMeasurement]) -> Result<usize> {
    let Some(first) = ms.first() else {
        return Err("empty exposure sequence".into());
    };
    first.validate()?;
    for (i, m) in ms.iter().enumerate() {
        m.validate()?;
        if m.centers != first.centers || m.grid_nx != first.grid_nx || m.grid_ny != first.grid_ny {
            return Err(format!("processing conditions changed at frame {i}"));
        }
    }
    Ok(first.valid.len())
}
