//! Inter motion-vector prediction (§8.2.1): the reference-MV scan
//! (`find_mv_refs` / `find_ref_mvs`), the MV component entropy reader and
//! the `fill_mv` driver, including the reference decoder's sub-8×8
//! "differing MV" scan quirks that bitstreams rely on.

use crate::booldec::BoolDecoder;
use crate::header::ProbsCtx;
pub use crate::predict::Mv;
use crate::tables::{MV_CLASS_TREE, MV_FP_TREE, MV_JOINT_TREE};

/// Per-8×8 motion-vector/reference bookkeeping, one entry per macroblock
/// (8px) cell of the frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct MvrefPair {
    /// Reference frame slot per list (or -1 = intra / unused).
    pub ref_: [i32; 2],
    pub mv: [Mv; 2],
}

pub const INVALID_MV_RAW: u32 = 0x8000_8000;

#[inline]
fn mv_raw(m: Mv) -> u32 {
    (m.x as u16 as u32) | (u32::from(m.y as u16) << 16)
}

#[inline]
fn clamp_mv(mv: Mv, min: Mv, max: Mv) -> Mv {
    Mv {
        x: mv.x.clamp(min.x, max.x),
        y: mv.y.clamp(min.y, max.y),
    }
}

/// Context the MV finder reads from; fields mirror the reference decoder's
/// tile/frame state.
pub struct MvFinder<'a> {
    /// Current frame's per-8px MV/ref grid (`row * map_w + col`).
    pub mvref: &'a [MvrefPair],
    /// Previous frame's grid when `use_last_frame_mvs` holds.
    pub mvref_prev: Option<&'a [MvrefPair]>,
    /// Above-row MV cache, two entries per 8px column.
    pub above_mv: &'a [[Mv; 2]],
    /// Left-column MV cache, two entries per 8px row-in-SB.
    pub left_mv: &'a [[Mv; 2]],
    pub map_w: usize,
    pub rows: usize,
    pub cols: usize,
    pub tile_col_start: usize,
    pub row: usize,
    pub col: usize,
    pub row7: usize,
    pub sign_bias: [bool; 4],
    pub use_last_frame_mvs: bool,
    pub min_mv: Mv,
    pub max_mv: Mv,
    /// Block size index of the block being decoded.
    pub bs: usize,
    /// The block's four sub-block MVs (for sub-8x8 seeding).
    pub block_mvs: [[Mv; 2]; 4],
}

/// Sub-block neighbour offsets (`mv_ref_blk_off`), per block size.
const MV_REF_BLK_OFF: [[[i8; 2]; 8]; 13] = [
    // BS_64x64
    [
        [3, -1],
        [-1, 3],
        [4, -1],
        [-1, 4],
        [-1, -1],
        [0, -1],
        [-1, 0],
        [6, -1],
    ],
    // BS_64x32
    [
        [0, -1],
        [-1, 0],
        [4, -1],
        [-1, 2],
        [-1, -1],
        [0, -3],
        [-3, 0],
        [2, -1],
    ],
    // BS_32x64
    [
        [-1, 0],
        [0, -1],
        [-1, 4],
        [2, -1],
        [-1, -1],
        [-3, 0],
        [0, -3],
        [-1, 2],
    ],
    // BS_32x32
    [
        [1, -1],
        [-1, 1],
        [2, -1],
        [-1, 2],
        [-1, -1],
        [0, -3],
        [-3, 0],
        [-3, -3],
    ],
    // BS_32x16
    [
        [0, -1],
        [-1, 0],
        [2, -1],
        [-1, -1],
        [-1, 1],
        [0, -3],
        [-3, 0],
        [-3, -3],
    ],
    // BS_16x32
    [
        [-1, 0],
        [0, -1],
        [-1, 2],
        [-1, -1],
        [1, -1],
        [-3, 0],
        [0, -3],
        [-3, -3],
    ],
    // BS_16x16
    [
        [0, -1],
        [-1, 0],
        [1, -1],
        [-1, 1],
        [-1, -1],
        [0, -3],
        [-3, 0],
        [-3, -3],
    ],
    // BS_16x8
    [
        [0, -1],
        [-1, 0],
        [1, -1],
        [-1, -1],
        [0, -2],
        [-2, 0],
        [-2, -1],
        [-1, -2],
    ],
    // BS_8x16
    [
        [-1, 0],
        [0, -1],
        [-1, 1],
        [-1, -1],
        [-2, 0],
        [0, -2],
        [-1, -2],
        [-2, -1],
    ],
    // BS_8x8
    [
        [0, -1],
        [-1, 0],
        [-1, -1],
        [0, -2],
        [-2, 0],
        [-1, -2],
        [-2, -1],
        [-2, -2],
    ],
    // BS_8x4
    [
        [0, -1],
        [-1, 0],
        [-1, -1],
        [0, -2],
        [-2, 0],
        [-1, -2],
        [-2, -1],
        [-2, -2],
    ],
    // BS_4x8
    [
        [0, -1],
        [-1, 0],
        [-1, -1],
        [0, -2],
        [-2, 0],
        [-1, -2],
        [-2, -1],
        [-2, -2],
    ],
    // BS_4x4
    [
        [0, -1],
        [-1, 0],
        [-1, -1],
        [0, -2],
        [-2, 0],
        [-1, -2],
        [-2, -1],
        [-2, -2],
    ],
];

