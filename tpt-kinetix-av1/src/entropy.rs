//! AV1 symbol (arithmetic) decoder (AV1 spec §8.2, "Parsing process for
//! symbol decoder").
//!
//! This is the multi-symbol adaptive range decoder that all CDF-coded (`S()`)
//! syntax elements in a real AV1 tile go through. It is a distinct entropy
//! coding engine from H.264 CABAC (`tpt-kinetix-h264`) and from the ad hoc
//! `BitReader`-based exp-golomb-like scheme `reconstruct::decode_tile_group`
//! used to read coefficients before AV1 Phase B — that scheme could not
//! decode a real AV1 bitstream, since real encoders never produce it.
//! [`crate::coeff`] now drives this decoder with the spec `coeffs()` syntax,
//! and `decode_tile_group` calls into that.
//!
//! Implements, verbatim per spec:
//! * §8.2.2 `init_symbol` — decoder initialization
//! * §8.2.3 `read_bool` — the fixed-probability (p=1/2) boolean special case
//! * §8.2.5 `read_literal` — `n` raw-ish bits built from `read_bool`
//! * §8.2.6 `read_symbol` — the general N-ary decode + CDF adaptation/update
//!
//! **Not implemented here**: §8.2.4 `exit_symbol` (trailing-bit validation
//! and the Tile*→Saved* CDF snapshot copy), since that is tied to per-tile
//! bookkeeping (`context_update_tile_id`, `disable_frame_end_update_cdf`,
//! the full set of named CDF arrays) that arrives with the real tile/
//! partition syntax in AV1 Phase C.
//!
//! CDF arrays use the spec's representation directly: an array of length
//! `N + 1` for an `N`-symbol alphabet, where `cdf[N - 1] == 1 << 15` (32768,
//! the fixed top of the probability space) and `cdf[N]` is an adaptation
//! counter (saturating at 32) rather than a probability.

/// Number of bits to reduce CDF precision during arithmetic coding (spec
/// `EC_PROB_SHIFT`).
const EC_PROB_SHIFT: u32 = 6;

/// Minimum probability assigned to each symbol during arithmetic coding
/// (spec `EC_MIN_PROB`).
const EC_MIN_PROB: u32 = 4;

/// `FloorLog2(x)`: floor of the base-2 logarithm of `x` (spec §4.7).
///
/// `x` must be >= 1 (guaranteed by all call sites below: `SymbolRange` is
/// always >= 1, and `N` — the symbol count — is always >= 2).
#[inline]
fn floor_log2(mut x: u32) -> u32 {
    debug_assert!(x >= 1);
    let mut s = 0u32;
    while x != 0 {
        x >>= 1;
        s += 1;
    }
    s - 1
}

/// The AV1 symbol (arithmetic) decoder.
///
/// Reads MSB-first from a byte-aligned buffer, per §8.2.2. Bits read past
/// the end of `data` are treated as zero padding, matching the spec's
/// `SymbolMaxBits`-going-negative behavior.
pub struct SymbolDecoder<'a> {
    data: &'a [u8],
    bit_pos: usize,
    symbol_value: u32,
    symbol_range: u32,
    symbol_max_bits: i64,
}

impl<'a> SymbolDecoder<'a> {
    /// `init_symbol(sz)` (§8.2.2), with `sz = data.len()`.
    pub fn new(data: &'a [u8]) -> Self {
        let mut dec = SymbolDecoder {
            data,
            bit_pos: 0,
            symbol_value: 0,
            symbol_range: 1 << 15,
            symbol_max_bits: 0,
        };
        let num_bits = ((data.len() * 8).min(15)) as u32;
        let buf = dec.f(num_bits);
        let padded_buf = buf << (15 - num_bits);
        dec.symbol_value = ((1u32 << 15) - 1) ^ padded_buf;
        dec.symbol_range = 1 << 15;
        dec.symbol_max_bits = 8 * data.len() as i64 - 15;
        dec
    }

