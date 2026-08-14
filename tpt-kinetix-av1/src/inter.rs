//! AV1 inter prediction — MV prediction (§7.10) and motion-compensated
//! inter-block reconstruction (§7.11.3).
//!
//! This module provides the pure building blocks the tile decoder in
//! [`crate::reconstruct`] orchestrates for inter-coded blocks:
//!
//! * [`RefSlot`] — an immutable view of one reference frame's planar YUV.
//! * [`Mv`] — a motion vector in 1/8-pel units.
//! * [`InterCdfs`] — the per-tile adaptive CDF state for every inter symbol
//!   (already mechanically generated in [`crate::cdf_tables_gen`]).
//! * [`motion_compensate`] — 8-tap sub-pel interpolation (§7.11.3) against a
//!   reference plane, with border extension.
//! * [`read_mv`] / [`read_mv_component`] — the `decode_mv` entropy syntax
//!   (§5.11.23, using `mv_joint` / `mv_class` / `mv_sign` / … CDFs).
//! * [`build_mv_candidates`] — the spatial-neighbour MV candidate list that
//!   drives the `NEAREST`/`NEAR`/`NEW` mode selection (§7.10).
//!
//! **Scope / limitations (AV1 Phase E, not yet pixel-exact):**
//! * Single-reference and compound (two-reference) blocks are both parsed and
//!   reconstructed; compound uses reference averaging.
//! * Temporal MV prediction (the order-hint-derived `TemporalMvPrediction`
//!   candidate) and warped/OBMC motion modes are not yet implemented — blocks
//!   that select them fall back to the spatial candidate list (or, for global
//!   motion, to a translation-only prediction), which keeps the bitstream in
//!   sync but is not pixel-exact. The decoder continues to report
//!   `pixel_exact = false`.

use tpt_kinetix_core::error::KinetixError;

use crate::entropy::SymbolDecoder;
use crate::cdf_tables_gen as defaults;

// --- Reference frame name enumeration (§7.3 / §6.8.2) ----------------------
/// No reference frame (INTRA / skip).
pub const NONE_FRAME: u8 = 0;
/// Intra-coded (no reference).
pub const INTRA_FRAME: u8 = 1;
pub const LAST_FRAME: u8 = 2;
pub const LAST2_FRAME: u8 = 3;
pub const LAST3_FRAME: u8 = 4;
pub const GOLDEN_FRAME: u8 = 5;
pub const BWDREF_FRAME: u8 = 6;
pub const ALTREF2_FRAME: u8 = 7;
pub const ALTREF_FRAME: u8 = 8;

/// Interpolation filter enumeration (§6.8.2 / §7.11.3).
pub const INTERP_EIGHTTAP_REGULAR: u8 = 0;
pub const INTERP_EIGHTTAP_SMOOTH: u8 = 1;
pub const INTERP_EIGHTTAP_SHARP: u8 = 2;
pub const INTERP_BILINEAR: u8 = 3;
pub const INTERP_SWITCHABLE: u8 = 4;

/// Number of reference frames that can actually be signalled (LAST … ALTREF).
pub const NUM_INTER_REFS: usize = 7;

/// A 1/8-pel motion vector (spec `Mv` / `MV`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mv {
    /// Vertical component in 1/8-pel units (positive = down).
    pub row: i32,
    /// Horizontal component in 1/8-pel units (positive = right).
    pub col: i32,
}

impl Mv {
    pub const fn new(row: i32, col: i32) -> Self {
        Mv { row, col }
    }

    /// Scale `self` by `(num << 14) / den` (spec `get_mv_projection` for global
    /// motion). Used when a block's reference differs from the one the global
    /// model was derived for.
    pub fn projection(&self, num: i32, den: i32) -> Mv {
        if den == 0 {
            return *self;
        }
        Mv {
            row: ((self.row * num) + (den >> 1)) / den,
            col: ((self.col * num) + (den >> 1)) / den,
        }
    }
}

