//! Spec-exact CAVLC variable-length-code tables (ITU-T H.264 §9.2).
//!
//! These tables replace the approximated ones that previously lived inline in
//! [`crate::slice`]. Every numeric value here is a fact taken from the H.264
//! specification:
//!
//! * `coeff_token` — Table 9-5 (four `nC`-selected VLC tables) and the
//!   chroma-DC 2×2 table.
//! * `total_zeros` — Tables 9-7 / 9-8 (4×4 luma) and Table 9-9 (chroma-DC 2×2).
//! * `run_before` — Table 9-10.
//!
//! Each VLC is stored as parallel `len` (codeword bit length) and `bits`
//! (codeword value) arrays, indexed by the *decoded* symbol. Decoding therefore
//! reads bits incrementally and finds the entry whose `(len, bits)` matches —
//! this is unambiguous because the H.264 VLCs are prefix codes. The numbers were
//! cross-checked against FFmpeg's `h264data.h` (which encodes the same spec
//! tables); the code layout here is original.

use crate::bitreader::BitReader;

/// Result of a CAVLC table lookup failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CavlcVlcError;

/// Generic longest-match decoder over parallel `(len, bits)` arrays.
///
/// `entries` yields `(len, bits, symbol)`. Reads up to the maximum `len` present
/// and returns the symbol of the entry whose codeword matches the bits read so
/// far. Entries with `len == 0` are treated as "not present" and skipped.
fn decode_vlc(
    r: &mut BitReader,
    lens: &[u8],
    bits: &[u16],
) -> Result<usize, CavlcVlcError> {
    debug_assert_eq!(lens.len(), bits.len());
    let max_len = lens.iter().copied().max().unwrap_or(0);
    let mut code: u32 = 0;
    for cur_len in 1..=max_len {
        let bit = r.read_bit().ok_or(CavlcVlcError)?;
        code = (code << 1) | bit as u32;
        for (idx, (&l, &b)) in lens.iter().zip(bits.iter()).enumerate() {
            if l == cur_len && b as u32 == code {
                return Ok(idx);
            }
        }
    }
    Err(CavlcVlcError)
}

// ────────────────────────────────────────────────────────────────────────────
// coeff_token (Table 9-5)
//
// Indexed as [vlc_table][trailing_ones + 4 * total_coeff]. `vlc_table` selects
// by nC:  0 -> 0<=nC<2, 1 -> 2<=nC<4, 2 -> 4<=nC<8, 3 -> nC>=8 (fixed 6-bit).
// A `len` of 0 marks an impossible (TotalCoeff, TrailingOnes) combination.
// ────────────────────────────────────────────────────────────────────────────

#[rustfmt::skip]
const COEFF_TOKEN_LEN: [[u8; 4 * 17]; 4] = [
    [
         1, 0, 0, 0,
         6, 2, 0, 0,     8, 6, 3, 0,     9, 8, 7, 5,    10, 9, 8, 6,
        11,10, 9, 7,    13,11,10, 8,    13,13,11, 9,    13,13,13,10,
        14,14,13,11,    14,14,14,13,    15,15,14,14,    15,15,15,14,
        16,15,15,15,    16,16,16,15,    16,16,16,16,    16,16,16,16,
    ],
    [
         2, 0, 0, 0,
         6, 2, 0, 0,     6, 5, 3, 0,     7, 6, 6, 4,     8, 6, 6, 4,
         8, 7, 7, 5,     9, 8, 8, 6,    11, 9, 9, 6,    11,11,11, 7,
        12,11,11, 9,    12,12,12,11,    12,12,12,11,    13,13,13,12,
        13,13,13,13,    13,14,13,13,    14,14,14,13,    14,14,14,14,
    ],
    [
         4, 0, 0, 0,
         6, 4, 0, 0,     6, 5, 4, 0,     6, 5, 5, 4,     7, 5, 5, 4,
         7, 5, 5, 4,     7, 6, 6, 4,     7, 6, 6, 4,     8, 7, 7, 5,
         8, 8, 7, 6,     9, 8, 8, 7,     9, 9, 8, 8,     9, 9, 9, 8,
        10, 9, 9, 9,    10,10,10,10,    10,10,10,10,    10,10,10,10,
    ],
    [
         6, 0, 0, 0,
         6, 6, 0, 0,     6, 6, 6, 0,     6, 6, 6, 6,     6, 6, 6, 6,
         6, 6, 6, 6,     6, 6, 6, 6,     6, 6, 6, 6,     6, 6, 6, 6,
         6, 6, 6, 6,     6, 6, 6, 6,     6, 6, 6, 6,     6, 6, 6, 6,
         6, 6, 6, 6,     6, 6, 6, 6,     6, 6, 6, 6,     6, 6, 6, 6,
    ],
];

