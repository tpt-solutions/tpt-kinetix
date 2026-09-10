//! Masked-compound blend masks (AV1 spec §7.11.3.11–§7.11.3.14).
//!
//! * [`wedge_mask`] — the `WedgeMasks[bsize][sign][index]` lookup, generated
//!   once from the spec's 1-D master tables (§7.11.3.11).
//! * [`diffwtd_mask`] — the difference-weighted mask derived from the two
//!   predictions (§7.11.3.12).
//! * [`mask_blend`] — combine two intermediate-domain "prep" predictions
//!   through a luma-domain mask, sub-sampling for chroma (§7.11.3.14).
//!
//! 8-bit only: `InterPostRound = 2 * FILTER_BITS - (InterRound0 + InterRound1)
//! = 14 - (3 + 7) = 4`, so the final mask-blend shift is `6 + 4 = 10` and the
//! diffwtd difference is rounded by `(BitDepth - 8) + InterPostRound = 4`.

use std::sync::OnceLock;

use super::{BLOCK_HEIGHT, BLOCK_SIZES, BLOCK_WIDTH};

const MASK_MASTER_SIZE: usize = 64;
const WEDGE_TYPES: usize = 16;
const INTER_POST_ROUND: u32 = 4;

// Wedge direction indices into `MasterMask`.
const WEDGE_HORIZONTAL: usize = 0;
const WEDGE_VERTICAL: usize = 1;
const WEDGE_OBLIQUE27: usize = 2;
const WEDGE_OBLIQUE63: usize = 3;
const WEDGE_OBLIQUE117: usize = 4;
const WEDGE_OBLIQUE153: usize = 5;

/// `Wedge_Bits[BLOCK_SIZES]` (spec §10): non-zero ⇒ the size carries wedge
/// masks. All non-zero entries are `4` (`WEDGE_TYPES == 16`).
const WEDGE_BITS: [u8; BLOCK_SIZES] = [
    0, 0, 0, 4, 4, 4, 4, 4, 4, 4, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 0, 0,
];

#[rustfmt::skip]
const WEDGE_MASTER_OBLIQUE_ODD: [i32; MASK_MASTER_SIZE] = [
    0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
    0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  1,  2,  6,  18,
    37, 53, 60, 63, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
    64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
];
#[rustfmt::skip]
const WEDGE_MASTER_OBLIQUE_EVEN: [i32; MASK_MASTER_SIZE] = [
    0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
    0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  1,  4,  11, 27,
    46, 58, 62, 63, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
    64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
];
#[rustfmt::skip]
const WEDGE_MASTER_VERTICAL: [i32; MASK_MASTER_SIZE] = [
    0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
    0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  2,  7,  21,
    43, 57, 62, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
    64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
];

/// `Wedge_Codebook[3][16][3]` — `{ direction, xoff, yoff }` per block shape
/// (0 = tall, 1 = wide, 2 = square).
#[rustfmt::skip]
const WEDGE_CODEBOOK: [[[usize; 3]; WEDGE_TYPES]; 3] = [
    [
        [WEDGE_OBLIQUE27, 4, 4], [WEDGE_OBLIQUE63, 4, 4],
        [WEDGE_OBLIQUE117, 4, 4], [WEDGE_OBLIQUE153, 4, 4],
        [WEDGE_HORIZONTAL, 4, 2], [WEDGE_HORIZONTAL, 4, 4],
        [WEDGE_HORIZONTAL, 4, 6], [WEDGE_VERTICAL, 4, 4],
        [WEDGE_OBLIQUE27, 4, 2], [WEDGE_OBLIQUE27, 4, 6],
        [WEDGE_OBLIQUE153, 4, 2], [WEDGE_OBLIQUE153, 4, 6],
        [WEDGE_OBLIQUE63, 2, 4], [WEDGE_OBLIQUE63, 6, 4],
        [WEDGE_OBLIQUE117, 2, 4], [WEDGE_OBLIQUE117, 6, 4],
    ],
    [
        [WEDGE_OBLIQUE27, 4, 4], [WEDGE_OBLIQUE63, 4, 4],
        [WEDGE_OBLIQUE117, 4, 4], [WEDGE_OBLIQUE153, 4, 4],
        [WEDGE_VERTICAL, 2, 4], [WEDGE_VERTICAL, 4, 4],
        [WEDGE_VERTICAL, 6, 4], [WEDGE_HORIZONTAL, 4, 4],
        [WEDGE_OBLIQUE27, 4, 2], [WEDGE_OBLIQUE27, 4, 6],
        [WEDGE_OBLIQUE153, 4, 2], [WEDGE_OBLIQUE153, 4, 6],
        [WEDGE_OBLIQUE63, 2, 4], [WEDGE_OBLIQUE63, 6, 4],
        [WEDGE_OBLIQUE117, 2, 4], [WEDGE_OBLIQUE117, 6, 4],
    ],
    [
        [WEDGE_OBLIQUE27, 4, 4], [WEDGE_OBLIQUE63, 4, 4],
        [WEDGE_OBLIQUE117, 4, 4], [WEDGE_OBLIQUE153, 4, 4],
        [WEDGE_HORIZONTAL, 4, 2], [WEDGE_HORIZONTAL, 4, 6],
        [WEDGE_VERTICAL, 2, 4], [WEDGE_VERTICAL, 6, 4],
        [WEDGE_OBLIQUE27, 4, 2], [WEDGE_OBLIQUE27, 4, 6],
        [WEDGE_OBLIQUE153, 4, 2], [WEDGE_OBLIQUE153, 4, 6],
        [WEDGE_OBLIQUE63, 2, 4], [WEDGE_OBLIQUE63, 6, 4],
        [WEDGE_OBLIQUE117, 2, 4], [WEDGE_OBLIQUE117, 6, 4],
    ],
];

