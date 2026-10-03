//! Streaming MP4 reader.
//!
//! [`Mp4Reader`] opens an MP4 over any [`ReadAt`] source **without loading the
//! file**: it hops over the top-level boxes with 16-byte header reads, reads
//! only the `moov` index into memory, builds a flat per-track sample index in
//! one pass, and then serves each packet with a single positional read.
//!
//! Compared with the original whole-file `Mp4Demuxer` this is
//!
//! * **O(index) memory**, not O(file): opening an 8 GiB MP4 whose `moov` is
//!   200 KiB reads ~200 KiB (and two or three 16-byte headers);
//! * **O(1) per packet** after an O(samples) build — the old code rescanned the
//!   `stsc`/`stss` tables for every sample, which is O(n²) over a file;
//! * **correct about timing** — `ctts` composition offsets are applied, so
//!   `pts != dts` on B-frame streams — and **time-interleaved** across tracks.
//!
//! # Hostile input
//!
//! Every size read from the file is bounded before it is used for an
//! allocation: `moov` is capped at [`MAX_MOOV_BYTES`], a track at
//! [`MAX_SAMPLES_PER_TRACK`] samples, the top-level scan at a fixed number of
//! boxes, and a sample's bytes are only allocated after checking that they lie
//! inside the file.

use anyhow::{anyhow, bail, Result};
use tpt_kinetix_core::{error::KinetixError, packet::Packet, timestamp::Timestamp};

use super::container::{parse_moov_payload, Mp4Track};
use crate::{source::ReadAt, Demuxer};

/// Largest `moov` payload [`Mp4Reader`] will read (256 MiB).
pub const MAX_MOOV_BYTES: u64 = 256 << 20;
/// Largest number of samples accepted in one track (16 Mi).
pub const MAX_SAMPLES_PER_TRACK: usize = 16 << 20;
/// Largest number of top-level boxes scanned while looking for `moov`.
const MAX_TOP_LEVEL_BOXES: usize = 1 << 20;

/// Where one sample lives and when it plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleRef {
    /// Absolute byte offset of the sample in the file.
    pub offset: u64,
    /// Sample size in bytes.
    pub size: u32,
    /// Decode timestamp in track timescale ticks.
    pub dts: u64,
    /// `pts - dts` in track timescale ticks (from `ctts`; 0 when absent).
    pub cts_offset: i32,
    /// Whether this is a sync (key-frame) sample.
    pub is_key: bool,
}

/// A streaming MP4 demuxer over a positional byte source.
pub struct Mp4Reader<S: ReadAt> {
    source: S,
    len: u64,
    tracks: Vec<Mp4Track>,
    index: Vec<Vec<SampleRef>>,
    cursor: Vec<usize>,
}

impl<S: ReadAt> Mp4Reader<S> {
    /// Opens `source`, reading only the box headers and the `moov` index.
    pub fn open(source: S) -> Result<Self> {
        let len = source.len()?;
        let (payload_off, payload_len) = find_moov(&source, len)?;
        let mut payload = vec![0u8; payload_len as usize];
        source.read_at(payload_off, &mut payload)?;
        let tracks = parse_moov_payload(&payload);
        drop(payload);
        let index = tracks.iter().map(build_index).collect::<Result<Vec<_>>>()?;
        let cursor = vec![0; tracks.len()];
        Ok(Self {
            source,
            len,
            tracks,
            index,
            cursor,
        })
    }

    /// The parsed tracks.
    pub fn tracks(&self) -> &[Mp4Track] {
        &self.tracks
    }

    /// The flat sample index of track `track` (in decode order).
    pub fn samples(&self, track: usize) -> &[SampleRef] {
        self.index.get(track).map_or(&[], Vec::as_slice)
    }

    /// Reads the bytes of one sample.
    pub fn read_sample(&self, s: &SampleRef) -> Result<Vec<u8>, KinetixError> {
        let end = s
            .offset
            .checked_add(u64::from(s.size))
            .filter(|&e| e <= self.len)
            .ok_or_else(|| {
                KinetixError::Parse(format!(
                    "sample at {}+{} exceeds file size {}",
                    s.offset, s.size, self.len
                ))
            })?;
        debug_assert!(end <= self.len);
        let mut data = vec![0u8; s.size as usize];
        self.source.read_at(s.offset, &mut data)?;
        Ok(data)
    }

