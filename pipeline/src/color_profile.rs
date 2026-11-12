//! Preview output colour/Log profile registry and CPU reference conversion.
//!
//! The pipeline always feeds scene-linear ACEScg (AP1 primaries, D60 white) to
//! this module. Each registered profile fixes, independently of the encoder:
//!
//! * one official transfer curve, transcribed with its constants and source;
//! * one target colour gamut defined by official primaries plus white point,
//!   converted through the staged path
//!   `AP1/D60 -> XYZ(D60) -> Bradford CAT -> XYZ(D65) -> target linear RGB`
//!   (every registered gamut is D65-referenced);
//! * an explicit input domain derived from the official inverse curve at signal
//!   0 and 1 (no tone mapping: negatives below the domain clip to the official
//!   lower endpoint and highlights above it clip to the upper endpoint);
//! * 8-bit quantization with round-half-away-from-zero and no dither;
//! * the delivery range, YCbCr coding matrix and container/bitstream tag policy.
//!
//! The constants below are transcriptions, not values recalled from memory.
//! Citations are attached to every transfer and gamut and are also written into
//! the run manifest. The existing IEC 61966-2-1 sRGB path is preserved exactly:
//! the `srgb-preview` profile composes the same matrices in the same order as
//! [`crate::preview::ap1_to_linear_srgb_matrix`].

use crate::json::Json;
use crate::preview;

/// Preview output range: the real signal range written to the encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewRange {
    /// Limited/video range (16..235 luma in 8 bits), existing sRGB delivery.
    Tv,
    /// Full/PC range (0..255), used by the camera Log profiles.
    Pc,
}

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

    /// Pixel formats an H.264 decoder may report for this range. ffmpeg 8
    /// reports full-range 8-bit 4:2:0 H.264 as the deprecated `yuvj420p`.
    pub fn accepted_decoder_pix_fmts(self) -> &'static [&'static str] {
        match self {
            PreviewRange::Tv => &["yuv420p"],
            PreviewRange::Pc => &["yuv420p", "yuvj420p"],
        }
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

/// Official transfer curves used by the registered preview profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    /// IEC 61966-2-1 sRGB OETF (existing display-referred path).
    Srgb,
    /// Sony S-Log3.
    SonySLog3,
    /// Canon Log 2.
    CanonLog2,
    /// Nikon N-Log.
    NikonNLog,
    /// DJI D-Log (original D-Log, not D-Log 2).
    DjiDLog,
}

