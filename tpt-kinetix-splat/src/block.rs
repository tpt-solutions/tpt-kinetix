//! Shared channel coding for `SPLT` (3D static) and 4D sequence chunks.
//!
//! A block is `n` rows of `c` channels. Each channel is quantised (or kept
//! exactly), delta-coded against the previous row, zigzag and varint mapped,
//! then rANS-coded with a frequency table fitted to that channel. The first
//! `position_channels` channels use the position bit depth, the rest the
//! attribute depth.

use tpt_kinetix_bitstream::rans::{RansDecoder, RansEncoder, SymbolInfo, SymbolModel};
use tpt_math_splat::sfc::quantize;

use crate::error::SplatError;
use crate::splt::Quantization;

/// Channel layout of a block: total channels, and how many lead channels are
/// positions.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Layout {
    pub channels: usize,
    pub position_channels: usize,
    /// A lossy-only integer channel holding a value in `0..=3` (the index of a
    /// quaternion's largest component). It is coded at 2 bits over the fixed
    /// range `[0, 3]`, so the value survives exactly.
    pub index_channel: Option<usize>,
}

/// The fixed domain of the integer index channel.
const INDEX_RANGE: (f32, f32) = (0.0, 3.0);

/// Bit depth of channel `ch`, or `None` for lossless.
pub(crate) fn channel_bits(ch: usize, layout: Layout, quant: Quantization) -> Option<u8> {
    match quant {
        Quantization::Lossless => None,
        Quantization::Lossy {
            position_bits,
            attribute_bits,
        } => Some(if layout.index_channel == Some(ch) {
            2
        } else if ch < layout.position_channels {
            position_bits
        } else {
            attribute_bits
        }),
    }
}

/// Per-channel ranges and coded streams for one block.
pub(crate) type EncodedBlock = (Vec<(f32, f32)>, Vec<Vec<u8>>);

/// Encodes `n` rows of `layout.channels` values. Returns each channel's
/// `(lo, hi)` range and its coded stream (frequency table, then rANS bytes).
///
/// # Errors
///
/// Propagates no errors today; the `Result` keeps room for future validation.
pub(crate) fn encode_block(
    data: &[f32],
    n: usize,
    layout: Layout,
    quant: Quantization,
    contexts: bool,
) -> Result<EncodedBlock, SplatError> {
    let c = layout.channels;
    let mut ranges = Vec::with_capacity(c);
    for ch in 0..c {
        if layout.index_channel == Some(ch) {
            ranges.push(INDEX_RANGE);
        } else {
            ranges.push(bounds_1d((0..n).map(|i| data[i * c + ch])));
        }
    }
    // Residuals of every value, row-major, so each can use its neighbours as context.
    let mut residuals = vec![0u64; n * c];
    for ch in 0..c {
        let mut prev: i64 = 0;
        for i in 0..n {
            let v = data[i * c + ch];
            let cur = match channel_bits(ch, layout, quant) {
                None => i64::from(v.to_bits()),
                Some(bits) => {
                    let (lo, hi) = ranges[ch];
                    i64::from(quantize(v, lo, hi, u32::from(bits)))
                }
            };
            residuals[i * c + ch] = zigzag(cur - prev);
            prev = cur;
        }
    }
    let mut streams = Vec::with_capacity(c);
    for ch in 0..c {
        // Each residual's varint goes into the substream its context selects.
        let mut subs = vec![Vec::new(); context_count(contexts)];
        for i in 0..n {
            let idx = i * c + ch;
            let same = if i == 0 { 0 } else { residuals[idx - c] };
            let before = if idx == 0 { 0 } else { residuals[idx - 1] };
            push_varint(
                &mut subs[context_of(contexts, same, before)],
                residuals[idx],
            );
        }
        let mut stream = Vec::new();
        for bytes in &subs {
            if contexts {
                // An unused context costs one zero byte and no table.
                if bytes.is_empty() {
                    push_varint(&mut stream, 0);
                    continue;
                }
                let sub = encode_substream(bytes, contexts);
                push_varint(&mut stream, sub.len() as u64);
                stream.extend_from_slice(&sub);
            } else {
                stream.extend_from_slice(&encode_substream(bytes, contexts));
            }
        }
        streams.push(stream);
    }
    Ok((ranges, streams))
}

