//! Attribute decode (DECISION 3).
//!
//! Both **lift** (region-adaptive predictive, the v1 default) and **RAHT**
//! (region-adaptive hierarchical transform, selectable) are implemented as
//! normative, self-consistent coders. The decoder branches on the sequence
//! header's `attribute_coding` flag and reconstructs the per-point attribute
//! streams in the same Morton order the geometry coder emitted.
//!
//! # Honesty note
//!
//! These are *simplified* G-PCC-faithful tools, not bit-exact TMC13 transforms:
//!
//! - **Lift** predicts each point's attributes from the mean of the previous
//!   `K` Morton-neighbour points and rANS-codes the residual (a valid
//!   region-adaptive predictive scheme; G-PCC uses a k-NN search instead of a
//!   fixed history window).
//! - **RAHT** applies an exactly-invertible integer Haar transform over the
//!   Morton-ordered attribute stream (a faithful "region-adaptive hierarchical
//!   transform" on the 1-D Morton sequence; G-PCC's RAHT is a 3-D octree
//!   transform with orthonormal weighting).
//!
//! Both round-trip losslessly when `quant_step == 1` (DECISION 4 lossless
//! path). They are not yet validated bit-exact against the TMC13 oracle, so
//! [`crate::VolumetricDecoderImpl::capabilities`] reports `pixel_exact: false`.

use tpt_kinetix_core::error::KinetixError;
use tpt_kinetix_core::frame::{PointAttribute, PointAttributeKind};

use crate::entropy::{decode_symbols, encode_symbols_rev};
use crate::header::{AttributeCoding, AttributeInfo};

/// Number of scalar samples per point for an attribute kind.
pub fn samples_per_attr(kind: PointAttributeKind) -> usize {
    match kind {
        PointAttributeKind::ColorRgb | PointAttributeKind::Normal => 3,
        PointAttributeKind::Reflectance => 1,
    }
}

/// History window for the lift predictor (number of preceding Morton
/// neighbours averaged to form the prediction).
const LIFT_K: usize = 3;

/// Quantizer step derived from the `lossless` header flag.
///
/// `quant_step == 1` is the lossless path (exact reconstruction); any larger
/// step trades fidelity for rate (DECISION 4).
pub fn quant_step(lossless: bool) -> i32 {
    if lossless {
        1
    } else {
        4
    }
}

/// Split a decoded [`PointCloud`]'s attributes into one scalar `i32` stream per
/// sample (used by the encoder). Returns streams in header order.
pub fn unpack_streams(attributes: &[PointAttribute], num_points: usize) -> Vec<Vec<i32>> {
    let mut streams = Vec::new();
    for attr in attributes {
        let bytes_per = if attr.bit_depth <= 8 { 1 } else { 2 };
        let n = samples_per_attr(attr.kind);
        for s in 0..n {
            let mut stream = Vec::with_capacity(num_points);
            let mut off = s * bytes_per;
            for _ in 0..num_points {
                let v = if bytes_per == 1 {
                    attr.data[off] as i32
                } else {
                    i32::from(u16::from_le_bytes([attr.data[off], attr.data[off + 1]]))
                };
                stream.push(v);
                off += n * bytes_per;
            }
            streams.push(stream);
        }
    }
    streams
}

/// Pack scalar streams back into [`PointAttribute`]s following `attributes`.
pub fn pack_streams(
    streams: &[Vec<i32>],
    attributes: &[AttributeInfo],
    num_points: usize,
) -> Vec<PointAttribute> {
    let mut out = Vec::with_capacity(attributes.len());
    let mut stream_idx = 0;
    for info in attributes {
        let n = samples_per_attr(info.kind);
        let bytes_per = if info.bit_depth <= 8 { 1 } else { 2 };
        let max_val = if info.bit_depth <= 8 {
            255
        } else {
            (1i32 << info.bit_depth) - 1
        };
        let mut data = vec![0u8; num_points * n * bytes_per];
        for s in 0..n {
            let stream = &streams[stream_idx];
            stream_idx += 1;
            let mut off = s * bytes_per;
            for &raw in stream.iter() {
                let v = raw.clamp(0, max_val);
                if bytes_per == 1 {
                    data[off] = v as u8;
                } else {
                    data[off..off + 2].copy_from_slice(&(v as u16).to_le_bytes());
                }
                off += n * bytes_per;
            }
        }
        out.push(PointAttribute {
            kind: info.kind,
            bit_depth: info.bit_depth,
            data,
        });
    }
    out
}

