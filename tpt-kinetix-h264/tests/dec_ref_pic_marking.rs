//! End-to-end check that `dec_ref_pic_marking` (§7.3.3.3) actually reaches
//! DPB marking (§8.2.5), rather than being parsed and discarded.
//!
//! The unit tests in `ref_pic.rs` cover the marking process itself against
//! hand-derived `picNumX` values; this test drives it from real slice-header
//! bitstream syntax, so a regression that re-severs the wiring (e.g. dropping
//! the header field again, or mis-sizing an MMCO operand) fails here even if
//! `Dpb::mark_decoded_picture` stays correct in isolation.
//!
//! The final section goes one level further out and drives whole Annex B
//! access units through the public [`H264Decoder::decode`] API, so the
//! `H264Decoder::store_reference_picture` wiring (which is what a real stream
//! goes through) is covered too, not just `SliceHeader` → `Dpb`.

use tpt_kinetix_core::{
    frame::VideoFrame, packet::Packet, pixel_format::PixelFormat, timestamp::Timestamp,
};
use tpt_kinetix_h264::nal::NalUnitType;
use tpt_kinetix_h264::ref_pic::{build_ref_list_l0, Dpb, DpbEntry, MmcoError, PicNumContext};
use tpt_kinetix_h264::slice::{DecRefPicMarking, MmcoOp, SliceHeader, SliceHeaderContext};
use tpt_kinetix_h264::H264Decoder;

/// `MaxFrameNum` for every stream synthesised here
/// (`log2_max_frame_num_minus4 == 0`).
const MAX_FRAME_NUM: u32 = 16;

/// Minimal MSB-first bit writer for synthesising a slice header.
struct BitWriter {
    buf: Vec<u8>,
    cur: u8,
    nbits: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            cur: 0,
            nbits: 0,
        }
    }

    fn bit(&mut self, b: u8) {
        self.cur = (self.cur << 1) | (b & 1);
        self.nbits += 1;
        if self.nbits == 8 {
            self.buf.push(self.cur);
            self.cur = 0;
            self.nbits = 0;
        }
    }

    fn bits(&mut self, value: u32, count: u8) {
        for i in (0..count).rev() {
            self.bit(((value >> i) & 1) as u8);
        }
    }

    fn ue(&mut self, v: u32) {
        let code = v + 1;
        let leading = 31 - code.leading_zeros();
        for _ in 0..leading {
            self.bit(0);
        }
        for i in (0..=leading).rev() {
            self.bit(((code >> i) & 1) as u8);
        }
    }

    fn se(&mut self, v: i32) {
        self.ue(if v <= 0 {
            (-2 * v) as u32
        } else {
            (2 * v - 1) as u32
        });
    }

    fn finish(mut self) -> Vec<u8> {
        self.bit(1);
        while self.nbits != 0 {
            self.bit(0);
        }
        self.buf
    }
}

fn ctx() -> SliceHeaderContext {
    SliceHeaderContext {
        log2_max_frame_num_minus4: 0, // MaxFrameNum = 16
        pic_order_cnt_type: 0,
        log2_max_pic_order_cnt_lsb_minus4: 0,
        frame_mbs_only_flag: true,
        bottom_field_pic_order_in_frame_present_flag: false,
        delta_pic_order_always_zero_flag: false,
        num_ref_idx_l0_default_active_minus1: 0,
        num_ref_idx_l1_default_active_minus1: 0,
        weighted_pred_flag: false,
        weighted_bipred_idc: 0,
        entropy_coding_mode_flag: false,
        deblocking_filter_control_present_flag: false,
        redundant_pic_cnt_present_flag: false,
        num_slice_groups_minus1: 0,
        chroma_array_type: 1,
    }
}

/// A non-IDR reference P slice with the given `frame_num`, whose
/// `dec_ref_pic_marking` carries `mmco` (each inner slice is one
/// `memory_management_control_operation` followed by its Table 7-9 operands).
/// An empty `mmco` writes `adaptive_ref_pic_marking_mode_flag == 0`.
fn p_slice_rbsp(frame_num: u32, mmco: &[&[u32]]) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.ue(0); // first_mb_in_slice
    w.ue(0); // slice_type = P
    w.ue(0); // pic_parameter_set_id
    w.bits(frame_num, 4);
    w.bits(frame_num * 2 % 16, 4); // pic_order_cnt_lsb
    w.bit(0); // num_ref_idx_active_override_flag
    w.bit(0); // ref_pic_list_modification_flag_l0
    if mmco.is_empty() {
        w.bit(0); // adaptive_ref_pic_marking_mode_flag
    } else {
        w.bit(1);
        for command in mmco {
            for &value in *command {
                w.ue(value);
            }
        }
        w.ue(0); // terminator
    }
    w.se(0); // slice_qp_delta
    w.finish()
}

