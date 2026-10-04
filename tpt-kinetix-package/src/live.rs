//! Live fMP4 HLS packaging.
//!
//! [`LivePackager`] is a pure state machine: give it the track list ([`StreamInfo`],
//! from any ingest: WebM, Enhanced RTMP, ...) and then the frames as they arrive,
//! and it keeps a sliding window of key-frame-aligned fMP4 segments plus the HLS
//! playlists that describe them. It performs no I/O and has no runtime
//! dependency, so a server of any kind can drive it (see `tpt-kinetix-stream`).
//!
//! Designed for the royalty-free codecs (AV1, VP9, Opus) but codec-agnostic: any
//! codec the fMP4 muxer can describe works. Layout matches the VOD packager: one
//! rendition per track (`track-{i}.m3u8`, `init-{i}.mp4`, `seg-{i}-{n}.m4s`) and
//! `master.m3u8`.
//!
//! Timing: video uses a 90 kHz timescale from the millisecond ingest clock. Opus
//! uses 48 kHz with each packet's duration read from its TOC byte and timestamps
//! synthesised from the first packet (re-anchored if the clocks drift by 100 ms),
//! which keeps audio gapless although ingest timestamps are millisecond-accurate.
//!
//! Not (yet) implemented: preload hints are emitted, but a client that reloads a
//! playlist while the requested media sequence number is still in progress gets
//! the current playlist immediately instead of a blocking 404.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::Arc;

use thiserror::Error;
use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_demux::rfconfig::opus_packet_samples;
use tpt_kinetix_mux::{FragmentWriter, MuxError};

use crate::codec_string;

const VIDEO_TIMESCALE: u32 = 90_000;

/// Live packaging errors.
#[derive(Debug, Error)]
pub enum LiveError {
    /// The track list or a frame was unusable.
    #[error("config: {0}")]
    Config(String),
    /// A fragment could not be built.
    #[error("mux: {0}")]
    Mux(#[from] MuxError),
}

/// Live packaging options.
#[derive(Debug, Clone)]
pub struct LiveOptions {
    /// Target segment length in seconds (segments end at the first lead-track
    /// key frame at or after this length).
    pub segment_seconds: f64,
    /// Number of segments listed in a playlist.
    pub window: usize,
    /// Target partial-segment (`EXT-X-PART`) length in seconds. `None`
    /// disables parts and reverts to plain segment-latency HLS.
    pub part_seconds: Option<f64>,
}

impl Default for LiveOptions {
    fn default() -> Self {
        Self {
            segment_seconds: 2.0,
            window: 6,
            part_seconds: Some(1.0 / 3.0),
        }
    }
}

/// What a player asked for with a media playlist reload: the low-latency
/// blocking-reload query parameters (`_HLS_msn`, `_HLS_part`, `_HLS_skip`).
#[derive(Debug, Clone, Copy, Default)]
pub struct PlaylistRequest {
    /// Last media sequence number the client had (`_HLS_msn`).
    pub msn: Option<u64>,
    /// Last part index within that segment (`_HLS_part`).
    pub part: Option<u64>,
    /// First segment the client wants (`_HLS_skip`), for delta updates.
    pub skip: Option<u64>,
}

struct Sample {
    dts: u64,
    cts: i32,
    key: bool,
    duration: Option<u32>,
    data: Vec<u8>,
}

struct Track {
    info: StreamInfo,
    pending: Vec<Sample>,
    /// Next synthesised decode time (audio).
    next_dts: Option<u64>,
}

/// A partial segment: an independently loadable fragment of the segment in
/// progress, published before the segment itself is complete.
struct Part {
    /// Data of this part.
    data: Arc<Vec<u8>>,
    /// Duration in seconds of this track inside the part.
    seconds: f64,
    /// Whether the part starts on a random-access point of its track.
    independent: bool,
}

struct Segment {
    number: u64,
    /// Per track; `None` when the track had no samples in this segment.
    data: Vec<Option<Arc<Vec<u8>>>>,
    seconds: Vec<f64>,
    /// The parts this segment was published as, per track, so a client that is
    /// part-way through the segment can still fetch the earlier ones.
    parts: Vec<Vec<Part>>,
}

/// Builds a live sliding-window HLS presentation.
pub struct LivePackager {
    opts: LiveOptions,
    tracks: Vec<Track>,
    lead: usize,
    seg_start_ms: Option<i64>,
    last_lead_ms: i64,
    next_number: u64,
    segments: VecDeque<Segment>,
    finished: bool,
    /// Parts published so far for the segment in progress, per track.
    parts: Vec<Vec<Part>>,
    /// Start of the part in progress, in milliseconds.
    part_start_ms: Option<i64>,
    /// Number of parts published for the segment in progress.
    part_index: u64,
    /// Samples already published as parts but not yet folded into a segment, per
    /// track. Retained so the segment's own fragment contains every sample.
    part_samples: Vec<Vec<Sample>>,
}

/// One `#EXT-X-PART` line naming the part's URI.
fn part_tag(out: &mut String, track: usize, segment: u64, index: u64, p: &Part) {
    let _ = writeln!(
        out,
        "#EXT-X-PART:DURATION={:.6},URI=\"part-{track}-{segment}-{index}.mp4\"{}",
        p.seconds,
        if p.independent {
            ",INDEPENDENT=YES"
        } else {
            ""
        }
    );
}

fn build_fragment(
    info: &StreamInfo,
    samples: &[Sample],
    sequence: u64,
) -> Result<Vec<u8>, LiveError> {
    let mut info = info.clone();
    info.index = 0;
    let scale = info.timescale;
    let mut w =
        FragmentWriter::new(&[info])?.starting_sequence(sequence.min(u64::from(u32::MAX)) as u32);
    for s in samples {
        let tb = (1, scale);
        w.push(
            &Packet {
                pts: Timestamp::new(s.dts as i64 + i64::from(s.cts), tb),
                dts: Timestamp::new(s.dts as i64, tb),
                data: s.data.clone(),
                stream_index: 0,
                is_key_frame: s.key,
            },
            s.duration,
        )?;
    }
    w.flush(true)?
        .ok_or_else(|| LiveError::Config("empty segment".into()))
}

impl LivePackager {
    /// A packager with `opts`; call [`Self::set_tracks`] before pushing frames.
    pub fn new(opts: LiveOptions) -> Self {
        Self {
            opts,
            tracks: Vec::new(),
            lead: 0,
            seg_start_ms: None,
            last_lead_ms: 0,
            next_number: 1,
            segments: VecDeque::new(),
            finished: false,
            parts: Vec::new(),
            part_start_ms: None,
            part_index: 0,
            part_samples: Vec::new(),
        }
    }

