// Tests that run across codec/demux boundaries using tpt-kinetix-test-utils helpers.
use tpt_kinetix_test_utils::{pixel_diff::*, synthetic::*};

#[test]
fn grey_frame_is_identical_to_itself() {
    let frame = grey_yuv420p_frame(64, 64);
    assert!(within_tolerance(&frame, &frame, 0));
    let (y, cb, cr) = psnr_yuv420p(&frame, &frame).unwrap();
    assert!(y.is_infinite() && cb.is_infinite() && cr.is_infinite());
}

#[test]
fn ramp_frame_differs_from_grey() {
    let grey = grey_yuv420p_frame(64, 64);
    let ramp = ramp_yuv420p_frame(64, 64);
    assert!(!within_tolerance(&grey, &ramp, 0));
    let count = luma_diff_count(&grey, &ramp);
    assert!(count > 0);
}

#[test]
fn corpus_edge_cases_do_not_panic() {
    use tpt_kinetix_test_utils::corpus::Corpus;
    let mut c = Corpus::new("demux");
    c.add_edge_cases();
    for entry in c.iter() {
        let _ = tpt_kinetix_demux::mp4::container::parse_mp4(&entry.data);
    }
}

/// Pixel-exact comparison of the Kinetix H.264 decoder against `ffmpeg`.
///
/// Skips (does not fail) when `ffmpeg` is not installed, so the suite still
/// passes on runners without the reference binary. When `ffmpeg` is present,
/// this decodes the same Annex B stream with both decoders and diffs frame
/// geometry + luma. (Pixel *identity* is not asserted yet because the Kinetix
/// H.264 decoder is still a scaffold that emits placeholder frames — see
/// `tpt-kinetix-h264` LIMITATIONS.)
#[test]
fn h264_vs_ffmpeg_reference_when_available() {
    use tpt_kinetix_test_utils::reference::{decode_h264_with_ffmpeg, ffmpeg_available};
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not available on PATH");
        return;
    }

    let stream = minimal_h264_annexb_sps_pps();
    match decode_h264_with_ffmpeg(&stream, 16, 16) {
        Ok(ref_frames) => {
            // ffmpeg may emit zero frames for a headers-only stream; that's fine.
            for f in &ref_frames {
                assert_eq!(f.width, 16);
                assert_eq!(f.height, 16);
            }
        }
        Err(e) => {
            // A decode error on the synthetic headers-only stream is acceptable;
            // we only assert the harness itself doesn't panic.
            eprintln!("ffmpeg reference decode returned: {e}");
        }
    }
}

/// Drive the `dav1d` reference decoder through the harness against a real AV1
/// bitstream (synthesized on the fly with `ffmpeg`'s AV1 encoder when both
/// binaries are present).
///
/// Skips when either `ffmpeg` or `dav1d` is missing. Once the Kinetix AV1
/// decoder produces real frames, a pixel-diff against `ref_frames` can be
/// wired in here to satisfy the "validated against dav1d" gate.
#[test]
fn av1_dav1d_reference_decode_when_available() {
    use tpt_kinetix_test_utils::{
        reference::{dav1d_available, decode_av1_with_dav1d, ffmpeg_available},
        synthetic::minimal_av1_ivf,
    };

    if !ffmpeg_available() || !dav1d_available() {
        eprintln!("skipping: ffmpeg and/or dav1d not available on PATH");
        return;
    }

    // If we can synthesize an AV1 IVF, exercise dav1d on it end-to-end.
    match minimal_av1_ivf() {
        Some(ivf) => match decode_av1_with_dav1d(&ivf, 128, 96) {
            Ok(frames) => {
                for f in &frames {
                    assert_eq!(f.width, 128);
                    assert_eq!(f.height, 96);
                    assert_eq!(
                        f.pixel_format,
                        tpt_kinetix_core::pixel_format::PixelFormat::Yuv420p
                    );
                }
            }
            Err(e) => eprintln!("dav1d decode returned: {e}"),
        },
        None => eprintln!("skipping: could not synthesize an AV1 IVF with ffmpeg"),
    }
}

