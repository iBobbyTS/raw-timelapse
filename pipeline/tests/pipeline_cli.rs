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
        "-c:v h264_videotoolbox",
        "-allow_sw 0",
    ] {
        assert!(manifest.contains(expected), "manifest missing {expected:?}");
    }

    // The default mode is the historical sequence-frozen contract: the full
    // white-balance block is deterministic, so assert its literal seven fields.
    let expected_white_balance = Json::obj(vec![
        (
            "strategy",
            Json::Str("sequence-frozen (REQ-008)".to_string()),
        ),
        ("reference_frame", Json::Str("IMG_1.DNG".to_string())),
        ("reference_source", Json::Str("as-shot".to_string())),
        (
            "reference",
            Json::Array(vec![Json::Float(0.5), Json::Int(1), Json::Float(0.8)]),
        ),
        (
            "propagation",
            Json::Str(
                "first numerically ordered frame decoded with its as-shot neutral; those \
                 three camera-neutral values passed as --white-balance to every later frame"
                    .to_string(),
            ),
        ),
        ("all_applied_equal", Json::Bool(true)),
        (
            "per_frame_as_shot_neutral",
            Json::Str("each frame keeps its own source as_shot_neutral for provenance".to_string()),
        ),
    ]);
    let manifest_json = read_json(&output.join("manifest.json"));
    assert_eq!(
        manifest_json.get("white_balance"),
        Some(&expected_white_balance),
        "the default white_balance block must stay byte-identical"
    );

    // The frozen summary line and the override argv shape of the baseline pass
    // are part of the default run's observable contract.
    let run_stdout = stdout(&result);
    assert!(
        run_stdout.contains(
            "frozen white balance: 0.5,1,0.8 from IMG_1.DNG (applied to every later frame)"
        ),
        "{run_stdout}"
    );
    assert!(
        !run_stdout.contains("per-frame white balance"),
        "the default run must not claim per-frame decoding: {run_stdout}"
    );
    let baseline_command = |index: usize| {
        let text = fs::read_to_string(output.join(format!("logs/analysis/frame_{index:06}.log")))
            .expect("baseline transcript");
        text.lines()
            .find(|line| line.starts_with("command:"))
            .expect("command line")
            .to_string()
    };
    assert!(
        !baseline_command(1).contains("--white-balance"),
        "frame 1 must be decoded with its own as-shot neutral: {}",
        baseline_command(1)
    );
    for index in [2, 3] {
        assert!(
            baseline_command(index).contains("--white-balance 0.5,1,0.8"),
            "frame {index} must receive the frozen triple: {}",
            baseline_command(index)
        );
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

    let encode_log = fs::read_to_string(output.join("logs/encode-preview.log")).expect("log");
    assert!(encode_log.contains("-c:v h264_videotoolbox"), "{encode_log}");
    assert!(encode_log.contains("-allow_sw 0"), "{encode_log}");

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

    // The analysis pass runs first and produces no PNG; its measurement sidecar
    // and transcript are retained, and no final frames directory was created.
    assert!(!output.join("frames").exists());
    assert!(output
        .join("analysis/baseline/frame_000001.analysis.json")
        .is_file());
    assert!(output
        .join("analysis/baseline/meta/frame_000001.json")
        .is_file());
    assert!(!output.join("linear-acescg-16bit-ffv1.mkv").exists());
    assert!(!output.join("srgb-preview-h264.mp4").exists());
    assert!(!output.join(".rawtl-partial").exists());
    let log =
        fs::read_to_string(output.join("logs/analysis/frame_000002.log")).expect("failure log");
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
    // A late encode failure must never leave a success status beside the error.
    let report = read_json(&output.join("exposure_report.json"));
    assert_eq!(
        report.get("run_status").and_then(Json::as_str),
        Some("FAILED")
    );
    assert_eq!(
        report.get("execution_status").and_then(Json::as_str),
        Some("FAILED")
    );
    assert!(
        report.get("error").and_then(Json::as_str).is_some(),
        "encode failure report must carry the error"
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
    let report = read_json(&output.join("exposure_report.json"));
    assert_eq!(
        report.get("run_status").and_then(Json::as_str),
        Some("FAILED"),
        "a roundtrip failure must not report a success status"
    );
    assert!(report.get("error").and_then(Json::as_str).is_some());
}

#[test]
fn rejects_overlapping_input_and_output_directories() {
    let root = test_dir("overlap");
    let tool = root.join("unused-native-tool");

    fn snapshot(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>> {
        let mut tree = std::collections::BTreeMap::new();
        for entry in fs::read_dir(root).expect("read directory") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                tree.insert(path.clone(), None);
                tree.extend(snapshot(&path));
            } else {
                tree.insert(path.clone(), Some(fs::read(path).expect("read file")));
            }
        }
        tree
    }

    let assert_rejected_without_writes = |input: &Path, output: &Path| {
        let before = snapshot(&root);
        let result = run_cli(&base_args(input, output, &tool));
        assert_eq!(result.status.code(), Some(11), "{}", stderr(&result));
        assert!(
            stderr(&result).contains("input/material directory"),
            "{}",
            stderr(&result)
        );
        assert_eq!(
            snapshot(&root),
            before,
            "rejection must leave no new directories or changed files"
        );
    };

    // output == input
    let same = root.join("same");
    make_input(&same, &["IMG_1.DNG"]);
    assert_rejected_without_writes(&same, &same);

    // output inside input
    let material = root.join("material");
    make_input(&material, &["IMG_1.DNG"]);
    let nested = material.join("out");
    assert_rejected_without_writes(&material, &nested);
    assert!(!nested.exists(), "rejected output must not be created");
    assert_rejected_without_writes(&material, &nested.join("deeper/run"));
    assert!(
        !nested.exists(),
        "missing parents must not be created either"
    );

    // Resolve an existing symlinked parent before checking a missing output leaf.
    let alias = root.join("material-alias");
    std::os::unix::fs::symlink(&material, &alias).expect("create material alias");
    assert_rejected_without_writes(&material, &alias.join("out"));
    assert!(
        !nested.exists(),
        "symlink aliases must not bypass the guard"
    );

    // input inside output
    let outer = root.join("outer");
    let inner = outer.join("material");
    make_input(&inner, &["IMG_1.DNG"]);
    assert_rejected_without_writes(&inner, &outer);
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

// ---------------------------------------------------------------------------
// Deflicker node integration (AC1) and REQ-011 regression path (AC3-style)
// ---------------------------------------------------------------------------

use rawtl_linear_video::json::Json;

const SYNTH_WIDTH: u32 = 240;
const SYNTH_HEIGHT: u32 = 180;

struct SyntheticMaterial {
    root: std::path::PathBuf,
    input: std::path::PathBuf,
    tool: std::path::PathBuf,
}

fn synthetic_material(name: &str, frames: Vec<FakeFrame>) -> SyntheticMaterial {
    require_ffmpeg();
    let root = test_dir(name);
    let input = root.join("material");
    let names: Vec<&str> = frames.iter().map(|frame| frame.name.as_str()).collect();
    make_input(&input, &names);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    let tool = fake_native_sequence(&fixtures, &frames, None, false, SYNTH_WIDTH, SYNTH_HEIGHT);
    SyntheticMaterial { root, input, tool }
}

fn run_synthetic(
    material: &SyntheticMaterial,
    output_name: &str,
    extra: &[&str],
) -> (Output, std::path::PathBuf) {
    let output = material.root.join(output_name);
    let mut args = base_args(&material.input, &output, &material.tool);
    args.extend_from_slice(extra);
    (run_cli(&args), output)
}

fn frame_status(report: &Json, index: usize) -> String {
    frame_record(report, index)
        .get("status")
        .and_then(Json::as_str)
        .unwrap_or("MISSING")
        .to_string()
}

fn frame_number(report: &Json, index: usize, field: &str) -> Option<f64> {
    frame_record(report, index)
        .get(field)
        .and_then(Json::as_f64)
}

#[test]
fn default_pairwise_corrects_injected_flicker_and_audit_passes() {
    let gains: Vec<f64> = (0..30)
        .map(|index| if index % 2 == 0 { -0.3 } else { 0.3 })
        .collect();
    let material = synthetic_material("flicker", synthetic_sequence(&gains));
    let (result, output) = run_synthetic(&material, "run", &[]);
    assert!(
        result.status.success(),
        "run failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&result),
        stderr(&result)
    );

    let report = read_json(&output.join("exposure_report.json"));
    assert_eq!(report.get("run_status").and_then(Json::as_str), Some("OK"));
    assert_eq!(
        report
            .get("gain_probe")
            .and_then(|probe| probe.get("passed")),
        Some(&Json::Bool(true)),
        "gain probe must pass on a faithful gain response"
    );
    assert_eq!(
        report
            .get("encoded_temporal_quality")
            .and_then(|quality| quality.get("status"))
            .and_then(Json::as_str),
        Some("PASS"),
        "the default pairwise path must correct the injected flicker"
    );

    let requests: Vec<f64> = gains
        .iter()
        .enumerate()
        .map(|(index, _)| {
            frame_number(&report, index, "requested_ev")
                .unwrap_or_else(|| panic!("frame {index} has no requested_ev"))
        })
        .collect();
    let mean = |indices: &[usize]| {
        indices.iter().map(|&index| requests[index]).sum::<f64>() / indices.len() as f64
    };
    let bright: Vec<usize> = (0..gains.len())
        .filter(|&index| gains[index] > 0.0)
        .collect();
    let dark: Vec<usize> = (0..gains.len())
        .filter(|&index| gains[index] < 0.0)
        .collect();
    assert!(
        mean(&dark) - mean(&bright) > 0.3,
        "requested EV must oppose the injected gain: dark={} bright={}",
        mean(&dark),
        mean(&bright)
    );
    for index in 0..gains.len() {
        assert_eq!(frame_status(&report, index), "OK");
    }
    // The applied sidecars measure the rendered data: adjacent frames no longer
    // show the injected +/-0.3 EV alternation.
    let mut previous: Option<f64> = None;
    for index in 0..gains.len() {
        let sidecar = read_json(&output.join(format!(
            "analysis/applied/frame_{:06}.analysis.json",
            index + 1
        )));
        let level = sidecar
            .get("tiles")
            .and_then(Json::as_array)
            .and_then(|tiles| tiles.first())
            .and_then(|tile| tile.get("log_luminance"))
            .and_then(Json::as_f64)
            .expect("applied tile level");
        if let Some(previous) = previous {
            assert!(
                (level - previous).abs() < 0.1,
                "frame {index} still flickers: {previous} -> {level}"
            );
        }
        previous = Some(level);
    }
    let feedback = report
        .get("feedback_history")
        .and_then(Json::as_array)
        .expect("feedback history");
    assert_eq!(
        feedback.last().and_then(|entry| entry.get("passed")),
        Some(&Json::Bool(true)),
        "render validation must converge"
    );

    for name in [
        "linear-acescg-16bit-ffv1.mkv",
        "srgb-preview-h264.mp4",
        "temporal_audit.json",
        "pairs.csv",
        "frames.csv",
        "exposure-curve.csv",
    ] {
        assert!(output.join(name).is_file(), "missing {name}");
    }
    let manifest = read_json(&output.join("manifest.json"));
    assert_eq!(
        manifest
            .get("deflicker")
            .and_then(|block| block.get("enabled")),
        Some(&Json::Bool(true))
    );
    assert_eq!(
        manifest
            .get("deflicker")
            .and_then(|block| block.get("audit_status"))
            .and_then(Json::as_str),
        Some("PASS")
    );
}

#[test]
fn confirmed_step_is_preserved_and_not_corrected_away() {
    let base = 0.18f64.log2();
    let mut frames = synthetic_sequence(&vec![0.0; 20]);
    for (index, frame) in frames.iter_mut().enumerate() {
        if index >= 10 {
            frame.log_level = base + 0.5;
        }
    }
    let material = synthetic_material("confirmed-step", frames);
    let (result, output) = run_synthetic(
        &material,
        "run",
        &["--step", "10:0.5", "--temporal-quality", "off"],
    );
    // Legacy semantics for the estimator (a genuine level step is not part of
    // the frozen target, so the shared-offset render check reports it as a
    // target error) combined with the fail-open publication contract: the
    // best round is published as COMPLETED_WITH_REVIEW with a recorded
    // warning, never blocked. The estimator side must still preserve the
    // declared step (pairwise intent).
    assert!(
        result.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&result),
        stderr(&result)
    );
    let report = read_json(&output.join("exposure_report.json"));
    assert_eq!(
        report.get("run_status").and_then(Json::as_str),
        Some("COMPLETED_WITH_REVIEW")
    );
    let warnings = report
        .get("warnings")
        .and_then(Json::as_array)
        .expect("warnings");
    assert!(
        warnings.iter().any(|warning| warning
            .as_str()
            .map(|text| text.starts_with("RENDER_TARGETS_NOT_REACHED"))
            .unwrap_or(false)),
        "warnings: {warnings:?}"
    );
    for index in 0..10 {
        assert_eq!(
            frame_number(&report, index, "cumulative_confirmed_step_ev"),
            Some(0.0),
            "frame {index} must precede the declared step"
        );
    }
    for index in 10..20 {
        assert_eq!(
            frame_number(&report, index, "cumulative_confirmed_step_ev"),
            Some(0.5),
            "frame {index} must carry the declared step"
        );
    }
    let mean = |range: std::ops::Range<usize>| {
        let values: Vec<f64> = range
            .map(|index| frame_number(&report, index, "requested_ev").expect("requested_ev"))
            .collect();
        values.iter().sum::<f64>() / values.len() as f64
    };
    assert!(
        (mean(10..20) - mean(0..10)).abs() < 0.1,
        "the estimator must not cancel the confirmed step: before={} after={}",
        mean(0..10),
        mean(10..20)
    );
    // The audit was disabled for this run (--temporal-quality off), so no
    // audit evidence exists beside the published deliverables.
    assert_eq!(
        report
            .get("encoded_temporal_quality")
            .and_then(|quality| quality.get("status"))
            .and_then(Json::as_str),
        Some("SKIPPED")
    );
    assert!(output.join("linear-acescg-16bit-ffv1.mkv").is_file());
    assert!(output.join("srgb-preview-h264.mp4").is_file());
}

