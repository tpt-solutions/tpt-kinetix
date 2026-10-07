//! Packager tests: plan invariants, init + segments reassembling to the source,
//! playlist/MPD structure, read coalescing, a genuinely asynchronous source, and
//! decoding the HLS output in ffmpeg (skipped when ffmpeg is absent).

use std::cell::Cell;
use std::future::Future;
use std::io::Cursor;
use std::pin::Pin;
use std::process::Command;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use proptest::prelude::*;
use tpt_kinetix_core::codec::CodecId;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_demux::{
    block_on, AsyncReadAt, Blocking, CountingSource, Demuxer, Mp4Reader, ReadAt,
};
use tpt_kinetix_mux::{Mp4Writer, WriterOptions};
use tpt_kinetix_package::{codec_string, Packager, PackagerOptions};

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

/// `seconds` of 30 fps video (key frame every `gop` frames, B-frame style
/// composition offsets) interleaved with 48 kHz audio.
fn make_packets(seconds: usize, gop: usize) -> Vec<Packet> {
    let frames = seconds * 30;
    let mut v = Vec::new();
    let mut a = 0i64;
    for i in 0..frames as i64 {
        let cts = if i % 3 == 0 { 2000 } else { 0 };
        v.push(pkt(
            0,
            30_000,
            i * 1000,
            cts,
            (i as usize) % gop == 0,
            vec![(i % 251) as u8; 300 + (i as usize % 9) * 40],
        ));
        // 48000/30 = 1600 audio samples per video frame ~ 1.5625 AAC frames.
        while a * 1024 <= i * 1600 + 1600 {
            v.push(pkt(
                1,
                48_000,
                a * 1024,
                0,
                true,
                vec![0xA0 + (a % 16) as u8; 120 + (a as usize % 7) * 10],
            ));
            a += 1;
        }
    }
    v
}

