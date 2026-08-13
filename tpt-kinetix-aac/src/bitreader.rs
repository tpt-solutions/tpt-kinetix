//! MSB-first bit reader for the AAC noiseless (Huffman) coding layer.
//!
//! AAC packets are read MSB-first; Huffman codewords are packed starting from
//! the most-significant bit of each byte. The reader supports peeking without
//! advancing (needed by the 2-step Huffman decode) and bounded consumption.

/// A MSB-first bit reader over a byte slice.
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Current bit position within `data`.
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Number of bits still available in the buffer.
    pub(crate) fn bits_left(&self) -> usize {
        self.data.len() * 8 - self.pos
    }

    /// Peek `n` bits (MSB-first) without advancing the read position.
    ///
    /// Returns 0 if fewer than `n` bits remain (callers must guard on
    /// [`bits_left`](Self::bits_left) before relying on the value).
    pub(crate) fn peek(&self, n: u32) -> u32 {
        let mut value = 0u32;
        for i in 0..n {
            let bit_pos = self.pos + i as usize;
            if bit_pos >= self.data.len() * 8 {
                break;
            }
            let byte = self.data[bit_pos / 8];
            let bit = (byte >> (7 - (bit_pos % 8))) & 1;
            value = (value << 1) | bit as u32;
        }
        value
    }

    /// Read and consume `n` bits (MSB-first). Returns 0 past end-of-buffer.
    pub(crate) fn get(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }

    /// Read a single bit.
    pub(crate) fn get_bit(&mut self) -> u32 {
        self.get(1)
    }

    /// Consume `n` bits without returning them.
    pub(crate) fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }
}
