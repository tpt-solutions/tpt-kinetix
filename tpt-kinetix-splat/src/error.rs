//! Error type shared by the splat model and all parsers.

/// Errors from building a [`GaussianCloud`](crate::GaussianCloud) or reading
/// and writing splat files.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SplatError {
    /// The input is not a well-formed splat file (truncated, inconsistent
    /// header, bad counts, malformed text).
    #[error("malformed splat data: {0}")]
    Parse(&'static str),
    /// The input is well formed but uses a feature this crate does not
    /// implement (binary big-endian PLY, list properties, SH degree above 3).
    #[error("unsupported splat feature: {0}")]
    Unsupported(&'static str),
    /// A cloud could not be constructed because its columns disagree.
    #[error("invalid gaussian cloud: {0}")]
    Invalid(&'static str),
}
