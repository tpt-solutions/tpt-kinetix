//! `kinetix` — command-line interface for the TPT Kinetix media engine.

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "kinetix",
    version,
    about = "TPT Kinetix — memory-safe, hyper-concurrent media processing engine",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Transcode a media file (e.g. H.264 MP4 → AV1).
    Transcode {
        /// Input file path.
        #[arg(short, long)]
        input: String,
        /// Output file path.
        #[arg(short, long)]
        output: String,
        /// Output video codec (default: av1).
        #[arg(long, default_value = "av1")]
        vcodec: String,
    },
    /// Start a live streaming server (RTMP ingest → HLS output).
    Stream {
        /// RTMP bind address.
        #[arg(long, default_value = "0.0.0.0:1935")]
        rtmp_addr: String,
        /// HLS output directory.
        #[arg(long, default_value = "./hls_output")]
        hls_dir: String,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Transcode {
            input,
            output,
            vcodec,
        } => {
            tracing::info!(%input, %output, %vcodec, "transcode requested");
            anyhow::bail!("Transcode not yet implemented — see todo.md Phase 2–5");
        }
        Commands::Stream { rtmp_addr, hls_dir } => {
            tracing::info!(%rtmp_addr, %hls_dir, "stream server requested");
            anyhow::bail!("Streaming not yet implemented — see todo.md Phase 6");
        }
    }
}
