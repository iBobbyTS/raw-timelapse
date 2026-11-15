//! D-Log 2 / D-Gamut 2 delivery checks: exact lossless readback, the default
//! ProRes patch budget, failure publication semantics and ordered clipping
//! statistics across worker concurrency.
//!
//! Every expected code is a frozen literal from the independent exact-arithmetic
//! oracle (`work/dlog2-delivery-impl/dlog2_oracle.py`, Decimal prec=80). The
//! oracle imports no production code; the product gains no Python dependency.
//! All frame material here is synthetic (fake native producer, read-only DNG
//! names); no real camera sample is processed.

mod common;

use common::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const PROFILE: &str = "dji-dlog2-dgamut2";

/// `(scene-linear AP1 fixture code, full-range handoff codes, limited-range
/// handoff codes)` for four flat patches. The 10-bit codes come from the oracle,
/// not from the production conversion.
const PATCHES: [([u16; 3], [u16; 3], [u16; 3]); 4] = [
    ([2949, 2949, 2949], [312, 511, 512], [331, 512, 512]),
    ([8192, 4096, 2048], [350, 474, 544], [364, 479, 541]),
    ([819, 6554, 14746], [365, 559, 444], [377, 554, 453]),
    ([14746, 2949, 328], [338, 392, 587], [353, 408, 578]),
];

/// Four pixel classes of the clip/concurrency fixture at reference gain 1000.
const CLIP_CLASSES: [([u16; 3], [u16; 3]); 4] = [
    ([2949, 2949, 2949], [935, 511, 512]),
    ([0, 16384, 0], [729, 445, 17]),
    ([16384, 0, 0], [811, 80, 655]),
    ([0, 0, 16384], [908, 573, 470]),
];
/// Neutral AP1 ramp: `(scene code, full-range codes, limited-range codes)`.
/// Frozen from the oracle (`gradient` section), spanning the toe and highlight
/// segments of the three-segment curve.
const GRADIENT: [([u16; 3], [u16; 3], [u16; 3]); 9] = [
    ([0, 0, 0], [64, 512, 512], [119, 512, 512]),
    ([2048, 2048, 2048], [280, 511, 512], [304, 512, 512]),
    ([4096, 4096, 4096], [341, 511, 512], [356, 512, 512]),
    ([6144, 6144, 6144], [377, 511, 512], [387, 512, 512]),
    ([8192, 8192, 8192], [403, 511, 512], [409, 512, 512]),
    ([10240, 10240, 10240], [423, 511, 512], [426, 512, 512]),
    ([12288, 12288, 12288], [439, 511, 512], [440, 512, 512]),
    ([14336, 14336, 14336], [453, 511, 512], [452, 512, 512]),
    ([16384, 16384, 16384], [465, 511, 512], [462, 512, 512]),
];
const GRADIENT_BAND: u32 = 32;
const GRADIENT_HEIGHT: u32 = 16;

const CLIP_WIDTH: u32 = 32;
const CLIP_HEIGHT: u32 = 8;
const CLIP_FRAMES: usize = 6;
/// Oracle counts: 128 low + 192 high channel samples per frame.
const CLIP_LOW: usize = 768;
const CLIP_HIGH: usize = 1152;
const CLIP_TOTAL: usize = 1920;
const CLIP_SAMPLES: usize = 4608;

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