fn block_shape(bsize: usize) -> usize {
    let w4 = BLOCK_WIDTH[bsize] / 4;
    let h4 = BLOCK_HEIGHT[bsize] / 4;
    if h4 > w4 {
        0
    } else if h4 < w4 {
        1
    } else {
        2
    }
}

#[inline]
fn clip3(lo: i32, hi: i32, v: i32) -> i32 {
    v.clamp(lo, hi)
}

/// All wedge masks for every wedge-enabled block size, flattened to
/// `masks[bsize][sign][index]` where each mask is a `w * h` row-major
/// `Vec<u8>` of 0..=64 weights.
type WedgeTable = Vec<[[Vec<u8>; WEDGE_TYPES]; 2]>;

fn wedge_table() -> &'static WedgeTable {
    static TABLE: OnceLock<WedgeTable> = OnceLock::new();
    TABLE.get_or_init(build_wedge_table)
}

fn build_wedge_table() -> WedgeTable {
    // Build the 64×64 master masks for the six directions.
    let n = MASK_MASTER_SIZE;
    let mut master = vec![[0i32; MASK_MASTER_SIZE * MASK_MASTER_SIZE]; 6];
    {
        let m = &mut master;
        for j in 0..n {
            let mut shift = (MASK_MASTER_SIZE / 4) as i32;
            let mut i = 0;
            while i < n {
                let e = WEDGE_MASTER_OBLIQUE_EVEN
                    [clip3(0, n as i32 - 1, j as i32 - shift) as usize];
                m[WEDGE_OBLIQUE63][i * n + j] = e;
                shift -= 1;
                let o = WEDGE_MASTER_OBLIQUE_ODD
                    [clip3(0, n as i32 - 1, j as i32 - shift) as usize];
                m[WEDGE_OBLIQUE63][(i + 1) * n + j] = o;
                m[WEDGE_VERTICAL][i * n + j] = WEDGE_MASTER_VERTICAL[j];
                m[WEDGE_VERTICAL][(i + 1) * n + j] = WEDGE_MASTER_VERTICAL[j];
                i += 2;
            }
        }
        for i in 0..n {
            for j in 0..n {
                let msk = m[WEDGE_OBLIQUE63][i * n + j];
                m[WEDGE_OBLIQUE27][j * n + i] = msk;
                m[WEDGE_OBLIQUE117][i * n + (n - 1 - j)] = 64 - msk;
                m[WEDGE_OBLIQUE153][(n - 1 - j) * n + i] = 64 - msk;
                m[WEDGE_HORIZONTAL][j * n + i] = m[WEDGE_VERTICAL][i * n + j];
            }
        }
    }

    let empty = || std::array::from_fn(|_| Vec::new());
    let mut table: WedgeTable = (0..BLOCK_SIZES).map(|_| [empty(), empty()]).collect();

    for bsize in 0..BLOCK_SIZES {
        if WEDGE_BITS[bsize] == 0 {
            continue;
        }
        let w = BLOCK_WIDTH[bsize];
        let h = BLOCK_HEIGHT[bsize];
        let shape = block_shape(bsize);
        for wedge in 0..WEDGE_TYPES {
            let [dir, xo, yo] = WEDGE_CODEBOOK[shape][wedge];
            let xoff = (MASK_MASTER_SIZE / 2) as i32 - ((xo * w) >> 3) as i32;
            let yoff = (MASK_MASTER_SIZE / 2) as i32 - ((yo * h) >> 3) as i32;
            let at = |y: i32, x: i32| master[dir][y as usize * n + x as usize];
            let mut sum = 0i32;
            for i in 0..w as i32 {
                sum += at(yoff, xoff + i);
            }
            for i in 1..h as i32 {
                sum += at(yoff + i, xoff);
            }
            let avg = (sum + (w as i32 + h as i32 - 1) / 2) / (w as i32 + h as i32 - 1);
            let flip = avg < 32;
            let mut m0 = vec![0u8; w * h];
            let mut m1 = vec![0u8; w * h];
            for i in 0..h {
                for j in 0..w {
                    let v = at(yoff + i as i32, xoff + j as i32);
                    m0[i * w + j] = v as u8;
                    m1[i * w + j] = (64 - v) as u8;
                }
            }
            // `WedgeMasks[bsize][flip]` = master, `[!flip]` = 64 - master.
            if flip {
                table[bsize][1][wedge] = m0;
                table[bsize][0][wedge] = m1;
            } else {
                table[bsize][0][wedge] = m0;
                table[bsize][1][wedge] = m1;
            }
        }
    }
    table
}

