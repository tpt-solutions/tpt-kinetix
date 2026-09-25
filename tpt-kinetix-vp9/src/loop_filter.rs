//! VP9 in-loop deblocking filter (§8.7): the per-edge filter core
//! (`loop_filter_edge`), the level/limit lookups, the per-superblock mask
//! construction (`setup_mask` / `adjust_mask`) and the per-superblock filter
//! drivers (`loopfilter_sb`), all mirroring the reference implementation.

// The filter and mask formulas intentionally mirror the reference
// index-for-index; rewriting the loops would only obscure the correspondence.
#![allow(clippy::needless_range_loop, clippy::neg_multiply)]
#![allow(clippy::too_many_arguments)]

use crate::frame::{FrameData, SbFilter};

/// `lim_lut` / `mblim_lut` for one sharpness value (reference `filter_lut`).
#[derive(Clone)]
pub struct FilterLut {
    pub lim: [u32; 64],
    pub mblim: [u32; 64],
}

impl FilterLut {
    pub fn new(sharpness: u8) -> Self {
        let mut lut = Self {
            lim: [0; 64],
            mblim: [0; 64],
        };
        for i in 1..64usize {
            let mut limit = i as u32;
            if sharpness > 0 {
                limit >>= (u32::from(sharpness) + 3) >> 2;
                limit = limit.min(u32::from(9 - sharpness));
            }
            limit = limit.max(1);
            lut.lim[i] = limit;
            lut.mblim[i] = 2 * (i as u32 + 2) + limit;
        }
        lut
    }
}

#[inline]
fn abs_diff(a: i32, b: i32) -> i32 {
    (a - b).abs()
}

