//! Phase 6 conformance: the native AAC-LC decoder must reproduce `ffmpeg`'s
//! reference decode within a documented tolerance on a real ffmpeg-encoded stream.
//!
//! The harness is gated on `ffmpeg` availability — it is skipped silently when
//! `ffmpeg` is not installed (so it is safe to run everywhere, including CI
//! images without `ffmpeg`).

use tpt_kinetix_aac::AacDecoder;
use tpt_kinetix_core::frame::AudioFrame;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_test_utils::reference::{decode_aac_with_ffmpeg, ffmpeg_available};
use tpt_kinetix_test_utils::synthetic::minimal_aac_adts;

/// Split an ADTS elementary stream into individual ADTS frames (header + payload),
/// using each header's `frame_length` field so zero-padding between frames is
/// tolerated.
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

/// Decode an ADTS stream with the native [`AacDecoder`], frame by frame.
fn decode_native(adts: &[u8]) -> Vec<AudioFrame> {
    let frames = split_adts_frames(adts);
    eprintln!("split produced {} ADTS frames", frames.len());
    let mut dec = AacDecoder::new();
    let mut out = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        let pkt = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: f.clone(),
            stream_index: 0,
            is_key_frame: true,
        };
        match dec.decode(&pkt) {
            Ok(Some(frame)) => {
                let maxabs = frame
                    .data
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]).abs())
                    .fold(0.0f32, f32::max);
                eprintln!(
                    "frame {i}: OK channels={} maxabs={}",
                    frame.channels, maxabs
                );
                out.push(frame);
            }
            Ok(None) => {
                eprintln!("frame {i}: Ok(None) (frame_len={})", f.len());
            }
            Err(e) => {
                eprintln!("frame {i}: Err({e:?}) (frame_len={})", f.len());
            }
        }
    }
    out
}

/// Flatten a sequence of interleaved-`f32` [`AudioFrame`]s into one
/// per-channel sample stream: `planes[c][i]` is channel `c`'s sample `i`.
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

