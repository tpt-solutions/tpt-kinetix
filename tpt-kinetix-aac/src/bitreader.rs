//! MSB-first bit reader for the AAC noiseless (Huffman) coding layer.
//!
//! AAC raw data blocks, ADTS payloads, and `AudioSpecificConfig` blobs are read
//! MSB-first (the most-significant bit of each byte is consumed first), matching
//! the AAC / ISO 14496-3 bitstream convention. The API deliberately mirrors
//! `tpt_kinetix_h264::bitreader` (MSB-first, `Option`-returning fallible
//! reads, bit-position bookkeeping) so the two codecs share the same reader
//! ergonomics, and adds the AAC-specific "escape value" helpers used by the
//! section and spectral-data syntax.

/// A MSB-first bit reader over a byte slice.
///
/// Every fallible read returns `Option`; `None` means the stream was exhausted
/// mid-field. There is no panicking path, which keeps the parser safe to drive
/// from untrusted input (the fuzz/property harnesses rely on this).
#[derive(Debug, Clone, Copy)]
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

    /// Read up to 32 bits, MSB first. Returns `None` if the stream runs out or if
    /// `n > 32` (the result cannot fit in a `u32`).
    pub fn read_bits(&mut self, n: u32) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        if n > 32 {
            return None;
        }
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

    /// Read an unsigned Exp-Golomb coded integer (`ue(v)`).
    ///
    /// Not used by AAC's own syntax (which is fixed-length + Huffman), but
    /// provided for API parity with the H.264 reader and to ease cross-codec
    /// reuse.
    pub fn read_ue(&mut self) -> Option<u32> {
        let mut leading_zeros: u32 = 0;
        loop {
            let bit = self.read_bit()?;
            if bit == 1 {
                break;
            }
            leading_zeros += 1;
            if leading_zeros > 31 {
                return None;
            }
        }
        if leading_zeros == 0 {
            return Some(0);
        }
        let suffix = self.read_bits(leading_zeros)?;
        Some((1u32 << leading_zeros).wrapping_sub(1).wrapping_add(suffix))
    }

    /// Read a signed Exp-Golomb coded integer (`se(v)`).
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

    /// Peek `n` bits (MSB-first) without advancing the read position.
    ///
    /// Returns `None` if fewer than `n` bits remain.
    pub fn peek(&self, n: u32) -> Option<u32> {
        let mut tmp = *self;
        tmp.read_bits(n)
    }

    /// Consume `n` bits without returning them.
    pub fn skip(&mut self, n: u32) {
        let total = self.bit_position() + n as usize;
        self.seek_to_bit(total);
    }

    /// Number of bits still available in the buffer.
    pub fn remaining_bits(&self) -> usize {
        if self.byte_pos >= self.data.len() {
            return 0;
        }
        (self.data.len() - self.byte_pos) * 8 - self.bit_pos as usize
    }

    /// Absolute bit position from the start of the stream (bits already consumed).
    #[inline]
    pub fn bit_position(&self) -> usize {
        self.byte_pos * 8 + self.bit_pos as usize
    }

    /// Seek to an absolute bit position from the start of the stream.
    pub fn seek_to_bit(&mut self, bit: usize) {
        self.byte_pos = bit / 8;
        self.bit_pos = (bit % 8) as u8;
    }

    /// Align to the next byte boundary. No-op when already aligned.
    pub fn byte_align(&mut self) {
        if self.bit_pos != 0 {
            self.byte_pos += 1;
            self.bit_pos = 0;
        }
    }

    /// Returns `true` if the current position is byte-aligned (`bit_pos == 0`).
    #[inline]
    pub fn is_aligned(&self) -> bool {
        self.bit_pos == 0
    }

    /// Read an AAC "escape value": read `base_bits` bits; if the value equals the
    /// all-ones sentinel `(1 << base_bits) - 1`, read `esc_bits` additional bits
    /// and return `sentinel + extra`. Otherwise return the base value.
    ///
    /// This is the two-stage escape used throughout ISO 14496-3 (e.g. the
    /// `AudioSpecificConfig` object-type escape reads 5 bits and, on the
    /// sentinel `31`, a further 6 bits). For the section-length escape — which
    /// repeats the sentinel to encode arbitrarily long values — see
    /// [`BitReader::read_section_length`].
    pub fn read_escape(&mut self, base_bits: u32, esc_bits: u32) -> Option<u32> {
        let base = self.read_bits(base_bits)?;
        let sentinel = (1u32 << base_bits) - 1;
        if base == sentinel {
            let extra = self.read_bits(esc_bits)?;
            Some(sentinel + extra)
        } else {
            Some(base)
        }
    }

    /// Read an AAC section length using the repeated-sentinel escape.
    ///
    /// A section length is the sum of successive `section_len_bits`-wide
    /// increments; an increment equal to the all-ones sentinel `(1 <<
    /// section_len_bits) - 1` signals that more increments follow (this is how
    /// a single section can cover more scalefactor bands than a single field
    /// width allows). `saturating_add` bounds the accumulator so a hostile
    /// stream cannot trigger arithmetic overflow.
    pub fn read_section_length(&mut self, section_len_bits: u32) -> Option<u32> {
        let sentinel = (1u32 << section_len_bits) - 1;
        let mut total = 0u32;
        loop {
            let incr = self.read_bits(section_len_bits)?;
            total = total.saturating_add(incr);
            if incr != sentinel {
                break;
            }
        }
        Some(total)
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
    fn test_read_bit_and_position() {
        let data = [0b1010_0101u8];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_bit(), Some(1));
        assert_eq!(r.read_bit(), Some(0));
        assert_eq!(r.bit_position(), 2);
        assert_eq!(r.remaining_bits(), 6);
    }

    #[test]
    fn test_read_ue_known_values() {
        // 0→"1", 1→"010", 2→"011", 3→"00100", 4→"00101" => 0xA6 0x42 0x80
        let data = [0xA6u8, 0x42, 0x80];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_ue(), Some(0));
        assert_eq!(r.read_ue(), Some(1));
        assert_eq!(r.read_ue(), Some(2));
        assert_eq!(r.read_ue(), Some(3));
        assert_eq!(r.read_ue(), Some(4));
    }

    #[test]
    fn test_read_se_known_values() {
        let data = [0xA6u8, 0x42, 0x80];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_se(), Some(0));
        assert_eq!(r.read_se(), Some(1));
        assert_eq!(r.read_se(), Some(-1));
        assert_eq!(r.read_se(), Some(2));
        assert_eq!(r.read_se(), Some(-2));
    }

    #[test]
    fn test_read_escape_base_only() {
        // base_bits=4, value 5 (not the 0xF sentinel) => returns 5.
        // 0101 ...
        let data = [0b0101_0000u8];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_escape(4, 4), Some(5));
    }

    #[test]
    fn test_read_escape_with_sentinel() {
        // base_bits=4 sentinel (1111) then esc_bits=4 value 3 (0011) => 15 + 3.
        // 1111 0011 ...
        let data = [0b1111_0011u8];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_escape(4, 4), Some(18));
    }

    #[test]
    fn test_read_section_length_accumulates() {
        // section_len_bits=4, increments 15,15,7 => total 15+15+7 = 37.
        // 1111 1111 0111 ...
        let data = [0b1111_1111u8, 0b0111_0000u8];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_section_length(4), Some(37));
    }

    #[test]
    fn test_read_section_length_single() {
        // section_len_bits=4, increment 10 then stop (10 != 15).
        // 1010 ...
        let data = [0b1010_0000u8];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_section_length(4), Some(10));
    }

    #[test]
    fn test_peek_does_not_advance() {
        let data = [0b1010_0101u8];
        let mut r = BitReader::new(&data);
        assert_eq!(r.peek(4), Some(0b1010));
        assert_eq!(r.peek(4), Some(0b1010));
        assert_eq!(r.read_bits(4), Some(0b1010));
        assert_eq!(r.bit_position(), 4);
    }

    #[test]
    fn test_exhaustion_returns_none() {
        let data = [0xFFu8];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_bits(8), Some(0xFF));
        assert_eq!(r.read_bit(), None);
        assert_eq!(r.read_bits(1), None);
    }
}
