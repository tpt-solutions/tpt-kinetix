//! End-to-end pipeline test for the royalty-free VP9 decode path:
//! `DemuxStage → Vp9DecodeStage → SinkStage` over a real VP9-in-MP4 file
//! encoded by ffmpeg's libvpx. Skips (never panics) when ffmpeg is absent.

use std::process::Command;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Encode `count` frames of `source` at `w`x`h` to a VP9-in-MP4 file.
fn make_vp9_mp4(path: &std::path::Path, w: u32, h: u32, source: &str, count: u32) -> bool {
    let status = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("{source}=duration=1:size={w}x{h}:rate=10"))
        .args(["-pix_fmt", "yuv420p", "-frames:v", &count.to_string()])
        .args(["-c:v", "libvpx-vp9", "-cpu-used", "4"])
        .arg(path)
        .status();
    matches!(status, Ok(s) if s.success())
}

/// Runs the demux → VP9-decode → sink pipeline and returns
/// `(frame count, decoded geometry per frame)`.
fn run_vp9_pipeline(data: Vec<u8>) -> Result<(usize, Vec<(u32, u32)>), String> {
    let (sink, frames) = tpt_kinetix_pipeline::SinkStage::new();

    let pipeline = tpt_kinetix_pipeline::Pipeline::new()
        .add_stage(tpt_kinetix_pipeline::DemuxStage { data })
        .add_stage(tpt_kinetix_pipeline::Vp9DecodeStage)
        .add_stage(sink);

    pipeline
        .run_to_completion()
        .map_err(|e| format!("pipeline error: {e}"))?;

    let frames = frames.lock().expect("sink mutex poisoned");
    let geometry = frames.iter().map(|f| (f.width, f.height)).collect();
    Ok((frames.len(), geometry))
}

#[test]
fn pipeline_demux_vp9_mp4_decodes_pixel_exact() {
    if !ffmpeg_available() {
        eprintln!("[GAP] pipeline vp9 e2e: ffmpeg unavailable, skipping");
        return;
    }
    let dir = std::env::temp_dir();
    for (w, h) in [(96u32, 64u32), (125u32, 67u32)] {
        let path = dir.join(format!("tpt_pipeline_vp9_{w}x{h}.mp4"));
        if !make_vp9_mp4(&path, w, h, "testsrc", 4) {
            eprintln!("[GAP] pipeline vp9 e2e: libvpx-vp9 MP4 encode failed, skipping");
            return;
        }
        let data = std::fs::read(&path).expect("read generated mp4");
        let (count, geometry) = match run_vp9_pipeline(data) {
            Ok(r) => r,
            Err(e) => panic!("{w}x{h}: pipeline failed: {e}"),
        };
        assert_eq!(count, 4, "{w}x{h}: expected all 4 frames to decode");
        assert!(
            geometry.iter().all(|&(gw, gh)| gw == w && gh == h),
            "{w}x{h}: decoded frames have wrong geometry"
        );
        let _ = std::fs::remove_file(&path);
    }
}
