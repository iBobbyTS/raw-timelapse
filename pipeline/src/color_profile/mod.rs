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
//!
//! Layout: `coding` (ranges, depth, YCbCr coding, quantizers), `transfer`
//! (transfer curves and vendor constants), `gamut` (target gamuts and pinned
//! matrices), `output_cat` (output-transform selection and realization),
//! `convert` (per-pixel conversion impl) and `test_contracts` (test-only
//! contract diagnostics). The registry itself lives here.

mod coding;
mod convert;
mod gamut;
mod output_cat;
#[cfg(test)]
#[cfg(test)]
mod test_contracts;
mod transfer;

pub use coding::{
    quantize_chroma, quantize_luma, quantize_signal, AutoReferenceGain, FrameOutput,
    PreviewRange, ReferenceCode, SignalDepth, TagPolicy, YcbcrCoding,
};
pub use gamut::Gamut;
pub use output_cat::{OutputCatDefault, OutputCatRealization, OutputCatSelection, OutputTransform};
pub use transfer::Transfer;

/// Re-exported static CAT registration surface (one path for CLI and tests).
pub use crate::chromatic_adaptation::{CatAlgorithm, CatRegistry};

pub(crate) use gamut::{
    BT2020_GAMUT, CINEMA_GAMUT, DGAMUT2_GAMUT, DGAMUT_GAMUT, SGAMUT3_CINE_GAMUT, SGAMUT3_GAMUT, SRGB_GAMUT,
};
pub(crate) use output_cat::{DJI_DLOG2_OUTPUT_CAT, PREVIEW_OUTPUT_CAT};
#[cfg(test)]
pub(crate) use gamut::{
    ACES_AP1_TO_AP0, DCTL_CAT02_D65_TO_D60, DCTL_DGAMUT2_TO_XYZ65, DCTL_XYZ60_TO_AP0,
    DGAMUT2_TO_XYZ65_INVERSE, DJI_DLOG2_DGAMUT2_TO_AP0,
};
#[cfg(test)]
pub(crate) use test_contracts::pair_json;
#[cfg(test)]
pub(crate) use transfer::{
    DJI_DLOG2_A, DJI_DLOG2_B1, DJI_DLOG2_B2, DJI_DLOG2_K2, DJI_DLOG2_MID, DJI_DLOG2_T,
    DJI_DLOG2_TWO_POW_A,
};

use crate::chromatic_adaptation as cat;
use crate::preview;
pub(crate) const SRGB_REFERENCES: [ReferenceCode; 1] = [ReferenceCode::new(0.18, "18% grey", 472)];

pub(crate) const SONY_REFERENCES: [ReferenceCode; 3] = [
    ReferenceCode::new(0.0, "0% black", 95),
    ReferenceCode::new(0.18, "18% grey", 420),
    ReferenceCode::new(0.9, "90% white", 598),
];

pub(crate) const CANON_REFERENCES: [ReferenceCode; 5] = [
    ReferenceCode::new(0.0, "0% black", 95),
    ReferenceCode::new(0.18, "18% grey", 407),
    ReferenceCode::new(0.9, "90% white (100% scene linear)", 575),
    ReferenceCode::new(7.2, "800%", 797),
    ReferenceCode::new(14.4, "1600%", 871),
];

pub(crate) const CANON_LOG3_REFERENCES: [ReferenceCode; 5] = [
    ReferenceCode::new(0.0, "0% black", 128),
    ReferenceCode::new(0.18, "18% grey", 351),
    ReferenceCode::new(0.9, "90% white (100% scene linear)", 577),
    ReferenceCode::new(7.2, "800%", 908),
    ReferenceCode::new(14.4, "1600%", 1020),
];

pub(crate) const NIKON_REFERENCES: [ReferenceCode; 3] = [
    ReferenceCode::new(0.0, "0% black", 127),
    ReferenceCode::new(0.18, "18% grey", 372),
    ReferenceCode::new(0.9, "90% white", 603),
];

pub(crate) const DJI_REFERENCES: [ReferenceCode; 3] = [
    ReferenceCode::new(0.0, "0% black", 95),
    ReferenceCode::new(0.18, "18% grey", 408),
    ReferenceCode::new(0.9, "90% white", 586),
];

