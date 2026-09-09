use super::*;

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
        let dbg_b0 = std::env::var("KINETIX_AV1_DBG_B0").is_ok() && mi_row == 0 && mi_col <= 32;
        if dbg_b0 {
            eprintln!("DBG b0 skip={skip} rng={}", self.dec.raw_state().0);
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
            }
            for c in mi_col..(mi_col + bw).min(self.mi_cols) {
                if let Some(s) = self.is_inter_above.get_mut(c) {
                    *s = 0;
                }
            }
            return Ok(());
        }

        // Compound vs single reference (§6.8.2). comp_mode is read only when
        // compound prediction is allowed (here: `reference_select`).
        let compound = if reference_select {
            self.dec.read_symbol(&mut self.map_inter_cdfs.comp_mode[0]) == 1
        } else {
            false
        };

        // Reference name(s).
        let mut ref_names = [NONE_FRAME; 2];
        if compound {
            // Compound reference-frame tree (§6.8.2). We consume the same symbols
            // the encoder wrote to stay in bit-sync; the actual forward/backward
            // names are derived from the same decisions.
            let _ct = self
                .dec
                .read_symbol(&mut self.map_inter_cdfs.comp_ref_type[0]);
            let fwd = if self
                .dec
                .read_symbol(&mut self.map_inter_cdfs.uni_comp_ref[0][0])
                == 0
            {
                if self
                    .dec
                    .read_symbol(&mut self.map_inter_cdfs.uni_comp_ref[0][1])
                    == 0
                {
                    LAST_FRAME
                } else {
                    LAST2_FRAME
                }
            } else if self
                .dec
                .read_symbol(&mut self.map_inter_cdfs.uni_comp_ref[0][2])
                == 0
            {
                LAST3_FRAME
            } else {
                GOLDEN_FRAME
            };
            let bwd = if self
                .dec
                .read_symbol(&mut self.map_inter_cdfs.uni_comp_ref[1][0])
                == 0
            {
                if self
                    .dec
                    .read_symbol(&mut self.map_inter_cdfs.comp_ref[0][0])
                    == 0
                {
                    BWDREF_FRAME
                } else {
                    ALTREF_FRAME
                }
            } else if self
                .dec
                .read_symbol(&mut self.map_inter_cdfs.comp_bwd_ref[0][0])
                == 0
            {
                ALTREF2_FRAME
            } else {
                BWDREF_FRAME
            };
            ref_names = [fwd, bwd];
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

        if !compound {
            // AV1 §7.10.2 `find_mv_stack` + §5.11.24 single-ref mode cascade.
            let (stack, ctx, n_mvs, drl_ctx) =
                self.inter_mv_stack(mi_row, mi_col, bsize, ref_names);
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
            // Compound path — still the simplified spatial-candidate build.
            let above = [
                (self.ref_above[mi_col][0], self.mv_above[mi_col][0]),
                (self.ref_above[mi_col][1], self.mv_above[mi_col][1]),
            ];
            let left = [
                (self.ref_left[mi_row][0], self.mv_left[mi_row][0]),
                (self.ref_left[mi_row][1], self.mv_left[mi_row][1]),
            ];
            let block_refs: Vec<u8> = ref_names
                .iter()
                .copied()
                .filter(|r| *r != NONE_FRAME)
                .collect();
            let candidates = build_mv_candidates(&above, &left, &block_refs, 2);
            for i in 0..2 {
                let r = ref_names[i];
                if r == NONE_FRAME {
                    continue;
                }
                let (_rn, mv) = decode_ref_and_mv(
                    &mut self.dec,
                    &mut self.map_inter_cdfs,
                    r,
                    &candidates,
                    allow_hp,
                    force_integer_mv,
                    0,
                    false,
                )?;
                mvs[i] = mv;
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

        // Y plane.
        self.inter_predict_plane(0, px_x0, px_y0, bw_px, bh_px, &ref_names, &mvs, filter)?;
        // Chroma planes (sub-sampled MV).
        let cmv: [Mv; 2] = [mvs[0].scaled_chroma(), mvs[1].scaled_chroma()];
        let cpx_x0 = px_x0 / 2;
        let cpx_y0 = px_y0 / 2;
        let cbw_px = (bw_px / 2).max(4);
        let cbh_px = (bh_px / 2).max(4);
        self.inter_predict_plane(1, cpx_x0, cpx_y0, cbw_px, cbh_px, &ref_names, &cmv, filter)?;
        self.inter_predict_plane(2, cpx_x0, cpx_y0, cbw_px, cbh_px, &ref_names, &cmv, filter)?;

        // Residual: read coefficients per transform block and add to the
        // prediction already written into the planes. The luma tx size is read
        // once here (it also drives the neighbour-context update below).
        let max_tx = max_tx_size_for_bsize(bsize);
        let luma_tx = if !skip && self.tx_mode_select && !self.lossless {
            self.read_tx_size(bsize, max_tx, mi_row, mi_col)
        } else {
            max_tx
        };
        self.add_inter_residual(mi_row, mi_col, bsize, skip, luma_tx)?;

        // Update inter neighbour state.
        let skip_byte = skip as u8;
        let luma_tx_w_byte = av1::TX_WIDTH[luma_tx] as u8;
        let luma_tx_h_byte = av1::TX_HEIGHT[luma_tx] as u8;
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(s) = self.is_inter_left.get_mut(r) {
                *s = 1;
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
        self.inter_predict_plane(0, px_x0, px_y0, bw_px, bh_px, &ref_names, &mvs, f)?;
        let cmv: [Mv; 2] = [mvs[0].scaled_chroma(), mvs[1].scaled_chroma()];
        let (cpx_x0, cpx_y0) = (px_x0 / 2, px_y0 / 2);
        let (cbw, cbh) = ((bw_px / 2).max(4), (bh_px / 2).max(4));
        self.inter_predict_plane(1, cpx_x0, cpx_y0, cbw, cbh, &ref_names, &cmv, f)?;
        self.inter_predict_plane(2, cpx_x0, cpx_y0, cbw, cbh, &ref_names, &cmv, f)?;

        let luma_tx = max_tx_size_for_bsize(bsize);
        self.add_inter_residual(mi_row, mi_col, bsize, true, luma_tx)?;

        let luma_tx_w = av1::TX_WIDTH[luma_tx] as u8;
        let luma_tx_h = av1::TX_HEIGHT[luma_tx] as u8;
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(s) = self.is_inter_left.get_mut(r) {
                *s = 1;
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
                        &mut t, bw, rp, rw, rw, rh, px_x, px_y, bw, bh, mvs[0], filter,
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

        // Compound: average the two predictions into a temp, then write.
        let combined = {
            let mut t0 = vec![0u8; bw * bh];
            let mut t1 = vec![0u8; bw * bh];
            if let Some(rf) = self.ref_slots.slots[slot0] {
                let (rp, rw, rh) = rf.plane(plane);
                motion_compensate(
                    &mut t0, bw, rp, rw, rw, rh, px_x, px_y, bw, bh, mvs[0], filter,
                );
            }
            if let Some(rf) = self.ref_slots.slots[slot1] {
                let (rp, rw, rh) = rf.plane(plane);
                motion_compensate(
                    &mut t1, bw, rp, rw, rw, rh, px_x, px_y, bw, bh, mvs[1], filter,
                );
            }
            let mut c = vec![0u8; bw * bh];
            for i in 0..bw * bh {
                c[i] = ((t0[i] as u32 + t1[i] as u32 + 1) >> 1) as u8;
            }
            c
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
        luma_tx: usize,
    ) -> Result<(), KinetixError> {
        let bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh = BLOCK_HEIGHT[bsize] / MI_SIZE;
        let luma_tx_w = av1::TX_WIDTH[luma_tx];
        let luma_tx_h = av1::TX_HEIGHT[luma_tx];
        let subsampling_x = self.subsampling_x as u8;
        let subsampling_y = self.subsampling_y as u8;

        // Whether to actually read residual coefficients — the previous
        // version of this function returned early here for either
        // condition, which also skipped every `FrameMeta` recording call
        // below (`mark_luma_edges`/`record_luma`/etc. never ran for *any*
        // inter block, skipped or not: `inter_block.rs` had no `self.meta`
        // references at all). That left the deblock filter blind to every
        // inter-coded block's transform geometry on every P/B frame — see
        // todo-av1.md's "inter FrameMeta gap" note. `TxSize` (and therefore
        // the real transform-edge geometry) is well-defined regardless of
        // `skip`/`luma_tx`, so the geometry recording below always runs;
        // only the actual coefficient read is gated on `has_residual`.
        let has_residual = !skip && luma_tx <= TX_16X16;

        // Y residual + per-transform-sub-block deblock-edge geometry
        // (mirrors the intra keyframe path in `intra_block.rs` — see its
        // identical `mark_luma_edges`/`mark_luma_edges4`/`record_luma4`
        // calls for why this must run per transform sub-block, not just
        // once per coded block).
        for ty in (0..bh * MI_SIZE).step_by(luma_tx_h) {
            for tx in (0..bw * MI_SIZE).step_by(luma_tx_w) {
                let px_x = mi_col * MI_SIZE + tx - self.tile_px_x0;
                let px_y = mi_row * MI_SIZE + ty - self.tile_px_y0;
                self.meta.mark_luma_edges(
                    px_x / 8,
                    px_y / 8,
                    (px_x + luma_tx_w).div_ceil(8),
                    (px_y + luma_tx_h).div_ceil(8),
                );
                self.meta.mark_luma_edges4(
                    px_x / 4,
                    px_y / 4,
                    (px_x + luma_tx_w).div_ceil(4),
                    (px_y + luma_tx_h).div_ceil(4),
                );
                self.meta.record_luma4(
                    px_x / 4,
                    px_y / 4,
                    (px_x + luma_tx_w).div_ceil(4),
                    (px_y + luma_tx_h).div_ceil(4),
                    luma_tx_w as u8,
                    luma_tx_h as u8,
                );
                let mut residual = vec![0i32; luma_tx_w * luma_tx_h];
                if has_residual {
                    let blk = TxBlockCtx {
                        plane: 0,
                        tx_size: luma_tx,
                        x4: px_x / 4,
                        y4: px_y / 4,
                        max_x4: self.luma_max_x4,
                        max_y4: self.luma_max_y4,
                        // See the matching fix/comment in `intra_block.rs`:
                        // this must be the *coded block's* plane size
                        // (`bw`/`bh` in samples), not this transform block's
                        // own `luma_tx_w`/`_h` — otherwise `all_zero`'s
                        // whole-block `ctx = 0` special case fires
                        // unconditionally.
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
                    let coeffs = read_coeffs(
                        &mut self.dec,
                        &mut self.coeff_cdfs,
                        &mut self.coeff_ctxs,
                        &blk,
                    )?;
                    if coeffs.eob > 0 {
                        let (qindex_dc, qindex_ac) = self.qindex_for_plane(0);
                        let dequant =
                            dequantize_coeffs(&coeffs.quant, luma_tx, qindex_dc, qindex_ac);
                        inverse_transform(
                            &dequant,
                            coeffs.tx_type,
                            luma_tx,
                            self.lossless,
                            &mut residual,
                        );
                    }
                }
                for dy in 0..luma_tx_h {
                    let sy = px_y + dy;
                    if sy >= self.tile_h {
                        break;
                    }
                    for dx in 0..luma_tx_w {
                        let sx = px_x + dx;
                        if sx >= self.tile_w {
                            break;
                        }
                        if let Some(slot) = self.y_plane.get_mut(sy * self.y_stride + sx) {
                            *slot = ((*slot as i32 + residual[dy * luma_tx_w + dx]).clamp(0, 255))
                                as u8;
                        }
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

        // Chroma residual.
        let cw = (luma_tx_w >> subsampling_x).max(4);
        let ch = (luma_tx_h >> subsampling_y).max(4);
        let c_tx = if cw >= 16 && ch >= 16 {
            TX_16X16
        } else if cw >= 8 && ch >= 8 {
            TX_8X8
        } else {
            TX_4X4
        };
        // Computed before the `&mut self.{u,v}_plane` reborrows in the loop
        // below — `qindex_for_plane` takes `&self`, which would conflict
        // with those live disjoint-field mutable borrows if called any later.
        let (u_qindex_dc, u_qindex_ac) = self.qindex_for_plane(1);
        let (v_qindex_dc, v_qindex_ac) = self.qindex_for_plane(2);
        for ty in (0..bh * MI_SIZE).step_by(luma_tx_h) {
            for tx in (0..bw * MI_SIZE).step_by(luma_tx_w) {
                let cpx_x = (mi_col * MI_SIZE + tx - self.tile_px_x0) >> subsampling_x;
                let cpx_y = (mi_row * MI_SIZE + ty - self.tile_px_y0) >> subsampling_y;
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
