//! A container that omits the codec configuration record (`av1C`, `vpcC`) still
//! demuxes: the record is built from the stream's first key frame. The fixtures are
//! real ffmpeg output with the record hidden (renamed to a `free` box in MP4, its
//! element ID changed to an unknown one in WebM, so every size stays valid).
//! Needs ffmpeg with libaom-av1 and libvpx-vp9; skipped otherwise.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use tpt_kinetix_core::codec::CodecId;
use tpt_kinetix_demux::{Demuxer, MkvReader, Mp4Reader};

fn ffmpeg_has(encoder: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(encoder))
        .unwrap_or(false)
}

fn make(dir: &Path, name: &str, codec: &str, container_args: &[&str]) -> PathBuf {
    let out = dir.join(name);
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc=size=160x120:rate=10")
        .args(["-t", "1", "-g", "10", "-c:v", codec, "-b:v", "150k"])
        .args(container_args)
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg could not make {name}");
    out
}

/// Replaces the first occurrence of `from` with `to` (same length), or panics.
fn rename_once(bytes: &mut [u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    assert_eq!(from.len(), to.len());
    let at = bytes
        .windows(from.len())
        .position(|w| w == from)
        .expect("the record is in the fixture");
    bytes[at..at + from.len()].copy_from_slice(to);
    bytes.to_vec()
}

/// The `av1C` / `vpcC` payload of the first video sample entry in `mp4`.
fn record_in(mp4: &[u8], kind: &[u8; 4]) -> Vec<u8> {
    let at = mp4
        .windows(4)
        .position(|w| w == kind)
        .expect("the record is in the fixture");
    // The box header is 4 bytes of size before the type.
    let size = u32::from_be_bytes(mp4[at - 4..at].try_into().unwrap()) as usize;
    mp4[at + 4..at - 4 + size].to_vec()
}

fn video_extradata_mp4(path: &Path) -> (CodecId, Vec<u8>) {
    let reader = Mp4Reader::open(File::open(path).unwrap()).unwrap();
    let s = reader
        .streams()
        .into_iter()
        .find(|s| s.media_type == tpt_kinetix_core::codec::MediaType::Video)
        .unwrap();
    (s.codec, s.extradata)
}

#[test]
fn mp4_av1_without_av1c_gets_one_from_its_key_frame() {
    if !ffmpeg_has("libaom-av1") {
        eprintln!("skipping: no libaom-av1 encoder");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt-cfg-av1-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = make(&dir, "good.mp4", "libaom-av1", &["-cpu-used", "8"]);
    let original = std::fs::read(&good).unwrap();
    let want = record_in(&original, b"av1C");

    let mut hidden = original.clone();
    let hidden_bytes = rename_once(&mut hidden, b"av1C", b"free");
    let hidden_path = dir.join("no-av1c.mp4");
    std::fs::write(&hidden_path, &hidden_bytes).unwrap();

    let (codec, extradata) = video_extradata_mp4(&hidden_path);
    assert_eq!(codec, CodecId::Av1);
    assert_eq!(
        extradata.first(),
        Some(&0x81),
        "built record starts with the av1C marker"
    );
    assert_eq!(extradata, want, "built record matches the one ffmpeg wrote");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mp4_vp9_without_vpcc_gets_one_from_its_key_frame() {
    if !ffmpeg_has("libvpx-vp9") {
        eprintln!("skipping: no libvpx-vp9 encoder");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt-cfg-vp9-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = make(&dir, "good.mp4", "libvpx-vp9", &[]);
    let original = std::fs::read(&good).unwrap();
    let want = record_in(&original, b"vpcC");

    let mut hidden = original.clone();
    let hidden_bytes = rename_once(&mut hidden, b"vpcC", b"free");
    let hidden_path = dir.join("no-vpcc.mp4");
    std::fs::write(&hidden_path, &hidden_bytes).unwrap();

    let (codec, extradata) = video_extradata_mp4(&hidden_path);
    assert_eq!(codec, CodecId::Vp9);
    assert!(!extradata.is_empty(), "a VP9 record was built");
    // The VP9 key frame carries profile, bit depth and subsampling, and those are
    // the fields a `vpcC` needs; the level byte may differ from ffmpeg's guess.
    assert_eq!(
        &extradata[..4],
        &want[..4],
        "profile, level, bit depth and chroma match"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn webm_av1_without_codec_private_gets_one_from_its_key_frame() {
    if !ffmpeg_has("libaom-av1") {
        eprintln!("skipping: no libaom-av1 encoder");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt-cfg-webm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = make(&dir, "good.webm", "libaom-av1", &["-cpu-used", "8"]);
    let original = std::fs::read(&good).unwrap();

    // CodecPrivate (ID 0x63A2) is the element after the "V_AV1" codec ID. Change
    // its ID to an unknown two-byte ID (0x4FFF), which a reader skips.
    let codec_id_at = original
        .windows(5)
        .position(|w| w == b"V_AV1")
        .expect("the codec ID is in the fixture");
    let mut hidden = original.clone();
    let private_at = hidden[codec_id_at..]
        .windows(2)
        .position(|w| w == [0x63, 0xA2])
        .expect("CodecPrivate follows the codec ID")
        + codec_id_at;
    hidden[private_at] = 0x4F;
    hidden[private_at + 1] = 0xFF;
    let hidden_path = dir.join("no-private.webm");
    std::fs::write(&hidden_path, &hidden).unwrap();

    let mut reader = MkvReader::open(File::open(&hidden_path).unwrap()).unwrap();
    let info = reader
        .streams()
        .iter()
        .find(|s| s.codec == CodecId::Av1)
        .expect("the AV1 track is still listed")
        .clone();
    assert_eq!(info.extradata.first(), Some(&0x81));
    let mut packets = 0;
    while let Some(p) = reader.read_packet().unwrap() {
        assert_eq!(p.stream_index, info.index);
        packets += 1;
    }
    assert!(packets >= 5, "the frames still demux, got {packets}");

    let _ = std::fs::remove_dir_all(&dir);
}
