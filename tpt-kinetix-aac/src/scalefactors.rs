//! Scalefactor decoding (ISO/IEC 14496-3 §4.6.3.4).
//!
//! Scalefactors are differentially coded (DPCM) via three independent running
//! predictors that persist for the whole channel (not reset per window group):
//! one for regular bands, one for intensity `is_position`, one for PNS
//! `noise_energy` (whose first occurrence in the channel is a raw 9-bit field,
//! not Huffman-coded — the `noise_pcm_flag` special case). A band whose
//! section codebook is `ZERO_HCB` carries no scalefactor (it is implicitly 0
//! and does not advance any predictor). See [`decode_scalefactors`] for the
//! exact per-predictor sign/baseline conventions used here.

use crate::bitreader::BitReader;
use crate::codebooks::decode_scalefactor;
use crate::syntax::{AacParseError, IcsInfo, SectionData};

/// Section codebook for an all-zero band (no data).
pub const ZERO_HCB: u8 = 0;
/// Reserved codebook index.
pub const RESERVED_HCB: u8 = 12;
/// PNS (perceptual noise substitution) band.
pub const NOISE_HCB: u8 = 13;
/// Intensity stereo band (with the direction bit stored in the MSB).
pub const INTENSITY_HCB2: u8 = 14;
/// Intensity stereo band (direction given by the sign of the position).
pub const INTENSITY_HCB: u8 = 15;

/// Returns `true` if `sect_cb` is an intensity band (14 or 15).
#[inline]
pub fn is_intensity(cb: u8) -> bool {
    cb == INTENSITY_HCB || cb == INTENSITY_HCB2
}

/// Returns `true` if `sect_cb` is a PNS (noise) band.
#[inline]
pub fn is_noise(cb: u8) -> bool {
    cb == NOISE_HCB
}

