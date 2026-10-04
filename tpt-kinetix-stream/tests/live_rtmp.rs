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
async fn publish(port: u16, key: &str, src: &std::path::Path, with_audio_config: bool) {
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
    if with_audio_config {
        wr.write_all(&chunks(
            5,
            0,
            8,
            1,
            &ex(0x90, b"Opus", &opus_head(&tracks[1].extradata)),
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
            chunks(5, ts, 8, 1, &ex(0x91, b"Opus", &f.data), CHUNK)
        };
        wr.write_all(&msg).await.unwrap();
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
    let server = LiveServer::new(LiveOptions {
        segment_seconds: 2.0,
        window: 100,
        part_seconds: None,
    });
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rtmp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ports = (
        http.local_addr().unwrap().port(),
        rtmp.local_addr().unwrap().port(),
    );
    let rtmp_server = server.rtmp_server("");
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
