//! Encoder-node acceptance: default high-bit delivery, explicit resize, the
//! software alternative and early configuration rejection.
mod common;

use common::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn base_args<'a>(input: &'a Path, video: &'a Path, tool: &'a Path) -> Vec<&'a str> {
    vec![
        "--input",
        input.to_str().expect("utf8"),
        "--output",
        video.to_str().expect("utf8"),
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
    let names: Vec<String> = (0..frames)
        .map(|index| format!("frame_{index:04}.DNG"))
        .collect();
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

fn video_for(root: &Path, label: &str) -> PathBuf {
    root.join(label).join(VIDEO_NAME)
}

fn scratch_leftovers(directory: &Path) -> Vec<String> {
    fs::read_dir(directory)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with(".rawtl-"))
                .collect()
        })
        .unwrap_or_default()
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
    let video = video_for(&root, "run");
    let mut args = base_args(&input, &video, &tool);
    args.extend(["--strength", "0"]);
    let result = run_cli(&args);
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        stderr(&result)
    );
    assert_only_video_and_log(video.parent().unwrap(), video.file_name().unwrap().to_str().unwrap(), &[]);
    let probe = ffprobe_color(&video);
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
    ] {
        assert!(probe.contains(expected), "{probe}");
    }
    let log = read_log(video.parent().unwrap());
    let encoder = log_kind(&log, "encoder");
    assert_eq!(encoder.len(), 1);
    for expected in [
        "encoder=prores_ks",
        "container=mov",
        "pix_fmt=yuv444p10le",
        "range=tv",
        "geometry=source",
    ] {
        assert!(encoder[0].contains(expected), "{encoder:?}");
    }
    let (luma, width, height) = decoded_luma_codes(&video);
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
        scratch_leftovers(video.parent().unwrap()).is_empty(),
        "no scratch may remain after success"
    );
}

