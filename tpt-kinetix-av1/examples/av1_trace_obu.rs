//! Decode a raw AV1 OBU (`.obu`, low-overhead bitstream) file with `Av1Decoder`.
//! Scratch tool for diffing Kinetix's per-block/per-symbol decode trace against
//! a patched `dav1d` (`KINETIX_AV1_TRACE=1 cargo run --example av1_trace_obu -- file.obu`).

use std::io::Read;

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: av1_trace_obu <file.obu>");
    let mut data = Vec::new();
    std::fs::File::open(&path)
        .expect("open obu")
        .read_to_end(&mut data)
        .expect("read obu");
    let mut dec = Av1Decoder::new();
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data,
        stream_index: 0,
        is_key_frame: true,
    };
    match dec.decode(&packet) {
        Ok(Some(f)) => {
            eprintln!("decoded {}x{} ({} bytes)", f.width, f.height, f.data.len());
            if let Ok(out) = std::env::var("KINETIX_AV1_DUMP_FINAL") {
                std::fs::write(&out, &f.data).expect("write final yuv");
                eprintln!("wrote final YUV to {out}");
            }
        }
        Ok(None) => eprintln!("no frame"),
        Err(e) => eprintln!("error: {e}"),
    }
}
