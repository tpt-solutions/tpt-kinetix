//! `Mp4Writer` / `faststart` round-trips through the real demuxer, plus
//! interoperability with ffmpeg/ffprobe when they are installed.

use std::io::Cursor;
use std::process::Command;

use proptest::prelude::*;
use tpt_kinetix_core::codec::CodecId;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_demux::{Demuxer, Mp4Reader};
use tpt_kinetix_mux::{faststart, Mp4Writer, MuxError, WriterOptions};

fn video_stream() -> StreamInfo {
    let mut s = StreamInfo::new(0, CodecId::H264, 30_000);
    s.width = 640;
    s.height = 360;
    s.extradata = vec![
        1, 0x64, 0, 0x1F, 0xFF, 0xE1, 0, 2, 0x67, 0x64, 1, 0, 2, 0x68, 0xEE,
    ];
    s
}

fn audio_stream() -> StreamInfo {
    let mut s = StreamInfo::new(1, CodecId::Aac, 48_000);
    s.channels = 2;
    s.sample_rate = 48_000;
    s.bits_per_sample = 16;
    s.extradata = vec![0x11, 0x90];
    s.edit_media_time = Some(1024);
    s
}

fn pkt(stream: u32, ts: u32, dts: i64, cts: i64, key: bool, data: Vec<u8>) -> Packet {
    Packet {
        pts: Timestamp::new(dts + cts, (1, ts)),
        dts: Timestamp::new(dts, (1, ts)),
        data,
        stream_index: stream,
        is_key_frame: key,
    }
}

/// 2 s of "video" (B-frame style composition offsets) interleaved with "audio".
fn sample_packets() -> Vec<Packet> {
    let mut v = Vec::new();
    for i in 0..60i64 {
        v.push(pkt(
            0,
            30_000,
            i * 1000,
            if i % 3 == 0 { 2000 } else { 0 },
            i % 15 == 0,
            vec![i as u8; 200 + (i as usize % 7) * 50],
        ));
        for j in 0..2i64 {
            let a = i * 2 + j;
            v.push(pkt(
                1,
                48_000,
                a * 1024,
                0,
                true,
                vec![0xA0 + (a % 16) as u8; 90 + (a as usize % 5) * 10],
            ));
        }
    }
    v
}

