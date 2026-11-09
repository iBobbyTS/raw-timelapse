use super::{
    pairwise::quadrants,
    stats::{finite, mad, median, percentile},
    Result,
};
pub const REC709_LUMA: [f64; 3] = [0.2126, 0.7152, 0.0722];
pub const CANDIDATE_STEP_EV: f64 = 0.05;
pub const SHORT_CONTEXT_FRAMES: usize = 24;
pub const GATE_P95_STEP_EV: f64 = 0.025;
pub const GATE_CURVATURE_RMS_EV: f64 = 0.012;
pub const GATE_SEVERE_EVENT_EV: f64 = 0.05;
pub const UNOBSERVABLE_PAIR_FRACTION: f64 = 0.75;
/// SDR input is normalized code/65535. Declared linear16 input is raw code,
/// with output = code/16384. Never infer a transfer from pixel format.
pub fn inverse_transfer(x: &[f64], transfer: &str) -> Result<Vec<f64>> {
    if !["srgb", "bt709", "linear16"].contains(&transfer) {
        return Err(format!("unsupported or unspecified transfer {transfer}"));
    }
    if x.iter().any(|v| !v.is_finite()) {
        return Err("nonfinite code value".into());
    }
    let out: Vec<_> = x
        .iter()
        .map(|&v| match transfer {
            "srgb" => {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            }
            "bt709" => {
                if v < 0.081 {
                    v / 4.5
                } else {
                    ((v + 0.099) / 1.099).powf(1. / 0.45)
                }
            }
            _ => v / 16384.,
        })
        .collect();
    if out.iter().any(|v| !v.is_finite()) {
        return Err("nonfinite inverse transfer output".into());
    }
    Ok(out)
}
pub fn classify_transfer_tag(tag: Option<&str>) -> Result<&'static str> {
    let t = tag.unwrap_or("unknown").to_lowercase();
    if ["unknown", "unspecified"].contains(&t.as_str()) {
        return Err("UNSPECIFIED_TRANSFER: explicit assumption required".into());
    }
    if [
        "smpte2084",
        "arib-std-b67",
        "log",
        "log10",
        "log16",
        "hlg",
        "pq",
        "slog",
        "slog2",
        "slog3",
        "vlog",
        "clog",
    ]
    .iter()
    .any(|s| t.contains(s))
    {
        return Err("UNSUPPORTED_TRANSFER: HDR/Log".into());
    }
    match t.as_str() {
        "srgb" | "iec61966-2-1" => Ok("srgb"),
        "bt709" | "bt.709" | "1" => Ok("bt709"),
        _ => Err(format!("UNSUPPORTED_TRANSFER: {t}")),
    }
}
pub fn resolve_transfer(
    tag: Option<&str>,
    requested: Option<&str>,
) -> Result<(String, Vec<String>)> {
    if let Some(r) = requested {
        if !["srgb", "bt709", "linear16"].contains(&r) {
            return Err("unsupported transfer assumption".into());
        }
        let mut warnings = vec![];
        if let Ok(t) = classify_transfer_tag(tag) {
            if t != r {
                warnings.push(format!(
                    "TRANSFER_TAG_MISMATCH: stream tagged {} but auditing as {r}",
                    tag.unwrap_or("unknown")
                ));
            }
        }
        Ok((r.into(), warnings))
    } else {
        Ok((classify_transfer_tag(tag)?.into(), vec![]))
    }
}
pub fn linear_luminance(
    rgb: &[[u16; 3]],
    width: usize,
    height: usize,
    transfer: &str,
) -> Result<Vec<f64>> {
    if width == 0 || height == 0 || width.checked_mul(height) != Some(rgb.len()) {
        return Err("expected HxWx3 uint16 RGB".into());
    }
    let codes: Vec<_> = rgb
        .iter()
        .flat_map(|p| {
            p.iter().map(|&v| {
                if transfer == "linear16" {
                    v as f64
                } else {
                    v as f64 / 65535.
                }
            })
        })
        .collect();
    let linear = inverse_transfer(&codes, transfer)?;
    Ok(linear
        .chunks_exact(3)
        .map(|p| p.iter().zip(REC709_LUMA).map(|(v, w)| v * w).sum())
        .collect())
}
fn image_shape(a: &[f64], w: usize, h: usize) -> Result<()> {
    if w == 0 || h == 0 || w.checked_mul(h) != Some(a.len()) || a.iter().any(|v| !v.is_finite()) {
        Err("invalid image shape or nonfinite pixels".into())
    } else {
        Ok(())
    }
}
fn mirror(mut i: isize, n: usize) -> usize {
    if n <= 1 {
        return 0;
    }
    let end = n as isize - 1;
    while i < 0 || i > end {
        if i < 0 {
            i = -i;
        }
        if i > end {
            i = 2 * end - i;
        }
    }
    i as usize
}
pub fn structure_blur(a: &[f64], w: usize, h: usize) -> Result<Vec<f64>> {
    image_shape(a, w, h)?;
    let mut kernel = [0.; 5];
    for (i, v) in kernel.iter_mut().enumerate() {
        *v = (-0.5 * ((i as f64 - 2.) / 1.2).powi(2)).exp();
    }
    let total: f64 = kernel.iter().sum();
    for v in &mut kernel {
        *v /= total;
    }
    let mut tmp = vec![0.; a.len()];
    let mut out = vec![0.; a.len()];
    for y in 0..h {
        for x in 0..w {
            for (i, &weight) in kernel.iter().enumerate() {
                tmp[y * w + x] += weight * a[mirror(y as isize + i as isize - 2, h) * w + x];
            }
        }
    }
    for y in 0..h {
        for x in 0..w {
            for (i, &weight) in kernel.iter().enumerate() {
                out[y * w + x] += weight * tmp[y * w + mirror(x as isize + i as isize - 2, w)];
            }
        }
    }
    Ok(out)
}
pub fn tile_grid(a: &[f64], w: usize, h: usize, nx: usize, ny: usize) -> Result<Vec<Vec<f64>>> {
    image_shape(a, w, h)?;
    if nx == 0 || ny == 0 || w % nx != 0 || h % ny != 0 {
        return Err("grid must divide image".into());
    }
    let tw = w / nx;
    let th = h / ny;
    let mut out = vec![];
    for j in 0..ny {
        for i in 0..nx {
            let mut row = vec![];
            for y in j * th..(j + 1) * th {
                row.extend_from_slice(&a[y * w + i * tw..y * w + (i + 1) * tw]);
            }
            out.push(row);
        }
    }
    Ok(out)
}
#[derive(Debug, Clone)]
pub struct TileStats {
    pub log_mean: Vec<f64>,
    pub log_pixels: Vec<Vec<f64>>,
    pub saturation_fraction: Vec<f64>,
}
impl TileStats {
    fn validate(&self, nx: usize, ny: usize) -> Result<()> {
        let k = self.log_mean.len();
        if nx == 0
            || ny == 0
            || nx.checked_mul(ny) != Some(k)
            || self.log_pixels.len() != k
            || self.saturation_fraction.len() != k
        {
            return Err("audit tile shape mismatch".into());
        }
        Ok(())
    }
}
pub fn tile_statistics(
    y: &[f64],
    saturation: &[f64],
    w: usize,
    h: usize,
    nx: usize,
    ny: usize,
) -> Result<TileStats> {
    let samples = tile_grid(y, w, h, nx, ny)?;
    let sats = tile_grid(saturation, w, h, nx, ny)?;
    let blurred = structure_blur(y, w, h)?;
    let logs: Vec<_> = blurred.iter().map(|v| v.max(1e-6).log2()).collect();
    let log_pixels = tile_grid(&logs, w, h, nx, ny)?;
    let log_mean = samples
        .into_iter()
        .map(|mut row| {
            row.sort_by(f64::total_cmp);
            let k = 1.max((row.len() as f64 * 0.05) as usize);
            if k.saturating_mul(2) >= row.len() {
                f64::NAN
            } else {
                let mean = row[k..row.len() - k].iter().sum::<f64>() / (row.len() - 2 * k) as f64;
                mean.max(1e-12).log2()
            }
        })
        .collect();
    let saturation_fraction = sats
        .iter()
        .map(|row| row.iter().sum::<f64>() / row.len() as f64)
        .collect();
    Ok(TileStats {
        log_mean,
        log_pixels,
        saturation_fraction,
    })
}
#[derive(Debug, Clone)]
pub struct PairMetric {
    pub common_ev: Option<f64>,
    pub spatial_mad_ev: Option<f64>,
    pub valid_tiles: usize,
    pub consensus_tiles: usize,
    pub consensus_fraction: Option<f64>,
    pub quadrants: usize,
    pub quality: String,
    pub local_p90_ev: Option<f64>,
}
pub fn pair_metric(
    now: &TileStats,
    previous: &TileStats,
    nx: usize,
    ny: usize,
    train: Option<&[bool]>,
) -> Result<(PairMetric, Vec<f64>, Vec<bool>)> {
    now.validate(nx, ny)?;
    previous.validate(nx, ny)?;
    let k = now.log_mean.len();
    if train.is_some_and(|t| t.len() != k) {
        return Err("audit training mask shape".into());
    }
    let mut good = vec![false; k];
    let d: Vec<_> = (0..k)
        .map(|i| now.log_mean[i] - previous.log_mean[i])
        .collect();
    for i in 0..k {
        if now.log_pixels[i].len() != previous.log_pixels[i].len() {
            return Err("audit pixel sample shape mismatch".into());
        }
        let diff: Vec<_> = now.log_pixels[i]
            .iter()
            .zip(&previous.log_pixels[i])
            .map(|(a, b)| a - b)
            .collect();
        let shape = if diff.iter().all(|v| v.is_finite()) {
            mad(&diff, median(&diff))
        } else {
            f64::NAN
        };
        good[i] = shape < 0.04
            && now.log_mean[i] > 0.002_f64.log2()
            && previous.log_mean[i] > 0.002_f64.log2()
            && now.saturation_fraction[i].max(previous.saturation_fraction[i]) < 0.01
            && train.is_none_or(|t| t[i]);
    }
    let count = good.iter().filter(|v| **v).count();
    let mut metric = PairMetric {
        common_ev: None,
        spatial_mad_ev: None,
        valid_tiles: count,
        consensus_tiles: 0,
        consensus_fraction: None,
        quadrants: 0,
        quality: "INSUFFICIENT_SUPPORT".into(),
        local_p90_ev: None,
    };
    if count < 24.max(k.div_ceil(4)) {
        return Ok((metric, d, good));
    }
    let values: Vec<_> = (0..k).filter(|&i| good[i]).map(|i| d[i]).collect();
    let med = median(&values);
    let spread = mad(&values, med);
    let inliers: Vec<_> = (0..k)
        .filter(|&i| good[i] && (d[i] - med).abs() <= 0.06)
        .collect();
    let frac = inliers.len() as f64 / count as f64;
    let quad = quadrants(inliers.iter().copied(), nx, ny);
    metric.common_ev = finite(med);
    metric.spatial_mad_ev = finite(spread);
    metric.consensus_tiles = inliers.len();
    metric.consensus_fraction = Some(frac);
    metric.quadrants = quad;
    metric.quality = if frac >= 0.6 && spread <= 0.06 && quad >= 3 {
        "RELIABLE_COMMON_CHANGE"
    } else {
        "LOCAL_OR_MIXED_CHANGE"
    }
    .into();
    metric.local_p90_ev = finite(percentile(
        &values.iter().map(|v| (v - med).abs()).collect::<Vec<_>>(),
        90.,
    ));
    Ok((metric, d, good))
}
#[derive(Debug, Clone)]
pub struct TemporalSummary {
    pub pairs: usize,
    pub usable_pairs: usize,
    pub median_abs_step_ev: Option<f64>,
    pub p95_abs_step_ev: Option<f64>,
    pub max_abs_step_ev: Option<f64>,
    pub curvature_rms_ev: Option<f64>,
    pub lag2_residual_rms_ev: Option<f64>,
    pub steps_over_0_02_ev: usize,
    pub steps_over_0_05_ev: usize,
}
fn rms(a: &[f64]) -> Option<f64> {
    if a.is_empty() {
        None
    } else {
        finite((a.iter().map(|v| v * v).sum::<f64>() / a.len() as f64).sqrt())
    }
}
pub fn temporal_summary(pairs: &[Option<f64>]) -> TemporalSummary {
    let a: Vec<_> = pairs.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
    let v: Vec<_> = a
        .iter()
        .filter(|v| v.is_finite())
        .map(|v| v.abs())
        .collect();
    let curv: Vec<_> = a
        .windows(2)
        .map(|w| 0.5 * (w[0] - w[1]))
        .filter(|v| v.is_finite())
        .collect();
    let lag2: Vec<_> = a
        .windows(3)
        .map(|w| 0.25 * (w[0] - 2. * w[1] + w[2]))
        .filter(|v| v.is_finite())
        .collect();
    TemporalSummary {
        pairs: a.len(),
        usable_pairs: v.len(),
        median_abs_step_ev: finite(median(&v)),
        p95_abs_step_ev: finite(percentile(&v, 95.)),
        max_abs_step_ev: finite(v.iter().copied().fold(f64::NEG_INFINITY, f64::max)),
        curvature_rms_ev: rms(&curv),
        lag2_residual_rms_ev: rms(&lag2),
        steps_over_0_02_ev: v.iter().filter(|v| **v > 0.02).count(),
        steps_over_0_05_ev: v.iter().filter(|v| **v > 0.05).count(),
    }
}
pub fn segment_status(frames: usize, s: &TemporalSummary) -> &'static str {
    if frames < SHORT_CONTEXT_FRAMES {
        "SHORT_CONTEXT"
    } else if s.usable_pairs < 8
        || s.pairs == 0
        || (s.usable_pairs as f64 / s.pairs as f64) < UNOBSERVABLE_PAIR_FRACTION
    {
        "UNOBSERVABLE"
    } else if s.max_abs_step_ev.is_some_and(|v| v > GATE_SEVERE_EVENT_EV)
        || s.p95_abs_step_ev.is_some_and(|v| v > GATE_P95_STEP_EV)
        || s.curvature_rms_ev
            .is_some_and(|v| v > GATE_CURVATURE_RMS_EV)
    {
        "FAIL"
    } else {
        "PASS"
    }
}
pub fn analysis_geometry(width: usize, height: usize) -> Result<(usize, usize)> {
    if width == 0 || height == 0 {
        return Err("invalid source geometry".into());
    }
    let w = ((width.min(912) as f64 / 24.).round_ties_even() as usize).max(1) * 24;
    let hf = height as f64 / width as f64 * w as f64 / 18.;
    if !hf.is_finite() || hf > usize::MAX as f64 / 18. {
        return Err("audit geometry overflow".into());
    }
    let h = (hf.round_ties_even() as usize)
        .max(1)
        .checked_mul(18)
        .ok_or("audit geometry overflow")?;
    if w / 24 < 8 || h / 18 < 8 {
        return Err("analysis resolution too small for tile grid".into());
    }
    Ok((w, h))
}
pub fn roi_level(y: &[f64], w: usize, h: usize, roi: [f64; 4]) -> Result<Option<f64>> {
    image_shape(y, w, h)?;
    if roi.iter().any(|v| !v.is_finite())
        || roi[0] < 0.
        || roi[1] < 0.
        || roi[2] > 1.
        || roi[3] > 1.
        || roi[0] >= roi[2]
        || roi[1] >= roi[3]
    {
        return Err("invalid audit ROI".into());
    }
    let mut a = vec![];
    for j in (roi[1] * h as f64) as usize..(roi[3] * h as f64) as usize {
        for i in (roi[0] * w as f64) as usize..(roi[2] * w as f64) as usize {
            a.push(y[j * w + i]);
        }
    }
    a.sort_by(f64::total_cmp);
    let k = 1.max((0.05 * a.len() as f64) as usize);
    if 2 * k >= a.len() {
        return Ok(None);
    }
    let mean = a[k..a.len() - k].iter().sum::<f64>() / (a.len() - 2 * k) as f64;
    Ok(if mean > 0.0001 {
        finite(mean.log2())
    } else {
        None
    })
}
#[derive(Debug, Clone)]
pub struct RoiSeriesMetrics {
    pub p95_step_ev: Option<f64>,
    pub hp3_rms_ev: Option<f64>,
    pub range_ev: Option<f64>,
    pub observed_frames: usize,
}
pub fn roi_series_metrics(levels: &[Option<f64>]) -> RoiSeriesMetrics {
    let a: Vec<_> = levels.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
    let steps: Vec<_> = a.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
    let hp: Vec<_> = a
        .windows(3)
        .map(|w| w[1] - 0.5 * (w[0] + w[2]))
        .filter(|v| v.is_finite())
        .collect();
    let valid: Vec<_> = a.iter().copied().filter(|v| v.is_finite()).collect();
    let range = valid.iter().copied().fold(f64::NEG_INFINITY, f64::max)
        - valid.iter().copied().fold(f64::INFINITY, f64::min);
    RoiSeriesMetrics {
        p95_step_ev: finite(percentile(&steps, 95.)),
        hp3_rms_ev: rms(&hp),
        range_ev: finite(range),
        observed_frames: valid.len(),
    }
}
/// Pure audit aggregation. S03 supplies decoded TileStats and metadata boundaries
/// (scene cuts union capture-time gaps, never filename suspicions).
#[derive(Debug, Clone)]
pub struct AuditPair {
    pub index: usize,
    pub boundary: bool,
    pub metric: PairMetric,
    pub candidate_global_jump: bool,
}
#[derive(Debug, Clone)]
pub struct AuditSegment {
    pub start: usize,
    pub end_exclusive: usize,
    pub frames: usize,
    pub summary: TemporalSummary,
    pub status: String,
}
#[derive(Debug, Clone)]
pub struct AuditSummary {
    pub pairs: Vec<AuditPair>,
    pub segments: Vec<AuditSegment>,
    pub status: String,
    pub candidate_count: usize,
}
pub fn audit_features(
    features: &[TileStats],
    nx: usize,
    ny: usize,
    boundaries: &[usize],
) -> Result<AuditSummary> {
    for f in features {
        f.validate(nx, ny)?;
    }
    let n = features.len();
    let mut starts = std::collections::BTreeSet::from([0, n]);
    for &b in boundaries {
        if b < n {
            starts.insert(b);
        }
    }
    let mut pairs = vec![];
    for t in 1..n {
        let (mut metric, _, _) = pair_metric(&features[t], &features[t - 1], nx, ny, None)?;
        let boundary = starts.contains(&t);
        if boundary {
            metric.quality = "SOURCE_DISCONTINUITY".into();
        }
        let candidate_global_jump = !boundary
            && metric.quality == "RELIABLE_COMMON_CHANGE"
            && metric
                .common_ev
                .is_some_and(|v| v.abs() > CANDIDATE_STEP_EV);
        pairs.push(AuditPair {
            index: t,
            boundary,
            metric,
            candidate_global_jump,
        });
    }
    let starts: Vec<_> = starts.into_iter().collect();
    let mut segments = vec![];
    for ab in starts.windows(2) {
        let (a, b) = (ab[0], ab[1]);
        let values: Vec<_> = pairs
            .iter()
            .filter(|p| a <= p.index && p.index < b)
            .map(|p| {
                if p.metric.quality == "RELIABLE_COMMON_CHANGE" {
                    p.metric.common_ev
                } else {
                    None
                }
            })
            .collect();
        let summary = temporal_summary(&values);
        let status = segment_status(b - a, &summary).into();
        segments.push(AuditSegment {
            start: a,
            end_exclusive: b,
            frames: b - a,
            summary,
            status,
        });
    }
    let gated: Vec<_> = segments
        .iter()
        .filter(|s| !["SHORT_CONTEXT", "UNOBSERVABLE"].contains(&s.status.as_str()))
        .collect();
    let status = if !gated.is_empty() {
        if gated.iter().any(|s| s.status == "FAIL") {
            "FAIL"
        } else {
            "PASS"
        }
    } else if segments.iter().all(|s| s.status == "UNOBSERVABLE") {
        "UNOBSERVABLE"
    } else {
        "SHORT_CONTEXT"
    };
    let candidate_count = pairs.iter().filter(|p| p.candidate_global_jump).count();
    Ok(AuditSummary {
        pairs,
        segments,
        status: status.into(),
        candidate_count,
    })
}
