use super::*;

/// Kinetix ref name (`NONE = 0`, `INTRA = 1`, `LAST_FRAME = 2` … `ALTREF_FRAME
/// = 8`) → dav1d numbering (`LAST = 0` … `ALTREF = 6`); `NONE`/`INTRA` → `-1`.
#[inline]
fn dav1d_ref(k: u8) -> i32 {
    if k >= LAST_FRAME {
        k as i32 - 2
    } else {
        -1
    }
}

/// `interintra_allowed_mask` (dav1d `tables.h`): single-ref block sizes that
/// may carry an inter-intra flag — {8x8, 8x16, 16x8, 16x16, 16x32, 32x16,
/// 32x32} in spec `BlockSize` indices.
fn interintra_allowed(bsize: usize) -> bool {
    matches!(
        bsize,
        BLOCK_8X8 | BLOCK_8X16 | BLOCK_16X8 | BLOCK_16X16 | BLOCK_16X32 | BLOCK_32X16 | BLOCK_32X32
    )
}

/// `wedge_allowed_mask` (dav1d `tables.h`): `interintra_allowed` plus 8x32 /
/// 32x8.
fn wedge_allowed(bsize: usize) -> bool {
    interintra_allowed(bsize) || matches!(bsize, BLOCK_8X32 | BLOCK_32X8)
}

/// dav1d `get_poc_diff` — signed order-hint difference, wrapped to the
/// `order_hint_n_bits` window.
fn poc_diff(order_hint_n_bits: u8, poc0: i32, poc1: i32) -> i32 {
    if order_hint_n_bits == 0 {
        return 0;
    }
    let mask = 1i32 << (order_hint_n_bits - 1);
    let diff = poc0 - poc1;
    (diff & (mask - 1)) - (diff & mask)
}

/// dav1d `f->jnt_weights[ref0][ref1]` computed per block (§7.11.3.15
/// `distance_weights`): the sixteenths weight for `preds[0]` in the
/// distance-weighted compound blend.
fn jnt_weight(order_hint_bits: u8, cur: i32, ref0poc: i32, ref1poc: i32) -> i32 {
    let d1 = poc_diff(order_hint_bits, ref0poc, cur).abs().min(31);
    let d0 = poc_diff(order_hint_bits, ref1poc, cur).abs().min(31);
    let order = usize::from(d0 <= d1);
    const QDW: [[i32; 2]; 3] = [[2, 3], [2, 5], [2, 7]];
    const QDT: [[i32; 2]; 4] = [[9, 7], [11, 5], [12, 4], [13, 3]];
    let mut k = 3usize;
    for (kk, w) in QDW.iter().enumerate() {
        let c0 = w[order];
        let c1 = w[1 - order];
        let d0c0 = d0 * c0;
        let d1c1 = d1 * c1;
        if (d0 > d1 && d0c0 < d1c1) || (d0 <= d1 && d0c0 > d1c1) {
            k = kk;
            break;
        }
    }
    QDT[k][order]
}

/// `Size_Group[]` (§ intra-mode size groups) restricted to the inter-intra
/// path — matches dav1d `ymode_size_context` for the allowed sizes.
fn size_group(bsize: usize) -> usize {
    match bsize {
        BLOCK_8X8 | BLOCK_8X16 | BLOCK_16X8 => 1,
        BLOCK_16X16 | BLOCK_16X32 | BLOCK_32X16 => 2,
        BLOCK_32X32 => 3,
        _ => 0,
    }
}

/// dav1d `wedge_ctx_lut` in spec `BlockSize` indices (only the inter-intra /
/// wedge-allowed sizes are ever queried).
fn wedge_ctx(bsize: usize) -> usize {
    match bsize {
        BLOCK_8X8 => 0,
        BLOCK_8X16 => 1,
        BLOCK_16X8 => 2,
        BLOCK_16X16 => 3,
        BLOCK_16X32 => 4,
        BLOCK_32X16 => 5,
        BLOCK_32X32 => 6,
        BLOCK_8X32 => 7,
        BLOCK_32X8 => 8,
        _ => 0,
    }
}

impl<'a> TileDecodeState<'a> {
    /// `has_overlappable_candidates()` (§5.11.23): true when the block has an
    /// inter-coded neighbour directly above or to the left (within the tile),
    /// which is the gate for the `motion_mode` / `use_obmc` read.
    fn has_overlappable_candidates(
        &self,
        mi_row: usize,
        mi_col: usize,
        bw: usize,
        bh: usize,
    ) -> bool {
        let row_start = self.tile_px_y0 / MI_SIZE;
        let col_start = self.tile_px_x0 / MI_SIZE;
        if mi_row > row_start {
            for c in mi_col..(mi_col + bw).min(self.mi_cols) {
                if self.is_inter_above[c] != 0 {
                    return true;
                }
            }
        }
        if mi_col > col_start {
            for r in mi_row..(mi_row + bh).min(self.mi_rows) {
                if self.is_inter_left[r] != 0 {
                    return true;
                }
            }
        }
        false
    }

    /// Approximation of dav1d `find_matching_ref` / §5.11.23 `find_warp_samples`
    /// reaching `NumSamples > 0`: an inter-coded above/left neighbour whose
    /// primary reference frame matches this block's. Enough to decide whether
    /// `read_motion_mode` reads the 3-way `motion_mode` symbol (warp allowed)
    /// or the `use_obmc` bool.
    fn has_matching_ref_candidates(
        &self,
        mi_row: usize,
        mi_col: usize,
        bw: usize,
        bh: usize,
        ref0: u8,
    ) -> bool {
        let row_start = self.tile_px_y0 / MI_SIZE;
        let col_start = self.tile_px_x0 / MI_SIZE;
        if mi_row > row_start {
            for c in mi_col..(mi_col + bw).min(self.mi_cols) {
                if self.is_inter_above[c] != 0 && self.ref_above[c][0] == ref0 {
                    return true;
                }
            }
        }
        if mi_col > col_start {
            for r in mi_row..(mi_row + bh).min(self.mi_rows) {
                if self.is_inter_left[r] != 0 && self.ref_left[r][0] == ref0 {
                    return true;
                }
            }
        }
        false
    }

    /// Build the §8.3.2 compound-context neighbour edge for the mi cell above
    /// `mi_col` (converting Kinetix ref names `LAST_FRAME = 2 …` to dav1d's
    /// `LAST = 0 …`).
    fn comp_edge_above(&self, mi_col: usize) -> CompEdge {
        if self.is_inter_above.get(mi_col).copied().unwrap_or(0) == 0 {
            return CompEdge::NA;
        }
        let r = self.ref_above[mi_col];
        CompEdge {
            intra: false,
            comp_type: self.comp_type_above[mi_col],
            ref0: dav1d_ref(r[0]),
            ref1: dav1d_ref(r[1]),
        }
    }

    fn comp_edge_left(&self, mi_row: usize) -> CompEdge {
        if self.is_inter_left.get(mi_row).copied().unwrap_or(0) == 0 {
            return CompEdge::NA;
        }
        let r = self.ref_left[mi_row];
        CompEdge {
            intra: false,
            comp_type: self.comp_type_left[mi_row],
            ref0: dav1d_ref(r[0]),
            ref1: dav1d_ref(r[1]),
        }
    }

