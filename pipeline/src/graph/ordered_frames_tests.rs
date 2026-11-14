use super::ordered_frames::*;
use std::sync::mpsc;
use std::time::Duration;

fn key(id: usize) -> TaskKey {
    TaskKey {
        node: "prepare".into(),
        frame_id: id,
        source: format!("source-{id}"),
    }
}
fn gated(stage: &mut OrderedStage<usize>, id: usize) -> mpsc::Sender<()> {
    let (send, recv) = mpsc::channel();
    stage
        .submit(
            key(id),
            Box::new(move || {
                recv.recv_timeout(Duration::from_secs(5))
                    .map_err(|e| e.to_string())?;
                Ok(id)
            }),
        )
        .unwrap();
    send
}
fn completion(stage: &mut OrderedStage<usize>, wake: &mpsc::Receiver<()>) {
    wake.recv_timeout(Duration::from_secs(5)).unwrap();
    stage.poll().unwrap();
    println!("completion observed: accepted={} outstanding={}", stage.accepted(), stage.outstanding());
}

#[test]
fn ordered_sliding_window_delivers_head_before_the_wave_finishes() {
    let (send, wake) = mpsc::channel();
    let mut stage = OrderedStage::new(4, send);
    let gates: Vec<_> = (0..4).map(|id| gated(&mut stage, id)).collect();
    for id in [2, 0] {
        gates[id].send(()).unwrap();
        completion(&mut stage, &wake);
        if id == 2 {
            assert!(stage.commit_next().is_none());
        }
    }
    let first = stage.commit_next().unwrap();
    assert_eq!((first.key, first.value), (key(0), 0));
    assert_eq!(stage.outstanding(), 4, "commit does not release capacity");
    stage.delivered(0).unwrap();
    let fifth = gated(&mut stage, 4); // Immediate refill; 1 and 3 are still gated.
    for id in [3, 1] {
        gates[id].send(()).unwrap();
        completion(&mut stage, &wake);
    }
    assert_eq!(stage.commit_next().unwrap().value, 1);
    stage.delivered(1).unwrap();
    let sixth = gated(&mut stage, 5);
    for id in 2..4 {
        assert_eq!(stage.commit_next().unwrap().value, id);
        stage.delivered(id).unwrap();
    }
    sixth.send(()).unwrap();
    completion(&mut stage, &wake);
    assert!(stage.commit_next().is_none());
    fifth.send(()).unwrap();
    completion(&mut stage, &wake);
    assert_eq!(stage.commit_next().unwrap().value, 4);
    stage.delivered(4).unwrap();
    assert_eq!(stage.commit_next().unwrap().value, 5);
    stage.delivered(5).unwrap();
    println!("delivered trace: [0, 1, 2, 3, 4, 5]; first four completions: [2, 0, 3, 1]");
    assert_eq!(stage.outstanding(), 0);
}

#[test]
fn slow_head_and_blocked_downstream_keep_completed_frames_in_capacity() {
    let (send, wake) = mpsc::channel();
    let mut stage = OrderedStage::new(3, send);
    let gates: Vec<_> = (0..3).map(|id| gated(&mut stage, id)).collect();
    for id in [2, 1] {
        gates[id].send(()).unwrap();
        completion(&mut stage, &wake);
        assert!(stage.commit_next().is_none());
        assert!(!stage.has_capacity());
        assert_eq!(stage.outstanding(), 3);
    }
    assert!(stage.submit(key(3), Box::new(|| Ok(3))).is_err());
    gates[0].send(()).unwrap();
    completion(&mut stage, &wake);
    for id in 0..3 {
        assert_eq!(stage.commit_next().unwrap().value, id);
    }
    assert!(!stage.has_capacity(), "downstream has accepted nothing");
    assert_eq!(stage.outstanding(), 3);
    for id in 0..3 {
        stage.delivered(id).unwrap();
        assert_eq!(stage.outstanding(), 2 - id);
    }
}

#[test]
fn later_failure_and_worker_panic_are_observed_without_waiting_for_head() {
    for panic in [false, true] {
        let (send, wake) = mpsc::channel();
        let mut stage = OrderedStage::new(2, send);
        let gate = gated(&mut stage, 0);
        stage
            .submit(
                key(1),
                Box::new(move || {
                    if panic {
                        panic!("controlled worker panic");
                    }
                    Err("controlled task failure".into())
                }),
            )
            .unwrap();
        wake.recv_timeout(Duration::from_secs(5)).unwrap();
        let failure = stage.poll().unwrap_err();
        assert!(
            failure.contains("source-1") && failure.contains("frame 1"),
            "{failure}"
        );
        gate.send(()).unwrap(); // Releasing the controlled head permits deterministic join.
    }
}

#[test]
fn admission_and_delivery_reject_duplicate_or_foreign_frame_ids() {
    let (send, wake) = mpsc::channel();
    let mut stage = OrderedStage::new(2, send);
    assert!(stage.submit(key(1), Box::new(|| Ok(1))).is_err());
    let gate = gated(&mut stage, 0);
    assert!(stage.submit(key(0), Box::new(|| Ok(0))).is_err());
    assert!(stage.delivered(0).is_err());
    gate.send(()).unwrap();
    completion(&mut stage, &wake);
    assert_eq!(stage.commit_next().unwrap().key, key(0));
    stage.delivered(0).unwrap();
    assert!(stage.delivered(0).is_err());
}
