//! Top-level pipeline orchestrator.
//!
//! Wires together the demux, decode, and filter stages, manages their
//! lifecycles, and exposes a simple `run` API.
//!
//! TODO (Phase 5): Full implementation.

use kinetix_core::error::KinetixError;

/// The assembled Kinetix processing pipeline.
pub struct Pipeline {
    _priv: (),
}

impl Pipeline {
    /// Create a new empty pipeline.
    pub fn new() -> Self {
        Self { _priv: () }
    }

    /// Run the pipeline to completion (blocking).
    ///
    /// TODO (Phase 5): Accept input source + output sink + stage configuration.
    pub fn run(&mut self) -> Result<(), KinetixError> {
        Err(KinetixError::Unsupported(
            "Pipeline not yet implemented (Phase 5)".into(),
        ))
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}
