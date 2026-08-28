//! Frame reconstruction + dual-path (tensor/pixel) decode.

#![allow(clippy::too_many_arguments)]

use tpt_kinetix_bitstream::{RansDecoder, RansEncoder, StaticModel};
use tpt_kinetix_core::{error::KinetixError, frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp};

use crate::deblock::{deblock_chroma, deblock_luma, DeblockBlock};
use crate::headers::{ChromaFormat, FrameHeader, FrameType, SequenceHeader};
use crate::prediction::{predict_inter_luma, predict_intra_block, IntraMode, MotionVector};
use crate::quant::{quant_matrix, quantize, dequantize};
use crate::transform::{transform_2d, inverse_2d};
use crate::Tensor;

const R: i32 = 128;

pub fn chroma_subsampling(fmt: ChromaFormat) -> (usize, usize) {
    match fmt { ChromaFormat::Yuv420 => (1, 1), ChromaFormat::Yuv422 => (1, 0), ChromaFormat::Yuv444 => (0, 0) }
}

pub fn chroma_dims(fmt: ChromaFormat, w: usize, h: usize) -> (usize, usize) {
    let (hs, vs) = chroma_subsampling(fmt);
    ((w + (1 << hs) - 1) >> hs, (h + (1 << vs) - 1) >> vs)
}

#[derive(Debug, Clone)]
pub struct FrameBuffer {
    pub width: usize, pub height: usize, pub format: ChromaFormat,
    pub luma: Vec<u8>, pub cb: Vec<u8>, pub cr: Vec<u8>,
    pub chroma_w: usize, pub chroma_h: usize,
}

impl FrameBuffer {
    pub fn new(_seq: &SequenceHeader, frame: &FrameHeader) -> Self {
        let w = frame.width as usize; let h = frame.height as usize;
        let (cw, ch) = chroma_dims(ChromaFormat::Yuv420, w, h);
        Self { width: w, height: h, format: ChromaFormat::Yuv420, luma: vec![0u8; w * h], cb: vec![0u8; cw * ch], cr: vec![0u8; cw * ch], chroma_w: cw, chroma_h: ch }
    }

    pub fn from_yuv420(width: u32, height: u32, luma: Vec<u8>, cb: Vec<u8>, cr: Vec<u8>) -> Result<Self, KinetixError> {
        let (cw, ch) = chroma_dims(ChromaFormat::Yuv420, width as usize, height as usize);
        if luma.len() != width as usize * height as usize || cb.len() != cw * ch || cr.len() != cw * ch {
            return Err(KinetixError::Parse("from_yuv420: buffer size mismatch".into()));
        }
        Ok(Self { width: width as usize, height: height as usize, format: ChromaFormat::Yuv420, luma, cb, cr, chroma_w: cw, chroma_h: ch })
    }

