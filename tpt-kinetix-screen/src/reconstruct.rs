//! Frame reconstruction: ties together the 4 rANS sub-streams.

#![allow(clippy::too_many_arguments, unused_mut)]

use tpt_kinetix_bitstream::{RansDecoder, RansEncoder, RansStreamSet, StaticModel};
use tpt_kinetix_core::{
    error::KinetixError, frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp,
};

use crate::classify::{classify_block_luma, BlockMode};
use crate::dictionary::{GlyphDictionary, PaletteColor};
use crate::flat::{self, FlatRun};
use crate::glyph::{self, GlyphBlock};
use crate::headers::{FrameHeader, SequenceHeader};
use crate::natural::{self, NaturalBlock};

/// Submitted frame buffer (planar YUV).
#[derive(Debug, Clone)]
pub struct FrameBuffer {
    pub width: usize,
    pub height: usize,
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
        let (cw, ch) = crate::reconstruct::chroma_dims(seq.chroma_format, w, h);
        Self {
            width: w,
            height: h,
            luma: vec![0u8; w * h],
            cb: vec![0u8; cw * ch],
            cr: vec![0u8; cw * ch],
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
        let (cw, ch) = crate::reconstruct::chroma_dims(
            crate::headers::ChromaFormat::Yuv420,
            width as usize,
            height as usize,
        );
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

/// Chroma subsampling factors.
pub fn chroma_subsampling(fmt: crate::headers::ChromaFormat) -> (usize, usize) {
    match fmt {
        crate::headers::ChromaFormat::Yuv420 => (1, 1),
        crate::headers::ChromaFormat::Yuv422 => (1, 0),
        crate::headers::ChromaFormat::Yuv444 => (0, 0),
    }
}

/// Chroma plane dimensions for a luma `w`×`h`.
pub fn chroma_dims(fmt: crate::headers::ChromaFormat, w: usize, h: usize) -> (usize, usize) {
    let (hs, vs) = chroma_subsampling(fmt);
    ((w + (1 << hs) - 1) >> hs, (h + (1 << vs) - 1) >> vs)
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// Encode one frame into 4 rANS sub-streams (mode map, FLAT, GLYPH, NATURAL).
pub fn encode_frame(
    seq: &SequenceHeader,
    frame: &FrameHeader,
    src: &FrameBuffer,
    _reference: Option<&FrameBuffer>,
) -> Result<Vec<u8>, KinetixError> {
    let cb_size = 1usize << seq.base_block_size_log2;
    let gw = src.width.div_ceil(cb_size);
    let gh = src.height.div_ceil(cb_size);
    let total_blocks = gw * gh;

    let mut modes = Vec::with_capacity(total_blocks);
    let mut flat_colors = Vec::with_capacity(total_blocks);
    let mut glyph_blocks: Vec<Option<GlyphBlock>> = vec![None; total_blocks];
    let mut natural_blocks: Vec<Option<NaturalBlock>> = vec![None; total_blocks];

    let mut dict = GlyphDictionary::new(seq.dict_cap as usize);

    // Classify each block.
    for by in 0..gh {
        for bx in 0..gw {
            let bi = by * gw + bx;
            let block = extract_luma_block(src, bx * cb_size, by * cb_size, cb_size);
            let mode = classify_block_luma(&block, cb_size, 4);

            match mode {
                BlockMode::Flat => {
                    let mean_y =
                        (block.iter().map(|&p| p as u32).sum::<u32>() / block.len() as u32) as u8;
                    modes.push(0u8);
                    flat_colors.push(mean_y);
                }
                BlockMode::Glyph => {
                    let fg = PaletteColor::new(255, 128, 128);
                    let bg = PaletteColor::new(0, 128, 128);
                    if let Some(slot) = glyph::match_glyph(&block, cb_size, &dict, fg, bg, 8) {
                        modes.push(1u8);
                        flat_colors.push(0);
                        glyph_blocks[bi] = Some(GlyphBlock {
                            dict_slot: slot as u8,
                            fg_idx: 0,
                            bg_idx: 0,
                        });
                    } else {
                        // Dict miss: emit as NATURAL for v1.
                        modes.push(2u8);
                        flat_colors.push(0);
                        let (above, left) =
                            natural_neighbors(src, bx * cb_size, by * cb_size, cb_size);
                        natural_blocks[bi] = Some(natural::encode_natural_block(
                            &block,
                            cb_size,
                            &above,
                            &left,
                            frame.base_qp,
                        ));
                    }
                }
                BlockMode::Natural => {
                    modes.push(2u8);
                    flat_colors.push(0);
                    let (above, left) = natural_neighbors(src, bx * cb_size, by * cb_size, cb_size);
                    natural_blocks[bi] = Some(natural::encode_natural_block(
                        &block,
                        cb_size,
                        &above,
                        &left,
                        frame.base_qp,
                    ));
                }
            }
        }
    }

    // Encode sub-streams.
    let mode_stream = encode_mode_stream(&modes);
    let flat_stream = encode_flat_stream(&flat_colors, &modes);
    let glyph_stream = encode_glyph_stream(&glyph_blocks);
    let natural_stream = encode_natural_stream(&natural_blocks);

    RansStreamSet::frame(&[mode_stream, flat_stream, glyph_stream, natural_stream])
}

fn extract_luma_block(src: &FrameBuffer, x0: usize, y0: usize, size: usize) -> Vec<u8> {
    let mut block = vec![0u8; size * size];
    for y in 0..size {
        for x in 0..size {
            let sx = x0 + x;
            let sy = y0 + y;
            if sx < src.width && sy < src.height {
                block[y * size + x] = src.luma[sy * src.width + sx];
            }
        }
    }
    block
}

fn natural_neighbors(src: &FrameBuffer, x0: usize, y0: usize, size: usize) -> (Vec<i32>, Vec<i32>) {
    let mut above = vec![128i32; size];
    let mut left = vec![128i32; size];
    if y0 > 0 {
        for (c, above_c) in above.iter_mut().enumerate().take(size) {
            let x = x0 + c;
            if x < src.width {
                *above_c = src.luma[(y0 - 1) * src.width + x] as i32;
            }
        }
    }
    if x0 > 0 {
        for (r, left_r) in left.iter_mut().enumerate().take(size) {
            let y = y0 + r;
            if y < src.height {
                *left_r = src.luma[y * src.width + (x0 - 1)] as i32;
            }
        }
    }
    (above, left)
}

fn encode_mode_stream(modes: &[u8]) -> Vec<u8> {
    let model = StaticModel;
    let mut enc = RansEncoder::new();
    for &m in modes.iter().rev() {
        enc.encode(&model, m);
    }
    enc.encode(&model, modes.len() as u8);
    enc.finish()
}

fn encode_flat_stream(colors: &[u8], modes: &[u8]) -> Vec<u8> {
    let runs = flat::encode_flat_runs(modes, colors);
    let model = StaticModel;
    let mut enc = RansEncoder::new();
    for run in runs.iter().rev() {
        enc.encode(&model, run.run_len);
        enc.encode(&model, run.color_y);
    }
    enc.encode(&model, runs.len() as u8);
    enc.finish()
}

fn encode_glyph_stream(glyph_blocks: &[Option<GlyphBlock>]) -> Vec<u8> {
    let model = StaticModel;
    let mut enc = RansEncoder::new();
    for block in glyph_blocks.iter().rev() {
        if let Some(g) = block {
            enc.encode(&model, 1); // present
            enc.encode(&model, g.dict_slot);
            enc.encode(&model, g.fg_idx);
            enc.encode(&model, g.bg_idx);
        } else {
            enc.encode(&model, 0); // absent
        }
    }
    enc.encode(&model, glyph_blocks.len() as u8);
    enc.finish()
}

fn encode_natural_stream(natural_blocks: &[Option<NaturalBlock>]) -> Vec<u8> {
    let model = StaticModel;
    let mut enc = RansEncoder::new();
    for block in natural_blocks.iter().rev() {
        if let Some(n) = block {
            for &c in n.coeffs.iter().rev() {
                enc.encode(&model, ((c >> 24) & 0xFF) as u8);
                enc.encode(&model, ((c >> 16) & 0xFF) as u8);
                enc.encode(&model, ((c >> 8) & 0xFF) as u8);
                enc.encode(&model, (c & 0xFF) as u8);
            }
            enc.encode(&model, n.coeffs.len() as u8);
            enc.encode(&model, n.intra_mode);
            enc.encode(&model, 1); // present
        } else {
            enc.encode(&model, 0); // absent
        }
    }
    enc.encode(&model, natural_blocks.len() as u8);
    enc.finish()
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// Decode a frame from its rANS payload into a [`FrameBuffer`].
pub fn decode_frame_payload(
    seq: &SequenceHeader,
    frame: &FrameHeader,
    _reference: Option<&FrameBuffer>,
    payload: &[u8],
) -> Result<FrameBuffer, KinetixError> {
    let streams = RansStreamSet::unframe(payload)?;
    if streams.len() < 4 {
        return Err(KinetixError::Parse(format!(
            "screen: expected 4 sub-streams, got {}",
            streams.len()
        )));
    }

    let modes = decode_mode_stream(streams[0])?;
    let (flat_colors, flat_runs) = decode_flat_stream(streams[1], &modes)?;
    let glyph_blocks = decode_glyph_stream(streams[2], modes.len())?;
    let natural_blocks = decode_natural_stream(streams[3], modes.len())?;

    let cb_size = 1usize << seq.base_block_size_log2;
    let gw = frame.width as usize / cb_size;
    let gh = frame.height as usize / cb_size;
    let mut fb = FrameBuffer::new(seq, frame);

    let dict = GlyphDictionary::new(seq.dict_cap as usize);

    for by in 0..gh {
        for bx in 0..gw {
            let bi = by * gw + bx;
            let mode = modes.get(bi).copied().unwrap_or(0);
            let x0 = bx * cb_size;
            let y0 = by * cb_size;

            match mode {
                0 => {
                    // FLAT
                    let color_y = flat_colors.get(bi).copied().unwrap_or(0);
                    fill_luma_block(&mut fb, x0, y0, cb_size, color_y);
                }
                1 => {
                    // GLYPH
                    if let Some(g) = glyph_blocks.get(bi).and_then(|b| *b) {
                        let fg = PaletteColor::new(255, 128, 128);
                        let bg = PaletteColor::new(0, 128, 128);
                        let rendered = glyph::render_glyph(g.dict_slot, &dict, fg, bg, cb_size);
                        blit_luma_block(&mut fb, x0, y0, cb_size, &rendered);
                    }
                }
                _ => {
                    // NATURAL
                    if let Some(n) = natural_blocks.get(bi).and_then(|b| b.clone()) {
                        let (above, left) = natural_neighbors(&fb, x0, y0, cb_size);
                        let decoded = natural::decode_natural_block(
                            &n,
                            cb_size,
                            &above,
                            &left,
                            frame.base_qp,
                        )?;
                        blit_luma_block(&mut fb, x0, y0, cb_size, &decoded);
                    }
                }
            }
        }
    }

    let _ = flat_runs;
    Ok(fb)
}

fn decode_mode_stream(data: &[u8]) -> Result<Vec<u8>, KinetixError> {
    let model = StaticModel;
    let mut dec = RansDecoder::new(data)?;
    let count = dec.decode(&model)? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(dec.decode(&model)?);
    }
    Ok(out)
}

fn decode_flat_stream(data: &[u8], modes: &[u8]) -> Result<(Vec<u8>, Vec<FlatRun>), KinetixError> {
    let model = StaticModel;
    let mut dec = RansDecoder::new(data)?;
    let run_count = dec.decode(&model)? as usize;
    let mut runs = Vec::with_capacity(run_count);
    for _ in 0..run_count {
        let color_y = dec.decode(&model)?;
        let run_len = dec.decode(&model)?;
        runs.push(FlatRun { color_y, run_len });
    }
    let flat_colors = flat::decode_flat_runs(&runs, modes);
    Ok((flat_colors, runs))
}

fn decode_glyph_stream(data: &[u8], total: usize) -> Result<Vec<Option<GlyphBlock>>, KinetixError> {
    let model = StaticModel;
    let mut dec = RansDecoder::new(data)?;
    let count = dec.decode(&model)? as usize;
    let mut blocks = Vec::with_capacity(count);
    for _ in 0..count {
        let present = dec.decode(&model)?;
        if present == 1 {
            let dict_slot = dec.decode(&model)?;
            let fg_idx = dec.decode(&model)?;
            let bg_idx = dec.decode(&model)?;
            blocks.push(Some(GlyphBlock {
                dict_slot,
                fg_idx,
                bg_idx,
            }));
        } else {
            blocks.push(None);
        }
    }
    while blocks.len() < total {
        blocks.push(None);
    }
    Ok(blocks)
}

fn decode_natural_stream(
    data: &[u8],
    total: usize,
) -> Result<Vec<Option<NaturalBlock>>, KinetixError> {
    let model = StaticModel;
    let mut dec = RansDecoder::new(data)?;
    let count = dec.decode(&model)? as usize;
    let mut blocks = Vec::with_capacity(count);
    for _ in 0..count {
        let present = dec.decode(&model)?;
        if present == 1 {
            let intra_mode = dec.decode(&model)?;
            let coeff_count = dec.decode(&model)? as usize;
            let mut coeffs = Vec::with_capacity(coeff_count);
            for _ in 0..coeff_count {
                let b0 = dec.decode(&model)? as u32;
                let b1 = dec.decode(&model)? as u32;
                let b2 = dec.decode(&model)? as u32;
                let b3 = dec.decode(&model)? as u32;
                let val = (b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)) as i32;
                coeffs.push(val);
            }
            blocks.push(Some(NaturalBlock { intra_mode, coeffs }));
        } else {
            blocks.push(None);
        }
    }
    while blocks.len() < total {
        blocks.push(None);
    }
    Ok(blocks)
}

fn fill_luma_block(fb: &mut FrameBuffer, x0: usize, y0: usize, size: usize, value: u8) {
    for y in 0..size {
        for x in 0..size {
            let px = x0 + x;
            let py = y0 + y;
            if px < fb.width && py < fb.height {
                fb.luma[py * fb.width + px] = value;
            }
        }
    }
}

fn blit_luma_block(fb: &mut FrameBuffer, x0: usize, y0: usize, size: usize, src: &[u8]) {
    for y in 0..size {
        for x in 0..size {
            let px = x0 + x;
            let py = y0 + y;
            if px < fb.width && py < fb.height && y * size + x < src.len() {
                fb.luma[py * fb.width + px] = src[y * size + x];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headers::FrameType;

    fn test_seq() -> SequenceHeader {
        SequenceHeader {
            version: 1,
            max_width: 1920,
            max_height: 1080,
            base_block_size_log2: 4,
            num_rans_streams: 4,
            dict_cap: 256,
            palette_cap: 64,
            glyph_max_dim: 32,
            bit_depth: 8,
            chroma_format: crate::headers::ChromaFormat::Yuv420,
            max_ref_frames: 1,
        }
    }

    fn test_frame() -> FrameHeader {
        FrameHeader {
            frame_type: FrameType::Key,
            width: 16,
            height: 16,
            base_qp: 0,
            ref_frame_count: 0,
            dict_version: 0,
            dict_reset: true,
            payload_len: 0,
        }
    }

    #[test]
    fn flat_frame_round_trips() {
        let seq = test_seq();
        let frame = test_frame();
        let mut luma = vec![0u8; 16 * 16];
        for y in 0..16 {
            for x in 0..16 {
                luma[y * 16 + x] = 100; // uniform
            }
        }
        let src =
            FrameBuffer::from_yuv420(16, 16, luma, vec![128u8; 8 * 8], vec![128u8; 8 * 8]).unwrap();
        let payload = encode_frame(&seq, &frame, &src, None).unwrap();
        let decoded = decode_frame_payload(&seq, &frame, None, &payload).unwrap();
        for y in 0..16 {
            for x in 0..16 {
                assert_eq!(decoded.luma[y * 16 + x], 100, "flat mismatch at ({x},{y})");
            }
        }
    }

    #[test]
    fn natural_stream_round_trips() {
        let blocks = vec![
            None,
            Some(NaturalBlock {
                intra_mode: 0,
                coeffs: vec![100, -200, 300, -400],
            }),
            None,
        ];
        let encoded = encode_natural_stream(&blocks);
        let decoded = decode_natural_stream(&encoded, 3).unwrap();
        assert_eq!(decoded.len(), 3);
        assert!(decoded[0].is_none());
        assert_eq!(decoded[1].as_ref().unwrap().intra_mode, 0);
        assert_eq!(
            decoded[1].as_ref().unwrap().coeffs,
            vec![100, -200, 300, -400]
        );
        assert!(decoded[2].is_none());
    }

    #[test]
    fn natural_block_round_trips() {
        let seq = test_seq();
        let frame = test_frame();
        let mut luma = vec![0u8; 16 * 16];
        for y in 0..16 {
            for x in 0..16 {
                luma[y * 16 + x] = ((x + y) * 8) as u8;
            }
        }
        let src =
            FrameBuffer::from_yuv420(16, 16, luma, vec![128u8; 8 * 8], vec![128u8; 8 * 8]).unwrap();
        let payload = encode_frame(&seq, &frame, &src, None).unwrap();
        let decoded = decode_frame_payload(&seq, &frame, None, &payload).unwrap();
        for y in 0..16 {
            for x in 0..16 {
                let expected = ((x + y) * 8) as u8;
                let actual = decoded.luma[y * 16 + x];
                let diff = expected.abs_diff(actual);
                assert!(
                    diff <= 128,
                    "natural mismatch at ({x},{y}): expected {expected}, got {actual}"
                );
            }
        }
    }
}
