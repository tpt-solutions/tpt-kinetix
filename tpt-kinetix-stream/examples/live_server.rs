//! A standalone live server, for trying browser publishing without the CLI:
//!
//! ```text
//! cargo run -p tpt-kinetix-stream --example live_server -- 8080 [token]
//! ```
//!
//! `SEGMENT_SECONDS` (default 2) and `PART_SECONDS` (default: the library default,
//! low-latency on; `0` disables parts) tune the packaging.
//!
//! then open `http://127.0.0.1:8080/publish`.

use tpt_kinetix_package::LiveOptions;
use tpt_kinetix_stream::{IngestPolicy, LiveServer};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `RUST_LOG=tpt_kinetix_stream=debug` shows what the server is doing.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let mut args = std::env::args().skip(1);
    let port = args.next().unwrap_or_else(|| "8080".into());
    let token = args.next();
    let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f64>().ok());
    let mut opts = LiveOptions {
        segment_seconds: env("SEGMENT_SECONDS").unwrap_or(2.0),
        ..LiveOptions::default()
    };
    if let Some(p) = env("PART_SECONDS") {
        opts.part_seconds = (p > 0.0).then_some(p);
    }
    let server = LiveServer::new(opts).with_policy(IngestPolicy {
        token,
        ..Default::default()
    });
    println!("live server on http://127.0.0.1:{port} (publish page: /publish)");
    server.bind_and_serve(&format!("127.0.0.1:{port}")).await
}
