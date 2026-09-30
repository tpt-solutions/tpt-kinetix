//! AV1 film-grain synthesis (spec §7.18.3): parameters, grain-template
//! generation, scaling LUTs and the 32×32 block noise-application pass.
//!
//! Grain is applied to the *output* picture only; reference frames stay
//! grain-free. The arithmetic follows dav1d's `filmgrain_tmpl.c` /
//! `fg_apply_tmpl.c`, which the conformance references are decoded with.

use crate::film_grain_table::GAUSSIAN_SEQUENCE;

const GRAIN_W: usize = 82;
const GRAIN_H: usize = 73;
const SUB_GRAIN_W: usize = 44;
const SUB_GRAIN_H: usize = 38;
const BLOCK: usize = 32;

/// `film_grain_params()` (§5.9.30) with the derived values the synthesis
/// process needs (`grain_scaling_minus_8` etc. are already un-biased).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilmGrainParams {
    /// `apply_grain`: grain is synthesised for this (shown) frame.
    pub apply_grain: bool,
    pub grain_seed: u16,
    pub point_y: Vec<(u8, u8)>,
    pub chroma_scaling_from_luma: bool,
    pub point_cb: Vec<(u8, u8)>,
    pub point_cr: Vec<(u8, u8)>,
    /// `grain_scaling_minus_8 + 8`.
    pub scaling_shift: u8,
    pub ar_coeff_lag: u8,
    /// AR coefficients with the +128 bias removed.
    pub ar_coeffs_y: Vec<i8>,
    pub ar_coeffs_cb: Vec<i8>,
    pub ar_coeffs_cr: Vec<i8>,
    /// `ar_coeff_shift_minus_6 + 6`.
    pub ar_coeff_shift: u8,
    pub grain_scale_shift: u8,
    /// `cb_mult - 128`, `cr_mult - 128`.
    pub uv_mult: [i32; 2],
    /// `cb_luma_mult - 128`, `cr_luma_mult - 128`.
    pub uv_luma_mult: [i32; 2],
    /// `cb_offset - 256`, `cr_offset - 256`.
    pub uv_offset: [i32; 2],
    pub overlap_flag: bool,
    pub clip_to_restricted_range: bool,
}

fn random_number(bits: u32, state: &mut u32) -> i32 {
    let r = *state;
    let bit = ((r) ^ (r >> 1) ^ (r >> 3) ^ (r >> 12)) & 1;
    *state = (r >> 1) | (bit << 15);
    ((*state >> (16 - bits)) & ((1 << bits) - 1)) as i32
}

fn round2(x: i32, shift: u32) -> i32 {
    (x + ((1i32 << shift) >> 1)) >> shift
}

fn generate_grain_y(p: &FilmGrainParams, bit_depth: u32) -> Vec<i32> {
    let bd8 = bit_depth - 8;
    let mut seed = u32::from(p.grain_seed);
    let shift = 4 - bd8 + u32::from(p.grain_scale_shift);
    let ctr = 128i32 << bd8;
    let (gmin, gmax) = (-ctr, ctr - 1);
    let mut buf = vec![0i32; (GRAIN_H + 1) * GRAIN_W];
    for y in 0..GRAIN_H {
        for x in 0..GRAIN_W {
            let v = random_number(11, &mut seed) as usize;
            buf[y * GRAIN_W + x] = round2(i32::from(GAUSSIAN_SEQUENCE[v]), shift);
        }
    }
    let lag = p.ar_coeff_lag as i32;
    for y in 3..GRAIN_H {
        for x in 3..GRAIN_W - 3 {
            let mut sum = 0i32;
            let mut ci = 0usize;
            'outer: for dy in -lag..=0 {
                for dx in -lag..=lag {
                    if dx == 0 && dy == 0 {
                        break 'outer;
                    }
                    let yy = (y as i32 + dy) as usize;
                    let xx = (x as i32 + dx) as usize;
                    sum += i32::from(p.ar_coeffs_y[ci]) * buf[yy * GRAIN_W + xx];
                    ci += 1;
                }
            }
            let g = buf[y * GRAIN_W + x] + round2(sum, u32::from(p.ar_coeff_shift));
            buf[y * GRAIN_W + x] = g.clamp(gmin, gmax);
        }
    }
    buf
}