/// `WedgeMasks[bsize][wedge_sign][wedge_index]` — a `BLOCK_WIDTH[bsize] *
/// BLOCK_HEIGHT[bsize]` row-major mask of 0..=64 luma blend weights.
pub(super) fn wedge_mask(bsize: usize, wedge_sign: bool, wedge_index: usize) -> Vec<u8> {
    let idx = wedge_index.min(WEDGE_TYPES - 1);
    let m = &wedge_table()[bsize][wedge_sign as usize][idx];
    if m.is_empty() {
        // Not a wedge-enabled size (shouldn't happen — caller gates on
        // `wedge_allowed`); fall back to a flat half-weight mask.
        vec![32u8; BLOCK_WIDTH[bsize] * BLOCK_HEIGHT[bsize]]
    } else {
        m.clone()
    }
}

/// Difference-weighted mask (§7.11.3.12) from the two intermediate-domain
/// predictions. `mask_type == true` inverts (`DIFFWTD_38_INV`).
pub(super) fn diffwtd_mask(mask_type: bool, p0: &[i32], p1: &[i32], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    for i in 0..w * h {
        let diff = (p0[i] - p1[i]).abs();
        let diff = round2(diff, INTER_POST_ROUND);
        let m = clip3(0, 64, 38 + diff / 16);
        out[i] = if mask_type { (64 - m) as u8 } else { m as u8 };
    }
    out
}

#[inline]
fn round2(x: i32, n: u32) -> i32 {
    if n == 0 {
        x
    } else {
        (x + (1 << (n - 1))) >> n
    }
}

