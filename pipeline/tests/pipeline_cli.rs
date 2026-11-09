mod common;

use common::*;
use std::fs;
use std::path::Path;
use std::process::Output;

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn run_success_setup(name: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    require_ffmpeg();
    let root = test_dir(name);
    let input = root.join("material");
    make_input(&input, &["IMG_10.DNG", "IMG_1.DNG", "IMG_2.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    (root, input, fixtures)
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

#[test]
fn publishes_lossless_linear_video_and_srgb_preview() {
    let (root, input, fixtures) = run_success_setup("success");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let output = root.join("run");

    let result = run_cli(&base_args(&input, &output, &tool));
    assert!(
        result.status.success(),
        "run failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&result),
        stderr(&result)
    );

    // Numeric ordering: IMG_1, IMG_2, IMG_10 (not lexicographic IMG_1, IMG_10, IMG_2).
    for (index, fixture_name) in [(1, "IMG_1"), (2, "IMG_2"), (3, "IMG_10")] {
        let produced =
            fs::read(output.join(format!("frames/frame_{index:06}.png"))).expect("produced frame");
        let expected =
            fs::read(fixtures.join(format!("{fixture_name}.png"))).expect("fixture frame");
        assert_eq!(
            produced, expected,
            "frame {index} must be produced from {fixture_name}"
        );
    }

    // Sequence-frozen white balance: first frame as-shot, later frames override.
    let first = fs::read_to_string(output.join("frames/frame_000001.json")).expect("json");
    assert!(
        first.contains("\"white_balance_source\":\"as-shot\""),
        "{first}"
    );
    assert!(
        first.contains("\"applied_white_balance\":[0.5,1,0.8]"),
        "{first}"
    );
    for index in [2, 3] {
        let text =
            fs::read_to_string(output.join(format!("frames/frame_{index:06}.json"))).expect("json");
        assert!(
            text.contains("\"white_balance_source\":\"override\""),
            "frame {index}: {text}"
        );
        assert!(
            text.contains("\"applied_white_balance\":[0.5,1,0.8]"),
            "frame {index}: {text}"
        );
        assert!(
            !text.contains("\"applied_white_balance\":[0.6,1,0.7]"),
            "frame {index}: {text}"
        );
        assert!(
            text.contains("\"as_shot_neutral\":["),
            "frame {index}: {text}"
        );
    }

    let linear = output.join("linear-acescg-16bit-ffv1.mkv");
    let preview = output.join("srgb-preview-h264.mp4");
    let partial = output.join(".rawtl-partial");
    assert!(linear.is_file(), "linear video must exist");
    assert!(preview.is_file(), "preview must exist");
    assert!(!partial.exists(), "partial directory must be cleaned up");

    let manifest = fs::read_to_string(output.join("manifest.json")).expect("manifest");
    for expected in [
        "\"all_applied_equal\": true",
        "\"value\": \"30/1\"",
        "legacy/python/src/rawtl/cli.py:91",
        "\"codec\": \"ffv1\"",
        "\"pix_fmt\": \"gbrp16le\"",
        "\"status\": \"validated\"",
        "\"full_decode\": \"pass\"",
    ] {
        assert!(manifest.contains(expected), "manifest missing {expected:?}");
    }

    let linear_probe = ffprobe(&linear);
    for expected in [
        "codec_name=ffv1",
        "pix_fmt=gbrp16le",
        "width=8",
        "height=6",
        "nb_read_frames=3",
    ] {
        assert!(linear_probe.contains(expected), "{linear_probe}");
    }
    let preview_probe = ffprobe(&preview);
    for expected in ["codec_name=h264", "pix_fmt=yuv420p", "nb_read_frames=3"] {
        assert!(preview_probe.contains(expected), "{preview_probe}");
    }

    let roundtrip = fs::read_to_string(output.join("logs/roundtrip.log")).expect("log");
    assert!(roundtrip.contains("result: PASS"), "{roundtrip}");
}

#[test]
fn stops_at_the_first_failing_frame_without_video() {
    let root = test_dir("fail-fast");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, Some("IMG_2.DNG"), false);
    let output = root.join("run");

    let result = run_cli(&base_args(&input, &output, &tool));
    assert_eq!(result.status.code(), Some(30), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(text.contains("frame 2/3"), "{text}");
    assert!(text.contains("IMG_2.DNG"), "{text}");
    assert!(text.contains("synthetic frame failure"), "{text}");
    assert!(text.contains("no video was produced"), "{text}");

    assert!(output.join("frames/frame_000001.png").is_file());
    assert!(!output.join("frames/frame_000002.png").exists());
    assert!(!output.join("linear-acescg-16bit-ffv1.mkv").exists());
    assert!(!output.join("srgb-preview-h264.mp4").exists());
    assert!(!output.join(".rawtl-partial").exists());
    let log = fs::read_to_string(output.join("logs/frame_000002.log")).expect("failure log");
    assert!(log.contains("synthetic frame failure"), "{log}");
}

#[test]
fn encoder_failure_after_bytes_written_publishes_nothing() {
    let root = test_dir("encoder-failure");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let encoder = fake_failing_encoder(&fixtures);
    let output = root.join("run");

    let mut args = base_args(&input, &output, &tool);
    args.extend_from_slice(&["--ffmpeg", encoder.to_str().expect("utf8")]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(40), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(text.contains("FFV1 encoder failed"), "{text}");
    assert!(
        text.contains("partial/intermediate artifacts retained"),
        "{text}"
    );

    assert!(!output.join("linear-acescg-16bit-ffv1.mkv").exists());
    assert!(!output.join("srgb-preview-h264.mp4").exists());
    let partial = output.join(".rawtl-partial/linear.mkv");
    assert!(
        partial.is_file(),
        "partial diagnostic file must be retained"
    );
    assert_eq!(
        fs::read(&partial).expect("read partial"),
        b"partial video bytes"
    );
}

#[test]
fn probe_rejection_publishes_nothing() {
    let root = test_dir("probe-rejection");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let ffprobe = fake_ffprobe_wrong_codec(&fixtures);
    let output = root.join("run");

    let mut args = base_args(&input, &output, &tool);
    args.extend_from_slice(&["--ffprobe", ffprobe.to_str().expect("utf8")]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(50), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(text.contains("codec is \"h264\""), "{text}");
    assert!(text.contains("expected \"ffv1\""), "{text}");
    assert!(
        text.contains("partial/intermediate artifacts retained"),
        "{text}"
    );
    assert!(!output.join("linear-acescg-16bit-ffv1.mkv").exists());
    assert!(!output.join("srgb-preview-h264.mp4").exists());
    assert!(output.join(".rawtl-partial/linear.mkv").is_file());
}

#[test]
fn lossless_roundtrip_mismatch_is_rejected() {
    let root = test_dir("roundtrip-mismatch");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let ffmpeg = fake_ffmpeg_zeroed_video_decode(&fixtures, (WIDTH * HEIGHT * 6) as usize, 3);
    let output = root.join("run");

    let mut args = base_args(&input, &output, &tool);
    args.extend_from_slice(&["--ffmpeg", ffmpeg.to_str().expect("utf8")]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(50), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(
        text.contains("lossless roundtrip mismatch at frame 1"),
        "{text}"
    );
    assert!(
        text.contains("partial/intermediate artifacts retained"),
        "{text}"
    );
    assert!(!output.join("linear-acescg-16bit-ffv1.mkv").exists());
    assert!(!output.join("srgb-preview-h264.mp4").exists());
}

#[test]
fn rejects_overlapping_input_and_output_directories() {
    require_ffmpeg();
    let root = test_dir("overlap");
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);

    // output == input
    let same = root.join("same");
    make_input(&same, &["IMG_1.DNG"]);
    let result = run_cli(&base_args(&same, &same, &tool));
    assert_eq!(result.status.code(), Some(11), "{}", stderr(&result));

    // output inside input
    let material = root.join("material");
    make_input(&material, &["IMG_1.DNG"]);
    let nested = material.join("out");
    let result = run_cli(&base_args(&material, &nested, &tool));
    assert_eq!(result.status.code(), Some(11), "{}", stderr(&result));

    // input inside output
    let outer = root.join("outer");
    let inner = outer.join("material");
    make_input(&inner, &["IMG_1.DNG"]);
    let result = run_cli(&base_args(&inner, &outer, &tool));
    assert_eq!(result.status.code(), Some(11), "{}", stderr(&result));
}

#[test]
fn rejects_non_empty_output_directory() {
    require_ffmpeg();
    let root = test_dir("nonempty");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let output = root.join("run");
    fs::create_dir_all(&output).expect("create output");
    fs::write(output.join("existing.txt"), b"keep me").expect("write existing");

    let result = run_cli(&base_args(&input, &output, &tool));
    assert_eq!(result.status.code(), Some(12), "{}", stderr(&result));
    assert!(output.join("existing.txt").is_file());
}

#[test]
fn detects_a_native_tool_that_ignores_the_frozen_override() {
    let root = test_dir("wb-drift");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, true);
    let output = root.join("run");

    let result = run_cli(&base_args(&input, &output, &tool));
    assert_eq!(result.status.code(), Some(30), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(
        text.contains("should use the frozen override white balance"),
        "{text}"
    );
    assert!(!output.join("linear-acescg-16bit-ffv1.mkv").exists());
}
