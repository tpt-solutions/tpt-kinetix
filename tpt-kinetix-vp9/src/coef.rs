//! VP9 coefficient (token) decoding for one transform block (§8.6),
//! including dequantization and the non-zero context statistics used by
//! probability adaptation.

use crate::booldec::BoolDecoder;
use crate::header::{coef_full_idx, COEF_FULL_LEN};

/// Decode the coefficient tokens of one transform block.
///
/// `full` is the working expanded 11-node probability tree;
/// `pt_tx` selects the `(tx group, block type, plane type)` slice.
/// `counts_*` accumulate adaptation statistics in the compact band-0 layout.
/// Returns the scan position after the last decoded symbol (the EOB).
#[allow(clippy::too_many_arguments)]
pub fn decode_coeffs_b(
    bc: &mut BoolDecoder,
    n_coeffs: usize,
    is_tx32x32: bool,
    full: &[u8; COEF_FULL_LEN],
    pt_tx: (usize, usize, usize),
    counts_coef: &mut [[u32; 3]],
    counts_eob: &mut [[u32; 2]],
    nnz0: u8,
    scan: &[i16],
    nb: &[i16],
    band_counts: &[i16; 6],
    qmul: [i32; 2],
    coeffs: &mut [i32],
) -> usize {
    // The reference clears dqcoeff around every block; our scratch buffer is
    // reused, so clear the slice to keep positions past the EOB at zero.
    coeffs[..n_coeffs].fill(0);
    let (tx, bt, pt) = pt_tx;
    debug_assert_eq!(scan.len(), n_coeffs);
    debug_assert_eq!(nb.len(), 2 * n_coeffs);

    let probs_row = |band: usize, ctx: usize| -> &[u8; 11] {
        let base = coef_full_idx(tx, bt, pt, band, ctx);
        full[base..base + 11].try_into().expect("fixed 11")
    };
    let bin = |band: usize, ctx: usize| -> usize {
        crate::frame::Counts::coef_bin(tx, bt, pt, band, ctx)
    };

    let mut cache = [0u8; 1024];
    let mut i = 0usize;
    let mut band = 0usize;
    let mut band_left = i32::from(band_counts[0]);
    let mut nnz = nnz0 as usize;
    let mut tp: [u8; 11] = *probs_row(band, nnz);

    // Debug-trace helper mirroring the instrumented libvpx read_bool.
    #[inline]
    fn rd(bc: &mut BoolDecoder, p: u8) -> bool {
        bc.read_bool(p)
    }

    'outer: loop {
        // EOB branch
        let eob = rd(bc, tp[0]);
        let b = bin(band, nnz);
        counts_eob[b][usize::from(eob)] += 1;
        if !eob {
            break;
        }

        loop {
            // ZERO branch (skips the EOB check on the next symbol)
            if !rd(bc, tp[1]) {
                let b = bin(band, nnz);
                counts_coef[b][0] += 1;
                band_left -= 1;
                if band_left == 0 {
                    band += 1;
                    band_left = i32::from(band_counts.get(band).copied().unwrap_or(1));
                }
                cache[scan[i] as usize] = 0;
                i += 1;
                if i == n_coeffs {
                    break 'outer; // invalid: blocks must end with EOB
                }
                nnz = (1
                    + u32::from(cache[nb[2 * i] as usize])
                    + u32::from(cache[nb[2 * i + 1] as usize])) as usize
                    >> 1;
                tp = *probs_row(band, nnz);
                continue;
            }

            let rc = scan[i] as usize;
            let val;
            let cache_val;
            if !rd(bc, tp[2]) {
                let b = bin(band, nnz);
                counts_coef[b][1] += 1;
                val = 1;
                cache_val = 1;
            } else {
                let b = bin(band, nnz);
                counts_coef[b][2] += 1;
                if !rd(bc, tp[3]) {
                    // 2 / 3 / 4
                    if !rd(bc, tp[4]) {
                        cache_val = 2;
                        val = 2;
                    } else {
                        val = 3 + i32::from(rd(bc, tp[5]));
                        cache_val = 3;
                    }
                } else if !rd(bc, tp[6]) {
                    // cat 1 / cat 2
                    cache_val = 4;
                    if !rd(bc, tp[7]) {
                        val = 5 + u32::from(rd(bc, 159)) as i32;
                    } else {
                        val = 7
                            + (u32::from(rd(bc, 165)) << 1) as i32
                            + u32::from(rd(bc, 145)) as i32;
                    }
                } else {
                    // cat 3..6
                    cache_val = 5;
                    if !rd(bc, tp[8]) {
                        if !rd(bc, tp[9]) {
                            val = 11
                                + ((u32::from(rd(bc, 173)) << 2)
                                    | (u32::from(rd(bc, 148)) << 1)
                                    | u32::from(rd(bc, 140)))
                                    as i32;
                        } else {
                            val = 19
                                + ((u32::from(rd(bc, 176)) << 3)
                                    | (u32::from(rd(bc, 155)) << 2)
                                    | (u32::from(rd(bc, 140)) << 1)
                                    | u32::from(rd(bc, 135)))
                                    as i32;
                        }
                    } else if !rd(bc, tp[10]) {
                        val = 35
                            + ((u32::from(rd(bc, 180)) << 4)
                                | (u32::from(rd(bc, 157)) << 3)
                                | (u32::from(rd(bc, 141)) << 2)
                                | (u32::from(rd(bc, 134)) << 1)
                                | u32::from(rd(bc, 130))) as i32;
                    } else {
                        // cat 6 (8-bit: 14 extra bits)
                        let mut v: u32 = 67;
                        v += u32::from(rd(bc, 254)) << 13;
                        v += u32::from(rd(bc, 254)) << 12;
                        v += u32::from(rd(bc, 254)) << 11;
                        v += u32::from(rd(bc, 252)) << 10;
                        v += u32::from(rd(bc, 249)) << 9;
                        v += u32::from(rd(bc, 243)) << 8;
                        v += u32::from(rd(bc, 230)) << 7;
                        v += u32::from(rd(bc, 196)) << 6;
                        v += u32::from(rd(bc, 177)) << 5;
                        v += u32::from(rd(bc, 153)) << 4;
                        v += u32::from(rd(bc, 140)) << 3;
                        v += u32::from(rd(bc, 133)) << 2;
                        v += u32::from(rd(bc, 130)) << 1;
                        v += u32::from(rd(bc, 129));
                        val = v as i32;
                    }
                }
            }
            cache[rc] = cache_val;

            band_left -= 1;
            if band_left == 0 {
                band += 1;
                band_left = i32::from(band_counts.get(band).copied().unwrap_or(1));
            }

            let sign = rd(bc, 128);
            let q = qmul[usize::from(i != 0)];
            let signed_val = if sign { -(val * q) } else { val * q };
            coeffs[rc] = if is_tx32x32 {
                signed_val / 2
            } else {
                signed_val
            };

            i += 1;
            if i >= n_coeffs {
                break 'outer;
            }
            nnz = (1
                + u32::from(cache[nb[2 * i] as usize])
                + u32::from(cache[nb[2 * i + 1] as usize])) as usize
                >> 1;
            tp = *probs_row(band, nnz);
            // after a value symbol the next read is an EOB check
            break;
        }
    }

    i
}
