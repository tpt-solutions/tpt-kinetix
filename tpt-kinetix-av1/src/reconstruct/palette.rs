use super::*;

/// `get_palette_color_context()` (AV1 spec §5.11.50): scores each of the
/// `n` palette indices by how often it appears among the left/above-left/above
/// neighbours of `color_map[r][c]`, partially selection-sorts the top
/// [`PALETTE_NUM_NEIGHBORS`] by score into `ColorOrder`, and hashes those
/// scores into a context index via [`PALETTE_COLOR_CONTEXT`]. Returns
/// `(ctx, ColorOrder)` — the caller remaps the decoded symbol through
/// `ColorOrder` to get the actual palette index (`ColorOrder[symbol]`).
#[allow(clippy::needless_range_loop)]
pub(super) fn get_palette_color_context(
    color_map: &[u8],
    stride: usize,
    r: usize,
    c: usize,
    n: usize,
) -> (usize, [u8; PALETTE_COLORS]) {
    let mut scores = [0i32; PALETTE_COLORS];
    let mut color_order: [u8; PALETTE_COLORS] = std::array::from_fn(|i| i as u8);
    if c > 0 {
        scores[color_map[r * stride + c - 1] as usize] += 2;
    }
    if r > 0 && c > 0 {
        scores[color_map[(r - 1) * stride + c - 1] as usize] += 1;
    }
    if r > 0 {
        scores[color_map[(r - 1) * stride + c] as usize] += 2;
    }
    for i in 0..PALETTE_NUM_NEIGHBORS {
        let mut max_score = scores[i];
        let mut max_idx = i;
        for j in (i + 1)..n {
            if scores[j] > max_score {
                max_score = scores[j];
                max_idx = j;
            }
        }
        if max_idx != i {
            let max_score = scores[max_idx];
            let max_color_order = color_order[max_idx];
            let mut k = max_idx;
            while k > i {
                scores[k] = scores[k - 1];
                color_order[k] = color_order[k - 1];
                k -= 1;
            }
            scores[i] = max_score;
            color_order[i] = max_color_order;
        }
    }
    let mut hash = 0i32;
    for i in 0..PALETTE_NUM_NEIGHBORS {
        hash += scores[i] * PALETTE_COLOR_HASH_MULTIPLIERS[i];
    }
    let ctx = PALETTE_COLOR_CONTEXT[hash as usize];
    debug_assert!(ctx >= 0, "get_palette_color_context produced an N/A hash");
    (ctx.max(0) as usize, color_order)
}

// ──────────────────────────────────────────────────────────────────────────────
// Dequantization
// ──────────────────────────────────────────────────────────────────────────────

/// AV1 8-bit DC quantizer lookup table (spec §7.12.2.1, transcribed verbatim
/// from `libgav1`'s `kDcLookup[8-bit]` reference table — identical to libaom's
/// `dc_qlookup_QTX`). AV1 keeps **separate** DC and AC quantizer tables; the
/// previous analytic approximation conflated them and diverged sharply from
/// these values at every non-trivial qindex.
pub(super) const DC_QLOOKUP_8: [i32; 256] = [
    4, 8, 8, 9, 10, 11, 12, 12, 13, 14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 23, 24, 25, 26, 26, 27,
    28, 29, 30, 31, 32, 32, 33, 34, 35, 36, 37, 38, 38, 39, 40, 41, 42, 43, 43, 44, 45, 46, 47, 48,
    48, 49, 50, 51, 52, 53, 53, 54, 55, 56, 57, 57, 58, 59, 60, 61, 62, 62, 63, 64, 65, 66, 66, 67,
    68, 69, 70, 70, 71, 72, 73, 74, 74, 75, 76, 77, 78, 78, 79, 80, 81, 81, 82, 83, 84, 85, 85, 87,
    88, 90, 92, 93, 95, 96, 98, 99, 101, 102, 104, 105, 107, 108, 110, 111, 113, 114, 116, 117,
    118, 120, 121, 123, 125, 127, 129, 131, 134, 136, 138, 140, 142, 144, 146, 148, 150, 152, 154,
    156, 158, 161, 164, 166, 169, 172, 174, 177, 180, 182, 185, 187, 190, 192, 195, 199, 202, 205,
    208, 211, 214, 217, 220, 223, 226, 230, 233, 237, 240, 243, 247, 250, 253, 257, 261, 265, 269,
    272, 276, 280, 284, 288, 292, 296, 300, 304, 309, 313, 317, 322, 326, 330, 335, 340, 344, 349,
    354, 359, 364, 369, 374, 379, 384, 389, 395, 400, 406, 411, 417, 423, 429, 435, 441, 447, 454,
    461, 467, 475, 482, 489, 497, 505, 513, 522, 530, 539, 549, 559, 569, 579, 590, 602, 614, 626,
    640, 654, 668, 684, 700, 717, 736, 755, 775, 796, 819, 843, 869, 896, 925, 955, 988, 1022,
    1058, 1098, 1139, 1184, 1232, 1282, 1336,
];

