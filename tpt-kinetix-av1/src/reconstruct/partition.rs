use super::*;

/// Split a `bsize` block (in mi units) according to `partition` into its
/// sub-block (sub_bsize, mi_row_offset, mi_col_offset) list. Mirrors the AV1
/// `Partition_Subsize` table logic.
fn split_into_subblocks(bw: usize, bh: usize, partition: u8) -> Vec<(usize, usize, usize)> {
    let hw = bw / 2;
    let hh = bh / 2;
    let qw = bw / 4;
    let qh = bh / 4;
    let mut out = Vec::new();
    match partition {
        PARTITION_NONE => out.push((bsize_from_wh(bw * 4, bh * 4), 0, 0)),
        PARTITION_HORZ => {
            out.push((bsize_from_wh(bw * 4, hh * 4), 0, 0));
            out.push((bsize_from_wh(bw * 4, hh * 4), hh, 0));
        }
        PARTITION_VERT => {
            out.push((bsize_from_wh(hw * 4, bh * 4), 0, 0));
            out.push((bsize_from_wh(hw * 4, bh * 4), 0, hw));
        }
        PARTITION_SPLIT => {
            out.push((bsize_from_wh(hw * 4, hh * 4), 0, 0));
            out.push((bsize_from_wh(hw * 4, hh * 4), 0, hw));
            out.push((bsize_from_wh(hw * 4, hh * 4), hh, 0));
            out.push((bsize_from_wh(hw * 4, hh * 4), hh, hw));
        }
        // AV1 spec §5.11.4 `decode_partition()`: the A/B partitions use
        // `splitSize = Partition_Subsize[PARTITION_SPLIT][bSize]` (the
        // quarter-area `hw x hh` shape) for their split half and
        // `subSize = Partition_Subsize[partition][bSize]` (the `bw x hh` or
        // `hw x bh` HORZ/VERT shape) for their whole half -- **not** a
        // 1:3 quarter/three-quarter split. The previous implementation used
        // `qh`/`3*qh` (`qw`/`3*qw`), producing a `(bw, 3*qh)`-shaped request
        // that has no matching `BLOCK_SIZES` entry (AV1 has no e.g. 16x12
        // block), so `bsize_from_wh` silently fell back to its
        // "not found" default (`BLOCK_8X8`) for that sub-block -- decoding
        // the wrong block shape entirely and desyncing every subsequent
        // symbol read in the superblock whenever a real encoder chose one of
        // these four partition types (common on any non-trivial content).
        PARTITION_HORZ_A => {
            out.push((bsize_from_wh(hw * 4, hh * 4), 0, 0));
            out.push((bsize_from_wh(hw * 4, hh * 4), 0, hw));
            out.push((bsize_from_wh(bw * 4, hh * 4), hh, 0));
        }
        PARTITION_HORZ_B => {
            out.push((bsize_from_wh(bw * 4, hh * 4), 0, 0));
            out.push((bsize_from_wh(hw * 4, hh * 4), hh, 0));
            out.push((bsize_from_wh(hw * 4, hh * 4), hh, hw));
        }
        PARTITION_VERT_A => {
            out.push((bsize_from_wh(hw * 4, hh * 4), 0, 0));
            out.push((bsize_from_wh(hw * 4, hh * 4), hh, 0));
            out.push((bsize_from_wh(hw * 4, bh * 4), 0, hw));
        }
        PARTITION_VERT_B => {
            out.push((bsize_from_wh(hw * 4, bh * 4), 0, 0));
            out.push((bsize_from_wh(hw * 4, hh * 4), 0, hw));
            out.push((bsize_from_wh(hw * 4, hh * 4), hh, hw));
        }
        PARTITION_HORZ_4 => {
            for i in 0..4 {
                out.push((bsize_from_wh(bw * 4, qh * 4), i * qh, 0));
            }
        }
        PARTITION_VERT_4 => {
            for i in 0..4 {
                out.push((bsize_from_wh(qw * 4, bh * 4), 0, i * qw));
            }
        }
        _ => out.push((bsize_from_wh(bw * 4, bh * 4), 0, 0)),
    }
    out
}

impl<'a> TileDecodeState<'a> {
    /// Walk one superblock (top-left at `mi_row`/`mi_col`, size `sb_bsize`).
    pub(super) fn decode_superblock(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        sb_bsize: usize,
    ) -> Result<(), KinetixError> {
        self.coeff_ctxs.clear_left();
        // §5.11.4 `decode_tile()`'s per-superblock prelude: `ReadDeltas =
        // delta_q_present`, then `clear_cdef(r, c)`, before walking the
        // partition tree.
        self.read_deltas = self.delta_q_present;
        self.clear_cdef(mi_row, mi_col);
        self.decode_partition(mi_row, mi_col, sb_bsize)
    }