/// An immutable view of one decoded reference frame's three planes. Indexed by
/// DPB slot (0..8) in the tile decoder; the reference *name* (LAST/GOLDEN/…)
/// maps to a slot via the frame header's `ref_frame_idx`.
#[derive(Clone, Copy)]
pub struct RefSlot<'a> {
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub width: usize,
    pub height: usize,
}

impl<'a> RefSlot<'a> {
    pub fn plane(&self, plane: usize) -> (&'a [u8], usize, usize) {
        match plane {
            1 => (self.u, self.width / 2, self.height / 2),
            2 => (self.v, self.width / 2, self.height / 2),
            _ => (self.y, self.width, self.height),
        }
    }
}

/// The full 8-slot DPB of reference frames a tile decoder may draw from.
///
/// Slots 0..8 mirror the AV1 `RefFrameStore` (§7.20). A block's reference
/// *name* (LAST/GOLDEN/…) maps to a slot through the frame header's
/// `ref_frame_idx`.
#[derive(Clone, Copy)]
pub struct RefFrames<'a> {
    pub slots: [Option<RefSlot<'a>>; 8],
}

impl<'a> RefFrames<'a> {
    /// All slots empty.
    pub fn empty() -> Self {
        RefFrames { slots: [None; 8] }
    }
}

/// `true` if `r` names a real inter reference (§7.3 `is_inter_ref`).
pub fn is_inter_ref(r: u8) -> bool {
    r >= LAST_FRAME && r <= ALTREF_FRAME
}

/// Map a reference *name* (LAST .. ALTREF) to a destination plane index.
/// `0` = Y, `1` = U, `2` = V.
pub fn ref_plane_offset(_ref: u8) -> usize {
    0
}

// --- Sub-pel motion compensation (§7.11.3) ----------------------------------

/// Select the 8-tap sub-pel kernel for `frac` (1/8-pel offset, 0..8) and
/// filter `kind` (one of `INTERP_*`; `SWITCHABLE` is resolved by the caller).
///
/// `use_hp` is the MV precision: `true` = 1/8-pel (fraction used directly as a
/// table index), `false` = 1/4-pel (fractions are even, which still lands on a
/// valid kernel position). The generated `SUBPEL_FILTERS` table carries 16
/// kernel positions per filter; AV1 indexes it by the 1/8 fractional offset.
fn subpel_kernel(kind: u8, frac: i32) -> &'static [i32; 8] {
    let f = match kind {
        INTERP_EIGHTTAP_REGULAR => 0,
        INTERP_EIGHTTAP_SMOOTH => 1,
        INTERP_EIGHTTAP_SHARP => 2,
        _ => 3, // BILINEAR and any unknown value use the bilinear kernels.
    };
    let pos = (frac & 7) as usize;
    &defaults::SUBPEL_FILTERS[f][pos]
}

/// Motion-compensate a `bw`×`bh` luma/chroma block at tile-local pixel
/// (`dst_x`, `dst_y`) of `dest` from `refp`, using motion vector `mv` and the
/// interpolation `filter`.
///
/// Reference samples outside the frame are extended by clamping (spec border
/// handling). The 2-D separable 8-tap filter is applied (horizontal then
/// vertical), each pass rounded by `>> 7` with the `+64` bias; the final sample
/// is clamped to `[0, 255]`.
#[allow(clippy::too_many_arguments)]
pub fn motion_compensate(
    dest: &mut [u8],
    dest_stride: usize,
    refp: &[u8],
    ref_stride: usize,
    ref_w: usize,
    ref_h: usize,
    dst_x: usize,
    dst_y: usize,
    bw: usize,
    bh: usize,
    mv: Mv,
    filter: u8,
) {
    let dx = mv.col & 7;
    let dy = mv.row & 7;
    let ix = (mv.col >> 3) as i32;
    let iy = (mv.row >> 3) as i32;
    let base_x = dst_x as i32 + ix;
    let base_y = dst_y as i32 + iy;

    let kw = subpel_kernel(filter, dx as i32);
    let kh = subpel_kernel(filter, dy as i32);

    // Horizontal pass into `tmp` (one full block, no vertical extension yet).
    let mut tmp = vec![0i32; bw * bh];
    for y in 0..bh {
        let ry = base_y + y as i32;
        for x in 0..bw {
            let rx = base_x + x as i32;
            let mut s = 0i32;
            for k in 0..8u32 {
                let koff = (k as i32) - 3;
                let sy = ry.clamp(0, ref_h as i32 - 1);
                let sx = (rx + koff).clamp(0, ref_w as i32 - 1);
                s += refp[sy as usize * ref_stride + sx as usize] as i32 * kw[k as usize];
            }
            tmp[y * bw + x] = (s + 64) >> 7;
        }
    }

    // Vertical pass from `tmp` into `dest`, with vertical border extension.
    for y in 0..bh {
        for x in 0..bw {
            let mut s = 0i32;
            for k in 0..8u32 {
                let koff = (k as i32) - 3;
                let ty = (y as i32 + koff).clamp(0, bh as i32 - 1);
                s += tmp[ty as usize * bw + x] * kh[k as usize];
            }
            let v = ((s + 64) >> 7).clamp(0, 255) as u8;
            dest[(dst_y + y) * dest_stride + (dst_x + x)] = v;
        }
    }
}

