//! End-to-end Enhanced RTMP test: a hand-written RTMP client publishes the frames
//! of an ffmpeg-made AV1 or VP9 + Opus WebM as `av01`/`vp09` + `Opus` (Enhanced
//! RTMP), and the live HLS the server produces is decoded by ffmpeg and compared
//! with the source. A hand-written client is used because no stock encoder
//! available to the tests sends Opus over RTMP. Skipped when ffmpeg is absent.

use std::process::Command;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tpt_kinetix_core::codec::CodecId;
use tpt_kinetix_demux::mkv_stream::{MkvEvent, MkvStream};
use tpt_kinetix_package::LiveOptions;
use tpt_kinetix_stream::rtmp::amf::{self, Amf0Value};
use tpt_kinetix_stream::LiveServer;

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn http_get(port: u16, path: &str) -> (u16, Vec<u8>) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(buf.len());
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, buf[(split + 4).min(buf.len())..].to_vec())
}

fn make_webm(dir: &std::path::Path, vcodec: &[&str], seconds: u32) -> Option<std::path::PathBuf> {
    let path = dir.join("src.webm");
    let secs = seconds.to_string();
    let ok = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=320x240:rate=25")
        .args(["-t", &secs, "-f", "lavfi", "-i"])
        .arg("sine=frequency=440:sample_rate=48000")
        .args(["-t", &secs, "-pix_fmt", "yuv420p"])
        .args(vcodec)
        .args(["-c:a", "libopus", "-ac", "2", "-b:a", "64k"])
        .arg(&path)
        .status()
        .ok()?
        .success();
    ok.then_some(path)
}

