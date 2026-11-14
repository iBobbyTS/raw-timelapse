use super::*;

fn configured(kind: &str, value: &str) -> String {
    include_str!("default.json")
        .replace(", \"concurrency\":1", "")
        .replace(
            &format!("\"kind\":\"{kind}\""),
            &format!("\"concurrency\":{value},\"kind\":\"{kind}\""),
        )
}
#[test]
fn concurrency_is_strict_and_capabilities_are_explicit() {
    for value in ["1", "4", "8", "64"] {
        for kind in ["prepare", "solve"] {
            let graph = Graph::parse(&configured(kind, value)).unwrap();
            assert_eq!(
                graph
                    .nodes
                    .iter()
                    .find(|n| match kind {
                        "prepare" => n.kind == Kind::Prepare,
                        _ => n.kind == Kind::Solve,
                    })
                    .unwrap()
                    .concurrency,
                value.parse::<usize>().unwrap()
            );
        }
    }
    for value in [
        "null",
        "\"4\"",
        "true",
        "false",
        "0",
        "-1",
        "1.0",
        "1e0",
        "65",
        "18446744073709551616",
    ] {
        assert!(
            Graph::parse(&configured("prepare", value)).is_err(),
            "{value}"
        );
    }
    assert!(Graph::parse(&configured("prepare", "2,\"concurrency\":3")).is_err());
    for kind in ["source", "zero", "develop", "color", "encode", "delete"] {
        let mut text = configured(kind, "2");
        if kind == "zero" {
            text = text.replace("\"kind\":\"solve\"", "\"kind\":\"zero\",\"concurrency\":2");
        }
        assert!(Graph::parse(&text).is_err(), "{kind}");
    }
}
#[test]
fn solve_zero_normalization_and_resize_preserve_valid_concurrency() {
    let mut graph = Graph::parse(&configured("solve", "8")).unwrap();
    graph.disable_measurement();
    let zero = graph.nodes.iter().find(|n| n.kind == Kind::Zero).unwrap();
    assert_eq!(zero.concurrency, 1);
    graph.apply_resize(8, 6).unwrap();
    let index = graph
        .nodes
        .iter()
        .position(|n| matches!(n.kind, Kind::Resize { .. }))
        .unwrap();
    assert_eq!(graph.nodes[index].concurrency, 1);
    // The override mutates geometry, leaving the declared capability untouched.
    graph.apply_resize(16, 12).unwrap();
    assert_eq!(graph.nodes[index].concurrency, 1);
}
