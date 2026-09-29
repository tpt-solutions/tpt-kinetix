//! Throwaway: decode an .ivf with Av1Decoder and dump one frame's raw
//! YUV420p bytes to a file, for direct comparison against an ffmpeg/dav1d
//! reference dump. `cargo run -p tpt-kinetix-av1 --example dbg_dump_frame --
//! <in.ivf> <frame_idx> <out.yuv>`.

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};

fn split_ivf_frames(ivf: &[u8]) -> Vec<Vec<u8>> {
    if ivf.len() < 44 || &ivf[0..4] != b"DKIF" {
        return Vec::new();
    }
    let mut frames = Vec::new();
    let mut off = 32usize;
    while off + 12 <= ivf.len() {
        let sz = u32::from_le_bytes([ivf[off], ivf[off + 1], ivf[off + 2], ivf[off + 3]]) as usize;
        if off + 12 + sz > ivf.len() {
            break;
        }
        frames.push(ivf[off + 12..off + 12 + sz].to_vec());
        off += 12 + sz;
    }
    frames
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: dbg_dump_frame <ivf> <frame_idx> <out.yuv>");
    let target: usize = args
        .next()
        .expect("frame_idx")
        .parse()
        .expect("frame_idx int");
    let out = args.next().expect("out.yuv");
    let bytes = std::fs::read(&path).expect("read ivf");
    let packets = split_ivf_frames(&bytes);
    let mut dec = Av1Decoder::new();
    for (i, data) in packets.iter().enumerate() {
        let pk = Packet {
            pts: Timestamp::new(i as i64, (1, 30)),
            dts: Timestamp::new(i as i64, (1, 30)),
            data: data.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        };
        match dec.decode(&pk) {
            Ok(Some(f)) => {
                eprintln!("[{i}] ok {}x{} ({} bytes)", f.width, f.height, f.data.len());
                if i == target {
                    std::fs::write(&out, &f.data).expect("write out");
                    eprintln!("wrote frame {i} ({} bytes) to {out}", f.data.len());
                    return;
                }
            }
            Ok(None) => eprintln!("[{i}] no frame"),
            Err(e) => {
                eprintln!("[{i}] ERROR: {e}");
                return;
            }
        }
    }
    eprintln!(
        "frame {target} never reached (stream had {} packets)",
        packets.len()
    );
}