/// An IDR slice with the given `long_term_reference_flag`.
fn idr_slice_rbsp(long_term_reference_flag: bool) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.ue(0); // first_mb_in_slice
    w.ue(2); // slice_type = I
    w.ue(0); // pic_parameter_set_id
    w.bits(0, 4); // frame_num
    w.ue(0); // idr_pic_id
    w.bits(0, 4); // pic_order_cnt_lsb
    w.bit(0); // no_output_of_prior_pics_flag
    w.bit(long_term_reference_flag as u8); // long_term_reference_flag
    w.se(0); // slice_qp_delta
    w.finish()
}

fn parse(rbsp: &[u8], nal_unit_type: NalUnitType) -> SliceHeader {
    SliceHeader::parse_with_context(rbsp, nal_unit_type, 1, &ctx()).expect("slice header")
}

fn pic_num_ctx(frame_num: u32) -> PicNumContext {
    PicNumContext {
        curr_frame_num: frame_num,
        max_frame_num: MAX_FRAME_NUM,
        field_pic_flag: false,
        bottom_field_flag: false,
    }
}

/// A decoded picture whose luma is filled with `frame_num`, so a reference list
/// built from the DPB is observable in the pixel data a decoder would predict
/// from.
fn decoded_picture(frame_num: u32) -> DpbEntry {
    DpbEntry {
        frame: VideoFrame {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: vec![frame_num as u8; 16 * 16 * 3 / 2],
            width: 16,
            height: 16,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: false,
        },
        frame_num,
        field_pic_flag: false,
        bottom_field_flag: false,
        pic_order_cnt: frame_num as i64 * 2,
        is_short_term: true,
        is_long_term: false,
        long_term_pic_num: -1,
        mv_grid: None,
    }
}

/// Decode a slice header and run the marking process it asks for, exactly as
/// `H264Decoder::store_reference_picture` does.
fn store(dpb: &mut Dpb, header: &SliceHeader, max_num_ref_frames: u32) -> Result<(), MmcoError> {
    let marking = header
        .dec_ref_pic_marking
        .clone()
        .expect("reference slice must carry dec_ref_pic_marking");
    dpb.mark_decoded_picture(
        decoded_picture(header.frame_num),
        &marking,
        pic_num_ctx(header.frame_num),
        max_num_ref_frames,
    )
    .map(|_| ())
}

fn dpb_frame_nums(dpb: &Dpb) -> Vec<u32> {
    dpb.iter().map(|e| e.frame_num).collect()
}

/// MMCO 1 read from real slice-header syntax removes exactly the picture the
/// stream named, and nothing else.
#[test]
fn mmco1_from_bitstream_marks_one_short_term_picture_unused() {
    let mut dpb = Dpb::new();
    for frame_num in 0..3u32 {
        let header = parse(&p_slice_rbsp(frame_num, &[]), NalUnitType::NonIdrSlice);
        assert_eq!(
            header.dec_ref_pic_marking,
            Some(DecRefPicMarking::SlidingWindow)
        );
        store(&mut dpb, &header, 4).expect("sliding window");
    }
    assert_eq!(dpb_frame_nums(&dpb), vec![0, 1, 2]);

    // CurrPicNum = 3, difference_of_pic_nums_minus1 = 1 → picNumX = 1.
    let header = parse(&p_slice_rbsp(3, &[&[1, 1]]), NalUnitType::NonIdrSlice);
    assert_eq!(
        header.dec_ref_pic_marking,
        Some(DecRefPicMarking::Adaptive(vec![MmcoOp::ShortTermUnused {
            difference_of_pic_nums_minus1: 1
        }]))
    );
    store(&mut dpb, &header, 4).expect("mmco 1");

    assert_eq!(dpb_frame_nums(&dpb), vec![0, 2, 3]);
    // ...and the reference list a following P slice would build no longer
    // contains the dropped picture's pixels.
    let list = build_ref_list_l0(&dpb, 3, pic_num_ctx(4), &[]).expect("ref list");
    let luma: Vec<u8> = list.iter().map(|e| e.frame.data[0]).collect();
    assert_eq!(luma, vec![3, 2, 0]);
}

