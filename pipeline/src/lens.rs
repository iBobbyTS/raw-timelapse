//! Lens policy and native contracts. Prepare owns calibration; the output
//! layer owns applying a fixed sequence rectangle, never the optical formulas.
use crate::error::{Error, Result, EXIT_FRAME};
use crate::json::Json;
use std::path::PathBuf;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Off,
    Auto,
}
impl Mode {
    pub fn token(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sampling {
    Bilinear,
    #[default]
    Quality,
}
impl Sampling {
    pub fn token(self) -> &'static str {
        match self {
            Self::Bilinear => "bilinear",
            Self::Quality => "quality",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Crop {
    #[default]
    Preserve,
    Safe,
    SequenceSafe,
}
impl Crop {
    pub fn is_safe(self) -> bool {
        self != Self::Preserve
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Policy {
    pub components: [bool; 3],
    pub vignette_strength: f64,
    pub sampling: Sampling,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            components: [true; 3],
            vignette_strength: 1.,
            sampling: Sampling::Quality,
        }
    }
}
impl Policy {
    pub fn requested(self, mode: Mode, i: usize) -> bool {
        mode == Mode::Auto && self.components[i] && (i != 0 || self.vignette_strength > 0.)
    }
    pub fn args(self, mode: Mode, args: &mut Vec<String>) {
        args.extend(["--lens-correction".into(), mode.token().into()]);
        for (name, enabled) in ["--lens-vignette", "--lens-distortion", "--lens-ca"]
            .iter()
            .zip(self.components)
        {
            args.extend([
                name.to_string(),
                if enabled { "auto" } else { "off" }.into(),
            ]);
        }
        args.extend([
            "--lens-vignette-strength".into(),
            self.vignette_strength.to_string(),
            "--lens-resampling".into(),
            self.sampling.token().into(),
        ]);
    }
}
#[derive(Debug, Clone)]
pub struct Settings {
    pub mode: Mode,
    pub exiftool: PathBuf,
    pub profile_roots: Vec<PathBuf>,
    pub policy: Policy,
    pub crop: Crop,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            exiftool: "/opt/homebrew/bin/exiftool".into(),
            profile_roots: vec![],
            policy: Policy::default(),
            crop: Crop::Preserve,
        }
    }
}
impl Settings {
    pub fn prepare_args(&self, args: &mut Vec<String>) {
        self.policy.args(self.mode, args);
        if self.mode == Mode::Auto {
            args.extend(["--exiftool".into(), self.exiftool.display().to_string()]);
            for root in &self.profile_roots {
                args.extend(["--lens-profile-dir".into(), root.display().to_string()]);
            }
        }
    }
}
pub fn validate_report(value: &Json, mode: Mode, policy: Policy) -> Result<()> {
    let invalid = || Error::new(EXIT_FRAME, "invalid lens correction contract");
    if value.get("revision").and_then(Json::as_u64) != Some(2)
        || value.get("requested").and_then(Json::as_bool) != Some(mode == Mode::Auto)
        || value.get("upstream_reversible").and_then(Json::as_bool) != Some(false)
        || (mode == Mode::Auto && value.get("collected").and_then(Json::as_bool) != Some(true))
        || value.get("vignette_strength").and_then(Json::as_f64) != Some(policy.vignette_strength)
        || value.get("resampling").and_then(Json::as_str) != Some(policy.sampling.token())
    {
        return Err(invalid());
    }
    let low = value
        .get("gain_min")
        .and_then(Json::as_f64)
        .ok_or_else(invalid)?;
    let high = value
        .get("gain_max")
        .and_then(Json::as_f64)
        .ok_or_else(invalid)?;
    if !low.is_finite() || !high.is_finite() || low <= 0. || high < low {
        return Err(invalid());
    }
    let components = value
        .get("components")
        .and_then(Json::as_array)
        .ok_or_else(invalid)?;
    if components.len() != 3 {
        return Err(invalid());
    }
    for (i, (component, name)) in components
        .iter()
        .zip(["vignette", "distortion", "ca"])
        .enumerate()
    {
        let requested = policy.requested(mode, i);
        let status = component.get("status").and_then(Json::as_str);
        if component.get("component").and_then(Json::as_str) != Some(name)
            || component.get("requested").and_then(Json::as_bool) != Some(requested)
            || (!requested && status != Some("off"))
            || (requested
                && !matches!(
                    status,
                    Some(
                        "applied"
                            | "identity"
                            | "already-applied"
                            | "sdk-owned"
                            | "embedded-opcode-owned"
                            | "unknown-upstream"
                            | "no-usable-model"
                            | "unsupported-embedded"
                            | "upstream-geometry-unmapped"
                    )
                ))
        {
            return Err(invalid());
        }
    }
    Ok(())
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValidArea {
    pub width: u32,
    pub height: u32,
    pub bounds: [f64; 4],
}
impl ValidArea {
    pub fn parse(
        value: &Json,
        source_hash: &str,
        prepared_hash: &str,
        settings: &Settings,
    ) -> Result<Self> {
        let invalid = || Error::new(EXIT_FRAME, "invalid lens valid-area contract");
        if value.get("revision").and_then(Json::as_u64) != Some(1)
            || value.get("source_sha256").and_then(Json::as_str) != Some(source_hash)
            || value.get("prepared_sha256").and_then(Json::as_str) != Some(prepared_hash)
            || value.get("coordinate_system").and_then(Json::as_str)
                != Some("normalized-pixel-centres")
            || value.get("certified").and_then(Json::as_bool) != Some(true)
            || value.get("mode").and_then(Json::as_str) != Some(settings.mode.token())
            || value.get("vignette_strength").and_then(Json::as_f64)
                != Some(settings.policy.vignette_strength)
            || value.get("resampling").and_then(Json::as_str)
                != Some(settings.policy.sampling.token())
        {
            return Err(invalid());
        }
        let components = value
            .get("components")
            .and_then(Json::as_array)
            .ok_or_else(invalid)?;
        if components.len() != 3
            || components
                .iter()
                .zip(settings.policy.components)
                .any(|(v, w)| v.as_bool() != Some(w))
        {
            return Err(invalid());
        }
        let width = value
            .get("width")
            .and_then(Json::as_u32)
            .ok_or_else(invalid)?;
        let height = value
            .get("height")
            .and_then(Json::as_u32)
            .ok_or_else(invalid)?;
        let fields = value
            .get("area")
            .and_then(Json::as_array)
            .ok_or_else(invalid)?;
        if width == 0
            || height == 0
            || u64::from(width) * u64::from(height) > 200_000_000
            || fields.len() != 4
        {
            return Err(invalid());
        }
        let mut bounds = [0.; 4];
        for (i, v) in fields.iter().enumerate() {
            bounds[i] = v.as_f64().ok_or_else(invalid)?;
        }
        if bounds.iter().any(|x| !x.is_finite() || *x < 0. || *x > 1.)
            || bounds[0] >= bounds[2]
            || bounds[1] >= bounds[3]
        {
            return Err(invalid());
        }
        Ok(Self {
            width,
            height,
            bounds,
        })
    }
}
pub fn intersect(a: [f64; 4], b: [f64; 4]) -> Result<[f64; 4]> {
    let c = [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ];
    if c[0] >= c[2] || c[1] >= c[3] {
        return Err(Error::new(
            EXIT_FRAME,
            "sequence has no common lens safe area",
        ));
    }
    Ok(c)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn report(requested: bool, status: &str) -> Json {
        crate::json::parse(&format!(r#"{{"revision":2,"requested":{requested},"collected":true,"upstream_reversible":false,"vignette_strength":1,"resampling":"quality","gain_min":1,"gain_max":1,"components":[{{"component":"vignette","status":"{status}","requested":{requested}}},{{"component":"distortion","status":"{status}","requested":{requested}}},{{"component":"ca","status":"{status}","requested":{requested}}}]}}"#)).unwrap()
    }
    #[test]
    fn validates_per_component_execution_not_generic_success() {
        for status in [
            "applied",
            "identity",
            "already-applied",
            "sdk-owned",
            "embedded-opcode-owned",
            "unknown-upstream",
            "no-usable-model",
            "unsupported-embedded",
            "upstream-geometry-unmapped",
        ] {
            validate_report(&report(true, status), Mode::Auto, Policy::default()).unwrap();
        }
        validate_report(&report(false, "off"), Mode::Off, Policy::default()).unwrap();
        for bad in [
            report(false, "applied"),
            report(true, "off"),
            report(true, "healthy"),
        ] {
            assert!(validate_report(&bad, Mode::Auto, Policy::default()).is_err());
        }
        let value = report(true, "applied");
        let text = crate::json::to_pretty_string(&value);
        for broken in [
            text.replace("vignette", "distortion"),
            text.replace(": 1", ": 9"),
            text.replace("true", "false"),
        ] {
            assert!(validate_report(
                &crate::json::parse(&broken).unwrap(),
                Mode::Auto,
                Policy::default()
            )
            .is_err());
        }
    }
    #[test]
    fn policy_report_rejects_ignored_component_strength_and_sampler() {
        let policy = Policy {
            components: [false, true, true],
            vignette_strength: 0.5,
            sampling: Sampling::Quality,
        };
        let text = r#"{"revision":2,"requested":true,"collected":true,"upstream_reversible":false,"vignette_strength":0.5,"resampling":"quality","gain_min":1,"gain_max":1,"components":[{"component":"vignette","status":"off","requested":false},{"component":"distortion","status":"applied","requested":true},{"component":"ca","status":"applied","requested":true}]}"#;
        validate_report(&crate::json::parse(text).unwrap(), Mode::Auto, policy).unwrap();
        for broken in [
            text.replace("off", "applied"),
            text.replace("quality", "bilinear"),
            text.replace("0.5", "1.0"),
        ] {
            assert!(
                validate_report(&crate::json::parse(&broken).unwrap(), Mode::Auto, policy).is_err()
            );
        }
    }
    #[test]
    fn certified_area_is_bound_to_identity_and_policy_and_intersects() {
        let settings = Settings::default();
        let text = r#"{"revision":1,"source_sha256":"source","prepared_sha256":"prepared","width":64,"height":48,"area":[0.1,0.1,0.9,0.9],"coordinate_system":"normalized-pixel-centres","certified":true,"mode":"off","components":[true,true,true],"vignette_strength":1,"resampling":"quality"}"#;
        let area = ValidArea::parse(
            &crate::json::parse(text).unwrap(),
            "source",
            "prepared",
            &settings,
        )
        .unwrap();
        assert_eq!(area.bounds, [0.1, 0.1, 0.9, 0.9]);
        for broken in [
            text.replace(r#""source""#, r#""other""#),
            text.replace(r#""certified":true"#, r#""certified":false"#),
            text.replace("quality", "bilinear"),
            text.replace("0.9,0.9", "0.0,0.0"),
        ] {
            assert!(ValidArea::parse(
                &crate::json::parse(&broken).unwrap(),
                "source",
                "prepared",
                &settings
            )
            .is_err());
        }
        assert_eq!(
            intersect([0., 0., 1., 1.], [0.1, 0.2, 0.8, 0.9]).unwrap(),
            [0.1, 0.2, 0.8, 0.9]
        );
        assert!(intersect([0., 0., 0.3, 0.3], [0.4, 0.4, 1., 1.]).is_err());
    }
    #[test]
    fn prepare_owns_tool_discovery_and_preserves_argument_boundaries() {
        let settings = Settings {
            mode: Mode::Auto,
            exiftool: "/tool with spaces/exiftool".into(),
            profile_roots: vec!["/profiles one".into(), "/profiles two".into()],
            ..Default::default()
        };
        let mut args = Vec::new();
        settings.prepare_args(&mut args);
        assert_eq!(
            args,
            [
                "--lens-correction",
                "auto",
                "--lens-vignette", "auto", "--lens-distortion", "auto", "--lens-ca", "auto",
                "--lens-vignette-strength", "1", "--lens-resampling", "quality",
                "--exiftool",
                "/tool with spaces/exiftool",
                "--lens-profile-dir",
                "/profiles one",
                "--lens-profile-dir",
                "/profiles two"
            ]
        );
        let mut off = Vec::new();
        Settings::default().prepare_args(&mut off);
        assert_eq!(off, ["--lens-correction", "off", "--lens-vignette", "auto", "--lens-distortion", "auto", "--lens-ca", "auto", "--lens-vignette-strength", "1", "--lens-resampling", "quality"]);
    }
}
