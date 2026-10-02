//! Frame reconstruction + dual-path (tensor/pixel) decode.

#![allow(clippy::too_many_arguments)]

use tpt_kinetix_bitstream::{RansDecoder, RansEncoder, StaticModel};
use tpt_kinetix_core::{
    error::KinetixError, frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp,
};

use crate::deblock::{deblock_chroma, deblock_luma, DeblockBlock};
use crate::headers::{ChromaFormat, FrameHeader, FrameType, SequenceHeader};
use crate::prediction::{predict_inter_luma, predict_intra_block, IntraMode, MotionVector};
use crate::quant::{dequantize, matrix_pos, quant_matrix, quantize};
use crate::transform::{inverse_2d_with_scratch, transform_2d};
use crate::Tensor;

const R: i32 = 128;

pub fn chroma_subsampling(fmt: ChromaFormat) -> (usize, usize) {
    match fmt {
        ChromaFormat::Yuv420 => (1, 1),
        ChromaFormat::Yuv422 => (1, 0),
        ChromaFormat::Yuv444 => (0, 0),
    }
}

pub fn chroma_dims(fmt: ChromaFormat, w: usize, h: usize) -> (usize, usize) {
    let (hs, vs) = chroma_subsampling(fmt);
    ((w + (1 << hs) - 1) >> hs, (h + (1 << vs) - 1) >> vs)
}

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
    pub fn new(_seq: &SequenceHeader, frame: &FrameHeader) -> Self {
        let w = frame.width as usize;
        let h = frame.height as usize;
        let (cw, ch) = chroma_dims(ChromaFormat::Yuv420, w, h);
        Self {
            width: w,
            height: h,
            format: ChromaFormat::Yuv420,
            luma: vec![0u8; w * h],
            // Neutral mid-gray so luma-only streams (chroma_present == false)
            // reconstruct to a valid all-neutral chroma plane.
            cb: vec![128u8; cw * ch],
            cr: vec![128u8; cw * ch],
            chroma_w: cw,
            chroma_h: ch,
        }
    }

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

#[derive(Debug, Clone, PartialEq)]
pub enum BlockSyntax {
    Intra {
        mode: u8,
        coeffs: Vec<i32>,
    },
    Inter {
        sub: u8,
        mv: MotionVector,
        coeffs: Vec<i32>,
    },
}

fn block_sizes(seq: &SequenceHeader) -> (usize, usize) {
    let luma_b = 1usize << seq.min_block_size_log2;
    let chroma_b = (luma_b / 2).max(4);
    (luma_b, chroma_b)
}

