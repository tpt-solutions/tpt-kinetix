//! Byte-aligned sequence and frame header layout.
//!
//! # Format design (v1)
//!
//! Both headers are **byte-aligned** — Screen spends its bit-packing budget on
//! the rANS-coded payloads (via [`tpt_kinetix_bitstream`]), not the headers, so
//! a plain byte reader is enough here.
//!
//! The header carries the three things the codec's mode classifier needs (see
//! `docs/screen-codec-design.md`):
//!
//! - the frame grid + arena ceilings (`max_width`/`max_height`/`base_block_size_log2`,
//!   `dict_cap`/`palette_cap`/`glyph_max_dim`) that bound the specialist state,
//! - the four rANS sub-streams (`num_rans_streams` — mode map, flat, glyph,
//!   natural) that the classifier fans output into,
//! - the cross-frame dictionary bookkeeping (`dict_version`/`dict_reset` in the
//!   frame header) used for glyph/palette reuse and resync (DECISION 3).
//!
//! ## Sequence header (once per stream, 18 bytes)
//!
//! | Field | Type | Notes |
//! |---|---|---|
//! | `magic` | `[u8; 4]` | `b"SCRN"` |
//! | `version` | `u8` | format version, `1` for this scaffold |
//! | `max_width` | `u16` BE | decoder sizes arenas from this, once |
//! | `max_height` | `u16` BE | |
//! | `base_block_size_log2` | `u8` | coding-block grid log2 (4 = 16x16, DECISION 2) |
//! | `num_rans_streams` | `u8` | independent entropy sub-streams (4: mode/flat/glyph/natural) |
//! | `dict_cap` | `u16` BE | glyph dictionary slot ceiling (DECISION 3) |
//! | `palette_cap` | `u8` | color palette entry ceiling (DECISION 3) |
//! | `glyph_max_dim` | `u8` | max glyph bitmap side in px (DECISION 3) |
//! | `bit_depth` | `u8` | 8 or 10 |
//! | `chroma_format` | `u8` | [`ChromaFormat`] discriminant |
//! | `max_ref_frames` | `u8` | reference-frame ceiling (v1: 1) |
//!
//! ## Frame header (once per frame, 13 bytes)
//!
//! | Field | Type | Notes |
//! |---|---|---|
//! | `frame_type` | `u8` | [`FrameType`] discriminant |
//! | `width` | `u16` BE | `<= sequence.max_width` |
//! | `height` | `u16` BE | `<= sequence.max_height` |
//! | `base_qp` | `u8` | frame-level base quant step (NATURAL mode) |
//! | `ref_frame_count` | `u8` | `0` for [`FrameType::Key`], `1` for Inter |
//! | `dict_version` | `u8` | monotonic cross-frame dictionary version (DECISION 3) |
//! | `dict_reset` | `u8` | 0/1, force clean dict rebuild (DECISION 3) |
//! | `payload_len` | `u32` BE | length of the rANS-coded payload that follows |

use tpt_kinetix_core::error::KinetixError;

use tpt_kinetix_bitstream::BitReader;

const MAGIC: [u8; 4] = *b"SCRN";
const SEQUENCE_HEADER_LEN: usize = 18;

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
/// its (single) reference.
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
/// Everything a decoder needs to size its arenas exactly once — see the module
/// docs for the byte layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceHeader {
    pub version: u8,
    pub max_width: u16,
    pub max_height: u16,
    pub base_block_size_log2: u8,
    pub num_rans_streams: u8,
    pub dict_cap: u16,
    pub palette_cap: u8,
    pub glyph_max_dim: u8,
    pub bit_depth: u8,
    pub chroma_format: ChromaFormat,
    pub max_ref_frames: u8,
}

