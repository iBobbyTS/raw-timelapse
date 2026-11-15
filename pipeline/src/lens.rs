//! Lens configuration and native delivery contract. Calibration ownership stays
//! upstream in Prepare; the graph and colour modules never interpret lens models.
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
#[derive(Debug, Clone)]
pub struct Settings {
    pub mode: Mode,
    pub exiftool: PathBuf,
    pub profile_roots: Vec<PathBuf>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            exiftool: PathBuf::from("/opt/homebrew/bin/exiftool"),
            profile_roots: Vec::new(),
        }
    }
}
impl Settings {
    pub fn prepare_args(&self, args: &mut Vec<String>) {
        args.extend(["--lens-correction".into(), self.mode.token().into()]);
        if self.mode == Mode::Auto {
            args.extend(["--exiftool".into(), self.exiftool.display().to_string()]);
            for root in &self.profile_roots {
                args.extend(["--lens-profile-dir".into(), root.display().to_string()]);
            }
        }
    }
}
pub fn validate_report(value: &Json, mode: Mode) -> Result<()> {
    let invalid = || Error::new(EXIT_FRAME, "invalid lens correction contract");
    if value.get("revision").and_then(Json::as_u64) != Some(1)
        || value.get("requested").and_then(Json::as_bool) != Some(mode == Mode::Auto)
        || value.get("upstream_reversible").and_then(Json::as_bool) != Some(false)
        || (mode == Mode::Auto && value.get("collected").and_then(Json::as_bool) != Some(true))
    {
        return Err(invalid());
    }
    let components = value
        .get("components")
        .and_then(Json::as_array)
        .ok_or_else(invalid)?;
    if components.len() != 3 {
        return Err(invalid());
    }
    for (component, name) in components.iter().zip(["vignette", "distortion", "ca"]) {
        let status = component.get("status").and_then(Json::as_str);
        if component.get("component").and_then(Json::as_str) != Some(name)
            || (mode == Mode::Off && status != Some("off"))
            || (mode == Mode::Auto
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

#[cfg(test)]
mod tests {
    use super::*;
    fn report(requested: bool, status: &str) -> Json {
        crate::json::parse(&format!(r#"{{"revision":1,"requested":{requested},"collected":true,"upstream_reversible":false,"components":[{{"component":"vignette","status":"{status}"}},{{"component":"distortion","status":"{status}"}},{{"component":"ca","status":"{status}"}}]}}"#)).unwrap()
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
            validate_report(&report(true, status), Mode::Auto).unwrap();
        }
        validate_report(&report(false, "off"), Mode::Off).unwrap();
        for bad in [
            report(false, "applied"),
            report(true, "off"),
            report(true, "healthy"),
        ] {
            assert!(validate_report(&bad, Mode::Auto).is_err());
        }
        let value = report(true, "applied");
        let text = crate::json::to_pretty_string(&value);
        for broken in [
            text.replace("vignette", "distortion"),
            text.replace(": 1", ": 9"),
            text.replace("true", "false"),
        ] {
            assert!(validate_report(&crate::json::parse(&broken).unwrap(), Mode::Auto).is_err());
        }
    }
    #[test]
    fn prepare_owns_tool_discovery_and_preserves_argument_boundaries() {
        let settings = Settings {
            mode: Mode::Auto,
            exiftool: "/tool with spaces/exiftool".into(),
            profile_roots: vec!["/profiles one".into(), "/profiles two".into()],
        };
        let mut args = Vec::new();
        settings.prepare_args(&mut args);
        assert_eq!(
            args,
            [
                "--lens-correction",
                "auto",
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
        assert_eq!(off, ["--lens-correction", "off"]);
    }
}
