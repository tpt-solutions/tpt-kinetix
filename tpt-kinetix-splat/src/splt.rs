//! `SPLT`: the native Gaussian splat codec for static 3D clouds.
//!
//! # Layout
//!
//! ```text
//! 0   magic      b"SPLT"
//! 4   version    u8 = 2 (1 is the same layout without residual contexts)
//! 5   flags      u8: bit 0 lossy, bit 1 frame is RUB (0 = RDF)
//! 6   sh_degree  u8 (0..=3)
//! 7   reserved   u8 = 0
//! 8   count      u32 LE
//! 12  pos_bits   u8  (0 when lossless, else 1..=24)
//! 13  attr_bits  u8  (0 when lossless, else 1..=24)
//! 14  reserved   u16 = 0
//! 16  ranges     lossy only: per channel, lo f32 LE, hi f32 LE
//! ..  payload    rANS stream set, one stream per channel
//! ```
//!
//! # Coding
//!
//! 1. Points are sorted along a 3D Hilbert curve over their positions, so
//!    neighbouring points in the file are neighbours in space. The file stores
//!    them in that order, so the decoded order is the sorted order and not the
//!    input order.
//! 2. Every point is a row of `C` channels: position (3), opacity (1), log
//!    scale (3), rotation (4), SH (3 · (degree+1)²). Channel coding is shared
//!    with the 4D sequence format (see `block.rs`).
//!
//! The decoder never trusts the header: counts, bit depths, ranges and every
//! decoded value are checked before use.

use tpt_math_splat::sfc::hilbert_key;

use crate::block::{decode_block, encode_block, read_ranges, write_ranges, Layout};
use crate::cloud::{sh_coeffs_for_degree, CoordinateFrame, GaussianCloud, MAX_SH_DEGREE};
use crate::error::SplatError;
use tpt_kinetix_bitstream::rans::RansStreamSet;

const MAGIC: [u8; 4] = *b"SPLT";
/// Current layout: each channel's residuals are split into contexts (see
/// `block.rs`) so each gets its own frequency table.
const VERSION: u8 = 2;
/// Earlier layout: one frequency table per channel. Still read, never written.
const VERSION_PLAIN: u8 = 1;
const FLAG_LOSSY: u8 = 0b01;
const FLAG_RUB: u8 = 0b10;
/// Lossy files store each rotation as its three smallest components plus the
/// index of the largest. Files without this flag store all four components.
const FLAG_SMALLEST3: u8 = 0b100;
/// Hilbert resolution per axis used for ordering. 16 bits gives 48-bit keys.
const HILBERT_BITS: u32 = 16;
/// Largest point count accepted, bounding decoder allocation.
const MAX_POINTS: usize = 1 << 24;
/// Largest quantisation depth per channel.
pub(crate) const MAX_BITS: u8 = 24;
const HEADER_LEN: usize = 16;
/// Leading channels that hold positions.
const POSITION_CHANNELS: usize = 3;

/// How attribute values are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantization {
    /// Keep the exact `f32` bit patterns. The round trip is bit-exact, up to
    /// the point order (see the module docs).
    Lossless,
    /// Quantise positions to `position_bits` and every other channel to
    /// `attribute_bits`, both in `1..=24`. Each channel's error is at most
    /// `(hi - lo) / (2 · (2^bits − 1))`.
    Lossy {
        /// Bits per position coordinate.
        position_bits: u8,
        /// Bits per other channel value.
        attribute_bits: u8,
    },
}

impl Quantization {
    /// Checks that lossy depths are in `1..=24`.
    pub(crate) fn validate(self) -> Result<(), SplatError> {
        if let Quantization::Lossy {
            position_bits,
            attribute_bits,
        } = self
        {
            if !(1..=MAX_BITS).contains(&position_bits) || !(1..=MAX_BITS).contains(&attribute_bits)
            {
                return Err(SplatError::Invalid("quantisation bits must be in 1..=24"));
            }
        }
        Ok(())
    }

    /// The `(position_bits, attribute_bits)` pair as stored in headers.
    pub(crate) fn depths(self) -> (u8, u8) {
        match self {
            Quantization::Lossless => (0, 0),
            Quantization::Lossy {
                position_bits,
                attribute_bits,
            } => (position_bits, attribute_bits),
        }
    }

    /// Rebuilds from header depths, checking them against the lossy flag.
    pub(crate) fn from_depths(
        lossy: bool,
        pos_bits: u8,
        attr_bits: u8,
    ) -> Result<Self, SplatError> {
        if lossy {
            if !(1..=MAX_BITS).contains(&pos_bits) || !(1..=MAX_BITS).contains(&attr_bits) {
                return Err(SplatError::Parse("quantisation depth out of range"));
            }
            Ok(Quantization::Lossy {
                position_bits: pos_bits,
                attribute_bits: attr_bits,
            })
        } else {
            if pos_bits != 0 || attr_bits != 0 {
                return Err(SplatError::Parse(
                    "lossless data has quantisation depths set",
                ));
            }
            Ok(Quantization::Lossless)
        }
    }
}