/// AV1 8-bit AC quantizer lookup table (spec §7.12.2.2, transcribed verbatim
/// from `libgav1`'s `kAcLookup[8-bit]` reference table).
pub(super) const AC_QLOOKUP_8: [i32; 256] = [
    4, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30,
    31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54,
    55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78,
    79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101,
    102, 104, 106, 108, 110, 112, 114, 116, 118, 120, 122, 124, 126, 128, 130, 132, 134, 136, 138,
    140, 142, 144, 146, 148, 150, 152, 155, 158, 161, 164, 167, 170, 173, 176, 179, 182, 185, 188,
    191, 194, 197, 200, 203, 207, 211, 215, 219, 223, 227, 231, 235, 239, 243, 247, 251, 255, 260,
    265, 270, 275, 280, 285, 290, 295, 300, 305, 311, 317, 323, 329, 335, 341, 347, 353, 359, 366,
    373, 380, 387, 394, 401, 408, 416, 424, 432, 440, 448, 456, 465, 474, 483, 492, 501, 510, 520,
    530, 540, 550, 560, 571, 582, 593, 604, 615, 627, 639, 651, 663, 676, 689, 702, 715, 729, 743,
    757, 771, 786, 801, 816, 832, 848, 864, 881, 898, 915, 933, 951, 969, 988, 1007, 1026, 1046,
    1066, 1087, 1108, 1129, 1151, 1173, 1196, 1219, 1243, 1267, 1292, 1317, 1343, 1369, 1396, 1423,
    1451, 1479, 1508, 1537, 1567, 1597, 1628, 1660, 1692, 1725, 1759, 1793, 1828,
];

/// A coded block's full palette state (AV1 spec §5.11.46/§5.11.49):
/// `palette_colors_{y,u,v}` plus the `ColorMapY`/`ColorMapUV` built once for
/// the whole block and sliced per transform block by
/// [`PaletteBlockInfo::off_x`]/`off_y`. Empty `colors_y`/`colors_u` mean
/// `PaletteSizeY`/`PaletteSizeUV == 0` (ordinary, non-palette prediction).
#[derive(Default)]
pub(super) struct PaletteData {
    pub(super) colors_y: Vec<i32>,
    pub(super) colors_u: Vec<i32>,
    pub(super) colors_v: Vec<i32>,
    pub(super) map_y: Vec<u8>,
    pub(super) stride_y: usize,
    pub(super) map_uv: Vec<u8>,
    pub(super) stride_uv: usize,
}

/// Inputs to the `predict_palette` process (AV1 spec §7.11.4) for one
/// transform block of a palette-coded coded block.
pub(super) struct PaletteBlockInfo<'a> {
    /// `palette_colors_{y,u,v}` for this plane, already `Clip1`-ranged.
    pub(super) colors: &'a [i32],
    /// The coded block's full `ColorMapY`/`ColorMapUV` (§5.11.49): one
    /// palette index per plane-sample position, row-major, `map_stride`-wide.
    pub(super) color_map: &'a [u8],
    pub(super) map_stride: usize,
    /// This transform block's offset from the coded block's origin, in
    /// samples (spec's `x * 4`, `y * 4`).
    pub(super) off_x: usize,
    pub(super) off_y: usize,
}

