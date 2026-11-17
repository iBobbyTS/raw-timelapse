//! Export profiles control FFmpeg encoding and the rawvideo pipe layout.
//! Output colour, geometry, frame rate and the destination belong to the caller.
mod format;
mod hardware;

pub use format::InputFormat;
pub use hardware::hardware_profiles;

#[derive(Clone, Debug, PartialEq)]
pub enum Selection {
    Named(String),
    Manual(ExportProfile),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExportProfile {
    pub name: String,
    pub input: InputFormat,
    pub(crate) encoder: String,
    pub(crate) codec: Option<&'static str>,
    pub(crate) encoder_format: Option<String>,
    pub(crate) decoded_format: Option<String>,
    pub(crate) decoded_profile: Option<&'static str>,
    pub(crate) args: Vec<String>,
    pub(crate) device_args: Vec<String>,
    /// The layout FFmpeg must convert to before uploading hardware frames.
    pub(crate) upload_format: Option<&'static str>,
}

pub const SOFTWARE_PROFILES: [&str; 7] = [
    "x265-12bit-444",
    "x265-10bit-422",
    "x265-8bit-420",
    "dnxhr-sq",
    "dnxhr-444",
    "prores-422",
    "prores-4444",
];

impl ExportProfile {
    pub fn encoder(&self) -> &str {
        &self.encoder
    }

    pub fn ffmpeg_args(&self) -> &[String] {
        &self.args
    }

    pub(crate) fn decoder_or_encoder_format(&self) -> Option<&str> {
        self.decoded_format
            .as_deref()
            .or(self.encoder_format.as_deref())
    }

    pub fn software(name: &str) -> Option<Self> {
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
        software_with_cores(name, cores)
    }

    /// The caller supplies output-side FFmpeg argv (one value per option).
    /// FFmpeg preflight checks the selected encoder and its private options.
    pub fn manual(input: InputFormat, args: Vec<String>) -> Result<Self, String> {
        let mut encoder = None;
        let mut encoder_format = None;
        let mut pairs = args.chunks_exact(2);
        for pair in &mut pairs {
            let [key, value] = [&pair[0], &pair[1]];
            let key_name = key.split(':').next().unwrap_or_default();
            if !key_name.strip_prefix('-').is_some_and(|flag| {
                !flag.is_empty() && flag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            }) || pipeline_option(key_name)
            {
                return Err(format!(
                    "export FFmpeg option {key:?} is not an encoding option"
                ));
            }
            if let Some(stream) = key.split_once(':').map(|(_, stream)| stream) {
                if !matches!(stream, "v" | "v:0" | "0") {
                    return Err(format!(
                        "export FFmpeg option {key:?} must address the sole video stream"
                    ));
                }
            }
            if value.is_empty()
                || value.contains('\0')
                || value.as_bytes().get(1).is_some_and(u8::is_ascii_alphabetic)
                    && value.starts_with('-')
            {
                return Err(format!("export FFmpeg option {key:?} requires a value"));
            }
            match key_name {
                "-c" | "-codec" | "-vcodec" => {
                    if !value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                        || value == "copy"
                    {
                        return Err(format!("invalid export video encoder {value:?}"));
                    }
                    if encoder.replace(value.clone()).is_some() {
                        return Err(
                            "manual export requires exactly one FFmpeg video encoder".into()
                        );
                    }
                }
                "-pix_fmt" => {
                    let format = value.strip_prefix('+').unwrap_or(value);
                    if !format.is_empty()
                        && !format
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    {
                        return Err(format!("invalid FFmpeg output pixel format {value:?}"));
                    }
                    if encoder_format.is_some() {
                        return Err("manual export accepts one FFmpeg output pixel format".into());
                    }
                    encoder_format = Some(format.to_owned());
                }
                _ => {}
            }
        }
        if !pairs.remainder().is_empty() {
            return Err("manual export FFmpeg arguments must be option/value pairs".into());
        }
        let encoder = encoder.ok_or("manual export requires a FFmpeg video encoder (-c:v)")?;
        let codec = match encoder.as_str() {
            "libx264" | "libx264rgb" => Some("h264"),
            "libx265" => Some("hevc"),
            "prores" | "prores_ks" | "prores_aw" => Some("prores"),
            "dnxhd" => Some("dnxhd"),
            "ffv1" => Some("ffv1"),
            name if name.starts_with("h264_") => Some("h264"),
            name if name.starts_with("hevc_") => Some("hevc"),
            _ => None,
        };
        Ok(Self {
            name: "manual".into(),
            input,
            encoder,
            codec,
            encoder_format: encoder_format.filter(|format| !format.is_empty()),
            decoded_format: None,
            decoded_profile: None,
            args,
            device_args: vec![],
            upload_format: None,
        })
    }

