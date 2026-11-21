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
    let mut streams = Vec::with_capacity(c);
    for ch in 0..c {
        let mut bytes = Vec::new();
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
            push_varint(&mut bytes, zigzag(cur - prev));
            prev = cur;
        }
        // Fit a frequency table to this channel's own byte histogram, store it,
        // then code the bytes against it. A generic model mismatches these
        // residuals and expands them.
        let mut counts = [0u64; 256];
        for &b in &bytes {
            counts[usize::from(b)] += 1;
        }
        let freqs = normalize(&counts);
        let mut stream = Vec::with_capacity(256 + bytes.len() / 2);
        for f in &freqs {
            push_varint(&mut stream, u64::from(*f));
        }
        stream.extend_from_slice(&rans_encode(&bytes, &TableModel::new(&freqs)));
        streams.push(stream);
    }
    Ok((ranges, streams))
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
) -> Result<Vec<f32>, SplatError> {
    let c = layout.channels;
    if let Some(ch) = layout.index_channel {
        if ranges.get(ch) != Some(&INDEX_RANGE) {
            return Err(SplatError::Parse("index channel range is not [0, 3]"));
        }
    }
    let mut models = Vec::with_capacity(c);
    let mut decoders = Vec::with_capacity(c);
    for stream in streams {
        let (freqs, used) = read_table(stream)?;
        models.push(TableModel::new(&freqs));
        decoders.push(
            RansDecoder::new(&stream[used..])
                .map_err(|_| SplatError::Parse("corrupt rANS stream"))?,
        );
    }
    let max: Vec<u64> = (0..c)
        .map(|ch| channel_bits(ch, layout, quant).map_or(u64::from(u32::MAX), |b| (1u64 << b) - 1))
        .collect();

    // Decode row by row, one varint per channel, so the streams advance in
    // step. No allocation depends on counts read from the stream beyond the
    // output itself.
    let mut prev = vec![0i64; c];
    let mut rows = Vec::with_capacity(n * c);
    for _ in 0..n {
        for ch in 0..c {
            let raw = read_varint_stream(&mut decoders[ch], &models[ch])?;
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
fn read_table(stream: &[u8]) -> Result<([u32; 256], usize), SplatError> {
    let mut f = [0u32; 256];
    let mut at = 0usize;
    for slot in f.iter_mut() {
        let (v, used) = read_varint_bytes(&stream[at..])?;
        if v > u64::from(TABLE_TOTAL) {
            return Err(SplatError::Parse("frequency table entry too large"));
        }
        *slot = v as u32;
        at += used;
    }
    if f.iter().map(|&x| u64::from(x)).sum::<u64>() != u64::from(TABLE_TOTAL) {
        return Err(SplatError::Parse(
            "frequency table does not sum to the total",
        ));
    }
    Ok((f, at))
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