#[test]
fn explicit_resize_is_an_ordinary_scene_linear_stage_or_a_clear_error() {
    let (root, input, tool) = gradient_material("encode-resize", 64, 48, 1);
    let resized = video_for(&root, "run");
    let mut args = base_args(&input, &resized, &tool);
    args.extend(["--strength", "0", "--resize", "32x24"]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    let probe = ffprobe_color(&resized);
    assert!(
        probe.contains("width=32") && probe.contains("height=24"),
        "{probe}"
    );
    let log = read_log(resized.parent().unwrap());
    assert!(
        log_kind(&log, "encoder")[0].contains("geometry=32x24"),
        "{log}"
    );
    assert!(log_kind(&log, "video")[0].contains("resize=32x24"), "{log}");

    // No resize node: exactly the source geometry, never an implicit cap.
    let plain = video_for(&root, "plain");
    let mut args = base_args(&input, &plain, &tool);
    args.extend(["--strength", "0"]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    let probe = ffprobe_color(&plain);
    assert!(
        probe.contains("width=64") && probe.contains("height=48"),
        "{probe}"
    );

    // 4:2:0 cannot carry odd geometry; reject before native preparation.
    let odd = video_for(&root, "odd").with_file_name(VIDEO_MP4_NAME);
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
    assert!(!odd.exists());
    assert!(scratch_leftovers(odd.parent().unwrap()).is_empty());
    let log = read_log(odd.parent().unwrap());
    assert!(log.contains("kind=run.error code=50"), "{log}");

    // 4:4:4 codecs accept the same odd geometry without rounding.
    let ffv1 = video_for(&root, "ffv1-odd");
    let ffv1 = ffv1.with_file_name(VIDEO_MKV_NAME);
    let mut args = base_args(&input, &ffv1, &tool);
    args.extend(["--strength", "0", "--encoder", "ffv1", "--resize", "9x7"]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    let probe = ffprobe_color(&ffv1);
    assert!(probe.contains("codec_name=ffv1"), "{probe}");
    assert!(
        probe.contains("width=9") && probe.contains("height=7"),
        "{probe}"
    );
}

#[test]
fn software_alternative_keeps_one_target_and_the_same_native_calls() {
    let (root, input, tool) = gradient_material("encode-alt", 64, 48, 3);
    let prores = video_for(&root, "prores");
    let mut args = base_args(&input, &prores, &tool);
    args.extend(["--strength", "0"]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    assert_only_video_and_log(prores.parent().unwrap(), VIDEO_NAME, &[]);

    let ffv1 = video_for(&root, "ffv1").with_file_name(VIDEO_MKV_NAME);
    let mut args = base_args(&input, &ffv1, &tool);
    args.extend(["--strength", "0", "--encoder", "ffv1"]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    assert_only_video_and_log(ffv1.parent().unwrap(), VIDEO_MKV_NAME, &[]);
    let probe = ffprobe_color(&ffv1);
    for expected in ["codec_name=ffv1", "pix_fmt=yuv444p10le", "nb_read_frames=3"] {
        assert!(probe.contains(expected), "{probe}");
    }
    assert_eq!(
        log_kind(&read_log(prores.parent().unwrap()), "prepare").len(),
        log_kind(&read_log(ffv1.parent().unwrap()), "prepare").len(),
        "converter calls must not change with the encoder"
    );
    assert_eq!(
        log_kind(&read_log(prores.parent().unwrap()), "develop").len(),
        log_kind(&read_log(ffv1.parent().unwrap()), "develop").len(),
        "develop calls must not change with the encoder"
    );
    let ffv1_log = read_log(ffv1.parent().unwrap());
    let encoder = log_kind(&ffv1_log, "encoder");
    assert!(encoder[0].contains("encoder=ffv1"), "{encoder:?}");
    assert!(encoder[0].contains("container=matroska"), "{encoder:?}");
    assert!(encoder[0].contains("range=tv"), "{encoder:?}");
}

#[test]
fn illegal_encoder_configuration_is_rejected_before_raw_work() {
    let (root, input, tool) = gradient_material("encode-reject", 64, 48, 1);

    // ProRes in MP4 is refused by the resolved configuration, before RAW work.
    let prores_mp4 = video_for(&root, "prores-mp4").with_file_name(VIDEO_MP4_NAME);
    let mut args = base_args(&input, &prores_mp4, &tool);
    args.extend(["--strength", "0", "--container", "mp4"]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(50), "{}", stderr(&result));
    assert!(
        stderr(&result).contains("cannot be muxed in MP4"),
        "{}",
        stderr(&result)
    );
    assert!(!prores_mp4.parent().unwrap().exists());

    for (label, video_name, extra, code, marker) in [
        (
            "prores-422",
            VIDEO_NAME,
            vec!["--pix-fmt", "yuv422p10le"],
            50,
            "requires a 4:4:4",
        ),
        (
            "ffv1-rate",
            VIDEO_MKV_NAME,
            vec!["--encoder", "ffv1", "--crf", "18"],
            50,
            "does not accept rate control",
        ),
        (
            "unknown-encoder",
            VIDEO_NAME,
            vec!["--encoder", "prores"],
            2,
            "expected one of",
        ),
        (
            "alpha",
            VIDEO_NAME,
            vec!["--pix-fmt", "yuva444p10le"],
            50,
            "carries alpha",
        ),
    ] {
        let directory = root.join(label);
        let video = directory.join(video_name);
        let mut args = base_args(&input, &video, &tool);
        args.extend(["--strength", "0"]);
        args.extend(extra);
        let result = run_cli(&args);
        assert_eq!(
            result.status.code(),
            Some(code),
            "{label}: {}",
            stderr(&result)
        );
        assert!(
            stderr(&result).contains(marker),
            "{label}: {}",
            stderr(&result)
        );
        assert!(
            !directory.exists(),
            "{label}: a rejected configuration creates no run directory"
        );
    }

    // An unsupported encoder option is rejected by the same layer.
    let video = video_for(&root, "bad-option");
    let mut args = base_args(&input, &video, &tool);
    args.extend(["--strength", "0", "--output-option=-vf=scale=2:2"]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(2), "{}", stderr(&result));
}

/// Reference black / 18% grey / 90% white bands through the real render,
/// ProRes encode and 10-bit decode path for every registered profile.
#[test]
fn every_profile_encodes_black_grey_and_white_reference_codes() {
    use rawtl_linear_video::color_profile::{lookup, Transfer};

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
        let range = "tv";
        let expected_srgb = [(0.0f64, 0u8), (0.18, 118), (0.9, 243)];
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
        let video = video_for(&root, "run");
        let mut args = base_args(&input, &video, &tool);
        args.extend(["--strength", "0", "--preview-profile", id]);
        let result = run_cli(&args);
        assert!(result.status.success(), "{id}: {}", stderr(&result));

        let decoded = Command::new(FFMPEG)
            .args(["-v", "error", "-nostdin", "-i"])
            .arg(&video)
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
    }
}

#[test]
fn software_h264_and_ffv1_configurations_produce_their_declared_formats() {
    let (root, input, tool) = gradient_material("encode-software", 64, 48, 1);

    let h264 = video_for(&root, "h264").with_file_name(VIDEO_MP4_NAME);
    let mut args = base_args(&input, &h264, &tool);
    args.extend(["--strength", "0", "--encoder", "libx264", "--crf", "20"]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    assert_only_video_and_log(h264.parent().unwrap(), VIDEO_MP4_NAME, &[]);
    let probe = ffprobe_color(&h264);
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
    let log = read_log(h264.parent().unwrap());
    let encoder = log_kind(&log, "encoder");
    assert!(encoder[0].contains("encoder=libx264"), "{encoder:?}");
    assert!(encoder[0].contains("container=mp4"), "{encoder:?}");
    assert!(encoder[0].contains("range=tv"), "{encoder:?}");

    // FFV1 keeps any configured lossless pixel format, including 4:2:0.
    let ffv1 = video_for(&root, "ffv1-420").with_file_name(VIDEO_MKV_NAME);
    let mut args = base_args(&input, &ffv1, &tool);
    args.extend(["--strength", "0", "--encoder", "ffv1", "--pix-fmt", "yuv420p"]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    let probe = ffprobe_color(&ffv1);
    for expected in ["codec_name=ffv1", "pix_fmt=yuv420p", "color_range=tv"] {
        assert!(probe.contains(expected), "{probe}");
    }
}

fn graph_with_encoder_config(encoder: &str) -> String {
    include_str!("../src/graph/default.json").replace(
        "\"kind\":\"encode\",",
        &format!("\"kind\":\"encode\",\"encoder\":{encoder},"),
    )
}

#[test]
fn configured_h264_high_bit_format_is_accepted_end_to_end() {
    let (root, input, tool) = gradient_material("repair-h264-high444", 64, 48, 1);
    let video = video_for(&root, "run").with_file_name(VIDEO_MP4_NAME);
    let mut args = base_args(&input, &video, &tool);
    args.extend([
        "--strength",
        "0",
        "--encoder",
        "libx264",
        "--pix-fmt",
        "yuv444p10le",
        "--codec-profile",
        "high444",
    ]);
    let result = run_cli(&args);
    assert!(
        result.status.success(),
        "a valid configured H.264 format must not fail validation after encoding:\n{}",
        stderr(&result)
    );
    assert_only_video_and_log(video.parent().unwrap(), video.file_name().unwrap().to_str().unwrap(), &[]);
    let probe = ffprobe_color(&video);
    for expected in [
        "codec_name=h264",
        "pix_fmt=yuv444p10le",
        "profile=High 4:4:4 Predictive",
        "width=64",
        "height=48",
    ] {
        assert!(probe.contains(expected), "{probe}");
    }
    let log = read_log(video.parent().unwrap());
    let encoder = log_kind(&log, "encoder");
    assert!(encoder[0].contains("pix_fmt=yuv444p10le"), "{encoder:?}");
    let video_line = log_kind(&log, "video");
    assert!(video_line[0].contains("pix_fmt=yuv444p10le"), "{video_line:?}");
}

#[test]
fn unsupported_encoder_pixel_format_is_rejected_before_raw_work() {
    let (root, input, tool) = gradient_material("repair-pix-fmt", 64, 48, 1);
    let video = video_for(&root, "run");
    let mut args = base_args(&input, &video, &tool);
    args.extend(["--strength", "0", "--pix-fmt", "yuv444p12le"]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(50), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(
        text.contains("does not support pixel format") && text.contains("yuv444p12le"),
        "{text}"
    );
    assert!(!video.exists());

    // A supported ProRes format with a matching profile still succeeds.
    let supported = video_for(&root, "supported");
    let mut args = base_args(&input, &supported, &tool);
    args.extend([
        "--strength",
        "0",
        "--pix-fmt",
        "yuv422p10le",
        "--codec-profile",
        "hq",
    ]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    let probe = ffprobe_color(&supported);
    for expected in ["codec_name=prores", "profile=HQ"] {
        assert!(probe.contains(expected), "{probe}");
    }
    assert!(
        probe.contains("pix_fmt=yuv422p12le") || probe.contains("pix_fmt=yuv422p10le"),
        "{probe}"
    );
}

#[test]
fn libx264_fixed_quantizer_changes_the_encoded_output() {
    let (root, input, tool) = gradient_material("repair-qscale", 64, 48, 2);
    let mut encoded = Vec::new();
    let mut parameters = Vec::new();
    for qscale in ["1", "40"] {
        let video = video_for(&root, &format!("q{qscale}")).with_file_name(VIDEO_MP4_NAME);
        let mut args = base_args(&input, &video, &tool);
        args.extend([
            "--strength",
            "0",
            "--encoder",
            "libx264",
            "--qscale",
            qscale,
        ]);
        let result = run_cli(&args);
        assert!(result.status.success(), "qscale {qscale}: {}", stderr(&result));
        let log = read_log(video.parent().unwrap());
        let args = log_field(log_kind(&log, "encoder.command")[0], "args").unwrap();
        parameters.push(args.to_string());
        encoded.push(fs::read(&video).expect("encoded file"));
    }
    assert_ne!(
        encoded[0], encoded[1],
        "distinct fixed-quantizer settings must produce distinct output"
    );
    for (qscale, parameters) in ["1", "40"].iter().zip(&parameters) {
        assert!(
            parameters.contains(" -qp "),
            "qscale {qscale}: fixed quantizer must use libx264's -qp control: {parameters}"
        );
        assert!(
            !parameters.contains(" -q:v "),
            "qscale {qscale}: libx264 ignores -q:v: {parameters}"
        );
    }
}

#[test]
fn naming_the_effective_encoder_keeps_graph_profile_and_pixel_format() {
    let (root, input, tool) = gradient_material("repair-effective-encoder", 64, 48, 1);

    // Reported shape 1: graph sets only the ProRes profile; naming the default
    // encoder on the CLI must not silently reset it to profile 4.
    let config = root.join("profile5.json");
    fs::write(&config, graph_with_encoder_config(r#"{"profile":"5"}"#)).unwrap();
    let output = root.join("profile5");
    let video = output.join(VIDEO_NAME);
    let mut args = base_args(&input, &video, &tool);
    args.extend([
        "--strength",
        "0",
        "--encoder",
        "prores_ks",
        "--graph",
        config.to_str().unwrap(),
    ]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    let log = read_log(&output);
    assert!(log_kind(&log, "encoder")[0].contains("profile=5"), "{log}");
    let probe = ffprobe_color(&video);
    assert!(probe.contains("profile=XQ"), "{probe}");

    // Reported shape 2: an omitted graph encoder with HQ/422 must stay valid
    // when the CLI names the already-effective default encoder.
    let config = root.join("hq422.json");
    fs::write(
        &config,
        graph_with_encoder_config(r#"{"profile":"hq","pix_fmt":"yuv422p10le"}"#),
    )
    .unwrap();
    let output = root.join("hq422");
    let video = output.join(VIDEO_NAME);
    let mut args = base_args(&input, &video, &tool);
    args.extend([
        "--strength",
        "0",
        "--encoder",
        "prores_ks",
        "--graph",
        config.to_str().unwrap(),
    ]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    let probe = ffprobe_color(&video);
    assert!(probe.contains("profile=HQ"), "{probe}");
    assert!(
        probe.contains("pix_fmt=yuv422p12le") || probe.contains("pix_fmt=yuv422p10le"),
        "{probe}"
    );
}
