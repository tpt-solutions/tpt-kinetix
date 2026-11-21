//! SPZ (Niantic) read support: legacy gzip files (versions 2 and 3) and
//! NGSP/ZSTD files (version 4).
//!
//! The reader decodes SPZ's quantised streams into a [`GaussianCloud`] in the
//! file's native frame, with the same values the reference `spz_to_ply` writes
//! by default. Checked against the nianticlabs/spz v2 samples:
//!
//! * positions are fixed-point 24-bit, scaled by `2^-fractionalBits`, unchanged;
//! * rotations are `[x, y, z, w]` in the file, reordered to `[w, x, y, z]`;
//! * scales, opacity logits and rest SH match the reference exactly;
//! * the DC value is the decoded colour `(v/255 - 0.5) / 0.15`, written to
//!   `f_dc` unchanged.
//!
//! No RUB↔RDF frame conversion is applied. The reference does not apply one
//! by default, so a conversion would make this reader disagree with it.

use std::io::Read;

use crate::cloud::{sh_coeffs_for_degree, CoordinateFrame, GaussianCloud, MAX_SH_DEGREE};
use crate::error::SplatError;

/// Bytes per point in the 24-bit fixed-point position stream.
const POS_BYTES: usize = 9;
/// Largest point count accepted, matching the reference's `int32_t` bound.
const MAX_POINTS: u64 = i32::MAX as u64;
/// Largest decompressed payload accepted, to bound memory on hostile input.
const MAX_DECOMPRESSED: u64 = 1 << 32;
/// SPZ colour encoding scale (`splat-utils.h`: `colorScale`).
const COLOR_SCALE: f32 = 0.15;

/// Attribute streams in file order, each already decompressed.
struct Streams {
    n: usize,
    degree: u8,
    frac_bits: u8,
    version: u32,
    positions: Vec<u8>,
    alphas: Vec<u8>,
    colors: Vec<u8>,
    scales: Vec<u8>,
    rotations: Vec<u8>,
    sh_rest: Vec<u8>,
}

/// Reads an SPZ file (versions 2, 3 and 4) into a [`GaussianCloud`].
///
/// The input is untrusted. Declared point counts and stream sizes are checked
/// before any large allocation, and decompression is capped.
///
/// # Errors
///
/// * [`SplatError::Parse`] for bad magic, truncated or inconsistent streams,
///   out-of-range counts, or corrupt compressed data.
/// * [`SplatError::Unsupported`] for version 1 (float16 positions) and for SH
///   degree above 3.
pub fn read_spz(bytes: &[u8]) -> Result<GaussianCloud, SplatError> {
    let streams = if bytes.starts_with(&[0x1f, 0x8b]) {
        read_legacy(bytes)?
    } else if bytes.starts_with(b"NGSP") {
        read_ngsp(bytes)?
    } else {
        return Err(SplatError::Parse("not an SPZ file"));
    };
    decode(&streams).map(to_cloud)
}

