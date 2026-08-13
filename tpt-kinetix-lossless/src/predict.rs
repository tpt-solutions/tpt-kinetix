//! Reversible intra prediction for the lossless codec (DECISION 2: predictive
//! primary path).
//!
//! The predictor is FFV1's median of the left, up, and up-left neighbours. It is
//! fully integer and reversible: the decoder reconstructs the same prediction
//! from already-decoded neighbours, so `sample - prediction` round-trips exactly.
//!
//! The function takes the sample buffer by shared reference and returns the
//! predicted value, so callers may mutate the buffer immediately afterwards
//! (needed by the decode loop, which writes reconstructed samples in place).

/// Predict the sample at `(x, y)` from its reconstructed neighbours.
///
/// Outside the image border missing neighbours are treated as 0, exactly as the
/// encoder does, so encode and decode agree.
#[inline]
pub fn predict(data: &[u16], width: usize, _height: usize, x: usize, y: usize) -> u16 {
    let at = |dx: usize, dy: usize| data[dy * width + dx];
    if x == 0 && y == 0 {
        0
    } else if x == 0 {
        at(0, y - 1)
    } else if y == 0 {
        at(x - 1, 0)
    } else {
        let left = i32::from(at(x - 1, y));
        let up = i32::from(at(x, y - 1));
        let up_left = i32::from(at(x - 1, y - 1));
        let p = left + up - up_left;
        let med = if p < left {
            left
        } else if p > up {
            up
        } else {
            p
        };
        med as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_predictor_border_cases() {
        let data = vec![
            10u16, 20, 30, 40, 5, 15, 25, 35, 2, 12, 22, 32, 8, 18, 28, 38,
        ];
        assert_eq!(predict(&data, 4, 4, 0, 0), 0); // corner
        assert_eq!(predict(&data, 4, 4, 1, 0), 10); // top edge -> left
        assert_eq!(predict(&data, 4, 4, 0, 1), 10); // left edge -> up (0,0)
        let _ = predict(&data, 4, 4, 2, 2); // interior, ensure no panic
    }
}
