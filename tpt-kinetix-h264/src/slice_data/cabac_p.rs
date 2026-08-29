use super::*;
#[allow(clippy::too_many_arguments)]
pub(crate) fn parse_intra_macroblock_cabac<T: crate::trace::DecodeTracer>(
    dec: &mut crate::entropy::CabacDecoder,
    ctxs: &mut CabacSliceContexts,
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nz_grid: &[MbNz],
    pred_ctx_grid: &[MbPredCtx],
    cabac_ctx_grid: &[MbCabacCtx],
    prev_qp: i32,
    prev_dqp_nonzero: bool,
    transform_8x8_mode_flag: bool,
    tracer: &mut T,
    nctx: NeighbourCtx,
) -> R<(Macroblock, MbNz, MbPredCtx, MbCabacCtx, i32, bool)> {
    use crate::cabac_tables::{
        CAT_CHROMA_AC, CAT_CHROMA_DC, CAT_LUMA_4X4, CAT_LUMA_AC, CAT_LUMA_DC,
    };

    let mut mb = Macroblock::new_skip();
    mb.skip = false;
    let mut this_nz = MbNz {
        present: true,
        ..Default::default()
    };
    let mut this_pred_ctx = MbPredCtx {
        present: true,
        ..Default::default()
    };
    let mut this_cabac_ctx = MbCabacCtx {
        present: true,
        ..Default::default()
    };

    let (left_idx, top_idx) = nctx.left_top(mb_x, mb_y, mb_cols);

    // mb_type.
    let mb_type_neighbors = crate::entropy::MbTypeNeighbors {
        left_is_16x16_or_pcm: left_idx
            .map(|i| cabac_ctx_grid[i].is_intra16x16_or_pcm)
            .unwrap_or(false),
        top_is_16x16_or_pcm: top_idx
            .map(|i| cabac_ctx_grid[i].is_intra16x16_or_pcm)
            .unwrap_or(false),
    };
    let mb_type = ctxs.mb_type.decode(dec, &mb_type_neighbors);
    if mb_type == 25 {
        return Err(SliceDataError::Unsupported(
            "I_PCM under CABAC not supported",
        ));
    }

    let (is_i16x16, i16_mode, cbp_chroma_mbtype, cbp_luma_mbtype) = if mb_type == 0 {
        (false, 0u8, 0u8, 0u8)
    } else if (1..=24).contains(&mb_type) {
        let (m, cc, cl) = I16X16_TABLE[(mb_type - 1) as usize];
        (true, m, cc, cl)
    } else {
        return Err(SliceDataError::Unsupported("mb_type out of range"));
    };

    let mut is_8x8 = false;
    if is_i16x16 {
        mb.mb_type = MbType::Intra16x16 {
            pred_mode: i16_mode,
            cbp_chroma: cbp_chroma_mbtype,
            cbp_luma: cbp_luma_mbtype,
        };
        mb.cbp = cbp_luma_mbtype | (cbp_chroma_mbtype << 4);
        this_cabac_ctx.is_intra16x16_or_pcm = true;
    } else {
        mb.mb_type = MbType::Intra4x4;
        // `transform_size_8x8_flag` (§9.3.3.1.1.10, ctxIdxOffset 399) is read
        // right after the mb_type decision, before the prediction modes
        // (§7.3.5.1), mirroring the CAVLC path's `is_8x8` gate
        // (`slice_data.rs` `parse_intra_macroblock`).
        is_8x8 = if transform_8x8_mode_flag {
            let left_8x8 = left_idx
                .map(|i| cabac_ctx_grid[i].transform_8x8)
                .unwrap_or(false);
            let top_8x8 = top_idx
                .map(|i| cabac_ctx_grid[i].transform_8x8)
                .unwrap_or(false);
            ctxs.transform_8x8.decode(dec, left_8x8, top_8x8)
        } else {
            false
        };
        mb.transform_size_8x8 = is_8x8;
        this_cabac_ctx.transform_8x8 = is_8x8;

        // prev_intra4x4_pred_mode_flag / rem_intra4x4_pred_mode ×16 (or, for
        // Intra_8x8, ×4), in the same Z-scan block order and MPM derivation
        // as the CAVLC path (see `mpm_pred_mode`) — only the bit-level decode
        // of the flag/rem differs between entropy modes. `Intra4x4PredModeCabacContext`
        // is spec-shared between 4x4 and 8x8 blocks (ctxIdx 68/69).
        let mut modes = [Intra4x4Mode::Dc; 16];
        if is_8x8 {
            let mut modes8 = [0u8; 4];
            for i8 in 0..4usize {
                let pred_mode =
                    mpm_pred_mode_8x8(pred_ctx_grid, mb_x, mb_y, mb_cols, &modes, i8, nctx);
                let final_mode = ctxs.intra4x4.decode(dec, pred_mode);
                modes8[i8] = final_mode;
                for sub in 0..4usize {
                    modes[raster_of_8x8_sub(i8, sub)] = Intra4x4Mode::from_u8(final_mode);
                }
            }
            mb.pred_modes_8x8 = Box::new(modes8);
        } else {
            for blk_idx in 0..16usize {
                let raster = raster_of_8x8_sub(blk_idx / 4, blk_idx % 4);
                let pred_mode =
                    mpm_pred_mode(pred_ctx_grid, mb_x, mb_y, mb_cols, &modes, raster, nctx);
                let final_mode = ctxs.intra4x4.decode(dec, pred_mode);
                modes[raster] = Intra4x4Mode::from_u8(final_mode);
            }
        }
        mb.pred_modes_4x4 = Box::new(modes);
        this_pred_ctx.is_intra4x4 = true;
        this_pred_ctx.modes = modes;
    }

    // intra_chroma_pred_mode.
    let left_chroma_nonzero = left_idx
        .map(|i| cabac_ctx_grid[i].chroma_pred_mode != 0)
        .unwrap_or(false);
    let top_chroma_nonzero = top_idx
        .map(|i| cabac_ctx_grid[i].chroma_pred_mode != 0)
        .unwrap_or(false);
    let chroma_pred = ctxs
        .chroma_pred
        .decode(dec, left_chroma_nonzero, top_chroma_nonzero);
    mb.intra_chroma_pred_mode = chroma_pred as u8;
    this_cabac_ctx.chroma_pred_mode = chroma_pred as u8;

    // coded_block_pattern (I_16×16 carries it in mb_type; I_NxN decodes it).
    let (cbp_l, cbp_c) = if is_i16x16 {
        (cbp_luma_mbtype, cbp_chroma_mbtype)
    } else {
        let (left_cbp, top_cbp) = cabac_cbp_neighbors(cabac_ctx_grid, mb_x, mb_y, mb_cols, nctx);
        let (l, c) = ctxs.cbp.decode(dec, left_cbp, top_cbp);
        mb.cbp = l | (c << 4);
        (l, c)
    };

    // mb_qp_delta, present when CBP != 0 or I_16×16.
    let mut qp = prev_qp;
    let mut dqp_nonzero = false;
    if cbp_l != 0 || cbp_c != 0 || is_i16x16 {
        let dqp = ctxs.qp_delta.decode(dec, prev_dqp_nonzero);
        dqp_nonzero = dqp != 0;
        qp = (prev_qp + dqp + 52).rem_euclid(52);
    }
    mb.qp = qp;

    // ---- Residual parsing ----
    let mut cbp_word: u16 = mb.cbp as u16;

    if is_i16x16 {
        let left_coded = dc_cbf_neighbor(cabac_ctx_grid, left_idx, 0x100);
        let top_coded = dc_cbf_neighbor(cabac_ctx_grid, top_idx, 0x100);
        if ctxs.cbf.decode(dec, CAT_LUMA_DC, left_coded, top_coded) {
            let (coeffs, _count) =
                ctxs.residual
                    .decode_block(dec, CAT_LUMA_DC, 16, nctx.is_field());
            mb.luma_dc = coeffs;
            cbp_word |= 0x100;
        }
    }

    if is_8x8 {
        // Luma8x8 (`ctxBlockCat == 5`): no separate `coded_block_flag` is
        // signalled (non-4:4:4, §9.3.3.1.1.9/FFmpeg's `decode_cabac_residual_nondc`)
        // -- the CBP luma bit alone gates presence. All four 4x4-raster
        // positions covered by one 8x8 block share the same significant-
        // coefficient count for neighbour cbf/nnz purposes (matches FFmpeg's
        // `fill_rectangle(nnz_cache, ..., coeff_count, 1)`).
        for blk8 in 0..4usize {
            if (cbp_l >> blk8) & 1 == 0 {
                continue;
            }
            let (coeffs_scan, count) = ctxs.residual.decode_block_8x8(dec, nctx.is_field());
            // decode_block_8x8 returns coefficients in scan-position order,
            // but dequant_idct_8x8 expects them in zigzag order.
            // Convert using INVERSE_ZIGZAG_8X8 (scan_pos -> zigzag_pos).
            let mut coeffs_zz = [0i16; 64];
            for (scan_pos, &level) in coeffs_scan.iter().enumerate() {
                coeffs_zz[crate::transform::INVERSE_ZIGZAG_8X8[scan_pos]] = level;
            }
            mb.luma_coeffs_8x8[blk8] = coeffs_zz;
            for sub in 0..4usize {
                this_nz.luma[raster_of_8x8_sub(blk8, sub)] = count;
            }
        }
    } else {
        let luma_max = if is_i16x16 { 15usize } else { 16usize };
        let luma_cat = if is_i16x16 { CAT_LUMA_AC } else { CAT_LUMA_4X4 };
        let blocks: Vec<usize> = {
            let mut v = Vec::with_capacity(16);
            for blk8 in 0..4usize {
                if (cbp_l >> blk8) & 1 == 0 {
                    continue;
                }
                for sub in 0..4usize {
                    v.push(raster_of_8x8_sub(blk8, sub));
                }
            }
            v
        };
        for block in blocks {
            let (left_coded, top_coded) =
                luma_cbf_neighbors(nz_grid, mb_x, mb_y, mb_cols, &this_nz, block, true, nctx);
            let coded = ctxs.cbf.decode(dec, luma_cat, left_coded, top_coded);
            if coded {
                let (coeffs, count) =
                    ctxs.residual
                        .decode_block(dec, luma_cat, luma_max, nctx.is_field());
                this_nz.luma[block] = count;
                if is_i16x16 {
                    let mut shifted = [0i16; 16];
                    shifted[1..16].copy_from_slice(&coeffs[0..15]);
                    mb.luma_coeffs[block] = shifted;
                } else {
                    mb.luma_coeffs[block] = coeffs;
                }
            }
        }
    }

    if cbp_c != 0 {
        for comp in 0..2usize {
            let bit = 0x40u16 << comp;
            let left_coded = dc_cbf_neighbor(cabac_ctx_grid, left_idx, bit);
            let top_coded = dc_cbf_neighbor(cabac_ctx_grid, top_idx, bit);
            let dc_coded = ctxs.cbf.decode(dec, CAT_CHROMA_DC, left_coded, top_coded);
            if dc_coded {
                let (coeffs, _count) =
                    ctxs.residual
                        .decode_block(dec, CAT_CHROMA_DC, 4, nctx.is_field());
                let dc = [coeffs[0], coeffs[1], coeffs[2], coeffs[3]];
                if comp == 0 {
                    mb.chroma_dc_cb = dc;
                } else {
                    mb.chroma_dc_cr = dc;
                }
                cbp_word |= bit;
            }
        }
    }

    if cbp_c == 2 {
        for comp in 0..2usize {
            for block in 0..4usize {
                let (left_coded, top_coded) = chroma_cbf_neighbors(
                    nz_grid, mb_x, mb_y, mb_cols, &this_nz, comp, block, true, nctx,
                );
                let ac_coded = ctxs.cbf.decode(dec, CAT_CHROMA_AC, left_coded, top_coded);
                if ac_coded {
                    let (coeffs, count) =
                        ctxs.residual
                            .decode_block(dec, CAT_CHROMA_AC, 15, nctx.is_field());
                    this_nz.chroma[comp * 4 + block] = count;
                    let mut shifted = [0i16; 16];
                    shifted[1..16].copy_from_slice(&coeffs[0..15]);
                    if comp == 0 {
                        mb.chroma_cb_coeffs[block] = shifted;
                    } else {
                        mb.chroma_cr_coeffs[block] = shifted;
                    }
                }
            }
        }
    }

    this_cabac_ctx.cbp_word = cbp_word;

    // Emit MB-level trace after full parse (mirrors the CAVLC path).
    {
        let mb_type_str = match mb.mb_type {
            MbType::Intra4x4 => "Intra4x4".to_string(),
            MbType::Intra16x16 {
                pred_mode,
                cbp_chroma,
                cbp_luma,
            } => {
                format!("Intra16x16(pred={pred_mode},cbp_chroma={cbp_chroma},cbp_luma={cbp_luma})")
            }
            _ => "Other".to_string(),
        };
        let mut modes = [0u8; 16];
        for (i, m) in mb.pred_modes_4x4.iter().enumerate() {
            modes[i] = *m as u8;
        }
        tracer.on_mb_parsed(
            mb_x,
            mb_y,
            &mb_type_str,
            mb.qp,
            mb.cbp,
            mb.intra_chroma_pred_mode,
            &modes,
        );
    }

    Ok((mb, this_nz, this_pred_ctx, this_cabac_ctx, qp, dqp_nonzero))
}

