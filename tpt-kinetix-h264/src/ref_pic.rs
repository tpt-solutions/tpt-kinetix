//! H.264 reference picture management for the frame-based decoding path.
//!
//! Implements Picture Order Count derivation (§8.2.1), the Decoded Picture
//! Buffer (§8.2.5), the decoded reference picture marking process (§8.2.5.1,
//! including sliding-window marking §8.2.5.3 and adaptive MMCO marking
//! §8.2.5.4), reference picture list initialisation (§8.2.4.2) and the
//! reference picture list modification process (§8.2.4.3). This module covers
//! frame pictures (`frame_mbs_only_flag == 1`, no PAFF/MBAFF); field pictures
//! are not yet implemented.

use tpt_kinetix_core::frame::VideoFrame;

use crate::slice::{DecRefPicMarking, MmcoOp, RefPicListModification};
use crate::sps::SeqParameterSet;

/// One stored picture in the Decoded Picture Buffer.
#[derive(Debug, Clone)]
pub struct DpbEntry {
    /// The reconstructed frame (luma + chroma planes, YUV420p).
    pub frame: VideoFrame,
    /// `frame_num` from the slice header that decoded this picture.
    pub frame_num: u32,
    /// `field_pic_flag` (§7.3.3) — true when this stored picture is a coded
    /// field rather than a frame. `false` for frame pictures (the usual case
    /// for progressive coding).
    pub field_pic_flag: bool,
    /// `bottom_field_flag` (§7.3.3) — for a field picture, true when it is the
    /// bottom field. Only meaningful when `field_pic_flag` is true; `false` for
    /// frame pictures.
    pub bottom_field_flag: bool,
    /// `PicOrderCnt` for a frame picture (§8.2.1) — used for reference-list
    /// ordering and for display reordering of later pictures.
    pub pic_order_cnt: i64,
    /// Whether the picture is still marked "used for short-term reference".
    pub is_short_term: bool,
    /// Whether the picture is still marked "used for long-term reference".
    pub is_long_term: bool,
    /// `LongTermPicNum` — only meaningful when `is_long_term`, and `-1`
    /// otherwise.
    ///
    /// For a frame picture `LongTermPicNum == LongTermFrameIdx` (§8.2.4.1), so
    /// this field doubles as the `LongTermFrameIdx` assigned by MMCO 3 / MMCO 6
    /// and compared against `MaxLongTermFrameIdx` by MMCO 4.
    pub long_term_pic_num: i32,
    /// Per-macroblock motion vector grid of this picture, used as the co-located
    /// picture for B-slice temporal direct mode (§8.4.1.2.3). `None` until this
    /// picture's B/P slice MV store is recorded (e.g. by `predict_b_slice_mvs`).
    pub mv_grid: Option<std::sync::Arc<Vec<[crate::mv::MvCell; 16]>>>,
}

impl DpbEntry {
    /// Whether this picture is still marked as a reference.
    pub fn is_reference(&self) -> bool {
        self.is_short_term || self.is_long_term
    }

    /// `PicNum` for this short-term reference picture (§8.2.4.1 / §8.2.4.2.5).
    ///
    /// For a frame picture `PicNum == FrameNumWrap`, where `FrameNumWrap` is
    /// `FrameNum - MaxFrameNum` when the stored `frame_num` is greater than the
    /// current picture's `frame_num` (i.e. it predates a `frame_num` wrap), and
    /// `FrameNum` otherwise.
    ///
    /// For a field picture (`ctx.field_pic_flag`) the `PicNum` is doubled
    /// (§8.2.4.2.5): `PicNum = 2 * FrameNumWrap + (bottom_field_flag ? 1 : 0)`,
    /// and `MaxPicNum == 2 * MaxFrameNum`. The current picture's `FrameNumWrap`
    /// comparison still uses the *frame* `frame_num` (not the doubled value), as
    /// the field's `frame_num` equals that of its frame.
    ///
    /// Meaningless for long-term pictures, which are addressed by
    /// [`DpbEntry::long_term_pic_num`] instead.
    pub fn pic_num(&self, ctx: PicNumContext) -> i64 {
        let frame_num = self.frame_num as i64;
        let frame_num_wrap = if frame_num > ctx.curr_frame_num as i64 {
            frame_num - ctx.max_frame_num as i64
        } else {
            frame_num
        };
        if ctx.field_pic_flag {
            // §8.2.4.2.5: doubled PicNum; the bottom field of a frame carries
            // the +1 parity. (Frame entries never carry a bottom flag.)
            2 * frame_num_wrap + if self.bottom_field_flag { 1 } else { 0 }
        } else {
            frame_num_wrap
        }
    }
}

/// The picture-numbering context the reference-list modification process needs
/// (§8.2.4.3.1).
///
/// For frame pictures `CurrPicNum == frame_num` and `MaxPicNum == MaxFrameNum`.
/// For field pictures (§8.2.4.2.5) both are doubled: `CurrPicNum == 2 *
/// frame_num + (bottom_field_flag ? 1 : 0)` and `MaxPicNum == 2 *
/// MaxFrameNum`, so the current picture's parity and field-ness must be known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PicNumContext {
    /// `frame_num` of the picture currently being decoded (`CurrPicNum`).
    pub curr_frame_num: u32,
    /// `MaxFrameNum` = `1 << (log2_max_frame_num_minus4 + 4)` (`MaxPicNum`).
    pub max_frame_num: u32,
    /// Whether the current picture is a coded field (PAFF).
    pub field_pic_flag: bool,
    /// Whether the current picture is the bottom field (only when
    /// `field_pic_flag` is true).
    pub bottom_field_flag: bool,
}

impl PicNumContext {
    /// Build the context from the active SPS, the current slice `frame_num`, and
    /// its field flags (§8.2.4.2.5).
    pub fn new(
        sps: &SeqParameterSet,
        curr_frame_num: u32,
        field_pic_flag: bool,
        bottom_field_flag: bool,
    ) -> Self {
        Self {
            curr_frame_num,
            max_frame_num: 1u32 << (sps.log2_max_frame_num_minus4 + 4),
            field_pic_flag,
            bottom_field_flag,
        }
    }
}

/// State carried between pictures for POC derivation (§8.2.1.1).
///
/// The state is tracked per-field rather than per-frame so the same machinery
/// serves both progressive (frame) and interlaced (field / PAFF) coding. For a
/// frame picture §8.2.1.1 assigns the same `PicOrderCnt` to both fields (modulo
/// `delta_pic_order_cnt_bottom`), so the two are equal; for a field picture only
/// the coded field's order count advances. The MSB/LSB predictor used by
/// `pic_order_cnt_type == 0` is derived from the larger of the two (§8.2.1.1).
#[derive(Debug, Clone, Default)]
pub struct PocState {
    /// `prevTopFieldOrderCnt` (§8.2.1.1) — order count of the previous top field.
    pub prev_top_field_order_cnt: i64,
    /// `prevBottomFieldOrderCnt` (§8.2.1.1) — order count of the previous bottom
    /// field.
    pub prev_bottom_field_order_cnt: i64,
    /// `prev_frame_num` — `frame_num` of the previous reference picture.
    pub prev_frame_num: u32,
}

impl PocState {
    /// Reset the POC/`frame_num` state after the current picture carried
    /// `memory_management_control_operation` equal to 5 (§8.2.1.1, §7.4.3).
    ///
    /// §8.2.1.1: when the previous reference picture in decoding order included
    /// MMCO 5, `prevPicOrderCntMsb` is 0 and `prevPicOrderCntLsb` is that
    /// picture's `TopFieldOrderCnt` *after* it was rebased by
    /// `tempPicOrderCnt` — which is 0 for a frame picture, since rebasing
    /// subtracts `PicOrderCnt( CurrPic ) = Min( Top, Bottom )` from both fields.
    /// §7.4.3 further requires `frame_num` of such a picture to be inferred as
    /// 0 for the pictures that follow it.
    pub fn reset_after_mmco5(&mut self) {
        self.prev_top_field_order_cnt = 0;
        self.prev_bottom_field_order_cnt = 0;
        self.prev_frame_num = 0;
    }
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

/// Derive `PicOrderCnt` for a frame or field picture (§8.2.1) and update
/// `state` for the next picture.
///
/// * `is_idr` — whether this is an IDR picture (resets the POC MSB/LSB state).
/// * `is_reference` — `nal_ref_idc != 0`; only reference pictures advance the
///   decoder's POC / frame-num state.
/// * `frame_num` — the slice `frame_num`.
/// * `pic_order_cnt_lsb` — the slice `pic_order_cnt_lsb`, required when
///   `pic_order_cnt_type == 0`.
/// * `field_pic_flag` / `bottom_field_flag` — true for a coded field (PAFF);
///   `bottom_field_flag` is only consulted when `field_pic_flag` is true.
/// * `delta_pic_order_cnt_bottom` — present for frame pictures (only) when
///   `bottom_field_pic_order_in_frame_present_flag` is set; derives
///   `BottomFieldOrderCnt` (§8.2.1.1). Ignored for field pictures.
///
/// Returns the `PicOrderCnt` of the coded picture (the field's order count for a
/// field picture, `TopFieldOrderCnt` for a frame picture).
#[allow(clippy::too_many_arguments)]
pub fn derive_pic_order_cnt(
    sps: &SeqParameterSet,
    is_idr: bool,
    is_reference: bool,
    frame_num: u32,
    pic_order_cnt_lsb: Option<u32>,
    field_pic_flag: bool,
    bottom_field_flag: bool,
    delta_pic_order_cnt_bottom: Option<i64>,
    state: &mut PocState,
) -> Result<i64, PicOrderCntError> {
    match sps.pic_order_cnt_type {
        0 => derive_poc_type0(
            sps,
            is_idr,
            is_reference,
            frame_num,
            pic_order_cnt_lsb,
            field_pic_flag,
            bottom_field_flag,
            delta_pic_order_cnt_bottom,
            state,
        ),
        2 => derive_poc_type2(
            sps,
            is_idr,
            is_reference,
            frame_num,
            field_pic_flag,
            bottom_field_flag,
            state,
        ),
        _ => Err(PicOrderCntError("pic_order_cnt_type 1 is not implemented")),
    }
}

/// §8.2.1.1 — `pic_order_cnt_type == 0` (frame and field).
#[allow(clippy::too_many_arguments)]
fn derive_poc_type0(
    sps: &SeqParameterSet,
    is_idr: bool,
    is_reference: bool,
    frame_num: u32,
    pic_order_cnt_lsb: Option<u32>,
    field_pic_flag: bool,
    bottom_field_flag: bool,
    delta_pic_order_cnt_bottom: Option<i64>,
    state: &mut PocState,
) -> Result<i64, PicOrderCntError> {
    let max_pic_order_cnt_lsb = 1i64 << (sps.log2_max_pic_order_cnt_lsb_minus4 + 4);
    let pic_order_cnt_lsb =
        pic_order_cnt_lsb.ok_or(PicOrderCntError("pic_order_cnt_lsb absent"))? as i64;

    if is_idr {
        state.prev_top_field_order_cnt = 0;
        state.prev_bottom_field_order_cnt = 0;
    }

    // §8.2.1.1: the MSB/LSB predictor for type 0 is taken from the larger of the
    // two previous field order counts, reduced mod MaxPicOrderCntLsb.
    let prev_max = state
        .prev_top_field_order_cnt
        .max(state.prev_bottom_field_order_cnt);
    let prev_pic_order_cnt_lsb = prev_max % max_pic_order_cnt_lsb;
    let prev_pic_order_cnt_msb = prev_max - prev_pic_order_cnt_lsb;

    let mut pic_order_cnt_msb = prev_pic_order_cnt_msb;
    let half = max_pic_order_cnt_lsb / 2;
    if pic_order_cnt_lsb < prev_pic_order_cnt_lsb
        && prev_pic_order_cnt_lsb - pic_order_cnt_lsb >= half
    {
        pic_order_cnt_msb += max_pic_order_cnt_lsb;
    } else if pic_order_cnt_lsb > prev_pic_order_cnt_lsb
        && pic_order_cnt_lsb - prev_pic_order_cnt_lsb > half
    {
        pic_order_cnt_msb -= max_pic_order_cnt_lsb;
    }

    let picture_order_cnt = pic_order_cnt_msb + pic_order_cnt_lsb;

    if is_reference {
        if field_pic_flag {
            // Only the coded field advances its predecessor's order count.
            if bottom_field_flag {
                state.prev_bottom_field_order_cnt = picture_order_cnt;
            } else {
                state.prev_top_field_order_cnt = picture_order_cnt;
            }
        } else {
            // §8.2.1.1: frame picture — both fields share `TopFieldOrderCnt`,
            // and `BottomFieldOrderCnt = Top + delta_pic_order_cnt_bottom`.
            state.prev_top_field_order_cnt = picture_order_cnt;
            state.prev_bottom_field_order_cnt =
                picture_order_cnt + delta_pic_order_cnt_bottom.unwrap_or(0);
        }
        state.prev_frame_num = frame_num;
    }

    Ok(picture_order_cnt)
}

/// §8.2.1.3 — `pic_order_cnt_type == 2` (frame and field).
fn derive_poc_type2(
    sps: &SeqParameterSet,
    is_idr: bool,
    is_reference: bool,
    frame_num: u32,
    field_pic_flag: bool,
    bottom_field_flag: bool,
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

    // §8.2.1.3: a field carries `PicOrderCnt = (frame_num_offset + frame_num) * 2
    // + 1` for the bottom field, `+ 0` for the top field; a frame picture is the
    // top-field value (its bottom field is +1, inferred by the caller).
    let base = (frame_num_offset + frame_num as i64) * 2;
    if field_pic_flag && bottom_field_flag {
        Ok(base + 1)
    } else {
        Ok(base)
    }
}

/// A `memory_management_control_operation` that could not be applied
/// (§8.2.5.4).
///
/// §7.4.3.3 requires every MMCO operand to name a picture that is currently in
/// the DPB and marked as the right kind of reference; a stream that violates
/// this is malformed. Rather than guess which picture was meant, the marking
/// process reports the failure and empties the DPB (see
/// [`Dpb::mark_decoded_picture`]), so later inter prediction falls back instead
/// of predicting from a wrongly-marked reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmcoError {
    /// No short-term reference picture in the DPB has this `PicNum`.
    MissingShortTerm { pic_num: i64 },
    /// No long-term reference picture in the DPB has this `LongTermPicNum`.
    MissingLongTerm { long_term_pic_num: u32 },
}

