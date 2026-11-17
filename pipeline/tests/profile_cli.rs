mod common;

use common::{fake_native_tool, ffprobe, make_input, require_ffmpeg, standard_frames, test_dir};
use rawtl_linear_video::json::Json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rawtl-linear-video"))
        .args(args)
        .output()
        .expect("run public CLI")
}

#[test]
fn public_cli_requires_a_profile_and_a_directory() {
    let root = test_dir("profile-invalid");
    let input = root.join("input.dng");
    fs::write(&input, []).unwrap();
    let directory = root.to_str().unwrap();
    let help = String::from_utf8(cli(&["--help"]).stdout).unwrap();
    assert!(help.contains("--profile") && !help.contains("--graph") && !help.contains("--resize"));

    for (args, message) in [
        (vec!["--profile", "default"], "--input"),
        (vec!["--input", directory], "--profile"),
        (
            vec!["--input", directory, "--profile", "profile.json"],
            "absolute",
        ),
        (
            vec!["--input", input.to_str().unwrap(), "--profile", "default"],
            "not a directory",
        ),
        (
            vec![
                "--input",
                directory,
                "--profile",
                "default",
                "--resize",
                "4x4",
            ],
            "unknown argument",
        ),
        (
            vec![
                "--input",
                directory,
                "--profile",
                "default",
                "--graph",
                "graph.json",
            ],
            "unknown argument",
        ),
    ] {
        let result = cli(&args);
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(
            !result.status.success() && error.contains(message),
            "{args:?}: {error}"
        );
    }
}

#[test]
fn custom_profile_delivers_by_its_own_container_beside_input() {
    require_ffmpeg();
    let root = test_dir("profile-delivery");
    let input = root.join("sequence.v2");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG", "IMG_10.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).unwrap();
    let tool = fake_native_tool(&fixtures, &standard_frames(&fixtures), None, false);
    let profile = root.join("custom.json");
    let settings = Json::obj(vec![
        (
            "dng_linear_frame",
            Json::Str(tool.to_str().unwrap().to_owned()),
        ),
        ("encoder", Json::Str("ffv1".into())),
        ("output_color", Json::Str("clog3".into())),
        ("fps", Json::Str("24000/1001".into())),
        ("resize", Json::Str("4x4".into())),
        ("strength", Json::Int(0)),
        ("verify_output", Json::Bool(true)),
    ]);
    fs::write(&profile, rawtl_linear_video::json::to_pretty_string(&settings)).unwrap();
    let args = [
        "--input",
        input.to_str().unwrap(),
        "--profile",
        profile.to_str().unwrap(),
    ];
    let wrong_path = root.join("wrong.mov");
    let mut wrong_args = args.to_vec();
    wrong_args.extend(["--output", wrong_path.to_str().unwrap()]);
    let wrong = cli(&wrong_args);
    assert!(!wrong.status.success());
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("matching container"));
    assert!(!wrong_path.with_extension("log").exists());

    let result = cli(&args);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let video = root.join("sequence.v2.mkv");
    let probe = ffprobe(&video);
    for expected in [
        "codec_name=ffv1",
        "width=4",
        "height=4",
        "avg_frame_rate=24000/1001",
        "nb_read_frames=3",
    ] {
        assert!(probe.contains(expected), "{probe}");
    }
    let log = fs::read_to_string(root.join("sequence.v2.log")).unwrap();
    assert!(log.contains("kind=profile") && log.contains("clog3"));
    assert_eq!(cli(&args).status.code(), Some(12));
}

#[test]
fn auto_log_resolves_only_the_first_numeric_source() {
    require_ffmpeg();
    let root = test_dir("profile-auto-log");
    let input = root.join("sequence");
    make_input(&input, &["IMG_10.DNG", "IMG_2.DNG"]);
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).unwrap();
    let tool = fake_native_tool(&fixtures, &standard_frames(&fixtures), None, false);
    let exiftool = root.join("exiftool");
    fs::write(
        &exiftool,
        r#"#!/bin/sh
for source do :; done
name=${source##*/}
printf '%s\n' "$name" >> "$(dirname "$0")/metadata-calls"
case "$name" in
  IMG_2.DNG) printf '%s\n' '[{"IFD0:Make":"Canon","IFD0:UniqueCameraModel":"Canon EOS R7"}]' ;;
  *) printf '%s\n' '[{"IFD0:Make":"FUJIFILM","IFD0:Model":"X-T5"}]' ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&exiftool, fs::Permissions::from_mode(0o755)).unwrap();

    let mut decoded = Vec::new();
    for (color, video_name) in [("auto-log", "auto.mkv"), ("clog2", "explicit.mkv")] {
        let settings = Json::obj(vec![
            ("dng_linear_frame", Json::Str(tool.to_str().unwrap().into())),
            ("exiftool", Json::Str(exiftool.to_str().unwrap().into())),
            ("lens_correction", Json::Str("off".into())),
            ("encoder", Json::Str("ffv1".into())),
            ("output_color", Json::Str(color.into())),
            ("output_cat", Json::Str("cat02".into())),
            ("log_reference_gain", Json::Str("1.25".into())),
            ("strength", Json::Int(0)),
        ]);
        let profile = root.join(format!("{color}.json"));
        fs::write(
            &profile,
            rawtl_linear_video::json::to_pretty_string(&settings),
        )
        .unwrap();
        let video = root.join(video_name);
        let result = cli(&[
            "--input",
            input.to_str().unwrap(),
            "--profile",
            profile.to_str().unwrap(),
            "--output",
            video.to_str().unwrap(),
        ]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let log = fs::read_to_string(video.with_extension("log")).unwrap();
        assert!(log.contains("kind=colour profile=clog2"), "{log}");
        assert!(log.contains("reference_gain=1.25"), "{log}");
        if color == "auto-log" {
            let choice = log
                .lines()
                .find(|line| line.contains("kind=auto-log"))
                .unwrap();
            assert!(
                choice.contains("IMG_2.DNG") && choice.ends_with("profile=clog2"),
                "{choice}"
            );
            assert!(log.find("kind=auto-log") < log.find("kind=prepare "));
        }
        let frames = Command::new(common::FFMPEG)
            .args(["-v", "error", "-i"])
            .arg(&video)
            .args(["-f", "rawvideo", "-pix_fmt", "yuv444p10le", "-"])
            .output()
            .unwrap();
        assert!(
            frames.status.success(),
            "{}",
            String::from_utf8_lossy(&frames.stderr)
        );
        assert_eq!(
            frames.stdout.len(),
            (2 * common::WIDTH * common::HEIGHT * 6) as usize
        );
        decoded.push(frames.stdout);
    }
    assert_eq!(decoded[0], decoded[1]);
    assert_eq!(
        fs::read_to_string(root.join("metadata-calls")).unwrap(),
        "IMG_2.DNG\n"
    );

    let input = root.join("unsupported");
    make_input(&input, &["IMG_10.DNG"]);
    let video = root.join("unsupported.mkv");
    let result = cli(&[
        "--input",
        input.to_str().unwrap(),
        "--profile",
        root.join("auto-log.json").to_str().unwrap(),
        "--output",
        video.to_str().unwrap(),
    ]);
    let error = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.status.code(), Some(10), "{error}");
    assert!(
        error.contains("unsupported camera make") && error.contains("output_color"),
        "{error}"
    );
    assert!(!video.exists());
    assert!(!fs::read_to_string(video.with_extension("log"))
        .unwrap()
        .contains("kind=prepare "));
}
