//! Throwaway diffmap for the testsrc2 96x64 intra-corpus gap.
//! Prints an 8x8-block heatmap plus exact per-pixel diffs for any block
//! with non-zero error, then identifies the first wrong block for tracing.
//! Delete before final commit.

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::{reference::decode_av1_obu_with_dav1d, synthetic::av1_intra_corpus};

#[test]
fn dbg_testsrc2_diffmap() {
    let corpus = av1_intra_corpus();
    let Some(entry) = corpus.iter().find(|e| e.label == "testsrc2") else {
        eprintln!("no testsrc2 entry");
        return;
    };
    let ref_frames = decode_av1_obu_with_dav1d(&entry.obu, entry.width, entry.height)
        .expect("dav1d reference decode");
    let ref_frame = &ref_frames[0];

    let mut dec = Av1Decoder::new();
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: entry.obu.clone(),
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec.decode(&packet).expect("kinetix decode").expect("frame");

    let w = entry.width as usize;
    let h = entry.height as usize;
    let stride = w;
    let uv_w = w / 2;
    let uv_stride = uv_w;
    let uv_h = h / 2;
    let u_off = stride * h;
    let v_off = u_off + uv_stride * uv_h;

    // --- luma heatmap ---
    eprintln!("=== LUMA 8x8-block mean-abs-diff heatmap ({w}x{h}) ===");
    for by in (0..h).step_by(8) {
        let mut row = String::new();
        for bx in (0..w).step_by(8) {
            let mut sum = 0u32;
            let mut n = 0u32;
            for y in by..(by + 8).min(h) {
                for x in bx..(bx + 8).min(w) {
                    let a = frame.data[y * stride + x] as i32;
                    let b = ref_frame.data[y * stride + x] as i32;
                    sum += (a - b).unsigned_abs();
                    n += 1;
                }
            }
            let avg = if sum == 0 { 0 } else { (sum + n / 2) / n };
            row.push_str(&format!("{avg:3} "));
        }
        eprintln!("y={by:3}: {row}");
    }

    // --- per-pixel dump for every nonzero block ---
    eprintln!("\n=== Per-pixel luma diffs for nonzero 8x8 blocks ===");
    let mut total_diff = 0u64;
    let mut first_bad: Option<(usize, usize, i32, i32)> = None;
    for by in (0..h).step_by(8) {
        for bx in (0..w).step_by(8) {
            let mut block_sum = 0u32;
            for y in by..(by + 8).min(h) {
                for x in bx..(bx + 8).min(w) {
                    let a = frame.data[y * stride + x] as i32;
                    let b = ref_frame.data[y * stride + x] as i32;
                    block_sum += (a - b).unsigned_abs();
                    total_diff += (a - b).unsigned_abs() as u64;
                    if first_bad.is_none() && (a - b).abs() > 0 {
                        first_bad = Some((x, y, a, b));
                    }
                }
            }
            if block_sum > 0 {
                let mb_col = bx / 4;  // 4x4 MI units
                let mb_row = by / 4;
                eprintln!("block bx={bx} by={by} (mi_col={mb_col} mi_row={mb_row}):");
                for y in by..(by + 8).min(h) {
                    let mut row = String::new();
                    for x in bx..(bx + 8).min(w) {
                        let a = frame.data[y * stride + x] as i32;
                        let b = ref_frame.data[y * stride + x] as i32;
                        let d = a - b;
                        if d == 0 {
                            row.push_str("   . ");
                        } else {
                            row.push_str(&format!("{d:+4} "));
                        }
                    }
                    eprintln!("  y={y}: {row}");
                }
            }
        }
    }
    eprintln!("\nFirst wrong luma pixel: {first_bad:?}");
    eprintln!("Total luma |diff| sum: {total_diff}");

    // --- chroma heatmaps ---
    for (plane, off, label) in [("U", u_off, "U"), ("V", v_off, "V")] {
        eprintln!("\n=== {label} 4x4-block mean-abs-diff heatmap ({uv_w}x{uv_h}) ===");
        let mut total = 0u64;
        for by in (0..uv_h).step_by(4) {
            let mut row = String::new();
            for bx in (0..uv_w).step_by(4) {
                let mut sum = 0u32;
                let mut n = 0u32;
                for y in by..(by + 4).min(uv_h) {
                    for x in bx..(bx + 4).min(uv_w) {
                        let a = frame.data[off + y * uv_stride + x] as i32;
                        let b = ref_frame.data[off + y * uv_stride + x] as i32;
                        sum += (a - b).unsigned_abs();
                        total += (a - b).unsigned_abs() as u64;
                        n += 1;
                    }
                }
                let avg = if sum == 0 { 0 } else { (sum + n / 2) / n };
                row.push_str(&format!("{avg:3} "));
            }
            eprintln!("y={by:3}: {row}");
        }
        eprintln!("Total {plane} |diff| sum: {total}");

        // per-pixel dump for nonzero 4x4 blocks
        eprintln!("Nonzero {plane} blocks:");
        for by in (0..uv_h).step_by(4) {
            for bx in (0..uv_w).step_by(4) {
                let mut block_sum = 0u32;
                for y in by..(by + 4).min(uv_h) {
                    for x in bx..(bx + 4).min(uv_w) {
                        let a = frame.data[off + y * uv_stride + x] as i32;
                        let b = ref_frame.data[off + y * uv_stride + x] as i32;
                        block_sum += (a - b).unsigned_abs();
                    }
                }
                if block_sum > 0 {
                    eprintln!("  bx={bx} by={by}:");
                    for y in by..(by + 4).min(uv_h) {
                        let mut row = String::new();
                        for x in bx..(bx + 4).min(uv_w) {
                            let a = frame.data[off + y * uv_stride + x] as i32;
                            let b = ref_frame.data[off + y * uv_stride + x] as i32;
                            let d = a - b;
                            if d == 0 { row.push_str("    . "); } else { row.push_str(&format!("{d:+5} ")); }
                        }
                        eprintln!("    y={y}: {row}");
                    }
                }
            }
        }
        let _ = plane;
        let _ = off;
    }
}
