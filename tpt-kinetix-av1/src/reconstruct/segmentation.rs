//! Segmentation (AV1 §5.11.9 `intra_segment_id` / `inter_segment_id` /
//! `read_segment_id`, §7.12.2 `get_qindex`, and the per-segment feature
//! queries). Cross-checked against dav1d's `decode_b`.

use super::*;
use crate::frame::{
    SEG_LVL_ALT_LF_Y_V, SEG_LVL_ALT_Q, SEG_LVL_GLOBALMV, SEG_LVL_REF_FRAME, SEG_LVL_SKIP,
};

/// `neg_deinterleave(diff, ref, max)` (§5.11.9).
fn neg_deinterleave(diff: i32, r: i32, max: i32) -> i32 {
    if r == 0 {
        return diff;
    }
    if r >= max - 1 {
        return max - diff - 1;
    }
    if 2 * r < max {
        if diff <= 2 * r {
            if diff & 1 != 0 {
                return r + ((diff + 1) >> 1);
            }
            return r - (diff >> 1);
        }
        diff
    } else {
        if diff <= 2 * (max - r - 1) {
            if diff & 1 != 0 {
                return r + ((diff + 1) >> 1);
            }
            return r - (diff >> 1);
        }
        max - (diff + 1)
    }
}

impl<'a> TileDecodeState<'a> {
    /// `seg_feature_active(feature)` for the current block.
    #[inline]
    pub(super) fn seg_active(&self, feature: usize) -> bool {
        self.seg.enabled && self.seg.params.active(self.segment_id, feature)
    }

    /// `get_qindex(ignoreDeltaQ, segmentId)` (§7.12.2).
    pub(super) fn get_qindex(&self, ignore_delta_q: bool, segment: usize) -> u8 {
        let use_delta = !ignore_delta_q && self.delta_q_present;
        if self.seg.enabled && self.seg.params.active(segment, SEG_LVL_ALT_Q) {
            let data = self.seg.params.value(segment, SEG_LVL_ALT_Q);
            let base = if use_delta {
                i32::from(self.current_q_index)
            } else {
                i32::from(self.base_q_idx)
            };
            (base + data).clamp(0, 255) as u8
        } else if use_delta {
            self.current_q_index
        } else {
            self.base_q_idx
        }
    }

    /// Set the per-block `Lossless` flag from the block's segment.
    #[inline]
    pub(super) fn apply_block_lossless(&mut self) {
        self.lossless = self.seg.lossless[self.segment_id & 7];
        // `transform_type`'s `qidx > 0` gate uses `get_qindex(1, segment_id)` (§5.11.47).
        self.qidx_pos = self.get_qindex(true, self.segment_id) > 0;
        // `SegQMLevel` (§5.9.12): 15 for lossless segments, else the frame's level.
        self.cur_qm = match self.seg.qm {
            Some(q) if !self.lossless => q,
            _ => [15; 3],
        };
    }

    /// `AvailU` / `AvailL` (tile-relative).
    #[inline]
    fn seg_avail(&self, mi_row: usize, mi_col: usize) -> (bool, bool) {
        (
            mi_row > self.tile_px_y0 / MI_SIZE,
            mi_col > self.tile_px_x0 / MI_SIZE,
        )
    }

    /// `SegmentIds[row][col]` of the frame being decoded.
    #[inline]
    fn cur_seg(&self, row: usize, col: usize) -> usize {
        usize::from(self.meta.segment_ids[row * self.mi_cols + col])
    }

    /// `get_segment_id()` (§5.11.9): minimum of `PrevSegmentIds` over the block.
    fn predicted_segment_id(&self, mi_row: usize, mi_col: usize, bsize: usize) -> usize {
        let Some(prev) = self.seg.prev else {
            return 0;
        };
        let bw4 = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh4 = BLOCK_HEIGHT[bsize] / MI_SIZE;
        let x_end = (mi_col + bw4).min(self.mi_cols);
        let y_end = (mi_row + bh4).min(self.mi_rows);
        let mut seg = 7u8;
        for y in mi_row..y_end {
            for x in mi_col..x_end {
                seg = seg.min(prev[y * self.mi_cols + x]);
            }
        }
        usize::from(seg)
    }

    /// `read_segment_id()` (§5.11.9): spatial prediction from the already
    /// decoded neighbours, then (unless `skip`) a coded `neg_deinterleave` diff.
    fn read_segment_id_sym(&mut self, mi_row: usize, mi_col: usize, skip: bool) -> usize {
        let (avail_u, avail_l) = self.seg_avail(mi_row, mi_col);
        let (pred, ctx) = if avail_u && avail_l {
            let a = self.cur_seg(mi_row - 1, mi_col);
            let l = self.cur_seg(mi_row, mi_col - 1);
            let al = self.cur_seg(mi_row - 1, mi_col - 1);
            let ctx = if al == a && al == l {
                2
            } else if al == a || al == l || a == l {
                1
            } else {
                0
            };
            (if a == al { a } else { l }, ctx)
        } else if avail_l {
            (self.cur_seg(mi_row, mi_col - 1), 0)
        } else if avail_u {
            (self.cur_seg(mi_row - 1, mi_col), 0)
        } else {
            (0, 0)
        };
        if skip {
            return pred;
        }
        let diff = self.mode_cdfs.read_segment_id(&mut self.dec, ctx) as i32;
        let max = self.seg.last_active as i32 + 1;
        let id = neg_deinterleave(diff, pred as i32, max);
        id.clamp(0, self.seg.last_active as i32) as usize
    }

