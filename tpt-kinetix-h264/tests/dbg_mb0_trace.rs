//! Diagnostic: trace MB(0,0) reconstruction to find the root cause of residual error.
//! Run with: cargo test -p tpt-kinetix-h264 --test dbg_mb0_trace -- --nocapture

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::{H264Decoder, trace::{DecodeTracer, TracePlane}};

fn ffmpeg_ok() -> bool {
    Command::new("ffmpeg").arg("-version").output()
        .map(|o| o.status.success()).unwrap_or(false)
}

fn gen(dir: &std::path::Path) -> Option<Vec<u8>> {
    let h264 = dir.join("dbg_mb0.h264");
    let ok = Command::new("ffmpeg").args([
        "-hide_banner", "-loglevel", "error", "-y",
        "-f", "lavfi", "-i", "mandelbrot=size=64x48:rate=1",
        "-frames:v", "1", "-c:v", "libx264",
        "-profile:v", "high", "-g", "1", "-bf", "0",
        "-pix_fmt", "yuv420p",
        "-x264-params", "cabac=0:ref=1:bframes=0:8x8dct=1:weightp=0:aud=0:no-deblock=1",
        h264.to_str()?,
    ]).output().map(|o| o.status.success()).unwrap_or(false);
    if !ok { return None; }
    std::fs::read(&h264).ok()
}

struct Mb0Tracer {
    blk: u8,
}

impl DecodeTracer for Mb0Tracer {
    fn on_mb_parsed(&mut self, mb_x: u32, mb_y: u32, mb_type: &str, qp: i32, cbp: u8, _chroma_mode: u8, pred_modes: &[u8; 16]) {
        if mb_x == 0 && mb_y == 0 {
            eprintln!("MB(0,0): type={mb_type} qp={qp} cbp=0x{cbp:02x} pred_modes={pred_modes:?}");
        }
    }

    fn on_cavlc_coeffs(&mut self, mb_x: u32, mb_y: u32, plane: TracePlane, blk: u8, coeffs: &[i16; 16]) {
        if mb_x == 0 && mb_y == 0 && blk < 16 {
            let nz: Vec<(usize, i16)> = coeffs.iter().enumerate().filter(|(_, &v)| v != 0).map(|(i,&v)| (i,v)).collect();
            eprintln!("  [{plane:?}] blk{blk} coeffs (zigzag, non-zero): {nz:?}");
        }
    }

    fn on_cavlc_block_info(&mut self, mb_x: u32, mb_y: u32, plane: TracePlane, blk: u8, nc: i32, total_coeff: u8, trailing_ones: u8, _dc_skip: u32) {
        if mb_x == 0 && mb_y == 0 && blk < 16 {
            eprintln!("  [{plane:?}] blk{blk} nc={nc} total_coeff={total_coeff} trailing_ones={trailing_ones}");
        }
    }

    fn on_intra_pred(&mut self, mb_x: u32, mb_y: u32, plane: TracePlane, blk: u8, pred: &[u8]) {
        if matches!(plane, TracePlane::Luma) && mb_x == 0 && mb_y == 0 {
            let first4: Vec<u8> = pred[0..pred.len().min(4)].to_vec();
            eprintln!("  MB({mb_x},{mb_y}) [Luma] blk{blk} pred[0..4]={first4:?} all_same={}", pred.iter().all(|&v| v == pred[0]));
            self.blk = blk;
        }
    }

    fn on_reconstructed(&mut self, mb_x: u32, mb_y: u32, plane: TracePlane, blk: u8, samples: &[u8]) {
        if matches!(plane, TracePlane::Luma) && mb_x == 0 && mb_y == 0 {
            let sz = samples.len().min(8);
            eprintln!("  MB({mb_x},{mb_y}) [Luma] blk{blk} recon[0..{sz}]={:?}", &samples[0..sz]);
        }
    }
}

#[test]
fn dbg_mb0_trace_test() {
    if !ffmpeg_ok() { eprintln!("ffmpeg not available; skipping"); return; }
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_dbg_mb0");
    std::fs::create_dir_all(&dir).unwrap();
    let annexb = match gen(&dir) { Some(b) => b, None => { eprintln!("gen failed; skipping"); return; } };

    let mut dec = H264Decoder::new();
    let mut tracer = Mb0Tracer { blk: 0 };
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    dec.decode_with_tracer(&pkt, &mut tracer).expect("decode ok");
}
