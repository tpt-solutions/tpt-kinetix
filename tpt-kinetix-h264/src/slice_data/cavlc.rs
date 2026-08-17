use super::*;


pub fn parse_i_slice<T: crate::trace::DecodeTracer>(
    reader: &mut BitReader,
    mb_cols: u32,
    mb_rows: u32,
    slice_qp: i32,
    chroma_qp_index_offset: i32,
    transform_8x8_mode: bool,
    mb_aff: bool,
    field_pic_flag: bool,
    tracer: &mut T,
) -> R<ParsedSlice> {
    let total = (mb_cols * mb_rows) as usize;
    let mut macroblocks: Vec<Macroblock> = Vec::with_capacity(total);
    let mut nz: Vec<MbNz> = vec![MbNz::default(); total];
    let mut pred_ctx: Vec<MbPredCtx> = vec![MbPredCtx::default(); total];
    let mut qp = slice_qp;
    // §7.4.4: `mb_field_decoding_flag` is read once per *macroblock pair* (i.e.
    // before every even-indexed macroblock) when the picture is an MBAFF frame
    // (the SPS enables `mb_adaptive_frame_field_flag` and the slice is not itself
    // a field picture). The flag applies to both the top and bottom macroblock of
    // the pair; for frame-only / PAFF streams it is simply absent.
    let mbaff_frame = mb_aff && !field_pic_flag;
    let mut cur_pair_field = false;
    // Phase G.4: per-frame-MB `mb_field_decoding_flag`, populated as each
    // pair is read so `NeighbourCtx` can resolve mixed field/frame neighbour
    // addresses (§6.4.10.1) for already-decoded macroblocks.
    let mut field_flags: Vec<Option<bool>> = vec![None; total];

    for mb_idx in 0..total {
        let mb_x = (mb_idx as u32) % mb_cols;
        let mb_y = (mb_idx as u32) / mb_cols;

        if mbaff_frame && mb_idx % 2 == 0 {
            cur_pair_field = reader
                .read_bit()
                .ok_or(SliceDataError::Eof("mb_field_decoding_flag"))?
                == 1;
            field_flags[mb_idx] = Some(cur_pair_field);
            if mb_idx + 1 < total {
                field_flags[mb_idx + 1] = Some(cur_pair_field);
            }
        }
        let nctx = NeighbourCtx::new(mbaff_frame, mb_rows, cur_pair_field, &field_flags);

        let mb_type = reader.read_ue().ok_or(SliceDataError::Eof("mb_type"))?;
        let (mb, this_nz, this_pred_ctx, new_qp) = parse_intra_macroblock(
            reader,
            mb_x,
            mb_y,
            mb_cols,
            &nz,
            &pred_ctx,
            qp,
            chroma_qp_index_offset,
            tracer,
            mb_type,
            transform_8x8_mode,
            nctx,
        )?;
        qp = new_qp;
        nz[mb_idx] = this_nz;
        pred_ctx[mb_idx] = this_pred_ctx;
        let mut mb = mb;
        mb.mb_field_flag = cur_pair_field;
        macroblocks.push(mb);
    }

    Ok(ParsedSlice {
        macroblocks,
        nz,
        mv_store: MvStore::new(total),
    })
}

/// One side's contribution to the Intra_4×4 MPM derivation (§8.3.1.1).
#[derive(Clone, Copy, PartialEq, Eq)]
enum NeighbourSide {
    /// The neighbouring macroblock is off-picture (or otherwise not coded):
    /// triggers `dcPredModePredictedFlag`, forcing *both* sides to DC.
    Unavailable,
    /// The neighbouring macroblock is present but not Intra_4×4/Intra_8×8:
    /// this side alone is defined as DC (2); the other side is unaffected.
    ForcedDc,
    /// The neighbouring macroblock is present and Intra_4×4: its stored mode.
    Real(u8),
}

impl NeighbourSide {
    /// This side's `intraMxMPredMode` value once
    /// `dcPredModePredictedFlag` has already been resolved to 0
    /// (i.e. neither side is [`NeighbourSide::Unavailable`]).
    fn value(self) -> u8 {
        match self {
            NeighbourSide::Real(v) => v,
            _ => 2,
        }
    }
}

/// Derive the most-probable Intra_4x4 prediction mode (`predIntra4x4PredMode`,
/// §8.3.1.1) for luma block `raster` (0..15 raster index within the current
/// MB) from the left/top neighbours. Shared by the CAVLC and CABAC I-slice
/// parsers -- entropy coding only changes how `prev_intra4x4_pred_mode_flag`/
/// `rem_intra4x4_pred_mode` are read, not this prediction.
pub(crate) fn mpm_pred_mode(
    pred_ctx_grid: &[MbPredCtx],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    modes: &[Intra4x4Mode; 16],
    raster: usize,
    nctx: NeighbourCtx,
) -> u8 {
    let bx = (raster % 4) as i32;
    let by = (raster / 4) as i32;
    let (left_idx, top_idx) = nctx.left_top(mb_x, mb_y, mb_cols);

    // §8.3.1.1: each side is one of three states — the neighbouring
    // *macroblock* is off-picture (`Unavailable`), present but not coded
    // Intra_4×4/Intra_8×8 (`ForcedDc`, its intraMxMPredMode is defined as 2
    // regardless of the other side), or present and Intra_4×4 (`Real`, the
    // actual stored mode). These are distinct: an unavailable *macroblock*
    // forces dcPredModePredictedFlag = 1 (both sides become DC,
    // short-circuiting the min entirely), while a present-but-non-4x4
    // neighbour only forces *that side* to DC and the other side's real mode
    // still participates in the min.
    let left_side = if bx > 0 {
        NeighbourSide::Real(modes[(by * 4 + bx - 1) as usize] as u8)
    } else if let Some(li) = left_idx {
        let n = &pred_ctx_grid[li];
        if !n.present {
            NeighbourSide::Unavailable
        } else if n.is_intra4x4 {
            NeighbourSide::Real(n.modes[(by * 4 + 3) as usize] as u8)
        } else {
            NeighbourSide::ForcedDc
        }
    } else {
        NeighbourSide::Unavailable
    };

    let top_side = if by > 0 {
        NeighbourSide::Real(modes[((by - 1) * 4 + bx) as usize] as u8)
    } else if let Some(ti) = top_idx {
        let n = &pred_ctx_grid[ti];
        if !n.present {
            NeighbourSide::Unavailable
        } else if n.is_intra4x4 {
            NeighbourSide::Real(n.modes[(3 * 4 + bx) as usize] as u8)
        } else {
            NeighbourSide::ForcedDc
        }
    } else {
        NeighbourSide::Unavailable
    };

    let dc_predicted =
        left_side == NeighbourSide::Unavailable || top_side == NeighbourSide::Unavailable;
    if dc_predicted {
        2u8
    } else {
        left_side.value().min(top_side.value())
    }
}

