//! WebVTT subtitle tracks (`wvtt`, ISO/IEC 14496-30) through `Mp4Writer` and the
//! real demuxer.
//!
//! ffmpeg in this environment cannot read `wvtt` inside MP4 (it reports the track
//! as `data` with codec id 0, and its muxer has no `wvtt` tag), so there is no
//! external oracle here. The checks below are therefore structural: the sample
//! entry layout and the cue bytes must survive a write and a read unchanged.

use std::io::Cursor;

use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_demux::{Demuxer, Mp4Reader};
use tpt_kinetix_mux::{Mp4Writer, WriterOptions};

const HEADER: &[u8] = b"WEBVTT\n";

fn subtitle_stream() -> StreamInfo {
    let mut s = StreamInfo::new(0, CodecId::WebVtt, 1000);
    s.extradata = HEADER.to_vec();
    s
}

/// One WebVTT cue sample: a `vttc` box holding a `payl` box with the text.
fn cue(text: &str) -> Vec<u8> {
    let mut payl = (8 + text.len() as u32).to_be_bytes().to_vec();
    payl.extend_from_slice(b"payl");
    payl.extend_from_slice(text.as_bytes());
    let mut vttc = (8 + payl.len() as u32).to_be_bytes().to_vec();
    vttc.extend_from_slice(b"vttc");
    vttc.extend_from_slice(&payl);
    vttc
}

/// Cues start at 0: a track whose first sample starts later loses that lead-in
/// on read, because the demuxer ignores leading empty edits.
fn cues() -> Vec<Packet> {
    ["Hello subtitles", "Second cue", "Third cue, longer text"]
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let start = (i as i64) * 2000;
            Packet {
                pts: Timestamp::new(start, (1, 1000)),
                dts: Timestamp::new(start, (1, 1000)),
                data: cue(t),
                stream_index: 0,
                is_key_frame: true,
            }
        })
        .collect()
}

fn write(packets: &[Packet]) -> Vec<u8> {
    let streams = [subtitle_stream()];
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), &streams, WriterOptions::default())
        .expect("writer accepts a WebVTT track");
    for p in packets {
        w.write_packet(p).expect("write cue");
    }
    w.finish().expect("finish").into_inner()
}

#[test]
fn webvtt_track_round_trips_through_the_demuxer() {
    let packets = cues();
    let file = write(&packets);

    let mut reader = Mp4Reader::open(file).expect("reader opens the file");
    let streams = reader.streams();
    assert_eq!(streams.len(), 1);
    let s = &streams[0];
    assert_eq!(s.codec, CodecId::WebVtt);
    assert_eq!(s.media_type, MediaType::Subtitle);
    assert_eq!(s.extradata, HEADER, "vttC carries the WebVTT header");

    let mut got = Vec::new();
    while let Some(p) = reader.read_packet().expect("read cue") {
        got.push(p);
    }
    assert_eq!(got.len(), packets.len());
    for (want, got) in packets.iter().zip(&got) {
        assert_eq!(
            got.data, want.data,
            "cue bytes are passed through unchanged"
        );
        assert_eq!(got.pts.value, want.pts.value);
    }
}

#[test]
fn sample_entry_is_wvtt_with_vtt_config() {
    let file = write(&cues());
    // The wvtt entry: SampleEntry header (6 reserved + data_reference_index),
    // then a vttC box holding the header text.
    let wvtt = file
        .windows(4)
        .position(|w| w == b"wvtt")
        .expect("wvtt sample entry");
    let entry = &file[wvtt + 4..];
    assert_eq!(&entry[..6], &[0u8; 6], "reserved bytes");
    assert_eq!(&entry[6..8], &1u16.to_be_bytes(), "data_reference_index");
    assert_eq!(&entry[8..12], &(8 + HEADER.len() as u32).to_be_bytes());
    assert_eq!(&entry[12..16], b"vttC");
    assert_eq!(&entry[16..16 + HEADER.len()], HEADER);
}

#[test]
fn subtitle_track_writes_a_null_media_header() {
    let file = write(&cues());
    assert!(
        file.windows(4).any(|w| w == b"nmhd"),
        "subtitle tracks use nmhd"
    );
    assert!(!file.windows(4).any(|w| w == b"smhd"), "not an audio track");
}
