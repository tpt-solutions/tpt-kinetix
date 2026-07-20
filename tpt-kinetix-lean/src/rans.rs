//! rANS entropy coding.
//!
//! # Why rANS instead of CABAC
//!
//! CABAC (used by H.264/HEVC) is a bit-serial adaptive binary arithmetic
//! coder: decoding bit *N* requires the updated probability state from bit
//! *N-1*, so a slice's coefficient data cannot be decoded except strictly in
//! order on one core. rANS (asymmetric numeral systems) reaches near-entropy
//! efficiency like arithmetic coding, but a stream can be split into
//! independent interleaved sub-streams that decode with no cross-stream
//! dependency — see [`RansStreamSet`] — which is what actually buys
//! multicore/SIMD entropy decode.
//!
//! # What's implemented here
//!
//! The core byte-renormalizing rANS primitives ([`RansEncoder`],
//! [`RansDecoder`]) are real and round-trip-tested against a uniform
//! [`StaticModel`]. What's *not* implemented yet is the actual per-symbol
//! probability model Lean's coefficient/mode coding will use (context
//! selection, adaptive frequency tables) — that's the extension point
//! [`SymbolModel`], stubbed here with only the uniform byte model as a
//! concrete (but not yet useful for real compression) implementation.

use tpt_kinetix_core::error::KinetixError;

/// Precision of the frequency table: all symbol frequencies for a given
/// model must sum to exactly `1 << PROB_BITS`.
const PROB_BITS: u32 = 12;
const PROB_SCALE: u32 = 1 << PROB_BITS;

/// `RANS_BYTE_L`: the renormalization lower bound (standard byte-oriented
/// rANS constant, per Fabian Giesen's `ryg_rans`).
const RANS_BYTE_L: u32 = 1 << 23;

/// A symbol's slot within the cumulative-frequency table: `[start, start +
/// freq)` out of `1 << PROB_BITS` total slots.
#[derive(Debug, Clone, Copy)]
pub struct SymbolInfo {
    pub start: u32,
    pub freq: u32,
}

/// Maps symbols to their frequency-table slot.
///
/// This is the extension point for real coefficient/mode coding: a context-
/// adaptive model (updating frequencies as symbols are seen, or selecting a
/// table by neighbouring-block context) would implement this trait. Only a
/// fixed uniform model exists today.
pub trait SymbolModel {
    /// Look up a symbol's `(start, freq)` slot.
    fn info(&self, symbol: u8) -> SymbolInfo;

    /// Invert a cumulative-frequency value (`0..PROB_SCALE`) back to the
    /// symbol occupying that slot, plus its `(start, freq)`.
    fn find(&self, cum_freq: u32) -> (u8, SymbolInfo);
}

/// A uniform byte model: all 256 symbols share equal probability.
///
/// This exists to make the rANS primitives round-trip-testable now; it is
/// not a useful compression model (real coefficient coding needs a skewed,
/// context-adaptive table) — replacing it is follow-up work, not part of
/// this scaffold.
pub struct StaticModel;

impl SymbolModel for StaticModel {
    fn info(&self, symbol: u8) -> SymbolInfo {
        let freq = PROB_SCALE / 256;
        SymbolInfo {
            start: freq * symbol as u32,
            freq,
        }
    }

    fn find(&self, cum_freq: u32) -> (u8, SymbolInfo) {
        let freq = PROB_SCALE / 256;
        let symbol = (cum_freq / freq).min(255) as u8;
        (symbol, self.info(symbol))
    }
}

/// Byte-oriented rANS encoder.
///
/// Symbols must be pushed in the **reverse** of decode order — this is a
/// structural property of rANS, not a bug: the decoder reconstructs symbols
/// in the same order they were encoded, and rANS encode naturally runs
/// back-to-front.
pub struct RansEncoder {
    state: u32,
    /// Encoded bytes, built up in reverse order internally, flipped to
    /// forward order by [`Self::finish`].
    out_rev: Vec<u8>,
}

impl RansEncoder {
    pub fn new() -> Self {
        Self {
            state: RANS_BYTE_L,
            out_rev: Vec::new(),
        }
    }