impl std::fmt::Display for MmcoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingShortTerm { pic_num } => {
                write!(f, "mmco: no short-term reference with PicNum {pic_num}")
            }
            Self::MissingLongTerm { long_term_pic_num } => write!(
                f,
                "mmco: no long-term reference with LongTermPicNum {long_term_pic_num}"
            ),
        }
    }
}

impl std::error::Error for MmcoError {}

/// What the decoded reference picture marking process (§8.2.5) did to the
/// current picture, for state the caller owns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MarkingOutcome {
    /// `memory_management_control_operation` equal to 5 was applied: every
    /// reference picture was dropped and the current picture's `frame_num` and
    /// `PicOrderCnt` were reset to 0. The caller must also reset its POC state
    /// (see [`PocState::reset_after_mmco5`]).
    pub mmco5: bool,
    /// The current picture was marked "used for long-term reference" — either
    /// by MMCO 6, or by an IDR with `long_term_reference_flag == 1` — instead
    /// of the usual short-term marking.
    pub current_is_long_term: bool,
}

/// The Decoded Picture Buffer.
///
/// Stores only reference pictures; non-reference pictures never enter it.
/// Pictures are inserted through [`Dpb::mark_decoded_picture`], which runs the
/// decoded reference picture marking process (§8.2.5): sliding-window marking
/// (§8.2.5.3) when the slice header requests it, or the adaptive MMCO commands
/// (§8.2.5.4) when it does not.
#[derive(Debug, Default)]
pub struct Dpb {
    entries: Vec<DpbEntry>,
    /// `MaxLongTermFrameIdx` (§8.2.5.4.4). `None` is the spec's "no long-term
    /// frame indices", the initial state and the state after MMCO 5 or an IDR
    /// with `long_term_reference_flag == 0`.
    max_long_term_frame_idx: Option<i32>,
}

