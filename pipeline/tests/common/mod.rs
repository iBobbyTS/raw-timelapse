//! Test support: tiny RGB16 PNG writer, fake native producers and command
//! helpers. Kept dependency-free to match the production crate.
//!
//! The fake producer is a small standalone Rust program
//! (`tests/common/fake_native_main.rs`) compiled once with `rustc`. Every test
//! gets a wrapper script that points `FAKE_NATIVE_CONFIG` at its own sequence
//! description, so tests can run in parallel.

#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

pub const FFMPEG: &str = "/opt/homebrew/opt/ffmpeg-full/bin/ffmpeg";
pub const FFPROBE: &str = "/opt/homebrew/opt/ffmpeg-full/bin/ffprobe";
pub const WIDTH: u32 = 8;
pub const HEIGHT: u32 = 6;

pub fn test_dir(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-tmp")
        .join(format!("{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("create test directory");
    root
}

pub fn require_ffmpeg() {
    assert!(
        Path::new(FFMPEG).is_file(),
        "ffmpeg is required for integration tests at {FFMPEG}"
    );
    assert!(
        Path::new(FFPROBE).is_file(),
        "ffprobe is required for integration tests at {FFPROBE}"
    );
}

pub fn run_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rawtl-linear-video"))
        .args(args)
        .output()
        .expect("run rawtl-linear-video")
}

pub fn ffprobe(path: &Path) -> String {
    let output = Command::new(FFPROBE)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=codec_name,pix_fmt,width,height,nb_read_frames,avg_frame_rate",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(
        output.status.success(),
        "ffprobe failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// ffprobe stream fields including the colour tags used by the profile tests.
pub fn ffprobe_color(path: &Path) -> String {
    let output = Command::new(FFPROBE)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=codec_name,pix_fmt,width,height,nb_read_frames,avg_frame_rate,\
             color_space,color_primaries,color_transfer,color_range",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(
        output.status.success(),
        "ffprobe failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Decode the first pixel of a preview through the real H.264 decoder using an
/// explicit input matrix/range, returning the RGB24 value. The call rejects
/// decoder failure instead of silently returning a default.
pub fn decode_first_pixel(path: &Path, matrix: &str, range: &str) -> [u8; 3] {
    let filter = format!("scale=in_color_matrix={matrix}:in_range={range},format=rgb24");
    let output = Command::new(FFMPEG)
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-vf"])
        .arg(&filter)
        .args(["-f", "rawvideo", "-"])
        .output()
        .expect("run ffmpeg decode");
    assert!(
        output.status.success(),
        "ffmpeg decode failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() >= 3, "no decoded pixel");
    [output.stdout[0], output.stdout[1], output.stdout[2]]
}

/// Decode with only an RGB output format: no input matrix/range/colour overrides.
pub fn decode_first_pixel_auto(path: &Path) -> [u8; 3] {
    let output = Command::new(FFMPEG)
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-pix_fmt", "rgb24", "-f", "rawvideo", "-"])
        .output()
        .expect("auto decode");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout[..3].try_into().expect("decoded RGB pixel")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorCodes {
    pub primaries: u16,
    pub transfer: u16,
    pub matrix: u16,
    pub full: u16,
}

/// Walk ISO BMFF boxes by declared sizes, never by searching for a byte pattern
/// that could occur in encoded media. Only the video sample entry is inspected.
pub fn mp4_nclx(path: &Path) -> ColorCodes {
    fn boxes(mut data: &[u8]) -> Vec<(&[u8], &[u8])> {
        let mut out = Vec::new();
        while !data.is_empty() {
            assert!(data.len() >= 8, "truncated box header");
            let size = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
            let (size, header) = match size {
                0 => (data.len(), 8),
                1 => {
                    assert!(data.len() >= 16);
                    (
                        usize::try_from(u64::from_be_bytes(data[8..16].try_into().unwrap()))
                            .unwrap(),
                        16,
                    )
                }
                _ => (size, 8),
            };
            assert!(size >= header && size <= data.len(), "invalid box size");
            out.push((&data[4..8], &data[header..size]));
            data = &data[size..];
        }
        out
    }
    fn child<'a>(data: &'a [u8], kind: &[u8]) -> &'a [u8] {
        boxes(data)
            .into_iter()
            .find(|(name, _)| *name == kind)
            .expect("required MP4 box")
            .1
    }
    let bytes = fs::read(path).expect("read MP4");
    let moov = child(&bytes, b"moov");
    let mut colours = Vec::new();
    for (_, trak) in boxes(moov).into_iter().filter(|(kind, _)| *kind == b"trak") {
        let mdia = child(trak, b"mdia");
        let minf = child(mdia, b"minf");
        let stbl = child(minf, b"stbl");
        let stsd = child(stbl, b"stsd");
        assert!(stsd.len() >= 8);
        for (kind, sample) in boxes(&stsd[8..]) {
            if kind != b"avc1" && kind != b"avc3" {
                continue;
            }
            assert!(sample.len() >= 78, "short VisualSampleEntry");
            let colr = child(&sample[78..], b"colr");
            assert_eq!(&colr[..4], b"nclx", "expected nclx (not nclc or ICC)");
            assert_eq!(colr.len(), 11);
            let value = |at| u16::from_be_bytes(colr[at..at + 2].try_into().unwrap());
            colours.push(ColorCodes {
                primaries: value(4),
                transfer: value(6),
                matrix: value(8),
                full: (colr[10] >> 7) as u16,
            });
        }
    }
    assert_eq!(colours.len(), 1, "one video nclx required");
    colours[0]
}

/// Inspect elementary-stream SPS VUI, with no MP4 metadata available to merge.
/// trace_headers exposes numeric syntax elements; every repeated SPS must agree.
pub fn h264_sps_vui(path: &Path) -> ColorCodes {
    let es = path.with_extension("h264");
    let extract = Command::new(FFMPEG)
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-c:v", "copy", "-f", "h264"])
        .arg(&es)
        .output()
        .expect("extract H.264");
    assert!(
        extract.status.success(),
        "{}",
        String::from_utf8_lossy(&extract.stderr)
    );
    let trace = Command::new(FFMPEG)
        .args(["-v", "verbose", "-nostdin", "-i"])
        .arg(&es)
        .args(["-c:v", "copy", "-bsf:v", "trace_headers", "-f", "null", "-"])
        .output()
        .expect("trace SPS");
    assert!(
        trace.status.success(),
        "{}",
        String::from_utf8_lossy(&trace.stderr)
    );
    fs::write(path.with_extension("sps-trace.log"), &trace.stderr).expect("save SPS trace");
    let text = String::from_utf8(trace.stderr).expect("SPS text");
    let field = |key: &str| {
        let values: Vec<u16> = text
            .lines()
            .filter(|line| line.split_whitespace().any(|token| token == key))
            .map(|line| {
                line.rsplit_once(" = ")
                    .expect("syntax value")
                    .1
                    .trim()
                    .parse()
                    .expect("numeric syntax value")
            })
            .collect();
        assert!(!values.is_empty(), "missing SPS VUI {key}");
        assert!(
            values.iter().all(|value| *value == values[0]),
            "inconsistent SPS {key}"
        );
        values[0]
    };
    ColorCodes {
        primaries: field("colour_primaries"),
        transfer: field("transfer_characteristics"),
        matrix: field("matrix_coefficients"),
        full: field("video_full_range_flag"),
    }
}

// ---------------------------------------------------------------------------
// PNG writer (16-bit RGB, filter 0, zlib stored blocks)
// ---------------------------------------------------------------------------

pub fn write_rgb48_png(path: &Path, width: u32, height: u32, pixels: &[[u16; 3]]) {
    assert_eq!(pixels.len(), (width * height) as usize);
    let mut raw = Vec::with_capacity(((width * 6 + 1) * height) as usize);
    for row in 0..height as usize {
        raw.push(0u8); // filter: none
        for column in 0..width as usize {
            let pixel = pixels[row * width as usize + column];
            for channel in pixel {
                raw.extend_from_slice(&channel.to_be_bytes());
            }
        }
    }

    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[16, 2, 0, 0, 0]); // 16-bit RGB, no interlace
    write_chunk(&mut png, b"IHDR", &ihdr);
    write_chunk(&mut png, b"IDAT", &zlib_store(&raw));
    write_chunk(&mut png, b"IEND", &[]);

    fs::write(path, png).expect("write PNG fixture");
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
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    png.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

// ---------------------------------------------------------------------------
// Fake native producer
// ---------------------------------------------------------------------------

/// One frame description for the fake producer.
#[derive(Debug, Clone)]
pub struct FakeFrame {
    pub name: String,
    /// When set, the producer copies these PNG bytes at EV 0 (legacy fixtures).
    pub fixture: Option<PathBuf>,
    pub own_neutral: String,
    /// log2 of the scene-linear background level (before any EV gain).
    pub log_level: f64,
    /// Fraction of the width occupied by a 2x (bright) moving object.
    pub object_fraction: f64,
    pub capture_time: Option<String>,
    /// `flat` or `alternating` (+/-0.5 EV tile pattern, for isolation tests).
    pub pattern: &'static str,
}

impl FakeFrame {
    pub fn new(name: &str, log_level: f64) -> FakeFrame {
        FakeFrame {
            name: name.to_string(),
            fixture: None,
            own_neutral: "0.5,1,0.8".to_string(),
            log_level,
            object_fraction: 0.0,
            capture_time: None,
            pattern: "flat",
        }
    }

    pub fn capture(mut self, time: &str) -> FakeFrame {
        self.capture_time = Some(time.to_string());
        self
    }

    pub fn object(mut self, fraction: f64) -> FakeFrame {
        self.object_fraction = fraction;
        self
    }

    pub fn pattern(mut self, pattern: &'static str) -> FakeFrame {
        self.pattern = pattern;
        self
    }

    pub fn neutral(mut self, neutral: &str) -> FakeFrame {
        self.own_neutral = neutral.to_string();
        self
    }
}

fn fake_native_binary() -> PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY
        .get_or_init(|| {
            let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("fake-native-bin");
            fs::create_dir_all(&directory).expect("create fake native dir");
            let binary = directory.join("fake-native");
            let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("common")
                .join("fake_native_main.rs");
            let output = Command::new("rustc")
                .args(["--edition", "2021", "-O"])
                .arg(&source)
                .arg("-o")
                .arg(&binary)
                .output()
                .expect("run rustc for the fake native producer");
            assert!(
                output.status.success(),
                "fake native compilation failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            binary
        })
        .clone()
}

/// White-balance reporting defects the fake producer can simulate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeWbLie {
    None,
    /// Report `override` with a fixed neutral although no --white-balance was
    /// passed (a producer that invents its own override).
    OverrideWhenUnset,
    /// Report `as-shot` while applying a neutral different from the frame's own
    /// as_shot_neutral.
    AsShotMismatch,
}

/// Sidecar defects the fake producer can simulate to exercise the typed
/// stage-handoff validation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FakeSidecarDefects {
    /// Emit an empty `source_sha256` in the frame metadata sidecar.
    pub empty_source_sha256: bool,
    /// Omit `kind` from the analysis sidecar.
    pub analysis_omit_kind: bool,
    /// Report an analysis source hash that differs from the frame metadata.
    pub analysis_sha_mismatch: bool,
}

fn write_fake_native(
    dir: &Path,
    frames: &[FakeFrame],
    fail_on: Option<&str>,
    ignore_override: bool,
    lie: FakeWbLie,
    defects: FakeSidecarDefects,
    width: u32,
    height: u32,
    gain_scale: f64,
) -> PathBuf {
    let (grid_nx, grid_ny) = if width >= 192 && height >= 128 {
        (12, 8)
    } else {
        (4, 2)
    };
    let mut config = String::new();
    config.push_str(&format!("width={width}\nheight={height}\n"));
    config.push_str(&format!("grid_nx={grid_nx}\ngrid_ny={grid_ny}\n"));
    config.push_str(&format!(
        "ignore_override={}\n",
        if ignore_override { 1 } else { 0 }
    ));
    if lie == FakeWbLie::OverrideWhenUnset {
        config.push_str("override_when_unset=1\n");
    }
    if lie == FakeWbLie::AsShotMismatch {
        config.push_str("as_shot_mismatch=1\n");
    }
    if let Some(name) = fail_on {
        config.push_str(&format!("fail_on={name}\n"));
    }
    if (gain_scale - 1.0).abs() > f64::EPSILON {
        config.push_str(&format!("gain_scale={gain_scale}\n"));
    }
    if defects.empty_source_sha256 {
        config.push_str("empty_source_sha256=1\n");
    }
    if defects.analysis_omit_kind {
        config.push_str("analysis_omit_kind=1\n");
    }
    if defects.analysis_sha_mismatch {
        config.push_str("analysis_sha_mismatch=1\n");
    }
    for frame in frames {
        let fixture = frame
            .fixture
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "-".to_string());
        let capture = frame
            .capture_time
            .clone()
            .unwrap_or_else(|| "-".to_string());
        config.push_str(&format!(
            "frame={}|{}|{}|{}|{}|{}|{}\n",
            frame.name,
            frame.own_neutral,
            frame.log_level,
            frame.object_fraction,
            capture,
            fixture,
            frame.pattern
        ));
    }
    let config_path = dir.join(format!("fake-native-{}.config", std::process::id()));
    fs::write(&config_path, config).expect("write fake native config");

    let wrapper = dir.join("fake-dng-linear-frame");
    let script = format!(
        "#!/bin/sh\nexport FAKE_NATIVE_CONFIG={}\nexec {} \"$@\"\n",
        shell_quote(&config_path.display().to_string()),
        shell_quote(&fake_native_binary().display().to_string())
    );
    write_executable(&wrapper, &script);
    wrapper
}