#[test]
fn ev_limits_clamp_to_limited_and_fail_the_target_check() {
    let gains: Vec<f64> = (0..12)
        .map(|index| if index % 2 == 0 { 1.0 } else { -1.0 })
        .collect();
    let material = synthetic_material("ev-limits", synthetic_sequence(&gains));
    let (result, output) = run_synthetic(
        &material,
        "run",
        &["--ev-limits", "-0.25,0.25", "--temporal-quality", "off"],
    );
    assert!(
        result.status.success(),
        "run failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&result),
        stderr(&result)
    );
    let report = read_json(&output.join("exposure_report.json"));
    assert_eq!(report.get("run_status").and_then(Json::as_str), Some("OK"));
    let mut limited = 0;
    for index in 0..gains.len() {
        if frame_status(&report, index) == "LIMITED" {
            limited += 1;
            let requested = frame_number(&report, index, "requested_ev").expect("requested");
            let applied = frame_number(&report, index, "applied_ev").expect("applied");
            assert!(
                requested != applied,
                "frame {index}: LIMITED must preserve requested != applied"
            );
            assert!(
                (-0.25..=0.25).contains(&applied),
                "frame {index}: applied {applied} escaped the declared limits"
            );
        }
    }
    assert!(limited > 0, "expected LIMITED frames, report: {report:?}");
    assert_eq!(
        report
            .get("gain_probe")
            .and_then(|probe| probe.get("passed")),
        Some(&Json::Bool(true))
    );
    assert!(output.join("linear-acescg-16bit-ffv1.mkv").is_file());
    assert!(output.join("exposure-curve.csv").is_file());
}

