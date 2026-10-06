//! Just-in-time HLS (fMP4) and DASH packaging of MP4 files.
//!
//! A [`Packager`] is built from an MP4's *index* only (see
//! [`tpt_kinetix_demux::Mp4Index`]): a few kilobytes read through any
//! [`AsyncReadAt`] (a local file, HTTP range requests, a `fetch` callback in a
//! Worker). From the index it plans key-frame-aligned segments and can produce,
//! on demand and without any pre-processing:
//!
//! * the HLS master and per-track media playlists ([`Packager::hls_master`],
//!   [`Packager::hls_media`]) and a DASH MPD ([`Packager::dash_mpd`]);
//! * each track's initialization segment ([`Packager::init_segment`]);
//! * any media segment ([`Packager::media_segment`]): a `moof` + `mdat` built
//!   from ranged reads of just that segment's samples.
//!
//! Tracks are packaged as separate renditions (one track per init/segment
//! file), the layout Apple's HLS and DASH players expect. Names:
//! `master.m3u8`, `track-{i}.m3u8`, `init-{i}.mp4`, `seg-{i}-{n}.m4s` (n from 1),
//! `manifest.mpd`.

mod codecs;
mod dash;
mod hls;
pub mod live;
mod plan;
#[cfg(feature = "wasm")]
pub mod wasm;

pub use codecs::codec_string;
pub use live::{CompletedSegment, LiveError, LiveOptions, LivePackager, PlaylistRequest};
pub use plan::{Segment, SegmentPlan};

use std::ops::Range;

use thiserror::Error;
use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_demux::mp4::SampleRef;
use tpt_kinetix_demux::{AsyncReadAt, Mp4Index};
use tpt_kinetix_mux::FragmentWriter;

/// What the packager needs from a demuxed source: where every sample is and
/// what each track is.
///
/// Implemented by [`Mp4Index`] (MP4, progressive or fragmented) and
/// [`MkvIndex`](tpt_kinetix_demux::MkvIndex) (Matroska/WebM), so the same
/// just-in-time HLS/DASH packaging serves both containers.
pub trait SampleIndex {
    /// Total length of the source in bytes (a sanity bound on sample ranges).
    fn file_len(&self) -> u64;

    /// The tracks, in source order.
    fn streams(&self) -> &[StreamInfo];

    /// Number of samples in `track`.
    fn sample_count(&self, track: usize) -> usize;

    /// The samples of `track`, in decode order.
    fn samples(&self, track: usize) -> &[SampleRef];
}

impl SampleIndex for Mp4Index {
    fn file_len(&self) -> u64 {
        Mp4Index::file_len(self)
    }
    fn streams(&self) -> &[StreamInfo] {
        self.stream_infos()
    }
    fn sample_count(&self, track: usize) -> usize {
        Mp4Index::sample_count(self, track)
    }
    fn samples(&self, track: usize) -> &[SampleRef] {
        Mp4Index::samples(self, track)
    }
}

impl SampleIndex for tpt_kinetix_demux::MkvIndex {
    fn file_len(&self) -> u64 {
        self.file_len()
    }
    fn streams(&self) -> &[StreamInfo] {
        self.streams()
    }
    fn sample_count(&self, track: usize) -> usize {
        self.sample_count(track)
    }
    fn samples(&self, track: usize) -> &[SampleRef] {
        self.samples(track)
    }
}

