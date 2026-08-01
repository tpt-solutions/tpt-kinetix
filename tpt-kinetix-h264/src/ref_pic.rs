//! H.264 reference picture management for the frame-based decoding path.
//!
//! Implements Picture Order Count derivation (§8.2.1), the Decoded Picture
//! Buffer (§8.2.5), and reference picture list construction (§8.2.4). This
//! module covers frame pictures (`frame_mbs_only_flag == 1`, no PAFF/MBAFF);
//! field pictures and MMCO long-term marking are not yet implemented.

use tpt_kinetix_core::frame::VideoFrame;

use crate::sps::SeqParameterSet;

/// One stored picture in the Decoded Picture Buffer.
#[derive(Debug, Clone)]
pub struct DpbEntry {
    /// The reconstructed frame (luma + chroma planes, YUV420p).
    pub frame: VideoFrame,
    /// `frame_num` from the slice header that decoded this picture.
    pub frame_num: u32,
    /// `PicOrderCnt` for a frame picture (§8.2.1) — used for reference-list
    /// ordering and for display reordering of later pictures.
    pub pic_order_cnt: i64,
    /// Whether the picture is still marked "used for short-term reference".
    pub is_short_term: bool,
    /// Whether the picture is still marked "used for long-term reference".
    pub is_long_term: bool,
    /// `LongTermPicNum` — only meaningful when `is_long_term`.
    pub long_term_pic_num: i32,
}

impl DpbEntry {
    /// Whether this picture is still marked as a reference.
    pub fn is_reference(&self) -> bool {
        self.is_short_term || self.is_long_term
    }
}

/// State carried between pictures for POC derivation (§8.2.1.1).
#[derive(Debug, Clone, Default)]
pub struct PocState {
    /// `prevPicOrderCntMsb`.
    pub prev_pic_order_cnt_msb: i64,
    /// `prevPicOrderCntLsb`.
    pub prev_pic_order_cnt_lsb: i64,
    /// `prevFrameNum` — `frame_num` of the previous reference picture.
    pub prev_frame_num: u32,
}

/// A POC type this decoder does not yet implement. Callers fall back to the
/// scaffold path for such streams rather than emit wrong output.
#[derive(Debug)]
pub struct PicOrderCntError(pub &'static str);

impl std::fmt::Display for PicOrderCntError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unsupported PicOrderCnt: {}", self.0)
    }
}

impl std::error::Error for PicOrderCntError {}

/// Derive `PicOrderCnt` for a frame picture (§8.2.1) and update `state` for
/// the next picture.
///
/// * `is_idr` — whether this is an IDR picture (resets the POC MSB/LSB state).
/// * `is_reference` — `nal_ref_idc != 0`; only reference pictures advance the
///   decoder's POC / frame-num state.
/// * `frame_num` — the slice `frame_num`.
/// * `pic_order_cnt_lsb` — the slice `pic_order_cnt_lsb`, required when
///   `pic_order_cnt_type == 0`.
pub fn derive_pic_order_cnt(
    sps: &SeqParameterSet,
    is_idr: bool,
    is_reference: bool,
    frame_num: u32,
    pic_order_cnt_lsb: Option<u32>,
    state: &mut PocState,
) -> Result<i64, PicOrderCntError> {
    match sps.pic_order_cnt_type {
        0 => derive_poc_type0(sps, is_idr, is_reference, frame_num, pic_order_cnt_lsb, state),
        2 => derive_poc_type2(sps, is_idr, is_reference, frame_num, state),
        _ => Err(PicOrderCntError("pic_order_cnt_type 1 is not implemented")),
    }
}