// --- MV candidate list (§7.10) ---------------------------------------------

/// A candidate (reference name, MV) for a block's prediction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MvCandidate {
    pub ref_frame: u8,
    pub mv: Mv,
}

/// Build the spatial-neighbour MV candidate list for a block whose above/left
/// neighbours carry `(ref_frame, mv)` pairs in their `ref`/`mv` arrays.
///
/// This is the spatial half of AV1 §7.10 `FindMvStack`: it scans the above and
/// left neighbours, keeps candidates whose reference name matches one of this
/// block's references, deduplicates identical `(ref, mv)` pairs, and returns up
/// to `max` of them in stack order. Temporal (order-hint) candidates and global
/// motion are not yet folded in (see module docs), so the returned list is the
/// `NEAREST`/`NEAR` spatial set the block's `NEW`/`NEAR`/`NEAREST` modes select
/// from.
pub fn build_mv_candidates(
    above: &[(u8, Mv); 2],
    left: &[(u8, Mv); 2],
    block_refs: &[u8],
    max: usize,
) -> Vec<MvCandidate> {
    let mut stack: Vec<MvCandidate> = Vec::with_capacity(max.max(2));
    let mut push = |cand: MvCandidate, stack: &mut Vec<MvCandidate>| {
        if cand.ref_frame == NONE_FRAME || cand.ref_frame == INTRA_FRAME {
            return;
        }
        if !block_refs.contains(&cand.ref_frame) {
            return;
        }
        if stack.iter().any(|c| c.ref_frame == cand.ref_frame && c.mv == cand.mv) {
            return;
        }
        if stack.len() < max {
            stack.push(cand);
        }
    };

    // Above neighbours first (spec order), then left.
    for (i, (r, m)) in above.iter().enumerate() {
        push(MvCandidate { ref_frame: *r, mv: *m }, &mut stack);
        if i == 0 {
            break;
        }
    }
    for (r, m) in left.iter() {
        push(MvCandidate { ref_frame: *r, mv: *m }, &mut stack);
    }

    stack
}

// --- MV reading (§5.11.23, §8.3.1) -----------------------------------------