/// Historical 8x6 fixture sequence used by the pre-deflicker tests.
pub fn fake_native_tool(
    dir: &Path,
    frames: &[FakeFrame],
    fail_on: Option<&str>,
    ignore_override: bool,
) -> PathBuf {
    write_fake_native(
        dir,
        frames,
        fail_on,
        ignore_override,
        FakeWbLie::None,
        FakeSidecarDefects::default(),
        WIDTH,
        HEIGHT,
        1.0,
    )
}

/// Synthetic sequence with an explicit geometry (deflicker tests).
pub fn fake_native_sequence(
    dir: &Path,
    frames: &[FakeFrame],
    fail_on: Option<&str>,
    ignore_override: bool,
    width: u32,
    height: u32,
) -> PathBuf {
    write_fake_native(
        dir,
        frames,
        fail_on,
        ignore_override,
        FakeWbLie::None,
        FakeSidecarDefects::default(),
        width,
        height,
        1.0,
    )
}

/// Same sequence, but the producer reports white balance dishonestly.
pub fn fake_native_sequence_wb_lie(
    dir: &Path,
    frames: &[FakeFrame],
    lie: FakeWbLie,
    width: u32,
    height: u32,
) -> PathBuf {
    write_fake_native(
        dir,
        frames,
        None,
        false,
        lie,
        FakeSidecarDefects::default(),
        width,
        height,
        1.0,
    )
}

