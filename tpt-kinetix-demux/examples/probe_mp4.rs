//! Probe an MP4 file and print its tracks.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p tpt-kinetix-demux --example probe_mp4 -- path/to/video.mp4
//! ```

use tpt_kinetix_demux::mp4::Mp4Demuxer;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: probe_mp4 <file.mp4>"))?;

    let data = std::fs::read(&path)?;
    let demuxer = Mp4Demuxer::new(data)?;

    println!("File: {path}");
    println!("Tracks: {}", demuxer.tracks().len());
    for track in demuxer.tracks() {
        println!(
            "  track #{} [{:?}] codec={:?} {}x{} samples={}",
            track.track_id,
            track.media_type,
            track.codec,
            track.width,
            track.height,
            track.sample_count(),
        );
    }

    Ok(())
}
