//! Tile-decoder mode parsing: `decode_mode`'s intra/keyframe branches, the
//! inter reference-selection ladders, context-cache updates and MV filling.
//! Inherent impls on [`TileDecoder`](super::frame::TileDecoder).

use tpt_kinetix_core::error::KinetixError;

use crate::booldec::BoolDecoder;
use crate::frame::{
    TileDecoder, INTER_MODE_CTX_LUT, INTER_MODE_OFF, INTRA_SIZE_GROUP, MAX_TX_FOR_BS,
    PART_ABOVE_CTX, PART_LEFT_CTX,
};
use crate::header::{FrameType, PRED_COMPREF, PRED_SWITCHABLE};
use crate::mv::{read_mv_component, read_mv_joint, MvFinder};
use crate::predict::{Mv, NEARESTMV, NEARMV, NEWMV, ZEROMV};
use crate::tables::{BWH_TAB, FILTER_TREE, INTER_MODE_TREE, INTRAMODE_TREE, SEGMENTATION_TREE};

const BS_8X8: usize = 9;
const BS_8X4: usize = 10;
const BS_4X8: usize = 11;
#[allow(dead_code)] // documented for completeness; sub-8x8 uses bs > BS_8X8 checks
const BS_4X4: usize = 12;

/// FilterMode enum value -> SUBPEL_FILTERS row.
const FILTER_MODE_TO_ROW: [usize; 3] = [2, 0, 1];

impl<'a> TileDecoder<'a> {
    pub(super) fn decode_mode(&mut self, bc: &mut BoolDecoder) -> Result<(), KinetixError> {
        let row = self.row;
        let col = self.col;
        let bs = self.b.bs;
        let (bw4u, bh4u) = crate::header::bwh(1, bs);
        let w4 = bw4u.min(self.cols() - col);
        let h4 = bh4u.min(self.rows() - row);
        let have_a = row > 0;
        let have_l = col > self.tile_col_start;
        let is_key_or_intra = self.hdr.frame_type == FrameType::Key || self.hdr.intra_only;

        self.parse_segmentation_and_skip(bc, w4, h4, have_a, have_l, is_key_or_intra)?;

        // tx size — intra blocks read it now; inter blocks read it AFTER the
        // inter mode/MV info (reference read_inter_frame_mode_info order).
        let max_tx = MAX_TX_FOR_BS[bs];
        let txfm_switchable = self.hdr.txfm_mode == crate::header::TxfmMode::Switchable;
        if is_key_or_intra || self.b.intra {
            self.parse_tx_size(bc, bs, max_tx, txfm_switchable, have_a, have_l, true);
        }
        if is_key_or_intra {
            self.b.comp = false;
            self.b.ref_ = [0; 2];
            self.parse_kf_intra_modes(bc, bs);
        } else if self.b.intra {
            self.b.comp = false;
            self.b.ref_ = [0; 2];
            if bs > BS_8X8 {
                for i in 0..4 {
                    let m = crate::mv::read_tree(bc, &INTRAMODE_TREE, &self.probs.mode.y_mode[0]);
                    self.b.mode[i] = m;
                    self.counts.y_mode[0][m] += 1;
                }
                if bs == BS_8X4 {
                    self.b.mode[1] = self.b.mode[0];
                    self.b.mode[3] = self.b.mode[2];
                } else if bs == BS_4X8 {
                    self.b.mode[2] = self.b.mode[0];
                    self.b.mode[3] = self.b.mode[1];
                }
            } else {
                let sz = INTRA_SIZE_GROUP[bs];
                let m = crate::mv::read_tree(bc, &INTRAMODE_TREE, &self.probs.mode.y_mode[sz]);
                self.b.mode = [m; 4];
                self.counts.y_mode[sz][m] += 1;
            }
            let uv_probs = self.probs.mode.uv_mode[self.b.mode[3]];
            self.b.uvmode = crate::mv::read_tree(bc, &INTRAMODE_TREE, &uv_probs);
            self.counts.uv_mode[self.b.mode[3]][self.b.uvmode] += 1;
        } else {
            self.parse_inter_mode_info(bc, have_a, have_l)?;
            // reference: read_tx_size(cm, xd, !skip || !inter, r)
            self.parse_tx_size(
                bc,
                bs,
                max_tx,
                txfm_switchable,
                have_a,
                have_l,
                !self.b.skip,
            );
        }

        self.set_ctxs(have_a, have_l, w4, h4);
        self.write_mvref_grid(w4, h4);
        Ok(())
    }

