//! Recording / DVR for the live server: finished segments are persisted to disk
//! as they complete, a growing playlist is kept next to them, and after the
//! publish ends the same files are a plain VOD presentation.
//!
//! Layout: `<root>/<key>/g<N>/` holds `init-T.mp4`, `seg-T-N.m4s`, one
//! `track-T.m3u8` per track and the `master.m3u8` (identical to the live one, so
//! its relative URIs resolve). `N` is the *generation*: it increases when a
//! publisher reconnects with a different codec configuration; a reconnect with
//! the same configuration continues the same generation.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use tpt_kinetix_package::{CompletedSegment, LivePackager};

/// How much a [`Recorder`] keeps.
#[derive(Clone, Copy, Debug, Default)]
pub struct RecordingLimits {
    /// Rolling DVR depth: once a track holds more than this much media, the
    /// oldest segments are deleted and dropped from the playlist (which then
    /// slides, `EXT-X-MEDIA-SEQUENCE` advancing). `None` keeps everything.
    pub max_duration: Option<Duration>,
    /// Generations kept per key, newest first (`None` keeps all). A generation is
    /// one continuous presentation; older ones are deleted when a new one starts.
    pub keep_generations: Option<usize>,
    /// Segment bytes kept per generation, across all tracks: when exceeded the
    /// oldest segments are deleted, like `max_duration` (the newest segment of
    /// each track is always kept, so one oversized segment can exceed it).
    pub max_bytes: Option<u64>,
}

struct Entry {
    bytes: u64,
    number: u64,
    seconds: f64,
    discontinuity: bool,
}

#[derive(Default)]
struct KeyRecording {
    generation: u64,
    /// Entries per track.
    tracks: Vec<Vec<Entry>>,
    inits_written: bool,
    /// Per track: discontinuity-flagged segments trimmed off the front.
    trimmed_discontinuities: Vec<u64>,
    /// Whether any segment has been trimmed.
    trimmed: bool,
}

/// Persists live presentations under a root directory.
pub struct Recorder {
    root: PathBuf,
    limits: RecordingLimits,
    state: Mutex<HashMap<String, KeyRecording>>,
}

impl Recorder {
    /// A recorder writing under `root` (created on first use).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            limits: RecordingLimits::default(),
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Applies retention limits.
    pub fn with_limits(mut self, limits: RecordingLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Deletes generations older than the newest `keep_generations`, once
    /// `current` exists.
    fn prune_generations(&self, key: &str, current: u64) {
        let Some(keep) = self.limits.keep_generations else {
            return;
        };
        let keep = keep.max(1) as u64;
        if current <= keep {
            return;
        }
        let Ok(rd) = std::fs::read_dir(self.root.join(key)) else {
            return;
        };
        for e in rd.filter_map(|e| e.ok()) {
            let g: Option<u64> = e.file_name().to_str().and_then(|n| n.strip_prefix('g')?.parse().ok());
            if g.is_some_and(|g| g + keep <= current) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }

    fn dir(&self, key: &str, generation: u64) -> PathBuf {
        self.root.join(key).join(format!("g{generation}"))
    }

    /// Writes everything `live` has completed since the last call, refreshes the
    /// playlists, and (when `ended`) closes them with `EXT-X-ENDLIST`.
    pub fn record(&self, key: &str, live: &mut LivePackager, ended: bool) -> Result<()> {
        let done = live.drain_completed();
        let mut state = self.state.lock().unwrap();
        let rec = state.entry(key.to_string()).or_insert_with(|| KeyRecording {
            // Never overwrite what an earlier run of the server recorded.
            generation: latest_generation(&self.root.join(key)).map_or(1, |g| g + 1),
            ..Default::default()
        });
        if done.is_empty() && !ended && rec.inits_written {
            return Ok(());
        }
        let dir = self.dir(key, rec.generation);
        let fresh_dir = !dir.exists();
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        if fresh_dir {
            self.prune_generations(key, rec.generation);
        }
        if rec.tracks.len() < live.track_count() {
            rec.tracks.resize_with(live.track_count(), Vec::new);
            rec.trimmed_discontinuities.resize(live.track_count(), 0);
        }
        if !rec.inits_written && live.is_ready() {
            for t in 0..live.track_count() {
                if let Some(init) = live.init_segment(t) {
                    write(&dir.join(format!("init-{t}.mp4")), &init)?;
                }
            }
            if let Some(master) = live.master_playlist() {
                write(&dir.join("master.m3u8"), master.as_bytes())?;
            }
            rec.inits_written = true;
        }
        for CompletedSegment {
            track,
            number,
            seconds,
            discontinuity,
            data,
        } in done
        {
            write(&dir.join(format!("seg-{track}-{number}.m4s")), &data)?;
            rec.tracks[track].push(Entry {
                bytes: data.len() as u64,
                number,
                seconds,
                discontinuity,
            });
        }
        if let Some(max) = self.limits.max_duration {
            let max = max.as_secs_f64();
            for (t, entries) in rec.tracks.iter_mut().enumerate() {
                let mut total: f64 = entries.iter().map(|e| e.seconds).sum();
                while entries.len() > 1 && total > max {
                    let old = entries.remove(0);
                    total -= old.seconds;
                    rec.trimmed_discontinuities[t] += u64::from(old.discontinuity);
                    rec.trimmed = true;
                    let _ = std::fs::remove_file(dir.join(format!("seg-{t}-{}.m4s", old.number)));
                }
            }
        }
        if let Some(max) = self.limits.max_bytes {
            loop {
                let total: u64 = rec.tracks.iter().flatten().map(|e| e.bytes).sum();
                // The oldest segment number that can still be dropped (every
                // track keeps its newest one).
                let oldest = rec
                    .tracks
                    .iter()
                    .filter(|t| t.len() > 1)
                    .map(|t| t[0].number)
                    .min();
                let Some(oldest) = oldest.filter(|_| total > max) else {
                    break;
                };
                for (t, entries) in rec.tracks.iter_mut().enumerate() {
                    if entries.len() > 1 && entries[0].number == oldest {
                        let old = entries.remove(0);
                        rec.trimmed_discontinuities[t] += u64::from(old.discontinuity);
                        rec.trimmed = true;
                        let _ = std::fs::remove_file(dir.join(format!("seg-{t}-{}.m4s", old.number)));
                    }
                }
            }
        }
        for (t, entries) in rec.tracks.iter().enumerate() {
            if entries.is_empty() {
                continue;
            }
            let text = playlist(t, entries, ended, rec.trimmed, rec.trimmed_discontinuities[t]);
            write(&dir.join(format!("track-{t}.m3u8")), text.as_bytes())?;
        }
        Ok(())
    }

    /// Starts a new generation for `key` (the codec configuration changed): the
    /// current one stays on disk, and later segments go to a fresh directory.
    pub fn next_generation(&self, key: &str) {
        let mut state = self.state.lock().unwrap();
        if let Some(rec) = state.get_mut(key) {
            let generation = rec.generation + 1;
            *rec = KeyRecording {
                generation,
                ..Default::default()
            };
        }
    }

    /// Reads a recorded file of `key`'s latest generation; `None` when absent or
    /// when `file` is not a plain recorded file name.
    pub fn read(&self, key: &str, file: &str) -> Option<Vec<u8>> {
        let ok = !file.is_empty()
            && file
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && !file.starts_with('.');
        if !ok {
            return None;
        }
        let known = self.state.lock().unwrap().get(key).map(|r| r.generation);
        // After a restart nothing is in memory: serve the newest generation on disk.
        let generation = match known {
            Some(g) => g,
            None => latest_generation(&self.root.join(key))?,
        };
        std::fs::read(self.dir(key, generation).join(file)).ok()
    }
}

fn latest_generation(dir: &Path) -> Option<u64> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str()?.strip_prefix('g')?.parse().ok())
        .max()
}

