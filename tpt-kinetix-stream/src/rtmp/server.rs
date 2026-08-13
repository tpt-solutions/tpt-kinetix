//! RTMP ingest server — accepts TCP connections, performs the handshake,
//! completes the AMF0 `connect`/`createStream`/`publish` negotiation, reassembles
//! the chunk stream into messages, depacketizes FLV audio/video, and forwards
//! high-level media events to a caller-supplied handler.

use std::sync::Arc;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

use super::{
    amf::{self, Amf0Value},
    chunk::{ChunkAssembler, MessageTypeId, RtmpMessage},
    flv::{self, FlvAudioTag, FlvVideoTag},
    handshake::perform_server_handshake,
};

/// Configuration for the RTMP ingest server.
#[derive(Debug, Clone)]
pub struct RtmpConfig {
    /// Address to bind on, e.g. `"0.0.0.0:1935"`.
    pub bind_addr: String,
}

impl Default for RtmpConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:1935".into(),
        }
    }
}

/// A high-level media event emitted after AMF negotiation and FLV
/// depacketization.
///
/// This is the bridge point into downstream processing (e.g. feeding audio /
/// video payloads into `tpt-kinetix-pipeline`).
#[derive(Debug, Clone)]
pub enum RtmpMediaEvent {
    /// The client issued `publish` for the given stream key.
    PublishStart {
        /// The stream key / name requested by the publisher.
        stream_key: String,
    },
    /// A depacketized video tag (SPS/PPS sequence header or coded NALUs).
    Video {
        /// Message timestamp in milliseconds.
        timestamp: u32,
        /// The parsed FLV video tag.
        tag: FlvVideoTag,
    },
    /// A depacketized audio tag (AudioSpecificConfig or coded frames).
    Audio {
        /// Message timestamp in milliseconds.
        timestamp: u32,
        /// The parsed FLV audio tag.
        tag: FlvAudioTag,
    },
    /// The publisher stopped or disconnected.
    PublishStop,
}

/// A handler invoked for every high-level media event on a connection.
///
/// Handlers must be cheap and `Send + Sync` because a clone is shared across all
/// connection tasks.
pub type MediaHandler = Arc<dyn Fn(&RtmpMediaEvent) + Send + Sync>;

/// An RTMP ingest server.
pub struct RtmpServer {
    config: RtmpConfig,
    handler: Option<MediaHandler>,
}

impl RtmpServer {
    /// Create a new server with the given configuration.
    pub fn new(config: RtmpConfig) -> Self {
        Self {
            config,
            handler: None,
        }
    }

    /// Register a handler that receives every high-level media event.
    pub fn with_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(&RtmpMediaEvent) + Send + Sync + 'static,
    {
        self.handler = Some(Arc::new(handler));
        self
    }

    /// Bind and start accepting RTMP connections.
    ///
    /// Each accepted connection is spawned into its own Tokio task. The future
    /// returned by this method runs forever (or until an accept error occurs).
    pub async fn run(&self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(&self.config.bind_addr).await?;
        tracing::info!(addr = %self.config.bind_addr, "RTMP server listening");
        loop {
            let (mut stream, peer_addr) = listener.accept().await?;
            tracing::info!(%peer_addr, "RTMP client connected");
            let handler = self.handler.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection(&mut stream, handler).await {
                    // A dropped/reset connection is expected and recovered by
                    // simply ending this task; the listener keeps accepting.
                    tracing::warn!(%peer_addr, error = %e, "RTMP connection ended");
                }
            });
        }
    }
}

/// Chunk stream ids we use when writing responses.
const CSID_PROTOCOL: u32 = 2;
const CSID_COMMAND: u32 = 3;

/// Serialize a single Type-0 RTMP chunk carrying a whole message.
///
/// `payload` must be `<= chunk_size`; for the small control/command messages we
/// emit here this always holds against the default 128-byte (or larger) size.
fn write_message(
    csid: u32,
    type_id: u8,
    stream_id: u32,
    timestamp: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + payload.len());
    // Basic header: fmt=0, csid (assume csid < 64).
    out.push((csid & 0x3F) as u8);
    // Message header (11 bytes).
    let ts = timestamp & 0x00FF_FFFF;
    out.extend_from_slice(&ts.to_be_bytes()[1..]); // 3-byte timestamp
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]); // 3-byte length
    out.push(type_id);
    out.extend_from_slice(&stream_id.to_le_bytes()); // 4-byte LE stream id
    out.extend_from_slice(payload);
    out
}

fn window_ack_size(size: u32) -> Vec<u8> {
    write_message(
        CSID_PROTOCOL,
        MessageTypeId::WindowAckSize as u8,
        0,
        0,
        &size.to_be_bytes(),
    )
}

fn set_peer_bandwidth(size: u32, limit_type: u8) -> Vec<u8> {
    let mut body = size.to_be_bytes().to_vec();
    body.push(limit_type);
    write_message(
        CSID_PROTOCOL,
        MessageTypeId::SetPeerBandwidth as u8,
        0,
        0,
        &body,
    )
}

fn set_chunk_size(size: u32) -> Vec<u8> {
    write_message(
        CSID_PROTOCOL,
        MessageTypeId::SetChunkSize as u8,
        0,
        0,
        &size.to_be_bytes(),
    )
}

fn command(values: &[Amf0Value]) -> Vec<u8> {
    let body = amf::encode_all(values);
    write_message(CSID_COMMAND, MessageTypeId::CommandAmf0 as u8, 0, 0, &body)
}

