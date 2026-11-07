# Live ingest server

A starter server that accepts a live WebM push over HTTP and serves the same stream as
low-latency HLS while the push is still running. It uses
[`tpt-kinetix-stream`](https://crates.io/crates/tpt-kinetix-stream).

```sh
cargo run
# in another terminal:
ffmpeg -re -i clip.mp4 -c copy -f webm -method POST -chunked_post 1 http://127.0.0.1:8080/ingest/cam
# then open http://127.0.0.1:8080/cam/master.m3u8 in a player
```

Set `LIVE_ADDR` to change the address. `src/main.rs` is the whole server.
