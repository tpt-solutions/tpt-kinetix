//! Roundtrip test: the HLS `TsMuxer`'s segment output must demux cleanly
//! with `tpt-kinetix-demux`'s `TsDemuxer` — same PID numbering, codec
//! identification via PMT stream_type, PTS recovery from PES headers, and
//! key-frame flags from adaptation-field random-access indicators.

use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_demux::{Demuxer, TsDemuxer};
use tpt_kinetix_stream::hls::ts::TsMuxer;

/// One AVCC access unit per frame: SPS/PPS then an IDR or non-IDR slice
/// (only the first frame carries SPS/PPS, mirroring a real encoder).
fn avcc_au(frame: usize, idr: bool) -> Vec<u8> {
    let mut au = Vec::new();
    if frame == 0 {
        for nal in [[0x67u8, 0x42], [0x68, 0xCE]] {
            au.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            au.extend_from_slice(&nal);
        }
    }
    let slice_nal: [u8; 3] = if idr {
        [0x65, 0x88, 0x84]
    } else {
        [0x41, 0x9A, 0x02]
    };
    au.extend_from_slice(&(slice_nal.len() as u32).to_be_bytes());
    au.extend_from_slice(&slice_nal);
    au
}

#[test]
fn muxed_segment_demuxes_with_matching_metadata() {
    const FRAMES: usize = 6;
    let mut mux = TsMuxer::new();
    for i in 0..FRAMES {
        let pts = 90_000u64 * i as u64 / 2; // 15 fps
        mux.write_access_unit(&avcc_au(i, i == 0), pts, i == 0);
    }
    let segment = mux.finish();

    let mut demuxer = TsDemuxer::new(segment).expect("mux output must be parseable");

    // Tables reconstructed from the muxer's PAT/PMT.
    assert_eq!(demuxer.programs().len(), 1);
    let streams = demuxer.streams();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].pid, 0x0100);
    assert_eq!(streams[0].codec, Some(CodecId::H264));
    assert_eq!(streams[0].media_type, MediaType::Video);
    assert!(streams[0].is_pcr);
    assert!(demuxer.pcr_90khz(0x0100).is_some());

    // Access units come back Annex-B framed, in order, with exact PTS and
    // key-frame flags.
    let mut pts = Vec::new();
    let mut keys = Vec::new();
    while let Some(pkt) = demuxer.read_packet().unwrap() {
        assert_eq!(pkt.stream_index, 0x0100);
        // AVCC was converted to Annex-B by the muxer: 4-byte start codes.
        assert_eq!(&pkt.data[..4], &[0x00, 0x00, 0x00, 0x01]);
        pts.push(pkt.pts.value);
        keys.push(pkt.is_key_frame);
    }
    assert_eq!(pts.len(), FRAMES);
    assert_eq!(
        pts,
        (0..FRAMES as i64)
            .map(|i| 90_000 * i / 2)
            .collect::<Vec<_>>()
    );
    assert_eq!(keys.first(), Some(&true));
    assert!(keys.iter().skip(1).all(|k| !*k));
}
