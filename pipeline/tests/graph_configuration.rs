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
    let directory = root.join(label);
    let video = directory.join(VIDEO_NAME);
    let result = run_cli(&[
        "--input",
        input.to_str().unwrap(),
        "--output",
        video.to_str().unwrap(),
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
    directory
}

fn cr3_material(root: &Path, fixtures: &Path) -> (PathBuf, PathBuf) {
    fs::create_dir_all(fixtures).unwrap();
    let input = root.join("input");
    make_input(&input, &["IMG_1.CR3", "IMG_2.CR3", "IMG_3.CR3"]);
    let frames: Vec<_> = (1..=3)
        .map(|id| FakeFrame::new(&format!("IMG_{id}.CR3"), 0.18f64.log2()))
        .collect();
    (input, fake_native_tool(fixtures, &frames, None, false))
}

#[test]
fn actual_graph_controls_identity_exposure_cleanup_and_source_protection() {
    let root = test_dir("graph-config");
    let fixtures = root.join("fixtures");
    let (input, tool) = cr3_material(&root, &fixtures);

    let baseline = run(&root, "baseline", &input, &tool, &graph());
    assert_only_video_and_log(&baseline, VIDEO_NAME, &[]);
    let base_pixel = decode_first_pixel(&baseline.join(VIDEO_NAME), "bt709", "tv");
    let trace = read_log(&baseline);
    assert_eq!(log_kind(&trace, "delete").len(), 3);
    for frame in 1..=3 {
        let develop = trace
            .find(&format!("kind=develop frame={frame} "))
            .expect("develop");
        let delete = trace
            .find(&format!("kind=delete frame={frame} "))
            .expect("delete");
        let encode = trace
            .find(&format!("kind=node node=encode frame={frame} "))
            .expect("encode");
        assert!(develop < delete && delete < encode, "{trace}");
    }
    assert!(!scratch_from_log(&trace).exists());

    // An identity node between apply-ev and colour changes nothing.
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
        decode_first_pixel(&kept.join(VIDEO_NAME), "bt709", "tv")
    );

    // An explicit exposure node applies exactly one EV and brightens the image.
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
    let pixel = decode_first_pixel(&exposed.join(VIDEO_NAME), "bt709", "tv");
    let exposed_log = read_log(&exposed);
    let exposure_evs: Vec<f64> = log_kind(&exposed_log, "exposure")
        .iter()
        .map(|line| log_field(line, "ev").unwrap().parse().unwrap())
        .collect();
    assert_eq!(exposure_evs, vec![1.0, 1.0, 1.0]);
    assert!(
        pixel.iter().map(|v| *v as u32).sum::<u32>()
            > base_pixel.iter().map(|v| *v as u32).sum::<u32>() + 60
    );

    // Without a delete node no cache deletion is dispatched; original sources
    // are never a deletion target and the run scratch still disappears.
    let mut retained = graph();
    nodes(&mut retained).pop();
    let retained = run(&root, "retained", &input, &tool, &retained);
    let retained_log = read_log(&retained);
    assert!(log_kind(&retained_log, "delete").is_empty(), "{retained_log}");
    assert!(!scratch_from_log(&retained_log).exists());
    for id in 1..=3 {
        assert!(input.join(format!("IMG_{id}.CR3")).is_file());
    }

    // Moving the delete wait behind encode reorders the events.
    let mut late = graph();
    set(
        &mut nodes(&mut late)[6],
        "waits",
        json::parse(r#"[{"node":"encode","scope":"frame"}]"#).unwrap(),
    );
    let late = run(&root, "late-delete", &input, &tool, &late);
    let trace = read_log(&late);
    for frame in 1..=3 {
        let encode = trace
            .find(&format!("kind=node node=encode frame={frame} "))
            .expect("encode");
        let delete = trace
            .find(&format!("kind=delete frame={frame} "))
            .expect("delete");
        assert!(encode < delete, "{trace}");
    }

    // A prepared LinearRaw DNG source is its own cache: nothing is deleted.
    let direct = root.join("direct");
    make_input(&direct, &["IMG_1.DNG", "IMG_2.DNG", "IMG_3.DNG"]);
    let originals: Vec<_> = (1..=3)
        .map(|id| fs::read(direct.join(format!("IMG_{id}.DNG"))).unwrap())
        .collect();
    let direct_fixtures = fixtures.join("direct");
    fs::create_dir_all(&direct_fixtures).unwrap();
    let frames: Vec<_> = (1..=3)
        .map(|id| FakeFrame::new(&format!("IMG_{id}.DNG"), 0.18f64.log2()))
        .collect();
    let direct_tool = fake_native_tool(&direct_fixtures, &frames, None, false);
    let output = run(&root, "direct-run", &direct, &direct_tool, &graph());
    assert!(log_kind(&read_log(&output), "delete").is_empty());
    for id in 1..=3 {
        assert_eq!(
            fs::read(direct.join(format!("IMG_{id}.DNG"))).unwrap(),
            originals[id - 1]
        );
    }
}

#[test]
fn invalid_graph_is_rejected_before_prepare_and_writes_nothing() {
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
    let directory = root.join("run");
    let video = directory.join(VIDEO_NAME);
    let result = run_cli(&[
        "--input",
        input.to_str().unwrap(),
        "--output",
        video.to_str().unwrap(),
        "--graph",
        config.to_str().unwrap(),
        "--dng-linear-frame",
        native.to_str().unwrap(),
    ]);
    assert_eq!(result.status.code(), Some(50));
    assert!(
        !directory.exists(),
        "an invalid graph creates no output directory, log or scratch"
    );
}
