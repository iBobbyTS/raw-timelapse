//! Target colour gamuts and pinned derivation matrices (DCTL transcriptions).
use crate::chromatic_adaptation::D65_WHITE_XY;
use crate::preview;
/// One target colour gamut with its official primaries and white point.
#[derive(Debug, Clone, Copy)]
pub struct Gamut {
    pub name: &'static str,
    pub primaries_xy: [[f64; 2]; 3],
    pub white_xy: [f64; 2],
    pub rgb_to_xyz: [[f64; 3]; 3],
    pub xyz_to_rgb: [[f64; 3]; 3],
    pub provenance: &'static str,
}

/// IEC 61966-2-1 sRGB primaries, D65 white. The `xyz_to_rgb` matrix is the
/// exact published matrix used by the pre-existing preview path.
pub(crate) const SRGB_GAMUT: Gamut = Gamut {
    name: "sRGB / Rec.709 (IEC 61966-2-1)",
    primaries_xy: [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]],
    white_xy: D65_WHITE_XY,
    rgb_to_xyz: [
        [0.412390799266, 0.357584339384, 0.180480788402],
        [0.212639005872, 0.715168678768, 0.072192315361],
        [0.019330818716, 0.119194779795, 0.950532152250],
    ],
    xyz_to_rgb: preview::XYZ_D65_TO_LINEAR_SRGB,
    provenance: "IEC 61966-2-1:1999 primaries and D65 white; existing pipeline matrix",
};

/// Sony S-Gamut3 primaries and D65 white (Sony technical summary, page 8).
pub(crate) const SGAMUT3_GAMUT: Gamut = Gamut {
    name: "Sony S-Gamut3",
    primaries_xy: [[0.73000, 0.28000], [0.14000, 0.85500], [0.10000, -0.05000]],
    white_xy: D65_WHITE_XY,
    rgb_to_xyz: [
        [0.706482713192, 0.128801049791, 0.115172164069],
        [0.270979670813, 0.786606411221, -0.057586082034],
        [-0.009677845386, 0.004600037493, 1.094135558654],
    ],
    xyz_to_rgb: [
        [1.507399899060, -0.245822137385, -0.171611680816],
        [-0.518151727083, 1.355391240902, 0.125878668162],
        [0.015511698158, -0.007872771427, 0.911916365575],
    ],
    provenance: "Sony technical summary V1.0, 'Color Primaries and conversion matrix'; \
                 matrix derived from the published primaries and D65 white by the standard \
                 normalised-primary-matrix construction",
};

/// Sony S-Gamut3.Cine primaries and D65 white (Sony technical summary, page 8).
pub(crate) const SGAMUT3_CINE_GAMUT: Gamut = Gamut {
    name: "Sony S-Gamut3.Cine",
    primaries_xy: [[0.76600, 0.27500], [0.22500, 0.80000], [0.08900, -0.08700]],
    white_xy: D65_WHITE_XY,
    rgb_to_xyz: [
        [0.599083920758, 0.248925516115, 0.102446490178],
        [0.215075820116, 0.885068501744, -0.100144321859],
        [-0.032065849545, -0.027658390679, 1.148781990984],
    ],
    xyz_to_rgb: [
        [1.846778969290, -0.525986122969, -0.210545211388],
        [-0.444153262903, 1.259442902808, 0.149399972888],
        [0.040855421196, 0.015640889310, 0.868207248699],
    ],
    provenance: "Sony technical summary V1.0, 'Color Primaries and conversion matrix'; \
                 matrix derived from the published primaries and D65 white by the standard \
                 normalised-primary-matrix construction",
};

/// Canon Cinema Gamut primaries and D65 white.
pub(crate) const CINEMA_GAMUT: Gamut = Gamut {
    name: "Canon Cinema Gamut",
    primaries_xy: [[0.7400, 0.2700], [0.1700, 1.1400], [0.0800, -0.1000]],
    white_xy: D65_WHITE_XY,
    rgb_to_xyz: [
        [0.716049646552, 0.129683477876, 0.104722802624],
        [0.261261357526, 0.869642145755, -0.130903503281],
        [-0.009676346575, -0.236481636126, 1.335215733461],
    ],
    xyz_to_rgb: [
        [1.489818274932, -0.260895902184, -0.142426521777],
        [-0.458166574469, 1.261627783050, 0.159623631630],
        [-0.070349667723, 0.221557667226, 0.776181603627],
    ],
    provenance: "Canon Color Gamut Conversion for DCI-P3+ and Cinema Gamut, PUB. DIM-1102-000 \
                 (2013), English p.2 (https://gdlp01.c-wss.com/gds/8/0200003438/01/colorgamutinfo-dim1102.pdf): \
                 D65 white (0.3127,0.3290), R(0.7400,0.2700), G(0.1700,1.1400), B(0.0800,-0.1000). \
                 Unchanged primaries cross-validated against colour-science and Canon's ACES \
                 linear Cinema Gamut IDT distributed by ASWF OCIO Config ACES. Matrices derived \
                 with D65 by normalised-primary-matrix construction, not the IDT's D55 calibration",
};