fn le_u32(b: &[u8], at: usize) -> Result<u32, SplatError> {
    let s = b
        .get(at..at + 4)
        .ok_or(SplatError::Parse("truncated SPZ header"))?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn le_u64(b: &[u8], at: usize) -> Result<u64, SplatError> {
    let s = b
        .get(at..at + 8)
        .ok_or(SplatError::Parse("truncated SPZ table of contents"))?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Ok(u64::from_le_bytes(a))
}

/// Flag bit 0x2: the file carries vendor extension records. Their framing is
/// not implemented, and a ZSTD decoder would read the trailing extension frames
/// as part of the last stream, so such files are refused rather than misread.
fn reject_extensions(flags: u8) -> Result<(), SplatError> {
    if flags & 0x2 != 0 {
        return Err(SplatError::Unsupported("SPZ files with extensions"));
    }
    Ok(())
}

/// Checks the point count and SH degree.
fn check_header(n: u32, degree: u8) -> Result<(usize, u8), SplatError> {
    if n == 0 || u64::from(n) > MAX_POINTS {
        return Err(SplatError::Parse("invalid SPZ point count"));
    }
    if degree > MAX_SH_DEGREE {
        return Err(SplatError::Unsupported("SPZ SH degree above 3"));
    }
    Ok((n as usize, degree))
}

/// Number of rest SH coefficients per channel (all coefficients but DC).
fn rest_coeffs(degree: u8) -> usize {
    sh_coeffs_for_degree(degree) - 1
}

/// Byte sizes of the six attribute streams for a version, in file order.
fn stream_sizes(n: usize, degree: u8, version: u32) -> [usize; 6] {
    let rot = if version == 2 { 3 } else { 4 };
    [
        POS_BYTES * n,
        n,
        3 * n,
        3 * n,
        rot * n,
        3 * rest_coeffs(degree) * n,
    ]
}

/// Legacy gzip files: a 16-byte header, then the six streams back to back.
fn read_legacy(bytes: &[u8]) -> Result<Streams, SplatError> {
    // Read the header before inflating the rest, so the body size is known
    // and the inflate can be capped.
    let mut gz = flate2::read::GzDecoder::new(bytes);
    let mut head = [0u8; 16];
    gz.read_exact(&mut head)
        .map_err(|_| SplatError::Parse("truncated gzip SPZ header"))?;
    if &head[0..4] != b"NGSP" {
        return Err(SplatError::Parse("bad SPZ magic in gzip stream"));
    }
    let version = le_u32(&head, 4)?;
    if !(2..=3).contains(&version) {
        return Err(SplatError::Unsupported(
            "legacy SPZ version other than 2 or 3",
        ));
    }
    let (n, degree) = check_header(le_u32(&head, 8)?, head[12])?;
    reject_extensions(head[14])?;
    let sizes = stream_sizes(n, degree, version);
    let body_len: u64 = sizes.iter().map(|&s| s as u64).sum();
    if body_len > MAX_DECOMPRESSED {
        return Err(SplatError::Parse("SPZ body exceeds decompression cap"));
    }
    let mut body = Vec::with_capacity(body_len as usize);
    gz.take(body_len)
        .read_to_end(&mut body)
        .map_err(|_| SplatError::Parse("corrupt gzip SPZ body"))?;
    if body.len() as u64 != body_len {
        return Err(SplatError::Parse(
            "gzip SPZ body shorter than header claims",
        ));
    }
    let mut parts = split(&body, &sizes).into_iter();
    let mut next = || parts.next().unwrap_or_default();
    Ok(Streams {
        n,
        degree,
        frac_bits: head[13],
        version,
        positions: next(),
        alphas: next(),
        colors: next(),
        scales: next(),
        rotations: next(),
        sh_rest: next(),
    })
}

/// Splits `body` into consecutive pieces of the given sizes. The caller has
/// checked that the sizes add up to `body.len()`.
fn split(body: &[u8], sizes: &[usize]) -> Vec<Vec<u8>> {
    let mut at = 0;
    sizes
        .iter()
        .map(|&s| {
            let piece = body[at..at + s].to_vec();
            at += s;
            piece
        })
        .collect()
}

/// NGSP (version 4): 32-byte header, table of contents, one ZSTD frame per stream.
fn read_ngsp(bytes: &[u8]) -> Result<Streams, SplatError> {
    let version = le_u32(bytes, 4)?;
    if version != 4 {
        return Err(SplatError::Unsupported("NGSP version other than 4"));
    }
    let (n, degree) = check_header(
        le_u32(bytes, 8)?,
        *bytes
            .get(12)
            .ok_or(SplatError::Parse("truncated SPZ header"))?,
    )?;
    let frac_bits = *bytes
        .get(13)
        .ok_or(SplatError::Parse("truncated SPZ header"))?;
    reject_extensions(
        *bytes
            .get(14)
            .ok_or(SplatError::Parse("truncated SPZ header"))?,
    )?;
    let num_streams = *bytes
        .get(15)
        .ok_or(SplatError::Parse("truncated SPZ header"))?;
    let toc_offset = le_u32(bytes, 16)? as usize;

    let sizes = stream_sizes(n, degree, version);
    let count = if degree == 0 { 5 } else { 6 };
    if usize::from(num_streams) != count {
        return Err(SplatError::Parse(
            "NGSP stream count does not match SH degree",
        ));
    }
    let toc_len = count * 16;
    let toc_end = toc_offset
        .checked_add(toc_len)
        .filter(|&e| e <= bytes.len())
        .ok_or(SplatError::Parse("NGSP table of contents out of range"))?;
    if toc_offset < 32 {
        return Err(SplatError::Parse("NGSP table of contents overlaps header"));
    }

    let mut pieces = Vec::with_capacity(count);
    let mut at = toc_end;
    for (i, &want) in sizes.iter().take(count).enumerate() {
        let entry = toc_offset + i * 16;
        let comp = le_u64(bytes, entry)? as usize;
        let uncomp = le_u64(bytes, entry + 8)?;
        if uncomp != want as u64 {
            return Err(SplatError::Parse(
                "NGSP stream size does not match point count",
            ));
        }
        let end = at
            .checked_add(comp)
            .filter(|&e| e <= bytes.len())
            .ok_or(SplatError::Parse("NGSP stream extends past end of file"))?;
        pieces.push(decompress_zstd(&bytes[at..end], want)?);
        at = end;
    }
    // A degree-0 file has no rest-SH stream; pad so indices line up.
    pieces.resize(6, Vec::new());
    let mut it = pieces.into_iter();
    let mut next = || it.next().unwrap_or_default();
    Ok(Streams {
        n,
        degree,
        frac_bits,
        version,
        positions: next(),
        alphas: next(),
        colors: next(),
        scales: next(),
        rotations: next(),
        sh_rest: next(),
    })
}

/// Decompresses one ZSTD frame that must produce exactly `want` bytes.
fn decompress_zstd(frame: &[u8], want: usize) -> Result<Vec<u8>, SplatError> {
    let mut decoder = ruzstd::decoding::StreamingDecoder::new(frame)
        .map_err(|_| SplatError::Parse("corrupt ZSTD stream"))?;
    let mut out = Vec::with_capacity(want);
    (&mut decoder)
        .take(want as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|_| SplatError::Parse("corrupt ZSTD stream"))?;
    if out.len() != want {
        return Err(SplatError::Parse("ZSTD stream size does not match header"));
    }
    Ok(out)
}

/// Decodes the raw streams into a [`GaussianCloud`] in the SPZ frame (RUB).
fn decode(s: &Streams) -> Result<Native, SplatError> {
    let n = s.n;
    let positions = s
        .positions
        .chunks_exact(POS_BYTES)
        .map(|p| {
            let mut out = [0f32; 3];
            for (axis, o) in out.iter_mut().enumerate() {
                let b = &p[axis * 3..axis * 3 + 3];
                let mut v = i32::from(b[0]) | (i32::from(b[1]) << 8) | (i32::from(b[2]) << 16);
                if v & 0x80_0000 != 0 {
                    v |= !0xff_ffff;
                }
                *o = v as f32 / (1u32 << s.frac_bits) as f32;
            }
            out
        })
        .collect::<Vec<_>>();
    let alphas = s
        .alphas
        .iter()
        .map(|&a| {
            let x = f32::from(a) / 255.0;
            (x / (1.0 - x)).ln()
        })
        .collect::<Vec<_>>();
    let colors = s
        .colors
        .chunks_exact(3)
        .map(|c| {
            let f = |v: u8| (f32::from(v) / 255.0 - 0.5) / COLOR_SCALE;
            [f(c[0]), f(c[1]), f(c[2])]
        })
        .collect::<Vec<_>>();
    let scales = s
        .scales
        .chunks_exact(3)
        .map(|c| {
            let f = |v: u8| f32::from(v) / 16.0 - 10.0;
            [f(c[0]), f(c[1]), f(c[2])]
        })
        .collect::<Vec<_>>();
    let rotations = decode_rotations(&s.rotations, s.version);
    let sh_rest = s
        .sh_rest
        .iter()
        .map(|&q| (f32::from(q) - 128.0) / 128.0)
        .collect();

    if positions.len() != n
        || alphas.len() != n
        || colors.len() != n
        || scales.len() != n
        || rotations.len() != n
    {
        return Err(SplatError::Parse(
            "SPZ stream length does not match point count",
        ));
    }
    Ok(Native {
        n,
        degree: s.degree,
        positions,
        alphas,
        colors,
        scales,
        rotations,
        sh_rest,
    })
}

/// SPZ-native values, before the frame conversion.
struct Native {
    n: usize,
    degree: u8,
    /// Positions in RUB.
    positions: Vec<[f32; 3]>,
    /// Opacity logit.
    alphas: Vec<f32>,
    /// `(rgb - 0.5) / COLOR_SCALE`, the DC colour in SPZ terms.
    colors: Vec<[f32; 3]>,
    /// Log scale.
    scales: Vec<[f32; 3]>,
    /// Quaternion `[x, y, z, w]`, normalised, RUB frame.
    rotations: Vec<[f32; 4]>,
    /// Rest SH, laid out `[point][coeff][channel]` with coeff counting from band 1.
    sh_rest: Vec<f32>,
}

/// Decodes rotations. Version 2 stores `xyz` in 8 bits and derives `w`.
/// Version 3 and later store the smallest three components.
fn decode_rotations(stream: &[u8], version: u32) -> Vec<[f32; 4]> {
    if version == 2 {
        stream
            .chunks_exact(3)
            .map(|r| {
                let x = f32::from(r[0]) / 127.5 - 1.0;
                let y = f32::from(r[1]) / 127.5 - 1.0;
                let z = f32::from(r[2]) / 127.5 - 1.0;
                let w = (1.0 - (x * x + y * y + z * z)).max(0.0).sqrt();
                [x, y, z, w]
            })
            .collect()
    } else {
        stream.chunks_exact(4).map(decode_smallest_three).collect()
    }
}

/// Smallest-three quaternion decode, returning `[x, y, z, w]`.
pub(crate) fn decode_smallest_three(r: &[u8]) -> [f32; 4] {
    const MASK: u32 = (1 << 9) - 1;
    let sqrt1_2 = std::f32::consts::FRAC_1_SQRT_2;
    let mut comp = u32::from(r[0])
        | (u32::from(r[1]) << 8)
        | (u32::from(r[2]) << 16)
        | (u32::from(r[3]) << 24);
    let largest = (comp >> 30) as usize;
    let mut q = [0f32; 4];
    let mut sum_sq = 0f32;
    for i in (0..4).rev() {
        if i != largest {
            let mag = comp & MASK;
            let neg = (comp >> 9) & 1;
            comp >>= 10;
            let mut v = sqrt1_2 * mag as f32 / MASK as f32;
            if neg == 1 {
                v = -v;
            }
            q[i] = v;
            sum_sq += v * v;
        }
    }
    q[largest] = (1.0 - sum_sq).max(0.0).sqrt();
    q
}

/// Builds the [`GaussianCloud`] from decoded SPZ values.
fn to_cloud(native: Native) -> GaussianCloud {
    let n = native.n;
    let k = sh_coeffs_for_degree(native.degree);
    let rest = k - 1;
    let mut positions = Vec::with_capacity(n);
    let mut rotations = Vec::with_capacity(n);
    let mut sh = vec![0f32; n * 3 * k];
    for i in 0..n {
        positions.push(native.positions[i]);
        // Native order is [x, y, z, w]; the model uses [w, x, y, z].
        let q = native.rotations[i];
        rotations.push([q[3], q[0], q[1], q[2]]);
        // The reference writes the decoded colour value straight into f_dc.
        let c = native.colors[i];
        for ch in 0..3 {
            sh[i * 3 * k + ch * k] = c[ch];
        }
        for j in 0..rest {
            for ch in 0..3 {
                sh[i * 3 * k + ch * k + 1 + j] = native.sh_rest[(i * rest + j) * 3 + ch];
            }
        }
    }
    // Every column was built from the same `n`, so the constructor's checks
    // pass; `GaussianCloud::new` only fails on length mismatch.
    GaussianCloud::new(
        positions,
        rotations,
        native.scales,
        native.alphas,
        native.degree,
        sh,
    )
    .expect("columns share one point count")
    .with_frame(CoordinateFrame::Rub)
}
