//! VP9 in-loop deblocking filter (§8.7): the per-edge filter core
//! (`loop_filter`), the level/limit lookups, per-block edge-mask recording
//! (`mask_edges`) and the per-superblock filter driver.

// The filter and mask formulas intentionally mirror the spec pseudocode
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
    let f = 1i32;
    for step in 0..8 {
        let p = off + step * stridea;
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

/// `mask_edges` port: record the 8px edges `tx`-sized transform blocks of a
/// block contribute to its superblock's filter masks.
///
/// `mask` is `[dir 0=col 1=row][8 rows][4 width classes]` for one plane
/// (class 0=16, 1=8, 2=4, 3=inner4-monochrome-only). `ss_h`/`ss_v` are the
/// plane subsampling, `col_end`/`row_end` the odd-frame-edge markers.
#[allow(clippy::too_many_arguments)]
pub fn mask_edges(
    mask: &mut [[[u8; 4]; 8]; 2],
    ss_h: u32,
    ss_v: u32,
    row_and_7: usize,
    col_and_7: usize,
    w: usize,
    h: usize,
    col_end: usize,
    row_end: usize,
    tx: usize,
    skip_inter: bool,
) {
    const WIDE_FILTER_COL_MASK: [u32; 2] = [0x11, 0x01];
    const WIDE_FILTER_ROW_MASK: [u32; 2] = [0x03, 0x07];
    let (ss_h, ss_v) = (ss_h as usize, ss_v as usize);
    let mut w = w;
    let mut h = h;

    // For subsampled planes, TX_4X4 blocks only filter on even 8px
    // boundaries (pairing two sub-blocks per UV edge).
    if tx == 0 && (ss_v | ss_h) != 0 {
        if h == ss_v {
            if row_and_7 & 1 != 0 {
                return;
            }
            if row_end == 0 {
                h += 1;
            }
        }
        if w == ss_h {
            if col_and_7 & 1 != 0 {
                return;
            }
            if col_end == 0 {
                w += 1;
            }
        }
    }

    if tx == 0 && !skip_inter {
        let t: u32 = 1 << col_and_7;
        let m_col = (t << w) - t; // w <= 8, fits u32
        let m_row_8 = m_col & WIDE_FILTER_COL_MASK[ss_h];
        let m_row_4 = m_col - m_row_8;
        for y in row_and_7..h + row_and_7 {
            let col_mask_id = 2 - usize::from((y as u32) & WIDE_FILTER_ROW_MASK[ss_v] != 0);
            mask[0][y][1] |= m_row_8 as u8;
            mask[0][y][2] |= m_row_4 as u8;
            if (ss_h & ss_v) != 0 && (col_end & 1) != 0 && (y & 1) != 0 {
                mask[1][y][col_mask_id] |= (((t << (w - 1)) - t) & 0xff) as u8;
            } else {
                mask[1][y][col_mask_id] |= (m_col & 0xff) as u8;
            }
            if ss_h == 0 {
                mask[0][y][3] |= (m_col & 0xff) as u8;
            }
            if ss_v == 0 {
                if ss_h != 0 && (col_end & 1) != 0 {
                    mask[1][y][3] |= (((t << (w - 1)) - t) & 0xff) as u8;
                } else {
                    mask[1][y][3] |= (m_col & 0xff) as u8;
                }
            }
        }
    } else if !skip_inter {
        let t: u32 = 1 << col_and_7;
        let m_col = (t << w) - t; // w <= 8, fits u32
        let mask_id = usize::from(tx == 1);
        let l2 = tx + ss_h - 1;
        const MASKS: [u32; 4] = [0xff, 0x55, 0x11, 0x01];
        let m_row = m_col & MASKS[l2];
        if ss_h != 0 && tx > 1 && (w ^ (w - 1)) == 1 {
            // odd UV col edges of tx16/tx32: force the 8-wide filter at the
            // visible edge to avoid filtering past it
            let m_row_16 = (((t << (w - 1)) - t) & MASKS[l2]) as u8;
            let m_row_8 = (m_row - (((t << (w - 1)) - t) & MASKS[l2])) as u8;
            for y in row_and_7..h + row_and_7 {
                mask[0][y][0] |= m_row_16;
                mask[0][y][1] |= m_row_8;
            }
        } else {
            for y in row_and_7..h + row_and_7 {
                mask[0][y][mask_id] |= (m_row & 0xff) as u8;
            }
        }
        let l2 = tx + ss_v - 1;
        let step1d = 1usize << l2;
        if ss_v != 0 && tx > 1 && (h ^ (h - 1)) == 1 {
            let mut y = row_and_7;
            while y < h + row_and_7 - 1 {
                mask[1][y][0] |= (m_col & 0xff) as u8;
                y += step1d;
            }
            if y - row_and_7 == h - 1 {
                mask[1][y][1] |= (m_col & 0xff) as u8;
            }
        } else {
            let mut y = row_and_7;
            while y < h + row_and_7 {
                mask[1][y][mask_id] |= (m_col & 0xff) as u8;
                y += step1d;
            }
        }
    } else if tx != 0 {
        // skip_inter, tx > 4x4: only the block edge itself
        let t: u32 = 1 << col_and_7;
        let m_col = (t << w) - t; // w <= 8, fits u32
        let mask_id_r = if tx == 1 || h == ss_v { 1 } else { 0 };
        mask[1][row_and_7][mask_id_r] |= (m_col & 0xff) as u8;
        let mask_id_c = if tx == 1 || w == ss_h { 1 } else { 0 };
        for y in row_and_7..h + row_and_7 {
            mask[0][y][mask_id_c] |= (t & 0xff) as u8;
        }
    } else {
        // skip_inter, TX_4X4
        let t: u32 = 1 << col_and_7;
        let m_col = (t << w) - t; // w <= 8, fits u32
        let t8 = t & WIDE_FILTER_COL_MASK[ss_h];
        let t4 = t - t8;
        for y in row_and_7..h + row_and_7 {
            mask[0][y][2] |= (t4 & 0xff) as u8;
            mask[0][y][1] |= (t8 & 0xff) as u8;
        }
        let id = 2 - usize::from((row_and_7 as u32) & WIDE_FILTER_ROW_MASK[ss_v] != 0);
        mask[1][row_and_7][id] |= (m_col & 0xff) as u8;
    }
}

/// Run the loop filter over one superblock (reference `ff_vp9_loopfilter_sb`
/// with `filter_plane_cols`/`filter_plane_rows`, restructured to iterate each
/// 8px mask segment independently — exact, because the reference filter math
/// is per-segment independent too).
pub fn loopfilter_sb(
    frame: &mut FrameData,
    sf: &SbFilter,
    luts: &FilterLut,
    sb_row: usize,
    sb_col: usize,
    ss_h: u32,
    ss_v: u32,
) {
    let sb_px = sb_col * 64;
    let sb_py = sb_row * 64;

    // Y plane: col edges then row edges
    filter_plane(frame, sf, luts, 0, sb_px, sb_py, 8, 8, true);
    // UV planes (4:2:0 geometry)
    if ss_h != 0 || ss_v != 0 {
        for chroma in 1..=2 {
            filter_plane(
                frame,
                sf,
                luts,
                chroma,
                sb_px >> ss_h,
                sb_py >> ss_v,
                8 >> ss_h,
                8 >> ss_v,
                false,
            );
        }
    }
}

/// Per-plane filter driver: iterates every 8px mask segment of the
/// superblock and applies the widest marked filter class per segment.
#[allow(clippy::too_many_arguments)]
fn filter_plane(
    frame: &mut FrameData,
    sf: &SbFilter,
    luts: &FilterLut,
    plane: usize,
    sb_px: usize,
    sb_py: usize,
    step_x: usize,
    step_y: usize,
    is_luma: bool,
) {
    let stride = if is_luma {
        frame.stride
    } else {
        frame.stride >> 1
    };
    let (px_w, px_h) = if is_luma {
        (frame.mi_cols * 8, frame.mi_rows * 8)
    } else {
        (frame.mi_cols * 4, frame.mi_rows * 4)
    };
    let plane_mask = usize::from(!is_luma);

    // Collect the filter operations first so the plane borrow can be taken
    // once per operation (Y mutates in place; UV via copy-out/filter/copy-in).
    for dir in 0..2 {
        for my in 0..8usize {
            for mx in 0..8usize {
                let classes = sf.mask[plane_mask][dir][my];
                let bit = 1u8 << mx;
                let wd = if classes[0] & bit != 0 {
                    16
                } else if classes[1] & bit != 0 {
                    8
                } else if classes[2] & bit != 0 {
                    4
                } else {
                    continue;
                };
                let lvl = sf.level[my * 8 + mx];
                if lvl == 0 {
                    continue;
                }
                let e = luts.mblim[lvl as usize] as i32;
                let i = luts.lim[lvl as usize] as i32;
                let h = (lvl >> 4) as i32;

                if dir == 0 {
                    // vertical edge at column `edge_x`, spanning rows
                    // [y0, y0 + step_y)
                    let edge_x = sb_px + mx * step_x;
                    if edge_x == 0 {
                        continue; // frame's left boundary
                    }
                    let y0 = sb_py + my * step_y;
                    if y0 >= px_h {
                        continue;
                    }
                    if is_luma {
                        let off = y0 * stride + edge_x;
                        loop_filter_edge(&mut frame.y, off, stride, 1, e, i, h, wd);
                    } else {
                        let off = y0 * stride + edge_x;
                        let mut tmp = if plane == 1 {
                            frame.v.clone()
                        } else {
                            frame.u.clone()
                        };
                        loop_filter_edge(&mut tmp, off, stride, 1, e, i, h, wd);
                        if plane == 1 {
                            frame.v = tmp;
                        } else {
                            frame.u = tmp;
                        }
                    }
                } else {
                    // horizontal edge at row `edge_y`, spanning columns
                    // [x0, x0 + step_x)
                    let edge_y = sb_py + my * step_y;
                    if edge_y == 0 {
                        continue; // frame's top boundary
                    }
                    let x0 = sb_px + mx * step_x;
                    if x0 >= px_w {
                        continue;
                    }
                    if is_luma {
                        let off = edge_y * stride + x0;
                        loop_filter_edge(&mut frame.y, off, 1, stride, e, i, h, wd);
                    } else {
                        let off = edge_y * stride + x0;
                        let mut tmp = if plane == 1 {
                            frame.v.clone()
                        } else {
                            frame.u.clone()
                        };
                        loop_filter_edge(&mut tmp, off, 1, stride, e, i, h, wd);
                        if plane == 1 {
                            frame.v = tmp;
                        } else {
                            frame.u = tmp;
                        }
                    }
                }
            }
        }
    }
}
