//! Per-pixel conversion from scene-linear AP1 into the profile signal coding.

use crate::preview;
use super::coding::{quantize_chroma, quantize_luma, quantize_signal, FrameOutput, PreviewRange, SignalDepth, YcbcrCoding};
use super::output_cat::OutputTransform;
use super::ColorProfile;

impl ColorProfile {
    /// Reference 8-bit quantization of signed FP32 AP1 input. Retained for the
    /// historical golden/reference tests; the product encoder node uses
    /// [`Self::convert_f32_frame_to_yuv`] exclusively, so there is one
    /// runtime quantization boundary. Test-support surface, consumed by
    /// `tests/canon_log3.rs`; not reachable from the CLI.
    pub fn convert_f32_frame(
        &self,
        bytes: &[u8],
        width: usize,
        height: usize,
        reference_gain: f64,
        out: &mut impl FrameOutput,
    ) -> Result<[usize; 2], String> {
        if !reference_gain.is_finite()
            || reference_gain <= 0.0
        {
            return Err("invalid FP32 profile/reference gain".into());
        }
        let n = width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(3))
            .ok_or("FP32 geometry overflow")?;
        if n.checked_mul(4) != Some(bytes.len()) {
            return Err("FP32 length mismatch".into());
        }
        let output = out.prepare(n)?;
        let m = self.composed_matrix();
        let (low, high) = self.transfer.domain();
        let mut clipped = [0usize; 2];
        for (src, dst) in bytes.chunks_exact(12).zip(output.chunks_exact_mut(3)) {
            let mut ap1 = [0.0; 3];
            for c in 0..3 {
                ap1[c] = f32::from_le_bytes(src[c * 4..c * 4 + 4].try_into().unwrap()) as f64;
                if !ap1[c].is_finite() {
                    return Err("nonfinite FP32 sample".into());
                }
            }
            let target = preview::mat3_vec(&m, ap1).map(|v| v * reference_gain * self.curve_input_scale());
            if target.iter().any(|v| !v.is_finite()) {
                return Err("nonfinite target color".into());
            }
            for v in target {
                clipped[0] += usize::from(v < low);
                clipped[1] += usize::from(v > high);
            }
            dst.copy_from_slice(&self.encode_target_linear(target));
        }
        Ok(clipped)
    }

    /// The profile's YCbCr coding matrix. This is the delivery transform
    /// declared by the same `ycbcr_matrix` token written into the container
    /// tags; it never changes the vendor primaries or transfer curve.
    pub fn ycbcr_coding(&self) -> Result<YcbcrCoding, String> {
        YcbcrCoding::from_matrix(self.ycbcr_matrix).ok_or_else(|| {
            format!(
                "profile {} declares unregistered YCbCr matrix {:?}",
                self.id, self.ycbcr_matrix
            )
        })
    }

    /// Signed FP32 AP1/D60 (native EV already applied) to the single encoder
    /// handoff: planar `yuv444p10le` target signal (Y, Cb, Cr planes).
    ///
    /// This is the one quantization boundary for every preset. The transfer
    /// curve, gamut matrix, reference scale and range convention all come from
    /// this profile; the resolved `output` transform supplies the frozen CAT and
    /// gamut composition chosen for the run. The quantizer below is the only
    /// place 10-bit codes appear.
    pub fn convert_f32_frame_to_yuv(
        &self,
        output: &OutputTransform,
        bytes: &[u8],
        width: usize,
        height: usize,
        reference_gain: f64,
        range: PreviewRange,
        depth: SignalDepth,
        out: &mut Vec<u16>,
    ) -> Result<[usize; 2], String> {
        if output.profile != self.id {
            return Err(format!(
                "output transform profile {:?} does not match {:?}",
                output.profile, self.id
            ));
        }
        if !reference_gain.is_finite()
            || reference_gain <= 0.0
        {
            return Err("invalid FP32 profile/reference gain".into());
        }
        let pixels = width
            .checked_mul(height)
            .ok_or("FP32 geometry overflow")?;
        let samples = pixels.checked_mul(3).ok_or("FP32 geometry overflow")?;
        if samples.checked_mul(4) != Some(bytes.len()) {
            return Err("FP32 length mismatch".into());
        }
        let coding = self.ycbcr_coding()?;
        let (kr, kg, kb, div_cb, div_cr) = coding.coefficients();
        let matrix = output.composed_matrix;
        let (low, high) = self.transfer.domain();
        let scale = reference_gain * self.curve_input_scale();
        out.resize(samples, 0);
        let mut clipped = [0usize; 2];
        for (index, src) in bytes.chunks_exact(12).enumerate() {
            let mut ap1 = [0.0; 3];
            for channel in 0..3 {
                ap1[channel] =
                    f32::from_le_bytes(src[channel * 4..channel * 4 + 4].try_into().unwrap())
                        as f64;
                if !ap1[channel].is_finite() {
                    return Err("nonfinite FP32 sample".into());
                }
            }
            let target =
                preview::mat3_vec(&matrix, ap1).map(|value| value * scale);
            if target.iter().any(|value| !value.is_finite()) {
                return Err("nonfinite target color".into());
            }
            for value in target {
                clipped[0] += usize::from(value < low);
                clipped[1] += usize::from(value > high);
            }
            let signal = [
                self.transfer.forward(target[0].clamp(low, high)),
                self.transfer.forward(target[1].clamp(low, high)),
                self.transfer.forward(target[2].clamp(low, high)),
            ];
            if signal.iter().any(|value| !value.is_finite()) {
                return Err("nonfinite target signal".into());
            }
            let luma = kr * signal[0] + kg * signal[1] + kb * signal[2];
            let cb = (signal[2] - luma) / div_cb;
            let cr = (signal[0] - luma) / div_cr;
            out[index] = quantize_luma(luma, range, depth);
            out[pixels + index] = quantize_chroma(cb, range, depth);
            out[2 * pixels + index] = quantize_chroma(cr, range, depth);
        }
        Ok(clipped)
    }

    /// Quantize one already-encoded target signal triple into the 10-bit
    /// handoff. Test-support surface: exposed so reference-code tests do not
    /// re-implement the production quantizer; not reachable from the CLI.
    pub fn encode_signal_yuv(
        &self,
        signal: [f64; 3],
        range: PreviewRange,
        depth: SignalDepth,
    ) -> Result<[u16; 3], String> {
        let coding = self.ycbcr_coding()?;
        let (kr, kg, kb, div_cb, div_cr) = coding.coefficients();
        let luma = kr * signal[0] + kg * signal[1] + kb * signal[2];
        Ok([
            quantize_luma(luma, range, depth),
            quantize_chroma((signal[2] - luma) / div_cb, range, depth),
            quantize_chroma((signal[0] - luma) / div_cr, range, depth),
        ])
    }

    /// Composed AP1/D60 scene-linear -> target linear matrix for the profile's
    /// default reference realization. The order of the matrix products matches
    /// `preview::ap1_to_linear_srgb_matrix` exactly so the sRGB profile stays
    /// bit-identical to the pre-existing path. Production conversion uses the
    /// run's resolved [`OutputTransform`], not this convenience accessor.
    pub fn composed_matrix(&self) -> [[f64; 3]; 3] {
        self.default_output_transform().composed_matrix
    }

    pub fn scale_filter_token(&self) -> &'static str {
        match self.ycbcr_matrix {
            "bt709" => "bt709",
            "bt2020nc" => "bt2020",
            other => other,
        }
    }

    /// Convert one scene-linear ACEScg AP1/D60 triple to 8-bit target signal.
    /// Test-support surface for the reference tests; the production pixel path
    /// is [`Self::convert_f32_frame_to_yuv`].
    pub fn encode_linear(&self, ap1: [f64; 3]) -> [u8; 3] {
        let target = preview::mat3_vec(&self.composed_matrix(), ap1)
            .map(|v| v * self.curve_input_scale());
        self.encode_target_linear(target)
    }

    /// Convert one linear *target-gamut* triple to 8-bit signal. Inputs outside
    /// the official inverse domain clip explicitly to its endpoints.
    pub fn encode_target_linear(&self, target_linear: [f64; 3]) -> [u8; 3] {
        let (low, high) = self.transfer.domain();
        let mut out = [0u8; 3];
        for channel in 0..3 {
            let value = target_linear[channel].clamp(low, high);
            out[channel] = quantize_signal(self.transfer.forward(value));
        }
        out
    }

    /// Compatibility utility for explicit RGB16 ACEScg inputs, retained for
    /// the historical golden/reference tests only. The production RAW pipeline
    /// uses convert_f32_frame_to_yuv for every profile; neither this nor
    /// convert_f32_frame is reachable from the CLI.
    pub fn convert_frame(
        &self,
        rgb48le: &[u8],
        width: usize,
        height: usize,
        out: &mut impl FrameOutput,
    ) -> Result<(), String> {
        if self.curve_input_scale() != 1.0 {
            return Err("this curve input scale requires signed FP32 input".into());
        }

        let expected = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(6))
            .ok_or_else(|| "frame size overflow".to_string())?;
        if rgb48le.len() != expected {
            return Err(format!(
                "frame byte length {} does not match {}x{} RGB16 ({} bytes)",
                rgb48le.len(),
                width,
                height,
                expected
            ));
        }
        let matrix = self.composed_matrix();
        let (low, high) = self.transfer.domain();
        let out = out.prepare(expected / 2)?;
        for (pixel, dest) in rgb48le.chunks_exact(6).zip(out.chunks_exact_mut(3)) {
            let scene = [
                u16::from_le_bytes([pixel[0], pixel[1]]) as f64 / preview::SCENE_SCALE,
                u16::from_le_bytes([pixel[2], pixel[3]]) as f64 / preview::SCENE_SCALE,
                u16::from_le_bytes([pixel[4], pixel[5]]) as f64 / preview::SCENE_SCALE,
            ];
            let target = preview::mat3_vec(&matrix, scene);
            for (value, dest) in target.into_iter().zip(dest) {
                let signal = self.transfer.forward(value.clamp(low, high));
                *dest = quantize_signal(signal);
            }
        }
        Ok(())
    }
}
