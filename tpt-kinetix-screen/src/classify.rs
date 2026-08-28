//! Mode classifier: assigns each coding block a mode (FLAT / GLYPH / NATURAL).

/// Classification mode for a coding block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockMode {
    /// Solid color or simple gradient.
    Flat = 0,
    /// Glyph/palette dictionary reference.
    Glyph = 1,
    /// Natural-image transform fallback.
    Natural = 2,
}

/// Classify a luma block (given as row-major `size*size` samples).
///
/// Heuristic for v1:
/// - FLAT: all samples within `tolerance` of the block mean.
/// - GLYPH: high-contrast edges (max-min above threshold), not flat.
/// - NATURAL: everything else (complex texture).
pub fn classify_block_luma(block: &[u8], size: usize, tolerance: u8) -> BlockMode {
    debug_assert_eq!(block.len(), size * size);

    let min = *block.iter().min().unwrap_or(&0);
    let max = *block.iter().max().unwrap_or(&255);
    let range = max.abs_diff(min);

    if range <= tolerance {
        return BlockMode::Flat;
    }

    // Count samples far from the mean to distinguish structured edges (GLYPH)
    // from complex texture (NATURAL).
    let mean = block.iter().map(|&p| p as u32).sum::<u32>() / block.len() as u32;
    let outliers = block
        .iter()
        .filter(|&&p| p.abs_diff(mean as u8) > tolerance * 2)
        .count();

    // GLYPH: structured — most pixels are near one of two values (fg/bg).
    // NATURAL: distributed — many pixels far from the mean.
    let outlier_frac = outliers as f64 / block.len() as f64;
    if outlier_frac < 0.5 {
        BlockMode::Glyph
    } else {
        BlockMode::Natural
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_block_is_flat() {
        let block = vec![128u8; 16]; // 4x4
        assert_eq!(classify_block_luma(&block, 4, 4), BlockMode::Flat);
    }

    #[test]
    fn two_tone_block_is_glyph() {
        // Half 0, half 255 — structured like a glyph.
        let mut block = Vec::with_capacity(16);
        for _ in 0..8 {
            block.push(0);
        }
        for _ in 0..8 {
            block.push(255);
        }
        assert_eq!(classify_block_luma(&block, 4, 4), BlockMode::Glyph);
    }

    #[test]
    fn noisy_block_is_natural() {
        // Random-looking distribution.
        let block: Vec<u8> = (0..16).map(|i| (i * 37 + 11) as u8).collect();
        assert_eq!(classify_block_luma(&block, 4, 4), BlockMode::Natural);
    }
}
