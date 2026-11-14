//! Prepare-only batch slot. Other stages retain their per-frame capacities.
use super::ordered_frames::{FrameTask, OrderedStage, StageStats, TaskKey};
use super::{PreparedBatch, PreparedReference, Result};
use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::time::Duration;

pub(super) struct BatchCompletion<R> {
    pub batch_id: usize,
    pub ids: Vec<usize>,
    pub frames: Vec<PreparedReference<R>>,
    pub elapsed: Duration,
    pub diagnostics: String,
}
pub(super) struct PrepareBatches<R> {
    limit: usize,
    stage: OrderedStage<PreparedBatch<R>>,
    expected: Option<Vec<TaskKey>>,
    accepted_frames: usize,
}
impl<R: Send + 'static> PrepareBatches<R> {
    pub fn new(limit: usize, wake: Sender<()>) -> Self {
        Self {
            limit,
            stage: OrderedStage::new(1, wake),
            expected: None,
            accepted_frames: 0,
        }
    }
    pub fn accepted(&self) -> usize {
        self.accepted_frames
    }
    pub fn has_capacity(&self) -> bool {
        self.stage.has_capacity()
    }
    pub fn outstanding(&self) -> usize {
        self.stage.outstanding()
    }
    /// Counts are batch tasks; the separate completion log records frame counts.
    pub fn stats(&self) -> StageStats {
        self.stage.stats()
    }
    pub fn poll(&mut self) -> Result<()> {
        self.stage.poll()
    }
    pub fn submit(&mut self, keys: Vec<TaskKey>, task: FrameTask<PreparedBatch<R>>) -> Result<()> {
        if !self.has_capacity()
            || keys.is_empty()
            || keys.len() > self.limit
            || keys
                .iter()
                .enumerate()
                .any(|(offset, key)| key.frame_id != self.accepted_frames + offset)
        {
            return Err("graph: invalid prepare batch admission/capacity".into());
        }
        let mut key = keys[0].clone();
        key.frame_id = self.stage.accepted(); // OrderedStage orders logical tasks.
        self.stage.submit(key, task)?;
        self.accepted_frames += keys.len();
        self.expected = Some(keys);
        Ok(())
    }
    pub fn commit_next(&mut self) -> Result<Option<BatchCompletion<R>>> {
        let Some(output) = self.stage.commit_next() else {
            return Ok(None);
        };
        let expected = self
            .expected
            .as_ref()
            .ok_or("graph: unaccepted prepare batch")?;
        if output.value.frames.len() != expected.len() {
            return Err("graph: prepare batch result coverage mismatch".into());
        }
        let mut found = BTreeMap::new();
        // Validate ALL identities before exposing even the first reference.
        for frame in output.value.frames {
            let key = expected
                .iter()
                .find(|key| key.frame_id == frame.frame_id)
                .ok_or("graph: prepare batch has extra frame ID")?;
            if key.source != frame.source || found.insert(frame.frame_id, frame).is_some() {
                return Err("graph: prepare batch duplicate/source identity mismatch".into());
            }
        }
        let mut frames = Vec::with_capacity(expected.len());
        for key in expected {
            frames.push(
                found
                    .remove(&key.frame_id)
                    .ok_or("graph: prepare batch missing frame ID")?,
            );
        }
        Ok(Some(BatchCompletion {
            batch_id: output.key.frame_id,
            ids: expected.iter().map(|k| k.frame_id).collect(),
            frames,
            elapsed: output.elapsed,
            diagnostics: output.value.diagnostics,
        }))
    }
    pub fn delivered(&mut self) -> Result<()> {
        // All prepared references have been materialized; downstream need not
        // release them before another native batch is allowed to start.
        self.stage.delivered(self.stage.stats().delivered)?;
        self.expected = None;
        Ok(())
    }
}
