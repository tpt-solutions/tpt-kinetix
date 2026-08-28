//! Parameter-vector codec (DECISION 3 implementation-order step 3).
//!
//! Carries the [`crate::FaceParams`] groups (DECISION 1) to the synthesizer. The
//! load-bearing structure: **identity is sent once per call (key frame);
//! per-frame payloads are expression/pose/illumination/appearance deltas** —
//! that delta coding is what makes the codec compress talking heads.
//!
//! Each group is rANS-coded as an independent sub-stream (reusing
//! [`tpt_kinetix_bitstream::RansStreamSet`], per DECISION 7). Deltas are
//! zero-centered and correlated, so they are coded through a **zero-biased
//! [`SymbolModel`]** ([`FaceCoefModel`]) that concentrates probability mass on
//! small magnitudes — the key compression win over a uniform model.
//!
//! Quantization is per-group (`group_qp[group]` from the sequence header, or a
//! per-frame override), scaled by `quant_precision` fractional bits. Coefficients
//! are clamped to the `i8` range `[-128, 127]` before zigzag-mapping to a `u8`
//! rANS symbol (the `tpt-kinetix-bitstream` coder is byte-symbol); deltas larger
//! than that are saturated (documented v1 limitation — a two-symbol escape is a
//! v2 extension).

use tpt_kinetix_bitstream::{
    RansDecoder, RansEncoder, RansStreamSet, SymbolInfo, SymbolModel, PROB_SCALE,
};
use tpt_kinetix_core::error::KinetixError;

use crate::header::{FaceFrameHeader, FaceSequenceHeader};

/// Errors from quantizing / rANS-coding the parameter vector.
#[derive(Debug, thiserror::Error)]
pub enum FaceParamError {
    /// Underlying rANS / stream-set error.
    #[error("face param: rANS error: {0}")]
    Rans(#[from] KinetixError),
    /// The framed payload held the wrong number of group sub-streams.
    #[error("face param: group count mismatch (expected {expected}, got {got})")]
    GroupCount { expected: usize, got: usize },
    /// An inter frame arrived without the identity vector it references.
    #[error("face param: identity missing for inter frame (key/setup must precede it)")]
    MissingIdentity,
    /// A group sub-stream was too short to even carry its length prefix.
    #[error("face param: truncated group payload")]
    Truncated,
}

/// Zero-biased symbol model for quantized coefficient deltas.
///
/// Symbols are zigzag-encoded signed integers (0 = 0, 1 = -1, 2 = +1, …), so a
/// zero-biased distribution concentrates mass on small magnitudes — exactly the
/// shape deltas have. Built once with a geometric decay over the 256 `u8`
/// symbols, normalized to sum exactly to [`PROB_SCALE`].
#[derive(Debug, Clone)]
pub struct FaceCoefModel {
    cum: Vec<u32>,
}

impl FaceCoefModel {
    /// Build the model with decay `r` (closer to 1 = stronger zero bias).
    pub fn new() -> Self {
        const N: usize = 256;
        let r = 0.97f64;
        let mut freq = [0u32; N];
        for (i, f) in freq.iter_mut().enumerate() {
            let p = (1.0 - r) * r.powi(i as i32);
            *f = (p * PROB_SCALE as f64).round().max(1.0) as u32;
        }
        // Normalize the total to exactly PROB_SCALE (all freqs stay >= 1).
        let mut sum: i64 = freq.iter().map(|&f| f as i64).sum();
        let mut i = 0;
        while sum != PROB_SCALE as i64 {
            if sum < PROB_SCALE as i64 {
                freq[i] += 1;
                sum += 1;
            } else if freq[i] > 1 {
                freq[i] -= 1;
                sum -= 1;
            }
            i = (i + 1) % N;
        }
        let mut cum = vec![0u32; N + 1];
        for i in 0..N {
            cum[i + 1] = cum[i] + freq[i];
        }
        Self { cum }
    }
}

impl Default for FaceCoefModel {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolModel for FaceCoefModel {
    fn info(&self, symbol: u8) -> SymbolInfo {
        let s = symbol as usize;
        SymbolInfo {
            start: self.cum[s],
            freq: self.cum[s + 1] - self.cum[s],
        }
    }