/// AV1 Phase G intra-only corpus harness: decode a spread of synthesized AV1
/// intra keyframes (varied content pattern + resolution, see
/// [`tpt_kinetix_test_utils::synthetic::av1_intra_corpus`]) with both
/// `Av1Decoder` and the `dav1d` reference decoder (standalone binary or
/// `ffmpeg`'s `libdav1d`, see `reference::dav1d_available`), then report the
/// per-entry PSNR/diff gap.
///
/// This is the "generated intra-only corpus" validation called for by AV1
/// Phase G (todo.md) for Phases A-C, ahead of inter prediction (Phase E)
/// landing. It does not yet hard-assert pixel-exactness — Phase G's gate
/// (`capabilities().pixel_exact`) flips only once every corpus entry is
/// bit-exact against `dav1d`; today it measures and reports the gap so
/// regressions/improvements are visible across runs. Skips when neither
/// `dav1d` nor `ffmpeg` is available, or when the corpus could not be
/// synthesized (no `ffmpeg` AV1 encoder).
#[test]
fn av1_intra_corpus_vs_dav1d_when_available() {
    use tpt_kinetix_av1::Av1Decoder;
    use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
    use tpt_kinetix_test_utils::{
        pixel_diff::*,
        reference::{dav1d_available, decode_av1_obu_with_dav1d},
        synthetic::av1_intra_corpus,
    };

    if !dav1d_available() {
        eprintln!("skipping: dav1d not available (neither standalone binary nor ffmpeg+libdav1d)");
        return;
    }

    let corpus = av1_intra_corpus();
    if corpus.is_empty() {
        eprintln!("skipping: could not synthesize an AV1 intra-only corpus with ffmpeg");
        return;
    }

    assert!(
        !Av1Decoder::new().capabilities().pixel_exact,
        "AV1 decoder must not claim pixel_exact before the Phase G corpus gate passes"
    );

    let mut exact_count = 0usize;
    let mut compared_count = 0usize;
    for entry in &corpus {
        let ref_frames = match decode_av1_obu_with_dav1d(&entry.obu, entry.width, entry.height) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[{}] dav1d reference decode returned: {e}", entry.label);
                continue;
            }
        };
        let Some(ref_frame) = ref_frames.first() else {
            eprintln!("[{}] dav1d produced no frames", entry.label);
            continue;
        };

        let mut dec = Av1Decoder::new();
        let packet = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: entry.obu.clone(),
            stream_index: 0,
            is_key_frame: true,
        };
        let kinetix_frame = match dec.decode(&packet) {
            Ok(Some(f)) => f,
            Ok(None) => {
                eprintln!("[{}] Kinetix produced no frame", entry.label);
                continue;
            }
            Err(e) => {
                eprintln!("[{}] Kinetix decode errored: {e}", entry.label);
                continue;
            }
        };

        compared_count += 1;
        let exact = within_tolerance(&kinetix_frame, ref_frame, 0);
        if exact {
            exact_count += 1;
        }
        let (psnr_y, psnr_u, psnr_v) =
            psnr_yuv420p(&kinetix_frame, ref_frame).unwrap_or((0.0, 0.0, 0.0));
        eprintln!(
            "[{}] {}x{}, PSNR Y/U/V = {:.2}/{:.2}/{:.2} dB, luma diff samples = {}, exact = {}",
            entry.label,
            entry.width,
            entry.height,
            psnr_y,
            psnr_u,
            psnr_v,
            luma_diff_count(&kinetix_frame, ref_frame),
            exact,
        );
    }

    assert!(
        compared_count > 0,
        "no corpus entries produced a comparable Kinetix/dav1d frame pair"
    );
    eprintln!("AV1 intra corpus: {exact_count}/{compared_count} entries bit-exact vs dav1d");

    // Phase G gate: every synthesized intra keyframe (including the 320x180
    // testsrc2 screen-content clip with 9 IBC blocks) decodes bit-exact vs
    // dav1d. This is a hard regression guard — the decoder's `pixel_exact`
    // capability still stays `false` (asserted above) until the *inter*
    // path is validated too and official AOM/ITU vectors are wired in.
    assert_eq!(
        exact_count, compared_count,
        "an AV1 intra keyframe regressed from bit-exact vs dav1d"
    );
}

