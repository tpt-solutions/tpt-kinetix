//! Diagnostic: dump per-block CAVLC metadata (nC, TotalCoeff, TrailingOnes)
//! alongside the coefficient arrays for every macroblock in the first few rows.
//! This lets us compare our parsed intermediates against a reference decoder to
//! identify where the CAVLC parsing diverges.

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;
use tpt_kinetix_test_utils::trace_dump::{MapTracer, Stage};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;

fn generate(dir: &std::path::Path) -> Option<Vec<u8>> {
    let h264 = dir.join("cavlc_diag.h264");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size={WIDTH}x{HEIGHT}:rate=1:duration=1"),
            "-frames:v",
            "1",
            "-c:v",
            "libx264",
            "-profile:v",
            "baseline",
            "-g",
            "1",
            "-bf",
            "0",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock=1",
            h264.to_str()?,
        ])
        .output()
        .ok()?;
    if !ok.status.success() {
        eprintln!(
            "ffmpeg encode failed: {}",
            String::from_utf8_lossy(&ok.stderr)
        );
        return None;
    }
    std::fs::read(&h264).ok()
}

#[test]
fn dump_cavlc_block_info_first_rows() {
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_cavlc_diag");
    std::fs::create_dir_all(&dir).unwrap();

    let annexb = match generate(&dir) {
        Some(b) => b,
        None => {
            eprintln!("ffmpeg not available or encode failed; skipping");
            return;
        }
    };

    let mut dec = H264Decoder::new();
    let mut tracer = MapTracer::new();
    let pkt = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let _frame = dec
        .decode_with_tracer(&pkt, &mut tracer)
        .expect("decode should not error")
        .expect("a frame should be produced");

    let mb_cols = WIDTH / 16;
    let mb_rows = HEIGHT / 16;

    // Dump first 2 rows of MBs (8 MBs for 64x48).
    let dump_rows = mb_rows.min(2);
    for mb_y in 0..dump_rows {
        for mb_x in 0..mb_cols {
            eprintln!("=== MB({mb_x},{mb_y}) ===");

            // Luma blocks: DC block is blk=16, AC blocks 0..15.
            for blk in 0u8..=16u8 {
                let info_key = tpt_kinetix_test_utils::trace_dump::TraceKey {
                    mb_x,
                    mb_y,
                    plane: tpt_kinetix_h264::TracePlane::Luma,
                    blk,
                    stage: Stage::CavlcBlockInfo,
                };
                let coeffs_key = tpt_kinetix_test_utils::trace_dump::TraceKey {
                    mb_x,
                    mb_y,
                    plane: tpt_kinetix_h264::TracePlane::Luma,
                    blk,
                    stage: Stage::CavlcCoeffs,
                };

                let info = tracer.block_info.get(&info_key);
                let coeffs = tracer.values.get(&coeffs_key);

                let label = if blk == 16 {
                    "DC".to_string()
                } else {
                    format!("blk{blk}")
                };

                match (info, coeffs) {
                    (Some(info), Some(coeffs)) => {
                        let nz_count = coeffs.iter().filter(|&&v| v != 0).count();
                        eprintln!(
                            "  Luma {label:>5}: nC={nc:>3} TC={tc:>2} T1={t1} nonzero={nz_count}/{}  coeffs={coeffs:?}",
                            coeffs.len(),
                            nc = info.n_c,
                            tc = info.total_coeff,
                            t1 = info.trailing_ones,
                        );
                    }
                    (Some(info), None) => {
                        eprintln!(
                            "  Luma {label:>5}: nC={nc:>3} TC={tc:>2} T1={t1}  (no coeff trace)",
                            nc = info.n_c,
                            tc = info.total_coeff,
                            t1 = info.trailing_ones,
                        );
                    }
                    _ => {}
                }
            }

            // Chroma Cb blocks
            for blk in 0u8..4u8 {
                let info_key = tpt_kinetix_test_utils::trace_dump::TraceKey {
                    mb_x,
                    mb_y,
                    plane: tpt_kinetix_h264::TracePlane::Cb,
                    blk,
                    stage: Stage::CavlcBlockInfo,
                };
                let coeffs_key = tpt_kinetix_test_utils::trace_dump::TraceKey {
                    mb_x,
                    mb_y,
                    plane: tpt_kinetix_h264::TracePlane::Cb,
                    blk,
                    stage: Stage::CavlcCoeffs,
                };
                let info = tracer.block_info.get(&info_key);
                let coeffs = tracer.values.get(&coeffs_key);

                if let (Some(info), Some(coeffs)) = (info, coeffs) {
                    let nz_count = coeffs.iter().filter(|&&v| v != 0).count();
                    eprintln!(
                        "  Cb    blk{blk}: nC={nc:>3} TC={tc:>2} T1={t1} nonzero={nz_count}/{}  coeffs={coeffs:?}",
                        coeffs.len(),
                        nc = info.n_c,
                        tc = info.total_coeff,
                        t1 = info.trailing_ones,
                    );
                }
            }

            // Chroma Cr blocks
            for blk in 0u8..4u8 {
                let info_key = tpt_kinetix_test_utils::trace_dump::TraceKey {
                    mb_x,
                    mb_y,
                    plane: tpt_kinetix_h264::TracePlane::Cr,
                    blk,
                    stage: Stage::CavlcBlockInfo,
                };
                let coeffs_key = tpt_kinetix_test_utils::trace_dump::TraceKey {
                    mb_x,
                    mb_y,
                    plane: tpt_kinetix_h264::TracePlane::Cr,
                    blk,
                    stage: Stage::CavlcCoeffs,
                };
                let info = tracer.block_info.get(&info_key);
                let coeffs = tracer.values.get(&coeffs_key);

                if let (Some(info), Some(coeffs)) = (info, coeffs) {
                    let nz_count = coeffs.iter().filter(|&&v| v != 0).count();
                    eprintln!(
                        "  Cr    blk{blk}: nC={nc:>3} TC={tc:>2} T1={t1} nonzero={nz_count}/{}  coeffs={coeffs:?}",
                        coeffs.len(),
                        nc = info.n_c,
                        tc = info.total_coeff,
                        t1 = info.trailing_ones,
                    );
                }
            }
        }
    }
}