/// Derive `nC` for a luma 4×4 block (§9.2.1) from the left and top neighbour
/// TotalCoeff counts. `block` is the raster index (0..15) within the current MB.
/// Derive the most-probable Intra_8x8 prediction mode for 8x8 sub-block `i8`
/// (spec section 8.3.2.1.1). The 4x4 helper `mpm_pred_mode` assumes each block
/// occupies one entry in the 4x4 raster grid, so a naive raster-1 / raster-4
/// neighbour lookup would be wrong for 8x8 blocks (which occupy rasters
/// 0,4,8,12). The left/above 8x8 neighbours are therefore resolved explicitly.
/// The mode of a neighbour 8x8 block is read from its top-left 4x4 sub-block,
/// where it has been replicated.
pub(crate) fn mpm_pred_mode_8x8(
    pred_ctx_grid: &[MbPredCtx],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    modes: &[Intra4x4Mode; 16],
    i8: usize,
    nctx: NeighbourCtx,
) -> u8 {
    let (left_idx, top_idx) = nctx.left_top(mb_x, mb_y, mb_cols);
    let left = if i8 == 1 || i8 == 3 {
        // Left neighbour is the same-MB 8×8 block one column to the left:
        // i8=1 → i8=0 (top-left), i8=3 → i8=2 (bottom-left).
        NeighbourSide::Real(modes[raster_of_8x8_sub(i8 - 1, 0)] as u8)
    } else if let Some(li) = left_idx {
        let n = &pred_ctx_grid[li];
        if !n.present {
            NeighbourSide::Unavailable
        } else if n.is_intra4x4 {
            // Left of i8=0 → right column of left MB = i8=1 (top-right).
            // Left of i8=2 → right column of left MB = i8=3 (bottom-right).
            NeighbourSide::Real(n.modes[raster_of_8x8_sub(i8 + 1, 0)] as u8)
        } else {
            NeighbourSide::ForcedDc
        }
    } else {
        NeighbourSide::Unavailable
    };

    let top = if i8 == 2 || i8 == 3 {
        // Top neighbour is the same-MB 8×8 block one row above:
        // i8=2 → i8=0 (top-left), i8=3 → i8=1 (top-right).
        NeighbourSide::Real(modes[raster_of_8x8_sub(i8 - 2, 0)] as u8)
    } else if let Some(ti) = top_idx {
        let n = &pred_ctx_grid[ti];
        if !n.present {
            NeighbourSide::Unavailable
        } else if n.is_intra4x4 {
            // Top of i8=0 → bottom row of top MB = i8=2 (bottom-left).
            // Top of i8=1 → bottom row of top MB = i8=3 (bottom-right).
            NeighbourSide::Real(n.modes[raster_of_8x8_sub(i8 + 2, 0)] as u8)
        } else {
            NeighbourSide::ForcedDc
        }
    } else {
        NeighbourSide::Unavailable
    };

    let dc_predicted = left == NeighbourSide::Unavailable || top == NeighbourSide::Unavailable;
    if dc_predicted {
        2u8
    } else {
        left.value().min(top.value())
    }
}
fn luma_nc(
    nz: &[MbNz],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    cur: &MbNz,
    block: usize,
    nctx: NeighbourCtx,
) -> i32 {
    let bx = (block % 4) as i32;
    let by = (block / 4) as i32;
    let (left_idx, top_idx) = nctx.left_top(mb_x, mb_y, mb_cols);

    // Left neighbour block.
    let left = if bx > 0 {
        Some(cur.luma[(by * 4 + bx - 1) as usize])
    } else if let Some(li) = left_idx {
        let n = &nz[li];
        n.present.then(|| n.luma[(by * 4 + 3) as usize])
    } else {
        None
    };

    // Top neighbour block.
    let top = if by > 0 {
        Some(cur.luma[((by - 1) * 4 + bx) as usize])
    } else if let Some(ti) = top_idx {
        let n = &nz[ti];
        n.present.then(|| n.luma[(3 * 4 + bx) as usize])
    } else {
        None
    };

    combine_nc(left, top)
}

/// Derive `nC` for a chroma AC 4×4 block (4:2:0: 2×2 grid per component).
fn chroma_nc(
    nz: &[MbNz],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    cur: &MbNz,
    comp: usize,  // 0 = Cb, 1 = Cr
    block: usize, // 0..3 within component
    nctx: NeighbourCtx,
) -> i32 {
    let base = comp * 4;
    let bx = (block % 2) as i32;
    let by = (block / 2) as i32;
    let (left_idx, top_idx) = nctx.left_top(mb_x, mb_y, mb_cols);

    let left = if bx > 0 {
        Some(cur.chroma[base + (by * 2 + bx - 1) as usize])
    } else if let Some(li) = left_idx {
        let n = &nz[li];
        n.present.then(|| n.chroma[base + (by * 2 + 1) as usize])
    } else {
        None
    };

    let top = if by > 0 {
        Some(cur.chroma[base + ((by - 1) * 2 + bx) as usize])
    } else if let Some(ti) = top_idx {
        let n = &nz[ti];
        n.present.then(|| n.chroma[base + (2 + bx) as usize])
    } else {
        None
    };

    combine_nc(left, top)
}