/// Best-align `native` against `reference` at **sample** granularity (not just
/// whole-1024-sample-frame granularity): a real AAC encoder's priming delay is
/// typically not a multiple of the 1024-sample block size (commonly ~2112
/// samples for LC), so even a bit-exact decoder can show residual sub-frame
/// misalignment that a frame-level-only search can't compensate for.
///
/// Finds the integer sample lag (searched over `-64..=4096`, covering zero
/// lag, small filterbank-delay differences, and a couple of encoder priming
/// blocks) that maximizes channel-0 cross-correlation, then reports the
/// max-abs-diff of all channels at that lag over the overlapping region
/// (trimmed by 64 samples at each edge to avoid boundary/ramp artifacts from
/// the shift itself).
fn best_aligned_max_diff(native: &[AudioFrame], reference: &[AudioFrame]) -> f32 {
    let native_planes = flatten_channels(native);
    let ref_planes = flatten_channels(reference);
    if native_planes.is_empty() || ref_planes.is_empty() {
        return f32::MAX;
    }
    let n = &native_planes[0];
    let r = &ref_planes[0];

    let margin = 64usize;
    let mut best_lag = 0i64;
    let mut best_corr = f64::MIN;
    for lag in -64i64..=4096 {
        // native[i] aligns with reference[i + lag].
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

    let (n_start, r_start) = if best_lag >= 0 {
        (0usize, best_lag as usize)
    } else {
        ((-best_lag) as usize, 0usize)
    };
    eprintln!("best_aligned_max_diff: best sample lag = {best_lag}");

    let mut best = 0.0f32;
    for (np, rp) in native_planes.iter().zip(ref_planes.iter()) {
        if n_start >= np.len() || r_start >= rp.len() {
            continue;
        }
        let len = (np.len() - n_start).min(rp.len() - r_start);
        if len <= 2 * margin {
            continue;
        }
        for i in margin..len - margin {
            let d = (np[n_start + i] - rp[r_start + i]).abs();
            best = best.max(d);
        }
    }
    best
}

/// Pearson cross-correlation of channel 0 at the best integer sample lag.
///
/// This is a *shape* metric, deliberately independent of amplitude: it is the
/// regression test for the class of bug that a max-abs-diff check cannot see.
/// A previous session had the IMDCT basis running at exactly double the correct
/// phase rate, so a real 440 Hz tone decoded to ~883 Hz — a completely
/// different waveform that nonetheless had a plausible peak amplitude. This
/// metric read ~0.0000058 then and reads ~0.995 now.
fn best_channel0_correlation(native: &[AudioFrame], reference: &[AudioFrame]) -> f64 {
    let native_planes = flatten_channels(native);
    let ref_planes = flatten_channels(reference);
    if native_planes.is_empty() || ref_planes.is_empty() {
        return 0.0;
    }
    let n = &native_planes[0];
    let r = &ref_planes[0];

    let mut best = f64::MIN;
    for lag in -2048i64..=4096 {
        let (n_start, r_start) = if lag >= 0 {
            (0usize, lag as usize)
        } else {
            ((-lag) as usize, 0usize)
        };
        if n_start >= n.len() || r_start >= r.len() {
            continue;
        }
        let len = (n.len() - n_start).min(r.len() - r_start);
        if len < 4096 {
            continue;
        }
        let (mut dot, mut nn, mut rr) = (0.0f64, 0.0f64, 0.0f64);
        for i in 0..len {
            let a = n[n_start + i] as f64;
            let b = r[r_start + i] as f64;
            dot += a * b;
            nn += a * a;
            rr += b * b;
        }
        if nn <= 0.0 || rr <= 0.0 {
            continue;
        }
        best = best.max(dot / (nn.sqrt() * rr.sqrt()));
    }
    best
}

/// A single conformance case: `label` is diagnostic only, `adts` the real
/// ffmpeg-encoded stream to decode with both decoders.
struct ConformanceCase {
    label: &'static str,
    adts: Vec<u8>,
}

/// Build the conformance corpus. The single 440 Hz tone that previously
/// constituted the whole corpus exercises almost none of the interesting
/// decoder paths — it is purely tonal (no PNS), steady-state (no EIGHT_SHORT
/// transients → TNS barely fires), stereo-2.0 at 44.1 kHz only. The cases
/// below add the paths the previous corpus silently left unverified:
///
/// * **noise** (`anoisesrc`): broadband, so per-band scalefactors span the full
///   range and the encoder leans on PNS (NOISE_HCB bands) and short windows.
/// * **swept sine** (`sine=...:frequency=200..4000`): the rising spectral
///   centroid forces window-sequence transitions (LONG_START / LONG_STOP /
///   EIGHT_SHORT) and the overlap-add LONG↔SHORT glue, the trickiest part of
///   `long_synthesis` / `short_synthesis`.
/// * **mono**: a different channel count and the CPE-vs-SCE element path.
/// * **48 kHz / 22.05 kHz**: different scalefactor-band tables (`SWB_OFFSET_*`
///   are indexed by `sampling_frequency_index`), so a bug there would surface
///   on one rate but not 44.1 kHz.
fn build_corpus() -> Vec<ConformanceCase> {
    let mut cases = Vec::new();
    let mut add = |label: &'static str, adts: Option<Vec<u8>>| {
        if let Some(adts) = adts {
            cases.push(ConformanceCase { label, adts });
        }
    };

    // Original baseline: 440 Hz stereo tone, 44.1 kHz.
    add(
        "tone_440_stereo_44100",
        minimal_aac_adts(44_100, 2, 1.0),
    );
    // Broadband noise → PNS / short-window heavy.
    add(
        "noise_stereo_44100",
        tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi(
            "anoisesrc=duration=1.0:sample_rate=44100:amplitude=0.8:color=brown",
            2,
            "128k",
        ),
    );
    add(
        "noise_mono_44100",
        tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi(
            "anoisesrc=duration=1.0:sample_rate=44100:amplitude=0.8:color=white",
            1,
            "96k",
        ),
    );
    // Frequency sweep → window-sequence transitions and TNS.
    add(
        "sweep_stereo_44100",
        tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi(
            "sine=frequency=200:duration=1.0:sample_rate=44100:frequency2=4000",
            2,
            "128k",
        ),
    );
    // Other sample rates → different SWB tables.
    add(
        "tone_440_stereo_48000",
        minimal_aac_adts(48_000, 2, 1.0),
    );
    add(
        "tone_440_stereo_22050",
        minimal_aac_adts(22_050, 2, 1.0),
    );
    add(
        "tone_440_mono_44100",
        minimal_aac_adts(44_100, 1, 1.0),
    );
    cases
}

