//! Bit-level reader over a byte slice, MSB-first within each byte.
//!
//! Shared across the TPT Kinetix original codecs (lean / vision / realtime).
//! Previously each of those crates carried its own copy; this is the single
//! source of truth (see `docs/realtime-codec-design.md` DECISION 7).

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
    ///
    /// This is the primitive every other read is built on, so it stays
    /// branch-light: a single bounds-checked load, no helper calls, and the
    /// wrap to the next byte handled with a mask instead of a modulo. For
    /// bulk reads prefer [`Self::read_bits`], which refills a 64-bit window
    /// instead of stepping one bit at a time.
    #[inline]
    pub fn read_bit(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.byte_pos)?;
        let bit = (byte >> (7 - self.bit_pos)) & 1;
        self.bit_pos += 1;
        if self.bit_pos == 8 {
            self.byte_pos += 1;
            self.bit_pos = 0;
        }
        Some(bit)
    }

    /// Read up to 32 bits, MSB first. Returns `None` if the stream runs out.
    ///
    /// # Performance
    ///
    /// When the requested bits are fully in range this refills a 64-bit
    /// big-endian window once and shifts, instead of calling [`Self::read_bit`]
    /// per bit — the per-bit call overhead (not the data movement) dominated
    /// this function, so the window path is several times faster for the
    /// multi-bit reads every header parser actually performs. The bit-at-a-time
    /// path is still used for the tail, where fewer than the 8 window bytes
    /// remain; it also preserves the original partial-consumption behaviour on
    /// exhaustion (bits successfully read before the end are consumed, then
    /// `None` is returned).
    pub fn read_bits(&mut self, n: u8) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        debug_assert!(n <= 32, "read_bits: n > 32 is not supported");

        // Bits needed from the start of the current byte, and how many whole
        // bytes that spans (ceil).
        let need = self.bit_pos as usize + n as usize;
        let span = need.div_ceil(8);
        if self.byte_pos + span <= self.data.len() {
            // Refill: copy the up-to-5 in-range bytes into a u64 window. The
            // copy is a fixed-size memmove the optimiser unrolls; the tail
            // bytes stay zero and are masked off below.
            let src = &self.data[self.byte_pos..self.byte_pos + span];
            let mut window = [0u8; 8];
            window[..span].copy_from_slice(src);
            let value = u64::from_be_bytes(window) >> (64 - need);

            self.bit_pos += n;
            self.byte_pos += (self.bit_pos >> 3) as usize;
            self.bit_pos &= 7;

            // `n <= 32`, so the mask never shifts by 64.
            let mask = (1u64 << n) - 1;
            return Some((value & mask) as u32);
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

    /// Independent reference: step the bit position one bit at a time, with no
    /// window refill and no shared code with `read_bits`.
    fn reference_read_bits(data: &[u8], bit_pos: usize, n: u8) -> Option<u32> {
        let mut value = 0u32;
        for i in 0..n as usize {
            let abs = bit_pos + i;
            let byte = *data.get(abs / 8)?;
            value = (value << 1) | ((byte >> (7 - (abs % 8))) & 1) as u32;
        }
        Some(value)
    }

    /// `read_bits` must agree with a bit-at-a-time reference for every width
    /// and every starting bit offset — covering the window fast path, the tail
    /// fallback, and the exhaustion boundary.
    #[test]
    fn read_bits_matches_bit_at_a_time_reference() {
        // Deterministic xorshift payload, wide enough to exercise every offset.
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let data: Vec<u8> = (0..64)
            .map(|_| {
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                (state.wrapping_mul(0x2545F491_4F6CDD1D) >> 56) as u8
            })
            .collect();

        for pre in 0..8u8 {
            for n in 1..=32u8 {
                let mut r = BitReader::new(&data);
                let _ = r.read_bits(pre);

                // Walk the whole payload one read at a time, checking every
                // value against the reference, starting from the position
                // *before* each read (the tail path consumes a partial read).
                let mut pos = r.bit_position();
                loop {
                    let got = r.read_bits(n);
                    let want = reference_read_bits(&data, pos, n);
                    match (got, want) {
                        (Some(g), Some(w)) => {
                            assert_eq!(g, w, "value mismatch: pre={pre} n={n} at {pos}");
                        }
                        (None, None) => break,
                        (g, w) => panic!(
                            "exhaustion mismatch: pre={pre} n={n} at {pos}: got {g:?} want {w:?}"
                        ),
                    }
                    pos += n as usize;
                }
                // The walk stops at the first read that cannot be satisfied; the
                // reader consumes the bits that *were* available before
                // reporting `None`, so it ends up at the end of the payload
                // while the reference walk stopped at the start of the short
                // read. Every fully-satisfied read above already matched.
                assert!(pos <= data.len() * 8, "overran the payload: {pos}");
                assert!(
                    data.len() * 8 - pos < n as usize,
                    "walk should have ended short"
                );
                assert_eq!(
                    r.bit_position(),
                    data.len() * 8,
                    "reader did not stop at the end: pre={pre} n={n}"
                );
            }
        }
    }

    /// The window path must never read past the end of the buffer: a read that
    /// starts in range but extends beyond it reports exhaustion exactly like
    /// the bit-at-a-time path, and does not consume the partial read.
    #[test]
    fn read_bits_straddling_end_of_buffer() {
        let data = [0b1011_0110u8, 0b0110_0001];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_bits(3), Some(0b101));
        // 3 + 6 = 9 bits, which are in range across the byte boundary.
        assert_eq!(r.read_bits(6), Some(0b101100));
        assert_eq!(r.remaining_bits(), 7);
        assert_eq!(r.bit_position(), 9);
        // Only 7 bits are left, so this 8-bit read must fail. As before the
        // window fast path, exhaustion consumes the bits that *were* available
        // and then reports `None` — preserved, not changed.
        assert_eq!(r.read_bits(8), None);
        assert_eq!(r.bit_position(), 16);
        assert_eq!(r.read_bits(1), None);
    }

    #[test]
    fn read_bits_zero_width_is_a_noop() {
        let data = [0xA5u8];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read_bits(0), Some(0));
        assert_eq!(r.bit_position(), 0);
        assert_eq!(r.read_u8(), Some(0xA5));
    }
}