/// The `_result` reply to `connect`.
fn connect_result(transaction_id: f64) -> Vec<u8> {
    command(&[
        Amf0Value::String("_result".into()),
        Amf0Value::Number(transaction_id),
        Amf0Value::Object(vec![
            ("fmsVer".into(), Amf0Value::String("FMS/3,0,1,123".into())),
            ("capabilities".into(), Amf0Value::Number(31.0)),
        ]),
        Amf0Value::Object(vec![
            ("level".into(), Amf0Value::String("status".into())),
            (
                "code".into(),
                Amf0Value::String("NetConnection.Connect.Success".into()),
            ),
            (
                "description".into(),
                Amf0Value::String("Connection succeeded.".into()),
            ),
        ]),
    ])
}

/// The `_result` reply to `createStream`, returning a stream id.
fn create_stream_result(transaction_id: f64, stream_id: f64) -> Vec<u8> {
    command(&[
        Amf0Value::String("_result".into()),
        Amf0Value::Number(transaction_id),
        Amf0Value::Null,
        Amf0Value::Number(stream_id),
    ])
}

/// The `onStatus` reply confirming `publish`.
fn publish_start_status() -> Vec<u8> {
    command(&[
        Amf0Value::String("onStatus".into()),
        Amf0Value::Number(0.0),
        Amf0Value::Null,
        Amf0Value::Object(vec![
            ("level".into(), Amf0Value::String("status".into())),
            (
                "code".into(),
                Amf0Value::String("NetStream.Publish.Start".into()),
            ),
            (
                "description".into(),
                Amf0Value::String("Publishing started.".into()),
            ),
        ]),
    ])
}

/// Handle a single RTMP client connection.
async fn handle_connection(
    stream: &mut TcpStream,
    handler: Option<MediaHandler>,
) -> anyhow::Result<()> {
    // 1. RTMP handshake.
    perform_server_handshake(stream).await?;
    tracing::info!("RTMP handshake complete");

    let emit = |event: RtmpMediaEvent| {
        if let Some(h) = handler.as_ref() {
            h(&event);
        }
    };

    // 2. Reassemble the chunk stream into messages and negotiate.
    let mut assembler = ChunkAssembler::new();
    let mut created_stream = false;
    let mut buf = [0u8; 8192];

    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            tracing::info!("RTMP client disconnected");
            emit(RtmpMediaEvent::PublishStop);
            break;
        }

        for msg in assembler.push(&buf[..n]) {
            match MessageTypeId::from_u8(msg.message_type_id) {
                Some(MessageTypeId::SetChunkSize) => {
                    if msg.payload.len() >= 4 {
                        let size = u32::from_be_bytes([
                            msg.payload[0],
                            msg.payload[1],
                            msg.payload[2],
                            msg.payload[3],
                        ]);
                        assembler.set_chunk_size(size);
                        tracing::debug!(size, "RTMP chunk size updated");
                    }
                }
                Some(MessageTypeId::CommandAmf0) => {
                    handle_command(stream, &msg, &mut created_stream, &emit).await?;
                }
                Some(MessageTypeId::Video) => match flv::parse_video_tag(&msg.payload) {
                    Ok(tag) => emit(RtmpMediaEvent::Video {
                        timestamp: msg.timestamp,
                        tag,
                    }),
                    Err(e) => tracing::warn!(error = %e, "bad FLV video tag"),
                },
                Some(MessageTypeId::Audio) => match flv::parse_audio_tag(&msg.payload) {
                    Ok(tag) => emit(RtmpMediaEvent::Audio {
                        timestamp: msg.timestamp,
                        tag,
                    }),
                    Err(e) => tracing::warn!(error = %e, "bad FLV audio tag"),
                },
                _ => {
                    tracing::trace!(type_id = msg.message_type_id, "ignoring RTMP message");
                }
            }
        }
    }

    Ok(())
}

/// Process a single AMF0 command message and send the appropriate response.
async fn handle_command(
    stream: &mut TcpStream,
    msg: &RtmpMessage,
    created_stream: &mut bool,
    emit: &impl Fn(RtmpMediaEvent),
) -> anyhow::Result<()> {
    let values = match amf::decode_all(&msg.payload) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "failed to decode AMF0 command");
            return Ok(());
        }
    };

    let command_name = values.first().and_then(|v| v.as_str()).unwrap_or("");
    let transaction_id = values.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);

    match command_name {
        "connect" => {
            tracing::info!("RTMP connect");
            // Standard control message sequence, then _result.
            stream.write_all(&window_ack_size(2_500_000)).await?;
            stream.write_all(&set_peer_bandwidth(2_500_000, 2)).await?;
            stream.write_all(&set_chunk_size(4096)).await?;
            stream.write_all(&connect_result(transaction_id)).await?;
            stream.flush().await?;
        }
        "createStream" => {
            tracing::info!("RTMP createStream");
            *created_stream = true;
            stream
                .write_all(&create_stream_result(transaction_id, 1.0))
                .await?;
            stream.flush().await?;
        }
        "publish" => {
            // publish(transaction, null, streamKey, publishType)
            let stream_key = values
                .get(3)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            tracing::info!(%stream_key, "RTMP publish");
            stream.write_all(&publish_start_status()).await?;
            stream.flush().await?;
            emit(RtmpMediaEvent::PublishStart { stream_key });
        }
        "deleteStream" | "FCUnpublish" | "closeStream" => {
            tracing::info!(command_name, "RTMP publish teardown");
            emit(RtmpMediaEvent::PublishStop);
        }
        "releaseStream" | "FCPublish" | "_checkbw" => {
            // Acknowledge with an empty _result so common encoders proceed.
            stream
                .write_all(&command(&[
                    Amf0Value::String("_result".into()),
                    Amf0Value::Number(transaction_id),
                    Amf0Value::Null,
                    Amf0Value::Null,
                ]))
                .await?;
            stream.flush().await?;
        }
        other => {
            tracing::debug!(command = other, "unhandled RTMP command");
        }
    }

    Ok(())
}
