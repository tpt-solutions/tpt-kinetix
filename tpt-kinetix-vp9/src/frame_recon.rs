//! Tile-decoder reconstruction: coefficient decoding per transform block,
//! intra reconstruction, inter reconstruction (motion compensation +
//! residual), and loop-filter level/edge-mask recording. Inherent impls on
//! [`TileDecoder`](crate::frame::TileDecoder).

#![allow(clippy::too_many_arguments)]

use std::rc::Rc;

use tpt_kinetix_core::error::KinetixError;

use crate::booldec::BoolDecoder;
use crate::coef::decode_coeffs_b;
use crate::frame::{merge_nnz, scans, splat_nnz, TileDecoder, BAND_COUNTS};
use crate::header::FrameType;
use crate::predict::Mv;
use crate::predict::{
    avg_mv2, avg_mv4, gather_intra_edges, intra_predict, mc_block, mc_block_scaled, FilterType,
};
use crate::tables::INTRA_TXFM_TYPE;
use crate::transform::{inverse_transform_add, DCT_DCT};

impl<'a> TileDecoder<'a> {
    /// `decode_coeffs`: parse all luma and chroma coefficient tokens of the
    /// current block, maintaining the above/left non-zero contexts. Returns
    /// whether any coefficient was present.
    pub(super) fn decode_block_coeffs(
        &mut self,
        bc: &mut BoolDecoder,
    ) -> Result<bool, KinetixError> {
        let col = self.col;
        let row = self.row;
        let row7 = self.row7;
        let bs = self.b.bs;
        let tx = self.b.tx;
        let (bw4, bh4) = crate::header::bwh(1, bs); // 8px
        let end_x = (bw4 << 1).min(2 * (self.cols() - col)); // 4px units
        let end_y = (bh4 << 1).min(2 * (self.rows() - row));
        let seg = self.b.seg_id as usize;
        let qmul_y = self.seg.qmul[seg][0];
        let qmul_uv = self.seg.qmul[seg][1];
        let intra = self.b.intra;
        let bt = usize::from(!intra);
        let mut total = false;

        let step = 1usize << tx;
        let is32 = tx == 3;

        // Luma: merge contexts once, decode every transform block, splat once.
        if tx > 0 {
            merge_nnz(&mut self.state.above_y_nnz, col * 2, end_x, step);
            merge_nnz(&mut self.left.y_nnz, row7 * 2, end_y, step);
        }
        let mut n = 0usize;
        let mut y = 0usize;
        while y < end_y {
            let mut x = 0usize;
            while x < end_x {
                let nnz = self.state.above_y_nnz[col * 2 + x] + self.left.y_nnz[row7 * 2 + y];
                let mode_idx = if bs > crate::frame::BS_8X8 && tx == 0 {
                    y * 2 + x
                } else {
                    0
                };
                // Directional scans apply to 4x4/8x8/16x16; only 32x32 is
                // all-default (reference vp9_scan_orders TX_32X32 row).
                let txtp = if intra && tx < 3 {
                    INTRA_TXFM_TYPE[self.b.mode[mode_idx]] as usize
                } else {
                    DCT_DCT
                };
                let tx4 = if self.lossless { 4 } else { tx };
                let (scan, nb) = scans(tx4, txtp);
                let n_coeffs = 16 * step * step;
                let eob = decode_coeffs_b(
                    bc,
                    n_coeffs,
                    is32,
                    self.probs.coef_full.as_ref(),
                    (tx, bt, 0),
                    &mut self.counts.coef,
                    &mut self.counts.eob,
                    nnz,
                    scan,
                    nb,
                    &BAND_COUNTS[tx],
                    [i32::from(qmul_y[0]), i32::from(qmul_y[1])],
                    &mut self.scratch_y[n * 16..n * 16 + n_coeffs],
                );
                total |= eob != 0;
                self.eob_y[n] = eob as u16;
                self.state.above_y_nnz[col * 2 + x] = u8::from(eob != 0);
                self.left.y_nnz[row7 * 2 + y] = u8::from(eob != 0);
                n += step * step;
                x += step;
            }
            y += step;
        }
        if tx > 0 {
            splat_nnz(&mut self.state.above_y_nnz, col * 2, end_x, step);
            splat_nnz(&mut self.left.y_nnz, row7 * 2, end_y, step);
        }

        // Chroma (both planes share the geometry; the contexts are per plane).
        let ss_h = self.hdr.subsampling_x as usize;
        let ss_v = self.hdr.subsampling_y as usize;
        let end_x_uv = end_x >> ss_h;
        let end_y_uv = end_y >> ss_v;
        let ustep = 1usize << self.b.uvtx;
        let uv_is32 = self.b.uvtx == 3;
        let uvtx4 = if self.lossless { 4 } else { self.b.uvtx };
        let (uvscan, uvnb) = scans(uvtx4, DCT_DCT);
        let uvtx = self.b.uvtx;
        if uvtx > 0 {
            for pl in 0..2 {
                merge_nnz(&mut self.state.above_uv_nnz[pl], col, end_x_uv, ustep);
                merge_nnz(&mut self.left.uv_nnz[pl], row7, end_y_uv, ustep);
            }
        }
        for pl in 0..2 {
            let mut n = 0usize;
            let mut y = 0usize;
            while y < end_y_uv {
                let mut x = 0usize;
                while x < end_x_uv {
                    let nnz = self.state.above_uv_nnz[pl][col + x] + self.left.uv_nnz[pl][row7 + y];
                    let n_coeffs = 16 * ustep * ustep;
                    let eob = decode_coeffs_b(
                        bc,
                        n_coeffs,
                        uv_is32,
                        self.probs.coef_full.as_ref(),
                        (uvtx, bt, 1),
                        &mut self.counts.coef,
                        &mut self.counts.eob,
                        nnz,
                        uvscan,
                        uvnb,
                        &BAND_COUNTS[uvtx],
                        [i32::from(qmul_uv[0]), i32::from(qmul_uv[1])],
                        &mut self.scratch_uv[pl][n * 16..n * 16 + n_coeffs],
                    );
                    total |= eob != 0;
                    self.eob_uv[pl][n] = eob as u16;
                    self.state.above_uv_nnz[pl][col + x] = u8::from(eob != 0);
                    self.left.uv_nnz[pl][row7 + y] = u8::from(eob != 0);
                    n += ustep * ustep;
                    x += ustep;
                }
                y += ustep;
            }
        }
        if uvtx > 0 {
            for pl in 0..2 {
                splat_nnz(&mut self.state.above_uv_nnz[pl], col, end_x_uv, ustep);
                splat_nnz(&mut self.left.uv_nnz[pl], row7, end_y_uv, ustep);
            }
        }

        Ok(total)
    }