    /// Inter-coded leaf block (AV1 Phase E): MV prediction (§7.10) + motion
    /// compensation (§7.11.3). Reconstructs a single/compound-reference block and
    /// adds the residual.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn decode_inter_block(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
    ) -> Result<(), KinetixError> {
        let bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh = BLOCK_HEIGHT[bsize] / MI_SIZE;
        if std::env::var("KINETIX_AV1_DBG_SB1").is_ok()
            && (16..=18).contains(&mi_row)
            && mi_col <= 2
        {
            eprintln!(
                "DBG SB1 INTER mi=({mi_col},{mi_row}) bsize={bsize} bit_pos={}",
                self.dec.bit_position()
            );
        }
        let allow_hp = self.allow_high_precision_mv;
        let frame_filter = self.interpolation_filter;
        let reference_select = self.reference_select;

        // `read_skip_mode()` (§5.11.11) — read *before* `read_skip()` in the
        // inter syntax order. A skip-mode block skips every other mode symbol:
        // it is compound, non-residual, and predicts from the fixed
        // `SkipModeFrame` pair with the NEAREST MVs.
        let above_skip = self.skip_above[mi_col] as usize;
        let left_skip = self.skip_left[mi_row] as usize;
        let above_inter = self.is_inter_above[mi_col] as usize;
        let left_inter = self.is_inter_left[mi_row] as usize;
        let skip_mode = if self.seg_feature_skip
            || !self.skip_mode_present
            || BLOCK_WIDTH[bsize] < 8
            || BLOCK_HEIGHT[bsize] < 8
        {
            false
        } else {
            let ctx = self.skip_mode_above[mi_col] as usize + self.skip_mode_left[mi_row] as usize;
            self.dec
                .read_symbol(&mut self.mode_cdfs.skip_mode[ctx.min(2)])
                == 1
        };
        if skip_mode {
            return self.decode_skip_mode_block(mi_row, mi_col, bsize);
        }

        // Skip flag (§5.11.11) — read before `is_inter`, matching the inter
        // syntax order.
        let skip = if self.seg_feature_skip {
            true
        } else {
            self.mode_cdfs
                .read_skip(&mut self.dec, (above_skip + left_skip).min(2))
                == 1
        };
        let dbg_b0 = std::env::var("KINETIX_AV1_DBG_B0").is_ok() && mi_row < 40 && mi_col < 40;
        if dbg_b0 {
            eprintln!(
                "DBG b0 mi=({mi_col},{mi_row}) bsize={bsize} skip={skip} rng={}",
                self.dec.raw_state().0
            );
        }

        // AV1 spec §5.11.18 `inter_frame_mode_info()`: `read_cdef()`/
        // `read_delta_qindex()`/`read_delta_lf()` come right after
        // `read_skip()`, then `ReadDeltas = 0`, before `read_is_inter()` —
        // same order and same no-op-unless-enabled behaviour as the intra
        // path (`intra_block.rs`).
        self.read_cdef(mi_row, mi_col, bsize, skip);
        self.read_delta_qindex(bsize, skip);
        self.read_delta_lf(bsize, skip);
        self.read_deltas = false;

        // `intra_inter` context (§8.3.2): based on whether the *available*
        // above/left neighbours are INTRA-coded, not a plain is-inter sum.
        let row_start = self.tile_px_y0 / MI_SIZE;
        let col_start = self.tile_px_x0 / MI_SIZE;
        let avail_u = mi_row > row_start;
        let avail_l = mi_col > col_start;
        let above_intra = avail_u && above_inter == 0;
        let left_intra = avail_l && left_inter == 0;
        let inter_ctx = if avail_u && avail_l {
            if above_intra && left_intra {
                3
            } else {
                usize::from(above_intra || left_intra)
            }
        } else if avail_u || avail_l {
            2 * usize::from(if avail_u { above_intra } else { left_intra })
        } else {
            0
        };
        let is_inter = self
            .dec
            .read_symbol(&mut self.map_inter_cdfs.is_inter[inter_ctx])
            == 1;
        if dbg_b0 {
            eprintln!(
                "DBG b0 is_inter={is_inter} ctx={inter_ctx} rng={}",
                self.dec.raw_state().0
            );
        }

        if !is_inter {
            // Intra-coded block inside an inter frame: reconstruct via the shared
            // intra machinery (mode symbols still read in inter order: skip above
            // is already consumed, so read y/uv mode then dispatch).
            let above_mode = self.ymode_above[mi_col] as usize;
            let left_mode = self.ymode_left[mi_row] as usize;
            let y_mode = self.mode_cdfs.read_intra_y_mode(
                &mut self.dec,
                INTRA_MODE_CONTEXT[above_mode],
                INTRA_MODE_CONTEXT[left_mode],
            );
            // `intra_angle_info_y()` (AV1 spec §5.11.42), same as the
            // keyframe path.
            let angle_delta_y = if bsize >= BLOCK_8X8 && is_directional_mode(y_mode as u8) {
                self.mode_cdfs.read_angle_delta(&mut self.dec, y_mode)
            } else {
                0
            };
            // `HasChroma` (AV1 spec §5.11.5) — see `has_chroma`'s doc comment;
            // same gate as the keyframe path.
            let has_chroma = !self.monochrome
                && has_chroma(
                    bsize,
                    mi_row,
                    mi_col,
                    self.subsampling_x,
                    self.subsampling_y,
                );
            let uv_mode = if has_chroma {
                self.mode_cdfs
                    .read_uv_mode(&mut self.dec, cfl_allowed_for_bsize(bsize), y_mode)
            } else {
                DC_PRED as usize
            };
            // `read_cfl_alphas()` (AV1 spec §5.11.45): read only when
            // `UVMode == UV_CFL_PRED`, immediately after `uv_mode` and before
            // `intra_angle_info_uv()`, per the `intra_block_mode_info()`
            // syntax order.
            let cfl_alpha = if has_chroma && uv_mode == UV_CFL_PRED {
                Some(self.mode_cdfs.read_cfl_alphas(&mut self.dec))
            } else {
                None
            };
            // `intra_angle_info_uv()` (AV1 spec §5.11.43).
            let angle_delta_uv =
                if has_chroma && bsize >= BLOCK_8X8 && is_directional_mode(uv_mode as u8) {
                    self.mode_cdfs.read_angle_delta(&mut self.dec, uv_mode)
                } else {
                    0
                };
            // `palette_mode_info()` (AV1 spec §5.11.46), same position as the
            // keyframe path.
            let (colors_y, colors_u, colors_v) =
                self.read_palette_mode_info(mi_row, mi_col, bsize, y_mode, uv_mode, has_chroma);
            // `filter_intra_mode_info()` (AV1 spec §5.11.24) is also read for
            // an intra block coded inside an inter frame (spec
            // `intra_block_mode_info()` calls it right after the mode reads,
            // same as the keyframe path) — this call site previously omitted
            // it entirely, which would desync every such block once inter
            // frames are actually decoded.
            let filter_intra_mode = if colors_y.is_empty() {
                self.mode_cdfs.read_filter_intra_mode_info(
                    &mut self.dec,
                    self.enable_filter_intra,
                    y_mode,
                    bsize,
                )
            } else {
                None
            };
            // `palette_tokens()` (AV1 spec §5.11.49), same position as the
            // keyframe path.
            let (map_y, stride_y) =
                self.read_color_map(bsize, mi_row, mi_col, colors_y.len(), false);
            let (map_uv, stride_uv) =
                self.read_color_map(bsize, mi_row, mi_col, colors_u.len(), true);
            let palette = PaletteData {
                colors_y,
                colors_u,
                colors_v,
                map_y,
                stride_y,
                map_uv,
                stride_uv,
            };
            // `read_tx_size`'s `allowSelect = !skip || !is_inter` is always
            // true here (`is_inter` is false on this branch), so `skip`
            // does not gate this read for an intra block — only for a true
            // inter block (see the other `read_tx_size` call site below).
            let max_tx = max_tx_size_for_bsize(bsize);
            let luma_tx = if self.tx_mode_select && !self.lossless {
                self.read_tx_size(bsize, max_tx, mi_row, mi_col)
            } else {
                max_tx
            };
            self.reconstruct_intra_subblock(
                mi_row,
                mi_col,
                bsize,
                y_mode,
                uv_mode,
                skip,
                luma_tx,
                filter_intra_mode,
                cfl_alpha,
                angle_delta_y,
                angle_delta_uv,
                &palette,
            )?;
            // Update inter neighbour state (this block is not inter).
            for r in mi_row..(mi_row + bh).min(self.mi_rows) {
                if let Some(s) = self.is_inter_left.get_mut(r) {
                    *s = 0;
                }
                if let Some(s) = self.comp_type_left.get_mut(r) {
                    *s = 0;
                }
            }
            for c in mi_col..(mi_col + bw).min(self.mi_cols) {
                if let Some(s) = self.is_inter_above.get_mut(c) {
                    *s = 0;
                }
                if let Some(s) = self.comp_type_above.get_mut(c) {
                    *s = 0;
                }
            }
            return Ok(());
        }

        // Compound vs single reference (§5.11.25). `comp_mode` is read only
        // when compound is allowed (`reference_select` / `switchable_comp_refs`
        // and the block is at least 8x8), with the §8.3.2 `get_comp_ctx`
        // neighbour context.
        let cedge_a = self.comp_edge_above(mi_col);
        let cedge_l = self.comp_edge_left(mi_row);
        let compound = if reference_select && BLOCK_WIDTH[bsize].min(BLOCK_HEIGHT[bsize]) > 4 {
            let ctx = comp_ctx(cedge_a, cedge_l, avail_u, avail_l);
            self.dec
                .read_symbol(&mut self.map_inter_cdfs.comp_mode[ctx])
                == 1
        } else {
            false
        };
        if dbg_b0 {
            eprintln!(
                "DBG b0 compflag={} rng={}",
                compound as u8,
                self.dec.raw_state().0
            );
        }

        // Reference name(s).
        let mut ref_names = [NONE_FRAME; 2];
        if compound {
            // Compound reference-frame tree (§5.11.25), ported from dav1d
            // (`decode.c`). dav1d ref numbering (LAST=0..ALTREF=6) internally;
            // `+ 2` converts back to Kinetix's `LAST_FRAME = 2` names. Kinetix's
            // CDF tables are stored transposed vs dav1d — `cdf[ctx][i]` where
            // dav1d indexes `cdf[i][ctx]`.
            let dir_ctx = comp_dir_ctx(cedge_a, cedge_l, avail_u, avail_l);
            let (fwd, bwd);
            if self
                .dec
                .read_symbol(&mut self.map_inter_cdfs.comp_ref_type[dir_ctx.min(4)])
                == 1
            {
                // BIDIR
                let c1 = fwd_ref_ctx(cedge_a, cedge_l, avail_u, avail_l);
                let f = if self
                    .dec
                    .read_symbol(&mut self.map_inter_cdfs.comp_ref[c1.min(2)][0])
                    == 1
                {
                    let c2 = fwd_ref_2_ctx(cedge_a, cedge_l, avail_u, avail_l);
                    2 + self
                        .dec
                        .read_symbol(&mut self.map_inter_cdfs.comp_ref[c2.min(2)][2])
                } else {
                    let c2 = fwd_ref_1_ctx(cedge_a, cedge_l, avail_u, avail_l);
                    self.dec
                        .read_symbol(&mut self.map_inter_cdfs.comp_ref[c2.min(2)][1])
                };
                let c3 = bwd_ref_ctx(cedge_a, cedge_l, avail_u, avail_l);
                let b = if self
                    .dec
                    .read_symbol(&mut self.map_inter_cdfs.comp_bwd_ref[c3.min(2)][0])
                    == 1
                {
                    6
                } else {
                    let c4 = bwd_ref_1_ctx(cedge_a, cedge_l, avail_u, avail_l);
                    4 + self
                        .dec
                        .read_symbol(&mut self.map_inter_cdfs.comp_bwd_ref[c4.min(2)][1])
                };
                fwd = f as u8 + 2;
                bwd = b as u8 + 2;
            } else {
                // UNIDIR
                let up = ref_ctx(cedge_a, cedge_l, avail_u, avail_l);
                if self
                    .dec
                    .read_symbol(&mut self.map_inter_cdfs.uni_comp_ref[up.min(2)][0])
                    == 1
                {
                    fwd = 6; // dav1d BWDREF(4) + 2
                    bwd = 8; // dav1d ALTREF(6) + 2
                } else {
                    let up1 = uni_p1_ctx(cedge_a, cedge_l, avail_u, avail_l);
                    let mut r1 = 1 + self
                        .dec
                        .read_symbol(&mut self.map_inter_cdfs.uni_comp_ref[up1.min(2)][1]);
                    if r1 == 2 {
                        let up2 = fwd_ref_2_ctx(cedge_a, cedge_l, avail_u, avail_l);
                        r1 += self
                            .dec
                            .read_symbol(&mut self.map_inter_cdfs.uni_comp_ref[up2.min(2)][2]);
                    }
                    fwd = 2; // dav1d LAST(0) + 2
                    bwd = r1 as u8 + 2;
                }
            }
            ref_names = [fwd, bwd];
            if dbg_b0 {
                eprintln!(
                    "DBG b0 comprefs={}/{} dir_ctx={dir_ctx} rng={}",
                    fwd - 2,
                    bwd - 2,
                    self.dec.raw_state().0
                );
            }
        } else {
            let above_refs = (above_inter != 0).then(|| self.ref_above[mi_col]);
            let left_refs = (left_inter != 0).then(|| self.ref_left[mi_row]);
            ref_names[0] = read_single_ref_name(
                &mut self.dec,
                &mut self.map_inter_cdfs,
                above_refs,
                left_refs,
            );
            if dbg_b0 {
                eprintln!("DBG b0 ref={} rng={}", ref_names[0], self.dec.raw_state().0);
            }
        }

        let mut mvs = [Mv::default(); 2];
        let force_integer_mv = self.force_integer_mv;
        let mut new_mf = 0u8;
        let mut single_mode = NEARESTMV;
        // dav1d `BlockContext::comp_type` for this block (0 for single-ref).
        let mut block_comp_type = 0u8;

        // AV1 §7.10.2 `find_mv_stack` — shared by both branches.
        let (stack, ctx, n_mvs, drl_ctx, comp_mode_ctx) =
            self.inter_mv_stack(mi_row, mi_col, bsize, ref_names);

        if !compound {
            // §5.11.24 single-ref mode cascade.
            let newmv_ctx = (ctx & 7) as usize;
            let globalmv_ctx = ((ctx >> 3) & 1) as usize;
            let refmv_ctx = ((ctx >> 4) & 15) as usize;

            // `new_mv` S(): 1 => NOT newmv, 0 => NEWMV.
            let not_newmv = self
                .dec
                .read_symbol(&mut self.map_inter_cdfs.new_mv[newmv_ctx.min(5)])
                == 1;
            if dbg_b0 {
                eprintln!(
                    "DBG b0 new_mv not={not_newmv} rng={}",
                    self.dec.raw_state().0
                );
            }
            let mut drl_idx = 0usize;
            let mode: u8;
            if not_newmv {
                // `zero_mv` S(): 0 => GLOBALMV, 1 => near path.
                let near_path = self
                    .dec
                    .read_symbol(&mut self.map_inter_cdfs.zero_mv[globalmv_ctx.min(1)])
                    == 1;
                if dbg_b0 {
                    eprintln!(
                        "DBG b0 zero_mv near={near_path} rng={}",
                        self.dec.raw_state().0
                    );
                }
                if !near_path {
                    mode = ZEROMV;
                    new_mf = 1;
                } else {
                    // `ref_mv` S(): 1 => NEARMV (+drl), 0 => NEARESTMV.
                    let rm = self
                        .dec
                        .read_symbol(&mut self.map_inter_cdfs.ref_mv[refmv_ctx.min(5)]);
                    if dbg_b0 {
                        eprintln!(
                            "DBG b0 ref_mv={rm} ctx={refmv_ctx} rng={}",
                            self.dec.raw_state().0
                        );
                    }
                    if rm == 1 {
                        mode = NEARMV;
                        drl_idx = 1;
                        if n_mvs > 2 {
                            drl_idx += self
                                .dec
                                .read_symbol(&mut self.map_inter_cdfs.drl_mode[drl_ctx[1].min(2)]);
                            if drl_idx == 2 && n_mvs > 3 {
                                drl_idx += self.dec.read_symbol(
                                    &mut self.map_inter_cdfs.drl_mode[drl_ctx[2].min(2)],
                                );
                            }
                        }
                    } else {
                        mode = NEARESTMV;
                    }
                }
            } else {
                mode = NEWMV;
                new_mf = 2;
                if n_mvs > 1 {
                    drl_idx += self
                        .dec
                        .read_symbol(&mut self.map_inter_cdfs.drl_mode[drl_ctx[0].min(2)]);
                    if drl_idx == 1 && n_mvs > 2 {
                        drl_idx += self
                            .dec
                            .read_symbol(&mut self.map_inter_cdfs.drl_mode[drl_ctx[1].min(2)]);
                    }
                }
            }

            let base_mv = stack.get(drl_idx).map(|m| m[0]).unwrap_or_default();
            if std::env::var("KINETIX_AV1_DBG_IMODE").is_ok() {
                eprintln!(
                    "DBG imode mi=({mi_col},{mi_row}) ref={} ctx={ctx:#x}(nm={newmv_ctx},gm={globalmv_ctx},rm={refmv_ctx}) \
                     mode={mode} drl={drl_idx} n_mvs={n_mvs} base=({},{}) rng={}",
                    ref_names[0], base_mv.row, base_mv.col, self.dec.raw_state().0
                );
            }
            single_mode = mode;
            mvs[0] = match mode {
                ZEROMV => Mv::default(),
                NEWMV => {
                    let diff = read_mv(
                        &mut self.dec,
                        &mut self.map_inter_cdfs,
                        allow_hp,
                        force_integer_mv,
                    )?;
                    Mv::new(base_mv.row + diff.row, base_mv.col + diff.col)
                }
                _ => base_mv,
            };
        } else {
            // §5.11.24 compound mode cascade (dav1d `decode.c`, `is_comp`).
            // `comp_inter_mode` (8-way) → per-ref sub-mode, then drl, then per-
            // ref MV assignment (NEAREST/NEAR from the stack, NEW = stack +
            // residual, GLOBAL = zero — global-motion warping not modelled).
            let comp_mode = self
                .dec
                .read_symbol(&mut self.map_inter_cdfs.comp_inter_mode[comp_mode_ctx.min(7)]);
            if dbg_b0 {
                eprintln!(
                    "DBG b0 compintermode={comp_mode} ctx={comp_mode_ctx} n_mvs={n_mvs} rng={}",
                    self.dec.raw_state().0
                );
            }
            // dav1d `dav1d_comp_inter_pred_modes` (enum order): per-ref sub-mode
            // as Kinetix mode constants (NEAREST=0, NEAR=1, GLOBAL/ZERO=4,
            // NEW=3).
            const IM: [[u8; 2]; 8] = [
                [NEARESTMV, NEARESTMV],
                [NEARMV, NEARMV],
                [NEARESTMV, NEWMV],
                [NEWMV, NEARESTMV],
                [NEARMV, NEWMV],
                [NEWMV, NEARMV],
                [ZEROMV, ZEROMV],
                [NEWMV, NEWMV],
            ];
            let im = IM[comp_mode.min(7)];

            let mut drl_idx = 0usize;
            if comp_mode == 7 {
                // NEWMV_NEWMV
                if n_mvs > 1 {
                    drl_idx += self
                        .dec
                        .read_symbol(&mut self.map_inter_cdfs.drl_mode[drl_ctx[0].min(2)]);
                    if drl_idx == 1 && n_mvs > 2 {
                        drl_idx += self
                            .dec
                            .read_symbol(&mut self.map_inter_cdfs.drl_mode[drl_ctx[1].min(2)]);
                    }
                }
            } else if im[0] == NEARMV || im[1] == NEARMV {
                drl_idx = 1;
                if n_mvs > 2 {
                    drl_idx += self
                        .dec
                        .read_symbol(&mut self.map_inter_cdfs.drl_mode[drl_ctx[1].min(2)]);
                    if drl_idx == 2 && n_mvs > 3 {
                        drl_idx += self
                            .dec
                            .read_symbol(&mut self.map_inter_cdfs.drl_mode[drl_ctx[2].min(2)]);
                    }
                }
            }

            let base = stack.get(drl_idx).copied().unwrap_or_default();
            for i in 0..2 {
                mvs[i] = match im[i] {
                    NEARESTMV | NEARMV => base[i],
                    ZEROMV => Mv::default(),
                    NEWMV => {
                        let diff = read_mv(
                            &mut self.dec,
                            &mut self.map_inter_cdfs,
                            allow_hp,
                            force_integer_mv,
                        )?;
                        Mv::new(base[i].row + diff.row, base[i].col + diff.col)
                    }
                    _ => base[i],
                };
            }
            if dbg_b0 {
                eprintln!(
                    "DBG b0 compmv drl={drl_idx} mv0=({},{}) mv1=({},{}) rng={}",
                    mvs[0].row,
                    mvs[0].col,
                    mvs[1].row,
                    mvs[1].col,
                    self.dec.raw_state().0
                );
            }

            // `read_compound_type()` (§5.11.26): `comp_group_idx` (mask vs
            // jnt/avg), then either `compound_idx` (jnt vs distance-weighted)
            // or the wedge / diffwtd branch + mask-sign literal.
            let mask_ctx = mask_comp_ctx(cedge_a, cedge_l);
            let comp_group_idx = if self.enable_masked_compound {
                self.dec
                    .read_symbol(&mut self.mode_cdfs.mask_comp[mask_ctx.min(5)])
                    == 1
            } else {
                false
            };
            if !comp_group_idx {
                if self.enable_jnt_comp {
                    // poc diffs for `get_jnt_comp_ctx`.
                    let poc = self.cur_order_hint as i32;
                    let ref_poc = |name: u8| -> i32 {
                        let slot = self.ref_to_slot[name as usize] as usize;
                        self.dpb_order_hints.get(slot).copied().unwrap_or(0) as i32
                    };
                    let d0 = poc_diff(self.order_hint_bits, ref_poc(ref_names[0]), poc).abs();
                    let d1 = poc_diff(self.order_hint_bits, poc, ref_poc(ref_names[1])).abs();
                    let jnt_ctx = jnt_comp_ctx(d0 == d1, cedge_a, cedge_l);
                    // 1 (WEIGHTED_AVG) + bit → 1 or 2 (AVG).
                    block_comp_type = 1 + self
                        .dec
                        .read_symbol(&mut self.mode_cdfs.jnt_comp[jnt_ctx.min(5)])
                        as u8;
                } else {
                    block_comp_type = 2; // COMP_INTER_AVG
                }
            } else if wedge_allowed(bsize) {
                let wctx = wedge_ctx(bsize);
                // COMP_INTER_WEDGE(4) - bit → 3 (SEG/diffwtd) or 4 (WEDGE).
                block_comp_type =
                    4 - self.dec.read_symbol(&mut self.mode_cdfs.wedge_comp[wctx]) as u8;
                if block_comp_type == 4 {
                    let _widx = self.dec.read_symbol(&mut self.mode_cdfs.wedge_idx[wctx]);
                }
                let _mask_sign = self.dec.read_bool();
            } else {
                block_comp_type = 3; // COMP_INTER_SEG
                let _mask_sign = self.dec.read_bool();
            }
            if dbg_b0 {
                eprintln!(
                    "DBG b0 comptype grp={} type={block_comp_type} rng={}",
                    comp_group_idx as u8,
                    self.dec.raw_state().0
                );
            }
        }

        // `read_interintra_mode()` (§5.11.28) — read right after the MV cascade
        // (dav1d `Post-interintra`) for a single-ref block whose size is in the
        // inter-intra-allowed set, when the sequence enables inter-intra
        // compound. One `interintra` bool; if set, an `interintra_mode` symbol
        // then (for wedge-allowed sizes) an `interintra_wedge` bool and
        // optionally a `wedge_idx`. Skipping this desynced every eligible block.
        let mut interintra_type = 0u8; // INTER_INTRA_NONE
        if self.enable_interintra
            && !compound
            && ref_names[1] == NONE_FRAME
            && interintra_allowed(bsize)
        {
            let grp = size_group(bsize);
            let is_ii = self.dec.read_symbol(&mut self.mode_cdfs.interintra[grp]) == 1;
            if is_ii {
                let _mode = self
                    .dec
                    .read_symbol(&mut self.mode_cdfs.interintra_mode[grp]);
                let wctx = wedge_ctx(bsize);
                // INTER_INTRA_BLEND (1) + wedge bit → BLEND or WEDGE (2).
                interintra_type = 1 + self
                    .dec
                    .read_symbol(&mut self.mode_cdfs.interintra_wedge[wctx])
                    as u8;
                if interintra_type == 2 {
                    let _widx = self.dec.read_symbol(&mut self.mode_cdfs.wedge_idx[wctx]);
                }
            }
            if dbg_b0 {
                eprintln!(
                    "DBG b0 interintra type={interintra_type} rng={}",
                    self.dec.raw_state().0
                );
            }
        }

        // `read_motion_mode()` (§5.11.23) — read after the MV/mode cascade and
        // before the interpolation filter. dav1d (`Post-motionmode`) reads a
        // symbol here whenever the block is single-ref, not skip_mode, at least
        // 8x8, `is_motion_mode_switchable`, ref[1] != INTRA_FRAME, and has an
        // overlappable (inter-coded) above/left neighbour. When a *matching-ref*
        // neighbour also exists (dav1d `find_matching_ref` mask nonzero) and
        // warped motion is enabled the 3-way `motion_mode` symbol is read,
        // otherwise the `use_obmc` bool. Full warp-sample derivation isn't
        // implemented, so a matching-ref neighbour is treated as `NumSamples>0`.
        let mut motion_mode = 0u8; // SIMPLE
        {
            let min_dim = BLOCK_WIDTH[bsize].min(BLOCK_HEIGHT[bsize]);
            let _ = single_mode; // GLOBALMV modelled translation-only: GmType never > TRANSLATION
            let eligible = !compound
                && self.is_motion_mode_switchable
                && min_dim >= 8
                && ref_names[1] == NONE_FRAME
                && interintra_type == 0
                && self.has_overlappable_candidates(mi_row, mi_col, bw, bh);
            if eligible {
                let matching_ref =
                    self.has_matching_ref_candidates(mi_row, mi_col, bw, bh, ref_names[0]);
                let allow_warp = self.allow_warped_motion && !force_integer_mv && matching_ref;
                if allow_warp {
                    motion_mode = self
                        .dec
                        .read_symbol(&mut self.mode_cdfs.motion_mode[bsize.min(21)])
                        as u8;
                } else {
                    motion_mode = self
                        .dec
                        .read_symbol(&mut self.mode_cdfs.use_obmc[bsize.min(21)])
                        as u8;
                }
                if dbg_b0 {
                    eprintln!(
                        "DBG b0 motion_mode={motion_mode} warp_allowed={allow_warp} rng={}",
                        self.dec.raw_state().0
                    );
                }
            }
        }
        // A WARP block reads no interpolation-filter symbol (dav1d sets
        // `has_subpel_filter = 0`).
        let frame_filter = if motion_mode == 2 {
            INTERP_EIGHTTAP_REGULAR
        } else {
            frame_filter
        };

        // Per-block interpolation filter (§5.11.27). When switchable, one
        // symbol per axis is read (`dir` 0 = vertical, 1 = horizontal) if
        // `enable_dual_filter`, otherwise a single shared symbol. dav1d
        // (`Post-subpel_filter1`/`filter2`) reads two whenever the sequence
        // header enables dual filters — reading only one desynced the entropy
        // decoder from the first inter block onward.
        let comp = usize::from(ref_names[1] != NONE_FRAME);
        if dbg_b0 {
            eprintln!(
                "DBG b0 frame_filter={frame_filter} dual={}",
                self.enable_dual_filter
            );
        }
        let mut filters = [frame_filter; 2];
        if frame_filter == INTERP_SWITCHABLE {
            let dirs = if self.enable_dual_filter { 2 } else { 1 };
            for (dir, fout) in filters.iter_mut().enumerate().take(dirs) {
                let base = ((dir & 1) * 2 + comp) * 4;
                let left_t = if left_inter != 0
                    && (self.ref_left[mi_row][0] == ref_names[0]
                        || self.ref_left[mi_row][1] == ref_names[0])
                {
                    self.filter_left[dir][mi_row] as usize
                } else {
                    3
                };
                let above_t = if above_inter != 0
                    && (self.ref_above[mi_col][0] == ref_names[0]
                        || self.ref_above[mi_col][1] == ref_names[0])
                {
                    self.filter_above[dir][mi_col] as usize
                } else {
                    3
                };
                let add = if left_t == above_t {
                    left_t
                } else if left_t == 3 {
                    above_t
                } else if above_t == 3 {
                    left_t
                } else {
                    3
                };
                let ctx = (base + add).min(15);
                *fout = self.dec.read_symbol(&mut self.mode_cdfs.interp_filter[ctx]) as u8;
                if dbg_b0 {
                    eprintln!(
                        "DBG b0 filter{dir}={} ctx={ctx} rng={}",
                        *fout,
                        self.dec.raw_state().0
                    );
                }
            }
            if dirs == 1 {
                filters[1] = filters[0];
            }
        }
        // MC currently applies a single kernel to both axes; use the vertical
        // filter (a full dual-axis kernel split is a follow-up).
        let filter = filters[0];

        // Motion-compensated prediction into the output planes (Y then chroma),
        // using the reference slots mapped from the reference names.
        let px_x0 = mi_col * MI_SIZE - self.tile_px_x0;
        let px_y0 = mi_row * MI_SIZE - self.tile_px_y0;
        let bw_px = bw * MI_SIZE;
        let bh_px = bh * MI_SIZE;

        // Compound blend weight (§7.11.3.15): `jnt_weight` for the
        // distance-weighted type, `8` (plain average) otherwise.
        let blend_weight = if compound && block_comp_type == 1 {
            let rp = |n: u8| {
                self.dpb_order_hints
                    .get(self.ref_to_slot[n as usize] as usize)
                    .copied()
                    .unwrap_or(0) as i32
            };
            jnt_weight(
                self.order_hint_bits,
                self.cur_order_hint as i32,
                rp(ref_names[0]),
                rp(ref_names[1]),
            )
        } else {
            8
        };

        // Y plane.
        self.inter_predict_plane(
            0,
            px_x0,
            px_y0,
            bw_px,
            bh_px,
            &ref_names,
            &mvs,
            filter,
            blend_weight,
        )?;
        // Chroma planes — `inter_predict_plane` interprets the luma MV at
        // 1/16-pel for the subsampled axes.
        let cpx_x0 = px_x0 / 2;
        let cpx_y0 = px_y0 / 2;
        let cbw_px = (bw_px / 2).max(4);
        let cbh_px = (bh_px / 2).max(4);
        self.inter_predict_plane(
            1,
            cpx_x0,
            cpx_y0,
            cbw_px,
            cbh_px,
            &ref_names,
            &mvs,
            filter,
            blend_weight,
        )?;
        self.inter_predict_plane(
            2,
            cpx_x0,
            cpx_y0,
            cbw_px,
            cbh_px,
            &ref_names,
            &mvs,
            filter,
            blend_weight,
        )?;

        // Residual. `read_block_tx_size` (§5.11.16) takes its inter/IBC branch
        // here (`IsInter == 1`): a recursive var-tx-tree of `txfm_split`
        // symbols (`read_block_tx_size_ibc`/`read_tx_tree`), NOT the
        // single-ternary `read_tx_size` used for real intra blocks — the wrong
        // syntax model desynced the entropy decoder from the first non-skip
        // inter block (dav1d `Post-vartxtree`). It also updates the shared
        // `tx_above`/`tx_left` neighbour context internally.
        let leaves = self.read_block_tx_size_ibc(mi_row, mi_col, bsize, skip);
        let luma_tx = leaves.first().map(|l| l.2).unwrap_or(TX_4X4);
        if dbg_b0 {
            eprintln!(
                "DBG b0 vartx leaves={} tx0={luma_tx} rng={}",
                leaves.len(),
                self.dec.raw_state().0
            );
        }
        self.add_inter_residual(mi_row, mi_col, bsize, skip, &leaves)?;
        if dbg_b0 {
            eprintln!("DBG b0 post-residual rng={}", self.dec.raw_state().0);
        }

        // Update inter neighbour state.
        let skip_byte = skip as u8;
        let luma_tx_w_byte = av1::TX_WIDTH[luma_tx] as u8;
        let luma_tx_h_byte = av1::TX_HEIGHT[luma_tx] as u8;
        // dav1d `BlockContext::comp_type` (from `read_compound_type`).
        let comp_type_byte = block_comp_type;
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(s) = self.is_inter_left.get_mut(r) {
                *s = 1;
            }
            if let Some(s) = self.comp_type_left.get_mut(r) {
                *s = comp_type_byte;
            }
            if let Some(slot) = self.ref_left.get_mut(r) {
                slot[0] = ref_names[0];
                slot[1] = ref_names[1];
            }
            if let Some(slot) = self.mv_left.get_mut(r) {
                slot[0] = mvs[0];
                slot[1] = mvs[1];
            }
            // Inter-coded blocks leave no intra mode / tx / skip context for
            // neighbours; AV1 treats their intra-mode neighbour as DC_PRED (0).
            if let Some(s) = self.ymode_left.get_mut(r) {
                *s = DC_PRED;
            }
            if let Some(s) = self.skip_left.get_mut(r) {
                *s = skip_byte;
            }
            if let Some(s) = self.skip_mode_left.get_mut(r) {
                *s = 0;
            }
            if let Some(s) = self.tx_left.get_mut(r) {
                *s = luma_tx_h_byte;
            }
            for (fv, arr) in filters.iter().zip(self.filter_left.iter_mut()) {
                if let Some(s) = arr.get_mut(r) {
                    *s = *fv;
                }
            }
        }
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(s) = self.is_inter_above.get_mut(c) {
                *s = 1;
            }
            if let Some(s) = self.comp_type_above.get_mut(c) {
                *s = comp_type_byte;
            }
            if let Some(slot) = self.ref_above.get_mut(c) {
                slot[0] = ref_names[0];
                slot[1] = ref_names[1];
            }
            if let Some(slot) = self.mv_above.get_mut(c) {
                slot[0] = mvs[0];
                slot[1] = mvs[1];
            }
            if let Some(s) = self.ymode_above.get_mut(c) {
                *s = DC_PRED;
            }
            if let Some(s) = self.skip_above.get_mut(c) {
                *s = skip_byte;
            }
            if let Some(s) = self.skip_mode_above.get_mut(c) {
                *s = 0;
            }
            if let Some(s) = self.tx_above.get_mut(c) {
                *s = luma_tx_w_byte;
            }
            for (fv, arr) in filters.iter().zip(self.filter_above.iter_mut()) {
                if let Some(s) = arr.get_mut(c) {
                    *s = *fv;
                }
            }
        }
        // 2-D ref-MV grid: record this block's refs + MVs for later stacks.
        self.splat_refmv_full(mi_row, mi_col, bsize, ref_names, mvs, new_mf);
        Ok(())
    }

    /// A `skip_mode` block (§7.11.3 / §5.11.11): compound prediction from the
    /// fixed `SkipModeFrame` pair with the NEAREST spatial MVs, no residual, no
    /// further entropy reads. The NEAREST MV derivation is the simplified
    /// spatial-only `build_mv_candidates` (a full compound `find_mv_stack` is
    /// still pending), so the prediction is close but not yet bit-exact.
    fn decode_skip_mode_block(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
    ) -> Result<(), KinetixError> {
        let bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh = BLOCK_HEIGHT[bsize] / MI_SIZE;

        // §5.11.18: cdef / delta_q / delta_lf still follow, with skip = 1.
        self.read_cdef(mi_row, mi_col, bsize, true);
        self.read_delta_qindex(bsize, true);
        self.read_delta_lf(bsize, true);
        self.read_deltas = false;

        let ref_names = self.skip_mode_frame;
        let above = [
            (self.ref_above[mi_col][0], self.mv_above[mi_col][0]),
            (self.ref_above[mi_col][1], self.mv_above[mi_col][1]),
        ];
        let left = [
            (self.ref_left[mi_row][0], self.mv_left[mi_row][0]),
            (self.ref_left[mi_row][1], self.mv_left[mi_row][1]),
        ];
        let mut mvs = [Mv::default(); 2];
        for (i, mv) in mvs.iter_mut().enumerate() {
            let cands = build_mv_candidates(&above, &left, &[ref_names[i]], 2);
            *mv = cands.first().map(|c| c.mv).unwrap_or_default();
        }

        let px_x0 = mi_col * MI_SIZE - self.tile_px_x0;
        let px_y0 = mi_row * MI_SIZE - self.tile_px_y0;
        let bw_px = bw * MI_SIZE;
        let bh_px = bh * MI_SIZE;
        let f = if self.interpolation_filter == INTERP_SWITCHABLE {
            0
        } else {
            self.interpolation_filter
        };
        // Skip-mode blocks are `COMP_INTER_AVG` (plain average, weight 8).
        self.inter_predict_plane(0, px_x0, px_y0, bw_px, bh_px, &ref_names, &mvs, f, 8)?;
        let (cpx_x0, cpx_y0) = (px_x0 / 2, px_y0 / 2);
        let (cbw, cbh) = ((bw_px / 2).max(4), (bh_px / 2).max(4));
        self.inter_predict_plane(1, cpx_x0, cpx_y0, cbw, cbh, &ref_names, &mvs, f, 8)?;
        self.inter_predict_plane(2, cpx_x0, cpx_y0, cbw, cbh, &ref_names, &mvs, f, 8)?;

        // Skip-mode blocks are always `skip = 1`: `read_block_tx_size` takes
        // its no-entropy-read branch (uniform max transform).
        let leaves = self.read_block_tx_size_ibc(mi_row, mi_col, bsize, true);
        let luma_tx = leaves.first().map(|l| l.2).unwrap_or(TX_4X4);
        self.add_inter_residual(mi_row, mi_col, bsize, true, &leaves)?;

        let luma_tx_w = av1::TX_WIDTH[luma_tx] as u8;
        let luma_tx_h = av1::TX_HEIGHT[luma_tx] as u8;
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(s) = self.is_inter_left.get_mut(r) {
                *s = 1;
            }
            if let Some(s) = self.comp_type_left.get_mut(r) {
                *s = 2; // skip-mode blocks are COMP_INTER_AVG
            }
            if let Some(slot) = self.ref_left.get_mut(r) {
                *slot = ref_names;
            }
            if let Some(slot) = self.mv_left.get_mut(r) {
                *slot = mvs;
            }
            if let Some(s) = self.ymode_left.get_mut(r) {
                *s = DC_PRED;
            }
            if let Some(s) = self.skip_left.get_mut(r) {
                *s = 1;
            }
            if let Some(s) = self.skip_mode_left.get_mut(r) {
                *s = 1;
            }
            if let Some(s) = self.tx_left.get_mut(r) {
                *s = luma_tx_h;
            }
            for arr in self.filter_left.iter_mut() {
                if let Some(s) = arr.get_mut(r) {
                    *s = f;
                }
            }
        }
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(s) = self.is_inter_above.get_mut(c) {
                *s = 1;
            }
            if let Some(s) = self.comp_type_above.get_mut(c) {
                *s = 2; // skip-mode blocks are COMP_INTER_AVG
            }
            if let Some(slot) = self.ref_above.get_mut(c) {
                *slot = ref_names;
            }
            if let Some(slot) = self.mv_above.get_mut(c) {
                *slot = mvs;
            }
            if let Some(s) = self.ymode_above.get_mut(c) {
                *s = DC_PRED;
            }
            if let Some(s) = self.skip_above.get_mut(c) {
                *s = 1;
            }
            if let Some(s) = self.skip_mode_above.get_mut(c) {
                *s = 1;
            }
            if let Some(s) = self.tx_above.get_mut(c) {
                *s = luma_tx_w;
            }
            for arr in self.filter_above.iter_mut() {
                if let Some(s) = arr.get_mut(c) {
                    *s = f;
                }
            }
        }
        // Populate the 2-D reference-MV grid so a later block's compound
        // `find_mv_stack` can see this skip-mode block's ref pair + MVs.
        self.splat_refmv_full(mi_row, mi_col, bsize, ref_names, mvs, 0);
        Ok(())
    }

    /// Motion-compensate one plane for an inter block: for single reference, copy
    /// the reference block; for compound, average the two reference predictions.
    /// Writes directly into the tile-local plane (the prediction values).
    #[allow(clippy::too_many_arguments)]
    fn inter_predict_plane(
        &mut self,
        plane: usize,
        px_x: usize,
        px_y: usize,
        bw: usize,
        bh: usize,
        ref_names: &[u8; 2],
        mvs: &[Mv; 2],
        filter: u8,
        // Compound blend weight in sixteenths for `preds[0]` (`8` = plain
        // average); ignored for single-reference blocks.
        blend_weight: i32,
    ) -> Result<(), KinetixError> {
        let stride = match plane {
            1 | 2 => self.uv_stride,
            _ => self.y_stride,
        };
        let w = match plane {
            1 | 2 => self.tile_cw,
            _ => self.tile_w,
        };
        let h = match plane {
            1 | 2 => self.tile_ch,
            _ => self.tile_h,
        };
        // MV sub-pel precision per axis: the caller passes the *luma* MV for
        // every plane; a subsampled chroma axis interprets it at 1/16-pel.
        let (hbits, vbits) = if plane == 0 {
            (3u32, 3u32)
        } else {
            (3 + self.subsampling_x as u32, 3 + self.subsampling_y as u32)
        };

        let slot0 = self.ref_to_slot[ref_names[0] as usize] as usize;
        let slot1 = self.ref_to_slot[ref_names[1] as usize] as usize;
        let ref1_none = self.ref_slots.slots[slot1].is_none();
        let use_compound = ref_names[1] != NONE_FRAME && !ref1_none;

        // Single reference: motion-compensate into a local temp (so we don't hold
        // both the reference slice and the output plane borrow at once), then blit.
        if !use_compound {
            let tmp = {
                let mut t = vec![0u8; bw * bh];
                if let Some(rf) = self.ref_slots.slots[slot0] {
                    let (rp, rw, rh) = rf.plane(plane);
                    motion_compensate(
                        &mut t, bw, rp, rw, rw, rh, px_x, px_y, bw, bh, mvs[0], filter, hbits,
                        vbits,
                    );
                }
                t
            };
            for dy in 0..bh {
                let sy = px_y + dy;
                if sy >= h {
                    break;
                }
                for dx in 0..bw {
                    let sx = px_x + dx;
                    if sx >= w {
                        break;
                    }
                    let v = tmp[dy * bw + dx];
                    match plane {
                        1 => self.u_plane[sy * stride + sx] = v,
                        2 => self.v_plane[sy * stride + sx] = v,
                        _ => self.y_plane[sy * stride + sx] = v,
                    }
                }
            }
            return Ok(());
        }

        // Compound: blend the two predictions in the higher-precision
        // intermediate domain (§7.11.3.1 `avg` / `w_avg`). Wedge / diffwtd
        // masks are not yet generated — those fall through to `blend_weight`
        // (a plain average unless the caller narrowed it).
        let combined = {
            let prep = |slot: usize, mv: Mv| -> Vec<i32> {
                if let Some(rf) = self.ref_slots.slots[slot] {
                    let (rp, rw, rh) = rf.plane(plane);
                    motion_compensate_prep(
                        rp, rw, rw, rh, px_x, px_y, bw, bh, mv, filter, hbits, vbits,
                    )
                } else {
                    vec![0i32; bw * bh]
                }
            };
            let t0 = prep(slot0, mvs[0]);
            let t1 = prep(slot1, mvs[1]);
            compound_blend(&t0, &t1, blend_weight)
        };
        for dy in 0..bh {
            let sy = px_y + dy;
            if sy >= h {
                break;
            }
            for dx in 0..bw {
                let sx = px_x + dx;
                if sx >= w {
                    break;
                }
                let v = combined[dy * bw + dx];
                match plane {
                    1 => self.u_plane[sy * stride + sx] = v,
                    2 => self.v_plane[sy * stride + sx] = v,
                    _ => self.y_plane[sy * stride + sx] = v,
                }
            }
        }
        Ok(())
    }

    /// Read the residual coefficients per transform block of an inter block and add
    /// them to the motion-compensated prediction already present in the planes.
    #[allow(clippy::too_many_arguments)]
    fn add_inter_residual(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
        skip: bool,
        leaves: &[(usize, usize, usize)],
    ) -> Result<(), KinetixError> {
        let bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh = BLOCK_HEIGHT[bsize] / MI_SIZE;
        // Uniform-grid fallback size for the (common) non-split case, used for
        // the chroma-tx heuristic and the loop-filter metadata grid.
        let luma_tx = leaves.first().map(|l| l.2).unwrap_or(TX_4X4);
        let luma_tx_w = av1::TX_WIDTH[luma_tx];
        let luma_tx_h = av1::TX_HEIGHT[luma_tx];
        let subsampling_x = self.subsampling_x as u8;
        let subsampling_y = self.subsampling_y as u8;

        // The `FrameMeta` geometry recording below always runs (the deblock
        // filter needs every inter block's transform geometry, skipped or
        // not); only the coefficient read is gated on `!skip`, per var-tx
        // leaf for luma and per chroma-tx block for chroma.

        // Y residual + per-transform-sub-block deblock-edge geometry
        // (mirrors the intra keyframe path in `intra_block.rs` — see its
        // identical `mark_luma_edges`/`mark_luma_edges4`/`record_luma4`
        // calls for why this must run per transform sub-block, not just
        // once per coded block).
        for &(leaf_mi_col, leaf_mi_row, leaf_tx) in leaves {
            let leaf_tx_w = av1::TX_WIDTH[leaf_tx];
            let leaf_tx_h = av1::TX_HEIGHT[leaf_tx];
            let px_x = leaf_mi_col * MI_SIZE - self.tile_px_x0;
            let px_y = leaf_mi_row * MI_SIZE - self.tile_px_y0;
            self.meta.mark_luma_edges(
                px_x / 8,
                px_y / 8,
                (px_x + leaf_tx_w).div_ceil(8),
                (px_y + leaf_tx_h).div_ceil(8),
            );
            self.meta.mark_luma_edges4(
                px_x / 4,
                px_y / 4,
                (px_x + leaf_tx_w).div_ceil(4),
                (px_y + leaf_tx_h).div_ceil(4),
            );
            self.meta.record_luma4(
                px_x / 4,
                px_y / 4,
                (px_x + leaf_tx_w).div_ceil(4),
                (px_y + leaf_tx_h).div_ceil(4),
                leaf_tx_w as u8,
                leaf_tx_h as u8,
            );
            let mut residual = vec![0i32; leaf_tx_w * leaf_tx_h];
            let blk = TxBlockCtx {
                plane: 0,
                tx_size: leaf_tx,
                x4: px_x / 4,
                y4: px_y / 4,
                max_x4: self.luma_max_x4,
                max_y4: self.luma_max_y4,
                // See the matching fix/comment in `intra_block.rs`: the
                // *coded block's* plane size, not this transform block's.
                block_w: bw * MI_SIZE,
                block_h: bh * MI_SIZE,
                intra_dir: 0,
                uv_mode: 0,
                qindex_positive: !self.lossless,
                reduced_tx_set: self.reduced_tx_set,
                lossless: self.lossless,
                is_inter: true,
                // Irrelevant for plane 0.
                coincident_luma_tx_type: av1::DCT_DCT,
            };
            if skip {
                // A skipped block reads no coeffs but still must reset the
                // neighbour context (see `clear_coeff_context`).
                clear_coeff_context(&mut self.coeff_ctxs, &blk, leaf_tx_w / 4, leaf_tx_h / 4);
            } else {
                let coeffs = read_coeffs(
                    &mut self.dec,
                    &mut self.coeff_cdfs,
                    &mut self.coeff_ctxs,
                    &blk,
                )?;
                // Coeffs are always *read* (entropy sync). The inverse
                // transform is applied only for `Tx_Size_Sqr_Up <= 16x16` —
                // the larger inter transforms are not yet conformance-checked
                // and produce a worse residual than none (regresses
                // `av1_inter_sequence` frame 2). TODO: verify the 32x32/64x64
                // inter inverse-transform + tx_type path, then widen.
                // Coeffs are always *read* (entropy sync — verified rng-exact
                // vs dav1d incl. the large `TX_64X32` leaf, and the DC-only
                // inverse transform is flat at every rect size). Applying the
                // 32/64-family residual still regresses `av1_inter_sequence`
                // frame 2 (4.7k→10k) — most likely the *cascade*: it is being
                // added onto an inter *prediction* that is itself still
                // approximate (compound blend, no OBMC/warp), so "correct
                // residual + wrong base" is worse than "small base alone".
                // Widen once inter prediction is bit-exact.
                if coeffs.eob > 0 && av1::TX_SIZE_SQR_UP[leaf_tx] <= TX_16X16 {
                    let (qindex_dc, qindex_ac) = self.qindex_for_plane(0);
                    let dequant = dequantize_coeffs(&coeffs.quant, leaf_tx, qindex_dc, qindex_ac);
                    inverse_transform(
                        &dequant,
                        coeffs.tx_type,
                        leaf_tx,
                        self.lossless,
                        &mut residual,
                    );
                }
            }
            for dy in 0..leaf_tx_h {
                let sy = px_y + dy;
                if sy >= self.tile_h {
                    break;
                }
                for dx in 0..leaf_tx_w {
                    let sx = px_x + dx;
                    if sx >= self.tile_w {
                        break;
                    }
                    if let Some(slot) = self.y_plane.get_mut(sy * self.y_stride + sx) {
                        *slot =
                            ((*slot as i32 + residual[dy * leaf_tx_w + dx]).clamp(0, 255)) as u8;
                    }
                }
            }
        }

        // Fixed 8×8-luma-grid loop-filter metadata (mirrors the intra
        // keyframe path's identical call in `intra_block.rs`).
        let blk_px_x = mi_col * MI_SIZE - self.tile_px_x0;
        let blk_px_y = mi_row * MI_SIZE - self.tile_px_y0;
        let bx0 = blk_px_x / 8;
        let by0 = blk_px_y / 8;
        let bx1 = (blk_px_x + bw * MI_SIZE).div_ceil(8);
        let by1 = (blk_px_y + bh * MI_SIZE).div_ceil(8);
        for by in by0..by1.min(self.meta.h8) {
            for bx in bx0..bx1.min(self.meta.w8) {
                self.meta
                    .record_luma(bx, by, luma_tx_w as u8, luma_tx_h as u8, skip);
            }
        }
        self.meta.record_delta_lf(bx0, by0, bx1, by1, self.delta_lf);
        self.meta.record_delta_lf4(
            blk_px_x / 4,
            blk_px_y / 4,
            (blk_px_x + bw * MI_SIZE).div_ceil(4),
            (blk_px_y + bh * MI_SIZE).div_ceil(4),
            self.delta_lf,
        );

        // Chroma residual. Inter chroma uses one uniform transform size
        // (`get_tx_size(get_plane_residual_size)`, §5.11.37) tiling the whole
        // block's chroma region — dav1d reads it (`Post-uv-cf-blk`) after all
        // the luma leaves, `pl=0` then `pl=1`.
        let sub_x = self.subsampling_x as usize;
        let sub_y = self.subsampling_y as usize;
        let c_tx = chroma_tx_size(bsize, sub_x, sub_y);
        let cw = av1::TX_WIDTH[c_tx];
        let ch = av1::TX_HEIGHT[c_tx];
        let plane_sz = {
            let sz = get_plane_residual_size(bsize, sub_x, sub_y);
            if sz == BLOCK_INVALID {
                bsize
            } else {
                sz
            }
        };
        let chroma_bw = BLOCK_WIDTH[plane_sz];
        let chroma_bh = BLOCK_HEIGHT[plane_sz];
        let base_cpx_x = (mi_col >> sub_x) * MI_SIZE - (self.tile_px_x0 >> sub_x);
        let base_cpx_y = (mi_row >> sub_y) * MI_SIZE - (self.tile_px_y0 >> sub_y);
        let has_residual = !skip;
        // Computed before the `&mut self.{u,v}_plane` reborrows in the loop
        // below — `qindex_for_plane` takes `&self`, which would conflict
        // with those live disjoint-field mutable borrows if called any later.
        let (u_qindex_dc, u_qindex_ac) = self.qindex_for_plane(1);
        let (v_qindex_dc, v_qindex_ac) = self.qindex_for_plane(2);
        for ty in (0..chroma_bh).step_by(ch) {
            for tx in (0..chroma_bw).step_by(cw) {
                let cpx_x = base_cpx_x + tx;
                let cpx_y = base_cpx_y + ty;
                if cpx_x >= self.tile_cw || cpx_y >= self.tile_ch {
                    continue;
                }
                // Real per-transform-sub-block chroma deblock-edge geometry
                // — mirrors the intra keyframe path's identical
                // `mark_chroma_edges` call in `intra_block.rs`.
                self.meta.mark_chroma_edges(
                    cpx_x / 4,
                    cpx_y / 4,
                    (cpx_x + cw).div_ceil(4),
                    (cpx_y + ch).div_ceil(4),
                );
                for (plane, dst, stride, w, h) in [
                    (
                        1usize,
                        &mut *self.u_plane,
                        self.uv_stride,
                        self.tile_cw,
                        self.tile_ch,
                    ),
                    (
                        2usize,
                        &mut *self.v_plane,
                        self.uv_stride,
                        self.tile_cw,
                        self.tile_ch,
                    ),
                ] {
                    let mut residual = vec![0i32; cw * ch];
                    if has_residual {
                        let blk = TxBlockCtx {
                            plane,
                            tx_size: c_tx,
                            x4: cpx_x / 4,
                            y4: cpx_y / 4,
                            max_x4: self.uv_max_x4,
                            max_y4: self.uv_max_y4,
                            // See the luma fix above: the coded block's
                            // chroma-plane size, not this transform block's
                            // own `cw`/`ch`. This inter chroma path already
                            // approximates the true `get_plane_residual_size`
                            // (see the `c_tx` heuristic above it), so this
                            // matches that same approximation rather than the
                            // exact spec table.
                            block_w: (bw * MI_SIZE) >> subsampling_x,
                            block_h: (bh * MI_SIZE) >> subsampling_y,
                            intra_dir: 0,
                            uv_mode: 0,
                            qindex_positive: !self.lossless,
                            reduced_tx_set: self.reduced_tx_set,
                            lossless: self.lossless,
                            is_inter: true,
                            // TODO(inter Phase E): like IBC's chroma path
                            // before its own fix, this needs the real
                            // coincident luma leaf's decoded `TxType`
                            // (`intra_block.rs`'s `luma_tx_types` lookup),
                            // not a `DCT_DCT` placeholder — not fixed here
                            // since this path isn't reached yet (`decode_
                            // inter_block` returns `Ok(None)` for
                            // non-keyframes).
                            coincident_luma_tx_type: av1::DCT_DCT,
                        };
                        let coeffs = read_coeffs(
                            &mut self.dec,
                            &mut self.coeff_cdfs,
                            &mut self.coeff_ctxs,
                            &blk,
                        )?;
                        if coeffs.eob > 0 {
                            let (qindex_dc, qindex_ac) = if plane == 1 {
                                (u_qindex_dc, u_qindex_ac)
                            } else {
                                (v_qindex_dc, v_qindex_ac)
                            };
                            let dequant =
                                dequantize_coeffs(&coeffs.quant, c_tx, qindex_dc, qindex_ac);
                            inverse_transform(
                                &dequant,
                                coeffs.tx_type,
                                c_tx,
                                self.lossless,
                                &mut residual,
                            );
                        }
                    }
                    for dy in 0..ch {
                        let sy = cpx_y + dy;
                        if sy >= h {
                            break;
                        }
                        for dx in 0..cw {
                            let sx = cpx_x + dx;
                            if sx >= w {
                                break;
                            }
                            if let Some(slot) = dst.get_mut(sy * stride + sx) {
                                *slot =
                                    ((*slot as i32 + residual[dy * cw + dx]).clamp(0, 255)) as u8;
                            }
                        }
                    }
                }
            }
        }
        // Record chroma tx/skip metadata for the same 8×8-luma grid region
        // (mirrors the intra keyframe path's identical call).
        let c_tx_w = av1::TX_WIDTH[c_tx] as u8;
        let c_tx_h = av1::TX_HEIGHT[c_tx] as u8;
        for by in by0..by1.min(self.meta.h8) {
            for bx in bx0..bx1.min(self.meta.w8) {
                self.meta.record_chroma(bx, by, c_tx_w, c_tx_h, skip);
            }
        }
        Ok(())
    }
}
