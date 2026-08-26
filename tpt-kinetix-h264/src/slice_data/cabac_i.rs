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
    if std::env::var("KINETIX_DUMP_PAYLOAD").is_ok() {
        if let Some(path) = std::env::temp_dir().join("dbg_mbaff_i1_payload.bin").to_str() {
            let mut buf = Vec::new();
            buf.extend_from_slice(&mb_cols.to_le_bytes());
            buf.extend_from_slice(&mb_rows.to_le_bytes());
            buf.extend_from_slice(&slice_qp.to_le_bytes());
            buf.push(mb_aff as u8);
            buf.push(field_pic_flag as u8);
            buf.push(transform_8x8_mode_flag as u8);
            buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
            buf.extend_from_slice(data);
            let _ = std::fs::write(path, buf);
        }
    }

    let mut dec = crate::entropy::CabacDecoder::new(data)
        .map_err(|_| SliceDataError::Eof("CABAC engine init"))?;
    let mut ctxs = CabacSliceContexts::new(slice_qp);

    let total = (mb_cols * mb_rows) as usize;
    let mut macroblocks: Vec<Macroblock> = (0..total).map(|_| Macroblock::new_skip()).collect();
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
        // MBAFF addressing (§6.4.2/§7.4.4): consecutive macroblock addresses
        // enumerate each PAIR as (top, bottom) before advancing horizontally —
        // addr 2k/2k+1 are the two MBs of pair k (pair k sits at frame-MB
        // column `pair % mb_cols`, MB rows `2*(pair / mb_cols)` and +1).
        // Frame-only streams use plain raster.
        let (mb_x, mb_y, grid_idx) = if mbaff_frame {
            let pair = mb_idx >> 1;
            let parity = mb_idx & 1;
            let px = pair % mb_cols as usize;
            let py = pair / mb_cols as usize;
            let mb_row = 2 * py + parity;
            (
                px as u32,
                mb_row as u32,
                // The grid index MUST be the macroblock's own frame-MB
                // address (`mb_y * mb_cols + mb_x`) — using the *pair* row
                // here made every bottom MB commit its neighbour-context
                // state over its top sibling's slot while leaving its own
                // slot zeroed, so all later MBs read a zeroed left/top CBP /
                // chroma-mode / nnz context and the CABAC engine drifted
                // (session #32e, found via dbg_mbaff_oracle bin diff).
                mb_row * mb_cols as usize + px,
            )
        } else {
            ((mb_idx as u32) % mb_cols, (mb_idx as u32) / mb_cols, mb_idx)
        };

        if mbaff_frame && mb_idx % 2 == 0 {
            // Session #32c experiment: KINETIX_NO_FIELD_BINS=1 skips the
            // mb_field_decoding_flag reads entirely, to test whether x264
            // actually emitted them for this stream.
            if std::env::var("KINETIX_NO_FIELD_BINS").is_err() {
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
                eprintln!(
                    "MBAFF pair at grid row {} col {mb_x}: mb_field_decoding_flag={cur_pair_field}",
                    2 * ((mb_idx >> 1) / mb_cols as usize)
                );
                field_flags[grid_idx] = Some(cur_pair_field);
                // The pair's bottom MB sits directly below the top MB in the
                // frame-MB grid.
                if grid_idx + (mb_cols as usize) < total {
                    field_flags[grid_idx + mb_cols as usize] = Some(cur_pair_field);
                }
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
        if std::env::var("KINETIX_BINTRACE").is_ok() {
            let mut modes = [0u8; 16];
            for (i, m) in mb.pred_modes_4x4.iter().enumerate() {
                modes[i] = *m as u8;
            }
            let (rs, os) = dec.debug_state();
            eprintln!(
                "TRC MB{mb_idx} px={mb_x} py={mb_y} field={cur_pair_field} type={:?} cbp={:#04x} qp={qp} t8={} chroma={} modes={modes:?} state={rs:#06x}/{os:#010x}",
                mb.mb_type,
                mb.cbp,
                mb.transform_size_8x8,
                mb.intra_chroma_pred_mode,
            );
        }
        nz[grid_idx] = this_nz;
        pred_ctx[grid_idx] = this_pred_ctx;
        let mut this_cabac_ctx = this_cabac_ctx;
        this_cabac_ctx.mb_field_flag = cur_pair_field;
        cabac_ctx[grid_idx] = this_cabac_ctx;
        let mut mb = mb;
        mb.mb_field_flag = cur_pair_field;
        macroblocks[grid_idx] = mb;

        // end_of_slice_flag (§7.3.4, §9.3.3.2.4) is read after *every*
        // macroblock. It must be 0 for every mid-slice MB; a 1 there means the
        // decode has desynced from the bitstream (see `todo.md` Phase D), so
        // bail rather than risk emitting wrong pixels. On the LAST MB either
        // value is accepted: spec-conformant encoders write a final terminate
        // (=1), but x264 omits it (it emits exactly total-1 terminate bins,
        // one before each MB except the first) and ffmpeg exits on the MB
        // count in that case.
        let end_of_slice = dec.decode_terminate() == 1;
        let is_last = mb_idx + 1 == total;
        if !is_last && end_of_slice {
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
