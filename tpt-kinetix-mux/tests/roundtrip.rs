//! Round-trip test: mux an MP4 with `tpt-kinetix-mux`, then parse it back with
//! `tpt-kinetix-demux` and verify the tracks and samples survive.

use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_demux::{mp4::Mp4Demuxer, Demuxer};
use tpt_kinetix_mux::{Mp4Muxer, Mp4MuxerConfig};

fn build_mp4() -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut muxer = Mp4Muxer::new(Mp4MuxerConfig {
        width: 320,
        height: 240,
        timescale: 30_000,
        sps: vec![0x67, 0x42, 0x00, 0x1e, 0xaa, 0xbb],
        pps: vec![0x68, 0xce, 0x3c, 0x80],
    });

    let samples: Vec<Vec<u8>> = vec![
        vec![0, 0, 0, 4, 0x65, 0x11, 0x22, 0x33], // keyframe
        vec![0, 0, 0, 3, 0x41, 0x44, 0x55],       // non-key
        vec![0, 0, 0, 2, 0x41, 0x66],             // non-key
    ];

    muxer.write_sample(&samples[0], 1000, true);
    muxer.write_sample(&samples[1], 1000, false);
    muxer.write_sample(&samples[2], 1000, false);

    (muxer.finish(), samples)
}

#[test]
fn muxed_mp4_parses_back_with_correct_track() {
    let (bytes, _samples) = build_mp4();

    let demuxer = Mp4Demuxer::new(bytes).expect("demuxer should parse muxed MP4");
    let tracks = demuxer.tracks();
    assert_eq!(tracks.len(), 1, "expected exactly one track");

    let track = &tracks[0];
    assert_eq!(track.media_type, MediaType::Video);
    assert_eq!(track.codec, Some(CodecId::H264));
    assert_eq!(track.width, 320);
    assert_eq!(track.height, 240);
    assert_eq!(track.timescale, 30_000);
    assert_eq!(track.sample_count(), 3);
}

#[test]
fn muxed_mp4_samples_read_back_byte_exact() {
    let (bytes, samples) = build_mp4();

    let mut demuxer = Mp4Demuxer::new(bytes).expect("demuxer should parse muxed MP4");

    let mut read = Vec::new();
    while let Some(pkt) = demuxer.read_packet().expect("read_packet should succeed") {
        read.push(pkt.data);
    }

    assert_eq!(read.len(), samples.len(), "sample count mismatch");
    for (i, (got, expected)) in read.iter().zip(samples.iter()).enumerate() {
        assert_eq!(got, expected, "sample {i} bytes differ");
    }
}

#[test]
fn first_sample_is_keyframe() {
    let (bytes, _) = build_mp4();
    let mut demuxer = Mp4Demuxer::new(bytes).unwrap();
    let first = demuxer.read_packet().unwrap().unwrap();
    assert!(first.is_key_frame);
    let second = demuxer.read_packet().unwrap().unwrap();
    assert!(!second.is_key_frame);
}
