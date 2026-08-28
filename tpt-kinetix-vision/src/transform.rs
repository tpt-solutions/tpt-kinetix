use crate::quant::{dequantize, quantize};

fn hadamard_matrix(n: usize) -> Vec<Vec<i32>> {
    debug_assert!(n.is_power_of_two(), "transform size must be a power of two");
    let mut m = vec![vec![1i32; 1]; 1];
    let mut size = 1;
    while size < n {
        let new_size = size * 2;
        let mut nm = vec![vec![0i32; new_size]; new_size];
        for i in 0..size {
            for j in 0..size {
                nm[i][j] = m[i][j];
                nm[i][j + size] = m[i][j];
                nm[i + size][j] = m[i][j];
                nm[i + size][j + size] = -m[i][j];
            }
        }
        m = nm;
        size = new_size;
    }
    m
}

#[inline]
fn hadamard_2d_raw(src: &[i32], n: usize, dst: &mut [i32]) {
    debug_assert_eq!(src.len(), n * n);
    debug_assert_eq!(dst.len(), n * n);
    let h = hadamard_matrix(n);
    for i in 0..n {
        for j in 0..n {
            let mut s = 0i64;
            for k in 0..n {
                let mut inner = 0i64;
                for l in 0..n {
                    inner += src[k * n + l] as i64 * h[j][l] as i64;
                }
                s += h[i][k] as i64 * inner;
            }
            dst[i * n + j] = s as i32;
        }
    }
}

pub fn transform_2d(src: &[i32], n: usize, dst: &mut [i32]) {
    hadamard_2d_raw(src, n, dst);
}

pub fn inverse_2d(src: &[i32], n: usize, dst: &mut [i32]) {
    debug_assert_eq!(src.len(), n * n);
    debug_assert_eq!(dst.len(), n * n);
    let scale = (n * n) as i64;
    let mut tmp = vec![0i32; n * n];
    hadamard_2d_raw(src, n, &mut tmp);
    for (i, v) in tmp.iter().enumerate() {
        dst[i] = (*v as i64).div_euclid(scale) as i32;
    }
}

pub fn hadamard_2d(src: &[i32; 16], dst: &mut [i32; 16]) {
    hadamard_2d_raw(src, 4, dst);
}

pub fn inverse_hadamard_2d(src: &[i32; 16], dst: &mut [i32; 16]) {
    let mut tmp = [0i32; 16];
    hadamard_2d_raw(src, 4, &mut tmp);
    for (i, v) in tmp.iter().enumerate() {
        dst[i] = (*v as i64).div_euclid(16) as i32;
    }
}

pub fn quant_coeff(val: i32, pos: (usize, usize), matrix_id: u8) -> i32 {
    quantize(val, pos, matrix_id)
}

pub fn dequant_coeff(val: i32, pos: (usize, usize), matrix_id: u8) -> i32 {
    dequantize(val, pos, matrix_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idct_round_trip(n: usize) {
        let mut src = vec![0i32; n * n];
        for (i, v) in src.iter_mut().enumerate() {
            *v = ((i * 7 + 3) % 17) as i32 - 8;
        }
        let mut coeffs = vec![0i32; n * n];
        transform_2d(&src, n, &mut coeffs);
        let mut out = vec![0i32; n * n];
        inverse_2d(&coeffs, n, &mut out);
        for (a, b) in src.iter().zip(out.iter()) {
            assert_eq!(*a, *b, "transform round-trip mismatch at n={n}");
        }
    }

    #[test]
    fn dct_round_trip_4() {
        idct_round_trip(4);
    }

    #[test]
    fn dct_round_trip_8() {
        idct_round_trip(8);
    }

    #[test]
    fn dct_round_trip_16() {
        idct_round_trip(16);
    }

    #[test]
    fn hadamard_round_trip() {
        let src = [1, -2, 5, 0, 3, 4, -1, 2, 0, -3, 6, 1, 2, -1, -2, 3];
        let mut tmp = [0i32; 16];
        let mut out = [0i32; 16];
        hadamard_2d(&src, &mut tmp);
        inverse_hadamard_2d(&tmp, &mut out);
        assert_eq!(src, out);
    }

    #[test]
    fn quant_coeff_uses_matrix() {
        let val = 128;
        let q_dc = quant_coeff(val, (0, 0), 0);
        let q_hf = quant_coeff(val, (7, 7), 0);
        assert!(q_hf.abs() < q_dc.abs());
    }

    #[test]
    fn dequant_coeff_roundtrip_dc() {
        let val = 64;
        let q = quant_coeff(val, (0, 0), 0);
        let dq = dequant_coeff(q, (0, 0), 0);
        assert_eq!(dq, val);
    }
}