impl Dpb {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            max_long_term_frame_idx: None,
        }
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

    /// `MaxLongTermFrameIdx` (§8.2.5.4.4); `None` is "no long-term frame
    /// indices".
    pub fn max_long_term_frame_idx(&self) -> Option<i32> {
        self.max_long_term_frame_idx
    }

    /// Number of pictures currently marked "used for short-term reference"
    /// (`numShortTerm`, §8.2.5.3).
    pub fn num_short_term(&self) -> usize {
        self.entries.iter().filter(|e| e.is_short_term).count()
    }

    /// Number of pictures currently marked "used for long-term reference"
    /// (`numLongTerm`, §8.2.5.3).
    pub fn num_long_term(&self) -> usize {
        self.entries.iter().filter(|e| e.is_long_term).count()
    }

    /// Insert a decoded reference picture using sliding-window marking
    /// (§8.2.5.3) — i.e. the `adaptive_ref_pic_marking_mode_flag == 0` case.
    ///
    /// Equivalent to [`Dpb::mark_decoded_picture`] with
    /// [`DecRefPicMarking::SlidingWindow`], which cannot fail.
    pub fn push(&mut self, entry: DpbEntry, ctx: PicNumContext, max_num_ref_frames: u32) {
        let result =
            self.mark_decoded_picture(entry, &DecRefPicMarking::SlidingWindow, ctx, max_num_ref_frames);
        debug_assert!(
            result.is_ok(),
            "sliding-window marking issues no MMCO commands and cannot fail"
        );
    }

    /// Run the decoded reference picture marking process (§8.2.5) for the
    /// just-decoded picture `current`, then store it.
    ///
    /// `marking` is the slice header's `dec_ref_pic_marking` (§7.3.3.3) and
    /// selects between the three paths of §8.2.5.1:
    ///
    /// * [`DecRefPicMarking::Idr`] — all reference pictures are marked "unused
    ///   for reference" and the IDR becomes the sole reference, short-term
    ///   unless `long_term_reference_flag` is set (in which case it takes
    ///   `LongTermFrameIdx == 0` and `MaxLongTermFrameIdx` becomes 0).
    /// * [`DecRefPicMarking::SlidingWindow`] — the sliding-window process
    ///   (§8.2.5.3) frees a slot by marking the short-term reference with the
    ///   smallest `FrameNumWrap` unused, then the current picture is marked
    ///   short-term.
    /// * [`DecRefPicMarking::Adaptive`] — the MMCO commands run in order
    ///   (§8.2.5.4); sliding-window marking is *not* applied. Unless MMCO 6
    ///   claimed the current picture as long-term, it is marked short-term
    ///   afterwards.
    ///
    /// `ctx` supplies `CurrPicNum`/`MaxPicNum` for the `picNumX` derivations,
    /// and `max_num_ref_frames` is the SPS `num_ref_frames`.
    ///
    /// # Errors
    ///
    /// Returns [`MmcoError`] when a command names a picture that is not in the
    /// DPB with the required marking. In that case the DPB is left **empty**
    /// (and `MaxLongTermFrameIdx` reset) rather than half-marked, so a
    /// subsequent slice cannot build a reference list from a state the stream
    /// never described; the caller falls back instead of emitting wrong pixels.
    pub fn mark_decoded_picture(
        &mut self,
        mut current: DpbEntry,
        marking: &DecRefPicMarking,
        ctx: PicNumContext,
        max_num_ref_frames: u32,
    ) -> Result<MarkingOutcome, MmcoError> {
        let mut outcome = MarkingOutcome::default();

        match marking {
            // §8.2.5.1 step 1: for an IDR, all reference pictures are marked
            // "unused for reference" before the current picture is stored.
            DecRefPicMarking::Idr {
                long_term_reference_flag,
                ..
            } => {
                self.entries.clear();
                if *long_term_reference_flag {
                    self.max_long_term_frame_idx = Some(0);
                    mark_long_term(&mut current, 0);
                    outcome.current_is_long_term = true;
                } else {
                    self.max_long_term_frame_idx = None;
                    mark_short_term(&mut current);
                }
            }
            DecRefPicMarking::SlidingWindow => {
                self.apply_sliding_window(ctx, max_num_ref_frames);
                mark_short_term(&mut current);
            }
            DecRefPicMarking::Adaptive(ops) => {
                if let Err(e) = self.apply_mmco(ops, ctx, &mut current, &mut outcome) {
                    // Fail safe: an unusable marking leaves no references
                    // behind, so the next inter slice cannot predict from a
                    // picture whose marking the stream got wrong.
                    self.entries.clear();
                    self.max_long_term_frame_idx = None;
                    return Err(e);
                }
                // §8.2.5.1: "when the current picture ... was not marked as
                // 'used for long-term reference' by memory_management_control_
                // operation equal to 6, it is marked as 'used for short-term
                // reference'".
                if !outcome.current_is_long_term {
                    mark_short_term(&mut current);
                }
            }
        }

        // No two short-term pictures may share a frame_num (§7.4.3); a repeat
        // replaces the older picture.
        let frame_num = current.frame_num;
        self.entries
            .retain(|e| !(e.is_short_term && e.frame_num == frame_num));
        self.entries.push(current);
        self.entries.retain(|e| e.is_reference());
        self.enforce_capacity(ctx, max_num_ref_frames);
        Ok(outcome)
    }

    /// Drain every stored picture's frame, clearing the buffer.
    pub fn take_frames(&mut self) -> Vec<VideoFrame> {
        self.max_long_term_frame_idx = None;
        self.entries.drain(..).map(|e| e.frame).collect()
    }

    /// §8.2.5.4 — the adaptive memory control marking commands, applied in the
    /// order they appear in the slice header.
    fn apply_mmco(
        &mut self,
        ops: &[MmcoOp],
        ctx: PicNumContext,
        current: &mut DpbEntry,
        outcome: &mut MarkingOutcome,
    ) -> Result<(), MmcoError> {
        let curr_pic_num = ctx.curr_frame_num as i64;

        for op in ops {
            match *op {
                // §8.2.5.4.1 — mark a short-term picture "unused for
                // reference". picNumX = CurrPicNum − ( difference_of_pic_nums_
                // minus1 + 1 ) (8-40); it is compared against PicNum, which is
                // FrameNumWrap for a frame picture and may be negative.
                MmcoOp::ShortTermUnused {
                    difference_of_pic_nums_minus1,
                } => {
                    let pic_num = curr_pic_num - (difference_of_pic_nums_minus1 as i64 + 1);
                    let idx = self
                        .find_short_term(pic_num, ctx)
                        .ok_or(MmcoError::MissingShortTerm { pic_num })?;
                    self.entries[idx].is_short_term = false;
                }
                // §8.2.5.4.2 — mark a long-term picture "unused for reference".
                MmcoOp::LongTermUnused { long_term_pic_num } => {
                    let idx = self
                        .find_long_term(long_term_pic_num as i32)
                        .ok_or(MmcoError::MissingLongTerm { long_term_pic_num })?;
                    self.entries[idx].is_long_term = false;
                }
                // §8.2.5.4.3 — assign a LongTermFrameIdx to a short-term
                // reference picture, converting it to long-term.
                MmcoOp::ShortTermToLongTerm {
                    difference_of_pic_nums_minus1,
                    long_term_frame_idx,
                } => {
                    let pic_num = curr_pic_num - (difference_of_pic_nums_minus1 as i64 + 1);
                    let idx = self
                        .find_short_term(pic_num, ctx)
                        .ok_or(MmcoError::MissingShortTerm { pic_num })?;
                    let long_term_frame_idx = long_term_frame_idx as i32;
                    // "When LongTermFrameIdx equal to long_term_frame_idx is
                    // already assigned to a long-term reference frame, that
                    // frame ... is marked as 'unused for reference'."
                    self.evict_long_term_frame_idx(long_term_frame_idx, Some(idx));
                    let entry = &mut self.entries[idx];
                    entry.is_short_term = false;
                    entry.is_long_term = true;
                    entry.long_term_pic_num = long_term_frame_idx;
                }
                // §8.2.5.4.4 — set MaxLongTermFrameIdx, dropping every
                // long-term picture above the new maximum.
                MmcoOp::SetMaxLongTermFrameIdx {
                    max_long_term_frame_idx_plus1,
                } => {
                    let max = (max_long_term_frame_idx_plus1 > 0)
                        .then(|| max_long_term_frame_idx_plus1 as i32 - 1);
                    self.max_long_term_frame_idx = max;
                    for entry in &mut self.entries {
                        if entry.is_long_term
                            && max.is_none_or(|m| entry.long_term_pic_num > m)
                        {
                            entry.is_long_term = false;
                        }
                    }
                }
                // §8.2.5.4.5 — mark all reference pictures "unused for
                // reference" and reset MaxLongTermFrameIdx. §7.4.3/§8.2.1 then
                // treat the current picture as having frame_num 0, and rebase
                // its PicOrderCnt to 0 (frame picture: both fields are shifted
                // by tempPicOrderCnt = PicOrderCnt( CurrPic )).
                MmcoOp::ResetAll => {
                    self.entries.clear();
                    self.max_long_term_frame_idx = None;
                    current.frame_num = 0;
                    current.pic_order_cnt = 0;
                    outcome.mmco5 = true;
                }
                // §8.2.5.4.6 — mark the *current* picture long-term.
                MmcoOp::CurrentToLongTerm {
                    long_term_frame_idx,
                } => {
                    let long_term_frame_idx = long_term_frame_idx as i32;
                    self.evict_long_term_frame_idx(long_term_frame_idx, None);
                    mark_long_term(current, long_term_frame_idx);
                    outcome.current_is_long_term = true;
                }
            }
            // Pictures marked "unused for reference" leave the DPB immediately,
            // so a later command in the same list cannot select one.
            self.entries.retain(|e| e.is_reference());
        }
        Ok(())
    }

    /// Index of the short-term reference picture with `PicNum == pic_num`
    /// (§8.2.4.1).
    fn find_short_term(&self, pic_num: i64, ctx: PicNumContext) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| pic_num_f(e, ctx) == Some(pic_num))
    }

    /// Index of the long-term reference picture with this `LongTermPicNum`
    /// (§8.2.4.1; equal to `LongTermFrameIdx` for frame pictures).
    fn find_long_term(&self, long_term_pic_num: i32) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| long_term_pic_num_f(e) == Some(long_term_pic_num as i64))
    }

    /// Mark any long-term picture already holding `long_term_frame_idx` as
    /// "unused for reference" (§8.2.5.4.3 / §8.2.5.4.6), except the entry at
    /// `keep` (the picture currently being assigned that index).
    fn evict_long_term_frame_idx(&mut self, long_term_frame_idx: i32, keep: Option<usize>) {
        for (i, entry) in self.entries.iter_mut().enumerate() {
            if Some(i) != keep && entry.is_long_term && entry.long_term_pic_num == long_term_frame_idx
            {
                entry.is_long_term = false;
            }
        }
    }

    /// §8.2.5.3 — sliding window decoded reference picture marking.
    ///
    /// Called *before* the current picture is stored, so the spec's
    /// `numShortTerm + numLongTerm == Max( max_num_ref_frames, 1 )` condition
    /// is a `>=` here; the loop form additionally recovers a DPB that a
    /// malformed stream already overfilled.
    fn apply_sliding_window(&mut self, ctx: PicNumContext, max_num_ref_frames: u32) {
        let max = (max_num_ref_frames as usize).max(1);
        while self.entries.iter().filter(|e| e.is_reference()).count() >= max {
            if !self.evict_oldest_short_term(ctx) {
                break;
            }
        }
    }

    /// Defensive post-marking clamp.
    ///
    /// The MMCO path can legitimately leave the DPB at capacity, but a
    /// malformed stream that never frees a slot would otherwise grow it without
    /// bound — each entry owns a full decoded frame, so that is a memory-
    /// exhaustion vector for the fuzzers. FFmpeg's
    /// `ff_h264_execute_ref_pic_marking` performs the same "number of reference
    /// frames exceeds max (probably corrupt input), discarding one" clamp.
    fn enforce_capacity(&mut self, ctx: PicNumContext, max_num_ref_frames: u32) {
        let max = (max_num_ref_frames as usize).max(1);
        while self.entries.iter().filter(|e| e.is_reference()).count() > max {
            if !self.evict_oldest_short_term(ctx) {
                break;
            }
        }
    }

    /// Mark the short-term reference with the smallest `FrameNumWrap` as
    /// "unused for reference" (§8.2.5.3) and drop it.
    ///
    /// Returns `false` when there was nothing left to evict. Long-term
    /// pictures are only touched as a last resort: the spec guarantees a
    /// conforming stream keeps at least one short-term reference whenever
    /// sliding-window marking runs, so reaching the fallback means the stream
    /// is malformed and the alternative is unbounded growth.
    fn evict_oldest_short_term(&mut self, ctx: PicNumContext) -> bool {
        let victim = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_short_term)
            .min_by_key(|(_, e)| e.pic_num(ctx))
            .map(|(i, _)| i)
            .or_else(|| self.entries.iter().position(|e| e.is_reference()));
        let Some(i) = victim else {
            return false;
        };
        self.entries[i].is_short_term = false;
        self.entries[i].is_long_term = false;
        self.entries.retain(|e| e.is_reference());
        true
    }
}

/// Mark a picture "used for short-term reference" (and not long-term).
fn mark_short_term(entry: &mut DpbEntry) {
    entry.is_short_term = true;
    entry.is_long_term = false;
    entry.long_term_pic_num = -1;
}

/// Mark a picture "used for long-term reference" with this `LongTermFrameIdx`.
fn mark_long_term(entry: &mut DpbEntry, long_term_frame_idx: i32) {
    entry.is_short_term = false;
    entry.is_long_term = true;
    entry.long_term_pic_num = long_term_frame_idx;
}

/// A `ref_pic_list_modification` command that could not be applied.
///
/// The spec requires the referenced picture to be present in the DPB and
/// marked as the appropriate kind of reference; a stream that violates this is
/// malformed (or exercises a feature this decoder does not track yet, such as
/// MMCO-created long-term references). Callers treat this as "cannot decode
/// this slice correctly" and fall back rather than emitting wrong pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefPicListError {
    /// No short-term reference picture in the DPB has this `PicNum`.
    MissingShortTerm { pic_num: i64 },
    /// No long-term reference picture in the DPB has this `LongTermPicNum`.
    MissingLongTerm { long_term_pic_num: u32 },
}

impl std::fmt::Display for RefPicListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingShortTerm { pic_num } => {
                write!(f, "ref_pic_list_modification: no short-term reference with PicNum {pic_num}")
            }
            Self::MissingLongTerm { long_term_pic_num } => write!(
                f,
                "ref_pic_list_modification: no long-term reference with LongTermPicNum {long_term_pic_num}"
            ),
        }
    }
}

impl std::error::Error for RefPicListError {}

/// `PicNumF( picX )` (§8.2.4.3.1): the `PicNum` of a short-term reference
/// picture, or "no value" for any picture not marked short-term.
///
/// The spec uses `MaxPicNum` as the sentinel for non-short-term pictures
/// precisely because no valid `picNumLX` can equal it; `None` expresses the
/// same "never matches" property without needing `MaxPicNum` here.
fn pic_num_f(entry: &DpbEntry, ctx: PicNumContext) -> Option<i64> {
    entry.is_short_term.then(|| entry.pic_num(ctx))
}

/// `LongTermPicNumF( picX )` (§8.2.4.3.2): the `LongTermPicNum` of a long-term
/// reference picture, or "no value" (spec sentinel `2 * (MaxLongTermFrameIdx +
/// 1)`, which no signalled `long_term_pic_num` can equal) otherwise.
fn long_term_pic_num_f(entry: &DpbEntry) -> Option<i64> {
    entry.is_long_term.then_some(entry.long_term_pic_num as i64)
}

/// The shared list-splice step of §8.2.4.3.1 / §8.2.4.3.2 (equations 8-38 and
/// 8-39): insert `picture` at `ref_idx`, shift everything after it one place
/// later, then drop any *other* copy of the same picture from the tail.
///
/// The spec grows the list to `num_ref_idx_lX_active_minus1 + 2` entries for
/// the duration of the process and discards the surplus tail entry on the next
/// shift, so the list is re-clamped to `num_active + 1` here to match.
fn splice_into_list(
    list: &mut Vec<DpbEntry>,
    num_active: usize,
    ref_idx: &mut usize,
    picture: DpbEntry,
    is_same_picture: impl Fn(&DpbEntry) -> bool,
) {
    let at = (*ref_idx).min(list.len());
    list.insert(at, picture);
    *ref_idx = at + 1;

    // `for( cIdx = refIdxLX; cIdx <= num_ref_idx_lX_active_minus1 + 1; cIdx++ )
    //     if( PicNumF( RefPicListX[ cIdx ] ) != picNumLX ) ...` — i.e. remove
    // every entry at or after the insertion point that is the same picture.
    let mut i = *ref_idx;
    while i < list.len() {
        if is_same_picture(&list[i]) {
            list.remove(i);
        } else {
            i += 1;
        }
    }
    list.truncate(num_active + 1);
}

