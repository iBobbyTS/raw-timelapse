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

/// Generated cache payloads: every regular file under the cache except the
/// prepared-context sidecars. The native producer keeps the source file name
/// (and its extension) for a converted LinearRaw payload, so filtering by a
/// `.dng` suffix would miss vendor RAW sources.
fn cache_payloads(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    fn walk(directory: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, files);
            } else if path.file_name().and_then(|name| name.to_str()) != Some("prepared.context") {
                files.push(path);
            }
        }
    }
    walk(directory, &mut files);
    files.sort();
    files
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

    // Without a delete node no cache deletion is dispatched: the graph owns
    // deletion, so the generated cache survives the successful run, the log
    // names the retained location, and the original sources stay untouched.
    let mut retained = graph();
    nodes(&mut retained).pop();
    let retained = run(&root, "retained", &input, &tool, &retained);
    let retained_log = read_log(&retained);
    assert!(log_kind(&retained_log, "delete").is_empty(), "{retained_log}");
    let retained_scratch = scratch_from_log(&retained_log);
    assert!(
        retained_scratch.is_dir(),
        "a custom graph without delete must retain its generated cache"
    );
    let generated = cache_payloads(&retained_scratch.join("cache"));
    assert_eq!(generated.len(), 3, "generated cache payloads: {generated:?}");
    for path in &generated {
        assert!(path.is_file(), "{}", path.display());
    }
    let retained_lines = log_kind(&retained_log, "retained-cache");
    assert_eq!(retained_lines.len(), 1, "{retained_log}");
    assert_eq!(
        log_field(retained_lines[0], "count"),
        Some("3"),
        "{retained_log}"
    );
    assert_eq!(
        log_field(retained_lines[0], "path").map(PathBuf::from),
        Some(retained_scratch.join("cache")),
        "{retained_log}"
    );
    assert!(retained.join(VIDEO_NAME).is_file());
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

#[test]
fn invalid_concurrency_is_rejected_before_any_output_or_native_work() {
    let root = test_dir("concurrency-reject");
    let input = root.join("input");
    make_input(&input, &["IMG_1.DNG"]);
    let native = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("native/build/dng-linear-frame");
    for (case, value) in [
        "null",
        "\"4\"",
        "true",
        "0",
        "-1",
        "1.0",
        "1e0",
        "65",
        "18446744073709551616",
        "2,\"concurrency\":3",
    ]
    .into_iter()
    .enumerate()
    {
        let mut config = graph();
        set(&mut nodes(&mut config)[1], "concurrency", Json::Int(777));
        let path = root.join(format!("invalid-{case}.json"));
        fs::write(
            &path,
            json::to_pretty_string(&config)
                .replace("\"concurrency\": 777", &format!("\"concurrency\": {value}")),
        )
        .unwrap();
        let output = root.join(format!("output-{case}"));
        let video = output.join(VIDEO_NAME);
        let result = run_cli(&[
            "--input",
            input.to_str().unwrap(),
            "--output",
            video.to_str().unwrap(),
            "--graph",
            path.to_str().unwrap(),
            "--dng-linear-frame",
            native.to_str().unwrap(),
        ]);
        assert_eq!(
            result.status.code(),
            Some(50),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!output.exists());
    }
}

#[test]
fn strength_zero_normalizes_parallel_solve_and_logs_effective_configuration() {
    let root = test_dir("parallel-zero");
    let (input, tool) = cr3_material(&root, &root.join("fixtures"));
    let mut config = graph();
    set(&mut nodes(&mut config)[1], "concurrency", Json::Int(4));
    set(&mut nodes(&mut config)[2], "concurrency", Json::Int(8));
    let output = run(&root, "output", &input, &tool, &config);
    let trace = read_log(&output);
    assert!(
        trace.contains("convert-cache:4,measure-solve:1,apply-ev:1"),
        "{trace}"
    );
    assert!(log_kind(&trace, "measure").is_empty());
    assert!(ffprobe(&output.join(VIDEO_NAME)).contains("nb_read_frames=3"));
}

#[test]
fn delete_reader_dependencies_reject_before_output_or_native_work() {
    let root = test_dir("delete-reader-dependencies");
    let input = root.join("input");
    make_input(&input, &["IMG_1.DNG", "IMG_2.DNG"]);
    let native = root.join("native-marker");
    let marker = root.join("native-started");
    write_executable(
        &native,
        "#!/bin/sh\ntouch \"$(dirname \"$0\")/native-started\"\nexit 99\n",
    );
    for (label, text) in [
        (
            "develop-wait",
            include_str!("fixtures/delete_reader_dependencies/develop_waits_delete.json"),
        ),
        (
            "sequence-wait",
            include_str!("fixtures/delete_reader_dependencies/color_waits_delete_sequence.json"),
        ),
    ] {
        let config = root.join(format!("{label}.json"));
        fs::write(&config, text).unwrap();
        let directory = root.join(label);
        let video = directory.join(VIDEO_NAME);
        let result = run_cli(&[
            "--input",
            input.to_str().unwrap(),
            "--output",
            video.to_str().unwrap(),
            "--graph",
            config.to_str().unwrap(),
            "--strength",
            "0",
            "--resize",
            "320x214",
            "--dng-linear-frame",
            native.to_str().unwrap(),
        ]);
        fs::write(root.join(format!("{label}.stdout")), &result.stdout).unwrap();
        fs::write(root.join(format!("{label}.stderr")), &result.stderr).unwrap();
        fs::write(
            root.join(format!("{label}.status")),
            format!("{}\nexit_code={:?}\n", result.status, result.status.code()),
        )
        .unwrap();
        assert_eq!(
            result.status.code(),
            Some(50),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!marker.exists(), "invalid graph invoked native work");
        assert!(
            !directory.exists(),
            "invalid graph created output/log/scratch"
        );
    }
}