/// `predict_palette` (AV1 spec §7.11.4): each output sample is simply the
/// palette color named by the pre-decoded color map at that position.
///
/// Reads are defensive (`.get()` with a fallback to `colors[0]`/mid-grey)
/// rather than a direct index: the chroma color map's dimensions come from
/// `read_color_map`'s own `blockWidth`/`blockHeight` (with the spec's
/// sub-4-sample `+= 2` padding for chroma-shared small blocks), while the
/// transform-block loop that calls this reads its coverage from this crate's
/// own (independently derived, `HasChroma`-unaware — see the `HasChroma`
/// notes elsewhere in this module) chroma tile-block extent; the two are not
/// proven to always agree at that specific small-block edge case, and an
/// out-of-bounds index there should degrade gracefully, not panic.
pub(super) fn predict_palette(info: &PaletteBlockInfo, tx_w: usize, tx_h: usize, out: &mut [i32]) {
    let fallback = info.colors.first().copied().unwrap_or(MID_SAMPLE);
    for i in 0..tx_h {
        for j in 0..tx_w {
            let idx = info
                .color_map
                .get((info.off_y + i) * info.map_stride + info.off_x + j)
                .copied()
                .map(usize::from);
            out[i * tx_w + j] = idx
                .and_then(|idx| info.colors.get(idx).copied())
                .unwrap_or(fallback);
        }
    }
}

/// Inputs to the `predict_chroma_from_luma` process (AV1 spec §7.11.5) for
/// one chroma transform block.
pub(super) struct CflParams<'a> {
    /// Already-reconstructed luma plane (tile-local), read-only here.
    pub(super) luma: &'a [u8],
    pub(super) luma_stride: usize,
    pub(super) sub_x: bool,
    pub(super) sub_y: bool,
    /// `MaxLumaW`/`MaxLumaH` (tile-local pixel coords): the coded block's
    /// luma extent, which clamps the luma-sample lookup at the block's own
    /// right/bottom edge rather than the plane's.
    pub(super) max_luma_w: usize,
    pub(super) max_luma_h: usize,
    /// `CflAlphaU` or `CflAlphaV`, already sign-applied.
    pub(super) alpha: i32,
}

/// `predict_chroma_from_luma` (AV1 spec §7.11.5): overwrite the DC-predicted
/// `pred` (already filled in by the caller, since `UV_CFL_PRED` uses
/// `DC_PRED` as its base predictor per §7.11.2.1) with `dc + alpha *
/// (L[i][j] - lumaAvg)`, where `L` is the subsampled reconstructed luma
/// block and `lumaAvg` its average.
pub(super) fn apply_cfl_prediction(
    pred: &mut [i32],
    tx_w: usize,
    tx_h: usize,
    cpx_x: usize,
    cpx_y: usize,
    cfl: &CflParams,
) {
    let sub_x = usize::from(cfl.sub_x);
    let sub_y = usize::from(cfl.sub_y);
    let mut l = vec![0i32; tx_w * tx_h];
    let mut luma_avg: i64 = 0;
    for i in 0..tx_h {
        let luma_y = ((cpx_y + i) << sub_y).min(cfl.max_luma_h.saturating_sub(1 << sub_y));
        for j in 0..tx_w {
            let luma_x = ((cpx_x + j) << sub_x).min(cfl.max_luma_w.saturating_sub(1 << sub_x));
            let mut t = 0i32;
            for dy in 0..=sub_y {
                for dx in 0..=sub_x {
                    t += cfl
                        .luma
                        .get((luma_y + dy) * cfl.luma_stride + (luma_x + dx))
                        .copied()
                        .map(i32::from)
                        .unwrap_or(0);
                }
            }
            let v = t << (3 - sub_x - sub_y);
            l[i * tx_w + j] = v;
            luma_avg += i64::from(v);
        }
    }
    let shift = tx_w.trailing_zeros() + tx_h.trailing_zeros();
    let luma_avg = ((luma_avg + (1i64 << (shift - 1))) >> shift) as i32;
    for i in 0..tx_h {
        for j in 0..tx_w {
            let dc = pred[i * tx_w + j];
            let diff = i64::from(cfl.alpha) * i64::from(l[i * tx_w + j] - luma_avg);
            let scaled_luma = round2_signed(diff, 6) as i32;
            pred[i * tx_w + j] = clip1(dc + scaled_luma);
        }
    }
}