/// §8.2.1.1 — `pic_order_cnt_type == 0`.
#[allow(clippy::too_many_arguments)]
fn derive_poc_type0(
    sps: &SeqParameterSet,
    is_idr: bool,
    is_reference: bool,
    frame_num: u32,
    pic_order_cnt_lsb: Option<u32>,
    state: &mut PocState,
) -> Result<i64, PicOrderCntError> {
    let max_pic_order_cnt_lsb = 1i64 << (sps.log2_max_pic_order_cnt_lsb_minus4 + 4);
    let pic_order_cnt_lsb =
        pic_order_cnt_lsb.ok_or(PicOrderCntError("pic_order_cnt_lsb absent"))? as i64;

    if is_idr {
        state.prev_pic_order_cnt_msb = 0;
        state.prev_pic_order_cnt_lsb = 0;
    }

    let mut pic_order_cnt_msb = state.prev_pic_order_cnt_msb;
    let half = max_pic_order_cnt_lsb / 2;
    if pic_order_cnt_lsb < state.prev_pic_order_cnt_lsb
        && state.prev_pic_order_cnt_lsb - pic_order_cnt_lsb >= half
    {
        pic_order_cnt_msb += max_pic_order_cnt_lsb;
    } else if pic_order_cnt_lsb > state.prev_pic_order_cnt_lsb
        && pic_order_cnt_lsb - state.prev_pic_order_cnt_lsb > half
    {
        pic_order_cnt_msb -= max_pic_order_cnt_lsb;
    }

    if is_reference {
        state.prev_pic_order_cnt_msb = pic_order_cnt_msb;
        state.prev_pic_order_cnt_lsb = pic_order_cnt_lsb;
        state.prev_frame_num = frame_num;
    }

    Ok(pic_order_cnt_msb + pic_order_cnt_lsb)
}

/// §8.2.1.3 — `pic_order_cnt_type == 2`.
fn derive_poc_type2(
    sps: &SeqParameterSet,
    is_idr: bool,
    is_reference: bool,
    frame_num: u32,
    state: &mut PocState,
) -> Result<i64, PicOrderCntError> {
    let max_frame_num = 1i64 << (sps.log2_max_frame_num_minus4 + 4);
    let frame_num_offset = if is_idr {
        0
    } else if is_reference && (frame_num as i64) < state.prev_frame_num as i64 {
        max_frame_num
    } else {
        0
    };

    if is_reference {
        state.prev_frame_num = frame_num;
    }

    Ok((frame_num_offset + frame_num as i64) * 2)
}

/// The Decoded Picture Buffer.
///
/// Stores only reference pictures; non-reference pictures never enter it.
/// When the buffer would hold more than `max_num_ref_frames` reference
/// pictures, the sliding-window marking process (§8.2.5.3) drops the
/// short-term reference with the smallest `PicOrderCnt`.
#[derive(Debug, Default)]
pub struct Dpb {
    entries: Vec<DpbEntry>,
}

impl Dpb {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn iter(&self) -> impl Iterator<Item = &DpbEntry> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Insert a decoded reference picture, applying sliding-window marking
    /// (§8.2.5.3) once the reference count exceeds `max_num_ref_frames`.
    ///
    /// A pre-existing short-term entry with the same `frame_num` is replaced
    /// (the spec forbids two pictures sharing a `frame_num`).
    pub fn push(&mut self, entry: DpbEntry, max_num_ref_frames: u32) {
        let frame_num = entry.frame_num;
        self.entries
            .retain(|e| !(e.is_short_term && e.frame_num == frame_num));
        self.entries.push(entry);
        self.apply_sliding_window(max_num_ref_frames);
        self.entries.retain(|e| e.is_reference());
    }

    /// Drain every stored picture's frame, clearing the buffer.
    pub fn take_frames(&mut self) -> Vec<VideoFrame> {
        self.entries.drain(..).map(|e| e.frame).collect()
    }

    fn apply_sliding_window(&mut self, max_num_ref_frames: u32) {
        let max = max_num_ref_frames as usize;
        while self.entries.iter().filter(|e| e.is_reference()).count() > max {
            let victim = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.is_short_term)
                .min_by_key(|(_, e)| e.pic_order_cnt)
                .map(|(i, _)| i)
                .or_else(|| self.entries.iter().position(|e| e.is_reference()));
            let Some(i) = victim else {
                break;
            };
            self.entries[i].is_short_term = false;
            self.entries[i].is_long_term = false;
        }
    }
}

