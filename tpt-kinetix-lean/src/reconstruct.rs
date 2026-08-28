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
use crate::transform::{dequant, inverse_2d, quant, transform_2d};

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

    // Luma.
    for (bi, db) in luma_db.iter_mut().enumerate().take(luma_total) {
        let sx = bi % gw;
        let sy = bi / gw;
        let block = &blocks[bi];
        *db = reconstruct_luma_block(&mut fb, reference, block, sx, sy, luma_b, qp)?;
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
                &mut fb, reference, block, plane_idx, sx, sy, chroma_b, qp,
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
) -> Result<DeblockBlock, KinetixError> {
    let x0 = bx * b;
    let y0 = by * b;
    let mut pred = vec![0i32; b * b];
    let db = match block {
        BlockSyntax::Intra { mode, .. } => {
            let (above, left, above_left) = neighbours_luma(fb, x0, y0, b);
            let m = IntraMode::from_u8(*mode).unwrap_or(IntraMode::Dc);
            predict_intra_block(&mut pred, b, m, &above, &left, above_left);
            DeblockBlock::intra(qp)
        }
        BlockSyntax::Inter { sub, mv, .. } => {
            let ref_ = reference.expect("inter without reference");
            predict_inter_luma(
                &mut pred,
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
    add_residual(&mut fb.luma, fb.width, x0, y0, b, qp, block, &pred);
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
) -> Result<DeblockBlock, KinetixError> {
    let x0 = bx * b;
    let y0 = by * b;
    let mut pred = vec![0i32; b * b];
    let db = match block {
        BlockSyntax::Intra { mode, .. } => {
            let (above, left, above_left) = neighbours_chroma(fb, x0, y0, b);
            let m = IntraMode::from_u8(*mode).unwrap_or(IntraMode::Dc);
            predict_intra_block(&mut pred, b, m, &above, &left, above_left);
            DeblockBlock::intra(qp)
        }
        BlockSyntax::Inter { sub, mv, .. } => {
            let ref_ = reference.expect("inter without reference");
            let ref_plane = if plane_idx == 0 { &ref_.cb } else { &ref_.cr };
            predict_chroma_block(
                &mut pred,
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
    add_residual(plane, fb.chroma_w, x0, y0, b, qp, block, &pred);
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
) {
    let n = b;
    let mut coeffs = vec![0i32; n * n];
    let src = match block {
        BlockSyntax::Intra { coeffs, .. } => coeffs,
        BlockSyntax::Inter { coeffs, .. } => coeffs,
    };
    for (k, &c) in src.iter().enumerate() {
        if k >= coeffs.len() {
            break;
        }
        coeffs[k] = dequant(c, qp as u8);
    }
    let mut residual = vec![0i32; n * n];
    inverse_2d(&coeffs, n, &mut residual);
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

fn neighbours_luma(fb: &FrameBuffer, x0: usize, y0: usize, b: usize) -> (Vec<i32>, Vec<i32>, i32) {
    let stride = fb.width;
    let mut above = vec![R; b];
    let mut left = vec![R; b];
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
    (above, left, above_left)
}

fn neighbours_chroma(
    fb: &FrameBuffer,
    x0: usize,
    y0: usize,
    b: usize,
) -> (Vec<i32>, Vec<i32>, i32) {
    let stride = fb.chroma_w;
    let mut above = vec![R; b];
    let mut left = vec![R; b];
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
    (above, left, above_left)
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
    for bi in 0..luma_total {
        let sx = bi % gw;
        let sy = bi / gw;
        luma_syntax.push(encode_luma_block(src, reference, sx, sy, luma_b, frame.base_qp, is_inter)?);
    }
    let mut cb_syntax = Vec::with_capacity(chroma_total);
    let mut cr_syntax = Vec::with_capacity(chroma_total);
    for bi in 0..chroma_total {
        let sx = bi % cgw;
        let sy = bi / cgw;
        cb_syntax.push(encode_chroma_block(src, reference, 0, sx, sy, chroma_b, frame.base_qp, is_inter)?);
        cr_syntax.push(encode_chroma_block(src, reference, 1, sx, sy, chroma_b, frame.base_qp, is_inter)?);
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
) -> Result<BlockSyntax, KinetixError> {
    let stride = src.width;
    let x0 = bx * b;
    let y0 = by * b;
    let n = b * b;
    let mut orig = vec![0i32; n];
    for r in 0..b {
        for c in 0..b {
            orig[r * b + c] = src.luma[(y0 + r) * stride + (x0 + c)] as i32;
        }
    }

    if is_inter {
        if let Some(ref_) = reference {
            let mut pred = vec![0i32; n];
            predict_inter_luma(
                &mut pred,
                b,
                &ref_.luma,
                ref_.width,
                ref_.width,
                ref_.height,
                x0,
                y0,
                MotionVector::zero(),
            );
            let coeffs = encode_residual(&orig, &pred, b, qp);
            if coeffs_is_lossless(&orig, &pred, &coeffs, b, qp) {
                return Ok(BlockSyntax::Inter {
                    sub: 0,
                    mv: MotionVector::zero(),
                    coeffs,
                });
            }
            return Ok(BlockSyntax::Inter {
                sub: 1,
                mv: MotionVector::zero(),
                coeffs,
            });
        }
    }

    let (above, left, above_left) = neighbours_luma(src, x0, y0, b);
    let mut best_mode = IntraMode::Dc;
    let mut best_coeffs = vec![];
    let mut best_err = i64::MAX;
    for m in 0..crate::prediction::NUM_INTRA_MODES {
        let mode = IntraMode::from_u8(m).unwrap();
        let mut pred = vec![0i32; n];
        predict_intra_block(&mut pred, b, mode, &above, &left, above_left);
        let coeffs = encode_residual(&orig, &pred, b, qp);
        let err = residual_error(&orig, &pred, &coeffs, b, qp);
        if err < best_err {
            best_err = err;
            best_mode = mode;
            best_coeffs = coeffs;
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
) -> Result<BlockSyntax, KinetixError> {
    let stride = src.chroma_w;
    let x0 = bx * b;
    let y0 = by * b;
    let n = b * b;
    let plane = if plane_idx == 0 { &src.cb } else { &src.cr };
    let mut orig = vec![0i32; n];
    for r in 0..b {
        for c in 0..b {
            orig[r * b + c] = plane[(y0 + r) * stride + (x0 + c)] as i32;
        }
    }
    if is_inter {
        if let Some(ref_) = reference {
            let ref_plane = if plane_idx == 0 { &ref_.cb } else { &ref_.cr };
            let mut pred = vec![0i32; n];
            predict_chroma_block(
                &mut pred,
                b,
                ref_plane,
                ref_.chroma_w,
                ref_.chroma_w,
                ref_.chroma_h,
                x0,
                y0,
                MotionVector::zero(),
            );
            let coeffs = encode_residual(&orig, &pred, b, qp);
            if coeffs_is_lossless(&orig, &pred, &coeffs, b, qp) {
                return Ok(BlockSyntax::Inter {
                    sub: 0,
                    mv: MotionVector::zero(),
                    coeffs,
                });
            }
        }
    }
    let (above, left, above_left) = neighbours_chroma(src, x0, y0, b);
    let mut best_mode = IntraMode::Dc;
    let mut best_coeffs = vec![];
    let mut best_err = i64::MAX;
    for m in 0..crate::prediction::NUM_INTRA_MODES {
        let mode = IntraMode::from_u8(m).unwrap();
        let mut pred = vec![0i32; n];
        predict_intra_block(&mut pred, b, mode, &above, &left, above_left);
        let coeffs = encode_residual(&orig, &pred, b, qp);
        let err = residual_error(&orig, &pred, &coeffs, b, qp);
        if err < best_err {
            best_err = err;
            best_mode = mode;
            best_coeffs = coeffs;
        }
    }
    Ok(BlockSyntax::Intra {
        mode: best_mode as u8,
        coeffs: best_coeffs,
    })
}

/// Transform `orig - pred`, quantise, and return the (trimmed) coefficient
/// list.
fn encode_residual(orig: &[i32], pred: &[i32], b: usize, qp: u8) -> Vec<i32> {
    let n = b * b;
    let mut residual = vec![0i32; n];
    for i in 0..n {
        residual[i] = orig[i] - pred[i];
    }
    let mut transformed = vec![0i32; n];
    transform_2d(&residual, b, &mut transformed);
    let mut coeffs = Vec::with_capacity(n);
    let mut last = 0;
    for (i, &t) in transformed.iter().enumerate() {
        let q = quant(t, qp);
        coeffs.push(q);
        if q != 0 {
            last = i + 1;
        }
    }
    coeffs.truncate(last);
    coeffs
}

fn apply_reconstruct(pred: &[i32], coeffs: &[i32], b: usize, qp: u8) -> Vec<i32> {
    let n = b * b;
    let mut full = vec![0i32; n];
    for (k, &c) in coeffs.iter().enumerate() {
        if k >= n {
            break;
        }
        full[k] = dequant(c, qp);
    }
    let mut residual = vec![0i32; n];
    inverse_2d(&full, b, &mut residual);
    let mut out = vec![0i32; n];
    for i in 0..n {
        out[i] = (pred[i] + residual[i]).clamp(0, 255);
    }
    out
}

fn coeffs_is_lossless(orig: &[i32], pred: &[i32], coeffs: &[i32], b: usize, qp: u8) -> bool {
    residual_error(orig, pred, coeffs, b, qp) == 0
}

fn residual_error(orig: &[i32], pred: &[i32], coeffs: &[i32], b: usize, qp: u8) -> i64 {
    let recon = apply_reconstruct(pred, coeffs, b, qp);
    orig.iter()
        .zip(recon.iter())
        .map(|(a, b)| (a - b).abs() as i64)
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
}