    /// The tx-size parse (reference `read_tx_size`): a tree read under the
    /// tx-size context when selection is allowed, otherwise the clamped max.
    #[allow(clippy::too_many_arguments)]
    fn parse_tx_size(
        &mut self,
        bc: &mut BoolDecoder,
        bs: usize,
        max_tx: usize,
        txfm_switchable: bool,
        have_a: bool,
        have_l: bool,
        allow_select: bool,
    ) {
        if allow_select && txfm_switchable && bs >= BS_8X8 {
            let col = self.col;
            let row7 = self.row7;
            let c: usize = if have_a {
                if have_l {
                    let above = if self.state.above_skip_ctx[col] != 0 {
                        max_tx
                    } else {
                        self.state.above_txfm_ctx[col] as usize
                    };
                    let left = if self.left.skip[row7] != 0 {
                        max_tx
                    } else {
                        self.left.txfm[row7] as usize
                    };
                    ((above + left > max_tx) as usize).clamp(0, 1)
                } else if self.state.above_skip_ctx[col] != 0 {
                    1
                } else {
                    (self.state.above_txfm_ctx[col] as usize * 2 > max_tx) as usize
                }
            } else if have_l {
                if self.left.skip[row7] != 0 {
                    1
                } else {
                    (self.left.txfm[row7] as usize * 2 > max_tx) as usize
                }
            } else {
                1
            };
            match max_tx {
                3 => {
                    let mut tx = usize::from(bc.read_bool(self.probs.mode.tx32p[c][0]));
                    if tx != 0 {
                        tx += usize::from(bc.read_bool(self.probs.mode.tx32p[c][1]));
                        if tx == 2 {
                            tx += usize::from(bc.read_bool(self.probs.mode.tx32p[c][2]));
                        }
                    }
                    self.b.tx = tx;
                    self.counts.tx32p[c][tx] += 1;
                }
                2 => {
                    let mut tx = usize::from(bc.read_bool(self.probs.mode.tx16p[c][0]));
                    if tx != 0 {
                        tx += usize::from(bc.read_bool(self.probs.mode.tx16p[c][1]));
                    }
                    self.b.tx = tx;
                    self.counts.tx16p[c][tx] += 1;
                }
                1 => {
                    let tx = usize::from(bc.read_bool(self.probs.mode.tx8p[c]));
                    self.b.tx = tx;
                    self.counts.tx8p[c][tx] += 1;
                }
                _ => self.b.tx = 0,
            }
        } else {
            self.b.tx = max_tx.min(self.hdr.txfm_mode as usize);
        }
    }

    fn parse_segmentation_and_skip(
        &mut self,
        bc: &mut BoolDecoder,
        w4: usize,
        h4: usize,
        _have_a: bool,
        _have_l: bool,
        is_key_or_intra: bool,
    ) -> Result<(), KinetixError> {
        let row = self.row;
        let col = self.col;
        let row7 = self.row7;
        if !self.hdr.segmentation.enabled {
            self.b.seg_id = 0;
        } else if is_key_or_intra {
            self.b.seg_id = if !self.hdr.segmentation.update_map {
                0
            } else {
                crate::mv::read_tree(bc, &SEGMENTATION_TREE, &self.hdr.segmentation.tree_probs)
                    as u8
            };
        } else if !self.hdr.segmentation.update_map
            || (self.hdr.segmentation.temporal_update
                && bc.read_bool(
                    self.hdr.segmentation.pred_probs[self.state.above_segpred_ctx[col] as usize
                        + self.left.segpred[row7] as usize],
                ))
        {
            let pred = match (self.fctx.prev_segmap, self.hdr.error_resilient) {
                (Some(segmap), false) => {
                    let mut p = 8u8;
                    for y in 0..h4 {
                        let base = (y + row) * self.state.frame.seg_stride + col;
                        for x in 0..w4 {
                            if base + x < segmap.len() {
                                p = p.min(segmap[base + x]);
                            }
                        }
                    }
                    p
                }
                _ => 0,
            };
            self.b.seg_id = pred;
            for i in 0..w4 {
                self.state.above_segpred_ctx[col + i] = 1;
            }
            for i in 0..h4 {
                self.left.segpred[row7 + i] = 1;
            }
        } else {
            self.b.seg_id =
                crate::mv::read_tree(bc, &SEGMENTATION_TREE, &self.hdr.segmentation.tree_probs)
                    as u8;
            for i in 0..w4 {
                self.state.above_segpred_ctx[col + i] = 0;
            }
            for i in 0..h4 {
                self.left.segpred[row7 + i] = 0;
            }
        }
        if self.hdr.segmentation.enabled && (self.hdr.segmentation.update_map || is_key_or_intra) {
            let stride = self.state.frame.seg_stride;
            for y in 0..h4 {
                for x in 0..w4 {
                    let idx = (row + y) * stride + col + x;
                    if idx < self.state.frame.segmap.len() {
                        self.state.frame.segmap[idx] = self.b.seg_id;
                    }
                }
            }
        }

        // skip
        self.b.skip = self.hdr.segmentation.enabled
            && self.hdr.segmentation.feat_enabled[self.b.seg_id as usize][3];
        if !self.b.skip {
            let c = self.left.skip[row7] as usize + self.state.above_skip_ctx[col] as usize;
            let prob = self.probs.mode.skip[c];
            self.b.skip = bc.read_bool(prob);
            self.counts.skip[c][usize::from(self.b.skip)] += 1;
        }

        // intra/inter flag
        if is_key_or_intra {
            self.b.intra = true;
        } else if self.hdr.segmentation.enabled
            && self.hdr.segmentation.feat_enabled[self.b.seg_id as usize][2]
        {
            self.b.intra = self.hdr.segmentation.feat[self.b.seg_id as usize][2] == 0;
        } else {
            let c: usize = if self.row > 0 && col > self.tile_col_start {
                let mut c = self.state.above_intra_ctx[col] + self.left.intra[row7];
                if c == 2 {
                    c += 1;
                }
                c as usize
            } else if self.row > 0 {
                2 * self.state.above_intra_ctx[col] as usize
            } else if col > self.tile_col_start {
                2 * self.left.intra[row7] as usize
            } else {
                0
            };
            let bit = bc.read_bool(self.probs.mode.intra[c]);
            self.counts.intra[c][usize::from(bit)] += 1;
            self.b.intra = !bit;
        }
        Ok(())
    }