#[test]
fn render_under_execution_publishes_the_best_round_with_a_warning() {
    let gains: Vec<f64> = (0..30)
        .map(|index| if index % 2 == 0 { -0.3 } else { 0.3 })
        .collect();
    let frames = synthetic_sequence(&gains);
    let material = {
        require_ffmpeg();
        let root = test_dir("under-execution");
        let input = root.join("material");
        let names: Vec<&str> = frames.iter().map(|frame| frame.name.as_str()).collect();
        make_input(&input, &names);
        let fixtures = root.join("fixtures");
        fs::create_dir_all(&fixtures).expect("create fixtures");
        let tool = fake_native_sequence_gain_scale(
            &fixtures,
            &frames,
            None,
            false,
            SYNTH_WIDTH,
            SYNTH_HEIGHT,
            0.5,
        );
        SyntheticMaterial { root, input, tool }
    };
    let (result, output) = run_synthetic(&material, "run", &["--temporal-quality", "off"]);
    assert!(
        result.status.success(),
        "fail-open contract must publish: stdout:\n{}\nstderr:\n{}",
        stdout(&result),
        stderr(&result)
    );
    let report = read_json(&output.join("exposure_report.json"));
    assert_eq!(
        report.get("run_status").and_then(Json::as_str),
        Some("COMPLETED_WITH_REVIEW"),
        "unreached render targets flag the run instead of blocking it"
    );
    let warnings = report
        .get("warnings")
        .and_then(Json::as_array)
        .expect("warnings");
    assert!(
        warnings.iter().any(|warning| warning
            .as_str()
            .map(|text| text.starts_with("RENDER_TARGETS_NOT_REACHED"))
            .unwrap_or(false)),
        "warnings: {warnings:?}"
    );
    assert_eq!(
        report
            .get("gain_probe")
            .and_then(|probe| probe.get("passed")),
        Some(&Json::Bool(true)),
        "the probe path executes the gain exactly; only rendering under-executes"
    );
    let feedback = report
        .get("feedback_history")
        .and_then(Json::as_array)
        .expect("feedback history");
    assert!(
        feedback.len() >= 2,
        "the under-execution feedback loop must be recorded"
    );
    // One validation round produced no further feedback update (the loop
    // stops after the third application pass), so the count is rounds - 1.
    assert_eq!(
        report.get("feedback_updates").and_then(Json::as_u64),
        Some(feedback.len() as u64 - 1),
        "failure reports must carry the true feedback update count"
    );
    assert!(
        report
            .get("feedback_updates")
            .and_then(Json::as_u64)
            .unwrap_or(0)
            > 0,
        "the pre-repair bug reported zero updates beside a full history"
    );
    assert!(output.join("linear-acescg-16bit-ffv1.mkv").is_file());
    assert!(output.join("srgb-preview-h264.mp4").is_file());
}

