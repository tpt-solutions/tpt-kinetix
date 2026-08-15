//! `tpt-kinetix-lossless` — a bit-exact **reversible** codec for high-bit-depth
//! still/frame data (medical imaging, scientific capture, archival).
//!
//! Unlike perceptual codecs (AV1/HEVC/H.264) or `tpt-kinetix-lean` (bounded-time
//! embedded *lossy* video), this codec has **no quality axis**: every preserved
//! bit is verified. The defining property is *guaranteed* lossless round-trip for
//! 10/12/16-bit samples, enforced by a built-in per-plane checksum (DECISION 3
//! of `docs/lossless-codec-design.md`), not just external testing.
//!
//! # v1 format (DECISION 2)
//!
//! The v1 transform is **predictive + entropy** (FFV1-like): each sample is
//! predicted from its reconstructed left/up/up-left neighbours via FFV1's median
//! predictor, the signed residual is rANS-coded with a per-context static
//! probability model (the same activity measure FFV1 uses for its `quant_table`,
//! shared from `tpt-kinetix-bitstream`), and every plane carries a CRC of its
//! reconstructed samples. A `transform_id` field reserves a reversible-wavelet
//! mode for a later phase without a format break.
//!
//! # Honesty contract
//!
//! `LosslessDecoder::capabilities()` reports `pixel_exact: true` for the v1
//! predictive transform — the pipeline is integer-reversible by construction and
//! verified by the round-trip tests below. If a stream carries a reserved
//! `transform_id` (future wavelet mode), the decoder returns
//! [`KinetixError::Unsupported`] instead of decoding to silent garbage.

pub mod crc;
pub mod entropy;
pub mod headers;
pub mod predict;

use tpt_kinetix_bitstream::bitreader::BitReader;
use tpt_kinetix_bitstream::lossless_context_models;
use tpt_kinetix_core::{capabilities::DecoderCapabilities, error::KinetixError};

use crate::{
    crc::checksum_plane,
    entropy::{decode_one_residual, encode_residual_stream, rice_k, BitWriter},
    headers::{FrameHeader, PlaneSpec, SequenceHeader},
    predict::predict,
};

/// Magic bytes for a lossless stream container.
pub const STREAM_MAGIC: &[u8; 4] = b"TKLS";

/// A single image plane: `width * height` `u16` samples, row-major.
///
/// `bit_depth` is the number of significant bits per sample (10/12/16 for v1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plane {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub data: Vec<u16>,
}

impl Plane {
    /// Validate dimensions/length and confirm the bit depth is supported in v1.
    fn validate(&self) -> Result<(), KinetixError> {
        if self.bit_depth != 10 && self.bit_depth != 12 && self.bit_depth != 16 {
            return Err(KinetixError::Unsupported(format!(
                "lossless: bit_depth {} not supported in v1 (need 10/12/16)",
                self.bit_depth
            )));
        }
        let expected = (self.width as usize) * (self.height as usize);
        if self.data.len() != expected {
            return Err(KinetixError::Parse(format!(
                "lossless: plane data length {} != expected {} ({}x{})",
                self.data.len(),
                expected,
                self.width,
                self.height
            )));
        }
        Ok(())
    }
}

/// Encode one plane's residuals into a byte buffer (header is written separately).
fn encode_plane(plane: &Plane) -> Result<Vec<u8>, KinetixError> {
    plane.validate()?;
    let pw = plane.width as usize;
    let ph = plane.height as usize;
    let mut residuals = Vec::with_capacity(pw * ph);
    let mut contexts = Vec::with_capacity(pw * ph);
    let mut up_mag = vec![0u32; pw];
    let mut left_mag: u32 = 0;
    for y in 0..ph {
        for (x, up) in up_mag.iter_mut().enumerate() {
            let pred = u32::from(predict(&plane.data, pw, ph, x, y));
            let sample = u32::from(plane.data[y * pw + x]);
            let residual = (sample as i32) - (pred as i32);
            let k = if x == 0 && y == 0 {
                0
            } else {
                rice_k(left_mag, *up)
            };
            residuals.push(residual);
            contexts.push(k as u8);
            let mag = residual.unsigned_abs();
            *up = mag;
            left_mag = mag;
        }
    }
    Ok(encode_residual_stream(&residuals, &contexts))
}

