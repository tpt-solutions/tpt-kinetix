//! Diagnostic (run with `cargo test -p tpt-kinetix-aac --test dbg_localize_noise_mono -- --nocapture --ignored`).
//! Localizes the `noise_mono_44100` correlation gap: prints per-frame channel-0
//! correlation and max-abs-diff so we can tell whether the error is systemic
//! (every short-window frame) or localized to a few frames.

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

fn best_lag(n: &[f32], r: &[f32]) -> i64 {
    let mut best = 0i64;
    let mut bestc = f64::MIN;
    for lag in -64i64..=2048 {
        let (n0, r0) = if lag >= 0 {
            (0usize, lag as usize)
        } else {
            ((-lag) as usize, 0usize)
        };
        if n0 >= n.len() || r0 >= r.len() {
            continue;
        }
        let len = (n.len() - n0).min(r.len() - r0);
        if len < 4096 {
            continue;
        }
        let (mut dot, mut nn, mut rr) = (0.0f64, 0.0, 0.0);
        for k in 0..len {
            let a = n[n0 + k] as f64;
            let b = r[r0 + k] as f64;
            dot += a * b;
            nn += a * a;
            rr += b * b;
        }
        if nn > 0.0 && rr > 0.0 {
            let c = dot / (nn.sqrt() * rr.sqrt());
            if c > bestc {
                bestc = c;
                best = lag;
            }
        }
    }
    best
}

#[test]
#[ignore]
fn localize_noise_mono() {
    if !ffmpeg_available() {
        eprintln!("skip (ffmpeg unavailable)");
        return;
    }
    let adts = match encode_aac_adts_lavfi(
        "anoisesrc=duration=1.0:sample_rate=44100:amplitude=0.8:color=white:seed=1",
        1,
        "96k",
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
    eprintln!(
        "native frames={} channels={} | ref frames={} channels={}",
        native.len(),
        np.len(),
        reference.len(),
        rp.len()
    );

    let n = &np[0];
    let r = &rp[0];
    let lag = best_lag(n, r);
    eprintln!("global best lag = {lag}");

    let (n0, r0) = if lag >= 0 {
        (0usize, lag as usize)
    } else {
        ((-lag) as usize, 0usize)
    };
    let len = (n.len() - n0).min(r.len() - r0);

    let mut worst_frame = 0usize;
    let mut worst_corr = 1.0f64;
    for fr in 0..native.len() {
        let ns = n0 + fr * 1024;
        let rs = r0 + fr * 1024;
        if ns + 1024 > n.len() || rs + 1024 > r.len() {
            break;
        }
        // Search a small per-frame lag to detect a constant time offset (phase bug).
        let mut best_fr_lag = 0i64;
        let mut best_fr_corr = 0.0f64;
        for fl in -8i64..=8 {
            let (a0, b0) = if fl >= 0 {
                (ns, rs + fl as usize)
            } else {
                (ns + (-fl) as usize, rs)
            };
            if a0 + 1024 > n.len() || b0 + 1024 > r.len() {
                continue;
            }
            let (mut dot, mut nn, mut rr) = (0.0f64, 0.0, 0.0);
            for k in 0..1024 {
                let a = n[a0 + k] as f64;
                let b = r[b0 + k] as f64;
                dot += a * b;
                nn += a * a;
                rr += b * b;
            }
            if nn > 0.0 && rr > 0.0 {
                let c = dot / (nn.sqrt() * rr.sqrt());
                if c > best_fr_corr {
                    best_fr_corr = c;
                    best_fr_lag = fl;
                }
            }
        }
        // Recompute maxdiff at the best per-frame lag.
        let (a0, b0) = if best_fr_lag >= 0 {
            (ns, rs + best_fr_lag as usize)
        } else {
            (ns + (-best_fr_lag) as usize, rs)
        };
        let mut mx = 0.0f32;
        for k in 0..1024 {
            let d = ((n[a0 + k] - r[b0 + k]) as f32).abs();
            if d > mx {
                mx = d;
            }
        }
        eprintln!(
            "frame {fr:3}: corr={best_fr_corr:.4} maxdiff={mx:.4} fl={best_fr_lag:+}",
        );
        if best_fr_corr < worst_corr {
            worst_corr = best_fr_corr;
            worst_frame = fr;
        }
    }
    eprintln!("worst frame = {worst_frame} corr={worst_corr:.4}");
}