fn write_i16(out: &mut Vec<u8>, v: i16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn read_i16(r: &mut &[u8]) -> Result<i16, KinetixError> {
    if r.len() < 2 {
        return Err(KinetixError::Parse("truncated i16".into()));
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
        return Err(KinetixError::Parse("truncated i32".into()));
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
        return Err(KinetixError::Parse("empty block".into()));
    }
    let kind = r[0];
    *r = &r[1..];
    match kind {
        0 => {
            if r.is_empty() {
                return Err(KinetixError::Parse("intra: missing mode".into()));
            }
            let mode = r[0];
            *r = &r[1..];
            let n = *r
                .first()
                .ok_or_else(|| KinetixError::Parse("intra: missing coeff count".into()))?
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
                .ok_or_else(|| KinetixError::Parse("inter: missing sub".into()))?;
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
                .ok_or_else(|| KinetixError::Parse("inter: missing coeff count".into()))?
                as usize;
            *r = &r[1..];
            let mut coeffs = Vec::with_capacity(n);
            for _ in 0..n {
                coeffs.push(read_i32(r)?);
            }
            Ok(BlockSyntax::Inter { sub, mv, coeffs })
        }
        other => Err(KinetixError::Parse(format!(
            "unknown prediction kind {other}"
        ))),
    }
}

pub fn encode_frame_bytes(raw: &[u8]) -> Vec<u8> {
    let model = StaticModel;
    let mut enc = RansEncoder::new();
    for &s in raw.iter().rev() {
        enc.encode(&model, s);
    }
    enc.finish()
}

pub fn decode_frame_bytes(payload: &[u8]) -> Result<Vec<u8>, KinetixError> {
    let model = StaticModel;
    let mut dec = RansDecoder::new(payload)?;
    let max = payload.len() * 4 + 1024;
    let mut out = Vec::new();
    let mut guard = 0;
    while let Ok(s) = dec.decode(&model) {
        out.push(s);
        guard += 1;
        if guard > max {
            break;
        }
    }
    Ok(out)
}

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
    let chroma_coded = seq.chroma_present;
    let qp = frame.base_qp as i32;
    let is_inter = frame.frame_type == FrameType::Inter;
    if is_inter && reference.is_none() {
        return Err(KinetixError::Parse(
            "vision inter frame without reference".into(),
        ));
    }
    // Exact block-count contract: a truncated or over-long block list is a
    // parse error, never an index panic (checked before any block access).
    let expected = luma_total + if chroma_coded { 2 * chroma_total } else { 0 };
    if blocks.len() != expected {
        return Err(KinetixError::Parse(format!(
            "vision payload has {} blocks, expected {expected}",
            blocks.len()
        )));
    }

    let matrix = quant_matrix(seq.quant_matrix_id);
    let mut luma_db = vec![DeblockBlock::intra(qp); luma_total.max(1)];
    let mut chroma_db = vec![DeblockBlock::intra(qp); chroma_total.max(1)];

    // One scratch set for the whole frame, shared by the luma and chroma loops.
    let mut scratch = BlockScratch::new(luma_b.max(chroma_b));

    for (bi, db) in luma_db.iter_mut().enumerate().take(luma_total) {
        let sx = bi % gw;
        let sy = bi / gw;
        *db = reconstruct_luma_block(
            &mut fb,
            reference,
            &blocks[bi],
            sx,
            sy,
            luma_b,
            qp,
            matrix,
            is_inter,
            &mut scratch,
        )?;
    }
    if chroma_coded {
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
                    matrix,
                    is_inter,
                    &mut scratch,
                )?;
            }
        }

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
    } else {
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
    }
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
    matrix: &[[u8; 8]; 8],
    _is_inter: bool,
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
            let above_left = neighbours_luma(fb, x0, y0, b, above, left);
            predict_intra_block(
                &mut pred[..b * b],
                b,
                IntraMode::from_u8(*mode).unwrap_or(IntraMode::Dc),
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
        matrix,
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
    matrix: &[[u8; 8]; 8],
    _is_inter: bool,
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
            predict_intra_block(
                &mut pred[..b * b],
                b,
                IntraMode::from_u8(*mode).unwrap_or(IntraMode::Dc),
                &above[..b],
                &left[..b],
                above_left,
            );
            DeblockBlock::intra(qp)
        }
        BlockSyntax::Inter { sub, mv, .. } => {
            let ref_ = reference.expect("inter without reference");
            let ref_plane = if plane_idx == 0 { &ref_.cb } else { &ref_.cr };
            predict_inter_luma(
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
        matrix,
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
    matrix: &[[u8; 8]; 8],
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
        let (mr, mc) = matrix_pos(k, n);
        coeffs[k] = dequantize(c, matrix, mr, mc, qp as u8);
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

/// Per-block scratch reused across a whole frame.
///
/// The reconstruction loop used to allocate a fresh `Vec` for the prediction
/// block, the two neighbour rows, the dequantised coefficients and the residual
/// *for every block*. One of these is created per frame instead and threaded
/// through.
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

pub fn decode_frame_payload(
    seq: &SequenceHeader,
    frame: &FrameHeader,
    reference: Option<&FrameBuffer>,
    payload: &[u8],
) -> Result<FrameBuffer, KinetixError> {
    let blocks = parse_blocks(seq, frame, payload)?;
    reconstruct_frame(seq, frame, reference, &blocks)
}

/// Parse the rANS payload into the block syntax list, enforcing the exact
/// expected block count (luma blocks, then cb/cr when the sequence declares
/// `chroma_present`). A truncated or over-long list is a parse error rather
/// than an index panic downstream.
fn parse_blocks(
    seq: &SequenceHeader,
    frame: &FrameHeader,
    payload: &[u8],
) -> Result<Vec<BlockSyntax>, KinetixError> {
    let raw = decode_frame_bytes(payload)?;
    let mut r = raw.as_slice();
    let mut blocks = Vec::new();
    while !r.is_empty() {
        blocks.push(read_block(&mut r)?);
    }
    let (luma_b, _) = block_sizes(seq);
    let luma_total =
        (frame.width as usize).div_ceil(luma_b) * (frame.height as usize).div_ceil(luma_b);
    let expected = luma_total
        + if seq.chroma_present {
            2 * chroma_block_total(seq, frame)
        } else {
            0
        };
    if blocks.len() != expected {
        return Err(KinetixError::Parse(format!(
            "vision payload has {} blocks, expected {expected}",
            blocks.len()
        )));
    }
    Ok(blocks)
}

fn chroma_block_total(seq: &SequenceHeader, frame: &FrameHeader) -> usize {
    let (_, chroma_b) = block_sizes(seq);
    let (cw, ch) = chroma_dims(
        ChromaFormat::Yuv420,
        frame.width as usize,
        frame.height as usize,
    );
    cw.div_ceil(chroma_b) * ch.div_ceil(chroma_b)
}

/// Encode a frame into a single rANS payload.
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
    let matrix = quant_matrix(seq.quant_matrix_id);

    // One scratch set for the whole frame; shared by the luma and chroma loops
    // and reused for every block and every intra mode trial.
    let mut scratch = EncodeScratch::new(luma_b.max(chroma_b));

    let mut luma_syntax = Vec::with_capacity(luma_total);
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
            matrix,
            is_inter,
            &mut scratch,
        )?);
    }
    let mut cb_syntax = Vec::new();
    let mut cr_syntax = Vec::new();
    if seq.chroma_present {
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
                matrix,
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
                matrix,
                is_inter,
                &mut scratch,
            )?);
        }
    }

    let mut raw = Vec::new();
    for b in &luma_syntax {
        write_block(&mut raw, b);
    }
    if seq.chroma_present {
        for b in &cb_syntax {
            write_block(&mut raw, b);
        }
        for b in &cr_syntax {
            write_block(&mut raw, b);
        }
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
    matrix: &[[u8; 8]; 8],
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
                matrix,
                &mut s.residual,
                &mut s.transformed,
                &mut s.coeffs,
            );
            return Ok(BlockSyntax::Inter {
                sub: 0,
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
            matrix,
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
            matrix,
            &mut s.full,
            &mut s.back,
            &mut s.tscratch,
            &mut s.recon,
        );
        if err < best_err {
            best_err = err;
            best_mode = mode;
            // Only cloned when a mode actually wins.
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
    matrix: &[[u8; 8]; 8],
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
            predict_inter_luma(
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
                matrix,
                &mut s.residual,
                &mut s.transformed,
                &mut s.coeffs,
            );
            return Ok(BlockSyntax::Inter {
                sub: 0,
                mv: MotionVector::zero(),
                coeffs: s.coeffs.clone(),
            });
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
            matrix,
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
            matrix,
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
/// to allocate ~6 `Vec`s. At 14 modes per block that is ~84 heap allocations
/// per block. Every buffer below is allocated once per frame and reused.
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
    matrix: &[[u8; 8]; 8],
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
        let (mr, mc) = matrix_pos(i, b);
        let q = quantize(t, matrix, mr, mc, qp);
        out.push(q);
        if q != 0 {
            last = i + 1;
        }
    }
    out.truncate(last);
}

