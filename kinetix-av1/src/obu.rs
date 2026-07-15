//! AV1 Open Bitstream Unit (OBU) parsing stubs.
//!
//! TODO (Phase 4): Implement full OBU header + payload parsing via `nom`,
//! validated against `dav1d` reference output.

/// AV1 OBU type field values (AV1 spec §5.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ObuType {
    SequenceHeader = 1,
    TemporalDelimiter = 2,
    FrameHeader = 3,
    TileGroup = 4,
    Metadata = 5,
    Frame = 6,
    RedundantFrameHeader = 7,
    TileList = 8,
    Padding = 15,
    Reserved,
}

impl ObuType {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::SequenceHeader,
            2 => Self::TemporalDelimiter,
            3 => Self::FrameHeader,
            4 => Self::TileGroup,
            5 => Self::Metadata,
            6 => Self::Frame,
            7 => Self::RedundantFrameHeader,
            8 => Self::TileList,
            15 => Self::Padding,
            _ => Self::Reserved,
        }
    }
}

/// A single parsed OBU with its header fields and payload bytes.
#[derive(Debug, Clone)]
pub struct Obu {
    pub obu_type: ObuType,
    /// Whether the OBU extension header is present.
    pub extension_flag: bool,
    /// Whether the OBU size field is present.
    pub has_size_field: bool,
    /// Raw OBU payload bytes.
    pub payload: Vec<u8>,
}

impl Obu {
    /// Parse one OBU from the front of `data`.
    ///
    /// TODO (Phase 4): Implement via `nom`.
    pub fn parse(_data: &[u8]) -> Option<(Self, usize)> {
        None
    }
}