    /// Declares the tracks. Their `timescale`s are overridden to the packager's
    /// own (90 kHz video, the sample rate for audio) since frames are pushed in
    /// milliseconds. Fails if a track cannot be described in fMP4.
    pub fn set_tracks(&mut self, infos: Vec<StreamInfo>) -> Result<(), LiveError> {
        if !self.tracks.is_empty() {
            let same = self.tracks.len() == infos.len()
                && self
                    .tracks
                    .iter()
                    .zip(&infos)
                    .all(|(t, i)| t.info.extradata == i.extradata);
            return if same {
                Ok(())
            } else {
                Err(LiveError::Config(
                    "the track configuration changed mid-stream; restart the publish".into(),
                ))
            };
        }
        let mut tracks = Vec::new();
        for (i, mut info) in infos.into_iter().enumerate() {
            info.index = i as u32;
            match info.media_type {
                MediaType::Video => info.timescale = VIDEO_TIMESCALE,
                MediaType::Audio => {
                    if info.sample_rate > 0 {
                        info.timescale = info.sample_rate;
                    }
                }
                MediaType::Other => continue,
            }
            if codec_string(&info).is_none() {
                return Err(LiveError::Config(format!(
                    "track {i} ({}) cannot be signalled in HLS",
                    info.codec.name()
                )));
            }
            tracks.push(Track {
                info,
                pending: Vec::new(),
                next_dts: None,
            });
        }
        if tracks.is_empty() {
            return Err(LiveError::Config("no audio or video track".into()));
        }
        self.lead = tracks
            .iter()
            .position(|t| t.info.media_type == MediaType::Video)
            .unwrap_or(0);
        let n = tracks.len();
        self.parts = (0..n).map(|_| Vec::new()).collect();
        self.part_samples = (0..n).map(|_| Vec::new()).collect();
        self.tracks = tracks;
        Ok(())
    }