/// MMCO 5 read from real slice-header syntax empties the DPB and renumbers the
/// current picture to `frame_num == 0`.
#[test]
fn mmco5_from_bitstream_resets_the_dpb() {
    let mut dpb = Dpb::new();
    for frame_num in 0..3u32 {
        let header = parse(&p_slice_rbsp(frame_num, &[]), NalUnitType::NonIdrSlice);
        store(&mut dpb, &header, 4).expect("sliding window");
    }
    assert_eq!(dpb.len(), 3);

    let header = parse(&p_slice_rbsp(3, &[&[5]]), NalUnitType::NonIdrSlice);
    assert_eq!(
        header.dec_ref_pic_marking,
        Some(DecRefPicMarking::Adaptive(vec![MmcoOp::ResetAll]))
    );
    store(&mut dpb, &header, 4).expect("mmco 5");

    assert_eq!(dpb.len(), 1);
    let stored = dpb.iter().next().unwrap();
    assert_eq!(stored.frame_num, 0, "MMCO 5 renumbers the current picture");
    assert_eq!(stored.pic_order_cnt, 0);
    // The frame data still belongs to the picture that carried MMCO 5.
    assert_eq!(stored.frame.data[0], 3);
    assert_eq!(dpb.max_long_term_frame_idx(), None);
}

/// MMCO 3 then MMCO 2, both read from real syntax: a short-term picture is
/// promoted to long-term and later released by `LongTermPicNum`.
#[test]
fn mmco3_then_mmco2_round_trips_a_long_term_reference() {
    let mut dpb = Dpb::new();
    for frame_num in 0..2u32 {
        let header = parse(&p_slice_rbsp(frame_num, &[]), NalUnitType::NonIdrSlice);
        store(&mut dpb, &header, 4).expect("sliding window");
    }

    // CurrPicNum = 2, difference_of_pic_nums_minus1 = 1 → picNumX = 0, which
    // becomes long-term with LongTermFrameIdx 2.
    let header = parse(&p_slice_rbsp(2, &[&[3, 1, 2]]), NalUnitType::NonIdrSlice);
    store(&mut dpb, &header, 4).expect("mmco 3");
    assert_eq!(dpb.num_long_term(), 1);
    let promoted = dpb.iter().find(|e| e.frame_num == 0).expect("frame 0");
    assert!(promoted.is_long_term);
    assert_eq!(promoted.long_term_pic_num, 2);

    // MMCO 2 releases it again by LongTermPicNum.
    let header = parse(&p_slice_rbsp(3, &[&[2, 2]]), NalUnitType::NonIdrSlice);
    store(&mut dpb, &header, 4).expect("mmco 2");
    assert_eq!(dpb.num_long_term(), 0);
    assert!(!dpb.iter().any(|e| e.frame_num == 0));
}

/// MMCO 6 in a P-slice header claims the *current* picture as long-term.
#[test]
fn mmco6_from_bitstream_marks_the_current_picture_long_term() {
    let mut dpb = Dpb::new();
    let header = parse(&p_slice_rbsp(0, &[]), NalUnitType::NonIdrSlice);
    store(&mut dpb, &header, 4).expect("sliding window");

    let header = parse(&p_slice_rbsp(1, &[&[6, 3]]), NalUnitType::NonIdrSlice);
    store(&mut dpb, &header, 4).expect("mmco 6");

    let current = dpb.iter().find(|e| e.frame_num == 1).expect("current");
    assert!(current.is_long_term);
    assert_eq!(current.long_term_pic_num, 3);
}

