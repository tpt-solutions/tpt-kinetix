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
    /// A delta update was requested (`_HLS_skip=YES|v2`); the value is not used.
    /// Only then may the playlist replace old segments with `EXT-X-SKIP`.
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
    /// Per track: decode time (track ticks) of the segment's first sample, i.e. its
    /// `tfdt`, which a DASH `SegmentTimeline` must state exactly.
    start_ticks: Vec<u64>,
    /// The parts this segment was published as, per track, so a client that is
    /// part-way through the segment can still fetch the earlier ones.
    parts: Vec<Vec<Part>>,
    /// Whether a publisher reconnected before this segment: it is preceded by
    /// `EXT-X-DISCONTINUITY` in the HLS playlists.
    discontinuity: bool,
}

/// One finished segment of one track, handed out by
/// [`LivePackager::drain_completed`] so it can be recorded.
#[derive(Clone)]
pub struct CompletedSegment {
    /// Track index.
    pub track: usize,
    /// Segment number (continues across reconnects).
    pub number: u64,
    /// Duration of the segment on this track, in seconds.
    pub seconds: f64,
    /// Whether a publisher reconnected just before this segment.
    pub discontinuity: bool,
    /// The fMP4 fragment.
    pub data: Arc<Vec<u8>>,
}

/// Builds a live sliding-window HLS presentation.
pub struct LivePackager {
    /// Segments finished but not yet drained, when recording is on.
    completed: Vec<CompletedSegment>,
    recording: bool,
    /// Wall-clock time (ms since the Unix epoch) of media time 0, fixed when the
    /// first segment completes: DASH `availabilityStartTime`.
    ast_ms: Option<i64>,
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
    /// A publisher resumed and no segment since has carried the discontinuity.
    pending_discontinuity: bool,
    /// Discontinuity-flagged segments that have left the retained window
    /// (`EXT-X-DISCONTINUITY-SEQUENCE`).
    discontinuities_dropped: u64,
    /// On resume: where the new publisher's first frame lands on the existing
    /// timeline (the end of the previous publish), so media times stay continuous.
    resume_base_ms: Option<i64>,
    /// Added to every incoming timestamp (set from `resume_base_ms` on the first
    /// frame after a resume).
    ts_offset_ms: i64,
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

/// The `mfhd` sequence number of part `index` of segment `number`: increasing
/// across a track's fragments, as CMAF requires.
fn part_sequence(number: u64, index: u64) -> u64 {
    number.saturating_mul(1000).saturating_add(index)
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
            completed: Vec::new(),
            recording: false,
            ast_ms: None,
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
            pending_discontinuity: false,
            discontinuities_dropped: 0,
            resume_base_ms: None,
            ts_offset_ms: 0,
        }
    }

    /// Turns on collection of finished segments for [`Self::drain_completed`].
    /// Segments finished before this call are not collected.
    pub fn set_recording(&mut self, on: bool) {
        self.recording = on;
        if !on {
            self.completed.clear();
        }
    }

    /// Takes the segments finished since the last call (recording must be on).
    pub fn drain_completed(&mut self) -> Vec<CompletedSegment> {
        std::mem::take(&mut self.completed)
    }

    /// Whether `infos` could continue this presentation: it has no tracks yet,
    /// or its tracks carry the same codec configuration.
    pub fn accepts_tracks(&self, infos: &[StreamInfo]) -> bool {
        self.tracks.is_empty()
            || (self.tracks.len() == infos.len()
                && self
                    .tracks
                    .iter()
                    .zip(infos)
                    .all(|(t, i)| {
                    // VP9's `vpcC` carries no size, so compare the dimensions too.
                    t.info.extradata == i.extradata
                        && t.info.width == i.width
                        && t.info.height == i.height
                }))
    }

    /// Reopens a finished presentation for a reconnecting publisher.
    ///
    /// Segment numbering and the media timeline continue where the previous
    /// publish ended, and the first new segment is marked with
    /// `EXT-X-DISCONTINUITY`. A no-op unless the stream is finished. Called by
    /// [`Self::set_tracks`] when a finished stream is given the same tracks.
    fn resume(&mut self) {
        if !self.finished {
            return;
        }
        self.finished = false;
        for t in &mut self.tracks {
            t.pending.clear();
            t.next_dts = None;
        }
        for p in &mut self.parts {
            p.clear();
        }
        for p in &mut self.part_samples {
            p.clear();
        }
        self.part_index = 0;
        self.part_start_ms = None;
        if let Some(end) = self.seg_start_ms.take() {
            // Something was published: continue after it.
            self.resume_base_ms = Some(end);
            self.pending_discontinuity = true;
        }
    }

    /// Declares the tracks. Their `timescale`s are overridden to the packager's
    /// own (90 kHz video, the sample rate for audio) since frames are pushed in
    /// milliseconds. Fails if a track cannot be described in fMP4.
    pub fn set_tracks(&mut self, infos: Vec<StreamInfo>) -> Result<(), LiveError> {
        if !self.tracks.is_empty() {
            return if self.accepts_tracks(&infos) {
                // A finished stream given the same tracks is a reconnect.
                self.resume();
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
        if let Some(base) = self.resume_base_ms.take() {
            self.ts_offset_ms = base - pts_ms;
        }
        let pts_ms = pts_ms + self.ts_offset_ms;
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
            let data = Arc::new(build_fragment(
                &t.info,
                &taken,
                part_sequence(number, self.part_index),
            )?);
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
        let mut start_ticks = Vec::with_capacity(self.tracks.len());
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
            // With parts, the segment is the concatenation of its parts' fragments
            // (plus one more fragment for any samples that missed the last part):
            // what a player gets from the parts so far is then a true prefix of the
            // finished segment, which low-latency DASH serves as CMAF chunks.
            let part_bytes: Vec<u8> = if prefix.is_empty() {
                Vec::new()
            } else {
                self.parts[i].iter().flat_map(|p| p.data.iter().copied()).collect()
            };
            let tail_only = !part_bytes.is_empty();
            let tail = if tail_only { std::mem::take(&mut current) } else { Vec::new() };
            prefix.append(&mut current);
            let current = prefix;
            if current.is_empty() && tail.is_empty() {
                data.push(None);
                seconds.push(0.0);
                start_ticks.push(0);
                continue;
            }
            start_ticks.push(current.first().or(tail.first()).map_or(0, |s| s.dts));
            let ticks: u64 = if i == self.lead {
                0
            } else {
                current
                    .iter()
                    .chain(tail.iter())
                    .map(|s| u64::from(s.duration.unwrap_or(0)))
                    .sum()
            };
            seconds.push(if i == self.lead {
                (end_ms - start_ms) as f64 / 1000.0
            } else {
                ticks as f64 / rate as f64
            });
            if tail_only {
                let mut bytes = part_bytes;
                if !tail.is_empty() {
                    bytes.extend(build_fragment(&t.info, &tail, part_sequence(number, 999))?);
                }
                data.push(Some(Arc::new(bytes)));
            } else {
                data.push(Some(Arc::new(build_fragment(&t.info, &current, number)?)));
            }
        }
        let discontinuity = std::mem::take(&mut self.pending_discontinuity);
        if self.recording {
            for (track, d) in data.iter().enumerate() {
                if let Some(d) = d {
                    self.completed.push(CompletedSegment {
                        track,
                        number,
                        seconds: seconds[track],
                        discontinuity,
                        data: d.clone(),
                    });
                }
            }
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
            start_ticks,
            parts,
            discontinuity,
        });
        if self.ast_ms.is_none() {
            // The wall clock now is media time `end_ms`: that fixes the origin.
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0i64, |d| d.as_millis() as i64);
            self.ast_ms = Some(now_ms - end_ms);
        }
        self.next_number += 1;
        // Keep a little beyond the window so a slow client can still fetch it.
        while self.segments.len() > self.opts.window + 3 {
            if let Some(old) = self.segments.pop_front() {
                self.discontinuities_dropped += u64::from(old.discontinuity);
            }
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

    /// The CMAF chunks published so far for `number` of `track`: every part of
    /// a completed segment, or the parts published so far when `number` is the
    /// segment in progress. Concatenated, they are a prefix of the final
    /// segment bytes, so a low-latency DASH player can start playback before
    /// the segment completes.
    ///
    /// Completed segments may have dropped their parts once the window went
    /// past `2 * window` (memory cap); then the prefix is the retained whole
    /// segment, i.e. itself.
    pub fn segment_prefix(&self, track: usize, number: u64) -> Option<Vec<u8>> {
        let collect = |parts: &[Part]| {
            let mut out = Vec::new();
            for p in parts {
                out.extend_from_slice(&p.data);
            }
            (!out.is_empty()).then_some(out)
        };
        if number == self.next_number {
            return self.parts.get(track).and_then(|p| collect(p));
        }
        if let Some(prefix) = self
            .segments
            .iter()
            .find(|s| s.number == number)
            .and_then(|s| s.parts.get(track).map(Vec::as_slice))
            .and_then(collect)
        {
            return Some(prefix);
        }
        // Parts evicted: the whole retained segment is its own prefix.
        self.segment(track, number).map(|b| b.to_vec())
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

    /// Estimated end-to-end latency of the live edge, in seconds: the age of
    /// the oldest frame in the segment in progress (i.e. how far behind real
    /// time a player joining now would start), or of the newest retained
    /// segment when idle. This is what the `/_stats` HTTP endpoint and the
    /// hls.js latency probe report as `live_latency_secs`.
    pub fn live_latency_secs(&self) -> Option<f64> {
        // Prefer the in-progress segment (the true live edge); fall back to
        // the newest retained segment when nothing is in flight. `None` only
        // before tracks exist or when no segment has ever completed.
        if self.tracks.get(self.lead).is_none() {
            return None;
        }
        Some(self.pending_secs().or_else(|| {
            self.segments
                .back()
                .and_then(|s| s.seconds.get(self.lead).copied())
        })?)
    }

    /// Target part duration in seconds (`None` when parts are disabled).
    pub fn part_seconds(&self) -> Option<f64> {
        self.parts_enabled().then(|| self.opts.part_seconds.unwrap_or(0.0))
    }

    /// Target segment duration in seconds.
    pub fn segment_seconds(&self) -> f64 {
        self.opts.segment_seconds
    }

    /// Seconds buffered for the segment in progress on the lead track (0 when
    /// idle). Backs [`Self::live_latency_secs`].
    fn pending_secs(&self) -> Option<f64> {
        let t = self.tracks.get(self.lead)?;
        if t.pending.is_empty() {
            return Some(0.0);
        }
        let scale = f64::from(t.info.timescale.max(1));
        let first = t.pending.first()?.dts;
        let last = t.pending.last()?;
        let end = last.dts + u64::from(last.duration.unwrap_or(0));
        Some(end.saturating_sub(first) as f64 / scale)
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
        // The playlist always lists the whole window. `_HLS_msn`/`_HLS_part` only
        // say how long the reload blocks (see `satisfies`); they never shorten it.
        // Only an explicit delta request (`_HLS_skip`) may replace the oldest
        // segments with `EXT-X-SKIP`, and never ones within CAN-SKIP-UNTIL of the
        // end of the playlist.
        // RFC 8216bis 4.4.3.8: at least 6 target durations.
        let can_skip_until = 6.0 * max.ceil();
        let mut start = first.number;
        if parts && req.skip.is_some() {
            let mut kept = 0.0;
            let mut keep_from = first.number;
            for s in self.window().collect::<Vec<_>>().into_iter().rev() {
                keep_from = s.number;
                kept += s.seconds[track];
                if kept >= can_skip_until {
                    break;
                }
            }
            start = keep_from;
        }
        let mut out = format!(
            "#EXTM3U\n#EXT-X-VERSION:{version}\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{}\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-MAP:URI=\"init-{track}.mp4\"\n",
            max.ceil() as u64,
            first.number,
        );
        let disc_seq = self.discontinuities_dropped
            + self
                .segments
                .iter()
                .filter(|s| s.number < first.number && s.discontinuity)
                .count() as u64;
        if disc_seq > 0 {
            let _ = writeln!(out, "#EXT-X-DISCONTINUITY-SEQUENCE:{disc_seq}");
        }
        if parts {
            // PART-TARGET is the *maximum* part duration. Parts are cut at the first
            // frame past the configured length, so they run a little over it; the
            // advertised target must cover the longest one actually published.
            let longest = self
                .window()
                .flat_map(|s| s.parts.get(track).into_iter().flatten())
                .chain(self.parts.get(track).into_iter().flatten())
                .map(|p| p.seconds)
                .fold(self.opts.part_seconds.unwrap_or(0.0), f64::max);
            let part_target = (longest * 1000.0).ceil() / 1000.0;
            let _ = write!(
                out,
                // PART-HOLD-BACK is how far behind the live edge a player stays: at
                // least 3 part targets (RFC 8216bis 4.4.3.8). This is the number
                // that sets low-latency playback latency.
                "#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK={:.3},CAN-SKIP-UNTIL={:.3}\n#EXT-X-PART-INF:PART-TARGET={part_target:.3}\n",
                part_target * 3.0,
                can_skip_until,
            );
        }
        let skipped = start.saturating_sub(first.number);
        if parts && skipped > 0 {
            let _ = writeln!(out, "#EXT-X-SKIP:SKIPPED-SEGMENTS={skipped}");
        }
        for s in self.segments.iter().filter(|s| s.number >= start) {
            let has = s.data.get(track).is_some_and(Option::is_some);
            if s.discontinuity && has {
                out.push_str("#EXT-X-DISCONTINUITY\n");
            }
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
            if self.pending_discontinuity {
                out.push_str("#EXT-X-DISCONTINUITY\n");
            }
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
        match req.part {
            // A part of the segment in progress (`msn == next_number`) is
            // answerable as soon as that part is published: that is the point of
            // a low-latency blocking reload. Waiting for the whole segment would
            // hold the player for up to a segment duration.
            Some(p) => msn <= self.next_number && self.part(track, msn, p).is_some(),
            // A whole segment: it must be complete.
            None => msn < self.next_number,
        }
    }

    /// The RFC 6381 codec string of `track`, or `None` when it has no HLS/DASH
    /// signalling.
    pub fn codec(&self, track: usize) -> Option<String> {
        self.tracks.get(track).and_then(|t| codec_string(&t.info))
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

    /// A **dynamic** DASH MPD for the live presentation, or `None` before the
    /// first segment exists.
    ///
    /// Unlike the VOD MPD this has no `mediaPresentationDuration`: it carries
    /// `availabilityStartTime`, a `minimumUpdatePeriod` (half a segment, so a
    /// player refetches the manifest at roughly the rate segments appear) and a
    /// `SegmentTimeline` over the current sliding window only, with `t` on the
    /// first entry. Segments are named exactly as the HLS ones
    /// (`seg-{track}-{n}.m4s`), so a segment is byte-identical whichever
    /// manifest a client follows.
    pub fn dash_mpd(&self) -> Option<String> {
        if !self.is_ready() {
            return None;
        }
        let ast_ms = self.ast_ms?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0i64, |d| d.as_millis() as i64);
        let seg = self.opts.segment_seconds;
        let window_secs = seg * self.opts.window as f64;
        let low_latency = self.parts_enabled();
        let mut out = String::new();
        out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        let delay = if low_latency {
            String::new()
        } else {
            format!(" suggestedPresentationDelay=\"PT{:.3}S\"", seg * 2.0)
        };
        let _ = writeln!(
            out,
            "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" \
             profiles=\"urn:mpeg:dash:profile:isoff-live:2011\" type=\"dynamic\" \
             availabilityStartTime=\"{}\" publishTime=\"{}\" \
             minimumUpdatePeriod=\"PT{:.3}S\" timeShiftBufferDepth=\"PT{window_secs:.3}S\" \
             maxSegmentDuration=\"PT{:.3}S\" minBufferTime=\"PT{:.3}S\"{delay}>",
            iso8601_utc(ast_ms),
            iso8601_utc(now_ms),
            (seg / 2.0).max(0.5),
            seg.max(0.5) + 1.0,
            (seg / 3.0).max(0.5),
        );
        if low_latency {
            // The latency a player should hold (and may catch up to): a couple of
            // parts behind the live edge, never below a second.
            let part = self.opts.part_seconds.unwrap_or(seg);
            let target = ((seg * 0.75).max(part * 3.0) * 1000.0).round();
            let _ = writeln!(
                out,
                "  <ServiceDescription id=\"0\">\n    \
                 <Latency target=\"{target}\" min=\"{}\" max=\"{}\"/>\n    \
                 <PlaybackRate min=\"0.96\" max=\"1.04\"/>\n  </ServiceDescription>",
                (target / 2.0).round(),
                (target * 2.0).round()
            );
        }
        // Lets a player whose clock differs from ours still find the live edge.
        let _ = writeln!(
            out,
            "  <UTCTiming schemeIdUri=\"urn:mpeg:dash:utc:direct:2014\" value=\"{}\"/>",
            iso8601_utc(now_ms)
        );
        let _ = writeln!(out, "  <Period id=\"0\" start=\"PT0S\">");

        let representation = |out: &mut String, i: usize| -> Option<()> {
            let t = &self.tracks[i];
            let codec = codec_string(&t.info)?;
            let mut bytes = 0u64;
            let mut secs = 0.0f64;
            for s in self.window() {
                if let Some(d) = s.data.get(i).and_then(Option::as_ref) {
                    bytes += d.len() as u64;
                }
                secs += s.seconds[i];
            }
            let bandwidth = if secs > 0.0 {
                (bytes as f64 * 8.0 / secs) as u64
            } else {
                0
            };
            let mut rep = format!(
                "id=\"{i}\" codecs=\"{}\" bandwidth=\"{}\"",
                escape_xml(&codec),
                bandwidth.max(1)
            );
            if t.info.media_type == MediaType::Video {
                let _ = write!(rep, " width=\"{}\" height=\"{}\"", t.info.width, t.info.height);
            } else {
                let _ = write!(rep, " audioSamplingRate=\"{}\"", t.info.sample_rate);
            }
            let _ = writeln!(out, "      <Representation {rep}>");
            if t.info.media_type == MediaType::Audio {
                let _ = writeln!(
                    out,
                    "        <AudioChannelConfiguration \
                     schemeIdUri=\"urn:mpeg:dash:23003:3:audio_channel_configuration:2011\" \
                     value=\"{}\"/>",
                    t.info.channels.max(1)
                );
            }
            let scale = u64::from(t.info.timescale.max(1));
            // Low-latency DASH: a segment may be requested from its start and is
            // then streamed as its CMAF chunks are published. Segments are
            // normally available when they end, so the offset is a whole segment.
            let ll = if low_latency {
                format!(" availabilityTimeOffset=\"{seg:.3}\" availabilityTimeComplete=\"false\"")
            } else {
                String::new()
            };
            let _ = writeln!(
                out,
                "        <SegmentTemplate timescale=\"{scale}\" initialization=\"init-{i}.mp4\" \
                 media=\"seg-{i}-$Number$.m4s\" startNumber=\"{}\"{ll}>",
                self.window().next().map_or(1, |s| s.number)
            );
            let _ = writeln!(out, "          <SegmentTimeline>");
            // `t` is each segment's real decode time (its `tfdt`), stated on every
            // entry so a track that skipped a segment cannot drift. A track with
            // no samples in a segment is skipped.
            for s in self.window() {
                let d = (s.seconds[i] * scale as f64) as u64;
                if d == 0 {
                    continue;
                }
                let _ = writeln!(
                    out,
                    "            <S t=\"{}\" d=\"{d}\"/>",
                    s.start_ticks.get(i).copied().unwrap_or(0)
                );
            }
            // Low-latency DASH: the segment still being published is listed too,
            // starting exactly where the last finished one ended and with its
            // expected length (the next refresh states the true one). Without it a
            // player could only ever ask for segments that are already complete.
            if low_latency && !self.finished {
                if let Some(last) = self.window().last() {
                    let d_last = (last.seconds[i] * scale as f64) as u64;
                    if d_last > 0 {
                        let _ = writeln!(
                            out,
                            "            <S t=\"{}\" d=\"{}\"/>",
                            last.start_ticks.get(i).copied().unwrap_or(0) + d_last,
                            (seg * scale as f64) as u64
                        );
                    }
                }
            }
            let _ = writeln!(out, "          </SegmentTimeline>");
            let _ = writeln!(out, "        </SegmentTemplate>");
            let _ = writeln!(out, "      </Representation>");
            Some(())
        };

        // All video tracks are renditions of one picture: one AdaptationSet with a
        // Representation each, which is what lets a player switch between them.
        let videos: Vec<usize> = (0..self.tracks.len())
            .filter(|&i| self.tracks[i].info.media_type == MediaType::Video)
            .collect();
        if !videos.is_empty() {
            let _ = writeln!(
                out,
                "    <AdaptationSet id=\"0\" contentType=\"video\" segmentAlignment=\"true\" \
                 startWithSAP=\"1\" mimeType=\"video/mp4\">"
            );
            for &i in &videos {
                representation(&mut out, i)?;
            }
            let _ = writeln!(out, "    </AdaptationSet>");
        }
        let mut set_id = 1;
        for i in 0..self.tracks.len() {
            if self.tracks[i].info.media_type == MediaType::Video {
                continue;
            }
            let _ = writeln!(
                out,
                "    <AdaptationSet id=\"{set_id}\" contentType=\"audio\" segmentAlignment=\"true\" \
                 startWithSAP=\"1\" mimeType=\"audio/mp4\">"
            );
            set_id += 1;
            representation(&mut out, i)?;
            let _ = writeln!(out, "    </AdaptationSet>");
        }
        out.push_str("  </Period>\n</MPD>\n");
        Some(out)
    }
}

/// XML-escapes a value for an MPD attribute.
/// `ms` since the Unix epoch as an ISO 8601 UTC date-time (`2026-10-07T01:27:55.123Z`).
fn iso8601_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let milli = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{milli:03}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod iso_tests {
    use super::iso8601_utc;

    #[test]
    fn iso8601_matches_known_instants() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601_utc(1_000_000_000_000), "2001-09-09T01:46:40.000Z");
        // Leap days, including the century one (2000 is a leap year).
        assert_eq!(iso8601_utc(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(iso8601_utc(1_709_208_000_123), "2024-02-29T12:00:00.123Z");
        // Before the epoch rounds toward the earlier second.
        assert_eq!(iso8601_utc(-1), "1969-12-31T23:59:59.999Z");
    }
}