/// Combine left/top neighbour TotalCoeff into `nC` (§9.2.1, equation 9-4).
fn combine_nc(left: Option<u8>, top: Option<u8>) -> i32 {
    match (left, top) {
        (Some(l), Some(t)) => (l as i32 + t as i32 + 1) >> 1,
        (Some(l), None) => l as i32,
        (None, Some(t)) => t as i32,
        (None, None) => 0,
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_intra_macroblock<T: crate::trace::DecodeTracer>(
    r: &mut BitReader,
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nz_grid: &[MbNz],
    pred_ctx_grid: &[MbPredCtx],
    prev_qp: i32,
    _chroma_qp_index_offset: i32,
    tracer: &mut T,
    mb_type: u32,
    transform_8x8_mode: bool,
    nctx: NeighbourCtx,
) -> R<(Macroblock, MbNz, MbPredCtx, i32)> {
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

    if mb_type == 25 {
        let total_bytes = 384usize;
        if r.remaining_bits() < total_bytes * 8 {
            return Err(SliceDataError::Eof("I_PCM insufficient bytes"));
        }
        mb.mb_type = MbType::IPcm;
        mb.skip = false;
        mb.pcm_samples = (0..total_bytes)
            .map(|_| r.read_u8().expect("I_PCM byte"))
            .collect();
        let mb_type_str = "IPcm".to_string();
        tracer.on_mb_parsed(
            mb_x,
            mb_y,
            &mb_type_str,
            mb.qp,
            mb.cbp,
            mb.intra_chroma_pred_mode,
            &[0; 16],
        );
        return Ok((mb, this_nz, this_pred_ctx, prev_qp));
    }

    // Determine intra mb_type semantics.
    let (is_i16x16, i16_mode, cbp_chroma, cbp_luma) = if mb_type == 0 {
        (false, 0u8, 0u8, 0u8)
    } else if (1..=24).contains(&mb_type) {
        let (m, cc, cl) = I16X16_TABLE[(mb_type - 1) as usize];
        (true, m, cc, cl)
    } else {
        return Err(SliceDataError::Unsupported("non-intra mb_type in I-slice"));
    };

    // `transform_size_8x8_flag` is read (after the mb_type decision but before the
    // prediction modes, §7.3.5.1) when the active PPS enables the High-profile 8×8
    // transform, and promotes an Intra_4×4 macroblock to Intra_8×8. Declared here
    // (function scope) so the residual stage below can see it.
    let mut is_8x8 = false;
    if !is_i16x16 && transform_8x8_mode {
        let bit = r
            .read_bit()
            .ok_or(SliceDataError::Eof("transform_size_8x8_flag"))?;
        is_8x8 = bit == 1;
    }

    if is_i16x16 {
        mb.mb_type = MbType::Intra16x16 {
            pred_mode: i16_mode,
            cbp_chroma,
            cbp_luma,
        };
        mb.cbp = cbp_luma | (cbp_chroma << 4);
    } else {
        mb.mb_type = MbType::Intra4x4;
        // `transform_size_8x8_flag` (already read above) promotes this Intra_4×4
        // macroblock to an Intra_8×8 one: prediction modes and the luma residual
        // are then coded/written per-8×8 block instead of per-4×4 block.
        let mut modes = [Intra4x4Mode::Dc; 16];
        if is_8x8 {
            mb.transform_size_8x8 = true;
            let mut modes8 = [0u8; 4];
            // Four 8×8 blocks; each carries one prediction mode, replicated into
            // its four 4×4 sub-blocks so neighbouring macroblocks derive the
            // most-probable mode from the correct neighbour block (§8.3.2.1.1).
            for i8 in 0..4usize {
                let pred_mode =
                    mpm_pred_mode_8x8(pred_ctx_grid, mb_x, mb_y, mb_cols, &modes, i8, nctx);
                let prev_flag = r
                    .read_bit()
                    .ok_or(SliceDataError::Eof("prev_intra8x8_pred_mode_flag"))?;
                let final_mode = if prev_flag == 1 {
                    pred_mode
                } else {
                    let rem = r
                        .read_bits(3)
                        .ok_or(SliceDataError::Eof("rem_intra8x8_pred_mode"))?
                        as u8;
                    // §7.4.5.1: rem is coded relative to predMode, skipping it.
                    if rem < pred_mode {
                        rem
                    } else {
                        rem + 1
                    }
                };
                modes8[i8] = final_mode;
                for sub in 0..4usize {
                    modes[raster_of_8x8_sub(i8, sub)] = Intra4x4Mode::from_u8(final_mode);
                }
            }
            mb.pred_modes_8x8 = Box::new(modes8);
        } else {
            // prev_intra4x4_pred_mode_flag / rem_intra4x4_pred_mode ×16 (§7.3.5.1),
            // read in luma4x4BlkIdx (Z-scan) order and converted to raster position
            // via `raster_of_8x8_sub` so the result lines up with how
            // `reconstruct.rs`/`luma_nc` index `pred_modes_4x4`/`nz`. For each block
            // the most-probable mode is derived from the left/top neighbour modes
            // (§8.3.1.1); a neighbour is treated as predicting DC when it's off the
            // picture or wasn't coded as Intra_4×4.
            for blk_idx in 0..16usize {
                let raster = raster_of_8x8_sub(blk_idx / 4, blk_idx % 4);
                let pred_mode =
                    mpm_pred_mode(pred_ctx_grid, mb_x, mb_y, mb_cols, &modes, raster, nctx);

                let prev_flag = r
                    .read_bit()
                    .ok_or(SliceDataError::Eof("prev_intra4x4_pred_mode_flag"))?;
                let final_mode = if prev_flag == 1 {
                    pred_mode
                } else {
                    let rem = r
                        .read_bits(3)
                        .ok_or(SliceDataError::Eof("rem_intra4x4_pred_mode"))?
                        as u8;
                    // §7.4.5.1: rem is coded relative to predMode, skipping it.
                    if rem < pred_mode {
                        rem
                    } else {
                        rem + 1
                    }
                };
                modes[raster] = Intra4x4Mode::from_u8(final_mode);
            }
        }
        mb.pred_modes_4x4 = Box::new(modes);
        this_pred_ctx.is_intra4x4 = true;
        this_pred_ctx.modes = modes;
    }

    // intra_chroma_pred_mode (§7.3.5.1), present for 4:2:0/4:2:2.
    let chroma_pred = r
        .read_ue()
        .ok_or(SliceDataError::Eof("intra_chroma_pred_mode"))?;
    mb.intra_chroma_pred_mode = chroma_pred as u8;

    // coded_block_pattern for Intra_4×4 (I_16×16 carries CBP in mb_type).
    let (cbp_l, cbp_c) = if is_i16x16 {
        (cbp_luma, cbp_chroma)
    } else {
        let code_num = r
            .read_ue()
            .ok_or(SliceDataError::Eof("coded_block_pattern"))?;
        if code_num as usize >= GOLOMB_TO_INTRA4X4_CBP.len() {
            return Err(SliceDataError::Unsupported("cbp code_num out of range"));
        }
        let cbp = GOLOMB_TO_INTRA4X4_CBP[code_num as usize];
        mb.cbp = cbp;
        (cbp & 0x0F, cbp >> 4)
    };

    // mb_qp_delta present when CBP != 0 or I_16×16.
    let mut qp = prev_qp;
    if cbp_l != 0 || cbp_c != 0 || is_i16x16 {
        let dqp = r.read_se().ok_or(SliceDataError::Eof("mb_qp_delta"))?;
        // §7.4.5, 8-bit (QpBdOffsetY = 0): QPY = (QPY_prev + dqp + 52) % 52.
        qp = (prev_qp + dqp + 52).rem_euclid(52);
    }
    mb.qp = qp;

    // `is_8x8` already reflects the per-macroblock `transform_size_8x8_flag`
    // bit read above (line ~745). It is intentionally NOT re-derived from
    // `transform_8x8_mode && mb_type == 0` here: that would force every
    // Intra_4×4 macroblock into the 8×8 residual layout whenever the PPS
    // enables the High-profile 8×8 transform, even for MBs that signalled
    // `transform_size_8x8_flag == 0` — those would then be parsed into
    // `luma_coeffs_8x8` but reconstructed via the 4×4 path (which reads the
    // all-zero `luma_coeffs`), producing silent wrong pixels.

    // ---- Residual parsing ----
    parse_intra_residuals(
        r,
        &mut mb,
        &mut this_nz,
        nz_grid,
        mb_x,
        mb_y,
        mb_cols,
        is_i16x16,
        cbp_l,
        cbp_c,
        is_8x8,
        tracer,
        nctx,
    )?;

    // Emit MB-level trace after full parse.
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

    Ok((mb, this_nz, this_pred_ctx, qp))
}

/// Bundles the CABAC context-variable state for every I-slice syntax element
/// implemented here. Context adaptation persists across the *whole slice*
/// (not per macroblock), so this is created once per slice and threaded
/// through every macroblock decode.


/// Parse the macroblock layer of a P slice (CAVLC, §7.3.4).
///
/// `reader` must be positioned at `SliceHeader::data_bit_offset`. P-slice
/// inter macroblocks use mb_type 0..=4 (Table 7-11); intra macroblocks inside
/// a P slice use I-table mb_type + 5 (§7.4.5). Skip runs are signalled with
/// `mb_skip_run` (ue(v)), read once per slice and again after each coded MB.
pub fn parse_p_slice<T: crate::trace::DecodeTracer>(
    reader: &mut BitReader,
    mb_cols: u32,
    mb_rows: u32,
    slice_qp: i32,
    num_ref_idx_l0_active: u32,
    chroma_qp_index_offset: i32,
    transform_8x8_mode: bool,
    tracer: &mut T,
) -> R<ParsedSlice> {
    let total = (mb_cols * mb_rows) as usize;
    let mut macroblocks: Vec<Macroblock> = Vec::with_capacity(total);
    let mut nz: Vec<MbNz> = vec![MbNz::default(); total];
    let mut pred_ctx: Vec<MbPredCtx> = vec![MbPredCtx::default(); total];
    let mut qp = slice_qp;
    // `mb_skip_run` is signalled once per slice and again after each coded MB
    // (§7.4.5); `None` means a fresh value must be read for the next MB.
    let mut mb_skip_run: Option<u32> = None;

    for mb_idx in 0..total {
        let mb_x = (mb_idx as u32) % mb_cols;
        let mb_y = (mb_idx as u32) / mb_cols;

        if mb_skip_run.is_none() {
            let run = reader.read_ue().ok_or(SliceDataError::Eof("mb_skip_run"))?;
            if run > total as u32 {
                return Err(SliceDataError::Unsupported("mb_skip_run out of range"));
            }
            mb_skip_run = Some(run);
        }
        let run = mb_skip_run.as_mut().expect("set above");
        if *run > 0 {
            *run -= 1;
            let mut mb = Macroblock::new_skip();
            mb.qp = qp;
            mb.skip = true;
            nz[mb_idx] = MbNz {
                present: true,
                ..Default::default()
            };
            // A skipped macroblock is present in the picture but is not coded
            // Intra_4×4/Intra_8×8, so neighbours must see it as `present` with
            // `is_intra4x4 = false` (i.e. ForcedDc). Leaving it at the grid
            // default (`present = false`) would make a later Intra_4×4 macroblock
            // treat the skip neighbour as off-picture and force its
            // `predIntra4x4PredMode` to DC, corrupting the prediction mode (and
            // cascading to its own neighbours).
            pred_ctx[mb_idx] = MbPredCtx {
                present: true,
                ..Default::default()
            };
            macroblocks.push(mb);
            continue;
        }
        mb_skip_run = None;

        let (mb, this_nz, this_pred_ctx, new_qp) = parse_p_macroblock(
            reader,
            mb_x,
            mb_y,
            mb_cols,
            &nz,
            &pred_ctx,
            qp,
            num_ref_idx_l0_active,
            chroma_qp_index_offset,
            transform_8x8_mode,
            tracer,
        )?;
        qp = new_qp;
        nz[mb_idx] = this_nz;
        pred_ctx[mb_idx] = this_pred_ctx;
        macroblocks.push(mb);
    }

    // Derive motion vectors for every inter macroblock (§8.4.1). The store is
    // per-slice; single-slice pictures use slice id 0.
    let mut mv_store = MvStore::new(total);
    crate::mv::predict_slice_mvs(&mut mv_store, mb_cols, 0, 0, &macroblocks)?;

    Ok(ParsedSlice {
        macroblocks,
        nz,
        mv_store,
    })
}

/// Decode `refIdxL0` for one partition (§7.3.5.1). A reference count of 1
/// implies index 0 with no bits; 2 implies a single bit (`^1`); otherwise ue(v).
fn read_ref_idx(r: &mut BitReader, ref_count: u32) -> R<i32> {
    let val = if ref_count == 1 {
        0
    } else if ref_count == 2 {
        (r.read_bit().ok_or(SliceDataError::Eof("ref_idx_l0"))? ^ 1) as u32
    } else {
        r.read_ue().ok_or(SliceDataError::Eof("ref_idx_l0"))?
    };
    if val >= ref_count {
        return Err(SliceDataError::Unsupported("ref_idx_l0 overflow"));
    }
    Ok(val as i32)
}

#[allow(clippy::too_many_arguments)]
fn parse_p_macroblock<T: crate::trace::DecodeTracer>(
    r: &mut BitReader,
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nz_grid: &[MbNz],
    pred_ctx_grid: &[MbPredCtx],
    prev_qp: i32,
    num_ref_idx_l0_active: u32,
    chroma_qp_index_offset: i32,
    transform_8x8_mode: bool,
    tracer: &mut T,
) -> R<(Macroblock, MbNz, MbPredCtx, i32)> {
    let mut mb = Macroblock::new_skip();
    mb.skip = false;
    let mut this_nz = MbNz {
        present: true,
        ..Default::default()
    };
    let this_pred_ctx = MbPredCtx {
        present: true,
        ..Default::default()
    };

    let mb_type_raw = r.read_ue().ok_or(SliceDataError::Eof("mb_type"))?;
    if mb_type_raw >= 5 {
        let i_type = mb_type_raw - 5;
        if i_type > 25 {
            return Err(SliceDataError::Unsupported(
                "mb_type out of range in P slice",
            ));
        }
        return parse_intra_macroblock(
            r,
            mb_x,
            mb_y,
            mb_cols,
            nz_grid,
            pred_ctx_grid,
            prev_qp,
            chroma_qp_index_offset,
            tracer,
            i_type,
            transform_8x8_mode,
            NeighbourCtx::NONE,
        );
    }

    // Inter macroblock — Table 7-11 P slice mb_type 0..=4.
    let (mb_type, ref0) = match mb_type_raw {
        0 => (MbType::PL016x16, false),
        1 => (MbType::P16x8, false),
        2 => (MbType::P8x16, false),
        3 => (MbType::P8x8, false),
        4 => (MbType::P8x8ref0, true),
        _ => unreachable!(),
    };
    mb.mb_type = mb_type;
    let mut motion = crate::macroblock::InterMotion::default();

    if mb_type_raw == 3 || mb_type_raw == 4 {
        // P_8x8: four sub-partitions with their own sub_mb_type / refs / mvd.
        let mut sub_types = [0u8; 4];
        for sub in sub_types.iter_mut() {
            let raw = r.read_ue().ok_or(SliceDataError::Eof("sub_mb_type"))?;
            if raw >= 4 {
                return Err(SliceDataError::Unsupported("sub_mb_type out of range"));
            }
            *sub = raw as u8;
        }
        motion.sub_mb_type = Some(sub_types);
        let ref_count = if ref0 { 1 } else { num_ref_idx_l0_active };
        for part in 0..4 {
            motion.ref_idx_l0.push(read_ref_idx(r, ref_count)?);
            let n_sub = P_SUB_MB_PARTS[sub_types[part] as usize];
            for _ in 0..n_sub {
                let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 x"))?;
                let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 y"))?;
                motion.mvd_l0.push((mx, my));
            }
        }
    } else {
        let n_parts = if mb_type_raw == 0 { 1 } else { 2 };
        for _ in 0..n_parts {
            motion
                .ref_idx_l0
                .push(read_ref_idx(r, num_ref_idx_l0_active)?);
            let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 x"))?;
            let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 y"))?;
            motion.mvd_l0.push((mx, my));
        }
    }
    mb.motion = Some(motion);

    // coded_block_pattern for inter macroblocks (Table 9-4, inter ordering).
    let code_num = r
        .read_ue()
        .ok_or(SliceDataError::Eof("coded_block_pattern"))?;
    if code_num as usize >= GOLOMB_TO_INTER_CBP.len() {
        return Err(SliceDataError::Unsupported("cbp code_num out of range"));
    }
    let cbp = GOLOMB_TO_INTER_CBP[code_num as usize];
    mb.cbp = cbp;
    let cbp_l = cbp & 0x0F;
    let cbp_c = cbp >> 4;

    // mb_qp_delta present when any residual block is coded.
    let mut qp = prev_qp;
    if cbp_l != 0 || cbp_c != 0 {
        let dqp = r.read_se().ok_or(SliceDataError::Eof("mb_qp_delta"))?;
        qp = (prev_qp + dqp + 52).rem_euclid(52);
    }
    mb.qp = qp;

    parse_intra_residuals(
        r,
        &mut mb,
        &mut this_nz,
        nz_grid,
        mb_x,
        mb_y,
        mb_cols,
        false,
        cbp_l,
        cbp_c,
        false,
        tracer,
        NeighbourCtx::NONE,
    )?;

    let mb_type_str = format!("{:?}", mb_type);
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

    Ok((mb, this_nz, this_pred_ctx, qp))
}

/// Parse the macroblock layer of a B slice (CAVLC, §7.3.4).
///
/// B-slice inter macroblocks use mb_type 0..=22 (Table 7-14, §7.4.5); intra
/// macroblocks inside a B slice use I-table mb_type + 23 (§7.4.5). Skip runs
/// are signalled with `mb_skip_run` (ue(v)), read once per slice and again
/// after each coded MB, and produce `MbType::BSkip` (not PSkip).
#[allow(clippy::too_many_arguments)]
pub fn parse_b_slice<T: crate::trace::DecodeTracer>(
    reader: &mut BitReader,
    mb_cols: u32,
    mb_rows: u32,
    slice_qp: i32,
    num_ref_idx_l0_active: u32,
    num_ref_idx_l1_active: u32,
    chroma_qp_index_offset: i32,
    transform_8x8_mode: bool,
    tracer: &mut T,
) -> R<ParsedSlice> {
    let total = (mb_cols * mb_rows) as usize;
    let mut macroblocks: Vec<Macroblock> = Vec::with_capacity(total);
    let mut nz: Vec<MbNz> = vec![MbNz::default(); total];
    let mut pred_ctx: Vec<MbPredCtx> = vec![MbPredCtx::default(); total];
    let mut qp = slice_qp;
    let mut mb_skip_run: Option<u32> = None;

    for mb_idx in 0..total {
        let mb_x = (mb_idx as u32) % mb_cols;
        let mb_y = (mb_idx as u32) / mb_cols;

        if mb_skip_run.is_none() {
            let run = reader.read_ue().ok_or(SliceDataError::Eof("mb_skip_run"))?;
            if run > total as u32 {
                return Err(SliceDataError::Unsupported("mb_skip_run out of range"));
            }
            mb_skip_run = Some(run);
        }
        let run = mb_skip_run.as_mut().expect("set above");
        if *run > 0 {
            *run -= 1;
            let mut mb = Macroblock::new_skip();
            mb.mb_type = MbType::BSkip;
            mb.qp = qp;
            mb.skip = true;
            nz[mb_idx] = MbNz {
                present: true,
                ..Default::default()
            };
            pred_ctx[mb_idx] = MbPredCtx {
                present: true,
                ..Default::default()
            };
            macroblocks.push(mb);
            continue;
        }
        mb_skip_run = None;

        let (mb, this_nz, this_pred_ctx, new_qp) = parse_b_macroblock(
            reader,
            mb_x,
            mb_y,
            mb_cols,
            &nz,
            &pred_ctx,
            qp,
            num_ref_idx_l0_active,
            num_ref_idx_l1_active,
            chroma_qp_index_offset,
            transform_8x8_mode,
            tracer,
        )?;
        qp = new_qp;
        nz[mb_idx] = this_nz;
        pred_ctx[mb_idx] = this_pred_ctx;
        macroblocks.push(mb);
    }

    let mut mv_store = MvStore::new(total);
    crate::mv::predict_b_slice_mvs(&mut mv_store, mb_cols, 0, 0, &macroblocks)?;

    Ok(ParsedSlice {
        macroblocks,
        nz,
        mv_store,
    })
}

#[allow(clippy::too_many_arguments)]
fn parse_b_macroblock<T: crate::trace::DecodeTracer>(
    r: &mut BitReader,
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nz_grid: &[MbNz],
    pred_ctx_grid: &[MbPredCtx],
    prev_qp: i32,
    num_ref_idx_l0_active: u32,
    num_ref_idx_l1_active: u32,
    chroma_qp_index_offset: i32,
    transform_8x8_mode: bool,
    tracer: &mut T,
) -> R<(Macroblock, MbNz, MbPredCtx, i32)> {
    use crate::macroblock::BPredDir;

    let mut mb = Macroblock::new_skip();
    mb.skip = false;
    let mut this_nz = MbNz {
        present: true,
        ..Default::default()
    };
    let this_pred_ctx = MbPredCtx {
        present: true,
        ..Default::default()
    };

    let mb_type_raw = r.read_ue().ok_or(SliceDataError::Eof("mb_type"))?;

    // mb_type >= 23 → intra macroblock (subtract 23 to get I-slice index).
    if mb_type_raw >= 23 {
        let i_type = mb_type_raw - 23;
        if i_type > 25 {
            return Err(SliceDataError::Unsupported(
                "mb_type out of range in B slice",
            ));
        }
        return parse_intra_macroblock(
            r,
            mb_x,
            mb_y,
            mb_cols,
            nz_grid,
            pred_ctx_grid,
            prev_qp,
            chroma_qp_index_offset,
            tracer,
            i_type,
            transform_8x8_mode,
            NeighbourCtx::NONE,
        );
    }

    // B-inter types (Table 7-14).
    let mut motion = crate::macroblock::InterMotion::default();

    match mb_type_raw {
        0 => {
            // B_Direct_16x16 — no motion data in bitstream.
            mb.mb_type = MbType::BDirect16x16;
        }
        1 => {
            // B_L0_16x16
            mb.mb_type = MbType::BL016x16;
            motion
                .ref_idx_l0
                .push(read_ref_idx(r, num_ref_idx_l0_active)?);
            let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 x"))?;
            let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 y"))?;
            motion.mvd_l0.push((mx, my));
        }
        2 => {
            // B_L1_16x16
            mb.mb_type = MbType::BL116x16;
            motion
                .ref_idx_l1
                .push(read_ref_idx(r, num_ref_idx_l1_active)?);
            let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l1 x"))?;
            let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l1 y"))?;
            motion.mvd_l1.push((mx, my));
        }
        3 => {
            // B_Bi_16x16
            mb.mb_type = MbType::BBi16x16;
            motion
                .ref_idx_l0
                .push(read_ref_idx(r, num_ref_idx_l0_active)?);
            motion
                .ref_idx_l1
                .push(read_ref_idx(r, num_ref_idx_l1_active)?);
            let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 x"))?;
            let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 y"))?;
            motion.mvd_l0.push((mx, my));
            let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l1 x"))?;
            let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l1 y"))?;
            motion.mvd_l1.push((mx, my));
        }
        4..=21 => {
            // Two-partition B types (16×8 or 8×16).
            let idx = (mb_type_raw - 4) as usize;
            let (is_16x8, dir0, dir1) = B_2PART_TABLE[idx];
            mb.mb_type = if is_16x8 {
                MbType::B16x8
            } else {
                MbType::B8x16
            };
            motion.pred_dirs = vec![dir0, dir1];

            // Pre-fill ref_idx with LIST_NOT_USED, overwrite for applicable partitions.
            motion.ref_idx_l0 = vec![crate::mv::LIST_NOT_USED; 2];
            motion.ref_idx_l1 = vec![crate::mv::LIST_NOT_USED; 2];

            // Read all L0 ref indices first.
            for part in 0..2usize {
                if motion.pred_dirs[part] == BPredDir::L0 || motion.pred_dirs[part] == BPredDir::Bi
                {
                    motion.ref_idx_l0[part] = read_ref_idx(r, num_ref_idx_l0_active)?;
                }
            }
            // Then all L1 ref indices.
            for part in 0..2usize {
                if motion.pred_dirs[part] == BPredDir::L1 || motion.pred_dirs[part] == BPredDir::Bi
                {
                    motion.ref_idx_l1[part] = read_ref_idx(r, num_ref_idx_l1_active)?;
                }
            }
            // All L0 MVDs first (§7.3.5.1), then all L1 MVDs.
            for part in 0..2usize {
                if motion.pred_dirs[part] == BPredDir::L0 || motion.pred_dirs[part] == BPredDir::Bi
                {
                    let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 x"))?;
                    let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 y"))?;
                    motion.mvd_l0.push((mx, my));
                }
            }
            for part in 0..2usize {
                if motion.pred_dirs[part] == BPredDir::L1 || motion.pred_dirs[part] == BPredDir::Bi
                {
                    let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l1 x"))?;
                    let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l1 y"))?;
                    motion.mvd_l1.push((mx, my));
                }
            }
        }
        22 => {
            // B_8x8 — four 8×8 sub-partitions.
            mb.mb_type = MbType::BB8x8;
            let mut sub_types = [0u8; 4];
            for st in sub_types.iter_mut() {
                let raw = r.read_ue().ok_or(SliceDataError::Eof("sub_mb_type"))?;
                if raw >= 13 {
                    return Err(SliceDataError::Unsupported("B sub_mb_type out of range"));
                }
                *st = raw as u8;
            }
            motion.sub_mb_type_b = Some(sub_types);

            // Pre-fill ref indices with LIST_NOT_USED.
            motion.ref_idx_l0 = vec![crate::mv::LIST_NOT_USED; 4];
            motion.ref_idx_l1 = vec![crate::mv::LIST_NOT_USED; 4];
            motion.pred_dirs = sub_types
                .iter()
                .map(|&st| {
                    if (st as usize) < 13 {
                        crate::mv::B_SUB_MB_DIR[st as usize]
                    } else {
                        BPredDir::Direct
                    }
                })
                .collect();

            // All L0 ref indices.
            for part in 0..4usize {
                let dir = motion.pred_dirs[part];
                if dir != BPredDir::Direct && (dir == BPredDir::L0 || dir == BPredDir::Bi) {
                    motion.ref_idx_l0[part] = read_ref_idx(r, num_ref_idx_l0_active)?;
                }
            }
            // All L1 ref indices.
            for part in 0..4usize {
                let dir = motion.pred_dirs[part];
                if dir != BPredDir::Direct && (dir == BPredDir::L1 || dir == BPredDir::Bi) {
                    motion.ref_idx_l1[part] = read_ref_idx(r, num_ref_idx_l1_active)?;
                }
            }
            // All L0 mvds (all parts × sub-parts, skipping Direct).
            for part in 0..4usize {
                let dir = motion.pred_dirs[part];
                if dir == BPredDir::Direct {
                    continue;
                }
                if dir == BPredDir::L0 || dir == BPredDir::Bi {
                    let n_sub = crate::mv::B_SUB_MB_PARTS[sub_types[part] as usize];
                    for _ in 0..n_sub {
                        let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 x"))?;
                        let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 y"))?;
                        motion.mvd_l0.push((mx, my));
                    }
                }
            }
            // All L1 mvds.
            for part in 0..4usize {
                let dir = motion.pred_dirs[part];
                if dir == BPredDir::Direct {
                    continue;
                }
                if dir == BPredDir::L1 || dir == BPredDir::Bi {
                    let n_sub = crate::mv::B_SUB_MB_PARTS[sub_types[part] as usize];
                    for _ in 0..n_sub {
                        let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l1 x"))?;
                        let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l1 y"))?;
                        motion.mvd_l1.push((mx, my));
                    }
                }
            }
        }
        _ => unreachable!("B mb_type_raw bounded to 0..=22 above"),
    }

    // Attach motion data for inter MBs (Direct has no motion struct).
    if mb_type_raw != 0 {
        mb.motion = Some(motion);
    }

    // coded_block_pattern for inter macroblocks.
    let code_num = r
        .read_ue()
        .ok_or(SliceDataError::Eof("coded_block_pattern"))?;
    if code_num as usize >= GOLOMB_TO_INTER_CBP.len() {
        return Err(SliceDataError::Unsupported("cbp code_num out of range"));
    }
    let cbp = GOLOMB_TO_INTER_CBP[code_num as usize];
    mb.cbp = cbp;
    let cbp_l = cbp & 0x0F;
    let cbp_c = cbp >> 4;

    let mut qp = prev_qp;
    if cbp_l != 0 || cbp_c != 0 {
        let dqp = r.read_se().ok_or(SliceDataError::Eof("mb_qp_delta"))?;
        qp = (prev_qp + dqp + 52).rem_euclid(52);
    }
    mb.qp = qp;

    parse_intra_residuals(
        r,
        &mut mb,
        &mut this_nz,
        nz_grid,
        mb_x,
        mb_y,
        mb_cols,
        false,
        cbp_l,
        cbp_c,
        false,
        tracer,
        NeighbourCtx::NONE,
    )?;

    let mb_type_str = format!("{:?}", mb.mb_type);
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

    Ok((mb, this_nz, this_pred_ctx, qp))
}

#[allow(clippy::too_many_arguments)]
fn parse_intra_residuals<T: crate::trace::DecodeTracer>(
    r: &mut BitReader,
    mb: &mut Macroblock,
    this_nz: &mut MbNz,
    nz_grid: &[MbNz],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    is_i16x16: bool,
    cbp_luma: u8,
    cbp_chroma: u8,
    is_8x8: bool,
    tracer: &mut T,
    nctx: NeighbourCtx,
) -> R<()> {
    use crate::trace::TracePlane;

    // Intra_16×16 luma DC block (16 coeffs) — always present for I_16×16.
    if is_i16x16 {
        let nc = luma_nc(nz_grid, mb_x, mb_y, mb_cols, this_nz, 0, nctx);
        let (coeffs, _tc, t1) = parse_cavlc_block(r, nc, 16)?;
        mb.luma_dc = coeffs;
        tracer.on_cavlc_coeffs(mb_x, mb_y, TracePlane::Luma, 16, &coeffs);
        tracer.on_cavlc_block_info(mb_x, mb_y, TracePlane::Luma, 16, nc, _tc, t1, 0);
    }

    // Luma residual blocks. With the 8×8 transform, each of the four 8×8 luma
    // regions is coded as four interleaved 16-coefficient CAVLC scans (one
    // per `i4x4` sub-stream, §7.3.5.3.3): coefficient `k` (0..16, already in
    // 4×4-zigzag order from `parse_cavlc_block`, same as the plain 4×4 path)
    // of sub-stream `sub` lands at raster position
    // `crate::transform::CAVLC_SCAN8X8[16*sub + k]` (CAVLC's own dedicated
    // 8×8 residual scan, distinct from a plain `4*k+sub` interleave of the
    // combined zigzag order -- see that table's doc comment). Converted to
    // a zigzag position via `INVERSE_ZIGZAG_8X8` since `dequant_idct_8x8`
    // (`transform.rs`) expects its `coeffs` input in zigzag order and
    // inverse-zigzags it itself.
    if is_8x8 {
        let mut block64 = [0i16; 64];
        for i8x8 in 0..4usize {
            if (cbp_luma >> i8x8) & 1 == 0 {
                continue;
            }
            block64.fill(0);
            for sub in 0..4usize {
                let raster = raster_of_8x8_sub(i8x8, sub);
                let nc = luma_nc(nz_grid, mb_x, mb_y, mb_cols, this_nz, raster, nctx);
                let (coeffs, tc, t1) = parse_cavlc_block(r, nc, 16)?;
                for k in 0..16usize {
                    let raw = crate::transform::CAVLC_SCAN8X8[16 * sub + k] as usize;
                    let cavlc_raster = raw;
                    let zz = crate::transform::INVERSE_ZIGZAG_8X8[cavlc_raster];
                    block64[zz] = coeffs[k];
                }
                this_nz.luma[raster] = tc;
                tracer.on_cavlc_coeffs(
                    mb_x,
                    mb_y,
                    TracePlane::Luma,
                    (i8x8 * 4 + sub) as u8,
                    &coeffs,
                );
                tracer.on_cavlc_block_info(
                    mb_x,
                    mb_y,
                    TracePlane::Luma,
                    (i8x8 * 4 + sub) as u8,
                    nc,
                    tc,
                    t1,
                    0,
                );
            }
            // §9.2.1 / FFmpeg h264_cavlc.c: after all 4 sub-streams are
            // decoded, accumulate the block's total non-zero count into the
            // top-left sub-block position (p0). This matches x264/FFmpeg's
            // nnz layout (`nnz[0] += nnz[1] + nnz[8] + nnz[9]`): p1/p2/p3
            // keep their individual sub-stream counts so that intra-MB
            // lookups from subsequent 8×8 blocks use the same nC the encoder
            // used, while p0 carries the accumulated sum for the p0 position.
            // Neighbouring MBs look at bottom-row / right-column positions
            // which may be p1, p2, or p3 — their individual counts are what
            // x264 encodes against, so they must be left as-is here too.
            let p0 = raster_of_8x8_sub(i8x8, 0);
            let p1 = raster_of_8x8_sub(i8x8, 1);
            let p2 = raster_of_8x8_sub(i8x8, 2);
            let p3 = raster_of_8x8_sub(i8x8, 3);
            this_nz.luma[p0] = this_nz.luma[p0]
                .saturating_add(this_nz.luma[p1])
                .saturating_add(this_nz.luma[p2])
                .saturating_add(this_nz.luma[p3]);
            mb.luma_coeffs_8x8[i8x8] = block64;
        }
    } else {
        // Luma 4×4 blocks. Both intra and inter use the BlkIdx scan order defined
        // by §7.3.5.2: iterate i8x8 (0..4), gate on CodedBlockPatternLuma bit i8x8,
        // then iterate i4x4 (0..4). BlkIdx = i8x8*4 + i4x4. raster_of_8x8_sub maps
        // (i8x8, i4x4) → raster position for nC neighbor lookups and coeff storage.
        let luma_max = if is_i16x16 { 15 } else { 16 };
        let blocks: Vec<usize> = {
            let mut v = Vec::with_capacity(16);
            for blk8 in 0..4usize {
                if (cbp_luma >> blk8) & 1 == 0 {
                    continue;
                }
                for sub in 0..4usize {
                    v.push(raster_of_8x8_sub(blk8, sub));
                }
            }
            v
        };
        for block in blocks {
            let nc = luma_nc(nz_grid, mb_x, mb_y, mb_cols, this_nz, block, nctx);
            let pos_before = r.bit_position() as u32;
            let result = parse_cavlc_block(r, nc, luma_max);
            let (coeffs, tc, t1) = result?;
            let pos_after = r.bit_position();
            this_nz.luma[block] = tc;
            // For I_16×16 the 15 AC coeffs occupy zigzag positions 1..=15.
            if is_i16x16 {
                let mut shifted = [0i16; 16];
                shifted[1..16].copy_from_slice(&coeffs[0..15]);
                mb.luma_coeffs[block] = shifted;
                tracer.on_cavlc_coeffs(mb_x, mb_y, TracePlane::Luma, block as u8, &shifted);
                tracer.on_cavlc_block_info(
                    mb_x,
                    mb_y,
                    TracePlane::Luma,
                    block as u8,
                    nc,
                    tc,
                    t1,
                    0,
                );
                tracer.on_cavlc_block_info_with_pos(
                    mb_x,
                    mb_y,
                    TracePlane::Luma,
                    block as u8,
                    nc,
                    tc,
                    t1,
                    pos_before,
                    pos_after,
                );
            } else {
                mb.luma_coeffs[block] = coeffs;
                tracer.on_cavlc_coeffs(mb_x, mb_y, TracePlane::Luma, block as u8, &coeffs);
                tracer.on_cavlc_block_info(
                    mb_x,
                    mb_y,
                    TracePlane::Luma,
                    block as u8,
                    nc,
                    tc,
                    t1,
                    0,
                );
                tracer.on_cavlc_block_info_with_pos(
                    mb_x,
                    mb_y,
                    TracePlane::Luma,
                    block as u8,
                    nc,
                    tc,
                    t1,
                    pos_before,
                    pos_after,
                );
            }
        }
    }

    // Chroma DC (Cb, Cr) present when cbp_chroma & 3 (i.e. == 1 or 2).
    if cbp_chroma != 0 {
        // chroma DC: 4 coeffs each, nC = -1 selects the chroma-DC coeff_token.
        let (cb_dc, _tc) = parse_cavlc_chroma_dc(r)?;
        let (cr_dc, _tc2) = parse_cavlc_chroma_dc(r)?;
        mb.chroma_dc_cb = cb_dc;
        mb.chroma_dc_cr = cr_dc;
        let mut cb_padded = [0i16; 16];
        cb_padded[..4].copy_from_slice(&cb_dc);
        let mut cr_padded = [0i16; 16];
        cr_padded[..4].copy_from_slice(&cr_dc);
        tracer.on_cavlc_coeffs(mb_x, mb_y, TracePlane::Cb, 16, &cb_padded);
        tracer.on_cavlc_coeffs(mb_x, mb_y, TracePlane::Cr, 16, &cr_padded);
    }

    // Chroma AC present only when cbp_chroma == 2.
    if cbp_chroma == 2 {
        for comp in 0..2usize {
            for block in 0..4usize {
                let nc = chroma_nc(nz_grid, mb_x, mb_y, mb_cols, this_nz, comp, block, nctx);
                let (coeffs, tc, t1) = parse_cavlc_block(r, nc, 15)?;
                this_nz.chroma[comp * 4 + block] = tc;
                // AC coeffs occupy zigzag positions 1..=15 (DC handled above).
                let mut shifted = [0i16; 16];
                shifted[1..16].copy_from_slice(&coeffs[0..15]);
                let plane = if comp == 0 {
                    TracePlane::Cb
                } else {
                    TracePlane::Cr
                };
                tracer.on_cavlc_coeffs(mb_x, mb_y, plane, block as u8, &shifted);
                tracer.on_cavlc_block_info(mb_x, mb_y, plane, block as u8, nc, tc, t1, 0);
                if comp == 0 {
                    mb.chroma_cb_coeffs[block] = shifted;
                } else {
                    mb.chroma_cr_coeffs[block] = shifted;
                }
            }
        }
    }

    Ok(())
}

/// Raster 4×4 index within a macroblock for the `sub`-th block of the `blk8`-th
/// 8×8 group (spec block scan order 6-10 / Figure 6-10).
pub fn raster_of_8x8_sub(blk8: usize, sub: usize) -> usize {
    // 8×8 group top-left in 4×4 units.
    let gx = (blk8 % 2) * 2;
    let gy = (blk8 / 2) * 2;
    let sx = sub % 2;
    let sy = sub / 2;
    (gy + sy) * 4 + (gx + sx)
}

/// Parse a CAVLC-coded residual block (§9.2). Returns the coefficients in
/// **zigzag** scan order (length `max_coeff`) and the TotalCoeff for nC context.
pub fn parse_cavlc_block(r: &mut BitReader, n_c: i32, max_coeff: usize) -> R<([i16; 16], u8, u8)> {
    let mut out = [0i16; 16];
    let (total_coeff, trailing_ones) = cavlc_tables::read_coeff_token(r, n_c)?;
    if total_coeff == 0 {
        return Ok((out, 0, 0));
    }

    let tc = total_coeff as usize;
    let t1 = trailing_ones as usize;
    let mut levels = [0i32; 16];

    // Trailing-one signs.
    for level in levels.iter_mut().take(t1) {
        let sign = r.read_bit().ok_or(SliceDataError::Eof("T1 sign"))?;
        *level = if sign == 1 { -1 } else { 1 };
    }

    // Remaining levels (§9.2.2).
    let mut suffix_length: u32 = if tc > 10 && t1 < 3 { 1 } else { 0 };
    #[allow(clippy::needless_range_loop)]
    for i in t1..tc {
        // level_prefix: count leading zeros then the terminating 1.
        let mut level_prefix: u32 = 0;
        loop {
            let bit = r.read_bit().ok_or(SliceDataError::Eof("level_prefix"))?;
            if bit == 1 {
                break;
            }
            level_prefix += 1;
            if level_prefix > MAX_LEVEL_PREFIX {
                return Err(SliceDataError::Cavlc);
            }
        }

        let level_suffix_size: u32 = if level_prefix == 14 && suffix_length == 0 {
            4
        } else if level_prefix >= 15 {
            level_prefix - 3
        } else {
            suffix_length
        };

        let level_suffix = if level_suffix_size > 0 {
            r.read_bits(level_suffix_size as u8)
                .ok_or(SliceDataError::Eof("level_suffix"))? as i32
        } else {
            0
        };

        let mut level_code = (level_prefix.min(15) << suffix_length) as i32 + level_suffix;
        if level_prefix >= 15 && suffix_length == 0 {
            level_code += 15;
        }
        if level_prefix >= 16 {
            level_code += (1 << (level_prefix - 3)) - 4096;
        }
        // First coefficient after trailing ones gets +2 bias when t1 < 3.
        if i == t1 && t1 < 3 {
            level_code += 2;
        }

        let level = if level_code % 2 == 0 {
            (level_code + 2) >> 1
        } else {
            (-level_code - 1) >> 1
        };
        levels[i] = level;

        if suffix_length == 0 {
            suffix_length = 1;
        }
        if level.unsigned_abs() > (3u32 << (suffix_length - 1)) && suffix_length < 6 {
            suffix_length += 1;
        }
    }

    // total_zeros + run_before (§9.2.3, §9.2.4).
    let total_zeros = if tc < max_coeff {
        cavlc_tables::read_total_zeros_4x4(r, total_coeff)? as i32
    } else {
        0
    };

    let mut zeros_left = total_zeros;
    // Place coefficients from highest-frequency to lowest.
    let mut pos = (tc as i32) - 1 + total_zeros;
    for (i, &level) in levels.iter().enumerate().take(tc) {
        if pos < 0 || pos >= out.len() as i32 {
            return Err(SliceDataError::Cavlc);
        }
        out[pos as usize] = level as i16;
        if i < tc - 1 {
            let run = if zeros_left > 0 {
                cavlc_tables::read_run_before(r, zeros_left.min(255) as u8)? as i32
            } else {
                0
            };
            pos -= 1 + run;
            zeros_left -= run;
        }
    }

    Ok((out, total_coeff, trailing_ones))
}

/// Parse a chroma-DC CAVLC block (4 coefficients, nC = -1).
fn parse_cavlc_chroma_dc(r: &mut BitReader) -> R<([i16; 4], u8)> {
    let mut out = [0i16; 4];
    let (total_coeff, trailing_ones) = cavlc_tables::read_coeff_token(r, -1)?;
    if total_coeff == 0 {
        return Ok((out, 0));
    }

    let tc = total_coeff as usize;
    let t1 = trailing_ones as usize;
    let mut levels = [0i32; 4];

    for level in levels.iter_mut().take(t1) {
        let sign = r.read_bit().ok_or(SliceDataError::Eof("chroma T1 sign"))?;
        *level = if sign == 1 { -1 } else { 1 };
    }

    let mut suffix_length: u32 = 0;
    #[allow(clippy::needless_range_loop)]
    for i in t1..tc {
        let mut level_prefix: u32 = 0;
        loop {
            let bit = r
                .read_bit()
                .ok_or(SliceDataError::Eof("chroma level_prefix"))?;
            if bit == 1 {
                break;
            }
            level_prefix += 1;
            if level_prefix > MAX_LEVEL_PREFIX {
                return Err(SliceDataError::Cavlc);
            }
        }
        let level_suffix_size = if level_prefix == 14 && suffix_length == 0 {
            4
        } else if level_prefix >= 15 {
            level_prefix - 3
        } else {
            suffix_length
        };
        let level_suffix = if level_suffix_size > 0 {
            r.read_bits(level_suffix_size as u8)
                .ok_or(SliceDataError::Eof("chroma level_suffix"))? as i32
        } else {
            0
        };
        let mut level_code = (level_prefix.min(15) << suffix_length) as i32 + level_suffix;
        if level_prefix >= 15 && suffix_length == 0 {
            level_code += 15;
        }
        if level_prefix >= 16 {
            level_code += (1 << (level_prefix - 3)) - 4096;
        }
        if i == t1 && t1 < 3 {
            level_code += 2;
        }
        let level = if level_code % 2 == 0 {
            (level_code + 2) >> 1
        } else {
            (-level_code - 1) >> 1
        };
        levels[i] = level;
        if suffix_length == 0 {
            suffix_length = 1;
        }
        if level.unsigned_abs() > (3u32 << (suffix_length - 1)) && suffix_length < 6 {
            suffix_length += 1;
        }
    }

    let total_zeros = if tc < 4 {
        cavlc_tables::read_total_zeros_chroma_dc(r, total_coeff)? as i32
    } else {
        0
    };

    let mut zeros_left = total_zeros;
    let mut pos = (tc as i32) - 1 + total_zeros;
    for (i, &level) in levels.iter().enumerate().take(tc) {
        if (0..4).contains(&pos) {
            out[pos as usize] = level as i16;
        }
        if i < tc - 1 {
            let run = if zeros_left > 0 {
                cavlc_tables::read_run_before(r, zeros_left.min(255) as u8)? as i32
            } else {
                0
            };
            pos -= 1 + run;
            zeros_left -= run;
        }
    }

    Ok((out, total_coeff))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_nc_rules() {
        assert_eq!(combine_nc(None, None), 0);
        assert_eq!(combine_nc(Some(4), None), 4);
        assert_eq!(combine_nc(None, Some(6)), 6);
        assert_eq!(combine_nc(Some(3), Some(4)), (3 + 4 + 1) >> 1);
    }
}