/// Packaging errors.
#[derive(Debug, Error)]
pub enum PackageError {
    /// The input could not be read or parsed.
    #[error("input: {0}")]
    Input(String),
    /// The request does not match the stream (no such track or segment).
    #[error("not found: {0}")]
    NotFound(String),
    /// A fragment could not be built.
    #[error("mux: {0}")]
    Mux(#[from] tpt_kinetix_mux::MuxError),
}

impl From<tpt_kinetix_core::error::KinetixError> for PackageError {
    fn from(e: tpt_kinetix_core::error::KinetixError) -> Self {
        PackageError::Input(e.to_string())
    }
}

/// Packaging options.
#[derive(Debug, Clone)]
pub struct PackagerOptions {
    /// Target segment duration in seconds; segments end at the first key frame
    /// of the lead track at or after this length.
    pub segment_seconds: f64,
    /// Merge two sample reads into one request when the bytes between them are
    /// at most this many (trading wasted bytes for fewer round trips).
    pub max_read_gap: u64,
}

impl Default for PackagerOptions {
    fn default() -> Self {
        Self {
            segment_seconds: 6.0,
            max_read_gap: 256 * 1024,
        }
    }
}

/// A packaged track: its stream info and which sample ranges form each segment.
struct PackagedTrack {
    /// Index of the track in the source file.
    source_index: usize,
    info: StreamInfo,
    codec: String,
}

/// An MP4 (or Matroska/WebM) prepared for on-demand HLS/DASH packaging.
pub struct Packager {
    index: Box<dyn SampleIndex + Send + Sync>,
    tracks: Vec<PackagedTrack>,
    plan: SegmentPlan,
    opts: PackagerOptions,
}

impl Packager {
    /// Loads an MP4 index from `source` and plans segments. Tracks that cannot be
    /// packaged (unknown codec, no codec string, non audio/video) are skipped.
    pub async fn load<S: AsyncReadAt>(
        source: &S,
        opts: PackagerOptions,
    ) -> Result<Self, PackageError> {
        let index = Mp4Index::load(source)
            .await
            .map_err(|e| PackageError::Input(format!("{e:#}")))?;
        Self::from_index(Box::new(index), opts)
    }

    /// Packages a Matroska/WebM source instead of an MP4.
    ///
    /// The index is built with one streaming pass
    /// ([`MkvIndex::open`](tpt_kinetix_demux::MkvIndex::open)); segments are
    /// then read by offset exactly as for MP4.
    pub fn load_mkv<S: tpt_kinetix_demux::ReadAt + ?Sized>(
        source: &S,
        opts: PackagerOptions,
    ) -> Result<Self, PackageError> {
        let index = tpt_kinetix_demux::MkvIndex::open(source)
            .map_err(|e| PackageError::Input(format!("{e:#}")))?;
        Self::from_index(Box::new(index), opts)
    }

    /// Builds a packager from an already-loaded index.
    pub fn from_index(
        index: Box<dyn SampleIndex + Send + Sync>,
        opts: PackagerOptions,
    ) -> Result<Self, PackageError> {
        let mut tracks = Vec::new();
        for (i, info) in index.streams().iter().cloned().enumerate() {
            let packagable = matches!(info.media_type, MediaType::Video | MediaType::Audio)
                && !matches!(info.codec, CodecId::Unknown(_))
                && index.sample_count(i) > 0;
            let Some(codec) = packagable.then(|| codec_string(&info)).flatten() else {
                continue;
            };
            tracks.push(PackagedTrack {
                source_index: i,
                info,
                codec,
            });
        }
        if tracks.is_empty() {
            return Err(PackageError::Input(
                "no packageable video or audio track".into(),
            ));
        }
        let sources: Vec<usize> = tracks.iter().map(|t| t.source_index).collect();
        let plan = plan::plan_segments(index.as_ref(), &sources, opts.segment_seconds);
        Ok(Self {
            index,
            tracks,
            plan,
            opts,
        })
    }

    /// The packaged streams, indexed by *packaged track number* (the `i` in
    /// `track-{i}.m3u8`).
    pub fn streams(&self) -> Vec<&StreamInfo> {
        self.tracks.iter().map(|t| &t.info).collect()
    }

    /// RFC 6381 codec string of packaged track `track`.
    pub fn codec(&self, track: usize) -> Option<&str> {
        self.tracks.get(track).map(|t| t.codec.as_str())
    }

    /// The segment plan.
    pub fn plan(&self) -> &SegmentPlan {
        &self.plan
    }

    /// Number of segments (identical for every track).
    pub fn segment_count(&self) -> usize {
        self.plan.segments.len()
    }

    fn track(&self, track: usize) -> Result<&PackagedTrack, PackageError> {
        self.tracks
            .get(track)
            .ok_or_else(|| PackageError::NotFound(format!("track {track}")))
    }

    /// The init segment of `track`: `ftyp` + `moov` with codec configuration.
    pub fn init_segment(&self, track: usize) -> Result<Vec<u8>, PackageError> {
        let t = self.track(track)?;
        let mut info = t.info.clone();
        info.index = 0;
        Ok(FragmentWriter::new(&[info])?.init_segment())
    }

