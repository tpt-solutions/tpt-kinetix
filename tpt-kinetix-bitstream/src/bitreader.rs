//! Bit-level reader over a byte slice, MSB-first within each byte.
//!
//! Shared across the TPT Kinetix original codecs (lean / vision / realtime).
//! Previously each of those crates carried its own copy; this is the single
//! source of truth (see `docs/realtime-codec-design.md` DECISION 7).
//!
//! The reader keeps a lazily-refilled 64-bit window so the per-bit primitives
//! ([`Self::read_bit`]) pay no per-call bounds check: the window is refilled
//! once per 64 bits (or at end-of-buffer), and the absolute bit position is a
//! single `usize`. The window is a pure prefetch cache — `pos` is the only
//! source of truth, and every slow path invalidates the cache rather than
//! trying to keep it coherent.

/// Efficient bit-level reader over a byte slice.
pub struct BitReader<'a> {
    data: &'a [u8],
    /// Absolute bit index of the *next* bit to be returned.
    pos: usize,
    /// Data bits `pos..pos+valid` MSB-aligned: bit 63 is the next bit to emit.
    /// Bits beyond `valid` are zero. A pure cache of `data[pos..]`.
    window: u64,
    /// Number of valid bits in `window` (0..=64), or 0 when it needs a refill.
    valid: u8,
}

