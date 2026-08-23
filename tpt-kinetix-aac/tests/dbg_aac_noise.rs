//! Temporary debug harness for the mono white-noise PNS conformance gap.

use tpt_kinetix_aac::AacDecoder;
use tpt_kinetix_core::frame::AudioFrame;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_test_utils::reference::{decode_aac_with_ffmpeg, ffmpeg_available};
use tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi;

fn split_adts_frames(adts: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut i = 0;
    while i + 7 <= adts.len() {
        if adts[i] == 0xFF && (adts[i + 1] & 0xF0) == 0xF0 {
            let frame_len = (((adts[i + 3] & 0x03) as usize) << 11)
                | ((adts[i + 4] as usize) << 3)
                | ((adts[i + 5] as usize) >> 5);
            if frame_len == 0 || i + frame_len > adts.len() {
                break;
            }
            frames.push(adts[i..i + frame_len].to_vec());
            i += frame_len;
        } else {
            i += 1;
        }
    }
    frames
}

fn decode_native(adts: &[u8]) -> Vec<AudioFrame> {
    let frames = split_adts_frames(adts);
    let mut dec = AacDecoder::new();
    let mut out = Vec::new();
    for f in &frames {
        let pkt = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: f.clone(),
            stream_index: 0,
            is_key_frame: true,
        };
        if let Ok(Some(frame)) = dec.decode(&pkt) {
            out.push(frame);
        }
    }
    out
}

fn rms(frames: &[AudioFrame]) -> Vec<f32> {
    frames
        .iter()
        .map(|f| {
            let mut s = 0.0f64;
            let mut n = 0;
            for c in f.data.chunks_exact(4) {
                let v = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                s += (v as f64) * (v as f64);
                n += 1;
            }
            (s / n as f64).sqrt() as f32
        })
        .collect()
}

#[test]
fn dbg_noise_mono() {
    if !ffmpeg_available() {
        return;
    }
    let adts = encode_aac_adts_lavfi(
        "anoisesrc=duration=1.0:sample_rate=44100:amplitude=0.8:color=white",
        1,
        "96k",
    )
    .expect("encode");
    let native = decode_native(&adts);
    let reference = decode_aac_with_ffmpeg(&adts).expect("ffmpeg");
    let nr = rms(&native);
    let rr = rms(&reference);
    for i in 0..nr.len().min(rr.len()) {
        eprintln!("frame {i}: native_rms={:.4} ref_rms={:.4}", nr[i], rr[i]);
    }

    // Find worst single sample and its location.
    let mut worst = 0.0f32;
    let mut worst_frame = 0usize;
    let mut worst_idx = 0usize;
    for (fi, f) in native.iter().enumerate() {
        for (si, c) in f.data.chunks_exact(4).enumerate() {
            let v = f32::from_le_bytes([c[0], c[1], c[2], c[3]]).abs();
            if v > worst {
                worst = v;
                worst_frame = fi;
                worst_idx = si;
            }
        }
    }
    eprintln!(
        "native WORST sample={worst} at frame {worst_frame} sample {worst_idx} ({} ch)",
        native.first().map_or(0, |f| f.channels)
    );
    eprintln!(
        "counts: native={} reference={} native_ch={} ref_ch={}",
        native.len(),
        reference.len(),
        native.first().map_or(0, |f| f.channels),
        reference.first().map_or(0, |f| f.channels)
    );
    // total samples
    let nsamp: usize = native.iter().map(|f| f.data.len() / 4).sum();
    let rsamp: usize = reference.iter().map(|f| f.data.len() / 4).sum();
    eprintln!("total samples: native={nsamp} reference={rsamp}");

    // Dump first 24 samples of frame 5 (native vs reference) to see divergence.
    if native.len() > 5 && reference.len() > 5 {
        let nf = &native[5].data;
        let rf = &reference[5].data;
        eprintln!("frame 5 (native vs ref):");
        for i in 0..24 {
            let nv = f32::from_le_bytes([nf[4 * i], nf[4 * i + 1], nf[4 * i + 2], nf[4 * i + 3]]);
            let rv = f32::from_le_bytes([rf[4 * i], rf[4 * i + 1], rf[4 * i + 2], rf[4 * i + 3]]);
            eprintln!("  [{i:2}] n={nv:+.4} r={rv:+.4} d={:.4}", nv - rv);
        }
    }

    // Reference worst sample for comparison.
    let mut rworst = 0.0f32;
    let mut rframe = 0usize;
    for (fi, f) in reference.iter().enumerate() {
        for c in f.data.chunks_exact(4) {
            let v = f32::from_le_bytes([c[0], c[1], c[2], c[3]]).abs();
            if v > rworst {
                rworst = v;
                rframe = fi;
            }
        }
    }
    eprintln!("reference WORST sample={rworst} at frame {rframe}");
}