/// Full-range reference codes of the pinned D-Log 2 definition. The vendor DCTL
/// publishes the formula rather than a code table, so these values are derived
/// from it and are not a camera-internal ISO/EI clipping table (nor limited-range
/// Y codes; delivery range is applied separately).
pub(crate) const DJI_DLOG2_REFERENCES: [ReferenceCode; 7] = [
    ReferenceCode::new(0.0, "0% black (linear segment origin)", 64),
    ReferenceCode::new(0.028961695254132, "lower breakpoint T", 152),
    ReferenceCode::new(0.18, "18% grey", 312),
    ReferenceCode::new(0.9, "90% white", 456),
    ReferenceCode::new(46.08, "800% (8 stops above 18% grey)", 812),
    ReferenceCode::new(184.32, "1024x 18% grey (10 stops)", 937),
    ReferenceCode::new(475.0, "upper clip limit H", 1023),
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
    /// Exhaustive on purpose: a future rev-eng curve needing the 0.9 reflection
    /// scale must fail to compile here instead of silently defaulting to 1.0.
    pub fn curve_input_scale(&self) -> f64 {
        match self.transfer {
            Transfer::CanonLog2RevEng | Transfer::CanonLog3RevEng => 0.9,
            Transfer::Srgb
            | Transfer::SonySLog3
            | Transfer::CanonLog2
            | Transfer::NikonNLog
            | Transfer::DjiDLog
            | Transfer::DjiDLog2 => 1.0,
        }
    }

    /// One sequence-wide gain for `--log-reference-gain auto`. `max_target`
    /// and `min_target` are the extremes of the composed output matrix over
    /// every scanned sample, in the same pre-scale units the delivery
    /// converter multiplies by `reference_gain * curve_input_scale`. Both
    /// constraints are upper bounds on the gain, so a positive result always
    /// exists for the camera Log curves whose encodable domain straddles zero.
    /// An all-black scan keeps the desired gain. The returned decision carries
    /// the bounds and the binding constraint so callers log it without
    /// re-deriving the arithmetic.
    pub fn auto_reference_gain(&self, max_target: f64, min_target: f64) -> AutoReferenceGain {
        let (low, high) = self.transfer.domain();
        let scale = self.curve_input_scale();
        let desired = self.transfer.desired_auto_reference_gain();
        let safe_positive = if max_target > 0.0 {
            high / (max_target * scale)
        } else {
            f64::INFINITY
        };
        let safe_negative = if min_target < 0.0 && low < 0.0 {
            low / (min_target * scale)
        } else {
            f64::INFINITY
        };
        let mut gain = desired.min(safe_positive).min(safe_negative);
        // Rounding guard: delivery multiplies each sample by `gain * scale`
        // once and compares against the domain. Nudge the gain down whole ulps
        // until both extreme products stay inside the encodable domain
        // bit-for-bit; when the desired gain already fits this never runs.
        for _ in 0..4 {
            let positive_ok = max_target <= 0.0 || max_target * (gain * scale) <= high;
            let negative_ok = min_target >= 0.0 || low >= 0.0 || min_target * (gain * scale) >= low;
            if positive_ok && negative_ok {
                break;
            }
            let decremented = f64::from_bits(gain.to_bits() - 1);
            if decremented <= 0.0 || !decremented.is_finite() || decremented >= gain {
                break;
            }
            gain = decremented;
        }
        let limited_by = if gain >= desired {
            "desired"
        } else if safe_positive <= safe_negative {
            "positive-headroom"
        } else {
            "negative-toe"
        };
        AutoReferenceGain {
            gain,
            desired,
            safe_positive,
            safe_negative,
            limited_by,
        }
    }
    /// Test-support predicate consumed by the reference tests.
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

}

// The coefficient/code-point sources govern the transform, not the
// project's choice of matrix for vendor Log RGB or its full-range delivery.
pub(crate) const BT2020_NCL_CONTAINER_REASON: &str =
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
pub(crate) const LOG_RANGE_REASON: &str = "Rawtl full-range (pc) delivery convention, not a vendor mandate. \
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
mod camera_recipe_tests;
#[cfg(test)]
mod delivered_range_tests;
#[cfg(test)]
mod output_cat_tests;
#[cfg(test)]
mod tests;