    /// Segment `n` (1-based) of `track` as `moof` + `mdat`, reading only that
    /// segment's samples from `source`.
    pub async fn media_segment<S: AsyncReadAt>(
        &self,
        source: &S,
        track: usize,
        n: usize,
    ) -> Result<Vec<u8>, PackageError> {
        let t = self.track(track)?;
        let seg = n
            .checked_sub(1)
            .and_then(|i| self.plan.segments.get(i))
            .ok_or_else(|| PackageError::NotFound(format!("segment {n}")))?;
        let range = seg.samples[track].clone();
        let samples = &self.index.samples(t.source_index)[range];
        if samples.is_empty() {
            return Err(PackageError::NotFound(format!(
                "segment {n} of track {track} is empty"
            )));
        }
        let data = self.read_samples(source, samples).await?;

        let mut info = t.info.clone();
        info.index = 0;
        let mut w = FragmentWriter::new(&[info])?.starting_sequence(n as u32);
        let scale = t.info.timescale;
        for (s, bytes) in samples.iter().zip(data) {
            let dts = s.dts as i64;
            let tb = (1, scale);
            w.push(
                &Packet {
                    pts: Timestamp::new(dts + i64::from(s.cts_offset), tb),
                    dts: Timestamp::new(dts, tb),
                    data: bytes,
                    stream_index: 0,
                    is_key_frame: s.is_key,
                },
                Some(s.duration),
            )?;
        }
        w.flush(true)?
            .ok_or_else(|| PackageError::NotFound(format!("segment {n} produced no data")))
    }

    /// Reads `samples`, merging nearby ranges into single requests.
    async fn read_samples<S: AsyncReadAt>(
        &self,
        source: &S,
        samples: &[SampleRef],
    ) -> Result<Vec<Vec<u8>>, PackageError> {
        let mut out: Vec<Vec<u8>> = Vec::with_capacity(samples.len());
        let mut i = 0;
        while i < samples.len() {
            // Grow a run while the next sample starts within `max_read_gap` bytes
            // after the current run's end (and does not go backwards).
            let start = samples[i].offset;
            let mut end = start + u64::from(samples[i].size);
            let mut j = i + 1;
            while j < samples.len() {
                let s = &samples[j];
                if s.offset < end || s.offset - end > self.opts.max_read_gap {
                    break;
                }
                end = s.offset + u64::from(s.size);
                j += 1;
            }
            if end > self.index.file_len() {
                return Err(PackageError::Input(format!(
                    "sample range {start}..{end} exceeds the file"
                )));
            }
            let mut buf = vec![0u8; (end - start) as usize];
            source
                .read_at(start, &mut buf)
                .await
                .map_err(|e| PackageError::Input(e.to_string()))?;
            for s in &samples[i..j] {
                let at = (s.offset - start) as usize;
                out.push(buf[at..at + s.size as usize].to_vec());
            }
            i = j;
        }
        Ok(out)
    }

    /// The HLS master playlist. `track-{i}.m3u8` URIs are relative.
    pub fn hls_master(&self) -> String {
        hls::master(self)
    }

    /// The HLS media playlist of `track`.
    pub fn hls_media(&self, track: usize) -> Result<String, PackageError> {
        self.track(track)?;
        Ok(hls::media(self, track))
    }

    /// The DASH MPD.
    pub fn dash_mpd(&self) -> String {
        dash::mpd(self)
    }

    /// Duration of segment `n` (1-based) of `track`, in seconds.
    pub fn segment_seconds(&self, track: usize, n: usize) -> f64 {
        n.checked_sub(1)
            .and_then(|i| self.plan.segments.get(i))
            .map_or(0.0, |s| s.seconds[track])
    }

    /// Average sample rate of `track` (frames per second for video), from the
    /// samples' own durations.
    pub(crate) fn frame_rate(&self, track: usize) -> f64 {
        let t = &self.tracks[track];
        let samples = self.index.samples(t.source_index);
        let ticks: u64 = samples.iter().map(|s| u64::from(s.duration)).sum();
        if ticks == 0 {
            0.0
        } else {
            samples.len() as f64 * f64::from(t.info.timescale) / ticks as f64
        }
    }

    /// Total bytes of the samples of `track` in `range` (for bandwidth estimates).
    pub(crate) fn range_bytes(&self, track: usize, range: &Range<usize>) -> u64 {
        self.index.samples(self.tracks[track].source_index)[range.clone()]
            .iter()
            .map(|s| u64::from(s.size))
            .sum()
    }

    /// Total duration of the presentation in seconds (longest track).
    pub fn duration_seconds(&self) -> f64 {
        self.tracks
            .iter()
            .filter_map(|t| t.info.duration_seconds())
            .fold(0.0, f64::max)
    }
}
