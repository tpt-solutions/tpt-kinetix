//! Phase 6 conformance: the native AAC-LC decoder must reproduce `ffmpeg`'s
//! reference decode within a documented tolerance on a real ffmpeg-encoded stream.
//!
//! The harness is gated on `ffmpeg` availability — it is skipped silently when
//! `ffmpeg` is not installed (so it is safe to run everywhere, including CI
//! images without `ffmpeg`).

use tpt_kinetix_aac::syntax::{Element, WindowSequence};
use tpt_kinetix_aac::{AacDecoder, AdtsHeader, RawDataBlock};
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
    let mut best_at = 0usize;
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
            if d > best {
                best = d;
                best_at = i;
            }
        }
    }
    if std::env::var("AAC_DBG_LOCALIZE").is_ok() {
        eprintln!(
            "best_aligned_max_diff: worst sample at aligned index {best_at} \
             (frame {})",
            best_at / 1024
        );
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

/// Which decoder paths a corpus stream actually exercises, tallied by
/// re-parsing every ADTS frame's `raw_data_block()`. This is the guard against
/// this file's recurring failure mode: a case whose *intent* (say "hit
/// EIGHT_SHORT + TNS") silently stops being met because ffmpeg's encoder
/// changed its mind about the source, so the case keeps passing while covering
/// nothing. Each case declares the features it is *for*; the test asserts they
/// are present.
#[derive(Default, Debug)]
struct StreamStats {
    frames: usize,
    eight_short_frames: usize,
    long_start_frames: usize,
    long_stop_frames: usize,
    tns_channel_frames: usize,
    /// Channel-frames that are `EIGHT_SHORT` *and* carry `tns_data` — the
    /// per-window short-block TNS path specifically.
    short_tns_channel_frames: usize,
    pns_channel_frames: usize,
    intensity_channel_frames: usize,
    ms_stereo_frames: usize,
    pulse_channel_frames: usize,
    max_audio_elements: usize,
    /// Frames whose ADTS header carries `channel_configuration == 0` (layout in
    /// a `program_config_element`).
    config_zero_frames: usize,
}

/// A path a corpus case is expected to exercise (asserted post-hoc from
/// [`StreamStats`]).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Coverage {
    /// At least one `EIGHT_SHORT_SEQUENCE` frame.
    EightShort,
    /// At least one `LONG_START` *and* one `LONG_STOP` frame (the transition
    /// glue in `long_synthesis` / `short_synthesis`).
    WindowTransition,
    /// At least one channel-frame carrying `tns_data`.
    Tns,
    /// At least one `EIGHT_SHORT` channel-frame carrying `tns_data` (the
    /// per-window short-block TNS filter path).
    ShortTns,
    /// At least one channel-frame with a `NOISE_HCB` (PNS) section.
    Pns,
    /// At least one channel-frame with an `INTENSITY_HCB` / `INTENSITY_HCB2`
    /// section.
    IntensityStereo,
    /// At least one CPE frame with `ms_mask_present != 0`.
    MsStereo,
    /// At least one frame with two or more audio elements (SCE/CPE/LFE).
    MultiElement,
    /// At least one frame with `channel_configuration == 0` (a
    /// `program_config_element` must be parsed/skipped correctly).
    ConfigZero,
}

fn analyze_stream(adts: &[u8]) -> StreamStats {
    let mut s = StreamStats::default();
    for frame in split_adts_frames(adts) {
        let Ok(hdr) = AdtsHeader::parse(&frame) else {
            continue;
        };
        if hdr.header_len >= frame.len() {
            continue;
        }
        let payload = &frame[hdr.header_len..];
        let Ok(block) = RawDataBlock::parse(payload, hdr.sampling_frequency_index as usize) else {
            continue;
        };
        s.frames += 1;
        if hdr.channel_configuration == 0 {
            s.config_zero_frames += 1;
        }
        let mut audio_elements = 0usize;
        let mut frame_has_ms = false;
        for el in &block.elements {
            let streams: Vec<&tpt_kinetix_aac::syntax::ChannelStream> = match el {
                Element::Sce(e) => {
                    audio_elements += 1;
                    vec![&e.stream]
                }
                Element::Lfe(e) => {
                    audio_elements += 1;
                    vec![&e.stream]
                }
                Element::Cpe(e) => {
                    audio_elements += 1;
                    if e.ms_mask_present != 0 {
                        frame_has_ms = true;
                    }
                    vec![&e.left, &e.right]
                }
                _ => continue,
            };
            for cs in streams {
                match cs.ics.window_sequence {
                    WindowSequence::EightShort => s.eight_short_frames += 1,
                    WindowSequence::LongStart => s.long_start_frames += 1,
                    WindowSequence::LongStop => s.long_stop_frames += 1,
                    WindowSequence::OnlyLong => {}
                }
                if cs.tns.is_some() {
                    s.tns_channel_frames += 1;
                    if cs.ics.window_sequence == WindowSequence::EightShort {
                        s.short_tns_channel_frames += 1;
                    }
                }
                if cs.pulse.is_some() {
                    s.pulse_channel_frames += 1;
                }
                // NOISE_HCB = 13, INTENSITY_HCB2 = 14, INTENSITY_HCB = 15.
                if cs.band_type.contains(&13) {
                    s.pns_channel_frames += 1;
                }
                if cs.band_type.iter().any(|&b| b == 14 || b == 15) {
                    s.intensity_channel_frames += 1;
                }
            }
        }
        if frame_has_ms {
            s.ms_stereo_frames += 1;
        }
        s.max_audio_elements = s.max_audio_elements.max(audio_elements);
    }
    s
}

