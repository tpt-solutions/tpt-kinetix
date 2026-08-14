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
