//! CENC (ClearKey) live packaging, checked against ffmpeg. The same packets go
//! through a clear and a protected [`LivePackager`]; the protected init + segments,
//! concatenated, must decode with `-decryption_key` to the clear frames, and must
//! not decode to them without the key. The playlist, the MPD and the license must
//! carry the protection too. Needs ffmpeg with libvpx-vp9; skipped otherwise.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use tpt_kinetix_core::codec::MediaType;
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_demux::{Demuxer, MkvReader};
use tpt_kinetix_mux::cenc::Scheme;
use tpt_kinetix_mux::Encryption;
use tpt_kinetix_package::{LiveOptions, LivePackager};

const KEY_ID: [u8; 16] = [0x00; 16];

fn key_id() -> [u8; 16] {
    let mut id = KEY_ID;
    id[15] = 0x01;
    id
}

fn key() -> [u8; 16] {
    [0x3c; 16]
}

fn available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A 3-second 10 fps VP9 WebM with a key frame every 10 frames.
fn make_webm(dir: &Path) -> PathBuf {
    let out = dir.join("src.webm");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc=size=160x120:rate=10")
        .args(["-t", "3", "-g", "10", "-c:v", "libvpx-vp9", "-b:v", "200k"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg could not make the test clip");
    out
}

/// The video track and its packets as (pts_ms, key, data).
fn video_packets(path: &Path) -> (StreamInfo, Vec<(i64, bool, Vec<u8>)>) {
    let mut reader = MkvReader::open(File::open(path).unwrap()).unwrap();
    let info = reader
        .streams()
        .iter()
        .find(|s| s.media_type == MediaType::Video)
        .unwrap()
        .clone();
    let mut packets = Vec::new();
    while let Some(p) = reader.read_packet().unwrap() {
        if p.stream_index != info.index {
            continue;
        }
        let (num, den) = p.pts.time_base;
        let ms = p.pts.value * i64::from(num) * 1000 / i64::from(den);
        packets.push((ms, p.is_key_frame, p.data));
    }
    (info, packets)
}

/// Packages `packets` live with `encryption` and returns init + every segment.
fn package(
    info: &StreamInfo,
    packets: &[(i64, bool, Vec<u8>)],
    encryption: Option<Encryption>,
) -> (Vec<u8>, LivePackager) {
    let mut live = LivePackager::new(LiveOptions {
        segment_seconds: 1.0,
        window: 100,
        part_seconds: None,
        encryption,
    });
    live.set_tracks(vec![info.clone()]).unwrap();
    for (ms, key, data) in packets {
        live.push(0, *ms, *key, data.clone(), Some(100)).unwrap();
    }
    live.finish().unwrap();
    let mut bytes = live.init_segment(0).expect("init segment");
    let mut n = 1;
    while let Some(seg) = live.segment(0, n) {
        bytes.extend_from_slice(&seg);
        n += 1;
    }
    assert!(n > 2, "expected several segments, got {}", n - 1);
    (bytes, live)
}

/// The frames ffmpeg decodes from `file` (framemd5 lines), optionally with a key.
fn frames(file: &Path, key: Option<&str>) -> Option<Vec<String>> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error"]);
    if let Some(k) = key {
        cmd.args(["-decryption_key", k]);
    }
    let out = cmd
        .arg("-i")
        .arg(file)
        .args(["-c", "copy", "-f", "framemd5", "-"])
        .output()
        .unwrap();
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(str::to_string)
            .collect(),
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn live_cenc_and_cbcs_decrypt_to_the_clear_frames() {
    if !available("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt-live-cenc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = make_webm(&dir);
    let (info, packets) = video_packets(&src);
    assert!(packets.len() >= 20, "test clip too short");

    let (clear, _) = package(&info, &packets, None);
    let clear_path = dir.join("clear.mp4");
    std::fs::write(&clear_path, &clear).unwrap();
    let clear_frames = frames(&clear_path, None).expect("clear output decodes");
    assert_eq!(clear_frames.len(), packets.len(), "one frame per packet");

    let key_hex = hex(&key());
    for scheme in [Scheme::Cenc, Scheme::Cbcs, Scheme::CbcsPattern] {
        let enc = Encryption {
            key_id: key_id(),
            key: key(),
            scheme,
        };
        let (protected, live) = package(&info, &packets, Some(enc));
        let path = dir.join(format!("protected-{scheme:?}.mp4"));
        std::fs::write(&path, &protected).unwrap();

        let decrypted = frames(&path, Some(&key_hex));
        assert_eq!(
            decrypted.as_deref(),
            Some(clear_frames.as_slice()),
            "{scheme:?}: decrypting the live output must give the clear frames"
        );
        assert_ne!(
            frames(&path, None),
            Some(clear_frames.clone()),
            "{scheme:?}: the protected output must not decode without the key"
        );

        let license = live
            .clearkey_license()
            .expect("a license for protected output");
        assert!(
            license.starts_with("{\"keys\":[{\"kty\":\"oct\",\"kid\":\""),
            "{license}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn live_playlists_and_mpd_signal_the_protection() {
    if !available("ffmpeg") {
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt-live-cenc-sig-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = make_webm(&dir);
    let (info, packets) = video_packets(&src);
    let enc = Encryption {
        key_id: key_id(),
        key: key(),
        scheme: Scheme::Cenc,
    };
    let (_, live) = package(&info, &packets, Some(enc));

    let media = live.media_playlist(0).expect("media playlist");
    assert!(
        media.contains("#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,KEYFORMAT=\"org.w3.clearkey\""),
        "media playlist names the key system:\n{media}"
    );
    let mpd = live.dash_mpd().expect("live MPD");
    assert!(
        mpd.contains("urn:mpeg:dash:mp4protection:2011"),
        "MPD has cenc protection"
    );
    assert!(mpd.contains("ClearKey1.0"), "MPD names ClearKey");

    // The same packets without encryption signal nothing.
    let (_, clear) = package(&info, &packets, None);
    assert!(!clear.media_playlist(0).unwrap().contains("EXT-X-KEY"));
    assert!(!clear.dash_mpd().unwrap().contains("ContentProtection"));
    assert!(clear.clearkey_license().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}
