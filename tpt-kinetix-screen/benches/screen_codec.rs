//! `tpt-kinetix-screen` encode + decode throughput.
//!
//! Screen content is dominated by flat regions, hard edges and repeated glyphs,
//! so the synthetic source here is a UI-like mosaic (large flat blocks, sharp
//! 1px borders, repeated text-like bars) rather than a photographic gradient.
//! That exercises the classifier, palette and dictionary paths the codec is
//! actually built around.
//!
//! Run with `cargo bench -p tpt-kinetix-screen`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_screen::{
    decoder::ScreenDecoder,
    headers::{ChromaFormat, FrameHeader, FrameType, SequenceHeader},
    reconstruct::{encode_frame, FrameBuffer},
};

/// The resolutions the baseline table is reported at.
const RESOLUTIONS: [(u32, u32); 3] = [(320, 240), (1280, 720), (1920, 1080)];

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

/// A synthetic UI/desktop frame: flat panels, 1px separators, and repeated
/// glyph-like bars so the palette/dictionary classifiers have real work.
fn source(w: u32, h: u32) -> FrameBuffer {
    let mut luma = vec![240u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let px = (y * w + x) as usize;
            // Panel separators: every 32 rows and 64 columns.
            if y % 32 == 0 || x % 64 == 0 {
                luma[px] = 128;
                continue;
            }
            // A repeating "text line" motif: alternating dark bars on light rows.
            if (y % 32) / 8 == 2 && (x % 24) < 14 {
                luma[px] = 16;
            } else if (y % 128) < 8 && x < w / 2 {
                // A flat title bar, the large-uniform-region case.
                luma[px] = 64;
            }
        }
    }
    let (cw, ch) = ((w as usize).div_ceil(2), (h as usize).div_ceil(2));
    let cb = vec![128u8; cw * ch];
    let cr = vec![128u8; cw * ch];
    FrameBuffer::from_yuv420(w, h, luma, cb, cr).expect("valid frame geometry")
}

fn make_packet(frame: &FrameHeader, payload: &[u8]) -> Packet {
    let header = frame.to_bytes();
    let mut data = Vec::with_capacity(header.len() + payload.len());
    data.extend_from_slice(&header);
    data.extend_from_slice(payload);
    Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data,
        stream_index: 0,
        is_key_frame: true,
    }
}

fn bench_screen(c: &mut Criterion) {
    let seq = sequence();

    for (w, h) in RESOLUTIONS {
        let frame = frame_header(w, h);
        let src = source(w, h);

        let payload = match encode_frame(&seq, &frame, &src, None) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("screen: encode_frame failed at {w}x{h}: {e}; skipping");
                continue;
            }
        };
        let packet = make_packet(&frame, &payload);

        let pixels = (w as u64) * (h as u64);
        let mut group = c.benchmark_group(format!("screen_{w}x{h}"));
        group.throughput(Throughput::Elements(pixels));
        group.sample_size(10);

        group.bench_with_input(
            BenchmarkId::from_parameter("encode"),
            &(&seq, &frame, &src),
            |b, inp| {
                b.iter(|| {
                    let (seq, frame, src) = inp;
                    std::hint::black_box(encode_frame(seq, frame, src, None).expect("encode"));
                });
            },
        );

        group.bench_with_input(BenchmarkId::from_parameter("decode"), &packet, |b, pkt| {
            b.iter_batched(
                || {
                    let mut d = ScreenDecoder::new();
                    d.set_sequence_header(sequence());
                    d
                },
                |mut d| {
                    let _ = d.decode(pkt);
                },
                criterion::BatchSize::SmallInput,
            );
        });

        group.finish();
    }
}

criterion_group!(benches, bench_screen);
criterion_main!(benches);
