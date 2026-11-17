use crate::json::Json;

    use super::*;

    fn close(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() <= tolerance
    }

    // -----------------------------------------------------------------------
    // `--log-reference-gain auto`: desired brightening capped by the
    // transfer's encodable domain, in the exact arithmetic delivery uses.
    // -----------------------------------------------------------------------

    #[test]
    fn auto_reference_gain_keeps_the_desired_gain_when_it_fits() {
        let dlog = lookup("dji-dlog-dgamut").unwrap();
        let dlog2 = lookup("dji-dlog2-dgamut2").unwrap();
        let clog3 = lookup("canon-log3-cinema-gamut-rev-eng-crm-dev").unwrap();
        assert_eq!(dlog2.transfer.desired_auto_reference_gain(), 4.0);
        assert_eq!(dlog.transfer.desired_auto_reference_gain(), 4.0);
        assert_eq!(dlog.auto_reference_gain(0.5, -0.001).gain, 4.0);
        assert_eq!(dlog.auto_reference_gain(0.0, 0.0).gain, 4.0);
        assert_eq!(clog3.transfer.desired_auto_reference_gain(), 8.0);
        // Peaks far inside the domain: the desired gain is used unchanged.
        assert_eq!(dlog2.auto_reference_gain(0.5, -0.001).gain, 4.0);
        assert_eq!(clog3.auto_reference_gain(0.5, -0.001).gain, 8.0);
        // An all-black scan has no binding constraint and keeps the desire.
        assert_eq!(dlog2.auto_reference_gain(0.0, 0.0).gain, 4.0);
        assert_eq!(clog3.auto_reference_gain(0.0, 0.0).gain, 8.0);
    }

    #[test]
    fn auto_reference_gain_caps_by_the_positive_domain_bound() {
        let dlog2 = lookup("dji-dlog2-dgamut2").unwrap();
        let (low, high) = dlog2.transfer.domain();
        let scale = dlog2.curve_input_scale();
        for max in [1.0, 47.5, 199.9999, 200.0, 475.0] {
            let decision = dlog2.auto_reference_gain(max, -1e-6);
            let gain = decision.gain;
            assert!(gain.is_finite() && gain > 0.0);
            assert!(gain <= dlog2.transfer.desired_auto_reference_gain());
            assert!(
                max * (gain * scale) <= high,
                "max {max} still exceeds the domain at gain {gain}"
            );
            let _ = low;
        }
        // The nominal headroom bound: 475/200 is exactly representable.
        let decision = dlog2.auto_reference_gain(200.0, -1e-6);
        let gain = decision.gain;
        assert_eq!(decision.limited_by, "positive-headroom");
        assert!((gain - high / 200.0).abs() < 1e-12);
    }

    #[test]
    fn auto_reference_gain_caps_by_the_negative_toe_bound() {
        let clog3 = lookup("canon-log3-cinema-gamut-rev-eng-crm-dev").unwrap();
        let (low, _high) = clog3.transfer.domain();
        let scale = clog3.curve_input_scale();
        assert!(low < 0.0);
        // A large out-of-gamut negative binds before the positive peak.
        let decision = clog3.auto_reference_gain(0.5, -0.5);
        let gain = decision.gain;
        assert_eq!(decision.limited_by, "negative-toe");
        assert!((gain - low / (-0.5 * scale)).abs() < 1e-12);
        assert!(gain < clog3.transfer.desired_auto_reference_gain());
        assert!(-0.5 * (gain * scale) >= low);
    }

    #[test]
    fn auto_reference_gain_never_pushes_any_log_outside_its_domain() {
        for profile in PROFILES.iter().filter(|p| p.transfer != Transfer::Srgb) {
            let (low, high) = profile.transfer.domain();
            let scale = profile.curve_input_scale();
            assert!(low < 0.0 && high > 0.0, "{}", profile.id);
            for (max, min) in [
                (0.05, -0.02),
                (1.0, -0.5),
                (7.3, -0.004),
                (120.0, -3.0),
                (470.0, -40.0),
            ] {
                let gain = profile.auto_reference_gain(max, min).gain;
                assert!(gain.is_finite() && gain > 0.0, "{}", profile.id);
                assert!(
                    max * (gain * scale) <= high,
                    "{} positive clip at ({max}, {min})",
                    profile.id
                );
                assert!(
                    min * (gain * scale) >= low,
                    "{} negative clip at ({max}, {min})",
                    profile.id
                );
            }
        }
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
        assert_eq!(quantize_luma(low, PreviewRange::Tv, SignalDepth::Ten) + 1, quantize_luma(high, PreviewRange::Tv, SignalDepth::Ten));
        assert_eq!(quantize_signal(low), quantize_signal(high));
        assert_eq!(quantize_luma(0.0, PreviewRange::Tv, SignalDepth::Ten), 64);
        assert_eq!(quantize_luma(1.0, PreviewRange::Tv, SignalDepth::Ten), 940);
        assert_eq!(quantize_luma(0.0, PreviewRange::Pc, SignalDepth::Ten), 0);
        assert_eq!(quantize_luma(1.0, PreviewRange::Pc, SignalDepth::Ten), 1023);
        assert_eq!(quantize_chroma(-0.5, PreviewRange::Tv, SignalDepth::Ten), 64);
        assert_eq!(quantize_chroma(0.0, PreviewRange::Tv, SignalDepth::Ten), 512);
        assert_eq!(quantize_chroma(0.5, PreviewRange::Tv, SignalDepth::Ten), 960);
        assert_eq!(quantize_chroma(0.0, PreviewRange::Pc, SignalDepth::Ten), 512);
        assert_eq!(quantize_chroma(-0.5, PreviewRange::Pc, SignalDepth::Ten), 0);
        assert_eq!(quantize_chroma(0.5, PreviewRange::Pc, SignalDepth::Ten), 1023);
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
                .convert_f32_frame_to_yuv(&profile.default_output_transform(), &bytes, 1, 1, 1.0, profile.range, SignalDepth::Ten, &mut out)
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
            let expected = profile.encode_signal_yuv([signal; 3], profile.range, SignalDepth::Ten).unwrap();
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
                .convert_f32_frame_to_yuv(&profile.default_output_transform(), &bytes, 1, 1, 1.0, profile.range, SignalDepth::Ten, &mut out)
                .unwrap();
            assert_eq!(out[0], quantize_luma(luma, profile.range, SignalDepth::Ten), "{}", profile.id);
            assert_eq!(
                out[1],
                quantize_chroma((signal[2] - luma) / div_cb, profile.range, SignalDepth::Ten),
                "{}",
                profile.id
            );
            assert_eq!(
                out[2],
                quantize_chroma((signal[0] - luma) / div_cr, profile.range, SignalDepth::Ten),
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
            .convert_f32_frame_to_yuv(&profile.default_output_transform(), &negative, 1, 1, 1.0, profile.range, SignalDepth::Ten, &mut out)
            .unwrap();
        assert!(counts[0] > 0 && counts[1] == 0);
        assert_eq!(
            out,
            vec![quantize_luma(0.0, profile.range, SignalDepth::Ten), 512, 512],
            "{}",
            profile.id
        );
        let highlight: Vec<_> = [1_000_000f32; 3]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        let counts = profile
            .convert_f32_frame_to_yuv(&profile.default_output_transform(), &highlight, 1, 1, 1.0, profile.range, SignalDepth::Ten, &mut out)
            .unwrap();
        assert!(counts[1] > 0 && counts[0] == 0);
        assert_eq!(
            out,
            vec![quantize_luma(1.0, profile.range, SignalDepth::Ten), 512, 512],
            "{}",
            profile.id
        );
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let bytes: Vec<_> = [bad; 3].into_iter().flat_map(f32::to_le_bytes).collect();
            assert!(profile
                .convert_f32_frame_to_yuv(&profile.default_output_transform(), &bytes, 1, 1, 1.0, profile.range, SignalDepth::Ten, &mut out)
                .is_err());
        }
        assert!(profile
            .convert_f32_frame_to_yuv(&profile.default_output_transform(), &negative[..11], 1, 1, 1.0, profile.range, SignalDepth::Ten, &mut out)
            .is_err());
        assert!(profile
            .convert_f32_frame_to_yuv(&profile.default_output_transform(), &negative, 1, 1, 0.0, profile.range, SignalDepth::Ten, &mut out)
            .is_err());
        let srgb = lookup("srgb-preview").unwrap();
        assert!(srgb
            .convert_f32_frame_to_yuv(&srgb.default_output_transform(), &negative, 1, 1, 2.0, PreviewRange::Tv, SignalDepth::Ten, &mut out)
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
                Some(u64::from(SignalDepth::Ten.bits()))
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