/// Apply the reference picture list modification process (§8.2.4.3) to an
/// already-initialised `list` (§8.2.4.2).
///
/// `list` is modified in place and left with at most `num_active` entries.
/// `num_active` is `num_ref_idx_lX_active_minus1 + 1` for the slice — note this
/// is *not* `list.len()`, because the initial list may be shorter than the
/// requested active count when the DPB holds fewer reference pictures.
///
/// Each `ShortTermSubtract`/`ShortTermAdd` command walks a running
/// `picNumLXPred` prediction (starting at `CurrPicNum`) by
/// `abs_diff_pic_num_minus1 + 1`, wrapping modulo `MaxPicNum`, and moves the
/// short-term picture with the resulting `PicNum` to the front of the
/// not-yet-placed part of the list. `LongTerm` does the same by
/// `LongTermPicNum`.
pub fn modify_ref_pic_list(
    list: &mut Vec<DpbEntry>,
    dpb: &Dpb,
    ctx: PicNumContext,
    num_active: usize,
    modifications: &[RefPicListModification],
) -> Result<(), RefPicListError> {
    if modifications.is_empty() {
        return Ok(());
    }

    let max_pic_num = ctx.max_frame_num as i64;
    let curr_pic_num = ctx.curr_frame_num as i64;
    // `picNumLXPred`, initialised to `CurrPicNum` before the first command and
    // carried across commands (§8.2.4.3.1).
    let mut pic_num_pred = curr_pic_num;
    let mut ref_idx = 0usize;

    for modification in modifications {
        match *modification {
            RefPicListModification::ShortTermSubtract {
                abs_diff_pic_num_minus1,
            }
            | RefPicListModification::ShortTermAdd {
                abs_diff_pic_num_minus1,
            } => {
                let subtract = matches!(
                    modification,
                    RefPicListModification::ShortTermSubtract { .. }
                );
                let abs_diff_pic_num = abs_diff_pic_num_minus1 as i64 + 1;

                // picNumLXNoWrap (8-34 / 8-35).
                let no_wrap = if subtract {
                    let v = pic_num_pred - abs_diff_pic_num;
                    if v < 0 {
                        v + max_pic_num
                    } else {
                        v
                    }
                } else {
                    let v = pic_num_pred + abs_diff_pic_num;
                    if v >= max_pic_num {
                        v - max_pic_num
                    } else {
                        v
                    }
                };
                pic_num_pred = no_wrap;

                // picNumLX (8-36).
                let pic_num = if no_wrap > curr_pic_num {
                    no_wrap - max_pic_num
                } else {
                    no_wrap
                };

                let picture = dpb
                    .iter()
                    .find(|e| pic_num_f(e, ctx) == Some(pic_num))
                    .ok_or(RefPicListError::MissingShortTerm { pic_num })?
                    .clone();
                splice_into_list(list, num_active, &mut ref_idx, picture, |e| {
                    pic_num_f(e, ctx) == Some(pic_num)
                });
            }
            RefPicListModification::LongTerm { long_term_pic_num } => {
                let target = long_term_pic_num as i64;
                let picture = dpb
                    .iter()
                    .find(|e| long_term_pic_num_f(e) == Some(target))
                    .ok_or(RefPicListError::MissingLongTerm { long_term_pic_num })?
                    .clone();
                splice_into_list(list, num_active, &mut ref_idx, picture, |e| {
                    long_term_pic_num_f(e) == Some(target)
                });
            }
        }
    }

    list.truncate(num_active);
    Ok(())
}

/// Build `RefPicList0` for a P (or SP) slice: initialisation (§8.2.4.2.1)
/// followed by the modification process (§8.2.4.3).
///
/// Initialisation orders short-term reference pictures by **descending
/// `PicNum`** (`FrameNumWrap` for frame pictures — *not* by `PicOrderCnt`,
/// which is the B-slice rule), followed by long-term pictures ordered by
/// ascending `LongTermPicNum`. The list is then truncated to
/// `num_ref_idx_l0_active` and `modifications` (the slice header's
/// `ref_pic_list_modification_l0` commands, empty for "no modification") are
/// applied.
///
/// If the DPB holds fewer pictures than requested, the last entry is repeated
/// to pad the list out; this is a pragmatic fallback for the spec's
/// "non-existing" entries, which conforming streams never reference.
///
/// Returns `None` when the DPB holds no reference pictures at all, or when a
/// modification command references a picture that is not in the DPB (see
/// [`RefPicListError`]) — in both cases the caller must fall back rather than
/// decode against a wrong reference list.
pub fn build_ref_list_l0(
    dpb: &Dpb,
    num_ref_idx_l0_active: usize,
    ctx: PicNumContext,
    modifications: &[RefPicListModification],
) -> Option<Vec<DpbEntry>> {
    let num_active = num_ref_idx_l0_active.max(1);

    let mut shorts: Vec<&DpbEntry> = dpb.iter().filter(|e| e.is_short_term).collect();
    shorts.sort_by_key(|e| std::cmp::Reverse(e.pic_num(ctx)));
    let mut longs: Vec<&DpbEntry> = dpb.iter().filter(|e| e.is_long_term).collect();
    longs.sort_by_key(|e| e.long_term_pic_num);

    if shorts.is_empty() && longs.is_empty() {
        return None;
    }

    let mut list: Vec<DpbEntry> = shorts.into_iter().chain(longs).cloned().collect();
    list.truncate(num_active);
    modify_ref_pic_list(&mut list, dpb, ctx, num_active, modifications).ok()?;
    while list.len() < num_ref_idx_l0_active {
        let last = list.last()?.clone();
        list.push(last);
    }
    Some(list)
}

/// A reference *field* for interlaced (PAFF / MBAFF) motion compensation.
///
/// A field reference is either a genuine field (a half-height YUV420p buffer
/// stored in the DPB, [`is_field`](FieldRef::is_field) == true) or one field of
/// a stored frame picture (a full-height buffer, sampled at field parity via a
/// doubled vertical stride). Motion compensation in interlaced mode operates on
/// field-line coordinates and uses [`FieldRef::sample_y`] to map a field line to
/// the underlying buffer row (§8.4.2.2.1 / §6.4.10.1).
#[derive(Debug, Clone)]
pub struct FieldRef {
    /// The underlying reconstructed plane set (half-height for a field,
    /// full-height for a frame).
    pub frame: VideoFrame,
    /// `true` when `frame` is a full-height *frame* picture: its two fields are
    /// addressed at even / odd rows rather than as a contiguous half-height
    /// buffer.
    pub is_frame: bool,
    /// When `is_frame`, the parity of the field this reference denotes
    /// (`false` = top, `true` = bottom).
    pub bottom: bool,
    /// `PicOrderCnt` of the underlying picture (used for weighted / temporal
    /// direct-mode derivation).
    pub pic_order_cnt: i64,
}

impl FieldRef {
    /// Whether this reference is a genuine (half-height) field rather than one
    /// field of a full-height frame.
    pub fn is_field(&self) -> bool {
        !self.is_frame
    }

    /// Map a field-line index `y_field` (in field-sample units) to the row in
    /// the underlying buffer. For a field reference the mapping is identity;
    /// for a frame reference the field's samples live at stride-2 spacing with
    /// the field's parity offset (§6.4.10.1).
    pub fn sample_y(&self, y_field: i32) -> i32 {
        if self.is_frame {
            2 * y_field + self.bottom as i32
        } else {
            y_field
        }
    }

    /// The height of one field in samples (half the frame height for a frame
    /// reference, the buffer height for a field reference).
    pub fn field_height(&self) -> u32 {
        if self.is_frame {
            self.frame.height / 2
        } else {
            self.frame.height
        }
    }

    /// Extract the contiguous half-height YUV420p planes for the referenced
    /// field, suitable for field-coordinate motion compensation (§8.4.2.2.1 /
    /// §6.4.10.1).
    ///
    /// For a frame reference (`is_frame == true`) the field's samples live at
    /// stride-2 spacing with the field's parity offset, so every other row is
    /// copied into the contiguous half-height output. For a genuine field
    /// reference the stored buffer is already half-height and is copied as-is.
    /// Returns `(luma, chroma_cb, chroma_cr)`.
    pub fn planes(&self) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let w = self.frame.width as usize;
        let full_h = self.frame.height as usize;
        let luma_len = w * full_h;
        let chroma_w = w / 2;
        let chroma_h = full_h / 2;
        let chroma_len = chroma_w * chroma_h;
        if self.is_frame {
            let fh = full_h / 2;
            let cfh = chroma_h / 2;
            let mut luma = vec![0u8; w * fh];
            let mut cb = vec![0u8; chroma_w * cfh];
            let mut cr = vec![0u8; chroma_w * cfh];
            for y in 0..fh {
                let sy = 2 * y + self.bottom as usize;
                luma[y * w..(y + 1) * w]
                    .copy_from_slice(&self.frame.data[sy * w..(sy + 1) * w]);
            }
            for y in 0..cfh {
                let sy = 2 * y + self.bottom as usize;
                let lo = luma_len + sy * chroma_w;
                cb[y * chroma_w..(y + 1) * chroma_w]
                    .copy_from_slice(&self.frame.data[lo..lo + chroma_w]);
                let lo = luma_len + chroma_len + sy * chroma_w;
                cr[y * chroma_w..(y + 1) * chroma_w]
                    .copy_from_slice(&self.frame.data[lo..lo + chroma_w]);
            }
            (luma, cb, cr)
        } else {
            let luma = self.frame.data[..luma_len].to_vec();
            let cb = self.frame.data[luma_len..luma_len + chroma_len].to_vec();
            let cr = self.frame.data[luma_len + chroma_len..luma_len + 2 * chroma_len].to_vec();
            (luma, cb, cr)
        }
    }
}