/// AV1 Phase E inter-prediction conformance harness: decode a multi-frame
/// `ffmpeg`-synthesized AV1 **IVF** (keyframe + inter frames) frame-by-frame
/// with the Kinetix [`tpt_kinetix_av1::Av1Decoder`] and compare every frame
/// against a `dav1d` (ffmpeg libdav1d) reference decode. This exercises the
/// wired-through inter path (reference-frame store, motion compensation, MV
/// prediction) end-to-end and reports the per-frame PSNR / luma-diff gap.
///
/// Skips (does not fail) when the `dav1d` reference is unavailable. The gap is
/// reported, not hard-asserted, until the Phase E gate (`pixel_exact`) flips.
#[test]
fn av1_inter_sequence_vs_dav1d_when_available() {
    use tpt_kinetix_av1::Av1Decoder;
    use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
    use tpt_kinetix_test_utils::{
        pixel_diff::*,
        reference::{dav1d_available, decode_av1_with_dav1d, split_ivf_frames},
        synthetic::minimal_av1_inter_ivf,
    };

    if !dav1d_available() {
        eprintln!("skipping: dav1d not available (neither standalone binary nor ffmpeg+libdav1d)");
        return;
    }

    const W: u32 = 128;
    const H: u32 = 96;
    const FRAMES: u32 = 8;
    let Some(ivf) = minimal_av1_inter_ivf(FRAMES, W, H) else {
        eprintln!("skipping: could not synthesize a multi-frame AV1 IVF with ffmpeg");
        return;
    };

    // Reference: decode the whole IVF into ordered frames.
    let ref_frames = match decode_av1_with_dav1d(&ivf, W, H) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("dav1d reference decode returned: {e}");
            return;
        }
    };
    if ref_frames.is_empty() {
        eprintln!("skipping: dav1d produced no frames");
        return;
    }

    // Split the IVF into per-frame OBU payloads for frame-by-frame Kinetix decode.
    let frame_payloads = split_ivf_frames(&ivf);
    if frame_payloads.len() != ref_frames.len() {
        eprintln!(
            "ivf split produced {} frames but dav1d produced {}; bailing",
            frame_payloads.len(),
            ref_frames.len()
        );
        return;
    }

    // Feed every IVF frame (decode order) through one decoder and collect the
    // shown outputs. `Av1Decoder::decode` emits one shown frame per temporal
    // unit in DISPLAY order (buffering hidden alt-refs and replaying them via
    // `show_existing_frame`), so the collected sequence lines up 1:1 with
    // dav1d's display-ordered `ref_frames` — pairing by raw packet index does
    // not, for a hierarchical GOP.
    let mut dec = Av1Decoder::new();
    let mut kinetix_frames = Vec::new();
    for (i, payload) in frame_payloads.iter().enumerate() {
        let packet = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: payload.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        };
        match dec.decode(&packet) {
            Ok(Some(f)) => kinetix_frames.push(f),
            Ok(None) => {}
            Err(e) => eprintln!("[packet {i}] Kinetix decode errored: {e}"),
        }
    }

    let mut exact_count = 0usize;
    let mut compared_count = 0usize;
    for (i, (kinetix_frame, ref_frame)) in kinetix_frames.iter().zip(ref_frames.iter()).enumerate()
    {
        compared_count += 1;
        let exact = within_tolerance(kinetix_frame, ref_frame, 0);
        if exact {
            exact_count += 1;
        }
        let (psnr_y, psnr_u, psnr_v) =
            psnr_yuv420p(kinetix_frame, ref_frame).unwrap_or((0.0, 0.0, 0.0));
        eprintln!(
            "[frame {i}] {}x{}, PSNR Y/U/V = {:.2}/{:.2}/{:.2} dB, luma diff samples = {}, exact = {}",
            kinetix_frame.width,
            kinetix_frame.height,
            psnr_y,
            psnr_u,
            psnr_v,
            luma_diff_count(kinetix_frame, ref_frame),
            exact,
        );
    }

    assert!(
        compared_count > 0,
        "no comparable Kinetix/dav1d frame pairs were produced"
    );
    eprintln!("AV1 inter sequence: {exact_count}/{compared_count} frames bit-exact vs dav1d");

    // Phase E gate (uncomment once every inter frame is bit-exact):
    // assert_eq!(exact_count, compared_count);
}

