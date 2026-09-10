//! `#[ignore]` diagnostic: is the `al17_44` / `al22` residual a channel
//! permutation/scale issue (like `al06` was) or a real reconstruction gap?

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
#[ignore = "diagnostic"]
#[allow(clippy::needless_range_loop)]
fn localize() {
    for (name, nch) in [
        ("al17_44", 2usize),
        ("al22_chCfg0PCE_44", 8),
        ("al15_44", 6),
    ] {
        let Ok(adts) = std::fs::read(root().join(format!("{name}.adts"))) else {
            continue;
        };
        let refb = std::fs::read(root().join(format!("{name}.ref.f32"))).unwrap();
        let refv: Vec<f32> = refb
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let mut dec = AacDecoder::new();
        let mut nat: Vec<f32> = Vec::new();
        for f in split_adts(&adts) {
            let pkt = Packet {
                pts: Timestamp::NONE,
                dts: Timestamp::NONE,
                data: f,
                stream_index: 0,
                is_key_frame: true,
            };
            if let Ok(Some(fr)) = dec.decode(&pkt) {
                for c in fr.data.chunks_exact(4) {
                    nat.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                }
            }
        }
        eprintln!(
            "=== {name}: native {} samples, ref {} (nch {nch})",
            nat.len(),
            refv.len()
        );
        if nat.is_empty() {
            continue;
        }
        let np: Vec<Vec<f32>> = (0..nch)
            .map(|c| nat.iter().skip(c).step_by(nch).copied().collect())
            .collect();
        let rp: Vec<Vec<f32>> = (0..nch)
            .map(|c| refv.iter().skip(c).step_by(nch).copied().collect())
            .collect();
        for a in 0..nch {
            for b in 0..nch {
                let len = np[a].len().min(rp[b].len());
                if len < 2048 {
                    continue;
                }
                let (mut sn, mut sr, mut snr) = (0.0f64, 0.0, 0.0);
                for i in 0..len {
                    sn += (np[a][i] as f64).powi(2);
                    sr += (rp[b][i] as f64).powi(2);
                    snr += np[a][i] as f64 * rp[b][i] as f64;
                }
                let corr = snr / (sn.sqrt() * sr.sqrt()).max(1e-30);
                if corr.abs() > 0.3 {
                    eprintln!(
                        "  nat ch{a} <-> ref ch{b}: corr={corr:.5} scale={:.5}",
                        snr / sn.max(1e-30)
                    );
                }
            }
        }
    }
}
