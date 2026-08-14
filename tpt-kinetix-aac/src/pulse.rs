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
            coeffs[offset] += pulse.amps[i] * (i as f32 + 1.0);
        }
    }
}
