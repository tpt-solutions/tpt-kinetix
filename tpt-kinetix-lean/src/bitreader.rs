//! Bit-level reader over a byte slice, MSB-first within each byte.
//!
//! This is Lean's own copy rather than a shared dependency — the workspace
//! has no shared bitstream-utility crate yet (`tpt-kinetix-h264` has its own
//! equivalent in `bitreader.rs`). Whether to factor a shared
//! `tpt-kinetix-bitstream` crate now that this is the second hand-rolled
//! reader is an open question tracked in `todo.md` (Phase 13), not decided
//! here.

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

    /// Read up to 32 bits, MSB first. Returns `None` if the stream runs out.
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

    /// Read the next 32 bits as a big-endian `u32`.
    #[inline]
    pub fn read_u32_be(&mut self) -> Option<u32> {
        self.read_bits(32)
    }

    /// Number of bits remaining in the stream.
    pub fn remaining_bits(&self) -> usize {
        if self.byte_pos >= self.data.len() {
            return 0;
        }
        (self.data.len() - self.byte_pos) * 8 - self.bit_pos as usize
    }

    /// Absolute bit position from the start of the stream.
    #[inline]
    pub fn bit_position(&self) -> usize {
        self.byte_pos * 8 + self.bit_pos as usize
    }

    /// Align to the next byte boundary. No-op when already aligned. Used
    /// before entering an rANS-coded region, which is byte-framed (see
    /// [`crate::rans`]).
    pub fn byte_align(&mut self) {
        if self.bit_pos != 0 {
            self.byte_pos += 1;
            self.bit_pos = 0;
        }
    }

    /// Returns `true` if the current position is byte-aligned.
    #[inline]
    pub fn is_aligned(&self) -> bool {
        self.bit_pos == 0
    }

    /// Returns the remaining bytes from the current (byte-aligned) position.
    ///
    /// Panics if the reader is not byte-aligned — call [`Self::byte_align`]
    /// first.
    pub fn remaining_bytes(&self) -> &'a [u8] {
        assert!(
            self.is_aligned(),
            "remaining_bytes: reader is not byte-aligned"
        );
        &self.data[self.byte_pos..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_bits() {
        let data = [0b1010_1010u8, 0b1111_0000];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_bits(4), Some(0b1010));
        assert_eq!(r.read_bits(4), Some(0b1010));
        assert_eq!(r.read_bits(8), Some(0b1111_0000));
    }

    #[test]
    fn test_remaining_bits_and_exhaustion() {
        let data = [0xFFu8, 0xFF];
        let mut r = BitReader::new(&data);
        assert_eq!(r.remaining_bits(), 16);
        let _ = r.read_bits(12);
        assert_eq!(r.remaining_bits(), 4);
        assert_eq!(r.read_bits(8), None);
    }

    #[test]
    fn test_byte_align() {
        let data = [0xFFu8, 0x00];
        let mut r = BitReader::new(&data);
        let _ = r.read_bits(3);
        assert!(!r.is_aligned());
        r.byte_align();
        assert!(r.is_aligned());
        assert_eq!(r.bit_position(), 8);
    }
}
