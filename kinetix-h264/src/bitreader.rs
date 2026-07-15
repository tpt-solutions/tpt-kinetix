//! Bit-level reader over a byte slice.
//!
//! Bits are consumed MSB-first within each byte, matching the H.264 bitstream
//! convention (Section 7.2 of ITU-T H.264).

/// Efficient bit-level reader over a byte slice.
pub struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    /// Bit index within `data[byte_pos]`: 0 = MSB (about to be read next).
    bit_pos: u8,
}

impl<'a> BitReader<'a> {
    /// Create a new `BitReader` positioned at the start of `data`.
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    /// Read the next single bit (0 or 1), or `None` if the stream is exhausted.
    pub fn read_bit(&mut self) -> Option<u8> {
        if self.byte_pos >= self.data.len() {
            return None;
        }
        let bit = (self.data[self.byte_pos] >> (7 - self.bit_pos)) & 1;
        self.bit_pos += 1;
        if self.bit_pos == 8 {
            self.byte_pos += 1;
            self.bit_pos = 0;
        }
        Some(bit)
    }

    /// Read up to 32 bits, MSB first.  Returns `None` if the stream runs out.
    pub fn read_bits(&mut self, n: u8) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        debug_assert!(n <= 32, "read_bits: n > 32 is not supported");
        let mut result = 0u32;
        for _ in 0..n {
            let bit = self.read_bit()?;
            result = (result << 1) | bit as u32;
        }
        Some(result)
    }

    /// Read the next 8 bits as a `u8`.
    #[inline]
    pub fn read_u8(&mut self) -> Option<u8> {
        self.read_bits(8).map(|v| v as u8)
    }

    /// Read the next 16 bits as a big-endian `u16`.
    #[inline]
    pub fn read_u16_be(&mut self) -> Option<u16> {
        self.read_bits(16).map(|v| v as u16)
    }

    /// Read an unsigned Exp-Golomb coded integer (`ue(v)` in H.264 syntax).
    ///
    /// Exp-Golomb(k): count leading zeros `M`, then read `M` more bits.
    /// Value = 2^M − 1 + trailing_bits.
    pub fn read_ue(&mut self) -> Option<u32> {
        let mut leading_zeros: u32 = 0;
        loop {
            let bit = self.read_bit()?;
            if bit == 1 {
                break;
            }
            leading_zeros += 1;
            if leading_zeros > 31 {
                // Guard against malformed streams.
                return None;
            }
        }
        if leading_zeros == 0 {
            return Some(0);
        }
        let suffix = self.read_bits(leading_zeros as u8)?;
        // 2^leading_zeros − 1 + suffix
        Some((1u32 << leading_zeros).wrapping_sub(1).wrapping_add(suffix))
    }

    /// Read a signed Exp-Golomb coded integer (`se(v)` in H.264 syntax).
    ///
    /// Maps ue value `k` to signed via: k=0→0, k=1→1, k=2→−1, k=3→2, k=4→−2 …
    pub fn read_se(&mut self) -> Option<i32> {
        let ue = self.read_ue()?;
        let se = if ue == 0 {
            0
        } else if ue % 2 == 1 {
            (ue + 1).div_ceil(2) as i32
        } else {
            -((ue / 2) as i32)
        };
        Some(se)
    }

    /// Number of bits remaining in the stream.
    pub fn remaining_bits(&self) -> usize {
        if self.byte_pos >= self.data.len() {
            return 0;
        }
        (self.data.len() - self.byte_pos) * 8 - self.bit_pos as usize
    }

    /// Returns `true` if the current position is byte-aligned (`bit_pos == 0`).
    #[inline]
    pub fn is_aligned(&self) -> bool {
        self.bit_pos == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// H.264 Exp-Golomb unsigned mapping (Table 9-1 of the spec):
    /// symbol 0  → codeword "1"
    /// symbol 1  → codeword "010"
    /// symbol 2  → codeword "011"
    /// symbol 3  → codeword "00100"
    /// symbol 4  → codeword "00101"
    #[test]
    fn test_read_ue_known_values() {
        // Build a byte stream that encodes: 0, 1, 2, 3, 4 in sequence.
        // 0  → 1           (1 bit)
        // 1  → 010         (3 bits)
        // 2  → 011         (3 bits)
        // 3  → 00100       (5 bits)
        // 4  → 00101       (5 bits)
        //                    Total: 17 bits → 3 bytes (padded with 0s)
        //
        // Concatenated: 1_010_011_00100_00101 → pad to 24 bits
        // 1010_0110_0100_0010_1000_0000
        //  A    6    4    2    8    0
        //
        // Let me compute this carefully:
        // bits: 1 0 1 0 0 1 1 0 0 1 0 0 0 0 1 0 1 + 7 pad zeros
        // byte 0: 1010 0110 = 0xA6
        // byte 1: 0100 0010 = 0x42
        // byte 2: 1000 0000 = 0x80
        let data = [0xA6u8, 0x42, 0x80];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_ue(), Some(0));
        assert_eq!(r.read_ue(), Some(1));
        assert_eq!(r.read_ue(), Some(2));
        assert_eq!(r.read_ue(), Some(3));
        assert_eq!(r.read_ue(), Some(4));
    }

    #[test]
    fn test_read_bits() {
        let data = [0b1010_1010u8, 0b1111_0000];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_bits(4), Some(0b1010));
        assert_eq!(r.read_bits(4), Some(0b1010));
        assert_eq!(r.read_bits(8), Some(0b1111_0000));
    }

    #[test]
    fn test_remaining_bits() {
        let data = [0xFFu8, 0xFF];
        let mut r = BitReader::new(&data);
        assert_eq!(r.remaining_bits(), 16);
        let _ = r.read_bits(4);
        assert_eq!(r.remaining_bits(), 12);
    }

    #[test]
    fn test_read_se() {
        // se mapping: ue=0→0, ue=1→1, ue=2→-1, ue=3→2, ue=4→-2
        // Encode: 0 (ue=0 → "1"), 1 (ue=1 → "010"), -1 (ue=2 → "011"),
        //         2 (ue=3 → "00100"), -2 (ue=4 → "00101")
        // Same bits as test above
        let data = [0xA6u8, 0x42, 0x80];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_se(), Some(0));
        assert_eq!(r.read_se(), Some(1));
        assert_eq!(r.read_se(), Some(-1));
        assert_eq!(r.read_se(), Some(2));
        assert_eq!(r.read_se(), Some(-2));
    }
}