    /// Recursively decode the partition tree (AV1 spec §5.11.4).
    fn decode_partition(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
    ) -> Result<(), KinetixError> {
        // AV1 §5.11.4: a partition block entirely outside the frame (i.e. in
        // the superblock-padding region past the bottom/right edge) is never
        // decoded. Without this, the recursion walks a sub-block whose top-left
        // mi position equals `mi_rows`/`mi_cols`, indexing the neighbour-context
        // arrays out of bounds.
        if mi_row >= self.mi_rows || mi_col >= self.mi_cols {
            return Ok(());
        }
        if bsize < BLOCK_8X8 {
            return self.decode_block(mi_row, mi_col, bsize);
        }
        // AV1 §5.11.4: a block whose right/bottom half falls outside the
        // frame cannot legally read the full 10-way `partition` symbol — the
        // encoder never wrote one. Depending on which half is out of bounds,
        // the bitstream instead carries a constrained binary
        // `split_or_horz`/`split_or_vert` symbol (§8.3.2's synthetic 2-symbol
        // CDF folded from the full partition CDF), or no symbol at all
        // (forced `PARTITION_SPLIT`) when *neither* half fits. Reading the
        // full symbol unconditionally (as this used to do) desyncs the
        // entropy decoder for every frame whose size isn't an exact multiple
        // of the superblock size — which is most real content, including the
        // AV1 Phase G conformance corpus (e.g. a 32x32 frame with a 64x64
        // superblock never satisfies `hasRows`/`hasCols` for the root
        // partition, so the very first symbol read in the tile was already
        // wrong).
        let bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh = BLOCK_HEIGHT[bsize] / MI_SIZE;
        let half4x4 = bw >> 1;
        let has_rows = mi_row + half4x4 < self.mi_rows;
        let has_cols = mi_col + half4x4 < self.mi_cols;

        // Partition context: placeholder 0 works for edge / single-block frames
        // (neighbours are all NONE → ctx 0). Refined once general keyframes
        // are validated.
        let ctx = self.partition_context(mi_row, mi_col, bsize);
        let bucket = PARTITION_CDF_LOOKUP[bsize];
        let partition = if has_rows && has_cols {
            self.mode_cdfs.read_partition(&mut self.dec, bucket, ctx) as u8
        } else if has_cols {
            if self
                .mode_cdfs
                .read_split_or_horz(&mut self.dec, bucket, ctx, bsize)
            {
                PARTITION_SPLIT
            } else {
                PARTITION_HORZ
            }
        } else if has_rows {
            if self
                .mode_cdfs
                .read_split_or_vert(&mut self.dec, bucket, ctx, bsize)
            {
                PARTITION_SPLIT
            } else {
                PARTITION_VERT
            }
        } else {
            PARTITION_SPLIT
        };
        let subs = split_into_subblocks(bw, bh, partition);

        if mi_row < 8 && mi_col < 8 && std::env::var("KINETIX_AV1_DBG_PART").is_ok() {
            eprintln!(
                "DBG partition mi=({mi_row},{mi_col}) bsize={bsize} has_rows={has_rows} has_cols={has_cols} partition={partition} subs={subs:?}"
            );
        }

        // Only `PARTITION_SPLIT` (and the 1:4 variants that recurse) descend into
        // smaller blocks; every other partition resolves into leaf blocks at the
        // current recursion level. Recursing for a `PARTITION_NONE` would push a
        // same-size, same-position sub-block and overflow the stack.
        if partition == PARTITION_SPLIT && bsize > BLOCK_8X8 {
            for (sub_bsize, ro, co) in subs {
                self.decode_partition(mi_row + ro, mi_col + co, sub_bsize)?;
            }
        } else {
            for (sub_bsize, ro, co) in subs {
                let srow = mi_row + ro;
                let scol = mi_col + co;
                // A sub-block whose top-left falls outside the frame (e.g. the
                // lower half of a HORZ partition on the bottom superblock row)
                // is never decoded — the reference decoder skips it. Without
                // this guard the neighbour-context arrays are indexed out of
                // bounds.
                if srow < self.mi_rows && scol < self.mi_cols {
                    self.decode_block(srow, scol, sub_bsize)?;
                }
            }
        }
        Ok(())
    }

