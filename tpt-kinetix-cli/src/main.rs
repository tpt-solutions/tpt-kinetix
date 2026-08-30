//! `tpt-kinetix` — command-line interface for the TPT Kinetix media engine.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tpt_kinetix_vision::{VisionDecoder, VisionDecoderImpl};

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
    /// Decode a Vision (video-for-machines) bitstream and reconstruct frames.
    ///
    /// The Vision codec's slow path (`decode_pixels`) runs the full
    /// reconstruction pipeline (intra/inter prediction, inverse transform,
    /// deblocking) and writes each frame as a PPM. The `--tensor` flag uses the
    /// fast path (`decode_tensor`) instead, printing feature-tensor stats.
    ///
    /// The input format is the raw `tpt-kinetix-vision` framing: a 16-byte
    /// sequence header followed by one or more frame packets, each a 15-byte
    /// frame header immediately followed by `payload_len` bytes of rANS payload.
    /// Use `--demo` to skip the file and run a self-contained
    /// synthesize → encode → reconstruct round-trip (no external encoder needed).
    Vision {
        /// Input vision bitstream file. Ignored when `--demo` is set.
        input: Option<PathBuf>,
        /// Output base path for reconstructed PPM frames (`<base>-<n>.ppm`).
        /// Ignored when `--tensor` is set.
        #[arg(short, long, default_value = "vision_out")]
        output: PathBuf,
        /// Use the fast tensor path instead of full pixel reconstruction.
        #[arg(long)]
        tensor: bool,
        /// Run a self-contained synthesize → encode → reconstruct demo and write
        /// the reconstructed frame (ignores `input`).
        #[arg(long)]
        demo: bool,
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
        } => {
            stream(
                &rtmp_addr,
                &hls_dir,
                &http_addr,
                segment_duration,
                window_size,
            )
            .await
        }
        Commands::Vision {
            input,
            output,
            tensor,
            demo,
        } => vision(input, &output, tensor, demo),
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

    // Probe the input once to learn the real video geometry and frame rate so the
    // output container carries correct dimensions and timing (the pipeline
    // re-derives width/height from the first decoded frame, but the muxer header
    // needs them up front).
    let (width, height, fps_num, fps_den) =
        input_video_geometry(&data).unwrap_or((1920, 1080, 30, 1));
    tracing::info!(width, height, fps_num, fps_den, "probed input video track");

    let rate_control = match bitrate {
        Some(bps) => {
            tracing::info!(bps, "using bitrate mode");
            tpt_kinetix_core::encode::RateControl::Bitrate {
                bits_per_second: bps,
            }
        }
        None => tpt_kinetix_core::encode::RateControl::ConstantQuality { quantizer: quality },
    };

    let geometry = VideoGeometry {
        width,
        height,
        fps_num,
        fps_den,
    };

    match vcodec {
        "av1" => transcode_to_av1(&data, output, rate_control, speed, geometry),
        "h264" => transcode_to_h264(&data, output, rate_control, speed, width, height),
        _ => anyhow::bail!("unsupported output codec '{vcodec}'. Supported: av1, h264"),
    }
}

/// Probe the input MP4 for its first video track's geometry and frame rate.
///
/// Returns `(width, height, fps_num, fps_den)`. The frame rate is derived from
/// the track's sample count and duration scaled to a denominator of 1000, so the
/// values are always finite and never divide by zero.
fn input_video_geometry(data: &[u8]) -> Option<(u32, u32, u32, u32)> {
    use tpt_kinetix_core::codec::MediaType;

    let demuxer = tpt_kinetix_demux::Mp4Demuxer::new(data.to_vec()).ok()?;
    let track = demuxer
        .tracks()
        .iter()
        .find(|t| t.media_type == MediaType::Video)?;

    if track.width == 0 || track.height == 0 {
        return None;
    }

    let samples = track.sample_count().max(1) as f64;
    let duration_secs = (track.duration as f64 / track.timescale.max(1) as f64).max(f64::EPSILON);
    let fps = samples / duration_secs;
    let fps_num = (fps * 1000.0).round().max(1.0) as u32;

    Some((track.width, track.height, fps_num, 1000))
}

