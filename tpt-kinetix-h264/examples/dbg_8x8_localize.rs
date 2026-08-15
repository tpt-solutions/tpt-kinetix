//! Scratch harness: localize where the High-profile 8x8 decode diverges from
//! ffmpeg. Not committed; for debugging only.
use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

fn run(cmd: &mut Command) -> bool {
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

fn generate(
    dir: &std::path::Path,
    tag: &str,
    w: u32,
    h: u32,
    dct8x8: bool,
) -> Option<(Vec<u8>, Vec<u8>)> {
    let h264 = dir.join(format!("loc_{tag}.h264"));
    let refyuv = dir.join(format!("loc_{tag}.yuv"));
    let x264 = if dct8x8 {
        "cabac=0:ref=1:bframes=0:8x8dct=1:weightp=0:aud=0:no-deblock=1"
    } else {
        "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock=1"
    };
    let inp = format!("mandelbrot=size={w}x{h}:rate=1");
    let args: Vec<&str> = vec![
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        &inp,
        "-frames:v",
        "1",
        "-c:v",
        "libx264",
        "-profile:v",
        "high",
        "-g",
        "1",
        "-bf",
        "0",
        "-pix_fmt",
        "yuv420p",
        "-x264-params",
        x264,
        h264.to_str()?,
    ];
    if !run(Command::new("ffmpeg").args(&args)) {
        return None;
    }
    if !run(Command::new("ffmpeg").args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-i",
        h264.to_str()?,
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        refyuv.to_str()?,
    ])) {
        return None;
    }
    Some((std::fs::read(&h264).ok()?, std::fs::read(&refyuv).ok()?))
}

fn main() {
    let dir = std::env::temp_dir().join("tpt_kx_loc");
    std::fs::create_dir_all(&dir).unwrap();
    for dct8x8 in [true, false] {
        let tag = if dct8x8 { "y8" } else { "y4" };
        let (annexb, refy) = match generate(&dir, tag, 64, 48, dct8x8) {
            Some(t) => t,
            None => {
                eprintln!("[{tag}] generate failed");
                continue;
            }
        };
        let mut dec = H264Decoder::new();
        let pkt = Packet {
            pts: Timestamp::new(0, (1, 30)),
            dts: Timestamp::new(0, (1, 30)),
            data: annexb,
            stream_index: 0,
            is_key_frame: true,
        };
        let frame = dec.decode(&pkt).expect("decode").expect("frame");
        let w = frame.width as usize;
        let h = frame.height as usize;
        let stride = w; // yuv420p luma stride == width for raw ffmpeg output
                        // Compare per macroblock (16x16) of the luma plane.
        let luma_bytes = w * h;
        let ours = &frame.data[..luma_bytes];
        let refl = &refy[..luma_bytes];
        let mb_cols = w / 16;
        let mb_rows = h / 16;
        let mut worst = 0;
        for mby in 0..mb_rows {
            for mbx in 0..mb_cols {
                let mut md = 0;
                let mut sum = 0;
                let mut cnt = 0;
                for yy in 0..16 {
                    for xx in 0..16 {
                        let px = mbx * 16 + xx;
                        let py = mby * 16 + yy;
                        let o = ours[py * stride + px] as i32;
                        let r = refl[py * stride + px] as i32;
                        let d = (o - r).abs();
                        if d > md {
                            md = d;
                        }
                        sum += d;
                        cnt += 1;
                    }
                }
                let avg = sum / cnt;
                if md > worst {
                    worst = md;
                }
                if md > 0 {
                    eprintln!("[{tag}] MB({mbx},{mby}) max={md} avg={avg}");
                }
            }
        }
        eprintln!("[{tag}] overall worst MB max_diff = {worst}");
    }
}
