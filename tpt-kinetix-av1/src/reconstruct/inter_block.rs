use super::*;

/// Masked-compound descriptor for one inter block, passed to every plane's
/// prediction call (§5.11.26 / §7.11.3.14). `comp_type` uses dav1d numbering:
/// 3 = `COMPOUND_DIFFWTD` (seg), 4 = `COMPOUND_WEDGE`; any other value means
/// no masked blend.
#[derive(Clone, Copy)]
pub(super) struct MaskDesc {
    pub comp_type: u8,
    pub wedge_index: usize,
    pub mask_sign: bool,
    pub bsize: usize,
}

impl MaskDesc {
    /// No masked blend (single-ref, or `COMP_INTER_AVG` / `_WEIGHTED_AVG`).
    fn none() -> Self {
        Self {
            comp_type: 0,
            wedge_index: 0,
            mask_sign: false,
            bsize: 0,
        }
    }
}

/// Kinetix ref name (`NONE = 0`, `INTRA = 1`, `LAST_FRAME = 2` … `ALTREF_FRAME
/// = 8`) → dav1d numbering (`LAST = 0` … `ALTREF = 6`); `NONE`/`INTRA` → `-1`.
#[inline]
fn dav1d_ref(k: u8) -> i32 {
    if k >= LAST_FRAME {
        k as i32 - 2
    } else {
        -1
    }
}

/// `interintra_allowed_mask` (dav1d `tables.h`): single-ref block sizes that
/// may carry an inter-intra flag — {8x8, 8x16, 16x8, 16x16, 16x32, 32x16,
/// 32x32} in spec `BlockSize` indices.
fn interintra_allowed(bsize: usize) -> bool {
    matches!(
        bsize,
        BLOCK_8X8 | BLOCK_8X16 | BLOCK_16X8 | BLOCK_16X16 | BLOCK_16X32 | BLOCK_32X16 | BLOCK_32X32
    )
}

/// `wedge_allowed_mask` (dav1d `tables.h`): `interintra_allowed` plus 8x32 /
/// 32x8.
fn wedge_allowed(bsize: usize) -> bool {
    interintra_allowed(bsize) || matches!(bsize, BLOCK_8X32 | BLOCK_32X8)
}

/// dav1d `get_poc_diff` — signed order-hint difference, wrapped to the
/// `order_hint_n_bits` window.
fn poc_diff(order_hint_n_bits: u8, poc0: i32, poc1: i32) -> i32 {
    if order_hint_n_bits == 0 {
        return 0;
    }
    let mask = 1i32 << (order_hint_n_bits - 1);
    let diff = poc0 - poc1;
    (diff & (mask - 1)) - (diff & mask)
}

/// dav1d `f->jnt_weights[ref0][ref1]` computed per block (§7.11.3.15
/// `distance_weights`): the sixteenths weight for `preds[0]` in the
/// distance-weighted compound blend.
fn jnt_weight(order_hint_bits: u8, cur: i32, ref0poc: i32, ref1poc: i32) -> i32 {
    let d1 = poc_diff(order_hint_bits, ref0poc, cur).abs().min(31);
    let d0 = poc_diff(order_hint_bits, ref1poc, cur).abs().min(31);
    let order = usize::from(d0 <= d1);
    const QDW: [[i32; 2]; 3] = [[2, 3], [2, 5], [2, 7]];
    const QDT: [[i32; 2]; 4] = [[9, 7], [11, 5], [12, 4], [13, 3]];
    let mut k = 3usize;
    for (kk, w) in QDW.iter().enumerate() {
        let c0 = w[order];
        let c1 = w[1 - order];
        let d0c0 = d0 * c0;
        let d1c1 = d1 * c1;
        if (d0 > d1 && d0c0 < d1c1) || (d0 <= d1 && d0c0 > d1c1) {
            k = kk;
            break;
        }
    }
    QDT[k][order]
}

/// `Size_Group[]` (§ intra-mode size groups) restricted to the inter-intra
/// path — matches dav1d `ymode_size_context` for the allowed sizes.
fn size_group(bsize: usize) -> usize {
    match bsize {
        BLOCK_8X8 | BLOCK_8X16 | BLOCK_16X8 => 1,
        BLOCK_16X16 | BLOCK_16X32 | BLOCK_32X16 => 2,
        BLOCK_32X32 => 3,
        _ => 0,
    }
}

/// dav1d `wedge_ctx_lut` in spec `BlockSize` indices (only the inter-intra /
/// wedge-allowed sizes are ever queried).
fn wedge_ctx(bsize: usize) -> usize {
    match bsize {
        BLOCK_8X8 => 0,
        BLOCK_8X16 => 1,
        BLOCK_16X8 => 2,
        BLOCK_16X16 => 3,
        BLOCK_16X32 => 4,
        BLOCK_32X16 => 5,
        BLOCK_32X32 => 6,
        BLOCK_8X32 => 7,
        BLOCK_32X8 => 8,
        _ => 0,
    }
}

/// `get_obmc_mask(length)` (§7.11.3.9): the OBMC blend weights for an overlap
/// region `length` samples deep, weighting the *neighbour's* prediction. The
/// weights decay away from the shared edge and reach 0 (dav1d
/// `dav1d_obmc_masks`: 2 → {19, 0}, 4 → {25, 14, 5, 0}, 8, 16, 32 ...).
/// The earlier table here was the raised-cosine SMOOTH curve rising to 64 —
/// an inverted, wrong-valued mask that blended the wrong rows with the wrong
/// weights.
fn obmc_mask(length: usize) -> &'static [i32] {
    match length {
        2 => &[19, 0],
        4 => &[25, 14, 5, 0],
        8 => &[28, 22, 16, 11, 7, 3, 0, 0],
        16 => &[30, 27, 24, 21, 18, 15, 12, 10, 8, 6, 4, 3, 0, 0, 0, 0],
        _ => &[
            31, 29, 28, 26, 24, 23, 21, 20, 19, 17, 16, 14, 13, 12, 11, 9, 8, 7, 6, 5, 4, 4, 3, 2,
            0, 0, 0, 0, 0, 0, 0, 0,
        ],
    }
}

impl<'a> TileDecodeState<'a> {
    /// `has_overlappable_candidates()` (§5.11.23): true when the block has an
    /// inter-coded neighbour directly above or to the left (within the tile),
    /// which is the gate for the `motion_mode` / `use_obmc` read.
    fn has_overlappable_candidates(
        &self,
        mi_row: usize,
        mi_col: usize,
        bw: usize,
        bh: usize,
    ) -> bool {
        let row_start = self.tile_px_y0 / MI_SIZE;
        let col_start = self.tile_px_x0 / MI_SIZE;
        if mi_row > row_start {
            for c in mi_col..(mi_col + bw).min(self.mi_cols) {
                if self.is_inter_above[c] != 0 {
                    return true;
                }
            }
        }
        if mi_col > col_start {
            for r in mi_row..(mi_row + bh).min(self.mi_rows) {
                if self.is_inter_left[r] != 0 {
                    return true;
                }
            }
        }
        false
    }

    /// §7.10.4 `find_warp_samples` / `add_sample`. Returns both the
    /// `NumSamples` count `read_motion_mode` (§5.11.23) needs to decide
    /// between the 3-way `motion_mode` symbol and the 2-way `use_obmc` bool
    /// (only its zero/nonzero-ness matters there), *and* the raw
    /// (unfiltered, i.e. not yet mv-diff-thresholded) `CandList` sample
    /// points the §7.13.4 least-squares affine fit needs when `motion_mode
    /// == WARP` is actually selected — dav1d's `derive_warpmv` runs its own
    /// separate threshold-and-replace pass over exactly this list (see
    /// [`warp::derive_warp_model`]), it does not reuse the entropy-side
    /// count. The entropy-critical `NumSamples == 0` test comes from the
    /// real spec scan (verified against a patched dav1d oracle: same
    /// decoded `use_obmc`/`motion_mode` symbol value, different `rng`, when
    /// a previous "any same-ref neighbour" approximation picked the wrong
    /// CDF outright).
    fn find_num_warp_samples(
        &self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
        ref0: u8,
        cur_mv: Mv,
    ) -> (usize, Vec<warp::WarpSample>) {
        let w4 = BLOCK_WIDTH[bsize] / MI_SIZE;
        let h4 = BLOCK_HEIGHT[bsize] / MI_SIZE;
        let row_start = self.tile_px_y0 / MI_SIZE;
        let col_start = self.tile_px_x0 / MI_SIZE;
        let avail_u = mi_row > row_start;
        let avail_l = mi_col > col_start;
        let threshold = (BLOCK_WIDTH[bsize].max(BLOCK_HEIGHT[bsize]) as i32).clamp(16, 112);

        if std::env::var("KINETIX_AV1_DBG_WARP").is_ok() {
            eprintln!("DBG warp mi=({mi_col},{mi_row}) cur_mv={cur_mv:?} threshold={threshold} w4={w4} h4={h4}");
        }
        let mut num_samples = 0usize;
        let mut num_scanned = 0usize;
        let mut raw_samples: Vec<warp::WarpSample> = Vec::with_capacity(8);
        // `px_dx`/`px_dy`/`sx`/`sy` are dav1d `derive_warpmv`'s `add_sample`
        // point-encoding parameters (§7.10.4): `sx`/`sy` in `{-1, 1}` select
        // which corner of the *neighbour* block the sample point sits at,
        // `px_dx`/`px_dy` the mi-unit offset from the current block's
        // top-left corner. These are independent of `dr`/`dc` (the grid
        // coordinates used to *fetch* the neighbour cell) whenever a large
        // neighbour is refetched at an interior mi cell — see the
        // same-size-neighbour branches below.
        let mut add_sample = |dr: isize, dc: isize, px_dx: i32, px_dy: i32, sx: i32, sy: i32| {
            const LEAST_SQUARES_SAMPLES_MAX: usize = 8;
            if num_scanned >= LEAST_SQUARES_SAMPLES_MAX {
                return;
            }
            let mv_row = mi_row as isize + dr;
            let mv_col = mi_col as isize + dc;
            if mv_row < row_start as isize
                || mv_col < col_start as isize
                || mv_row >= self.mi_rows as isize
                || mv_col >= self.mi_cols as isize
            {
                return;
            }
            let cell = self.refmv_cell(mv_row as usize, mv_col as usize);
            if std::env::var("KINETIX_AV1_DBG_WARP").is_ok() {
                eprintln!(
                    "DBG warp add_sample dr={dr} dc={dc} -> ({mv_row},{mv_col}) \
                     cell.refs={:?} cell.mv={:?} ref0={ref0}",
                    cell.refs, cell.mv[0]
                );
            }
            if cell.refs[0] != ref0 || cell.refs[1] != NONE_FRAME {
                return;
            }
            num_scanned += 1;
            // Raw (pre-threshold) point pair — dav1d `add_sample` macro:
            // `pts[np][0] = 16*(2*dx + sx*bw4(neighbour)) - 8`, `pts[np][1] =
            // pts[np][0] + neighbour_mv`. Built regardless of the mv-diff
            // threshold below (that filtering happens later, only if
            // `motion_mode == WARP` is actually selected).
            if raw_samples.len() < LEAST_SQUARES_SAMPLES_MAX {
                let nb_w4 = (cell.w4 as i32).max(1);
                let nb_h4 = (cell.h4 as i32).max(1);
                let src_x = 16 * (2 * px_dx + sx * nb_w4) - 8;
                let src_y = 16 * (2 * px_dy + sy * nb_h4) - 8;
                raw_samples.push(warp::WarpSample {
                    src: [src_x, src_y],
                    dst: [src_x + cell.mv[0].col, src_y + cell.mv[0].row],
                });
            }
            let mv_diff = (cell.mv[0].row - cur_mv.row).abs() + (cell.mv[0].col - cur_mv.col).abs();
            let valid = mv_diff <= threshold;
            // §7.10.4.2: an invalid sample past the first scanned one is
            // simply not added to NumSamples/CandList — it must NOT halt
            // the outer scan (only the LEAST_SQUARES_SAMPLES_MAX cap above
            // does that). A stray `stop` flag here previously caused every
            // later add_sample() call in the whole find_warp_samples scan
            // (later top-edge steps, the left edge, top-left, top-right) to
            // be skipped outright once one early sample missed the mv-diff
            // threshold, diverging the grid cells visited from dav1d/spec.
            if !valid && num_scanned > 1 {
                return;
            }
            if valid {
                num_samples += 1;
            }
        };

        let mut do_top_left = true;
        let mut do_top_right = true;
        if avail_u {
            let src = self.refmv_cell(mi_row - 1, mi_col);
            let src_w = (src.w4 as usize).max(1);
            if w4 <= src_w {
                let off = mi_col & (src_w - 1);
                if off != 0 {
                    do_top_left = false;
                }
                if src_w - off > w4 {
                    do_top_right = false;
                }
                add_sample(-1, 0, -(off as i32), 0, 1, -1);
            } else {
                let mut i = 0usize;
                let limit = w4.min(self.mi_cols.saturating_sub(mi_col));
                while i < limit {
                    let cell = self.refmv_cell(mi_row - 1, mi_col + i);
                    let step = w4.min((cell.w4 as usize).max(1)).max(1);
                    add_sample(-1, i as isize, i as i32, 0, 1, -1);
                    i += step;
                }
            }
        }
        if avail_l {
            let src = self.refmv_cell(mi_row, mi_col - 1);
            let src_h = (src.h4 as usize).max(1);
            if h4 <= src_h {
                let off = mi_row & (src_h - 1);
                if off != 0 {
                    do_top_left = false;
                }
                add_sample(0, -1, 0, -(off as i32), -1, 1);
            } else {
                let mut i = 0usize;
                let limit = h4.min(self.mi_rows.saturating_sub(mi_row));
                while i < limit {
                    let cell = self.refmv_cell(mi_row + i, mi_col - 1);
                    let step = h4.min((cell.h4 as usize).max(1)).max(1);
                    add_sample(i as isize, -1, 0, i as i32, -1, 1);
                    i += step;
                }
            }
        }
        if do_top_left {
            add_sample(-1, -1, 0, 0, -1, -1);
        }
        if do_top_right && w4.max(h4) <= 16 {
            add_sample(-1, w4 as isize, w4 as i32, 0, 1, -1);
        }
        if num_samples == 0 && num_scanned > 0 {
            num_samples = 1;
        }
        (num_samples, raw_samples)
    }

    /// Fetch a `refmv_grid` cell, treating out-of-range coordinates as an
    /// unwritten (`NONE_FRAME`) cell rather than panicking.
    fn refmv_cell(&self, row: usize, col: usize) -> RefMvCell {
        if row >= self.mi_rows || col >= self.mi_cols {
            RefMvCell::default()
        } else {
            self.refmv_grid[row * self.refmv_stride + col]
        }
    }

    /// Build the §8.3.2 compound-context neighbour edge for the mi cell above
    /// `mi_col` (converting Kinetix ref names `LAST_FRAME = 2 …` to dav1d's
    /// `LAST = 0 …`).
    fn comp_edge_above(&self, mi_col: usize) -> CompEdge {
        if self.is_inter_above.get(mi_col).copied().unwrap_or(0) == 0 {
            return CompEdge::NA;
        }
        let r = self.ref_above[mi_col];
        CompEdge {
            intra: false,
            comp_type: self.comp_type_above[mi_col],
            ref0: dav1d_ref(r[0]),
            ref1: dav1d_ref(r[1]),
        }
    }

    fn comp_edge_left(&self, mi_row: usize) -> CompEdge {
        if self.is_inter_left.get(mi_row).copied().unwrap_or(0) == 0 {
            return CompEdge::NA;
        }
        let r = self.ref_left[mi_row];
        CompEdge {
            intra: false,
            comp_type: self.comp_type_left[mi_row],
            ref0: dav1d_ref(r[0]),
            ref1: dav1d_ref(r[1]),
        }
    }