    /// Intra reconstruction of the current block (prediction + residual).
    pub(super) fn intra_recon(&mut self) -> Result<(), KinetixError> {
        let col = self.col;
        let row = self.row;
        let bs = self.b.bs;
        let tx = self.b.tx;
        let stride = self.state.frame.stride;
        let (bw4, bh4) = crate::header::bwh(1, bs);
        let end_x = (bw4 << 1).min(2 * (self.cols() - col));
        let end_y = (bh4 << 1).min(2 * (self.rows() - row));
        let step1d = 1usize << tx;
        let step = 1usize << (tx * 2);
        let tx4 = if self.lossless { 4 } else { tx };
        let skip = self.b.skip;
        let trace = std::env::var_os("TPT_VP9_TRACE").is_some();

        let mut n = 0usize;
        let mut y = 0usize;
        while y < end_y {
            let mut x = 0usize;
            while x < end_x {
                let mode_idx = if bs > crate::frame::BS_8X8 && tx == 0 {
                    y * 2 + x
                } else {
                    0
                };
                let mode = self.b.mode[mode_idx];
                let hr = x * 4 + (4 << tx) < bw4 * 8;
                let (edges, mode) = gather_intra_edges(
                    &self.state.frame.y,
                    stride,
                    col * 8 + x * 4,
                    row * 8 + y * 4,
                    4 << tx,
                    mode,
                    row > 0 || y > 0,
                    col > self.tile_col_start || x > 0,
                    hr,
                    (self.cols() - col) * 8 - x * 4,
                    (self.rows() - row) * 8 - y * 4,
                );
                let off = (row * 8 + y * 4) * stride + col * 8 + x * 4;
                intra_predict(mode, &edges, &mut self.state.frame.y, off, stride);
                if trace {
                    eprintln!(
                        "EDGE hr={} above={} {} {} {} {} {} {} {} {} {}",
                        usize::from(hr),
                        edges.top[1],
                        edges.top[2],
                        edges.top[3],
                        edges.top[4],
                        edges.top[5],
                        edges.top[6],
                        edges.top[7],
                        edges.top[8],
                        edges.top[9],
                        edges.top[10]
                    );
                    let wsz = 4 << tx;
                    for r in 0..wsz {
                        let vals: Vec<String> = (0..wsz)
                            .map(|c| self.state.frame.y[off + r * stride + c].to_string())
                            .collect();
                        eprintln!(
                            "PREDP m={} tx={} x={} y={} ht={} hl={} row{}={}",
                            mode,
                            tx,
                            x * 4,
                            y * 4,
                            usize::from(row > 0 || y > 0),
                            usize::from(col > self.tile_col_start || x > 0),
                            r,
                            vals.join(" ")
                        );
                    }
                }
                let eob = usize::from(!skip) * self.eob_y[n] as usize;
                if trace && !skip {
                    let txtp = INTRA_TXFM_TYPE[self.b.mode[mode_idx]] as usize;
                    eprintln!(
                        "BLK mode={} tx_type={} plane=0 row={} col={}",
                        self.b.mode[mode_idx],
                        txtp,
                        y >> tx,
                        x >> tx
                    );
                }
                if eob != 0 {
                    let txtp = INTRA_TXFM_TYPE[self.b.mode[mode_idx]] as usize;
                    let sz = (4 << tx) * (4 << tx);
                    let coeffs: Vec<i32> = self.scratch_y[n * 16..n * 16 + sz].to_vec();
                    inverse_transform_add(
                        tx4,
                        txtp,
                        eob,
                        &coeffs,
                        &mut self.state.frame.y,
                        off,
                        stride,
                    );
                    if trace {
                        let wsz = 4 << tx;
                        for r in 0..wsz {
                            let vals: Vec<String> = (0..wsz)
                                .map(|c| self.state.frame.y[off + r * stride + c].to_string())
                                .collect();
                            eprintln!(
                                "PSTR tx={} x={} y={} row{}={}",
                                tx,
                                x * 4,
                                y * 4,
                                r,
                                vals.join(" ")
                            );
                        }
                    }
                }
                n += step;
                x += step1d;
            }
            y += step1d;
        }

        // chroma
        let uv_stride = stride >> 1;
        let mi_cols = self.cols();
        let mi_rows = self.rows();
        let end_x_uv = end_x >> 1;
        let end_y_uv = end_y >> 1;
        let ustep = 1usize << self.b.uvtx;
        let uvn = 1usize << (self.b.uvtx * 2);
        let uvtx4 = if self.lossless { 4 } else { self.b.uvtx };
        for pl in 0..2 {
            let mut n = 0usize;
            let mut y = 0usize;
            while y < end_y_uv {
                let mut x = 0usize;
                while x < end_x_uv {
                    let plane: &mut Vec<u8> = if pl == 0 {
                        &mut self.state.frame.u
                    } else {
                        &mut self.state.frame.v
                    };
                    let (edges, mode) = gather_intra_edges(
                        plane,
                        uv_stride,
                        col * 4 + x * 4,
                        row * 4 + y * 4,
                        4 << self.b.uvtx,
                        self.b.uvmode,
                        row > 0 || y > 0,
                        col > self.tile_col_start || x > 0,
                        x * 4 + (4 << self.b.uvtx) < bw4 * 4,
                        (mi_cols - col) * 4 - x * 4,
                        (mi_rows - row) * 4 - y * 4,
                    );
                    let off = (row * 4 + y * 4) * uv_stride + col * 4 + x * 4;
                    intra_predict(mode, &edges, plane, off, uv_stride);
                    let eob = usize::from(!skip) * self.eob_uv[pl][n] as usize;
                    if eob != 0 {
                        let sz = (4 << self.b.uvtx) * (4 << self.b.uvtx);
                        let coeffs: Vec<i32> = self.scratch_uv[pl][n * 16..n * 16 + sz].to_vec();
                        inverse_transform_add(uvtx4, DCT_DCT, eob, &coeffs, plane, off, uv_stride);
                    }
                    n += uvn;
                    x += ustep;
                }
                y += ustep;
            }
        }
        Ok(())
    }