#[test]
fn wrong_correction_passes_the_probe_but_the_audit_records_fail_and_publishes() {
    let base = 0.18f64.log2();
    let frames: Vec<FakeFrame> = (0..30)
        .map(|index| {
            FakeFrame::new(&format!("frame_{:04}.DNG", index), base)
                .object((0.70 - 0.03 * index as f64).max(0.05))
                .capture(&format!(
                    "2026:11:08 20:{:02}:{:02}",
                    index / 60,
                    index % 60
                ))
        })
        .collect();
    let material = synthetic_material("audit-decoupling", frames);
    let (result, output) = run_synthetic(
        &material,
        "run",
        &["--estimator", "legacy", "--brightness", "uniform"],
    );
    assert!(
        result.status.success(),
        "the audit verdict must never block publication: stdout:\n{}\nstderr:\n{}",
        stdout(&result),
        stderr(&result)
    );
    let report = read_json(&output.join("exposure_report.json"));
    assert_eq!(
        report.get("run_status").and_then(Json::as_str),
        Some("COMPLETED_WITH_REVIEW")
    );
    let warnings = report
        .get("warnings")
        .and_then(Json::as_array)
        .expect("warnings");
    assert!(
        warnings.iter().any(|warning| warning
            .as_str()
            .map(|text| text.starts_with("encoded temporal quality FAIL"))
            .unwrap_or(false)),
        "warnings: {warnings:?}"
    );
    assert_eq!(
        report
            .get("gain_probe")
            .and_then(|probe| probe.get("passed")),
        Some(&Json::Bool(true)),
        "the decoupling proof requires a passing gain probe"
    );
    let quality = report.get("encoded_temporal_quality").expect("quality");
    assert_eq!(quality.get("status").and_then(Json::as_str), Some("FAIL"));
    assert!(
        quality
            .get("events")
            .and_then(Json::as_array)
            .map(|events| !events.is_empty())
            .unwrap_or(false),
        "audit FAIL must carry events"
    );
    assert!(output.join("linear-acescg-16bit-ffv1.mkv").is_file());
    assert!(output.join("srgb-preview-h264.mp4").is_file());
    let audit = read_json(&output.join("temporal_audit.json"));
    assert_eq!(audit.get("status").and_then(Json::as_str), Some("FAIL"));
    let manifest = read_json(&output.join("manifest.json"));
    assert_eq!(
        manifest
            .get("deflicker")
            .and_then(|block| block.get("audit_status"))
            .and_then(Json::as_str),
        Some("FAIL"),
        "the manifest must carry the audit verdict verbatim"
    );
}