#[inline]
fn clip_pixel(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// The per-edge filter core (reference `loop_filter`): filters 8 unit steps
/// (rows for vertical edges, columns for horizontal edges) starting at `off`.
///
/// `wd`: 4, 8 or 16 (filter width across the edge).
fn loop_filter_edge(
    data: &mut [u8],
    off: usize,
    stridea: usize,
    strideb: usize,
    e: i32,
    i: i32,
    h: i32,
    wd: usize,
) {
    let dbg56 = std::env::var_os("TPT_VP9_DBG56").is_some() && off == 56;
    if dbg56 {
        eprintln!(
            "EDGE56 wd={} e={} i={} h={} data[48..64]={:?}",
            wd,
            e,
            i,
            h,
            &data[48..64]
        );
    }
    let f = 1i32;
    for step in 0..8 {
        let p = off + step * stridea;
        if dbg56 {
            println!("  ROWSTEP {} ", step);
            let g = |d: i64| data[(p as i64 + d * strideb as i64) as usize] as i32;
            eprintln!(
                "  row {step}: {} {} {} {} | {} {} {} {}",
                g(-4),
                g(-3),
                g(-2),
                g(-1),
                g(0),
                g(1),
                g(2),
                g(3)
            );
        }
        macro_rules! at {
            ($d:expr) => {
                data[(p as i64 + ($d) * strideb as i64) as usize] as i32
            };
        }
        macro_rules! set {
            ($d:expr, $v:expr) => {
                data[(p as i64 + ($d) * strideb as i64) as usize] = clip_pixel($v)
            };
        }
        let p3 = at!(-4);
        let p2 = at!(-3);
        let p1 = at!(-2);
        let p0 = at!(-1);
        let q0 = at!(0);
        let q1 = at!(1);
        let q2 = at!(2);
        let q3 = at!(3);

        let fm = abs_diff(p3, p2) <= i
            && abs_diff(p2, p1) <= i
            && abs_diff(p1, p0) <= i
            && abs_diff(q1, q0) <= i
            && abs_diff(q2, q1) <= i
            && abs_diff(q3, q2) <= i
            && abs_diff(p0, q0) * 2 + (abs_diff(p1, q1) >> 1) <= e;
        if !fm {
            continue;
        }

        let mut flat8out = false;
        let mut flat8in = false;
        if wd >= 16 {
            let p7 = at!(-8);
            let p6 = at!(-7);
            let p5 = at!(-6);
            let p4 = at!(-5);
            let q4 = at!(4);
            let q5 = at!(5);
            let q6 = at!(6);
            let q7 = at!(7);
            flat8out = abs_diff(p7, p0) <= f
                && abs_diff(p6, p0) <= f
                && abs_diff(p5, p0) <= f
                && abs_diff(p4, p0) <= f
                && abs_diff(q4, q0) <= f
                && abs_diff(q5, q0) <= f
                && abs_diff(q6, q0) <= f
                && abs_diff(q7, q0) <= f;
        }
        if wd >= 8 {
            flat8in = abs_diff(p3, p0) <= f
                && abs_diff(p2, p0) <= f
                && abs_diff(p1, p0) <= f
                && abs_diff(q1, q0) <= f
                && abs_diff(q2, q0) <= f
                && abs_diff(q3, q0) <= f;
        }

        if wd >= 16 && flat8out && flat8in {
            let p7 = at!(-8);
            let p6 = at!(-7);
            let p5 = at!(-6);
            let p4 = at!(-5);
            let q4 = at!(4);
            let q5 = at!(5);
            let q6 = at!(6);
            let q7 = at!(7);
            set!(
                -7,
                (p7 + p7 + p7 + p7 + p7 + p7 + p7 + p6 * 2 + p5 + p4 + p3 + p2 + p1 + p0 + q0 + 8)
                    >> 4
            );
            set!(
                -6,
                (p7 + p7 + p7 + p7 + p7 + p7 + p6 + p5 * 2 + p4 + p3 + p2 + p1 + p0 + q0 + q1 + 8)
                    >> 4
            );
            set!(
                -5,
                (p7 + p7 + p7 + p7 + p7 + p6 + p5 + p4 * 2 + p3 + p2 + p1 + p0 + q0 + q1 + q2 + 8)
                    >> 4
            );
            set!(
                -4,
                (p7 + p7 + p7 + p7 + p6 + p5 + p4 + p3 * 2 + p2 + p1 + p0 + q0 + q1 + q2 + q3 + 8)
                    >> 4
            );
            set!(
                -3,
                (p7 + p7 + p7 + p6 + p5 + p4 + p3 + p2 * 2 + p1 + p0 + q0 + q1 + q2 + q3 + q4 + 8)
                    >> 4
            );
            set!(
                -2,
                (p7 + p7 + p6 + p5 + p4 + p3 + p2 + p1 * 2 + p0 + q0 + q1 + q2 + q3 + q4 + q5 + 8)
                    >> 4
            );
            set!(
                -1,
                (p7 + p6 + p5 + p4 + p3 + p2 + p1 + p0 * 2 + q0 + q1 + q2 + q3 + q4 + q5 + q6 + 8)
                    >> 4
            );
            set!(
                0,
                (p6 + p5 + p4 + p3 + p2 + p1 + p0 + q0 * 2 + q1 + q2 + q3 + q4 + q5 + q6 + q7 + 8)
                    >> 4
            );
            set!(
                1,
                (p5 + p4 + p3 + p2 + p1 + p0 + q0 + q1 * 2 + q2 + q3 + q4 + q5 + q6 + q7 + q7 + 8)
                    >> 4
            );
            set!(
                2,
                (p4 + p3 + p2 + p1 + p0 + q0 + q1 + q2 * 2 + q3 + q4 + q5 + q6 + q7 + q7 + q7 + 8)
                    >> 4
            );
            set!(
                3,
                (p3 + p2 + p1 + p0 + q0 + q1 + q2 + q3 * 2 + q4 + q5 + q6 + q7 + q7 + q7 + q7 + 8)
                    >> 4
            );
            set!(
                4,
                (p2 + p1 + p0 + q0 + q1 + q2 + q3 + q4 * 2 + q5 + q6 + q7 + q7 + q7 + q7 + q7 + 8)
                    >> 4
            );
            set!(
                5,
                (p1 + p0 + q0 + q1 + q2 + q3 + q4 + q5 * 2 + q6 + q7 + q7 + q7 + q7 + q7 + q7 + 8)
                    >> 4
            );
            set!(
                6,
                (p0 + q0 + q1 + q2 + q3 + q4 + q5 + q6 * 2 + q7 + q7 + q7 + q7 + q7 + q7 + q7 + 8)
                    >> 4
            );
        } else if wd >= 8 && flat8in {
            set!(-3, (p3 + p3 + p3 + 2 * p2 + p1 + p0 + q0 + 4) >> 3);
            set!(-2, (p3 + p3 + p2 + 2 * p1 + p0 + q0 + q1 + 4) >> 3);
            set!(-1, (p3 + p2 + p1 + 2 * p0 + q0 + q1 + q2 + 4) >> 3);
            set!(0, (p2 + p1 + p0 + 2 * q0 + q1 + q2 + q3 + 4) >> 3);
            set!(1, (p1 + p0 + q0 + 2 * q1 + q2 + q3 + q3 + 4) >> 3);
            set!(2, (p0 + q0 + q1 + 2 * q2 + q3 + q3 + q3 + 4) >> 3);
        } else {
            let hev = abs_diff(p1, p0) > h || abs_diff(q1, q0) > h;
            if hev {
                let mut f1 = (p1 - q1).clamp(-128, 127);
                f1 = (3 * (q0 - p0) + f1).clamp(-128, 127);
                let f1v = (f1 + 4).min(127) >> 3;
                let f2v = (f1 + 3).min(127) >> 3;
                set!(-1, p0 + f2v);
                set!(0, q0 - f1v);
            } else {
                let f = (3 * (q0 - p0)).clamp(-128, 127);
                let f1v = (f + 4).min(127) >> 3;
                let f2v = (f + 3).min(127) >> 3;
                set!(-1, p0 + f2v);
                set!(0, q0 - f1v);
                let half = (f1v + 1) >> 1;
                set!(-2, p1 + half);
                set!(1, q1 - half);
            }
        }
    }
}

// --- Mask tables (reference vp9_loopfilter.c tables, reindexed to our
// `BS_*` order, which is the reverse of the reference BLOCK_SIZES enum). ---

const LEFT_PREDICTION_MASK: [u64; 13] = [
    0x0101010101010101, // 64X64
    0x0000000001010101, // 64X32
    0x0101010101010101, // 32X64
    0x0000000001010101, // 32X32
    0x0000000000000101, // 32X16
    0x0000000001010101, // 16X32
    0x0000000000000101, // 16X16
    0x0000000000000001, // 16X8
    0x0000000000000101, // 8X16
    0x0000000000000001, // 8X8
    0x0000000000000001, // 8X4
    0x0000000000000001, // 4X8
    0x0000000000000001, // 4X4
];

const ABOVE_PREDICTION_MASK: [u64; 13] = [
    0x00000000000000ff, // 64X64
    0x00000000000000ff, // 64X32
    0x000000000000000f, // 32X64
    0x000000000000000f, // 32X32
    0x000000000000000f, // 32X16
    0x0000000000000003, // 16X32
    0x0000000000000003, // 16X16
    0x0000000000000003, // 16X8
    0x0000000000000001, // 8X16
    0x0000000000000001, // 8X8
    0x0000000000000001, // 8X4
    0x0000000000000001, // 4X8
    0x0000000000000001, // 4X4
];

const SIZE_MASK: [u64; 13] = [
    0xffffffffffffffff,
    0x00000000ffffffff,
    0x0f0f0f0f0f0f0f0f,
    0x000000000f0f0f0f,
    0x0000000000000f0f,
    0x0000000003030303,
    0x0000000000000303,
    0x0000000000000003,
    0x0000000000000101,
    0x0000000000000001,
    0x0000000000000001,
    0x0000000000000001,
    0x0000000000000001,
];

const LEFT_TXFORM_MASK: [u64; 4] = [
    0xffffffffffffffff,
    0xffffffffffffffff,
    0x5555555555555555,
    0x1111111111111111,
];

const ABOVE_TXFORM_MASK: [u64; 4] = [
    0xffffffffffffffff,
    0xffffffffffffffff,
    0x00ff00ff00ff00ff,
    0x000000ff000000ff,
];

const LEFT_PREDICTION_MASK_UV: [u16; 13] = [
    0x1111, // 64X64
    0x0011, // 64X32
    0x1111, // 32X64
    0x0011, // 32X32
    0x0001, // 32X16
    0x0011, // 16X32
    0x0001, // 16X16
    0x0001, // 16X8
    0x0001, // 8X16
    0x0001, // 8X8
    0x0001, // 8X4
    0x0001, // 4X8
    0x0001, // 4X4
];

const ABOVE_PREDICTION_MASK_UV: [u16; 13] = [
    0x000f, 0x000f, 0x0003, 0x0003, 0x0003, 0x0001, 0x0001, 0x0001, 0x0001, 0x0001, 0x0001, 0x0001,
    0x0001,
];

const SIZE_MASK_UV: [u16; 13] = [
    0xffff, 0x00ff, 0x3333, 0x0033, 0x0003, 0x0011, 0x0001, 0x0001, 0x0001, 0x0001, 0x0001, 0x0001,
    0x0001,
];

const LEFT_TXFORM_MASK_UV: [u16; 4] = [0xffff, 0xffff, 0x5555, 0x1111];
const ABOVE_TXFORM_MASK_UV: [u16; 4] = [0xffff, 0xffff, 0x0f0f, 0x000f];
const LEFT_BORDER: u64 = 0x1111111111111111;
const ABOVE_BORDER: u64 = 0x000000ff000000ff;
const LEFT_BORDER_UV: u16 = 0x1111;
const ABOVE_BORDER_UV: u16 = 0x000f;

/// `uv_txsize_lookup[bs][tx][1][1]` (4:2:0), our `BS_*` order.
const UV_TXSIZE: [[u8; 4]; 13] = [
    [0, 1, 2, 3], // 64X64
    [0, 1, 2, 2], // 64X32
    [0, 1, 2, 2], // 32X64
    [0, 1, 2, 2], // 32X32
    [0, 1, 1, 1], // 32X16
    [0, 1, 1, 1], // 16X32
    [0, 1, 1, 1], // 16X16
    [0, 0, 1, 1], // 16X8
    [0, 0, 0, 0], // 8X16
    [0, 0, 0, 0], // 8X8
    [0, 0, 0, 0], // 8X4
    [0, 0, 0, 0], // 4X8
    [0, 0, 0, 0], // 4X4
];

const SHIFT_32_Y: [usize; 4] = [0, 4, 32, 36];
const SHIFT_16_Y: [usize; 4] = [0, 2, 16, 18];
const SHIFT_8_Y: [usize; 4] = [0, 1, 8, 9];
const SHIFT_32_UV: [usize; 4] = [0, 2, 8, 10];
const SHIFT_16_UV: [usize; 4] = [0, 1, 4, 5];

// block-size indices used by the walk (block_level * 3 + partition order)
const BS_64X64: u8 = 0;
const BS_64X32: u8 = 1;
const BS_32X64: u8 = 2;
const BS_32X32: u8 = 3;
const BS_32X16: u8 = 4;
const BS_16X32: u8 = 5;
const BS_16X16: u8 = 6;
const BS_16X8: u8 = 7;
const BS_8X16: u8 = 8;

/// Compute the per-superblock masks from the per-unit grid (reference
/// `vp9_setup_mask`). `max_rows`/`max_cols` are the 8px units inside the
/// frame.
pub fn setup_mask(f: &mut SbFilter, max_rows: usize, max_cols: usize) {
    let bs00 = f.unit[0].bs;
    if bs00 == BS_64X64 {
        build_masks(f, 0, 0, 0, true);
    } else if bs00 == BS_64X32 {
        build_masks(f, 0, 0, 0, true);
        if 4 < max_rows {
            build_masks(f, 32, 32, 8, true);
        }
    } else if bs00 == BS_32X64 {
        build_masks(f, 0, 0, 0, true);
        if 4 < max_cols {
            build_masks(f, 4, 4, 2, true);
        }
    } else {
        for idx32 in 0..4 {
            let c32 = (idx32 & 1) << 2;
            let r32 = (idx32 >> 1) << 2;
            if c32 >= max_cols || r32 >= max_rows {
                continue;
            }
            let u32i = r32 * 8 + c32;
            let sy32 = SHIFT_32_Y[idx32];
            let suv32 = SHIFT_32_UV[idx32];
            let bs32 = f.unit[u32i].bs;
            if bs32 == BS_32X32 {
                build_masks(f, u32i, sy32, suv32, true);
            } else if bs32 == BS_32X16 {
                build_masks(f, u32i, sy32, suv32, true);
                if r32 + 2 < max_rows {
                    build_masks(f, u32i + 16, sy32 + 16, suv32 + 4, true);
                }
            } else if bs32 == BS_16X32 {
                build_masks(f, u32i, sy32, suv32, true);
                if c32 + 2 < max_cols {
                    build_masks(f, u32i + 2, sy32 + 2, suv32 + 1, true);
                }
            } else {
                for idx16 in 0..4 {
                    let c16 = c32 + ((idx16 & 1) << 1);
                    let r16 = r32 + ((idx16 >> 1) << 1);
                    if c16 >= max_cols || r16 >= max_rows {
                        continue;
                    }
                    let u16i = r16 * 8 + c16;
                    let sy16 = sy32 + SHIFT_16_Y[idx16];
                    let suv16 = suv32 + SHIFT_16_UV[idx16];
                    let bs16 = f.unit[u16i].bs;
                    if bs16 == BS_16X16 {
                        build_masks(f, u16i, sy16, suv16, true);
                    } else if bs16 == BS_16X8 {
                        build_masks(f, u16i, sy16, suv16, true);
                        if r16 + 1 < max_rows {
                            build_y_mask(f, u16i + 8, sy16 + 8);
                        }
                    } else if bs16 == BS_8X16 {
                        build_masks(f, u16i, sy16, suv16, true);
                        if c16 + 1 < max_cols {
                            build_y_mask(f, u16i + 1, sy16 + 1);
                        }
                    } else {
                        for idx8 in 0..4 {
                            let c8 = c16 + (idx8 & 1);
                            let r8 = r16 + (idx8 >> 1);
                            if c8 >= max_cols || r8 >= max_rows {
                                continue;
                            }
                            let u8i = r8 * 8 + c8;
                            if idx8 == 0 {
                                build_masks(f, u8i, sy16 + SHIFT_8_Y[idx8], suv16, true);
                            } else {
                                build_y_mask(f, u8i, sy16 + SHIFT_8_Y[idx8]);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Reference `build_masks` (uv parts) / `build_y_mask` (uv omitted), selected
/// with `with_uv`. `shift_y` is the unit index of the block's top-left corner.
fn build_masks(f: &mut SbFilter, unit: usize, shift_y: usize, shift_uv: usize, with_uv: bool) {
    let u = f.unit[unit];
    if std::env::var_os("TPT_VP9_TRACE").is_some() {
        eprintln!(
            "BM unit={} u.bs={} shift_y={} lvl={} tx={}",
            unit, u.bs, shift_y, u.lvl, u.tx
        );
    }
    if u.bs == 255 {
        return;
    }
    let bs = u.bs as usize;
    let tx = u.tx as usize;
    let uvtx = u.uvtx as usize;
    let lvl = u.lvl;
    if lvl == 0 {
        return;
    }
    let (w, h) = crate::header::bwh(1, bs);
    let mut index = shift_y;
    for _ in 0..h {
        for k in 0..w {
            f.lfl_y[index + k] = lvl;
        }
        index += 8;
    }

    f.above_y[tx] |= ABOVE_PREDICTION_MASK[bs] << shift_y;
    f.left_y[tx] |= LEFT_PREDICTION_MASK[bs] << shift_y;
    if with_uv {
        f.above_uv[uvtx] |= ABOVE_PREDICTION_MASK_UV[bs] << shift_uv;
        f.left_uv[uvtx] |= LEFT_PREDICTION_MASK_UV[bs] << shift_uv;
    }

    // skip + inter blocks keep only their prediction (block) edges
    if u.skip_inter {
        return;
    }

    f.above_y[tx] |= (SIZE_MASK[bs] & ABOVE_TXFORM_MASK[tx]) << shift_y;
    f.left_y[tx] |= (SIZE_MASK[bs] & LEFT_TXFORM_MASK[tx]) << shift_y;
    if with_uv {
        f.above_uv[uvtx] |= (SIZE_MASK_UV[bs] & ABOVE_TXFORM_MASK_UV[uvtx]) << shift_uv;
        f.left_uv[uvtx] |= (SIZE_MASK_UV[bs] & LEFT_TXFORM_MASK_UV[uvtx]) << shift_uv;
    }

    if tx == 0 {
        f.int_4x4_y |= SIZE_MASK[bs] << shift_y;
    }
    if with_uv && uvtx == 0 {
        f.int_4x4_uv |= SIZE_MASK_UV[bs] << shift_uv;
    }
}

fn build_y_mask(f: &mut SbFilter, unit: usize, shift_y: usize) {
    build_masks(f, unit, shift_y, 0, false);
}

/// Reference `vp9_adjust_mask`.
pub fn adjust_mask(f: &mut SbFilter, mi_row: usize, mi_col: usize, mi_rows: usize, mi_cols: usize) {
    f.left_y[2] |= f.left_y[3];
    f.above_y[2] |= f.above_y[3];
    f.left_uv[2] |= f.left_uv[3];
    f.above_uv[2] |= f.above_uv[3];

    f.left_y[1] |= f.left_y[0] & LEFT_BORDER;
    f.left_y[0] &= !LEFT_BORDER;
    f.above_y[1] |= f.above_y[0] & ABOVE_BORDER;
    f.above_y[0] &= !ABOVE_BORDER;
    f.left_uv[1] |= f.left_uv[0] & LEFT_BORDER_UV;
    f.left_uv[0] &= !LEFT_BORDER_UV;
    f.above_uv[1] |= f.above_uv[0] & ABOVE_BORDER_UV;
    f.above_uv[0] &= !ABOVE_BORDER_UV;

    if mi_row + 8 > mi_rows {
        let rows = mi_rows - mi_row;
        let mask_y = (1u64 << (rows << 3)).wrapping_sub(1);
        let mask_uv = (1u16 << (((rows + 1) >> 1) << 2)).wrapping_sub(1);
        for i in 0..3 {
            f.left_y[i] &= mask_y;
            f.above_y[i] &= mask_y;
            f.left_uv[i] &= mask_uv;
            f.above_uv[i] &= mask_uv;
        }
        f.int_4x4_y &= mask_y;
        f.int_4x4_uv &= mask_uv;

        if rows == 1 {
            f.above_uv[1] |= f.above_uv[2];
            f.above_uv[2] = 0;
        }
        if rows == 5 {
            f.above_uv[1] |= f.above_uv[2] & 0xff00;
            f.above_uv[2] &= !(f.above_uv[2] & 0xff00);
        }
    }

    if mi_col + 8 > mi_cols {
        let columns = mi_cols - mi_col;
        let mask_y = ((1u64 << columns) - 1).wrapping_mul(0x0101010101010101);
        let mask_uv = ((1u16 << ((columns + 1) >> 1)) - 1).wrapping_mul(0x1111);
        let mask_uv_int = ((1u16 << (columns >> 1)) - 1).wrapping_mul(0x1111);

        for i in 0..3 {
            f.left_y[i] &= mask_y;
            f.above_y[i] &= mask_y;
            f.left_uv[i] &= mask_uv;
            f.above_uv[i] &= mask_uv;
        }
        f.int_4x4_y &= mask_y;
        f.int_4x4_uv &= mask_uv_int;

        if columns == 1 {
            f.left_uv[1] |= f.left_uv[2];
            f.left_uv[2] = 0;
        }
        if columns == 5 {
            f.left_uv[1] |= f.left_uv[2] & 0xcccc;
            f.left_uv[2] &= !(f.left_uv[2] & 0xcccc);
        }
    }

    if mi_col == 0 {
        for i in 0..3 {
            f.left_y[i] &= 0xfefefefefefefefe;
            f.left_uv[i] &= 0xeeee;
        }
    }
}

/// Apply one filter of width `wd` (4/8/16) at `off` with the thresholds for
/// level `lvl` (reference `vpx_lpf_{vertical,horizontal}_{4,8,16}`).
fn apply(
    data: &mut [u8],
    vertical: bool,
    wd: usize,
    off: usize,
    lvl: u8,
    luts: &FilterLut,
    stride: usize,
) {
    if lvl == 0 {
        return;
    }
    let e = luts.mblim[lvl as usize] as i32;
    let i = luts.lim[lvl as usize] as i32;
    let h = (lvl >> 4) as i32;
    // vertical edges step down rows (8 filtered lines `stride` apart);
    // horizontal edges step across columns.
    let (k, stepta, strideb): (&str, usize, usize) = if vertical {
        ("V", stride, 1)
    } else {
        ("H", 1, stride)
    };
    if std::env::var_os("TPT_VP9_TRACE").is_some() {
        eprintln!("LFP k={} wd={} off={} pitch={}", k, wd, off, stride);
    }
    if std::env::var_os("TPT_VP9_OPS").is_some() {
        // dump the kernel's touched window (8(+8) steps along `strtea`,
        // [-depth, depth-1] along `strideb`) before/after, matching the
        // oracle's KI/KO dumps byte-for-byte for op-level comparison
        let depth: usize = if wd == 4 { 4 } else { wd / 2 };
        let grab = |d: &[u8]| -> String {
            // our loop_filter_edge always filters 8 steps per call (a 16_dual
            // on the oracle side is two consecutive calls); oracle count=16
            // dumps are split into 8-step halves when comparing
            let mut s = String::new();
            for step in 0..8usize {
                let p = off + step * stepta;
                for j in -(depth as i64)..(depth as i64) {
                    let q = p as i64 + j * strideb as i64;
                    if q < 0 || q as usize >= d.len() {
                        continue;
                    }
                    s.push_str(&format!("{:02x}", d[q as usize]));
                }
            }
            s
        };
        let pre = grab(data);
        loop_filter_edge(data, off, stepta, strideb, e, i, h, wd);
        eprintln!(
            "OPD k={} wd={} off={} e={} i={} h={}\n  in={} out={}",
            k,
            wd,
            off,
            e,
            i,
            h,
            pre,
            grab(data)
        );
        return;
    }
    loop_filter_edge(data, off, stepta, strideb, e, i, h, wd);
}

/// Reference `filter_selectively_vert_row2`.
#[allow(clippy::too_many_arguments)]
fn filter_selectively_vert_row2(
    data: &mut [u8],
    pitch: usize,
    s0: usize,
    factor: usize,
    mut m16: u32,
    mut m8: u32,
    mut m4: u32,
    mut m4i: u32,
    luts: &FilterLut,
    lfl: &[u8],
) {
    let dual_mask_cutoff: u32 = if factor != 0 { 0xff } else { 0xffff };
    let lfl_forward = if factor != 0 { 4usize } else { 8 };
    let dual_one: u32 = 1 | (1 << lfl_forward);
    let mut ss0 = s0;
    let mut li = 0usize;
    let mut mask = (m16 | m8 | m4 | m4i) & dual_mask_cutoff;
    while mask != 0 {
        if mask & dual_one != 0 {
            let l0 = lfl[li];
            let l1 = lfl[li + lfl_forward];
            let ss1 = ss0 + 8 * pitch;

            if m16 & dual_one != 0 {
                if (m16 & dual_one) == dual_one {
                    apply(data, true, 16, ss0, l0, luts, pitch);
                    // 16_dual applies the first half's thresholds to both
                    apply(data, true, 16, ss1, l0, luts, pitch);
                } else if m16 & 1 != 0 {
                    apply(data, true, 16, ss0, l0, luts, pitch);
                } else {
                    apply(data, true, 16, ss1, l1, luts, pitch);
                }
            }
            if m8 & dual_one != 0 {
                if (m8 & dual_one) == dual_one {
                    apply(data, true, 8, ss0, l0, luts, pitch);
                    apply(data, true, 8, ss1, l1, luts, pitch);
                } else if m8 & 1 != 0 {
                    apply(data, true, 8, ss0, l0, luts, pitch);
                } else {
                    apply(data, true, 8, ss1, l1, luts, pitch);
                }
            }
            if m4 & dual_one != 0 {
                if (m4 & dual_one) == dual_one {
                    apply(data, true, 4, ss0, l0, luts, pitch);
                    apply(data, true, 4, ss1, l1, luts, pitch);
                } else if m4 & 1 != 0 {
                    apply(data, true, 4, ss0, l0, luts, pitch);
                } else {
                    apply(data, true, 4, ss1, l1, luts, pitch);
                }
            }
            if m4i & dual_one != 0 {
                if (m4i & dual_one) == dual_one {
                    apply(data, true, 4, ss0 + 4, l0, luts, pitch);
                    apply(data, true, 4, ss1 + 4, l1, luts, pitch);
                } else if m4i & 1 != 0 {
                    apply(data, true, 4, ss0 + 4, l0, luts, pitch);
                } else {
                    apply(data, true, 4, ss1 + 4, l1, luts, pitch);
                }
            }
        }
        ss0 += 8;
        li += 1;
        m16 >>= 1;
        m8 >>= 1;
        m4 >>= 1;
        m4i >>= 1;
        mask = (mask & !dual_one) >> 1;
    }
}

/// Reference `filter_selectively_horiz`.
#[allow(clippy::too_many_arguments)]
fn filter_selectively_horiz(
    data: &mut [u8],
    pitch: usize,
    s0: usize,
    mut m16: u32,
    mut m8: u32,
    mut m4: u32,
    mut m4i: u32,
    luts: &FilterLut,
    lfl: &[u8],
) {
    let mut ss = s0;
    let mut li = 0usize;
    let mut mask = m16 | m8 | m4 | m4i;
    while mask != 0 {
        let mut count = 1usize;
        if mask & 1 != 0 {
            let l0 = lfl[li];
            if m16 & 1 != 0 {
                if (m16 & 3) == 3 {
                    apply(data, false, 16, ss, l0, luts, pitch);
                    apply(data, false, 16, ss + 8, l0, luts, pitch);
                    count = 2;
                } else {
                    apply(data, false, 16, ss, l0, luts, pitch);
                }
            } else if m8 & 1 != 0 {
                if (m8 & 3) == 3 {
                    let l1 = lfl[li + 1];
                    apply(data, false, 8, ss, l0, luts, pitch);
                    apply(data, false, 8, ss + 8, l1, luts, pitch);
                    if (m4i & 3) == 3 {
                        apply(data, false, 4, ss + 4 * pitch, l0, luts, pitch);
                        apply(data, false, 4, ss + 8 + 4 * pitch, l1, luts, pitch);
                    } else if m4i & 1 != 0 {
                        apply(data, false, 4, ss + 4 * pitch, l0, luts, pitch);
                    } else if m4i & 2 != 0 {
                        apply(data, false, 4, ss + 8 + 4 * pitch, l1, luts, pitch);
                    }
                    count = 2;
                } else {
                    apply(data, false, 8, ss, l0, luts, pitch);
                    if m4i & 1 != 0 {
                        apply(data, false, 4, ss + 4 * pitch, l0, luts, pitch);
                    }
                }
            } else if m4 & 1 != 0 {
                if (m4 & 3) == 3 {
                    let l1 = lfl[li + 1];
                    apply(data, false, 4, ss, l0, luts, pitch);
                    apply(data, false, 4, ss + 8, l1, luts, pitch);
                    if (m4i & 3) == 3 {
                        apply(data, false, 4, ss + 4 * pitch, l0, luts, pitch);
                        apply(data, false, 4, ss + 8 + 4 * pitch, l1, luts, pitch);
                    } else if m4i & 1 != 0 {
                        apply(data, false, 4, ss + 4 * pitch, l0, luts, pitch);
                    } else if m4i & 2 != 0 {
                        apply(data, false, 4, ss + 8 + 4 * pitch, l1, luts, pitch);
                    }
                    count = 2;
                } else {
                    apply(data, false, 4, ss, l0, luts, pitch);
                    if m4i & 1 != 0 {
                        apply(data, false, 4, ss + 4 * pitch, l0, luts, pitch);
                    }
                }
            } else {
                apply(data, false, 4, ss + 4 * pitch, l0, luts, pitch);
            }
        }
        ss += 8 * count;
        li += count;
        m16 >>= count;
        m8 >>= count;
        m4 >>= count;
        m4i >>= count;
        mask >>= count;
    }
}

/// `uv_txsize_lookup[bs][tx][1][1]` for 4:2:0, in our `BS_*` order.
pub fn uv_txsize(bs: usize, tx: usize) -> usize {
    UV_TXSIZE[bs][tx] as usize
}

/// Run the loop filter over one superblock (reference `loop_filter_rows`
/// per-SB body: `vp9_adjust_mask` + `vp9_filter_block_plane_ss00` for luma
/// and `vp9_filter_block_plane_ss11` for each 4:2:0 chroma plane).
pub fn loopfilter_sb(
    frame: &mut FrameData,
    sf: &SbFilter,
    luts: &FilterLut,
    sb_row: usize,
    sb_col: usize,
) {
    let mi_rows = frame.mi_rows;
    let mi_cols = frame.mi_cols;
    if std::env::var_os("TPT_VP9_TRACE").is_some() {
        let nz = sf.unit.iter().filter(|u| u.lvl != 0).count();
        let valid = sf.unit.iter().filter(|u| u.bs != 255).count();
        eprintln!(
            "LFSB r={} c={} nzlvl={} valid={}",
            sb_row, sb_col, nz, valid
        );
    }
    if std::env::var_os("TPT_VP9_TRACE").is_some() {
        for (i, u) in sf.unit.iter().enumerate() {
            eprintln!(
                "UNIT r={} c={} {} r{} c{} bs={} tx={} lvl={}",
                sb_row,
                sb_col,
                i,
                i / 8,
                i % 8,
                u.bs,
                u.tx,
                u.lvl
            );
        }
    }
    let mut lfm = sf.clone();
    setup_mask(
        &mut lfm,
        (mi_rows - sb_row * 8).min(8),
        (mi_cols - sb_col * 8).min(8),
    );
    adjust_mask(&mut lfm, sb_row * 8, sb_col * 8, mi_rows, mi_cols);
    if std::env::var_os("TPT_VP9_TRACE").is_some() {
        eprintln!(
            "LFM2 r{} c{} {:016x} {:016x} {:016x} {:016x} {:016x} {:016x}",
            sb_row,
            sb_col,
            lfm.left_y[0],
            lfm.left_y[1],
            lfm.left_y[2],
            lfm.above_y[0],
            lfm.above_y[1],
            lfm.above_y[2]
        );
    }

    // ---- luma (reference ss00) ----
    let stride = frame.stride;
    let sb_px = sb_col * 64;
    let sb_py = sb_row * 64;
    {
        // vertical pass, two unit-rows at a time
        let mut m16 = lfm.left_y[2];
        let mut m8 = lfm.left_y[1];
        let mut m4 = lfm.left_y[0];
        let mut m4i = lfm.int_4x4_y;
        let mut py = sb_py;
        let mut r = 0usize;
        while r < 8 && sb_row * 8 + r < mi_rows {
            let lfl: Vec<u8> = lfm.lfl_y[(r << 3)..((r << 3) + 16)].to_vec();
            let y = &mut frame.y;
            filter_selectively_vert_row2(
                y,
                stride,
                sb_px + py * stride,
                0,
                m16 as u32,
                m8 as u32,
                m4 as u32,
                m4i as u32,
                luts,
                &lfl,
            );
            if std::env::var_os("TPT_VP9_TRACE").is_some() {
                eprintln!(
                    "LFM2 r{} c{} {:016x} {:016x} {:016x} {:016x} {:016x} {:016x}",
                    sb_row,
                    sb_col,
                    lfm.left_y[0],
                    lfm.left_y[1],
                    lfm.left_y[2],
                    lfm.above_y[0],
                    lfm.above_y[1],
                    lfm.above_y[2]
                );
            }
            py += 16;
            m16 >>= 16;
            m8 >>= 16;
            m4 >>= 16;
            m4i >>= 16;
            r += 2;
        }

        // horizontal pass
        let mut m16 = lfm.above_y[2];
        let mut m8 = lfm.above_y[1];
        let mut m4 = lfm.above_y[0];
        let mut m4i = lfm.int_4x4_y;
        let mut py = sb_py;
        let mut r = 0usize;
        while r < 8 && sb_row * 8 + r < mi_rows {
            let (m16r, m8r, m4r) = if sb_row * 8 + r == 0 {
                (0u32, 0u32, 0u32)
            } else {
                ((m16 & 0xff) as u32, (m8 & 0xff) as u32, (m4 & 0xff) as u32)
            };
            let m4ir = (m4i & 0xff) as u32;
            if std::env::var_os("TPT_VP9_TRACE").is_some() {
                eprintln!(
                    "HMASK2 r={} m16={:x} m8={:x} m4={:x} m4i={:x}",
                    r, m16r, m8r, m4r, m4ir
                );
            }
            let lfl: Vec<u8> = lfm.lfl_y[(r << 3)..((r << 3) + 8)].to_vec();
            let y = &mut frame.y;
            filter_selectively_horiz(
                y,
                stride,
                sb_px + py * stride,
                m16r,
                m8r,
                m4r,
                m4ir,
                luts,
                &lfl,
            );
            py += 8;
            m16 >>= 8;
            m8 >>= 8;
            m4 >>= 8;
            m4i >>= 8;
            r += 1;
        }
    }

    // ---- chroma 4:2:0 (reference ss11) ----
    let ustride = stride >> 1;
    for chroma in 1..=2 {
        let sb_px_uv = sb_px >> 1;
        let sb_py_uv = sb_py >> 1;
        let mut lfl_uv = [0u8; 16];

        // vertical pass, four unit-rows at a time
        let mut m16 = lfm.left_uv[2] as u32;
        let mut m8 = lfm.left_uv[1] as u32;
        let mut m4 = lfm.left_uv[0] as u32;
        let mut m4i = lfm.int_4x4_uv as u32;
        let mut py = sb_py_uv;
        let mut r = 0usize;
        while r < 8 && sb_row * 8 + r < mi_rows {
            for c in 0..4 {
                lfl_uv[(r << 1) + c] = lfm.lfl_y[(r << 3) + (c << 1)];
                lfl_uv[((r + 2) << 1) + c] = lfm.lfl_y[((r + 2) << 3) + (c << 1)];
            }
            {
                let plane = if chroma == 1 {
                    &mut frame.u
                } else {
                    &mut frame.v
                };
                filter_selectively_vert_row2(
                    plane,
                    ustride,
                    sb_px_uv + py * ustride,
                    1,
                    m16,
                    m8,
                    m4,
                    m4i,
                    luts,
                    &lfl_uv[(r << 1)..],
                );
            }
            py += 16;
            m16 >>= 8;
            m8 >>= 8;
            m4 >>= 8;
            m4i >>= 8;
            r += 4;
        }

        // horizontal pass
        let mut m16 = lfm.above_uv[2] as u32;
        let mut m8 = lfm.above_uv[1] as u32;
        let mut m4 = lfm.above_uv[0] as u32;
        let mut m4i = lfm.int_4x4_uv as u32;
        let mut py = sb_py_uv;
        let mut r = 0usize;
        while r < 8 && sb_row * 8 + r < mi_rows {
            let skip_border_4x4 = sb_row * 8 + r == mi_rows - 1;
            let m4ir: u32 = if skip_border_4x4 { 0 } else { m4i & 0xf };
            let (m16r, m8r, m4r) = if sb_row * 8 + r == 0 {
                (0u32, 0u32, 0u32)
            } else {
                (m16 & 0xf, m8 & 0xf, m4 & 0xf)
            };
            let lfl_uv_r: Vec<u8> = lfl_uv[(r << 1)..((r << 1) + 4)].to_vec();
            let plane = if chroma == 1 {
                &mut frame.u
            } else {
                &mut frame.v
            };
            filter_selectively_horiz(
                plane,
                ustride,
                sb_px_uv + py * ustride,
                m16r,
                m8r,
                m4r,
                m4ir,
                luts,
                &lfl_uv_r,
            );
            py += 8;
            m16 >>= 4;
            m8 >>= 4;
            m4 >>= 4;
            m4i >>= 4;
            r += 2;
        }
    }
}