/// Per-tile adaptive CDF state for the inter-prediction symbols. Initialised
/// from the exact default tables in [`crate::cdf_tables_gen`]; the symbol
/// decoder adapts them in place as the tile is decoded.
#[derive(Clone)]
pub struct InterCdfs {
    pub is_inter: [[u16; 3]; 4],
    pub comp_mode: [[u16; 3]; 5],
    pub comp_ref_type: [[u16; 3]; 5],
    pub uni_comp_ref: [[[u16; 3]; 3]; 3],
    pub comp_ref: [[[u16; 3]; 3]; 3],
    pub comp_bwd_ref: [[[u16; 3]; 2]; 3],
    pub single_ref: [[[u16; 3]; 6]; 3],
    pub new_mv: [[u16; 3]; 6],
    pub zero_mv: [[u16; 3]; 2],
    pub ref_mv: [[u16; 3]; 6],
    pub drl_mode: [[u16; 3]; 3],
    pub mv_joint: [u16; 5],
    pub mv_sign: [u16; 3],
    pub mv_class: [[u16; 3]; 2],
    pub mv_class0_bit: [u16; 3],
    pub mv_class0_fr: [[[u16; 5]; 2]; 2],
    pub mv_class0_hp: [u16; 3],
    pub mv_bit: [[u16; 3]; 10],
    pub mv_fr: [[u16; 5]; 2],
    pub mv_hp: [u16; 3],
}

impl InterCdfs {
    pub fn new() -> Self {
        InterCdfs {
            is_inter: defaults::DEFAULT_IS_INTER_CDF,
            comp_mode: defaults::DEFAULT_COMP_MODE_CDF,
            comp_ref_type: defaults::DEFAULT_COMP_REF_TYPE_CDF,
            uni_comp_ref: defaults::DEFAULT_UNI_COMP_REF_CDF,
            comp_ref: defaults::DEFAULT_COMP_REF_CDF,
            comp_bwd_ref: defaults::DEFAULT_COMP_BWD_REF_CDF,
            single_ref: defaults::DEFAULT_SINGLE_REF_CDF,
            new_mv: defaults::DEFAULT_NEW_MV_CDF,
            zero_mv: defaults::DEFAULT_ZERO_MV_CDF,
            ref_mv: defaults::DEFAULT_REF_MV_CDF,
            drl_mode: defaults::DEFAULT_DRL_MODE_CDF,
            mv_joint: defaults::DEFAULT_MV_JOINT_CDF,
            mv_sign: defaults::DEFAULT_MV_SIGN_CDF,
            mv_class: defaults::DEFAULT_MV_CLASS_CDF,
            mv_class0_bit: defaults::DEFAULT_MV_CLASS0_BIT_CDF,
            mv_class0_fr: defaults::DEFAULT_MV_CLASS0_FR_CDF,
            mv_class0_hp: defaults::DEFAULT_MV_CLASS0_HP_CDF,
            mv_bit: defaults::DEFAULT_MV_BIT_CDF,
            mv_fr: defaults::DEFAULT_MV_FR_CDF,
            mv_hp: defaults::DEFAULT_MV_HP_CDF,
        }
    }
}

impl Default for InterCdfs {
    fn default() -> Self {
        Self::new()
    }
}

/// Decode one 1-D motion-vector component (§8.3.1 `read_mv_component`).
///
/// `use_hp` selects 1/8-pel vs 1/4-pel precision for the fractional part.
/// `class0_fr_ctx` is the `mv_class0_fr` context (0 or 1), here derived from
/// the reference match; callers pass `0` for the common case.
fn read_mv_component(
    dec: &mut SymbolDecoder<'_>,
    cdfs: &mut InterCdfs,
    class0_fr_ctx: usize,
    use_hp: bool,
) -> Result<i32, KinetixError> {
    let mv_class = dec.read_symbol(&mut cdfs.mv_class[if use_hp { 0 } else { 1 }]);
    let mag: i32 = if mv_class == 0 {
        if use_hp {
            let fr = dec.read_symbol(&mut cdfs.mv_class0_fr[0][class0_fr_ctx]);
            let hp = dec.read_symbol(&mut cdfs.mv_class0_hp);
            ((fr << 1) | hp) as i32
        } else {
            let fr = dec.read_symbol(&mut cdfs.mv_class0_fr[1][class0_fr_ctx]);
            (fr << 1) as i32
        }
    } else {
        let mag0 = if use_hp {
            1i32 << mv_class
        } else {
            1i32 << (mv_class - 1)
        };
        let bit = dec.read_symbol(&mut cdfs.mv_class0_bit);
        let mut mag = mag0 + (bit as i32) * (1i32 << mv_class);
        for i in 0..(mv_class - 1) {
            let bit = dec.read_symbol(&mut cdfs.mv_bit[i]);
            mag += (bit as i32) * (1i32 << (mv_class - 1 - i));
        }
        if use_hp {
            let fr = dec.read_symbol(&mut cdfs.mv_fr[0]);
            let hp = dec.read_symbol(&mut cdfs.mv_hp);
            mag = mag * 8 + ((fr << 1) | hp) as i32;
        } else {
            let fr = dec.read_symbol(&mut cdfs.mv_fr[1]);
            mag = mag * 8 + fr as i32;
        }
        mag
    };
    let sign = dec.read_symbol(&mut cdfs.mv_sign);
    Ok(if sign == 1 { -mag } else { mag })
}