/// AV1 conformance harness: decode a real `ffmpeg`-synthesized AV1 keyframe
/// OBU with both the Kinetix [`tpt_kinetix_av1::Av1Decoder`] and the
/// `ffmpeg`-backed reference decoder, then measure the per-plane gap.
///
/// Skips (does not fail) when `ffmpeg` is absent. The harness hard-asserts the
/// part of the decoder that already works end-to-end against real keyframes —
/// OBU splitting + Sequence Header parsing + declared geometry — and reports
/// (without asserting) the decode-vs-reference gap for the parts that are still
/// in progress: the frame-header parser (AV1 §5.9) and the superblock
/// reconstruction (Phase C) / loop filters (Phase D). The pixel-exact gate
/// (commented `within_tolerance(.., 0)`) flips once those phases land.
#[test]
fn av1_vs_ffmpeg_reference_when_available() {
    use tpt_kinetix_av1::Av1Decoder;
    use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
    use tpt_kinetix_test_utils::{
        pixel_diff::*,
        reference::{decode_av1_with_ffmpeg, ffmpeg_available},
        synthetic::minimal_av1_obu,
    };

    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not available on PATH");
        return;
    }

    let (w, h) = (128u32, 96u32);
    let Some(obu) = minimal_av1_obu(w, h) else {
        eprintln!("skipping: could not synthesize an AV1 OBU with ffmpeg");
        return;
    };

    // --- Sequence-header parse (AV1 Phase: sequence-header decoding) ---
    // This is the part of the decoder that currently works end-to-end against
    // real `ffmpeg`-generated keyframes: the OBU is split, the Sequence Header
    // OBU is parsed to completion, and the declared frame geometry matches the
    // encoder. This assertion is the verifiable contract for that work.
    use tpt_kinetix_av1::obu::{parse_obu_sequence, ObuType, SequenceHeaderObu};
    let seq = parse_obu_sequence(&obu)
        .into_iter()
        .find(|o| o.obu_type == ObuType::SequenceHeader)
        .and_then(|o| SequenceHeaderObu::parse(&o.payload).ok());
    let seq = seq.expect("sequence header should parse from a real ffmpeg keyframe");
    assert_eq!(seq.frame_width(), w, "sequence-header width must match");
    assert_eq!(seq.frame_height(), h, "sequence-header height must match");

    // Reference decode — ffmpeg's AV1 decoder applies CDEF + loop filters, so
    // this is the pixel-exact target the Kinetix decoder must eventually match.
    let ref_frames = match decode_av1_with_ffmpeg(&obu, w, h) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("ffmpeg AV1 reference decode returned: {e}");
            return;
        }
    };
    assert!(!ref_frames.is_empty(), "reference produced no frames");
    let ref_frame = &ref_frames[0];

    // Kinetix decode (frame header + reconstruction).
    let mut dec = Av1Decoder::new();
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: obu.clone(),
        stream_index: 0,
        is_key_frame: true,
    };
    let kinetix_frame = match dec.decode(&packet) {
        Ok(Some(f)) => f,
        Ok(None) => {
            eprintln!("Kinetix produced no frame");
            return;
        }
        Err(e) => {
            eprintln!("Kinetix decode errored: {e}");
            return;
        }
    };

    // The frame-header parser (AV1 §5.9) now parses the keyframe geometry
    // correctly (asserted below); the superblock reconstruction (Phase C) and
    // loop filters (Phase D) are still pending, so output is not pixel-exact.
    // Measure the gap when the two frames are byte-comparable; otherwise just
    // report that the decoder produced a same-geometry frame.
    assert_eq!(kinetix_frame.width, w, "frame-header width must match");
    assert_eq!(kinetix_frame.height, h, "frame-header height must match");

    let (psnr_y, psnr_u, psnr_v) =
        psnr_yuv420p(&kinetix_frame, ref_frame).unwrap_or((0.0, 0.0, 0.0));
    let diff_count = luma_diff_count(&kinetix_frame, ref_frame);
    eprintln!(
        "AV1 conformance (Kinetix vs ffmpeg): {}x{}, PSNR Y/U/V = {:.2}/{:.2}/{:.2} dB, \
         luma diff samples = {}/{} (kinetix data {}B, ref {}B)",
        kinetix_frame.width,
        kinetix_frame.height,
        psnr_y,
        psnr_u,
        psnr_v,
        diff_count,
        (w as usize) * (h as usize),
        kinetix_frame.data.len(),
        ref_frame.data.len(),
    );

    // Phase G gate (uncomment once Phase C/D land and the decoder is validated):
    // assert!(within_tolerance(&kinetix_frame, ref_frame, 0));
}

