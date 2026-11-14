use super::*;
#[derive(Default)]
struct Fake {
    frame_count: usize,
    measured: Vec<usize>,
    events: Vec<Event>,
    deleted: Vec<usize>,
    consumed: Vec<(usize, Vec<f32>)>,
    order: Vec<String>,
    resizes: Vec<(u32, u32)>,
    zero: bool,
    bad_plan: bool,
    plan_defect: Option<&'static str>,
}
impl Backend for Fake {
    type Reference = String;
    type Measurement = usize;
    type Image = Vec<f32>;
    fn check(&mut self) -> Result<()> {
        Ok(())
    }
    fn source_identity(&self, id: usize) -> String {
        format!("source{id}")
    }
    fn prepare_task(&mut self, id: usize) -> Result<FrameTask<String>> {
        self.order.push(format!("prepare {id}"));
        Ok(Box::new(move || Ok(format!("hash{id}"))))
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
    fn commit_measure(
        &mut self,
        id: usize,
        _: &String,
        measurement: usize,
        _: Duration,
    ) -> Result<()> {
        assert_eq!(measurement, id);
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
    fn develop_task(&mut self, id: usize, _: &String, ev: f64) -> Result<FrameTask<Vec<f32>>> {
        assert!(self
            .events
            .iter()
            .any(|e| e.node == "measure-solve" && e.scope == Scope::Sequence));
        if self.zero {
            assert_eq!(ev, 0.);
        } else {
            assert_eq!(ev, (id % 3) as f64);
        }
        Ok(Box::new(move || {
            Ok(vec![-0.5 * ev.exp2() as f32, 2. * ev.exp2() as f32])
        }))
    }
    fn image_task(
        &mut self,
        kind: Kind,
        _: usize,
        mut image: Vec<f32>,
    ) -> Result<FrameTask<Vec<f32>>> {
        Ok(Box::new(move || {
            if let Kind::Exposure(ev) = kind {
                for x in &mut image {
                    *x *= ev.exp2() as f32;
                }
            }
            Ok(image)
        }))
    }
    fn commit_image(&mut self, kind: Kind, _: usize, _: &Vec<f32>, _: Duration) -> Result<()> {
        if let Kind::Resize { width, height } = kind {
            self.resizes.push((width, height));
            self.order.push(format!("resize {width}x{height}"));
        }
        Ok(())
    }
    fn encode_task(&mut self, _: usize, image: Vec<f32>) -> Result<FrameTask<Vec<f32>>> {
        if !self.order.iter().any(|s| s == "begin") {
            self.order.push("begin".into());
        }
        Ok(Box::new(move || Ok(image)))
    }
    fn commit_encode(&mut self, id: usize, image: Vec<f32>, _: Duration) -> Result<()> {
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
fn explicit_resize_node_stays_between_develop_and_colour() {
    let text = include_str!("default.json").replace(
        "{\"id\":\"color\", \"kind\":\"color\", \"concurrency\":1, \"inputs\":{\"image\":\"apply-ev\"}}",
        "{\"id\":\"resize\",\"kind\":\"resize\",\"width\":16,\"height\":12,\"inputs\":{\"image\":\"apply-ev\"}},{\"id\":\"color\",\"kind\":\"color\",\"inputs\":{\"image\":\"resize\"}}",
    );
    let graph = Graph::parse(&text).unwrap();
    assert_eq!(graph.resize_geometry(), Some((16, 12)));
    let mut fake = Fake::default();
    execute(&graph, 3, &mut fake).unwrap();
    assert_eq!(fake.resizes, vec![(16, 12); 3]);
    for id in 0..3 {
        let resize = fake
            .order
            .iter()
            .position(|entry| entry == "resize 16x12")
            .unwrap();
        let push = fake
            .order
            .iter()
            .position(|entry| entry == &format!("push {id}"))
            .unwrap();
        assert!(resize < push);
    }
    // Resize is scene-linear: it cannot sit after the colour node.
    let after_color = include_str!("default.json").replace(
        "{\"id\":\"encode\", \"kind\":\"encode\", \"concurrency\":1, \"inputs\":{\"image\":\"color\"}}",
        "{\"id\":\"resize\",\"kind\":\"resize\",\"width\":16,\"height\":12,\"inputs\":{\"image\":\"color\"}},{\"id\":\"encode\",\"kind\":\"encode\",\"inputs\":{\"image\":\"resize\"}}",
    );
    let error = Graph::parse(&after_color).unwrap_err();
    assert!(error.contains("type mismatch"), "{error}");
}

#[test]
fn cli_resize_inserts_or_overrides_the_single_explicit_node() {
    let mut graph = Graph::default_graph(true);
    assert_eq!(graph.resize_geometry(), None);
    graph.apply_resize(16, 12).unwrap();
    assert_eq!(graph.resize_geometry(), Some((16, 12)));
    let mut fake = Fake::default();
    execute(&graph, 3, &mut fake).unwrap();
    assert_eq!(fake.resizes, vec![(16, 12); 3]);

    graph.apply_resize(8, 6).unwrap();
    assert_eq!(graph.resize_geometry(), Some((8, 6)));
    let mut fake = Fake::default();
    execute(&graph, 3, &mut fake).unwrap();
    assert_eq!(fake.resizes, vec![(8, 6); 3]);

    let duplicated = include_str!("default.json").replace(
        "{\"id\":\"color\", \"kind\":\"color\", \"concurrency\":1, \"inputs\":{\"image\":\"apply-ev\"}}",
        "{\"id\":\"resize-one\",\"kind\":\"resize\",\"width\":16,\"height\":12,\"inputs\":{\"image\":\"apply-ev\"}},{\"id\":\"resize-two\",\"kind\":\"resize\",\"width\":8,\"height\":6,\"inputs\":{\"image\":\"resize-one\"}},{\"id\":\"color\",\"kind\":\"color\",\"inputs\":{\"image\":\"resize-two\"}}",
    );
    assert!(Graph::parse(&duplicated)
        .unwrap_err()
        .contains("at most one resize"));
}

#[test]
fn encode_node_owns_the_bounded_encoder_configuration() {
    let text = include_str!("default.json").replace(
        "\"kind\":\"encode\",",
        "\"kind\":\"encode\",\"encoder\":{\"encoder\":\"ffv1\",\"container\":\"matroska\",\"pix_fmt\":\"yuv444p10le\"},",
    );
    let graph = Graph::parse(&text).unwrap();
    let patch = graph.encoder_patch().expect("encoder patch");
    assert_eq!(patch.encoder.as_deref(), Some("ffv1"));
    assert_eq!(patch.container, Some(crate::encoder::Container::Matroska));
    // The default graph has no encoder block and inherits built-in defaults.
    assert!(Graph::default_graph(true).encoder_patch().is_none());

    for (text, marker) in [
        (
            include_str!("default.json").replace(
                "\"kind\":\"color\",",
                "\"kind\":\"color\",\"encoder\":{\"encoder\":\"ffv1\"},",
            ),
            "only belongs to encode",
        ),
        (
            include_str!("default.json")
                .replace("\"kind\":\"color\",", "\"kind\":\"color\",\"width\":10,"),
            "only belong to resize",
        ),
        (
            include_str!("default.json").replace(
                "\"kind\":\"encode\",",
                "\"kind\":\"encode\",\"encoder\":{\"scale\":2},",
            ),
            "unknown encoder field",
        ),
        (
            include_str!("default.json").replace(
                "\"kind\":\"encode\",",
                "\"kind\":\"encode\",\"encoder\":{\"options\":[\"-vf\",\"scale\"]},",
            ),
            "not supported",
        ),
    ] {
        let error = Graph::parse(&text).unwrap_err();
        assert!(error.contains(marker), "{error}");
    }
    let missing = include_str!("default.json").replace(
        "{\"id\":\"color\", \"kind\":\"color\", \"concurrency\":1, \"inputs\":{\"image\":\"apply-ev\"}}",
        "{\"id\":\"resize\",\"kind\":\"resize\",\"width\":16,\"inputs\":{\"image\":\"apply-ev\"}},{\"id\":\"color\",\"kind\":\"color\",\"inputs\":{\"image\":\"resize\"}}",
    );
    assert!(Graph::parse(&missing)
        .unwrap_err()
        .contains("positive integer height"));
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
        measurement_continue: Option<mpsc::Receiver<()>>,
        sink_ready: mpsc::SyncSender<usize>,
        sink_continue: Arc<Mutex<mpsc::Receiver<()>>>,
    }
    impl Backend for Gated {
        type Reference = String;
        type Measurement = usize;
        type Image = Vec<f32>;
        fn check(&mut self) -> Result<()> {
            Ok(())
        }
        fn source_identity(&self, id: usize) -> String {
            self.fake.source_identity(id)
        }
        fn prepare_task(&mut self, id: usize) -> Result<FrameTask<String>> {
            std::fs::write(self.root.join(format!("{id}.dng")), b"generated").unwrap();
            self.fake.prepare_task(id)
        }
        fn commit_prepare(&mut self, _: usize, _: &String, _: Duration) -> Result<()> {
            Ok(())
        }
        fn hash<'a>(&self, r: &'a String) -> &'a str {
            r
        }
        fn measure_task(&mut self, id: usize, _: &String) -> Result<FrameTask<usize>> {
            if id == 2 {
                let ready = self.measurement_ready.clone();
                let proceed = self.measurement_continue.take().unwrap();
                return Ok(Box::new(move || {
                    ready.send(()).unwrap();
                    proceed.recv_timeout(Duration::from_secs(10)).unwrap();
                    Ok(id)
                }));
            }
            Ok(Box::new(move || Ok(id)))
        }
        fn commit_measure(
            &mut self,
            id: usize,
            r: &String,
            value: usize,
            elapsed: Duration,
        ) -> Result<()> {
            self.fake.commit_measure(id, r, value, elapsed)
        }
        fn solve(&mut self) -> Result<Vec<Gain>> {
            self.fake.solve()
        }
        fn develop_task(&mut self, id: usize, r: &String, ev: f64) -> Result<FrameTask<Vec<f32>>> {
            self.fake.develop_task(id, r, ev)
        }
        fn image_task(
            &mut self,
            kind: Kind,
            id: usize,
            i: Vec<f32>,
        ) -> Result<FrameTask<Vec<f32>>> {
            self.fake.image_task(kind, id, i)
        }
        fn encode_task(&mut self, id: usize, i: Vec<f32>) -> Result<FrameTask<Vec<f32>>> {
            let ready = self.sink_ready.clone();
            let gate = self.sink_continue.clone();
            Ok(Box::new(move || {
                ready.send(id).unwrap();
                gate.lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
                Ok(i)
            }))
        }
        fn commit_encode(&mut self, id: usize, i: Vec<f32>, elapsed: Duration) -> Result<()> {
            self.fake.commit_encode(id, i, elapsed)
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
        measurement_continue: Some(wait_permit),
        sink_ready: sink,
        sink_continue: Arc::new(Mutex::new(wait_consume)),
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
        assert!(std::fs::read_dir(&root).unwrap().count() <= 2 - id);
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

#[test]
fn indirect_sequence_wait_rejects_before_backend_side_effects() {
    let configured = include_str!("default.json").replace(
        "\"kind\":\"color\",",
        "\"kind\":\"color\",\"waits\":[{\"node\":\"delete-cache\",\"scope\":\"sequence\"}],",
    );
    for text in [
        configured.clone(),
        configured.replace(
            "\"kind\":\"solve\", \"concurrency\":8",
            "\"kind\":\"zero\", \"concurrency\":1",
        ),
    ] {
        let mut backend = Fake::default();
        let parsed = Graph::parse(&text);
        if let Ok(graph) = &parsed {
            let _ = execute(graph, 3, &mut backend);
        }
        assert!(
            backend.order.is_empty(),
            "configuration reached backend operations: {:?}",
            backend.order
        );
        assert!(backend.measured.is_empty());
        assert!(backend.deleted.is_empty());
        assert!(backend.events.is_empty());
        let diagnostic = parsed.expect_err("reject the bounded-draining conflict during parse");
        for name in ["sequence wait", "color", "delete-cache", "apply-ev"] {
            assert!(diagnostic.contains(name), "{diagnostic}");
        }
    }
}

#[test]
fn frame_wait_on_delete_completion_still_drains_images() {
    let text = include_str!("default.json").replace(
        "\"kind\":\"color\",",
        "\"kind\":\"color\",\"waits\":[{\"node\":\"delete-cache\",\"scope\":\"frame\"}],",
    );
    let mut backend = Fake::default();
    execute(&Graph::parse(&text).unwrap(), 3, &mut backend).unwrap();
    assert_eq!(backend.deleted, vec![0, 1, 2]);
    assert_eq!(backend.consumed.len(), 3);
}

fn assert_delete_reader_dependencies_reject_before_backend_work(
    configured: &str,
    diagnostic_parts: &[&str],
) {
    for (text, zero) in [
        (configured.to_owned(), false),
        (
            configured.replace("\"kind\": \"solve\"", "\"kind\": \"zero\""),
            true,
        ),
    ] {
        for count in [3, 1] {
            let mut backend = Fake {
                frame_count: count,
                zero,
                ..Default::default()
            };
            let parsed = Graph::parse(&text);
            if let Ok(graph) = &parsed {
                let _ = execute(graph, count, &mut backend);
            }
            assert!(
                backend.order.is_empty(),
                "configuration reached backend operations: {:?}",
                backend.order
            );
            assert!(backend.measured.is_empty());
            assert!(backend.deleted.is_empty());
            assert!(backend.consumed.is_empty());
            assert!(backend.events.is_empty());
            let diagnostic =
                parsed.expect_err("reject implicit Delete reader conflict during parse");
            for part in diagnostic_parts {
                assert!(diagnostic.contains(part), "{diagnostic}");
            }
        }
    }
}

#[test]
fn delete_reader_dependencies_reject_develop_wait_before_backend_work() {
    assert_delete_reader_dependencies_reject_before_backend_work(
        include_str!("../../tests/fixtures/delete_reader_dependencies/develop_waits_delete.json"),
        &["cycle"],
    );
}

#[test]
fn delete_reader_dependencies_reject_sequence_wait_before_backend_work() {
    assert_delete_reader_dependencies_reject_before_backend_work(
        include_str!(
            "../../tests/fixtures/delete_reader_dependencies/color_waits_delete_sequence.json"
        ),
        &["sequence wait", "color", "delete-cache", "apply-ev"],
    );
}

#[test]
fn delete_reader_dependencies_allow_frame_scoped_color_wait() {
    let text = include_str!(
        "../../tests/fixtures/delete_reader_dependencies/color_waits_delete_sequence.json"
    )
    .replace("\"scope\": \"sequence\"", "\"scope\": \"frame\"");
    let mut backend = Fake::default();
    execute(&Graph::parse(&text).unwrap(), 3, &mut backend).unwrap();
    assert_eq!(backend.deleted, vec![0, 1, 2]);
    assert_eq!(backend.consumed.len(), 3);
    for id in 0..3 {
        let position = |name| {
            backend
                .events
                .iter()
                .position(|e| e.node == name && e.frame_id == Some(id))
                .unwrap()
        };
        assert!(position("apply-ev") < position("delete-cache"));
        assert!(position("delete-cache") < position("color"));
        assert!(position("color") < position("encode"));
    }
}