/// Number of channels per point for a degree: 3 + 1 + 3 + 4 + 3·K.
fn channel_count(degree: u8) -> usize {
    11 + 3 * sh_coeffs_for_degree(degree)
}

/// Flattens a cloud into `count × C` values in row order.
fn rows(cloud: &GaussianCloud) -> Vec<f32> {
    let k3 = 3 * sh_coeffs_for_degree(cloud.sh_degree());
    let c = channel_count(cloud.sh_degree());
    let mut out = Vec::with_capacity(cloud.len() * c);
    for i in 0..cloud.len() {
        out.extend_from_slice(&cloud.positions()[i]);
        out.push(cloud.opacities()[i]);
        out.extend_from_slice(&cloud.scales()[i]);
        out.extend_from_slice(&cloud.rotations()[i]);
        out.extend_from_slice(&cloud.sh()[i * k3..(i + 1) * k3]);
    }
    out
}

/// Serialises a [`GaussianCloud`] as `SPLT`.
///
/// # Errors
///
/// [`SplatError::Invalid`] if a quantisation depth is outside `1..=24`, or if
/// the cloud has more than 2^24 points.
pub fn write_splt(cloud: &GaussianCloud, quant: Quantization) -> Result<Vec<u8>, SplatError> {
    quant.validate()?;
    let n = cloud.len();
    if n > MAX_POINTS {
        return Err(SplatError::Invalid("too many points for SPLT"));
    }
    let degree = cloud.sh_degree();
    let c = channel_count(degree);
    let mut data = rows(cloud);

    // Order the points along a Hilbert curve over their positions. Sorting is
    // a permutation of whole rows, so the values are unchanged.
    let (pmin, pmax) = position_bounds(&data, n, c);
    let mut order: Vec<(u64, usize)> = (0..n)
        .map(|i| {
            let p = [data[i * c], data[i * c + 1], data[i * c + 2]];
            (hilbert_key(p, pmin, pmax, HILBERT_BITS).unwrap_or(0), i)
        })
        .collect();
    order.sort_unstable();
    let mut sorted = Vec::with_capacity(data.len());
    for &(_, i) in &order {
        sorted.extend_from_slice(&data[i * c..(i + 1) * c]);
    }
    data = sorted;

    let lossy = matches!(quant, Quantization::Lossy { .. });
    if lossy {
        data = smallest_three_rows(&data, n, c, degree);
    }
    let layout = Layout {
        channels: c,
        position_channels: POSITION_CHANNELS,
        index_channel: lossy.then_some(c - 1),
    };
    let (ranges, streams) = encode_block(&data, n, layout, quant, true)?;

    let mut flags = 0u8;
    if lossy {
        flags |= FLAG_LOSSY | FLAG_SMALLEST3;
    }
    if cloud.frame() == CoordinateFrame::Rub {
        flags |= FLAG_RUB;
    }
    let (pos_bits, attr_bits) = quant.depths();

    let mut out =
        Vec::with_capacity(HEADER_LEN + c * 8 + streams.iter().map(Vec::len).sum::<usize>());
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.push(flags);
    out.push(degree);
    out.push(0);
    out.extend_from_slice(&(n as u32).to_le_bytes());
    out.push(pos_bits);
    out.push(attr_bits);
    out.extend_from_slice(&[0, 0]);
    if flags & FLAG_LOSSY != 0 {
        write_ranges(&mut out, &ranges);
    }
    out.extend_from_slice(
        &RansStreamSet::frame(&streams)
            .map_err(|_| SplatError::Invalid("too many SPLT streams"))?,
    );
    Ok(out)
}