#[rustfmt::skip]
const COEFF_TOKEN_BITS: [[u16; 4 * 17]; 4] = [
    [
         1, 0, 0, 0,
         5, 1, 0, 0,     7, 4, 1, 0,     7, 6, 5, 3,     7, 6, 5, 3,
         7, 6, 5, 4,    15, 6, 5, 4,    11,14, 5, 4,     8,10,13, 4,
        15,14, 9, 4,    11,10,13,12,    15,14, 9,12,    11,10,13, 8,
        15, 1, 9,12,    11,14,13, 8,     7,10, 9,12,     4, 6, 5, 8,
    ],
    [
         3, 0, 0, 0,
        11, 2, 0, 0,     7, 7, 3, 0,     7,10, 9, 5,     7, 6, 5, 4,
         4, 6, 5, 6,     7, 6, 5, 8,    15, 6, 5, 4,    11,14,13, 4,
        15,10, 9, 4,    11,14,13,12,     8,10, 9, 8,    15,14,13,12,
        11,10, 9,12,     7,11, 6, 8,     9, 8,10, 1,     7, 6, 5, 4,
    ],
    [
        15, 0, 0, 0,
        15,14, 0, 0,    11,15,13, 0,     8,12,14,12,    15,10,11,11,
        11, 8, 9,10,     9,14,13, 9,     8,10, 9, 8,    15,14,13,13,
        11,14,10,12,    15,10,13,12,    11,14, 9,12,     8,10,13, 8,
        13, 7, 9,12,     9,12,11,10,     5, 8, 7, 6,     1, 4, 3, 2,
    ],
    [
         3, 0, 0, 0,
         0, 1, 0, 0,     4, 5, 6, 0,     8, 9,10,11,    12,13,14,15,
        16,17,18,19,    20,21,22,23,    24,25,26,27,    28,29,30,31,
        32,33,34,35,    36,37,38,39,    40,41,42,43,    44,45,46,47,
        48,49,50,51,    52,53,54,55,    56,57,58,59,    60,61,62,63,
    ],
];

/// chroma-DC coeff_token for 4:2:0 (nC == -1), Table 9-5 last column.
/// Indexed [trailing_ones + 4*total_coeff], total_coeff 0..=4, t1 0..=3.
#[rustfmt::skip]
const CHROMA_DC_COEFF_TOKEN_LEN: [u8; 4 * 5] = [
    2, 0, 0, 0,
    6, 1, 0, 0,
    6, 6, 3, 0,
    6, 7, 7, 6,
    6, 8, 8, 7,
];

#[rustfmt::skip]
const CHROMA_DC_COEFF_TOKEN_BITS: [u16; 4 * 5] = [
    1, 0, 0, 0,
    7, 1, 0, 0,
    4, 6, 1, 0,
    3, 3, 2, 5,
    2, 3, 2, 0,
];

/// Selects the coeff_token VLC table from `nC`.
///
/// * `nC == -1` -> chroma-DC 2×2 table.
/// * `0 <= nC < 2` -> table 0, `2 <= nC < 4` -> 1, `4 <= nC < 8` -> 2,
///   `nC >= 8` -> 3 (fixed 6-bit code).
///
/// Returns `(total_coeff, trailing_ones)`.
pub fn read_coeff_token(r: &mut BitReader, n_c: i32) -> Result<(u8, u8), CavlcVlcError> {
    if n_c == -1 {
        let idx = decode_vlc(r, &CHROMA_DC_COEFF_TOKEN_LEN, &CHROMA_DC_COEFF_TOKEN_BITS)?;
        return Ok(((idx / 4) as u8, (idx % 4) as u8));
    }
    let table = if n_c < 2 {
        0
    } else if n_c < 4 {
        1
    } else if n_c < 8 {
        2
    } else {
        3
    };
    let idx = decode_vlc(r, &COEFF_TOKEN_LEN[table], &COEFF_TOKEN_BITS[table])?;
    Ok(((idx / 4) as u8, (idx % 4) as u8))
}

// ────────────────────────────────────────────────────────────────────────────
// total_zeros (Tables 9-7 / 9-8 for 4×4, Table 9-9 for chroma-DC 2×2)
//
// TOTAL_ZEROS_LEN[tzVlcIndex][total_zeros] where tzVlcIndex = TotalCoeff - 1.
// ────────────────────────────────────────────────────────────────────────────