    /// Raw bitstream bit read, MSB-first, byte-aligned start. Returns 0 for
    /// positions past the end of `data` (spec §8.2.2 padding behavior).
    fn read_bit(&mut self) -> u32 {
        let byte_idx = self.bit_pos / 8;
        let bit = if byte_idx < self.data.len() {
            let shift = 7 - (self.bit_pos % 8);
            (self.data[byte_idx] >> shift) & 1
        } else {
            0
        };
        self.bit_pos += 1;
        bit as u32
    }

    /// `f(n)` (§9 "Parsing process for f(n)"): read `n` bits MSB-first.
    fn f(&mut self, n: u32) -> u32 {
        let mut x = 0u32;
        for _ in 0..n {
            x = 2 * x + self.read_bit();
        }
        x
    }

    /// `read_symbol(cdf)` (§8.2.6).
    ///
    /// `cdf` has length `N + 1` for an `N`-symbol alphabet: `cdf[N - 1]`
    /// must equal `1 << 15`, and `cdf[N]` is the adaptation counter. Always
    /// performs the CDF update (equivalent to `disable_cdf_update == 0`);
    /// callers that need the non-adapting variant should pass a scratch
    /// copy of the table.
    ///
    /// Returns the decoded symbol index (`0..N`).
    pub fn read_symbol(&mut self, cdf: &mut [u16]) -> usize {
        let n = cdf.len() - 1;
        debug_assert!(n >= 2, "cdf must describe at least 2 symbols");
        debug_assert_eq!(cdf[n - 1], 1 << 15, "cdf[N-1] must be 32768");

        let mut cur = self.symbol_range;
        let mut prev;
        let mut symbol: usize = 0;
        loop {
            prev = cur;
            let f = (1u32 << 15) - cdf[symbol] as u32;
            cur = ((self.symbol_range >> 8) * (f >> EC_PROB_SHIFT)) >> (7 - EC_PROB_SHIFT);
            cur += EC_MIN_PROB * (n as u32 - symbol as u32 - 1);
            if self.symbol_value >= cur {
                break;
            }
            symbol += 1;
        }

        self.symbol_range = prev - cur;
        self.symbol_value -= cur;

        // Renormalization (§8.2.6, ordered steps 1-7).
        let bits = 15 - floor_log2(self.symbol_range);
        self.symbol_range <<= bits;
        let num_bits = bits.min(self.symbol_max_bits.max(0) as u32);
        let new_data = self.f(num_bits);
        let padded_data = new_data << (bits - num_bits);
        self.symbol_value = padded_data ^ (((self.symbol_value + 1) << bits) - 1);
        self.symbol_max_bits -= bits as i64;

        // CDF adaptation/update.
        let count = cdf[n] as u32;
        let rate = 3 + (count > 15) as u32 + (count > 31) as u32 + floor_log2(n as u32).min(2);
        let mut tmp: u32 = 0;
        for (i, slot) in cdf[..n - 1].iter_mut().enumerate() {
            if i == symbol {
                tmp = 1 << 15;
            }
            let c = *slot as u32;
            *slot = if tmp < c {
                (c - ((c - tmp) >> rate)) as u16
            } else {
                (c + ((tmp - c) >> rate)) as u16
            };
        }
        if cdf[n] < 32 {
            cdf[n] += 1;
        }

        symbol
    }

    /// `read_bool()` (§8.2.3): fixed p=1/2 boolean special case. The cdf is
    /// constructed fresh each call, so its post-decode adaptation is
    /// discarded, matching the spec note that implementations may skip it.
    pub fn read_bool(&mut self) -> bool {
        let mut cdf = [1u16 << 14, 1u16 << 15, 0u16];
        self.read_symbol(&mut cdf) == 1
    }

