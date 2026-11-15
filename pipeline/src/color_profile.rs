//! Preview output colour/Log profile registry and CPU reference conversion.
//!
//! The pipeline always feeds scene-linear ACEScg (AP1 primaries, D60 white) to
//! this module. Each registered profile fixes, independently of the encoder:
//!
//! * one published or independently recovered curve, with constants and source;
//! * one target colour gamut defined by official primaries plus white point,
//!   converted through the staged path
//!   `AP1/D60 -> XYZ(D60) -> output CAT -> target linear RGB`, where the
//!   adaptation targets the gamut's own white point;
//! * one pinned default output CAT realization. The pre-existing profiles keep
//!   the published Bradford D60->D65 matrix and the historic composition order
//!   bit-for-bit; a profile may pin a different fixed chain (D-Log 2 pins its
//!   DCTL AP1->AP0->D-Gamut 2/CAT02 chain). An
//!   explicit CLI CAT choice that differs from the profile default uses the
//!   standard derivation and is reported as `custom-adaptation`;
//! * an explicit input domain derived from the official inverse curve at signal
//!   0 and 1 (no tone mapping: negatives below the domain clip to the official
//!   lower endpoint and highlights above it clip to the upper endpoint);
//! * one 10-bit planar 4:4:4 YCbCr handoff for the encoder node, quantized with
//!   round-half-away-from-zero and no dither (the single quantization boundary);
//! * the delivery range, YCbCr coding matrix and container/bitstream tag policy.
//!
//! The constants below are transcriptions, not values recalled from memory.
//! Citations are attached to every transfer and gamut and are also written into
//! the delivered colour contract. The existing IEC 61966-2-1 sRGB path is preserved exactly:
//! the `srgb-preview` profile composes the same matrices in the same order as
//! [`crate::preview::ap1_to_linear_srgb_matrix`].

use crate::chromatic_adaptation::{self as cat, D65_WHITE_XY};
use crate::json::Json;
use crate::preview;

/// Re-exported static CAT registration surface (one path for CLI and tests).
pub use crate::chromatic_adaptation::{CatAlgorithm, CatRegistry};

/// Preview output range: the real signal range written to the encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewRange {
    /// Limited/video range (16..235 luma in 8 bits), existing sRGB delivery.
    Tv,
    /// Full/PC range (0..255), used by the camera Log profiles.
    Pc,
}

/// The encoder input handoff is a single 10-bit 4:4:4 YCbCr quantization. Both
/// ProRes and lower-bit-depth codecs start from this same buffer; FFmpeg only
/// subsamples/reduces it, never re-develops the RAW.
pub const SIGNAL_BITS: u32 = 10;
pub const SIGNAL_MAX: u16 = 1023;

impl PreviewRange {
    pub fn as_str(self) -> &'static str {
        match self {
            PreviewRange::Tv => "tv",
            PreviewRange::Pc => "pc",
        }
    }

    /// `ffmpeg` `scale`/`setparams` token.
    pub fn filter_token(self) -> &'static str {
        self.as_str()
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

/// 10-bit luma code for a normalized target signal. Limited range is
/// `64 + 876*signal` (H.273 10-bit video range); full range is `1023*signal`.
pub fn quantize_luma(signal: f64, range: PreviewRange) -> u16 {
    let value = signal.clamp(0.0, 1.0);
    let code = match range {
        PreviewRange::Tv => 64.0 + 876.0 * value,
        PreviewRange::Pc => 1023.0 * value,
    };
    code.round().clamp(0.0, f64::from(SIGNAL_MAX)) as u16
}

/// 10-bit chroma code for a non-constant-luminance Cb/Cr value in [-0.5, 0.5].
/// Limited range is `512 + 896*value` (64..960); full range centres on the
/// 10-bit midpoint.
pub fn quantize_chroma(value: f64, range: PreviewRange) -> u16 {
    // Snap numerical noise on the neutral axis so a mathematically neutral
    // pixel always codes as 512, not 511/512 depending on the sign of a
    // sub-code matrix residual. 1e-7 is four orders below one 10-bit code.
    let value = if value.abs() < 1e-7 {
        0.0
    } else {
        value.clamp(-0.5, 0.5)
    };
    let code = match range {
        PreviewRange::Tv => 512.0 + 896.0 * value,
        PreviewRange::Pc => 511.5 + 1023.0 * value,
    };
    code.round().clamp(0.0, f64::from(SIGNAL_MAX)) as u16
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
    const fn new(linear: f64, label: &'static str, code10: u16) -> Self {
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

/// Published and independently recovered transfer curves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    /// IEC 61966-2-1 sRGB OETF (existing display-referred path).
    Srgb,
    /// Sony S-Log3.
    SonySLog3,
    /// Canon Log 2.
    CanonLog2,
    CanonLog2RevEng,
    /// Recovered CRD three-segment Canon Log 3.
    CanonLog3RevEng,
    /// Nikon N-Log.
    NikonNLog,
    /// DJI D-Log (original D-Log, not D-Log 2).
    DjiDLog,
    /// DJI D-Log 2 (fixed three-segment DCTL definition).
    DjiDLog2,
}

// Pinned D-Log 2 constants, transcribed from the fixed DCTL
// (`specs/DLog2_DGamut2_to_ACES_AP0_cat02.dctl`, dwg-transforms 5614391).
// They are the curve definition, not values recovered from an encoder LUT.
const DJI_DLOG2_H: f64 = 475.0;
const DJI_DLOG2_A: f64 = 16.285770761945304;
const DJI_DLOG2_K1: f64 = 0.059439938321493;
const DJI_DLOG2_B1: f64 = 0.304985337243402;
const DJI_DLOG2_K2: f64 = 2.960935245492250;
const DJI_DLOG2_B2: f64 = 0.148314799066323;
const DJI_DLOG2_T: f64 = 0.028961695254132;
/// `0.18` is the published middle-grey pivot of both inner segments.
const DJI_DLOG2_MID: f64 = 0.18;
/// The oracle's rounded f64 of the exact `2^A` for the pinned `A` (the platform
/// `exp2` may differ by a few ulp, far inside the 1e-12 curve budget). The
/// per-sample path therefore needs no `exp2` for a constant.
const DJI_DLOG2_TWO_POW_A: f64 = 79892.40627605369;
const DJI_DLOG2_TWO_POW_A_MINUS_1: f64 = DJI_DLOG2_TWO_POW_A - 1.0;
/// `H / (2^A - 1)`, the published highlight-segment scale.
const DJI_DLOG2_UPPER_SCALE: f64 = DJI_DLOG2_H / DJI_DLOG2_TWO_POW_A_MINUS_1;

impl Transfer {
    pub fn name(self) -> &'static str {
        match self {
            Transfer::CanonLog2RevEng => "Canon Log 2 (rev-eng crm-dev)",
            Transfer::CanonLog3RevEng => "Canon Log 3 (rev-eng crm-dev)",
            Transfer::Srgb => "IEC 61966-2-1 sRGB OETF",
            Transfer::SonySLog3 => "Sony S-Log3",
            Transfer::CanonLog2 => "Canon Log 2",
            Transfer::NikonNLog => "Nikon N-Log",
            Transfer::DjiDLog => "DJI D-Log",
            Transfer::DjiDLog2 => "DJI D-Log 2",
        }
    }

    pub fn formula(self) -> &'static str {
        match self {
            Transfer::CanonLog3RevEng => "x=reflection/0.9; if x < -0.014: L=-0.42889912*ln(1-14.98325*x)/ln(10)+0.07623209; else if x < 0.014: L=2.3069815*x+0.073059361; else: L=0.42889912*ln(1+14.98325*x)/ln(10)+0.069886632; y=(L*56064+4096)/65472",
            Transfer::CanonLog2RevEng => "x=reflection/0.9; L=sign(x)*0.122411586*ln(1+87.09937546*abs(x))+0.035388128; y=(L*56064+4096)/65472",
            Transfer::Srgb => "if x <= 0.0031308: y = 12.92*x; else y = 1.055*x^(1/2.4) - 0.055",
            Transfer::SonySLog3 => {
                "if x >= 0.01125: y = (420 + log10((x+0.01)/(0.18+0.01))*261.5)/1023; \
                 else y = (x*(171.2102946929-95)/0.01125 + 95)/1023"
            }
            Transfer::CanonLog2 => {
                "x = reflection/0.9; if x < 0: y = -0.24136077*log10(1-87.099375*x)+0.092864125; \
                 else y = 0.24136077*log10(87.099375*x+1)+0.092864125"
            }
            Transfer::NikonNLog => {
                "if y < 0.328: code = 650*(y+0.0075)^(1/3); else code = 150*ln(y)+619; \
                 signal = code/1023"
            }
            Transfer::DjiDLog => {
                "if x <= 0.0078: y = 6.025*x + 0.0929; \
                 else y = log10(x*0.9892+0.0108)*0.256663 + 0.584555"
            }
            Transfer::DjiDLog2 => {
                "H=475, A=16.285770761945304, K1=0.059439938321493, B1=0.304985337243402, \
                 K2=2.960935245492250, B2=0.148314799066323, T=0.028961695254132; \
                 if x < T: y = B2 + K2*(x-T); else if x < 0.18: y = B1 + K1*log2(x/0.18); \
                 else: y = log2(1 + x*(2^A-1)/H)/A"
            }
        }
    }

    pub fn document(self) -> &'static str {
        match self {
            Transfer::CanonLog3RevEng => {
                "Cinema RAW Development reconstructed continuous Log3 (camera-profile calibrated CR3 adaptation)"
            }
            Transfer::CanonLog2RevEng => {
                "Cinema RAW Development reconstructed continuous Log2 (camera-profile calibrated CR3 adaptation)"
            }
            Transfer::Srgb => "IEC 61966-2-1:1999",
            Transfer::SonySLog3 => "Technical Summary for S-Gamut3.Cine/S-Log3 and S-Gamut3/S-Log3",
            Transfer::CanonLog2 => {
                "Description of the Canon Log, Canon Log 2 and Canon Log 3 Gamma Curves"
            }
            Transfer::NikonNLog => "N-Log Specification Document",
            Transfer::DjiDLog => "D-Log and D-Gamut White Paper",
            Transfer::DjiDLog2 => {
                "D-Log 2 / D-Gamut 2 to ACES AP0 (CAT02) fixed DCTL transform spec"
            }
        }
    }

    pub fn version(self) -> &'static str {
        match self {
            Transfer::CanonLog2RevEng | Transfer::CanonLog3RevEng => {
                "CRD 2.15.10.1 / CrmSdk 2.11.0.27; rawtl revision 1"
            }
            Transfer::Srgb => "1999",
            Transfer::SonySLog3 => "V1.0 (2013-12, Japanese edition)",
            Transfer::CanonLog2 => "2018-11-01",
            Transfer::NikonNLog => "1.0.0 (2018-09-01)",
            Transfer::DjiDLog => "2017-10-10 (Zenmuse X7)",
            Transfer::DjiDLog2 => {
                "dwg-transforms 56143916e275c617471f7845b5c6b57795d0ee28, DCTL SHA256 \
                 c0f4d7e7854fcb86df4d330d9ddb1e84fc9c22378cd49c61033f3ca0db4f47e5"
            }
        }
    }

    pub fn url(self) -> &'static str {
        match self {
            Transfer::CanonLog2RevEng => "docs/canon-log2-rev-eng.md",
            Transfer::CanonLog3RevEng => "docs/canon-log3-rev-eng.md",
            Transfer::Srgb => "https://webstore.iec.ch/publication/6169",
            Transfer::SonySLog3 => {
                "https://www.sony.jp/ls-camera/knowledge/pdf/\
                 TechnicalSummary_for-S-Gamut3Cine_S-Gamut3_S-Log3_V1_00.pdf"
            }
            Transfer::CanonLog2 => {
                "https://downloads.canon.com/nw/learn/white-papers/cinema-eos/\
                 white-paper-canon-log-gamma-curves.pdf"
            }
            Transfer::NikonNLog => {
                "http://download.nikonimglib.com/archive3/hDCmK00m9JDI03RPruD74xpoU905/\
                 N-Log_Specification_(En)01.pdf"
            }
            Transfer::DjiDLog => {
                "https://dl.djicdn.com/downloads/zenmuse+x7/20171010/\
                 D-Log_D-Gamut_Whitepaper.pdf"
            }
            Transfer::DjiDLog2 => {
                "https://github.com/thatcherfreeman/dwg-transforms/blob/\
                 56143916e275c617471f7845b5c6b57795d0ee28/specs/\
                 DLog2_DGamut2_to_ACES_AP0_cat02.dctl"
            }
        }
    }

    /// Forward transfer: linear target-gamut reflection -> normalized signal.
    pub fn forward(self, value: f64) -> f64 {
        match self {
            Transfer::CanonLog3RevEng => {
                let x = value / 0.9;
                // Preserve the recovered SDK's separate multiply/divide/add
                // operations and rounded constants (no FMA, smoothing or LUT).
                let l = if x < -0.014 {
                    (1.0 - 14.98325 * x).ln() * -0.42889912 / std::f64::consts::LN_10 + 0.07623209
                } else if x < 0.014 {
                    x * 2.3069815 + 0.073059361
                } else {
                    (1.0 + 14.98325 * x).ln() * 0.42889912 / std::f64::consts::LN_10 + 0.069886632
                };
                (l * 56064.0 + 4096.0) / 65472.0
            }
            Transfer::CanonLog2RevEng => {
                let x = value / 0.9;
                let l = x.signum() * 0.122411586 * (87.09937546 * x.abs()).ln_1p() + 0.035388128;
                (l * 56064.0 + 4096.0) / 65472.0
            }
            Transfer::Srgb => preview::srgb_oetf(value),
            Transfer::SonySLog3 => {
                if value >= 0.01125 {
                    (420.0 + ((value + 0.01) / (0.18 + 0.01)).log10() * 261.5) / 1023.0
                } else {
                    (value * (171.2102946929 - 95.0) / 0.01125 + 95.0) / 1023.0
                }
            }
            Transfer::CanonLog2 => {
                let x = value / 0.9;
                if x < 0.0 {
                    -0.24136077 * (1.0 - 87.099375 * x).log10() + 0.092864125
                } else {
                    0.24136077 * (87.099375 * x + 1.0).log10() + 0.092864125
                }
            }
            Transfer::NikonNLog => {
                let code = if value < 0.328 {
                    650.0 * (value + 0.0075).powf(1.0 / 3.0)
                } else {
                    150.0 * value.ln() + 619.0
                };
                code / 1023.0
            }
            Transfer::DjiDLog => {
                if value <= 0.0078 {
                    6.025 * value + 0.0929
                } else {
                    (value * 0.9892 + 0.0108).log10() * 0.256663 + 0.584555
                }
            }
            Transfer::DjiDLog2 => {
                // Three-segment DCTL curve, evaluated branch-by-branch with the
                // pinned constants. The negative toe is not clipped here: pure
                // curve functions keep signed/over-range values, and delivery
                // clips to the encodable signal domain later.
                if value < DJI_DLOG2_T {
                    DJI_DLOG2_B2 + DJI_DLOG2_K2 * (value - DJI_DLOG2_T)
                } else if value < DJI_DLOG2_MID {
                    DJI_DLOG2_B1
                        + DJI_DLOG2_K1 * (value / DJI_DLOG2_MID).log2()
                } else {
                    (1.0 + value * DJI_DLOG2_TWO_POW_A_MINUS_1 / DJI_DLOG2_H).log2()
                        / DJI_DLOG2_A
                }
            }
        }
    }

    /// Inverse transfer: normalized signal -> linear target-gamut reflection.
    pub fn inverse(self, signal: f64) -> f64 {
        match self {
            Transfer::CanonLog3RevEng => {
                let l = (signal * 65472.0 - 4096.0) / 56064.0;
                // Analytic inverse of the recovered forward curve, not a
                // recovered SDK inverse LUT. Rounded SDK coefficients cause
                // ~1.68e-10 downward jumps at the joins; prefer the central
                // branch in the overlapping signal intervals.
                let x = if l < 0.073059361 - 2.3069815 * 0.014 {
                    (-((0.07623209 - l) * std::f64::consts::LN_10 / 0.42889912).exp_m1() / 14.98325)
                        .min(-0.014)
                } else if l < 0.073059361 + 2.3069815 * 0.014 {
                    (l - 0.073059361) / 2.3069815
                } else {
                    (((l - 0.069886632) * std::f64::consts::LN_10 / 0.42889912).exp_m1() / 14.98325)
                        .max(0.014)
                };
                0.9 * x
            }
            Transfer::CanonLog2RevEng => {
                let l = (signal * 65472.0 - 4096.0) / 56064.0 - 0.035388128;
                0.9 * l.signum() * (l.abs() / 0.122411586).exp_m1() / 87.09937546
            }
            Transfer::Srgb => preview::srgb_eotf(signal),
            Transfer::SonySLog3 => {
                if signal >= 171.2102946929 / 1023.0 {
                    10.0_f64.powf((signal * 1023.0 - 420.0) / 261.5) * (0.18 + 0.01) - 0.01
                } else {
                    (signal * 1023.0 - 95.0) * 0.01125 / (171.2102946929 - 95.0)
                }
            }
            Transfer::CanonLog2 => {
                let x = if signal < 0.092864125 {
                    -(10.0_f64.powf((0.092864125 - signal) / 0.24136077) - 1.0) / 87.099375
                } else {
                    (10.0_f64.powf((signal - 0.092864125) / 0.24136077) - 1.0) / 87.099375
                };
                x * 0.9
            }
            Transfer::NikonNLog => {
                let code = signal * 1023.0;
                if code < 452.0 {
                    (code / 650.0).powf(3.0) - 0.0075
                } else {
                    ((code - 619.0) / 150.0).exp()
                }
            }
            Transfer::DjiDLog => {
                if signal <= 0.14 {
                    (signal - 0.0929) / 6.025
                } else {
                    (10.0_f64.powf(3.89616 * signal - 2.27752) - 0.0108) / 0.9892
                }
            }
            Transfer::DjiDLog2 => {
                // Analytic inverse of the pinned forward curve. The DCTL prints
                // this direction; `forward` above is its exact inverse.
                if signal < DJI_DLOG2_B2 {
                    DJI_DLOG2_T + (signal - DJI_DLOG2_B2) / DJI_DLOG2_K2
                } else if signal < DJI_DLOG2_B1 {
                    DJI_DLOG2_MID
                        * ((signal - DJI_DLOG2_B1) / DJI_DLOG2_K1).exp2()
                } else {
                    DJI_DLOG2_UPPER_SCALE
                        * ((DJI_DLOG2_A * signal).exp2() - 1.0)
                }
            }
        }
    }

    /// Linear input endpoints corresponding to signal 0 and 1. Inputs outside
    /// this interval are clipped explicitly (no tone mapping).
    pub fn domain(self) -> (f64, f64) {
        (self.inverse(0.0), self.inverse(1.0))
    }

    pub fn references(self) -> &'static [ReferenceCode] {
        match self {
            Transfer::CanonLog2RevEng => &CANON_REFERENCES,
            Transfer::CanonLog3RevEng => &CANON_LOG3_REFERENCES,
            Transfer::Srgb => &SRGB_REFERENCES,
            Transfer::SonySLog3 => &SONY_REFERENCES,
            Transfer::CanonLog2 => &CANON_REFERENCES,
            Transfer::NikonNLog => &NIKON_REFERENCES,
            Transfer::DjiDLog => &DJI_REFERENCES,
            Transfer::DjiDLog2 => &DJI_DLOG2_REFERENCES,
        }
    }

    /// How the reference codes were established.
    pub fn reference_basis(self) -> &'static str {
        match self {
            Transfer::CanonLog3RevEng => "independently reconstructed CRD Log3 curve; 589824 SDK forward LUT entries plus the effective 65536-entry output LUT matched exactly",
            Transfer::CanonLog2RevEng => "independently reconstructed CRD continuous curve; Canon appendix gray codes cross-check",
            Transfer::Srgb => "existing IEC 61966-2-1 8-bit reference value for 18% grey",
            Transfer::SonySLog3 => {
                "published 10-bit code values in the Sony technical summary appendix"
            }
            Transfer::CanonLog2 => {
                "published 10-bit code values in the Canon white paper appendix table"
            }
            Transfer::NikonNLog => {
                "evaluated from the official N-Log formula at 0%, 18% and 90% reflection; \
                 the specification publishes the formula and curve, not a code table"
            }
            Transfer::DjiDLog => {
                "published 10-bit code values in the DJI white paper characteristics table"
            }
            Transfer::DjiDLog2 => {
                "evaluated from the pinned fixed DCTL formula at the stated reflection values \
                 and quantized to the full 10-bit signal range; the DCTL publishes no vendor \
                 code table, so these are reference codes for this implementation, not a \
                 camera/video matching claim"
            }
        }
    }
}

