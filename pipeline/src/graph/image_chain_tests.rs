//! Deterministic whole-chain admission, commit, delivery and cancellation oracles.
use super::*;
use std::sync::{mpsc, Mutex};

#[derive(Clone, Debug)]
enum Trace {
    Start(String, usize),
    Computed(String, usize),
    Event(Event),
    Slots(String, ordered_frames::StageStats),
    Deleted(usize),
}
struct Controlled {
    trace: mpsc::Sender<Trace>,
    gates: BTreeMap<(String, usize), mpsc::Receiver<()>>,
    cancellation: Arc<(Mutex<bool>, std::sync::Condvar)>,
    events: Vec<Event>,
    consumed: Vec<usize>,
    fail: Option<(String, usize)>,
    block_until_cancel: Option<(String, usize)>,
}
impl Controlled {
    fn task(&mut self, name: &str, id: usize, image: usize) -> FrameTask<usize> {
        let name = name.to_owned();
        let gate = self.gates.remove(&(name.clone(), id));
        let trace = self.trace.clone();
        let fail = self.fail.as_ref() == Some(&(name.clone(), id));
        let cancel_gate = self.block_until_cancel.as_ref() == Some(&(name.clone(), id));
        let cancellation = self.cancellation.clone();
        Box::new(move || {
            trace.send(Trace::Start(name.clone(), id)).unwrap();
            if cancel_gate {
                let cancelled = cancellation.0.lock().unwrap();
                let (cancelled, deadline) = cancellation
                    .1
                    .wait_timeout_while(cancelled, Duration::from_secs(10), |cancelled| !*cancelled)
                    .unwrap();
                if deadline.timed_out() || !*cancelled {
                    return Err("cancellation deadline".into());
                }
            }
            if let Some(gate) = gate {
                gate.recv_timeout(Duration::from_secs(10))
                    .map_err(|e| e.to_string())?;
            }
            if fail {
                return Err("controlled image failure".into());
            }
            trace.send(Trace::Computed(name, id)).unwrap();
            Ok(image)
        })
    }
}
impl Backend for Controlled {
    type Reference = String;
    type Measurement = usize;
    type Image = usize;
    fn check(&mut self) -> Result<()> {
        Ok(())
    }
    fn source_identity(&self, id: usize) -> String {
        format!("source-{id}")
    }
    fn prepare_task(&mut self, id: usize) -> Result<FrameTask<String>> {
        Ok(Box::new(move || Ok(format!("hash-{id}"))))
    }
    fn commit_prepare(&mut self, _: usize, _: &String, _: Duration) -> Result<()> {
        Ok(())
    }
    fn hash<'a>(&self, r: &'a String) -> &'a str {
        r
    }
    fn measure_task(&mut self, id: usize, _: &String) -> Result<FrameTask<usize>> {
        Ok(Box::new(move || Ok(id)))
    }
    fn commit_measure(&mut self, _: usize, _: &String, _: usize, _: Duration) -> Result<()> {
        Ok(())
    }
    fn solve(&mut self) -> Result<Vec<Gain>> {
        panic!("zero plan must not measure")
    }
    fn develop_task(&mut self, id: usize, _: &String, ev: f64) -> Result<FrameTask<usize>> {
        assert_eq!(ev, 0.);
        Ok(self.task("develop", id, id))
    }
    fn image_task(&mut self, kind: Kind, id: usize, image: usize) -> Result<FrameTask<usize>> {
        let name = match kind {
            Kind::Identity => "identity",
            Kind::Exposure(_) => "exposure",
            Kind::Resize { .. } => "resize",
            Kind::Color => "color",
            _ => panic!("unexpected image kind"),
        };
        assert_eq!(image, id);
        Ok(self.task(name, id, image))
    }
    fn encode_task(&mut self, id: usize, image: usize) -> Result<FrameTask<usize>> {
        Ok(self.task("encode", id, image))
    }
    fn commit_encode(&mut self, id: usize, image: usize, _: Duration) -> Result<()> {
        assert_eq!(id, image);
        self.consumed.push(id);
        Ok(())
    }
    fn end(&mut self) -> Result<()> {
        Ok(())
    }
    fn delete(&mut self, id: usize, _: &String) -> Result<()> {
        assert!(self
            .events
            .iter()
            .any(|e| e.node == "develop" && e.frame_id == Some(id)));
        self.trace.send(Trace::Deleted(id)).unwrap();
        Ok(())
    }
    fn event(&mut self, e: &Event) -> Result<()> {
        self.events.push(e.clone());
        self.trace.send(Trace::Event(e.clone())).unwrap();
        Ok(())
    }
    fn activity(&mut self, node: &str, stats: ordered_frames::StageStats) -> Result<()> {
        self.trace.send(Trace::Slots(node.into(), stats)).unwrap();
        Ok(())
    }
    fn cancel(&mut self) {
        *self.cancellation.0.lock().unwrap() = true;
        self.cancellation.1.notify_all();
    }
}
fn graph(capacity: usize, deletion: bool, wait: &str) -> Graph {
    let delete = if deletion {
        r#",{"id":"delete","kind":"delete","inputs":{"frames":"prepare"},"waits":[{"node":"develop","scope":"frame"}]}"#
    } else {
        ""
    };
    Graph::parse(&format!(r#"{{"nodes":[
      {{"id":"source","kind":"source"}},{{"id":"prepare","kind":"prepare","inputs":{{"source":"source"}}}},
      {{"id":"zero","kind":"zero","inputs":{{"frames":"prepare"}}}},
      {{"id":"develop","kind":"develop","concurrency":{capacity},"inputs":{{"frames":"prepare","plan":"zero"}}}},
      {{"id":"identity","kind":"identity","concurrency":2,"inputs":{{"image":"develop"}}}},
      {{"id":"exposure","kind":"exposure","ev":1,"concurrency":2,"inputs":{{"image":"identity"}}}},
      {{"id":"resize","kind":"resize","width":8,"height":6,"concurrency":2,"inputs":{{"image":"exposure"}}}},
      {{"id":"color","kind":"color","concurrency":2,"inputs":{{"image":"resize"}}{wait}}},
      {{"id":"encode","kind":"encode","inputs":{{"image":"color"}}}}{delete}]}}"#)).unwrap()
}
fn recv(rx: &mpsc::Receiver<Trace>, trace: &mut Vec<Trace>, until: impl Fn(&Trace) -> bool) {
    loop {
        let event = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("bounded chain progress deadline");
        let done = until(&event);
        trace.push(event);
        if done {
            return;
        }
    }
}
#[test]
fn image_chain_slow_head_ordered_commits_and_blocked_sink_hold_all_capacity() {
    let (send, rx) = mpsc::channel();
    let mut gates = BTreeMap::new();
    let mut permits = vec![];
    for id in 0..4 {
        let (tx, gate) = mpsc::channel();
        permits.push(tx);
        gates.insert(("develop".into(), id), gate);
    }
    let (sink, gate) = mpsc::channel();
    gates.insert(("encode".into(), 0), gate);
    let mut backend = Controlled {
        trace: send,
        gates,
        cancellation: Default::default(),
        events: vec![],
        consumed: vec![],
        fail: None,
        block_until_cancel: None,
    };
    let worker = std::thread::spawn(move || {
        execute(
            &graph(4, true, r#", "waits":[{"node":"delete","scope":"frame"}]"#),
            24,
            &mut backend,
        )
        .unwrap();
        backend
    });
    let mut trace = vec![];
    recv(
        &rx,
        &mut trace,
        |e| matches!(e,Trace::Slots(n,s) if n=="develop" && s.accepted==4),
    );
    for id in [2, 3, 1] {
        permits[id].send(()).unwrap();
        recv(
            &rx,
            &mut trace,
            |e| matches!(e,Trace::Computed(n,f) if n=="develop" && *f==id),
        );
    }
    assert!(!trace
        .iter()
        .any(|e| matches!(e,Trace::Event(e) if e.node=="develop")));
    permits[0].send(()).unwrap();
    recv(
        &rx,
        &mut trace,
        |e| matches!(e,Trace::Start(n,0) if n=="encode"),
    );
    // With sink 0 held, the chain must fill exactly the sum of its capacities.
    let expected = [
        ("develop", 4),
        ("identity", 2),
        ("exposure", 2),
        ("resize", 2),
        ("color", 2),
        ("encode", 1),
    ];
    loop {
        if expected.iter().all(|(name, capacity)| {
            trace
                .iter()
                .rev()
                .find_map(|e| match e {
                    Trace::Slots(n, s) if n == name => Some(s.accepted - s.delivered == *capacity),
                    _ => None,
                })
                .unwrap_or(false)
        }) {
            break;
        }
        recv(&rx, &mut trace, |_| true);
    }
    let mut latest = BTreeMap::new();
    for e in &trace {
        if let Trace::Slots(n, s) = e {
            assert!(s.accepted - s.delivered <= s.capacity);
            assert!(s.delivered <= s.committed && s.committed <= s.accepted);
            latest.insert(n.clone(), *s);
        }
    }
    assert_eq!(
        latest["develop"].accepted, 13,
        "no hidden queue beyond 4+2+2+2+2+1"
    );
    assert!(!trace
        .iter()
        .any(|e| matches!(e,Trace::Event(e) if e.node=="encode")));
    assert!(trace.iter().any(|e| matches!(e, Trace::Deleted(0))));
    sink.send(()).unwrap();
    let backend = worker.join().unwrap();
    assert_eq!(backend.consumed, (0..24).collect::<Vec<_>>());
    for name in [
        "develop", "identity", "exposure", "resize", "color", "encode",
    ] {
        let ids: Vec<_> = backend
            .events
            .iter()
            .filter(|e| e.node == name)
            .filter_map(|e| e.frame_id)
            .collect();
        assert_eq!(ids, (0..24).collect::<Vec<_>>());
        assert_eq!(
            backend
                .events
                .iter()
                .filter(|e| e.node == name && e.scope == Scope::Sequence)
                .count(),
            1
        );
    }
    for id in 0..24 {
        let pos = |name| {
            backend
                .events
                .iter()
                .position(|e| e.node == name && e.frame_id == Some(id))
                .unwrap()
        };
        assert!(pos("develop") < pos("delete"));
        assert!(pos("delete") < pos("color"));
    }
    println!("bounded chain trace: {trace:?}");
}
#[test]
fn image_chain_no_delete_retains_cache_and_sequence_wait_rejects_at_every_capacity() {
    for capacity in [1, 4, 8, 64] {
        let (send, rx) = mpsc::channel();
        let mut b = Controlled {
            trace: send,
            gates: BTreeMap::new(),
            cancellation: Default::default(),
            events: vec![],
            consumed: vec![],
            fail: None,
            block_until_cancel: None,
        };
        execute(&graph(capacity, false, ""), 3, &mut b).unwrap();
        assert!(!rx.try_iter().any(|e| matches!(e, Trace::Deleted(_))));
        assert_eq!(b.consumed, vec![0, 1, 2]);
        let mut g = graph(capacity, true, "");
        let color = g.nodes.iter().position(|n| n.kind == Kind::Color).unwrap();
        let delete = g.nodes.iter().position(|n| n.kind == Kind::Delete).unwrap();
        g.nodes[color].waits.push(Wait {
            node: delete,
            scope: Scope::Sequence,
        });
        g.order.clear();
        assert!(g.validate().unwrap_err().contains("bounded image draining"));
    }
}

#[test]
fn image_chain_postprocessing_out_of_order_results_commit_in_input_order() {
    let (send, rx) = mpsc::channel();
    let mut gates = BTreeMap::new();
    let mut permits = BTreeMap::new();
    for name in ["develop", "identity", "exposure", "resize", "color"] {
        for id in 0..2 {
            let (permit, gate) = mpsc::channel();
            permits.insert((name, id), permit);
            gates.insert((name.into(), id), gate);
        }
    }
    let mut b = Controlled {
        trace: send,
        gates,
        cancellation: Default::default(),
        events: vec![],
        consumed: vec![],
        fail: None,
        block_until_cancel: None,
    };
    let worker = std::thread::spawn(move || {
        execute(&graph(2, true, ""), 6, &mut b).unwrap();
        b
    });
    let mut trace = vec![];
    for name in ["develop", "identity", "exposure", "resize", "color"] {
        for id in 0..2 {
            if !trace
                .iter()
                .any(|e| matches!(e,Trace::Start(n,f) if n==name && *f==id))
            {
                recv(
                    &rx,
                    &mut trace,
                    |e| matches!(e,Trace::Start(n,f) if n==name && *f==id),
                );
            }
        }
        permits[&(name, 1)].send(()).unwrap();
        recv(
            &rx,
            &mut trace,
            |e| matches!(e,Trace::Computed(n,1) if n==name),
        );
        assert!(!trace
            .iter()
            .any(|e| matches!(e,Trace::Event(e) if e.node==name)));
        permits[&(name, 0)].send(()).unwrap();
    }
    let b = worker.join().unwrap();
    assert_eq!(b.consumed, (0..6).collect::<Vec<_>>());
    for name in ["develop", "identity", "exposure", "resize", "color"] {
        assert_eq!(
            b.events
                .iter()
                .filter(|e| e.node == name)
                .filter_map(|e| e.frame_id)
                .collect::<Vec<_>>(),
            (0..6).collect::<Vec<_>>()
        );
    }
}

#[test]
fn image_chain_later_failure_cancels_head_and_discards_late_success() {
    let (send, rx) = mpsc::channel();
    let cancellation = Arc::default();
    let mut b = Controlled {
        trace: send,
        gates: BTreeMap::new(),
        cancellation,
        events: vec![],
        consumed: vec![],
        fail: Some(("develop".into(), 1)),
        block_until_cancel: Some(("develop".into(), 0)),
    };
    let error = execute(&graph(4, true, ""), 24, &mut b).unwrap_err();
    assert!(
        error.contains("node develop frame 1 source hash-1 failed: controlled image failure"),
        "{error}"
    );
    assert!(*b.cancellation.0.lock().unwrap());
    assert!(b.consumed.is_empty());
    assert!(!b
        .events
        .iter()
        .any(|e| matches!(e.node.as_str(), "develop" | "delete" | "encode")));
    assert!(
        rx.try_iter()
            .any(|e| matches!(e,Trace::Computed(n,0) if n=="develop")),
        "cancelled head returns late success, which must be discarded"
    );
}
