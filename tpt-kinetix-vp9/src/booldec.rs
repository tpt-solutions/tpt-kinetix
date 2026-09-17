//! The VP9 bool (range) decoder (§7.2 of the VP9 bitstream specification).
//!
//! Used for the compressed frame header and every tile's data. Implements the
//! spec's arithmetic decoder: an 8-bit range coder with a 16-bit value window
//! and per-symbol `prob`ability in `1..=255` (higher = more likely zero).

use tpt_kinetix_core::error::KinetixError;

/// Bool decoder over a byte slice.
#[derive(Debug)]
pub struct BoolDecoder<'a> {
    data: &'a [u8],
    /// Index of the next *bit* to shift into `value` (the 16-bit preload
    /// advances this to 16).
    next_bit: usize,
    /// 16-bit window into the bitstream (invariant: `value < range << 8`).
    value: u16,
    range: u16,
}

impl<'a> BoolDecoder<'a> {
    /// Initialize with the spec's `BD_INIT`: preload 16 bits, range 255.
    ///
    /// Returns an error if the slice is shorter than two bytes (a valid VP9
    /// bool-coded partition always carries at least a marker bit plus
    /// termination padding).
    pub fn new(data: &'a [u8]) -> Result<Self, KinetixError> {
        if data.len() < 2 {
            return Err(KinetixError::Parse(
                "vp9: bool-coded partition shorter than 2 bytes".into(),
            ));
        }
        Ok(Self {
            data,
            next_bit: 16,
            value: u16::from_be_bytes([data[0], data[1]]),
            range: 255,
        })
    }

    /// Read one bool with the given probability. Returns `false` for the
    /// sub-interval of relative size `prob/256` (the "0" branch).
    #[inline]
    pub fn read_bool(&mut self, prob: u8) -> bool {
        let split = 1 + (((u32::from(self.range) - 1) * u32::from(prob)) >> 8); // 1..=255
        let bigsplit = (split << 8) as u16;
        let bit = self.value >= bigsplit;
        if std::env::var("TPT_VP9_READS").is_ok() {
            eprintln!(
                "READ p={prob} -> {} (val={:#06x} range={} consumed={})",
                u8::from(bit), self.value, self.range, self.next_bit
            );
        }
        if bit {
            self.range -= split as u16;
            self.value -= bigsplit;
        } else {
            self.range = split as u16;
        }
        self.normalize();
        bit
    }

    /// Like [`Self::read_bool`] but returns a `bool`-of-"was it the 1 branch"
    /// as `u32` (0/1) — convenience mirroring the spec pseudocode.
    #[inline]
    pub fn read_bool_u32(&mut self, prob: u8) -> u32 {
        u32::from(self.read_bool(prob))
    }

    #[inline]
    fn normalize(&mut self) {
        while self.range < 128 {
            self.range <<= 1;
            let byte_idx = self.next_bit / 8;
            let bit = if byte_idx < self.data.len() {
                (self.data[byte_idx] >> (7 - (self.next_bit % 8))) & 1
            } else {
                0
            };
            self.next_bit += 1;
            self.value = (self.value << 1) | u16::from(bit);
        }
    }

    /// Bits consumed so far (for debugging).
    pub fn bits_consumed(&self) -> usize {
        self.next_bit
    }

    /// `L(n)` in the spec: n literal bits, MSB first, each with prob 128.
    pub fn read_literal(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | u32::from(self.read_bool(128));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canonical VP8/VP9 bool encoder (RFC 6386 §7.3), test-only reference
    /// used to round-trip symbols through [`BoolDecoder`].
    struct BoolEncoder {
        out: Vec<u8>,
        range: u32,
        bottom: u64,
        bit_count: i32,
    }

    impl BoolEncoder {
        fn new() -> Self {
            Self {
                out: Vec::new(),
                range: 255,
                bottom: 0,
                bit_count: 24,
            }
        }

        fn write_bool(&mut self, prob: u8, bit: bool) {
            let split = 1 + (((self.range - 1) * u32::from(prob)) >> 8);
            if bit {
                self.bottom += u64::from(split);
                self.range -= split;
            } else {
                self.range = split;
            }
            while self.range < 128 {
                self.bottom <<= 1;
                self.range <<= 1;
                self.emit_when_full();
            }
        }

        fn emit_when_full(&mut self) {
            self.bit_count -= 1;
            if self.bit_count == 0 {
                if self.bottom & (1 << 32) != 0 {
                    // carry: propagate into the already-emitted bytes
                    let mut q = self.out.len();
                    loop {
                        assert!(q > 0, "carry past start of partition");
                        q -= 1;
                        if self.out[q] == 255 {
                            self.out[q] = 0;
                        } else {
                            self.out[q] += 1;
                            break;
                        }
                    }
                }
                self.out.push((self.bottom >> 24) as u8);
                self.bottom &= (1 << 24) - 1;
                self.bit_count = 8;
            }
        }

        /// `flush_bool_encoder`: 32 zero shifts to push out the interval.
        fn flush(mut self) -> Vec<u8> {
            for _ in 0..32 {
                self.bottom <<= 1;
                self.emit_when_full();
            }
            self.out
        }
    }

    /// Deterministic LCG so failures reproduce.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) as u32
        }
    }

    #[test]
    fn round_trips_random_bools_and_literals() {
        let mut rng = Lcg(0x5EED_1234);
        // (prob, bit) symbol stream plus occasional literals
        let mut symbols: Vec<(u8, bool)> = Vec::new();
        let mut literals: Vec<(u32, u32)> = Vec::new(); // (n, value) position-tagged
        let mut script: Vec<u32> = Vec::new();
        for _ in 0..2000 {
            script.push(rng.next() % 3);
        }
        let mut enc = BoolEncoder::new();
        for op in script {
            match op {
                0 | 1 => {
                    let prob = (rng.next() % 255 + 1) as u8; // 1..=255
                    let bit = rng.next() & 1 != 0;
                    symbols.push((prob, bit));
                    enc.write_bool(prob, bit);
                }
                _ => {
                    let n = rng.next() % 8 + 1;
                    let v = rng.next() & ((1 << n) - 1);
                    literals.push((n, v));
                    for i in (0..n).rev() {
                        let bit = (v >> i) & 1 != 0;
                        symbols.push((128, bit));
                        enc.write_bool(128, bit);
                    }
                }
            }
        }
        let data = enc.flush();
        let mut dec = BoolDecoder::new(&data).unwrap();
        for (idx, (prob, bit)) in symbols.iter().enumerate() {
            assert_eq!(
                dec.read_bool(*prob),
                *bit,
                "mismatch at symbol {idx} prob={prob}"
            );
        }
    }

    #[test]
    fn first_bool_prob128_false_when_top_byte_below_128() {
        // split(128) = 128, bigsplit = 0x8000; any first byte < 128 reads 0.
        let mut d = BoolDecoder::new(&[127, 255]).unwrap();
        assert!(!d.read_bool(128));
    }

    #[test]
    fn too_short_errors() {
        assert!(BoolDecoder::new(&[0]).is_err());
        assert!(BoolDecoder::new(&[]).is_err());
    }
}
