use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::slice::{SliceHeader, SliceHeaderContext};
use tpt_kinetix_h264::sps::SeqParameterSet;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let data = std::fs::read(&path).unwrap();
    let nals = parse_nal_units_from_annexb(&data);
    let mut sps = None;
    let mut pps = None;
    for n in &nals {
        match n.nal_unit_type {
            NalUnitType::Sps => sps = Some(SeqParameterSet::parse(&n.rbsp).unwrap()),
            NalUnitType::Pps => pps = Some(PicParameterSet::parse(&n.rbsp).unwrap()),
            _ => {}
        }
    }
    let sps = sps.unwrap();
    let pps = pps.unwrap();
    let ctx = SliceHeaderContext {
        log2_max_frame_num_minus4: sps.log2_max_frame_num_minus4,
        pic_order_cnt_type: sps.pic_order_cnt_type,
        log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_pic_order_cnt_lsb_minus4,
        frame_mbs_only_flag: sps.frame_mbs_only_flag,
        bottom_field_pic_order_in_frame_present_flag: pps.bottom_field_pic_order_in_frame_present_flag,
        delta_pic_order_always_zero_flag: false,
        num_ref_idx_l0_default_active_minus1: pps.num_ref_idx_l0_default_active_minus1,
        num_ref_idx_l1_default_active_minus1: pps.num_ref_idx_l1_default_active_minus1,
        weighted_pred_flag: pps.weighted_pred_flag,
        weighted_bipred_idc: pps.weighted_bipred_idc,
        entropy_coding_mode_flag: pps.entropy_coding_mode_flag,
        deblocking_filter_control_present_flag: pps.deblocking_filter_control_present_flag,
        redundant_pic_cnt_present_flag: pps.redundant_pic_cnt_present_flag,
        num_slice_groups_minus1: pps.num_slice_groups_minus1,
        chroma_array_type: sps.chroma_format_idc,
    };
    for n in &nals {
        if n.nal_unit_type == NalUnitType::NonIdrSlice {
            let h = SliceHeader::parse_with_context(&n.rbsp, n.nal_unit_type, &ctx).unwrap();
            let mut r = BitReader::new(&n.rbsp);
            r.seek_to_bit(h.data_bit_offset);
            let skip_run = read_ue(&mut r);
            let mb_type_ue = read_ue(&mut r);
            let mvd_h = read_se(&mut r);
            let mvd_v = read_se(&mut r);
            let cbp_golomb = read_ue(&mut r) as usize;
            let mb_qp_delta = read_se(&mut r);
            let start = r.bit_position();

            // Decode CBP from inter Golomb code (Table 9-4)
            const GOLOMB_TO_INTER_CBP: [u8; 48] = [
                 0,  1,  2,  4,  8,  3,  5, 10, 12, 15,  7, 11, 13, 14,  6,  9,
                16, 17, 18, 20, 24, 19, 21, 26, 28, 31, 23, 27, 29, 30, 22, 25,
                32, 33, 34, 36, 40, 35, 37, 42, 44, 47, 39, 43, 45, 46, 38, 41,
            ];
            let cbp_raw = GOLOMB_TO_INTER_CBP[cbp_golomb.min(47)];
            let cbp_luma = (cbp_raw & 0xf) as usize;
            println!("skip_run={skip_run} mb_type={mb_type_ue} mvd=({mvd_h},{mvd_v}) cbp_golomb={cbp_golomb} cbp_raw={cbp_raw:#04x} cbp_luma={cbp_luma:#x} qp_delta={mb_qp_delta}");
            println!("residuals start at {start}");
            let mut nz = [0u8; 16];
            let mut blocks = Vec::new();
            for blk8 in 0..4usize {
                if (cbp_luma >> blk8) & 1 == 0usize {
                    continue;
                }
                for sub in 0..4usize {
                    blocks.push(raster_of_8x8_sub(blk8, sub));
                }
            }
            for &block in &blocks {
                let bx = (block % 4) as i32;
                let by = (block / 4) as i32;
                let left = if bx > 0 {
                    Some(nz[(by * 4 + bx - 1) as usize])
                } else {
                    None
                };
                let top = if by > 0 {
                    Some(nz[((by - 1) * 4 + bx) as usize])
                } else {
                    None
                };
                let nc = match (left, top) {
                    (Some(l), Some(t)) => (l as i32 + t as i32 + 1) >> 1,
                    (Some(l), None) => l as i32,
                    (None, Some(t)) => t as i32,
                    (None, None) => 0,
                };
                let pos_before = r.bit_position();

                // Peek 16 bits for diagnostics
                let peek_bits = {
                    let p = r.bit_position();
                    let mut bits = String::new();
                    for _ in 0..16 {
                        bits.push(if r.read_bit().unwrap_or(0) == 1 { '1' } else { '0' });
                    }
                    r.seek_to_bit(p);
                    bits
                };

                let token_result = tpt_kinetix_h264::cavlc_tables::read_coeff_token(&mut r, nc);
                let pos_after_token = r.bit_position();

                match token_result {
                    Err(e) => {
                        println!("block={block:2} pos={pos_before:3} nc={nc:2} bits={peek_bits} => coeff_token ERR: {e:?}");
                        return;
                    }
                    Ok((0, _)) => {
                        println!("block={block:2} pos={pos_before:3} nc={nc:2} bits={peek_bits} => tc=0 (token_bits={})", pos_after_token - pos_before);
                        nz[block] = 0;
                        continue;
                    }
                    Ok((tc_raw, t1_raw)) => {
                        let tc = tc_raw as usize;
                        let t1 = t1_raw as usize;
                        let trace_block = block == 13;
                        let mut suffix: u32 = if tc > 10 && t1 < 3 { 1 } else { 0 };
                        let mut levels = Vec::new();
                        for i in 0..tc {
                            let level_start = r.bit_position();
                            if i < t1 {
                                let sign = r.read_bit().unwrap();
                                let lv = if sign == 0 { 1i32 } else { -1 };
                                levels.push(lv);
                                if trace_block { println!("  t1[{i}] sign={sign} level={lv} bits=1 pos={level_start}→{}", r.bit_position()); }
                                continue;
                            }
                            let mut prefix: u32 = 0;
                            loop {
                                let b = r.read_bit().unwrap();
                                if b == 1 { break; }
                                prefix += 1;
                            }
                            let ssize = if prefix == 14 && suffix == 0 {
                                4
                            } else if prefix >= 15 {
                                prefix - 3
                            } else {
                                suffix
                            };
                            let sfx = if ssize > 0 {
                                r.read_bits(ssize as u8).unwrap() as i32
                            } else {
                                0
                            };
                            let mut lc = (prefix.min(15) << suffix) as i32 + sfx;
                            if prefix >= 15 && suffix == 0 { lc += 15; }
                            if prefix >= 16 { lc += (1 << (prefix - 3)) - 4096; }
                            if i == t1 && t1 < 3 { lc += 2; }
                            let level = if lc % 2 == 0 { (lc + 2) >> 1 } else { (-lc - 1) >> 1 };
                            levels.push(level);
                            let old_suffix = suffix;
                            if suffix == 0 { suffix = 1; }
                            if level.unsigned_abs() > (3u32 << (suffix - 1)) && suffix < 6 {
                                suffix += 1;
                            }
                            if trace_block {
                                println!("  lv[{i}] prefix={prefix} ssize={ssize} sfx={sfx} lc={lc} level={level} suf:{old_suffix}→{suffix} bits={} pos={level_start}→{}", r.bit_position()-level_start, r.bit_position());
                            }
                        }
                        let pos_after_levels = r.bit_position();
                        let tz = if tc < 16 {
                            tpt_kinetix_h264::cavlc_tables::read_total_zeros_4x4(&mut r, tc as u8).unwrap()
                        } else {
                            0
                        };
                        let pos_after_tz = r.bit_position();
                        let mut zl = tz as u32;
                        for _ in 0..tc.saturating_sub(1) {
                            let rb = if zl > 0 {
                                tpt_kinetix_h264::cavlc_tables::read_run_before(&mut r, zl.min(255) as u8)
                                    .unwrap() as u32
                            } else {
                                0
                            };
                            zl = zl.saturating_sub(rb);
                        }
                        let pos_after = r.bit_position();
                        println!(
                            "block={block:2} pos={pos_before:3} nc={nc:2} bits={peek_bits} => tc={tc} t1={t1} tz={tz} token_bits={} level_bits={} tz_bits={} run_bits={} total_bits={}",
                            pos_after_token - pos_before,
                            pos_after_levels - pos_after_token,
                            pos_after_tz - pos_after_levels,
                            pos_after - pos_after_tz,
                            pos_after - pos_before,
                        );
                        nz[block] = tc as u8;
                    }
                }
            }
            println!("reached end: pos={}", r.bit_position());
        }
    }
}

fn raster_of_8x8_sub(blk8: usize, sub: usize) -> usize {
    let gx = (blk8 % 2) * 2;
    let gy = (blk8 / 2) * 2;
    let sx = sub % 2;
    let sy = sub / 2;
    (gy + sy) * 4 + (gx + sx)
}

fn read_ue(r: &mut BitReader) -> u32 {
    r.read_ue().unwrap()
}

fn read_se(r: &mut BitReader) -> i32 {
    r.read_se().unwrap()
}
