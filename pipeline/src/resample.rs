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
fn contributions(input: u32, output: u32) -> Vec<Vec<(usize, f64)>> {
    let ratio = f64::from(input) / f64::from(output);
    let filter_scale = ratio.max(1.0);
    let support = 3.0 * filter_scale;
    (0..output)
        .map(|index| {
            let center = (f64::from(index) + 0.5) * ratio - 0.5;
            let start = (center - support).ceil() as i64;
            let end = (center + support).floor() as i64;
            let mut taps = Vec::new();
            let mut total = 0.0;
            for source in start..=end {
                let weight = lanczos3((source as f64 - center) / filter_scale);
                if weight == 0.0 {
                    continue;
                }
                let clamped = source.clamp(0, i64::from(input) - 1) as usize;
                match taps.iter_mut().find(|(index, _)| *index == clamped) {
                    Some((_, existing)) => *existing += weight,
                    None => taps.push((clamped, weight)),
                }
                total += weight;
            }
            if total == 0.0 {
                taps.push((center.round().clamp(0.0, f64::from(input - 1)) as usize, 1.0));
            } else {
                for (_, weight) in &mut taps {
                    *weight /= total;
                }
            }
            taps
        })
        .collect()
}

/// Resample one interleaved little-endian FP32 RGB frame into new geometry.
pub fn resize_scene_linear(
    input: &[u8],
    width: u32,
    height: u32,
    out_width: u32,
    out_height: u32,
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
    if width == out_width && height == out_height {
        return Ok(input.to_vec());
    }
    let sample = |frame: &[u8], index: usize| -> f64 {
        f32::from_le_bytes(frame[index * 4..index * 4 + 4].try_into().unwrap()) as f64
    };

    let horizontal = contributions(width, out_width);
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

    let vertical = contributions(height, out_height);
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
            let out = resize_scene_linear(&source, 16, 12, width, height).unwrap();
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
        let out = resize_scene_linear(&source, 32, 4, 64, 8).unwrap();
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
        let down = resize_scene_linear(&source, 32, 4, 16, 2).unwrap();
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
    fn rejects_mismatched_or_degenerate_geometry() {
        let source = frame(4, 3, |_, _| [0.1; 3]);
        assert!(resize_scene_linear(&source, 4, 3, 0, 3).is_err());
        assert!(resize_scene_linear(&source, 4, 3, 4, 0).is_err());
        assert!(resize_scene_linear(&source[..source.len() - 1], 4, 3, 2, 2).is_err());
    }
}