impl<'a> MvFinder<'a> {
    /// `find_ref_mvs`: returns the predicted MV for `ref` frame slot.
    ///
    /// `z` selects the MV list (0/1, compound), `idx` is false for
    /// NEARESTMV (first hit wins) and true for NEARMV (first *differing*
    /// hit wins), `sb` is the sub-block id (0..3) or -1 for whole-block MVs.
    #[allow(clippy::needless_range_loop, unused_assignments)] // mirrors the reference scan
    pub fn find_ref_mvs(&self, ref_slot: i32, z: usize, idx: bool, sb: i32) -> Mv {
        let f = self;
        let mut mem: u32 = INVALID_MV_RAW;
        let mut mem_sub8x8: u32 = INVALID_MV_RAW;
        let _pmv = Mv::zero();
        let p = &MV_REF_BLK_OFF[f.bs];

        // RETURN_DIRECT_MV
        macro_rules! return_direct {
            ($m:expr) => {{
                let m = $m;
                if !idx {
                    return m;
                } else if mem == INVALID_MV_RAW {
                    mem = mv_raw(m);
                } else if mv_raw(m) != mem {
                    return m;
                }
            }};
        }

        // RETURN_MV
        macro_rules! return_mv {
            ($m:expr) => {{
                let m: Mv = $m;
                if sb > 0 {
                    if mem_sub8x8 == INVALID_MV_RAW {
                        let tmp = clamp_mv(m, f.min_mv, f.max_mv);
                        if mv_raw(tmp) != mem {
                            return tmp;
                        }
                        mem_sub8x8 = mv_raw(m);
                    } else if mem_sub8x8 != mv_raw(m) {
                        let tmp = clamp_mv(m, f.min_mv, f.max_mv);
                        if mv_raw(tmp) == mem {
                            // reference decoder quirk: clamped-equal case
                            // returns a zero MV here
                            return Mv::zero();
                        }
                        return tmp;
                    }
                } else {
                    let raw = mv_raw(m);
                    if !idx {
                        return clamp_mv(m, f.min_mv, f.max_mv);
                    } else if mem == INVALID_MV_RAW {
                        #[allow(unused_assignments)]
                        {
                            mem = raw;
                        }
                    } else if raw != mem {
                        return clamp_mv(m, f.min_mv, f.max_mv);
                    }
                }
            }};
        }

        macro_rules! return_scale {
            ($m:expr, $refmv:expr) => {{
                let m: Mv = $m;
                let scale = f.sign_bias[$refmv as usize] != f.sign_bias[ref_slot as usize];
                if scale {
                    return_mv!(Mv { x: -m.x, y: -m.y });
                } else {
                    return_mv!(m);
                }
            }};
        }

        let map = |r: usize, c: usize| (r) * f.map_w + (c);

        if sb >= 0 {
            // sub-8x8: seed from the block's other sub-block MVs
            let b = &f.block_mvs;
            if sb == 2 || sb == 1 {
                return_direct!(b[0][z]);
            } else if sb == 3 {
                return_direct!(b[2][z]);
                return_direct!(b[1][z]);
                return_direct!(b[0][z]);
            }

            if f.row > 0 {
                let mv = &f.mvref[map(f.row - 1, f.col)];
                if mv.ref_[0] == ref_slot {
                    return_mv!(f.above_mv[2 * f.col + (sb as usize & 1)][0]);
                } else if mv.ref_[1] == ref_slot {
                    return_mv!(f.above_mv[2 * f.col + (sb as usize & 1)][1]);
                }
            }
            if f.col > f.tile_col_start {
                let mv = &f.mvref[map(f.row, f.col - 1)];
                if mv.ref_[0] == ref_slot {
                    return_mv!(f.left_mv[2 * f.row7 + ((sb as usize) >> 1)][0]);
                } else if mv.ref_[1] == ref_slot {
                    return_mv!(f.left_mv[2 * f.row7 + ((sb as usize) >> 1)][1]);
                }
            }
        }

        // previously coded MVs in the neighbourhood, same reference frame
        let start = if sb >= 0 { 2 } else { 0 };
        for i in start..8 {
            let c = i32::from(p[i][0]) + f.col as i32;
            let r = i32::from(p[i][1]) + f.row as i32;
            if c >= f.tile_col_start as i32 && c < f.cols as i32 && r >= 0 && r < f.rows as i32 {
                let mv = &f.mvref[map(r as usize, c as usize)];
                if mv.ref_[0] == ref_slot {
                    return_mv!(mv.mv[0]);
                } else if mv.ref_[1] == ref_slot {
                    return_mv!(mv.mv[1]);
                }
            }
        }

        // MV at this position in the previous frame, same reference frame
        if f.use_last_frame_mvs {
            if let Some(prev) = f.mvref_prev {
                let mv = &prev[map(f.row, f.col)];
                if mv.ref_[0] == ref_slot {
                    return_mv!(mv.mv[0]);
                } else if mv.ref_[1] == ref_slot {
                    return_mv!(mv.mv[1]);
                }
            }
        }

        // neighbourhood, different reference frame (scaled by sign bias)
        for i in 0..8 {
            let c = i32::from(p[i][0]) + f.col as i32;
            let r = i32::from(p[i][1]) + f.row as i32;
            if c >= f.tile_col_start as i32 && c < f.cols as i32 && r >= 0 && r < f.rows as i32 {
                let mv = &f.mvref[map(r as usize, c as usize)];
                if mv.ref_[0] != ref_slot && mv.ref_[0] >= 0 {
                    return_scale!(mv.mv[0], mv.ref_[0]);
                }
                if mv.ref_[1] != ref_slot && mv.ref_[1] >= 0 && mv_raw(mv.mv[0]) != mv_raw(mv.mv[1])
                {
                    return_scale!(mv.mv[1], mv.ref_[1]);
                }
            }
        }

        // previous frame, different reference frame
        if f.use_last_frame_mvs {
            if let Some(prev) = f.mvref_prev {
                let mv = &prev[map(f.row, f.col)];
                if mv.ref_[0] != ref_slot && mv.ref_[0] >= 0 {
                    return_scale!(mv.mv[0], mv.ref_[0]);
                }
                if mv.ref_[1] != ref_slot && mv.ref_[1] >= 0 && mv_raw(mv.mv[0]) != mv_raw(mv.mv[1])
                {
                    return_scale!(mv.mv[1], mv.ref_[1]);
                }
            }
        }

        clamp_mv(Mv::zero(), f.min_mv, f.max_mv)
    }
}

