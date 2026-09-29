//! Scratch: decode an AV1 IVF with Kinetix only, frame-aligned, so the
//! KINETIX_AV1_DBG_* traces can be captured without building the
//! test-utils/h264 dependency chain (which a concurrent session may have
//! broken). Usage: tile2_probe <ivf> [max-shown-frames]

use std::io::Read;

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: tile2_probe <ivf> [max]");
    let max: Option<usize> = std::env::args().nth(2).and_then(|s| s.parse().ok());
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();

    // split IVF frames: 32-byte header, then 12-byte frame headers
    let mut pos = 32usize;
    let mut packets = Vec::new();
    while pos + 12 <= bytes.len() {
        let size = u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        packets.push(bytes[pos + 12..pos + 12 + size].to_vec());
        pos += 12 + size;
    }

    let mut dec = Av1Decoder::new();
    let mut shown = 0usize;
    for (i, data) in packets.iter().enumerate() {
        let pk = Packet {
            pts: Timestamp::new(i as i64, (1, 30)),
            dts: Timestamp::new(i as i64, (1, 30)),
            data: data.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        };
        if dec.decode(&pk).is_ok() {
            shown += 1;
        }
        if let Some(m) = max {
            if shown >= m {
                break;
            }
        }
    }
}
