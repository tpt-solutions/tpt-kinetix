//! Top-level pipeline orchestrator.
//!
//! [`Pipeline`] wires an ordered list of [`Stage`]s together using bounded
//! `crossbeam-channel` channels, then spawns each stage's worker thread.

use std::thread::JoinHandle;

use crossbeam_channel::Sender;
use tpt_kinetix_core::error::KinetixError;

use crate::{
    channel::{PipelineMessage, DEFAULT_CAPACITY},
    stage::Stage,
};

/// The assembled Kinetix processing pipeline.
///
/// Build a pipeline with [`Pipeline::new`], attach stages via
/// [`Pipeline::add_stage`], and execute it with [`Pipeline::run`] or
/// [`Pipeline::run_to_completion`].
pub struct Pipeline {
    stages: Vec<Box<dyn Stage>>,
    channel_capacity: usize,
}

impl Pipeline {
    /// Creates a new empty pipeline with the default channel capacity.
    pub fn new() -> Self {
        Self {
            stages: Vec::new(),
            channel_capacity: DEFAULT_CAPACITY,
        }
    }

    /// Creates a new empty pipeline with the given inter-stage channel capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            stages: Vec::new(),
            channel_capacity: capacity,
        }
    }

    /// Appends a stage to the end of the pipeline (builder pattern).
    pub fn add_stage(mut self, stage: impl Stage + 'static) -> Self {
        self.stages.push(Box::new(stage));
        self
    }

    /// Wires all stages together and spawns their worker threads.
    ///
    /// Returns the [`JoinHandle`]s for all stage threads.  Stages are wired
    /// left-to-right in the order they were added.  The first stage receives a
    /// dummy (immediately-disconnected) input receiver; the last stage receives
    /// a dummy (immediately-dropped) output sender.
    pub fn run(self) -> Result<Vec<JoinHandle<Result<(), KinetixError>>>, KinetixError> {
        let stages = self.stages;
        let n = stages.len();

        if n == 0 {
            return Ok(vec![]);
        }

        // Build the per-stage input receivers and output senders.
        //
        // Layout for N stages (0-indexed):
        //   stage 0  : dummy_rx  ──> stage 0 ──> inter[0].tx
        //   stage i  : inter[i-1].rx ──> stage i ──> inter[i].tx
        //   stage N-1: inter[N-2].rx ──> stage N-1 ──> dummy_tx

        let mut input_rxs: Vec<crossbeam_channel::Receiver<PipelineMessage>> =
            Vec::with_capacity(n);
        let mut output_txs: Vec<Sender<PipelineMessage>> = Vec::with_capacity(n);

        // Dummy input for the first stage (sender is dropped immediately, so
        // the receiver is permanently disconnected — DemuxStage ignores it).
        let (_, dummy_first_rx) = crossbeam_channel::bounded::<PipelineMessage>(1);
        input_rxs.push(dummy_first_rx);

        // N-1 inter-stage channels.
        for _ in 0..n.saturating_sub(1) {
            let (tx, rx) = crossbeam_channel::bounded::<PipelineMessage>(self.channel_capacity);
            output_txs.push(tx);
            input_rxs.push(rx);
        }

        // Dummy output for the last stage (receiver is dropped immediately, so
        // sends fail silently — SinkStage ignores its output channel).
        let (dummy_last_tx, _) = crossbeam_channel::bounded::<PipelineMessage>(1);
        output_txs.push(dummy_last_tx);

        debug_assert_eq!(input_rxs.len(), n);
        debug_assert_eq!(output_txs.len(), n);

        // Spawn each stage with its wired channel endpoints.
        let handles: Vec<JoinHandle<Result<(), KinetixError>>> = stages
            .into_iter()
            .zip(input_rxs)
            .zip(output_txs)
            .map(|((stage, rx), tx)| stage.spawn(rx, tx))
            .collect();

        Ok(handles)
    }

    /// Wires all stages, spawns their threads, and blocks until every thread
    /// finishes.
    ///
    /// Returns the first error encountered, if any.
    pub fn run_to_completion(self) -> Result<(), KinetixError> {
        let handles = self.run()?;
        for handle in handles {
            handle
                .join()
                .map_err(|_| KinetixError::Parse("a stage thread panicked".into()))??;
        }
        Ok(())
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}
