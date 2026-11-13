//! Standalone fake `dng-linear-frame` used only by the pipeline integration
//! tests. It is compiled with plain `rustc` (no dependencies) by the test
//! harness and configured through `FAKE_NATIVE_CONFIG`; it never ships.
//!
//! Supported producer options: --white-balance, --converter,
//! --converter-timeout, --exposure-ev, --emit-analysis, --analysis-roi,
//! --no-png. The analysis sidecar follows the frozen S01 schema.

#[path = "../../src/digest.rs"]
mod digest;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_EDGE: u32 = 1024;

#[derive(Clone)]
struct FrameSpec {
    name: String,
    own_neutral: String,
    log_level: f64,
    object_fraction: f64,
    capture_time: Option<String>,
    fixture: Option<String>,
    pattern: String,
}

struct Config {
    width: u32,
    height: u32,
    grid_nx: usize,
    grid_ny: usize,
    ignore_override: bool,
    /// Report `override` with a fixed neutral even when no --white-balance was
    /// passed (models a producer that invents an override).
    override_when_unset: bool,
    /// Report `as-shot` while applying a neutral that differs from the frame's
    /// own as_shot_neutral (models a producer that lies about what it applied).
    as_shot_mismatch: bool,
    fail_on: Option<String>,
    gain_scale: f64,
    /// Emit an empty `source_sha256` in the metadata sidecar. Models a forged
    /// frame sidecar that fails the typed semantic contract.
    empty_source_sha256: bool,
    /// Corrupt only final-frame metadata; analysis/application records stay valid.
    frame_source_sha256_defect: Option<String>,
    /// Omit `kind` from the analysis sidecar.
    analysis_omit_kind: bool,
    /// Report an analysis source hash that differs from the frame metadata.
    analysis_sha_mismatch: bool,
    rev_defect: Option<String>,
    bad_magic: bool,
    short_pixels: bool,
    stall_pixels: bool,
    omit_metadata: bool,
    descriptor_geometry_mismatch: bool,
    nonzero_after_records: bool,
    frame_fail_on: Option<String>,
    prepared_route: Option<String>,
    prepared_source_override: Option<String>,
    prepared_dng_override: Option<String>,
    prepared_metadata_defect: Option<String>,
    prepare_fail: bool,
    frames: Vec<FrameSpec>,
}

/// Neutral applied by the two lying knobs; never equal to a test frame's own.
const LIE_NEUTRAL: &str = "0.42,1,0.66";

struct Options {
    input: PathBuf,
    png: PathBuf,
    json: PathBuf,
    white_balance: Option<String>,
    exposure_ev: f64,
    emit_analysis: Option<PathBuf>,
    analysis_roi: [f64; 4],
    no_png: bool,
    stdout_mode: Option<String>,
    camera_method: String,
    /// Prepared-context path for a --prepared consumer invocation.
    prepared: Option<PathBuf>,
    /// --prepare operation inputs.
    prepare: Option<PrepareArgs>,
}

struct PrepareArgs {
    scratch: PathBuf,
    emit: PathBuf,
}

fn main() {
    match run() {
        Ok(()) => {}
        Err((code, message)) => {
            eprintln!("{message}");
            std::process::exit(code);
        }
    }
}

