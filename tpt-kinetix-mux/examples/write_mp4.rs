//! Write a minimal single-track H.264 MP4 with `tpt-kinetix-mux`.
//!
//! This uses placeholder NAL bytes so the example is self-contained; the point
//! is to show the muxer API and produce a structurally valid MP4.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p tpt-kinetix-mux --example write_mp4 -- out.mp4
//! ```

use tpt_kinetix_mux::{Mp4Muxer, Mp4MuxerConfig};

fn main() -> std::io::Result<()> {
    let out = std::env::args().nth(1).unwrap_or_else(|| "out.mp4".into());

    let mut muxer = Mp4Muxer::new(Mp4MuxerConfig {
        width: 320,
        height: 240,
        timescale: 30_000,
        // Truncated example SPS/PPS (real values come from the H.264 bitstream).
        sps: vec![0x67, 0x42, 0x00, 0x1e, 0xaa, 0xbb],
        pps: vec![0x68, 0xce, 0x3c, 0x80],
    });

    // 30 fake AVCC access units at 30fps (duration = timescale / fps = 1000).
    for i in 0..30 {
        let nal = vec![0, 0, 0, 3, 0x65, i as u8, 0x00];
        muxer.write_sample(&nal, 1000, i == 0);
    }

    let bytes = muxer.finish();
    std::fs::write(&out, &bytes)?;
    println!("wrote {} bytes to {out}", bytes.len());
    Ok(())
}
