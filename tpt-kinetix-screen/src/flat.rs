//! Flat-fill / run-length mode.

use crate::dictionary::PaletteColor;

/// A run of consecutive FLAT blocks with the same luma color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlatRun {
    pub color_y: u8,
    pub run_len: u8,
}

/// Encode flat blocks into run-length pairs.
///
/// `modes` is the block-mode slice; `colors` is the per-block luma color
/// (only meaningful where the mode is Flat). Consecutive FLAT blocks
/// sharing the same color are coalesced into one run.
pub fn encode_flat_runs(modes: &[u8], colors: &[u8]) -> Vec<FlatRun> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < modes.len() {
        if modes[i] == 0 {
            let color = colors[i];
            let mut end = i + 1;
            while end < modes.len() && modes[end] == 0 && colors[end] == color && end - i < 255 {
                end += 1;
            }
            runs.push(FlatRun {
                color_y: color,
                run_len: (end - i) as u8,
            });
            i = end;
        } else {
            i += 1;
        }
    }
    runs
}

/// Expand flat runs back into per-block color indices, using the mode map to
/// place colors only at FLAT block positions.
pub fn decode_flat_runs(runs: &[FlatRun], modes: &[u8]) -> Vec<u8> {
    let total = modes.len();
    let mut colors = vec![0u8; total];
    let mut run_idx = 0;
    let mut run_remaining = 0u8;
    let mut cur_color = 0u8;

    for (bi, &mode) in modes.iter().enumerate() {
        if mode == 0 {
            if run_remaining == 0 {
                if run_idx < runs.len() {
                    cur_color = runs[run_idx].color_y;
                    run_remaining = runs[run_idx].run_len;
                    run_idx += 1;
                }
            }
            if run_remaining > 0 {
                colors[bi] = cur_color;
                run_remaining -= 1;
            }
        }
    }
    colors
}

/// Look up the palette color for a flat block.
pub fn flat_color(idx: u8, palette: &[Option<PaletteColor>]) -> PaletteColor {
    palette.get(idx as usize)
        .copied()
        .flatten()
        .unwrap_or(PaletteColor::new(0, 128, 128))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_consecutive_same_color_runs() {
        let modes = vec![0, 0, 0, 1, 0, 0];
        let colors = vec![100, 100, 100, 0, 50, 50];
        let runs = encode_flat_runs(&modes, &colors);
        // 3 flats of color 100, then non-flat skipped, then 2 flats of color 50
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0], FlatRun { color_y: 100, run_len: 3 });
        assert_eq!(runs[1], FlatRun { color_y: 50, run_len: 2 });
    }

    #[test]
    fn decode_runs_expands_correctly() {
        let runs = vec![
            FlatRun { color_y: 100, run_len: 3 },
            FlatRun { color_y: 50, run_len: 1 },
        ];
        // Modes: 3 flats, then 1 non-flat, then 1 flat → colors [100,100,100,0,50]
        let modes = vec![0, 0, 0, 1, 0];
        let colors = decode_flat_runs(&runs, &modes);
        assert_eq!(colors, vec![100, 100, 100, 0, 50]);
    }
}
