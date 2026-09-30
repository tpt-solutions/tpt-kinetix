//! Superres upscaling -- port of dav1d `mc_tmpl.c` `resize_c` and
//! `decode.c`'s `scale_fac` / `get_upscale_x0`.
//!
//! A superres frame is coded at a horizontally downscaled size
//! (`FrameHeader::width`) and upscaled to `FrameHeader::upscaled_width`
//! between CDEF and loop restoration. The filter is row-independent, so
//! whole-frame row processing matches dav1d's per-sbrow `filter_sbrow_resize`.
//! The same filter also upscales the loop-restoration stripe-boundary rows,
//! which dav1d backs up from the *deblocked* downscaled plane
//! (`dav1d_copy_lpf` -> `backup_lpf` runs `mc.resize` for `lr_backup &&
//! resize`) -- so the pre-CDEF snapshot is upscaled with it too when
//! restoration is active.

use crate::Px;

/// The 64 8-tap superres interpolation sub-filters (dav1d
/// `dav1d_resize_filter`; 8-bit coefficients, `>> 7` normalisation at every
/// bit depth).
pub(super) const RESIZE_FILTER: [[i8; 8]; 64] = [
    [0, 0, 0, -128, 0, 0, 0, 0],
    [0, 0, 1, -128, -2, 1, 0, 0],
    [0, -1, 3, -127, -4, 2, -1, 0],
    [0, -1, 4, -127, -6, 3, -1, 0],
    [0, -2, 6, -126, -8, 3, -1, 0],
    [0, -2, 7, -125, -11, 4, -1, 0],
    [1, -2, 8, -125, -13, 5, -2, 0],
    [1, -3, 9, -124, -15, 6, -2, 0],
    [1, -3, 10, -123, -18, 6, -2, 1],
    [1, -3, 11, -122, -20, 7, -3, 1],
    [1, -4, 12, -121, -22, 8, -3, 1],
    [1, -4, 13, -120, -25, 9, -3, 1],
    [1, -4, 14, -118, -28, 9, -3, 1],
    [1, -4, 15, -117, -30, 10, -4, 1],
    [1, -5, 16, -116, -32, 11, -4, 1],
    [1, -5, 16, -114, -35, 12, -4, 1],
    [1, -5, 17, -112, -38, 12, -4, 1],
    [1, -5, 18, -111, -40, 13, -5, 1],
    [1, -5, 18, -109, -43, 14, -5, 1],
    [1, -6, 19, -107, -45, 14, -5, 1],
    [1, -6, 19, -105, -48, 15, -5, 1],
    [1, -6, 19, -103, -51, 16, -5, 1],
    [1, -6, 20, -101, -53, 16, -6, 1],
    [1, -6, 20, -99, -56, 17, -6, 1],
    [1, -6, 20, -97, -58, 17, -6, 1],
    [1, -6, 20, -95, -61, 18, -6, 1],
    [2, -7, 20, -93, -64, 18, -6, 2],
    [2, -7, 20, -91, -66, 19, -6, 1],
    [2, -7, 20, -88, -69, 19, -6, 1],
    [2, -7, 20, -86, -71, 19, -6, 1],
    [2, -7, 20, -84, -74, 20, -7, 2],
    [2, -7, 20, -81, -76, 20, -7, 1],
    [2, -7, 20, -79, -79, 20, -7, 2],
    [1, -7, 20, -76, -81, 20, -7, 2],
    [2, -7, 20, -74, -84, 20, -7, 2],
    [1, -6, 19, -71, -86, 20, -7, 2],
    [1, -6, 19, -69, -88, 20, -7, 2],
    [1, -6, 19, -66, -91, 20, -7, 2],
    [2, -6, 18, -64, -93, 20, -7, 2],
    [1, -6, 18, -61, -95, 20, -6, 1],
    [1, -6, 17, -58, -97, 20, -6, 1],
    [1, -6, 17, -56, -99, 20, -6, 1],
    [1, -6, 16, -53, -101, 20, -6, 1],
    [1, -5, 16, -51, -103, 19, -6, 1],
    [1, -5, 15, -48, -105, 19, -6, 1],
    [1, -5, 14, -45, -107, 19, -6, 1],
    [1, -5, 14, -43, -109, 18, -5, 1],
    [1, -5, 13, -40, -111, 18, -5, 1],
    [1, -4, 12, -38, -112, 17, -5, 1],
    [1, -4, 12, -35, -114, 16, -5, 1],
    [1, -4, 11, -32, -116, 16, -5, 1],
    [1, -4, 10, -30, -117, 15, -4, 1],
    [1, -3, 9, -28, -118, 14, -4, 1],
    [1, -3, 9, -25, -120, 13, -4, 1],
    [1, -3, 8, -22, -121, 12, -4, 1],
    [1, -3, 7, -20, -122, 11, -3, 1],
    [1, -2, 6, -18, -123, 10, -3, 1],
    [0, -2, 6, -15, -124, 9, -3, 1],
    [0, -2, 5, -13, -125, 8, -2, 1],
    [0, -1, 4, -11, -125, 7, -2, 0],
    [0, -1, 3, -8, -126, 6, -2, 0],
    [0, -1, 3, -6, -127, 4, -1, 0],
    [0, -1, 2, -4, -127, 3, -1, 0],
    [0, 0, 1, -2, -128, 1, 0, 0],
];