/// Build the `RefPicList0` for an interlaced *field* picture (§8.2.4.2.5).
///
/// Reference fields are collected from the DPB — a stored field contributes one
/// field reference, while a stored frame contributes its two fields (top and
/// bottom). They are ordered by descending `PicNum` (short-term) then ascending
/// `LongTermPicNum` (long-term), with the top/bottom group order swapped when
/// the current picture is a bottom field.
///
/// `ref_pic_list_modification` is not yet applied to field lists (most simple
/// interlaced clips do not use it); the list is truncated to
/// `num_ref_idx_l0_active` and padded by repetition like the frame path.
pub fn build_field_ref_list_l0(
    dpb: &Dpb,
    current_bottom: bool,
    num_ref_idx_l0_active: usize,
    ctx: PicNumContext,
) -> Option<Vec<FieldRef>> {
    let num_active = num_ref_idx_l0_active.max(1);
    let mut fields: Vec<FieldRef> = Vec::new();
    for e in dpb.iter() {
        if e.field_pic_flag {
            fields.push(FieldRef {
                frame: e.frame.clone(),
                is_frame: false,
                bottom: e.bottom_field_flag,
                pic_order_cnt: e.pic_order_cnt,
            });
        } else {
            fields.push(FieldRef {
                frame: e.frame.clone(),
                is_frame: true,
                bottom: false,
                pic_order_cnt: e.pic_order_cnt,
            });
            fields.push(FieldRef {
                frame: e.frame.clone(),
                is_frame: true,
                bottom: true,
                pic_order_cnt: e.pic_order_cnt,
            });
        }
    }
    if fields.is_empty() {
        return None;
    }

    // Compute each candidate field's PicNum (short-term) for ordering.
    let mut st_top: Vec<&FieldRef> = Vec::new();
    let mut st_bottom: Vec<&FieldRef> = Vec::new();
    let mut lt_top: Vec<&FieldRef> = Vec::new();
    let mut lt_bottom: Vec<&FieldRef> = Vec::new();
    for f in &fields {
        // Determine the underlying DPB short/long status by matching the frame.
        let is_short = dpb
            .iter()
            .filter(|e| e.frame.height == f.frame.height && e.pic_order_cnt == f.pic_order_cnt)
            .any(|e| e.is_short_term);
        let is_long = !is_short;
        let pic_num = dpb
            .iter()
            .find(|e| e.frame.height == f.frame.height && e.pic_order_cnt == f.pic_order_cnt)
            .map(|e| e.pic_num(ctx))
            .unwrap_or(0);
        if is_short {
            if f.bottom {
                st_bottom.push(f);
            } else {
                st_top.push(f);
            }
        } else if is_long {
            if f.bottom {
                lt_bottom.push(f);
            } else {
                lt_top.push(f);
            }
        } else {
            let _ = pic_num;
        }
    }
    st_top.sort_by_key(|f| std::cmp::Reverse(field_pic_num(f, dpb, ctx)));
    st_bottom.sort_by_key(|f| std::cmp::Reverse(field_pic_num(f, dpb, ctx)));
    lt_top.sort_by_key(|f| field_long_num(f, dpb));
    lt_bottom.sort_by_key(|f| field_long_num(f, dpb));

    let mut ordered: Vec<FieldRef> = Vec::new();
    if current_bottom {
        for f in st_bottom {
            ordered.push(f.clone());
        }
        for f in st_top {
            ordered.push(f.clone());
        }
        for f in lt_bottom {
            ordered.push(f.clone());
        }
        for f in lt_top {
            ordered.push(f.clone());
        }
    } else {
        for f in st_top {
            ordered.push(f.clone());
        }
        for f in st_bottom {
            ordered.push(f.clone());
        }
        for f in lt_top {
            ordered.push(f.clone());
        }
        for f in lt_bottom {
            ordered.push(f.clone());
        }
    }

    ordered.truncate(num_active);
    while ordered.len() < num_ref_idx_l0_active {
        let last = ordered.last()?.clone();
        ordered.push(last);
    }
    Some(ordered)
}

/// Build the `RefPicList1` for an interlaced *field* picture (§8.2.4.2.5).
///
/// For field pictures both reference lists are constructed by the same
/// field-ordering rule (unlike frame pictures, where L0 / L1 differ by POC
/// direction); see [`build_field_ref_list_l0`].
pub fn build_field_ref_list_l1(
    dpb: &Dpb,
    current_bottom: bool,
    num_ref_idx_l1_active: usize,
    ctx: PicNumContext,
) -> Option<Vec<FieldRef>> {
    build_field_ref_list_l0(dpb, current_bottom, num_ref_idx_l1_active, ctx)
}

/// `PicNum` of a candidate field reference (§8.2.4.2.5), doubled for fields.
fn field_pic_num(f: &FieldRef, dpb: &Dpb, ctx: PicNumContext) -> i64 {
    dpb.iter()
        .find(|e| e.frame.height == f.frame.height && e.pic_order_cnt == f.pic_order_cnt)
        .map(|e| e.pic_num(ctx))
        .unwrap_or(0)
}

/// `LongTermPicNum` of a candidate long-term field reference.
fn field_long_num(f: &FieldRef, dpb: &Dpb) -> i64 {
    dpb.iter()
        .find(|e| e.frame.height == f.frame.height && e.pic_order_cnt == f.pic_order_cnt)
        .map(|e| e.long_term_pic_num as i64)
        .unwrap_or(0)
}

/// Build `RefPicList0` for a B-slice (§8.2.4.2.3).
///
/// Distinct from the P-slice ordering: short-term references are grouped by
/// `PicOrderCnt` relative to `current_poc`:
/// 1. Short-term with `POC < current_poc`, descending POC.
/// 2. Short-term with `POC >= current_poc`, ascending POC.
/// 3. Long-term, ascending `LongTermPicNum`.
///
/// Returns `None` when the DPB holds no reference pictures.
pub fn build_ref_list_l0_b_slice(
    dpb: &Dpb,
    num_ref_idx_l0_active: usize,
    current_poc: i64,
    ctx: PicNumContext,
    modifications: &[RefPicListModification],
) -> Option<Vec<DpbEntry>> {
    let num_active = num_ref_idx_l0_active.max(1);

    let mut before: Vec<&DpbEntry> = dpb
        .iter()
        .filter(|e| e.is_short_term && e.pic_order_cnt < current_poc)
        .collect();
    before.sort_by_key(|e| std::cmp::Reverse(e.pic_order_cnt));

    let mut at_or_after: Vec<&DpbEntry> = dpb
        .iter()
        .filter(|e| e.is_short_term && e.pic_order_cnt >= current_poc)
        .collect();
    at_or_after.sort_by_key(|e| e.pic_order_cnt);

    let mut longs: Vec<&DpbEntry> = dpb.iter().filter(|e| e.is_long_term).collect();
    longs.sort_by_key(|e| e.long_term_pic_num);

    if before.is_empty() && at_or_after.is_empty() && longs.is_empty() {
        return None;
    }

    let mut list: Vec<DpbEntry> = before
        .into_iter()
        .chain(at_or_after)
        .chain(longs)
        .cloned()
        .collect();
    list.truncate(num_active);
    modify_ref_pic_list(&mut list, dpb, ctx, num_active, modifications).ok()?;
    while list.len() < num_ref_idx_l0_active {
        let last = list.last()?.clone();
        list.push(last);
    }
    Some(list)
}