fn run() -> Result<(), (i32, String)> {
    let options = parse_args()?;
    let config_path = env::var("FAKE_NATIVE_CONFIG")
        .map_err(|_| (2, "FAKE_NATIVE_CONFIG is not set".to_string()))?;
    let config = load_config(Path::new(&config_path))?;
    if let Some(prepare) = &options.prepare {
        return run_prepare(&config, &options, prepare);
    }
    let context = match &options.prepared {
        Some(path) => Some(FakeContext::load(path)?),
        None => None,
    };
    let source_path = match &context {
        Some(context) => context.source.clone(),
        None => options.input.clone(),
    };
    let name = source_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| (3, "input has no file name".to_string()))?
        .to_string();
    if let Ok(path) = env::var("FAKE_NATIVE_PIDS") {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(f, "{}", std::process::id()).unwrap();
    }
    if matches!(options.stdout_mode.as_deref(), Some("--stdout-frame" | "--stdout-frame-f32"))
        && config.frame_fail_on.as_deref() == Some(&name)
    {
        return Err((3, "synthetic mid-run frame failure".into()));
    }
    if let Some(failing) = &config.fail_on {
        if failing == &name {
            return Err((3, "synthetic frame failure".to_string()));
        }
    }
    let spec = config
        .frames
        .iter()
        .find(|frame| frame.name == name)
        .ok_or_else(|| (4, format!("no fixture for {name}")))?;
    let (applied, wb_source) = if config.override_when_unset && options.white_balance.is_none() {
        (LIE_NEUTRAL.to_string(), "override")
    } else if config.as_shot_mismatch && options.white_balance.is_none() {
        (LIE_NEUTRAL.to_string(), "as-shot")
    } else {
        match (&options.white_balance, config.ignore_override) {
            (Some(override_wb), false) => (override_wb.clone(), "override"),
            _ => (
                context
                    .as_ref()
                    .map(|context| context.as_shot_neutral.clone())
                    .unwrap_or_else(|| spec.own_neutral.clone()),
                "as-shot",
            ),
        }
    };
    let source_sha = match &context {
        Some(context) => context.source_sha256.clone(),
        None => {
            if config.empty_source_sha256 {
                String::new()
            } else {
                digest::sha256(&read_file(&source_path)?)
            }
        }
    };
    let source_sha = if config.empty_source_sha256 {String::new()}else{source_sha};
    let gain = options.exposure_ev.exp2();
    let tiles = tile_levels(
        &config,
        spec,
        options.exposure_ev,
        options.stdout_mode.as_deref() == Some("--stdout-analysis")
            && (options.exposure_ev == 0.0 || options.exposure_ev.abs() == 0.25),
    );

    if !options.no_png && options.stdout_mode.is_none() {
        match (&spec.fixture, options.exposure_ev) {
            (Some(fixture), ev) if ev == 0.0 => {
                fs::copy(fixture, &options.png)
                    .map_err(|err| (5, format!("cannot copy fixture: {err}")))?;
            }
            _ => write_png(&options.png, &config, &tiles)?,
        }
    }

    let reported_capture = match &context {
        Some(context) => context.capture_time.clone(),
        None => spec.capture_time.clone(),
    };
    let capture = match &reported_capture {
        Some(value) => json_string(value),
        None => "null".to_string(),
    };
    let own_neutral = context
        .as_ref()
        .map(|context| context.as_shot_neutral.clone())
        .unwrap_or_else(|| spec.own_neutral.clone());
    let analysis_pointer = match &options.emit_analysis {
        Some(path) => json_string(&path.display().to_string()),
        None => "null".to_string(),
    };
    let metadata = format!(
        "{{\"schema_version\":1,\"source\":{},\"source_sha256\":{},\"width\":{},\"height\":{},\
\"applied_white_balance\":[{applied}],\"white_balance_source\":\"{wb_source}\",\
\"as_shot_neutral\":[{own}],\"observed_scene_min\":0,\"observed_scene_max\":1,\
\"quantization\":{{\"scale\":16384,\"rounding\":\"nearest; ties upward\",\
\"negative_policy\":\"clip to 0\",\"upper_policy\":\"above 65535/16384 clips to 65535\",\
\"negative_samples\":0,\"above_upper_samples\":0,\"transfer\":\"linear; no OETF\"}},\
\"exposure_ev\":{ev},\"gain\":{gain},\"capture_time\":{capture},\
\"analysis_sidecar\":{analysis_pointer}}}\n",
        json_string(&source_path.display().to_string()),
        json_string(&source_sha),
        config.width,
        config.height,
        own = own_neutral,
        ev = options.exposure_ev,
    );
    let mut metadata = metadata;
    if let Some(context) = &context {
        let defect = config.prepared_metadata_defect.as_deref();
        if defect != Some("missing") {
            let route = if defect == Some("route") {
                "linearraw-sdk".to_string()
            } else {
                context.route.clone()
            };
            let dng = if defect == Some("dng") {
                context.source.display().to_string()
            } else {
                context.dng.display().to_string()
            };
            let sha = if defect == Some("sha") {
                "0".repeat(64)
            } else {
                context.dng_sha256.clone()
            };
            let generated = if defect == Some("generated") {
                !context.generated
            } else {
                context.generated
            };
            let consumed = defect != Some("consumed");
            metadata.insert_str(
                1,
                &format!(
                    "\"route\":{},\"prepared\":{{\"schema_version\":1,\"dng\":{},\"sha256\":{},\"generated\":{},\"consumed_context\":{}}},",
                    json_string(&route),
                    json_string(&dng),
                    json_string(&sha),
                    generated,
                    consumed
                ),
            );
        }
    }
    let metadata = if config.omit_metadata {
        metadata.replace(&format!("\"width\":{},", config.width), "")
    } else {
        metadata
    };
    let metadata = if matches!(options.stdout_mode.as_deref(), Some("--stdout-frame" | "--stdout-frame-f32")) {
        let field = format!("\"source_sha256\":{},", json_string(&source_sha));
        match config.frame_source_sha256_defect.as_deref() {
            Some("omit") => metadata.replace(&field, ""),
            Some("null") => metadata.replace(&field, "\"source_sha256\":null,"),
            Some("non-string") => metadata.replace(&field, "\"source_sha256\":7,"),
            Some("empty") => metadata.replace(&field, "\"source_sha256\":\"\","),
            None => metadata,
            Some(other) => return Err((2, format!("unknown frame source hash defect: {other}"))),
        }
    } else {
        metadata
    };
    let is_f32 = options.stdout_mode.as_deref() == Some("--stdout-frame-f32");
    let mut metadata = metadata;
    {
        let defect = config.rev_defect.as_deref();
        let model = if defect == Some("mixed-model") && name.contains("2") {
            "Canon EOS R6"
        } else if defect == Some("non-canon") {
            "Nikon Z9"
        } else {
            "Canon EOS R7"
        };
        let method = if defect == Some("method") { "bogus" } else { &options.camera_method };
        let effective = if defect == Some("effective") {
            "bogus"
        } else if options.camera_method == "dng-camera-to-pcs" {
            "dng-camera-to-pcs-forward-matrix"
        } else { "legacy-inverse-cm" };
        let contract = format!("\"color_contract_version\":1,\"color\":{{\"profile_fingerprint\":\"controlled-test-profile\",\"algorithm_revision\":3,\"exposure_calibration\":{{\"applied\":true,\"method\":\"canon-normal-white-fixed-v1\",\"adc_maximum\":65535,\"black\":0,\"normal_white\":65535,\"dng_white\":65535,\"dng_black\":0,\"storage_scale\":1,\"normal_white_gain\":1,\"fixed_scene_gain\":1.8,\"native_scene_gain\":1.8,\"scene_to_reflection\":0.9,\"auto_tone_normalization\":false,\"baseline_exposure_applied\":false}},\"hue_sat_map_present\":false,\"hue_sat_map_applied\":false,\"camera_to_xyz_d50\":[[1,0,0],[0,1,0],[0,0,1]],\"camera_to_acescg\":[[1,0,0],[0,1,0],[0,0,1]],\"method_requested\":\"{method}\",\"method_effective\":\"{effective}\",\"camera_model\":\"{model}\"}},");
        let contract = if defect == Some("old-calibration") {
            contract.replace("\"algorithm_revision\":3", "\"algorithm_revision\":1")
        } else if defect == Some("skipped-fixed") {
            contract.replace("\"applied\":true", "\"applied\":false")
        } else if defect == Some("auto-tone") {
            contract.replace("\"auto_tone_normalization\":false", "\"auto_tone_normalization\":true")
        } else if defect == Some("double-scale") {
            contract.replace("\"native_scene_gain\":1.8", "\"native_scene_gain\":3.6")
        } else if defect == Some("skipped-hsm") {
            contract.replace(
                "\"hue_sat_map_present\":false",
                "\"hue_sat_map_present\":true",
            )
        } else {
            contract
        };
        if defect != Some("missing-contract") {
            metadata.insert_str(1, &contract);
        }
    }
    if is_f32 {
        metadata.insert_str(1, "\"linear_pixel_format\":\"rgbf32le\",");
        let start = metadata.find("\"quantization\":{").unwrap();
        let end = metadata[start..].find("},").unwrap() + start + 2;
        metadata.replace_range(start..end, "\"quantization\":{\"dtype\":\"f32\",\"negative_policy\":\"preserve\",\"upper_policy\":\"preserve\"},");
    }
    if let Some(mode) = &options.stdout_mode {
        let mut out = std::io::stdout().lock();
        out.write_all(if config.bad_magic {
            b"BADMAGIC"
        } else if is_f32 && config.rev_defect.as_deref() != Some("version") {
            b"RAWTLST2"
        } else {
            b"RAWTLST1"
        })
        .unwrap();
        record(&mut out, 1, metadata.as_bytes())?;
        if mode == "--stdout-analysis" {
            record(
                &mut out,
                2,
                sidecar_json(&config, spec, &tiles, &source_sha, &options, gain).as_bytes(),
            )?;
        } else {
            let desc = format!("{{\"width\":{},\"height\":{},\"pixel_format\":\"rgb48le\",\"pixels_sha256\":\"{}\"}}", if config.descriptor_geometry_mismatch {config.width+1} else {config.width}, config.height, "a".repeat(64));

            if !is_f32 {
                record(&mut out, 3, desc.as_bytes())?;
            }
            let mut pixels = Vec::new();
            if let Some(fixture) = &spec.fixture {
                let decoded =
                    std::process::Command::new("/opt/homebrew/opt/ffmpeg-full/bin/ffmpeg")
                        .args([
                            "-v", "error", "-nostdin", "-i", fixture, "-pix_fmt", "rgb48le", "-f",
                            "rawvideo", "-",
                        ])
                        .output()
                        .unwrap();
                if !decoded.status.success() {
                    return Err((5, "fixture decode failed".into()));
                }
                for code in decoded.stdout.chunks_exact(2) {
                    let value = u16::from_le_bytes([code[0], code[1]]) as f64
                        * (options.exposure_ev * config.gain_scale).exp2();
                    if is_f32 {
                        pixels.extend_from_slice(&((value / 16384.0) as f32).to_le_bytes());
                    } else {
                        pixels.extend_from_slice(&(value.round().clamp(0., 65535.) as u16).to_le_bytes());
                    }
                }
            } else {
                for y in 0..config.height {
                    for x in 0..config.width {
                        let j = y as usize * config.grid_ny / config.height as usize;
                        let i = x as usize * config.grid_nx / config.width as usize;
                        let value = tiles[j * config.grid_nx + i].0.exp2();
                        for _ in 0..3 {
                            if is_f32 { pixels.extend_from_slice(&(value as f32).to_le_bytes()); }
                            else { pixels.extend_from_slice(&quantize(value).to_le_bytes()); }
                        }
                    }
                }
            }
            if is_f32 {
                match config.rev_defect.as_deref() {
                    Some("nan") => pixels[..4].copy_from_slice(&f32::NAN.to_le_bytes()),
                    Some("infinity") => pixels[..4].copy_from_slice(&f32::INFINITY.to_le_bytes()),
                    _ => (),
                }
                let hash = if config.rev_defect.as_deref() == Some("hash") {
                    "0".repeat(64)
                } else {
                    digest::sha256(&pixels)
                };
                let format = if config.rev_defect.as_deref() == Some("format") {
                    "rgb48le"
                } else {
                    "rgbf32le"
                };
                let width = if config.descriptor_geometry_mismatch {
                    config.width + 1
                } else {
                    config.width
                };
                let desc = format!("{{\"width\":{width},\"height\":{},\"pixel_format\":\"{format}\",\"byte_length\":{},\"color_contract_version\":1,\"space\":\"AP1/D60\",\"transfer\":\"linear\",\"pixels_sha256\":\"{hash}\"}}", config.height, pixels.len());
                record(&mut out, 3, desc.as_bytes())?;
            }
            if config.stall_pixels {
                out.flush().unwrap();
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
            if config.short_pixels {
                out.write_all(&[4, 0, 0, 0]).unwrap();
                out.write_all(&(pixels.len() as u64).to_le_bytes()).unwrap();
                out.write_all(&pixels[..pixels.len() - 1]).unwrap();
            } else {
                record(&mut out, 4, &pixels)?;
            }
        }
        out.flush().map_err(|e| (50, e.to_string()))?;
        if config.nonzero_after_records {
            return Err((3, "synthetic failure after complete records".into()));
        }
    } else {
        fs::write(&options.json, metadata).map_err(|e| (5, e.to_string()))?;
        if let Some(path) = &options.emit_analysis {
            fs::write(
                path,
                sidecar_json(&config, spec, &tiles, &source_sha, &options, gain),
            )
            .map_err(|e| (5, e.to_string()))?;
        }
    }
    Ok(())
}

