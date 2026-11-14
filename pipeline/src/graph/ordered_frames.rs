//! Bounded frame admission, completion and ordered commit. A committed result
//! still owns its slot until its consumer acknowledges delivery.
use super::Result;
use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub type FrameTask<T> = Box<dyn FnOnce() -> Result<T> + Send + 'static>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskKey {
    pub node: String,
    pub frame_id: usize,
    pub source: String,
}
pub struct Completion<T> {
    pub key: TaskKey,
    pub value: T,
    pub elapsed: Duration,
}
struct Slot<T> {
    key: TaskKey,
    worker: Option<JoinHandle<()>>,
    completed: Option<Completion<T>>,
    committed: bool,
}
/// All queues and completed results are charged to the same admission slots.
/// There is no unbounded input queue or per-wave barrier.
pub struct OrderedStage<T> {
    capacity: usize,
    accepted: usize,
    committed: usize,
    delivered: usize,
    slots: BTreeMap<usize, Slot<T>>,
    send: Sender<(TaskKey, Result<T>, Duration)>,
    receive: Receiver<(TaskKey, Result<T>, Duration)>,
    wake: Sender<()>,
}
impl<T: Send + 'static> OrderedStage<T> {
    pub fn new(capacity: usize, wake: Sender<()>) -> Self {
        assert!((1..=64).contains(&capacity));
        let (send, receive) = mpsc::channel();
        Self {
            capacity,
            accepted: 0,
            committed: 0,
            delivered: 0,
            slots: BTreeMap::new(),
            send,
            receive,
            wake,
        }
    }
    pub fn outstanding(&self) -> usize {
        self.accepted - self.delivered
    }
    pub fn has_capacity(&self) -> bool {
        self.outstanding() < self.capacity
    }
    pub fn accepted(&self) -> usize {
        self.accepted
    }
    pub fn submit(&mut self, key: TaskKey, task: FrameTask<T>) -> Result<()> {
        if !self.has_capacity() || key.frame_id != self.accepted {
            return Err("graph: full frame stage or nonsequential admission identity".into());
        }
        let send = self.send.clone();
        let wake = self.wake.clone();
        let worker_key = key.clone();
        let worker = std::thread::Builder::new()
            .name(format!("{}-{}", key.node, key.frame_id))
            .spawn(move || {
                let started = Instant::now();
                let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task))
                    .unwrap_or_else(|_| Err("frame worker panicked".into()));
                let _ = send.send((worker_key, value, started.elapsed()));
                let _ = wake.send(());
            })
            .map_err(|e| format!("graph: cannot start frame worker: {e}"))?;
        self.slots.insert(
            key.frame_id,
            Slot {
                key,
                worker: Some(worker),
                completed: None,
                committed: false,
            },
        );
        self.accepted += 1;
        Ok(())
    }
    /// Observe every completion, including failures beyond a blocked head.
    pub fn poll(&mut self) -> Result<()> {
        while let Ok((key, result, elapsed)) = self.receive.try_recv() {
            let slot = self
                .slots
                .get_mut(&key.frame_id)
                .ok_or("graph: unaccepted completion")?;
            if slot.key != key || slot.completed.is_some() || slot.committed {
                return Err("graph: duplicate/foreign completion identity".into());
            }
            if let Some(worker) = slot.worker.take() {
                worker
                    .join()
                    .map_err(|_| "graph: completion worker panicked".to_string())?;
            }
            let value = result.map_err(|e| {
                format!(
                    "node {} frame {} source {} failed: {e}",
                    key.node, key.frame_id, key.source
                )
            })?;
            slot.completed = Some(Completion {
                key,
                value,
                elapsed,
            });
        }
        Ok(())
    }
    pub fn commit_next(&mut self) -> Option<Completion<T>> {
        let slot = self.slots.get_mut(&self.committed)?;
        let output = slot.completed.take()?;
        slot.committed = true;
        self.committed += 1;
        Some(output)
    }
    pub fn delivered(&mut self, id: usize) -> Result<()> {
        if id != self.delivered || !self.slots.get(&id).is_some_and(|s| s.committed) {
            return Err("graph: delivery before commit or wrong frame identity".into());
        }
        self.slots.remove(&id);
        self.delivered += 1;
        Ok(())
    }
}
impl<T> Drop for OrderedStage<T> {
    fn drop(&mut self) {
        // The graph cancels its run owner before dropping stages on failure.
        for slot in self.slots.values_mut() {
            if let Some(worker) = slot.worker.take() {
                let _ = worker.join();
            }
        }
    }
}
