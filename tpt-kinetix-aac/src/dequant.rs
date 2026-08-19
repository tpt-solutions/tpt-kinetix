//! Spectral data decoding and dequantization (ISO/IEC 14496-3 §4.6.3).
//!
//! The AAC bitstream orders a channel as: `scalefactor_data`, `pulse_data`,
//! `tns_data`, then `spectral_data`. Scalefactors are therefore decoded first
//! (consuming `scalefactor_data`), followed by the pulse / TNS tools, and finally
//! the Huffman `spectral_data`. This module exposes those steps separately.
//!
//! The Huffman spectral data is decoded into a 1024-line frequency-ordered
//! coefficient buffer (already dequantized), expanding the short-window grouping
//! (the bitstream interleaves grouped short windows by scalefactor band).

use crate::bitreader::BitReader;
use crate::codebooks::decode_spectral_quad;
use crate::scalefactors::{is_intensity, is_noise, ZERO_HCB};
use crate::syntax::{AacParseError, IcsInfo, SectionData};

/// Per-band dequantization scale `2^((global_gain - 100 - sf) / 4)`.
pub fn dequant_scale(global_gain: u8, sf: i32) -> f32 {
    let q = global_gain as f64 - 100.0 - sf as f64;
    (2.0f64).powf(0.25 * q) as f32
}

/// Per-group base line offset into the 1024 buffer (short windows only; the sum
/// of previous groups' `group_len * 128`). Mirrors the spectral decode order.
pub fn group_base_offsets(ics: &IcsInfo) -> Vec<usize> {
    let num_groups = ics.num_window_groups();
    let mut gindex = vec![0usize; num_groups];
    let mut acc = 0usize;
    for (gi, g) in gindex.iter_mut().enumerate() {
        *g = acc;
        acc += ics.group_len(gi) * 128;
    }
    gindex
}

/// Inverse quantizer: `|q|^(4/3) · scale`, with the sign of `q`.
#[inline]
fn dequant_coeff(q: i32, scale: f32) -> f32 {
    if q == 0 {
        return 0.0;
    }
    let sign = if q < 0 { -1.0f32 } else { 1.0f32 };
    let mag = (q.unsigned_abs() as f64).powf(4.0 / 3.0) as f32;
    sign * mag * scale
}

/// Expand the section data into a per-`(group * max_sfb + sfb)` band codebook.
pub fn expand_band_types(sections: &SectionData, ics: &IcsInfo) -> Vec<u8> {
    let num_groups = ics.num_window_groups();
    let max_sfb = ics.max_sfb as usize;
    let mut band_type = vec![0u8; num_groups * max_sfb];
    for (gi, group) in sections.groups.iter().enumerate() {
        let mut sfb = 0usize;
        for sec in group {
            for _ in 0..sec.sect_len as usize {
                if sfb < max_sfb {
                    band_type[gi * max_sfb + sfb] = sec.sect_cb;
                }
                sfb += 1;
            }
        }
    }
    band_type
}

