//! Independent-engine lockstep replay of `mbaff_ip`'s CABAC P slice through
//! MB9's `mb_type`, to pin the CABAC MBAFF P desync (todo-h264 #32aa).
//!
//! Drives BOTH a from-scratch port of ffmpeg's `get_cabac` / `get_cabac_terminate`
//! (tables parsed straight from the vendored `cabac_ref.c`, **with the u8-wrap
//! fix** — ffmpeg stores RangeLPS ≥ 128 as negative `int8` literals in a
//! `uint8_t` table; `dbg_engine_diff.rs` parses them as `i32` and guards
//! around them, so its `FfEngine` has never been validated for a large-range
//! low-pStateIdx decode — which is exactly MB0's first skip bin here) AND the
//! crate's `CabacDecoder`, sharing one logical context model, through ffmpeg's
//! exact P-slice FRAME_MBAFF element sequence:
//!
//!   4 fully-skipped pairs -> per pair: skip(ctx11) skip(ctx11) terminate
//!   pair 4 (MB8 skip, MB9 coded): MB8 skip(ctx11), MB9 skip(ctx11),
//!                                 mb_field_decoding_flag(ctx70)
//!   MB9 mb_type: ctx14, ctx15, ctx16   (ctx indices confirmed vs the crate
//!               parser's own KINETIX_BINTRACE for this clip)
//!
//! RESULT (todo-h264 #32ab): the two engines agree bin-for-bin. With the
//! `mb_field_decoding_flag` context (ctxIdx 70) initialised from the **PB**
//! table (ffmpeg's behaviour for a P slice), ctx15 decodes **1** → 16x8,
//! matching ffmpeg's real decode (`-flags2 +export_mvs`). With `ctxIdx 70`
//! from the **I** table (`ORACLE_FIELD_I_INIT=1` — what the crate's
//! `MbFieldDecodingFlagContext::new` did before the fix), ctx15 decodes
//! **0** → P_8x8, reproducing the crate parser's broken output. The engine
//! offset stays identical across both; only `range` drifts. → the crate was
//! initialising ctxIdx 70..=72 from the wrong (I-slice) init table for P/B
//! slices. Fixed via `MbFieldDecodingFlagContext::new_pb`.
//!
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_mbaff_p_ffengine_oracle -- --nocapture
//!  (`ORACLE_FIELD_I_INIT=1` to reproduce the pre-fix bug)

use std::process::Command;

use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::cabac_tables::CABAC_CTX_INIT_PB0;
use tpt_kinetix_h264::entropy::{CabacContext, CabacDecoder};
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::slice::SliceHeaderContext;
use tpt_kinetix_h264::sps::SeqParameterSet;

const CABAC_BITS: i32 = 16;
const CABAC_MASK: i32 = (1 << CABAC_BITS) - 1;

struct FfTables {
    norm_shift: Vec<i32>,
    lps_range: Vec<i32>, // parsed as u8 (0..=255), ffmpeg semantics
    mlps_state: Vec<i32>,
}
impl FfTables {
    fn parse(path: &std::path::Path) -> Self {
        let src = std::fs::read_to_string(path).expect("cabac_ref.c");
        let start = src.find("ff_h264_cabac_tables").expect("symbol");
        let open = src[start..].find('{').unwrap() + start;
        let close = src[open..].find('}').unwrap() + open;
        let body = &src[open + 1..close];
        let mut nums = Vec::new();
        for line in body.lines() {
            let line = match line.find("//") {
                Some(p) => &line[..p],
                None => line,
            };
            for tok in line.split(|c: char| !(c.is_ascii_digit() || c == '-')) {
                if !tok.is_empty() && tok.parse::<i64>().is_ok() {
                    // The table is `const uint8_t[]`; negative int8 literals
                    // (e.g. -128, -51) are the wrapped forms of 128, 205, …
                    nums.push((tok.parse::<i64>().unwrap() & 0xFF) as i32);
                }
            }
        }
        assert!(nums.len() >= 1280);
        FfTables {
            norm_shift: nums[..512].to_vec(),
            lps_range: nums[512..1024].to_vec(),
            mlps_state: nums[1024..1280].to_vec(),
        }
    }
}

