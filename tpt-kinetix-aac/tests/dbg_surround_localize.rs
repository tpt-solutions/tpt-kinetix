//! Scratch diagnostic: localize the `surround_51` residual (0.0023, corr 1.0)
//! to a specific output channel + frame. Delete once the intensity-stereo /
//! multi-element gap is root-caused.

use tpt_kinetix_aac::AacDecoder;
use tpt_kinetix_core::{Packet, Timestamp};

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

fn planes(interleaved: &[f32], ch: usize) -> Vec<Vec<f32>> {
    let mut p = vec![Vec::new(); ch];
    for (i, &s) in interleaved.iter().enumerate() {
        p[i % ch].push(s);
    }
    p
}

#[test]
#[ignore = "diagnostic"]
fn localize_surround_51() {
    let Some(adts) = tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi(
        "aevalsrc=exprs='0.3*sin(2*PI*200*t)|0.3*sin(2*PI*400*t)|0.2*sin(2*PI*600*t)|0.1*sin(2*PI*80*t)|0.25*sin(2*PI*900*t)|0.25*sin(2*PI*1100*t)':channel_layout=5.1:s=44100:d=1.0",
        6,
        "256k",
    ) else {
        eprintln!("ffmpeg unavailable");
        return;
    };

    // Per-frame element structure for the first 5 frames.
    use tpt_kinetix_aac::syntax::Element;
    use tpt_kinetix_aac::{AdtsHeader, RawDataBlock};
    for (fi, f) in split_adts(&adts).iter().enumerate().take(5) {
        let hdr = AdtsHeader::parse(f).unwrap();
        let block =
            RawDataBlock::parse(&f[hdr.header_len..], hdr.sampling_frequency_index as usize)
                .unwrap();
        eprint!("frame {fi}: ");
        for el in &block.elements {
            match el {
                Element::Sce(e) => eprint!(
                    "SCE(tag{},{:?},bt_int={}) ",
                    e.instance_tag,
                    e.stream.ics.window_sequence,
                    e.stream.band_type.iter().any(|&b| b == 14 || b == 15)
                ),
                Element::Cpe(e) => {
                    eprint!(
                        "CPE(tag{}, ms={}, L={:?} R={:?}, groups L{} R{} sfg L{} R{}, Rbt={:?}) ",
                        e.instance_tag,
                        e.ms_mask_present,
                        e.left.ics.window_sequence,
                        e.right.ics.window_sequence,
                        e.left.ics.num_window_groups(),
                        e.right.ics.num_window_groups(),
                        e.left.ics.scale_factor_grouping,
                        e.right.ics.scale_factor_grouping,
                        e.right.band_type,
                    )
                }
                Element::Lfe(e) => eprint!("LFE(tag{}) ", e.instance_tag),
                _ => {}
            }
        }
        eprintln!();
    }

    let mut dec = AacDecoder::new();
    let mut native: Vec<f32> = Vec::new();
    for f in split_adts(&adts) {
        let pkt = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: f,
            stream_index: 0,
            is_key_frame: true,
        };
        if let Ok(Some(frame)) = dec.decode(&pkt) {
            for c in frame.data.chunks_exact(4) {
                native.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            }
        }
    }

    let reference =
        tpt_kinetix_test_utils::reference::decode_aac_with_ffmpeg(&adts).expect("ffmpeg decode");
    let mut refv: Vec<f32> = Vec::new();
    for fr in &reference {
        for c in fr.data.chunks_exact(4) {
            refv.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
        }
    }

    let ch = 6;
    let np = planes(&native, ch);
    let rp = planes(&refv, ch);
    let names = ["FL", "FR", "FC", "LFE", "BL", "BR"];

    for (c, name) in names.iter().enumerate() {
        let n = &np[c];
        let r = &rp[c];
        let len = n.len().min(r.len());
        let frames = len / 1024;
        let mut worst = 0.0f32;
        let mut worst_fr = 0usize;
        let mut per_frame = Vec::new();
        for fr in 0..frames {
            let mut fmax = 0.0f32;
            for i in fr * 1024..(fr + 1) * 1024 {
                let d = (n[i] - r[i]).abs();
                if d > fmax {
                    fmax = d;
                }
            }
            per_frame.push(fmax);
            if fmax > worst {
                worst = fmax;
                worst_fr = fr;
            }
        }
        let head: Vec<String> = per_frame[..per_frame.len().min(8)]
            .iter()
            .map(|v| format!("{v:.5}"))
            .collect();
        eprintln!("ch {c} {name}: worst {worst:.6} @ frame {worst_fr} | first 8 frames: {head:?}");

        // For the worst channel, show WHERE in frames 1 & 2 the error sits.
        // 128-sample buckets: EIGHT_SHORT output regions map to short windows
        // roughly as nflat_ls(448) + window*128.
        if worst > 1e-5 {
            for fr in 1..=2usize {
                let base = fr * 1024;
                let buckets: Vec<String> = (0..8)
                    .map(|b| {
                        let mut m = 0.0f32;
                        for i in base + b * 128..base + (b + 1) * 128 {
                            m = m.max((n[i] - r[i]).abs());
                        }
                        format!("{m:.5}")
                    })
                    .collect();
                eprintln!("    frame {fr} (128-buckets): {}", buckets.join(" "));
            }
        }
    }
}
