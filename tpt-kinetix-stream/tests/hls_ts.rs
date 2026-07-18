//! Integration test: mux H.264 access units into a TS segment through the
//! HLS packager and verify the on-disk segment + playlist.

use tpt_kinetix_stream::hls::server::{HlsConfig, HlsPackager};

#[test]
fn packager_writes_ts_segment_and_updates_playlist() {
    let tmp = std::env::temp_dir().join(format!("kinetix_hls_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);

    let config = HlsConfig {
        segment_duration_secs: 2,
        output_dir: tmp.to_string_lossy().to_string(),
        window_size: 5,
        http_bind_addr: "127.0.0.1:0".into(),
    };
    let mut packager = HlsPackager::new(config);

    // Two access units: an IDR then a P-frame (AVCC length-prefixed).
    let idr = vec![0u8, 0, 0, 4, 0x65, 0xAA, 0xBB, 0xCC];
    let p = vec![0u8, 0, 0, 3, 0x41, 0xDD, 0xEE];

    let written = packager
        .write_ts_segment(&[(idr, 0, true), (p, 3000, false)])
        .expect("ts segment write should succeed");

    assert!(written > 0);
    assert_eq!(
        written % 188,
        0,
        "TS segment must be whole 188-byte packets"
    );

    // Segment file exists and is a valid TS stream.
    let seg_path = tmp.join("segment00000.ts");
    let seg = std::fs::read(&seg_path).expect("segment file should exist");
    assert_eq!(seg.len(), written);
    assert_eq!(seg[0], 0x47, "TS sync byte");

    // Playlist references the segment.
    let m3u8 = std::fs::read_to_string(tmp.join("playlist.m3u8")).unwrap();
    assert!(m3u8.contains("#EXTM3U"));
    assert!(m3u8.contains("segment00000.ts"));

    let _ = std::fs::remove_dir_all(&tmp);
}