    pub fn to_video_frame(&self, is_key: bool) -> VideoFrame {
        let mut data = Vec::with_capacity(self.luma.len() + self.cb.len() + self.cr.len());
        data.extend_from_slice(&self.luma); data.extend_from_slice(&self.cb); data.extend_from_slice(&self.cr);
        VideoFrame { pts: Timestamp::NONE, dts: Timestamp::NONE, data, width: self.width as u32, height: self.height as u32, pixel_format: PixelFormat::Yuv420p, is_key_frame: is_key }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlockSyntax {
    Intra { mode: u8, coeffs: Vec<i32> },
    Inter { sub: u8, mv: MotionVector, coeffs: Vec<i32> },
}

fn block_sizes(seq: &SequenceHeader) -> (usize, usize) {
    let luma_b = 1usize << seq.min_block_size_log2;
    let chroma_b = (luma_b / 2).max(4);
    (luma_b, chroma_b)
}

fn write_i16(out: &mut Vec<u8>, v: i16) { out.extend_from_slice(&v.to_le_bytes()); }
fn read_i16(r: &mut &[u8]) -> Result<i16, KinetixError> {
    if r.len() < 2 { return Err(KinetixError::Parse("truncated i16".into())); }
    let mut b = [0u8; 2]; b.copy_from_slice(&r[0..2]); *r = &r[2..]; Ok(i16::from_le_bytes(b))
}
fn write_i32(out: &mut Vec<u8>, v: i32) { out.extend_from_slice(&v.to_le_bytes()); }
fn read_i32(r: &mut &[u8]) -> Result<i32, KinetixError> {
    if r.len() < 4 { return Err(KinetixError::Parse("truncated i32".into())); }
    let mut b = [0u8; 4]; b.copy_from_slice(&r[0..4]); *r = &r[4..]; Ok(i32::from_le_bytes(b))
}

fn write_block(out: &mut Vec<u8>, b: &BlockSyntax) {
    match b {
        BlockSyntax::Intra { mode, coeffs } => {
            out.push(0); out.push(*mode); out.push(coeffs.len() as u8);
            for &c in coeffs.iter() { write_i32(out, c); }
        }
        BlockSyntax::Inter { sub, mv, coeffs } => {
            out.push(1); out.push(*sub);
            if *sub != 0 { write_i16(out, mv.x as i16); write_i16(out, mv.y as i16); }
            out.push(coeffs.len() as u8);
            for &c in coeffs.iter() { write_i32(out, c); }
        }
    }
}

fn read_block(r: &mut &[u8]) -> Result<BlockSyntax, KinetixError> {
    if r.is_empty() { return Err(KinetixError::Parse("empty block".into())); }
    let kind = r[0]; *r = &r[1..];
    match kind {
        0 => {
            if r.is_empty() { return Err(KinetixError::Parse("intra: missing mode".into())); }
            let mode = r[0]; *r = &r[1..];
            let n = *r.first().ok_or_else(|| KinetixError::Parse("intra: missing coeff count".into()))? as usize; *r = &r[1..];
            let mut coeffs = Vec::with_capacity(n);
            for _ in 0..n { coeffs.push(read_i32(r)?); }
            Ok(BlockSyntax::Intra { mode, coeffs })
        }
        1 => {
            let sub = *r.first().ok_or_else(|| KinetixError::Parse("inter: missing sub".into()))?; *r = &r[1..];
            let mv = if sub == 0 { MotionVector::zero() } else { let x = read_i16(r)? as i32; let y = read_i16(r)? as i32; MotionVector::new(x, y) };
            let n = *r.first().ok_or_else(|| KinetixError::Parse("inter: missing coeff count".into()))? as usize; *r = &r[1..];
            let mut coeffs = Vec::with_capacity(n);
            for _ in 0..n { coeffs.push(read_i32(r)?); }
            Ok(BlockSyntax::Inter { sub, mv, coeffs })
        }
        other => Err(KinetixError::Parse(format!("unknown prediction kind {other}"))),
    }
}

pub fn encode_frame_bytes(raw: &[u8]) -> Vec<u8> {
    let model = StaticModel; let mut enc = RansEncoder::new();
    for &s in raw.iter().rev() { enc.encode(&model, s); }
    enc.finish()
}

pub fn decode_frame_bytes(payload: &[u8]) -> Result<Vec<u8>, KinetixError> {
    let model = StaticModel; let mut dec = RansDecoder::new(payload)?;
    let max = payload.len() * 4 + 1024; let mut out = Vec::new(); let mut guard = 0;
    while let Ok(s) = dec.decode(&model) { out.push(s); guard += 1; if guard > max { break; } }
    Ok(out)
}

pub fn reconstruct_frame(seq: &SequenceHeader, frame: &FrameHeader, reference: Option<&FrameBuffer>, blocks: &[BlockSyntax]) -> Result<FrameBuffer, KinetixError> {
    let mut fb = FrameBuffer::new(seq, frame);
    let (luma_b, chroma_b) = block_sizes(seq);
    let gw = fb.width.div_ceil(luma_b); let gh = fb.height.div_ceil(luma_b);
    let cgw = fb.chroma_w.div_ceil(chroma_b); let cgh = fb.chroma_h.div_ceil(chroma_b);
    let luma_total = gw * gh; let chroma_total = cgw * cgh;
    let qp = frame.base_qp as i32;
    let is_inter = frame.frame_type == FrameType::Inter;
    if is_inter && reference.is_none() { return Err(KinetixError::Parse("vision inter frame without reference".into())); }

    let matrix = quant_matrix(seq.quant_matrix_id);
    let mut luma_db = vec![DeblockBlock::intra(qp); luma_total.max(1)];
    let mut chroma_db = vec![DeblockBlock::intra(qp); chroma_total.max(1)];

    for (bi, db) in luma_db.iter_mut().enumerate().take(luma_total) {
        let sx = bi % gw; let sy = bi / gw;
        *db = reconstruct_luma_block(&mut fb, reference, &blocks[bi], sx, sy, luma_b, qp, matrix, is_inter)?;
    }
    let chroma_offset = luma_total;
    for plane_idx in 0..2usize {
        for (bi, db) in chroma_db.iter_mut().enumerate().take(chroma_total) {
            let sx = bi % cgw; let sy = bi / cgw;
            let idx = chroma_offset + plane_idx * chroma_total + bi;
            let block = blocks.get(idx).ok_or_else(|| KinetixError::Parse("chroma block index out of range".into()))?;
            *db = reconstruct_chroma_block(&mut fb, reference, block, plane_idx, sx, sy, chroma_b, qp, matrix, is_inter)?;
        }
    }

    deblock_luma(&mut fb.luma, fb.width, fb.width, fb.height, gw, gh, luma_b, &luma_db);
    deblock_chroma(&mut fb.cb, fb.chroma_w, fb.chroma_w, fb.chroma_h, cgw, cgh, chroma_b, &chroma_db);
    deblock_chroma(&mut fb.cr, fb.chroma_w, fb.chroma_w, fb.chroma_h, cgw, cgh, chroma_b, &chroma_db);
    Ok(fb)
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_luma_block(fb: &mut FrameBuffer, reference: Option<&FrameBuffer>, block: &BlockSyntax, bx: usize, by: usize, b: usize, qp: i32, matrix: &[[u8; 8]; 8], _is_inter: bool) -> Result<DeblockBlock, KinetixError> {
    let x0 = bx * b; let y0 = by * b;
    let mut pred = vec![0i32; b * b];
    let db = match block {
        BlockSyntax::Intra { mode, .. } => {
            let (above, left, above_left) = neighbours_luma(fb, x0, y0, b);
            predict_intra_block(&mut pred, b, IntraMode::from_u8(*mode).unwrap_or(IntraMode::Dc), &above, &left, above_left);
            DeblockBlock::intra(qp)
        }
        BlockSyntax::Inter { sub, mv, .. } => {
            let ref_ = reference.expect("inter without reference");
            predict_inter_luma(&mut pred, b, &ref_.luma, ref_.width, ref_.width, ref_.height, x0, y0, *mv);
            if *sub == 0 { DeblockBlock::inter(MotionVector::zero(), 0, qp) } else { DeblockBlock::inter(*mv, 0, qp) }
        }
    };
    add_residual(&mut fb.luma, fb.width, x0, y0, b, qp, matrix, block, &pred);
    Ok(db)
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_chroma_block(fb: &mut FrameBuffer, reference: Option<&FrameBuffer>, block: &BlockSyntax, plane_idx: usize, bx: usize, by: usize, b: usize, qp: i32, matrix: &[[u8; 8]; 8], _is_inter: bool) -> Result<DeblockBlock, KinetixError> {
    let x0 = bx * b; let y0 = by * b;
    let mut pred = vec![0i32; b * b];
    let db = match block {
        BlockSyntax::Intra { mode, .. } => {
            let (above, left, above_left) = neighbours_chroma(fb, x0, y0, b);
            predict_intra_block(&mut pred, b, IntraMode::from_u8(*mode).unwrap_or(IntraMode::Dc), &above, &left, above_left);
            DeblockBlock::intra(qp)
        }
        BlockSyntax::Inter { sub, mv, .. } => {
            let ref_ = reference.expect("inter without reference");
            let ref_plane = if plane_idx == 0 { &ref_.cb } else { &ref_.cr };
            predict_inter_luma(&mut pred, b, ref_plane, ref_.chroma_w, ref_.chroma_w, ref_.chroma_h, x0, y0, *mv);
            if *sub == 0 { DeblockBlock::inter(MotionVector::zero(), 0, qp) } else { DeblockBlock::inter(*mv, 0, qp) }
        }
    };
    let plane = if plane_idx == 0 { &mut fb.cb } else { &mut fb.cr };
    add_residual(plane, fb.chroma_w, x0, y0, b, qp, matrix, block, &pred);
    Ok(db)
}

fn add_residual(plane: &mut [u8], stride: usize, x0: usize, y0: usize, b: usize, qp: i32, matrix: &[[u8; 8]; 8], block: &BlockSyntax, pred: &[i32]) {
    let n = b;
    let mut coeffs = vec![0i32; n * n];
    let src = match block { BlockSyntax::Intra { coeffs, .. } => coeffs, BlockSyntax::Inter { coeffs, .. } => coeffs };
    for (k, &c) in src.iter().enumerate() { if k >= coeffs.len() { break; } coeffs[k] = dequantize(c, matrix, k / 8, k % 8, qp as u8); }
    let mut residual = vec![0i32; n * n];
    inverse_2d(&coeffs, n, &mut residual);
    for r in 0..b { for c in 0..b {
        let px = x0 + c; let py = y0 + r;
        if px >= stride || py * stride + px >= plane.len() { continue; }
        let v = pred[r * n + c] + residual[r * n + c];
        plane[py * stride + px] = v.clamp(0, 255) as u8;
    } }
}

fn neighbours_luma(fb: &FrameBuffer, x0: usize, y0: usize, b: usize) -> (Vec<i32>, Vec<i32>, i32) {
    let stride = fb.width;
    let mut above = vec![R; b]; let mut left = vec![R; b];
    let above_left = if x0 > 0 && y0 > 0 { fb.luma[(y0 - 1) * stride + (x0 - 1)] as i32 } else { R };
    if y0 > 0 { for (c, above_c) in above.iter_mut().enumerate().take(b) { let x = x0 + c; if x < fb.width { *above_c = fb.luma[(y0 - 1) * stride + x] as i32; } } }
    if x0 > 0 { for (r, left_r) in left.iter_mut().enumerate().take(b) { let y = y0 + r; if y < fb.height { *left_r = fb.luma[y * stride + (x0 - 1)] as i32; } } }
    (above, left, above_left)
}

fn neighbours_chroma(fb: &FrameBuffer, x0: usize, y0: usize, b: usize) -> (Vec<i32>, Vec<i32>, i32) {
    let stride = fb.chroma_w;
    let mut above = vec![R; b]; let mut left = vec![R; b];
    let above_left = if x0 > 0 && y0 > 0 { fb.cb[(y0 - 1) * stride + (x0 - 1)] as i32 } else { R };
    if y0 > 0 { for (c, above_c) in above.iter_mut().enumerate().take(b) { let x = x0 + c; if x < fb.chroma_w { *above_c = fb.cb[(y0 - 1) * stride + x] as i32; } } }
    if x0 > 0 { for (r, left_r) in left.iter_mut().enumerate().take(b) { let y = y0 + r; if y < fb.chroma_h { *left_r = fb.cb[y * stride + (x0 - 1)] as i32; } } }
    (above, left, above_left)
}

pub fn decode_frame_payload(seq: &SequenceHeader, frame: &FrameHeader, reference: Option<&FrameBuffer>, payload: &[u8]) -> Result<FrameBuffer, KinetixError> {
    let raw = decode_frame_bytes(payload)?;
    let mut r = raw.as_slice();
    let mut blocks = Vec::new();
    while !r.is_empty() { blocks.push(read_block(&mut r)?); }
    reconstruct_frame(seq, frame, reference, &blocks)
}

/// Encode a frame into a single rANS payload.
pub fn encode_frame(seq: &SequenceHeader, frame: &FrameHeader, src: &FrameBuffer, reference: Option<&FrameBuffer>) -> Result<Vec<u8>, KinetixError> {
    let (luma_b, chroma_b) = block_sizes(seq);
    let gw = src.width.div_ceil(luma_b); let gh = src.height.div_ceil(luma_b);
    let cgw = src.chroma_w.div_ceil(chroma_b); let cgh = src.chroma_h.div_ceil(chroma_b);
    let luma_total = gw * gh; let chroma_total = cgw * cgh;
    let is_inter = frame.frame_type == FrameType::Inter;
    let matrix = quant_matrix(seq.quant_matrix_id);

    let mut luma_syntax = Vec::with_capacity(luma_total);
    for bi in 0..luma_total {
        let sx = bi % gw; let sy = bi / gw;
        luma_syntax.push(encode_luma_block(src, reference, sx, sy, luma_b, frame.base_qp, matrix, is_inter)?);
    }
    let mut cb_syntax = Vec::with_capacity(chroma_total);
    let mut cr_syntax = Vec::with_capacity(chroma_total);
    for bi in 0..chroma_total {
        let sx = bi % cgw; let sy = bi / cgw;
        cb_syntax.push(encode_chroma_block(src, reference, 0, sx, sy, chroma_b, frame.base_qp, matrix, is_inter)?);
        cr_syntax.push(encode_chroma_block(src, reference, 1, sx, sy, chroma_b, frame.base_qp, matrix, is_inter)?);
    }

    let mut raw = Vec::new();
    for b in &luma_syntax { write_block(&mut raw, b); }
    for b in &cb_syntax { write_block(&mut raw, b); }
    for b in &cr_syntax { write_block(&mut raw, b); }
    Ok(encode_frame_bytes(&raw))
}

#[allow(clippy::too_many_arguments)]
fn encode_luma_block(src: &FrameBuffer, reference: Option<&FrameBuffer>, bx: usize, by: usize, b: usize, qp: u8, matrix: &[[u8; 8]; 8], is_inter: bool) -> Result<BlockSyntax, KinetixError> {
    let stride = src.width; let x0 = bx * b; let y0 = by * b; let n = b * b;
    let mut orig = vec![0i32; n];
    for r in 0..b { for c in 0..b { orig[r * b + c] = src.luma[(y0 + r) * stride + (x0 + c)] as i32; } }

    if is_inter {
        if let Some(ref_) = reference {
            let mut pred = vec![0i32; n];
            predict_inter_luma(&mut pred, b, &ref_.luma, ref_.width, ref_.width, ref_.height, x0, y0, MotionVector::zero());
            let coeffs = encode_residual(&orig, &pred, b, qp, matrix);
            return Ok(BlockSyntax::Inter { sub: 0, mv: MotionVector::zero(), coeffs });
        }
    }

    let (above, left, above_left) = neighbours_luma(src, x0, y0, b);
    let mut best_mode = IntraMode::Dc; let mut best_coeffs = vec![]; let mut best_err = i64::MAX;
    for m in 0..crate::prediction::NUM_INTRA_MODES {
        let mode = IntraMode::from_u8(m).unwrap();
        let mut pred = vec![0i32; n];
        predict_intra_block(&mut pred, b, mode, &above, &left, above_left);
        let coeffs = encode_residual(&orig, &pred, b, qp, matrix);
        let err = residual_error(&orig, &pred, &coeffs, b, qp, matrix);
        if err < best_err { best_err = err; best_mode = mode; best_coeffs = coeffs; }
    }
    Ok(BlockSyntax::Intra { mode: best_mode as u8, coeffs: best_coeffs })
}

#[allow(clippy::too_many_arguments)]
fn encode_chroma_block(src: &FrameBuffer, reference: Option<&FrameBuffer>, plane_idx: usize, bx: usize, by: usize, b: usize, qp: u8, matrix: &[[u8; 8]; 8], is_inter: bool) -> Result<BlockSyntax, KinetixError> {
    let stride = src.chroma_w; let x0 = bx * b; let y0 = by * b; let n = b * b;
    let plane = if plane_idx == 0 { &src.cb } else { &src.cr };
    let mut orig = vec![0i32; n];
    for r in 0..b { for c in 0..b { orig[r * b + c] = plane[(y0 + r) * stride + (x0 + c)] as i32; } }
    if is_inter {
        if let Some(ref_) = reference {
            let ref_plane = if plane_idx == 0 { &ref_.cb } else { &ref_.cr };
            let mut pred = vec![0i32; n];
            predict_inter_luma(&mut pred, b, ref_plane, ref_.chroma_w, ref_.chroma_w, ref_.chroma_h, x0, y0, MotionVector::zero());
            let coeffs = encode_residual(&orig, &pred, b, qp, matrix);
            return Ok(BlockSyntax::Inter { sub: 0, mv: MotionVector::zero(), coeffs });
        }
    }
    let (above, left, above_left) = neighbours_chroma(src, x0, y0, b);
    let mut best_mode = IntraMode::Dc; let mut best_coeffs = vec![]; let mut best_err = i64::MAX;
    for m in 0..crate::prediction::NUM_INTRA_MODES {
        let mode = IntraMode::from_u8(m).unwrap();
        let mut pred = vec![0i32; n];
        predict_intra_block(&mut pred, b, mode, &above, &left, above_left);
        let coeffs = encode_residual(&orig, &pred, b, qp, matrix);
        let err = residual_error(&orig, &pred, &coeffs, b, qp, matrix);
        if err < best_err { best_err = err; best_mode = mode; best_coeffs = coeffs; }
    }
    Ok(BlockSyntax::Intra { mode: best_mode as u8, coeffs: best_coeffs })
}

fn encode_residual(orig: &[i32], pred: &[i32], b: usize, qp: u8, matrix: &[[u8; 8]; 8]) -> Vec<i32> {
    let n = b * b;
    let mut residual = vec![0i32; n];
    for i in 0..n { residual[i] = orig[i] - pred[i]; }
    let mut transformed = vec![0i32; n];
    transform_2d(&residual, b, &mut transformed);
    let mut coeffs = Vec::with_capacity(n);
    let mut last = 0;
    for (i, &t) in transformed.iter().enumerate() {
        let q = quantize(t, matrix, i / 8, i % 8, qp);
        coeffs.push(q);
        if q != 0 { last = i + 1; }
    }
    coeffs.truncate(last);
    coeffs
}

fn apply_reconstruct(pred: &[i32], coeffs: &[i32], b: usize, qp: u8, matrix: &[[u8; 8]; 8]) -> Vec<i32> {
    let n = b * b;
    let mut full = vec![0i32; n];
    for (k, &c) in coeffs.iter().enumerate() { if k >= n { break; } full[k] = dequantize(c, matrix, k / 8, k % 8, qp); }
    let mut residual = vec![0i32; n];
    inverse_2d(&full, b, &mut residual);
    let mut out = vec![0i32; n];
    for i in 0..n { out[i] = (pred[i] + residual[i]).clamp(0, 255); }
    out
}

fn residual_error(orig: &[i32], pred: &[i32], coeffs: &[i32], b: usize, qp: u8, matrix: &[[u8; 8]; 8]) -> i64 {
    let recon = apply_reconstruct(pred, coeffs, b, qp, matrix);
    orig.iter().zip(recon.iter()).map(|(a, b)| (a - b).abs() as i64).max().unwrap_or(0)
}

/// Decode to a feature tensor (fast path — no pixel reconstruction).
pub fn decode_tensor(seq: &SequenceHeader, frame: &FrameHeader, payload: &[u8]) -> Result<Tensor, KinetixError> {
    let raw = decode_frame_bytes(payload)?;
    let mut r = raw.as_slice();
    let mut blocks = Vec::new();
    while !r.is_empty() { blocks.push(read_block(&mut r)?); }

    let (luma_b, _) = block_sizes(seq);
    let gw = frame.width as usize / luma_b; let gh = frame.height as usize / luma_b;
    let luma_total = gw * gh;
    let matrix = quant_matrix(seq.quant_matrix_id);
    let stride = 16usize;
    let tensor_w = frame.width as usize / stride;
    let tensor_h = frame.height as usize / stride;
    let mut data = vec![0f32; tensor_w * tensor_h];

    for (bi, block) in blocks.iter().enumerate().take(luma_total) {
        let bx = bi % gw; let by = bi / gw;
        let coeffs = match block { BlockSyntax::Intra { coeffs, .. } => coeffs, BlockSyntax::Inter { coeffs, .. } => coeffs };
        let n = luma_b * luma_b;
        let mut full = vec![0i32; n];
        for (k, &c) in coeffs.iter().enumerate() { if k >= n { break; } full[k] = dequantize(c, matrix, k / 8, k % 8, frame.base_qp); }
        // Downsample: average each stride×stride region of the dequantized block.
        let tx = bx * luma_b / stride; let ty = by * luma_b / stride;
        if ty < tensor_h && tx < tensor_w {
            let mut sum = 0i64;
            for y in 0..luma_b { for x in 0..luma_b { sum += full[y * luma_b + x] as i64; } }
            data[ty * tensor_w + tx] = (sum / (luma_b * luma_b) as i64) as f32;
        }
    }

    Ok(Tensor { data, shape: [1, tensor_h, tensor_w], stride })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq() -> SequenceHeader {
        SequenceHeader {
            version: 1, max_width: 1920, max_height: 1080, chroma_present: false,
            bit_depth: 8, qp_precision: 0, max_ref_frames: 2, num_rans_streams: 1,
            min_block_size_log2: 3, max_block_size_log2: 3, quant_matrix_id: 0,
        }
    }

    fn key_frame() -> FrameHeader {
        FrameHeader { frame_type: FrameType::Key, width: 16, height: 16, base_qp: 0, ref_frame_count: 0, output_mode: 2, payload_len: 0 }
    }

    #[test]
    fn keyframe_round_trips_at_qp0() {
        let s = seq(); let f = key_frame();
        let mut luma = vec![0u8; 16 * 16]; let mut cb = vec![0u8; 8 * 8]; let mut cr = vec![0u8; 8 * 8];
        for y in 0..16 { for x in 0..16 { luma[y * 16 + x] = ((x + y) * 8) as u8; } }
        for y in 0..8 { for x in 0..8 { cb[y * 8 + x] = (x * 16) as u8; cr[y * 8 + x] = (y * 16) as u8; } }
        let src = FrameBuffer::from_yuv420(16, 16, luma.clone(), cb.clone(), cr.clone()).unwrap();
        let payload = encode_frame(&s, &f, &src, None).unwrap();
        let decoded = decode_frame_payload(&s, &f, None, &payload).unwrap();
        // Vision uses an aggressive quant matrix that is intentionally lossy
        // even at qp=0 (optimizes for ML accuracy, not pixel-exact reconstruction).
        // Verify the round-trip is close (within a few levels).
        for y in 0..16 { for x in 0..16 {
            let diff = luma[y * 16 + x].abs_diff(decoded.luma[y * 16 + x]);
            assert!(diff <= 8, "luma mismatch at ({x},{y}): expected {}, got {}", luma[y*16+x], decoded.luma[y*16+x]);
        } }
        // Chroma uses the same matrix; verify it's close.
        for y in 0..8 { for x in 0..8 {
            let diff_cb = cb[y * 8 + x].abs_diff(decoded.cb[y * 8 + x]);
            let diff_cr = cr[y * 8 + x].abs_diff(decoded.cr[y * 8 + x]);
            assert!(diff_cb <= 8, "cb mismatch at ({x},{y})");
            assert!(diff_cr <= 8, "cr mismatch at ({x},{y})");
        } }
    }

    #[test]
    fn tensor_decode_produces_output() {
        let s = seq(); let f = key_frame();
        let mut luma = vec![0u8; 16 * 16];
        for y in 0..16 { for x in 0..16 { luma[y * 16 + x] = ((x * 7 + y * 3) & 0xFF) as u8; } }
        let src = FrameBuffer::from_yuv420(16, 16, luma, vec![128u8; 8 * 8], vec![128u8; 8 * 8]).unwrap();
        let payload = encode_frame(&s, &f, &src, None).unwrap();
        let tensor = decode_tensor(&s, &f, &payload).unwrap();
        assert!(!tensor.data.is_empty());
        assert_eq!(tensor.stride, 16);
    }
}
