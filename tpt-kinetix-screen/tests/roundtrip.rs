//! Screen encode -> decode must reproduce the source **exactly**.
//!
//! `tpt-kinetix-screen` had no integration tests at all, so the Phase 3
//! optimisation of `natural.rs` / `reconstruct.rs` (367 changed lines) went in
//! with nothing checking its output. `ffmpeg_compare` later reported this codec
//! as `bit-exact=false` while *expecting* `bit-exact=true` — a real correctness
//! failure that a round-trip test here would have caught immediately.
//!
//! The codec is lossless by design: `SequenceHeader::base_qp = 0` with a key
//! frame and `dict_reset` must reproduce every sample. Any mismatch is a bug,
//! not a quality setting.
//!
//! Uses the same UI-like synthetic source as the bench (flat panels, 1px
//! separators, repeated glyph bars) so the palette/dictionary/transform paths
//! all get exercised, and varies resolution and frame count because the
//! classifier picks different block modes at different sizes.

use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_screen::{
    decoder::ScreenDecoder,
    headers::{ChromaFormat, FrameHeader, FrameType, SequenceHeader},
    reconstruct::{encode_frame, FrameBuffer},
};

fn sequence() -> SequenceHeader {
    SequenceHeader {
        version: 1,
        max_width: 1920,
        max_height: 1080,
        base_block_size_log2: 4,
        num_rans_streams: 4,
        dict_cap: 256,
        palette_cap: 64,
        glyph_max_dim: 32,
        bit_depth: 8,
        chroma_format: ChromaFormat::Yuv420,
        max_ref_frames: 1,
    }
}

fn frame_header(w: u32, h: u32) -> FrameHeader {
    FrameHeader {
        frame_type: FrameType::Key,
        width: w as u16,
        height: h as u16,
        base_qp: 0,
        ref_frame_count: 0,
        dict_version: 0,
        dict_reset: true,
        payload_len: 0,
    }
}

/// Identical to the bench's `source`: a synthetic UI frame whose flat regions,
/// hard edges and repeated glyphs drive the classifiers the codec is built on.
fn source(w: u32, h: u32, t: u32) -> FrameBuffer {
    let mut luma = vec![240u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let px = (y * w + x) as usize;
            if y % 32 == 0 || x % 64 == 0 {
                luma[px] = 128;
                continue;
            }
            if (y % 32) / 8 == 2 && (x % 24) < 14 {
                luma[px] = 16;
            } else if (y % 128) < 8 && x < w / 2 {
                luma[px] = 64;
            }
        }
    }
    // A moving highlight, so consecutive frames differ and the inter/entropy
    // paths are not trivially repeated-input cases.
    let hx = (t * 7) % w.max(1);
    let cw = (w as usize).div_ceil(2);
    let ch = (h as usize).div_ceil(2);
    let mut cb = vec![128u8; cw * ch];
    let mut cr = vec![128u8; cw * ch];
    for y in 0..h.min(12) {
        for x in hx..(hx + 24).min(w) {
            luma[(y * w + x) as usize] = 200;
        }
    }
    // Move a chroma block too, so the chroma planes are not uniformly flat.
    if cw > 8 && ch > 8 {
        let bx = (t as usize * 3) % (cw - 8);
        for y in 4..12 {
            for x in bx..bx + 8 {
                cb[y * cw + x] = 90;
                cr[y * cw + x] = 160;
            }
        }
    }
    FrameBuffer::from_yuv420(w, h, luma, cb, cr).expect("valid frame geometry")
}

/// Encode one frame and decode it back, returning the decoded luma.
fn roundtrip_luma(w: u32, h: u32, t: u32) -> Vec<u8> {
    roundtrip(w, h, t).0
}