/// Same sequence, but the producer executes only `scale` of the requested EV
/// (models a renderer that under-applies gain).
pub fn fake_native_sequence_gain_scale(
    dir: &Path,
    frames: &[FakeFrame],
    fail_on: Option<&str>,
    ignore_override: bool,
    width: u32,
    height: u32,
    scale: f64,
) -> PathBuf {
    write_fake_native(
        dir,
        frames,
        fail_on,
        ignore_override,
        FakeWbLie::None,
        FakeSidecarDefects::default(),
        width,
        height,
        scale,
    )
}

/// Same sequence, but sidecar fields are forged to exercise the typed
/// stage-handoff rejection paths.
pub fn fake_native_sequence_sidecar_defect(
    dir: &Path,
    frames: &[FakeFrame],
    defects: FakeSidecarDefects,
    width: u32,
    height: u32,
) -> PathBuf {
    write_fake_native(
        dir,
        frames,
        None,
        false,
        FakeWbLie::None,
        defects,
        width,
        height,
        1.0,
    )
}

pub fn fake_failing_encoder(dir: &Path) -> PathBuf {
    let path = dir.join("fake-ffmpeg-encoder-failure");
    let script = "#!/bin/sh\nfor last in \"$@\"; do :; done\nprintf 'partial video bytes' > \"$last\"\necho 'synthetic encoder failure after write' >&2\nexit 1\n";
    write_executable(&path, script);
    path
}

