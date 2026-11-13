//! Encoder-node acceptance: default high-bit delivery, explicit resize, the
//! software alternative and early configuration rejection.
mod common;

use common::*;
use rawtl_linear_video::json::Json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn base_args<'a>(input: &'a Path, output: &'a Path, tool: &'a Path) -> Vec<&'a str> {
    vec![
        "--input",
        input.to_str().expect("utf8"),
        "--output",
        output.to_str().expect("utf8"),
        "--dng-linear-frame",
        tool.to_str().expect("utf8"),
        "--ffmpeg",
        FFMPEG,
        "--ffprobe",
        FFPROBE,
    ]
}

/// One synthetic sequence whose fixture is a horizontal neutral gradient.
fn gradient_material(
    name: &str,
    width: u32,
    height: u32,
    frames: usize,
) -> (PathBuf, PathBuf, PathBuf) {
    require_ffmpeg();
    let root = test_dir(name);
    let input = root.join("material");
    let names: Vec<String> = (0..frames).map(|index| format!("frame_{index:04}.DNG")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    make_input(&input, &refs);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    let pixels: Vec<[u16; 3]> = (0..height)
        .flat_map(|_| {
            (0..width).map(|x| {
                let code = ((u64::from(x) * 16384) / u64::from(width)).min(16384) as u16;
                [code, code, code]
            })
        })
        .collect();
    let mut fake_frames = Vec::new();
    for name in &names {
        let path = fixtures.join(format!("{}.png", name.replace('.', "_")));
        write_rgb48_png(&path, width, height, &pixels);
        let mut frame = FakeFrame::new(name, 0.0);
        frame.fixture = Some(path);
        fake_frames.push(frame);
    }
    let tool = fake_native_sequence(&fixtures, &fake_frames, None, false, width, height);
    (root, input, tool)
}

fn video_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read output")
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| ["mov", "mp4", "mkv"].iter().any(|ext| name.ends_with(ext)))
        .collect();
    names.sort();
    names
}

fn render_log_count(root: &Path) -> usize {
    fs::read_dir(root.join("logs/render"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("frame_")
                })
                .count()
        })
        .unwrap_or(0)
}

fn prepare_log_count(root: &Path) -> usize {
    fs::read_dir(root.join("logs/prepare"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("frame_")
                })
                .count()
        })
        .unwrap_or(0)
}

/// Decode the first frame to 10-bit 4:4:4 and return the distinct luma codes.
fn decoded_luma_codes(path: &Path) -> (Vec<u16>, u32, u32) {
    let output = Command::new(FFMPEG)
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv444p10le", "-"])
        .output()
        .expect("decode to 10-bit");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let probe = ffprobe_color(path);
    let width = probe
        .lines()
        .find_map(|line| line.strip_prefix("width="))
        .unwrap()
        .parse()
        .unwrap();
    let height = probe
        .lines()
        .find_map(|line| line.strip_prefix("height="))
        .unwrap()
        .parse()
        .unwrap();
    let luma: Vec<u16> = output.stdout[..(width as usize * height as usize) * 2]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    (luma, width, height)
}

