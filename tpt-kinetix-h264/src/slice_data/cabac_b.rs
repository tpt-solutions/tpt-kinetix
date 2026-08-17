use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn parse_p_macroblock_cabac<T: crate::trace::DecodeTracer>(
    dec: &mut crate::entropy::CabacDecoder,
    ctxs: &mut PbCabacSliceContexts,
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nz_grid: &[MbNz],
    pred_ctx_grid: &[MbPredCtx],
    cabac_ctx_grid: &[MbCabacCtx],
    inter_grid: &[MbInterCabacCtx],
    prev_qp: i32,
    prev_dqp_nonzero: bool,
    num_ref_idx_l0_active: u32,
    _chroma_qp_index_offset: i32,
    transform_8x8_mode_flag: bool,
    tracer: &mut T,
) -> R<(
    Macroblock,
    MbNz,
    MbPredCtx,
    MbCabacCtx,
    MbInterCabacCtx,
    i32,
    bool,
)> {
    let left_idx = (mb_x > 0).then(|| (mb_y * mb_cols + mb_x - 1) as usize);
    let top_idx = (mb_y > 0).then(|| ((mb_y - 1) * mb_cols + mb_x) as usize);

    let inter_type = ctxs.mb_type_p.decode(dec);
    match inter_type {
        None => {
            // Intra macroblock inside P slice.
            let intra_t = ctxs.intra_suffix.decode(dec);
            let (mb, this_nz, this_pred_ctx, this_cabac_ctx, new_qp, dqp_nz) =
                parse_intra_mb_cabac_pb(
                    dec,
                    ctxs,
                    mb_x,
                    mb_y,
                    mb_cols,
                    nz_grid,
                    pred_ctx_grid,
                    cabac_ctx_grid,
                    prev_qp,
                    prev_dqp_nonzero,
                    intra_t,
                    transform_8x8_mode_flag,
                    tracer,
                )?;
            let this_inter = MbInterCabacCtx::default();
            Ok((
                mb,
                this_nz,
                this_pred_ctx,
                this_cabac_ctx,
                this_inter,
                new_qp,
                dqp_nz,
            ))
        }
        Some(shape) => {
            // shape: 0=P_L0_16x16, 1=P_L0_L0_16x8, 2=P_L0_L0_8x16, 3=P_8x8
            let mb_type = match shape {
                0 => MbType::PL016x16,
                1 => MbType::P16x8,
                2 => MbType::P8x16,
                3 | _ => MbType::P8x8,
            };
            let mut mb = Macroblock::new_skip();
            mb.skip = false;
            mb.mb_type = mb_type;
            let mut this_nz = MbNz {
                present: true,
                ..Default::default()
            };
            let this_pred_ctx = MbPredCtx {
                present: true,
                ..Default::default()
            };
            let mut this_inter = MbInterCabacCtx {
                present: true,
                ..Default::default()
            };
            let mut motion = crate::macroblock::InterMotion::default();

            let n_parts: usize = if shape == 3 {
                4
            } else if shape == 0 {
                1
            } else {
                2
            };

            if shape == 3 {
                // P_8x8: read all sub_mb_types first.
                let mut sub_types = [0u8; 4];
                for st in &mut sub_types {
                    *st = ctxs.sub_mb_p.decode(dec) as u8;
                }
                motion.sub_mb_type = Some(sub_types);
                // ref_idx per 8×8 partition (only coded when num_ref_idx_l0_active > 1, spec §7.3.5.2).
                for part in 0..4 {
                    let (col4, row4, _, _) = partition_dims(mb_type, part);
                    let ri = if num_ref_idx_l0_active > 1 {
                        let (xp, yp) = (col4 as u32 * 4, row4 as u32 * 4);
                        let (lg, tg) = ref_idx_gt0_neighbors(
                            inter_grid,
                            &this_inter,
                            left_idx,
                            top_idx,
                            xp,
                            yp,
                            8,
                            8,
                            0,
                        );
                        let r = ctxs.ref_idx.decode(dec, lg, tg);
                        if r >= num_ref_idx_l0_active {
                            return Err(SliceDataError::Unsupported("ref_idx overflow"));
                        }
                        r
                    } else {
                        0
                    };
                    motion.ref_idx_l0.push(ri as i32);
                    // fill 2×2 blocks in the 8×8 quadrant with ref_idx
                    let blks = partition_blocks(col4, row4, 2, 2);
                    if ri > 0 {
                        for &b in &blks {
                            this_inter.l0_ref_gt0 |= 1 << b;
                        }
                    }
                }
                // MVDs per sub-partition.
                for part in 0..4 {
                    let sub_t = sub_types[part] as usize;
                    let n_sub = P_SUB_MB_PARTS[sub_t];
                    let (col4, row4, w4, h4) = partition_dims(mb_type, part);
                    // Each sub-partition within the 8×8 is stacked vertically (8×4) or side-by-side (4×8).
                    // For simplicity treat each sub as occupying the same 8×8 block for amvd context.
                    let (xp, yp, wp, hp) = (
                        col4 as u32 * 4,
                        row4 as u32 * 4,
                        w4 as u32 * 4,
                        h4 as u32 * 4,
                    );
                    for sub_i in 0..n_sub {
                        let mvd_x = cabac_decode_mvd_component(
                            dec,
                            &mut ctxs.mvd_l0_x,
                            inter_grid,
                            &this_inter,
                            left_idx,
                            top_idx,
                            xp,
                            yp,
                            wp,
                            hp,
                            0,
                            0,
                        )?;
                        let mvd_y = cabac_decode_mvd_component(
                            dec,
                            &mut ctxs.mvd_l0_y,
                            inter_grid,
                            &this_inter,
                            left_idx,
                            top_idx,
                            xp,
                            yp,
                            wp,
                            hp,
                            0,
                            1,
                        )?;
                        eprintln!("  MB({mb_x},{mb_y}) P8x8 part={part} sub={sub_i} mvd=({mvd_x},{mvd_y})");
                        motion.mvd_l0.push((mvd_x, mvd_y));
                        let blk = row4 * 4 + col4; // representative block for last sub
                        let _ = sub_i;
                        this_inter.l0_mvd_abs[blk] = [
                            (mvd_x.unsigned_abs() as u8).min(70),
                            (mvd_y.unsigned_abs() as u8).min(70),
                        ];
                    }
                }
            } else {
                // 16×16, 16×8, or 8×16.
                // ref_idx is only coded when num_ref_idx_l0_active > 1 (spec §7.3.5.2).
                for part in 0..n_parts {
                    let (col4, row4, w4, h4) = partition_dims(mb_type, part);
                    let (xp, yp, wp, hp) = (
                        col4 as u32 * 4,
                        row4 as u32 * 4,
                        w4 as u32 * 4,
                        h4 as u32 * 4,
                    );
                    let ri = if num_ref_idx_l0_active > 1 {
                        let (lg, tg) = ref_idx_gt0_neighbors(
                            inter_grid,
                            &this_inter,
                            left_idx,
                            top_idx,
                            xp,
                            yp,
                            wp,
                            hp,
                            0,
                        );
                        let r = ctxs.ref_idx.decode(dec, lg, tg);
                        if r >= num_ref_idx_l0_active {
                            return Err(SliceDataError::Unsupported("ref_idx overflow"));
                        }
                        r
                    } else {
                        0
                    };
                    motion.ref_idx_l0.push(ri as i32);
                    let blks = partition_blocks(col4, row4, w4, h4);
                    if ri > 0 {
                        for &b in &blks {
                            this_inter.l0_ref_gt0 |= 1 << b;
                        }
                    }

                    let mvd_x = cabac_decode_mvd_component(
                        dec,
                        &mut ctxs.mvd_l0_x,
                        inter_grid,
                        &this_inter,
                        left_idx,
                        top_idx,
                        xp,
                        yp,
                        wp,
                        hp,
                        0,
                        0,
                    )?;
                    let mvd_y = cabac_decode_mvd_component(
                        dec,
                        &mut ctxs.mvd_l0_y,
                        inter_grid,
                        &this_inter,
                        left_idx,
                        top_idx,
                        xp,
                        yp,
                        wp,
                        hp,
                        0,
                        1,
                    )?;
                    eprintln!("  MB({mb_x},{mb_y}) mvd_l0[{}]=({mvd_x},{mvd_y}) ri={ri}", motion.mvd_l0.len());
                    motion.mvd_l0.push((mvd_x, mvd_y));
                    this_inter.set_partition_l0(&blks, mvd_x, mvd_y, ri as i32);
                }
            }

            mb.motion = Some(motion);
            let (s_r, s_o) = dec.debug_state();
            let (cbp_l, cbp_c) =
                decode_inter_cbp_cabac(dec, ctxs, cabac_ctx_grid, mb_x, mb_y, mb_cols)?;
            let cbp = cbp_l | (cbp_c << 4);
            mb.cbp = cbp;
            let (e_r, e_o) = dec.debug_state();
            eprintln!("  MB({mb_x},{mb_y}) inter cbp={cbp:#04x}(l={cbp_l:#x} c={cbp_c}) after_mvd={s_r:#06x}/{s_o:#010x} after_cbp={e_r:#06x}/{e_o:#010x}");
            let mut qp = prev_qp;
            let mut dqp_nz = false;
            if cbp_l != 0 || cbp_c != 0 {
                let (r0, o0) = dec.debug_state();
                let dqp = ctxs.qp_delta.decode(dec, prev_dqp_nonzero);
                dqp_nz = dqp != 0;
                qp = (prev_qp + dqp + 52).rem_euclid(52);
                let (r1, o1) = dec.debug_state();
                eprintln!("  MB({mb_x},{mb_y}) dqp={dqp} qp={qp} after_qpdelta={r1:#06x}/{o1:#010x}  (before={r0:#06x}/{o0:#010x})");
            }
            mb.qp = qp;

            let (r_pre_res, o_pre_res) = dec.debug_state();
            let this_cabac_ctx = decode_inter_residual_cabac(
                dec,
                ctxs,
                &mut mb,
                &mut this_nz,
                nz_grid,
                cabac_ctx_grid,
                mb_x,
                mb_y,
                mb_cols,
                cbp_l,
                cbp_c,
            )?;
            let (r_post_res, o_post_res) = dec.debug_state();
            eprintln!("  MB({mb_x},{mb_y}) residual: before={r_pre_res:#06x}/{o_pre_res:#010x} after={r_post_res:#06x}/{o_post_res:#010x}");

            let mb_type_str = format!("{:?}", mb.mb_type);
            tracer.on_mb_parsed(mb_x, mb_y, &mb_type_str, mb.qp, mb.cbp, 0, &[0u8; 16]);
            Ok((
                mb,
                this_nz,
                this_pred_ctx,
                this_cabac_ctx,
                this_inter,
                qp,
                dqp_nz,
            ))
        }
    }
}