    pub(crate) fn new(
        name: &str,
        encoder: &str,
        codec: &'static str,
        input: InputFormat,
        encoder_format: &str,
        args: &[&str],
    ) -> Self {
        let mut parameters = vec![
            "-c:v".into(),
            encoder.into(),
            "-pix_fmt".into(),
            encoder_format.into(),
        ];
        parameters.extend(args.iter().map(|param| (*param).into()));
        Self {
            name: name.into(),
            input,
            encoder: encoder.into(),
            codec: Some(codec),
            encoder_format: Some(encoder_format.trim_start_matches('+').into()),
            decoded_format: Some(input.name().into()),
            decoded_profile: None,
            args: parameters,
            device_args: vec![],
            upload_format: None,
        }
    }
}

fn software_with_cores(name: &str, cores: usize) -> Option<ExportProfile> {
    use InputFormat::*;
    let (input, crf, preset) = match name {
        "x265-12bit-444" => (Yuv444p12, "4", Some("slow")),
        "x265-10bit-422" => (Yuv422p10, "8", Some("slow")),
        "x265-8bit-420" => (Yuv420p, "20", None),
        _ => {
            let (encoder, codec, input, profile, label) = match name {
                "dnxhr-sq" => ("dnxhd", "dnxhd", Yuv422p, "dnxhr_sq", "DNXHR SQ"),
                "dnxhr-444" => ("dnxhd", "dnxhd", Yuv444p10, "dnxhr_444", "DNXHR 444"),
                "prores-422" => ("prores_ks", "prores", Yuv422p10, "2", "Standard"),
                "prores-4444" => ("prores_ks", "prores", Yuv444p10, "4", "4444"),
                _ => return None,
            };
            let mut export = ExportProfile::new(
                name,
                encoder,
                codec,
                input,
                input.name(),
                &["-profile:v", profile],
            );
            export.decoded_profile = Some(label);
            return Some(export);
        }
    };
    let mut export =
        ExportProfile::new(name, "libx265", "hevc", input, input.name(), &["-crf", crf]);
    if let Some(preset) = preset {
        export.args.extend(["-preset".into(), preset.into()]);
    }
    let pools = ((cores as f64) * 0.7).round().max(1.0) as usize;
    export
        .args
        .extend(["-x265-params".into(), format!("pools={pools}")]);
    Some(export)
}

fn pipeline_option(key: &str) -> bool {
    matches!(
        key,
        "-i" | "-f"
            | "-map"
            | "-map_metadata"
            | "-map_chapters"
            | "-metadata"
            | "-s"
            | "-r"
            | "-fps_mode"
            | "-vsync"
            | "-aspect"
            | "-frames"
            | "-vframes"
            | "-t"
            | "-to"
            | "-ss"
            | "-shortest"
            | "-fs"
            | "-stream_loop"
            | "-itsoffset"
            | "-pixel_format"
            | "-video_size"
            | "-framerate"
            | "-vf"
            | "-lavfi"
            | "-filter"
            | "-filter_complex"
            | "-filter_script"
            | "-filter_complex_script"
            | "-an"
            | "-acodec"
            | "-sn"
            | "-scodec"
            | "-dn"
            | "-v"
            | "-loglevel"
            | "-y"
            | "-n"
            | "-progress"
            | "-report"
            | "-movflags"
            | "-pass"
            | "-passlogfile"
            | "-vstats"
            | "-vstats_file"
            | "-stats_enc_pre"
            | "-stats_enc_post"
            | "-stats_mux_pre"
            | "-colorspace"
            | "-color_primaries"
            | "-color_trc"
            | "-color_range"
            | "-init_hw_device"
            | "-filter_hw_device"
            | "-hwaccel"
    )
}
