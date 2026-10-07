# tpt-kinetix-stream

Async streaming engine for TPT Kinetix: an RTMP ingest server and HLS packaging
(segment generation, sliding-window playlists, and a minimal HTTP server).

See the [workspace README](../README.md) for the full project overview and
quickstart guide.

## Features

- **RTMP ingest** — TCP server, handshake, chunk-stream reassembly, AMF0
  `connect`/`publish` negotiation, FLV depacketisation and a pluggable
  per-connection event handler. **Enhanced RTMP** (v1 and v2): AV1 / VP9 video
  and Opus audio by FourCC, `Multitrack` (several video renditions or audio
  tracks in one message), `ModEx`, HDR `colorInfo` (written as `colr` / `mdcv` /
  `clli`) and the v2 capability exchange (`capsEx`, FourCC info maps). RTMPS
  with the `rtmps` feature.
- **Live server** (`LiveServer`) — turns publishers into live HLS (fMP4, with
  low-latency parts and blocking playlist reload) and dynamic DASH. Ingest by
  HTTP `POST`/`PUT /ingest/<key>`, WebSocket (a browser publishes
  `MediaRecorder` chunks; ready-made page at `/publish`), WHIP (`whip` feature)
  or RTMP (`LiveServer::rtmp_server`). Optional DVR recording, and
  HTTPS / `wss://` with `LiveServer::with_tls` (`rtmps` feature).
- **Ingest hardening** (`IngestPolicy`) — publish tokens (global and per key),
  idle / duration / byte / bitrate cut-offs (global, with per-key overrides in
  `key_limits`), stream-count and one-publisher-per-key limits, WebSocket ping
  keep-alive, and Prometheus counters at `GET /metrics`. A publish that is cut
  off still finishes its playlists.
- **HLS output** — MPEG-TS segment writing, sliding-window `#EXTM3U` playlist
  generation, and a minimal HTTP server (`GET /playlist.m3u8`,
  `GET /segmentNNN.ts`) with path-traversal protection.

## Running the live server

```sh
# HTTP, WebSocket and Enhanced RTMP ingest; low-latency HLS + DASH out
tpt-kinetix live --port 8080 --rtmp-port 1935     --publish-token s3cret --idle-timeout 30 --max-bitrate-kbps 8000     --key-limit studio:max_bitrate_kbps=40000,idle_timeout=120

# the same over TLS (HTTPS, wss://, RTMPS); needs a build with the `tls` feature
cargo run -p tpt-kinetix-cli --features tls -- live --tls-cert cert.pem --tls-key key.pem
```

Publish with `ffmpeg ... -f webm -method POST http://host:8080/ingest/<key>`, an
Enhanced RTMP encoder (such as a recent OBS) to `rtmp://host:1935/live/<key>?token=s3cret`,
or open `http://host:8080/publish` in Chrome. Play `/<key>/master.m3u8` (HLS)
or `/<key>/manifest.mpd` (DASH). Known hls.js behaviour with audio in
low-latency mode is written up in [todo-io.md](../todo-io.md).

## Status & limitations

- Codecs are the royalty-free set: AV1 and VP9 video, Opus audio. Other
  codecs are logged and ignored.
- Not implemented: `permessage-deflate` for WebSockets, publishing from
  browsers whose `MediaRecorder` cannot produce AV1/VP9 WebM, HDR
  `fullRange` signalling.
- The `tests/` directory covers each ingest path end to end against ffmpeg
  frame MD5s; the real-player checks are `just browser-package-test`,
  `just live-browser-test` and `just latency-test`.

## Quickstart: RTMP ingest

```rust,no_run
use tpt_kinetix_stream::rtmp::{RtmpServer, RtmpConfig, RtmpMediaEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = RtmpServer::new(RtmpConfig::default()) // binds 0.0.0.0:1935
        .with_handler(|event| match event {
            RtmpMediaEvent::PublishStart {
                stream_key,
                capabilities,
            } => {
                println!("publish started: {stream_key} ({capabilities:?})");
            }
            RtmpMediaEvent::Video { timestamp, tag } => {
                // Forward `tag.data` (AVCC NALUs) into tpt-kinetix-pipeline here.
                println!("video @ {timestamp}: {} bytes", tag.data.len());
            }
            RtmpMediaEvent::Audio { timestamp, tag } => {
                println!("audio @ {timestamp}: {} bytes", tag.data.len());
            }
            RtmpMediaEvent::Hdr { timestamp, hdr } => {
                println!("HDR metadata @ {timestamp}: {} bytes", hdr.raw.len());
            }
            RtmpMediaEvent::Multitrack { track_number } => {
                println!("multitrack select: {track_number}");
            }
            RtmpMediaEvent::PublishStop => println!("publish stopped"),
        });
    server.run().await
}
```

Push a stream to it with OBS or ffmpeg:

```sh
ffmpeg -re -i input.mp4 -c:v libx264 -f flv rtmp://localhost:1935/live/stream
```

## Quickstart: HLS output

```rust,no_run
use tpt_kinetix_stream::hls::playlist::HlsPlaylist;
use tpt_kinetix_stream::hls::segment::HlsSegment;

let mut playlist = HlsPlaylist::new(6 /* target duration */, 5 /* window size */);
playlist.add_segment(HlsSegment {
    index: 0,
    duration_secs: 5.98,
    path: "segment00000.ts".into(),
    byte_range: None,
});
let m3u8 = playlist.render();
println!("{m3u8}");
```
