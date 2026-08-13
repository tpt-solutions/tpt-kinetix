//! CRC helpers for the lossless codec's built-in reversibility contract.
//!
//! Every encoded plane carries a checksum of its reconstructed samples; the
//! decoder verifies it and returns an error on mismatch instead of yielding
//! wrong data. See `docs/lossless-codec-design.md` DECISION 3.

/// IEEE 802.3 CRC-32 (poly `0xEDB8_8320`, reflected).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// ISO/IEC 13818-1 CRC-64 (poly `0x42F0_E1EB_A9EA_3693`, reflected).
///
/// Used for 16-bit planes where CRC-32's collision comfort is insufficient
/// over large sample payloads (per `docs/lossless-codec-design.md` DECISION 3).
pub fn crc64(data: &[u8]) -> u64 {
    let mut crc: u64 = 0xFFFF_FFFF_FFFF_FFFF;
    for &b in data {
        crc ^= u64::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x42F0_E1EB_A9EA_3693
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Pick the checksum width by plane bit depth, per DECISION 3.
pub fn checksum_plane(bit_depth: u8, samples: &[u16]) -> Vec<u8> {
    let bytes: Vec<u8> = samples
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    if bit_depth >= 16 {
        crc64(&bytes).to_le_bytes().to_vec()
    } else {
        crc32(&bytes).to_le_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_known_empty() {
        assert_eq!(crc32(&[]), 0x0000_0000);
    }

    #[test]
    fn crc64_known_empty() {
        assert_eq!(crc64(&[]), 0x0000_0000_0000_0000);
    }

    #[test]
    fn crc_changes_with_content() {
        assert_ne!(crc32(b"hello"), crc32(b"world"));
        assert_ne!(crc64(b"hello"), crc64(b"world"));
    }
}
