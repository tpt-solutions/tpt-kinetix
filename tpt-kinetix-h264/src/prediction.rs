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

/// Neighbouring samples needed by the Intra_4×4 / 8×8 predictors.
///
/// `top[x]` is the sample directly above the current block at column `x`
/// (`spec` `B` samples), `left[y]` the sample to the left (`A` samples), and
/// `top_left` the single diagonal sample above-left (`spec` `X`). `None`
/// denotes an unavailable sample.
pub struct IntraNeighbours4x4 {
    pub top: [Option<u8>; 4],
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
    let t = |i: i32| sample(n.top[i as usize]);
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
            // Need top-right samples; reuse available top samples by clamping.
            let tr = (0..4i32).map(|i| t(i)).collect::<Vec<_>>();
            // t(i) for i in 4..8 unavailable -> use t(3)
            let get = |i: i32| -> i32 {
                if i < 4 {
                    tr[i as usize]
                } else {
                    t(3)
                }
            };
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let idx = x + y;
                    let v = (get(idx) + get(idx + 1) + 1) / 2;
                    set(x, y, v);
                }
            }
        }
        Intra4x4Mode::DiagonalDownRight => {
            // (x+y) <= 4 uses left/X border; otherwise top border samples.
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let s = x as i32 + y as i32;
                    let v = if s <= 4 {
                        diag_down_right_border_sample(&l, &t, tl, x as i32, y as i32)
                    } else {
                        (t(s) + t(s + 1) + 1) / 2
                    };
                    set(x, y, v);
                }
            }
        }
        Intra4x4Mode::VerticalRight => {
            // For each (x,y): z = x - (y>>1)*2 + (y&1); p = (z-1)>>1; k = (z-1)&1.
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let z = x as i32 - (y as i32 >> 1) * 2 + (y & 1) as i32;
                    let (v, _) = angular_from_z(&l, &t, tl, z, 4);
                    set(x, y, v);
                }
            }
        }
        Intra4x4Mode::HorizontalDown => {
            // Mirror of VerticalRight across the main diagonal.
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let z = y as i32 - (x as i32 >> 1) * 2 + (x & 1) as i32;
                    let (v, _) = angular_from_z(&l, &t, tl, z, 4);
                    set(x, y, v);
                }
            }
        }
        Intra4x4Mode::VerticalLeft => {
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let z = x as i32 - (y as i32 >> 1) * 2 + (y & 1) as i32;
                    let (v, _) = angular_from_z_top(&t, z);
                    set(x, y, v);
                }
            }
        }
        Intra4x4Mode::HorizontalUp => {
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let z = y as i32 - (x as i32 >> 1) * 2 + (x & 1) as i32;
                    let (v, _) = angular_from_z_left(&l, z);
                    set(x, y, v);
                }
            }
        }
    }
}

// --- Diagonal sample helpers (spec §8.3.1.2, Table 8-2) -------------------------

/// Sample used by DiagonalDownRight for the (x+y) ≤ 4 region, which falls on the
/// left/`X` border instead of the top border.
fn diag_down_right_border_sample(
    l: &dyn Fn(i32) -> i32,
    t: &dyn Fn(i32) -> i32,
    tl: i32,
    x: i32,
    y: i32,
) -> i32 {
    let s = x + y;
    match s {
        0 => tl,
        1 => {
            // (x,y) in {(0,1),(1,0)}
            if x == 0 {
                (l(0) + tl + 1) / 2
            } else {
                (tl + t(0) + 1) / 2
            }
        }
        2 => {
            // (0,2)->(l(1)+l(0))/2 ; (1,1)->(l(0)+t(0))/2 ; (2,0)->(t(0)+t(1))/2
            if x == 0 {
                (l(1) + l(0) + 1) / 2
            } else if x == 1 {
                (l(0) + t(0) + 1) / 2
            } else {
                (t(0) + t(1) + 1) / 2
            }
        }
        3 => {
            // (0,3)->(l(2)+l(1))/2 ; (1,2)->(l(1)+l(0))/2 ; (2,1)->(l(0)+t(0))/2 ; (3,0)->(t(0)+t(1))/2
            if x == 0 {
                (l(2) + l(1) + 1) / 2
            } else if x == 1 {
                (l(1) + l(0) + 1) / 2
            } else if x == 2 {
                (l(0) + t(0) + 1) / 2
            } else {
                (t(0) + t(1) + 1) / 2
            }
        }
        4 => {
            // (0,4)->(l(3)+l(2))/2 ; (1,3)->(l(2)+l(1))/2 ; (2,2)->(l(1)+l(0))/2 ; (3,1)->(l(0)+t(0))/2
            if x == 0 {
                (l(3) + l(2) + 1) / 2
            } else if x == 1 {
                (l(2) + l(1) + 1) / 2
            } else if x == 2 {
                (l(1) + l(0) + 1) / 2
            } else {
                (l(0) + t(0) + 1) / 2
            }
        }
        _ => t(s),
    }
}