struct FfEngine<'a> {
    low: i32,
    range: i32,
    data: &'a [u8],
    idx: usize,
    t: &'a FfTables,
}
impl<'a> FfEngine<'a> {
    fn new(data: &'a [u8], t: &'a FfTables) -> Self {
        let mut e = FfEngine { low: 0, range: 0, data, idx: 0, t };
        e.low = (e.byte() as i32) << 18;
        e.low += (e.byte() as i32) << 10;
        e.low += 1 << 9; // even-alignment branch
        e.range = 0x1FE;
        e
    }
    fn byte(&mut self) -> u8 {
        let b = self.data.get(self.idx).copied().unwrap_or(0);
        self.idx += 1;
        b
    }
    fn refill(&mut self) {
        let b0 = self.data.get(self.idx).copied().unwrap_or(0);
        let b1 = self.data.get(self.idx + 1).copied().unwrap_or(0);
        self.low = self.low.wrapping_add(((b0 as i32) << 9) + ((b1 as i32) << 1));
        self.low -= CABAC_MASK;
        self.idx += (CABAC_BITS / 8) as usize;
    }
    fn refill2(&mut self) {
        let i = (self.low.trailing_zeros() as i32) - CABAC_BITS;
        let b0 = self.data.get(self.idx).copied().unwrap_or(0);
        let b1 = self.data.get(self.idx + 1).copied().unwrap_or(0);
        let x = (-CABAC_MASK).wrapping_add(((b0 as i32) << 9) + ((b1 as i32) << 1));
        self.low = self.low.wrapping_add(x.wrapping_shl(i as u32));
        self.idx += (CABAC_BITS / 8) as usize;
    }
    fn get(&mut self, state: &mut u8) -> i32 {
        let s0 = *state as i32;
        let li = (2 * (self.range & 0xC0) + s0) as usize;
        let range_lps = self.t.lps_range[li];
        self.range -= range_lps;
        let lps_mask = ((self.range << (CABAC_BITS + 1)) - self.low) >> 31;
        self.low -= (self.range << (CABAC_BITS + 1)) & lps_mask;
        self.range += (range_lps - self.range) & lps_mask;
        let s = s0 ^ lps_mask;
        *state = self.t.mlps_state[(128 + s) as usize] as u8;
        let bit = s & 1;
        let shift = self.t.norm_shift[self.range as usize];
        self.range <<= shift;
        self.low <<= shift;
        if (self.low & CABAC_MASK) == 0 {
            self.refill2();
        }
        bit
    }
    fn terminate(&mut self) -> bool {
        self.range -= 2;
        if self.low < self.range << (CABAC_BITS + 1) {
            let shift = ((self.range - 0x100) as u32) >> 31;
            self.range <<= shift;
            self.low <<= shift;
            if (self.low & CABAC_MASK) == 0 {
                self.refill();
            }
            false
        } else {
            true
        }
    }
}

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg").arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn cabac_payload() -> Option<(Vec<u8>, i32)> {
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_ffeng_oracle");
    std::fs::create_dir_all(&dir).ok()?;
    let h = dir.join("mbaff_ip.h264");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i",
            "testsrc=size=64x64:rate=1:duration=2", "-c:v", "libx264", "-pix_fmt", "yuv420p",
            "-x264-params",
            "cabac=1:bframes=0:keyint=300:min-keyint=300:deblock=0:interlaced=1:tff=1:threads=1",
            h.to_str()?,
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }
    let annexb = std::fs::read(&h).ok()?;
    let units = parse_nal_units_from_annexb(&annexb);
    let sps = units.iter().find(|u| u.nal_unit_type == NalUnitType::Sps)
        .and_then(|u| SeqParameterSet::parse(&u.rbsp).ok())?;
    let pps = units.iter().find(|u| u.nal_unit_type == NalUnitType::Pps)
        .and_then(|u| PicParameterSet::parse(&u.rbsp, None).ok())?;
    let p = units.iter().find(|u| u.nal_unit_type == NalUnitType::NonIdrSlice)?;
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
        chroma_array_type: if sps.separate_colour_plane_flag { 0 } else { sps.chroma_format_idc },
    };
    let header = tpt_kinetix_h264::slice::SliceHeader::parse_with_context(
        &p.rbsp, p.nal_unit_type, p.nal_ref_idc, &ctx,
    )
    .ok()?;
    let slice_qp = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;
    eprintln!("data_bit_offset={} slice_qp={slice_qp}", header.data_bit_offset);
    let mut r = BitReader::new(&p.rbsp);
    r.seek_to_bit(header.data_bit_offset);
    r.byte_align();
    Some((r.remaining_bytes().to_vec(), slice_qp))
}