/// Geometry and timing of the input video track, used to size the output.
struct VideoGeometry {
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
}

fn transcode_to_av1(
    data: &[u8],
    output: &str,
    rate_control: tpt_kinetix_core::encode::RateControl,
    speed: u8,
    geometry: VideoGeometry,
) -> Result<()> {
    tracing::info!(output, ?rate_control, speed, "starting transcode to AV1");

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

    write_ivf(
        output,
        &packets,
        geometry.width,
        geometry.height,
        geometry.fps_num,
        geometry.fps_den,
    )?;

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
    _data: &[u8],
    output: &str,
    _rate_control: tpt_kinetix_core::encode::RateControl,
    _speed: u8,
    width: u32,
    height: u32,
) -> Result<()> {
    // The workspace currently ships no H.264 *encoder* — only a decoder and the
    // `rav1e`-backed AV1 encoder. Transcoding to H.264 would require re-encoding
    // the decoded frames, which is unsupported; fail clearly instead of producing
    // a corrupt container with placeholder SPS/PPS.
    anyhow::bail!(
        "transcode to H.264 is not yet supported (no H.264 encoder in this build); \
         use --vcodec av1. Requested output {} ({}x{})",
        output,
        width,
        height
    );
}

fn write_ivf(
    path: &str,
    packets: &[tpt_kinetix_core::packet::Packet],
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
) -> Result<()> {
    let mut out = Vec::new();

    out.extend_from_slice(b"DKIF");
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"AV01");
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&fps_num.to_le_bytes());
    out.extend_from_slice(&fps_den.to_le_bytes());
    out.extend_from_slice(&(packets.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    for (i, pkt) in packets.iter().enumerate() {
        out.extend_from_slice(&(pkt.size() as u32).to_le_bytes());
        // IVF timestamps are in the stream's timebase (1/fps here), so the
        // frame index is the correct presentation timestamp.
        out.extend_from_slice(&(i as u64).to_le_bytes());
        out.extend_from_slice(&pkt.data);
    }

    std::fs::write(path, &out).with_context(|| format!("failed to write output file: {path}"))?;

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
            if let Err(e) = p.push_access_unit(access_unit, pts_90khz, is_key) {
                tracing::warn!(error = %e, "failed to mux HLS segment");
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
            tracing::info!("RTMP publish stopped; flushing remaining HLS segments");
            let mut p = packager.lock().expect("packager mutex poisoned");
            if let Err(e) = p.flush() {
                tracing::warn!(error = %e, "failed to flush HLS segments");
            }
        }
    }
}

// ── Vision ────────────────────────────────────────────────────────────────────

