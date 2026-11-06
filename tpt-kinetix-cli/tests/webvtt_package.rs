//! `tpt-kinetix package` with a WebVTT track: the HLS master gets a SUBTITLES
//! group, `subs.m3u8` lists one `.vtt` per segment, and each segment holds the
//! cues that overlap its window. Needs ffmpeg (with libvpx-vp9 and libopus) to
//! make the A/V source; skipped otherwise.

use std::io::Cursor;
use std::path::Path;
use std::process::Command;

use tpt_kinetix_core::codec::CodecId;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_demux::{Demuxer, Mp4Reader};
use tpt_kinetix_mux::{Mp4Writer, WriterOptions};

/// A second subtitle track (another language), to check numbered renditions.
const CUES_FR: [(&str, i64, i64); 2] = [("Bonjour", 0, 3000), ("Au revoir", 3000, 6000)];

const CUES: [(&str, i64, i64); 3] = [
    ("Hello subtitles", 0, 2500),
    ("Second cue", 2500, 5000),
    ("Third cue", 5000, 6000),
];

fn available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn cue_sample(text: &str) -> Vec<u8> {
    let mut payl = (8 + text.len() as u32).to_be_bytes().to_vec();
    payl.extend_from_slice(b"payl");
    payl.extend_from_slice(text.as_bytes());
    let mut vttc = (8 + payl.len() as u32).to_be_bytes().to_vec();
    vttc.extend_from_slice(b"vttc");
    vttc.extend_from_slice(&payl);
    vttc
}

/// Writes `dir/src.mp4` with the A/V of `av` plus two WebVTT tracks.
fn make_source(av: &Path, dir: &Path) -> std::path::PathBuf {
    let bytes = std::fs::read(av).unwrap();
    let mut reader = Mp4Reader::open(bytes).unwrap();
    let mut streams = reader.streams();
    let mut packets: Vec<Packet> = Vec::new();
    while let Some(p) = reader.read_packet().unwrap() {
        packets.push(p);
    }

    for (cues, lang) in [(&CUES[..], "eng"), (&CUES_FR[..], "fra")] {
        let mut sub = StreamInfo::new(streams.len() as u32, CodecId::WebVtt, 1000);
        sub.language = Some(lang.into());
        sub.extradata = b"WEBVTT
"
        .to_vec();
        let sub_index = sub.index;
        streams.push(sub);
        for &(text, start, _) in cues {
            packets.push(Packet {
                pts: Timestamp::new(start, (1, 1000)),
                dts: Timestamp::new(start, (1, 1000)),
                data: cue_sample(text),
                stream_index: sub_index,
                is_key_frame: true,
            });
        }
    }
    packets.sort_by(|a, b| a.dts.as_secs_f64().total_cmp(&b.dts.as_secs_f64()));

    let mut w =
        Mp4Writer::new(Cursor::new(Vec::new()), &streams, WriterOptions::default()).unwrap();
    for p in &packets {
        w.write_packet(p).unwrap();
    }
    let out = dir.join("src.mp4");
    std::fs::write(&out, w.finish().unwrap().into_inner()).unwrap();
    out
}

