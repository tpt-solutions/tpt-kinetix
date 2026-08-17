//! Scratch debugging harness for the AV1 multi-block intra-decode
//! investigation (post-`solid_red` fix). Not part of the permanent test
//! suite — delete once root-caused.
use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::{reference::decode_av1_obu_with_dav1d, synthetic::av1_intra_corpus};

#[test]
fn dbg_smptebars() {
    let corpus = av1_intra_corpus();
    let Some(entry) = corpus.iter().find(|e| e.label == "smptebars") else {
        eprintln!("no smptebars entry (ffmpeg unavailable?)");
        return;
    };
    eprintln!("obu bytes: {}", entry.obu.len());

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
    eprintln!(
        "dims {w}x{h}, ref len {}, kinetix len {}",
        ref_frame.data.len(),
        frame.data.len()
    );

    // Dump per-16x16-block max abs diff over the Y plane, so we can see which
    // blocks (in decode order, top-left first) are already right vs wrong.
    for by in (0..h).step_by(16) {
        let mut row = String::new();
        for bx in (0..w).step_by(16) {
            let mut max_diff = 0i32;
            for y in by..(by + 16).min(h) {
                for x in bx..(bx + 16).min(w) {
                    let r = ref_frame.data[y * w + x] as i32;
                    let k = frame.data[y * w + x] as i32;
                    max_diff = max_diff.max((r - k).abs());
                }
            }
            row.push_str(&format!("{max_diff:4}"));
        }
        eprintln!("{row}");
    }

    eprintln!("ref top-left 32x8 Y:");
    for y in 0..8 {
        let row: Vec<u8> = (0..32).map(|x| ref_frame.data[y * w + x]).collect();
        eprintln!("{row:?}");
    }
    eprintln!("kinetix top-left 32x8 Y:");
    for y in 0..8 {
        let row: Vec<u8> = (0..32).map(|x| frame.data[y * w + x]).collect();
        eprintln!("{row:?}");
    }
}
