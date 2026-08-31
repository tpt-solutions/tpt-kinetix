use super::*;

impl<'a> TileDecodeState<'a> {
    pub(super) fn decode_intra_block(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
    ) -> Result<(), KinetixError> {
        let _bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let _bh = BLOCK_HEIGHT[bsize] / MI_SIZE;

        crate::entropy::mark_block(|| {
            format!(
                "mode_info mi=({mi_col},{mi_row}) bsize={bsize} px=({},{})",
                mi_col * MI_SIZE,
                mi_row * MI_SIZE
            )
        });

        // AV1 spec §5.11.7 `intra_frame_mode_info()` reads, in this exact
        // order: segment_id, skip, [cdef/delta_q/delta_lf], then the intra
        // mode/filter-intra reads, then (from the enclosing `decode_block()`,
        // §5.11.5) `read_block_tx_size()` last. The previous code read
        // y_mode/uv_mode *before* segment_id/skip — since every read shares
        // one arithmetic-coded bitstream position, that order mismatch
        // desynced literally every intra block in every frame from the very
        // first symbol read (segment_id/skip's bits were consumed as if they
        // were y_mode's). Fixed 2026-08-16.

        // Segment id (AV1 spec §5.11.8 / §5.11.9). When no segmentation is
        // active every block is segment 0 and no symbol is read. The per-segment
        // feature override of qindex / skip is not yet applied here (the test
        // corpus uses no segmentation), so qindex and the skip flag below fall
        // back to the frame-level values.
        let seg_ctx = self.segment_id_context(mi_row, mi_col);
        let _seg_id = if self.segmentation_enabled {
            self.mode_cdfs.read_segment_id(&mut self.dec, seg_ctx)
        } else {
            0
        };

        // Skip flag (AV1 spec §5.11.11). When segmentation enables the
        // SEG_LVL_SKIP feature for this block, skip is forced on.
        let above_skip = self.skip_above[mi_col] as usize;
        let left_skip = self.skip_left[mi_row] as usize;
        let skip = if self.seg_feature_skip {
            true
        } else {
            self.mode_cdfs
                .read_skip(&mut self.dec, (above_skip + left_skip).min(2))
                == 1
        };

        // AV1 spec §5.11.7: `read_cdef()`/`read_delta_qindex()`/
        // `read_delta_lf()` come right after `read_skip()`, then
        // `ReadDeltas = 0` (only the first coded block of each superblock
        // can consume a delta, regardless of whether it actually did). Each
        // of the three is a true no-op (reads zero bits) unless the
        // corresponding frame-header feature is actually on.
        self.read_cdef(mi_row, mi_col, bsize, skip);
        self.read_delta_qindex(bsize, skip);
        self.read_delta_lf(bsize, skip);
        self.read_deltas = false;

        // AV1 spec §5.11.7: when allow_intrabc, read use_intrabc = f(1) before
        // y_mode. Not reading this bit desyncs y_mode and every subsequent read
        // by 1 bit for every block in the tile — a progressive, accumulating
        // corruption.
        if self.allow_intrabc {
            let use_intrabc = self.dec.read_literal(1);
            if use_intrabc == 1 {
                // IBC block: full decode (MV read + block-copy reconstruction)
                // is not yet implemented. Return an error so the frame is
                // flagged not-pixel-exact rather than producing garbage from
                // a desynced bitstream.
                return Err(KinetixError::Parse(
                    "intra block copy (use_intrabc=1) not yet implemented".into(),
                ));
            }
        }

        // Intra luma mode (keyframe path, AV1 spec §5.11.9 / §8.3.2
        // `intra_frame_y_mode`): `TileIntraFrameYModeCdf[abovemode][leftmode]`,
        // each index used *directly* as its own axis of the 2-D context — not
        // summed and re-split (`(above+left).min(4)`, `((above+left)/5).min(4)`,
        // the previous, incorrect implementation here). That reshuffling
        // produced the wrong context for almost every above/left combination
        // (e.g. above=4,left=0 gave ctx (4,0) via the sum path yielding
        // `y_ctx=4`→`(4,0)`, which only accidentally matches; above=2,left=3
        // gives `y_ctx=5`→`(4,1)` instead of the correct `(2,3)`), desyncing
        // the entropy decoder on the very first symbol read for most blocks.
        let above_mode = self.ymode_above[mi_col] as usize;
        let left_mode = self.ymode_left[mi_row] as usize;
        let y_mode = self.mode_cdfs.read_intra_y_mode(
            &mut self.dec,
            INTRA_MODE_CONTEXT[above_mode],
            INTRA_MODE_CONTEXT[left_mode],
        );
        if std::env::var("KINETIX_AV1_DBG_YMODE").is_ok() {
            eprintln!(
                "DBG ymode mi=({mi_col},{mi_row}) bsize={bsize} above_mode={above_mode} left_mode={left_mode} ctx=({},{}) -> y_mode={y_mode} bit={}",
                INTRA_MODE_CONTEXT[above_mode],
                INTRA_MODE_CONTEXT[left_mode],
                self.dec.bit_position()
            );
        }

        // `intra_angle_info_y()` (AV1 spec §5.11.42): `angle_delta_y` is read
        // right after `intra_frame_y_mode`, before `uv_mode`, whenever
        // `MiSize >= BLOCK_8X8 && is_directional_mode(YMode)`. Previously
        // missing entirely — since this shares the same bitstream position
        // as every later symbol in the block, every directional-Y-mode block
        // at least 8x8 desynced from this point onward whenever the encoder
        // wrote a non-zero-cost angle delta (which real encoders do often;
        // directional modes are a common choice on non-flat content).
        let angle_delta_y = if bsize >= BLOCK_8X8 && is_directional_mode(y_mode as u8) {
            self.mode_cdfs.read_angle_delta(&mut self.dec, y_mode)
        } else {
            0
        };

        // Chroma mode (AV1 spec §5.11.10). `HasChroma` (§5.11.5): this leaf's
        // own chroma syntax is only present when it isn't the first (luma-only)
        // half of a sub-4-sample chroma-sharing pair — see `has_chroma`.
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
        // `intra_angle_info_uv()`, per the `intra_frame_mode_info()` syntax
        // order.
        let cfl_alpha = if has_chroma && uv_mode == UV_CFL_PRED {
            Some(self.mode_cdfs.read_cfl_alphas(&mut self.dec))
        } else {
            None
        };

        // `intra_angle_info_uv()` (AV1 spec §5.11.43): same shape as the luma
        // angle delta above, gated on `UVMode` instead of `YMode`.
        let angle_delta_uv =
            if has_chroma && bsize >= BLOCK_8X8 && is_directional_mode(uv_mode as u8) {
                self.mode_cdfs.read_angle_delta(&mut self.dec, uv_mode)
            } else {
                0
            };

        // `palette_mode_info()` (AV1 spec §5.11.46), read right after the
        // angle deltas and before `filter_intra_mode_info()`, per
        // `intra_frame_mode_info()`'s syntax order.
        let (colors_y, colors_u, colors_v) =
            self.read_palette_mode_info(mi_row, mi_col, bsize, y_mode, uv_mode, has_chroma);

        // `filter_intra_mode_info()` (AV1 spec §5.11.24). Reads a symbol only
        // when `enable_filter_intra && y_mode == DC_PRED && PaletteSizeY == 0
        // && max(w,h) <= 32`. The decoded mode is wired into the luma
        // prediction via `predict_filter_intra` (AV1 spec §7.11.2.3) below.
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

        // `palette_tokens()` (AV1 spec §5.11.49), read right after
        // `mode_info()` (i.e. after `filter_intra_mode_info()`) and before
        // `read_block_tx_size()`, per `decode_block()`'s syntax order.
        let (map_y, stride_y) = self.read_color_map(bsize, mi_row, mi_col, colors_y.len(), false);
        let (map_uv, stride_uv) = self.read_color_map(bsize, mi_row, mi_col, colors_u.len(), true);
        let palette = PaletteData {
            colors_y,
            colors_u,
            colors_v,
            map_y,
            stride_y,
            map_uv,
            stride_uv,
        };

        // Transform size (AV1 spec §5.11.15/17). `allowSelect = !skip ||
        // !is_inter` is always true for this (pure-intra keyframe) path, so
        // `skip` does not gate whether `tx_depth` is read — a skipped intra
        // block still signals its transform size (it just has no residual to
        // apply it to). Previously gating this on `!skip` silently desynced
        // every skipped keyframe block whenever `TxMode == TX_MODE_SELECT`.
        let max_tx = max_tx_size_for_bsize(bsize);
        let luma_tx = if self.tx_mode_select && !self.lossless {
            self.read_tx_size(bsize, max_tx, mi_row, mi_col)
        } else {
            max_tx
        };

        // Opt-in per-block symbol trace, in the same shape as a `DAV1D_TRACE`
        // dav1d debug build (`BLOCK` line + per-coeff `KTRACE CF` lines in
        // `reconstruct_block.rs`), for diffing the entropy decode against the
        // reference decoder block-by-block.
        if std::env::var("KINETIX_AV1_TRACE").is_ok() {
            let hc = has_chroma;
            eprintln!(
                "KTRACE BLOCK bx={mi_col} by={mi_row} bw4={} bh4={} skip={} ymode={y_mode}{} tx={luma_tx} r={}",
                BLOCK_WIDTH[bsize] / MI_SIZE,
                BLOCK_HEIGHT[bsize] / MI_SIZE,
                skip as u8,
                if hc { format!(" uvmode={uv_mode}") } else { String::new() },
                self.dec.raw_state().0,
            );
        }

        if mi_row < 4 && std::env::var("KINETIX_AV1_DBG").is_ok() {
            eprintln!(
                "DBG decode_intra_block mi=({mi_row},{mi_col}) bsize={bsize} y_mode={y_mode} uv_mode={uv_mode} skip={skip} luma_tx={luma_tx} filter_intra={filter_intra_mode:?} colors_y={:?} colors_u={:?}",
                palette.colors_y, palette.colors_u
            );
        }
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
        )
    }

    /// Reconstruct an intra-coded block's luma + chroma transform blocks once the
    /// luma/chroma prediction modes, skip flag, and (already-decoded) luma
    /// transform size are known. Shared by the intra-frame path
    /// ([`Self::decode_intra_block`]) and the intra-in-inter path (AV1 Phase E)
    /// so both feed the same inverse-transform + intra-prediction machinery.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn reconstruct_intra_subblock(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
        y_mode: usize,
        uv_mode: usize,
        skip: bool,
        luma_tx: usize,
        filter_intra_mode: Option<usize>,
        cfl_alpha: Option<(i32, i32)>,
        angle_delta_y: i32,
        angle_delta_uv: i32,
        palette: &PaletteData,
    ) -> Result<(), KinetixError> {
        let bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh = BLOCK_HEIGHT[bsize] / MI_SIZE;
        // Reconstruct luma transform blocks.
        let luma_tx_w = av1::TX_WIDTH[luma_tx];
        let luma_tx_h = av1::TX_HEIGHT[luma_tx];

        // `intra_tx_type`'s CDF-selection `intraDir` (AV1 spec, "Parsing
        // process" / intra_tx_type derivation): when `use_filter_intra` is
        // set, `intraDir = Filter_Intra_Mode_To_Intra_Dir[filter_intra_mode]`
        // (`{DC_PRED, V_PRED, H_PRED, D157_PRED, DC_PRED}`), *not* `YMode`
        // directly. `filter_intra_mode` and `y_mode` are mutually exclusive
        // in practice (`filter_intra_mode_info()` is only read when
        // `YMode == DC_PRED`), but the *CDF context* still differs — e.g.
        // `filter_intra_mode == FILTER_H_PRED` (2) must select the
        // `H_PRED`-indexed transform-type CDF bucket, not the `DC_PRED`
        // bucket `y_mode` (always 0 here) would give. Using `y_mode`
        // unconditionally picked the wrong `intra_tx_type` CDF context for
        // every filter-intra block — a common real-encoder choice on
        // low-detail/gradient content (this crate's whole `testsrc`/
        // `mandelbrot` corpus decodes filter-intra for most of its
        // top-left blocks) — the same "plausible but wrong decoded
        // symbol, no desync" corruption signature as the
        // `INTRA_MODE_CONTEXT` bug two sessions ago.
        const FILTER_INTRA_MODE_TO_INTRA_DIR: [usize; 5] = [
            DC_PRED as usize,
            V_PRED as usize,
            H_PRED as usize,
            D157_PRED as usize,
            DC_PRED as usize,
        ];
        let luma_intra_dir = match filter_intra_mode {
            Some(m) => FILTER_INTRA_MODE_TO_INTRA_DIR[m],
            None => y_mode,
        };

        // Computed before the `&mut self.{y,u,v}_plane` reborrows below —
        // `qindex_for_plane` takes `&self`, which conflicts with those live
        // disjoint-field mutable borrows if called any later.
        let (y_qindex_dc, y_qindex_ac) = self.qindex_for_plane(0);
        let (u_qindex_dc, u_qindex_ac) = self.qindex_for_plane(1);
        let (v_qindex_dc, v_qindex_ac) = self.qindex_for_plane(2);

        let y_plane = &mut *self.y_plane;
        let u_plane = &mut *self.u_plane;
        let v_plane = &mut *self.v_plane;

        // Luma transform blocks (every square and rectangular `TxSize`; the
        // inverse-transform set covers all 19 AV1 spec `TxSize` values).
        for ty in (0..bh * MI_SIZE).step_by(luma_tx_h) {
            for tx in (0..bw * MI_SIZE).step_by(luma_tx_w) {
                let px_x = mi_col * MI_SIZE + tx - self.tile_px_x0;
                let px_y = mi_row * MI_SIZE + ty - self.tile_px_y0;
                let blk = TxBlockCtx {
                    plane: 0,
                    tx_size: luma_tx,
                    x4: px_x / 4,
                    y4: px_y / 4,
                    max_x4: self.luma_max_x4,
                    max_y4: self.luma_max_y4,
                    // `Block_Width[get_plane_residual_size(MiSize, 0)]` (spec
                    // §8.3.2 `all_zero`'s `bw`/`bh`) — the *coded block's*
                    // plane-residual size, not this transform block's own
                    // `Tx_Width`/`Tx_Height`. For plane 0 the residual size is
                    // `MiSize` itself (no subsampling), i.e. `bw`/`bh` in
                    // samples. A previous revision passed `luma_tx_w`/`_h`
                    // here, making `blk.block_w == w && blk.block_h == h`
                    // (the whole-block `ctx = 0` special case) unconditionally
                    // true for every transform block — including every block
                    // whose `tx_size` splits a larger coded block, which then
                    // read `all_zero` from the wrong CDF bucket and decoded
                    // the wrong boolean whenever the true (neighbour-derived)
                    // context wasn't already 0. This desynced every
                    // multi-transform-block coded block's residual, and
                    // everything after it in decode order — while a
                    // single-block-single-transform frame (`solid_red`) never
                    // exercised the buggy branch at all, since there `bw == w`
                    // was actually true.
                    block_w: bw * MI_SIZE,
                    block_h: bh * MI_SIZE,
                    intra_dir: luma_intra_dir,
                    uv_mode,
                    qindex_positive: !self.lossless,
                    reduced_tx_set: self.reduced_tx_set,
                    lossless: self.lossless,
                };
                let palette_y = (!palette.colors_y.is_empty()).then(|| PaletteBlockInfo {
                    colors: &palette.colors_y,
                    color_map: &palette.map_y,
                    map_stride: palette.stride_y,
                    off_x: tx,
                    off_y: ty,
                });
                reconstruct_tx_block(
                    &mut self.dec,
                    &mut self.coeff_cdfs,
                    &mut self.coeff_ctxs,
                    &blk,
                    y_plane,
                    self.y_stride,
                    self.tile_w,
                    self.tile_h,
                    px_x,
                    px_y,
                    luma_tx,
                    y_qindex_dc,
                    y_qindex_ac,
                    y_mode,
                    skip,
                    filter_intra_mode,
                    self.enable_intra_edge_filter,
                    None,
                    angle_delta_y,
                    palette_y,
                )?;
            }
        }

        // Record per-8×8-luma-block metadata for the in-loop filters (AV1 Phase
        // D). Coordinates are tile-local, matching the tile-local plane buffers
        // this tile reconstructs into; `reconstruct_av1_frame` merges the
        // per-tile metas into a full-frame `FrameMeta` and runs
        // `apply_post_filters` over the assembled frame.
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

        // Reconstruct chroma transform blocks (4:2:0 / 4:2:2 / 4:4:4).
        // `HasChroma` (AV1 spec §5.11.5, see [`has_chroma`]): a block that is
        // the first (even row/col) half of a sub-4-sample chroma-sharing
        // pair carries no chroma syntax/residual at all — that shared data
        // was already reconstructed on (or waits for) the pair's second
        // block. Skipping this whole section for such a block, rather than
        // reconstructing a redundant, wrongly-positioned partial chroma
        // block per luma sub-block, is the fix for the "not modelled"
        // simplification noted elsewhere in this module.
        if !self.monochrome
            && has_chroma(
                bsize,
                mi_row,
                mi_col,
                self.subsampling_x,
                self.subsampling_y,
            )
        {
            let sub_x = self.subsampling_x as usize;
            let sub_y = self.subsampling_y as usize;
            // `MaxLumaW`/`MaxLumaH` (AV1 spec §7.11.2.1, set when `plane ==
            // 0`): the pixel extent of the coded block's just-reconstructed
            // luma region, used by CFL (§7.11.5) to clamp its luma-sample
            // lookups at the block's own right/bottom edge rather than the
            // frame's.
            let max_luma_w = blk_px_x + bw * MI_SIZE;
            let max_luma_h = blk_px_y + bh * MI_SIZE;
            // AV1 spec §5.11.37 `get_tx_size(plane, txSz)`: the chroma
            // transform size is derived from the *whole coded block's* size
            // (`bsize`), not from the luma transform size directly, via
            // `Max_Tx_Size_Rect[get_plane_residual_size(MiSize, plane)]` plus
            // the 64-sample clamp. A previous revision instead bucketed a
            // per-luma-tx-block `cw`/`ch` (derived from `luma_tx_w`/`_h`) into
            // the nearest *square* candidate — wrong for any bsize whose
            // subsampled residual size is itself rectangular (e.g. every
            // non-square bsize under 4:2:0), and recomputed uselessly once
            // per luma tx sub-block instead of once per coded block.
            let c_tx = chroma_tx_size(
                bsize,
                usize::from(self.subsampling_x),
                usize::from(self.subsampling_y),
            );
            let cw = av1::TX_WIDTH[c_tx];
            let ch = av1::TX_HEIGHT[c_tx];
            // Chroma transform blocks tile the coded block's *chroma-space*
            // residual extent directly (spec §5.11.34 `residual()`'s
            // `transform_block` loop, stepping by the single chroma `txSz`
            // it derived for the whole block) — not the per-luma-tx-block
            // grid `luma_tx_w`/`_h` step used above, which only coincides
            // with the chroma step when `cw`/`ch` happen to equal the
            // subsampled luma tx step (the previous revision's bucketed
            // square `c_tx` always did; a real rectangular `c_tx` may not).
            // AV1 spec §5.11.34 `residual()`: `baseXBlock = (MiCol >> subX) *
            // MI_SIZE` — the mi position is floor-divided by the subsampling
            // *before* multiplying back up to samples, not the other way
            // round. For an odd `mi_col`/`mi_row` (always the case for the
            // chroma-carrying half of a `HasChroma`-shared pair, since the
            // *other* half sits at the preceding even position) those two
            // orders disagree — e.g. `mi_col == 1`, `sub_x == 1`:
            // `(1 >> 1) * 4 == 0` vs the previous `(1 * 4) >> 1 == 2` — and
            // only the spec's order lands both halves of the pair on the
            // same shared chroma origin.
            let base_cpx_x = (mi_col >> sub_x) * MI_SIZE - (self.tile_px_x0 >> sub_x);
            let base_cpx_y = (mi_row >> sub_y) * MI_SIZE - (self.tile_px_y0 >> sub_y);
            // `num4x4W * 4` / `num4x4H * 4` (spec `residual()`): the chroma
            // extent is this block's own `get_plane_residual_size(MiSize,
            // plane)`, which already accounts for the sub-4x4 floor (e.g.
            // `Subsampled_Size[BLOCK_4X4][1][1] == BLOCK_4X4`) — no separate
            // "shared group size" is needed, since only the `HasChroma`
            // block of a pair reaches this code at all.
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
            for ty in (0..chroma_bh).step_by(ch) {
                for tx in (0..chroma_bw).step_by(cw) {
                    let cpx_x = base_cpx_x + tx;
                    let cpx_y = base_cpx_y + ty;
                    if cpx_x >= self.tile_cw || cpx_y >= self.tile_ch {
                        continue;
                    }
                    let blk_u = TxBlockCtx {
                        plane: 1,
                        tx_size: c_tx,
                        x4: cpx_x / 4,
                        y4: cpx_y / 4,
                        max_x4: self.uv_max_x4,
                        max_y4: self.uv_max_y4,
                        // Same fix as the luma case above: the coded block's
                        // chroma plane-residual size (`chroma_bw`/`chroma_bh`,
                        // already `Block_Width`/`Height[get_plane_residual_
                        // size(MiSize, 1)]`), not this transform block's own
                        // `cw`/`ch`.
                        block_w: chroma_bw,
                        block_h: chroma_bh,
                        intra_dir: uv_mode,
                        uv_mode,
                        qindex_positive: !self.lossless,
                        reduced_tx_set: self.reduced_tx_set,
                        lossless: self.lossless,
                    };
                    let blk_v = TxBlockCtx { plane: 2, ..blk_u };
                    let cfl_u = cfl_alpha.map(|(au, _)| CflParams {
                        luma: &*y_plane,
                        luma_stride: self.y_stride,
                        sub_x: self.subsampling_x,
                        sub_y: self.subsampling_y,
                        max_luma_w,
                        max_luma_h,
                        alpha: au,
                    });
                    let cfl_v = cfl_alpha.map(|(_, av)| CflParams {
                        luma: &*y_plane,
                        luma_stride: self.y_stride,
                        sub_x: self.subsampling_x,
                        sub_y: self.subsampling_y,
                        max_luma_w,
                        max_luma_h,
                        alpha: av,
                    });
                    let palette_u = (!palette.colors_u.is_empty()).then(|| PaletteBlockInfo {
                        colors: &palette.colors_u,
                        color_map: &palette.map_uv,
                        map_stride: palette.stride_uv,
                        off_x: tx,
                        off_y: ty,
                    });
                    let palette_v = (!palette.colors_v.is_empty()).then(|| PaletteBlockInfo {
                        colors: &palette.colors_v,
                        color_map: &palette.map_uv,
                        map_stride: palette.stride_uv,
                        off_x: tx,
                        off_y: ty,
                    });
                    reconstruct_tx_block(
                        &mut self.dec,
                        &mut self.coeff_cdfs,
                        &mut self.coeff_ctxs,
                        &blk_u,
                        u_plane,
                        self.uv_stride,
                        self.tile_cw,
                        self.tile_ch,
                        cpx_x,
                        cpx_y,
                        c_tx,
                        u_qindex_dc,
                        u_qindex_ac,
                        uv_mode,
                        skip,
                        None,
                        self.enable_intra_edge_filter,
                        cfl_u,
                        angle_delta_uv,
                        palette_u,
                    )?;
                    reconstruct_tx_block(
                        &mut self.dec,
                        &mut self.coeff_cdfs,
                        &mut self.coeff_ctxs,
                        &blk_v,
                        v_plane,
                        self.uv_stride,
                        self.tile_cw,
                        self.tile_ch,
                        cpx_x,
                        cpx_y,
                        c_tx,
                        v_qindex_dc,
                        v_qindex_ac,
                        uv_mode,
                        skip,
                        None,
                        self.enable_intra_edge_filter,
                        cfl_v,
                        angle_delta_uv,
                        palette_v,
                    )?;
                }
            }
            // Record chroma tx/skip metadata for the same 8×8-luma grid region.
            let c_tx_w = av1::TX_WIDTH[c_tx] as u8;
            let c_tx_h = av1::TX_HEIGHT[c_tx] as u8;
            for by in by0..by1.min(self.meta.h8) {
                for bx in bx0..bx1.min(self.meta.w8) {
                    self.meta.record_chroma(bx, by, c_tx_w, c_tx_h, skip);
                }
            }
        }

        // Update neighbour contexts for this block.
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(slot) = self.ymode_left.get_mut(r) {
                *slot = y_mode as u8;
            }
        }
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(slot) = self.ymode_above.get_mut(c) {
                *slot = y_mode as u8;
            }
        }
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(slot) = self.uv_left.get_mut(r) {
                *slot = uv_mode as u8;
            }
        }
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(slot) = self.uv_above.get_mut(c) {
                *slot = uv_mode as u8;
            }
        }
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(slot) = self.tx_left.get_mut(r) {
                *slot = av1::TX_HEIGHT[luma_tx] as u8;
            }
        }
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(slot) = self.tx_above.get_mut(c) {
                *slot = av1::TX_WIDTH[luma_tx] as u8;
            }
        }
        let skip_byte = skip as u8;
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(slot) = self.skip_left.get_mut(r) {
                *slot = skip_byte;
            }
        }
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(slot) = self.skip_above.get_mut(c) {
                *slot = skip_byte;
            }
        }
        // `PaletteColors[{0,1}][MiRow][MiCol]` (AV1 spec §5.11.46's implicit
        // per-position storage that [`Self::get_palette_cache`] reads back):
        // record this block's Y/U palettes (or clear them, for a non-palette
        // block) across its whole mi extent, mirroring the neighbour-context
        // update pattern above.
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(slot) = self.palette_y_colors_left.get_mut(r) {
                *slot = palette.colors_y.clone();
            }
            if let Some(slot) = self.palette_u_colors_left.get_mut(r) {
                *slot = palette.colors_u.clone();
            }
        }
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(slot) = self.palette_y_colors_above.get_mut(c) {
                *slot = palette.colors_y.clone();
            }
            if let Some(slot) = self.palette_u_colors_above.get_mut(c) {
                *slot = palette.colors_u.clone();
            }
        }
        Ok(())
    }
}
