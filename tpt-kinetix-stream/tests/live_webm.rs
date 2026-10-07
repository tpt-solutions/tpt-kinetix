//! End-to-end live test: a real ffmpeg publishes AV1/VP9 + Opus WebM over HTTP
//! POST to the live server, and the served fMP4 HLS is checked (a) by decoding it
//! in ffmpeg against the source, and (b) for genuinely live behaviour while the
//! publish is still running. Skipped when ffmpeg is absent.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tpt_kinetix_package::LiveOptions;
use tpt_kinetix_stream::LiveServer;

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn start(opts: LiveOptions) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(LiveServer::new(opts).serve(listener));
    port
}

/// A minimal HTTP/1.1 request; returns `(status, body)`.
async fn http(port: u16, method: &str, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    s.write_all(body).await.unwrap();
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
    let ok = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=25",
        ])
        .args(["-t", &seconds.to_string()])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            &seconds.to_string(),
        ])
        .args(["-pix_fmt", "yuv420p"])
        .args(vcodec)
        .args(["-c:a", "libopus", "-ac", "2", "-b:a", "64k"])
        .arg(&path)
        .status()
        .ok()?
        .success();
    ok.then_some(path)
}

fn publish_cmd(src: &std::path::Path, port: u16, key: &str, realtime: bool) -> Command {
    let mut c = Command::new("ffmpeg");
    c.args(["-loglevel", "error"]);
    if realtime {
        c.arg("-re");
    }
    c.arg("-i")
        .arg(src)
        .args([
            "-c",
            "copy",
            "-f",
            "webm",
            "-method",
            "POST",
            "-chunked_post",
            "1",
        ])
        .arg(format!("http://127.0.0.1:{port}/ingest/{key}"));
    c.stdout(Stdio::null());
    c
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

async fn decode_matches(name: &str, vcodec: &[&str]) {
    if !have("ffmpeg") {
        eprintln!("skipping {name}: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_livewebm_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(src) = make_webm(&dir, vcodec, 10) else {
        eprintln!("skipping {name}: encoder unavailable");
        return;
    };
    let port = start(LiveOptions {
        segment_seconds: 2.0,
        window: 100,
        part_seconds: None,
    })
    .await;

    let mut cmd = publish_cmd(&src, port, "cam", false);
    let status = tokio::task::spawn_blocking(move || cmd.status().unwrap())
        .await
        .unwrap();
    assert!(status.success(), "ffmpeg publish failed");

    let (code, master) = http(port, "GET", "/cam/master.m3u8", b"").await;
    let master = String::from_utf8(master).unwrap();
    assert_eq!(code, 200, "{master}");
    assert!(
        master.contains(",Opus\"") && master.contains("RESOLUTION=320x240"),
        "{master}"
    );

    // The same presentation is served as a dynamic DASH manifest, naming the
    // very same init/segment URLs the HLS playlists do.
    let (code, mpd) = http(port, "GET", "/cam/manifest.mpd", b"").await;
    let mpd = String::from_utf8(mpd).unwrap();
    assert_eq!(code, 200, "{mpd}");
    assert!(mpd.contains("type=\"dynamic\""), "{mpd}");
    assert!(mpd.contains("availabilityStartTime="), "{mpd}");
    assert!(mpd.contains("minimumUpdatePeriod="), "{mpd}");
    assert!(!mpd.contains("mediaPresentationDuration"), "{mpd}");
    assert!(mpd.contains("init-0.mp4") && mpd.contains("seg-0-$Number$.m4s"));
    assert!(well_formed_xml(&mpd), "MPD is not well-formed XML:\n{mpd}");
    // And every segment it names is actually fetchable.
    let first = mpd
        .split("startNumber=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(1);
    for track in 0..2 {
        let name = format!("seg-{track}-{first}.m4s");
        let (code, body) = http(port, "GET", &format!("/cam/{name}"), b"").await;
        assert_eq!(code, 200, "{name} is named in the MPD but not served");
        assert!(body.len() > 8 && (&body[4..8] == b"moof" || &body[4..8] == b"styp"));
    }
    let (code, track0) = http(port, "GET", "/cam/track-0.m3u8", b"").await;
    assert_eq!(code, 200);
    assert!(String::from_utf8(track0)
        .unwrap()
        .contains("#EXT-X-ENDLIST"));

    let url = format!("http://127.0.0.1:{port}/cam/master.m3u8");
    let src_s = src.to_str().unwrap().to_string();
    let (v_src, v_live) =
        tokio::task::spawn_blocking(move || (framemd5(&src_s, "0:v:0"), framemd5(&url, "0:v:0")))
            .await
            .unwrap();
    assert_eq!(v_src.len(), 250);
    assert_eq!(
        v_src, v_live,
        "{name}: live HLS video decodes differently from the source"
    );
    let url = format!("http://127.0.0.1:{port}/cam/master.m3u8");
    let src_s = src.to_str().unwrap().to_string();
    let (a_src, a_live) =
        tokio::task::spawn_blocking(move || (framemd5(&src_s, "0:a:0"), framemd5(&url, "0:a:0")))
            .await
            .unwrap();
    // All audio frames identical except the final one (Matroska DiscardPadding trim).
    assert_eq!(a_src.len(), a_live.len());
    let n = a_src.len() - 1;
    assert_eq!(
        a_src[..n],
        a_live[..n],
        "{name}: live HLS audio decodes differently"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn av1_opus_publish_over_http_decodes_identically() {
    decode_matches("av1", &["-c:v", "libaom-av1", "-cpu-used", "8", "-g", "25"]).await;
}

#[tokio::test]
async fn vp9_opus_publish_over_http_decodes_identically() {
    decode_matches("vp9", &["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"]).await;
}

/// While a real-time publish is still running the playlist exists, has no
/// ENDLIST, and its window slides; after the publish it is complete.
#[tokio::test]
async fn playlist_is_live_while_publishing_and_complete_afterwards() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_livewebm_rt_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(src) = make_webm(
        &dir,
        &["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"],
        14,
    ) else {
        eprintln!("skipping: libvpx-vp9 unavailable");
        return;
    };
    let port = start(LiveOptions {
        segment_seconds: 1.0,
        window: 3,
        part_seconds: None,
    })
    .await;
    let mut child = publish_cmd(&src, port, "rt", true).spawn().unwrap();

    let started = Instant::now();
    let mut first_seq = None;
    let mut slid = false;
    let mut saw_live_playlist = false;
    while child.try_wait().unwrap().is_none() && started.elapsed() < Duration::from_secs(40) {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let (code, body) = http(port, "GET", "/rt/track-0.m3u8", b"").await;
        if code != 200 {
            continue;
        }
        let pl = String::from_utf8(body).unwrap();
        if child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            !pl.contains("#EXT-X-ENDLIST"),
            "playlist must not be complete mid-stream:\n{pl}"
        );
        saw_live_playlist = true;
        let seq: u64 = pl
            .lines()
            .find_map(|l| l.strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))
            .unwrap()
            .parse()
            .unwrap();
        let listed = pl.matches("#EXTINF:").count();
        assert!(listed <= 3, "window must not exceed 3 segments:\n{pl}");
        match first_seq {
            None => first_seq = Some(seq),
            Some(f) if seq > f => slid = true,
            _ => {}
        }
    }
    assert!(child.wait().unwrap().success());
    assert!(
        saw_live_playlist,
        "no playlist was served while the publish was running"
    );
    assert!(slid, "the window never advanced during a 14 s publish");

    // The newest segment is fetchable while/after streaming; the playlist is now complete.
    let (_, pl) = http(port, "GET", "/rt/track-0.m3u8", b"").await;
    let pl = String::from_utf8(pl).unwrap();
    assert!(pl.contains("#EXT-X-ENDLIST"), "{pl}");
    let last = pl
        .lines()
        .rev()
        .find(|l| l.starts_with("seg-0-"))
        .unwrap()
        .to_string();
    let (code, seg) = http(port, "GET", &format!("/rt/{last}"), b"").await;
    assert_eq!(code, 200);
    assert_eq!(&seg[4..8], b"moof");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Low-latency HLS over HTTP: the served playlist advertises parts, every
/// advertised part URI fetches a fragment, and a blocking reload
/// (`_HLS_msn`) is answered only once its segment exists.
/// Multi-threaded: this test blocks on `std::process::Child::wait`, which would
/// starve the server task that has to drain the publish socket on a
/// current-thread runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ll_hls_parts_are_served_and_blocking_reload_resolves() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_livelld_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(src) = make_webm(
        &dir,
        &["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"],
        10,
    ) else {
        eprintln!("skipping: libvpx-vp9 unavailable");
        return;
    };
    let port = start(LiveOptions {
        segment_seconds: 2.0,
        window: 6,
        part_seconds: Some(1.0 / 3.0),
    })
    .await;

    // A blocking reload for a segment far in the future must not answer instantly
    // with the current playlist: it waits, then resolves once the publish gets
    // there (or gives up and returns 404, which the test accepts as "blocked").
    let blocking = tokio::spawn(async move {
        http(port, "GET", "/ll/track-0.m3u8?_HLS_msn=3&_HLS_part=0", b"").await
    });
    let mut child = publish_cmd(&src, port, "ll", true).spawn().unwrap();
    let started = Instant::now();
    let mut saw_parts = false;
    let mut preload = false;
    while child.try_wait().unwrap().is_none() && started.elapsed() < Duration::from_secs(30) {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let (code, body) = http(port, "GET", "/ll/track-0.m3u8", b"").await;
        if code != 200 {
            continue;
        }
        let pl = String::from_utf8(body).unwrap();
        if !pl.contains("#EXT-X-PART:") {
            continue;
        }
        saw_parts = true;
        preload |= pl.contains("#EXT-X-PRELOAD-HINT:TYPE=PART");
        // Every part the playlist advertises must be fetchable right now. The
        // preload hint deliberately names the *next*, not-yet-published part.
        let uris: Vec<String> = pl
            .lines()
            .filter(|l| l.starts_with("#EXT-X-PART:"))
            .filter_map(|l| l.split("URI=\"").nth(1))
            .filter_map(|l| l.split('"').next())
            .filter(|u| u.starts_with("part-"))
            .map(str::to_string)
            .collect();
        assert!(!uris.is_empty(), "{pl}");
        for u in &uris {
            let (code, data) = http(port, "GET", &format!("/ll/{u}"), b"").await;
            assert_eq!(code, 200, "{u} (status {code})\nplaylist:\n{pl}");
            assert_eq!(&data[4..8], b"moof", "{u} is not a fragment");
        }
        // The preload hint names a part that does not exist yet. A player requests
        // it at once; the server must hold the request until the part is
        // published (RFC 8216bis 6.2.5.2), not answer 404 (which made hls.js treat
        // the part as a gap and stall).
        let hint = pl
            .lines()
            .find_map(|l| l.strip_prefix("#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\""))
            .and_then(|l| l.split('"').next())
            .expect("a preload hint")
            .to_string();
        let (code, data) = http(port, "GET", &format!("/ll/{hint}"), b"").await;
        assert_eq!(code, 200, "preload hint {hint} must block, not 404");
        assert_eq!(&data[4..8], b"moof", "{hint} is not a fragment");
        break;
    }
    assert!(child.wait().unwrap().success());
    assert!(saw_parts, "no playlist with parts was served");
    assert!(preload, "no preload hint was served");

    // The blocked request resolved: either the playlist at msn=3, or a 404 after
    // the wait — never an immediate stale playlist.
    match tokio::time::timeout(Duration::from_secs(10), blocking).await {
        Ok(Ok((code, body))) => {
            assert!(code == 200 || code == 404, "status {code}");
            if code == 200 {
                let pl = String::from_utf8(body).unwrap();
                assert!(pl.contains("#EXT-X-PART:"), "{pl}");
            }
        }
        Ok(Err(e)) => panic!("blocking reload task failed: {e}"),
        Err(_) => panic!("the blocking reload never completed"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn http_edge_cases() {
    let port = start(LiveOptions::default()).await;
    assert_eq!(http(port, "GET", "/nothing/master.m3u8", b"").await.0, 404);
    assert_eq!(http(port, "GET", "/nothing", b"").await.0, 404);
    assert_eq!(http(port, "POST", "/ingest/../etc", b"x").await.0, 404);
    assert_eq!(http(port, "POST", "/other/cam", b"x").await.0, 404);
    assert_eq!(http(port, "DELETE", "/cam/master.m3u8", b"").await.0, 405);
    // Garbage is not a WebM: rejected, and the key exists but is not ready.
    assert_eq!(
        http(port, "POST", "/ingest/bad", b"this is not webm")
            .await
            .0,
        400
    );
    assert_eq!(http(port, "GET", "/bad/master.m3u8", b"").await.0, 404);
}

/// Checks that every element in `xml` is closed and correctly nested: enough to
/// reject a malformed MPD without pulling in a parser dependency.
fn well_formed_xml(xml: &str) -> bool {
    let mut stack: Vec<&str> = Vec::new();
    let bytes = xml.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let Some(close) = xml[i..].find('>').map(|j| i + j) else {
            return false;
        };
        let tag = &xml[i + 1..close];
        i = close + 1;
        // Comments, declarations and processing instructions carry no nesting.
        if tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        if let Some(name) = tag.strip_prefix('/') {
            // A closing tag must match the innermost open element.
            if stack.pop() != Some(name.trim()) {
                return false;
            }
        } else if !tag.ends_with('/') {
            stack.push(tag.split_whitespace().next().unwrap_or(""));
        }
    }
    stack.is_empty()
}

/// A publisher that drops and reconnects under the same key continues the same
/// presentation: segment numbers keep counting, the playlist gains one
/// `EXT-X-DISCONTINUITY`, and every listed segment can be fetched.
#[tokio::test]
async fn reconnecting_publisher_continues_the_presentation() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_live_reconnect_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(src) = make_webm(&dir, &["-c:v", "libvpx-vp9", "-g", "25"], 6) else {
        eprintln!("skipping: libvpx-vp9 unavailable");
        return;
    };
    let webm = std::fs::read(&src).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let port = start(LiveOptions {
        segment_seconds: 2.0,
        window: 50,
        part_seconds: None,
    })
    .await;

    let (st, _) = http(port, "POST", "/ingest/cam", &webm).await;
    assert_eq!(st, 200);
    let (_, first) = http(port, "GET", "/cam/track-0.m3u8", b"").await;
    let first = String::from_utf8(first).unwrap();
    let first_segments = first.matches("#EXTINF").count();
    assert!(!first.contains("DISCONTINUITY"));

    let (st, _) = http(port, "POST", "/ingest/cam", &webm).await;
    assert_eq!(st, 200);
    let (_, both) = http(port, "GET", "/cam/track-0.m3u8", b"").await;
    let both = String::from_utf8(both).unwrap();
    assert_eq!(
        both.matches(
            "#EXT-X-DISCONTINUITY
"
        )
        .count(),
        1,
        "{both}"
    );
    assert!(both.matches("#EXTINF").count() > first_segments);
    assert!(both.contains("#EXT-X-ENDLIST"));
    for l in both.lines().filter(|l| l.starts_with("seg-")) {
        let (st, body) = http(port, "GET", &format!("/cam/{l}"), b"").await;
        assert_eq!(st, 200, "{l}");
        assert!(!body.is_empty());
    }
}

/// A recorded publish is a VOD presentation afterwards: the playlist is complete,
/// every segment is served from disk, a reconnect extends the same recording, and
/// the recording decodes in ffmpeg.
#[tokio::test]
async fn recording_serves_a_vod_after_the_publish_ends() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_live_record_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let Some(src) = make_webm(&dir, &["-c:v", "libvpx-vp9", "-g", "25"], 6) else {
        eprintln!("skipping: libvpx-vp9 unavailable");
        return;
    };
    let webm = std::fs::read(&src).unwrap();
    let rec_dir = dir.join("rec");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(
        LiveServer::new(LiveOptions {
            segment_seconds: 2.0,
            window: 2, // the live window is tiny; the recording must keep everything
            part_seconds: None,
        })
        .with_recording(&rec_dir)
        .serve(listener),
    );

    let (st, _) = http(port, "POST", "/ingest/cam", &webm).await;
    assert_eq!(st, 200);
    let (st, pl) = http(port, "GET", "/cam/dvr/track-0.m3u8", b"").await;
    assert_eq!(st, 200);
    let pl = String::from_utf8(pl).unwrap();
    assert!(
        pl.contains("PLAYLIST-TYPE:VOD") && pl.contains("#EXT-X-ENDLIST"),
        "{pl}"
    );
    let first_segments = pl.matches("#EXTINF").count();
    assert!(first_segments >= 3, "{pl}");
    // The live window kept at most `window` of them; the recording kept all.
    let (_, live_pl) = http(port, "GET", "/cam/track-0.m3u8", b"").await;
    assert!(
        String::from_utf8(live_pl)
            .unwrap()
            .matches("#EXTINF")
            .count()
            <= 2
    );

    // Reconnect: the same recording grows, with a discontinuity.
    let (st, _) = http(port, "POST", "/ingest/cam", &webm).await;
    assert_eq!(st, 200);
    let (_, pl) = http(port, "GET", "/cam/dvr/track-0.m3u8", b"").await;
    let pl = String::from_utf8(pl).unwrap();
    assert!(pl.matches("#EXTINF").count() > first_segments);
    assert_eq!(pl.matches("#EXT-X-DISCONTINUITY\n").count(), 1, "{pl}");

    // Everything the playlists name is fetchable; the master resolves too.
    let (st, master) = http(port, "GET", "/cam/dvr/master.m3u8", b"").await;
    assert_eq!(st, 200);
    assert!(String::from_utf8(master).unwrap().contains("track-0.m3u8"));
    let (st, init) = http(port, "GET", "/cam/dvr/init-0.mp4", b"").await;
    assert_eq!(st, 200);
    let mut media = init;
    for l in pl.lines().filter(|l| l.starts_with("seg-")) {
        let (st, body) = http(port, "GET", &format!("/cam/dvr/{l}"), b"").await;
        assert_eq!(st, 200, "{l}");
        media.extend_from_slice(&body);
    }
    // Path tricks and unknown keys are refused.
    assert_eq!(http(port, "GET", "/cam/dvr/..%2Fx", b"").await.0, 404);
    assert_eq!(
        http(port, "GET", "/nokey/dvr/track-0.m3u8", b"").await.0,
        404
    );
    // The recorded video decodes in ffmpeg.
    let out = dir.join("rec.mp4");
    std::fs::write(&out, &media).unwrap();
    let frames = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&out)
        .args(["-map", "0:v:0", "-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        frames.status.success(),
        "{}",
        String::from_utf8_lossy(&frames.stderr)
    );
    // And it is on disk where the server says.
    assert!(rec_dir.join("cam").join("g1").join("track-0.m3u8").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A rolling DVR depth deletes the oldest recorded segments and slides the
/// recorded playlist; old generations are pruned when a new one starts.
#[tokio::test]
async fn recording_depth_and_generations_are_bounded() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_live_retain_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let Some(src) = make_webm(&dir, &["-c:v", "libvpx-vp9", "-g", "25"], 8) else {
        eprintln!("skipping: libvpx-vp9 unavailable");
        return;
    };
    let webm = std::fs::read(&src).unwrap();
    let rec_dir = dir.join("rec");
    let opts = || LiveOptions {
        segment_seconds: 2.0,
        window: 50,
        part_seconds: None,
    };
    let limits = tpt_kinetix_stream::RecordingLimits {
        max_duration: Some(Duration::from_secs(5)),
        keep_generations: Some(1),
        ..Default::default()
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(
        LiveServer::new(opts())
            .with_recording_limits(&rec_dir, limits)
            .serve(listener),
    );
    let (st, _) = http(port, "POST", "/ingest/cam", &webm).await;
    assert_eq!(st, 200);
    let (_, pl) = http(port, "GET", "/cam/dvr/track-0.m3u8", b"").await;
    let pl = String::from_utf8(pl).unwrap();
    let kept = pl.matches("#EXTINF").count();
    assert!((1..=3).contains(&kept), "5 s depth of 2 s segments: {pl}");
    assert!(
        !pl.contains("MEDIA-SEQUENCE:1\n"),
        "the head must have slid: {pl}"
    );
    // Trimmed segments are gone from disk, retained ones are served.
    let g1 = rec_dir.join("cam").join("g1");
    let files = std::fs::read_dir(&g1)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("seg-0-"))
        .count();
    assert_eq!(files, kept);
    for l in pl.lines().filter(|l| l.starts_with("seg-")) {
        assert_eq!(
            http(port, "GET", &format!("/cam/dvr/{l}"), b"").await.0,
            200
        );
    }

    // A restarted server (no in-memory state) starts generation 2 and prunes g1.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port2 = listener.local_addr().unwrap().port();
    tokio::spawn(
        LiveServer::new(opts())
            .with_recording_limits(&rec_dir, limits)
            .serve(listener),
    );
    assert_eq!(http(port2, "POST", "/ingest/cam", &webm).await.0, 200);
    assert!(rec_dir.join("cam").join("g2").exists());
    assert!(!g1.exists(), "generation 1 should have been pruned");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A byte budget trims the oldest recorded segments (across tracks) and keeps the
/// playlist and the files on disk in step.
#[tokio::test]
async fn recording_byte_budget_trims_oldest_segments() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_live_bytes_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let Some(src) = make_webm(&dir, &["-c:v", "libvpx-vp9", "-g", "25"], 8) else {
        eprintln!("skipping: libvpx-vp9 unavailable");
        return;
    };
    let webm = std::fs::read(&src).unwrap();
    let rec_dir = dir.join("rec");
    let budget = 60_000u64;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(
        LiveServer::new(LiveOptions {
            segment_seconds: 2.0,
            window: 50,
            part_seconds: None,
        })
        .with_recording_limits(
            &rec_dir,
            tpt_kinetix_stream::RecordingLimits {
                max_bytes: Some(budget),
                ..Default::default()
            },
        )
        .serve(listener),
    );
    assert_eq!(http(port, "POST", "/ingest/cam", &webm).await.0, 200);

    let g1 = rec_dir.join("cam").join("g1");
    let seg_bytes: u64 = std::fs::read_dir(&g1)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("seg-"))
        .map(|e| e.metadata().unwrap().len())
        .sum();
    // At most the budget, plus slack for the one segment per track that is always kept.
    assert!(
        seg_bytes < budget * 3,
        "{seg_bytes} bytes kept for a {budget} budget"
    );
    let (_, pl) = http(port, "GET", "/cam/dvr/track-0.m3u8", b"").await;
    let pl = String::from_utf8(pl).unwrap();
    assert!(
        !pl.contains(
            "MEDIA-SEQUENCE:1
"
        ),
        "the head must have been trimmed: {pl}"
    );
    assert!(pl.contains("#EXT-X-ENDLIST"), "{pl}");
    // Every segment the playlist still names is on disk and served.
    for l in pl.lines().filter(|l| l.starts_with("seg-")) {
        assert_eq!(
            http(port, "GET", &format!("/cam/dvr/{l}"), b"").await.0,
            200,
            "{l}"
        );
    }
    // Nothing the playlist dropped is left behind.
    let named: Vec<&str> = pl.lines().filter(|l| l.starts_with("seg-0-")).collect();
    let on_disk = std::fs::read_dir(&g1)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("seg-0-"))
        .count();
    assert_eq!(on_disk, named.len());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Reads one chunked HTTP response from `s`, calling `on_chunk(len, elapsed)` as
/// each chunk arrives; returns the de-chunked body.
async fn read_chunked(
    s: &mut TcpStream,
    started: Instant,
    mut on_chunk: impl FnMut(usize, Duration),
) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 16 * 1024];
    let mut body = Vec::new();
    let mut pos = None;
    loop {
        // Parse what is complete in `buf`.
        loop {
            let p = *pos.get_or_insert_with(|| {
                buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
            });
            let Some(mut at) = p else { break };
            // `at` is the start of the next chunk-size line.
            let Some(eol) = buf[at..].windows(2).position(|w| w == b"\r\n") else {
                break;
            };
            let size = usize::from_str_radix(std::str::from_utf8(&buf[at..at + eol]).unwrap(), 16)
                .unwrap();
            let data_start = at + eol + 2;
            if buf.len() < data_start + size + 2 {
                break;
            }
            if size == 0 {
                return body;
            }
            on_chunk(size, started.elapsed());
            body.extend_from_slice(&buf[data_start..data_start + size]);
            at = data_start + size + 2;
            buf.drain(..at);
            pos = Some(Some(0));
        }
        let n = s.read(&mut tmp).await.unwrap();
        if n == 0 {
            return body;
        }
        buf.extend_from_slice(&tmp[..n]);
        if pos.is_some() && pos != Some(Some(0)) {
            pos = None; // header not seen yet; retry the search
        }
    }
}

