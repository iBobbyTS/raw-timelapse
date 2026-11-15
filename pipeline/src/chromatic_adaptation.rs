//! Static chromatic-adaptation (CAT) algorithms and the standard derivation.
//!
//! The output colour transform is part of the output profile, not of a transfer
//! curve: a profile declares the CAT its own reference chain realizes, and the
//! CLI may override that choice. This module owns the algorithm registry, the
//! white-point encoding and the full-adaptation derivation used whenever the
//! selected method differs from the profile's fixed reference realization.
//!
//! Conventions (fixed by the plan):
//!
//! * column vectors, `M_out = inverse(M_target_to_XYZ) * CAT * M_AP1_to_XYZ`;
//! * `CAT = inverse(B) * diag((B * W_to) / (B * W_from)) * B`;
//! * white points are chromaticity `(x, y)` encoded at `Y = 1`;
//! * the same source and target white is exactly the identity;
//! * Bradford and CAT02 have independent fixed cone-matrix sources.
//!
//! The registry is static: adding an algorithm means adding a [`CatAlgorithm`]
//! to [`STANDARD_CAT_ALGORITHMS`] (or registering one in a test corpus), never
//! changing a transfer implementation or the native decoder. There is no
//! dynamic loading and no user-supplied matrix file.

/// ACES white point (D60) used by ACEScg AP1.
pub const ACES_WHITE_XY: [f64; 2] = [0.32168, 0.33767];

/// CIE D65 white point used by the current output gamuts.
pub const D65_WHITE_XY: [f64; 2] = [0.3127, 0.3290];

/// One statically registered chromatic-adaptation algorithm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CatAlgorithm {
    /// CLI/registry token, e.g. `bradford`, `cat02`.
    pub token: &'static str,
    /// Human-readable algorithm name.
    pub name: &'static str,
    /// Cone response matrix used by the full-adaptation derivation.
    pub cone_matrix: [[f64; 3]; 3],
    /// Source of the cone matrix and derivation.
    pub provenance: &'static str,
}

/// Bradford cone response. Published values; the pipeline already uses the
/// derived D60->D65 constant for its fixed reference realization.
pub const BRADFORD: CatAlgorithm = CatAlgorithm {
    token: "bradford",
    name: "Bradford",
    cone_matrix: [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ],
    provenance: "Bradford cone response matrix (Lam 1985), full adaptation with \
                 whites encoded at Y=1",
};

/// CIECAM02 CAT02 cone response. This is the independent CIECAM02 source, not
/// the four-decimal CAT02 65->60 constant printed inside a vendor DCTL.
pub const CAT02: CatAlgorithm = CatAlgorithm {
    token: "cat02",
    name: "CAT02 (CIECAM02)",
    cone_matrix: [
        [0.7328, 0.4296, -0.1624],
        [-0.7036, 1.6975, 0.0061],
        [0.0030, 0.0136, 0.9834],
    ],
    provenance: "CAT02 cone response matrix, CIE 159:2004 / CIECAM02, full adaptation \
                 with whites encoded at Y=1",
};

/// Every algorithm the product registers at startup. Adding an output CAT means
/// adding an entry here (and its numeric tests); no transfer implementation or
/// native decoder changes.
pub static STANDARD_CAT_ALGORITHMS: [CatAlgorithm; 2] = [BRADFORD, CAT02];

/// The reserved `--output-cat` value that selects the profile's own default.
pub const PROFILE_SELECTION: &str = "profile";

/// Static CAT registration. The product builds [`CatRegistry::standard`]; tests
/// may register additional algorithms without touching transfers or native code.
#[derive(Debug, Clone, Default)]
pub struct CatRegistry {
    algorithms: Vec<CatAlgorithm>,
}

impl CatRegistry {
    pub fn standard() -> Self {
        Self {
            algorithms: STANDARD_CAT_ALGORITHMS.to_vec(),
        }
    }

