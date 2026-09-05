//! Session #32d: MBAFF I-slice ORACLE walk (todo-h264.md "DECISIVE NEXT STEP").
//!
//! Mechanically transcribes FFmpeg's I-slice CABAC path
//! (`ff_h264_decode_mb_cabac`, intra branch) for MBAFF frame pictures:
//! field-decoding flag (ctx 70+), I-slice mb_type (ctx 3..=10),
//! transform_size_8x8_flag (ctx 399+), Intra4x4/8x8 MPM (§8.3.1.1),
//! chroma_pre_mode (ctx 64/67), cbp luma/chroma (ctx 73..=84), mb_qp_delta
//! (ctx 60..=63) — then the residual walk through this crate's own
//! (session-#32b-proven) `CodedBlockFlagContext`/`ResidualCabacContext`
//! types so the engine stays aligned through every `end_of_slice_flag`.
//!
//! Prints one `TRC MBn ...` line per macroblock in the same format as the
//! crate parser's `KINETIX_BINTRACE` output; diffing the two pinpoints the
//! first divergent element/context on the real `mbaff_i1` payload.
//!
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_mbaff_oracle -- --nocapture

use tpt_kinetix_h264::cabac_tables::{
    CAT_CHROMA_AC, CAT_CHROMA_DC, CAT_LUMA_4X4, CAT_LUMA_AC, CAT_LUMA_DC,
};
use tpt_kinetix_h264::entropy::{CabacContext, CabacDecoder};

const W_MB: usize = 4;
const H_MB: usize = 4;
const TOTAL: usize = W_MB * H_MB;

/// Raster 4×4 index within a macroblock for the `sub`-th block of the
/// `blk8`-th 8×8 group (spec block scan order, Figure 6-10).
fn raster_of_8x8_sub(blk8: usize, sub: usize) -> usize {
    let gx = (blk8 % 2) * 2;
    let gy = (blk8 / 2) * 2;
    let sx = sub % 2;
    let sy = sub / 2;
    (gy + sy) * 4 + (gx + sx)
}

fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut i = 0;
    while i < nal.len() {
        if i + 2 < nal.len() && nal[i] == 0 && nal[i + 1] == 0 && nal[i + 2] == 3 {
            out.push(0);
            i += 3;
        } else {
            out.push(nal[i]);
            i += 1;
        }
    }
    out
}

fn find_nals(data: &[u8]) -> Vec<(u8, u8, Vec<u8>)> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut nals = Vec::new();
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(data.len());
        // Trim the trailing zero bytes that belong to the next start code.
        let mut end = e;
        while end > s && data[end - 1] == 0 && e - end < 3 {
            end -= 1;
        }
        let raw = &data[s..end];
        if raw.is_empty() {
            continue;
        }
        nals.push((raw[0] & 0x1F, raw[0] >> 5, unescape(&raw[1..])));
    }
    nals
}

#[derive(Clone, Copy, PartialEq)]
enum Side {
    Unavailable,
    ForcedDc,
    Real(u8),
}

impl Side {
    fn value(self) -> u8 {
        match self {
            Side::Real(v) => v,
            _ => 2,
        }
    }
}

#[derive(Clone, Copy)]
struct PredCtx {
    present: bool,
    is4x4: bool,
    modes: [u8; 16],
}

impl Default for PredCtx {
    fn default() -> Self {
        PredCtx {
            present: false,
            is4x4: false,
            modes: [2; 16],
        }
    }
}