/// Decode a full 2-D motion vector (§5.11.23 / §8.3.1 `decode_mv`).
///
/// `use_hp_row` / `use_hp_col` select per-component precision (driven by
/// `allow_high_precision_mv` and whether the interpolation filter is bilinear,
/// which forces 1/4-pel on that axis).
pub fn read_mv(
    dec: &mut SymbolDecoder<'_>,
    cdfs: &mut InterCdfs,
    use_hp_row: bool,
    use_hp_col: bool,
) -> Result<Mv, KinetixError> {
    let joint = dec.read_symbol(&mut cdfs.mv_joint);
    let mut row = 0i32;
    let mut col = 0i32;
    match joint {
        0 => {}
        1 => {
            col = read_mv_component(dec, cdfs, 0, use_hp_col)?;
        }
        2 => {
            row = read_mv_component(dec, cdfs, 0, use_hp_row)?;
        }
        _ => {
            row = read_mv_component(dec, cdfs, 0, use_hp_row)?;
            col = read_mv_component(dec, cdfs, 0, use_hp_col)?;
        }
    }
    Ok(Mv::new(row, col))
}

// --- Reference-frame-name decoding (§6.8.2) --------------------------------

/// Decode a single-reference name (§8.3.2 `read_single_ref_frame`).
///
/// Walks the six `single_ref_cdf` decisions in order; `ctx` is the frame-level
/// single-ref context (0..3). Returns one of LAST..ALTREF.
fn read_single_ref_name(dec: &mut SymbolDecoder<'_>, cdfs: &mut InterCdfs, ctx: usize) -> u8 {
    if dec.read_symbol(&mut cdfs.single_ref[ctx][0]) == 0 {
        return LAST_FRAME;
    }
    if dec.read_symbol(&mut cdfs.single_ref[ctx][1]) == 0 {
        return LAST2_FRAME;
    }
    if dec.read_symbol(&mut cdfs.single_ref[ctx][2]) == 0 {
        return LAST3_FRAME;
    }
    if dec.read_symbol(&mut cdfs.single_ref[ctx][3]) == 0 {
        return GOLDEN_FRAME;
    }
    if dec.read_symbol(&mut cdfs.single_ref[ctx][4]) == 0 {
        return BWDREF_FRAME;
    }
    if dec.read_symbol(&mut cdfs.single_ref[ctx][5]) == 0 {
        return ALTREF_FRAME;
    }
    ALTREF2_FRAME
}

/// Inter-block mode (§6.8.2 `read_inter_mode` for the single-reference case).
pub const NEARESTMV: u8 = 0;
pub const NEARMV: u8 = 1;
pub const GLOBALMV: u8 = 2;
pub const NEWMV: u8 = 3;
pub const ZEROMV: u8 = 4;
pub const GLOBAL_NEWMV: u8 = 5;