/// Parse a CABAC-coded P-slice (§7.3.4, §9.3).
///
/// `mb_aff` / `field_pic_flag` enable MBAFF pair parsing (§7.4.4): when the
/// SPS sets `mb_adaptive_frame_field_flag` and the slice is a frame picture,
/// `mb_field_decoding_flag` is decoded once per macroblock pair and skip-flag
/// decisions are paired per FFmpeg's `ff_h264_decode_mb_cabac` (a skipped top
/// MB pre-reads the bottom MB's skip flag; the pair's field flag is decoded
/// when the bottom is coded).
#[allow(clippy::too_many_arguments)]
pub fn parse_p_slice_cabac<T: crate::trace::DecodeTracer>(
    data: &[u8],
    mb_cols: u32,
    mb_rows: u32,
    slice_qp: i32,
    mb_aff: bool,
    field_pic_flag: bool,
    cabac_init_idc: usize,
    num_ref_idx_l0_active: u32,
    chroma_qp_index_offset: i32,
    transform_8x8_mode_flag: bool,
    tracer: &mut T,
) -> R<ParsedSlice> {
    let mut dec = crate::entropy::CabacDecoder::new(data)
        .map_err(|_| SliceDataError::Eof("CABAC engine init"))?;
    let mut ctxs = PbCabacSliceContexts::new_p(slice_qp, cabac_init_idc);

    let total = (mb_cols * mb_rows) as usize;
    // Pre-allocated and assigned by frame-MB grid address (not decode order):
    // in an MBAFF frame the parse visits macroblocks in pair-scan order
    // (top, bottom of pair 0, then pair 1 …) so `mb_idx` ≠ grid address for
    // every bottom macroblock. Committing by grid address keeps neighbour
    // lookups and `predict_slice_mvs` correct (mirrors `cabac_i.rs`, #32e).
    let mut macroblocks: Vec<Macroblock> = (0..total).map(|_| Macroblock::new_skip()).collect();
    let mut nz: Vec<MbNz> = vec![MbNz::default(); total];
    let mut pred_ctx: Vec<MbPredCtx> = vec![MbPredCtx::default(); total];
    let mut cabac_ctx: Vec<MbCabacCtx> = vec![MbCabacCtx::default(); total];
    let mut inter_ctx: Vec<MbInterCabacCtx> = vec![MbInterCabacCtx::default(); total];
    let mut qp = slice_qp;
    let mut prev_dqp_nonzero = false;
    // MBAFF pair state (§7.4.4). `mbaff_frame` mirrors FFmpeg's FRAME_MBAFF:
    // SPS `mb_adaptive_frame_field_flag` set AND the slice is a frame picture.
    let mbaff_frame = mb_aff && !field_pic_flag;
    let mut cur_pair_field = false;
    let mut field_flags: Vec<Option<bool>> = vec![None; total];
    // FFmpeg's sl->prev_mb_skipped / sl->next_mb_skipped: when the top MB of a
    // pair is skipped, the bottom MB's skip flag was already read (and the
    // pair's field flag decoded if the bottom is coded).
    let mut prev_mb_skipped = false;
    let mut next_mb_skipped = false;

    for mb_idx in 0..total {
        // MBAFF pair-scan addressing (§6.4.2/§7.4.4): addresses 2k / 2k+1 are
        // the top / bottom macroblock of pair k, at frame-MB column
        // `k % mb_cols` and rows `2*(k / mb_cols)` / `+1`. Frame-only slices
        // use plain raster.
        let (mb_x, mb_y, grid_idx) = if mbaff_frame {
            let pair = mb_idx >> 1;
            let px = (pair % mb_cols as usize) as u32;
            let py = (pair / mb_cols as usize) as u32;
            let mb_row = 2 * py + (mb_idx & 1) as u32;
            (px, mb_row, (mb_row * mb_cols + px) as usize)
        } else {
            ((mb_idx as u32) % mb_cols, (mb_idx as u32) / mb_cols, mb_idx)
        };
        let left_idx = (mb_x > 0).then(|| grid_idx - 1);
        let top_idx = (mb_y > 0).then(|| grid_idx - mb_cols as usize);

        let skip_neighbors = crate::entropy::MbSkipNeighbors {
            left_available: mb_x > 0,
            left_skipped: left_idx.map(|i| macroblocks[i].skip).unwrap_or(false),
            top_available: mb_y > 0,
            top_skipped: top_idx.map(|i| macroblocks[i].skip).unwrap_or(false),
        };
        let (r0, o0) = dec.debug_state();
        // FFmpeg ff_h264_decode_mb_cabac skip handling (MBAFF pairing):
        //   - bottom MB of a pair whose top was skipped reuses the already-
        //     decoded next_mb_skipped instead of reading a bin;
        //   - a skipped TOP MB pre-reads the bottom MB's skip flag, and if the
        //     bottom is coded, the pair's mb_field_decoding_flag follows;
        //   - a coded TOP MB reads the pair's mb_field_decoding_flag.
        let top_of_pair = mbaff_frame && mb_idx % 2 == 0;
        let is_skip = if mbaff_frame && !top_of_pair && prev_mb_skipped {
            next_mb_skipped
        } else {
            ctxs.mb_skip.decode(&mut dec, &skip_neighbors)
        };
        let mut pair_field_pending = false;
        if mbaff_frame && top_of_pair {
            if is_skip {
                // Read the bottom MB's skip flag for (x, y+1): its left
                // neighbour is (x-1, y+1); its top is THIS MB (skip=true).
                let bot_left_skipped = if mb_x > 0 {
                    // Left MB of the BOTTOM MB (x-1, y+1): already decoded as
                    // part of the previous pair.
                    macroblocks
                        .get(((mb_y as usize) + 1) * mb_cols as usize + mb_x as usize - 1)
                        .map(|m| m.skip)
                        .unwrap_or(false)
                } else {
                    false
                };
                let bot_neighbors = crate::entropy::MbSkipNeighbors {
                    left_available: mb_x > 0,
                    left_skipped: bot_left_skipped,
                    top_available: true,
                    top_skipped: true,
                };
                next_mb_skipped = ctxs.mb_skip.decode(&mut dec, &bot_neighbors);
                if !next_mb_skipped {
                    pair_field_pending = true;
                }
            } else {
                pair_field_pending = true;
            }
        }
        if pair_field_pending {
            let left_field = if mb_x > 0 {
                cabac_ctx[(mb_y as usize) * mb_cols as usize + mb_x as usize - 1].mb_field_flag
            } else {
                false
            };
            let top_field = if mb_y > 0 {
                cabac_ctx[((mb_y as usize) - 1) * mb_cols as usize + mb_x as usize].mb_field_flag
            } else {
                false
            };
            cur_pair_field = ctxs.mb_field.decode(&mut dec, left_field, top_field);
            field_flags[grid_idx] = Some(cur_pair_field);
            // The pair's bottom macroblock sits one frame-MB row below.
            let bot_grid = grid_idx + mb_cols as usize;
            if bot_grid < total {
                field_flags[bot_grid] = Some(cur_pair_field);
            }
        }
        let (r1, o1) = dec.debug_state();
        if is_skip {
            if std::env::var("KINETIX_BINTRACE").is_ok() {
                eprintln!("MB{mb_idx} ({mb_x},{mb_y}) SKIP  cabac={r0:#06x}/{o0:#010x} -> {r1:#06x}/{o1:#010x}");
            }
            let mut mb = Macroblock::new_skip();
            mb.mb_type = MbType::PSkip;
            mb.qp = qp;
            mb.skip = true;
            mb.mb_field_flag = cur_pair_field;
            prev_mb_skipped = true;
            // §9.3.3.1.1.5: ctxIdxInc for the next MB's mb_qp_delta is 0 when
            // the previous MB is skipped (it carries no mb_qp_delta). Failing
            // to clear this desyncs the arithmetic engine on the first coded
            // MB after any run of skips.
            prev_dqp_nonzero = false;
            nz[grid_idx] = MbNz {
                present: true,
                ..Default::default()
            };
            pred_ctx[grid_idx] = MbPredCtx {
                present: true,
                ..Default::default()
            };
            cabac_ctx[grid_idx] = MbCabacCtx {
                present: true,
                cbp_word: 0,
                ..Default::default()
            };
            inter_ctx[grid_idx] = MbInterCabacCtx {
                present: true,
                ..Default::default()
            };
            macroblocks[grid_idx] = mb;
            // §7.3.4 slice_data(): end_of_slice_flag is decoded after EVERY
            // non-I_PCM macroblock — including skipped ones (it sits outside
            // macroblock_layer() in the slice_data() do/while loop, gated only
            // on `mb_type != I_PCM`). Skipping it here desyncs the arithmetic
            // engine by one terminate bin per skip MB.
            //
            // EXCEPTION (MBAFF frame): the flag is NOT coded after the TOP
            // macroblock of a pair (`CurrMbAddr % 2 == 0` ⇒ `moreDataFlag = 1`
            // unconditionally); it follows only the bottom macroblock.
            if !(mbaff_frame && mb_idx % 2 == 0) {
                let end_of_slice = dec.decode_terminate() == 1;
                let is_last = mb_idx + 1 == total;
                // The final MB's terminate bin may be absent (x264 writes
                // exactly total-1 terminate bins — one before each MB except
                // the first — and no bin after the last MB; ffmpeg tolerates
                // both). Only an early end_of_slice mid-slice indicates a
                // desync.
                if !is_last && end_of_slice {
                    return Err(SliceDataError::Unsupported(
                        "end_of_slice_flag mismatch (P-CABAC, skip MB)",
                    ));
                }
            }
            continue;
        }
        if std::env::var("KINETIX_BINTRACE").is_ok() {
            eprintln!("MB{mb_idx} ({mb_x},{mb_y}) CODED skip_flag: {r0:#06x}/{o0:#010x} -> {r1:#06x}/{o1:#010x}");
        }

        let nctx = NeighbourCtx::new(mbaff_frame, mb_rows, cur_pair_field, &field_flags);
        let (mb, this_nz, this_pred_ctx, this_cabac_ctx, this_inter_ctx, new_qp, dqp_nz) =
            parse_p_macroblock_cabac(
                &mut dec,
                &mut ctxs,
                mb_x,
                mb_y,
                mb_cols,
                &nz,
                &pred_ctx,
                &cabac_ctx,
                &inter_ctx,
                qp,
                prev_dqp_nonzero,
                num_ref_idx_l0_active,
                chroma_qp_index_offset,
                transform_8x8_mode_flag,
                nctx,
                tracer,
            )?;
        qp = new_qp;
        prev_dqp_nonzero = dqp_nz;
        prev_mb_skipped = false;
        nz[grid_idx] = this_nz;
        pred_ctx[grid_idx] = this_pred_ctx;
        let mut this_cabac_ctx = this_cabac_ctx;
        this_cabac_ctx.mb_field_flag = cur_pair_field;
        cabac_ctx[grid_idx] = this_cabac_ctx;
        inter_ctx[grid_idx] = this_inter_ctx;
        let mut mb = mb;
        mb.mb_field_flag = cur_pair_field;
        macroblocks[grid_idx] = mb;

        // §7.3.4: no end_of_slice_flag after the TOP macroblock of an MBAFF
        // frame pair (see the skip-MB path above for the spec reference).
        if !(mbaff_frame && mb_idx % 2 == 0) {
            let end_of_slice = dec.decode_terminate() == 1;
            let is_last = mb_idx + 1 == total;
            if !is_last && end_of_slice {
                return Err(SliceDataError::Unsupported(
                    "end_of_slice_flag mismatch (P-CABAC)",
                ));
            }
        }
    }

    let mut mv_store = MvStore::new(total);
    crate::mv::predict_slice_mvs_ex(&mut mv_store, mb_cols, 0, 0, &macroblocks, mbaff_frame)?;
    Ok(ParsedSlice {
        macroblocks,
        nz,
        mv_store,
    })
}
