//! AR foveation / gaze-contingent quality falloff (DECISION 6).
//!
//! When the sequence enables foveation (the AR preset), the encoder codes the
//! foveal (gaze-centred) slices at `base_qp` and the peripheral slices at
//! progressively higher QP, so the bitrate follows the retina's acuity falloff.
//! Both encoder and decoder derive the per-slice QP from the same header fields
//! — the slice grid (sequence), the frame's `foveation_center_*`, and
//! `base_qp` — so no extra bitstream field is required and the round-trip stays
//! exact. (The other mechanism the design notes, reduced-resolution peripheral
//! slices, is the v2 upgrade; QP falloff is concrete and lossless at the
//! fovea boundary.)

use crate::headers::{FrameHeader, SequenceHeader};

/// Maximum extra QP applied to the most peripheral slice.
pub const MAX_FOVEATION_QP_DELTA: u8 = 18;

/// The gaze map for one frame: the centre of interest in frame coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GazeMap {
    pub center_x: u16,
    pub center_y: u16,
}

impl GazeMap {
    /// Build the gaze map for a frame, or `None` if foveation is disabled in
    /// the sequence header (so every slice uses `base_qp`).
    pub fn for_frame(seq: &SequenceHeader, frame: &FrameHeader) -> Option<GazeMap> {
        if !seq.foveation_enabled {
            return None;
        }
        Some(GazeMap {
            center_x: frame.foveation_center_x,
            center_y: frame.foveation_center_y,
        })
    }

    /// Per-slice QP for grid cell `(col, row)`. Foveal slices (near the gaze
    /// centre) get `base_qp`; peripheral slices get up to `base_qp +
    /// MAX_FOVEATION_QP_DELTA`, scaled by the normalised distance from the
    /// centre.
    pub fn qp_for_slice(
        &self,
        seq: &SequenceHeader,
        frame: &FrameHeader,
        col: usize,
        row: usize,
    ) -> u8 {
        let cols = seq.slice_grid_cols as usize;
        let rows = seq.slice_grid_rows as usize;
        if cols == 0 || rows == 0 {
            return frame.base_qp;
        }
        let cx = if frame.width == 0 {
            cols as f64 / 2.0
        } else {
            (frame.foveation_center_x as f64 / frame.width as f64) * cols as f64
        };
        let cy = if frame.height == 0 {
            rows as f64 / 2.0
        } else {
            (frame.foveation_center_y as f64 / frame.height as f64) * rows as f64
        };
        let dx = (col as f64 - cx) / (cols as f64 / 2.0);
        let dy = (row as f64 - cy) / (rows as f64 / 2.0);
        let dist = (dx * dx + dy * dy).sqrt().clamp(0.0, 1.0);
        let delta = (dist * MAX_FOVEATION_QP_DELTA as f64).round() as i32;
        (frame.base_qp as i32 + delta).clamp(0, 51) as u8
    }
}

/// Per-slice QP for global slice index `slice` (row-major grid), derived from
/// the header fields. Returns `frame.base_qp` when foveation is disabled, so a
/// non-foveated stream is unchanged. The encoder and decoder both call this with
/// the same `slice` index, which is what keeps the round-trip exact.
pub fn slice_qp_by_index(seq: &SequenceHeader, frame: &FrameHeader, slice: usize) -> u8 {
    if !seq.foveation_enabled {
        return frame.base_qp;
    }
    let cols = seq.slice_grid_cols as usize;
    let rows = seq.slice_grid_rows as usize;
    if cols == 0 || rows == 0 {
        return frame.base_qp;
    }
    let col = slice % cols;
    let row = slice / cols;
    GazeMap {
        center_x: frame.foveation_center_x,
        center_y: frame.foveation_center_y,
    }
    .qp_for_slice(seq, frame, col, row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headers::{FrameType, ProfilePreset};

    fn seq(foveation: bool) -> SequenceHeader {
        SequenceHeader {
            version: 1,
            max_width: 1280,
            max_height: 720,
            profile: ProfilePreset::AR,
            slice_grid_cols: 8,
            slice_grid_rows: 8,
            fec_overhead_pct: 25,
            foveation_enabled: foveation,
            min_block_size_log2: 3,
            max_block_size_log2: 3,
            bit_depth: 8,
            chroma_format: crate::headers::ChromaFormat::Yuv420,
            num_rans_streams: 64,
            max_ref_frames: 1,
            max_deadline_ms: 16,
        }
    }

    fn frame(cx: u16, cy: u16) -> FrameHeader {
        FrameHeader {
            frame_type: FrameType::Key,
            width: 1280,
            height: 720,
            base_qp: 10,
            ref_frame_count: 0,
            deadline_ms: 16,
            force_idr: true,
            foveation_center_x: cx,
            foveation_center_y: cy,
            intra_refresh_mask: vec![0b0000_0001],
            payload_len: 0,
        }
    }

    #[test]
    fn for_frame_none_when_disabled() {
        let s = seq(false);
        let f = frame(640, 360);
        assert!(GazeMap::for_frame(&s, &f).is_none());
    }

    #[test]
    fn foveal_slice_uses_base_qp() {
        let s = seq(true);
        let f = frame(640, 360);
        let g = GazeMap::for_frame(&s, &f).unwrap();
        // The gaze centre maps to the middle of the grid, so the central slice
        // should get the lowest QP (base_qp).
        let center_qp = g.qp_for_slice(&s, &f, 4, 4);
        assert_eq!(center_qp, f.base_qp, "foveal slice must use base_qp");
        let corner_qp = g.qp_for_slice(&s, &f, 0, 0);
        assert!(corner_qp > center_qp, "peripheral slice must be coarser");
    }

    #[test]
    fn qp_grows_monotonically_with_distance() {
        let s = seq(true);
        let f = frame(640, 360);
        let g = GazeMap::for_frame(&s, &f).unwrap();
        let near = g.qp_for_slice(&s, &f, 4, 4);
        let mid = g.qp_for_slice(&s, &f, 4, 0);
        let far = g.qp_for_slice(&s, &f, 0, 0);
        assert!(near <= mid);
        assert!(mid <= far);
        assert!(far > near);
    }

    #[test]
    fn disabled_sequence_uses_base_qp_everywhere() {
        let s = seq(false);
        let f = frame(640, 360);
        for slice in 0..64usize {
            assert_eq!(slice_qp_by_index(&s, &f, slice), f.base_qp);
        }
    }
}
