//! Entropy-coding helpers for the volumetric codec.
//!
//! The volumetric codec reuses the rANS primitives from `tpt-kinetix-bitstream`
//! (DECISION 7) but needs **context-modeled** symbol models, which that crate
//! does not yet provide (its `SymbolModel` is a single uniform table). This
//! module supplies the missing *context-indexed* models the geometry/attribute
//! coders need, built on top of the shared [`RansEncoder`]/[`RansDecoder`] —
//! keeping the actual entropy primitive in `tpt-kinetix-bitstream`.

use tpt_kinetix_bitstream::{RansDecoder, RansEncoder, StaticModel, SymbolInfo, SymbolModel};
use tpt_kinetix_core::error::KinetixError;

/// Precision of every frequency table: symbols must sum to `1 << PROB_BITS`
/// exactly. Must match `tpt-kinetix-bitstream`'s `PROB_BITS` (12).
const PROB_BITS: u32 = 12;
const PROB_SCALE: u32 = 1 << PROB_BITS;

/// A fixed binary model: symbol `0` gets `PROB_SCALE - freq1`, symbol `1`
/// gets `freq1`. Fixed (non-adaptive) so it can be shared by the
/// reverse-order rANS encoder and the forward-order decoder without tracking
/// model state per symbol.
pub struct FixedBinaryModel {
    freq1: u32,
}

impl FixedBinaryModel {
    /// Create a binary model with the given probability mass on symbol `1`.
    ///
    /// `freq1` is clamped to `(1, PROB_SCALE)` so neither symbol becomes
    /// unencodable.
    pub fn new(freq1: u32) -> Self {
        let f = freq1.clamp(1, PROB_SCALE - 1);
        Self { freq1: f }
    }
}

impl SymbolModel for FixedBinaryModel {
    fn info(&self, symbol: u8) -> SymbolInfo {
        if symbol == 0 {
            SymbolInfo {
                start: 0,
                freq: PROB_SCALE - self.freq1,
            }
        } else {
            SymbolInfo {
                start: PROB_SCALE - self.freq1,
                freq: self.freq1,
            }
        }
    }

    fn find(&self, cum_freq: u32) -> (u8, SymbolInfo) {
        if cum_freq < PROB_SCALE - self.freq1 {
            (0, self.info(0))
        } else {
            (1, self.info(1))
        }
    }
}

/// A bank of [`FixedBinaryModel`]s indexed by context id.
///
/// The occupancy coder holds `1 << 8` contexts (one per possible parent
/// occupancy pattern). [`Self::model`] hands back the model for a context.
pub struct BinaryCtxModels {
    models: Vec<FixedBinaryModel>,
}

impl BinaryCtxModels {
    /// Build `count` identical binary models, each with `freq1` mass on `1`.
    pub fn new(count: usize, freq1: u32) -> Self {
        Self {
            models: (0..count).map(|_| FixedBinaryModel::new(freq1)).collect(),
        }
    }

    /// Model for context `ctx` (clamped to the bank's range).
    pub fn model(&self, ctx: usize) -> &FixedBinaryModel {
        &self.models[ctx.min(self.models.len() - 1)]
    }
}

/// Encode a sequence of `(context, bit)` occupancy symbols with a context
/// model bank, returning the rANS byte stream (reverse decode order).
pub fn encode_bits_rev(syms: &[(usize, u8)], models: &BinaryCtxModels) -> Vec<u8> {
    let mut enc = RansEncoder::new();
    for (ctx, bit) in syms.iter().rev() {
        enc.encode(models.model(*ctx), *bit);
    }
    enc.finish()
}

/// Encode a sequence of unsigned symbols with the uniform [`StaticModel`].
pub fn encode_symbols_rev(syms: &[u8]) -> Vec<u8> {
    let model = StaticModel;
    let mut enc = RansEncoder::new();
    for &s in syms.iter().rev() {
        enc.encode(&model, s);
    }
    enc.finish()
}

/// Decode `n` occupancy bits, each drawn from the model for the supplied
/// context. `contexts[i]` is the context for bit `i`.
pub fn decode_bits(
    data: &[u8],
    contexts: &[usize],
    models: &BinaryCtxModels,
) -> Result<Vec<u8>, KinetixError> {
    let mut dec = RansDecoder::new(data)?;
    let mut out = Vec::with_capacity(contexts.len());
    for &ctx in contexts {
        out.push(dec.decode(models.model(ctx))?);
    }
    Ok(out)
}

/// Decode `n` uniform-model symbols.
pub fn decode_symbols(data: &[u8], n: usize) -> Result<Vec<u8>, KinetixError> {
    let mut dec = RansDecoder::new(data)?;
    let mut out = Vec::with_capacity(n);
    let model = StaticModel;
    for _ in 0..n {
        out.push(dec.decode(&model)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_model_round_trips() {
        let models = BinaryCtxModels::new(256, PROB_SCALE / 4);
        let bits: Vec<(usize, u8)> = (0..200)
            .map(|i| ((i * 7) % 256, (i % 3 == 0) as u8))
            .collect();
        let bytes = encode_bits_rev(&bits, &models);
        let decoded = decode_bits(
            &bytes,
            &bits.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
            &models,
        )
        .expect("decode");
        assert_eq!(decoded, bits.iter().map(|(_, b)| *b).collect::<Vec<_>>());
    }

    #[test]
    fn symbols_round_trip() {
        let syms: Vec<u8> = (0..128u32).map(|i| (i * 3 % 256) as u8).collect();
        let bytes = encode_symbols_rev(&syms);
        assert_eq!(decode_symbols(&bytes, syms.len()).expect("decode"), syms);
    }

    #[test]
    fn i16_residual_round_trips() {
        // Attribute residuals span the full signed 16-bit range; the coder
        // stores them as two rANS symbols so nothing is clamped.
        let residuals: Vec<i16> = vec![-300, -1, 0, 1, 255, 1000, -32768, 32767];
        let mut syms: Vec<u8> = Vec::new();
        for r in &residuals {
            syms.extend_from_slice(&r.to_le_bytes());
        }
        let bytes = encode_symbols_rev(&syms);
        let decoded = decode_symbols(&bytes, syms.len()).expect("decode");
        let mut out = Vec::new();
        for chunk in decoded.chunks_exact(2) {
            out.push(i16::from_le_bytes([chunk[0], chunk[1]]));
        }
        assert_eq!(out, residuals);
    }
}