fn generate_grain_uv(
    p: &FilmGrainParams,
    buf_y: &[i32],
    uv: usize,
    ssx: usize,
    ssy: usize,
    bit_depth: u32,
) -> Vec<i32> {
    let bd8 = bit_depth - 8;
    let mut seed = u32::from(p.grain_seed) ^ if uv == 1 { 0x49d8 } else { 0xb524 };
    let shift = 4 - bd8 + u32::from(p.grain_scale_shift);
    let ctr = 128i32 << bd8;
    let (gmin, gmax) = (-ctr, ctr - 1);
    let cw = if ssx == 1 { SUB_GRAIN_W } else { GRAIN_W };
    let ch = if ssy == 1 { SUB_GRAIN_H } else { GRAIN_H };
    let mut buf = vec![0i32; (GRAIN_H + 1) * GRAIN_W];
    for y in 0..ch {
        for x in 0..cw {
            let v = random_number(11, &mut seed) as usize;
            buf[y * GRAIN_W + x] = round2(i32::from(GAUSSIAN_SEQUENCE[v]), shift);
        }
    }
    let coeffs = if uv == 0 {
        &p.ar_coeffs_cb
    } else {
        &p.ar_coeffs_cr
    };
    let lag = p.ar_coeff_lag as i32;
    for y in 3..ch {
        for x in 3..cw - 3 {
            let mut sum = 0i32;
            let mut ci = 0usize;
            'outer: for dy in -lag..=0 {
                for dx in -lag..=lag {
                    if dx == 0 && dy == 0 {
                        if p.point_y.is_empty() {
                            break 'outer;
                        }
                        let lx = ((x - 3) << ssx) + 3;
                        let ly = ((y - 3) << ssy) + 3;
                        let mut luma = 0i32;
                        for i in 0..=ssy {
                            for j in 0..=ssx {
                                luma += buf_y[(ly + i) * GRAIN_W + lx + j];
                            }
                        }
                        luma = round2(luma, (ssx + ssy) as u32);
                        sum += luma * i32::from(coeffs[ci]);
                        break 'outer;
                    }
                    let yy = (y as i32 + dy) as usize;
                    let xx = (x as i32 + dx) as usize;
                    sum += i32::from(coeffs[ci]) * buf[yy * GRAIN_W + xx];
                    ci += 1;
                }
            }
            let g = buf[y * GRAIN_W + x] + round2(sum, u32::from(p.ar_coeff_shift));
            buf[y * GRAIN_W + x] = g.clamp(gmin, gmax);
        }
    }
    buf
}

fn generate_scaling(bit_depth: u32, points: &[(u8, u8)]) -> Vec<u8> {
    let shift_x = bit_depth - 8;
    let size = 1usize << bit_depth;
    let mut scaling = vec![0u8; size];
    if points.is_empty() {
        return scaling;
    }
    let first = (usize::from(points[0].0)) << shift_x;
    scaling[..first].fill(points[0].1);
    for w in points.windows(2) {
        let (bx, by) = (i32::from(w[0].0), i32::from(w[0].1));
        let (ex, ey) = (i32::from(w[1].0), i32::from(w[1].1));
        let dx = ex - bx;
        let dy = ey - by;
        let delta = dy * ((0x10000 + (dx >> 1)) / dx);
        let mut d = 0x8000i32;
        for x in 0..dx {
            scaling[((bx + x) as usize) << shift_x] = (by + (d >> 16)) as u8;
            d += delta;
        }
    }
    let last = points[points.len() - 1];
    let n = usize::from(last.0) << shift_x;
    scaling[n..].fill(last.1);
    if shift_x > 0 {
        let pad = 1usize << shift_x;
        let rnd = (pad >> 1) as i32;
        for w in points.windows(2) {
            let bx = usize::from(w[0].0) << shift_x;
            let ex = usize::from(w[1].0) << shift_x;
            let mut x = 0;
            while x < ex - bx {
                let range = i32::from(scaling[bx + x + pad]) - i32::from(scaling[bx + x]);
                let mut r = rnd;
                for n in 1..pad {
                    r += range;
                    scaling[bx + x + n] = (i32::from(scaling[bx + x]) + (r >> shift_x)) as u8;
                }
                x += pad;
            }
        }
    }
    scaling
}