struct FakeContext {
    source: PathBuf,
    source_sha256: String,
    route: String,
    dng: PathBuf,
    dng_sha256: String,
    generated: bool,
    as_shot_neutral: String,
    capture_time: Option<String>,
}

impl FakeContext {
    fn load(path: &Path) -> Result<Self, (i32, String)> {
        let text = fs::read_to_string(path)
            .map_err(|error| (26, format!("cannot read prepared context: {error}")))?;
        let mut lines = text.split('\n');
        if lines.next() != Some("rawtl-prepared-frame/1") {
            return Err((26, "unsupported prepared context header".to_string()));
        }
        let mut fields = std::collections::BTreeMap::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err((26, "malformed prepared context field".to_string()));
            };
            if fields
                .insert(key.to_string(), percent_decode(value)?)
                .is_some()
            {
                return Err((26, format!("duplicate prepared context field {key}")));
            }
        }
        let get = |key: &str| -> Result<String, (i32, String)> {
            fields
                .get(key)
                .cloned()
                .ok_or_else(|| (26, format!("missing prepared context field {key}")))
        };
        let generated = match get("generated")?.as_str() {
            "0" => false,
            "1" => true,
            _ => return Err((26, "invalid prepared generated flag".to_string())),
        };
        let capture_time = match get("has_capture_time")?.as_str() {
            "1" => Some(get("capture_time")?),
            "0" => None,
            _ => return Err((26, "invalid has_capture_time".to_string())),
        };
        Ok(Self {
            source: PathBuf::from(get("source")?),
            source_sha256: get("source_sha256")?,
            route: get("route")?,
            dng: PathBuf::from(get("prepared_dng")?),
            dng_sha256: get("prepared_sha256")?,
            generated,
            as_shot_neutral: get("as_shot_neutral")?,
            capture_time,
        })
    }
}

