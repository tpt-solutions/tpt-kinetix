//! Tests for the streaming [`Mp4Reader`]: it must open files without loading
//! them, interleave tracks by decode time, apply `ctts`, scale linearly, and
//! never panic or over-allocate on hostile input.

use proptest::prelude::*;
use tpt_kinetix_demux::{CountingSource, Demuxer, Mp4Reader, ReadAt};

mod common;
use common::*;

fn drain<S: ReadAt>(r: &mut Mp4Reader<S>) -> Vec<tpt_kinetix_core::packet::Packet> {
    let mut v = Vec::new();
    while let Some(p) = r.read_packet().unwrap() {
        v.push(p);
    }
    v
}

// ---------------------------------------------------------------------------
// Behaviour
// ---------------------------------------------------------------------------

#[test]
fn interleaves_tracks_by_decode_time_and_applies_ctts() {
    let v = video_samples(30, 100);
    let a = audio_samples(40);
    for moov_first in [false, true] {
        let file = build_mp4(
            &[
                Track::video(1000, v.clone()),
                Track::audio(48_000, a.clone()),
            ],
            moov_first,
        );
        let mut r = Mp4Reader::open(file).unwrap();
        assert_eq!(r.tracks().len(), 2);
        let pk = drain(&mut r);
        assert_eq!(pk.len(), 70);

        // Non-decreasing decode time in microseconds.
        let us = |p: &tpt_kinetix_core::packet::Packet| {
            p.dts.value as i128 * 1_000_000 / p.dts.time_base.1 as i128
        };
        assert!(
            pk.windows(2).all(|w| us(&w[0]) <= us(&w[1])),
            "not time-ordered"
        );
        // Both tracks present and interleaved (not all video then all audio).
        let first_audio = pk.iter().position(|p| p.stream_index == 1).unwrap();
        let last_video = pk.iter().rposition(|p| p.stream_index == 0).unwrap();
        assert!(first_audio < last_video);

        // Per-track content, order, ctts and key flags.
        let vp: Vec<_> = pk.iter().filter(|p| p.stream_index == 0).collect();
        for (i, p) in vp.iter().enumerate() {
            assert_eq!(p.data, v[i].data);
            assert_eq!(p.dts.value, 40 * i as i64);
            assert_eq!(p.pts.value, 40 * i as i64 + v[i].cts as i64);
            assert_eq!(p.is_key_frame, v[i].key);
        }
        let ap: Vec<_> = pk.iter().filter(|p| p.stream_index == 1).collect();
        for (i, p) in ap.iter().enumerate() {
            assert_eq!(p.data, a[i].data);
            assert_eq!(p.dts.value, 1024 * i as i64);
            assert!(p.is_key_frame);
        }
    }
}

#[test]
fn open_reads_only_headers_and_the_moov_index() {
    // ~21 MB of mdat, index at the end.
    let file = build_mp4(&[Track::video(1000, video_samples(40, 512 * 1024))], false);
    let total = file.len() as u64;
    assert!(total > 20_000_000);
    let src = CountingSource::new(file);
    let r = Mp4Reader::open(src).unwrap();
    // Take the source back to inspect the counters.
    let src = r.into_source();
    assert!(
        src.bytes() < 4096,
        "open() read {} bytes of a {total}-byte file",
        src.bytes()
    );
    assert!(src.calls() <= 8, "open() made {} reads", src.calls());

    // Reading one packet costs exactly that packet's bytes.
    let before = src.bytes();
    let mut r = Mp4Reader::open(src).unwrap();
    let p = r.read_packet().unwrap().unwrap();
    assert_eq!(p.data.len(), 512 * 1024);
    let src = r.into_source();
    let delta = src.bytes() - before;
    assert!(delta < 512 * 1024 + 4096, "first packet cost {delta} bytes");
}

#[test]
fn faststart_files_need_a_single_header_read() {
    let file = build_mp4(&[Track::video(1000, video_samples(40, 64 * 1024))], true);
    let src = CountingSource::new(file);
    let r = Mp4Reader::open(src).unwrap();
    let src = r.into_source();
    // 16-byte header probe(s) + the moov payload.
    assert!(src.calls() <= 4, "{} reads", src.calls());
}

