/// Debug quarter-pel position (3,1) for the IPPPP frame 2 issue.
/// MV for block 10 of MB(3,2) = (27,1), giving fx=3, fy=1.
/// Block 10 base = global (56, 40).
/// Reference frame = P1 (decoded frame 1).
use std::process::Command;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;
use tpt_kinetix_h264::motion_comp;

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
        "-bf", "0", "-pix_fmt", "yuv420p",
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
    for (idx, &(_, ps)) in starts.iter().enumerate() {
        let end = starts.get(idx + 1).map(|&(s,_)| s).unwrap_or(annexb.len());
        let mut u = vec![0, 0, 0, 1];
        u.extend_from_slice(&annexb[ps..end]);
        out.push(u);
    }
    out
}

/// Manual single-pixel interpolation at fraction (fx,fy) from base (x0,y0)
/// using 4 different candidate formulas for position (3,1).
fn pred_luma_31_variants(plane: &[u8], stride: usize, pw: usize, ph: usize, x0: i32, y0: i32) -> [u8; 4] {
    // Helper closures
    let get = |x: i32, y: i32| -> u8 {
        let x = x.clamp(0, pw as i32 - 1);
        let y = y.clamp(0, ph as i32 - 1);
        plane[y as usize * stride + x as usize]
    };
    const TAP: [i32; 6] = [1, -5, 20, 20, -5, 1];
    let tap_h = |xi: i32, yi: i32| -> i32 {
        let mut s = 0i32;
        for (k, &c) in TAP.iter().enumerate() { s += c * get(xi + k as i32 - 2, yi) as i32; }
        s
    };
    let tap_v = |xi: i32, yi: i32| -> i32 {
        let mut s = 0i32;
        for (k, &c) in TAP.iter().enumerate() { s += c * get(xi, yi + k as i32 - 2) as i32; }
        s
    };
    let half_h = |xi: i32, yi: i32| -> u8 { ((tap_h(xi, yi) + 16) >> 5).clamp(0, 255) as u8 };
    let half_v = |xi: i32, yi: i32| -> u8 { ((tap_v(xi, yi) + 16) >> 5).clamp(0, 255) as u8 };
    let half_j = |xi: i32, yi: i32| -> u8 {
        let mut s = 0i32;
        for (k, &c) in TAP.iter().enumerate() { s += c * tap_h(xi, yi + k as i32 - 2); }
        ((s + 512) >> 10).clamp(0, 255) as u8
    };
    let avg = |a: u8, b: u8| -> u8 { ((a as u16 + b as u16 + 1) >> 1) as u8 };

    // Current code: avg(b(x0,y0), j(x0+1,y0))
    let a = avg(half_h(x0, y0), half_j(x0 + 1, y0));
    // My first fix: avg(j(x0,y0), b(x0+1,y0))
    let b = avg(half_j(x0, y0), half_h(x0 + 1, y0));
    // Option C: avg(b(x0,y0), h(x0+1,y0)) — horizontal half and vertical half at x+1
    let c = avg(half_h(x0, y0), half_v(x0 + 1, y0));
    // Option D: avg(g(x0+1,y0), j(x0,y0)) — integer at (x+1,y) and diag half at (x,y)
    let d = avg(get(x0 + 1, y0), half_j(x0, y0));

    [a, b, c, d]
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

    // Decode to get frame 1 (P1 = reference for P2)
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

    let p1 = &decoded[1].data[..luma_len];  // P1 luma
    let p2_ours = &decoded[2].data[..luma_len];  // P2 luma (ours)
    let p2_ref = &refyuv[2 * frame_len..3 * frame_len][..luma_len];  // P2 luma (ffmpeg)

    let w = WIDTH as usize;
    let h = HEIGHT as usize;

    // MV for block 10 of MB(3,2) is (27, 1) quarter-pel.
    // Block 10 in raster order: bx=(10%4)*4=8, by=(10/4)*4=8
    // Global base: x0=48+8=56, y0=32+8=40.
    let mvx = 27i32;
    let mvy = 1i32;

    println!("Block 10 of MB(3,2): global base (56,40), MV=({mvx},{mvy})");
    println!("MV frac: fx={}, fy={}", mvx.rem_euclid(4), mvy.rem_euclid(4));

    // MC prediction using current code (via interpolate_luma)
    let mut pred_current = [0u8; 16];
    motion_comp::interpolate_luma(&mut pred_current, 4, p1, w, w, h, 56, 40, mvx, mvy, 4, 4);

    println!("\nMC prediction (current code):");
    for row in 0..4 {
        for col in 0..4 { print!("{:3} ", pred_current[row*4+col]); }
        println!();
    }

    println!("\nP2 reference (ffmpeg, block 10):");
    for row in 0..4 {
        for col in 0..4 { print!("{:3} ", p2_ref[(40+row)*w + (56+col)]); }
        println!();
    }

    println!("\nP2 ours (block 10):");
    for row in 0..4 {
        for col in 0..4 { print!("{:3} ", p2_ours[(40+row)*w + (56+col)]); }
        println!();
    }

    // Now compute variants for just the (3,1) position, for each row in the block
    println!("\nFor column 0 (fx=3, fy=1) at each row of the block:");
    println!("  [using P1 as reference, x0_ref=62, fy=1]");

    for row in 0..4usize {
        // split(py) where py = 4*(40+row) + mvy = 4*(40+row)+1
        let py_full = 4 * (40 + row as i32) + mvy;
        let y0_ref = py_full.div_euclid(4);
        let fy = py_full - 4 * y0_ref;
        // split(px) where px = 4*56 + mvx = 251
        let px_full = 4 * 56 + mvx;  // col=0
        let x0_ref = px_full.div_euclid(4);
        let fx = px_full - 4 * x0_ref;

        let variants = pred_luma_31_variants(p1, w, w, h, x0_ref, y0_ref);
        let p2_want = p2_ref[(40+row)*w + 56];
        let p2_have = p2_ours[(40+row)*w + 56];

        println!("  row={row} (global y={}): x0_ref={x0_ref}, y0_ref={y0_ref}, fx={fx}, fy={fy}",
            40+row);
        println!("    A=avg(b,j(+1))={} B=avg(j,b(+1))={} C=avg(b,hv(+1))={} D=avg(G',j)={}",
            variants[0], variants[1], variants[2], variants[3]);
        println!("    ffmpeg_pixel={p2_want} our_pixel={p2_have}");
    }

    // Also print P1 around x=62 for reference
    println!("\nP1 luma around x=62, y=36..45:");
    for row in 36..46 {
        let slice = [
            p1[row * w + 59], p1[row * w + 60], p1[row * w + 61],
            p1[row * w + 62], p1[row * w + 63],
        ];
        println!("  y={row} x=59..63: {:3} {:3} {:3} {:3} {:3}",
            slice[0], slice[1], slice[2], slice[3], slice[4]);
    }
}
