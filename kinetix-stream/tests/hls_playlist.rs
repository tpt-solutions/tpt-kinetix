use std::path::PathBuf;

use kinetix_stream::hls::playlist::HlsPlaylist;
use kinetix_stream::hls::segment::HlsSegment;

fn make_segment(index: u64) -> HlsSegment {
    HlsSegment {
        index,
        duration_secs: 6.0,
        path: PathBuf::from(HlsSegment::filename(index)),
        byte_range: None,
    }
}

#[test]
fn test_playlist_render() {
    let mut playlist = HlsPlaylist::new(6, 5);
    for i in 0..3 {
        playlist.add_segment(make_segment(i));
    }

    let rendered = playlist.render();

    assert!(rendered.contains("#EXTM3U"), "missing #EXTM3U");
    assert!(
        rendered.contains("#EXT-X-TARGETDURATION:6"),
        "missing #EXT-X-TARGETDURATION"
    );
    assert!(
        rendered.contains("segment00000.ts"),
        "missing segment00000.ts"
    );
    assert!(
        rendered.contains("segment00001.ts"),
        "missing segment00001.ts"
    );
    assert!(
        rendered.contains("segment00002.ts"),
        "missing segment00002.ts"
    );
    assert!(!rendered.contains("#EXT-X-ENDLIST"), "unexpected #EXT-X-ENDLIST");
}

#[test]
fn test_playlist_window_sliding() {
    let window = 3usize;
    let mut playlist = HlsPlaylist::new(6, window);

    // Add window_size + 2 segments
    for i in 0..(window + 2) as u64 {
        playlist.add_segment(make_segment(i));
    }

    // media_sequence should have incremented by 2
    assert_eq!(
        playlist.media_sequence,
        2,
        "media_sequence should be 2 after 2 evictions"
    );
    // Only `window` segments should remain
    assert_eq!(
        playlist.segments.len(),
        window,
        "playlist should contain exactly window_size segments"
    );
}

#[test]
fn test_playlist_endlist() {
    let mut playlist = HlsPlaylist::new(6, 5);
    playlist.add_segment(make_segment(0));
    playlist.ended = true;

    let rendered = playlist.render();
    assert!(
        rendered.contains("#EXT-X-ENDLIST"),
        "missing #EXT-X-ENDLIST when ended=true"
    );
}
