//! Cross-checks `Mp4Reader::streams()` against `ffprobe` on files produced by
//! real encoders. Skipped (with a note) when `ffmpeg`/`ffprobe` are not on
//! `PATH`, so it never breaks a machine without them.

use std::process::Command;

use tpt_kinetix_core::codec::CodecId;
use tpt_kinetix_demux::Mp4Reader;

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// One `ffprobe` stream as `key -> value`.
fn ffprobe_streams(path: &std::path::Path) -> Vec<std::collections::HashMap<String, String>> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name,codec_type,width,height,channels,sample_rate,extradata_size",
            "-of",
            "compact",
        ])
        .arg(path)
        .output()
        .expect("run ffprobe");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("stream|"))
        .map(|l| {
            l.split('|')
                .skip(1)
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .collect()
}

fn codec_for(name: &str) -> CodecId {
    match name {
        "h264" => CodecId::H264,
        "hevc" => CodecId::H265,
        "av1" => CodecId::Av1,
        "vp9" => CodecId::Vp9,
        "aac" => CodecId::Aac,
        "opus" => CodecId::Opus,
        "mp3" => CodecId::Mp3,
        "ac3" => CodecId::Ac3,
        "eac3" => CodecId::Eac3,
        "flac" => CodecId::Flac,
        other => panic!("unmapped ffprobe codec {other}"),
    }
}

fn encode(
    dir: &std::path::Path,
    name: &str,
    vcodec: &[&str],
    acodec: &[&str],
) -> Option<std::path::PathBuf> {
    let path = dir.join(name);
    let status = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=25",
            "-t",
            "1",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "1",
        ])
        .args(["-pix_fmt", "yuv420p"])
        .args(vcodec)
        .args(acodec)
        .arg(&path)
        .status()
        .ok()?;
    status.success().then_some(path)
}

#[test]
fn streams_match_ffprobe_across_codecs() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_real_ffmpeg_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // (file, video encoder args, audio encoder args). Encoders this ffmpeg lacks
    // are skipped individually.
    let cases: &[(&str, &[&str], &[&str])] = &[
        (
            "h264_aac.mp4",
            &["-c:v", "libx264"],
            &["-c:a", "aac", "-ac", "2"],
        ),
        (
            "av1_opus.mp4",
            &["-c:v", "libaom-av1", "-cpu-used", "8"],
            &["-c:a", "libopus", "-ac", "2"],
        ),
        (
            "vp9_opus.mp4",
            &["-c:v", "libvpx-vp9"],
            &["-c:a", "libopus", "-ac", "1"],
        ),
        (
            "hevc_ac3.mp4",
            &["-c:v", "libx265"],
            &["-c:a", "ac3", "-ac", "6"],
        ),
        (
            "h264_mp3.mp4",
            &["-c:v", "libx264"],
            &["-c:a", "libmp3lame", "-ac", "2"],
        ),
        (
            "h264_flac.mp4",
            &["-c:v", "libx264"],
            &["-c:a", "flac", "-strict", "-2", "-ac", "2"],
        ),
        (
            "h264_eac3.mp4",
            &["-c:v", "libx264"],
            &["-c:a", "eac3", "-ac", "2"],
        ),
    ];
    let mut checked = 0;
    for (name, v, a) in cases {
        let Some(path) = encode(&dir, name, v, a) else {
            eprintln!("skipping {name}: encoder unavailable");
            continue;
        };
        let reference = ffprobe_streams(&path);
        let reader = Mp4Reader::open(std::fs::File::open(&path).unwrap()).unwrap();
        let streams = reader.streams();
        assert_eq!(streams.len(), reference.len(), "{name}: stream count");
        for (s, r) in streams.iter().zip(&reference) {
            let ctx = format!("{name} stream {}", s.index);
            assert_eq!(s.codec, codec_for(&r["codec_name"]), "{ctx}: codec");
            let num = |k: &str| r.get(k).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
            if r["codec_type"] == "video" {
                assert_eq!(
                    (s.width, s.height),
                    (num("width"), num("height")),
                    "{ctx}: size"
                );
            } else {
                assert_eq!(u32::from(s.channels), num("channels"), "{ctx}: channels");
                assert_eq!(s.sample_rate, num("sample_rate"), "{ctx}: sample rate");
            }
            // Where ffmpeg's `extradata` is the container's own record
            // (H.264/H.265/AV1 `*C` boxes), the sizes must agree exactly.
            if matches!(s.codec, CodecId::H264 | CodecId::H265 | CodecId::Av1) {
                assert_eq!(
                    s.extradata.len() as u32,
                    num("extradata_size"),
                    "{ctx}: extradata"
                );
            }
            assert!(
                !s.extradata.is_empty() || matches!(s.codec, CodecId::Vp9 | CodecId::Mp3),
                "{ctx}: no codec config extracted"
            );
        }
        // Passthrough is lossless: every packet is readable.
        let mut r = Mp4Reader::open(std::fs::File::open(&path).unwrap()).unwrap();
        let mut n = 0;
        while let Some(p) = tpt_kinetix_demux::Demuxer::read_packet(&mut r).unwrap() {
            assert!(!p.data.is_empty());
            n += 1;
        }
        assert!(n > 0, "{name}: no packets");
        checked += 1;
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(checked > 0, "no ffmpeg encoder was usable");
}
