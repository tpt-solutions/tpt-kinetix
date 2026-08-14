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
use crate::syntax::IcsInfo;

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
/// `band_type[g * max_sfb + sfb]` (length `num_groups * max_sfb`) must already
/// be populated from the section data. Returns the decoded scalefactor for each
/// (group, sfb) — for normal bands this is the linear scale factor; for
/// intensity bands the intensity position; for PNS bands the noise energy.
pub fn decode_scalefactors(
    reader: &mut BitReader,
    ics: &IcsInfo,
    band_type: &[u8],
) -> Result<Vec<i32>, crate::syntax::AacParseError> {
    let num_groups = ics.num_window_groups();
    let max_sfb = ics.max_sfb as usize;
    let mut sf = vec![0i32; num_groups * max_sfb];

    for g in 0..num_groups {
        // `chain_active` is true once we have seen an intensity/noise band in
        // this group, so the next intensity/noise band continues the DPCM
        // chain (predictor = previous value); the first one resets to 0.
        let mut chain_active = false;
        let mut prev = 0i32;
        let mut prev_was_is = false;

        for sfb in 0..max_sfb {
            let idx = g * max_sfb + sfb;
            let bt = band_type[idx];
            if bt == ZERO_HCB {
                sf[idx] = 0;
                continue;
            }
            let hcod = decode_scalefactor(reader).ok_or(crate::syntax::AacParseError::UnexpectedEof)?;

            let predictor = if is_intensity(bt) || is_noise(bt) {
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
            sf[idx] = val;
            prev = val;
            if is_intensity(bt) || is_noise(bt) {
                chain_active = true;
            }
            prev_was_is = is_intensity(bt) || is_noise(bt);
            let _ = prev_was_is;
        }
    }
    Ok(sf)
}