impl SequenceHeader {
    /// Parse a sequence header from the start of `reader`.
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
        let base_block_size_log2 = read_u8(reader, "base_block_size_log2")?;
        let num_rans_streams = read_u8(reader, "num_rans_streams")?;
        let dict_cap = read_u16(reader, "dict_cap")?;
        let palette_cap = read_u8(reader, "palette_cap")?;
        let glyph_max_dim = read_u8(reader, "glyph_max_dim")?;
        let bit_depth = read_u8(reader, "bit_depth")?;
        let chroma_format = ChromaFormat::from_u8(read_u8(reader, "chroma_format")?)?;
        let max_ref_frames = read_u8(reader, "max_ref_frames")?;

        if !(3..=6).contains(&base_block_size_log2) {
            return Err(KinetixError::Parse(format!(
                "sequence header: base_block_size_log2 {base_block_size_log2} outside 3..=6"
            )));
        }
        if num_rans_streams < 4 {
            return Err(KinetixError::Parse(format!(
                "sequence header: num_rans_streams {num_rans_streams} < 4 (mode/flat/glyph/natural)"
            )));
        }
        if dict_cap == 0 {
            return Err(KinetixError::Parse(
                "sequence header: dict_cap must be >= 1".into(),
            ));
        }
        if palette_cap == 0 {
            return Err(KinetixError::Parse(
                "sequence header: palette_cap must be >= 1".into(),
            ));
        }
        if glyph_max_dim == 0 || glyph_max_dim > 64 {
            return Err(KinetixError::Parse(format!(
                "sequence header: glyph_max_dim {glyph_max_dim} outside 1..=64"
            )));
        }
        if bit_depth != 8 && bit_depth != 10 {
            return Err(KinetixError::Parse(format!(
                "sequence header: unsupported bit_depth {bit_depth} (only 8/10)"
            )));
        }
        if max_ref_frames == 0 {
            return Err(KinetixError::Parse(
                "sequence header: max_ref_frames must be >= 1".into(),
            ));
        }

        Ok(Self {
            version,
            max_width,
            max_height,
            base_block_size_log2,
            num_rans_streams,
            dict_cap,
            palette_cap,
            glyph_max_dim,
            bit_depth,
            chroma_format,
            max_ref_frames,
        })
    }

    /// Serialize back to the wire format.
    pub fn to_bytes(&self) -> [u8; SEQUENCE_HEADER_LEN] {
        let mut out = [0u8; SEQUENCE_HEADER_LEN];
        out[0..4].copy_from_slice(&MAGIC);
        out[4] = self.version;
        out[5..7].copy_from_slice(&self.max_width.to_be_bytes());
        out[7..9].copy_from_slice(&self.max_height.to_be_bytes());
        out[9] = self.base_block_size_log2;
        out[10] = self.num_rans_streams;
        out[11..13].copy_from_slice(&self.dict_cap.to_be_bytes());
        out[13] = self.palette_cap;
        out[14] = self.glyph_max_dim;
        out[15] = self.bit_depth;
        out[16] = self.chroma_format as u8;
        out[17] = self.max_ref_frames;
        out
    }
}

