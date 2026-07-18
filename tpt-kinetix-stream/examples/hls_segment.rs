//! Generate an HLS MPEG-TS segment and `.m3u8` playlist from synthetic H.264
//! access units. Self-contained (writes to a temp directory).
//!
//! Run with:
//!
//! ```sh
//! cargo run -p tpt-kinetix-stream --example hls_segment
//! ```

use tpt_kinetix_stream::hls::server::{HlsConfig, HlsPackager};

fn main() -> anyhow::Result<()> {
    let out_dir = std::env::temp_dir().join("tpt_kinetix_hls_example");
    std::fs::create_dir_all(&out_dir)?;

    let mut packager = HlsPackager::new(HlsConfig {
        segment_duration_secs: 2,
        output_dir: out_dir.to_string_lossy().to_string(),
        window_size: 5,
        http_bind_addr: "127.0.0.1:0".into(),
    });

    // Two synthetic AVCC access units (an IDR then a P-frame).
    let idr = vec![0u8, 0, 0, 4, 0x65, 0xAA, 0xBB, 0xCC];
    let p = vec![0u8, 0, 0, 3, 0x41, 0xDD, 0xEE];

    let n = packager.write_ts_segment(&[(idr, 0, true), (p, 3000, false)])?;

    println!("wrote {n}-byte TS segment to {}", out_dir.display());
    println!(
        "playlist:\n{}",
        std::fs::read_to_string(out_dir.join("playlist.m3u8"))?
    );
    Ok(())
}
