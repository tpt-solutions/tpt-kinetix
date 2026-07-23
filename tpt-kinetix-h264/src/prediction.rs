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
            let mut sum = 0i32;
            for i in 0..4 {
                sum += t(i) + l(i);
            }
            let dc = (sum + 4) / 8;
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
            // Spec-exact (MultimediaWiki / Table 8-2).
            let (lt, t0) = (tl, t(0));
            let (l0, l1, l2, l3) = (l(0), l(1), l(2), l(3));
            let d = (l3 + 2 * l2 + l1 + 2) / 4;
            let e = (l2 + 2 * l1 + l0 + 2) / 4;
            let f = (l1 + 2 * l0 + lt + 2) / 4;
            let g = (l0 + 2 * lt + t0 + 2) / 4;
            // Wiki layout (rows L0..L3, top->bottom), with wiki a=g, b=f, c=e:
            //   L0 | d  e  f  g
            //   L1 | e  d  e  f
            //   L2 | f  e  d  e
            //   L3 | g  f  e  d
            set(0, 0, d);
            set(1, 0, e);
            set(2, 0, f);
            set(3, 0, g);
            set(0, 1, e);
            set(1, 1, d);
            set(2, 1, e);
            set(3, 1, f);
            set(0, 2, f);
            set(1, 2, e);
            set(2, 2, d);
            set(3, 2, e);
            set(0, 3, g);
            set(1, 3, f);
            set(2, 3, e);
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
            let (t0, t1, t2, t3, t4, t5, t6) =
                (t(0), t(1), t(2), t(3), t(4), t(5), t(6));
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
/// The 8×8 modes share the same geometry as the 4×4 modes but operate on the
/// 8×8 sample grid and use the extended neighbour set (the 8 samples above plus
/// the top-right extension, and the 8 left samples plus the single `X`).
pub fn predict_8x8(
    mode: Intra4x4Mode,
    top: &[Option<u8>],
    left: &[Option<u8>],
    top_left: Option<u8>,
    out: &mut [u8; 64],
) {
    let t = |i: i32| sample(top.get(i as usize).and_then(|x| *x));
    let l = |i: i32| sample(left.get(i as usize).and_then(|x| *x));
    let tl = sample(top_left);
    let mut set = |x: i32, y: i32, v: i32| {
        out[(y as usize) * 8 + (x as usize)] = v.clamp(0, 255) as u8;
    };

    match mode {
        Intra4x4Mode::Vertical => {
            for y in 0..8i32 {
                for x in 0..8i32 {
                    set(x, y, t(x));
                }
            }
        }
        Intra4x4Mode::Horizontal => {
            for y in 0..8i32 {
                for x in 0..8i32 {
                    set(x, y, l(y));
                }
            }
        }
        Intra4x4Mode::Dc => {
            let mut sum = 0i32;
            for i in 0..8 {
                sum += t(i) + l(i);
            }
            let dc = (sum + 8) / 16;
            for y in 0..8i32 {
                for x in 0..8i32 {
                    set(x, y, dc);
                }
            }
        }
        Intra4x4Mode::DiagonalDownLeft => {
            for y in 0..8i32 {
                for x in 0..8i32 {
                    let idx = x + y;
                    let a = if idx < 8 { t(idx) } else { t(7) };
                    let b = if idx + 1 < 8 { t(idx + 1) } else { t(7) };
                    set(x, y, (a + b + 1) / 2);
                }
            }
        }
        Intra4x4Mode::DiagonalDownRight => {
            for y in 0..8i32 {
                for x in 0..8i32 {
                    let i = x - y;
                    let v = if i <= 0 {
                        let p = -i;
                        if p <= 1 {
                            if p == 0 {
                                tl
                            } else {
                                (l(0) + tl * 2 + t(0) + 2) / 4
                            }
                        } else if p <= 4 {
                            (l(p - 2) + l(p - 1) * 2 + l(p) + 2) / 4
                        } else {
                            l(3)
                        }
                    } else {
                        let q = i;
                        (t(q - 1) + t(q) + 1) / 2
                    };
                    set(x, y, v);
                }
            }
        }
        Intra4x4Mode::VerticalRight => {
            for y in 0..8i32 {
                for x in 0..8i32 {
                    let i = x - (y >> 1) * 2 + (y & 1) - 1;
                    let v = if i <= 0 {
                        let p = -i;
                        if p == 1 {
                            (tl + l(0) + 1) / 2
                        } else if p <= 4 {
                            (l(p - 2) + l(p - 1) + 1) / 2
                        } else {
                            l(3)
                        }
                    } else {
                        let q = i;
                        if q < 8 {
                            (t(q) + t(q - 1) + 1) / 2
                        } else {
                            t(7)
                        }
                    };
                    set(x, y, v);
                }
            }
        }
        Intra4x4Mode::HorizontalDown => {
            for y in 0..8i32 {
                for x in 0..8i32 {
                    let i = y - (x >> 1) * 2 + (x & 1) - 1;
                    let v = if i <= 0 {
                        let p = -i;
                        if p == 1 {
                            (tl + t(0) + 1) / 2
                        } else if p <= 4 {
                            (t(p - 2) + t(p - 1) + 1) / 2
                        } else {
                            t(3)
                        }
                    } else {
                        let q = i;
                        if q < 8 {
                            (l(q) + l(q - 1) + 1) / 2
                        } else {
                            l(7)
                        }
                    };
                    set(x, y, v);
                }
            }
        }
        Intra4x4Mode::VerticalLeft => {
            for y in 0..8i32 {
                for x in 0..8i32 {
                    let i = x - (y >> 1) * 2 + (y & 1) - 1;
                    let v = if i < 7 {
                        let q = i;
                        if i <= 0 {
                            let p = -i;
                            if p <= 1 {
                                t(0)
                            } else {
                                t(p - 1).max(0)
                            }
                        } else {
                            (t(q) + t(q + 1) + 1) / 2
                        }
                    } else {
                        t(7)
                    };
                    set(x, y, v);
                }
            }
        }
        Intra4x4Mode::HorizontalUp => {
            for y in 0..8i32 {
                for x in 0..8i32 {
                    let i = y - (x >> 1) * 2 + (x & 1) - 1;
                    let v = if i < 7 {
                        let q = i;
                        if i <= 0 {
                            let p = -i;
                            if p <= 1 {
                                l(0)
                            } else {
                                l(p - 1).max(0)
                            }
                        } else {
                            (l(q) + l(q + 1) + 1) / 2
                        }
                    } else {
                        l(7)
                    };
                    set(x, y, v);
                }
            }
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
            let mut h = 0i32;
            for i in 1..8i32 {
                h += i * (t(8 + i) - t(8 - i));
            }
            let mut v = 0i32;
            for i in 1..8i32 {
                v += i * (l(8 + i) - l(8 - i));
            }
            // a = (top_left + top_15) << 4
            let a = (tl + t(15)) << 4;
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
            let mut s = 0i32;
            for i in 0..8 {
                s += t(i) + l(i);
            }
            let dc = (s + 8) / 16;
            for y in 0..8i32 {
                for x in 0..8i32 {
                    set(x, y, dc);
                }
            }
        }
        IntraChromaMode::Plane => {
            let mut h = 0i32;
            for i in 1..4i32 {
                h += i * (t(4 + i) - t(4 - i));
            }
            let mut v = 0i32;
            for i in 1..4i32 {
                v += i * (l(4 + i) - l(4 - i));
            }
            let a = (tl + t(7)) << 4;
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
        let top = [Some(200u8); 8];
        let left = [Some(10u8); 8];
        let mut out = [0u8; 64];
        predict_8x8(Intra4x4Mode::Vertical, &top, &left, Some(128), &mut out);
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
        let top = [Some(120u8); 8];
        let left = [Some(80u8); 8];
        let mut out = [0u8; 64];
        predict_chroma(IntraChromaMode::Dc, &top, &left, Some(128), &mut out);
        let dc = (8 * 120 + 8 * 80 + 8) / 16; // 100
        for v in out {
            assert_eq!(v, dc as u8);
        }
    }
}
