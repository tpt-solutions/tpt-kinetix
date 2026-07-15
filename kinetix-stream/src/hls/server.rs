//! HLS packager and minimal tokio-based HTTP server.

use std::path::{Path, PathBuf};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::playlist::HlsPlaylist;
use super::segment::HlsSegment;

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
}

impl HlsPackager {
    /// Create a new packager with the given configuration.
    pub fn new(config: HlsConfig) -> Self {
        let playlist = HlsPlaylist::new(config.segment_duration_secs, config.window_size);
        Self {
            config,
            playlist,
            next_index: 0,
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
            let content_type = if file_path.extension().and_then(|e| e.to_str()) == Some("m3u8")
            {
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
