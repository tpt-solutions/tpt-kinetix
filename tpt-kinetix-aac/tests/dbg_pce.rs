//! `#[ignore]` diagnostic for channel_config-0 PCE streams.
//!   cargo test -p tpt-kinetix-aac --test dbg_pce -- --ignored --nocapture

use std::path::{Path, PathBuf};
use tpt_kinetix_aac::{AacDecoder, Element, RawDataBlock};
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

fn corr(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
    for i in 0..n {
        let (x, y) = (a[i] as f64, b[i] as f64);
        sa += x;
        sb += y;
        saa += x * x;
        sbb += y * y;
        sab += x * y;
    }
    let nf = n as f64;
    let cov = sab - sa * sb / nf;
    let va = (saa - sa * sa / nf).sqrt();
    let vb = (sbb - sb * sb / nf).sqrt();
    if va == 0.0 || vb == 0.0 {
        return 0.0;
    }
    cov / (va * vb)
}

#[test]
#[ignore]
fn dump() {
    for name in ["al15_44", "al22_chCfg0PCE_44", "al17_44"] {
        let Ok(adts) = std::fs::read(root().join(format!("{name}.adts"))) else {
            eprintln!("=== {name}: no fixture");
            continue;
        };
        let frames = split_adts(&adts);
        let f = &frames[0];
        let sf = ((f[2] >> 2) & 0x0F) as usize;
        let hdr_len = if f[1] & 1 == 0 { 9 } else { 7 };
        let block = RawDataBlock::parse(&f[hdr_len..], sf).unwrap();
        let seq: Vec<&str> = block
            .elements
            .iter()
            .map(|e| match e {
                Element::Sce(_) => "SCE",
                Element::Cpe(_) => "CPE",
                Element::Cce(_) => "CCE",
                Element::Lfe(_) => "LFE",
                Element::Fil(_) => "FIL",
                Element::End => "END",
            })
            .collect();
        let tags: Vec<String> = block
            .elements
            .iter()
            .filter_map(|e| match e {
                Element::Sce(s) => Some(format!("SCE#{}", s.instance_tag)),
                Element::Cpe(c) => Some(format!("CPE#{}", c.instance_tag)),
                Element::Lfe(l) => Some(format!("LFE#{}", l.instance_tag)),
                Element::Cce(c) => Some(format!("CCE#{}", c.instance_tag)),
                _ => None,
            })
            .collect();
        eprintln!("=== {name} sf={sf} elements={seq:?} tags={tags:?}");
        eprintln!("    pce={:?}", block.pce);

        let refb = std::fs::read(root().join(format!("{name}.ref.f32"))).unwrap();
        let refv: Vec<f32> = refb
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        let mut dec = AacDecoder::new();
        let mut got: Vec<f32> = Vec::new();
        let mut nch = 0usize;
        for fr in split_adts(&adts) {
            let pkt = Packet {
                pts: Timestamp::NONE,
                dts: Timestamp::NONE,
                data: fr,
                stream_index: 0,
                is_key_frame: true,
            };
            if let Ok(Some(a)) = dec.decode(&pkt) {
                nch = a.channels as usize;
                for c in a.data.chunks_exact(4) {
                    got.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                }
            }
        }
        if nch == 0 {
            continue;
        }
        // de-interleave
        let deint = |v: &[f32], nch: usize| -> Vec<Vec<f32>> {
            let mut ch = vec![Vec::new(); nch];
            for (i, &s) in v.iter().enumerate() {
                ch[i % nch].push(s);
            }
            ch
        };
        let g = deint(&got, nch);
        let r = deint(&refv, nch);
        eprintln!("    corr matrix (got row x ref col):");
        for (gi, gc) in g.iter().enumerate() {
            let row: Vec<String> = r.iter().map(|rc| format!("{:+.2}", corr(gc, rc))).collect();
            eprintln!("      got{gi}: {}", row.join("  "));
        }
        // Per got-channel: best-matching ref channel + least-squares scale fit.
        for (gi, gc) in g.iter().enumerate() {
            let (mut bri, mut bc) = (0usize, -2.0f64);
            for (ri, rc) in r.iter().enumerate() {
                let c = corr(gc, rc);
                if c > bc {
                    bc = c;
                    bri = ri;
                }
            }
            let rc = &r[bri];
            let n = gc.len().min(rc.len());
            let (mut num, mut den) = (0.0f64, 0.0f64);
            for i in 0..n {
                num += gc[i] as f64 * rc[i] as f64;
                den += gc[i] as f64 * gc[i] as f64;
            }
            let scale = if den != 0.0 { num / den } else { 0.0 };
            eprintln!("      got{gi} -> ref{bri} corr={bc:.4} ref/got scale={scale:.4}");
        }
    }
}