#[test]
fn works_over_a_real_file() {
    let file = build_mp4(
        &[
            Track::video(1000, video_samples(20, 300)),
            Track::audio(48_000, audio_samples(30)),
        ],
        false,
    );
    let dir = std::env::temp_dir().join(format!("tpt_mp4reader_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.mp4");
    std::fs::write(&path, &file).unwrap();
    let mut from_file = Mp4Reader::open(std::fs::File::open(&path).unwrap()).unwrap();
    let mut from_mem = Mp4Reader::open(file).unwrap();
    let a = drain(&mut from_file);
    let b = drain(&mut from_mem);
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(
            (x.pts, x.dts, x.stream_index, &x.data),
            (y.pts, y.dts, y.stream_index, &y.data)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn seek_snaps_back_to_a_key_frame() {
    let v = video_samples(60, 50); // key every 12th sample, 40 ms apart
    let mut r = Mp4Reader::open(build_mp4(&[Track::video(1000, v)], false)).unwrap();
    r.seek(1_300).unwrap(); // sample 32 (1280 ms) -> key at 24 (960 ms)
    let p = r.read_packet().unwrap().unwrap();
    assert!(p.is_key_frame);
    assert_eq!(p.dts.value, 24 * 40);
    r.seek(0).unwrap();
    assert_eq!(r.read_packet().unwrap().unwrap().dts.value, 0);
    r.seek(10_000_000).unwrap(); // far past the end -> last key frame
    assert_eq!(r.read_packet().unwrap().unwrap().dts.value, 48 * 40);
}

#[test]
fn indexing_is_linear_not_quadratic() {
    // The previous demuxer rescanned stsc/stss per sample: O(n^2). 300k samples
    // would take minutes; the flat index does it in well under a second.
    let n = 300_000;
    let samples: Vec<Sample> = (0..n)
        .map(|i| Sample {
            data: vec![i as u8],
            delta: 1,
            cts: 0,
            key: i % 30 == 0,
        })
        .collect();
    let mut track = Track::video(1000, samples);
    track.samples_per_chunk = 10;
    let file = build_mp4(&[track], false);
    let t = std::time::Instant::now();
    let mut r = Mp4Reader::open(file).unwrap();
    let mut count = 0usize;
    while let Some(p) = r.read_packet().unwrap() {
        assert_eq!(p.data[0], count as u8);
        count += 1;
    }
    assert_eq!(count, n);
    assert!(
        t.elapsed() < std::time::Duration::from_secs(20),
        "took {:?}",
        t.elapsed()
    );
}

// ---------------------------------------------------------------------------
// Hostile input
// ---------------------------------------------------------------------------

#[test]
fn rejects_oversized_or_inconsistent_boxes_without_allocating() {
    // moov claiming 3 GiB.
    let mut f = boxed(b"ftyp", b"isom\0\0\0\0");
    f.extend(be32(0xC000_0000));
    f.extend(b"moov");
    f.extend([0u8; 64]);
    assert!(Mp4Reader::open(f).is_err());

    // 64-bit size that overflows the file.
    let mut f = Vec::new();
    f.extend(be32(1));
    f.extend(b"moov");
    f.extend(u64::MAX.to_be_bytes());
    assert!(Mp4Reader::open(f).is_err());

    // Box size smaller than its own header.
    let mut f = Vec::new();
    f.extend(be32(3));
    f.extend(b"free");
    f.extend([0u8; 16]);
    assert!(Mp4Reader::open(f).is_err());

    // Empty and tiny inputs.
    assert!(Mp4Reader::open(Vec::new()).is_err());
    assert!(Mp4Reader::open(vec![0u8; 7]).is_err());
}

#[test]
fn sample_pointing_past_eof_is_an_error_not_a_panic() {
    // A faststart file (moov first) truncated mid-mdat: the index is intact but
    // the last samples point past the end of the file.
    let mut file = build_mp4(&[Track::video(1000, video_samples(6, 1000))], true);
    file.truncate(file.len() - 3000);
    let mut r = Mp4Reader::open(file).unwrap();
    let mut saw_err = false;
    for _ in 0..10 {
        match r.read_packet() {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {
                saw_err = true;
                break;
            }
        }
    }
    assert!(saw_err, "reading past EOF must surface an error");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// Mutating a valid file (and feeding raw noise) never panics, hangs or
    /// aborts on allocation: open() + draining must return.
    #[test]
    fn reader_never_panics_on_mutated_files(
        flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..24),
        truncate in prop::option::of(any::<prop::sample::Index>()),
        noise in prop::collection::vec(any::<u8>(), 0..512),
    ) {
        let base = build_mp4(
            &[Track::video(1000, video_samples(30, 40)), Track::audio(48_000, audio_samples(20))],
            false,
        );
        let mut file = base.clone();
        for (i, b) in &flips {
            let at = i.index(file.len());
            file[at] = *b;
        }
        if let Some(t) = truncate {
            file.truncate(t.index(file.len()).max(1));
        }
        for input in [file, noise] {
            if let Ok(mut r) = Mp4Reader::open(input) {
                for _ in 0..100_000 {
                    match r.read_packet() {
                        Ok(Some(_)) => {}
                        Ok(None) | Err(_) => break,
                    }
                }
                let _ = r.seek(500);
            }
        }
    }
}
