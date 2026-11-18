//! Graph-side whole-set gate and prepare-only batch capacity oracles.
use super::*;

fn config(batch: &str, concurrency: &str) -> String {
    include_str!("default.json")
        .replace("\"batch_size\":32", &format!("\"batch_size\":{batch}"))
        .replace(
            "\"kind\":\"prepare\", \"concurrency\":1",
            &format!("\"kind\":\"prepare\", \"concurrency\":{concurrency}"),
        )
}
#[test]
fn prepare_batch_configuration_matrix() {
    for value in ["1", "32", "64"] {
        let graph = Graph::parse(&config(value, "1")).unwrap();
        assert_eq!(graph.nodes[1].batch_size, value.parse::<usize>().unwrap());
    }
    for value in [
        "null",
        "true",
        "false",
        "\"32\"",
        "0",
        "-1",
        "1.0",
        "1e0",
        "65",
        "18446744073709551616",
        "2,\"batch_size\":3",
    ] {
        assert!(Graph::parse(&config(value, "1")).is_err(), "{value}");
    }
    let omitted = config("1", "4").replace(", \"batch_size\":1", "");
    let graph = Graph::parse(&omitted).unwrap();
    assert_eq!(
        (graph.nodes[1].batch_size, graph.nodes[1].concurrency),
        (1, 4)
    );
    let error = Graph::parse(&config("32", "4")).unwrap_err();
    assert!(
        error.contains("batch_size") && error.contains("concurrency=1"),
        "{error}"
    );
    for kind in [
        "source",
        "solve",
        "zero",
        "barrier",
        "references",
        "develop",
        "identity",
        "exposure",
        "resize",
        "color",
        "encode",
        "delete",
    ] {
        let text =
            config("1", "1").replace("\"kind\":\"prepare\"", &format!("\"kind\":\"{kind}\""));
        assert!(
            Graph::parse(&text)
                .unwrap_err()
                .contains("batch_size only belongs to prepare"),
            "{kind}"
        );
    }
    assert_eq!(Graph::default_graph(true).nodes[1].batch_size, 32);
}

#[test]
fn available_prefix_checks_every_source_and_wait_and_flushes_tail() {
    let graph = Graph::parse(&config("4", "1")).unwrap();
    let mut node = graph.nodes[1].clone();
    node.waits.push(Wait {
        node: 2,
        scope: Scope::Frame,
    });
    let mut states: Vec<State<(), ()>> = (0..graph.nodes.len()).map(|_| State::default()).collect();
    states[0].frames.extend(0..7);
    states[2].frames.extend([0, 1, 3, 4, 5, 6]);
    assert_eq!(available_prepare_prefix(&node, &states, 0, 7), vec![0, 1]);
    assert!(available_prepare_prefix(&node, &states, 2, 7).is_empty());
    states[2].frames.insert(2);
    states[0].frames.remove(&3);
    assert_eq!(
        available_prepare_prefix(&node, &states, 0, 7),
        vec![0, 1, 2]
    );
    states[0].frames.insert(3);
    assert_eq!(
        available_prepare_prefix(&node, &states, 0, 7),
        vec![0, 1, 2, 3]
    );
    assert_eq!(
        available_prepare_prefix(&node, &states, 4, 7),
        vec![4, 5, 6]
    );
    node.waits[0].scope = Scope::Sequence;
    assert!(available_prepare_prefix(&node, &states, 0, 7).is_empty());
    states[2].closed = true;
    assert_eq!(available_prepare_prefix(&node, &states, 0, 7).len(), 4);
}

#[test]
fn whole_batch_identity_gate_rejects_last_item_before_any_graph_commit() {
    for defect in ["missing", "duplicate", "extra", "source", "last-context"] {
        let mut fake = Fake {
            batch_defect: Some(defect),
            ..Default::default()
        };
        let error = execute(&Graph::parse(&config("4", "1")).unwrap(), 4, &mut fake).unwrap_err();
        assert!(
            error.contains("batch") || error.contains("last context"),
            "{error}"
        );
        assert!(fake.prepare_commits.is_empty(), "{defect}");
        assert!(fake.measured.is_empty());
        assert!(!fake
            .events
            .iter()
            .any(|e| e.node == "convert-cache" || e.node == "measure-solve"));
        assert!(fake.consumed.is_empty());
    }
}