    /// Encode one symbol (call in reverse of intended decode order).
    pub fn encode(&mut self, model: &dyn SymbolModel, symbol: u8) {
        let SymbolInfo { start, freq } = model.info(symbol);
        debug_assert!(freq > 0, "symbol with zero frequency is unencodable");

        // Renormalize: emit bytes until state is small enough that encoding
        // this symbol can't push it past the u32 range.
        let freq_max = (RANS_BYTE_L >> PROB_BITS) << 8;
        let x_max = freq_max * freq;
        while self.state >= x_max {
            self.out_rev.push((self.state & 0xFF) as u8);
            self.state >>= 8;
        }

        // C(x, s) = (x / freq) * PROB_SCALE + (x % freq) + start
        self.state = (self.state / freq) * PROB_SCALE + (self.state % freq) + start;
    }

    /// Finalize the stream, returning the encoded bytes (forward order,
    /// including the final state so the decoder can be initialized).
    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..4 {
            self.out_rev.push((self.state & 0xFF) as u8);
            self.state >>= 8;
        }
        self.out_rev.reverse();
        self.out_rev
    }
}

impl Default for RansEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Byte-oriented rANS decoder, the inverse of [`RansEncoder`].
pub struct RansDecoder<'a> {
    state: u32,
    data: &'a [u8],
    pos: usize,
}

impl<'a> RansDecoder<'a> {
    /// Create a decoder over `data` as produced by [`RansEncoder::finish`].
    pub fn new(data: &'a [u8]) -> Result<Self, KinetixError> {
        if data.len() < 4 {
            return Err(KinetixError::Parse(
                "rANS stream shorter than the 4-byte initial state".into(),
            ));
        }
        let state = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        Ok(Self {
            state,
            data,
            pos: 4,
        })
    }

    /// Decode one symbol.
    pub fn decode(&mut self, model: &dyn SymbolModel) -> Result<u8, KinetixError> {
        let cum_freq = self.state & (PROB_SCALE - 1);
        let (symbol, SymbolInfo { start, freq }) = model.find(cum_freq);

        // D(x) = freq * (x >> PROB_BITS) + (x & (PROB_SCALE-1)) - start
        self.state = freq * (self.state >> PROB_BITS) + cum_freq - start;

        while self.state < RANS_BYTE_L {
            let byte = *self.data.get(self.pos).ok_or_else(|| {
                KinetixError::Parse("rANS stream exhausted during renormalization".into())
            })?;
            self.state = (self.state << 8) | byte as u32;
            self.pos += 1;
        }

        Ok(symbol)
    }
}

/// Frames `n` independently-decodable rANS sub-streams into a single byte
/// buffer, and splits one back apart.
///
/// This is the mechanism that makes Lean's entropy stage parallel: each
/// sub-stream is a self-contained rANS-coded byte range with no dependency
/// on the others, so a decoder can hand each range to a separate
/// thread/SIMD lane. The sequence header's `num_rans_streams` field (see
/// [`crate::headers::SequenceHeader`]) declares how many sub-streams a
/// conforming decoder should expect per frame.
///
/// # Wire format
///
/// `[stream_count: u8][len_0: u32 BE]...[len_{n-1}: u32 BE][stream_0 bytes]...[stream_{n-1} bytes]`
pub struct RansStreamSet;

impl RansStreamSet {
    /// Frame independently-encoded streams into one buffer.
    pub fn frame(streams: &[Vec<u8>]) -> Result<Vec<u8>, KinetixError> {
        if streams.len() > u8::MAX as usize {
            return Err(KinetixError::Parse(format!(
                "too many rANS sub-streams: {} (max {})",
                streams.len(),
                u8::MAX
            )));
        }
        let mut out =
            Vec::with_capacity(1 + streams.len() * 4 + streams.iter().map(Vec::len).sum::<usize>());
        out.push(streams.len() as u8);
        for s in streams {
            out.extend_from_slice(&(s.len() as u32).to_be_bytes());
        }
        for s in streams {
            out.extend_from_slice(s);
        }
        Ok(out)
    }

