//! Bit-level I/O and the reversible entropy stage for the lossless codec.
//!
//! The encode side uses a local MSB-first [`BitWriter`]. The decode side reuses
//! `tpt-kinetix-bitstream`'s `BitReader` (DECISION 6 of the design doc: share the
//! extracted bitstream primitives — these originated in `tpt-kinetix-lean` and
//! were factored into `tpt-kinetix-bitstream` per the realtime codec's
//! DECISION 7). Residuals are coded with an adaptive Rice code whose parameter
//! `k` is derived per-sample from the magnitudes of the already-coded left/up
//! residuals, so both encoder and decoder compute it identically without
//! signalling it.

use tpt_kinetix_bitstream::bitreader::BitReader;

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

/// Write a signed residual using a Rice code with parameter `k`.
pub fn write_rice(w: &mut BitWriter, k: u32, residual: i32) {
    let m = map_residual(residual);
    let q = m >> k;
    for _ in 0..q {
        w.write_bit(0);
    }
    w.write_bit(1);
    w.write_bits(m & ((1u32 << k) - 1), k as u8);
}

/// Read a signed residual coded with a Rice code of parameter `k`.
pub fn read_rice(r: &mut BitReader<'_>, k: u32) -> Option<i32> {
    let mut q: u32 = 0;
    loop {
        let bit = r.read_bit()?;
        if bit == 1 {
            break;
        }
        q += 1;
    }
    let rem = r.read_bits(k as u8)?;
    let m = (q << k) | rem;
    Some(unmap_residual(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rice_roundtrip_various_k() {
        let values = [0i32, 1, -1, 2, -2, 255, -255, 4096, -4096, 32767, -32768];
        for &v in &values {
            for k in 0..=8u32 {
                let mut w = BitWriter::new();
                write_rice(&mut w, k, v);
                let bytes = w.finish();
                let mut r = BitReader::new(&bytes);
                let got = read_rice(&mut r, k).unwrap();
                assert_eq!(got, v, "k={k}, v={v}");
            }
        }
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
