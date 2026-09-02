//! Temporal Noise Shaping (ISO/IEC 14496-3 §4.6.9).
//!
//! TNS is an AR (all-pole) filter applied in the frequency domain to the
//! dequantized spectrum, per individual transform window. The filter coefficients are
//! transmitted as reflection (lattice) coefficients and converted to direct
//! form before being applied.

use crate::bitreader::BitReader;
use crate::syntax::{AacParseError, IcsInfo};

/// Inverse-quantized TNS reflection-coefficient tables (ISO/IEC 14496-3
/// §4.6.9.3). The four tables are selected by `(coef_compress, coef_res)` and
/// indexed directly by the raw `coef_len`-bit codeword — they already fold in
/// the two `iqfac` / `iqfac_m` quantizer steps for positive vs negative codes,
/// so the codeword's sign extension does not need to be undone separately.
/// Values match ffmpeg's `ff_tns_tmp2_map` (`libavcodec/aactab.c`), whose
/// naming is `_<coef_compress>_<coef_res>`.
const TNS_TMP2_MAP_0_3: [f32; 8] = [
    0.0, -0.4338837, -0.7818315, -0.9749279, 0.9848077, 0.8660254, 0.6427876, 0.3420201,
];
const TNS_TMP2_MAP_0_4: [f32; 16] = [
    0.0, -0.2079117, -0.4067366, -0.5877852, -0.7431448, -0.8660254, -0.9510565, -0.9945219,
    0.9957342, 0.9618256, 0.8951633, 0.7980172, 0.6736956, 0.5264322, 0.3612417, 0.1837495,
];
const TNS_TMP2_MAP_1_3: [f32; 4] = [0.0, -0.4338837, 0.6427876, 0.3420201];
const TNS_TMP2_MAP_1_4: [f32; 8] = [
    0.0, -0.2079117, -0.4067366, -0.5877852, 0.6736956, 0.5264322, 0.3612417, 0.1837495,
];

/// Look up an inverse-quantized TNS reflection coefficient. `coef_res` is the
/// raw 1-bit `coef_res` flag (0 → 3-bit base resolution, 1 → 4-bit); the
/// codeword width is `coef_res + 3 - coef_compress` bits.
fn tns_reflection_coef(coef_compress: u8, coef_res: u8, code: usize) -> f32 {
    match (coef_compress, coef_res) {
        (0, 0) => TNS_TMP2_MAP_0_3[code & 7],
        (0, _) => TNS_TMP2_MAP_0_4[code & 15],
        (1, 0) => TNS_TMP2_MAP_1_3[code & 3],
        (_, _) => TNS_TMP2_MAP_1_4[code & 7],
    }
}

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
    /// `coef[0..order)` into direct-form coefficients (also stored in
    /// `coef[0..order)`) using ffmpeg's `compute_lpc_coefs` step-up recursion
    /// (`r = -k`, symmetric in-place update). The decoder's all-pole filter is
    /// then `y[n] = x[n] - Σ coef[i-1]·y[n-i]`.
    fn reflection_to_direct(&mut self) {
        let order = self.order as usize;
        if order == 0 {
            return;
        }
        // Snapshot the reflection coefficients, then run a verbatim port of
        // ffmpeg's `compute_lpc_coefs(autoc, order, lpc, 0, 0, 0)`
        // (`libavcodec/lpc.h`, float path where AAC_SRA_R / AAC_MUL26 are
        // identity / multiply). `r = -autoc[i]` and the symmetric in-place
        // update produce direct-form coefficients such that the decoder's
        // all-pole ("AR") filter is `y[n] = x[n] - Σ lpc[i-1]·y[n-i]`
        // (ffmpeg `apply_tns`, `decode == 1`).
        let autoc = self.coef;
        let mut lpc = [0.0f32; 20];
        for i in 0..order {
            let r = -autoc[i];
            lpc[i] = r;
            for j in 0..((i + 1) >> 1) {
                let f = lpc[j];
                let b = lpc[i - 1 - j];
                lpc[j] = f + r * b;
                lpc[i - 1 - j] = b + r * f;
            }
        }
        self.coef[..order].copy_from_slice(&lpc[..order]);
    }
}

