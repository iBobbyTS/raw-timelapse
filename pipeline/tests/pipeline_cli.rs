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
fn publishes_one_streamed_video_with_manifest_and_no_frame_files() {
    let (root, input, fixtures) = run_success_setup("success");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let output = root.join("run");
    let result = run_cli(&base_args(&input, &output, &tool));
    assert!(
        result.status.success(),
        "{}\n{}",
        stdout(&result),
        stderr(&result)
    );
    let mut names: Vec<_> = fs::read_dir(&output)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "exposure-curve.csv",
            "exposure_report.json",
            "frames.csv",
            "logs",
            "manifest.json",
            "output.mp4",
            "pairs.csv",
            "temporal_audit.json"
        ]
    );
    let manifest = read_json(&output.join("manifest.json"));
    assert!(manifest.get("linear_video").is_none());
    assert!(manifest.get("preview").is_none());
    let video = manifest.get("video").unwrap();
    assert_eq!(video.get("codec").and_then(Json::as_str), Some("h264"));
    assert_eq!(video.get("file").and_then(Json::as_str), Some("output.mp4"));
    assert_eq!(video.get("frames").and_then(Json::as_u64), Some(3));
    assert!(video.get("bytes").and_then(Json::as_u64).unwrap() > 0);
    let entries = manifest
        .get("frames")
        .and_then(|v| v.get("entries"))
        .and_then(Json::as_array)
        .unwrap();
    for (offset, name) in ["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"].iter().enumerate() {
        let frame = &entries[offset];
        assert_eq!(frame.get("source").and_then(Json::as_str), Some(*name));
        for key in ["pixels_sha256", "source_sha256"] {
            let hash = frame.get(key).and_then(Json::as_str).unwrap();
            assert!(hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()));
        }
        for removed in [
            "png",
            "metadata",
            "png_sha256",
            "applied_white_balance",
            "white_balance_source",
            "as_shot_neutral",
        ] {
            assert!(frame.get(removed).is_none(), "{removed}");
        }
        let log = fs::read_to_string(output.join(frame.get("log").and_then(Json::as_str).unwrap()))
            .unwrap();
        assert!(log.contains("--stdout-frame-f32"));
        assert!(log.contains("pixel bytes: 576"));
        assert!(log.contains("exit: exit code 0"));
        let metadata = metadata_from_log(&log);
        assert_eq!(
            metadata.get("white_balance_source").and_then(Json::as_str),
            Some(if offset == 0 { "as-shot" } else { "override" })
        );
        assert_eq!(
            white_balance_triple(metadata.get("applied_white_balance").unwrap()),
            [0.5, 1.0, 0.8]
        );
        assert!(log.is_ascii());
        assert!(!log.contains('\0'));
    }
    let report = read_json(&output.join("exposure_report.json"));
    assert_eq!(
        report
            .get("analysis")
            .unwrap()
            .get("transport")
            .and_then(Json::as_str),
        Some("MemoryFile; native stdout protocol v1")
    );
    let text = fs::read_to_string(output.join("exposure_report.json")).unwrap();
    assert!(!text.contains("baseline_dir") && !text.contains("applied_dir"));
    let probe = ffprobe(&output.join("output.mp4"));
    for expected in ["codec_name=h264", "width=8", "height=6", "nb_read_frames=3"] {
        assert!(probe.contains(expected), "{probe}");
    }
    let log = fs::read_to_string(output.join("logs/encode.log")).unwrap();
    assert!(log.contains("-i pipe:0"));
    assert!(!log.contains("-nostdin"));
    assert!(log.contains("-c:v h264_videotoolbox -allow_sw 0"));
    assert!(log.contains("scale=out_color_matrix=bt709:out_range=tv"));
    let timings = manifest.get("timings_s").unwrap();
    for key in [
        "analysis",
        "feedback_iteration_0",
        "encode",
        "validate_video",
    ] {
        assert!(timings.get(key).is_some(), "{key}");
    }
}