    /// Consumes the reader and returns the underlying source.
    pub fn into_source(self) -> S {
        self.source
    }
}

impl<S: ReadAt> Demuxer for Mp4Reader<S> {
    /// Returns the next packet in decode-time order across all tracks.
    fn read_packet(&mut self) -> Result<Option<Packet>, KinetixError> {
        // The track whose next sample has the smallest DTS (cross-multiplied so
        // different timescales compare exactly); ties go to the lower track.
        let mut best: Option<(usize, u128, u128)> = None;
        for (ti, idx) in self.index.iter().enumerate() {
            let Some(s) = idx.get(self.cursor[ti]) else {
                continue;
            };
            let ts = u128::from(self.tracks[ti].timescale.max(1));
            let better = match best {
                None => true,
                Some((_, bdts, bts)) => u128::from(s.dts) * bts < bdts * ts,
            };
            if better {
                best = Some((ti, u128::from(s.dts), ts));
            }
        }
        let Some((ti, _, _)) = best else {
            return Ok(None);
        };
        let s = self.index[ti][self.cursor[ti]];
        let data = self.read_sample(&s)?;
        self.cursor[ti] += 1;
        let time_base = (1, self.tracks[ti].timescale);
        let dts = s.dts as i64;
        Ok(Some(Packet {
            pts: Timestamp::new(dts + i64::from(s.cts_offset), time_base),
            dts: Timestamp::new(dts, time_base),
            data,
            stream_index: ti as u32,
            is_key_frame: s.is_key,
        }))
    }

    /// Seeks every track to the closest sync sample at or before `target_pts_ms`.
    fn seek(&mut self, target_pts_ms: i64) -> Result<(), KinetixError> {
        for (ti, idx) in self.index.iter().enumerate() {
            let ticks = (i128::from(target_pts_ms) * i128::from(self.tracks[ti].timescale) / 1000)
                .clamp(0, i128::from(u64::MAX)) as u64;
            let mut sample = idx.partition_point(|s| s.dts <= ticks).saturating_sub(1);
            while sample > 0 && !idx[sample].is_key {
                sample -= 1;
            }
            self.cursor[ti] = sample;
        }
        Ok(())
    }
}

/// Locates the `moov` payload: returns `(payload_offset, payload_len)`.
fn find_moov<S: ReadAt>(source: &S, len: u64) -> Result<(u64, u64)> {
    let mut pos = 0u64;
    for _ in 0..MAX_TOP_LEVEL_BOXES {
        if pos + 8 > len {
            break;
        }
        let mut hdr = [0u8; 16];
        let want = (len - pos).min(16) as usize;
        source.read_at(pos, &mut hdr[..want])?;
        let size32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let kind = [hdr[4], hdr[5], hdr[6], hdr[7]];
        let (hlen, size) = match size32 {
            1 => {
                if want < 16 {
                    bail!("truncated 64-bit box header at offset {pos}");
                }
                (16u64, u64::from_be_bytes(hdr[8..16].try_into().unwrap()))
            }
            0 => (8, len - pos), // "extends to end of file"
            n => (8, u64::from(n)),
        };
        if size < hlen {
            bail!("invalid box size {size} at offset {pos}");
        }
        if &kind == b"moov" {
            if size > len - pos {
                bail!("moov box at offset {pos} extends past end of file");
            }
            let payload_len = size - hlen;
            if payload_len > MAX_MOOV_BYTES {
                bail!(
                    "moov payload of {payload_len} bytes exceeds the {MAX_MOOV_BYTES}-byte limit"
                );
            }
            return Ok((pos + hlen, payload_len));
        }
        pos = pos
            .checked_add(size)
            .ok_or_else(|| anyhow!("box size overflow at offset {pos}"))?;
    }
    Err(anyhow!("no moov box found in MP4 data"))
}