pub fn fake_ffprobe_wrong_codec(dir: &Path) -> PathBuf {
    let path = dir.join("fake-ffprobe-wrong-codec");
    let script = format!(
        "#!/bin/sh\ncat <<EOF\ncodec_name=ffv1\npix_fmt=yuv420p\nwidth={WIDTH}\nheight={HEIGHT}\nnb_read_frames=2\navg_frame_rate=30/1\nEOF\nexit 0\n"
    );
    write_executable(&path, &script);
    path
}

pub fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write script");
    let mut permissions = fs::metadata(path).expect("stat script").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod script");
}

pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

// ---------------------------------------------------------------------------
// Input material helpers
// ---------------------------------------------------------------------------

/// Create placeholder DNG files (content is irrelevant to the fake producer).
pub fn make_input(dir: &Path, names: &[&str]) {
    fs::create_dir_all(dir).expect("create input dir");
    for name in names {
        fs::write(dir.join(name), b"placeholder DNG").expect("write input frame");
    }
}

pub fn fixture(dir: &Path, name: &str, code: u16) -> PathBuf {
    let path = dir.join(format!("{name}.png"));
    let pixels = vec![[code, code, code]; (WIDTH * HEIGHT) as usize];
    write_rgb48_png(&path, WIDTH, HEIGHT, &pixels);
    path
}

