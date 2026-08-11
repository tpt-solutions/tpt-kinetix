/// Decode the IPPPP clip and print per-block diagnostics for MB(3,2) in frame 2.
/// Shows MC prediction, IDCT residual, and final reconstruction for the wrong block.
use std::process::Command;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;
const FRAMES: usize = 5;

fn run(cmd: &mut Command) -> bool {
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

fn generate(dir: &std::path::Path) -> Option<(Vec<u8>, Vec<u8>)> {
    let h264 = dir.join("ipppp_dbg.h264");
    let refyuv = dir.join("ipppp_dbg.yuv");

    let input_spec = format!("testsrc=size={WIDTH}x{HEIGHT}:rate=1:duration={FRAMES}");
    let x264_params =
        "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock=1:keyint=250:min-keyint=250"
            .to_string();
    if !run(Command::new("ffmpeg").args([
        "-hide_banner", "-loglevel", "error", "-y",
        "-f", "lavfi", "-i", &input_spec,
        "-frames:v", &FRAMES.to_string(),
        "-c:v", "libx264",
        "-profile:v", "baseline",
        "-bf", "0",
        "-pix_fmt", "yuv420p",
        "-x264-params", &x264_params,
        h264.to_str()?,
    ])) { return None; }

    if !run(Command::new("ffmpeg").args([
        "-hide_banner", "-loglevel", "error", "-y",
        "-i", h264.to_str()?,
        "-f", "rawvideo", "-pix_fmt", "yuv420p",
        refyuv.to_str()?,
    ])) { return None; }

    Some((std::fs::read(&h264).ok()?, std::fs::read(&refyuv).ok()?))
}

fn split_nals(annexb: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= annexb.len() {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push((i, i + 3));
            i += 3;
        } else if i + 4 <= annexb.len()
            && annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 0 && annexb[i + 3] == 1
        {
            starts.push((i, i + 4));
            i += 4;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (idx, &(_, payload_start)) in starts.iter().enumerate() {
        let end = starts.get(idx + 1).map(|&(s,_)| s).unwrap_or(annexb.len());
        let mut unit = vec![0, 0, 0, 1];
        unit.extend_from_slice(&annexb[payload_start..end]);
        out.push(unit);
    }
    out
}

fn main() {
    let dir = std::env::temp_dir().join("tpt_kinetix_dbg_ipppp");
    std::fs::create_dir_all(&dir).unwrap();

    let (annexb, refyuv) = match generate(&dir) {
        Some(t) => t,
        None => { eprintln!("ffmpeg generation failed"); return; }
    };

    let frame_len = (WIDTH as usize * HEIGHT as usize * 3) / 2;
    let luma_len = WIDTH as usize * HEIGHT as usize;
    let chroma_len = (WIDTH as usize / 2) * (HEIGHT as usize / 2);

    let mut dec = H264Decoder::new();
    let mut decoded = Vec::new();
    for unit in split_nals(&annexb) {
        let pkt = Packet {
            pts: Timestamp::new(decoded.len() as i64, (1, 30)),
            dts: Timestamp::new(decoded.len() as i64, (1, 30)),
            data: unit,
            stream_index: 0,
            is_key_frame: decoded.is_empty(),
        };
        if let Some(frame) = dec.decode(&pkt).expect("decode error") {
            decoded.push(frame);
        }
    }

    println!("Decoded {} frames", decoded.len());

    for (fi, frame) in decoded.iter().enumerate() {
        let reference = &refyuv[fi * frame_len..(fi + 1) * frame_len];
        let our = &frame.data;

        let mut wrong = Vec::new();
        for i in 0..frame_len.min(our.len()) {
            let d = (our[i] as i32 - reference[i] as i32).abs();
            if d != 0 {
                let (plane, px, py) = if i < luma_len {
                    ("Y", i % WIDTH as usize, i / WIDTH as usize)
                } else if i < luma_len + chroma_len {
                    let ci = i - luma_len;
                    ("Cb", ci % (WIDTH as usize / 2), ci / (WIDTH as usize / 2))
                } else {
                    let ci = i - luma_len - chroma_len;
                    ("Cr", ci % (WIDTH as usize / 2), ci / (WIDTH as usize / 2))
                };
                wrong.push((plane, px, py, our[i], reference[i], d));
            }
        }

        let max_diff = wrong.iter().map(|w| w.5).max().unwrap_or(0);
        println!("frame {fi}: max_diff={max_diff}, num_wrong={}/{frame_len}", wrong.len());
        for (plane, px, py, got, want, d) in &wrong {
            println!("  {plane} ({px},{py}) got={got} want={want} diff={d}");
        }

        if fi == 2 && !wrong.is_empty() {
            // Print reference pixels around the wrong area for context
            println!("\n  Reference P2 frame luma around (56, 40..43):");
            for row in 40..44 {
                let start = row * WIDTH as usize + 52;
                let slice = &reference[start..start + 12];
                print!("    y={row} x=52..63: ");
                for &v in slice { print!("{:3} ", v); }
                println!();
            }
            println!("\n  Our P2 frame luma around (56, 40..43):");
            for row in 40..44 {
                let start = row * WIDTH as usize + 52;
                let slice = &our[start..start + 12];
                print!("    y={row} x=52..63: ");
                for &v in slice { print!("{:3} ", v); }
                println!();
            }

            // Print P1 reference frame around where MB(3,2) likely looks for MC
            // MB(3,2) covers x=48..63, y=32..47. Block 10 is at x=56..59, y=40..43.
            // We don't know the MV, but print P1 luma around the area for comparison.
            if let Some(p1) = decoded.get(1) {
                println!("\n  P1 luma around (56, 40..43) [DPB ref for P2]:");
                for row in 40..44 {
                    let start = row * WIDTH as usize + 52;
                    let slice = &p1.data[start..start + 12];
                    print!("    y={row} x=52..63: ");
                    for &v in slice { print!("{:3} ", v); }
                    println!();
                }
            }
        }
    }

    // Also decode the raw bitstream with ffmpeg's compact debug output
    println!("\nNow extracting macroblock info from ffmpeg debug...");
    let mb_info = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "debug",
            "-skip_frame", "noref",
            "-i", dir.join("ipppp_dbg.h264").to_str().unwrap(),
            "-f", "null", "-",
        ])
        .output();
    if let Ok(out) = mb_info {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Print lines mentioning mb or mv near our coordinates
        for line in stderr.lines() {
            if (line.contains("mb_type") || line.contains("mv_cache") || line.contains("coeff"))
                && line.contains("56")
            {
                println!("  ffmpeg: {line}");
            }
        }
    }
}
