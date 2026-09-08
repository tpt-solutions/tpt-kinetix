//! Scratch harness: localise the testsrc_128x96 chroma ±1 reconstruction
//! gap (todo-av1.md 2026-09-08). Dumps first diverging U/V pixel vs the
//! dav1d reference and a window around it. Delete once root-caused.
use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::{reference::decode_av1_obu_with_dav1d, synthetic::av1_intra_corpus};

#[test]
fn dbg_testsrc_chroma() {
    let corpus = av1_intra_corpus();
    let Some(entry) = corpus.iter().find(|e| e.label == "testsrc") else {
        eprintln!("no testsrc entry (ffmpeg unavailable?)");
        return;
    };
    // Escape hatch: dump the exact OBU the corpus feeds, so a standalone
    // (patched) dav1d can decode the identical bytes with e.g.
    // `--inloopfilters nocdef`.
    if let Ok(path) = std::env::var("KINETIX_DUMP_OBU") {
        std::fs::write(&path, &entry.obu).unwrap();
        eprintln!("wrote {} OBU bytes to {path}", entry.obu.len());
        return;
    }
    // Escape hatch: read the reference from a raw yuv420p file (a specific
    // dav1d filter-pipeline variant) instead of the default dav1d decode.
    let ref_data: Vec<u8> = if let Ok(path) = std::env::var("KINETIX_REF_YUV") {
        std::fs::read(&path).unwrap()
    } else {
        let Ok(ref_frames) = decode_av1_obu_with_dav1d(&entry.obu, entry.width, entry.height)
        else {
            eprintln!("dav1d reference unavailable");
            return;
        };
        ref_frames[0].data.clone()
    };
    let ref_frame = tpt_kinetix_core::frame::VideoFrame {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: ref_data,
        width: entry.width,
        height: entry.height,
        pixel_format: tpt_kinetix_core::pixel_format::PixelFormat::Yuv420p,
        is_key_frame: true,
    };
    let ref_frame = &ref_frame;

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
    let cw = w / 2;
    let ch = h / 2;
    let y_size = w * h;
    let c_size = cw * ch;

    for (pname, poff) in [("U", y_size), ("V", y_size + c_size)] {
        let k = &frame.data[poff..poff + c_size];
        let r = &ref_frame.data[poff..poff + c_size];
        let mut first: Option<(usize, usize)> = None;
        let mut ndiff = 0;
        let mut maxd = 0i32;
        for cy in 0..ch {
            for cx in 0..cw {
                let d = (k[cy * cw + cx] as i32 - r[cy * cw + cx] as i32).abs();
                if d != 0 {
                    ndiff += 1;
                    maxd = maxd.max(d);
                    if first.is_none() {
                        first = Some((cx, cy));
                    }
                }
            }
        }
        eprintln!("plane {pname}: {ndiff}/{c_size} px differ, max |diff|={maxd}, first={first:?}");
        if let Some((fx, fy)) = first {
            let x0 = fx.saturating_sub(4);
            let y0 = fy.saturating_sub(2);
            eprintln!("  window x={x0}..{} y={y0}..{}", x0 + 12, y0 + 8);
            for cy in y0..(y0 + 8).min(ch) {
                let kr: Vec<i32> = (x0..(x0 + 12).min(cw))
                    .map(|cx| k[cy * cw + cx] as i32)
                    .collect();
                let rr: Vec<i32> = (x0..(x0 + 12).min(cw))
                    .map(|cx| r[cy * cw + cx] as i32)
                    .collect();
                let dr: Vec<i32> = kr.iter().zip(&rr).map(|(a, b)| a - b).collect();
                eprintln!("  cy={cy:3} k={kr:?}");
                eprintln!("  cy={cy:3} r={rr:?}");
                eprintln!("  cy={cy:3} d={dr:?}");
            }
        }
    }
}
