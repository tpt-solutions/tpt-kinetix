//! `tpt-kinetix` — command-line interface for the TPT Kinetix media engine.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "tpt-kinetix",
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
    /// Inspect a media container and print its tracks (demux-only, runnable today).
    Probe {
        /// Input file path (MP4/ISO-BMFF).
        input: PathBuf,
    },
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
        Commands::Probe { input } => probe(&input),
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

/// Inspect an MP4/ISO-BMFF container and print a summary of its tracks.
///
/// This exercises only the demux/identification path, which is fully
/// implemented today (unlike `transcode`/`stream`).
fn probe(input: &std::path::Path) -> Result<()> {
    let data = std::fs::read(input)
        .with_context(|| format!("failed to read input file: {}", input.display()))?;

    let demuxer = tpt_kinetix_demux::Mp4Demuxer::new(data)
        .with_context(|| format!("failed to parse MP4 container: {}", input.display()))?;

    let tracks = demuxer.tracks();
    println!("File: {}", input.display());
    println!("Tracks: {}", tracks.len());

    for track in tracks {
        let codec = track
            .codec
            .map(|c| format!("{c:?}"))
            .unwrap_or_else(|| "unknown".to_string());
        let duration_s = if track.timescale != 0 {
            track.duration as f64 / track.timescale as f64
        } else {
            0.0
        };
        println!(
            "  track #{id} [{media:?}] codec={codec} samples={samples} duration={dur:.3}s",
            id = track.track_id,
            media = track.media_type,
            codec = codec,
            samples = track.sample_count(),
            dur = duration_s,
        );
        if track.width != 0 || track.height != 0 {
            println!("    resolution: {}x{}", track.width, track.height);
        }

        // Report decoder readiness so users know which tracks can actually be
        // decoded pixel-exactly today.
        if let Some(caps) = decoder_capabilities_for(track.codec) {
            let status = if caps.pixel_exact {
                "pixel-exact"
            } else {
                "NOT pixel-exact (placeholder output)"
            };
            println!("    decoder: {status} — {}", caps.notes);
        }
    }

    Ok(())
}

/// Returns the [`DecoderCapabilities`] for a given codec, if a decoder exists.
fn decoder_capabilities_for(
    codec: Option<tpt_kinetix_core::codec::CodecId>,
) -> Option<tpt_kinetix_core::capabilities::DecoderCapabilities> {
    use tpt_kinetix_core::codec::CodecId;
    match codec {
        Some(CodecId::H264) => Some(tpt_kinetix_h264::H264Decoder::new().capabilities()),
        Some(CodecId::Av1) => Some(tpt_kinetix_av1::Av1Decoder::new().capabilities()),
        Some(CodecId::Aac) => Some(tpt_kinetix_aac::AacDecoder::new().capabilities()),
        _ => None,
    }
}
