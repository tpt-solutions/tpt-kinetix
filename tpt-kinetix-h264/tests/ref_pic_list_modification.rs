//! End-to-end check that `ref_pic_list_modification` (§7.3.3.1) actually
//! reaches reference-list construction (§8.2.4.3), rather than being parsed
//! and discarded.
//!
//! The unit tests in `ref_pic.rs` cover the modification process itself
//! against hand-derived `picNumLX` values; this test drives it from real
//! bitstream syntax so a regression that re-severs the wiring (e.g. dropping
//! the header field again) fails here even if `modify_ref_pic_list` stays
//! correct in isolation.

use tpt_kinetix_core::{frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp};
use tpt_kinetix_h264::nal::NalUnitType;
use tpt_kinetix_h264::ref_pic::{build_ref_list_l0, Dpb, DpbEntry, PicNumContext};
use tpt_kinetix_h264::slice::{RefPicListModification, SliceHeader, SliceHeaderContext};

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

/// A non-IDR P slice with `frame_num == 4`, `num_ref_idx_l0_active_minus1 == 3`
/// and the given `(modification_of_pic_nums_idc, value)` command list.
fn p_slice_rbsp(mods: &[(u32, u32)]) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.ue(0); // first_mb_in_slice
    w.ue(0); // slice_type = P
    w.ue(0); // pic_parameter_set_id
    w.bits(4, 4); // frame_num = 4
    w.bits(8, 4); // pic_order_cnt_lsb
    w.bit(1); // num_ref_idx_active_override_flag
    w.ue(3); // num_ref_idx_l0_active_minus1 = 3
    w.bit(if mods.is_empty() { 0 } else { 1 }); // ref_pic_list_modification_flag_l0
    if !mods.is_empty() {
        for &(idc, value) in mods {
            w.ue(idc);
            w.ue(value);
        }
        w.ue(3); // terminator
    }
    w.bit(0); // adaptive_ref_pic_marking_mode_flag
    w.se(0); // slice_qp_delta
    w.finish()
}

fn parse(rbsp: &[u8]) -> SliceHeader {
    SliceHeader::parse_with_context(rbsp, NalUnitType::NonIdrSlice, 1, &ctx())
        .expect("slice header")
}

fn dpb_with_four_refs() -> Dpb {
    let mut dpb = Dpb::new();
    for frame_num in 0..4u32 {
        dpb.push(
            DpbEntry {
                frame: VideoFrame {
                    pts: Timestamp::NONE,
                    dts: Timestamp::NONE,
                    // Distinct luma fill so a reordered list is observable in
                    // the pixel data a decoder would motion-compensate from.
                    data: vec![frame_num as u8; 16 * 16 * 3 / 2],
                    width: 16,
                    height: 16,
                    pixel_format: PixelFormat::Yuv420p,
                    is_key_frame: false,
                },
                frame_num,
                pic_order_cnt: frame_num as i64 * 2,
                is_short_term: true,
                is_long_term: false,
                long_term_pic_num: -1,
            },
            PicNumContext {
                curr_frame_num: frame_num,
                max_frame_num: 16,
            },
            4,
        );
    }
    dpb
}

fn list_frame_nums(dpb: &Dpb, header: &SliceHeader) -> Vec<u32> {
    let pic_num_ctx = PicNumContext {
        curr_frame_num: header.frame_num,
        max_frame_num: 16,
    };
    build_ref_list_l0(
        dpb,
        header.num_ref_idx_l0_active_minus1 as usize + 1,
        pic_num_ctx,
        &header.ref_pic_list_modification_l0,
    )
    .expect("ref list")
    .iter()
    .map(|e| e.frame_num)
    .collect()
}

#[test]
fn explicit_reorder_command_changes_ref_pic_list0_order() {
    let dpb = dpb_with_four_refs();

    // §8.2.4.2.1 initialisation: short-term refs by descending PicNum.
    let default_header = parse(&p_slice_rbsp(&[]));
    assert!(default_header.ref_pic_list_modification_l0.is_empty());
    let default_order = list_frame_nums(&dpb, &default_header);
    assert_eq!(default_order, vec![3, 2, 1, 0]);

    // §8.2.4.3.1: picNumLXPred starts at CurrPicNum (4);
    // abs_diff_pic_num_minus1 = 3 subtracts 4, giving picNumLX = 0.
    let reordered_header = parse(&p_slice_rbsp(&[(0, 3)]));
    assert_eq!(
        reordered_header.ref_pic_list_modification_l0,
        vec![RefPicListModification::ShortTermSubtract {
            abs_diff_pic_num_minus1: 3
        }]
    );
    let reordered = list_frame_nums(&dpb, &reordered_header);

    assert_eq!(reordered, vec![0, 3, 2, 1]);
    assert_ne!(
        reordered, default_order,
        "reorder command must not decode to the default list"
    );
}

#[test]
fn reorder_selects_a_different_reference_picture_for_ref_idx_0() {
    // ref_idx 0 is by far the most-used index, so check the reorder actually
    // swaps the *picture data* a decoder would predict from, not just the
    // bookkeeping order.
    let dpb = dpb_with_four_refs();
    let pic_num_ctx = PicNumContext {
        curr_frame_num: 4,
        max_frame_num: 16,
    };

    let default_l0 = build_ref_list_l0(&dpb, 4, pic_num_ctx, &[]).unwrap();
    assert_eq!(default_l0[0].frame.data[0], 3);

    let header = parse(&p_slice_rbsp(&[(0, 3)]));
    let reordered_l0 =
        build_ref_list_l0(&dpb, 4, pic_num_ctx, &header.ref_pic_list_modification_l0).unwrap();
    assert_eq!(reordered_l0[0].frame.data[0], 0);
}

#[test]
fn multiple_commands_apply_in_order() {
    let dpb = dpb_with_four_refs();
    // idc 0 with abs_diff 2 → picNumLX 2 at index 0 (pred becomes 2);
    // idc 0 with abs_diff 1 → picNumLX 1 at index 1.
    let header = parse(&p_slice_rbsp(&[(0, 1), (0, 0)]));
    assert_eq!(list_frame_nums(&dpb, &header), vec![2, 1, 3, 0]);
}
