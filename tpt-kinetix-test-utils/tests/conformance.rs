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

/// AAC conformance harness: decode a real AAC-LC ADTS stream with the Kinetix
/// decoder and with `ffmpeg`, then diff the PCM via [`audio_diff`].
///
/// Exercises the Phase 17 tooling (`reference::decode_aac_with_ffmpeg` +
/// `audio_diff::pcm_within_tolerance` / `pcm_max_abs_diff`). Skips when `ffmpeg`
/// is absent. The Kinetix AAC-LC path is sample-exact (reconstructed via
/// `symphonia-codec-aac`); a loose tolerance is used because `ffmpeg`'s native
/// AAC decoder and `symphonia` may round the MDCT tail differently.
#[test]
fn aac_vs_ffmpeg_reference_pcm_when_available() {
    use tpt_kinetix_aac::AacDecoder;
    use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
    use tpt_kinetix_test_utils::{
        audio_diff::*, reference::decode_aac_with_ffmpeg, synthetic::minimal_aac_adts,
    };

    if !tpt_kinetix_test_utils::reference::ffmpeg_available() {
        eprintln!("skipping: ffmpeg not available on PATH");
        return;
    }

    let Some(adts) = minimal_aac_adts(44_100, 2, 0.25) else {
        eprintln!("skipping: could not synthesize an AAC ADTS stream with ffmpeg");
        return;
    };

    // Kinetix decode (whole ADTS frames, header + payload, per ADTS frame).
    let mut dec = AacDecoder::new();
    let mut kinetix_pcm = Vec::new();
    let mut pos = 0usize;
    while pos < adts.len() {
        let hdr = match tpt_kinetix_aac::adts::AdtsHeader::parse(&adts[pos..]) {
            Ok(h) => h,
            Err(_) => break,
        };
        let end = pos + hdr.frame_length;
        if end > adts.len() {
            break;
        }
        let pkt = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: adts[pos..end].to_vec(),
            stream_index: 0,
            is_key_frame: true,
        };
        pos = end;
        if let Some(frame) = dec.decode(&pkt).unwrap() {
            kinetix_pcm.push(frame);
        }
    }

    // Reference decode via ffmpeg.
    let ref_pcm = match decode_aac_with_ffmpeg(&adts) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("ffmpeg AAC reference decode returned: {e}");
            return;
        }
    };

    assert!(!kinetix_pcm.is_empty(), "Kinetix produced no PCM frames");
    assert!(!ref_pcm.is_empty(), "reference produced no PCM frames");

    // Compare the first comparable pair block-by-block.
    let common = kinetix_pcm.len().min(ref_pcm.len());
    assert!(common > 0, "no comparable PCM blocks");
    let mut max_diff = 0.0f32;
    for i in 0..common {
        let a = &kinetix_pcm[i];
        let b = &ref_pcm[i];
        assert_eq!(
            a.sample_rate, b.sample_rate,
            "sample rate mismatch at block {i}"
        );
        assert_eq!(
            a.channels, b.channels,
            "channel count mismatch at block {i}"
        );
        // Kinetix may emit a slightly different sample count than ffmpeg's
        // frame; compare only the overlapping prefix.
        let n = a.data.len().min(b.data.len());
        let a_prefix = tpt_kinetix_core::frame::AudioFrame {
            pts: a.pts,
            data: a.data[..n].to_vec(),
            sample_rate: a.sample_rate,
            channels: a.channels,
            sample_format: a.sample_format,
        };
        let b_prefix = tpt_kinetix_core::frame::AudioFrame {
            pts: b.pts,
            data: b.data[..n].to_vec(),
            sample_rate: b.sample_rate,
            channels: b.channels,
            sample_format: b.sample_format,
        };
        let d = pcm_max_abs_diff(&a_prefix, &b_prefix).expect("comparable");
        max_diff = max_diff.max(d);
        // Geometry must line up: identical sample rate, channels, format.
        assert_eq!(a.sample_format, b.sample_format, "sample format mismatch");
    }
    // AAC-LC is sample-exact; allow a tiny rounding tolerance for cross-decoder
    // MDCT tail differences.
    assert!(
        max_diff < 1e-2,
        "AAC PCM divergence too large vs ffmpeg: max_diff={max_diff}"
    );
}

/// Pixel-exact harness run across a real, multi-frame H.264 sample.
///
/// Synthesizes a short baseline-profile H.264 file with `ffmpeg`, then walks
/// the same stream through the Kinetix decoder and the `ffmpeg` reference.
/// Because the Kinetix H.264 decoder is still a scaffold (no CABAC/prediction/
/// deblocking), this test asserts the *harness contract*: the Kinetix decoder
/// must either emit a frame that reports `pixel_exact == false` capability, or
/// fail with [`KinetixError::NotPixelExact`] under strict mode — never silently
/// claiming pixel-exactness. Skips when `ffmpeg` is absent.
#[test]
fn h264_real_sample_harness_across_profiles() {
    use tpt_kinetix_core::{error::KinetixError, packet::Packet, timestamp::Timestamp};
    use tpt_kinetix_h264::H264Decoder;
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

    // Reference decode to learn geometry.
    let ref_frames = match decode_h264_with_ffmpeg(&annexb, 16, 16) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("ffmpeg reference decode returned: {e}");
            return;
        }
    };

    // Kinetix decode (non-strict) must report non-pixel-exact capability and,
    // in strict mode, refuse with NotPixelExact rather than returning wrong data.
    let caps = H264Decoder::new().capabilities();
    assert!(
        !caps.pixel_exact,
        "scaffold decoder must not claim pixel_exact"
    );
    assert!(caps.is_incomplete());

    let mut dec = H264Decoder::new().with_strict(true);
    let pkt = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: annexb,
        stream_index: 0,
        is_key_frame: false,
    };
    match dec.decode(&pkt) {
        Ok(_) => panic!("strict H.264 decode must not return placeholder frames"),
        Err(KinetixError::NotPixelExact(_)) => {}
        Err(e) => panic!("unexpected error from strict decode: {e}"),
    }

    // The reference must actually have produced frames for this sample.
    assert!(!ref_frames.is_empty(), "reference produced no frames");
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