#[test]
fn unreliable_frames_get_global_reference_fallback_and_publish() {
    let base = 0.18f64.log2();
    let mut frames = synthetic_sequence(&vec![0.0; 12]);
    for (index, frame) in frames.iter_mut().enumerate() {
        frame.log_level = base;
        frame.capture_time = Some(format!("2026:11:08 20:{:02}:{:02}", index / 60, index % 60));
    }
    frames[8].pattern = "sparse";
    let material = synthetic_material("global-reference", frames);

    let (result, output) = run_synthetic(&material, "run", &[]);
    assert!(
        result.status.success(),
        "the fail-open contract always publishes: stdout:\n{}\nstderr:\n{}",
        stdout(&result),
        stderr(&result)
    );
    let report = read_json(&output.join("exposure_report.json"));
    assert_eq!(
        report.get("run_status").and_then(Json::as_str),
        Some("COMPLETED_WITH_REVIEW")
    );
    assert_eq!(frame_status(&report, 8), "GLOBAL_REFERENCE");
    assert!(
        frame_number(&report, 8, "applied_ev").is_some(),
        "the fallback must estimate the unreliable frame instead of 0 EV"
    );
    assert!(frame_number(&report, 8, "render_ev").is_some());
    let frame_eight = frame_record(&report, 8);
    let warnings = frame_eight
        .get("warnings")
        .and_then(Json::as_array)
        .expect("warnings");
    assert!(
        warnings.iter().any(|warning| warning
            .as_str()
            .map(|text| text.starts_with("GLOBAL_REFERENCE_FALLBACK"))
            .unwrap_or(false)),
        "frame 8 warnings: {warnings:?}"
    );
    assert!(
        (0..8).any(|index| frame_status(&report, index) == "OK"),
        "the reliable prefix must stay OK (fill-only fallback)"
    );
    let fallback = report
        .get("global_reference_fallback")
        .expect("global_reference_fallback block");
    assert_eq!(
        fallback.get("frames_filled").and_then(Json::as_u64),
        Some(1),
        "fallback block: {fallback:?}"
    );
    let run_warnings = report
        .get("warnings")
        .and_then(Json::as_array)
        .expect("warnings");
    assert!(run_warnings
        .iter()
        .any(|warning| warning
            .as_str()
            .map(|text| text.starts_with("GLOBAL_REFERENCE_FALLBACK"))
            .unwrap_or(false)));
    assert!(output.join("linear-acescg-16bit-ffv1.mkv").is_file());
    assert!(output.join("srgb-preview-h264.mp4").is_file());
}

#[test]
fn strength_zero_is_the_single_pass_regression_path() {
    let root = test_dir("strength-zero");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);

    let run = |output_name: &str| {
        let output = root.join(output_name);
        let mut args = base_args(&input, &output, &tool);
        args.extend_from_slice(&["--strength", "0"]);
        let result = run_cli(&args);
        assert!(
            result.status.success(),
            "run failed\nstdout:\n{}\nstderr:\n{}",
            stdout(&result),
            stderr(&result)
        );
        output
    };
    let first = run("run-a");
    let second = run("run-b");

    for (index, fixture_name) in [(1, "IMG_1"), (2, "IMG_2"), (3, "IMG_10")] {
        let produced =
            fs::read(first.join(format!("frames/frame_{index:06}.png"))).expect("produced frame");
        let expected =
            fs::read(fixtures.join(format!("{fixture_name}.png"))).expect("fixture frame");
        assert_eq!(produced, expected, "strength 0 rewrote frame {index}");
    }
    assert!(
        !first.join("analysis").exists(),
        "no analysis pass at strength 0"
    );
    assert!(!first.join("temporal_audit.json").exists());
    assert!(!first.join("pairs.csv").exists());
    assert!(!first.join("frames.csv").exists());

    let report = read_json(&first.join("exposure_report.json"));
    for index in 0..3 {
        assert_eq!(frame_status(&report, index), "DISABLED");
    }

    // The disabled (historical) path keeps the frozen argument shape: frame 1
    // decodes with its own as-shot neutral, later frames carry the override.
    let disabled_command = |index: usize| {
        let text = fs::read_to_string(first.join(format!("logs/frame_{index:06}.log")))
            .expect("frame transcript");
        text.lines()
            .find(|line| line.starts_with("command:"))
            .expect("command line")
            .to_string()
    };
    assert!(
        !disabled_command(1).contains("--white-balance"),
        "{}",
        disabled_command(1)
    );
    for index in [2, 3] {
        assert!(
            disabled_command(index).contains("--white-balance 0.5,1,0.8"),
            "{}",
            disabled_command(index)
        );
    }
    assert_eq!(
        report
            .get("encoded_temporal_quality")
            .and_then(|quality| quality.get("status"))
            .and_then(Json::as_str),
        Some("SKIPPED")
    );
    let manifest = read_json(&first.join("manifest.json"));
    assert_eq!(
        manifest
            .get("deflicker")
            .and_then(|block| block.get("enabled")),
        Some(&Json::Bool(false))
    );

    // Same inputs, arguments and toolchain. Matroska container bytes carry a
    // random segment identity (the pre-existing encoder byte-determinism
    // boundary), so byte equality is not claimed; the FFV1 payload and the
    // encoder command must be identical.
    let first_md5 = framemd5(&first.join("linear-acescg-16bit-ffv1.mkv"));
    let second_md5 = framemd5(&second.join("linear-acescg-16bit-ffv1.mkv"));
    assert_eq!(first_md5, second_md5, "strength 0 FFV1 content changed");
    let encode_command = |output: &std::path::Path| {
        let log = fs::read_to_string(output.join("logs/encode-linear.log")).expect("encode log");
        log.lines()
            .find(|line| line.starts_with("command:"))
            .expect("encoder command line")
            .to_string()
    };
    let normalized = |command: String| command.replace("run-a", "RUN").replace("run-b", "RUN");
    assert_eq!(
        normalized(encode_command(&first)),
        normalized(encode_command(&second))
    );
    let roundtrip = fs::read_to_string(first.join("logs/roundtrip.log")).expect("roundtrip log");
    assert!(roundtrip.contains("result: PASS"), "{roundtrip}");
}

