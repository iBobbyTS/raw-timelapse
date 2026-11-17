//! Material-selected real graph equivalence; invoked explicitly by S02 receipts.
mod common;
#[test]
fn real_graph_batch_and_single_frame_equivalence() {
    let Some(input) = std::env::var_os("RAWTL_GRAPH_REAL_INPUT") else {
        eprintln!(
            "unconfigured: RAWTL_GRAPH_REAL_INPUT is needed for actual ADC graph equivalence"
        );
        return;
    };
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let native = std::env::var_os("RAWTL_NATIVE_TOOL")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repo.join("native/build/dng-linear-frame"));
    let work = common::test_dir("graph-real").canonicalize().unwrap();
    let result = std::process::Command::new("/usr/bin/python3")
        .arg(repo.join("pipeline/tests/graph_batch_real.py"))
        .arg(common::internal_runner())
        .arg(native)
        .arg(&work)
        .arg(input)
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
