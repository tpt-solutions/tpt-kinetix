use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::{reference::decode_av1_obu_with_dav1d, synthetic::av1_intra_corpus};

#[test]
fn dbg_mandelbrot_diffmap() {
    let corpus = av1_intra_corpus();
    let Some(entry) = corpus.iter().find(|e| e.label == "mandelbrot") else {
        eprintln!("no mandelbrot entry");
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

    // Per-8x8-block mean abs diff heatmap over the full luma plane.
    eprintln!("block mean-abs-diff heatmap (8x8 blocks), w={w} h={h}:");
    for by in (0..h).step_by(8) {
        let mut row = String::new();
        for bx in (0..w).step_by(8) {
            let mut sum = 0u32;
            let mut n = 0u32;
            for y in by..(by + 8).min(h) {
                for x in bx..(bx + 8).min(w) {
                    let a = frame.data[y * w + x] as i32;
                    let b = ref_frame.data[y * w + x] as i32;
                    sum += (a - b).unsigned_abs();
                    n += 1;
                }
            }
            let avg = sum / n.max(1);
            row.push_str(&format!("{avg:4}"));
        }
        eprintln!("y={by:3}: {row}");
    }

    eprintln!("zoom region x=32..48 y=0..16 kinetix/ref actual values:");
    for y in 0..16 {
        let k: Vec<u8> = (32..48).map(|x| frame.data[y * w + x]).collect();
        let r: Vec<u8> = (32..48).map(|x| ref_frame.data[y * w + x]).collect();
        eprintln!("y={y:3} kinetix={k:?}");
        eprintln!("y={y:3} ref    ={r:?}");
    }

    eprintln!("zoom region x=32..64 y=8..32 kinetix vs ref (per-pixel diff):");
    for y in 8..32 {
        let mut row = String::new();
        for x in 32..64 {
            let a = frame.data[y * w + x] as i32;
            let b = ref_frame.data[y * w + x] as i32;
            row.push_str(&format!("{:4}", a - b));
        }
        eprintln!("y={y:3}: {row}");
    }

    // First pixel (raster order) where abs diff exceeds a small threshold.
    let mut first: Option<(usize, usize, i32, i32)> = None;
    'outer: for y in 0..h {
        for x in 0..w {
            let a = frame.data[y * w + x] as i32;
            let b = ref_frame.data[y * w + x] as i32;
            if (a - b).abs() > 3 {
                first = Some((x, y, a, b));
                break 'outer;
            }
        }
    }
    eprintln!("first pixel with |diff|>3 (raster order): {first:?}");
}