    /// Keyframe / intra-only y-mode parsing (per-sub-block for sub-8x8).
    fn parse_kf_intra_modes(&mut self, bc: &mut BoolDecoder, bs: usize) {
        let col = self.col;
        let row7 = self.row7;
        let row = self.row;
        if bs > BS_8X8 {
            // Missing (out-of-frame) neighbours read DC_PRED (0) — the
            // context arrays hold NO_NEIGHBOUR_MODE (14) there.
            let mut a0 = if row > 0 {
                self.state.above_mode_ctx[col * 2]
            } else {
                0
            };
            let mut a1 = if row > 0 {
                self.state.above_mode_ctx[col * 2 + 1]
            } else {
                0
            };
            let mut l0 = if col > self.tile_col_start {
                self.left.mode[row7 * 2]
            } else {
                0
            };
            let mut l1 = if col > self.tile_col_start {
                self.left.mode[row7 * 2 + 1]
            } else {
                0
            };
            let tree = &INTRAMODE_TREE;

            let m0 = {
                let __p = crate::header::kf_ymode_probs_for(a0, l0);

                crate::mv::read_tree(bc, tree, &__p) as u8
            };
            self.b.mode[0] = m0 as usize;
            a0 = m0;
            if bs != BS_8X4 {
                let m1 = {
                    let __p = crate::header::kf_ymode_probs_for(a1, m0);

                    crate::mv::read_tree(bc, tree, &__p) as u8
                };
                self.b.mode[1] = m1 as usize;
                l0 = m1;
                a1 = m1;
            } else {
                self.b.mode[1] = m0 as usize;
                l0 = m0;
                a1 = m0;
            }
            if bs != BS_4X8 {
                let m2 = {
                    let __p = crate::header::kf_ymode_probs_for(a0, l1);

                    crate::mv::read_tree(bc, tree, &__p) as u8
                };
                self.b.mode[2] = m2 as usize;
                a0 = m2;
                if bs != BS_8X4 {
                    let m3 = {
                        let __p = crate::header::kf_ymode_probs_for(a1, m2);

                        crate::mv::read_tree(bc, tree, &__p) as u8
                    };
                    self.b.mode[3] = m3 as usize;
                    l1 = m3;
                    a1 = m3;
                } else {
                    self.b.mode[3] = m2 as usize;
                    l1 = m2;
                    a1 = m2;
                }
            } else {
                self.b.mode[2] = m0 as usize;
                self.b.mode[3] = self.b.mode[1];
                l1 = self.b.mode[1] as u8;
                a1 = self.b.mode[1] as u8;
            }
            self.state.above_mode_ctx[col * 2] = a0;
            self.state.above_mode_ctx[col * 2 + 1] = a1;
            self.left.mode[row7 * 2] = l0;
            self.left.mode[row7 * 2 + 1] = l1;
        } else {
            let probs = crate::header::kf_ymode_probs_for(
                if row > 0 {
                    self.state.above_mode_ctx[col * 2]
                } else {
                    0
                },
                if col > self.tile_col_start {
                    self.left.mode[row7 * 2]
                } else {
                    0
                },
            );
            let m = crate::mv::read_tree(bc, &INTRAMODE_TREE, &probs) as u8;
            self.b.mode = [m as usize; 4];
            let n = BWH_TAB[bs * 2] as usize;
            let m2 = BWH_TAB[bs * 2 + 1] as usize;
            for i in 0..n {
                self.state.above_mode_ctx[col * 2 + i] = m;
            }
            for i in 0..m2 {
                self.left.mode[row7 * 2 + i] = m;
            }
        }
        let uv_probs = crate::header::kf_uvmode_probs_for(self.b.mode[3]);
        self.b.uvmode = crate::mv::read_tree(bc, &INTRAMODE_TREE, &uv_probs);
    }

