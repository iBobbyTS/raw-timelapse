//! Resolve one delivery colour contract before any graph work is scheduled.

use crate::cli::ReferenceGain;
use crate::color_profile::{
    self, CatRegistry, ColorProfile, OutputCatSelection, OutputTransform, Transfer,
};
use crate::error::{Error, Result, EXIT_INPUT, EXIT_TOOL, EXIT_USAGE};
use crate::order::SourceFrame;
use crate::run_log::{quote, RunLog};
use crate::{json, proc};
use std::path::Path;

pub const AUTO_LOG: &str = "auto-log";

#[derive(Debug, Clone)]
pub enum OutputColor {
    Explicit(ResolvedOutputColor),
    AutoLog {
        cat: OutputCatSelection,
        gain: Option<ReferenceGain>,
    },
}

#[derive(Debug, Clone)]
pub struct ResolvedOutputColor {
    pub profile: &'static ColorProfile,
    pub transform: OutputTransform,
    pub reference_gain: ReferenceGain,
}

impl ResolvedOutputColor {
    pub fn new(
        profile: &'static ColorProfile,
        cat: OutputCatSelection,
        gain: Option<ReferenceGain>,
    ) -> Result<Self> {
        if gain == Some(ReferenceGain::Auto) && profile.transfer == Transfer::Srgb {
            return Err(Error::new(
                EXIT_USAGE,
                "--log-reference-gain auto requires a camera Log profile; srgb-preview has no Log encoding headroom",
            ));
        }
        let transform = profile
            .resolve_output_cat(cat, &CatRegistry::standard())
            .map_err(|message| Error::new(EXIT_USAGE, message))?;
        let reference_gain = gain.unwrap_or(match profile.transfer {
            Transfer::DjiDLog => ReferenceGain::Auto,
            _ => ReferenceGain::Fixed(1.0),
        });
        Ok(Self {
            profile,
            transform,
            reference_gain,
        })
    }
}

impl OutputColor {
    pub fn resolve(
        &self,
        first: &SourceFrame,
        exiftool: &Path,
        log: &mut RunLog,
    ) -> Result<ResolvedOutputColor> {
        let (cat, gain) = match self {
            Self::Explicit(resolved) => return Ok(resolved.clone()),
            Self::AutoLog { cat, gain } => (*cat, *gain),
        };
        let (make, model) = read_camera(first, exiftool)?;
        let id = camera_log(&make, &model).map_err(|reason| camera_error(first, reason))?;
        let profile = color_profile::lookup(id).expect("Auto Log only selects registered profiles");
        let resolved = ResolvedOutputColor::new(profile, cat, gain)?;
        log.line(&format!(
            "kind=auto-log source={} make={} model={} profile={}",
            quote(&first.name),
            quote(&make),
            quote(&model),
            profile.id
        ))?;
        Ok(resolved)
    }
}

fn camera_error(first: &SourceFrame, reason: impl std::fmt::Display) -> Error {
    Error::new(EXIT_INPUT, format!(
        "Auto Log cannot select a profile from first input {:?}: {reason}. Set output_color to an explicit profile to use this input",
        first.name
    ))
}