    fn find(&self, cum_freq: u32) -> (u8, SymbolInfo) {
        let mut lo = 0usize;
        let mut hi = 256usize;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.cum[mid] <= cum_freq {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let s = (lo - 1) as u8;
        (s, self.info(s))
    }
}

/// Zigzag-map a signed scalar to a `u8` rANS symbol (`[-128,127]` → `0..=255`).
fn encode_sym(v: i32) -> u8 {
    let v = v.clamp(-128, 127);
    if v >= 0 {
        ((v as u32) << 1) as u8
    } else {
        ((((-v) as u32) << 1) - 1) as u8
    }
}

/// Inverse of [`encode_sym`].
fn decode_sym(s: u8) -> i32 {
    let u = s as u32;
    if u & 1 == 0 {
        (u >> 1) as i32
    } else {
        let magnitude = (u >> 1) as i32;
        -(magnitude + 1)
    }
}

/// Per-group quantization step from `group_qp[group]` and `quant_precision`.
fn quant_step(qp: u8, precision: u8) -> f32 {
    2.0f32.powi(-(qp as i32) - (precision as i32))
}

/// Dequantize a clamped, zigzag-coded coefficient back to `f32`.
fn dequantize(q: i32, qp: u8, precision: u8) -> f32 {
    q as f32 * quant_step(qp, precision)
}

/// Quantize a `f32` coefficient, clamping to the `i8` symbol range.
fn quantize(v: f32, qp: u8, precision: u8) -> i32 {
    (v / quant_step(qp, precision)).round() as i32
}

fn encode_group(coeffs: &[f32], qp: u8, precision: u8, model: &dyn SymbolModel) -> Vec<u8> {
    let mut enc = RansEncoder::new();
    // rANS encodes back-to-front: push symbols in reverse of decode order.
    for &c in coeffs.iter().rev() {
        let q = quantize(c, qp, precision).clamp(-128, 127);
        enc.encode(model, encode_sym(q));
    }
    let mut rans = enc.finish();
    let mut out = Vec::with_capacity(2 + rans.len());
    out.extend_from_slice(&(coeffs.len() as u16).to_be_bytes());
    out.append(&mut rans);
    out
}

fn decode_group(
    payload: &[u8],
    qp: u8,
    precision: u8,
    model: &dyn SymbolModel,
) -> Result<Vec<f32>, FaceParamError> {
    if payload.len() < 2 {
        return Err(FaceParamError::Truncated);
    }
    let count = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let mut dec = RansDecoder::new(&payload[2..])?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let sym = dec.decode(model)?;
        out.push(dequantize(decode_sym(sym), qp, precision));
    }
    Ok(out)
}

/// Encodes / decodes a [`crate::FaceParams`] vector as the rANS-coded frame payload.
///
/// Group order on the wire is fixed: `[identity, expression, pose,
/// illumination, appearance]`. A key frame carries all five; an inter frame
/// carries the last four (identity is taken from the preceding key/setup).
pub struct FaceParamCodec {
    model: FaceCoefModel,
}

impl FaceParamCodec {
    /// Create a codec with the default zero-biased coefficient model.
    pub fn new() -> Self {
        Self {
            model: FaceCoefModel::new(),
        }
    }

