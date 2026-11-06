//! WebVTT subtitles through the CLI: `remux` to MP4 keeps the `wvtt` track with
//! its cue bytes, and `remux` to WebM skips it with a message (WebM has no
//! WebVTT mapping yet). The input is written by `Mp4Writer`, because ffmpeg
//! cannot produce `wvtt` in MP4 here.

use std::io::Cursor;
use std::path::Path;
use std::process::Command;

use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_demux::{Demuxer, Mp4Reader};
use tpt_kinetix_mux::{Mp4Writer, WriterOptions};

const CUES: [&str; 2] = ["Hello subtitles", "Second cue"];

fn cue(text: &str) -> Vec<u8> {
    let mut payl = (8 + text.len() as u32).to_be_bytes().to_vec();
    payl.extend_from_slice(b"payl");
    payl.extend_from_slice(text.as_bytes());
    let mut vttc = (8 + payl.len() as u32).to_be_bytes().to_vec();
    vttc.extend_from_slice(b"vttc");
    vttc.extend_from_slice(&payl);
    vttc
}

fn write_input(path: &Path) {
    let mut s = StreamInfo::new(0, CodecId::WebVtt, 1000);
    s.extradata = b"WEBVTT\n".to_vec();
    let packets: Vec<Packet> = CUES
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let start = i as i64 * 2000;
            Packet {
                pts: Timestamp::new(start, (1, 1000)),
                dts: Timestamp::new(start, (1, 1000)),
                data: cue(t),
                stream_index: 0,
                is_key_frame: true,
            }
        })
        .collect();
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), &[s], WriterOptions::default()).unwrap();
    for p in &packets {
        w.write_packet(p).unwrap();
    }
    std::fs::write(path, w.finish().unwrap().into_inner()).unwrap();
}

fn remux(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .arg("remux")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn remux_to_mp4_keeps_the_webvtt_track() {
    let dir = std::env::temp_dir().join(format!("tpt-webvtt-remux-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("in.mp4");
    let output = dir.join("out.mp4");
    write_input(&input);

    let out = remux(&[
        input.to_str().unwrap(),
        output.to_str().unwrap(),
        "--faststart",
    ]);
    assert!(
        out.status.success(),
        "remux failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut reader = Mp4Reader::open(std::fs::read(&output).unwrap()).unwrap();
    let streams = reader.streams();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].codec, CodecId::WebVtt);
    assert_eq!(streams[0].media_type, MediaType::Subtitle);
    let mut texts = Vec::new();
    while let Some(p) = reader.read_packet().unwrap() {
        texts.push(p.data);
    }
    assert_eq!(texts, CUES.iter().map(|t| cue(t)).collect::<Vec<_>>());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remux_to_webm_skips_the_subtitle_track() {
    let dir = std::env::temp_dir().join(format!("tpt-webvtt-webm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("in.mp4");
    let output = dir.join("out.webm");
    write_input(&input);

    let out = remux(&[input.to_str().unwrap(), output.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("skipping stream 0"), "stderr: {stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}
