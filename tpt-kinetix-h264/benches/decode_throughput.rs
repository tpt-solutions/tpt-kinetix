//! H.264 decode throughput benchmark.
//!
//! Compares single-threaded vs. `rayon`-parallel macroblock-row reconstruction
//! for the same synthetic slice at a resolution large enough to expose the
//! parallel speedup.
//!
//! Run with `cargo bench -p tpt-kinetix-h264`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_h264::{sps::SeqParameterSet, H264Decoder};

/// Build an SPS describing a `width`x`height` frame (in pixels).
fn sps_for(width_mbs: u32, height_mbs: u32) -> SeqParameterSet {
    SeqParameterSet {
        profile_idc: 66,
        level_idc: 40,
        seq_parameter_set_id: 0,
        chroma_format_idc: 1,
        separate_colour_plane_flag: false,
        log2_max_frame_num_minus4: 0,
        pic_order_cnt_type: 0,
        log2_max_pic_order_cnt_lsb_minus4: 0,
        num_ref_frames: 1,
        gaps_in_frame_num_value_allowed_flag: false,
        pic_width_in_mbs_minus1: width_mbs - 1,
        pic_height_in_map_units_minus1: height_mbs - 1,
        frame_mbs_only_flag: true,
        frame_cropping_flag: false,
        frame_crop_left_offset: 0,
        frame_crop_right_offset: 0,
        frame_crop_top_offset: 0,
        frame_crop_bottom_offset: 0,
    }
}

/// A minimal Annex B packet containing a single non-IDR slice NAL, enough to
/// drive `decode_slice` once an SPS is present in the store.
fn slice_packet() -> Packet {
    // Start code + slice NAL header (type 1 = non-IDR slice) + a byte of payload.
    let data = vec![0x00, 0x00, 0x00, 0x01, 0x41, 0x9a, 0x00];
    Packet {
        pts: Timestamp::new(0, (1, 90_000)),
        dts: Timestamp::new(0, (1, 90_000)),
        data,
        stream_index: 0,
        is_key_frame: false,
    }
}

fn bench_decode(c: &mut Criterion) {
    // 1920x1080 => 120x68 macroblocks.
    let (w_mbs, h_mbs) = (120u32, 68u32);
    let width = w_mbs * 16;
    let height = h_mbs * 16;
    let packet = slice_packet();

    let mut group = c.benchmark_group("h264_decode_slice_1080p");
    group.throughput(Throughput::Elements((width * height) as u64));

    for parallel in [false, true] {
        let label = if parallel { "parallel" } else { "serial" };
        group.bench_with_input(BenchmarkId::from_parameter(label), &parallel, |b, &par| {
            b.iter_batched(
                || {
                    let mut dec = H264Decoder::new().with_parallel(par);
                    dec.insert_sps(sps_for(w_mbs, h_mbs));
                    dec
                },
                |mut dec| {
                    let _ = dec.decode(&packet).expect("decode");
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);