/// Read one MV component delta for NEWMV (§8.2.1 `read_mv_component`).
/// `counts` collects the adaptation statistics (11-slot class bins etc.).
pub fn read_mv_component(
    bc: &mut BoolDecoder,
    probs: &ProbsCtx,
    idx: usize,
    hp: bool,
    counts: &mut crate::frame::MvCompCounts,
) -> i32 {
    let sign = bc.read_bool(probs.mv_comp[idx].sign);
    let c = read_tree(bc, &MV_CLASS_TREE, &probs.mv_comp[idx].classes);
    counts.sign[usize::from(sign)] += 1;
    counts.classes[c] += 1;

    let n: i32 = if c != 0 {
        let mut mag: i32 = 0;
        for m in 0..c {
            let bit = bc.read_bool(probs.mv_comp[idx].bits[m]);
            mag |= i32::from(bit) << m;
            counts.bits[m][usize::from(bit)] += 1;
        }
        let mut mag = mag << 3;
        let bit = read_tree(bc, &MV_FP_TREE, &probs.mv_comp[idx].fp);
        mag |= (bit as i32) << 1;
        counts.fp[bit] += 1;
        if hp {
            let hpbit = bc.read_bool(probs.mv_comp[idx].hp);
            counts.hp[usize::from(hpbit)] += 1;
            mag |= i32::from(hpbit);
        } else {
            mag |= 1;
            counts.hp[1] += 1;
        }
        mag + (8 << c)
    } else {
        let z = bc.read_bool(probs.mv_comp[idx].class0);
        counts.class0[usize::from(z)] += 1;
        let bit = read_tree(
            bc,
            &MV_FP_TREE,
            &probs.mv_comp[idx].class0_fp[usize::from(z)],
        );
        counts.class0_fp[usize::from(z)][bit] += 1;
        let mut v = (i32::from(z) << 3) | ((bit as i32) << 1);
        if hp {
            let hpbit = bc.read_bool(probs.mv_comp[idx].class0_hp);
            counts.class0_hp[usize::from(hpbit)] += 1;
            v |= i32::from(hpbit);
        } else {
            v |= 1;
            counts.class0_hp[1] += 1;
        }
        v
    };

    if sign {
        -(n + 1)
    } else {
        n + 1
    }
}

/// Walk an `[[i8; 2]; N]` decision tree; negative values are (negated) leaf
/// payloads. `probs[i]` is the probability of node `i`.
#[inline]
pub fn read_tree(bc: &mut BoolDecoder, tree: &[[i8; 2]], probs: &[u8]) -> usize {
    let mut i = 0usize;
    loop {
        let bit = usize::from(bc.read_bool(probs[i]));
        let next = tree[i][bit];
        // Leaf payloads are negated node values; note -0 == 0, so a leaf with
        // payload 0 (e.g. DC_PRED / ZEROMV) reads as 0 and must terminate the
        // walk exactly like the reference's `while (i > 0)` loop.
        if next <= 0 {
            return next.unsigned_abs() as usize;
        }
        i = next as usize;
        if i >= tree.len() {
            // corrupt walk: bail to leaf 0 rather than index out of bounds
            return 0;
        }
    }
}

/// Joint tree decode returns 0..3 (`ZERO, H, V, HV`).
pub fn read_mv_joint(bc: &mut BoolDecoder, probs: &ProbsCtx) -> usize {
    read_tree(bc, &MV_JOINT_TREE, &probs.mv_joint)
}
