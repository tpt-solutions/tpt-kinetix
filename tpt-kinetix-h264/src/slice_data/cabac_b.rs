use super::*;

/// Sub-partition (col4, row4, w4, h4) for sub_i-th sub-partition within a
/// P_8x8 8×8 partition at (col4, row4) with P sub_mb_type `sub_t` (0..=3).
/// Used to derive correct per-sub amvd_sum neighbors (§9.3.3.1.1.7) and to
/// fill l0_mvd_abs for only the sub-partition's 4×4 blocks.
fn p8x8_sub_dims(
    sub_t: usize,
    col4: usize,
    row4: usize,
    sub_i: usize,
) -> (usize, usize, usize, usize) {
    match (sub_t, sub_i) {
        (0, 0) => (col4, row4, 2, 2),         // P_8x8: one 8×8
        (1, 0) => (col4, row4, 2, 1),         // P_8x4: top 8×4
        (1, 1) => (col4, row4 + 1, 2, 1),     // P_8x4: bottom 8×4
        (2, 0) => (col4, row4, 1, 2),         // P_4x8: left 4×8
        (2, 1) => (col4 + 1, row4, 1, 2),     // P_4x8: right 4×8
        (3, 0) => (col4, row4, 1, 1),         // P_4x4: top-left
        (3, 1) => (col4 + 1, row4, 1, 1),     // P_4x4: top-right
        (3, 2) => (col4, row4 + 1, 1, 1),     // P_4x4: bottom-left
        (3, 3) => (col4 + 1, row4 + 1, 1, 1), // P_4x4: bottom-right
        _ => unreachable!("p8x8_sub_dims: sub_t={sub_t} sub_i={sub_i}"),
    }
}

