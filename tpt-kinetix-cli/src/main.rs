//! `tpt-kinetix` — command-line interface for the TPT Kinetix media engine.

use std::path::PathBuf;
use std::sync::Arc;

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
        /// Quality (0=best, 255=worst). Default 100.
        #[arg(long, default_value_t = 100)]
        quality: u8,
        /// Speed preset (0=slowest/best, 10=fastest). Default 6.
        #[arg(long, default_value_t = 6)]
        speed: u8,
        /// Target bitrate in bits/sec (overrides quality if set).
        #[arg(long)]
        bitrate: Option<u32>,
    },
    /// Start a live streaming server (RTMP ingest → HLS output).
    Stream {
        /// RTMP bind address.
        #[arg(long, default_value = "0.0.0.0:1935")]
        rtmp_addr: String,
        /// HLS output directory.
        #[arg(long, default_value = "./hls_output")]
        hls_dir: String,
        /// HLS HTTP server bind address.
        #[arg(long, default_value = "0.0.0.0:8080")]
        http_addr: String,
        /// HLS segment duration in seconds.
        #[arg(long, default_value_t = 6)]
        segment_duration: u32,
        /// HLS playlist window size (number of segments).
        #[arg(long, default_value_t = 5)]
        window_size: usize,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Probe { input } => probe(&input),
        Commands::Transcode {
            input,
            output,
            vcodec,
            quality,
            speed,
            bitrate,
        } => transcode(&input, &output, &vcodec, quality, speed, bitrate),
        Commands::Stream {
            rtmp_addr,
            hls_dir,
            http_addr,
            segment_duration,
            window_size,
        } => stream(&rtmp_addr, &hls_dir, &http_addr, segment_duration, window_size).await,
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

/// Returns the [`tpt_kinetix_core::capabilities::DecoderCapabilities`] for a given
/// codec, if a decoder exists.
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

// ── Transcode ────────────────────────────────────────────────────────────────

fn transcode(
    input: &str,
    output: &str,
    vcodec: &str,
    quality: u8,
    speed: u8,
    bitrate: Option<u32>,
) -> Result<()> {
    let input_path = PathBuf::from(input);
    let data = std::fs::read(&input_path)
        .with_context(|| format!("failed to read input file: {input}"))?;

    let rate_control = match bitrate {
        Some(bps) => {
            tracing::info!(bps, "using bitrate mode");
            tpt_kinetix_core::encode::RateControl::Bitrate {
                bits_per_second: bps,
            }
        }
        None => tpt_kinetix_core::encode::RateControl::ConstantQuality { quantizer: quality },
    };

    match vcodec {
        "av1" => transcode_to_av1(&data, output, rate_control, speed),
        "h264" => transcode_to_h264(&data, output, rate_control, speed),
        _ => anyhow::bail!(
            "unsupported output codec '{vcodec}'. Supported: av1, h264"
        ),
    }
}

fn transcode_to_av1(
    data: &[u8],
    output: &str,
    rate_control: tpt_kinetix_core::encode::RateControl,
    speed: u8,
) -> Result<()> {
    tracing::info!(
        output,
        ?rate_control,
        speed,
        "starting transcode to AV1"
    );

    let config = tpt_kinetix_core::encode::EncodeConfig {
        width: 0,
        height: 0,
        rate_control,
        speed: tpt_kinetix_core::encode::SpeedPreset::Custom(speed),
        keyframe_interval: 240,
    };

    let (sink, packets) = tpt_kinetix_pipeline::PacketSinkStage::new();

    let pipeline = tpt_kinetix_pipeline::Pipeline::new()
        .add_stage(tpt_kinetix_pipeline::DemuxStage {
            data: data.to_vec(),
        })
        .add_stage(tpt_kinetix_pipeline::DecodeStage)
        .add_stage(tpt_kinetix_pipeline::EncodeStage::new(config))
        .add_stage(sink);

    tracing::info!("running transcode pipeline...");
    pipeline
        .run_to_completion()
        .map_err(|e| anyhow::anyhow!("pipeline error: {e}"))?;

    let packets = Arc::try_unwrap(packets)
        .expect("Arc still shared")
        .into_inner()
        .expect("mutex poisoned");

    tracing::info!(packet_count = packets.len(), "encoding complete");

    if packets.is_empty() {
        anyhow::bail!("no packets produced — input may not contain decodable video");
    }

    write_ivf(output, &packets)?;

    let total_bytes: usize = packets.iter().map(|p| p.size()).sum();
    tracing::info!(
        output,
        packets = packets.len(),
        total_bytes,
        "wrote AV1 output"
    );

    Ok(())
}

fn transcode_to_h264(
    data: &[u8],
    output: &str,
    rate_control: tpt_kinetix_core::encode::RateControl,
    speed: u8,
) -> Result<()> {
    tracing::info!(
        output,
        ?rate_control,
        speed,
        "starting transcode to H.264"
    );

    let config = tpt_kinetix_core::encode::EncodeConfig {
        width: 0,
        height: 0,
        rate_control,
        speed: tpt_kinetix_core::encode::SpeedPreset::Custom(speed),
        keyframe_interval: 240,
    };

    let (sink, packets) = tpt_kinetix_pipeline::PacketSinkStage::new();

    let pipeline = tpt_kinetix_pipeline::Pipeline::new()
        .add_stage(tpt_kinetix_pipeline::DemuxStage {
            data: data.to_vec(),
        })
        .add_stage(tpt_kinetix_pipeline::DecodeStage)
        .add_stage(tpt_kinetix_pipeline::EncodeStage::new(config))
        .add_stage(sink);

    tracing::info!("running transcode pipeline...");
    pipeline
        .run_to_completion()
        .map_err(|e| anyhow::anyhow!("pipeline error: {e}"))?;

    let packets = Arc::try_unwrap(packets)
        .expect("Arc still shared")
        .into_inner()
        .expect("mutex poisoned");

    tracing::info!(packet_count = packets.len(), "encoding complete");

    if packets.is_empty() {
        anyhow::bail!("no packets produced — input may not contain decodable video");
    }

    write_h264_mp4(output, &packets)?;

    let total_bytes: usize = packets.iter().map(|p| p.size()).sum();
    tracing::info!(
        output,
        packets = packets.len(),
        total_bytes,
        "wrote H.264 MP4 output"
    );

    Ok(())
}

fn write_ivf(path: &str, packets: &[tpt_kinetix_core::packet::Packet]) -> Result<()> {
    let mut out = Vec::new();

    let width = 1920u16;
    let height = 1080u16;
    let frame_rate_num = 30u32;
    let frame_rate_den = 1u32;

    out.extend_from_slice(b"DKIF");
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"AV01");
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&frame_rate_num.to_le_bytes());
    out.extend_from_slice(&frame_rate_den.to_le_bytes());
    out.extend_from_slice(&(packets.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    for pkt in packets {
        out.extend_from_slice(&(pkt.size() as u32).to_le_bytes());
        out.extend_from_slice(&(pkt.pts.value as u64).to_le_bytes());
        out.extend_from_slice(&pkt.data);
    }

    std::fs::write(path, &out)
        .with_context(|| format!("failed to write output file: {path}"))?;

    Ok(())
}

fn write_h264_mp4(path: &str, packets: &[tpt_kinetix_core::packet::Packet]) -> Result<()> {
    let width = 1920u16;
    let height = 1080u16;
    let timescale = 30_000u32;

    let mut muxer = tpt_kinetix_mux::Mp4Muxer::new(tpt_kinetix_mux::Mp4MuxerConfig {
        width,
        height,
        timescale,
        sps: vec![0x67, 0x42, 0x00, 0x1e],
        pps: vec![0x68, 0xce, 0x3c, 0x80],
    });

    for pkt in packets {
        muxer.write_sample(&pkt.data, 1000, pkt.is_key_frame);
    }

    let mp4 = muxer.finish();
    std::fs::write(path, &mp4)
        .with_context(|| format!("failed to write output file: {path}"))?;

    Ok(())
}

// ── Stream ───────────────────────────────────────────────────────────────────

async fn stream(
    rtmp_addr: &str,
    hls_dir: &str,
    http_addr: &str,
    segment_duration: u32,
    window_size: usize,
) -> Result<()> {
    tracing::info!(
        rtmp_addr,
        hls_dir,
        http_addr,
        segment_duration,
        window_size,
        "starting stream server"
    );

    let packager = Arc::new(std::sync::Mutex::new(tpt_kinetix_stream::HlsPackager::new(
        tpt_kinetix_stream::HlsConfig {
            segment_duration_secs: segment_duration,
            output_dir: hls_dir.to_string(),
            window_size,
            http_bind_addr: http_addr.to_string(),
        },
    )));

    let rtmp_server = tpt_kinetix_stream::RtmpServer::new(tpt_kinetix_stream::RtmpConfig {
        bind_addr: rtmp_addr.to_string(),
    });

    let packager_for_handler = Arc::clone(&packager);
    let rtmp_server = rtmp_server.with_handler(move |event| {
        handle_rtmp_event(&packager_for_handler, event);
    });

    let hls_http = tokio::spawn({
        let output_dir = hls_dir.to_string();
        let http_addr = http_addr.to_string();
        async move {
            let serving_config = tpt_kinetix_stream::HlsConfig {
                segment_duration_secs: segment_duration,
                output_dir,
                window_size,
                http_bind_addr: http_addr,
            };
            let packager = tpt_kinetix_stream::HlsPackager::new(serving_config);
            if let Err(e) = packager.serve().await {
                tracing::error!(error = %e, "HLS HTTP server error");
            }
        }
    });

    let rtmp = tokio::spawn(async move {
        if let Err(e) = rtmp_server.run().await {
            tracing::error!(error = %e, "RTMP server error");
        }
    });

    tracing::info!(
        rtmp_addr = rtmp_addr,
        hls_http = http_addr,
        "stream servers running — press Ctrl+C to stop"
    );

    tokio::select! {
        _ = hls_http => {},
        _ = rtmp => {},
    }

    Ok(())
}

fn handle_rtmp_event(
    packager: &Arc<std::sync::Mutex<tpt_kinetix_stream::HlsPackager>>,
    event: &tpt_kinetix_stream::rtmp::server::RtmpMediaEvent,
) {
    use tpt_kinetix_stream::rtmp::server::RtmpMediaEvent;

    match event {
        RtmpMediaEvent::PublishStart { stream_key } => {
            tracing::info!(stream_key, "RTMP publish started");
        }
        RtmpMediaEvent::Video { timestamp, tag } => {
            if tag.is_sequence_header() {
                tracing::debug!("received AVC sequence header (SPS/PPS)");
                return;
            }

            let pts_90khz = *timestamp as u64 * 90;
            let is_key = tag.frame_type.is_keyframe();
            let access_unit = tag.data.clone();

            tracing::debug!(
                timestamp = *timestamp,
                data_len = access_unit.len(),
                is_key,
                "received video access unit"
            );

            let mut p = packager.lock().expect("packager mutex poisoned");
            let access_units = vec![(access_unit, pts_90khz, is_key)];
            if let Err(e) = p.write_ts_segment(&access_units) {
                tracing::warn!(error = %e, "failed to write TS segment");
            }
        }
        RtmpMediaEvent::Audio { timestamp, tag } => {
            tracing::debug!(
                timestamp = *timestamp,
                is_sequence_header = tag.is_sequence_header(),
                "received audio (not yet muxed to HLS)"
            );
        }
        RtmpMediaEvent::PublishStop => {
            tracing::info!("RTMP publish stopped");
        }
    }
}