const SRGB_REFERENCES: [ReferenceCode; 1] = [ReferenceCode::new(0.18, "18% grey", 472)];

const SONY_REFERENCES: [ReferenceCode; 3] = [
    ReferenceCode::new(0.0, "0% black", 95),
    ReferenceCode::new(0.18, "18% grey", 420),
    ReferenceCode::new(0.9, "90% white", 598),
];

const CANON_REFERENCES: [ReferenceCode; 5] = [
    ReferenceCode::new(0.0, "0% black", 95),
    ReferenceCode::new(0.18, "18% grey", 407),
    ReferenceCode::new(0.9, "90% white (100% scene linear)", 575),
    ReferenceCode::new(7.2, "800%", 797),
    ReferenceCode::new(14.4, "1600%", 871),
];

const CANON_LOG3_REFERENCES: [ReferenceCode; 5] = [
    ReferenceCode::new(0.0, "0% black", 128),
    ReferenceCode::new(0.18, "18% grey", 351),
    ReferenceCode::new(0.9, "90% white (100% scene linear)", 577),
    ReferenceCode::new(7.2, "800%", 908),
    ReferenceCode::new(14.4, "1600%", 1020),
];

const NIKON_REFERENCES: [ReferenceCode; 3] = [
    ReferenceCode::new(0.0, "0% black", 127),
    ReferenceCode::new(0.18, "18% grey", 372),
    ReferenceCode::new(0.9, "90% white", 603),
];

const DJI_REFERENCES: [ReferenceCode; 3] = [
    ReferenceCode::new(0.0, "0% black", 95),
    ReferenceCode::new(0.18, "18% grey", 408),
    ReferenceCode::new(0.9, "90% white", 586),
];

/// Full-range reference codes of the pinned D-Log 2 definition. The vendor DCTL
/// publishes the formula rather than a code table, so these values are derived
/// from it and are not a camera-internal ISO/EI clipping table (nor limited-range
/// Y codes; delivery range is applied separately).
const DJI_DLOG2_REFERENCES: [ReferenceCode; 7] = [
    ReferenceCode::new(0.0, "0% black (linear segment origin)", 64),
    ReferenceCode::new(0.028961695254132, "lower breakpoint T", 152),
    ReferenceCode::new(0.18, "18% grey", 312),
    ReferenceCode::new(0.9, "90% white", 456),
    ReferenceCode::new(46.08, "800% (8 stops above 18% grey)", 812),
    ReferenceCode::new(184.32, "1024x 18% grey (10 stops)", 937),
    ReferenceCode::new(475.0, "upper clip limit H", 1023),
];

/// One target colour gamut with its official primaries and white point.
#[derive(Debug, Clone, Copy)]
pub struct Gamut {
    pub name: &'static str,
    pub primaries_xy: [[f64; 2]; 3],
    pub white_xy: [f64; 2],
    pub rgb_to_xyz: [[f64; 3]; 3],
    pub xyz_to_rgb: [[f64; 3]; 3],
    pub provenance: &'static str,
}

/// IEC 61966-2-1 sRGB primaries, D65 white. The `xyz_to_rgb` matrix is the
/// exact published matrix used by the pre-existing preview path.
const SRGB_GAMUT: Gamut = Gamut {
    name: "sRGB / Rec.709 (IEC 61966-2-1)",
    primaries_xy: [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]],
    white_xy: D65_WHITE_XY,
    rgb_to_xyz: [
        [0.412390799266, 0.357584339384, 0.180480788402],
        [0.212639005872, 0.715168678768, 0.072192315361],
        [0.019330818716, 0.119194779795, 0.950532152250],
    ],
    xyz_to_rgb: preview::XYZ_D65_TO_LINEAR_SRGB,
    provenance: "IEC 61966-2-1:1999 primaries and D65 white; existing pipeline matrix",
};

/// Sony S-Gamut3 primaries and D65 white (Sony technical summary, page 8).
const SGAMUT3_GAMUT: Gamut = Gamut {
    name: "Sony S-Gamut3",
    primaries_xy: [[0.73000, 0.28000], [0.14000, 0.85500], [0.10000, -0.05000]],
    white_xy: D65_WHITE_XY,
    rgb_to_xyz: [
        [0.706482713192, 0.128801049791, 0.115172164069],
        [0.270979670813, 0.786606411221, -0.057586082034],
        [-0.009677845386, 0.004600037493, 1.094135558654],
    ],
    xyz_to_rgb: [
        [1.507399899060, -0.245822137385, -0.171611680816],
        [-0.518151727083, 1.355391240902, 0.125878668162],
        [0.015511698158, -0.007872771427, 0.911916365575],
    ],
    provenance: "Sony technical summary V1.0, 'Color Primaries and conversion matrix'; \
                 matrix derived from the published primaries and D65 white by the standard \
                 normalised-primary-matrix construction",
};

/// Sony S-Gamut3.Cine primaries and D65 white (Sony technical summary, page 8).
const SGAMUT3_CINE_GAMUT: Gamut = Gamut {
    name: "Sony S-Gamut3.Cine",
    primaries_xy: [[0.76600, 0.27500], [0.22500, 0.80000], [0.08900, -0.08700]],
    white_xy: D65_WHITE_XY,
    rgb_to_xyz: [
        [0.599083920758, 0.248925516115, 0.102446490178],
        [0.215075820116, 0.885068501744, -0.100144321859],
        [-0.032065849545, -0.027658390679, 1.148781990984],
    ],
    xyz_to_rgb: [
        [1.846778969290, -0.525986122969, -0.210545211388],
        [-0.444153262903, 1.259442902808, 0.149399972888],
        [0.040855421196, 0.015640889310, 0.868207248699],
    ],
    provenance: "Sony technical summary V1.0, 'Color Primaries and conversion matrix'; \
                 matrix derived from the published primaries and D65 white by the standard \
                 normalised-primary-matrix construction",
};

/// Canon Cinema Gamut primaries and D65 white.
const CINEMA_GAMUT: Gamut = Gamut {
    name: "Canon Cinema Gamut",
    primaries_xy: [[0.7400, 0.2700], [0.1700, 1.1400], [0.0800, -0.1000]],
    white_xy: D65_WHITE_XY,
    rgb_to_xyz: [
        [0.716049646552, 0.129683477876, 0.104722802624],
        [0.261261357526, 0.869642145755, -0.130903503281],
        [-0.009676346575, -0.236481636126, 1.335215733461],
    ],
    xyz_to_rgb: [
        [1.489818274932, -0.260895902184, -0.142426521777],
        [-0.458166574469, 1.261627783050, 0.159623631630],
        [-0.070349667723, 0.221557667226, 0.776181603627],
    ],
    provenance: "Canon Color Gamut Conversion for DCI-P3+ and Cinema Gamut, PUB. DIM-1102-000 \
                 (2013), English p.2 (https://gdlp01.c-wss.com/gds/8/0200003438/01/colorgamutinfo-dim1102.pdf): \
                 D65 white (0.3127,0.3290), R(0.7400,0.2700), G(0.1700,1.1400), B(0.0800,-0.1000). \
                 Unchanged primaries cross-validated against colour-science and Canon's ACES \
                 linear Cinema Gamut IDT distributed by ASWF OCIO Config ACES. Matrices derived \
                 with D65 by normalised-primary-matrix construction, not the IDT's D55 calibration",
};

/// ITU-R BT.2020 primaries and D65 white (the N-Log specification repeats them).
const BT2020_GAMUT: Gamut = Gamut {
    name: "ITU-R BT.2020",
    primaries_xy: [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]],
    white_xy: D65_WHITE_XY,
    rgb_to_xyz: [
        [0.636958048301, 0.144616903586, 0.168880975164],
        [0.262700212011, 0.677998071519, 0.059301716470],
        [0.000000000000, 0.028072693049, 1.060985057711],
    ],
    xyz_to_rgb: [
        [1.716651187971, -0.355670783776, -0.253366281374],
        [-0.666684351832, 1.616481236635, 0.015768545814],
        [0.017639857445, -0.042770613258, 0.942103121235],
    ],
    provenance: "Nikon N-Log Specification Document §3 Gamut and White Point (primaries and \
                 D65 white identical to ITU-R BT.2020); matrix from the published primaries",
};

/// DJI D-Gamut primaries and D65 white.
const DGAMUT_GAMUT: Gamut = Gamut {
    name: "DJI D-Gamut",
    primaries_xy: [[0.71, 0.31], [0.21, 0.88], [0.09, -0.08]],
    white_xy: D65_WHITE_XY,
    rgb_to_xyz: [
        [0.648171968634, 0.194058149821, 0.108225808597],
        [0.283004662361, 0.813196056392, -0.096200718753],
        [-0.018258365314, -0.083167778495, 1.190483894568],
    ],
    xyz_to_rgb: [
        [1.725775899752, -0.431443974938, -0.191752877750],
        [-0.602444115230, 1.390574412063, 0.167137296299],
        [-0.015618936122, 0.090529173446, 0.848729956075],
    ],
    provenance: "DJI D-Log/D-Gamut white paper (2017-10-10): primaries R(0.71,0.31), \
                 G(0.21,0.88), B(0.09,-0.08), D65 white, and printed RGB<->XYZ matrices. The \
                 printed 4-decimal matrices are not exactly neutral; the matrices here are \
                 derived from the published primaries and white point and agree with the \
                 printed matrices to 1.84e-4",
};

/// DJI D-Gamut 2. The fixed DCTL publishes only the 4-decimal D-Gamut 2 ->
/// XYZ(D65) matrix, so the primary chromaticities here are recovered from that
/// rounded matrix (they are not an infinite-precision official primary table)
/// and the matrix is used digit for digit. Its row sums are 0.9503/0.9999/1.0891,
/// i.e. the rounded chain is not exactly neutral at D65; that residual is kept.
const DGAMUT2_GAMUT: Gamut = Gamut {
    name: "DJI D-Gamut 2",
    primaries_xy: [
        [0.7346787041954328, 0.2653212958045672],
        [0.15996792623032977, 0.8400320737696703],
        [0.08999181892555222, -0.0799927279338242],
    ],
    white_xy: D65_WHITE_XY,
    rgb_to_xyz: DCTL_DGAMUT2_TO_XYZ65,
    xyz_to_rgb: DGAMUT2_TO_XYZ65_INVERSE,
    provenance: "derived-from-rounded-matrix: primary chromaticities recovered from the \
                 pinned DCTL's 4-decimal D-Gamut 2->XYZ65 matrix; D65 white is the chain's \
                 own coding white. The matrix is used as published, never normalised to an \
                 exact neutral axis (max rgb-white residual 2.5e-4). Not an infinite-precision \
                 official DJI primaries table",
};

/// Pinned DCTL D-Gamut 2 -> XYZ(D65), copied digit for digit (row major).
const DCTL_DGAMUT2_TO_XYZ65: [[f64; 3]; 3] = [
    [0.6917, 0.1596, 0.0990],
    [0.2498, 0.8381, -0.0880],
    [0.0, 0.0, 1.0891],
];

/// Pinned DCTL CAT02 XYZ(D65)->XYZ(D60), copied digit for digit. The reference
/// chain applies its inverse (XYZ(D60)->XYZ(D65)), as required by
/// `inverse(M_DG2_AP0) * M_AP1_AP0`; the printed constant itself is preserved
/// digit for digit and its 4-decimal rounding contributes to the chain's
/// white-axis residual.
const DCTL_CAT02_D65_TO_D60: [[f64; 3]; 3] = [
    [1.0120, 0.0080, -0.0158],
    [0.0058, 1.0014, -0.0063],
    [-0.0003, -0.0010, 0.9276],
];

/// Pinned DCTL XYZ(D60) -> ACES AP0, copied digit for digit.
const DCTL_XYZ60_TO_AP0: [[f64; 3]; 3] = [
    [1.0498, 0.0, -0.0001],
    [-0.4959, 1.3733, 0.0982],
    [0.0, 0.0, 0.9913],
];

/// ACES AP1 -> AP0 in f64 from the ACES v1.3 chromaticities (common D60
/// 0.32168/0.33767), by the normalised-primary-matrix construction. Derived by
/// the independent oracle (`dlog2_oracle.py`); not the DCTL's rounded XYZ->AP0.
const ACES_AP1_TO_AP0: [[f64; 3]; 3] = [
    [0.6954522413574518, 0.14067869647029416, 0.16386906217225408],
    [0.04479456337203771, 0.8596711184564219, 0.09553431817154041],
    [-0.005525882558113589, 0.004025210305978657, 1.001500672252135],
];

/// Pinned DCTL D-Gamut 2 -> ACES AP0: `M_XYZ60_AP0 * M_CAT02_65_60 * M_DG2_XYZ65`.
const DJI_DLOG2_DGAMUT2_TO_AP0: [[f64; 3]; 3] = [
    [0.736958385971, 0.176597444598, 0.08627254501],
    [0.000873985052, 1.07033801293, -0.071242291115],
    [-0.000453331403, -0.000878271974, 1.001517785098],
];

/// Inverse of the pinned DCTL D-Gamut 2 -> XYZ(D65) matrix (the gamut's declared
/// `xyz_to_rgb`), derived in f64 by the independent oracle.
const DGAMUT2_TO_XYZ65_INVERSE: [[f64; 3]; 3] = [
    [1.5524806727641005, -0.29564003743365996, -0.16500955825710037],
    [-0.462724820494538, 1.281292066997886, 0.14559127639773503],
    [0.0, 0.0, 0.918189330639978],
];

/// The complete pinned reference chain used per pixel:
/// `inverse(M_DG2_AP0) * M_AP1_AP0`, i.e. AP1/D60 -> AP0/D60 -> (the DCTL's
/// printed CAT02 D65->D60 inverted) -> XYZ/D65 -> D-Gamut 2 linear.
const DJI_DLOG2_AP1_TO_DGAMUT2: [[f64; 3]; 3] = [
    [0.9345061440003459, -0.0022031394702776525, 0.06794785515875235],
    [0.04075107103644273, 0.8034933746397581, 0.15577128678091975],
    [-0.005058772922983332, 0.0047227291779441995, 1.0001502715262953],
];

/// Net AP1/D60 -> D-Gamut 2/D65 chromatic adaptation realized by the pinned
/// chain, reported as the `output_cat` matrix. It is the exact f64 inverse of the
/// DCTL's printed CAT02 D65->D60 constant (not a re-derivation from primaries);
/// the per-pixel chain still uses the printed constant through
/// [`DJI_DLOG2_AP1_TO_DGAMUT2`].
const DJI_DLOG2_NET_D60_TO_D65: [[f64; 3]; 3] = [
    [0.9881924154338105, -0.007877731919166615, 0.01677857961703693],
    [-0.00572153126675796, 0.9986543412975397, 0.006685125222250674],
    [0.00031342841026669386, 0.0010740524166901572, 1.078063517355657],
];

