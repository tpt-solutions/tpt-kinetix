//! Tile and superblock decode (§6.4/§8): partition trees, block state,
//! per-block context caches and the scan-order tables' indirection.
//! The mode parse lives in [`crate::frame_mode`], reconstruction in
//! [`crate::frame_recon`], and the loop-filter level/mask bookkeeping is
//! shared with [`crate::loop_filter`].
//!
//! Structure mirrors the reference decoder: an above-context set spanning the
//! frame width plus per-tile left caches, per-superblock loop-filter level and
//! edge masks, and a per-8px MV/reference grid used by MV prediction.

use std::rc::Rc;

use tpt_kinetix_core::error::KinetixError;

use crate::booldec::BoolDecoder;
use crate::header::{bwh, FrameHeader, FrameType, SegFeatures, WorkingProbs};
use crate::mv::{read_tree, MvrefPair};
use crate::predict::Mv;
use crate::tables::{
    BWH_TAB, DEFAULT_KF_PARTITION_PROBS, DEFAULT_SCAN_16X16, DEFAULT_SCAN_16X16_NB,
    DEFAULT_SCAN_32X32, DEFAULT_SCAN_32X32_NB, DEFAULT_SCAN_4X4, DEFAULT_SCAN_4X4_NB,
    DEFAULT_SCAN_8X8, DEFAULT_SCAN_8X8_NB, PARTITION_TREE,
};

pub const BL_64X64: usize = 0;
pub const BL_32X32: usize = 1;
pub const BL_16X16: usize = 2;
pub const BL_8X8: usize = 3;

/// Partition tree leaves (§8.2.2).
pub const PARTITION_NONE: usize = 0;
pub const PARTITION_H: usize = 1;
pub const PARTITION_V: usize = 2;
pub const PARTITION_SPLIT: usize = 3;

/// Sub-8x8 block-size indices (`block_level * 3 + block_partition` order).
pub const BS_8X8: usize = 9;
pub const BS_8X4: usize = 10;
pub const BS_4X8: usize = 11;
pub const BS_4X4: usize = 12;

/// Max transform size per block size (reference `max_tx_for_bl_bp`).
pub(crate) const MAX_TX_FOR_BS: [usize; 13] = [3, 3, 3, 3, 2, 2, 2, 1, 1, 1, 0, 0, 0];

/// Size group for inter-frame intra blocks (y-mode prob group).
pub(crate) const INTRA_SIZE_GROUP: [usize; 10] = [3, 3, 3, 3, 2, 2, 2, 1, 1, 1];