/// §8.2.5.1: an IDR with `long_term_reference_flag == 1` empties the DPB and
/// stores itself as a long-term reference with `LongTermFrameIdx == 0`.
#[test]
fn idr_long_term_reference_flag_from_bitstream() {
    let mut dpb = Dpb::new();
    for frame_num in 0..3u32 {
        let header = parse(&p_slice_rbsp(frame_num, &[]), NalUnitType::NonIdrSlice);
        store(&mut dpb, &header, 4).expect("sliding window");
    }

    let header = parse(&idr_slice_rbsp(true), NalUnitType::IdrSlice);
    store(&mut dpb, &header, 4).expect("idr");
    assert_eq!(dpb.len(), 1);
    assert!(dpb.iter().next().unwrap().is_long_term);
    assert_eq!(dpb.max_long_term_frame_idx(), Some(0));

    let header = parse(&idr_slice_rbsp(false), NalUnitType::IdrSlice);
    store(&mut dpb, &header, 4).expect("idr");
    assert_eq!(dpb.len(), 1);
    assert!(dpb.iter().next().unwrap().is_short_term);
    assert_eq!(dpb.max_long_term_frame_idx(), None);
}

/// §8.2.5.1: a picture with `adaptive_ref_pic_marking_mode_flag == 1` does not
/// run the sliding-window process, so which picture survives depends on the
/// MMCO commands and not on the DPB being full.
#[test]
fn adaptive_marking_overrides_sliding_window_end_to_end() {
    let mut sliding = Dpb::new();
    let mut adaptive = Dpb::new();
    for frame_num in 0..2u32 {
        let header = parse(&p_slice_rbsp(frame_num, &[]), NalUnitType::NonIdrSlice);
        store(&mut sliding, &header, 2).expect("sliding window");
        store(&mut adaptive, &header, 2).expect("sliding window");
    }

    // Sliding window evicts the smallest FrameNumWrap (frame_num 0)...
    let header = parse(&p_slice_rbsp(2, &[]), NalUnitType::NonIdrSlice);
    store(&mut sliding, &header, 2).expect("sliding window");
    assert_eq!(dpb_frame_nums(&sliding), vec![1, 2]);

    // ...while MMCO 1 naming picNumX = 1 keeps frame_num 0 instead.
    let header = parse(&p_slice_rbsp(2, &[&[1, 0]]), NalUnitType::NonIdrSlice);
    store(&mut adaptive, &header, 2).expect("mmco 1");
    assert_eq!(dpb_frame_nums(&adaptive), vec![0, 2]);
}

/// A command naming a picture that is not in the DPB is malformed; the DPB is
/// emptied so the next slice falls back instead of predicting from a
/// wrongly-marked reference.
#[test]
fn mmco_naming_an_absent_picture_fails_safe_end_to_end() {
    let mut dpb = Dpb::new();
    let header = parse(&p_slice_rbsp(0, &[]), NalUnitType::NonIdrSlice);
    store(&mut dpb, &header, 4).expect("sliding window");

    // CurrPicNum = 1, difference_of_pic_nums_minus1 = 4 → picNumX = −4.
    let header = parse(&p_slice_rbsp(1, &[&[1, 4]]), NalUnitType::NonIdrSlice);
    let err = store(&mut dpb, &header, 4).unwrap_err();
    assert_eq!(err, MmcoError::MissingShortTerm { pic_num: -4 });
    assert!(dpb.is_empty());
    assert!(build_ref_list_l0(&dpb, 1, pic_num_ctx(2), &[]).is_none());
}

// ---- through the public decoder API ------------------------------------
//
// Everything above stops at `SliceHeader` → `Dpb`. The tests below feed whole
// Annex B access units to `H264Decoder::decode`, so the marking has to survive
// the decoder's own `store_reference_picture` step (POC derivation, the
// `nal_ref_idc == 0` gate, the MMCO-5 POC-state reset, and the fail-safe error
// branch) to be observable in `H264Decoder::dpb`.

/// SPS `num_ref_frames`: large enough that sliding-window marking never evicts
/// anything in these tests, so every DPB change is attributable to an MMCO
/// command rather than to §8.2.5.3.
const NUM_REF_FRAMES: u32 = 8;

