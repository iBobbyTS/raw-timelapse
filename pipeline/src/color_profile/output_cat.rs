//! Output CAT selection, realization and the resolved per-run transform.
use crate::chromatic_adaptation as cat;
use crate::preview;
use super::gamut::*;

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

/// The output realization retained by the non-DJI registered profiles: the
/// published Bradford D60->D65 constant composed exactly as before.
pub(crate) const PREVIEW_OUTPUT_CAT: OutputCatDefault = OutputCatDefault {
    method: cat::BRADFORD.token,
    adaptation: preview::BRADFORD_D60_TO_D65,
    composed_override: None,
    provenance: "published Bradford D60(ACES white)->D65 constant already used by the \
                 pipeline; reference chain is AP1/D60 -> XYZ/D60 -> fixed Bradford -> XYZ/D65 -> \
                 the profile's declared target linear matrix, in the historic composition order",
};

/// Original D-Log default, frozen from the standard full CAT02 derivation:
/// inverse(D-Gamut->XYZ) * CAT02(ACES white, D65) * AP1->XYZ. It matches the
/// previously explicit `--output-cat cat02` path, replacing output Bradford.
pub(crate) const DJI_DLOG_OUTPUT_CAT: OutputCatDefault = OutputCatDefault {
    method: cat::CAT02.token,
    adaptation: [
        [0.9882325471218134, -0.007885454399865766, 0.01675763545162398],
        [-0.005693191933411961, 0.9986923398294999, 0.0066723722028848576],
        [0.00035327314234228887, 0.0011239768688093201, 1.0780829488488735],
    ],
    composed_override: Some([
        [1.0113227507798794, -0.07164051412233424, 0.06031776340253864],
        [-0.021197024649948024, 0.8593593389210054, 0.1618376857018451],
        [0.009435039242172493, 0.06329003589077266, 0.927274924820115],
    ]),
    provenance: "D-Log default: standard CAT02 ACES white/D60->D65, D-Gamut primaries \
                 from DJI white paper; inverse(target RGB->XYZ) x CAT02 x AP1->XYZ. \
                 Same colorspace conversion convention as ACES DJI D-Log/D-Gamut CTL; \
                 this does not claim camera exposure calibration",
};

/// The D-Log 2 profile's pinned reference realization: the fixed DCTL chain,
/// preserving its four-decimal constants (including the white-axis residual).
pub(crate) const DJI_DLOG2_OUTPUT_CAT: OutputCatDefault = OutputCatDefault {
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
