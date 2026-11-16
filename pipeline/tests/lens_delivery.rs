//! Actual SDK -> lens kernel -> sequence crop -> scene-linear resize -> FFmpeg.
mod common;
#[test]
fn lens_controls_and_fixed_crop_reach_real_video() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let native = std::env::var_os("RAWTL_NATIVE_TOOL")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repo.join("native/build/dng-linear-frame"));
    let work = common::test_dir("lens-delivery").canonicalize().unwrap();
    let result = std::process::Command::new("/usr/bin/python3")
        .arg(repo.join("pipeline/tests/lens_delivery.py"))
        .arg(env!("CARGO_BIN_EXE_rawtl-linear-video"))
        .arg(native)
        .arg(&work)
        .output()
        .unwrap();
    std::fs::write(work.join("harness.stdout"), &result.stdout).unwrap();
    std::fs::write(work.join("harness.stderr"), &result.stderr).unwrap();
    std::fs::write(
        work.join("harness.exit"),
        format!("{:?}\n", result.status.code()),
    )
    .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
