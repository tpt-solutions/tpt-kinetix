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

    // One scratch set for the whole frame, reused for every block. The natural
    // path alone used ~8 allocations per block (extracted block, two neighbour
    // rows, prediction, residual, transformed, and the inverse transform's
    // temporaries).
    let mut scratch = natural::NaturalScratch::new(cb_size);
    // The extracted block gets its own per-frame buffer rather than living in
    // the scratch: it is read while the scratch's work buffers are borrowed
    // mutably, so keeping it separate avoids aliasing them.
    let mut block_buf = vec![0u8; cb_size * cb_size];

    // Classify each block.
    for by in 0..gh {
        for bx in 0..gw {
            let bi = by * gw + bx;
            natural::extract_luma_block_into(
                src,
                bx * cb_size,
                by * cb_size,
                cb_size,
                &mut block_buf,
            );
            let block = &block_buf[..cb_size * cb_size];
            let mode = classify_block_luma(block, cb_size, 4);

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
                    if let Some(slot) = glyph::match_glyph(block, cb_size, &dict, fg, bg, 8) {
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
                        natural::natural_neighbors_into(
                            src,
                            bx * cb_size,
                            by * cb_size,
                            cb_size,
                            &mut scratch.above,
                            &mut scratch.left,
                        );
                        let natural::NaturalScratch {
                            above,
                            left,
                            pred,
                            work,
                            tscratch,
                            ..
                        } = &mut scratch;
                        natural_blocks[bi] = Some(natural::encode_natural_block_into(
                            block,
                            cb_size,
                            &above[..cb_size],
                            &left[..cb_size],
                            frame.base_qp,
                            pred,
                            work,
                            tscratch,
                        ));
                    }
                }
                BlockMode::Natural => {
                    modes.push(2u8);
                    flat_colors.push(0);
                    natural::natural_neighbors_into(
                        src,
                        bx * cb_size,
                        by * cb_size,
                        cb_size,
                        &mut scratch.above,
                        &mut scratch.left,
                    );
                    let natural::NaturalScratch {
                        above,
                        left,
                        pred,
                        work,
                        tscratch,
                        ..
                    } = &mut scratch;
                    natural_blocks[bi] = Some(natural::encode_natural_block_into(
                        block,
                        cb_size,
                        &above[..cb_size],
                        &left[..cb_size],
                        frame.base_qp,
                        pred,
                        work,
                        tscratch,
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

/// Push a `u32` as four symbols. The rANS coder is a stack (the decoder pops
/// in reverse push order), so the bytes go in high-to-low to be read
/// low-byte-first on the other side.
fn push_u32(enc: &mut RansEncoder, model: &StaticModel, v: u32) {
    for shift in [24, 16, 8, 0] {
        enc.encode(model, ((v >> shift) & 0xFF) as u8);
    }
}

/// Pop a `u32` pushed by [`push_u32`] (little-endian, low byte read first).
fn pop_u32(dec: &mut RansDecoder, model: &StaticModel) -> Result<u32, KinetixError> {
    let mut v = 0u32;
    for shift in [0, 8, 16, 24] {
        let b = dec
            .decode(model)
            .map_err(|e| KinetixError::Parse(format!("screen: count byte: {e}")))?;
        v |= u32::from(b) << shift;
    }
    Ok(v)
}

fn encode_mode_stream(modes: &[u8]) -> Vec<u8> {
    let model = StaticModel;
    let mut enc = RansEncoder::new();
    for &m in modes.iter().rev() {
        enc.encode(&model, m);
    }
    push_u32(&mut enc, &model, modes.len() as u32);
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
    push_u32(&mut enc, &model, runs.len() as u32);
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
    push_u32(&mut enc, &model, glyph_blocks.len() as u32);
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
            push_u32(&mut enc, &model, n.coeffs.len() as u32);
            enc.encode(&model, n.intra_mode);
            enc.encode(&model, 1); // present
        } else {
            enc.encode(&model, 0); // absent
        }
    }
    push_u32(&mut enc, &model, natural_blocks.len() as u32);
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

    // One scratch set for the whole frame (see `encode_frame`).
    let mut scratch = natural::NaturalScratch::new(cb_size);

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
                        natural::natural_neighbors_into(
                            &fb,
                            x0,
                            y0,
                            cb_size,
                            &mut scratch.above,
                            &mut scratch.left,
                        );
                        let natural::NaturalScratch {
                            above,
                            left,
                            pred,
                            work,
                            residual,
                            tscratch,
                            out,
                            ..
                        } = &mut scratch;
                        natural::decode_natural_block_into(
                            &n,
                            cb_size,
                            &above[..cb_size],
                            &left[..cb_size],
                            frame.base_qp,
                            pred,
                            work,
                            residual,
                            tscratch,
                            out,
                        );
                        blit_luma_block(&mut fb, x0, y0, cb_size, &out[..cb_size * cb_size]);
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
    let count = pop_u32(&mut dec, &model)? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(dec.decode(&model)?);
    }
    Ok(out)
}

fn decode_flat_stream(data: &[u8], modes: &[u8]) -> Result<(Vec<u8>, Vec<FlatRun>), KinetixError> {
    let model = StaticModel;
    let mut dec = RansDecoder::new(data)?;
    let run_count = pop_u32(&mut dec, &model)? as usize;
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
    let count = pop_u32(&mut dec, &model)? as usize;
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
    let count = pop_u32(&mut dec, &model)? as usize;
    let mut blocks = Vec::with_capacity(count);
    for _ in 0..count {
        let present = dec.decode(&model)?;
        if present == 1 {
            let intra_mode = dec.decode(&model)?;
            let coeff_count = pop_u32(&mut dec, &model)? as usize;
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

    /// Golden vector: pins the exact encoded bytes and decoded pixels for a frame
    /// mixing flat regions, a glyph-like pattern, and noisy natural content. Any
    /// change here is a bitstream/output change and must be deliberate: update
    /// the constants and note it in the changelog.
    #[test]
    fn golden_vector_pins_bitstream_and_output() {
        let seq = test_seq();
        let mut frame = test_frame();
        frame.width = 32;
        frame.height = 32;
        let noisy = golden_source(7);
        let mut src = noisy.clone();
        // Top-left 16x16 flat, top-right 16x16 two-colour "glyph" stripes,
        // bottom half stays noisy (natural).
        for y in 0..16 {
            for x in 0..32 {
                src.luma[y * 32 + x] = if x < 16 {
                    100
                } else if (x / 2 + y / 2) % 2 == 0 {
                    20
                } else {
                    230
                };
            }
        }
        let payload = encode_frame(&seq, &frame, &src, None).unwrap();
        let dec = decode_frame_payload(&seq, &frame, None, &payload).unwrap();
        let got = [fnv1a(&[&payload]), fnv1a(&[&dec.luma, &dec.cb, &dec.cr])];
        eprintln!("GOLDEN {got:#x?}");
        assert_eq!(got, GOLDEN);
    }

    /// Changed 2026-10-02: stream counts are coded as 4 rANS symbols (u32 LE)
    /// instead of one byte, which wrapped at 256+ blocks/coefficients and
    /// corrupted every frame larger than 255 coding blocks (see
    /// `large_grid_round_trips_at_qp0`).
    const GOLDEN: [u64; 2] = [0xa8c387dda16ab119, 0xde6681ded1af0a84];

    /// Regression: 4x4 coding blocks on a 64x64 frame give a 16x16 = 256-block
    /// grid, which overflowed the old byte-wide mode count (256 mod 256 = 0) and
    /// left almost every block decoding as flat black. FLAT blocks quantise to
    /// the block mean (classify tolerance 4) and NATURAL blocks are exact at
    /// qp 0 up to prediction drift, so the whole frame must stay close to the
    /// source; under the overflow most pixels decode to 0.
    #[test]
    fn large_grid_round_trips_at_qp0() {
        let mut seq = test_seq();
        seq.base_block_size_log2 = 2; // 4x4 blocks
        let mut frame = test_frame();
        frame.width = 64;
        frame.height = 64;

        // UI-like content: light background, dark separators every 8 px, a
        // two-tone "text" band — enough to exercise FLAT and NATURAL blocks.
        let mut luma = vec![240u8; 64 * 64];
        for y in 0..64usize {
            for x in 0..64usize {
                if y % 8 == 0 || x % 8 == 0 {
                    luma[y * 64 + x] = 128;
                } else if (8..24).contains(&y) && x % 4 < 3 {
                    luma[y * 64 + x] = 16;
                }
            }
        }
        let src =
            FrameBuffer::from_yuv420(64, 64, luma, vec![128u8; 32 * 32], vec![128u8; 32 * 32])
                .unwrap();
        let payload = encode_frame(&seq, &frame, &src, None).unwrap();
        let dec = decode_frame_payload(&seq, &frame, None, &payload).unwrap();

        let src_mean: f64 = src.luma.iter().map(|v| u32::from(*v)).sum::<u32>() as f64 / 4096.0;
        let dec_mean: f64 = dec.luma.iter().map(|v| u32::from(*v)).sum::<u32>() as f64 / 4096.0;
        assert!(
            (src_mean - dec_mean).abs() < 8.0,
            "decoded frame mean {dec_mean:.1} vs source {src_mean:.1} — large-scale corruption"
        );
        let max_diff = src
            .luma
            .iter()
            .zip(dec.luma.iter())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert!(
            max_diff <= 32,
            "max per-pixel diff {max_diff} — block(s) decoded from the wrong mode"
        );
    }
}
