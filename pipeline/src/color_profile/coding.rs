//! Signal ranges, YCbCr coding, quantizers and writable conversion backing.

/// Preview output range: the real signal range written to the encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewRange {
    /// Limited/video range (16..235 luma in 8 bits), existing sRGB delivery.
    Tv,
    /// Full/PC range (0..255), used by the camera Log profiles.
    Pc,
}

/// Depth of the legacy 4:4:4 YCbCr handoff. Export profiles specify their own
/// pipe format independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalDepth {
    Ten,
    Twelve,
}

impl SignalDepth {
    pub fn token(self) -> &'static str {
        match self {
            Self::Ten => "10",
            Self::Twelve => "12",
        }
    }
    pub fn bits(self) -> u32 {
        match self {
            Self::Ten => 10,
            Self::Twelve => 12,
        }
    }
    /// Largest representable code (the full-range peak).
    pub fn max(self) -> u16 {
        match self {
            Self::Ten => 1023,
            Self::Twelve => 4095,
        }
    }
    /// The planar 4:4:4 little-endian rawvideo pixel format FFmpeg receives.
    /// Every sample is one u16 word with the code in the low bits.
    pub fn pix_fmt(self) -> &'static str {
        match self {
            Self::Ten => "yuv444p10le",
            Self::Twelve => "yuv444p12le",
        }
    }
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "10" => Ok(Self::Ten),
            "12" => Ok(Self::Twelve),
            _ => Err(format!("invalid --signal-bits {value:?}: expected 10 or 12")),
        }
    }
}

impl PreviewRange {
    pub fn as_str(self) -> &'static str {
        match self {
            PreviewRange::Tv => "tv",
            PreviewRange::Pc => "pc",
        }
    }

}

/// YCbCr coding matrix applied to the already-transferred target signal.
/// Coefficients are the published H.273/H.274 values for the tag written with
/// the output; coding is independent of the vendor gamut and transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YcbcrCoding {
    /// ITU-R BT.709 / Rec.709 non-constant luminance.
    Bt709,
    /// ITU-R BT.2020 non-constant luminance.
    Bt2020Ncl,
}

impl YcbcrCoding {
    pub fn from_matrix(token: &str) -> Option<Self> {
        match token {
            "bt709" => Some(YcbcrCoding::Bt709),
            "bt2020nc" => Some(YcbcrCoding::Bt2020Ncl),
            _ => None,
        }
    }

    /// `(Kr, Kg, Kb, 2(1-Kb), 2(1-Kr))` for non-constant-luminance coding.
    pub fn coefficients(self) -> (f64, f64, f64, f64, f64) {
        match self {
            YcbcrCoding::Bt709 => (0.2126, 0.7152, 0.0722, 1.8556, 1.5748),
            YcbcrCoding::Bt2020Ncl => (0.2627, 0.6780, 0.0593, 1.8814, 1.4746),
        }
    }

    pub fn formula(self) -> &'static str {
        match self {
            YcbcrCoding::Bt709 => {
                "Y'=0.2126*R'+0.7152*G'+0.0722*B'; Cb=(B'-Y')/1.8556; Cr=(R'-Y')/1.5748"
            }
            YcbcrCoding::Bt2020Ncl => {
                "Y'=0.2627*R'+0.6780*G'+0.0593*B'; Cb=(B'-Y')/1.8814; Cr=(R'-Y')/1.4746"
            }
        }
    }
}

/// Luma code for a normalized target signal. Limited range follows H.273
/// (10-bit `64 + 876*signal`, 12-bit `256 + 3504*signal`); full range is
/// `max*signal`.
pub fn quantize_luma(signal: f64, range: PreviewRange, depth: SignalDepth) -> u16 {
    YcbcrQuantizer::new(range, depth.bits()).luma(signal)
}

/// Chroma code for a non-constant-luminance Cb/Cr value in [-0.5, 0.5].
/// Limited range follows H.273 (10-bit `512 + 896*value` i.e. 64..960;
/// 12-bit `2048 + 3584*value` i.e. 256..3840); full range centres on the
/// depth midpoint.
pub fn quantize_chroma(value: f64, range: PreviewRange, depth: SignalDepth) -> u16 {
    YcbcrQuantizer::new(range, depth.bits()).chroma(value)
}

pub(crate) struct YcbcrQuantizer {
    black: f64,
    luma_extent: f64,
    neutral: f64,
    chroma_extent: f64,
    max: f64,
}