#[rustfmt::skip]
const TOTAL_ZEROS_LEN: [[u8; 16]; 15] = [
    [1,3,3,4,4,5,5,6,6,7,7,8,8,9,9,9],
    [3,3,3,3,3,4,4,4,4,5,5,6,6,6,6,0],
    [4,3,3,3,4,4,3,3,4,5,5,6,5,6,0,0],
    [5,3,4,4,3,3,3,4,3,4,5,5,5,0,0,0],
    [4,4,4,3,3,3,3,3,4,5,4,5,0,0,0,0],
    [6,5,3,3,3,3,3,3,4,3,6,0,0,0,0,0],
    [6,5,3,3,3,2,3,4,3,6,0,0,0,0,0,0],
    [6,4,5,3,2,2,3,3,6,0,0,0,0,0,0,0],
    [6,6,4,2,2,3,2,5,0,0,0,0,0,0,0,0],
    [5,5,3,2,2,2,4,0,0,0,0,0,0,0,0,0],
    [4,4,3,3,1,3,0,0,0,0,0,0,0,0,0,0],
    [4,4,2,1,3,0,0,0,0,0,0,0,0,0,0,0],
    [3,3,1,2,0,0,0,0,0,0,0,0,0,0,0,0],
    [2,2,1,0,0,0,0,0,0,0,0,0,0,0,0,0],
    [1,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
];

#[rustfmt::skip]
const TOTAL_ZEROS_BITS: [[u16; 16]; 15] = [
    [1,3,2,3,2,3,2,3,2,3,2,3,2,3,2,1],
    [7,6,5,4,3,5,4,3,2,3,2,3,2,1,0,0],
    [5,7,6,5,4,3,4,3,2,3,2,1,1,0,0,0],
    [3,7,5,4,6,5,4,3,3,2,2,1,0,0,0,0],
    [5,4,3,7,6,5,4,3,2,1,1,0,0,0,0,0],
    [1,1,7,6,5,4,3,2,1,1,0,0,0,0,0,0],
    [1,1,5,4,3,3,2,1,1,0,0,0,0,0,0,0],
    [1,1,1,3,3,2,2,1,0,0,0,0,0,0,0,0],
    [1,0,1,3,2,1,1,1,0,0,0,0,0,0,0,0],
    [1,0,1,3,2,1,1,0,0,0,0,0,0,0,0,0],
    [0,1,1,2,1,3,0,0,0,0,0,0,0,0,0,0],
    [0,1,1,1,1,0,0,0,0,0,0,0,0,0,0,0],
    [0,1,1,1,0,0,0,0,0,0,0,0,0,0,0,0],
    [0,1,1,0,0,0,0,0,0,0,0,0,0,0,0,0],
    [0,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
];

/// Read `total_zeros` for a 4×4 block. `total_coeff` in 1..=15.
pub fn read_total_zeros_4x4(r: &mut BitReader, total_coeff: u8) -> Result<u8, CavlcVlcError> {
    if total_coeff == 0 || total_coeff >= 16 {
        return Ok(0);
    }
    let vlc = (total_coeff - 1) as usize;
    let idx = decode_vlc(r, &TOTAL_ZEROS_LEN[vlc], &TOTAL_ZEROS_BITS[vlc])?;
    Ok(idx as u8)
}

/// chroma-DC (2×2) total_zeros — Table 9-9. `total_coeff` in 1..=3.
#[rustfmt::skip]
const CHROMA_DC_TOTAL_ZEROS_LEN: [[u8; 4]; 3] = [
    [1,2,3,3],
    [1,2,2,0],
    [1,1,0,0],
];

#[rustfmt::skip]
const CHROMA_DC_TOTAL_ZEROS_BITS: [[u16; 4]; 3] = [
    [1,1,1,0],
    [1,1,0,0],
    [1,0,0,0],
];

/// Read `total_zeros` for a chroma-DC 2×2 block. `total_coeff` in 1..=3.
pub fn read_total_zeros_chroma_dc(
    r: &mut BitReader,
    total_coeff: u8,
) -> Result<u8, CavlcVlcError> {
    if total_coeff == 0 || total_coeff >= 4 {
        return Ok(0);
    }
    let vlc = (total_coeff - 1) as usize;
    let idx = decode_vlc(r, &CHROMA_DC_TOTAL_ZEROS_LEN[vlc], &CHROMA_DC_TOTAL_ZEROS_BITS[vlc])?;
    Ok(idx as u8)
}

// ────────────────────────────────────────────────────────────────────────────
// run_before (Table 9-10)
//
// RUN_LEN[min(zerosLeft, 7) - 1][run_before].
// ────────────────────────────────────────────────────────────────────────────

#[rustfmt::skip]
const RUN_LEN: [[u8; 15]; 7] = [
    [1,1,0,0,0,0,0,0,0,0,0,0,0,0,0],
    [1,2,2,0,0,0,0,0,0,0,0,0,0,0,0],
    [2,2,2,2,0,0,0,0,0,0,0,0,0,0,0],
    [2,2,2,3,3,0,0,0,0,0,0,0,0,0,0],
    [2,2,3,3,3,3,0,0,0,0,0,0,0,0,0],
    [2,3,3,3,3,3,3,0,0,0,0,0,0,0,0],
    [3,3,3,3,3,3,3,4,5,6,7,8,9,10,11],
];

#[rustfmt::skip]
const RUN_BITS: [[u16; 15]; 7] = [
    [1,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
    [1,1,0,0,0,0,0,0,0,0,0,0,0,0,0],
    [3,2,1,0,0,0,0,0,0,0,0,0,0,0,0],
    [3,2,1,1,0,0,0,0,0,0,0,0,0,0,0],
    [3,2,3,2,1,0,0,0,0,0,0,0,0,0,0],
    [3,0,1,3,2,5,4,0,0,0,0,0,0,0,0],
    [7,6,5,4,3,2,1,1,1,1,1,1,1,1,1],
];

/// Read `run_before` given the number of zeros still to place (`zeros_left`).
pub fn read_run_before(r: &mut BitReader, zeros_left: u8) -> Result<u8, CavlcVlcError> {
    if zeros_left == 0 {
        return Ok(0);
    }
    let vlc = (zeros_left.min(7) - 1) as usize;
    let idx = decode_vlc(r, &RUN_LEN[vlc], &RUN_BITS[vlc])?;
    Ok(idx as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every codeword in every `(len, bits)` table must be a valid prefix code:
    /// decoding the exact bit pattern of a symbol must recover that symbol.
    fn roundtrip(lens: &[u8], bits: &[u16]) {
        for (sym, (&l, &b)) in lens.iter().zip(bits.iter()).enumerate() {
            if l == 0 {
                continue;
            }
            // Emit the codeword MSB-first into a byte buffer.
            let mut buf = 0u32;
            buf |= (b as u32) << (32 - l);
            let bytes = buf.to_be_bytes();
            let mut r = BitReader::new(&bytes);
            let got = decode_vlc(&mut r, lens, bits).expect("decode");
            assert_eq!(got, sym, "len={l} bits={b:b}");
        }
    }

    #[test]
    fn coeff_token_tables_roundtrip() {
        for t in 0..4 {
            roundtrip(&COEFF_TOKEN_LEN[t], &COEFF_TOKEN_BITS[t]);
        }
        roundtrip(&CHROMA_DC_COEFF_TOKEN_LEN, &CHROMA_DC_COEFF_TOKEN_BITS);
    }

    #[test]
    fn total_zeros_tables_roundtrip() {
        for t in 0..15 {
            roundtrip(&TOTAL_ZEROS_LEN[t], &TOTAL_ZEROS_BITS[t]);
        }
        for t in 0..3 {
            roundtrip(&CHROMA_DC_TOTAL_ZEROS_LEN[t], &CHROMA_DC_TOTAL_ZEROS_BITS[t]);
        }
    }

    #[test]
    fn run_before_tables_roundtrip() {
        for t in 0..7 {
            roundtrip(&RUN_LEN[t], &RUN_BITS[t]);
        }
    }

    #[test]
    fn coeff_token_known_values() {
        // nC=0 table: TotalCoeff=0,T1=0 is the 1-bit code "1".
        let mut r = BitReader::new(&[0b1000_0000]);
        assert_eq!(read_coeff_token(&mut r, 0).unwrap(), (0, 0));
        // nC=0 table: TotalCoeff=1,T1=1 is "01".
        let mut r = BitReader::new(&[0b0100_0000]);
        assert_eq!(read_coeff_token(&mut r, 0).unwrap(), (1, 1));
    }

    #[test]
    fn run_before_known_values() {
        // zeros_left=1: run_before 0 => "1", run_before 1 => "0".
        let mut r = BitReader::new(&[0b1000_0000]);
        assert_eq!(read_run_before(&mut r, 1).unwrap(), 0);
        let mut r = BitReader::new(&[0b0000_0000]);
        assert_eq!(read_run_before(&mut r, 1).unwrap(), 1);
    }
}
