//! Minimal MP4 / ISO BMFF demuxer skeleton.
//!
//! This module will grow into a full ISO BMFF parser driven by `nom`.
//! For Phase 0 the structure is stubbed out so the workspace compiles and
//! the public API surface is established.

use kinetix_core::{error::KinetixError, packet::Packet};

use crate::Demuxer;

/// Stateful MP4 demuxer.
///
/// Holds a reference to the raw container bytes and a read cursor.
/// In a future version this will become an async reader over a byte stream.
pub struct Mp4Demuxer<'a> {
    data: &'a [u8],
    cursor: usize,
}

impl<'a> Mp4Demuxer<'a> {
    /// Creates a new demuxer from a byte slice containing the full MP4 file.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, cursor: 0 }
    }

    /// Returns the number of bytes remaining from the current cursor position.
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.cursor)
    }
}

impl<'a> Demuxer for Mp4Demuxer<'a> {
    /// Attempts to read the next packet from the container.
    ///
    /// # TODO (Phase 2)
    /// - Parse `ftyp`, `moov`, `mdat` boxes using `nom`.
    /// - Walk the `trak` / `mdia` / `stbl` box hierarchy to build a sample table.
    /// - Yield `Packet` instances from the sample table.
    fn read_packet(&mut self) -> Result<Option<Packet>, KinetixError> {
        if self.cursor >= self.data.len() {
            return Ok(None);
        }
        Err(KinetixError::Unsupported(
            "MP4 demuxer not yet implemented (Phase 2)".to_owned(),
        ))
    }

    fn seek(&mut self, _target_pts_ms: i64) -> Result<(), KinetixError> {
        // TODO(phase-2): random-access via stss/stts tables.
        Err(KinetixError::Unsupported(
            "MP4 seek not yet implemented (Phase 2)".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_slice_returns_none() {
        let mut demuxer = Mp4Demuxer::new(&[]);
        assert_eq!(demuxer.remaining(), 0);
        assert!(matches!(demuxer.read_packet(), Ok(None)));
    }
}
