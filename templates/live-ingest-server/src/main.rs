//! A live ingest server: encoders POST WebM to it, and players read HLS back.
//!
//! * Publish: `POST http://<addr>/ingest/<stream>` with a WebM body (for example
//!   `ffmpeg -re -i in.mp4 -c copy -f webm -method POST -chunked_post 1 http://<addr>/ingest/cam`).
//! * Play:    `http://<addr>/<stream>/master.m3u8`, served while the publish runs.
//!
//! The address comes from `LIVE_ADDR` (default `127.0.0.1:8080`).

use tpt_kinetix_package::LiveOptions;
use tpt_kinetix_stream::LiveServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = std::env::var("LIVE_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

    // Two-second segments and a six-segment window, with partial segments
    // enabled. Change `LiveOptions` to tune latency and playlist length.
    let server = LiveServer::new(LiveOptions::default());

    println!("ingest:  POST http://{addr}/ingest/<stream>");
    println!("playback: http://{addr}/<stream>/master.m3u8");
    server.bind_and_serve(&addr).await?;
    Ok(())
}
