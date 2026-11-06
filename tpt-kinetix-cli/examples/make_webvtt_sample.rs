//! Adds a WebVTT subtitle track to an MP4, for testing subtitle packaging.
//!
//! ffmpeg cannot write `wvtt` into MP4 on this machine, so this writes the track
//! with `Mp4Writer`. The cues are fixed (three cues over the first six seconds),
//! and the sample is overwritten if it exists.
//!
//! Usage: cargo run --release -p tpt-kinetix-cli --example make_webvtt_sample -- in.mp4 out.mp4

use std::io::Cursor;

use tpt_kinetix_core::codec::CodecId;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_demux::{Demuxer, Mp4Reader};
use tpt_kinetix_mux::{Mp4Writer, WriterOptions};

const CUES: [(&str, i64); 3] = [
    ("First subtitle", 0),
    ("Second subtitle", 2500),
    ("Third subtitle", 5000),
];

/// One `wvtt` sample: a `vttc` box holding a `payl` box with the text.
fn cue_sample(text: &str) -> Vec<u8> {
    let mut payl = (8 + text.len() as u32).to_be_bytes().to_vec();
    payl.extend_from_slice(b"payl");
    payl.extend_from_slice(text.as_bytes());
    let mut vttc = (8 + payl.len() as u32).to_be_bytes().to_vec();
    vttc.extend_from_slice(b"vttc");
    vttc.extend_from_slice(&payl);
    vttc
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 3,
        "usage: make_webvtt_sample <in.mp4> <out.mp4>"
    );
    let mut reader = Mp4Reader::open(std::fs::read(&args[1])?)?;
    let mut streams = reader.streams();
    let mut packets = Vec::new();
    while let Some(p) = reader.read_packet()? {
        packets.push(p);
    }

    let index = streams.len() as u32;
    let mut sub = StreamInfo::new(index, CodecId::WebVtt, 1000);
    sub.extradata = b"WEBVTT\n".to_vec();
    streams.push(sub);
    for (text, start) in CUES {
        packets.push(Packet {
            pts: Timestamp::new(start, (1, 1000)),
            dts: Timestamp::new(start, (1, 1000)),
            data: cue_sample(text),
            stream_index: index,
            is_key_frame: true,
        });
    }
    packets.sort_by(|a, b| a.dts.as_secs_f64().total_cmp(&b.dts.as_secs_f64()));

    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), &streams, WriterOptions::default())?;
    for p in &packets {
        w.write_packet(p)?;
    }
    std::fs::write(&args[2], w.finish()?.into_inner())?;
    println!(
        "wrote {} with a WebVTT track of {} cues",
        args[2],
        CUES.len()
    );
    Ok(())
}