    /// Feeds one frame of track `stream` with its ingest timestamp `pts_ms`.
    /// `duration_ms` is used for video when known; otherwise a frame's duration
    /// is its distance to the next.
    pub fn push(
        &mut self,
        stream: usize,
        pts_ms: i64,
        key: bool,
        data: Vec<u8>,
        duration_ms: Option<u32>,
    ) -> Result<(), LiveError> {
        if self.finished || stream >= self.tracks.len() {
            return Ok(());
        }
        if self.tracks[stream].info.media_type == MediaType::Video {
            self.push_video(stream, pts_ms, key, data, duration_ms)
        } else {
            self.push_audio(stream, pts_ms, data);
            Ok(())
        }
    }

    fn push_video(
        &mut self,
        stream: usize,
        pts_ms: i64,
        key: bool,
        data: Vec<u8>,
        duration_ms: Option<u32>,
    ) -> Result<(), LiveError> {
        let dts = (pts_ms.max(0) as u64) * 90;
        if let Some(last) = self.tracks[stream].pending.last_mut() {
            last.duration = Some(dts.saturating_sub(last.dts).min(u64::from(u32::MAX)) as u32);
        }
        let is_lead = stream == self.lead;
        if is_lead {
            match self.seg_start_ms {
                None if !key => return Ok(()), // wait for the first key frame
                None => {
                    self.seg_start_ms = Some(pts_ms);
                    self.part_start_ms = Some(pts_ms);
                    // Audio buffered before the start: keep what is not earlier.
                    for t in &mut self.tracks {
                        if t.info.media_type != MediaType::Video {
                            let rate = u64::from(t.info.timescale);
                            t.pending
                                .retain(|s| (s.dts * 1000 / rate) as i64 + 1 >= pts_ms);
                        }
                    }
                }
                Some(start) => {
                    // Publish a partial segment before the segment is complete.
                    if self.parts_enabled() {
                        if let Some(part_start) = self.part_start_ms {
                            let want = (self.opts.part_seconds.unwrap_or(0.0) * 1000.0) as i64;
                            if (pts_ms - part_start) >= want {
                                self.cut_part(pts_ms)?;
                            }
                        }
                    }
                    if key && (pts_ms - start) as f64 >= self.opts.segment_seconds * 1000.0 {
                        self.cut(pts_ms, false)?;
                    }
                }
            }
            self.last_lead_ms = pts_ms;
        } else if self.seg_start_ms.is_none() {
            return Ok(());
        }
        self.tracks[stream].pending.push(Sample {
            dts,
            cts: 0,
            key,
            duration: duration_ms.map(|d| d * 90),
            data,
        });
        Ok(())
    }

    fn push_audio(&mut self, stream: usize, pts_ms: i64, data: Vec<u8>) {
        // Audio often arrives before the first lead key frame is seen (muxers
        // interleave by timestamp, not by track): keep it, bounded, and prune by
        // timestamp once the first segment's start time is known.
        let start = self.seg_start_ms;
        let t = &mut self.tracks[stream];
        let rate = u64::from(t.info.timescale);
        // Per-packet duration in track ticks (Opus: from the TOC, at 48 kHz).
        let duration = match t.info.codec {
            CodecId::Opus => {
                opus_packet_samples(&data).map(|s| (u64::from(s) * rate / 48_000) as u32)
            }
            CodecId::Aac => Some(1024),
            _ => None,
        };
        let observed = (pts_ms.max(0) as u64) * rate / 1000;
        let dts = match t.next_dts {
            Some(n) if observed.abs_diff(n) <= rate / 10 => n,
            _ => observed, // first packet, or the clocks drifted: re-anchor
        };
        let dur = duration.unwrap_or((rate / 50) as u32); // 20 ms fallback
        t.next_dts = Some(dts + u64::from(dur));
        if let Some(start) = start {
            if (dts * 1000 / rate) as i64 + 1 < start {
                return;
            }
        } else if t.pending.len() >= 512 {
            t.pending.remove(0);
        }
        t.pending.push(Sample {
            dts,
            cts: 0,
            key: true,
            duration: Some(dur),
            data,
        });
    }

    /// Whether partial segments are enabled and configured sanely.
    fn parts_enabled(&self) -> bool {
        self.opts.part_seconds.is_some_and(|p| p > 0.0)
    }

