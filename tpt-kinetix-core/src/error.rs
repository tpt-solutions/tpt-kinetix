use thiserror::Error;

/// Top-level error type for the Kinetix engine.
#[derive(Debug, Error)]
pub enum KinetixError {
    /// Wraps any I/O error from the standard library.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A parsing or bitstream error, with a human-readable message.
    #[error("parse error: {0}")]
    Parse(String),

    /// The requested feature or format is not yet supported.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// The input stream has been fully consumed.
    #[error("end of stream")]
    EndOfStream,
}
