//! Glyph / palette mode.

use crate::dictionary::{GlyphDictionary, PaletteColor};

/// A glyph-mode block payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphBlock {
    pub dict_slot: u8,
    pub fg_idx: u8,
    pub bg_idx: u8,
}

/// Match a block against the glyph dictionary.
///
/// Returns the best-matching slot if the block is within `tolerance` of a
/// dictionary entry (using the entry's mask with the given fg/bg colors),
/// or `None` on a miss.
pub fn match_glyph(
    block: &[u8],
    size: usize,
    dict: &GlyphDictionary,
    fg: PaletteColor,
    bg: PaletteColor,
    tolerance: u8,
) -> Option<usize> {
    for (slot_idx, entry) in dict.slots.iter().enumerate() {
        let entry = entry.as_ref()?;
        if entry.width as usize != size || entry.height as usize != size {
            continue;
        }
        let mut total_diff: u32 = 0;
        let mut ok = true;
        for y in 0..size {
            for x in 0..size {
                let expected = if entry.pixel(x, y) { fg.y } else { bg.y };
                let actual = block[y * size + x];
                let diff = expected.abs_diff(actual) as u32;
                total_diff += diff;
                if diff > tolerance as u32 * 2 {
                    ok = false;
                    break;
                }
            }
            if !ok {
                break;
            }
        }
        if ok {
            let mean_diff = total_diff / (size * size) as u32;
            if mean_diff <= tolerance as u32 {
                return Some(slot_idx);
            }
        }
    }
    None
}

/// Render a glyph block into luma samples using the dictionary entry + colors.
pub fn render_glyph(
    slot: u8,
    dict: &GlyphDictionary,
    fg: PaletteColor,
    bg: PaletteColor,
    size: usize,
) -> Vec<u8> {
    let mut out = vec![0u8; size * size];
    if let Some(entry) = dict.get(slot as usize) {
        for y in 0..size {
            for x in 0..size {
                out[y * size + x] = if entry.pixel(x, y) { fg.y } else { bg.y };
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictionary::GlyphEntry;

    #[test]
    fn match_exact_glyph() {
        let mut dict = GlyphDictionary::new(4);
        // 4x4 glyph: left half fg, right half bg
        let mut mask = vec![0u8; 2]; // 4x4 = 16 bits = 2 bytes
        mask[0] = 0b1111_0000;
        mask[1] = 0b1111_0000;
        dict.insert(0, GlyphEntry::new(4, 4, mask)).unwrap();

        let fg = PaletteColor::new(255, 0, 0);
        let bg = PaletteColor::new(0, 0, 0);
        // Block matching: left=255, right=0
        let mut block = vec![0u8; 16];
        for y in 0..4 {
            for x in 0..2 {
                block[y * 4 + x] = 255;
            }
        }
        let slot = match_glyph(&block, 4, &dict, fg, bg, 2);
        assert_eq!(slot, Some(0));
    }

    #[test]
    fn render_glyph_produces_mask() {
        let mut dict = GlyphDictionary::new(4);
        let mut mask = vec![0u8; 2];
        mask[0] = 0b1010_1010;
        mask[1] = 0b1010_1010;
        dict.insert(0, GlyphEntry::new(4, 4, mask)).unwrap();

        let fg = PaletteColor::new(200, 0, 0);
        let bg = PaletteColor::new(50, 0, 0);
        let block = render_glyph(0, &dict, fg, bg, 4);
        // Row 0: 1,0,1,0 → 200,50,200,50
        assert_eq!(block[0], 200);
        assert_eq!(block[1], 50);
        assert_eq!(block[2], 200);
        assert_eq!(block[3], 50);
    }
}
