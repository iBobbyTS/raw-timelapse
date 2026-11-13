//! Product CLI acceptance: one video file, one sibling run log, a run-specific
//! scratch directory, and the failure/degraded/retry contracts.
//!
//! Every test that needs real encoding calls the FFmpeg 8 binary directly.
//! Synthetic frames cross the fake native producer; one guarded journey uses
//! the staged Canon R7 CR3 material with the real native producer.

mod common;

use common::*;
use rawtl_linear_video::json::Json;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Output;

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
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

fn run_directory(root: &Path, name: &str) -> PathBuf {
    root.join(name)
}

/// The historical three-frame fixture with vendor RAW names, so the prepared
/// cache is run-generated and therefore deletable.
fn cr3_standard_frames(fixtures: &Path) -> Vec<FakeFrame> {
    standard_frames(fixtures)
        .into_iter()
        .map(|mut frame| {
            frame.name = frame.name.replace(".DNG", ".CR3");
            frame
        })
        .collect()
}

fn append_config(fixtures: &Path, line: &str) {
    let config = fixtures.join(format!("fake-native-{}.config", std::process::id()));
    writeln!(
        fs::OpenOptions::new().append(true).open(&config).expect("config"),
        "{line}"
    )
    .expect("append config");
}

// ---------------------------------------------------------------------------
// Core surface
// ---------------------------------------------------------------------------

#[test]
fn publishes_one_video_and_one_run_log_with_a_clean_scratch() {
    require_ffmpeg();
    let root = test_dir("surface");
    let input = root.join("material");
    make_input(&input, &["IMG_10.CR3", "IMG_1.CR3", "IMG_2.CR3"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let frames = cr3_standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let directory = run_directory(&root, "run");
    fs::create_dir_all(&directory).expect("create run dir");
    fs::write(directory.join("notes.txt"), b"user file").expect("pre-existing file");

    let video = video_in(&directory);
    let result = run_cli(&base_args(&input, &video, &tool));
    assert!(
        result.status.success(),
        "{}\n{}",
        stdout(&result),
        stderr(&result)
    );
    assert_only_video_and_log(&directory, VIDEO_NAME, &["notes.txt"]);
    let log = read_log(&directory);

    // The video is the configured ProRes 4444 at the source geometry.
    let probe = ffprobe(&video_in(&directory));
    for expected in [
        "codec_name=prores",
        "profile=4444",
        "pix_fmt=yuv444p12le",
        "width=8",
        "height=6",
        "nb_read_frames=3",
        "avg_frame_rate=30/1",
    ] {
        assert!(probe.contains(expected), "{probe}");
    }

    // The log records the run contract in one place.
    for expected in [
        "kind=run.start",
        "kind=settings fps=30/1",
        "kind=graph source=\"built-in\"",
        "kind=encoder encoder=prores_ks",
        "kind=colour profile=srgb-preview",
        "kind=prepare frame=1",
        "kind=measure frame=1",
        "kind=develop frame=1",
        "kind=delete frame=1",
        "kind=timing preflight=",
        "kind=publish",
        "kind=run.complete",
    ] {
        assert!(log.contains(expected), "missing {expected} in:\n{log}");
    }
    // Node order: develop completes, the cache is deleted, then the frame is
    // pushed to the encoder.
    let develop_at = log.find("kind=develop frame=1").expect("develop");
    let delete_at = log.find("kind=delete frame=1").expect("delete");
    let encode_at = log
        .find("kind=node node=encode frame=1")
        .expect("encode node");
    assert!(develop_at < delete_at && delete_at < encode_at, "{log}");
    // Every declared node reports a completion event.
    for node in [
        "source",
        "convert-cache",
        "measure-solve",
        "apply-ev",
        "color",
        "encode",
        "delete-cache",
    ] {
        assert!(
            log.contains(&format!("kind=node node={node} ")),
            "missing node {node}"
        );
    }
    // Converter runs once per source; only one full-resolution develop each.
    assert_eq!(log_kind(&log, "prepare").len(), 3);
    assert_eq!(log_kind(&log, "measure").len(), 3);
    assert_eq!(log_kind(&log, "develop").len(), 3);
    assert_eq!(log_kind(&log, "delete").len(), 3);

    // The scratch directory is run-specific and gone after success.
    let scratch = scratch_from_log(&log);
    assert!(!scratch.exists(), "scratch {} survived", scratch.display());
    assert!(
        !fs::read_dir(&directory)
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with(".rawtl-")),
        "no scratch directory may remain"
    );

    // Nothing else from the retired report workflow exists.
    for retired in [
        "manifest.json",
        "exposure_report.json",
        "exposure-curve.csv",
        "pairs.csv",
        "frames.csv",
        "temporal_audit.json",
        "logs",
    ] {
        assert!(!directory.join(retired).exists(), "{retired} must not exist");
    }
}

#[test]
fn refuses_to_overwrite_an_existing_video_or_log() {
    require_ffmpeg();
    let root = test_dir("retry");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let directory = run_directory(&root, "run");
    let video = video_in(&directory);

    let first = run_cli(&base_args(&input, &video, &tool));
    assert!(first.status.success(), "{}", stderr(&first));
    let log_before = read_log(&directory);
    let video_bytes = fs::read(&video).expect("video");

    // A retry with the same name never overwrites the published video or log.
    let second = run_cli(&base_args(&input, &video, &tool));
    assert_eq!(second.status.code(), Some(12), "{}", stderr(&second));
    assert!(stderr(&second).contains("refusing to overwrite"), "{}", stderr(&second));
    assert_eq!(fs::read(&video).expect("video"), video_bytes);
    assert_eq!(read_log(&directory), log_before);

    // Removing only the video is still not an overwrite invitation.
    fs::remove_file(&video).expect("remove video");
    let third = run_cli(&base_args(&input, &video, &tool));
    assert_eq!(third.status.code(), Some(12), "{}", stderr(&third));
    assert!(stderr(&third).contains("timelapse.log"), "{}", stderr(&third));

    // A stale scratch from an earlier run is never picked up: a fresh name uses
    // a new, empty scratch directory and leaves the old one untouched.
    let stale = directory.join(".rawtl-stale-run");
    fs::create_dir_all(stale.join("cache/frame_000001")).expect("stale scratch");
    fs::write(
        stale.join("cache/frame_000001/prepared.context"),
        b"stale cache",
    )
    .expect("stale cache");
    let fresh_directory = run_directory(&root, "fresh");
    let fresh = fresh_directory.join(VIDEO_NAME);
    let fourth = run_cli(&base_args(&input, &fresh, &tool));
    assert!(fourth.status.success(), "{}", stderr(&fourth));
    let fresh_log = read_log(&fresh_directory);
    let fresh_scratch = scratch_from_log(&fresh_log);
    assert_ne!(fresh_scratch, stale);
    assert_ne!(fresh_scratch, scratch_from_log(&log_before));
    assert_eq!(
        fs::read(stale.join("cache/frame_000001/prepared.context")).unwrap(),
        b"stale cache",
        "an earlier scratch is never reused or cleaned by a new run"
    );
}

#[test]
fn refuses_a_preexisting_dangling_video_symlink_and_preserves_it() {
    require_ffmpeg();
    let root = test_dir("dangling-video-link");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let directory = run_directory(&root, "run");
    fs::create_dir_all(&directory).expect("create run dir");
    let video = video_in(&directory);
    let link_target = PathBuf::from("missing-user-target.mov");
    std::os::unix::fs::symlink(&link_target, &video).expect("dangling video link");
    assert!(!video.exists(), "the fixture link must dangle");

    let result = run_cli(&base_args(&input, &video, &tool));
    assert_eq!(result.status.code(), Some(12), "{}", stderr(&result));
    assert!(
        stderr(&result).contains("refusing to overwrite"),
        "{}",
        stderr(&result)
    );

    // The user entry is preserved exactly: still a symlink to the same target.
    let metadata = fs::symlink_metadata(&video).expect("link metadata");
    assert!(metadata.file_type().is_symlink(), "the link was replaced");
    assert_eq!(fs::read_link(&video).expect("link target"), link_target);
    assert!(!video.exists(), "the link must still dangle");

    // Refusal happens before any log, scratch or RAW work.
    let mut entries: Vec<String> = fs::read_dir(&directory)
        .expect("read run dir")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(entries, vec![VIDEO_NAME.to_string()], "only the user link remains");
    assert!(!log_in(&directory).exists());
    assert!(
        !fs::read_dir(&directory)
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with(".rawtl-")),
        "no scratch may be created for a refused output"
    );
}

