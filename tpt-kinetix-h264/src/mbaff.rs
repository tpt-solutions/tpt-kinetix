//! MBAFF (macroblock-adaptive frame/field) interlaced decoding helpers
//! (`todo.md` Phase G.4 / G.5).
//!
//! Two pieces are implemented here, both derived from the H.264 specification
//! clause 6.4.10.1 and cross-checked against FFmpeg's reference
//! `fill_decode_neighbors` / `hl_decode_mb` logic (the canonical §6.4.10.1
//! implementation):
//!
//! 1. **Neighbour derivation** — [`derive_neighbours`] computes the frame
//!    macroblock addresses of the top-left / top / top-right / left macroblock
//!    neighbours of a macroblock inside an MBAFF frame, correctly handling the
//!    *mixed* field/frame case where the current macroblock pair and one of its
//!    neighbours are coded with different `mb_field_decoding_flag` values.
//!
//! 2. **Field/frame-adaptive pair placement** — [`place_mbaff_luma_pair`] /
//!    [`place_mbaff_chroma_pair`] write the two reconstructed 16×16 (luma) /
//!    8×8 (chroma) macroblock planes of a macroblock pair into a full
//!    interlaced frame, using the pair's `mb_field_decoding_flag` to select
//!    between *frame* placement (each macroblock occupies a contiguous half of
//!    the 32-line pair region) and *field* placement (the two macroblocks are
//!    interleaved by scanline parity, top macroblock → even lines, bottom
//!    macroblock → odd lines).
//!
//! The decoder's interlaced paths are, per [`crate::H264Decoder::capabilities`],
//! explicitly **not pixel-exact** yet (the full field-aware intra prediction
//! and motion scaling remain future work). These functions provide the
//! spec-correct structural machinery so that (a) neighbours derive correctly
//! for mixed field/frame pairs and (b) the reconstructed macroblock-pair
//! output lands in the right scanlines of the interlaced frame.

/// Neighbouring macroblock addresses (in the *frame* macroblock grid) for the
/// current macroblock, as derived by §6.4.10.1. `None` means the neighbour is
/// off-picture (unavailable).
///
/// The grid uses the same frame-MB addressing the parser produces: a picture of
/// `mb_cols × mb_rows` macroblocks (where `mb_rows` is `height/16` for an
/// MBAFF frame), and a macroblock pair is the pair of rows `(2k, 2k+1)` in that
/// grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MbaffNeighbours {
    /// Macroblock above-left (uses the same scanline as `top` minus one column).
    pub topleft: Option<usize>,
    /// Macroblock directly above.
    pub top: Option<usize>,
    /// Macroblock above-right.
    pub topright: Option<usize>,
    /// Macroblock directly to the left (top 16×16 half for a frame pair).
    pub left_top: Option<usize>,
    /// Macroblock directly to the left (bottom 16×16 half for a frame pair).
    pub left_bottom: Option<usize>,
    /// FFmpeg `topleft_partition` sentinel: which 4×4 partition of the top-left
    /// neighbour supplies the diagonal cache context. `-1` means "use the
    /// bottom-right partition" (the default); `0` means "use the top-left
    /// partition" (the special case for a frame-current / field-left pair).
    pub topleft_partition: i32,
}

/// Look up the `mb_field_decoding_flag` of frame-MB `idx`, returning `None`
/// when the index is outside `[0, mb_cols*mb_rows)` (off-picture).
#[inline]
fn flag_at(field_flags: &[Option<bool>], idx: isize, total: usize) -> Option<bool> {
    if idx < 0 || (idx as usize) >= total {
        None
    } else {
        field_flags[idx as usize]
    }
}