impl YcbcrQuantizer {
    pub(crate) fn new(range: PreviewRange, bits: u32) -> Self {
        let max = f64::from((1u32 << bits) - 1);
        let scale = f64::from(1u32 << (bits - 8));
        let (black, luma_extent, neutral, chroma_extent) = match range {
            PreviewRange::Tv => (16.0 * scale, 219.0 * scale, 128.0 * scale, 224.0 * scale),
            PreviewRange::Pc => (0.0, max, max / 2.0, max),
        };
        Self { black, luma_extent, neutral, chroma_extent, max }
    }

    pub(crate) fn luma(&self, value: f64) -> u16 {
        (self.black + self.luma_extent * value.clamp(0.0, 1.0))
            .round().clamp(0.0, self.max) as u16
    }

    pub(crate) fn chroma(&self, value: f64) -> u16 {
        // Ignore matrix residuals around neutral, especially where full
        // range's exact center is half way between two codes.
        let value = if value.abs() < 1e-7 { 0.0 } else { value.clamp(-0.5, 0.5) };
        (self.neutral + self.chroma_extent * value)
            .round().clamp(0.0, self.max) as u16
    }
}

/// Accurate per-field H.273 code points; 2 means unspecified, not a fallback
/// to the primaries or transfer of the selected YCbCr coding matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TagPolicy {
    pub primaries: u16,
    pub transfer: u16,
    pub matrix: u16,
}

impl TagPolicy {
    pub const SRGB: Self = Self {
        primaries: 1,
        transfer: 13,
        matrix: 1,
    };
    pub const VENDOR_LOG: Self = Self {
        primaries: 2,
        transfer: 2,
        matrix: 9,
    };
    pub const NIKON_LOG: Self = Self {
        primaries: 9,
        transfer: 2,
        matrix: 9,
    };

    pub fn primaries_token(self) -> &'static str {
        match self.primaries {
            1 => "bt709",
            9 => "bt2020",
            _ => "unknown",
        }
    }
    pub fn transfer_token(self) -> &'static str {
        match self.transfer {
            13 => "iec61966-2-1",
            _ => "unknown",
        }
    }
    pub fn matrix_token(self) -> &'static str {
        match self.matrix {
            1 => "bt709",
            9 => "bt2020nc",
            _ => "unknown",
        }
    }
}

/// One official reference code point published with a transfer curve.
#[derive(Debug, Clone, Copy)]
pub struct ReferenceCode {
    /// Linear reflection/scene value at which the vendor publishes the code.
    pub linear: f64,
    pub label: &'static str,
    /// Official 10-bit code, when the vendor publishes a code value.
    pub code10: u16,
}

impl ReferenceCode {
    pub(crate) const fn new(linear: f64, label: &'static str, code10: u16) -> Self {
        Self {
            linear,
            label,
            code10,
        }
    }

    /// Expected 8-bit code under the recorded conversion convention
    /// `round(code10 * 255 / 1023)`.
    pub fn code8(self) -> u16 {
        (self.code10 as f64 * 255.0 / 1023.0).round() as u16
    }
}


/// Writable backing for conversion. The pipeline supplies the unique Arc that
/// becomes its RGB24 NdArray, so conversion does not copy a full-frame buffer.
/// Test-support surface: backs the reference conversions below, not the CLI.
pub trait FrameOutput {
    fn prepare(&mut self, length: usize) -> Result<&mut [u8], String>;
}
impl FrameOutput for Vec<u8> {
    fn prepare(&mut self, length: usize) -> Result<&mut [u8], String> {
        self.resize(length, 0);
        Ok(self.as_mut_slice())
    }
}
impl FrameOutput for std::sync::Arc<[u8]> {
    fn prepare(&mut self, length: usize) -> Result<&mut [u8], String> {
        if self.len() != length {
            return Err("RGB24 output geometry mismatch".into());
        }
        std::sync::Arc::get_mut(self)
            .ok_or_else(|| "RGB24 output backing must be uniquely owned".into())
    }
}

/// Resolved `--log-reference-gain auto` decision: the applied gain plus the
/// bounds it was derived from and the binding constraint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoReferenceGain {
    pub gain: f64,
    pub desired: f64,
    pub safe_positive: f64,
    pub safe_negative: f64,
    pub limited_by: &'static str,
}


/// 8-bit quantization: clamp to [0,1], scale by 255 and round half away from
/// zero. The value is clamped again after the transfer so a numerically
/// out-of-range curve evaluation cannot wrap.
pub fn quantize_signal(signal: f64) -> u8 {
    (signal.clamp(0.0, 1.0) * 255.0).round().clamp(0.0, 255.0) as u8
}
