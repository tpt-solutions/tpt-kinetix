//! Pipeline stage trait and standard stage types.
//!
//! TODO (Phase 5): Implement demux, decode, and filter stages.

use kinetix_core::error::KinetixError;

/// A single processing stage in the Kinetix pipeline.
///
/// Each stage owns its worker thread(s) and communicates with adjacent stages
/// via bounded `crossbeam-channel` channels.
pub trait Stage: Send {
    /// Human-readable stage name for logging/diagnostics.
    fn name(&self) -> &str;

    /// Start the stage's worker thread(s).  Returns an error if the stage is
    /// already running or cannot acquire resources.
    fn start(&mut self) -> Result<(), KinetixError>;

    /// Request a graceful shutdown and block until all in-flight work drains.
    fn stop(&mut self) -> Result<(), KinetixError>;
}