#[test]
fn default_delivery_keeps_source_geometry_and_real_ten_bit_precision() {
    // The source is wider than the deleted 4096 preview cap: the graph must not
    // scale it, and the decoded handoff must carry more than 8-bit levels.
    let (root, input, tool) = gradient_material("encode-default", 5000, 40, 1);
    let output = root.join("run");
    let mut args = base_args(&input, &output, &tool);
    args.extend(["--strength", "0"]);
    let result = run_cli(&args);
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        stderr(&result)
    );
    assert_eq!(video_files(&output), vec!["output.mov".to_string()]);
    let probe = ffprobe_color(&output.join("output.mov"));
    for expected in [
        "codec_name=prores",
        "profile=4444",
        "pix_fmt=yuv444p12le",
        "width=5000",
        "height=40",
        "nb_read_frames=1",
        "avg_frame_rate=30/1",
        "color_space=bt709",
        "color_primaries=bt709",
        "color_transfer=iec61966-2-1",
        "color_range=unknown",
    ] {
        assert!(probe.contains(expected), "{probe}");
    }
    let (luma, width, height) = decoded_luma_codes(&output.join("output.mov"));
    assert_eq!((width, height), (5000, 40));
    let mut distinct: Vec<u16> = luma.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert!(
        distinct.len() > 300,
        "10-bit handoff collapsed to {} levels (an RGB8 repack can carry at most 256)",
        distinct.len()
    );
    assert_eq!(distinct[0], 64, "limited-range black code");
    assert_eq!(*distinct.last().unwrap(), 940, "limited-range white code");
    assert!(
        !output.join(".rawtl-partial/output").exists(),
        "the publish step must leave no additional video"
    );

    let manifest = read_json(&output.join("manifest.json"));
    let video = manifest.get("video").unwrap();
    assert_eq!(
        video.get("file").and_then(Json::as_str),
        Some("output.mov")
    );
    assert_eq!(
        video.get("encoder").and_then(Json::as_str),
        Some("prores_ks")
    );
    assert_eq!(video.get("container").and_then(Json::as_str), Some("mov"));
    assert_eq!(video.get("pix_fmt").and_then(Json::as_str), Some("yuv444p10le"));
    assert_eq!(video.get("range").and_then(Json::as_str), Some("tv"));
    let scale = video.get("scale").unwrap();
    assert_eq!(scale.get("applied").and_then(Json::as_bool), Some(false));
}