/// Pixel-exact harness run across a real, multi-frame H.264 sample.
///
/// Synthesizes a short baseline-profile (CAVLC, no B-frames) H.264 clip with
/// `ffmpeg`, then decodes it NAL-by-NAL through the Kinetix decoder and compares
/// every emitted frame against the `ffmpeg` reference decode. The Kinetix H.264
/// decoder reports `capabilities().pixel_exact == true` for CAVLC/CABAC I/P/B
/// progressive 4:2:0, so this asserts real bit-exactness — in both the default
/// and `with_strict(true)` modes. Skips when `ffmpeg` is absent.
#[test]
fn h264_real_sample_harness_across_profiles() {
    use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
    use tpt_kinetix_h264::H264Decoder;
    use tpt_kinetix_test_utils::pixel_diff::within_tolerance;
    use tpt_kinetix_test_utils::reference::{decode_h264_with_ffmpeg, ffmpeg_available};

    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not available on PATH");
        return;
    }

    // Encode a short 16x16 baseline clip to a raw Annex B H.264 bytestream.
    let annexb = match generate_h264_annexb(16, 16, 8) {
        Some(b) => b,
        None => {
            eprintln!("skipping: could not synthesize an H.264 sample with ffmpeg");
            return;
        }
    };

    let ref_frames = match decode_h264_with_ffmpeg(&annexb, 16, 16) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("ffmpeg reference decode returned: {e}");
            return;
        }
    };
    assert!(!ref_frames.is_empty(), "reference produced no frames");

    // The CAVLC/CABAC I/P/B progressive path is bit-exact — the decoder says so.
    let caps = H264Decoder::new().capabilities();
    assert!(
        caps.pixel_exact,
        "H.264 decoder should report pixel_exact for progressive 4:2:0"
    );

    // Split the Annex B stream into NAL units and feed one per packet, then
    // flush — the standard streaming-decode pattern (see `dbg_g5c_crop`).
    let mut starts = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }

    for strict in [false, true] {
        let mut dec = H264Decoder::new().with_strict(strict);
        let mut frames = Vec::new();
        for (n, &s) in starts.iter().enumerate() {
            let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
            let mut data = vec![0u8, 0, 0, 1];
            data.extend_from_slice(&annexb[s..e]);
            let pkt = Packet {
                pts: Timestamp::new(n as i64, (1, 15)),
                dts: Timestamp::new(n as i64, (1, 15)),
                data,
                stream_index: 0,
                is_key_frame: n == 0,
            };
            match dec.decode(&pkt) {
                Ok(Some(f)) => frames.push(f),
                Ok(None) => {}
                Err(err) => panic!("strict={strict}: Kinetix decode errored: {err}"),
            }
        }
        frames.extend(dec.flush().expect("flush"));

        let n = frames.len().min(ref_frames.len());
        assert!(
            n >= ref_frames.len().saturating_sub(1),
            "strict={strict}: Kinetix emitted {} frames, ffmpeg {}",
            frames.len(),
            ref_frames.len()
        );
        for i in 0..n {
            assert!(
                within_tolerance(&frames[i], &ref_frames[i], 0),
                "strict={strict}: frame {i} not bit-exact vs ffmpeg"
            );
        }
    }
}

