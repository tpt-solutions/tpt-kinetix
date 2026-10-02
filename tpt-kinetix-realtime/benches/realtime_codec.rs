//! `tpt-kinetix-realtime` encode + decode throughput.
//!
//! Realtime's design centre is sub-frame latency, so besides frames/s and
//! MPix/s this bench reports **per-slice decode cost** — the granularity the
//! codec actually emits at — via the slice grid dimensions.
//!
//! Run with `cargo bench -p tpt-kinetix-realtime`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_realtime::{
    decoder::RealtimeDecoder,
    headers::{ChromaFormat, FrameHeader, FrameType, ProfilePreset, SequenceHeader},
    reconstruct::{encode_frame_slices, FrameBuffer},
    SliceGrid,
};

/// The resolutions the baseline table is reported at.
const RESOLUTIONS: [(u32, u32); 3] = [(320, 240), (1280, 720), (1920, 1080)];

/// Slice grid: 8x8 = 64 slices, the conferencing preset from the harness.
const GRID_COLS: u8 = 8;
const GRID_ROWS: u8 = 8;

fn sequence() -> SequenceHeader {
    SequenceHeader {
        version: 1,
        max_width: 1920,
        max_height: 1080,
        profile: ProfilePreset::Conferencing,
        slice_grid_cols: GRID_COLS,
        slice_grid_rows: GRID_ROWS,
        fec_overhead_pct: 20,
        foveation_enabled: false,
        min_block_size_log2: 3,
        max_block_size_log2: 3,
        bit_depth: 8,
        chroma_format: ChromaFormat::Yuv420,
        num_rans_streams: 64,
        max_ref_frames: 1,
        max_deadline_ms: 16,
    }
}

fn frame_header(w: u32, h: u32) -> FrameHeader {
    FrameHeader {
        frame_type: FrameType::Key,
        width: w as u16,
        height: h as u16,
        base_qp: 0,
        ref_frame_count: 0,
        deadline_ms: 16,
        force_idr: true,
        foveation_center_x: w as u16 / 2,
        foveation_center_y: h as u16 / 2,
        // The parser always reads refresh_mask_len() (grid rows / 8) mask
        // bytes, so the writer must emit a zero-filled mask of that length
        // (0 bits = no intra refresh), not an empty vector.
        intra_refresh_mask: vec![0; usize::from(GRID_ROWS).div_ceil(8)],
        payload_len: 0,
    }
}

/// A moving natural-image-like clip: smooth gradients plus per-frame motion,
/// so temporal prediction actually has something to predict.
fn source(w: u32, h: u32, t: u32) -> FrameBuffer {
    let mut luma = vec![0u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let v = (((x + t * 3) * 255) / w.max(1)) as u8 ^ (((y + t * 2) * 255) / h.max(1)) as u8;
            luma[(y * w + x) as usize] = v;
        }
    }
    let (cw, ch) = ((w as usize).div_ceil(2), (h as usize).div_ceil(2));
    let cb = vec![128u8; cw * ch];
    let cr = vec![128u8; cw * ch];
    FrameBuffer::from_yuv420(w, h, luma, cb, cr).expect("valid frame geometry")
}

/// Frame a key-frame payload into a packet the decoder accepts.
fn make_packet(frame: &FrameHeader, slices: &[Vec<u8>], seq: &SequenceHeader) -> Packet {
    let grid = SliceGrid {
        cols: seq.slice_grid_cols,
        rows: seq.slice_grid_rows,
    };
    let framed = grid.frame(slices).expect("frame the slice set");
    // The wire format carries the payload length in the frame header and the
    // decoder slices the packet by it. With the placeholder 0 the decoder
    // rejects the packet outright — which this bench's decode case silently
    // measured until the ffmpeg-compare harness surfaced it (2026-10-02).
    let frame = FrameHeader {
        payload_len: framed.len() as u32,
        ..frame.clone()
    };
    let header = frame.to_bytes();
    let mut data = Vec::with_capacity(header.len() + framed.len());
    data.extend_from_slice(&header);
    data.extend_from_slice(&framed);
    Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data,
        stream_index: 0,
        is_key_frame: true,
    }
}

fn bench_realtime(c: &mut Criterion) {
    let seq = sequence();

    for (w, h) in RESOLUTIONS {
        let frame = frame_header(w, h);
        let src = source(w, h, 0);

        let slices = match encode_frame_slices(&seq, &frame, &src, None) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("realtime: encode_frame_slices failed at {w}x{h}: {e}; skipping");
                continue;
            }
        };
        let packet = make_packet(&frame, &slices, &seq);

        let pixels = (w as u64) * (h as u64);
        let mut group = c.benchmark_group(format!("realtime_{w}x{h}"));
        // Elements are slices: 64 per frame is the latency-relevant unit.
        group.throughput(Throughput::Elements(slices.len() as u64));
        group.sample_size(10);

        group.bench_with_input(
            BenchmarkId::from_parameter("encode"),
            &(&seq, &frame, &src),
            |b, inp| {
                b.iter(|| {
                    let (seq, frame, src) = inp;
                    std::hint::black_box(
                        encode_frame_slices(seq, frame, src, None).expect("encode"),
                    );
                });
            },
        );

        group.bench_with_input(BenchmarkId::from_parameter("decode"), &packet, |b, pkt| {
            b.iter_batched(
                || {
                    let mut d = RealtimeDecoder::new();
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
        let _ = pixels;
    }
}

criterion_group!(benches, bench_realtime);
criterion_main!(benches);
