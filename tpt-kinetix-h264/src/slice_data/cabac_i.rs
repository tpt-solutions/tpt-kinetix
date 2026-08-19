use super::*;

/// Parse the macroblock layer of a CABAC-coded I-slice (§7.3.4, §9.3).
///
/// `data` must already be byte-aligned to the start of `slice_data()`'s
/// CABAC payload (i.e. after consuming any `cabac_alignment_one_bit`s via
/// `BitReader::byte_align` + `BitReader::remaining_bytes`).
///
/// Scope: 4:2:0, frame-only (no MBAFF/field), no I_PCM (returns
/// `Unsupported` so callers fall back like any other unhandled slice — see
/// `todo.md` Phase D for the rationale). `transform_size_8x8_flag`/Intra_8x8
/// (High profile) is supported for intra macroblocks only (Phase F.4) --
/// inter (P_8x8/B_Direct) 8x8 transform is not, matching the CAVLC path's
/// existing scope.
pub fn parse_i_slice_cabac<T: crate::trace::DecodeTracer>(
    data: &[u8],
    mb_cols: u32,
    mb_rows: u32,
    slice_qp: i32,
    mb_aff: bool,
    field_pic_flag: bool,
    transform_8x8_mode_flag: bool,
    tracer: &mut T,
) -> R<ParsedSlice> {
    let mut dec = crate::entropy::CabacDecoder::new(data)
        .map_err(|_| SliceDataError::Eof("CABAC engine init"))?;
    let mut ctxs = CabacSliceContexts::new(slice_qp);

    let total = (mb_cols * mb_rows) as usize;
    let mut macroblocks: Vec<Macroblock> = Vec::with_capacity(total);
    let mut nz: Vec<MbNz> = vec![MbNz::default(); total];
    let mut pred_ctx: Vec<MbPredCtx> = vec![MbPredCtx::default(); total];
    let mut cabac_ctx: Vec<MbCabacCtx> = vec![MbCabacCtx::default(); total];
    let mut qp = slice_qp;
    let mut prev_dqp_nonzero = false;
    // §7.4.4: `mb_field_decoding_flag` is decoded once per macroblock pair in an
    // MBAFF frame (the SPS enables `mb_adaptive_frame_field_flag` and the slice is
    // not itself a field picture). The flag applies to both the top and bottom
    // macroblock of the pair; for frame-only / PAFF streams it is absent.
    let mbaff_frame = mb_aff && !field_pic_flag;
    let mut cur_pair_field = false;
    // Phase G.4: per-frame-MB `mb_field_decoding_flag`, populated as each
    // pair is decoded so `NeighbourCtx` can resolve mixed field/frame
    // neighbour addresses (§6.4.10.1) for already-decoded macroblocks.
    let mut field_flags: Vec<Option<bool>> = vec![None; total];

    for mb_idx in 0..total {
        let mb_x = (mb_idx as u32) % mb_cols;
        let mb_y = (mb_idx as u32) / mb_cols;

        if mbaff_frame && mb_idx % 2 == 0 {
            let left_field = if mb_x > 0 {
                cabac_ctx[(mb_y * mb_cols + mb_x - 1) as usize].mb_field_flag
            } else {
                false
            };
            let top_field = if mb_y > 0 {
                cabac_ctx[((mb_y - 1) * mb_cols + mb_x) as usize].mb_field_flag
            } else {
                false
            };
            cur_pair_field = ctxs.mb_field.decode(&mut dec, left_field, top_field);
            field_flags[mb_idx] = Some(cur_pair_field);
            if mb_idx + 1 < total {
                field_flags[mb_idx + 1] = Some(cur_pair_field);
            }
        }
        let nctx = NeighbourCtx::new(mbaff_frame, mb_rows, cur_pair_field, &field_flags);

        let (mb, this_nz, this_pred_ctx, this_cabac_ctx, new_qp, dqp_nonzero) =
            parse_intra_macroblock_cabac(
                &mut dec,
                &mut ctxs,
                mb_x,
                mb_y,
                mb_cols,
                &nz,
                &pred_ctx,
                &cabac_ctx,
                qp,
                prev_dqp_nonzero,
                transform_8x8_mode_flag,
                tracer,
                nctx,
            )?;
        qp = new_qp;
        prev_dqp_nonzero = dqp_nonzero;
        nz[mb_idx] = this_nz;
        pred_ctx[mb_idx] = this_pred_ctx;
        let mut this_cabac_ctx = this_cabac_ctx;
        this_cabac_ctx.mb_field_flag = cur_pair_field;
        cabac_ctx[mb_idx] = this_cabac_ctx;
        let mut mb = mb;
        mb.mb_field_flag = cur_pair_field;
        macroblocks.push(mb);

        // end_of_slice_flag (§7.3.4, §9.3.3.2.4) is read after *every*
        // macroblock, not just as an early-exit check; for a well-formed
        // single-slice-per-picture stream it must be 0 until the last MB and
        // 1 exactly there. A mismatch means the decode has desynced from the
        // bitstream (see `todo.md` Phase D: known remaining gap for streams
        // where an early macroblock is Intra_4x4 with real residual), so
        // bail rather than risk emitting wrong pixels.
        let end_of_slice = dec.decode_terminate() == 1;
        let is_last = mb_idx + 1 == total;
        if end_of_slice != is_last {
            return Err(SliceDataError::Unsupported(
                "end_of_slice_flag mismatch (CABAC decode desynced)",
            ));
        }
    }

    Ok(ParsedSlice {
        macroblocks,
        nz,
        mv_store: MvStore::new(total),
    })
}

