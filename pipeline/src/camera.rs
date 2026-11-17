//! Input development policy resolved from the original RAW sequence.

/// Intrinsic correction from the selected camera profile.
pub const CAMERA_PROFILE_CORRECTION: &str = "hue-sat-map";

/// Frozen input policy shared by measure and develop. Legacy is test-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraRecipe {
    LegacyInverseCm,
    Generic,
    CanonCr3,
}

impl CameraRecipe {
    pub fn native_token(self) -> &'static str {
        match self {
            Self::LegacyInverseCm => "legacy-inverse-cm",
            Self::Generic | Self::CanonCr3 => "dng-camera-to-pcs",
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            Self::LegacyInverseCm => "legacy-test",
            Self::Generic => "generic",
            Self::CanonCr3 => "canon-cr3",
        }
    }

    pub fn calibration(self) -> &'static str {
        if self.requires_canon_cr3() {
            "canon-normal-white-fixed-v1"
        } else {
            "none"
        }
    }

    pub fn requires_canon_cr3(self) -> bool {
        self == Self::CanonCr3
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraColor {
    Auto,
    Generic,
    CanonCr3,
}

impl CameraColor {
    pub fn token(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Generic => "generic",
            Self::CanonCr3 => "canon-cr3",
        }
    }

    /// Resolve after single-extension enumeration, before RAW preparation.
    /// Native verifies the original camera identity and calibration metadata.
    pub fn resolve(self, all_cr3: bool) -> Result<CameraRecipe, String> {
        match self {
            Self::Auto | Self::CanonCr3 if all_cr3 => Ok(CameraRecipe::CanonCr3),
            Self::Auto | Self::Generic => Ok(CameraRecipe::Generic),
            Self::CanonCr3 => {
                Err("--camera-color canon-cr3 requires original Canon CR3 photos".into())
            }
        }
    }
}