impl<'a> TileDecodeState<'a> {
    /// `get_palette_cache(plane)` (AV1 spec §5.11.46): merges the above and
    /// left neighbours' palettes into a single sorted, duplicate-free cache
    /// (both inputs are already sorted ascending, so this is a sorted merge
    /// rather than the spec's more general `sort()`-based description).
    /// `plane` is `0` for Y or `1` for U (V is never cached).
    fn get_palette_cache(&self, plane: usize, mi_row: usize, mi_col: usize) -> Vec<i32> {
        let (colors_above, colors_left) = if plane == 0 {
            (&self.palette_y_colors_above, &self.palette_y_colors_left)
        } else {
            (&self.palette_u_colors_above, &self.palette_u_colors_left)
        };
        // Spec's literal `(MiRow * MI_SIZE) % 64` gate: the above cache is
        // only used when this block is not at the top of its superblock row
        // (deliberately *not* the general `AvailU`; this is palette-specific).
        let above: &[i32] = if (mi_row * MI_SIZE) % 64 != 0 {
            colors_above.get(mi_col).map(Vec::as_slice).unwrap_or(&[])
        } else {
            &[]
        };
        let left: &[i32] = if mi_col > 0 {
            colors_left.get(mi_row).map(Vec::as_slice).unwrap_or(&[])
        } else {
            &[]
        };
        let mut cache = Vec::with_capacity(above.len() + left.len());
        let (mut ai, mut li) = (0, 0);
        while ai < above.len() && li < left.len() {
            let (a, l) = (above[ai], left[li]);
            if l < a {
                if cache.last() != Some(&l) {
                    cache.push(l);
                }
                li += 1;
            } else {
                if cache.last() != Some(&a) {
                    cache.push(a);
                }
                ai += 1;
                if l == a {
                    li += 1;
                }
            }
        }
        for &v in &above[ai..] {
            if cache.last() != Some(&v) {
                cache.push(v);
            }
        }
        for &v in &left[li..] {
            if cache.last() != Some(&v) {
                cache.push(v);
            }
        }
        cache
    }

    /// Reads `palette_colors_y`/`palette_colors_u` (AV1 spec §5.11.46): the
    /// shared cache + literal + delta-coded-remainder scheme both Y and U
    /// use (V is different — see [`Self::read_palette_colors_v`]). `is_u`
    /// only changes the `range` computation's `-1` (spec: the Y branch
    /// subtracts it, the U branch doesn't — `Clip1` already keeps `val` in
    /// range either way, so this only affects `paletteBits`' shrink rate,
    /// not correctness of `val` itself, but must match to stay in sync with
    /// the encoder's own bit-count choice for the next iteration).
    fn read_palette_colors_yu(&mut self, size: usize, cache: &[i32], is_u: bool) -> Vec<i32> {
        let mut colors = Vec::with_capacity(size);
        let mut idx = 0usize;
        let mut i = 0usize;
        while i < cache.len() && idx < size {
            let use_cache = self.dec.read_literal(1) == 1;
            if use_cache {
                colors.push(cache[i]);
                idx += 1;
            }
            i += 1;
        }
        if idx < size {
            colors.push(self.dec.read_literal(BIT_DEPTH) as i32);
            idx += 1;
        }
        let mut palette_bits = 0u32;
        if idx < size {
            let min_bits = BIT_DEPTH - 3;
            let extra = self.dec.read_literal(2);
            palette_bits = min_bits + extra;
        }
        while idx < size {
            let delta = self.dec.read_literal(palette_bits) as i32 + 1;
            let val = clip1(colors[idx - 1] + delta);
            colors.push(val);
            let sub = if is_u { 0 } else { 1 };
            let range = ((1i32 << BIT_DEPTH) - val - sub).max(0) as u32;
            palette_bits = palette_bits.min(ceil_log2(range));
            idx += 1;
        }
        colors.sort_unstable();
        colors
    }

