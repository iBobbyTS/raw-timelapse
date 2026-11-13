use super::*;
#[derive(Default)]
struct Fake {
    frame_count: usize,
    measured: Vec<usize>,
    events: Vec<Event>,
    deleted: Vec<usize>,
    consumed: Vec<(usize, Vec<f32>)>,
    order: Vec<String>,
    zero: bool,
    bad_plan: bool,
    plan_defect: Option<&'static str>,
}
impl Backend for Fake {
    type Reference = String;
    type Image = Vec<f32>;
    fn check(&mut self) -> Result<()> {
        Ok(())
    }
    fn prepare(&mut self, id: usize) -> Result<String> {
        self.order.push(format!("prepare {id}"));
        Ok(format!("hash{id}"))
    }
    fn hash<'a>(&self, r: &'a String) -> &'a str {
        r
    }
    fn measure(&mut self, id: usize, _: &String) -> Result<()> {
        self.measured.push(id);
        assert!(!self.events.iter().any(|e| e.node == "apply-ev"));
        Ok(())
    }
    fn solve(&mut self) -> Result<Vec<Gain>> {
        let count = if self.frame_count == 0 {
            3
        } else {
            self.frame_count
        };
        assert_eq!(self.measured, (0..count).collect::<Vec<_>>());
        assert!(self
            .events
            .iter()
            .any(|e| e.node == "convert-cache" && e.scope == Scope::Sequence));
        assert!(self.deleted.is_empty());
        let mut plan: Vec<_> = (0..count)
            .map(|id| Gain {
                frame_id: if self.bad_plan { 0 } else { id },
                source_sha256: format!("hash{id}"),
                ev: (id % 3) as f64,
            })
            .collect();
        match self.plan_defect {
            Some("missing") => {
                plan.pop();
            }
            Some("order") => plan.swap(0, 1),
            Some("nonfinite") => plan[0].ev = f64::NAN,
            Some("hash") => plan[0].source_sha256 = "foreign".into(),
            _ => (),
        }
        Ok(plan)
    }
    fn develop(&mut self, id: usize, _: &String, ev: f64) -> Result<Vec<f32>> {
        assert!(self
            .events
            .iter()
            .any(|e| e.node == "measure-solve" && e.scope == Scope::Sequence));
        if self.zero {
            assert_eq!(ev, 0.);
        } else {
            assert_eq!(ev, (id % 3) as f64);
        }
        Ok(vec![-0.5 * ev.exp2() as f32, 2. * ev.exp2() as f32])
    }
    fn exposure(&mut self, image: &mut Vec<f32>, ev: f64) -> Result<()> {
        for x in image {
            *x *= ev.exp2() as f32;
        }
        Ok(())
    }
    fn color(&mut self, image: Vec<f32>) -> Result<Vec<f32>> {
        Ok(image)
    }
    fn begin(&mut self) -> Result<()> {
        self.order.push("begin".into());
        Ok(())
    }
    fn push(&mut self, id: usize, image: Vec<f32>) -> Result<()> {
        if self
            .events
            .iter()
            .any(|e| e.node == "delete-cache" && e.frame_id == Some(id))
        {
            assert!(self.deleted.contains(&id));
        }
        self.order.push(format!("push {id}"));
        self.consumed.push((id, image));
        Ok(())
    }
    fn end(&mut self) -> Result<()> {
        self.order.push("end".into());
        Ok(())
    }
    fn delete(&mut self, id: usize, _: &String) -> Result<()> {
        self.order.push(format!("delete {id}"));
        self.deleted.push(id);
        Ok(())
    }
    fn event(&mut self, e: &Event) -> Result<()> {
        assert!(!self.events.contains(e));
        self.events.push(e.clone());
        Ok(())
    }
}
#[test]
fn sealed_solver_owned_pixels_and_delete_before_sink_read() {
    let mut fake = Fake::default();
    execute(&Graph::default_graph(true), 3, &mut fake).unwrap();
    assert_eq!(
        fake.consumed,
        vec![(0, vec![-0.5, 2.]), (1, vec![-1., 4.]), (2, vec![-2., 8.])]
    );
    for id in 0..3 {
        assert!(
            fake.order
                .iter()
                .position(|s| s == &format!("delete {id}"))
                .unwrap()
                < fake
                    .order
                    .iter()
                    .position(|s| s == &format!("push {id}"))
                    .unwrap()
        );
    }
}
#[test]
fn strength_zero_is_explicit_and_skips_measurement() {
    let mut fake = Fake {
        zero: true,
        ..Default::default()
    };
    execute(&Graph::default_graph(false), 3, &mut fake).unwrap();
    assert!(fake.measured.is_empty());
}
#[test]
fn ordinary_reference_streams_and_two_barriers() {
    let text=include_str!("default.json").replace("{\"id\":\"measure-solve\"", "{\"id\":\"seal-one\",\"kind\":\"barrier\",\"inputs\":{\"frames\":\"convert-cache\"}}, {\"id\":\"ref-stream\",\"kind\":\"references\",\"inputs\":{\"frames\":\"seal-one\"}}, {\"id\":\"seal-two\",\"kind\":\"barrier\",\"inputs\":{\"frames\":\"ref-stream\"}}, {\"id\":\"measure-solve\"").replace("\"frames\":\"convert-cache\"}},\n    {\"id\":\"apply-ev\"", "\"frames\":\"seal-two\"}},\n    {\"id\":\"apply-ev\"");
    let graph = Graph::parse(&text).unwrap();
    let mut fake = Fake::default();
    execute(&graph, 3, &mut fake).unwrap();
    for name in ["seal-one", "seal-two"] {
        assert!(fake
            .events
            .iter()
            .any(|e| e.node == name && e.scope == Scope::Sequence));
    }
}
#[test]
fn sealing_reorders_and_rejects_missing_duplicate_late_or_duplicate_eof() {
    let mut s = Seal::new(3);
    s.push(2, 20).unwrap();
    s.push(0, 0).unwrap();
    assert!(s.ordered().is_err());
    assert!(s.end(3).is_err());
    assert!(s.push(0, 99).is_err());
    s.push(1, 10).unwrap();
    s.end(3).unwrap();
    assert_eq!(s.ordered().unwrap(), vec![&0, &10, &20]);
    assert!(s.end(3).is_err());
    assert!(s.push(2, 99).is_err());
}
#[test]
fn bad_plan_is_a_protocol_error_without_develop_or_delete() {
    let mut fake = Fake {
        bad_plan: true,
        ..Default::default()
    };
    assert!(execute(&Graph::default_graph(true), 3, &mut fake)
        .unwrap_err()
        .contains("malformed exposure plan"));
    assert!(fake.deleted.is_empty());
    assert!(!fake.events.iter().any(|e| e.node == "apply-ev"));
}
#[test]
fn identity_and_explicit_exposure_are_real_nodes() {
    let text=include_str!("default.json").replace("{\"id\":\"color\"", "{\"id\":\"identity\",\"kind\":\"identity\",\"inputs\":{\"image\":\"apply-ev\"}},{\"id\":\"extra-ev\",\"kind\":\"exposure\",\"ev\":1,\"inputs\":{\"image\":\"identity\"}},{\"id\":\"color\"").replace("\"image\":\"apply-ev\"}},\n", "\"image\":\"extra-ev\"}},\n");
    let mut fake = Fake::default();
    execute(&Graph::parse(&text).unwrap(), 3, &mut fake).unwrap();
    assert_eq!(fake.consumed[0].1, vec![-1., 4.]);
}
#[test]
fn delete_removal_and_event_reconnection_control_timing() {
    let text = include_str!("default.json");
    let mut fake = Fake::default();
    execute(
        &Graph::parse(&text.replace("\"node\":\"apply-ev\"", "\"node\":\"encode\"")).unwrap(),
        3,
        &mut fake,
    )
    .unwrap();
    for id in 0..3 {
        assert!(
            fake.order
                .iter()
                .position(|s| s == &format!("push {id}"))
                .unwrap()
                < fake
                    .order
                    .iter()
                    .position(|s| s == &format!("delete {id}"))
                    .unwrap()
        );
    }
    let mut root = json::parse(text).unwrap();
    if let Json::Object(fields) = &mut root {
        if let Json::Array(nodes) = &mut fields[0].1 {
            nodes.pop();
        }
    }
    let mut fake = Fake::default();
    execute(
        &Graph::parse(&json::to_pretty_string(&root)).unwrap(),
        3,
        &mut fake,
    )
    .unwrap();
    assert!(fake.deleted.is_empty());
}
#[test]
fn rejects_cycles_port_mismatch_scope_unknown_and_bounded_deadlock() {
    let text = include_str!("default.json");
    for bad in [
        text.replace("\"source\":\"source\"", "\"source\":\"convert-cache\""),
        text.replace("\"image\":\"color\"", "\"image\":\"convert-cache\""),
        text.replace("\"node\":\"apply-ev\"", "\"node\":\"measure-solve\""),
        text.replace("\"scope\":\"frame\"", "\"scope\":\"bad\""),
        text.replace("\"kind\":\"color\"", "\"kind\":\"script\""),
        text.replace("\"node\":\"apply-ev\"", "\"node\":\"absent\""),
        text.replace(
            "\"kind\":\"encode\",",
            "\"kind\":\"encode\",\"waits\":[{\"node\":\"color\",\"scope\":\"sequence\"}],",
        ),
    ] {
        assert!(Graph::parse(&bad).is_err(), "{bad}");
    }
}

