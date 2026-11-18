//! Public run arguments and the JSON processing profile.

use crate::cli::{self, Parsed};
use crate::error::{Error, Result, EXIT_USAGE};
use crate::json::{self, Json};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::Path;

pub fn parse(args: &[String]) -> Result<Parsed> {
    if let Some(("--desktop-export", rest)) = args.split_first().map(|(first, rest)| (first.as_str(), rest)) {
        use std::io::Read;
        const LIMIT: u64 = 1024 * 1024;
        let mut contents = String::new();
        std::io::stdin().lock().take(LIMIT + 1).read_to_string(&mut contents)
            .map_err(|e| usage(format!("cannot read export settings: {e}")))?;
        if contents.len() as u64 > LIMIT {
            return Err(usage("desktop export settings exceed size limit"));
        }
        return parse_with_profile(rest, Some(&contents));
    }
    parse_with_profile(args, None)
}

fn parse_with_profile(args: &[String], desktop: Option<&str>) -> Result<Parsed> {
    match args {
        [arg] if arg == "--help" || arg == "-h" => return Ok(Parsed::Help),
        [arg] if arg == "--version" => return Ok(Parsed::Version),
        _ => {}
    }

    let mut input = None;
    let mut output = None;
    let mut profile = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let (flag, inline) = arg
            .split_once('=')
            .map_or((arg, None), |(flag, value)| (flag, Some(value)));
        let slot = match flag {
            "--input" => &mut input,
            "--output" => &mut output,
            "--profile" => &mut profile,
            _ => {
                return Err(usage(format!(
                    "unknown argument {flag:?}. Use --profile for processing and output settings"
                )))
            }
        };
        if slot.is_some() {
            return Err(usage(format!("{flag} may be given only once")));
        }
        let value = cli::take_value(args, &mut index, flag, inline)?;
        if value.is_empty() {
            return Err(usage(format!("{flag} requires a nonempty value")));
        }
        *slot = Some(value);
        index += 1;
    }

    let input = input.ok_or_else(|| usage("--input <DIR> is required"))?;
    let (profile, loaded) = if let Some(contents) = desktop {
        if profile.is_some() {
            return Err(usage("desktop export receives its settings from stdin"));
        }
        ("desktop".into(), parse_contents(contents))
    } else {
        let profile = profile.ok_or_else(|| usage("--profile <default|ABSOLUTE_FILE> is required"))?;
        let loaded = load(&profile);
        (profile, loaded)
    };
    let mut settings = loaded.map_err(|message| usage(format!("profile {profile:?}: {message}")))?;
    settings.push(format!("--input={input}"));
    if let Some(output) = &output {
        settings.push(format!("--output={output}"));
    }
    let Parsed::Run(mut options) = cli::parse_settings(&settings, false).map_err(|err| {
        usage(format!(
            "profile {profile:?} or run arguments: {}",
            err.message
        ))
    })?
    else {
        unreachable!("profile fields represent run settings");
    };
    options.profile_source = Some(profile.clone());
    if output.is_none() {
        let input = crate::guard::canonical_input(&options.input)?;
        let container = crate::graph_run::output_container(&options)?;
        let stem = input
            .file_name()
            .ok_or_else(|| usage("the input directory has no name; specify --output"))?;
        let mut video_name = OsString::from(stem);
        video_name.push(".");
        video_name.push(container.extension());
        let mut log_name = OsString::from(stem);
        log_name.push(".log");
        options.video = input.with_file_name(video_name);
        options.log = input.with_file_name(log_name);
        options.input = input;
    }
    Ok(Parsed::Run(options))
}

fn usage(message: impl Into<String>) -> Error {
    Error::new(EXIT_USAGE, message)
}

fn load(source: &str) -> std::result::Result<Vec<String>, String> {
    let contents = if source == "default" {
        include_str!("default_profile.json").to_owned()
    } else {
        let path = Path::new(source);
        if !path.is_absolute() {
            return Err("expected default or an absolute file path".into());
        }
        if !path.is_file() {
            return Err("expected a readable profile file".into());
        }
        std::fs::read_to_string(path).map_err(|err| err.to_string())?
    };
    parse_contents(&contents)
}

fn parse_contents(contents: &str) -> std::result::Result<Vec<String>, String> {
    let Json::Object(fields) = json::parse(contents)? else {
        return Err("expected a JSON object".into());
    };
    let mut seen = BTreeSet::new();
    let mut settings = Vec::new();
    for (key, value) in fields {
        if !seen.insert(key.clone()) {
            return Err(format!("duplicate field {key:?}"));
        }
        match (key.as_str(), value) {
            ("deflicker_diagnostics", Json::Bool(true)) => settings.push("--deflicker-diagnostics".into()),
            ("deflicker_diagnostics", Json::Bool(false)) => {}
            ("verify_output", Json::Bool(true)) => settings.push("--verify-output".into()),
            ("verify_output", Json::Bool(false)) => {}
            (
                "lens_profile_dirs" | "exclude_rois" | "steps" | "output_options",
                Json::Array(values),
            ) => {
                let flag = match key.as_str() {
                    "lens_profile_dirs" => "lens-profile-dir",
                    "exclude_rois" => "exclude-roi",
                    "steps" => "step",
                    _ => "output-option",
                };
                for value in values {
                    let value = value
                        .as_str()
                        .ok_or_else(|| format!("{key:?} must contain only strings"))?;
                    settings.push(format!("--{flag}={value}"));
                }
            }
            ("output_color", Json::Str(value)) => {
                settings.push(format!("--preview-profile={value}"))
            }
            (
                "white_balance" | "output_cat" | "camera_color" | "export_profile" | "export_hardware_variant" | "encoder"
                | "pix_fmt" | "container" | "codec_profile" | "resize" | "lens_correction" | "lens_vignette"
                | "lens_distortion" | "lens_ca" | "lens_resampling" | "lens_crop" | "brightness"
                | "estimator" | "ev_limits" | "roi" | "dng_linear_frame" | "ffmpeg" | "ffprobe"
                | "exiftool" | "converter",
                Json::Str(value),
            ) => {
                settings.push(format!("--{}={value}", key.replace('_', "-")));
            }
            ("fps" | "log_reference_gain", Json::Str(value)) => {
                settings.push(format!("--{}={value}", key.replace('_', "-")));
            }
            (
                "fps" | "signal_bits" | "crf" | "qscale" | "bitrate" | "period"
                | "converter_timeout",
                Json::Int(value),
            ) => {
                settings.push(format!("--{}={value}", key.replace('_', "-")));
            }
            ("strength" | "lens_vignette_strength" | "log_reference_gain", value)
                if value.as_f64().is_some() =>
            {
                settings.push(format!(
                    "--{}={}",
                    key.replace('_', "-"),
                    value.as_f64().unwrap()
                ));
            }
            _ => {
                return Err(format!(
                    "unknown profile field or invalid value for {key:?}"
                ))
            }
        }
    }
    Ok(settings)
}
