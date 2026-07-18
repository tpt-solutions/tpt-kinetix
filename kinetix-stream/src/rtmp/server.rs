//! RTMP ingest server — accepts TCP connections, performs the handshake,
//! reassembles the chunk stream into messages, and forwards media messages to a
//! caller-supplied handler.

use std::sync::Arc;

use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

use super::chunk::{ChunkAssembler, MessageTypeId, RtmpMessage};
use super::handshake::perform_server_handshake;

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

/// A handler invoked for every reassembled RTMP message on a connection.
///
/// This is the bridge point into downstream processing (e.g. feeding audio /
/// video payloads into `kinetix-pipeline`). Handlers must be cheap and
/// `Send + Sync` because a clone is shared across all connection tasks.
pub type MessageHandler = Arc<dyn Fn(&RtmpMessage) + Send + Sync>;

/// An RTMP ingest server.
pub struct RtmpServer {
    config: RtmpConfig,
    handler: Option<MessageHandler>,
}

impl RtmpServer {
    /// Create a new server with the given configuration.
    pub fn new(config: RtmpConfig) -> Self {
        Self {
            config,
            handler: None,
        }
    }

    /// Register a handler that receives every reassembled RTMP message.
    ///
    /// Typically used to forward `Audio`/`Video` payloads into a processing
    /// pipeline.
    pub fn with_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(&RtmpMessage) + Send + Sync + 'static,
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

/// Handle a single RTMP client connection: handshake, then reassemble chunks
/// into messages and dispatch them to `handler`.
async fn handle_connection(
    stream: &mut TcpStream,
    handler: Option<MessageHandler>,
) -> anyhow::Result<()> {
    // 1. RTMP handshake
    perform_server_handshake(stream).await?;
    tracing::info!("RTMP handshake complete");

    // 2. Reassemble the chunk stream into messages.
    let mut assembler = ChunkAssembler::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            tracing::info!("RTMP client disconnected");
            break;
        }
        for msg in assembler.push(&buf[..n]) {
            // Honour SetChunkSize so subsequent reassembly stays in sync.
            if MessageTypeId::from_u8(msg.message_type_id) == Some(MessageTypeId::SetChunkSize) {
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
                continue;
            }

            tracing::debug!(
                type_id = msg.message_type_id,
                len = msg.payload.len(),
                "RTMP message"
            );
            if let Some(h) = handler.as_ref() {
                h(&msg);
            }
        }
    }

    Ok(())
}
