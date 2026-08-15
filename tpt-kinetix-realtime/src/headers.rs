//! Profile-agnostic sequence and frame header layout.
//!
//! # Format design (v1)
//!
//! Both headers are **byte-aligned** — Realtime spends its bit-packing budget
//! on the rANS-coded slice payloads (via [`tpt_kinetix_bitstream`]), not the
//! headers, so a plain byte reader is enough here.
//!
//! The header carries the three things that make the codec profile-agnostic
//! (see `docs/realtime-codec-design.md` DECISION 6): a [`ProfilePreset`]
//! discriminant, the loss-resilience parameters (`slice_grid_*` for the
//! sub-frame partition, `fec_overhead_pct` for the FEC layer), and the
//! foveation flag for the AR preset. Cloud gaming / conferencing / AR are
//! three preset parameter sets over this one header shape — not separate
//! codecs.
//!
//! ## Sequence header (once per stream, 20 bytes)
//!
//! | Field | Type | Notes |
//! |---|---|---|
//! | `magic` | `[u8; 4]` | `b"RTIM"` |
//! | `version` | `u8` | format version, `1` for this scaffold |
//! | `max_width` | `u16` BE | decoder sizes arenas from this, once |
//! | `max_height` | `u16` BE | |
//! | `profile` | `u8` | [`ProfilePreset`] discriminant (0 cloud gaming / 1 conferencing / 2 AR) |
//! | `slice_grid_cols` | `u8` | columns of the independent slice grid (DECISION 3) |
//! | `slice_grid_rows` | `u8` | rows of the slice grid; also sizes the intra-refresh mask |
//! | `fec_overhead_pct` | `u8` | FEC repair fraction 0..100 (DECISION 1) |
//! | `foveation_enabled` | `u8` | 0/1, AR preset only (DECISION 6) |
//! | `block_size_log2` | `u8` | packed `min<<4 | max` fixed shallow partition |
//! | `bit_depth` | `u8` | 8 or 10 |
//! | `chroma_format` | `u8` | [`ChromaFormat`] discriminant |
//! | `num_rans_streams` | `u8` | independent entropy sub-streams per frame (== slice count) |
//! | `max_ref_frames` | `u8` | reference-frame ceiling (v1: 1) |
//! | `max_deadline_ms` | `u8` | encode-deadline ceiling the profile targets (DECISION 4) |
//!
//! ## Frame header (once per frame, variable length)
//!
//! | Field | Type | Notes |
//! |---|---|---|
//! | `frame_type` | `u8` | [`FrameType`] discriminant |
//! | `width` | `u16` BE | `<= sequence.max_width` |
//! | `height` | `u16` BE | `<= sequence.max_height` |
//! | `base_qp` | `u8` | frame-level base quant step |
//! | `ref_frame_count` | `u8` | `0` for [`FrameType::Key`], `1` for Inter (single backward ref) |
//! | `deadline_ms` | `u8` | encode deadline actually applied this frame (DECISION 4) |
//! | `force_idr` | `u8` | 0/1, decoder-requested resync (DECISION 2) |
//! | `foveation_center_x` | `u16` BE | gaze center X (AR preset) |
//! | `foveation_center_y` | `u16` BE | gaze center Y (AR preset) |
//! | `intra_refresh_mask` | `[u8; ceil(rows/8)]` | which slice rows are intra-coded this frame (DECISION 2) |
//! | `payload_len` | `u32` BE | length of the rANS-coded slice payload that follows |

use tpt_kinetix_core::error::KinetixError;

use tpt_kinetix_bitstream::BitReader;

const MAGIC: [u8; 4] = *b"RTIM";
const SEQUENCE_HEADER_LEN: usize = 20;

/// Target decode profile. All three share the same bitstream; the preset only
/// selects the default parameter set (slice grid density, FEC overhead,
/// foveation, latency envelope). AR is the hardest preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfilePreset {
    CloudGaming = 0,
    Conferencing = 1,
    AR = 2,
}

