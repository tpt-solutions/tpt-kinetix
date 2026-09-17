//! MSB-first raw bit reader for the VP9 uncompressed header (§6.2).
//!
//! All `f(n)` fields in the VP9 spec are read most-significant-bit first from
//! whole bytes; the compressed header and tile data use the bool decoder in
//! [`crate::booldec`] instead.

use tpt_kinetix_core::error::KinetixError;

/// Reads big-endian bit fields from a byte slice.
#[derive(Debug)]
pub struct BitReader<'a> {
    data: &'a [u8],
    /// Next bit to read (0-based from the start of `data`).
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Number of bits consumed so far.
    pub fn bit_pos(&self) -> usize {
        self.pos
    }

    /// Round the consumed bit count up to the next whole byte offset.
    pub fn byte_pos(&self) -> usize {
        self.pos.div_ceil(8)
    }

    /// Byte offset of the next unread byte (skipping any padding bits of a
    /// partially consumed byte), as the spec's `byte_alignment()` does.
    pub fn aligned_byte_pos(&self) -> usize {
        self.byte_pos()
    }

    /// Read `n` bits (n <= 24) MSB-first. Missing bits past the end of the
    /// buffer read as zero, matching reference-decoder behaviour of padding
    /// with zeros — but a read that starts past the end is a parse error via
    /// [`Self::try_f`].
    pub fn f(&mut self, n: u32) -> u32 {
        let mut v: u32 = 0;
        for _ in 0..n {
            let byte = self.data.get(self.pos / 8).copied().unwrap_or(0);
            let bit = (byte >> (7 - (self.pos % 8))) & 1;
            v = (v << 1) | u32::from(bit);
            self.pos += 1;
        }
        v
    }

    /// [`Self::f`] with an out-of-data check: errors once the read extends
    /// past the end of the buffer.
    pub fn try_f(&mut self, n: u32, what: &str) -> Result<u32, KinetixError> {
        if self.pos + n as usize > self.data.len() * 8 {
            return Err(KinetixError::Parse(format!(
                "vp9: uncompressed header truncated while reading {what} ({} bits at bit {} of {} bytes)",
                n,
                self.pos,
                self.data.len()
            )));
        }
        Ok(self.f(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_msb_first_fields() {
        let data = [0b1011_0010, 0b0111_0001];
        let mut r = BitReader::new(&data);
        assert_eq!(r.f(2), 0b10);
        assert_eq!(r.f(1), 1);
        assert_eq!(r.f(3), 0b100);
        assert_eq!(r.f(10), 0b1001110001); // 1,0 | 01110001 crossing the byte boundary
        assert_eq!(r.bit_pos(), 16);
    }

    #[test]
    fn zero_padding_past_end() {
        let data = [0b1000_0000];
        let mut r = BitReader::new(&data);
        assert_eq!(r.f(1), 1);
        assert_eq!(r.f(7), 0);
        assert_eq!(r.f(4), 0); // past the end: zero-padded
        assert_eq!(r.byte_pos(), 2);
    }

    #[test]
    fn try_f_errors_past_end() {
        let mut r = BitReader::new(&[0xff]);
        assert!(r.try_f(8, "x").is_ok());
        assert!(r.try_f(1, "x").is_err());
    }
}