    /// The inter-frame block mode/reference parse (`decode_mode` inter path).
    fn parse_inter_mode_info(
        &mut self,
        bc: &mut BoolDecoder,
        have_a: bool,
        have_l: bool,
    ) -> Result<(), KinetixError> {
        let col = self.col;
        let row7 = self.row7;
        let bs = self.b.bs;

        // segment-feature reference override
        if self.hdr.segmentation.enabled
            && self.hdr.segmentation.feat_enabled[self.b.seg_id as usize][2]
        {
            self.b.comp = false;
            self.b.ref_[0] = self.hdr.segmentation.feat[self.b.seg_id as usize][2] as u8 - 1;
        } else {
            // compound prediction flag
            if self.hdr.comp_pred_mode != PRED_SWITCHABLE {
                self.b.comp = self.hdr.comp_pred_mode == PRED_COMPREF;
            } else {
                let c: usize = if have_a {
                    if have_l {
                        if self.state.above_comp_ctx[col] != 0 && self.left.comp[row7] != 0 {
                            4
                        } else if self.state.above_comp_ctx[col] != 0 {
                            2 + usize::from(
                                self.left.intra[row7] != 0
                                    || self.left.ref_[row7] == self.fctx.fixcompref as u8,
                            )
                        } else if self.left.comp[row7] != 0 {
                            2 + usize::from(
                                self.state.above_intra_ctx[col] != 0
                                    || self.state.above_ref_ctx[col] == self.fctx.fixcompref as u8,
                            )
                        } else {
                            usize::from(
                                self.state.above_intra_ctx[col] == 0
                                    && self.state.above_ref_ctx[col] == self.fctx.fixcompref as u8,
                            ) ^ usize::from(
                                self.left.intra[row7] == 0
                                    && self.left.ref_[row7] == self.fctx.fixcompref as u8,
                            )
                        }
                    } else if self.state.above_comp_ctx[col] != 0 {
                        3
                    } else {
                        usize::from(
                            self.state.above_intra_ctx[col] == 0
                                && self.state.above_ref_ctx[col] == self.fctx.fixcompref as u8,
                        )
                    }
                } else if have_l {
                    if self.left.comp[row7] != 0 {
                        3
                    } else {
                        usize::from(
                            self.left.intra[row7] == 0
                                && self.left.ref_[row7] == self.fctx.fixcompref as u8,
                        )
                    }
                } else {
                    1
                };
                let bit = bc.read_bool(self.probs.mode.comp[c]);
                self.counts.comp[c][usize::from(bit)] += 1;
                self.b.comp = bit;
            }

            // references
            if self.b.comp {
                let fix_idx = usize::from(self.hdr.sign_bias[self.fctx.fixcompref]);
                let var_idx = 1 - fix_idx;
                self.b.ref_[fix_idx] = self.fctx.fixcompref as u8;
                let c: usize = if have_a {
                    if have_l {
                        if self.state.above_intra_ctx[col] != 0 {
                            if self.left.intra[row7] != 0 {
                                2
                            } else {
                                1 + 2 * usize::from(
                                    self.left.ref_[row7] != self.fctx.varcompref[1] as u8,
                                )
                            }
                        } else if self.left.intra[row7] != 0 {
                            1 + 2 * usize::from(
                                self.state.above_ref_ctx[col] != self.fctx.varcompref[1] as u8,
                            )
                        } else {
                            let refl = self.left.ref_[row7];
                            let refa = self.state.above_ref_ctx[col];
                            let v1 = self.fctx.varcompref[1] as u8;
                            let f = self.fctx.fixcompref as u8;
                            let v0 = self.fctx.varcompref[0] as u8;
                            if refl == refa && refa == v1 {
                                0
                            } else if self.left.comp[row7] == 0
                                && self.state.above_comp_ctx[col] == 0
                            {
                                if (refa == f && refl == v0) || (refl == f && refa == v0) {
                                    4
                                } else if refa == refl {
                                    3
                                } else {
                                    1
                                }
                            } else if self.left.comp[row7] == 0 {
                                if refa == v1 && refl != v1 {
                                    1
                                } else if refl == v1 && refa != v1 {
                                    2
                                } else {
                                    4
                                }
                            } else if self.state.above_comp_ctx[col] == 0 {
                                if refl == v1 && refa != v1 {
                                    1
                                } else if refa == v1 && refl != v1 {
                                    2
                                } else {
                                    4
                                }
                            } else if refl == refa {
                                4
                            } else {
                                2
                            }
                        }
                    } else if self.state.above_intra_ctx[col] != 0 {
                        2
                    } else if self.state.above_comp_ctx[col] != 0 {
                        4 * usize::from(
                            self.state.above_ref_ctx[col] != self.fctx.varcompref[1] as u8,
                        )
                    } else {
                        3 * usize::from(
                            self.state.above_ref_ctx[col] != self.fctx.varcompref[1] as u8,
                        )
                    }
                } else if have_l {
                    if self.left.intra[row7] != 0 {
                        2
                    } else if self.left.comp[row7] != 0 {
                        4 * usize::from(self.left.ref_[row7] != self.fctx.varcompref[1] as u8)
                    } else {
                        3 * usize::from(self.left.ref_[row7] != self.fctx.varcompref[1] as u8)
                    }
                } else {
                    2
                };
                let bit = bc.read_bool(self.probs.mode.comp_ref[c]);
                self.counts.comp_ref[c][usize::from(bit)] += 1;
                self.b.ref_[var_idx] = self.fctx.varcompref[usize::from(bit)] as u8;
            } else {
                // single reference
                let c: usize = if have_a && self.state.above_intra_ctx[col] == 0 {
                    if have_l && self.left.intra[row7] == 0 {
                        if self.left.comp[row7] != 0 {
                            if self.state.above_comp_ctx[col] != 0 {
                                1 + usize::from(
                                    self.fctx.fixcompref != 0
                                        || self.left.ref_[row7] != 0
                                        || self.state.above_ref_ctx[col] != 0,
                                )
                            } else {
                                3 * usize::from(self.state.above_ref_ctx[col] == 0)
                                    + usize::from(
                                        self.fctx.fixcompref != 0 || self.left.ref_[row7] != 0,
                                    )
                            }
                        } else if self.state.above_comp_ctx[col] != 0 {
                            3 * usize::from(self.left.ref_[row7] == 0)
                                + usize::from(
                                    self.fctx.fixcompref != 0 || self.state.above_ref_ctx[col] != 0,
                                )
                        } else {
                            2 * usize::from(self.left.ref_[row7] == 0)
                                + 2 * usize::from(self.state.above_ref_ctx[col] == 0)
                        }
                    } else if self.state.above_intra_ctx[col] != 0 {
                        2
                    } else if self.state.above_comp_ctx[col] != 0 {
                        1 + usize::from(
                            self.fctx.fixcompref != 0 || self.state.above_ref_ctx[col] != 0,
                        )
                    } else {
                        4 * usize::from(self.state.above_ref_ctx[col] == 0)
                    }
                } else if have_l && self.left.intra[row7] == 0 {
                    if self.left.intra[row7] != 0 {
                        2
                    } else if self.left.comp[row7] != 0 {
                        1 + usize::from(self.fctx.fixcompref != 0 || self.left.ref_[row7] != 0)
                    } else {
                        4 * usize::from(self.left.ref_[row7] == 0)
                    }
                } else {
                    2
                };
                let bit = bc.read_bool(self.probs.mode.single_ref[c][0]);
                self.counts.single_ref[c][0][usize::from(bit)] += 1;
                if !bit {
                    self.b.ref_[0] = 0;
                } else {
                    let c2: usize = if have_a {
                        if have_l {
                            if self.left.intra[row7] != 0 {
                                if self.state.above_intra_ctx[col] != 0 {
                                    2
                                } else if self.state.above_comp_ctx[col] != 0 {
                                    1 + 2 * usize::from(
                                        self.fctx.fixcompref == 1
                                            || self.state.above_ref_ctx[col] == 1,
                                    )
                                } else if self.state.above_ref_ctx[col] == 0 {
                                    3
                                } else {
                                    4 * usize::from(self.state.above_ref_ctx[col] == 1)
                                }
                            } else if self.state.above_intra_ctx[col] != 0 {
                                if self.left.intra[row7] != 0 {
                                    2
                                } else if self.left.comp[row7] != 0 {
                                    1 + 2 * usize::from(
                                        self.fctx.fixcompref == 1 || self.left.ref_[row7] == 1,
                                    )
                                } else if self.left.ref_[row7] == 0 {
                                    3
                                } else {
                                    4 * usize::from(self.left.ref_[row7] == 1)
                                }
                            } else if self.state.above_comp_ctx[col] != 0 {
                                if self.left.comp[row7] != 0 {
                                    if self.left.ref_[row7] == self.state.above_ref_ctx[col] {
                                        3 * usize::from(
                                            self.fctx.fixcompref == 1 || self.left.ref_[row7] == 1,
                                        )
                                    } else {
                                        2
                                    }
                                } else if self.left.ref_[row7] == 0 {
                                    1 + 2 * usize::from(
                                        self.fctx.fixcompref == 1
                                            || self.state.above_ref_ctx[col] == 1,
                                    )
                                } else {
                                    3 * usize::from(self.left.ref_[row7] == 1)
                                        + usize::from(
                                            self.fctx.fixcompref == 1
                                                || self.state.above_ref_ctx[col] == 1,
                                        )
                                }
                            } else if self.left.comp[row7] != 0 {
                                if self.state.above_ref_ctx[col] == 0 {
                                    1 + 2 * usize::from(
                                        self.fctx.fixcompref == 1 || self.left.ref_[row7] == 1,
                                    )
                                } else {
                                    3 * usize::from(self.state.above_ref_ctx[col] == 1)
                                        + usize::from(
                                            self.fctx.fixcompref == 1 || self.left.ref_[row7] == 1,
                                        )
                                }
                            } else if self.state.above_ref_ctx[col] == 0 {
                                if self.left.ref_[row7] == 0 {
                                    3
                                } else {
                                    4 * usize::from(self.left.ref_[row7] == 1)
                                }
                            } else if self.left.ref_[row7] == 0 {
                                4 * usize::from(self.state.above_ref_ctx[col] == 1)
                            } else {
                                2 * usize::from(self.left.ref_[row7] == 1)
                                    + 2 * usize::from(self.state.above_ref_ctx[col] == 1)
                            }
                        } else if self.state.above_intra_ctx[col] != 0
                            || (self.state.above_comp_ctx[col] == 0
                                && self.state.above_ref_ctx[col] == 0)
                        {
                            2
                        } else if self.state.above_comp_ctx[col] != 0 {
                            3 * usize::from(
                                self.fctx.fixcompref == 1 || self.state.above_ref_ctx[col] == 1,
                            )
                        } else {
                            4 * usize::from(self.state.above_ref_ctx[col] == 1)
                        }
                    } else if have_l {
                        if self.left.intra[row7] != 0
                            || (self.left.comp[row7] == 0 && self.left.ref_[row7] == 0)
                        {
                            2
                        } else if self.left.comp[row7] != 0 {
                            3 * usize::from(self.fctx.fixcompref == 1 || self.left.ref_[row7] == 1)
                        } else {
                            4 * usize::from(self.left.ref_[row7] == 1)
                        }
                    } else {
                        2
                    };
                    let bit = bc.read_bool(self.probs.mode.single_ref[c2][1]);
                    self.counts.single_ref[c2][1][usize::from(bit)] += 1;
                    self.b.ref_[0] = 1 + u8::from(bit);
                }
            }
        }

        // inter prediction modes
        if bs <= BS_8X8 {
            if self.hdr.segmentation.enabled
                && self.hdr.segmentation.feat_enabled[self.b.seg_id as usize][3]
            {
                self.b.mode = [ZEROMV; 4];
            } else {
                let off = INTER_MODE_OFF[bs];
                let c = INTER_MODE_CTX_LUT[self.state.above_mode_ctx[col + off] as usize]
                    [self.left.mode[row7 + off] as usize] as usize;
                if std::env::var_os("TPT_VP9_TRACE").is_some() {
                    eprintln!(
                        "IMCTX r={} c={} ctx={} p={} {} {}",
                        self.row,
                        col,
                        c,
                        self.probs.mode.mv_mode[c][0],
                        self.probs.mode.mv_mode[c][1],
                        self.probs.mode.mv_mode[c][2]
                    );
                }
                let m = crate::mv::read_tree(bc, &INTER_MODE_TREE, &self.probs.mode.mv_mode[c]);
                // tree leaves: 0=ZEROMV, 1=NEARESTMV, 2=NEARMV, 3=NEWMV
                self.b.mode = [[ZEROMV, NEARESTMV, NEARMV, NEWMV][m]; 4];
                self.counts.mv_mode[c][m] += 1;
            }
        }

        // interpolation filter
        if self.hdr.filter_mode == 3 {
            let c: usize = if have_a && self.state.above_mode_ctx[col] as usize >= NEARESTMV {
                if have_l && self.left.mode[row7] as usize >= NEARESTMV {
                    if self.state.above_filter_ctx[col] == self.left.filter[row7] {
                        self.left.filter[row7] as usize
                    } else {
                        3
                    }
                } else {
                    self.state.above_filter_ctx[col] as usize
                }
            } else if have_l && self.left.mode[row7] as usize >= NEARESTMV {
                self.left.filter[row7] as usize
            } else {
                3
            };
            let leaf = crate::mv::read_tree(bc, &FILTER_TREE, &self.probs.mode.filter[c]);
            self.counts.filter[c][leaf] += 1;
            self.b.filter = leaf;
            self.b.filter_type = crate::predict::FilterType::from_leaf(leaf).0;
        } else {
            // raw filter mode: FilterMode enum (0=SMOOTH, 1=REGULAR, 2=SHARP)
            // -> row in SUBPEL_FILTERS ([REGULAR, SHARP, SMOOTH])
            self.b.filter_type = FILTER_MODE_TO_ROW[self.hdr.filter_mode as usize];
        }

        if bs > BS_8X8 {
            // sub-8x8: per-sub-block modes and MVs
            let c = INTER_MODE_CTX_LUT[self.state.above_mode_ctx[col] as usize]
                [self.left.mode[row7] as usize] as usize;
            let vref_list = [self.b.ref_[0], self.b.ref_[1]];
            let _ = vref_list;
            let mut read_mode = |bc: &mut BoolDecoder, td: &mut Self| -> usize {
                let m = crate::mv::read_tree(bc, &INTER_MODE_TREE, &td.probs.mode.mv_mode[c]);
                td.counts.mv_mode[c][m] += 1;
                m
            };
            let _ = &mut read_mode;
            let m0 = crate::mv::read_tree(bc, &INTER_MODE_TREE, &self.probs.mode.mv_mode[c]);
            self.counts.mv_mode[c][m0] += 1;
            self.b.mode[0] = [ZEROMV, NEARESTMV, NEARMV, NEWMV][m0];
            self.fill_mv(bc, 0, m0);
            if bs != BS_8X4 {
                let m1 = crate::mv::read_tree(bc, &INTER_MODE_TREE, &self.probs.mode.mv_mode[c]);
                self.counts.mv_mode[c][m1] += 1;
                self.b.mode[1] = [ZEROMV, NEARESTMV, NEARMV, NEWMV][m1];
                self.fill_mv(bc, 1, m1);
            } else {
                self.b.mode[1] = self.b.mode[0];
                self.b.mv[1] = self.b.mv[0];
            }
            if bs != BS_4X8 {
                let m2 = crate::mv::read_tree(bc, &INTER_MODE_TREE, &self.probs.mode.mv_mode[c]);
                self.counts.mv_mode[c][m2] += 1;
                self.b.mode[2] = [ZEROMV, NEARESTMV, NEARMV, NEWMV][m2];
                self.fill_mv(bc, 2, m2);
                if bs != BS_8X4 {
                    let m3 =
                        crate::mv::read_tree(bc, &INTER_MODE_TREE, &self.probs.mode.mv_mode[c]);
                    self.counts.mv_mode[c][m3] += 1;
                    self.b.mode[3] = [ZEROMV, NEARESTMV, NEARMV, NEWMV][m3];
                    self.fill_mv(bc, 3, m3);
                } else {
                    self.b.mode[3] = self.b.mode[2];
                    self.b.mv[3] = self.b.mv[2];
                }
            } else {
                self.b.mode[2] = self.b.mode[0];
                self.b.mv[2] = self.b.mv[0];
                self.b.mode[3] = self.b.mode[1];
                self.b.mv[3] = self.b.mv[1];
            }
        } else {
            self.fill_mv(bc, -1, self.b.mode[0]);
            self.b.mv[1] = self.b.mv[0];
            self.b.mv[2] = self.b.mv[0];
            self.b.mv[3] = self.b.mv[0];
        }

        Ok(())
    }

