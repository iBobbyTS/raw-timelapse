//! Transfer curves with published constants and pinned source citations.
use crate::preview;
use super::coding::ReferenceCode;
use super::{
    CANON_LOG3_REFERENCES, CANON_REFERENCES, DJI_DLOG2_REFERENCES, DJI_REFERENCES, NIKON_REFERENCES,
    SONY_REFERENCES, SRGB_REFERENCES,
};

/// Published and independently recovered transfer curves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    /// IEC 61966-2-1 sRGB OETF (existing display-referred path).
    Srgb,
    /// Sony S-Log3.
    SonySLog3,
    /// Canon Log 2.
    CanonLog2,
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
pub(crate) const DJI_DLOG2_H: f64 = 475.0;
pub(crate) const DJI_DLOG2_A: f64 = 16.285770761945304;
const DJI_DLOG2_K1: f64 = 0.059439938321493;
pub(crate) const DJI_DLOG2_B1: f64 = 0.304985337243402;
pub(crate) const DJI_DLOG2_K2: f64 = 2.960935245492250;
pub(crate) const DJI_DLOG2_B2: f64 = 0.148314799066323;
pub(crate) const DJI_DLOG2_T: f64 = 0.028961695254132;
/// `0.18` is the published middle-grey pivot of both inner segments.
pub(crate) const DJI_DLOG2_MID: f64 = 0.18;
/// The oracle's rounded f64 of the exact `2^A` for the pinned `A` (the platform
/// `exp2` may differ by a few ulp, far inside the 1e-12 curve budget). The
/// per-sample path therefore needs no `exp2` for a constant.
pub(crate) const DJI_DLOG2_TWO_POW_A: f64 = 79892.40627605369;
const DJI_DLOG2_TWO_POW_A_MINUS_1: f64 = DJI_DLOG2_TWO_POW_A - 1.0;
/// `H / (2^A - 1)`, the published highlight-segment scale.
const DJI_DLOG2_UPPER_SCALE: f64 = DJI_DLOG2_H / DJI_DLOG2_TWO_POW_A_MINUS_1;

impl Transfer {
    pub fn name(self) -> &'static str {
        match self {
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
            Transfer::CanonLog3RevEng => {
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
            Transfer::CanonLog3RevEng => "docs/canon-log3-rev-eng.md",
            Transfer::Srgb => "https://webstore.iec.ch/publication/6169",
            Transfer::SonySLog3 => {
                "https://www.sony.jp/ls-camera/knowledge/pdf/\
                 TechnicalSummary_for-S-Gamut3Cine_S-Gamut3_S-Log3_V1_00.pdf"
            }
            Transfer::CanonLog2 => {
                "https://downloads.canon.com/nw/pdfs/camcorders/whitepapers/\
                 canon-log-gamma-curves-white-paper.pdf"
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

    /// Desired gain for `--log-reference-gain auto`: two stops on D-Log / D-Log 2 and
    /// three stops on every other camera Log curve. This is a measured user
    /// choice for dark timelapse delivery, not a property of the curve; the
    /// domain bound in `ColorProfile::auto_reference_gain` is what actually
    /// protects highlights.
    pub fn desired_auto_reference_gain(self) -> f64 {
        match self {
            Self::DjiDLog | Self::DjiDLog2 => 4.0,
            _ => 8.0,
        }
    }

    pub fn references(self) -> &'static [ReferenceCode] {
        match self {
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
