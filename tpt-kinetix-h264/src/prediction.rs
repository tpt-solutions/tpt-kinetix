//! H.264 intra prediction (spec §8.3).
//!
//! Implements the three intra prediction families:
//!
//! * **Intra_4×4** — 9 modes (`spec §8.3.1.2`, Table 8-2) over 4×4 luma blocks.
//! * **Intra_8×8** — 9 modes (`spec §8.3.2.2`, Table 8-4) over 8×8 luma blocks.
//! * **Intra_16×16** — 4 modes (`spec §8.3.3.1`, Table 8-5) over a whole
//!   macroblock, plus the chroma 4-mode prediction (`spec §8.3.4`, Table 8-6).
//!
//! All modes take the already-reconstructed top and top-left neighbour samples
//! (the "border" rows/columns) and fill a prediction block. Boundary samples
//! that are unavailable (frame edge or un-decoded neighbour) are substituted
//! with the spec's `R` constant (`128` for 8-bit luma/chroma).

use crate::macroblock::MbType;

/// Border substitution constant for unavailable samples (8-bit, spec §8.3).
const R: i32 = 128;

/// Selectable chroma format assumptions for neighbour availability.
///
/// The decoder feeds the neighbouring samples it has already reconstructed; the
/// prediction routines treat anything `None` as unavailable and substitute `R`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntraChromaMode {
    /// DC (mode 0).
    Dc,
    /// Horizontal (mode 1).
    Horizontal,
    /// Vertical (mode 2).
    Vertical,
    /// Plane (mode 3).
    Plane,
}

impl IntraChromaMode {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Dc,
            1 => Self::Horizontal,
            2 => Self::Vertical,
            _ => Self::Plane,
        }
    }
}

/// The 9 Intra_4×4 prediction modes (spec Table 8-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Intra4x4Mode {
    Vertical = 0,
    Horizontal = 1,
    Dc = 2,
    DiagonalDownLeft = 3,
    DiagonalDownRight = 4,
    VerticalRight = 5,
    HorizontalDown = 6,
    VerticalLeft = 7,
    HorizontalUp = 8,
}

impl Intra4x4Mode {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Vertical,
            1 => Self::Horizontal,
            2 => Self::Dc,
            3 => Self::DiagonalDownLeft,
            4 => Self::DiagonalDownRight,
            5 => Self::VerticalRight,
            6 => Self::HorizontalDown,
            7 => Self::VerticalLeft,
            _ => Self::HorizontalUp,
        }
    }
}

/// The 4 Intra_16×16 prediction modes (spec Table 8-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Intra16x16Mode {
    Vertical = 0,
    Horizontal = 1,
    Dc = 2,
    Plane = 3,
}

impl Intra16x16Mode {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Vertical,
            1 => Self::Horizontal,
            2 => Self::Dc,
            _ => Self::Plane,
        }
    }
}

/// Neighbouring samples needed by the Intra_4×4 predictor.
///
/// `top[0..4]` are the samples directly above the block; `top[4..8]` are the
/// four **top-right** samples (spec §8.3.1.2.1) needed by the DiagonalDownLeft
/// and VerticalLeft modes. When top-right samples are unavailable the spec
/// substitutes `top[3]`; callers may pass `None` and the predictor applies that
/// substitution. `left[y]` is the sample to the left, and `top_left` the
/// above-left diagonal sample.
pub struct IntraNeighbours4x4 {
    pub top: [Option<u8>; 8],
    pub left: [Option<u8>; 4],
    pub top_left: Option<u8>,
}

/// Neighbouring samples for the Intra_16×16 predictor (16 samples per edge).
pub struct IntraNeighbours16x16 {
    pub top: [Option<u8>; 16],
    pub left: [Option<u8>; 16],
    pub top_left: Option<u8>,
}

fn sample(v: Option<u8>) -> i32 {
    v.map(|x| x as i32).unwrap_or(R)
}

