//! Sequence and frame header layout.
//!
//! # Format design (v1)
//!
//! Both headers are **byte-aligned** — Lean spends its bit-packing budget on
//! the rANS-coded payload ([`crate::rans`]), not the headers, so a plain
//! byte reader is enough here and no bit-writer is needed for this scaffold.
//!
//! ## Sequence header (once per stream, 14 bytes)
//!
//! | Field | Type | Notes |
//! |---|---|---|
//! | `magic` | `[u8; 4]` | `b"LEAN"` |
//! | `version` | `u8` | format version, `1` for this scaffold |
//! | `max_width` | `u16` BE | frame-size ceiling; decoder sizes arenas from this, once |
//! | `max_height` | `u16` BE | |
//! | `max_ref_frames` | `u8` | reference-frame ceiling (v1 default: 4) |
//! | `block_size_log2` | `u8` | packed `min_block_size_log2 << 4 \| max_block_size_log2` — fixed shallow partition range, e.g. 8x8..64x64 is `3 << 4 \| 6` |
//! | `bit_depth` | `u8` | 8 or 10 |
//! | `chroma_format` | `u8` | [`ChromaFormat`] discriminant |
//! | `num_rans_streams` | `u8` | independently-decodable entropy sub-streams per frame (see [`crate::rans`]) |
//!
//! Declaring `max_width`/`max_height`/`max_ref_frames`/`num_rans_streams` up
//! front is what lets a decoder allocate its working arena exactly once at
//! stream start and never grow it mid-stream — the core bounded-memory
//! property this format is designed around.
//!
//! ## Frame header (once per frame, 11 bytes)
//!
//! | Field | Type | Notes |
//! |---|---|---|
//! | `frame_type` | `u8` | [`FrameType`] discriminant |
//! | `width` | `u16` BE | must be `<= sequence.max_width` |
//! | `height` | `u16` BE | must be `<= sequence.max_height` |
//! | `base_qp` | `u8` | frame-level base quant step |
//! | `ref_frame_count` | `u8` | `0` for [`FrameType::Key`] |
//! | `payload_len` | `u32` BE | length in bytes of the rANS-coded payload that follows |

use tpt_kinetix_core::error::KinetixError;

use crate::bitreader::BitReader;

const MAGIC: [u8; 4] = *b"LEAN";
const SEQUENCE_HEADER_LEN: usize = 14;
const FRAME_HEADER_LEN: usize = 11;

/// Chroma subsampling format (mirrors the common 4:2:0 / 4:2:2 / 4:4:4 set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromaFormat {
    Yuv420 = 0,
    Yuv422 = 1,
    Yuv444 = 2,
}

impl ChromaFormat {
    fn from_u8(v: u8) -> Result<Self, KinetixError> {
        match v {
            0 => Ok(Self::Yuv420),
            1 => Ok(Self::Yuv422),
            2 => Ok(Self::Yuv444),
            other => Err(KinetixError::Parse(format!(
                "invalid chroma_format {other}"
            ))),
        }
    }
}

/// Whether a frame is an independently-decodable key frame or predicted from
/// references.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Key = 0,
    Inter = 1,
}

impl FrameType {
    fn from_u8(v: u8) -> Result<Self, KinetixError> {
        match v {
            0 => Ok(Self::Key),
            1 => Ok(Self::Inter),
            other => Err(KinetixError::Parse(format!("invalid frame_type {other}"))),
        }
    }
}

/// Stream-level parameters, parsed once at the start of decode.
///
/// Everything a decoder needs to size its arenas exactly once — see the
/// module docs for the byte layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceHeader {
    pub version: u8,
    pub max_width: u16,
    pub max_height: u16,
    pub max_ref_frames: u8,
    pub min_block_size_log2: u8,
    pub max_block_size_log2: u8,
    pub bit_depth: u8,
    pub chroma_format: ChromaFormat,
    pub num_rans_streams: u8,
}

