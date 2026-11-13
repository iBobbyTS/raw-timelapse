mod common;
use common::*;
use rawtl_linear_video::json::{self, Json};
use std::fs;
use std::path::{Path, PathBuf};
fn graph() -> Json {
    json::parse(include_str!("../src/graph/default.json")).unwrap()
}
fn nodes(graph: &mut Json) -> &mut Vec<Json> {
    if let Json::Object(fields) = graph {
        if let Json::Array(nodes) = &mut fields[0].1 {
            return nodes;
        }
    }
    panic!("graph shape")
}
fn set(node: &mut Json, key: &str, value: Json) {
    if let Json::Object(fields) = node {
        if let Some((_, v)) = fields.iter_mut().find(|(k, _)| k == key) {
            *v = value;
        } else {
            fields.push((key.into(), value));
        }
    }
}
fn run(root: &Path, label: &str, input: &Path, native: &Path, graph: &Json) -> PathBuf {
    let config = root.join(format!("{label}.json"));
    fs::write(&config, json::to_pretty_string(graph)).unwrap();
    let output = root.join(label);
    let result = run_cli(&[
        "--input",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--dng-linear-frame",
        native.to_str().unwrap(),
        "--graph",
        config.to_str().unwrap(),
        "--strength",
        "0",
    ]);
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    output
}
#[test]
fn actual_graph_controls_identity_exposure_cleanup_and_source_protection() {
    let root = test_dir("graph-config");
    let input = root.join("input");
    let fixtures = root.join("fixtures");
    fs::create_dir_all(&fixtures).unwrap();
    make_input(&input, &["IMG_1.CR3", "IMG_2.CR3", "IMG_3.CR3"]);
    let frames: Vec<_> = (1..=3)
        .map(|id| FakeFrame::new(&format!("IMG_{id}.CR3"), 0.18f64.log2()))
        .collect();
    let tool = fake_native_tool(&fixtures, &frames, None, false);
    let baseline = run(&root, "baseline", &input, &tool, &graph());
    let base_pixel = decode_first_pixel(&baseline.join("output.mp4"), "bt709", "tv");
    let trace = fs::read_to_string(baseline.join("logs/graph.log")).unwrap();
    assert_eq!(
        trace
            .lines()
            .filter(|l| l.starts_with("deleted frame="))
            .count(),
        3
    );
    for id in 0..3 {
        assert!(
            trace
                .find(&format!("node=apply-ev frame=Some({id})"))
                .unwrap()
                < trace.find(&format!("deleted frame={id}")).unwrap()
        );
        assert!(
            trace.find(&format!("deleted frame={id}")).unwrap()
                < trace
                    .find(&format!("node=encode frame=Some({id})"))
                    .unwrap()
        );
    }
    let mut identity = graph();
    nodes(&mut identity).push(
        json::parse(r#"{"id":"keep-pixels","kind":"identity","inputs":{"image":"apply-ev"}}"#)
            .unwrap(),
    );
    set(
        &mut nodes(&mut identity)[4],
        "inputs",
        json::parse(r#"{"image":"keep-pixels"}"#).unwrap(),
    );
    let kept = run(&root, "identity", &input, &tool, &identity);
    assert_eq!(
        base_pixel,
        decode_first_pixel(&kept.join("output.mp4"), "bt709", "tv")
    );
    set(
        nodes(&mut identity).last_mut().unwrap(),
        "kind",
        Json::Str("exposure".into()),
    );
    set(
        nodes(&mut identity).last_mut().unwrap(),
        "ev",
        Json::Float(1.),
    );
    let exposed = run(&root, "exposure", &input, &tool, &identity);
    let pixel = decode_first_pixel(&exposed.join("output.mp4"), "bt709", "tv");
    let manifest = read_json(&exposed.join("manifest.json"));
    let entry = &manifest
        .get("frames")
        .unwrap()
        .get("entries")
        .and_then(Json::as_array)
        .unwrap()[0];
    assert_eq!(entry.get("applied_ev").and_then(Json::as_f64), Some(1.));
    assert!(
        (entry
            .get("observed_scene_max")
            .and_then(Json::as_f64)
            .unwrap()
            - 0.36)
            .abs()
            < 1e-5
    );
    assert!(
        pixel.iter().map(|v| *v as u32).sum::<u32>()
            > base_pixel.iter().map(|v| *v as u32).sum::<u32>() + 60
    );
    let mut retained = graph();
    nodes(&mut retained).pop();
    let retained = run(&root, "retained", &input, &tool, &retained);
    for id in 1..=3 {
        assert!(retained
            .join(format!(
                ".rawtl-partial/cache/frame_{id:06}/linear/IMG_{id}.CR3"
            ))
            .is_file());
    }
    let mut late = graph();
    set(
        &mut nodes(&mut late)[6],
        "waits",
        json::parse(r#"[{"node":"encode","scope":"frame"}]"#).unwrap(),
    );
    let late = run(&root, "late-delete", &input, &tool, &late);
    let trace = fs::read_to_string(late.join("logs/graph.log")).unwrap();
    for id in 0..3 {
        assert!(
            trace
                .find(&format!("node=encode frame=Some({id})"))
                .unwrap()
                < trace.find(&format!("deleted frame={id}")).unwrap()
        );
    }
    let direct = root.join("direct");
    make_input(&direct, &["IMG_1.DNG", "IMG_2.DNG", "IMG_3.DNG"]);
    let originals: Vec<_> = (1..=3)
        .map(|id| fs::read(direct.join(format!("IMG_{id}.DNG"))).unwrap())
        .collect();
    let frames: Vec<_> = (1..=3)
        .map(|id| FakeFrame::new(&format!("IMG_{id}.DNG"), 0.18f64.log2()))
        .collect();
    fs::create_dir_all(fixtures.join("direct")).unwrap();
    let direct_tool = fake_native_tool(&fixtures.join("direct"), &frames, None, false);
    let output = run(&root, "direct-run", &direct, &direct_tool, &graph());
    for id in 1..=3 {
        assert_eq!(
            fs::read(direct.join(format!("IMG_{id}.DNG"))).unwrap(),
            originals[id - 1]
        );
    }
    assert!(!fs::read_to_string(output.join("logs/graph.log"))
        .unwrap()
        .contains("deleted frame="));
}
#[test]
fn invalid_graph_is_rejected_before_prepare() {
    let root = test_dir("graph-reject");
    let input = root.join("input");
    make_input(&input, &["IMG_1.DNG"]);
    let config = root.join("graph.json");
    fs::write(
        &config,
        include_str!("../src/graph/default.json")
            .replace("\"image\":\"color\"", "\"image\":\"convert-cache\""),
    )
    .unwrap();
    let native = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("native/build/dng-linear-frame");
    let result = run_cli(&[
        "--input",
        input.to_str().unwrap(),
        "--output",
        root.join("run").to_str().unwrap(),
        "--graph",
        config.to_str().unwrap(),
        "--dng-linear-frame",
        native.to_str().unwrap(),
    ]);
    assert_eq!(result.status.code(), Some(50));
    assert!(!root.join("run/logs/prepare").exists());
}
