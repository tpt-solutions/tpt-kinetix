//! Glyph dictionary + color palette for cross-frame reuse.

use tpt_kinetix_core::error::KinetixError;

/// A single glyph entry: a 1-bit mask bitmap (bit-packed, row-major).
#[derive(Debug, Clone)]
pub struct GlyphEntry {
    pub width: u8,
    pub height: u8,
    /// Bit-packed mask: 1 = foreground, 0 = background. Row-major, 8 pixels per byte.
    pub mask: Vec<u8>,
}

impl GlyphEntry {
    pub fn new(width: u8, height: u8, mask: Vec<u8>) -> Self {
        Self { width, height, mask }
    }

    /// Get the bit at pixel (x, y).
    pub fn pixel(&self, x: usize, y: usize) -> bool {
        let idx = y * self.width as usize + x;
        let byte = idx / 8;
        let bit = idx % 8;
        if byte >= self.mask.len() {
            return false;
        }
        (self.mask[byte] >> (7 - bit)) & 1 == 1
    }
}

/// Cross-frame glyph dictionary (DECISION 3).
#[derive(Debug, Clone)]
pub struct GlyphDictionary {
    cap: usize,
    pub slots: Vec<Option<GlyphEntry>>,
}

impl GlyphDictionary {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            slots: vec![None; cap],
        }
    }

    pub fn insert(&mut self, slot: usize, entry: GlyphEntry) -> Result<(), KinetixError> {
        if slot >= self.cap {
            return Err(KinetixError::Parse(format!(
                "glyph slot {slot} exceeds dict cap {}",
                self.cap
            )));
        }
        self.slots[slot] = Some(entry);
        Ok(())
    }

    pub fn get(&self, slot: usize) -> Option<&GlyphEntry> {
        self.slots.get(slot).and_then(|s| s.as_ref())
    }

    pub fn reset(&mut self) {
        self.slots = vec![None; self.cap];
    }
}

/// Color palette entry (YUV).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteColor {
    pub y: u8,
    pub u: u8,
    pub v: u8,
}

impl PaletteColor {
    pub fn new(y: u8, u: u8, v: u8) -> Self {
        Self { y, u, v }
    }
}

/// Cross-frame color palette (DECISION 3).
#[derive(Debug, Clone)]
pub struct ColorPalette {
    cap: usize,
    pub entries: Vec<Option<PaletteColor>>,
}

impl ColorPalette {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            entries: vec![None; cap],
        }
    }

    pub fn insert(&mut self, index: usize, color: PaletteColor) -> Result<(), KinetixError> {
        if index >= self.cap {
            return Err(KinetixError::Parse(format!(
                "palette index {index} exceeds cap {}",
                self.cap
            )));
        }
        self.entries[index] = Some(color);
        Ok(())
    }

    pub fn get(&self, index: usize) -> Option<PaletteColor> {
        self.entries.get(index).copied().flatten()
    }

    pub fn reset(&mut self) {
        self.entries = vec![None; self.cap];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_dictionary_insert_and_get() {
        let mut dict = GlyphDictionary::new(4);
        assert!(dict.get(0).is_none());
        let entry = GlyphEntry::new(8, 8, vec![0xFF; 8]);
        dict.insert(0, entry.clone()).unwrap();
        assert!(dict.get(0).is_some());
        assert_eq!(dict.get(0).unwrap().width, 8);
    }

    #[test]
    fn glyph_dictionary_rejects_out_of_range() {
        let mut dict = GlyphDictionary::new(2);
        let entry = GlyphEntry::new(8, 8, vec![0xFF; 8]);
        assert!(dict.insert(5, entry).is_err());
    }

    #[test]
    fn glyph_pixel_readback() {
        let entry = GlyphEntry::new(8, 1, vec![0b1010_1010]);
        assert!(entry.pixel(0, 0));
        assert!(!entry.pixel(1, 0));
        assert!(entry.pixel(2, 0));
    }

    #[test]
    fn palette_insert_and_get() {
        let mut pal = ColorPalette::new(4);
        assert!(pal.get(0).is_none());
        pal.insert(1, PaletteColor::new(100, 128, 128)).unwrap();
        assert_eq!(pal.get(1), Some(PaletteColor::new(100, 128, 128)));
    }

    #[test]
    fn dictionary_reset_clears_entries() {
        let mut dict = GlyphDictionary::new(4);
        dict.insert(0, GlyphEntry::new(8, 8, vec![0xFF; 8])).unwrap();
        dict.reset();
        assert!(dict.get(0).is_none());
    }
}