/// Number of residual substreams per channel.
fn context_count(contexts: bool) -> usize {
    if contexts {
        CONTEXTS
    } else {
        1
    }
}

/// Substream for a residual, from two neighbours' zigzag values: `same`, the
/// same channel's previous residual, and `before`, the residual just before it
/// in row-major order. `same` picks one of four magnitude buckets and `before`
/// splits on whether it reaches 8 bits. Without contexts every residual is in
/// substream 0.
fn context_of(contexts: bool, same: u64, before: u64) -> usize {
    if !contexts {
        return 0;
    }
    let a = (bit_len(same) / 4).min(3);
    let b = usize::from(bit_len(before) >= 8);
    ((a * 2 + b) * 2 + usize::from(same & 1 == 1)) * 2 + usize::from(before & 1 == 1)
}

/// Number of significant bits in `v` (0 for 0).
fn bit_len(v: u64) -> usize {
    64 - v.leading_zeros() as usize
}

/// Residual contexts per channel when context coding is on.
pub(crate) const CONTEXTS: usize = 32;

/// Fits a frequency table to one substream's byte histogram and codes the
/// bytes against it, returning the table followed by the rANS bytes. A
/// generic model mismatches these residuals and expands them.
fn encode_substream(bytes: &[u8], sparse: bool) -> Vec<u8> {
    let mut counts = [0u64; 256];
    for &b in bytes {
        counts[usize::from(b)] += 1;
    }
    let freqs = normalize(&counts);
    let mut out = Vec::with_capacity(256 + bytes.len() / 2);
    write_table(&mut out, &freqs, sparse);
    out.extend_from_slice(&rans_encode(bytes, &TableModel::new(&freqs)));
    out
}

/// Writes ranges as `lo, hi` pairs of little-endian `f32`.
pub(crate) fn write_ranges(out: &mut Vec<u8>, ranges: &[(f32, f32)]) {
    for (lo, hi) in ranges {
        out.extend_from_slice(&lo.to_le_bytes());
        out.extend_from_slice(&hi.to_le_bytes());
    }
}

/// Reads `c` ranges written by [`write_ranges`], checking that they are finite
/// and ordered.
pub(crate) fn read_ranges(table: &[u8], c: usize) -> Result<Vec<(f32, f32)>, SplatError> {
    let mut ranges = Vec::with_capacity(c);
    for ch in 0..c {
        let lo = f32::from_le_bytes(table[ch * 8..ch * 8 + 4].try_into().unwrap_or([0; 4]));
        let hi = f32::from_le_bytes(table[ch * 8 + 4..ch * 8 + 8].try_into().unwrap_or([0; 4]));
        if !lo.is_finite() || !hi.is_finite() || hi < lo {
            return Err(SplatError::Parse("channel range invalid"));
        }
        ranges.push((lo, hi));
    }
    Ok(ranges)
}