impl SequenceHeader {
    /// Parse a sequence header from the start of `reader`. Leaves the reader
    /// positioned immediately after the header on success.
    pub fn parse(reader: &mut BitReader<'_>) -> Result<Self, KinetixError> {
        let mut magic = [0u8; 4];
        for byte in &mut magic {
            *byte = reader
                .read_u8()
                .ok_or_else(|| KinetixError::Parse("sequence header: truncated magic".into()))?;
        }
        if magic != MAGIC {
            return Err(KinetixError::Parse(format!(
                "sequence header: bad magic {magic:?}, expected {MAGIC:?}"
            )));
        }

        let version = read_u8(reader, "version")?;
        let max_width = read_u16(reader, "max_width")?;
        let max_height = read_u16(reader, "max_height")?;
        let max_ref_frames = read_u8(reader, "max_ref_frames")?;
        let block_size_log2 = read_u8(reader, "block_size_log2")?;
        let bit_depth = read_u8(reader, "bit_depth")?;
        let chroma_format = ChromaFormat::from_u8(read_u8(reader, "chroma_format")?)?;
        let num_rans_streams = read_u8(reader, "num_rans_streams")?;

        let min_block_size_log2 = block_size_log2 >> 4;
        let max_block_size_log2 = block_size_log2 & 0x0F;
        if min_block_size_log2 > max_block_size_log2 {
            return Err(KinetixError::Parse(format!(
                "sequence header: min_block_size_log2 ({min_block_size_log2}) > max_block_size_log2 ({max_block_size_log2})"
            )));
        }

        Ok(Self {
            version,
            max_width,
            max_height,
            max_ref_frames,
            min_block_size_log2,
            max_block_size_log2,
            bit_depth,
            chroma_format,
            num_rans_streams,
        })
    }

    /// Serialize back to the wire format (used by tests and by an eventual
    /// encoder).
    pub fn to_bytes(self) -> [u8; SEQUENCE_HEADER_LEN] {
        let mut out = [0u8; SEQUENCE_HEADER_LEN];
        out[0..4].copy_from_slice(&MAGIC);
        out[4] = self.version;
        out[5..7].copy_from_slice(&self.max_width.to_be_bytes());
        out[7..9].copy_from_slice(&self.max_height.to_be_bytes());
        out[9] = self.max_ref_frames;
        out[10] = (self.min_block_size_log2 << 4) | (self.max_block_size_log2 & 0x0F);
        out[11] = self.bit_depth;
        out[12] = self.chroma_format as u8;
        out[13] = self.num_rans_streams;
        out
    }
}

/// Per-frame parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub frame_type: FrameType,
    pub width: u16,
    pub height: u16,
    pub base_qp: u8,
    pub ref_frame_count: u8,
    pub payload_len: u32,
}

impl FrameHeader {
    /// Parse a frame header, validating it against the stream's
    /// [`SequenceHeader`] ceilings.
    pub fn parse(
        reader: &mut BitReader<'_>,
        sequence: &SequenceHeader,
    ) -> Result<Self, KinetixError> {
        let frame_type = FrameType::from_u8(read_u8(reader, "frame_type")?)?;
        let width = read_u16(reader, "width")?;
        let height = read_u16(reader, "height")?;
        let base_qp = read_u8(reader, "base_qp")?;
        let ref_frame_count = read_u8(reader, "ref_frame_count")?;
        let payload_len = reader
            .read_u32_be()
            .ok_or_else(|| KinetixError::Parse("frame header: truncated payload_len".into()))?;

        if width > sequence.max_width || height > sequence.max_height {
            return Err(KinetixError::Parse(format!(
                "frame header: {width}x{height} exceeds sequence ceiling {}x{}",
                sequence.max_width, sequence.max_height
            )));
        }
        if ref_frame_count > sequence.max_ref_frames {
            return Err(KinetixError::Parse(format!(
                "frame header: ref_frame_count {ref_frame_count} exceeds sequence ceiling {}",
                sequence.max_ref_frames
            )));
        }
        if frame_type == FrameType::Key && ref_frame_count != 0 {
            return Err(KinetixError::Parse(
                "frame header: key frame must have ref_frame_count == 0".into(),
            ));
        }

        Ok(Self {
            frame_type,
            width,
            height,
            base_qp,
            ref_frame_count,
            payload_len,
        })
    }

