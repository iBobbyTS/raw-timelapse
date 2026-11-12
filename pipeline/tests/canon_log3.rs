//! Independent oracle: hashes come from the original Canon SDK, not Rust math.
//! Reproduce with rev-crm-2/scripts/verify_log3_sdk.py; see docs/canon-log3-rev-eng.md.
use rawtl_linear_video::{
    color_profile::{lookup, CameraColorMethod, Transfer},
    json::{self, Json},
};

#[cfg(target_os = "macos")]
#[path = "../src/digest.rs"]
mod digest;

const ID: &str = "canon-log3-cinema-gamut-rev-eng-crm-dev";

#[test]
#[cfg(target_os = "macos")]
fn production_curve_reproduces_all_589824_original_sdk_codes() {
    let oracle = json::parse(include_str!("fixtures/canon-log3-sdk-oracle.json")).unwrap();
    let p = lookup(ID).unwrap();
    for row in oracle.get("cases").and_then(Json::as_array).unwrap() {
        let ev = row.get("exposure_twelfths").and_then(Json::as_f64).unwrap();
        let gain = 2.0f64.powf(ev / 12.0);
        let mut bytes = Vec::with_capacity(65536 * 2);
        for i in 0..65536 {
            let x = (gain + gain) * ((i as f64 - 512.0) / 15872.0);
            // Undo only the continuous normalization to compare the SDK's
            // 16-bit codes, including 65535 saturation, not RGB8 delivery.
            let code = (p.transfer.forward(x * 0.9) * 65472.0 + 0.5)
                .trunc()
                .clamp(0.0, 65535.0) as u16;
            bytes.extend_from_slice(&code.to_le_bytes());
        }
        assert_eq!(
            digest::sha256(&bytes),
            row.get("table_sha256").and_then(Json::as_str).unwrap(),
            "p={ev}"
        );
    }
}

#[test]
fn joins_domain_signed_pixels_and_reference_scale_match_independent_math() {
    let p = lookup(ID).unwrap();
    assert_eq!(p.display_name(), "clog3 (rev-eng crm-dev)");
    assert_eq!(p.camera_color_method(), CameraColorMethod::DngCameraToPcs);
    assert!(p.is_rev_eng());
    assert_eq!(p.transfer, Transfer::CanonLog3RevEng);
    let (low, high) = p.transfer.domain();
    assert!((low - -0.07381334162475732).abs() < 1e-14);
    assert!((high - 14.66830073241655).abs() < 1e-12);
    // Cover both sides of both joins; rounded SDK constants produce tiny
    // downward jumps, so the mathematical inverse has a bounded ambiguity.
    for r in [
        -0.1,
        -0.012600001,
        -0.0126,
        -0.012599999,
        -0.005,
        0.0,
        0.005,
        0.012599999,
        0.0126,
        0.012600001,
        0.18,
        0.9,
        4.0,
        8.0,
        50.0,
    ] {
        assert!(
            (p.transfer.inverse(p.transfer.forward(r)) - r).abs() < 1e-10,
            "r={r}"
        );
    }
    for signal in 0..=65535 {
        let y = signal as f64 / 65535.0;
        assert!((p.transfer.forward(p.transfer.inverse(y)) - y).abs() < 2e-10);
    }
    let input = [-0.005f32, 0.5, 0.05];
    let bytes: Vec<_> = input.into_iter().flat_map(f32::to_le_bytes).collect();
    let mut out = Vec::new();
    assert_eq!(
        p.convert_f32_frame(&bytes, 1, 1, 1.0, &mut out).unwrap(),
        [0, 0]
    );
    assert_eq!(out, [36, 114, 74]); // independently evaluated AP1 -> Cinema Gamut -> Log3
    assert_ne!(out, p.encode_linear([0.0, 0.5, 0.05]));
    p.convert_f32_frame(&bytes, 1, 1, 2.0, &mut out).unwrap();
    assert_eq!(out, p.encode_linear(input.map(|v| v as f64 * 2.0)));
    let clipped: Vec<_> = [-10f32, 100.0, 100.0]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    let counts = p.convert_f32_frame(&clipped, 1, 1, 1.0, &mut out).unwrap();
    assert!(counts[1] > 0);
    assert!(p.convert_frame(&[0; 6], 1, 1, &mut out).is_err());
    for gain in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(p.convert_f32_frame(&bytes, 1, 1, gain, &mut out).is_err());
    }
}