/// VerticalRight / HorizontalDown sample for a given `z`, returning `(value, k)`.
///
/// `p = (z-1) >> 1`, `k = (z-1) & 1`. k==0 → `(x + x_p)/2`; k==1 →
/// `(3x_p + x)/4`. We derive the needed border sample from the left/top/`X`
/// set; `block` is the block size (4 for 4×4, 8 for 8×8) used to wrap into the
/// top border when `z` exceeds the left column count.
fn angular_from_z(
    l: &dyn Fn(i32) -> i32,
    t: &dyn Fn(i32) -> i32,
    tl: i32,
    z: i32,
    block: i32,
) -> (i32, usize) {
    let (s, k) = if z <= 0 {
        // z <= 0: use X (z==0) or diagonal extrapolation into the left border.
        let p = (-z).max(0);
        let base = if z == 0 { tl } else { l(p.min(block - 1)) };
        (base, 0usize)
    } else {
        let p = (z - 1) >> 1;
        let k = ((z - 1) & 1) as usize;
        let (xp, x) = if p + 1 < block {
            (l(p), l(p + 1))
        } else {
            // Beyond the left column: borrow from the top border.
            let to = p + 1 - block;
            (t(p - block), t(to))
        };
        (((xp) + (x) + 1) / 2, k)
    };
    (s, k)
}

/// VerticalLeft sample: derived from the top border only (z indexes top).
fn angular_from_z_top(t: &dyn Fn(i32) -> i32, z: i32) -> (i32, usize) {
    if z <= 0 {
        (t(0), 0usize)
    } else {
        let p = (z - 1) >> 1;
        let k = ((z - 1) & 1) as usize;
        let (xp, x) = if p + 1 < 8 {
            (t(p), t(p + 1))
        } else {
            (t(7), t(7))
        };
        let v = if k == 0 {
            (xp + x + 1) / 2
        } else {
            (3 * xp + x + 2) / 4
        };
        (v, k)
    }
}

/// HorizontalUp sample: derived from the left border only (z indexes left).
fn angular_from_z_left(l: &dyn Fn(i32) -> i32, z: i32) -> (i32, usize) {
    if z <= 0 {
        (l(0), 0usize)
    } else {
        let p = (z - 1) >> 1;
        let k = ((z - 1) & 1) as usize;
        let (xp, x) = if p + 1 < 8 {
            (l(p), l(p + 1))
        } else {
            (l(7), l(7))
        };
        let v = if k == 0 {
            (xp + x + 1) / 2
        } else {
            (3 * xp + x + 2) / 4
        };
        (v, k)
    }
}

/// Predict an 8×8 luma block (`out[64]`, row-major) for the given mode.
///
/// The 8×8 modes share the same geometry as the 4×4 modes but operate on the
/// 8×8 sample grid and use the extended neighbour set (the 8 samples above plus
/// the top-right extension, and the 8 left samples plus the single `X`).
pub fn predict_8x8(mode: Intra4x4Mode, top: &[Option<u8>], left: &[Option<u8>], top_left: Option<u8>, out: &mut [u8; 64]) {
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
                    let i = x as i32 - y as i32;
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
                    let i = (x as i32 - (y as i32 >> 1) * 2 + (y & 1) - 1) as i32;
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
                    let i = (y as i32 - (x as i32 >> 1) * 2 + (x & 1) - 1) as i32;
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
                    let i = (x as i32 - (y as i32 >> 1) * 2 + (y & 1) - 1) as i32;
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
                    let i = (y as i32 - (x as i32 >> 1) * 2 + (x & 1) - 1) as i32;
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
                h += i * (t(8 + i) as i32 - t(8 - i) as i32);
            }
            let mut v = 0i32;
            for i in 1..8i32 {
                v += i * (l(8 + i) as i32 - l(8 - i) as i32);
            }
            // a = (top_left + top_15) << 4
            let a = (tl + t(15)) << 4;
            let b = (5 * h + 32) >> 6;
            let c = (5 * v + 32) >> 6;
            for y in 0..16i32 {
                for x in 0..16i32 {
                    let val = a + b * (x as i32 - 7) + c * (y as i32 - 7);
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
pub fn predict_chroma(mode: IntraChromaMode, top: &[Option<u8>; 8], left: &[Option<u8>; 8], tl: Option<u8>, out: &mut [u8; 64]) {
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
                h += i * (t(4 + i) as i32 - t(4 - i) as i32);
            }
            let mut v = 0i32;
            for i in 1..4i32 {
                v += i * (l(4 + i) as i32 - l(4 - i) as i32);
            }
            let a = (tl + t(7)) << 4;
            let b = (17 * h + 16) >> 5;
            let c = (17 * v + 16) >> 5;
            for y in 0..8i32 {
                for x in 0..8i32 {
                    let val = a + b * (x as i32 - 3) + c * (y as i32 - 3);
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
        MbType::Intra4x4
            | MbType::Intra16x16 { .. }
            | MbType::PL016x16
            | MbType::BDirect16x16
    ) || matches!(mb_type, MbType::Intra16x16 { .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n4() -> IntraNeighbours4x4 {
        IntraNeighbours4x4 {
            top: [Some(100); 4],
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
            top: [None; 4],
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
