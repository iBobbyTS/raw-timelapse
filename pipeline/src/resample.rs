//! Explicit scene-linear resampling for the graph's resize node.
//!
//! Resampling runs on signed FP32 AP1/D60 samples *after* EV and *before* the
//! target Log curve, so every preset and codec sees the same linear-light
//! geometry. Separable Lanczos-3 with a widened kernel for downscaling; weights
//! are normalized after edge clamping, and a linear ramp is preserved exactly
//! in the interior for an integer scale. Nothing here quantizes: the only
//! quantization is the colour profile's 10-bit encoder handoff.

use std::f64::consts::PI;

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

fn lanczos3(x: f64) -> f64 {
    let x = x.abs();
    if x < 3.0 {
        sinc(x) * sinc(x / 3.0)
    } else {
        0.0
    }
}

/// Normalized source contributions for one output coordinate, with the kernel
/// widened by the scale factor when downsampling to suppress aliasing.
fn contributions_window(
    output: u32,
    start: f64,
    end: f64,
    low: i64,
    high: i64,
) -> Vec<Vec<(usize, f64)>> {
    let ratio = (end - start) / f64::from(output);
    let filter_scale = ratio.max(1.0);
    let support = 3.0 * filter_scale;
    (0..output)
        .map(|index| {
            let center = start + (f64::from(index) + 0.5) * ratio - 0.5;
            let start = (center - support).ceil() as i64;
            let end = (center + support).floor() as i64;
            let mut taps = Vec::new();
            let mut total = 0.0;
            for source in start..=end {
                let weight = lanczos3((source as f64 - center) / filter_scale);
                if weight == 0.0 {
                    continue;
                }
                let clamped = source.clamp(low, high) as usize;
                match taps.iter_mut().find(|(index, _)| *index == clamped) {
                    Some((_, existing)) => *existing += weight,
                    None => taps.push((clamped, weight)),
                }
                total += weight;
            }
            if total == 0.0 {
                taps.push((center.round().clamp(low as f64, high as f64) as usize, 1.0));
            } else {
                for (_, weight) in &mut taps {
                    *weight /= total;
                }
            }
            taps
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropWindow {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub low_x: i64,
    pub high_x: i64,
    pub low_y: i64,
    pub high_y: i64,
}
pub fn crop_window(
    width: u32,
    height: u32,
    out_width: u32,
    out_height: u32,
    bounds: [f64; 4],
) -> Result<CropWindow, String> {
    if width == 0
        || height == 0
        || out_width == 0
        || out_height == 0
        || bounds.iter().any(|v| !v.is_finite() || *v < 0. || *v > 1.)
        || bounds[0] >= bounds[2]
        || bounds[1] >= bounds[3]
    {
        return Err("invalid lens crop bounds or output geometry".into());
    }
    // Only complete certified output pixels are eligible as resize taps. The
    // reconstruction footprint is clamped to these valid pixels, never black
    // pixels beyond the certified rectangle.
    let low_x = (bounds[0] * f64::from(width - 1)).ceil() as i64;
    let high_x = (bounds[2] * f64::from(width - 1)).floor() as i64;
    let low_y = (bounds[1] * f64::from(height - 1)).ceil() as i64;
    let high_y = (bounds[3] * f64::from(height - 1)).floor() as i64;
    if low_x > high_x || low_y > high_y {
        return Err("lens crop contains no complete pixels".into());
    }
    let available_width = (high_x - low_x + 1) as f64;
    let available_height = (high_y - low_y + 1) as f64;
    let aspect = f64::from(out_width) / f64::from(out_height);
    let crop_width = available_width.min(available_height * aspect);
    let crop_height = available_height.min(available_width / aspect);
    let left = low_x as f64 + (available_width - crop_width) * 0.5;
    let top = low_y as f64 + (available_height - crop_height) * 0.5;
    Ok(CropWindow {
        left,
        top,
        right: left + crop_width,
        bottom: top + crop_height,
        low_x,
        high_x,
        low_y,
        high_y,
    })
}
/// Resample one interleaved little-endian FP32 RGB frame into new geometry,
/// optionally restricted to a certified sub-rectangle (`bounds`).
pub fn resize_scene_linear_region(
    input: &[u8],
    width: u32,
    height: u32,
    out_width: u32,
    out_height: u32,
    bounds: Option<[f64; 4]>,
) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 || out_width == 0 || out_height == 0 {
        return Err("resize geometry must be positive".into());
    }
    let expected = width as usize * height as usize * 12;
    if input.len() != expected {
        return Err(format!(
            "resize input is {} bytes, expected {}x{} FP32 RGB ({expected})",
            input.len(),
            width,
            height
        ));
    }
    let window = match bounds {
        Some(bounds) => crop_window(width, height, out_width, out_height, bounds)?,
        None => CropWindow {
            left: 0.,
            top: 0.,
            right: f64::from(width),
            bottom: f64::from(height),
            low_x: 0,
            high_x: i64::from(width - 1),
            low_y: 0,
            high_y: i64::from(height - 1),
        },
    };
    if width == out_width
        && height == out_height
        && window.left == 0.
        && window.top == 0.
        && window.right == f64::from(width)
        && window.bottom == f64::from(height)
    {
        return Ok(input.to_vec());
    }
    let sample = |frame: &[u8], index: usize| -> f64 {
        f32::from_le_bytes(frame[index * 4..index * 4 + 4].try_into().unwrap()) as f64
    };

    let horizontal = contributions_window(
        out_width,
        window.left,
        window.right,
        window.low_x,
        window.high_x,
    );
    let mut intermediate = vec![0.0f32; out_width as usize * height as usize * 3];
    for y in 0..height as usize {
        let row = y * width as usize * 3;
        for (x, taps) in horizontal.iter().enumerate() {
            let mut acc = [0.0f64; 3];
            for (source, weight) in taps {
                let base = row + source * 3;
                for channel in 0..3 {
                    acc[channel] += sample(input, base + channel) * weight;
                }
            }
            let base = (y * out_width as usize + x) * 3;
            for channel in 0..3 {
                intermediate[base + channel] = acc[channel] as f32;
            }
        }
    }

    let vertical = contributions_window(
        out_height,
        window.top,
        window.bottom,
        window.low_y,
        window.high_y,
    );
    let mut output = vec![0u8; out_width as usize * out_height as usize * 12];
    for (y, taps) in vertical.iter().enumerate() {
        for x in 0..out_width as usize {
            let mut acc = [0.0f64; 3];
            for (source, weight) in taps {
                let base = (source * out_width as usize + x) * 3;
                for channel in 0..3 {
                    acc[channel] += f64::from(intermediate[base + channel]) * weight;
                }
            }
            let base = (y * out_width as usize + x) * 12;
            for channel in 0..3 {
                let value = acc[channel] as f32;
                if !value.is_finite() {
                    return Err("resize produced a nonfinite sample".into());
                }
                output[base + channel * 4..base + channel * 4 + 4]
                    .copy_from_slice(&value.to_le_bytes());
            }
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [f32; 3]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for y in 0..height {
            for x in 0..width {
                for value in pixel(x, y) {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
        bytes
    }

    fn pixel(bytes: &[u8], width: u32, x: u32, y: u32) -> [f32; 3] {
        let base = ((y * width + x) * 12) as usize;
        [0, 4, 8].map(|offset| {
            f32::from_le_bytes(bytes[base + offset..base + offset + 4].try_into().unwrap())
        })
    }

    #[test]
    fn constant_scene_linear_survives_any_geometry() {
        let source = frame(16, 12, |_, _| [0.18, -0.002, 4.0]);
        for (width, height) in [(8, 6), (32, 24), (7, 5), (16, 12)] {
            let out = resize_scene_linear_region(&source, 16, 12, width, height, None).unwrap();
            assert_eq!(out.len(), width as usize * height as usize * 12);
            for y in 0..height {
                for x in 0..width {
                    assert_eq!(pixel(&out, width, x, y), [0.18, -0.002, 4.0]);
                }
            }
        }
    }

    #[test]
    fn linear_ramp_is_preserved_in_the_interior() {
        // A scene-linear ramp resampled by an integer factor must keep its
        // exact slope away from the edge-clamped borders: normalized symmetric
        // kernels preserve linear functions. The ramp varies only in x, so the
        // constant vertical pass preserves each row.
        let source = frame(32, 4, |x, _| {
            let value = 0.05 + 0.02 * x as f32;
            [value; 3]
        });
        let out = resize_scene_linear_region(&source, 32, 4, 64, 8, None).unwrap();
        for x in 8..=56 {
            let value = f64::from(pixel(&out, 64, x, 4)[0]);
            let expected = 0.05 + 0.02 * ((x as f64 + 0.5) / 2.0 - 0.5);
            // A truncated sinc kernel at half-integer centres is not exactly
            // linear-preserving; the deviation is far below one 10-bit code.
            assert!(
                (value - expected).abs() < 2e-3,
                "upscale x={x}: {value} vs {expected}"
            );
            assert!((value - expected).abs() as f32 * 876.0 < 2.0);
        }
        let down = resize_scene_linear_region(&source, 32, 4, 16, 2, None).unwrap();
        for x in 3..=12 {
            let value = f64::from(pixel(&down, 16, x, 1)[0]);
            let expected = 0.05 + 0.02 * ((x as f64 + 0.5) * 2.0 - 0.5);
            assert!(
                (value - expected).abs() < 1e-6,
                "downscale x={x}: {value} vs {expected}"
            );
        }
    }

    #[test]
    fn safe_crop_never_samples_invalid_frame_border_and_preserves_aspect() {
        let source = frame(32, 24, |x, y| {
            if (4..28).contains(&x) && (3..21).contains(&y) {
                [-0.25, 0.5, 4.]
            } else {
                [1000., -1000., 0.]
            }
        });
        let bounds = [4. / 31., 3. / 23., 27. / 31., 20. / 23.];
        for (w, h) in [(32, 24), (64, 36), (8, 8)] {
            let window = crop_window(32, 24, w, h, bounds).unwrap();
            assert!(
                ((window.right - window.left) / (window.bottom - window.top)
                    - f64::from(w) / f64::from(h))
                .abs()
                    < 1e-12
            );
            let out = resize_scene_linear_region(&source, 32, 24, w, h, Some(bounds)).unwrap();
            for y in 0..h {
                for x in 0..w {
                    assert_eq!(pixel(&out, w, x, y), [-0.25, 0.5, 4.]);
                }
            }
        }
        let unchanged =
            resize_scene_linear_region(&source, 32, 24, 32, 24, Some([0., 0., 1., 1.])).unwrap();
        assert_eq!(unchanged, source);
        assert!(crop_window(32, 24, 64, 36, [0.8, 0.8, 0.1, 0.1]).is_err());
    }
    #[test]
    fn rejects_mismatched_or_degenerate_geometry() {
        let source = frame(4, 3, |_, _| [0.1; 3]);
        assert!(resize_scene_linear_region(&source, 4, 3, 0, 3, None).is_err());
        assert!(resize_scene_linear_region(&source, 4, 3, 4, 0, None).is_err());
        assert!(resize_scene_linear_region(&source[..source.len() - 1], 4, 3, 2, 2, None).is_err());
    }
}
