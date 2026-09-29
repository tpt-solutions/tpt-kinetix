//! Quantization matrices optimized for ML inference accuracy.

/// Built-in quantization matrix ID 0 (detection-aggressive).
/// Low frequencies preserved; high frequencies aggressively coarsened.
pub const MATRIX_AGGRESSIVE: [[u8; 8]; 8] = [
    [1, 1, 1, 2, 3, 4, 6, 8],
    [1, 1, 1, 2, 3, 4, 6, 8],
    [1, 1, 2, 3, 4, 6, 8, 12],
    [2, 2, 3, 4, 6, 8, 12, 16],
    [3, 3, 4, 6, 8, 12, 16, 24],
    [4, 4, 6, 8, 12, 16, 24, 32],
    [6, 6, 8, 12, 16, 24, 32, 48],
    [8, 8, 12, 16, 24, 32, 48, 64],
];

/// Built-in quantization matrix ID 1 (balanced).
pub const MATRIX_BALANCED: [[u8; 8]; 8] = [
    [1, 1, 1, 1, 2, 3, 4, 6],
    [1, 1, 1, 1, 2, 3, 4, 6],
    [1, 1, 1, 2, 3, 4, 6, 8],
    [1, 1, 2, 3, 4, 6, 8, 12],
    [2, 2, 3, 4, 6, 8, 12, 16],
    [3, 3, 4, 6, 8, 12, 16, 24],
    [4, 4, 6, 8, 12, 16, 24, 32],
    [6, 6, 8, 12, 16, 24, 32, 48],
];

/// Built-in quantization matrix ID 2 (conservative, near-flat).
pub const MATRIX_CONSERVATIVE: [[u8; 8]; 8] = [
    [1, 1, 1, 1, 1, 2, 2, 3],
    [1, 1, 1, 1, 1, 2, 2, 3],
    [1, 1, 1, 1, 2, 2, 3, 4],
    [1, 1, 1, 2, 2, 3, 4, 5],
    [1, 1, 2, 2, 3, 4, 5, 6],
    [2, 2, 2, 3, 4, 5, 6, 8],
    [2, 2, 3, 4, 5, 6, 8, 10],
    [3, 3, 4, 5, 6, 8, 10, 12],
];

/// Get the quantization matrix for a given ID (0-2 built-in).
pub fn quant_matrix(id: u8) -> &'static [[u8; 8]; 8] {
    match id {
        0 => &MATRIX_AGGRESSIVE,
        1 => &MATRIX_BALANCED,
        _ => &MATRIX_CONSERVATIVE,
    }
}

/// Map coefficient index `k` of an `n x n` transform block onto the 8x8
/// quantization matrix, folding by frequency so a 16x16/32x32/64x64 block
/// reuses the matrix at proportionally scaled positions (identity at n = 8).
///
/// The header allows block sizes 8..64 (log2 3..=6); the built-in matrices are
/// fixed 8x8, so without this folding any larger block would index out of
/// bounds.
#[inline]
pub fn matrix_pos(k: usize, n: usize) -> (usize, usize) {
    debug_assert!((4..=64).contains(&n) && n.is_power_of_two());
    let fold = |v: usize| v * 8 / n;
    (fold(k / n), fold(k % n))
}

/// Quantize a coefficient at position (r, c) using the given matrix.
#[inline]
pub fn quantize(coeff: i32, matrix: &[[u8; 8]; 8], r: usize, c: usize, qp: u8) -> i32 {
    let step = matrix[r][c] as i32 * (qp as i32 + 1);
    let step = step.max(1);
    if coeff >= 0 {
        (coeff + step / 2) / step
    } else {
        (coeff - step / 2) / step
    }
}

/// Dequantize a coefficient at position (r, c) using the given matrix.
#[inline]
pub fn dequantize(coeff: i32, matrix: &[[u8; 8]; 8], r: usize, c: usize, qp: u8) -> i32 {
    coeff * matrix[r][c] as i32 * (qp as i32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qp0_is_lossless_round_trip() {
        let matrix = quant_matrix(0);
        // At qp=0, quant uses step = matrix[r][c]. For matrix=1 positions this is
        // exactly lossless; for matrix>1 positions there is rounding. Verify the
        // matrix=1 positions round-trip exactly and others are within 1 level.
        for r in 0..8 {
            for c in 0..8 {
                let q = quantize(100, matrix, r, c, 0);
                let dq = dequantize(q, matrix, r, c, 0);
                let diff = (dq - 100).abs();
                if matrix[r][c] == 1 {
                    assert_eq!(dq, 100, "qp=0 lossless mismatch at ({r},{c})");
                } else {
                    assert!(
                        diff <= matrix[r][c] as i32,
                        "qp=0 rounding too large at ({r},{c})"
                    );
                }
            }
        }
    }

    #[test]
    fn aggressive_preserves_dc() {
        let matrix = quant_matrix(0);
        // DC coefficient (0,0) has matrix value 1 → minimal quantization.
        let q = quantize(1000, matrix, 0, 0, 4);
        let dq = dequantize(q, matrix, 0, 0, 4);
        assert_eq!(dq, 1000, "DC should be preserved at qp=4");
    }

    #[test]
    fn aggressive_coarsens_hf() {
        let matrix = quant_matrix(0);
        // High-frequency coefficient (7,7) has matrix value 64 → heavy quantization.
        let q = quantize(100, matrix, 7, 7, 4);
        assert_eq!(q, 0, "HF should be heavily quantized");
    }

    #[test]
    fn matrix_pos_is_identity_at_8() {
        for k in 0..64 {
            assert_eq!(matrix_pos(k, 8), (k / 8, k % 8));
        }
    }

    #[test]
    fn matrix_pos_folds_larger_blocks_in_bounds() {
        // Every coefficient of every supported block size must fold to a
        // valid 8x8 matrix position (this was an out-of-bounds panic before
        // the folding was introduced). 4x4 is the chroma block size.
        for log2 in 2..=6 {
            let n = 1usize << log2;
            for k in 0..n * n {
                let (r, c) = matrix_pos(k, n);
                assert!(r < 8 && c < 8, "k={k} n={n} -> ({r},{c})");
            }
        }
        // DC folds to DC at every block size.
        for log2 in 2..=6 {
            let n = 1usize << log2;
            assert_eq!(matrix_pos(0, n), (0, 0));
        }
    }
}