fn sample_lut(
    lut: &[i32],
    offsets: &[[i32; 2]; 2],
    sub: (usize, usize),
    blk: (usize, usize),
    x: usize,
    y: usize,
) -> i32 {
    let r = offsets[blk.0][blk.1];
    let offx = 3 + (2 >> sub.0) * (3 + (r >> 4)) as usize;
    let offy = 3 + (2 >> sub.1) * (3 + (r & 0xF)) as usize;
    lut[(offy + y + (BLOCK >> sub.1) * blk.1) * GRAIN_W + offx + x + (BLOCK >> sub.0) * blk.0]
}

/// Blend weights `w[sub][i]` for the overlapped rows/columns.
fn overlap_w(sub: usize, i: usize) -> [i32; 2] {
    if sub == 0 {
        [[27, 17], [17, 27]][i]
    } else {
        [23, 22]
    }
}

/// Grain value at block-relative `(x, y)` including the 2-sample overlap
/// blending with the left / upper neighbouring blocks.
#[allow(clippy::too_many_arguments)]
fn grain_at(
    lut: &[i32],
    offsets: &[[i32; 2]; 2],
    sub: (usize, usize),
    xstart: usize,
    ystart: usize,
    x: usize,
    y: usize,
    gmin: i32,
    gmax: i32,
) -> i32 {
    let cur = |bx, by| sample_lut(lut, offsets, sub, (bx, by), x, y);
    let mix =
        |old: i32, new: i32, w: [i32; 2]| round2(old * w[0] + new * w[1], 5).clamp(gmin, gmax);
    if y >= ystart && x >= xstart {
        cur(0, 0)
    } else if y >= ystart {
        mix(cur(1, 0), cur(0, 0), overlap_w(sub.0, x))
    } else if x >= xstart {
        mix(cur(0, 1), cur(0, 0), overlap_w(sub.1, y))
    } else {
        let top = mix(cur(1, 1), cur(0, 1), overlap_w(sub.0, x));
        let left = mix(cur(1, 0), cur(0, 0), overlap_w(sub.0, x));
        mix(top, left, overlap_w(sub.1, y))
    }
}

/// Which plane a block pass is synthesising for.
enum Pass<'a> {
    Luma,
    Chroma {
        uv: usize,
        luma: &'a [u16],
        luma_stride: usize,
        luma_w: usize,
    },
}

