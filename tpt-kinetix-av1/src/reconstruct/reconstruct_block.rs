use super::*;

/// `BlockDecoded` state for one transform block (AV1 §7.11.2): the current
/// superblock's per-4×4 "already reconstructed" grid for this plane, plus the
/// transform block's SB-relative 4×4 origin and size. `reconstruct_tx_block`
/// derives `haveAboveRight`/`haveBelowLeft` from `grid` before prediction and
/// marks the covered cells `1` afterwards.
pub(super) struct BlockDecodedCtx<'a> {
    pub(super) grid: &'a mut [u8],
    pub(super) sub_r: usize,
    pub(super) sub_c: usize,
    pub(super) step_x: usize,
    pub(super) step_y: usize,
}

impl BlockDecodedCtx<'_> {
    fn get(&self, sr: isize, sc: isize) -> bool {
        if sr < -1 || sc < -1 {
            return false;
        }
        let idx = ((sr + 1) as usize) * BD_STRIDE + ((sc + 1) as usize);
        self.grid.get(idx).copied().unwrap_or(0) != 0
    }
    fn have_above_right(&self) -> bool {
        self.get(self.sub_r as isize - 1, (self.sub_c + self.step_x) as isize)
    }
    fn have_below_left(&self) -> bool {
        self.get((self.sub_r + self.step_y) as isize, self.sub_c as isize - 1)
    }
    pub(super) fn mark(&mut self) {
        for i in 0..self.step_y {
            for j in 0..self.step_x {
                let idx = (self.sub_r + i + 1) * BD_STRIDE + (self.sub_c + j + 1);
                if idx < self.grid.len() {
                    self.grid[idx] = 1;
                }
            }
        }
    }
}