/// Predict a 4×4 block (`out[4][4]`) for the given [`Intra4x4Mode`].
pub fn predict_4x4(mode: Intra4x4Mode, n: &IntraNeighbours4x4, out: &mut [u8; 16]) {
    // Top samples with spec top-right substitution: if a top-right sample
    // (index 4..8) is unavailable, it is replaced by top[3] (§8.3.1.2.1).
    let top3 = n.top[3];
    let t = |i: i32| -> i32 {
        let idx = i as usize;
        if idx < 8 {
            match n.top[idx] {
                Some(v) => v as i32,
                None if idx >= 4 => sample(top3),
                None => R,
            }
        } else {
            sample(top3)
        }
    };
    let l = |i: i32| sample(n.left[i as usize]);
    let tl = sample(n.top_left);

    // Index helper: out[y*4 + x]
    let mut set = |x: i32, y: i32, v: i32| {
        out[(y as usize) * 4 + (x as usize)] = v.clamp(0, 255) as u8;
    };

    match mode {
        Intra4x4Mode::Vertical => {
            for y in 0..4i32 {
                for x in 0..4i32 {
                    set(x, y, t(x));
                }
            }
        }
        Intra4x4Mode::Horizontal => {
            for y in 0..4i32 {
                for x in 0..4i32 {
                    set(x, y, l(y));
                }
            }
        }
        Intra4x4Mode::Dc => {
            // §8.3.1.2.3: the average is taken over whichever of top/left is
            // actually available — substituting a phantom `R` (128) for a
            // missing side into an 8-sample average (as `t`/`l` do for the
            // directional modes) would corrupt the DC value, not just fill in
            // a border sample, so availability must be checked explicitly
            // here rather than delegated to the `t`/`l` helpers.
            let top_avail = n.top[0..4].iter().any(|s| s.is_some());
            let left_avail = n.left.iter().any(|s| s.is_some());
            let dc = if top_avail && left_avail {
                let mut sum = 0i32;
                for i in 0..4 {
                    sum += t(i) + l(i);
                }
                (sum + 4) >> 3
            } else if top_avail {
                let mut sum = 0i32;
                for i in 0..4 {
                    sum += t(i);
                }
                (sum + 2) >> 2
            } else if left_avail {
                let mut sum = 0i32;
                for i in 0..4 {
                    sum += l(i);
                }
                (sum + 2) >> 2
            } else {
                R
            };
            for y in 0..4i32 {
                for x in 0..4i32 {
                    set(x, y, dc);
                }
            }
        }
        Intra4x4Mode::DiagonalDownLeft => {
            // Spec-exact (MultimediaWiki / Table 8-2). `t` supplies T0..T7.
            let (t0, t1, t2, t3, t4, t5, t6, t7) = (t(0), t(1), t(2), t(3), t(4), t(5), t(6), t(7));
            let a = (t0 + 2 * t1 + t2 + 2) / 4;
            let b = (t1 + 2 * t2 + t3 + 2) / 4;
            let c = (t2 + 2 * t3 + t4 + 2) / 4;
            let d = (t3 + 2 * t4 + t5 + 2) / 4;
            let e = (t4 + 2 * t5 + t6 + 2) / 4;
            let f = (t5 + 2 * t6 + t7 + 2) / 4;
            let g = (t6 + 3 * t7 + 2) / 4;
            // out[y][x]: (0,0)=a, (0,1)=b, ... diagonal sweep.
            set(0, 0, a);
            set(1, 0, b);
            set(2, 0, c);
            set(3, 0, d);
            set(0, 1, b);
            set(1, 1, c);
            set(2, 1, d);
            set(3, 1, e);
            set(0, 2, c);
            set(1, 2, d);
            set(2, 2, e);
            set(3, 2, f);
            set(0, 3, d);
            set(1, 3, e);
            set(2, 3, f);
            set(3, 3, g);
        }
        Intra4x4Mode::DiagonalDownRight => {
            // Spec §8.3.1.2.5 / ffmpeg `pred4x4_down_right_c`. Cross-checked
            // against libavcodec/h264pred_template.c.
            let (lt, t0, t1, t2, t3) = (tl, t(0), t(1), t(2), t(3));
            let (l0, l1, l2, l3) = (l(0), l(1), l(2), l(3));
            let a = (l3 + 2 * l2 + l1 + 2) / 4;
            let b = (l2 + 2 * l1 + l0 + 2) / 4;
            let c = (l1 + 2 * l0 + lt + 2) / 4;
            let d = (l0 + 2 * lt + t0 + 2) / 4; // main diagonal
            let e = (lt + 2 * t0 + t1 + 2) / 4;
            let f = (t0 + 2 * t1 + t2 + 2) / 4;
            let g = (t1 + 2 * t2 + t3 + 2) / 4;
            //   L0 | d  e  f  g
            //   L1 | c  d  e  f
            //   L2 | b  c  d  e
            //   L3 | a  b  c  d
            set(0, 0, d);
            set(1, 0, e);
            set(2, 0, f);
            set(3, 0, g);
            set(0, 1, c);
            set(1, 1, d);
            set(2, 1, e);
            set(3, 1, f);
            set(0, 2, b);
            set(1, 2, c);
            set(2, 2, d);
            set(3, 2, e);
            set(0, 3, a);
            set(1, 3, b);
            set(2, 3, c);
            set(3, 3, d);
        }
        Intra4x4Mode::VerticalRight => {
            // Spec-exact (MultimediaWiki / Table 8-2).
            let (lt, t0, t1, t2, t3) = (tl, t(0), t(1), t(2), t(3));
            let (l0, l1, l2) = (l(0), l(1), l(2));
            let a = (lt + t0 + 1) / 2;
            let b = (t0 + t1 + 1) / 2;
            let c = (t1 + t2 + 1) / 2;
            let d = (t2 + t3 + 1) / 2;
            let e = (l0 + 2 * lt + t0 + 2) / 4;
            let f = (lt + 2 * t0 + t1 + 2) / 4;
            let g = (t0 + 2 * t1 + t2 + 2) / 4;
            let h = (t1 + 2 * t2 + t3 + 2) / 4;
            let i = (lt + 2 * l0 + l1 + 2) / 4;
            let j = (l0 + 2 * l1 + l2 + 2) / 4;
            // Wiki layout rows:
            //   L0 | a b c d
            //   L1 | e f g h
            //   L2 | i a b c
            //       j e f g
            set(0, 0, a);
            set(1, 0, b);
            set(2, 0, c);
            set(3, 0, d);
            set(0, 1, e);
            set(1, 1, f);
            set(2, 1, g);
            set(3, 1, h);
            set(0, 2, i);
            set(1, 2, a);
            set(2, 2, b);
            set(3, 2, c);
            set(0, 3, j);
            set(1, 3, e);
            set(2, 3, f);
            set(3, 3, g);
        }
        Intra4x4Mode::HorizontalDown => {
            // Spec-exact (MultimediaWiki / Table 8-2).
            let (lt, t0, t1, t2) = (tl, t(0), t(1), t(2));
            let (l0, l1, l2, l3) = (l(0), l(1), l(2), l(3));
            let a = (lt + l0 + 1) / 2;
            let b = (l0 + 2 * lt + t0 + 2) / 4;
            let c = (lt + 2 * t0 + t1 + 2) / 4;
            let d = (t0 + 2 * t1 + t2 + 2) / 4;
            let e = (l0 + l1 + 1) / 2;
            let f = (lt + 2 * l0 + l1 + 2) / 4;
            let g = (l1 + l2 + 1) / 2;
            let h = (l0 + 2 * l1 + l2 + 2) / 4;
            let i = (l2 + l3 + 1) / 2;
            let j = (l1 + 2 * l2 + l3 + 2) / 4;
            // Wiki layout rows:
            //   L0 | a b c d
            //   L1 | e f a b
            //   L2 | g h e f
            //   L3 | i j g h
            set(0, 0, a);
            set(1, 0, b);
            set(2, 0, c);
            set(3, 0, d);
            set(0, 1, e);
            set(1, 1, f);
            set(2, 1, a);
            set(3, 1, b);
            set(0, 2, g);
            set(1, 2, h);
            set(2, 2, e);
            set(3, 2, f);
            set(0, 3, i);
            set(1, 3, j);
            set(2, 3, g);
            set(3, 3, h);
        }
        Intra4x4Mode::VerticalLeft => {
            // Spec-exact (MultimediaWiki / Table 8-2).
            let (t0, t1, t2, t3, t4, t5, t6) = (t(0), t(1), t(2), t(3), t(4), t(5), t(6));
            let a = (t0 + t1 + 1) / 2;
            let b = (t1 + t2 + 1) / 2;
            let c = (t2 + t3 + 1) / 2;
            let d = (t3 + t4 + 1) / 2;
            let e = (t4 + t5 + 1) / 2;
            let f = (t0 + 2 * t1 + t2 + 2) / 4;
            let g = (t1 + 2 * t2 + t3 + 2) / 4;
            let h = (t2 + 2 * t3 + t4 + 2) / 4;
            let i = (t3 + 2 * t4 + t5 + 2) / 4;
            let j = (t4 + 2 * t5 + t6 + 2) / 4;
            // Wiki layout rows:
            //   T0 | a b c d
            //   L1 | f g h i
            //   L2 | b c d e
            //   L3 | g h i j
            set(0, 0, a);
            set(1, 0, b);
            set(2, 0, c);
            set(3, 0, d);
            set(0, 1, f);
            set(1, 1, g);
            set(2, 1, h);
            set(3, 1, i);
            set(0, 2, b);
            set(1, 2, c);
            set(2, 2, d);
            set(3, 2, e);
            set(0, 3, g);
            set(1, 3, h);
            set(2, 3, i);
            set(3, 3, j);
        }
        Intra4x4Mode::HorizontalUp => {
            // Spec-exact (MultimediaWiki / Table 8-2).
            let (l0, l1, l2, l3) = (l(0), l(1), l(2), l(3));
            let a = (l0 + l1 + 1) / 2;
            let b = (l0 + 2 * l1 + l2 + 2) / 4;
            let c = (l1 + l2 + 1) / 2;
            let d = (l1 + 2 * l2 + l3 + 2) / 4;
            let e = (l2 + l3 + 1) / 2;
            let f = (l2 + 3 * l3 + 2) / 4;
            let g = l3;
            // Wiki layout rows:
            //   L0 | a b c d
            //   L1 | c d e f
            //   L2 | e f g g
            //   L3 | g g g g
            set(0, 0, a);
            set(1, 0, b);
            set(2, 0, c);
            set(3, 0, d);
            set(0, 1, c);
            set(1, 1, d);
            set(2, 1, e);
            set(3, 1, f);
            set(0, 2, e);
            set(1, 2, f);
            set(2, 2, g);
            set(3, 2, g);
            set(0, 3, g);
            set(1, 3, g);
            set(2, 3, g);
            set(3, 3, g);
        }
    }
}

