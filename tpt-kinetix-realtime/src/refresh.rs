//! Intra-refresh scheduling (DECISION 2).
//!
//! Instead of periodic full-IDR keyframes (which spike bitrate and stall
//! latency), Realtime cycles a small set of slice rows into intra coding each
//! frame. After `period` frames every row has been refreshed, bounding error
//! propagation to the refresh period without a keyframe bit spike. This module
//! computes the per-frame `intra_refresh_mask` bitmask the encoder writes into
//! the frame header and the decoder reads to know which rows are
//! independently decodable (the terminal fallback for any slice FEC/concealment
//! could not recover).

/// Schedules which slice rows are intra-coded per frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntraRefreshScheduler {
    /// Number of slice rows in the grid.
    pub rows: u8,
    /// Frames to cycle through all rows (roughly 0.5-1 s at the frame rate).
    pub period: u16,
}

impl IntraRefreshScheduler {
    /// Number of rows marked intra per frame (`ceil(rows / period)`).
    pub fn rows_per_frame(&self) -> usize {
        let rows = self.rows as usize;
        let period = self.period as usize;
        rows.div_ceil(period)
    }

    /// Length in bytes of the resulting mask (`ceil(rows / 8)`).
    pub fn mask_len(&self) -> usize {
        self.rows.div_ceil(8) as usize
    }

    /// Compute the intra-refresh bitmask for frame `frame_index`.
    ///
    /// Marks `rows_per_frame()` consecutive rows starting at
    /// `(frame_index * rows_per_frame) % rows`, wrapping around. Over `period`
    /// frames this covers every row at least once, so a decoder that loses a
    /// packet only waits at most the refresh period for that region to be
    /// re-established as intra.
    pub fn mask_for_frame(&self, frame_index: u64) -> Vec<u8> {
        let rows = self.rows as usize;
        let per = self.rows_per_frame();
        let start = ((frame_index % rows as u64) * per as u64 % rows as u64) as usize;
        let mut mask = vec![0u8; self.mask_len()];
        for k in 0..per {
            let row = (start + k) % rows;
            mask[row / 8] |= 1u8 << (row % 8);
        }
        mask
    }

    /// True if `row` is intra-coded in the mask for `frame_index`.
    pub fn is_intra(&self, frame_index: u64, row: u8) -> bool {
        let row = row as usize;
        if row >= self.rows as usize {
            return false;
        }
        let mask = self.mask_for_frame(frame_index);
        (mask[row / 8] >> (row % 8)) & 1 == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_row_per_frame_covers_all_rows() {
        let sched = IntraRefreshScheduler { rows: 8, period: 8 };
        assert_eq!(sched.rows_per_frame(), 1);
        let mut seen = [false; 8];
        for i in 0..8u64 {
            let mask = sched.mask_for_frame(i);
            assert_eq!(mask.len(), 1);
        for r in 0..8 {
            if sched.is_intra(i, r) {
                seen[r as usize] = true;
            }
        }
        }
        assert!(seen.iter().all(|&b| b), "every row must be intra at least once");
    }

    #[test]
    fn multi_row_per_frame_wraps_and_covers() {
        // 17 rows over 3 frames -> 6 rows/frame, wrapping on the last.
        let sched = IntraRefreshScheduler { rows: 17, period: 3 };
        assert_eq!(sched.rows_per_frame(), 6);
        assert_eq!(sched.mask_len(), 3); // ceil(17/8) = 3
        let mut seen = [false; 17];
        for i in 0..3u64 {
            for r in 0..17u8 {
                if sched.is_intra(i, r) {
                    seen[r as usize] = true;
                }
            }
        }
        assert!(seen.iter().all(|&b| b), "all 17 rows covered within the period");
    }

    #[test]
    fn single_row_single_period() {
        let sched = IntraRefreshScheduler { rows: 1, period: 1 };
        assert_eq!(sched.mask_for_frame(0), vec![1]);
        assert!(sched.is_intra(0, 0));
    }

    #[test]
    fn mask_len_matches_row_count() {
        assert_eq!(IntraRefreshScheduler { rows: 8, period: 8 }.mask_len(), 1);
        assert_eq!(IntraRefreshScheduler { rows: 9, period: 9 }.mask_len(), 2);
        assert_eq!(IntraRefreshScheduler { rows: 17, period: 3 }.mask_len(), 3);
    }
}