/// One-frame material: horizontal patches of `patch_width` pixels.
fn patch_material(
    name: &str,
    patch_width: u32,
    height: u32,
) -> (PathBuf, PathBuf, PathBuf) {
    require_ffmpeg();
    let root = test_dir(name);
    let input = root.join("material");
    make_input(&input, &["frame_0000.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let width = patch_width * PATCHES.len() as u32;
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for _ in 0..height {
        for x in 0..width {
            pixels.push(PATCHES[(x / patch_width) as usize].0);
        }
    }
    let png = fixtures.join("patches.png");
    write_rgb48_png(&png, width, height, &pixels);
    let mut frame = FakeFrame::new("frame_0000.DNG", 0.0);
    frame.fixture = Some(png);
    let tool = fake_native_sequence(&fixtures, &[frame], None, false, width, height);
    (root, input, tool)
}

/// One-frame material: a neutral tonal ramp of nine flat bands.
fn gradient_material(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    require_ffmpeg();
    let root = test_dir(name);
    let input = root.join("material");
    make_input(&input, &["frame_0000.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let width = GRADIENT_BAND * GRADIENT.len() as u32;
    let mut pixels = Vec::with_capacity((width * GRADIENT_HEIGHT) as usize);
    for _ in 0..GRADIENT_HEIGHT {
        for x in 0..width {
            pixels.push(GRADIENT[(x / GRADIENT_BAND) as usize].0);
        }
    }
    let png = fixtures.join("gradient.png");
    write_rgb48_png(&png, width, GRADIENT_HEIGHT, &pixels);
    let mut frame = FakeFrame::new("frame_0000.DNG", 0.0);
    frame.fixture = Some(png);
    let tool = fake_native_sequence(&fixtures, &[frame], None, false, width, GRADIENT_HEIGHT);
    (root, input, tool)
}

/// The clip/concurrency fixture: `CLIP_FRAMES` identical frames whose four
/// pixel classes produce both low and high target-domain clips at gain 1000.
fn clip_material(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    require_ffmpeg();
    let root = test_dir(name);
    let names: Vec<String> = (0..CLIP_FRAMES).map(|i| format!("frame_{i:04}.DNG")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let input = root.join("material");
    make_input(&input, &refs);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let mut pixels = Vec::with_capacity((CLIP_WIDTH * CLIP_HEIGHT) as usize);
    for _ in 0..CLIP_HEIGHT {
        for x in 0..CLIP_WIDTH {
            pixels.push(CLIP_CLASSES[(x / 8) as usize].0);
        }
    }
    let png = fixtures.join("classes.png");
    write_rgb48_png(&png, CLIP_WIDTH, CLIP_HEIGHT, &pixels);
    let frames: Vec<FakeFrame> = names
        .iter()
        .map(|name| {
            let mut frame = FakeFrame::new(name, 0.0);
            frame.fixture = Some(png.clone());
            frame
        })
        .collect();
    let tool = fake_native_sequence(&fixtures, &frames, None, false, CLIP_WIDTH, CLIP_HEIGHT);
    (root, input, tool)
}

/// Decode the first frame as planar 10-bit 4:4:4 without any range/matrix
/// conversion, so the raw handoff codes are compared directly.
fn decode_planes(path: &Path) -> (Vec<u16>, Vec<u16>, Vec<u16>, usize, usize) {
    let probe = ffprobe_color(path);
    let parse = |key: &str| -> usize {
        probe
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .unwrap_or_else(|| panic!("missing {key} in {probe}"))
            .parse()
            .unwrap()
    };
    let width = parse("width=");
    let height = parse("height=");
    let output = Command::new(FFMPEG)
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv444p10le", "-"])
        .output()
        .expect("decode 10-bit planes");
    assert!(
        output.status.success(),
        "decode failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pixels = width * height;
    assert_eq!(output.stdout.len(), pixels * 3 * 2);
    let plane = |index: usize| -> Vec<u16> {
        output.stdout[index * pixels * 2..(index + 1) * pixels * 2]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect()
    };
    (plane(0), plane(1), plane(2), width, height)
}

fn codes_at(
    planes: &(Vec<u16>, Vec<u16>, Vec<u16>, usize, usize),
    x: usize,
    y: usize,
) -> [u16; 3] {
    let index = y * planes.3 + x;
    [planes.0[index], planes.1[index], planes.2[index]]
}

fn clip_line(log: &str) -> Option<&str> {
    log_kind(log, "clip").first().copied()
}

#[test]
fn dlog2_ffv1_readback_is_exact_and_prores_patches_stay_within_budget() {
    let patch_width = 64;
    let height = 32;
    let (root, input, tool) = patch_material("dlog2-readback", patch_width, height);

    // --- FFV1 (full range, lossless): every code must match the oracle. -----
    let ffv1 = root.join("ffv1").join(VIDEO_MKV_NAME);
    let mut args = base_args(&input, &ffv1, &tool);
    args.extend([
        "--strength",
        "0",
        "--preview-profile",
        PROFILE,
        "--encoder",
        "ffv1",
        "--verify-output",
    ]);
    let result = run_cli(&args);
    assert!(
        result.status.success(),
        "ffv1 run failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        stderr(&result)
    );
    assert_only_video_and_log(ffv1.parent().unwrap(), VIDEO_MKV_NAME, &[]);
    let probe = ffprobe_color(&ffv1);
    for expected in [
        "codec_name=ffv1",
        "pix_fmt=yuv444p10le",
        "color_space=bt2020nc",
        "color_primaries=unknown",
        "color_transfer=unknown",
        "nb_read_frames=1",
    ] {
        assert!(probe.contains(expected), "ffv1: {probe}");
    }
    let planes = decode_planes(&ffv1);
    assert_eq!((planes.3, planes.4), (256, height as usize));
    for (patch, (_, full, _)) in PATCHES.iter().enumerate() {
        for y in 0..height as usize {
            for x in 0..patch_width as usize {
                let observed = codes_at(&planes, patch * patch_width as usize + x, y);
                assert_eq!(
                    observed, *full,
                    "ffv1 patch {patch} ({x},{y}) must be bit-exact"
                );
            }
        }
    }
    let log = read_log(ffv1.parent().unwrap());
    let clip = clip_line(&log).expect("successful run reports clip totals");
    assert_eq!(log_field(clip, "frames"), Some("1"), "{clip}");
    assert_eq!(log_field(clip, "clipped_low"), Some("0"), "{clip}");
    assert_eq!(log_field(clip, "clipped_high"), Some("0"), "{clip}");
    let samples = (256 * height * 3).to_string();
    assert_eq!(
        log_field(clip, "total_channel_samples"),
        Some(samples.as_str()),
        "{clip} channel samples, not pixels"
    );
    let colour = log_kind(&log, "colour")[0];
    assert!(colour.contains("profile=dji-dlog2-dgamut2"), "{colour}");
    assert!(colour.contains("effective_range=pc"), "{colour}");
    assert!(colour.contains("reference_status=unverified"), "{colour}");
    assert!(colour.contains("output_cat_method=cat02"), "{colour}");
    assert!(colour.contains("output_cat_realization=reference"), "{colour}");

    // --- ProRes (limited range, lossy): interior patch budget. --------------
    let prores = root.join("prores").join(VIDEO_NAME);
    let mut args = base_args(&input, &prores, &tool);
    args.extend(["--strength", "0", "--preview-profile", PROFILE]);
    let result = run_cli(&args);
    assert!(
        result.status.success(),
        "prores run failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        stderr(&result)
    );
    assert_only_video_and_log(prores.parent().unwrap(), VIDEO_NAME, &[]);
    let probe = ffprobe_color(&prores);
    for expected in [
        "codec_name=prores",
        "profile=4444",
        "color_space=bt2020nc",
        "color_primaries=unknown",
        "color_transfer=unknown",
    ] {
        assert!(probe.contains(expected), "prores: {probe}");
    }
    let planes = decode_planes(&prores);
    let mut boundary_worst = 0i32;
    for (patch, (_, _, limited)) in PATCHES.iter().enumerate() {
        let mut worst = 0i32;
        let mut sum = 0i64;
        let mut count = 0i64;
        let start = patch * patch_width as usize;
        let end = start + patch_width as usize;
        // Reject banding/patch-edge ringing from the measurement: only pixels at
        // least 8 samples away from a patch boundary are compared.
        for y in 0..height as usize {
            for x in start..end {
                let observed = codes_at(&planes, x, y);
                for channel in 0..3 {
                    let difference =
                        (i32::from(observed[channel]) - i32::from(limited[channel])).abs();
                    if x >= start + 8 && x + 8 < end {
                        worst = worst.max(difference);
                        sum += i64::from(difference);
                        count += 1;
                    } else {
                        boundary_worst = boundary_worst.max(difference);
                    }
                }
            }
        }
        if patch == 0 {
            // Reachability pre-check on the current encoder: if this baseline
            // flat patch cannot meet the frozen budget, the cause must be
            // separated (quantization vs matrix vs codec) and reported instead
            // of relaxing the bound.
            assert!(
                worst <= 4 && sum as f64 / count as f64 <= 1.0,
                "ProRes reachability pre-check failed for the neutral patch: \
                 max {worst} mean {}; do not relax the budget",
                sum as f64 / count as f64
            );
        }
        let mean = sum as f64 / count as f64;
        eprintln!(
            "dlog2 prores patch {patch}: interior max {worst} codes, mean {mean:.4} \
             (budget max<=4 mean<=1)"
        );
        assert!(worst <= 4, "patch {patch}: interior max error {worst} > 4 codes");
        assert!(mean <= 1.0, "patch {patch}: interior mean error {mean:.4} > 1 code");
    }
    eprintln!("dlog2 prores worst boundary error (reported, not asserted): {boundary_worst} codes");
    let prores_log = read_log(prores.parent().unwrap());
    let clip = clip_line(&prores_log).expect("clip totals");
    assert_eq!(log_field(clip, "clipped_low"), Some("0"), "{clip}");
    assert_eq!(log_field(clip, "clipped_high"), Some("0"), "{clip}");
}

#[test]
fn dlog2_failure_publishes_nothing_and_reports_no_delivered_clip_totals() {
    let root = test_dir("dlog2-failure");
    let names = ["frame_0000.DNG", "frame_0001.DNG", "frame_0002.DNG"];
    let input = root.join("material");
    make_input(&input, &names);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).expect("fixtures");
    let frames: Vec<FakeFrame> = names
        .iter()
        .map(|name| FakeFrame::new(name, 0.18f64.log2()))
        .collect();
    let tool = fake_native_tool(&fixtures, &frames, Some("frame_0001.DNG"), false);
    let directory = root.join("run");
    let video = directory.join(VIDEO_NAME);
    let mut args = base_args(&input, &video, &tool);
    args.extend(["--strength", "0", "--preview-profile", PROFILE]);
    let result = run_cli(&args);
    assert_eq!(result.status.code(), Some(30), "{}", stderr(&result));
    assert!(
        !video.exists(),
        "a failed run must not publish a video"
    );
    assert!(
        video.parent().unwrap().join("timelapse.log").exists(),
        "the failure log is retained"
    );
    // Only the log (plus the retained scratch) exists: no manifest/audit sidecar.
    let entries: Vec<String> = fs::read_dir(&directory)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with(".rawtl-"))
        .collect();
    assert_eq!(entries, vec!["timelapse.log".to_string()], "{entries:?}");
    let log = read_log(&directory);
    assert!(!log.contains("kind=video"), "no video record on failure: {log}");
    assert!(
        clip_line(&log).is_none(),
        "uncommitted frames must not produce delivered clip totals: {log}"
    );
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn dlog2_clip_totals_and_handoff_codes_do_not_depend_on_worker_concurrency() {
    let (root, input, tool) = clip_material("dlog2-concurrency");
    let sequential_graph = root.join("graph-sequential.json");
    let sequential = include_str!("../src/graph/default.json")
        .replace("\"concurrency\":4", "\"concurrency\":1")
        .replace("\"concurrency\":8", "\"concurrency\":1");
    fs::write(&sequential_graph, sequential).expect("write sequential graph");

    let run = |label: &str, graph: Option<&Path>| -> (PathBuf, String) {
        let directory = root.join(label);
        let video = directory.join(VIDEO_MKV_NAME);
        let mut owned: Vec<String> = vec![
            "--input".into(),
            input.display().to_string(),
            "--output".into(),
            video.display().to_string(),
            "--dng-linear-frame".into(),
            tool.display().to_string(),
            "--ffmpeg".into(),
            FFMPEG.into(),
            "--ffprobe".into(),
            FFPROBE.into(),
            "--strength".into(),
            "0".into(),
            "--preview-profile".into(),
            PROFILE.into(),
            "--encoder".into(),
            "ffv1".into(),
            "--log-reference-gain".into(),
            "1000".into(),
        ];
        if let Some(graph) = graph {
            owned.push("--graph".into());
            owned.push(graph.display().to_string());
        }
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        let result = run_cli(&refs);
        assert!(
            result.status.success(),
            "{label} run failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&result.stdout),
            stderr(&result)
        );
        assert_only_video_and_log(&directory, VIDEO_MKV_NAME, &[]);
        (video, read_log(&directory))
    };

    // Default graph: develop/color run four frames at a time.
    let (parallel_video, parallel_log) = run("parallel", None);
    // Same sequence with every node serialized.
    let (sequential_video, sequential_log) = run("sequential", Some(&sequential_graph));

    let parallel_clip = clip_line(&parallel_log).expect("parallel clip totals");
    let sequential_clip = clip_line(&sequential_log).expect("sequential clip totals");
    eprintln!("parallel:   {parallel_clip}");
    eprintln!("sequential: {sequential_clip}");
    // The log prefix carries wall-clock/elapsed values; compare every clip field.
    let clip_fields = |line: &str| -> Vec<(String, String)> {
        ["frames", "clipped_low", "clipped_high", "clipped_total",
         "total_channel_samples", "unit"]
            .iter()
            .map(|key| {
                (
                    (*key).to_string(),
                    log_field(line, key)
                        .unwrap_or_else(|| panic!("missing {key} in {line}"))
                        .to_string(),
                )
            })
            .collect()
    };
    assert_eq!(
        clip_fields(parallel_clip),
        clip_fields(sequential_clip),
        "ordered totals must match"
    );
    let frames = CLIP_FRAMES.to_string();
    let low = CLIP_LOW.to_string();
    let high = CLIP_HIGH.to_string();
    let total = CLIP_TOTAL.to_string();
    let samples = CLIP_SAMPLES.to_string();
    assert_eq!(log_field(parallel_clip, "frames"), Some(frames.as_str()), "{parallel_clip}");
    assert_eq!(log_field(parallel_clip, "clipped_low"), Some(low.as_str()), "{parallel_clip}");
    assert_eq!(log_field(parallel_clip, "clipped_high"), Some(high.as_str()), "{parallel_clip}");
    assert_eq!(log_field(parallel_clip, "clipped_total"), Some(total.as_str()), "{parallel_clip}");
    assert_eq!(
        log_field(parallel_clip, "total_channel_samples"),
        Some(samples.as_str()),
        "{parallel_clip}"
    );
    assert_eq!(log_field(parallel_clip, "unit"), Some("channel_samples"));

    // The delivered 10-bit handoff is identical in both runs and matches the
    // oracle for both clipped and unclipped classes.
    let parallel_planes = decode_planes(&parallel_video);
    let sequential_planes = decode_planes(&sequential_video);
    assert_eq!(parallel_planes.0, sequential_planes.0, "luma differs");
    assert_eq!(parallel_planes.1, sequential_planes.1, "Cb differs");
    assert_eq!(parallel_planes.2, sequential_planes.2, "Cr differs");
    for (class, (_, codes)) in CLIP_CLASSES.iter().enumerate() {
        for y in 0..CLIP_HEIGHT as usize {
            for x in class * 8..(class + 1) * 8 {
                let observed = codes_at(&parallel_planes, x, y);
                assert_eq!(
                    observed, *codes,
                    "class {class} at ({x},{y}) must match the full-range oracle"
                );
            }
        }
    }
}

#[test]
fn dlog2_incomplete_run_publishes_no_delivered_clip_totals() {
    // The colour stage may already have committed some frames when the encoder
    // fails. An incomplete run must publish nothing and must not present a clip
    // summary as delivered statistics; the failure path keeps only the log and
    // the retained scratch for diagnosis.
    let (root, input, tool) = clip_material("dlog2-concurrency-failure");
    let failing_encoder = fake_failing_encoder(&root);
    let directory = root.join("run");
    let video = directory.join(VIDEO_MKV_NAME);
    let mut args = base_args(&input, &video, &tool);
    args.extend([
        "--strength",
        "0",
        "--preview-profile",
        PROFILE,
        "--encoder",
        "ffv1",
        "--log-reference-gain",
        "1000",
    ]);
    let mut owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    owned.push("--ffmpeg".into());
    owned.push(failing_encoder.display().to_string());
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    let result = run_cli(&refs);
    assert_eq!(result.status.code(), Some(60), "{}", stderr(&result));
    assert!(!video.exists(), "an incomplete run must not publish a video");
    let log = read_log(&directory);
    assert!(!log.contains("kind=video"), "no video record: {log}");
    assert!(
        clip_line(&log).is_none(),
        "an incomplete run must not report delivered clip totals: {log}"
    );
}

#[test]
fn dlog2_neutral_gradient_is_exact_in_ffv1_and_within_budget_in_prores() {
    let (root, input, tool) = gradient_material("dlog2-gradient");
    let width = (GRADIENT_BAND * GRADIENT.len() as u32) as usize;

    let ffv1 = root.join("ffv1").join(VIDEO_MKV_NAME);
    let mut args = base_args(&input, &ffv1, &tool);
    args.extend([
        "--strength",
        "0",
        "--preview-profile",
        PROFILE,
        "--encoder",
        "ffv1",
    ]);
    let result = run_cli(&args);
    assert!(result.status.success(), "ffv1: {}", stderr(&result));
    let planes = decode_planes(&ffv1);
    let mut previous = 0u16;
    for (band, (_, full, _)) in GRADIENT.iter().enumerate() {
        for y in 0..GRADIENT_HEIGHT as usize {
            for x in band * GRADIENT_BAND as usize..(band + 1) * GRADIENT_BAND as usize {
                let observed = codes_at(&planes, x, y);
                assert_eq!(observed, *full, "ffv1 gradient band {band} at ({x},{y})");
            }
        }
        assert!(full[0] > previous, "gradient luma must increase");
        previous = full[0];
    }

    let prores = root.join("prores").join(VIDEO_NAME);
    let mut args = base_args(&input, &prores, &tool);
    args.extend(["--strength", "0", "--preview-profile", PROFILE]);
    let result = run_cli(&args);
    assert!(result.status.success(), "prores: {}", stderr(&result));
    let planes = decode_planes(&prores);
    assert_eq!(planes.3, width);
    let mut boundary_worst = 0i32;
    for band in 0..GRADIENT.len() {
        let limited = GRADIENT[band].2;
        let start = band * GRADIENT_BAND as usize;
        let end = start + GRADIENT_BAND as usize;
        let mut worst = 0i32;
        let mut sum = 0i64;
        let mut count = 0i64;
        for y in 0..GRADIENT_HEIGHT as usize {
            for x in start..end {
                let observed = codes_at(&planes, x, y);
                for channel in 0..3 {
                    let difference =
                        (i32::from(observed[channel]) - i32::from(limited[channel])).abs();
                    if x >= start + 8 && x + 8 < end {
                        worst = worst.max(difference);
                        sum += i64::from(difference);
                        count += 1;
                    } else {
                        boundary_worst = boundary_worst.max(difference);
                    }
                }
            }
        }
        let mean = sum as f64 / count as f64;
        eprintln!(
            "dlog2 prores gradient band {band}: interior max {worst}, mean {mean:.4}"
        );
        assert!(worst <= 4, "band {band}: interior max {worst} > 4");
        assert!(mean <= 1.0, "band {band}: interior mean {mean:.4} > 1");
    }
    eprintln!("dlog2 prores gradient worst boundary error (reported): {boundary_worst}");
}