/// Insert H.264 emulation-prevention `0x03` bytes so the decoder's removal step
/// reproduces the RBSP these builders emitted.
fn add_epb(rbsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rbsp.len());
    let mut zeros = 0u32;
    for &b in rbsp {
        if zeros >= 2 && b <= 0x03 {
            out.push(0x03);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// Wrap an RBSP in a 4-byte start code plus the given NAL header byte.
fn annexb(nal_header: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut v = vec![0x00, 0x00, 0x00, 0x01, nal_header];
    v.extend(add_epb(rbsp));
    v
}

/// Baseline-profile SPS for a single-macroblock (16×16) CAVLC stream, matching
/// the [`ctx`] slice-header context above: `MaxFrameNum` 16,
/// `pic_order_cnt_type` 0, `MaxPicOrderCntLsb` 16.
fn sps_rbsp() -> Vec<u8> {
    let mut w = BitWriter::new();
    w.bits(66, 8); // profile_idc (Baseline)
    w.bits(0, 8); // constraint_set flags + reserved_zero_2bits
    w.bits(30, 8); // level_idc
    w.ue(0); // seq_parameter_set_id
    w.ue(0); // log2_max_frame_num_minus4 → MaxFrameNum = 16
    w.ue(0); // pic_order_cnt_type
    w.ue(0); // log2_max_pic_order_cnt_lsb_minus4 → 4-bit pic_order_cnt_lsb
    w.ue(NUM_REF_FRAMES); // num_ref_frames
    w.bit(0); // gaps_in_frame_num_value_allowed_flag
    w.ue(0); // pic_width_in_mbs_minus1 → 16 px
    w.ue(0); // pic_height_in_map_units_minus1 → 16 px
    w.bit(1); // frame_mbs_only_flag
    w.bit(0); // direct_8x8_inference_flag
    w.bit(0); // frame_cropping_flag
    w.bit(0); // vui_parameters_present_flag
    w.finish()
}

fn pps_rbsp() -> Vec<u8> {
    let mut w = BitWriter::new();
    w.ue(0); // pic_parameter_set_id
    w.ue(0); // seq_parameter_set_id
    w.bit(0); // entropy_coding_mode_flag (CAVLC)
    w.bit(0); // bottom_field_pic_order_in_frame_present_flag
    w.ue(0); // num_slice_groups_minus1
    w.ue(0); // num_ref_idx_l0_default_active_minus1
    w.ue(0); // num_ref_idx_l1_default_active_minus1
    w.bit(0); // weighted_pred_flag
    w.bits(0, 2); // weighted_bipred_idc
    w.se(0); // pic_init_qp_minus26
    w.se(0); // pic_init_qs_minus26
    w.se(0); // chroma_qp_index_offset
    w.bit(0); // deblocking_filter_control_present_flag
    w.bit(0); // constrained_intra_pred_flag
    w.bit(0); // redundant_pic_cnt_present_flag
    w.finish()
}

/// One `I_16x16_2_0_0` macroblock (DC prediction, `CodedBlockPatternLuma` and
/// `CodedBlockPatternChroma` both 0), so the whole picture is a single
/// macroblock that the real CAVLC I-slice path decodes. Only the
/// `Intra16x16DCLevel` `coeff_token` is coded (`TotalCoeff == 0`, `nC < 2` →
/// the single bit `1`), because an Intra_16×16 macroblock always carries its
/// luma DC block regardless of the coded block pattern (§7.3.5.3).
fn i_16x16_macroblock(w: &mut BitWriter) {
    w.ue(3); // mb_type = I_16x16_2_0_0 (Table 7-11)
    w.ue(0); // intra_chroma_pred_mode = DC
    w.se(0); // mb_qp_delta (always present for Intra_16×16)
    w.bit(1); // Intra16x16DCLevel coeff_token: TotalCoeff = 0
}

/// An IDR I slice (`frame_num == 0`) with the given `long_term_reference_flag`.
fn decodable_idr_rbsp(long_term_reference_flag: bool) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.ue(0); // first_mb_in_slice
    w.ue(2); // slice_type = I
    w.ue(0); // pic_parameter_set_id
    w.bits(0, 4); // frame_num
    w.ue(0); // idr_pic_id
    w.bits(0, 4); // pic_order_cnt_lsb
    w.bit(0); // no_output_of_prior_pics_flag
    w.bit(long_term_reference_flag as u8);
    w.se(0); // slice_qp_delta
    i_16x16_macroblock(&mut w);
    w.finish()
}

/// A non-IDR I slice carrying `mmco` (each inner slice is one
/// `memory_management_control_operation` followed by its Table 7-9 operands;
/// an empty `mmco` writes `adaptive_ref_pic_marking_mode_flag == 0`).
///
/// I slices are used rather than P slices so the picture reconstructs without
/// depending on the very reference list the marking process is under test for.
fn decodable_i_slice_rbsp(frame_num: u32, mmco: &[&[u32]]) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.ue(0); // first_mb_in_slice
    w.ue(2); // slice_type = I
    w.ue(0); // pic_parameter_set_id
    w.bits(frame_num, 4);
    w.bits(frame_num * 2 % 16, 4); // pic_order_cnt_lsb
    if mmco.is_empty() {
        w.bit(0); // adaptive_ref_pic_marking_mode_flag
    } else {
        w.bit(1);
        for command in mmco {
            for &value in *command {
                w.ue(value);
            }
        }
        w.ue(0); // terminator
    }
    w.se(0); // slice_qp_delta
    i_16x16_macroblock(&mut w);
    w.finish()
}