/// Build `RefPicList1` for a B-slice (§8.2.4.2.3).
///
/// The initial list ordering is:
/// 1. Short-term references with `PicOrderCnt > current_poc`, ascending POC.
/// 2. Short-term references with `PicOrderCnt <= current_poc`, descending POC.
/// 3. Long-term references, ascending `LongTermPicNum`.
///
/// Returns `None` when the DPB holds no reference pictures.
pub fn build_ref_list_l1(
    dpb: &Dpb,
    num_ref_idx_l1_active: usize,
    current_poc: i64,
    ctx: PicNumContext,
    modifications: &[RefPicListModification],
) -> Option<Vec<DpbEntry>> {
    let num_active = num_ref_idx_l1_active.max(1);

    let mut after: Vec<&DpbEntry> = dpb
        .iter()
        .filter(|e| e.is_short_term && e.pic_order_cnt > current_poc)
        .collect();
    after.sort_by_key(|e| e.pic_order_cnt);

    let mut at_or_before: Vec<&DpbEntry> = dpb
        .iter()
        .filter(|e| e.is_short_term && e.pic_order_cnt <= current_poc)
        .collect();
    at_or_before.sort_by_key(|e| std::cmp::Reverse(e.pic_order_cnt));

    let mut longs: Vec<&DpbEntry> = dpb.iter().filter(|e| e.is_long_term).collect();
    longs.sort_by_key(|e| e.long_term_pic_num);

    if after.is_empty() && at_or_before.is_empty() && longs.is_empty() {
        return None;
    }

    let mut list: Vec<DpbEntry> = after
        .into_iter()
        .chain(at_or_before)
        .chain(longs)
        .cloned()
        .collect();
    list.truncate(num_active);
    modify_ref_pic_list(&mut list, dpb, ctx, num_active, modifications).ok()?;
    while list.len() < num_ref_idx_l1_active {
        let last = list.last()?.clone();
        list.push(last);
    }
    Some(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::ScalingLists;
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
            mb_adaptive_frame_field_flag: false,
            frame_cropping_flag: false,
            frame_crop_left_offset: 0,
            frame_crop_right_offset: 0,
            frame_crop_top_offset: 0,
            frame_crop_bottom_offset: 0,
            scaling: ScalingLists::flat(),
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
            field_pic_flag: false,
            bottom_field_flag: false,
            pic_order_cnt: poc,
            is_short_term: true,
            is_long_term: false,
            long_term_pic_num: -1,
            mv_grid: None,
        }
    }

    /// `PicNumContext` with `MaxFrameNum` = 16 (`log2_max_frame_num_minus4 ==
    /// 0`), matching the `sps(..)` helper's default.
    fn ctx(curr_frame_num: u32) -> PicNumContext {
        PicNumContext {
            curr_frame_num,
            max_frame_num: 16,
            field_pic_flag: false,
            bottom_field_flag: false,
        }
    }

    /// Store `e` with sliding-window marking, using the entry's own `frame_num`
    /// as `CurrPicNum` (which is what the decoder does when it stores a
    /// just-decoded picture).
    fn push(dpb: &mut Dpb, e: DpbEntry, max_num_ref_frames: u32) {
        let frame_num = e.frame_num;
        dpb.push(e, ctx(frame_num), max_num_ref_frames);
    }

    /// Store a picture as a long-term reference with `long_term_frame_idx`, via
    /// the real MMCO 6 path (§8.2.5.4.6) rather than by hand-setting flags —
    /// the marking process is the only way a picture becomes long-term.
    fn push_long_term(
        dpb: &mut Dpb,
        frame_num: u32,
        poc: i64,
        long_term_frame_idx: u32,
        max_num_ref_frames: u32,
    ) {
        dpb.mark_decoded_picture(
            entry(frame_num, poc),
            &DecRefPicMarking::Adaptive(vec![MmcoOp::CurrentToLongTerm {
                long_term_frame_idx,
            }]),
            ctx(frame_num),
            max_num_ref_frames,
        )
        .expect("mmco 6 stores a long-term reference");
    }


    fn frame_nums(list: &[DpbEntry]) -> Vec<u32> {
        list.iter().map(|e| e.frame_num).collect()
    }

    #[test]
    fn poc_type0_idr_resets_and_increments() {
        let sps = sps(0, 4, 0, 1); // MaxPicOrderCntLsb = 256
        let mut state = PocState::default();
        let poc = derive_pic_order_cnt(&sps, true, true, 0, Some(0), false, false, None, &mut state).unwrap();
        assert_eq!(poc, 0);
        let poc = derive_pic_order_cnt(&sps, false, true, 1, Some(1), false, false, None, &mut state).unwrap();
        assert_eq!(poc, 1);
        let poc = derive_pic_order_cnt(&sps, false, true, 2, Some(2), false, false, None, &mut state).unwrap();
        assert_eq!(poc, 2);
    }

    #[test]
    fn poc_type0_msb_wraparound_forward() {
        // MaxPicOrderCntLsb = 16, wrap threshold half = 8.
        let sps = sps(0, 0, 0, 1);
        let mut state = PocState::default();
        derive_pic_order_cnt(&sps, true, true, 0, Some(0), false, false, None, &mut state).unwrap();
        // lsb 6 (poc 6), then lsb 13 (diff 7 <= 8, no wrap) → poc 13,
        // then lsb 2: 13 - 2 = 11 >= 8 → msb += 16 → 18.
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 1, Some(6), false, false, None, &mut state).unwrap(),
            6
        );
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 2, Some(13), false, false, None, &mut state).unwrap(),
            13
        );
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 3, Some(2), false, false, None, &mut state).unwrap(),
            18
        );
    }

    #[test]
    fn poc_type0_msb_wraparound_backward() {
        let sps = sps(0, 0, 0, 1); // MaxPicOrderCntLsb = 16
        let mut state = PocState::default();
        derive_pic_order_cnt(&sps, true, true, 0, Some(0), false, false, None, &mut state).unwrap();
        // lsb 2 (poc 2), then lsb 14: 14 - 2 = 12 > 8 → msb -= 16 → -2.
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 1, Some(2), false, false, None, &mut state).unwrap(),
            2
        );
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 2, Some(14), false, false, None, &mut state).unwrap(),
            -2
        );
    }

    #[test]
    fn poc_type0_non_reference_does_not_advance_state() {
        let sps = sps(0, 4, 0, 1);
        let mut state = PocState::default();
        derive_pic_order_cnt(&sps, true, true, 0, Some(0), false, false, None, &mut state).unwrap();
        // Non-reference picture with lsb 50 must not update prev state.
        let poc = derive_pic_order_cnt(&sps, false, false, 1, Some(50), false, false, None, &mut state).unwrap();
        assert_eq!(poc, 50);
        assert_eq!(state.prev_top_field_order_cnt, 0);
        assert_eq!(state.prev_bottom_field_order_cnt, 0);
        assert_eq!(state.prev_frame_num, 0);
    }

    #[test]
    fn poc_type2_doubles_frame_num() {
        let sps = sps(2, 0, 4, 1); // MaxFrameNum = 256
        let mut state = PocState::default();
        assert_eq!(derive_pic_order_cnt(&sps, true, true, 0, None, false, false, None, &mut state).unwrap(), 0);
        assert_eq!(derive_pic_order_cnt(&sps, false, true, 1, None, false, false, None, &mut state).unwrap(), 2);
        assert_eq!(derive_pic_order_cnt(&sps, false, true, 2, None, false, false, None, &mut state).unwrap(), 4);
    }

    /// §8.2.1.3 — field-coded `pic_order_cnt_type == 2`: the bottom field gets
    /// `PicOrderCnt = (frame_num_offset + frame_num) * 2 + 1` and the top field
    /// `+ 0`; the two fields of one frame are thus 1 apart.
    #[test]
    fn poc_type2_field_top_and_bottom_differ_by_one() {
        let sps = sps(2, 0, 4, 1);
        let mut state = PocState::default();
        // IDR top field (frame 0): 0
        assert_eq!(
            derive_pic_order_cnt(&sps, true, true, 0, None, true, false, None, &mut state).unwrap(),
            0
        );
        // IDR bottom field (frame 0): 1
        assert_eq!(
            derive_pic_order_cnt(&sps, true, true, 0, None, true, true, None, &mut state).unwrap(),
            1
        );
        // Next frame's top field: (1 * 2) + 0 = 2 (frame_num advances, top=+0)
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 1, None, true, false, None, &mut state).unwrap(),
            2
        );
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 1, None, true, true, None, &mut state).unwrap(),
            3
        );
    }

    /// §8.2.1.1 — field-coded `pic_order_cnt_type == 0`: each field carries its
    /// own `pic_order_cnt_lsb` in a separate slice, and only that field's
    /// predecessor order count advances (so a later frame's prediction sees the
    /// larger of the two previous fields).
    #[test]
    fn poc_type0_field_top_and_bottom_use_separate_lsb() {
        let sps = sps(0, 4, 0, 1); // MaxPicOrderCntLsb = 256
        let mut state = PocState::default();
        // IDR top field of frame 0: lsb 0 → 0
        assert_eq!(
            derive_pic_order_cnt(&sps, true, true, 0, Some(0), true, false, None, &mut state).unwrap(),
            0
        );
        // IDR bottom field of frame 0: lsb 1 → 1
        assert_eq!(
            derive_pic_order_cnt(&sps, true, true, 0, Some(1), true, true, None, &mut state).unwrap(),
            1
        );
        // Frame 1 top field: lsb 2 → 2
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 1, Some(2), true, false, None, &mut state).unwrap(),
            2
        );
        // Frame 1 bottom field: lsb 3 → 3
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 1, Some(3), true, true, None, &mut state).unwrap(),
            3
        );
    }

    /// §8.2.1.1 — frame-coded `pic_order_cnt_type == 0` with
    /// `bottom_field_pic_order_in_frame_present_flag`: `BottomFieldOrderCnt =
    /// TopFieldOrderCnt + delta_pic_order_cnt_bottom`, and both previous field
    /// order counts advance so the next picture's MSB/LSB predictor is correct.
    #[test]
    fn poc_type0_frame_with_delta_pic_order_cnt_bottom() {
        let sps = sps(0, 4, 0, 1); // MaxPicOrderCntLsb = 256
        let mut state = PocState::default();
        // Frame 0: top lsb 0, delta bottom 4 → top = 0, bottom = 4.
        assert_eq!(
            derive_pic_order_cnt(&sps, true, true, 0, Some(0), false, false, Some(4), &mut state).unwrap(),
            0
        );
        assert_eq!(state.prev_top_field_order_cnt, 0);
        assert_eq!(state.prev_bottom_field_order_cnt, 4);
        // Frame 1: top lsb 2 → msb 0 (0 is the larger prev field, lsb 0; 2 > 0
        // but diff 2 < half(128)) → top = 2, bottom = 2 + 6 = 8.
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 1, Some(2), false, false, Some(6), &mut state).unwrap(),
            2
        );
        assert_eq!(state.prev_top_field_order_cnt, 2);
        assert_eq!(state.prev_bottom_field_order_cnt, 8);
    }

    /// §8.2.1.1 — `delta_pic_order_cnt_bottom` is ignored for field pictures: a
    /// bottom field gets its order count from its own `pic_order_cnt_lsb`, not
    /// from a frame-level `delta`.
    #[test]
    fn poc_type0_field_ignores_delta_pic_order_cnt_bottom() {
        let sps = sps(0, 4, 0, 1);
        let mut state = PocState::default();
        derive_pic_order_cnt(&sps, true, true, 0, Some(0), true, false, None, &mut state).unwrap();
        // A top field with delta present must NOT fold the delta into its POC.
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 1, Some(5), true, false, Some(99), &mut state).unwrap(),
            5
        );
    }

    #[test]
    fn ref_list_l0_short_term_descending_pic_num() {
        // §8.2.4.2.1 orders short-term refs by descending PicNum
        // (FrameNumWrap), *not* by PicOrderCnt. The POCs here are deliberately
        // shuffled relative to frame_num so the two rules disagree.
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 4), 4);
        push(&mut dpb, entry(1, 0), 4);
        push(&mut dpb, entry(2, 8), 4);
        let list = build_ref_list_l0(&dpb, 2, ctx(3), &[]).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].frame_num, 2);
        assert_eq!(list[1].frame_num, 1);
    }

    #[test]
    fn ref_list_l0_short_term_ordering_handles_frame_num_wrap() {
        // MaxFrameNum = 16, current frame_num = 1: frame_num 14/15 predate the
        // wrap, so their FrameNumWrap goes negative and they sort *after* 0/1.
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(14, 0), 8);
        push(&mut dpb, entry(15, 0), 8);
        push(&mut dpb, entry(0, 0), 8);
        push(&mut dpb, entry(1, 0), 8);
        let list = build_ref_list_l0(&dpb, 4, ctx(1), &[]).unwrap();
        let order: Vec<u32> = list.iter().map(|e| e.frame_num).collect();
        assert_eq!(order, vec![1, 0, 15, 14]);
    }

    #[test]
    fn ref_list_l0_long_term_after_short_term() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 4), 4);
        push_long_term(&mut dpb, 1, 0, 0, 4);
        let list = build_ref_list_l0(&dpb, 2, ctx(2), &[]).unwrap();
        assert_eq!(list[0].pic_order_cnt, 4);
        assert!(list[0].is_short_term);
        assert!(list[1].is_long_term);
    }

    #[test]
    fn ref_list_l0_repeats_last_when_dpb_short() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 4), 4);
        let list = build_ref_list_l0(&dpb, 3, ctx(1), &[]).unwrap();
        assert_eq!(list.len(), 3);
        assert!(list.iter().all(|e| e.pic_order_cnt == 4));
    }

    #[test]
    fn ref_list_l0_empty_dpb_returns_none() {
        let dpb = Dpb::new();
        assert!(build_ref_list_l0(&dpb, 1, ctx(0), &[]).is_none());
    }

    // ---- §8.2.4.3 reference picture list modification -------------------

    /// The headline Phase E.1 test: an explicit reorder command must produce a
    /// different `RefPicList0` than §8.2.4.2 initialisation alone.
    #[test]
    fn modification_reorders_p_slice_list_away_from_default() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 0), 4);
        push(&mut dpb, entry(1, 2), 4);
        push(&mut dpb, entry(2, 4), 4);
        push(&mut dpb, entry(3, 6), 4);
        let ctx = ctx(4); // CurrPicNum = 4, MaxPicNum = 16

        let default = build_ref_list_l0(&dpb, 4, ctx, &[]).unwrap();
        assert_eq!(frame_nums(&default), vec![3, 2, 1, 0]);

        // picNumLXPred starts at CurrPicNum (4); abs_diff_pic_num_minus1 = 3
        // subtracts 4 → picNumLX = 0, moving frame_num 0 to the head.
        let mods = [RefPicListModification::ShortTermSubtract {
            abs_diff_pic_num_minus1: 3,
        }];
        let modified = build_ref_list_l0(&dpb, 4, ctx, &mods).unwrap();

        assert_eq!(frame_nums(&modified), vec![0, 3, 2, 1]);
        assert_ne!(frame_nums(&modified), frame_nums(&default));
        // Reordering is a permutation: the same pictures, in a new order.
        let (mut a, mut b) = (frame_nums(&modified), frame_nums(&default));
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b);
    }

    #[test]
    fn modification_pred_is_carried_across_commands() {
        let mut dpb = Dpb::new();
        for frame_num in 0..4 {
            push(&mut dpb, entry(frame_num, frame_num as i64 * 2), 4);
        }
        let ctx = ctx(4);

        // Command 1: pred 4 − 2 = 2 → place frame_num 2 at index 0.
        // Command 2: pred 2 − 1 = 1 → place frame_num 1 at index 1.
        // The second command proves picNumLXPred carried the *first* command's
        // picNumLXNoWrap (2) rather than resetting to CurrPicNum (4).
        let mods = [
            RefPicListModification::ShortTermSubtract {
                abs_diff_pic_num_minus1: 1,
            },
            RefPicListModification::ShortTermSubtract {
                abs_diff_pic_num_minus1: 0,
            },
        ];
        let list = build_ref_list_l0(&dpb, 4, ctx, &mods).unwrap();
        assert_eq!(frame_nums(&list), vec![2, 1, 3, 0]);
    }

    #[test]
    fn modification_short_term_add_wraps_modulo_max_pic_num() {
        // MaxPicNum = 16, CurrPicNum = 1. Adding 2 to the pred gives
        // picNumLXNoWrap = 3, which is > CurrPicNum, so picNumLX = 3 − 16 =
        // −13 — the FrameNumWrap of the pre-wrap picture frame_num 3.
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(3, 0), 8);
        push(&mut dpb, entry(0, 2), 8);
        push(&mut dpb, entry(1, 4), 8);
        let ctx = ctx(1);

        assert_eq!(
            frame_nums(&build_ref_list_l0(&dpb, 3, ctx, &[]).unwrap()),
            vec![1, 0, 3]
        );

        let mods = [RefPicListModification::ShortTermAdd {
            abs_diff_pic_num_minus1: 1,
        }];
        let list = build_ref_list_l0(&dpb, 3, ctx, &mods).unwrap();
        assert_eq!(frame_nums(&list), vec![3, 1, 0]);
    }

    #[test]
    fn modification_promotes_long_term_picture() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 0), 4);
        push(&mut dpb, entry(1, 2), 4);
        push_long_term(&mut dpb, 2, 0, 7, 4);
        let ctx = ctx(3);

        let default = build_ref_list_l0(&dpb, 3, ctx, &[]).unwrap();
        assert_eq!(frame_nums(&default), vec![1, 0, 2]);
        assert!(default[2].is_long_term);

        let mods = [RefPicListModification::LongTerm {
            long_term_pic_num: 7,
        }];
        let list = build_ref_list_l0(&dpb, 3, ctx, &mods).unwrap();
        assert_eq!(frame_nums(&list), vec![2, 1, 0]);
        assert!(list[0].is_long_term);
    }

    #[test]
    fn modification_leaves_list_length_unchanged() {
        let mut dpb = Dpb::new();
        for frame_num in 0..4 {
            push(&mut dpb, entry(frame_num, frame_num as i64), 4);
        }
        let mods = [RefPicListModification::ShortTermSubtract {
            abs_diff_pic_num_minus1: 2,
        }];
        for num_active in 1..=4 {
            let list = build_ref_list_l0(&dpb, num_active, ctx(4), &mods).unwrap();
            assert_eq!(list.len(), num_active, "num_active = {num_active}");
            assert_eq!(list[0].frame_num, 1);
        }
    }

    #[test]
    fn modification_can_pull_in_a_picture_the_truncated_list_dropped() {
        // num_ref_idx_l0_active = 1 truncates the initial list to {frame_num
        // 3}; the reorder still has the whole DPB to select from (§8.2.4.3
        // looks the picture up in the DPB, not in the truncated list).
        let mut dpb = Dpb::new();
        for frame_num in 0..4 {
            push(&mut dpb, entry(frame_num, frame_num as i64), 4);
        }
        let mods = [RefPicListModification::ShortTermSubtract {
            abs_diff_pic_num_minus1: 3,
        }];
        let list = build_ref_list_l0(&dpb, 1, ctx(4), &mods).unwrap();
        assert_eq!(frame_nums(&list), vec![0]);
    }

    #[test]
    fn modification_referencing_absent_picture_fails_rather_than_guessing() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 0), 4);
        let ctx = ctx(1);
        let mut list = vec![entry(0, 0)];

        // pred 1 − 2 = −1 → +MaxPicNum (16) = 15 > CurrPicNum → picNumLX = −1,
        // which no picture in the DPB has.
        let err = modify_ref_pic_list(
            &mut list,
            &dpb,
            ctx,
            1,
            &[RefPicListModification::ShortTermSubtract {
                abs_diff_pic_num_minus1: 1,
            }],
        )
        .unwrap_err();
        assert_eq!(err, RefPicListError::MissingShortTerm { pic_num: -1 });

        let err = modify_ref_pic_list(
            &mut list,
            &dpb,
            ctx,
            1,
            &[RefPicListModification::LongTerm {
                long_term_pic_num: 4,
            }],
        )
        .unwrap_err();
        assert_eq!(
            err,
            RefPicListError::MissingLongTerm {
                long_term_pic_num: 4
            }
        );

        // The public builder converts that into "cannot build a list", so the
        // caller falls back instead of decoding against a wrong reference.
        assert!(build_ref_list_l0(
            &dpb,
            1,
            ctx,
            &[RefPicListModification::LongTerm {
                long_term_pic_num: 4
            }]
        )
        .is_none());
    }

    #[test]
    fn empty_modification_list_is_a_no_op() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 0), 4);
        push(&mut dpb, entry(1, 2), 4);
        let ctx = ctx(2);
        let mut list = build_ref_list_l0(&dpb, 2, ctx, &[]).unwrap();
        let before = frame_nums(&list);
        modify_ref_pic_list(&mut list, &dpb, ctx, 2, &[]).unwrap();
        assert_eq!(frame_nums(&list), before);
    }

    #[test]
    fn sliding_window_keeps_most_recent_refs() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 0), 1);
        assert_eq!(dpb.len(), 1);
        push(&mut dpb, entry(1, 1), 1);
        assert_eq!(dpb.len(), 1);
        assert_eq!(dpb.iter().next().unwrap().pic_order_cnt, 1);
        assert_eq!(dpb.iter().next().unwrap().frame_num, 1);
    }

    #[test]
    fn push_replaces_same_frame_num() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(5, 0), 4);
        push(&mut dpb, entry(5, 99), 4);
        assert_eq!(dpb.len(), 1);
        assert_eq!(dpb.iter().next().unwrap().pic_order_cnt, 99);
    }

    /// §8.2.5.3 evicts the short-term reference with the smallest
    /// **FrameNumWrap**, which is not the same as the smallest `PicOrderCnt`
    /// once `frame_num` has wrapped: with `MaxFrameNum == 16` and
    /// `CurrPicNum == 1`, frame_num 15 predates the wrap (FrameNumWrap = −1)
    /// and must go before frame_num 0 even though its POC is the largest.
    #[test]
    fn sliding_window_evicts_smallest_frame_num_wrap_not_smallest_poc() {
        let mut dpb = Dpb::new();
        dpb.push(entry(15, 100), ctx(15), 2);
        dpb.push(entry(0, 0), ctx(0), 2);
        assert_eq!(dpb.len(), 2);

        // Storing frame_num 1 must free a slot: FrameNumWrap(15) = −1 <
        // FrameNumWrap(0) = 0, so the pre-wrap picture is the victim.
        dpb.push(entry(1, 50), ctx(1), 2);
        assert_eq!(frame_nums(&dpb.iter().cloned().collect::<Vec<_>>()), vec![0, 1]);
    }

    // ---- §8.2.5 decoded reference picture marking ------------------------

    /// Store `current` with the given marking, using `MaxFrameNum == 16`.
    fn mark(
        dpb: &mut Dpb,
        current: DpbEntry,
        marking: &DecRefPicMarking,
        max_num_ref_frames: u32,
    ) -> Result<MarkingOutcome, MmcoError> {
        let frame_num = current.frame_num;
        dpb.mark_decoded_picture(current, marking, ctx(frame_num), max_num_ref_frames)
    }

    fn adaptive(ops: &[MmcoOp]) -> DecRefPicMarking {
        DecRefPicMarking::Adaptive(ops.to_vec())
    }

    fn dpb_frame_nums(dpb: &Dpb) -> Vec<u32> {
        dpb.iter().map(|e| e.frame_num).collect()
    }

    /// Headline Phase E.2 test #1: MMCO 1 marks one specific short-term
    /// reference "unused for reference" and leaves the rest of the DPB alone.
    #[test]
    fn mmco1_marks_the_selected_short_term_picture_unused() {
        let mut dpb = Dpb::new();
        for frame_num in 0..3u32 {
            push(&mut dpb, entry(frame_num, frame_num as i64 * 2), 4);
        }
        assert_eq!(dpb_frame_nums(&dpb), vec![0, 1, 2]);

        // CurrPicNum = 3; difference_of_pic_nums_minus1 = 2 selects
        // picNumX = 3 − 3 = 0 (§8.2.5.4.1, equation 8-40).
        let outcome = mark(
            &mut dpb,
            entry(3, 6),
            &adaptive(&[MmcoOp::ShortTermUnused {
                difference_of_pic_nums_minus1: 2,
            }]),
            4,
        )
        .expect("mmco 1");

        assert!(!outcome.mmco5);
        assert!(!outcome.current_is_long_term);
        // frame_num 0 is gone; 1 and 2 survive and the current picture joined
        // them as a short-term reference.
        assert_eq!(dpb_frame_nums(&dpb), vec![1, 2, 3]);
        assert!(dpb.iter().all(|e| e.is_short_term && !e.is_long_term));
        assert_eq!(dpb.num_short_term(), 3);
        assert_eq!(dpb.num_long_term(), 0);
    }

    /// Headline Phase E.2 test #2: MMCO 5 empties the DPB, resets
    /// `MaxLongTermFrameIdx`, and rebases the current picture to
    /// `frame_num == 0` / `PicOrderCnt == 0` (§8.2.5.4.5, §7.4.3, §8.2.1.1).
    #[test]
    fn mmco5_resets_the_dpb_and_rebases_the_current_picture() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 0), 4);
        push(&mut dpb, entry(1, 2), 4);
        push_long_term(&mut dpb, 2, 4, 3, 4);
        assert_eq!(dpb.len(), 3);
        assert_eq!(dpb.num_long_term(), 1);

        let outcome = mark(&mut dpb, entry(3, 6), &adaptive(&[MmcoOp::ResetAll]), 4)
            .expect("mmco 5");

        assert!(outcome.mmco5);
        assert!(!outcome.current_is_long_term);
        // Every prior reference is gone; only the current picture remains, and
        // it is renumbered to frame_num 0 with PicOrderCnt 0.
        assert_eq!(dpb.len(), 1);
        let stored = dpb.iter().next().unwrap();
        assert_eq!(stored.frame_num, 0);
        assert_eq!(stored.pic_order_cnt, 0);
        assert!(stored.is_short_term);
        assert!(!stored.is_long_term);
        assert_eq!(dpb.max_long_term_frame_idx(), None);
    }

    /// The POC state the decoder carries must follow MMCO 5 too, otherwise the
    /// next picture's `PicOrderCntMsb` is derived against a stale predecessor.
    #[test]
    fn mmco5_poc_state_reset_makes_the_next_picture_start_from_zero() {
        let sps = sps(0, 4, 0, 4); // MaxPicOrderCntLsb = 256, wrap threshold 128
        let mut state = PocState::default();
        derive_pic_order_cnt(&sps, true, true, 0, Some(0), false, false, None, &mut state).unwrap();
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 1, Some(100), false, false, None, &mut state).unwrap(),
            100
        );
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 2, Some(200), false, false, None, &mut state).unwrap(),
            200
        );

        // Without the reset, the previous order count stays at 200 and the next
        // picture's lsb of 2 is read as a wrap: 200 − 2 >= 128 → msb += 256.
        let mut stale = state.clone();
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 3, Some(2), false, false, None, &mut stale).unwrap(),
            258
        );

        state.reset_after_mmco5();
        assert_eq!(state.prev_top_field_order_cnt, 0);
        assert_eq!(state.prev_bottom_field_order_cnt, 0);
        assert_eq!(state.prev_frame_num, 0);
        assert_eq!(
            derive_pic_order_cnt(&sps, false, true, 3, Some(2), false, false, None, &mut state).unwrap(),
            2
        );
    }

    #[test]
    fn mmco2_marks_the_selected_long_term_picture_unused() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 0), 4);
        push_long_term(&mut dpb, 1, 2, 2, 4);
        assert_eq!(dpb.num_long_term(), 1);

        mark(
            &mut dpb,
            entry(2, 4),
            &adaptive(&[MmcoOp::LongTermUnused {
                long_term_pic_num: 2,
            }]),
            4,
        )
        .expect("mmco 2");

        assert_eq!(dpb.num_long_term(), 0);
        assert_eq!(dpb_frame_nums(&dpb), vec![0, 2]);
    }

    #[test]
    fn mmco3_converts_a_short_term_picture_to_long_term() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 0), 4);
        push(&mut dpb, entry(1, 2), 4);

        // CurrPicNum = 2, difference_of_pic_nums_minus1 = 1 → picNumX = 0.
        mark(
            &mut dpb,
            entry(2, 4),
            &adaptive(&[MmcoOp::ShortTermToLongTerm {
                difference_of_pic_nums_minus1: 1,
                long_term_frame_idx: 5,
            }]),
            4,
        )
        .expect("mmco 3");

        let converted = dpb.iter().find(|e| e.frame_num == 0).expect("frame 0");
        assert!(!converted.is_short_term);
        assert!(converted.is_long_term);
        assert_eq!(converted.long_term_pic_num, 5);
        assert_eq!(dpb.num_short_term(), 2); // frame_num 1 and the current pic
        assert_eq!(dpb.num_long_term(), 1);
    }

    /// §8.2.5.4.3: reusing a `LongTermFrameIdx` drops the picture that held it.
    #[test]
    fn mmco3_reusing_a_long_term_frame_idx_evicts_the_previous_holder() {
        let mut dpb = Dpb::new();
        push_long_term(&mut dpb, 7, 14, 1, 4);
        push(&mut dpb, entry(0, 0), 4);

        mark(
            &mut dpb,
            entry(1, 2),
            &adaptive(&[MmcoOp::ShortTermToLongTerm {
                difference_of_pic_nums_minus1: 0,
                long_term_frame_idx: 1,
            }]),
            4,
        )
        .expect("mmco 3");

        assert!(
            !dpb.iter().any(|e| e.frame_num == 7),
            "the previous holder of LongTermFrameIdx 1 must be dropped"
        );
        let converted = dpb.iter().find(|e| e.frame_num == 0).expect("frame 0");
        assert!(converted.is_long_term);
        assert_eq!(converted.long_term_pic_num, 1);
    }

    #[test]
    fn mmco4_sets_max_long_term_frame_idx_and_drops_higher_indices() {
        let mut dpb = Dpb::new();
        for (frame_num, idx) in [(5u32, 0u32), (6, 1), (7, 2)] {
            push_long_term(&mut dpb, frame_num, frame_num as i64 * 2, idx, 8);
        }
        assert_eq!(dpb.num_long_term(), 3);

        // max_long_term_frame_idx_plus1 = 2 → MaxLongTermFrameIdx = 1, so the
        // picture holding index 2 is marked unused.
        mark(
            &mut dpb,
            entry(1, 2),
            &adaptive(&[MmcoOp::SetMaxLongTermFrameIdx {
                max_long_term_frame_idx_plus1: 2,
            }]),
            8,
        )
        .expect("mmco 4");

        assert_eq!(dpb.max_long_term_frame_idx(), Some(1));
        assert_eq!(dpb.num_long_term(), 2);
        assert!(!dpb.iter().any(|e| e.frame_num == 7));

        // plus1 == 0 is "no long-term frame indices": every long-term goes.
        mark(
            &mut dpb,
            entry(2, 4),
            &adaptive(&[MmcoOp::SetMaxLongTermFrameIdx {
                max_long_term_frame_idx_plus1: 0,
            }]),
            8,
        )
        .expect("mmco 4 reset");
        assert_eq!(dpb.max_long_term_frame_idx(), None);
        assert_eq!(dpb.num_long_term(), 0);
    }

    #[test]
    fn mmco6_marks_the_current_picture_long_term() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 0), 4);

        let outcome = mark(
            &mut dpb,
            entry(1, 2),
            &adaptive(&[MmcoOp::CurrentToLongTerm {
                long_term_frame_idx: 3,
            }]),
            4,
        )
        .expect("mmco 6");

        assert!(outcome.current_is_long_term);
        let current = dpb.iter().find(|e| e.frame_num == 1).expect("current");
        assert!(current.is_long_term);
        assert!(!current.is_short_term);
        assert_eq!(current.long_term_pic_num, 3);
        // The other picture is untouched: MMCO 6 only claims the current one.
        assert!(dpb.iter().find(|e| e.frame_num == 0).unwrap().is_short_term);
    }

    /// §8.2.5.1: a picture with `adaptive_ref_pic_marking_mode_flag == 1` does
    /// **not** run the sliding-window process, so the DPB may legitimately sit
    /// at capacity without evicting anything.
    #[test]
    fn adaptive_marking_overrides_sliding_window() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 0), 2);
        push(&mut dpb, entry(1, 2), 2);
        assert_eq!(dpb.len(), 2);

        // Sliding window would drop frame_num 0 to make room. MMCO 1 instead
        // names frame_num 1 (CurrPicNum 2 − 1), so 0 survives.
        mark(
            &mut dpb,
            entry(2, 4),
            &adaptive(&[MmcoOp::ShortTermUnused {
                difference_of_pic_nums_minus1: 0,
            }]),
            2,
        )
        .expect("mmco 1");

        assert_eq!(dpb_frame_nums(&dpb), vec![0, 2]);
    }

    /// An MMCO list that frees nothing must not be able to grow the DPB without
    /// bound — each entry owns a decoded frame (FFmpeg applies the same
    /// defensive clamp).
    #[test]
    fn adaptive_marking_still_clamps_a_malformed_overfull_dpb() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 0), 2);
        push(&mut dpb, entry(1, 2), 2);

        for frame_num in 2..6u32 {
            mark(
                &mut dpb,
                entry(frame_num, frame_num as i64 * 2),
                &adaptive(&[]),
                2,
            )
            .expect("empty mmco list");
            assert!(dpb.len() <= 2, "DPB grew past max_num_ref_frames");
        }
        assert_eq!(dpb_frame_nums(&dpb), vec![4, 5]);
    }

    #[test]
    fn idr_marking_clears_the_dpb() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 0), 4);
        push(&mut dpb, entry(1, 2), 4);

        let outcome = mark(
            &mut dpb,
            entry(0, 0),
            &DecRefPicMarking::Idr {
                no_output_of_prior_pics_flag: false,
                long_term_reference_flag: false,
            },
            4,
        )
        .expect("idr");

        assert!(!outcome.current_is_long_term);
        assert_eq!(dpb.len(), 1);
        assert!(dpb.iter().next().unwrap().is_short_term);
        assert_eq!(dpb.max_long_term_frame_idx(), None);
    }

    /// §8.2.5.1: `long_term_reference_flag == 1` stores the IDR as a long-term
    /// reference with `LongTermFrameIdx == 0` and `MaxLongTermFrameIdx == 0`.
    #[test]
    fn idr_long_term_reference_flag_stores_a_long_term_picture() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(9, 18), 4);

        let outcome = mark(
            &mut dpb,
            entry(0, 0),
            &DecRefPicMarking::Idr {
                no_output_of_prior_pics_flag: false,
                long_term_reference_flag: true,
            },
            4,
        )
        .expect("idr");

        assert!(outcome.current_is_long_term);
        assert_eq!(dpb.len(), 1);
        let stored = dpb.iter().next().unwrap();
        assert!(stored.is_long_term);
        assert!(!stored.is_short_term);
        assert_eq!(stored.long_term_pic_num, 0);
        assert_eq!(dpb.max_long_term_frame_idx(), Some(0));
    }

    /// A long-term picture created by MMCO 3 is addressable by the
    /// §8.2.4.3.2 reference-list modification path, which is what makes the
    /// two halves of Phase E fit together.
    #[test]
    fn mmco3_long_term_picture_is_selectable_by_ref_list_modification() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 0), 4);
        push(&mut dpb, entry(1, 2), 4);
        mark(
            &mut dpb,
            entry(2, 4),
            &adaptive(&[MmcoOp::ShortTermToLongTerm {
                difference_of_pic_nums_minus1: 1,
                long_term_frame_idx: 4,
            }]),
            4,
        )
        .expect("mmco 3");

        let list = build_ref_list_l0(
            &dpb,
            3,
            ctx(3),
            &[RefPicListModification::LongTerm {
                long_term_pic_num: 4,
            }],
        )
        .expect("ref list");
        assert!(list[0].is_long_term);
        assert_eq!(list[0].frame_num, 0);
    }

    /// A command naming a picture the stream never stored is malformed; the
    /// DPB is emptied rather than left half-marked, so the next slice falls
    /// back instead of predicting from a wrongly-marked reference.
    #[test]
    fn mmco_referencing_an_absent_picture_fails_safe() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 0), 4);
        push(&mut dpb, entry(1, 2), 4);

        let err = mark(
            &mut dpb,
            entry(2, 4),
            &adaptive(&[MmcoOp::ShortTermUnused {
                difference_of_pic_nums_minus1: 9,
            }]),
            4,
        )
        .unwrap_err();
        assert_eq!(err, MmcoError::MissingShortTerm { pic_num: -8 });
        assert!(dpb.is_empty());
        assert!(build_ref_list_l0(&dpb, 1, ctx(3), &[]).is_none());

        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 0), 4);
        let err = mark(
            &mut dpb,
            entry(1, 2),
            &adaptive(&[MmcoOp::LongTermUnused {
                long_term_pic_num: 2,
            }]),
            4,
        )
        .unwrap_err();
        assert_eq!(err, MmcoError::MissingLongTerm { long_term_pic_num: 2 });
        assert!(dpb.is_empty());
    }

    /// A picture marked unused by an earlier command in the same list is gone
    /// for the commands that follow it.
    #[test]
    fn mmco_commands_apply_in_order() {
        let mut dpb = Dpb::new();
        push(&mut dpb, entry(0, 0), 4);
        push(&mut dpb, entry(1, 2), 4);

        // Both commands select picNumX = 1 (CurrPicNum 2 − 1); the second one
        // therefore finds nothing left to unmark.
        let err = mark(
            &mut dpb,
            entry(2, 4),
            &adaptive(&[
                MmcoOp::ShortTermUnused {
                    difference_of_pic_nums_minus1: 0,
                },
                MmcoOp::ShortTermUnused {
                    difference_of_pic_nums_minus1: 0,
                },
            ]),
            4,
        )
        .unwrap_err();
        assert_eq!(err, MmcoError::MissingShortTerm { pic_num: 1 });
    }
}
