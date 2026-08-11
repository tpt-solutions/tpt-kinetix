//! H.264 motion-compensated sample interpolation (§8.4.2.2).
//!
//! Luma uses the 6-tap filter `(1,-5,20,20,-5,1)` for half-pel positions
//! (§8.4.2.2.1): `b` horizontally, `h` vertically, `j` in both directions
//! (filtering the *unclipped* `b1` values). Quarter-pel positions are the
//! rounded average of the two neighbouring clipped integer/half-pel samples.
//!
//! Chroma (4:2:0) uses bilinear interpolation in 1/8-chroma-sample steps
//! (§8.4.2.2.2) with `pred = (xa*ya*A + fx*ya*B + xa*fy*C + fx*fy*D + 32) >> 6`.
//! The chroma motion vector in 1/8 units equals the luma MV numerically
//! (1/4-luma-sample == 1/8-chroma-sample for 4:2:0).
//!
//! All reads outside the picture are edge-clamped, matching the spec's rule
//! that unavailable reference samples are replaced by the closest in-picture
//! sample (and ffmpeg's edge padding).

/// Clip an intermediate interpolation value to the 8-bit sample range.
#[inline]
fn clip(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Edge-clamped single sample read from `plane` (`pw`/`ph` = plane dimensions,
/// `stride` = row pitch in bytes).
#[inline]
fn get(plane: &[u8], stride: usize, pw: usize, ph: usize, x: i32, y: i32) -> u8 {
    let x = x.clamp(0, pw as i32 - 1);
    let y = y.clamp(0, ph as i32 - 1);
    plane[y as usize * stride + x as usize]
}

/// The 6-tap low-pass coefficients (§8.4.2.2.1): `(1,-5,20,20,-5,1)`.
const TAP: [i32; 6] = [1, -5, 20, 20, -5, 1];

/// Unclipped horizontal 6-tap sum `b1` at integer position `(x, y)`.
#[inline]
fn tap_h(plane: &[u8], stride: usize, pw: usize, ph: usize, x: i32, y: i32) -> i32 {
    let mut s = 0;
    for (k, &c) in TAP.iter().enumerate() {
        s += c * get(plane, stride, pw, ph, x + k as i32 - 2, y) as i32;
    }
    s
}

/// Unclipped vertical 6-tap sum `h1` at integer position `(x, y)`.
#[inline]
fn tap_v(plane: &[u8], stride: usize, pw: usize, ph: usize, x: i32, y: i32) -> i32 {
    let mut s = 0;
    for (k, &c) in TAP.iter().enumerate() {
        s += c * get(plane, stride, pw, ph, x, y + k as i32 - 2) as i32;
    }
    s
}

/// Half-pel sample `b` (horizontal, §8.4.2.2.1): `clip((b1 + 16) >> 5)`.
#[inline]
fn half_h(plane: &[u8], stride: usize, pw: usize, ph: usize, x: i32, y: i32) -> u8 {
    clip((tap_h(plane, stride, pw, ph, x, y) + 16) >> 5)
}

/// Half-pel sample `h` (vertical, §8.4.2.2.1): `clip((h1 + 16) >> 5)`.
#[inline]
fn half_v(plane: &[u8], stride: usize, pw: usize, ph: usize, x: i32, y: i32) -> u8 {
    clip((tap_v(plane, stride, pw, ph, x, y) + 16) >> 5)
}

/// Half-pel sample `j` (both directions, §8.4.2.2.1). Computed as the vertical
/// 6-tap over the *unclipped* horizontal sums `b1`; the two filter stages
/// require a combined `+512 >> 10` shift and clip:
/// `j = clip((Σ tap_k * b1(x, y+k-2) + 512) >> 10)`.
#[inline]
fn half_j(plane: &[u8], stride: usize, pw: usize, ph: usize, x: i32, y: i32) -> u8 {
    let mut s = 0;
    for (k, &c) in TAP.iter().enumerate() {
        s += c * tap_h(plane, stride, pw, ph, x, y + k as i32 - 2);
    }
    clip((s + 512) >> 10)
}

/// Rounded average of two clipped samples: `(a + b + 1) >> 1` (§8.4.2.2.1).
#[inline]
fn avg(a: u8, b: u8) -> u8 {
    ((a as u16 + b as u16 + 1) >> 1) as u8
}

/// Predict a single luma sample at fractional position `(x, y)` in 1/4-pel
/// units (§8.4.2.2.1). `pw`/`ph` are the luma reference plane dimensions.
pub fn pred_luma(plane: &[u8], stride: usize, pw: usize, ph: usize, x: i32, y: i32) -> u8 {
    let (x0, fx) = split(x);
    let (y0, fy) = split(y);
    let g = |dx: i32, dy: i32| get(plane, stride, pw, ph, x0 + dx, y0 + dy);
    let b = |dx: i32| half_h(plane, stride, pw, ph, x0 + dx, y0);
    let hv = |dy: i32| half_v(plane, stride, pw, ph, x0, y0 + dy);
    // half_v at (x0+dx, y0+dy) — needed for the (3,1) and (3,3) positions.
    let hv_x = |dx: i32, dy: i32| half_v(plane, stride, pw, ph, x0 + dx, y0 + dy);
    let j = |dx: i32, dy: i32| half_j(plane, stride, pw, ph, x0 + dx, y0 + dy);

    match (fx, fy) {
        (0, 0) => g(0, 0),
        (1, 0) => avg(g(0, 0), b(0)),
        (2, 0) => b(0),
        (3, 0) => avg(b(0), g(1, 0)),
        (0, 1) => avg(g(0, 0), hv(0)),
        (1, 1) => avg(b(0), hv(0)),
        (2, 1) => avg(b(0), j(0, 0)),
        // i = avg(b(x0,y0), h(x0+1,y0)) — §8.4.2.2.1: nearest half-pars to (3/4,1/4)
        // are the horizontal half-pel at (x0,y0) and the vertical half-pel at (x0+1,y0).
        (3, 1) => avg(b(0), hv_x(1, 0)),
        (0, 2) => hv(0),
        (1, 2) => avg(hv(0), j(0, 0)),
        (2, 2) => j(0, 0),
        (3, 2) => avg(j(0, 0), g(1, 0)),
        (0, 3) => avg(hv(0), g(0, 1)),
        (1, 3) => avg(hv(0), j(0, 1)),
        (2, 3) => avg(j(0, 0), g(1, 1)),
        // r = avg(j(x0,y0), G(x0+1,y0+1)) — §8.4.2.2.1: midpoint of diagonal half-pel
        // and the integer to the lower-right lands at (3/4,3/4).
        (3, 3) => avg(j(0, 0), g(1, 1)),
        _ => unreachable!(),
    }
}

/// Integer part of `mv` in 1/4-pel units (floor division) and its fractional
/// part in 0..4.
#[inline]
fn split(mv: i32) -> (i32, i32) {
    let i = mv.div_euclid(4);
    (i, mv - 4 * i)
}

/// Interpolate a `w`×`h` luma block at integer pixel origin `(x, y)` with
/// motion vector `(mvx, mvy)` (1/4-luma-sample units), writing rows of
/// `dst_stride` into `dst`. `pw`/`ph` are the luma reference plane dimensions.
#[allow(clippy::too_many_arguments)]
pub fn interpolate_luma(
    dst: &mut [u8],
    dst_stride: usize,
    plane: &[u8],
    stride: usize,
    pw: usize,
    ph: usize,
    x: i32,
    y: i32,
    mvx: i32,
    mvy: i32,
    w: usize,
    h: usize,
) {
    for row in 0..h {
        for col in 0..w {
            let px = 4 * x + 4 * col as i32 + mvx;
            let py = 4 * y + 4 * row as i32 + mvy;
            dst[row * dst_stride + col] = pred_luma(plane, stride, pw, ph, px, py);
        }
    }
}

/// Interpolate a `w`×`h` chroma block (§8.4.2.2.2). `(x, y)` is the block
/// origin in chroma samples; `(cmvx, cmvy)` are the chroma motion-vector
/// components in 1/8-chroma-sample units (equal to the luma MV numerically
/// for 4:2:0). `pw`/`ph` are the chroma reference plane dimensions. Writes
/// rows of `dst_stride` into `dst`.
#[allow(clippy::too_many_arguments)]
pub fn interpolate_chroma(
    dst: &mut [u8],
    dst_stride: usize,
    plane: &[u8],
    stride: usize,
    pw: usize,
    ph: usize,
    x: i32,
    y: i32,
    cmvx: i32,
    cmvy: i32,
    w: usize,
    h: usize,
) {
    let bx = x + cmvx.div_euclid(8);
    let by = y + cmvy.div_euclid(8);
    let fx = cmvx.rem_euclid(8);
    let fy = cmvy.rem_euclid(8);
    let xa = 8 - fx;
    let ya = 8 - fy;
    for row in 0..h {
        for col in 0..w {
            let px = bx + col as i32;
            let py = by + row as i32;
            let a = get(plane, stride, pw, ph, px, py) as i32;
            let b = get(plane, stride, pw, ph, px + 1, py) as i32;
            let c = get(plane, stride, pw, ph, px, py + 1) as i32;
            let d = get(plane, stride, pw, ph, px + 1, py + 1) as i32;
            let v = xa * ya * a + fx * ya * b + xa * fy * c + fx * fy * d + 32;
            dst[row * dst_stride + col] = clip(v >> 6);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane_8x8(v: u8) -> ([u8; 64], [u8; 16]) {
        ([v; 64], [v; 16])
    }

    /// A ramp plane: luma 8x8, row-major value = y*8 + x.
    fn ramp_luma() -> ([u8; 64], [u8; 16]) {
        let mut l = [0u8; 64];
        let mut c = [0u8; 16];
        for y in 0..8 {
            for x in 0..8 {
                l[y * 8 + x] = (y * 8 + x) as u8;
            }
        }
        for y in 0..4 {
            for x in 0..4 {
                c[y * 4 + x] = (y * 8 + x) as u8;
            }
        }
        (l, c)
    }

    #[test]
    fn integer_motion_copies_samples() {
        let (l, _) = ramp_luma();
        let mut dst = [0u8; 4];
        interpolate_luma(&mut dst, 1, &l, 8, 8, 8, 0, 0, 0, 0, 4, 1);
        // Pixel (0,0) = 0, (1,0) = 1, ...
        assert_eq!(dst, [0, 1, 2, 3]);

        let mut dst = [0u8; 4];
        interpolate_luma(&mut dst, 1, &l, 8, 8, 8, 2, 0, 0, 0, 4, 1);
        // Pixel (2,0) = 2, (2,1) = 3, ...
        assert_eq!(dst, [2, 3, 4, 5]);
    }

    #[test]
    fn horizontal_half_pel_six_tap() {
        // 100s at cols 2,3 keep the filter taps clear of the clamped left edge.
        // b at base x0=2 (px=10, fx=2): taps cols 0..5 = 0 0 100 100 0 0
        // b1 = 1*0 -5*0 +20*100 +20*100 -5*0 +1*0 = 4000; (4000+16)>>5 = 125.
        let mut l = [0u8; 64];
        l[2] = 100;
        l[3] = 100;
        let mut dst = [0u8; 1];
        interpolate_luma(&mut dst, 1, &l, 8, 8, 8, 0, 0, 10, 0, 1, 1);
        assert_eq!(dst[0], 125);
    }

    #[test]
    fn vertical_half_pel_six_tap() {
        // 100s at rows 2,3 keep the filter taps clear of the clamped top edge.
        // h at base y0=2 (py=10, fy=2): taps rows 0..5 = 0 0 100 100 0 0
        // h1 = 1*0 -5*0 +20*100 +20*100 -5*0 +1*0 = 4000; (4000+16)>>5 = 125.
        let mut l = [0u8; 64];
        l[16] = 100;
        l[24] = 100;
        let mut dst = [0u8; 1];
        interpolate_luma(&mut dst, 1, &l, 8, 8, 8, 0, 0, 0, 10, 1, 1);
        assert_eq!(dst[0], 125);
    }

    #[test]
    fn both_half_pel_uses_unclipped_horizontal_sums() {
        // Constant plane -> every interpolation yields the constant.
        let (l, _) = plane_8x8(200);
        let mut dst = [0u8; 4];
        interpolate_luma(&mut dst, 1, &l, 8, 8, 8, 0, 0, 2, 2, 4, 1);
        assert_eq!(dst, [200; 4]);

        // j at integer base (2,2) of a step edge: only row 0 holds the 100s.
        // b1(2,0) uses cols 0..5 = 100 100 0 0 0 0
        //   = 1*100 -5*100 +20*0 +20*0 -5*0 +1*0 = -400.
        // Rows 1..5 have all-zero b1, so:
        // j1 = 1*(-400) -5*0 +20*0 +20*0 -5*0 +1*0 = -400
        // j = clip((-400+512)>>10) = clip(112>>10) = 0.
        let mut l = [0u8; 64];
        l[0] = 100;
        l[1] = 100;
        let mut dst = [0u8; 1];
        interpolate_luma(&mut dst, 1, &l, 8, 8, 8, 0, 0, 10, 10, 1, 1);
        assert_eq!(dst[0], 0);
    }

    #[test]
    fn quarter_pel_averages_round() {
        let (l, _) = ramp_luma();
        // a at (0,0) with fx=1: (int(0,0) + b(0,0) + 1)>>1.
        // b(0,0): cols -2..3 clamped: 0 0 0 1 2 3 -> 1*0 -5*0 +20*0 +20*1 -5*2 +1*3
        //   = 20 - 10 + 3 = 13; (13+16)>>5 = 0. So a = (0 + 0 + 1)>>1 = 0.
        let mut dst = [0u8; 1];
        interpolate_luma(&mut dst, 1, &l, 8, 8, 8, 0, 0, 1, 0, 1, 1);
        assert_eq!(dst[0], 0);

        // c at (0,0) with fx=3: (b(0,0) + int(1,0) + 1)>>1 = (0 + 1 + 1)>>1 = 1.
        let mut dst = [0u8; 1];
        interpolate_luma(&mut dst, 1, &l, 8, 8, 8, 0, 0, 3, 0, 1, 1);
        assert_eq!(dst[0], 1);
    }

    #[test]
    fn negative_motion_floors_to_integer_part() {
        let (l, _) = ramp_luma();
        // MV (-1, 0): 1/4-pel left. Integer base -1, frac 3.
        // Pixel (0,0): c at (-1,0) = (b(-1,0) + int(0,0) + 1)>>1.
        // b(-1,0): cols -3..2 clamped to 0 (left edge): all 0 -> b=0.
        // So value = (0 + 0 + 1)>>1 = 0.
        let mut dst = [0u8; 1];
        interpolate_luma(&mut dst, 1, &l, 8, 8, 8, 0, 0, -1, 0, 1, 1);
        assert_eq!(dst[0], 0);
    }

    #[test]
    fn edge_clamp_keeps_bounds() {
        let (l, _) = ramp_luma();
        // MV pushing the window far past the right edge -> clamped to last col.
        let mut dst = [0u8; 2];
        interpolate_luma(&mut dst, 1, &l, 8, 8, 8, 0, 0, 40, 0, 2, 1);
        assert_eq!(dst, [7, 7]);
        // MV (10, 0) -> x0=2, fx=2: b(2,0) taps cols 0..5 = 0 1 2 3 4 5
        //   = 1*0 -5*1 +20*2 +20*3 -5*4 +1*5 = -5+40+60-20+5 = 80; (80+16)>>5 = 3.
        // Second column: px=4+10=14 -> x0=3, fx=2: taps 1..6 = 1 2 3 4 5 6
        //   = 1-10+60+80-25+6 = 112; (112+16)>>5 = 4.
        let mut dst = [0u8; 2];
        interpolate_luma(&mut dst, 1, &l, 8, 8, 8, 0, 0, 10, 0, 2, 1);
        assert_eq!(dst, [3, 4]);
    }

    #[test]
    fn chroma_integer_motion_copies() {
        let (_, c) = ramp_luma();
        let mut dst = [0u8; 4];
        interpolate_chroma(&mut dst, 1, &c, 4, 4, 4, 0, 0, 0, 0, 4, 1);
        // Chroma values: row 0 = 0 1 2 3.
        assert_eq!(dst, [0, 1, 2, 3]);
    }

    #[test]
    fn chroma_half_pel_bilinear() {
        let (_, c) = ramp_luma();
        // cmvx=4 (1/2 chroma sample): fx=4. A=0, B=1:
        // (4*0 + 4*1 + 32)>>6 = 36>>6 = 0. Hmm: xa=4, fx=4: v = 4*8*0 + 4*8*1 + 32
        // wait: v = xa*ya*A + fx*ya*B + xa*fy*C + fx*fy*D + 32, fy=0, ya=8.
        // v = 4*8*0 + 4*8*1 + 0 + 0 + 32 = 32+32 = 64; 64>>6 = 1.
        let mut dst = [0u8; 1];
        interpolate_chroma(&mut dst, 1, &c, 4, 4, 4, 0, 0, 4, 0, 1, 1);
        assert_eq!(dst[0], 1);
    }

    /// (3,1) = i = avg(j(x,y), b(x+1,y))  [§8.4.2.2.1]
    /// (3,3) = r = avg(j(x+1,y), j(x,y+1))  [§8.4.2.2.1]
    /// These two positions were previously wrong (b and j swapped / wrong offsets).
    #[test]
    fn quarter_pel_positions_3_1_and_3_3() {
        let (l, _) = ramp_luma();

        // (3,1): i = avg(j(x0,y0), b(x0+1,y0)).
        // At base (0,0): j(0,0) = half_j(0,0), b(1,0) = half_h(1,0).
        let mut j00 = [0u8; 1];
        interpolate_luma(&mut j00, 1, &l, 8, 8, 8, 0, 0, 2, 2, 1, 1); // (fx=2,fy=2) -> j(0,0)
        let mut b10 = [0u8; 1];
        interpolate_luma(&mut b10, 1, &l, 8, 8, 8, 0, 0, 6, 0, 1, 1); // (fx=2 but base x=1,fy=0)
        // b(x0+1,y0): px = 4*(x+1)+2 = 6 when x=0. Already encoded as mv=(6,0).
        let mut got31 = [0u8; 1];
        interpolate_luma(&mut got31, 1, &l, 8, 8, 8, 0, 0, 3, 1, 1, 1); // (fx=3,fy=1)
        assert_eq!(
            got31[0],
            ((j00[0] as u16 + b10[0] as u16 + 1) >> 1) as u8,
            "(3,1) must be avg(j(x0,y0), b(x0+1,y0))"
        );

        // (3,3): r = avg(j(x0+1,y0), j(x0,y0+1)).
        let mut j10 = [0u8; 1];
        interpolate_luma(&mut j10, 1, &l, 8, 8, 8, 0, 0, 6, 2, 1, 1); // j(x0+1,y0): px=6,py=2
        let mut j01 = [0u8; 1];
        interpolate_luma(&mut j01, 1, &l, 8, 8, 8, 0, 0, 2, 6, 1, 1); // j(x0,y0+1): px=2,py=6
        let mut got33 = [0u8; 1];
        interpolate_luma(&mut got33, 1, &l, 8, 8, 8, 0, 0, 3, 3, 1, 1); // (fx=3,fy=3)
        assert_eq!(
            got33[0],
            ((j10[0] as u16 + j01[0] as u16 + 1) >> 1) as u8,
            "(3,3) must be avg(j(x0+1,y0), j(x0,y0+1))"
        );
    }

    /// Spec quarter-sample positions (§8.4.2.2.1): the three diagonal quarter
    /// samples (1,3)/(2,3)/(3,2) average against an *integer* reference sample,
    /// not another interpolated sample. With a ramp plane the interpolated and
    /// integer samples are distinct, so recombining the documented brackets
    /// must reproduce the decoder output exactly.
    #[test]
    fn quarter_pel_positions_match_spec_table() {
        let (l, _) = ramp_luma();

        // (3,2): avg(J(x,y), G(x+1,y)).
        let mut j00 = [0u8; 1];
        interpolate_luma(&mut j00, 1, &l, 8, 8, 8, 0, 0, 2, 2, 1, 1); // mv (2,2) -> j(0,0)
        let mut g10 = [0u8; 1];
        interpolate_luma(&mut g10, 1, &l, 8, 8, 8, 0, 0, 4, 0, 1, 1); // mv (4,0) -> G(1,0)
        let mut got = [0u8; 1];
        interpolate_luma(&mut got, 1, &l, 8, 8, 8, 0, 0, 3, 2, 1, 1); // mv (3,2) -> (3,2)
        assert_eq!(got[0], ((j00[0] as i32 + g10[0] as i32 + 1) >> 1) as u8);

        // (2,3): avg(J(x,y), G(x+1,y+1)).
        let mut g11 = [0u8; 1];
        interpolate_luma(&mut g11, 1, &l, 8, 8, 8, 0, 0, 4, 4, 1, 1); // mv (4,4) -> G(1,1)
        let mut got23 = [0u8; 1];
        interpolate_luma(&mut got23, 1, &l, 8, 8, 8, 0, 0, 2, 3, 1, 1); // mv (2,3) -> (2,3)
        assert_eq!(got23[0], ((j00[0] as i32 + g11[0] as i32 + 1) >> 1) as u8);

        // (1,3): avg(H(x,y), J(x,y+1)).
        let mut h00 = [0u8; 1];
        interpolate_luma(&mut h00, 1, &l, 8, 8, 8, 0, 0, 0, 2, 1, 1); // mv (0,2) -> H(0,0)
        let mut j01 = [0u8; 1];
        interpolate_luma(&mut j01, 1, &l, 8, 8, 8, 0, 0, 2, 6, 1, 1); // mv (2,6) -> J(0,1)
        let mut got13 = [0u8; 1];
        interpolate_luma(&mut got13, 1, &l, 8, 8, 8, 0, 0, 1, 3, 1, 1); // mv (1,3) -> (1,3)
        assert_eq!(got13[0], ((h00[0] as i32 + j01[0] as i32 + 1) >> 1) as u8);
    }

    #[test]
    fn chroma_quarter_pel_bilinear() {
        let (_, c) = ramp_luma();
        // cmvx=2, cmvy=2: fx=fy=2. A=0, B=1, C=8, D=9 (cols 0/1, rows 0/1).
        // v = 6*6*0 + 2*6*1 + 6*2*8 + 2*2*9 + 32 = 0 + 12 + 96 + 36 + 32 = 176
        // 176>>6 = 2.
        let mut dst = [0u8; 1];
        interpolate_chroma(&mut dst, 1, &c, 4, 4, 4, 0, 0, 2, 2, 1, 1);
        assert_eq!(dst[0], 2);
    }
}
