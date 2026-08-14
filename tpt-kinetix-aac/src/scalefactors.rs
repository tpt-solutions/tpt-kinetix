//! Scalefactor decoding (ISO/IEC 14496-3 §4.6.3.4).
//!
//! Scalefactors are differentially coded (DPCM). The predictor is `0` at the
//! start of the stream and at the start of each window group for intensity /
//! noise bands; otherwise it is the previously decoded scalefactor. A band whose
//! section codebook is `ZERO_HCB` carries no scalefactor (it is implicitly 0 and
//! does not advance the predictor).
//!
//! For intensity / PNS bands the decoded value is not a linear scale factor but
//! an *intensity position* / *noise energy*; it is still DPCM-coded the same
//! way and simply interpreted differently by the stereo / PNS tools.

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
/// decoded scalefactor for each `(group, sfb)` — for normal bands this is the
/// linear scale factor; for intensity bands the intensity position; for PNS
/// bands the noise energy.
pub fn decode_scalefactors(
    reader: &mut BitReader,
    ics: &IcsInfo,
    sections: &SectionData,
    band_type: &[u8],
) -> Result<Vec<i32>, AacParseError> {
    let num_groups = ics.num_window_groups();
    let max_sfb = ics.max_sfb as usize;
    let mut sf = vec![0i32; num_groups * max_sfb];

    for g in 0..num_groups {
        // `chain_active` is true once we have seen an intensity/noise band in
        // this group, so the next intensity/noise band continues the DPCM
        // chain (predictor = previous value); the first one resets to 0.
        let mut chain_active = false;
        let mut prev = 0i32;
        let mut sfb = 0usize;
        for sec in &sections.groups[g] {
            let sect_cb = sec.sect_cb;
            for _ in 0..sec.sect_len as usize {
                let idx = g * max_sfb + sfb;
                if sect_cb == ZERO_HCB {
                    // Zero book: no scalefactor is transmitted.
                    if sfb < max_sfb {
                        sf[idx] = 0;
                    }
                } else {
                    let hcod = decode_scalefactor(reader).ok_or(AacParseError::UnexpectedEof)?;

                    let predictor = if is_intensity(sect_cb) || is_noise(sect_cb) {
                        if !chain_active {
                            // First intensity/noise band in the group: reset predictor.
                            0
                        } else {
                            prev
                        }
                    } else if g == 0 && sfb == 0 {
                        0
                    } else {
                        prev
                    };

                    let val = predictor - hcod;
                    if sfb < max_sfb {
                        sf[idx] = val;
                    }
                    prev = val;
                    if is_intensity(sect_cb) || is_noise(sect_cb) {
                        chain_active = true;
                    }
                }
                sfb += 1;
            }
        }
        let _ = band_type;
    }
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
    /// into a byte buffer. `decode_scalefactor` returns `idx - 60`.
    fn encode_scalefactor(idx: usize) -> Vec<u8> {
        let (code, len) = SCALEFACTOR_BOOK[idx];
        let mut bits = Vec::new();
        for b in 0..len {
            bits.push(((code >> (len - 1 - b)) & 1) as u8);
        }
        let mut out = Vec::new();
        let mut cur = 0u8;
        let mut n = 0u32;
        for &b in &bits {
            cur = (cur << 1) | b;
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
            groups: vec![vec![Section { sect_cb: 1, sect_len: 1 }]],
        };
        let bt = expand_band_types(&sections, &ics);

        // idx 60 → dpcm 0 → val 0
        let bytes = encode_scalefactor(60);
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_scalefactors(&mut r, &ics, &sections, &bt).unwrap(), vec![0]);

        // idx 61 → dpcm 1 → val -1
        let bytes = encode_scalefactor(61);
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_scalefactors(&mut r, &ics, &sections, &bt).unwrap(), vec![-1]);

        // idx 59 → dpcm -1 → val 1
        let bytes = encode_scalefactor(59);
        let mut r = BitReader::new(&bytes);
        assert_eq!(decode_scalefactors(&mut r, &ics, &sections, &bt).unwrap(), vec![1]);
    }

    #[test]
    fn zero_hcb_band_carries_no_scalefactor() {
        // A ZERO_HCB band contributes an implicit 0 scalefactor and consumes no bits.
        let ics = ics_long(1);
        let sections = SectionData {
            groups: vec![vec![Section { sect_cb: 0, sect_len: 1 }]],
        };
        let bt = expand_band_types(&sections, &ics);
        let mut r = BitReader::new(&[0u8; 4]);
        assert_eq!(decode_scalefactors(&mut r, &ics, &sections, &bt).unwrap(), vec![0]);
    }

    #[test]
    fn scalefactor_dpcm_carries_across_bands() {
        // Two non-zero bands: first val 0 (dpcm 0), second val 2 (dpcm = 0 - 2 = -2
        // → idx 58). The predictor advances by the previous value.
        let ics = ics_long(2);
        let sections = SectionData {
            groups: vec![vec![Section { sect_cb: 1, sect_len: 2 }]],
        };
        let bt = expand_band_types(&sections, &ics);
        let mut bits = encode_scalefactor(60); // band0: dpcm 0 → val 0
        bits.extend_from_slice(&encode_scalefactor(58)); // band1: dpcm -2 → val 2
        let mut r = BitReader::new(&bits);
        assert_eq!(
            decode_scalefactors(&mut r, &ics, &sections, &bt).unwrap(),
            vec![0, 2]
        );
    }
}
