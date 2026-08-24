//! Scratch: decode a mono-noise AAC file with both the native decoder and
//! ffmpeg, and compare per-1024-frame band energy to localize the PNS mismatch.
//! Run: cargo +stable test --example dbg_pns -- --nocapture (then invoke via main)
//! Place adts at C:\Users\phill\AppData\Local\Temp\kilo\mn.aac

use std::process::Command;
use tpt_kinetix_aac::AacDecoder;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;

fn split_adts(data: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut i = 0;
    while i + 7 <= data.len() {
        if data[i] == 0xFF && (data[i + 1] & 0xF0) == 0xF0 {
            let fl = (((data[i + 3] & 0x03) as usize) << 11)
                | ((data[i + 4] as usize) << 3)
                | ((data[i + 5] as usize) >> 5);
            if fl == 0 || i + fl > data.len() {
                break;
            }
            frames.push(data[i..i + fl].to_vec());
            i += fl;
        } else {
            i += 1;
        }
    }
    frames
}

fn f32le(buf: &[u8]) -> Vec<f32> {
    buf.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn main() {
    let path = std::env::var("AAC_DBG_FILE")
        .unwrap_or_else(|_| r"C:\Users\phill\AppData\Local\Temp\kilo\mn.aac".to_string());
    let adts = std::fs::read(&path).unwrap();
    let frames = split_adts(&adts);
    eprintln!("frames={}", frames.len());

    // native, also capture per-frame window_sequence
    let mut dec = AacDecoder::new();
    let mut native_interleaved = Vec::new();
    let mut channels = 1usize;
    for f in &frames {
        let pkt = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: f.clone(),
            stream_index: 0,
            is_key_frame: true,
        };
        if let Ok(Some(fr)) = dec.decode(&pkt) {
            channels = fr.channels as usize;
            native_interleaved.extend_from_slice(&fr.data);
        }
    }
    let native_interleaved = f32le(&native_interleaved);
    // Channel-0 only, de-interleaved, so a stereo file compares like-for-like
    // against reference channel 0 instead of an ffmpeg-side downmix (which
    // would blend both channels and no longer be a clean per-channel diff).
    let native: Vec<f32> = native_interleaved
        .iter()
        .step_by(channels)
        .copied()
        .collect();

    // ffmpeg reference pcm (f32le), same channel count as the source so no
    // downmix happens, then de-interleaved to channel 0 the same way.
    let out = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-i",
            &path,
            "-f",
            "f32le",
            "-ac",
            &channels.to_string(),
            "-",
        ])
        .output()
        .unwrap();
    let reference_interleaved = f32le(&out.stdout);
    let reference: Vec<f32> = reference_interleaved
        .iter()
        .step_by(channels)
        .copied()
        .collect();
    eprintln!(
        "channels={channels} native samples={} ref={}",
        native.len(),
        reference.len()
    );

    // Compare energy in overlapping 1024-sample windows.
    let n = native.len().min(reference.len());
    for w in (0..n).step_by(1024) {
        let ne: f64 = native[w..w + 1024.min(n - w)]
            .iter()
            .map(|x| (*x as f64) * (*x as f64))
            .sum();
        let re: f64 = reference[w..w + 1024.min(n - w)]
            .iter()
            .map(|x| (*x as f64) * (*x as f64))
            .sum();
        let ratio = if re > 1e-9 { ne / re } else { f64::NAN };
        eprintln!(
            "frame@{} native_e={:.4e} ref_e={:.4e} ratio={:.3}",
            w, ne, re, ratio
        );
    }
    // Detailed sample-range dump: native vs reference vs abs-diff, so a
    // known worst-case index (e.g. from `best_aligned_max_diff`'s
    // `AAC_DBG_LOCALIZE` output) can be inspected directly. Range is
    // `AAC_DBG_RANGE=start:end` (default 400:600, the original frame-0
    // spot-check).
    let (rs, re) = std::env::var("AAC_DBG_RANGE")
        .ok()
        .and_then(|s| {
            let (a, b) = s.split_once(':')?;
            Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
        })
        .unwrap_or((400, 600));
    eprintln!("--- samples {rs}..{re} native vs ref (abs-diff) ---");
    for i in rs..re.min(n) {
        eprintln!(
            "  i={i:6}  native={:+.5}  ref={:+.5}  diff={:.5}",
            native[i],
            reference[i],
            (native[i] - reference[i]).abs()
        );
    }
    // Full-buffer max-abs-diff scan (zero lag), so a worst-case index can be
    // found directly instead of guessed.
    let mut max_diff = 0.0f32;
    let mut max_diff_at = 0usize;
    for i in 0..n {
        let d = (native[i] - reference[i]).abs();
        if d > max_diff {
            max_diff = d;
            max_diff_at = i;
        }
    }
    eprintln!("zero-lag max-abs-diff = {max_diff:.5} at i={max_diff_at}");

    // peak
    let nm = native.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
    let rm = reference.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
    eprintln!("native peak={:.4} ref peak={:.4}", nm, rm);

    // zero-lag Pearson correlation over steady-state region (skip frame 0).
    let start = 1024usize;
    let end = n;
    let nn = (end - start) as f64;
    let mean_a: f64 = native[start..end].iter().map(|x| *x as f64).sum::<f64>() / nn;
    let mean_b: f64 = reference[start..end].iter().map(|x| *x as f64).sum::<f64>() / nn;
    let mut cov = 0.0f64;
    let mut va = 0.0f64;
    let mut vb = 0.0f64;
    for i in start..end {
        let a = native[i] as f64 - mean_a;
        let b = reference[i] as f64 - mean_b;
        cov += a * b;
        va += a * a;
        vb += b * b;
    }
    let corr = cov / (va.sqrt() * vb.sqrt());
    eprintln!("zero-lag correlation (steady-state) = {:.4}", corr);

    // Find the best overall lag (dot-product, matching the conformance
    // test's methodology) over a wide search window, then report
    // correlation in short per-frame windows AT THAT FIXED LAG to see
    // whether the match degrades over time (progressive desync) or is
    // uniformly poor from the start (a per-band/local issue).
    let mut best_lag = 0i64;
    let mut best_dot = f64::MIN;
    for lag in -2048i64..=4096 {
        let (ns, rs) = if lag >= 0 {
            (0usize, lag as usize)
        } else {
            ((-lag) as usize, 0usize)
        };
        if ns >= native.len() || rs >= reference.len() {
            continue;
        }
        let len = (native.len() - ns).min(reference.len() - rs);
        if len < 4096 {
            continue;
        }
        let mut dot = 0.0f64;
        for i in 0..len {
            dot += native[ns + i] as f64 * reference[rs + i] as f64;
        }
        if dot > best_dot {
            best_dot = dot;
            best_lag = lag;
        }
    }
    eprintln!("global best lag = {best_lag}");
    let (ns0, rs0) = if best_lag >= 0 {
        (0usize, best_lag as usize)
    } else {
        ((-best_lag) as usize, 0usize)
    };
    let total_len = (native.len() - ns0).min(reference.len() - rs0);
    for w in (0..total_len).step_by(1024) {
        let wl = 1024.min(total_len - w);
        if wl < 256 {
            continue;
        }
        let mut dot = 0.0f64;
        let mut nn = 0.0f64;
        let mut rr = 0.0f64;
        for i in 0..wl {
            let a = native[ns0 + w + i] as f64;
            let b = reference[rs0 + w + i] as f64;
            dot += a * b;
            nn += a * a;
            rr += b * b;
        }
        let c = if nn > 0.0 && rr > 0.0 {
            dot / (nn.sqrt() * rr.sqrt())
        } else {
            f64::NAN
        };
        eprintln!("windowed@{w} corr={:.4}", c);
    }
}
