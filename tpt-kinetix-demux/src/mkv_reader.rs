//! Streaming Matroska / WebM reader over a [`ReadAt`] source.
//!
//! [`MkvReader`] is the file-shaped counterpart of [`MkvStream`]: it makes one
//! sequential pass over the source with [`MkvStream`] to build a frame index
//! (offset, size, timestamp, key flag), keeping only that index in memory, and
//! then serves each packet with a single positional read. Frame payloads are
//! never all resident, so opening an 8 GiB WebM costs no more memory than a
//! short one.
//!
//! Because Matroska carries presentation timestamps only and has no reordering
//! at the container level, `dts == pts`. Frames are returned in file order,
//! which is timestamp order for the muxers that write Matroska.
//!
//! The index pass reads the whole file once: Matroska has no `moov`-equivalent
//! index to seek to, so unlike MP4 this is not a two-round-trip probe. It is
//! still a single streaming pass with O(frames) memory, which is what a
//! packager needs.
//!
//! # Hostile input
//!
//! All limits of [`MkvStream`] apply: leaf elements are capped, laced blocks are
//! rejected, and the frame count is bounded by [`MAX_FRAMES`].

use tpt_kinetix_core::error::KinetixError;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_core::timestamp::Timestamp;

use crate::source::ReadAt;
use crate::Demuxer;

use super::mkv_stream::{MkvEvent, MkvFrame, MkvStream};

/// Largest number of frames accepted in the index (4 Mi).
pub const MAX_FRAMES: usize = 4 << 20;

/// Bytes read per `read_at` during the indexing pass.
const SCAN_CHUNK: usize = 256 * 1024;

/// One frame of a Matroska file, located but not loaded.
#[derive(Debug, Clone, PartialEq)]
pub struct MkvSample {
    /// Index of the track in [`MkvReader::streams`].
    pub stream: usize,
    /// Absolute byte offset of the payload.
    pub offset: u64,
    /// Payload size in bytes.
    pub size: u32,
    /// Presentation (= decode) time in milliseconds.
    pub pts_ms: i64,
    /// Whether the frame is a random-access point.
    pub is_key: bool,
}

/// A Matroska / WebM file indexed over a [`ReadAt`] source.
pub struct MkvReader<S: ReadAt> {
    source: S,
    streams: Vec<StreamInfo>,
    samples: Vec<MkvSample>,
    cursor: usize,
}

impl<S: ReadAt> MkvReader<S> {
    /// Indexes `source` with one sequential pass.
    ///
    /// Fails when the file is not Matroska or has no AV1, VP9 or Opus track.
    pub fn open(source: S) -> Result<Self, KinetixError> {
        let len = source.len()?;
        let mut parser = MkvStream::new();
        let mut streams: Vec<StreamInfo> = Vec::new();
        let mut samples: Vec<MkvSample> = Vec::new();
        let mut at = 0u64;
        let cap = usize::try_from(len)
            .unwrap_or(SCAN_CHUNK)
            .clamp(1, SCAN_CHUNK);
        let mut buf = vec![0u8; cap];
        while at < len {
            let want = usize::try_from(len - at).unwrap_or(SCAN_CHUNK).min(cap);
            source.read_at(at, &mut buf[..want])?;
            let events = parser.push(&buf[..want])?;
            at += want as u64;
            collect(&events, &mut streams, &mut samples)?;
        }
        collect(&parser.finish()?, &mut streams, &mut samples)?;
        Ok(Self {
            source,
            streams,
            samples,
            cursor: 0,
        })
    }

    /// Codec-agnostic descriptions of every track.
    pub fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    /// Number of frames.
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// The frame index, in file order.
    pub fn samples(&self) -> &[MkvSample] {
        &self.samples
    }

    /// The frames of track `stream`, in file order.
    pub fn samples_of(&self, stream: usize) -> Vec<&MkvSample> {
        self.samples.iter().filter(|s| s.stream == stream).collect()
    }

    /// Time of the last frame in milliseconds (0 when empty).
    pub fn duration_ms(&self) -> i64 {
        self.samples.last().map_or(0, |s| s.pts_ms)
    }

    /// Reads the payload of `s` with one positional read.
    pub fn read_sample(&self, s: &MkvSample) -> Result<Vec<u8>, KinetixError> {
        let size =
            usize::try_from(s.size).map_err(|_| KinetixError::Parse("frame too large".into()))?;
        let mut data = vec![0u8; size];
        self.source.read_at(s.offset, &mut data)?;
        Ok(data)
    }

    /// Consumes the reader and returns the underlying source.
    pub fn into_source(self) -> S {
        self.source
    }
}

fn collect(
    events: &[MkvEvent],
    streams: &mut Vec<StreamInfo>,
    samples: &mut Vec<MkvSample>,
) -> Result<(), KinetixError> {
    for e in events {
        match e {
            MkvEvent::Tracks(t) => {
                if streams.is_empty() {
                    *streams = t.clone();
                }
            }
            MkvEvent::Frame(f) => {
                samples.push(sample_of(f)?);
                if samples.len() > MAX_FRAMES {
                    return Err(KinetixError::Parse(format!(
                        "more than {MAX_FRAMES} frames"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn sample_of(f: &MkvFrame) -> Result<MkvSample, KinetixError> {
    Ok(MkvSample {
        stream: f.stream,
        offset: f.offset,
        size: u32::try_from(f.data.len())
            .map_err(|_| KinetixError::Parse("frame larger than 4 GiB".into()))?,
        pts_ms: f.pts_ms,
        is_key: f.key,
    })
}

impl<S: ReadAt> Demuxer for MkvReader<S> {
    /// Returns the next frame in file order, or `Ok(None)` at the end.
    fn read_packet(&mut self) -> Result<Option<Packet>, KinetixError> {
        let Some(s) = self.samples.get(self.cursor) else {
            return Ok(None);
        };
        self.cursor += 1;
        let timescale = self
            .streams
            .get(s.stream)
            .map_or(1000, |i| i.timescale.max(1));
        let time_base = (1, timescale);
        // Matroska timestamps are milliseconds; re-express them in track ticks.
        let dts = s.pts_ms * i64::from(timescale) / 1000;
        Ok(Some(Packet {
            pts: Timestamp::new(dts, time_base),
            dts: Timestamp::new(dts, time_base),
            data: self.read_sample(s)?,
            stream_index: s.stream as u32,
            is_key_frame: s.is_key,
        }))
    }

    /// Seeks to the closest key frame at or before `target_pts_ms`.
    fn seek(&mut self, target_pts_ms: i64) -> Result<(), KinetixError> {
        self.cursor = self
            .samples
            .iter()
            .enumerate()
            .rev()
            .find(|(_, s)| s.pts_ms <= target_pts_ms && s.is_key)
            .map_or(0, |(i, _)| i);
        Ok(())
    }
}