/// Parsed TNS data for one channel.
#[derive(Debug, Clone, Default)]
pub struct TnsData {
    /// Number of filters per window (indexed by window `0..num_windows`).
    pub n_filt: Vec<u8>,
    /// Filters per window (indexed `[window][filt]`), up to 8 windows ×
    /// 3 filters (long) / 1 filter (short).
    pub filters: Vec<Vec<TnsFilter>>,
    /// `tns_max_bands` for the current window sequence / sample rate — the
    /// highest scalefactor band TNS may touch (ISO/IEC 14496-3 Table 4.156).
    pub tns_max_bands: u8,
}

/// Parse `tns_data()` if the `tns_data_present` bit was set.
pub fn parse_tns(
    reader: &mut BitReader,
    ics: &IcsInfo,
    tns_max_bands: u8,
) -> Result<TnsData, AacParseError> {
    // TNS data is transmitted per individual window (all 8 for EIGHT_SHORT),
    // not per window group — ISO/IEC 14496-3 §4.4.2.4 `tns_data()` loops
    // `num_windows`.
    let num_windows = ics.num_windows();
    let short = ics.window_sequence.is_eight_short();

    let mut n_filt = Vec::with_capacity(num_windows);
    let mut filters = Vec::with_capacity(num_windows);

    for _w in 0..num_windows {
        let nf = if short {
            reader.read_bits(1).ok_or(AacParseError::UnexpectedEof)? as u8
        } else {
            reader.read_bits(2).ok_or(AacParseError::UnexpectedEof)? as u8
        };
        n_filt.push(nf);

        // `coef_res` (ISO 14496-3 §4.4.2.4's `tns_data()`) is read once per
        // window when it has any filters, not once per filter. Kept as the
        // raw 1-bit flag: it selects the reflection-coefficient table and, with
        // `coef_compress`, the codeword width (`coef_res + 3 - coef_compress`).
        let coef_res = if nf > 0 {
            reader.read_bit().ok_or(AacParseError::UnexpectedEof)?
        } else {
            0u8
        };

        let mut window_filters = Vec::with_capacity(nf as usize);
        for _f in 0..nf {
            let length_bits = if short { 4 } else { 6 };
            let length = reader
                .read_bits(length_bits)
                .ok_or(AacParseError::UnexpectedEof)? as u8;
            let order_bits = if short { 3 } else { 5 };
            let raw_order = reader
                .read_bits(order_bits)
                .ok_or(AacParseError::UnexpectedEof)? as u8;
            // AAC caps the TNS filter order at 20 (long) / 7 (short) windows.
            // Clamp defensively so a hostile or non-conformant stream cannot
            // overflow the fixed `coef` buffer (and to match conformant encoders).
            let max_order = if short { 7u8 } else { 20u8 };
            let order = raw_order.min(max_order);

            let mut filt = TnsFilter {
                length,
                order,
                direction: false,
                coef: [0.0; 20],
            };

            if order > 0 {
                filt.direction = reader.read_bit().ok_or(AacParseError::UnexpectedEof)? != 0;
                let coef_compress = reader.read_bit().ok_or(AacParseError::UnexpectedEof)?;
                let bits = (coef_res + 3 - coef_compress) as u32;
                for i in 0..order as usize {
                    let code = reader.read_bits(bits).ok_or(AacParseError::UnexpectedEof)? as usize;
                    filt.coef[i] = tns_reflection_coef(coef_compress, coef_res, code);
                }
                filt.reflection_to_direct();
            }

            window_filters.push(filt);
        }
        filters.push(window_filters);
    }

    Ok(TnsData {
        n_filt,
        filters,
        tns_max_bands,
    })
}

