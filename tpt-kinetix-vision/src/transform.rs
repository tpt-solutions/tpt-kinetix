//! Integer Walsh–Hadamard transform bank (same math as lean).

fn hadamard_matrix(n: usize) -> Vec<Vec<i32>> {
    debug_assert!(n.is_power_of_two());
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

fn hadamard_2d_raw(src: &[i32], n: usize, dst: &mut [i32]) {
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
    let scale = (n * n) as i64;
    let mut tmp = vec![0i32; n * n];
    hadamard_2d_raw(src, n, &mut tmp);
    for (i, v) in tmp.iter().enumerate() {
        dst[i] = (*v as i64).div_euclid(scale) as i32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_4() {
        let src = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160];
        let mut coeffs = vec![0i32; 16];
        transform_2d(&src, 4, &mut coeffs);
        let mut out = vec![0i32; 16];
        inverse_2d(&coeffs, 4, &mut out);
        for (a, b) in src.iter().zip(out.iter()) {
            assert_eq!(a, b);
        }
    }
}