/// Decode one intra transform block: read its coefficients with the symbol
/// decoder, dequantize, inverse transform, predict, and write the result back
/// into `samples`.
#[allow(clippy::too_many_arguments)]
pub(super) fn reconstruct_tx_block(
    dec: &mut SymbolDecoder,
    cdfs: &mut TileCdfs,
    ctxs: &mut CoeffContexts,
    blk: &TxBlockCtx,
    samples: &mut [u8],
    stride: usize,
    plane_w: usize,
    plane_h: usize,
    px_x: usize,
    px_y: usize,
    internal_tx_size: usize,
    qindex_dc: u8,
    qindex_ac: u8,
    pred_mode: usize,
    skip: bool,
    filter_intra_mode: Option<usize>,
    enable_intra_edge_filter: bool,
    // Smooth-neighbour `filterType` (AV1 §7.11.2.9) for this block's plane —
    // 1 if the above/left neighbour uses a SMOOTH* mode, else 0. Only consumed
    // by directional prediction's edge filter / upsample thresholds.
    filter_type: i32,
    cfl: Option<CflParams>,
    angle_delta: i32,
    palette: Option<PaletteBlockInfo>,
    mut bd: BlockDecodedCtx<'_>,
) -> Result<(), KinetixError> {
    let pred_mode = pred_mode as u8;
    let tx_w = av1::TX_WIDTH[internal_tx_size];
    let tx_h = av1::TX_HEIGHT[internal_tx_size];
    let num_coeffs = tx_w * tx_h;

    let dbg_uv =
        px_y < 8 && px_x < 12 && blk.plane != 0 && std::env::var("KINETIX_AV1_DBG_UV").is_ok();
    // `KINETIX_AV1_DBG_PX=x,y` targets one exact tx block (any plane/pos).
    let dbg_px = std::env::var("KINETIX_AV1_DBG_PX").ok().and_then(|s| {
        let mut it = s.split(',');
        Some((
            it.next()?.trim().parse::<usize>().ok()?,
            it.next()?.trim().parse::<usize>().ok()?,
        ))
    }) == Some((px_x, px_y));
    let dbg = (px_y < 32
        && (px_x < 4 || (16..64).contains(&px_x))
        && blk.plane == 0
        && std::env::var("KINETIX_AV1_DBG").is_ok())
        || dbg_uv
        || dbg_px;
    if dbg_uv {
        eprintln!("DBG UV plane={} px=({px_x},{px_y})", blk.plane);
    }

    let mark_plane = blk.plane;
    crate::entropy::mark_block(|| {
        format!(
            "coeffs plane={mark_plane} px=({px_x},{px_y}) tx={tx_w}x{tx_h} skip={skip} pred_mode={pred_mode}"
        )
    });

    let mut residual = vec![0i32; num_coeffs];
    if !skip {
        if dbg {
            eprintln!("DBG bit_pos before coeffs = {:?}", dec.dbg_bit_pos());
        }
        // Phase G.0 bridge: capture raw bytes + TxBlockCtx + Kinetix's own
        // symbol slice for the independent Python oracle
        // (tools/av1_oracle/diff_block.py) when KINETIX_AV1_CAPTURE names this
        // block. The context/CDF/raw-decoder-state snapshots are taken
        // *before* read_coeffs — read_coeffs mutates `ctxs` (writes this
        // block's own cul_level/dc_category into above_level/left_level),
        // `cdfs` (CDF adaptation), and `dec` (symbol_range/symbol_value), so
        // snapshotting after the call would hand the oracle this same
        // block's own post-read state instead of the state the real decoder
        // used to make its reads. The raw arithmetic-coder state
        // (`symbol_range`/`symbol_value`) specifically must be captured
        // directly rather than re-derived from raw bytes at this bit offset:
        // `SymbolDecoder::new`'s `init_symbol` forces `symbol_range = 1 <<
        // 15`, which only matches the real mid-tile value when the true
        // range happens to already be exactly `32768` at this instant (it's
        // otherwise anywhere in `[1 << 15, 1 << 16)` post-renormalization) —
        // see `SymbolDecoder::raw_state`'s doc comment.
        let pre_raw_state = dec.raw_state();
        let pre_trace_len = crate::entropy::symbol_trace_len_now();
        let capture = crate::entropy::should_capture(blk);
        let pre_ctx_snap = capture.then(|| ctxs.ctx_snapshot(blk.plane));
        let pre_cdf_snap = capture.then(|| cdfs.cdf_snapshot());
        let coeffs = read_coeffs(dec, cdfs, ctxs, blk)?;
        if std::env::var("KINETIX_AV1_TRACE").is_ok() {
            eprintln!(
                "KTRACE CF plane={mark_plane} px=({px_x},{px_y}) tx={tx_w}x{tx_h} txtp={} eob={} r={}",
                coeffs.tx_type,
                coeffs.eob,
                dec.raw_state().0,
            );
        }
        if let (Some(ctx_snap), Some(cdf_snap)) = (&pre_ctx_snap, &pre_cdf_snap) {
            crate::entropy::maybe_capture_block(
                dec,
                blk,
                ctx_snap,
                cdf_snap,
                qindex_ac,
                pre_raw_state,
                pre_trace_len,
            );
        }
        if dbg {
            eprintln!(
                    "DBG reconstruct_tx_block px=({px_x},{px_y}) tx_w={tx_w} tx_h={tx_h} eob={} tx_type={} quant={:?}",
                    coeffs.eob,
                    coeffs.tx_type,
                    &coeffs.quant[..]
                );
        }
        if coeffs.eob > 0 {
            let dequant = dequantize_coeffs(&coeffs.quant, internal_tx_size, qindex_dc, qindex_ac);
            if dbg {
                eprintln!(
                    "DBG dequant qindex_dc={qindex_dc} qindex_ac={qindex_ac} dequant={:?}",
                    &dequant[..]
                );
            }
            inverse_transform(
                &dequant,
                coeffs.tx_type,
                internal_tx_size,
                blk.lossless,
                &mut residual,
            );
            if dbg {
                eprintln!(
                    "DBG residual[0..8]={:?}",
                    &residual[..residual.len().min(8)]
                );
                if std::env::var("KINETIX_AV1_DBG_FULL").is_ok() {
                    eprintln!("DBG full quant={:?}", coeffs.quant);
                    eprintln!("DBG full residual={residual:?}");
                }
            }
        }
    } else if dbg {
        eprintln!("DBG reconstruct_tx_block px=({px_x},{px_y}) tx_w={tx_w} tx_h={tx_h} SKIP");
    }

    let borders = block_borders(
        samples,
        stride,
        plane_w,
        plane_h,
        tx_w,
        tx_h,
        px_x,
        px_y,
        bd.have_above_right(),
        bd.have_below_left(),
    );
    if dbg {
        eprintln!(
            "DBG top={:?} left={:?} tl={} have_above={} have_left={}",
            &borders.top[..borders.top.len().min(8)],
            &borders.left[..borders.left.len().min(8)],
            borders.tl,
            borders.have_above,
            borders.have_left
        );
    }
    let mut pred = vec![0i32; num_coeffs];
    // AV1 spec §7.11.2.1's top-level dispatch: palette (§7.11.4) takes
    // priority over everything else (filter-intra, CFL, ordinary modes) when
    // `PaletteSize{Y,UV} > 0` for this plane.
    if dbg {
        eprintln!("DBG palette_present={}", palette.is_some());
    }
    match &palette {
        Some(p) => predict_palette(p, tx_w, tx_h, &mut pred),
        None => match filter_intra_mode {
            Some(fi_mode) if blk.plane == 0 => {
                predict_filter_intra(
                    fi_mode,
                    &borders.top,
                    &borders.left,
                    borders.tl,
                    tx_w,
                    tx_h,
                    &mut pred,
                );
            }
            _ => predict_intra_block(
                pred_mode,
                &borders,
                tx_w,
                tx_h,
                &mut pred,
                enable_intra_edge_filter,
                filter_type,
                angle_delta,
                plane_w.saturating_sub(px_x),
                plane_h.saturating_sub(px_y),
            ),
        },
    }
    // `predict_chroma_from_luma` (AV1 spec §7.11.5): applied to the DC
    // prediction just computed above, before the residual is added.
    if let Some(cfl) = &cfl {
        apply_cfl_prediction(&mut pred, tx_w, tx_h, px_x, px_y, cfl);
    }
    if dbg {
        eprintln!("DBG pred[0..8]={:?}", &pred[..pred.len().min(8)]);
        if dbg_px {
            eprintln!("DBG borders.left={:?}", borders.left);
            eprintln!("DBG borders.top ={:?}", borders.top);
            eprintln!("DBG borders.tl  ={}", borders.tl);
            eprintln!(
                "DBG cfl={:?} filter_intra={:?} palette={}",
                cfl.is_some(),
                filter_intra_mode,
                palette.is_some()
            );
            for r in 0..tx_h {
                let p: Vec<i32> = (0..tx_w).map(|c| pred[r * tx_w + c]).collect();
                let q: Vec<i32> = (0..tx_w).map(|c| residual[r * tx_w + c]).collect();
                eprintln!("DBG row{r:>2}: pred={:?} res={:?}", p, q);
            }
        }
    }

    for dy in 0..tx_h {
        let sy = px_y + dy;
        if sy >= plane_h {
            break;
        }
        for dx in 0..tx_w {
            let sx = px_x + dx;
            if sx >= plane_w {
                break;
            }
            if let Some(slot) = samples.get_mut(sy * stride + sx) {
                *slot = (pred[dy * tx_w + dx] + residual[dy * tx_w + dx]).clamp(0, 255) as u8;
            }
        }
    }

    // §7.11.2: this transform block's 4×4 cells are now reconstructed.
    bd.mark();

    Ok(())
}