    /// `read_literal(n)` (§8.2.5): build an `n`-bit value from `read_bool`.
    pub fn read_literal(&mut self, n: u32) -> u32 {
        let mut x = 0u32;
        for _ in 0..n {
            x = 2 * x + self.read_bool() as u32;
        }
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // All golden vectors below were cross-checked against an independent
    // Python transcription of AV1 spec §8.2 (init_symbol/read_symbol/
    // read_bool/read_literal), run on synthetic byte buffers — the AV1
    // spec itself does not ship a worked numeric example for the symbol
    // decoder, so this differential check (two independent implementations
    // of the same spec text) stands in for it.

    #[test]
    fn floor_log2_matches_bit_length_minus_one() {
        assert_eq!(floor_log2(1), 0);
        assert_eq!(floor_log2(2), 1);
        assert_eq!(floor_log2(3), 1);
        assert_eq!(floor_log2(4), 2);
        assert_eq!(floor_log2(32768), 15);
        assert_eq!(floor_log2(65535), 15);
    }

    #[test]
    fn bool_all_zero_bytes_decodes_all_zero_symbols() {
        let mut dec = SymbolDecoder::new(&[0x00, 0x00, 0x00]);
        let syms: Vec<bool> = (0..8).map(|_| dec.read_bool()).collect();
        assert_eq!(syms, vec![false; 8]);
    }

    #[test]
    fn bool_all_one_bytes_decodes_all_one_symbols() {
        let mut dec = SymbolDecoder::new(&[0xFF, 0xFF, 0xFF]);
        let syms: Vec<bool> = (0..8).map(|_| dec.read_bool()).collect();
        assert_eq!(syms, vec![true; 8]);
    }

    #[test]
    fn read_literal_matches_reference_trace() {
        let mut dec = SymbolDecoder::new(&[0xA5, 0x3C, 0x81, 0xF0]);
        let lits: Vec<u32> = (0..4).map(|_| dec.read_literal(4)).collect();
        assert_eq!(lits, vec![10, 5, 4, 2]);
    }

    #[test]
    fn multi_symbol_uniform4_matches_reference_trace() {
        let mut cdf = [8192u16, 16384, 24576, 32768, 0];
        let mut dec = SymbolDecoder::new(&[0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC]);

        let expected_symbols = [0usize, 1, 0, 0, 1, 3];
        let expected_cdf_snapshots: [[u16; 5]; 6] = [
            [8960, 16896, 24832, 32768, 1],
            [8680, 17392, 25080, 32768, 2],
            [9432, 17872, 25320, 32768, 3],
            [10161, 18337, 25552, 32768, 4],
            [9844, 18787, 25777, 32768, 5],
            [9537, 18200, 24972, 32768, 6],
        ];

        for i in 0..6 {
            let symbol = dec.read_symbol(&mut cdf);
            assert_eq!(symbol, expected_symbols[i], "symbol mismatch at step {i}");
            assert_eq!(cdf, expected_cdf_snapshots[i], "cdf mismatch at step {i}");
        }
    }

    #[test]
    fn txb_skip_context0_matches_reference_trace() {
        // Default_Txb_Skip_Cdf[3][3][0] from the AV1 spec's default CDF
        // table (10.additional.tables.md).
        let mut cdf = [31671u16, 32768, 0];
        let mut dec = SymbolDecoder::new(&[0x7E, 0x91, 0x2D, 0x44, 0xC3, 0x0F, 0xAA, 0x55]);

        let expected_symbols = [0usize; 10];
        let expected_cdf_snapshots: [[u16; 3]; 10] = [
            [31739, 32768, 1],
            [31803, 32768, 2],
            [31863, 32768, 3],
            [31919, 32768, 4],
            [31972, 32768, 5],
            [32021, 32768, 6],
            [32067, 32768, 7],
            [32110, 32768, 8],
            [32151, 32768, 9],
            [32189, 32768, 10],
        ];

        for i in 0..10 {
            let symbol = dec.read_symbol(&mut cdf);
            assert_eq!(symbol, expected_symbols[i], "symbol mismatch at step {i}");
            assert_eq!(cdf, expected_cdf_snapshots[i], "cdf mismatch at step {i}");
        }
    }

    #[test]
    fn read_literal32_forces_multiple_renormalization_refills() {
        let mut dec = SymbolDecoder::new(&[0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF]);
        assert_eq!(dec.read_literal(32), 16_325_324);
    }

    #[test]
    fn short_buffer_reads_zero_padding_past_end() {
        let mut dec = SymbolDecoder::new(&[0x80]);
        let syms: Vec<bool> = (0..12).map(|_| dec.read_bool()).collect();
        let expected = [
            true, false, false, false, false, false, false, false, false, false, false, false,
        ];
        assert_eq!(syms, expected);
    }
}