/// Decodes `n` rows from one stream per channel, returning row-major values.
pub(crate) fn decode_block(
    streams: &[&[u8]],
    n: usize,
    layout: Layout,
    quant: Quantization,
    ranges: &[(f32, f32)],
    contexts: bool,
) -> Result<Vec<f32>, SplatError> {
    let c = layout.channels;
    if let Some(ch) = layout.index_channel {
        if ranges.get(ch) != Some(&INDEX_RANGE) {
            return Err(SplatError::Parse("index channel range is not [0, 3]"));
        }
    }
    // Per channel, one decoder per substream. `None` marks an unused context.
    let mut channels: Vec<Vec<Option<(TableModel, RansDecoder<'_>)>>> = Vec::with_capacity(c);
    for &stream in streams {
        let mut subs = Vec::with_capacity(context_count(contexts));
        for sub in split_substreams(stream, contexts)? {
            subs.push(match sub {
                None => None,
                Some(bytes) => {
                    let (freqs, used) = read_table(bytes, contexts)?;
                    let dec = RansDecoder::new(&bytes[used..])
                        .map_err(|_| SplatError::Parse("corrupt rANS stream"))?;
                    Some((TableModel::new(&freqs), dec))
                }
            });
        }
        channels.push(subs);
    }
    let max: Vec<u64> = (0..c)
        .map(|ch| channel_bits(ch, layout, quant).map_or(u64::from(u32::MAX), |b| (1u64 << b) - 1))
        .collect();

    // Decode row by row, one varint per channel, so the streams advance in
    // step. The context of each varint depends only on the previous residual
    // of that channel, which the decoder has already read. No allocation
    // depends on counts read from the stream beyond the output itself.
    let mut prev = vec![0i64; c];
    let mut prev_zz = vec![0u64; c];
    let mut last_zz = 0u64;
    let mut rows = Vec::with_capacity(n * c);
    for _ in 0..n {
        for ch in 0..c {
            let k = context_of(contexts, prev_zz[ch], last_zz);
            let (model, dec) = channels[ch][k]
                .as_mut()
                .ok_or(SplatError::Parse("residual context has no stream"))?;
            let raw = read_varint_stream(dec, model)?;
            prev_zz[ch] = raw;
            last_zz = raw;
            let cur = prev[ch]
                .checked_add(unzigzag(raw))
                .ok_or(SplatError::Parse("residual overflow"))?;
            if cur < 0 || cur as u64 > max[ch] {
                return Err(SplatError::Parse("value outside its channel domain"));
            }
            prev[ch] = cur;
            rows.push(match channel_bits(ch, layout, quant) {
                None => f32::from_bits(cur as u32),
                Some(b) => {
                    let (lo, hi) = ranges[ch];
                    let levels = ((1u32 << b) - 1) as f32;
                    lo + cur as f32 * (hi - lo) / levels
                }
            });
        }
    }
    Ok(rows)
}

/// Bounding box of a point set, ignoring non-finite coordinates.
/// Range of finite values, or `(0, 0)` when there are none.
fn bounds_1d(values: impl Iterator<Item = f32>) -> (f32, f32) {
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for v in values.filter(|v| v.is_finite()) {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if lo > hi {
        (0.0, 0.0)
    } else {
        (lo, hi)
    }
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn push_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// Reads one LEB128 value from a rANS-coded byte stream.
fn read_varint_stream(dec: &mut RansDecoder<'_>, model: &TableModel) -> Result<u64, SplatError> {
    let mut v = 0u64;
    for i in 0..10 {
        let b = dec
            .decode(model)
            .map_err(|_| SplatError::Parse("corrupt SPLT rANS stream"))?;
        let part = u64::from(b & 0x7f);
        if i == 9 && part > 1 {
            return Err(SplatError::Parse("varint overflows 64 bits"));
        }
        v |= part << (7 * i);
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(SplatError::Parse("varint longer than 10 bytes"))
}

/// rANS-encodes bytes with a static model.
fn rans_encode(bytes: &[u8], model: &TableModel) -> Vec<u8> {
    let mut enc = RansEncoder::new();
    // rANS encodes in reverse, so push the bytes back to front.
    for &b in bytes.iter().rev() {
        enc.encode(model, b);
    }
    enc.finish()
}

/// Probability precision of the per-channel tables. Must equal the rANS
/// coder's `PROB_SCALE` (`1 << PROB_BITS`, 12 bits in `tpt-kinetix-bitstream`).
const TABLE_TOTAL: u32 = 1 << 12;

/// Scales byte counts to frequencies summing to exactly [`TABLE_TOTAL`]. Every
/// byte that occurs keeps a frequency of at least 1.
fn normalize(counts: &[u64; 256]) -> [u32; 256] {
    let total: u64 = counts.iter().sum();
    let mut f = [0u32; 256];
    if total == 0 {
        f[0] = TABLE_TOTAL;
        return f;
    }
    for (s, &c) in counts.iter().enumerate() {
        if c > 0 {
            f[s] = ((c * u64::from(TABLE_TOTAL)) / total).max(1) as u32;
        }
    }
    let most = (0..256).max_by_key(|&s| counts[s]).unwrap_or(0);
    let mut sum: u32 = f.iter().sum();
    // Rounding up to 1 can overshoot. Take units back from the largest entries.
    while sum > TABLE_TOTAL {
        let largest = (0..256).max_by_key(|&s| f[s]).unwrap_or(0);
        f[largest] -= 1;
        sum -= 1;
    }
    f[most] += TABLE_TOTAL - sum;
    f
}

/// Reads a channel's 256-entry frequency table, returning it and its length.
fn read_table(stream: &[u8], sparse: bool) -> Result<([u32; 256], usize), SplatError> {
    let mut f = [0u32; 256];
    let mut at = 0usize;
    if sparse {
        // Count of used symbols, then (gap, frequency) per used symbol. The gap
        // is counted from one past the previous symbol, so symbols strictly increase.
        let (k, used) = read_varint_bytes(stream)?;
        at += used;
        if k > 256 {
            return Err(SplatError::Parse("frequency table lists too many symbols"));
        }
        let mut next_min = 0u64;
        for _ in 0..k {
            let (gap, used) = read_varint_bytes(&stream[at..])?;
            at += used;
            let (freq, used) = read_varint_bytes(&stream[at..])?;
            at += used;
            let sym = next_min
                .checked_add(gap)
                .filter(|&s| s < 256)
                .ok_or(SplatError::Parse("frequency table symbol out of range"))?;
            if freq == 0 || freq > u64::from(TABLE_TOTAL) {
                return Err(SplatError::Parse("frequency table entry out of range"));
            }
            f[sym as usize] = freq as u32;
            next_min = sym + 1;
        }
    } else {
        for slot in f.iter_mut() {
            let (v, used) = read_varint_bytes(&stream[at..])?;
            if v > u64::from(TABLE_TOTAL) {
                return Err(SplatError::Parse("frequency table entry too large"));
            }
            *slot = v as u32;
            at += used;
        }
    }
    if f.iter().map(|&x| u64::from(x)).sum::<u64>() != u64::from(TABLE_TOTAL) {
        return Err(SplatError::Parse(
            "frequency table does not sum to the total",
        ));
    }
    Ok((f, at))
}

/// Writes a frequency table: all 256 entries (`sparse` false), or only the
/// used symbols as `(gap, frequency)` pairs (`sparse` true). See [`read_table`].
fn write_table(out: &mut Vec<u8>, freqs: &[u32; 256], sparse: bool) {
    if sparse {
        let used: Vec<usize> = (0..256).filter(|&s| freqs[s] > 0).collect();
        push_varint(out, used.len() as u64);
        let mut next_min = 0usize;
        for s in used {
            push_varint(out, (s - next_min) as u64);
            push_varint(out, u64::from(freqs[s]));
            next_min = s + 1;
        }
    } else {
        for f in freqs {
            push_varint(out, u64::from(*f));
        }
    }
}

/// Reads one LEB128 value from raw bytes, returning `(value, bytes used)`.
fn read_varint_bytes(data: &[u8]) -> Result<(u64, usize), SplatError> {
    let mut v = 0u64;
    for (i, &b) in data.iter().take(10).enumerate() {
        let part = u64::from(b & 0x7f);
        if i == 9 && part > 1 {
            return Err(SplatError::Parse("varint overflows 64 bits"));
        }
        v |= part << (7 * i);
        if b & 0x80 == 0 {
            return Ok((v, i + 1));
        }
    }
    Err(SplatError::Parse("table ends mid-value"))
}

/// Splits one channel's stream into its substreams. Without contexts the whole
/// stream is one substream. With contexts each substream is a varint length
/// (0 for an unused context) followed by its bytes.
fn split_substreams(stream: &[u8], contexts: bool) -> Result<Vec<Option<&[u8]>>, SplatError> {
    if !contexts {
        return Ok(vec![Some(stream)]);
    }
    let mut out = Vec::with_capacity(CONTEXTS);
    let mut at = 0usize;
    for _ in 0..CONTEXTS {
        let (len, used) = read_varint_bytes(stream.get(at..).unwrap_or(&[]))?;
        at += used;
        if len == 0 {
            out.push(None);
            continue;
        }
        let len = usize::try_from(len).map_err(|_| SplatError::Parse("substream too long"))?;
        let end = at
            .checked_add(len)
            .filter(|&end| end <= stream.len())
            .ok_or(SplatError::Parse("substream runs past its channel"))?;
        out.push(Some(&stream[at..end]));
        at = end;
    }
    if at != stream.len() {
        return Err(SplatError::Parse(
            "trailing bytes after a channel's substreams",
        ));
    }
    Ok(out)
}

/// A static model built from one channel's frequency table.
struct TableModel {
    /// `cum[s]` is the first slot of symbol `s`. `cum[256] == TABLE_TOTAL`.
    cum: [u32; 257],
    /// Maps each slot to the symbol that owns it.
    inv: Vec<u8>,
}

impl TableModel {
    fn new(freqs: &[u32; 256]) -> Self {
        let mut cum = [0u32; 257];
        for s in 0..256 {
            cum[s + 1] = cum[s] + freqs[s];
        }
        let mut inv = vec![0u8; TABLE_TOTAL as usize];
        for s in 0..256 {
            for slot in cum[s]..cum[s + 1] {
                inv[slot as usize] = s as u8;
            }
        }
        Self { cum, inv }
    }
}

impl SymbolModel for TableModel {
    fn info(&self, symbol: u8) -> SymbolInfo {
        let s = usize::from(symbol);
        SymbolInfo {
            start: self.cum[s],
            freq: self.cum[s + 1] - self.cum[s],
        }
    }

    fn find(&self, cum_freq: u32) -> (u8, SymbolInfo) {
        let symbol = self.inv[cum_freq as usize];
        (symbol, self.info(symbol))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lossless round trip through both coding modes. Channel 0 jumps by large
    /// amounts so residuals reach every context; channel 1 is constant, so its
    /// residuals stay in context 0 and the other contexts are unused.
    #[test]
    fn round_trips_in_both_modes_and_every_context() {
        let c = 4;
        let n = 3000;
        let mut seed = 0x9e37_79b9_u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed
        };
        let mut data = Vec::with_capacity(n * c);
        let mut walk = 0f32;
        for _ in 0..n {
            let r = next();
            let jump = match r % 4 {
                0 => 0.001,
                1 => 3.0,
                2 => 4000.0,
                _ => 1.0e7,
            };
            walk += jump * if r & 8 == 0 { 1.0 } else { -1.0 };
            data.push(walk);
            data.push(1.5);
            data.push((next() % 1000) as f32 * 0.25);
            data.push(-0.0);
        }
        let layout = Layout {
            channels: c,
            position_channels: 3,
            index_channel: None,
        };
        for contexts in [false, true] {
            let (ranges, streams) =
                encode_block(&data, n, layout, Quantization::Lossless, contexts).unwrap();
            let views: Vec<&[u8]> = streams.iter().map(Vec::as_slice).collect();
            let back =
                decode_block(&views, n, layout, Quantization::Lossless, &ranges, contexts).unwrap();
            let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
            assert_eq!(bits(&back), bits(&data), "contexts = {contexts}");
        }
    }

    #[test]
    fn substream_split_rejects_overrun_and_trailing_bytes() {
        // Context 0 claims 9 bytes but only 2 follow.
        let mut bad = vec![9u8, 1, 2];
        bad.extend_from_slice(&[0, 0, 0]);
        assert!(split_substreams(&bad, true).is_err());
        // All contexts empty plus a stray byte.
        let mut stray = vec![0u8; CONTEXTS];
        stray.push(7);
        assert!(split_substreams(&stray, true).is_err());
        // All contexts empty exactly.
        let empty = vec![0u8; CONTEXTS];
        let ok = split_substreams(&empty, true).unwrap();
        assert!(ok.iter().all(Option::is_none));
    }

    /// Sparse tables round-trip, and sparse and dense parse to the same table.
    #[test]
    fn sparse_table_round_trips() {
        let mut counts = [0u64; 256];
        for (s, c) in [(0usize, 5u64), (7, 300), (255, 1), (128, 42)] {
            counts[s] = c;
        }
        let freqs = normalize(&counts);
        let mut sparse = Vec::new();
        write_table(&mut sparse, &freqs, true);
        let (back, used) = read_table(&sparse, true).unwrap();
        assert_eq!(back, freqs);
        assert_eq!(used, sparse.len());
        let mut dense = Vec::new();
        write_table(&mut dense, &freqs, false);
        assert_eq!(read_table(&dense, false).unwrap().0, freqs);
    }

    #[test]
    fn sparse_table_rejects_malformed_input() {
        // Too many symbols.
        assert!(read_table(&[0x81, 0x01], true).is_err());
        // Symbol past 255: gap 255 from zero is fine, then gap 1 pushes it to 256.
        let mut overflow = vec![2u8, 255, 1, 1, 1];
        assert!(read_table(&overflow, true).is_err());
        // A zero frequency is not a used symbol.
        overflow = vec![1u8, 0, 0];
        assert!(read_table(&overflow, true).is_err());
        // Frequencies that do not sum to the total.
        let bad_sum = vec![1u8, 3, 5];
        assert!(read_table(&bad_sum, true).is_err());
    }
}