#[test]
fn old_run_directory_output_is_rejected_with_a_migration_message() {
    let root = test_dir("migration");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG"]);
    let directory = root.join("run");
    fs::create_dir_all(&directory).expect("create old output dir");
    let tool = root.join("unused-native-tool");

    let result = run_cli(&[
        "--input",
        input.to_str().unwrap(),
        "--output",
        directory.to_str().unwrap(),
        "--dng-linear-frame",
        tool.to_str().unwrap(),
    ]);
    assert_eq!(result.status.code(), Some(2), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(text.contains("names a directory"), "{text}");
    assert!(text.contains("timelapse.mov"), "{text}");
    assert!(
        fs::read_dir(&directory).unwrap().next().is_none(),
        "a rejected output must stay empty"
    );
}

#[test]
fn removed_quality_workflow_flags_are_rejected_explicitly() {
    require_ffmpeg();
    let root = test_dir("quality-flags");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG"]);
    let tool = root.join("unused-native-tool");
    let video = root.join("run").join(VIDEO_NAME);
    for (flag, marker) in [
        ("--temporal-quality", "temporal-quality"),
        ("--temporal-quality=off", "temporal-quality"),
        ("--on-review=error", "on-review"),
        ("--save-frames", "save-frames"),
        ("--overwrite", "overwrite"),
    ] {
        let mut args = base_args(&input, &video, &tool);
        args.push(flag);
        let result = run_cli(&args);
        assert_eq!(result.status.code(), Some(2), "{flag}: {}", stderr(&result));
        assert!(
            stderr(&result).contains(marker),
            "{flag}: {}",
            stderr(&result)
        );
        assert!(!video.exists(), "{flag} must not create a video");
        assert!(!root.join("run").exists(), "{flag} must not create the directory");
    }
}

#[test]
fn container_extension_must_match_the_configured_container() {
    require_ffmpeg();
    let root = test_dir("container-extension");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);

    let mp4 = root.join("run").join(VIDEO_MP4_NAME);
    let result = run_cli(&base_args(&input, &mp4, &tool));
    assert_eq!(result.status.code(), Some(2), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(text.contains(".mp4") && text.contains("mov"), "{text}");
    assert!(!root.join("run").exists(), "no artifact for a config error");

    // Naming the matching container makes the same file acceptable.
    let mkv = root.join("run").join(VIDEO_MKV_NAME);
    let mut args = base_args(&input, &mkv, &tool);
    args.extend(["--encoder", "ffv1", "--strength", "0"]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    assert!(ffprobe_color(&mkv).contains("codec_name=ffv1"));
}

#[test]
fn no_quality_verdict_or_audit_content_reaches_the_product() {
    require_ffmpeg();
    let root = test_dir("no-quality");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let directory = run_directory(&root, "run");
    let video = video_in(&directory);
    let result = run_cli(&base_args(&input, &video, &tool));
    assert!(result.status.success(), "{}", stderr(&result));

    let log = read_log(&directory);
    for product_text in [&log, &stdout(&result), &stderr(&result)] {
        for banned in [
            "REVIEW_REQUIRED",
            "COMPLETED_WITH_REVIEW",
            "GLOBAL_REFERENCE",
            "plan_quality",
            "temporal_quality",
            "PASS",
            "FAIL",
            "质量评分",
        ] {
            assert!(
                !product_text.contains(banned),
                "product output must not contain {banned}:\n{product_text}"
            );
        }
    }
    // No per-frame metadata document, matrix or pixel payload.
    for line in log.lines() {
        assert!(line.len() <= 1400, "unbounded log line: {line}");
        assert!(!line.contains("[[") && !line.contains("\"matrix\""), "{line}");
    }
}

// ---------------------------------------------------------------------------
// Failure and stop behaviour
// ---------------------------------------------------------------------------

#[test]
fn stops_at_the_first_failing_frame_without_video() {
    let root = test_dir("fail-fast");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, Some("IMG_2.DNG"), false);
    let directory = run_directory(&root, "run");

    let video = video_in(&directory);
    let result = run_cli(&base_args(&input, &video, &tool));
    assert_eq!(result.status.code(), Some(30), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(text.contains("frame 2/3"), "{text}");
    assert!(text.contains("IMG_2.DNG"), "{text}");
    assert!(text.contains("synthetic frame failure"), "{text}");
    assert!(text.contains("retained"), "{text}");
    assert!(!video_in(&directory).exists());

    let log = read_log(&directory);
    assert!(log.contains("kind=run.error code=30"), "{log}");
    assert!(log.contains("synthetic frame failure"), "{log}");
    assert!(log.contains("retained=true"), "{log}");
    let scratch = scratch_from_log(&log);
    assert!(scratch.is_dir(), "failure keeps the scratch for diagnosis");
    assert!(scratch.join("cache").is_dir());
    let transcripts = scratch.join("transcripts/prepare");
    assert!(
        fs::read_dir(&transcripts).unwrap().next().is_some(),
        "converter transcripts are retained in scratch"
    );
    // No later frame was developed.
    assert!(!log.contains("kind=develop frame=3"), "{log}");
}

#[test]
fn encoder_failure_publishes_nothing_and_records_bounded_stderr() {
    let root = test_dir("encoder-failure");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let encoder = fake_failing_encoder(&fixtures);
    let directory = run_directory(&root, "run");

    let video = video_in(&directory);
    let mut args = base_args(&input, &video, &tool);
    args.extend(["--ffmpeg", encoder.to_str().expect("utf8")]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(60), "{}", stderr(&result));
    assert!(
        stderr(&result).contains("partial/intermediate artifacts retained"),
        "{}",
        stderr(&result)
    );
    assert!(!video_in(&directory).exists());
    let log = read_log(&directory);
    assert!(log.contains("kind=run.error code=60"), "{log}");
    assert!(
        log.contains("synthetic encoder failure after write"),
        "the encoder stderr tail enters the single log:\n{log}"
    );
    let scratch = scratch_from_log(&log);
    assert!(
        scratch.join(VIDEO_NAME).is_file(),
        "the partial output stays in scratch"
    );
}

#[test]
fn probe_rejection_publishes_nothing() {
    let root = test_dir("probe-rejection");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let ffprobe = fake_ffprobe_wrong_codec(&fixtures);
    let directory = run_directory(&root, "run");

    let video = video_in(&directory);
    let mut args = base_args(&input, &video, &tool);
    args.extend(["--ffprobe", ffprobe.to_str().expect("utf8")]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(60), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(text.contains("codec is \"ffv1\""), "{text}");
    assert!(text.contains("expected \"prores\""), "{text}");
    assert!(!video_in(&directory).exists());
    let log = read_log(&directory);
    assert!(log.contains("kind=run.error code=60"), "{log}");
}

#[test]
fn rejects_output_inside_the_material_directory_without_writes() {
    let root = test_dir("overlap");
    let material = root.join("material");
    make_input(&material, &["IMG_1.DNG"]);
    let tool = root.join("unused-native-tool");
    let before: Vec<PathBuf> = fs::read_dir(&material)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .collect();

    let result = run_cli(&base_args(&material, &material.join(VIDEO_NAME), &tool));
    assert_eq!(result.status.code(), Some(11), "{}", stderr(&result));
    assert!(
        stderr(&result).contains("inside the input/material directory"),
        "{}",
        stderr(&result)
    );
    let after: Vec<PathBuf> = fs::read_dir(&material)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(before, after, "a rejected output writes nothing");
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
        append_config(&fixtures, knob);
        append_config(&fixtures, "width=240");
        append_config(&fixtures, "height=180");
        let encoder = fixtures.join("controlled-encoder");
        let body = if encoder_exit {
            "exit 7"
        } else {
            "exec /usr/bin/python3 -c 'import sys,time; sys.stdin.buffer.read(259200); time.sleep(60)'"
        };
        // The capability preflight is a synthetic lavfi frame; only the real
        // streaming invocation stalls or fails.
        write_executable(
            &encoder,
            &format!(
                "#!/bin/sh\necho $$ >> \"$FAKE_NATIVE_PIDS\"\ncase \" $* \" in *\" lavfi \"*|*\" -h \"*) exit 0;; esac\n{body}\n"
            ),
        );
        let directory = run_directory(&root, "run");
        let video = video_in(&directory);
        let mut args = base_args(&input, &video, &native);
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
        assert!(!video_in(&directory).exists());

        let log = read_log(&directory);
        let scratch = scratch_from_log(&log);
        assert!(scratch.is_dir(), "{label}: scratch retained");
        let mut pids = fs::read_to_string(root.join("owned.pids")).unwrap_or_default();
        fn collect_transcripts(dir: &Path, out: &mut String) {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        collect_transcripts(&entry.path(), out);
                    } else if let Ok(text) = fs::read_to_string(entry.path()) {
                        for line in text.lines() {
                            if let Some(pid) = line.strip_prefix("pid: ") {
                                out.push_str(pid);
                                out.push('\n');
                            }
                        }
                    }
                }
            }
        }
        collect_transcripts(&scratch, &mut pids);
        assert!(
            !pids.trim().is_empty(),
            "{label}: the fixture must instrument owned children"
        );
        let mut survivors = vec![];
        for pid in pids.lines().filter(|pid| !pid.trim().is_empty()) {
            assert!(pid.parse::<u32>().unwrap() > 1);
            let alive = std::process::Command::new("kill")
                .args(["-0", pid])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success();
            if alive {
                survivors.push(pid.to_string());
                let _ = std::process::Command::new("kill")
                    .args(["-9", pid])
                    .status();
            }
        }
        assert!(
            survivors.is_empty(),
            "{label}: owned children survived CLI return: {survivors:?}"
        );
    }
}

