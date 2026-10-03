//! Tests for the streaming [`Mp4Reader`]: it must open files without loading
//! them, interleave tracks by decode time, apply `ctts`, scale linearly, and
//! never panic or over-allocate on hostile input.

use proptest::prelude::*;
use tpt_kinetix_demux::{CountingSource, Demuxer, Mp4Reader, ReadAt};

// ---------------------------------------------------------------------------
// A small flexible MP4 writer (multi-track, ctts, stss, chunking)
// ---------------------------------------------------------------------------

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&be32(payload.len() as u32 + 8));
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

fn full(version: u8, body: &[u8]) -> Vec<u8> {
    let mut p = vec![version, 0, 0, 0];
    p.extend_from_slice(body);
    p
}

#[derive(Clone)]
struct Sample {
    data: Vec<u8>,
    delta: u32,
    cts: i32,
    key: bool,
}

#[derive(Clone)]
struct Track {
    handler: [u8; 4],
    fourcc: [u8; 4],
    timescale: u32,
    samples_per_chunk: u32,
    samples: Vec<Sample>,
}

impl Track {
    fn video(timescale: u32, samples: Vec<Sample>) -> Self {
        Self {
            handler: *b"vide",
            fourcc: *b"avc1",
            timescale,
            samples_per_chunk: 3,
            samples,
        }
    }
    fn audio(timescale: u32, samples: Vec<Sample>) -> Self {
        Self {
            handler: *b"soun",
            fourcc: *b"mp4a",
            timescale,
            samples_per_chunk: 2,
            samples,
        }
    }
}

fn runs<T: PartialEq + Copy>(vals: impl Iterator<Item = T>) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for v in vals {
        match out.last_mut() {
            Some((n, last)) if *last == v => *n += 1,
            _ => out.push((1, v)),
        }
    }
    out
}

fn build_moov(tracks: &[Track], chunk_offsets: &[Vec<u32>]) -> Vec<u8> {
    let mut moov = Vec::new();
    moov.extend(boxed(b"mvhd", &full(0, &[0u8; 96])));
    for (ti, t) in tracks.iter().enumerate() {
        let n = t.samples.len() as u32;
        let stts = runs(t.samples.iter().map(|s| s.delta));
        let mut stts_p = be32(stts.len() as u32).to_vec();
        for (c, d) in &stts {
            stts_p.extend(be32(*c));
            stts_p.extend(be32(*d));
        }
        let ctts = runs(t.samples.iter().map(|s| s.cts));
        let mut ctts_p = be32(ctts.len() as u32).to_vec();
        for (c, o) in &ctts {
            ctts_p.extend(be32(*c));
            ctts_p.extend(be32(*o as u32));
        }
        let keys: Vec<u32> = t
            .samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.key)
            .map(|(i, _)| i as u32 + 1)
            .collect();
        let mut stss_p = be32(keys.len() as u32).to_vec();
        for k in &keys {
            stss_p.extend(be32(*k));
        }
        let mut stsc_p = be32(1).to_vec();
        stsc_p.extend(be32(1));
        stsc_p.extend(be32(t.samples_per_chunk));
        stsc_p.extend(be32(1));
        let mut stsz_p = be32(0).to_vec();
        stsz_p.extend(be32(n));
        for s in &t.samples {
            stsz_p.extend(be32(s.data.len() as u32));
        }
        let mut stco_p = be32(chunk_offsets[ti].len() as u32).to_vec();
        for o in &chunk_offsets[ti] {
            stco_p.extend(be32(*o));
        }
        let mut stsd_p = be32(1).to_vec();
        stsd_p.extend(boxed(&t.fourcc, &[0u8; 16]));

        let mut stbl = Vec::new();
        stbl.extend(boxed(b"stsd", &full(0, &stsd_p)));
        stbl.extend(boxed(b"stts", &full(0, &stts_p)));
        if t.samples.iter().any(|s| s.cts != 0) {
            stbl.extend(boxed(b"ctts", &full(1, &ctts_p)));
        }
        if t.samples.iter().any(|s| !s.key) {
            stbl.extend(boxed(b"stss", &full(0, &stss_p)));
        }
        stbl.extend(boxed(b"stsc", &full(0, &stsc_p)));
        stbl.extend(boxed(b"stsz", &full(0, &stsz_p)));
        stbl.extend(boxed(b"stco", &full(0, &stco_p)));

        // tkhd body: creation, modification, track_id, reserved, duration, 52 bytes
        // of layer/matrix/etc, then 16.16 width and height.
        let mut tkhd = vec![0u8; 8];
        tkhd.extend(be32(ti as u32 + 1));
        tkhd.extend([0u8; 8]);
        tkhd.extend([0u8; 52]);
        tkhd.extend(be32(1920 << 16));
        tkhd.extend(be32(1080 << 16));
        // mdhd body: creation, modification, timescale, duration, language.
        let mut mdhd = vec![0u8; 8];
        mdhd.extend(be32(t.timescale));
        mdhd.extend(be32(0));
        mdhd.extend([0u8; 4]);
        // hdlr body: pre_defined, handler_type, 12 reserved, empty name.
        let mut hdlr = vec![0u8; 4];
        hdlr.extend(t.handler);
        hdlr.extend([0u8; 12 + 1]);
        let mdia = [
            boxed(b"mdhd", &full(0, &mdhd)),
            boxed(b"hdlr", &full(0, &hdlr)),
            boxed(b"minf", &boxed(b"stbl", &stbl)),
        ]
        .concat();
        let trak = [boxed(b"tkhd", &full(0, &tkhd)), boxed(b"mdia", &mdia)].concat();
        moov.extend(boxed(b"trak", &trak));
    }
    boxed(b"moov", &moov)
}

