use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::{reference::decode_av1_obu_with_dav1d, synthetic::av1_intra_corpus};

#[test]
fn dbg_mandelbrot128() {
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
    eprintln!("ref top-left 8x8 Y:");
    for y in 0..8 {
        let row: Vec<u8> = (0..8).map(|x| ref_frame.data[y * w + x]).collect();
        eprintln!("y={y}: {row:?}");
    }
    eprintln!("kinetix top-left 8x8 Y:");
    for y in 0..8 {
        let row: Vec<u8> = (0..8).map(|x| frame.data[y * w + x]).collect();
        eprintln!("y={y}: {row:?}");
    }
}