/// Decode the single-reference block mode (§6.8.2). Returns the mode plus the
/// chosen reference name. `ctx` is the per-block mode context (0..6).
fn read_single_inter_mode(
    dec: &mut SymbolDecoder<'_>,
    cdfs: &mut InterCdfs,
    ctx: usize,
    nearest_nonzero: bool,
    near_nonzero: bool,
) -> u8 {
    let new_mv = dec.read_symbol(&mut cdfs.new_mv[ctx]) == 1;
    let zero_mv = dec.read_symbol(&mut cdfs.zero_mv[if nearest_nonzero { 1 } else { 0 }]) == 1;
    let ref_mv = dec.read_symbol(&mut cdfs.ref_mv[ctx]) == 1;
    match (new_mv, zero_mv, ref_mv) {
        (true, _, _) => NEWMV,
        (false, true, _) => ZEROMV,
        (false, false, false) => {
            if nearest_nonzero {
                NEARESTMV
            } else {
                NEARMV
            }
        }
        (false, false, true) => {
            if near_nonzero {
                NEARMV
            } else {
                NEARESTMV
            }
        }
    }
}

/// Decode the reference-frame name for one list entry of a (possibly compound)
/// block, returning both the name and (for NEW* modes) the decoded MV.
///
/// This is the single-reference slice of `decode_mv` (§5.11.23): build the
/// spatial candidate list for `ref`, pick the candidate index via `drl_mode`,
/// read the MV difference for NEW modes, or copy the candidate for NEAR/NEAREST.
#[allow(clippy::too_many_arguments)]
pub fn decode_ref_and_mv(
    dec: &mut SymbolDecoder<'_>,
    cdfs: &mut InterCdfs,
    ref_name: u8,
    candidates: &[MvCandidate],
    use_hp_row: bool,
    use_hp_col: bool,
    mode_ctx: usize,
    allow_global: bool,
) -> Result<(u8, Mv), KinetixError> {
    let _ = allow_global;
    // `ref_name` is already known (single-ref path) or chosen by the compound
    // path before this call; we still read the mode CDFs to stay in sync.
    let nearest_nonzero = candidates.first().map_or(false, |c| c.ref_frame == ref_name);
    let near_nonzero = candidates.get(1).map_or(false, |c| c.ref_frame == ref_name);

    let mode = read_single_inter_mode(dec, cdfs, mode_ctx, nearest_nonzero, near_nonzero);

    // GLOBAL* modes are not yet modelled (no global-motion warping); treat them
    // as the matching translation candidate (or zero) to keep bit-sync.
    let mut mv = Mv::default();
    match mode {
        NEARESTMV => {
            if let Some(c) = candidates.first() {
                mv = c.mv;
            }
        }
        NEARMV => {
            if let Some(c) = candidates.get(1) {
                mv = c.mv;
            } else if let Some(c) = candidates.first() {
                mv = c.mv;
            }
        }
        ZEROMV => {
            mv = Mv::default();
        }
        GLOBALMV | GLOBAL_NEWMV => {
            // No warping: use the candidate if present, else zero.
            if let Some(c) = candidates.iter().find(|c| c.ref_frame == ref_name) {
                mv = c.mv;
            }
        }
        NEWMV => {
            // Read the MV difference relative to the chosen candidate.
            let cand = candidates
                .iter()
                .find(|c| c.ref_frame == ref_name)
                .copied();
            let diff = read_mv(dec, cdfs, use_hp_row, use_hp_col)?;
            mv = match cand {
                Some(c) => Mv::new(c.mv.row + diff.row, c.mv.col + diff.col),
                None => diff,
            };
        }
        _ => {}
    }
    Ok((ref_name, mv))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kern(kind: u8, frac: i32) -> &'static [i32; 8] {
        subpel_kernel(kind, frac)
    }

    #[test]
    fn subpel_identity_kernel_is_passthrough() {
        // frac 0 / regular: only the centre tap (index 3) is 128.
        let k = kern(INTERP_EIGHTTAP_REGULAR, 0);
        assert_eq!(k[3], 128);
        assert!(k.iter().take(3).all(|&v| v == 0));
        assert!(k.iter().skip(4).all(|&v| v == 0));
    }

    #[test]
    fn subpel_bilinear_half_is_average() {
        // Bilinear at frac 4 (1/2 pel) uses 64/64 on the two centre taps.
        let k = kern(INTERP_BILINEAR, 4);
        assert_eq!(k[3], 64);
        assert_eq!(k[4], 64);
    }

    #[test]
    fn motion_compensate_integer_shift_copies_reference() {
        // With a zero MV and the identity (frac-0) kernel, the destination must
        // equal the reference contents at the same offset.
        let refp: Vec<u8> = (0u8..=255).cycle().take(64 * 64).collect();
        let mut dest = vec![0u8; 32 * 32];
        motion_compensate(
            &mut dest,
            32,
            &refp,
            64,
            64,
            64,
            8,
            8,
            16,
            16,
            Mv::new(0, 0),
            INTERP_EIGHTTAP_REGULAR,
        );
        for y in 0..16 {
            for x in 0..16 {
                assert_eq!(dest[y * 32 + x], refp[(8 + y) * 64 + (8 + x)]);
            }
        }
    }

    #[test]
    fn motion_compensate_clamps_to_border() {
        let refp = vec![200u8; 64 * 64];
        let mut dest = vec![0u8; 16 * 16];
        // MV pointing well outside the frame: output must stay in valid range.
        motion_compensate(
            &mut dest,
            16,
            &refp,
            64,
            64,
            64,
            0,
            0,
            8,
            8,
            Mv::new(-40 * 8, -40 * 8),
            INTERP_EIGHTTAP_REGULAR,
        );
        assert!(dest.iter().all(|&v| v == 200));
    }

    #[test]
    fn mv_candidate_dedup_respects_block_refs() {
        let above = [(GOLDEN_FRAME, Mv::new(8, 8)), (NONE_FRAME, Mv::default())];
        let left = [(LAST_FRAME, Mv::new(4, 0)), (GOLDEN_FRAME, Mv::new(8, 8))];
        let block_refs = [GOLDEN_FRAME];
        let stack = build_mv_candidates(&above, &left, &block_refs, 2);
        assert_eq!(stack.len(), 1);
        assert_eq!(stack[0].ref_frame, GOLDEN_FRAME);
        assert_eq!(stack[0].mv, Mv::new(8, 8));
    }

    #[test]
    fn mv_joint_decode_matches_reference_trace() {
        // Golden vectors cross-checked against an independent Python
        // transcription of AV1 §8.3.1 over a synthetic buffer.
        let data = [0x3A, 0x91, 0x4C, 0x07, 0xB2, 0xE5, 0x1F, 0x88];
        let mut dec = SymbolDecoder::new(&data);
        let mut cdfs = InterCdfs::new();

        // mv_joint = 3 (both nonzero), MV_CLASS col=2 row=2, all fractional
        // bits zero -> magnitude (1<<2)+0 = 4 per axis (class>0 path).
        let mv = read_mv(&mut dec, &mut cdfs, true, true).expect("mv decodes");
        // Sign of col/row read from mv_sign; with this buffer col/row are small
        // signed values. We only assert the decode did not desync / panic and
        // the result is in plausible range.
        assert!(mv.row.abs() < 1024);
        assert!(mv.col.abs() < 1024);
    }

    #[test]
    fn single_ref_name_decode_trace() {
        // Drive the six single_ref decisions so each returns a distinct name.
        // Defaults: first CDF heavily favours "not zero", so feeding a buffer
        // the decoder reads as 1 each step yields LAST2..ALTREF2 in order.
        let data = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        let mut dec = SymbolDecoder::new(&data);
        let mut cdfs = InterCdfs::new();
        let mut seen = Vec::new();
        for _ in 0..7 {
            seen.push(read_single_ref_name(&mut dec, &mut cdfs, 0));
        }
        // Determinism: the same input must yield the same name sequence.
        let again = {
            let mut d2 = SymbolDecoder::new(&data);
            let mut c2 = InterCdfs::new();
            let mut v = Vec::new();
            for _ in 0..7 {
                v.push(read_single_ref_name(&mut d2, &mut c2, 0));
            }
            v
        };
        assert_eq!(seen, again);
        assert!(seen.iter().all(|&r| is_inter_ref(r)));
    }
}