/// Reconstruct `pred + residual(coeffs)` into `out`, reusing every buffer.
#[allow(clippy::too_many_arguments)]
fn apply_reconstruct_into(
    pred: &[i32],
    coeffs: &[i32],
    b: usize,
    qp: u8,
    matrix: &[[u8; 8]; 8],
    full: &mut [i32],
    back: &mut [i32],
    tscratch: &mut [i32],
    out: &mut [i32],
) {
    let n = b * b;
    full[..n].fill(0);
    for (k, &c) in coeffs.iter().enumerate().take(n) {
        let (mr, mc) = matrix_pos(k, b);
        full[k] = dequantize(c, matrix, mr, mc, qp);
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
    matrix: &[[u8; 8]; 8],
    full: &mut [i32],
    back: &mut [i32],
    tscratch: &mut [i32],
    recon: &mut [i32],
) -> i64 {
    let n = b * b;
    apply_reconstruct_into(pred, coeffs, b, qp, matrix, full, back, tscratch, recon);
    orig[..n]
        .iter()
        .zip(recon[..n].iter())
        .map(|(a, r)| (a - r).abs() as i64)
        .max()
        .unwrap_or(0)
}

/// Decode to a feature tensor (fast path — no pixel reconstruction).
pub fn decode_tensor(
    seq: &SequenceHeader,
    frame: &FrameHeader,
    payload: &[u8],
) -> Result<Tensor, KinetixError> {
    let blocks = parse_blocks(seq, frame, payload)?;

    let (luma_b, _) = block_sizes(seq);
    let gw = frame.width as usize / luma_b;
    let gh = frame.height as usize / luma_b;
    let luma_total = gw * gh;
    let matrix = quant_matrix(seq.quant_matrix_id);
    let stride = 16usize;
    let tensor_w = frame.width as usize / stride;
    let tensor_h = frame.height as usize / stride;
    let mut data = vec![0f32; tensor_w * tensor_h];

    for (bi, block) in blocks.iter().enumerate().take(luma_total) {
        let bx = bi % gw;
        let by = bi / gw;
        let coeffs = match block {
            BlockSyntax::Intra { coeffs, .. } => coeffs,
            BlockSyntax::Inter { coeffs, .. } => coeffs,
        };
        let n = luma_b * luma_b;
        let mut full = vec![0i32; n];
        for (k, &c) in coeffs.iter().enumerate() {
            if k >= n {
                break;
            }
            let (mr, mc) = matrix_pos(k, luma_b);
            full[k] = dequantize(c, matrix, mr, mc, frame.base_qp);
        }
        // Downsample: average each stride×stride region of the dequantized block.
        let tx = bx * luma_b / stride;
        let ty = by * luma_b / stride;
        if ty < tensor_h && tx < tensor_w {
            let mut sum = 0i64;
            for y in 0..luma_b {
                for x in 0..luma_b {
                    sum += full[y * luma_b + x] as i64;
                }
            }
            data[ty * tensor_w + tx] = (sum / (luma_b * luma_b) as i64) as f32;
        }
    }

    Ok(Tensor {
        data,
        shape: [1, tensor_h, tensor_w],
        stride,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq() -> SequenceHeader {
        SequenceHeader {
            version: 1,
            max_width: 1920,
            max_height: 1080,
            chroma_present: true,
            bit_depth: 8,
            qp_precision: 0,
            max_ref_frames: 2,
            num_rans_streams: 1,
            min_block_size_log2: 3,
            max_block_size_log2: 3,
            quant_matrix_id: 0,
        }
    }

    fn key_frame() -> FrameHeader {
        FrameHeader {
            frame_type: FrameType::Key,
            width: 16,
            height: 16,
            base_qp: 0,
            ref_frame_count: 0,
            output_mode: 2,
            payload_len: 0,
        }
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
        // Vision uses an aggressive quant matrix that is intentionally lossy
        // even at qp=0 (optimizes for ML accuracy, not pixel-exact reconstruction).
        // Verify the round-trip is close (within a few levels).
        for y in 0..16 {
            for x in 0..16 {
                let diff = luma[y * 16 + x].abs_diff(decoded.luma[y * 16 + x]);
                assert!(
                    diff <= 8,
                    "luma mismatch at ({x},{y}): expected {}, got {}",
                    luma[y * 16 + x],
                    decoded.luma[y * 16 + x]
                );
            }
        }
        // Chroma uses the same matrix; verify it's close.
        for y in 0..8 {
            for x in 0..8 {
                let diff_cb = cb[y * 8 + x].abs_diff(decoded.cb[y * 8 + x]);
                let diff_cr = cr[y * 8 + x].abs_diff(decoded.cr[y * 8 + x]);
                assert!(diff_cb <= 8, "cb mismatch at ({x},{y})");
                assert!(diff_cr <= 8, "cr mismatch at ({x},{y})");
            }
        }
    }

    #[test]
    fn tensor_decode_produces_output() {
        let s = seq();
        let f = key_frame();
        let mut luma = vec![0u8; 16 * 16];
        for y in 0..16 {
            for x in 0..16 {
                luma[y * 16 + x] = ((x * 7 + y * 3) & 0xFF) as u8;
            }
        }
        let src =
            FrameBuffer::from_yuv420(16, 16, luma, vec![128u8; 8 * 8], vec![128u8; 8 * 8]).unwrap();
        let payload = encode_frame(&s, &f, &src, None).unwrap();
        let tensor = decode_tensor(&s, &f, &payload).unwrap();
        assert!(!tensor.data.is_empty());
        assert_eq!(tensor.stride, 16);
    }

    #[test]
    fn luma_only_stream_reconstructs_neutral_chroma() {
        // chroma_present == false is the default for detection encodes: the
        // payload carries no chroma blocks and the decoder emits neutral 4:2:0.
        let mut s = seq();
        s.chroma_present = false;
        let f = key_frame();
        let mut luma = vec![0u8; 16 * 16];
        for y in 0..16 {
            for x in 0..16 {
                luma[y * 16 + x] = ((x + y) * 8) as u8;
            }
        }
        let src =
            FrameBuffer::from_yuv420(16, 16, luma.clone(), vec![128; 64], vec![128; 64]).unwrap();
        let payload = encode_frame(&s, &f, &src, None).unwrap();
        let decoded = decode_frame_payload(&s, &f, None, &payload).unwrap();
        for y in 0..16 {
            for x in 0..16 {
                let diff = luma[y * 16 + x].abs_diff(decoded.luma[y * 16 + x]);
                assert!(diff <= 8, "luma mismatch at ({x},{y}): {diff}");
            }
        }
        assert!(decoded.cb.iter().all(|&v| v == 128));
        assert!(decoded.cr.iter().all(|&v| v == 128));
    }

    #[test]
    fn sixteen_by_sixteen_blocks_round_trip() {
        // The header allows block sizes up to 64x64; exercise 16x16 to guard
        // the quantization-matrix folding for non-8 blocks (previously an
        // out-of-bounds panic).
        let mut s = seq();
        s.min_block_size_log2 = 4;
        s.max_block_size_log2 = 4;
        let f = FrameHeader {
            frame_type: FrameType::Key,
            width: 16,
            height: 16,
            base_qp: 0,
            ref_frame_count: 0,
            output_mode: 2,
            payload_len: 0,
        };
        let mut luma = vec![0u8; 16 * 16];
        for y in 0..16 {
            for x in 0..16 {
                luma[y * 16 + x] = ((x * 11 + y * 5) & 0xFF) as u8;
            }
        }
        let src =
            FrameBuffer::from_yuv420(16, 16, luma.clone(), vec![128u8; 8 * 8], vec![128u8; 8 * 8])
                .unwrap();
        let payload = encode_frame(&s, &f, &src, None).unwrap();
        let decoded = decode_frame_payload(&s, &f, None, &payload).unwrap();
        for y in 0..16 {
            for x in 0..16 {
                let diff = luma[y * 16 + x].abs_diff(decoded.luma[y * 16 + x]);
                assert!(diff <= 12, "luma mismatch at ({x},{y}): {diff}");
            }
        }
    }

    #[test]
    fn truncated_block_list_is_an_error_not_a_panic() {
        let s = seq();
        let f = key_frame();
        let src =
            FrameBuffer::from_yuv420(16, 16, vec![64u8; 256], vec![128u8; 64], vec![128u8; 64])
                .unwrap();
        let payload = encode_frame(&s, &f, &src, None).unwrap();
        // rANS payloads decode back-to-front, so truncating the tail cuts the
        // last-coded (first-decoded) blocks: the count check must reject this.
        let truncated = &payload[..payload.len() / 2];
        assert!(decode_frame_payload(&s, &f, None, truncated).is_err());
    }

    #[test]
    fn extra_payload_bytes_are_rejected() {
        // The rANS payload must decode to exactly the expected block list.
        // (Muxer padding never reaches this layer: `VisionDecoder` and the
        // CLI both slice the packet to the header's `payload_len` first.)
        let s = seq();
        let f = key_frame();
        let src =
            FrameBuffer::from_yuv420(16, 16, vec![64u8; 256], vec![128u8; 64], vec![128u8; 64])
                .unwrap();
        let payload = encode_frame(&s, &f, &src, None).unwrap();
        let mut padded = payload.clone();
        padded.extend_from_slice(&[0u8; 7]);
        assert!(decode_frame_payload(&s, &f, None, &padded).is_err());
    }

    #[test]
    fn inter_frame_round_trips_against_key_reference() {
        let s = seq();
        let key = key_frame();
        let inter = FrameHeader {
            frame_type: FrameType::Inter,
            width: 16,
            height: 16,
            base_qp: 0,
            ref_frame_count: 1,
            output_mode: 2,
            payload_len: 0,
        };
        let luma: Vec<u8> = (0..256).map(|i| (i * 3) as u8).collect();
        let src = FrameBuffer::from_yuv420(16, 16, luma.clone(), vec![128u8; 64], vec![128u8; 64])
            .unwrap();
        let key_payload = encode_frame(&s, &key, &src, None).unwrap();
        let reference = decode_frame_payload(&s, &key, None, &key_payload).unwrap();
        let inter_payload = encode_frame(&s, &inter, &src, Some(&reference)).unwrap();
        let decoded = decode_frame_payload(&s, &inter, Some(&reference), &inter_payload).unwrap();
        for y in 0..16 {
            for x in 0..16 {
                let diff = luma[y * 16 + x].abs_diff(decoded.luma[y * 16 + x]);
                assert!(diff <= 8, "inter luma mismatch at ({x},{y}): {diff}");
            }
        }
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
            output_mode: 2,
            payload_len: 0,
        };
        let inter = FrameHeader {
            frame_type: FrameType::Inter,
            width: 32,
            height: 32,
            base_qp: 12,
            ref_frame_count: 1,
            output_mode: 2,
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
        0x6e38f29cef7fa299,
        0xcc92f686ac6789c9,
        0xece422f2b9734a65,
        0x86f2e87cc452fe42,
    ];
}
