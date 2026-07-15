//! NAL unit types and parsing stubs.
//!
//! TODO (Phase 3): Implement full NAL unit parsing (SPS, PPS, slice headers)
//! via `nom`.

/// H.264 NAL unit type codes (Table 7-1 of ITU-T H.264).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NalUnitType {
    Unspecified = 0,
    NonIdrSlice = 1,
    DataPartitionA = 2,
    DataPartitionB = 3,
    DataPartitionC = 4,
    IdrSlice = 5,
    Sei = 6,
    Sps = 7,
    Pps = 8,
    AccessUnitDelimiter = 9,
    EndOfSequence = 10,
    EndOfStream = 11,
    FillerData = 12,
    Unknown = 255,
}

impl NalUnitType {
    pub fn from_byte(byte: u8) -> Self {
        match byte & 0x1F {
            1 => Self::NonIdrSlice,
            2 => Self::DataPartitionA,
            3 => Self::DataPartitionB,
            4 => Self::DataPartitionC,
            5 => Self::IdrSlice,
            6 => Self::Sei,
            7 => Self::Sps,
            8 => Self::Pps,
            9 => Self::AccessUnitDelimiter,
            10 => Self::EndOfSequence,
            11 => Self::EndOfStream,
            12 => Self::FillerData,
            _ => Self::Unknown,
        }
    }

    pub fn is_vcl(self) -> bool {
        matches!(
            self,
            Self::NonIdrSlice
                | Self::DataPartitionA
                | Self::DataPartitionB
                | Self::DataPartitionC
                | Self::IdrSlice
        )
    }
}

/// A raw NAL unit — the forbidden_zero_bit, nal_ref_idc, and payload bytes.
#[derive(Debug, Clone)]
pub struct NalUnit {
    pub nal_unit_type: NalUnitType,
    /// nal_ref_idc field (2 bits).
    pub nal_ref_idc: u8,
    /// The RBSP payload bytes (after start-code and header byte removal).
    pub rbsp: Vec<u8>,
}

impl NalUnit {
    /// Parse a single NAL unit from an Annex B byte stream start code.
    ///
    /// TODO (Phase 3): Implement via `nom`.
    pub fn parse(_data: &[u8]) -> Option<Self> {
        None
    }
}