fn bounded_owned_run(args: &[&str], root: &Path) -> Output {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let out = root.join("bounded.stdout");
    let err = root.join("bounded.stderr");
    let mut child = Command::new(env!("CARGO_BIN_EXE_rawtl-linear-video"))
        .args(args)
        .env("FAKE_NATIVE_PIDS", root.join("owned.pids"))
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
    assert!(
        in_time,
        "pipeline exceeded bounded completion deadline; stdout {} stderr {}",
        out.display(),
        err.display()
    );
    Output {
        status,
        stdout: fs::read(out).unwrap(),
        stderr: fs::read(err).unwrap(),
    }
}

// ---------------------------------------------------------------------------
// Deflicker plan behaviour through the product surface
// ---------------------------------------------------------------------------

const SYNTH_WIDTH: u32 = 240;
const SYNTH_HEIGHT: u32 = 180;

struct SyntheticMaterial {
    root: PathBuf,
    input: PathBuf,
    fixtures: PathBuf,
    tool: PathBuf,
}

fn synthetic_material(name: &str, frames: Vec<FakeFrame>) -> SyntheticMaterial {
    require_ffmpeg();
    let root = test_dir(name);
    let input = root.join("material");
    let names: Vec<&str> = frames.iter().map(|frame| frame.name.as_str()).collect();
    make_input(&input, &names);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let tool = fake_native_sequence(&fixtures, &frames, None, false, SYNTH_WIDTH, SYNTH_HEIGHT);
    SyntheticMaterial {
        root,
        input,
        fixtures,
        tool,
    }
}

