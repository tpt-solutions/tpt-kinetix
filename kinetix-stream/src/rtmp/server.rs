//! RTMP ingest server — accepts TCP connections, performs the handshake, and
//! reads incoming chunk stream data.

use tokio::net::TcpListener;

use super::handshake::perform_server_handshake;

/// Configuration for the RTMP ingest server.
#[derive(Debug, Clone)]
pub struct RtmpConfig {
    /// Address to bind on, e.g. `"0.0.0.0:1935"`.
    pub bind_addr: String,
}

impl Default for RtmpConfig {
    fn default() -> Self {
        Self { bind_addr: "0.0.0.0:1935".into() }
    }
}

/// An RTMP ingest server.
pub struct RtmpServer {
    config: RtmpConfig,
}

impl RtmpServer {
    /// Create a new server with the given configuration.
    pub fn new(config: RtmpConfig) -> Self {
        Self { config }
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
            tokio::spawn(async move {
                if let Err(e) = handle_connection(&mut stream).await {
                    tracing::warn!(%peer_addr, error = %e, "RTMP connection error");
                }
            });
        }
    }
}

/// Handle a single RTMP client connection.
async fn handle_connection(stream: &mut tokio::net::TcpStream) -> anyhow::Result<()> {
    // 1. RTMP handshake
    perform_server_handshake(stream).await?;
    tracing::info!("RTMP handshake complete");

    // 2. Read chunk stream (stub — log message type IDs without full AMF decode)
    // TODO (Phase 7): full AMF0 command parsing for connect/publish.
    use tokio::io::AsyncReadExt;
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            tracing::info!("RTMP client disconnected");
            break;
        }
        // Log the first byte so the compiler doesn't optimise the read away.
        tracing::debug!(first_byte = buf[0], bytes_read = n, "RTMP chunk data");
    }

    Ok(())
}
