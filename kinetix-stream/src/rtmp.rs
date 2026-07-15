//! RTMP ingest server.
//!
//! TODO (Phase 6): Implement RTMP handshake, chunk stream parsing, and live
//! stream ingestion feeding into `kinetix-pipeline`.

use kinetix_core::error::KinetixError;

/// Configuration for the RTMP ingest server.
#[derive(Debug, Clone)]
pub struct RtmpConfig {
    /// Address to bind the RTMP server on, e.g. `"0.0.0.0:1935"`.
    pub bind_addr: String,
}

impl Default for RtmpConfig {
    fn default() -> Self {
        Self { bind_addr: "0.0.0.0:1935".into() }
    }
}

/// An RTMP ingest server handle.
pub struct RtmpServer {
    config: RtmpConfig,
}

impl RtmpServer {
    pub fn new(config: RtmpConfig) -> Self {
        Self { config }
    }

    /// Start listening for RTMP push connections.
    ///
    /// TODO (Phase 6): Implement on top of `tokio::net::TcpListener`.
    pub async fn run(&self) -> Result<(), KinetixError> {
        let _ = &self.config;
        Err(KinetixError::Unsupported(
            "RTMP server not yet implemented (Phase 6)".into(),
        ))
    }
}
