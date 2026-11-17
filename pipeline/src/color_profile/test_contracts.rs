//! Test-only colour-contract diagnostics compiled from each profile.
#[cfg(test)]
use crate::json::Json;

use super::coding::{PreviewRange, SignalDepth, TagPolicy};
use super::gamut::{
    ACES_AP1_TO_AP0, DCTL_CAT02_D65_TO_D60, DCTL_DGAMUT2_TO_XYZ65, DCTL_XYZ60_TO_AP0,
    DJI_DLOG2_DGAMUT2_TO_AP0,
};
use super::output_cat::{OutputCatRealization, OutputTransform};
use super::ColorProfile;
use crate::chromatic_adaptation::{self as cat, D65_WHITE_XY};
use crate::preview;

#[cfg(test)]
impl ColorProfile {
    /// The complete per-profile colour contract for one resolved run transform.
    /// Compiled for tests only: asserted by this module's contract regressions
    /// and unreachable from the CLI; un-gate deliberately if a runtime consumer
    /// ever needs it.
    #[cfg(test)]
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
                    ("bits", Json::Int(i64::from(SignalDepth::Ten.bits()))),
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
    #[cfg(test)]
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

    #[cfg(test)]
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

    #[cfg(test)]
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

#[cfg(test)]
pub(crate) fn pair_json(pair: [f64; 2]) -> Json {
    Json::Array(pair.iter().map(|value| Json::Float(*value)).collect())
}
