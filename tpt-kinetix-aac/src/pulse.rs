//! Pulse data (ISO/IEC 14496-3 §4.6.3.5).
//!
//! Pulse data adds a small number of large-magnitude spectral lines to an
//! otherwise sparse band (used for highly tonal signals). It is applied to the
//! dequantized spectrum before the IMDCT.

use crate::bitreader::BitReader;
use crate::syntax::AacParseError;

/// Parsed pulse-data for one channel.
#[derive(Debug, Clone, Default)]
pub struct PulseData {
    /// Scalefactor band the pulses start in (lower bands only).
    pub start_sfb: u8,
    /// Relative offsets (cumulative) to the affected spectral lines.
    pub offsets: Vec<u8>,
    /// Amplitudes (already incremented by 1 at parse time).
    pub amps: Vec<f32>,
}

/// Parse `pulse_data()` (called only when `pulse_data_present` was set).
pub fn parse_pulse(reader: &mut BitReader) -> Result<PulseData, AacParseError> {
    let np = reader.read_bits(1).ok_or(AacParseError::UnexpectedEof)? as usize + 1;
    let start_sfb = reader.read_bits(6).ok_or(AacParseError::UnexpectedEof)? as u8;
    let mut offsets = Vec::with_capacity(np);
    let mut amps = Vec::with_capacity(np);
    for _ in 0..np {
        offsets.push(reader.read_bits(5).ok_or(AacParseError::UnexpectedEof)? as u8);
        let amp = reader.read_bits(4).ok_or(AacParseError::UnexpectedEof)? as u8 + 1;
        amps.push(amp as f32);
    }
    Ok(PulseData {
        start_sfb,
        offsets,
        amps,
    })
}

/// Apply pulse data to the dequantized spectrum in place.
pub fn apply_pulse(pulse: &PulseData, swb: &[u16], coeffs: &mut [f32; 1024]) {
    let mut offset = swb[pulse.start_sfb as usize] as usize;
    for i in 0..pulse.offsets.len() {
        offset += pulse.offsets[i] as usize;
        if offset < 1024 {
            // §4.6.3.5: add the pulse amplitude (already incremented by 1 at
            // parse time) to the spectral line at the cumulative offset, with the
            // sign set by the parity of that offset (even → +, odd → −).
            let sign = if offset & 1 == 0 { 1.0f32 } else { -1.0f32 };
            coeffs[offset] += sign * pulse.amps[i];
        }
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
        // number_pulse = 1 (1 bit → np = 1); start_sfb = 0 (6 bits);
        // one pulse: offset = 0 (5 bits), amp = 3 (4 bits → stored = 4).
        let bits: Vec<u8> = vec![
            0,             // number_pulse = 0 → np = 1
            0, 0, 0, 0, 0, 0, // start_sfb = 0
            0, 0, 0, 0, 0, // offset[0] = 0
            0, 0, 1, 1,    // amp[0] = 3 → 3 + 1 = 4
        ];
        let bytes = bits_to_bytes(&bits);
        let mut r = BitReader::new(&bytes);
        let p = parse_pulse(&mut r).unwrap();
        assert_eq!(p.start_sfb, 0);
        assert_eq!(p.offsets, vec![0]);
        assert_eq!(p.amps, vec![4.0]);
    }

    #[test]
    fn apply_pulse_adds_unsigned_amplitude() {
        // start_sfb line 0; offsets 0,2 → lines 0 and 2 (both even → +).
        let pulse = PulseData {
            start_sfb: 0,
            offsets: vec![0, 2],
            amps: vec![3.0, 5.0],
        };
        let mut coeffs = [0.0f32; 1024];
        apply_pulse(&pulse, &[0u16, 4, 8], &mut coeffs);
        assert!((coeffs[0] - 3.0).abs() < 1e-6, "line 0 += 3");
        assert!((coeffs[2] - 5.0).abs() < 1e-6, "line 2 += 5");
    }

    #[test]
    fn apply_pulse_sign_alternates_with_offset_parity() {
        // cumulative offsets 1 (odd → −) then 2 (even → +).
        let pulse = PulseData {
            start_sfb: 0,
            offsets: vec![1, 1],
            amps: vec![2.0, 2.0],
        };
        let mut coeffs = [0.0f32; 1024];
        apply_pulse(&pulse, &[0u16, 4], &mut coeffs);
        assert!((coeffs[1] + 2.0).abs() < 1e-6, "line 1 -= 2 (odd)");
        assert!((coeffs[2] - 2.0).abs() < 1e-6, "line 2 += 2 (even)");
    }
}
