mod common;

use common::{fake_native_tool, ffprobe, make_input, require_ffmpeg, standard_frames, test_dir};
use rawtl_linear_video::json::Json;
use std::fs;
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
