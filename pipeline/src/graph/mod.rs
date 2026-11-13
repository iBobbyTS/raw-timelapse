//! A synchronous typed DAG. Lightweight references may accumulate through EOF;
//! image outputs have one slot, with a single consumer and downstream-first
//! draining. Events are distinct from data, and deletion runs before image
//! consumers on each scheduling turn.
use crate::json::{self, Json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    Source,
    Frames,
    Plan,
    Linear,
    Color,
    None,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Source,
    Prepare,
    Solve,
    Zero,
    Barrier,
    References,
    Develop,
    Identity,
    Exposure(f64),
    Color,
    Encode,
    Delete,
}
impl Kind {
    fn output(self) -> Type {
        match self {
            Self::Source => Type::Source,
            Self::Prepare | Self::Barrier | Self::References => Type::Frames,
            Self::Solve | Self::Zero => Type::Plan,
            Self::Develop | Self::Identity | Self::Exposure(_) => Type::Linear,
            Self::Color => Type::Color,
            _ => Type::None,
        }
    }
    fn ports(self) -> Vec<(&'static str, Type)> {
        match self {
            Self::Source => vec![],
            Self::Prepare => vec![("source", Type::Source)],
            Self::Solve | Self::Zero | Self::Barrier | Self::References | Self::Delete => {
                vec![("frames", Type::Frames)]
            }
            Self::Develop => vec![("frames", Type::Frames), ("plan", Type::Plan)],
            Self::Identity | Self::Exposure(_) | Self::Color => vec![("image", Type::Linear)],
            Self::Encode => vec![("image", Type::Color)],
        }
    }
    fn sequence_only(self) -> bool {
        matches!(self, Self::Solve | Self::Zero | Self::Barrier)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Frame,
    Sequence,
}
#[derive(Clone, Debug)]
pub struct Wait {
    pub node: usize,
    pub scope: Scope,
}
#[derive(Clone, Debug)]
pub struct Node {
    pub id: String,
    pub kind: Kind,
    pub inputs: BTreeMap<String, usize>,
    pub waits: Vec<Wait>,
}
#[derive(Clone, Debug)]
pub struct Graph {
    nodes: Vec<Node>,
    order: Vec<usize>,
}
fn string<'a>(j: &'a Json, key: &str) -> Result<&'a str> {
    j.get(key)
        .and_then(Json::as_str)
        .ok_or_else(|| format!("graph: missing string {key}"))
}
fn object(j: &Json) -> Result<&[(String, Json)]> {
    if let Json::Object(v) = j {
        Ok(v)
    } else {
        Err("graph: expected object".into())
    }
}
fn keys(j: &Json, allowed: &[&str]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for (key, _) in object(j)? {
        if !allowed.contains(&key.as_str()) || !seen.insert(key) {
            return Err(format!("graph: unknown/duplicate field {key}"));
        }
    }
    Ok(())
}
impl Graph {
    pub fn default_graph(measure: bool) -> Self {
        let mut graph = Self::parse(include_str!("default.json")).expect("valid built-in graph");
        if !measure {
            for node in &mut graph.nodes {
                if node.kind == Kind::Solve {
                    node.kind = Kind::Zero;
                }
            }
        }
        graph
    }
    pub fn parse(text: &str) -> Result<Self> {
        let root = json::parse(text)?;
        keys(&root, &["nodes"])?;
        let raw = root
            .get("nodes")
            .and_then(Json::as_array)
            .ok_or("graph: nodes must be an array")?;
        let mut ids = BTreeMap::new();
        for (i, n) in raw.iter().enumerate() {
            let id = string(n, "id")?;
            if id.is_empty()
                || !id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                || ids.insert(id, i).is_some()
            {
                return Err("graph: invalid/duplicate node ID".into());
            }
        }
        let resolve = |id: &str| {
            ids.get(id)
                .copied()
                .ok_or_else(|| format!("graph: undeclared node {id}"))
        };
        let mut nodes = Vec::new();
        for n in raw {
            keys(n, &["id", "kind", "inputs", "waits", "ev"])?;
            let kind = match string(n, "kind")? {
                "source" => Kind::Source,
                "prepare" => Kind::Prepare,
                "solve" => Kind::Solve,
                "zero" => Kind::Zero,
                "barrier" => Kind::Barrier,
                "references" => Kind::References,
                "develop" => Kind::Develop,
                "identity" => Kind::Identity,
                "exposure" => Kind::Exposure(
                    n.get("ev")
                        .and_then(Json::as_f64)
                        .filter(|e| e.is_finite() && e.exp2().is_finite() && e.exp2() > 0.)
                        .ok_or("graph: exposure requires finite representable ev")?,
                ),
                "color" => Kind::Color,
                "encode" => Kind::Encode,
                "delete" => Kind::Delete,
                k => return Err(format!("graph: unknown built-in {k}")),
            };
            if n.get("ev").is_some() && !matches!(kind, Kind::Exposure(_)) {
                return Err("graph: ev only belongs to exposure".into());
            }
            let mut inputs = BTreeMap::new();
            if let Some(input) = n.get("inputs") {
                for (port, id) in object(input)? {
                    if inputs
                        .insert(
                            port.clone(),
                            resolve(id.as_str().ok_or("graph: input must name a node")?)?,
                        )
                        .is_some()
                    {
                        return Err("graph: duplicate input port".into());
                    }
                }
            }
            let mut waits = Vec::new();
            if let Some(w) = n.get("waits") {
                for w in w.as_array().ok_or("graph: waits must be an array")? {
                    keys(w, &["node", "scope"])?;
                    waits.push(Wait {
                        node: resolve(string(w, "node")?)?,
                        scope: match string(w, "scope")? {
                            "frame" => Scope::Frame,
                            "sequence" => Scope::Sequence,
                            _ => return Err("graph: wrong event scope".into()),
                        },
                    });
                }
            }
            nodes.push(Node {
                id: string(n, "id")?.into(),
                kind,
                inputs,
                waits,
            });
        }
        let mut graph = Self {
            nodes,
            order: vec![],
        };
        graph.validate()?;
        Ok(graph)
    }
    pub fn disable_measurement(&mut self) {
        for n in &mut self.nodes {
            if n.kind == Kind::Solve {
                n.kind = Kind::Zero;
            }
        }
    }
    fn validate(&mut self) -> Result<()> {
        for kind in [
            Kind::Source,
            Kind::Prepare,
            Kind::Develop,
            Kind::Color,
            Kind::Encode,
        ] {
            if self.nodes.iter().filter(|n| n.kind == kind).count() != 1 {
                return Err(format!("graph: exactly one {kind:?} required"));
            }
        }
        if self
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, Kind::Solve | Kind::Zero))
            .count()
            != 1
        {
            return Err("graph: exactly one exposure plan producer required".into());
        }
        let mut consumers = vec![0; self.nodes.len()];
        for n in &self.nodes {
            let ports = n.kind.ports();
            if n.inputs.len() != ports.len() {
                return Err(format!("graph: {} input port count mismatch", n.id));
            }
            for (port, ty) in ports {
                let input = *n
                    .inputs
                    .get(port)
                    .ok_or_else(|| format!("graph: {} missing port {port}", n.id))?;
                if self.nodes[input].kind.output() != ty {
                    return Err(format!("graph: {}.{port} type mismatch", n.id));
                }
                consumers[input] += 1;
            }
            let mut seen = BTreeSet::new();
            for w in &n.waits {
                if !seen.insert((w.node, w.scope == Scope::Frame)) {
                    return Err("graph: duplicate event dependency".into());
                }
                if w.scope == Scope::Frame && self.nodes[w.node].kind.sequence_only() {
                    return Err("graph: frame event requested from sequence-only node".into());
                }
                if n.kind.sequence_only() && w.scope == Scope::Frame {
                    return Err("graph: sequence node requires sequence event scope".into());
                }
            }
            if n.kind == Kind::Delete && n.waits.is_empty() {
                return Err("graph: delete requires an explicit completion event".into());
            }
        }
        for (i, n) in self.nodes.iter().enumerate() {
            if matches!(n.kind.output(), Type::Linear | Type::Color) && consumers[i] != 1 {
                return Err(
                    "graph: an image output requires exactly one consumer (bounded image chain)"
                        .into(),
                );
            }
            if n.kind.output() != Type::None && consumers[i] == 0 {
                return Err(format!("graph: unused output {}", n.id));
            }
        }
        let mut done = BTreeSet::new();
        while done.len() < self.nodes.len() {
            let next = (0..self.nodes.len()).find(|i| {
                !done.contains(i)
                    && self.nodes[*i].inputs.values().all(|v| done.contains(v))
                    && self.nodes[*i].waits.iter().all(|w| done.contains(&w.node))
            });
            if let Some(i) = next {
                done.insert(i);
                self.order.push(i);
            } else {
                return Err("graph: data/event dependency cycle".into());
            }
        }
        // A sequence completion requires its transitive data/event producers
        // to finish every frame. Reject waits that would hold an image slot
        // while any such prerequisite still needs that slot drained.
        for (i, n) in self.nodes.iter().enumerate() {
            if matches!(
                n.kind,
                Kind::Develop | Kind::Identity | Kind::Exposure(_) | Kind::Color | Kind::Encode
            ) {
                for w in n.waits.iter().filter(|w| w.scope == Scope::Sequence) {
                    if let Some(producer) =
                        self.completion_image_ancestor(i, w.node, &mut BTreeSet::new())
                    {
                        return Err(format!(
                            "graph: {} sequence wait on {} would block bounded image draining: completion requires upstream image producer {}",
                            n.id, self.nodes[w.node].id, self.nodes[producer].id
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    fn completion_image_ancestor(
        &self,
        consumer: usize,
        target: usize,
        visited: &mut BTreeSet<usize>,
    ) -> Option<usize> {
        if !visited.insert(target) {
            return None;
        }
        if self.image_ancestor(consumer, target) {
            return Some(target);
        }
        // Even a frame-scoped prerequisite must cover the complete inventory
        // before its dependent node can publish sequence completion.
        self.nodes[target]
            .inputs
            .values()
            .chain(self.nodes[target].waits.iter().map(|wait| &wait.node))
            .find_map(|producer| self.completion_image_ancestor(consumer, *producer, visited))
    }
    fn image_ancestor(&self, i: usize, wanted: usize) -> bool {
        self.nodes[i].inputs.values().any(|p| {
            matches!(self.nodes[*p].kind.output(), Type::Linear | Type::Color)
                && (*p == wanted || self.image_ancestor(*p, wanted))
        })
    }
}

/// A sealing protocol for stream/sequence inputs. Counts alone never establish
/// coverage; duplicate tokens, duplicate EOF and missing IDs are errors.
pub struct Seal<T> {
    count: usize,
    values: BTreeMap<usize, T>,
    eof: bool,
}
impl<T> Seal<T> {
    pub fn new(count: usize) -> Self {
        Self {
            count,
            values: BTreeMap::new(),
            eof: false,
        }
    }
    pub fn push(&mut self, id: usize, value: T) -> Result<()> {
        if self.eof || id >= self.count || self.values.contains_key(&id) {
            return Err("graph: duplicate/late/out-of-range frame ID".into());
        }
        self.values.insert(id, value);
        Ok(())
    }
    pub fn end(&mut self, count: usize) -> Result<()> {
        if self.eof || count != self.count || self.values.len() != count {
            return Err("graph: duplicate EOF or missing frame IDs/count".into());
        }
        self.eof = true;
        Ok(())
    }
    pub fn ordered(&self) -> Result<Vec<&T>> {
        if !self.eof {
            return Err("graph: input has no EOF".into());
        }
        Ok(self.values.values().collect())
    }
}
#[derive(Clone, Debug)]
pub struct Gain {
    pub frame_id: usize,
    pub source_sha256: String,
    pub ev: f64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub node: String,
    pub frame_id: Option<usize>,
    pub scope: Scope,
}
/// Backends own actual I/O; scheduler owns invocation order and event emission.
pub trait Backend {
    type Reference: Clone;
    type Image;
    fn check(&mut self) -> Result<()>;
    fn prepare(&mut self, id: usize) -> Result<Self::Reference>;
    fn hash<'a>(&self, r: &'a Self::Reference) -> &'a str;
    fn measure(&mut self, id: usize, r: &Self::Reference) -> Result<()>;
    fn solve(&mut self) -> Result<Vec<Gain>>;
    fn develop(&mut self, id: usize, r: &Self::Reference, ev: f64) -> Result<Self::Image>;
    fn exposure(&mut self, image: &mut Self::Image, ev: f64) -> Result<()>;
    fn color(&mut self, image: Self::Image) -> Result<Self::Image>;
    fn begin(&mut self) -> Result<()>;
    fn push(&mut self, id: usize, image: Self::Image) -> Result<()>;
    fn end(&mut self) -> Result<()>;
    fn delete(&mut self, id: usize, r: &Self::Reference) -> Result<()>;
    fn event(&mut self, event: &Event) -> Result<()>;
}
struct State<R, I> {
    refs: Vec<R>,
    plan: Option<Arc<Vec<Gain>>>,
    image: Option<(usize, I)>,
    next: usize,
    frames: BTreeSet<usize>,
    closed: bool,
    begun: bool,
}
impl<R, I> Default for State<R, I> {
    fn default() -> Self {
        Self {
            refs: vec![],
            plan: None,
            image: None,
            next: 0,
            frames: BTreeSet::new(),
            closed: false,
            begun: false,
        }
    }
}
fn ready<R, I>(node: &Node, states: &[State<R, I>], id: Option<usize>) -> bool {
    node.waits.iter().all(|w| match w.scope {
        Scope::Sequence => states[w.node].closed,
        Scope::Frame => id.map_or(states[w.node].closed, |id| {
            states[w.node].frames.contains(&id)
        }),
    })
}
fn emit<B: Backend>(
    b: &mut B,
    node: &Node,
    state: &mut State<B::Reference, B::Image>,
    id: Option<usize>,
) -> Result<()> {
    b.check()?;
    if let Some(id) = id {
        if !state.frames.insert(id) {
            return Err("graph: duplicate completion".into());
        }
    } else {
        if state.closed {
            return Err("graph: duplicate sequence completion".into());
        }
        state.closed = true;
    }
    b.event(&Event {
        node: node.id.clone(),
        frame_id: id,
        scope: if id.is_some() {
            Scope::Frame
        } else {
            Scope::Sequence
        },
    })
}
/// Execute all nodes, retaining only O(N) references/features and one image per
/// active chain slot. The downstream-first sweep prevents filling those slots;
/// after develop, deletion is dispatched before any color/encoder operation.
pub fn execute<B: Backend>(graph: &Graph, count: usize, b: &mut B) -> Result<()> {
    let mut inventory = Seal::new(count);
    for id in 0..count {
        inventory.push(id, ())?;
    }
    inventory.end(count)?;
    execute_sealed(graph, &inventory, b)
}
/// External producers must seal their full ID inventory before any expensive
/// operation. Out-of-order arrival is accepted; output remains source indexed.
pub fn execute_sealed<B: Backend>(graph: &Graph, inventory: &Seal<()>, b: &mut B) -> Result<()> {
    inventory.ordered()?;
    let count = inventory.count;
    if count == 0 {
        return Err("graph: empty inventory".into());
    }
    let mut states: Vec<State<B::Reference, B::Image>> =
        (0..graph.nodes.len()).map(|_| State::default()).collect();
    let deletes: Vec<_> = graph
        .order
        .iter()
        .copied()
        .filter(|i| graph.nodes[*i].kind == Kind::Delete)
        .collect();
    let order: Vec<_> = deletes
        .into_iter()
        .chain(
            graph
                .order
                .iter()
                .rev()
                .copied()
                .filter(|i| graph.nodes[*i].kind != Kind::Delete),
        )
        .collect();
    loop {
        b.check()?;
        if states.iter().all(|s| s.closed) {
            return Ok(());
        }
        let mut progress = false;
        for &i in &order {
            let node = &graph.nodes[i];
            if states[i].closed {
                continue;
            }
            let id = states[i].next;
            if !ready(
                node,
                &states,
                if node.kind.sequence_only() || id == count {
                    None
                } else {
                    Some(id)
                },
            ) {
                continue;
            }
            let input = |port: &str| node.inputs[port];
            let mut frame_done = false;
            let mut close = false;
            match node.kind {
                Kind::Source => {
                    if id < count {
                        states[i].next += 1;
                        frame_done = true;
                    } else {
                        close = true;
                    }
                }
                Kind::Prepare => {
                    let p = input("source");
                    if states[p].frames.contains(&id) {
                        let r = b.prepare(id)?;
                        states[i].refs.push(r);
                        states[i].next += 1;
                        frame_done = true;
                    } else if states[p].closed && id == count {
                        close = true;
                    }
                }
                Kind::References | Kind::Barrier => {
                    let p = input("frames");
                    if node.kind == Kind::Barrier {
                        if states[p].closed {
                            if states[p].refs.len() != count {
                                return Err("graph: barrier EOF coverage mismatch".into());
                            }
                            states[i].refs = states[p].refs.clone();
                            close = true;
                        }
                    } else if let Some(r) = states[p].refs.get(id).cloned() {
                        states[i].refs.push(r);
                        states[i].next += 1;
                        frame_done = true;
                    } else if states[p].closed && id == count {
                        close = true;
                    }
                }
                Kind::Solve | Kind::Zero => {
                    let p = input("frames");
                    // Measurements are incremental, but publishing is gated on
                    // the producer's exact sealed inventory, including EOF.
                    if let Some(r) = states[p].refs.get(id).cloned() {
                        if node.kind == Kind::Solve {
                            b.measure(id, &r)?;
                        }
                        states[i].next += 1;
                        progress = true;
                    } else if states[p].closed {
                        if id != count || states[p].refs.len() != count {
                            return Err("graph: solver EOF coverage mismatch".into());
                        }
                        let plan = if node.kind == Kind::Solve {
                            b.solve()?
                        } else {
                            states[p]
                                .refs
                                .iter()
                                .enumerate()
                                .map(|(frame_id, r)| Gain {
                                    frame_id,
                                    source_sha256: b.hash(r).into(),
                                    ev: 0.,
                                })
                                .collect()
                        };
                        if plan.len() != count
                            || plan.iter().enumerate().any(|(id, g)| {
                                g.frame_id != id
                                    || g.source_sha256 != b.hash(&states[p].refs[id])
                                    || !g.ev.is_finite()
                                    || !g.ev.exp2().is_finite()
                                    || g.ev.exp2() <= 0.
                            })
                        {
                            return Err("graph: malformed exposure plan identity/order/gain".into());
                        }
                        states[i].plan = Some(Arc::new(plan));
                        close = true;
                    }
                }
                Kind::Develop => {
                    let p = input("frames");
                    let plan = input("plan");
                    // One full-resolution image in the complete final chain.
                    let occupied = states.iter().any(|s| s.image.is_some());
                    if !occupied && states[plan].closed && states[i].image.is_none() {
                        if let Some(r) = states[p].refs.get(id).cloned() {
                            let g = &states[plan].plan.as_ref().ok_or("graph: missing plan")?[id];
                            if g.source_sha256 != b.hash(&r) {
                                return Err("graph: EV join identity mismatch".into());
                            }
                            let image = b.develop(id, &r, g.ev)?;
                            states[i].image = Some((id, image));
                            states[i].next += 1;
                            frame_done = true;
                        } else if states[p].closed && id == count {
                            close = true;
                        }
                    }
                }
                Kind::Identity | Kind::Exposure(_) | Kind::Color | Kind::Encode => {
                    let p = input("image");
                    if states[i].image.is_none()
                        && states[p]
                            .image
                            .as_ref()
                            .is_some_and(|(frame, _)| *frame == id)
                    {
                        if node.kind == Kind::Encode && !states[i].begun {
                            b.begin()?;
                            states[i].begun = true;
                        }
                        let (frame, mut image) = states[p].image.take().unwrap();
                        match node.kind {
                            Kind::Exposure(ev) => {
                                b.exposure(&mut image, ev)?;
                                states[i].image = Some((frame, image));
                            }
                            Kind::Color => {
                                states[i].image = Some((frame, b.color(image)?));
                            }
                            Kind::Encode => b.push(frame, image)?,
                            _ => states[i].image = Some((frame, image)),
                        }
                        states[i].next += 1;
                        frame_done = true;
                    } else if states[p].closed && states[p].image.is_none() && id == count {
                        if node.kind == Kind::Encode {
                            b.end()?;
                        }
                        close = true;
                    }
                }
                Kind::Delete => {
                    let p = input("frames");
                    if let Some(r) = states[p].refs.get(id).cloned() {
                        b.delete(id, &r)?;
                        states[i].next += 1;
                        frame_done = true;
                    } else if states[p].closed && id == count {
                        close = true;
                    }
                }
            }
            if frame_done {
                emit(b, node, &mut states[i], Some(id))?;
                progress = true;
            }
            if close {
                emit(b, node, &mut states[i], None)?;
                progress = true;
            }
        }
        if !progress {
            return Err("graph: unsatisfied data/EOF/event dependencies".into());
        }
    }
}
#[cfg(test)]
mod tests;
