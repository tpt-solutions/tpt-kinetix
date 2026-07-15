//! HLS packaging: fMP4/MPEG-TS segment generation and `.m3u8` playlist output.
//!
//! TODO (Phase 6): Implement segment writer, playlist rolling window, and
//! minimal HTTP server for serving segments.

use kinetix_core::error::KinetixError;

/// Configuration for the HLS packager.
#[derive(Debug, Clone)]
pub struct HlsConfig {
    /// Target segment duration in seconds.
    pub segment_duration_secs: u32,
    /// Directory to write segments and playlists to.
    pub output_dir: String,
    /// Number of segments to keep in the live playlist window.
    pub window_size: usize,
}

impl Default for HlsConfig {
    fn default() -> Self {
        Self {
            segment_duration_secs: 6,
            output_dir: "./hls_output".into(),
            window_size: 5,
        }
    }
}

/// HLS packager state.
pub struct HlsPackager {
    config: HlsConfig,
    segment_index: u64,
}

impl HlsPackager {
    pub fn new(config: HlsConfig) -> Self {
        Self { config, segment_index: 0 }
    }

    /// Write the current batch of packets as the next HLS segment and update
    /// the `.m3u8` playlist.
    ///
    /// TODO (Phase 6): Implement fMP4 segment muxing and playlist generation.
    pub fn write_segment(&mut self) -> Result<(), KinetixError> {
        let _ = &self.config;
        self.segment_index += 1;
        Err(KinetixError::Unsupported(
            "HLS packager not yet implemented (Phase 6)".into(),
        ))
    }

    pub fn segment_index(&self) -> u64 {
        self.segment_index
    }
}