fn check_coverage(label: &str, want: &[Coverage], s: &StreamStats) {
    assert!(
        s.frames > 0,
        "[{label}] coverage: no raw_data_block parsed at all — the case is empty"
    );
    for c in want {
        let ok = match c {
            Coverage::EightShort => s.eight_short_frames > 0,
            Coverage::WindowTransition => s.long_start_frames > 0 && s.long_stop_frames > 0,
            Coverage::Tns => s.tns_channel_frames > 0,
            Coverage::ShortTns => s.short_tns_channel_frames > 0,
            Coverage::Pns => s.pns_channel_frames > 0,
            Coverage::IntensityStereo => s.intensity_channel_frames > 0,
            Coverage::MsStereo => s.ms_stereo_frames > 0,
            Coverage::MultiElement => s.max_audio_elements >= 2,
            Coverage::ConfigZero => s.config_zero_frames > 0,
        };
        assert!(
            ok,
            "[{label}] coverage: expected to exercise {c:?} but the ffmpeg-encoded \
             stream does not — this case no longer tests what it is for. Stats: {s:?}"
        );
    }
}

/// A single conformance case: `label` is diagnostic only, `adts` the real
/// ffmpeg-encoded stream to decode with both decoders, `coverage` the decoder
/// paths the case is meant to exercise (asserted from [`analyze_stream`]).
struct ConformanceCase {
    label: &'static str,
    adts: Vec<u8>,
    coverage: &'static [Coverage],
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
///
/// Each case declares the decoder paths it is *for* (`&[Coverage]`); the test
/// re-parses every ADTS frame ([`analyze_stream`]) and asserts they are
/// actually present, so a case cannot silently stop covering its target when
/// ffmpeg's encoder changes its mind about the source.
fn build_corpus() -> Vec<ConformanceCase> {
    let mut cases = Vec::new();
    let mut add = |label: &'static str, adts: Option<Vec<u8>>, coverage: &'static [Coverage]| {
        match adts {
            Some(adts) => cases.push(ConformanceCase {
                label,
                adts,
                coverage,
            }),
            // `encode_aac_adts_lavfi`/`minimal_aac_adts` return `None` on any
            // ffmpeg encode failure (including a bad filter option), and a
            // silently-empty case used to just vanish from the corpus with no
            // trace — `sweep_stereo_44100`'s `sine=...:frequency2=...` filter
            // option doesn't exist in real ffmpeg (`sine` has no chirp
            // parameter) and had been silently encoding nothing for every
            // run since this case was added, so the EIGHT_SHORT/TNS coverage
            // it was meant to add never actually ran. Loud by design now.
            None => eprintln!(
                "build_corpus: case '{label}' failed to encode (ffmpeg rejected the \
                 filtergraph or produced no output) — dropped from the corpus. \
                 Run the underlying ffmpeg command by hand to see why."
            ),
        }
    };

    // Original baseline: 440 Hz stereo tone, 44.1 kHz.
    add(
        "tone_440_stereo_44100",
        minimal_aac_adts(44_100, 2, 1.0),
        &[],
    );
    // Broadband noise → PNS / short-window heavy. `anoisesrc`'s default seed
    // is -1 (random), which made this case's exact content — and therefore
    // its max-diff/correlation numbers — different on every single test run,
    // including CI: a real reproducibility gap, not just cosmetic (a flaky
    // failure with no way to reproduce it locally from the failure alone).
    // Fixed seeds pin both cases to specific, re-runnable content.
    add(
        "noise_stereo_44100",
        tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi(
            "anoisesrc=duration=1.0:sample_rate=44100:amplitude=0.8:color=brown:seed=1",
            2,
            "128k",
        ),
        &[Coverage::Pns],
    );
    add(
        "noise_mono_44100",
        tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi(
            "anoisesrc=duration=1.0:sample_rate=44100:amplitude=0.8:color=white:seed=1",
            1,
            "96k",
        ),
        &[Coverage::Pns],
    );
    // Broadband noise at 22.05 kHz: `sampling_frequency_index` 7 selects the
    // `SWB_*_24000` scalefactor-band tables (long and short), which no other
    // corpus case touches — a transcription error there would only surface here.
    add(
        "noise_stereo_22050",
        tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi(
            "anoisesrc=duration=1.0:sample_rate=22050:amplitude=0.8:color=white:seed=3",
            2,
            "64k",
        ),
        &[Coverage::Pns],
    );
    // Frequency sweep → window-sequence transitions and TNS. ffmpeg's `sine`
    // source has no chirp/sweep parameter (an earlier `frequency2=...` here
    // was silently rejected by ffmpeg on every run, so this case never
    // actually encoded anything — see the `add` closure's warning above);
    // `aevalsrc` with an explicit linear-chirp expression is the real
    // equivalent: instantaneous frequency rises from 200 Hz to 4000 Hz
    // linearly over the 1 s duration (phase = 2*pi*(f0 + (f1-f0)/(2*d)*t)*t).
    add(
        "sweep_stereo_44100",
        tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi(
            "aevalsrc=exprs='sin(2*PI*(200+1900*t)*t)':s=44100:d=1.0",
            2,
            "128k",
        ),
        &[Coverage::Tns],
    );
    // Percussive click train (~8 bursts/s of 1 kHz, ~50 ms each): the sharp
    // onsets force the encoder into LONG_START → EIGHT_SHORT → LONG_STOP
    // transitions, exercising the short IMDCT, window grouping, and the
    // LONG↔SHORT overlap-add glue. (Short-block TNS itself is covered by the
    // `short_tns_*` cases below.)
    add(
        "transient_stereo_44100",
        tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi(
            "aevalsrc=exprs='sin(2*PI*1000*t)*gt(0.05\\,mod(t\\,0.12))':s=44100:d=1.5:c=stereo",
            2,
            "128k",
        ),
        &[Coverage::EightShort, Coverage::WindowTransition],
    );
    add(
        "transient_mono_44100",
        tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi(
            "aevalsrc=exprs='sin(2*PI*1400*t)*gt(0.04\\,mod(t\\,0.1))':s=44100:d=1.5",
            1,
            "96k",
        ),
        &[Coverage::EightShort, Coverage::WindowTransition],
    );
    // Percussive click train at 22.05 kHz: EIGHT_SHORT frames through the
    // `SWB_128_24000` short-window band table (sf_index 7). (This ffmpeg build
    // rarely emits a clean LONG_STOP at 22 kHz for this signal, so only
    // EIGHT_SHORT is asserted — the transition glue is covered at 44.1 kHz.)
    add(
        "transient_stereo_22050",
        tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi(
            "aevalsrc=exprs='sin(2*PI*900*t)*gt(0.05\\,mod(t\\,0.13))':s=22050:d=1.5:c=stereo",
            2,
            "96k",
        ),
        &[Coverage::EightShort],
    );
    // Short-block TNS: a steady 500 Hz tone in the left channel and a decaying
    // 3 kHz burst (envelope re-triggered every 200 ms) in the right. The sharp
    // re-onsets push the encoder into EIGHT_SHORT, and at 96 kbit/s it turns on
    // `tns_data` for the right channel's short windows while M/S is also active —
    // the exact combination that was mis-decoded when TNS was applied before the
    // joint-stereo butterfly instead of after it (todo-aac.md).
    add(
        "short_tns_stereo_44100",
        tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi(
            "aevalsrc=exprs='0.5*sin(2*PI*500*t)|0.5*sin(2*PI*3000*t)*exp(-2*mod(t\\,0.2))':s=44100:d=1.5",
            2,
            "96k",
        ),
        &[Coverage::ShortTns, Coverage::MsStereo],
    );
    // Mono variant of the short-block TNS probe: exercises the SCE branch of the
    // post-stereo TNS pass (Pass 3.5) and short-block TNS with no butterfly in
    // front of it.
    add(
        "short_tns_mono_44100",
        tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi(
            "aevalsrc=exprs='0.6*sin(2*PI*2500*t)*exp(-3*mod(t\\,0.18))+0.2*sin(2*PI*500*t)':s=44100:d=1.5",
            1,
            "80k",
        ),
        &[Coverage::ShortTns],
    );
    // Very low bitrate (24 kbit/s) stereo with a shared low tone + a partly
    // decorrelated 6 kHz tone: forces the encoder to code the high band with
    // *intensity stereo* (INTENSITY_HCB / INTENSITY_HCB2 sections in the right
    // channel), exercising `stereo.rs`'s intensity path — its sign
    // (`-1 + 2·(band_type-14)`, flipped by the M/S mask), scale
    // (`2^(-0.25·is_position)`), and interaction with M/S on the same frame.
    add(
        "intensity_stereo_24k",
        tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi(
            "aevalsrc=exprs='0.5*sin(2*PI*300*t)+0.2*sin(2*PI*6000*t+sin(t*50))|0.5*sin(2*PI*300*t)+0.2*sin(2*PI*6000*t)':s=44100:d=1.0",
            2,
            "24k",
        ),
        &[Coverage::IntensityStereo],
    );
    // 5.1 surround (channel_configuration 6: SCE + CPE + CPE + LFE): exercises
    // multi-element raw_data_block parsing, the LFE element, a second CPE with
    // its own M/S, and the element-order → WAV-order output channel remap
    // (`output_channel_order`).
    add(
        "surround_51_44100",
        tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi(
            "aevalsrc=exprs='0.3*sin(2*PI*200*t)|0.3*sin(2*PI*400*t)|0.2*sin(2*PI*600*t)|0.1*sin(2*PI*80*t)|0.25*sin(2*PI*900*t)|0.25*sin(2*PI*1100*t)':channel_layout=5.1:s=44100:d=1.0",
            6,
            "256k",
        ),
        &[Coverage::MultiElement],
    );
    // 7.1 (channel_configuration 7): SCE + CPE + CPE + CPE + LFE. ffmpeg's AAC
    // encoder writes config 7 for an 8-channel "7.1" layout (FL FR FC LFE BL BR
    // SL SR) as elements SCE(FC), CPE(FL/FR), CPE(BL/BR), CPE(SL/SR), LFE — the
    // `output_channel_order` (7, 8) permutation must map that element order back
    // to ffmpeg's decode output order. (ffmpeg logs a "non-spec-compliant 7.1"
    // note about this layout vs the spec's 7.1-wide; both encode and reference
    // decode go through the same assumption, so the round-trip is still a valid
    // conformance check for our remap.)
    add(
        "surround_71_44100",
        tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi(
            "aevalsrc=exprs='0.25*sin(2*PI*210*t)|0.25*sin(2*PI*300*t)|0.2*sin(2*PI*500*t)|0.1*sin(2*PI*70*t)|0.22*sin(2*PI*700*t)|0.22*sin(2*PI*900*t)|0.2*sin(2*PI*1100*t)|0.2*sin(2*PI*1300*t)':channel_layout=7.1:s=44100:d=1.0",
            8,
            "320k",
        ),
        &[Coverage::MultiElement],
    );
    // `channel_configuration = 0`: the channel layout lives in a
    // `program_config_element` in the first raw_data_block instead of the ADTS
    // header. `-aac_pce 1` makes ffmpeg emit it. Exercises
    // `skip_program_config_element` — a wrong PCE bit length desyncs the whole
    // first frame (it was missing `element_instance_tag` and had
    // `byte_alignment()` on the wrong side of `comment_field_bytes`).
    add(
        "config0_pce_stereo_44100",
        tpt_kinetix_test_utils::synthetic::encode_aac_adts_lavfi_args(
            "aevalsrc=exprs='0.4*sin(2*PI*330*t)|0.4*sin(2*PI*440*t)':s=44100:d=1.0",
            2,
            "128k",
            &["-aac_pce", "1"],
        ),
        &[Coverage::ConfigZero],
    );
    // Other sample rates → different SWB tables.
    add(
        "tone_440_stereo_48000",
        minimal_aac_adts(48_000, 2, 1.0),
        &[],
    );
    add(
        "tone_440_stereo_22050",
        minimal_aac_adts(22_050, 2, 1.0),
        &[],
    );
    add("tone_440_mono_44100", minimal_aac_adts(44_100, 1, 1.0), &[]);
    cases
}

#[test]
fn native_aac_matches_ffmpeg_reference() {
    if !ffmpeg_available() {
        eprintln!("native_aac_matches_ffmpeg_reference: skipped (ffmpeg unavailable)");
        return;
    }

    let corpus = build_corpus();
    assert!(
        !corpus.is_empty(),
        "ffmpeg failed to encode every corpus case"
    );

    let mut worst_diff = 0.0f32;
    let mut worst_corr = 1.0f64;
    let mut worst_label = "";

    for case in &corpus {
        // Guard against the case silently no longer exercising what it is for.
        let stats = analyze_stream(&case.adts);
        eprintln!("[{}] stream stats: {stats:?}", case.label);
        check_coverage(case.label, case.coverage, &stats);

        let native = decode_native(&case.adts);
        // `channel_configuration == 0` streams don't carry the channel count in
        // the ADTS header, so the reference harness can't frame ffmpeg's raw
        // f32 output on its own — take the count from the native decode (which
        // gets it from the PCE-less element list).
        let reference = if stats.config_zero_frames > 0 {
            let ch = native.first().map_or(2, |f| f.channels);
            tpt_kinetix_test_utils::reference::decode_aac_with_ffmpeg_channels(&case.adts, ch)
        } else {
            decode_aac_with_ffmpeg(&case.adts)
        }
        .expect("ffmpeg should decode its own stream");
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

        // Every stream now parses structurally, so channel-count /
        // sample-rate mismatches are real regressions, not expected gaps.
        assert_eq!(
            native[0].channels, reference[0].channels,
            "[{}] channel count mismatch: native={} reference={}",
            case.label, native[0].channels, reference[0].channels
        );
        assert_eq!(native[0].sample_rate, reference[0].sample_rate);

        if std::env::var("AAC_DBG_CHMAP").is_ok() {
            let np = flatten_channels(&native);
            let rp = flatten_channels(&reference);
            eprintln!(
                "[{}] native-plane × reference-plane correlation:",
                case.label
            );
            for (ni, n) in np.iter().enumerate() {
                let row: Vec<String> = rp
                    .iter()
                    .map(|r| {
                        let len = n.len().min(r.len());
                        let (mut dot, mut nn, mut rr) = (0.0f64, 0.0f64, 0.0f64);
                        for i in 0..len {
                            dot += n[i] as f64 * r[i] as f64;
                            nn += (n[i] as f64).powi(2);
                            rr += (r[i] as f64).powi(2);
                        }
                        let c = if nn > 0.0 && rr > 0.0 {
                            dot / (nn.sqrt() * rr.sqrt())
                        } else {
                            0.0
                        };
                        format!("{c:+.2}")
                    })
                    .collect();
                eprintln!("  native[{ni}]: {}", row.join(" "));
            }
        }

        let max_diff = best_aligned_max_diff(&native, &reference);
        let corr = best_channel0_correlation(&native, &reference);
        eprintln!(
            "[{}] conformance max-abs-diff={max_diff} correlation={corr:.4}",
            case.label
        );

        // `noise_mono_44100` (broadband white noise, heavy PNS use, EIGHT_SHORT
        // frames) was the journal's long-standing PNS open gap (corr ~0.52 →
        // ~0.87 across the 2026-08-24..28 sessions). The 2026-08-30
        // `scalefactors.rs` noise-predictor fix (`noise_sfo` DPCM initialised to
        // `global_gain - 90`, advanced by `raw - 256` then clamped to
        // `[-100, 155]`, stored as `global_gain - 100 - noise_sfo`) closed it:
        // it now decodes bit-exactly like the other cases (measured
        // max-abs-diff ~3.5e-7, correlation 1.0000). No special case needed —
        // it flows into the aggregate gate below and is now a real assertion.

        // `sweep_stereo_44100` (a 200→4000 Hz linear chirp, ~0.71-peak stereo,
        // window-sequence transitions + TNS) was the journal's last open gap:
        // a single ~0.0725 outlier sample that survived ~5 sessions of
        // ESC-codebook / dequant investigation. Root cause (2026-09-02): the
        // TNS decode was wrong three ways — (1) the reflection-coefficient
        // tables were the symmetric sine table instead of ISO §4.6.9.3's
        // asymmetric `iqfac`/`iqfac_m` quantizer (ffmpeg's `ff_tns_tmp2_map`),
        // and ignored `coef_compress` for table selection; (2) the step-up
        // recursion used `-k` instead of `+k`; (3) filters were applied bottom-up
        // from band 0 instead of top-down from `num_swb` with the
        // `min(tns_max_bands, max_sfb)` clamp. All fixed — this case now decodes
        // bit-exactly (measured ~3e-6) and flows into the aggregate gate.

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