    /// Encode one frame's parameter vector into the rANS payload bytes that
    /// follow a [`FaceFrameHeader`] (per DECISION 3 framing).
    pub fn encode_frame(
        &self,
        seq: &FaceSequenceHeader,
        header: &FaceFrameHeader,
        params: &crate::FaceParams,
    ) -> Result<Vec<u8>, FaceParamError> {
        let qp = header.group_qp_override.unwrap_or(seq.group_qp);
        // Each entry carries its own group-type index so the per-group qp is taken
        // from the correct slot of `group_qp` regardless of key/inter ordering.
        let groups: Vec<(&[f32], usize)> = if header.flags.inter {
            vec![
                (&params.expression, 1),
                (&params.pose, 2),
                (&params.illumination, 3),
                (&params.appearance, 4),
            ]
        } else {
            vec![
                (&params.identity, 0),
                (&params.expression, 1),
                (&params.pose, 2),
                (&params.illumination, 3),
                (&params.appearance, 4),
            ]
        };
        let mut streams = Vec::with_capacity(groups.len() + 1);
        for (g, gi) in groups {
            streams.push(encode_group(g, qp[gi], seq.quant_precision, &self.model));
        }
        // Landmark companion (DECISION 1): when enabled, append landmark deltas
        // as an additional sub-stream (N points × 2 i16 coordinates).
        if seq.flags.landmark_companion {
            streams.push(encode_landmarks(&params.landmarks));
        }
        Ok(RansStreamSet::frame(&streams)?)
    }

    /// Decode a frame's rANS payload back into a [`crate::FaceParams`].
    ///
    /// For an inter frame, `setup_identity` must supply the identity vector from
    /// the preceding key/setup frame; otherwise [`FaceParamError::MissingIdentity`]
    /// is returned.
    pub fn decode_frame(
        &self,
        seq: &FaceSequenceHeader,
        header: &FaceFrameHeader,
        setup_identity: Option<&[f32]>,
        payload: &[u8],
    ) -> Result<crate::FaceParams, FaceParamError> {
        let qp = header.group_qp_override.unwrap_or(seq.group_qp);
        let streams = RansStreamSet::unframe(payload)?;
        let group_count = if header.flags.inter { 4 } else { 5 };
        let expected = group_count + usize::from(seq.flags.landmark_companion);
        if streams.len() != expected {
            return Err(FaceParamError::GroupCount {
                expected,
                got: streams.len(),
            });
        }
        let mut idx = 0usize;
        let identity = if header.flags.inter {
            setup_identity
                .ok_or(FaceParamError::MissingIdentity)?
                .to_vec()
        } else {
            let g = decode_group(streams[idx], qp[0], seq.quant_precision, &self.model)?;
            idx += 1;
            g
        };
        let expression = decode_group(streams[idx], qp[1], seq.quant_precision, &self.model)?;
        idx += 1;
        let pose = decode_group(streams[idx], qp[2], seq.quant_precision, &self.model)?;
        idx += 1;
        let illumination = decode_group(streams[idx], qp[3], seq.quant_precision, &self.model)?;
        idx += 1;
        let appearance = decode_group(streams[idx], qp[4], seq.quant_precision, &self.model)?;

        let landmarks = if seq.flags.landmark_companion {
            decode_landmarks(streams[idx + 1])?
        } else {
            Vec::new()
        };

        Ok(crate::FaceParams {
            identity,
            expression,
            pose,
            illumination,
            appearance,
            landmarks,
        })
    }
}

/// Encode landmark companion data: count (u16 BE) + N×(x, y) as i16 BE deltas.
fn encode_landmarks(landmarks: &[(i16, i16)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + landmarks.len() * 4);
    out.extend_from_slice(&(landmarks.len() as u16).to_be_bytes());
    for &(x, y) in landmarks {
        out.extend_from_slice(&x.to_be_bytes());
        out.extend_from_slice(&y.to_be_bytes());
    }
    out
}

/// Decode landmark companion data.
fn decode_landmarks(data: &[u8]) -> Result<Vec<(i16, i16)>, FaceParamError> {
    if data.len() < 2 {
        return Err(FaceParamError::Truncated);
    }
    let count = u16::from_be_bytes([data[0], data[1]]) as usize;
    let expected = 2 + count * 4;
    if data.len() < expected {
        return Err(FaceParamError::Truncated);
    }
    let mut landmarks = Vec::with_capacity(count);
    for i in 0..count {
        let off = 2 + i * 4;
        let x = i16::from_be_bytes([data[off], data[off + 1]]);
        let y = i16::from_be_bytes([data[off + 2], data[off + 3]]);
        landmarks.push((x, y));
    }
    Ok(landmarks)
}

impl Default for FaceParamCodec {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FaceParams;