    /// `intra_segment_id()` (§5.11.8). Call once before `read_skip` when
    /// `SegIdPreSkip` and once after otherwise (with the decoded `skip`).
    pub(super) fn intra_segment_id(&mut self, mi_row: usize, mi_col: usize, skip: bool) {
        self.segment_id = if self.seg.enabled {
            self.read_segment_id_sym(mi_row, mi_col, skip)
        } else {
            0
        };
        self.apply_block_lossless();
    }

    fn set_seg_pred_ctx(&mut self, mi_row: usize, mi_col: usize, bsize: usize, v: u8) {
        let bw4 = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh4 = BLOCK_HEIGHT[bsize] / MI_SIZE;
        for c in mi_col..(mi_col + bw4).min(self.mi_cols) {
            self.seg_pred_above[c] = v;
        }
        for r in mi_row..(mi_row + bh4).min(self.mi_rows) {
            self.seg_pred_left[r] = v;
        }
    }

    /// `inter_segment_id(preSkip)` (§5.11.9).
    pub(super) fn inter_segment_id(
        &mut self,
        pre_skip: bool,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
        skip: bool,
    ) {
        if !self.seg.enabled {
            self.segment_id = 0;
            self.apply_block_lossless();
            return;
        }
        let predicted = self.predicted_segment_id(mi_row, mi_col, bsize);
        if !self.seg.update_map {
            self.segment_id = predicted;
        } else if pre_skip && !self.seg.pre_skip {
            self.segment_id = 0;
        } else if !pre_skip && skip {
            self.set_seg_pred_ctx(mi_row, mi_col, bsize, 0);
            self.segment_id = self.read_segment_id_sym(mi_row, mi_col, true);
        } else if self.seg.temporal_update {
            let ctx =
                usize::from(self.seg_pred_above[mi_col]) + usize::from(self.seg_pred_left[mi_row]);
            let predicted_flag = self.mode_cdfs.read_seg_id_predicted(&mut self.dec, ctx);
            self.segment_id = if predicted_flag {
                predicted
            } else {
                self.read_segment_id_sym(mi_row, mi_col, false)
            };
            self.set_seg_pred_ctx(mi_row, mi_col, bsize, u8::from(predicted_flag));
        } else {
            self.segment_id = self.read_segment_id_sym(mi_row, mi_col, false);
        }
        self.apply_block_lossless();
    }

    /// Record the finished block's `segment_id` into `SegmentIds` (only when
    /// the map is being updated; otherwise the frame keeps `PrevSegmentIds`).
    pub(super) fn store_segment_id(&mut self, mi_row: usize, mi_col: usize, bsize: usize) {
        if !(self.seg.enabled && self.seg.update_map) {
            return;
        }
        let bw4 = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh4 = BLOCK_HEIGHT[bsize] / MI_SIZE;
        let id = self.segment_id as u8;
        for r in mi_row..(mi_row + bh4).min(self.mi_rows) {
            for c in mi_col..(mi_col + bw4).min(self.mi_cols) {
                self.meta.segment_ids[r * self.mi_cols + c] = id;
            }
        }
    }

    /// `read_is_inter()`'s segmentation overrides (§5.11.20): `Some(is_inter)`
    /// when `SEG_LVL_REF_FRAME` / `SEG_LVL_GLOBALMV` decide it.
    pub(super) fn seg_forced_is_inter(&self) -> Option<bool> {
        if self.seg_active(SEG_LVL_REF_FRAME) {
            Some(self.seg.params.value(self.segment_id, SEG_LVL_REF_FRAME) != 0)
        } else if self.seg_active(SEG_LVL_GLOBALMV) {
            Some(true)
        } else {
            None
        }
    }

    /// The current segment's `SEG_LVL_ALT_LF_U` / `_V` deltas (0 when inactive)
    /// for the chroma deblock level cache (§7.14.4).
    pub(super) fn seg_lf_deltas(&self) -> [i32; 2] {
        let f = |feature: usize| {
            if self.seg_active(feature) {
                self.seg.params.value(self.segment_id, feature)
            } else {
                0
            }
        };
        [f(SEG_LVL_ALT_LF_Y_V + 2), f(SEG_LVL_ALT_LF_Y_V + 3)]
    }

    /// `read_skip_mode()`'s segmentation gate (§5.11.11).
    pub(super) fn seg_blocks_skip_mode(&self) -> bool {
        self.seg_active(SEG_LVL_SKIP)
            || self.seg_active(SEG_LVL_REF_FRAME)
            || self.seg_active(SEG_LVL_GLOBALMV)
    }
}