/// Parses an `SPLT` file into a [`GaussianCloud`], in the order it was stored.
///
/// # Errors
///
/// [`SplatError::Parse`] for a bad magic, reserved fields set, out-of-range
/// counts or depths, truncated or corrupt streams, or decoded values that fall
/// outside their channel's domain. [`SplatError::Unsupported`] for unknown
/// versions and SH degrees above 3.
pub fn read_splt(bytes: &[u8]) -> Result<GaussianCloud, SplatError> {
    if bytes.len() < HEADER_LEN || bytes[..4] != MAGIC {
        return Err(SplatError::Parse("not an SPLT file"));
    }
    let contexts = match bytes[4] {
        VERSION => true,
        VERSION_PLAIN => false,
        _ => return Err(SplatError::Unsupported("SPLT version other than 1 or 2")),
    };
    let flags = bytes[5];
    if flags & !(FLAG_LOSSY | FLAG_RUB | FLAG_SMALLEST3) != 0
        || bytes[7] != 0
        || bytes[14..16] != [0, 0]
    {
        return Err(SplatError::Parse("reserved SPLT header bits set"));
    }
    let degree = bytes[6];
    if degree > MAX_SH_DEGREE {
        return Err(SplatError::Unsupported("SPLT SH degree above 3"));
    }
    let n = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    if n > MAX_POINTS {
        return Err(SplatError::Parse("SPLT point count out of range"));
    }
    let lossy = flags & FLAG_LOSSY != 0;
    let quant = Quantization::from_depths(lossy, bytes[12], bytes[13])?;

    let c = channel_count(degree);
    let mut pos = HEADER_LEN;
    let mut ranges = Vec::new();
    if lossy {
        let table = bytes
            .get(pos..pos + c * 8)
            .ok_or(SplatError::Parse("truncated SPLT range table"))?;
        ranges = read_ranges(table, c)?;
        pos += c * 8;
    }

    let streams = RansStreamSet::unframe(&bytes[pos..])
        .map_err(|_| SplatError::Parse("corrupt SPLT stream table"))?;
    if streams.len() != c {
        return Err(SplatError::Parse("SPLT stream count does not match degree"));
    }
    let smallest3 = flags & FLAG_SMALLEST3 != 0;
    if smallest3 && !lossy {
        return Err(SplatError::Parse(
            "smallest-three rotations require a lossy file",
        ));
    }
    let layout = Layout {
        channels: c,
        position_channels: POSITION_CHANNELS,
        index_channel: smallest3.then_some(c - 1),
    };
    let values = decode_block(&streams, n, layout, quant, &ranges, contexts)?;

    let k3 = 3 * sh_coeffs_for_degree(degree);
    let mut positions = Vec::with_capacity(n);
    let mut opacities = Vec::with_capacity(n);
    let mut scales = Vec::with_capacity(n);
    let mut rotations = Vec::with_capacity(n);
    let mut sh = Vec::with_capacity(n * k3);
    for r in values.chunks_exact(c) {
        positions.push([r[0], r[1], r[2]]);
        opacities.push(r[3]);
        scales.push([r[4], r[5], r[6]]);
        if smallest3 {
            rotations.push(restore_quaternion(r, r[c - 1] as usize));
            sh.extend_from_slice(&r[10..c - 1]);
        } else {
            rotations.push([r[7], r[8], r[9], r[10]]);
            sh.extend_from_slice(&r[11..]);
        }
    }
    let frame = if flags & FLAG_RUB != 0 {
        CoordinateFrame::Rub
    } else {
        CoordinateFrame::Rdf
    };
    Ok(GaussianCloud::new(positions, rotations, scales, opacities, degree, sh)?.with_frame(frame))
}

/// Rewrites lossy rows so each rotation is stored as its three smallest
/// components (sign made consistent with the largest, which is positive), then
/// the SH block, then the index of the largest component. Rows keep their width.
/// Quaternions are normalised first. A zero quaternion becomes the identity.
fn smallest_three_rows(data: &[f32], n: usize, c: usize, degree: u8) -> Vec<f32> {
    let k3 = 3 * sh_coeffs_for_degree(degree);
    let mut out = Vec::with_capacity(data.len());
    for r in data.chunks_exact(c).take(n) {
        let norm = r[7..11].iter().map(|v| v * v).sum::<f32>().sqrt();
        let q = if norm > 0.0 && norm.is_finite() {
            [r[7] / norm, r[8] / norm, r[9] / norm, r[10] / norm]
        } else {
            [1.0, 0.0, 0.0, 0.0]
        };
        let mut m = 0;
        for j in 1..4 {
            if q[j].abs() > q[m].abs() {
                m = j;
            }
        }
        let sign = if q[m] < 0.0 { -1.0 } else { 1.0 };
        out.extend_from_slice(&r[0..7]);
        for (j, &qj) in q.iter().enumerate() {
            if j != m {
                out.push(qj * sign);
            }
        }
        out.extend_from_slice(&r[11..11 + k3]);
        out.push(m as f32);
    }
    out
}

/// Inverse of [`smallest_three_rows`] for one row: the quaternion as `[w, x, y, z]`.
fn restore_quaternion(r: &[f32], largest: usize) -> [f32; 4] {
    let mut q = [0f32; 4];
    let mut sum_sq = 0f32;
    let mut k = 0;
    for (j, slot) in q.iter_mut().enumerate() {
        if j != largest {
            *slot = r[7 + k];
            sum_sq += *slot * *slot;
            k += 1;
        }
    }
    q[largest] = (1.0 - sum_sq).max(0.0).sqrt();
    q
}

/// Bounding box of the positions in row-major `data`, ignoring non-finite
/// coordinates. Degenerate boxes become `[0, 0]`.
fn position_bounds(data: &[f32], n: usize, c: usize) -> ([f32; 3], [f32; 3]) {
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for i in 0..n {
        for a in 0..3 {
            let v = data[i * c + a];
            if v.is_finite() {
                lo[a] = lo[a].min(v);
                hi[a] = hi[a].max(v);
            }
        }
    }
    for a in 0..3 {
        if lo[a] > hi[a] {
            lo[a] = 0.0;
            hi[a] = 0.0;
        }
    }
    (lo, hi)
}
