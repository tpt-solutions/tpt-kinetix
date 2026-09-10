//! `#[ignore]` diagnostic: per-output-channel and per-frame diff of `al15_44`
//! (channel_config 0 / PCE + independent CCE coupling) against ffmpeg's decode.
//!   cargo test -p tpt-kinetix-aac --test dbg_al15_cce -- --ignored --nocapture
//!
//! al15 is bit-exact except ~1.5 LSB of lossy-float rounding from the coupling
//! multiply-add; this prints where any larger error would land (which channel,
//! which frame) if the coupling or the PCE channel-order regresses.

use std::path::{Path, PathBuf};
use tpt_kinetix_aac::AacDecoder;
use tpt_kinetix_core::{Packet, Timestamp};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/iso")
}

fn split_adts(adts: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 7 <= adts.len() {
        if adts[i] != 0xFF || (adts[i + 1] & 0xF0) != 0xF0 {
            break;
        }
        let len = (((adts[i + 3] as usize & 0x03) << 11)
            | ((adts[i + 4] as usize) << 3)
            | ((adts[i + 5] as usize & 0xE0) >> 5))
            & 0x1FFF;
        if len == 0 || i + len > adts.len() {
            break;
        }
        out.push(adts[i..i + len].to_vec());
        i += len;
    }
    out
}

#[test]
#[ignore]
fn al15_channel_diff() {
    let adts = std::fs::read(root().join("al15_44.adts")).unwrap();
    let refb = std::fs::read(root().join("al15_44.ref.f32")).unwrap();
    let refv: Vec<f32> = refb
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    let mut dec = AacDecoder::new();
    let mut got: Vec<f32> = Vec::new();
    for f in split_adts(&adts) {
        let pkt = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: f,
            stream_index: 0,
            is_key_frame: true,
        };
        if let Ok(Some(a)) = dec.decode(&pkt) {
            for c in a.data.chunks_exact(4) {
                got.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            }
        }
    }
    let nch = 6;
    let frames = got.len().min(refv.len()) / (1024 * nch);
    const NAMES: [&str; 6] = ["FL", "FR", "FC", "LFE", "FLc", "FRc"];

    for ch in 0..nch {
        let (mut se, mut mx) = (0.0f64, 0.0f64);
        for i in 0..frames * 1024 {
            let d = (got[i * nch + ch] - refv[i * nch + ch]) as f64;
            se += d * d;
            mx = mx.max(d.abs());
        }
        eprintln!(
            "  ch{ch} {:<4} rms={:8.2} LSB  max={:8.2} LSB",
            NAMES[ch],
            (se / (frames * 1024) as f64).sqrt() * 32768.0,
            mx * 32768.0
        );
    }

    eprint!("  worst per-frame max LSB: ");
    for fr in 0..frames.min(24) {
        let mut mx = 0.0f64;
        for i in fr * 1024..(fr + 1) * 1024 {
            for ch in 0..nch {
                mx = mx.max((got[i * nch + ch] - refv[i * nch + ch]).abs() as f64);
            }
        }
        eprint!("{:.1} ", mx * 32768.0);
    }
    eprintln!();
}