/// Parse a CABAC-coded B-slice (§7.3.4, §9.3).
#[allow(clippy::too_many_arguments)]
pub fn parse_b_slice_cabac<T: crate::trace::DecodeTracer>(
    data: &[u8],
    mb_cols: u32,
    mb_rows: u32,
    slice_qp: i32,
    cabac_init_idc: usize,
    num_ref_idx_l0_active: u32,
    num_ref_idx_l1_active: u32,
    chroma_qp_index_offset: i32,
    transform_8x8_mode_flag: bool,
    tracer: &mut T,
) -> R<ParsedSlice> {
    let mut dec = crate::entropy::CabacDecoder::new(data)
        .map_err(|_| SliceDataError::Eof("CABAC engine init"))?;
    let mut ctxs = PbCabacSliceContexts::new_b(slice_qp, cabac_init_idc);

    let total = (mb_cols * mb_rows) as usize;
    let mut macroblocks: Vec<Macroblock> = Vec::with_capacity(total);
    let mut nz: Vec<MbNz> = vec![MbNz::default(); total];
    let mut pred_ctx: Vec<MbPredCtx> = vec![MbPredCtx::default(); total];
    let mut cabac_ctx: Vec<MbCabacCtx> = vec![MbCabacCtx::default(); total];
    let mut inter_ctx: Vec<MbInterCabacCtx> = vec![MbInterCabacCtx::default(); total];
    let mut qp = slice_qp;
    let mut prev_dqp_nonzero = false;

    for mb_idx in 0..total {
        let mb_x = (mb_idx as u32) % mb_cols;
        let mb_y = (mb_idx as u32) / mb_cols;
        let left_idx = (mb_x > 0).then(|| mb_idx - 1);
        let top_idx = (mb_y > 0).then(|| mb_idx - mb_cols as usize);

        let skip_neighbors = crate::entropy::MbSkipNeighbors {
            left_available: mb_x > 0,
            left_skipped: left_idx.map(|i| macroblocks[i].skip).unwrap_or(false),
            top_available: mb_y > 0,
            top_skipped: top_idx.map(|i| macroblocks[i].skip).unwrap_or(false),
        };
        let is_skip = ctxs.mb_skip.decode(&mut dec, &skip_neighbors);
        if is_skip {
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
            cabac_ctx[mb_idx] = MbCabacCtx {
                present: true,
                cbp_word: 0,
                ..Default::default()
            };
            inter_ctx[mb_idx] = MbInterCabacCtx {
                present: true,
                ..Default::default()
            };
            macroblocks.push(mb);
            // Skip MBs have no end_of_slice_flag (it lives inside macroblock_layer() which
            // is not called for skip MBs per spec §7.3.4).
            continue;
        }

        let (mb, this_nz, this_pred_ctx, this_cabac_ctx, this_inter_ctx, new_qp, dqp_nz) =
            parse_b_macroblock_cabac(
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
                num_ref_idx_l1_active,
                chroma_qp_index_offset,
                transform_8x8_mode_flag,
                tracer,
            )?;
        qp = new_qp;
        prev_dqp_nonzero = dqp_nz;
        nz[mb_idx] = this_nz;
        pred_ctx[mb_idx] = this_pred_ctx;
        cabac_ctx[mb_idx] = this_cabac_ctx;
        inter_ctx[mb_idx] = this_inter_ctx;
        macroblocks.push(mb);

        let end_of_slice = dec.decode_terminate() == 1;
        let is_last = mb_idx + 1 == total;
        if end_of_slice != is_last {
            return Err(SliceDataError::Unsupported(
                "end_of_slice_flag mismatch (B-CABAC)",
            ));
        }
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
fn parse_b_macroblock_cabac<T: crate::trace::DecodeTracer>(
    dec: &mut crate::entropy::CabacDecoder,
    ctxs: &mut PbCabacSliceContexts,
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nz_grid: &[MbNz],
    pred_ctx_grid: &[MbPredCtx],
    cabac_ctx_grid: &[MbCabacCtx],
    inter_grid: &[MbInterCabacCtx],
    prev_qp: i32,
    prev_dqp_nonzero: bool,
    num_ref_idx_l0_active: u32,
    num_ref_idx_l1_active: u32,
    _chroma_qp_index_offset: i32,
    transform_8x8_mode_flag: bool,
    tracer: &mut T,
) -> R<(
    Macroblock,
    MbNz,
    MbPredCtx,
    MbCabacCtx,
    MbInterCabacCtx,
    i32,
    bool,
)> {
    use crate::macroblock::BPredDir;

    let left_idx = (mb_x > 0).then(|| (mb_y * mb_cols + mb_x - 1) as usize);
    let top_idx = (mb_y > 0).then(|| ((mb_y - 1) * mb_cols + mb_x) as usize);

    let b_inter_type = ctxs.mb_type_b.decode(dec);
    if b_inter_type.is_none() {
        // Intra macroblock inside B slice.
        let intra_t = ctxs.intra_suffix.decode(dec);
        let (mb, this_nz, this_pred_ctx, this_cabac_ctx, new_qp, dqp_nz) = parse_intra_mb_cabac_pb(
            dec,
            ctxs,
            mb_x,
            mb_y,
            mb_cols,
            nz_grid,
            pred_ctx_grid,
            cabac_ctx_grid,
            prev_qp,
            prev_dqp_nonzero,
            intra_t,
            transform_8x8_mode_flag,
            tracer,
        )?;
        return Ok((
            mb,
            this_nz,
            this_pred_ctx,
            this_cabac_ctx,
            MbInterCabacCtx::default(),
            new_qp,
            dqp_nz,
        ));
    }
    let b_type_raw = b_inter_type.unwrap();

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
    let mut this_inter = MbInterCabacCtx {
        present: true,
        ..Default::default()
    };
    let mut motion = crate::macroblock::InterMotion::default();

    match b_type_raw {
        0 => {
            mb.mb_type = MbType::BDirect16x16;
            // no motion data to decode
        }
        1 => {
            mb.mb_type = MbType::BL016x16;
            let (lg, tg) =
                ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 0);
            let ri = ctxs.ref_idx.decode(dec, lg, tg);
            if ri >= num_ref_idx_l0_active {
                return Err(SliceDataError::Unsupported("ref_idx L0 overflow"));
            }
            motion.ref_idx_l0.push(ri as i32);
            let blks: Vec<usize> = (0..16).collect();
            if ri > 0 {
                for &b in &blks {
                    this_inter.l0_ref_gt0 |= 1 << b;
                }
            }
            let mvx = cabac_decode_mvd_component(
                dec,
                &mut ctxs.mvd_l0_x,
                inter_grid,
                &this_inter,
                left_idx,
                top_idx,
                0,
                0,
                16,
                16,
                0,
                0,
            )?;
            let mvy = cabac_decode_mvd_component(
                dec,
                &mut ctxs.mvd_l0_y,
                inter_grid,
                &this_inter,
                left_idx,
                top_idx,
                0,
                0,
                16,
                16,
                0,
                1,
            )?;
            motion.mvd_l0.push((mvx, mvy));
            this_inter.set_partition_l0(&blks, mvx, mvy, ri as i32);
        }
        2 => {
            mb.mb_type = MbType::BL116x16;
            let (lg, tg) =
                ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 1);
            let ri = ctxs.ref_idx.decode(dec, lg, tg);
            if ri >= num_ref_idx_l1_active {
                return Err(SliceDataError::Unsupported("ref_idx L1 overflow"));
            }
            motion.ref_idx_l1.push(ri as i32);
            let blks: Vec<usize> = (0..16).collect();
            if ri > 0 {
                for &b in &blks {
                    this_inter.l1_ref_gt0 |= 1 << b;
                }
            }
            let mvx = cabac_decode_mvd_component(
                dec,
                &mut ctxs.mvd_l1_x,
                inter_grid,
                &this_inter,
                left_idx,
                top_idx,
                0,
                0,
                16,
                16,
                1,
                0,
            )?;
            let mvy = cabac_decode_mvd_component(
                dec,
                &mut ctxs.mvd_l1_y,
                inter_grid,
                &this_inter,
                left_idx,
                top_idx,
                0,
                0,
                16,
                16,
                1,
                1,
            )?;
            motion.mvd_l1.push((mvx, mvy));
            this_inter.set_partition_l1(&blks, mvx, mvy, ri as i32);
        }
        3 => {
            mb.mb_type = MbType::BBi16x16;
            let blks: Vec<usize> = (0..16).collect();
            let (lg, tg) =
                ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 0);
            let ri0 = ctxs.ref_idx.decode(dec, lg, tg);
            if ri0 >= num_ref_idx_l0_active {
                return Err(SliceDataError::Unsupported("ref_idx L0 overflow"));
            }
            motion.ref_idx_l0.push(ri0 as i32);
            let (lg1, tg1) =
                ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 1);
            let ri1 = ctxs.ref_idx.decode(dec, lg1, tg1);
            if ri1 >= num_ref_idx_l1_active {
                return Err(SliceDataError::Unsupported("ref_idx L1 overflow"));
            }
            motion.ref_idx_l1.push(ri1 as i32);
            if ri0 > 0 {
                for &b in &blks {
                    this_inter.l0_ref_gt0 |= 1 << b;
                }
            }
            if ri1 > 0 {
                for &b in &blks {
                    this_inter.l1_ref_gt0 |= 1 << b;
                }
            }
            let mx0 = cabac_decode_mvd_component(
                dec,
                &mut ctxs.mvd_l0_x,
                inter_grid,
                &this_inter,
                left_idx,
                top_idx,
                0,
                0,
                16,
                16,
                0,
                0,
            )?;
            let my0 = cabac_decode_mvd_component(
                dec,
                &mut ctxs.mvd_l0_y,
                inter_grid,
                &this_inter,
                left_idx,
                top_idx,
                0,
                0,
                16,
                16,
                0,
                1,
            )?;
            motion.mvd_l0.push((mx0, my0));
            this_inter.set_partition_l0(&blks, mx0, my0, ri0 as i32);
            let mx1 = cabac_decode_mvd_component(
                dec,
                &mut ctxs.mvd_l1_x,
                inter_grid,
                &this_inter,
                left_idx,
                top_idx,
                0,
                0,
                16,
                16,
                1,
                0,
            )?;
            let my1 = cabac_decode_mvd_component(
                dec,
                &mut ctxs.mvd_l1_y,
                inter_grid,
                &this_inter,
                left_idx,
                top_idx,
                0,
                0,
                16,
                16,
                1,
                1,
            )?;
            motion.mvd_l1.push((mx1, my1));
            this_inter.set_partition_l1(&blks, mx1, my1, ri1 as i32);
        }
        4..=21 => {
            let idx = (b_type_raw - 4) as usize;
            let (is_16x8, dir0, dir1) = B_2PART_TABLE[idx];
            mb.mb_type = if is_16x8 {
                MbType::B16x8
            } else {
                MbType::B8x16
            };
            let dirs = [dir0, dir1];
            motion.pred_dirs = vec![dir0, dir1];
            motion.ref_idx_l0 = vec![crate::mv::LIST_NOT_USED; 2];
            motion.ref_idx_l1 = vec![crate::mv::LIST_NOT_USED; 2];

            // All L0 ref_idx first.
            for part in 0..2usize {
                let (c4, r4, w4, h4) = partition_dims(mb.mb_type, part);
                let (xp, yp, wp, hp) = (c4 as u32 * 4, r4 as u32 * 4, w4 as u32 * 4, h4 as u32 * 4);
                if dirs[part] == BPredDir::L0 || dirs[part] == BPredDir::Bi {
                    let (lg, tg) = ref_idx_gt0_neighbors(
                        inter_grid,
                        &this_inter,
                        left_idx,
                        top_idx,
                        xp,
                        yp,
                        wp,
                        hp,
                        0,
                    );
                    let ri = ctxs.ref_idx.decode(dec, lg, tg);
                    if ri >= num_ref_idx_l0_active {
                        return Err(SliceDataError::Unsupported("ref_idx L0 overflow"));
                    }
                    motion.ref_idx_l0[part] = ri as i32;
                    let blks = partition_blocks(c4, r4, w4, h4);
                    if ri > 0 {
                        for &b in &blks {
                            this_inter.l0_ref_gt0 |= 1 << b;
                        }
                    }
                }
            }
            // All L1 ref_idx.
            for part in 0..2usize {
                let (c4, r4, w4, h4) = partition_dims(mb.mb_type, part);
                let (xp, yp, wp, hp) = (c4 as u32 * 4, r4 as u32 * 4, w4 as u32 * 4, h4 as u32 * 4);
                if dirs[part] == BPredDir::L1 || dirs[part] == BPredDir::Bi {
                    let (lg, tg) = ref_idx_gt0_neighbors(
                        inter_grid,
                        &this_inter,
                        left_idx,
                        top_idx,
                        xp,
                        yp,
                        wp,
                        hp,
                        1,
                    );
                    let ri = ctxs.ref_idx.decode(dec, lg, tg);
                    if ri >= num_ref_idx_l1_active {
                        return Err(SliceDataError::Unsupported("ref_idx L1 overflow"));
                    }
                    motion.ref_idx_l1[part] = ri as i32;
                    let blks = partition_blocks(c4, r4, w4, h4);
                    if ri > 0 {
                        for &b in &blks {
                            this_inter.l1_ref_gt0 |= 1 << b;
                        }
                    }
                }
            }
            // All L0 MVDs.
            for part in 0..2usize {
                let (c4, r4, w4, h4) = partition_dims(mb.mb_type, part);
                let (xp, yp, wp, hp) = (c4 as u32 * 4, r4 as u32 * 4, w4 as u32 * 4, h4 as u32 * 4);
                if dirs[part] == BPredDir::L0 || dirs[part] == BPredDir::Bi {
                    let mvx = cabac_decode_mvd_component(
                        dec,
                        &mut ctxs.mvd_l0_x,
                        inter_grid,
                        &this_inter,
                        left_idx,
                        top_idx,
                        xp,
                        yp,
                        wp,
                        hp,
                        0,
                        0,
                    )?;
                    let mvy = cabac_decode_mvd_component(
                        dec,
                        &mut ctxs.mvd_l0_y,
                        inter_grid,
                        &this_inter,
                        left_idx,
                        top_idx,
                        xp,
                        yp,
                        wp,
                        hp,
                        0,
                        1,
                    )?;
                    motion.mvd_l0.push((mvx, mvy));
                    let blks = partition_blocks(c4, r4, w4, h4);
                    this_inter.set_partition_l0(&blks, mvx, mvy, motion.ref_idx_l0[part]);
                }
            }
            // All L1 MVDs.
            for part in 0..2usize {
                let (c4, r4, w4, h4) = partition_dims(mb.mb_type, part);
                let (xp, yp, wp, hp) = (c4 as u32 * 4, r4 as u32 * 4, w4 as u32 * 4, h4 as u32 * 4);
                if dirs[part] == BPredDir::L1 || dirs[part] == BPredDir::Bi {
                    let mvx = cabac_decode_mvd_component(
                        dec,
                        &mut ctxs.mvd_l1_x,
                        inter_grid,
                        &this_inter,
                        left_idx,
                        top_idx,
                        xp,
                        yp,
                        wp,
                        hp,
                        1,
                        0,
                    )?;
                    let mvy = cabac_decode_mvd_component(
                        dec,
                        &mut ctxs.mvd_l1_y,
                        inter_grid,
                        &this_inter,
                        left_idx,
                        top_idx,
                        xp,
                        yp,
                        wp,
                        hp,
                        1,
                        1,
                    )?;
                    motion.mvd_l1.push((mvx, mvy));
                    let blks = partition_blocks(c4, r4, w4, h4);
                    this_inter.set_partition_l1(&blks, mvx, mvy, motion.ref_idx_l1[part]);
                }
            }
            mb.motion = Some(motion);
        }
        22 => {
            // B_8x8
            mb.mb_type = MbType::BB8x8;
            let mut sub_types = [0u8; 4];
            for st in &mut sub_types {
                *st = ctxs.sub_mb_b.decode(dec) as u8;
            }
            motion.sub_mb_type_b = Some(sub_types);
            motion.ref_idx_l0 = vec![crate::mv::LIST_NOT_USED; 4];
            motion.ref_idx_l1 = vec![crate::mv::LIST_NOT_USED; 4];
            let sub_dirs: Vec<BPredDir> = sub_types
                .iter()
                .map(|&st| {
                    if (st as usize) < 13 {
                        crate::mv::B_SUB_MB_DIR[st as usize]
                    } else {
                        BPredDir::Direct
                    }
                })
                .collect();
            motion.pred_dirs = sub_dirs.clone();

            for part in 0..4 {
                let (c4, r4, _, _) = partition_dims(mb.mb_type, part);
                let (xp, yp, wp, hp) = (c4 as u32 * 4, r4 as u32 * 4, 8u32, 8u32);
                let blks = partition_blocks(c4, r4, 2, 2);
                if sub_dirs[part] != BPredDir::Direct
                    && (sub_dirs[part] == BPredDir::L0 || sub_dirs[part] == BPredDir::Bi)
                {
                    let (lg, tg) = ref_idx_gt0_neighbors(
                        inter_grid,
                        &this_inter,
                        left_idx,
                        top_idx,
                        xp,
                        yp,
                        wp,
                        hp,
                        0,
                    );
                    let ri = ctxs.ref_idx.decode(dec, lg, tg);
                    if ri >= num_ref_idx_l0_active {
                        return Err(SliceDataError::Unsupported("ref_idx L0 overflow"));
                    }
                    motion.ref_idx_l0[part] = ri as i32;
                    if ri > 0 {
                        for &b in &blks {
                            this_inter.l0_ref_gt0 |= 1 << b;
                        }
                    }
                }
                if sub_dirs[part] != BPredDir::Direct
                    && (sub_dirs[part] == BPredDir::L1 || sub_dirs[part] == BPredDir::Bi)
                {
                    let (lg, tg) = ref_idx_gt0_neighbors(
                        inter_grid,
                        &this_inter,
                        left_idx,
                        top_idx,
                        xp,
                        yp,
                        wp,
                        hp,
                        1,
                    );
                    let ri = ctxs.ref_idx.decode(dec, lg, tg);
                    if ri >= num_ref_idx_l1_active {
                        return Err(SliceDataError::Unsupported("ref_idx L1 overflow"));
                    }
                    motion.ref_idx_l1[part] = ri as i32;
                    if ri > 0 {
                        for &b in &blks {
                            this_inter.l1_ref_gt0 |= 1 << b;
                        }
                    }
                }
            }
            for part in 0..4 {
                let (c4, r4, _, _) = partition_dims(mb.mb_type, part);
                let (xp, yp, wp, hp) = (c4 as u32 * 4, r4 as u32 * 4, 8u32, 8u32);
                let blks = partition_blocks(c4, r4, 2, 2);
                if sub_dirs[part] == BPredDir::Direct {
                    continue;
                }
                if sub_dirs[part] == BPredDir::L0 || sub_dirs[part] == BPredDir::Bi {
                    let n = crate::mv::B_SUB_MB_PARTS[sub_types[part] as usize];
                    for _ in 0..n {
                        let mvx = cabac_decode_mvd_component(
                            dec,
                            &mut ctxs.mvd_l0_x,
                            inter_grid,
                            &this_inter,
                            left_idx,
                            top_idx,
                            xp,
                            yp,
                            wp,
                            hp,
                            0,
                            0,
                        )?;
                        let mvy = cabac_decode_mvd_component(
                            dec,
                            &mut ctxs.mvd_l0_y,
                            inter_grid,
                            &this_inter,
                            left_idx,
                            top_idx,
                            xp,
                            yp,
                            wp,
                            hp,
                            0,
                            1,
                        )?;
                        motion.mvd_l0.push((mvx, mvy));
                        this_inter.set_partition_l0(&blks, mvx, mvy, motion.ref_idx_l0[part]);
                    }
                }
                if sub_dirs[part] == BPredDir::L1 || sub_dirs[part] == BPredDir::Bi {
                    let n = crate::mv::B_SUB_MB_PARTS[sub_types[part] as usize];
                    for _ in 0..n {
                        let mvx = cabac_decode_mvd_component(
                            dec,
                            &mut ctxs.mvd_l1_x,
                            inter_grid,
                            &this_inter,
                            left_idx,
                            top_idx,
                            xp,
                            yp,
                            wp,
                            hp,
                            1,
                            0,
                        )?;
                        let mvy = cabac_decode_mvd_component(
                            dec,
                            &mut ctxs.mvd_l1_y,
                            inter_grid,
                            &this_inter,
                            left_idx,
                            top_idx,
                            xp,
                            yp,
                            wp,
                            hp,
                            1,
                            1,
                        )?;
                        motion.mvd_l1.push((mvx, mvy));
                        this_inter.set_partition_l1(&blks, mvx, mvy, motion.ref_idx_l1[part]);
                    }
                }
            }
            mb.motion = Some(motion);
        }
        _ => unreachable!(),
    }

    if b_type_raw != 0 {
        let (cbp_l, cbp_c) =
            decode_inter_cbp_cabac(dec, ctxs, cabac_ctx_grid, mb_x, mb_y, mb_cols)?;
        let cbp = cbp_l | (cbp_c << 4);
        mb.cbp = cbp;
        let mut qp = prev_qp;
        let mut dqp_nz = false;
        if cbp_l != 0 || cbp_c != 0 {
            let dqp = ctxs.qp_delta.decode(dec, prev_dqp_nonzero);
            dqp_nz = dqp != 0;
            qp = (prev_qp + dqp + 52).rem_euclid(52);
        }
        mb.qp = qp;
        let this_cabac_ctx = decode_inter_residual_cabac(
            dec,
            ctxs,
            &mut mb,
            &mut this_nz,
            nz_grid,
            cabac_ctx_grid,
            mb_x,
            mb_y,
            mb_cols,
            cbp_l,
            cbp_c,
        )?;
        let mb_type_str = format!("{:?}", mb.mb_type);
        tracer.on_mb_parsed(mb_x, mb_y, &mb_type_str, mb.qp, mb.cbp, 0, &[0u8; 16]);
        return Ok((
            mb,
            this_nz,
            this_pred_ctx,
            this_cabac_ctx,
            this_inter,
            qp,
            dqp_nz,
        ));
    }

    // B_Direct: no CBP/residual syntax, qp unchanged.
    mb.qp = prev_qp;
    let this_cabac_ctx = MbCabacCtx {
        present: true,
        cbp_word: 0,
        ..Default::default()
    };
    tracer.on_mb_parsed(mb_x, mb_y, "BDirect16x16", mb.qp, 0, 0, &[0u8; 16]);
    Ok((
        mb,
        this_nz,
        this_pred_ctx,
        this_cabac_ctx,
        this_inter,
        prev_qp,
        prev_dqp_nonzero,
    ))
}

