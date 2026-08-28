//! Sequence and frame header definitions for the lossless bitstream.
//!
//! Headers are encoded with the same MSB-first bit framing as the residual
//! data, so `tpt-kinetix-bitstream`'s `BitReader` decodes them identically to
//! the encode-side [`BitWriter`].

use tpt_kinetix_bitstream::bitreader::BitReader;

use crate::entropy::{read_bits_u16, read_bits_u8, BitWriter};

/// Per-plane description fixed for the whole stream (bounded-memory contract:
/// `max_width`/`max_height` let a decoder size its arena once).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaneSpec {
    /// Bits per sample: 10, 12, or 16 for v1 (DECISION 1).
    pub bit_depth: u8,
}

/// Stream-level parameters shared by every frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceHeader {
    /// Format version. `1` for the v1 predictive format.
    pub version: u8,
    /// Maximum frame width in pixels (bounded arena sizing).
    pub max_width: u16,
    /// Maximum frame height in pixels (bounded arena sizing).
    pub max_height: u16,
    /// Reserved transform selector. `0` = reversible predictive (v1).
    /// A v1 decoder treats any non-zero value as [`Unsupported`](tpt_kinetix_core::error::KinetixError::Unsupported).
    pub transform_id: u8,
    /// Per-plane bit depths, in plane order.
    pub planes: Vec<PlaneSpec>,
}

/// Per-frame header: dimensions, one checksum per plane, the byte length of
/// each plane's residual payload, and a SHA-256 stream checksum covering the
/// whole frame body (DECISION 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHeader {
    pub width: u16,
    pub height: u16,
    /// One checksum (CRC-32 for <16-bit, CRC-64 for 16-bit) per plane, in
    /// plane order.
    pub plane_checksums: Vec<Vec<u8>>,
    /// Byte length of each plane's residual payload, in plane order. Lets the
    /// decoder slice each plane's bitstream out of the concatenated frame body
    /// (each payload is byte-aligned) and decode it from its own reader.
    pub plane_lengths: Vec<u32>,
    /// SHA-256 digest of the frame body (all plane payloads concatenated),
    /// for end-to-end integrity verification.
    pub stream_sha256: [u8; 32],
}

impl SequenceHeader {
    /// Number of planes declared by the sequence.
    pub fn plane_count(&self) -> usize {
        self.planes.len()
    }

    pub fn encode(&self, w: &mut BitWriter) {
        w.write_bits(u32::from(self.version), 8);
        w.write_bits(u32::from(self.max_width), 16);
        w.write_bits(u32::from(self.max_height), 16);
        w.write_bits(u32::from(self.transform_id), 8);
        w.write_bits(u32::from(self.planes.len() as u8), 8);
        for p in &self.planes {
            w.write_bits(u32::from(p.bit_depth), 8);
        }
    }

    pub fn decode(r: &mut BitReader<'_>) -> Option<SequenceHeader> {
        let version = read_bits_u8(r, 8)?;
        let max_width = read_bits_u16(r, 16)?;
        let max_height = read_bits_u16(r, 16)?;
        let transform_id = read_bits_u8(r, 8)?;
        let count = read_bits_u8(r, 8)?;
        let mut planes = Vec::with_capacity(count as usize);
        for _ in 0..count {
            planes.push(PlaneSpec {
                bit_depth: read_bits_u8(r, 8)?,
            });
        }
        Some(SequenceHeader {
            version,
            max_width,
            max_height,
            transform_id,
            planes,
        })
    }
}

impl FrameHeader {
    pub fn encode(&self, w: &mut BitWriter) {
        w.write_bits(u32::from(self.width), 16);
        w.write_bits(u32::from(self.height), 16);
        w.write_bits(u32::from(self.plane_checksums.len() as u8), 8);
        for (crc, len) in self.plane_checksums.iter().zip(&self.plane_lengths) {
            w.write_bits(u32::from(crc.len() as u8), 8);
            for &b in crc {
                w.write_bits(u32::from(b), 8);
            }
            w.write_bits(*len, 32);
        }
        for &b in &self.stream_sha256 {
            w.write_bits(u32::from(b), 8);
        }
    }

    pub fn decode(r: &mut BitReader<'_>) -> Option<FrameHeader> {
        let width = read_bits_u16(r, 16)?;
        let height = read_bits_u16(r, 16)?;
        let count = read_bits_u8(r, 8)?;
        let mut plane_checksums = Vec::with_capacity(count as usize);
        let mut plane_lengths = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let len = read_bits_u8(r, 8)?;
            let mut crc = Vec::with_capacity(len as usize);
            for _ in 0..len {
                crc.push(read_bits_u8(r, 8)?);
            }
            let plen = r.read_bits(32)?;
            plane_checksums.push(crc);
            plane_lengths.push(plen);
        }
        let mut stream_sha256 = [0u8; 32];
        for b in &mut stream_sha256 {
            *b = read_bits_u8(r, 8)?;
        }
        Some(FrameHeader {
            width,
            height,
            plane_checksums,
            plane_lengths,
            stream_sha256,
        })
    }
}
