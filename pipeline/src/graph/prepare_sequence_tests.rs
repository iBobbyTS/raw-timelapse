//! Whole-sequence identity, cancellation and early measurement ordering.
use super::*;

#[test]
fn whole_sequence_identity_gate_rejects_last_item_before_any_graph_commit() {
    for defect in ["missing", "duplicate", "extra", "source", "last-context"] {
        let mut fake = Fake {
            sequence_defect: Some(defect),
            ..Default::default()
        };
        let error = execute(&Graph::default_graph(true), 4, &mut fake).unwrap_err();
        assert!(
            error.contains("sequence") || error.contains("last context"),
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
fn completed_sequence_cannot_publish_after_commit_time_cancellation() {
    let mut fake = Fake {
        sequence_defect: Some("late-cancel"),
        cancelled: Some(Arc::default()),
        ..Default::default()
    };
    let error = execute(&Graph::default_graph(true), 4, &mut fake).unwrap_err();
    assert!(error.contains("cancelled"), "{error}");
    assert!(fake.prepare_commits.is_empty());
    assert!(fake.measured.is_empty());
    assert!(!fake.events.iter().any(|e| e.node == "convert-cache"));
}

#[test]
fn early_measurements_drain_an_ordered_prefix_without_waiting_for_sequence_or_using_final_slots() {
    let (trace, events) = std::sync::mpsc::channel();
    let (seal, sequence_completion) = std::sync::mpsc::channel();
    let ready = Arc::new(std::sync::Mutex::new(BTreeSet::from([1, 2, 3])));
    let mut fake = Fake {
        frame_count: 4,
        trace: Some(trace),
        early_ready: Some(ready.clone()),
        sequence_completion: Some(sequence_completion),
        ..Default::default()
    };
    let worker = std::thread::spawn(move || {
        let text = include_str!("default.json").replace(
            r#""kind":"solve", "concurrency":8"#,
            r#""kind":"solve", "concurrency":1"#,
        );
        execute(&Graph::parse(&text).unwrap(), 4, &mut fake).unwrap();
        fake
    });
    assert_eq!(
        events.recv_timeout(Duration::from_secs(2)).unwrap(),
        "prepare-start [0, 1, 2, 3]"
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
    assert_eq!(fake.sequences, vec![vec![0, 1, 2, 3]]);
    assert_eq!(fake.sequence_stats.last().unwrap().delivered, 1);
    assert_eq!(fake.prepare_commits, vec![0, 1, 2, 3]);
    assert_eq!(fake.measured, vec![0, 1, 2, 3]);
}
