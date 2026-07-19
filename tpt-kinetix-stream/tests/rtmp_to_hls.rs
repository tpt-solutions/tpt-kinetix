//! End-to-end live streaming test: RTMP ingest → HLS packaging → playable output.
//!
//! This wires the [`RtmpServer`] ingest path to the [`HlsPackager`] by bridging
//! depacketized FLV video tags into MPEG-TS HLS segments, then (when `ffmpeg`
//! is available) verifies the generated `.m3u8` + `.ts` output is remuxable by
//! a real player-grade decoder.
//!
//! The test is skipped automatically on hosts without `ffmpeg`.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use tpt_kinetix_stream::{
    hls::server::{HlsConfig, HlsPackager},
    rtmp::{
        flv::FlvVideoCodec,
        server::{RtmpConfig, RtmpMediaEvent, RtmpServer},
    },
};
use tpt_kinetix_test_utils::reference::ffmpeg_available;

#[tokio::test]
async fn rtmp_ingest_to_hls_playlist_and_segments() {
    // Bind to an ephemeral local port to avoid clashing with a running service.
    let rtmp_addr = "127.0.0.1:19351";
    let hls_addr = "127.0.0.1:18081";
    let out_dir = std::env::temp_dir().join(format!("kinetix_hls_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).unwrap();

    // Shared counter of depacketized coded video tags we forward into HLS.
    let video_tags = Arc::new(AtomicUsize::new(0));

    let packager = Arc::new(std::sync::Mutex::new(HlsPackager::new(HlsConfig {
        segment_duration_secs: 2,
        output_dir: out_dir.to_string_lossy().into_owned(),
        window_size: 5,
        http_bind_addr: hls_addr.to_string(),
    })));

    // Start the RTMP ingest server with a bridge handler that muxes every coded
    // FLV video tag into the next HLS TS segment.
    let rtmp = RtmpServer::new(RtmpConfig {
        bind_addr: rtmp_addr.to_string(),
    })
    .with_handler({
        let video_tags = video_tags.clone();
        let packager = packager.clone();
        // Retain the most recent AVCDecoderConfigurationRecord (SPS/PPS) so every
        // emitted segment is independently decodable (HLS segments are random-
        // access points and must carry their own parameter sets).
        let sps_pps: Arc<std::sync::Mutex<Option<Vec<Vec<u8>>>>> =
            Arc::new(std::sync::Mutex::new(None));
        move |event| {
            if let RtmpMediaEvent::Video { tag, .. } = event {
                if !matches!(tag.codec, FlvVideoCodec::Avc) {
                    return;
                }
                if tag.is_sequence_header() {
                    // `tag.data` is an AVCDecoderConfigurationRecord, not raw
                    // AVCC NALUs. Expand it into SPS/PPS AVCC access units so the
                    // muxer can emit them as in-band parameter sets.
                    *sps_pps.lock().unwrap() = Some(avcc_config_record_to_aus(&tag.data));
                    return;
                }
                // `tag.data` is AVCC (4-byte length-prefixed NALUs) — exactly
                // what `HlsPackager::write_ts_segment` expects. Prepend the
                // parameter sets so the segment is self-contained.
                video_tags.fetch_add(1, Ordering::SeqCst);
                let idx = video_tags.load(Ordering::SeqCst);
                let mut aus = Vec::new();
                if let Some(cfg) = sps_pps.lock().unwrap().clone() {
                    for au in cfg {
                        aus.push((au, 0, false));
                    }
                }
                aus.push((
                    tag.data.clone(),
                    idx as u64 * 3000,
                    tag.frame_type.is_keyframe(),
                ));
                let mut guard = packager.lock().unwrap();
                if let Err(e) = guard.write_ts_segment(&aus) {
                    eprintln!("write_ts_segment failed: {e}");
                }
            }
        }
    });

    let rtmp_task = tokio::spawn(async move { rtmp.run().await });

    // Give the server a moment to bind.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Use ffmpeg to publish a synthetic RTMP stream to our ingest server and let
    // it run for a couple of seconds, generating live segments.
    if !ffmpeg_available() {
        eprintln!("skipping rtmp->hls e2e: ffmpeg not installed");
        rtmp_task.abort();
        let _ = std::fs::remove_dir_all(&out_dir);
        return;
    }

    let publish = tokio::process::Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=128x96:rate=15:duration=3",
            "-c:v",
            "libx264",
            "-profile:v",
            "baseline",
            "-pix_fmt",
            "yuv420p",
            "-preset",
            "ultrafast",
            "-f",
            "flv",
            &format!("rtmp://{rtmp_addr}/live/stream"),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    match publish {
        Ok(mut child) => {
            let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
        }
        Err(e) => {
            eprintln!("skipping rtmp->hls e2e: cannot launch ffmpeg: {e}");
            rtmp_task.abort();
            let _ = std::fs::remove_dir_all(&out_dir);
            return;
        }
    }

    // The bridge should have produced at least one coded video tag/segment.
    assert!(
        video_tags.load(Ordering::SeqCst) > 0,
        "expected at least one depacketized FLV video tag from the RTMP push"
    );

    // Verify the playlist file exists and references at least one .ts segment.
    let playlist_path = out_dir.join("playlist.m3u8");
    let playlist = std::fs::read_to_string(&playlist_path)
        .expect("HLS playlist.m3u8 should have been written");
    assert!(
        playlist.contains("#EXTM3U"),
        "playlist missing #EXTM3U header"
    );
    let ts_refs = playlist.matches(".ts").count();
    assert!(
        ts_refs > 0,
        "playlist should reference at least one .ts segment"
    );

    // Verify at least one segment is a standards-compliant TS file (188-byte
    // packets starting with the 0x47 sync byte) by remuxing it with ffmpeg.
    let first_seg = out_dir.join("segment00000.ts");
    let seg_bytes = std::fs::read(&first_seg).expect("first segment file should exist");
    assert_eq!(seg_bytes.len() % 188, 0, "segment must be whole TS packets");
    assert_eq!(seg_bytes[0], 0x47, "segment must start with TS sync byte");

    // Real-player-grade validation: ffmpeg must be able to remux the segment
    // without error (proves it is well-formed enough for playback).
    let remux_out = std::process::Command::new("ffmpeg")
        .args([
            "-loglevel",
            "warning",
            "-i",
            first_seg.to_str().unwrap(),
            "-c",
            "copy",
            "-f",
            "null",
            "-",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();
    match &remux_out {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            eprintln!(
                "ffmpeg remux failed:\n{}",
                String::from_utf8_lossy(&o.stderr)
            );
            std::fs::write(
                "D:/Programming/1PRODUCTION/Open Source/tpt-kinetix/debug_seg.ts",
                &seg_bytes,
            )
            .ok();
        }
        Err(e) => eprintln!("could not run ffmpeg: {e}"),
    }
    assert!(
        matches!(remux_out, Ok(ref o) if o.status.success()),
        "ffmpeg failed to remux the generated HLS segment"
    );

    rtmp_task.abort();
    let _ = std::fs::remove_dir_all(&out_dir);
}

/// Parse an `AVCDecoderConfigurationRecord` (as carried in an FLV/HLS AVCC
/// sequence header) into a list of AVCC access units, one per SPS/PPS NALU.
///
/// Each returned `Vec<u8>` is a 4-byte big-endian length prefix followed by the
/// raw NALU — exactly the form [`HlsPackager::write_ts_segment`] expects.
fn avcc_config_record_to_aus(record: &[u8]) -> Vec<Vec<u8>> {
    let mut aus = Vec::new();
    // Need: version(1) + profile(1) + compat(1) + level(1) + lengthSize(1)
    //        + numSPS(1) + ...
    if record.len() < 6 {
        return aus;
    }
    let num_sps = record[5] & 0x1F;
    let mut pos = 6usize;
    for _ in 0..num_sps {
        if pos + 2 > record.len() {
            break;
        }
        let sps_len = u16::from_be_bytes([record[pos], record[pos + 1]]) as usize;
        pos += 2;
        if pos + sps_len > record.len() {
            break;
        }
        let mut au = Vec::with_capacity(sps_len + 4);
        au.extend_from_slice(&(sps_len as u32).to_be_bytes());
        au.extend_from_slice(&record[pos..pos + sps_len]);
        aus.push(au);
        pos += sps_len;
    }
    if pos >= record.len() {
        return aus;
    }
    let num_pps = record[pos];
    pos += 1;
    for _ in 0..num_pps {
        if pos + 2 > record.len() {
            break;
        }
        let pps_len = u16::from_be_bytes([record[pos], record[pos + 1]]) as usize;
        pos += 2;
        if pos + pps_len > record.len() {
            break;
        }
        let mut au = Vec::with_capacity(pps_len + 4);
        au.extend_from_slice(&(pps_len as u32).to_be_bytes());
        au.extend_from_slice(&record[pos..pos + pps_len]);
        aus.push(au);
        pos += pps_len;
    }
    aus
}
