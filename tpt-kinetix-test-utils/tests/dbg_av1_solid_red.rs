//! Scratch debugging harness for the AV1 intra-decode desync investigation.
//! Not part of the permanent test suite — delete once root-caused.
use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::{reference::decode_av1_obu_with_dav1d, synthetic::av1_intra_corpus};

#[test]
fn dbg_solid_red() {
    let corpus = av1_intra_corpus();
    let Some(entry) = corpus.iter().find(|e| e.label == "solid_red") else {
        eprintln!("no solid_red entry (ffmpeg unavailable?)");
        return;
    };
    eprintln!("obu bytes: {}", entry.obu.len());
    eprintln!("obu hex: {:02x?}", &entry.obu[..entry.obu.len().min(64)]);

    let ref_frames = decode_av1_obu_with_dav1d(&entry.obu, entry.width, entry.height)
        .expect("dav1d reference decode");
    let ref_frame = &ref_frames[0];
    eprintln!("ref frame len: {}", ref_frame.data.len());
    eprintln!("ref top-left 8x8 Y:");
    for y in 0..8 {
        let row: Vec<u8> = (0..8)
            .map(|x| ref_frame.data[y * entry.width as usize + x])
            .collect();
        eprintln!("{row:?}");
    }

    let mut dec = Av1Decoder::new();
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: entry.obu.clone(),
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec.decode(&packet).expect("kinetix decode").expect("frame");
    eprintln!("kinetix frame data len: {}", frame.data.len());
    eprintln!("kinetix top-left 8x8 Y:");
    for y in 0..8 {
        let row: Vec<u8> = (0..8)
            .map(|x| frame.data[y * entry.width as usize + x])
            .collect();
        eprintln!("{row:?}");
    }

    if let Some(fh) = dec.last_frame_header() {
        eprintln!(
            "FrameHeader: type={:?} w={} h={} base_q_idx={} tile_cols={} tile_rows={} lossless={} delta_q_present={} delta_lf_present={} reduced_tx_set={} cdef_bits={}",
            fh.frame_type, fh.width, fh.height, fh.base_q_idx, fh.tile_cols, fh.tile_rows, fh.lossless,
            fh.delta_q_present, fh.delta_lf_present, fh.reduced_tx_set, fh.cdef_bits
        );
    }
    if let Some(seq) = dec.sequence_header() {
        eprintln!(
            "SeqHeader: sb128={} enable_cdef={} enable_restoration={} profile={} enable_filter_intra={} enable_intra_edge_filter={}",
            seq.use_128x128_superblock, seq.enable_cdef, seq.enable_restoration, seq.seq_profile,
            seq.enable_filter_intra, seq.enable_intra_edge_filter
        );
    }
    if let Some(fh) = dec.last_frame_header() {
        eprintln!(
            "allow_screen_content_tools={}",
            fh.allow_screen_content_tools
        );
    }
}
