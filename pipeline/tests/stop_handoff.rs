mod common;
#[test]
fn caller_cancel_reaps_real_native_converter_group_and_encoder() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let native = repo.join("native/build/dng-linear-frame");
    assert!(
        native.is_file(),
        "build native producer before process-boundary tests"
    );
    let work = common::test_dir("stop-handoff");
    let result = std::process::Command::new("/usr/bin/python3")
        .arg(repo.join("pipeline/tests/stop_handoff.py"))
        .arg(env!("CARGO_BIN_EXE_rawtl-linear-video"))
        .arg(native)
        .arg(work)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    println!("{}", String::from_utf8_lossy(&result.stdout));
}
