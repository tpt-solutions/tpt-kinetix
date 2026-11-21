//! SPZ writer: version 4 NGSP files.
//!
//! Each attribute stream is one ZSTD frame. With the default `zstd-encode`
//! feature the frame is compressed by libzstd, which gives output close to the
//! reference's size. Without it, the frame is made of raw (uncompressed) blocks,
//! which are valid ZSTD and need no encoder.
//!
//! Quantisation matches the reference writer's defaults: 12 fractional bits
//! for positions, 8-bit alpha and colour, log scale as `(s + 10) * 16`,
//! smallest-three rotations, and SH at full 8-bit resolution (bucket size 1).

use crate::cloud::{sh_coeffs_for_degree, GaussianCloud};
use crate::error::SplatError;

/// Fixed-point fractional bits for positions (the reference default).
const FRACTIONAL_BITS: u8 = 12;
/// SPZ colour encoding scale (see `spz.rs`).
const COLOR_SCALE: f32 = 0.15;
/// Largest raw ZSTD block payload.
#[cfg(any(test, not(feature = "zstd-encode")))]
const RAW_BLOCK_MAX: usize = 128 * 1024;
/// Largest 24-bit signed fixed-point value.
const FIXED_MAX: f32 = ((1 << 23) - 1) as f32;
const FIXED_MIN: f32 = -((1 << 23) as f32);

/// Serialises a [`GaussianCloud`] as an SPZ version 4 file.
///
/// The output is in the cloud's own frame; no RUB↔RDF conversion is applied,
/// matching the reader. SPZ has no frame field, so the tag is not written.
/// Streams are ZSTD-compressed when the `zstd-encode` feature is on (default).
///
/// # Errors
///
/// * [`SplatError::Invalid`] for an empty cloud, or a position that does not
///   fit the 24-bit fixed-point range (|x| must stay below 2048 at 12 bits).
pub fn write_spz(cloud: &GaussianCloud) -> Result<Vec<u8>, SplatError> {
    let n = cloud.len();
    if n == 0 {
        return Err(SplatError::Invalid("cannot write an empty SPZ cloud"));
    }
    let degree = cloud.sh_degree();
    let k = sh_coeffs_for_degree(degree);
    let rest = k - 1;
    let scale = (1u32 << FRACTIONAL_BITS) as f32;

    let mut positions = Vec::with_capacity(n * 9);
    for p in cloud.positions() {
        for &v in p {
            let fixed = (v * scale).round();
            if !(FIXED_MIN..=FIXED_MAX).contains(&fixed) {
                return Err(SplatError::Invalid(
                    "position out of 24-bit fixed-point range",
                ));
            }
            let f = fixed as i32;
            positions.extend_from_slice(&[f as u8, (f >> 8) as u8, (f >> 16) as u8]);
        }
    }

    let alphas: Vec<u8> = cloud
        .opacities()
        .iter()
        .map(|&a| to_u8((1.0 / (1.0 + (-a).exp())) * 255.0))
        .collect();

    let mut colors = Vec::with_capacity(n * 3);
    let mut scales = Vec::with_capacity(n * 3);
    let mut rotations = Vec::with_capacity(n * 4);
    let mut sh_rest = Vec::with_capacity(n * rest * 3);
    let sh = cloud.sh();
    for i in 0..n {
        let base = i * 3 * k;
        for ch in 0..3 {
            colors.push(to_u8(sh[base + ch * k] * (COLOR_SCALE * 255.0) + 127.5));
        }
        for &s in &cloud.scales()[i] {
            scales.push(to_u8((s + 10.0) * 16.0));
        }
        // Model order is [w, x, y, z]; the SPZ stream is [x, y, z, w].
        let q = cloud.rotations()[i];
        rotations.extend_from_slice(&encode_smallest_three([q[1], q[2], q[3], q[0]]));
        for j in 0..rest {
            for ch in 0..3 {
                let q = (sh[base + ch * k + 1 + j] * 128.0).round() + 128.0;
                sh_rest.push(q.clamp(0.0, 255.0) as u8);
            }
        }
    }

    let mut streams = vec![positions, alphas, colors, scales, rotations];
    if degree > 0 {
        streams.push(sh_rest);
    }
    let frames: Vec<(usize, Vec<u8>)> = streams
        .iter()
        .map(|s| Ok((s.len(), compress_stream(s)?)))
        .collect::<Result<_, SplatError>>()?;

    let toc_len = frames.len() * 16;
    let mut out =
        Vec::with_capacity(32 + toc_len + frames.iter().map(|(_, f)| f.len()).sum::<usize>());
    out.extend_from_slice(b"NGSP");
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&(n as u32).to_le_bytes());
    out.push(degree);
    out.push(FRACTIONAL_BITS);
    out.push(0); // flags: no antialiasing, no extensions
    out.push(frames.len() as u8);
    out.extend_from_slice(&32u32.to_le_bytes()); // TOC follows the header
    out.extend_from_slice(&[0u8; 12]);
    for (raw_len, frame) in &frames {
        out.extend_from_slice(&(frame.len() as u64).to_le_bytes());
        out.extend_from_slice(&(*raw_len as u64).to_le_bytes());
    }
    for (_, frame) in &frames {
        out.extend_from_slice(frame);
    }
    Ok(out)
}