fn percent_decode(text: &str) -> Result<String, (i32, String)> {
    fn digit(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err((26, "truncated prepared context escape".to_string()));
            }
            match (digit(bytes[index + 1]), digit(bytes[index + 2])) {
                (Some(hi), Some(lo)) => out.push((hi << 4) | lo),
                _ => return Err((26, "invalid prepared context escape".to_string())),
            }
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| (26, "prepared context is not UTF-8".to_string()))
}

fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte == b'%' || byte < 32 || byte == 127 {
            out.push_str(&format!("%{byte:02x}"));
        } else {
            out.push(byte as char);
        }
    }
    out
}

fn run_prepare(
    config: &Config,
    options: &Options,
    prepare: &PrepareArgs,
) -> Result<(), (i32, String)> {
    if config.prepare_fail {
        return Err((3, "synthetic prepare failure".to_string()));
    }
    let input = fs::canonicalize(&options.input)
        .map_err(|error| (26, format!("cannot resolve source: {error}")))?;
    let name = input
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| (3, "source has no file name".to_string()))?
        .to_string();
    let spec = config
        .frames
        .iter()
        .find(|frame| frame.name == name)
        .ok_or_else(|| (4, format!("no fixture for {name}")))?;
    if !prepare.scratch.is_dir() {
        return Err((26, "prepare scratch directory does not exist".to_string()));
    }
    let route = config.prepared_route.clone().unwrap_or_else(|| {
        if input
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("dng"))
        {
            "linearraw-sdk".to_string()
        } else {
            "vendor-raw-converter-linearraw-sdk".to_string()
        }
    });
    let generated = route != "linearraw-sdk";
    let source_sha = digest::sha256(&read_file(&input)?);
    let dng = if generated {
        let linear = prepare.scratch.join("linear");
        if linear.exists() {
            return Err((26, "prepare scratch already contains a linear directory".to_string()));
        }
        fs::create_dir(&linear)
            .map_err(|error| (26, format!("cannot create scratch: {error}")))?;
        let dng = linear.join(&name);
        fs::copy(&input, &dng)
            .map_err(|error| (26, format!("cannot stage prepared DNG: {error}")))?;
        dng
    } else {
        input.clone()
    };
    let dng = config
        .prepared_dng_override
        .clone()
        .map(PathBuf::from)
        .unwrap_or(dng);
    let dng_sha = if generated {
        digest::sha256(&read_file(&dng)?)
    } else {
        source_sha.clone()
    };
    let source = config
        .prepared_source_override
        .clone()
        .map(PathBuf::from)
        .unwrap_or(input);
    let capture = match &spec.capture_time {
        Some(value) => format!("has_capture_time=1\ncapture_time={}\n", percent_encode(value)),
        None => "has_capture_time=0\n".to_string(),
    };
    let text = format!(
        "rawtl-prepared-frame/1\nsource={}\nsource_sha256={}\nroute={}\nprepared_dng={}\nprepared_sha256={}\ngenerated={}\nmain_ifd_width={}\nmain_ifd_height={}\nas_shot_neutral={}\n{}converter_used={}\n",
        percent_encode(&source.display().to_string()),
        source_sha,
        route,
        percent_encode(&dng.display().to_string()),
        dng_sha,
        if generated { 1 } else { 0 },
        config.width,
        config.height,
        percent_encode(&spec.own_neutral),
        capture,
        if generated { 1 } else { 0 },
    );
    fs::write(&prepare.emit, text)
        .map_err(|error| (26, format!("cannot write prepared context: {error}")))?;
    Ok(())
}