/// Derive the neighbouring macroblock addresses for `mb` at frame-MB position
/// `(mb_x, mb_y)` inside an MBAFF frame (`mb_aff == true`), per §6.4.10.1.
///
/// `cur_field` is the `mb_field_decoding_flag` of the *current* macroblock (and
/// its pair, since the flag is signalled once per pair). `field_flags` is the
/// per-frame-MB `mb_field_decoding_flag` of already-decoded neighbours,
/// indexed by frame-MB address; `None` for an off-picture position.
///
/// This is a faithful port of FFmpeg's `fill_decode_neighbors` neighbour-address
/// computation (the `FRAME_MBAFF(h)` block), which is itself the normative
/// §6.4.10.1 derivation.
pub fn derive_neighbours(
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    mb_rows: u32,
    cur_field: bool,
    field_flags: &[Option<bool>],
) -> MbaffNeighbours {
    let mb_cols = mb_cols as isize;
    let total = (mb_cols as usize) * (mb_rows as usize);
    let mb_xy = (mb_y as isize) * mb_cols + (mb_x as isize);

    // top_xy = mb_xy - (mb_stride << MB_FIELD(sl)): a field-coded current MB's
    // "top" neighbour is two frame-MB rows up; a frame-coded current MB's is one
    // row up.
    let top_step = if cur_field { 2 * mb_cols } else { mb_cols };
    let mut top_xy = mb_xy - top_step;

    let mut topleft_xy = top_xy - 1;
    let mut topright_xy = top_xy + 1;
    let mut left_top = mb_xy - 1;
    let mut left_bottom = mb_xy - 1;
    let mut topleft_partition = -1i32;

    // FRAME_MBAFF adjustments (the mixed field/frame case).
    let left_mb_field = flag_at(field_flags, left_top, total).unwrap_or(false);
    if mb_y & 1 == 1 {
        // Bottom macroblock of the pair.
        if left_mb_field != cur_field {
            left_top = mb_xy - mb_cols - 1;
            left_bottom = left_top;
            if cur_field {
                left_bottom += mb_cols;
                // left_block = left_block_options[3]
            } else {
                topleft_xy += mb_cols;
                topleft_partition = 0;
                // left_block = left_block_options[1]
            }
        }
    } else {
        // Top macroblock of the pair.
        if cur_field {
            // When the current pair is field-coded, the top / top-left /
            // top-right neighbours each shift down by one frame-MB row *unless*
            // that neighbour is itself frame-coded (i.e. its field flag is 0).
            // Only apply the shift if the initial neighbour address is valid
            // (within the picture). Off-picture neighbours remain unavailable.
            if topleft_xy >= 0 {
                topleft_xy += add_if_frame(flag_at(field_flags, topleft_xy, total));
            }
            if topright_xy >= 0 {
                topright_xy += add_if_frame(flag_at(field_flags, topright_xy, total));
            }
            if top_xy >= 0 {
                top_xy += add_if_frame(flag_at(field_flags, top_xy, total));
            }
        }
        if left_mb_field != cur_field {
            if cur_field {
                left_bottom += mb_cols;
                // left_block = left_block_options[3]
            } else {
                // left_block = left_block_options[2]
            }
        }
    }

    let clamp = |idx: isize| -> Option<usize> {
        if idx < 0 || (idx as usize) >= total {
            None
        } else {
            Some(idx as usize)
        }
    };
    // A left neighbour only exists when we are not in the first column.
    let left = if mb_x > 0 {
        clamp(left_top).map(|t| (t, clamp(left_bottom)))
    } else {
        None
    };

    MbaffNeighbours {
        topleft: clamp(topleft_xy),
        top: clamp(top_xy),
        topright: clamp(topright_xy),
        left_top: left.map(|(t, _)| t),
        left_bottom: left.and_then(|(_, b)| b),
        topleft_partition,
    }
}

/// `mb_stride & (flag - 1)`: add `mb_cols` when the neighbour is frame-coded
/// (`flag == false`), add nothing when it is field-coded (`flag == true`).
#[inline]
fn add_if_frame(flag: Option<bool>) -> isize {
    match flag {
        Some(true) => 0,
        _ => 1, // treat unavailable like frame-coded for the shift amount
    }
}