    /// Closes the part in progress at `end_ms` and publishes it as an
    /// `EXT-X-PART`-addressable fragment. Samples are retained in
    /// [`Self::part_samples`] so the segment's own fragment still contains them.
    fn cut_part(&mut self, end_ms: i64) -> Result<(), LiveError> {
        let number = self.next_number;
        let start_ms = self.part_start_ms.unwrap_or(end_ms);
        let mut added = 0usize;
        for (i, t) in self.tracks.iter_mut().enumerate() {
            let rate = u64::from(t.info.timescale);
            let boundary = (end_ms.max(0) as u64) * rate / 1000;
            let split = t.pending.partition_point(|s| s.dts < boundary);
            if split == 0 {
                continue;
            }
            let taken: Vec<Sample> = t.pending.drain(..split).collect();
            let ticks: u64 = taken
                .iter()
                .map(|s| u64::from(s.duration.unwrap_or(0)))
                .sum();
            let seconds = if i == self.lead {
                ((end_ms - start_ms).max(0) as f64) / 1000.0
            } else {
                ticks as f64 / rate as f64
            };
            let independent = t.info.media_type != MediaType::Video || taken[0].key;
            let data = Arc::new(build_fragment(&t.info, &taken, number)?);
            self.part_samples[i].extend(taken);
            self.parts[i].push(Part {
                data,
                seconds,
                independent,
            });
            added += 1;
        }
        if added == 0 {
            // Nothing since the last part: keep waiting.
            return Ok(());
        }
        self.part_index += 1;
        self.part_start_ms = Some(end_ms);
        Ok(())
    }

    /// Closes the segment in progress at `end_ms` and appends it to the window.
    fn cut(&mut self, end_ms: i64, flush_all: bool) -> Result<(), LiveError> {
        let start_ms = self.seg_start_ms.unwrap_or(end_ms);
        let number = self.next_number;
        // LL-HLS requires every sample of the segment to be inside a part: close
        // the last part at the segment boundary before the segment itself.
        if self.parts_enabled() && self.seg_start_ms.is_some() {
            self.cut_part(end_ms)?;
        }
        let mut data = Vec::with_capacity(self.tracks.len());
        let mut seconds = Vec::with_capacity(self.tracks.len());
        for (i, t) in self.tracks.iter_mut().enumerate() {
            let rate = u64::from(t.info.timescale);
            let mut current = if t.info.media_type == MediaType::Video {
                std::mem::take(&mut t.pending)
            } else {
                // Audio: everything decoded before the boundary (all of it when the
                // stream ends, so no tail packets are lost).
                let boundary = (end_ms.max(0) as u64) * rate / 1000;
                let split = if flush_all {
                    t.pending.len()
                } else {
                    t.pending.partition_point(|s| s.dts < boundary)
                };
                let rest = t.pending.split_off(split);
                std::mem::replace(&mut t.pending, rest)
            };
            // Samples already published as parts come first (earlier decode times).
            let mut prefix = std::mem::take(&mut self.part_samples[i]);
            prefix.append(&mut current);
            let current = prefix;
            if current.is_empty() {
                data.push(None);
                seconds.push(0.0);
                continue;
            }
            let ticks: u64 = if i == self.lead {
                0
            } else {
                current
                    .iter()
                    .map(|s| u64::from(s.duration.unwrap_or(0)))
                    .sum()
            };
            seconds.push(if i == self.lead {
                (end_ms - start_ms) as f64 / 1000.0
            } else {
                ticks as f64 / rate as f64
            });
            data.push(Some(Arc::new(build_fragment(&t.info, &current, number)?)));
        }
        let parts = std::mem::take(&mut self.parts);
        self.parts = (0..self.tracks.len()).map(|_| Vec::new()).collect();
        self.part_samples = (0..self.tracks.len()).map(|_| Vec::new()).collect();
        self.part_index = 0;
        self.part_start_ms = Some(end_ms);
        self.segments.push_back(Segment {
            number,
            data,
            seconds,
            parts,
        });
        self.next_number += 1;
        // Keep a little beyond the window so a slow client can still fetch it.
        while self.segments.len() > self.opts.window + 3 {
            self.segments.pop_front();
        }
        self.seg_start_ms = Some(end_ms);
        Ok(())
    }