/// Per-frame parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHeader {
    pub frame_type: FrameType,
    pub width: u16,
    pub height: u16,
    pub base_qp: u8,
    pub ref_frame_count: u8,
    /// Monotonic cross-frame dictionary version (DECISION 3).
    pub dict_version: u8,
    /// Force a clean dictionary rebuild on this frame (DECISION 3).
    pub dict_reset: bool,
    /// Length of the rANS-coded payload that follows the frame header.
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
        let dict_version = read_u8(reader, "dict_version")?;
        let dict_reset = read_u8(reader, "dict_reset")? != 0;
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
        if frame_type == FrameType::Inter && ref_frame_count == 0 {
            return Err(KinetixError::Parse(
                "frame header: inter frame must have a reference".into(),
            ));
        }

        Ok(Self {
            frame_type,
            width,
            height,
            base_qp,
            ref_frame_count,
            dict_version,
            dict_reset,
            payload_len,
        })
    }

    /// Serialize back to the wire format (13 bytes).
    pub fn to_bytes(&self) -> [u8; 13] {
        let mut out = [0u8; 13];
        out[0] = self.frame_type as u8;
        out[1..3].copy_from_slice(&self.width.to_be_bytes());
        out[3..5].copy_from_slice(&self.height.to_be_bytes());
        out[5] = self.base_qp;
        out[6] = self.ref_frame_count;
        out[7] = self.dict_version;
        out[8] = u8::from(self.dict_reset);
        out[9..13].copy_from_slice(&self.payload_len.to_be_bytes());
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
            base_block_size_log2: 4,
            num_rans_streams: 4,
            dict_cap: 256,
            palette_cap: 64,
            glyph_max_dim: 32,
            bit_depth: 8,
            chroma_format: ChromaFormat::Yuv420,
            max_ref_frames: 1,
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
    fn sequence_header_rejects_bad_block_size() {
        let mut seq = sample_sequence();
        seq.base_block_size_log2 = 2;
        let bytes = seq.to_bytes();
        let mut reader = BitReader::new(&bytes);
        assert!(SequenceHeader::parse(&mut reader).is_err());
    }

    #[test]
    fn sequence_header_rejects_too_few_streams() {
        let mut seq = sample_sequence();
        seq.num_rans_streams = 3;
        let bytes = seq.to_bytes();
        let mut reader = BitReader::new(&bytes);
        assert!(SequenceHeader::parse(&mut reader).is_err());
    }

    #[test]
    fn sequence_header_rejects_bad_bit_depth() {
        let mut seq = sample_sequence();
        seq.bit_depth = 12;
        let bytes = seq.to_bytes();
        let mut reader = BitReader::new(&bytes);
        assert!(SequenceHeader::parse(&mut reader).is_err());
    }

    #[test]
    fn frame_header_round_trips() {
        let seq = sample_sequence();
        let frame = FrameHeader {
            frame_type: FrameType::Inter,
            width: 1280,
            height: 720,
            base_qp: 24,
            ref_frame_count: 1,
            dict_version: 7,
            dict_reset: false,
            payload_len: 4096,
        };
        let bytes = frame.to_bytes();
        let mut reader = BitReader::new(&bytes);
        let parsed = FrameHeader::parse(&mut reader, &seq).expect("parse");
        assert_eq!(parsed, frame);
    }

    #[test]
    fn frame_header_rejects_key_with_refs() {
        let seq = sample_sequence();
        let frame = FrameHeader {
            frame_type: FrameType::Key,
            width: 640,
            height: 480,
            base_qp: 20,
            ref_frame_count: 1,
            dict_version: 0,
            dict_reset: true,
            payload_len: 0,
        };
        let bytes = frame.to_bytes();
        let mut reader = BitReader::new(&bytes);
        assert!(FrameHeader::parse(&mut reader, &seq).is_err());
    }

    #[test]
    fn frame_header_rejects_inter_without_ref() {
        let seq = sample_sequence();
        let frame = FrameHeader {
            frame_type: FrameType::Inter,
            width: 640,
            height: 480,
            base_qp: 20,
            ref_frame_count: 0,
            dict_version: 1,
            dict_reset: false,
            payload_len: 0,
        };
        let bytes = frame.to_bytes();
        let mut reader = BitReader::new(&bytes);
        assert!(FrameHeader::parse(&mut reader, &seq).is_err());
    }

    #[test]
    fn frame_header_rejects_oversize() {
        let seq = sample_sequence();
        let frame = FrameHeader {
            frame_type: FrameType::Key,
            width: 1920,
            height: 1081,
            base_qp: 20,
            ref_frame_count: 0,
            dict_version: 0,
            dict_reset: true,
            payload_len: 0,
        };
        let bytes = frame.to_bytes();
        let mut reader = BitReader::new(&bytes);
        assert!(FrameHeader::parse(&mut reader, &seq).is_err());
    }
}
