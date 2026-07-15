//! NAL unit types and parsing.
//!
//! Implements full NAL unit extraction from both Annex B byte streams
//! and AVCC length-prefixed containers, plus emulation-prevention-byte removal.

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
    /// Map the low 5 bits of a NAL header byte to the corresponding type.
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

    /// Returns `true` for VCL (video coding layer) NAL types.
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

/// A parsed NAL unit containing the type, nal_ref_idc, and RBSP payload.
#[derive(Debug, Clone)]
pub struct NalUnit {
    /// NAL unit type (low 5 bits of the NAL header byte).
    pub nal_unit_type: NalUnitType,
    /// `nal_ref_idc` field (bits 5-6 of the NAL header byte).
    pub nal_ref_idc: u8,
    /// Raw Byte Sequence Payload — header byte removed and emulation-prevention
    /// bytes stripped.
    pub rbsp: Vec<u8>,
}

impl NalUnit {
    /// Construct a `NalUnit` from a raw NAL payload (including the header byte).
    ///
    /// Returns `None` if `raw` is empty.
    fn from_raw(raw: &[u8]) -> Option<Self> {
        if raw.is_empty() {
            return None;
        }
        let header = raw[0];
        // forbidden_zero_bit must be 0; ignore gracefully.
        let nal_ref_idc = (header >> 5) & 0x03;
        let nal_unit_type = NalUnitType::from_byte(header);
        let rbsp = remove_emulation_prevention_bytes(&raw[1..]);
        Some(Self {
            nal_unit_type,
            nal_ref_idc,
            rbsp,
        })
    }
}