fn parse_args() -> Result<Options, (i32, String)> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "--prepare") {
        let input = PathBuf::from(args.get(1).ok_or((2, "missing source".to_string()))?);
        let mut scratch = None;
        let mut emit = None;
        let mut index = 2;
        while index < args.len() {
            match args[index].as_str() {
                "--scratch" => {
                    scratch = Some(PathBuf::from(value(&args, &mut index)?));
                    index += 1;
                }
                "--emit-prepared" => {
                    emit = Some(PathBuf::from(value(&args, &mut index)?));
                    index += 1;
                }
                "--converter" | "--converter-timeout" => {
                    let _ = value(&args, &mut index)?;
                    index += 1;
                }
                other => return Err((2, format!("unexpected argument: {other}"))),
            }
        }
        return Ok(Options {
            input,
            png: PathBuf::new(),
            json: PathBuf::new(),
            white_balance: None,
            exposure_ev: 0.0,
            emit_analysis: None,
            analysis_roi: [0.0, 0.0, 1.0, 1.0],
            no_png: false,
            stdout_mode: None,
            camera_method: "legacy-inverse-cm".to_string(),
            prepared: None,
            prepare: Some(PrepareArgs {
                scratch: scratch.ok_or((2, "--prepare requires --scratch".to_string()))?,
                emit: emit.ok_or((2, "--prepare requires --emit-prepared".to_string()))?,
            }),
        });
    }
    let stdout_mode = args
        .first()
        .filter(|s| {
            s.as_str() == "--stdout-analysis"
                || s.as_str() == "--stdout-frame"
                || s.as_str() == "--stdout-frame-f32"
        })
        .cloned();
    let mut prepared = None;
    let (input, png, json, start) = if stdout_mode.is_some() {
        let first = args.get(1).ok_or((2, "missing input".to_string()))?;
        if first == "--prepared" {
            prepared = Some(PathBuf::from(
                args.get(2).ok_or((2, "missing prepared context".to_string()))?,
            ));
            (PathBuf::new(), PathBuf::new(), PathBuf::new(), 3)
        } else {
            (PathBuf::from(first), PathBuf::new(), PathBuf::new(), 2)
        }
    } else {
        if args.len() < 3 {
            return Err((2, "expected input png json".into()));
        }
        (
            PathBuf::from(&args[0]),
            PathBuf::from(&args[1]),
            PathBuf::from(&args[2]),
            3,
        )
    };
    let mut camera_method = "legacy-inverse-cm".to_string();
    let mut white_balance = None;
    let mut exposure_ev = 0.0;
    let mut emit_analysis = None;
    let mut analysis_roi = [0.0, 0.0, 1.0, 1.0];
    let mut no_png = false;
    let mut index = start;
    while index < args.len() {
        match args[index].as_str() {
            "--camera-color-method" => {
                camera_method = value(&args, &mut index)?;
                index += 1;
            }
            "--white-balance" => {
                white_balance = Some(value(&args, &mut index)?);
                index += 1;
            }
            "--converter" | "--converter-timeout" => {
                if prepared.is_some() {
                    return Err((2, "converter options are not applicable to a prepared context".to_string()));
                }
                let _ = value(&args, &mut index)?;
                index += 1;
            }
            "--exposure-ev" => {
                exposure_ev = value(&args, &mut index)?
                    .parse()
                    .map_err(|_| (2, "invalid --exposure-ev".to_string()))?;
                index += 1;
            }
            "--emit-analysis" => {
                emit_analysis = Some(PathBuf::from(value(&args, &mut index)?));
                index += 1;
            }
            "--analysis-roi" => {
                let text = value(&args, &mut index)?;
                let parts: Vec<&str> = text.split(',').collect();
                if parts.len() != 4 {
                    return Err((2, "invalid --analysis-roi".to_string()));
                }
                for (slot, part) in analysis_roi.iter_mut().zip(parts) {
                    *slot = part
                        .trim()
                        .parse()
                        .map_err(|_| (2, "invalid --analysis-roi".to_string()))?;
                }
                index += 1;
            }
            "--no-png" => {
                no_png = true;
                index += 1;
            }
            other => return Err((2, format!("unexpected argument: {other}"))),
        }
    }
    Ok(Options {
        input,
        png,
        json,
        white_balance,
        exposure_ev,
        emit_analysis,
        analysis_roi,
        no_png,
        stdout_mode,
        camera_method,
        prepared,
        prepare: None,
    })
}