fn write(path: &Path, data: &[u8]) -> Result<()> {
    // Write-then-rename so a player never reads a half-written playlist.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))
}

/// The growing (EVENT) or finished (VOD) media playlist of one track.
///
/// `trimmed` means the head was cut by the DVR depth: a live playlist then slides
/// (no `PLAYLIST-TYPE`, which promises append-only) and reports how many
/// discontinuities left the front.
fn playlist(
    track: usize,
    entries: &[Entry],
    ended: bool,
    trimmed: bool,
    dropped_discontinuities: u64,
) -> String {
    let max = entries.iter().map(|e| e.seconds).fold(1.0, f64::max);
    let kind = match (ended, trimmed) {
        (true, _) => "#EXT-X-PLAYLIST-TYPE:VOD\n",
        (false, false) => "#EXT-X-PLAYLIST-TYPE:EVENT\n",
        (false, true) => "",
    };
    let disc = if dropped_discontinuities > 0 {
        format!("#EXT-X-DISCONTINUITY-SEQUENCE:{dropped_discontinuities}\n")
    } else {
        String::new()
    };
    let mut out = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{}\n{disc}{kind}#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-MAP:URI=\"init-{track}.mp4\"\n",
        max.ceil() as u64,
        entries[0].number,
    );
    for e in entries {
        if e.discontinuity {
            out.push_str("#EXT-X-DISCONTINUITY\n");
        }
        let _ = writeln!(out, "#EXTINF:{:.6},\nseg-{track}-{}.m4s", e.seconds, e.number);
    }
    if ended {
        out.push_str("#EXT-X-ENDLIST\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_event_then_vod() {
        let entries = vec![
            Entry {
                bytes: 1,
                number: 1,
                seconds: 2.0,
                discontinuity: false,
            },
            Entry {
                bytes: 1,
                number: 2,
                seconds: 2.5,
                discontinuity: true,
            },
        ];
        let live = playlist(0, &entries, false, false, 0);
        assert!(live.contains("PLAYLIST-TYPE:EVENT"));
        assert!(!live.contains("ENDLIST"));
        assert!(live.contains("TARGETDURATION:3"));
        let vod = playlist(0, &entries, true, false, 0);
        let sliding = playlist(0, &entries, false, true, 1);
        assert!(!sliding.contains("PLAYLIST-TYPE"));
        assert!(sliding.contains("DISCONTINUITY-SEQUENCE:1"));
        assert!(vod.contains("PLAYLIST-TYPE:VOD") && vod.ends_with("#EXT-X-ENDLIST\n"));
        assert_eq!(vod.matches("#EXT-X-DISCONTINUITY\n").count(), 1);
    }

    #[test]
    fn read_rejects_path_tricks() {
        let r = Recorder::new(std::env::temp_dir());
        for f in ["", "../x", "a/b", ".hidden", "a\\b"] {
            assert!(r.read("k", f).is_none(), "{f}");
        }
    }
}