/// Mask blend (§7.11.3.14) of two intermediate-domain "prep" predictions
/// `p0`/`p1` (each `w * h`) through `mask` (luma-domain `(weights, mw, mh)`).
/// `subx`/`suby` select the plane sub-sampling for the mask fetch.
pub(super) fn mask_blend(
    mask: Option<&(Vec<u8>, usize, usize)>,
    subx: usize,
    suby: usize,
    p0: &[i32],
    p1: &[i32],
    w: usize,
    h: usize,
) -> Vec<u8> {
    let Some((mvec, mw, mh)) = mask else {
        // No mask recorded (e.g. plane-0 call failed to store one); average.
        return p0
            .iter()
            .zip(p1)
            .map(|(&a, &b)| round2(a + b, 1 + INTER_POST_ROUND).clamp(0, 255) as u8)
            .collect();
    };
    let mw = *mw;
    let mh = *mh;
    let fetch = |y: usize, x: usize| -> i32 {
        let yy = y.min(mh - 1);
        let xx = x.min(mw - 1);
        mvec[yy * mw + xx] as i32
    };
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let m = if subx == 0 && suby == 0 {
                fetch(y, x)
            } else if subx == 1 && suby == 0 {
                round2(fetch(y, 2 * x) + fetch(y, 2 * x + 1), 1)
            } else {
                round2(
                    fetch(2 * y, 2 * x)
                        + fetch(2 * y, 2 * x + 1)
                        + fetch(2 * y + 1, 2 * x)
                        + fetch(2 * y + 1, 2 * x + 1),
                    2,
                )
            };
            let v = round2(m * p0[y * w + x] + (64 - m) * p1[y * w + x], 6 + INTER_POST_ROUND);
            out[y * w + x] = v.clamp(0, 255) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wedge_table_populated_for_enabled_sizes() {
        let t = wedge_table();
        for bsize in 0..BLOCK_SIZES {
            let enabled = WEDGE_BITS[bsize] != 0;
            let present = !t[bsize][0][0].is_empty();
            assert_eq!(enabled, present, "bsize {bsize}");
            if enabled {
                let sz = BLOCK_WIDTH[bsize] * BLOCK_HEIGHT[bsize];
                for signed in &t[bsize] {
                    for mask in signed {
                        assert_eq!(mask.len(), sz);
                        assert!(mask.iter().all(|&v| v <= 64));
                    }
                }
            }
        }
    }

    #[test]
    fn wedge_sign_is_complement() {
        // WedgeMasks[bsize][0] + WedgeMasks[bsize][1] == 64 everywhere.
        let m0 = wedge_mask(super::super::BLOCK_16X16, false, 5);
        let m1 = wedge_mask(super::super::BLOCK_16X16, true, 5);
        for (a, b) in m0.iter().zip(&m1) {
            assert_eq!(a + b, 64);
        }
    }

    #[test]
    fn wedge_horizontal_16x16_matches_master() {
        // Codebook[square][5] = { WEDGE_HORIZONTAL, 4, 6 }: yoff = 32 -
        // ((6*16)>>3) = 20. A horizontal wedge is constant across each row, with
        // row r taking Wedge_Master_Vertical[20 + r]. The strip average (~12) is
        // below 32 ⇒ flipSign, so *sign == true* carries the master values.
        let m = wedge_mask(super::super::BLOCK_16X16, true, 5);
        for row in 0..16 {
            let want = WEDGE_MASTER_VERTICAL[20 + row] as u8;
            assert!(
                m[row * 16..row * 16 + 16].iter().all(|&v| v == want),
                "row {row}: {:?} != {want}",
                &m[row * 16..row * 16 + 16]
            );
        }
        // sign == false is the exact complement.
        let mf = wedge_mask(super::super::BLOCK_16X16, false, 5);
        for (a, b) in m.iter().zip(&mf) {
            assert_eq!(a + b, 64);
        }
    }

    #[test]
    fn diffwtd_basic() {
        // Equal predictions ⇒ diff 0 ⇒ m = 38.
        let p = vec![100i32; 16];
        let m = diffwtd_mask(false, &p, &p, 4, 4);
        assert!(m.iter().all(|&v| v == 38));
        let mi = diffwtd_mask(true, &p, &p, 4, 4);
        assert!(mi.iter().all(|&v| v == 64 - 38));
        // diff 4000 ⇒ Round2(4000, 4) = 250 ⇒ m = 38 + 250/16 = 53.
        let a = vec![4000i32; 4];
        let b = vec![0i32; 4];
        let m = diffwtd_mask(false, &a, &b, 2, 2);
        assert!(m.iter().all(|&v| v == 53));
        // Very large difference saturates to 64.
        let a = vec![7000i32; 4];
        let m = diffwtd_mask(false, &a, &b, 2, 2);
        assert!(m.iter().all(|&v| v == 64));
    }

    #[test]
    fn mask_blend_luma_extremes() {
        let p0 = vec![200i32; 4];
        let p1 = vec![40i32; 4];
        // m = 64 everywhere ⇒ output = Round2(64*p0, 10) = p0>>4 rounded.
        let mask = (vec![64u8; 4], 2, 2);
        let out = mask_blend(Some(&mask), 0, 0, &p0, &p1, 2, 2);
        let want = round2(64 * 200, 10).clamp(0, 255) as u8;
        assert!(out.iter().all(|&v| v == want));
    }
}
