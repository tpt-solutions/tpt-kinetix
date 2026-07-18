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

    /// The decoder produced output, but its decode path is not yet pixel-exact,
    /// so the frames must not be trusted as correct pixel data.
    ///
    /// This is a deliberate, explicit signal used instead of silently returning
    /// `Ok` with wrong data. The contained string names the codec and the
    /// missing feature(s).
    #[error("decoder not pixel-exact yet: {0}")]
    NotPixelExact(String),

    /// The input stream has been fully consumed.
    #[error("end of stream")]
    EndOfStream,
}