    /// Register one algorithm. Rejects the reserved `profile` token, duplicate
    /// tokens and a non-finite or singular cone matrix.
    pub fn register(&mut self, algorithm: CatAlgorithm) -> Result<(), String> {
        if algorithm.token.is_empty() {
            return Err("output CAT token must not be empty".into());
        }
        if algorithm.token == PROFILE_SELECTION {
            return Err(format!(
                "output CAT token {:?} is reserved for the profile default",
                algorithm.token
            ));
        }
        if self.lookup(algorithm.token).is_some() {
            return Err(format!(
                "output CAT {:?} is already registered",
                algorithm.token
            ));
        }
        if !matrix_is_finite(&algorithm.cone_matrix) {
            return Err(format!(
                "output CAT {:?} has a non-finite cone matrix",
                algorithm.token
            ));
        }
        invert3(&algorithm.cone_matrix).map_err(|message| {
            format!("output CAT {:?} cannot be registered: {message}", algorithm.token)
        })?;
        self.algorithms.push(algorithm);
        Ok(())
    }

    pub fn lookup(&self, token: &str) -> Option<&CatAlgorithm> {
        self.algorithms
            .iter()
            .find(|algorithm| algorithm.token == token)
    }

    /// Token listing for CLI diagnostics, in registration order.
    pub fn tokens(&self) -> String {
        self.algorithms
            .iter()
            .map(|algorithm| algorithm.token)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Encode a chromaticity white point as XYZ at `Y = 1`.
pub fn white_xyz(xy: [f64; 2]) -> Result<[f64; 3], String> {
    let [x, y] = xy;
    if !x.is_finite() || !y.is_finite() || y <= 0.0 {
        return Err(format!(
            "invalid white point ({x}, {y}): both coordinates must be finite and y must be positive"
        ));
    }
    let xyz = [x / y, 1.0, (1.0 - x - y) / y];
    if xyz.iter().any(|value| !value.is_finite()) {
        return Err(format!("white point ({x}, {y}) encodes non-finite XYZ"));
    }
    Ok(xyz)
}

/// Full chromatic adaptation between two white points with one algorithm.
///
/// `B` is the algorithm's cone matrix; the map is
/// `inverse(B) * diag((B * W_to) / (B * W_from)) * B`. Equal whites return the
/// exact identity.
pub fn adaptation_matrix(
    algorithm: &CatAlgorithm,
    from_xy: [f64; 2],
    to_xy: [f64; 2],
) -> Result<[[f64; 3]; 3], String> {
    let from = white_xyz(from_xy)?;
    let to = white_xyz(to_xy)?;
    if from_xy == to_xy {
        return Ok(identity());
    }
    let cone_inverse = invert3(&algorithm.cone_matrix).map_err(|message| {
        format!("CAT {:?} cone matrix is invalid: {message}", algorithm.token)
    })?;
    let from_cone = mat3_vec(&algorithm.cone_matrix, from);
    let to_cone = mat3_vec(&algorithm.cone_matrix, to);
    let mut diagonal = [[0.0; 3]; 3];
    for index in 0..3 {
        if !from_cone[index].is_finite() || from_cone[index] == 0.0 {
            return Err(format!(
                "CAT {:?} degenerates on the source white ({}, {})",
                algorithm.token, from_xy[0], from_xy[1]
            ));
        }
        let ratio = to_cone[index] / from_cone[index];
        if !ratio.is_finite() {
            return Err(format!(
                "CAT {:?} produced a non-finite cone ratio",
                algorithm.token
            ));
        }
        diagonal[index][index] = ratio;
    }
    let matrix = mat3_mul(&cone_inverse, &mat3_mul(&diagonal, &algorithm.cone_matrix));
    if !matrix_is_finite(&matrix) {
        return Err(format!(
            "CAT {:?} produced a non-finite adaptation matrix",
            algorithm.token
        ));
    }
    Ok(matrix)
}

/// Standard output composition for a custom CAT choice:
/// `inverse(M_target_to_XYZ) * CAT(W_from, W_to) * M_AP1_to_XYZ`.
///
/// `target_rgb_to_xyz` is the target gamut's declared primaries matrix; it is
/// inverted once here so the per-pixel path uses frozen coefficients.
pub fn custom_output_matrix(
    algorithm: &CatAlgorithm,
    from_xy: [f64; 2],
    to_xy: [f64; 2],
    ap1_to_xyz: &[[f64; 3]; 3],
    target_rgb_to_xyz: &[[f64; 3]; 3],
) -> Result<[[f64; 3]; 3], String> {
    let adaptation = adaptation_matrix(algorithm, from_xy, to_xy)?;
    let target_inverse = invert3(target_rgb_to_xyz)?;
    let matrix = mat3_mul(&target_inverse, &mat3_mul(&adaptation, ap1_to_xyz));
    if !matrix_is_finite(&matrix) {
        return Err("custom output CAT produced a non-finite composed matrix".into());
    }
    Ok(matrix)
}

pub fn identity() -> [[f64; 3]; 3] {
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
}

pub fn mat3_mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for (row, out_row) in out.iter_mut().enumerate() {
        for (column, value) in out_row.iter_mut().enumerate() {
            let mut sum = 0.0;
            for k in 0..3 {
                sum += a[row][k] * b[k][column];
            }
            *value = sum;
        }
    }
    out
}

pub fn mat3_vec(matrix: &[[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    let mut out = [0.0; 3];
    for (row, value) in out.iter_mut().enumerate() {
        *value = matrix[row][0] * vector[0]
            + matrix[row][1] * vector[1]
            + matrix[row][2] * vector[2];
    }
    out
}

/// Three-dimensional matrix inverse with an explicit singularity guard. Used
/// only while a run is being initialized (CAT/gamut composition), never
/// per pixel.
pub fn invert3(matrix: &[[f64; 3]; 3]) -> Result<[[f64; 3]; 3], String> {
    if !matrix_is_finite(matrix) {
        return Err("matrix is not finite".into());
    }
    let determinant = matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
        - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
        + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]);
    let scale = matrix
        .iter()
        .flatten()
        .fold(0.0f64, |largest, value| largest.max(value.abs()));
    if !determinant.is_finite() || determinant.abs() <= 1e-12 * scale.powi(3) {
        return Err(format!(
            "matrix is singular or numerically degenerate (determinant {determinant:e})"
        ));
    }
    let mut out = [[0.0; 3]; 3];
    for row in 0..3 {
        for column in 0..3 {
            let rows: Vec<usize> = (0..3).filter(|index| *index != row).collect();
            let columns: Vec<usize> = (0..3).filter(|index| *index != column).collect();
            let minor = matrix[rows[0]][columns[0]] * matrix[rows[1]][columns[1]]
                - matrix[rows[0]][columns[1]] * matrix[rows[1]][columns[0]];
            let sign = if (row + column) % 2 == 0 { 1.0 } else { -1.0 };
            out[column][row] = sign * minor / determinant;
        }
    }
    if !matrix_is_finite(&out) {
        return Err("matrix inverse is not finite".into());
    }
    Ok(out)
}

pub fn matrix_is_finite(matrix: &[[f64; 3]; 3]) -> bool {
    matrix.iter().flatten().all(|value| value.is_finite())
}

/// Stable change-detection fingerprint for one realized output transform. The
/// value is FNV-1a 64-bit over the canonical method/white/matrix description; it
/// identifies configuration drift and is not a cryptographic digest.
pub fn transform_fingerprint(
    profile: &str,
    method: &str,
    realization: &str,
    source_white_xy: [f64; 2],
    target_white_xy: [f64; 2],
    cat_matrix: &[[f64; 3]; 3],
    composed_matrix: &[[f64; 3]; 3],
) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for text in [profile, method, realization] {
        feed(text.as_bytes());
        feed(b"|");
    }
    for value in source_white_xy.iter().chain(target_white_xy.iter()) {
        feed(&value.to_bits().to_le_bytes());
    }
    for value in cat_matrix.iter().flatten().chain(composed_matrix.iter().flatten()) {
        feed(&value.to_bits().to_le_bytes());
    }
    format!("fnv1a64:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(observed: f64, expected: f64, tolerance: f64) -> bool {
        let difference = (observed - expected).abs();
        difference <= tolerance || difference <= tolerance * expected.abs()
    }

