use kinetix_core::{frame::VideoFrame, pixel_format::PixelFormat};

/// Returns the byte offset and size of each plane for a YUV420p frame.
///
/// Returns `None` if the frame's pixel format is not `Yuv420p` or the data
/// buffer is too small.
fn yuv420p_planes(frame: &VideoFrame) -> Option<(usize, usize, usize, usize, usize, usize)> {
    if frame.pixel_format != PixelFormat::Yuv420p {
        return None;
    }
    let w = frame.width as usize;
    let h = frame.height as usize;
    let cw = (w + 1) / 2;
    let ch = (h + 1) / 2;
    let y_size = w * h;
    let cb_size = cw * ch;
    let cr_size = cw * ch;
    if frame.data.len() < y_size + cb_size + cr_size {
        return None;
    }
    let y_off = 0;
    let cb_off = y_size;
    let cr_off = y_size + cb_size;
    Some((y_off, y_size, cb_off, cb_size, cr_off, cr_size))
}

/// Compute the PSNR (dB) of a single plane.
///
/// Returns `f64::INFINITY` if the two slices are identical (MSE == 0).
fn plane_psnr(a: &[u8], b: &[u8]) -> f64 {
    debug_assert_eq!(a.len(), b.len());
    let mse: f64 = a
        .iter()
        .zip(b.iter())
        .map(|(&x, &y)| {
            let d = x as f64 - y as f64;
            d * d
        })
        .sum::<f64>()
        / a.len() as f64;

    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

/// Compare two YUV420p frames pixel-by-pixel and return the per-plane PSNR in
/// dB for the Y, Cb, and Cr planes respectively.
///
/// Returns `(f64::INFINITY, f64::INFINITY, f64::INFINITY)` for identical frames.
/// Returns `None` if the frames have different dimensions, a non-YUV420p pixel
/// format, or insufficient data.
pub fn psnr_yuv420p(a: &VideoFrame, b: &VideoFrame) -> Option<(f64, f64, f64)> {
    if a.width != b.width || a.height != b.height {
        return None;
    }
    let (ya_off, y_size, cba_off, cb_size, cra_off, cr_size) = yuv420p_planes(a)?;
    let (yb_off, _, cbb_off, _, crb_off, _) = yuv420p_planes(b)?;

    let y_psnr = plane_psnr(
        &a.data[ya_off..ya_off + y_size],
        &b.data[yb_off..yb_off + y_size],
    );
    let cb_psnr = plane_psnr(
        &a.data[cba_off..cba_off + cb_size],
        &b.data[cbb_off..cbb_off + cb_size],
    );
    let cr_psnr = plane_psnr(
        &a.data[cra_off..cra_off + cr_size],
        &b.data[crb_off..crb_off + cr_size],
    );

    Some((y_psnr, cb_psnr, cr_psnr))
}

/// Returns `true` if every corresponding sample in `a` and `b` differs by at
/// most `max_diff`.
///
/// Returns `false` if the frames have different dimensions, pixel formats, or
/// insufficient data.
pub fn within_tolerance(a: &VideoFrame, b: &VideoFrame, max_diff: u8) -> bool {
    if a.width != b.width || a.height != b.height || a.pixel_format != b.pixel_format {
        return false;
    }
    if a.data.len() != b.data.len() {
        return false;
    }
    a.data
        .iter()
        .zip(b.data.iter())
        .all(|(&x, &y)| x.abs_diff(y) <= max_diff)
}

/// Count the number of pixels where the Y (luma) plane differs between `a` and
/// `b`.
///
/// Returns 0 when the frames are identical or if either frame is not YUV420p /
/// has insufficient data.
pub fn luma_diff_count(a: &VideoFrame, b: &VideoFrame) -> usize {
    if a.width != b.width || a.height != b.height {
        return 0;
    }
    let (ya_off, y_size, ..) = match yuv420p_planes(a) {
        Some(v) => v,
        None => return 0,
    };
    let (yb_off, ..) = match yuv420p_planes(b) {
        Some(v) => v,
        None => return 0,
    };
    a.data[ya_off..ya_off + y_size]
        .iter()
        .zip(b.data[yb_off..yb_off + y_size].iter())
        .filter(|(&x, &y)| x != y)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::{grey_yuv420p_frame, ramp_yuv420p_frame};

    #[test]
    fn identical_frames_have_infinite_psnr() {
        let f = grey_yuv420p_frame(16, 16);
        let (y, cb, cr) = psnr_yuv420p(&f, &f).unwrap();
        assert!(y.is_infinite());
        assert!(cb.is_infinite());
        assert!(cr.is_infinite());
    }

    #[test]
    fn different_frames_have_finite_psnr() {
        let a = grey_yuv420p_frame(16, 16);
        let b = ramp_yuv420p_frame(16, 16);
        let (y, _cb, _cr) = psnr_yuv420p(&a, &b).unwrap();
        assert!(y.is_finite());
        assert!(y < 100.0);
    }

    #[test]
    fn luma_diff_count_zero_for_identical() {
        let f = grey_yuv420p_frame(16, 16);
        assert_eq!(luma_diff_count(&f, &f), 0);
    }
}
