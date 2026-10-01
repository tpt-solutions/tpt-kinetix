//! `tpt-kinetix-vision` encode + decode throughput (pixel and tensor paths).
//!
//! Vision has two decode outputs — reconstructed pixels for detector input and
//! a compact `Tensor` for machine consumption — so both are benched alongside
//! the encode path.
//!
//! Run with `cargo bench -p tpt-kinetix-vision`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_vision::{
    headers::{FrameHeader, FrameType, SequenceHeader},
    reconstruct::{encode_frame, FrameBuffer},
    VisionDecoder, VisionDecoderImpl,
};

/// The resolutions the baseline table is reported at.
const RESOLUTIONS: [(u32, u32); 3] = [(320, 240), (1280, 720), (1920, 1080)];

fn sequence() -> SequenceHeader {
    SequenceHeader {
        version: 1,
        max_width: 1920,
        max_height: 1080,
        chroma_present: true,
        bit_depth: 8,
        qp_precision: 0,
        max_ref_frames: 2,
        num_rans_streams: 1,
        min_block_size_log2: 3,
        max_block_size_log2: 3,
        quant_matrix_id: 0,
    }
}

/// `output_mode`: 2 = reconstructed pixels, 0 = tensor.
fn frame_header(w: u32, h: u32, output_mode: u8) -> FrameHeader {
    FrameHeader {
        frame_type: FrameType::Key,
        width: w as u16,
        height: h as u16,
        base_qp: 0,
        ref_frame_count: 0,
        output_mode,
        payload_len: 0,
    }
}

/// A detector-like source: textured regions (edges, gradients) over flat
/// background, which is what a video-for-ML codec actually has to code.
fn source(w: u32, h: u32) -> FrameBuffer {
    let mut luma = vec![64u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let px = (y * w + x) as usize;
            // Blocky high-frequency detail (8x8 blocks, as a detector sees),
            // overlaid with a coarse illumination ramp. The addition wraps rather than
            // saturating, giving high-entropy detector-like detail.
            let block = (((x / 8) * 7 + (y / 8) * 13) % 128) as u8;
            let ramp = ((x + y) % 64) as u8;
            luma[px] = block.wrapping_add(ramp / 2);
        }
    }
    let (cw, ch) = ((w as usize).div_ceil(2), (h as usize).div_ceil(2));
    let cb = vec![100u8; cw * ch];
    let cr = vec![180u8; cw * ch];
    FrameBuffer::from_yuv420(w, h, luma, cb, cr).expect("valid frame geometry")
}

fn make_packet(frame: &FrameHeader, payload: &[u8]) -> Packet {
    let mut frame = frame.clone();
    frame.payload_len = payload.len() as u32;
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

fn bench_vision(c: &mut Criterion) {
    let seq = sequence();

    for (w, h) in RESOLUTIONS {
        let pixels = (w as u64) * (h as u64);
        let src = source(w, h);

        let pixel_frame = frame_header(w, h, 2);
        let tensor_frame = frame_header(w, h, 0);

        let pixel_payload = match encode_frame(&seq, &pixel_frame, &src, None) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("vision: encode_frame failed at {w}x{h}: {e}; skipping");
                continue;
            }
        };
        let pixel_packet = make_packet(&pixel_frame, &pixel_payload);

        let tensor_payload = encode_frame(&seq, &tensor_frame, &src, None)
            .unwrap_or_else(|e| panic!("vision tensor encode failed at {w}x{h}: {e}"));
        let tensor_packet = make_packet(&tensor_frame, &tensor_payload);

        let mut group = c.benchmark_group(format!("vision_{w}x{h}"));
        group.throughput(Throughput::Elements(pixels));
        group.sample_size(10);

        group.bench_with_input(
            BenchmarkId::from_parameter("encode"),
            &(&seq, &pixel_frame, &src),
            |b, inp| {
                b.iter(|| {
                    let (seq, frame, src) = inp;
                    std::hint::black_box(encode_frame(seq, frame, src, None).expect("encode"));
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::from_parameter("decode_pixels"),
            &pixel_packet,
            |b, pkt| {
                b.iter_batched(
                    || {
                        let mut d = VisionDecoderImpl::new();
                        d.set_sequence_header(sequence());
                        d
                    },
                    |mut d| {
                        let _ = d.decode_pixels(pkt);
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::from_parameter("decode_tensor"),
            &tensor_packet,
            |b, pkt| {
                b.iter_batched(
                    || {
                        let mut d = VisionDecoderImpl::new();
                        d.set_sequence_header(sequence());
                        d
                    },
                    |mut d| {
                        let _ = d.decode_tensor(pkt);
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );

        group.finish();
    }
}

criterion_group!(benches, bench_vision);
criterion_main!(benches);