    /// `ff_vp9_fill_mv`: predict or read the MV(s) for sub-block `sb`
    /// (-1 = whole block), mode `mode`.
    fn fill_mv(&mut self, bc: &mut BoolDecoder, sb: i32, mode: usize) {
        if mode == ZEROMV {
            self.b.mv[sb.clamp(0, 3) as usize] = [Mv::zero(); 2];
            return;
        }
        let ref0 = self.b.ref_[0] as i32;
        let hp_ok = |v: Mv| self.hdr.allow_high_precision_mv && v.x.abs() < 64 && v.y.abs() < 64;
        let z = 0usize;
        let finder = self.make_finder();
        let mut mv0 =
            finder.find_ref_mvs(ref0, z, mode == NEARMV, if mode == NEWMV { -1 } else { sb });
        if (mode == NEWMV || sb == -1) && !hp_ok(mv0) {
            mv0 = round_odd_mv(mv0);
        }
        self.b.mv[sb.clamp(0, 3) as usize][0] = mv0;
        if mode == NEWMV {
            let j = read_mv_joint(bc, &self.probs.mode);
            self.counts.mv_joint[j] += 1;
            let hp = hp_ok(mv0);
            if j >= 2 {
                let d = read_mv_component(bc, &self.probs.mode, 0, hp, &mut self.counts.mv_comp[0]);
                mv0.y = mv0.y.wrapping_add(d as i16);
            }
            if j & 1 != 0 {
                let d = read_mv_component(bc, &self.probs.mode, 1, hp, &mut self.counts.mv_comp[1]);
                mv0.x = mv0.x.wrapping_add(d as i16);
            }
            self.b.mv[sb.clamp(0, 3) as usize][0] = mv0;
        }

        if self.b.comp {
            let ref1 = self.b.ref_[1] as i32;
            let finder = self.make_finder();
            let mut mv1 =
                finder.find_ref_mvs(ref1, 1, mode == NEARMV, if mode == NEWMV { -1 } else { sb });
            if (mode == NEWMV || sb == -1) && !hp_ok(mv1) {
                mv1 = round_odd_mv(mv1);
            }
            self.b.mv[sb.clamp(0, 3) as usize][1] = mv1;
            if mode == NEWMV {
                let j = read_mv_joint(bc, &self.probs.mode);
                self.counts.mv_joint[j] += 1;
                let hp = hp_ok(mv1);
                if j >= 2 {
                    let d =
                        read_mv_component(bc, &self.probs.mode, 0, hp, &mut self.counts.mv_comp[0]);
                    mv1.y = mv1.y.wrapping_add(d as i16);
                }
                if j & 1 != 0 {
                    let d =
                        read_mv_component(bc, &self.probs.mode, 1, hp, &mut self.counts.mv_comp[1]);
                    mv1.x = mv1.x.wrapping_add(d as i16);
                }
                self.b.mv[sb.clamp(0, 3) as usize][1] = mv1;
            }
        }
    }