fn read_camera(first: &SourceFrame, exiftool: &Path) -> Result<(String, String)> {
    proc::require_executable(exiftool, "exiftool (Auto Log)")?;
    let path = first
        .path
        .to_str()
        .ok_or_else(|| camera_error(first, "input path is not valid UTF-8"))?;
    let args = [
        "-config",
        "",
        "-json",
        "-G1",
        "-IFD0:Make",
        "-IFD0:Model",
        "-IFD0:UniqueCameraModel",
        "--",
        path,
    ]
    .map(str::to_owned);
    let output = proc::run_capture(exiftool, &args, EXIT_TOOL)?;
    if !output.status.success() {
        return Err(camera_error(
            first,
            format!(
                "ExifTool returned {} ({})",
                proc::exit_description(&output.status),
                proc::tail(&output.stderr, 400)
            ),
        ));
    }
    let json = json::parse(&output.stdout)
        .map_err(|reason| camera_error(first, format!("invalid ExifTool JSON: {reason}")))?;
    let json::Json::Array(records) = json else {
        return Err(camera_error(
            first,
            "ExifTool did not return a list of records",
        ));
    };
    let [record @ json::Json::Object(_)] = records.as_slice() else {
        return Err(camera_error(
            first,
            "ExifTool did not return one metadata record",
        ));
    };
    let tag = |key| {
        record
            .get(key)
            .and_then(json::Json::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let make = tag("IFD0:Make").unwrap_or_default();
    let model = tag("IFD0:Model")
        .or_else(|| tag("IFD0:UniqueCameraModel"))
        .unwrap_or_default();
    Ok((make.into(), model.into()))
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn camera_log(make: &str, model: &str) -> std::result::Result<&'static str, String> {
    const CANON_LOG_2_MODELS: &[&str] = &[
        "r1",
        "r3",
        "r5",
        "r5markii",
        "r5c",
        "r6markii",
        "r6markiii",
        "r6v",
        "r7",
        "r8",
        "r8markii",
        "c50",
        "c70",
        "c80",
        "c300markii",
        "c300markiii",
        "c400",
        "c500markii",
        "c700",
        "c700pl",
        "c700gspl",
        "c700ff",
        "c700ffpl",
    ];
    let model = normalized(model);
    match normalized(make).as_str() {
        "canon" | "canoninc" => {
            if model.is_empty() {
                return Err("Canon camera model is missing".into());
            }
            let model = model.trim_start_matches("canon").trim_start_matches("eos");
            let model = model.replace("mark3", "markiii").replace("mark2", "markii");
            Ok(if CANON_LOG_2_MODELS.contains(&model.as_str()) {
                "clog2"
            } else {
                "clog3"
            })
        }
        "dji" | "djitechnologycoltd" | "szdjitechnologycoltd" => {
            if model.is_empty() {
                return Err("DJI camera model is missing".into());
            }
            let model = model.trim_start_matches("dji").trim_start_matches("osmo");
            // The original Pocket 4P DNG records IFD0:Model as PP-041.
            Ok(match model {
                "pp041" | "pocket4p" | "pocket4pro" => "dji-dlog2-dgamut2",
                _ => "dji-dlog-dgamut",
            })
        }
        "nikon" | "nikoncorporation" => Ok("nikon-nlog-bt2020"),
        "sony" | "sonycorporation" => Ok("sony-slog3-sgamut3"),
        "" => Err("camera make is missing".into()),
        _ => Err(format!("unsupported camera make {make:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_clog2_for_each_listed_canon_model() {
        for model in [
            "R1",
            "R3",
            "R5",
            "R5 Mark II",
            "R5 C",
            "R6 Mark II",
            "R6 Mark III",
            "R6 V",
            "R7",
            "R8",
            "R8 Mark II",
            "C50",
            "C70",
            "C80",
            "C300 Mark II",
            "C300 Mark III",
            "C400",
            "C500 Mark II",
            "C700",
            "C700 PL",
            "C700 GS PL",
            "C700 FF",
            "C700 FF PL",
        ] {
            let model = format!("Canon EOS {model}");
            assert_eq!(camera_log("Canon", &model).unwrap(), "clog2", "{model}");
        }
        assert_eq!(
            camera_log(" CANON Inc. ", "Eos R6 Mark-3").unwrap(),
            "clog2"
        );
        for model in [
            "Canon EOS R6",
            "Canon EOS R50",
            "Canon EOS C300",
            "Canon EOS C500",
        ] {
            assert_eq!(camera_log("Canon", model).unwrap(), "clog3", "{model}");
        }
    }

    #[test]
    fn selects_dji_nikon_and_sony_profiles_by_camera_identity() {
        for model in [
            "PP-041",
            "DJI PP-041",
            "Osmo Pocket 4P",
            "DJI Osmo Pocket 4 Pro",
        ] {
            assert_eq!(
                camera_log("DJI", model).unwrap(),
                "dji-dlog2-dgamut2",
                "{model}"
            );
        }
        for model in ["FC3582", "Osmo Pocket 4", "Osmo Pocket 3"] {
            assert_eq!(
                camera_log("DJI", model).unwrap(),
                "dji-dlog-dgamut",
                "{model}"
            );
        }
        assert_eq!(
            camera_log("NIKON CORPORATION", "NIKON Z 8").unwrap(),
            "nikon-nlog-bt2020"
        );
        assert_eq!(
            camera_log("SONY", "ILCE-7M4").unwrap(),
            "sony-slog3-sgamut3"
        );
        assert!(camera_log("Canon", "").is_err());
        assert!(camera_log("DJI", "  ").is_err());
        assert!(camera_log("", "").is_err());
        assert!(camera_log("FUJIFILM", "X-T5").is_err());
    }
}