fn value(args: &[String], index: &mut usize) -> Result<String, (i32, String)> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| (2, "missing option value".to_string()))
}

fn load_config(path: &Path) -> Result<Config, (i32, String)> {
    let text = fs::read_to_string(path)
        .map_err(|err| (2, format!("cannot read config {}: {err}", path.display())))?;
    let mut config = Config {
        width: 8,
        height: 6,
        grid_nx: 4,
        grid_ny: 2,
        ignore_override: false,
        override_when_unset: false,
        as_shot_mismatch: false,
        fail_on: None,
        gain_scale: 1.0,
        empty_source_sha256: false,
        frame_source_sha256_defect: None,
        analysis_omit_kind: false,
        analysis_sha_mismatch: false,
        rev_defect: None,
        bad_magic: false,
        short_pixels: false,
        stall_pixels: false,
        omit_metadata: false,
        descriptor_geometry_mismatch: false,
        nonzero_after_records: false,
        frame_fail_on: None,
        prepared_route: None,
        prepared_source_override: None,
        prepared_dng_override: None,
        prepared_metadata_defect: None,
        prepare_fail: false,
        frames: vec![],
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("frame=") {
            let fields: Vec<&str> = rest.splitn(7, '|').collect();
            if fields.len() != 7 {
                return Err((2, format!("invalid frame line: {line}")));
            }
            config.frames.push(FrameSpec {
                name: fields[0].to_string(),
                own_neutral: fields[1].to_string(),
                log_level: fields[2]
                    .parse()
                    .map_err(|_| (2, format!("invalid log level: {line}")))?,
                object_fraction: fields[3]
                    .parse()
                    .map_err(|_| (2, format!("invalid object fraction: {line}")))?,
                capture_time: if fields[4] == "-" {
                    None
                } else {
                    Some(fields[4].to_string())
                },
                fixture: if fields[5] == "-" {
                    None
                } else {
                    Some(fields[5].to_string())
                },
                pattern: fields[6].to_string(),
            });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err((2, format!("invalid config line: {line}")));
        };
        match key {
            "rev_defect" => config.rev_defect = Some(value.into()),
            "bad_magic" => config.bad_magic = value == "1",
            "stall_pixels" => config.stall_pixels = value == "1",
            "short_pixels" => config.short_pixels = value == "1",
            "descriptor_geometry_mismatch" => config.descriptor_geometry_mismatch = value == "1",
            "omit_metadata" => config.omit_metadata = value == "1",
            "nonzero_after_records" => config.nonzero_after_records = value == "1",
            "frame_fail_on" => config.frame_fail_on = Some(value.into()),
            "width" => config.width = value.parse().map_err(|_| (2, "bad width".into()))?,
            "height" => config.height = value.parse().map_err(|_| (2, "bad height".into()))?,
            "grid_nx" => config.grid_nx = value.parse().map_err(|_| (2, "bad grid_nx".into()))?,
            "grid_ny" => config.grid_ny = value.parse().map_err(|_| (2, "bad grid_ny".into()))?,
            "ignore_override" => config.ignore_override = value == "1",
            "override_when_unset" => config.override_when_unset = value == "1",
            "as_shot_mismatch" => config.as_shot_mismatch = value == "1",
            "fail_on" => config.fail_on = Some(value.to_string()),
            "gain_scale" => {
                config.gain_scale = value.parse().map_err(|_| (2, "bad gain_scale".into()))?
            }
            "frame_source_sha256_defect" => config.frame_source_sha256_defect = Some(value.into()),
            "empty_source_sha256" => config.empty_source_sha256 = value == "1",
            "analysis_omit_kind" => config.analysis_omit_kind = value == "1",
            "analysis_sha_mismatch" => config.analysis_sha_mismatch = value == "1",
            "prepared_route" => config.prepared_route = Some(value.into()),
            "prepared_source_override" => config.prepared_source_override = Some(value.into()),
            "prepared_dng_override" => config.prepared_dng_override = Some(value.into()),
            "prepared_metadata_defect" => config.prepared_metadata_defect = Some(value.into()),
            "prepare_fail" => config.prepare_fail = value == "1",
            other => return Err((2, format!("unknown config key: {other}"))),
        }
    }
    if config.frames.is_empty() || config.grid_nx == 0 || config.grid_ny == 0 {
        return Err((2, "incomplete fake config".to_string()));
    }
    Ok(config)
}

