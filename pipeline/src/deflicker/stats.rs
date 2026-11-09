// NumPy's default linear percentile interpolation, with explicit empty semantics.
pub(crate) fn percentile(values: &[f64], p: f64) -> f64 {
    let mut a: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if a.is_empty() {
        return f64::NAN;
    }
    a.sort_by(f64::total_cmp);
    let pos = (a.len() - 1) as f64 * p / 100.;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    a[lo] + (a[hi] - a[lo]) * (pos - lo as f64)
}
pub(crate) fn median(a: &[f64]) -> f64 {
    percentile(a, 50.)
}
pub(crate) fn mad(a: &[f64], center: f64) -> f64 {
    1.4826 * median(&a.iter().map(|x| (x - center).abs()).collect::<Vec<_>>())
}
pub(crate) fn finite(x: f64) -> Option<f64> {
    x.is_finite().then_some(x)
}
pub(crate) fn need(k: usize) -> usize {
    8.max(k.div_ceil(4))
}
pub(crate) fn interpolate(t: usize, a: usize, b: usize, x: f64, y: f64) -> f64 {
    x + (y - x) * (t - a) as f64 / (b - a) as f64
}