/// Build `RefPicList0` for a P (or SP) slice (§8.2.4.1).
///
/// Short-term reference pictures are ordered by descending `PicOrderCnt`,
/// followed by long-term pictures ordered by ascending `LongTermPicNum`. The
/// list is truncated to `num_ref_idx_l0_active`; if the DPB holds fewer
/// pictures than requested, the last entry is repeated (as permitted by the
/// spec, which guarantees the current picture itself is always available).
///
/// Returns `None` when the DPB holds no reference pictures at all (such a P
/// slice is invalid per spec, but the caller must decide how to handle it).
pub fn build_ref_list_l0(dpb: &Dpb, num_ref_idx_l0_active: usize) -> Option<Vec<DpbEntry>> {
    let mut shorts: Vec<&DpbEntry> = dpb.iter().filter(|e| e.is_short_term).collect();
    shorts.sort_by_key(|e| std::cmp::Reverse(e.pic_order_cnt));
    let mut longs: Vec<&DpbEntry> = dpb.iter().filter(|e| e.is_long_term).collect();
    longs.sort_by_key(|e| e.long_term_pic_num);

    if shorts.is_empty() && longs.is_empty() {
        return None;
    }

    let mut list: Vec<DpbEntry> = shorts.into_iter().chain(longs).cloned().collect();
    list.truncate(num_ref_idx_l0_active.max(1));
    while list.len() < num_ref_idx_l0_active {
        let last = list.last()?.clone();
        list.push(last);
    }
    Some(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_kinetix_core::{pixel_format::PixelFormat, timestamp::Timestamp};

    fn sps(
        poc_type: u32,
        log2_lsb_minus4: u32,
        log2_frame_num_minus4: u32,
        num_ref_frames: u32,
    ) -> SeqParameterSet {
        SeqParameterSet {
            profile_idc: 66,
            level_idc: 30,
            seq_parameter_set_id: 0,
            chroma_format_idc: 1,
            separate_colour_plane_flag: false,
            log2_max_frame_num_minus4: log2_frame_num_minus4,
            pic_order_cnt_type: poc_type,
            log2_max_pic_order_cnt_lsb_minus4: log2_lsb_minus4,
            num_ref_frames,
            gaps_in_frame_num_value_allowed_flag: false,
            pic_width_in_mbs_minus1: 9,
            pic_height_in_map_units_minus1: 9,
            frame_mbs_only_flag: true,
            frame_cropping_flag: false,
            frame_crop_left_offset: 0,
            frame_crop_right_offset: 0,
            frame_crop_top_offset: 0,
            frame_crop_bottom_offset: 0,
        }
    }

    fn entry(frame_num: u32, poc: i64) -> DpbEntry {
        DpbEntry {
            frame: VideoFrame {
                pts: Timestamp::NONE,
                dts: Timestamp::NONE,
                data: Vec::new(),
                width: 160,
                height: 160,
                pixel_format: PixelFormat::Yuv420p,
                is_key_frame: false,
            },
            frame_num,
            pic_order_cnt: poc,
            is_short_term: true,
            is_long_term: false,
            long_term_pic_num: -1,
        }
    }

    fn long_entry(long_term_pic_num: i32) -> DpbEntry {
        let mut e = entry(99, 0);
        e.is_short_term = false;
        e.is_long_term = true;
        e.long_term_pic_num = long_term_pic_num;
        e
    }

    #[test]
    fn poc_type0_idr_resets_and_increments() {
        let sps = sps(0, 4, 0, 1); // MaxPicOrderCntLsb = 256
        let mut state = PocState::default();
        let poc = derive_pic_order_cnt(&sps, true, true, 0, Some(0), &mut state).unwrap();
        assert_eq!(poc, 0);
        let poc = derive_pic_order_cnt(&sps, false, true, 1, Some(1), &mut state).unwrap();
        assert_eq!(poc, 1);
        let poc = derive_pic_order_cnt(&sps, false, true, 2, Some(2), &mut state).unwrap();
        assert_eq!(poc, 2);
    }

    #[test]
    fn poc_type0_msb_wraparound_forward() {
        // MaxPicOrderCntLsb = 16, wrap threshold half = 8.
        let sps = sps(0, 0, 0, 1);
        let mut state = PocState::default();
        derive_pic_order_cnt(&sps, true, true, 0, Some(0), &mut state).unwrap();
        // lsb 6 (poc 6), then lsb 13 (diff 7 <= 8, no wrap) → poc 13,
        // then lsb 2: 13 - 2 = 11 >= 8 → msb += 16 → 18.
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 1, Some(6), &mut state).unwrap(),
            6
        );
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 2, Some(13), &mut state).unwrap(),
            13
        );
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 3, Some(2), &mut state).unwrap(),
            18
        );
    }

    #[test]
    fn poc_type0_msb_wraparound_backward() {
        let sps = sps(0, 0, 0, 1); // MaxPicOrderCntLsb = 16
        let mut state = PocState::default();
        derive_pic_order_cnt(&sps, true, true, 0, Some(0), &mut state).unwrap();
        // lsb 2 (poc 2), then lsb 14: 14 - 2 = 12 > 8 → msb -= 16 → -2.
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 1, Some(2), &mut state).unwrap(),
            2
        );
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 2, Some(14), &mut state).unwrap(),
            -2
        );
    }

    #[test]
    fn poc_type0_non_reference_does_not_advance_state() {
        let sps = sps(0, 4, 0, 1);
        let mut state = PocState::default();
        derive_pic_order_cnt(&sps, true, true, 0, Some(0), &mut state).unwrap();
        // Non-reference picture with lsb 50 must not update prev state.
        let poc = derive_pic_order_cnt(&sps, false, false, 1, Some(50), &mut state).unwrap();
        assert_eq!(poc, 50);
        assert_eq!(state.prev_pic_order_cnt_lsb, 0);
        assert_eq!(state.prev_frame_num, 0);
    }

    #[test]
    fn poc_type2_doubles_frame_num() {
        let sps = sps(2, 0, 4, 1); // MaxFrameNum = 256
        let mut state = PocState::default();
        assert_eq!(derive_pic_order_cnt(&sps, true, true, 0, None, &mut state).unwrap(), 0);
        assert_eq!(derive_pic_order_cnt(&sps, false, true, 1, None, &mut state).unwrap(), 2);
        assert_eq!(derive_pic_order_cnt(&sps, false, true, 2, None, &mut state).unwrap(), 4);
    }

    #[test]
    fn ref_list_l0_short_term_descending_poc() {
        let mut dpb = Dpb::new();
        dpb.push(entry(0, 4), 4);
        dpb.push(entry(1, 0), 4);
        dpb.push(entry(2, 8), 4);
        let list = build_ref_list_l0(&dpb, 2).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].pic_order_cnt, 8);
        assert_eq!(list[1].pic_order_cnt, 4);
    }

    #[test]
    fn ref_list_l0_long_term_after_short_term() {
        let mut dpb = Dpb::new();
        dpb.push(entry(0, 4), 4);
        let mut lt = long_entry(0);
        lt.frame_num = 1;
        dpb.push(lt, 4);
        let list = build_ref_list_l0(&dpb, 2).unwrap();
        assert_eq!(list[0].pic_order_cnt, 4);
        assert!(list[0].is_short_term);
        assert!(list[1].is_long_term);
    }

    #[test]
    fn ref_list_l0_repeats_last_when_dpb_short() {
        let mut dpb = Dpb::new();
        dpb.push(entry(0, 4), 4);
        let list = build_ref_list_l0(&dpb, 3).unwrap();
        assert_eq!(list.len(), 3);
        assert!(list.iter().all(|e| e.pic_order_cnt == 4));
    }

    #[test]
    fn ref_list_l0_empty_dpb_returns_none() {
        let dpb = Dpb::new();
        assert!(build_ref_list_l0(&dpb, 1).is_none());
    }

    #[test]
    fn sliding_window_keeps_most_recent_refs() {
        let mut dpb = Dpb::new();
        dpb.push(entry(0, 0), 1);
        assert_eq!(dpb.len(), 1);
        dpb.push(entry(1, 1), 1);
        assert_eq!(dpb.len(), 1);
        assert_eq!(dpb.iter().next().unwrap().pic_order_cnt, 1);
        assert_eq!(dpb.iter().next().unwrap().frame_num, 1);
    }

    #[test]
    fn push_replaces_same_frame_num() {
        let mut dpb = Dpb::new();
        dpb.push(entry(5, 0), 4);
        dpb.push(entry(5, 99), 4);
        assert_eq!(dpb.len(), 1);
        assert_eq!(dpb.iter().next().unwrap().pic_order_cnt, 99);
    }
}
