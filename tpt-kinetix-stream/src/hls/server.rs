//! HLS packager and minimal tokio-based HTTP server.

use std::path::{Path, PathBuf};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::{playlist::HlsPlaylist, segment::HlsSegment, ts::TsMuxer};

/// Configuration for the HLS packager and its HTTP server.
#[derive(Debug, Clone)]
pub struct HlsConfig {
    /// Target segment duration in seconds.
    pub segment_duration_secs: u32,
    /// Directory where segments and playlists are written.
    pub output_dir: String,
    /// Number of segments retained in the live playlist window.
    pub window_size: usize,
    /// Address the HTTP server binds to.
    pub http_bind_addr: String,
}

impl Default for HlsConfig {
    fn default() -> Self {
        Self {
            segment_duration_secs: 6,
            output_dir: "./hls_output".into(),
            window_size: 5,
            http_bind_addr: "0.0.0.0:8080".into(),
        }
    }
}

/// Stateful HLS packager: writes `.ts` segment files and maintains the live
/// `.m3u8` playlist.
pub struct HlsPackager {
    config: HlsConfig,
    playlist: HlsPlaylist,
    next_index: u64,
    /// Access units buffered for the segment currently being assembled.
    pending: Vec<(Vec<u8>, u64, bool)>,
    /// 90 kHz PTS of the first access unit in the pending segment.
    segment_start_pts: Option<u64>,
}

impl HlsPackager {
    /// Create a new packager with the given configuration.
    pub fn new(config: HlsConfig) -> Self {
        let playlist = HlsPlaylist::new(config.segment_duration_secs, config.window_size);
        Self {
            config,
            playlist,
            next_index: 0,
            pending: Vec::new(),
            segment_start_pts: None,
        }
    }

    /// Write `data` as the next segment file, add it to the playlist, and
    /// atomically update `playlist.m3u8` in the output directory.
    pub fn write_segment(&mut self, data: &[u8]) -> anyhow::Result<()> {
        let out_dir = Path::new(&self.config.output_dir);
        std::fs::create_dir_all(out_dir)?;

        let index = self.next_index;
        self.next_index += 1;

        let filename = HlsSegment::filename(index);
        let seg_path = out_dir.join(&filename);
        std::fs::write(&seg_path, data)?;

        // Estimate duration from the config target (real muxers compute it from PTS).
        let duration_secs = f64::from(self.config.segment_duration_secs);
        let seg = HlsSegment {
            index,
            duration_secs,
            path: seg_path,
            byte_range: None,
        };

        self.playlist.add_segment(seg);

        // Atomically update the playlist file.
        let playlist_path = out_dir.join("playlist.m3u8");
        self.playlist.render_to_file(&playlist_path)?;

        Ok(())
    }

    /// Return a reference to the current playlist state.
    pub fn playlist(&self) -> &HlsPlaylist {
        &self.playlist
    }

    /// Append one H.264 access unit (AVCC form) to the in-progress segment.
    ///
    /// Each element is `(avcc_bytes, pts_90khz, is_keyframe)`. The buffered
    /// access units are muxed into a `.ts` segment and written via
    /// [`HlsPackager::write_ts_segment`] when either:
    /// - a new keyframe arrives after a non-empty segment (a natural GOP
    ///   boundary), or
    /// - the accumulated duration (in 90 kHz ticks) reaches
    ///   `segment_duration_secs`.
    ///
    /// This honours `segment_duration_secs` instead of emitting one tiny segment
    /// per access unit. Call [`HlsPackager::flush`] at end-of-stream to write out
    /// any remaining buffered access units.
    pub fn push_access_unit(
        &mut self,
        avcc: Vec<u8>,
        pts_90khz: u64,
        is_key: bool,
    ) -> anyhow::Result<()> {
        // Roll over at a keyframe boundary once we have pending data, so each
        // segment begins with a random-access point.
        if is_key && !self.pending.is_empty() {
            self.flush_pending()?;
        }
        if self.pending.is_empty() {
            self.segment_start_pts = Some(pts_90khz);
        }
        self.pending.push((avcc, pts_90khz, is_key));

        if let Some(start) = self.segment_start_pts {
            let elapsed = pts_90khz.saturating_sub(start);
            if elapsed >= (self.config.segment_duration_secs as u64) * 90_000 {
                self.flush_pending()?;
            }
        }
        Ok(())
    }

    /// Flush any buffered access units into a new segment, if any remain.
    pub fn flush(&mut self) -> anyhow::Result<()> {
        self.flush_pending()
    }

    fn flush_pending(&mut self) -> anyhow::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let pending = std::mem::take(&mut self.pending);
        self.segment_start_pts = None;
        let bytes = self.write_ts_segment(&pending)?;
        tracing::debug!(
            bytes,
            segments = self.playlist.segments.len(),
            "wrote HLS segment"
        );
        Ok(())
    }

    /// Mux a batch of H.264 access units (AVCC form) into an MPEG-TS segment
    /// and write it out via [`HlsPackager::write_segment`].
    ///
    /// Each element of `access_units` is `(avcc_bytes, pts_90khz, is_keyframe)`.
    /// Returns the number of bytes written.
    pub fn write_ts_segment(
        &mut self,
        access_units: &[(Vec<u8>, u64, bool)],
    ) -> anyhow::Result<usize> {
        let mut mux = TsMuxer::new();
        for (avcc, pts, key) in access_units {
            mux.write_access_unit(avcc, *pts, *key);
        }
        let ts = mux.finish();
        let len = ts.len();
        self.write_segment(&ts)?;
        Ok(len)
    }

    /// Start a minimal HTTP/1.1 server (tokio-only, no external HTTP library)
    /// that serves:
    /// - `GET /playlist.m3u8` — the current live playlist (rendered in memory)
    /// - `GET /segment*.ts`   — segment files from the output directory
    pub async fn serve(&self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(&self.config.http_bind_addr).await?;
        tracing::info!(addr = %self.config.http_bind_addr, "HLS HTTP server listening");

        let output_dir = PathBuf::from(&self.config.output_dir);

        loop {
            let (mut stream, peer_addr) = listener.accept().await?;
            tracing::debug!(%peer_addr, "HLS HTTP connection");

            // Clone what the task needs.
            let out_dir = output_dir.clone();

            tokio::spawn(async move {
                if let Err(e) = serve_connection(&mut stream, &out_dir).await {
                    tracing::warn!(%peer_addr, error = %e, "HLS HTTP error");
                }
            });
        }
    }
}

