//! Lean frame reconstruction.
//!
//! This is the module that turns a decoded payload into pixels. The
//! reconstruction core is shared with `tpt-kinetix-lean`'s design (the same
//! prediction/transform/deblock math the realtime codec ports): a
//! fixed-shallow-partition block loop where each block is reconstructed as
//! `prediction + inverse_transform(dequant(residual))`, then the whole picture
//! is passed through the single-stage deblock filter.
//!
//! # Entropy coding
//!
//! Lean declares `num_rans_streams` in the sequence header for parallel
//! entropy decode. This module uses a **single** rANS stream per frame for v1
//! (the payload is one self-contained rANS-coded byte range carrying all
//! blocks in raster order: luma, then Cb, then Cr). Multi-stream interleaving
//! is the v2 extension point the `num_rans_streams` field reserves.
//!
//! # Honesty
//!
//! The reconstruction is real and runs end-to-end, but Lean is an original
//! codec with no external reference oracle, so [`crate::decoder::
//! LeanDecoder::capabilities`] keeps `pixel_exact` false. Round-trip safety
//! (encode → decode reproduces the encoded bytes' reconstruction) is covered
//! by the tests here.

use tpt_kinetix_bitstream::{RansDecoder, RansEncoder, StaticModel};
use tpt_kinetix_core::{
    error::KinetixError, frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp,
};

use crate::deblock::{deblock_chroma, deblock_luma, DeblockBlock};
use crate::headers::{ChromaFormat, FrameHeader, FrameType, SequenceHeader};
use crate::prediction::{
    chroma_subpel, predict_inter_luma, predict_intra_block, IntraMode, MotionVector,
};
use crate::transform::{dequant, inverse_2d_with_scratch, quant, transform_2d};

/// Substitution constant for unavailable intra-neighbour samples.
const R: i32 = 128;

/// Chroma subsampling factors (horizontal, vertical shift) per format.
pub fn chroma_subsampling(fmt: ChromaFormat) -> (usize, usize) {
    match fmt {
        ChromaFormat::Yuv420 => (1, 1),
        ChromaFormat::Yuv422 => (1, 0),
        ChromaFormat::Yuv444 => (0, 0),
    }
}

/// Chroma plane dimensions for a luma `w`×`h`.
pub fn chroma_dims(fmt: ChromaFormat, w: usize, h: usize) -> (usize, usize) {
    let (hs, vs) = chroma_subsampling(fmt);
    ((w + (1 << hs) - 1) >> hs, (h + (1 << vs) - 1) >> vs)
}

/// A reconstructed (or source) frame, planar YUV.
#[derive(Debug, Clone)]
pub struct FrameBuffer {
    pub width: usize,
    pub height: usize,
    pub format: ChromaFormat,
    pub luma: Vec<u8>,
    pub cb: Vec<u8>,
    pub cr: Vec<u8>,
    pub chroma_w: usize,
    pub chroma_h: usize,
}

impl FrameBuffer {
    pub fn new(seq: &SequenceHeader, frame: &FrameHeader) -> Self {
        let w = frame.width as usize;
        let h = frame.height as usize;
        let (cw, ch) = chroma_dims(seq.chroma_format, w, h);
        Self {
            width: w,
            height: h,
            format: seq.chroma_format,
            luma: vec![0u8; w * h],
            cb: vec![0u8; cw * ch],
            cr: vec![0u8; cw * ch],
            chroma_w: cw,
            chroma_h: ch,
        }
    }

    /// Build a frame buffer from already-packed planar YUV420p.
    pub fn from_yuv420(
        width: u32,
        height: u32,
        luma: Vec<u8>,
        cb: Vec<u8>,
        cr: Vec<u8>,
    ) -> Result<Self, KinetixError> {
        let (cw, ch) = chroma_dims(ChromaFormat::Yuv420, width as usize, height as usize);
        if luma.len() != width as usize * height as usize
            || cb.len() != cw * ch
            || cr.len() != cw * ch
        {
            return Err(KinetixError::Parse(
                "from_yuv420: buffer size mismatch".into(),
            ));
        }
        Ok(Self {
            width: width as usize,
            height: height as usize,
            format: ChromaFormat::Yuv420,
            luma,
            cb,
            cr,
            chroma_w: cw,
            chroma_h: ch,
        })
    }

    pub fn to_video_frame(&self, is_key: bool) -> VideoFrame {
        let mut data = Vec::with_capacity(self.luma.len() + self.cb.len() + self.cr.len());
        data.extend_from_slice(&self.luma);
        data.extend_from_slice(&self.cb);
        data.extend_from_slice(&self.cr);
        VideoFrame {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data,
            width: self.width as u32,
            height: self.height as u32,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: is_key,
        }
    }
}

/// The reconstructable shape of one block (luma or chroma).
#[derive(Debug, Clone, PartialEq)]
pub enum BlockSyntax {
    Intra {
        mode: u8,
        /// Quantised coefficients, raster order, first `num_coeff` positions.
        coeffs: Vec<i32>,
    },
    Inter {
        /// 0 = skip (zero MV, 0 bits), 1 = NEWMV, 2 = NEARESTMV.
        sub: u8,
        mv: MotionVector,
        coeffs: Vec<i32>,
    },
}