#[test]
fn out_of_order_adc_complete_set_commits_in_order_and_next_batch_overlaps_slow_measure() {
    let (trace, events) = std::sync::mpsc::channel();
    let (complete, completions) = std::sync::mpsc::channel();
    let (measure, measure_gate) = std::sync::mpsc::channel();
    let mut fake = Fake {
        frame_count: 9,
        trace: Some(trace),
        batch_completion: Some(completions),
        slow_measure: Some(measure_gate),
        ..Default::default()
    };
    let worker = std::thread::spawn(move || {
        execute(&Graph::parse(&config("4", "1")).unwrap(), 9, &mut fake).unwrap();
        fake
    });
    let mut observed = vec![];
    let recv_until = |observed: &mut Vec<String>, wanted: &str| loop {
        let event = events
            .recv_timeout(Duration::from_secs(10))
            .expect("batch progress deadline");
        let done = event == wanted;
        observed.push(event);
        if done {
            break;
        }
    };
    recv_until(&mut observed, "batch-start [0, 1, 2, 3]");
    for id in [3, 1, 0] {
        complete.send(id).unwrap();
        recv_until(&mut observed, &format!("adc-complete {id}"));
        assert!(!observed
            .iter()
            .any(|e| e.starts_with("prepare-commit") || e.starts_with("measure-start")));
    }
    complete.send(2).unwrap();
    recv_until(&mut observed, "batch-start [4, 5, 6, 7]");
    assert_eq!(
        observed
            .iter()
            .filter(|e| e.starts_with("prepare-commit"))
            .cloned()
            .collect::<Vec<_>>(),
        [
            "prepare-commit 0",
            "prepare-commit 1",
            "prepare-commit 2",
            "prepare-commit 3"
        ]
    );
    recv_until(&mut observed, "batch-start [8]");
    // Head measurement is still blocked, but all lightweight batches drained.
    measure.send(()).unwrap();
    let fake = worker.join().unwrap();
    assert_eq!(
        fake.batches,
        vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7], vec![8]]
    );
    assert_eq!(fake.prepare_commits, (0..9).collect::<Vec<_>>());
    assert_eq!(fake.measured, (0..9).collect::<Vec<_>>());
    assert!(fake
        .batch_stats
        .iter()
        .all(|s| s.capacity == 1 && s.accepted - s.delivered <= 1 && s.peak <= 1));
    assert_eq!(fake.batch_stats.last().unwrap().delivered, 3);
    for name in [
        "source",
        "convert-cache",
        "measure-solve",
        "apply-ev",
        "color",
        "encode",
    ] {
        assert_eq!(
            fake.events
                .iter()
                .filter(|e| e.node == name && e.scope == Scope::Sequence)
                .count(),
            1
        );
    }
}

#[test]
fn default_batch_tail_zero_and_legal_source_waits_preserve_exact_eof() {
    for scope in ["frame", "sequence"] {
        let text = config("4", "1").replace(
            "\"kind\":\"prepare\",",
            &format!(
                "\"kind\":\"prepare\",\"waits\":[{{\"node\":\"source\",\"scope\":\"{scope}\"}}],"
            ),
        );
        let mut graph = Graph::parse(&text).unwrap();
        graph.disable_measurement();
        let mut fake = Fake {
            zero: true,
            frame_count: 9,
            ..Default::default()
        };
        execute(&graph, 9, &mut fake).unwrap();
        assert_eq!(
            fake.batches,
            vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7], vec![8]]
        );
        assert!(fake.measured.is_empty());
        assert_eq!(fake.consumed.len(), 9);
        assert_eq!(
            fake.events
                .iter()
                .filter(|e| e.node == "convert-cache" && e.scope == Scope::Sequence)
                .count(),
            1
        );
    }
    for target in ["convert-cache", "measure-solve", "apply-ev", "encode"] {
        for scope in ["frame", "sequence"] {
            let text = config("4", "1").replace("\"kind\":\"source\",", &format!("\"kind\":\"source\",\"waits\":[{{\"node\":\"{target}\",\"scope\":\"{scope}\"}}],"));
            let error = Graph::parse(&text).unwrap_err();
            assert!(
                error.contains("cycle")
                    || error.contains("frame event requested from sequence-only"),
                "{error}"
            );
        }
    }
}

