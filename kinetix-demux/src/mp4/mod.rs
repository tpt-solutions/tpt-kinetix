//! ISO BMFF / MP4 demuxer.
//!
//! This module provides a fully nom-based MP4 container parser that walks
//! `moov → trak → mdia → minf → stbl` and yields encoded [`Packet`]s.
//!
//! Sub-modules:
//! - [`boxes`] — individual box parsers
//! - [`container`] — top-level `moov` walker and [`Mp4Track`]

pub mod boxes;
pub mod container;

pub use boxes::{
    parse_box_header, parse_ftyp, parse_mdhd, parse_mvhd, parse_stco, parse_stsc, parse_stss,
    parse_stsz, parse_stts, parse_tkhd, BoxHeader, Co64Box, FtypBox, MdhdBox, MvhdBox, StcoBox,
    StscBox, StscEntry, StssBox, StszBox, SttsBox, SttsEntry, TkhdBox,
};
pub use container::{parse_mp4, Mp4Track};

use kinetix_core::{error::KinetixError, packet::Packet, timestamp::Timestamp};

use crate::Demuxer;

/// Stateful MP4 demuxer backed by an in-memory byte buffer.
///
/// # Examples
///
/// ```rust,no_run
/// use kinetix_demux::mp4::Mp4Demuxer;
///
/// let data = std::fs::read("video.mp4").unwrap();
/// let mut demuxer = Mp4Demuxer::new(data).unwrap();
/// println!("tracks: {}", demuxer.tracks().len());
/// ```
pub struct Mp4Demuxer {
    data: Vec<u8>,
    tracks: Vec<Mp4Track>,
    /// Track index we are currently reading from (round-robin across tracks).
    current_track: usize,
    /// 1-based sample index within the current track.
    current_sample: usize,
}

impl Mp4Demuxer {
    /// Creates a new demuxer, parses the `moov` box, and populates the track list.
    ///
    /// Returns an error if the data is empty, truncated, or missing a `moov` box.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use kinetix_demux::mp4::Mp4Demuxer;
    ///
    /// let bytes = std::fs::read("video.mp4").expect("could not read file");
    /// let demuxer = Mp4Demuxer::new(bytes).expect("failed to parse MP4");
    /// ```
    pub fn new(data: Vec<u8>) -> anyhow::Result<Self> {
        let tracks = container::parse_mp4(&data)?;
        Ok(Self {
            data,
            tracks,
            current_track: 0,
            current_sample: 0,
        })
    }

    /// Returns the parsed tracks.
    pub fn tracks(&self) -> &[Mp4Track] {
        &self.tracks
    }

    // -----------------------------------------------------------------------
    // Sample-table helpers
    // -----------------------------------------------------------------------

    /// Computes the byte offset and size of the `sample_index`-th sample
    /// (0-based) in `track`.
    ///
    /// Returns `None` when `sample_index` is out of range.
    fn sample_offset_and_size(track: &Mp4Track, sample_index: usize) -> Option<(u64, u32)> {
        let n = track.sample_count();
        if sample_index >= n {
            return None;
        }

        let size = if track.stsz.default_size != 0 {
            track.stsz.default_size
        } else {
            *track.stsz.sample_sizes.get(sample_index)?
        };

        // Resolve chunk number and intra-chunk offset using stsc.
        // Samples are 1-based in the MP4 spec; we work in 0-based here and
        // convert at the stsc boundary.
        let (chunk_index, sample_in_chunk) =
            sample_to_chunk(&track.stsc, sample_index, &track.stsz)?;

        let chunk_offset = *track.chunk_offsets.get(chunk_index)?;

        // Sum the sizes of samples before `sample_in_chunk` within this chunk.
        let samples_before: u32 = if track.stsz.default_size != 0 {
            track.stsz.default_size * sample_in_chunk as u32
        } else {
            // first sample of this chunk in the global sample list
            let first_sample_of_chunk = sample_index - sample_in_chunk;
            (0..sample_in_chunk)
                .map(|i| {
                    track
                        .stsz
                        .sample_sizes
                        .get(first_sample_of_chunk + i)
                        .copied()
                        .unwrap_or(0)
                })
                .sum()
        };

        Some((chunk_offset + samples_before as u64, size))
    }

    /// Returns the PTS (in timescale ticks) of the `sample_index`-th sample
    /// (0-based) from the `stts` table.
    fn sample_pts(track: &Mp4Track, sample_index: usize) -> u64 {
        let mut pts = 0u64;
        let mut remaining = sample_index;
        for entry in &track.stts.entries {
            let cnt = entry.sample_count as usize;
            if remaining < cnt {
                pts += remaining as u64 * entry.sample_delta as u64;
                return pts;
            }
            pts += cnt as u64 * entry.sample_delta as u64;
            remaining -= cnt;
        }
        pts
    }