/// Decode the DPCM scalefactor sequence for one channel.
///
/// `band_type` (length `num_groups * max_sfb`) is populated from the section
/// data; `sections` is the raw section structure and is iterated directly so
/// that every band the bitstream covers (including any past `max_sfb`, which the
/// reference decoder reads but ignores) consumes its scalefactor. Returns the
/// decoded scalefactor for each `(group, sfb)`.
///
/// Three independent DPCM predictors run for the whole channel (ISO 14496-3
/// §4.6.3.4), **not** reset per window group:
/// - regular (non-`ZERO_HCB`, non-intensity, non-noise) bands: a running value
///   that starts at 0 and represents *the negative offset from `global_gain`*
///   (chosen to match [`crate::dequant::dequant_scale`]'s
///   `2^((global_gain - 100 - sf) / 4)`, which folds `global_gain` in
///   separately) — each codeword `t` (`decode_scalefactor` returns `t - 60`)
///   updates it as `predictor - (t - 60)`, since the spec's absolute
///   `scale_factor += (t - 60)` and `global_gain - absolute` flips the sign.
/// - intensity bands: `is_position`, a literal running total (`+= t - 60`,
///   *no* `global_gain` baseline — [`crate::stereo::apply_stereo`] uses it
///   directly as `2^(-0.25 * is_position)`).
/// - noise (PNS) bands: `noise_energy`'s *first* occurrence in the whole
///   channel is a raw 9-bit field (`value - 256`), not Huffman-coded (the
///   `noise_pcm_flag` special case); subsequent noise bands DPCM off it the
///   same way regular bands do, so it is stored in the same
///   negative-offset-from-`global_gain` form.
pub fn decode_scalefactors(
    reader: &mut BitReader,
    ics: &IcsInfo,
    sections: &SectionData,
    band_type: &[u8],
) -> Result<Vec<i32>, AacParseError> {
    let num_groups = ics.num_window_groups();
    let max_sfb = ics.max_sfb as usize;
    let mut sf = vec![0i32; num_groups * max_sfb];

    let mut scale_factor = 0i32;
    let mut is_position = 0i32;
    let mut noise_energy = 0i32;
    let mut noise_pcm_flag = true;

    for g in 0..num_groups {
        let mut sfb = 0usize;
        for sec in &sections.groups[g] {
            let sect_cb = sec.sect_cb;
            for _ in 0..sec.sect_len as usize {
                if sfb >= max_sfb {
                    // A section's declared length pushed it past `max_sfb`; per
                    // ISO 14496-3 §4.4.3.1's `section_data()` pseudocode (bounded
                    // by `while (i < max_sfb)`) there is no scalefactor data in
                    // the bitstream past this point, so no bits are consumed.
                    sfb += 1;
                    continue;
                }
                let idx = g * max_sfb + sfb;
                let val = if sect_cb == ZERO_HCB {
                    // Zero book: no scalefactor is transmitted.
                    0
                } else if is_intensity(sect_cb) {
                    let hcod = decode_scalefactor(reader).ok_or(AacParseError::UnexpectedEof)?;
                    is_position += hcod;
                    is_position
                } else if is_noise(sect_cb) {
                    if noise_pcm_flag {
                        noise_pcm_flag = false;
                        let raw = reader.read_bits(9).ok_or(AacParseError::UnexpectedEof)? as i32;
                        // Absolute noise_energy = global_gain - 90 + (raw - 256);
                        // stored here as -(that - global_gain) = 90 - (raw - 256).
                        noise_energy = 90 - (raw - 256);
                    } else {
                        let hcod =
                            decode_scalefactor(reader).ok_or(AacParseError::UnexpectedEof)?;
                        noise_energy -= hcod;
                    }
                    noise_energy
                } else {
                    let hcod = decode_scalefactor(reader).ok_or(AacParseError::UnexpectedEof)?;
                    scale_factor -= hcod;
                    scale_factor
                };
                if sfb < max_sfb {
                    sf[idx] = val;
                }
                sfb += 1;
            }
        }
    }
    let _ = band_type;
    Ok(sf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codebooks::SCALEFACTOR_BOOK;
    use crate::dequant::expand_band_types;
    use crate::syntax::{IcsInfo, Section, SectionData, WindowSequence};

    fn ics_long(max_sfb: u8) -> IcsInfo {
        IcsInfo {
            window_sequence: WindowSequence::OnlyLong,
            window_shape: false,
            max_sfb,
            scale_factor_grouping: 0,
            predictor_data_present: false,
            predictor_reset_mode: None,
        }
    }

    /// Encode one scalefactor codeword (index `idx` in the ISO scalefactor book)
    /// into a raw MSB-first bit vector (0/1 per element). The caller packs the
    /// full bit vector into bytes once, so concatenating multiple codewords does
    /// not introduce spurious zero padding bits between them. `decode_scalefactor`
    /// returns `idx - 60`.
    fn encode_scalefactor_bits(idx: usize) -> Vec<u8> {
        let (code, len) = SCALEFACTOR_BOOK[idx];
        let mut bits = Vec::new();
        for b in 0..len {
            bits.push(((code >> (len - 1 - b)) & 1) as u8);
        }
        bits
    }

    /// Pack a 0/1 bit vector into MSB-first bytes.
    fn pack_bits(bits: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cur = 0u8;
        let mut n = 0u32;
        for &b in bits {
            cur = (cur << 1) | (b & 1);
            n += 1;
            if n == 8 {
                out.push(cur);
                cur = 0;
                n = 0;
            }
        }
        if n > 0 {
            out.push(cur << (8 - n));
        }
        out
    }

    #[test]
    fn decode_scalefactor_zero_and_signed() {
        // Single non-zero band; decode_scalefactor returns dpcm = idx - 60.
        // val = predictor(0) - dpcm.
        let ics = ics_long(1);
        let sections = SectionData {
            groups: vec![vec![Section {
                sect_cb: 1,
                sect_len: 1,
            }]],
        };
        let bt = expand_band_types(&sections, &ics);

        // idx 60 → dpcm 0 → val 0
        let bytes = pack_bits(&encode_scalefactor_bits(60));
        let mut r = BitReader::new(&bytes);
        assert_eq!(
            decode_scalefactors(&mut r, &ics, &sections, &bt, 100).unwrap(),
            vec![0]
        );

        // idx 61 → dpcm 1 → val -1
        let bytes = pack_bits(&encode_scalefactor_bits(61));
        let mut r = BitReader::new(&bytes);
        assert_eq!(
            decode_scalefactors(&mut r, &ics, &sections, &bt, 100).unwrap(),
            vec![-1]
        );

        // idx 59 → dpcm -1 → val 1
        let bytes = pack_bits(&encode_scalefactor_bits(59));
        let mut r = BitReader::new(&bytes);
        assert_eq!(
            decode_scalefactors(&mut r, &ics, &sections, &bt, 100).unwrap(),
            vec![1]
        );
    }

    #[test]
    fn zero_hcb_band_carries_no_scalefactor() {
        // A ZERO_HCB band contributes an implicit 0 scalefactor and consumes no bits.
        let ics = ics_long(1);
        let sections = SectionData {
            groups: vec![vec![Section {
                sect_cb: 0,
                sect_len: 1,
            }]],
        };
        let bt = expand_band_types(&sections, &ics);
        let mut r = BitReader::new(&[0u8; 4]);
        assert_eq!(
            decode_scalefactors(&mut r, &ics, &sections, &bt, 100).unwrap(),
            vec![0]
        );
    }

    #[test]
    fn scalefactor_dpcm_carries_across_bands() {
        // Two non-zero bands: first val 0 (dpcm 0), second val 2 (dpcm = 0 - 2 = -2
        // → idx 58). The predictor advances by the previous value.
        let ics = ics_long(2);
        let sections = SectionData {
            groups: vec![vec![Section {
                sect_cb: 1,
                sect_len: 2,
            }]],
        };
        let bt = expand_band_types(&sections, &ics);
        let mut bits = encode_scalefactor_bits(60); // band0: dpcm 0 → val 0
        bits.extend_from_slice(&encode_scalefactor_bits(58)); // band1: dpcm -2 → val 2
        let bytes = pack_bits(&bits);
        let mut r = BitReader::new(&bytes);
        assert_eq!(
            decode_scalefactors(&mut r, &ics, &sections, &bt, 100).unwrap(),
            vec![0, 2]
        );
    }
}