impl<'a> BitReader<'a> {
    /// Create a new `BitReader` positioned at the start of `data`.
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            window: 0,
            valid: 0,
        }
    }

    /// Total bits in the stream (`data.len() * 8`).
    #[inline]
    fn total_bits(&self) -> usize {
        self.data.len() << 3
    }

    /// Load the 64 bits starting at `pos` into the window (zero-padded at the
    /// end of the buffer), MSB-aligned so bit 63 is the next bit to emit.
    #[inline]
    fn refill(&mut self) {
        debug_assert!(self.valid == 0);
        let total = self.total_bits();
        if self.pos >= total {
            return;
        }
        let byte = self.pos >> 3;
        let mut window = [0u8; 8];
        let end = (byte + 8).min(self.data.len());
        window[..end - byte].copy_from_slice(&self.data[byte..end]);
        self.window = u64::from_be_bytes(window) << (self.pos & 7);
        // Real bits from `pos`: everything the buffer still holds from the
        // window's byte offset, minus the already-consumed head of that byte
        // (the shift dropped it). Capping at `total - pos` alone would count
        // the shifted-out top bits as data and hand back zeros mid-stream.
        let from_byte = (self.data.len() - byte) << 3;
        // Cap BEFORE subtracting the consumed head: the window only ever holds
        // 64 loaded bits, and the shift drops `pos & 7` of them.
        self.valid = (from_byte.min(64) - (self.pos & 7)) as u8;
    }

    /// Invalidate the window after any direct `pos` mutation.
    #[inline]
    fn invalidate(&mut self) {
        self.valid = 0;
    }

    /// Read the next single bit (0 or 1), or `None` if the stream is exhausted.
    ///
    /// The hot path is a load-shift-store off the prefilled window; the window
    /// refill (and the only bounds check) happens once per 64 bits instead of
    /// once per bit.
    #[inline]
    pub fn read_bit(&mut self) -> Option<u8> {
        if self.valid == 0 {
            self.refill();
            if self.valid == 0 {
                return None;
            }
        }
        let bit = (self.window >> 63) as u8;
        self.window <<= 1;
        self.valid -= 1;
        self.pos += 1;
        Some(bit)
    }

    /// Read up to 32 bits, MSB first. Returns `None` if the stream runs out.
    ///
    /// The common case (request fully covered by the window) is a shift and a
    /// store; a refill happens at most once per call. On exhaustion the bits
    /// that *were* available are consumed before `None` is returned — the same
    /// partial-consumption contract this reader has always had.
    pub fn read_bits(&mut self, n: u8) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        debug_assert!(n <= 32, "read_bits: n > 32 is not supported");
        let n = n as u32;

        if (self.valid as u32) < n {
            self.invalidate();
            self.refill();
            if (self.valid as u32) < n {
                // Exhausted mid-read: consume what the window holds, then fail,
                // matching the historical bit-at-a-time behaviour.
                let have = self.valid;
                let mut result = 0u32;
                for _ in 0..have {
                    result = (result << 1) | ((self.window >> 63) as u32);
                    self.window <<= 1;
                }
                self.pos += have as usize;
                self.valid = 0;
                return None;
            }
        }

        let value = (self.window >> (64 - n)) as u32;
        self.window <<= n;
        self.valid -= n as u8;
        self.pos += n as usize;
        Some(value)
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
        self.total_bits().saturating_sub(self.pos)
    }

    /// Absolute bit position from the start of the stream.
    #[inline]
    pub fn bit_position(&self) -> usize {
        self.pos
    }

    /// Align to the next byte boundary. No-op when already aligned. Used
    /// before entering an rANS-coded region, which is byte-framed (see
    /// [`crate::rans`]).
    pub fn byte_align(&mut self) {
        let aligned = (self.pos + 7) & !7;
        if aligned != self.pos {
            self.pos = aligned;
            self.invalidate();
        }
    }

    /// Returns `true` if the current position is byte-aligned.
    #[inline]
    pub fn is_aligned(&self) -> bool {
        self.pos & 7 == 0
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
        &self.data[self.pos >> 3..]
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

    /// `read_bit` must agree with the reference for every payload length and
    /// starting offset, including the exhausted tail (no consumption on
    /// failure).
    #[test]
    fn read_bit_matches_reference_and_stops_cleanly() {
        let mut state = 0xDEADBEEFu32;
        let data: Vec<u8> = (0..17)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state >> 24) as u8
            })
            .collect();
        let total = data.len() * 8;
        for pre in 0..9usize {
            let mut r = BitReader::new(&data);
            for _ in 0..pre {
                r.read_bit().expect("pre bits");
            }
            for i in pre..total {
                let byte = data[i / 8];
                let want = (byte >> (7 - (i % 8))) & 1;
                assert_eq!(r.read_bit(), Some(want), "bit {i}");
            }
            assert_eq!(r.read_bit(), None);
            assert_eq!(r.bit_position(), total);
        }
    }

    /// Mixed `read_bit` / `read_bits` traffic must stay coherent: the window
    /// cache and the absolute position must never disagree.
    #[test]
    fn mixed_reads_stay_coherent() {
        let mut state = 0x0F0F_0F0Fu32;
        let data: Vec<u8> = (0..33)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                (state >> 24) as u8
            })
            .collect();
        let mut r = BitReader::new(&data);
        let mut pos = 0usize;
        // A deterministic smear of widths: 1, 3, 7, 11, 16, 32, 1, ...
        let widths = [1u8, 3, 7, 11, 16, 32];
        let mut w = 0usize;
        while pos + 32 <= data.len() * 8 {
            let n = widths[w % widths.len()];
            w += 1;
            let got = if n == 1 {
                r.read_bit().map(u32::from)
            } else {
                r.read_bits(n)
            };
            let want = reference_read_bits(&data, pos, n);
            assert_eq!(got, want, "width {n} at {pos}");
            pos += n as usize;
        }
    }

    /// `remaining_bytes` / `byte_align` interplay after windowed reads: handing
    /// the tail to the rANS layer must see exactly the right bytes.
    #[test]
    fn remaining_bytes_after_windowed_reads() {
        let data: Vec<u8> = (0..24u8).collect();
        let mut r = BitReader::new(&data);
        // 19 bits = all of bytes 0..2 plus nothing of byte 3:
        // 0x00 (8) | 0x01 (8) | top 3 bits of 0x02 (000).
        assert_eq!(r.read_bits(19).unwrap(), (1 << 3));
        r.byte_align();
        assert_eq!(r.bit_position(), 24);
        assert_eq!(r.remaining_bytes(), &data[3..]);
    }
}
