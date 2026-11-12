//! Shared preview matrix and sRGB transfer helpers.
//!
//! The sRGB output profile uses the following conversion:
//!
//! * RAW streaming supplies signed FP32 scene-linear ACEScg (AP1/D60);
//!   SCENE_SCALE remains for explicit legacy RGB16 utilities and fixtures;
//! * AP1/D60 -> XYZ(D60) -> Bradford CAT -> XYZ(D65) -> linear sRGB(D65);
//! * linear sRGB is clipped per channel to [0, 1] (display clipping; no tone
//!   mapping, no highlight roll-off, negatives and out-of-gamut colours clip);
//! * the IEC 61966-2-1 sRGB transfer function is applied, then the result is
//!   quantized to 8 bits with round-half-away-from-zero.
//!
//! The matrices below are the standard published values. The composed product
//! is used per pixel; the unit tests re-derive the staged path to show the
//! composition and check neutral and non-neutral reference values.

use crate::json::Json;

pub const SCENE_SCALE: f64 = 16384.0;

/// ACEScg AP1 (D60 white) -> CIE XYZ (D60).
pub const AP1_D60_TO_XYZ_D60: [[f64; 3]; 3] = [
    [0.6624541811, 0.1340042065, 0.1561876870],
    [0.2722287168, 0.6740817658, 0.0536895174],
    [-0.0055746495, 0.0040607335, 1.0103391003],
];

/// Bradford chromatic adaptation D60 -> D65, derived from the published
/// white points and the Bradford cone matrix.
pub const BRADFORD_D60_TO_D65: [[f64; 3]; 3] = [
    [0.987224008703, -0.006113228607, 0.015953288336],
    [-0.007598371812, 1.001861484740, 0.005330035791],
    [0.003072577059, -0.005095961511, 1.081680603066],
];

/// XYZ (D65) -> linear sRGB (IEC 61966-2-1 primaries, D65 white).
pub const XYZ_D65_TO_LINEAR_SRGB: [[f64; 3]; 3] = [
    [3.240969941904521, -1.537383177570093, -0.498610760293000],
    [-0.969243636280879, 1.875967501507720, 0.041555057407175],
    [0.055630079696993, -0.203976958888976, 1.056971514242878],
];

pub fn mat3_mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for row in 0..3 {
        for column in 0..3 {
            let mut sum = 0.0;
            for k in 0..3 {
                sum += a[row][k] * b[k][column];
            }
            out[row][column] = sum;
        }
    }
    out
}