    /// Inter-coded leaf block (AV1 Phase E): MV prediction (§7.10) + motion
    /// compensation (§7.11.3). Reconstructs a single/compound-reference block and
    /// adds the residual.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn decode_inter_block(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
    ) -> Result<(), KinetixError> {
        let bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh = BLOCK_HEIGHT[bsize] / MI_SIZE;
        if std::env::var("KINETIX_AV1_DBG_SB1").is_ok()
            && (16..=18).contains(&mi_row)
            && mi_col <= 2
        {
            eprintln!(
                "DBG SB1 INTER mi=({mi_col},{mi_row}) bsize={bsize} bit_pos={}",
                self.dec.bit_position()
            );
        }
        if std::env::var("KINETIX_AV1_DBG_B0ENTER").is_ok() {
            eprintln!(
                "DBG b0enter mi=({mi_col},{mi_row}) bsize={bsize} rng={}",
                self.dec.raw_state().0
            );
        }
        let allow_hp = self.allow_high_precision_mv;
        let frame_filter = self.interpolation_filter;
        let reference_select = self.reference_select;

        // `read_skip_mode()` (§5.11.11) — read *before* `read_skip()` in the
        // inter syntax order. A skip-mode block skips every other mode symbol:
        // it is compound, non-residual, and predicts from the fixed
        // `SkipModeFrame` pair with the NEAREST MVs.
        let above_skip = self.skip_above[mi_col] as usize;
        let left_skip = self.skip_left[mi_row] as usize;
        let above_inter = self.is_inter_above[mi_col] as usize;
        let left_inter = self.is_inter_left[mi_row] as usize;
        let skip_mode = if self.seg_feature_skip
            || !self.skip_mode_present
            || BLOCK_WIDTH[bsize] < 8
            || BLOCK_HEIGHT[bsize] < 8
        {
            false
        } else {
            let ctx = self.skip_mode_above[mi_col] as usize + self.skip_mode_left[mi_row] as usize;
            let sm = self
                .dec
                .read_symbol(&mut self.mode_cdfs.skip_mode[ctx.min(2)])
                == 1;
            if std::env::var("KINETIX_AV1_DBG_B0").is_ok() {
                eprintln!(
                    "DBG b0 skipmode={sm} ctx={ctx} rng={}",
                    self.dec.raw_state().0
                );
            }
            sm
        };
        if skip_mode {
            return self.decode_skip_mode_block(mi_row, mi_col, bsize);
        }

        // Skip flag (§5.11.11) — read before `is_inter`, matching the inter
        // syntax order.
        let skip_ctx = (above_skip + left_skip).min(2);
        let skip = if self.seg_feature_skip {
            true
        } else {
            self.mode_cdfs.read_skip(&mut self.dec, skip_ctx) == 1
        };
        let dbg_b0 = std::env::var("KINETIX_AV1_DBG_B0").is_ok() && mi_row < 40 && mi_col < 40;
        if dbg_b0 {
            eprintln!(
                "DBG b0 mi=({mi_col},{mi_row}) bsize={bsize} skip={skip} rng={}",
                self.dec.raw_state().0
            );
        }

        // AV1 spec §5.11.18 `inter_frame_mode_info()`: `read_cdef()`/
        // `read_delta_qindex()`/`read_delta_lf()` come right after
        // `read_skip()`, then `ReadDeltas = 0`, before `read_is_inter()` —
        // same order and same no-op-unless-enabled behaviour as the intra
        // path (`intra_block.rs`).
        self.read_cdef(mi_row, mi_col, bsize, skip);
        self.read_delta_qindex(bsize, skip);
        self.read_delta_lf(bsize, skip);
        self.read_deltas = false;

        // `intra_inter` context (§8.3.2): based on whether the *available*
        // above/left neighbours are INTRA-coded, not a plain is-inter sum.
        let row_start = self.tile_px_y0 / MI_SIZE;
        let col_start = self.tile_px_x0 / MI_SIZE;
        let avail_u = mi_row > row_start;
        let avail_l = mi_col > col_start;
        let above_intra = avail_u && above_inter == 0;
        let left_intra = avail_l && left_inter == 0;
        let inter_ctx = if avail_u && avail_l {
            if above_intra && left_intra {
                3
            } else {
                usize::from(above_intra || left_intra)
            }
        } else if avail_u || avail_l {
            2 * usize::from(if avail_u { above_intra } else { left_intra })
        } else {
            0
        };
        if std::env::var("KINETIX_AV1_DBG_CDFROW").is_ok() {
            eprintln!(
                "KIN INTRACDF ctx={inter_ctx} row={:?} rng={} mi=({mi_col},{mi_row}) oh={}",
                &self.map_inter_cdfs.is_inter[inter_ctx][..],
                self.dec.raw_state().0,
                self.cur_order_hint
            );
        }
        let is_inter = self
            .dec
            .read_symbol(&mut self.map_inter_cdfs.is_inter[inter_ctx])
            == 1;
        if dbg_b0 {
            eprintln!(
                "DBG b0 is_inter={is_inter} ctx={inter_ctx} rng={}",
                self.dec.raw_state().0
            );
        }

        if !is_inter {
            // Intra-coded block inside an inter frame: reconstruct via the shared
            // intra machinery (mode symbols still read in inter order: skip above
            // is already consumed, so read y/uv mode then dispatch).
            // `y_mode` (§8.3.2), NOT `intra_frame_y_mode` — an intra block
            // coded inside an inter frame uses the single-context
            // `TileYModeCdf[Size_Group[MiSize]]`, unrelated to the
            // above/left-neighbour-mode 2D context the keyframe path reads
            // (`read_intra_y_mode`/`intra_y_mode`). Reusing the keyframe CDF
            // here shares the same 13-mode alphabet so it never desynced by
            // producing an invalid symbol, but adapts the wrong CDF entries
            // under the wrong context, diverging the coder's `rng` from the
            // very first intra-in-inter-frame block onward.
            let y_mode = self.mode_cdfs.read_y_mode(&mut self.dec, SIZE_GROUP[bsize]);
            if dbg_b0 {
                eprintln!("DBG b0 ymode={y_mode} rng={}", self.dec.raw_state().0);
            }
            // `intra_angle_info_y()` (AV1 spec §5.11.42), same as the
            // keyframe path.
            let angle_delta_y = if bsize >= BLOCK_8X8 && is_directional_mode(y_mode as u8) {
                self.mode_cdfs.read_angle_delta(&mut self.dec, y_mode)
            } else {
                0
            };
            // `HasChroma` (AV1 spec §5.11.5) — see `has_chroma`'s doc comment;
            // same gate as the keyframe path.
            let has_chroma = !self.monochrome
                && has_chroma(
                    bsize,
                    mi_row,
                    mi_col,
                    self.subsampling_x,
                    self.subsampling_y,
                );
            let uv_mode = if has_chroma {
                self.mode_cdfs
                    .read_uv_mode(&mut self.dec, cfl_allowed_for_bsize(bsize), y_mode)
            } else {
                DC_PRED as usize
            };
            if dbg_b0 {
                eprintln!(
                    "DBG b0 uvmode={uv_mode} has_chroma={has_chroma} rng={}",
                    self.dec.raw_state().0
                );
            }
            // `read_cfl_alphas()` (AV1 spec §5.11.45): read only when
            // `UVMode == UV_CFL_PRED`, immediately after `uv_mode` and before
            // `intra_angle_info_uv()`, per the `intra_block_mode_info()`
            // syntax order.
            let cfl_alpha = if has_chroma && uv_mode == UV_CFL_PRED {
                Some(self.mode_cdfs.read_cfl_alphas(&mut self.dec))
            } else {
                None
            };
            // `intra_angle_info_uv()` (AV1 spec §5.11.43).
            let angle_delta_uv =
                if has_chroma && bsize >= BLOCK_8X8 && is_directional_mode(uv_mode as u8) {
                    self.mode_cdfs.read_angle_delta(&mut self.dec, uv_mode)
                } else {
                    0
                };
            // `palette_mode_info()` (AV1 spec §5.11.46), same position as the
            // keyframe path.
            let (colors_y, colors_u, colors_v) =
                self.read_palette_mode_info(mi_row, mi_col, bsize, y_mode, uv_mode, has_chroma);
            // `filter_intra_mode_info()` (AV1 spec §5.11.24) is also read for
            // an intra block coded inside an inter frame (spec
            // `intra_block_mode_info()` calls it right after the mode reads,
            // same as the keyframe path) — this call site previously omitted
            // it entirely, which would desync every such block once inter
            // frames are actually decoded.
            let filter_intra_mode = if colors_y.is_empty() {
                self.mode_cdfs.read_filter_intra_mode_info(
                    &mut self.dec,
                    self.enable_filter_intra,
                    y_mode,
                    bsize,
                )
            } else {
                None
            };
            // `palette_tokens()` (AV1 spec §5.11.49), same position as the
            // keyframe path.
            let (map_y, stride_y) =
                self.read_color_map(bsize, mi_row, mi_col, colors_y.len(), false);
            let (map_uv, stride_uv) =
                self.read_color_map(bsize, mi_row, mi_col, colors_u.len(), true);
            let palette = PaletteData {
                colors_y,
                colors_u,
                colors_v,
                map_y,
                stride_y,
                map_uv,
                stride_uv,
            };
            // `read_tx_size`'s `allowSelect = !skip || !is_inter` is always
            // true here (`is_inter` is false on this branch), so `skip`
            // does not gate this read for an intra block — only for a true
            // inter block (see the other `read_tx_size` call site below).
            let max_tx = max_tx_size_for_bsize(bsize);
            let luma_tx = if self.tx_mode_select && !self.lossless {
                self.read_tx_size(bsize, max_tx, mi_row, mi_col)
            } else {
                max_tx
            };
            if dbg_b0 {
                eprintln!(
                    "DBG b0 intra-in-inter tx={luma_tx} skip={skip} rng={}",
                    self.dec.raw_state().0
                );
            }
            self.reconstruct_intra_subblock(
                mi_row,
                mi_col,
                bsize,
                y_mode,
                uv_mode,
                skip,
                luma_tx,
                filter_intra_mode,
                cfl_alpha,
                angle_delta_y,
                angle_delta_uv,
                &palette,
            )?;
            if dbg_b0 {
                eprintln!(
                    "DBG b0 intra-in-inter post-residual rng={}",
                    self.dec.raw_state().0
                );
            }
            // Update inter neighbour state (this block is not inter). The
            // ref/MV neighbour arrays must also be cleared: dav1d marks intra
            // blocks with ref == INTRA (so OBMC's `ref.ref[0] > 0` overlap
            // scan skips them); leaving the previous inter block's ref/MV
            // here made OBMC blend with phantom neighbours.
            for r in mi_row..(mi_row + bh).min(self.mi_rows) {
                if let Some(s) = self.is_inter_left.get_mut(r) {
                    *s = 0;
                }
                if let Some(s) = self.comp_type_left.get_mut(r) {
                    *s = 0;
                }
                if let Some(slot) = self.ref_left.get_mut(r) {
                    slot[0] = crate::inter::INTRA_FRAME;
                    slot[1] = crate::inter::NONE_FRAME;
                }
                if let Some(slot) = self.mv_left.get_mut(r) {
                    slot[0] = Mv::default();
                    slot[1] = Mv::default();
                }
            }
            for c in mi_col..(mi_col + bw).min(self.mi_cols) {
                if let Some(s) = self.is_inter_above.get_mut(c) {
                    *s = 0;
                }
                if let Some(s) = self.comp_type_above.get_mut(c) {
                    *s = 0;
                }
                if let Some(slot) = self.ref_above.get_mut(c) {
                    slot[0] = crate::inter::INTRA_FRAME;
                    slot[1] = crate::inter::NONE_FRAME;
                }
                if let Some(slot) = self.mv_above.get_mut(c) {
                    slot[0] = Mv::default();
                    slot[1] = Mv::default();
                }
            }
            // Every decoded block must splat the 2-D ref-MV grid (see
            // `splat_refmv`'s doc comment) — an intra-coded block inside an
            // inter frame was the one call site that didn't, leaving stale
            // grid cells that `find_warp_samples` (§7.10.4, used by
            // `read_motion_mode`) and the inter MV stack would misread as an
            // inter neighbour's real ref/MV.
            self.splat_refmv(mi_row, mi_col, bsize, None);
            // §7.14.4: record the block's current DeltaLF over its span. The
            // inter path does this after its residual; this early-returning
            // intra branch previously skipped it, leaving stale (all-zero)
            // grid deltas for these cells — and since DeltaLF persists until
            // a later block re-reads it, every edge whose level resolves
            // through such a cell derived its strength from a delta that
            // was never the block's own.
            let blk_px_x = mi_col * MI_SIZE - self.tile_px_x0;
            let blk_px_y = mi_row * MI_SIZE - self.tile_px_y0;
            let bx0 = blk_px_x / 8;
            let by0 = blk_px_y / 8;
            let bx1 = (blk_px_x + bw * MI_SIZE).div_ceil(8);
            let by1 = (blk_px_y + bh * MI_SIZE).div_ceil(8);
            self.meta.record_delta_lf(bx0, by0, bx1, by1, self.delta_lf);
            self.meta.record_delta_lf4(
                blk_px_x / 4,
                blk_px_y / 4,
                (blk_px_x + bw * MI_SIZE).div_ceil(4),
                (blk_px_y + bh * MI_SIZE).div_ceil(4),
                self.delta_lf,
            );
            // Intra block: ref = INTRA_FRAME (0), modeType = 0 (§7.14.4).
            let (lu, lv) = crate::reconstruct::chroma_lf_levels_snapshot(
                self.lf_frame_levels,
                self.lf_ref_deltas,
                self.lf_mode_deltas,
                self.lf_delta_enabled,
                self.delta_lf,
                0,
                0,
            );
            self.meta.record_lf_level_chroma(
                blk_px_x / 8,
                blk_px_y / 8,
                (blk_px_x + bw * MI_SIZE).div_ceil(8),
                (blk_px_y + bh * MI_SIZE).div_ceil(8),
                lu as u8,
                lv as u8,
            );
            return Ok(());
        }

        // Compound vs single reference (§5.11.25). `comp_mode` is read only
        // when compound is allowed (`reference_select` / `switchable_comp_refs`
        // and the block is at least 8x8), with the §8.3.2 `get_comp_ctx`
        // neighbour context.
        let cedge_a = self.comp_edge_above(mi_col);
        let cedge_l = self.comp_edge_left(mi_row);
        let compound = if reference_select && BLOCK_WIDTH[bsize].min(BLOCK_HEIGHT[bsize]) > 4 {
            let ctx = comp_ctx(cedge_a, cedge_l, avail_u, avail_l);
            self.dec
                .read_symbol(&mut self.map_inter_cdfs.comp_mode[ctx])
                == 1
        } else {
            false
        };
        if dbg_b0 {
            eprintln!(
                "DBG b0 compflag={} rng={}",
                compound as u8,
                self.dec.raw_state().0
            );
        }

        // Reference name(s).
        let mut ref_names = [NONE_FRAME; 2];
        if compound {
            // Compound reference-frame tree (§5.11.25), ported from dav1d
            // (`decode.c`). dav1d ref numbering (LAST=0..ALTREF=6) internally;
            // `+ 2` converts back to Kinetix's `LAST_FRAME = 2` names. Kinetix's
            // CDF tables are stored transposed vs dav1d — `cdf[ctx][i]` where
            // dav1d indexes `cdf[i][ctx]`.
            let dir_ctx = comp_dir_ctx(cedge_a, cedge_l, avail_u, avail_l);
            let (fwd, bwd);
            if self
                .dec
                .read_symbol(&mut self.map_inter_cdfs.comp_ref_type[dir_ctx.min(4)])
                == 1
            {
                // BIDIR
                let c1 = fwd_ref_ctx(cedge_a, cedge_l, avail_u, avail_l);
                let f = if self
                    .dec
                    .read_symbol(&mut self.map_inter_cdfs.comp_ref[c1.min(2)][0])
                    == 1
                {
                    let c2 = fwd_ref_2_ctx(cedge_a, cedge_l, avail_u, avail_l);
                    2 + self
                        .dec
                        .read_symbol(&mut self.map_inter_cdfs.comp_ref[c2.min(2)][2])
                } else {
                    let c2 = fwd_ref_1_ctx(cedge_a, cedge_l, avail_u, avail_l);
                    self.dec
                        .read_symbol(&mut self.map_inter_cdfs.comp_ref[c2.min(2)][1])
                };
                let c3 = bwd_ref_ctx(cedge_a, cedge_l, avail_u, avail_l);
                let b = if self
                    .dec
                    .read_symbol(&mut self.map_inter_cdfs.comp_bwd_ref[c3.min(2)][0])
                    == 1
                {
                    6
                } else {
                    let c4 = bwd_ref_1_ctx(cedge_a, cedge_l, avail_u, avail_l);
                    4 + self
                        .dec
                        .read_symbol(&mut self.map_inter_cdfs.comp_bwd_ref[c4.min(2)][1])
                };
                fwd = f as u8 + 2;
                bwd = b as u8 + 2;
            } else {
                // UNIDIR
                let up = ref_ctx(cedge_a, cedge_l, avail_u, avail_l);
                if self
                    .dec
                    .read_symbol(&mut self.map_inter_cdfs.uni_comp_ref[up.min(2)][0])
                    == 1
                {
                    fwd = 6; // dav1d BWDREF(4) + 2
                    bwd = 8; // dav1d ALTREF(6) + 2
                } else {
                    let up1 = uni_p1_ctx(cedge_a, cedge_l, avail_u, avail_l);
                    let mut r1 = 1 + self
                        .dec
                        .read_symbol(&mut self.map_inter_cdfs.uni_comp_ref[up1.min(2)][1]);
                    if r1 == 2 {
                        let up2 = fwd_ref_2_ctx(cedge_a, cedge_l, avail_u, avail_l);
                        r1 += self
                            .dec
                            .read_symbol(&mut self.map_inter_cdfs.uni_comp_ref[up2.min(2)][2]);
                    }
                    fwd = 2; // dav1d LAST(0) + 2
                    bwd = r1 as u8 + 2;
                }
            }
            ref_names = [fwd, bwd];
            if dbg_b0 {
                eprintln!(
                    "DBG b0 comprefs={}/{} dir_ctx={dir_ctx} rng={}",
                    fwd - 2,
                    bwd - 2,
                    self.dec.raw_state().0
                );
            }
        } else {
            let above_refs = (above_inter != 0).then(|| self.ref_above[mi_col]);
            let left_refs = (left_inter != 0).then(|| self.ref_left[mi_row]);
            ref_names[0] = read_single_ref_name(
                &mut self.dec,
                &mut self.map_inter_cdfs,
                above_refs,
                left_refs,
            );
            if dbg_b0 {
                eprintln!("DBG b0 ref={} rng={}", ref_names[0], self.dec.raw_state().0);
            }
        }

        let mut mvs = [Mv::default(); 2];
        let force_integer_mv = self.force_integer_mv;
        let mut new_mf = 0u8;
        let mut single_mode = NEARESTMV;
        // §7.14.4 loop-filter mode type for this block: 1 for non-GLOBAL
        // inter modes (NEARESTMV/NEARMV/NEWMV and compound combinations other
        // than GLOBAL_GLOBALMV), 0 for GLOBALMV/GLOBAL_GLOBALMV. Consumed by
        // the deblock level derivation.
        let lf_mode_type: u8;
        // dav1d `has_subpel_filter`: whether this block's interpolation filter
        // symbols are read (false → `EIGHTTAP_REGULAR` unconditionally).
        // Sub-8×8 blocks always interpolate; GLOBALMV blocks interpolate only
        // when their global motion is a (subpel) TRANSLATION.
        let mut has_subpel_filter: bool;
        // dav1d `BlockContext::comp_type` for this block (0 for single-ref).
        let mut block_comp_type = 0u8;
        // Masked-compound parameters (§5.11.26): only meaningful when
        // `block_comp_type` is 3 (COMPOUND_DIFFWTD) or 4 (COMPOUND_WEDGE).
        let mut wedge_index = 0usize;
        let mut mask_sign = false;

        // AV1 §7.10.2 `find_mv_stack` — shared by both branches.
        let (stack, ctx, n_mvs, drl_ctx, comp_mode_ctx) =
            self.inter_mv_stack(mi_row, mi_col, bsize, ref_names);
        if dbg_b0 {
            eprintln!(
                "DBG b0 mvstack n_mvs={n_mvs} ctx=0x{ctx:x} comp_ctx={comp_mode_ctx} \
                 s0=({},{}) s1=({},{})",
                stack.first().map(|s| s[0].row).unwrap_or(0),
                stack.first().map(|s| s[0].col).unwrap_or(0),
                stack.get(1).map(|s| s[0].row).unwrap_or(0),
                stack.get(1).map(|s| s[0].col).unwrap_or(0),
            );
        }

        if !compound {
            // §5.11.24 single-ref mode cascade.
            let newmv_ctx = (ctx & 7) as usize;
            let globalmv_ctx = ((ctx >> 3) & 1) as usize;
            let refmv_ctx = ((ctx >> 4) & 15) as usize;

            // `new_mv` S(): 1 => NOT newmv, 0 => NEWMV.
            let not_newmv = self
                .dec
                .read_symbol(&mut self.map_inter_cdfs.new_mv[newmv_ctx.min(5)])
                == 1;
            if dbg_b0 {
                eprintln!(
                    "DBG b0 new_mv not={not_newmv} rng={}",
                    self.dec.raw_state().0
                );
            }
            let mut drl_idx = 0usize;
            let mode: u8;
            if not_newmv {
                // `zero_mv` S(): 0 => GLOBALMV, 1 => near path.
                let near_path = self
                    .dec
                    .read_symbol(&mut self.map_inter_cdfs.zero_mv[globalmv_ctx.min(1)])
                    == 1;
                if dbg_b0 {
                    eprintln!(
                        "DBG b0 zero_mv near={near_path} rng={}",
                        self.dec.raw_state().0
                    );
                }
                if !near_path {
                    mode = ZEROMV;
                    new_mf = 1;
                } else {
                    // `ref_mv` S(): 1 => NEARMV (+drl), 0 => NEARESTMV.
                    let rm = self
                        .dec
                        .read_symbol(&mut self.map_inter_cdfs.ref_mv[refmv_ctx.min(5)]);
                    if dbg_b0 {
                        eprintln!(
                            "DBG b0 ref_mv={rm} ctx={refmv_ctx} rng={}",
                            self.dec.raw_state().0
                        );
                    }
                    if rm == 1 {
                        mode = NEARMV;
                        drl_idx = 1;
                        if n_mvs > 2 {
                            drl_idx += self
                                .dec
                                .read_symbol(&mut self.map_inter_cdfs.drl_mode[drl_ctx[1].min(2)]);
                            if drl_idx == 2 && n_mvs > 3 {
                                drl_idx += self.dec.read_symbol(
                                    &mut self.map_inter_cdfs.drl_mode[drl_ctx[2].min(2)],
                                );
                            }
                        }
                    } else {
                        mode = NEARESTMV;
                    }
                }
            } else {
                mode = NEWMV;
                new_mf = 2;
                if n_mvs > 1 {
                    drl_idx += self
                        .dec
                        .read_symbol(&mut self.map_inter_cdfs.drl_mode[drl_ctx[0].min(2)]);
                    if drl_idx == 1 && n_mvs > 2 {
                        drl_idx += self
                            .dec
                            .read_symbol(&mut self.map_inter_cdfs.drl_mode[drl_ctx[1].min(2)]);
                    }
                }
            }

            let base_mv = stack.get(drl_idx).map(|m| m[0]).unwrap_or_default();
            if std::env::var("KINETIX_AV1_DBG_IMODE").is_ok() {
                eprintln!(
                    "DBG imode mi=({mi_col},{mi_row}) ref={} ctx={ctx:#x}(nm={newmv_ctx},gm={globalmv_ctx},rm={refmv_ctx}) \
                     mode={mode} drl={drl_idx} n_mvs={n_mvs} base=({},{}) rng={}",
                    ref_names[0], base_mv.row, base_mv.col, self.dec.raw_state().0
                );
            }
            single_mode = mode;
            // GLOBALMV/ZEROMV are the only single-ref modes with modeType 0.
            lf_mode_type = u8::from(mode != crate::inter::GLOBALMV && mode != ZEROMV);
            // dav1d `has_subpel_filter`: sub-8×8 blocks always interpolate;
            // GLOBALMV blocks interpolate only when the global motion is a
            // (subpel) translation; every other mode always does. Consumed by
            // the interpolation-filter read below.
            has_subpel_filter = bw.min(bh) == 1;
            mvs[0] = match mode {
                ZEROMV => {
                    has_subpel_filter |=
                        self.gm_type[(ref_names[0] - 1) as usize] == crate::frame::GM_TRANSLATION;
                    self.get_gmv_2d(ref_names[0], mi_col, mi_row, bw, bh)
                }
                NEWMV => {
                    has_subpel_filter = true;
                    let diff = read_mv(
                        &mut self.dec,
                        &mut self.map_inter_cdfs,
                        allow_hp,
                        force_integer_mv,
                    )?;
                    Mv::new(base_mv.row + diff.row, base_mv.col + diff.col)
                }
                _ => {
                    has_subpel_filter = true;
                    base_mv
                }
            };
        } else {
            // §5.11.24 compound mode cascade (dav1d `decode.c`, `is_comp`).
            // `comp_inter_mode` (8-way) → per-ref sub-mode, then drl, then per-
            // ref MV assignment (NEAREST/NEAR from the stack, NEW = stack +
            // residual, GLOBAL = zero — global-motion warping not modelled).
            let comp_mode = self
                .dec
                .read_symbol(&mut self.map_inter_cdfs.comp_inter_mode[comp_mode_ctx.min(7)]);
            if dbg_b0 {
                eprintln!(
                    "DBG b0 compintermode={comp_mode} ctx={comp_mode_ctx} n_mvs={n_mvs} rng={}",
                    self.dec.raw_state().0
                );
            }
            // dav1d `dav1d_comp_inter_pred_modes` (enum order): per-ref sub-mode
            // as Kinetix mode constants (NEAREST=0, NEAR=1, GLOBAL/ZERO=4,
            // NEW=3).
            const IM: [[u8; 2]; 8] = [
                [NEARESTMV, NEARESTMV],
                [NEARMV, NEARMV],
                [NEARESTMV, NEWMV],
                [NEWMV, NEARESTMV],
                [NEARMV, NEWMV],
                [NEWMV, NEARMV],
                [ZEROMV, ZEROMV],
                [NEWMV, NEWMV],
            ];
            let im = IM[comp_mode.min(7)];
            // dav1d `splat_tworef_mv`: mf bit0 = GLOBALMV_GLOBALMV, bit1 =
            // NEWMV-containing modes (mask `!!((1 << mode) & 0xbc) * 2`; the
            // GLOBAL_NEWMV-shaped row 1 is excluded along with the
            // NEAREST/NEAR-only rows 0-1).
            new_mf = if comp_mode == 6 {
                1
            } else if (1 << comp_mode) & 0xbc != 0 {
                2
            } else {
                0
            };
            // comp_mode 6 = GLOBAL_GLOBALMV — the only compound mode with
            // §7.14.4 modeType 0.
            lf_mode_type = u8::from(comp_mode != 6);

            let mut drl_idx = 0usize;
            if comp_mode == 7 {
                // NEWMV_NEWMV
                if n_mvs > 1 {
                    drl_idx += self
                        .dec
                        .read_symbol(&mut self.map_inter_cdfs.drl_mode[drl_ctx[0].min(2)]);
                    if drl_idx == 1 && n_mvs > 2 {
                        drl_idx += self
                            .dec
                            .read_symbol(&mut self.map_inter_cdfs.drl_mode[drl_ctx[1].min(2)]);
                    }
                }
            } else if im[0] == NEARMV || im[1] == NEARMV {
                drl_idx = 1;
                if n_mvs > 2 {
                    drl_idx += self
                        .dec
                        .read_symbol(&mut self.map_inter_cdfs.drl_mode[drl_ctx[1].min(2)]);
                    if drl_idx == 2 && n_mvs > 3 {
                        drl_idx += self
                            .dec
                            .read_symbol(&mut self.map_inter_cdfs.drl_mode[drl_ctx[2].min(2)]);
                    }
                }
            }

            let base = stack.get(drl_idx).copied().unwrap_or_default();
            // dav1d `has_subpel_filter`: sub-8×8 blocks and every non-
            // GLOBAL_GLOBALMV mode always interpolate; a GLOBAL_GLOBALMV block
            // interpolates only when either reference's global motion is a
            // (subpel) translation.
            has_subpel_filter = bw.min(bh) == 1 || comp_mode != 6;
            for i in 0..2 {
                mvs[i] = match im[i] {
                    NEARESTMV | NEARMV => base[i],
                    ZEROMV => {
                        has_subpel_filter |= self.gm_type[(ref_names[i] - 1) as usize]
                            == crate::frame::GM_TRANSLATION;
                        self.get_gmv_2d(ref_names[i], mi_col, mi_row, bw, bh)
                    }
                    NEWMV => {
                        let diff = read_mv(
                            &mut self.dec,
                            &mut self.map_inter_cdfs,
                            allow_hp,
                            force_integer_mv,
                        )?;
                        Mv::new(base[i].row + diff.row, base[i].col + diff.col)
                    }
                    _ => base[i],
                };
            }
            if dbg_b0 {
                eprintln!(
                    "DBG b0 compmv drl={drl_idx} mv0=({},{}) mv1=({},{}) rng={}",
                    mvs[0].row,
                    mvs[0].col,
                    mvs[1].row,
                    mvs[1].col,
                    self.dec.raw_state().0
                );
            }

            // `read_compound_type()` (§5.11.26): `comp_group_idx` (mask vs
            // jnt/avg), then either `compound_idx` (jnt vs distance-weighted)
            // or the wedge / diffwtd branch + mask-sign literal.
            let mask_ctx = mask_comp_ctx(cedge_a, cedge_l);
            let comp_group_idx = if self.enable_masked_compound {
                self.dec
                    .read_symbol(&mut self.mode_cdfs.mask_comp[mask_ctx.min(5)])
                    == 1
            } else {
                false
            };
            if !comp_group_idx {
                if self.enable_jnt_comp {
                    // poc diffs for `get_jnt_comp_ctx`.
                    let poc = self.cur_order_hint as i32;
                    let ref_poc = |name: u8| -> i32 {
                        let slot = self.ref_to_slot[name as usize] as usize;
                        self.dpb_order_hints.get(slot).copied().unwrap_or(0) as i32
                    };
                    let d0 = poc_diff(self.order_hint_bits, ref_poc(ref_names[0]), poc).abs();
                    let d1 = poc_diff(self.order_hint_bits, poc, ref_poc(ref_names[1])).abs();
                    let jnt_ctx = jnt_comp_ctx(d0 == d1, cedge_a, cedge_l);
                    // 1 (WEIGHTED_AVG) + bit → 1 or 2 (AVG).
                    block_comp_type = 1 + self
                        .dec
                        .read_symbol(&mut self.mode_cdfs.jnt_comp[jnt_ctx.min(5)])
                        as u8;
                } else {
                    block_comp_type = 2; // COMP_INTER_AVG
                }
            } else if wedge_allowed(bsize) {
                let wctx = wedge_ctx(bsize);
                // COMP_INTER_WEDGE(4) - bit → 3 (SEG/diffwtd) or 4 (WEDGE).
                block_comp_type =
                    4 - self.dec.read_symbol(&mut self.mode_cdfs.wedge_comp[wctx]) as u8;
                if block_comp_type == 4 {
                    wedge_index = self.dec.read_symbol(&mut self.mode_cdfs.wedge_idx[wctx]);
                }
                mask_sign = self.dec.read_bool();
            } else {
                block_comp_type = 3; // COMP_INTER_SEG
                mask_sign = self.dec.read_bool();
            }
            if dbg_b0 {
                eprintln!(
                    "DBG b0 comptype grp={} type={block_comp_type} rng={}",
                    comp_group_idx as u8,
                    self.dec.raw_state().0
                );
            }
        }

        // `read_interintra_mode()` (§5.11.28) — read right after the MV cascade
        // (dav1d `Post-interintra`) for a single-ref block whose size is in the
        // inter-intra-allowed set, when the sequence enables inter-intra
        // compound. One `interintra` bool; if set, an `interintra_mode` symbol
        // then (for wedge-allowed sizes) an `interintra_wedge` bool and
        // optionally a `wedge_idx`. Skipping this desynced every eligible block.
        let mut interintra_type = 0u8; // INTER_INTRA_NONE
                                       // Inter-intra mode symbol → our intra prediction mode. The symbol
                                       // alphabet is {DC, V, H, SMOOTH} (dav1d `InterIntraPredMode`), which
                                       // coincides with our IntraPredMode numbering for DC/V/H; SMOOTH (3)
                                       // maps to `SMOOTH` (9).
        let mut interintra_mode = H_PRED;
        let mut ii_wedge_index = 0usize;
        if self.enable_interintra
            && !compound
            && ref_names[1] == NONE_FRAME
            && interintra_allowed(bsize)
        {
            let grp = size_group(bsize);
            let is_ii = self.dec.read_symbol(&mut self.mode_cdfs.interintra[grp]) == 1;
            if is_ii {
                interintra_mode = match self
                    .dec
                    .read_symbol(&mut self.mode_cdfs.interintra_mode[grp])
                {
                    0 => DC_PRED,
                    1 => V_PRED,
                    2 => H_PRED,
                    _ => SMOOTH,
                };
                let wctx = wedge_ctx(bsize);
                // INTER_INTRA_BLEND (1) + wedge bit → BLEND or WEDGE (2).
                interintra_type = 1 + self
                    .dec
                    .read_symbol(&mut self.mode_cdfs.interintra_wedge[wctx])
                    as u8;
                if interintra_type == 2 {
                    ii_wedge_index = self.dec.read_symbol(&mut self.mode_cdfs.wedge_idx[wctx]);
                }
            }
            if dbg_b0 {
                eprintln!(
                    "DBG b0 interintra type={interintra_type} rng={}",
                    self.dec.raw_state().0
                );
            }
        }

        // `read_motion_mode()` (§5.11.23) — read after the MV/mode cascade and
        // before the interpolation filter. dav1d (`Post-motionmode`) reads a
        // symbol here whenever the block is single-ref, not skip_mode, at least
        // 8x8, `is_motion_mode_switchable`, ref[1] != INTRA_FRAME, and has an
        // overlappable (inter-coded) above/left neighbour. When a *matching-ref*
        // neighbour also exists (dav1d `find_matching_ref` mask nonzero) and
        // warped motion is enabled the 3-way `motion_mode` symbol is read,
        // otherwise the `use_obmc` bool.
        let mut motion_mode = 0u8; // SIMPLE
                                   // §7.13.3/§7.13.4 local warp model, derived only when `motion_mode ==
                                   // WARP` (2) is actually selected below. `None` covers both "not a
                                   // WARP block" and dav1d's own translation-only fallback (LS system
                                   // singular, or the fitted shear too extreme to filter) — either way
                                   // the caller falls back to the ordinary translational prediction.
        let mut warp_model: Option<warp::WarpModel> = None;
        {
            let min_dim = BLOCK_WIDTH[bsize].min(BLOCK_HEIGHT[bsize]);
            let _ = single_mode; // GLOBALMV modelled translation-only: GmType never > TRANSLATION
            let eligible = !compound
                && self.is_motion_mode_switchable
                && min_dim >= 8
                && ref_names[1] == NONE_FRAME
                && interintra_type == 0
                && self.has_overlappable_candidates(mi_row, mi_col, bw, bh);
            if eligible {
                let (num_samples, raw_samples) =
                    self.find_num_warp_samples(mi_row, mi_col, bsize, ref_names[0], mvs[0]);
                // `is_scaled(RefFrame[0])` (spec's fourth `use_obmc` gate) is
                // not modelled — none of the corpus streams use reference
                // scaling, so it is always treated as false.
                let allow_warp = self.allow_warped_motion && !force_integer_mv && num_samples > 0;
                if allow_warp {
                    motion_mode = self
                        .dec
                        .read_symbol(&mut self.mode_cdfs.motion_mode[bsize.min(21)])
                        as u8;
                } else {
                    motion_mode = self
                        .dec
                        .read_symbol(&mut self.mode_cdfs.use_obmc[bsize.min(21)])
                        as u8;
                }
                if motion_mode == 2 {
                    let bw4 = bw as i32;
                    let bh4 = bh as i32;
                    warp_model = warp::derive_warp_model(
                        &raw_samples,
                        bw4,
                        bh4,
                        mvs[0],
                        mi_col as i32,
                        mi_row as i32,
                    );
                    if std::env::var("KINETIX_AV1_DBG_WARP").is_ok() {
                        eprintln!(
                            "DBG warp derive mi=({mi_col},{mi_row}) bw4={bw4} bh4={bh4} mv={:?} num_samples={num_samples} raw_len={} raw={:?} model_valid={} model={:?}",
                            mvs[0],
                            raw_samples.len(),
                            raw_samples,
                            warp_model.is_some(),
                            warp_model
                        );
                    }
                }
                if dbg_b0 {
                    eprintln!(
                        "DBG b0 motion_mode={motion_mode} warp_allowed={allow_warp} rng={}",
                        self.dec.raw_state().0
                    );
                }
            }
        }
        // A WARP block reads no interpolation-filter symbol (dav1d sets
        // `has_subpel_filter = 0`).
        let frame_filter = if motion_mode == 2 {
            INTERP_EIGHTTAP_REGULAR
        } else {
            frame_filter
        };

        // Per-block interpolation filter (§5.11.27). When switchable, one
        // symbol per axis is read (`dir` 0 = vertical, 1 = horizontal) if
        // `enable_dual_filter`, otherwise a single shared symbol. dav1d
        // (`Post-subpel_filter1`/`filter2`) reads two whenever the sequence
        // header enables dual filters — reading only one desynced the entropy
        // decoder from the first inter block onward.
        let comp = usize::from(ref_names[1] != NONE_FRAME);
        if dbg_b0 {
            eprintln!(
                "DBG b0 frame_filter={frame_filter} dual={}",
                self.enable_dual_filter
            );
        }
        let mut filters = [frame_filter; 2];
        if frame_filter == INTERP_SWITCHABLE {
            // dav1d: `if (has_subpel_filter) { read filter symbols } else {
            //   filter[0] = filter[1] = FILTER_8TAP_REGULAR; }` — a GLOBALMV
            // block over IDENTITY global motion has integer MVs, so dav1d
            // reads *no* filter symbols for it and forces REGULAR; skipping
            // this gate read symbols dav1d never produced and desynced the
            // tile on GLOBALMV-heavy streams.
            if has_subpel_filter {
                let dirs = if self.enable_dual_filter { 2 } else { 1 };
                for (dir, fout) in filters.iter_mut().enumerate().take(dirs) {
                    let base = ((dir & 1) * 2 + comp) * 4;
                    let left_t = if left_inter != 0
                        && (self.ref_left[mi_row][0] == ref_names[0]
                            || self.ref_left[mi_row][1] == ref_names[0])
                    {
                        self.filter_left[dir][mi_row] as usize
                    } else {
                        3
                    };
                    let above_t = if above_inter != 0
                        && (self.ref_above[mi_col][0] == ref_names[0]
                            || self.ref_above[mi_col][1] == ref_names[0])
                    {
                        self.filter_above[dir][mi_col] as usize
                    } else {
                        3
                    };
                    let add = if left_t == above_t {
                        left_t
                    } else if left_t == 3 {
                        above_t
                    } else if above_t == 3 {
                        left_t
                    } else {
                        3
                    };
                    let ctx = (base + add).min(15);
                    *fout = self.dec.read_symbol(&mut self.mode_cdfs.interp_filter[ctx]) as u8;
                    if dbg_b0 {
                        eprintln!(
                            "DBG b0 filter{dir}={} ctx={ctx} rng={}",
                            *fout,
                            self.dec.raw_state().0
                        );
                    }
                }
                if dirs == 1 {
                    filters[1] = filters[0];
                }
            } else {
                filters = [INTERP_EIGHTTAP_REGULAR; 2];
            }
        }
        // MC currently applies a single kernel to both axes; use the vertical
        // filter (a full dual-axis kernel split is a follow-up).
        let filter = filters;

        // Motion-compensated prediction into the output planes (Y then chroma),
        // using the reference slots mapped from the reference names.
        let px_x0 = mi_col * MI_SIZE - self.tile_px_x0;
        let px_y0 = mi_row * MI_SIZE - self.tile_px_y0;
        let bw_px = bw * MI_SIZE;
        let bh_px = bh * MI_SIZE;

        // Compound blend weight (§7.11.3.15): `jnt_weight` for the
        // distance-weighted type, `8` (plain average) otherwise.
        let blend_weight = if compound && block_comp_type == 1 {
            let rp = |n: u8| {
                self.dpb_order_hints
                    .get(self.ref_to_slot[n as usize] as usize)
                    .copied()
                    .unwrap_or(0) as i32
            };
            jnt_weight(
                self.order_hint_bits,
                self.cur_order_hint as i32,
                rp(ref_names[0]),
                rp(ref_names[1]),
            )
        } else {
            8
        };

        // Masked-compound descriptor passed to every plane: the luma-domain
        // mask is generated on the plane-0 call and sub-sampled for chroma.
        let mask_desc = MaskDesc {
            comp_type: block_comp_type,
            wedge_index,
            mask_sign,
            bsize,
        };

        // Y plane.
        self.inter_predict_plane(
            0,
            px_x0,
            px_y0,
            bw_px,
            bh_px,
            &ref_names,
            &mvs,
            filter,
            blend_weight,
            mask_desc,
            mi_row,
            mi_col,
            warp_model.as_ref(),
        )?;
        // Chroma planes — `inter_predict_plane` interprets the luma MV at
        // 1/16-pel for the subsampled axes.
        //
        // §7.11.3.4 sub-8x8 chroma (4:2:0): a leaf narrower or shorter than
        // 8 luma px shares the parent 8x8's chroma with its siblings, and
        // dav1d's scheme (recon_tmpl.c `is_sub8x8`) is:
        //   * every sub-8x8 leaf runs one MC per *quadrant* it can attribute
        //     a neighbour mv to — for an 8x4 leaf: the top half from the cell
        //     directly above (with the above cell's mv + filters), the bottom
        //     half from its own mv; a 4x8 leaf mirrors this with the left
        //     cell; a 4x4 leaf fills TL from the diagonal cell (whose filter
        //     is the running `tl_filter2d`), BL from the left cell, TR from
        //     the above cell and BR from its own mv — every call writes a
        //     2x2-chroma quadrant at the leaf's own chroma origin plus the
        //     offsets, so the writes overlap the sibling quadrants and the
        //     last leaf's writes win;
        //   * when a quadrant's attributing cell is NOT inter-coded
        //     (`is_sub8x8` gate fails), the leaf instead makes ONE mc over
        //     the whole parent 8x8 chroma (`bw4 << (bw4 == ss_hor)`,
        //     `t->bx & ~ss_hor`) with its own mv and filters.
        // The previous per-leaf-only scheme happened to match dav1d on the
        // 128x96 clip (coinciding mvs) and diverged on 96x64's mixed
        // intra/inter 8x4 splits.
        let cpx_x0 = px_x0 / 2;
        let cpx_y0 = px_y0 / 2;
        let cbw_px = bw_px >> self.subsampling_x as u32;
        let cbh_px = bh_px >> self.subsampling_y as u32;
        let is_420 = self.subsampling_x && self.subsampling_y;
        // dav1d `has_chroma`: only the odd-parity half of a sub-8x8 pair (or
        // the bottom-right 4x4 of a quad) owns the parent 8x8's chroma at all.
        let has_chroma = (bw > 1 || (mi_col & 1) == 1) && (bh > 1 || (mi_row & 1) == 1);
        let sub8x8_leaf = is_420 && (bw == 1 || bh == 1) && has_chroma;
        if sub8x8_leaf {
            // All sub-8x8 chroma MCs are based at the PARENT 8x8's chroma
            // origin (dav1d's `uvdstoff` floors `t->bx/by >> ss_hor/ver`).
            let base_x = ((mi_col & !1) * MI_SIZE - self.tile_px_x0) / 2;
            let base_y = ((mi_row & !1) * MI_SIZE - self.tile_px_y0) / 2;
            let mut gate = true;
            if bw == 1 {
                gate &= mi_col > 0
                    && self.refmv_cell(mi_row, mi_col - 1).refs[0] > crate::inter::INTRA_FRAME;
            }
            if bh == 1 {
                gate &= mi_row > 0
                    && self.refmv_cell(mi_row - 1, mi_col).refs[0] > crate::inter::INTRA_FRAME;
            }
            if bw == 1 && bh == 1 {
                gate &= mi_col > 0
                    && mi_row > 0
                    && self.refmv_cell(mi_row - 1, mi_col - 1).refs[0] > crate::inter::INTRA_FRAME;
            }
            if gate {
                // Quadrant MCs, in dav1d's order, each at the parent chroma
                // origin plus the running offsets.
                let mut h_off = 0usize;
                let mut v_off = 0usize;
                if bw == 1 && bh == 1 {
                    // TL quadrant from the diagonal cell; its filter is the
                    // running `tl_filter2d` (dav1d `t->tl_4x4_filter`).
                    let cell = self.refmv_cell(mi_row - 1, mi_col - 1);
                    let f = self.tl_filter2d.unwrap_or((0, 0));
                    for plane in 1..=2usize {
                        self.inter_predict_plane(
                            plane,
                            base_x,
                            base_y,
                            cbw_px,
                            cbh_px,
                            &ref_names,
                            &[cell.mv[0], Mv::default()],
                            [f.1, f.0],
                            blend_weight,
                            mask_desc,
                            mi_row,
                            mi_col,
                            None,
                        )?;
                    }
                    v_off = 2;
                    h_off = 2;
                }
                if bw == 1 {
                    // BL quadrant from the left cell (4x4) / the whole leaf
                    // (4x8), with the left cell's filters.
                    let cell = self.refmv_cell(mi_row, mi_col - 1);
                    let f = (self.filter_left[0][mi_row], self.filter_left[1][mi_row]);
                    for plane in 1..=2usize {
                        self.inter_predict_plane(
                            plane,
                            base_x,
                            base_y + v_off,
                            cbw_px,
                            cbh_px,
                            &ref_names,
                            &[cell.mv[0], Mv::default()],
                            [f.1, f.0],
                            blend_weight,
                            mask_desc,
                            mi_row,
                            mi_col,
                            None,
                        )?;
                    }
                    h_off = 2;
                }
                if bh == 1 {
                    // TR quadrant from the above cell with the above cell's
                    // filters.
                    let cell = self.refmv_cell(mi_row - 1, mi_col);
                    let f = (self.filter_above[0][mi_col], self.filter_above[1][mi_col]);
                    for plane in 1..=2usize {
                        self.inter_predict_plane(
                            plane,
                            base_x + h_off,
                            base_y,
                            cbw_px,
                            cbh_px,
                            &ref_names,
                            &[cell.mv[0], Mv::default()],
                            [f.1, f.0],
                            blend_weight,
                            mask_desc,
                            mi_row,
                            mi_col,
                            None,
                        )?;
                    }
                    v_off = 2;
                }
                // BR quadrant (or the sibling half) with the leaf's own mv
                // and filters.
                for plane in 1..=2usize {
                    self.inter_predict_plane(
                        plane,
                        base_x + h_off,
                        base_y + v_off,
                        cbw_px,
                        cbh_px,
                        &ref_names,
                        &mvs,
                        filter,
                        blend_weight,
                        mask_desc,
                        mi_row,
                        mi_col,
                        None,
                    )?;
                }
            } else {
                // Normal path: one mc over the WHOLE parent 8x8's chroma
                // (dav1d `bw4 << (bw4 == ss_hor)`, `t->bx & ~ss_hor`) with
                // this leaf's own mv and filters.
                let pw = cbw_px << (bw == 1) as u32;
                let ph = cbh_px << (bh == 1) as u32;
                for plane in 1..=2usize {
                    self.inter_predict_plane(
                        plane,
                        base_x,
                        base_y,
                        pw,
                        ph,
                        &ref_names,
                        &mvs,
                        filter,
                        blend_weight,
                        mask_desc,
                        mi_row,
                        mi_col,
                        None,
                    )?;
                }
            }
            // dav1d `skip_inter_chroma_pred: t->tl_4x4_filter = filter_2d`.
            self.tl_filter2d = Some((filter[0], filter[1]));
        } else {
            self.inter_predict_plane(
                1,
                cpx_x0,
                cpx_y0,
                cbw_px,
                cbh_px,
                &ref_names,
                &mvs,
                filter,
                blend_weight,
                mask_desc,
                mi_row,
                mi_col,
                warp_model.as_ref(),
            )?;
            self.inter_predict_plane(
                2,
                cpx_x0,
                cpx_y0,
                cbw_px,
                cbh_px,
                &ref_names,
                &mvs,
                filter,
                blend_weight,
                mask_desc,
                mi_row,
                mi_col,
                warp_model.as_ref(),
            )?;
        }

        // Overlapped motion compensation (§7.11.3.9) — blend the base
        // prediction with predictions from the above / left neighbours'
        // motion vectors. Only meaningful for `motion_mode == OBMC` (1);
        // WARP (2) blocks never run OBMC (dav1d: `motion_mode == MM_OBMC`
        // is mutually exclusive with `MM_WARP` at the syntax level).

        // Debug: dump base MC prediction BEFORE OBMC for blocks in error region.
        if std::env::var("KINETIX_AV1_DBG_PRED").is_ok() {
            let px_end_y = px_y0 + bh_px;
            let px_end_x = px_x0 + bw_px;
            if px_end_y > 56 && px_y0 < 96 && px_x0 < 128
                || std::env::var("KINETIX_AV1_DBG_PRED_ALL").is_ok()
            {
                eprintln!(
                    "PRED-BASE mi=({mi_col},{mi_row}) bw={bw} bh={bh} skip={skip} ref={} dir0_v={} dir1_h={} mv=({},{}) px=({px_x0},{px_y0})",
                    ref_names[0], filter[0], filter[1], mvs[0].col, mvs[0].row
                );
                for row in px_y0..px_end_y.min(96) {
                    if row < 56 && std::env::var("KINETIX_AV1_DBG_PRED_ALL").is_err() {
                        continue;
                    }
                    let vals: Vec<u8> = (px_x0..px_end_x.min(128))
                        .map(|c| self.y_plane[row * self.y_stride + c])
                        .collect();
                    eprintln!("  y={row}: {vals:?}");
                }
                // For the specific divergent block mi(4,18), also dump reference
                // frame pixels to diagnose whether the error is in our reference frame
                // or in the filter computation itself.
                if mi_col == 4 && mi_row == 18 {
                    let slot0 = self.ref_to_slot[ref_names[0] as usize] as usize;
                    if let Some(rf) = self.ref_slots.slots[slot0] {
                        let (rp, rw, rh) = rf.plane(0);
                        let ix = mvs[0].col >> 3;
                        let iy = mvs[0].row >> 3;
                        let base_x = px_x0 as i32 + ix;
                        let base_y = px_y0 as i32 + iy;
                        eprintln!("  REF-PIXELS base=({base_x},{base_y}) rw={rw} rh={rh}:");
                        for ty in 0..(bh_px + 7) {
                            let ry = (base_y + ty as i32 - 3).clamp(0, rh as i32 - 1) as usize;
                            let vals: Vec<u8> = (0..bw_px)
                                .map(|x| {
                                    let rx = (base_x + x as i32).clamp(0, rw as i32 - 1) as usize;
                                    rp[ry * rw + rx]
                                })
                                .collect();
                            eprintln!("    ref_row={ry}: {vals:?}");
                        }
                    }
                }
            }
        }

        if std::env::var("KINETIX_AV1_DBG_TAPBLK").is_ok() {
            let px_end_x = px_x0 + bw_px;
            let px_end_y = px_y0 + bh_px;
            if px_x0 < 138 && px_end_x > 128 && px_y0 < 96 && px_end_y > 82 {
                let slot0 = self.ref_to_slot[ref_names[0] as usize] as usize;
                let ref_oh0 = self.dpb_order_hints.get(slot0).copied().unwrap_or(255);
                eprintln!(
                    "TAPBLK seq={} mi=({mi_col},{mi_row}) bsize={bsize} px=({px_x0},{px_y0}) bw={bw_px} bh={bh_px} mm={motion_mode} ii={interintra_type} skip={skip} ref={:?} ref_slot0={slot0} ref_oh0={ref_oh0} mv0={:?} ref_to_slot={:?} dpb_oh={:?}",
                    crate::debug_frame_seq::current(),
                    ref_names,
                    mvs[0],
                    self.ref_to_slot,
                    self.dpb_order_hints,
                );
            }
        }
        if motion_mode == 1 && std::env::var("KINETIX_AV1_NOOBMC").is_err() {
            for plane in 0..3 {
                self.apply_obmc(mi_row, mi_col, bsize, plane);
            }
        }

        // Inter-intra (§7.11.3.6): blend an intra prediction built from the
        // reconstructed block edges with the inter prediction, weighted by the
        // (sign-0) wedge mask. **Luma only** — dav1d applies `II_MASK(0, ..)`
        // on plane 0's `dst`/stride alone and never touches planes 1-2; the
        // previous all-plane loop blended intra prediction into U/V, corrupting
        // chroma on every inter-intra block (visible as ±1-13 sample chroma
        // diffs on the 128x96 inter clip's inter-intra blocks).
        if interintra_type != 0 {
            self.apply_interintra(mi_row, mi_col, bsize, interintra_mode, ii_wedge_index);
        }

        // Debug: dump pre-residual prediction (post-OBMC) for error-region blocks.
        if std::env::var("KINETIX_AV1_DBG_PRED").is_ok() {
            let px_end_y = px_y0 + bh_px;
            let px_end_x = px_x0 + bw_px;
            if px_end_y > 56 && px_y0 < 96 && px_x0 < 128
                || std::env::var("KINETIX_AV1_DBG_PRED_ALL").is_ok()
            {
                eprintln!(
                    "PRED mi=({mi_col},{mi_row}) bw={bw} bh={bh} mm={motion_mode} skip={skip} ref={} dir0_v={} dir1_h={} mv=({},{}) px=({px_x0},{px_y0})",
                    ref_names[0], filter[0], filter[1], mvs[0].col, mvs[0].row
                );
                for row in px_y0..px_end_y.min(96) {
                    if row < 56 && std::env::var("KINETIX_AV1_DBG_PRED_ALL").is_err() {
                        continue;
                    }
                    let vals: Vec<u8> = (px_x0..px_end_x.min(128))
                        .map(|c| self.y_plane[row * self.y_stride + c])
                        .collect();
                    eprintln!("  y={row}: {vals:?}");
                }
            }
        }
        // Residual. `read_block_tx_size` (§5.11.16) takes its inter/IBC branch
        // here (`IsInter == 1`): a recursive var-tx-tree of `txfm_split`
        // symbols (`read_block_tx_size_ibc`/`read_tx_tree`), NOT the
        // single-ternary `read_tx_size` used for real intra blocks — the wrong
        // syntax model desynced the entropy decoder from the first non-skip
        // inter block (dav1d `Post-vartxtree`). It also updates the shared
        // `tx_above`/`tx_left` neighbour context internally.
        let leaves = self.read_block_tx_size_ibc(mi_row, mi_col, bsize, skip);
        let luma_tx = leaves.first().map(|l| l.2).unwrap_or(TX_4X4);
        if dbg_b0 {
            eprintln!(
                "DBG b0 vartx leaves={} tx0={luma_tx} rng={} all={:?}",
                leaves.len(),
                self.dec.raw_state().0,
                leaves,
            );
        }
        // Capture pre-residual snapshot for the specific failing block to compare
        // prediction vs final output at the row-specific error positions.
        let pred_snap: Vec<u8> = if std::env::var("KINETIX_AV1_DBG_PRED").is_ok()
            && mi_col == 4
            && mi_row == 18
            && !skip
        {
            let stride = self.y_stride;
            let mut snap = Vec::with_capacity(bw_px * bh_px);
            for y in px_y0..px_y0 + bh_px {
                for x in px_x0..px_x0 + bw_px {
                    snap.push(self.y_plane[y * stride + x]);
                }
            }
            snap
        } else {
            Vec::new()
        };
        self.add_inter_residual(mi_row, mi_col, bsize, skip, &leaves)?;
        // Debug: show how residual changed prediction at block mi(4,18).
        if !pred_snap.is_empty() {
            eprintln!(
                "RESID mi=(4,18) leaves={} tx0={}",
                leaves.len(),
                leaves.first().map(|l| l.2).unwrap_or(0)
            );
            for row in 0..bh_px {
                let y = px_y0 + row;
                let deltas: Vec<i32> = (0..bw_px)
                    .map(|col| {
                        let x = px_x0 + col;
                        self.y_plane[y * self.y_stride + x] as i32
                            - pred_snap[row * bw_px + col] as i32
                    })
                    .collect();
                let post: Vec<u8> = (0..bw_px)
                    .map(|col| {
                        let x = px_x0 + col;
                        self.y_plane[y * self.y_stride + x]
                    })
                    .collect();
                eprintln!(
                    "  y={y} pred={:?}",
                    &pred_snap[row * bw_px..(row + 1) * bw_px]
                );
                eprintln!("       post={post:?}  delta={deltas:?}");
            }
        }
        // §7.14.4 deblock-level inputs: this block's primary reference (spec
        // delta index = name − 1; INTRA_FRAME=1 maps to 0) and mode type.
        // The span is the block's tile-local luma rectangle, mirroring
        // `record_delta_lf4`'s addressing.
        if std::env::var("KINETIX_AV1_DBG_LFREF").is_ok() {
            eprintln!(
                "LFREF n={} mi=({mi_col},{mi_row}) px=({px_x0},{px_y0}) bw={bw_px} bh={bh_px} ref_names={ref_names:?} lf_mode_type={lf_mode_type} recorded_ref={}",
                crate::debug_frame_seq::current(),
                ref_names[0] - 1
            );
        }
        self.meta.record_lf4(
            px_x0 / 4,
            px_y0 / 4,
            (px_x0 + bw_px).div_ceil(4),
            (px_y0 + bh_px).div_ceil(4),
            ref_names[0] - 1,
            lf_mode_type,
        );
        // §7.14.4: the block's own final chroma levels — the chroma level
        // cache is last-decoded-block-wins (dav1d `f->lf.level` chroma
        // writes), so a chroma cell shared with an earlier block of a
        // different kind keeps THIS block's level.
        let (lu, lv) = crate::reconstruct::chroma_lf_levels_snapshot(
            self.lf_frame_levels,
            self.lf_ref_deltas,
            self.lf_mode_deltas,
            self.lf_delta_enabled,
            self.delta_lf,
            ref_names[0] - 1,
            lf_mode_type,
        );
        self.meta.record_lf_level_chroma(
            px_x0 / 8,
            px_y0 / 8,
            (px_x0 + bw_px).div_ceil(8),
            (px_y0 + bh_px).div_ceil(8),
            lu as u8,
            lv as u8,
        );
        if dbg_b0 {
            eprintln!("DBG b0 post-residual rng={}", self.dec.raw_state().0);
        }

        // Update inter neighbour state.
        let skip_byte = skip as u8;
        let luma_tx_w_byte = av1::TX_WIDTH[luma_tx] as u8;
        let luma_tx_h_byte = av1::TX_HEIGHT[luma_tx] as u8;
        // dav1d `BlockContext::comp_type` (from `read_compound_type`).
        let comp_type_byte = block_comp_type;
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(s) = self.is_inter_left.get_mut(r) {
                *s = 1;
            }
            if let Some(s) = self.comp_type_left.get_mut(r) {
                *s = comp_type_byte;
            }
            if let Some(slot) = self.ref_left.get_mut(r) {
                slot[0] = ref_names[0];
                slot[1] = ref_names[1];
            }
            if let Some(slot) = self.mv_left.get_mut(r) {
                slot[0] = mvs[0];
                slot[1] = mvs[1];
            }
            // Inter-coded blocks leave no intra mode / tx / skip context for
            // neighbours; AV1 treats their intra-mode neighbour as DC_PRED (0).
            if let Some(s) = self.ymode_left.get_mut(r) {
                *s = DC_PRED;
            }
            if let Some(s) = self.skip_left.get_mut(r) {
                *s = skip_byte;
            }
            if let Some(s) = self.skip_mode_left.get_mut(r) {
                *s = 0;
            }
            if let Some(s) = self.tx_left.get_mut(r) {
                *s = luma_tx_h_byte;
            }
            for (fv, arr) in filters.iter().zip(self.filter_left.iter_mut()) {
                if let Some(s) = arr.get_mut(r) {
                    *s = *fv;
                }
            }
        }
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(s) = self.is_inter_above.get_mut(c) {
                *s = 1;
            }
            if let Some(s) = self.comp_type_above.get_mut(c) {
                *s = comp_type_byte;
            }
            if let Some(slot) = self.ref_above.get_mut(c) {
                slot[0] = ref_names[0];
                slot[1] = ref_names[1];
            }
            if let Some(slot) = self.mv_above.get_mut(c) {
                slot[0] = mvs[0];
                slot[1] = mvs[1];
            }
            if let Some(s) = self.ymode_above.get_mut(c) {
                *s = DC_PRED;
            }
            if let Some(s) = self.skip_above.get_mut(c) {
                *s = skip_byte;
            }
            if let Some(s) = self.skip_mode_above.get_mut(c) {
                *s = 0;
            }
            if let Some(s) = self.tx_above.get_mut(c) {
                *s = luma_tx_w_byte;
            }
            for (fv, arr) in filters.iter().zip(self.filter_above.iter_mut()) {
                if let Some(s) = arr.get_mut(c) {
                    *s = *fv;
                }
            }
        }
        // 2-D ref-MV grid: record this block's refs + MVs for later stacks.
        //
        // §7.10.4.2 `add_sample` excludes any neighbour cell with
        // `RefFrames[mvRow][mvCol][1] != NONE` outright (return before even
        // counting it as scanned) — that's how the spec keeps inter-intra
        // blocks out of `find_warp_samples`'s matching-ref scan even though
        // they're single-reference. dav1d's own grid-splat implements this by
        // storing `ref[1] = INTRA_FRAME` (not `NONE`) for such a block
        // (`splat_oneref_mv`: `.ref.ref = { ref0+1, interintra_type ? 0 : -1 }`,
        // where dav1d's `0` sentinel is `INTRA_FRAME` in its own numbering).
        // Kinetix's own `ref_names` (used for ref_above/ref_left context and
        // MC) correctly keeps `NONE_FRAME` here — only the warp-samples grid
        // cell needs the inter-intra marker, so it's applied to a separate
        // `grid_refs` rather than `ref_names` itself.
        let mut grid_refs = ref_names;
        if interintra_type != 0 {
            debug_assert_eq!(grid_refs[1], NONE_FRAME);
            grid_refs[1] = crate::inter::INTRA_FRAME;
        }
        self.splat_refmv_full(mi_row, mi_col, bsize, grid_refs, mvs, new_mf);
        Ok(())
    }

    /// dav1d `get_gmv_2d` (env.h): the block motion vector for a GLOBALMV-
    /// coded block, derived from the frame's global motion model for
    /// `ref_name` (§7.11.3). TRANSLATION yields the signaled translation
    /// directly (the parser already scaled it to MV units at
    /// `<< (GM_TRANS_ONLY_PREC_BITS + !allow_high_precision_mv)`); ROTZOOM/
    /// AFFINE evaluate the full model at the block centre. IDENTITY is the
    /// zero MV. `force_integer_mv` rounds the result to whole pixels.
    ///
    /// Note: for ROTZOOM/AFFINE dav1d does not use this MV for prediction at
    /// all — it warps with the full model (`gmv_warp_allowed`). This centre MV
    /// is only the fallback there; no corpus clip exercises global rotation
    /// yet.
    fn get_gmv_2d(&self, ref_name: u8, mi_col: usize, mi_row: usize, bw: usize, bh: usize) -> Mv {
        let idx = ref_name as usize - 1;
        if std::env::var("KINETIX_AV1_DBG_GMV").is_ok() {
            eprintln!(
                "GMV n={} ref={} type={} mat={:?} mi=({mi_col},{mi_row}) bw={bw} bh={bh}",
                crate::debug_frame_seq::current(),
                ref_name,
                self.gm_type[idx],
                &self.gm_params[idx][..]
            );
        }
        let mat = &self.gm_params[idx];
        match self.gm_type[idx] {
            crate::frame::GM_IDENTITY => Mv::default(),
            crate::frame::GM_TRANSLATION => {
                let mut res = Mv::new(mat[0] >> 13, mat[1] >> 13);
                if self.force_integer_mv {
                    fix_int_mv_precision(&mut res);
                }
                res
            }
            _ => {
                let x = (mi_col * MI_SIZE + bw * MI_SIZE / 2 - 1) as i32;
                let y = (mi_row * MI_SIZE + bh * MI_SIZE / 2 - 1) as i32;
                let xc = (mat[2] - (1 << 16)) * x + mat[3] * y + mat[0];
                let yc = (mat[5] - (1 << 16)) * y + mat[4] * x + mat[1];
                let shift = 16 - (3 - i32::from(!self.allow_high_precision_mv));
                let round = (1 << shift) >> 1;
                let mut res = Mv::new(
                    apply_sign(
                        ((yc.abs() + round) >> shift) << i32::from(!self.allow_high_precision_mv),
                        yc,
                    ),
                    apply_sign(
                        ((xc.abs() + round) >> shift) << i32::from(!self.allow_high_precision_mv),
                        xc,
                    ),
                );
                if self.force_integer_mv {
                    fix_int_mv_precision(&mut res);
                }
                res
            }
        }
    }

    /// A `skip_mode` block (§7.11.3 / §5.11.11): compound prediction from the
    /// fixed `SkipModeFrame` pair with the NEAREST spatial MVs, no residual, no
    /// further entropy reads. The NEAREST MV derivation is the simplified
    /// spatial-only `build_mv_candidates` (a full compound `find_mv_stack` is
    /// still pending), so the prediction is close but not yet bit-exact.
    fn decode_skip_mode_block(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
    ) -> Result<(), KinetixError> {
        let bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh = BLOCK_HEIGHT[bsize] / MI_SIZE;

        // §5.11.18: cdef / delta_q / delta_lf still follow, with skip = 1.
        self.read_cdef(mi_row, mi_col, bsize, true);
        self.read_delta_qindex(bsize, true);
        self.read_delta_lf(bsize, true);
        self.read_deltas = false;

        let ref_names = self.skip_mode_frame;
        // dav1d (`skip_mode` branch): the block's MVs are the NEARESTMV
        // candidates of the MV stack built for the SkipModeFrame reference
        // pair — i.e. a full `dav1d_refmvs_find` run, stack entry 0. (Zeroing
        // or using the abridged neighbour predictor poisons the refmv grid
        // for later OBMC/warp-sample scans.)
        let (stack, _, _, _, _) = self.inter_mv_stack(mi_row, mi_col, bsize, ref_names);
        let mv0 = stack.first().map(|s| s[0]).unwrap_or_default();
        let mv1 = stack.first().map(|s| s[1]).unwrap_or_default();
        let mvs = [mv0, mv1];

        let px_x0 = mi_col * MI_SIZE - self.tile_px_x0;
        let px_y0 = mi_row * MI_SIZE - self.tile_px_y0;
        let bw_px = bw * MI_SIZE;
        let bh_px = bh * MI_SIZE;
        if std::env::var("KINETIX_AV1_DBG_TAPBLK").is_ok() {
            let px_end_x = px_x0 + bw_px;
            let px_end_y = px_y0 + bh_px;
            if px_x0 < 138 && px_end_x > 128 && px_y0 < 96 && px_end_y > 82 {
                eprintln!(
                    "TAPBLK-SKIPMODE seq={} mi=({mi_col},{mi_row}) bsize={bsize} px=({px_x0},{px_y0}) bw={bw_px} bh={bh_px} ref={:?} mv0={:?}",
                    crate::debug_frame_seq::current(),
                    ref_names,
                    mvs[0],
                );
            }
        }
        let f = if self.interpolation_filter == INTERP_SWITCHABLE {
            0
        } else {
            self.interpolation_filter
        };
        // Skip-mode blocks are `COMP_INTER_AVG` (plain average, weight 8) and
        // always compound (two references), so `motion_mode`/WARP never
        // applies here (WARP requires single-ref) — `warp_model` is always
        // `None`.
        let nm = MaskDesc::none();
        self.inter_predict_plane(
            0,
            px_x0,
            px_y0,
            bw_px,
            bh_px,
            &ref_names,
            &mvs,
            [f, f],
            8,
            nm,
            mi_row,
            mi_col,
            None,
        )?;
        let (cpx_x0, cpx_y0) = (px_x0 / 2, px_y0 / 2);
        let (cbw, cbh) = ((bw_px / 2).max(4), (bh_px / 2).max(4));
        self.inter_predict_plane(
            1,
            cpx_x0,
            cpx_y0,
            cbw,
            cbh,
            &ref_names,
            &mvs,
            [f, f],
            8,
            nm,
            mi_row,
            mi_col,
            None,
        )?;
        self.inter_predict_plane(
            2,
            cpx_x0,
            cpx_y0,
            cbw,
            cbh,
            &ref_names,
            &mvs,
            [f, f],
            8,
            nm,
            mi_row,
            mi_col,
            None,
        )?;

        // Skip-mode blocks are always `skip = 1`: `read_block_tx_size` takes
        // its no-entropy-read branch (uniform max transform).
        let leaves = self.read_block_tx_size_ibc(mi_row, mi_col, bsize, true);
        let luma_tx = leaves.first().map(|l| l.2).unwrap_or(TX_4X4);
        self.add_inter_residual(mi_row, mi_col, bsize, true, &leaves)?;
        // §7.14.4 deblock-level inputs (see the identical call in the ordinary
        // inter-block path above): a skip-mode block never went through that
        // path, so without this call `lf_ref4`/`lf_mode4` kept whatever was
        // left over for this cell (0 == INTRA_FRAME by `FrameMeta`'s default),
        // making every edge touching a skip-mode block derive its filter
        // level from `loop_filter_ref_deltas[INTRA_FRAME]` instead of the
        // block's real `SkipModeFrame` reference. Skip-mode always predicts
        // from the NEAREST-MV stack entry (never GLOBALMV), so `modeType` is
        // unconditionally 1 here, matching the `comp_mode != GLOBALMV_GLOBALMV`
        // derivation used for ordinary compound blocks above.
        if std::env::var("KINETIX_AV1_DBG_LFREF").is_ok() {
            eprintln!(
                "LFREF-SKIPMODE n={} mi=({mi_col},{mi_row}) px=({px_x0},{px_y0}) bw={bw_px} bh={bh_px} ref_names={ref_names:?} mvs=({},{}),({},{})",
                crate::debug_frame_seq::current(),
                mvs[0].row,
                mvs[0].col,
                mvs[1].row,
                mvs[1].col,
            );
        }
        let (lu, lv) = crate::reconstruct::chroma_lf_levels_snapshot(
            self.lf_frame_levels,
            self.lf_ref_deltas,
            self.lf_mode_deltas,
            self.lf_delta_enabled,
            self.delta_lf,
            ref_names[0] - 1,
            1,
        );
        self.meta.record_lf_level_chroma(
            px_x0 / 8,
            px_y0 / 8,
            (px_x0 + bw_px).div_ceil(8),
            (px_y0 + bh_px).div_ceil(8),
            lu as u8,
            lv as u8,
        );
        // §7.14.1: skip-mode blocks are real inter block boundaries — mark the
        // chroma deblock edge even though they have no chroma coefficients.
        // The skip-mode path never enters the chroma-tx loop (which is the
        // only other mark_chroma_edges call site for inter blocks), so without
        // this call the edge flag stays false and the horizontal deblock filter
        // skips the row boundary, diverging from dav1d.
        self.meta.mark_chroma_edges(
            px_x0 / 8,
            px_y0 / 8,
            (px_x0 + bw_px).div_ceil(8),
            (px_y0 + bh_px).div_ceil(8),
        );
        self.meta.record_lf4(
            px_x0 / 4,
            px_y0 / 4,
            (px_x0 + bw_px).div_ceil(4),
            (px_y0 + bh_px).div_ceil(4),
            ref_names[0] - 1,
            1,
        );
        // §7.14.4: skip-mode blocks read DeltaLF just like the ordinary path
        // (`read_delta_lf` above); record the running values for this span
        // so edge levels resolved through these cells use them.
        self.meta.record_delta_lf(
            px_x0 / 8,
            px_y0 / 8,
            (px_x0 + bw_px).div_ceil(8),
            (px_y0 + bh_px).div_ceil(8),
            self.delta_lf,
        );
        self.meta.record_delta_lf4(
            px_x0 / 4,
            px_y0 / 4,
            (px_x0 + bw_px).div_ceil(4),
            (px_y0 + bh_px).div_ceil(4),
            self.delta_lf,
        );

        let luma_tx_w = av1::TX_WIDTH[luma_tx] as u8;
        let luma_tx_h = av1::TX_HEIGHT[luma_tx] as u8;
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(s) = self.is_inter_left.get_mut(r) {
                *s = 1;
            }
            if let Some(s) = self.comp_type_left.get_mut(r) {
                *s = 2; // skip-mode blocks are COMP_INTER_AVG
            }
            if let Some(slot) = self.ref_left.get_mut(r) {
                *slot = ref_names;
            }
            if let Some(slot) = self.mv_left.get_mut(r) {
                *slot = mvs;
            }
            if let Some(s) = self.ymode_left.get_mut(r) {
                *s = DC_PRED;
            }
            if let Some(s) = self.skip_left.get_mut(r) {
                *s = 1;
            }
            if let Some(s) = self.skip_mode_left.get_mut(r) {
                *s = 1;
            }
            if let Some(s) = self.tx_left.get_mut(r) {
                *s = luma_tx_h;
            }
            for arr in self.filter_left.iter_mut() {
                if let Some(s) = arr.get_mut(r) {
                    *s = f;
                }
            }
        }
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(s) = self.is_inter_above.get_mut(c) {
                *s = 1;
            }
            if let Some(s) = self.comp_type_above.get_mut(c) {
                *s = 2; // skip-mode blocks are COMP_INTER_AVG
            }
            if let Some(slot) = self.ref_above.get_mut(c) {
                *slot = ref_names;
            }
            if let Some(slot) = self.mv_above.get_mut(c) {
                *slot = mvs;
            }
            if let Some(s) = self.ymode_above.get_mut(c) {
                *s = DC_PRED;
            }
            if let Some(s) = self.skip_above.get_mut(c) {
                *s = 1;
            }
            if let Some(s) = self.skip_mode_above.get_mut(c) {
                *s = 1;
            }
            if let Some(s) = self.tx_above.get_mut(c) {
                *s = luma_tx_w;
            }
            for arr in self.filter_above.iter_mut() {
                if let Some(s) = arr.get_mut(c) {
                    *s = f;
                }
            }
        }
        // Populate the 2-D reference-MV grid so a later block's compound
        // `find_mv_stack` can see this skip-mode block's ref pair + MVs.
        self.splat_refmv_full(mi_row, mi_col, bsize, ref_names, mvs, 0);
        Ok(())
    }

    /// Overlapped motion compensation for one plane (§7.11.3.9 / §7.11.3.10).
    /// Blends the base prediction already in the plane with predictions formed
    /// from the above and left neighbours' motion vectors, using the raised-cosine
    /// `Obmc_Mask_*` weights (decaying away from the shared edge).
    fn apply_obmc(&mut self, mi_row: usize, mi_col: usize, bsize: usize, plane: usize) {
        let (subx, suby) = if plane == 0 {
            (0usize, 0usize)
        } else {
            (self.subsampling_x as usize, self.subsampling_y as usize)
        };
        let bw4 = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh4 = BLOCK_HEIGHT[bsize] / MI_SIZE;
        let w = (bw4 * MI_SIZE) >> subx;
        let h = (bh4 * MI_SIZE) >> suby;
        let row_start = self.tile_px_y0 / MI_SIZE;
        let col_start = self.tile_px_x0 / MI_SIZE;
        let avail_u = mi_row > row_start;
        let avail_l = mi_col > col_start;

        let (pstride, pw, ph) = match plane {
            1 | 2 => (self.uv_stride, self.tile_cw, self.tile_ch),
            _ => (self.y_stride, self.tile_w, self.tile_h),
        };
        let (hbits, vbits) = if plane == 0 {
            (3u32, 3u32)
        } else {
            (3 + self.subsampling_x as u32, 3 + self.subsampling_y as u32)
        };
        let grid_w4 = |r: usize, c: usize| -> usize {
            self.refmv_grid
                .get(r * self.refmv_stride + c)
                .map(|cell| cell.w4 as usize)
                .unwrap_or(0)
        };
        let grid_h4 = |r: usize, c: usize| -> usize {
            self.refmv_grid
                .get(r * self.refmv_stride + c)
                .map(|cell| cell.h4 as usize)
                .unwrap_or(0)
        };

        // Collected overlap jobs, gathered first so the mutable plane borrow is
        // taken only for the blend.
        struct ObmcJob {
            pass: u8,
            px: usize,
            py: usize,
            pred_w: usize,
            pred_h: usize,
            mv: Mv,
            // The neighbour's per-direction filters, raw read order:
            // [dir0 (vertical), dir1 (horizontal)] — swap when passing to
            // `motion_compensate`.
            filters: [u8; 2],
            nb_ref: u8,
        }
        let mut jobs: Vec<ObmcJob> = Vec::new();

        if avail_u && SUBSAMPLED_SIZE[bsize][subx][suby] >= BLOCK_8X8 {
            let n_limit = 4.min((bw4 as u32).trailing_zeros() as usize);
            let mut x4 = mi_col;
            let mut n_count = 0;
            let x_end = self.mi_cols.min(mi_col + bw4);
            while n_count < n_limit && x4 < x_end {
                let cand_row = mi_row - 1;
                let cand_col = x4 | 1;
                let step4 = grid_w4(cand_row, cand_col).clamp(2, 16);
                let nb_ref = self.ref_above.get(cand_col).map(|r| r[0]).unwrap_or(0);
                if nb_ref > crate::inter::INTRA_FRAME {
                    n_count += 1;
                    let pred_w = w.min((step4 * MI_SIZE) >> subx);
                    let pred_h = (h >> 1).min(32 >> suby);
                    let px = ((x4 * MI_SIZE) as isize - self.tile_px_x0 as isize) >> subx;
                    let py = ((mi_row * MI_SIZE) as isize - self.tile_px_y0 as isize) >> suby;
                    if px >= 0 && py >= 0 && pred_w > 0 && pred_h > 0 {
                        jobs.push(ObmcJob {
                            pass: 0,
                            px: px as usize,
                            py: py as usize,
                            pred_w,
                            pred_h,
                            mv: self.mv_above[cand_col][0],
                            filters: [
                                self.filter_above[0].get(cand_col).copied().unwrap_or(0),
                                self.filter_above[1].get(cand_col).copied().unwrap_or(0),
                            ],
                            nb_ref,
                        });
                    }
                }
                x4 += step4;
            }
        }
        if avail_l {
            let n_limit = 4.min((bh4 as u32).trailing_zeros() as usize);
            let mut y4 = mi_row;
            let mut n_count = 0;
            let y_end = self.mi_rows.min(mi_row + bh4);
            while n_count < n_limit && y4 < y_end {
                let cand_row = y4 | 1;
                let cand_col = mi_col - 1;
                let step4 = grid_h4(cand_row, cand_col).clamp(2, 16);
                let nb_ref = self.ref_left.get(cand_row).map(|r| r[0]).unwrap_or(0);
                if nb_ref > crate::inter::INTRA_FRAME {
                    n_count += 1;
                    let pred_w = (w >> 1).min(32 >> subx);
                    let pred_h = h.min((step4 * MI_SIZE) >> suby);
                    let px = ((mi_col * MI_SIZE) as isize - self.tile_px_x0 as isize) >> subx;
                    let py = ((y4 * MI_SIZE) as isize - self.tile_px_y0 as isize) >> suby;
                    if px >= 0 && py >= 0 && pred_w > 0 && pred_h > 0 {
                        jobs.push(ObmcJob {
                            pass: 1,
                            px: px as usize,
                            py: py as usize,
                            pred_w,
                            pred_h,
                            mv: self.mv_left[cand_row][0],
                            filters: [
                                self.filter_left[0].get(cand_row).copied().unwrap_or(0),
                                self.filter_left[1].get(cand_row).copied().unwrap_or(0),
                            ],
                            nb_ref,
                        });
                    }
                }
                y4 += step4;
            }
        }

        let dbg_obmc = std::env::var("KINETIX_AV1_DBG_OBMC").is_ok()
            && plane == 1
            && (16..=22).contains(&mi_row);
        // Detailed per-sample trace for the specific divergent block.
        let dbg_obmc_deep = dbg_obmc && mi_col == 4 && mi_row == 20;
        if dbg_obmc {
            eprintln!(
                "OBMC mi=({mi_col},{mi_row}) bsize={bsize} jobs={}",
                jobs.len()
            );
            for j in &jobs {
                eprintln!(
                    "  job pass={} px={} py={} w={} h={} nb_ref={} dir0_v={} dir1_h={} mv=({},{})",
                    j.pass,
                    j.px,
                    j.py,
                    j.pred_w,
                    j.pred_h,
                    j.nb_ref,
                    j.filters[0],
                    j.filters[1],
                    j.mv.col,
                    j.mv.row
                );
            }
        }
        for job in jobs {
            let ObmcJob {
                pass,
                px,
                py,
                pred_w,
                pred_h,
                mv,
                filters,
                nb_ref,
            } = job;
            let slot = self.ref_to_slot[nb_ref as usize] as usize;
            let Some(rf) = self.ref_slots.slots[slot] else {
                if dbg_obmc {
                    eprintln!("  SKIP job: no ref slot for nb_ref={nb_ref}");
                }
                continue;
            };
            let (rp, rw, rh) = rf.plane(plane);
            let mut obmc = vec![0u8; pred_w * pred_h];
            // `filters` here is `[dir0, dir1]` (see the neighbour-job
            // construction above); dir1 is horizontal, dir0 is vertical.
            motion_compensate(
                &mut obmc, pred_w, rp, rw, rw, rh, px, py, pred_w, pred_h, mv, filters[1],
                filters[0], hbits, vbits,
            );
            let mask = obmc_mask(if pass == 0 { pred_h } else { pred_w });
            let dst = match plane {
                1 => &mut self.u_plane,
                2 => &mut self.v_plane,
                _ => &mut self.y_plane,
            };
            let mut any_diff = false;
            let mut before: Vec<u8> = Vec::new();
            for i in 0..pred_h {
                let sy = py + i;
                if sy >= ph {
                    break;
                }
                for j in 0..pred_w {
                    let sx = px + j;
                    if sx >= pw {
                        break;
                    }
                    let m = if pass == 0 { mask[i] } else { mask[j] };
                    let cur = dst[sy * pstride + sx] as i32;
                    let o = obmc[i * pred_w + j] as i32;
                    if dbg_obmc && cur != o {
                        any_diff = true;
                    }
                    if dbg_obmc_deep {
                        before.push(dst[sy * pstride + sx]);
                    }
                    // §7.11.3.9: mask weights the *neighbour's* prediction; (64-m) weights current.
                    dst[sy * pstride + sx] =
                        (((m * o + (64 - m) * cur) + 32) >> 6).clamp(0, 255) as u8;
                }
            }
            if dbg_obmc {
                eprintln!(
                    "  blend done: any_diff={any_diff} obmc[0]={} dst_sample={}",
                    obmc[0],
                    dst[py * pstride + px]
                );
            }
            if dbg_obmc_deep {
                eprintln!("  DEEP pass={pass} px={px} py={py} pred_w={pred_w} pred_h={pred_h}:");
                for i in 0..pred_h {
                    let sy = py + i;
                    if sy >= ph {
                        break;
                    }
                    let nbr_row: Vec<u8> = (0..pred_w).map(|j| obmc[i * pred_w + j]).collect();
                    let bef_row: Vec<u8> = (0..pred_w)
                        .map(|j| before.get(i * pred_w + j).copied().unwrap_or(0))
                        .collect();
                    let dst_row: Vec<u8> = (0..pred_w)
                        .map(|j| {
                            let sx = px + j;
                            if sx < pw {
                                dst[sy * pstride + sx]
                            } else {
                                0
                            }
                        })
                        .collect();
                    eprintln!(
                        "    row={sy} nbr={nbr_row:?} before={bef_row:?} dst_after={dst_row:?}"
                    );
                }
            }
        }
    }

    /// Inter-intra blending (§7.11.3.6): predict the block intra from the
    /// reconstructed above/left edges and weight it over the already-written
    /// inter prediction with the sign-0 wedge mask
    /// (`dst = (inter * (64 - m) + intra * m + 32) >> 6`).
    fn apply_interintra(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
        ii_mode: u8,
        wedge_index: usize,
    ) {
        let bw_px = BLOCK_WIDTH[bsize];
        let bh_px = BLOCK_HEIGHT[bsize];
        let mask = crate::reconstruct::wedge::wedge_mask(bsize, false, wedge_index);
        // Inter-intra blends ALL THREE planes (dav1d recon_tmpl.c runs the
        // same `interintra_type` block for luma (II_MASK(0, ..)) and again
        // per chroma plane (II_MASK(chr_layout_idx, ..)) with intra
        // prediction built from each plane's own reconstructed edges). The
        // chroma planes use the CHROMA-layout wedge masks (generated at
        // chroma resolution per the spec's per-plane wedge mask process),
        // not the sub-sampled luma mask.
        let chroma_mask = crate::reconstruct::wedge::wedge_mask_420(bsize, false, wedge_index);
        for plane in 0..3usize {
            let (subx, suby) = if plane == 0 {
                (0usize, 0usize)
            } else {
                (self.subsampling_x as usize, self.subsampling_y as usize)
            };
            let pw = bw_px >> subx;
            let ph = bh_px >> suby;
            // Per-plane mask: the luma-layout mask for plane 0, the
            // chroma-layout mask (at chroma resolution) for planes 1-2.
            let (plane_mask, pmw) = if plane == 0 {
                (&mask, bw_px)
            } else {
                (&chroma_mask, bw_px >> self.subsampling_x as usize)
            };
            let (pstride, tile_w, tile_h) = match plane {
                1 => (self.uv_stride, self.tile_cw, self.tile_ch),
                2 => (self.uv_stride, self.tile_cw, self.tile_ch),
                _ => (self.y_stride, self.tile_w, self.tile_h),
            };
            let plane_buf = match plane {
                1 => &self.u_plane,
                2 => &self.v_plane,
                _ => &self.y_plane,
            };
            let px = ((mi_col * MI_SIZE) as isize - self.tile_px_x0 as isize) >> subx;
            let py = ((mi_row * MI_SIZE) as isize - self.tile_px_y0 as isize) >> suby;
            if px < 0 || py < 0 {
                continue;
            }
            let (px, py) = (px as usize, py as usize);
            if px >= tile_w || py >= tile_h {
                continue;
            }
            // Intra prediction from this plane's own reconstructed neighbours.
            let borders = crate::reconstruct::predict::block_borders(
                plane_buf, pstride, tile_w, tile_h, pw, ph, px, py, true, false,
            );
            let mut tmp = vec![0i32; pw * ph];
            crate::reconstruct::predict::predict_intra_block(
                ii_mode,
                &borders,
                pw,
                ph,
                &mut tmp,
                self.enable_intra_edge_filter,
                0,
                0,
                tile_w.saturating_sub(px),
                tile_h.saturating_sub(py),
            );
            if std::env::var("KINETIX_AV1_DBG_IIDUMP").is_ok() && mi_col == 4 && mi_row == 20 {
                eprintln!(
                    "IIDUMP plane={plane} ii_mode={ii_mode} wedge={wedge_index} pw={pw} ph={ph} intra={:?} mask={:?}",
                    &tmp[..(pw * ph).min(32)],
                    {
                        let sub = if plane == 0 { 0 } else { 1 };
                        (0..4)
                            .map(|y| {
                                (0..8)
                                    .map(|x| {
                                        mask[((y << sub).min(bh_px - 1)) * bw_px
                                            + ((x << sub).min(bw_px - 1))]
                                    })
                                    .collect::<Vec<u8>>()
                            })
                            .collect::<Vec<Vec<u8>>>()
                    }
                );
            }
            // Blend into the tile plane. Chroma planes fetch the mask at
            // chroma resolution from the chroma-layout table.
            let dst = match plane {
                1 => &mut self.u_plane,
                2 => &mut self.v_plane,
                _ => &mut self.y_plane,
            };
            for y in 0..ph {
                let sy = py + y;
                if sy >= tile_h {
                    break;
                }
                let my = if plane == 0 { y } else { y.min(ph - 1) };
                for x in 0..pw {
                    let sx = px + x;
                    if sx >= tile_w {
                        break;
                    }
                    let mx = if plane == 0 { x } else { x.min(pw - 1) };
                    let m = plane_mask[my * pmw + mx] as i32;
                    let idx = sy * pstride + sx;
                    let d = dst[idx] as i32;
                    let t = tmp[y * pw + x];
                    dst[idx] = ((d * (64 - m) + t * m + 32) >> 6).clamp(0, 255) as u8;
                }
            }
        }
    }

    /// Motion-compensate one plane for an inter block: for single reference, copy
    /// the reference block; for compound, average the two reference predictions.
    /// Writes directly into the tile-local plane (the prediction values).
    #[allow(clippy::too_many_arguments)]
    fn inter_predict_plane(
        &mut self,
        plane: usize,
        px_x: usize,
        px_y: usize,
        bw: usize,
        bh: usize,
        ref_names: &[u8; 2],
        mvs: &[Mv; 2],
        // [dir0 (vertical, first-read), dir1 (horizontal, second-read)]
        // kernels — swap when passing to `motion_compensate`/`_prep`.
        filters: [u8; 2],
        // Compound blend weight in sixteenths for `preds[0]` (`8` = plain
        // average); ignored for single-reference blocks.
        blend_weight: i32,
        mask: MaskDesc,
        // Frame-absolute mi (4-pixel) position of the block — only used by
        // the WARP path's `block_warp_process` (§7.11.3.5), which evaluates
        // the affine model in absolute frame coordinates.
        mi_row: usize,
        mi_col: usize,
        // `Some` only for a single-ref block with `motion_mode == WARP` and
        // a successfully-derived local warp model (§7.13.4); `None` covers
        // every other case, including dav1d's own translation-only
        // fallback, and falls back to ordinary translational MC below.
        warp_model: Option<&warp::WarpModel>,
    ) -> Result<(), KinetixError> {
        let stride = match plane {
            1 | 2 => self.uv_stride,
            _ => self.y_stride,
        };
        let w = match plane {
            1 | 2 => self.tile_cw,
            _ => self.tile_w,
        };
        let h = match plane {
            1 | 2 => self.tile_ch,
            _ => self.tile_h,
        };
        // MV sub-pel precision per axis: the caller passes the *luma* MV for
        // every plane; a subsampled chroma axis interprets it at 1/16-pel.
        let (hbits, vbits) = if plane == 0 {
            (3u32, 3u32)
        } else {
            (3 + self.subsampling_x as u32, 3 + self.subsampling_y as u32)
        };

        // Malformed / not-yet-supported streams (e.g. frame-header features
        // this parser doesn't implement) can leave a ref name whose slot
        // mapping is unset — reject them with an error instead of panicking.
        if ref_names[0] as usize >= self.ref_to_slot.len()
            || ref_names[1] as usize >= self.ref_to_slot.len()
            || self.ref_to_slot[ref_names[0] as usize] >= 8
            || self.ref_to_slot[ref_names[1] as usize] >= 8
        {
            return Err(KinetixError::Unsupported(format!(
                "AV1 inter block references unmapped reference frame slot \
                 (names {:?} -> slots {:?}/{:?})",
                ref_names,
                self.ref_to_slot.get(ref_names[0] as usize),
                self.ref_to_slot.get(ref_names[1] as usize),
            )));
        }
        let slot0 = self.ref_to_slot[ref_names[0] as usize] as usize;
        let slot1 = self.ref_to_slot[ref_names[1] as usize] as usize;
        let ref1_none = self.ref_slots.slots[slot1].is_none();
        let use_compound = ref_names[1] != NONE_FRAME && !ref1_none;

        if plane != 0 && std::env::var("KINETIX_DBG_MCCHK").is_ok() && (24..=52).contains(&px_y) {
            eprintln!(
                "MCCHK oh={} pl={} x={} y={} w={} h={} mv0=({},{}) mv1=({},{}) f=({},{}) comp={} ref0={} mi=({},{})",
                self.cur_order_hint,
                plane, px_x, px_y, bw, bh, mvs[0].row, mvs[0].col, mvs[1].row, mvs[1].col,
                filters[0], filters[1], use_compound as u8, ref_names[0], mi_col, mi_row
            );
        }

        // Single reference: motion-compensate into a local temp (so we don't hold
        // both the reference slice and the output plane borrow at once), then blit.
        if !use_compound {
            // §7.11.3.5 `block_warp_process` gate: matches dav1d's
            // `imin(bw4, bh4) > 1` (luma) / `imin(cbw4, cbh4) > 1` (chroma)
            // check, restated in this plane's own pixel units (`bw`/`bh`
            // are already the plane-scaled block size, so "> 1 mi unit" is
            // "> 4 px" here for every plane, luma included).
            let warp_eligible = bw > 4 && bh > 4;
            let (ss_hor, ss_ver) = if plane == 0 {
                (0u32, 0u32)
            } else {
                (self.subsampling_x as u32, self.subsampling_y as u32)
            };
            let tmp = {
                let mut t = vec![0u8; bw * bh];
                if let Some(rf) = self.ref_slots.slots[slot0] {
                    let (rp, rw, rh) = rf.plane(plane);
                    // `KINETIX_AV1_NO_WARP` is a bisection escape hatch (not
                    // spec behaviour): forces every WARP block back to plain
                    // translational MC, for isolating how much of a given
                    // corpus entry's remaining pixel diff is actually
                    // warp-path-attributable vs a different, pre-existing
                    // bug. Left in place (mirrors this crate's other
                    // `KINETIX_AV1_DBG_*` debug hooks) since AV1 inter is
                    // still not pixel-exact and future sessions will want it.
                    let warp_forced_off = std::env::var("KINETIX_AV1_NO_WARP").is_ok();
                    match warp_model {
                        Some(model) if warp_eligible && !warp_forced_off => {
                            if std::env::var("KINETIX_AV1_DBG_WARP").is_ok() {
                                eprintln!(
                                    "DBG warp APPLY plane={plane} mi=({mi_col},{mi_row}) bw={bw} bh={bh} model={model:?}"
                                );
                            }
                            warp::block_warp_process(
                                &mut t,
                                bw,
                                rp,
                                rw,
                                rh,
                                model,
                                mi_col as i32,
                                mi_row as i32,
                                bw,
                                bh,
                                ss_hor,
                                ss_ver,
                            );
                        }
                        _ => {
                            // §5.11.27 / dav1d `filter_fns`: the interp_filter
                            // syntax reads `dir 0` first then `dir 1`, but
                            // dav1d's `dav1d_filter_2d[filter[1]][filter[0]]`
                            // packing feeds `filter[1]` to the HORIZONTAL
                            // kernel and `filter[0]` to the VERTICAL one
                            // (verified against a patched-dav1d put_8tap_c
                            // trace: mi(4,18) read dir0=REGULAR, dir1=SMOOTH,
                            // and the real per-pixel filter dav1d applied
                            // horizontally was SMOOTH, not REGULAR). `filters`
                            // here is still `[dir0, dir1]` (kept that way for
                            // the neighbour-context storage below), so swap
                            // at the point of use.
                            motion_compensate(
                                &mut t, bw, rp, rw, rw, rh, px_x, px_y, bw, bh, mvs[0], filters[1],
                                filters[0], hbits, vbits,
                            );
                        }
                    }
                }
                t
            };
            if std::env::var("KINETIX_AV1_DBG_PREDUMP").is_ok() && plane == 1 {
                eprintln!(
                    "PREDUMP mi=({mi_col},{mi_row}) cpx=({px_x},{px_y}) w={bw} h={bh} mv=({},{}) f=({},{}) pred={:?}",
                    mvs[0].row, mvs[0].col, filters[0], filters[1],
                    &tmp[..(bw * bh).min(64)]
                );
            }
            for dy in 0..bh {
                let sy = px_y + dy;
                if sy >= h {
                    break;
                }
                for dx in 0..bw {
                    let sx = px_x + dx;
                    if sx >= w {
                        break;
                    }
                    let v = tmp[dy * bw + dx];
                    match plane {
                        1 => self.u_plane[sy * stride + sx] = v,
                        2 => self.v_plane[sy * stride + sx] = v,
                        _ => self.y_plane[sy * stride + sx] = v,
                    }
                }
            }
            return Ok(());
        }

        // Compound: blend the two predictions in the higher-precision
        // intermediate domain (§7.11.3.1 `avg` / `w_avg`, or the §7.11.3.14
        // mask blend for COMPOUND_WEDGE / COMPOUND_DIFFWTD).
        let combined = {
            let prep = |slot: usize, mv: Mv| -> Vec<i32> {
                if let Some(rf) = self.ref_slots.slots[slot] {
                    let (rp, rw, rh) = rf.plane(plane);
                    // See the single-ref motion_compensate call above: `filters`
                    // is `[dir0, dir1]`; dir1 is horizontal, dir0 is vertical.
                    motion_compensate_prep(
                        rp, rw, rw, rh, px_x, px_y, bw, bh, mv, filters[1], filters[0], hbits,
                        vbits,
                    )
                } else {
                    vec![0i32; bw * bh]
                }
            };
            let t0 = prep(slot0, mvs[0]);
            let t1 = prep(slot1, mvs[1]);
            if std::env::var("KINETIX_AV1_DBG_COMP").is_ok()
                && plane == 0
                && (mi_row == 12 || mi_row == 14)
            {
                eprintln!(
                    "COMP mi=({mi_col},{mi_row}) bw={bw} bh={bh} ref0={} ref1={} mv0={:?} mv1={:?} weight={blend_weight} comp_type={} filters=({},{})",
                    ref_names[0], ref_names[1], mvs[0], mvs[1], mask.comp_type, filters[0],
                    filters[1]
                );
                for row in 0..bh {
                    eprintln!("  t0 row={row}: {:?}", &t0[row * bw..row * bw + bw]);
                    eprintln!("  t1 row={row}: {:?}", &t1[row * bw..row * bw + bw]);
                }
                if bw > 28 && bh > 7 {
                    eprintln!("  at (28,7): t0={} t1={}", t0[7 * bw + 28], t1[7 * bw + 28]);
                }
            }
            if mask.comp_type == 3 || mask.comp_type == 4 {
                // Generate (plane 0) or sub-sample (chroma) the luma-domain
                // blend mask, then mask-blend per §7.11.3.14.
                if plane == 0 {
                    let m = if mask.comp_type == 4 {
                        crate::reconstruct::wedge::wedge_mask(
                            mask.bsize,
                            mask.mask_sign,
                            mask.wedge_index,
                        )
                    } else {
                        crate::reconstruct::wedge::diffwtd_mask(mask.mask_sign, &t0, &t1, bw, bh)
                    };
                    if std::env::var("KINETIX_AV1_DBG_COMP").is_ok()
                        && (mi_row == 12 || mi_row == 14)
                    {
                        eprintln!("  mask sign={} rows:", mask.mask_sign);
                        for row in 0..bh {
                            eprintln!("    {row}: {:?}", &m[row * bw..row * bw + bw]);
                        }
                    }
                    self.compound_mask = Some((m, bw, bh));
                }
                let subx = (plane != 0) as usize & self.subsampling_x as usize;
                let suby = (plane != 0) as usize & self.subsampling_y as usize;
                crate::reconstruct::wedge::mask_blend(
                    self.compound_mask.as_ref(),
                    subx,
                    suby,
                    &t0,
                    &t1,
                    bw,
                    bh,
                )
            } else {
                compound_blend(&t0, &t1, blend_weight)
            }
        };
        for dy in 0..bh {
            let sy = px_y + dy;
            if sy >= h {
                break;
            }
            for dx in 0..bw {
                let sx = px_x + dx;
                if sx >= w {
                    break;
                }
                let v = combined[dy * bw + dx];
                match plane {
                    1 => self.u_plane[sy * stride + sx] = v,
                    2 => self.v_plane[sy * stride + sx] = v,
                    _ => self.y_plane[sy * stride + sx] = v,
                }
            }
        }
        Ok(())
    }

    /// Read the residual coefficients per transform block of an inter block and add
    /// them to the motion-compensated prediction already present in the planes.
    #[allow(clippy::too_many_arguments)]
    fn add_inter_residual(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
        skip: bool,
        leaves: &[(usize, usize, usize)],
    ) -> Result<(), KinetixError> {
        let bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh = BLOCK_HEIGHT[bsize] / MI_SIZE;
        // Uniform-grid fallback size for the (common) non-split case, used for
        // the chroma-tx heuristic and the loop-filter metadata grid.
        let luma_tx = leaves.first().map(|l| l.2).unwrap_or(TX_4X4);
        let luma_tx_w = av1::TX_WIDTH[luma_tx];
        let luma_tx_h = av1::TX_HEIGHT[luma_tx];
        let subsampling_x = self.subsampling_x as u8;
        let subsampling_y = self.subsampling_y as u8;

        // The `FrameMeta` geometry recording below always runs (the deblock
        // filter needs every inter block's transform geometry, skipped or
        // not); only the coefficient read is gated on `!skip`, per var-tx
        // leaf for luma and per chroma-tx block for chroma.

        // Y residual + per-transform-sub-block deblock-edge geometry
        // (mirrors the intra keyframe path in `intra_block.rs` — see its
        // identical `mark_luma_edges`/`mark_luma_edges4`/`record_luma4`
        // calls for why this must run per transform sub-block, not just
        // once per coded block).
        //
        // Each leaf's decoded `TxType` is also collected: the chroma reads
        // below derive their tx type (and therefore their eob CDF context —
        // 1-D vs 2-D transform class) from the *co-located luma leaf*, so a
        // wrong type here desyncs the chroma coefficient read.
        let mut luma_leaf_types: Vec<(usize, usize, usize, usize, usize)> = Vec::new();
        for &(leaf_mi_col, leaf_mi_row, leaf_tx) in leaves {
            let leaf_tx_w = av1::TX_WIDTH[leaf_tx];
            let leaf_tx_h = av1::TX_HEIGHT[leaf_tx];
            let px_x = leaf_mi_col * MI_SIZE - self.tile_px_x0;
            let px_y = leaf_mi_row * MI_SIZE - self.tile_px_y0;
            self.meta.mark_luma_edges(
                px_x / 8,
                px_y / 8,
                (px_x + leaf_tx_w).div_ceil(8),
                (px_y + leaf_tx_h).div_ceil(8),
            );
            self.meta.mark_luma_edges4(
                px_x / 4,
                px_y / 4,
                (px_x + leaf_tx_w).div_ceil(4),
                (px_y + leaf_tx_h).div_ceil(4),
            );
            self.meta.record_luma4(
                px_x / 4,
                px_y / 4,
                (px_x + leaf_tx_w).div_ceil(4),
                (px_y + leaf_tx_h).div_ceil(4),
                leaf_tx_w as u8,
                leaf_tx_h as u8,
            );
            let mut residual = vec![0i32; leaf_tx_w * leaf_tx_h];
            let blk = TxBlockCtx {
                plane: 0,
                tx_size: leaf_tx,
                x4: px_x / 4,
                y4: px_y / 4,
                max_x4: self.luma_max_x4,
                max_y4: self.luma_max_y4,
                // See the matching fix/comment in `intra_block.rs`: the
                // *coded block's* plane size, not this transform block's.
                block_w: bw * MI_SIZE,
                block_h: bh * MI_SIZE,
                intra_dir: 0,
                uv_mode: 0,
                qindex_positive: !self.lossless,
                reduced_tx_set: self.reduced_tx_set,
                lossless: self.lossless,
                is_inter: true,
                // Irrelevant for plane 0.
                coincident_luma_tx_type: av1::DCT_DCT,
            };
            if skip {
                // A skipped block reads no coeffs but still must reset the
                // neighbour context (see `clear_coeff_context`).
                clear_coeff_context(&mut self.coeff_ctxs, &blk, leaf_tx_w / 4, leaf_tx_h / 4);
            } else {
                let coeffs = read_coeffs(
                    &mut self.dec,
                    &mut self.coeff_cdfs,
                    &mut self.coeff_ctxs,
                    &blk,
                )?;
                if std::env::var("KINETIX_AV1_DBG_B0").is_ok() {
                    eprintln!(
                        "DBG y-cf-blk mi=({mi_col},{mi_row}) tx={leaf_tx} txtp={} eob={} rng={}",
                        coeffs.tx_type,
                        coeffs.eob,
                        self.dec.raw_state().0
                    );
                }
                if std::env::var("KINETIX_AV1_DBG_PRED").is_ok()
                    && mi_col == 4
                    && mi_row == 18
                    && leaf_mi_col == 4
                    && leaf_mi_row == 18
                {
                    let (qdc, qac) = self.qindex_for_plane(0);
                    eprintln!(
                        "COEFF mi=(4,18) tx={leaf_tx} txtp={} eob={} qdc={qdc} qac={qac} rng={}",
                        coeffs.tx_type,
                        coeffs.eob,
                        self.dec.raw_state().0
                    );
                    for (i, &q) in coeffs.quant.iter().enumerate().take(coeffs.eob) {
                        if q != 0 {
                            eprintln!("  quant[{i}]={q}");
                        }
                    }
                }
                // Coeffs are always *read* (entropy sync). The residual is
                // applied at every tx size: `inverse_transform` handles the
                // adjusted-size (≤32-side) dequant stride and the 32/64-family
                // shifts generically.
                if coeffs.eob > 0 {
                    let (qindex_dc, qindex_ac) = self.qindex_for_plane(0);
                    let dequant = dequantize_coeffs(&coeffs.quant, leaf_tx, qindex_dc, qindex_ac);
                    if std::env::var("KINETIX_AV1_DBG_PRED").is_ok()
                        && mi_col == 4
                        && mi_row == 18
                        && leaf_mi_col == 4
                        && leaf_mi_row == 18
                    {
                        eprintln!("  dequant[..16]={:?}", &dequant[..16.min(dequant.len())]);
                        eprintln!(
                            "  residual BEFORE itx (all zeros expected): {:?}",
                            &residual[..16.min(residual.len())]
                        );
                    }
                    inverse_transform(
                        &dequant,
                        coeffs.tx_type,
                        leaf_tx,
                        self.lossless,
                        &mut residual,
                    );
                    if std::env::var("KINETIX_AV1_DBG_ITX").is_ok()
                        && (leaf_tx == 12 || leaf_tx == 4)
                    {
                        eprintln!(
                            "KIN ITX64x32 eob={} txtp={} dequant_row0: {:?}",
                            coeffs.eob,
                            coeffs.tx_type,
                            &dequant[..32.min(dequant.len())]
                        );
                        let stride = 64;
                        let rows = if leaf_tx == 4 { 64 } else { 32 };
                        let rowsums: Vec<i32> = (0..rows)
                            .map(|y| residual[y * stride..(y + 1) * stride].iter().sum())
                            .collect();
                        eprintln!("KIN RESID rowsums: {rowsums:?}");
                    }
                    if std::env::var("KINETIX_AV1_DBG_PRED").is_ok()
                        && mi_col == 4
                        && mi_row == 18
                        && leaf_mi_col == 4
                        && leaf_mi_row == 18
                    {
                        eprintln!("  residual AFTER itx (row-major 16x8):");
                        for row in 0..leaf_tx_h {
                            eprintln!(
                                "    row {row}: {:?}",
                                &residual[row * leaf_tx_w..(row + 1) * leaf_tx_w]
                            );
                        }
                    }
                    luma_leaf_types.push((px_x, px_y, leaf_tx_w, leaf_tx_h, coeffs.tx_type));
                }
            }
            for dy in 0..leaf_tx_h {
                let sy = px_y + dy;
                if sy >= self.tile_h {
                    break;
                }
                for dx in 0..leaf_tx_w {
                    let sx = px_x + dx;
                    if sx >= self.tile_w {
                        break;
                    }
                    if let Some(slot) = self.y_plane.get_mut(sy * self.y_stride + sx) {
                        *slot =
                            ((*slot as i32 + residual[dy * leaf_tx_w + dx]).clamp(0, 255)) as u8;
                    }
                }
            }
        }

        // Fixed 8×8-luma-grid loop-filter metadata (mirrors the intra
        // keyframe path's identical call in `intra_block.rs`).
        let blk_px_x = mi_col * MI_SIZE - self.tile_px_x0;
        let blk_px_y = mi_row * MI_SIZE - self.tile_px_y0;
        let bx0 = blk_px_x / 8;
        let by0 = blk_px_y / 8;
        let bx1 = (blk_px_x + bw * MI_SIZE).div_ceil(8);
        let by1 = (blk_px_y + bh * MI_SIZE).div_ceil(8);
        for by in by0..by1.min(self.meta.h8) {
            for bx in bx0..bx1.min(self.meta.w8) {
                self.meta
                    .record_luma(bx, by, luma_tx_w as u8, luma_tx_h as u8, skip);
            }
        }
        self.meta.record_delta_lf(bx0, by0, bx1, by1, self.delta_lf);
        self.meta.record_delta_lf4(
            blk_px_x / 4,
            blk_px_y / 4,
            (blk_px_x + bw * MI_SIZE).div_ceil(4),
            (blk_px_y + bh * MI_SIZE).div_ceil(4),
            self.delta_lf,
        );

        // Chroma residual. Inter chroma uses one uniform transform size
        // (`get_tx_size(get_plane_residual_size)`, §5.11.37) tiling the whole
        // block's chroma region — dav1d reads it (`Post-uv-cf-blk`) after all
        // the luma leaves, `pl=0` then `pl=1`.
        let sub_x = self.subsampling_x as usize;
        let sub_y = self.subsampling_y as usize;
        let c_tx = chroma_tx_size(bsize, sub_x, sub_y);
        let cw = av1::TX_WIDTH[c_tx];
        let ch = av1::TX_HEIGHT[c_tx];
        let plane_sz = {
            let sz = get_plane_residual_size(bsize, sub_x, sub_y);
            if sz == BLOCK_INVALID {
                bsize
            } else {
                sz
            }
        };
        let chroma_bw = BLOCK_WIDTH[plane_sz];
        let chroma_bh = BLOCK_HEIGHT[plane_sz];
        let base_cpx_x = (mi_col >> sub_x) * MI_SIZE - (self.tile_px_x0 >> sub_x);
        let base_cpx_y = (mi_row >> sub_y) * MI_SIZE - (self.tile_px_y0 >> sub_y);
        let has_residual = !skip;
        // §5.11.36/§7.12.3: an inter chroma transform block's tx type derives
        // from the *co-located luma leaf's* decoded tx type (1-D/identity
        // luma types make the chroma read use the 1-D eob CDF context).
        // §7.3.1 sub-8x8 chroma ownership: a 4px-wide/tall block at odd mi
        // parity codes the chroma for the whole parent 8x8, so some of its
        // chroma positions map to the SIBLING sub-block's luma area, which
        // this block's own leaf list does not cover. dav1d uses the block's
        // own luma tx type for every chroma tx block it codes (`b->txtp`),
        // so on a lookup miss fall back to this block's own first luma leaf
        // instead of DCT_DCT — a DCT_DCT fallback read the chroma tx type
        // with the wrong CDF context (txtp differs from the bitstream's) and
        // desynced the tile at the very first chroma read of such a block.
        let own_luma_tx_type = luma_leaf_types
            .first()
            .map(|&(_, _, _, _, t)| t)
            .unwrap_or(av1::DCT_DCT);
        let co_located_luma_type = |clpx_x: usize, clpx_y: usize| -> usize {
            let lx = clpx_x << sub_x;
            let ly = clpx_y << sub_y;
            for &(lx0, ly0, w, _h, t) in &luma_leaf_types {
                if lx >= lx0 && lx < lx0 + w && ly >= ly0 && ly < ly0 + _h {
                    return t;
                }
            }
            own_luma_tx_type
        };
        // Computed before the `&mut self.{u,v}_plane` reborrows in the loop
        // below — `qindex_for_plane` takes `&self`, which would conflict
        // with those live disjoint-field mutable borrows if called any later.
        let (u_qindex_dc, u_qindex_ac) = self.qindex_for_plane(1);
        let (v_qindex_dc, v_qindex_ac) = self.qindex_for_plane(2);
        // §7.3.1 has_chroma (4:2:0): a block owns chroma only if its width
        // exceeds one chroma column-pair (bw > 1 mi) or it sits at an odd
        // mi_col, and likewise for height/mi_row. Blocks failing this (e.g.
        // an 8x4 leaf at an even mi_row) have NO chroma — dav1d's
        // read_coef_blocks skips the chroma coefficient loop entirely
        // (`if (!has_chroma) continue;`), so reading our uv coefficients
        // here consumed extra bits and desynced the tile.
        let has_chroma = (bw > 1 || (mi_col & 1) == 1) && (bh > 1 || (mi_row & 1) == 1);
        for ty in (0..chroma_bh).step_by(ch) {
            for tx in (0..chroma_bw).step_by(cw) {
                let cpx_x = base_cpx_x + tx;
                let cpx_y = base_cpx_y + ty;
                if cpx_x >= self.tile_cw || cpx_y >= self.tile_ch {
                    continue;
                }
                // AV1 §7.14.1: chroma deblock edges are at luma block
                // boundaries, regardless of whether this block owns chroma
                // samples (has_chroma). A block at even (mi_col, mi_row) with
                // 4×4 luma size has_chroma=false but still creates a real
                // luma-grid boundary that the chroma deblock must filter.
                self.meta.mark_chroma_edges(
                    cpx_x / 4,
                    cpx_y / 4,
                    (cpx_x + cw).div_ceil(4),
                    (cpx_y + ch).div_ceil(4),
                );
                if !has_chroma {
                    // No chroma of its own (§7.3.1): skip coefficient reading
                    // and reconstruction, but edge geometry above is still needed.
                    continue;
                }
                for (plane, dst, stride, w, h) in [
                    (
                        1usize,
                        &mut *self.u_plane,
                        self.uv_stride,
                        self.tile_cw,
                        self.tile_ch,
                    ),
                    (
                        2usize,
                        &mut *self.v_plane,
                        self.uv_stride,
                        self.tile_cw,
                        self.tile_ch,
                    ),
                ] {
                    let mut residual = vec![0i32; cw * ch];
                    if !has_residual {
                        // A skipped block reads no chroma coeffs but must
                        // still reset the neighbour context — the luma path
                        // above already does this (`clear_coeff_context`);
                        // this branch was missing here, so a skip inter
                        // block left the chroma `above_level`/`left_level`/
                        // `*_dc` arrays holding whatever a previous block (or
                        // an earlier frame, since the arrays persist across
                        // `decode()` calls) had written, corrupting
                        // `all_zero_ctx` for the next real chroma read at
                        // that position (first observed on a hierarchical-GOP
                        // stream: a skip block leaving `left=11` stale, then
                        // desyncing the very next coded TX_8X4 chroma block).
                        let clear_blk = TxBlockCtx {
                            plane,
                            tx_size: c_tx,
                            x4: cpx_x / 4,
                            y4: cpx_y / 4,
                            max_x4: self.uv_max_x4,
                            max_y4: self.uv_max_y4,
                            block_w: 0,
                            block_h: 0,
                            intra_dir: 0,
                            uv_mode: 0,
                            qindex_positive: !self.lossless,
                            reduced_tx_set: self.reduced_tx_set,
                            lossless: self.lossless,
                            is_inter: true,
                            coincident_luma_tx_type: av1::DCT_DCT,
                        };
                        clear_coeff_context(&mut self.coeff_ctxs, &clear_blk, cw / 4, ch / 4);
                    }
                    if has_residual {
                        let blk = TxBlockCtx {
                            plane,
                            tx_size: c_tx,
                            x4: cpx_x / 4,
                            y4: cpx_y / 4,
                            max_x4: self.uv_max_x4,
                            max_y4: self.uv_max_y4,
                            // See the luma fix above: the coded block's
                            // chroma-plane size, not this transform block's
                            // own `cw`/`ch`. This inter chroma path already
                            // approximates the true `get_plane_residual_size`
                            // (see the `c_tx` heuristic above it), so this
                            // matches that same approximation rather than the
                            // exact spec table.
                            block_w: (bw * MI_SIZE) >> subsampling_x,
                            block_h: (bh * MI_SIZE) >> subsampling_y,
                            intra_dir: 0,
                            uv_mode: 0,
                            qindex_positive: !self.lossless,
                            reduced_tx_set: self.reduced_tx_set,
                            lossless: self.lossless,
                            is_inter: true,
                            // The real co-located luma leaf's decoded type —
                            // `get_uv_inter_txtp` needs it to pick the chroma
                            // transform family, and `read_eob`'s is_1d CDF
                            // context depends on it (a DCT_DCT placeholder
                            // here desynced the chroma read on every inter
                            // block whose luma leaf used a 1-D/identity type).
                            coincident_luma_tx_type: co_located_luma_type(cpx_x, cpx_y),
                        };
                        let coeffs = read_coeffs(
                            &mut self.dec,
                            &mut self.coeff_cdfs,
                            &mut self.coeff_ctxs,
                            &blk,
                        )?;
                        if std::env::var("KINETIX_AV1_DBG_B0").is_ok() {
                            eprintln!(
                                "DBG uv-cf-blk mi=({mi_col},{mi_row}) cpx=({cpx_x},{cpx_y}) pl={plane} tx={c_tx} txtp={} eob={} rng={}",
                                coeffs.tx_type,
                                coeffs.eob,
                                self.dec.raw_state().0
                            );
                        }
                        if coeffs.eob > 0 {
                            let (qindex_dc, qindex_ac) = if plane == 1 {
                                (u_qindex_dc, u_qindex_ac)
                            } else {
                                (v_qindex_dc, v_qindex_ac)
                            };
                            let dequant =
                                dequantize_coeffs(&coeffs.quant, c_tx, qindex_dc, qindex_ac);
                            inverse_transform(
                                &dequant,
                                coeffs.tx_type,
                                c_tx,
                                self.lossless,
                                &mut residual,
                            );
                            if std::env::var("KINETIX_AV1_DBG_RESDUMP").is_ok() {
                                eprintln!(
                                    "RESDUMP mi=({mi_col},{mi_row}) cpx=({cpx_x},{cpx_y}) pl={plane} tx={c_tx} txtp={} residual={:?}",
                                    coeffs.tx_type,
                                    &residual[..(cw * ch).min(64)]
                                );
                                eprintln!(
                                    "COEFFDUMP mi=({mi_col},{mi_row}) cpx=({cpx_x},{cpx_y}) pl={plane} tx={c_tx} eob={} dequant={:?}",
                                    coeffs.eob,
                                    &dequant[..(cw * ch).min(64)]
                                );
                            }
                        }
                    }
                    for dy in 0..ch {
                        let sy = cpx_y + dy;
                        if sy >= h {
                            break;
                        }
                        for dx in 0..cw {
                            let sx = cpx_x + dx;
                            if sx >= w {
                                break;
                            }
                            if let Some(slot) = dst.get_mut(sy * stride + sx) {
                                *slot =
                                    ((*slot as i32 + residual[dy * cw + dx]).clamp(0, 255)) as u8;
                            }
                        }
                    }
                    if std::env::var("KINETIX_AV1_DBG_RESDUMP").is_ok()
                        && mi_col == 4
                        && mi_row == 20
                        && cpx_x == 8
                        && cpx_y == 40
                        && plane == 1
                    {
                        eprintln!(
                            "POSTADD u rows40-43 cols8-15: {:?}",
                            (0..4)
                                .map(|r| {
                                    (0..8)
                                        .map(|c| dst[(40 + r) * stride + 8 + c])
                                        .collect::<Vec<u8>>()
                                })
                                .collect::<Vec<Vec<u8>>>()
                        );
                    }
                }
            }
        }
        // Record chroma tx/skip metadata for the same 8×8-luma grid region
        // (mirrors the intra keyframe path's identical call).
        let c_tx_w = av1::TX_WIDTH[c_tx] as u8;
        let c_tx_h = av1::TX_HEIGHT[c_tx] as u8;
        for by in by0..by1.min(self.meta.h8) {
            for bx in bx0..bx1.min(self.meta.w8) {
                self.meta.record_chroma(bx, by, c_tx_w, c_tx_h, skip);
            }
        }
        Ok(())
    }
}

/// dav1d `apply_sign` (common/intops.h).
fn apply_sign(v: i32, s: i32) -> i32 {
    if s < 0 {
        -v
    } else {
        v
    }
}

/// dav1d `fix_int_mv_precision` (env.h): round both components to whole
/// pixels (1/8-pel units), sign-aware.
fn fix_int_mv_precision(mv: &mut Mv) {
    mv.col = (mv.col - (mv.col >> 15) + 3) & !7;
    mv.row = (mv.row - (mv.row >> 15) + 3) & !7;
}