#[test]
fn mbaff_i1_oracle_walk() {
    let dir = std::env::temp_dir().join("dbg_g5_interlaced");
    let h264 = dir.join("mbaff_i1.h264");
    if !h264.exists() {
        eprintln!("skip: run dbg_g5_interlaced corpus first");
        return;
    }
    let data = std::fs::read(&h264).unwrap();
    let nals = find_nals(&data);
    let sps_nal = nals.iter().find(|n| n.0 == 7).expect("no SPS").2.clone();
    let pps_nal = nals.iter().find(|n| n.0 == 8).expect("no PPS").2.clone();
    let slice = nals.iter().find(|n| n.0 == 5).expect("no IDR slice");

    let sps = tpt_kinetix_h264::sps::SeqParameterSet::parse(&sps_nal).expect("SPS parse");
    let pps = tpt_kinetix_h264::pps::PicParameterSet::parse(&pps_nal, None).expect("PPS parse");
    let hdr_ctx = tpt_kinetix_h264::slice::SliceHeaderContext {
        log2_max_frame_num_minus4: sps.log2_max_frame_num_minus4,
        pic_order_cnt_type: sps.pic_order_cnt_type,
        log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_pic_order_cnt_lsb_minus4,
        frame_mbs_only_flag: sps.frame_mbs_only_flag,
        bottom_field_pic_order_in_frame_present_flag: pps
            .bottom_field_pic_order_in_frame_present_flag,
        delta_pic_order_always_zero_flag: false,
        num_ref_idx_l0_default_active_minus1: pps.num_ref_idx_l0_default_active_minus1,
        num_ref_idx_l1_default_active_minus1: pps.num_ref_idx_l1_default_active_minus1,
        weighted_pred_flag: pps.weighted_pred_flag,
        weighted_bipred_idc: pps.weighted_bipred_idc,
        entropy_coding_mode_flag: pps.entropy_coding_mode_flag,
        deblocking_filter_control_present_flag: pps.deblocking_filter_control_present_flag,
        redundant_pic_cnt_present_flag: pps.redundant_pic_cnt_present_flag,
        num_slice_groups_minus1: pps.num_slice_groups_minus1,
        chroma_array_type: if sps.chroma_format_idc == 0 { 0 } else { 1 },
    };
    let hdr = tpt_kinetix_h264::slice::SliceHeader::parse_with_context(
        &slice.2,
        tpt_kinetix_h264::nal::NalUnitType::IdrSlice,
        slice.1,
        &hdr_ctx,
    )
    .expect("slice header parse");
    assert!(!hdr.field_pic_flag, "expected a frame picture");
    let slice_qp = 26 + pps.pic_init_qp_minus26 + hdr.slice_qp_delta;
    let payload = &slice.2[hdr.data_bit_offset.div_ceil(8)..];
    println!("ORACLE slice_qp={slice_qp} payload_len={}", payload.len());

    let mut st: Vec<CabacContext> = (0..1024)
        .map(|i| {
            let (m, n) = tpt_kinetix_h264::cabac_tables::CABAC_CTX_INIT_I[i];
            let mut c = CabacContext::init(m as i32, n as i32, slice_qp);
            c.ctx_id = i as u16;
            c
        })
        .collect();
    let mut ctxs = tpt_kinetix_h264::slice_data::CabacSliceContexts::new(slice_qp);
    let mut dec = CabacDecoder::new(payload).expect("cabac init");

    // Per-frame-MB grids.
    let mut class16 = [false; TOTAL]; // I16x16 or PCM
    let mut t8grid = [false; TOTAL];
    let mut cbpw = [0x7CFu16; TOTAL]; // sentinel until decoded
    let mut chroma_tab = [0u8; TOTAL];
    let mut predg = vec![PredCtx::default(); TOTAL];
    let mut nzg_luma = [[0u8; 16]; TOTAL];
    let mut nzg_chroma = [[0u8; 8]; TOTAL];
    let mut field_flags: Vec<Option<bool>> = vec![None; TOTAL];

    let mut qp = slice_qp;
    let mut prev_dqp_nonzero = false;
    let mut prev_pair_field = false;

    'pairs: for pair in 0..(TOTAL / 2) {
        let px = pair % W_MB;
        let py = pair / W_MB;
        let gy_top = 2 * py;

        // mb_field_decoding_flag (decode_cabac_field_decoding_flag):
        // ctx = (prev pair's flag && !!mb_x) + (pair-above interlaced).
        let mut fctx = 0usize;
        if px > 0 && prev_pair_field {
            fctx += 1;
        }
        if py > 0 && field_flags[(gy_top - 1) * W_MB + px].unwrap_or(false) {
            fctx += 1;
        }
        let cur_field = dec.decode_decision(&mut st[70 + fctx]) == 1;
        field_flags[gy_top * W_MB + px] = Some(cur_field);
        field_flags[(gy_top + 1) * W_MB + px] = Some(cur_field);

        for m in 0..2usize {
            let mb_idx = pair * 2 + m;
            let gx = px;
            let gy = gy_top + m;
            let gidx = gy * W_MB + px;
            let nb = tpt_kinetix_h264::mbaff::derive_neighbours(
                gx as u32,
                gy as u32,
                W_MB as u32,
                H_MB as u32,
                cur_field,
                &field_flags,
            );
            let left = nb.left_top;
            let top = nb.top;
            let lclass = left.map(|i| class16[i]).unwrap_or(false);
            let tclass = top.map(|i| class16[i]).unwrap_or(false);

            // I-slice mb_type: decode_cabac_intra_mb_type(sl,3,intra_slice=1).
            let first =
                dec.decode_decision(&mut st[3 + lclass as usize + 2 * tclass as usize]) as u32;
            let mb_type_ff = if first == 0 {
                0u32
            } else if dec.decode_terminate() == 1 {
                25
            } else {
                let mut t = 1u32;
                t += 12 * dec.decode_decision(&mut st[6]) as u32;
                if dec.decode_decision(&mut st[7]) == 1 {
                    t += 4 + 4 * dec.decode_decision(&mut st[8]) as u32;
                }
                t += 2 * dec.decode_decision(&mut st[9]) as u32;
                t += dec.decode_decision(&mut st[10]) as u32;
                t
            };
            assert!(mb_type_ff != 25, "I_PCM not expected in this clip");
            let is_i16 = mb_type_ff != 0;
            let mut cbp_l = 0u8;
            let mut cbp_c = 0u8;
            let mut i16_mode = 0u8;
            if is_i16 {
                let t = mb_type_ff - 1;
                i16_mode = (t % 4) as u8;
                cbp_c = ((t % 12) / 4) as u8;
                cbp_l = if t % 12 >= 4 { 0x0F } else { 0 };
            }

            // transform_size_8x8_flag + Intra pred modes (I_NxN only).
            let mut is_8x8 = false;
            let mut modes = [2u8; 16];
            if !is_i16 {
                let l8 = left.map(|i| t8grid[i]).unwrap_or(false);
                let t8f = top.map(|i| t8grid[i]).unwrap_or(false);
                is_8x8 = pps.transform_8x8_mode_flag
                    && dec.decode_decision(&mut st[399 + l8 as usize + t8f as usize]) == 1;

                fn side_of(p: Option<usize>, grid: &[PredCtx], r: usize, c: usize) -> Side {
                    match p {
                        None => Side::Unavailable,
                        Some(i) => {
                            let n = &grid[i];
                            if !n.present {
                                Side::Unavailable
                            } else if n.is4x4 {
                                Side::Real(n.modes[raster_of_8x8_sub(r, c)])
                            } else {
                                Side::ForcedDc
                            }
                        }
                    }
                }
                let decode_mode =
                    |dec: &mut CabacDecoder, st: &mut Vec<CabacContext>, pred: u8| -> u8 {
                        if dec.decode_decision(&mut st[68]) == 1 {
                            return pred;
                        }
                        let mut mode = 0u8;
                        for i in 0..3u8 {
                            mode += (dec.decode_decision(&mut st[69])) << i;
                        }
                        mode + (mode >= pred) as u8
                    };

                if is_8x8 {
                    for i8 in 0..4usize {
                        // ffmpeg pred_intra_mode over scan8 quadrant adjacency.
                        let lside = match i8 {
                            0 => side_of(left, &predg, 1, 1),
                            1 => Side::Real(modes[raster_of_8x8_sub(0, 1)]),
                            2 => side_of(left, &predg, 3, 1),
                            _ => Side::Real(modes[raster_of_8x8_sub(2, 1)]),
                        };
                        let tside = match i8 {
                            0 => side_of(top, &predg, 2, 2),
                            1 => side_of(top, &predg, 3, 2),
                            2 => Side::Real(modes[raster_of_8x8_sub(0, 2)]),
                            _ => Side::Real(modes[raster_of_8x8_sub(1, 2)]),
                        };
                        let pred = if matches!(lside, Side::Unavailable)
                            || matches!(tside, Side::Unavailable)
                        {
                            2
                        } else {
                            lside.value().min(tside.value())
                        };
                        let mode = decode_mode(&mut dec, &mut st, pred);
                        for sub in 0..4usize {
                            modes[raster_of_8x8_sub(i8, sub)] = mode;
                        }
                    }
                } else {
                    for blk in 0..16usize {
                        let raster = raster_of_8x8_sub(blk / 4, blk % 4);
                        let bx = raster % 4;
                        let by = raster / 4;
                        // Spec §8.3.1.1 MPM with three-state neighbour sides.
                        let ls = if bx > 0 {
                            Side::Real(modes[by * 4 + bx - 1])
                        } else if let Some(li) = left {
                            let n = &predg[li];
                            if !n.present {
                                Side::Unavailable
                            } else if n.is4x4 {
                                Side::Real(n.modes[by * 4 + 3])
                            } else {
                                Side::ForcedDc
                            }
                        } else {
                            Side::Unavailable
                        };
                        let ts = if by > 0 {
                            Side::Real(modes[(by - 1) * 4 + bx])
                        } else if let Some(ti) = top {
                            let n = &predg[ti];
                            if !n.present {
                                Side::Unavailable
                            } else if n.is4x4 {
                                Side::Real(n.modes[12 + bx])
                            } else {
                                Side::ForcedDc
                            }
                        } else {
                            Side::Unavailable
                        };
                        let pred =
                            if matches!(ls, Side::Unavailable) || matches!(ts, Side::Unavailable) {
                                2
                            } else {
                                ls.value().min(ts.value())
                            };
                        modes[raster] = decode_mode(&mut dec, &mut st, pred);
                    }
                }
            }

            // intra_chroma_pred_mode (decode_cabac_mb_chroma_pre_mode).
            let lc = left.map(|i| chroma_tab[i] != 0).unwrap_or(false);
            let tc = top.map(|i| chroma_tab[i] != 0).unwrap_or(false);
            let chroma = if dec.decode_decision(&mut st[64 + lc as usize + tc as usize]) == 0 {
                0u8
            } else if dec.decode_decision(&mut st[67]) == 0 {
                1
            } else if dec.decode_decision(&mut st[67]) == 0 {
                2
            } else {
                3
            };

            // coded_block_pattern (I_NxN only; I16x16 carries it in mb_type).
            if !is_i16 {
                let lcbp = left.map(|i| cbpw[i]).unwrap_or(0x7CF);
                let tcbp = top.map(|i| cbpw[i]).unwrap_or(0x7CF);
                // decode_cabac_mb_cbp_luma: running cur-word ctx updates.
                // Verbatim FFmpeg: bin0 uses neighbour words; bins 1-3 mix
                // neighbour words with the RUNNING cbp bits.
                let mut cur = 0u8;
                let ctx = (lcbp & 0x02 == 0) as usize + 2 * (tcbp & 0x04 == 0) as usize;
                cur += dec.decode_decision(&mut st[73 + ctx]) as u8;
                let ctx = (cur & 0x01 == 0) as usize + 2 * (tcbp & 0x08 == 0) as usize;
                cur += (dec.decode_decision(&mut st[73 + ctx]) as u8) << 1;
                let ctx = (lcbp & 0x08 == 0) as usize + 2 * (cur & 0x01 == 0) as usize;
                cur += (dec.decode_decision(&mut st[73 + ctx]) as u8) << 2;
                let ctx = (cur & 0x04 == 0) as usize + 2 * (cur & 0x02 == 0) as usize;
                cur += (dec.decode_decision(&mut st[73 + ctx]) as u8) << 3;
                cbp_l = cur;
                // decode_cabac_mb_cbp_chroma.
                let cacb_a = (lcbp >> 4) & 0x03;
                let cacb_b = (tcbp >> 4) & 0x03;
                let mut cctx = 0usize;
                if cacb_a > 0 {
                    cctx += 1;
                }
                if cacb_b > 0 {
                    cctx += 2;
                }
                if dec.decode_decision(&mut st[77 + cctx]) == 1 {
                    let mut cctx = 4usize;
                    if cacb_a == 2 {
                        cctx += 1;
                    }
                    if cacb_b == 2 {
                        cctx += 2;
                    }
                    cbp_c = 1 + dec.decode_decision(&mut st[77 + cctx]) as u8;
                }
            }

            // mb_qp_delta (ffmpeg decode_cabac_mb_dqp: first bin at
            // 60+(last_qscale_diff!=0); on 1 -> val=1, ctx=2; each further
            // `1` bin reads at 60+2 ONCE then pins ctx to 3).
            let need_qp = is_i16 || cbp_l != 0 || cbp_c != 0;
            if need_qp {
                let mut dqp = 0u32;
                if dec.decode_decision(&mut st[60 + prev_dqp_nonzero as usize]) == 1 {
                    dqp = 1;
                    let mut qctx = 2usize;
                    loop {
                        if dec.decode_decision(&mut st[60 + qctx]) == 0 {
                            break;
                        }
                        qctx = 3;
                        dqp += 1;
                    }
                    qp = (qp + dqp as i32) % 52;
                }
                prev_dqp_nonzero = dqp != 0;
            } else {
                prev_dqp_nonzero = false;
            }

            // ---- residual (crate-proven types; neighbour rules replicated).
            let mut word = cbp_l as u16 | ((cbp_c as u16) << 4);
            let mut nz_luma = [0u8; 16];
            let mut nz_chroma = [0u8; 8];
            if is_i16 {
                let lc = left.map(|i| cbpw[i] & 0x100 != 0).unwrap_or(true);
                let tc = top.map(|i| cbpw[i] & 0x100 != 0).unwrap_or(true);
                if ctxs.cbf.decode(&mut dec, CAT_LUMA_DC, lc, tc) {
                    ctxs.residual
                        .decode_block(&mut dec, CAT_LUMA_DC, 16, cur_field);
                    word |= 0x100;
                }
            }
            if is_8x8 {
                for blk8 in 0..4usize {
                    if (cbp_l >> blk8) & 1 == 0 {
                        continue;
                    }
                    let (_coeffs, count) = ctxs.residual.decode_block_8x8(&mut dec, cur_field);
                    for sub in 0..4usize {
                        nz_luma[raster_of_8x8_sub(blk8, sub)] = count;
                    }
                }
            } else {
                let luma_max = if is_i16 { 15 } else { 16 };
                let cat = if is_i16 { CAT_LUMA_AC } else { CAT_LUMA_4X4 };
                for blk8 in 0..4usize {
                    if (cbp_l >> blk8) & 1 == 0 {
                        continue;
                    }
                    for sub in 0..4usize {
                        let blk = raster_of_8x8_sub(blk8, sub);
                        let bx = blk % 4;
                        let by = blk / 4;
                        let lcb = if bx > 0 {
                            nz_luma[by * 4 + bx - 1] > 0
                        } else if let Some(li) = left {
                            nzg_luma[li][by * 4 + 3] > 0
                        } else {
                            true
                        };
                        let tcb2 = if by > 0 {
                            nz_luma[(by - 1) * 4 + bx] > 0
                        } else if let Some(ti) = top {
                            nzg_luma[ti][12 + bx] > 0
                        } else {
                            true
                        };
                        if ctxs.cbf.decode(&mut dec, cat, lcb, tcb2) {
                            let (_c, count) = ctxs
                                .residual
                                .decode_block(&mut dec, cat, luma_max, cur_field);
                            nz_luma[blk] = count;
                        }
                    }
                }
            }
            if cbp_c != 0 {
                for comp in 0..2usize {
                    let bit = 0x40u16 << comp;
                    let lc = left.map(|i| cbpw[i] & bit != 0).unwrap_or(true);
                    let tc = top.map(|i| cbpw[i] & bit != 0).unwrap_or(true);
                    if ctxs.cbf.decode(&mut dec, CAT_CHROMA_DC, lc, tc) {
                        ctxs.residual
                            .decode_block(&mut dec, CAT_CHROMA_DC, 4, cur_field);
                        word |= bit;
                    }
                }
            }
            if cbp_c == 2 {
                for comp in 0..2usize {
                    for block in 0..4usize {
                        let base = comp * 4;
                        let bx = block % 2;
                        let by = block / 2;
                        let lcb = if bx > 0 {
                            nz_chroma[base + by * 2 + bx - 1] > 0
                        } else if let Some(li) = left {
                            nzg_chroma[li][base + by * 2 + 1] > 0
                        } else {
                            true
                        };
                        let tcb2 = if by > 0 {
                            nz_chroma[base + (by - 1) * 2 + bx] > 0
                        } else if let Some(ti) = top {
                            nzg_chroma[ti][base + 2 + bx] > 0
                        } else {
                            true
                        };
                        if ctxs.cbf.decode(&mut dec, CAT_CHROMA_AC, lcb, tcb2) {
                            let (_c, count) =
                                ctxs.residual
                                    .decode_block(&mut dec, CAT_CHROMA_AC, 15, cur_field);
                            nz_chroma[base + block] = count;
                        }
                    }
                }
            }

            // Commit grids.
            class16[gidx] = is_i16;
            t8grid[gidx] = is_8x8;
            cbpw[gidx] = word;
            chroma_tab[gidx] = chroma;
            predg[gidx] = PredCtx {
                present: true,
                is4x4: !is_i16,
                modes,
            };
            nzg_luma[gidx] = nz_luma;
            nzg_chroma[gidx] = nz_chroma;

            let (rs, os) = dec.debug_state();
            let type_str = if is_i16 {
                format!(
                    "Intra16x16 {{ pred_mode: {i16_mode}, cbp_chroma: {cbp_c}, cbp_luma: {} }}",
                    if cbp_l != 0 { 15 } else { 0 }
                )
            } else {
                "Intra4x4".to_string()
            };
            println!(
                "TRC MB{mb_idx} px={gx} py={gy} field={cur_field} type={type_str} cbp={:#04x} qp={qp} t8={is_8x8} chroma={chroma} modes={modes:?} state={rs:#06x}/{os:#010x}",
                cbp_l | (cbp_c << 4),
            );

            let eos = dec.decode_terminate() == 1;
            let is_last = mb_idx + 1 == TOTAL;
            if !is_last && eos {
                println!("ORACLE: premature end_of_slice at MB{mb_idx} -- walk desynced");
                break 'pairs;
            }
        }

        prev_pair_field = cur_field;
    }
}
