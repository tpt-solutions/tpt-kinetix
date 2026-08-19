use super::*;

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
    qindex: u8,
    pred_mode: usize,
    skip: bool,
    filter_intra_mode: Option<usize>,
    enable_intra_edge_filter: bool,
    cfl: Option<CflParams>,
    angle_delta: i32,
    palette: Option<PaletteBlockInfo>,
) -> Result<(), KinetixError> {
    let pred_mode = pred_mode as u8;
    let tx_w = av1::TX_WIDTH[internal_tx_size];
    let tx_h = av1::TX_HEIGHT[internal_tx_size];
    let num_coeffs = tx_w * tx_h;

    let dbg_uv = px_y < 8 && px_x < 12 && blk.plane != 0 && std::env::var("KINETIX_AV1_DBG_UV").is_ok();
    let dbg = (px_y < 32 && (16..64).contains(&px_x) && blk.plane == 0 && std::env::var("KINETIX_AV1_DBG").is_ok())
        || dbg_uv;
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
        let coeffs = read_coeffs(dec, cdfs, ctxs, blk)?;
        if dbg {
            eprintln!(
                "DBG reconstruct_tx_block px=({px_x},{px_y}) tx_w={tx_w} tx_h={tx_h} eob={} tx_type={} quant[0..8]={:?}",
                coeffs.eob,
                coeffs.tx_type,
                &coeffs.quant[..coeffs.quant.len().min(8)]
            );
        }
        if coeffs.eob > 0 {
            let dequant = dequantize_coeffs(&coeffs.quant, internal_tx_size, qindex);
            if dbg {
                eprintln!(
                    "DBG dequant qindex={qindex} dequant[0..8]={:?}",
                    &dequant[..dequant.len().min(8)]
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
                if px_x == 0 && px_y == 0 && std::env::var("KINETIX_AV1_DBG_FULL").is_ok() {
                    eprintln!("DBG full quant={:?}", coeffs.quant);
                    eprintln!("DBG full residual={residual:?}");
                }
            }
        }
    } else if dbg {
        eprintln!("DBG reconstruct_tx_block px=({px_x},{px_y}) tx_w={tx_w} tx_h={tx_h} SKIP");
    }

    let borders = block_borders(samples, stride, plane_w, plane_h, tx_w, tx_h, px_x, px_y);
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
                blk.plane == 0,
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

    Ok(())
}