/// Noise-add one row of 32×32 (subsampled for chroma) blocks. `dst`/`src`
/// point at the first sample row of this block row; `bh` is its height in
/// this plane's samples, `pw` the plane width.
#[allow(clippy::too_many_arguments)]
fn block_row(
    p: &FilmGrainParams,
    pass: &Pass<'_>,
    dst: &mut [u16],
    src: &[u16],
    stride: usize,
    pw: usize,
    scaling: &[u8],
    lut: &[i32],
    bh: usize,
    row_num: usize,
    sub: (usize, usize),
    bit_depth: u32,
    is_id: bool,
) {
    let bd8 = bit_depth - 8;
    let ctr = 128i32 << bd8;
    let (gmin, gmax) = (-ctr, ctr - 1);
    let pix_max = (1i32 << bit_depth) - 1;
    let (min_v, max_v) = if p.clip_to_restricted_range {
        let hi = match pass {
            Pass::Luma => 235,
            Pass::Chroma { .. } if is_id => 235,
            Pass::Chroma { .. } => 240,
        };
        (16i32 << bd8, hi << bd8)
    } else {
        (0, pix_max)
    };
    let rows = 1 + usize::from(p.overlap_flag && row_num > 0);
    let mut seed = [0u32; 2];
    for (i, s) in seed.iter_mut().enumerate().take(rows) {
        let r = row_num as i32 - i as i32;
        *s = u32::from(p.grain_seed);
        *s ^= (((r * 37 + 178) & 0xFF) as u32) << 8;
        *s ^= ((r * 173 + 105) & 0xFF) as u32;
    }
    let mut offsets = [[0i32; 2]; 2];
    let step = BLOCK >> sub.0;
    let mut bx = 0;
    while bx < pw {
        let bw = step.min(pw - bx);
        if p.overlap_flag && bx > 0 {
            // Row 1 is only read when `rows == 2`, so copying both is safe.
            offsets[1] = offsets[0];
        }
        for (off, s) in offsets[0].iter_mut().zip(seed.iter_mut()).take(rows) {
            *off = random_number(8, s);
        }
        let ystart = if p.overlap_flag && row_num > 0 {
            (2 >> sub.1).min(bh)
        } else {
            0
        };
        let xstart = if p.overlap_flag && bx > 0 {
            (2 >> sub.0).min(bw)
        } else {
            0
        };
        for y in 0..bh {
            for x in 0..bw {
                let grain = grain_at(lut, &offsets, sub, xstart, ystart, x, y, gmin, gmax);
                let idx = y * stride + bx + x;
                let s = i32::from(src[idx]);
                let scale_idx = match pass {
                    Pass::Luma => s,
                    Pass::Chroma {
                        uv,
                        luma,
                        luma_stride,
                        luma_w,
                    } => {
                        let lx = (bx + x) << sub.0;
                        let ly = y << sub.1;
                        let base = ly * luma_stride;
                        let mut avg = i32::from(luma[base + lx.min(luma_w - 1)]);
                        if sub.0 == 1 {
                            avg = (avg + i32::from(luma[base + (lx + 1).min(luma_w - 1)]) + 1) >> 1;
                        }
                        if p.chroma_scaling_from_luma {
                            avg
                        } else {
                            let combined = avg * p.uv_luma_mult[*uv] + s * p.uv_mult[*uv];
                            ((combined >> 6) + (p.uv_offset[*uv] << bd8)).clamp(0, pix_max)
                        }
                    }
                };
                let noise = round2(
                    i32::from(scaling[scale_idx as usize]) * grain,
                    u32::from(p.scaling_shift),
                );
                dst[idx] = (s + noise).clamp(min_v, max_v) as u16;
            }
        }
        bx += step;
    }
}