impl Transfer {
    pub fn name(self) -> &'static str {
        match self {
            Transfer::Srgb => "IEC 61966-2-1 sRGB OETF",
            Transfer::SonySLog3 => "Sony S-Log3",
            Transfer::CanonLog2 => "Canon Log 2",
            Transfer::NikonNLog => "Nikon N-Log",
            Transfer::DjiDLog => "DJI D-Log",
        }
    }

    pub fn formula(self) -> &'static str {
        match self {
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
        }
    }

    pub fn document(self) -> &'static str {
        match self {
            Transfer::Srgb => "IEC 61966-2-1:1999",
            Transfer::SonySLog3 => "Technical Summary for S-Gamut3.Cine/S-Log3 and S-Gamut3/S-Log3",
            Transfer::CanonLog2 => {
                "Description of the Canon Log, Canon Log 2 and Canon Log 3 Gamma Curves"
            }
            Transfer::NikonNLog => "N-Log Specification Document",
            Transfer::DjiDLog => "D-Log and D-Gamut White Paper",
        }
    }

    pub fn version(self) -> &'static str {
        match self {
            Transfer::Srgb => "1999",
            Transfer::SonySLog3 => "V1.0 (2013-12, Japanese edition)",
            Transfer::CanonLog2 => "2018-11-01",
            Transfer::NikonNLog => "1.0.0 (2018-09-01)",
            Transfer::DjiDLog => "2017-10-10 (Zenmuse X7)",
        }
    }

    pub fn url(self) -> &'static str {
        match self {
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
        }
    }

    /// Forward transfer: linear target-gamut reflection -> normalized signal.
    pub fn forward(self, value: f64) -> f64 {
        match self {
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
        }
    }

    /// Inverse transfer: normalized signal -> linear target-gamut reflection.
    pub fn inverse(self, signal: f64) -> f64 {
        match self {
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
        }
    }

    /// Linear input endpoints corresponding to signal 0 and 1. Inputs outside
    /// this interval are clipped explicitly (no tone mapping).
    pub fn domain(self) -> (f64, f64) {
        (self.inverse(0.0), self.inverse(1.0))
    }

    pub fn references(self) -> &'static [ReferenceCode] {
        match self {
            Transfer::Srgb => &SRGB_REFERENCES,
            Transfer::SonySLog3 => &SONY_REFERENCES,
            Transfer::CanonLog2 => &CANON_REFERENCES,
            Transfer::NikonNLog => &NIKON_REFERENCES,
            Transfer::DjiDLog => &DJI_REFERENCES,
        }
    }

    /// How the reference codes were established.
    pub fn reference_basis(self) -> &'static str {
        match self {
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

const D65_WHITE_XY: [f64; 2] = [0.3127, 0.3290];

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
    pub preview_filename: &'static str,
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

impl ColorProfile {
    /// Composed AP1/D60 scene-linear -> target linear matrix. The order of the
    /// matrix products matches `preview::ap1_to_linear_srgb_matrix` exactly so
    /// the sRGB profile stays bit-identical to the pre-existing path.
    pub fn composed_matrix(&self) -> [[f64; 3]; 3] {
        preview::mat3_mul(
            &self.gamut.xyz_to_rgb,
            &preview::mat3_mul(&preview::BRADFORD_D60_TO_D65, &preview::AP1_D60_TO_XYZ_D60),
        )
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
        let target = preview::mat3_vec(&self.composed_matrix(), ap1);
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

    /// Convert one frame of little-endian RGB16 ACEScg codes into packed RGB24.
    pub fn convert_frame(
        &self,
        rgb48le: &[u8],
        width: usize,
        height: usize,
        out: &mut impl FrameOutput,
    ) -> Result<(), String> {
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

    /// The complete per-profile colour contract written into `manifest.json`.
    pub fn contract_json(&self) -> Json {
        let (low, high) = self.transfer.domain();
        let mut matrices = vec![
            (
                "acescg_ap1_d60_to_xyz_d60",
                preview::matrix_json(&preview::AP1_D60_TO_XYZ_D60),
            ),
            (
                "bradford_cat_d60_to_d65",
                preview::matrix_json(&preview::BRADFORD_D60_TO_D65),
            ),
            (
                "xyz_d65_to_target_linear",
                preview::matrix_json(&self.gamut.xyz_to_rgb),
            ),
            (
                "composed_ap1_d60_to_target_linear",
                preview::matrix_json(&self.composed_matrix()),
            ),
        ];
        if self.id == "srgb-preview" {
            // Keep the pre-existing sRGB contract keys available to manifest
            // readers while the generic per-profile keys above are added.
            matrices.push((
                "xyz_d65_to_linear_srgb",
                preview::matrix_json(&preview::XYZ_D65_TO_LINEAR_SRGB),
            ));
            matrices.push((
                "composed_ap1_d60_to_linear_srgb_d65",
                preview::matrix_json(&preview::ap1_to_linear_srgb_matrix()),
            ));
        }

        Json::obj(vec![
            ("profile", Json::Str(self.id.to_string())),
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
                        Json::Str(
                            "16-bit RGB codes interpreted as code/16384 scene-linear ACEScg \
                             AP1/D60; no OETF in the source PNG"
                                .to_string(),
                        ),
                    ),
                ]),
            ),
            (
                "quantization",
                Json::obj(vec![
                    ("bits", Json::Int(8)),
                    (
                        "rounding",
                        Json::Str("round-half-away-from-zero on signal*255; no dither".to_string()),
                    ),
                    ("dither", Json::Bool(false)),
                    (
                        "tradeoff",
                        Json::Str(
                            "8-bit Log output can show banding; no dither is applied by owner \
                             decision, and richer output formats remain a future direction."
                                .to_string(),
                        ),
                    ),
                    (
                        "reference_convention",
                        Json::Str(
                            "official 10-bit codes are compared as code8 = round(code10*255/1023) \
                             with a +/-1 dual-rounding tolerance"
                                .to_string(),
                        ),
                    ),
                ]),
            ),
            (
                "encoding",
                Json::obj(vec![
                    ("codec", Json::Str("h264".to_string())),
                    (
                        "encoder",
                        Json::Str("h264_videotoolbox, 8-bit 4:2:0 (unchanged)".to_string()),
                    ),
                    ("pix_fmt", Json::Str("yuv420p".to_string())),
                    ("range", Json::Str(self.range.as_str().to_string())),
                    ("range_reason", Json::Str(self.range_reason.to_string())),
                    ("ycbcr_matrix", Json::Str(self.ycbcr_matrix.to_string())),
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
                "future_output",
                Json::Str(
                    "8-bit SDR delivery only; 10-bit, HEVC/ProRes and HLG/PQ outputs are a \
                     recorded future direction, not implemented"
                        .to_string(),
                ),
            ),
        ])
    }

    pub fn emitted_tags_json(&self) -> Json {
        let fields = || {
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
                    Json::Int(if self.range == PreviewRange::Pc { 1 } else { 0 }),
                ),
            ])
        };
        Json::obj(vec![
            ("h264_sps_vui", fields()),
            ("mp4_colr_nclx", fields()),
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
            ("emitted_tags", self.emitted_tags_json()),
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

pub static PROFILES: [ColorProfile; 6] = [
    ColorProfile {
        id: "srgb-preview",
        transfer: Transfer::Srgb,
        gamut: &SRGB_GAMUT,
        range: PreviewRange::Tv,
        ycbcr_matrix: "bt709",
        range_reason: "existing display-referred sRGB review delivery uses limited (tv) range",
        ycbcr_reason: "existing sRGB preview conversion uses the BT.709 matrix",
        tag_policy: TagPolicy::SRGB,
        preview_filename: "preview-h264-srgb-bt709.mp4",
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
        preview_filename: "preview-h264-slog3-sgamut3.mp4",
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
        preview_filename: "preview-h264-slog3-sgamut3cine.mp4",
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
        preview_filename: "preview-h264-clog2-cgamut.mp4",
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
        preview_filename: "preview-h264-nlog-bt2020.mp4",
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
        preview_filename: "preview-h264-dlog-dgamut.mp4",
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
    fn registry_has_the_frozen_six_profiles_and_srgb_default() {
        assert_eq!(PROFILES.len(), 6);
        assert_eq!(default_profile().id, "srgb-preview");
        for id in [
            "srgb-preview",
            "dji-dlog-dgamut",
            "canon-log2-cinema-gamut",
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
    fn preview_filenames_follow_the_transfer_gamut_contract() {
        for (id, filename) in [
            ("srgb-preview", "preview-h264-srgb-bt709.mp4"),
            ("dji-dlog-dgamut", "preview-h264-dlog-dgamut.mp4"),
            ("canon-log2-cinema-gamut", "preview-h264-clog2-cgamut.mp4"),
            ("sony-slog3-sgamut3", "preview-h264-slog3-sgamut3.mp4"),
            (
                "sony-slog3-sgamut3-cine",
                "preview-h264-slog3-sgamut3cine.mp4",
            ),
            ("nikon-nlog-bt2020", "preview-h264-nlog-bt2020.mp4"),
        ] {
            assert_eq!(lookup(id).expect(id).preview_filename, filename);
        }
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
            let contract = profile.contract_json();
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
            let tradeoff = quantization
                .get("tradeoff")
                .and_then(Json::as_str)
                .expect("tradeoff");
            for required in ["banding", "no dither", "owner", "future direction"] {
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
        let srgb = lookup("srgb-preview").expect("srgb").contract_json();
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
