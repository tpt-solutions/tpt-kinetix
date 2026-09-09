//! Pulse data (ISO/IEC 14496-3 §4.6.3.5).
//!
//! Pulse data adds a small number of large-magnitude spectral lines to an
//! otherwise sparse band (used for highly tonal signals). It is applied to the
//! dequantized spectrum before the IMDCT.

use crate::bitreader::BitReader;
use crate::dequant::dequant_scale;
use crate::scalefactors::{is_intensity, is_noise, ZERO_HCB};
use crate::syntax::AacParseError;

/// Parsed pulse-data for one channel.
#[derive(Debug, Clone, Default)]
pub struct PulseData {
    /// Scalefactor band the pulses start in (lower bands only).
    pub start_sfb: u8,
    /// Relative offsets (cumulative) to the affected spectral lines.
    pub offsets: Vec<u8>,
    /// Pulse amplitudes (`pulse_amp`, 4 bits, unsigned magnitude — used as-is,
    /// **not** incremented; ISO/IEC 14496-3 §4.6.3.5 / ffmpeg `decode_pulses`).
    pub amps: Vec<f32>,
}

/// Parse `pulse_data()` (called only when `pulse_data_present` was set).
///
/// `number_pulse` is a **2-bit** field (ISO 14496-3 Table 4.61): the loop runs
/// `number_pulse + 1` times, so 1..=4 pulses.
pub fn parse_pulse(reader: &mut BitReader) -> Result<PulseData, AacParseError> {
    let np = reader.read_bits(2).ok_or(AacParseError::UnexpectedEof)? as usize + 1;
    let start_sfb = reader.read_bits(6).ok_or(AacParseError::UnexpectedEof)? as u8;
    let mut offsets = Vec::with_capacity(np);
    let mut amps = Vec::with_capacity(np);
    for _ in 0..np {
        offsets.push(reader.read_bits(5).ok_or(AacParseError::UnexpectedEof)? as u8);
        let amp = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)? as u8;
        amps.push(amp as f32);
    }
    Ok(PulseData {
        start_sfb,
        offsets,
        amps,
    })
}