    /// Inter reconstruction: motion compensation then the residual.
    pub(super) fn inter_recon(&mut self) -> Result<(), KinetixError> {
        self.inter_pred()?;

        if !self.b.skip {
            let col = self.col;
            let row = self.row;
            let bs = self.b.bs;
            let stride = self.state.frame.stride;
            let (bw4, bh4) = crate::header::bwh(1, bs);
            let end_x = (bw4 << 1).min(2 * (self.cols() - col));
            let end_y = (bh4 << 1).min(2 * (self.rows() - row));
            let step1d = 1usize << self.b.tx;
            let tx4 = if self.lossless { 4 } else { self.b.tx };
            let mut n = 0usize;
            let mut y = 0usize;
            while y < end_y {
                let mut x = 0usize;
                while x < end_x {
                    let eob = self.eob_y[n] as usize;
                    if eob != 0 {
                        let off = (row * 8 + y * 4) * stride + col * 8 + x * 4;
                        let sz = (4 << self.b.tx) * (4 << self.b.tx);
                        let coeffs: Vec<i32> = self.scratch_y[n * 16..n * 16 + sz].to_vec();
                        inverse_transform_add(
                            tx4,
                            DCT_DCT,
                            eob,
                            &coeffs,
                            &mut self.state.frame.y,
                            off,
                            stride,
                        );
                    }
                    n += step1d * step1d;
                    x += step1d;
                }
                y += step1d;
            }
            let uv_stride = stride >> 1;
            let end_x_uv = end_x >> 1;
            let end_y_uv = end_y >> 1;
            let ustep = 1usize << self.b.uvtx;
            let uvtx4 = if self.lossless { 4 } else { self.b.uvtx };
            for pl in 0..2 {
                let mut n = 0usize;
                let mut y = 0usize;
                while y < end_y_uv {
                    let mut x = 0usize;
                    while x < end_x_uv {
                        let eob = self.eob_uv[pl][n] as usize;
                        if eob != 0 {
                            let off = (row * 4 + y * 4) * uv_stride + col * 4 + x * 4;
                            let sz = (4 << self.b.uvtx) * (4 << self.b.uvtx);
                            let coeffs: Vec<i32> =
                                self.scratch_uv[pl][n * 16..n * 16 + sz].to_vec();
                            let plane: &mut Vec<u8> = if pl == 0 {
                                &mut self.state.frame.u
                            } else {
                                &mut self.state.frame.v
                            };
                            inverse_transform_add(
                                uvtx4, DCT_DCT, eob, &coeffs, plane, off, uv_stride,
                            );
                        }
                        n += ustep * ustep;
                        x += ustep;
                    }
                    y += ustep;
                }
            }
        }
        Ok(())
    }

