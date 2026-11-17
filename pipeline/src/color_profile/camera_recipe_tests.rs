
    use super::*;
    #[test]
    fn selection_table_and_curve_units_are_independent() {
        for profile in PROFILES.iter() {
            for cr3 in [false, true] {
                let canon = cr3 && profile.transfer == Transfer::CanonLog3RevEng;
                let expected = if canon { CameraRecipe::CanonClog3 }
                    else { CameraRecipe::Generic };
                assert_eq!(CameraColor::Auto.resolve(profile, cr3).unwrap(), expected);
                assert_eq!(CAMERA_PROFILE_CORRECTION, "hue-sat-map");
                assert_eq!(CameraColor::Generic.resolve(profile, cr3).unwrap(), CameraRecipe::Generic);
                assert_eq!(CameraColor::CanonClog3.resolve(profile, cr3).is_ok(), canon);
                assert_eq!(profile.curve_input_scale(), match profile.transfer { Transfer::CanonLog2RevEng | Transfer::CanonLog3RevEng => 0.9, _ => 1.0 });
            }
        }
    }
    #[test]
    fn generic_recipe_applies_profile_color_correction() {
        let recipe = CameraRecipe::Generic;
        assert_eq!(recipe.native_token(), "dng-camera-to-pcs");
        assert_eq!(recipe.token(), "generic");
        assert_eq!(CAMERA_PROFILE_CORRECTION, "hue-sat-map");
        assert_eq!(recipe.calibration(), "none");
        assert!(!recipe.requires_canon_cr3());
    }
    #[test]
    fn twelve_bit_quantizers_use_h273_scaled_code_points() {
        let d = SignalDepth::Twelve;
        // Limited-range luma: 256 + 3504*v over H.273's 256..3760.
        assert_eq!(quantize_luma(0.0, PreviewRange::Tv, d), 256);
        assert_eq!(quantize_luma(1.0, PreviewRange::Tv, d), 3760);
        // Full-range luma peaks at 4095.
        assert_eq!(quantize_luma(1.0, PreviewRange::Pc, d), 4095);
        assert_eq!(quantize_luma(0.0, PreviewRange::Pc, d), 0);
        // Neutral chroma sits at the 12-bit limited-range centre 2048;
        // extremes reach 256..3840 (2048 +/- 1792).
        assert_eq!(quantize_chroma(0.0, PreviewRange::Tv, d), 2048);
        assert_eq!(quantize_chroma(0.5, PreviewRange::Tv, d), 3840);
        assert_eq!(quantize_chroma(-0.5, PreviewRange::Tv, d), 256);
        // Full-range chroma centres on 2047.5; +/-0.5 reaches exactly 4095/0
        // (the 10-bit path behaves identically: 511.5 +/- 511.5).
        assert_eq!(quantize_chroma(0.0, PreviewRange::Pc, d), 2048);
        assert_eq!(quantize_chroma(0.5, PreviewRange::Pc, d), 4095);
        assert_eq!(quantize_chroma(-0.5, PreviewRange::Pc, d), 0);
        // Clamping beyond the normalized signal domain stays inside the codes.
        assert_eq!(quantize_luma(2.0, PreviewRange::Tv, d), 3760);
        assert_eq!(quantize_chroma(0.9, PreviewRange::Pc, d), 4095);
    }
    #[test]
    fn curve_input_scale_table_is_explicit_for_every_registered_profile() {
        let expected = |id: &str| {
            if id.starts_with("canon-log") && id.ends_with("rev-eng-crm-dev") {
                0.9
            } else {
                1.0
            }
        };
        for profile in PROFILES {
            assert_eq!(
                profile.curve_input_scale(),
                expected(profile.id),
                "profile {}",
                profile.id
            );
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
                p.convert_f32_frame_to_yuv(&p.default_output_transform(), &bytes, 1, 1, gain, PreviewRange::Pc, SignalDepth::Ten, &mut out).unwrap();
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
                profile.convert_f32_frame_to_yuv(&profile.default_output_transform(), &bytes, 1, 1, gain, PreviewRange::Pc, SignalDepth::Ten, &mut Vec::new()).unwrap();
            }
            for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
                assert!(profile.convert_f32_frame(&bytes, 1, 1, bad, &mut Vec::new()).is_err());
                assert!(profile.convert_f32_frame_to_yuv(&profile.default_output_transform(), &bytes, 1, 1, bad, PreviewRange::Pc, SignalDepth::Ten, &mut Vec::new()).is_err());
            }
        }
    }