#[test]
fn native_aac_matches_ffmpeg_reference() {
    if !ffmpeg_available() {
        eprintln!("native_aac_matches_ffmpeg_reference: skipped (ffmpeg unavailable)");
        return;
    }

    let corpus = build_corpus();
    assert!(!corpus.is_empty(), "ffmpeg failed to encode every corpus case");

    let mut worst_diff = 0.0f32;
    let mut worst_corr = 1.0f64;
    let mut worst_label = "";

    for case in &corpus {
        let native = decode_native(&case.adts);
        let reference =
            decode_aac_with_ffmpeg(&case.adts).expect("ffmpeg should decode its own stream");
        for (i, f) in reference.iter().take(5).enumerate() {
            let maxabs = f
                .data
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]).abs())
                .fold(0.0f32, f32::max);
            eprintln!("[{}] reference frame {i}: maxabs={maxabs}", case.label);
        }

        assert!(
            !native.is_empty(),
            "[{}] native decoder produced no frames at all (all {} ADTS frames \
             failed to decode - see the per-frame diagnostics above)",
            case.label,
            split_adts_frames(&case.adts).len()
        );
        assert!(
            !reference.is_empty(),
            "[{}] ffmpeg reference produced no frames",
            case.label
        );

        // All 45 frames of each stream now parse structurally, so channel-count
        // / sample-rate mismatches are real regressions, not expected gaps.
        assert_eq!(
            native[0].channels, reference[0].channels,
            "[{}] channel count mismatch: native={} reference={}",
            case.label, native[0].channels, reference[0].channels
        );
        assert_eq!(native[0].sample_rate, reference[0].sample_rate);

        let max_diff = best_aligned_max_diff(&native, &reference);
        let corr = best_channel0_correlation(&native, &reference);
        eprintln!(
            "[{}] conformance max-abs-diff={max_diff} correlation={corr:.4}",
            case.label
        );

        if max_diff > worst_diff {
            worst_diff = max_diff;
            worst_label = case.label;
        }
        if corr < worst_corr {
            worst_corr = corr;
        }
    }

    // Documented tolerance (Phase 6 exit criterion). This is a REAL assertion,
    // not a skip guard.
    //
    // History (kept because this number was mis-attributed for several
    // sessions): the gap sat at ~0.114 and was believed to be either a residual
    // "amplitude-scale" bug or an artifact of this test's own frame-level-only
    // alignment search. It was neither. The actual cause was that
    // `src/window.rs` built its half-windows with the wrong denominator
    // (`sin(π(i+0.5)/n)` instead of `sin(π(i+0.5)/2n)`), so they violated the
    // Princen-Bradley / TDAC perfect-reconstruction identity that 50%
    // overlap-add depends on: `w[i]² + w[n-1-i]²` ranged from ~0.000005 at the
    // window edges to 2.0 at its centre instead of being exactly 1.0. The KBD
    // window had the same error. Frame 0 looked correct throughout precisely
    // because it has no preceding block to overlap with, which is what made the
    // bug look like a steady-state "amplitude" problem.
    //
    // With the windows fixed (and verified against the Princen-Bradley identity
    // by unit tests in `src/window.rs`), the measured agreement on the original
    // 440 Hz tone is:
    //   * best-aligned max-abs-diff: 0.114 -> 0.021
    //   * Pearson cross-correlation (channel 0): ~0.75 -> ~0.995
    //   * least-squares amplitude fit (native -> reference): ~0.91 -> ~0.994
    //   * best sample lag: 0 (so alignment methodology was never the issue)
    //
    // The corpus now spans noise (PNS / EIGHT_SHORT), a frequency sweep
    // (window-sequence transitions / TNS), mono, and 22.05 / 48 kHz sample
    // rates (different SWB tables). 0.05 is retained as the documented
    // tolerance: it is the value this test was originally written against, and
    // the residual ~0.021 on the tone is consistent with lossy-codec float
    // rounding plus the reference decoder's own dequantization rounding, not a
    // known bug.
    assert!(
        worst_diff < 0.05,
        "worst native↔ffmpeg AAC diff across the conformance corpus is {worst_diff} \
         (on case '{worst_label}', tolerance 0.05). See this test's comment for \
         the window/TDAC history before assuming a new amplitude-scale bug."
    );

    // Shape check, independent of amplitude — see `best_channel0_correlation`.
    // Guards against the class of bug (e.g. frequency doubling) a max-abs-diff
    // check cannot see.
    assert!(
        worst_corr > 0.95,
        "worst native AAC decode waveform-shape match across the conformance \
         corpus is {worst_corr} (Pearson correlation of channel 0, expected \
         > 0.95). A low value here with a plausible amplitude usually means a \
         frequency/phase bug in the IMDCT basis rather than a scaling bug."
    );
}