    fn seq() -> FaceSequenceHeader {
        FaceSequenceHeader {
            version: crate::FACE_VERSION,
            asset_basis_id: 0,
            basis_hash: [0; 8],
            max_width: 1280,
            max_height: 720,
            flags: crate::header::SequenceFlags::default(),
            quant_precision: 0,
            group_qp: [2, 3, 2, 2, 2],
        }
    }

    fn key_header() -> FaceFrameHeader {
        FaceFrameHeader {
            flags: crate::header::FrameFlags {
                inter: false,
                has_qp_override: false,
            },
            width: 1280,
            height: 720,
            ref_mode: 0,
            group_qp_override: None,
            payload_len: 0,
        }
    }

    fn inter_header() -> FaceFrameHeader {
        FaceFrameHeader {
            flags: crate::header::FrameFlags {
                inter: true,
                has_qp_override: false,
            },
            width: 1280,
            height: 720,
            ref_mode: 0,
            group_qp_override: None,
            payload_len: 0,
        }
    }

    fn sample_params() -> FaceParams {
        // Realistic, normalized 3DMM coefficient scales (kept within the i8 symbol
        // range after quantization so the round-trip is not dominated by clamping).
        let mk = |n: usize, s: f32| (0..n).map(|i| (i as f32 - n as f32 / 2.0) * s).collect();
        FaceParams {
            identity: mk(80, 0.01),
            expression: mk(50, 0.005),
            pose: vec![0.0, 0.005, -0.01, 0.1, -0.08, 0.03],
            illumination: mk(27, 0.005),
            appearance: mk(40, 0.008),
            ..Default::default()
        }
    }

    #[test]
    fn model_is_valid_distribution() {
        let m = FaceCoefModel::new();
        let total: u32 = (0..=255u8).map(|s| m.info(s).freq).sum();
        assert_eq!(total, PROB_SCALE);
        for s in 0..=255u8 {
            assert!(m.info(s).freq >= 1, "symbol {s} has zero freq");
        }
    }

    #[test]
    fn zigzag_is_bijective_in_range() {
        for v in -128..=127i32 {
            assert_eq!(decode_sym(encode_sym(v)), v);
        }
    }

    #[test]
    fn model_round_trips_every_symbol() {
        // Exercises the full frequency range — catches any find/info inversion
        // that the small round-trip test might miss.
        let model = FaceCoefModel::new();
        let symbols: Vec<u8> = (0..=255).collect();
        let mut enc = RansEncoder::new();
        for &s in symbols.iter().rev() {
            enc.encode(&model, s);
        }
        let bytes = enc.finish();
        let mut dec = RansDecoder::new(&bytes).expect("decoder init");
        for &expected in &symbols {
            assert_eq!(dec.decode(&model).expect("decode"), expected);
        }
    }

    #[test]
    fn key_frame_round_trips_within_quant_tolerance() {
        let codec = FaceParamCodec::new();
        let s = seq();
        let h = key_header();
        let p = sample_params();
        let payload = codec.encode_frame(&s, &h, &p).expect("encode");
        let decoded = codec.decode_frame(&s, &h, None, &payload).expect("decode");
        assert_eq!(decoded.identity.len(), p.identity.len());
        assert_eq!(decoded.expression.len(), p.expression.len());
        for (orig, got) in p.identity.iter().zip(decoded.identity.iter()) {
            assert!(
                (orig - got).abs() <= quant_step(s.group_qp[0], s.quant_precision) / 2.0 + 1e-4
            );
        }
    }

