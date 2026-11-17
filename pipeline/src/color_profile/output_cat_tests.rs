use crate::chromatic_adaptation::D65_WHITE_XY;
use crate::json::Json;

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
    fn original_dlog_cat02_default_matches_standard_derivation_and_replaces_bradford() {
        let p = lookup("dji-dlog-dgamut").unwrap();
        let registry = CatRegistry::standard();
        let default = p.resolve_output_cat(OutputCatSelection::Profile, &registry).unwrap();
        let same = p.resolve_output_cat(OutputCatSelection::Method("cat02"), &registry).unwrap();
        assert_eq!(default.method, "cat02");
        assert_eq!(default.composed_matrix, same.composed_matrix);
        assert_eq!(default.realization, OutputCatRealization::Reference);
        let expected = cat::custom_output_matrix(&cat::CAT02, cat::ACES_WHITE_XY,
            p.gamut.white_xy, &preview::AP1_D60_TO_XYZ_D60, &p.gamut.rgb_to_xyz).unwrap();
        assert_matrix_close(default.composed_matrix, expected, 1e-14, "D-Log CAT02");
        let bradford = p.resolve_output_cat(OutputCatSelection::Method("bradford"), &registry).unwrap();
        assert_eq!(bradford.realization, OutputCatRealization::CustomAdaptation);
        assert_ne!(default.composed_matrix, bradford.composed_matrix);
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
                .convert_f32_frame_to_yuv(output, &bytes, 1, 1, 1.0, profile.range, SignalDepth::Ten, &mut out)
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
            .convert_f32_frame_to_yuv(&other, &bytes, 1, 1, 1.0, profile.range, SignalDepth::Ten, &mut out)
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
            .convert_f32_frame_to_yuv(&output, &low_bytes, 1, 1, 1.0, PreviewRange::Pc, SignalDepth::Ten, &mut codes)
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
            .convert_f32_frame_to_yuv(&output, &high_bytes, 1, 1, 1.0, PreviewRange::Pc, SignalDepth::Ten, &mut codes)
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
        assert!(!profile.is_canon_log());
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
