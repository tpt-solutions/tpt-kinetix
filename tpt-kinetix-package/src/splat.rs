//! Time-slab segments for Gaussian splat sequences.
//!
//! A sequence (`SPLS`, or an MP4 `spls` track) is cut into one standalone
//! `SPLS` segment per time slab. Each segment is a valid sequence covering its
//! own time range, so a client can fetch and decode just the slabs for the
//! time it needs. [`splat_playlist`] lists the segments as JSON, with each
//! segment's time range, size and URI.

use tpt_kinetix_splat::{read_sequence, sequence_config, sequence_from_config};

use crate::PackageError;

/// One time slab in a [`SplatPlaylist`].
#[derive(Debug, Clone, PartialEq)]
pub struct SplatSegment {
    /// Position in the sequence, from 0.
    pub index: usize,
    /// Start of the slab's time range.
    pub t_start: f32,
    /// End of the slab's time range.
    pub t_end: f32,
    /// Splats in this segment.
    pub members: u32,
    /// Size of the segment file in bytes.
    pub bytes: u64,
    /// Relative URI of the segment file.
    pub uri: String,
}

/// The segment list for a sequence.
#[derive(Debug, Clone, PartialEq)]
pub struct SplatPlaylist {
    /// Slab duration in the sequence's time units.
    pub slab_duration: f32,
    /// Segments in time order.
    pub segments: Vec<SplatSegment>,
}

impl SplatPlaylist {
    /// Index of the first segment whose time range contains `t`.
    #[must_use]
    pub fn segment_at(&self, t: f32) -> Option<usize> {
        self.segments
            .iter()
            .position(|s| s.t_start <= t && t <= s.t_end)
    }

    /// The playlist as JSON. Every value is generated and validated here, so no
    /// escaping is needed.
    #[must_use]
    pub fn to_json(&self) -> String {
        let segments: Vec<String> = self
            .segments
            .iter()
            .map(|s| {
                format!(
                    "{{\"index\":{},\"t_start\":{},\"t_end\":{},\"members\":{},\"bytes\":{},\"uri\":\"{}\"}}",
                    s.index, s.t_start, s.t_end, s.members, s.bytes, s.uri
                )
            })
            .collect();
        format!(
            "{{\"slab_duration\":{},\"segments\":[{}]}}",
            self.slab_duration,
            segments.join(",")
        )
    }
}

/// Builds the segment list for an `SPLS` file. Segment `k` is named
/// `seg-{k}.spls`.
///
/// # Errors
///
/// [`PackageError::Input`] if the bytes are not a valid sequence.
pub fn splat_playlist(spls: &[u8]) -> Result<SplatPlaylist, PackageError> {
    let seq = read_sequence(spls).map_err(|e| PackageError::Input(format!("{e}")))?;
    let mut segments = Vec::with_capacity(seq.entries().len());
    for (index, e) in seq.entries().iter().enumerate() {
        segments.push(SplatSegment {
            index,
            t_start: e.t_start,
            t_end: e.t_end,
            members: e.members,
            bytes: segment_bytes_len(&seq, index)?,
            uri: format!("seg-{index}.spls"),
        });
    }
    Ok(SplatPlaylist {
        slab_duration: seq.slab_duration(),
        segments,
    })
}

/// Length of segment `k`'s file: the header, one index entry, and its payload.
fn segment_bytes_len(seq: &tpt_kinetix_splat::Sequence<'_>, k: usize) -> Result<u64, PackageError> {
    let payload = seq
        .slab_payload(k)
        .map_err(|e| PackageError::Input(format!("{e}")))?;
    Ok((20 + 28 + payload.len()) as u64)
}

/// The standalone `SPLS` file for segment `k`: a sequence of one slab whose
/// time range matches that slab.
///
/// # Errors
///
/// [`PackageError::NotFound`] if `k` is out of range; [`PackageError::Input`]
/// if the sequence is not valid.
pub fn splat_segment(spls: &[u8], k: usize) -> Result<Vec<u8>, PackageError> {
    let seq = read_sequence(spls).map_err(|e| PackageError::Input(format!("{e}")))?;
    let entry = seq
        .entries()
        .get(k)
        .ok_or_else(|| PackageError::NotFound(format!("segment {k}")))?;
    let mut config = sequence_config(&seq).map_err(|e| PackageError::Input(format!("{e}")))?;
    // The config's start time is the sequence's first slab. Point it at this
    // slab, so the segment's single entry starts at the same time.
    config[12..16].copy_from_slice(&entry.t_start.to_le_bytes());
    let payload = seq
        .slab_payload(k)
        .map_err(|e| PackageError::Input(format!("{e}")))?;
    sequence_from_config(&config, &[payload]).map_err(|e| PackageError::Input(format!("{e}")))
}