    /// Split a framed buffer back into its independent sub-stream byte
    /// ranges, each of which can be handed to a separate [`RansDecoder`].
    pub fn unframe(data: &[u8]) -> Result<Vec<&[u8]>, KinetixError> {
        let count = *data
            .first()
            .ok_or_else(|| KinetixError::Parse("rANS stream set: empty buffer".into()))?
            as usize;
        let header_len = 1 + count * 4;
        if data.len() < header_len {
            return Err(KinetixError::Parse(
                "rANS stream set: truncated length table".into(),
            ));
        }
        let mut lens = Vec::with_capacity(count);
        for i in 0..count {
            let off = 1 + i * 4;
            lens.push(
                u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
                    as usize,
            );
        }
        let mut streams = Vec::with_capacity(count);
        let mut pos = header_len;
        for len in lens {
            let end = pos
                .checked_add(len)
                .filter(|&end| end <= data.len())
                .ok_or_else(|| {
                    KinetixError::Parse("rANS stream set: sub-stream length overruns buffer".into())
                })?;
            streams.push(&data[pos..end]);
            pos = end;
        }
        Ok(streams)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_symbol_round_trips() {
        let model = StaticModel;
        let mut enc = RansEncoder::new();
        enc.encode(&model, 42);
        let bytes = enc.finish();

        let mut dec = RansDecoder::new(&bytes).expect("decoder init");
        assert_eq!(dec.decode(&model).expect("decode"), 42);
    }

    #[test]
    fn symbol_sequence_round_trips() {
        let model = StaticModel;
        let symbols: Vec<u8> = (0..64).map(|i| ((i * 37) % 256) as u8).collect();

        let mut enc = RansEncoder::new();
        // rANS encodes back-to-front: push in reverse so decode order matches.
        for &s in symbols.iter().rev() {
            enc.encode(&model, s);
        }
        let bytes = enc.finish();

        let mut dec = RansDecoder::new(&bytes).expect("decoder init");
        let decoded: Vec<u8> = (0..symbols.len())
            .map(|_| dec.decode(&model).expect("decode"))
            .collect();
        assert_eq!(decoded, symbols);
    }

    #[test]
    fn decoder_rejects_short_stream() {
        assert!(RansDecoder::new(&[0, 1, 2]).is_err());
    }

    #[test]
    fn stream_set_frames_and_unframes() {
        let streams = vec![vec![1u8, 2, 3], vec![4u8, 5], vec![]];
        let framed = RansStreamSet::frame(&streams).expect("frame");
        let unframed = RansStreamSet::unframe(&framed).expect("unframe");
        assert_eq!(unframed, vec![&[1u8, 2, 3][..], &[4u8, 5][..], &[][..]]);
    }

    #[test]
    fn stream_set_rejects_truncated_buffer() {
        assert!(RansStreamSet::unframe(&[2, 0, 0, 0, 5]).is_err());
    }

    #[test]
    fn independent_substreams_decode_without_cross_dependency() {
        // Encode two unrelated symbol sequences into separate rANS streams,
        // frame them together, then decode each sub-stream range on its
        // own — demonstrating there is no shared decode state between them
        // (the property that makes this parallelizable).
        let model = StaticModel;
        let seq_a: Vec<u8> = vec![10, 20, 30];
        let seq_b: Vec<u8> = vec![200, 201];

        let mut enc_a = RansEncoder::new();
        for &s in seq_a.iter().rev() {
            enc_a.encode(&model, s);
        }
        let mut enc_b = RansEncoder::new();
        for &s in seq_b.iter().rev() {
            enc_b.encode(&model, s);
        }

        let framed = RansStreamSet::frame(&[enc_a.finish(), enc_b.finish()]).expect("frame");
        let unframed = RansStreamSet::unframe(&framed).expect("unframe");

        let mut dec_a = RansDecoder::new(unframed[0]).expect("dec a");
        let decoded_a: Vec<u8> = (0..seq_a.len())
            .map(|_| dec_a.decode(&model).unwrap())
            .collect();
        assert_eq!(decoded_a, seq_a);

        let mut dec_b = RansDecoder::new(unframed[1]).expect("dec b");
        let decoded_b: Vec<u8> = (0..seq_b.len())
            .map(|_| dec_b.decode(&model).unwrap())
            .collect();
        assert_eq!(decoded_b, seq_b);
    }
}