#[test]
fn late_encoder_failure_on_disabled_path_reports_failed_status() {
    let root = test_dir("disabled-encode-failure");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let encoder = fake_failing_encoder(&fixtures);
    let output = root.join("run");

    let mut args = base_args(&input, &output, &tool);
    args.extend_from_slice(&[
        "--strength",
        "0",
        "--ffmpeg",
        encoder.to_str().expect("utf8"),
    ]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(40), "{}", stderr(&result));
    let report = read_json(&output.join("exposure_report.json"));
    assert_eq!(
        report.get("run_status").and_then(Json::as_str),
        Some("FAILED"),
        "the disabled path must replace its early OK with the failure class"
    );
    assert_eq!(
        report.get("execution_status").and_then(Json::as_str),
        Some("FAILED")
    );
    assert!(report.get("error").and_then(Json::as_str).is_some());
    assert!(!output.join("linear-acescg-16bit-ffv1.mkv").exists());
}

#[test]
fn fractional_applied_ev_survives_sidecar_precision() {
    // The producer serializes the analysis sidecar with 10 significant digits,
    // so a fractional correction never round-trips through f64 exactly. The
    // legacy/uniform estimator on a linear gain ramp produces such EVs.
    let gains: Vec<f64> = (0..12).map(|index| 0.1 * index as f64).collect();
    let material = synthetic_material("fractional-ev", synthetic_sequence(&gains));
    let (result, output) = run_synthetic(
        &material,
        "run",
        &[
            "--estimator",
            "legacy",
            "--brightness",
            "uniform",
            "--strength",
            "0.7",
            "--temporal-quality",
            "off",
        ],
    );
    assert!(
        result.status.success(),
        "fractional-EV run failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&result),
        stderr(&result)
    );
    let report = read_json(&output.join("exposure_report.json"));
    assert_eq!(report.get("run_status").and_then(Json::as_str), Some("OK"));
    let mut exercised = false;
    for index in 0..gains.len() {
        let applied = frame_number(&report, index, "applied_ev")
            .unwrap_or_else(|| panic!("frame {index} has no applied_ev"));
        let requested = frame_number(&report, index, "requested_ev").expect("requested_ev");
        assert_eq!(applied, requested);
        if format!("{applied:.9e}")
            .parse::<f64>()
            .expect("formatted EV")
            != applied
        {
            exercised = true;
        }
    }
    assert!(
        exercised,
        "the fixture must contain an EV that does not round-trip at 10 significant digits"
    );
    assert!(output.join("linear-acescg-16bit-ffv1.mkv").is_file());
    assert!(output.join("frames/frame_000001.png").is_file());
}

// ---------------------------------------------------------------------------
// White balance modes
// ---------------------------------------------------------------------------

fn white_balance_triple(value: &Json) -> [f64; 3] {
    let values = value.as_array().expect("white balance array");
    assert_eq!(values.len(), 3, "white balance must have three channels");
    [
        values[0].as_f64().expect("channel 0"),
        values[1].as_f64().expect("channel 1"),
        values[2].as_f64().expect("channel 2"),
    ]
}

/// `command:` lines of every per-frame native transcript under `logs_dir`.
fn frame_commands(logs_dir: &Path) -> Vec<(std::path::PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<(std::path::PathBuf, String)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("frame_") && name.ends_with(".log"))
                .unwrap_or(false)
            {
                let text = fs::read_to_string(&path).expect("read frame transcript");
                let command = text
                    .lines()
                    .find(|line| line.starts_with("command:"))
                    .unwrap_or_else(|| panic!("no command line in {}", path.display()));
                out.push((path, command.to_string()));
            }
        }
    }
    let mut out = Vec::new();
    walk(logs_dir, &mut out);
    out
}

