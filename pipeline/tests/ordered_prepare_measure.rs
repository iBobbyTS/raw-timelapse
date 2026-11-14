mod common;
use common::*;
use std::fs;
use std::path::PathBuf;

#[test]
fn real_cli_orders_prepare_and_measure_and_freezes_source_zero_white_balance() {
    let root = test_dir("ordered-prepare-measure");
    let input = root.join("input");
    make_input(
        &input,
        &["IMG_1.DNG", "IMG_2.DNG", "IMG_3.DNG", "IMG_4.DNG"],
    );
    let frames: Vec<_> = (1..=4)
        .map(|id| {
            FakeFrame::new(&format!("IMG_{id}.DNG"), -2.).neutral(if id == 1 {
                "0.4,1,0.7"
            } else {
                "0.8,1,0.5"
            })
        })
        .collect();
    let tool = fake_native_tool(&root, &frames, None, false);
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = std::process::Command::new("/usr/bin/python3")
        .arg(repo.join("tests/ordered_prepare_measure.py"))
        .args([
            env!("CARGO_BIN_EXE_rawtl-linear-video"),
            tool.to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    fs::write(root.join("harness.stdout"), &output.stdout).unwrap();
    fs::write(root.join("harness.stderr"), &output.stderr).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let video = root.join("output/timelapse.mov");
    assert!(ffprobe(&video).contains("nb_read_frames=4"));
    let trace = fs::read_to_string(root.join("output/timelapse.log")).unwrap();
    for kind in ["prepare", "measure", "develop"] {
        let ids: Vec<_> = log_kind(&trace, kind)
            .iter()
            .map(|line| log_field(line, "frame").unwrap().parse::<usize>().unwrap())
            .collect();
        assert_eq!(ids, [1, 2, 3, 4], "{kind}: {trace}");
    }
    for line in log_kind(&trace, "develop") {
        assert_eq!(log_field(line, "wb"), Some("0.4,1,0.7"));
    }
    let baseline = root.join("serial/timelapse.mov");
    let result = run_cli(&[
        "--input",
        input.to_str().unwrap(),
        "--output",
        baseline.to_str().unwrap(),
        "--dng-linear-frame",
        tool.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let serial = fs::read_to_string(baseline.with_extension("log")).unwrap();
    for (parallel, serial) in log_kind(&trace, "develop")
        .iter()
        .zip(log_kind(&serial, "develop"))
    {
        for field in [
            "frame",
            "source",
            "ev",
            "wb",
            "source_sha256",
            "pixels_sha256",
        ] {
            assert_eq!(
                log_field(parallel, field),
                log_field(serial, field),
                "{field}"
            );
        }
    }
}