/// Wraps one stream in a ZSTD frame: compressed with the `zstd` crate when the
/// `zstd-encode` feature is on (the default), otherwise as raw blocks.
fn compress_stream(raw: &[u8]) -> Result<Vec<u8>, SplatError> {
    #[cfg(feature = "zstd-encode")]
    {
        // Level 3 is zstd's default trade-off between speed and ratio.
        zstd::bulk::compress(raw, 3).map_err(|_| SplatError::Invalid("zstd compression failed"))
    }
    #[cfg(not(feature = "zstd-encode"))]
    {
        Ok(zstd_raw_frame(raw))
    }
}

/// Rounds half away from zero and clamps to `0..=255`.
fn to_u8(x: f32) -> u8 {
    x.round().clamp(0.0, 255.0) as u8
}

/// Smallest-three encoding of a quaternion given as `[x, y, z, w]`: 2 bits for
/// the largest component's index, then three 10-bit fields (sign and 9-bit
/// magnitude). The largest component is made positive first.
fn encode_smallest_three(q: [f32; 4]) -> [u8; 4] {
    let norm = q
        .iter()
        .map(|v| v * v)
        .sum::<f32>()
        .sqrt()
        .max(f32::MIN_POSITIVE);
    let q = q.map(|v| v / norm);
    let mut largest = 0;
    for i in 1..4 {
        if q[i].abs() > q[largest].abs() {
            largest = i;
        }
    }
    let negate = q[largest] < 0.0;
    let sqrt1_2 = std::f32::consts::FRAC_1_SQRT_2;
    let mut comp = largest as u32;
    for (i, &v) in q.iter().enumerate() {
        if i != largest {
            let negbit = u32::from((v < 0.0) ^ negate);
            let mag = ((511.0 * (v.abs() / sqrt1_2)) + 0.5).clamp(0.0, 511.0) as u32;
            comp = (comp << 10) | (negbit << 9) | mag;
        }
    }
    comp.to_le_bytes()
}

/// Wraps `data` in one ZSTD frame of raw blocks, with the content size stored.
#[cfg(any(test, not(feature = "zstd-encode")))]
fn zstd_raw_frame(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 32);
    out.extend_from_slice(&[0x28, 0xB5, 0x2F, 0xFD]); // ZSTD magic, little-endian
                                                      // Frame_Content_Size_flag = 3 (8 bytes), Single_Segment_flag = 1, no checksum.
    out.push(0xE0);
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    if data.is_empty() {
        // One empty last block, type Raw.
        out.extend_from_slice(&1u32.to_le_bytes()[..3]);
        return out;
    }
    let chunks: Vec<&[u8]> = data.chunks(RAW_BLOCK_MAX).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let last = u32::from(i + 1 == chunks.len());
        // Block header: bit 0 Last_Block, bits 1-2 Block_Type (0 = Raw), bits 3+ size.
        let header = last | ((chunk.len() as u32) << 3);
        out.extend_from_slice(&header.to_le_bytes()[..3]);
        out.extend_from_slice(chunk);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smallest_three_round_trips_through_reader_decode() {
        // Identity and a rotation with mixed signs.
        for q in [
            [0.0, 0.0, 0.0, 1.0],
            [0.6, -0.48, 0.36, -0.52],
            [-0.5, 0.5, -0.5, 0.5],
        ] {
            let norm = q.iter().map(|v| v * v).sum::<f32>().sqrt();
            let unit = q.map(|v| v / norm);
            let packed = encode_smallest_three(unit);
            let back = crate::spz::decode_smallest_three(&packed);
            // q and -q are the same rotation.
            let direct: f32 = (0..4)
                .map(|i| (back[i] - unit[i]).abs())
                .fold(0.0, f32::max);
            let flipped: f32 = (0..4)
                .map(|i| (back[i] + unit[i]).abs())
                .fold(0.0, f32::max);
            assert!(direct.min(flipped) < 2e-3, "{q:?} -> {back:?}");
        }
    }

    #[test]
    fn raw_frame_has_zstd_magic_and_size() {
        let frame = zstd_raw_frame(&[1, 2, 3]);
        assert_eq!(&frame[..4], &[0x28, 0xB5, 0x2F, 0xFD]);
        assert_eq!(u64::from_le_bytes(frame[5..13].try_into().unwrap()), 3);
    }
}