/// Predict an 8×8 luma block (`out[64]`, row-major) for the given mode.
///
/// Faithful port of FFmpeg's `pred8x8l_*` functions (libavcodec/h264pred),
/// which implement the spec's Intra_8×8 prediction (§8.3.2.2). The neighbour
/// set is the full 8×8 one: `top[0..8]` are the samples directly above the
/// block, `top[8..16]` are the top-right extension (the samples to the upper
/// right, from the already-reconstructed MB/row to the right), `left[0..8]` are
/// the left samples, and `top_left` is the single above-left sample `X`.
///
/// Availability is resolved exactly as FFmpeg does: `p[-1,-1]` (top-left) is
/// available only when *both* the top and left rows are present; the
/// top-right samples are available only when the whole top-right extension row
/// is present. Each array entry is `None` when the corresponding sample is
/// outside the picture or not yet reconstructed, and is substituted with `R`
/// (128) by the loader below — except where FFmpeg's `has_topleft` /
/// `has_topright` flags dictate a different fallback (which the loader also
/// applies verbatim).
pub fn predict_8x8(
    mode: Intra4x4Mode,
    top: &[Option<u8>; 16],
    left: &[Option<u8>; 8],
    top_left: Option<u8>,
    out: &mut [u8; 64],
) {
    let tval = |x: usize| -> i32 { top.get(x).copied().flatten().map(|v| v as i32).unwrap_or(R) };
    let lval = |y: usize| -> i32 { left.get(y).copied().flatten().map(|v| v as i32).unwrap_or(R) };
    let tl = top_left.map(|v| v as i32).unwrap_or(R);
    let has_topleft = top[0].is_some() && left[0].is_some();
    let has_topright = top[8..16].iter().all(|s| s.is_some());

    // §8.3.2.2 neighbour loading (mirrors FFmpeg's PREDICT_8x8_LOAD_TOP /
    // LOAD_LEFT / LOAD_TOPRIGHT / LOAD_TOPLEFT).
    let t0 = ((if has_topleft { tl } else { tval(0) }) + 2 * tval(0) + tval(1) + 2) >> 2;
    let t1 = (tval(0) + 2 * tval(1) + tval(2) + 2) >> 2;
    let t2 = (tval(1) + 2 * tval(2) + tval(3) + 2) >> 2;
    let t3 = (tval(2) + 2 * tval(3) + tval(4) + 2) >> 2;
    let t4 = (tval(3) + 2 * tval(4) + tval(5) + 2) >> 2;
    let t5 = (tval(4) + 2 * tval(5) + tval(6) + 2) >> 2;
    let t6 = (tval(5) + 2 * tval(6) + tval(7) + 2) >> 2;
    let t7 = ((if has_topright { tval(8) } else { tval(7) }) + 2 * tval(7) + tval(6) + 2) >> 2;
    let (t8, t9, t10, t11, t12, t13, t14, t15) = if has_topright {
        (
            (tval(7) + 2 * tval(8) + tval(9) + 2) >> 2,
            (tval(8) + 2 * tval(9) + tval(10) + 2) >> 2,
            (tval(9) + 2 * tval(10) + tval(11) + 2) >> 2,
            (tval(10) + 2 * tval(11) + tval(12) + 2) >> 2,
            (tval(11) + 2 * tval(12) + tval(13) + 2) >> 2,
            (tval(12) + 2 * tval(13) + tval(14) + 2) >> 2,
            (tval(13) + 2 * tval(14) + tval(15) + 2) >> 2,
            (tval(14) + 3 * tval(15) + 2) >> 2,
        )
    } else {
        let v = tval(7);
        (v, v, v, v, v, v, v, v)
    };

    let l0 = ((if has_topleft { tl } else { lval(0) }) + 2 * lval(0) + lval(1) + 2) >> 2;
    let l1 = (lval(0) + 2 * lval(1) + lval(2) + 2) >> 2;
    let l2 = (lval(1) + 2 * lval(2) + lval(3) + 2) >> 2;
    let l3 = (lval(2) + 2 * lval(3) + lval(4) + 2) >> 2;
    let l4 = (lval(3) + 2 * lval(4) + lval(5) + 2) >> 2;
    let l5 = (lval(4) + 2 * lval(5) + lval(6) + 2) >> 2;
    let l6 = (lval(5) + 2 * lval(6) + lval(7) + 2) >> 2;
    let l7 = (lval(6) + 3 * lval(7) + 2) >> 2;

    // PREDICT_8x8_LOAD_TOPLEFT.
    let lt = (lval(0) + 2 * tl + tval(0) + 2) >> 2;

    let mut s = |pts: &[(i32, i32)], v: i32| {
        for &(x, y) in pts {
            out[(y as usize) * 8 + (x as usize)] = v.clamp(0, 255) as u8;
        }
    };

    match mode {
        Intra4x4Mode::Vertical => {
            for x in 0..8i32 {
                let v = tval(x as usize);
                for y in 0..8i32 {
                    s(&[(x, y)], v);
                }
            }
        }
        Intra4x4Mode::Horizontal => {
            for y in 0..8i32 {
                let v = lval(y as usize);
                for x in 0..8i32 {
                    s(&[(x, y)], v);
                }
            }
        }
        Intra4x4Mode::Dc => {
            // §8.3.2.2.3: like the 4×4/16×16 DC modes, the average is taken
            // over whichever of top/left is actually available — averaging
            // in the filtered `t`/`l` values unconditionally would silently
            // blend in the phantom `R` (128) substituted for a missing side
            // (see the 4×4 Dc arm above for the same issue), pulling the DC
            // value toward 128 instead of the real neighbour average.
            let top_avail = top[0..8].iter().any(|s| s.is_some());
            let left_avail = left.iter().any(|s| s.is_some());
            let dc = if top_avail && left_avail {
                (lval(0) + lval(1) + lval(2) + lval(3) + lval(4) + lval(5) + lval(6) + lval(7)
                    + tval(0) + tval(1) + tval(2) + tval(3) + tval(4) + tval(5) + tval(6) + tval(7)
                    + 8)
                    >> 4
            } else if top_avail {
                (tval(0) + tval(1) + tval(2) + tval(3) + tval(4) + tval(5) + tval(6) + tval(7) + 4) >> 3
            } else if left_avail {
                (lval(0) + lval(1) + lval(2) + lval(3) + lval(4) + lval(5) + lval(6) + lval(7) + 4) >> 3
            } else {
                R
            };
            for y in 0..8i32 {
                for x in 0..8i32 {
                    s(&[(x, y)], dc);
                }
            }
        }
        Intra4x4Mode::DiagonalDownLeft => {
            s(&[(0, 0)], (t0 + 2 * t1 + t2 + 2) >> 2);
            s(&[(0, 1), (1, 0)], (t1 + 2 * t2 + t3 + 2) >> 2);
            s(&[(0, 2), (1, 1), (2, 0)], (t2 + 2 * t3 + t4 + 2) >> 2);
            s(&[(0, 3), (1, 2), (2, 1), (3, 0)], (t3 + 2 * t4 + t5 + 2) >> 2);
            s(&[(0, 4), (1, 3), (2, 2), (3, 1), (4, 0)], (t4 + 2 * t5 + t6 + 2) >> 2);
            s(&[(0, 5), (1, 4), (2, 3), (3, 2), (4, 1), (5, 0)], (t5 + 2 * t6 + t7 + 2) >> 2);
            s(&[(0, 6), (1, 5), (2, 4), (3, 3), (4, 2), (5, 1), (6, 0)], (t6 + 2 * t7 + t8 + 2) >> 2);
            s(
                &[(0, 7), (1, 6), (2, 5), (3, 4), (4, 3), (5, 2), (6, 1), (7, 0)],
                (t7 + 2 * t8 + t9 + 2) >> 2,
            );
            s(&[(1, 7), (2, 6), (3, 5), (4, 4), (5, 3), (6, 2), (7, 1)], (t8 + 2 * t9 + t10 + 2) >> 2);
            s(&[(2, 7), (3, 6), (4, 5), (5, 4), (6, 3), (7, 2)], (t9 + 2 * t10 + t11 + 2) >> 2);
            s(&[(3, 7), (4, 6), (5, 5), (6, 4), (7, 3)], (t10 + 2 * t11 + t12 + 2) >> 2);
            s(&[(4, 7), (5, 6), (6, 5), (7, 4)], (t11 + 2 * t12 + t13 + 2) >> 2);
            s(&[(5, 7), (6, 6), (7, 5)], (t12 + 2 * t13 + t14 + 2) >> 2);
            s(&[(6, 7), (7, 6)], (t13 + 2 * t14 + t15 + 2) >> 2);
            s(&[(7, 7)], (t14 + 3 * t15 + 2) >> 2);
        }
        Intra4x4Mode::DiagonalDownRight => {
            s(&[(0, 7)], (l7 + 2 * l6 + l5 + 2) >> 2);
            s(&[(0, 6), (1, 7)], (l6 + 2 * l5 + l4 + 2) >> 2);
            s(&[(0, 5), (1, 6), (2, 7)], (l5 + 2 * l4 + l3 + 2) >> 2);
            s(&[(0, 4), (1, 5), (2, 6), (3, 7)], (l4 + 2 * l3 + l2 + 2) >> 2);
            s(&[(0, 3), (1, 4), (2, 5), (3, 6), (4, 7)], (l3 + 2 * l2 + l1 + 2) >> 2);
            s(&[(0, 2), (1, 3), (2, 4), (3, 5), (4, 6), (5, 7)], (l2 + 2 * l1 + l0 + 2) >> 2);
            s(
                &[(0, 1), (1, 2), (2, 3), (3, 4), (4, 5), (5, 6), (6, 7)],
                (l1 + 2 * l0 + lt + 2) >> 2,
            );
            s(
                &[(0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (5, 5), (6, 6), (7, 7)],
                (l0 + 2 * lt + t0 + 2) >> 2,
            );
            s(
                &[(1, 0), (2, 1), (3, 2), (4, 3), (5, 4), (6, 5), (7, 6)],
                (lt + 2 * t0 + t1 + 2) >> 2,
            );
            s(&[(2, 0), (3, 1), (4, 2), (5, 3), (6, 4), (7, 5)], (t0 + 2 * t1 + t2 + 2) >> 2);
            s(&[(3, 0), (4, 1), (5, 2), (6, 3), (7, 4)], (t1 + 2 * t2 + t3 + 2) >> 2);
            s(&[(4, 0), (5, 1), (6, 2), (7, 3)], (t2 + 2 * t3 + t4 + 2) >> 2);
            s(&[(5, 0), (6, 1), (7, 2)], (t3 + 2 * t4 + t5 + 2) >> 2);
            s(&[(6, 0), (7, 1)], (t4 + 2 * t5 + t6 + 2) >> 2);
            s(&[(7, 0)], (t5 + 2 * t6 + t7 + 2) >> 2);
        }
        Intra4x4Mode::VerticalRight => {
            s(&[(0, 6)], (l5 + 2 * l4 + l3 + 2) >> 2);
            s(&[(0, 7)], (l6 + 2 * l5 + l4 + 2) >> 2);
            s(&[(0, 4), (1, 6)], (l3 + 2 * l2 + l1 + 2) >> 2);
            s(&[(0, 2), (1, 4), (2, 6)], (l1 + 2 * l0 + lt + 2) >> 2);
            s(&[(0, 3), (1, 5), (2, 7)], (l2 + 2 * l1 + l0 + 2) >> 2);
            s(&[(0, 1), (1, 3), (2, 5), (3, 7)], (l0 + 2 * lt + t0 + 2) >> 2);
            s(&[(0, 0), (1, 2), (2, 4), (3, 6)], (lt + t0 + 1) >> 1);
            s(&[(1, 1), (2, 3), (3, 5), (4, 7)], (lt + 2 * t0 + t1 + 2) >> 2);
            s(&[(1, 0), (2, 2), (3, 4), (4, 6)], (t0 + t1 + 1) >> 1);
            s(&[(2, 1), (3, 3), (4, 5), (5, 7)], (t0 + 2 * t1 + t2 + 2) >> 2);
            s(&[(2, 0), (3, 2), (4, 4), (5, 6)], (t1 + t2 + 1) >> 1);
            s(&[(3, 1), (4, 3), (5, 5), (6, 7)], (t1 + 2 * t2 + t3 + 2) >> 2);
            s(&[(3, 0), (4, 2), (5, 4), (6, 6)], (t2 + t3 + 1) >> 1);
            s(&[(4, 1), (5, 3), (6, 5), (7, 7)], (t2 + 2 * t3 + t4 + 2) >> 2);
            s(&[(4, 0), (5, 2), (6, 4), (7, 6)], (t3 + t4 + 1) >> 1);
            s(&[(5, 1), (6, 3), (7, 5)], (t3 + 2 * t4 + t5 + 2) >> 2);
            s(&[(5, 0), (6, 2), (7, 4)], (t4 + t5 + 1) >> 1);
            s(&[(6, 1), (7, 3)], (t4 + 2 * t5 + t6 + 2) >> 2);
            s(&[(6, 0), (7, 2)], (t5 + t6 + 1) >> 1);
            s(&[(7, 1)], (t5 + 2 * t6 + t7 + 2) >> 2);
            s(&[(7, 0)], (t6 + t7 + 1) >> 1);
        }
        Intra4x4Mode::HorizontalDown => {
            s(&[(0, 7)], (l6 + l7 + 1) >> 1);
            s(&[(1, 7)], (l5 + 2 * l6 + l7 + 2) >> 2);
            s(&[(0, 6), (2, 7)], (l5 + l6 + 1) >> 1);
            s(&[(1, 6), (3, 7)], (l4 + 2 * l5 + l6 + 2) >> 2);
            s(&[(0, 5), (2, 6), (4, 7)], (l4 + l5 + 1) >> 1);
            s(&[(1, 5), (3, 6), (5, 7)], (l3 + 2 * l4 + l5 + 2) >> 2);
            s(&[(0, 4), (2, 5), (4, 6), (6, 7)], (l3 + l4 + 1) >> 1);
            s(&[(1, 4), (3, 5), (5, 6), (7, 7)], (l2 + 2 * l3 + l4 + 2) >> 2);
            s(&[(0, 3), (2, 4), (4, 5), (6, 6)], (l2 + l3 + 1) >> 1);
            s(&[(1, 3), (3, 4), (5, 5), (7, 6)], (l1 + 2 * l2 + l3 + 2) >> 2);
            s(&[(0, 2), (2, 3), (4, 4), (6, 5)], (l1 + l2 + 1) >> 1);
            s(&[(1, 2), (3, 3), (5, 4), (7, 5)], (l0 + 2 * l1 + l2 + 2) >> 2);
            s(&[(0, 1), (2, 2), (4, 3), (6, 4)], (l0 + l1 + 1) >> 1);
            s(&[(1, 1), (3, 2), (5, 3), (7, 4)], (lt + 2 * l0 + l1 + 2) >> 2);
            s(&[(0, 0), (2, 1), (4, 2), (6, 3)], (lt + l0 + 1) >> 1);
            s(&[(1, 0), (3, 1), (5, 2), (7, 3)], (l0 + 2 * lt + t0 + 2) >> 2);
            s(&[(2, 0), (4, 1), (6, 2)], (t1 + 2 * t0 + lt + 2) >> 2);
            s(&[(3, 0), (5, 1), (7, 2)], (t2 + 2 * t1 + t0 + 2) >> 2);
            s(&[(4, 0), (6, 1)], (t3 + 2 * t2 + t1 + 2) >> 2);
            s(&[(5, 0), (7, 1)], (t4 + 2 * t3 + t2 + 2) >> 2);
            s(&[(6, 0)], (t5 + 2 * t4 + t3 + 2) >> 2);
            s(&[(7, 0)], (t6 + 2 * t5 + t4 + 2) >> 2);
        }
        Intra4x4Mode::VerticalLeft => {
            s(&[(0, 0)], (t0 + t1 + 1) >> 1);
            s(&[(0, 1)], (t0 + 2 * t1 + t2 + 2) >> 2);
            s(&[(0, 2), (1, 0)], (t1 + t2 + 1) >> 1);
            s(&[(0, 3), (1, 1)], (t1 + 2 * t2 + t3 + 2) >> 2);
            s(&[(0, 4), (1, 2), (2, 0)], (t2 + t3 + 1) >> 1);
            s(&[(0, 5), (1, 3), (2, 1)], (t2 + 2 * t3 + t4 + 2) >> 2);
            s(&[(0, 6), (1, 4), (2, 2), (3, 0)], (t3 + t4 + 1) >> 1);
            s(&[(0, 7), (1, 5), (2, 3), (3, 1)], (t3 + 2 * t4 + t5 + 2) >> 2);
            s(&[(1, 6), (2, 4), (3, 2), (4, 0)], (t4 + t5 + 1) >> 1);
            s(&[(1, 7), (2, 5), (3, 3), (4, 1)], (t4 + 2 * t5 + t6 + 2) >> 2);
            s(&[(2, 6), (3, 4), (4, 2), (5, 0)], (t5 + t6 + 1) >> 1);
            s(&[(2, 7), (3, 5), (4, 3), (5, 1)], (t5 + 2 * t6 + t7 + 2) >> 2);
            s(&[(3, 6), (4, 4), (5, 2), (6, 0)], (t6 + t7 + 1) >> 1);
            s(&[(3, 7), (4, 5), (5, 3), (6, 1)], (t6 + 2 * t7 + t8 + 2) >> 2);
            s(&[(4, 6), (5, 4), (6, 2), (7, 0)], (t7 + t8 + 1) >> 1);
            s(&[(4, 7), (5, 5), (6, 3), (7, 1)], (t7 + 2 * t8 + t9 + 2) >> 2);
            s(&[(5, 6), (6, 4), (7, 2)], (t8 + t9 + 1) >> 1);
            s(&[(5, 7), (6, 5), (7, 3)], (t8 + 2 * t9 + t10 + 2) >> 2);
            s(&[(6, 6), (7, 4)], (t9 + t10 + 1) >> 1);
            s(&[(6, 7), (7, 5)], (t9 + 2 * t10 + t11 + 2) >> 2);
            s(&[(7, 6)], (t10 + t11 + 1) >> 1);
            s(&[(7, 7)], (t10 + 2 * t11 + t12 + 2) >> 2);
        }
        Intra4x4Mode::HorizontalUp => {
            s(&[(0, 0)], (l0 + l1 + 1) >> 1);
            s(&[(1, 0)], (l0 + 2 * l1 + l2 + 2) >> 2);
            s(&[(0, 1), (2, 0)], (l1 + l2 + 1) >> 1);
            s(&[(1, 1), (3, 0)], (l1 + 2 * l2 + l3 + 2) >> 2);
            s(&[(0, 2), (2, 1), (4, 0)], (l2 + l3 + 1) >> 1);
            s(&[(1, 2), (3, 1), (5, 0)], (l2 + 2 * l3 + l4 + 2) >> 2);
            s(&[(0, 3), (2, 2), (4, 1), (6, 0)], (l3 + l4 + 1) >> 1);
            s(&[(1, 3), (3, 2), (5, 1), (7, 0)], (l3 + 2 * l4 + l5 + 2) >> 2);
            s(&[(0, 4), (2, 3), (4, 2), (6, 1)], (l4 + l5 + 1) >> 1);
            s(&[(1, 4), (3, 3), (5, 2), (7, 1)], (l4 + 2 * l5 + l6 + 2) >> 2);
            s(&[(0, 5), (2, 4), (4, 3), (6, 2)], (l5 + l6 + 1) >> 1);
            s(&[(1, 5), (3, 4), (5, 3), (7, 2)], (l5 + 2 * l6 + l7 + 2) >> 2);
            s(&[(0, 6), (2, 5), (4, 4), (6, 3)], (l6 + l7 + 1) >> 1);
            s(&[(1, 6), (3, 5), (5, 4), (7, 3)], (l6 + 3 * l7 + 2) >> 2);
            s(
                &[
                    (0, 7), (1, 7), (2, 6), (2, 7), (3, 6), (3, 7), (4, 5), (4, 6), (4, 7), (5, 5),
                    (5, 6), (5, 7), (6, 4), (6, 5), (6, 6), (6, 7), (7, 4), (7, 5), (7, 6), (7, 7),
                ],
                l7,
            );
        }
    }
}
/// Predict a 16×16 luma macroblock (`out[256]`) for the given [`Intra16x16Mode`].
///
/// `top`/`left` are the 16 samples above/left of the macroblock plus the single
/// `X` above-left sample. Plane mode uses the spec's linear interpolation
/// (`spec §8.3.3.1.2`).
pub fn predict_16x16(mode: Intra16x16Mode, n: &IntraNeighbours16x16, out: &mut [u8; 256]) {
    let t = |i: i32| sample(n.top[i as usize]);
    let l = |i: i32| sample(n.left[i as usize]);
    let tl = sample(n.top_left);
    let mut set = |x: i32, y: i32, v: i32| {
        out[(y as usize) * 16 + (x as usize)] = v.clamp(0, 255) as u8;
    };

    match mode {
        Intra16x16Mode::Vertical => {
            for y in 0..16i32 {
                for x in 0..16i32 {
                    set(x, y, t(x));
                }
            }
        }
        Intra16x16Mode::Horizontal => {
            for y in 0..16i32 {
                for x in 0..16i32 {
                    set(x, y, l(y));
                }
            }
        }
        Intra16x16Mode::Dc => {
            let top_avail = n.top.iter().any(|s| s.is_some());
            let left_avail = n.left.iter().any(|s| s.is_some());
            let dc = if top_avail && left_avail {
                let mut s = 0i32;
                for i in 0..16 {
                    s += t(i) + l(i);
                }
                (s + 16) / 32
            } else if top_avail {
                let mut s = 0i32;
                for i in 0..16 {
                    s += t(i);
                }
                (s + 8) / 16
            } else if left_avail {
                let mut s = 0i32;
                for i in 0..16 {
                    s += l(i);
                }
                (s + 8) / 16
            } else {
                R
            };
            for y in 0..16i32 {
                for x in 0..16i32 {
                    set(x, y, dc);
                }
            }
        }
        Intra16x16Mode::Plane => {
            // §8.3.3.4: H = Σ_{x'=0}^{7} (x'+1)*(p[8+x',-1] - p[6-x',-1]),
            // where p[6-x',-1] for x'=7 is p[-1,-1] = `tl` — handled as a
            // separate 8th term since `t`/`l` can't index -1 directly.
            let mut h = 0i32;
            for xp in 0..7i32 {
                h += (xp + 1) * (t(8 + xp) - t(6 - xp));
            }
            h += 8 * (t(15) - tl);
            let mut v = 0i32;
            for yp in 0..7i32 {
                v += (yp + 1) * (l(8 + yp) - l(6 - yp));
            }
            v += 8 * (l(15) - tl);
            // a = 16 * (p[-1,15] + p[15,-1])
            let a = 16 * (l(15) + t(15));
            let b = (5 * h + 32) >> 6;
            let c = (5 * v + 32) >> 6;
            for y in 0..16i32 {
                for x in 0..16i32 {
                    let val = a + b * (x - 7) + c * (y - 7);
                    set(x, y, (val + 16) >> 5);
                }
            }
        }
    }
}

/// Predict an 8×8 chroma block (`out[64]`) for the given [`IntraChromaMode`].
///
/// Chroma 4×4 blocks share the same 8×8 prediction geometry (`spec §8.3.4`).
/// `top`/`left` are the 8 samples above/left of the 8×8 chroma block, `tl` the
/// single diagonal sample.
pub fn predict_chroma(
    mode: IntraChromaMode,
    top: &[Option<u8>; 8],
    left: &[Option<u8>; 8],
    tl: Option<u8>,
    out: &mut [u8; 64],
) {
    let t = |i: i32| sample(top[i as usize]);
    let l = |i: i32| sample(left[i as usize]);
    let tl = sample(tl);
    let mut set = |x: i32, y: i32, v: i32| {
        out[(y as usize) * 8 + (x as usize)] = v.clamp(0, 255) as u8;
    };

    match mode {
        IntraChromaMode::Vertical => {
            for y in 0..8i32 {
                for x in 0..8i32 {
                    set(x, y, t(x));
                }
            }
        }
        IntraChromaMode::Horizontal => {
            for y in 0..8i32 {
                for x in 0..8i32 {
                    set(x, y, l(y));
                }
            }
        }
        IntraChromaMode::Dc => {
            // §8.3.4.1: each of the four 4×4 quadrants gets its own DC value
            // (not one flat average for the whole 8×8). When both sides are
            // available, the top-left and bottom-right quadrants average
            // *both* their top and left samples, but the top-right quadrant
            // uses *only* its top samples and the bottom-left quadrant uses
            // *only* its left samples — an asymmetric rule, not a simplified
            // whole-block DC. When only one side is available (or neither),
            // every quadrant falls back to that side's samples (or `R`).
            let top_avail = top.iter().any(|s| s.is_some());
            let left_avail = left.iter().any(|s| s.is_some());

            // avg of `vals` (1..=4 samples), rounded per §8.3.4.1.
            let avg = |vals: &[i32]| -> i32 {
                let n = vals.len() as i32;
                let sum: i32 = vals.iter().sum();
                (sum + n / 2) / n
            };

            let top_l = [t(0), t(1), t(2), t(3)];
            let top_r = [t(4), t(5), t(6), t(7)];
            let left_t = [l(0), l(1), l(2), l(3)];
            let left_b = [l(4), l(5), l(6), l(7)];

            let (dc0, dc1, dc2, dc3) = if top_avail && left_avail {
                let both_tl: Vec<i32> = top_l.iter().chain(left_t.iter()).copied().collect();
                let both_br: Vec<i32> = top_r.iter().chain(left_b.iter()).copied().collect();
                (avg(&both_tl), avg(&top_r), avg(&left_b), avg(&both_br))
            } else if top_avail {
                (avg(&top_l), avg(&top_r), avg(&top_l), avg(&top_r))
            } else if left_avail {
                (avg(&left_t), avg(&left_t), avg(&left_b), avg(&left_b))
            } else {
                (R, R, R, R)
            };

            for y in 0..8i32 {
                for x in 0..8i32 {
                    let dc = match (x < 4, y < 4) {
                        (true, true) => dc0,
                        (false, true) => dc1,
                        (true, false) => dc2,
                        (false, false) => dc3,
                    };
                    set(x, y, dc);
                }
            }
        }
        IntraChromaMode::Plane => {
            // §8.3.4.4: H = Σ_{x'=0}^{3} (x'+1)*(p[4+x',-1] - p[2-x',-1]),
            // where p[2-x',-1] for x'=3 is p[-1,-1] = `tl` — handled as a
            // separate 4th term since `t`/`l` can't index -1 directly.
            let mut h = 0i32;
            for xp in 0..3i32 {
                h += (xp + 1) * (t(4 + xp) - t(2 - xp));
            }
            h += 4 * (t(7) - tl);
            let mut v = 0i32;
            for yp in 0..3i32 {
                v += (yp + 1) * (l(4 + yp) - l(2 - yp));
            }
            v += 4 * (l(7) - tl);
            let a = 16 * (l(7) + t(7));
            let b = (17 * h + 16) >> 5;
            let c = (17 * v + 16) >> 5;
            for y in 0..8i32 {
                for x in 0..8i32 {
                    let val = a + b * (x - 3) + c * (y - 3);
                    set(x, y, (val + 16) >> 5);
                }
            }
        }
    }
}

/// Map the Intra_8×8 / 16×16 decode block type into the prediction machinery.
///
/// Returns the deterministic intra mode for each 8×8 (used by `Intra_8×8`) or
/// the 16×16 mode (used by `Intra_16×16`). Inter/`Skip` blocks have no intra
/// prediction and fall back to DC-fill handled by the caller.
pub fn mb_is_intra(mb_type: MbType) -> bool {
    matches!(
        mb_type,
        MbType::Intra4x4 | MbType::Intra16x16 { .. } | MbType::PL016x16 | MbType::BDirect16x16
    ) || matches!(mb_type, MbType::Intra16x16 { .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n4() -> IntraNeighbours4x4 {
        IntraNeighbours4x4 {
            top: [Some(100); 8],
            left: [Some(50); 4],
            top_left: Some(128),
        }
    }

    #[test]
    fn mode_vertical_copies_top_row() {
        let n = n4();
        let mut out = [0u8; 16];
        predict_4x4(Intra4x4Mode::Vertical, &n, &mut out);
        for y in 0..4i32 {
            for x in 0..4i32 {
                assert_eq!(out[(y as usize) * 4 + (x as usize)], 100);
            }
        }
    }

    #[test]
    fn mode_horizontal_copies_left_col() {
        let n = n4();
        let mut out = [0u8; 16];
        predict_4x4(Intra4x4Mode::Horizontal, &n, &mut out);
        for y in 0..4i32 {
            for x in 0..4i32 {
                assert_eq!(out[(y as usize) * 4 + (x as usize)], 50);
            }
        }
    }

    #[test]
    fn mode_dc_is_average_of_borders() {
        let n = n4();
        let mut out = [0u8; 16];
        predict_4x4(Intra4x4Mode::Dc, &n, &mut out);
        // (4*100 + 4*50)/8 = (400+200)/8 = 75
        for v in out {
            assert_eq!(v, 75);
        }
    }

    #[test]
    fn mode_dc_unavailable_borders_use_r() {
        let n = IntraNeighbours4x4 {
            top: [None; 8],
            left: [None; 4],
            top_left: None,
        };
        let mut out = [0u8; 16];
        predict_4x4(Intra4x4Mode::Dc, &n, &mut out);
        for v in out {
            assert_eq!(v, R as u8);
        }
    }

    #[test]
    fn predict_8x8_vertical() {
        let top = [Some(200u8); 16];
        let left = [Some(10u8); 8];
        let mut out = [0u8; 64];
        // top_left must match top neighbours for pure vertical prediction
        predict_8x8(Intra4x4Mode::Vertical, &top, &left, Some(200), &mut out);
        for v in out {
            assert_eq!(v, 200);
        }
    }

    #[test]
    fn predict_16x16_plane_is_linear() {
        // Constant gradient → plane should follow a*.. formula; just ensure no
        // out-of-range writes and deterministic output.
        let n = IntraNeighbours16x16 {
            top: [Some(0u8); 16],
            left: [Some(0u8); 16],
            top_left: Some(0),
        };
        let mut out = [0u8; 256];
        predict_16x16(Intra16x16Mode::Plane, &n, &mut out);
        // With all zero borders the plane prediction is 0 everywhere.
        for v in out {
            assert_eq!(v, 0);
        }
    }

    #[test]
    fn predict_chroma_dc() {
        // §8.3.4.1: top-left/bottom-right quadrants blend both sides;
        // top-right uses only top, bottom-left uses only left.
        let top = [Some(120u8); 8];
        let left = [Some(80u8); 8];
        let mut out = [0u8; 64];
        predict_chroma(IntraChromaMode::Dc, &top, &left, Some(128), &mut out);
        let dc_tl = (4 * 120 + 4 * 80 + 4) / 8; // 100
        let dc_tr = (4 * 120 + 2) / 4; // 120
        let dc_bl = (4 * 80 + 2) / 4; // 80
        let dc_br = dc_tl; // 100
        for y in 0..8usize {
            for x in 0..8usize {
                let expected = match (x < 4, y < 4) {
                    (true, true) => dc_tl,
                    (false, true) => dc_tr,
                    (true, false) => dc_bl,
                    (false, false) => dc_br,
                };
                assert_eq!(out[y * 8 + x], expected as u8, "at ({x},{y})");
            }
        }
    }
}