/// Write the two luma macroblock planes of a macroblock pair into a full-frame
/// interlaced luma plane.
///
/// `top`/`bottom` are the 16×16 (256-sample) reconstructed luma planes of the
/// pair's top and bottom macroblocks. `pair_row` is the 0-based macroblock-pair
/// row; the pair covers frame scanlines `[32*pair_row, 32*pair_row+32)`.
///
/// - **Frame placement** (`field == false`): `top` occupies scanlines
///   `[0,16)`, `bottom` occupies `[16,32)` of the pair region (each macroblock
///   is a contiguous half).
/// - **Field placement** (`field == true`): `top` occupies the *even* scanlines
///   `2j` and `bottom` the *odd* scanlines `2j+1` of the pair region (the two
///   macroblocks are interleaved by parity).
pub fn place_mbaff_luma_pair(
    out: &mut [u8],
    stride: usize,
    pair_row: usize,
    mb_x: usize,
    field: bool,
    top: &[u8; 256],
    bottom: &[u8; 256],
) {
    let base_x = mb_x * 16;
    let base_y = pair_row * 32;
    for j in 0..16usize {
        let row_top = if field { base_y + 2 * j } else { base_y + j };
        let row_bottom = if field {
            base_y + 2 * j + 1
        } else {
            base_y + 16 + j
        };
        if let Some(sl) = out.get_mut(row_top * stride + base_x..row_top * stride + base_x + 16) {
            sl.copy_from_slice(&top[j * 16..j * 16 + 16]);
        }
        if let Some(sl) =
            out.get_mut(row_bottom * stride + base_x..row_bottom * stride + base_x + 16)
        {
            sl.copy_from_slice(&bottom[j * 16..j * 16 + 16]);
        }
    }
}