/// Sub-partition (col4, row4, w4, h4) for sub_i-th sub-partition within a
/// B_8x8 8×8 partition at (col4, row4) with B sub_mb_type `sub_t` (0..=12),
/// per spec Table 7-17: type 0 is B_Direct_8x8 (handled by the caller); types
/// `{1,5,9}` are one 8×8; `{2,6,10}` two 8×4 (top/bottom); `{3,7,11}` two 4×8
/// (left/right); `{4,8,12}` four 4×4 in scan order.
fn b8x8_sub_dims(
    sub_t: usize,
    col4: usize,
    row4: usize,
    sub_i: usize,
) -> (usize, usize, usize, usize) {
    // spec Table 7-18 order: {1,2,3}=8×8, {4,6,8}=8×4, {5,7,9}=4×8, {10,11,12}=4×4
    match sub_t {
        0..=3 => (col4, row4, 2, 2), // one 8×8 sub-part (0 = B_Direct_8x8)
        4 | 6 | 8 => match sub_i {
            // two 8×4
            0 => (col4, row4, 2, 1),
            _ => (col4, row4 + 1, 2, 1),
        },
        5 | 7 | 9 => match sub_i {
            // two 4×8
            0 => (col4, row4, 1, 2),
            _ => (col4 + 1, row4, 1, 2),
        },
        10..=12 => match sub_i {
            // four 4×4
            0 => (col4, row4, 1, 1),
            1 => (col4 + 1, row4, 1, 1),
            2 => (col4, row4 + 1, 1, 1),
            _ => (col4 + 1, row4 + 1, 1, 1),
        },
        _ => unreachable!("b8x8_sub_dims: sub_t={sub_t}"),
    }
}

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
    _direct_8x8_inference_flag: bool,
    nctx: NeighbourCtx,
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
    let (left_idx, top_idx) = nctx.left_top(mb_x, mb_y, mb_cols);

    let (r_pre_t, o_pre_t) = dec.debug_state();
    let inter_type = ctxs.mb_type_p.decode(dec);
    // ctxIdx 17 is one physical context in FFmpeg (partition bit here, bin 0
    // of the intra-in-P suffix there) -- keep both copies adapted identically.
    ctxs.sync_shared_mb_type_ctx_prefix_to_suffix_p();
    let (r_post_t, o_post_t) = dec.debug_state();
    if std::env::var("KINETIX_BINTRACE").is_ok() {
        eprintln!("  MB({mb_x},{mb_y}) mb_type={inter_type:?} cabac: {r_pre_t:#06x}/{o_pre_t:#010x} -> {r_post_t:#06x}/{o_post_t:#010x}");
    }
    match inter_type {
        None => {
            // Intra macroblock inside P slice.
            let intra_t = ctxs.intra_suffix.decode(dec);
            // Reverse sync of shared ctxIdx 17 (see above).
            ctxs.sync_shared_mb_type_ctx_suffix_to_prefix_p();
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
                    nctx,
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
                if std::env::var("KINETIX_BINTRACE").is_ok() {
                    eprintln!("  P8x8 MB({mb_x},{mb_y}) sub_types={sub_types:?}");
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
                    if ri > 0 && std::env::var("KINETIX_BINTRACE").is_ok() {
                        eprintln!("REFIDX_GT0 mb=({mb_x},{mb_y}) P_8x8 part={part} ri={ri}");
                    }
                    // fill 2×2 blocks in the 8×8 quadrant with ref_idx
                    let blks = partition_blocks(col4, row4, 2, 2);
                    if ri > 0 {
                        for &b in &blks {
                            this_inter.l0_ref_gt0 |= 1 << b;
                        }
                    }
                }
                // MVDs per sub-partition. For each 8×8 partition the sub-type
                // may split it into 8×4, 4×8, or 4×4 sub-partitions. Each
                // sub-partition gets its own (xp,yp,wp,hp) for amvd_sum context
                // derivation (spec §9.3.3.1.1.7), and l0_mvd_abs is stored for
                // all 4×4 blocks in the sub-partition so later partitions see
                // the correct intra-MB neighbor MVD magnitudes.
                for part in 0..4 {
                    let sub_t = sub_types[part] as usize;
                    let n_sub = P_SUB_MB_PARTS[sub_t];
                    let (col4, row4, _, _) = partition_dims(mb_type, part);
                    for sub_i in 0..n_sub {
                        let (sc4, sr4, sw4, sh4) = p8x8_sub_dims(sub_t, col4, row4, sub_i);
                        let (xp, yp, wp, hp) = (
                            sc4 as u32 * 4,
                            sr4 as u32 * 4,
                            sw4 as u32 * 4,
                            sh4 as u32 * 4,
                        );
                        let (r0, o0) = dec.debug_state();
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
                        let (r1, o1) = dec.debug_state();
                        if std::env::var("KINETIX_BINTRACE").is_ok() {
                            eprintln!("    part={part} sub={sub_i} sub_t={sub_t} xp={xp} yp={yp} wp={wp} hp={hp} mvd=({mvd_x},{mvd_y}) cabac: {r0:#06x}/{o0:#010x} -> {r1:#06x}/{o1:#010x}");
                        }
                        motion.mvd_l0.push((mvd_x, mvd_y));
                        let sub_blks = partition_blocks(sc4, sr4, sw4, sh4);
                        this_inter.set_partition_l0(
                            &sub_blks,
                            mvd_x,
                            mvd_y,
                            motion.ref_idx_l0[part],
                        );
                    }
                }
            } else {
                // 16×16, 16×8, or 8×16.
                // §7.3.5.1 `mb_pred`: all `ref_idx_l0` are signalled first, then
                // all `mvd_l0` — not interleaved per partition (matters once
                // `num_ref_idx_l0_active > 1`, where `ref_idx_l0` consumes bins;
                // interleaving desyncs the CABAC engine).
                let mut part_geom = [(0u32, 0u32, 0u32, 0u32); 2];
                let mut part_blks: [Vec<usize>; 2] = [Vec::new(), Vec::new()];
                for part in 0..n_parts {
                    let (col4, row4, w4, h4) = partition_dims(mb_type, part);
                    let (xp, yp, wp, hp) = (
                        col4 as u32 * 4,
                        row4 as u32 * 4,
                        w4 as u32 * 4,
                        h4 as u32 * 4,
                    );
                    part_geom[part] = (xp, yp, wp, hp);
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
                    if ri > 0 && std::env::var("KINETIX_BINTRACE").is_ok() {
                        eprintln!("REFIDX_GT0 mb=({mb_x},{mb_y}) part={part} ri={ri}");
                    }
                    let blks = partition_blocks(col4, row4, w4, h4);
                    if ri > 0 {
                        for &b in &blks {
                            this_inter.l0_ref_gt0 |= 1 << b;
                        }
                    }
                    part_blks[part] = blks;
                }
                for part in 0..n_parts {
                    let (xp, yp, wp, hp) = part_geom[part];
                    let ri = motion.ref_idx_l0[part];
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
                    if std::env::var("KINETIX_BINTRACE").is_ok() {
                        eprintln!(
                            "  MB({mb_x},{mb_y}) mvd_l0[{}]=({mvd_x},{mvd_y}) ri={ri}",
                            motion.mvd_l0.len()
                        );
                    }
                    motion.mvd_l0.push((mvd_x, mvd_y));
                    this_inter.set_partition_l0(&part_blks[part], mvd_x, mvd_y, ri);
                }
            }

            mb.motion = Some(motion);
            let (s_r, s_o) = dec.debug_state();
            let (cbp_l, cbp_c) =
                decode_inter_cbp_cabac(dec, ctxs, cabac_ctx_grid, mb_x, mb_y, mb_cols, nctx)?;
            let cbp = cbp_l | (cbp_c << 4);
            mb.cbp = cbp;
            let (e_r, e_o) = dec.debug_state();
            if std::env::var("KINETIX_BINTRACE").is_ok() {
                eprintln!("  MB({mb_x},{mb_y}) inter cbp={cbp:#04x}(l={cbp_l:#x} c={cbp_c}) after_mvd={s_r:#06x}/{s_o:#010x} after_cbp={e_r:#06x}/{e_o:#010x}");
            }
            // `transform_size_8x8_flag` (§7.3.5.1, §9.3.3.1.1.10, ctxIdxOffset
            // 399): present AFTER `coded_block_pattern` and BEFORE `mb_qp_delta`
            // when `transform_8x8_mode_flag && CodedBlockPatternLuma > 0`. Inter
            // MBs are never Intra_16×16, so no extra guard. Omitting it desyncs
            // every subsequent MB (the flag's absence silently consumes the
            // first bit of mb_qp_delta / the next MB) — the CABAC twin of the
            // CAVLC bug fixed in #32j.
            //
            // FFmpeg additionally gates this on `get_dct8x8_allowed` (line
            // 2347 of `h264_cabac_ref.c`). For P_L0_16x16 / P_16x8 / P_8x16
            // (`shape` 0/1/2) `dct8x8_allowed` is NEVER narrowed — it stays
            // `= transform_8x8_mode`, i.e. the flag IS read (ffmpeg only
            // narrows it inside the `IS_8X8` branch, line 2161). For P_8x8
            // (`shape` 3) `get_dct8x8_allowed` returns true iff no sub-
            // partition is smaller than 8×8: with `direct_8x8_inference_flag`
            // set, all four `sub_mb_type` raw values must be 0 (P_L0_8x8);
            // without it, raw 3 (P_L0_4x4 → `MB_TYPE_8x8`, no 16x8/8x16 bit)
            // is also permitted.
            let dct8x8_allowed = if shape == 3 {
                let subs = mb
                    .motion
                    .as_ref()
                    .and_then(|m| m.sub_mb_type)
                    .unwrap_or([0u8; 4]);
                if _direct_8x8_inference_flag {
                    subs.iter().all(|&s| s == 0)
                } else {
                    subs.iter().all(|&s| s == 0 || s == 3)
                }
            } else {
                true
            };
            let mut is_8x8 = false;
            if transform_8x8_mode_flag && cbp_l != 0 && dct8x8_allowed {
                let left_8x8 = left_idx
                    .map(|i| cabac_ctx_grid[i].transform_8x8)
                    .unwrap_or(false);
                let top_8x8 = top_idx
                    .map(|i| cabac_ctx_grid[i].transform_8x8)
                    .unwrap_or(false);
                is_8x8 = ctxs.transform_8x8.decode(dec, left_8x8, top_8x8);
            }
            mb.transform_size_8x8 = is_8x8;
            if std::env::var("KINETIX_BINTRACE").is_ok() {
                eprintln!("  MB({mb_x},{mb_y}) inter t8={is_8x8}");
            }
            let mut qp = prev_qp;
            let mut dqp_nz = false;
            if cbp_l != 0 || cbp_c != 0 {
                let (r0, o0) = dec.debug_state();
                let dqp = ctxs.qp_delta.decode(dec, prev_dqp_nonzero);
                dqp_nz = dqp != 0;
                qp = (prev_qp + dqp + 52).rem_euclid(52);
                let (r1, o1) = dec.debug_state();
                if std::env::var("KINETIX_BINTRACE").is_ok() {
                    eprintln!("  MB({mb_x},{mb_y}) dqp={dqp} qp={qp} after_qpdelta={r1:#06x}/{o1:#010x}  (before={r0:#06x}/{o0:#010x})");
                }
            }
            mb.qp = qp;

            let (r_pre_res, o_pre_res) = dec.debug_state();
            let mut this_cabac_ctx = decode_inter_residual_cabac(
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
                is_8x8,
                nctx,
            )?;
            this_cabac_ctx.transform_8x8 = is_8x8;
            let (r_post_res, o_post_res) = dec.debug_state();
            if std::env::var("KINETIX_BINTRACE").is_ok() {
                eprintln!("  MB({mb_x},{mb_y}) residual: before={r_pre_res:#06x}/{o_pre_res:#010x} after={r_post_res:#06x}/{o_post_res:#010x}");
            }

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
    mb_aff: bool,
    field_pic_flag: bool,
    cabac_init_idc: usize,
    num_ref_idx_l0_active: u32,
    num_ref_idx_l1_active: u32,
    chroma_qp_index_offset: i32,
    transform_8x8_mode_flag: bool,
    direct_8x8_inference_flag: bool,
    colocated_mv: Option<&[[crate::mv::MvCell; 16]]>,
    direct_spatial_mv_pred_flag: bool,
    temporal: Option<&crate::mv::TemporalDirectCtx>,
    tracer: &mut T,
) -> R<ParsedSlice> {
    let mut dec = crate::entropy::CabacDecoder::new(data)
        .map_err(|_| SliceDataError::Eof("CABAC engine init"))?;
    let mut ctxs = PbCabacSliceContexts::new_b(slice_qp, cabac_init_idc);

    let total = (mb_cols * mb_rows) as usize;
    // Assigned by frame-MB grid address, not decode order — see the same note
    // in `cabac_p.rs::parse_p_slice_cabac` (MBAFF pair scan).
    let mut macroblocks: Vec<Macroblock> = (0..total).map(|_| Macroblock::new_skip()).collect();
    let mut nz: Vec<MbNz> = vec![MbNz::default(); total];
    let mut pred_ctx: Vec<MbPredCtx> = vec![MbPredCtx::default(); total];
    let mut cabac_ctx: Vec<MbCabacCtx> = vec![MbCabacCtx::default(); total];
    let mut inter_ctx: Vec<MbInterCabacCtx> = vec![MbInterCabacCtx::default(); total];
    let mut qp = slice_qp;
    let mut prev_dqp_nonzero = false;
    // MBAFF pair state — same FFmpeg-mirrored pairing as the P path
    // (see parse_p_slice_cabac in cabac_p.rs).
    let mbaff_frame = mb_aff && !field_pic_flag;
    // PAFF field picture: field-coded throughout (field residual CABAC contexts
    // + field inverse scans). MBAFF branch overrides per pair, never for a field
    // picture. See parse_p_slice_cabac.
    let mut cur_pair_field = field_pic_flag;
    let mut field_flags: Vec<Option<bool>> = vec![None; total];
    let mut prev_mb_skipped = false;
    let mut next_mb_skipped = false;
    let mut decoded_mb_count = total;

    'mb_loop: for mb_idx in 0..total {
        // MBAFF pair-scan addressing (§6.4.2/§7.4.4) — see `cabac_p.rs`.
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
        let top_of_pair = mbaff_frame && mb_idx % 2 == 0;
        let is_skip = if mbaff_frame && !top_of_pair && prev_mb_skipped {
            next_mb_skipped
        } else {
            ctxs.mb_skip.decode(&mut dec, &skip_neighbors)
        };
        let mut pair_field_pending = false;
        if mbaff_frame && top_of_pair {
            if is_skip {
                let bot_left_skipped = if mb_x > 0 {
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
                let left_idx = (mb_y as usize) * mb_cols as usize + mb_x as usize - 1;
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
                let top_idx = ((mb_y as usize) - 1) * mb_cols as usize + mb_x as usize;
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
            field_flags[grid_idx] = Some(cur_pair_field);
            let bot_grid = grid_idx + mb_cols as usize;
            if bot_grid < total {
                field_flags[bot_grid] = Some(cur_pair_field);
            }
        }
        if is_skip {
            let mut mb = Macroblock::new_skip();
            mb.mb_type = MbType::BSkip;
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
            // EXCEPTION (MBAFF frame): not coded after the TOP macroblock of a
            // pair (`CurrMbAddr % 2 == 0` ⇒ `moreDataFlag = 1`).
            if !(mbaff_frame && mb_idx % 2 == 0) {
                let end_of_slice = dec.decode_terminate() == 1;
                if end_of_slice {
                    let is_last = mb_idx + 1 == total;
                    if !is_last {
                        decoded_mb_count = mb_idx + 1;
                    }
                    break 'mb_loop;
                }
            }
            continue;
        }

        // ctxIdxInc for B mb_type's first bin: count of available neighbours
        // that are NOT direct/skip (spec Table 9-39).
        let non_direct = |i: usize| {
            !macroblocks[i].skip && !matches!(macroblocks[i].mb_type, MbType::BDirect16x16)
        };
        let non_direct_neighbours = (mb_x > 0 && non_direct(left_idx.unwrap())) as usize
            + (mb_y > 0 && non_direct(top_idx.unwrap())) as usize;

        let nctx = NeighbourCtx::new(mbaff_frame, mb_rows, cur_pair_field, &field_flags);
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
                direct_8x8_inference_flag,
                non_direct_neighbours,
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
        // frame pair.
        if !(mbaff_frame && mb_idx % 2 == 0) {
            let end_of_slice = dec.decode_terminate() == 1;
            if end_of_slice {
                let is_last = mb_idx + 1 == total;
                if !is_last {
                    decoded_mb_count = mb_idx + 1;
                }
                break 'mb_loop;
            }
        }
    }

    let mut mv_store = MvStore::new(total);
    crate::mv::predict_b_slice_mvs(
        &mut mv_store,
        mb_cols,
        0,
        0,
        &macroblocks,
        colocated_mv,
        direct_spatial_mv_pred_flag,
        temporal,
    )?;
    Ok(ParsedSlice {
        macroblocks,
        nz,
        mv_store,
        decoded_mb_count,
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
    direct_8x8_inference_flag: bool,
    non_direct_neighbours: usize,
    nctx: NeighbourCtx,
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

    let (left_idx, top_idx) = nctx.left_top(mb_x, mb_y, mb_cols);

    let b_inter_type = ctxs.mb_type_b.decode(dec, non_direct_neighbours);
    // ctxIdx 32 is one physical context in FFmpeg (the final inter/intra gate
    // here -- and the L0/L1 discriminator / bits-nibble bins -- and bin 0 of
    // the intra-in-B suffix there) -- keep both copies adapted identically.
    ctxs.sync_shared_mb_type_ctx_prefix_to_suffix_b();
    if b_inter_type.is_none() {
        // Intra macroblock inside B slice.
        let intra_t = ctxs.intra_suffix.decode(dec);
        // Reverse sync of shared ctxIdx 32 (see above).
        ctxs.sync_shared_mb_type_ctx_suffix_to_prefix_b();
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
            nctx,
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
    if std::env::var("KINETIX_BINTRACE").is_ok() {
        let (r, o) = dec.debug_state();
        eprintln!("  B-MB({mb_x},{mb_y}) b_type_raw={b_type_raw} after_mbtype={r:#06x}/{o:#010x}");
    }

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
            // ref_idx is only coded when num_ref_idx_lX_active_minus1 > 0
            // (§7.3.5.2); with a single reference it is implicitly 0.
            let ri = if num_ref_idx_l0_active > 1 {
                let r = ctxs.ref_idx.decode(dec, lg, tg);
                if r >= num_ref_idx_l0_active {
                    return Err(SliceDataError::Unsupported("ref_idx L0 overflow"));
                }
                r
            } else {
                0
            };
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
            mb.motion = Some(motion);
        }
        2 => {
            mb.mb_type = MbType::BL116x16;
            let (lg, tg) =
                ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 1);
            let ri = if num_ref_idx_l1_active > 1 {
                let r = ctxs.ref_idx.decode(dec, lg, tg);
                if r >= num_ref_idx_l1_active {
                    return Err(SliceDataError::Unsupported("ref_idx L1 overflow"));
                }
                r
            } else {
                0
            };
            motion.ref_idx_l1.push(ri as i32);
            let blks: Vec<usize> = (0..16).collect();
            if ri > 0 {
                for &b in &blks {
                    this_inter.l1_ref_gt0 |= 1 << b;
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
                1,
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
                1,
                1,
            )?;
            motion.mvd_l1.push((mvx, mvy));
            this_inter.set_partition_l1(&blks, mvx, mvy, ri as i32);
            mb.motion = Some(motion);
        }
        3 => {
            mb.mb_type = MbType::BBi16x16;
            let blks: Vec<usize> = (0..16).collect();
            let (lg, tg) =
                ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 0);
            let ri0 = if num_ref_idx_l0_active > 1 {
                let r = ctxs.ref_idx.decode(dec, lg, tg);
                if r >= num_ref_idx_l0_active {
                    return Err(SliceDataError::Unsupported("ref_idx L0 overflow"));
                }
                r
            } else {
                0
            };
            motion.ref_idx_l0.push(ri0 as i32);
            let (lg1, tg1) =
                ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 1);
            let ri1 = if num_ref_idx_l1_active > 1 {
                let r = ctxs.ref_idx.decode(dec, lg1, tg1);
                if r >= num_ref_idx_l1_active {
                    return Err(SliceDataError::Unsupported("ref_idx L1 overflow"));
                }
                r
            } else {
                0
            };
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
                &mut ctxs.mvd_l0_x,
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
                &mut ctxs.mvd_l0_y,
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
            if std::env::var("KINETIX_BINTRACE").is_ok() {
                eprintln!(
                    "  BBi({mb_x},{mb_y}) ri0={ri0} ri1={ri1} mvd0=({mx0},{my0}) mvd1=({mx1},{my1})"
                );
            }
            mb.motion = Some(motion);
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
                    let ri = if num_ref_idx_l0_active > 1 {
                        let r = ctxs.ref_idx.decode(dec, lg, tg);
                        if r >= num_ref_idx_l0_active {
                            return Err(SliceDataError::Unsupported("ref_idx L0 overflow"));
                        }
                        r
                    } else {
                        0
                    };
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
                    let ri = if num_ref_idx_l1_active > 1 {
                        let r = ctxs.ref_idx.decode(dec, lg, tg);
                        if r >= num_ref_idx_l1_active {
                            return Err(SliceDataError::Unsupported("ref_idx L1 overflow"));
                        }
                        r
                    } else {
                        0
                    };
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
                        &mut ctxs.mvd_l0_x,
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
                        &mut ctxs.mvd_l0_y,
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
            if std::env::var("KINETIX_BINTRACE").is_ok() {
                let (r, o) = dec.debug_state();
                eprintln!(
                    "  B8x8 MB({mb_x},{mb_y}) sub_types={sub_types:?} after_sub={r:#06x}/{o:#010x}"
                );
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

            // §7.3.5.2 `sub_mb_pred()`: **all** `ref_idx_l0[mbPartIdx]` across
            // the 4 8×8 partitions are signalled first, in one `mbPartIdx`
            // loop, *then* all `ref_idx_l1[mbPartIdx]` in a second, separate
            // loop — not interleaved per-partition (`ref_idx_l0[0];
            // ref_idx_l1[0]; ref_idx_l0[1]; ...`), which is what an earlier
            // version of this function did. Any real B_8x8 macroblock with
            // at least one L0/Bi partition *and* at least one L1/Bi partition
            // (very common in real multi-reference B content) desynced the
            // CABAC engine here — the same interleaving-order mistake already
            // found and fixed for CAVLC/CABAC P_16x8/P_8x16 elsewhere in this
            // file, just in `ref_idx_l0`-vs-`ref_idx_l1` grouping instead of
            // `ref_idx`-vs-`mvd`.
            for part in 0..4 {
                if sub_dirs[part] == BPredDir::Direct
                    || !(sub_dirs[part] == BPredDir::L0 || sub_dirs[part] == BPredDir::Bi)
                {
                    continue;
                }
                let (c4, r4, _, _) = partition_dims(mb.mb_type, part);
                let (xp, yp, wp, hp) = (c4 as u32 * 4, r4 as u32 * 4, 8u32, 8u32);
                let blks = partition_blocks(c4, r4, 2, 2);
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
                let ri = if num_ref_idx_l0_active > 1 {
                    let r = ctxs.ref_idx.decode(dec, lg, tg);
                    if r >= num_ref_idx_l0_active {
                        return Err(SliceDataError::Unsupported("ref_idx L0 overflow"));
                    }
                    r
                } else {
                    0
                };
                motion.ref_idx_l0[part] = ri as i32;
                if ri > 0 {
                    for &b in &blks {
                        this_inter.l0_ref_gt0 |= 1 << b;
                    }
                }
            }
            for part in 0..4 {
                if sub_dirs[part] == BPredDir::Direct
                    || !(sub_dirs[part] == BPredDir::L1 || sub_dirs[part] == BPredDir::Bi)
                {
                    continue;
                }
                let (c4, r4, _, _) = partition_dims(mb.mb_type, part);
                let (xp, yp, wp, hp) = (c4 as u32 * 4, r4 as u32 * 4, 8u32, 8u32);
                let blks = partition_blocks(c4, r4, 2, 2);
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
                let ri = if num_ref_idx_l1_active > 1 {
                    let r = ctxs.ref_idx.decode(dec, lg, tg);
                    if r >= num_ref_idx_l1_active {
                        return Err(SliceDataError::Unsupported("ref_idx L1 overflow"));
                    }
                    r
                } else {
                    0
                };
                motion.ref_idx_l1[part] = ri as i32;
                if ri > 0 {
                    for &b in &blks {
                        this_inter.l1_ref_gt0 |= 1 << b;
                    }
                }
            }
            // MVDs, in ffmpeg's `h264_cabac_ref.c` L2140 order: list-outer,
            // part-inner, sub-partition innermost (NOT part-outer/list-inner —
            // the order changes the within-MB `amvd_sum` neighbour state fed to
            // `cabac_decode_mvd_component`, which desyncs the CABAC engine on
            // any B_8x8 with two lists active).
            for list in 0..2usize {
                for part in 0..4 {
                    let (c4, r4, _, _) = partition_dims(mb.mb_type, part);
                    let sub_t = sub_types[part] as usize;
                    let dir = sub_dirs[part];
                    if dir == BPredDir::Direct {
                        continue;
                    }
                    let uses = match list {
                        0 => dir == BPredDir::L0 || dir == BPredDir::Bi,
                        _ => dir == BPredDir::L1 || dir == BPredDir::Bi,
                    };
                    if !uses {
                        continue;
                    }
                    let n = crate::mv::B_SUB_MB_PARTS[sub_t];
                    for sub_i in 0..n {
                        let (sc4, sr4, sw4, sh4) = b8x8_sub_dims(sub_t, c4, r4, sub_i);
                        let (xp, yp, wp, hp) = (
                            sc4 as u32 * 4,
                            sr4 as u32 * 4,
                            sw4 as u32 * 4,
                            sh4 as u32 * 4,
                        );
                        let sub_blks = partition_blocks(sc4, sr4, sw4, sh4);
                        // mvd contexts (ctxIdxOffset 40 horiz / 47 vert) are
                        // shared across L0/L1 per spec; only the `list` arg
                        // (neighbour l0/l1 abs-mvd array) differs.
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
                            list,
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
                            list,
                            1,
                        )?;
                        if list == 0 {
                            motion.mvd_l0.push((mvx, mvy));
                            this_inter.set_partition_l0(
                                &sub_blks,
                                mvx,
                                mvy,
                                motion.ref_idx_l0[part],
                            );
                        } else {
                            motion.mvd_l1.push((mvx, mvy));
                            this_inter.set_partition_l1(
                                &sub_blks,
                                mvx,
                                mvy,
                                motion.ref_idx_l1[part],
                            );
                        }
                    }
                }
            }
            mb.motion = Some(motion);
        }
        _ => unreachable!(),
    }

    // DEBUG (session #25): force an MVD value for one macroblock via
    // KINETIX_FORCE_MVD="mb_x,mb_y,list,dx,dy" to identify the true encoder
    // value when a decoded MVD is suspected wrong.
    if let Ok(s) = std::env::var("KINETIX_FORCE_MVD") {
        let v: Vec<i32> = s.split(',').filter_map(|p| p.parse().ok()).collect();
        if v.len() == 5 && v[0] == mb_x as i32 && v[1] == mb_y as i32 {
            if let Some(motion) = &mut mb.motion {
                let (name, arr) = if v[2] == 0 {
                    ("L0", &mut motion.mvd_l0)
                } else {
                    ("L1", &mut motion.mvd_l1)
                };
                if let Some(last) = arr.last_mut() {
                    if std::env::var("KINETIX_BINTRACE").is_ok() {
                        eprintln!(
                            "FORCE_MVD MB({mb_x},{mb_y}) {name} {:?} -> ({},{})",
                            last, v[3], v[4]
                        );
                    }
                    *last = (v[3], v[4]);
                }
            }
        }
    }

    // CBP / mb_qp_delta / residual are signalled for ALL inter macroblocks,
    // including B_Direct_16x16 (spec §7.3.4/§7.3.5.1: `coded_block_pattern` is
    // present whenever MbPartPredMode != Intra_16x16, and a direct MB with
    // cbp != 0 carries residual coefficients). Skipping those bins here
    // desyncs the arithmetic engine for every following macroblock.
    let (cbp_l, cbp_c) =
        decode_inter_cbp_cabac(dec, ctxs, cabac_ctx_grid, mb_x, mb_y, mb_cols, nctx)?;
    let cbp = cbp_l | (cbp_c << 4);
    mb.cbp = cbp;
    // `transform_size_8x8_flag` (§7.3.5.1, §9.3.3.1.1.10, ctxIdxOffset 399):
    // present AFTER `coded_block_pattern` and BEFORE `mb_qp_delta` when
    // `transform_8x8_mode_flag && CodedBlockPatternLuma > 0` — the CABAC twin
    // of the CAVLC bug fixed in #32j. Same placement as the P-slice inter path
    // above.
    //
    // FFmpeg gates this on `dct8x8_allowed` (`h264_cabac_ref.c` line 2347),
    // which starts `= transform_8x8_mode` and is ONLY narrowed for:
    //   - B_Direct_16x16 (b_type_raw 0):  `&= direct_8x8_inference_flag`
    //     (line 2224);
    //   - B_8x8 (b_type_raw 22): `= get_dct8x8_allowed()` on the four
    //     `sub_mb_type`s (line 2161) — true iff every sub-partition is 8×8 or
    //     larger (raw {0,1,2,3}; with `!direct_8x8_inference_flag`, the 4×4
    //     variants raw {10,11,12} are also permitted since they carry only the
    //     `MB_TYPE_8x8` bit).
    // For B_L0/L1/Bi_16x16 (1..=3) AND B_16x8/B_8x16 (4..=21) it is NOT
    // narrowed — the flag IS read. The previous `matches!(1..=3)` gate wrongly
    // dropped the 16×8/8×16 case and desynced every stream with a coded
    // B_16x8/B_8x16 MB (e.g. `mbaff_ibp`).
    let dct8x8_allowed = match b_type_raw {
        0 => direct_8x8_inference_flag,
        22 => {
            let subs = mb
                .motion
                .as_ref()
                .and_then(|m| m.sub_mb_type_b)
                .unwrap_or([0u8; 4]);
            if direct_8x8_inference_flag {
                subs.iter().all(|&s| s <= 3)
            } else {
                subs.iter().all(|&s| s <= 3 || (10..=12).contains(&s))
            }
        }
        _ => true,
    };
    let mut is_8x8 = false;
    if transform_8x8_mode_flag && cbp_l != 0 && dct8x8_allowed {
        let left_8x8 = left_idx
            .map(|i| cabac_ctx_grid[i].transform_8x8)
            .unwrap_or(false);
        let top_8x8 = top_idx
            .map(|i| cabac_ctx_grid[i].transform_8x8)
            .unwrap_or(false);
        is_8x8 = ctxs.transform_8x8.decode(dec, left_8x8, top_8x8);
    }
    mb.transform_size_8x8 = is_8x8;
    let mut qp = prev_qp;
    let mut dqp_nz = false;
    if cbp_l != 0 || cbp_c != 0 {
        let dqp = ctxs.qp_delta.decode(dec, prev_dqp_nonzero);
        dqp_nz = dqp != 0;
        qp = (prev_qp + dqp + 52).rem_euclid(52);
    }
    mb.qp = qp;
    let mut this_cabac_ctx = decode_inter_residual_cabac(
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
        is_8x8,
        nctx,
    )?;
    this_cabac_ctx.transform_8x8 = is_8x8;
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
    nctx: NeighbourCtx,
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
    let (left_idx, top_idx) = nctx.left_top(mb_x, mb_y, mb_cols);

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
                let pred_mode =
                    mpm_pred_mode_8x8(pred_ctx_grid, mb_x, mb_y, mb_cols, &modes, i8, nctx);
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
                let pred_mode =
                    mpm_pred_mode(pred_ctx_grid, mb_x, mb_y, mb_cols, &modes, raster, nctx);
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
            crate::slice_data::ctx::cabac_cbp_neighbors(cabac_ctx_grid, mb_x, mb_y, mb_cols, nctx);
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
            let (coeffs, _count) =
                ctxs.residual
                    .decode_block(dec, CAT_LUMA_DC, 16, nctx.is_field());
            mb.luma_dc = coeffs;
            cbp_word |= 0x100;
        }
    }
    if is_8x8 {
        for blk8 in 0..4usize {
            if (cbp_l >> blk8) & 1 == 0 {
                continue;
            }
            let (coeffs, count) = ctxs.residual.decode_block_8x8(dec, nctx.is_field());
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
            let (left_coded, top_coded) =
                luma_cbf_neighbors(nz_grid, mb_x, mb_y, mb_cols, &this_nz, block, true, nctx);
            if ctxs.cbf.decode(dec, luma_cat, left_coded, top_coded) {
                let (coeffs, count) =
                    ctxs.residual
                        .decode_block(dec, luma_cat, luma_max, nctx.is_field());
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
                let (coeffs, _) =
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
        for comp in 0..2 {
            for block in 0..4 {
                let (left_coded, top_coded) = chroma_cbf_neighbors(
                    nz_grid, mb_x, mb_y, mb_cols, &this_nz, comp, block, true, nctx,
                );
                if ctxs.cbf.decode(dec, CAT_CHROMA_AC, left_coded, top_coded) {
                    let (coeffs, count) =
                        ctxs.residual
                            .decode_block(dec, CAT_CHROMA_AC, 15, nctx.is_field());
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
    nctx: NeighbourCtx,
) -> R<(u8, u8)> {
    // For inter MBs, off-picture neighbours use 0x00F (not 0x7CF) per FFmpeg.
    // Under MBAFF frame pairs the left CBP is rebuilt from left_top/left_bottom
    // (see `cabac_cbp_neighbors_inter`); the wholesale copy is only correct for
    // non-MBAFF or all-frame-coded pairs.
    let (left, top) =
        super::ctx::cabac_cbp_neighbors_inter(cabac_ctx_grid, mb_x, mb_y, mb_cols, nctx, 0x00F);
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
    is_8x8: bool,
    nctx: NeighbourCtx,
) -> R<MbCabacCtx> {
    use crate::cabac_tables::{CAT_CHROMA_AC, CAT_CHROMA_DC, CAT_LUMA_4X4};
    let (left_idx, top_idx) = nctx.left_top(mb_x, mb_y, mb_cols);
    let mut cbp_word: u16 = mb.cbp as u16;

    if is_8x8 {
        // High-profile 8×8 transform on an INTER macroblock
        // (`transform_size_8x8_flag` set): luma residual lives in four 8×8
        // blocks (`luma_coeffs_8x8`), decoded via `decode_block_8x8` and
        // stored in zigzag order for `dequant_idct_8x8_scan`. No separate
        // `coded_block_flag` is signalled (non-4:4:4, §9.3.3.1.1.9) — the
        // CBP luma bit alone gates presence, mirroring the intra 8×8 path.
        for blk8 in 0..4usize {
            if (cbp_l >> blk8) & 1 == 0 {
                continue;
            }
            // `decode_block_8x8` returns coefficients in **scan-position order**
            // (`out[scan_pos] = level`), which is exactly what
            // `dequant_idct_8x8_scan(..., ZIGZAG_8X8)` in `reconstruct_inter_luma`
            // expects (`block[ZIGZAG_8X8[z]] = dequant(coeffs[z])`). Store them
            // directly — the earlier `INVERSE_ZIGZAG_8X8[scan_pos]` remap was a
            // double-permutation that scrambled every non-DC coefficient
            // (`mbaff_ip` MB(1,3): 236/256 differ → 60/256 after this fix).
            let (coeffs_scan, count) = ctxs.residual.decode_block_8x8(dec, nctx.is_field());
            mb.luma_coeffs_8x8[blk8] = coeffs_scan;
            for sub in 0..4usize {
                this_nz.luma[raster_of_8x8_sub(blk8, sub)] = count;
            }
        }
    } else {
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
            let (left_coded, top_coded) =
                luma_cbf_neighbors(nz_grid, mb_x, mb_y, mb_cols, this_nz, block, false, nctx);
            let (lr, lo) = dec.debug_state();
            let has_coeff = ctxs.cbf.decode(dec, CAT_LUMA_4X4, left_coded, top_coded);
            let (ar, ao) = dec.debug_state();
            if std::env::var("KINETIX_BINTRACE").is_ok() {
                eprintln!("    luma blk={block} left={left_coded} top={top_coded} cbf={has_coeff} {lr:#06x}/{lo:#010x}->{ar:#06x}/{ao:#010x}");
            }
            if has_coeff {
                let (coeffs, count) =
                    ctxs.residual
                        .decode_block(dec, CAT_LUMA_4X4, 16, nctx.is_field());
                let (cr, co) = dec.debug_state();
                if std::env::var("KINETIX_BINTRACE").is_ok() {
                    eprintln!(
                        "    luma blk={block} nz={count} coeffs={:?} after={cr:#06x}/{co:#010x}",
                        &coeffs[..16]
                    );
                }
                this_nz.luma[block] = count;
                mb.luma_coeffs[block] = coeffs;
            }
        }
    }
    if cbp_c != 0 {
        for comp in 0..2 {
            let bit = 0x40u16 << comp;
            // §9.3.3.1.1.9 / FFmpeg fill_decode_caches: for INTER macroblocks
            // an off-picture neighbour's cbp sentinel is 0x00F, whose chroma-DC
            // bits are clear — i.e. "not coded" (intra MBs use 0x7CF → coded).
            let left_coded = match left_idx {
                None => false,
                Some(i) => cabac_ctx_grid[i].cbp_word & bit != 0,
            };
            let top_coded = match top_idx {
                None => false,
                Some(i) => cabac_ctx_grid[i].cbp_word & bit != 0,
            };
            if ctxs.cbf.decode(dec, CAT_CHROMA_DC, left_coded, top_coded) {
                let (coeffs, _) =
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
        for comp in 0..2 {
            for block in 0..4 {
                let (left_coded, top_coded) = chroma_cbf_neighbors(
                    nz_grid, mb_x, mb_y, mb_cols, this_nz, comp, block, false, nctx,
                );
                if ctxs.cbf.decode(dec, CAT_CHROMA_AC, left_coded, top_coded) {
                    let (coeffs, count) =
                        ctxs.residual
                            .decode_block(dec, CAT_CHROMA_AC, 15, nctx.is_field());
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