    /// Reads `palette_colors_v` (AV1 spec §5.11.46): a completely different
    /// scheme from Y/U — either `PaletteSizeUV` raw `BitDepth`-bit literals,
    /// or a first literal plus signed, wraparound-clamped deltas. Not sorted
    /// afterward (unlike Y/U).
    fn read_palette_colors_v(&mut self, size: usize) -> Vec<i32> {
        let mut colors = vec![0i32; size];
        let delta_encode = self.dec.read_literal(1) == 1;
        if delta_encode {
            let min_bits = BIT_DEPTH - 4;
            let max_val = 1i32 << BIT_DEPTH;
            let extra = self.dec.read_literal(2);
            let palette_bits = min_bits + extra;
            colors[0] = self.dec.read_literal(BIT_DEPTH) as i32;
            for idx in 1..size {
                let mut delta = self.dec.read_literal(palette_bits) as i32;
                if delta != 0 && self.dec.read_literal(1) == 1 {
                    delta = -delta;
                }
                let mut val = colors[idx - 1] + delta;
                if val < 0 {
                    val += max_val;
                }
                if val >= max_val {
                    val -= max_val;
                }
                colors[idx] = clip1(val);
            }
        } else {
            for slot in &mut colors {
                *slot = self.dec.read_literal(BIT_DEPTH) as i32;
            }
        }
        colors
    }

    /// `palette_mode_info()` (AV1 spec §5.11.46). Returns the (possibly
    /// empty — empty meaning `PaletteSize{Y,UV} == 0`) `palette_colors_{y,u,v}`
    /// arrays. The UV branch is gated on `has_chroma` (AV1 spec §5.11.5,
    /// see [`has_chroma`]'s doc comment) in addition to `uv_mode == DC_PRED`
    /// — callers pass `uv_mode == DC_PRED` (never a real chroma mode) for a
    /// block whose own `has_chroma` is false, so this second gate is the one
    /// that actually matters there.
    pub(super) fn read_palette_mode_info(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
        y_mode: usize,
        uv_mode: usize,
        has_chroma: bool,
    ) -> (Vec<i32>, Vec<i32>, Vec<i32>) {
        if bsize < BLOCK_8X8
            || BLOCK_WIDTH[bsize] > 64
            || BLOCK_HEIGHT[bsize] > 64
            || !self.allow_screen_content_tools
        {
            return (Vec::new(), Vec::new(), Vec::new());
        }
        let bsize_ctx = mi_width_log2(bsize) + mi_height_log2(bsize) - 2;

        let mut colors_y = Vec::new();
        if y_mode == DC_PRED as usize {
            let above_has = mi_row > 0
                && self
                    .palette_y_colors_above
                    .get(mi_col)
                    .is_some_and(|v| !v.is_empty());
            let left_has = mi_col > 0
                && self
                    .palette_y_colors_left
                    .get(mi_row)
                    .is_some_and(|v| !v.is_empty());
            let ctx = above_has as usize + left_has as usize;
            if self
                .mode_cdfs
                .read_has_palette_y(&mut self.dec, bsize_ctx, ctx)
            {
                let size = 2 + self.mode_cdfs.read_palette_size_y(&mut self.dec, bsize_ctx);
                let cache = self.get_palette_cache(0, mi_row, mi_col);
                colors_y = self.read_palette_colors_yu(size, &cache, false);
            }
        }

        let mut colors_u = Vec::new();
        let mut colors_v = Vec::new();
        if has_chroma && uv_mode == DC_PRED as usize {
            let ctx = usize::from(!colors_y.is_empty());
            if self.mode_cdfs.read_has_palette_uv(&mut self.dec, ctx) {
                let size = 2 + self
                    .mode_cdfs
                    .read_palette_size_uv(&mut self.dec, bsize_ctx);
                let cache = self.get_palette_cache(1, mi_row, mi_col);
                colors_u = self.read_palette_colors_yu(size, &cache, true);
                colors_v = self.read_palette_colors_v(size);
            }
        }

        (colors_y, colors_u, colors_v)
    }

