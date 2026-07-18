//! Read an AudioSpecificConfig (e.g. an FLV AAC sequence header) and report the
//! audio parameters, then parse an ADTS stream's frames.
//!
//! Run with: `cargo run -p tpt-kinetix-aac --example parse_aac`

use tpt_kinetix_aac::{adts, config::AudioSpecificConfig};

fn main() {
    // A FLV AAC sequence header for AAC-LC, 44.1 kHz, stereo.
    let asc_bytes = [0x12, 0x10];
    let cfg = AudioSpecificConfig::parse(&asc_bytes).expect("valid ASC");
    println!(
        "AudioSpecificConfig: object_type={} sample_rate={} Hz channels={}",
        cfg.object_type, cfg.sample_rate, cfg.channels
    );

    // A minimal ADTS-framed stream: two frames, each 7-byte header + 1 payload byte.
    let mut frame = [0xFF, 0xF1, 0x50, 0x80, 0x01, 0x00, 0xFC];
    frame[4] = 0x01; // aac_frame_length = 8
    let mut stream: Vec<u8> = Vec::new();
    stream.extend_from_slice(&frame);
    stream.push(0xAA);
    stream.extend_from_slice(&frame);
    stream.push(0xBB);

    for (i, (hdr, _payload)) in adts::iter_frames(&stream).iter().enumerate() {
        println!(
            "ADTS frame {}: {} Hz, {} ch, len={} bytes",
            i, hdr.sample_rate, hdr.channels, hdr.frame_length
        );
    }
}