/// Write the two chroma (Cb or Cr) macroblock planes of a macroblock pair into a
/// full-frame interlaced chroma plane.
///
/// `top`/`bottom` are the 8×8 (64-sample) reconstructed chroma planes. The pair
/// covers chroma scanlines `[16*pair_row, 16*pair_row+16)` (half the luma
/// region height, 4:2:0). Placement follows the same frame/field rule as
/// [`place_mbaff_luma_pair`], with chroma parity matching luma parity.
pub fn place_mbaff_chroma_pair(
    out: &mut [u8],
    stride: usize,
    pair_row: usize,
    mb_x: usize,
    field: bool,
    top: &[u8; 64],
    bottom: &[u8; 64],
) {
    let base_x = mb_x * 8;
    let base_y = pair_row * 16;
    for j in 0..8usize {
        let row_top = if field { base_y + 2 * j } else { base_y + j };
        let row_bottom = if field {
            base_y + 2 * j + 1
        } else {
            base_y + 8 + j
        };
        if let Some(sl) = out.get_mut(row_top * stride + base_x..row_top * stride + base_x + 8) {
            sl.copy_from_slice(&top[j * 8..j * 8 + 8]);
        }
        if let Some(sl) =
            out.get_mut(row_bottom * stride + base_x..row_bottom * stride + base_x + 8)
        {
            sl.copy_from_slice(&bottom[j * 8..j * 8 + 8]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert that, for a fully *frame-coded* MBAFF frame (every pair's
    /// `mb_field_decoding_flag == false`), neighbour derivation degenerates to
    /// the ordinary progressive frame neighbour derivation: left = `mb_xy-1`,
    /// top = `mb_xy - mb_cols`, top-left = `mb_xy - mb_cols - 1`,
    /// top-right = `mb_xy - mb_cols + 1`.
    #[test]
    fn all_frame_degenerates_to_progressive() {
        let mb_cols = 4u32;
        let mb_rows = 4u32;
        let total = (mb_cols * mb_rows) as usize;
        let field_flags = vec![Some(false); total];

        // A non-edge, frame-coded current macroblock.
        for mb_y in 1..mb_rows {
            for mb_x in 1..(mb_cols - 1) {
                let n = derive_neighbours(mb_x, mb_y, mb_cols, mb_rows, false, &field_flags);
                let mb_xy = (mb_y * mb_cols + mb_x) as usize;
                assert_eq!(n.left_top, Some(mb_xy - 1), "left @ ({mb_x},{mb_y})");
                assert_eq!(
                    n.top,
                    Some(mb_xy - mb_cols as usize),
                    "top @ ({mb_x},{mb_y})"
                );
                assert_eq!(
                    n.topleft,
                    Some(mb_xy - mb_cols as usize - 1),
                    "topleft @ ({mb_x},{mb_y})"
                );
                assert_eq!(
                    n.topright,
                    Some(mb_xy - mb_cols as usize + 1),
                    "topright @ ({mb_x},{mb_y})"
                );
                assert_eq!(n.topleft_partition, -1);
            }
        }
    }

    /// A top-row (pair row 0 top macroblock) that is field-coded has no top
    /// neighbours (they are off-picture). The "shift down" logic only applies
    /// when the neighbour is within the picture but frame-coded.
    #[test]
    fn field_top_row_shifts_neighbours_down() {
        let mb_cols = 4u32;
        let mb_rows = 4u32;
        let total = (mb_cols * mb_rows) as usize;
        let mut field_flags = vec![Some(false); total];
        // Make the entire first pair row field-coded (both MBs of pair 0).
        for x in 0..mb_cols {
            field_flags[x as usize] = Some(true); // top MB of pair 0
            field_flags[(mb_cols + x) as usize] = Some(true); // bottom MB of pair 0
        }

        // Current = top MB of pair 0 (mb_y == 0), field-coded.
        let mb_x = 1u32;
        let mb_y = 0u32;
        let n = derive_neighbours(mb_x, mb_y, mb_cols, mb_rows, true, &field_flags);
        // Top neighbours are off-picture (mb_y == 0), so they should be None.
        assert_eq!(n.top, None, "field top at mb_y=0 has no top neighbour");
        assert_eq!(
            n.topleft, None,
            "field top at mb_y=0 has no topleft neighbour"
        );
        assert_eq!(
            n.topright, None,
            "field top at mb_y=0 has no topright neighbour"
        );
    }

    /// Mixed case: a frame-coded current macroblock whose left neighbour is
    /// field-coded (bottom MB of a pair, `mb_y & 1 == 1`). The left neighbour
    /// address must jump to the previous pair row (`mb_xy - mb_cols - 1`) because
    /// the field-coded left pair spans two frame-MB rows.
    #[test]
    fn frame_bottom_mixed_left_field() {
        let mb_cols = 4u32;
        let mb_rows = 4u32;
        let total = (mb_cols * mb_rows) as usize;
        let mut field_flags = vec![Some(false); total];
        // Left column (mb_x == 0) pairs are field-coded.
        for y in 0..mb_rows {
            field_flags[(y * mb_cols) as usize] = Some(true); // top MB of left pair
            field_flags[(y * mb_cols + 1) as usize] = Some(true); // bottom MB of left pair
        }

        // Current = bottom MB (mb_y == 3, odd) of a frame-coded pair at mb_x==2.
        let mb_x = 2u32;
        let mb_y = 3u32;
        let n = derive_neighbours(mb_x, mb_y, mb_cols, mb_rows, false, &field_flags);
        let mb_xy = (mb_y * mb_cols + mb_x) as usize;
        // Left pair is field → left address becomes mb_xy - mb_cols - 1.
        assert_eq!(
            n.left_top,
            Some(mb_xy - mb_cols as usize - 1),
            "frame bottom MB next to a field-coded left pair jumps left addr up a row"
        );
        assert_eq!(
            n.left_bottom,
            Some(mb_xy - mb_cols as usize - 1),
            "frame bottom MB next to a field-coded left pair jumps left addr up a row"
        );
    }

    /// Edge macroblocks must never produce out-of-bounds neighbour indices.
    #[test]
    fn edge_macroblocks_stay_in_bounds() {
        let mb_cols = 3u32;
        let mb_rows = 3u32;
        let total = (mb_cols * mb_rows) as usize;
        let field_flags = vec![Some(true); total]; // all field-coded
        for mb_y in 0..mb_rows {
            for mb_x in 0..mb_cols {
                let n = derive_neighbours(mb_x, mb_y, mb_cols, mb_rows, true, &field_flags);
                for i in [n.topleft, n.top, n.topright, n.left_top, n.left_bottom]
                    .into_iter()
                    .flatten()
                {
                    assert!(i < total, "neighbour {i} out of bounds @ ({mb_x},{mb_y})");
                }
            }
        }
    }

    /// Frame placement: a pair's two 16×16 luma planes land in two contiguous
    /// 16-row halves of the 32-line pair region, top → first half.
    #[test]
    fn frame_placement_is_contiguous_halves() {
        let stride = 16;
        let mut out = vec![0u8; stride * 32];
        let top = [10u8; 256];
        let bottom = [20u8; 256];
        place_mbaff_luma_pair(&mut out, stride, 0, 0, false, &top, &bottom);
        for y in 0..16 {
            assert!(
                out[y * stride..y * stride + 16].iter().all(|&v| v == 10),
                "top half row {y}"
            );
        }
        for y in 16..32 {
            assert!(
                out[y * stride..y * stride + 16].iter().all(|&v| v == 20),
                "bottom half row {y}"
            );
        }
    }

    /// Field placement: a pair's two 16×16 luma planes are interleaved by parity
    /// — top macroblock → even scanlines, bottom → odd scanlines.
    #[test]
    fn field_placement_interleaves_parity() {
        let stride = 16;
        let mut out = vec![0u8; stride * 32];
        let top = [10u8; 256];
        let bottom = [20u8; 256];
        place_mbaff_luma_pair(&mut out, stride, 0, 0, true, &top, &bottom);
        for y in 0..32 {
            let expected = if y % 2 == 0 { 10 } else { 20 };
            assert!(
                out[y * stride..y * stride + 16]
                    .iter()
                    .all(|&v| v == expected),
                "field pair scanline {y} should be {expected}"
            );
        }
    }

    /// Field chroma placement interleaves parity over the 16-line chroma region.
    #[test]
    fn field_chroma_placement_interleaves_parity() {
        let stride = 8;
        let mut out = vec![0u8; stride * 16];
        let top = [5u8; 64];
        let bottom = [7u8; 64];
        place_mbaff_chroma_pair(&mut out, stride, 0, 0, true, &top, &bottom);
        for y in 0..16 {
            let expected = if y % 2 == 0 { 5 } else { 7 };
            assert!(
                out[y * stride..y * stride + 8]
                    .iter()
                    .all(|&v| v == expected),
                "field chroma scanline {y} should be {expected}"
            );
        }
    }

    /// Frame chroma placement is two contiguous 8-row halves.
    #[test]
    fn frame_chroma_placement_is_contiguous_halves() {
        let stride = 8;
        let mut out = vec![0u8; stride * 16];
        let top = [5u8; 64];
        let bottom = [7u8; 64];
        place_mbaff_chroma_pair(&mut out, stride, 0, 0, false, &top, &bottom);
        for y in 0..8 {
            assert!(
                out[y * stride..y * stride + 8].iter().all(|&v| v == 5),
                "chroma top half row {y}"
            );
        }
        for y in 8..16 {
            assert!(
                out[y * stride..y * stride + 8].iter().all(|&v| v == 7),
                "chroma bottom half row {y}"
            );
        }
    }
}
