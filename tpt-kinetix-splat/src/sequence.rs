//! `SPLS`: a 4D splat sequence, split into independently decodable time slabs.
//!
//! # Layout
//!
//! ```text
//! 0    magic         b"SPLS"
//! 4    version       u8 = 1
//! 5    flags         u8: bit 0 lossy
//! 6    sh_degree     u8 (0..=3)
//! 7    reserved      u8 = 0
//! 8    slab_count    u32 LE
//! 12   pos_bits      u8  (0 when lossless, else 1..=24)
//! 13   attr_bits     u8  (0 when lossless, else 1..=24)
//! 14   reserved      u16 = 0
//! 16   slab_duration f32 LE
//! 20   index         slab_count × 28 bytes:
//!                      t_start f32, t_end f32, offset u64, length u64, members u32
//! ..   payloads      one per slab, at the offsets in the index
//! ```
//!
//! A slab payload is `members u32`, then (lossy only) 17 + 3K channel ranges,
//! then one rANS stream per channel. Channels are, in order: mean (4), log
//! scale (4), left quaternion (4), right quaternion (4), opacity (1), SH
//! (3K). Slab contents are in Hilbert order, and decoding returns them in that
//! same order.
//!
//! # Seeking
//!
//! The index gives each slab's time range and byte range, so a reader can find
//! the slabs for a time and decode only those.

use tpt_kinetix_bitstream::rans::RansStreamSet;

use crate::block::{decode_block, encode_block, read_ranges, write_ranges, Layout};
use crate::chunk::{chunk_cloud, temporal_chunks, MAX_SLABS};
use crate::cloud::{sh_coeffs_for_degree, MAX_SH_DEGREE};
use crate::cloud4d::GaussianCloud4D;
use crate::error::SplatError;
use crate::splt::Quantization;

const MAGIC: [u8; 4] = *b"SPLS";
const VERSION: u8 = 1;
const FLAG_LOSSY: u8 = 0b01;
const HEADER_LEN: usize = 20;
const INDEX_ENTRY_LEN: usize = 28;
/// Largest points in one slab, bounding decoder allocation per slab.
const MAX_SLAB_POINTS: usize = 1 << 24;
/// Leading channels that hold the 4D mean.
const POSITION_CHANNELS: usize = 4;

/// Channels per splat for a degree: 4 + 4 + 4 + 4 + 1 + 3·K.
fn channel_count(degree: u8) -> usize {
    17 + 3 * sh_coeffs_for_degree(degree)
}

/// One entry of the slab index.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlabEntry {
    /// Start of the slab's time range.
    pub t_start: f32,
    /// End of the slab's time range.
    pub t_end: f32,
    /// Byte offset of the slab payload from the start of the file.
    pub offset: u64,
    /// Byte length of the slab payload.
    pub length: u64,
    /// Number of splats in the slab.
    pub members: u32,
}

/// Serialises a 4D cloud as a time-sliced sequence.
///
/// The cloud is cut into slabs of `slab` time units. A splat goes into every
/// slab where its temporal weight exceeds `eps` (see [`temporal_chunks`]).
///
/// # Errors
///
/// [`SplatError::Invalid`] for bad slab or `eps` values, bad depths, or a
/// degenerate splat. Chunking errors pass through.
pub fn write_sequence(
    cloud: &GaussianCloud4D,
    slab: f32,
    eps: f32,
    quant: Quantization,
) -> Result<Vec<u8>, SplatError> {
    quant.validate()?;
    let chunks = temporal_chunks(cloud, slab, eps)?;
    let c = channel_count(cloud.sh_degree());
    let layout = Layout {
        channels: c,
        position_channels: POSITION_CHANNELS,
    };

    let mut payloads = Vec::with_capacity(chunks.len());
    let mut entries = Vec::with_capacity(chunks.len());
    for chunk in &chunks {
        let sub = chunk_cloud(cloud, chunk)?;
        let n = sub.len();
        if n > MAX_SLAB_POINTS {
            return Err(SplatError::Invalid("too many splats in one slab"));
        }
        let data = rows(&sub);
        let (ranges, streams) = encode_block(&data, n, layout, quant)?;
        let mut payload = Vec::new();
        payload.extend_from_slice(&(n as u32).to_le_bytes());
        if matches!(quant, Quantization::Lossy { .. }) {
            write_ranges(&mut payload, &ranges);
        }
        payload.extend_from_slice(
            &RansStreamSet::frame(&streams)
                .map_err(|_| SplatError::Invalid("too many SPLS streams"))?,
        );
        entries.push((chunk.t_start, chunk.t_end, n as u32, payload.len() as u64));
        payloads.push(payload);
    }

    let index_end = HEADER_LEN + entries.len() * INDEX_ENTRY_LEN;
    let mut out = Vec::with_capacity(index_end + payloads.iter().map(Vec::len).sum::<usize>());
    let (pos_bits, attr_bits) = quant.depths();
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.push(if matches!(quant, Quantization::Lossy { .. }) {
        FLAG_LOSSY
    } else {
        0
    });
    out.push(cloud.sh_degree());
    out.push(0);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    out.push(pos_bits);
    out.push(attr_bits);
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&slab.to_le_bytes());

    let mut offset = index_end as u64;
    for &(t_start, t_end, members, length) in &entries {
        out.extend_from_slice(&t_start.to_le_bytes());
        out.extend_from_slice(&t_end.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&members.to_le_bytes());
        offset += length;
    }
    for p in &payloads {
        out.extend_from_slice(p);
    }
    Ok(out)
}