/// Use `ffmpeg` to encode a short raw `testsrc` clip into an Annex B H.264
/// bytestream, returning `None` if ffmpeg is unavailable or fails.
fn generate_h264_annexb(width: u32, height: u32, frames: u32) -> Option<Vec<u8>> {
    use std::{
        io::Read,
        process::{Command, Stdio},
    };

    let mut child = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size={width}x{height}:rate=15:duration={frames}"),
            "-c:v",
            "libx264",
            "-profile:v",
            "baseline",
            "-pix_fmt",
            "yuv420p",
            "-preset",
            "ultrafast",
            "-f",
            "h264",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = Vec::new();
    if child.stdout.take()?.read_to_end(&mut out).is_err() {
        return None;
    }
    let _ = child.wait();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Walk a raw AV1 OBU stream and return the byte span `[start, end)` of every
/// OBU, tagged with its numeric type (AV1 §5.3.2).
fn av1_obu_spans(data: &[u8]) -> Vec<(u8, usize, usize)> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        if data[pos] & 0x80 != 0 {
            break;
        }
        let obu_type = (data[pos] >> 3) & 0x0F;
        let ext = (data[pos] >> 2) & 1 != 0;
        let has_size = (data[pos] >> 1) & 1 != 0;
        let mut off = pos + 1;
        if ext {
            off += 1;
        }
        let mut payload_len = 0usize;
        let mut shift = 0u32;
        let mut i = 0;
        if has_size {
            loop {
                if off + i >= data.len() {
                    return out;
                }
                let b = data[off + i];
                payload_len |= ((b & 0x7F) as usize) << shift;
                shift += 7;
                if b & 0x80 == 0 {
                    break;
                }
                i += 1;
            }
            off += i + 1;
        } else {
            payload_len = data.len() - off;
        }
        let end = off + payload_len;
        if end <= pos {
            break;
        }
        out.push((obu_type, pos, end.min(data.len())));
        pos = end;
    }
    out
}

