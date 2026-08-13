//! Frame filters (scaling, format conversion).
//!
//! These are pure per-frame transforms used by [`crate::stage::FilterStage`].
//! The scaler uses nearest-neighbour sampling, which is fast and dependency-free
//! and sufficient for the pipeline's format/geometry-adaptation role. Higher
//! quality resamplers can be added later behind the same signature.

use tpt_kinetix_core::{frame::VideoFrame, pixel_format::PixelFormat};

/// Nearest-neighbour scale a YUV420p [`VideoFrame`] to `dst_w`x`dst_h`.
///
/// Returns the input unchanged if it is not YUV420p, if the requested size is
/// zero, or if it already matches the target dimensions.
///
/// # Examples
///
/// ```
/// use tpt_kinetix_core::{frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp};
/// use tpt_kinetix_pipeline::filter::scale_yuv420p;
///
/// let src = VideoFrame {
///     pts: Timestamp::NONE,
///     dts: Timestamp::NONE,
///     data: vec![128u8; 4 * 4 + 2 * (2 * 2)],
///     width: 4,
///     height: 4,
///     pixel_format: PixelFormat::Yuv420p,
///     is_key_frame: true,
/// };
/// let dst = scale_yuv420p(&src, 8, 8);
/// assert_eq!((dst.width, dst.height), (8, 8));
/// assert_eq!(dst.data.len(), 8 * 8 + 2 * (4 * 4));
/// ```
pub fn scale_yuv420p(src: &VideoFrame, dst_w: u32, dst_h: u32) -> VideoFrame {
    if src.pixel_format != PixelFormat::Yuv420p
        || dst_w == 0
        || dst_h == 0
        || (src.width == dst_w && src.height == dst_h)
    {
        return src.clone();
    }

    let sw = src.width as usize;
    let sh = src.height as usize;
    let scw = sw.div_ceil(2);
    let sch = sh.div_ceil(2);

    let dw = dst_w as usize;
    let dh = dst_h as usize;
    let dcw = dw.div_ceil(2);
    let dch = dh.div_ceil(2);

    let sy_size = sw * sh;
    let sc_size = scw * sch;

    // Guard against short input buffers.
    if src.data.len() < sy_size + 2 * sc_size {
        return src.clone();
    }
    let (sy, rest) = src.data.split_at(sy_size);
    let (scb, scr) = rest.split_at(sc_size);

    let mut data = Vec::with_capacity(dw * dh + 2 * dcw * dch);
    scale_plane(sy, sw, sh, dw, dh, &mut data);
    scale_plane(scb, scw, sch, dcw, dch, &mut data);
    scale_plane(scr, scw, sch, dcw, dch, &mut data);

    VideoFrame {
        pts: src.pts,
        dts: src.dts,
        data,
        width: dst_w,
        height: dst_h,
        pixel_format: PixelFormat::Yuv420p,
        is_key_frame: src.is_key_frame,
    }
}

/// Append a nearest-neighbour-scaled copy of `src` (of size `sw`x`sh`) at
/// `dw`x`dh` onto `out`.
fn scale_plane(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize, out: &mut Vec<u8>) {
    if sw == 0 || sh == 0 {
        out.resize(out.len() + dw * dh, 0);
        return;
    }
    for y in 0..dh {
        let sy = (y * sh) / dh;
        let row = &src[sy * sw..sy * sw + sw];
        for x in 0..dw {
            let sx = (x * sw) / dw;
            out.push(row[sx]);
        }
    }
}

#[cfg(test)]
mod tests {
    use tpt_kinetix_core::timestamp::Timestamp;

    use super::*;

    fn frame(w: u32, h: u32, fill: u8) -> VideoFrame {
        let cw = (w as usize).div_ceil(2);
        let ch = (h as usize).div_ceil(2);
        let len = (w * h) as usize + 2 * cw * ch;
        VideoFrame {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: vec![fill; len],
            width: w,
            height: h,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: true,
        }
    }

    #[test]
    fn upscales_dimensions_and_len() {
        let src = frame(4, 4, 200);
        let dst = scale_yuv420p(&src, 8, 8);
        assert_eq!((dst.width, dst.height), (8, 8));
        assert_eq!(dst.data.len(), 8 * 8 + 2 * (4 * 4));
        // Constant plane stays constant after scaling.
        assert!(dst.data.iter().all(|&b| b == 200));
    }

    #[test]
    fn downscale_halves() {
        let src = frame(8, 8, 42);
        let dst = scale_yuv420p(&src, 4, 4);
        assert_eq!((dst.width, dst.height), (4, 4));
        assert_eq!(dst.data.len(), 4 * 4 + 2 * (2 * 2));
    }

    #[test]
    fn same_size_is_passthrough() {
        let src = frame(6, 6, 1);
        let dst = scale_yuv420p(&src, 6, 6);
        assert_eq!(dst.data, src.data);
    }

    #[test]
    fn non_yuv420p_passthrough() {
        let mut src = frame(4, 4, 5);
        src.pixel_format = PixelFormat::Rgb24;
        let dst = scale_yuv420p(&src, 8, 8);
        assert_eq!(dst.width, 4);
    }
}