#[test]
fn package_writes_the_subtitle_group_and_segments() {
    if !available("ffmpeg") || !available("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt-webvtt-package-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let webm = dir.join("av.webm");
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=25",
            "-t",
            "6",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "6",
        ])
        .args([
            "-pix_fmt",
            "yuv420p",
            "-c:v",
            "libvpx-vp9",
            "-g",
            "25",
            "-b:v",
            "300k",
        ])
        .args(["-c:a", "libopus", "-ac", "2", "-b:a", "64k"])
        .arg(&webm)
        .status()
        .unwrap();
    assert!(status.success());

    let av = dir.join("av.mp4");
    let remux = Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .args(["remux"])
        .arg(&webm)
        .arg(&av)
        .arg("--faststart")
        .status()
        .unwrap();
    assert!(remux.success());

    let src = make_source(&av, &dir);
    let out = dir.join("pkg");
    let status = Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .arg("package")
        .arg(&src)
        .arg(&out)
        .args(["--segment-seconds", "2"])
        .status()
        .unwrap();
    assert!(status.success(), "package failed");

    let master = std::fs::read_to_string(out.join("master.m3u8")).unwrap();
    assert!(
        master.contains("TYPE=SUBTITLES,GROUP-ID=\"subs\""),
        "{master}"
    );
    assert!(master.contains("SUBTITLES=\"subs\""), "{master}");
    // A second track is a second rendition, not a default.
    assert!(
        master.contains("NAME=\"subtitles-1\"") && master.contains("URI=\"subs-1.m3u8\""),
        "{master}"
    );
    // Each rendition names its language from the source's mdhd.
    let rendition = |k: usize| {
        master
            .lines()
            .find(|l| l.contains(&format!("NAME=\"subtitles-{k}\"")))
            .unwrap_or_default()
            .to_string()
    };
    assert!(rendition(0).contains("LANGUAGE=\"eng\""), "{master}");
    assert!(rendition(1).contains("LANGUAGE=\"fra\""), "{master}");
    assert!(
        master.contains("NAME=\"subtitles-0\"") && master.contains("DEFAULT=YES"),
        "{master}"
    );

    let subs = std::fs::read_to_string(out.join("subs-0.m3u8")).unwrap();
    assert!(subs.contains("#EXT-X-ENDLIST"), "{subs}");
    let segments = subs.matches("sub-").count();
    assert!(segments >= 2, "expected several subtitle segments: {subs}");

    // Every cue appears in at least one segment that covers its time.
    let mut all = String::new();
    for n in 1..=segments {
        all.push_str(&std::fs::read_to_string(out.join(format!("sub-0-{n}.vtt"))).unwrap());
    }
    for (text, _, _) in CUES {
        assert!(
            all.contains(text),
            "cue '{text}' is missing from every segment"
        );
    }

    // The first segment starts at 0 and must hold the first cue but not the last.
    let first = std::fs::read_to_string(out.join("sub-0-1.vtt")).unwrap();
    assert!(first.starts_with("WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:0,LOCAL:00:00:00.000"));
    assert!(
        first.contains("00:00:00.000 --> 00:00:02.500\nHello subtitles"),
        "{first}"
    );
    assert!(!first.contains("Third cue"), "{first}");

    // The second track is packaged on the same grid, with its own cues.
    let fr = std::fs::read_to_string(out.join("sub-1-1.vtt")).unwrap();
    assert!(fr.contains("Bonjour"), "{fr}");
    assert!(std::fs::read_to_string(out.join("subs-1.m3u8"))
        .unwrap()
        .contains("sub-1-1.vtt"));

    // DASH: a text adaptation set on the same segment grid.
    let mpd = std::fs::read_to_string(out.join("manifest.mpd")).unwrap();
    assert!(
        mpd.contains("contentType=\"text\" mimeType=\"text/vtt\""),
        "{mpd}"
    );
    assert!(mpd.contains("media=\"sub-0-$Number$.vtt\""), "{mpd}");

    // serve: the same resources, packaged on demand over HTTP.
    let port = 18_000 + (std::process::id() % 2000) as u16;
    let mut server = Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .arg("serve")
        .arg(&src)
        .args(["--port", &port.to_string(), "--segment-seconds", "2"])
        .spawn()
        .unwrap();
    let get = |path: &str| -> String {
        for _ in 0..100 {
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                use std::io::{Read, Write};
                s.write_all(
                    format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .unwrap();
                let mut body = String::new();
                s.read_to_string(&mut body).unwrap();
                return body;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("serve did not start");
    };
    let playlist = get("/subs-0.m3u8");
    let vtt = get("/sub-0-1.vtt");
    let _ = server.kill();
    let _ = server.wait();
    assert!(playlist.contains("sub-0-1.vtt"), "{playlist}");
    assert!(vtt.contains("Hello subtitles"), "{vtt}");

    let _ = std::fs::remove_dir_all(&dir);
}