fn assert_commands_without_white_balance(logs_dir: &Path) -> usize {
    let commands = frame_commands(logs_dir);
    assert!(
        !commands.is_empty(),
        "expected native frame transcripts under {}",
        logs_dir.display()
    );
    for (path, command) in &commands {
        assert!(
            !command.contains("--white-balance"),
            "{} passed an override: {command}",
            path.display()
        );
    }
    commands.len()
}

/// Every sidecar under `dir` must report `as-shot` and apply exactly its own
/// `as_shot_neutral`; returns the applied triples in filename order.
fn as_shot_sidecars(dir: &Path) -> Vec<[f64; 3]> {
    let mut files: Vec<std::path::PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", dir.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no sidecars under {}", dir.display());
    let mut triples = Vec::with_capacity(files.len());
    for path in files {
        let sidecar = read_json(&path);
        assert_eq!(
            sidecar.get("white_balance_source").and_then(Json::as_str),
            Some("as-shot"),
            "{} must report as-shot",
            path.display()
        );
        let applied = white_balance_triple(
            sidecar
                .get("applied_white_balance")
                .unwrap_or_else(|| panic!("{} has no applied_white_balance", path.display())),
        );
        let own = white_balance_triple(
            sidecar
                .get("as_shot_neutral")
                .unwrap_or_else(|| panic!("{} has no as_shot_neutral", path.display())),
        );
        assert_eq!(
            applied,
            own,
            "{} must apply its own as-shot neutral",
            path.display()
        );
        triples.push(applied);
    }
    triples
}

fn assert_per_frame_manifest(output: &Path) {
    let manifest = read_json(&output.join("manifest.json"));
    let block = manifest
        .get("white_balance")
        .expect("manifest white_balance block");
    assert_eq!(
        block.get("strategy").and_then(Json::as_str),
        Some("per-frame")
    );
    assert_eq!(block.get("reference_frame"), Some(&Json::Null));
    assert_eq!(block.get("reference_source"), Some(&Json::Null));
    assert_eq!(block.get("reference"), Some(&Json::Null));
    let propagation = block
        .get("propagation")
        .and_then(Json::as_str)
        .expect("propagation");
    assert!(
        propagation.contains("own as-shot neutral") && propagation.contains("no --white-balance"),
        "per-frame propagation must describe the per-frame contract: {propagation}"
    );
    let provenance = block
        .get("per_frame_as_shot_neutral")
        .and_then(Json::as_str)
        .expect("per_frame_as_shot_neutral");
    assert!(
        provenance.contains("applied_white_balance"),
        "per-frame provenance must state the applied/own equality: {provenance}"
    );
    let text = fs::read_to_string(output.join("manifest.json")).expect("manifest text");
    assert!(
        !text.contains("all_applied_equal\": true"),
        "a per-frame run must not claim one frozen triple: {text}"
    );
    assert!(
        !text.contains("applied to every later frame"),
        "a per-frame manifest must not claim frozen propagation: {text}"
    );
}

/// Flicker fixture with a distinct as-shot neutral per frame.
fn per_frame_flicker_sequence(count: usize) -> Vec<FakeFrame> {
    (0..count)
        .map(|index| {
            let gain = if index % 2 == 0 { -0.3 } else { 0.3 };
            let red = 0.40 + 0.005 * index as f64;
            let blue = 0.60 + 0.004 * index as f64;
            FakeFrame::new(&format!("frame_{:04}.DNG", index), 0.18f64.log2() + gain)
                .neutral(&format!("{red:.3},1,{blue:.3}"))
                .capture(&format!(
                    "2026:11:08 20:{:02}:{:02}",
                    index / 60,
                    index % 60
                ))
        })
        .collect()
}

#[test]
fn per_frame_mode_decodes_every_frame_with_its_own_as_shot_neutral() {
    let root = test_dir("wb-per-frame");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let output = root.join("run");

    let mut args = base_args(&input, &output, &tool);
    args.extend_from_slice(&["--white-balance", "per-frame", "--strength", "0"]);
    let result = run_cli(&args);
    assert!(
        result.status.success(),
        "run failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&result),
        stderr(&result)
    );

    // Every frame decodes with its own RAW as-shot neutral; the three neutrals
    // differ, so a producer that froze one triple would fail this assertion.
    let applied = as_shot_sidecars(&output.join("frames"));
    assert_eq!(applied.len(), 3);
    assert_ne!(applied[0], applied[1]);
    assert_ne!(applied[0], applied[2]);
    assert_ne!(applied[1], applied[2]);

    assert_commands_without_white_balance(&output.join("logs"));
    assert_per_frame_manifest(&output);

    let run_stdout = stdout(&result);
    assert!(
        run_stdout
            .contains("per-frame white balance: every frame decoded with its own as-shot neutral"),
        "{run_stdout}"
    );
    assert!(
        !run_stdout.contains("applied to every later frame"),
        "the per-frame summary must not claim frozen propagation: {run_stdout}"
    );
    assert!(!run_stdout.contains("frozen white balance"), "{run_stdout}");

    assert!(output.join("linear-acescg-16bit-ffv1.mkv").is_file());
    assert!(output.join("srgb-preview-h264.mp4").is_file());
}

