//! Diagnostic (run with `cargo test -p tpt-kinetix-aac --test dbg_localize_sweep -- --nocapture --ignored`).
//! Localizes the `sweep_stereo_44100` single-sample max-diff outlier: per-frame
//! correlation/max-diff, and within the worst frame the worst sample index, so we
//! can tell whether it is one isolated sample (float rounding in a large-coefficient
//! IMDCT) or a specific band.

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

fn planes(frames: &[AudioFrame]) -> Vec<Vec<f32>> {
    let channels = frames.first().map_or(0, |f| f.channels as usize);
    let mut p = vec![Vec::new(); channels];
    for f in frames {
        let s: Vec<f32> = f
            .data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        for (i, v) in s.iter().enumerate() {
            p[i % channels].push(*v);
        }
    }
    p
}

#[test]
#[ignore]
fn localize_sweep() {
    if !ffmpeg_available() {
        eprintln!("skip (ffmpeg unavailable)");
        return;
    }
    let adts = match encode_aac_adts_lavfi(
        "aevalsrc=exprs='sin(2*PI*(200+1900*t)*t)':s=44100:d=1.0",
        2,
        "128k",
    ) {
        Some(a) => a,
        None => {
            eprintln!("skip (encode failed)");
            return;
        }
    };

    let native = decode_native(&adts);
    let reference = decode_aac_with_ffmpeg(&adts).expect("ffmpeg decode");
    assert!(!native.is_empty() && !reference.is_empty());

    let np = planes(&native);
    let rp = planes(&reference);
    let n = &np[0];
    let r = &rp[0];

    let mut worst_frame = 0usize;
    let mut worst_corr = 1.0f64;
    let mut worst_max = 0.0f32;
    for fr in 0..native.len() {
        let ns = fr * 1024;
        let rs = fr * 1024;
        if ns + 1024 > n.len() || rs + 1024 > r.len() {
            break;
        }
        let (mut dot, mut nn, mut rr, mut mx) = (0.0f64, 0.0, 0.0, 0.0f32);
        let mut mx_sample = 0usize;
        for k in 0..1024 {
            let a = n[ns + k] as f64;
            let b = r[rs + k] as f64;
            dot += a * b;
            nn += a * a;
            rr += b * b;
            let d = ((a as f32) - (b as f32)).abs();
            if d > mx {
                mx = d;
                mx_sample = k;
            }
        }
        let corr = if nn > 0.0 && rr > 0.0 {
            dot / (nn.sqrt() * rr.sqrt())
        } else {
            0.0
        };
        if mx > worst_max {
            worst_max = mx;
            worst_frame = fr;
        }
        if corr < worst_corr {
            worst_corr = corr;
        }
        if mx > 0.001 {
            eprintln!(
                "frame {fr:3}: corr={corr:.5} maxdiff={mx:.6} sample={mx_sample}",
            );
        }
    }
    eprintln!("worst frame = {worst_frame} maxdiff={worst_max:.6} corr={worst_corr:.5}");
}