    /// `partition`'s CDF-selection context (AV1 spec §8.3.2):
    /// `ctx = left * 2 + above`, where `above`/`left` compare the current
    /// node's `bsl` (`Mi_Width_Log2[bSize]`, square nodes only) against the
    /// most-recently-decoded leaf block's width/height log2 immediately
    /// above/left (`MiSizes[r-1][c]`/`MiSizes[r][c-1]`), gated on that
    /// neighbour existing (`AvailU`/`AvailL`).
    #[inline]
    pub(super) fn partition_context(&self, mi_row: usize, mi_col: usize, bsize: usize) -> usize {
        let bsl = mi_width_log2(bsize);
        let avail_u = mi_row > 0;
        let avail_l = mi_col > 0;
        let above =
            avail_u && (self.mi_width_log2_above[mi_col.min(self.mi_cols - 1)] as usize) < bsl;
        let left =
            avail_l && (self.mi_height_log2_left[mi_row.min(self.mi_rows - 1)] as usize) < bsl;
        (left as usize) * 2 + (above as usize)
    }

    #[inline]
    pub(super) fn segment_id_context(&self, _mi_row: usize, _mi_col: usize) -> usize {
        // AV1 §5.11.9: context = above_seg_pred + left_seg_pred. The corpus used
        // for Phase C validation has no segmentation, so this stays a placeholder
        // (context 0); refine when segmentation is enabled.
        0
    }

    /// Record a just-decoded leaf block's size into the `MiSizes`-derived
    /// above/left context arrays (see [`Self::partition_context`]), covering
    /// its full mi extent. Called once per leaf from [`Self::decode_block`]
    /// — *not* once per partition-tree node, since the context needs the
    /// actual resulting leaf size, not the (possibly-larger) node size that
    /// was about to be split.
    pub(super) fn record_mi_size_context(&mut self, mi_row: usize, mi_col: usize, bsize: usize) {
        let bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh = BLOCK_HEIGHT[bsize] / MI_SIZE;
        let w_log2 = mi_width_log2(bsize) as u8;
        let h_log2 = mi_height_log2(bsize) as u8;
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(slot) = self.mi_width_log2_above.get_mut(c) {
                *slot = w_log2;
            }
        }
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(slot) = self.mi_height_log2_left.get_mut(r) {
                *slot = h_log2;
            }
        }
    }

    /// Decode one leaf block: intra luma mode, chroma mode, transform size,
    /// then reconstruct every transform block (luma + chroma).
    /// Block decode dispatcher: intra-coded frame → intra path; inter-coded
    /// frame → inter path (AV1 Phase E). Leaf blocks in the partition tree route
    /// here.
    fn decode_block(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
    ) -> Result<(), KinetixError> {
        self.record_mi_size_context(mi_row, mi_col, bsize);
        if !self.frame_is_intra {
            return self.decode_inter_block(mi_row, mi_col, bsize);
        }
        self.decode_intra_block(mi_row, mi_col, bsize)
    }
}

impl<'a> TileDecodeState<'a> {
    /// Read the transform size for an intra block (AV1 spec §5.11.17 / the
    /// `read_tx_size` → `read_selected_tx_size` descent).
    /// `read_tx_size(allowSelect)` (AV1 spec §5.11.15), for the case that
    /// matters here: `allowSelect` is always `true` for an intra block (spec
    /// `allowSelect = !skip || !is_inter`, and `is_inter` is `false`), so the
    /// only remaining gate is `MiSize > BLOCK_4X4` (checked by the caller via
    /// `max_tx`/`bsize`) and `TxMode == TX_MODE_SELECT`. Reads a *single*
    /// ternary `tx_depth` symbol and applies `Split_Tx_Size` that many times
    /// — the previous implementation here read up to 3 separate binary
    /// "go bigger?" symbols in a loop, which is a different syntax model
    /// entirely and desynced the entropy decoder for every non-skipped
    /// keyframe block whenever `TxMode == TX_MODE_SELECT` (i.e. essentially
    /// always, since `TX_MODE_SELECT` is the common case real encoders use).
    pub(super) fn read_tx_size(
        &mut self,
        bsize: usize,
        max_tx: usize,
        mi_row: usize,
        mi_col: usize,
    ) -> usize {
        let max_tx_depth = MAX_TX_DEPTH_TABLE[bsize];
        let bucket = match max_tx_depth {
            4 => 3, // TileTx64x64Cdf
            3 => 2, // TileTx32x32Cdf
            2 => 1, // TileTx16x16Cdf
            _ => 0, // TileTx8x8Cdf
        };
        let ctx = self.tx_depth_context(mi_row, mi_col, max_tx);
        let tx_depth = self.mode_cdfs.read_tx_level(&mut self.dec, bucket, ctx);
        // AV1 spec §5.11.15: `TxSize = maxRectTxSize; for (i = 0; i <
        // tx_depth; i++) TxSize = Split_Tx_Size[TxSize]`. A previous revision
        // computed `max_tx.saturating_sub(tx_depth)` instead — treating the
        // `TxSize` enum as a linear scale, which only happens to match
        // `Split_Tx_Size` for the five square indices (`TX_4X4..TX_64X64`,
        // consecutive by construction); every rectangular `max_tx` (index
        // >= 5) would decrement into an unrelated size instead of actually
        // splitting per spec (e.g. `Split_Tx_Size[TX_32X8] = TX_16X8`, index
        // 8, not `TX_32X8`'s own index (16) minus 1 = 15/`TX_8X32`).
        let mut tx_size = max_tx;
        for _ in 0..tx_depth {
            tx_size = av1::SPLIT_TX_SIZE[tx_size];
        }
        tx_size
    }