#[test]
fn per_frame_mode_runs_the_activated_probe_and_renders_without_an_override() {
    let frames = per_frame_flicker_sequence(30);
    let material = synthetic_material("wb-per-frame-deflicker", frames);
    let (result, output) = run_synthetic(&material, "run", &["--white-balance", "per-frame"]);
    assert!(
        result.status.success(),
        "run failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&result),
        stderr(&result)
    );

    let report = read_json(&output.join("exposure_report.json"));
    assert_eq!(report.get("run_status").and_then(Json::as_str), Some("OK"));
    let probe = report.get("gain_probe").expect("gain probe block");
    assert_eq!(
        probe.get("status").and_then(Json::as_str),
        Some("PASS"),
        "the gain probe must really run and pass: {probe:?}"
    );
    assert_eq!(probe.get("passed"), Some(&Json::Bool(true)));
    let tested = probe
        .get("tested_frames")
        .and_then(Json::as_array)
        .expect("tested frames");
    assert!(
        !tested.is_empty(),
        "the probe must test real frames: {probe:?}"
    );
    assert!(
        probe
            .get("observations")
            .and_then(Json::as_u64)
            .unwrap_or(0)
            > 0,
        "the probe must produce observations: {probe:?}"
    );

    // Every pass (baseline, render, probe) ran without an override.
    assert_commands_without_white_balance(&output.join("logs"));
    let probe_commands = frame_commands(&output.join("logs/probe"));
    assert!(
        probe_commands.len() >= 2,
        "both probe gain directions must leave real transcripts, found {}",
        probe_commands.len()
    );

    let baseline = as_shot_sidecars(&output.join("analysis/baseline/meta"));
    assert_eq!(baseline.len(), 30);
    let probe_meta = as_shot_sidecars(&output.join("analysis/probe/meta"));
    assert!(!probe_meta.is_empty());
    let rendered = as_shot_sidecars(&output.join("frames"));
    assert_eq!(rendered.len(), 30);
    let mut distinct = std::collections::BTreeSet::new();
    for triple in &rendered {
        assert!(
            distinct.insert(triple.map(f64::to_bits)),
            "the fixture must keep distinct own neutrals, duplicate {triple:?}"
        );
    }
    for index in 0..30 {
        assert_eq!(frame_status(&report, index), "OK");
    }

    assert_per_frame_manifest(&output);
    let run_stdout = stdout(&result);
    assert!(
        !run_stdout.contains("applied to every later frame"),
        "{run_stdout}"
    );
    assert!(output.join("linear-acescg-16bit-ffv1.mkv").is_file());
    assert!(output.join("srgb-preview-h264.mp4").is_file());
}

#[test]
fn per_frame_mode_rejects_a_producer_that_invents_an_override() {
    let root = test_dir("wb-per-frame-override-lie");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_sequence_wb_lie(
        &fixtures,
        &frames,
        FakeWbLie::OverrideWhenUnset,
        WIDTH,
        HEIGHT,
    );
    let output = root.join("run");

    let mut args = base_args(&input, &output, &tool);
    args.extend_from_slice(&["--white-balance", "per-frame", "--strength", "0"]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(30), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(
        text.contains("should use its as-shot white balance"),
        "the error must name the required as-shot contract: {text}"
    );
    assert!(text.contains("frame 1/3"), "{text}");
    assert!(text.contains("override"), "{text}");
    assert!(!output.join("linear-acescg-16bit-ffv1.mkv").exists());
    assert!(!output.join("srgb-preview-h264.mp4").exists());
}

#[test]
fn rejects_a_producer_that_reports_as_shot_but_applies_another_neutral() {
    let root = test_dir("wb-as-shot-lie");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    let frames = standard_frames(&fixtures);
    let tool =
        fake_native_sequence_wb_lie(&fixtures, &frames, FakeWbLie::AsShotMismatch, WIDTH, HEIGHT);
    let output = root.join("run");

    let mut args = base_args(&input, &output, &tool);
    args.extend_from_slice(&["--white-balance", "per-frame", "--strength", "0"]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(30), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(
        text.contains("reports as-shot white balance but applied"),
        "the error must reject the applied/own mismatch: {text}"
    );
    assert!(text.contains("as_shot_neutral"), "{text}");
    assert!(text.contains("frame 1/3"), "{text}");
    assert!(!output.join("linear-acescg-16bit-ffv1.mkv").exists());
}

#[test]
fn rejects_invalid_white_balance_mode_values() {
    let root = test_dir("wb-invalid-mode");
    let input = root.join("material");
    let output = root.join("run");
    let tool = root.join("unused-native-tool");
    for extra in [
        vec!["--white-balance", "bogus"],
        vec!["--white-balance", ""],
        vec!["--white-balance="],
    ] {
        let mut args = base_args(&input, &output, &tool);
        args.extend(extra.iter());
        let result = run_cli(&args);
        assert_eq!(
            result.status.code(),
            Some(2),
            "must reject {extra:?}: {}",
            stderr(&result)
        );
        assert!(
            stderr(&result).contains("--white-balance"),
            "the usage error must name the flag: {}",
            stderr(&result)
        );
    }
}
