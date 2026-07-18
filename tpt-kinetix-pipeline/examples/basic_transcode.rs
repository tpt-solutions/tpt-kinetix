//! Basic transcode-style example: generate synthetic YUV420p frames, scale them
//! with the pipeline's filter, and encode them to AV1 with `rav1e`.
//!
//! This is self-contained (no input file needed) and demonstrates how the
//! filter + encode building blocks fit together.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p tpt-kinetix-pipeline --example basic_transcode
//! ```

use tpt_kinetix_av1::{Av1Encoder, Av1EncoderConfig};
use tpt_kinetix_core::{frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp};
use tpt_kinetix_pipeline::filter::scale_yuv420p;

/// Build a synthetic YUV420p frame with a moving vertical gradient.
fn synthetic_frame(index: u32, w: u32, h: u32) -> VideoFrame {
    let y_size = (w * h) as usize;
    let c_size = ((w / 2) * (h / 2)) as usize;
    let mut data = vec![0u8; y_size + 2 * c_size];
    for y in 0..h {
        for x in 0..w {
            data[(y * w + x) as usize] = ((x + index * 4) % 256) as u8;
        }
    }
    for c in data[y_size..].iter_mut() {
        *c = 128;
    }
    VideoFrame {
        pts: Timestamp::new(index as i64, (1, 30)),
        dts: Timestamp::new(index as i64, (1, 30)),
        data,
        width: w,
        height: h,
        pixel_format: PixelFormat::Yuv420p,
        is_key_frame: index == 0,
    }
}

fn main() -> anyhow::Result<()> {
    let (src_w, src_h) = (128, 96);
    let (dst_w, dst_h) = (64, 48);

    let mut encoder = Av1Encoder::new(&Av1EncoderConfig {
        width: dst_w,
        height: dst_h,
        speed: 10,
        ..Default::default()
    })?;

    let mut packets = 0usize;
    let mut bytes = 0usize;

    for i in 0..10 {
        let frame = synthetic_frame(i, src_w, src_h);
        let scaled = scale_yuv420p(&frame, dst_w, dst_h);
        if let Some(pkt) = encoder.encode_frame(&scaled)? {
            packets += 1;
            bytes += pkt.data.len();
        }
    }

    for pkt in encoder.flush()? {
        packets += 1;
        bytes += pkt.data.len();
    }

    println!("encoded {packets} AV1 packets, {bytes} bytes total");
    Ok(())
}