#[test]
fn completed_batch_cannot_publish_after_commit_time_cancellation() {
    let mut fake = Fake {
        batch_defect: Some("late-cancel"),
        cancelled: Some(Arc::default()),
        ..Default::default()
    };
    let error = execute(&Graph::parse(&config("4", "1")).unwrap(), 4, &mut fake).unwrap_err();
    assert!(error.contains("cancelled"), "{error}");
    assert!(fake.prepare_commits.is_empty());
    assert!(fake.measured.is_empty());
    assert!(!fake.events.iter().any(|e| e.node == "convert-cache"));
}

#[test]
fn controlled_backend_dispatches_short_available_prefix_without_waiting_for_full_batch() {
    let graph = Graph::parse(&config("4", "1")).unwrap();
    let node = &graph.nodes[1];
    let mut states: Vec<State<String, Vec<f32>>> =
        (0..graph.nodes.len()).map(|_| State::default()).collect();
    states[0].frames.extend([0, 1]);
    let (wake, completion) = std::sync::mpsc::channel();
    let mut stage = PrepareBatches::new(4, wake);
    let mut backend = Fake::default();
    assert!(admit_prepare_batch(node, &states, 5, &mut stage, &mut backend).unwrap());
    assert_eq!(backend.batches, vec![vec![0, 1]]);
    // Real task completion, not a guessed sleep; unavailable frame 2 cannot
    // prevent this short batch from becoming ready to materialize.
    completion.recv_timeout(Duration::from_secs(10)).unwrap();
    stage.poll().unwrap();
    assert_eq!(stage.commit_next().unwrap().unwrap().ids, vec![0, 1]);
    states[0].frames.extend([2, 3, 4]);
    assert!(
        !admit_prepare_batch(node, &states, 5, &mut stage, &mut backend).unwrap(),
        "pending materialization holds its batch slot"
    );
    stage.delivered().unwrap();
    assert!(admit_prepare_batch(node, &states, 5, &mut stage, &mut backend).unwrap());
    assert_eq!(backend.batches, vec![vec![0, 1], vec![2, 3, 4]]);
    completion.recv_timeout(Duration::from_secs(10)).unwrap();
    stage.poll().unwrap();
    assert_eq!(stage.commit_next().unwrap().unwrap().ids, vec![2, 3, 4]);
    stage.delivered().unwrap();
    assert!(
        !admit_prepare_batch(node, &states, 5, &mut stage, &mut backend).unwrap(),
        "no empty batch at EOF"
    );
}

#[test]
fn early_measurements_drain_an_ordered_prefix_without_waiting_for_batch_or_using_final_slots() {
    let (trace, events) = std::sync::mpsc::channel();
    let (seal, batch_completion) = std::sync::mpsc::channel();
    let ready = Arc::new(std::sync::Mutex::new(BTreeSet::from([1, 2, 3])));
    let mut fake = Fake {
        frame_count: 4,
        trace: Some(trace),
        early_ready: Some(ready.clone()),
        batch_completion: Some(batch_completion),
        ..Default::default()
    };
    let worker = std::thread::spawn(move || {
        let text = config("4", "1").replace(
            r#""kind":"solve", "concurrency":8"#,
            r#""kind":"solve", "concurrency":1"#,
        );
        execute(&Graph::parse(&text).unwrap(), 4, &mut fake).unwrap();
        fake
    });
    assert_eq!(
        events.recv_timeout(Duration::from_secs(2)).unwrap(),
        "batch-start [0, 1, 2, 3]"
    );
    assert!(events.recv_timeout(Duration::from_millis(120)).is_err());
    ready.lock().unwrap().insert(0);
    // With a single measurement worker, success on all four while Prepare
    // remains unsealed proves provisional completions release their slots.
    for id in 0..4 {
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            format!("early {id}")
        );
    }
    for id in (0..4).rev() {
        seal.send(id).unwrap();
    }
    let fake = worker.join().unwrap();
    assert_eq!(fake.prepare_commits, vec![0, 1, 2, 3]);
    assert_eq!(fake.measured, vec![0, 1, 2, 3]);
}