/// Encode the attribute streams into a single rANS byte stream.
pub fn encode_attributes(streams: &[Vec<i32>], coding: AttributeCoding, lossless: bool) -> Vec<u8> {
    let q = quant_step(lossless);
    let mut symbols: Vec<u8> = Vec::new();

    match coding {
        AttributeCoding::Lift => {
            for stream in streams {
                let n = stream.len();
                for i in 0..n {
                    let pred = if i == 0 {
                        0
                    } else {
                        let lo = i.saturating_sub(LIFT_K);
                        let window = &stream[lo..i];
                        window.iter().sum::<i32>() / window.len() as i32
                    };
                    let residual = (stream[i] - pred).div_euclid(q) as i16;
                    symbols.extend_from_slice(&residual.to_le_bytes());
                }
            }
        }
        AttributeCoding::Raht => {
            for stream in streams {
                let mut coeffs = stream.clone();
                raht_forward(&mut coeffs);
                for &c in &coeffs {
                    let v = (c.div_euclid(q)) as i16;
                    symbols.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
    }

    encode_symbols_rev(&symbols)
}

/// Decode the attribute rANS stream back into per-sample `i32` streams.
pub fn decode_attributes(
    data: &[u8],
    num_streams: usize,
    num_points: usize,
    coding: AttributeCoding,
    lossless: bool,
) -> Result<Vec<Vec<i32>>, KinetixError> {
    let q = quant_step(lossless);
    let total = num_streams * num_points * 2;
    let symbols = decode_symbols(data, total)?;

    let mut streams: Vec<Vec<i32>> = vec![vec![0; num_points]; num_streams];

    match coding {
        AttributeCoding::Lift => {
            for (s, stream) in streams.iter_mut().enumerate() {
                let base = s * num_points * 2;
                for i in 0..num_points {
                    let off = base + 2 * i;
                    let residual = i16::from_le_bytes([symbols[off], symbols[off + 1]]) as i32 * q;
                    let pred = if i == 0 {
                        0
                    } else {
                        let lo = i.saturating_sub(LIFT_K);
                        let window = &stream[lo..i];
                        window.iter().sum::<i32>() / window.len() as i32
                    };
                    stream[i] = pred + residual;
                }
            }
        }
        AttributeCoding::Raht => {
            for (s, stream) in streams.iter_mut().enumerate() {
                let base = s * num_points * 2;
                let mut coeffs = vec![0i32; num_points];
                for (i, slot) in coeffs.iter_mut().enumerate() {
                    let off = base + 2 * i;
                    *slot = i16::from_le_bytes([symbols[off], symbols[off + 1]]) as i32 * q;
                }
                raht_inverse(&mut coeffs);
                *stream = coeffs;
            }
        }
    }

    Ok(streams)
}

/// Exactly-invertible integer Haar (1-D, multi-level) over the Morton-ordered
/// attribute stream — the simplified RAHT forward transform.
fn raht_forward(ch: &mut [i32]) {
    let n = ch.len();
    let mut size = 2usize;
    while size <= n {
        // `i` indexes `ch`; the partial final block (when `n` is not a power of
        // two) is intentionally skipped, so a plain `step_by` will not do.
        #[allow(clippy::needless_range_loop)]
        let mut i = 0;
        while i + size <= n {
            let block = &mut ch[i..i + size];
            for pair in block.chunks_exact_mut(2) {
                let x = pair[0];
                let y = pair[1];
                let d = x - y;
                let a = y + (d >> 1);
                pair[0] = a;
                pair[1] = d;
            }
            i += size;
        }
        size *= 2;
    }
}

/// Inverse of [`raht_forward`].
fn raht_inverse(ch: &mut [i32]) {
    let n = ch.len();
    let mut size = 1usize;
    while size * 2 <= n {
        size *= 2;
    }
    while size >= 2 {
        #[allow(clippy::needless_range_loop)]
        let mut i = 0;
        while i + size <= n {
            let block = &mut ch[i..i + size];
            for pair in block.chunks_exact_mut(2) {
                let a = pair[0];
                let d = pair[1];
                let y = a - (d >> 1);
                let x = d + y;
                pair[0] = x;
                pair[1] = y;
            }
            i += size;
        }
        size /= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_haar_is_lossless() {
        let mut v = vec![10, 20, 5, 40, 100, 99, 0, 7];
        let w = v.clone();
        raht_forward(&mut v);
        raht_inverse(&mut v);
        assert_eq!(v, w);
    }

    #[test]
    fn lift_round_trips_lossless() {
        let streams = vec![
            vec![10, 12, 11, 200, 205, 1, 0],
            vec![20, 21, 19, 30, 33, 2, 5],
        ];
        let bytes = encode_attributes(&streams, AttributeCoding::Lift, true);
        let decoded = decode_attributes(&bytes, 2, 7, AttributeCoding::Lift, true).expect("decode");
        assert_eq!(decoded, streams);
    }

    #[test]
    fn raht_round_trips_lossless() {
        let streams = vec![vec![10, 12, 11, 200, 205, 1, 0, 7, 50, 51]];
        let bytes = encode_attributes(&streams, AttributeCoding::Raht, true);
        let decoded =
            decode_attributes(&bytes, 1, 10, AttributeCoding::Raht, true).expect("decode");
        assert_eq!(decoded, streams);
    }
}
