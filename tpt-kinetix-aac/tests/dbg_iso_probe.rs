//! `#[ignore]` diagnostic: per-frame decode-error breakdown for the ISO
//! conformance streams that are still known gaps. Run with:
//!   cargo test -p tpt-kinetix-aac --test dbg_iso_probe -- --ignored --nocapture
//! Needs the fetched fixtures under `tests/fixtures/iso/`.

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
fn probe() {
    for name in [
        "am05_44",
        "am00_88",
        "al17_44",
        "al07_96",
        "al22_chCfg0PCE_44",
        "al06_44",
    ] {
        let Ok(adts) = std::fs::read(root().join(format!("{name}.adts"))) else {
            eprintln!("=== {name}: no fixture, skipped");
            continue;
        };
        let mut dec = AacDecoder::new();
        let mut counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        let mut first_err_frame = None;
        let mut ok_frames = 0u32;
        for (fi, f) in split_adts(&adts).into_iter().enumerate() {
            let pkt = Packet {
                pts: Timestamp::NONE,
                dts: Timestamp::NONE,
                data: f,
                stream_index: 0,
                is_key_frame: true,
            };
            match dec.decode(&pkt) {
                Ok(_) => ok_frames += 1,
                Err(e) => {
                    *counts.entry(format!("{e:?}")).or_default() += 1;
                    if first_err_frame.is_none() {
                        first_err_frame = Some(fi);
                    }
                }
            }
        }
        eprintln!("=== {name}: ok={ok_frames} first_err_frame={first_err_frame:?}");
        let mut v: Vec<_> = counts.into_iter().collect();
        v.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        for (k, c) in v {
            eprintln!("    {c:5}  {k}");
        }
    }
}