/// Flattens a 4D cloud into `count × C` values, one row per splat.
fn rows(cloud: &GaussianCloud4D) -> Vec<f32> {
    let k3 = 3 * sh_coeffs_for_degree(cloud.sh_degree());
    let c = channel_count(cloud.sh_degree());
    let mut out = Vec::with_capacity(cloud.len() * c);
    for i in 0..cloud.len() {
        out.extend_from_slice(&cloud.means()[i]);
        out.extend_from_slice(&cloud.log_scales()[i]);
        out.extend_from_slice(&cloud.rot_left()[i]);
        out.extend_from_slice(&cloud.rot_right()[i]);
        out.push(cloud.opacities()[i]);
        out.extend_from_slice(&cloud.sh()[i * k3..(i + 1) * k3]);
    }
    out
}

/// A parsed `SPLS` file: the header and slab index, with a borrow of the bytes
/// for decoding individual slabs.
#[derive(Debug, Clone)]
pub struct Sequence<'a> {
    data: &'a [u8],
    degree: u8,
    quant: Quantization,
    slab_duration: f32,
    entries: Vec<SlabEntry>,
}

impl<'a> Sequence<'a> {
    /// Slab duration the file was written with.
    #[must_use]
    pub fn slab_duration(&self) -> f32 {
        self.slab_duration
    }

    /// SH degree of every splat.
    #[must_use]
    pub fn sh_degree(&self) -> u8 {
        self.degree
    }

    /// The slab index.
    #[must_use]
    pub fn entries(&self) -> &[SlabEntry] {
        &self.entries
    }

    /// Indices of the slabs whose time range contains `t`. Usually one, two at
    /// a boundary.
    #[must_use]
    pub fn slabs_at(&self, t: f32) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.t_start <= t && t <= e.t_end)
            .map(|(k, _)| k)
            .collect()
    }

    /// Decodes slab `k` on its own. The result is in the order it was stored.
    ///
    /// # Errors
    ///
    /// [`SplatError::Invalid`] if `k` is out of range. [`SplatError::Parse`] if
    /// the slab's payload is corrupt.
    pub fn decode_slab(&self, k: usize) -> Result<GaussianCloud4D, SplatError> {
        let e = self
            .entries
            .get(k)
            .ok_or(SplatError::Invalid("slab index out of range"))?;
        let payload = &self.data[e.offset as usize..(e.offset + e.length) as usize];
        if payload.len() < 4 {
            return Err(SplatError::Parse("SPLS slab payload truncated"));
        }
        let n = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
        if n != e.members as usize || n > MAX_SLAB_POINTS {
            return Err(SplatError::Parse("SPLS slab member count mismatch"));
        }
        let c = channel_count(self.degree);
        let mut pos = 4;
        let mut ranges = Vec::new();
        if matches!(self.quant, Quantization::Lossy { .. }) {
            let table = payload
                .get(pos..pos + c * 8)
                .ok_or(SplatError::Parse("SPLS range table truncated"))?;
            ranges = read_ranges(table, c)?;
            pos += c * 8;
        }
        let streams = RansStreamSet::unframe(&payload[pos..])
            .map_err(|_| SplatError::Parse("corrupt SPLS stream table"))?;
        if streams.len() != c {
            return Err(SplatError::Parse("SPLS stream count does not match degree"));
        }
        let layout = Layout {
            channels: c,
            position_channels: POSITION_CHANNELS,
        };
        let values = decode_block(&streams, n, layout, self.quant, &ranges)?;

        let k3 = 3 * sh_coeffs_for_degree(self.degree);
        let mut means = Vec::with_capacity(n);
        let mut log_scales = Vec::with_capacity(n);
        let mut rot_left = Vec::with_capacity(n);
        let mut rot_right = Vec::with_capacity(n);
        let mut opacities = Vec::with_capacity(n);
        let mut sh = Vec::with_capacity(n * k3);
        for r in values.chunks_exact(c) {
            means.push([r[0], r[1], r[2], r[3]]);
            log_scales.push([r[4], r[5], r[6], r[7]]);
            rot_left.push([r[8], r[9], r[10], r[11]]);
            rot_right.push([r[12], r[13], r[14], r[15]]);
            opacities.push(r[16]);
            sh.extend_from_slice(&r[17..]);
        }
        GaussianCloud4D::new(
            means,
            log_scales,
            rot_left,
            rot_right,
            opacities,
            self.degree,
            sh,
        )
    }
}