fn build_mp4(packets: &[Packet]) -> Vec<u8> {
    let mut w = Mp4Writer::new(
        Cursor::new(Vec::new()),
        &[video_stream(), audio_stream()],
        WriterOptions {
            chunk_duration_ms: 500,
            ..Default::default()
        },
    )
    .unwrap();
    for p in packets {
        w.write_packet(p).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn load(file: Vec<u8>, seconds: f64) -> (Packager, Vec<u8>) {
    let p = block_on(Packager::load(
        &Blocking(&file),
        PackagerOptions {
            segment_seconds: seconds,
            ..Default::default()
        },
    ))
    .unwrap();
    (p, file)
}

fn read_all(file: Vec<u8>) -> Vec<Packet> {
    let mut r = Mp4Reader::open(file).unwrap();
    let mut v = Vec::new();
    while let Some(p) = r.read_packet().unwrap() {
        v.push(p);
    }
    v
}

#[test]
fn plan_covers_every_sample_once_and_cuts_video_at_key_frames() {
    let (p, _) = load(build_mp4(&make_packets(20, 45)), 4.0);
    let plan = p.plan();
    assert!(plan.segments.len() >= 4, "{} segments", plan.segments.len());
    for t in 0..p.streams().len() {
        let mut next = 0usize;
        for seg in &plan.segments {
            assert_eq!(
                seg.samples[t].start, next,
                "track {t} ranges must be contiguous"
            );
            next = seg.samples[t].end;
        }
        let total: usize = plan.segments.iter().map(|s| s.samples[t].len()).sum();
        assert_eq!(next, total);
    }
    // Video segments (lead) start on key frames, with gop 45 (1.5 s) and a 4 s
    // target each non-final segment spans at least 4 s.
    let lead = plan.lead;
    assert_eq!(lead, 0);
    for (k, seg) in plan.segments.iter().enumerate() {
        if k + 1 < plan.segments.len() {
            assert!(
                seg.seconds[lead] >= 4.0,
                "segment {k}: {}",
                seg.seconds[lead]
            );
        }
    }
    // Total duration of the spans equals the presentation length.
    let total: f64 = plan.segments.iter().map(|s| s.seconds[0]).sum();
    assert!((total - 20.0).abs() < 0.2, "video spans sum to {total}");
}

#[test]
fn init_plus_segments_reassemble_each_track_exactly() {
    let packets = make_packets(15, 30);
    let (p, file) = load(build_mp4(&packets), 3.0);
    for track in 0..2usize {
        let mut out = p.init_segment(track).unwrap();
        for n in 1..=p.segment_count() {
            out.extend(block_on(p.media_segment(&Blocking(&file), track, n)).unwrap());
        }
        let got = read_all(out);
        let want: Vec<&Packet> = packets
            .iter()
            .filter(|x| x.stream_index == track as u32)
            .collect();
        assert_eq!(got.len(), want.len(), "track {track} packet count");
        for (g, w) in got.iter().zip(&want) {
            assert_eq!(g.data, w.data);
            assert_eq!(
                (g.pts.value, g.dts.value, g.is_key_frame),
                (w.pts.value, w.dts.value, w.is_key_frame)
            );
        }
    }
}

#[test]
fn segments_are_independently_addressable_with_their_own_sequence_numbers() {
    let (p, file) = load(build_mp4(&make_packets(12, 30)), 3.0);
    // Fetch segment 3 first, alone: it must be a valid moof+mdat with sequence 3
    // and a tfdt at its own start time, not zero.
    let seg = block_on(p.media_segment(&Blocking(&file), 0, 3)).unwrap();
    // A DASH-IF media segment: `styp`, a one-reference `sidx` covering exactly the
    // `moof` + `mdat` that follow it, then the fragment.
    assert_eq!(&seg[4..8], b"styp");
    let styp_len = u32::from_be_bytes(seg[0..4].try_into().unwrap()) as usize;
    assert_eq!(&seg[styp_len + 4..styp_len + 8], b"sidx");
    let sidx_len = u32::from_be_bytes(seg[styp_len..styp_len + 4].try_into().unwrap()) as usize;
    let moof_at = styp_len + sidx_len;
    assert_eq!(&seg[moof_at + 4..moof_at + 8], b"moof");
    let sidx = &seg[styp_len + 8..moof_at];
    assert_eq!(sidx[0], 1, "sidx version 1");
    // version/flags(4) ref_id(4) timescale(4) earliest(8) first_offset(8) rsv(2) count(2)
    assert_eq!(u16::from_be_bytes(sidx[30..32].try_into().unwrap()), 1);
    let ref_size = u32::from_be_bytes(sidx[32..36].try_into().unwrap()) & 0x7FFF_FFFF;
    assert_eq!(ref_size as usize, seg.len() - moof_at);
    let at = seg.windows(4).position(|w| w == b"mfhd").unwrap() + 8;
    assert_eq!(u32::from_be_bytes(seg[at..at + 4].try_into().unwrap()), 3);
    let tf = seg.windows(4).position(|w| w == b"tfdt").unwrap() + 4;
    assert_eq!(seg[tf], 1, "tfdt version 1");
    let base = u64::from_be_bytes(seg[tf + 4..tf + 12].try_into().unwrap());
    // tfdt is the DTS of the segment's first sample in the source.
    let reader = Mp4Reader::open(file.clone()).unwrap();
    let first = reader.samples(0)[p.plan().segments[2].samples[0].start];
    assert_eq!(base, first.dts);
    assert!(base > 0);
}

#[test]
fn media_segment_rejects_unknown_tracks_and_segments() {
    let (p, file) = load(build_mp4(&make_packets(6, 30)), 3.0);
    let src = Blocking(&file);
    assert!(block_on(p.media_segment(&src, 9, 1)).is_err());
    assert!(block_on(p.media_segment(&src, 0, 0)).is_err());
    assert!(block_on(p.media_segment(&src, 0, 999)).is_err());
    assert!(p.init_segment(5).is_err());
    assert!(p.hls_media(5).is_err());
}

#[test]
fn hls_and_dash_documents_have_the_expected_structure() {
    let (p, _) = load(build_mp4(&make_packets(20, 30)), 4.0);
    let n = p.segment_count();

    let master = p.hls_master();
    assert!(master.starts_with("#EXTM3U\n"));
    assert!(
        master.contains("CODECS=\"avc1.64001F,mp4a.40.2\""),
        "{master}"
    );
    assert!(master.contains("RESOLUTION=640x360"));
    assert!(master.contains("#EXT-X-MEDIA:TYPE=AUDIO"));
    assert!(master.contains("AUDIO=\"audio\""));
    assert!(master.contains("track-0.m3u8") && master.contains("track-1.m3u8"));
    assert!(master.contains("FRAME-RATE=30.000"), "{master}");

    for t in 0..2 {
        let m = p.hls_media(t).unwrap();
        assert!(m.contains("#EXT-X-ENDLIST"));
        assert!(m.contains(&format!("#EXT-X-MAP:URI=\"init-{t}.mp4\"")));
        assert_eq!(m.matches("#EXTINF:").count(), n);
        assert_eq!(m.matches(&format!("seg-{t}-")).count(), n);
        let target: u64 = m
            .lines()
            .find_map(|l| l.strip_prefix("#EXT-X-TARGETDURATION:"))
            .unwrap()
            .parse()
            .unwrap();
        let max = m
            .lines()
            .filter_map(|l| l.strip_prefix("#EXTINF:"))
            .map(|l| l.trim_end_matches(',').parse::<f64>().unwrap())
            .fold(0.0, f64::max);
        assert!(
            target as f64 >= max - 1e-9,
            "TARGETDURATION {target} < longest {max}"
        );
    }

    let mpd = p.dash_mpd();
    assert_balanced_xml(&mpd);
    assert!(mpd.contains("type=\"static\""));
    assert!(mpd.contains("codecs=\"avc1.64001F\"") && mpd.contains("codecs=\"mp4a.40.2\""));
    assert!(
        mpd.contains("initialization=\"init-0.mp4\"")
            && mpd.contains("media=\"seg-1-$Number$.m4s\"")
    );
    // Timeline durations of each representation add up to its track length.
    for (t, ts, secs) in [(0usize, 30_000.0f64, 20.0f64), (1, 48_000.0, 20.0)] {
        let block = mpd
            .split("<SegmentTemplate")
            .nth(t + 1)
            .unwrap()
            .split("</SegmentTemplate>")
            .next()
            .unwrap();
        let mut total = 0u64;
        for s in block.lines().filter(|l| l.trim_start().starts_with("<S ")) {
            let attr = |name: &str| {
                s.split(&format!(" {name}=\""))
                    .nth(1)
                    .and_then(|r| r.split('"').next())
                    .and_then(|v| v.parse::<u64>().ok())
            };
            total += attr("d").unwrap() * (attr("r").unwrap_or(0) + 1);
        }
        assert!(
            (total as f64 / ts - secs).abs() < 0.2,
            "track {t}: timeline covers {} s",
            total as f64 / ts
        );
    }
}

/// Minimal well-formedness check: tags nest and close.
fn assert_balanced_xml(xml: &str) {
    let mut stack: Vec<String> = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find('<') {
        rest = &rest[i + 1..];
        let end = rest.find('>').expect("unterminated tag");
        let tag = &rest[..end];
        rest = &rest[end + 1..];
        if tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        if let Some(name) = tag.strip_prefix('/') {
            assert_eq!(
                stack.pop().as_deref(),
                Some(name),
                "mismatched close tag </{name}>"
            );
        } else if !tag.ends_with('/') {
            stack.push(tag.split_whitespace().next().unwrap().to_string());
        }
    }
    assert!(stack.is_empty(), "unclosed tags: {stack:?}");
}

#[test]
fn one_segment_costs_few_reads_and_a_zero_gap_costs_many() {
    let file = build_mp4(&make_packets(12, 30));
    let counted = CountingSource::new(file);
    let p = block_on(Packager::load(
        &Blocking(&counted),
        PackagerOptions {
            segment_seconds: 3.0,
            max_read_gap: 256 * 1024,
        },
    ))
    .unwrap();
    let before = counted.calls();
    let seg = block_on(p.media_segment(&Blocking(&counted), 0, 2)).unwrap();
    let reads = counted.calls() - before;
    assert!(!seg.is_empty());
    // Interleaved chunks put audio between video runs: gaps are tolerated, so a
    // 3 s segment (90 frames in ~6 chunks) is a handful of reads.
    assert!(reads <= 3, "{reads} reads with coalescing");

    let strict = block_on(Packager::load(
        &Blocking(&counted),
        PackagerOptions {
            segment_seconds: 3.0,
            max_read_gap: 0,
        },
    ))
    .unwrap();
    let before = counted.calls();
    let seg2 = block_on(strict.media_segment(&Blocking(&counted), 0, 2)).unwrap();
    let strict_reads = counted.calls() - before;
    assert_eq!(seg, seg2, "coalescing must not change the bytes");
    assert!(
        strict_reads > reads,
        "{strict_reads} reads without gap tolerance"
    );
}

// ---------------------------------------------------------------------------
// A genuinely asynchronous source (every read suspends once before completing)
// ---------------------------------------------------------------------------

struct YieldOnce(bool);

impl Future for YieldOnce {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

struct SlowSource {
    data: Vec<u8>,
    suspensions: Cell<u32>,
}

impl AsyncReadAt for SlowSource {
    async fn len(&self) -> std::io::Result<u64> {
        YieldOnce(false).await;
        self.suspensions.set(self.suspensions.get() + 1);
        Ok(self.data.len() as u64)
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        YieldOnce(false).await;
        self.suspensions.set(self.suspensions.get() + 1);
        self.data.read_at(offset, buf)
    }
}

/// A small executor: polls until ready (the waker is a no-op, as the future wakes itself).
fn run<F: Future>(f: F) -> F::Output {
    fn raw() -> RawWaker {
        fn clone(_: *const ()) -> RawWaker {
            raw()
        }
        fn noop(_: *const ()) {}
        static VT: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
        RawWaker::new(std::ptr::null(), &VT)
    }
    let waker = unsafe { Waker::from_raw(raw()) };
    let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

#[test]
fn works_over_a_source_that_really_suspends() {
    let file = build_mp4(&make_packets(10, 30));
    let slow = SlowSource {
        data: file.clone(),
        suspensions: Cell::new(0),
    };
    let p = run(Packager::load(
        &slow,
        PackagerOptions {
            segment_seconds: 3.0,
            ..Default::default()
        },
    ))
    .unwrap();
    assert!(
        slow.suspensions.get() >= 3,
        "the index load must have awaited real I/O"
    );
    let seg = run(p.media_segment(&slow, 0, 2)).unwrap();

    // Identical to what the synchronous path produces.
    let (psync, _) = load(file.clone(), 3.0);
    let seg_sync = block_on(psync.media_segment(&Blocking(&file), 0, 2)).unwrap();
    assert_eq!(seg, seg_sync);
    // And block_on refuses to pretend a suspending source is synchronous.
    let panicked = std::panic::catch_unwind(|| {
        let slow = SlowSource {
            data: vec![0; 8],
            suspensions: Cell::new(0),
        };
        let _ = block_on(AsyncReadAt::len(&slow));
    });
    assert!(panicked.is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(60))]

    /// Whatever the key-frame spacing, target length and audio presence, the
    /// segments partition each track and reassemble to the source packets.
    #[test]
    fn random_files_package_and_reassemble(
        seconds in 2usize..14,
        gop in 5usize..60,
        target in 1u32..8,
    ) {
        let packets = make_packets(seconds, gop);
        let (p, file) = load(build_mp4(&packets), f64::from(target));
        for track in 0..2usize {
            let mut out = p.init_segment(track).unwrap();
            for n in 1..=p.segment_count() {
                if p.plan().segments[n - 1].samples[track].is_empty() { continue; }
                out.extend(block_on(p.media_segment(&Blocking(&file), track, n)).unwrap());
            }
            let got = read_all(out);
            let want: Vec<&Packet> = packets.iter().filter(|x| x.stream_index == track as u32).collect();
            prop_assert_eq!(got.len(), want.len());
            for (g, w) in got.iter().zip(&want) {
                prop_assert_eq!(&g.data, &w.data);
                prop_assert_eq!((g.pts.value, g.dts.value), (w.pts.value, w.dts.value));
            }
        }
    }
}

#[test]
fn codec_strings_match_what_the_streams_carry() {
    assert_eq!(codec_string(&video_stream()).unwrap(), "avc1.64001F");
    assert_eq!(codec_string(&audio_stream()).unwrap(), "mp4a.40.2");
}

// ---------------------------------------------------------------------------
// ffmpeg end to end
// ---------------------------------------------------------------------------

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn framemd5(args: &[&str], map: &str) -> Vec<String> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error"])
        .args(args)
        .args(["-map", map, "-f", "framemd5", "-"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.rsplit(',').next().unwrap().trim().to_string())
        .collect()
}

/// Packages an ffmpeg-made file (B-frames, AAC priming) and checks that ffmpeg
/// decodes the HLS output to exactly the frames the source decodes to.
#[test]
fn hls_output_decodes_identically_to_the_source_in_ffmpeg() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_pkg_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
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
            "8",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "8",
            "-pix_fmt",
            "yuv420p",
        ])
        .args([
            "-c:v", "libx264", "-bf", "2", "-g", "25", "-c:a", "aac", "-ac", "2",
        ])
        .arg(&src)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("skipping: libx264/aac unavailable");
        return;
    }
    let file = std::fs::File::open(&src).unwrap();
    let p = block_on(Packager::load(
        &Blocking(&file),
        PackagerOptions {
            segment_seconds: 3.0,
            ..Default::default()
        },
    ))
    .unwrap();
    let out = dir.join("hls");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("master.m3u8"), p.hls_master()).unwrap();
    for t in 0..p.streams().len() {
        std::fs::write(out.join(format!("track-{t}.m3u8")), p.hls_media(t).unwrap()).unwrap();
        std::fs::write(
            out.join(format!("init-{t}.mp4")),
            p.init_segment(t).unwrap(),
        )
        .unwrap();
        for n in 1..=p.segment_count() {
            let seg = block_on(p.media_segment(&Blocking(&file), t, n)).unwrap();
            std::fs::write(out.join(format!("seg-{t}-{n}.m4s")), seg).unwrap();
        }
    }
    let master = out.join("master.m3u8");
    let (src_s, hls_s) = (src.to_str().unwrap(), master.to_str().unwrap());
    for map in ["0:v:0", "0:a:0"] {
        let want = framemd5(&["-i", src_s], map);
        let got = framemd5(&["-i", hls_s], map);
        assert!(!want.is_empty(), "{map}: source decoded nothing");
        assert_eq!(
            want, got,
            "{map}: HLS output decodes differently from the source"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Packages a Matroska/WebM source: the playlists, segments and MPD must
/// describe it like an MP4 of the same media, and ffmpeg must decode the HLS
/// output to exactly the frames the source decodes to.
#[test]
fn webm_source_packages_into_playable_hls() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_pkgwebm_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.webm");
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
            "6",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "6",
            "-pix_fmt",
            "yuv420p",
        ])
        .args(["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"])
        .args(["-c:a", "libopus", "-ac", "2", "-b:a", "64k"])
        .arg(&src)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("skipping: libvpx-vp9/libopus unavailable");
        return;
    }

    let bytes = std::fs::read(&src).unwrap();
    let p = Packager::load_mkv(
        &bytes,
        PackagerOptions {
            segment_seconds: 2.0,
            ..Default::default()
        },
    )
    .unwrap();

    // Two tracks (VP9 video, Opus audio), both signallable in HLS.
    assert_eq!(p.streams().len(), 2, "tracks");
    assert!(p.codec(0).unwrap().starts_with("vp09."), "{:?}", p.codec(0));
    assert_eq!(p.codec(1), Some("Opus"));
    assert!(p.segment_count() >= 3, "{} segments", p.segment_count());

    // The playlists and the MPD name the segments that exist.
    for t in 0..2usize {
        let pl = p.hls_media(t).unwrap();
        assert!(pl.contains(&format!("init-{t}.mp4")), "{pl}");
        for n in 1..=p.segment_count() {
            assert!(pl.contains(&format!("seg-{t}-{n}.m4s")), "{pl}");
            assert!(
                block_on(p.media_segment(&Blocking(&bytes), t, n))
                    .unwrap()
                    .len()
                    > 8
            );
        }
        assert!(p.init_segment(t).unwrap().len() > 8);
    }
    assert!(p.hls_master().contains("RESOLUTION=320x240"));
    assert!(p.dash_mpd().contains("vp09."));

    // All of a track's segments reassemble into a playable fMP4.
    for t in 0..2usize {
        let mut file = p.init_segment(t).unwrap();
        for n in 1..=p.segment_count() {
            file.extend(block_on(p.media_segment(&Blocking(&bytes), t, n)).unwrap());
        }
        let mut r = Mp4Reader::open(file).unwrap();
        let mut count = 0;
        while r.read_packet().unwrap().is_some() {
            count += 1;
        }
        assert!(count > 0, "track {t} produced no packets");
    }

    // And ffmpeg decodes the packaged HLS to exactly the source.
    let out = dir.join("hls");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("master.m3u8"), p.hls_master()).unwrap();
    for t in 0..2usize {
        std::fs::write(out.join(format!("track-{t}.m3u8")), p.hls_media(t).unwrap()).unwrap();
        std::fs::write(
            out.join(format!("init-{t}.mp4")),
            p.init_segment(t).unwrap(),
        )
        .unwrap();
        for n in 1..=p.segment_count() {
            std::fs::write(
                out.join(format!("seg-{t}-{n}.m4s")),
                block_on(p.media_segment(&Blocking(&bytes), t, n)).unwrap(),
            )
            .unwrap();
        }
    }
    let want = framemd5(&["-i", src.to_str().unwrap()], "0:v:0");
    assert!(!want.is_empty());
    assert_eq!(
        framemd5(&["-i", out.join("master.m3u8").to_str().unwrap()], "0:v:0"),
        want,
        "WebM-packaged HLS decodes differently from the source"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
