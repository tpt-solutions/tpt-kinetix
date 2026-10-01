//! `tpt-kinetix-lean` encode + decode throughput.
//!
//! Lean is the embedded-first original codec, so its headline number is
//! decode frames/s and MPix/s at 320x240, 720p and 1080p. Encoding is measured
//! too, because Lean's embedded targets budget both directions.
//!
//! Run with `cargo bench -p tpt-kinetix-lean`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_lean::{
    decoder::LeanDecoder,
    headers::ChromaFormat,
    reconstruct::{encode_frame, FrameBuffer},
    FrameHeader, FrameType, SequenceHeader,
};

/// The resolutions the baseline table is reported at.
const RESOLUTIONS: [(u32, u32); 3] = [(320, 240), (1280, 720), (1920, 1080)];

fn sequence() -> SequenceHeader {
    SequenceHeader {
        version: 1,
        max_width: 1920,
        max_height: 1080,
        max_ref_frames: 4,
        min_block_size_log2: 3,
        max_block_size_log2: 3,
        bit_depth: 8,
        chroma_format: ChromaFormat::Yuv420,
        num_rans_streams: 1,
    }
}

fn frame_header(w: u32, h: u32) -> FrameHeader {
    FrameHeader {
        frame_type: FrameType::Key,
        width: w as u16,
        height: h as u16,
        base_qp: 0,
        ref_frame_count: 0,
        payload_len: 0,
    }
}

/// A synthetic natural-image-like source: smooth gradients plus a mid-frequency
/// pattern. Flat grey would let any predictor trivially win and hide real cost.
fn source(w: u32, h: u32) -> FrameBuffer {
    let mut luma = vec![0u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let v = ((x * 255) / w.max(1)) as u8 ^ ((y * 255) / h.max(1)) as u8;
            luma[(y * w + x) as usize] = v;
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

fn bench_lean(c: &mut Criterion) {
    let seq = sequence();

    for (w, h) in RESOLUTIONS {
        let frame = frame_header(w, h);
        let src = source(w, h);

        // Encode once up front so the decode case measures decode only.
        let payload = match encode_frame(&seq, &frame, &src, None) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("lean: encode_frame failed at {w}x{h}: {e}; skipping");
                continue;
            }
        };
        let packet = make_packet(&frame, &payload);

        let pixels = (w as u64) * (h as u64);
        let mut group = c.benchmark_group(format!("lean_{w}x{h}"));
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
                    let mut d = LeanDecoder::new();
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

criterion_group!(benches, bench_lean);
criterion_main!(benches);
