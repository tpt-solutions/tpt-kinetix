//! Byte-aligned sequence and frame header layout.

use tpt_kinetix_bitstream::BitReader;
use tpt_kinetix_core::error::KinetixError;

const MAGIC: [u8; 4] = *b"VISN";
const SEQUENCE_HEADER_LEN: usize = 16;

/// Chroma subsampling format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromaFormat {
    Yuv420 = 0,
    Yuv422 = 1,
    Yuv444 = 2,
}

impl ChromaFormat {
    pub fn from_u8(v: u8) -> Result<Self, KinetixError> {
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

/// Whether a frame is an independently-decodable key frame or predicted from its reference.
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

/// Stream-level parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceHeader {
    pub version: u8,
    pub max_width: u16,
    pub max_height: u16,
    pub chroma_present: bool,
    pub bit_depth: u8,
    pub qp_precision: u8,
    pub max_ref_frames: u8,
    pub num_rans_streams: u8,
    pub min_block_size_log2: u8,
    pub max_block_size_log2: u8,
    pub quant_matrix_id: u8,
}

impl SequenceHeader {
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
        let chroma_present = read_u8(reader, "chroma_present")? != 0;
        let bit_depth = read_u8(reader, "bit_depth")?;
        let qp_precision = read_u8(reader, "qp_precision")?;
        let max_ref_frames = read_u8(reader, "max_ref_frames")?;
        let num_rans_streams = read_u8(reader, "num_rans_streams")?;
        let block_size_log2 = read_u8(reader, "block_size_log2")?;
        let quant_matrix_id = read_u8(reader, "quant_matrix_id")?;

        let min_block_size_log2 = block_size_log2 >> 4;
        let max_block_size_log2 = block_size_log2 & 0x0F;
        if min_block_size_log2 > max_block_size_log2 {
            return Err(KinetixError::Parse(format!(
                "sequence header: min_block_size_log2 ({min_block_size_log2}) > max_block_size_log2 ({max_block_size_log2})"
            )));
        }
        if bit_depth != 8 && bit_depth != 10 {
            return Err(KinetixError::Parse(format!(
                "sequence header: unsupported bit_depth {bit_depth}"
            )));
        }
        if quant_matrix_id > 3 {
            return Err(KinetixError::Parse(format!(
                "sequence header: quant_matrix_id {quant_matrix_id} > 3"
            )));
        }

        Ok(Self {
            version,
            max_width,
            max_height,
            chroma_present,
            bit_depth,
            qp_precision,
            max_ref_frames,
            num_rans_streams,
            min_block_size_log2,
            max_block_size_log2,
            quant_matrix_id,
        })
    }

    pub fn to_bytes(&self) -> [u8; SEQUENCE_HEADER_LEN] {
        let mut out = [0u8; SEQUENCE_HEADER_LEN];
        out[0..4].copy_from_slice(&MAGIC);
        out[4] = self.version;
        out[5..7].copy_from_slice(&self.max_width.to_be_bytes());
        out[7..9].copy_from_slice(&self.max_height.to_be_bytes());
        out[9] = u8::from(self.chroma_present);
        out[10] = self.bit_depth;
        out[11] = self.qp_precision;
        out[12] = self.max_ref_frames;
        out[13] = self.num_rans_streams;
        out[14] = (self.min_block_size_log2 << 4) | (self.max_block_size_log2 & 0x0F);
        out[15] = self.quant_matrix_id;
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
    pub output_mode: u8,
    pub payload_len: u32,
}

impl FrameHeader {
    pub fn parse(
        reader: &mut BitReader<'_>,
        sequence: &SequenceHeader,
    ) -> Result<Self, KinetixError> {
        let frame_type = FrameType::from_u8(read_u8(reader, "frame_type")?)?;
        let width = read_u16(reader, "width")?;
        let height = read_u16(reader, "height")?;
        let base_qp = read_u8(reader, "base_qp")?;
        let ref_frame_count = read_u8(reader, "ref_frame_count")?;
        let output_mode = read_u8(reader, "output_mode")?;
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
            output_mode,
            payload_len,
        })
    }

    pub fn to_bytes(&self) -> [u8; 15] {
        let mut out = [0u8; 15];
        out[0] = self.frame_type as u8;
        out[1..3].copy_from_slice(&self.width.to_be_bytes());
        out[3..5].copy_from_slice(&self.height.to_be_bytes());
        out[4 + 1] = self.base_qp;
        out[5 + 1] = self.ref_frame_count;
        out[6 + 1] = self.output_mode;
        out[7 + 1..11 + 1].copy_from_slice(&self.payload_len.to_be_bytes());
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
            chroma_present: false,
            bit_depth: 8,
            qp_precision: 0,
            max_ref_frames: 2,
            num_rans_streams: 1,
            min_block_size_log2: 3,
            max_block_size_log2: 3,
            quant_matrix_id: 0,
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
    fn frame_header_round_trips() {
        let seq = sample_sequence();
        let frame = FrameHeader {
            frame_type: FrameType::Key,
            width: 1280,
            height: 720,
            base_qp: 24,
            ref_frame_count: 0,
            output_mode: 2,
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
            output_mode: 0,
            payload_len: 0,
        };
        let bytes = frame.to_bytes();
        let mut reader = BitReader::new(&bytes);
        assert!(FrameHeader::parse(&mut reader, &seq).is_err());
    }
}
