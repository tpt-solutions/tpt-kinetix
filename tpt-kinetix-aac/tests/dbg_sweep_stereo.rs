//! Debug harness for the sweep_stereo_44100 single-sample outlier.
//!
//! Run with:
//!   cargo test -p tpt-kinetix-aac dbg_sweep_stereo -- --nocapture

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

fn flatten_channels(frames: &[AudioFrame]) -> Vec<Vec<f32>> {
    let channels = frames.first().map_or(0, |f| f.channels as usize);
    let mut planes = vec![Vec::new(); channels];
    for f in frames {
        let samples: Vec<f32> = f
            .data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        for (i, s) in samples.iter().enumerate() {
            planes[i % channels].push(*s);
        }
    }
    planes
}

/// Find the best integer sample lag maximising channel-0 cross-correlation.
fn best_lag(native_planes: &[Vec<f32>], ref_planes: &[Vec<f32>]) -> i64 {
    let n = &native_planes[0];
    let r = &ref_planes[0];
    let margin = 64usize;
    let mut best_lag = 0i64;
    let mut best_corr = f64::MIN;
    for lag in -64i64..=4096 {
        let (n_start, r_start) = if lag >= 0 {
            (0usize, lag as usize)
        } else {
            ((-lag) as usize, 0usize)
        };
        if n_start >= n.len() || r_start >= r.len() {
            continue;
        }
        let len = (n.len() - n_start).min(r.len() - r_start);
        if len <= 2 * margin {
            continue;
        }
        let mut corr = 0.0f64;
        for i in margin..len - margin {
            corr += n[n_start + i] as f64 * r[r_start + i] as f64;
        }
        if corr > best_corr {
            best_corr = corr;
            best_lag = lag;
        }
    }
    best_lag
}

#[test]
fn dbg_sweep_stereo() {
    if !ffmpeg_available() {
        eprintln!("skipped: ffmpeg not available");
        return;
    }

    // Same filter as conformance_aac.rs
    let adts = encode_aac_adts_lavfi(
        "aevalsrc=exprs='sin(2*PI*(200+1900*t)*t)':s=44100:d=1.0",
        2,
        "128k",
    )
    .expect("ffmpeg should encode the sweep");

    let native = decode_native(&adts);
    let reference = decode_aac_with_ffmpeg(&adts).expect("ffmpeg decode");

    eprintln!(
        "native frames={}  ref frames={}",
        native.len(),
        reference.len()
    );

    let np = flatten_channels(&native);
    let rp = flatten_channels(&reference);
    let lag = best_lag(&np, &rp);
    eprintln!("best sample lag = {lag}");

    let channels = np.len();
    let (n_start, r_start) = if lag >= 0 {
        (0usize, lag as usize)
    } else {
        ((-lag) as usize, 0usize)
    };

    // Locate the worst-diff sample across all channels.
    let margin = 64usize;
    let mut worst_diff = 0.0f32;
    let mut worst_aligned_idx = 0usize;
    let mut worst_ch = 0usize;
    for (ch, (nch, rch)) in np.iter().zip(rp.iter()).enumerate() {
        if n_start >= nch.len() || r_start >= rch.len() {
            continue;
        }
        let len = (nch.len() - n_start).min(rch.len() - r_start);
        if len <= 2 * margin {
            continue;
        }
        for i in margin..len - margin {
            let d = (nch[n_start + i] - rch[r_start + i]).abs();
            if d > worst_diff {
                worst_diff = d;
                worst_aligned_idx = i;
                worst_ch = ch;
            }
        }
    }

    let frame_idx = worst_aligned_idx / 1024;
    let intra_frame = worst_aligned_idx % 1024;
    let native_val = np[worst_ch][n_start + worst_aligned_idx];
    let ref_val = rp[worst_ch][r_start + worst_aligned_idx];
    eprintln!(
        "WORST DIFF = {worst_diff:.6}  ch={worst_ch}  aligned_idx={worst_aligned_idx}  \
         frame={frame_idx}  intra={intra_frame}  native={native_val:.6}  ref={ref_val:.6}"
    );

    // Print ±16 samples around the worst sample for each channel.
    for ch in 0..channels {
        let nch = &np[ch];
        let rch = &rp[ch];
        if n_start >= nch.len() || r_start >= rch.len() {
            continue;
        }
        let len = (nch.len() - n_start).min(rch.len() - r_start);
        let lo = worst_aligned_idx.saturating_sub(16);
        let hi = (worst_aligned_idx + 17).min(len);
        eprintln!("  ch={ch}:");
        for i in lo..hi {
            let n = nch[n_start + i];
            let r = rch[r_start + i];
            let marker = if i == worst_aligned_idx { " <<<<" } else { "" };
            eprintln!(
                "    [{i:5}] native={n:+.6}  ref={r:+.6}  diff={:.6}{marker}",
                (n - r).abs()
            );
        }
    }

    // Dump all samples in frames 24-25 with diff > 0.003.
    for fi in 24..=25 {
        let lo = fi * 1024;
        let hi = (fi + 1) * 1024;
        for ch in 0..channels {
            let nch = &np[ch];
            let rch = &rp[ch];
            for i in lo..hi {
                if n_start + i >= nch.len() || r_start + i >= rch.len() {
                    continue;
                }
                let d = (nch[n_start + i] - rch[r_start + i]).abs();
                if d > 0.003 {
                    eprintln!(
                        "  frame{fi} ch{ch} intra={} abs_diff={d:.6} native={:.6} ref={:.6}",
                        i - lo,
                        nch[n_start + i],
                        rch[r_start + i]
                    );
                }
            }
        }
    }

    // Dump max-diff per frame to see if the outlier is isolated to one frame.
    eprintln!("\nPer-frame max-diff (both channels):");
    let total_aligned = (np[0].len() - n_start).min(rp[0].len() - r_start);
    for fi in 0..total_aligned.div_ceil(1024) {
        let lo = fi * 1024;
        let hi = ((fi + 1) * 1024).min(total_aligned);
        let mut frame_max = 0.0f32;
        for ch in 0..channels {
            for i in lo..hi {
                if n_start + i >= np[ch].len() || r_start + i >= rp[ch].len() {
                    continue;
                }
                let d = (np[ch][n_start + i] - rp[ch][r_start + i]).abs();
                if d > frame_max {
                    frame_max = d;
                }
            }
        }
        eprintln!("  frame {fi:2}: max_diff={frame_max:.6}");
    }
}