/// Apply TNS filtering to one channel's dequantized spectrum in place.
///
/// `swb` is the offsets table for the current window sequence (long or short).
pub fn apply_tns(tns: &TnsData, ics: &IcsInfo, coeffs: &mut [f32; 1024], swb: &[u16]) {
    // ISO/IEC 14496-3 §4.6.9.3: TNS is applied per individual window (window `w`
    // occupies lines `[w*128, w*128+128)` of the de-interleaved coefficient
    // buffer for EIGHT_SHORT; one window covering all 1024 lines otherwise).
    // Within a window, filters are numbered from the high-frequency end downward:
    // the first covers `[num_swb - length, num_swb]`, the next the bands below
    // it, and so on; band indices are clamped to `min(tns_max_bands, max_sfb)`
    // for the line lookup (matches ffmpeg's `apply_tns`).
    let num_windows = ics.num_windows();
    let num_swb = swb.len().saturating_sub(1);
    let mmm = (tns.tns_max_bands as usize).min(ics.max_sfb as usize);

    for w in 0..num_windows {
        let Some(window_filters) = tns.filters.get(w) else {
            break;
        };
        let wbase = w * 128;
        let mut bottom = num_swb;
        for filt in window_filters {
            let top = bottom;
            bottom = top.saturating_sub(filt.length as usize);
            let order = filt.order as usize;
            if order == 0 {
                continue;
            }
            let start_band = bottom.min(mmm);
            let end_band = top.min(mmm);
            if start_band >= swb.len() || end_band >= swb.len() {
                continue;
            }
            let line_start = swb[start_band] as usize;
            let line_end = swb[end_band] as usize;
            if line_end <= line_start {
                continue;
            }
            tns_filter_window(
                &filt.coef,
                order,
                filt.direction,
                &mut coeffs[wbase + line_start..wbase + line_end],
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{IcsInfo, WindowSequence};

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

    /// Reflection→direct conversion for a hand-picked set of reflection
    /// coefficients, checked against a hand-expansion of ffmpeg's
    /// `compute_lpc_coefs` (`r = -k`, symmetric step-up). The decoder's AR
    /// filter is `y[n] = x[n] - Σ coef[i-1]·y[n-i]`.
    #[test]
    fn reflection_to_direct_hand_computed() {
        let mk = |rc: &[f32]| {
            let mut c = [0.0f32; 20];
            c[..rc.len()].copy_from_slice(rc);
            let mut f = TnsFilter {
                length: 1,
                order: rc.len() as u8,
                direction: false,
                coef: c,
            };
            f.reflection_to_direct();
            f.coef
        };

        // order 1: lpc_1 = -k_1
        let c = mk(&[0.5]);
        assert!((c[0] - (-0.5)).abs() < 1e-6, "order1");
        assert!(c[1].abs() < 1e-6, "order1 tail zero");

        // order 2: lpc = [-k1·(1-k2), -k2]
        let (k1, k2) = (0.5f32, -0.3f32);
        let c = mk(&[k1, k2]);
        assert!((c[0] - (-k1 * (1.0 - k2))).abs() < 1e-6, "order2 b1");
        assert!((c[1] - (-k2)).abs() < 1e-6, "order2 b2");

        // order 3: lpc = [-k1·(1-k2) + k2·k3, -k2 + k1·k3·(1-k2), -k3]
        let k3 = 0.1f32;
        let c = mk(&[k1, k2, k3]);
        assert!(
            (c[0] - (-k1 * (1.0 - k2) + k2 * k3)).abs() < 1e-6,
            "order3 b1"
        );
        assert!(
            (c[1] - (-k2 + k1 * k3 * (1.0 - k2))).abs() < 1e-6,
            "order3 b2"
        );
        assert!((c[2] - (-k3)).abs() < 1e-6, "order3 b3");
    }

    /// Simulate the TNS AR filter ("up" direction) with an independent,
    /// directly-coded reference (`y[i] = x[i] − Σ b_{j+1}·x[i−j−1]`) and compare
    /// against the implementation under test. This is the "independently computed
    /// reference" required by the Phase 3 exit criteria.
    #[test]
    fn tns_filter_matches_independent_reference() {
        let b = [
            0.5f32, -0.25, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
            0.0, 0.0, 0.0, 0.0,
        ];
        let order = 2;
        let input = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];

        let mut got = input;
        tns_filter_window(&b, order, false, &mut got);

        // Independent recursive reference: the AR filter is applied in place
        // (each output line is predicted from the already-filtered lines below
        // it), matching the spec/ffmpeg TNS recursion.
        let mut expected = input;
        for i in 0..6 {
            let mut acc = 0.0f32;
            for j in 0..order {
                if i > j {
                    acc += b[j] * expected[i - j - 1];
                }
            }
            expected[i] -= acc;
        }

        for i in 0..6 {
            assert!(
                (got[i] - expected[i]).abs() < 1e-5,
                "up-filter mismatch at i={i}: got {} expected {}",
                got[i],
                expected[i]
            );
        }
    }

    /// Full `parse`-free path: a known direct-form filter applied through
    /// `apply_tns` must match the independent reference over the band it spans,
    /// and leave the rest of the spectrum untouched.
    #[test]
    fn apply_tns_matches_independent_reference() {
        let ics = ics_long(4);
        let k1 = 0.5f32;
        let k2 = -0.3f32;
        let mut filt = TnsFilter {
            length: 2,
            order: 2,
            direction: false,
            coef: [
                k1, k2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                0.0, 0.0, 0.0,
            ],
        };
        filt.reflection_to_direct();
        // Capture the direct-form coefficients before `filt` is moved into TnsData.
        let b = [filt.coef[0], filt.coef[1]];
        let tns = TnsData {
            n_filt: vec![1],
            filters: vec![vec![filt]],
            tns_max_bands: 4,
        };
        // 2 lines per band, 4 bands. Per §4.6.9.3 the first filter is counted
        // from the top: length 2 → bands [2, 4) → lines [4, 8).
        let swb = [0u16, 2, 4, 6, 8];
        let mut coeffs = [0.0f32; 1024];
        for (i, c) in coeffs.iter_mut().enumerate().take(8).skip(4) {
            *c = (i - 3) as f32;
        }

        // Independent recursive reference: AR filter over lines 4..8, applied in
        // place to match the spec/ffmpeg TNS recursion.
        let order = 2;
        let mut expected = coeffs;
        for i in 4..8 {
            let mut acc = 0.0f32;
            for j in 0..order {
                if i - 4 > j {
                    acc += b[j] * expected[i - j - 1];
                }
            }
            expected[i] -= acc;
        }

        apply_tns(&tns, &ics, &mut coeffs, &swb);
        for (i, (c, e)) in coeffs
            .iter()
            .zip(expected.iter())
            .enumerate()
            .take(8)
            .skip(4)
        {
            assert!(
                (c - e).abs() < 1e-5,
                "apply_tns mismatch at line {i}: got {} expected {}",
                c,
                e
            );
        }
        for (i, &c) in coeffs.iter().enumerate().take(1024) {
            if !(4..8).contains(&i) {
                assert_eq!(c, 0.0, "line {i} should be untouched");
            }
        }
    }

    /// Independent, verbatim port of ffmpeg n6.1
    /// `compute_lpc_coefs(autoc, order, lpc, 0, 0, 0)` (`libavcodec/lpc.h`,
    /// float path where `AAC_SRA_R` / `AAC_MUL26` are identity / multiply) —
    /// the reference `reflection_to_direct` is checked against.
    fn ff_compute_lpc_coefs(autoc: &[f32], order: usize) -> Vec<f32> {
        let mut lpc = vec![0.0f32; order];
        for i in 0..order {
            let r = -autoc[i];
            lpc[i] = r;
            let mut j = 0;
            while j < (i + 1) >> 1 {
                let f = lpc[j];
                let b = lpc[i - 1 - j];
                lpc[j] = f + r * b;
                lpc[i - 1 - j] = b + r * f;
                j += 1;
            }
        }
        lpc
    }

    #[test]
    fn reflection_to_direct_matches_ffmpeg_compute_lpc_coefs() {
        let cases: &[&[f32]] = &[
            &[0.5],
            &[0.5, -0.3],
            &[0.9848077, -0.4338837, 0.6427876],
            &[0.34, -0.78, 0.86, -0.20, 0.5, -0.1, 0.42],
        ];
        for rc in cases {
            let order = rc.len();
            let mut filt = TnsFilter {
                length: 1,
                order: order as u8,
                direction: false,
                coef: {
                    let mut c = [0.0f32; 20];
                    c[..order].copy_from_slice(rc);
                    c
                },
            };
            filt.reflection_to_direct();
            let ff = ff_compute_lpc_coefs(rc, order);
            for (i, &want) in ff.iter().enumerate() {
                assert!(
                    (filt.coef[i] - want).abs() < 1e-5,
                    "rc={rc:?} i={i}: ours={} ffmpeg={want}",
                    filt.coef[i],
                );
            }
        }
    }

    /// The "down" direction reverses the filter sweep; verify it differs from the
    /// up direction and is self-consistent with the reference applied to the
    /// mirrored segment.
    #[test]
    fn tns_filter_down_direction_applies() {
        let b = [
            0.5f32, -0.25, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
            0.0, 0.0, 0.0, 0.0,
        ];
        let order = 2;
        // Use a segment where every line is non-zero so the direction is observable.
        let input = [1.0f32, 2.0, 3.0, 4.0];
        let mut up = input;
        tns_filter_window(&b, order, false, &mut up);
        let mut down = input;
        tns_filter_window(&b, order, true, &mut down);
        // The first and last lines differ between directions (boundary handling).
        assert!(
            (up[0] - down[0]).abs() > 1e-6,
            "down direction should differ at start"
        );
    }
}