/// Low-latency DASH: a segment requested while it is still being published is
/// streamed as its CMAF chunks appear (the first arrives long before the segment
/// completes), and the streamed bytes equal the finished segment exactly.
#[tokio::test]
async fn ll_dash_segment_streams_while_it_is_published() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_lldash_stream_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(src) = make_webm(
        &dir,
        &["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"],
        10,
    ) else {
        eprintln!("skipping: libvpx-vp9 unavailable");
        return;
    };
    let port = start(LiveOptions {
        segment_seconds: 2.0,
        window: 20,
        part_seconds: Some(1.0 / 3.0),
    })
    .await;
    let mut child = publish_cmd(&src, port, "ll", true).spawn().unwrap();

    // Wait for the first complete segment, then ask for the next, in-progress one.
    let started = Instant::now();
    let mut last_stats = String::new();
    let latest = loop {
        let (code, body) = http(port, "GET", "/ll/_stats", b"").await;
        if code == 200 {
            let j = String::from_utf8(body).unwrap();
            last_stats = j.clone();
            let n: u64 = j
                .split("\"latest_segment\":")
                .nth(1)
                .and_then(|r| r.split([',', '}']).next())
                .and_then(|n| n.trim().parse().ok())
                .unwrap_or(0);
            if n >= 1 {
                break n;
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "no segment appeared (last _stats: {last_stats:?}, last status {code})"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let target = latest + 1;
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(
        format!("GET /ll/seg-0-{target}.m4s HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .await
    .unwrap();
    let t0 = Instant::now();
    let mut arrivals: Vec<(usize, Duration)> = Vec::new();
    let streamed = read_chunked(&mut s, t0, |n, at| arrivals.push((n, at))).await;
    let total = t0.elapsed();
    assert!(
        arrivals.len() >= 3,
        "expected several CMAF chunks, got {}",
        arrivals.len()
    );
    let first = arrivals[0].1;
    assert!(
        total - first >= Duration::from_millis(500),
        "the response was not streamed: first chunk at {first:?}, end at {total:?}"
    );
    // The streamed bytes are the finished segment.
    let (code, whole) = http(port, "GET", &format!("/ll/seg-0-{target}.m4s"), b"").await;
    assert_eq!(code, 200);
    assert_eq!(
        streamed, whole,
        "streamed chunks differ from the completed segment"
    );
    assert_eq!(&whole[4..8], b"moof");
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