fn write(streams: &[StreamInfo], packets: &[Packet], opts: WriterOptions) -> Vec<u8> {
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), streams, opts).unwrap();
    for p in packets {
        w.write_packet(p).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn read_all(file: Vec<u8>) -> (Vec<StreamInfo>, Vec<Packet>) {
    let mut r = Mp4Reader::open(file).unwrap();
    let streams = r.streams();
    let mut out = Vec::new();
    while let Some(p) = r.read_packet().unwrap() {
        out.push(p);
    }
    (streams, out)
}

fn per_stream(packets: &[Packet], s: u32) -> Vec<&Packet> {
    packets.iter().filter(|p| p.stream_index == s).collect()
}

fn assert_same_packets(want: &[Packet], got: &[Packet]) {
    assert_eq!(want.len(), got.len());
    for s in 0..2 {
        let (w, g) = (per_stream(want, s), per_stream(got, s));
        assert_eq!(w.len(), g.len(), "stream {s} packet count");
        for (a, b) in w.iter().zip(&g) {
            assert_eq!(a.data, b.data);
            assert_eq!(
                (a.pts.value, a.dts.value, a.is_key_frame),
                (b.pts.value, b.dts.value, b.is_key_frame)
            );
        }
    }
}

#[test]
fn round_trips_streams_packets_and_timing() {
    let packets = sample_packets();
    // Tiny chunks force many chunk offsets and stsc runs.
    let opts = WriterOptions {
        chunk_duration_ms: 100,
        ..Default::default()
    };
    let file = write(&[video_stream(), audio_stream()], &packets, opts);
    let (streams, got) = read_all(file);

    assert_eq!(streams.len(), 2);
    assert_eq!(streams[0].codec, CodecId::H264);
    assert_eq!((streams[0].width, streams[0].height), (640, 360));
    assert_eq!(streams[0].extradata, video_stream().extradata);
    assert_eq!(streams[1].codec, CodecId::Aac);
    assert_eq!((streams[1].channels, streams[1].sample_rate), (2, 48_000));
    assert_eq!(streams[1].extradata, [0x11, 0x90]);
    // Edit lists survive: AAC priming is kept, video gets its composition delay.
    assert_eq!(streams[1].edit_media_time, Some(1024));
    assert_eq!(streams[0].edit_media_time, Some(2000));
    assert_same_packets(&packets, &got);
}

#[test]
fn output_is_time_interleaved_in_chunks() {
    let file = write(
        &[video_stream(), audio_stream()],
        &sample_packets(),
        WriterOptions::default(),
    );
    let (_, got) = read_all(file);
    // Reading yields time-ordered packets from both streams.
    let us = |p: &Packet| p.dts.value as i128 * 1_000_000 / p.dts.time_base.1 as i128;
    assert!(got.windows(2).all(|w| us(&w[0]) <= us(&w[1])));
    assert!(got.iter().any(|p| p.stream_index == 0) && got.iter().any(|p| p.stream_index == 1));
}

#[test]
fn co64_offsets_are_readable() {
    let packets = sample_packets();
    let file = write(
        &[video_stream(), audio_stream()],
        &packets,
        WriterOptions {
            force_co64: true,
            ..Default::default()
        },
    );
    assert!(file.windows(4).any(|w| w == b"co64"));
    assert!(!file.windows(4).any(|w| w == b"stco"));
    let (_, got) = read_all(file);
    assert_same_packets(&packets, &got);
}

#[test]
fn last_sample_duration_hint_is_kept() {
    let mut w = Mp4Writer::new(
        Cursor::new(Vec::new()),
        &[audio_stream()],
        WriterOptions::default(),
    )
    .unwrap();
    for i in 0..5i64 {
        let d = if i == 4 { Some(312) } else { Some(1024) };
        w.write_packet_with_duration(&pkt(0, 48_000, i * 1024, 0, true, vec![1; 20]), d)
            .unwrap();
    }
    let file = w.finish().unwrap().into_inner();
    let mut r = Mp4Reader::open(file).unwrap();
    let mut durs = Vec::new();
    while let Some((_, d)) = r.read_packet_timed().unwrap() {
        durs.push(d);
    }
    assert_eq!(durs, [1024, 1024, 1024, 1024, 312]);
}

#[test]
fn faststart_moves_moov_front_and_preserves_everything() {
    let packets = sample_packets();
    let file = write(
        &[video_stream(), audio_stream()],
        &packets,
        WriterOptions {
            chunk_duration_ms: 100,
            ..Default::default()
        },
    );
    assert!(
        top_level(&file).iter().position(|k| k == "moov")
            > top_level(&file).iter().position(|k| k == "mdat")
    );

    let mut out = Vec::new();
    let report = faststart(&mut Cursor::new(&file), &mut out).unwrap();
    assert!(!report.already_faststart);
    assert_eq!(report.bytes_written as usize, out.len());
    assert_eq!(out.len(), file.len());
    let order = top_level(&out);
    assert!(
        order.iter().position(|k| k == "moov") < order.iter().position(|k| k == "mdat"),
        "{order:?}"
    );
    let (_, got) = read_all(out.clone());
    assert_same_packets(&packets, &got);

    // Idempotent.
    let mut again = Vec::new();
    let r2 = faststart(&mut Cursor::new(&out), &mut again).unwrap();
    assert!(r2.already_faststart);
    assert_eq!(again, out);
}

fn top_level(file: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= file.len() {
        let size32 = u32::from_be_bytes(file[pos..pos + 4].try_into().unwrap()) as usize;
        let kind = String::from_utf8_lossy(&file[pos + 4..pos + 8]).to_string();
        let size = if size32 == 1 {
            u64::from_be_bytes(file[pos + 8..pos + 16].try_into().unwrap()) as usize
        } else {
            size32
        };
        out.push(kind);
        if size < 8 {
            break;
        }
        pos += size;
    }
    out
}

#[test]
fn faststart_rejects_garbage() {
    let mut out = Vec::new();
    assert!(faststart(&mut Cursor::new(vec![0u8; 5]), &mut out).is_err());
    assert!(faststart(&mut Cursor::new(Vec::<u8>::new()), &mut out).is_err());
    // A box claiming to be bigger than the file.
    let mut bad = vec![0, 0, 0xFF, 0xFF];
    bad.extend(b"moov");
    bad.extend([0u8; 8]);
    assert!(faststart(&mut Cursor::new(bad), &mut out).is_err());
}

#[test]
fn writer_rejects_bad_input() {
    let opts = WriterOptions::default;
    assert!(matches!(
        Mp4Writer::new(Cursor::new(Vec::new()), &[], opts()),
        Err(MuxError::InvalidConfig(_))
    ));
    // No codec config for H.264.
    let bare = StreamInfo::new(0, CodecId::H264, 1000);
    assert!(Mp4Writer::new(Cursor::new(Vec::new()), &[bare], opts()).is_err());
    // Unknown codec.
    let unk = StreamInfo::new(0, CodecId::Unknown(*b"zzzz"), 1000);
    assert!(matches!(
        Mp4Writer::new(Cursor::new(Vec::new()), &[unk], opts()),
        Err(MuxError::Unsupported(_))
    ));
    // Zero timescale.
    let mut zero = video_stream();
    zero.timescale = 0;
    assert!(Mp4Writer::new(Cursor::new(Vec::new()), &[zero], opts()).is_err());

    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), &[video_stream()], opts()).unwrap();
    // Unknown stream.
    assert!(w
        .write_packet(&pkt(3, 30_000, 0, 0, true, vec![1]))
        .is_err());
    w.write_packet(&pkt(0, 30_000, 1000, 0, true, vec![1]))
        .unwrap();
    // DTS going backwards.
    assert!(matches!(
        w.write_packet(&pkt(0, 30_000, 500, 0, false, vec![1])),
        Err(MuxError::InvalidTimestamps(_))
    ));
}

