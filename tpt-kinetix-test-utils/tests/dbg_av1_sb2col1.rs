//! Focused debug: instrument the second superblock row where corruption
//! starts. Targets testsrc's row 1 (mi_row >= 16).
use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::{reference::decode_av1_obu_with_dav1d, synthetic::av1_intra_corpus};

#[test]
fn dbg_testsrc_sb2col1() {
    let corpus = av1_intra_corpus();
    let Some(entry) = corpus.iter().find(|e| e.label == "testsrc") else {
        eprintln!("no testsrc entry");
        return;
    };

    let ref_frames = decode_av1_obu_with_dav1d(&entry.obu, entry.width, entry.height)
        .expect("dav1d reference decode");
    let ref_frame = &ref_frames[0];

    std::env::set_var("KINETIX_AV1_NOFILTER", "1");
    std::env::set_var("KINETIX_AV1_DBG", "1");
    let mut dec = Av1Decoder::new();
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: entry.obu.clone(),
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec.decode(&packet).expect("kinetix decode").expect("frame");
    std::env::remove_var("KINETIX_AV1_DBG");
    std::env::remove_var("KINETIX_AV1_NOFILTER");

    let w = entry.width as usize;
    let h = entry.height as usize;

    // Compare row 64-72 (second superblock row) in detail.
    eprintln!("\nRow 64-72, cols 32-96 (around the divergence):");
    for y in 64..72.min(h) {
        let k_row: Vec<u8> = (32..96.min(w)).map(|x| frame.data[y * w + x]).collect();
        let d_row: Vec<u8> = (32..96.min(w)).map(|x| ref_frame.data[y * w + x]).collect();
        eprintln!("y={y}: kinetix={k_row:?}");
        eprintln!("y={y}: dav1d  ={d_row:?}");
    }
}