/// Decode one plane's residuals from the rANS `data` buffer, verifying the
/// per-plane checksum.
fn decode_plane(
    data: &[u8],
    seq: &SequenceHeader,
    spec: &PlaneSpec,
    width: u16,
    height: u16,
    expected_crc: &[u8],
) -> Result<Plane, KinetixError> {
    if spec.bit_depth != 10 && spec.bit_depth != 12 && spec.bit_depth != 16 {
        return Err(KinetixError::Unsupported(format!(
            "lossless: bit_depth {} not supported in v1",
            spec.bit_depth
        )));
    }
    let mask = (1u32 << spec.bit_depth) - 1;
    let w = width as usize;
    let h = height as usize;
    let mut samples = vec![0u16; w * h];
    let mut up_mag = vec![0u32; w];
    let mut left_mag: u32 = 0;
    let models = lossless_context_models();
    let mut dec = tpt_kinetix_bitstream::RansDecoder::new(data)
        .map_err(|e| KinetixError::Parse(format!("lossless: plane residual stream: {e}")))?;
    for y in 0..h {
        for x in 0..w {
            let pred = u32::from(predict(&samples, w, h, x, y));
            let k = if x == 0 && y == 0 {
                0
            } else {
                rice_k(left_mag, up_mag[x])
            };
            let residual = decode_one_residual(&mut dec, k as u8, &models)?;
            let sample = ((pred as i32 + residual) as u32 & mask) as u16;
            samples[y * w + x] = sample;
            let mag = residual.unsigned_abs();
            up_mag[x] = mag;
            left_mag = mag;
        }
    }
    let plane_index = seq.planes.iter().position(|p| p == spec).unwrap_or(0);
    let actual = checksum_plane(spec.bit_depth, &samples);
    if actual != expected_crc {
        return Err(KinetixError::Parse(format!(
            "lossless: reversibility check failed (plane {plane_index}): decoder output did not match embedded checksum"
        )));
    }
    Ok(Plane {
        width: u32::from(width),
        height: u32::from(height),
        bit_depth: spec.bit_depth,
        data: samples,
    })
}

/// Encoder for the lossless format.
pub struct LosslessEncoder;

impl LosslessEncoder {
    pub fn new() -> Self {
        Self
    }

    /// Encode one frame (a set of planes) into a self-contained byte payload.
    ///
    /// The payload is `[frame_header][plane_0_residuals]...[plane_n_residuals]`,
    /// where `frame_header` carries a checksum per plane.
    pub fn encode_frame(
        &self,
        seq: &SequenceHeader,
        planes: &[Plane],
    ) -> Result<Vec<u8>, KinetixError> {
        if planes.len() != seq.plane_count() {
            return Err(KinetixError::Parse(format!(
                "lossless: frame has {} planes but sequence declares {}",
                planes.len(),
                seq.plane_count()
            )));
        }
        let mut plane_payloads = Vec::with_capacity(planes.len());
        let mut crcs = Vec::with_capacity(planes.len());
        let mut lengths = Vec::with_capacity(planes.len());
        for (plane, spec) in planes.iter().zip(&seq.planes) {
            if plane.bit_depth != spec.bit_depth {
                return Err(KinetixError::Parse(
                    "lossless: plane bit_depth disagrees with sequence header".to_string(),
                ));
            }
            if plane.width as u16 > seq.max_width || plane.height as u16 > seq.max_height {
                return Err(KinetixError::Parse(
                    "lossless: frame exceeds sequence max dimensions".to_string(),
                ));
            }
            let bytes = encode_plane(plane)?;
            crcs.push(checksum_plane(spec.bit_depth, &plane.data));
            lengths.push(bytes.len() as u32);
            plane_payloads.push(bytes);
        }

        let mut header = BitWriter::new();
        FrameHeader {
            width: planes[0].width as u16,
            height: planes[0].height as u16,
            plane_checksums: crcs,
            plane_lengths: lengths,
        }
        .encode(&mut header);
        let mut out = header.finish();
        for p in plane_payloads {
            out.extend(p);
        }
        Ok(out)
    }
}

impl Default for LosslessEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Decoder for the lossless format.
pub struct LosslessDecoder {
    strict: bool,
}

impl LosslessDecoder {
    /// Create a decoder in non-strict mode.
    pub fn new() -> Self {
        Self { strict: false }
    }

    /// Enable strict mode (currently no behavioural difference for the v1
    /// predictive transform, which is always bit-exact; reserved for future
    /// modes that may degrade gracefully).
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Report decoder capabilities. `pixel_exact` is `true` for the v1
    /// predictive transform.
    pub fn capabilities(&self) -> DecoderCapabilities {
        DecoderCapabilities {
            codec: "lossless",
            pixel_exact: true,
            supports_cabac: false,
            supports_cavlc: false,
            supports_intra_prediction: true,
            supports_inter_prediction: false,
            supports_deblocking: false,
            notes: "v1 reversible predictive + rANS entropy (per-context static model from \
                    tpt-kinetix-bitstream); bit-exact by construction; wavelet mode reserved via \
                    transform_id (not yet implemented)",
        }
    }

