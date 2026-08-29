//! H.264 motion-vector prediction (§8.4.1) and per-macroblock motion state.
//!
//! Implements the P-slice motion-vector predictor: the neighbours A (left),
//! B (above), C (above-right) and D (above-left, used when C is unavailable)
//! are resolved at 4×4-block granularity and combined with the median /
//! `match_count` rules of §8.4.1.3.1, including the directional shortcuts for
//! 16×8 / 8×16 partitions (§8.4.1.3.1) and the P-skip MVP of §8.4.1.1.
//!
//! The semantics follow `libavcodec/h264_mvpred.h` (ffmpeg) and the reference
//! `rust_h264` decoder. Only progressive (non-MBAFF) pictures are handled;
//! MBAFF frame/field pair remapping is out of scope. Unavailable neighbours
//! are `None` (ffmpeg `PART_NOT_AVAILABLE`); decoded intra macroblocks are
//! stored with ref index -1 (ffmpeg `LIST_NOT_USED`) and a zero motion vector,
//! so they never match a target ref index and contribute 0 to the median.

// The prediction API is consumed by the decoder's P-slice decode path, which
// is not yet enabled; the module is exercised by its unit tests until then.
#![cfg_attr(not(test), allow(dead_code))]

use crate::macroblock::{InterMotion, Macroblock, MbType};

/// Ref index for a neighbour that is not present: outside the picture, in a
/// different slice, or a 4×4 block not yet decoded (ffmpeg `PART_NOT_AVAILABLE`).
pub const PART_NOT_AVAILABLE: i32 = -2;
/// Ref index for a decoded intra macroblock (ffmpeg `LIST_NOT_USED`).
pub const LIST_NOT_USED: i32 = -1;

/// Number of sub-partitions per P_8x8 `sub_mb_type` (Table 7-13):
/// 8×8=1, 8×4=2, 4×8=2, 4×4=4.
const P_SUB_MB_PARTS: [usize; 4] = [1, 2, 2, 4];

/// Motion state of a single 4×4 block (holds both L0 and L1 for B-slices).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MvCell {
    /// L0 motion vector in quarter-luma-sample units.
    pub mv: [i32; 2],
    /// L0 reference picture index (`LIST_NOT_USED` for intra or L1-only).
    pub ref_idx: i32,
    /// L1 motion vector (B-slice bi-prediction / L1 partitions).
    pub mv_l1: [i32; 2],
    /// L1 reference picture index (`LIST_NOT_USED` for intra, P-slice, or L0-only).
    pub ref_idx_l1: i32,
}

impl MvCell {
    /// State of a decoded intra macroblock block.
    pub const INTRA: MvCell = MvCell {
        mv: [0, 0],
        ref_idx: LIST_NOT_USED,
        mv_l1: [0, 0],
        ref_idx_l1: LIST_NOT_USED,
    };
    /// State of a 4×4 block that has not been decoded yet.
    pub const UNAVAILABLE: MvCell = MvCell {
        mv: [0, 0],
        ref_idx: PART_NOT_AVAILABLE,
        mv_l1: [0, 0],
        ref_idx_l1: PART_NOT_AVAILABLE,
    };
}

/// A resolved spatial neighbour candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MvNeighbor {
    pub mv: [i32; 2],
    pub ref_idx: i32,
}

impl From<MvCell> for MvNeighbor {
    fn from(cell: MvCell) -> Self {
        Self {
            mv: cell.mv,
            ref_idx: cell.ref_idx,
        }
    }
}

/// Per-macroblock motion state for a whole picture.
///
/// Grids are committed as macroblocks finish decoding. A macroblock is
/// available as a neighbour only when it has been decoded in the *current*
/// slice; earlier-slice macroblocks are treated as unavailable (§8.4.1.2).
#[derive(Debug, Clone)]
pub struct MvStore {
    mbs: Vec<Option<[MvCell; 16]>>,
    slice_ids: Vec<u32>,
    /// `mb_field_decoding_flag` of each committed macroblock (MBAFF). Used by
    /// [`Self::fix_mv_mbaff`] to translate neighbours into the current MB's
    /// units.
    mb_field: Vec<bool>,
    /// Field convention of the macroblock currently being predicted
    /// (interior-mutability scratch so neighbour fetches can convert without
    /// threading a parameter through every helper; the parse is
    /// single-threaded).
    cur_field: std::cell::Cell<bool>,
}

impl MvStore {
    /// Create a store for `total_mbs` raster macroblocks.
    pub fn new(total_mbs: usize) -> Self {
        Self {
            mbs: vec![None; total_mbs],
            slice_ids: vec![0; total_mbs],
            mb_field: vec![false; total_mbs],
            cur_field: std::cell::Cell::new(false),
        }
    }

    /// Commit the 16-block grid of `mb_idx`, decoded in `slice_id`.
    pub(crate) fn commit(&mut self, mb_idx: usize, cells: [MvCell; 16], slice_id: u32) {
        self.mbs[mb_idx] = Some(cells);
        self.slice_ids[mb_idx] = slice_id;
    }

    /// Record `mb_field_decoding_flag` of `mb_idx` (MBAFF frames only).
    pub(crate) fn set_mb_field(&mut self, mb_idx: usize, field: bool) {
        if let Some(f) = self.mb_field.get_mut(mb_idx) {
            *f = field;
        }
    }

    /// The stored flag (false for frame-coded / PAFF macroblocks).
    fn mb_field_of(&self, mb_idx: usize) -> bool {
        self.mb_field.get(mb_idx).copied().unwrap_or(false)
    }

    /// Declare the field convention of the macroblock about to be predicted;
    /// subsequent neighbour fetches convert candidates into its units.
    pub(crate) fn set_cur_field(&self, field: bool) {
        self.cur_field.set(field);
    }

    /// The 16-block grid of `mb_idx`, if it has been decoded.
    pub fn cells_of(&self, mb_idx: usize) -> Option<[MvCell; 16]> {
        self.mbs[mb_idx]
    }

    /// Flatten every macroblock's grid into a per-picture motion array
    /// (undecoded / intra macroblocks become [`MvCell::INTRA`]). Used to
    /// persist a reference picture's motion for direct-mode col_zero_flag
    /// lookups.
    pub(crate) fn to_grid_vec(&self) -> Vec<[MvCell; 16]> {
        let mut out = Vec::with_capacity(self.mbs.len());
        for i in 0..self.mbs.len() {
            out.push(self.mbs[i].unwrap_or([crate::mv::MvCell::INTRA; 16]));
        }
        out
    }

    fn is_available(&self, mb_idx: usize, slice_id: u32) -> bool {
        self.mbs.get(mb_idx).copied().flatten().is_some() && self.slice_ids[mb_idx] == slice_id
    }

    fn cell(&self, mb_idx: usize, blk: usize) -> MvNeighbor {
        let n: MvNeighbor = self.mbs[mb_idx].expect("available MB")[blk].into();
        Self::fix_mv_mbaff(n, self.mb_field_of(mb_idx), self.cur_field.get())
    }

    /// Apply FFmpeg's `FIX_MV_MBAFF` conversion (h264_mvpred_ref.h @n5.1 lines
    /// 237-254): translate a neighbour's `(ref_idx, mv)` from its own coding
    /// convention into the current macroblock's.
    ///
    /// * Current MB **field**-coded, neighbour **frame**-coded: reference
    ///   indices expand to per-field entries (`refn <<= 1`) and the vertical
    ///   MV component converts from frame-line to field-line quarters
    ///   (`y /= 2`, C truncation).
    /// * Current MB frame-coded, neighbour interlaced: the inverse
    ///   (`refn >>= 1`, `y *= 2`).
    ///
    /// Same-convention neighbours are returned unchanged.
    fn fix_mv_mbaff(mut n: MvNeighbor, nb_field: bool, cur_field: bool) -> MvNeighbor {
        if nb_field == cur_field {
            return n;
        }
        if cur_field && !nb_field {
            n.ref_idx <<= 1;
            n.mv[1] /= 2;
        } else {
            n.ref_idx >>= 1;
            n.mv[1] *= 2;
        }
        n
    }

    /// Extract the L1 fields of a committed cell as a neighbour.
    fn cell_l1(&self, mb_idx: usize, blk: usize) -> MvNeighbor {
        let c = self.mbs[mb_idx].expect("available MB")[blk];
        let n = MvNeighbor {
            mv: c.mv_l1,
            ref_idx: c.ref_idx_l1,
        };
        Self::fix_mv_mbaff(n, self.mb_field_of(mb_idx), self.cur_field.get())
    }
}

// ---------------------------------------------------------------------------
// Neighbour resolution (progressive only). Offsets are in luma pixels relative
// to the macroblock top-left; `blk` indexes the 16 4×4 blocks raster order.
// ---------------------------------------------------------------------------