/// Apply film grain to a decoded picture in place. Planes are dense
/// (`stride == width`); `u`/`v` are `(w + ssx) >> ssx` wide. `is_identity`
/// is `matrix_coefficients == MC_IDENTITY`.
#[allow(clippy::too_many_arguments)]
pub fn apply_film_grain(
    p: &FilmGrainParams,
    bit_depth: u32,
    ssx: usize,
    ssy: usize,
    is_identity: bool,
    w: usize,
    h: usize,
    y: &mut [u16],
    u: &mut [u16],
    v: &mut [u16],
) {
    if !p.apply_grain || w == 0 || h == 0 {
        return;
    }
    let cw = (w + ssx) >> ssx;
    let has_uv = [!p.point_cb.is_empty(), !p.point_cr.is_empty()];
    let csfl = p.chroma_scaling_from_luma;
    let do_uv = csfl || has_uv[0] || has_uv[1];

    let lut_y = generate_grain_y(p, bit_depth);
    let lut_uv = [
        (csfl || has_uv[0]).then(|| generate_grain_uv(p, &lut_y, 0, ssx, ssy, bit_depth)),
        (csfl || has_uv[1]).then(|| generate_grain_uv(p, &lut_y, 1, ssx, ssy, bit_depth)),
    ];
    let scaling_y =
        (!p.point_y.is_empty() || csfl).then(|| generate_scaling(bit_depth, &p.point_y));
    let scaling_uv = [
        has_uv[0].then(|| generate_scaling(bit_depth, &p.point_cb)),
        has_uv[1].then(|| generate_scaling(bit_depth, &p.point_cr)),
    ];

    // Chroma uses the un-grained luma, so keep the input picture.
    let src_y = y.to_vec();
    let src_u = u.to_vec();
    let src_v = v.to_vec();
    let n_rows = h.div_ceil(BLOCK);
    for row in 0..n_rows {
        let bh_luma = BLOCK.min(h - row * BLOCK);
        if !p.point_y.is_empty() {
            let off = row * BLOCK * w;
            block_row(
                p,
                &Pass::Luma,
                &mut y[off..],
                &src_y[off..],
                w,
                w,
                scaling_y.as_deref().unwrap_or(&[]),
                &lut_y,
                bh_luma,
                row,
                (0, 0),
                bit_depth,
                is_identity,
            );
        }
        if !do_uv {
            continue;
        }
        let bh = (bh_luma + ssy) >> ssy;
        let off = ((row * BLOCK) >> ssy) * cw;
        let luma_off = row * BLOCK * w;
        for pl in 0..2 {
            if !(csfl || has_uv[pl]) {
                continue;
            }
            let scaling = if csfl {
                scaling_y.as_deref().unwrap_or(&[])
            } else {
                scaling_uv[pl].as_deref().unwrap_or(&[])
            };
            let (dst, src) = if pl == 0 {
                (&mut u[off..], &src_u[off..])
            } else {
                (&mut v[off..], &src_v[off..])
            };
            block_row(
                p,
                &Pass::Chroma {
                    uv: pl,
                    luma: &src_y[luma_off..],
                    luma_stride: w,
                    luma_w: w,
                },
                dst,
                src,
                cw,
                cw,
                scaling,
                lut_uv[pl].as_deref().unwrap_or(&[]),
                bh,
                row,
                (ssx, ssy),
                bit_depth,
                is_identity,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> FilmGrainParams {
        FilmGrainParams {
            apply_grain: true,
            grain_seed: 1234,
            point_y: vec![(0, 40), (255, 80)],
            scaling_shift: 10,
            ar_coeff_lag: 0,
            ar_coeffs_y: vec![],
            ar_coeffs_cb: vec![0],
            ar_coeffs_cr: vec![0],
            ar_coeff_shift: 7,
            overlap_flag: true,
            ..FilmGrainParams::default()
        }
    }

    #[test]
    fn scaling_lut_interpolates_between_points() {
        let s = generate_scaling(8, &[(0, 0), (255, 255)]);
        assert_eq!(s.len(), 256);
        assert_eq!(s[0], 0);
        assert_eq!(s[255], 255);
        assert!(s[100] >= 99 && s[100] <= 101);
        assert!(generate_scaling(10, &[]).iter().all(|&v| v == 0));
    }

    #[test]
    fn disabled_grain_is_a_no_op() {
        let mut p = params();
        p.apply_grain = false;
        let (mut y, mut u, mut v) = (
            vec![100u16; 64 * 64],
            vec![100u16; 32 * 32],
            vec![100u16; 32 * 32],
        );
        apply_film_grain(&p, 8, 1, 1, false, 64, 64, &mut y, &mut u, &mut v);
        assert!(y.iter().all(|&s| s == 100));
    }

    #[test]
    fn grain_is_deterministic_and_bounded() {
        let p = params();
        let run = || {
            let (mut y, mut u, mut v) = (
                vec![512u16; 70 * 40],
                vec![512u16; 35 * 20],
                vec![512u16; 35 * 20],
            );
            apply_film_grain(&p, 10, 1, 1, false, 70, 40, &mut y, &mut u, &mut v);
            (y, u, v)
        };
        let (a, b) = (run(), run());
        assert_eq!(a, b);
        assert!(
            a.0.iter().any(|&s| s != 512),
            "luma grain must change samples"
        );
        assert!(a.0.iter().all(|&s| s < 1024));
        // No chroma scaling points and no chroma_scaling_from_luma: untouched.
        assert!(a.1.iter().all(|&s| s == 512));
    }
}
