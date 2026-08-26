//! Manual, spec-faithful reference parse of a CAVLC P slice, used to locate
#![allow(warnings)]
//! where `parse_p_slice` desynchronises from the true bitstream. Mirrors
//! ITU-T H.264 §7.3.4 / §7.3.5 / §9.2 exactly (not by copying the decoder's
//! own parser) and logs the bit position at each parsing step so it can be
//! diffed against the decoder's `P-MB(..)` trace.

use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::nal::{self, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::sps::SeqParameterSet;

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;

fn gen() -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_pref");
    std::fs::create_dir_all(&dir).ok()?;
    let h264 = dir.join("ip.h264");
    let ok = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x48:rate=1:duration=2",
            "-frames:v",
            "2",
            "-c:v",
            "libx264",
            "-profile:v",
            "baseline",
            "-g",
            "2",
            "-bf",
            "0",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:deblock=0:keyint=2:min-keyint=2",
            h264.to_str()?,
        ])
        .output()
        .ok()?;
    if !ok.status.success() {
        eprintln!(
            "ffmpeg encode failed: {}",
            String::from_utf8_lossy(&ok.stderr)
        );
        return None;
    }
    std::fs::read(&h264).ok()
}

#[test]
fn reference_parse_p_slice() {
    let annexb = match gen() {
        Some(b) => b,
        None => {
            eprintln!("reference_parse_p_slice: skipped (ffmpeg unavailable or encode failed)");
            return;
        }
    };
    let units = nal::parse_nal_units_from_annexb(&annexb);

    let sps = units
        .iter()
        .find(|u| u.nal_unit_type == NalUnitType::Sps)
        .unwrap();
    let sps = SeqParameterSet::parse(&sps.rbsp).expect("sps");
    let pps = units
        .iter()
        .find(|u| u.nal_unit_type == NalUnitType::Pps)
        .unwrap();
    let pps = PicParameterSet::parse(&pps.rbsp, None).expect("pps");

    let p = units
        .iter()
        .find(|u| u.nal_unit_type == NalUnitType::NonIdrSlice)
        .expect("P slice");

    let mb_cols = WIDTH.div_ceil(16);
    let mb_rows = HEIGHT.div_ceil(16);
    let total = (mb_cols * mb_rows) as usize;

    let mut r = BitReader::new(&p.rbsp);
    // Minimal slice-header walk to reach data_bit_offset (mirrors §7.3.3).
    let _first_mb = r.read_ue().unwrap();
    let slice_type_raw = r.read_ue().unwrap();
    let slice_type = slice_type_raw % 5;
    let _pps_id = r.read_ue().unwrap();
    let frame_num_bits = (sps.log2_max_frame_num_minus4 + 4) as u8;
    let _frame_num = r.read_bits(frame_num_bits).unwrap();
    if !sps.frame_mbs_only_flag {
        let _field = r.read_bit().unwrap();
    }
    if sps.pic_order_cnt_type == 0 {
        let bits = (sps.log2_max_pic_order_cnt_lsb_minus4 + 4) as u8;
        let _poc_lsb = r.read_bits(bits).unwrap();
        if pps.bottom_field_pic_order_in_frame_present_flag && sps.frame_mbs_only_flag {
            let _d = r.read_se().unwrap();
        }
    }
    if pps.redundant_pic_cnt_present_flag {
        let _ = r.read_ue().unwrap();
    }
    if slice_type == 1 {
        let _ = r.read_bit().unwrap();
    }
    if slice_type == 0 || slice_type == 1 || slice_type == 3 {
        let ovr = r.read_bit().unwrap() == 1;
        if ovr {
            let _ = r.read_ue().unwrap();
            if slice_type == 1 {
                let _ = r.read_ue().unwrap();
            }
        }
    }
    if slice_type == 0 || slice_type == 1 || slice_type == 3 {
        // ref_pic_list_modification l0 (§7.3.3.1)
        let flag_l0 = r.read_bit().unwrap() == 1;
        if flag_l0 {
            loop {
                let op = r.read_ue().unwrap();
                if op == 3 {
                    break;
                }
                if op == 0 || op == 1 {
                    let _ = r.read_ue().unwrap();
                } else if op == 2 {
                    let _ = r.read_ue().unwrap();
                }
            }
        }
    }
    if slice_type == 1 {
        // l1
        let flag_l1 = r.read_bit().unwrap() == 1;
        if flag_l1 {
            loop {
                let op = r.read_ue().unwrap();
                if op == 3 {
                    break;
                }
            }
        }
    }
    if pps.weighted_pred_flag && (slice_type == 0 || slice_type == 3) {
        let _ = r.read_ue().unwrap();
        let _ = r.read_se().unwrap();
        if slice_type == 3 {
            let _ = r.read_se().unwrap();
        }
    }
    let nal_ref_idc = p.nal_ref_idc;
    if nal_ref_idc != 0 {
        // dec_ref_pic_marking (§7.3.3.3) — non-IDR: only the adaptive flag.
        let adaptive = r.read_bit().unwrap() == 1;
        if adaptive {
            loop {
                let op = r.read_ue().unwrap();
                if op == 0 {
                    break;
                }
                if op == 1 || op == 3 {
                    let _ = r.read_ue().unwrap();
                } else if op == 2 {
                    let _ = r.read_ue().unwrap();
                } else if op == 3 || op == 6 {
                    let _ = r.read_ue().unwrap();
                } else if op == 4 {
                    let _ = r.read_ue().unwrap();
                }
            }
        }
    }
    let _slice_qp_delta = r.read_se().unwrap();
    if pps.deblocking_filter_control_present_flag {
        let idc = r.read_ue().unwrap();
        if idc != 1 {
            let _ = r.read_se().unwrap();
            let _ = r.read_se().unwrap();
        }
    }

    let data_bit_offset = r.bit_position();
    eprintln!("REF: data_bit_offset={data_bit_offset}");

    let mut mb_skip_run: Option<u32> = None;
    let qp = 26 + pps.pic_init_qp_minus26 + 0; // slice_qp_delta handled above
    let _ = qp;
    for mb_idx in 0..total {
        let mb_x = (mb_idx as u32) % mb_cols;
        let mb_y = (mb_idx as u32) / mb_cols;
        if mb_skip_run.is_none() {
            let run = r.read_ue().unwrap();
            mb_skip_run = Some(run);
        }
        let run = mb_skip_run.as_mut().unwrap();
        if *run > 0 {
            *run -= 1;
            eprintln!("REF MB({mb_x},{mb_y}): SKIP");
            continue;
        }
        mb_skip_run = None;
        let start = r.bit_position();
        let mb_type = r.read_ue().unwrap();
        eprintln!("REF MB({mb_x},{mb_y}): mb_type={mb_type} start_bit={start}");
        if mb_type >= 5 {
            // intra-in-P: skip detailed parse, just note
            eprintln!("    intra-in-P, mb_type={mb_type}");
            continue;
        }
        // motion
        let num_ref = pps.num_ref_idx_l0_default_active_minus1 + 1;
        if mb_type == 3 || mb_type == 4 {
            let ref_count = if mb_type == 4 { 1 } else { num_ref };
            let mut subs = [0u32; 4];
            for s in &mut subs {
                *s = r.read_ue().unwrap();
            }
            for &sub in &subs {
                let _ref = if ref_count == 1 {
                    0u32
                } else if ref_count == 2 {
                    r.read_bit().unwrap() as u32 ^ 1
                } else {
                    r.read_ue().unwrap()
                };
                let n_sub = match sub {
                    0 => 1,
                    1 => 2,
                    2 => 2,
                    _ => 4,
                };
                for _ in 0..n_sub {
                    let _mx = r.read_se().unwrap();
                    let _my = r.read_se().unwrap();
                }
            }
        } else {
            let n_parts = if mb_type == 0 { 1 } else { 2 };
            for _ in 0..n_parts {
                let _ref = if num_ref == 1 {
                    0u32
                } else if num_ref == 2 {
                    r.read_bit().unwrap() as u32 ^ 1
                } else {
                    r.read_ue().unwrap()
                };
                let _mx = r.read_se().unwrap();
                let _my = r.read_se().unwrap();
            }
        }
        let cbp_code = r.read_ue().unwrap();
        eprintln!(
            "    cbp_code_num={cbp_code} pos_after_cbp={}",
            r.bit_position()
        );
        let cbp = [
            0u32, 1, 2, 4, 8, 3, 5, 10, 12, 15, 7, 11, 13, 14, 6, 9, 16, 17, 18, 20, 24, 19, 21,
            26, 28, 31, 23, 27, 29, 30, 22, 25, 32, 33, 34, 36, 40, 35, 37, 42, 44, 47, 39, 43, 45,
            46, 38, 41,
        ][cbp_code as usize];
        let cbp_l = cbp & 0x0F;
        let cbp_c = cbp >> 4;
        if cbp_l != 0 || cbp_c != 0 {
            let _dqp = r.read_se().unwrap();
        }
        eprintln!("    cbp_l={cbp_l:#x} cbp_c={cbp_c}");
    }
}