    fn ref_frame_rc(&self, slot: u8) -> Result<Rc<crate::frame::FrameData>, KinetixError> {
        self.fctx.refs[slot as usize]
            .clone()
            .ok_or_else(|| KinetixError::Parse(format!("vp9: reference {slot} unavailable")))
    }

    fn mc_luma(
        &mut self,
        mv: Mv,
        frame_rc: &Rc<crate::frame::FrameData>,
        ref_slot: usize,
        px: usize,
        py: usize,
        bw: usize,
        bh: usize,
        avg: bool,
    ) -> Result<(), KinetixError> {
        let filter = FilterType(self.b.filter_type);
        let stride = self.state.frame.stride;
        let cur_w4 = self.cols() * 4;
        let cur_h4 = self.rows() * 4;
        let off = py * stride + px;
        let scale = self.fctx.mvscale[ref_slot];
        if scale[0] == 0xFFFF || (self.b.comp && scale[0] == 0xFFFF) {
            return Err(KinetixError::Unsupported(
                "vp9: reference frame has invalid dimensions for scaled MC".into(),
            ));
        }
        if scale[0] == 0 {
            mc_block(
                &mut self.state.frame.y,
                off,
                stride,
                &frame_rc.y,
                frame_rc.stride,
                frame_rc.mi_cols * 8,
                frame_rc.mi_rows * 8,
                px,
                py,
                mv,
                true,
                filter,
                bw,
                bh,
                avg,
            );
        } else {
            mc_block_scaled(
                &mut self.state.frame.y,
                off,
                stride,
                &frame_rc.y,
                frame_rc.stride,
                frame_rc.mi_cols * 8,
                frame_rc.mi_rows * 8,
                px,
                py,
                mv,
                true,
                filter,
                bw,
                bh,
                scale,
                self.fctx.mvstep[ref_slot],
                avg,
                cur_w4,
                cur_h4,
            );
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn mc_chroma(
        &mut self,
        mv: Mv,
        frame_rc: &Rc<crate::frame::FrameData>,
        ref_slot: usize,
        px_uv: usize,
        py_uv: usize,
        bw_uv: usize,
        bh_uv: usize,
        avg: bool,
    ) -> Result<(), KinetixError> {
        let filter = FilterType(self.b.filter_type);
        let uv_stride = self.state.frame.stride >> 1;
        let scale = self.fctx.mvscale[ref_slot];
        let cur_w4 = self.cols() * 4;
        let cur_h4 = self.rows() * 4;
        let mvstep = self.fctx.mvstep[ref_slot];
        let off = py_uv * uv_stride + px_uv;
        {
            let plane = &mut self.state.frame.u;
            if scale[0] == 0 {
                mc_block(
                    plane,
                    off,
                    uv_stride,
                    &frame_rc.u,
                    frame_rc.stride >> 1,
                    frame_rc.mi_cols * 4,
                    frame_rc.mi_rows * 4,
                    px_uv,
                    py_uv,
                    mv,
                    false,
                    filter,
                    bw_uv,
                    bh_uv,
                    avg,
                );
            } else {
                mc_block_scaled(
                    plane,
                    off,
                    uv_stride,
                    &frame_rc.u,
                    frame_rc.stride >> 1,
                    frame_rc.mi_cols * 4,
                    frame_rc.mi_rows * 4,
                    px_uv,
                    py_uv,
                    mv,
                    false,
                    filter,
                    bw_uv,
                    bh_uv,
                    scale,
                    mvstep,
                    avg,
                    cur_w4,
                    cur_h4,
                );
            }
        }
        {
            let plane = &mut self.state.frame.v;
            if scale[0] == 0 {
                mc_block(
                    plane,
                    off,
                    uv_stride,
                    &frame_rc.v,
                    frame_rc.stride >> 1,
                    frame_rc.mi_cols * 4,
                    frame_rc.mi_rows * 4,
                    px_uv,
                    py_uv,
                    mv,
                    false,
                    filter,
                    bw_uv,
                    bh_uv,
                    avg,
                );
            } else {
                mc_block_scaled(
                    plane,
                    off,
                    uv_stride,
                    &frame_rc.v,
                    frame_rc.stride >> 1,
                    frame_rc.mi_cols * 4,
                    frame_rc.mi_rows * 4,
                    px_uv,
                    py_uv,
                    mv,
                    false,
                    filter,
                    bw_uv,
                    bh_uv,
                    scale,
                    mvstep,
                    avg,
                    cur_w4,
                    cur_h4,
                );
            }
        }
        Ok(())
    }

    /// `inter_pred` (4:2:0 paths of the MC template).
    fn inter_pred(&mut self) -> Result<(), KinetixError> {
        let col = self.col;
        let row = self.row;
        let bs = self.b.bs;
        let px = col * 8;
        let py = row * 8;
        let tab = crate::tables::BWH_TAB;
        let bw = tab[bs * 2] as usize * 4;
        let bh = tab[bs * 2 + 1] as usize * 4;

        let n_refs = usize::from(self.b.comp) + 1;
        for r in 0..n_refs {
            let ref_slot = self.b.ref_[r] as usize;
            let frame_rc = self.ref_frame_rc(self.b.ref_[r])?;
            let avg = r == 1;

            if bs <= crate::frame::BS_8X8 {
                // one luma + one chroma MC at block granularity
                let mv = self.b.mv[0][r];
                self.mc_luma(mv, &frame_rc, ref_slot, px, py, bw, bh, avg)?;
                self.mc_chroma(
                    mv,
                    &frame_rc,
                    ref_slot,
                    px >> 1,
                    py >> 1,
                    bw >> 1,
                    bh >> 1,
                    avg,
                )?;
            } else {
                match bs {
                    10 => {
                        // BS_8x4: two 8x4 luma, one averaged chroma 4x2
                        self.mc_luma(self.b.mv[0][r], &frame_rc, ref_slot, px, py, 8, 4, avg)?;
                        self.mc_luma(self.b.mv[2][r], &frame_rc, ref_slot, px, py + 4, 8, 4, avg)?;
                        let uvmv = avg_mv2(self.b.mv[0][r], self.b.mv[2][r]);
                        self.mc_chroma(uvmv, &frame_rc, ref_slot, px >> 1, py >> 1, 4, 2, avg)?;
                    }
                    11 => {
                        // BS_4x8: two 4x8 luma, one averaged chroma 2x4
                        self.mc_luma(self.b.mv[0][r], &frame_rc, ref_slot, px, py, 4, 8, avg)?;
                        self.mc_luma(self.b.mv[1][r], &frame_rc, ref_slot, px + 4, py, 4, 8, avg)?;
                        let uvmv = avg_mv2(self.b.mv[0][r], self.b.mv[1][r]);
                        self.mc_chroma(uvmv, &frame_rc, ref_slot, px >> 1, py >> 1, 2, 4, avg)?;
                    }
                    _ => {
                        // BS_4x4: four 4x4 luma, one averaged chroma 2x2
                        self.mc_luma(self.b.mv[0][r], &frame_rc, ref_slot, px, py, 4, 4, avg)?;
                        self.mc_luma(self.b.mv[1][r], &frame_rc, ref_slot, px + 4, py, 4, 4, avg)?;
                        self.mc_luma(self.b.mv[2][r], &frame_rc, ref_slot, px, py + 4, 4, 4, avg)?;
                        self.mc_luma(
                            self.b.mv[3][r],
                            &frame_rc,
                            ref_slot,
                            px + 4,
                            py + 4,
                            4,
                            4,
                            avg,
                        )?;
                        let uvmv = avg_mv4(
                            self.b.mv[0][r],
                            self.b.mv[1][r],
                            self.b.mv[2][r],
                            self.b.mv[3][r],
                        );
                        self.mc_chroma(uvmv, &frame_rc, ref_slot, px >> 1, py >> 1, 2, 2, avg)?;
                    }
                }
            }
        }
        let _ = FrameType::Key;
        Ok(())
    }

    /// Record loop-filter level and per-unit info for the current block
    /// (the reference stores these in the mode-info grid, read later by
    /// `vp9_setup_mask`).
    pub(super) fn record_filter_edges(&mut self, w4: usize, h4: usize) {
        if self.hdr.loop_filter.level == 0 {
            return;
        }
        let seg = self.b.seg_id as usize;
        let lvl_idx = if self.b.intra {
            0
        } else {
            (self.b.ref_[0] as usize) + 1
        };
        let mode_idx = usize::from(self.b.mode[3] != 12); // != ZEROMV
        let lvl = self.seg.lflvl[seg][lvl_idx][mode_idx];
        let col7 = self.col & 7;
        let row7 = self.row7;
        let sb_index = (self.row / 8) * self.state.frame.sb64_cols + (self.col / 8);
        let mut sf = std::mem::take(&mut self.state.lflvl[sb_index]);
        let uvtx = crate::loop_filter::uv_txsize(self.b.bs, self.b.tx) as u8;
        let skip_inter = !self.b.intra && self.b.skip;
        let w = w4.min(8 - col7);
        let h = h4.min(8 - row7);
        for y in 0..h {
            for x in 0..w {
                let u = &mut sf.unit[(row7 + y) * 8 + col7 + x];
                u.bs = self.b.bs as u8;
                u.tx = self.b.tx as u8;
                u.uvtx = uvtx;
                u.skip_inter = skip_inter;
                u.lvl = lvl;
            }
        }
        self.state.lflvl[sb_index] = sf;
    }
}