fn run_synthetic(
    material: &SyntheticMaterial,
    output_name: &str,
    extra: &[&str],
) -> (Output, PathBuf) {
    let directory = material.root.join(output_name);
    let video = directory.join(VIDEO_NAME);
    let mut args = base_args(&material.input, &video, &material.tool);
    args.extend(extra);
    (run_cli(&args), directory)
}

#[test]
fn pairwise_uniform_corrects_flicker_with_one_measure_and_render() {
    let gains: Vec<f64> = (0..30)
        .map(|index| if index % 2 == 0 { -0.3 } else { 0.3 })
        .collect();
    let material = synthetic_material("flicker", synthetic_sequence(&gains));
    let (result, directory) = run_synthetic(&material, "run", &["--brightness", "uniform"]);
    assert!(
        result.status.success(),
        "{}\n{}",
        stdout(&result),
        stderr(&result)
    );
    let log = read_log(&directory);
    let evs = develop_evs(&log);
    assert_eq!(evs.len(), 30);
    let corrected: Vec<f64> = gains.iter().zip(&evs).map(|(g, ev)| g + ev).collect();
    assert!(
        corrected.windows(2).all(|w| (w[1] - w[0]).abs() < 0.1),
        "{corrected:?}"
    );
    assert_eq!(log_kind(&log, "measure").len(), 30);
    assert_eq!(log_kind(&log, "develop").len(), 30);
    assert_only_video_and_log(&directory, VIDEO_NAME, &[]);
    assert!(!scratch_from_log(&log).exists());
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
    let (result, directory) = run_synthetic(&material, "run", &["--step", "10:0.5"]);
    assert!(
        result.status.success(),
        "{}\n{}",
        stdout(&result),
        stderr(&result)
    );
    let log = read_log(&directory);
    let evs = develop_evs(&log);
    assert_eq!(evs.len(), 20);
    let mean = |range: std::ops::Range<usize>| {
        let values = &evs[range];
        values.iter().sum::<f64>() / values.len() as f64
    };
    assert!(
        (mean(10..20) - mean(0..10)).abs() < 0.1,
        "the estimator must not cancel the confirmed step: before={} after={}",
        mean(0..10),
        mean(10..20)
    );
    assert!(video_in(&directory).is_file());
}

#[test]
fn ev_limits_clamp_every_applied_ev() {
    let gains: Vec<f64> = (0..12)
        .map(|index| if index % 2 == 0 { 1.0 } else { -1.0 })
        .collect();
    let material = synthetic_material("ev-limits", synthetic_sequence(&gains));
    let (result, directory) = run_synthetic(&material, "run", &["--ev-limits", "-0.25,0.25"]);
    assert!(
        result.status.success(),
        "{}\n{}",
        stdout(&result),
        stderr(&result)
    );
    let log = read_log(&directory);
    let evs = develop_evs(&log);
    assert_eq!(evs.len(), 12);
    assert!(
        evs.iter().all(|ev| (-0.25..=0.25).contains(ev)),
        "{evs:?}"
    );
    assert!(evs.iter().any(|ev| *ev == 0.25 || *ev == -0.25), "{evs:?}");
    assert_only_video_and_log(&directory, VIDEO_NAME, &[]);
}

#[test]
fn product_graph_does_not_add_full_frame_feedback_rounds() {
    let material = synthetic_material(
        "one-render",
        synthetic_sequence(&[0.3, -0.3, 0.3, -0.3, 0.3, -0.3]),
    );
    append_config(&material.fixtures, "gain_scale=0.5");
    let (result, directory) = run_synthetic(&material, "run", &[]);
    assert!(result.status.success(), "{}", stderr(&result));
    let log = read_log(&directory);
    assert_eq!(log_kind(&log, "measure").len(), 6);
    assert_eq!(log_kind(&log, "develop").len(), 6);
    assert_eq!(log_kind(&log, "prepare").len(), 6);
}

#[test]
fn degraded_photometry_and_single_frame_still_publish_finite_ev() {
    // A one-frame sequence has no pairs at all.
    let single = synthetic_material("single-frame", synthetic_sequence(&[0.2]));
    let (result, directory) = run_synthetic(&single, "run", &[]);
    assert!(result.status.success(), "{}", stderr(&result));
    let log = read_log(&directory);
    let evs = develop_evs(&log);
    assert_eq!(evs.len(), 1);
    assert!(evs[0].is_finite(), "{evs:?}");
    assert!(video_in(&directory).is_file());

    // Most tiles invalid: the estimator falls back to a finite plan.
    let mut frames = synthetic_sequence(&[0.2, -0.2, 0.2, -0.2]);
    frames[1].pattern = "sparse";
    let sparse = synthetic_material("best-effort-graph", frames);
    let (result, directory) = run_synthetic(&sparse, "run", &[]);
    assert!(result.status.success(), "{}", stderr(&result));
    let log = read_log(&directory);
    let evs = develop_evs(&log);
    assert_eq!(evs.len(), 4);
    assert!(evs.iter().all(|ev| ev.is_finite()), "{evs:?}");
    assert_eq!(log_kind(&log, "develop").len(), 4);
}

#[test]
fn fractional_ev_values_survive_the_sidecar_round_trip() {
    // The producer serializes the analysis sidecar with 10 significant digits,
    // so a fractional correction never round-trips through f64 exactly.
    let gains: Vec<f64> = (0..12).map(|index| 0.1 * index as f64).collect();
    let material = synthetic_material("fractional-ev", synthetic_sequence(&gains));
    let (result, directory) = run_synthetic(
        &material,
        "run",
        &["--estimator", "legacy", "--brightness", "uniform", "--strength", "0.7"],
    );
    assert!(
        result.status.success(),
        "{}\n{}",
        stdout(&result),
        stderr(&result)
    );
    let log = read_log(&directory);
    let evs = develop_evs(&log);
    assert_eq!(evs.len(), 12);
    assert!(evs.iter().all(|ev| ev.is_finite()), "{evs:?}");
    assert!(video_in(&directory).is_file());
}