fn metadata_from_log(text: &str) -> Json {
    let value = text
        .lines()
        .find_map(|line| line.strip_prefix("JSON 0x01: "))
        .expect("metadata JSON in transcript");
    rawtl_linear_video::json::parse(value).unwrap()
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

    assert!(!output.join("output.mp4").exists());
    assert!(!output.join("output.mp4").exists());
    assert!(output.join(".rawtl-partial").is_dir());
    assert!(!output.join("analysis").exists());
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
    assert_eq!(result.status.code(), Some(60), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(text.contains("streaming encoder failed"), "{text}");
    assert!(
        text.contains("partial/intermediate artifacts retained"),
        "{text}"
    );

    assert!(!output.join("output.mp4").exists());
    assert!(!output.join("output.mp4").exists());
    let partial = output.join(".rawtl-partial/output.mp4");
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
    assert_eq!(result.status.code(), Some(60), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(text.contains("codec is \"ffv1\""), "{text}");
    assert!(text.contains("expected \"h264\""), "{text}");
    assert!(
        text.contains("partial/intermediate artifacts retained"),
        "{text}"
    );
    assert!(!output.join("output.mp4").exists());
    assert!(!output.join("output.mp4").exists());
    assert!(output.join(".rawtl-partial/output.mp4").is_file());
}

#[test]
fn forged_metadata_source_hash_is_rejected_by_the_typed_boundary() {
    let result = run_sidecar_defect(
        "forged-metadata-hash",
        FakeSidecarDefects {
            empty_source_sha256: true,
            ..FakeSidecarDefects::default()
        },
    );
    assert_eq!(result.status.code(), Some(30), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(text.contains("frame 1/2"), "{text}");
    assert!(
        text.contains("metadata is missing or invalid field \"source_sha256\""),
        "{text}"
    );
}

#[test]
fn stdout_frame_missing_source_sha256_is_rejected_after_valid_analysis() {
    assert_final_source_hash_rejected("omit");
}

#[test]
fn stdout_frame_null_source_sha256_is_rejected_after_valid_analysis() {
    assert_final_source_hash_rejected("null");
}

#[test]
fn stdout_frame_non_string_source_sha256_is_rejected_after_valid_analysis() {
    assert_final_source_hash_rejected("non-string");
}

#[test]
fn stdout_frame_empty_source_sha256_is_rejected_after_valid_analysis() {
    assert_final_source_hash_rejected("empty");
}

fn assert_final_source_hash_rejected(defect: &str) {
    use std::io::Write;
    let root = test_dir(&format!("final-source-hash-{defect}"));
    let input = root.join("material");
    let frames = synthetic_sequence(&[0.0, 0.1, -0.1]);
    let names: Vec<&str> = frames.iter().map(|frame| frame.name.as_str()).collect();
    make_input(&input, &names);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).unwrap();
    let tool = fake_native_sequence_sidecar_defect(
        &fixtures,
        &frames,
        FakeSidecarDefects::default(),
        WIDTH,
        HEIGHT,
    );
    let config = fixtures.join(format!("fake-native-{}.config", std::process::id()));
    writeln!(
        fs::OpenOptions::new().append(true).open(config).unwrap(),
        "frame_source_sha256_defect={defect}"
    )
    .unwrap();
    let output = root.join("run");
    let result = run_cli(&base_args(&input, &output, &tool));

    // The defect must reach the final consumer after successful measurement
    // passes; a baseline/sidecar rejection would miss the admitted failure.
    for pass in ["analysis", "applied-iter0"] {
        for index in 1..=frames.len() {
            let log = fs::read_to_string(output.join(format!("logs/{pass}/frame_{index:06}.log")))
                .unwrap();
            let metadata = metadata_from_log(&log);
            assert!(metadata
                .get("source_sha256")
                .and_then(Json::as_str)
                .is_some_and(|hash| !hash.is_empty()));
            assert!(
                log.contains("JSON 0x02: ") && log.contains("exit: exit code 0"),
                "{log}"
            );
        }
    }
    let final_log = fs::read_to_string(output.join("logs/encode/frame_000001.log")).unwrap();
    let metadata = metadata_from_log(&final_log);
    match defect {
        "omit" => assert!(metadata.get("source_sha256").is_none()),
        "null" => assert_eq!(metadata.get("source_sha256"), Some(&Json::Null)),
        "non-string" => assert_eq!(metadata.get("source_sha256"), Some(&Json::Int(7))),
        "empty" => assert_eq!(
            metadata.get("source_sha256").and_then(Json::as_str),
            Some("")
        ),
        _ => unreachable!(),
    }
    assert_eq!(
        result.status.code(),
        Some(30),
        "{defect}: final video published={}; stderr: {}",
        output.join("output.mp4").exists(),
        stderr(&result)
    );
    let error = stderr(&result);
    assert!(
        error.contains("frame 1/3") && error.contains(&frames[0].name),
        "{error}"
    );
    assert!(
        error.contains("metadata is missing or invalid field \"source_sha256\""),
        "{error}"
    );
    assert!(!output.join("output.mp4").exists());
    assert!(output.join(".rawtl-partial").is_dir());
    assert!(output.join("exposure_report.json").is_file());
    assert!(
        final_log.contains("command:") && final_log.contains("exit:"),
        "{final_log}"
    );
    assert!(
        !output.join("logs/encode/frame_000002.log").exists(),
        "stop subsequent final-frame dispatch"
    );
    let report = read_json(&output.join("exposure_report.json"));
    assert_eq!(
        report.get("run_status").and_then(Json::as_str),
        Some("FAILED")
    );
}

#[test]
fn forged_analysis_sidecar_without_kind_is_rejected() {
    let result = run_sidecar_defect(
        "forged-analysis-kind",
        FakeSidecarDefects {
            analysis_omit_kind: true,
            ..FakeSidecarDefects::default()
        },
    );
    assert_eq!(result.status.code(), Some(30), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(text.contains("frame 1/2"), "{text}");
    assert!(
        text.contains("analysis sidecar has an unsupported schema"),
        "{text}"
    );
}

#[test]
fn forged_analysis_sidecar_hash_mismatch_is_rejected() {
    let result = run_sidecar_defect(
        "forged-analysis-hash",
        FakeSidecarDefects {
            analysis_sha_mismatch: true,
            ..FakeSidecarDefects::default()
        },
    );
    assert_eq!(result.status.code(), Some(30), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(text.contains("frame 1/2"), "{text}");
    assert!(
        text.contains("analysis sidecar source_sha256 does not match the frame metadata"),
        "{text}"
    );
}

fn run_sidecar_defect(name: &str, defects: FakeSidecarDefects) -> Output {
    let root = test_dir(name);
    let input = root.join("material");
    let frames = synthetic_sequence(&[0.0, 0.1]);
    let names: Vec<&str> = frames.iter().map(|frame| frame.name.as_str()).collect();
    make_input(&input, &names);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    let tool = fake_native_sequence_sidecar_defect(&fixtures, &frames, defects, WIDTH, HEIGHT);
    let output = root.join("run");
    run_cli(&base_args(&input, &output, &tool))
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
    assert!(!output.join("output.mp4").exists());
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
fn pairwise_uniform_corrects_injected_flicker_and_audit_passes() {
    let gains: Vec<f64> = (0..30)
        .map(|index| if index % 2 == 0 { -0.3 } else { 0.3 })
        .collect();
    let material = synthetic_material("flicker", synthetic_sequence(&gains));
    let (result, output) = run_synthetic(&material, "run", &["--brightness", "uniform"]);
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
        assert_eq!(frame_status(&report, index), "UNIFORM");
    }
    // The applied sidecars measure the rendered data: adjacent frames no longer
    // show the injected +/-0.3 EV alternation.
    let mut previous: Option<f64> = None;
    for index in 0..gains.len() {
        let logs = fs::read_to_string(
            output.join(format!("logs/applied-iter0/frame_{:06}.log", index + 1)),
        )
        .unwrap();
        let text = logs
            .lines()
            .find_map(|l| l.strip_prefix("JSON 0x02: "))
            .unwrap();
        let sidecar = rawtl_linear_video::json::parse(text).unwrap();
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
        "output.mp4",
        "output.mp4",
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
    // Confirmed steps now belong to the desired physical signal as well as
    // the estimator. Correct execution preserves the step without inventing
    // a render-target error or feedback correction.
    assert!(
        result.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&result),
        stderr(&result)
    );
    let report = read_json(&output.join("exposure_report.json"));
    assert_eq!(report.get("run_status").and_then(Json::as_str), Some("OK"));
    let warnings = report
        .get("warnings")
        .and_then(Json::as_array)
        .expect("warnings");
    assert!(
        !warnings.iter().any(|warning| warning
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
    assert!(output.join("output.mp4").is_file());
    assert!(output.join("output.mp4").is_file());
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
    // Limited frames cannot be counted as covered merely because the few
    // unconstrained frames meet their own targets.
    assert_eq!(
        report.get("run_status").and_then(Json::as_str),
        Some("COMPLETED_WITH_REVIEW")
    );
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
    let history = report
        .get("feedback_history")
        .and_then(Json::as_array)
        .unwrap();
    let last = history.last().unwrap();
    assert_eq!(
        last.get("unobservable_frames")
            .and_then(Json::as_array)
            .unwrap()
            .len(),
        limited
    );
    assert_eq!(last.get("passed"), Some(&Json::Bool(false)));
    assert_eq!(
        report
            .get("gain_probe")
            .and_then(|probe| probe.get("passed")),
        Some(&Json::Bool(true))
    );
    assert!(output.join("output.mp4").is_file());
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
    // Encoding must use the restored best round, even after later feedback.
    let manifest = read_json(&output.join("manifest.json"));
    let entries = manifest
        .get("frames")
        .and_then(|v| v.get("entries"))
        .and_then(Json::as_array)
        .unwrap();
    for (index, entry) in entries.iter().enumerate() {
        let accepted = frame_number(&report, index, "applied_ev").unwrap();
        assert_eq!(
            entry.get("applied_ev").and_then(Json::as_f64),
            Some(accepted)
        );
        let log =
            fs::read_to_string(output.join(format!("logs/encode/frame_{:06}.log", index + 1)))
                .unwrap();
        assert_eq!(
            metadata_from_log(&log)
                .get("exposure_ev")
                .and_then(Json::as_f64),
            Some(accepted)
        );
    }
    assert!(output.join("output.mp4").is_file());
    assert!(output.join("output.mp4").is_file());
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
    assert!(output.join("output.mp4").is_file());
    assert!(output.join("output.mp4").is_file());
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
    assert!(run_warnings.iter().any(|warning| warning
        .as_str()
        .map(|text| text.starts_with("GLOBAL_REFERENCE_FALLBACK"))
        .unwrap_or(false)));
    assert!(output.join("output.mp4").is_file());
    assert!(output.join("output.mp4").is_file());
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

    let entries = read_json(&first.join("manifest.json"));
    for entry in entries
        .get("frames")
        .and_then(|v| v.get("entries"))
        .and_then(Json::as_array)
        .unwrap()
    {
        assert_eq!(entry.get("applied_ev").and_then(Json::as_f64), Some(0.0));
        assert!(entry.get("pixels_sha256").and_then(Json::as_str).is_some());
    }
    assert!(
        !first.join("analysis").exists(),
        "no analysis pass at strength 0"
    );
    assert!(!first.join("temporal_audit.json").exists());
    assert!(first.join("pairs.csv").is_file());
    assert!(first.join("frames.csv").is_file());

    let report = read_json(&first.join("exposure_report.json"));
    for index in 0..3 {
        assert_eq!(frame_status(&report, index), "DISABLED");
    }
    // The disabled (historical) path keeps the frozen argument shape: frame 1
    // decodes with its own as-shot neutral, later frames carry the override.
    let disabled_command = |index: usize| {
        let text = fs::read_to_string(first.join(format!("logs/encode/frame_{index:06}.log")))
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

    // Both same-candidate runs traverse the pipe with the same encoder options.
    let command = |output: &Path| {
        fs::read_to_string(output.join("logs/encode.log"))
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string()
    };
    assert_eq!(
        command(&first).replace("run-a", "RUN"),
        command(&second).replace("run-b", "RUN")
    );
    assert!(!first.join("frames").exists());
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
    assert_eq!(result.status.code(), Some(60), "{}", stderr(&result));
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
    assert!(!output.join("output.mp4").exists());
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
    assert!(output.join("output.mp4").is_file());
    assert!(!output.join("frames").exists());
}

// ---------------------------------------------------------------------------
// Preview output colour/Log profiles
// ---------------------------------------------------------------------------

/// Build a 64x48 three-frame sequence whose frames are a uniform RGB16 ACEScg
/// code triple. The fake producer copies the fixtures at EV 0.
fn profile_material(
    name: &str,
    rgb: [u16; 3],
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    profile_material_for_extension(name, rgb, "DNG")
}

fn profile_material_for_extension(
    name: &str,
    rgb: [u16; 3],
    extension: &str,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    require_ffmpeg();
    let root = test_dir(name);
    let input = root.join("material");
    let names: Vec<String> = (0..3)
        .map(|index| format!("frame_{index:04}.{extension}"))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    make_input(&input, &refs);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("create fixtures");
    let mut frames = Vec::new();
    for index in 0..3 {
        let path = fixtures.join(format!("patch_{index}.png"));
        write_rgb48_png(&path, 64, 48, &vec![rgb; 64 * 48]);
        let mut frame = FakeFrame::new(&format!("frame_{index:04}.{extension}"), 0.0);
        frame.fixture = Some(path);
        frame.capture_time = Some(format!("2026:11:08 20:00:{:02}", index + 1));
        frames.push(frame);
    }
    let tool = fake_native_sequence(&fixtures, &frames, None, false, 64, 48);
    (root, input, tool)
}

fn run_profile(root: &Path, input: &Path, tool: &Path, id: &str) -> Output {
    let output = root.join("run");
    let mut args = base_args(input, &output, tool);
    args.push("--preview-profile");
    args.push(id);
    run_cli(&args)
}

#[test]
fn preview_profiles_emit_their_declared_colour_contract() {
    let cases: [(&str, &str, &str, &[&str], bool, &str, &str); 6] = [
        (
            "srgb-preview",
            "output.mp4",
            "tv",
            &["yuv420p"],
            false,
            "bt709",
            "iec.ch",
        ),
        (
            "sony-slog3-sgamut3",
            "output.mp4",
            "pc",
            &["yuv420p", "yuvj420p"],
            true,
            "bt2020nc",
            "sony.jp",
        ),
        (
            "sony-slog3-sgamut3-cine",
            "output.mp4",
            "pc",
            &["yuv420p", "yuvj420p"],
            true,
            "bt2020nc",
            "sony.jp",
        ),
        (
            "canon-log2-cinema-gamut",
            "output.mp4",
            "pc",
            &["yuv420p", "yuvj420p"],
            true,
            "bt2020nc",
            "canon.com",
        ),
        (
            "nikon-nlog-bt2020",
            "output.mp4",
            "pc",
            &["yuv420p", "yuvj420p"],
            true,
            "bt2020nc",
            "nikon",
        ),
        (
            "dji-dlog-dgamut",
            "output.mp4",
            "pc",
            &["yuv420p", "yuvj420p"],
            true,
            "bt2020nc",
            "djicdn",
        ),
    ];

    for (id, filename, range, pix_fmts, vendor, matrix, source_marker) in cases {
        let (root, input, fixtures) = run_success_setup(&format!("profile-contract-{id}"));
        let frames = standard_frames(&fixtures);
        let tool = fake_native_tool(&fixtures, &frames, None, false);
        let result = run_profile(&root, &input, &tool, id);
        assert!(
            result.status.success(),
            "{id} run failed\nstdout:\n{}\nstderr:\n{}",
            stdout(&result),
            stderr(&result)
        );
        let output = root.join("run");
        let manifest = read_json(&output.join("manifest.json"));
        assert_eq!(
            manifest
                .get("quantization_policy")
                .unwrap()
                .get("linear_dtype")
                .and_then(Json::as_str),
            Some("signed f32 LE AP1/D60")
        );
        for (path, command) in frame_commands(&output.join("logs")) {
            assert!(
                command.contains("--camera-color-method legacy-inverse-cm"),
                "{command}"
            );
            if path.to_string_lossy().contains("/encode/") {
                assert!(command.contains("--stdout-frame-f32"), "{command}");
            } else {
                assert!(command.contains("--stdout-analysis"), "{command}");
            }
        }
        let preview = output.join(filename);
        assert!(preview.is_file(), "{id}: missing {filename}");

        let probe = ffprobe_color(&preview);
        for expected in ["codec_name=h264", "width=8", "height=6", "nb_read_frames=3"] {
            assert!(probe.contains(expected), "{id}: {probe}");
        }
        assert!(
            probe.contains(&format!("color_range={range}")),
            "{id}: {probe}"
        );
        let pix_fmt = probe
            .lines()
            .find_map(|line| line.strip_prefix("pix_fmt="))
            .expect("pix_fmt");
        assert!(
            pix_fmts.contains(&pix_fmt),
            "{id}: pix_fmt {pix_fmt:?} not in {pix_fmts:?}"
        );
        let expected = if !vendor {
            ColorCodes {
                primaries: 1,
                transfer: 13,
                matrix: 1,
                full: 0,
            }
        } else {
            ColorCodes {
                primaries: if id == "nikon-nlog-bt2020" { 9 } else { 2 },
                transfer: 2,
                matrix: 9,
                full: 1,
            }
        };
        let primaries = if !vendor {
            "bt709"
        } else if id == "nikon-nlog-bt2020" {
            "bt2020"
        } else {
            "unknown"
        };
        for tag in [
            format!("color_space={matrix}"),
            format!("color_primaries={primaries}"),
            format!(
                "color_transfer={}",
                if vendor { "unknown" } else { "iec61966-2-1" }
            ),
        ] {
            assert!(probe.contains(&tag), "{id}: {probe}");
        }
        let container = mp4_nclx(&preview);
        let bitstream = h264_sps_vui(&preview);
        assert_eq!(container, expected, "{id}: container");
        assert_eq!(bitstream, expected, "{id}: SPS VUI");
        assert_eq!(container, bitstream, "{id}: layers disagree");
        println!(
            "tags {id}: MP4 nclx {container:?}; H.264 SPS VUI {bitstream:?}; ffprobe\n{probe}"
        );

        let manifest = fs::read_to_string(output.join("manifest.json")).expect("manifest");
        let json = rawtl_linear_video::json::parse(&manifest).expect("manifest JSON");
        let tags = json
            .get("video")
            .unwrap()
            .get("emitted_tags")
            .expect("emitted_tags");
        for layer in ["h264_sps_vui", "mp4_colr_nclx"] {
            let fields = tags.get(layer).expect(layer);
            for (field, value) in [
                ("colour_primaries", expected.primaries),
                ("transfer_characteristics", expected.transfer),
                ("matrix_coefficients", expected.matrix),
                ("video_full_range_flag", expected.full),
            ] {
                assert_eq!(
                    fields.get(field).and_then(|v| v.as_u64()),
                    Some(value as u64),
                    "{id} {layer} {field}"
                );
            }
        }
        for expected in [
            format!("\"file\": \"{filename}\""),
            format!("\"profile\": \"{id}\""),
            "\"dither\": false".to_string(),
            format!("\"range\": \"{range}\""),
            format!("\"ycbcr_matrix\": \"{matrix}\""),
            "\"full_decode\": \"pass\"".to_string(),
        ] {
            assert!(
                manifest.contains(&expected),
                "{id}: manifest missing {expected:?}"
            );
        }
        assert!(
            manifest.contains(source_marker),
            "{id}: manifest must cite the vendor source ({source_marker})"
        );
        assert!(
            manifest.contains("\"reference_codes\""),
            "{id}: manifest must record official reference codes"
        );
    }
}

/// Independent transcription of BT.2020-2 (10/2015), Table 4 NCL equations:
/// https://www.itu.int/rec/R-REC-BT.2020-2-201510-I/en
/// Y'=Kr*R'+(1-Kr-Kb)*G'+Kb*B', Cb=(B'-Y')/2(1-Kb), Cr=(R'-Y')/2(1-Kr).
/// Compare the actual swscale token used by the pipeline with those equations,
/// before lossy H.264 encoding; allow one code for fixed-point rounding.
#[test]
fn bt2020_ncl_swscale_matches_the_standard_derivation() {
    require_ffmpeg();
    let root = test_dir("bt2020-standard");
    for rgb in [
        [255u8, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
        [196, 64, 32],
        [128; 3],
    ] {
        let input = root.join(format!("{rgb:?}.rgb"));
        fs::write(&input, rgb.repeat(64 * 48)).expect("write RGB fixture");
        let [r, g, b] = rgb.map(|code| code as f64 / 255.0);
        let y = 0.2627 * r + 0.6780 * g + 0.0593 * b;
        let cb = (b - y) / 1.8814;
        let cr = (r - y) / 1.4746;
        for (range, y_scale, c_scale, y_offset) in
            [("tv", 219.0, 224.0, 16.0), ("pc", 255.0, 255.0, 0.0)]
        {
            let output = root.join(format!("{rgb:?}-{range}.yuv"));
            let filter = format!("scale=out_color_matrix=bt2020:out_range={range},format=yuv444p");
            let result = std::process::Command::new(FFMPEG)
                .args([
                    "-v",
                    "error",
                    "-nostdin",
                    "-f",
                    "rawvideo",
                    "-pixel_format",
                    "rgb24",
                    "-video_size",
                    "64x48",
                    "-i",
                ])
                .arg(&input)
                .args(["-frames:v", "1", "-vf", &filter, "-f", "rawvideo"])
                .arg(&output)
                .output()
                .expect("run swscale");
            assert!(result.status.success(), "{}", stderr(&result));
            let bytes = fs::read(output).expect("YCbCr fixture");
            assert_eq!(bytes.len(), 64 * 48 * 3);
            let actual = [bytes[0], bytes[64 * 48], bytes[2 * 64 * 48]];
            let expected = [
                y_offset + y_scale * y,
                128.0 + c_scale * cb,
                128.0 + c_scale * cr,
            ]
            .map(|code| code.round().clamp(0.0, 255.0) as u8);
            for channel in 0..3 {
                assert!(
                    (actual[channel] as i32 - expected[channel] as i32).abs() <= 1,
                    "{rgb:?} {range}: swscale {actual:?} vs BT.2020-2 {expected:?}"
                );
            }
            println!(
                "BT.2020 NCL {rgb:?} {range}: standard {expected:?}, swscale {actual:?}: PASS"
            );
        }
    }
}

/// The known patch goes through the real render -> encode -> decode path. The
/// expected triples are independently derived (reference_derive.py); they are
/// separated by >=25 codes so the observation rejects a wrong or bypassed
/// profile, and the two Sony gamuts differ by 59 codes in red.
#[test]
fn preview_profiles_map_a_known_patch_through_the_encoded_stream() {
    // Uniform 64x48 patches have <=3 codes of correct-path error. The nearest
    // wrong-matrix result is Canon at 5 codes (wrong range: 8), so a vendor
    // bound of 3 leaves a two-code rejection margin without relying on tags.
    // Keep the historical sRGB bound; its live path is outside this repair.
    const VENDOR_TOLERANCE: i32 = 3;
    const SRGB_TOLERANCE: i32 = 12;
    let cases: [(&str, &str, &str, &str, [u8; 3]); 6] = [
        ("srgb-preview", "output.mp4", "bt709", "tv", [0, 255, 0]),
        (
            "sony-slog3-sgamut3",
            "output.mp4",
            "bt2020",
            "pc",
            [59, 147, 20],
        ),
        (
            "sony-slog3-sgamut3-cine",
            "output.mp4",
            "bt2020",
            "pc",
            [0, 145, 49],
        ),
        (
            "canon-log2-cinema-gamut",
            "output.mp4",
            "bt2020",
            "pc",
            [48, 140, 96],
        ),
        (
            "nikon-nlog-bt2020",
            "output.mp4",
            "bt2020",
            "pc",
            [0, 154, 0],
        ),
        (
            "dji-dlog-dgamut",
            "output.mp4",
            "bt2020",
            "pc",
            [0, 145, 74],
        ),
    ];

    let mut observed = Vec::new();
    for (id, filename, matrix, range, expected) in cases {
        // AP1/D60 pure green: 0.0, 1.0, 0.0 in PNG RGB16 codes.
        let (root, input, tool) = profile_material(&format!("profile-patch-{id}"), [0, 16384, 0]);
        let result = run_profile(&root, &input, &tool, id);
        assert!(
            result.status.success(),
            "{id} run failed\nstdout:\n{}\nstderr:\n{}",
            stdout(&result),
            stderr(&result)
        );
        let preview = root.join("run").join(filename);
        let decoded = decode_first_pixel(&preview, matrix, range);
        let difference = (0..3)
            .map(|channel| (decoded[channel] as i32 - expected[channel] as i32).abs())
            .max()
            .unwrap_or(0);
        let tolerance = if id == "srgb-preview" {
            SRGB_TOLERANCE
        } else {
            VENDOR_TOLERANCE
        };
        let accepts = |pixel: [u8; 3]| {
            (0..3)
                .map(|channel| (pixel[channel] as i32 - expected[channel] as i32).abs())
                .max()
                .unwrap_or(0)
                <= tolerance
        };
        assert!(accepts(decoded),
            "{id}: decoded {decoded:?} vs independent reference {expected:?} (tolerance {tolerance})");
        println!(
            "sensitivity {id}: correct {decoded:?}, error {difference}, bound {tolerance}: PASS"
        );

        if id != "srgb-preview" {
            for (name, from, to) in [
                (
                    "wrong-matrix",
                    "out_color_matrix=bt2020",
                    "out_color_matrix=bt709",
                ),
                ("wrong-range", "out_range=pc", "out_range=tv"),
            ] {
                // Mutate only the preview's pixel conversion, preserving all
                // tags and encoder arguments. Decode under the declared matrix
                // and range, so metadata cannot make a wrong conversion pass.
                let shim = root.join(format!("ffmpeg-{name}"));
                write_executable(&shim, &format!(
                    "#!/bin/bash\nargs=()\nfor arg in \"$@\"; do\n  args+=(\"${{arg//{from}/{to}}}\")\ndone\nexec {} \"${{args[@]}}\"\n",
                    shell_quote(FFMPEG)
                ));
                let output = root.join(name);
                let mut args = base_args(&input, &output, &tool);
                args.extend_from_slice(&[
                    "--preview-profile",
                    id,
                    "--ffmpeg",
                    shim.to_str().expect("utf8"),
                ]);
                let result = run_cli(&args);
                assert!(result.status.success(), "{id} {name}: {}", stderr(&result));
                let wrong = decode_first_pixel(&output.join(filename), matrix, range);
                let wrong_difference = (0..3)
                    .map(|channel| (wrong[channel] as i32 - expected[channel] as i32).abs())
                    .max()
                    .unwrap_or(0);
                assert!(!accepts(wrong),
                    "{id} {name} must fail the same patch bound {tolerance}: {wrong:?} vs {expected:?}");
                println!("sensitivity {id}: {name} {wrong:?}, error {wrong_difference}, bound {tolerance}: REJECTED");
            }
        }
        observed.push((id, expected));
    }

    // A wrong profile cannot pass: the independent references are far apart.
    for left in 0..observed.len() {
        for right in left + 1..observed.len() {
            let (left_id, left_code) = observed[left];
            let (right_id, right_code) = observed[right];
            let distance = (0..3)
                .map(|channel| (left_code[channel] as i32 - right_code[channel] as i32).abs())
                .max()
                .unwrap_or(0);
            assert!(
                distance > SRGB_TOLERANCE,
                "{left_id} and {right_id} references are not distinguishable: {left_code:?} vs {right_code:?}"
            );
        }
    }

    // The two Sony gamuts are separated by 59 codes in red.
    let sony = observed
        .iter()
        .find(|(id, _)| *id == "sony-slog3-sgamut3")
        .expect("sony")
        .1;
    let cine = observed
        .iter()
        .find(|(id, _)| *id == "sony-slog3-sgamut3-cine")
        .expect("cine")
        .1;
    assert!((sony[0] as i32 - cine[0] as i32).abs() >= 25);
}

/// No decoder colour/range overrides: tags must select the same conversion as
/// the forced-correct path. Uniform 64x48 patches isolate matrix/range errors
/// from edge chroma sampling. Correct hardware-codec error is <=3 codes; wrong
/// matrix green-patch error is >=5. Keep the same three-code acceptance margin.
#[test]
fn vendor_previews_auto_decode_from_their_own_tags() {
    for (id, filename, expected) in [
        ("sony-slog3-sgamut3", "output.mp4", [59u8, 147, 20]),
        ("sony-slog3-sgamut3-cine", "output.mp4", [0, 145, 49]),
        ("canon-log2-cinema-gamut", "output.mp4", [48, 140, 96]),
        (REV3_PROFILE, "output.mp4", [39, 135, 77]),
        ("nikon-nlog-bt2020", "output.mp4", [0, 154, 0]),
        ("dji-dlog-dgamut", "output.mp4", [0, 145, 74]),
    ] {
        let (root, input, tool) = profile_material_for_extension(
            &format!("auto-tags-{id}"),
            [0, 16384, 0],
            if id == REV3_PROFILE { "CR3" } else { "DNG" },
        );
        let result = run_profile(&root, &input, &tool, id);
        assert!(result.status.success(), "{id}: {}", stderr(&result));
        let preview = root.join("run").join(filename);
        let auto = decode_first_pixel_auto(&preview);
        let correct = decode_first_pixel(&preview, "bt2020", "pc");
        let wrong_matrix = decode_first_pixel(&preview, "bt709", "pc");
        // On this FFmpeg build a scale in_range hint on a tagged yuvj420p
        // stream is not a reliable counterfactual. Change the actual range
        // declarations in BOTH layers, then decode without overrides again.
        let limited = root.join("wrong-range-tags.mp4");
        let retag = std::process::Command::new(FFMPEG)
            .args(["-v", "error", "-nostdin", "-i"])
            .arg(&preview)
            .args([
                "-c:v",
                "copy",
                "-color_range",
                "tv",
                "-bsf:v",
                "h264_metadata=video_full_range_flag=0",
                "-movflags",
                "+faststart+write_colr",
            ])
            .arg(&limited)
            .output()
            .expect("retag range");
        assert!(retag.status.success(), "{id}: {}", stderr(&retag));
        assert_eq!(mp4_nclx(&limited).full, 0, "{id} limited nclx");
        assert_eq!(h264_sps_vui(&limited).full, 0, "{id} limited SPS");
        let wrong_range = decode_first_pixel_auto(&limited);
        let distance = |a: [u8; 3], b: [u8; 3]| {
            (0..3)
                .map(|c| (a[c] as i32 - b[c] as i32).abs())
                .max()
                .unwrap()
        };
        assert!(
            distance(auto, expected) <= 3,
            "{id} auto {auto:?} vs reference {expected:?}"
        );
        assert!(
            distance(auto, correct) <= 1,
            "{id} auto {auto:?} vs forced {correct:?}"
        );
        assert!(
            distance(auto, wrong_matrix) >= 4,
            "{id} auto {auto:?} vs wrong matrix {wrong_matrix:?}"
        );
        assert!(
            distance(auto, wrong_range) > 3,
            "{id} auto {auto:?} vs wrong range {wrong_range:?}"
        );
        // Judge decoder selection against the forced-correct decode: codec
        // error can cancel a wrong inverse's error against the pre-encode RGB
        // reference (observed for Sony). The separate encode mutations below
        // remain checked against that independent reference for every vendor.
        assert!(
            distance(wrong_range, expected) > 3,
            "{id}: wrong range passes reference"
        );
        println!("auto {id}: reference={expected:?} auto={auto:?} forced={correct:?} wrong-matrix={wrong_matrix:?} wrong-range={wrong_range:?}; errors auto/ref={} auto/forced={} auto/wrong-matrix={} auto/wrong-range={}",
            distance(auto,expected),distance(auto,correct),distance(auto,wrong_matrix),distance(auto,wrong_range));
    }
}

/// Decoding with the wrong YCbCr matrix must visibly disagree with the declared
/// reference, so the end-to-end check is not satisfied by a decoder default.
#[test]
fn wrong_ycbcr_matrix_is_rejected_by_the_patch_check() {
    // N-Log compresses highlights least in this range, so AP1/D60 red 3.5 maps
    // to a maximally chromatic coded triple [202,0,0]; the BT.709 inverse
    // differs by 11 codes from the declared BT.2020 matrix, while H.264 error
    // on the uniform patch stays well below that.
    const CORRECT_TOLERANCE: i32 = 8;
    let (root, input, tool) = profile_material("profile-wrong-matrix", [57344, 0, 0]);
    let result = run_profile(&root, &input, &tool, "nikon-nlog-bt2020");
    assert!(
        result.status.success(),
        "run failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&result),
        stderr(&result)
    );
    let preview = root.join("run").join("output.mp4");
    let expected = [202u8, 0, 0];
    let decoded = decode_first_pixel(&preview, "bt2020", "pc");
    let difference = (0..3)
        .map(|channel| (decoded[channel] as i32 - expected[channel] as i32).abs())
        .max()
        .unwrap_or(0);
    assert!(
        difference <= CORRECT_TOLERANCE,
        "declared matrix: {decoded:?} vs {expected:?}"
    );

    let wrong = decode_first_pixel(&preview, "bt709", "pc");
    let wrong_difference = (0..3)
        .map(|channel| (wrong[channel] as i32 - expected[channel] as i32).abs())
        .max()
        .unwrap_or(0);
    assert!(
        wrong_difference > CORRECT_TOLERANCE,
        "decoding with the wrong matrix must be rejected: {wrong:?} vs {expected:?}"
    );
}

#[test]
fn rejects_unknown_or_empty_preview_profile_with_exit_usage() {
    let (root, input, fixtures) = run_success_setup("profile-unknown");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let output = root.join("run");
    for value in ["no-such-profile", ""] {
        let mut args = base_args(&input, &output, &tool);
        args.push("--preview-profile");
        args.push(value);
        let result = run_cli(&args);
        assert_eq!(
            result.status.code(),
            Some(2),
            "value {value:?} must be EXIT_USAGE\nstdout:\n{}\nstderr:\n{}",
            stdout(&result),
            stderr(&result)
        );
        assert!(
            stderr(&result).contains("--preview-profile"),
            "{}",
            stderr(&result)
        );
        assert!(!output.exists(), "usage rejection must not create output");
    }
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
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("log"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no sidecars under {}", dir.display());
    let mut triples = Vec::with_capacity(files.len());
    for path in files {
        let sidecar = metadata_from_log(&fs::read_to_string(&path).unwrap());
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
    let applied = as_shot_sidecars(&output.join("logs/encode"));
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

    assert!(output.join("output.mp4").is_file());
    assert!(output.join("output.mp4").is_file());
}

#[test]
fn per_frame_mode_runs_the_activated_probe_and_renders_without_an_override() {
    let frames = per_frame_flicker_sequence(30);
    let material = synthetic_material("wb-per-frame-deflicker", frames);
    let (result, output) = run_synthetic(
        &material,
        "run",
        &["--white-balance", "per-frame", "--brightness", "uniform"],
    );
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

    let baseline = as_shot_sidecars(&output.join("logs/analysis"));
    assert_eq!(baseline.len(), 30);
    let probe_meta = as_shot_sidecars(&output.join("logs/probe"));
    assert!(!probe_meta.is_empty());
    let rendered = as_shot_sidecars(&output.join("logs/encode"));
    assert_eq!(rendered.len(), 30);
    let mut distinct = std::collections::BTreeSet::new();
    for triple in &rendered {
        assert!(
            distinct.insert(triple.map(f64::to_bits)),
            "the fixture must keep distinct own neutrals, duplicate {triple:?}"
        );
    }
    for index in 0..30 {
        assert_eq!(frame_status(&report, index), "UNIFORM");
    }

    assert_per_frame_manifest(&output);
    let run_stdout = stdout(&result);
    assert!(
        !run_stdout.contains("applied to every later frame"),
        "{run_stdout}"
    );
    assert!(output.join("output.mp4").is_file());
    assert!(output.join("output.mp4").is_file());
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
    assert!(!output.join("output.mp4").exists());
    assert!(!output.join("output.mp4").exists());
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
    assert!(!output.join("output.mp4").exists());
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

/// Complete within a fixed bound and verify each instrumented owned process
/// disappeared before the CLI returned. Output files avoid pipe-fill deadlocks
/// in the test harness itself.
fn bounded_owned_run(args: &[&str], root: &Path) -> Output {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let out = root.join("bounded.stdout");
    let err = root.join("bounded.stderr");
    let pids = root.join("owned.pids");
    let mut child = Command::new(env!("CARGO_BIN_EXE_rawtl-linear-video"))
        .args(args)
        .env("FAKE_NATIVE_PIDS", &pids)
        .stdout(Stdio::from(fs::File::create(&out).unwrap()))
        .stderr(Stdio::from(fs::File::create(&err).unwrap()))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let (status, in_time) = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break (status, true);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            break (child.wait().unwrap(), false);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut owned = fs::read_to_string(&pids).unwrap_or_default();
    fn log_pids(dir: &Path, owned: &mut String) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    log_pids(&entry.path(), owned);
                } else if let Ok(log) = fs::read_to_string(entry.path()) {
                    for line in log.lines() {
                        if let Some(pid) = line.strip_prefix("pid: ") {
                            owned.push_str(pid);
                            owned.push('\n');
                        }
                    }
                }
            }
        }
    }
    log_pids(&root.join("run/logs"), &mut owned);
    assert!(
        !owned.is_empty(),
        "the fixture must instrument real owned children"
    );
    let mut survivors = vec![];
    for pid in owned.lines() {
        assert!(pid.parse::<u32>().unwrap() > 1);
        let exists = Command::new("kill")
            .args(["-0", pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        if exists {
            survivors.push(pid.to_string());
            let _ = Command::new("kill").args(["-9", pid]).status();
        }
    }
    assert!(
        in_time,
        "pipeline exceeded bounded completion deadline; stdout {} stderr {}",
        out.display(),
        err.display()
    );
    assert!(
        survivors.is_empty(),
        "owned children survived CLI return: {survivors:?}"
    );
    Output {
        status,
        stdout: fs::read(out).unwrap(),
        stderr: fs::read(err).unwrap(),
    }
}

#[test]
fn protocol_and_child_failures_reap_live_counterparts_and_never_publish() {
    for (label, knob, encoder_exit, code, expected) in [
        ("bad-magic", "bad_magic=1", false, 30, "magic"),
        (
            "short-pixels",
            "short_pixels=1",
            false,
            30,
            "short record payload",
        ),
        ("missing-metadata", "omit_metadata=1", false, 30, "width"),
        (
            "descriptor-geometry",
            "descriptor_geometry_mismatch=1",
            false,
            30,
            "descriptor geometry",
        ),
        (
            "mid-run",
            "frame_fail_on=IMG_2.DNG",
            false,
            30,
            "synthetic mid-run frame failure",
        ),
        (
            "nonzero-after-records",
            "nonzero_after_records=1",
            false,
            30,
            "exit code 3",
        ),
        (
            "early-encoder",
            "stall_pixels=1",
            true,
            60,
            "streaming encoder failed",
        ),
    ] {
        let root = test_dir(&format!("bounded-{label}"));
        let input = root.join("material");
        make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_3.DNG"]);
        let fixtures = root.join("fixtures");
        fs::create_dir_all(&fixtures).unwrap();
        let frames: Vec<_> = ["IMG_1.DNG", "IMG_2.DNG", "IMG_3.DNG"]
            .into_iter()
            .map(|s| FakeFrame::new(s, 0.18f64.log2()))
            .collect();
        let native = fake_native_tool(&fixtures, &frames, None, false);
        let config = fixtures.join(format!("fake-native-{}.config", std::process::id()));
        use std::io::Write;
        writeln!(
            fs::OpenOptions::new().append(true).open(config).unwrap(),
            "{knob}\nwidth=240\nheight=180"
        )
        .unwrap();
        let encoder = fixtures.join("controlled-encoder");
        let body = if encoder_exit {
            "exit 7"
        } else {
            "exec /usr/bin/python3 -c 'import sys,time; sys.stdin.buffer.read(129600); time.sleep(60)'"
        };
        write_executable(
            &encoder,
            &format!("#!/bin/sh\necho $$ >> \"$FAKE_NATIVE_PIDS\"\n{body}\n"),
        );
        let output = root.join("run");
        let mut args = base_args(&input, &output, &native);
        args.extend(["--strength", "0", "--ffmpeg", encoder.to_str().unwrap()]);
        let result = bounded_owned_run(&args, &root);
        assert_eq!(
            result.status.code(),
            Some(code),
            "{label}: {}",
            stderr(&result)
        );
        assert!(
            stderr(&result).contains(expected),
            "{label}: {}",
            stderr(&result)
        );
        assert!(output.join(".rawtl-partial").is_dir());
        assert!(!output.join("output.mp4").exists());
        assert!(
            !output.join("logs/encode/frame_000003.log").exists(),
            "stop dispatch on failure"
        );
        let first = fs::read_to_string(output.join("logs/encode/frame_000001.log")).unwrap();
        assert!(
            first.contains("exit:"),
            "native status must be retained: {first}"
        );
        if output.join("logs/encode.log").exists() {
            let log = fs::read_to_string(output.join("logs/encode.log")).unwrap();
            assert!(log.contains("exit:"), "encoder status must be retained");
            assert!(
                log.contains("pid: "),
                "live counterpart fixture must start both processes"
            );
        }
    }
}

const REV_PROFILE: &str = "canon-log2-cinema-gamut-rev-eng-crm-dev";
const REV3_PROFILE: &str = "canon-log3-cinema-gamut-rev-eng-crm-dev";

#[test]
fn rev_eng_profile_streams_f32_through_every_pass_and_audits_the_actual_video() {
    for id in [REV_PROFILE, REV3_PROFILE] {
        for wb in ["first-frame", "per-frame"] {
            let frames = (0..28)
                .map(|i| {
                    let mut f = FakeFrame::new(
                        &format!("IMG_{i:04}.CR3"),
                        0.18f64.log2() + if i % 2 == 0 { 0.2 } else { -0.2 },
                    );
                    f.own_neutral = if i % 2 == 0 { "0.5,1,0.8" } else { "0.6,1,0.7" }.into();
                    f
                })
                .collect();
            let material = synthetic_material(&format!("rev-eng-{id}-{wb}"), frames);
            let (result, output) = run_synthetic(
                &material,
                "run",
                &[
                    "--preview-profile",
                    id,
                    "--log-reference-gain",
                    "2",
                    "--white-balance",
                    wb,
                ],
            );
            assert!(
                result.status.success(),
                "{}\n{}",
                stdout(&result),
                stderr(&result)
            );
            assert!(output.join("output.mp4").exists());
            assert!(!output.join("frames").exists());
            let expected_tags = ColorCodes {
                primaries: 2,
                transfer: 2,
                matrix: 9,
                full: 1,
            };
            assert_eq!(mp4_nclx(&output.join("output.mp4")), expected_tags);
            assert_eq!(h264_sps_vui(&output.join("output.mp4")), expected_tags);
            let manifest = read_json(&output.join("manifest.json"));
            let contract = manifest
                .get("video")
                .unwrap()
                .get("color_contract")
                .unwrap();
            assert_eq!(contract.get("profile").and_then(Json::as_str), Some(id));
            assert_eq!(
                contract.get("display_name").and_then(Json::as_str),
                Some(if id == REV_PROFILE {
                    "clog2(rev-eng crm-dev)"
                } else {
                    "clog3 (rev-eng crm-dev)"
                })
            );
            let q = manifest.get("quantization_policy").unwrap();
            assert_eq!(q.get("reference_gain").and_then(Json::as_f64), Some(2.));
            let audit = fs::read_to_string(output.join("temporal_audit.json")).unwrap();
            assert!(audit.contains(id), "{audit}");
            assert!(audit.contains("reference_gain"), "{audit}");
            // Analysis, gain probe, application feedback, final encoding all carry
            // and validate the same camera strategy. No RGB48 fallback is allowed.
            for (path, command) in frame_commands(&output.join("logs")) {
                assert!(
                    command.contains("--camera-color-method dng-camera-to-pcs"),
                    "{command}"
                );
                let log = fs::read_to_string(&path).unwrap();
                if path.to_string_lossy().contains("/encode/") {
                    assert!(command.contains("--stdout-frame-f32"), "{command}");
                    assert!(log.contains("pixel bytes: 518400"));
                } else {
                    assert!(command.contains("--stdout-analysis"), "{command}");
                }
                let m = metadata_from_log(&log);
                assert_eq!(
                    m.get("color")
                        .unwrap()
                        .get("method_requested")
                        .and_then(Json::as_str),
                    Some("dng-camera-to-pcs")
                );
            }
        }
    }
}

#[test]
fn all_profiles_reject_corrupt_f32_contracts_and_canon_keeps_its_constraints() {
    for profile in &rawtl_linear_video::color_profile::PROFILES {
        for (knob, expected) in [
            ("rev_defect=version", "magic"),
            ("rev_defect=method", "camera color contract"),
            ("rev_defect=effective", "camera color contract"),
            ("rev_defect=missing-contract", "color contract"),
            ("rev_defect=non-canon", "camera color contract"),
            ("rev_defect=mixed-model", "mixed camera models"),
            ("rev_defect=format", "pixel_format"),
            ("rev_defect=hash", "pixels_sha256 mismatch"),
            ("empty_source_sha256=1", "source_sha256"),
            ("rev_defect=nan", "nonfinite FP32"),
            (
                "rev_defect=old-calibration",
                "incomplete camera profile calibration",
            ),
            (
                "rev_defect=skipped-hsm",
                "incomplete camera profile calibration",
            ),
            (
                "rev_defect=skipped-fixed",
                "incomplete camera profile calibration",
            ),
            (
                "rev_defect=auto-tone",
                "incomplete camera profile calibration",
            ),
            (
                "rev_defect=double-scale",
                "incomplete camera profile calibration",
            ),
            ("rev_defect=infinity", "nonfinite FP32"),
            ("short_pixels=1", "short record payload"),
            ("descriptor_geometry_mismatch=1", "descriptor geometry"),
            ("nonzero_after_records=1", "exit code 3"),
        ] {
            if !profile.is_rev_eng()
                && [
                    "rev_defect=non-canon",
                    "rev_defect=mixed-model",
                    "rev_defect=old-calibration",
                    "rev_defect=skipped-hsm",
                    "rev_defect=skipped-fixed",
                    "rev_defect=auto-tone",
                    "rev_defect=double-scale",
                ]
                .contains(&knob)
            {
                continue;
            }
            let root = test_dir(&format!("float-contract-{}-{knob}", profile.id));
            let input = root.join("material");
            make_input(&input, &["IMG_1.CR3", "IMG_2.CR3"]);
            let fixtures = root.join("fixtures");
            fs::create_dir_all(&fixtures).unwrap();
            let frames = vec![
                FakeFrame::new("IMG_1.CR3", 0.18f64.log2()),
                FakeFrame::new("IMG_2.CR3", 0.18f64.log2()),
            ];
            let tool = fake_native_sequence(&fixtures, &frames, None, false, 240, 180);
            use std::io::Write;
            let config = fixtures.join(format!("fake-native-{}.config", std::process::id()));
            writeln!(
                fs::OpenOptions::new().append(true).open(config).unwrap(),
                "{knob}"
            )
            .unwrap();
            let output = root.join("run");
            let mut args = base_args(&input, &output, &tool);
            args.extend(["--strength", "0", "--preview-profile", profile.id]);
            let result = run_cli(&args);
            assert_eq!(
                result.status.code(),
                Some(30),
                "{knob}: {}",
                stderr(&result)
            );
            assert!(
                stderr(&result).contains(expected),
                "{knob}: {}",
                stderr(&result)
            );
            assert!(!output.join("output.mp4").exists());
        }
    }
}
#[test]
fn log_reference_gain_is_finite_positive_and_exclusive_to_new_profile() {
    for id in [REV_PROFILE, REV3_PROFILE] {
        for gain in ["0", "-1", "NaN", "inf", "1e999", "oops"] {
            let result = run_cli(&[
                "--input",
                "/tmp",
                "--output",
                "/tmp/unused",
                "--preview-profile",
                id,
                "--log-reference-gain",
                gain,
            ]);
            assert_eq!(result.status.code(), Some(2), "{}", stderr(&result));
        }
        for profile in ["canon-log2-cinema-gamut", "srgb-preview"] {
            let result = run_cli(&[
                "--input",
                "/tmp",
                "--output",
                "/tmp/unused",
                "--preview-profile",
                profile,
                "--log-reference-gain",
                "1",
            ]);
            assert_eq!(result.status.code(), Some(2), "{}", stderr(&result));
        }
        let (root, input, fixtures) = run_success_setup(&format!("rev-reject-dng-{id}"));
        let tool = fake_native_tool(&fixtures, &standard_frames(&fixtures), None, false);
        let output = root.join("run");
        let mut args = base_args(&input, &output, &tool);
        args.extend(["--strength", "0", "--preview-profile", id]);
        let result = run_cli(&args);
        assert_eq!(result.status.code(), Some(30), "{}", stderr(&result));
        assert!(stderr(&result).contains("requires Canon CR3"));
        assert!(!output.join("output.mp4").exists());
    }
}
