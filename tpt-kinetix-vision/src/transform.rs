//! Integer Walsh–Hadamard transform bank (same math as lean).

use std::sync::OnceLock;

/// Build the row-major `H_n` for a power-of-two `n`.
///
/// Called once per size and cached: `hadamard_2d_raw` runs once per block per
/// frame in both the encode and the decode direction, and this used to allocate
/// a nested `Vec<Vec<i32>>` (`1 + n` heap allocations) plus `O(n² log n)` work on
/// *every* call. Entries and accumulation order in the transform are unchanged,
/// so the result stays bit-exact.
fn build_hadamard(n: usize) -> Vec<i32> {
    debug_assert!(n.is_power_of_two());
    let mut m = vec![1i32; 1];
    let mut size = 1;
    while size < n {
        let new_size = size * 2;
        let mut nm = vec![0i32; new_size * new_size];
        for i in 0..size {
            for j in 0..size {
                let v = m[i * size + j];
                nm[i * new_size + j] = v;
                nm[i * new_size + (j + size)] = v;
                nm[(i + size) * new_size + j] = v;
                nm[(i + size) * new_size + (j + size)] = -v;
            }
        }
        m = nm;
        size = new_size;
    }
    m
}

/// Largest transform/block size supported, as a power of two (64).
const MAX_HADAMARD_LOG2: usize = 6;

/// One cached matrix per size, indexed by `log2(n)`. `OnceLock` keeps this
/// correct if several frames are decoded concurrently.
static HADAMARD: [OnceLock<Vec<i32>>; MAX_HADAMARD_LOG2 + 1] =
    [const { OnceLock::new() }; MAX_HADAMARD_LOG2 + 1];

/// Cached row-major `H_n`, built on first use for this size.
#[inline]
fn hadamard_matrix(n: usize) -> &'static [i32] {
    let log2 = n.trailing_zeros() as usize;
    assert!(
        n.is_power_of_two() && log2 <= MAX_HADAMARD_LOG2,
        "transform size {n} is not a supported power of two (<= {})",
        1usize << MAX_HADAMARD_LOG2
    );
    HADAMARD[log2].get_or_init(|| build_hadamard(n)).as_slice()
}

fn hadamard_2d_raw(src: &[i32], n: usize, dst: &mut [i32]) {
    let h = hadamard_matrix(n);
    for i in 0..n {
        for j in 0..n {
            let mut s = 0i64;
            for k in 0..n {
                let mut inner = 0i64;
                for l in 0..n {
                    inner += src[k * n + l] as i64 * h[j * n + l] as i64;
                }
                s += h[i * n + k] as i64 * inner;
            }
            dst[i * n + j] = s as i32;
        }
    }
}

pub fn transform_2d(src: &[i32], n: usize, dst: &mut [i32]) {
    hadamard_2d_raw(src, n, dst);
}

/// Inverse of [`transform_2d`] into caller-supplied scratch.
///
/// `scratch` must be at least `n * n` long and is fully overwritten. Split out
/// so the per-block reconstruction loop can hand in a buffer it already owns
/// instead of allocating one per block. The accumulation order is identical to
/// [`inverse_2d`], so the two agree bit for bit.
pub fn inverse_2d_with_scratch(src: &[i32], n: usize, dst: &mut [i32], scratch: &mut [i32]) {
    debug_assert!(scratch.len() >= n * n);
    let scale = (n * n) as i64;
    let tmp = &mut scratch[..n * n];
    hadamard_2d_raw(src, n, tmp);
    for (i, v) in tmp.iter().enumerate() {
        dst[i] = (*v as i64).div_euclid(scale) as i32;
    }
}

pub fn inverse_2d(src: &[i32], n: usize, dst: &mut [i32]) {
    let mut scratch = vec![0i32; n * n];
    inverse_2d_with_scratch(src, n, dst, &mut scratch);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_4() {
        let src = vec![
            10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160,
        ];
        let mut coeffs = vec![0i32; 16];
        transform_2d(&src, 4, &mut coeffs);
        let mut out = vec![0i32; 16];
        inverse_2d(&coeffs, 4, &mut out);
        for (a, b) in src.iter().zip(out.iter()) {
            assert_eq!(a, b);
        }
    }
}
