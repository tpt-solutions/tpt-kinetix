# tpt-kinetix-stream

Async streaming engine for TPT Kinetix: an RTMP ingest server and HLS packaging
(segment generation, sliding-window playlists, and a minimal HTTP server).

See the [workspace README](../README.md) for the full project overview and
quickstart guide.

## Features

- **RTMP ingest** — TCP server, handshake, chunk-stream reassembly into complete
  messages, and a pluggable per-message handler for feeding downstream
  processing.
- **HLS output** — `.ts` segment writing, sliding-window `#EXTM3U` playlist
  generation, and a minimal HTTP server (`GET /playlist.m3u8`, `GET /segmentNNN.ts`)
  with path-traversal protection.

## Status & limitations

- The RTMP server performs the handshake and reassembles the chunk stream into
  `RtmpMessage`s (honouring `SetChunkSize`). Full AMF0 `connect`/`publish`
  command negotiation and FLV tag → elementary-stream depacketisation are not
  yet implemented, so the handler currently receives raw message payloads.
- HLS segments are written verbatim; full TS/fMP4 muxing of transcoded output is
  future work.

## Quickstart: RTMP ingest

```rust,no_run
use tpt_kinetix_stream::rtmp::{RtmpServer, RtmpConfig, RtmpMediaEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = RtmpServer::new(RtmpConfig::default()) // binds 0.0.0.0:1935
        .with_handler(|event| match event {
            RtmpMediaEvent::PublishStart { stream_key } => {
                println!("publish started: {stream_key}");
            }
            RtmpMediaEvent::Video { timestamp, tag } => {
                // Forward `tag.data` (AVCC NALUs) into tpt-kinetix-pipeline here.
                println!("video @ {timestamp}: {} bytes", tag.data.len());
            }
            RtmpMediaEvent::Audio { timestamp, tag } => {
                println!("audio @ {timestamp}: {} bytes", tag.data.len());
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