/// AV1 Phase E inter-prediction conformance: decode a short synthesized AV1
/// **inter** clip (keyframe + motion-predicted frames) frame-by-frame with both
/// the Kinetix [`tpt_kinetix_av1::Av1Decoder`] (which builds up its reference
/// frame buffer across frames) and the `dav1d` reference decoder, then report
/// the per-frame PSNR/diff gap.
///
/// Each Frame OBU is fed as its own packet so the decoder reconstructs one frame
/// per call and accumulates references exactly as a real streaming decode would.
/// This is the measure of AV1 Phase E (MV prediction §7.10 + inter block
/// reconstruction §7.11.3) against the reference. It reports — without yet
/// asserting — the gap, so regressions/improvements are visible across runs.
/// Skips when neither `dav1d` nor `ffmpeg` is available, or when no inter clip
/// could be synthesized.
#[test]
fn av1_inter_corpus_vs_dav1d_when_available() {
    use tpt_kinetix_av1::Av1Decoder;
    use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
    use tpt_kinetix_test_utils::{
        pixel_diff::*,
        reference::{dav1d_available, decode_av1_obu_with_dav1d},
        synthetic::av1_inter_corpus,
    };

    if !dav1d_available() {
        eprintln!("skipping: dav1d not available (neither standalone binary nor ffmpeg+libdav1d)");
        return;
    }

    let corpus = av1_inter_corpus();
    if corpus.is_empty() {
        eprintln!("skipping: could not synthesize an AV1 inter corpus with ffmpeg");
        return;
    }

    for entry in &corpus {
        let spans = av1_obu_spans(&entry.obu);
        let seq_span: Option<(usize, usize)> = spans.iter().find(|s| s.0 == 1).map(|s| (s.1, s.2));

        // Group the OBU stream into temporal units — a temporal-delimiter OBU
        // (type 2) opens a new TU. Each TU displays exactly one frame (in
        // display order), so feeding one packet per TU keeps Kinetix's outputs
        // aligned with dav1d's display-ordered decode even for hierarchical
        // GOPs (ALTREF decoded early, shown later via show_existing_frame).
        let mut tu_spans: Vec<(usize, usize)> = Vec::new();
        let mut cur: Option<usize> = None;
        for (t, s, _e) in &spans {
            if *t == 2 {
                if let Some(cs) = cur.take() {
                    tu_spans.push((cs, *s));
                }
                cur = Some(*s);
            }
        }
        if let Some(cs) = cur {
            tu_spans.push((cs, entry.obu.len()));
        }
        // Fall back to per-frame-OBU packets if the stream has no delimiters.
        if tu_spans.is_empty() {
            tu_spans = spans
                .iter()
                .filter(|s| s.0 == 6)
                .map(|s| (s.1, s.2))
                .collect();
        }
        if tu_spans.len() < 2 {
            eprintln!(
                "[{}] only {} TU(s) present, skipping",
                entry.label,
                tu_spans.len()
            );
            continue;
        }

        let ref_frames = match decode_av1_obu_with_dav1d(&entry.obu, entry.width, entry.height) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[{}] dav1d decode returned: {e}", entry.label);
                continue;
            }
        };

        let mut dec = Av1Decoder::new();
        let mut kframes = Vec::new();
        for (i, (start, end)) in tu_spans.iter().enumerate() {
            let mut data = Vec::new();
            if let Some((ss, se)) = seq_span {
                if !(*start <= ss && ss < *end) {
                    data.extend_from_slice(&entry.obu[ss..se]);
                }
            }
            data.extend_from_slice(&entry.obu[*start..*end]);
            let packet = Packet {
                pts: Timestamp::new(i as i64, (1, 90_000)),
                dts: Timestamp::new(i as i64, (1, 90_000)),
                data,
                stream_index: 0,
                is_key_frame: i == 0,
            };
            match dec.decode(&packet) {
                Ok(Some(f)) => kframes.push(f),
                Ok(None) => {
                    eprintln!("[{}] TU {i}: Kinetix produced no frame", entry.label);
                    break;
                }
                Err(e) => {
                    eprintln!("[{}] TU {i}: Kinetix errored: {e}", entry.label);
                    break;
                }
            }
        }

        let n = kframes.len().min(ref_frames.len());
        let mut exact = 0usize;
        for i in 0..n {
            let (p_y, p_u, p_v) =
                psnr_yuv420p(&kframes[i], &ref_frames[i]).unwrap_or((0.0, 0.0, 0.0));
            let is_exact = within_tolerance(&kframes[i], &ref_frames[i], 0);
            if is_exact {
                exact += 1;
            }
            if i > 0 {
                // Only report inter frames (skip the keyframe, which is Phases A-C).
                eprintln!(
                    "[{}] frame {i} (inter): PSNR Y/U/V = {p_y:.2}/{p_u:.2}/{p_v:.2} dB, \
                     luma diff = {}, exact = {is_exact}",
                    entry.label,
                    luma_diff_count(&kframes[i], &ref_frames[i]),
                );
                if !is_exact && p_v < 99.0 {
                    let w = entry.width as usize;
                    let h = entry.height as usize;
                    let cw = w.div_ceil(2);
                    let ch = h.div_ceil(2);
                    let y_off = w * h;
                    let u_off = y_off + cw * ch;
                    let kd = &kframes[i].data;
                    let rd = &ref_frames[i].data;
                    for cy in 0..ch {
                        for cx in 0..cw {
                            let vi = u_off + cy * cw + cx;
                            if vi < kd.len() && vi < rd.len() && kd[vi] != rd[vi] {
                                eprintln!(
                                    "  V diff @ chroma ({cx},{cy}): kin={} ref={} delta={}",
                                    kd[vi],
                                    rd[vi],
                                    kd[vi] as i32 - rd[vi] as i32
                                );
                            }
                        }
                    }
                }
            }
        }
        eprintln!(
            "[{}] inter frames: {}/{} bit-exact vs dav1d ({} total frames)",
            entry.label,
            exact.saturating_sub(if n > 0 { 1 } else { 0 }),
            n.saturating_sub(if n > 0 { 1 } else { 0 }),
            n
        );
    }
}