    /// Decode one frame payload previously produced by
    /// [`LosslessEncoder::encode_frame`].
    pub fn decode_frame(
        &mut self,
        seq: &SequenceHeader,
        data: &[u8],
    ) -> Result<Vec<Plane>, KinetixError> {
        if seq.transform_id != 0 {
            return Err(KinetixError::Unsupported(format!(
                "lossless: reserved transform_id {} not implemented in v1",
                seq.transform_id
            )));
        }
        let _ = self.strict;
        let mut r = BitReader::new(data);
        let fh = FrameHeader::decode(&mut r)
            .ok_or_else(|| KinetixError::Parse("lossless: truncated frame header".to_string()))?;
        if fh.plane_checksums.len() != seq.plane_count() {
            return Err(KinetixError::Parse(
                "lossless: frame header plane count mismatch".to_string(),
            ));
        }
        if fh.plane_lengths.len() != seq.plane_count() {
            return Err(KinetixError::Parse(
                "lossless: frame header plane count mismatch".to_string(),
            ));
        }
        // The frame body is the byte range after the (byte-aligned) header. Each
        // plane payload is byte-aligned and decoded from its own reader so one
        // plane's padding bits cannot leak into the next.
        let header_len = {
            let mut hw = BitWriter::new();
            fh.encode(&mut hw);
            hw.finish().len()
        };
        let body = &data[header_len..];
        let mut pos = 0usize;
        let mut planes = Vec::with_capacity(seq.plane_count());
        for (idx, (spec, crc)) in seq.planes.iter().zip(&fh.plane_checksums).enumerate() {
            let len = fh.plane_lengths[idx] as usize;
            let slice = body.get(pos..pos + len).ok_or_else(|| {
                KinetixError::Parse("lossless: truncated plane payload".to_string())
            })?;
            planes.push(decode_plane(slice, seq, spec, fh.width, fh.height, crc)?);
            pos += len;
        }
        Ok(planes)
    }
}

impl Default for LosslessDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq_for(planes: &[u8]) -> SequenceHeader {
        SequenceHeader {
            version: 1,
            max_width: 64,
            max_height: 64,
            transform_id: 0,
            planes: planes.iter().map(|&b| PlaneSpec { bit_depth: b }).collect(),
        }
    }

    fn roundtrip(bit_depth: u8, w: u32, h: u32, data: Vec<u16>) {
        let seq = seq_for(&[bit_depth]);
        let plane = Plane {
            width: w,
            height: h,
            bit_depth,
            data,
        };
        let enc = LosslessEncoder::new();
        let bytes = enc
            .encode_frame(&seq, std::slice::from_ref(&plane))
            .unwrap();
        let mut dec = LosslessDecoder::new();
        let out = dec.decode_frame(&seq, &bytes).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0], plane,
            "round-trip mismatch at bit_depth {bit_depth}"
        );
    }

    #[test]
    fn roundtrip_10bit_ramp() {
        let n = 32 * 32;
        let data: Vec<u16> = (0..n).map(|i| (i % 1024) as u16).collect();
        roundtrip(10, 32, 32, data);
    }

    #[test]
    fn roundtrip_12bit_random() {
        let n = 16 * 16;
        let data: Vec<u16> = (0..n)
            .map(|i| ((i * 2654435761usize) % 4096) as u16)
            .collect();
        roundtrip(12, 16, 16, data);
    }

    #[test]
    fn roundtrip_16bit_patterns() {
        let n = 24 * 24;
        let data: Vec<u16> = (0..n)
            .map(|i| {
                let x = i % 24;
                let y = i / 24;
                ((x ^ y) as u16) * 257
            })
            .collect();
        roundtrip(16, 24, 24, data);
    }

    #[test]
    fn roundtrip_multiplane_rgb16() {
        let seq = seq_for(&[16, 16, 16]);
        let mk = |seed: u16| {
            (0..16 * 16)
                .map(|i| (((i as u32) * (seed as u32)) % 65536) as u16)
                .collect()
        };
        let planes = vec![
            Plane {
                width: 16,
                height: 16,
                bit_depth: 16,
                data: mk(1),
            },
            Plane {
                width: 16,
                height: 16,
                bit_depth: 16,
                data: mk(7),
            },
            Plane {
                width: 16,
                height: 16,
                bit_depth: 16,
                data: mk(13),
            },
        ];
        let enc = LosslessEncoder::new();
        let bytes = enc.encode_frame(&seq, &planes).unwrap();
        let mut dec = LosslessDecoder::new();
        let out = dec.decode_frame(&seq, &bytes).unwrap();
        assert_eq!(out, planes);
    }

    #[test]
    fn corrupted_payload_fails_reversibility() {
        let seq = seq_for(&[16]);
        let data: Vec<u16> = (0..16 * 16).map(|i| (i % 65536) as u16).collect();
        let plane = Plane {
            width: 16,
            height: 16,
            bit_depth: 16,
            data,
        };
        let enc = LosslessEncoder::new();
        let mut bytes = enc.encode_frame(&seq, &[plane]).unwrap();
        // Flip a residual byte: checksum must catch the mismatch.
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        let mut dec = LosslessDecoder::new();
        assert!(dec.decode_frame(&seq, &bytes).is_err());
    }

    #[test]
    fn reserved_transform_id_rejected() {
        let mut seq = seq_for(&[16]);
        seq.transform_id = 1;
        let mut dec = LosslessDecoder::new();
        let data: Vec<u16> = vec![0; 4];
        let plane = Plane {
            width: 2,
            height: 2,
            bit_depth: 16,
            data,
        };
        let enc = LosslessEncoder::new();
        let bytes = enc.encode_frame(&seq, &[plane]).unwrap();
        // encode_frame doesn't validate transform_id, but decode must reject it.
        let res = dec.decode_frame(&seq, &bytes);
        assert!(matches!(res, Err(KinetixError::Unsupported(_))));
    }
}
