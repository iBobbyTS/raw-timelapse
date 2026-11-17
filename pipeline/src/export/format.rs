//! Rawvideo byte layouts accepted by the export pipe.

use crate::color_profile::{PreviewRange, YcbcrCoding, YcbcrQuantizer};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputFormat {
    Yuv420p,
    Yuv420p10,
    Yuv420p12,
    Yuv422p,
    Yuv422p10,
    Yuv422p12,
    Yuv444p,
    Yuv444p10,
    Yuv444p12,
    Gbrp,
    Gbrp10,
    Gbrp12,
    Gbrp16,
}

impl InputFormat {
    pub const ALL: [Self; 13] = [
        Self::Yuv420p,
        Self::Yuv420p10,
        Self::Yuv420p12,
        Self::Yuv422p,
        Self::Yuv422p10,
        Self::Yuv422p12,
        Self::Yuv444p,
        Self::Yuv444p10,
        Self::Yuv444p12,
        Self::Gbrp,
        Self::Gbrp10,
        Self::Gbrp12,
        Self::Gbrp16,
    ];

    pub fn parse(format: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.name() == format)
            .ok_or_else(|| {
                format!(
                    "unsupported export input format {format:?}; expected one of {}",
                    Self::ALL.map(Self::name).join(", ")
                )
            })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Yuv420p => "yuv420p",
            Self::Yuv420p10 => "yuv420p10le",
            Self::Yuv420p12 => "yuv420p12le",
            Self::Yuv422p => "yuv422p",
            Self::Yuv422p10 => "yuv422p10le",
            Self::Yuv422p12 => "yuv422p12le",
            Self::Yuv444p => "yuv444p",
            Self::Yuv444p10 => "yuv444p10le",
            Self::Yuv444p12 => "yuv444p12le",
            Self::Gbrp => "gbrp",
            Self::Gbrp10 => "gbrp10le",
            Self::Gbrp12 => "gbrp12le",
            Self::Gbrp16 => "gbrp16le",
        }
    }

    pub fn bits(self) -> u32 {
        match self {
            Self::Yuv420p | Self::Yuv422p | Self::Yuv444p | Self::Gbrp => 8,
            Self::Yuv420p10 | Self::Yuv422p10 | Self::Yuv444p10 | Self::Gbrp10 => 10,
            Self::Yuv420p12 | Self::Yuv422p12 | Self::Yuv444p12 | Self::Gbrp12 => 12,
            Self::Gbrp16 => 16,
        }
    }

    pub fn is_rgb(self) -> bool {
        matches!(
            self,
            Self::Gbrp | Self::Gbrp10 | Self::Gbrp12 | Self::Gbrp16
        )
    }

    pub(crate) fn is_subsampled(self) -> bool {
        self.chroma_block() != (1, 1)
    }

    fn chroma_block(self) -> (usize, usize) {
        match self {
            Self::Yuv420p | Self::Yuv420p10 | Self::Yuv420p12 => (2, 2),
            Self::Yuv422p | Self::Yuv422p10 | Self::Yuv422p12 => (2, 1),
            _ => (1, 1),
        }
    }

    pub fn validate_geometry(self, width: u32, height: u32) -> Result<(), String> {
        let (columns, rows) = self.chroma_block();
        if width == 0 || height == 0 || width as usize % columns != 0 || height as usize % rows != 0
        {
            return Err(format!("export input {} cannot represent {width}x{height}; dimensions must be positive multiples of {columns}x{rows}", self.name()));
        }
        Ok(())
    }

    /// Pack target nonlinear RGB (after the selected gamut and transfer),
    /// quantizing at the selected depth. The RGB pipe is always full range.
    /// Chroma is averaged over the output pixel footprint before quantization.
    pub fn pack(
        self,
        signal: &[[f32; 3]],
        width: u32,
        height: u32,
        range: PreviewRange,
        coding: YcbcrCoding,
    ) -> Result<Vec<u8>, String> {
        self.validate_geometry(width, height)?;
        let (width, height) = (width as usize, height as usize);
        let pixels = width
            .checked_mul(height)
            .ok_or("export frame geometry overflow")?;
        if signal.len() != pixels {
            return Err(format!(
                "export received {} RGB pixels, expected {pixels}",
                signal.len()
            ));
        }
        let (columns, rows) = self.chroma_block();
        let chroma = pixels / (columns * rows);
        let sample_bytes = if self.bits() == 8 { 1 } else { 2 };
        let length = chroma
            .checked_mul(2)
            .and_then(|n| n.checked_add(pixels))
            .and_then(|n| n.checked_mul(sample_bytes))
            .ok_or("export frame size overflow")?;
        let mut out = vec![0; length];
        let mut write = |index: usize, value: u16| {
            let bytes = value.to_le_bytes();
            let offset = index * sample_bytes;
            out[offset..offset + sample_bytes].copy_from_slice(&bytes[..sample_bytes]);
        };
        if self.is_rgb() {
            let max = f64::from((1u32 << self.bits()) - 1);
            let quantize = |value: f32| (f64::from(value.clamp(0.0, 1.0)) * max).round() as u16;
            for (index, [r, g, b]) in signal.iter().copied().enumerate() {
                write(index, quantize(g));
                write(pixels + index, quantize(b));
                write(2 * pixels + index, quantize(r));
            }
            return Ok(out);
        }

        let (kr, kg, kb, cb_scale, cr_scale) = coding.coefficients();
        let quantizer = YcbcrQuantizer::new(range, self.bits());
        let block_size = (columns * rows) as f64;
        for y in (0..height).step_by(rows) {
            for x in (0..width).step_by(columns) {
                let mut cb = 0.0;
                let mut cr = 0.0;
                for dy in 0..rows {
                    for dx in 0..columns {
                        let index = (y + dy) * width + x + dx;
                        let [r, g, b] = signal[index].map(f64::from);
                        let luma = kr * r + kg * g + kb * b;
                        write(index, quantizer.luma(luma));
                        cb += (b - luma) / cb_scale;
                        cr += (r - luma) / cr_scale;
                    }
                }
                let chroma_index = (y / rows) * (width / columns) + x / columns;
                write(pixels + chroma_index, quantizer.chroma(cb / block_size));
                write(
                    pixels + chroma + chroma_index,
                    quantizer.chroma(cr / block_size),
                );
            }
        }
        Ok(out)
    }
}