/// Strip H.264 emulation-prevention bytes from an RBSP byte sequence.
///
/// The H.264 spec (Section 7.4.1) forbids the byte sequences `00 00 00`,
/// `00 00 01`, and `00 00 02` inside RBSP data; an encoder inserts `03` as
/// a "emulation prevention byte" to break up such sequences.  This function
/// removes those `03` bytes.
///
/// Example: `[0x00, 0x00, 0x03, 0x01]` → `[0x00, 0x00, 0x01]`.
///
/// # Examples
///
/// ```
/// use kinetix_h264::nal::remove_emulation_prevention_bytes;
/// // 00 00 03 01 -> 00 00 01 (emulation prevention byte removed)
/// let input = [0x00, 0x00, 0x03, 0x01];
/// assert_eq!(remove_emulation_prevention_bytes(&input), vec![0x00, 0x00, 0x01]);
/// ```
pub fn remove_emulation_prevention_bytes(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        // Detect `00 00 03` followed by 00/01/02/03.
        if i + 2 < data.len()
            && data[i] == 0x00
            && data[i + 1] == 0x00
            && data[i + 2] == 0x03
            && (i + 3 >= data.len() || data[i + 3] <= 0x03)
        {
            out.push(0x00);
            out.push(0x00);
            i += 3; // skip the emulation-prevention 0x03
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

/// Parse NAL units from an Annex B byte stream.
///
/// Annex B uses start codes (`00 00 00 01` or `00 00 01`) as delimiters.
/// The returned `NalUnit`s have their RBSP emulation-prevention bytes stripped.
pub fn parse_nal_units_from_annexb(data: &[u8]) -> Vec<NalUnit> {
    // Collect byte offsets of all start codes.
    let mut starts: Vec<usize> = Vec::new();
    let mut i = 0;
    while i + 2 < data.len() {
        if data[i] == 0x00 && data[i + 1] == 0x00 {
            if i + 3 < data.len() && data[i + 2] == 0x00 && data[i + 3] == 0x01 {
                starts.push(i + 4); // 4-byte start code
                i += 4;
                continue;
            } else if data[i + 2] == 0x01 {
                starts.push(i + 3); // 3-byte start code
                i += 3;
                continue;
            }
        }
        i += 1;
    }

    // Extract the raw bytes between consecutive start codes.
    let mut units = Vec::with_capacity(starts.len());
    for (idx, &start) in starts.iter().enumerate() {
        let end = if idx + 1 < starts.len() {
            // Back up past any leading zero bytes of the next start code.
            let next_start_code_begin = starts[idx + 1];
            // The start code is either 3 or 4 bytes, preceded by the next
            // start marker.  We need to find where the trailing zeros begin.
            let mut e = next_start_code_begin;
            // Walk back over the leading 0x00 bytes that belong to the next SC.
            if e >= 3 && data[e - 3] == 0x00 && data[e - 2] == 0x00 && data[e - 1] == 0x01 {
                e -= 3;
            } else if e >= 4
                && data[e - 4] == 0x00
                && data[e - 3] == 0x00
                && data[e - 2] == 0x00
                && data[e - 1] == 0x01
            {
                e -= 4;
            }
            // Strip any trailing zero bytes that precede the start code.
            while e > start && data[e - 1] == 0x00 {
                e -= 1;
            }
            e
        } else {
            data.len()
        };

        if start < end {
            if let Some(unit) = NalUnit::from_raw(&data[start..end]) {
                units.push(unit);
            }
        }
    }
    units
}

/// Parse NAL units from an AVCC (ISO 14496-15) length-prefixed stream.
///
/// `length_size` is the number of bytes used for each length prefix (1, 2, or 4).
pub fn parse_nal_units_from_avcc(data: &[u8], length_size: u8) -> Vec<NalUnit> {
    let ls = length_size as usize;
    if ls == 0 || ls == 3 || ls > 4 {
        return Vec::new();
    }

    let mut units = Vec::new();
    let mut pos = 0;
    while pos + ls <= data.len() {
        // Read the length prefix (big-endian).
        let mut len: usize = 0;
        for b in data[pos..pos + ls].iter() {
            len = (len << 8) | *b as usize;
        }
        pos += ls;

        if len == 0 || pos + len > data.len() {
            break;
        }

        if let Some(unit) = NalUnit::from_raw(&data[pos..pos + len]) {
            units.push(unit);
        }
        pos += len;
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remove_emulation_prevention_bytes_basic() {
        // 00 00 03 01 → 00 00 01
        let input = [0x00u8, 0x00, 0x03, 0x01];
        let output = remove_emulation_prevention_bytes(&input);
        assert_eq!(output, vec![0x00u8, 0x00, 0x01]);
    }

    #[test]
    fn test_remove_emulation_prevention_bytes_no_epb() {
        let input = [0x00u8, 0x01, 0x02, 0x03];
        let output = remove_emulation_prevention_bytes(&input);
        assert_eq!(output, input.to_vec());
    }

    #[test]
    fn test_remove_emulation_prevention_bytes_multiple() {
        // 00 00 03 02 is a valid EPB sequence → 00 00 02
        // followed by normal bytes
        let input = [0x00u8, 0x00, 0x03, 0x02, 0xFF];
        let output = remove_emulation_prevention_bytes(&input);
        assert_eq!(output, vec![0x00u8, 0x00, 0x02, 0xFF]);
    }

    #[test]
    fn test_annexb_parse_4byte_start_code() {
        // A minimal SPS NAL unit: start code + header byte (0x67 = SPS) + dummy payload
        let data = [0x00u8, 0x00, 0x00, 0x01, 0x67, 0x42, 0xC0, 0x1E];
        let units = parse_nal_units_from_annexb(&data);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].nal_unit_type, NalUnitType::Sps);
        assert_eq!(units[0].nal_ref_idc, 3); // bits 5-6 of 0x67 = 11
    }

    #[test]
    fn test_annexb_parse_3byte_start_code() {
        let data = [0x00u8, 0x00, 0x01, 0x68, 0xCE, 0x38, 0x80];
        let units = parse_nal_units_from_annexb(&data);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].nal_unit_type, NalUnitType::Pps);
    }

    #[test]
    fn test_avcc_parse() {
        // length_size=4, one NAL unit of 3 bytes: 0x65 (IDR slice)
        let mut data = vec![0x00u8, 0x00, 0x00, 0x03]; // length = 3
        data.extend_from_slice(&[0x65, 0xB8, 0x00]); // IDR header + 2 bytes
        let units = parse_nal_units_from_avcc(&data, 4);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].nal_unit_type, NalUnitType::IdrSlice);
    }

    #[test]
    fn test_nal_unit_type_is_vcl() {
        assert!(NalUnitType::IdrSlice.is_vcl());
        assert!(NalUnitType::NonIdrSlice.is_vcl());
        assert!(!NalUnitType::Sps.is_vcl());
        assert!(!NalUnitType::Pps.is_vcl());
    }
}