#[test]
fn unordered_source_inventory_is_sealed_before_dispatch() {
    let mut input = Seal::new(3);
    input.push(2, ()).unwrap();
    input.push(0, ()).unwrap();
    let mut fake = Fake::default();
    assert!(execute_sealed(&Graph::default_graph(true), &input, &mut fake).is_err());
    assert!(fake.order.is_empty());
    input.push(1, ()).unwrap();
    input.end(3).unwrap();
    execute_sealed(&Graph::default_graph(true), &input, &mut fake).unwrap();
    assert_eq!(
        fake.consumed.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[test]
fn last_measurement_and_unread_sink_are_controlled_by_events() {
    use std::sync::{mpsc, Mutex};
    use std::time::Duration;
    struct Gated {
        fake: Fake,
        root: std::path::PathBuf,
        events: Arc<Mutex<Vec<Event>>>,
        measurement_ready: mpsc::SyncSender<()>,
        measurement_continue: mpsc::Receiver<()>,
        sink_ready: mpsc::SyncSender<usize>,
        sink_continue: mpsc::Receiver<()>,
    }
    impl Backend for Gated {
        type Reference = String;
        type Image = Vec<f32>;
        fn check(&mut self) -> Result<()> {
            Ok(())
        }
        fn prepare(&mut self, id: usize) -> Result<String> {
            std::fs::write(self.root.join(format!("{id}.dng")), b"generated").unwrap();
            self.fake.prepare(id)
        }
        fn hash<'a>(&self, r: &'a String) -> &'a str {
            r
        }
        fn measure(&mut self, id: usize, r: &String) -> Result<()> {
            if id == 2 {
                self.measurement_ready.send(()).unwrap();
                self.measurement_continue
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
            }
            self.fake.measure(id, r)
        }
        fn solve(&mut self) -> Result<Vec<Gain>> {
            self.fake.solve()
        }
        fn develop(&mut self, id: usize, r: &String, ev: f64) -> Result<Vec<f32>> {
            self.fake.develop(id, r, ev)
        }
        fn exposure(&mut self, i: &mut Vec<f32>, ev: f64) -> Result<()> {
            self.fake.exposure(i, ev)
        }
        fn color(&mut self, i: Vec<f32>) -> Result<Vec<f32>> {
            Ok(i)
        }
        fn begin(&mut self) -> Result<()> {
            Ok(())
        }
        fn push(&mut self, id: usize, i: Vec<f32>) -> Result<()> {
            self.sink_ready.send(id).unwrap();
            self.sink_continue
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            self.fake.push(id, i)
        }
        fn end(&mut self) -> Result<()> {
            Ok(())
        }
        fn delete(&mut self, id: usize, r: &String) -> Result<()> {
            std::fs::remove_file(self.root.join(format!("{id}.dng"))).unwrap();
            self.fake.delete(id, r)
        }
        fn event(&mut self, e: &Event) -> Result<()> {
            self.events.lock().unwrap().push(e.clone());
            self.fake.event(e)
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("gated-graph-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let (ready, wait_ready) = mpsc::sync_channel(0);
    let (permit, wait_permit) = mpsc::sync_channel(0);
    let (sink, wait_sink) = mpsc::sync_channel(0);
    let (consume, wait_consume) = mpsc::sync_channel(0);
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut backend = Gated {
        fake: Fake::default(),
        root: root.clone(),
        events: events.clone(),
        measurement_ready: ready,
        measurement_continue: wait_permit,
        sink_ready: sink,
        sink_continue: wait_consume,
    };
    let worker = std::thread::spawn(move || {
        execute(&Graph::default_graph(true), 3, &mut backend).unwrap();
        backend.fake
    });
    wait_ready.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(!events
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.node == "measure-solve" || e.node == "apply-ev" || e.node == "delete-cache"));
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 3);
    permit.send(()).unwrap();
    for id in 0..3 {
        assert_eq!(wait_sink.recv_timeout(Duration::from_secs(10)).unwrap(), id);
        assert!(!root.join(format!("{id}.dng")).exists());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2 - id);
        consume.send(()).unwrap();
    }
    let fake = worker.join().unwrap();
    assert_eq!(fake.consumed[2].1, vec![-2., 8.]);
    std::fs::remove_dir(&root).unwrap();
}