/// Shared intra-macroblock decode path for intra MBs embedded in P/B slices.
/// `intra_t` is the raw I-slice mb_type index (0..=24, 25=I_PCM) returned by
/// `IntraMbTypeSuffixCabacContext::decode`.
#[allow(clippy::too_many_arguments)]
fn parse_intra_mb_cabac_pb<T: crate::trace::DecodeTracer>(
    dec: &mut crate::entropy::CabacDecoder,
    ctxs: &mut PbCabacSliceContexts,
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nz_grid: &[MbNz],
    pred_ctx_grid: &[MbPredCtx],
    cabac_ctx_grid: &[MbCabacCtx],
    prev_qp: i32,
    prev_dqp_nonzero: bool,
    intra_t: u32,
    transform_8x8_mode_flag: bool,
    tracer: &mut T,
) -> R<(Macroblock, MbNz, MbPredCtx, MbCabacCtx, i32, bool)> {
    if intra_t == 25 {
        return Err(SliceDataError::Unsupported(
            "I_PCM in P/B CABAC not supported",
        ));
    }
    // Reuse the I-slice CABAC logic by building a temporary CabacSliceContexts
    // from the PB contexts (all PB contexts share the same context variables).
    // We need to call parse_intra_macroblock_cabac but it takes CabacSliceContexts.
    // Instead, replicate the intra decode inline using ctxs' PB variants.
    let left_idx = (mb_x > 0).then(|| (mb_y * mb_cols + mb_x - 1) as usize);
    let top_idx = (mb_y > 0).then(|| ((mb_y - 1) * mb_cols + mb_x) as usize);

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

    let (is_i16x16, i16_mode, cbp_chroma_mbtype, cbp_luma_mbtype) = if intra_t == 0 {
        (false, 0u8, 0u8, 0u8)
    } else if (1..=24).contains(&intra_t) {
        let (m, cc, cl) = I16X16_TABLE[(intra_t - 1) as usize];
        (true, m, cc, cl)
    } else {
        return Err(SliceDataError::Unsupported(
            "mb_type out of range in P/B intra",
        ));
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

        let mut modes = [crate::prediction::Intra4x4Mode::Dc; 16];
        if is_8x8 {
            let mut modes8 = [0u8; 4];
            for i8 in 0..4usize {
                let pred_mode = mpm_pred_mode_8x8(
                    pred_ctx_grid,
                    mb_x,
                    mb_y,
                    mb_cols,
                    &modes,
                    i8,
                    NeighbourCtx::NONE,
                );
                let final_mode = ctxs.intra4x4.decode(dec, pred_mode);
                modes8[i8] = final_mode;
                for sub in 0..4usize {
                    modes[raster_of_8x8_sub(i8, sub)] =
                        crate::prediction::Intra4x4Mode::from_u8(final_mode);
                }
            }
            mb.pred_modes_8x8 = Box::new(modes8);
        } else {
            for blk_idx in 0..16usize {
                let raster = raster_of_8x8_sub(blk_idx / 4, blk_idx % 4);
                let pred_mode = mpm_pred_mode(
                    pred_ctx_grid,
                    mb_x,
                    mb_y,
                    mb_cols,
                    &modes,
                    raster,
                    NeighbourCtx::NONE,
                );
                let final_mode = ctxs.intra4x4.decode(dec, pred_mode);
                modes[raster] = crate::prediction::Intra4x4Mode::from_u8(final_mode);
            }
        }
        mb.pred_modes_4x4 = Box::new(modes);
        this_pred_ctx.is_intra4x4 = true;
        this_pred_ctx.modes = modes;
    }

    let left_chroma_nz = left_idx
        .map(|i| cabac_ctx_grid[i].chroma_pred_mode != 0)
        .unwrap_or(false);
    let top_chroma_nz = top_idx
        .map(|i| cabac_ctx_grid[i].chroma_pred_mode != 0)
        .unwrap_or(false);
    let chroma_pred = ctxs.chroma_pred.decode(dec, left_chroma_nz, top_chroma_nz);
    mb.intra_chroma_pred_mode = chroma_pred as u8;
    this_cabac_ctx.chroma_pred_mode = chroma_pred as u8;

    let (cbp_l, cbp_c) = if is_i16x16 {
        (cbp_luma_mbtype, cbp_chroma_mbtype)
    } else {
        let (left_cbp, top_cbp) =
            cabac_cbp_neighbors(cabac_ctx_grid, mb_x, mb_y, mb_cols, NeighbourCtx::NONE);
        let (l, c) = ctxs.cbp.decode(dec, left_cbp, top_cbp);
        mb.cbp = l | (c << 4);
        (l, c)
    };

    let mut qp = prev_qp;
    let mut dqp_nz = false;
    if cbp_l != 0 || cbp_c != 0 || is_i16x16 {
        let dqp = ctxs.qp_delta.decode(dec, prev_dqp_nonzero);
        dqp_nz = dqp != 0;
        qp = (prev_qp + dqp + 52).rem_euclid(52);
    }
    mb.qp = qp;

    // Residual (reuse same CABAC decode helpers as I-slice).
    use crate::cabac_tables::{
        CAT_CHROMA_AC, CAT_CHROMA_DC, CAT_LUMA_4X4, CAT_LUMA_AC, CAT_LUMA_DC,
    };
    let mut cbp_word: u16 = mb.cbp as u16;

    if is_i16x16 {
        let left_coded = dc_cbf_neighbor(cabac_ctx_grid, left_idx, 0x100);
        let top_coded = dc_cbf_neighbor(cabac_ctx_grid, top_idx, 0x100);
        if ctxs.cbf.decode(dec, CAT_LUMA_DC, left_coded, top_coded) {
            let (coeffs, _count) = ctxs.residual.decode_block(dec, CAT_LUMA_DC, 16);
            mb.luma_dc = coeffs;
            cbp_word |= 0x100;
        }
    }
    if is_8x8 {
        for blk8 in 0..4usize {
            if (cbp_l >> blk8) & 1 == 0 {
                continue;
            }
            let (coeffs, count) = ctxs.residual.decode_block_8x8(dec);
            mb.luma_coeffs_8x8[blk8] = coeffs;
            for sub in 0..4usize {
                this_nz.luma[raster_of_8x8_sub(blk8, sub)] = count;
            }
        }
    } else {
        let luma_max = if is_i16x16 { 15 } else { 16 };
        let luma_cat = if is_i16x16 { CAT_LUMA_AC } else { CAT_LUMA_4X4 };
        let blocks: Vec<usize> = {
            let mut v = Vec::with_capacity(16);
            for blk8 in 0..4 {
                if (cbp_l >> blk8) & 1 == 0 {
                    continue;
                }
                for sub in 0..4 {
                    v.push(raster_of_8x8_sub(blk8, sub));
                }
            }
            v
        };
        for block in blocks {
            let (left_coded, top_coded) = luma_cbf_neighbors(
                nz_grid,
                mb_x,
                mb_y,
                mb_cols,
                &this_nz,
                block,
                true,
                NeighbourCtx::NONE,
            );
            if ctxs.cbf.decode(dec, luma_cat, left_coded, top_coded) {
                let (coeffs, count) = ctxs.residual.decode_block(dec, luma_cat, luma_max);
                this_nz.luma[block] = count;
                if is_i16x16 {
                    let mut s = [0i16; 16];
                    s[1..16].copy_from_slice(&coeffs[0..15]);
                    mb.luma_coeffs[block] = s;
                } else {
                    mb.luma_coeffs[block] = coeffs;
                }
            }
        }
    }
    if cbp_c != 0 {
        for comp in 0..2 {
            let bit = 0x40u16 << comp;
            let left_coded = dc_cbf_neighbor(cabac_ctx_grid, left_idx, bit);
            let top_coded = dc_cbf_neighbor(cabac_ctx_grid, top_idx, bit);
            if ctxs.cbf.decode(dec, CAT_CHROMA_DC, left_coded, top_coded) {
                let (coeffs, _) = ctxs.residual.decode_block(dec, CAT_CHROMA_DC, 4);
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
        for comp in 0..2 {
            for block in 0..4 {
                let (left_coded, top_coded) = chroma_cbf_neighbors(
                    nz_grid,
                    mb_x,
                    mb_y,
                    mb_cols,
                    &this_nz,
                    comp,
                    block,
                    true,
                    NeighbourCtx::NONE,
                );
                if ctxs.cbf.decode(dec, CAT_CHROMA_AC, left_coded, top_coded) {
                    let (coeffs, count) = ctxs.residual.decode_block(dec, CAT_CHROMA_AC, 15);
                    this_nz.chroma[comp * 4 + block] = count;
                    let mut s = [0i16; 16];
                    s[1..16].copy_from_slice(&coeffs[0..15]);
                    if comp == 0 {
                        mb.chroma_cb_coeffs[block] = s;
                    } else {
                        mb.chroma_cr_coeffs[block] = s;
                    }
                }
            }
        }
    }
    this_cabac_ctx.cbp_word = cbp_word;
    let mb_type_str = if is_i16x16 {
        "Intra16x16".to_string()
    } else {
        "Intra4x4".to_string()
    };
    tracer.on_mb_parsed(
        mb_x,
        mb_y,
        &mb_type_str,
        mb.qp,
        mb.cbp,
        mb.intra_chroma_pred_mode,
        &[0u8; 16],
    );
    Ok((mb, this_nz, this_pred_ctx, this_cabac_ctx, qp, dqp_nz))
}

/// Decode `coded_block_pattern` for an inter macroblock using CABAC
/// (§9.3.3.1.1.4). Reuses `CbpCabacContext::decode` with inter-MB sentinel.
fn decode_inter_cbp_cabac(
    dec: &mut crate::entropy::CabacDecoder,
    ctxs: &mut PbCabacSliceContexts,
    cabac_ctx_grid: &[MbCabacCtx],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
) -> R<(u8, u8)> {
    // For inter MBs, off-picture neighbours use 0x00F (not 0x7CF) per FFmpeg.
    let left = if mb_x > 0 {
        cabac_ctx_grid[((mb_y * mb_cols) + mb_x - 1) as usize].cbp_word
    } else {
        0x00F
    };
    let top = if mb_y > 0 {
        cabac_ctx_grid[(((mb_y - 1) * mb_cols) + mb_x) as usize].cbp_word
    } else {
        0x00F
    };
    Ok(ctxs.cbp.decode(dec, left, top))
}

/// Decode inter-macroblock residual via CABAC and return the updated `MbCabacCtx`.
#[allow(clippy::too_many_arguments)]
fn decode_inter_residual_cabac(
    dec: &mut crate::entropy::CabacDecoder,
    ctxs: &mut PbCabacSliceContexts,
    mb: &mut Macroblock,
    this_nz: &mut MbNz,
    nz_grid: &[MbNz],
    cabac_ctx_grid: &[MbCabacCtx],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    cbp_l: u8,
    cbp_c: u8,
) -> R<MbCabacCtx> {
    use crate::cabac_tables::{CAT_CHROMA_AC, CAT_CHROMA_DC, CAT_LUMA_4X4};
    let left_idx = (mb_x > 0).then(|| (mb_y * mb_cols + mb_x - 1) as usize);
    let top_idx = (mb_y > 0).then(|| ((mb_y - 1) * mb_cols + mb_x) as usize);
    let mut cbp_word: u16 = mb.cbp as u16;

    let blocks: Vec<usize> = {
        let mut v = Vec::with_capacity(16);
        for blk8 in 0..4 {
            if (cbp_l >> blk8) & 1 == 0 {
                continue;
            }
            for sub in 0..4 {
                v.push(raster_of_8x8_sub(blk8, sub));
            }
        }
        v
    };
    for block in blocks {
        let (left_coded, top_coded) = luma_cbf_neighbors(
            nz_grid,
            mb_x,
            mb_y,
            mb_cols,
            this_nz,
            block,
            false,
            NeighbourCtx::NONE,
        );
        let (lr, lo) = dec.debug_state();
        let has_coeff = ctxs.cbf.decode(dec, CAT_LUMA_4X4, left_coded, top_coded);
        let (ar, ao) = dec.debug_state();
        eprintln!("    luma blk={block} left={left_coded} top={top_coded} cbf={has_coeff} {lr:#06x}/{lo:#010x}->{ar:#06x}/{ao:#010x}");
        if has_coeff {
            let (coeffs, count) = ctxs.residual.decode_block(dec, CAT_LUMA_4X4, 16);
            let (cr, co) = dec.debug_state();
            eprintln!("    luma blk={block} nz={count} coeffs={:?} after={cr:#06x}/{co:#010x}", &coeffs[..16]);
            this_nz.luma[block] = count;
            mb.luma_coeffs[block] = coeffs;
        }
    }
    if cbp_c != 0 {
        for comp in 0..2 {
            let bit = 0x40u16 << comp;
            let left_coded = dc_cbf_neighbor(cabac_ctx_grid, left_idx, bit);
            let top_coded = dc_cbf_neighbor(cabac_ctx_grid, top_idx, bit);
            if ctxs.cbf.decode(dec, CAT_CHROMA_DC, left_coded, top_coded) {
                let (coeffs, _) = ctxs.residual.decode_block(dec, CAT_CHROMA_DC, 4);
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
        for comp in 0..2 {
            for block in 0..4 {
                let (left_coded, top_coded) = chroma_cbf_neighbors(
                    nz_grid,
                    mb_x,
                    mb_y,
                    mb_cols,
                    this_nz,
                    comp,
                    block,
                    false,
                    NeighbourCtx::NONE,
                );
                if ctxs.cbf.decode(dec, CAT_CHROMA_AC, left_coded, top_coded) {
                    let (coeffs, count) = ctxs.residual.decode_block(dec, CAT_CHROMA_AC, 15);
                    this_nz.chroma[comp * 4 + block] = count;
                    let mut s = [0i16; 16];
                    s[1..16].copy_from_slice(&coeffs[0..15]);
                    if comp == 0 {
                        mb.chroma_cb_coeffs[block] = s;
                    } else {
                        mb.chroma_cr_coeffs[block] = s;
                    }
                }
            }
        }
    }
    Ok(MbCabacCtx {
        present: true,
        cbp_word,
        ..Default::default()
    })
}