#[test]
fn rescales_packets_in_a_different_time_base() {
    // Source timestamps in 1/90000 are rescaled to the stream's 1/30000.
    let mut w = Mp4Writer::new(
        Cursor::new(Vec::new()),
        &[video_stream()],
        WriterOptions::default(),
    )
    .unwrap();
    for i in 0..10i64 {
        let p = Packet {
            pts: Timestamp::new(i * 3000, (1, 90_000)),
            dts: Timestamp::new(i * 3000, (1, 90_000)),
            data: vec![i as u8; 30],
            stream_index: 0,
            is_key_frame: i == 0,
        };
        w.write_packet(&p).unwrap();
    }
    let (_, got) = read_all(w.finish().unwrap().into_inner());
    assert_eq!(got.len(), 10);
    assert!(got
        .iter()
        .enumerate()
        .all(|(i, p)| p.dts.value == i as i64 * 1000 && p.dts.time_base == (1, 30_000)));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(120))]

    /// Any plausible packet sequence round-trips exactly.
    #[test]
    fn random_sequences_round_trip(
        n in 1usize..120,
        seed in any::<u64>(),
        chunk_ms in 1u32..800,
        with_audio in any::<bool>(),
    ) {
        let mut st = seed | 1;
        let mut next = || { st ^= st << 13; st ^= st >> 7; st ^= st << 17; st };
        let mut packets = Vec::new();
        let (mut vd, mut ad) = (0i64, 0i64);
        for i in 0..n {
            let size = 1 + (next() % 3000) as usize;
            let cts = (next() % 4) as i64 * 500;
            packets.push(pkt(0, 30_000, vd, cts, i % 12 == 0 || next() % 9 == 0, vec![(next() & 0xFF) as u8; size]));
            vd += 500 + (next() % 1000) as i64;
            if with_audio {
                for _ in 0..(next() % 3) {
                    let size = 1 + (next() % 400) as usize;
                    packets.push(pkt(1, 48_000, ad, 0, true, vec![(next() & 0xFF) as u8; size]));
                    ad += 1024;
                }
            }
        }
        let streams: Vec<StreamInfo> = if with_audio { vec![video_stream(), audio_stream()] } else { vec![video_stream()] };
        let file = write(&streams, &packets, WriterOptions { chunk_duration_ms: chunk_ms, ..Default::default() });
        let mut r = Mp4Reader::open(file).unwrap();
        let mut got = Vec::new();
        while let Some(p) = r.read_packet().unwrap() { got.push(p); }
        for s in 0..streams.len() as u32 {
            let (w, g) = (per_stream(&packets, s), per_stream(&got, s));
            prop_assert_eq!(w.len(), g.len());
            for (a, b) in w.iter().zip(&g) {
                prop_assert_eq!(&a.data, &b.data);
                prop_assert_eq!((a.pts.value, a.dts.value, a.is_key_frame), (b.pts.value, b.dts.value, b.is_key_frame));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ffmpeg interoperability
// ---------------------------------------------------------------------------

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ffprobe_packets(path: &std::path::Path) -> Vec<String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "packet=stream_index,pts,dts,duration,size,flags",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    let mut v: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    v.sort();
    v
}

/// Remux ffmpeg-made files with `Mp4Reader` -> `Mp4Writer` and check that ffmpeg
/// decodes them cleanly and that every packet (stream, pts, dts, duration, size,
/// flags) is identical to the source's.
#[test]
fn remuxed_files_decode_in_ffmpeg_and_keep_every_packet() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_remux_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cases: &[(&str, &[&str], &[&str])] = &[
        (
            "h264_aac",
            &["-c:v", "libx264", "-bf", "2"],
            &["-c:a", "aac", "-ac", "2"],
        ),
        (
            "av1_opus",
            &["-c:v", "libaom-av1", "-cpu-used", "8"],
            &["-c:a", "libopus", "-ac", "2"],
        ),
        (
            "hevc_ac3",
            &["-c:v", "libx265"],
            &["-c:a", "ac3", "-ac", "6"],
        ),
        ("h264_mp3", &["-c:v", "libx264"], &["-c:a", "libmp3lame"]),
        (
            "vp9_flac",
            &["-c:v", "libvpx-vp9"],
            &["-c:a", "flac", "-strict", "-2"],
        ),
    ];
    let mut checked = 0;
    for (name, v, a) in cases {
        let src = dir.join(format!("{name}.mp4"));
        let ok = Command::new("ffmpeg")
            .args([
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x240:rate=25",
                "-t",
                "2",
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "2",
                "-pix_fmt",
                "yuv420p",
            ])
            .args(*v)
            .args(*a)
            .arg(&src)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("skipping {name}: encoder unavailable");
            continue;
        }
        let mut reader = Mp4Reader::open(std::fs::File::open(&src).unwrap()).unwrap();
        let streams = reader.streams();
        let dst = dir.join(format!("{name}_out.mp4"));
        let mut w = Mp4Writer::new(
            std::fs::File::create(&dst).unwrap(),
            &streams,
            WriterOptions::default(),
        )
        .unwrap();
        while let Some((p, d)) = reader.read_packet_timed().unwrap() {
            w.write_packet_with_duration(&p, Some(d)).unwrap();
        }
        w.finish().unwrap();

        let decode = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&dst)
            .args(["-f", "null", "-"])
            .output()
            .unwrap();
        assert!(
            decode.stderr.is_empty(),
            "{name}: ffmpeg decode errors: {}",
            String::from_utf8_lossy(&decode.stderr)
        );
        assert_eq!(
            ffprobe_packets(&src),
            ffprobe_packets(&dst),
            "{name}: packets differ"
        );
        checked += 1;
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(checked > 0, "no ffmpeg encoder was usable");
}