/// Encode one frame and decode it back, returning `(luma, chroma)`, where
/// `chroma` is the decoded Cb and Cr planes concatenated.
fn roundtrip(w: u32, h: u32, t: u32) -> (Vec<u8>, Vec<u8>) {
    let seq = sequence();
    let fhdr = frame_header(w, h);
    let src = source(w, h, t);

    let payload = encode_frame(&seq, &fhdr, &src, None).expect("encode_frame");
    let mut data = fhdr.to_bytes().to_vec();
    data.extend_from_slice(&payload);

    let pkt = Packet {
        pts: Timestamp::new(t as i64, (1, 30)),
        dts: Timestamp::new(t as i64, (1, 30)),
        data,
        stream_index: 0,
        is_key_frame: true,
    };

    let mut dec = ScreenDecoder::new();
    dec.set_sequence_header(seq);
    let frame = dec
        .decode(&pkt)
        .expect("decode should succeed")
        .expect("decode should emit a frame");

    assert_eq!(
        (frame.width, frame.height),
        (w, h),
        "decoded frame geometry mismatch at {w}x{h} frame {t}"
    );
    let luma_len = (w * h) as usize;
    (
        frame.data[..luma_len].to_vec(),
        frame.data[luma_len..].to_vec(),
    )
}

/// Pins the actual v1 contract: **luma** is bit-exact, chroma is not coded and
/// decodes as all-zero.
///
/// This is the distinction that made `ffmpeg_compare` report screen as
/// non-bit-exact for a long time. That harness compares whole frames, so any
/// source with non-zero chroma can never round-trip — not because of a decoding
/// bug, but because v1 only ever writes luma (`reconstruct.rs` has no chroma
/// write path). The luma guarantee is the real one, and `check` asserts it.
///
/// If a later version codes chroma, delete this test and raise `bit_exact` back
/// to `true` in the screen `OrigCompare`.
#[test]
fn luma_is_exact_and_chroma_is_not_coded() {
    let (luma, chroma) = roundtrip(320, 240, 0);
    let src = source(320, 240, 0);
    assert_eq!(luma, src.luma, "luma must be bit-exact");
    assert!(
        chroma.iter().all(|&p| p == 0),
        "v1 does not code chroma, so both planes must decode as zero"
    );
}

fn check(w: u32, h: u32, frames: u32) {
    for t in 0..frames {
        let src = source(w, h, t);
        let got = roundtrip_luma(w, h, t);
        let want = &src.luma;
        assert_eq!(want.len(), got.len(), "luma length mismatch");
        if want != &got {
            let first = want
                .iter()
                .zip(got.iter())
                .position(|(a, b)| a != b)
                .expect("lengths equal, so a difference exists");
            let px = first as u32;
            panic!(
                "screen round-trip is not bit-exact at {w}x{h} frame {t}: \
                 first mismatch at ({}, {}): got {}, want {}",
                px % w,
                px / w,
                got[first],
                want[first]
            );
        }
    }
}

/// Block-aligned geometry: both axes are exact multiples of the 16-pixel base
/// block. These pass and stay active — they are the regression guard for the
/// palette/dictionary/transform paths.
#[test]
fn roundtrip_is_bit_exact_320x240() {
    check(320, 240, 8);
}

#[test]
fn roundtrip_is_bit_exact_1280x720() {
    check(1280, 720, 4);
}

/// 1920x1080 is 120 blocks wide but 67.5 blocks tall, so the bottom 8 rows are a
/// *partial* block.
///
/// This was failing until the decode-side block grid was changed from truncating
/// division to `div_ceil`, matching the encoder: the encoder wrote 68 block rows
/// while the decoder walked only 67, so those 8 rows were never written and
/// decoded as 0. First mismatch was at `(0, 1072)` — exactly where block row 67
/// begins.
///
/// Regression guard for that specific class of bug: any frame dimension that is
/// not a multiple of the 16-pixel base block.
#[test]
fn roundtrip_is_bit_exact_1920x1080() {
    check(1920, 1080, 3);
}

/// The same defect seen from the other axis: 67x53 is 4 full block columns plus a
/// 3-pixel remainder, and the rightmost column was never written. First mismatch
/// was at `(64, 0)` — the start of that partial column.
///
/// Together with `roundtrip_is_bit_exact_1920x1080` this covers both axes of the
/// partial-block case.
#[test]
fn roundtrip_is_bit_exact_odd_geometry() {
    check(67, 53, 3);
    check(130, 66, 2);
}
