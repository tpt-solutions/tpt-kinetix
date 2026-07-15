//! Pipeline-specific error types.

use thiserror::Error;

/// Errors that can occur within or between pipeline stages.
#[derive(Debug, Error)]
pub enum PipelineError {
    /// A stage's worker thread returned an error.
    #[error("stage '{stage}' failed: {source}")]
    StageFailed {
        stage: &'static str,
        source: kinetix_core::error::KinetixError,
    },

    /// A channel between stages was unexpectedly disconnected.
    #[error("channel disconnected")]
    ChannelDisconnected,
}