/// Parses an `SPLS` file's header and slab index.
///
/// Only the index is read here. Slabs are decoded with [`Sequence::decode_slab`].
///
/// # Errors
///
/// [`SplatError::Parse`] for a bad magic, reserved bits, out-of-range depths,
/// an index that runs past the end of the file, or inconsistent slab times.
/// [`SplatError::Unsupported`] for other versions or SH degrees above 3.
pub fn read_sequence(data: &[u8]) -> Result<Sequence<'_>, SplatError> {
    if data.len() < HEADER_LEN || data[..4] != MAGIC {
        return Err(SplatError::Parse("not an SPLS file"));
    }
    if data[4] != VERSION {
        return Err(SplatError::Unsupported("SPLS version other than 1"));
    }
    let flags = data[5];
    if flags & !FLAG_LOSSY != 0 || data[7] != 0 || data[14..16] != [0, 0] {
        return Err(SplatError::Parse("reserved SPLS header bits set"));
    }
    let degree = data[6];
    if degree > MAX_SH_DEGREE {
        return Err(SplatError::Unsupported("SPLS SH degree above 3"));
    }
    let quant = Quantization::from_depths(flags & FLAG_LOSSY != 0, data[12], data[13])?;
    let slab_duration = f32::from_le_bytes([data[16], data[17], data[18], data[19]]);
    if !slab_duration.is_finite() || slab_duration <= 0.0 {
        return Err(SplatError::Parse("SPLS slab duration invalid"));
    }
    let count = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
    if count > MAX_SLABS {
        return Err(SplatError::Parse("SPLS slab count out of range"));
    }
    let index_end = HEADER_LEN
        .checked_add(count * INDEX_ENTRY_LEN)
        .filter(|&e| e <= data.len())
        .ok_or(SplatError::Parse("SPLS index runs past end of file"))?;

    let mut entries = Vec::with_capacity(count);
    for k in 0..count {
        let at = HEADER_LEN + k * INDEX_ENTRY_LEN;
        let f =
            |o: usize| f32::from_le_bytes(data[at + o..at + o + 4].try_into().unwrap_or([0; 4]));
        let u =
            |o: usize| u64::from_le_bytes(data[at + o..at + o + 8].try_into().unwrap_or([0; 8]));
        let entry = SlabEntry {
            t_start: f(0),
            t_end: f(4),
            offset: u(8),
            length: u(16),
            members: u32::from_le_bytes(data[at + 24..at + 28].try_into().unwrap_or([0; 4])),
        };
        if !entry.t_start.is_finite() || !entry.t_end.is_finite() || entry.t_end < entry.t_start {
            return Err(SplatError::Parse("SPLS slab time range invalid"));
        }
        let payload_ok = entry.offset >= index_end as u64
            && entry
                .offset
                .checked_add(entry.length)
                .is_some_and(|end| end <= data.len() as u64);
        if !payload_ok {
            return Err(SplatError::Parse("SPLS slab payload out of range"));
        }
        entries.push(entry);
    }
    Ok(Sequence {
        data,
        degree,
        quant,
        slab_duration,
        entries,
    })
}