/// Builds a file. `moov_first` puts the index before `mdat` ("faststart").
fn build_mp4(tracks: &[Track], moov_first: bool) -> Vec<u8> {
    let ftyp = boxed(b"ftyp", b"isom\0\0\0\0isomavc1");
    // mdat payload: each track's chunks back to back.
    let mut mdat = Vec::new();
    let mut rel_offsets: Vec<Vec<u32>> = Vec::new();
    for t in tracks {
        let mut offs = Vec::new();
        for chunk in t.samples.chunks(t.samples_per_chunk as usize) {
            offs.push(mdat.len() as u32);
            for s in chunk {
                mdat.extend_from_slice(&s.data);
            }
        }
        rel_offsets.push(offs);
    }
    let moov_len = build_moov(tracks, &rel_offsets).len();
    let base = ftyp.len() + if moov_first { moov_len } else { 0 } + 8;
    let abs: Vec<Vec<u32>> = rel_offsets
        .iter()
        .map(|v| v.iter().map(|o| o + base as u32).collect())
        .collect();
    let moov = build_moov(tracks, &abs);
    assert_eq!(moov.len(), moov_len);
    let mdat_box = boxed(b"mdat", &mdat);
    if moov_first {
        [ftyp, moov, mdat_box].concat()
    } else {
        [ftyp, mdat_box, moov].concat()
    }
}

fn video_samples(n: usize, size: usize) -> Vec<Sample> {
    (0..n)
        .map(|i| Sample {
            data: vec![(i % 251) as u8; size],
            delta: 40,
            // B-frame-ish: display order differs from decode order.
            cts: if i % 3 == 0 { 80 } else { 0 },
            key: i % 12 == 0,
        })
        .collect()
}

fn audio_samples(n: usize) -> Vec<Sample> {
    (0..n)
        .map(|i| Sample {
            data: vec![0xA0 | (i % 16) as u8; 17],
            delta: 1024,
            cts: 0,
            key: true,
        })
        .collect()
}

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