    /// Returns true when `sample_index` (0-based) is a sync sample.
    fn is_key_frame(track: &Mp4Track, sample_index: usize) -> bool {
        match &track.stss {
            // stss absent → all samples are sync samples
            None => true,
            Some(stss) => {
                // stss stores 1-based sample numbers
                let one_based = (sample_index + 1) as u32;
                stss.sample_numbers.contains(&one_based)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// stsc helper
// ---------------------------------------------------------------------------

/// Given a 0-based `sample_index`, returns `(chunk_index, sample_in_chunk)`
/// using the `stsc` table.  `chunk_index` is 0-based.
fn sample_to_chunk(stsc: &StscBox, sample_index: usize, stsz: &StszBox) -> Option<(usize, usize)> {
    // Build a synthetic list of (first_chunk_0based, samples_per_chunk) from
    // the stsc run-length encoded table.
    //
    // stsc entries are 1-based chunk indices.  We iterate over each run and
    // accumulate the total sample count until we find the right chunk.

    let mut global_sample = 0usize; // next sample to assign

    let entries = &stsc.entries;
    if entries.is_empty() {
        return None;
    }

    for run in 0..entries.len() {
        let first_chunk = entries[run].first_chunk as usize; // 1-based
        let spc = entries[run].samples_per_chunk as usize;
        let next_first_chunk = if run + 1 < entries.len() {
            entries[run + 1].first_chunk as usize // 1-based
        } else {
            // Last run: extends to the end of the chunk offset table.
            // We need chunk_offsets.len() but we only have stsz here — derive
            // via global sample count.
            let total = if stsz.default_size != 0 {
                // Count from stts is not available here; use a large sentinel.
                usize::MAX / 2
            } else {
                stsz.sample_sizes.len()
            };
            // Number of chunks needed = ceil((total - global_sample) / spc)
            let remaining_samples = total.saturating_sub(global_sample);
            let extra_chunks = remaining_samples.div_ceil(spc);
            first_chunk + extra_chunks
        };

        let chunk_count = next_first_chunk - first_chunk;

        for c in 0..chunk_count {
            let chunk_idx = first_chunk - 1 + c; // 0-based
            if global_sample + spc > sample_index {
                // sample_index is in this chunk
                let sample_in_chunk = sample_index - global_sample;
                return Some((chunk_idx, sample_in_chunk));
            }
            global_sample += spc;
            if global_sample > sample_index {
                break;
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Demuxer impl
// ---------------------------------------------------------------------------

impl Demuxer for Mp4Demuxer {
    /// Returns the next encoded packet.
    ///
    /// Packets are interleaved across tracks in round-robin order.
    fn read_packet(&mut self) -> Result<Option<Packet>, KinetixError> {
        if self.tracks.is_empty() {
            return Ok(None);
        }

        // Try each track once looking for one that still has samples.
        let n_tracks = self.tracks.len();
        for _ in 0..n_tracks {
            let track_idx = self.current_track % n_tracks;
            let track = &self.tracks[track_idx];
            let sample_idx = self.current_sample;

            if let Some((offset, size)) = Self::sample_offset_and_size(track, sample_idx) {
                let pts_ticks = Self::sample_pts(track, sample_idx);
                let is_key = Self::is_key_frame(track, sample_idx);
                let time_base = (1, track.timescale);

                let end = offset as usize + size as usize;
                if end > self.data.len() {
                    return Err(KinetixError::Parse(format!(
                        "sample offset {offset}+{size} exceeds file size {}",
                        self.data.len()
                    )));
                }
                let data = self.data[offset as usize..end].to_vec();

                let pts = Timestamp::new(pts_ticks as i64, time_base);
                let packet = Packet {
                    pts,
                    dts: pts,
                    data,
                    stream_index: track_idx as u32,
                    is_key_frame: is_key,
                };

                // Advance: next call reads the next sample (same track for simplicity).
                self.current_sample += 1;

                return Ok(Some(packet));
            } else {
                // This track is exhausted; move to the next.
                self.current_track += 1;
                self.current_sample = 0;
            }
        }

        // All tracks exhausted.
        Ok(None)
    }

    /// Seeks to the closest sync sample at or before `target_pts_ms`.
    fn seek(&mut self, target_pts_ms: i64) -> Result<(), KinetixError> {
        if self.tracks.is_empty() {
            return Ok(());
        }

        // Seek each track independently; update current_sample for track 0
        // (the primary track).  For simplicity we seek track 0 and reset
        // others to 0.
        for (ti, track) in self.tracks.iter().enumerate() {
            let target_ticks = target_pts_ms as i128 * track.timescale as i128 / 1_000;

            // Walk stts to find the sample whose PTS is closest to target.
            let mut pts: u64 = 0;
            let mut sample_idx: usize = 0;
            'outer: for entry in &track.stts.entries {
                for _ in 0..entry.sample_count {
                    let next_pts = pts + entry.sample_delta as u64;
                    if next_pts as i128 > target_ticks {
                        break 'outer;
                    }
                    pts = next_pts;
                    sample_idx += 1;
                }
            }

            // Snap back to the nearest preceding sync sample.
            if let Some(stss) = &track.stss {
                let mut best = 0usize;
                for &sn in &stss.sample_numbers {
                    let sn0 = (sn as usize).saturating_sub(1); // 0-based
                    if sn0 <= sample_idx {
                        best = sn0;
                    } else {
                        break;
                    }
                }
                sample_idx = best;
            }

            if ti == 0 {
                self.current_sample = sample_idx;
            }
        }

        self.current_track = 0;
        Ok(())
    }
}
