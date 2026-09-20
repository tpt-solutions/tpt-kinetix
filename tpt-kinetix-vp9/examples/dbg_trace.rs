//! Debug helper: decode an IVF file with the VP9 decoder, forwarding any
//! env-gated traces (e.g. `TPT_VP9_COEF`) to stderr so they can be diffed
//! against an instrumented reference decoder run over the same file.
//!
//! Usage: `cargo run -p tpt-kinetix-vp9 --example dbg_trace <file.ivf>`

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_vp9::Vp9Decoder;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: dbg_trace <file.ivf>");
    let data = std::fs::read(&path).expect("read ivf");
    let mut pos = 32usize; // IVF file header
    let mut dec = Vp9Decoder::new();
    let mut n = 0usize;
    while pos + 12 <= data.len() {
        let fsz =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 12;
        if pos + fsz > data.len() {
            break;
        }
        let frame = data[pos..pos + fsz].to_vec();
        pos += fsz;
        let packet = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: frame,
            stream_index: 0,
            is_key_frame: n == 0,
        };
        match dec.decode(&packet) {
            Ok(Some(vf)) => {
                eprintln!(
                    "frame {n}: {}x{} {} bytes",
                    vf.width,
                    vf.height,
                    vf.data.len()
                );
                if let Some(out) = std::env::var_os("TPT_VP9_YUV") {
                    std::fs::write(out, &vf.data).expect("write yuv");
                }
            }
            Ok(None) => eprintln!("frame {n}: no output"),
            Err(e) => eprintln!("frame {n}: ERROR {e}"),
        }
        n += 1;
    }
    eprintln!("decoded {n} frames");
}
