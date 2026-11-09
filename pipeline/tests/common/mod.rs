//! Test support: tiny RGB16 PNG writer, fake native producers and command
//! helpers. Kept dependency-free to match the production crate.

#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

pub struct FakeFrame {
    pub name: String,
    pub fixture: PathBuf,
    pub own_neutral: String,
}

pub fn fixture(dir: &Path, name: &str, code: u16) -> PathBuf {
    let path = dir.join(format!("{name}.png"));
    let pixels = vec![[code, code, code]; (WIDTH * HEIGHT) as usize];
    write_rgb48_png(&path, WIDTH, HEIGHT, &pixels);
    path
}

/// Build a shell script that behaves like the accepted single-frame producer:
/// it copies the fixture keyed by input basename and writes a schema_version 1
/// sidecar. `fail_on` makes it exit 3 before writing outputs; `ignore_override`
/// simulates a producer that silently keeps its own neutral.
pub fn fake_native_tool(
    dir: &Path,
    frames: &[FakeFrame],
    fail_on: Option<&str>,
    ignore_override: bool,
) -> PathBuf {
    let path = dir.join("fake-dng-linear-frame");
    let mut script = String::new();
    script.push_str("#!/bin/sh\nset -eu\n");
    script.push_str("input=\"$1\"; png=\"$2\"; json=\"$3\"; shift 3\n");
    script.push_str("wb=\"\"\n");
    script.push_str("while [ \"$#\" -gt 0 ]; do\n");
    script.push_str("  case \"$1\" in\n");
    script.push_str("    --white-balance) wb=\"$2\"; shift 2 ;;\n");
    script.push_str("    --converter) shift 2 ;;\n");
    script.push_str("    --converter-timeout) shift 2 ;;\n");
    script.push_str("    *) echo \"unexpected argument: $1\" >&2; exit 2 ;;\n");
    script.push_str("  esac\n");
    script.push_str("done\n");
    script.push_str("name=$(basename \"$input\")\n");
    if let Some(failing) = fail_on {
        script.push_str(&format!(
            "if [ \"$name\" = {} ]; then echo \"synthetic frame failure\" >&2; exit 3; fi\n",
            shell_quote(failing)
        ));
    }
    script.push_str("case \"$name\" in\n");
    for frame in frames {
        script.push_str(&format!(
            "  {}) fixture={}; own={} ;;\n",
            shell_quote(&frame.name),
            shell_quote(&frame.fixture.display().to_string()),
            shell_quote(&frame.own_neutral)
        ));
    }
    script.push_str("  *) echo \"no fixture for $name\" >&2; exit 4 ;;\n");
    script.push_str("esac\n");
    script.push_str("cp \"$fixture\" \"$png\"\n");
    if ignore_override {
        script.push_str("applied=\"$own\"; wbsrc=as-shot\n");
    } else {
        script.push_str(
            "if [ -n \"$wb\" ]; then applied=\"$wb\"; wbsrc=override; else applied=\"$own\"; wbsrc=as-shot; fi\n",
        );
    }
    script.push_str("cat > \"$json\" <<EOF\n");
    script.push_str(&format!(
        "{{\"schema_version\":1,\"source\":\"$input\",\"source_sha256\":\"fixture\",\"width\":{WIDTH},\"height\":{HEIGHT},\"applied_white_balance\":[$applied],\"white_balance_source\":\"$wbsrc\",\"as_shot_neutral\":[$own],\"observed_scene_min\":0.0,\"observed_scene_max\":1.0,\"quantization\":{{\"scale\":16384,\"rounding\":\"nearest; ties upward\",\"negative_policy\":\"clip to 0\",\"upper_policy\":\"above 65535/16384 clips to 65535\",\"negative_samples\":0,\"above_upper_samples\":0,\"transfer\":\"linear; no OETF\"}}}}\n"
    ));
    script.push_str("EOF\n");
    script.push_str("exit 0\n");
    write_executable(&path, &script);
    path
}

pub fn fake_failing_encoder(dir: &Path) -> PathBuf {
    let path = dir.join("fake-ffmpeg-encoder-failure");
    let script = "#!/bin/sh\nfor last in \"$@\"; do :; done\nprintf 'partial video bytes' > \"$last\"\necho 'synthetic encoder failure after write' >&2\nexit 1\n";
    write_executable(&path, script);
    path
}

/// Delegates real encoding to ffmpeg, but feeds zeroed frames for the MKV
/// decode step so the lossless roundtrip comparison must fail.
pub fn fake_ffmpeg_zeroed_video_decode(dir: &Path, frame_bytes: usize, count: usize) -> PathBuf {
    let path = dir.join("fake-ffmpeg-roundtrip-mismatch");
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in\n  *'-f matroska'*) exec {} \"$@\" ;;\n  *linear.mkv*) head -c {} /dev/zero; exit 0 ;;\n  *) exec {} \"$@\" ;;\nesac\n",
        shell_quote(FFMPEG),
        frame_bytes * count,
        shell_quote(FFMPEG)
    );
    write_executable(&path, &script);
    path
}

pub fn fake_ffprobe_wrong_codec(dir: &Path) -> PathBuf {
    let path = dir.join("fake-ffprobe-wrong-codec");
    let script = format!(
        "#!/bin/sh\ncat <<EOF\ncodec_name=h264\npix_fmt=yuv420p\nwidth={WIDTH}\nheight={HEIGHT}\nnb_read_frames=2\navg_frame_rate=30/1\nEOF\nexit 0\n"
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

pub fn standard_frames(fixture_dir: &Path) -> Vec<FakeFrame> {
    vec![
        FakeFrame {
            name: "IMG_1.DNG".to_string(),
            fixture: fixture(fixture_dir, "IMG_1", 1000),
            own_neutral: "0.5,1,0.8".to_string(),
        },
        FakeFrame {
            name: "IMG_2.DNG".to_string(),
            fixture: fixture(fixture_dir, "IMG_2", 2000),
            own_neutral: "0.6,1,0.7".to_string(),
        },
        FakeFrame {
            name: "IMG_10.DNG".to_string(),
            fixture: fixture(fixture_dir, "IMG_10", 3000),
            own_neutral: "0.55,1,0.75".to_string(),
        },
    ]
}