/// Base log2 level per tile plus the optional moving bright object (+1 EV).
/// Returns the measured value and whether the tile is valid; `sparse` marks all
/// but the first eight tiles invalid (a frame with no usable pair support).
fn tile_levels(config: &Config, spec: &FrameSpec, ev: f64, no_png: bool) -> Vec<(f64, bool)> {
    let mut tiles = Vec::with_capacity(config.grid_nx * config.grid_ny);
    // Analysis/probe requests (--no-png) execute the gain exactly; the
    // renderer path may under-execute it, which is what the gain probe must
    // catch (and, if it did not, what render validation must catch).
    let gained = ev * if no_png { 1.0 } else { config.gain_scale };
    for j in 0..config.grid_ny {
        for i in 0..config.grid_nx {
            let index = j * config.grid_nx + i;
            let center_x = (i as f64 + 0.5) / config.grid_nx as f64;
            let object = if center_x < spec.object_fraction {
                1.0
            } else {
                0.0
            };
            let pattern = match spec.pattern.as_str() {
                "alternating" => {
                    if (i + j) % 2 == 0 {
                        0.5
                    } else {
                        -0.5
                    }
                }
                _ => 0.0,
            };
            let valid = spec.pattern != "sparse" || index < 8;
            tiles.push((spec.log_level + pattern + object + gained, valid));
        }
    }
    tiles
}