/// Decode the Huffman `spectral_data` for one channel into a 1024-line,
/// frequency-ordered, dequantized coefficient buffer.
#[allow(clippy::too_many_arguments)]
pub fn decode_spectral_data(
    reader: &mut BitReader,
    ics: &IcsInfo,
    sections: &SectionData,
    band_type: &[u8],
    scalefactor: &[i32],
    swb_long: &[u16],
    swb_short: &[u16],
    global_gain: u8,
) -> Result<[f32; 1024], AacParseError> {
    let num_groups = ics.num_window_groups();
    let max_sfb = ics.max_sfb as usize;
    let short = ics.window_sequence.is_eight_short();
    let swb = if short { swb_short } else { swb_long };

    // Per-(group, sfb) dequant scale.
    let mut scale = vec![0.0f32; num_groups * max_sfb];
    for gi in 0..num_groups {
        for (sfb, sc) in scale[gi * max_sfb..(gi + 1) * max_sfb]
            .iter_mut()
            .enumerate()
        {
            let idx = gi * max_sfb + sfb;
            let bt = band_type[idx];
            if bt == ZERO_HCB || is_noise(bt) || is_intensity(bt) {
                *sc = 0.0;
            } else {
                *sc = dequant_scale(global_gain, scalefactor[idx]);
            }
        }
    }

    // Per-group base line offset (short windows only).
    let mut gindex = vec![0usize; num_groups];
    {
        let mut acc = 0usize;
        for (gi, g) in gindex.iter_mut().enumerate() {
            *g = acc;
            acc += ics.group_len(gi) * 128;
        }
    }

    let mut coeffs = [0.0f32; 1024];

    for gi in 0..num_groups {
        let glen = ics.group_len(gi);
        let gbase = gindex[gi];
        let mut sfb = 0usize;
        for sec in &sections.groups[gi] {
            let sect_cb = sec.sect_cb;
            for _ in 0..sec.sect_len as usize {
                // Stop at the end of the scalefactor-band table (hostile-input
                // safety); the reference decoder ignores any remaining bands.
                if sfb + 1 >= swb.len() {
                    break;
                }
                let width = (swb[sfb + 1] - swb[sfb]) as usize;
                let active = sfb < max_sfb;
                let scale_val = if active {
                    scale[gi * max_sfb + sfb]
                } else {
                    0.0
                };
                for w_idx in 0..glen {
                    let base = gbase + w_idx * 128 + swb[sfb] as usize;
                    let mut bin = 0usize;
                    while bin < width {
                        if sect_cb == ZERO_HCB {
                            // all zero: nothing to decode
                        } else if is_intensity(sect_cb) || is_noise(sect_cb) {
                            // intensity / PNS bands carry no spectral coefficients
                        } else if (1..=4).contains(&sect_cb) {
                            let q = decode_spectral_quad(reader, sect_cb)
                                .ok_or(AacParseError::UnexpectedEof)?;
                            if active {
                                for b in 0..4 {
                                    coeffs[base + bin + b] = dequant_coeff(q[b], scale_val);
                                }
                            }
                        } else if (5..=11).contains(&sect_cb) {
                            let q1 = decode_spectral_quad(reader, sect_cb)
                                .ok_or(AacParseError::UnexpectedEof)?;
                            let q2 = decode_spectral_quad(reader, sect_cb)
                                .ok_or(AacParseError::UnexpectedEof)?;
                            if active {
                                let vals = [q1[2], q1[3], q2[2], q2[3]];
                                for b in 0..4 {
                                    coeffs[base + bin + b] = dequant_coeff(vals[b], scale_val);
                                }
                            }
                        } else {
                            return Err(AacParseError::BadSectionCodebook);
                        }
                        bin += 4;
                    }
                }
                sfb += 1;
            }
        }
    }

    Ok(coeffs)
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn dequant_scale_hand_computed() {
        // 2^((global_gain - 100 - sf) / 4)
        assert!((dequant_scale(100, 0) - 1.0).abs() < 1e-6);
        assert!((dequant_scale(104, 0) - 2.0).abs() < 1e-6);
        assert!((dequant_scale(100, 4) - 0.5).abs() < 1e-6);
        assert!((dequant_scale(96, 0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn dequant_coeff_hand_computed() {
        // |q|^(4/3) * scale
        assert_eq!(dequant_coeff(0, 1.0), 0.0);
        assert!((dequant_coeff(1, 1.0) - 1.0).abs() < 1e-6);
        // 8^(4/3) = (2^3)^(4/3) = 2^4 = 16
        assert!((dequant_coeff(8, 1.0) - 16.0).abs() < 1e-6);
        // sign preserved, scale applied
        assert!((dequant_coeff(-1, 2.0) + 2.0).abs() < 1e-6);
    }

    #[test]
    fn expand_band_types_fills_sections() {
        let ics = ics_long(4);
        let sections = SectionData {
            groups: vec![vec![
                Section {
                    sect_cb: 1,
                    sect_len: 2,
                },
                Section {
                    sect_cb: 0,
                    sect_len: 2,
                },
            ]],
        };
        let bt = expand_band_types(&sections, &ics);
        assert_eq!(bt, vec![1u8, 1, 0, 0]);
    }
}