/// Feed one access unit (already Annex B framed) to the decoder and require it
/// to produce a picture — a slice that fell back instead of decoding would
/// never reach the marking step, which would make the DPB assertions vacuous.
fn decode_au(dec: &mut H264Decoder, au: Vec<u8>) -> VideoFrame {
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: au,
        stream_index: 0,
        is_key_frame: false,
    };
    dec.decode(&packet)
        .expect("decode must not error")
        .expect("access unit must produce a picture")
}

/// SPS + PPS + IDR, as the first access unit of every decoder test below.
fn decode_stream_start(dec: &mut H264Decoder) {
    let mut au = annexb(0x67, &sps_rbsp());
    au.extend(annexb(0x68, &pps_rbsp()));
    au.extend(annexb(0x65, &decodable_idr_rbsp(false)));
    decode_au(dec, au);
}

/// Decode a non-IDR reference I picture carrying `mmco`.
fn decode_reference_picture(dec: &mut H264Decoder, frame_num: u32, mmco: &[&[u32]]) {
    // nal_ref_idc = 3, nal_unit_type = 1 (non-IDR coded slice).
    decode_au(dec, annexb(0x61, &decodable_i_slice_rbsp(frame_num, mmco)));
}

/// Headline Phase E.2 test #1, at the decoder level: an MMCO 1 command in a
/// real slice header removes exactly the picture it names from the decoder's
/// DPB.
#[test]
fn mmco1_through_the_decoder_marks_a_short_term_picture_unused() {
    let mut dec = H264Decoder::new();
    decode_stream_start(&mut dec);
    decode_reference_picture(&mut dec, 1, &[]);
    decode_reference_picture(&mut dec, 2, &[]);
    assert_eq!(
        dpb_frame_nums(dec.dpb()),
        vec![0, 1, 2],
        "sliding-window marking should keep all three reference pictures"
    );

    // CurrPicNum = 3, difference_of_pic_nums_minus1 = 2 → picNumX = 0.
    decode_reference_picture(&mut dec, 3, &[&[1, 2]]);
    assert_eq!(dpb_frame_nums(dec.dpb()), vec![1, 2, 3]);
    assert_eq!(dec.dpb().num_short_term(), 3);
    assert_eq!(dec.dpb().num_long_term(), 0);
}

/// Headline Phase E.2 test #2, at the decoder level: MMCO 5 empties the
/// decoder's DPB and renumbers the picture that carried it to `frame_num == 0`
/// / `PicOrderCnt == 0` (§8.2.5.4.5, §7.4.3).
#[test]
fn mmco5_through_the_decoder_resets_the_dpb() {
    let mut dec = H264Decoder::new();
    decode_stream_start(&mut dec);
    // pic_order_cnt_lsb climbs 0 → 6 → 8 → 12 in steps below MaxPicOrderCntLsb
    // / 2 (= 8), so no MSB wrap is triggered on the way in and prevPicOrderCnt
    // Lsb is 12 when the MMCO 5 picture is marked.
    decode_reference_picture(&mut dec, 3, &[]);
    decode_reference_picture(&mut dec, 4, &[]);
    assert_eq!(dec.dpb().len(), 3);

    decode_reference_picture(&mut dec, 6, &[&[5]]);
    assert_eq!(dec.dpb().len(), 1, "MMCO 5 drops every prior reference");
    let stored = dec.dpb().iter().next().expect("current picture");
    assert_eq!(stored.frame_num, 0);
    assert_eq!(stored.pic_order_cnt, 0);
    assert!(stored.is_short_term);
    assert_eq!(dec.dpb().max_long_term_frame_idx(), None);

    // §8.2.1.1: the decoder's POC state must have been rebased too, so the
    // next picture's PicOrderCntMsb is derived against the reset predecessor
    // rather than the pre-MMCO-5 one. Without the reset, prevPicOrderCntLsb
    // stays 12 and this picture's lsb of 2 is read as a wrap (12 − 2 >= 8),
    // giving PicOrderCnt 18 instead of 2.
    decode_reference_picture(&mut dec, 1, &[]);
    let pocs: Vec<i64> = dec.dpb().iter().map(|e| e.pic_order_cnt).collect();
    assert_eq!(pocs, vec![0, 2]);
}