    /// Ends the stream: flushes the partial segment and marks the playlists complete.
    pub fn finish(&mut self) -> Result<(), LiveError> {
        if self.finished {
            return Ok(());
        }
        if self.seg_start_ms.is_some()
            && self
                .tracks
                .get(self.lead)
                .is_some_and(|t| !t.pending.is_empty())
        {
            // The last lead frame has no successor: reuse the previous duration (or 1/30 s).
            let lead = self.lead;
            let pending = &mut self.tracks[lead].pending;
            let n = pending.len();
            let prev = n
                .checked_sub(2)
                .and_then(|i| pending[i].duration)
                .unwrap_or(3000);
            if let Some(last) = pending.last_mut() {
                last.duration.get_or_insert(prev);
            }
            let tail_ms = pending
                .last()
                .map_or(0, |s| i64::from(s.duration.unwrap_or(0)) / 90);
            self.cut(self.last_lead_ms + tail_ms, true)?;
        }
        self.finished = true;
        Ok(())
    }

    /// Whether the stream has ended.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Whether at least one segment is available (playlists exist from then on).
    pub fn is_ready(&self) -> bool {
        !self.segments.is_empty()
    }

    /// Number of tracks.
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// The init segment of `track`, available as soon as the first segment is.
    pub fn init_segment(&self, track: usize) -> Option<Vec<u8>> {
        if !self.is_ready() {
            return None;
        }
        let mut info = self.tracks.get(track)?.info.clone();
        info.index = 0;
        FragmentWriter::new(&[info]).ok().map(|w| w.init_segment())
    }

    /// Segment `number` of `track`, if still in the retained window.
    pub fn segment(&self, track: usize, number: u64) -> Option<Arc<Vec<u8>>> {
        self.segments
            .iter()
            .find(|s| s.number == number)
            .and_then(|s| s.data.get(track).cloned().flatten())
    }

    /// Part `index` (0-based) of segment `number` of `track`, if available.
    /// Covers both the parts of a completed segment and those of the segment in
    /// progress (whose number is [`Self::latest_segment`] + 1).
    pub fn part(&self, track: usize, number: u64, index: u64) -> Option<Arc<Vec<u8>>> {
        if number == self.next_number {
            return self
                .parts
                .get(track)
                .and_then(|p| p.get(usize::try_from(index).ok()?))
                .map(|p| p.data.clone());
        }
        self.segments
            .iter()
            .find(|s| s.number == number)
            .and_then(|s| s.parts.get(track)?.get(usize::try_from(index).ok()?))
            .map(|p| p.data.clone())
    }

    /// Number of parts published for the segment in progress (`0` when parts
    /// are disabled or nothing has been published yet).
    pub fn latest_part(&self) -> u64 {
        self.part_index
    }

    fn window(&self) -> impl Iterator<Item = &Segment> {
        let skip = self.segments.len().saturating_sub(self.opts.window);
        self.segments.iter().skip(skip)
    }

    /// The media playlist of `track` (sliding window; `EXT-X-ENDLIST` once finished).
    pub fn media_playlist(&self, track: usize) -> Option<String> {
        self.media_playlist_for(track, &PlaylistRequest::default())
    }

    /// The media playlist of `track`, honouring a low-latency reload request.
    ///
    /// With `req.msn` set, the returned playlist starts at the requested media
    /// sequence number (waiting for it to appear is the caller's job — see
    /// [`Self::satisfies`]); `req.skip` asks for a delta playlist. Both are
    /// ignored when parts are disabled, where a full playlist is returned.
    pub fn media_playlist_for(&self, track: usize, req: &PlaylistRequest) -> Option<String> {
        self.tracks.get(track)?;
        let first = self.window().next()?;
        let parts = self.parts_enabled();
        let max = self
            .window()
            .map(|s| s.seconds[track])
            .fold(self.opts.segment_seconds, f64::max);
        let version = if parts { 9 } else { 7 };
        // Delta playlist: start at the first segment the client still wants, which
        // must not be older than the retained window.
        let oldest = self.segments.front().map_or(0, |s| s.number);
        let start = if parts {
            req.msn
                .or(req.skip)
                .map(|n| n.max(oldest).max(first.number))
                .unwrap_or(first.number)
        } else {
            first.number
        };
        let mut out = format!(
            "#EXTM3U\n#EXT-X-VERSION:{version}\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{start}\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-MAP:URI=\"init-{track}.mp4\"\n",
            max.ceil() as u64,
        );
        if parts {
            let part_target = self.opts.part_seconds.unwrap_or(0.0);
            let _ = write!(
                out,
                "#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK={},CAN-SKIP-UNTIL={:.3}\n#EXT-X-PART-INF:PART-TARGET={part_target:.5}\n",
                // Keep one part of each track per segment available beyond the window.
                ((self.opts.segment_seconds / part_target).ceil() as u64).max(1) * 2,
                self.opts.segment_seconds * self.opts.window as f64,
            );
        }
        let skipped = start.saturating_sub(oldest);
        if parts && skipped > 0 {
            let _ = writeln!(out, "#EXT-X-SKIP:SKIPPED-SEGMENTS={skipped}");
        }
        for s in self.segments.iter().filter(|s| s.number >= start) {
            let has = s.data.get(track).is_some_and(Option::is_some);
            if parts {
                if let Some(ps) = s.parts.get(track) {
                    for (i, p) in ps.iter().enumerate() {
                        part_tag(&mut out, track, s.number, i as u64, p);
                    }
                }
            }
            if has {
                let _ = writeln!(
                    out,
                    "#EXTINF:{:.6},\nseg-{track}-{}.m4s",
                    s.seconds[track], s.number
                );
            }
        }
        if parts && !self.finished && self.is_ready() {
            // The segment in progress: its published parts, then a hint for the next.
            let in_progress = self.next_number;
            if let Some(ps) = self.parts.get(track) {
                for (i, p) in ps.iter().enumerate() {
                    part_tag(&mut out, track, in_progress, i as u64, p);
                }
            }
            let _ = writeln!(
                out,
                "#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"part-{track}-{in_progress}-{}.mp4\"",
                self.part_index
            );
        }
        if self.finished {
            out.push_str("#EXT-X-ENDLIST\n");
        }
        Some(out)
    }