/// Decode a Vision bitstream and reconstruct its frames.
///
/// Exposes the vision codec's full reconstruction path (`decode_pixels`, the slow
/// path) through the CLI "shell" so the codec is runnable end-to-end without the
/// transcoding pipeline. With `--tensor`, the fast feature-tensor path
/// (`decode_tensor`) is used instead and only tensor stats are printed.
///
/// With `--demo`, a synthetic frame is synthesized in memory, encoded with the
/// vision encoder, then reconstructed — a self-contained round-trip that needs no
/// external encoder or input file.
fn vision(
    input: Option<PathBuf>,
    output: &std::path::Path,
    tensor: bool,
    demo: bool,
) -> Result<()> {
    use tpt_kinetix_bitstream::BitReader;
    use tpt_kinetix_core::packet::Packet;
    use tpt_kinetix_vision::{
        FrameHeader, FrameType, SequenceHeader, VisionDecoder, VisionDecoderImpl,
    };

    let mut decoder = VisionDecoderImpl::new();

    if demo {
        return vision_demo(&mut decoder, output);
    }

    let input = input
        .ok_or_else(|| anyhow::anyhow!("vision: --input is required unless --demo is given"))?;
    let data = std::fs::read(&input)
        .with_context(|| format!("failed to read input file: {}", input.display()))?;

    if data.len() < 16 {
        anyhow::bail!("vision: input too short for a 16-byte sequence header");
    }
    let mut seq_reader = BitReader::new(&data[..16]);
    let sequence = SequenceHeader::parse(&mut seq_reader)
        .map_err(|e| anyhow::anyhow!("vision: bad sequence header: {e}"))?;
    decoder.set_sequence_header(sequence);

    let mut pos = 16usize;
    let mut frame_idx = 0usize;
    let mut stats = Vec::new();
    while pos + 15 <= data.len() {
        let mut fh_reader = BitReader::new(&data[pos..pos + 15]);
        let frame_header = FrameHeader::parse(&mut fh_reader, &sequence)
            .map_err(|e| anyhow::anyhow!("vision: bad frame header at byte {pos}: {e}"))?;
        let payload_len = frame_header.payload_len as usize;
        let payload_start = pos + 15;
        let payload_end = payload_start + payload_len;
        if payload_end > data.len() {
            anyhow::bail!(
                "vision: frame #{frame_idx} payload_len {payload_len} runs past end of file"
            );
        }
        let mut packet_data = Vec::with_capacity(15 + payload_len);
        packet_data.extend_from_slice(&data[pos..pos + 15]);
        packet_data.extend_from_slice(&data[payload_start..payload_end]);

        if tensor {
            let pkt = Packet {
                pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
                dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
                data: packet_data,
                stream_index: 0,
                is_key_frame: frame_header.frame_type == FrameType::Key,
            };
            let t = decoder
                .decode_tensor(&pkt)
                .map_err(|e| anyhow::anyhow!("vision: decode_tensor failed: {e}"))?
                .ok_or_else(|| anyhow::anyhow!("vision: decode_tensor returned no tensor"))?;
            println!(
                "frame #{frame_idx}: tensor shape [{}, {}, {}] stride {} ({} values)",
                t.c(),
                t.h(),
                t.width(),
                t.stride,
                t.len()
            );
        } else {
            let pkt = Packet {
                pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
                dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
                data: packet_data,
                stream_index: 0,
                is_key_frame: frame_header.frame_type == FrameType::Key,
            };
            let vf = decoder
                .decode_pixels(&pkt)
                .map_err(|e| anyhow::anyhow!("vision: decode_pixels failed: {e}"))?
                .ok_or_else(|| anyhow::anyhow!("vision: decode_pixels returned no frame"))?;
            let path = output.with_extension(format!("frame-{frame_idx:04}.ppm"));
            write_frame_ppm(&path, &vf)?;
            stats.push((frame_idx, vf.width, vf.height));
        }

        pos = payload_end;
        frame_idx += 1;
    }

    if frame_idx == 0 {
        anyhow::bail!("vision: no frames found in input");
    }
    if tensor {
        println!("vision: decoded {frame_idx} frame tensor(s)");
    } else {
        for (i, w, h) in stats {
            println!("vision: wrote frame #{i} ({w}x{h})");
        }
    }
    Ok(())
}

