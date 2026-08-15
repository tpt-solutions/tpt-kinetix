//! Bit-level I/O and the reversible entropy stage for the lossless codec.
//!
//! The encode side uses a local MSB-first [`BitWriter`] for the *headers* (the
//! same framing `tpt-kinetix-bitstream`'s `BitReader` decodes). The *residual*
//! stream uses the shared rANS primitives (DECISION 6 of the design doc: reuse
//! `tpt-kinetix-bitstream`'s `RansEncoder`/`RansDecoder` rather than a
//! hand-rolled coder), with a per-context static probability model
//! ([`lossless_context_models`]) selected by the same activity measure FFV1
//! uses for its `quant_table`. Because rANS encodes symbols in reverse decode
//! order, the model must be *static* (not updated per symbol) so the forward
//! decoder and reverse encoder agree — see `tpt-kinetix-bitstream`'s
//! `SkewedModel`.

use tpt_kinetix_bitstream::bitreader::BitReader;
use tpt_kinetix_bitstream::{lossless_context_models, RansDecoder, RansEncoder, SkewedModel};

/// MSB-first bit writer. Bytes are filled from the most-significant bit; the
/// final partial byte is zero-padded on [`BitWriter::finish`].
pub struct BitWriter {
    buf: Vec<u8>,
    cur: u8,
    nbits: u8,
}

impl BitWriter {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            cur: 0,
            nbits: 0,
        }
    }

    pub fn write_bit(&mut self, bit: u8) {
        self.cur = (self.cur << 1) | (bit & 1);
        self.nbits += 1;
        if self.nbits == 8 {
            self.buf.push(self.cur);
            self.cur = 0;
            self.nbits = 0;
        }
    }

    pub fn write_bits(&mut self, value: u32, n: u8) {
        for i in (0..n).rev() {
            self.write_bit(((value >> i) & 1) as u8);
        }
    }

    /// Pad the current byte with zero bits and return the encoded buffer.
    pub fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            self.cur <<= 8 - self.nbits;
            self.buf.push(self.cur);
            self.cur = 0;
            self.nbits = 0;
        }
        self.buf
    }
}

impl Default for BitWriter {
    fn default() -> Self {
        Self::new()
    }
}

/// Number of significant bits in `v` (0 for v == 0).
fn bits(v: u32) -> u32 {
    if v == 0 {
        0
    } else {
        32 - v.leading_zeros()
    }
}

/// Adaptive Rice parameter from the left/up residual magnitudes.
///
/// `k` tracks the local activity so small residuals (flat/near-predictable
/// regions) use a small `k` and large residuals use a larger one. Clamped to
/// `[0, 15]`.
pub fn rice_k(left_mag: u32, up_mag: u32) -> u32 {
    let ctx = (bits(left_mag) + bits(up_mag)).div_ceil(2);
    ctx.min(15)
}

/// Map a signed residual to a non-negative codeword operand.
fn map_residual(r: i32) -> u32 {
    if r >= 0 {
        (r as u32) << 1
    } else {
        ((-r as u32) << 1) - 1
    }
}

/// Invert [`map_residual`].
fn unmap_residual(m: u32) -> i32 {
    if m & 1 == 0 {
        (m >> 1) as i32
    } else {
        -(((m + 1) >> 1) as i32)
    }
}

/// Read `n` bits (MSB-first) as a `u8`.
pub fn read_bits_u8(r: &mut BitReader<'_>, n: u8) -> Option<u8> {
    r.read_bits(n).map(|v| v as u8)
}

/// Read `n` bits (MSB-first) as a `u16`.
pub fn read_bits_u16(r: &mut BitReader<'_>, n: u8) -> Option<u16> {
    r.read_bits(n).map(|v| v as u16)
}

/// Encode a plane's folded residuals as a single reverse-ordered rANS stream.
///
/// `residuals[i]` is the signed residual and `contexts[i]` its activity context
/// (the same `rice_k` measure FFV1 uses). Each folded residual `m` is split into
/// three bytes `(m & 0xFF, m>>8 & 0xFF, m>>16 & 0xFF)` and coded with the
/// static model for that context. The encoder pushes in reverse so the decoder
/// reconstructs symbols in the original order.
pub fn encode_residual_stream(residuals: &[i32], contexts: &[u8]) -> Vec<u8> {
    debug_assert_eq!(residuals.len(), contexts.len());
    let models = lossless_context_models();
    let mut enc = RansEncoder::new();
    for i in (0..residuals.len()).rev() {
        let m = map_residual(residuals[i]) as u32;
        let model = &models[contexts[i] as usize];
        enc.encode(model, (m >> 16) as u8);
        enc.encode(model, ((m >> 8) & 0xFF) as u8);
        enc.encode(model, (m & 0xFF) as u8);
    }
    enc.finish()
}

/// Decode one folded residual from `dec` using the static model for `ctx`.
pub fn decode_one_residual(
    dec: &mut RansDecoder<'_>,
    ctx: u8,
    models: &[SkewedModel],
) -> Result<i32, tpt_kinetix_core::error::KinetixError> {
    let model = &models[ctx as usize];
    let b0 = dec.decode(model)?;
    let b1 = dec.decode(model)?;
    let b2 = dec.decode(model)?;
    let m = u32::from(b0) | (u32::from(b1) << 8) | (u32::from(b2) << 16);
    Ok(unmap_residual(m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_kinetix_core::error::KinetixError;

    #[test]
    fn residual_stream_round_trips() {
        let residuals: Vec<i32> = vec![
            0, 1, -1, 2, -2, 255, -255, 4096, -4096, 32767, -32768, 65535, -65535, 131070,
        ];
        let contexts: Vec<u8> = (0..residuals.len() as u8)
            .map(|i| (i % 16) as u8)
            .collect();
        let bytes = encode_residual_stream(&residuals, &contexts);
        let models = lossless_context_models();
        let mut dec = RansDecoder::new(&bytes).expect("decoder init");
        let mut out = Vec::with_capacity(residuals.len());
        for &ctx in &contexts {
            out.push(decode_one_residual(&mut dec, ctx, &models).expect("decode"));
        }
        assert_eq!(out, residuals);
    }

    #[test]
    fn residual_stream_rejects_truncated_input() {
        let residuals = [1000i32, -2000, 42];
        let contexts = [3u8, 7, 1];
        let mut bytes = encode_residual_stream(&residuals, &contexts);
        bytes.truncate(bytes.len() / 2); // drop half the stream
        let models = lossless_context_models();
        let mut dec = RansDecoder::new(&bytes).expect("decoder init");
        let res = decode_one_residual(&mut dec, contexts[0], &models);
        assert!(matches!(res, Err(KinetixError::Parse(_))));
    }

    #[test]
    fn bitwriter_msb_alignment() {
        let mut w = BitWriter::new();
        w.write_bits(0b1011, 4);
        w.write_bits(0b01, 2);
        let bytes = w.finish();
        // 0b101101 + 00 pad = 0b1011_0100 = 0xB4
        assert_eq!(bytes, vec![0xB4]);
    }
}