pub fn standard_frames(fixture_dir: &Path) -> Vec<FakeFrame> {
    vec![
        FakeFrame {
            name: "IMG_1.DNG".to_string(),
            fixture: Some(fixture(fixture_dir, "IMG_1", 1000)),
            own_neutral: "0.5,1,0.8".to_string(),
            log_level: (1000.0f64 / 16384.0).log2(),
            object_fraction: 0.0,
            capture_time: Some("2026:11:08 20:00:00".to_string()),
            pattern: "flat",
        },
        FakeFrame {
            name: "IMG_2.DNG".to_string(),
            fixture: Some(fixture(fixture_dir, "IMG_2", 2000)),
            own_neutral: "0.6,1,0.7".to_string(),
            log_level: (2000.0f64 / 16384.0).log2(),
            object_fraction: 0.0,
            capture_time: Some("2026:11:08 20:00:10".to_string()),
            pattern: "flat",
        },
        FakeFrame {
            name: "IMG_10.DNG".to_string(),
            fixture: Some(fixture(fixture_dir, "IMG_10", 3000)),
            own_neutral: "0.55,1,0.75".to_string(),
            log_level: (3000.0f64 / 16384.0).log2(),
            object_fraction: 0.0,
            capture_time: Some("2026:11:08 20:00:20".to_string()),
            pattern: "flat",
        },
    ]
}

/// Deterministic synthetic sequence: base 0.18 scene-linear plus injected EV.
pub fn synthetic_sequence(gains: &[f64]) -> Vec<FakeFrame> {
    let base = 0.18f64.log2();
    gains
        .iter()
        .enumerate()
        .map(|(index, gain)| {
            FakeFrame::new(&format!("frame_{:04}.DNG", index), base + gain).capture(&format!(
                "2026:11:08 20:{:02}:{:02}",
                index / 60,
                index % 60
            ))
        })
        .collect()
}

pub fn frame_names(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("frame_{:04}.DNG", index))
        .collect()
}

// ---------------------------------------------------------------------------
// Assertion helpers shared by the deflicker integration tests
// ---------------------------------------------------------------------------

pub fn read_json(path: &Path) -> rawtl_linear_video::json::Json {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
    rawtl_linear_video::json::parse(&text)
        .unwrap_or_else(|err| panic!("invalid JSON {}: {err}", path.display()))
}

pub fn frame_record(
    report: &rawtl_linear_video::json::Json,
    index: usize,
) -> rawtl_linear_video::json::Json {
    report
        .get("frames")
        .and_then(|frames| frames.as_array())
        .and_then(|frames| frames.get(index))
        .cloned()
        .unwrap_or_else(|| panic!("report has no frame {index}"))
}