fn framemd5(input: &str, map: &str) -> Vec<String> {
    let o = Command::new("ffmpeg")
        .args([
            "-v", "error", "-i", input, "-map", map, "-f", "framemd5", "-",
        ])
        .output()
        .unwrap();
    assert!(
        o.stderr.is_empty(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.rsplit(',').next().unwrap().trim().to_string())
        .collect()
}

/// One RTMP message as chunks (format 0 first, then format 3), `chunk` bytes each.
fn chunks(csid: u8, ts: u32, type_id: u8, stream_id: u32, payload: &[u8], chunk: usize) -> Vec<u8> {
    let mut out = vec![csid];
    out.extend_from_slice(&ts.to_be_bytes()[1..]);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
    out.push(type_id);
    out.extend_from_slice(&stream_id.to_le_bytes());
    for (i, piece) in payload.chunks(chunk).enumerate() {
        if i > 0 {
            out.push(0xC0 | csid);
        }
        out.extend_from_slice(piece);
    }
    out
}

/// `OpusHead` from an MP4 `dOps` payload.
fn opus_head(dops: &[u8]) -> Vec<u8> {
    let mut v = b"OpusHead".to_vec();
    v.push(1);
    v.push(dops[1]); // channels
    v.extend_from_slice(&u16::from_be_bytes([dops[2], dops[3]]).to_le_bytes());
    v.extend_from_slice(&u32::from_be_bytes(dops[4..8].try_into().unwrap()).to_le_bytes());
    v.extend_from_slice(&i16::from_be_bytes([dops[8], dops[9]]).to_le_bytes());
    v.push(dops[10]);
    v
}

/// Publishes `src` over Enhanced RTMP to `port` under `key`, then disconnects.
/// An Enhanced RTMP `colorInfo` payload: BT.2020 / PQ, MaxCLL 1000, MaxFALL 400
/// and a P3-D65 mastering display of 0.0001-1000 cd/m².
fn color_info() -> Vec<u8> {
    let obj = |kv: &[(&str, f64)]| {
        Amf0Value::Object(
            kv.iter()
                .map(|(k, v)| (k.to_string(), Amf0Value::Number(*v)))
                .collect(),
        )
    };
    amf::encode_all(&[
        Amf0Value::String("colorInfo".into()),
        Amf0Value::Object(vec![
            (
                "colorConfig".into(),
                obj(&[
                    ("bitDepth", 10.0),
                    ("colorPrimaries", 9.0),
                    ("transferCharacteristics", 16.0),
                    ("matrixCoefficients", 9.0),
                ]),
            ),
            (
                "hdrCll".into(),
                obj(&[("maxFall", 400.0), ("maxCLL", 1000.0)]),
            ),
            (
                "hdrMdcv".into(),
                obj(&[
                    ("redX", 0.68),
                    ("redY", 0.32),
                    ("greenX", 0.265),
                    ("greenY", 0.69),
                    ("blueX", 0.15),
                    ("blueY", 0.06),
                    ("whitePointX", 0.3127),
                    ("whitePointY", 0.329),
                    ("maxLuminance", 1000.0),
                    ("minLuminance", 0.0001),
                ]),
            ),
        ]),
    ])
}

async fn publish(port: u16, key: &str, src: &std::path::Path, with_audio_config: bool) {
    publish_inner(port, key, src, with_audio_config, None, 1).await
}

/// Like [`publish`], but the Opus audio travels as an Enhanced RTMP audio
/// `Multitrack` message carrying `audio_tracks` copies of the same track
/// (`trackId` 0, 1, ...), as a publisher with several audio sources would send.
async fn publish_audio_tracks(port: u16, key: &str, src: &std::path::Path, audio_tracks: u8) {
    publish_inner(port, key, src, true, None, audio_tracks).await
}

/// Like [`publish`], but after the last frame the connection stays open and
/// silent for `stall` instead of closing cleanly (a dead encoder).
async fn publish_then_stall(port: u16, key: &str, src: &std::path::Path, stall: Duration) {
    publish_inner(port, key, src, true, Some(stall), 1).await
}

async fn publish_inner(
    port: u16,
    key: &str,
    src: &std::path::Path,
    with_audio_config: bool,
    stall: Option<Duration>,
    audio_tracks: u8,
) {
    const CHUNK: usize = 4096;
    // Demux the WebM into tracks and frames.
    let mut parser = MkvStream::new();
    let bytes = std::fs::read(src).unwrap();
    let mut events = parser.push(&bytes).unwrap();
    events.extend(parser.finish().unwrap());
    let mut tracks = Vec::new();
    let mut frames = Vec::new();
    for e in events {
        match e {
            MkvEvent::Tracks(t) => tracks = t,
            MkvEvent::Cue(_) => {}
            MkvEvent::Frame(f) => frames.push(f),
        }
    }
    assert_eq!(tracks.len(), 2);

    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    // Handshake.
    s.write_all(&[3]).await.unwrap();
    let c1: Vec<u8> = (0..1536).map(|i| (i * 13 % 251) as u8).collect();
    s.write_all(&c1).await.unwrap();
    let mut srv = vec![0u8; 1 + 1536 + 1536];
    s.read_exact(&mut srv).await.unwrap();
    s.write_all(&srv[1..1537]).await.unwrap();

    // Drain whatever the server replies with.
    let (mut rd, mut wr) = s.into_split();
    let drain = tokio::spawn(async move {
        let mut b = [0u8; 4096];
        while rd.read(&mut b).await.map(|n| n > 0).unwrap_or(false) {}
    });

    let cmd = |values: &[Amf0Value]| chunks(3, 0, 20, 0, &amf::encode_all(values), CHUNK);
    wr.write_all(&chunks(2, 0, 1, 0, &(CHUNK as u32).to_be_bytes(), 128))
        .await
        .unwrap();
    wr.write_all(&cmd(&[
        Amf0Value::String("connect".into()),
        Amf0Value::Number(1.0),
        Amf0Value::Object(vec![
            ("app".into(), Amf0Value::String("live".into())),
            (
                "fourCcList".into(),
                Amf0Value::StrictArray(vec![
                    Amf0Value::String("av01".into()),
                    Amf0Value::String("vp09".into()),
                    Amf0Value::String("Opus".into()),
                ]),
            ),
        ]),
    ]))
    .await
    .unwrap();
    wr.write_all(&cmd(&[
        Amf0Value::String("createStream".into()),
        Amf0Value::Number(2.0),
        Amf0Value::Null,
    ]))
    .await
    .unwrap();
    wr.write_all(&cmd(&[
        Amf0Value::String("publish".into()),
        Amf0Value::Number(0.0),
        Amf0Value::Null,
        Amf0Value::String(key.into()),
        Amf0Value::String("live".into()),
    ]))
    .await
    .unwrap();

    let fourcc: &[u8; 4] = match tracks[0].codec {
        CodecId::Av1 => b"av01",
        CodecId::Vp9 => b"vp09",
        c => panic!("unexpected {c:?}"),
    };
    let ex = |first: u8, cc: &[u8], body: &[u8]| [&[first][..], cc, body].concat();
    // An audio message: single-track, or a ManyTracks `Multitrack` (5) with one
    // shared FourCC and `trackId, size24, payload` per track.
    let audio_msg = |inner: u8, body: &[u8]| -> Vec<u8> {
        if audio_tracks <= 1 {
            return ex(0x90 | inner, b"Opus", body);
        }
        let mut v = vec![0x95, (1 << 4) | inner];
        v.extend_from_slice(b"Opus");
        for id in 0..audio_tracks {
            v.push(id);
            v.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
            v.extend_from_slice(body);
        }
        v
    };
    // Sequence starts: video (av1C / vpcC), audio (OpusHead).
    wr.write_all(&chunks(
        4,
        0,
        9,
        1,
        &ex(0x80 | 0x10, fourcc, &tracks[0].extradata),
        CHUNK,
    ))
    .await
    .unwrap();
    // An HDR metadata packet (Enhanced RTMP `colorInfo`): it must reach the init
    // segment as `colr` / `mdcv` / `clli` without disturbing the media flow.
    wr.write_all(&chunks(
        4,
        0,
        9,
        1,
        &ex(0x80 | 0x10 | 4, fourcc, &color_info()),
        CHUNK,
    ))
    .await
    .unwrap();
    if with_audio_config {
        wr.write_all(&chunks(
            5,
            0,
            8,
            1,
            &audio_msg(0, &opus_head(&tracks[1].extradata)),
            CHUNK,
        ))
        .await
        .unwrap();
    }
    for f in &frames {
        let ts = f.pts_ms.max(0) as u32;
        let msg = if f.stream == 0 {
            let first = 0x80 | (if f.key { 0x10 } else { 0x20 }) | 3; // CodedFramesX
            chunks(4, ts, 9, 1, &ex(first, fourcc, &f.data), CHUNK)
        } else {
            chunks(5, ts, 8, 1, &audio_msg(1, &f.data), CHUNK)
        };
        wr.write_all(&msg).await.unwrap();
    }
    if let Some(stall) = stall {
        wr.flush().await.unwrap();
        tokio::time::sleep(stall).await;
        return;
    }
    wr.write_all(&cmd(&[
        Amf0Value::String("deleteStream".into()),
        Amf0Value::Number(3.0),
        Amf0Value::Null,
        Amf0Value::Number(1.0),
    ]))
    .await
    .unwrap();
    wr.flush().await.unwrap();
    drop(wr);
    let _ = tokio::time::timeout(Duration::from_secs(5), drain).await;
}

async fn start() -> (u16, u16) {
    start_with(tpt_kinetix_stream::IngestPolicy::default()).await
}

async fn start_with(policy: tpt_kinetix_stream::IngestPolicy) -> (u16, u16) {
    let server = LiveServer::new(LiveOptions {
        segment_seconds: 2.0,
        window: 100,
        part_seconds: None,
    })
    .with_policy(policy);
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rtmp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ports = (
        http.local_addr().unwrap().port(),
        rtmp.local_addr().unwrap().port(),
    );
    let rtmp_server = server.rtmp_server("", None);
    tokio::spawn(server.serve(http));
    tokio::spawn(async move { rtmp_server.serve(rtmp).await });
    ports
}

async fn wait_complete(http: u16, key: &str) -> String {
    let started = Instant::now();
    loop {
        let (code, body) = http_get(http, &format!("/{key}/track-0.m3u8")).await;
        if code == 200 {
            let pl = String::from_utf8(body).unwrap();
            if pl.contains("#EXT-X-ENDLIST") {
                return pl;
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "the live playlist never completed"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn rtmp_matches(name: &str, vcodec: &[&str]) {
    if !have("ffmpeg") {
        eprintln!("skipping {name}: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_livertmp_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(src) = make_webm(&dir, vcodec, 10) else {
        eprintln!("skipping {name}: encoder unavailable");
        return;
    };
    let (http, rtmp) = start().await;
    publish(rtmp, "cam", &src, true).await;
    wait_complete(http, "cam").await;

    let (code, master) = http_get(http, "/cam/master.m3u8").await;
    let master = String::from_utf8(master).unwrap();
    assert_eq!(code, 200, "{master}");
    assert!(
        master.contains(",Opus\"") && master.contains("RESOLUTION=320x240"),
        "{master}"
    );

    let url = format!("http://127.0.0.1:{http}/cam/master.m3u8");
    let src_s = src.to_str().unwrap().to_string();
    let (v_src, v_live) = tokio::task::spawn_blocking({
        let (u, s) = (url.clone(), src_s.clone());
        move || (framemd5(&s, "0:v:0"), framemd5(&u, "0:v:0"))
    })
    .await
    .unwrap();
    assert_eq!(v_src.len(), 250);
    assert_eq!(
        v_src, v_live,
        "{name}: RTMP-ingested video decodes differently"
    );
    let (a_src, a_live) =
        tokio::task::spawn_blocking(move || (framemd5(&src_s, "0:a:0"), framemd5(&url, "0:a:0")))
            .await
            .unwrap();
    // Everything but the final packet (Matroska DiscardPadding trim) is identical.
    assert_eq!(a_src.len(), a_live.len());
    let n = a_src.len() - 1;
    assert_eq!(
        a_src[..n],
        a_live[..n],
        "{name}: RTMP-ingested audio differs"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn enhanced_rtmp_vp9_opus_decodes_identically() {
    rtmp_matches("vp9", &["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"]).await;
}

#[tokio::test]
async fn enhanced_rtmp_av1_opus_decodes_identically() {
    rtmp_matches("av1", &["-c:v", "libaom-av1", "-cpu-used", "8", "-g", "25"]).await;
}

/// A publisher that never sends an audio configuration is treated as video only.
#[tokio::test]
async fn video_only_publisher_is_announced_after_the_audio_wait() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_livertmp_vo_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(src) = make_webm(&dir, &["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"], 6) else {
        eprintln!("skipping: libvpx-vp9 unavailable");
        return;
    };
    let (http, rtmp) = start().await;
    // No audio sequence start: the Opus frames that follow are dropped.
    publish(rtmp, "vo", &src, false).await;
    wait_complete(http, "vo").await;
    let (_, master) = http_get(http, "/vo/master.m3u8").await;
    let master = String::from_utf8(master).unwrap();
    assert!(!master.contains("Opus"), "{master}");
    assert!(master.contains("vp09"), "{master}");
    let (code, _) = http_get(http, "/vo/track-1.m3u8").await;
    assert_eq!(code, 404);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stock ffmpeg publishing VP9 over Enhanced RTMP (needs ffmpeg 6.1+ git / 7.0+;
/// skipped when its FLV muxer does not support the codec).
#[tokio::test]
async fn stock_ffmpeg_publishes_vp9_over_enhanced_rtmp() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let (http, rtmp) = start().await;
    let url = format!("rtmp://127.0.0.1:{rtmp}/live/real");
    let status = tokio::task::spawn_blocking(move || {
        Command::new("ffmpeg")
            .args(["-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=320x240:rate=25")
            .args(["-t", "6", "-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"])
            .args(["-f", "flv", &url])
            .status()
    })
    .await
    .unwrap();
    if !status.map(|s| s.success()).unwrap_or(false) {
        eprintln!("skipping: this ffmpeg cannot publish VP9 over Enhanced RTMP");
        return;
    }
    wait_complete(http, "real").await;
    let (_, master) = http_get(http, "/real/master.m3u8").await;
    let master = String::from_utf8(master).unwrap();
    assert!(
        master.contains("vp09") && master.contains("RESOLUTION=320x240"),
        "{master}"
    );
    let url = format!("http://127.0.0.1:{http}/real/master.m3u8");
    let frames = tokio::task::spawn_blocking(move || framemd5(&url, "0:v:0").len())
        .await
        .unwrap();
    assert_eq!(frames, 150);
}

/// The v2 capability exchange is recorded on `PublishStart`: the FourCC lists
/// and app from the `connect` command object reach the session. Exercises the
/// same path a real OBS 30+ (Enhanced RTMP) publish takes — OBS sends
/// `fourCcList`/`audioFourCcList` in `connect`, which stock ffmpeg does not.
/// Run a real OBS publish manually with:
/// `Settings -> Stream -> Service Custom, Server rtmp://127.0.0.1:1935/live,
///  Stream Key <key>`; the server logs the announced FourCCs at info level.
#[tokio::test]
async fn capability_exchange_reaches_publish_start() {
    use std::sync::{Arc, Mutex};
    use tpt_kinetix_stream::rtmp::server::{RtmpConfig, RtmpMediaEvent, RtmpServer};
    let rtmp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rtmp_port = rtmp.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<RtmpMediaEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let server = RtmpServer::new(RtmpConfig {
        bind_addr: String::new(),
        tls: None,
    })
    .with_handler({
        let seen = seen.clone();
        move |e| seen.lock().unwrap().push(e.clone())
    });
    tokio::spawn(async move { server.serve(rtmp).await });

    let mut s = TcpStream::connect(("127.0.0.1", rtmp_port)).await.unwrap();
    s.write_all(&[3]).await.unwrap();
    let c1: Vec<u8> = (0..1536).map(|i| (i * 13 % 251) as u8).collect();
    s.write_all(&c1).await.unwrap();
    let mut srv = vec![0u8; 1 + 1536 + 1536];
    s.read_exact(&mut srv).await.unwrap();
    s.write_all(&srv[1..1537]).await.unwrap();
    let cmd = |values: &[Amf0Value]| chunks(3, 0, 20, 0, &amf::encode_all(values), 4096);
    // OBS-style connect: app + video/audio FourCC lists + multitrack flag.
    s.write_all(&cmd(&[
        Amf0Value::String("connect".into()),
        Amf0Value::Number(1.0),
        Amf0Value::Object(vec![
            ("app".into(), Amf0Value::String("live".into())),
            (
                "fourCcList".into(),
                Amf0Value::StrictArray(vec![
                    Amf0Value::String("av01".into()),
                    Amf0Value::String("vp09".into()),
                ]),
            ),
            (
                "audioFourCcList".into(),
                Amf0Value::StrictArray(vec![Amf0Value::String("Opus".into())]),
            ),
            ("multitrack".into(), Amf0Value::Boolean(true)),
        ]),
    ]))
    .await
    .unwrap();
    s.write_all(&cmd(&[
        Amf0Value::String("createStream".into()),
        Amf0Value::Number(2.0),
        Amf0Value::Null,
    ]))
    .await
    .unwrap();
    s.write_all(&cmd(&[
        Amf0Value::String("publish".into()),
        Amf0Value::Number(0.0),
        Amf0Value::Null,
        Amf0Value::String("caps".into()),
        Amf0Value::String("live".into()),
    ]))
    .await
    .unwrap();
    // HDR metadata + multitrack packets must surface as their own events.
    let ex = |first: u8, cc: &[u8], body: &[u8]| [&[first][..], cc, body].concat();
    s.write_all(&chunks(
        4,
        0,
        9,
        1,
        &ex(0x80 | 0x10 | 4, b"av01", &[9, 9]),
        128,
    ))
    .await
    .unwrap();
    // A real Enhanced RTMP v2 Multitrack message: ManyTracks (1), CodedFramesX (3),
    // one shared FourCC, then tracks 2 and 3 each with a 24-bit size.
    let mt = [
        &[0x80 | 0x10 | 6, (1 << 4) | 3][..],
        b"av01",
        &[2, 0, 0, 3, 1, 2, 3],
        &[3, 0, 0, 2, 4, 5],
    ]
    .concat();
    s.write_all(&chunks(4, 1, 9, 1, &mt, 128)).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let events = seen.lock().unwrap().clone();
    let Some(RtmpMediaEvent::PublishStart {
        stream_key,
        capabilities,
    }) = events
        .iter()
        .find(|e| matches!(e, RtmpMediaEvent::PublishStart { .. }))
        .cloned()
    else {
        panic!("no PublishStart; got {events:?}");
    };
    assert_eq!(stream_key, "caps");
    assert_eq!(capabilities.app.as_deref(), Some("live"));
    assert!(capabilities.video_four_ccs.contains(b"av01"));
    assert!(capabilities.video_four_ccs.contains(b"vp09"));
    assert!(capabilities.audio_four_ccs.contains(b"Opus"));
    assert!(capabilities.multitrack);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RtmpMediaEvent::Hdr { .. })),
        "HDR metadata event missing: {events:?}"
    );
    let track_ids: Vec<(u8, Vec<u8>)> = events
        .iter()
        .filter_map(|e| match e {
            RtmpMediaEvent::Video { tag, .. } => Some((tag.track_id, tag.data.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        track_ids,
        vec![(2, vec![1, 2, 3]), (3, vec![4, 5])],
        "each track of a Multitrack message is its own event: {events:?}"
    );
}

/// RTMP publishers are held to the same ingest policy as HTTP ones: a token
/// carried in the stream key (`name?token=...`, the OBS convention) and limits.
#[tokio::test]
async fn rtmp_publish_honours_token_and_limits() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_livertmp_policy_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(src) = make_webm(&dir, &["-c:v", "libvpx-vp9", "-g", "25"], 10) else {
        eprintln!("skipping: libvpx-vp9 unavailable");
        return;
    };

    // Token required: a publish without one never appears, one with it does.
    let (http, rtmp) = start_with(tpt_kinetix_stream::IngestPolicy {
        token: Some("tok".into()),
        ..Default::default()
    })
    .await;
    publish(rtmp, "denied", &src, true).await;
    publish(rtmp, "denied?token=wrong", &src, true).await;
    assert_eq!(http_get(http, "/denied/master.m3u8").await.0, 404);
    publish(rtmp, "ok?token=tok", &src, true).await;
    let pl = wait_complete(http, "ok").await;
    let full = pl.matches("#EXTINF").count();
    assert!(full >= 4, "{pl}");
    let (_, metrics) = http_get(http, "/metrics").await;
    let metrics = String::from_utf8(metrics).unwrap();
    assert!(
        metrics.contains("kinetix_publishes_refused_auth_total 2"),
        "{metrics}"
    );

    // A byte limit cuts the publish off: what arrived stays playable, but it is
    // much shorter than the source.
    let (http, rtmp) = start_with(tpt_kinetix_stream::IngestPolicy {
        max_bytes: Some(120_000),
        ..Default::default()
    })
    .await;
    publish(rtmp, "cut", &src, true).await;
    let pl = wait_complete(http, "cut").await;
    let cut = pl.matches("#EXTINF").count();
    assert!(
        cut >= 1 && cut < full,
        "cut at {cut} segments vs {full} in full"
    );
    let (_, metrics) = http_get(http, "/metrics").await;
    let metrics = String::from_utf8(metrics).unwrap();
    assert!(
        metrics.contains("kinetix_publishes_cut_off_total 1"),
        "{metrics}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A publisher that stops sending without closing the connection is ended by the
/// idle timeout: the presentation completes and the slot is released.
#[tokio::test]
async fn rtmp_idle_publisher_is_ended() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_livertmp_idle_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(src) = make_webm(&dir, &["-c:v", "libvpx-vp9", "-g", "25"], 6) else {
        eprintln!("skipping: libvpx-vp9 unavailable");
        return;
    };
    let (http, rtmp) = start_with(tpt_kinetix_stream::IngestPolicy {
        idle_timeout: Some(Duration::from_millis(600)),
        reject_concurrent: true,
        ..Default::default()
    })
    .await;
    // All frames go out in one burst, then silence with the socket still open.
    // The playlist must complete well before the stall ends.
    let publisher = tokio::spawn({
        let src = src.clone();
        async move { publish_then_stall(rtmp, "cam", &src, Duration::from_secs(8)).await }
    });
    let started = Instant::now();
    let pl = wait_complete(http, "cam").await;
    assert!(
        started.elapsed() < Duration::from_secs(7),
        "completed only when the connection closed, not by the idle timeout"
    );
    assert!(pl.matches("#EXTINF").count() >= 2, "{pl}");
    let (_, metrics) = http_get(http, "/metrics").await;
    let metrics = String::from_utf8(metrics).unwrap();
    assert!(
        metrics.contains("kinetix_publishes_cut_off_total 1"),
        "{metrics}"
    );
    assert!(metrics.contains("kinetix_publishers_active 0"), "{metrics}");
    publisher.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

/// An Enhanced RTMP v2 *multitrack* publisher (what OBS Enhanced Broadcasting
/// sends): two video renditions in one message per frame, plus Opus audio.
async fn publish_ladder(port: u16, key: &str, src: &std::path::Path) {
    const CHUNK: usize = 4096;
    let mut parser = MkvStream::new();
    let bytes = std::fs::read(src).unwrap();
    let mut events = parser.push(&bytes).unwrap();
    events.extend(parser.finish().unwrap());
    let (mut tracks, mut frames) = (Vec::new(), Vec::new());
    for e in events {
        match e {
            MkvEvent::Tracks(t) => tracks = t,
            MkvEvent::Cue(_) => {}
            MkvEvent::Frame(f) => frames.push(f),
        }
    }
    assert_eq!(tracks.len(), 3, "two video renditions and audio");

    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(&[3]).await.unwrap();
    let c1: Vec<u8> = (0..1536).map(|i| (i * 13 % 251) as u8).collect();
    s.write_all(&c1).await.unwrap();
    let mut srv = vec![0u8; 1 + 1536 + 1536];
    s.read_exact(&mut srv).await.unwrap();
    s.write_all(&srv[1..1537]).await.unwrap();
    let (mut rd, mut wr) = s.into_split();
    let drain = tokio::spawn(async move {
        let mut b = [0u8; 4096];
        while rd.read(&mut b).await.map(|n| n > 0).unwrap_or(false) {}
    });
    let cmd = |values: &[Amf0Value]| chunks(3, 0, 20, 0, &amf::encode_all(values), CHUNK);
    wr.write_all(&chunks(2, 0, 1, 0, &(CHUNK as u32).to_be_bytes(), 128))
        .await
        .unwrap();
    wr.write_all(&cmd(&[
        Amf0Value::String("connect".into()),
        Amf0Value::Number(1.0),
        Amf0Value::Object(vec![
            ("app".into(), Amf0Value::String("live".into())),
            ("multitrack".into(), Amf0Value::Boolean(true)),
            (
                "fourCcList".into(),
                Amf0Value::StrictArray(vec![
                    Amf0Value::String("vp09".into()),
                    Amf0Value::String("Opus".into()),
                ]),
            ),
        ]),
    ]))
    .await
    .unwrap();
    wr.write_all(&cmd(&[
        Amf0Value::String("createStream".into()),
        Amf0Value::Number(2.0),
        Amf0Value::Null,
    ]))
    .await
    .unwrap();
    wr.write_all(&cmd(&[
        Amf0Value::String("publish".into()),
        Amf0Value::Number(0.0),
        Amf0Value::Null,
        Amf0Value::String(key.into()),
        Amf0Value::String("live".into()),
    ]))
    .await
    .unwrap();

    // ManyTracks (1), one shared FourCC, each track `id, size24, payload`.
    let multitrack = |first: u8, inner: u8, parts: &[(u8, &[u8])]| {
        let mut v = vec![first, (1 << 4) | inner];
        v.extend_from_slice(b"vp09");
        for (id, data) in parts {
            v.push(*id);
            v.extend_from_slice(&(data.len() as u32).to_be_bytes()[1..]);
            v.extend_from_slice(data);
        }
        v
    };
    // One multitrack message carries both renditions' sequence starts.
    let start = multitrack(
        0x80 | 0x10 | 6,
        0,
        &[(0, &tracks[0].extradata), (1, &tracks[1].extradata)],
    );
    wr.write_all(&chunks(4, 0, 9, 1, &start, CHUNK))
        .await
        .unwrap();
    let ex = |first: u8, cc: &[u8], body: &[u8]| [&[first][..], cc, body].concat();
    wr.write_all(&chunks(
        5,
        0,
        8,
        1,
        &ex(0x90, b"Opus", &opus_head(&tracks[2].extradata)),
        CHUNK,
    ))
    .await
    .unwrap();

    // Frames of the two renditions that share a timestamp travel in one message.
    let mut pending: Vec<(u8, bool, Vec<u8>)> = Vec::new();
    let mut pending_ts = 0u32;
    let flush = |pending: &mut Vec<(u8, bool, Vec<u8>)>, ts: u32| -> Vec<u8> {
        if pending.is_empty() {
            return Vec::new();
        }
        let key = pending[0].1;
        assert!(
            pending.iter().all(|p| p.1 == key),
            "renditions must share key frames"
        );
        let first = 0x80 | (if key { 0x10 } else { 0x20 }) | 6;
        let parts: Vec<(u8, &[u8])> = pending.iter().map(|p| (p.0, p.2.as_slice())).collect();
        let msg = multitrack(first, 3, &parts);
        pending.clear();
        chunks(4, ts, 9, 1, &msg, CHUNK)
    };
    for f in &frames {
        let ts = f.pts_ms.max(0) as u32;
        if f.stream < 2 {
            if !pending.is_empty() && pending_ts != ts {
                let m = flush(&mut pending, pending_ts);
                wr.write_all(&m).await.unwrap();
            }
            pending_ts = ts;
            pending.push((f.stream as u8, f.key, f.data.clone()));
        } else {
            let m = flush(&mut pending, pending_ts);
            wr.write_all(&m).await.unwrap();
            wr.write_all(&chunks(5, ts, 8, 1, &ex(0x91, b"Opus", &f.data), CHUNK))
                .await
                .unwrap();
        }
    }
    let m = flush(&mut pending, pending_ts);
    wr.write_all(&m).await.unwrap();
    wr.write_all(&cmd(&[
        Amf0Value::String("deleteStream".into()),
        Amf0Value::Number(3.0),
        Amf0Value::Null,
        Amf0Value::Number(1.0),
    ]))
    .await
    .unwrap();
    wr.flush().await.unwrap();
    drop(wr);
    let _ = tokio::time::timeout(Duration::from_secs(5), drain).await;
}

/// OBS-style multitrack publishing becomes a multi-rendition ladder: each video
/// track is its own HLS variant, with its own resolution, and decodes exactly
/// like the matching rendition of the source.
#[tokio::test]
async fn multitrack_rtmp_publish_becomes_a_ladder() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_livertmp_ladder_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("ladder.webm");
    let ok = std::process::Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=25",
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
        ])
        .args(["-filter_complex", "[0:v]split=2[a][b];[b]scale=320:180[c]"])
        .args([
            "-map", "[a]", "-map", "[c]", "-map", "1:a", "-pix_fmt", "yuv420p",
        ])
        .args([
            "-c:v",
            "libvpx-vp9",
            "-g",
            "25",
            "-keyint_min",
            "25",
            "-sc_threshold",
            "0",
        ])
        .args([
            "-b:v:0", "600k", "-b:v:1", "200k", "-c:a", "libopus", "-ac", "2", "-b:a", "64k",
        ])
        .arg(&src)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("skipping: libvpx-vp9 unavailable");
        return;
    }
    let (http, rtmp) = start().await;
    publish_ladder(rtmp, "cam", &src).await;
    wait_complete(http, "cam").await;

    let (code, master) = http_get(http, "/cam/master.m3u8").await;
    let master = String::from_utf8(master).unwrap();
    assert_eq!(code, 200, "{master}");
    let variants = master
        .lines()
        .filter(|l| l.starts_with("#EXT-X-STREAM-INF"))
        .count();
    assert_eq!(variants, 2, "{master}");
    assert!(
        master.contains("RESOLUTION=640x360") && master.contains("RESOLUTION=320x180"),
        "{master}"
    );

    for (track, map) in [(0usize, "0:v:0"), (1usize, "0:v:1")] {
        let url = format!("http://127.0.0.1:{http}/cam/track-{track}.m3u8");
        let src_s = src.to_str().unwrap().to_string();
        let (want, got) =
            tokio::task::spawn_blocking(move || (framemd5(&src_s, map), framemd5(&url, "0:v:0")))
                .await
                .unwrap();
        assert!(!want.is_empty());
        assert_eq!(
            got, want,
            "rendition {track} decodes differently from the source"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rtmp_color_info_reaches_the_init_segment() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_livertmp_{}_color", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(src) = make_webm(&dir, &["-c:v", "libvpx-vp9", "-b:v", "300k"], 3) else {
        eprintln!("skipping: encoder unavailable");
        return;
    };
    let (http, rtmp) = start().await;
    publish(rtmp, "hdr", &src, true).await;
    let playlist = wait_complete(http, "hdr").await;
    let map = playlist
        .lines()
        .find_map(|l| l.strip_prefix("#EXT-X-MAP:URI=\""))
        .and_then(|l| l.split('"').next())
        .unwrap_or_else(|| panic!("no EXT-X-MAP in {playlist}"));
    let (code, init) = http_get(http, &format!("/hdr/{map}")).await;
    assert_eq!(code, 200);
    let find = |kind: &[u8; 4]| init.windows(4).position(|w| w == kind);
    // colr: 'nclx', BT.2020 (9), PQ (16), BT.2020 NCL (9), limited range.
    let colr = find(b"colr").expect("colr box");
    assert_eq!(&init[colr + 4..colr + 15], b"nclx\0\x09\0\x10\0\x09\0");
    // mdcv: G, B, R, white point, then max / min luminance.
    let mdcv = find(b"mdcv").expect("mdcv box");
    let want: Vec<u8> = [13250u16, 34500, 7500, 3000, 34000, 16000, 15635, 16450]
        .iter()
        .flat_map(|v| v.to_be_bytes())
        .chain(10_000_000u32.to_be_bytes())
        .chain(1u32.to_be_bytes())
        .collect();
    assert_eq!(&init[mdcv + 4..mdcv + 4 + 24], &want[..]);
    // clli: MaxCLL then MaxFALL.
    let clli = find(b"clli").expect("clli box");
    assert_eq!(&init[clli + 4..clli + 8], &[0x03, 0xE8, 0x01, 0x90]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An Enhanced RTMP audio `Multitrack` publish becomes one audio rendition per
/// `trackId`: both are announced, listed in the master playlist, complete, and
/// decode exactly like the source audio.
#[tokio::test]
async fn audio_multitrack_rtmp_publish_becomes_two_audio_renditions() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_livertmp_{}_amt", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(src) = make_webm(&dir, &["-c:v", "libvpx-vp9", "-b:v", "300k"], 4) else {
        eprintln!("skipping: encoder unavailable");
        return;
    };
    let (http, rtmp) = start().await;
    publish_audio_tracks(rtmp, "mt", &src, 2).await;
    wait_complete(http, "mt").await;

    let (code, master) = http_get(http, "/mt/master.m3u8").await;
    let master = String::from_utf8(master).unwrap();
    assert_eq!(code, 200, "{master}");
    assert_eq!(
        master.matches("TYPE=AUDIO").count(),
        2,
        "two audio renditions: {master}"
    );
    for track in [1, 2] {
        let (code, pl) = http_get(http, &format!("/mt/track-{track}.m3u8")).await;
        let pl = String::from_utf8(pl).unwrap();
        assert_eq!(code, 200, "track {track}: {pl}");
        assert!(pl.contains("#EXT-X-ENDLIST"), "track {track}: {pl}");
    }

    let src_s = src.to_str().unwrap().to_string();
    let a_src = tokio::task::spawn_blocking({
        let s = src_s.clone();
        move || framemd5(&s, "0:a:0")
    })
    .await
    .unwrap();
    for track in [1, 2] {
        let url = format!("http://127.0.0.1:{http}/mt/track-{track}.m3u8");
        let a_live = tokio::task::spawn_blocking(move || framemd5(&url, "0:a:0"))
            .await
            .unwrap();
        // Everything but the final packet (Matroska DiscardPadding trim) is identical.
        assert_eq!(a_src.len(), a_live.len(), "audio track {track}");
        let n = a_src.len() - 1;
        assert_eq!(a_src[..n], a_live[..n], "audio track {track} differs");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
