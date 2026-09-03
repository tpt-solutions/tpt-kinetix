use super::*;

impl<'a> TileDecodeState<'a> {
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

        // Skip flag (§5.11.11) — read before `is_inter`, matching the inter
        // syntax order.
        let above_skip = self.skip_above[mi_col] as usize;
        let left_skip = self.skip_left[mi_row] as usize;
        let above_inter = self.is_inter_above[mi_col] as usize;
        let left_inter = self.is_inter_left[mi_row] as usize;
        let skip = if self.seg_feature_skip {
            true
        } else {
            self.mode_cdfs
                .read_skip(&mut self.dec, (above_skip + left_skip).min(2))
                == 1
        };

        // AV1 spec §5.11.18 `inter_frame_mode_info()`: `read_cdef()`/
        // `read_delta_qindex()`/`read_delta_lf()` come right after
        // `read_skip()`, then `ReadDeltas = 0`, before `read_is_inter()` —
        // same order and same no-op-unless-enabled behaviour as the intra
        // path (`intra_block.rs`).
        self.read_cdef(mi_row, mi_col, bsize, skip);
        self.read_delta_qindex(bsize, skip);
        self.read_delta_lf(bsize, skip);
        self.read_deltas = false;

        // is_inter (Y) — context from neighbour inter flags.
        let inter_ctx = (left_inter + above_inter).min(3);
        let is_inter = self
            .dec
            .read_symbol(&mut self.map_inter_cdfs.is_inter[inter_ctx])
            == 1;

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
            ref_names[0] = read_single_ref_name(&mut self.dec, &mut self.map_inter_cdfs, 0);
        }

        // Build the spatial MV candidate list (§7.10) from neighbours.
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

        // Per reference: read mode + MV (§5.11.23). MV precision is
        // `allow_high_precision_mv` (1/8 vs 1/4 pel); `force_integer_mv`
        // skips the fractional reads entirely.
        let mut mvs = [Mv::default(); 2];
        let force_integer_mv = self.force_integer_mv;
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

        // Per-block interpolation filter (read only when switchable).
        let filter = if frame_filter == INTERP_SWITCHABLE {
            self.dec.read_symbol(&mut self.mode_cdfs.interp_filter[0]) as u8
        } else {
            frame_filter
        };

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
            if let Some(s) = self.tx_left.get_mut(r) {
                *s = luma_tx_h_byte;
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
            if let Some(s) = self.tx_above.get_mut(c) {
                *s = luma_tx_w_byte;
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

        if skip || luma_tx > TX_16X16 {
            return Ok(());
        }

        // Y residual.
        for ty in (0..bh * MI_SIZE).step_by(luma_tx_h) {
            for tx in (0..bw * MI_SIZE).step_by(luma_tx_w) {
                let px_x = mi_col * MI_SIZE + tx - self.tile_px_x0;
                let px_y = mi_row * MI_SIZE + ty - self.tile_px_y0;
                let mut residual = vec![0i32; luma_tx_w * luma_tx_h];
                if !skip {
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
                    if !skip {
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
        Ok(())
    }
}