#[test]
fn strength_zero_is_the_single_pass_regression_path() {
    let root = test_dir("strength-zero");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);

    let run = |name: &str| {
        let directory = run_directory(&root, name);
        let video = video_in(&directory);
        let mut args = base_args(&input, &video, &tool);
        args.extend(["--strength", "0"]);
        let result = run_cli(&args);
        assert!(
            result.status.success(),
            "run failed\nstdout:\n{}\nstderr:\n{}",
            stdout(&result),
            stderr(&result)
        );
        directory
    };
    let first = run("run-a");
    let second = run("run-b");

    let log = read_log(&first);
    assert!(log_kind(&log, "measure").is_empty(), "no analysis at strength 0");
    let evs = develop_evs(&log);
    assert_eq!(evs, vec![0.0, 0.0, 0.0]);
    // Frame 1 decodes with its own as-shot neutral; later frames carry the
    // frozen override (same three-valued field, different source).
    let wb: Vec<(String, String)> = log_kind(&log, "develop")
        .iter()
        .map(|line| {
            (
                log_field(line, "wb").unwrap().to_string(),
                log_field(line, "wb_source").unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(wb[0].1, "as-shot");
    assert_eq!(wb[1].1, "override");
    assert_eq!(wb[2].1, "override");
    assert_eq!(wb[0].0, wb[1].0);
    assert_eq!(wb[0].0, wb[2].0);

    // Both same-candidate runs traverse the pipe with the same encoder argv.
    let command = |directory: &Path| {
        let log = read_log(directory);
        let args = log_field(log_kind(&log, "encoder.command")[0], "args").unwrap();
        args.split(" -f mov ").next().unwrap().to_string()
    };
    assert_eq!(command(&first), command(&second));
    assert_only_video_and_log(&first, VIDEO_NAME, &[]);
}

#[test]
fn late_encoder_failure_on_disabled_path_reports_the_error() {
    let root = test_dir("disabled-encode-failure");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let encoder = fake_failing_encoder(&fixtures);
    let directory = run_directory(&root, "run");

    let video = video_in(&directory);
    let mut args = base_args(&input, &video, &tool);
    args.extend([
        "--strength",
        "0",
        "--ffmpeg",
        encoder.to_str().expect("utf8"),
    ]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(60), "{}", stderr(&result));
    assert!(!video_in(&directory).exists());
    let log = read_log(&directory);
    assert!(log.contains("kind=run.error code=60"), "{log}");
    assert!(log.contains("synthetic encoder failure"), "{log}");
}

// ---------------------------------------------------------------------------
// White balance modes
// ---------------------------------------------------------------------------

fn white_balance_triple(text: &str) -> [f64; 3] {
    let values: Vec<f64> = text
        .split(',')
        .map(|value| value.parse().expect("wb channel"))
        .collect();
    assert_eq!(values.len(), 3, "{text}");
    [values[0], values[1], values[2]]
}

fn develop_wb(log: &str) -> Vec<([f64; 3], [f64; 3], String)> {
    log_kind(log, "develop")
        .iter()
        .map(|line| {
            (
                white_balance_triple(log_field(line, "wb").unwrap()),
                white_balance_triple(log_field(line, "as_shot").unwrap()),
                log_field(line, "wb_source").unwrap().to_string(),
            )
        })
        .collect()
}

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
    fs::create_dir_all(&fixtures).expect("fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let directory = run_directory(&root, "run");

    let video = video_in(&directory);
    let mut args = base_args(&input, &video, &tool);
    args.extend(["--white-balance", "per-frame", "--strength", "0"]);
    let result = run_cli(&args);
    assert!(
        result.status.success(),
        "run failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&result),
        stderr(&result)
    );
    let log = read_log(&directory);
    let applied = develop_wb(&log);
    assert_eq!(applied.len(), 3);
    for (wb, as_shot, source) in &applied {
        assert_eq!(source, "as-shot");
        assert_eq!(wb, as_shot, "each frame applies its own neutral");
    }
    assert_ne!(applied[0].0, applied[1].0);
    assert_ne!(applied[1].0, applied[2].0);
    assert!(
        stdout(&result)
            .contains("per-frame white balance: every frame decoded with its own as-shot neutral"),
        "{}",
        stdout(&result)
    );
    assert!(!stdout(&result).contains("applied to every later frame"));
    assert!(video_in(&directory).is_file());
}

#[test]
fn per_frame_deflicker_measures_and_renders_without_an_override() {
    let material = synthetic_material("wb-per-frame-deflicker", per_frame_flicker_sequence(30));
    let (result, directory) = run_synthetic(
        &material,
        "run",
        &["--white-balance", "per-frame", "--brightness", "uniform"],
    );
    assert!(result.status.success(), "{}", stderr(&result));
    let log = read_log(&directory);
    let measured: Vec<(String, String)> = log_kind(&log, "measure")
        .iter()
        .map(|line| {
            (
                log_field(line, "frame").unwrap().to_string(),
                log_field(line, "source").unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(measured.len(), 30);
    let rendered = develop_wb(&log);
    assert_eq!(rendered.len(), 30);
    assert!(rendered.iter().all(|(wb, own, source)| {
        source == "as-shot" && wb == own
    }));
    assert!(video_in(&directory).is_file());
}

#[test]
fn per_frame_mode_rejects_a_producer_that_invents_an_override() {
    let root = test_dir("wb-per-frame-override-lie");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_sequence_wb_lie(
        &fixtures,
        &frames,
        FakeWbLie::OverrideWhenUnset,
        WIDTH,
        HEIGHT,
    );
    let directory = run_directory(&root, "run");
    let video = video_in(&directory);
    let mut args = base_args(&input, &video, &tool);
    args.extend(["--white-balance", "per-frame", "--strength", "0"]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(30), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(text.contains("should use its as-shot white balance"), "{text}");
    assert!(text.contains("frame 1/3"), "{text}");
    assert!(!video_in(&directory).exists());
}

#[test]
fn rejects_a_producer_that_reports_as_shot_but_applies_another_neutral() {
    let root = test_dir("wb-as-shot-lie");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let frames = standard_frames(&fixtures);
    let tool =
        fake_native_sequence_wb_lie(&fixtures, &frames, FakeWbLie::AsShotMismatch, WIDTH, HEIGHT);
    let directory = run_directory(&root, "run");
    let video = video_in(&directory);
    let mut args = base_args(&input, &video, &tool);
    args.extend(["--white-balance", "per-frame", "--strength", "0"]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(30), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(text.contains("reports as-shot white balance but applied"), "{text}");
    assert!(text.contains("as_shot_neutral"), "{text}");
    assert!(!video_in(&directory).exists());
}

#[test]
fn rejects_invalid_white_balance_mode_values() {
    let root = test_dir("wb-invalid-mode");
    let input = root.join("material");
    let tool = root.join("unused-native-tool");
    let video = root.join("run").join(VIDEO_NAME);
    for extra in [
        vec!["--white-balance", "bogus"],
        vec!["--white-balance", ""],
        vec!["--white-balance="],
    ] {
        let mut args = base_args(&input, &video, &tool);
        args.extend(extra.iter());
        let result = run_cli(&args);
        assert_eq!(
            result.status.code(),
            Some(2),
            "must reject {extra:?}: {}",
            stderr(&result)
        );
        assert!(stderr(&result).contains("--white-balance"), "{}", stderr(&result));
        assert!(!root.join("run").exists());
    }
}

// ---------------------------------------------------------------------------
// Typed handoff defects
// ---------------------------------------------------------------------------

fn metadata_from_transcript(text: &str) -> Json {
    let value = text
        .lines()
        .find_map(|line| line.strip_prefix("JSON 0x01: "))
        .expect("metadata JSON in transcript");
    rawtl_linear_video::json::parse(value).unwrap()
}

fn assert_final_source_hash_rejected(defect: &str) {
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
    append_config(&fixtures, &format!("frame_source_sha256_defect={defect}"));
    let directory = run_directory(&root, "run");
    let video = video_in(&directory);
    let result = run_cli(&base_args(&input, &video, &tool));

    assert_eq!(
        result.status.code(),
        Some(30),
        "{defect}: video published={}; stderr: {}",
        video_in(&directory).exists(),
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
    assert!(!video_in(&directory).exists());
    let log = read_log(&directory);
    assert!(log.contains("kind=run.error code=30"), "{log}");
    let scratch = scratch_from_log(&log);
    let transcript = fs::read_to_string(scratch.join("transcripts/develop/frame_000001.log"))
        .expect("final-frame transcript");
    metadata_from_transcript(&transcript);
    assert!(!log.contains("kind=develop frame=3"), "{log}");
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

fn run_sidecar_defect(name: &str, defects: FakeSidecarDefects) -> (Output, PathBuf) {
    let root = test_dir(name);
    let input = root.join("material");
    let frames = synthetic_sequence(&[0.0, 0.1]);
    let names: Vec<&str> = frames.iter().map(|frame| frame.name.as_str()).collect();
    make_input(&input, &names);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let tool = fake_native_sequence_sidecar_defect(&fixtures, &frames, defects, WIDTH, HEIGHT);
    let directory = run_directory(&root, "run");
    let video = video_in(&directory);
    (run_cli(&base_args(&input, &video, &tool)), directory)
}

#[test]
fn forged_metadata_source_hash_is_rejected_by_the_typed_boundary() {
    let (result, _) = run_sidecar_defect(
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
fn forged_analysis_sidecar_without_kind_is_rejected() {
    let (result, _) = run_sidecar_defect(
        "forged-analysis-kind",
        FakeSidecarDefects {
            analysis_omit_kind: true,
            ..FakeSidecarDefects::default()
        },
    );
    assert_eq!(result.status.code(), Some(30), "{}", stderr(&result));
    let text = stderr(&result);
    assert!(text.contains("frame 1/2"), "{text}");
    assert!(text.contains("analysis sidecar has an unsupported schema"), "{text}");
}

#[test]
fn forged_analysis_sidecar_hash_mismatch_is_rejected() {
    let (result, _) = run_sidecar_defect(
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

#[test]
fn detects_a_native_tool_that_ignores_the_frozen_override() {
    let root = test_dir("wb-drift");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, true);
    let directory = run_directory(&root, "run");
    let video = video_in(&directory);
    let result = run_cli(&base_args(&input, &video, &tool));
    assert_eq!(result.status.code(), Some(30), "{}", stderr(&result));
    assert!(
        stderr(&result).contains("should use the frozen override white balance"),
        "{}",
        stderr(&result)
    );
    assert!(!video_in(&directory).exists());
}

// ---------------------------------------------------------------------------
// Colour/Log profiles through the encoded stream
// ---------------------------------------------------------------------------

fn profile_material_for_extension(
    name: &str,
    rgb: [u16; 3],
    extension: &str,
) -> (PathBuf, PathBuf, PathBuf) {
    require_ffmpeg();
    let root = test_dir(name);
    let input = root.join("material");
    let names: Vec<String> = (0..3)
        .map(|index| format!("frame_{index:04}.{extension}"))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    make_input(&input, &refs);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
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

fn run_profile(root: &Path, input: &Path, tool: &Path, id: &str) -> (Output, PathBuf) {
    let directory = run_directory(root, "run");
    let video = video_in(&directory);
    let mut args = base_args(input, &video, tool);
    args.push("--preview-profile");
    args.push(id);
    (run_cli(&args), directory)
}

#[test]
fn preview_profiles_emit_their_declared_colour_contract() {
    let cases: [(&str, &str, bool, &str, &str); 6] = [
        ("srgb-preview", "bt709", false, "bt709", "iec.ch"),
        ("sony-slog3-sgamut3", "bt2020nc", true, "unknown", "sony.jp"),
        ("sony-slog3-sgamut3-cine", "bt2020nc", true, "unknown", "sony.jp"),
        ("canon-log2-cinema-gamut", "bt2020nc", true, "unknown", "canon.com"),
        ("nikon-nlog-bt2020", "bt2020nc", true, "bt2020", "nikon"),
        ("dji-dlog-dgamut", "bt2020nc", true, "unknown", "djicdn"),
    ];

    for (id, matrix, vendor, primaries, _source) in cases {
        let (root, input, tool) = profile_material_for_extension(
            &format!("profile-contract-{id}"),
            [0, 16384, 0],
            if id.contains("rev-eng") { "CR3" } else { "DNG" },
        );
        let (result, directory) = run_profile(&root, &input, &tool, id);
        assert!(
            result.status.success(),
            "{id} run failed\nstdout:\n{}\nstderr:\n{}",
            stdout(&result),
            stderr(&result)
        );
        let log = read_log(&directory);
        let colour = log_kind(&log, "colour");
        assert_eq!(colour.len(), 1, "{log}");
        assert!(colour[0].contains(&format!("profile={id}")), "{log}");
        assert!(colour[0].contains(&format!("matrix={matrix}")), "{log}");
        assert!(colour[0].contains("camera_color=legacy-inverse-cm"), "{log}");
        let encoder_command = log_kind(&log, "encoder.command");
        assert_eq!(encoder_command.len(), 1, "{log}");
        assert!(
            log_field(encoder_command[0], "args")
                .unwrap()
                .contains("-pix_fmt yuv444p10le"),
            "{log}"
        );

        let video = video_in(&directory);
        assert!(video.is_file(), "{id}: missing video");
        let probe = ffprobe_color(&video);
        for expected in [
            "codec_name=prores",
            "profile=4444",
            "pix_fmt=yuv444p12le",
            "width=64",
            "height=48",
            "nb_read_frames=3",
        ] {
            assert!(probe.contains(expected), "{id}: {probe}");
        }
        let encoder_line = log_kind(&log, "encoder");
        assert!(
            encoder_line[0].contains("range=tv") && encoder_line[0].contains("pix_fmt=yuv444p10le"),
            "{id}: {log}"
        );
        let transfer = if vendor { "unknown" } else { "iec61966-2-1" };
        for tag in [
            format!("color_space={matrix}"),
            format!("color_primaries={primaries}"),
            format!("color_transfer={transfer}"),
        ] {
            assert!(probe.contains(&tag), "{id}: {probe}");
        }
    }
}

#[test]
fn production_ycbcr_quantization_matches_the_standard_derivation() {
    use rawtl_linear_video::color_profile::{lookup, quantize_chroma, quantize_luma};
    let cases = [
        ("srgb-preview", 0.2126, 0.7152, 0.0722, 1.8556, 1.5748),
        (
            "canon-log3-cinema-gamut-rev-eng-crm-dev",
            0.2627,
            0.6780,
            0.0593,
            1.8814,
            1.4746,
        ),
        ("nikon-nlog-bt2020", 0.2627, 0.6780, 0.0593, 1.8814, 1.4746),
    ];
    for (id, kr, kg, kb, div_cb, div_cr) in cases {
        let profile = lookup(id).expect(id);
        for signal in [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.18, 0.18, 0.18],
            [0.4, 0.18, 0.09],
            [-0.05, 0.6, 1.2],
        ] {
            let y = kr * signal[0] + kg * signal[1] + kb * signal[2];
            let expected = [
                quantize_luma(y, profile.range),
                quantize_chroma((signal[2] - y) / div_cb, profile.range),
                quantize_chroma((signal[0] - y) / div_cr, profile.range),
            ];
            assert_eq!(
                profile.encode_signal_yuv10(signal, profile.range).unwrap(),
                expected,
                "{id} {signal:?}"
            );
        }
    }
}

#[test]
fn preview_profiles_map_a_known_patch_through_the_encoded_stream() {
    const VENDOR_TOLERANCE: i32 = 3;
    const SRGB_TOLERANCE: i32 = 12;
    let cases: [(&str, &str, &str, [u8; 3]); 6] = [
        ("srgb-preview", "bt709", "tv", [0, 255, 0]),
        ("sony-slog3-sgamut3", "bt2020", "tv", [59, 147, 20]),
        ("sony-slog3-sgamut3-cine", "bt2020", "tv", [0, 145, 49]),
        ("canon-log2-cinema-gamut", "bt2020", "tv", [48, 140, 96]),
        ("nikon-nlog-bt2020", "bt2020", "tv", [0, 154, 0]),
        ("dji-dlog-dgamut", "bt2020", "tv", [0, 145, 74]),
    ];

    let mut observed = Vec::new();
    for (id, matrix, range, expected) in cases {
        let (root, input, tool) =
            profile_material_for_extension(&format!("profile-patch-{id}"), [0, 16384, 0], "DNG");
        let (result, directory) = run_profile(&root, &input, &tool, id);
        assert!(
            result.status.success(),
            "{id} run failed\nstdout:\n{}\nstderr:\n{}",
            stdout(&result),
            stderr(&result)
        );
        let video = video_in(&directory);
        let decoded = decode_first_pixel(&video, matrix, range);
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
        assert!(
            accepts(decoded),
            "{id}: decoded {decoded:?} vs independent reference {expected:?}"
        );
        if id != "srgb-preview" {
            for (name, wrong_matrix, wrong_range) in [
                ("wrong-matrix", "bt709", range),
                ("wrong-range", matrix, "pc"),
            ] {
                let wrong = decode_first_pixel(&video, wrong_matrix, wrong_range);
                assert!(
                    !accepts(wrong),
                    "{id} {name} must fail the same patch bound {tolerance}: {wrong:?}"
                );
            }
        }
        observed.push((id, expected));
    }
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
                "{left_id} and {right_id} references are not distinguishable"
            );
        }
    }
}

#[test]
fn wrong_ycbcr_matrix_is_rejected_by_the_patch_check() {
    const CORRECT_TOLERANCE: i32 = 8;
    let (root, input, tool) = profile_material_for_extension(
        "profile-wrong-matrix",
        [57344, 0, 0],
        "DNG",
    );
    let (result, directory) = run_profile(&root, &input, &tool, "nikon-nlog-bt2020");
    assert!(
        result.status.success(),
        "run failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&result),
        stderr(&result)
    );
    let video = video_in(&directory);
    let expected = [202u8, 0, 0];
    let decoded = decode_first_pixel(&video, "bt2020", "tv");
    let difference = (0..3)
        .map(|channel| (decoded[channel] as i32 - expected[channel] as i32).abs())
        .max()
        .unwrap_or(0);
    assert!(difference <= CORRECT_TOLERANCE, "declared matrix: {decoded:?}");
    let wrong = decode_first_pixel(&video, "bt709", "tv");
    let wrong_difference = (0..3)
        .map(|channel| (wrong[channel] as i32 - expected[channel] as i32).abs())
        .max()
        .unwrap_or(0);
    assert!(
        wrong_difference > CORRECT_TOLERANCE,
        "decoding with the wrong matrix must be rejected: {wrong:?}"
    );
}

const REV_PROFILE: &str = "canon-log2-cinema-gamut-rev-eng-crm-dev";
const REV3_PROFILE: &str = "canon-log3-cinema-gamut-rev-eng-crm-dev";

#[test]
fn vendor_previews_auto_decode_from_their_own_tags() {
    for (id, expected) in [
        ("sony-slog3-sgamut3", [59u8, 147, 20]),
        ("sony-slog3-sgamut3-cine", [0, 145, 49]),
        ("canon-log2-cinema-gamut", [48, 140, 96]),
        (REV3_PROFILE, [39, 135, 77]),
        ("nikon-nlog-bt2020", [0, 154, 0]),
        ("dji-dlog-dgamut", [0, 145, 74]),
    ] {
        let (root, input, tool) = profile_material_for_extension(
            &format!("auto-tags-{id}"),
            [0, 16384, 0],
            if id == REV3_PROFILE { "CR3" } else { "DNG" },
        );
        let (result, directory) = run_profile(&root, &input, &tool, id);
        assert!(result.status.success(), "{id}: {}", stderr(&result));
        let video = video_in(&directory);
        let auto = decode_first_pixel_auto(&video);
        let correct = decode_first_pixel(&video, "bt2020", "tv");
        let wrong_matrix = decode_first_pixel(&video, "bt709", "tv");
        let wrong_range = decode_first_pixel(&video, "bt2020", "pc");
        let distance = |a: [u8; 3], b: [u8; 3]| {
            (0..3)
                .map(|c| (a[c] as i32 - b[c] as i32).abs())
                .max()
                .unwrap()
        };
        assert!(distance(auto, expected) <= 3, "{id} auto {auto:?}");
        assert!(distance(auto, correct) <= 1, "{id} auto vs forced");
        assert!(distance(auto, wrong_matrix) >= 4, "{id} wrong matrix");
        assert!(distance(auto, wrong_range) > 3, "{id} wrong range");
        assert!(distance(wrong_range, expected) > 3, "{id}: wrong range passes");
    }
}

#[test]
fn rejects_unknown_or_empty_preview_profile_with_exit_usage() {
    let root = test_dir("profile-unknown");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG"]);
    let tool = root.join("unused-native-tool");
    let video = root.join("run").join(VIDEO_NAME);
    for value in ["no-such-profile", ""] {
        let mut args = base_args(&input, &video, &tool);
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
        assert!(stderr(&result).contains("--preview-profile"), "{}", stderr(&result));
        assert!(!root.join("run").exists(), "usage rejection must not create output");
    }
}

#[test]
fn rev_eng_profile_streams_f32_through_graph_measure_and_render() {
    for id in [REV_PROFILE, REV3_PROFILE] {
        for wb in ["first-frame", "per-frame"] {
            let frames: Vec<FakeFrame> = (0..28)
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
            let (result, directory) = run_synthetic(
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
            let video = video_in(&directory);
            assert!(video.exists());
            let log = read_log(&directory);
            let colour = log_kind(&log, "colour");
            assert_eq!(colour.len(), 1);
            for expected in [
                format!("profile={id}"),
                "camera_color=dng-camera-to-pcs".to_string(),
                "reference_gain=2".to_string(),
            ] {
                assert!(colour[0].contains(&expected), "{log}");
            }
            let developments = log_kind(&log, "develop");
            assert_eq!(developments.len(), 28);
            for line in &developments {
                assert_eq!(log_field(line, "pixels"), Some("518400"), "{line}");
            }
            assert_eq!(log_kind(&log, "measure").len(), 28);
            let probe = ffprobe_color(&video);
            for tag in [
                "codec_name=prores",
                "profile=4444",
                "color_space=bt2020nc",
                "color_primaries=unknown",
                "color_transfer=unknown",
                "color_range=unknown",
            ] {
                assert!(probe.contains(tag), "{probe}");
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
            ("rev_defect=auto-tone", "incomplete camera profile calibration"),
            ("rev_defect=double-scale", "incomplete camera profile calibration"),
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
            append_config(&fixtures, knob);
            let directory = run_directory(&root, "run");
            let video = video_in(&directory);
            let mut args = base_args(&input, &video, &tool);
            args.extend(["--strength", "0", "--preview-profile", profile.id]);
            let result = run_cli(&args);
            assert_eq!(result.status.code(), Some(30), "{knob}: {}", stderr(&result));
            assert!(stderr(&result).contains(expected), "{knob}: {}", stderr(&result));
            assert!(!video_in(&directory).exists());
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
                "/tmp/unused.mov",
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
                "/tmp/unused.mov",
                "--preview-profile",
                profile,
                "--log-reference-gain",
                "1",
            ]);
            assert_eq!(result.status.code(), Some(2), "{}", stderr(&result));
        }
        let root = test_dir(&format!("rev-reject-dng-{id}"));
        let input = root.join("material");
        make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
        let fixtures = root.join("fixtures");
        fs::create_dir_all(&fixtures).expect("fixtures");
        let tool = fake_native_tool(&fixtures, &standard_frames(&fixtures), None, false);
        let directory = run_directory(&root, "run");
        let video = video_in(&directory);
        let mut args = base_args(&input, &video, &tool);
        args.extend(["--strength", "0", "--preview-profile", id]);
        let result = run_cli(&args);
        assert_eq!(result.status.code(), Some(30), "{}", stderr(&result));
        assert!(stderr(&result).contains("requires Canon CR3"));
        assert!(!video_in(&directory).exists());
    }
}

#[test]
fn rational_fps_is_honoured_by_the_encoder_and_the_log() {
    let root = test_dir("fps-rational");
    let input = root.join("material");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let frames = standard_frames(&fixtures);
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let directory = run_directory(&root, "run");
    let video = video_in(&directory);
    let mut args = base_args(&input, &video, &tool);
    args.extend(["--strength", "0", "--fps", "30000/1001"]);
    let result = run_cli(&args);
    assert!(result.status.success(), "{}", stderr(&result));
    let log = read_log(&directory);
    assert!(log.contains("kind=settings fps=30000/1001"), "{log}");
    let probe = ffprobe(&video_in(&directory));
    assert!(
        probe.contains("avg_frame_rate=30000/1001"),
        "{probe}"
    );
}

// ---------------------------------------------------------------------------
// Real material journey (staged, read-only; skipped when absent)
// ---------------------------------------------------------------------------

#[test]
fn real_cr3_small_sequence_delivers_one_video_and_one_log() {
    let material = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".agent-work/tmp/node-graph-product/material/cr3-boundary");
    let native = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("native/build/dng-linear-frame");
    let converter = "/Applications/Adobe DNG Converter.app/Contents/MacOS/Adobe DNG Converter";
    if !material.is_dir() || !native.is_file() || !Path::new(converter).is_file() {
        eprintln!(
            "skipping the real CR3 journey: material={} native={} converter={}",
            material.display(),
            native.display(),
            converter
        );
        return;
    }
    require_ffmpeg();
    let root = test_dir("real-cr3");
    let input = root.join("material");
    fs::create_dir_all(&input).unwrap();
    let mut staged: Vec<PathBuf> = fs::read_dir(&material)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("cr3"))
        })
        .collect();
    staged.sort();
    for source in staged.iter().take(3) {
        let name = source.file_name().unwrap();
        fs::copy(source, input.join(name)).expect("stage CR3");
    }
    let directory = run_directory(&root, "run");
    let video = video_in(&directory);
    let result = run_cli(&base_args(&input, &video, &native));
    assert!(
        result.status.success(),
        "{}\n{}",
        stdout(&result),
        stderr(&result)
    );
    assert_only_video_and_log(&directory, VIDEO_NAME, &[]);
    let log = read_log(&directory);
    assert_eq!(log_kind(&log, "prepare").len(), 3);
    assert_eq!(log_kind(&log, "develop").len(), 3);
    assert_eq!(log_kind(&log, "delete").len(), 3);
    let probe = ffprobe(&video_in(&directory));
    for expected in ["codec_name=prores", "profile=4444", "nb_read_frames=3"] {
        assert!(probe.contains(expected), "{probe}");
    }
    assert!(!scratch_from_log(&log).exists());
    for source in staged.iter().take(3) {
        assert_eq!(
            fs::read(material.join(source.file_name().unwrap())).unwrap(),
            fs::read(input.join(source.file_name().unwrap())).unwrap(),
            "the source material is read-only"
        );
    }
}