    /// Serialize back to the wire format.
    pub fn to_bytes(self) -> [u8; FRAME_HEADER_LEN] {
        let mut out = [0u8; FRAME_HEADER_LEN];
        out[0] = self.frame_type as u8;
        out[1..3].copy_from_slice(&self.width.to_be_bytes());
        out[3..5].copy_from_slice(&self.height.to_be_bytes());
        out[5] = self.base_qp;
        out[6] = self.ref_frame_count;
        out[7..11].copy_from_slice(&self.payload_len.to_be_bytes());
        out
    }
}

fn read_u8(reader: &mut BitReader<'_>, field: &str) -> Result<u8, KinetixError> {
    reader
        .read_u8()
        .ok_or_else(|| KinetixError::Parse(format!("truncated field: {field}")))
}

fn read_u16(reader: &mut BitReader<'_>, field: &str) -> Result<u16, KinetixError> {
    reader
        .read_u16_be()
        .ok_or_else(|| KinetixError::Parse(format!("truncated field: {field}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sequence() -> SequenceHeader {
        SequenceHeader {
            version: 1,
            max_width: 1920,
            max_height: 1080,
            max_ref_frames: 4,
            min_block_size_log2: 3, // 8
            max_block_size_log2: 6, // 64
            bit_depth: 8,
            chroma_format: ChromaFormat::Yuv420,
            num_rans_streams: 4,
        }
    }

    #[test]
    fn sequence_header_round_trips() {
        let seq = sample_sequence();
        let bytes = seq.to_bytes();
        let mut reader = BitReader::new(&bytes);
        let parsed = SequenceHeader::parse(&mut reader).expect("parse");
        assert_eq!(parsed, seq);
    }

    #[test]
    fn sequence_header_rejects_bad_magic() {
        let mut bytes = sample_sequence().to_bytes();
        bytes[0] = b'X';
        let mut reader = BitReader::new(&bytes);
        assert!(SequenceHeader::parse(&mut reader).is_err());
    }

    #[test]
    fn sequence_header_rejects_inverted_block_range() {
        let mut seq = sample_sequence();
        seq.min_block_size_log2 = 6;
        seq.max_block_size_log2 = 3;
        let bytes = seq.to_bytes();
        let mut reader = BitReader::new(&bytes);
        assert!(SequenceHeader::parse(&mut reader).is_err());
    }

    #[test]
    fn frame_header_rejects_oversized_dimensions() {
        let seq = sample_sequence();
        let frame = FrameHeader {
            frame_type: FrameType::Key,
            width: seq.max_width + 1,
            height: 720,
            base_qp: 20,
            ref_frame_count: 0,
            payload_len: 0,
        };
        let bytes = frame.to_bytes();
        let mut reader = BitReader::new(&bytes);
        assert!(FrameHeader::parse(&mut reader, &seq).is_err());
    }

    #[test]
    fn frame_header_rejects_key_frame_with_refs() {
        let seq = sample_sequence();
        let frame = FrameHeader {
            frame_type: FrameType::Key,
            width: 640,
            height: 480,
            base_qp: 20,
            ref_frame_count: 1,
            payload_len: 0,
        };
        let bytes = frame.to_bytes();
        let mut reader = BitReader::new(&bytes);
        assert!(FrameHeader::parse(&mut reader, &seq).is_err());
    }

    #[test]
    fn frame_header_round_trips() {
        let seq = sample_sequence();
        let frame = FrameHeader {
            frame_type: FrameType::Inter,
            width: 1280,
            height: 720,
            base_qp: 24,
            ref_frame_count: 2,
            payload_len: 4096,
        };
        let bytes = frame.to_bytes();
        let mut reader = BitReader::new(&bytes);
        let parsed = FrameHeader::parse(&mut reader, &seq).expect("parse");
        assert_eq!(parsed, frame);
    }
}