fn floor_div(a: i32, b: i32) -> i32 {
    let q = a / b;
    let r = a % b;
    if (r != 0) && ((r < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

// ---------------------------------------------------------------------------
// Per-block syntax encode / decode
// ---------------------------------------------------------------------------

fn write_i16(out: &mut Vec<u8>, v: i16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn read_i16(r: &mut &[u8]) -> Result<i16, KinetixError> {
    if r.len() < 2 {
        return Err(KinetixError::Parse("block syntax: truncated i16".into()));
    }
    let mut b = [0u8; 2];
    b.copy_from_slice(&r[0..2]);
    *r = &r[2..];
    Ok(i16::from_le_bytes(b))
}

fn write_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn read_i32(r: &mut &[u8]) -> Result<i32, KinetixError> {
    if r.len() < 4 {
        return Err(KinetixError::Parse("block syntax: truncated i32".into()));
    }
    let mut b = [0u8; 4];
    b.copy_from_slice(&r[0..4]);
    *r = &r[4..];
    Ok(i32::from_le_bytes(b))
}

fn write_block(out: &mut Vec<u8>, b: &BlockSyntax) {
    match b {
        BlockSyntax::Intra { mode, coeffs } => {
            out.push(0);
            out.push(*mode);
            out.push(coeffs.len() as u8);
            for &c in coeffs.iter() {
                write_i32(out, c);
            }
        }
        BlockSyntax::Inter { sub, mv, coeffs } => {
            out.push(1);
            out.push(*sub);
            if *sub != 0 {
                write_i16(out, mv.x as i16);
                write_i16(out, mv.y as i16);
            }
            out.push(coeffs.len() as u8);
            for &c in coeffs.iter() {
                write_i32(out, c);
            }
        }
    }
}

fn read_block(r: &mut &[u8]) -> Result<BlockSyntax, KinetixError> {
    if r.is_empty() {
        return Err(KinetixError::Parse("block syntax: empty block".into()));
    }
    let kind = r[0];
    *r = &r[1..];
    match kind {
        0 => {
            if r.is_empty() {
                return Err(KinetixError::Parse("intra block: missing mode".into()));
            }
            let mode = r[0];
            *r = &r[1..];
            let n = *r
                .first()
                .ok_or_else(|| KinetixError::Parse("intra block: missing coeff count".into()))?
                as usize;
            *r = &r[1..];
            let mut coeffs = Vec::with_capacity(n);
            for _ in 0..n {
                coeffs.push(read_i32(r)?);
            }
            Ok(BlockSyntax::Intra { mode, coeffs })
        }
        1 => {
            let sub = *r
                .first()
                .ok_or_else(|| KinetixError::Parse("inter block: missing sub".into()))?;
            *r = &r[1..];
            let mv = if sub == 0 {
                MotionVector::zero()
            } else {
                let x = read_i16(r)? as i32;
                let y = read_i16(r)? as i32;
                MotionVector::new(x, y)
            };
            let n = *r
                .first()
                .ok_or_else(|| KinetixError::Parse("inter block: missing coeff count".into()))?
                as usize;
            *r = &r[1..];
            let mut coeffs = Vec::with_capacity(n);
            for _ in 0..n {
                coeffs.push(read_i32(r)?);
            }
            Ok(BlockSyntax::Inter { sub, mv, coeffs })
        }
        other => Err(KinetixError::Parse(format!(
            "block syntax: unknown prediction kind {other}"
        ))),
    }
}

/// rANS-wrap a raw block byte stream (single-stream v1 entropy stage).
pub fn encode_frame_bytes(raw: &[u8]) -> Vec<u8> {
    let model = StaticModel;
    let mut enc = RansEncoder::new();
    for &s in raw.iter().rev() {
        enc.encode(&model, s);
    }
    enc.finish()
}

/// Reverse [`encode_frame_bytes`].
pub fn decode_frame_bytes(payload: &[u8]) -> Result<Vec<u8>, KinetixError> {
    let model = StaticModel;
    let mut dec = RansDecoder::new(payload)?;
    let mut out = Vec::with_capacity(payload.len().saturating_sub(4));
    let max = payload.len() * 4 + 1024;
    let mut guard = 0usize;
    while let Ok(s) = dec.decode(&model) {
        out.push(s);
        guard += 1;
        if guard > max {
            break;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Reconstruction
// ---------------------------------------------------------------------------

fn block_sizes(seq: &SequenceHeader) -> (usize, usize) {
    let luma_b = 1usize << seq.min_block_size_log2;
    let chroma_b = match seq.chroma_format {
        ChromaFormat::Yuv444 => luma_b,
        _ => (luma_b / 2).max(4),
    };
    (luma_b, chroma_b)
}

/// Reconstruct one frame from its decoded block syntax list.
///
/// `blocks` is the full block list in raster order: all luma blocks first,
/// then all Cb blocks, then all Cr blocks.
pub fn reconstruct_frame(
    seq: &SequenceHeader,
    frame: &FrameHeader,
    reference: Option<&FrameBuffer>,
    blocks: &[BlockSyntax],
) -> Result<FrameBuffer, KinetixError> {
    let mut fb = FrameBuffer::new(seq, frame);
    let (luma_b, chroma_b) = block_sizes(seq);
    let gw = fb.width.div_ceil(luma_b);
    let gh = fb.height.div_ceil(luma_b);
    let cgw = fb.chroma_w.div_ceil(chroma_b);
    let cgh = fb.chroma_h.div_ceil(chroma_b);
    let luma_total = gw * gh;
    let chroma_total = cgw * cgh;
    let qp = frame.base_qp as i32;

    let is_inter = frame.frame_type == FrameType::Inter;
    if is_inter && reference.is_none() {
        return Err(KinetixError::Parse(
            "lean inter frame without a reference".into(),
        ));
    }

    let mut luma_db = vec![DeblockBlock::intra(qp); luma_total.max(1)];
    let mut chroma_db = vec![DeblockBlock::intra(qp); chroma_total.max(1)];

    // One scratch set for the whole frame, sized for whichever plane has the
    // larger block (luma and chroma share it; chroma blocks are never larger).
    let mut scratch = BlockScratch::new(luma_b.max(chroma_b));

    // Luma.
    for (bi, db) in luma_db.iter_mut().enumerate().take(luma_total) {
        let sx = bi % gw;
        let sy = bi / gw;
        let block = &blocks[bi];
        *db = reconstruct_luma_block(&mut fb, reference, block, sx, sy, luma_b, qp, &mut scratch)?;
    }

    // Chroma (Cb, then Cr).
    let chroma_offset = luma_total;
    for plane_idx in 0..2usize {
        for (bi, db) in chroma_db.iter_mut().enumerate().take(chroma_total) {
            let sx = bi % cgw;
            let sy = bi / cgw;
            let idx = chroma_offset + plane_idx * chroma_total + bi;
            let block = blocks
                .get(idx)
                .ok_or_else(|| KinetixError::Parse("chroma block index out of range".into()))?;
            *db = reconstruct_chroma_block(
                &mut fb,
                reference,
                block,
                plane_idx,
                sx,
                sy,
                chroma_b,
                qp,
                &mut scratch,
            )?;
        }
    }

    // Single-stage in-loop deblock, applied after full reconstruction.
    deblock_luma(
        &mut fb.luma,
        fb.width,
        fb.width,
        fb.height,
        gw,
        gh,
        luma_b,
        &luma_db,
    );
    deblock_chroma(
        &mut fb.cb,
        fb.chroma_w,
        fb.chroma_w,
        fb.chroma_h,
        cgw,
        cgh,
        chroma_b,
        &chroma_db,
    );
    deblock_chroma(
        &mut fb.cr,
        fb.chroma_w,
        fb.chroma_w,
        fb.chroma_h,
        cgw,
        cgh,
        chroma_b,
        &chroma_db,
    );

    Ok(fb)
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_luma_block(
    fb: &mut FrameBuffer,
    reference: Option<&FrameBuffer>,
    block: &BlockSyntax,
    bx: usize,
    by: usize,
    b: usize,
    qp: i32,
    scratch: &mut BlockScratch,
) -> Result<DeblockBlock, KinetixError> {
    let x0 = bx * b;
    let y0 = by * b;
    // Split borrows: the prediction block and the neighbour rows are disjoint
    // fields of the same per-frame scratch.
    let BlockScratch {
        pred,
        above,
        left,
        coeffs,
        residual,
        tscratch,
    } = scratch;
    let db = match block {
        BlockSyntax::Intra { mode, .. } => {
            let above_left = neighbours_luma(fb, x0, y0, b, above, left);
            let m = IntraMode::from_u8(*mode).unwrap_or(IntraMode::Dc);
            predict_intra_block(
                &mut pred[..b * b],
                b,
                m,
                &above[..b],
                &left[..b],
                above_left,
            );
            DeblockBlock::intra(qp)
        }
        BlockSyntax::Inter { sub, mv, .. } => {
            let ref_ = reference.expect("inter without reference");
            predict_inter_luma(
                &mut pred[..b * b],
                b,
                &ref_.luma,
                ref_.width,
                ref_.width,
                ref_.height,
                x0,
                y0,
                *mv,
            );
            if *sub == 0 {
                DeblockBlock::inter(MotionVector::zero(), 0, qp)
            } else {
                DeblockBlock::inter(*mv, 0, qp)
            }
        }
    };
    add_residual(
        &mut fb.luma,
        fb.width,
        x0,
        y0,
        b,
        qp,
        block,
        &pred[..b * b],
        coeffs,
        residual,
        tscratch,
    );
    Ok(db)
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_chroma_block(
    fb: &mut FrameBuffer,
    reference: Option<&FrameBuffer>,
    block: &BlockSyntax,
    plane_idx: usize,
    bx: usize,
    by: usize,
    b: usize,
    qp: i32,
    scratch: &mut BlockScratch,
) -> Result<DeblockBlock, KinetixError> {
    let x0 = bx * b;
    let y0 = by * b;
    let BlockScratch {
        pred,
        above,
        left,
        coeffs,
        residual,
        tscratch,
    } = scratch;
    let db = match block {
        BlockSyntax::Intra { mode, .. } => {
            let above_left = neighbours_chroma(fb, x0, y0, b, above, left);
            let m = IntraMode::from_u8(*mode).unwrap_or(IntraMode::Dc);
            predict_intra_block(
                &mut pred[..b * b],
                b,
                m,
                &above[..b],
                &left[..b],
                above_left,
            );
            DeblockBlock::intra(qp)
        }
        BlockSyntax::Inter { sub, mv, .. } => {
            let ref_ = reference.expect("inter without reference");
            let ref_plane = if plane_idx == 0 { &ref_.cb } else { &ref_.cr };
            predict_chroma_block(
                &mut pred[..b * b],
                b,
                ref_plane,
                ref_.chroma_w,
                ref_.chroma_w,
                ref_.chroma_h,
                x0,
                y0,
                *mv,
            );
            if *sub == 0 {
                DeblockBlock::inter(MotionVector::zero(), 0, qp)
            } else {
                DeblockBlock::inter(*mv, 0, qp)
            }
        }
    };
    let plane = if plane_idx == 0 {
        &mut fb.cb
    } else {
        &mut fb.cr
    };
    add_residual(
        plane,
        fb.chroma_w,
        x0,
        y0,
        b,
        qp,
        block,
        &pred[..b * b],
        coeffs,
        residual,
        tscratch,
    );
    Ok(db)
}

#[allow(clippy::too_many_arguments)]
fn add_residual(
    plane: &mut [u8],
    stride: usize,
    x0: usize,
    y0: usize,
    b: usize,
    qp: i32,
    block: &BlockSyntax,
    pred: &[i32],
    coeffs: &mut [i32],
    residual: &mut [i32],
    tscratch: &mut [i32],
) {
    let n = b;
    // The buffers are reused across blocks, so clear them first: the original
    // code got a freshly zeroed `Vec` per block, and a short coefficient list
    // must still leave the tail of the block at zero.
    coeffs[..n * n].fill(0);
    let src = match block {
        BlockSyntax::Intra { coeffs, .. } => coeffs,
        BlockSyntax::Inter { coeffs, .. } => coeffs,
    };
    for (k, &c) in src.iter().enumerate().take(n * n) {
        coeffs[k] = dequant(c, qp as u8);
    }
    inverse_2d_with_scratch(&coeffs[..n * n], n, residual, tscratch);
    for r in 0..b {
        for c in 0..b {
            let px = x0 + c;
            let py = y0 + r;
            if px >= stride || py * stride + px >= plane.len() {
                continue;
            }
            let v = pred[r * n + c] + residual[r * n + c];
            plane[py * stride + px] = v.clamp(0, 255) as u8;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn predict_chroma_block(
    out: &mut [i32],
    b: usize,
    ref_plane: &[u8],
    ref_stride: usize,
    ref_w: usize,
    ref_h: usize,
    x0: usize,
    y0: usize,
    mv: MotionVector,
) {
    let base_x = x0 as i32 + floor_div(mv.x, 8);
    let base_y = y0 as i32 + floor_div(mv.y, 8);
    let ex = ((mv.x % 8) + 8) % 8;
    let ey = ((mv.y % 8) + 8) % 8;
    for r in 0..b {
        for c in 0..b {
            out[r * b + c] = chroma_subpel(
                ref_plane,
                ref_stride,
                base_x + c as i32,
                base_y + r as i32,
                ex,
                ey,
                ref_w as i32,
                ref_h as i32,
            );
        }
    }
}

/// Per-block scratch reused across a whole frame.
///
/// The reconstruction loop used to allocate a fresh `Vec` for the prediction
/// block and two more for the neighbour rows *for every block*, so a frame cost
/// `3 * block_count` heap allocations. One of these is created per frame
/// instead and threaded through.
///
/// Sized for the largest supported block (`MAX_BLOCK_SIZE` squared, plus a
/// row of neighbour samples). The sequence header parser rejects block sizes
/// above `MAX_BLOCK_SIZE_LOG2`, so the slice lengths below are always valid.
pub(crate) struct BlockScratch {
    /// `b * b` prediction samples for the current block.
    pub pred: Vec<i32>,
    /// `b` reconstructed samples above the block.
    pub above: Vec<i32>,
    /// `b` reconstructed samples left of the block.
    pub left: Vec<i32>,
    /// `b * b` dequantised coefficients.
    pub coeffs: Vec<i32>,
    /// `b * b` inverse-transform output (the residual).
    pub residual: Vec<i32>,
    /// `b * b` transform scratch. Must be distinct from both `coeffs` (the
    /// transform's source) and `residual` (its destination).
    pub tscratch: Vec<i32>,
}

impl BlockScratch {
    pub(crate) fn new(block: usize) -> Self {
        debug_assert!(block <= crate::headers::MAX_BLOCK_SIZE);
        Self {
            pred: vec![0; block * block],
            above: vec![R; block],
            left: vec![R; block],
            coeffs: vec![0; block * block],
            residual: vec![0; block * block],
            tscratch: vec![0; block * block],
        }
    }
}

fn neighbours_luma(
    fb: &FrameBuffer,
    x0: usize,
    y0: usize,
    b: usize,
    above: &mut [i32],
    left: &mut [i32],
) -> i32 {
    let stride = fb.width;
    above[..b].fill(R);
    left[..b].fill(R);
    let above_left = if x0 > 0 && y0 > 0 {
        fb.luma[(y0 - 1) * stride + (x0 - 1)] as i32
    } else {
        R
    };
    if y0 > 0 {
        for (c, above_c) in above.iter_mut().enumerate().take(b) {
            let x = x0 + c;
            if x < fb.width {
                *above_c = fb.luma[(y0 - 1) * stride + x] as i32;
            }
        }
    }
    if x0 > 0 {
        for (r, left_r) in left.iter_mut().enumerate().take(b) {
            let y = y0 + r;
            if y < fb.height {
                *left_r = fb.luma[y * stride + (x0 - 1)] as i32;
            }
        }
    }
    above_left
}

fn neighbours_chroma(
    fb: &FrameBuffer,
    x0: usize,
    y0: usize,
    b: usize,
    above: &mut [i32],
    left: &mut [i32],
) -> i32 {
    let stride = fb.chroma_w;
    above[..b].fill(R);
    left[..b].fill(R);
    let above_left = if x0 > 0 && y0 > 0 {
        fb.cb[(y0 - 1) * stride + (x0 - 1)] as i32
    } else {
        R
    };
    if y0 > 0 {
        for (c, above_c) in above.iter_mut().enumerate().take(b) {
            let x = x0 + c;
            if x < fb.chroma_w {
                *above_c = fb.cb[(y0 - 1) * stride + x] as i32;
            }
        }
    }
    if x0 > 0 {
        for (r, left_r) in left.iter_mut().enumerate().take(b) {
            let y = y0 + r;
            if y < fb.chroma_h {
                *left_r = fb.cb[y * stride + (x0 - 1)] as i32;
            }
        }
    }
    above_left
}

// ---------------------------------------------------------------------------
// Frame-level encode / decode entry points
// ---------------------------------------------------------------------------

/// Encode one frame into a single rANS payload (luma + Cb + Cr blocks in
/// raster order). The caller writes the frame header with the resulting
/// payload length.
pub fn encode_frame(
    seq: &SequenceHeader,
    frame: &FrameHeader,
    src: &FrameBuffer,
    reference: Option<&FrameBuffer>,
) -> Result<Vec<u8>, KinetixError> {
    let (luma_b, chroma_b) = block_sizes(seq);
    let gw = src.width.div_ceil(luma_b);
    let gh = src.height.div_ceil(luma_b);
    let cgw = src.chroma_w.div_ceil(chroma_b);
    let cgh = src.chroma_h.div_ceil(chroma_b);
    let luma_total = gw * gh;
    let chroma_total = cgw * cgh;
    let is_inter = frame.frame_type == FrameType::Inter;

    let mut luma_syntax = Vec::with_capacity(luma_total);
    // One scratch set for the whole frame; shared by the luma and chroma loops
    // and reused for every block and every intra mode trial.
    let mut scratch = EncodeScratch::new(luma_b.max(chroma_b));

    for bi in 0..luma_total {
        let sx = bi % gw;
        let sy = bi / gw;
        luma_syntax.push(encode_luma_block(
            src,
            reference,
            sx,
            sy,
            luma_b,
            frame.base_qp,
            is_inter,
            &mut scratch,
        )?);
    }
    let mut cb_syntax = Vec::with_capacity(chroma_total);
    let mut cr_syntax = Vec::with_capacity(chroma_total);
    for bi in 0..chroma_total {
        let sx = bi % cgw;
        let sy = bi / cgw;
        cb_syntax.push(encode_chroma_block(
            src,
            reference,
            0,
            sx,
            sy,
            chroma_b,
            frame.base_qp,
            is_inter,
            &mut scratch,
        )?);
        cr_syntax.push(encode_chroma_block(
            src,
            reference,
            1,
            sx,
            sy,
            chroma_b,
            frame.base_qp,
            is_inter,
            &mut scratch,
        )?);
    }

    let mut raw = Vec::new();
    for b in &luma_syntax {
        write_block(&mut raw, b);
    }
    for b in &cb_syntax {
        write_block(&mut raw, b);
    }
    for b in &cr_syntax {
        write_block(&mut raw, b);
    }
    Ok(encode_frame_bytes(&raw))
}

#[allow(clippy::too_many_arguments)]
fn encode_luma_block(
    src: &FrameBuffer,
    reference: Option<&FrameBuffer>,
    bx: usize,
    by: usize,
    b: usize,
    qp: u8,
    is_inter: bool,
    s: &mut EncodeScratch,
) -> Result<BlockSyntax, KinetixError> {
    let stride = src.width;
    let x0 = bx * b;
    let y0 = by * b;
    let n = b * b;
    for r in 0..b {
        for c in 0..b {
            s.orig[r * b + c] = src.luma[(y0 + r) * stride + (x0 + c)] as i32;
        }
    }

    if is_inter {
        if let Some(ref_) = reference {
            predict_inter_luma(
                &mut s.pred[..n],
                b,
                &ref_.luma,
                ref_.width,
                ref_.width,
                ref_.height,
                x0,
                y0,
                MotionVector::zero(),
            );
            encode_residual_into(
                &s.orig[..n],
                &s.pred[..n],
                b,
                qp,
                &mut s.residual,
                &mut s.transformed,
                &mut s.coeffs,
            );
            let err = residual_error(
                &s.orig[..n],
                &s.pred[..n],
                &s.coeffs,
                b,
                qp,
                &mut s.full,
                &mut s.back,
                &mut s.tscratch,
                &mut s.recon,
            );
            let sub = u8::from(err != 0);
            return Ok(BlockSyntax::Inter {
                sub,
                mv: MotionVector::zero(),
                coeffs: s.coeffs.clone(),
            });
        }
    }

    let above_left = neighbours_luma(src, x0, y0, b, &mut s.above, &mut s.left);
    let mut best_mode = IntraMode::Dc;
    let mut best_coeffs: Vec<i32> = Vec::new();
    let mut best_err = i64::MAX;
    for m in 0..crate::prediction::NUM_INTRA_MODES {
        let mode = IntraMode::from_u8(m).unwrap();
        predict_intra_block(
            &mut s.pred[..n],
            b,
            mode,
            &s.above[..b],
            &s.left[..b],
            above_left,
        );
        encode_residual_into(
            &s.orig[..n],
            &s.pred[..n],
            b,
            qp,
            &mut s.residual,
            &mut s.transformed,
            &mut s.coeffs,
        );
        let err = residual_error(
            &s.orig[..n],
            &s.pred[..n],
            &s.coeffs,
            b,
            qp,
            &mut s.full,
            &mut s.back,
            &mut s.tscratch,
            &mut s.recon,
        );
        if err < best_err {
            best_err = err;
            best_mode = mode;
            // Only cloned when a mode actually wins, so the copy cost is
            // amortised over the whole mode search.
            best_coeffs.clear();
            best_coeffs.extend_from_slice(&s.coeffs);
        }
    }
    Ok(BlockSyntax::Intra {
        mode: best_mode as u8,
        coeffs: best_coeffs,
    })
}

#[allow(clippy::too_many_arguments)]
fn encode_chroma_block(
    src: &FrameBuffer,
    reference: Option<&FrameBuffer>,
    plane_idx: usize,
    bx: usize,
    by: usize,
    b: usize,
    qp: u8,
    is_inter: bool,
    s: &mut EncodeScratch,
) -> Result<BlockSyntax, KinetixError> {
    let stride = src.chroma_w;
    let x0 = bx * b;
    let y0 = by * b;
    let n = b * b;
    let plane = if plane_idx == 0 { &src.cb } else { &src.cr };
    for r in 0..b {
        for c in 0..b {
            s.orig[r * b + c] = plane[(y0 + r) * stride + (x0 + c)] as i32;
        }
    }
    if is_inter {
        if let Some(ref_) = reference {
            let ref_plane = if plane_idx == 0 { &ref_.cb } else { &ref_.cr };
            predict_chroma_block(
                &mut s.pred[..n],
                b,
                ref_plane,
                ref_.chroma_w,
                ref_.chroma_w,
                ref_.chroma_h,
                x0,
                y0,
                MotionVector::zero(),
            );
            encode_residual_into(
                &s.orig[..n],
                &s.pred[..n],
                b,
                qp,
                &mut s.residual,
                &mut s.transformed,
                &mut s.coeffs,
            );
            let err = residual_error(
                &s.orig[..n],
                &s.pred[..n],
                &s.coeffs,
                b,
                qp,
                &mut s.full,
                &mut s.back,
                &mut s.tscratch,
                &mut s.recon,
            );
            if err == 0 {
                return Ok(BlockSyntax::Inter {
                    sub: 0,
                    mv: MotionVector::zero(),
                    coeffs: s.coeffs.clone(),
                });
            }
        }
    }
    let above_left = neighbours_chroma(src, x0, y0, b, &mut s.above, &mut s.left);
    let mut best_mode = IntraMode::Dc;
    let mut best_coeffs: Vec<i32> = Vec::new();
    let mut best_err = i64::MAX;
    for m in 0..crate::prediction::NUM_INTRA_MODES {
        let mode = IntraMode::from_u8(m).unwrap();
        predict_intra_block(
            &mut s.pred[..n],
            b,
            mode,
            &s.above[..b],
            &s.left[..b],
            above_left,
        );
        encode_residual_into(
            &s.orig[..n],
            &s.pred[..n],
            b,
            qp,
            &mut s.residual,
            &mut s.transformed,
            &mut s.coeffs,
        );
        let err = residual_error(
            &s.orig[..n],
            &s.pred[..n],
            &s.coeffs,
            b,
            qp,
            &mut s.full,
            &mut s.back,
            &mut s.tscratch,
            &mut s.recon,
        );
        if err < best_err {
            best_err = err;
            best_mode = mode;
            best_coeffs.clear();
            best_coeffs.extend_from_slice(&s.coeffs);
        }
    }
    Ok(BlockSyntax::Intra {
        mode: best_mode as u8,
        coeffs: best_coeffs,
    })
}

/// Per-frame scratch for the encoder's mode search.
///
/// The encoder trials all 14 intra modes for every block, and each trial used
/// to allocate ~6 `Vec`s (prediction, residual, transformed, coefficients,
/// dequantised coefficients, reconstruction). At 14 modes per block that is
/// ~84 heap allocations per block, which is what made lean encode the slowest
/// path in the crate. Every buffer below is allocated once per frame and
/// reused.
pub(crate) struct EncodeScratch {
    /// Source block samples.
    pub orig: Vec<i32>,
    /// Prediction for the mode currently being trialled.
    pub pred: Vec<i32>,
    /// `orig - pred`.
    pub residual: Vec<i32>,
    /// Forward-transform output.
    pub transformed: Vec<i32>,
    /// Dequantised coefficients.
    pub full: Vec<i32>,
    /// Inverse-transform output.
    pub back: Vec<i32>,
    /// Transform scratch.
    pub tscratch: Vec<i32>,
    /// Reconstructed block.
    pub recon: Vec<i32>,
    /// `b` samples above the block.
    pub above: Vec<i32>,
    /// `b` samples left of the block.
    pub left: Vec<i32>,
    /// Coefficient list for the mode being trialled (reused; cloned only when
    /// a mode turns out to be the best so far).
    pub coeffs: Vec<i32>,
}

impl EncodeScratch {
    pub(crate) fn new(block: usize) -> Self {
        debug_assert!(block <= crate::headers::MAX_BLOCK_SIZE);
        let n = block * block;
        Self {
            orig: vec![0; n],
            pred: vec![0; n],
            residual: vec![0; n],
            transformed: vec![0; n],
            full: vec![0; n],
            back: vec![0; n],
            tscratch: vec![0; n],
            recon: vec![0; n],
            above: vec![R; block],
            left: vec![R; block],
            coeffs: Vec::with_capacity(n),
        }
    }
}

/// Transform `orig - pred`, quantise, and write the (trimmed) coefficient list
/// into `out` (cleared first, so it can be reused across mode trials).
fn encode_residual_into(
    orig: &[i32],
    pred: &[i32],
    b: usize,
    qp: u8,
    residual: &mut [i32],
    transformed: &mut [i32],
    out: &mut Vec<i32>,
) {
    let n = b * b;
    for i in 0..n {
        residual[i] = orig[i] - pred[i];
    }
    transform_2d(&residual[..n], b, transformed);
    out.clear();
    out.reserve(n);
    let mut last = 0;
    for (i, &t) in transformed[..n].iter().enumerate() {
        let q = quant(t, qp);
        out.push(q);
        if q != 0 {
            last = i + 1;
        }
    }
    out.truncate(last);
}

/// Reconstruct `pred + residual(coeffs)` into `out`, reusing every buffer.
/// `out` is fully overwritten.
#[allow(clippy::too_many_arguments)]
fn apply_reconstruct_into(
    pred: &[i32],
    coeffs: &[i32],
    b: usize,
    qp: u8,
    full: &mut [i32],
    back: &mut [i32],
    tscratch: &mut [i32],
    out: &mut [i32],
) {
    let n = b * b;
    full[..n].fill(0);
    for (k, &c) in coeffs.iter().enumerate().take(n) {
        full[k] = dequant(c, qp);
    }
    inverse_2d_with_scratch(&full[..n], b, back, tscratch);
    for i in 0..n {
        out[i] = (pred[i] + back[i]).clamp(0, 255);
    }
}

/// Max absolute reconstruction error, computed without allocating.
#[allow(clippy::too_many_arguments)]
fn residual_error(
    orig: &[i32],
    pred: &[i32],
    coeffs: &[i32],
    b: usize,
    qp: u8,
    full: &mut [i32],
    back: &mut [i32],
    tscratch: &mut [i32],
    recon: &mut [i32],
) -> i64 {
    let n = b * b;
    apply_reconstruct_into(pred, coeffs, b, qp, full, back, tscratch, recon);
    orig[..n]
        .iter()
        .zip(recon[..n].iter())
        .map(|(a, r)| (a - r).abs() as i64)
        .max()
        .unwrap_or(0)
}

/// Decode a frame from its rANS payload into a [`FrameBuffer`].
pub fn decode_frame_payload(
    seq: &SequenceHeader,
    frame: &FrameHeader,
    reference: Option<&FrameBuffer>,
    payload: &[u8],
) -> Result<FrameBuffer, KinetixError> {
    let raw = decode_frame_bytes(payload)?;
    let mut r = raw.as_slice();
    let mut blocks = Vec::new();
    while !r.is_empty() {
        blocks.push(read_block(&mut r)?);
    }
    reconstruct_frame(seq, frame, reference, &blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq() -> SequenceHeader {
        SequenceHeader {
            version: 1,
            max_width: 1920,
            max_height: 1080,
            max_ref_frames: 4,
            min_block_size_log2: 3,
            max_block_size_log2: 3,
            bit_depth: 8,
            chroma_format: ChromaFormat::Yuv420,
            num_rans_streams: 1,
        }
    }

    fn key_frame() -> FrameHeader {
        FrameHeader {
            frame_type: FrameType::Key,
            width: 16,
            height: 16,
            base_qp: 0,
            ref_frame_count: 0,
            payload_len: 0,
        }
    }

    #[test]
    fn block_syntax_round_trips() {
        let blocks = vec![
            BlockSyntax::Intra {
                mode: 5,
                coeffs: vec![12, -3, 0, 7],
            },
            BlockSyntax::Inter {
                sub: 1,
                mv: MotionVector::new(4, -8),
                coeffs: vec![],
            },
            BlockSyntax::Inter {
                sub: 0,
                mv: MotionVector::zero(),
                coeffs: vec![],
            },
        ];
        let mut raw = Vec::new();
        for b in &blocks {
            write_block(&mut raw, b);
        }
        let mut r = raw.as_slice();
        let mut got = Vec::new();
        while !r.is_empty() {
            got.push(read_block(&mut r).unwrap());
        }
        assert_eq!(got, blocks);
    }

    #[test]
    fn frame_rans_round_trips() {
        let blocks = vec![BlockSyntax::Intra {
            mode: 1,
            coeffs: vec![1, 2, 3, -4, 5],
        }];
        let mut raw = Vec::new();
        for b in &blocks {
            write_block(&mut raw, b);
        }
        let wrapped = encode_frame_bytes(&raw);
        let unwrapped = decode_frame_bytes(&wrapped).unwrap();
        assert_eq!(raw, unwrapped);
    }

    #[test]
    fn keyframe_round_trips_at_qp0() {
        let s = seq();
        let f = key_frame();
        let mut luma = vec![0u8; 16 * 16];
        let mut cb = vec![0u8; 8 * 8];
        let mut cr = vec![0u8; 8 * 8];
        for y in 0..16 {
            for x in 0..16 {
                luma[y * 16 + x] = ((x + y) * 8) as u8;
            }
        }
        for y in 0..8 {
            for x in 0..8 {
                cb[y * 8 + x] = (x * 16) as u8;
                cr[y * 8 + x] = (y * 16) as u8;
            }
        }
        let src = FrameBuffer::from_yuv420(16, 16, luma.clone(), cb.clone(), cr.clone()).unwrap();
        let payload = encode_frame(&s, &f, &src, None).unwrap();
        let decoded = decode_frame_payload(&s, &f, None, &payload).unwrap();
        assert_eq!(decoded.luma, luma, "luma must round-trip at qp=0");
        assert_eq!(decoded.cb, cb);
        assert_eq!(decoded.cr, cr);
    }

    #[test]
    fn inter_skip_round_trips_identical_frames() {
        let s = seq();
        let f = key_frame();
        let mut luma = vec![0u8; 16 * 16];
        for y in 0..16 {
            for x in 0..16 {
                luma[y * 16 + x] = ((x * 7 + y * 3) & 0xFF) as u8;
            }
        }
        let cb = vec![100u8; 8 * 8];
        let cr = vec![50u8; 8 * 8];
        let src = FrameBuffer::from_yuv420(16, 16, luma.clone(), cb.clone(), cr.clone()).unwrap();
        let ref_ = src.clone();

        let mut pf = f;
        pf.frame_type = FrameType::Inter;
        pf.ref_frame_count = 1;
        let payload = encode_frame(&s, &pf, &src, Some(&ref_)).unwrap();
        let decoded = decode_frame_payload(&s, &pf, Some(&ref_), &payload).unwrap();
        assert_eq!(decoded.luma, luma);
        assert_eq!(decoded.cb, cb);
        assert_eq!(decoded.cr, cr);
    }

    /// FNV-1a 64-bit, used to pin golden vectors without storing large blobs.
    fn fnv1a(chunks: &[&[u8]]) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for c in chunks {
            for &b in *c {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x100000001b3);
            }
        }
        h
    }

    /// Deterministic synthetic 32x32 4:2:0 content: gradient + LCG noise.
    fn golden_source(seed: u32) -> FrameBuffer {
        let mut state = seed;
        let mut next = move || {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 24) as u8
        };
        let luma: Vec<u8> = (0..32 * 32)
            .map(|i| ((i % 32) * 4 + (i / 32) * 2) as u8 / 2 + next() / 4)
            .collect();
        let cb: Vec<u8> = (0..16 * 16)
            .map(|i| 96 + (i % 16) as u8 * 3 + next() / 16)
            .collect();
        let cr: Vec<u8> = (0..16 * 16)
            .map(|i| 160 - (i / 16) as u8 * 3 + next() / 16)
            .collect();
        FrameBuffer::from_yuv420(32, 32, luma, cb, cr).unwrap()
    }

    /// Golden vector: pins the exact encoded bytes and decoded pixels (lossy QP,
    /// key + inter). Any change here is a bitstream/output change and must be
    /// deliberate: update the constants and note it in the changelog.
    #[test]
    fn golden_vector_pins_bitstream_and_output() {
        let s = seq();
        let key = FrameHeader {
            frame_type: FrameType::Key,
            width: 32,
            height: 32,
            base_qp: 12,
            ref_frame_count: 0,
            payload_len: 0,
        };
        let inter = FrameHeader {
            frame_type: FrameType::Inter,
            width: 32,
            height: 32,
            base_qp: 12,
            ref_frame_count: 1,
            payload_len: 0,
        };
        let src0 = golden_source(1);
        let src1 = golden_source(2);
        let key_payload = encode_frame(&s, &key, &src0, None).unwrap();
        let key_dec = decode_frame_payload(&s, &key, None, &key_payload).unwrap();
        let inter_payload = encode_frame(&s, &inter, &src1, Some(&key_dec)).unwrap();
        let inter_dec = decode_frame_payload(&s, &inter, Some(&key_dec), &inter_payload).unwrap();
        let got = [
            fnv1a(&[&key_payload]),
            fnv1a(&[&key_dec.luma, &key_dec.cb, &key_dec.cr]),
            fnv1a(&[&inter_payload]),
            fnv1a(&[&inter_dec.luma, &inter_dec.cb, &inter_dec.cr]),
        ];
        eprintln!("GOLDEN {got:#x?}");
        assert_eq!(got, GOLDEN);
    }

    const GOLDEN: [u64; 4] = [
        0xf7cab705bedd8db5,
        0x740512f05fec7f5f,
        0xa72b86dd13a79cf8,
        0xf7284f3fbb505436,
    ];
}