#[test]
fn explicit_resize_is_an_ordinary_scene_linear_stage_or_a_clear_error() {
    let (root, input, tool) = gradient_material("encode-resize", 64, 48, 1);
    let output = root.join("run");
    let mut args = base_args(&input, &output, &tool);
    args.extend(["--strength", "0", "--resize", "32x24"]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    let probe = ffprobe_color(&output.join("output.mov"));
    assert!(probe.contains("width=32") && probe.contains("height=24"), "{probe}");
    let manifest = read_json(&output.join("manifest.json"));
    let scale = manifest.get("video").unwrap().get("scale").unwrap();
    assert_eq!(scale.get("applied").and_then(Json::as_bool), Some(true));
    assert!(scale
        .get("algorithm")
        .and_then(Json::as_str)
        .unwrap()
        .contains("lanczos3"));
    assert_eq!(
        scale.get("source").and_then(Json::as_array).map(|values| {
            values.iter().filter_map(Json::as_u64).collect::<Vec<_>>()
        }),
        Some(vec![64, 48])
    );

    // No resize node: exactly the source geometry, never an implicit cap.
    let plain = root.join("plain");
    let mut args = base_args(&input, &plain, &tool);
    args.extend(["--strength", "0"]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    let probe = ffprobe_color(&plain.join("output.mov"));
    assert!(probe.contains("width=64") && probe.contains("height=48"), "{probe}");

    // 4:2:0 cannot carry odd geometry; reject before native preparation.
    let odd = root.join("odd");
    let mut args = base_args(&input, &odd, &tool);
    args.extend([
        "--strength",
        "0",
        "--encoder",
        "libx264",
        "--resize",
        "9x6",
    ]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(50), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(text.contains("even") && text.contains("9x6"), "{text}");
    assert!(!odd.join("logs/prepare").exists());
    assert!(!odd.join("output.mp4").exists());

    // 4:4:4 codecs accept the same odd geometry without rounding.
    let ffv1 = root.join("ffv1-odd");
    let mut args = base_args(&input, &ffv1, &tool);
    args.extend(["--strength", "0", "--encoder", "ffv1", "--resize", "9x7"]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    let probe = ffprobe_color(&ffv1.join("output.mkv"));
    assert!(probe.contains("codec_name=ffv1"), "{probe}");
    assert!(probe.contains("width=9") && probe.contains("height=7"), "{probe}");
}

#[test]
fn software_alternative_keeps_one_target_and_the_same_native_calls() {
    let (root, input, tool) = gradient_material("encode-alt", 64, 48, 3);
    let prores = root.join("prores");
    let mut args = base_args(&input, &prores, &tool);
    args.extend(["--strength", "0"]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(video_files(&prores), vec!["output.mov".to_string()]);

    let ffv1 = root.join("ffv1");
    let mut args = base_args(&input, &ffv1, &tool);
    args.extend(["--strength", "0", "--encoder", "ffv1"]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(video_files(&ffv1), vec!["output.mkv".to_string()]);
    let probe = ffprobe_color(&ffv1.join("output.mkv"));
    for expected in ["codec_name=ffv1", "pix_fmt=yuv444p10le", "nb_read_frames=3"] {
        assert!(probe.contains(expected), "{probe}");
    }
    assert_eq!(
        prepare_log_count(&prores),
        prepare_log_count(&ffv1),
        "converter calls must not change with the encoder"
    );
    assert_eq!(
        render_log_count(&prores),
        render_log_count(&ffv1),
        "develop calls must not change with the encoder"
    );

    let manifest = read_json(&ffv1.join("manifest.json"));
    let video = manifest.get("video").unwrap();
    assert_eq!(video.get("encoder").and_then(Json::as_str), Some("ffv1"));
    assert_eq!(
        video.get("container").and_then(Json::as_str),
        Some("matroska")
    );
    assert_eq!(video.get("range").and_then(Json::as_str), Some("tv"));
}

#[test]
fn illegal_encoder_configuration_is_rejected_before_raw_work() {
    let (root, input, tool) = gradient_material("encode-reject", 64, 48, 1);
    for (label, extra, code, marker) in [
        (
            "prores-mp4",
            vec!["--container", "mp4"],
            50,
            "cannot be muxed in MP4",
        ),
        (
            "prores-422",
            vec!["--pix-fmt", "yuv422p10le"],
            50,
            "requires a 4:4:4",
        ),
        (
            "ffv1-rate",
            vec!["--encoder", "ffv1", "--crf", "18"],
            50,
            "does not accept rate control",
        ),
        (
            "unknown-encoder",
            vec!["--encoder", "prores"],
            2,
            "expected one of",
        ),
        (
            "alpha",
            vec!["--pix-fmt", "yuva444p10le"],
            50,
            "carries alpha",
        ),
    ] {
        let output = root.join(label);
        let mut args = base_args(&input, &output, &tool);
        args.extend(["--strength", "0"]);
        args.extend(extra);
        let result = run_cli(&args);
        assert_eq!(result.status.code(), Some(code), "{label}: {}", stderr(&result));
        let text = stderr(&result);
        assert!(text.contains(marker), "{label}: {text}");
        assert!(
            !output.join("logs/prepare").exists(),
            "{label}: RAW preparation started despite rejection"
        );
        assert!(!output.join("output.mov").exists());
        assert!(!output.join("output.mp4").exists());
    }

    // An unsupported encoder option is rejected by the same layer.
    let output = root.join("bad-option");
    let mut args = base_args(&input, &output, &tool);
    args.extend(["--strength", "0", "--output-option=-vf=scale=2:2"]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(2), "{}", stderr(&result));
}

/// Reference black / 18% grey / 90% white bands through the real render,
/// ProRes encode and 10-bit decode path for every registered profile. Expected
/// values come from each transfer's published reference codes, not from the
/// production matrices.
#[test]
fn every_profile_encodes_black_grey_and_white_reference_codes() {
    use rawtl_linear_video::color_profile::{lookup, Transfer};

    // Reference reflection values: 0% black, 18% grey, 90% white.
    let reference_linear = [0.0f64, 0.18, 0.9];
    for id in [
        "srgb-preview",
        "sony-slog3-sgamut3",
        "sony-slog3-sgamut3-cine",
        "canon-log2-cinema-gamut",
        "canon-log2-cinema-gamut-rev-eng-crm-dev",
        "canon-log3-cinema-gamut-rev-eng-crm-dev",
        "nikon-nlog-bt2020",
        "dji-dlog-dgamut",
    ] {
        let profile = lookup(id).expect(id);
        let extension = if profile.camera_color_method().requires_canon_cr3() {
            "CR3"
        } else {
            "DNG"
        };
        let matrix = profile.scale_filter_token();
        // ProRes delivery is limited range for every profile; the reference
        // codes are signals, recovered by a video-range YCbCr decoder.
        let range = "tv";
        let expected_srgb = [(0.0f64, 0u8), (0.18, 118), (0.9, 243)];
        // Scene values that reach the published reflection references; the
        // rev-eng Canon profiles apply their scene -> reflection scale once.
        let scale = profile.scene_to_reflection_scale();
        let scene: Vec<f64> = reference_linear.iter().map(|value| value / scale).collect();
        let expected: Vec<[u8; 3]> = if profile.transfer == Transfer::Srgb {
            expected_srgb.iter().map(|(_, code)| [*code; 3]).collect()
        } else {
            reference_linear
                .iter()
                .map(|linear| {
                    let reference = profile
                        .transfer
                        .references()
                        .iter()
                        .find(|reference| (reference.linear - linear).abs() < 1e-9)
                        .unwrap_or_else(|| panic!("{id} has no reference at {linear}"));
                    [reference.code8() as u8; 3]
                })
                .collect()
        };
        let bands: Vec<u16> = scene
            .iter()
            .map(|value| (value * 16384.0).round() as u16)
            .collect();

        let root = test_dir(&format!("encode-bands-{id}"));
        let input = root.join("material");
        let name = format!("frame_0000.{extension}");
        make_input(&input, &[&name]);
        let fixtures = root.join("fixtures");
        fs::create_dir_all(&fixtures).expect("fixtures");
        let mut pixels = Vec::with_capacity(192 * 48);
        for _ in 0..48 {
            for x in 0..192 {
                let code = bands[x / 64];
                pixels.push([code, code, code]);
            }
        }
        let fixture = fixtures.join("bands.png");
        write_rgb48_png(&fixture, 192, 48, &pixels);
        let mut frame = FakeFrame::new(&name, 0.0);
        frame.fixture = Some(fixture);
        let tool = fake_native_sequence(&fixtures, &[frame], None, false, 192, 48);
        let output = root.join("run");
        let mut args = base_args(&input, &output, &tool);
        args.extend(["--strength", "0", "--preview-profile", id]);
        let result = run_cli(&args);
        assert!(
            result.status.success(),
            "{id}: {}",
            stderr(&result)
        );

        let decoded = Command::new(FFMPEG)
            .args(["-v", "error", "-nostdin", "-i"])
            .arg(output.join("output.mov"))
            .args([
                "-frames:v",
                "1",
                "-vf",
                &format!("scale=in_color_matrix={matrix}:in_range={range},format=rgb24"),
                "-f",
                "rawvideo",
                "-",
            ])
            .output()
            .expect("decode bands");
        assert!(
            decoded.status.success(),
            "{id}: {}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        for (band, expected) in expected.iter().enumerate() {
            let offset = (32 + band * 64) * 3;
            let observed = &decoded.stdout[offset..offset + 3];
            for channel in 0..3 {
                assert!(
                    (i32::from(observed[channel]) - i32::from(expected[channel])).abs() <= 3,
                    "{id} band {band} channel {channel}: {observed:?} vs {expected:?}"
                );
            }
        }
        println!("bands {id}: {expected:?} PASS");
    }
}

#[test]
fn software_h264_and_ffv1_configurations_produce_their_declared_formats() {
    let (root, input, tool) = gradient_material("encode-software", 64, 48, 1);

    let h264 = root.join("h264");
    let mut args = base_args(&input, &h264, &tool);
    args.extend(["--strength", "0", "--encoder", "libx264", "--crf", "20"]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(video_files(&h264), vec!["output.mp4".to_string()]);
    let probe = ffprobe_color(&h264.join("output.mp4"));
    for expected in [
        "codec_name=h264",
        "pix_fmt=yuv420p",
        "width=64",
        "height=48",
        "color_range=tv",
        "color_space=bt709",
    ] {
        assert!(probe.contains(expected), "{probe}");
    }
    let manifest = read_json(&h264.join("manifest.json"));
    let video = manifest.get("video").unwrap();
    assert_eq!(video.get("encoder").and_then(Json::as_str), Some("libx264"));
    assert_eq!(video.get("container").and_then(Json::as_str), Some("mp4"));
    assert_eq!(video.get("range_tag").and_then(Json::as_str), Some("written by the container"));

    // FFV1 keeps any configured lossless pixel format, including 4:2:0.
    let ffv1_420 = root.join("ffv1-420");
    let mut args = base_args(&input, &ffv1_420, &tool);
    args.extend([
        "--strength",
        "0",
        "--encoder",
        "ffv1",
        "--pix-fmt",
        "yuv420p",
    ]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    let probe = ffprobe_color(&ffv1_420.join("output.mkv"));
    for expected in ["codec_name=ffv1", "pix_fmt=yuv420p", "color_range=tv"] {
        assert!(probe.contains(expected), "{probe}");
    }
}