/// ITU-R BT.2020 primaries and D65 white (the N-Log specification repeats them).
pub(crate) const BT2020_GAMUT: Gamut = Gamut {
    name: "ITU-R BT.2020",
    primaries_xy: [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]],
    white_xy: D65_WHITE_XY,
    rgb_to_xyz: [
        [0.636958048301, 0.144616903586, 0.168880975164],
        [0.262700212011, 0.677998071519, 0.059301716470],
        [0.000000000000, 0.028072693049, 1.060985057711],
    ],
    xyz_to_rgb: [
        [1.716651187971, -0.355670783776, -0.253366281374],
        [-0.666684351832, 1.616481236635, 0.015768545814],
        [0.017639857445, -0.042770613258, 0.942103121235],
    ],
    provenance: "Nikon N-Log Specification Document §3 Gamut and White Point (primaries and \
                 D65 white identical to ITU-R BT.2020); matrix from the published primaries",
};

/// DJI D-Gamut primaries and D65 white.
pub(crate) const DGAMUT_GAMUT: Gamut = Gamut {
    name: "DJI D-Gamut",
    primaries_xy: [[0.71, 0.31], [0.21, 0.88], [0.09, -0.08]],
    white_xy: D65_WHITE_XY,
    rgb_to_xyz: [
        [0.648171968634, 0.194058149821, 0.108225808597],
        [0.283004662361, 0.813196056392, -0.096200718753],
        [-0.018258365314, -0.083167778495, 1.190483894568],
    ],
    xyz_to_rgb: [
        [1.725775899752, -0.431443974938, -0.191752877750],
        [-0.602444115230, 1.390574412063, 0.167137296299],
        [-0.015618936122, 0.090529173446, 0.848729956075],
    ],
    provenance: "DJI D-Log/D-Gamut white paper (2017-10-10): primaries R(0.71,0.31), \
                 G(0.21,0.88), B(0.09,-0.08), D65 white, and printed RGB<->XYZ matrices. The \
                 printed 4-decimal matrices are not exactly neutral; the matrices here are \
                 derived from the published primaries and white point and agree with the \
                 printed matrices to 1.84e-4",
};

/// DJI D-Gamut 2. The fixed DCTL publishes only the 4-decimal D-Gamut 2 ->
/// XYZ(D65) matrix, so the primary chromaticities here are recovered from that
/// rounded matrix (they are not an infinite-precision official primary table)
/// and the matrix is used digit for digit. Its row sums are 0.9503/0.9999/1.0891,
/// i.e. the rounded chain is not exactly neutral at D65; that residual is kept.
pub(crate) const DGAMUT2_GAMUT: Gamut = Gamut {
    name: "DJI D-Gamut 2",
    primaries_xy: [
        [0.7346787041954328, 0.2653212958045672],
        [0.15996792623032977, 0.8400320737696703],
        [0.08999181892555222, -0.0799927279338242],
    ],
    white_xy: D65_WHITE_XY,
    rgb_to_xyz: DCTL_DGAMUT2_TO_XYZ65,
    xyz_to_rgb: DGAMUT2_TO_XYZ65_INVERSE,
    provenance: "derived-from-rounded-matrix: primary chromaticities recovered from the \
                 pinned DCTL's 4-decimal D-Gamut 2->XYZ65 matrix; D65 white is the chain's \
                 own coding white. The matrix is used as published, never normalised to an \
                 exact neutral axis (max rgb-white residual 2.5e-4). Not an infinite-precision \
                 official DJI primaries table",
};

/// Pinned DCTL D-Gamut 2 -> XYZ(D65), copied digit for digit (row major).
pub(crate) const DCTL_DGAMUT2_TO_XYZ65: [[f64; 3]; 3] = [
    [0.6917, 0.1596, 0.0990],
    [0.2498, 0.8381, -0.0880],
    [0.0, 0.0, 1.0891],
];

