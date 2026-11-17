use super::{ExportProfile, InputFormat};

/// Discover H.264/HEVC hardware encoders from the selected FFmpeg executable.
/// The runner returns combined command output and whether the command succeeded.
/// A listed encoder is only exported after it produces a packet on this machine.
pub fn hardware_profiles(
    mut run: impl FnMut(&[String]) -> Result<(bool, String), String>,
) -> Result<Vec<ExportProfile>, String> {
    let (success, list) = run(&strings(&["-hide_banner", "-encoders"]))?;
    if !success {
        return Err(format!("could not list FFmpeg encoders: {}", tail(&list)));
    }

    let mut profiles = Vec::new();
    for line in list.lines() {
        let mut fields = line.split_whitespace();
        let Some(flags) = fields.next() else { continue };
        let Some(encoder) = fields.next() else {
            continue;
        };
        if flags.len() != 6 || !flags.starts_with('V') {
            continue;
        }
        let codec = if line.contains("(codec h264)") || encoder.starts_with("h264_") {
            "h264"
        } else if line.contains("(codec hevc)") || encoder.starts_with("hevc_") {
            "hevc"
        } else {
            continue;
        };
        let (success, help) = run(&strings(&[
            "-hide_banner",
            "-h",
            &format!("encoder={encoder}"),
        ]))?;
        if !success || !hardware_capability(&help) {
            continue;
        }
        let formats = help
            .lines()
            .find_map(|line| line.trim().strip_prefix("Supported pixel formats:"));
        let Some((_, backend)) = encoder.split_once('_') else {
            continue;
        };
        let inputs: &[_] = if codec == "hevc" {
            &[InputFormat::Yuv420p10, InputFormat::Yuv420p]
        } else {
            &[InputFormat::Yuv420p]
        };
        'depth: for &input in inputs {
            for profile in candidates(encoder, backend, codec, input, formats) {
                let mut args = strings(&[
                    "-hide_banner",
                    "-nostdin",
                    "-loglevel",
                    "error",
                    "-abort_on",
                    "empty_output",
                ]);
                args.extend(profile.device_args.iter().cloned());
                args.extend(strings(&[
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("testsrc2=s=640x480:r=30,format={}", input.name()),
                    "-frames:v",
                    "1",
                ]));
                if let Some(filter) = profile.hardware_filter() {
                    args.extend(["-vf".into(), filter]);
                }
                args.extend(profile.args.iter().cloned());
                args.extend(strings(&["-f", "null", "-"]));
                if run(&args)?.0 {
                    profiles.push(profile);
                    break 'depth;
                }
            }
        }
    }
    profiles.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(profiles)
}

fn hardware_capability(help: &str) -> bool {
    help.lines().any(|line| {
        let line = line.trim();
        line.strip_prefix("General capabilities:")
            .is_some_and(|caps| caps.split_whitespace().any(|cap| cap == "hardware"))
            || line.starts_with("Supported hardware devices:")
    })
}

fn candidates(
    encoder: &str,
    backend: &str,
    codec: &'static str,
    input: InputFormat,
    formats: Option<&str>,
) -> Vec<ExportProfile> {
    let has =
        |format: &str| formats.map_or(true, |list| list.split_whitespace().any(|f| f == format));
    let semiplanar = if input.bits() == 10 { "p010le" } else { "nv12" };
    let device = match backend {
        "vaapi" => Some(("vaapi", "vaapi")),
        "vulkan" => Some(("vulkan", "vulkan")),
        "d3d12va" => Some(("d3d12va", "d3d12")),
        _ => None,
    };
    let quality = match backend {
        "videotoolbox" => &["-allow_sw", "0", "-q:v", "65"][..],
        "nvenc" => &["-rc", "vbr", "-cq", "23", "-b:v", "0"],
        "qsv" => &["-global_quality", "23"],
        "amf" => &["-rc", "qvbr", "-qvbr_quality_level", "23"],
        "vaapi" => &["-rc_mode", "ICQ", "-global_quality", "23"],
        "vulkan" => &["-rc_mode", "driver", "-b:v", "0"],
        "mf" => &[
            "-hw_encoding",
            "1",
            "-rate_control",
            "quality",
            "-quality",
            "75",
        ],
        "mediacodec" => &["-bitrate_mode", "cq", "-global_quality", "65"],
        "d3d12va" => &["-global_quality", "23"],
        _ => &["-b:v", "0"],
    };
    let layouts = device.map_or_else(
        || vec![input.name(), semiplanar],
        |(_, format)| vec![format],
    );
    layouts
        .into_iter()
        .filter(|layout| has(layout))
        .map(|layout| {
            let mut profile = ExportProfile::new(
                encoder,
                encoder,
                codec,
                input,
                &format!("+{layout}"),
                quality,
            );
            if let Some((device_type, _)) = device {
                profile.device_args = vec![
                    "-init_hw_device".into(),
                    format!("{device_type}=rawtl_export"),
                    "-filter_hw_device".into(),
                    "rawtl_export".into(),
                ];
                profile.upload_format = Some(semiplanar);
            } else if layout != input.name() {
                profile.upload_format = Some(semiplanar);
            }
            profile
        })
        .collect()
}

impl ExportProfile {
    pub(crate) fn hardware_filter(&self) -> Option<String> {
        self.upload_format.map(|format| {
            let upload = if self.device_args.is_empty() {
                ""
            } else {
                ",hwupload"
            };
            format!("scale,format={format}{upload}")
        })
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).into()).collect()
}

fn tail(text: &str) -> &str {
    let start = text
        .char_indices()
        .rev()
        .nth(800)
        .map_or(0, |(index, _)| index);
    text[start..].trim()
}
