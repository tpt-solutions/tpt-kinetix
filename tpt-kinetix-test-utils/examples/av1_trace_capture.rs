use std::{env, fs};

use tpt_kinetix_av1::entropy::{enable_symbol_trace, take_block_markers, take_symbol_trace};
use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::{synthetic::minimal_av1_obu, trace::TraceCapture};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = env::args()
        .nth(1)
        .unwrap_or_else(|| "av1-trace.json".to_string());
    let obu = minimal_av1_obu(64, 48).ok_or("ffmpeg could not create an AV1 test OBU")?;
    let mut decoder = Av1Decoder::new();
    enable_symbol_trace();
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: obu,
        stream_index: 0,
        is_key_frame: true,
    };
    decoder.decode(&packet)?;
    let capture = TraceCapture::from_av1_trace(&take_symbol_trace(), &take_block_markers());
    fs::write(&output, serde_json::to_string_pretty(&capture)?)?;
    println!("wrote {output} ({} entries)", capture.entries.len());
    Ok(())
}