    #[test]
    fn inter_frame_uses_setup_identity() {
        let codec = FaceParamCodec::new();
        let s = seq();
        let key = key_header();
        let inter = inter_header();
        let p = sample_params();
        let key_payload = codec.encode_frame(&s, &key, &p).expect("encode key");
        let key_params = codec
            .decode_frame(&s, &key, None, &key_payload)
            .expect("decode key");

        // Re-encode the *same* params as an inter frame (deltas vs the key).
        let inter_payload = codec.encode_frame(&s, &inter, &p).expect("encode inter");
        let decoded = codec
            .decode_frame(&s, &inter, Some(&key_params.identity), &inter_payload)
            .expect("decode inter");
        assert_eq!(decoded.identity, key_params.identity);
        for (orig, got) in p.expression.iter().zip(decoded.expression.iter()) {
            assert!(
                (orig - got).abs() <= quant_step(s.group_qp[1], s.quant_precision) / 2.0 + 1e-4
            );
        }
    }

    #[test]
    fn inter_frame_without_identity_errors() {
        let codec = FaceParamCodec::new();
        let s = seq();
        let inter = inter_header();
        let p = sample_params();
        let payload = codec.encode_frame(&s, &inter, &p).expect("encode");
        assert!(matches!(
            codec.decode_frame(&s, &inter, None, &payload),
            Err(FaceParamError::MissingIdentity)
        ));
    }

    #[test]
    fn wrong_group_count_is_rejected() {
        let codec = FaceParamCodec::new();
        let s = seq();
        let key = key_header();
        let p = sample_params();
        let payload = codec.encode_frame(&s, &key, &p).expect("encode");
        // Claim it is an inter frame (expects 4 groups) but feed 5.
        assert!(matches!(
            codec.decode_frame(&s, &inter_header(), None, &payload),
            Err(FaceParamError::GroupCount { .. })
        ));
    }

    #[test]
    fn large_coefficient_saturates_i8_symbol_range() {
        // Documents the v1 limitation: quantized values are clamped to [-128,127]
        // (a single `u8` rANS symbol). A coefficient far outside that range after
        // quantization saturates predictably rather than overflowing.
        let codec = FaceParamCodec::new();
        let s = seq();
        let h = key_header();
        let p = FaceParams {
            identity: vec![100.0], // >> i8 range at qp=2 (step 0.25 → q=400)
            expression: vec![],
            pose: vec![],
            illumination: vec![],
            appearance: vec![],
            ..Default::default()
        };
        let payload = codec.encode_frame(&s, &h, &p).expect("encode");
        let decoded = codec.decode_frame(&s, &h, None, &payload).expect("decode");
        // 100.0 / 0.25 = 400 → clamped to 127 → dequantized to 127 * 0.25 = 31.75.
        assert_eq!(decoded.identity, vec![31.75]);
    }

    #[test]
    fn landmark_companion_round_trips() {
        let mut s = seq();
        s.flags.landmark_companion = true;
        let h = key_header();
        let p = FaceParams {
            identity: vec![0.1, 0.2],
            expression: vec![0.0],
            pose: vec![0.0; 6],
            illumination: vec![0.0; 9],
            appearance: vec![],
            landmarks: vec![(100, 200), (-50, 300), (0, 0), (1024, 768)],
        };
        let codec = FaceParamCodec::new();
        let payload = codec.encode_frame(&s, &h, &p).expect("encode");
        let decoded = codec.decode_frame(&s, &h, None, &payload).expect("decode");
        assert_eq!(decoded.landmarks, p.landmarks);
        assert_eq!(decoded.landmark_count(), 4);
    }

    #[test]
    fn landmark_absent_when_flag_unset() {
        let s = seq(); // landmark_companion = false
        let h = key_header();
        let p = FaceParams {
            identity: vec![0.1],
            expression: vec![],
            pose: vec![],
            illumination: vec![],
            appearance: vec![],
            ..Default::default()
        };
        let codec = FaceParamCodec::new();
        let payload = codec.encode_frame(&s, &h, &p).expect("encode");
        let decoded = codec.decode_frame(&s, &h, None, &payload).expect("decode");
        assert!(decoded.landmarks.is_empty());
    }
}
