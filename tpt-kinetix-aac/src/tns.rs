//! Temporal Noise Shaping (ISO/IEC 14496-3 §4.6.9).
//!
//! TNS is an AR (all-pole) filter applied in the frequency domain to the
//! dequantized spectrum, per window group. The filter coefficients are
//! transmitted as reflection (lattice) coefficients and converted to direct
//! form before being applied.

use crate::bitreader::BitReader;
use crate::syntax::{AacParseError, IcsInfo};

/// Reflection-coefficient lookup tables (factual standard data, from the AAC
/// TNS coefficient tables; the first/last entries are unused sentinels).
const TNS_COEF_3: [f32; 8] = [
    0.0, -0.4338837, -0.7818315, -0.9749279, -0.9749279, -0.7818315, -0.4338837, 0.0,
];
const TNS_COEF_4: [f32; 16] = [
    0.0, -0.2079117, -0.4067366, -0.5877852, -0.7431448, -0.8660254, -0.9510565, -0.9945219,
    -0.9945219, -0.9510565, -0.8660254, -0.7431448, -0.5877852, -0.4067366, -0.2079117, 0.0,
];

/// A single TNS filter.
#[derive(Debug, Clone)]
pub struct TnsFilter {
    /// Number of bands this filter spans.
    pub length: u8,
    /// Filter order (number of reflection coefficients).
    pub order: u8,
    /// `false` = "up" (low→high in the band), `true` = "down".
    pub direction: bool,
    /// Direct-form (prediction) coefficients after lattice→direct conversion.
    pub coef: [f32; 20],
}

impl TnsFilter {
    /// Convert the `order` reflection (lattice) coefficients currently in
    /// `coef[0..order)` into direct-form prediction coefficients
    /// `b_1..b_order` (also stored in `coef[0..order)`), per the step-up
    /// recursion of §4.6.9.2. The filter then uses `y[n] = x[n] - Σ b_i·x[n-i]`.
    fn reflection_to_direct(&mut self) {
        let order = self.order as usize;
        if order == 0 {
            return;
        }
        let rc = self.coef; // snapshot reflection coefficients
        let mut d = [0.0f32; 21];
        d[0] = 1.0;
        for m in 1..=order {
            let k = rc[m - 1];
            let mut tmp = [0.0f32; 21];
            for i in 0..m - 1 {
                tmp[i] = d[i] - k * d[m - 1 - i];
            }
            for i in 0..m - 1 {
                d[i] = tmp[i];
            }
            d[m - 1] = k;
        }
        // b_1..b_order = d[1..order+1]
        for i in 0..order {
            self.coef[i] = d[i + 1];
        }
    }
}

/// Parsed TNS data for one channel.
#[derive(Debug, Clone, Default)]
pub struct TnsData {
    /// Number of filters per window group.
    pub n_filt: Vec<u8>,
    /// Filters per window group (indexed `[group][filt]`), up to 8 groups ×
    /// 8 filters.
    pub filters: Vec<Vec<TnsFilter>>,
}