/// One registered preview output profile.
#[derive(Debug, Clone, Copy)]
pub struct ColorProfile {
    pub id: &'static str,
    pub transfer: Transfer,
    pub gamut: &'static Gamut,
    pub range: PreviewRange,
    pub ycbcr_matrix: &'static str,
    pub range_reason: &'static str,
    pub ycbcr_reason: &'static str,
    pub tag_policy: TagPolicy,
    /// The profile's own default output CAT and pinned reference realization.
    pub output_cat: OutputCatDefault,
}

/// The output CAT requested for this run. The method belongs to the output
/// profile (or to the CLI override), never to a transfer curve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputCatSelection {
    /// The selected profile's own default realization.
    Profile,
    /// One explicitly requested registered CAT token.
    Method(&'static str),
}

impl OutputCatSelection {
    /// Requested value as written in the run log and colour contract.
    pub fn token(self) -> &'static str {
        match self {
            OutputCatSelection::Profile => cat::PROFILE_SELECTION,
            OutputCatSelection::Method(token) => token,
        }
    }
}

/// How the selected CAT was realized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputCatRealization {
    /// The profile's pinned reference chain, using its fixed published constants.
    Reference,
    /// The standard derivation for an explicit non-default method.
    CustomAdaptation,
}

impl OutputCatRealization {
    pub fn token(self) -> &'static str {
        match self {
            OutputCatRealization::Reference => "reference",
            OutputCatRealization::CustomAdaptation => "custom-adaptation",
        }
    }
}

/// A profile's pinned default CAT and its fixed reference realization.
///
/// The reference realization is the profile's real extension point: a newly
/// registered profile declares its method, the adaptation matrix actually
/// applied between AP1/D60 and the gamut white, and (when the profile's chain is
/// not expressible as CAT-then-gamut, e.g. a pinned DCTL AP1->AP0->gamut chain)
/// the complete composed matrix. Reference constants are preserved as published
/// and are never replaced by a re-derived higher-precision matrix.
#[derive(Debug, Clone, Copy)]
pub struct OutputCatDefault {
    /// Registered CAT algorithm token the reference chain realizes.
    pub method: &'static str,
    /// Fixed adaptation matrix actually applied: AP1/D60 -> gamut white point.
    pub adaptation: [[f64; 3]; 3],
    /// Pinned full composed AP1/D60 -> target-linear matrix for a profile-specific
    /// chain. `None` composes `adaptation` with the gamut's declared `xyz_to_rgb`
    /// in the historic order.
    pub composed_override: Option<[[f64; 3]; 3]>,
    /// Source of the fixed chain and its rounding.
    pub provenance: &'static str,
}

/// The pre-existing output realization shared by every registered profile: the
/// published Bradford D60->D65 constant composed exactly as before.
const PREVIEW_OUTPUT_CAT: OutputCatDefault = OutputCatDefault {
    method: cat::BRADFORD.token,
    adaptation: preview::BRADFORD_D60_TO_D65,
    composed_override: None,
    provenance: "published Bradford D60(ACES white)->D65 constant already used by the \
                 pipeline; reference chain is AP1/D60 -> XYZ/D60 -> fixed Bradford -> XYZ/D65 -> \
                 the profile's declared target linear matrix, in the historic composition order",
};

/// The D-Log 2 profile's pinned reference realization: the fixed DCTL chain,
/// preserving its four-decimal constants (including the white-axis residual).
const DJI_DLOG2_OUTPUT_CAT: OutputCatDefault = OutputCatDefault {
    method: cat::CAT02.token,
    adaptation: DJI_DLOG2_NET_D60_TO_D65,
    composed_override: Some(DJI_DLOG2_AP1_TO_DGAMUT2),
    provenance: "derived-from-pinned-DCTL: AP1/D60 -> AP0/D60 (ACES v1.3 chromaticities, \
                 f64 normalised-primary construction) -> XYZ/D60 -> the inverse of the DCTL's \
                 printed CAT02 D65->D60 constant (i.e. XYZ/D60 -> XYZ/D65, as required by \
                 inverse(M_DG2_AP0) * M_AP1_AP0) -> XYZ/D65 -> D-Gamut 2 linear (inverse of \
                 the DCTL's 4-decimal D-Gamut 2->XYZ65 matrix). The three DCTL matrices are \
                 used digit for digit; the rounded chain's white-axis residual (max 2.5e-4) \
                 is preserved and never normalised away. The reported adaptation matrix is \
                 the net D60->D65 step (exact f64 inverse of the printed constant, i.e. the \
                 matrix actually applied here), not a re-derived CAT. reference_status=\
                 unverified refers to camera exposure/video matching, not to this math \
                 implementation",
};

/// One resolved output transform: frozen before per-pixel conversion.
#[derive(Debug, Clone)]
pub struct OutputTransform {
    /// Profile id this transform belongs to.
    pub profile: &'static str,
    /// The selection as requested (profile default or an explicit token).
    pub requested: OutputCatSelection,
    /// Registered CAT token actually realized.
    pub method: &'static str,
    /// Human-readable algorithm name.
    pub method_name: &'static str,
    pub realization: OutputCatRealization,
    /// Source white (AP1/D60) and the gamut's target white, encoded at Y=1.
    pub source_white_xy: [f64; 2],
    pub target_white_xy: [f64; 2],
    /// Adaptation matrix actually applied.
    pub cat_matrix: [[f64; 3]; 3],
    /// Composed AP1/D60 -> target-linear matrix used per pixel.
    pub composed_matrix: [[f64; 3]; 3],
    /// Algorithm/reference source and derivation note.
    pub provenance: String,
    /// Stable change-detection fingerprint (not a cryptographic digest).
    pub fingerprint: String,
}

impl OutputTransform {
    /// Bounded one-line provenance for the run log.
    pub fn log_fields(&self) -> String {
        format!(
            "output_cat_requested={} output_cat_method={} output_cat_realization={} \
             output_cat_source_white={:.5},{:.5} output_cat_target_white={:.5},{:.5} \
             output_cat_fingerprint={}",
            self.requested.token(),
            self.method,
            self.realization.token(),
            self.source_white_xy[0],
            self.source_white_xy[1],
            self.target_white_xy[0],
            self.target_white_xy[1],
            self.fingerprint
        )
    }
}

/// Writable backing for conversion. The pipeline supplies the unique Arc that
/// becomes its RGB24 NdArray, so conversion does not copy a full-frame buffer.
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

/// Frozen input development recipe. Legacy is retained only for historical tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraRecipe {
    LegacyInverseCm,
    Generic,
    CanonClog3,
}
impl CameraRecipe {
    pub fn native_token(self) -> &'static str {
        match self {
            Self::LegacyInverseCm => "legacy-inverse-cm",
            Self::Generic | Self::CanonClog3 => "dng-camera-to-pcs",
        }
    }
    pub fn token(self) -> &'static str {
        match self { Self::LegacyInverseCm => "legacy-test", Self::Generic => "generic", Self::CanonClog3 => "canon-clog3" }
    }
    pub fn correction(self) -> &'static str {
        if self == Self::CanonClog3 { "hue-sat-map" } else { "none" }
    }
    pub fn calibration(self) -> &'static str {
        if self == Self::CanonClog3 { "canon-normal-white-fixed-v1" } else { "none" }
    }
    pub fn requires_canon_cr3(self) -> bool { self == Self::CanonClog3 }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraColor { Auto, Generic, CanonClog3 }