/// Handle one HTTP/1.1 request on `stream`.
async fn serve_connection(
    stream: &mut tokio::net::TcpStream,
    output_dir: &Path,
) -> anyhow::Result<()> {
    // Read the request headers into a buffer (up to 8 KiB).
    let mut buf = vec![0u8; 8192];
    let mut total = 0usize;

    loop {
        let n = stream.read(&mut buf[total..]).await?;
        if n == 0 {
            break;
        }
        total += n;
        // Stop reading once we see the end of headers.
        if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if total >= buf.len() {
            break;
        }
    }

    let request_text = std::str::from_utf8(&buf[..total]).unwrap_or("");
    let request_line = request_text.lines().next().unwrap_or("");

    // Parse: METHOD /path HTTP/1.x
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");

    if method != "GET" {
        let response = b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n";
        stream.write_all(response).await?;
        return Ok(());
    }

    // Strip query string if any.
    let path = path.split('?').next().unwrap_or(path);

    // Map path to a file in the output directory.
    // Accept only /playlist.m3u8 and /segment*.ts to avoid path-traversal.
    let file_path = if path == "/playlist.m3u8" {
        output_dir.join("playlist.m3u8")
    } else if path.starts_with("/segment") && path.ends_with(".ts") {
        // Validate the segment filename: must be "/segment" + digits + ".ts"
        let name = &path[1..]; // strip leading '/'
        let stem = name.strip_suffix(".ts").unwrap_or(name);
        let stem = stem.strip_prefix("segment").unwrap_or(stem);
        if stem.chars().all(|c| c.is_ascii_digit()) {
            output_dir.join(name)
        } else {
            return send_404(stream).await;
        }
    } else {
        return send_404(stream).await;
    };

    // Read and serve the file.
    match tokio::fs::read(&file_path).await {
        Ok(contents) => {
            let content_type = if file_path.extension().and_then(|e| e.to_str()) == Some("m3u8") {
                "application/vnd.apple.mpegurl"
            } else {
                "video/MP2T"
            };

            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\n\r\n",
                content_type,
                contents.len()
            );
            stream.write_all(header.as_bytes()).await?;
            stream.write_all(&contents).await?;
        }
        Err(_) => {
            send_404(stream).await?;
        }
    }

    stream.flush().await?;
    Ok(())
}

async fn send_404(stream: &mut tokio::net::TcpStream) -> anyhow::Result<()> {
    let response = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
    stream.write_all(response).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_avcc() -> Vec<u8> {
        // A tiny fake IDR NAL in AVCC (4-byte length prefix) form.
        vec![0, 0, 0, 2, 0x65, 0x88]
    }

    #[test]
    fn rolls_segment_on_keyframe_boundary() {
        let dir = std::env::temp_dir().join(format!("kinetix_hls_test_{}", std::process::id()));
        let cfg = HlsConfig {
            segment_duration_secs: 100, // large: only keyframe roll should trigger
            output_dir: dir.to_string_lossy().to_string(),
            window_size: 10,
            http_bind_addr: "0.0.0.0:0".into(),
        };
        let mut p = HlsPackager::new(cfg);

        // First keyframe starts segment 0.
        p.push_access_unit(fake_avcc(), 0, true).unwrap();
        // A non-keyframe AU stays in the same segment.
        p.push_access_unit(fake_avcc(), 90_000, false).unwrap();
        assert_eq!(p.playlist().segments.len(), 0);
        // A second keyframe forces a roll.
        p.push_access_unit(fake_avcc(), 180_000, true).unwrap();
        assert_eq!(p.playlist().segments.len(), 1);
        // Flush the trailing segment.
        p.flush().unwrap();
        assert_eq!(p.playlist().segments.len(), 2);
    }

    #[test]
    fn rolls_segment_on_duration_threshold() {
        let dir = std::env::temp_dir().join(format!("kinetix_hls_test2_{}", std::process::id()));
        let cfg = HlsConfig {
            segment_duration_secs: 2, // 2s => 180_000 ticks
            output_dir: dir.to_string_lossy().to_string(),
            window_size: 10,
            http_bind_addr: "0.0.0.0:0".into(),
        };
        let mut p = HlsPackager::new(cfg);

        // All non-keyframes, spaced past the duration threshold.
        p.push_access_unit(fake_avcc(), 0, false).unwrap();
        p.push_access_unit(fake_avcc(), 180_000, false).unwrap();
        assert_eq!(p.playlist().segments.len(), 1);
        // Another 2s later — second roll.
        p.push_access_unit(fake_avcc(), 360_000, false).unwrap();
        p.push_access_unit(fake_avcc(), 540_000, false).unwrap();
        assert_eq!(p.playlist().segments.len(), 2);
        // Another 2s later — third segment, then flush the final partial.
        p.push_access_unit(fake_avcc(), 720_000, false).unwrap();
        assert_eq!(p.playlist().segments.len(), 2);
        p.flush().unwrap();
        assert_eq!(p.playlist().segments.len(), 3);
    }
}