/// MMCO 3 (short-term → long-term) and MMCO 6 (current picture → long-term)
/// both reach the decoder's DPB, and MMCO 2 releases the result by
/// `LongTermPicNum`.
#[test]
fn long_term_mmco_commands_reach_the_decoder_dpb() {
    let mut dec = H264Decoder::new();
    decode_stream_start(&mut dec);
    decode_reference_picture(&mut dec, 1, &[]);

    // CurrPicNum = 2, difference_of_pic_nums_minus1 = 1 → picNumX = 0, which
    // becomes long-term with LongTermFrameIdx 2. The same picture also claims
    // itself as long-term with LongTermFrameIdx 5 via MMCO 6.
    decode_reference_picture(&mut dec, 2, &[&[3, 1, 2], &[6, 5]]);
    let long_terms: Vec<(u32, i32)> = dec
        .dpb()
        .iter()
        .filter(|e| e.is_long_term)
        .map(|e| (e.frame_num, e.long_term_pic_num))
        .collect();
    assert_eq!(long_terms, vec![(0, 2), (2, 5)]);
    assert_eq!(dec.dpb().num_short_term(), 1); // frame_num 1

    // MMCO 2 releases LongTermPicNum 2 (the promoted IDR).
    decode_reference_picture(&mut dec, 3, &[&[2, 2]]);
    assert!(!dec.dpb().iter().any(|e| e.frame_num == 0));
    assert_eq!(dec.dpb().num_long_term(), 1);
}

/// §8.2.5.1: an IDR with `long_term_reference_flag == 1` empties the decoder's
/// DPB and stores itself as a long-term reference with `LongTermFrameIdx == 0`.
#[test]
fn idr_long_term_reference_flag_reaches_the_decoder_dpb() {
    let mut dec = H264Decoder::new();
    decode_stream_start(&mut dec);
    decode_reference_picture(&mut dec, 1, &[]);
    assert_eq!(dec.dpb().len(), 2);

    let mut au = annexb(0x67, &sps_rbsp());
    au.extend(annexb(0x68, &pps_rbsp()));
    au.extend(annexb(0x65, &decodable_idr_rbsp(true)));
    decode_au(&mut dec, au);

    assert_eq!(dec.dpb().len(), 1);
    let stored = dec.dpb().iter().next().unwrap();
    assert!(stored.is_long_term);
    assert_eq!(stored.long_term_pic_num, 0);
    assert_eq!(dec.dpb().max_long_term_frame_idx(), Some(0));
}

/// §8.2.5.1: only reference pictures are marked and stored. A slice with
/// `nal_ref_idc == 0` carries no `dec_ref_pic_marking` at all and must leave
/// the DPB untouched even though it decodes fine.
#[test]
fn non_reference_picture_does_not_enter_the_decoder_dpb() {
    let mut dec = H264Decoder::new();
    decode_stream_start(&mut dec);
    let before = dpb_frame_nums(dec.dpb());

    // nal_ref_idc = 0, nal_unit_type = 1: no dec_ref_pic_marking syntax.
    decode_au(&mut dec, annexb(0x01, &decodable_i_slice_rbsp(1, &[])));
    assert_eq!(dpb_frame_nums(dec.dpb()), before);
}

/// A malformed MMCO command must not leave the decoder predicting from a
/// wrongly-marked reference: the DPB is emptied instead, and decoding
/// continues (the decoder falls back rather than erroring out).
#[test]
fn malformed_mmco_empties_the_decoder_dpb_instead_of_half_marking_it() {
    let mut dec = H264Decoder::new();
    decode_stream_start(&mut dec);
    decode_reference_picture(&mut dec, 1, &[]);
    assert_eq!(dec.dpb().len(), 2);

    // CurrPicNum = 2, difference_of_pic_nums_minus1 = 9 → picNumX = −8, which
    // no picture in the DPB has.
    decode_reference_picture(&mut dec, 2, &[&[1, 9]]);
    assert!(dec.dpb().is_empty());
}
