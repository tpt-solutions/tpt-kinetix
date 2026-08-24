//! Scratch debug tool (uncommitted, per this repo's convention): decodes a
//! raw AV1 OBU file with `KINETIX_AV1_NOFILTER=1` and dumps the raw I420
//! frame bytes so they can be diffed against a genuine pre-filter reference
//! (a standalone `dav1d` build's `--inloopfilters none` output) outside the
//! Rust harness.
//!
//! Run: `cargo run -p tpt-kinetix-test-utils --example dbg_av1_prefilter_dump -- <obu_path> <out_path>`

use std::io::Write;

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let obu_path = &args[1];
    let out_path = &args[2];
    let obu = std::fs::read(obu_path).expect("read obu");
    std::env::set_var("KINETIX_AV1_NOFILTER", "1");
    let mut dec = Av1Decoder::new();
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: obu,
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec.decode(&packet).expect("decode").expect("frame");
    let mut f = std::fs::File::create(out_path).expect("create out");
    f.write_all(&frame.data).expect("write");
    eprintln!("wrote {} bytes to {out_path}", frame.data.len());
}
