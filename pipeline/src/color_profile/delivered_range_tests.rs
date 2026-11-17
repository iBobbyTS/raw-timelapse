
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
            .convert_f32_frame_to_yuv(&profile.default_output_transform(), &bytes, 1, 1, 1.0, PreviewRange::Tv, SignalDepth::Ten, &mut limited)
            .unwrap();
        let mut full = Vec::new();
        profile
            .convert_f32_frame_to_yuv(&profile.default_output_transform(), &bytes, 1, 1, 1.0, PreviewRange::Pc, SignalDepth::Ten, &mut full)
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