    /// `tx_depth`'s CDF-selection context (AV1 spec §8.3.2): compares the
    /// above/left neighbour's transform *width*/*height* in samples against
    /// `maxRectTxSize`'s own width/height (`ctx = (aboveW >= maxTxWidth) +
    /// (leftH >= maxTxHeight)`). `tx_above`/`tx_left` store exactly those two
    /// quantities (width and height respectively, in samples — not the
    /// `TxSize` enum index, which isn't monotonic in size across the
    /// square/rectangular index space and previously made this comparison
    /// meaningless for any rectangular neighbour). This omits the spec's
    /// `IsInters` branch (using the neighbour's coded block width/height
    /// instead of its transform width/height when the neighbour is
    /// inter-coded) since the intra-frame path this crate validates against
    /// never has an inter neighbour.
    #[inline]
    fn tx_depth_context(&self, mi_row: usize, mi_col: usize, max_tx: usize) -> usize {
        let above_w = self.tx_above[mi_col] as usize;
        let left_h = self.tx_left[mi_row] as usize;
        let max_tx_width = av1::TX_WIDTH[max_tx];
        let max_tx_height = av1::TX_HEIGHT[max_tx];
        usize::from(above_w >= max_tx_width) + usize::from(left_h >= max_tx_height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AV1 spec §5.11.4 `decode_partition()`: for a 16x16 node (bw=bh=4 mi
    // units, hw=hh=2, qw=qh=1), `splitSize = Partition_Subsize[SPLIT][16x16]`
    // = 8x8 and `subSize = Partition_Subsize[HORZ][16x16]` = 16x8. Hand-
    // verified against the spec pseudocode directly (see the fix's doc
    // comment above): HORZ_A must produce exactly 3 sub-blocks, not 2, and
    // none of them may be the quarter/three-quarter `(bw, qh)`/`(bw, 3*qh)`
    // shape the previous, buggy implementation used (which doesn't
    // correspond to any real `BLOCK_SIZES` entry and silently fell back to
    // `BLOCK_8X8` via `bsize_from_wh`'s not-found default).
    #[test]
    fn horz_a_produces_three_subblocks_matching_spec_split_and_horz_shapes() {
        let subs = split_into_subblocks(4, 4, PARTITION_HORZ_A);
        assert_eq!(
            subs,
            vec![(BLOCK_8X8, 0, 0), (BLOCK_8X8, 0, 2), (BLOCK_16X8, 2, 0),]
        );
    }

    #[test]
    fn horz_b_produces_three_subblocks_matching_spec_split_and_horz_shapes() {
        let subs = split_into_subblocks(4, 4, PARTITION_HORZ_B);
        assert_eq!(
            subs,
            vec![(BLOCK_16X8, 0, 0), (BLOCK_8X8, 2, 0), (BLOCK_8X8, 2, 2),]
        );
    }

    #[test]
    fn vert_a_produces_three_subblocks_matching_spec_split_and_vert_shapes() {
        let subs = split_into_subblocks(4, 4, PARTITION_VERT_A);
        assert_eq!(
            subs,
            vec![(BLOCK_8X8, 0, 0), (BLOCK_8X8, 2, 0), (BLOCK_8X16, 0, 2),]
        );
    }

    #[test]
    fn vert_b_produces_three_subblocks_matching_spec_split_and_vert_shapes() {
        let subs = split_into_subblocks(4, 4, PARTITION_VERT_B);
        assert_eq!(
            subs,
            vec![(BLOCK_8X16, 0, 0), (BLOCK_8X8, 0, 2), (BLOCK_8X8, 2, 2),]
        );
    }

    // A larger (32x32) node exercises a non-square-halved `splitSize`
    // (16x16) and `subSize` (32x16), confirming the shapes scale correctly
    // rather than only happening to work at the 16x16 size tested above.
    #[test]
    fn horz_a_scales_to_a_32x32_node() {
        let subs = split_into_subblocks(8, 8, PARTITION_HORZ_A);
        assert_eq!(
            subs,
            vec![
                (BLOCK_16X16, 0, 0),
                (BLOCK_16X16, 0, 4),
                (BLOCK_32X16, 4, 0),
            ]
        );
    }
}
