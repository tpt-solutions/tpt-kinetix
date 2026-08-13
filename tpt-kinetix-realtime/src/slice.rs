//! Slice-grid framing for a Realtime frame.
//!
//! DECISION 3: a frame is partitioned into an independently packetizable grid
//! of `slice_grid_cols * slice_grid_rows` slices. Each slice is its own
//! rANS-coded byte range with no cross-slice dependency, so a decoder can hand
//! each slice to a separate thread and apply concealment to any slice it
//! cannot recover (via FEC or the intra-refresh fallback). This module frames
//! the per-slice payloads into the single frame payload buffer and splits
//! them back apart, reusing [`tpt_kinetix_bitstream::RansStreamSet`].
//!
//! # Cap
//!
//! [`tpt_kinetix_bitstream::RansStreamSet`] addresses sub-streams with a
//! `u8`, so a grid may hold at most 255 slices. A conforming encoder keeps
//! `cols * rows <= 255` (e.g. for the AR "fine grid" preset, 15x16 = 240);
//! [`SliceGrid::frame`] rejects anything larger rather than silently
//! truncating.

use tpt_kinetix_bitstream::RansStreamSet;
use tpt_kinetix_core::error::KinetixError;

/// The independent slice grid declared by the sequence header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SliceGrid {
    pub cols: u8,
    pub rows: u8,
}

impl SliceGrid {
    /// Total number of slices in the grid (`cols * rows`).
    pub fn count(&self) -> usize {
        self.cols as usize * self.rows as usize
    }

    /// Frame one slice payload per grid cell into the frame payload buffer.
    ///
    /// `slices` must contain exactly `count()` independently-coded rANS byte
    /// ranges (one per slice, row-major). Returns the framed buffer ready to
    /// follow the frame header's `payload_len`.
    pub fn frame(&self, slices: &[Vec<u8>]) -> Result<Vec<u8>, KinetixError> {
        let expected = self.count();
        if slices.len() != expected {
            return Err(KinetixError::Parse(format!(
                "slice grid: expected {expected} slice payloads ({}x{}), got {}",
                self.cols, self.rows, slices.len()
            )));
        }
        if expected > u8::MAX as usize {
            return Err(KinetixError::Parse(format!(
                "slice grid: {expected} slices exceeds the rANS stream-set limit ({})",
                u8::MAX
            )));
        }
        RansStreamSet::frame(slices)
    }

    /// Split a framed frame payload back into its independent per-slice byte
    /// ranges. The count is validated against the grid so a truncated or
    /// mismatched payload is rejected rather than silently mis-sliced.
    pub fn unframe<'a>(&self, data: &'a [u8]) -> Result<Vec<&'a [u8]>, KinetixError> {
        let streams = RansStreamSet::unframe(data)?;
        if streams.len() != self.count() {
            return Err(KinetixError::Parse(format!(
                "slice grid: payload holds {} slices, expected {} ({}x{})",
                streams.len(),
                self.count(),
                self.cols,
                self.rows
            )));
        }
        Ok(streams)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_count_is_cols_times_rows() {
        assert_eq!(SliceGrid { cols: 4, rows: 4 }.count(), 16);
        assert_eq!(SliceGrid { cols: 8, rows: 8 }.count(), 64);
    }

    #[test]
    fn frame_round_trips_through_unframe() {
        let grid = SliceGrid { cols: 4, rows: 4 };
        let slices: Vec<Vec<u8>> = (0..grid.count())
            .map(|i| vec![(i * 7) as u8, (i * 13) as u8, i as u8])
            .collect();
        let framed = grid.frame(&slices).expect("frame");
        let unframed = grid.unframe(&framed).expect("unframe");
        assert_eq!(unframed.len(), grid.count());
        for (orig, got) in slices.iter().zip(unframed.iter()) {
            assert_eq!(orig, got);
        }
    }

    #[test]
    fn frame_rejects_wrong_slice_count() {
        let grid = SliceGrid { cols: 2, rows: 2 };
        let slices = vec![vec![0u8]; 3];
        assert!(grid.frame(&slices).is_err());
    }

    #[test]
    fn unframe_rejects_count_mismatch() {
        let grid = SliceGrid { cols: 2, rows: 2 };
        let correct: Vec<Vec<u8>> = vec![vec![1], vec![2], vec![3], vec![4]];
        let framed = grid.frame(&correct).expect("frame");
        // A grid with a different slice count must reject the payload:
        let wrong = SliceGrid { cols: 1, rows: 2 }; // count 2 != 4
        assert!(wrong.unframe(&framed).is_err());
    }

    #[test]
    fn frame_rejects_over_255_slices() {
        let grid = SliceGrid { cols: 16, rows: 16 }; // 256 > 255
        let slices = vec![vec![0u8]; grid.count()];
        assert!(grid.frame(&slices).is_err());
    }
}
