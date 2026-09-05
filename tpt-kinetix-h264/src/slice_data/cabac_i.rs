use super::*;

/// Parse the macroblock layer of a CABAC-coded I-slice (§7.3.4, §9.3).
///
/// `data` must already be byte-aligned to the start of `slice_data()`'s
/// CABAC payload (i.e. after consuming any `cabac_alignment_one_bit`s via
/// `BitReader::byte_align` + `BitReader::remaining_bytes`).
///
/// Scope: 4:2:0. I_PCM macroblocks are handled (CABAC engine is flushed and
/// restarted from the post-PCM byte boundary). `transform_size_8x8_flag`/Intra_8x8
/// (High profile) is supported for intra macroblocks only (Phase F.4) --
/// inter (P_8x8/B_Direct) 8x8 transform is not, matching the CAVLC path's
/// existing scope.
///
/// Multi-slice support: `first_mb` is the macroblock address this call's
/// bitstream actually starts at (`first_mb_in_slice`, 0 for a single-slice
/// picture or the first slice of a multi-slice one). `slice_id` is this
/// slice's index within the picture (assigned by the caller, sequentially
/// from 0). `macroblocks`/`nz`/`pred_ctx`/`cabac_ctx`/`slice_id_grid` are the
/// picture-wide accumulator buffers (sized `mb_cols * mb_rows`, pre-seeded by
/// the caller with `Macroblock::new_skip()` / `Default::default()` /
/// `u16::MAX` respectively before the picture's first slice, and left
/// untouched — carrying earlier slices' state — for every later slice of the
/// same picture); this call writes only into the `first_mb..` range it
/// actually decodes and leaves the rest alone. `slice_id_grid` MUST use a
/// sentinel not equal to any real `slice_id` (the caller uses `u16::MAX`) for
/// not-yet-decoded macroblocks, since [`NeighbourCtx::new_with_slices`] relies
/// on it to treat those as unavailable neighbours (§6.4.9).
///
/// Returns the exclusive upper bound of macroblocks actually decoded by THIS
/// call (`first_mb..returned_value`): equal to `mb_cols * mb_rows` when the
/// slice's own `end_of_slice_flag` legitimately fired only after covering the
/// rest of the picture (single-slice case, or the last slice of a multi-slice
/// picture), less than that when more slices are expected to follow.
#[allow(clippy::too_many_arguments)]
pub fn parse_i_slice_cabac<T: crate::trace::DecodeTracer>(
    data: &[u8],
    mb_cols: u32,
    mb_rows: u32,
    slice_qp: i32,
    mb_aff: bool,
    field_pic_flag: bool,
    transform_8x8_mode_flag: bool,
    tracer: &mut T,
    first_mb: u32,
    slice_id: u16,
    macroblocks: &mut [Macroblock],
    nz: &mut [MbNz],
    pred_ctx: &mut [MbPredCtx],
    cabac_ctx: &mut [MbCabacCtx],
    slice_id_grid: &mut [u16],
) -> R<usize> {
    if std::env::var("KINETIX_DUMP_PAYLOAD").is_ok() {
        if let Some(path) = std::env::temp_dir()
            .join("dbg_mbaff_i1_payload.bin")
            .to_str()
        {
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
    debug_assert_eq!(macroblocks.len(), total);
    debug_assert_eq!(nz.len(), total);
    debug_assert_eq!(pred_ctx.len(), total);
    debug_assert_eq!(cabac_ctx.len(), total);
    debug_assert_eq!(slice_id_grid.len(), total);
    let mut qp = slice_qp;
    let mut prev_dqp_nonzero = false;
    // §7.4.4: `mb_field_decoding_flag` is decoded once per macroblock pair in an
    // MBAFF frame (the SPS enables `mb_adaptive_frame_field_flag` and the slice is
    // not itself a field picture). The flag applies to both the top and bottom
    // macroblock of the pair; for frame-only / PAFF streams it is absent.
    let mbaff_frame = mb_aff && !field_pic_flag;
    // A PAFF field picture (`field_pic_flag == 1`) is field-coded throughout:
    // `mb_field_decoding_flag` is absent, but every macroblock still selects the
    // field significance/last CABAC context ranges (§9.3.3.1.3, Table 9-40) and
    // the field 4×4/8×8 inverse scans. The MBAFF branch below overrides this
    // per pair; it never runs for a field picture (mbaff_frame is then false).
    let mut cur_pair_field = field_pic_flag;
    // Phase G.4: per-frame-MB `mb_field_decoding_flag`, populated as each
    // pair is decoded so `NeighbourCtx` can resolve mixed field/frame
    // neighbour addresses (§6.4.10.1) for already-decoded macroblocks.
    // Reset per call (not carried across slices): MBAFF multi-slice is out of
    // scope for this parser's multi-slice support (see the module doc on
    // `parse_i_slice_cabac`'s `first_mb`/`slice_id` parameters) — no target
    // fixture combines MBAFF with more than one slice per picture, and
    // `mbaff_frame` is only ever true for an SPS with
    // `mb_adaptive_frame_field_flag` set, which the multi-slice CABAC callers
    // in this codebase do not currently feed continuation slices for.
    let mut field_flags: Vec<Option<bool>> = vec![None; total];
    let mut decoded_mb_count = total;
    let first_mb = first_mb as usize;

    for mb_idx in first_mb..total {
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
                    let left_idx = (mb_y * mb_cols + mb_x - 1) as usize;
                    let left_mb = &macroblocks[left_idx];
                    if !left_mb.skip {
                        cabac_ctx[left_idx].mb_field_flag
                    } else {
                        false
                    }
                } else {
                    false
                };
                let top_field = if mb_y > 0 {
                    let top_idx = ((mb_y - 1) * mb_cols + mb_x) as usize;
                    let top_mb = &macroblocks[top_idx];
                    if !top_mb.skip {
                        cabac_ctx[top_idx].mb_field_flag
                    } else {
                        false
                    }
                } else {
                    false
                };
                cur_pair_field = ctxs.mb_field.decode(&mut dec, left_field, top_field);
                if std::env::var("KINETIX_BINTRACE").is_ok() {
                    eprintln!(
                        "MBAFF pair at grid row {} col {mb_x}: mb_field_decoding_flag={cur_pair_field}",
                        2 * ((mb_idx >> 1) / mb_cols as usize)
                    );
                }
                field_flags[grid_idx] = Some(cur_pair_field);
                // The pair's bottom MB sits directly below the top MB in the
                // frame-MB grid.
                if grid_idx + (mb_cols as usize) < total {
                    field_flags[grid_idx + mb_cols as usize] = Some(cur_pair_field);
                }
            }
        }
        let nctx = NeighbourCtx::new_with_slices(
            mbaff_frame,
            mb_rows,
            cur_pair_field,
            &field_flags,
            slice_id_grid,
            slice_id,
        );

        let parse_result = parse_intra_macroblock_cabac(
            &mut dec,
            &mut ctxs,
            mb_x,
            mb_y,
            mb_cols,
            nz,
            pred_ctx,
            cabac_ctx,
            qp,
            prev_dqp_nonzero,
            transform_8x8_mode_flag,
            tracer,
            nctx,
        );

        // I_PCM under CABAC (§9.3.2.6, §9.3.4.6): after the mb_type CABAC decode
        // returns 25 the engine has called decode_terminate() internally.  Flush
        // to a byte boundary, read 384 raw PCM bytes, then reinitialise the CABAC
        // engine from the bytes that follow the PCM payload.
        if matches!(parse_result, Err(SliceDataError::IPcm)) {
            let remaining = dec.flush_to_pcm();
            let pcm_byte_count = 384usize; // 256 luma + 64 Cb + 64 Cr (4:2:0, 8-bit)
            if remaining.len() < pcm_byte_count {
                return Err(SliceDataError::Eof("I_PCM insufficient bytes (CABAC)"));
            }
            let pcm_samples: Vec<u8> = remaining[..pcm_byte_count].to_vec();
            let after_pcm = &remaining[pcm_byte_count..];
            dec = crate::entropy::CabacDecoder::new(after_pcm)
                .map_err(|_| SliceDataError::Eof("CABAC reinit after I_PCM"))?;

            let mut mb = Macroblock::new_skip();
            mb.skip = false;
            mb.mb_type = MbType::IPcm;
            mb.pcm_samples = pcm_samples;
            mb.mb_field_flag = cur_pair_field;

            let mut this_nz = MbNz {
                present: true,
                ..Default::default()
            };
            // §9.2.1: an I_PCM neighbour contributes nN=16 to CAVLC coeff_token
            // context; the same value is used for the CABAC CBF context.
            this_nz.luma = [16u8; 16];
            this_nz.chroma = [16u8; 8];

            let mut this_cabac_ctx = MbCabacCtx {
                present: true,
                ..Default::default()
            };
            this_cabac_ctx.is_intra16x16_or_pcm = true;
            this_cabac_ctx.mb_field_flag = cur_pair_field;

            nz[grid_idx] = this_nz;
            pred_ctx[grid_idx] = MbPredCtx {
                present: true,
                ..Default::default()
            };
            cabac_ctx[grid_idx] = this_cabac_ctx;
            macroblocks[grid_idx] = mb;
            slice_id_grid[grid_idx] = slice_id;

            // After I_PCM the CABAC engine was restarted; there is no
            // end_of_slice_flag to decode from the old engine state.
            // The next MB will be parsed by the freshly initialised decoder.
            continue;
        }

        let (mb, this_nz, this_pred_ctx, this_cabac_ctx, new_qp, dqp_nonzero) = parse_result?;
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
        slice_id_grid[grid_idx] = slice_id;

        // end_of_slice_flag (§7.3.4, §9.3.3.2.4). In an MBAFF *frame* the flag
        // is NOT coded after the TOP macroblock of a pair
        // (`CurrMbAddr % 2 == 0` ⇒ `moreDataFlag = 1` unconditionally, spec
        // §7.3.4); it follows only the bottom macroblock. For non-MBAFF slices
        // it follows every macroblock.
        //
        // A `1` here is `moreDataFlag = 0` (§7.3.4): this slice's
        // `macroblock_layer()` data is over. For a single-slice picture that
        // only ever happens on the true last MB; for a multi-slice picture
        // (`first_mb_in_slice` of the NEXT slice > 0) it legitimately fires
        // mid-picture, at the boundary of this slice's own MB range — that
        // is not a desync, it's the picture's remaining macroblocks
        // belonging to a different slice's bitstream that this call was
        // never going to see. Stop decoding and return what's been parsed so
        // far (the rest of `macroblocks` stays at its `Macroblock::new_skip`
        // default, same as before any multi-slice picture support existed).
        if !(mbaff_frame && mb_idx % 2 == 0) {
            let end_of_slice = dec.decode_terminate() == 1;
            if end_of_slice {
                let is_last = mb_idx + 1 == total;
                if !is_last {
                    decoded_mb_count = mb_idx + 1;
                }
                break;
            }
        }
    }

    Ok(decoded_mb_count)
}