impl CameraColor {
    pub fn token(self) -> &'static str { match self { Self::Auto => "auto", Self::Generic => "generic", Self::CanonClog3 => "canon-clog3" } }
    /// Called after the existing single-extension enumeration, before prepare.
    pub fn resolve(self, profile: &ColorProfile, all_cr3: bool) -> Result<CameraRecipe, String> {
        let clog3 = profile.transfer == Transfer::CanonLog3RevEng;
        match self {
            Self::Generic => Ok(CameraRecipe::Generic),
            Self::Auto if clog3 && all_cr3 => Ok(CameraRecipe::CanonClog3),
            Self::Auto => Ok(CameraRecipe::Generic),
            Self::CanonClog3 if clog3 && all_cr3 => Ok(CameraRecipe::CanonClog3),
            Self::CanonClog3 => Err("--camera-color canon-clog3 requires CLog3 profile and original Canon CR3 photos".into()),
        }
    }
}
impl ColorProfile {
    /// Curve input units, independent of the camera recipe and exposure calibration.
    pub fn curve_input_scale(&self) -> f64 {
        match self.transfer { Transfer::CanonLog2RevEng | Transfer::CanonLog3RevEng => 0.9, _ => 1.0 }
    }
    pub fn is_rev_eng(&self) -> bool {
        matches!(
            self.transfer,
            Transfer::CanonLog2RevEng | Transfer::CanonLog3RevEng
        )
    }
    pub fn display_name(&self) -> &'static str {
        match self.id {
            "canon-log2-cinema-gamut" => "clog2(white paper)",
            "canon-log2-cinema-gamut-rev-eng-crm-dev" => "clog2(rev-eng crm-dev)",
            "canon-log3-cinema-gamut-rev-eng-crm-dev" => "clog3 (rev-eng crm-dev)",
            _ => self.id,
        }
    }

    /// Resolve the output CAT for this run. The profile's own fixed reference
    /// realization is used for `Profile` and for an explicit method equal to the
    /// profile default (idempotent same-value selection). A different explicit
    /// method uses the standard derivation and is labeled `custom-adaptation`.
    pub fn resolve_output_cat(
        &self,
        requested: OutputCatSelection,
        registry: &cat::CatRegistry,
    ) -> Result<OutputTransform, String> {
        cat::white_xyz(self.gamut.white_xy)?;
        match requested {
            OutputCatSelection::Profile => self.reference_output_transform(requested, registry),
            OutputCatSelection::Method(token) if token == self.output_cat.method => {
                self.reference_output_transform(requested, registry)
            }
            OutputCatSelection::Method(token) => {
                let algorithm = registry.lookup(token).ok_or_else(|| {
                    format!(
                        "invalid output CAT {:?}: expected {} or one of {}",
                        token,
                        cat::PROFILE_SELECTION,
                        registry.tokens()
                    )
                })?;
                self.custom_output_transform(requested, algorithm)
            }
        }
    }

    /// The profile's pinned reference realization, preserving its published
    /// constants and the historic composition order bit-for-bit.
    fn reference_output_transform(
        &self,
        requested: OutputCatSelection,
        registry: &cat::CatRegistry,
    ) -> Result<OutputTransform, String> {
        let default = &self.output_cat;
        let algorithm = registry.lookup(default.method).ok_or_else(|| {
            format!(
                "profile {} declares unregistered output CAT {:?}; registered: {}",
                self.id,
                default.method,
                registry.tokens()
            )
        })?;
        if !cat::matrix_is_finite(&default.adaptation) {
            return Err(format!(
                "profile {} reference output CAT {:?} has a non-finite adaptation matrix",
                self.id, default.method
            ));
        }
        if !cat::matrix_is_finite(&self.gamut.xyz_to_rgb) {
            return Err(format!(
                "profile {} target linear matrix is not finite",
                self.id
            ));
        }
        let composed = match default.composed_override {
            Some(matrix) => matrix,
            None => cat::mat3_mul(
                &self.gamut.xyz_to_rgb,
                &cat::mat3_mul(&default.adaptation, &preview::AP1_D60_TO_XYZ_D60),
            ),
        };
        if !cat::matrix_is_finite(&composed) {
            return Err(format!(
                "profile {} reference output transform is not finite",
                self.id
            ));
        }
        // A singular reference chain would silently discard a colour dimension.
        cat::invert3(&composed).map_err(|message| {
            format!(
                "profile {} reference output transform is invalid: {message}",
                self.id
            )
        })?;
        Ok(self.output_transform(
            requested,
            algorithm,
            OutputCatRealization::Reference,
            default.adaptation,
            composed,
            default.provenance.to_string(),
        ))
    }

    /// The standard derivation for an explicit non-default method.
    fn custom_output_transform(
        &self,
        requested: OutputCatSelection,
        algorithm: &cat::CatAlgorithm,
    ) -> Result<OutputTransform, String> {
        let adaptation =
            cat::adaptation_matrix(algorithm, cat::ACES_WHITE_XY, self.gamut.white_xy)?;
        let composed = cat::custom_output_matrix(
            algorithm,
            cat::ACES_WHITE_XY,
            self.gamut.white_xy,
            &preview::AP1_D60_TO_XYZ_D60,
            &self.gamut.rgb_to_xyz,
        )?;
        Ok(self.output_transform(
            requested,
            algorithm,
            OutputCatRealization::CustomAdaptation,
            adaptation,
            composed,
            format!(
                "{}; standard derivation at run start: inverse(target RGB->XYZ) x \
                 CAT(AP1/D60, target white) x AP1->XYZ, full adaptation with whites at Y=1",
                algorithm.provenance
            ),
        ))
    }

    fn output_transform(
        &self,
        requested: OutputCatSelection,
        algorithm: &cat::CatAlgorithm,
        realization: OutputCatRealization,
        cat_matrix: [[f64; 3]; 3],
        composed_matrix: [[f64; 3]; 3],
        provenance: String,
    ) -> OutputTransform {
        let fingerprint = cat::transform_fingerprint(
            self.id,
            algorithm.token,
            realization.token(),
            cat::ACES_WHITE_XY,
            self.gamut.white_xy,
            &cat_matrix,
            &composed_matrix,
        );
        OutputTransform {
            profile: self.id,
            requested,
            method: algorithm.token,
            method_name: algorithm.name,
            realization,
            source_white_xy: cat::ACES_WHITE_XY,
            target_white_xy: self.gamut.white_xy,
            cat_matrix,
            composed_matrix,
            provenance,
            fingerprint,
        }
    }

    /// The profile's default (reference) output transform. Used by the
    /// historical 8-bit reference path and the reference-code tests; the
    /// production encoder handoff receives the run's resolved transform.
    pub fn default_output_transform(&self) -> OutputTransform {
        self.resolve_output_cat(OutputCatSelection::Profile, &cat::CatRegistry::standard())
            .expect("registered profile must resolve its reference output transform")
    }
    /// Reference 8-bit quantization of signed FP32 AP1 input. Retained for the
    /// historical golden/reference tests; the product encoder node uses
    /// [`Self::convert_f32_frame_to_yuv10`] exclusively, so there is one
    /// runtime quantization boundary.
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
    pub fn convert_f32_frame_to_yuv10(
        &self,
        output: &OutputTransform,
        bytes: &[u8],
        width: usize,
        height: usize,
        reference_gain: f64,
        range: PreviewRange,
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
            out[index] = quantize_luma(luma, range);
            out[pixels + index] = quantize_chroma(cb, range);
            out[2 * pixels + index] = quantize_chroma(cr, range);
        }
        Ok(clipped)
    }

    /// Quantize one already-encoded target signal triple into the 10-bit
    /// handoff. Exposed so reference-code tests do not re-implement the
    /// production quantizer.
    pub fn encode_signal_yuv10(
        &self,
        signal: [f64; 3],
        range: PreviewRange,
    ) -> Result<[u16; 3], String> {
        let coding = self.ycbcr_coding()?;
        let (kr, kg, kb, div_cb, div_cr) = coding.coefficients();
        let luma = kr * signal[0] + kg * signal[1] + kb * signal[2];
        Ok([
            quantize_luma(luma, range),
            quantize_chroma((signal[2] - luma) / div_cb, range),
            quantize_chroma((signal[0] - luma) / div_cr, range),
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

    /// Compatibility utility for explicit RGB16 ACEScg inputs. The RAW pipeline
    /// uses convert_f32_frame for every profile.
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

    /// `-vf` colour conversion half of the preview filter chain (without the
    /// optional lanczos resize prefix).
    pub fn color_filter(&self) -> String {
        format!(
            "scale=out_color_matrix={}:out_range={},\
             setparams=color_primaries={}:color_trc={}:colorspace={}:range={}",
            self.scale_filter_token(),
            self.range.filter_token(),
            self.tag_policy.primaries_token(),
            self.tag_policy.transfer_token(),
            self.tag_policy.matrix_token(),
            self.range.filter_token()
        )
    }

    /// The complete per-profile colour contract for one resolved run transform.
    pub fn contract_json(&self, output: &OutputTransform) -> Json {
        debug_assert_eq!(output.profile, self.id);
        let (low, high) = self.transfer.domain();
        let mut matrices = vec![
            (
                "acescg_ap1_d60_to_xyz_d60",
                preview::matrix_json(&preview::AP1_D60_TO_XYZ_D60),
            ),
            ("output_cat", preview::matrix_json(&output.cat_matrix)),
            (
                "xyz_to_target_linear",
                preview::matrix_json(&self.gamut.xyz_to_rgb),
            ),
            (
                "composed_ap1_d60_to_target_linear",
                preview::matrix_json(&output.composed_matrix),
            ),
        ];
        if output.method == cat::BRADFORD.token
            && output.realization == OutputCatRealization::Reference
            && output.target_white_xy == D65_WHITE_XY
        {
            // Legacy-compatible field with its true old meaning: the published
            // Bradford D60->D65 constant that the pre-existing profiles apply.
            // It is never emitted for a different method or a custom derivation.
            matrices.push((
                "bradford_cat_d60_to_d65",
                preview::matrix_json(&output.cat_matrix),
            ));
        }
        if self.id == "dji-dlog2-dgamut2" {
            // Full traceability for the pinned reference chain. `output_cat`
            // above is the net D60->D65 adaptation; these are the printed DCTL
            // intermediates and the f64 ACES AP1->AP0 derivation it consumes.
            matrices.push((
                "dgamut2_to_xyz65",
                preview::matrix_json(&DCTL_DGAMUT2_TO_XYZ65),
            ));
            matrices.push((
                "cat02_d65_to_d60",
                preview::matrix_json(&DCTL_CAT02_D65_TO_D60),
            ));
            matrices.push((
                "xyz60_to_acesap0",
                preview::matrix_json(&DCTL_XYZ60_TO_AP0),
            ));
            matrices.push((
                "dgamut2_to_acesap0",
                preview::matrix_json(&DJI_DLOG2_DGAMUT2_TO_AP0),
            ));
            matrices.push((
                "acesap1_to_acesap0",
                preview::matrix_json(&ACES_AP1_TO_AP0),
            ));
        }
        if self.id == "srgb-preview" {
            // Keep the pre-existing sRGB contract keys available to the encoder
            // readers. Both describe the effective conversion, not a default
            // helper: the composed key must follow the run's resolved transform
            // so a custom realization cannot publish two incompatible
            // compositions. For the reference realization it is bit-identical to
            // `preview::ap1_to_linear_srgb_matrix()`.
            matrices.push((
                "xyz_d65_to_linear_srgb",
                preview::matrix_json(&preview::XYZ_D65_TO_LINEAR_SRGB),
            ));
            matrices.push((
                "composed_ap1_d60_to_linear_srgb_d65",
                preview::matrix_json(&output.composed_matrix),
            ));
        }

        Json::obj(vec![
            ("profile", Json::Str(self.id.to_string())),
            ("display_name", Json::Str(self.display_name().into())),
            ("curve_input_scale", Json::Float(self.curve_input_scale())),
            ("pipeline_revision", Json::Int(5)),
            (
                "transfer",
                Json::obj(vec![
                    ("name", Json::Str(self.transfer.name().to_string())),
                    ("formula", Json::Str(self.transfer.formula().to_string())),
                    (
                        "source",
                        Json::obj(vec![
                            ("document", Json::Str(self.transfer.document().to_string())),
                            ("version", Json::Str(self.transfer.version().to_string())),
                            ("url", Json::Str(self.transfer.url().to_string())),
                        ]),
                    ),
                    (
                        "reference_codes",
                        Json::Array(
                            self.transfer
                                .references()
                                .iter()
                                .map(|reference| {
                                    Json::obj(vec![
                                        ("label", Json::Str(reference.label.to_string())),
                                        ("linear", Json::Float(reference.linear)),
                                        ("code10", Json::Int(reference.code10 as i64)),
                                        ("reference_code8", Json::Int(reference.code8() as i64)),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                    (
                        "reference_basis",
                        Json::Str(self.transfer.reference_basis().to_string()),
                    ),
                ]),
            ),
            (
                "gamut",
                Json::obj(vec![
                    ("name", Json::Str(self.gamut.name.to_string())),
                    ("provenance", Json::Str(self.gamut.provenance.to_string())),
                    (
                        "primaries_xy",
                        Json::obj(vec![
                            ("red", pair_json(self.gamut.primaries_xy[0])),
                            ("green", pair_json(self.gamut.primaries_xy[1])),
                            ("blue", pair_json(self.gamut.primaries_xy[2])),
                        ]),
                    ),
                    ("white_xy", pair_json(self.gamut.white_xy)),
                    ("rgb_to_xyz", preview::matrix_json(&self.gamut.rgb_to_xyz)),
                    ("xyz_to_rgb", preview::matrix_json(&self.gamut.xyz_to_rgb)),
                ]),
            ),
            (
                "output_cat",
                Json::obj(vec![
                    ("requested", Json::Str(output.requested.token().into())),
                    ("method", Json::Str(output.method.into())),
                    ("algorithm", Json::Str(output.method_name.into())),
                    ("realization", Json::Str(output.realization.token().into())),
                    (
                        "reference",
                        Json::Bool(output.realization == OutputCatRealization::Reference),
                    ),
                    (
                        "custom_adaptation",
                        Json::Bool(
                            output.realization == OutputCatRealization::CustomAdaptation,
                        ),
                    ),
                    (
                        "direction",
                        Json::Str("AP1/D60 source white -> target gamut white".into()),
                    ),
                    ("source_white_xy", pair_json(output.source_white_xy)),
                    ("target_white_xy", pair_json(output.target_white_xy)),
                    ("fingerprint", Json::Str(output.fingerprint.clone())),
                    ("provenance", Json::Str(output.provenance.clone())),
                ]),
            ),
            ("matrices", Json::obj(matrices)),
            (
                "input_domain",
                Json::obj(vec![
                    ("linear_min", Json::Float(low)),
                    ("linear_max", Json::Float(high)),
                    (
                        "clip",
                        Json::Str(
                            "per-channel target-linear values clip to the official inverse \
                             endpoints; no tone mapping and no implicit highlight roll-off"
                                .to_string(),
                        ),
                    ),
                    (
                        "interpretation",
                        Json::Str("signed FP32 AP1/D60; native EV already applied; reference gain applied once in target gamut (1 for profiles without reference scaling)".to_string()),
                    ),
                ]),
            ),
            (
                "quantization",
                Json::obj(vec![
                    ("bits", Json::Int(f64::from(SIGNAL_BITS) as i64)),
                    (
                        "handoff",
                        Json::Str("planar yuv444p10le target signal; the single quantization boundary before FFmpeg".to_string()),
                    ),
                    (
                        "rounding",
                        Json::Str("round-half-away-from-zero on the coded signal (luma 64..940 or 0..1023; chroma centred at 512); no dither".to_string()),
                    ),
                    ("dither", Json::Bool(false)),
                    (
                        "tradeoff",
                        Json::Str(
                            "The 10-bit handoff is the delivery precision; lossy codec error is \
                             measured separately from this quantization. Lower-bit-depth codecs \
                             start from the same 10-bit buffer."
                                .to_string(),
                        ),
                    ),
                    (
                        "reference_convention",
                        Json::Str(
                            "official 10-bit codes are compared directly; lossy encoder error is \
                             reported separately"
                                .to_string(),
                        ),
                    ),
                ]),
            ),
            (
                "encoding",
                Json::obj(vec![
                    (
                        "signal_pix_fmt",
                        Json::Str("yuv444p10le".to_string()),
                    ),
                    ("range", Json::Str(self.range.as_str().to_string())),
                    ("range_reason", Json::Str(self.range_reason.to_string())),
                    ("ycbcr_matrix", Json::Str(self.ycbcr_matrix.to_string())),
                    ("ycbcr_formula", Json::Str(
                        self.ycbcr_coding()
                            .map(|coding| coding.formula().to_string())
                            .unwrap_or_else(|_| "unregistered".to_string())
                    )),
                    ("ycbcr_reason", Json::Str(self.ycbcr_reason.to_string())),
                    ("delivery_background", self.delivery_background_json()),
                    (
                        "scale_matrix_token",
                        Json::Str(self.scale_filter_token().to_string()),
                    ),
                    ("tag_policy", self.tag_policy_json()),
                ]),
            ),
            (
                "output_contract",
                Json::Str(
                    "One configured video per run: the encoder node's container receives this \
                     profile's target signal. ProRes is delivered limited-range because MOV colr \
                     is nclc (no range field); other containers carry the range they can express."
                        .to_string(),
                ),
            ),
        ])
    }

    /// Container colour code points for the range actually delivered. The range
    /// field can only be carried by MP4/Matroska `nclx`; MOV `nclc` has no
    /// range field, so the numbers here are a declaration, not a claim that
    /// every container writes them.
    pub fn emitted_tags_json(&self, range: PreviewRange) -> Json {
        Json::obj(vec![
            (
                "colour_primaries",
                Json::Int(self.tag_policy.primaries as i64),
            ),
            (
                "transfer_characteristics",
                Json::Int(self.tag_policy.transfer as i64),
            ),
            (
                "matrix_coefficients",
                Json::Int(self.tag_policy.matrix as i64),
            ),
            (
                "video_full_range_flag",
                Json::Int(if range == PreviewRange::Pc { 1 } else { 0 }),
            ),
        ])
    }

    fn tag_policy_json(&self) -> Json {
        Json::obj(vec![
            ("mode", Json::Str("per-field".to_string())),
            (
                "color_primaries",
                Json::Str(self.tag_policy.primaries_token().to_string()),
            ),
            (
                "color_trc",
                Json::Str(self.tag_policy.transfer_token().to_string()),
            ),
            (
                "colorspace",
                Json::Str(self.tag_policy.matrix_token().to_string()),
            ),
            ("range", Json::Str(self.range.as_str().to_string())),
            ("profile_code_points", self.emitted_tags_json(self.range)),
            (
                "reason",
                Json::Str(
                    "Per-field tag policy: write accurate code points in both layers; vendor Log \
                transfer and non-standard primaries have no accurate generic code point and \
                remain unspecified; matrix tags describe coding, not a gamut/transfer conversion"
                        .to_string(),
                ),
            ),
        ])
    }

    fn delivery_background_json(&self) -> Json {
        if self.tag_policy == TagPolicy::SRGB {
            return Json::Array(vec![]);
        }
        Json::Array(vec![
            Json::Str("Canon DP-V3010 Instruction Manual, dpv3010-im3-en, pp.47/50: automatic \
                YCbCr matrix is BT.709 outside the BT.2020 setting; Cinema Gamut monitoring \
                workflows therefore do not establish a universal BT.2020 delivery mandate \
                (https://gdlp01.c-wss.com/gds/7/0300013757/06/dpv3010-im3-en.pdf)".to_string()),
            Json::Str("Sony BVM-HX310 Operating Instructions, 4-748-188-13(1), software 1.2, \
                pp.24-25: for other colour spaces the receiver's Transfer Matrix must match \
                the signal output device (https://pro.sony/support/res/manuals/4748/2c99048095dffc320734089caca25c96/47481881M.pdf)".to_string()),
            Json::Str("Nikon Z 9 TIPS, Using the N-Log 3D LUT (October 2021), step 7 documents \
                both Limited/Video and Full/Full links; full range here is rawtl's convention \
                (https://nps.nikonimaging.com/technical_info/technical_solutions/z9_tips/3d_lut/). \
                The N-Log specification establishes BT.2020 primaries, not rawtl's NCL delivery choice".to_string()),
        ])
    }
}

fn pair_json(pair: [f64; 2]) -> Json {
    Json::Array(pair.iter().map(|value| Json::Float(*value)).collect())
}

/// 8-bit quantization: clamp to [0,1], scale by 255 and round half away from
/// zero. The value is clamped again after the transfer so a numerically
/// out-of-range curve evaluation cannot wrap.
pub fn quantize_signal(signal: f64) -> u8 {
    (signal.clamp(0.0, 1.0) * 255.0).round().clamp(0.0, 255.0) as u8
}

// The coefficient/code-point sources govern the transform, not the
// project's choice of matrix for vendor Log RGB or its full-range delivery.
const BT2020_NCL_CONTAINER_REASON: &str =
    "Rawtl delivery convention, not a vendor requirement: bt2020nc. \
     ITU-R BT.2020-2 (10/2015), Parameter values for ultra-high definition television systems \
     for production and international programme exchange, Table 4 \
     (https://www.itu.int/rec/R-REC-BT.2020-2-201510-I/en): \
     Y'=0.2627*R'+0.6780*G'+0.0593*B', Cb=(B'-Y')/1.8814, Cr=(R'-Y')/1.4746. \
     ITU-T H.273 (V4), Coding-independent code points for video signal type identification \
     (07/2024), section 8.3/Table 4 (https://www.itu.int/rec/T-REC-H.273-202407-I/en), \
     assigns matrix_coefficients=9. Khronos Data Format Specification v1.4.0 (02/2025), \
     section 15.1.3 independently derives the transform \
     (https://registry.khronos.org/DataFormat/specs/1.4/dataformat.1.4.html). \
     Only the ideal matrix transform is reversible; actual 8-bit 4:2:0 delivery has \
     quantization, chroma subsampling and lossy compression. This coding matrix does not \
     change vendor primaries or transfer and implies no gamut-containment claim.";
const LOG_RANGE_REASON: &str = "Rawtl full-range (pc) delivery convention, not a vendor mandate. \
     Nikon's N-Log LUT guide documents both limited and full links \
     (https://nps.nikonimaging.com/technical_info/technical_solutions/z9_tips/3d_lut/); \
     the transfer's code domain alone does not determine the delivery range.";

pub static PROFILES: [ColorProfile; 9] = [
    ColorProfile {
        id: "srgb-preview",
        transfer: Transfer::Srgb,
        gamut: &SRGB_GAMUT,
        range: PreviewRange::Tv,
        ycbcr_matrix: "bt709",
        range_reason: "existing display-referred sRGB review delivery uses limited (tv) range",
        ycbcr_reason: "existing sRGB preview conversion uses the BT.709 matrix",
        tag_policy: TagPolicy::SRGB,
        output_cat: PREVIEW_OUTPUT_CAT,
    },
    ColorProfile {
        id: "sony-slog3-sgamut3",
        transfer: Transfer::SonySLog3,
        gamut: &SGAMUT3_GAMUT,
        range: PreviewRange::Pc,
        ycbcr_matrix: "bt2020nc",
        range_reason: LOG_RANGE_REASON,
        ycbcr_reason: BT2020_NCL_CONTAINER_REASON,
        tag_policy: TagPolicy::VENDOR_LOG,
        output_cat: PREVIEW_OUTPUT_CAT,
    },
    ColorProfile {
        id: "sony-slog3-sgamut3-cine",
        transfer: Transfer::SonySLog3,
        gamut: &SGAMUT3_CINE_GAMUT,
        range: PreviewRange::Pc,
        ycbcr_matrix: "bt2020nc",
        range_reason: LOG_RANGE_REASON,
        ycbcr_reason: BT2020_NCL_CONTAINER_REASON,
        tag_policy: TagPolicy::VENDOR_LOG,
        output_cat: PREVIEW_OUTPUT_CAT,
    },
    ColorProfile {
        id: "canon-log2-cinema-gamut",
        transfer: Transfer::CanonLog2,
        gamut: &CINEMA_GAMUT,
        range: PreviewRange::Pc,
        ycbcr_matrix: "bt2020nc",
        range_reason: LOG_RANGE_REASON,
        ycbcr_reason: BT2020_NCL_CONTAINER_REASON,
        tag_policy: TagPolicy::VENDOR_LOG,
        output_cat: PREVIEW_OUTPUT_CAT,
    },
    ColorProfile {
        id: "nikon-nlog-bt2020",
        transfer: Transfer::NikonNLog,
        gamut: &BT2020_GAMUT,
        range: PreviewRange::Pc,
        ycbcr_matrix: "bt2020nc",
        range_reason: LOG_RANGE_REASON,
        ycbcr_reason: BT2020_NCL_CONTAINER_REASON,
        tag_policy: TagPolicy::NIKON_LOG,
        output_cat: PREVIEW_OUTPUT_CAT,
    },
    ColorProfile {
        id: "dji-dlog-dgamut",
        transfer: Transfer::DjiDLog,
        gamut: &DGAMUT_GAMUT,
        range: PreviewRange::Pc,
        ycbcr_matrix: "bt2020nc",
        range_reason: LOG_RANGE_REASON,
        ycbcr_reason: BT2020_NCL_CONTAINER_REASON,
        tag_policy: TagPolicy::VENDOR_LOG,
        output_cat: PREVIEW_OUTPUT_CAT,
    },
    ColorProfile {
        id: "dji-dlog2-dgamut2",
        transfer: Transfer::DjiDLog2,
        gamut: &DGAMUT2_GAMUT,
        range: PreviewRange::Pc,
        ycbcr_matrix: "bt2020nc",
        range_reason: LOG_RANGE_REASON,
        ycbcr_reason: BT2020_NCL_CONTAINER_REASON,
        tag_policy: TagPolicy::VENDOR_LOG,
        output_cat: DJI_DLOG2_OUTPUT_CAT,
    },
    ColorProfile {
        id: "canon-log2-cinema-gamut-rev-eng-crm-dev",
        transfer: Transfer::CanonLog2RevEng,
        gamut: &CINEMA_GAMUT,
        range: PreviewRange::Pc,
        ycbcr_matrix: "bt2020nc",
        range_reason: LOG_RANGE_REASON,
        ycbcr_reason: BT2020_NCL_CONTAINER_REASON,
        tag_policy: TagPolicy::VENDOR_LOG,
        output_cat: PREVIEW_OUTPUT_CAT,
    },
    ColorProfile {
        id: "canon-log3-cinema-gamut-rev-eng-crm-dev",
        transfer: Transfer::CanonLog3RevEng,
        gamut: &CINEMA_GAMUT,
        range: PreviewRange::Pc,
        ycbcr_matrix: "bt2020nc",
        range_reason: LOG_RANGE_REASON,
        ycbcr_reason: BT2020_NCL_CONTAINER_REASON,
        tag_policy: TagPolicy::VENDOR_LOG,
        output_cat: PREVIEW_OUTPUT_CAT,
    },
];

pub fn default_profile() -> &'static ColorProfile {
    &PROFILES[0]
}

pub fn lookup(id: &str) -> Option<&'static ColorProfile> {
    PROFILES.iter().find(|profile| profile.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() <= tolerance
    }

    #[test]
    fn rev_eng_curve_and_signed_f32_boundary_are_independently_reproduced() {
        let p = lookup("canon-log2-cinema-gamut-rev-eng-crm-dev").unwrap();
        assert_eq!(p.display_name(), "clog2(rev-eng crm-dev)");
        assert_eq!(
            lookup("canon-log2-cinema-gamut").unwrap().display_name(),
            "clog2(white paper)"
        );
        assert_ne!(p.transfer, Transfer::CanonLog2);
        let contract = p.contract_json(&p.default_output_transform());
        let input = contract
            .get("input_domain")
            .unwrap()
            .get("interpretation")
            .and_then(Json::as_str)
            .unwrap();
        assert!(input.contains("signed FP32") && !input.contains("16384"));

        for (x, code) in [
            (0., 95.),
            (0.18, 407.),
            (0.9, 575.),
            (7.2, 797.),
            (14.4, 871.),
        ] {
            assert_eq!((p.transfer.forward(x) * 1023.0).round(), code);
            assert!((p.transfer.inverse(p.transfer.forward(x)) - x).abs() < 1e-11);
        }
        for x in [-0.014, -0.005, 0., 0.18, 1., 4., 8., 50.] {
            assert!((p.transfer.inverse(p.transfer.forward(x)) - x).abs() < 1e-11);
        }
        // An AP1-negative color becomes all-positive in Cinema Gamut.
        let ap1 = [-0.005f32, 0.5, 0.05];
        let bytes: Vec<_> = ap1.into_iter().flat_map(f32::to_le_bytes).collect();
        let mut output = vec![];
        p.convert_f32_frame(&bytes, 1, 1, 1.0, &mut output).unwrap();
        assert_eq!(output, vec![37, 120, 87]); // independent scene -> reflection (.9) -> CRD Log2
        let expected = p.encode_linear(ap1.map(|x| x as f64));
        assert_eq!(output, expected);
        assert_ne!(output, p.encode_linear([0., 0.5, 0.05]));
        p.convert_f32_frame(&bytes, 1, 1, 2., &mut output).unwrap();
        assert_eq!(output, p.encode_linear(ap1.map(|x| 2.0 * x as f64)));
        let bytes: Vec<_> = [8f32; 3].into_iter().flat_map(f32::to_le_bytes).collect();
        p.convert_f32_frame(&bytes, 1, 1, 1., &mut output).unwrap();
        assert_eq!(output, p.encode_linear([8.; 3]));
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let bytes: Vec<_> = [bad; 3].into_iter().flat_map(f32::to_le_bytes).collect();
            assert!(p.convert_f32_frame(&bytes, 1, 1, 1., &mut output).is_err());
        }
    }

    #[test]
    fn every_profile_accepts_signed_f32_without_legacy_linear_clipping() {
        for p in &PROFILES {
            let bytes: Vec<_> = [-0.005f32, 0.5, 0.05]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect();
            let mut out = vec![];
            p.convert_f32_frame(&bytes, 1, 1, 1., &mut out).unwrap();
            assert_eq!(out.len(), 3);
            let input = p.contract_json(&p.default_output_transform());
            let input = input
                .get("input_domain")
                .unwrap()
                .get("interpretation")
                .and_then(Json::as_str)
                .unwrap();
            assert!(
                input.contains("signed FP32") && !input.contains("16384"),
                "{}",
                p.id
            );
            if p.transfer == Transfer::CanonLog2 || p.transfer == Transfer::CanonLog2RevEng {
                assert_eq!(out, if p.is_rev_eng() { [37, 120, 87] } else { [38, 122, 90] }, "independent Canon negative-AP1 vector");
            }
            let bytes: Vec<_> = [8f32; 3].into_iter().flat_map(f32::to_le_bytes).collect();
            p.convert_f32_frame(&bytes, 1, 1, 1., &mut out).unwrap();
            if p.transfer != Transfer::Srgb {
                assert_ne!(
                    out,
                    p.encode_linear([65535. / 16384.; 3]),
                    "{} lost high range",
                    p.id
                );
            }
            assert!(p.convert_f32_frame(&bytes, 1, 1, 2., &mut out).is_ok());
            assert!(p
                .convert_f32_frame(&bytes[..11], 1, 1, 1., &mut out)
                .is_err());
            for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                let bytes: Vec<_> = [bad; 3].into_iter().flat_map(f32::to_le_bytes).collect();
                assert!(p.convert_f32_frame(&bytes, 1, 1, 1., &mut out).is_err());
            }
        }
    }

    #[test]
    fn registry_has_nine_profiles_and_srgb_default() {
        assert_eq!(PROFILES.len(), 9);
        assert_eq!(default_profile().id, "srgb-preview");
        for id in [
            "srgb-preview",
            "dji-dlog-dgamut",
            "dji-dlog2-dgamut2",
            "canon-log2-cinema-gamut",
            "canon-log2-cinema-gamut-rev-eng-crm-dev",
            "canon-log3-cinema-gamut-rev-eng-crm-dev",
            "sony-slog3-sgamut3",
            "sony-slog3-sgamut3-cine",
            "nikon-nlog-bt2020",
        ] {
            assert!(lookup(id).is_some(), "missing profile {id}");
        }
        assert_eq!(
            lookup("srgb-preview").map(|profile| profile.id),
            Some("srgb-preview")
        );
        assert!(lookup("dlog-dgamut").is_none());
        assert!(lookup("").is_none());
        assert!(lookup("srgb-preview ").is_none());
    }

    #[test]
    fn srgb_composed_matrix_is_bit_identical_to_the_existing_path() {
        let profile = lookup("srgb-preview").expect("srgb");
        let expected = preview::ap1_to_linear_srgb_matrix();
        let observed = profile.composed_matrix();
        for row in 0..3 {
            for column in 0..3 {
                assert_eq!(
                    observed[row][column].to_bits(),
                    expected[row][column].to_bits(),
                    "srgb composed matrix [{row}][{column}]"
                );
            }
        }
    }

    #[test]
    fn every_profile_preserves_neutral_grey_through_the_gamut_conversion() {
        for profile in &PROFILES {
            let matrix = profile.composed_matrix();
            let linear = preview::mat3_vec(&matrix, [0.18, 0.18, 0.18]);
            if profile.id == "dji-dlog2-dgamut2" {
                // The pinned DCTL chain is deliberately not normalised to an exact
                // neutral axis; its rounded white-axis residual is frozen here as
                // the oracle's AP1-white mapping scaled by 0.18.
                const WHITE_MAPPED: [f64; 3] =
                    [1.0002508596888207, 1.0000157324571206, 0.9998142277812561];
                for channel in 0..3 {
                    let expected = WHITE_MAPPED[channel] * 0.18;
                    assert!(
                        close(linear[channel], expected, 1e-12),
                        "{} channel {channel} = {} vs {expected}",
                        profile.id,
                        linear[channel]
                    );
                    assert!(
                        (linear[channel] - 0.18).abs() > 1e-6,
                        "{} must keep the rounded-chain residual",
                        profile.id
                    );
                }
                continue;
            }
            for channel in 0..3 {
                assert!(
                    close(linear[channel], 0.18, 1e-9),
                    "{} channel {channel} = {}",
                    profile.id,
                    linear[channel]
                );
            }
        }
    }

    #[test]
    fn official_reference_codes_are_reproduced() {
        for profile in &PROFILES {
            for reference in profile.transfer.references() {
                let codes = profile.encode_target_linear([reference.linear; 3]);
                for channel in 0..3 {
                    let difference = (codes[channel] as i32 - reference.code8() as i32).abs();
                    assert!(
                        difference <= 1,
                        "{} {} channel {channel}: observed {} vs reference {} (code10 {})",
                        profile.id,
                        reference.label,
                        codes[channel],
                        reference.code8(),
                        reference.code10
                    );
                }
            }
        }
    }

    #[test]
    fn independently_derived_non_neutral_patches_are_reproduced() {
        // Independently derived by reference_derive.py from the published
        // primaries and transfer formulas (see the S01 report); the test does
        // not call the production matrices to obtain these expectations.
        for (id, expected) in [
            ("srgb-preview", [198u8, 109, 75]),
            ("sony-slog3-sgamut3", [125, 104, 87]),
            ("sony-slog3-sgamut3-cine", [127, 104, 89]),
            ("canon-log2-cinema-gamut", [120, 101, 88]),
            ("nikon-nlog-bt2020", [121, 93, 74]),
            ("dji-dlog-dgamut", [123, 99, 86]),
        ] {
            let profile = lookup(id).expect(id);
            let codes = profile.encode_linear([0.4, 0.18, 0.09]);
            for channel in 0..3 {
                assert!(
                    (codes[channel] as i32 - expected[channel] as i32).abs() <= 1,
                    "{id} channel {channel}: observed {} vs independent reference {}",
                    codes[channel],
                    expected[channel]
                );
            }
        }
    }

    #[test]
    fn sony_gamut_choice_is_distinguishable() {
        // Pure AP1 green separates S-Gamut3 from S-Gamut3.Cine by a wide
        // margin after S-Log3 and 8-bit quantization. Expectations come from
        // reference_derive.py, not from the production matrices.
        let sgamut3 = lookup("sony-slog3-sgamut3").expect("sgamut3");
        let cine = lookup("sony-slog3-sgamut3-cine").expect("cine");
        let a = sgamut3.encode_linear([0.0, 1.0, 0.0]);
        let b = cine.encode_linear([0.0, 1.0, 0.0]);
        assert!(
            (a[0] as i32 - b[0] as i32).abs() >= 10,
            "the chosen reference must distinguish the two Sony gamuts: {a:?} vs {b:?}"
        );
        for (channel, expected) in [59u8, 147, 20].iter().enumerate() {
            assert!(
                (a[channel] as i32 - *expected as i32).abs() <= 1,
                "S-Gamut3 channel {channel}: {} vs {expected}",
                a[channel]
            );
        }
        for (channel, expected) in [0u8, 145, 49].iter().enumerate() {
            assert!(
                (b[channel] as i32 - *expected as i32).abs() <= 1,
                "S-Gamut3.Cine channel {channel}: {} vs {expected}",
                b[channel]
            );
        }
    }

    #[test]
    fn transfer_formulas_round_trip_within_official_precision() {
        for profile in &PROFILES {
            let (low, high) = profile.transfer.domain();
            for value in [low, 0.0, 0.005, 0.18, 0.5, 1.0, 4.0] {
                if value < low || value > high {
                    continue;
                }
                let back = profile.transfer.inverse(profile.transfer.forward(value));
                let tolerance = if profile.transfer == Transfer::DjiDLog {
                    // The DJI inverse constants are published rounded to six
                    // decimals; 1e-5 is far below one 10-bit code.
                    1e-5
                } else {
                    1e-9
                };
                assert!(
                    close(back, value, tolerance),
                    "{} inv(fwd({value})) = {back}",
                    profile.id
                );
            }
        }
    }

    #[test]
    fn quantized_round_trip_stays_within_one_ten_bit_code() {
        for profile in &PROFILES {
            for value in [0.01, 0.18, 0.5, 1.0] {
                let signal = profile.transfer.forward(value);
                let code = (signal * 1023.0).round() as i32;
                let back = profile.transfer.inverse(code as f64 / 1023.0);
                let again = (profile.transfer.forward(back) * 1023.0).round() as i32;
                assert!(
                    (again - code).abs() <= 1,
                    "{}: code {code} -> {again}",
                    profile.id
                );
            }
        }
    }

    #[test]
    fn ten_bit_handoff_carries_precision_that_eight_bit_quantization_loses() {
        // One 10-bit luma step is invisible after 8-bit quantization but must
        // survive the production handoff; this is the property that rejects a
        // "quantize to RGB8, then wrap in a 10-bit codec" implementation.
        let low = 0.5;
        let high = low + 1.0 / 876.0;
        assert_eq!(quantize_luma(low, PreviewRange::Tv) + 1, quantize_luma(high, PreviewRange::Tv));
        assert_eq!(quantize_signal(low), quantize_signal(high));
        assert_eq!(quantize_luma(0.0, PreviewRange::Tv), 64);
        assert_eq!(quantize_luma(1.0, PreviewRange::Tv), 940);
        assert_eq!(quantize_luma(0.0, PreviewRange::Pc), 0);
        assert_eq!(quantize_luma(1.0, PreviewRange::Pc), 1023);
        assert_eq!(quantize_chroma(-0.5, PreviewRange::Tv), 64);
        assert_eq!(quantize_chroma(0.0, PreviewRange::Tv), 512);
        assert_eq!(quantize_chroma(0.5, PreviewRange::Tv), 960);
        assert_eq!(quantize_chroma(0.0, PreviewRange::Pc), 512);
        assert_eq!(quantize_chroma(-0.5, PreviewRange::Pc), 0);
        assert_eq!(quantize_chroma(0.5, PreviewRange::Pc), 1023);
    }

    #[test]
    fn every_profile_maps_neutral_grey_to_neutral_chroma_and_reference_luma() {
        for profile in &PROFILES {
            let bytes: Vec<_> = [0.18f32; 3]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect();
            let mut out = Vec::new();
            let clipped = profile
                .convert_f32_frame_to_yuv10(&profile.default_output_transform(), &bytes, 1, 1, 1.0, profile.range, &mut out)
                .unwrap();
            assert_eq!(clipped, [0, 0], "{}", profile.id);
            assert_eq!(out.len(), 3);
            if profile.id == "dji-dlog2-dgamut2" {
                // The pinned rounded chain keeps a ~2.5e-4 white-axis residual, so
                // a neutral input lands one chroma code below the midpoint. This
                // frozen expectation proves the residual was not normalised away.
                assert_eq!((out[1], out[2]), (511, 512), "{} neutral chroma", profile.id);
            } else {
                assert_eq!((out[1], out[2]), (512, 512), "{} neutral chroma", profile.id);
            }
            let signal = profile.transfer.forward(
                preview::mat3_vec(&profile.composed_matrix(), [0.18; 3])[0]
                    * profile.curve_input_scale(),
            );
            // The frame adapter and the reference signal encoder agree.
            if profile.id == "dji-dlog2-dgamut2" {
                continue;
            }
            let expected = profile.encode_signal_yuv10([signal; 3], profile.range).unwrap();
            assert_eq!(&out[..], &expected, "{}", profile.id);
        }
    }

    #[test]
    fn ten_bit_frame_conversion_matches_the_documented_ycbcr_equations() {
        // Independent BT.709/BT.2020 NCL evaluation for one non-neutral AP1
        // triple; the production path must agree exactly.
        for profile in &PROFILES {
            let ap1 = [0.4f64, 0.18, 0.09];
            let target = preview::mat3_vec(&profile.composed_matrix(), ap1)
                .map(|value| value * profile.curve_input_scale());
            let (low, high) = profile.transfer.domain();
            let signal = target.map(|value| profile.transfer.forward(value.clamp(low, high)));
            let (kr, kg, kb, div_cb, div_cr) = profile.ycbcr_coding().unwrap().coefficients();
            let luma = kr * signal[0] + kg * signal[1] + kb * signal[2];
            let bytes: Vec<_> = ap1
                .into_iter()
                .map(|value| value as f32)
                .flat_map(f32::to_le_bytes)
                .collect();
            let mut out = Vec::new();
            profile
                .convert_f32_frame_to_yuv10(&profile.default_output_transform(), &bytes, 1, 1, 1.0, profile.range, &mut out)
                .unwrap();
            assert_eq!(out[0], quantize_luma(luma, profile.range), "{}", profile.id);
            assert_eq!(
                out[1],
                quantize_chroma((signal[2] - luma) / div_cb, profile.range),
                "{}",
                profile.id
            );
            assert_eq!(
                out[2],
                quantize_chroma((signal[0] - luma) / div_cr, profile.range),
                "{}",
                profile.id
            );
            // Non-neutral input must actually produce non-neutral chroma.
            assert!(out[1] != 512 || out[2] != 512, "{}", profile.id);
        }
    }

    #[test]
    fn ten_bit_conversion_counts_both_domain_boundaries_and_rejects_bad_input() {
        let profile = lookup("canon-log3-cinema-gamut-rev-eng-crm-dev").unwrap();
        let mut out = Vec::new();
        let negative: Vec<_> = [-1000f32; 3]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        let counts = profile
            .convert_f32_frame_to_yuv10(&profile.default_output_transform(), &negative, 1, 1, 1.0, profile.range, &mut out)
            .unwrap();
        assert!(counts[0] > 0 && counts[1] == 0);
        assert_eq!(
            out,
            vec![quantize_luma(0.0, profile.range), 512, 512],
            "{}",
            profile.id
        );
        let highlight: Vec<_> = [1_000_000f32; 3]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        let counts = profile
            .convert_f32_frame_to_yuv10(&profile.default_output_transform(), &highlight, 1, 1, 1.0, profile.range, &mut out)
            .unwrap();
        assert!(counts[1] > 0 && counts[0] == 0);
        assert_eq!(
            out,
            vec![quantize_luma(1.0, profile.range), 512, 512],
            "{}",
            profile.id
        );
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let bytes: Vec<_> = [bad; 3].into_iter().flat_map(f32::to_le_bytes).collect();
            assert!(profile
                .convert_f32_frame_to_yuv10(&profile.default_output_transform(), &bytes, 1, 1, 1.0, profile.range, &mut out)
                .is_err());
        }
        assert!(profile
            .convert_f32_frame_to_yuv10(&profile.default_output_transform(), &negative[..11], 1, 1, 1.0, profile.range, &mut out)
            .is_err());
        assert!(profile
            .convert_f32_frame_to_yuv10(&profile.default_output_transform(), &negative, 1, 1, 0.0, profile.range, &mut out)
            .is_err());
        let srgb = lookup("srgb-preview").unwrap();
        assert!(srgb
            .convert_f32_frame_to_yuv10(&srgb.default_output_transform(), &negative, 1, 1, 2.0, PreviewRange::Tv, &mut out)
            .is_ok());
    }

    #[test]
    fn quantization_rounds_half_away_from_zero() {
        assert_eq!(quantize_signal(0.0), 0);
        assert_eq!(quantize_signal(1.0), 255);
        assert_eq!(quantize_signal(2.5 / 255.0), 3);
        assert_eq!(quantize_signal(1.5 / 255.0), 2);
        assert_eq!(quantize_signal(-1.0), 0);
        assert_eq!(quantize_signal(2.0), 255);
    }

    #[test]
    fn input_domain_clips_explicitly_at_the_official_endpoints() {
        for profile in &PROFILES {
            let (low, high) = profile.transfer.domain();
            assert!(close(profile.transfer.forward(low), 0.0, 1e-5));
            assert!(close(profile.transfer.forward(high), 1.0, 1e-5));
            assert_eq!(profile.encode_target_linear([low - 1.0; 3]), [0, 0, 0]);
            assert_eq!(
                profile.encode_target_linear([high + 1.0; 3]),
                [255, 255, 255]
            );
        }
        // Camera Log curves may legally encode slight negative scene values;
        // Canon Log 2 maps -0.01 to a code below its 0% black code 24.
        let canon = lookup("canon-log2-cinema-gamut").expect("canon");
        assert!(canon.encode_target_linear([-0.01; 3])[0] < 24);
    }

    #[test]
    fn frame_conversion_reads_little_endian_rgb16() {
        let profile = lookup("sony-slog3-sgamut3").expect("sony");
        let mut input = Vec::new();
        for _ in 0..3 {
            input.extend_from_slice(&2949u16.to_le_bytes());
        }
        for _ in 0..3 {
            input.extend_from_slice(&0u16.to_le_bytes());
        }
        let mut out = Vec::new();
        profile
            .convert_frame(&input, 2, 1, &mut out)
            .expect("convert");
        assert_eq!(out.len(), 6);
        let expected = profile.encode_target_linear([2949.0 / 16384.0; 3]);
        assert_eq!(&out[0..3], &expected);
        assert_eq!(&out[3..6], &[24, 24, 24]);

        let error = profile
            .convert_frame(&input[..11], 2, 1, &mut out)
            .expect_err("must reject");
        assert!(error.contains("does not match"));
    }

    #[test]
    fn contract_json_records_sources_domain_range_and_tag_policy() {
        for profile in &PROFILES {
            let contract = profile.contract_json(&profile.default_output_transform());
            assert_eq!(
                contract.get("profile").and_then(|value| value.as_str()),
                Some(profile.id)
            );
            let transfer = contract.get("transfer").expect("transfer");
            assert_eq!(
                transfer
                    .get("source")
                    .and_then(|source| source.get("url"))
                    .and_then(|url| url.as_str()),
                Some(profile.transfer.url())
            );
            let domain = contract.get("input_domain").expect("domain");
            assert!(domain.get("linear_min").is_some());
            assert!(domain.get("linear_max").is_some());
            let quantization = contract.get("quantization").expect("quantization");
            assert!(matches!(
                quantization.get("dither"),
                Some(Json::Bool(false))
            ));
            assert_eq!(
                quantization.get("bits").and_then(Json::as_u64),
                Some(u64::from(SIGNAL_BITS))
            );
            assert_eq!(
                quantization.get("handoff").and_then(Json::as_str),
                Some("planar yuv444p10le target signal; the single quantization boundary before FFmpeg")
            );
            let tradeoff = quantization
                .get("tradeoff")
                .and_then(Json::as_str)
                .expect("tradeoff");
            for required in ["10-bit", "handoff", "lossy", "same 10-bit buffer"] {
                assert!(tradeoff.contains(required), "{}: {tradeoff}", profile.id);
            }
            let encoding = contract.get("encoding").expect("encoding");
            assert_eq!(
                encoding.get("range").and_then(|value| value.as_str()),
                Some(profile.range.as_str())
            );
            assert_eq!(
                encoding
                    .get("ycbcr_matrix")
                    .and_then(|value| value.as_str()),
                Some(profile.ycbcr_matrix)
            );
            assert_eq!(
                encoding
                    .get("signal_pix_fmt")
                    .and_then(|value| value.as_str()),
                Some("yuv444p10le")
            );
            let tags = encoding.get("tag_policy").expect("tags");
            assert_eq!(tags.get("mode").and_then(Json::as_str), Some("per-field"));
            assert_eq!(
                tags.get("colorspace").and_then(Json::as_str),
                Some(profile.ycbcr_matrix)
            );
            if profile.tag_policy != TagPolicy::SRGB {
                assert!(profile.ycbcr_reason.contains("Rawtl delivery convention"));
                assert!(profile.ycbcr_reason.contains("not a vendor requirement"));
                assert!(profile.range_reason.contains("not a vendor mandate"));
                assert_eq!(profile.tag_policy.transfer, 2);
                assert_eq!(profile.tag_policy.matrix, 9);
                assert_eq!(
                    profile.tag_policy.primaries,
                    if profile.id == "nikon-nlog-bt2020" {
                        9
                    } else {
                        2
                    }
                );
                assert_eq!(
                    encoding
                        .get("delivery_background")
                        .and_then(Json::as_array)
                        .unwrap()
                        .len(),
                    3
                );
            }
        }
        let srgb = lookup("srgb-preview").expect("srgb");
        let srgb_contract = srgb.contract_json(&srgb.default_output_transform());
        let srgb = &srgb_contract;
        let matrices = srgb.get("matrices").expect("matrices");
        for key in [
            "acescg_ap1_d60_to_xyz_d60",
            "bradford_cat_d60_to_d65",
            "xyz_d65_to_linear_srgb",
            "composed_ap1_d60_to_linear_srgb_d65",
        ] {
            assert_eq!(
                matrices.get(key).expect(key).as_array().expect(key).len(),
                3
            );
        }
    }
}

#[cfg(test)]
mod delivered_range_tests {
    use super::*;

    #[test]
    fn sony_green_reference_patch_survives_limited_delivery() {
        // The profile convention is full range, but the ProRes encoder node
        // delivers limited range. The handoff codes must follow the delivered
        // range so a video-range decoder recovers the independent reference.
        let profile = lookup("sony-slog3-sgamut3").unwrap();
        let bytes: Vec<_> = [0f32, 1.0, 0.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        let target = preview::mat3_vec(&profile.composed_matrix(), [0.0, 1.0, 0.0]);
        let (low, high) = profile.transfer.domain();
        let signal = target.map(|value| profile.transfer.forward(value.clamp(low, high)));
        assert_eq!(
            (signal.map(|value| (value * 255.0).round())),
            [59.0, 147.0, 20.0]
        );
        let mut limited = Vec::new();
        profile
            .convert_f32_frame_to_yuv10(&profile.default_output_transform(), &bytes, 1, 1, 1.0, PreviewRange::Tv, &mut limited)
            .unwrap();
        let mut full = Vec::new();
        profile
            .convert_f32_frame_to_yuv10(&profile.default_output_transform(), &bytes, 1, 1, 1.0, PreviewRange::Pc, &mut full)
            .unwrap();
        // Y=64+876*Y', Cb/Cr=512+896*Cb/Cr for the delivered (limited) range.
        let y = 0.2627 * signal[0] + 0.6780 * signal[1] + 0.0593 * signal[2];
        assert_eq!(
            limited,
            vec![
                (64.0 + 876.0 * y).round() as u16,
                (512.0 + 896.0 * (signal[2] - y) / 1.8814).round() as u16,
                (512.0 + 896.0 * (signal[0] - y) / 1.4746).round() as u16,
            ]
        );
        assert_eq!(limited, vec![464, 332, 374]);
        assert_eq!(full, vec![467, 306, 354]);
        assert_ne!(limited, full);
    }
}

#[cfg(test)]
mod camera_recipe_tests {
    use super::*;
    #[test]
    fn selection_table_and_curve_units_are_independent() {
        for profile in PROFILES.iter() {
            for cr3 in [false, true] {
                let canon = cr3 && profile.transfer == Transfer::CanonLog3RevEng;
                assert_eq!(CameraColor::Auto.resolve(profile, cr3).unwrap(), if canon { CameraRecipe::CanonClog3 } else { CameraRecipe::Generic });
                assert_eq!(CameraColor::Generic.resolve(profile, cr3).unwrap(), CameraRecipe::Generic);
                assert_eq!(CameraColor::CanonClog3.resolve(profile, cr3).is_ok(), canon);
                assert_eq!(profile.curve_input_scale(), match profile.transfer { Transfer::CanonLog2RevEng | Transfer::CanonLog3RevEng => 0.9, _ => 1.0 });
            }
        }
    }
    #[test]
    fn curve_input_units_and_user_gain_are_applied_once() {
        for (id, code) in [("canon-log2-cinema-gamut-rev-eng-crm-dev", 407u16),
                           ("canon-log3-cinema-gamut-rev-eng-crm-dev", 351u16)] {
            let p = lookup(id).unwrap();
            // Independent published/reconstructed 18% reference, full-range 10-bit.
            for (scene, gain) in [(0.2f32, 1.0), (0.1, 2.0), (0.4, 0.5)] {
                let bytes: Vec<_> = [scene; 3].into_iter().flat_map(f32::to_le_bytes).collect();
                let mut out = Vec::new();
                p.convert_f32_frame_to_yuv10(&p.default_output_transform(), &bytes, 1, 1, gain, PreviewRange::Pc, &mut out).unwrap();
                assert_eq!(out, [code, 512, 512], "{id} scene={scene} gain={gain}");
            }
        }
    }
    #[test]
    fn every_profile_accepts_only_positive_finite_reference_gain_in_both_paths() {
        let bytes: Vec<_> = [0.1f32; 3].into_iter().flat_map(f32::to_le_bytes).collect();
        for profile in PROFILES.iter() {
            for gain in [0.5, 1.0, 2.0] {
                profile.convert_f32_frame(&bytes, 1, 1, gain, &mut Vec::new()).unwrap();
                profile.convert_f32_frame_to_yuv10(&profile.default_output_transform(), &bytes, 1, 1, gain, PreviewRange::Pc, &mut Vec::new()).unwrap();
            }
            for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
                assert!(profile.convert_f32_frame(&bytes, 1, 1, bad, &mut Vec::new()).is_err());
                assert!(profile.convert_f32_frame_to_yuv10(&profile.default_output_transform(), &bytes, 1, 1, bad, PreviewRange::Pc, &mut Vec::new()).is_err());
            }
        }
    }
}

#[cfg(test)]
mod output_cat_tests {
    use super::*;

    fn assert_close(observed: f64, expected: f64, tolerance: f64, label: &str) {
        let difference = (observed - expected).abs();
        assert!(
            difference <= tolerance || difference <= tolerance * expected.abs(),
            "{label}: {observed:.17} vs {expected:.17} (abs {difference:e})"
        );
    }

    fn assert_matrix_close(
        observed: [[f64; 3]; 3],
        expected: [[f64; 3]; 3],
        tolerance: f64,
        label: &str,
    ) {
        for row in 0..3 {
            for column in 0..3 {
                assert_close(
                    observed[row][column],
                    expected[row][column],
                    tolerance,
                    &format!("{label} [{row}][{column}]"),
                );
            }
        }
    }

    /// Read one serialized matrix back as f64 values. `Json::Float` keeps the
    /// exact f64 value, so equality here is bit-identity of the published
    /// numbers, not a re-derivation.
    fn json_matrix(matrix: &Json, label: &str) -> [[f64; 3]; 3] {
        let rows = matrix
            .as_array()
            .unwrap_or_else(|| panic!("{label} is not an array"));
        assert_eq!(rows.len(), 3, "{label} row count");
        let mut out = [[0.0; 3]; 3];
        for (row, values) in rows.iter().enumerate() {
            let values = values
                .as_array()
                .unwrap_or_else(|| panic!("{label} row {row} is not an array"));
            assert_eq!(values.len(), 3, "{label} row {row} width");
            for column in 0..3 {
                out[row][column] = values[column]
                    .as_f64()
                    .unwrap_or_else(|| panic!("{label} [{row}][{column}] is not a number"));
            }
        }
        out
    }

    /// sRGB primaries normalised to the CIE D50 white; an independent fixture
    /// for a non-D65 target white. Matrix values were derived with exact
    /// rational arithmetic (cat_oracle.py), not with production code.
    static D50_TEST_GAMUT: Gamut = Gamut {
        name: "test fixture: sRGB primaries / CIE D50 white",
        primaries_xy: [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]],
        white_xy: [0.3457, 0.3585],
        rgb_to_xyz: [
            [0.4851629670705136, 0.348865475850403, 0.13026723350865105],
            [0.25016215489573357, 0.697730951700806, 0.052106893403460426],
            [0.022742014081430322, 0.11628849195013434, 0.6860740964788956],
        ],
        xyz_to_rgb: [
            [2.7548396630707845, -1.30678291709768, -0.4238214866262745],
            [-0.9934670220281292, 1.922851775736947, 0.042593603493674116],
            [0.07707356924796838, -0.2826030872425507, 1.4643978157113986],
        ],
        provenance: "test fixture: published sRGB primaries normalised to CIE D50",
    };

    /// A pinned, vendor-rounded reference chain: four-decimal values that no
    /// re-derivation would produce. The resolver must return it verbatim.
    const PINNED_CHAIN: [[f64; 3]; 3] = [
        [1.7051, -0.6218, -0.0833],
        [-0.1303, 1.1408, -0.0105],
        [-0.0240, -0.1290, 1.1530],
    ];
    const PINNED_BRADFORD_D60_D50: [[f64; 3]; 3] = [
        [1.0342, 0.0168, -0.0375],
        [0.0217, 0.9922, -0.0127],
        [-0.0069, 0.0113, 0.8132],
    ];
    const PINNED_CAT02_D60_D65: [[f64; 3]; 3] = [
        [0.9882, -0.0079, 0.0168],
        [-0.0057, 0.9987, 0.0067],
        [0.0004, 0.0011, 1.0781],
    ];

    fn test_profile(
        id: &'static str,
        gamut: &'static Gamut,
        output_cat: OutputCatDefault,
    ) -> ColorProfile {
        ColorProfile {
            id,
            transfer: Transfer::Srgb,
            gamut,
            range: PreviewRange::Tv,
            ycbcr_matrix: "bt709",
            range_reason: "test fixture",
            ycbcr_reason: "test fixture",
            tag_policy: TagPolicy::SRGB,
            output_cat,
        }
    }

    #[test]
    fn every_registered_profile_keeps_its_fixed_reference_realization() {
        let registry = CatRegistry::standard();
        for profile in PROFILES.iter() {
            let output = profile
                .resolve_output_cat(OutputCatSelection::Profile, &registry)
                .unwrap();
            assert_eq!(output.method, profile.output_cat.method, "{}", profile.id);
            assert_eq!(output.realization, OutputCatRealization::Reference);
            assert_eq!(output.target_white_xy, profile.gamut.white_xy);
            if let Some(pinned) = profile.output_cat.composed_override {
                // A profile with a pinned vendor chain reproduces it verbatim; its
                // own test owns the chain/provenance assertions.
                assert_eq!(
                    output.composed_matrix, pinned,
                    "{} must keep its pinned composed chain",
                    profile.id
                );
                assert_eq!(
                    output.cat_matrix, profile.output_cat.adaptation,
                    "{} must report its declared adaptation",
                    profile.id
                );
                continue;
            }
            // The historic composition, expressed with the same constants and
            // multiplication order, must be reproduced bit-for-bit.
            let historic = cat::mat3_mul(
                &profile.gamut.xyz_to_rgb,
                &cat::mat3_mul(&preview::BRADFORD_D60_TO_D65, &preview::AP1_D60_TO_XYZ_D60),
            );
            for row in 0..3 {
                for column in 0..3 {
                    assert_eq!(
                        output.composed_matrix[row][column].to_bits(),
                        historic[row][column].to_bits(),
                        "{} composed [{row}][{column}]",
                        profile.id
                    );
                    assert_eq!(
                        output.cat_matrix[row][column].to_bits(),
                        preview::BRADFORD_D60_TO_D65[row][column].to_bits(),
                        "{} cat [{row}][{column}]",
                        profile.id
                    );
                }
            }
            // White stays neutral for the Bradford reference realization; the
            // pinned-chain profile owns its residual expectation in its own test.
            let white = cat::mat3_vec(&output.composed_matrix, [1.0; 3]);
            for channel in 0..3 {
                assert_close(white[channel], 1.0, 1e-9, &format!("{} white {channel}", profile.id));
            }
        }
    }

    #[test]
    fn explicit_same_value_selection_is_idempotent() {
        let registry = CatRegistry::standard();
        for profile in PROFILES.iter() {
            let default = profile
                .resolve_output_cat(OutputCatSelection::Profile, &registry)
                .unwrap();
            let same = profile
                .resolve_output_cat(
                    OutputCatSelection::Method(profile.output_cat.method),
                    &registry,
                )
                .unwrap();
            assert_eq!(same.method, default.method, "{}", profile.id);
            assert_eq!(same.realization, OutputCatRealization::Reference);
            assert_eq!(same.fingerprint, default.fingerprint, "{}", profile.id);
            assert_eq!(same.composed_matrix, default.composed_matrix);
            assert_eq!(same.cat_matrix, default.cat_matrix);
        }
    }

    #[test]
    fn srgb_reference_and_custom_transforms_match_the_independent_oracle() {
        // Frozen exact-rational oracle (cat_oracle.py). The reference path keeps
        // the published 12-decimal Bradford constant; the custom path uses the
        // standard derivation with the CAT02 cone matrix.
        let reference_expected = [
            [1.7050509926055784, -0.6217921204861147, -0.08325887199715537],
            [-0.1302564174633593, 1.140804736511389, -0.010548319079933866],
            [-0.024003356819728795, -0.12896897609304023, 1.1529723328565],
        ];
        let custom_expected = [
            [1.7050796435565116, -0.6242334613015463, -0.08084618213163959],
            [-0.12970053007433263, 1.138468546504952, -0.008768016463546534],
            [-0.024166344958335986, -0.12461415808102454, 1.1487805029813045],
        ];
        let profile = lookup("srgb-preview").unwrap();
        let registry = CatRegistry::standard();
        let reference = profile
            .resolve_output_cat(OutputCatSelection::Profile, &registry)
            .unwrap();
        assert_matrix_close(reference.composed_matrix, reference_expected, 1e-10, "srgb reference");
        // Bit-identical to the pre-existing preview path.
        let legacy = preview::ap1_to_linear_srgb_matrix();
        for row in 0..3 {
            for column in 0..3 {
                assert_eq!(
                    reference.composed_matrix[row][column].to_bits(),
                    legacy[row][column].to_bits(),
                    "srgb legacy [{row}][{column}]"
                );
            }
        }

        let custom = profile
            .resolve_output_cat(OutputCatSelection::Method("cat02"), &registry)
            .unwrap();
        assert_eq!(custom.realization, OutputCatRealization::CustomAdaptation);
        assert_eq!(custom.method, "cat02");
        assert_ne!(custom.fingerprint, reference.fingerprint);
        assert_matrix_close(custom.composed_matrix, custom_expected, 1e-10, "srgb custom cat02");
        assert_matrix_close(
            custom.cat_matrix,
            [
                [0.9882325471218135, -0.007885454399865814, 0.016757635451623946],
                [-0.005693191933411921, 0.9986923398294999, 0.006672372202884866],
                [0.0003532731423422887, 0.0011239768688093192, 1.0780829488488732],
            ],
            1e-10,
            "srgb custom cat02 adaptation",
        );

        // Non-neutral, signed and HDR probes; the oracle values are the exact
        // linear results of the two composed matrices above.
        for (ap1, reference_linear, custom_linear) in [
            (
                [0.4, 0.18, 0.09],
                [0.5626045168749867, 0.15229293686951223, 0.07095175153244623],
                [0.5623936779964788, 0.1522550048594391, 0.0712931588303986],
            ),
            (
                [0.05, 0.1, 1.0],
                [-0.060185534415487914, 0.09701933369803706, 1.1388752674062095],
                [-0.05801554608396864, 0.09859381168323203, 1.1351107699252854],
            ),
            (
                [-0.02, 0.5, 2.0],
                [-0.5115148240894796, 0.5519108584450939, 2.2419402448028745],
                [-0.5079106877851826, 0.5542922509268695, 2.235737253821264],
            ),
        ] {
            let observed = cat::mat3_vec(&reference.composed_matrix, ap1);
            let observed_custom = cat::mat3_vec(&custom.composed_matrix, ap1);
            for channel in 0..3 {
                assert_close(
                    observed[channel],
                    reference_linear[channel],
                    1e-10,
                    &format!("reference probe {ap1:?} channel {channel}"),
                );
                assert_close(
                    observed_custom[channel],
                    custom_linear[channel],
                    1e-10,
                    &format!("custom probe {ap1:?} channel {channel}"),
                );
            }
        }
        // White stays neutral through the custom derivation as well.
        let white = cat::mat3_vec(&custom.composed_matrix, [1.0; 3]);
        for channel in 0..3 {
            assert_close(white[channel], 1.0, 1e-9, "custom white");
        }
    }

    #[test]
    fn pixel_conversion_uses_the_resolved_transform_not_a_label() {
        let profile = lookup("srgb-preview").unwrap();
        let registry = CatRegistry::standard();
        // A saturated AP1 green-blue: Bradford and CAT02 differ by ~9 Cb codes,
        // far above quantization noise.
        let ap1 = [0.2f32, 0.4, 0.05];
        let bytes: Vec<_> = ap1.into_iter().flat_map(f32::to_le_bytes).collect();
        let convert = |output: &OutputTransform| {
            let mut out = Vec::new();
            profile
                .convert_f32_frame_to_yuv10(output, &bytes, 1, 1, 1.0, profile.range, &mut out)
                .unwrap();
            out
        };
        let default = profile.default_output_transform();
        let same = profile
            .resolve_output_cat(OutputCatSelection::Method("bradford"), &registry)
            .unwrap();
        let custom = profile
            .resolve_output_cat(OutputCatSelection::Method("cat02"), &registry)
            .unwrap();
        let default_codes = convert(&default);
        assert_eq!(default_codes, convert(&same), "same-value selection must keep the reference pixels");
        let custom_codes = convert(&custom);
        assert_ne!(default_codes, custom_codes, "a different CAT must change pixels");
        let difference = default_codes
            .iter()
            .zip(&custom_codes)
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap();
        assert!(difference >= 5, "expected a decisive pixel change, got {difference} codes: {default_codes:?} vs {custom_codes:?}");

        // A transform resolved for another profile cannot be applied here.
        let other = lookup("sony-slog3-sgamut3").unwrap().default_output_transform();
        let mut out = Vec::new();
        assert!(profile
            .convert_f32_frame_to_yuv10(&other, &bytes, 1, 1, 1.0, profile.range, &mut out)
            .is_err());
    }

    #[test]
    fn fixed_reference_chain_and_new_cat_registration_are_declaration_only() {
        // A new profile declares a fixed reference chain. It is returned
        // verbatim (no re-derivation) and same-value selection keeps it.
        let pinned = test_profile(
            "test-pinned-d50",
            &D50_TEST_GAMUT,
            OutputCatDefault {
                method: "bradford",
                adaptation: PINNED_BRADFORD_D60_D50,
                composed_override: Some(PINNED_CHAIN),
                provenance: "test fixture: pinned four-decimal chain",
            },
        );
        let registry = CatRegistry::standard();
        let reference = pinned
            .resolve_output_cat(OutputCatSelection::Profile, &registry)
            .unwrap();
        assert_eq!(reference.realization, OutputCatRealization::Reference);
        assert_eq!(reference.target_white_xy, [0.3457, 0.3585]);
        assert_eq!(reference.composed_matrix, PINNED_CHAIN);
        assert_eq!(reference.cat_matrix, PINNED_BRADFORD_D60_D50);
        let same = pinned
            .resolve_output_cat(OutputCatSelection::Method("bradford"), &registry)
            .unwrap();
        assert_eq!(same.composed_matrix, PINNED_CHAIN);
        assert_eq!(same.fingerprint, reference.fingerprint);

        // A different explicit method uses the standard derivation and respects
        // the D50 target white (the frozen CAT02 D60->D50 oracle).
        let custom = pinned
            .resolve_output_cat(OutputCatSelection::Method("cat02"), &registry)
            .unwrap();
        assert_eq!(custom.realization, OutputCatRealization::CustomAdaptation);
        assert_matrix_close(
            custom.cat_matrix,
            [
                [1.030110927178595, 0.0225701512453698, -0.03925913684386439],
                [0.016221083785699784, 1.0003478529389636, -0.015662579580460134],
                [-0.0008625790421145067, -0.002550635686881516, 0.8212294692745158],
            ],
            1e-10,
            "custom CAT02 D60->D50",
        );
        assert_ne!(custom.composed_matrix, PINNED_CHAIN);

        // A profile whose default is a pinned CAT02 chain (the shape the DLog2
        // section will register): `cat02` keeps the reference chain, `bradford`
        // is custom.
        let cat02_default = test_profile(
            "test-pinned-cat02",
            &SRGB_GAMUT,
            OutputCatDefault {
                method: "cat02",
                adaptation: PINNED_CAT02_D60_D65,
                composed_override: Some(PINNED_CHAIN),
                provenance: "test fixture: pinned CAT02 chain",
            },
        );
        let reference = cat02_default
            .resolve_output_cat(OutputCatSelection::Profile, &registry)
            .unwrap();
        assert_eq!(reference.realization, OutputCatRealization::Reference);
        assert_eq!(reference.composed_matrix, PINNED_CHAIN);
        let explicit = cat02_default
            .resolve_output_cat(OutputCatSelection::Method("cat02"), &registry)
            .unwrap();
        assert_eq!(explicit.composed_matrix, PINNED_CHAIN);
        assert_eq!(explicit.fingerprint, reference.fingerprint);
        assert_eq!(
            cat02_default
                .resolve_output_cat(OutputCatSelection::Method("bradford"), &registry)
                .unwrap()
                .realization,
            OutputCatRealization::CustomAdaptation
        );

        // Adding an algorithm is registration only: no Transfer variant and no
        // native change. The registered cone matrix drives the derivation.
        let mut extended = CatRegistry::standard();
        extended
            .register(CatAlgorithm {
                token: "test-diagonal",
                name: "test diagonal cone",
                cone_matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                provenance: "test fixture",
            })
            .unwrap();
        let output = pinned
            .resolve_output_cat(OutputCatSelection::Method("test-diagonal"), &extended)
            .unwrap();
        assert_eq!(output.method, "test-diagonal");
        assert_eq!(output.realization, OutputCatRealization::CustomAdaptation);
        let algorithm = extended.lookup("test-diagonal").unwrap();
        let derived = cat::adaptation_matrix(algorithm, cat::ACES_WHITE_XY, [0.3457, 0.3585])
            .unwrap();
        assert_eq!(output.cat_matrix, derived);
        assert_ne!(output.cat_matrix, reference.cat_matrix);
    }

    #[test]
    fn contract_reports_the_actual_transform() {
        let profile = lookup("srgb-preview").unwrap();
        let registry = CatRegistry::standard();
        let reference = profile.default_output_transform();
        let contract = profile.contract_json(&reference);
        let output_cat = contract.get("output_cat").unwrap();
        assert_eq!(output_cat.get("requested").and_then(Json::as_str), Some("profile"));
        assert_eq!(output_cat.get("method").and_then(Json::as_str), Some("bradford"));
        assert_eq!(
            output_cat.get("realization").and_then(Json::as_str),
            Some("reference")
        );
        assert_eq!(output_cat.get("reference").and_then(Json::as_bool), Some(true));
        assert_eq!(
            output_cat.get("fingerprint").and_then(Json::as_str),
            Some(reference.fingerprint.as_str())
        );
        let matrices = contract.get("matrices").unwrap();
        assert!(matrices.get("output_cat").is_some());
        assert!(matrices.get("bradford_cat_d60_to_d65").is_some());
        assert!(matrices.get("composed_ap1_d60_to_target_linear").is_some());
        // The legacy composed key describes the effective conversion; on the
        // reference path it is the historic Bradford composition exactly.
        let reference_composed = json_matrix(
            matrices
                .get("composed_ap1_d60_to_target_linear")
                .expect("generic composed matrix"),
            "reference generic composed",
        );
        let reference_legacy = json_matrix(
            matrices
                .get("composed_ap1_d60_to_linear_srgb_d65")
                .expect("legacy composed matrix"),
            "reference legacy composed",
        );
        assert_eq!(reference_legacy, reference_composed);
        assert_eq!(reference_legacy, preview::ap1_to_linear_srgb_matrix());
        let reference_fixture = [
            [1.7050509926055784, -0.6217921204861147, -0.08325887199715537],
            [-0.1302564174633593, 1.140804736511389, -0.010548319079933866],
            [-0.024003356819728795, -0.12896897609304023, 1.1529723328565],
        ];
        assert_matrix_close(reference_legacy, reference_fixture, 1e-10, "reference legacy composed");
        // The legacy XYZ stage is the gamut's fixed target decode and stays the
        // same under any CAT; it must still match the generic target matrix.
        assert_eq!(
            json_matrix(
                matrices.get("xyz_d65_to_linear_srgb").expect("legacy xyz"),
                "legacy xyz",
            ),
            json_matrix(
                matrices.get("xyz_to_target_linear").expect("generic xyz"),
                "generic xyz",
            )
        );

        let custom = profile
            .resolve_output_cat(OutputCatSelection::Method("cat02"), &registry)
            .unwrap();
        let contract = profile.contract_json(&custom);
        let output_cat = contract.get("output_cat").unwrap();
        assert_eq!(output_cat.get("requested").and_then(Json::as_str), Some("cat02"));
        assert_eq!(output_cat.get("method").and_then(Json::as_str), Some("cat02"));
        assert_eq!(
            output_cat.get("realization").and_then(Json::as_str),
            Some("custom-adaptation")
        );
        assert_eq!(
            output_cat.get("custom_adaptation").and_then(Json::as_bool),
            Some(true)
        );
        assert_ne!(
            output_cat.get("fingerprint").and_then(Json::as_str),
            Some(reference.fingerprint.as_str())
        );
        let matrices = contract.get("matrices").unwrap();
        // A non-Bradford realization must not carry the legacy Bradford field.
        assert!(matrices.get("bradford_cat_d60_to_d65").is_none());
        assert!(matrices.get("output_cat").is_some());
        // Every composed key must publish the CAT02-derived effective matrix,
        // not the reference helper (B1-01: two incompatible compositions under
        // one conversion). The custom composed matrix differs from the Bradford
        // fixture, so an unconditional default-path helper fails here.
        let custom_expected = [
            [1.7050796435565116, -0.6242334613015463, -0.08084618213163959],
            [-0.12970053007433263, 1.138468546504952, -0.008768016463546534],
            [-0.024166344958335986, -0.12461415808102454, 1.1487805029813045],
        ];
        let custom_composed = json_matrix(
            matrices
                .get("composed_ap1_d60_to_target_linear")
                .expect("custom generic composed"),
            "custom generic composed",
        );
        let custom_legacy = json_matrix(
            matrices
                .get("composed_ap1_d60_to_linear_srgb_d65")
                .expect("custom legacy composed"),
            "custom legacy composed",
        );
        assert_eq!(custom_legacy, custom_composed);
        assert_matrix_close(custom_legacy, custom_expected, 1e-10, "custom legacy composed");
        assert_ne!(
            custom_legacy, reference_fixture,
            "a custom realization must not publish the Bradford composition"
        );
        assert_matrix_close(
            custom_legacy,
            custom.composed_matrix,
            0.0,
            "custom legacy equals the resolved transform",
        );
        // The CAT-independent target decode is still truthful.
        assert_eq!(
            json_matrix(
                matrices.get("xyz_d65_to_linear_srgb").expect("legacy xyz"),
                "legacy xyz",
            ),
            json_matrix(
                matrices.get("xyz_to_target_linear").expect("generic xyz"),
                "generic xyz",
            )
        );
    }

    #[test]
    fn invalid_registrations_and_selections_are_rejected() {
        let registry = CatRegistry::standard();
        let broken = test_profile(
            "test-broken-reference",
            &SRGB_GAMUT,
            OutputCatDefault {
                method: "bradford",
                adaptation: [
                    [f64::NAN, 0.0, 0.0],
                    [0.0, 1.0, 0.0],
                    [0.0, 0.0, 1.0],
                ],
                composed_override: None,
                provenance: "test fixture",
            },
        );
        assert!(broken
            .resolve_output_cat(OutputCatSelection::Profile, &registry)
            .is_err());

        let singular_reference = test_profile(
            "test-singular-reference",
            &SRGB_GAMUT,
            OutputCatDefault {
                method: "bradford",
                adaptation: [
                    [1.0, 1.0, 0.0],
                    [1.0, 1.0, 0.0],
                    [0.0, 0.0, 1.0],
                ],
                composed_override: None,
                provenance: "test fixture",
            },
        );
        assert!(singular_reference
            .resolve_output_cat(OutputCatSelection::Profile, &registry)
            .is_err());

        let unregistered = test_profile(
            "test-unregistered-default",
            &SRGB_GAMUT,
            OutputCatDefault {
                method: "not-registered",
                adaptation: PINNED_BRADFORD_D60_D50,
                composed_override: Some(PINNED_CHAIN),
                provenance: "test fixture",
            },
        );
        assert!(unregistered
            .resolve_output_cat(OutputCatSelection::Profile, &registry)
            .is_err());

        let profile = lookup("srgb-preview").unwrap();
        assert!(profile
            .resolve_output_cat(OutputCatSelection::Method("not-registered"), &registry)
            .is_err());
        assert!(profile
            .resolve_output_cat(OutputCatSelection::Method(""), &registry)
            .is_err());

        // A singular target primaries matrix rejects the custom derivation at
        // startup instead of producing unbounded coefficients.
        static SINGULAR_GAMUT: Gamut = Gamut {
            name: "test singular",
            primaries_xy: [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]],
            white_xy: [0.3127, 0.3290],
            rgb_to_xyz: [[1.0; 3]; 3],
            xyz_to_rgb: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            provenance: "test fixture",
        };
        let singular = test_profile("test-singular", &SINGULAR_GAMUT, PREVIEW_OUTPUT_CAT);
        assert!(singular
            .resolve_output_cat(OutputCatSelection::Method("cat02"), &registry)
            .is_err());
    }

    // -----------------------------------------------------------------------
    // D-Log 2 / D-Gamut 2. Every expected value below is a frozen literal from
    // the independent exact-arithmetic oracle
    // (`work/dlog2-delivery-impl/dlog2_oracle.py`, Decimal prec=80); no
    // production function generated its own expectation.
    // -----------------------------------------------------------------------

    const DLOG2_PROFILE: &str = "dji-dlog2-dgamut2";

    /// `(x, E(x), D(E(x)))` evaluated with Decimal arithmetic.
    const DLOG2_CURVE_TABLE: [(f64, f64, f64); 46] = [
        (-0.02, 0.0033423899093129347, -0.02),
        (-0.018, 0.009264260400297436, -0.018),
        (0.0, 0.06256109481915793, 0.0),
        (0.028961694254132, 0.14831479610538775, 0.028961694254132),
        (0.028961695254132, 0.14831479906632428, 0.028961695254132),
        (0.028961696254132, 0.14831480202725947, 0.028961696254132),
        (0.028966695254132, 0.1483296024647456, 0.028966695254132),
        (0.179999999, 0.3049853367669925, 0.179999999),
        (0.18, 0.3049853372434017, 0.17999999999999944),
        (0.180000001, 0.3049853377198112, 0.180000001),
        (0.180005, 0.30498771925871637, 0.180005),
        (0.5, 0.3936579476875581, 0.5),
        (0.9, 0.4452638679050807, 0.9),
        (1.0, 0.45453920205905907, 1.0),
        (7.2, 0.6289635845258591, 7.2),
        (14.4, 0.6903303269095441, 14.4),
        (46.08, 0.7933443332817633, 46.08),
        (100.0, 0.8619739998501822, 100.0),
        (184.32, 0.9161423531223952, 184.32),
        (300.0, 0.9592924100042289, 300.0),
        (474.999999, 0.999999999813505, 474.999999),
        (475.0, 1.0, 475.0),
        (150.39779326365692, 0.8981254762291176, 150.39779326365692),
        (328.8269775283602, 0.9674199748426283, 328.8269775283602),
        (282.1527625083657, 0.9538591874283551, 282.1527625083657),
        (51.16504897258945, 0.8026161954492318, 51.16504897258945),
        (23.4394745314631, 0.7334748474646435, 23.4394745314631),
        (449.5213767104369, 0.9951161804220953, 449.5213767104369),
        (75.66944227286588, 0.8372782264263015, 75.66944227286588),
        (399.17844737217223, 0.9845945114300898, 399.17844737217223),
        (6.806006529087203, 0.6239825820646916, 6.806006529087203),
        (364.2272219149476, 0.9764774246557627, 364.2272219149476),
        (329.2867154608838, 0.9675437398443811, 329.2867154608838),
        (159.16108294471684, 0.9031421918956427, 159.16108294471684),
        (467.7246328509946, 0.9986326823471917, 467.7246328509946),
        (398.06072613813484, 0.9843461209794062, 398.06072613813484),
        (209.9598677105037, 0.9276797431373521, 209.9598677105037),
        (10.140500028026523, 0.6592792983449932, 10.140500028026523),
        (84.94667467453363, 0.8475224011990078, 84.94667467453363),
        (161.059519279585, 0.9041925368309099, 161.059519279585),
        (315.9380390860737, 0.9638778672546596, 315.9380390860737),
        (121.84617753719733, 0.8794767400364846, 121.84617753719733),
        (171.9687241793418, 0.9099981506690425, 171.9687241793418),
        (116.04251215356985, 0.8751537000147835, 116.04251215356985),
        (339.2393206459607, 0.9701815226297833, 339.2393206459607),
        (411.24517361532673, 0.9872326603670275, 411.24517361532673),
    ];

    /// `AP1/D60 -> D-Gamut 2` pinned chain (Decimal chain, rounded to f64).
    const DLOG2_AP1_TO_DGAMUT2: [[f64; 3]; 3] = [
        [0.9345061440003459, -0.0022031394702776525, 0.06794785515875235],
        [0.04075107103644273, 0.8034933746397581, 0.15577128678091975],
        [-0.005058772922983332, 0.0047227291779441995, 1.0001502715262953],
    ];

    fn assert_oracle_close(observed: f64, expected: f64, absolute: f64, relative: f64, label: &str) {
        let difference = (observed - expected).abs();
        assert!(
            difference <= absolute || difference <= relative * expected.abs(),
            "{label}: {observed:.17} vs {expected:.17} (abs {difference:e})"
        );
    }

    #[test]
    fn dji_dlog2_curve_matches_the_independent_high_precision_oracle() {
        let transfer = Transfer::DjiDLog2;
        // The pinned 2^A literal is the oracle's rounded f64; the platform
        // `exp2` may differ by a few ulp, far inside the curve budget.
        assert!((DJI_DLOG2_TWO_POW_A - DJI_DLOG2_A.exp2()).abs() <= 1e-10);
        for (linear, signal, roundtrip) in DLOG2_CURVE_TABLE {
            let observed = transfer.forward(linear);
            assert_oracle_close(
                observed,
                signal,
                1e-12,
                1e-12,
                &format!("E({linear})"),
            );
            let back = transfer.inverse(signal);
            assert_oracle_close(
                back,
                roundtrip,
                1e-10,
                1e-10,
                &format!("D(E({linear}))"),
            );
        }
        // Strictly increasing over the whole pinned domain.
        let mut ordered: Vec<(f64, f64)> = DLOG2_CURVE_TABLE
            .iter()
            .map(|(linear, signal, _)| (*linear, *signal))
            .collect();
        ordered.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        for pair in ordered.windows(2) {
            assert!(
                pair[1].1 > pair[0].1,
                "curve is not monotone between {} and {}",
                pair[0].0,
                pair[1].0
            );
        }
        // Segment joins are continuous at the pinned (not rounded) breakpoints.
        for x in [
            DJI_DLOG2_T,
            DJI_DLOG2_MID,
            DJI_DLOG2_B2,
            DJI_DLOG2_B1,
        ] {
            let left = transfer.forward(x - 1e-12);
            let right = transfer.forward(x + 1e-12);
            assert!((left - right).abs() < 1e-11, "join at {x}");
        }
        // A rounded threshold (0.029 instead of the pinned T) would move
        // E(T+5e-6) by ~8.6e-5, far outside the 1e-12 budget.
        let near = DJI_DLOG2_T + 5e-6;
        assert_oracle_close(
            transfer.forward(near),
            0.1483296024647456,
            1e-12,
            1e-12,
            "pinned T branch",
        );
        let rounded_threshold_value =
            DJI_DLOG2_B2 + DJI_DLOG2_K2 * (near - 0.029);
        assert!(
            (transfer.forward(near) - rounded_threshold_value).abs() > 1e-5,
            "a rounded T threshold must not pass"
        );
    }

    #[test]
    fn dji_dlog2_reference_codes_are_the_theoretical_full_range_goldens() {
        let transfer = Transfer::DjiDLog2;
        let goldens = [
            (0.0, 64u16),
            (0.028961695254132, 152),
            (0.18, 312),
            (0.9, 456),
            (46.08, 812),
            (184.32, 937),
            (475.0, 1023),
        ];
        let references = transfer.references();
        assert_eq!(references.len(), goldens.len());
        for (reference, (linear, code)) in references.iter().zip(goldens) {
            assert_eq!(reference.linear, linear);
            assert_eq!(reference.code10, code);
            assert_eq!(
                (transfer.forward(linear) * 1023.0)
                    .round()
                    .clamp(0.0, 1023.0),
                f64::from(code),
                "full-range code at {linear}"
            );
            assert_eq!(reference.code8(), (f64::from(code) * 255.0 / 1023.0).round() as u16);
        }
        // The delivery domain is the inverse of the encodable signal range.
        assert_eq!(transfer.domain(), (transfer.inverse(0.0), transfer.inverse(1.0)));
        assert_oracle_close(
            transfer.domain().0,
            -0.021128829113842125,
            1e-15,
            0.0,
            "D(0)",
        );
        assert_oracle_close(transfer.domain().1, 475.0, 1e-12, 1e-12, "D(1)");
    }

    #[test]
    fn dji_dlog2_curve_is_pure_and_delivery_clipping_equals_signal_clipping() {
        let transfer = Transfer::DjiDLog2;
        // Pure curve: no pre-clamping of negatives or over-range values.
        assert!(transfer.forward(-0.5) < 0.0);
        assert!(transfer.forward(1000.0) > 1.0);
        assert_oracle_close(
            transfer.forward(-0.02),
            0.0033423899093129347,
            1e-12,
            1e-12,
            "signed toe",
        );

        // For the monotone curve, clipping in the target-linear domain to the
        // inverse endpoints is equivalent to clipping the signal to [0, 1].
        let (low, high) = transfer.domain();
        for target in [-1000.0, -1.0, -0.5, -0.0212, low, 0.0, 0.18, 475.0, high, 476.0, 1000.0] {
            let target_clipped = transfer.forward(target.clamp(low, high));
            let signal_clipped = transfer.forward(target).clamp(0.0, 1.0);
            assert_oracle_close(
                target_clipped,
                signal_clipped,
                1e-14,
                0.0,
                &format!("clip equivalence at {target}"),
            );
        }

        // The production conversion reports the same domain clips in channel
        // samples and never applies them before the pure curve.
        let profile = lookup(DLOG2_PROFILE).unwrap();
        let output = profile.default_output_transform();
        let mut codes = Vec::new();
        let low_bytes: Vec<u8> = [-0.5f32; 3]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        let counts = profile
            .convert_f32_frame_to_yuv10(&output, &low_bytes, 1, 1, 1.0, PreviewRange::Pc, &mut codes)
            .unwrap();
        assert_eq!(counts, [3, 0]);
        assert_eq!(codes[0], 0, "clipped-low luma is the signal-domain zero");
        assert!(matches!(codes[1], 511 | 512), "{codes:?}");
        assert!(matches!(codes[2], 511 | 512), "{codes:?}");
        let high_bytes: Vec<u8> = [1000f32; 3]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        let counts = profile
            .convert_f32_frame_to_yuv10(&output, &high_bytes, 1, 1, 1.0, PreviewRange::Pc, &mut codes)
            .unwrap();
        assert_eq!(counts, [0, 3]);
        assert_eq!(codes[0], 1023);
        assert!(matches!(codes[1], 511 | 512), "{codes:?}");
        assert!(matches!(codes[2], 511 | 512), "{codes:?}");
    }

    #[test]
    fn dji_dlog2_f32_signal_stays_within_the_recorded_one_e_minus_five_budget() {
        // A hypothetical f32 evaluation of the same pinned formula; the product
        // itself evaluates in f64. Recorded separately from the f64 budget.
        fn forward_f32(value: f32) -> f32 {
            let h = 475.0f32;
            let a = 16.285770761945304f32;
            let k1 = 0.059439938321493f32;
            let b1 = 0.304985337243402f32;
            let k2 = 2.960935245492250f32;
            let b2 = 0.148314799066323f32;
            let t = 0.028961695254132f32;
            let two_pow_a_minus_1 = 79891.40627605369f32;
            if value < t {
                b2 + k2 * (value - t)
            } else if value < 0.18 {
                b1 + k1 * (value / 0.18).log2()
            } else {
                (1.0 + value * two_pow_a_minus_1 / h).log2() / a
            }
        }
        let mut worst = 0.0f64;
        for (linear, signal, _) in DLOG2_CURVE_TABLE {
            let observed = f64::from(forward_f32(linear as f32));
            let difference = (observed - signal).abs();
            worst = worst.max(difference);
            assert!(
                difference <= 1e-5,
                "f32 E({linear}) = {observed:.9} vs f64 {signal:.9} (abs {difference:e})"
            );
        }
        eprintln!("dji_dlog2 f32-vs-f64 max signal error: {worst:e} (budget 1e-5)");
        assert!(worst <= 1e-5);
    }

    #[test]
    fn dji_dlog2_matrices_preserve_the_pinned_rounded_chain() {
        let profile = lookup(DLOG2_PROFILE).unwrap();
        let output = profile.default_output_transform();
        for row in 0..3 {
            for column in 0..3 {
                assert_oracle_close(
                    output.composed_matrix[row][column],
                    DLOG2_AP1_TO_DGAMUT2[row][column],
                    1e-12,
                    1e-12,
                    &format!("composed [{row}][{column}]"),
                );
            }
        }
        // Closed loop: the chain is inverse(M_DG2_AP0) * M_AP1_AP0.
        let chain = preview::mat3_mul(
            &cat::invert3(&DJI_DLOG2_DGAMUT2_TO_AP0).unwrap(),
            &ACES_AP1_TO_AP0,
        );
        for row in 0..3 {
            for column in 0..3 {
                assert_oracle_close(
                    chain[row][column],
                    output.composed_matrix[row][column],
                    1e-12,
                    1e-12,
                    &format!("chain [{row}][{column}]"),
                );
            }
        }
        // The three DCTL matrices are the printed four-decimal values.
        assert_eq!(
            profile.gamut.rgb_to_xyz,
            [[0.6917, 0.1596, 0.0990], [0.2498, 0.8381, -0.0880], [0.0, 0.0, 1.0891]]
        );
        assert_eq!(
            DCTL_CAT02_D65_TO_D60,
            [[1.0120, 0.0080, -0.0158], [0.0058, 1.0014, -0.0063], [-0.0003, -0.0010, 0.9276]]
        );
        assert_eq!(
            DCTL_XYZ60_TO_AP0,
            [[1.0498, 0.0, -0.0001], [-0.4959, 1.3733, 0.0982], [0.0, 0.0, 0.9913]]
        );
        // M_DG2_AP0 is the printed product, not a re-derivation.
        let product = preview::mat3_mul(
            &DCTL_XYZ60_TO_AP0,
            &preview::mat3_mul(&DCTL_CAT02_D65_TO_D60, &DCTL_DGAMUT2_TO_XYZ65),
        );
        for row in 0..3 {
            for column in 0..3 {
                assert_oracle_close(
                    product[row][column],
                    DJI_DLOG2_DGAMUT2_TO_AP0[row][column],
                    1e-15,
                    1e-15,
                    &format!("M_DG2_AP0 [{row}][{column}]"),
                );
            }
        }
        // The reported adaptation is the exact inverse of the printed constant.
        let net = cat::invert3(&DCTL_CAT02_D65_TO_D60).unwrap();
        for row in 0..3 {
            for column in 0..3 {
                assert_oracle_close(
                    output.cat_matrix[row][column],
                    net[row][column],
                    1e-12,
                    1e-12,
                    &format!("net adaptation [{row}][{column}]"),
                );
            }
        }
        assert_eq!(output.method, "cat02");
        assert_eq!(output.realization, OutputCatRealization::Reference);
        // The rounded chain's white-axis residual is preserved, not normalised.
        let white = preview::mat3_vec(&output.composed_matrix, [1.0, 1.0, 1.0]);
        let residual = white
            .iter()
            .fold(0.0f64, |largest, value| largest.max((value - 1.0).abs()));
        assert_oracle_close(residual, 0.0002508596888206352, 1e-12, 0.0, "white residual");
        assert!(residual > 1e-4, "the chain must not be normalised to neutral");
        // A Bradford chain with the same gamut is measurably different: CAT02 is
        // not silently replaced by the pre-existing default.
        let bradford = preview::mat3_mul(
            &DGAMUT2_TO_XYZ65_INVERSE,
            &preview::mat3_mul(&preview::BRADFORD_D60_TO_D65, &preview::AP1_D60_TO_XYZ_D60),
        );
        let difference = bradford
            .iter()
            .flatten()
            .zip(output.composed_matrix.iter().flatten())
            .fold(0.0f64, |largest, (a, b)| largest.max((a - b).abs()));
        assert!(difference > 1e-3, "CAT02 and Bradford chains coincide: {difference:e}");
        // A custom explicit method derives a different matrix and says so.
        let custom = profile
            .resolve_output_cat(
                OutputCatSelection::Method("bradford"),
                &cat::CatRegistry::standard(),
            )
            .unwrap();
        assert_eq!(custom.realization, OutputCatRealization::CustomAdaptation);
        assert_ne!(custom.composed_matrix, output.composed_matrix);
        assert_ne!(custom.fingerprint, output.fingerprint);
    }

    #[test]
    fn dji_dlog2_profile_reports_its_effective_contract() {
        let profile = lookup(DLOG2_PROFILE).unwrap();
        assert_eq!(profile.transfer.name(), "DJI D-Log 2");
        assert_eq!(profile.curve_input_scale(), 1.0);
        assert!(!profile.is_rev_eng());
        // The new profile has an independent identity from the original D-Log
        // profile: different transfer, gamut, reference chain and codes.
        assert_ne!(Transfer::DjiDLog2, Transfer::DjiDLog);
        let original = lookup("dji-dlog-dgamut").unwrap();
        assert_ne!(original.transfer.forward(0.18), profile.transfer.forward(0.18));
        assert_ne!(original.composed_matrix(), profile.composed_matrix());
        assert_ne!(original.gamut.name, profile.gamut.name);
        assert_eq!(profile.gamut.name, "DJI D-Gamut 2");
        assert_eq!(profile.range, PreviewRange::Pc);
        assert_eq!(profile.ycbcr_matrix, "bt2020nc");
        assert_eq!(profile.tag_policy, TagPolicy::VENDOR_LOG);
        // The vendor Log tag policy leaves primaries/transfer unspecified; the
        // YCbCr matrix tag describes coding, not BT.2020 primaries.
        assert_eq!(profile.tag_policy.primaries_token(), "unknown");
        assert_eq!(profile.tag_policy.transfer_token(), "unknown");
        assert_eq!(profile.tag_policy.matrix_token(), "bt2020nc");

        let output = profile.default_output_transform();
        let contract = profile.contract_json(&output);
        let cat = contract.get("output_cat").expect("output_cat");
        assert_eq!(cat.get("method").and_then(Json::as_str), Some("cat02"));
        assert_eq!(cat.get("realization").and_then(Json::as_str), Some("reference"));
        assert_eq!(cat.get("reference").and_then(Json::as_bool), Some(true));
        assert_eq!(
            cat.get("custom_adaptation").and_then(Json::as_bool),
            Some(false)
        );
        let matrices = contract.get("matrices").expect("matrices");
        // Legacy Bradford key keeps its old meaning only; this reference is not
        // a Bradford/D65 realization and must not emit it.
        assert!(matrices.get("bradford_cat_d60_to_d65").is_none());
        let printed = matrices
            .get("cat02_d65_to_d60")
            .expect("printed DCTL CAT02 matrix");
        assert_eq!(
            printed,
            &preview::matrix_json(&DCTL_CAT02_D65_TO_D60),
            "the contract must expose the printed constant for traceability"
        );
        assert!(matrices.get("dgamut2_to_acesap0").is_some());
        assert!(matrices.get("acesap1_to_acesap0").is_some());
        let transfer = contract.get("transfer").expect("transfer");
        assert_eq!(
            transfer.get("source").and_then(|s| s.get("url")).and_then(Json::as_str),
            Some(
                "https://github.com/thatcherfreeman/dwg-transforms/blob/\
                 56143916e275c617471f7845b5c6b57795d0ee28/specs/\
                 DLog2_DGamut2_to_ACES_AP0_cat02.dctl"
            )
        );
        let gamut = contract.get("gamut").expect("gamut");
        assert_eq!(
            gamut.get("white_xy"),
            Some(&pair_json(D65_WHITE_XY))
        );
    }
}