pub fn mat3_vec(matrix: &[[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    let mut out = [0.0; 3];
    for row in 0..3 {
        out[row] =
            matrix[row][0] * vector[0] + matrix[row][1] * vector[1] + matrix[row][2] * vector[2];
    }
    out
}

/// Composed AP1/D60 scene-linear -> linear sRGB/D65 matrix.
pub fn ap1_to_linear_srgb_matrix() -> [[f64; 3]; 3] {
    mat3_mul(
        &XYZ_D65_TO_LINEAR_SRGB,
        &mat3_mul(&BRADFORD_D60_TO_D65, &AP1_D60_TO_XYZ_D60),
    )
}

/// Staged conversion used by the reference tests and documented colour
/// contract; production uses the algebraically identical composed matrix.
#[allow(dead_code)]
pub fn convert_scene_linear(ap1: [f64; 3]) -> [f64; 3] {
    let xyz_d60 = mat3_vec(&AP1_D60_TO_XYZ_D60, ap1);
    let xyz_d65 = mat3_vec(&BRADFORD_D60_TO_D65, xyz_d60);
    mat3_vec(&XYZ_D65_TO_LINEAR_SRGB, xyz_d65)
}

/// IEC 61966-2-1 sRGB opto-electronic transfer function.
pub fn srgb_oetf(value: f64) -> f64 {
    if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

/// IEC 61966-2-1 sRGB electro-optical transfer function (inverse OETF).
pub fn srgb_eotf(value: f64) -> f64 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

pub fn matrix_json(matrix: &[[f64; 3]; 3]) -> Json {
    Json::Array(
        matrix
            .iter()
            .map(|row| Json::Array(row.iter().map(|value| Json::Float(*value)).collect()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clip a linear sRGB triple to [0, 1], apply the sRGB OETF and quantize.
    fn encode_display_pixel(linear_srgb: [f64; 3]) -> [u8; 3] {
        let mut out = [0u8; 3];
        for channel in 0..3 {
            let clipped = linear_srgb[channel].clamp(0.0, 1.0);
            let encoded = srgb_oetf(clipped);
            out[channel] = (encoded * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        out
    }

    fn close(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() <= tolerance
    }

    #[test]
    fn staged_and_composed_conversions_agree() {
        let matrix = ap1_to_linear_srgb_matrix();
        for scene in [
            [0.18, 0.18, 0.18],
            [0.5, 0.25, 0.125],
            [1.0, 0.0, 0.0],
            [-0.1, 0.2, 0.3],
            [2.5, 0.5, 0.05],
        ] {
            let staged = convert_scene_linear(scene);
            let composed = mat3_vec(&matrix, scene);
            for channel in 0..3 {
                assert!(
                    close(staged[channel], composed[channel], 1e-12),
                    "scene {scene:?} channel {channel}: staged {} vs composed {}",
                    staged[channel],
                    composed[channel]
                );
            }
        }
    }

    #[test]
    fn neutral_scene_linear_maps_to_neutral_srgb() {
        let linear = convert_scene_linear([0.18, 0.18, 0.18]);
        for channel in 0..3 {
            assert!(
                close(linear[channel], 0.18, 1e-9),
                "neutral channel {channel} = {}",
                linear[channel]
            );
        }
        let bytes = encode_display_pixel(linear);
        // OETF(0.18) = 0.4613561295..., *255 = 117.645..., rounds to 118.
        assert_eq!(bytes, [118, 118, 118]);
        assert!(close(srgb_oetf(0.18), 0.46135612950044164, 1e-12));
    }

    #[test]
    fn non_neutral_reference_values() {
        // Expected values independently derived from the staged matrices with
        // extended-precision arithmetic during implementation review.
        let linear = convert_scene_linear([0.5, 0.25, 0.125]);
        let expected_linear = [0.686670107181761, 0.218754435511259, 0.099877619173666];
        let expected_oetf = [0.847050500866016, 0.505059911982991, 0.348984034295307];
        for channel in 0..3 {
            assert!(
                close(linear[channel], expected_linear[channel], 1e-9),
                "channel {channel}: {} vs {}",
                linear[channel],
                expected_linear[channel]
            );
            assert!(close(
                srgb_oetf(linear[channel].clamp(0.0, 1.0)),
                expected_oetf[channel],
                1e-12
            ));
        }
        assert_eq!(encode_display_pixel(linear), [216, 129, 89]);

        // Treating AP1 values as if they were already sRGB would produce a
        // clearly different result; the test rejects that mistake.
        let naive = encode_display_pixel([0.5, 0.25, 0.125]);
        assert_ne!(naive, [216, 129, 89]);
        assert_eq!(naive, [188, 137, 99]);
    }

    #[test]
    fn clipping_policy_is_explicit_and_two_sided() {
        // Deep negative scene values clip to black after the conversion.
        let negative = encode_display_pixel(convert_scene_linear([-0.5, -0.5, -0.5]));
        assert_eq!(negative, [0, 0, 0]);
        // Values far above display white clip to 255 without tone mapping.
        let highlight = encode_display_pixel(convert_scene_linear([4.0, 4.0, 4.0]));
        assert_eq!(highlight, [255, 255, 255]);
        // Primary AP1 colours are legal display colours once clipped.
        assert_eq!(
            encode_display_pixel(convert_scene_linear([1.0, 0.0, 0.0])),
            [255, 0, 0]
        );
        assert_eq!(
            encode_display_pixel(convert_scene_linear([0.0, 1.0, 0.0])),
            [0, 255, 0]
        );
        assert_eq!(
            encode_display_pixel(convert_scene_linear([0.0, 0.0, 1.0])),
            [0, 0, 255]
        );
    }

    #[test]
    fn oetf_is_continuous_at_the_linear_segment_boundary() {
        // The published piecewise constants are an approximation of the exact
        // crossover, so agreement at the stated 0.0031308 threshold is close
        // rather than bit-exact.
        let x = 0.003_130_8;
        assert!(close(12.92 * x, 1.055 * x.powf(1.0 / 2.4) - 0.055, 1e-7));
        assert_eq!(srgb_oetf(0.0), 0.0);
        assert!(close(srgb_oetf(1.0), 1.0, 1e-15));
    }
}