/// dav1d `scale_fac(ref_sz, this_sz)` (decode.c): fixed-point phase step per
/// output sample, in 14 fractional bits.
pub(super) fn scale_step(in_w: usize, out_w: usize) -> i32 {
    ((((in_w as i64) << 14) + (out_w as i64) / 2) / (out_w as i64)) as i32
}

/// dav1d `get_upscale_x0` (decode.c): the phase of the first output sample.
pub(super) fn upscale_x0(in_w: usize, out_w: usize, step: i32) -> i32 {
    let (in_w, out_w) = (in_w as i64, out_w as i64);
    let step = step as i64;
    let err = out_w * step - (in_w << 14);
    let x0 = (-((out_w - in_w) << 13) + (out_w >> 1)) / out_w + 128 - err / 2;
    (x0 & 0x3fff) as i32
}

/// Upscale one row from `src` (whose first `src_w` samples are the source
/// grid -- dav1d passes the padded grid width `4 * f->bw` here, and the
/// filter clamps reads to `[0, src_w - 1]`) into `dst[0..dst_w]`.
pub(super) fn upscale_row(
    dst: &mut [Px],
    src: &[Px],
    src_w: usize,
    dst_w: usize,
    dx: i32,
    mx0: i32,
    pix_max: i32,
) {
    let mut mx = mx0;
    let mut src_x: i32 = -1;
    let last = src_w as i32 - 1;
    for d in dst[..dst_w].iter_mut() {
        let f = &RESIZE_FILTER[(mx >> 8) as usize];
        let mut sum = 0i32;
        for (k, tap) in f.iter().enumerate() {
            let sx = (src_x - 3 + k as i32).clamp(0, last) as usize;
            sum += *tap as i32 * src[sx] as i32;
        }
        // dav1d `resize_c` negates the accumulated sum: the stored table is
        // the negated filter (its row 0 is `-128` at the centre tap).
        *d = ((-sum + 64) >> 7).clamp(0, pix_max) as Px;
        mx += dx;
        src_x += mx >> 14;
        mx &= 0x3fff;
    }
}

/// Upscale a whole plane row by row: `src` has `src_stride`-strided rows of
/// `src_w` valid grid samples; `dst` has `dst_stride`-strided rows; the first
/// `h` rows are filtered.
#[allow(clippy::too_many_arguments)]
pub(super) fn upscale_plane(
    dst: &mut [Px],
    dst_stride: usize,
    src: &[Px],
    src_stride: usize,
    src_w: usize,
    dst_w: usize,
    h: usize,
    dx: i32,
    mx0: i32,
    pix_max: i32,
) {
    for row in 0..h {
        upscale_row(
            &mut dst[row * dst_stride..],
            &src[row * src_stride..],
            src_w,
            dst_w,
            dx,
            mx0,
            pix_max,
        );
    }
}
