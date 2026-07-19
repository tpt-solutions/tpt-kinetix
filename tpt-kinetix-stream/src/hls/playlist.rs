//! HLS `.m3u8` playlist generation with a sliding window.

use std::{collections::VecDeque, io::Write as _};

use super::segment::HlsSegment;

/// Live HLS playlist with a bounded sliding window of segments.
pub struct HlsPlaylist {
    /// `#EXT-X-TARGETDURATION` value (ceiling of the longest segment duration).
    pub target_duration: u32,
    /// `#EXT-X-MEDIA-SEQUENCE` value — increments as old segments are evicted.
    pub media_sequence: u64,
    /// The current window of segments.
    pub segments: VecDeque<HlsSegment>,
    /// Maximum number of segments retained in the live playlist.
    pub window_size: usize,
    /// When `true`, `#EXT-X-ENDLIST` is appended to the rendered playlist.
    pub ended: bool,
}

impl HlsPlaylist {
    /// Create a new, empty playlist.
    pub fn new(target_duration: u32, window_size: usize) -> Self {
        Self {
            target_duration,
            media_sequence: 0,
            segments: VecDeque::new(),
            window_size,
            ended: false,
        }
    }

    /// Append a segment to the playlist, evicting the oldest one if the window
    /// is full.
    pub fn add_segment(&mut self, seg: HlsSegment) {
        self.segments.push_back(seg);
        if self.segments.len() > self.window_size {
            self.media_sequence += 1;
            self.segments.pop_front();
        }
    }

    /// Render the playlist as an HLS `.m3u8` string.
    pub fn render(&self) -> String {
        let mut out = String::new();

        out.push_str("#EXTM3U\n");
        out.push_str("#EXT-X-VERSION:3\n");
        out.push_str(&format!("#EXT-X-TARGETDURATION:{}\n", self.target_duration));
        out.push_str(&format!("#EXT-X-MEDIA-SEQUENCE:{}\n", self.media_sequence));

        for seg in &self.segments {
            out.push_str(&format!("#EXTINF:{:.3},\n", seg.duration_secs));
            out.push_str(&HlsSegment::filename(seg.index));
            out.push('\n');
        }

        if self.ended {
            out.push_str("#EXT-X-ENDLIST\n");
        }

        out
    }

    /// Write the rendered playlist to `path` atomically (write then rename).
    pub fn render_to_file(&self, path: &std::path::Path) -> std::io::Result<()> {
        // Write to a temp file alongside the target and rename atomically.
        let tmp_path = path.with_extension("m3u8.tmp");
        {
            let mut f = std::fs::File::create(&tmp_path)?;
            f.write_all(self.render().as_bytes())?;
            f.flush()?;
        }
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }
}