#[test]
fn mbaff_ip_p_engine_lockstep_through_mb9_mb_type() {
    if !ffmpeg_available() {
        eprintln!("no ffmpeg; skipping");
        return;
    }
    let (payload, slice_qp) = match cabac_payload() {
        Some(v) => v,
        None => {
            eprintln!("payload extraction failed");
            return;
        }
    };
    eprintln!(
        "P-CABAC payload[{}]: {}",
        payload.len(),
        payload.iter().take(16).map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
    );

    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../cabac_ref.c");
    let t = FfTables::parse(&src);

    // ctxIdx 70-72 (mb_field_decoding_flag): the crate's
    // `MbFieldDecodingFlagContext::new` currently inits these from the
    // *I-slice* table regardless of slice type. Toggle to reproduce.
    let field_ctx_from_i = std::env::var("ORACLE_FIELD_I_INIT").is_ok();
    let i_tab = tpt_kinetix_h264::cabac_tables::CABAC_CTX_INIT_I;
    let mut crate_ctx: Vec<CabacContext> = (0..1024)
        .map(|i| {
            let (m, n) = if field_ctx_from_i && (70..=72).contains(&i) {
                i_tab[i]
            } else {
                CABAC_CTX_INIT_PB0[i]
            };
            CabacContext::init(m as i32, n as i32, slice_qp)
        })
        .collect();
    let mut ff_states: Vec<u8> =
        crate_ctx.iter().map(|c| (c.state << 1) | (c.mps & 1)).collect();

    let mut padded = payload.clone();
    padded.extend(std::iter::repeat_n(0u8, 64));
    let mut ff = FfEngine::new(&padded, &t);
    let mut dec = CabacDecoder::new(&padded).expect("crate cabac init");

    eprintln!(
        "ctx11 init: crate(pi={},mps={}) ff_packed={}   ctx70 crate(pi={},mps={})   ctx14 (pi={},mps={})  ctx15 (pi={},mps={})",
        crate_ctx[11].state, crate_ctx[11].mps, ff_states[11],
        crate_ctx[70].state, crate_ctx[70].mps,
        crate_ctx[14].state, crate_ctx[14].mps,
        crate_ctx[15].state, crate_ctx[15].mps,
    );

    #[rustfmt::skip]
    let seq: &[(&str, i32)] = &[
        ("MB0.skip", 11), ("MB1.skip", 11), ("T(pair0)", -1),
        ("MB2.skip", 11), ("MB3.skip", 11), ("T(pair1)", -1),
        ("MB4.skip", 11), ("MB5.skip", 11), ("T(pair2)", -1),
        ("MB6.skip", 11), ("MB7.skip", 11), ("T(pair3)", -1),
        ("MB8.skip", 11), ("MB9.skip", 11), ("MB9.field(ctx70)", 70),
        ("MB9.mbtype.b0(ctx14)", 14),
        ("MB9.mbtype.b1(ctx15)", 15),
    ];

    let mut diverged = false;
    let mut last_bin = -1i32;
    for (label, ctx) in seq {
        if *ctx < 0 {
            let f = ff.terminate();
            let c = dec.decode_terminate() == 1;
            eprintln!("{label:<24} ff_end={} crate_end={}", f as i32, c as i32);
            if f != c {
                eprintln!("  >>> TERMINATE DIVERGENCE at {label}");
                diverged = true;
                break;
            }
        } else {
            let ci = *ctx as usize;
            let (pi, mps) = (crate_ctx[ci].state, crate_ctx[ci].mps);
            let pre = dec.debug_state();
            let fbin = ff.get(&mut ff_states[ci]);
            let cbin = dec.decode_decision(&mut crate_ctx[ci]) as i32;
            last_bin = cbin;
            eprintln!(
                "{label:<24} ctx{ci} (pi={pi},mps={mps})  ff_bin={fbin} crate_bin={cbin}  crate_engine_pre={:#06x}/{:#010x}",
                pre.0, pre.1
            );
            if fbin != cbin {
                eprintln!("  >>> BIN DIVERGENCE at {label}: ff={fbin} crate={cbin}  <<<");
                diverged = true;
                break;
            }
        }
    }

    if !diverged {
        eprintln!(
            "\n== engines AGREED bin-for-bin through MB9 mb_type bin-1 (ctx15 = {last_bin}) =="
        );
        if last_bin == 0 {
            eprintln!(
                "ctx15 == 0 on BOTH ⇒ ffmpeg also takes the P_L0_16x16/P_8x8 branch. \
                 The 'MB9 = 16x8' from export_mvs must be a different MB, OR the desync \
                 is a WRONG CONTEXT / element BEFORE ctx15 that this transcription also has."
            );
        } else {
            eprintln!(
                "ctx15 == 1 on BOTH ⇒ ffmpeg takes the 16x8/8x16 branch. The crate PARSER \
                 decodes ctx15 -> 0 (P_8x8) here, so `parse_p_macroblock_cabac` reads a \
                 different bin sequence than this ffmpeg transcription — an element-order \
                 or context bug in the crate parser, NOT the engine."
            );
        }
    }
}