    /// Whether a blocking playlist request `req` can now be answered: the
    /// segment (and, if given, the part) it waits for exists. `track` only
    /// matters when a part index is requested, since parts are per track.
    pub fn satisfies(&self, track: usize, req: &PlaylistRequest) -> bool {
        if self.finished {
            return true;
        }
        let Some(msn) = req.msn else {
            return true;
        };
        // The next segment to be published must be the one asked for (or later).
        if msn >= self.next_number {
            return false;
        }
        match req.part {
            Some(p) => self.part(track, msn, p).is_some(),
            None => msn < self.next_number,
        }
    }

    /// The master playlist.
    pub fn master_playlist(&self) -> Option<String> {
        if !self.is_ready() {
            return None;
        }
        let bandwidth = |track: usize| -> u64 {
            self.window()
                .filter_map(|s| {
                    let bytes = s.data.get(track)?.as_ref()?.len() as f64;
                    (s.seconds[track] > 0.0).then(|| (bytes * 8.0 / s.seconds[track]) as u64)
                })
                .max()
                .unwrap_or(0)
        };
        let audio: Vec<usize> = (0..self.tracks.len())
            .filter(|&i| self.tracks[i].info.media_type == MediaType::Audio)
            .collect();
        let mut out = String::from("#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-INDEPENDENT-SEGMENTS\n");
        for (n, &a) in audio.iter().enumerate() {
            let _ = writeln!(
                out,
                "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"audio-{a}\",DEFAULT={},AUTOSELECT=YES,CHANNELS=\"{}\",URI=\"track-{a}.m3u8\"",
                if n == 0 { "YES" } else { "NO" },
                self.tracks[a].info.channels.max(1)
            );
        }
        let lead_audio = audio.first().copied();
        for (v, t) in self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.info.media_type == MediaType::Video)
        {
            let mut codecs = codec_string(&t.info)?;
            let mut bw = bandwidth(v);
            let mut audio_attr = "";
            if let Some(a) = lead_audio {
                let _ = write!(codecs, ",{}", codec_string(&self.tracks[a].info)?);
                bw += bandwidth(a);
                audio_attr = ",AUDIO=\"audio\"";
            }
            let _ = writeln!(
                out,
                "#EXT-X-STREAM-INF:BANDWIDTH={},CODECS=\"{codecs}\",RESOLUTION={}x{}{audio_attr}\ntrack-{v}.m3u8",
                bw.max(1),
                t.info.width,
                t.info.height
            );
        }
        if audio.len() == self.tracks.len() {
            // Audio-only presentation: plain variants.
            for &a in &audio {
                let _ = writeln!(
                    out,
                    "#EXT-X-STREAM-INF:BANDWIDTH={},CODECS=\"{}\"\ntrack-{a}.m3u8",
                    bandwidth(a).max(1),
                    codec_string(&self.tracks[a].info)?
                );
            }
        }
        Some(out)
    }

    /// Highest segment number produced so far (0 before the first).
    pub fn latest_segment(&self) -> u64 {
        self.next_number - 1
    }
}