/// Inter-mode context LUT (`inter_mode_ctx_lut[above][left]`; cache values
/// < 10 are intra, 10..13 the inter modes).
pub(crate) const INTER_MODE_CTX_LUT: [[u8; 14]; 14] = [
    [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
    [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
    [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
    [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
    [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
    [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
    [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
    [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
    [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
    [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
    [5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 2, 2, 1, 3],
    [5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 2, 2, 1, 3],
    [5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 1, 1, 0, 3],
    [5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 3, 3, 3, 4],
];

/// Sub-block neighbour offset for the <=8x8 inter-mode context.
pub(crate) const INTER_MODE_OFF: [usize; 10] = [3, 0, 0, 1, 0, 0, 0, 0, 0, 0];

/// Partition-context update tables (`above_ctx` / `left_ctx`).
pub(crate) const PART_ABOVE_CTX: [u8; 13] = [
    0x0, 0x0, 0x8, 0x8, 0x8, 0xc, 0xc, 0xc, 0xe, 0xe, 0xe, 0xf, 0xf,
];
pub(crate) const PART_LEFT_CTX: [u8; 13] = [
    0x0, 0x8, 0x0, 0x8, 0xc, 0x8, 0xc, 0xe, 0xc, 0xe, 0xf, 0xe, 0xf,
];

/// Per-tx-size coefficient band occupancy (reference `band_counts`).
pub(crate) const BAND_COUNTS: [[i16; 6]; 4] = [
    [1, 2, 3, 4, 3, 3],
    [1, 2, 3, 4, 11, 43],
    [1, 2, 3, 4, 11, 235],
    [1, 2, 3, 4, 11, 1003],
];

/// Scan order selection: `ff_vp9_scans[tx + 4*lossless][tx_type]` with the
/// paired neighbour table. `tx_type`: 0=DCT_DCT, 1=ADST_DCT, 2=DCT_ADST,
/// 3=ADST_ADST.
pub(crate) fn scans(tx4: usize, tx_type: usize) -> (&'static [i16], &'static [i16]) {
    match (tx4, tx_type) {
        (0, 1) => (
            &crate::tables::COL_SCAN_4X4,
            &crate::tables::COL_SCAN_4X4_NB,
        ),
        (0, 2) => (
            &crate::tables::ROW_SCAN_4X4,
            &crate::tables::ROW_SCAN_4X4_NB,
        ),
        (1, 1) => (
            &crate::tables::COL_SCAN_8X8,
            &crate::tables::COL_SCAN_8X8_NB,
        ),
        (1, 2) => (
            &crate::tables::ROW_SCAN_8X8,
            &crate::tables::ROW_SCAN_8X8_NB,
        ),
        (2, 1) => (
            &crate::tables::COL_SCAN_16X16,
            &crate::tables::COL_SCAN_16X16_NB,
        ),
        (2, 2) => (
            &crate::tables::ROW_SCAN_16X16,
            &crate::tables::ROW_SCAN_16X16_NB,
        ),
        // lossless row (4): WHT always uses the default 4x4 scan
        (4, _) => (DEFAULT_SCAN_4X4.as_slice(), DEFAULT_SCAN_4X4_NB.as_slice()),
        (0, _) => (DEFAULT_SCAN_4X4.as_slice(), DEFAULT_SCAN_4X4_NB.as_slice()),
        (1, _) => (DEFAULT_SCAN_8X8.as_slice(), DEFAULT_SCAN_8X8_NB.as_slice()),
        (2, _) => (
            DEFAULT_SCAN_16X16.as_slice(),
            DEFAULT_SCAN_16X16_NB.as_slice(),
        ),
        _ => (
            DEFAULT_SCAN_32X32.as_slice(),
            DEFAULT_SCAN_32X32_NB.as_slice(),
        ),
    }
}

/// A reconstructed frame: padded planar YUV plus the per-8px metadata grids.
pub struct FrameData {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    /// Padded plane stride / height (visible + block-overhang padding).
    pub stride: usize,
    pub buf_h: usize,
    pub width: u32,
    pub height: u32,
    pub mi_cols: usize,
    pub mi_rows: usize,
    pub sb64_cols: usize,
    /// Per-8px segmentation id map (`mi_rows * seg_stride`).
    pub segmap: Vec<u8>,
    pub seg_stride: usize,
    /// Per-8px MV/reference grid (same geometry as `segmap`).
    pub mvrefs: Vec<MvrefPair>,
}

impl FrameData {
    pub fn new(width: u32, height: u32) -> Self {
        let mi_cols = (width as usize).div_ceil(8);
        let mi_rows = (height as usize).div_ceil(8);
        let sb64_cols = mi_cols.div_ceil(8);
        let _sb64_rows = mi_rows.div_ceil(8);
        // Generous padding so block overhangs and 8-tap filter reads never
        // leave the plane.
        let stride = mi_cols * 8 + 64;
        let buf_h = mi_rows * 8 + 64;
        let seg_stride = sb64_cols * 8;
        let grid_len = seg_stride * mi_rows;
        Self {
            y: vec![0; stride * buf_h],
            u: vec![0; (stride >> 1) * (buf_h >> 1)],
            v: vec![0; (stride >> 1) * (buf_h >> 1)],
            stride,
            buf_h,
            width,
            height,
            mi_cols,
            mi_rows,
            sb64_cols,
            segmap: vec![0; grid_len],
            seg_stride,
            mvrefs: vec![MvrefPair::default(); grid_len],
        }
    }

    pub fn sb64_rows(&self) -> usize {
        self.mi_rows.div_ceil(8)
    }
}

/// Per-superblock loop-filter level and edge masks (reference `VP9Filter`).
#[derive(Clone)]
pub struct SbFilter {
    pub level: [u8; 64],
    /// `[plane 0=y 1=uv][0=col 1=row][8 rows][4 width classes]` masks.
    pub mask: [[[[u8; 4]; 8]; 2]; 2],
}

impl Default for SbFilter {
    fn default() -> Self {
        Self {
            level: [0; 64],
            mask: [[[[0; 4]; 8]; 2]; 2],
        }
    }
}

/// Frame-scoped above-context caches (shared by all tiles, each tile owning
/// its column range).
pub struct FrameState {
    pub frame: FrameData,
    pub above_partition_ctx: Vec<u8>,
    pub above_mode_ctx: Vec<u8>,
    pub above_y_nnz: Vec<u8>,
    pub above_uv_nnz: [Vec<u8>; 2],
    pub above_skip_ctx: Vec<u8>,
    pub above_txfm_ctx: Vec<u8>,
    pub above_segpred_ctx: Vec<u8>,
    pub above_intra_ctx: Vec<u8>,
    pub above_comp_ctx: Vec<u8>,
    pub above_ref_ctx: Vec<u8>,
    pub above_filter_ctx: Vec<u8>,
    pub above_mv_ctx: Vec<[Mv; 2]>,
    pub lflvl: Vec<SbFilter>,
}

impl FrameState {
    pub fn new(hdr: &FrameHeader) -> Self {
        let cols = hdr.mi_cols;
        let frame = FrameData::new(hdr.width, hdr.height);
        let n_sb = frame.sb64_cols * frame.sb64_rows();
        Self {
            frame,
            above_partition_ctx: vec![0; cols],
            above_mode_ctx: vec![0; cols * 2],
            above_y_nnz: vec![0; cols * 2],
            above_uv_nnz: [vec![0; cols], vec![0; cols]],
            above_skip_ctx: vec![0; cols],
            above_txfm_ctx: vec![0; cols],
            above_segpred_ctx: vec![0; cols],
            above_intra_ctx: vec![0; cols],
            above_comp_ctx: vec![0; cols],
            above_ref_ctx: vec![0; cols],
            above_filter_ctx: vec![0; cols],
            above_mv_ctx: vec![Default::default(); cols * 2],
            lflvl: (0..n_sb).map(|_| SbFilter::default()).collect(),
        }
    }
}

/// Per-tile left-context caches.
#[derive(Default)]
pub struct LeftCtx {
    pub y_nnz: [u8; 16],
    pub mode: [u8; 16],
    pub mv: [[Mv; 2]; 16],
    pub uv_nnz: [[u8; 16]; 2],
    pub partition: [u8; 8],
    pub skip: [u8; 8],
    pub txfm: [u8; 8],
    pub segpred: [u8; 8],
    pub intra: [u8; 8],
    pub comp: [u8; 8],
    pub ref_: [u8; 8],
    pub filter: [u8; 8],
}

/// Adaptation statistics gathered across a frame (reference `counts`).
#[derive(Default)]
pub struct Counts {
    pub y_mode: [[u32; 10]; 4],
    pub uv_mode: [[u32; 10]; 10],
    pub filter: [[u32; 3]; 4],
    pub mv_mode: [[u32; 4]; 7],
    pub intra: [[u32; 2]; 4],
    pub comp: [[u32; 2]; 5],
    pub single_ref: [[[u32; 2]; 2]; 5],
    pub comp_ref: [[u32; 2]; 5],
    pub tx32p: [[u32; 4]; 2],
    pub tx16p: [[u32; 3]; 2],
    pub tx8p: [[u32; 2]; 2],
    pub skip: [[u32; 2]; 3],
    pub mv_joint: [u32; 4],
    pub mv_comp: [MvCompCounts; 2],
    pub partition: [[[u32; 4]; 4]; 4],
    /// Per-context coefficient bins (528 coded contexts, 3 / 2 bins each).
    pub coef: Vec<[u32; 3]>,
    pub eob: Vec<[u32; 2]>,
}

#[derive(Default, Clone)]
pub struct MvCompCounts {
    pub sign: [u32; 2],
    pub classes: [u32; 11],
    pub class0: [u32; 2],
    pub bits: [[u32; 2]; 10],
    pub class0_fp: [[u32; 4]; 2],
    pub fp: [u32; 4],
    pub class0_hp: [u32; 2],
    pub hp: [u32; 2],
}

impl Counts {
    pub fn new() -> Self {
        Self {
            coef: vec![[0; 3]; 16 * 33],
            eob: vec![[0; 2]; 16 * 33],
            ..Default::default()
        }
    }

    /// Context index for the coefficient count bins (compact band-0 layout).
    #[inline]
    pub fn coef_bin(tx: usize, bt: usize, pt: usize, band: usize, ctx: usize) -> usize {
        debug_assert!(!(band == 0 && ctx >= 3));
        (tx * 4 + bt * 2 + pt) * 33
            + if band == 0 {
                ctx
            } else {
                3 + (band - 1) * 6 + ctx
            }
    }
}

/// Everything the tile decoder needs that lives beyond one frame.
pub struct FrameDecodeCtx<'a> {
    pub refs: &'a [Option<Rc<FrameData>>; 8],
    /// Previous decoded frame's MV grid (`REF_FRAME_MVPAIR`).
    pub mvpair: Option<&'a [MvrefPair]>,
    pub mvpair_w: usize,
    /// Previous decoded frame's segmentation map (`REF_FRAME_SEGMAP`).
    pub prev_segmap: Option<&'a [u8]>,
    pub prev_segmap_w: usize,
    /// Per-reference MV scaling for resized references
    /// (0 = unscaled, 0xFFFF = invalid).
    pub mvscale: [[u16; 2]; 3],
    pub mvstep: [[u8; 2]; 3],
    /// `fixcompref` / `varcompref` derived from sign biases.
    pub fixcompref: usize,
    pub varcompref: [usize; 2],
}

/// One decoded block's parse results (reference `VP9Block`).
#[derive(Default, Clone)]
pub struct BlockInfo {
    pub seg_id: u8,
    pub skip: bool,
    pub intra: bool,
    pub comp: bool,
    pub ref_: [u8; 2],
    pub mode: [usize; 4],
    pub uvmode: usize,
    pub filter: usize,
    pub filter_type: usize,
    pub mv: [[Mv; 2]; 4],
    pub tx: usize,
    pub uvtx: usize,
    pub bs: usize,
}

/// Tile decoder: parses and reconstructs one tile column region.
pub struct TileDecoder<'a> {
    pub hdr: &'a FrameHeader,
    pub probs: &'a WorkingProbs,
    pub seg: &'a SegFeatures,
    pub fctx: FrameDecodeCtx<'a>,
    pub state: &'a mut FrameState,
    pub counts: &'a mut Counts,
    pub lossless: bool,
    pub tile_col_start: usize,
    pub tile_col_end: usize,
    pub tile_row_start: usize,
    pub tile_row_end: usize,
    pub left: LeftCtx,
    pub b: BlockInfo,
    pub row: usize,
    pub col: usize,
    pub row7: usize,
    pub min_mv: Mv,
    pub max_mv: Mv,
    /// Dequantized coefficient scratch (one 32x32 block).
    pub scratch_y: Vec<i32>,
    pub scratch_uv: [Vec<i32>; 2],
    /// Per-transform-block EOBs (luma: up to 256 4px sub-blocks).
    pub eob_y: Vec<u16>,
    pub eob_uv: [Vec<u16>; 2],
}

impl<'a> TileDecoder<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        hdr: &'a FrameHeader,
        probs: &'a WorkingProbs,
        seg: &'a SegFeatures,
        fctx: FrameDecodeCtx<'a>,
        state: &'a mut FrameState,
        counts: &'a mut Counts,
        tile_col_start: usize,
        tile_col_end: usize,
    ) -> Self {
        Self {
            hdr,
            probs,
            seg,
            fctx,
            state,
            counts,
            lossless: hdr.lossless,
            tile_col_start,
            tile_col_end,
            tile_row_start: 0,
            tile_row_end: usize::MAX,
            left: LeftCtx::default(),
            b: BlockInfo::default(),
            row: 0,
            col: 0,
            row7: 0,
            min_mv: Mv::zero(),
            max_mv: Mv::zero(),
            // a 64x64 block spans 256 4px sub-blocks (4096 coefficients);
            // chroma half that per plane for 4:2:0
            scratch_y: vec![0; 4096],
            scratch_uv: [vec![0; 1024], vec![0; 1024]],
            eob_y: vec![0; 256],
            eob_uv: [vec![0; 64], vec![0; 64]],
        }
    }

    pub fn cols(&self) -> usize {
        self.hdr.mi_cols
    }

    pub fn rows(&self) -> usize {
        self.hdr.mi_rows
    }

    /// Decode all superblocks of this tile with the given bool decoder.
    pub fn decode_tile(&mut self, bc: &mut BoolDecoder) -> Result<(), KinetixError> {
        let sb_end = (self.tile_col_end / 8).min(self.state.frame.sb64_cols);
        let row_end = (self.tile_row_end / 8).min(self.state.frame.sb64_rows());
        for sb_row in (self.tile_row_start / 8)..row_end {
            for sb_col in (self.tile_col_start / 8)..sb_end {
                self.decode_sb(bc, sb_row * 8, sb_col * 8, BL_64X64)?;
            }
        }
        Ok(())
    }

    fn decode_sb(
        &mut self,
        bc: &mut BoolDecoder,
        row: usize,
        col: usize,
        bl: usize,
    ) -> Result<(), KinetixError> {
        self.row = row;
        self.row7 = row & 7;
        self.col = col;
        let hbs = 4usize >> bl; // in 8px units
        let c = (((self.state.above_partition_ctx[col] >> (3 - bl)) & 1)
            | (((self.left.partition[self.row7] >> (3 - bl)) & 1) << 1)) as usize;
        let p: [u8; 3] = if self.hdr.frame_type == FrameType::Key || self.hdr.intra_only {
            let base = (bl * 4 + c) * 3;
            [
                DEFAULT_KF_PARTITION_PROBS[base],
                DEFAULT_KF_PARTITION_PROBS[base + 1],
                DEFAULT_KF_PARTITION_PROBS[base + 2],
            ]
        } else {
            self.probs.mode.partition[bl][c]
        };

        if bl == BL_8X8 {
            let bp = read_tree(bc, &PARTITION_TREE, &p);
            self.counts.partition[bl][c][bp] += 1;
            self.decode_block(bc, row, col, bl, bp)
        } else if col + hbs < self.cols() {
            if row + hbs < self.rows() {
                let bp = read_tree(bc, &PARTITION_TREE, &p);
                self.counts.partition[bl][c][bp] += 1;
                match bp {
                    PARTITION_NONE => self.decode_block(bc, row, col, bl, bp),
                    PARTITION_H => {
                        self.decode_block(bc, row, col, bl, bp)?;
                        self.decode_block(bc, row + hbs, col, bl, bp)
                    }
                    PARTITION_V => {
                        self.decode_block(bc, row, col, bl, bp)?;
                        self.decode_block(bc, row, col + hbs, bl, bp)
                    }
                    _ => {
                        self.decode_sb(bc, row, col, bl + 1)?;
                        self.decode_sb(bc, row, col + hbs, bl + 1)?;
                        self.decode_sb(bc, row + hbs, col, bl + 1)?;
                        self.decode_sb(bc, row + hbs, col + hbs, bl + 1)
                    }
                }
            } else if bc.read_bool(p[1]) {
                self.counts.partition[bl][c][PARTITION_SPLIT] += 1;
                self.decode_sb(bc, row, col, bl + 1)?;
                self.decode_sb(bc, row, col + hbs, bl + 1)
            } else {
                self.counts.partition[bl][c][PARTITION_H] += 1;
                self.decode_block(bc, row, col, bl, PARTITION_H)
            }
        } else if row + hbs < self.rows() {
            if bc.read_bool(p[2]) {
                self.counts.partition[bl][c][PARTITION_SPLIT] += 1;
                self.decode_sb(bc, row, col, bl + 1)?;
                self.decode_sb(bc, row + hbs, col, bl + 1)
            } else {
                self.counts.partition[bl][c][PARTITION_V] += 1;
                self.decode_block(bc, row, col, bl, PARTITION_V)
            }
        } else {
            self.counts.partition[bl][c][PARTITION_SPLIT] += 1;
            self.decode_sb(bc, row, col, bl + 1)
        }
    }

    fn decode_block(
        &mut self,
        bc: &mut BoolDecoder,
        row: usize,
        col: usize,
        bl: usize,
        bp: usize,
    ) -> Result<(), KinetixError> {
        let trace_start_bits = bc.bits_consumed();
        self.row = row;
        self.row7 = row & 7;
        self.col = col;
        let bs = bl * 3 + bp;
        self.b.bs = bs;
        self.b.mv = [[Mv::zero(); 2]; 4];
        let (w4, h4) = bwh(1, bs); // 8px units

        // MV clamp bounds for this block position.
        self.min_mv = Mv {
            x: (-(128 + (col * 64) as i32)) as i16,
            y: (-(128 + (row * 64) as i32)) as i16,
        };
        // blocks may overhang the visible frame at odd sizes: keep signed
        self.max_mv = Mv {
            x: (128 + (self.cols() as i32 - col as i32 - w4 as i32) * 64) as i16,
            y: (128 + (self.rows() as i32 - row as i32 - h4 as i32) * 64) as i16,
        };

        self.decode_mode(bc)?;

        // chroma tx size
        let ss_dec = ((self.hdr.subsampling_x != 0 && w4 * 2 == (1 << self.b.tx))
            || (self.hdr.subsampling_y != 0 && h4 * 2 == (1 << self.b.tx)))
            as usize;
        self.b.uvtx = self.b.tx.saturating_sub(ss_dec);

        if !self.b.skip {
            let has_coeffs = self.decode_block_coeffs(bc)?;
            if !has_coeffs && bs <= BS_8X8 && !self.b.intra {
                self.b.skip = true;
                for i in 0..w4 {
                    self.state.above_skip_ctx[col + i] = 1;
                }
                for i in 0..h4 {
                    self.left.skip[self.row7 + i] = 1;
                }
            }
        } else {
            for i in 0..w4 {
                let a = (col + i) * 2;
                self.state.above_y_nnz[a] = 0;
                self.state.above_y_nnz[a + 1] = 0;
                self.state.above_uv_nnz[0][col + i] = 0;
                self.state.above_uv_nnz[1][col + i] = 0;
            }
            for i in 0..h4 {
                let l = (self.row7 + i) * 2;
                self.left.y_nnz[l] = 0;
                self.left.y_nnz[l + 1] = 0;
                self.left.uv_nnz[0][self.row7 + i] = 0;
                self.left.uv_nnz[1][self.row7 + i] = 0;
            }
        }

        if self.b.intra {
            self.intra_recon()?;
        } else {
            self.inter_recon()?;
        }

        self.record_filter_edges(w4, h4);

        if std::env::var("TPT_VP9_TRACE").is_ok() {
            eprintln!(
                "TRACE block r{row} c{col} bs={bs} skip={} intra={} modes={:?} uv={} tx={} seg={} bytes={}..{}",
                self.b.skip,
                self.b.intra,
                self.b.mode,
                self.b.uvmode,
                self.b.tx,
                self.b.seg_id,
                trace_start_bits / 8,
                bc.bits_consumed() / 8
            );
        }

        // left/above MV cache update (inter frames only)
        if self.hdr.frame_type != FrameType::Key && !self.hdr.intra_only {
            if bs > BS_8X8 {
                let mv0 = self.b.mv[3];
                self.left.mv[self.row7 * 2] = self.b.mv[1];
                self.left.mv[self.row7 * 2 + 1] = mv0;
                self.state.above_mv_ctx[col * 2] = self.b.mv[2];
                self.state.above_mv_ctx[col * 2 + 1] = mv0;
            } else {
                let mv0 = self.b.mv[3];
                for n in 0..w4 * 2 {
                    self.state.above_mv_ctx[col * 2 + n] = mv0;
                }
                for n in 0..h4 * 2 {
                    self.left.mv[self.row7 * 2 + n] = mv0;
                }
            }
        }
        Ok(())
    }
}

/// OR-reduce the nnz context arrays over each transform-block group before
/// decoding (reference `MERGE_CTX`).
pub(crate) fn merge_nnz(arr: &mut [u8], off: usize, end: usize, step: usize) {
    let mut n = 0;
    while n < end {
        let v = arr[off + n..off + n + step].iter().any(|&x| x != 0);
        arr[off + n] = u8::from(v);
        n += step;
    }
}

/// Propagate each group's first context over the group after decoding
/// (reference `SPLAT_CTX`).
pub(crate) fn splat_nnz(arr: &mut [u8], off: usize, end: usize, step: usize) {
    let mut n = 0;
    while n < end {
        let v = arr[off + n];
        for x in 1..step {
            if n + x < end {
                arr[off + n + x] = v;
            }
        }
        n += step;
    }
}

/// Keep `BWH_TAB` referenced from this module (used via `bwh`).
#[allow(dead_code)]
fn _bwh_tab_len() -> usize {
    BWH_TAB.len()
}