/// Pinned DCTL CAT02 XYZ(D65)->XYZ(D60), copied digit for digit. The reference
/// chain applies its inverse (XYZ(D60)->XYZ(D65)), as required by
/// `inverse(M_DG2_AP0) * M_AP1_AP0`; the printed constant itself is preserved
/// digit for digit and its 4-decimal rounding contributes to the chain's
/// white-axis residual. Compiled for tests only (consumed by the contract
/// diagnostics); the applied matrix is the pinned composition below.
#[cfg(test)]
pub(crate) const DCTL_CAT02_D65_TO_D60: [[f64; 3]; 3] = [
    [1.0120, 0.0080, -0.0158],
    [0.0058, 1.0014, -0.0063],
    [-0.0003, -0.0010, 0.9276],
];

/// Pinned DCTL XYZ(D60) -> ACES AP0, copied digit for digit.
#[cfg(test)]
pub(crate) const DCTL_XYZ60_TO_AP0: [[f64; 3]; 3] = [
    [1.0498, 0.0, -0.0001],
    [-0.4959, 1.3733, 0.0982],
    [0.0, 0.0, 0.9913],
];

/// ACES AP1 -> AP0 in f64 from the ACES v1.3 chromaticities (common D60
/// 0.32168/0.33767), by the normalised-primary-matrix construction. Derived by
/// the independent oracle (`dlog2_oracle.py`); not the DCTL's rounded XYZ->AP0.
#[cfg(test)]
pub(crate) const ACES_AP1_TO_AP0: [[f64; 3]; 3] = [
    [0.6954522413574518, 0.14067869647029416, 0.16386906217225408],
    [0.04479456337203771, 0.8596711184564219, 0.09553431817154041],
    [-0.005525882558113589, 0.004025210305978657, 1.001500672252135],
];

/// Pinned DCTL D-Gamut 2 -> ACES AP0: `M_XYZ60_AP0 * M_CAT02_65_60 * M_DG2_XYZ65`.
#[cfg(test)]
pub(crate) const DJI_DLOG2_DGAMUT2_TO_AP0: [[f64; 3]; 3] = [
    [0.736958385971, 0.176597444598, 0.08627254501],
    [0.000873985052, 1.07033801293, -0.071242291115],
    [-0.000453331403, -0.000878271974, 1.001517785098],
];

/// Inverse of the pinned DCTL D-Gamut 2 -> XYZ(D65) matrix (the gamut's declared
/// `xyz_to_rgb`), derived in f64 by the independent oracle.
pub(crate) const DGAMUT2_TO_XYZ65_INVERSE: [[f64; 3]; 3] = [
    [1.5524806727641005, -0.29564003743365996, -0.16500955825710037],
    [-0.462724820494538, 1.281292066997886, 0.14559127639773503],
    [0.0, 0.0, 0.918189330639978],
];

/// The complete pinned reference chain used per pixel:
/// `inverse(M_DG2_AP0) * M_AP1_AP0`, i.e. AP1/D60 -> AP0/D60 -> (the DCTL's
/// printed CAT02 D65->D60 inverted) -> XYZ/D65 -> D-Gamut 2 linear.
pub(crate) const DJI_DLOG2_AP1_TO_DGAMUT2: [[f64; 3]; 3] = [
    [0.9345061440003459, -0.0022031394702776525, 0.06794785515875235],
    [0.04075107103644273, 0.8034933746397581, 0.15577128678091975],
    [-0.005058772922983332, 0.0047227291779441995, 1.0001502715262953],
];

/// Net AP1/D60 -> D-Gamut 2/D65 chromatic adaptation realized by the pinned
/// chain, reported as the `output_cat` matrix. It is the exact f64 inverse of the
/// DCTL's printed CAT02 D65->D60 constant (not a re-derivation from primaries);
/// the per-pixel chain still uses the printed constant through
/// [`DJI_DLOG2_AP1_TO_DGAMUT2`].
pub(crate) const DJI_DLOG2_NET_D60_TO_D65: [[f64; 3]; 3] = [
    [0.9881924154338105, -0.007877731919166615, 0.01677857961703693],
    [-0.00572153126675796, 0.9986543412975397, 0.006685125222250674],
    [0.00031342841026669386, 0.0010740524166901572, 1.078063517355657],
];