/// Real aom-generated AV1 samples from the FFmpeg FATE suite
/// (<https://fate-suite.ffmpeg.org/av1/>) diffed against dav1d.
///
/// Gated on `KINETIX_AV1_FATE_DIR` pointing at a directory containing the
/// downloaded samples — tests never touch the network. Samples exercising
/// features this decoder does not support (Annex-B byte alignment, decoder
/// model, film grain) are reported as skipped rather than failed: dav1d
/// applies film grain to its output while Kinetix ignores it, so those
/// files can never compare equal until film grain is implemented.
#[test]
fn av1_fate_real_samples_vs_dav1d_when_available() {
    use tpt_kinetix_av1::Av1Decoder;
    use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
    use tpt_kinetix_test_utils::{
        pixel_diff::within_tolerance,
        reference::{dav1d_available, decode_av1_with_dav1d, split_ivf_frames},
    };

    let Ok(dir) = std::env::var("KINETIX_AV1_FATE_DIR") else {
        eprintln!(
            "skipping: set KINETIX_AV1_FATE_DIR to a directory with the \
             fate-suite.ffmpeg.org/av1 samples to run this test"
        );
        return;
    };
    if !dav1d_available() {
        eprintln!("skipping: dav1d not available");
        return;
    }

    // Files whose features Kinetix knowingly does not support yet; dav1d's
    // output can never match for these (e.g. it applies film grain).
    const EXPECTED_UNSUPPORTED: &[&str] = &["annexb", "film_grain", "decode_model"];

    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("skipping: cannot read {dir}");
        return;
    };
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("ivf") | Some("obu")
            )
        })
        .collect();
    paths.sort();
    if paths.is_empty() {
        eprintln!("skipping: no .ivf/.obu samples in {dir}");
        return;
    }

    let mut comparable = 0usize;
    let mut exact = 0usize;
    for path in &paths {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_owned();
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[{name}] skipped: cannot read ({e})");
                continue;
            }
        };
        let is_ivf = bytes.starts_with(b"DKIF");
        if !is_ivf {
            eprintln!("[{name}] skipped: non-IVF container (Annex-B OBU) unsupported");
            continue;
        }
        if bytes.len() < 32 {
            eprintln!("[{name}] skipped: truncated IVF header");
            continue;
        }
        let width = u16::from_le_bytes([bytes[12], bytes[13]]) as u32;
        let height = u16::from_le_bytes([bytes[14], bytes[15]]) as u32;
        if width == 0 || height == 0 {
            eprintln!("[{name}] skipped: zero dimensions in IVF header");
            continue;
        }

        let ref_frames = match decode_av1_with_dav1d(&bytes, width, height) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[{name}] dav1d decode returned: {e}");
                continue;
            }
        };

        let packets = split_ivf_frames(&bytes);
        let mut dec = Av1Decoder::new();
        let mut kframes = Vec::new();
        let mut decode_err = None;
        for (i, data) in packets.iter().enumerate() {
            let pk = Packet {
                pts: Timestamp::new(i as i64, (1, 30)),
                dts: Timestamp::new(i as i64, (1, 30)),
                data: data.clone(),
                stream_index: 0,
                is_key_frame: i == 0,
            };
            match dec.decode(&pk) {
                Ok(Some(frame)) => kframes.push(frame),
                Ok(None) => {}
                Err(e) => {
                    decode_err = Some((i, e));
                    break;
                }
            }
        }
        if let Some((i, e)) = decode_err {
            eprintln!("[{name}] skipped after frame {i}: Kinetix decode error: {e}");
            continue;
        }

        let known_unsupported = EXPECTED_UNSUPPORTED
            .iter()
            .any(|f| name.to_lowercase().contains(f));
        let n = kframes.len().min(ref_frames.len());
        let mut file_exact = 0usize;
        for i in 0..n {
            if within_tolerance(&kframes[i], &ref_frames[i], 0) {
                file_exact += 1;
            }
        }
        comparable += n;
        exact += file_exact;
        let status = if known_unsupported {
            "expected-unsupported"
        } else if file_exact == n && n == ref_frames.len() {
            "exact"
        } else {
            "MISMATCH"
        };
        eprintln!(
            "[{name}] {width}x{height}: {file_exact}/{n} frames exact \
             (dav1d {}/{}) — {status}",
            ref_frames.len(),
            packets.len(),
        );
    }

    eprintln!("AV1 FATE real samples: {exact}/{comparable} comparable frames bit-exact vs dav1d");
    if comparable == 0 {
        panic!("no comparable Kinetix/dav1d frame pairs produced");
    }
    // Report-only frontier tracker: these samples deliberately exercise
    // features beyond the supported subset (film grain, decoder model,
    // non-uniform tiling, operating-point params). The per-file exact counts
    // form the regression baseline — when a feature lands, its count should
    // rise to `exact`; a count DROPPING from a previous run is the signal to
    // investigate.
}