/// Self-contained synthesize → encode → reconstruct round-trip demonstrating the
/// vision codec's reconstruction through the CLI shell.
fn vision_demo(decoder: &mut VisionDecoderImpl, output: &std::path::Path) -> Result<()> {
    use tpt_kinetix_vision::{encode_frame, FrameBuffer, FrameHeader, FrameType, SequenceHeader};

    let size = 64u16;
    let sequence = SequenceHeader {
        version: 1,
        max_width: size,
        max_height: size,
        chroma_present: true,
        bit_depth: 8,
        qp_precision: 0,
        max_ref_frames: 1,
        num_rans_streams: 1,
        min_block_size_log2: 3,
        max_block_size_log2: 3,
        quant_matrix_id: 0,
    };
    let frame = FrameHeader {
        frame_type: FrameType::Key,
        width: size,
        height: size,
        base_qp: 0,
        ref_frame_count: 0,
        output_mode: 2,
        payload_len: 0,
    };

    let n = size as usize;
    let mut luma = vec![0u8; n * n];
    let mut cb = vec![0u8; (n / 2) * (n / 2)];
    let mut cr = vec![0u8; (n / 2) * (n / 2)];
    for y in 0..n {
        for x in 0..n {
            luma[y * n + x] = ((x + y) * 2) as u8;
            let (cx, cy) = (x / 2, y / 2);
            cb[cy * (n / 2) + cx] = (x * 2) as u8;
            cr[cy * (n / 2) + cx] = (y * 2) as u8;
        }
    }

    let src = FrameBuffer::from_yuv420(size as u32, size as u32, luma, cb, cr)
        .map_err(|e| anyhow::anyhow!("vision demo: bad source buffer: {e}"))?;
    let payload = encode_frame(&sequence, &frame, &src, None)
        .map_err(|e| anyhow::anyhow!("vision demo: encode failed: {e}"))?;

    // The frame header must advertise the real rANS payload length so any reader
    // (including the file path below) can frame the packet correctly.
    let frame = FrameHeader {
        payload_len: payload.len() as u32,
        ..frame
    };

    let header_bytes = frame.to_bytes();
    let mut packet_data = Vec::with_capacity(header_bytes.len() + payload.len());
    packet_data.extend_from_slice(&header_bytes);
    packet_data.extend_from_slice(&payload);

    // Persist the raw stream (sequence header + frame packet) so it can be fed
    // back through the file-read path (`vision --input`) as a round-trip check.
    let mut raw_stream = Vec::with_capacity(16 + packet_data.len());
    raw_stream.extend_from_slice(&sequence.to_bytes());
    raw_stream.extend_from_slice(&packet_data);
    let dump_path = output.with_extension("vision");
    std::fs::write(&dump_path, &raw_stream)
        .with_context(|| format!("failed to write {}", dump_path.display()))?;

    decoder.set_sequence_header(sequence);

    let pkt = tpt_kinetix_core::packet::Packet {
        pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        data: packet_data,
        stream_index: 0,
        is_key_frame: true,
    };

    let vf = decoder
        .decode_pixels(&pkt)
        .map_err(|e| anyhow::anyhow!("vision demo: decode_pixels failed: {e}"))?
        .ok_or_else(|| anyhow::anyhow!("vision demo: decode_pixels returned no frame"))?;

    let path = output.with_extension("ppm");
    write_frame_ppm(&path, &vf)?;
    println!(
        "vision demo: reconstructed {}x{} frame -> {} (raw stream: {})",
        vf.width,
        vf.height,
        path.display(),
        dump_path.display()
    );
    Ok(())
}

/// Write a `VideoFrame` (YUV420p planar) as a P6 PPM (RGB) file.
fn write_frame_ppm(path: &std::path::Path, vf: &tpt_kinetix_core::frame::VideoFrame) -> Result<()> {
    let w = vf.width as usize;
    let h = vf.height as usize;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let expected = w * h + 2 * cw * ch;
    if vf.data.len() < expected {
        anyhow::bail!(
            "vision: frame data {} bytes < expected {} for {}x{} YUV420p",
            vf.data.len(),
            expected,
            w,
            h
        );
    }
    let luma = &vf.data[..w * h];
    let cb = &vf.data[w * h..w * h + cw * ch];
    let cr = &vf.data[w * h + cw * ch..w * h + 2 * cw * ch];

    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for y in 0..h {
        for x in 0..w {
            let yv = luma[y * w + x] as f32;
            let (cx, cy) = ((x >> 1).min(cw - 1), (y >> 1).min(ch - 1));
            let cbp = cb[cy * cw + cx] as f32 - 128.0;
            let crp = cr[cy * cw + cx] as f32 - 128.0;
            let r = (yv + 1.402 * crp).clamp(0.0, 255.0);
            let g = (yv - 0.344136 * cbp - 0.714136 * crp).clamp(0.0, 255.0);
            let b = (yv + 1.772 * cbp).clamp(0.0, 255.0);
            out.push(r as u8);
            out.push(g as u8);
            out.push(b as u8);
        }
    }
    std::fs::write(path, &out).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}
