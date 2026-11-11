//! Standalone fake `dng-linear-frame` used only by the pipeline integration
//! tests. It is compiled with plain `rustc` (no dependencies) by the test
//! harness and configured through `FAKE_NATIVE_CONFIG`; it never ships.
//!
//! Supported producer options: --white-balance, --converter,
//! --converter-timeout, --exposure-ev, --emit-analysis, --analysis-roi,
//! --no-png. The analysis sidecar follows the frozen S01 schema.

use std::env;
use std::fs;
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
    let name = options
        .input
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| (3, "input has no file name".to_string()))?
        .to_string();
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
            _ => (spec.own_neutral.clone(), "as-shot"),
        }
    };
    let source_sha = format!("fake-{:016x}", fnv1a(read_file(&options.input)?));
    let gain = options.exposure_ev.exp2();
    let tiles = tile_levels(&config, spec, options.exposure_ev, options.no_png);

    if !options.no_png {
        match (&spec.fixture, options.exposure_ev) {
            (Some(fixture), ev) if ev == 0.0 => {
                fs::copy(fixture, &options.png)
                    .map_err(|err| (5, format!("cannot copy fixture: {err}")))?;
            }
            _ => write_png(&options.png, &config, &tiles)?,
        }
    }

    let capture = match &spec.capture_time {
        Some(value) => json_string(value),
        None => "null".to_string(),
    };
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
        json_string(&options.input.display().to_string()),
        json_string(&source_sha),
        config.width,
        config.height,
        own = spec.own_neutral,
        ev = options.exposure_ev,
    );
    fs::write(&options.json, metadata)
        .map_err(|err| (5, format!("cannot write metadata: {err}")))?;

    if let Some(path) = &options.emit_analysis {
        let sidecar = sidecar_json(&config, spec, &tiles, &source_sha, &options, gain);
        fs::write(path, sidecar)
            .map_err(|err| (5, format!("cannot write analysis sidecar: {err}")))?;
    }
    Ok(())
}

fn parse_args() -> Result<Options, (i32, String)> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() < 3 {
        return Err((2, "expected <input> <png> <json>".to_string()));
    }
    let input = PathBuf::from(&args[0]);
    let png = PathBuf::from(&args[1]);
    let json = PathBuf::from(&args[2]);
    let mut white_balance = None;
    let mut exposure_ev = 0.0;
    let mut emit_analysis = None;
    let mut analysis_roi = [0.0, 0.0, 1.0, 1.0];
    let mut no_png = false;
    let mut index = 3;
    while index < args.len() {
        match args[index].as_str() {
            "--white-balance" => {
                white_balance = Some(value(&args, &mut index)?);
                index += 1;
            }
            "--converter" | "--converter-timeout" => {
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
    out.push_str("{\"schema_version\":1,\"kind\":\"rawtl-analysis-sidecar\",");
    out.push_str(&format!("\"source_sha256\":{},", json_string(source_sha)));
    // The accepted producer serializes sidecar numbers with 10 significant
    // digits; model that seam (the metadata JSON keeps full precision).
    let sidecar_ev = format!("{:.9e}", options.exposure_ev);
    out.push_str("\"analysis\":{");
    out.push_str(&format!(
        "\"max_edge\":{MAX_EDGE},\"downsampled_width\":{},\"downsampled_height\":{},\
\"grid_nx\":{},\"grid_ny\":{},\"roi\":[{},{},{},{}],\
\"roi_pixel_bounds\":[0,0,{},{}],\"tile_layout\":\"row-major (j*grid_nx+i)\",\
\"analysis_domain\":\"gained ACEScg AP1/D60 scene-linear FP32\",\
\"luma_weights\":[0.2126,0.7152,0.0722],\"quantum_linear\":0.00006103515625,\
\"floor\":0.00048828125,\"saturation_mask_quality\":\"camera_domain\",\
\"saturation_threshold\":0.98,\"output_clip_code_threshold\":16057,\
\"output_clip_quality\":\"delivered_domain\",\"structure_stride\":4,\
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
    let clip_threshold = 16057.0 / 16384.0;
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
\"valid\":{valid},\"saturation_fraction\":0.0,\"output_clip_fraction\":{clip},\
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