/// Parse `tns_data()` if the `tns_data_present` bit was set.
pub fn parse_tns(
    reader: &mut BitReader,
    ics: &IcsInfo,
    tns_max_bands: u8,
) -> Result<TnsData, AacParseError> {
    let num_groups = ics.num_window_groups();
    let short = ics.window_sequence.is_eight_short();

    let mut n_filt = Vec::with_capacity(num_groups);
    let mut filters = Vec::with_capacity(num_groups);

    for _g in 0..num_groups {
        let nf = if short {
            reader.read_bits(1).ok_or(AacParseError::UnexpectedEof)? as u8
        } else {
            reader.read_bits(2).ok_or(AacParseError::UnexpectedEof)? as u8
        };
        n_filt.push(nf);

        let mut group_filters = Vec::with_capacity(nf as usize);
        // running band offset within this group's filters
        let mut band = 0u8;
        for _f in 0..nf {
            let coef_res = if reader.read_bit().ok_or(AacParseError::UnexpectedEof)? != 0 {
                4u8
            } else {
                3u8
            };
            let length_bits = if short { 4 } else { 6 };
            let length = reader
                .read_bits(length_bits)
                .ok_or(AacParseError::UnexpectedEof)? as u8;
            let order_bits = if short { 3 } else { 5 };
            let order = reader
                .read_bits(order_bits)
                .ok_or(AacParseError::UnexpectedEof)? as u8;

            let mut filt = TnsFilter {
                length,
                order,
                direction: false,
                coef: [0.0; 20],
            };

            if order > 0 {
                filt.direction = reader.read_bit().ok_or(AacParseError::UnexpectedEof)? != 0;
                let coef_compress = if reader.read_bit().ok_or(AacParseError::UnexpectedEof)? != 0 {
                    1u8
                } else {
                    0u8
                };
                let bits = (coef_res - coef_compress) as usize;
                for i in 0..order as usize {
                    let code = reader
                        .read_bits(bits as u32)
                        .ok_or(AacParseError::UnexpectedEof)? as usize;
                    filt.coef[i] = if bits <= 3 {
                        TNS_COEF_3[code.min(7)]
                    } else {
                        TNS_COEF_4[code.min(15)]
                    };
                }
                filt.reflection_to_direct();
            }

            let _ = tns_max_bands;
            let _ = band;
            group_filters.push(filt);
        }
        filters.push(group_filters);
    }

    Ok(TnsData { n_filt, filters })
}

/// Apply TNS filtering to one channel's dequantized spectrum in place.
///
/// `swb` is the offsets table for the current window sequence (long or short).
pub fn apply_tns(
    tns: &TnsData,
    ics: &IcsInfo,
    coeffs: &mut [f32; 1024],
    swb: &[u16],
) {
    let num_groups = ics.num_window_groups();

    // group base line offsets (mirrors decode_spectrum)
    let mut gindex = vec![0usize; num_groups];
    {
        let mut acc = 0usize;
        for gi in 0..num_groups {
            gindex[gi] = acc;
            acc += ics.group_len(gi) * 128;
        }
    }

    for gi in 0..num_groups {
        let glen = ics.group_len(gi);
        let gbase = gindex[gi];
        let group_filters = &tns.filters[gi];
        let mut band_start = 0usize;
        for filt in group_filters {
            let order = filt.order as usize;
            if order == 0 || filt.length == 0 {
                band_start += filt.length as usize;
                continue;
            }
            let start_band = band_start;
            let end_band = (band_start + filt.length as usize).min(swb.len() - 1);
            let mut line_start = swb[start_band] as usize;
            let mut line_end = swb[end_band] as usize;
            if line_end <= line_start {
                band_start += filt.length as usize;
                continue;
            }

            for w_idx in 0..glen {
                let base = gbase + w_idx * 128;
                // apply filter over [base+line_start, base+line_end) within this
                // window's 128-line segment.
                tns_filter_window(
                    &filt.coef,
                    order,
                    filt.direction,
                    &mut coeffs[base + line_start..base + line_end],
                );
            }
            band_start += filt.length as usize;
            let _ = line_end;
        }
    }
}

/// Apply one TNS AR filter to a contiguous spectrum segment.
fn tns_filter_window(coef: &[f32; 20], order: usize, direction: bool, seg: &mut [f32]) {
    let size = seg.len();
    if size == 0 || order == 0 {
        return;
    }
    if direction {
        // "down": filter from high frequency toward low.
        for i in (0..size).rev() {
            let mut acc = 0.0f32;
            for j in 0..order {
                if i + j + 1 >= size {
                    break;
                }
                acc += coef[j] * seg[i + j + 1];
            }
            seg[i] -= acc;
        }
    } else {
        // "up": filter from low frequency toward high.
        for i in 0..size {
            let mut acc = 0.0f32;
            for j in 0..order {
                if i < j + 1 {
                    break;
                }
                acc += coef[j] * seg[i - j - 1];
            }
            seg[i] -= acc;
        }
    }
}