/// Run-length iterator over `(count, value)` pairs, yielding `default` once the
/// table is exhausted.
struct Runs<'a, T: Copy> {
    runs: &'a [(u32, T)],
    i: usize,
    left: u32,
    default: T,
}

impl<'a, T: Copy> Runs<'a, T> {
    fn new(runs: &'a [(u32, T)], default: T) -> Self {
        Self {
            runs,
            i: 0,
            left: runs.first().map_or(0, |r| r.0),
            default,
        }
    }

    fn next(&mut self) -> T {
        while self.left == 0 {
            self.i += 1;
            match self.runs.get(self.i) {
                Some(r) => self.left = r.0,
                None => return self.default,
            }
        }
        self.left -= 1;
        self.runs[self.i].1
    }
}

/// Expands a track's sample tables into a flat, decode-ordered index in one
/// pass over the tables (O(samples + chunks)).
fn build_index(track: &Mp4Track) -> Result<Vec<SampleRef>> {
    let n = track.sample_count();
    if n > MAX_SAMPLES_PER_TRACK {
        bail!(
            "track {} has {n} samples (limit {MAX_SAMPLES_PER_TRACK})",
            track.track_id
        );
    }
    if track.stsc.entries.is_empty() {
        return Ok(Vec::new());
    }

    let stts: Vec<(u32, u32)> = track
        .stts
        .entries
        .iter()
        .map(|e| (e.sample_count, e.sample_delta))
        .collect();
    let ctts: Vec<(u32, i32)> = track
        .ctts
        .entries
        .iter()
        .map(|e| (e.sample_count, e.sample_offset))
        .collect();
    let mut deltas = Runs::new(&stts, 0);
    let mut offsets = Runs::new(&ctts, 0);

    let mut is_key = vec![track.stss.is_none(); n];
    if let Some(stss) = &track.stss {
        for &sn in &stss.sample_numbers {
            if let Some(slot) = (sn as usize).checked_sub(1).and_then(|i| is_key.get_mut(i)) {
                *slot = true;
            }
        }
    }

    let size_of = |i: usize| -> u32 {
        if track.stsz.default_size != 0 {
            track.stsz.default_size
        } else {
            track.stsz.sample_sizes[i]
        }
    };

    let mut out = Vec::with_capacity(n);
    let runs = &track.stsc.entries;
    let mut run = 0usize;
    let mut dts = 0u64;
    'chunks: for (ci, &chunk_off) in track.chunk_offsets.iter().enumerate() {
        let chunk_no = ci as u32 + 1;
        while run + 1 < runs.len() && runs[run + 1].first_chunk <= chunk_no {
            run += 1;
        }
        let mut off = chunk_off;
        for _ in 0..runs[run].samples_per_chunk {
            let i = out.len();
            if i >= n {
                break 'chunks;
            }
            let size = size_of(i);
            out.push(SampleRef {
                offset: off,
                size,
                dts,
                cts_offset: offsets.next(),
                is_key: is_key[i],
            });
            dts = dts.saturating_add(u64::from(deltas.next()));
            off = off.saturating_add(u64::from(size));
        }
    }
    // A table that describes fewer chunks than samples simply ends the track
    // early, as the previous implementation did.
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_expand_then_default() {
        let table = [(2u32, 10u32), (1, 7)];
        let mut r = Runs::new(&table, 0);
        let got: Vec<u32> = (0..5).map(|_| r.next()).collect();
        assert_eq!(got, [10, 10, 7, 0, 0]);
        let mut empty = Runs::<i32>::new(&[], -1);
        assert_eq!(empty.next(), -1);
    }

    #[test]
    fn find_moov_rejects_garbage_sizes() {
        // size 4 < header length 8
        let mut data = vec![0, 0, 0, 4];
        data.extend_from_slice(b"free");
        data.extend_from_slice(&[0; 8]);
        assert!(find_moov(&data, data.len() as u64).is_err());
        // no moov at all
        let mut ok = vec![0, 0, 0, 16];
        ok.extend_from_slice(b"free");
        ok.extend_from_slice(&[0; 8]);
        assert!(find_moov(&ok, ok.len() as u64).is_err());
    }
}
