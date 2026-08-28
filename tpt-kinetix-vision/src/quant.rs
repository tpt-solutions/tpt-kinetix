pub const MATRIX_ID_DETECTION_AGGRESSIVE: u8 = 0;
pub const MATRIX_ID_BALANCED: u8 = 1;
pub const MATRIX_ID_CONSERVATIVE: u8 = 2;
pub const MATRIX_ID_EMBEDDED: u8 = 3;

const MATRIX_AGGRESSIVE: [[u8; 8]; 8] = [
    [1, 1, 1, 2, 3, 4, 6, 8],
    [1, 1, 1, 2, 3, 4, 6, 8],
    [1, 1, 2, 3, 4, 6, 8, 12],
    [2, 2, 3, 4, 6, 8, 12, 16],
    [3, 3, 4, 6, 8, 12, 16, 24],
    [4, 4, 6, 8, 12, 16, 24, 32],
    [6, 6, 8, 12, 16, 24, 32, 48],
    [8, 8, 12, 16, 24, 32, 48, 64],
];

const MATRIX_BALANCED: [[u8; 8]; 8] = [
    [1, 1, 2, 3, 5, 8, 12, 16],
    [1, 2, 2, 4, 6, 9, 13, 18],
    [2, 2, 3, 5, 8, 12, 16, 22],
    [3, 4, 5, 7, 10, 14, 19, 25],
    [5, 6, 8, 10, 13, 17, 22, 28],
    [8, 9, 12, 14, 17, 21, 26, 32],
    [12, 13, 16, 19, 22, 26, 31, 37],
    [16, 18, 22, 25, 28, 32, 37, 43],
];

const MATRIX_CONSERVATIVE: [[u8; 8]; 8] = [
    [1, 1, 1, 2, 3, 4, 5, 7],
    [1, 1, 2, 2, 3, 4, 6, 7],
    [1, 2, 2, 3, 4, 5, 7, 8],
    [2, 2, 3, 4, 5, 6, 8, 10],
    [3, 3, 4, 5, 6, 8, 10, 12],
    [4, 4, 5, 6, 8, 10, 12, 14],
    [5, 6, 7, 8, 10, 12, 14, 17],
    [7, 7, 8, 10, 12, 14, 17, 20],
];

fn get_matrix(id: u8) -> &'static [[u8; 8]; 8] {
    match id {
        MATRIX_ID_DETECTION_AGGRESSIVE => &MATRIX_AGGRESSIVE,
        MATRIX_ID_BALANCED => &MATRIX_BALANCED,
        MATRIX_ID_CONSERVATIVE => &MATRIX_CONSERVATIVE,
        _ => &MATRIX_AGGRESSIVE,
    }
}

#[inline]
pub fn quantize(value: i32, position: (usize, usize), matrix_id: u8) -> i32 {
    let matrix = get_matrix(matrix_id);
    let q = matrix[position.1][position.0] as i32;
    if value >= 0 {
        (value + q / 2) / q
    } else {
        (value - q / 2) / q
    }
}

#[inline]
pub fn dequantize(coeff: i32, position: (usize, usize), matrix_id: u8) -> i32 {
    let matrix = get_matrix(matrix_id);
    let q = matrix[position.1][position.0] as i32;
    coeff * q
}

pub fn matrix_value(matrix_id: u8, x: usize, y: usize) -> u8 {
    let matrix = get_matrix(matrix_id);
    matrix[y][x]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggressive_matrix_dc_is_one() {
        assert_eq!(matrix_value(0, 0, 0), 1);
    }

    #[test]
    fn aggressive_matrix_high_freq_is_coarse() {
        assert_eq!(matrix_value(0, 7, 7), 64);
    }

    #[test]
    fn quant_dequant_roundtrip_dc() {
        let val = 128;
        let q = quantize(val, (0, 0), 0);
        let dq = dequantize(q, (0, 0), 0);
        assert_eq!(dq, val);
    }

    #[test]
    fn aggressive_quantizes_high_freq() {
        let val = 100;
        let q_low = quantize(val, (0, 0), 0);
        let q_high = quantize(val, (7, 7), 0);
        assert!(q_high.abs() <= q_low.abs());
    }

    #[test]
    fn balanced_is_less_aggressive() {
        let val = 100;
        let q_agg = quantize(val, (7, 7), 0);
        let q_bal = quantize(val, (7, 7), 1);
        assert!(q_bal.abs() > q_agg.abs());
    }

    #[test]
    fn conservative_preserves_more() {
        let val = 100;
        let q_bal = quantize(val, (7, 7), 1);
        let q_con = quantize(val, (7, 7), 2);
        assert!(q_con.abs() > q_bal.abs());
    }

    #[test]
    fn embedded_id_falls_back_to_aggressive() {
        assert_eq!(matrix_value(3, 0, 0), 1);
        assert_eq!(matrix_value(3, 7, 7), 64);
    }
}