    fn make_finder(&self) -> MvFinder<'_> {
        MvFinder {
            mvref: &self.state.frame.mvrefs,
            mvref_prev: self.fctx.mvpair,
            above_mv: &self.state.above_mv_ctx,
            left_mv: &self.left.mv,
            map_w: self.state.frame.seg_stride,
            rows: self.rows(),
            cols: self.cols(),
            tile_col_start: self.tile_col_start,
            row: self.row,
            col: self.col,
            row7: self.row7,
            sign_bias: [
                false,
                self.hdr.sign_bias[0],
                self.hdr.sign_bias[1],
                self.hdr.sign_bias[2],
            ],
            use_last_frame_mvs: self.hdr.use_last_frame_mvs && self.fctx.mvpair.is_some(),
            min_mv: self.min_mv,
            max_mv: self.max_mv,
            bs: self.b.bs,
            block_mvs: self.b.mv,
        }
    }

    /// SET_CTXS: splat the block's context values into the above/left caches.
    fn set_ctxs(&mut self, have_a: bool, have_l: bool, w4: usize, h4: usize) {
        let col = self.col;
        let row7 = self.row7;
        let bs = self.b.bs;
        let skip = u8::from(self.b.skip);
        let tx = self.b.tx as u8;
        let pabove = PART_ABOVE_CTX[bs];
        let pleft = PART_LEFT_CTX[bs];
        let is_inter_frame = self.hdr.frame_type != FrameType::Key && !self.hdr.intra_only;
        let vref = self.b.ref_[if self.b.comp {
            usize::from(self.hdr.sign_bias[self.fctx.varcompref[0]])
        } else {
            0
        }];

        for i in 0..w4 {
            self.state.above_skip_ctx[col + i] = skip;
            self.state.above_txfm_ctx[col + i] = tx;
            self.state.above_partition_ctx[col + i] = pabove;
        }
        for i in 0..h4 {
            self.left.skip[row7 + i] = skip;
            self.left.txfm[row7 + i] = tx;
            self.left.partition[row7 + i] = pleft;
        }
        if is_inter_frame {
            let intra = u8::from(self.b.intra);
            let comp = u8::from(self.b.comp);
            let mode3 = self.b.mode[3] as u8;
            for i in 0..w4 {
                self.state.above_intra_ctx[col + i] = intra;
                self.state.above_comp_ctx[col + i] = comp;
                self.state.above_mode_ctx[col + i] = mode3;
            }
            for i in 0..h4 {
                self.left.intra[row7 + i] = intra;
                self.left.comp[row7 + i] = comp;
                self.left.mode[row7 + i] = mode3;
            }
            if !self.b.intra {
                for i in 0..w4 {
                    self.state.above_ref_ctx[col + i] = vref;
                }
                for i in 0..h4 {
                    self.left.ref_[row7 + i] = vref;
                }
                if self.hdr.filter_mode == 3 {
                    let f = self.b.filter as u8;
                    for i in 0..w4 {
                        self.state.above_filter_ctx[col + i] = f;
                    }
                    for i in 0..h4 {
                        self.left.filter[row7 + i] = f;
                    }
                }
            }
        }
        let _ = have_a;
        let _ = have_l;
    }

    /// Record this block's mode/ref/MV into the per-8px MV grid.
    fn write_mvref_grid(&mut self, w4: usize, h4: usize) {
        let row = self.row;
        let col = self.col;
        let stride = self.state.frame.seg_stride;
        let is_inter_frame = self.hdr.frame_type != FrameType::Key && !self.hdr.intra_only;
        for y in 0..h4 {
            let base = (row + y) * stride + col;
            if is_inter_frame && !self.b.intra {
                for x in 0..w4 {
                    let mv = &mut self.state.frame.mvrefs[base + x];
                    if self.b.comp {
                        mv.ref_ = [i32::from(self.b.ref_[0]), i32::from(self.b.ref_[1])];
                        mv.mv = self.b.mv[3];
                    } else {
                        mv.ref_ = [i32::from(self.b.ref_[0]), -1];
                        mv.mv = [self.b.mv[3][0], Mv::zero()];
                    }
                }
            } else {
                for x in 0..w4 {
                    let mv = &mut self.state.frame.mvrefs[base + x];
                    mv.ref_ = [-1, -1];
                }
            }
        }
    }
}

/// Round away odd MV components when high precision is not allowed
/// (reference `fill_mv` post-processing).
fn round_odd_mv(mv: Mv) -> Mv {
    let mut x = mv.x;
    let mut y = mv.y;
    if y & 1 != 0 {
        if y < 0 {
            y += 1;
        } else {
            y -= 1;
        }
    }
    if x & 1 != 0 {
        if x < 0 {
            x += 1;
        } else {
            x -= 1;
        }
    }
    Mv { x, y }
}