#[test]
fn lightweight_reference_fork_reaches_eof_without_image_queue_backpressure() {
    let mut fake = Fake {
        frame_count: 257,
        ..Default::default()
    };
    execute(&Graph::default_graph(true), 257, &mut fake).unwrap();
    assert_eq!(fake.measured.len(), 257);
    assert_eq!(fake.consumed.len(), 257);
    assert!(
        fake.order.iter().position(|s| s == "prepare 256").unwrap()
            < fake.order.iter().position(|s| s == "push 0").unwrap()
    );
}

#[test]
fn malformed_exposure_tables_never_reach_develop() {
    for defect in ["missing", "order", "nonfinite", "hash"] {
        let mut fake = Fake {
            plan_defect: Some(defect),
            ..Default::default()
        };
        assert!(execute(&Graph::default_graph(true), 3, &mut fake)
            .unwrap_err()
            .contains("malformed exposure plan"));
        assert!(fake.deleted.is_empty());
        assert!(fake.consumed.is_empty());
    }
}
#[test]
fn combined_data_and_event_cycle_is_rejected_with_valid_port_types() {
    let graph = include_str!("default.json").replace(
        "\"kind\":\"prepare\",",
        "\"kind\":\"prepare\",\"waits\":[{\"node\":\"apply-ev\",\"scope\":\"frame\"}],",
    );
    assert!(Graph::parse(&graph).unwrap_err().contains("cycle"));
}