    /// `palette_tokens()` (AV1 spec §5.11.49): decodes the full-coded-block
    /// `ColorMapY`/`ColorMapUV` (flat, row-major, `blockWidth`-strided; index
    /// `[i * stride + j]`). Returns `(map, stride)`; an empty map means the
    /// corresponding `PaletteSize` was `0` (nothing to read).
    pub(super) fn read_color_map(
        &mut self,
        bsize: usize,
        mi_row: usize,
        mi_col: usize,
        n: usize,
        is_uv: bool,
    ) -> (Vec<u8>, usize) {
        if n == 0 {
            return (Vec::new(), 0);
        }
        let sub_x = usize::from(self.subsampling_x);
        let sub_y = usize::from(self.subsampling_y);
        let mut block_width = BLOCK_WIDTH[bsize];
        let mut block_height = BLOCK_HEIGHT[bsize];
        let mut onscreen_width = block_width.min(self.mi_cols.saturating_sub(mi_col) * MI_SIZE);
        let mut onscreen_height = block_height.min(self.mi_rows.saturating_sub(mi_row) * MI_SIZE);
        if is_uv {
            block_width >>= sub_x;
            block_height >>= sub_y;
            onscreen_width >>= sub_x;
            onscreen_height >>= sub_y;
            if block_width < 4 {
                block_width += 2;
                onscreen_width += 2;
            }
            if block_height < 4 {
                block_height += 2;
                onscreen_height += 2;
            }
        }
        let stride = block_width;
        let mut map = vec![0u8; block_width * block_height];

        let color_idx_y0 = read_ns(&mut self.dec, n as u32) as u8;
        map[0] = color_idx_y0;
        // `onscreen_width`/`onscreen_height` are only `0` if this block's mi
        // position is already off the (frame-wide) `mi_cols`/`mi_rows` grid,
        // which the partition-tree recursion's `hasRows`/`hasCols` guards
        // should make unreachable — but this loop's bound subtracts 1 from
        // their sum, so guard it explicitly rather than trust that invariant
        // under adversarial/fuzzed input.
        if onscreen_width > 0 && onscreen_height > 0 {
            for i in 1..(onscreen_height + onscreen_width - 1) {
                let j_hi = i.min(onscreen_width - 1);
                let j_lo = (i + 1).saturating_sub(onscreen_height);
                let mut j = j_hi;
                loop {
                    let (r, c) = (i - j, j);
                    let (ctx, color_order) = get_palette_color_context(&map, stride, r, c, n);
                    let sym = if is_uv {
                        self.mode_cdfs
                            .read_palette_color_idx_uv(&mut self.dec, n, ctx)
                    } else {
                        self.mode_cdfs
                            .read_palette_color_idx_y(&mut self.dec, n, ctx)
                    };
                    map[r * stride + c] = color_order[sym];
                    if j == j_lo {
                        break;
                    }
                    j -= 1;
                }
            }
        }
        if onscreen_width > 0 {
            for i in 0..onscreen_height {
                let edge = map[i * stride + onscreen_width - 1];
                for j in onscreen_width..block_width {
                    map[i * stride + j] = edge;
                }
            }
        }
        if onscreen_height > 0 {
            for i in onscreen_height..block_height {
                let (src, dst) = map.split_at_mut(i * stride);
                let edge_row = &src[(onscreen_height - 1) * stride..onscreen_height * stride];
                dst[..block_width].copy_from_slice(edge_row);
            }
        }
        (map, stride)
    }
}