impl ProfilePreset {
    fn from_u8(v: u8) -> Result<Self, KinetixError> {
        match v {
            0 => Ok(Self::CloudGaming),
            1 => Ok(Self::Conferencing),
            2 => Ok(Self::AR),
            other => Err(KinetixError::Parse(format!(
                "invalid profile_preset {other}"
            ))),
        }
    }
}

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
/// Everything a decoder needs to size its arenas exactly once — see the
/// module docs for the byte layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceHeader {
    pub version: u8,
    pub max_width: u16,
    pub max_height: u16,
    pub profile: ProfilePreset,
    pub slice_grid_cols: u8,
    pub slice_grid_rows: u8,
    pub fec_overhead_pct: u8,
    pub foveation_enabled: bool,
    pub min_block_size_log2: u8,
    pub max_block_size_log2: u8,
    pub bit_depth: u8,
    pub chroma_format: ChromaFormat,
    pub num_rans_streams: u8,
    pub max_ref_frames: u8,
    pub max_deadline_ms: u8,
}

impl SequenceHeader {
    /// Number of bytes in the per-frame intra-refresh bitmask, derived from
    /// the slice grid row count.
    pub fn refresh_mask_len(&self) -> usize {
        self.slice_grid_rows.div_ceil(8) as usize
    }

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
        let profile = ProfilePreset::from_u8(read_u8(reader, "profile")?)?;
        let slice_grid_cols = read_u8(reader, "slice_grid_cols")?;
        let slice_grid_rows = read_u8(reader, "slice_grid_rows")?;
        let fec_overhead_pct = read_u8(reader, "fec_overhead_pct")?;
        let foveation_enabled = read_u8(reader, "foveation_enabled")? != 0;
        let block_size_log2 = read_u8(reader, "block_size_log2")?;
        let bit_depth = read_u8(reader, "bit_depth")?;
        let chroma_format = ChromaFormat::from_u8(read_u8(reader, "chroma_format")?)?;
        let num_rans_streams = read_u8(reader, "num_rans_streams")?;
        let max_ref_frames = read_u8(reader, "max_ref_frames")?;
        let max_deadline_ms = read_u8(reader, "max_deadline_ms")?;

        let min_block_size_log2 = block_size_log2 >> 4;
        let max_block_size_log2 = block_size_log2 & 0x0F;
        if min_block_size_log2 > max_block_size_log2 {
            return Err(KinetixError::Parse(format!(
                "sequence header: min_block_size_log2 ({min_block_size_log2}) > max_block_size_log2 ({max_block_size_log2})"
            )));
        }
        if slice_grid_cols == 0 || slice_grid_rows == 0 {
            return Err(KinetixError::Parse(
                "sequence header: slice grid must be at least 1x1".into(),
            ));
        }
        if fec_overhead_pct > 100 {
            return Err(KinetixError::Parse(format!(
                "sequence header: fec_overhead_pct {fec_overhead_pct} > 100"
            )));
        }
        if bit_depth != 8 && bit_depth != 10 {
            return Err(KinetixError::Parse(format!(
                "sequence header: unsupported bit_depth {bit_depth} (only 8/10)"
            )));
        }