    #[test]
    fn registry_rejects_reserved_duplicate_and_degenerate_entries() {
        let mut registry = CatRegistry::standard();
        assert!(registry.lookup("bradford").is_some());
        assert!(registry.lookup("cat02").is_some());
        assert!(registry.lookup("profile").is_none());
        assert!(registry.register(BRADFORD).is_err(), "duplicate token");
        assert!(
            registry
                .register(CatAlgorithm {
                    token: "profile",
                    ..BRADFORD
                })
                .is_err(),
            "reserved token"
        );
        assert!(
            registry
                .register(CatAlgorithm {
                    token: "empty",
                    cone_matrix: [[0.0; 3]; 3],
                    ..BRADFORD
                })
                .is_err(),
            "singular cone matrix"
        );
        assert!(
            registry
                .register(CatAlgorithm {
                    token: "nan",
                    cone_matrix: [[f64::NAN, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                    ..BRADFORD
                })
                .is_err(),
            "non-finite cone matrix"
        );
    }

    #[test]
    fn white_points_are_encoded_at_y_equals_one_and_invalid_whites_rejected() {
        let d65 = white_xyz(D65_WHITE_XY).unwrap();
        assert!(close(d65[0], 0.950_455_927_051_671_7, 1e-12));
        assert_eq!(d65[1], 1.0);
        assert!(close(d65[2], 1.089_057_750_759_878, 1e-12));
        for bad in [
            [f64::NAN, 0.3],
            [0.3, f64::INFINITY],
            [0.3, 0.0],
            [0.3, -0.2],
        ] {
            assert!(white_xyz(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn identical_whites_are_exactly_the_identity() {
        for algorithm in [BRADFORD, CAT02] {
            let matrix =
                adaptation_matrix(&algorithm, ACES_WHITE_XY, ACES_WHITE_XY).unwrap();
            assert_eq!(matrix, identity(), "{}", algorithm.token);
        }
    }

    #[test]
    fn reverse_direction_inverts_the_forward_adaptation() {
        for algorithm in [BRADFORD, CAT02] {
            let forward = adaptation_matrix(&algorithm, ACES_WHITE_XY, D65_WHITE_XY).unwrap();
            let reverse = adaptation_matrix(&algorithm, D65_WHITE_XY, ACES_WHITE_XY).unwrap();
            let product = mat3_mul(&forward, &reverse);
            for row in 0..3 {
                for column in 0..3 {
                    let expected = if row == column { 1.0 } else { 0.0 };
                    assert!(
                        close(product[row][column], expected, 1e-12),
                        "{} [{row}][{column}] = {}",
                        algorithm.token,
                        product[row][column]
                    );
                }
            }
        }
    }

    #[test]
    fn adaptation_maps_the_source_white_to_the_target_white() {
        for algorithm in [BRADFORD, CAT02] {
            let matrix = adaptation_matrix(&algorithm, ACES_WHITE_XY, D65_WHITE_XY).unwrap();
            let mapped = mat3_vec(&matrix, white_xyz(ACES_WHITE_XY).unwrap());
            let target = white_xyz(D65_WHITE_XY).unwrap();
            for channel in 0..3 {
                assert!(
                    close(mapped[channel], target[channel], 1e-12),
                    "{} channel {channel}: {} vs {}",
                    algorithm.token,
                    mapped[channel],
                    target[channel]
                );
            }
        }
    }

    #[test]
    fn invert3_rejects_singular_and_ill_conditioned_matrices() {
        assert!(invert3(&[[1.0, 2.0, 3.0]; 3]).is_err());
        assert!(invert3(&[
            [1.0, 1.0, 0.0],
            [1.0, 1.0 + 1e-15, 0.0],
            [0.0, 0.0, 1.0]
        ])
        .is_err());
        assert!(invert3(&[[0.0; 3]; 3]).is_err());
        let inverse = invert3(&BRADFORD.cone_matrix).unwrap();
        let product = mat3_mul(&inverse, &BRADFORD.cone_matrix);
        for row in 0..3 {
            for column in 0..3 {
                let expected = if row == column { 1.0 } else { 0.0 };
                assert!(close(product[row][column], expected, 1e-12));
            }
        }
    }

    #[test]
    fn fingerprint_is_stable_and_sensitive() {
        let matrix = adaptation_matrix(&BRADFORD, ACES_WHITE_XY, D65_WHITE_XY).unwrap();
        let first = transform_fingerprint(
            "srgb-preview",
            "bradford",
            "reference",
            ACES_WHITE_XY,
            D65_WHITE_XY,
            &matrix,
            &matrix,
        );
        let second = transform_fingerprint(
            "srgb-preview",
            "bradford",
            "reference",
            ACES_WHITE_XY,
            D65_WHITE_XY,
            &matrix,
            &matrix,
        );
        assert_eq!(first, second);
        let other = transform_fingerprint(
            "srgb-preview",
            "cat02",
            "custom-adaptation",
            ACES_WHITE_XY,
            D65_WHITE_XY,
            &matrix,
            &matrix,
        );
        assert_ne!(first, other);
        assert!(first.starts_with("fnv1a64:"));
    }

    const D50_WHITE_XY: [f64; 2] = [0.3457, 0.3585];

    /// abs + rel 1e-10 against the frozen exact-rational oracle
    /// (`cat_oracle.py`, which parses the published decimals as
    /// `fractions.Fraction` and imports no production code).
    fn assert_oracle(observed: [[f64; 3]; 3], expected: [[f64; 3]; 3], label: &str) {
        for row in 0..3 {
            for column in 0..3 {
                let value = observed[row][column];
                let target = expected[row][column];
                let difference = (value - target).abs();
                assert!(
                    difference <= 1e-10 || difference <= 1e-10 * target.abs(),
                    "{label} [{row}][{column}]: {value:.17} vs {target:.17} \
                     (abs {difference:e})"
                );
            }
        }
    }

    #[test]
    fn bradford_matches_the_independent_exact_oracle() {
        // Exact-rational oracle values; the published Bradford constant used by
        // the fixed reference path is the same matrix rounded to 12 decimals.
        assert_oracle(
            adaptation_matrix(&BRADFORD, ACES_WHITE_XY, D65_WHITE_XY).unwrap(),
            [
                [0.9872240087030176, -0.00611322860685685, 0.015953288335912624],
                [-0.0075983718116623, 1.0018614847396539, 0.005330035791388932],
                [0.003072577058531529, -0.005095961511130595, 1.0816806030657953],
            ],
            "bradford D60(ACES)->D65",
        );
        assert_oracle(
            adaptation_matrix(&BRADFORD, D50_WHITE_XY, D65_WHITE_XY).unwrap(),
            [
                [0.9554734214880752, -0.023098454948764523, 0.06325924320057066],
                [-0.028369709333863583, 1.0099953980813041, 0.021041441191917306],
                [0.012314014864481996, -0.02050764929889898, 1.330365926242124],
            ],
            "bradford D50->D65",
        );
        assert_oracle(
            adaptation_matrix(&BRADFORD, D50_WHITE_XY, ACES_WHITE_XY).unwrap(),
            [
                [0.9675703791752964, -0.016926240170461484, 0.044295452175457585],
                [-0.021024114487297657, 1.008065770232668, 0.014795327078591209],
                [0.00853666249585025, -0.014161830869900836, 1.2298503067345758],
            ],
            "bradford D50->D60(ACES)",
        );
        assert_oracle(
            adaptation_matrix(&BRADFORD, ACES_WHITE_XY, D50_WHITE_XY).unwrap(),
            [
                [1.0342128721749322, 0.0168391293224598, -0.03745176709107609],
                [0.021671134550209956, 0.9921839911796545, -0.01271668534785111],
                [-0.006929154913974046, 0.011308198920635879, 0.8132206139818088],
            ],
            "bradford D60(ACES)->D50",
        );
        assert_oracle(
            adaptation_matrix(&BRADFORD, D65_WHITE_XY, ACES_WHITE_XY).unwrap(),
            [
                [1.013034914649986, 0.006105257823207134, -0.014970943626587517],
                [0.007698230125414972, 0.9981633521182776, -0.005032038535111868],
                [-0.002841317432439071, 0.004685156722537226, 0.9245061374576631],
            ],
            "bradford D65->D60(ACES)",
        );
    }

    #[test]
    fn cat02_matches_the_independent_exact_oracle() {
        assert_oracle(
            adaptation_matrix(&CAT02, ACES_WHITE_XY, D65_WHITE_XY).unwrap(),
            [
                [0.9882325471218135, -0.007885454399865814, 0.016757635451623946],
                [-0.005693191933411921, 0.9986923398294999, 0.006672372202884866],
                [0.0003532731423422887, 0.0011239768688093192, 1.0780829488488732],
            ],
            "cat02 D60(ACES)->D65",
        );
    }

    #[test]
    fn bradford_and_cat02_are_independent_algorithms() {
        let bradford = adaptation_matrix(&BRADFORD, ACES_WHITE_XY, D65_WHITE_XY).unwrap();
        let cat02 = adaptation_matrix(&CAT02, ACES_WHITE_XY, D65_WHITE_XY).unwrap();
        let largest = bradford
            .iter()
            .flatten()
            .zip(cat02.iter().flatten())
            .fold(0.0f64, |largest, (a, b)| largest.max((a - b).abs()));
        assert!(
            largest > 1e-3,
            "the two registered CATs must not resolve to the same matrix (max difference {largest:e})"
        );
        assert_ne!(BRADFORD.cone_matrix, CAT02.cone_matrix);
    }

    #[test]
    fn custom_output_composition_rejects_invalid_whites_and_target_matrices() {
        let ap1 = [[0.6624541811, 0.1340042065, 0.1561876870],
                   [0.2722287168, 0.6740817658, 0.0536895174],
                   [-0.0055746495, 0.0040607335, 1.0103391003]];
        let gamut = [[0.412390799266, 0.357584339384, 0.180480788402],
                     [0.212639005872, 0.715168678768, 0.072192315361],
                     [0.019330818716, 0.119194779795, 0.950532152250]];
        assert!(custom_output_matrix(&BRADFORD, ACES_WHITE_XY, [0.3127, 0.0], &ap1, &gamut).is_err());
        assert!(
            custom_output_matrix(&BRADFORD, [f64::NAN, 0.3], D65_WHITE_XY, &ap1, &gamut).is_err()
        );
        assert!(custom_output_matrix(&BRADFORD, ACES_WHITE_XY, D65_WHITE_XY, &ap1, &[[1.0; 3]; 3])
            .is_err());
        assert!(
            custom_output_matrix(&BRADFORD, ACES_WHITE_XY, D65_WHITE_XY, &ap1, &[
                [f64::INFINITY, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ])
            .is_err()
        );
    }
}
