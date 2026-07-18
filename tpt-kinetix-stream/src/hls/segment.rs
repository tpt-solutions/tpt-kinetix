//! HLS segment descriptor.

use std::path::PathBuf;

/// Metadata for a single HLS `.ts` segment.
#[derive(Debug, Clone)]
pub struct HlsSegment {
    /// Monotonically-increasing segment index (0-based).
    pub index: u64,
    /// Actual duration of this segment in seconds.
    pub duration_secs: f64,
    /// Path to the `.ts` file on disk.
    pub path: PathBuf,
    /// Optional byte range within the file `(offset, length)`.
    pub byte_range: Option<(u64, u64)>,
}

impl HlsSegment {
    /// Canonical filename for a segment with the given index.
    ///
    /// Example: `HlsSegment::filename(3)` → `"segment00003.ts"`.
    pub fn filename(index: u64) -> String {
        format!("segment{index:05}.ts")
    }
}