        Ok(Self {
            version,
            max_width,
            max_height,
            profile,
            slice_grid_cols,
            slice_grid_rows,
            fec_overhead_pct,
            foveation_enabled,
            min_block_size_log2,
            max_block_size_log2,
            bit_depth,
            chroma_format,
            num_rans_streams,
            max_ref_frames,
            max_deadline_ms,
        })
    }

    /// Serialize back to the wire format.
    pub fn to_bytes(&self) -> [u8; SEQUENCE_HEADER_LEN] {
        let mut out = [0u8; SEQUENCE_HEADER_LEN];
        out[0..4].copy_from_slice(&MAGIC);
        out[4] = self.version;
        out[5..7].copy_from_slice(&self.max_width.to_be_bytes());
        out[7..9].copy_from_slice(&self.max_height.to_be_bytes());
        out[9] = self.profile as u8;
        out[10] = self.slice_grid_cols;
        out[11] = self.slice_grid_rows;
        out[12] = self.fec_overhead_pct;
        out[13] = u8::from(self.foveation_enabled);
        out[14] = (self.min_block_size_log2 << 4) | (self.max_block_size_log2 & 0x0F);
        out[15] = self.bit_depth;
        out[16] = self.chroma_format as u8;
        out[17] = self.num_rans_streams;
        out[18] = self.max_ref_frames;
        out[19] = self.max_deadline_ms;
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
    pub deadline_ms: u8,
    pub force_idr: bool,
    pub foveation_center_x: u16,
    pub foveation_center_y: u16,
    /// Which slice rows are intra-coded this frame (rolling intra-refresh).
    pub intra_refresh_mask: Vec<u8>,
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
        let deadline_ms = read_u8(reader, "deadline_ms")?;
        let force_idr = read_u8(reader, "force_idr")? != 0;
        let foveation_center_x = read_u16(reader, "foveation_center_x")?;
        let foveation_center_y = read_u16(reader, "foveation_center_y")?;

        let mask_len = sequence.refresh_mask_len();
        let mut intra_refresh_mask = vec![0u8; mask_len];
        for byte in intra_refresh_mask.iter_mut() {
            *byte = read_u8(reader, "intra_refresh_mask")?;
        }

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
        if foveation_center_x > width || foveation_center_y > height {
            return Err(KinetixError::Parse(
                "frame header: foveation center outside frame bounds".into(),
            ));
        }

        Ok(Self {
            frame_type,
            width,
            height,
            base_qp,
            ref_frame_count,
            deadline_ms,
            force_idr,
            foveation_center_x,
            foveation_center_y,
            intra_refresh_mask,
            payload_len,
        })
    }

    /// Serialize back to the wire format (variable length: header + mask + payload_len).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(13 + self.intra_refresh_mask.len() + 4);
        out.push(self.frame_type as u8);
        out.extend_from_slice(&self.width.to_be_bytes());
        out.extend_from_slice(&self.height.to_be_bytes());
        out.push(self.base_qp);
        out.push(self.ref_frame_count);
        out.push(self.deadline_ms);
        out.push(u8::from(self.force_idr));
        out.extend_from_slice(&self.foveation_center_x.to_be_bytes());
        out.extend_from_slice(&self.foveation_center_y.to_be_bytes());
        out.extend_from_slice(&self.intra_refresh_mask);
        out.extend_from_slice(&self.payload_len.to_be_bytes());
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
            profile: ProfilePreset::AR,
            slice_grid_cols: 8,
            slice_grid_rows: 8,
            fec_overhead_pct: 25,
            foveation_enabled: true,
            min_block_size_log2: 3,
            max_block_size_log2: 6,
            bit_depth: 8,
            chroma_format: ChromaFormat::Yuv420,
            num_rans_streams: 64,
            max_ref_frames: 1,
            max_deadline_ms: 16,
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
    fn sequence_header_rejects_bad_fec_and_bit_depth() {
        let mut seq = sample_sequence();
        seq.fec_overhead_pct = 150;
        let bytes = seq.to_bytes();
        let mut reader = BitReader::new(&bytes);
        assert!(SequenceHeader::parse(&mut reader).is_err());

        let mut seq = sample_sequence();
        seq.bit_depth = 12;
        let bytes = seq.to_bytes();
        let mut reader = BitReader::new(&bytes);
        assert!(SequenceHeader::parse(&mut reader).is_err());
    }

    #[test]
    fn frame_header_refresh_mask_len_matches_grid() {
        let seq = sample_sequence();
        assert_eq!(seq.refresh_mask_len(), 1); // 8 rows -> 1 byte

        let mut seq_fine = sample_sequence();
        seq_fine.slice_grid_rows = 17;
        assert_eq!(seq_fine.refresh_mask_len(), 3);
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
            deadline_ms: 16,
            force_idr: false,
            foveation_center_x: 640,
            foveation_center_y: 360,
            intra_refresh_mask: vec![0b0000_0001],
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
            deadline_ms: 16,
            force_idr: false,
            foveation_center_x: 0,
            foveation_center_y: 0,
            intra_refresh_mask: vec![0],
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
            deadline_ms: 16,
            force_idr: false,
            foveation_center_x: 0,
            foveation_center_y: 0,
            intra_refresh_mask: vec![0],
            payload_len: 0,
        };
        let bytes = frame.to_bytes();
        let mut reader = BitReader::new(&bytes);
        assert!(FrameHeader::parse(&mut reader, &seq).is_err());
    }

    #[test]
    fn frame_header_rejects_foveation_outside_bounds() {
        let seq = sample_sequence();
        let frame = FrameHeader {
            frame_type: FrameType::Inter,
            width: 640,
            height: 480,
            base_qp: 20,
            ref_frame_count: 1,
            deadline_ms: 16,
            force_idr: false,
            foveation_center_x: 700,
            foveation_center_y: 480,
            intra_refresh_mask: vec![0],
            payload_len: 0,
        };
        let bytes = frame.to_bytes();
        let mut reader = BitReader::new(&bytes);
        assert!(FrameHeader::parse(&mut reader, &seq).is_err());
    }
}