/// A: the 4×4 block to the left of the partition top-left.
fn neighbor_left(
    store: &MvStore,
    cur: &[MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    py_off: usize,
    px_off: usize,
    slice_id: u32,
) -> Option<MvNeighbor> {
    if px_off > 0 {
        let blk = (py_off / 4) * 4 + (px_off - 4) / 4;
        return Some(cur[blk].into());
    }
    if mb_idx % mb_width == 0 {
        return None;
    }
    let left_mb = mb_idx - 1;
    if !store.is_available(left_mb, slice_id) {
        return None;
    }
    let blk = (py_off / 4) * 4 + 3;
    Some(store.cell(left_mb, blk))
}

/// B: the 4×4 block above the partition top-left.
fn neighbor_above(
    store: &MvStore,
    cur: &[MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    py_off: usize,
    px_off: usize,
    slice_id: u32,
) -> Option<MvNeighbor> {
    if py_off > 0 {
        let blk = ((py_off - 4) / 4) * 4 + px_off / 4;
        return Some(cur[blk].into());
    }
    if mb_idx / mb_width == 0 {
        return None;
    }
    let above_mb = mb_idx - mb_width;
    if !store.is_available(above_mb, slice_id) {
        return None;
    }
    let blk = 3 * 4 + px_off / 4;
    Some(store.cell(above_mb, blk))
}

/// C: the 4×4 block above-right of the partition top-left (spec 8.4.1.3.2).
///
/// Within the current macroblock, C is unavailable when it falls in an 8×8
/// sub-macroblock that has not been decoded yet (spec 6.4.11.7).
#[allow(clippy::too_many_arguments)]
fn neighbor_above_right(
    store: &MvStore,
    cur: &[MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    py_off: usize,
    px_off: usize,
    part_w: usize,
    slice_id: u32,
) -> Option<MvNeighbor> {
    let right_col = px_off + part_w;

    if py_off > 0 {
        if right_col < 16 {
            let cur_8x8_col = px_off / 8;
            let tgt_8x8_col = right_col / 8;
            let cur_8x8_row = py_off / 8;
            let tgt_8x8_row = (py_off - 4) / 8;
            let cur_8x8 = cur_8x8_row * 2 + cur_8x8_col;
            let tgt_8x8 = tgt_8x8_row * 2 + tgt_8x8_col;
            if tgt_8x8 > cur_8x8 {
                return None;
            }
            let blk = ((py_off - 4) / 4) * 4 + right_col / 4;
            return Some(cur[blk].into());
        }
        return None;
    }

    if mb_idx / mb_width == 0 {
        return None;
    }
    if right_col < 16 {
        let above_mb = mb_idx - mb_width;
        if !store.is_available(above_mb, slice_id) {
            return None;
        }
        let blk = 3 * 4 + right_col / 4;
        Some(store.cell(above_mb, blk))
    } else if mb_idx % mb_width + 1 < mb_width {
        let above_right_mb = mb_idx - mb_width + 1;
        if !store.is_available(above_right_mb, slice_id) {
            return None;
        }
        Some(store.cell(above_right_mb, 3 * 4))
    } else {
        None
    }
}

/// D: the 4×4 block above-left of the partition top-left (fallback for C).
fn neighbor_above_left(
    store: &MvStore,
    cur: &[MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    py_off: usize,
    px_off: usize,
    slice_id: u32,
) -> Option<MvNeighbor> {
    if py_off > 0 && px_off > 0 {
        let blk = ((py_off - 4) / 4) * 4 + (px_off - 4) / 4;
        return Some(cur[blk].into());
    }
    if py_off == 0 && px_off == 0 {
        if mb_idx / mb_width == 0 || mb_idx % mb_width == 0 {
            return None;
        }
        let al_mb = mb_idx - mb_width - 1;
        if !store.is_available(al_mb, slice_id) {
            return None;
        }
        Some(store.cell(al_mb, 3 * 4 + 3))
    } else if py_off == 0 && px_off > 0 {
        if mb_idx / mb_width == 0 {
            return None;
        }
        let above_mb = mb_idx - mb_width;
        if !store.is_available(above_mb, slice_id) {
            return None;
        }
        let blk = 3 * 4 + (px_off - 4) / 4;
        Some(store.cell(above_mb, blk))
    } else {
        // py_off > 0 && px_off == 0
        if mb_idx % mb_width == 0 {
            return None;
        }
        let left_mb = mb_idx - 1;
        if !store.is_available(left_mb, slice_id) {
            return None;
        }
        let blk = ((py_off - 4) / 4) * 4 + 3;
        Some(store.cell(left_mb, blk))
    }
}

// ---------------------------------------------------------------------------
// L1 neighbour helpers (identical to L0 versions but extract mv_l1/ref_idx_l1).
// ---------------------------------------------------------------------------

fn neighbor_left_l1(
    store: &MvStore,
    cur: &[MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    py_off: usize,
    px_off: usize,
    slice_id: u32,
) -> Option<MvNeighbor> {
    if px_off > 0 {
        let blk = (py_off / 4) * 4 + (px_off - 4) / 4;
        let c = cur[blk];
        return Some(MvNeighbor {
            mv: c.mv_l1,
            ref_idx: c.ref_idx_l1,
        });
    }
    if mb_idx % mb_width == 0 {
        return None;
    }
    let left_mb = mb_idx - 1;
    if !store.is_available(left_mb, slice_id) {
        return None;
    }
    let blk = (py_off / 4) * 4 + 3;
    Some(store.cell_l1(left_mb, blk))
}

fn neighbor_above_l1(
    store: &MvStore,
    cur: &[MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    py_off: usize,
    px_off: usize,
    slice_id: u32,
) -> Option<MvNeighbor> {
    if py_off > 0 {
        let blk = ((py_off - 4) / 4) * 4 + px_off / 4;
        let c = cur[blk];
        return Some(MvNeighbor {
            mv: c.mv_l1,
            ref_idx: c.ref_idx_l1,
        });
    }
    if mb_idx / mb_width == 0 {
        return None;
    }
    let above_mb = mb_idx - mb_width;
    if !store.is_available(above_mb, slice_id) {
        return None;
    }
    let blk = 3 * 4 + px_off / 4;
    Some(store.cell_l1(above_mb, blk))
}

#[allow(clippy::too_many_arguments)]
fn neighbor_above_right_l1(
    store: &MvStore,
    cur: &[MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    py_off: usize,
    px_off: usize,
    part_w: usize,
    slice_id: u32,
) -> Option<MvNeighbor> {
    let right_col = px_off + part_w;
    if py_off > 0 {
        if right_col < 16 {
            let cur_8x8_col = px_off / 8;
            let tgt_8x8_col = right_col / 8;
            let cur_8x8_row = py_off / 8;
            let tgt_8x8_row = (py_off - 4) / 8;
            let cur_8x8 = cur_8x8_row * 2 + cur_8x8_col;
            let tgt_8x8 = tgt_8x8_row * 2 + tgt_8x8_col;
            if tgt_8x8 > cur_8x8 {
                return None;
            }
            let blk = ((py_off - 4) / 4) * 4 + right_col / 4;
            let c = cur[blk];
            return Some(MvNeighbor {
                mv: c.mv_l1,
                ref_idx: c.ref_idx_l1,
            });
        }
        return None;
    }
    if mb_idx / mb_width == 0 {
        return None;
    }
    if right_col < 16 {
        let above_mb = mb_idx - mb_width;
        if !store.is_available(above_mb, slice_id) {
            return None;
        }
        let blk = 3 * 4 + right_col / 4;
        Some(store.cell_l1(above_mb, blk))
    } else if mb_idx % mb_width + 1 < mb_width {
        let above_right_mb = mb_idx - mb_width + 1;
        if !store.is_available(above_right_mb, slice_id) {
            return None;
        }
        Some(store.cell_l1(above_right_mb, 3 * 4))
    } else {
        None
    }
}

fn neighbor_above_left_l1(
    store: &MvStore,
    cur: &[MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    py_off: usize,
    px_off: usize,
    slice_id: u32,
) -> Option<MvNeighbor> {
    if py_off > 0 && px_off > 0 {
        let blk = ((py_off - 4) / 4) * 4 + (px_off - 4) / 4;
        let c = cur[blk];
        return Some(MvNeighbor {
            mv: c.mv_l1,
            ref_idx: c.ref_idx_l1,
        });
    }
    if py_off == 0 && px_off == 0 {
        if mb_idx / mb_width == 0 || mb_idx % mb_width == 0 {
            return None;
        }
        let al_mb = mb_idx - mb_width - 1;
        if !store.is_available(al_mb, slice_id) {
            return None;
        }
        Some(store.cell_l1(al_mb, 3 * 4 + 3))
    } else if py_off == 0 && px_off > 0 {
        if mb_idx / mb_width == 0 {
            return None;
        }
        let above_mb = mb_idx - mb_width;
        if !store.is_available(above_mb, slice_id) {
            return None;
        }
        let blk = 3 * 4 + (px_off - 4) / 4;
        Some(store.cell_l1(above_mb, blk))
    } else {
        // py_off > 0 && px_off == 0
        if mb_idx % mb_width == 0 {
            return None;
        }
        let left_mb = mb_idx - 1;
        if !store.is_available(left_mb, slice_id) {
            return None;
        }
        let blk = ((py_off - 4) / 4) * 4 + 3;
        Some(store.cell_l1(left_mb, blk))
    }
}

// ---------------------------------------------------------------------------
// Predictors
// ---------------------------------------------------------------------------

/// Median of three motion vectors (component-wise).
fn median3(x: [i32; 2], y: [i32; 2], z: [i32; 2]) -> [i32; 2] {
    let mut xs = [x[0], y[0], z[0]];
    let mut ys = [x[1], y[1], z[1]];
    xs.sort();
    ys.sort();
    [xs[1], ys[1]]
}

/// The `match_count` / median rule of §8.4.1.3.1 for a partition.
fn median_pred(
    a: Option<MvNeighbor>,
    b: Option<MvNeighbor>,
    c: Option<MvNeighbor>,
    ref_idx: i32,
) -> [i32; 2] {
    let ref_a = a.map_or(-1, |n| n.ref_idx);
    let ref_b = b.map_or(-1, |n| n.ref_idx);
    let ref_c = c.map_or(-1, |n| n.ref_idx);
    let match_count =
        (ref_a == ref_idx) as u8 + (ref_b == ref_idx) as u8 + (ref_c == ref_idx) as u8;

    if match_count == 1 {
        if ref_a == ref_idx {
            return a.unwrap().mv;
        }
        if ref_b == ref_idx {
            return b.unwrap().mv;
        }
        return c.unwrap().mv;
    }

    // §8.4.1.3.1: with no matching neighbour, use A when B and C are both
    // unavailable (A's ref index is irrelevant here).
    if let (None, None, Some(n)) = (b, c, a) {
        return n.mv;
    }

    let mv_a = a.map_or([0, 0], |n| n.mv);
    let mv_b = b.map_or([0, 0], |n| n.mv);
    let mv_c = c.map_or([0, 0], |n| n.mv);
    median3(mv_a, mv_b, mv_c)
}

/// Predict the MV of a 16×16 / 16×8 / 8×16 macroblock partition (§8.4.1.3.1).
///
/// `part_idx`, `part_w`, `part_h` describe the partition; the directional
/// shortcuts for the two halves of 16×8 and 8×16 partitions are applied.
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict_mv(
    store: &MvStore,
    cur: &[MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    part_idx: usize,
    part_w: usize,
    part_h: usize,
    ref_idx: i32,
    slice_id: u32,
) -> [i32; 2] {
    let py_off = if part_h == 8 && part_w == 16 {
        part_idx * 8
    } else {
        0
    };
    let px_off = if part_w == 8 && part_h == 16 {
        part_idx * 8
    } else {
        0
    };

    let a = neighbor_left(store, cur, mb_idx, mb_width, py_off, px_off, slice_id);
    let b = neighbor_above(store, cur, mb_idx, mb_width, py_off, px_off, slice_id);
    let c = neighbor_above_right(
        store, cur, mb_idx, mb_width, py_off, px_off, part_w, slice_id,
    )
    .or_else(|| neighbor_above_left(store, cur, mb_idx, mb_width, py_off, px_off, slice_id));

    let pred = if part_w == 16 && part_h == 8 {
        if part_idx == 0 {
            if let Some(n) = &b {
                if n.ref_idx == ref_idx {
                    n.mv
                } else {
                    median_pred(a, b, c, ref_idx)
                }
            } else {
                median_pred(a, b, c, ref_idx)
            }
        } else if let Some(n) = &a {
            if n.ref_idx == ref_idx {
                n.mv
            } else {
                median_pred(a, b, c, ref_idx)
            }
        } else {
            median_pred(a, b, c, ref_idx)
        }
    } else if part_w == 8 && part_h == 16 {
        if part_idx == 0 {
            if let Some(n) = &a {
                if n.ref_idx == ref_idx {
                    n.mv
                } else {
                    median_pred(a, b, c, ref_idx)
                }
            } else {
                median_pred(a, b, c, ref_idx)
            }
        } else if let Some(n) = &c {
            if n.ref_idx == ref_idx {
                n.mv
            } else {
                median_pred(a, b, c, ref_idx)
            }
        } else {
            median_pred(a, b, c, ref_idx)
        }
    } else {
        median_pred(a, b, c, ref_idx)
    };

    // Session #30 diagnostic (todo-h264.md): MVP input/output trace for the
    // c_p8x8 investigation. Gated on KINETIX_BINTRACE like the other parsers.
    if std::env::var("KINETIX_BINTRACE").is_ok() {
        let fmt = |n: &Option<MvNeighbor>| match n {
            None => "None".to_string(),
            Some(n) => format!("Some(mv=({},{}) ri={})", n.mv[0], n.mv[1], n.ref_idx),
        };
        eprintln!(
            "MVP mb{} part({part_w}x{part_h},{part_idx}) ref{ref_idx}: A={} B={} C={} -> ({},{})",
            mb_idx,
            fmt(&a),
            fmt(&b),
            fmt(&c),
            pred[0],
            pred[1]
        );
    }

    pred
}

/// Predict the MV of a P_8x8 sub-macroblock partition (§8.4.1.3.2).
///
/// `px` / `py` are the partition top-left in luma pixels relative to the
/// macroblock; `spw` is the partition width in pixels.
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict_mv_sub(
    store: &MvStore,
    cur: &[MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    px: usize,
    py: usize,
    spw: usize,
    ref_idx: i32,
    slice_id: u32,
) -> [i32; 2] {
    let a = neighbor_left(store, cur, mb_idx, mb_width, py, px, slice_id);
    let b = neighbor_above(store, cur, mb_idx, mb_width, py, px, slice_id);
    let c = neighbor_above_right(store, cur, mb_idx, mb_width, py, px, spw, slice_id)
        .or_else(|| neighbor_above_left(store, cur, mb_idx, mb_width, py, px, slice_id));

    let pred = median_pred(a, b, c, ref_idx);

    // Session #31 diagnostic (todo-h264.md): P_8x8 sub-partition MVP trace,
    // same format as predict_mv's, gated on KINETIX_BINTRACE.
    if std::env::var("KINETIX_BINTRACE").is_ok() {
        let fmt = |n: &Option<MvNeighbor>| match n {
            None => "None".to_string(),
            Some(n) => format!("Some(mv=({},{}) ri={})", n.mv[0], n.mv[1], n.ref_idx),
        };
        eprintln!(
            "MVP-SUB mb{} sub({px},{py} {spw}w) ref{ref_idx}: A={} B={} C={} -> ({},{})",
            mb_idx,
            fmt(&a),
            fmt(&b),
            fmt(&c),
            pred[0],
            pred[1]
        );
    }

    pred
}

/// P-skip MVP (§8.4.1.1): (0,0) when A or B is unavailable or a zero-MV
/// reference-0 block, otherwise the median predictor for a 16×16 partition
/// with ref index 0.
pub(crate) fn predict_skip_mv(
    store: &MvStore,
    cur: &[MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    slice_id: u32,
) -> [i32; 2] {
    let a = neighbor_left(store, cur, mb_idx, mb_width, 0, 0, slice_id);
    let b = neighbor_above(store, cur, mb_idx, mb_width, 0, 0, slice_id);
    let a_zero = a.is_none_or(|n| n.ref_idx == 0 && n.mv == [0, 0]);
    let b_zero = b.is_none_or(|n| n.ref_idx == 0 && n.mv == [0, 0]);
    if a_zero || b_zero {
        return [0, 0];
    }
    predict_mv(store, cur, mb_idx, mb_width, 0, 16, 16, 0, slice_id)
}

/// Predict the L1 MV of a 16×16 / 16×8 / 8×16 macroblock partition.
/// Identical to [`predict_mv`] but uses L1 neighbour fields.
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict_mv_l1(
    store: &MvStore,
    cur: &[MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    part_idx: usize,
    part_w: usize,
    part_h: usize,
    ref_idx: i32,
    slice_id: u32,
) -> [i32; 2] {
    let py_off = if part_h == 8 && part_w == 16 {
        part_idx * 8
    } else {
        0
    };
    let px_off = if part_w == 8 && part_h == 16 {
        part_idx * 8
    } else {
        0
    };

    let a = neighbor_left_l1(store, cur, mb_idx, mb_width, py_off, px_off, slice_id);
    let b = neighbor_above_l1(store, cur, mb_idx, mb_width, py_off, px_off, slice_id);
    let c = neighbor_above_right_l1(
        store, cur, mb_idx, mb_width, py_off, px_off, part_w, slice_id,
    )
    .or_else(|| neighbor_above_left_l1(store, cur, mb_idx, mb_width, py_off, px_off, slice_id));

    if part_w == 16 && part_h == 8 {
        if part_idx == 0 {
            if let Some(n) = b {
                if n.ref_idx == ref_idx {
                    return n.mv;
                }
            }
        } else if let Some(n) = a {
            if n.ref_idx == ref_idx {
                return n.mv;
            }
        }
    }
    if part_w == 8 && part_h == 16 {
        if part_idx == 0 {
            if let Some(n) = a {
                if n.ref_idx == ref_idx {
                    return n.mv;
                }
            }
        } else if let Some(n) = c {
            if n.ref_idx == ref_idx {
                return n.mv;
            }
        }
    }

    median_pred(a, b, c, ref_idx)
}

/// Predict the L1 MV of a B_8x8 sub-macroblock partition.
/// Identical to [`predict_mv_sub`] but uses L1 neighbour fields.
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict_mv_sub_l1(
    store: &MvStore,
    cur: &[MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    px: usize,
    py: usize,
    spw: usize,
    ref_idx: i32,
    slice_id: u32,
) -> [i32; 2] {
    let a = neighbor_left_l1(store, cur, mb_idx, mb_width, py, px, slice_id);
    let b = neighbor_above_l1(store, cur, mb_idx, mb_width, py, px, slice_id);
    let c = neighbor_above_right_l1(store, cur, mb_idx, mb_width, py, px, spw, slice_id)
        .or_else(|| neighbor_above_left_l1(store, cur, mb_idx, mb_width, py, px, slice_id));
    median_pred(a, b, c, ref_idx)
}

/// Write a partition's L0 and L1 cells into the current macroblock grid.
fn commit_rect(
    cur: &mut [MvCell; 16],
    px: usize,
    py: usize,
    w: usize,
    h: usize,
    mv: [i32; 2],
    ref_idx: i32,
    mv_l1: [i32; 2],
    ref_idx_l1: i32,
) {
    for by in (py / 4)..(py / 4 + h / 4) {
        for bx in (px / 4)..(px / 4 + w / 4) {
            cur[by * 4 + bx] = MvCell {
                mv,
                ref_idx,
                mv_l1,
                ref_idx_l1,
            };
        }
    }
}

/// Compute and commit the 16-block motion grid of one inter macroblock.
///
/// The raw `mvd_l0` from [`InterMotion`] is added to each partition's
/// predictor, per §8.4.1. P-skip macroblocks use §8.4.1.1 with ref index 0.
pub(crate) fn predict_inter_macroblock(
    store: &MvStore,
    cur: &mut [MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    slice_id: u32,
    mb: &Macroblock,
) -> Result<(), &'static str> {
    match mb.mb_type {
        MbType::PSkip => {
            let mv = predict_skip_mv(store, cur, mb_idx, mb_width, slice_id);
            commit_rect(cur, 0, 0, 16, 16, mv, 0, [0, 0], LIST_NOT_USED);
            Ok(())
        }
        MbType::PL016x16 => {
            let motion = inter_motion(mb)?;
            let ref_idx = *motion
                .ref_idx_l0
                .first()
                .ok_or("P_16x16 without ref_idx_l0")?;
            let mvd = *motion.mvd_l0.first().ok_or("P_16x16 without mvd_l0")?;
            let pred = predict_mv(store, cur, mb_idx, mb_width, 0, 16, 16, ref_idx, slice_id);
            let mv = [pred[0] + mvd.0, pred[1] + mvd.1];
            commit_rect(cur, 0, 0, 16, 16, mv, ref_idx, [0, 0], LIST_NOT_USED);
            Ok(())
        }
        MbType::P16x8 => {
            let motion = inter_motion(mb)?;
            for part in 0..2 {
                let ref_idx = *motion
                    .ref_idx_l0
                    .get(part)
                    .ok_or("P_16x8 without ref_idx_l0")?;
                let mvd = *motion.mvd_l0.get(part).ok_or("P_16x8 without mvd_l0")?;
                let pred = predict_mv(store, cur, mb_idx, mb_width, part, 16, 8, ref_idx, slice_id);
                let mv = [pred[0] + mvd.0, pred[1] + mvd.1];
                commit_rect(cur, 0, 8 * part, 16, 8, mv, ref_idx, [0, 0], LIST_NOT_USED);
            }
            Ok(())
        }
        MbType::P8x16 => {
            let motion = inter_motion(mb)?;
            for part in 0..2 {
                let ref_idx = *motion
                    .ref_idx_l0
                    .get(part)
                    .ok_or("P_8x16 without ref_idx_l0")?;
                let mvd = *motion.mvd_l0.get(part).ok_or("P_8x16 without mvd_l0")?;
                let pred = predict_mv(store, cur, mb_idx, mb_width, part, 8, 16, ref_idx, slice_id);
                let mv = [pred[0] + mvd.0, pred[1] + mvd.1];
                commit_rect(cur, 8 * part, 0, 8, 16, mv, ref_idx, [0, 0], LIST_NOT_USED);
            }
            Ok(())
        }
        MbType::P8x8 | MbType::P8x8ref0 => {
            let motion = inter_motion(mb)?;
            let sub = motion.sub_mb_type.ok_or("P_8x8 without sub_mb_type")?;
            let mut mvd_cursor = 0usize;
            for part in 0..4 {
                let bx = 8 * (part % 2);
                let by = 8 * (part / 2);
                let ref_idx = *motion
                    .ref_idx_l0
                    .get(part)
                    .ok_or("P_8x8 without ref_idx_l0")?;
                for j in 0..P_SUB_MB_PARTS[sub[part] as usize] {
                    let mvd = *motion
                        .mvd_l0
                        .get(mvd_cursor)
                        .ok_or("P_8x8 without mvd_l0")?;
                    mvd_cursor += 1;
                    let (px, py, w, h) = match sub[part] {
                        0 => (bx, by, 8, 8),
                        1 => (bx, by + 4 * j, 8, 4),
                        2 => (bx + 4 * j, by, 4, 8),
                        3 => (bx + 4 * (j % 2), by + 4 * (j / 2), 4, 4),
                        _ => return Err("invalid sub_mb_type"),
                    };
                    let pred =
                        predict_mv_sub(store, cur, mb_idx, mb_width, px, py, w, ref_idx, slice_id);
                    let mv = [pred[0] + mvd.0, pred[1] + mvd.1];
                    commit_rect(cur, px, py, w, h, mv, ref_idx, [0, 0], LIST_NOT_USED);
                }
            }
            Ok(())
        }
        _ => Err("not an inter P macroblock"),
    }
}

fn inter_motion(mb: &Macroblock) -> Result<&InterMotion, &'static str> {
    mb.motion.as_ref().ok_or("inter macroblock without motion")
}

/// Run MV prediction over a slice's macroblocks and commit their grids.
///
/// `first_mb` is the slice's first macroblock address; intra macroblocks are
/// stored as fully-`LIST_NOT_USED` grids so later inter macroblocks treat
/// them as unavailable neighbours.
pub(crate) fn predict_slice_mvs(
    store: &mut MvStore,
    mb_cols: u32,
    slice_id: u32,
    first_mb: u32,
    mbs: &[Macroblock],
) -> Result<(), &'static str> {
    predict_slice_mvs_ex(store, mb_cols, slice_id, first_mb, mbs, false)
}

/// As [`predict_slice_mvs`], with `mbaff_frame` selecting pair-scan decode
/// order. `mbs` must be indexed by frame-MB grid address (as the MBAFF-aware
/// parsers now commit). Processing in decode order matters: a macroblock's
/// above-right neighbour is only an available MV predictor once it has been
/// decoded (spec §6.4.9 / §8.4.1.3.2), and in an MBAFF frame the next pair's
/// top macroblock is decoded *after* the current pair's bottom.
pub(crate) fn predict_slice_mvs_ex(
    store: &mut MvStore,
    mb_cols: u32,
    slice_id: u32,
    first_mb: u32,
    mbs: &[Macroblock],
    mbaff_frame: bool,
) -> Result<(), &'static str> {
    let total = mbs.len();
    let cols = mb_cols as usize;
    for d in 0..total {
        let grid_idx = if mbaff_frame {
            let pair = d >> 1;
            let px = pair % cols;
            let py = pair / cols;
            (2 * py + (d & 1)) * cols + px
        } else {
            first_mb as usize + d
        };
        let mb = &mbs[if mbaff_frame { grid_idx } else { d }];
        let mut cur = [MvCell::INTRA; 16];
        // MBAFF: record this MB's field convention so later neighbours are
        // FIX_MV_MBAFF-converted when their own convention differs, and
        // declare it for THIS MB's own neighbour fetches.
        store.set_mb_field(grid_idx, mb.mb_field_flag);
        store.set_cur_field(mb.mb_field_flag);
        if mb.motion.is_some() || mb.skip {
            predict_inter_macroblock(store, &mut cur, grid_idx, cols, slice_id, mb)?;
        }
        store.commit(grid_idx, cur, slice_id);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// B-slice constants (Table 7-15)
// ---------------------------------------------------------------------------

/// Number of sub-partitions per B_8x8 `sub_mb_type` (0..=12), in the exact
/// order of spec Table 7-18 / FFmpeg `ff_h264_b_sub_mb_type_info` (which the
/// CABAC `decode_cabac_b_mb_sub_type` and CAVLC `ue(v)` both index directly):
///   0 = B_Direct_8x8            (Direct)
///   1/2/3 = B_{L0,L1,Bi}_8x8    (1 part, 8×8)
///   4/5 = B_L0_{8x4,4x8}        (2 parts)
///   6/7 = B_L1_{8x4,4x8}        (2 parts)
///   8/9 = B_Bi_{8x4,4x8}        (2 parts)
///   10/11/12 = B_{L0,L1,Bi}_4x4 (4 parts, 4×4)
pub(crate) const B_SUB_MB_PARTS: [usize; 13] = [1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 4, 4, 4];

/// Prediction direction per B_8x8 `sub_mb_type` (0..=12), same layout as
/// [`B_SUB_MB_PARTS`] (spec Table 7-18 order).
pub(crate) const B_SUB_MB_DIR: [crate::macroblock::BPredDir; 13] = {
    use crate::macroblock::BPredDir;
    [
        BPredDir::Direct, // 0  B_Direct_8x8
        BPredDir::L0,     // 1  B_L0_8x8
        BPredDir::L1,     // 2  B_L1_8x8
        BPredDir::Bi,     // 3  B_Bi_8x8
        BPredDir::L0,     // 4  B_L0_8x4
        BPredDir::L0,     // 5  B_L0_4x8
        BPredDir::L1,     // 6  B_L1_8x4
        BPredDir::L1,     // 7  B_L1_4x8
        BPredDir::Bi,     // 8  B_Bi_8x4
        BPredDir::Bi,     // 9  B_Bi_4x8
        BPredDir::L0,     // 10 B_L0_4x4
        BPredDir::L1,     // 11 B_L1_4x4
        BPredDir::Bi,     // 12 B_Bi_4x4
    ]
};

/// Compute (px, py, width, height) for sub-partition `j` of a B_8x8 sub_mb_type
/// within the 8×8 block whose top-left corner is at `(bx, by)`.
///
/// Partition shapes follow spec Table 7-18 order: `{1,2,3}` are one 8×8,
/// `{4,6,8}` two 8×4, `{5,7,9}` two 4×8, `{10,11,12}` four 4×4.
fn b8x8_sub_rect(sub_type: usize, bx: usize, by: usize, j: usize) -> (usize, usize, usize, usize) {
    match sub_type {
        // 8×8: one sub-part
        1..=3 => (bx, by, 8, 8),
        // 8×4: two sub-parts (top / bottom)
        4 | 6 | 8 => (bx, by + 4 * j, 8, 4),
        // 4×8: two sub-parts (left / right)
        5 | 7 | 9 => (bx + 4 * j, by, 4, 8),
        // 4×4: four sub-parts
        10..=12 => (bx + 4 * (j % 2), by + 4 * (j / 2), 4, 4),
        _ => (bx, by, 8, 8),
    }
}

/// Compute and commit the 16-block motion grid of one inter B macroblock.
///
/// Handles all B-slice partition types. Direct mode (`BSkip`/`BDirect16x16`
/// and `B_Direct_8x8` sub-partitions) uses a simplified fill — pixel-exact
/// direct mode (spatial/temporal derivation per §8.4.1.2) is a Phase E.5 task.
#[allow(clippy::too_many_arguments)]
/// Median of three values (FFmpeg `mid_pred`).
fn mid3(a: i32, b: i32, c: i32) -> i32 {
    let mut v = [a, b, c];
    v.sort_unstable();
    v[1]
}

/// Spatial direct-mode motion derivation for one macroblock
/// (spec §8.4.1.2.2; transcribed from FFmpeg `pred_spatial_direct_motion`).
///
/// For each list independently: gather the A (left), B (above) and C
/// (above-right; D above-left fallback when C is unavailable) neighbour cells
/// of the macroblock's top-left corner; the list's reference index is the
/// smallest non-negative one among them and its motion vector comes from a
/// matching neighbour (majority → median). When no neighbour uses the list,
/// that list is *dropped* (`used == false`); when both are dropped both lists
/// fall back to reference 0 / zero MV.
fn derive_spatial_direct(
    store: &MvStore,
    cur: &[MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    slice_id: u32,
) -> ([i32; 2], [[i32; 2]; 2], [bool; 2]) {
    let mut refs = [0i32; 2];
    let mut mvs = [[0i32; 2]; 2];
    let mut used = [true; 2];
    for list in 0..2usize {
        let (a, b, c_raw) = match list {
            1 => (
                neighbor_left_l1(store, cur, mb_idx, mb_width, 0, 0, slice_id),
                neighbor_above_l1(store, cur, mb_idx, mb_width, 0, 0, slice_id),
                neighbor_above_right_l1(store, cur, mb_idx, mb_width, 0, 0, 16, slice_id),
            ),
            _ => (
                neighbor_left(store, cur, mb_idx, mb_width, 0, 0, slice_id),
                neighbor_above(store, cur, mb_idx, mb_width, 0, 0, slice_id),
                neighbor_above_right(store, cur, mb_idx, mb_width, 0, 0, 16, slice_id),
            ),
        };
        // D is consulted only when C is *unavailable*; a decoded neighbour
        // whose prediction does not use this list stays as its own candidate.
        let d = match list {
            1 => neighbor_above_left_l1(store, cur, mb_idx, mb_width, 0, 0, slice_id),
            _ => neighbor_above_left(store, cur, mb_idx, mb_width, 0, 0, slice_id),
        };
        let c = c_raw.or(d);
        let usable = |o: &Option<MvNeighbor>| o.as_ref().is_some_and(|n| n.ref_idx >= 0);
        let mv_of = |o: &Option<MvNeighbor>| o.as_ref().map(|n| n.mv).unwrap_or([0, 0]);
        if !usable(&a) && !usable(&b) && !usable(&c) {
            mvs[list] = [0, 0];
            used[list] = false;
            continue;
        }
        let cands = [(&a, usable(&a)), (&b, usable(&b)), (&c, usable(&c))];
        let min_ref = cands
            .iter()
            .filter(|(_, u)| *u)
            .map(|(o, _)| o.as_ref().unwrap().ref_idx)
            .min()
            .unwrap();
        let matches = cands
            .iter()
            .filter(|(o, u)| *u && o.as_ref().unwrap().ref_idx == min_ref)
            .count();
        mvs[list] = if matches > 1 {
            let (av, bv, cv) = (mv_of(&a), mv_of(&b), mv_of(&c));
            [mid3(av[0], bv[0], cv[0]), mid3(av[1], bv[1], cv[1])]
        } else {
            cands
                .iter()
                .find(|(o, u)| *u && o.as_ref().unwrap().ref_idx == min_ref)
                .map(|(o, _)| mv_of(o))
                .unwrap()
        };
        refs[list] = min_ref;
    }
    if !used[0] && !used[1] {
        used = [true, true];
        refs = [0, 0];
        mvs = [[0, 0], [0, 0]];
    }
    (refs, mvs, used)
}

/// Fill the direct 8×8 quadrants `quads` with the derived spatial-direct
/// motion and apply the colocated zero-MV rule (`col_zero_flag`), which needs
/// the co-located picture's per-block motion grid (list-1 reference 0).
#[allow(clippy::too_many_arguments)]
fn apply_spatial_direct(
    store: &MvStore,
    cur: &mut [MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    slice_id: u32,
    quads: &[usize],
    colocated: Option<&[[MvCell; 16]]>,
) {
    let (refs, mvs, used) = derive_spatial_direct(store, cur, mb_idx, mb_width, slice_id);
    let fill = |cur: &mut [MvCell; 16], bx: usize, by: usize| {
        commit_rect(
            cur,
            bx,
            by,
            8,
            8,
            mvs[0],
            if used[0] { refs[0] } else { LIST_NOT_USED },
            mvs[1],
            if used[1] { refs[1] } else { LIST_NOT_USED },
        );
    };
    // Whole-macroblock fast path: both MVs zero → plain fill, no col_zero pass
    // (FFmpeg returns before its colocated adjustment in this case).
    if quads.len() == 4 && mvs[0] == [0, 0] && mvs[1] == [0, 0] {
        for q in [0usize, 1, 2, 3] {
            fill(cur, 8 * (q % 2), 8 * (q / 2));
        }
        return;
    }
    for &q in quads {
        fill(cur, 8 * (q % 2), 8 * (q / 2));
    }
    // col_zero_flag: zero each list's MV where refIdx is 0 and the colocated
    // block's corresponding MV is (near-)zero.
    if let Some(cells) = colocated.and_then(|g| g.get(mb_idx)) {
        let coloc_intra = cells.iter().all(|c| c.ref_idx < 0 && c.ref_idx_l1 < 0);
        if !coloc_intra {
            for &q in quads {
                let cc = cells[8 * (q / 2) + (q % 2) * 2];
                let col_zero = (cc.ref_idx == 0 && cc.mv[0].abs() <= 1 && cc.mv[1].abs() <= 1)
                    || (cc.ref_idx < 0
                        && cc.ref_idx_l1 == 0
                        && cc.mv_l1[0].abs() <= 1
                        && cc.mv_l1[1].abs() <= 1);
                if col_zero {
                    let blocks =
                        crate::slice_data::partition_blocks(2 * (q % 2), 2 * (q / 2), 2, 2);
                    for blk in blocks {
                        if used[0] && refs[0] == 0 {
                            cur[blk].mv = [0, 0];
                        }
                        if used[1] && refs[1] == 0 {
                            cur[blk].mv_l1 = [0, 0];
                        }
                    }
                }
            }
        }
    }
}

pub(crate) fn predict_inter_b_macroblock(
    store: &MvStore,
    cur: &mut [MvCell; 16],
    mb_idx: usize,
    mb_width: usize,
    slice_id: u32,
    mb: &Macroblock,
    colocated: Option<&[[MvCell; 16]]>,
) -> Result<(), &'static str> {
    use crate::macroblock::{BPredDir, MbType};

    match mb.mb_type {
        MbType::BSkip | MbType::BDirect16x16 => {
            apply_spatial_direct(
                store,
                cur,
                mb_idx,
                mb_width,
                slice_id,
                &[0, 1, 2, 3],
                colocated,
            );
            Ok(())
        }
        MbType::BL016x16 => {
            let motion = inter_motion(mb)?;
            let ref_idx = *motion
                .ref_idx_l0
                .first()
                .ok_or("BL0_16x16 without ref_idx_l0")?;
            let mvd = *motion.mvd_l0.first().ok_or("BL0_16x16 without mvd_l0")?;
            let pred = predict_mv(store, cur, mb_idx, mb_width, 0, 16, 16, ref_idx, slice_id);
            let mv = [pred[0] + mvd.0, pred[1] + mvd.1];
            commit_rect(cur, 0, 0, 16, 16, mv, ref_idx, [0, 0], LIST_NOT_USED);
            Ok(())
        }
        MbType::BL116x16 => {
            let motion = inter_motion(mb)?;
            let ref_idx_l1 = *motion
                .ref_idx_l1
                .first()
                .ok_or("BL1_16x16 without ref_idx_l1")?;
            let mvd_l1 = *motion.mvd_l1.first().ok_or("BL1_16x16 without mvd_l1")?;
            let pred_l1 = predict_mv_l1(
                store, cur, mb_idx, mb_width, 0, 16, 16, ref_idx_l1, slice_id,
            );
            let mv_l1 = [pred_l1[0] + mvd_l1.0, pred_l1[1] + mvd_l1.1];
            if crate::entropy::bin_trace_enabled() {
                eprintln!(
                    "  BL1MV(mb{mb_idx}) ri1={ref_idx_l1} pred1=({},{})+({},{}) -> mv1={mv_l1:?}",
                    pred_l1[0], pred_l1[1], mvd_l1.0, mvd_l1.1
                );
            }
            commit_rect(cur, 0, 0, 16, 16, [0, 0], LIST_NOT_USED, mv_l1, ref_idx_l1);
            Ok(())
        }
        MbType::BBi16x16 => {
            let motion = inter_motion(mb)?;
            let ref_idx = *motion
                .ref_idx_l0
                .first()
                .ok_or("BBi_16x16 without ref_idx_l0")?;
            let mvd_l0 = *motion.mvd_l0.first().ok_or("BBi_16x16 without mvd_l0")?;
            let ref_idx_l1 = *motion
                .ref_idx_l1
                .first()
                .ok_or("BBi_16x16 without ref_idx_l1")?;
            let mvd_l1 = *motion.mvd_l1.first().ok_or("BBi_16x16 without mvd_l1")?;
            let pred = predict_mv(store, cur, mb_idx, mb_width, 0, 16, 16, ref_idx, slice_id);
            let pred_l1 = predict_mv_l1(
                store, cur, mb_idx, mb_width, 0, 16, 16, ref_idx_l1, slice_id,
            );
            let mv = [pred[0] + mvd_l0.0, pred[1] + mvd_l0.1];
            let mv_l1 = [pred_l1[0] + mvd_l1.0, pred_l1[1] + mvd_l1.1];
            if crate::entropy::bin_trace_enabled() {
                eprintln!(
                    "  BBiMV({mb_idx}) pred0=({},{})+({},{}) -> mv0={mv:?}  pred1=({},{})+({},{}) -> mv1={mv_l1:?}",
                    pred[0],
                    pred[1],
                    mvd_l0.0,
                    mvd_l0.1,
                    pred_l1[0],
                    pred_l1[1],
                    mvd_l1.0,
                    mvd_l1.1
                );
            }
            commit_rect(cur, 0, 0, 16, 16, mv, ref_idx, mv_l1, ref_idx_l1);
            Ok(())
        }
        MbType::B16x8 | MbType::B8x16 => {
            let motion = inter_motion(mb)?;
            let is_16x8 = mb.mb_type == MbType::B16x8;
            let (part_w, part_h) = if is_16x8 { (16, 8) } else { (8, 16) };
            let mut l0_cur = 0usize;
            let mut l1_cur = 0usize;
            for part in 0..2usize {
                let dir = motion
                    .pred_dirs
                    .get(part)
                    .copied()
                    .ok_or("B16x8/B8x16 without pred_dirs")?;
                let ref_idx = if dir == BPredDir::L0 || dir == BPredDir::Bi {
                    *motion
                        .ref_idx_l0
                        .get(part)
                        .ok_or("B2part without ref_idx_l0")?
                } else {
                    LIST_NOT_USED
                };
                let ref_idx_l1 = if dir == BPredDir::L1 || dir == BPredDir::Bi {
                    *motion
                        .ref_idx_l1
                        .get(part)
                        .ok_or("B2part without ref_idx_l1")?
                } else {
                    LIST_NOT_USED
                };
                let mv = if dir == BPredDir::L0 || dir == BPredDir::Bi {
                    let mvd = *motion.mvd_l0.get(l0_cur).ok_or("B2part without mvd_l0")?;
                    l0_cur += 1;
                    let pred = predict_mv(
                        store, cur, mb_idx, mb_width, part, part_w, part_h, ref_idx, slice_id,
                    );
                    [pred[0] + mvd.0, pred[1] + mvd.1]
                } else {
                    [0, 0]
                };
                let mv_l1 = if dir == BPredDir::L1 || dir == BPredDir::Bi {
                    let mvd = *motion.mvd_l1.get(l1_cur).ok_or("B2part without mvd_l1")?;
                    l1_cur += 1;
                    let pred_l1 = predict_mv_l1(
                        store, cur, mb_idx, mb_width, part, part_w, part_h, ref_idx_l1, slice_id,
                    );
                    [pred_l1[0] + mvd.0, pred_l1[1] + mvd.1]
                } else {
                    [0, 0]
                };
                let (px, py) = if is_16x8 {
                    (0, 8 * part)
                } else {
                    (8 * part, 0)
                };
                commit_rect(cur, px, py, part_w, part_h, mv, ref_idx, mv_l1, ref_idx_l1);
            }
            Ok(())
        }
        MbType::BB8x8 => {
            let motion = inter_motion(mb)?;
            let sub_types = motion.sub_mb_type_b.ok_or("BB8x8 without sub_mb_type_b")?;
            let mut l0_cur = 0usize;
            let mut l1_cur = 0usize;
            let mut direct_quads: Vec<usize> = Vec::new();
            for part in 0..4usize {
                let bx = 8 * (part % 2);
                let by = 8 * (part / 2);
                let sub_type = sub_types[part] as usize;
                let dir = if sub_type < 13 {
                    B_SUB_MB_DIR[sub_type]
                } else {
                    BPredDir::Direct
                };
                let n_sub = if sub_type < 13 {
                    B_SUB_MB_PARTS[sub_type]
                } else {
                    1
                };

                if dir == BPredDir::Direct {
                    direct_quads.push(part);
                    continue;
                }

                let ref_idx = if dir == BPredDir::L0 || dir == BPredDir::Bi {
                    *motion
                        .ref_idx_l0
                        .get(part)
                        .ok_or("BB8x8 without ref_idx_l0")?
                } else {
                    LIST_NOT_USED
                };
                let ref_idx_l1 = if dir == BPredDir::L1 || dir == BPredDir::Bi {
                    *motion
                        .ref_idx_l1
                        .get(part)
                        .ok_or("BB8x8 without ref_idx_l1")?
                } else {
                    LIST_NOT_USED
                };

                for j in 0..n_sub {
                    let (spx, spy, spw, sph) = b8x8_sub_rect(sub_type, bx, by, j);
                    let mv = if dir == BPredDir::L0 || dir == BPredDir::Bi {
                        let mvd = *motion.mvd_l0.get(l0_cur).ok_or("BB8x8 without mvd_l0")?;
                        l0_cur += 1;
                        let pred = predict_mv_sub(
                            store, cur, mb_idx, mb_width, spx, spy, spw, ref_idx, slice_id,
                        );
                        [pred[0] + mvd.0, pred[1] + mvd.1]
                    } else {
                        [0, 0]
                    };
                    let mv_l1 = if dir == BPredDir::L1 || dir == BPredDir::Bi {
                        let mvd = *motion.mvd_l1.get(l1_cur).ok_or("BB8x8 without mvd_l1")?;
                        l1_cur += 1;
                        let pred_l1 = predict_mv_sub_l1(
                            store, cur, mb_idx, mb_width, spx, spy, spw, ref_idx_l1, slice_id,
                        );
                        [pred_l1[0] + mvd.0, pred_l1[1] + mvd.1]
                    } else {
                        [0, 0]
                    };
                    commit_rect(cur, spx, spy, spw, sph, mv, ref_idx, mv_l1, ref_idx_l1);
                }
            }
            if !direct_quads.is_empty() {
                apply_spatial_direct(
                    store,
                    cur,
                    mb_idx,
                    mb_width,
                    slice_id,
                    &direct_quads,
                    colocated,
                );
            }
            Ok(())
        }
        _ => Err("not an inter B macroblock"),
    }
}

/// Run B-slice MV prediction over a slice's macroblocks and commit their grids.
///
/// B-inter macroblocks are predicted with `predict_inter_b_macroblock`; intra
/// macroblocks are stored as fully-`LIST_NOT_USED` grids.
pub(crate) fn predict_b_slice_mvs(
    store: &mut MvStore,
    mb_cols: u32,
    slice_id: u32,
    first_mb: u32,
    mbs: &[Macroblock],
    colocated: Option<&[[MvCell; 16]]>,
) -> Result<(), &'static str> {
    use crate::macroblock::MbType;
    let mut cur = [MvCell::INTRA; 16];
    for (i, mb) in mbs.iter().enumerate() {
        let mb_idx = first_mb as usize + i;
        let is_b_inter = mb.skip
            || matches!(
                mb.mb_type,
                MbType::BSkip
                    | MbType::BDirect16x16
                    | MbType::BL016x16
                    | MbType::BL116x16
                    | MbType::BBi16x16
                    | MbType::B16x8
                    | MbType::B8x16
                    | MbType::BB8x8
            );
        if is_b_inter {
            predict_inter_b_macroblock(
                store,
                &mut cur,
                mb_idx,
                mb_cols as usize,
                slice_id,
                mb,
                colocated,
            )?;
        } else {
            cur = [MvCell::INTRA; 16];
        }
        store.commit(mb_idx, cur, slice_id);
    }
    Ok(())
}

/// Vertical motion-vector scaling for interlaced (field) prediction (§8.4.1.3).
///
/// When a field macroblock predicts from a reference *field* (or from one field
/// of a frame reference), the vertical component of its motion vector is scaled
/// by the ratio of the temporal distances between the current field and the
/// reference field (`tb`) and between the two fields of the reference frame
/// (`td`):
///
/// ```text
/// tx = (16384 + |td| / 2) / td            (td != 0)
/// dist_scale_factor = (tb * tx + 32) >> 6
/// mvY' = (mvY * dist_scale_factor + 256) >> 9
/// ```
///
/// `td` and `tb` are nominal temporal distances (in field units), `Clip3(-128,
/// 127, …)`; convention: `DistRef0`/`DistRef1` are the `PicOrderCnt` of the
/// reference frame's top/bottom fields and `DistCur0`/`DistCur1` the current
/// picture's, so `td = DistRef1 − DistRef0` and `tb = CurFieldPOC −
/// RefFieldPOC`. With `td == 0` (no temporal separation between the reference
/// frame's fields) the MV is returned unchanged.
pub fn scale_field_mv_y(mv_y: i32, tb: i32, td: i32) -> i32 {
    if td == 0 {
        return mv_y;
    }
    let tb = tb.clamp(-128, 127);
    let td = td.clamp(-128, 127);
    let td_abs = td.unsigned_abs() as i32;
    let tx = (16384 + td_abs / 2) / td;
    let dist_scale_factor = (tb * tx + 32) >> 6;
    (mv_y * dist_scale_factor + 256) >> 9
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(mv: [i32; 2], ref_idx: i32) -> MvCell {
        MvCell {
            mv,
            ref_idx,
            mv_l1: [0, 0],
            ref_idx_l1: LIST_NOT_USED,
        }
    }

    /// A store with a single committed macroblock.
    fn store_with(mb_idx: usize, grid: [MvCell; 16], slice_id: u32) -> MvStore {
        let mut store = MvStore::new(16);
        store.commit(mb_idx, grid, slice_id);
        store
    }

    #[test]
    fn skip_corner_returns_zero() {
        // MB (0,0): no A (col 0), no B (row 0) -> (0,0).
        let store = MvStore::new(16);
        let cur = [MvCell::INTRA; 16];
        assert_eq!(predict_skip_mv(&store, &cur, 0, 4, 1), [0, 0]);
    }

    #[test]
    fn skip_zero_ref_neighbor_returns_zero() {
        // MB (1,1) = mb_idx 5. A (left MB 4, block 3) = ref 0, mv (0,0) -> zero.
        let mut grid = [cell([1, 1], 0); 16];
        grid[3] = cell([0, 0], 0);
        let store = store_with(4, grid, 1);
        let cur = [MvCell::INTRA; 16];
        assert_eq!(predict_skip_mv(&store, &cur, 5, 4, 1), [0, 0]);
    }

    #[test]
    fn skip_uses_median_predictor() {
        // MB (1,1) = mb_idx 5. A = left MB 4 block 3 (mv 3,4 ref 1), B = above
        // MB 1 block 12 (mv 1,1 ref 1), C = above-right MB 2 block 12
        // (mv 1,1 ref 1) -> neither A nor B is zero -> median predictor ref 0.
        let mut left = [cell([9, 9], 1); 16];
        left[3] = cell([3, 4], 1);
        let mut above = [cell([9, 9], 1); 16];
        above[12] = cell([1, 1], 1);
        let mut above_right = [cell([9, 9], 1); 16];
        above_right[12] = cell([1, 1], 1);
        let mut store = MvStore::new(16);
        store.commit(4, left, 1);
        store.commit(1, above, 1);
        store.commit(2, above_right, 1);
        let cur = [MvCell::INTRA; 16];
        // match_count ref 0 = 0 -> median of A (3,4), B (1,1), C (1,1) = (1,1).
        assert_eq!(predict_skip_mv(&store, &cur, 5, 4, 1), [1, 1]);
    }

    #[test]
    fn skip_b_not_in_slice_unavailable() {
        // MB (1,1) in slice 2; above MB (0) decoded in slice 1 -> unavailable.
        let mut left = [cell([9, 9], 1); 16];
        left[3] = cell([0, 0], 1);
        let store = store_with(3, left, 2);
        let cur = [MvCell::INTRA; 16];
        // A available (non-zero), B unavailable -> (0,0).
        assert_eq!(predict_skip_mv(&store, &cur, 4, 4, 2), [0, 0]);
    }

    #[test]
    fn p16x16_single_match_uses_that_neighbor() {
        // MB (1,1) = mb_idx 5, 16x16 ref 1. Only A (left MB 4 block 3,
        // mv 3,3 ref 1) matches; B (mb 1) and C (mb 2) are ref 0.
        let mut left = [cell([9, 9], 0); 16];
        left[3] = cell([3, 3], 1);
        let mut above = [cell([9, 9], 0); 16];
        above[12] = cell([1, 2], 0);
        let mut above_right = [cell([9, 9], 0); 16];
        above_right[12] = cell([1, 2], 0);
        let mut store = MvStore::new(16);
        store.commit(4, left, 1);
        store.commit(1, above, 1);
        store.commit(2, above_right, 1);
        let cur = [cell([0, 0], 0); 16];
        let mv = predict_mv(&store, &cur, 5, 4, 0, 16, 16, 1, 1);
        assert_eq!(mv, [3, 3]);
    }

    #[test]
    fn p16x16_median_when_all_match() {
        // MB (1,1) = mb_idx 5. All three neighbours ref 1 -> median of
        // A (3,4), B (1,2), C (2,1) = (2,2).
        let mut left = [cell([9, 9], 0); 16];
        left[3] = cell([3, 4], 1);
        let mut above = [cell([9, 9], 0); 16];
        above[12] = cell([1, 2], 1);
        let mut above_right = [cell([9, 9], 0); 16];
        above_right[12] = cell([2, 1], 1);
        let mut store = MvStore::new(16);
        store.commit(4, left, 1);
        store.commit(1, above, 1);
        store.commit(2, above_right, 1);
        let cur = [cell([0, 0], 0); 16];
        let mv = predict_mv(&store, &cur, 5, 4, 0, 16, 16, 1, 1);
        assert_eq!(mv, [2, 2]);
    }

    #[test]
    fn p16x16_count_zero_uses_a_when_bc_missing() {
        // MB (1,0) top row: B/C/D all unavailable (row 0). A (mb 0) ref 1.
        // Predict ref 0 -> count 0, B+C missing, A present -> A's MV.
        let mut grid = [cell([9, 9], 0); 16];
        grid[3] = cell([5, 7], 1);
        let store = store_with(0, grid, 1);
        let cur = [cell([0, 0], 0); 16];
        let mv = predict_mv(&store, &cur, 1, 4, 0, 16, 16, 0, 1);
        assert_eq!(mv, [5, 7]);
    }

    #[test]
    fn p16x16_d_fallback_at_right_edge() {
        // MB (1,1) at right picture edge (mb_width 2). C unavailable, D =
        // above-left MB (0) block 15. D ref 1 -> single match.
        let mut above_left = [cell([9, 9], 0); 16];
        above_left[15] = cell([4, 4], 1);
        let mut left = [cell([9, 9], 0); 16];
        left[3] = cell([8, 8], 0);
        let mut store = MvStore::new(16);
        store.commit(0, above_left, 1);
        store.commit(1, left, 1);
        let cur = [cell([0, 0], 0); 16];
        let mv = predict_mv(&store, &cur, 3, 2, 0, 16, 16, 1, 1);
        assert_eq!(mv, [4, 4]);
    }

    #[test]
    fn p16x8_part0_uses_b() {
        // MB (1,1) 16x8 part 0 ref 1. B = above MB block 12 ref 1 -> return B.
        let mut above = [cell([9, 9], 0); 16];
        above[12] = cell([6, 6], 1);
        let store = store_with(0, above, 1);
        let cur = [cell([0, 0], 0); 16];
        let mv = predict_mv(&store, &cur, 4, 4, 0, 16, 8, 1, 1);
        assert_eq!(mv, [6, 6]);
    }

    #[test]
    fn p16x8_part1_uses_a() {
        // MB (1,1) = mb_idx 5, 16x8 part 1 (py=8) ref 1. A = left MB 4
        // block 11 (mv 7,7 ref 1) matches.
        let mut left = [cell([9, 9], 0); 16];
        left[11] = cell([7, 7], 1);
        let store = store_with(4, left, 1);
        let cur = [cell([0, 0], 0); 16];
        let mv = predict_mv(&store, &cur, 5, 4, 1, 16, 8, 1, 1);
        assert_eq!(mv, [7, 7]);
    }

    #[test]
    fn p8x16_part1_uses_c() {
        // MB (1,1) 8x16 part 1 (px=8) ref 1. C = above-right MB block 12 ref 1.
        let mut above_right = [cell([9, 9], 0); 16];
        above_right[12] = cell([2, 5], 1);
        let store = store_with(1, above_right, 1);
        let cur = [cell([0, 0], 0); 16];
        let mv = predict_mv(&store, &cur, 4, 4, 1, 8, 16, 1, 1);
        assert_eq!(mv, [2, 5]);
    }

    #[test]
    fn p8x8_sub_within_mb_neighbors() {
        // MB (1,1) P_8x8 at (8,8): neighbours all within the current MB.
        // Commit the first three 8x8 sub-MBs, then predict 4x4 partition 0.
        let mut cur = [cell([0, 0], 0); 16];
        // 8x8 idx 0 at (0,0) ref 0
        for c in cur[0..4].iter_mut() {
            *c = cell([1, 1], 0);
        }
        // 8x8 idx 1 at (8,0) ref 0
        for c in cur[4..8].iter_mut() {
            *c = cell([2, 2], 0);
        }
        // 8x8 idx 2 at (0,8) ref 0
        for c in cur[8..12].iter_mut() {
            *c = cell([3, 3], 0);
        }
        // Predict 4x4 at (8,8) ref 0: A = block 9 (mv 3,3), B = block 6
        // (mv 2,2), C = block 7 (mv 2,2) -> all match -> median (2,2).
        let store = MvStore::new(16);
        let mv = predict_mv_sub(&store, &cur, 4, 4, 8, 8, 4, 0, 1);
        assert_eq!(mv, [2, 2]);
    }

    #[test]
    fn p8x8_sub_c_unavailable_uses_d() {
        // 8x8 at (8,8): C at right MB edge is unavailable -> D = block 5.
        // Set A = block 9 ref 0, B = block 6 ref 0, D = block 5 ref 1.
        let mut cur = [cell([9, 9], 0); 16];
        cur[9] = cell([1, 1], 0);
        cur[6] = cell([2, 2], 0);
        cur[5] = cell([4, 4], 1);
        // Predict 8x8 at (8,8) ref 1: single match D -> D's MV.
        let store = MvStore::new(16);
        let mv = predict_mv_sub(&store, &cur, 4, 4, 8, 8, 8, 1, 1);
        assert_eq!(mv, [4, 4]);
    }

    #[test]
    fn intra_neighbor_contributes_zero() {
        // MB (1,1) 16x16 ref 1. A intra (ref -1, mv 0), B ref 1 mv (5,1),
        // C ref 1 mv (1,5). match_count = 2 (B,C) -> median(0, 5,1, 1,5).
        let mut above = [cell([9, 9], 0); 16];
        above[12] = cell([5, 1], 1);
        let mut above_right = [cell([9, 9], 0); 16];
        above_right[12] = cell([1, 5], 1);
        let mut store = MvStore::new(16);
        store.commit(3, [MvCell::INTRA; 16], 1);
        store.commit(0, above, 1);
        store.commit(1, above_right, 1);
        let cur = [cell([0, 0], 0); 16];
        let mv = predict_mv(&store, &cur, 4, 4, 0, 16, 16, 1, 1);
        assert_eq!(mv, [1, 1]);
    }

    #[test]
    fn predict_inter_16x16_commits_grid() {
        let mut store = MvStore::new(16);
        // Left MB (mb 0): block 3 = ref 1, mv (3,0). Above: none (row 0).
        let mut left = [cell([9, 9], 0); 16];
        left[3] = cell([3, 0], 1);
        store.commit(0, left, 1);
        // MB 1 (0,1): 16x16 ref 1, mvd (1,1). A matches -> pred (3,0), mv (4,1).
        let mut motion = InterMotion::default();
        motion.ref_idx_l0.push(1);
        motion.mvd_l0.push((1, 1));
        let mut mb = Macroblock::new_skip();
        mb.skip = false;
        mb.mb_type = MbType::PL016x16;
        mb.motion = Some(motion);
        let mut cur = [MvCell::INTRA; 16];
        predict_inter_macroblock(&store, &mut cur, 1, 2, 1, &mb).unwrap();
        store.commit(1, cur, 1);
        let grid = store.cells_of(1).unwrap();
        assert_eq!(grid[0], cell([4, 1], 1));
        assert_eq!(grid[15], cell([4, 1], 1));
    }

    #[test]
    fn predict_slice_skips_and_intra() {
        let mut store = MvStore::new(4);
        let mut mbs = Vec::new();
        // MB 0: P_Skip.
        let mut skip = Macroblock::new_skip();
        skip.skip = true;
        skip.mb_type = MbType::PSkip;
        mbs.push(skip);
        // MB 1: intra.
        let mut intra = Macroblock::new_skip();
        intra.skip = false;
        intra.mb_type = MbType::Intra4x4;
        intra.motion = None;
        mbs.push(intra);
        // MB 2: P_16x16 ref 1 with zero mvd; A (mb 1) is intra, B/C absent.
        let mut motion = InterMotion::default();
        motion.ref_idx_l0.push(1);
        motion.mvd_l0.push((0, 0));
        let mut p = Macroblock::new_skip();
        p.skip = false;
        p.mb_type = MbType::PL016x16;
        p.motion = Some(motion);
        mbs.push(p);

        predict_slice_mvs(&mut store, 2, 1, 0, &mbs).unwrap();
        // Skip MB: (0,0) since no neighbours.
        assert_eq!(store.cells_of(0).unwrap()[0], cell([0, 0], 0));
        // Intra MB stored as LIST_NOT_USED.
        assert_eq!(store.cells_of(1).unwrap()[0], MvCell::INTRA);
        // P_16x16: A intra -> count 0, A present -> A's mv (0,0) -> mv (0,0).
        assert_eq!(store.cells_of(2).unwrap()[0], cell([0, 0], 1));
    }

    #[test]
    fn field_mv_scaling_zero_td_is_identity() {
        // No temporal separation between the reference frame's fields: the MV is
        // returned unchanged.
        assert_eq!(scale_field_mv_y(40, 0, 0), 40);
        assert_eq!(scale_field_mv_y(-8, 100, 0), -8);
    }

    #[test]
    fn field_mv_scaling_same_parity_doubles() {
        // Reference frame's fields 1 POC apart (td = 1), current top field 2
        // POCs after the reference top field (tb = 2). tx = (16384 + 0)/1 = 16384,
        // dist_scale_factor = (2*16384 + 32) >> 6 = 512, mvY' = (40*512 + 256) >> 9 = 40.
        // A same-parity field-to-field prediction with tb == td therefore maps a
        // field-line MV straight across (the field already lives at 1x spacing).
        assert_eq!(scale_field_mv_y(40, 2, 1), 40);
        // tb == td == 1 -> factor 256 -> mvY' = mvY * 256 >> 9 == mvY / 2 rounded.
        assert_eq!(scale_field_mv_y(40, 1, 1), (40 * 256 + 256) >> 9);
    }

    #[test]
    fn field_mv_scaling_opposite_parity_halves() {
        // Opposite-parity prediction (current top, reference bottom) with td = 1,
        // tb = 1 gives factor 256 (mvY/2). With td = 1 and tb = -1 the factor is
        // negative (the field is temporally before the reference), so a downward
        // MV is flipped upward — matching §8.4.1.3's scaling direction.
        let v = 40;
        let scaled = (v * 256 + 256) >> 9;
        assert_eq!(scale_field_mv_y(v, 1, 1), scaled);
        assert!(
            scaled < v,
            "same-parity field scaling should shrink a frame-line MV"
        );
    }
}