fn sidecar_json(
    config: &Config,
    spec: &FrameSpec,
    tiles: &[(f64, bool)],
    source_sha: &str,
    options: &Options,
    gain: f64,
) -> String {
    let mut out = String::new();
    out.push_str("{\"schema_version\":2,");
    if !config.analysis_omit_kind {
        out.push_str("\"kind\":\"rawtl-analysis-sidecar\",");
    }
    let sidecar_hash = if config.analysis_sha_mismatch {
        "fake-mismatched-source"
    } else {
        source_sha
    };
    out.push_str(&format!("\"source_sha256\":{},", json_string(sidecar_hash)));
    // The accepted producer serializes sidecar numbers with 10 significant
    // digits; model that seam (the metadata JSON keeps full precision).
    let sidecar_ev = format!("{:.9e}", options.exposure_ev);
    out.push_str("\"analysis\":{");
    out.push_str(&format!(
        "\"max_edge\":{MAX_EDGE},\"downsampled_width\":{},\"downsampled_height\":{},\
\"grid_nx\":{},\"grid_ny\":{},\"roi\":[{},{},{},{}],\
\"roi_pixel_bounds\":[0,0,{},{}],\"tile_layout\":\"row-major (j*grid_nx+i)\",\
\"analysis_domain\":\"gained ACEScg AP1/D60 scene-linear FP32\",\
\"luma_weights\":[0.2126,0.7152,0.0722],\"highlight_threshold_linear\":0.980010986328125,\
\"floor\":0.00048828125,\"saturation_mask_quality\":\"camera_domain\",\
\"saturation_threshold\":0.98,\"highlight_quality\":\"analysis_ap1_threshold\",\"structure_stride\":4,\
\"exposure_ev\":{},\"gain\":{}",
        config.width,
        config.height,
        config.grid_nx,
        config.grid_ny,
        options.analysis_roi[0],
        options.analysis_roi[1],
        options.analysis_roi[2],
        options.analysis_roi[3],
        config.width,
        config.height,
        sidecar_ev,
        gain,
    ));
    out.push_str("},\"tiles\":[");
    let clip_threshold = 0.980010986328125;
    for (index, (log_level, valid)) in tiles.iter().enumerate() {
        let i = index % config.grid_nx;
        let j = index / config.grid_nx;
        let center_x = (i as f64 + 0.5) / config.grid_nx as f64;
        let center_y = (j as f64 + 0.5) / config.grid_ny as f64;
        let clip = if log_level.exp2() >= clip_threshold {
            1.0
        } else {
            0.0
        };
        let mut samples = Vec::new();
        if *valid {
            for _ in 0..8 {
                samples.push(format!("{log_level}"));
            }
        }
        let luminance = if *valid {
            format!("{log_level}")
        } else {
            "null".to_string()
        };
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"center\":[{center_x},{center_y}],\"log_luminance\":{luminance},\
\"valid\":{valid},\"saturation_fraction\":0.0,\"analysis_highlight_fraction\":{clip},\
\"structure_samples\":[{}],\"channel_log_means\":[{log_level},{log_level},{log_level}]}}",
            samples.join(",")
        ));
        let _ = spec;
    }
    out.push_str("]}\n");
    out
}

fn write_png(path: &Path, config: &Config, tiles: &[(f64, bool)]) -> Result<(), (i32, String)> {
    let width = config.width;
    let height = config.height;
    let linear: Vec<f64> = tiles.iter().map(|(value, _)| value.exp2()).collect();
    let mut raw = Vec::with_capacity(((width * 6 + 1) * height) as usize);
    for y in 0..height {
        raw.push(0u8);
        let j = (y as usize * config.grid_ny) / height as usize;
        for x in 0..width {
            let i = (x as usize * config.grid_nx) / width as usize;
            let value = linear[j * config.grid_nx + i];
            let code = quantize(value);
            for _ in 0..3 {
                raw.extend_from_slice(&code.to_be_bytes());
            }
        }
    }
    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[16, 2, 0, 0, 0]);
    write_chunk(&mut png, b"IHDR", &ihdr);
    write_chunk(&mut png, b"IDAT", &zlib_store(&raw));
    write_chunk(&mut png, b"IEND", &[]);
    fs::write(path, png).map_err(|err| (5, format!("cannot write PNG: {err}")))
}

fn quantize(value: f64) -> u16 {
    let code = (value * 16384.0).round();
    if !code.is_finite() || code <= 0.0 {
        0
    } else if code >= 65535.0 {
        65535
    } else {
        code as u16
    }
}

fn zlib_store(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut remaining = raw;
    loop {
        let take = remaining.len().min(65_535);
        let final_block = take == remaining.len();
        out.push(if final_block { 0x01 } else { 0x00 });
        let len = take as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(&remaining[..take]);
        remaining = &remaining[take..];
        if final_block {
            break;
        }
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for byte in data {
        a = (a + *byte as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn write_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let mut input = Vec::with_capacity(4 + data.len());
    input.extend_from_slice(kind);
    input.extend_from_slice(data);
    png.extend_from_slice(&crc32(&input).to_be_bytes());
}

fn read_file(path: &Path) -> Result<Vec<u8>, (i32, String)> {
    fs::read(path).map_err(|err| (5, format!("cannot read input: {err}")))
}

fn fnv1a(data: Vec<u8>) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn record(out: &mut impl Write, kind: u8, bytes: &[u8]) -> Result<(), (i32, String)> {
    out.write_all(&[kind, 0, 0, 0])
        .and_then(|_| out.write_all(&(bytes.len() as u64).to_le_bytes()))
        .and_then(|_| out.write_all(bytes))
        .map_err(|e| (50, e.to_string()))
}