/// Apply pulse data to the dequantized spectrum in place.
///
/// ISO/IEC 14496-3 §4.6.3.5 applies each pulse in the **quantized** domain, not
/// by adding a raw value to the dequantized line: the affected coefficient is
/// converted back to its signed quantized value `q`, its magnitude is increased
/// by `pulse_amp` (`q -> q + sign(q)·amp`; a zero line becomes `-amp`), and
/// the result is re-dequantized with that band's scalefactor gain. This mirrors
/// ffmpeg's `decode_spectrum_and_dequant` pulse block
/// (`co /= sf; ico = q ± amp; coef = cbrt(|ico|)·ico·sf`). Pulses are only legal
/// in long windows, so `swb`/`scalefactor`/`band_type` are the long-window,
/// single-group tables. A pulse landing in a `ZERO`/`NOISE`/intensity band, or a
/// band with a zero gain, is skipped (matching ffmpeg's `band_type != NOISE_BT
/// && sf[idx]` guard).
pub fn apply_pulse(
    pulse: &PulseData,
    swb: &[u16],
    coeffs: &mut [f32; 1024],
    global_gain: u8,
    scalefactor: &[i32],
    band_type: &[u8],
) {
    // `start_sfb` is untrusted (from the bitstream); a hostile or desynced
    // stream can name a band past the scalefactor-band table.
    let Some(&start) = swb.get(pulse.start_sfb as usize) else {
        return;
    };
    let mut pos = start as usize;
    for i in 0..pulse.offsets.len() {
        pos += pulse.offsets[i] as usize;
        if pos >= 1024 {
            break;
        }
        // Scalefactor band containing `pos` (last `swb[k] <= pos`).
        let Some(sfb) = swb.iter().rposition(|&o| o as usize <= pos) else {
            continue;
        };
        if sfb + 1 >= swb.len() {
            continue;
        }
        let bt = band_type.get(sfb).copied().unwrap_or(0);
        if bt == ZERO_HCB || is_noise(bt) || is_intensity(bt) {
            continue;
        }
        let sf = scalefactor.get(sfb).copied().unwrap_or(0);
        let scale = dequant_scale(global_gain, sf) as f64;
        if scale == 0.0 {
            continue;
        }
        let amp = pulse.amps[i] as f64;
        let co = coeffs[pos] as f64;
        let q_new = if co != 0.0 {
            // `co / scale` == sign(q)·|q|^(4/3); dividing by `|·|^(1/4)` recovers
            // the signed quantized value `q`.
            let t = co / scale;
            let q = t / t.abs().powf(0.25);
            q + if q > 0.0 { amp } else { -amp }
        } else {
            -amp
        };
        coeffs[pos] = (q_new.signum() * q_new.abs().powf(4.0 / 3.0) * scale) as f32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
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
    fn parse_pulse_hand_computed() {
        // number_pulse = 0 (2 bits → np = 1); start_sfb = 0 (6 bits);
        // one pulse: offset = 0 (5 bits), amp = 3 (4 bits, used verbatim).
        let bits: Vec<u8> = vec![
            0, 0, // number_pulse = 0 → np = 1
            0, 0, 0, 0, 0, 0, // start_sfb = 0
            0, 0, 0, 0, 0, // offset[0] = 0
            0, 0, 1, 1, // amp[0] = 3
        ];
        let bytes = bits_to_bytes(&bits);
        let mut r = BitReader::new(&bytes);
        let p = parse_pulse(&mut r).unwrap();
        assert_eq!(p.start_sfb, 0);
        assert_eq!(p.offsets, vec![0]);
        assert_eq!(p.amps, vec![3.0]);
    }

    #[test]
    fn apply_pulse_zero_line_becomes_neg_amp_redequantized() {
        // A zero coefficient at the pulse line becomes `-amp` in the quantized
        // domain, re-dequantized: `-(amp^(4/3))·scale`. With global_gain 100 and
        // scalefactor 0, scale = 2^0 = 1, so line 0 → -(4^(4/3)) ≈ -6.3496.
        let pulse = PulseData {
            start_sfb: 0,
            offsets: vec![0],
            amps: vec![4.0],
        };
        let mut coeffs = [0.0f32; 1024];
        apply_pulse(&pulse, &[0u16, 4, 8], &mut coeffs, 100, &[0, 0], &[2, 2]);
        let want = -(4.0f64.powf(4.0 / 3.0)) as f32;
        assert!((coeffs[0] - want).abs() < 1e-3, "got {}", coeffs[0]);
    }

    #[test]
    fn apply_pulse_grows_quantized_magnitude() {
        // A non-zero line: q recovered, |q| += amp, re-dequantized. scale = 1
        // (gg 100, sf 0). Start from a dequantized 8.0 = q^(4/3) with q = 8^(3/4)
        // ≈ 4.757; after +2 → 6.757; re-dequant 6.757^(4/3) ≈ 12.42.
        let pulse = PulseData {
            start_sfb: 0,
            offsets: vec![1],
            amps: vec![2.0],
        };
        let mut coeffs = [0.0f32; 1024];
        coeffs[1] = 8.0;
        apply_pulse(&pulse, &[0u16, 4, 8], &mut coeffs, 100, &[0, 0], &[2, 2]);
        let q = 8.0f64.powf(0.75) + 2.0;
        let want = (q.powf(4.0 / 3.0)) as f32;
        assert!((coeffs[1] - want).abs() < 1e-2, "got {}", coeffs[1]);
    }

    #[test]
    fn apply_pulse_skips_zero_and_noise_bands() {
        let pulse = PulseData {
            start_sfb: 0,
            offsets: vec![0],
            amps: vec![4.0],
        };
        // band 0 = ZERO_HCB → skipped
        let mut coeffs = [0.0f32; 1024];
        apply_pulse(&pulse, &[0u16, 4], &mut coeffs, 100, &[0], &[0]);
        assert_eq!(coeffs[0], 0.0);
        // band 0 = NOISE_HCB (13) → skipped
        let mut coeffs = [0.0f32; 1024];
        apply_pulse(&pulse, &[0u16, 4], &mut coeffs, 100, &[0], &[13]);
        assert_eq!(coeffs[0], 0.0);
    }
}
