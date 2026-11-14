//! Real graph -> accepted Rust batch API -> native -> controlled ADC boundaries.
mod common;
#[test]
fn graph_batch_atomicity_identity_mapping_and_joint_cancellation() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let native = std::env::var_os("RAWTL_NATIVE_TOOL")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repo.join("native/build/dng-linear-frame"));
    let work = common::test_dir("graph-batch").canonicalize().unwrap();
    let result = std::process::Command::new("/usr/bin/python3")
        .arg(repo.join("pipeline/tests/graph_batch.py"))
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
    println!("{}", String::from_utf8_lossy(&result.stdout));
}
