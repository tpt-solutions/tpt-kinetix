//! VP9 decoder state machine: packet-level sequencing (including superframe
//! splitting), reference frame management, frame-context probability
//! adaptation and the [`tpt_kinetix_core`] decode API.

use std::rc::Rc;

use tpt_kinetix_core::{
    capabilities::DecoderCapabilities, error::KinetixError, frame::VideoFrame, packet::Packet,
    pixel_format::PixelFormat, timestamp::Timestamp,
};

use crate::booldec::BoolDecoder;
use crate::frame::{Counts, FrameData, FrameDecodeCtx, FrameState, TileDecoder};
use crate::header::{
    derive_segment_features, parse_compressed_header, parse_uncompressed_header, FrameCtx,
    FrameHeader, FrameType, WorkingProbs,
};
use crate::loop_filter::{loopfilter_sb, FilterLut};

/// A VP9 decoder.
pub struct Vp9Decoder {
    strict: bool,
    refs: [Option<Rc<FrameData>>; 8],
    frame_ctxs: [FrameCtx; 4],
    /// The previously decoded frame (the reference decoder's pre-update
    /// `CUR_FRAME`), used for `REF_FRAME_SEGMAP` / `REF_FRAME_MVPAIR`.
    prev: Option<Rc<FrameData>>,
    /// `REF_FRAME_SEGMAP`: the segmentation map reference frame.
    segmap_src: Option<Rc<FrameData>>,
    prev_invisible: bool,
    prev_was_keyframe: bool,
    /// Loop filter header state carried between frames: the ref/mode deltas
    /// persist unless the frame resets or updates them.
    prev_lf: crate::header::LoopFilterHeader,
}

impl Default for Vp9Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Vp9Decoder {
    /// Create a new decoder in non-strict mode.
    pub fn new() -> Self {
        Self {
            strict: false,
            refs: Default::default(),
            frame_ctxs: [
                FrameCtx::default_ctx(),
                FrameCtx::default_ctx(),
                FrameCtx::default_ctx(),
                FrameCtx::default_ctx(),
            ],
            prev: None,
            segmap_src: None,
            prev_invisible: false,
            prev_was_keyframe: true,
            prev_lf: crate::header::LoopFilterHeader::default(),
        }
    }

    /// Enable strict mode: [`Vp9Decoder::decode`] returns
    /// [`KinetixError::NotPixelExact`] instead of untrusted frames while the
    /// decoder is not validated pixel-exact.
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Report what this decoder can do today.
    pub fn capabilities(&self) -> DecoderCapabilities {
        DecoderCapabilities {
            codec: "vp9",
            pixel_exact: true,
            supports_cabac: false,
            supports_cavlc: false,
            supports_intra_prediction: true,
            supports_inter_prediction: true,
            supports_deblocking: true,
            notes: "profile 0 (8-bit 4:2:0) decode; byte-exact vs ffmpeg/libvpx on the conformance corpus (lossless/lossy, intra/inter, odd sizes, tiles)",
        }
    }

    /// Decode a packet (a VP9 frame chunk or superframe).
    pub fn decode(&mut self, packet: &Packet) -> Result<Option<VideoFrame>, KinetixError> {
        if self.strict && !self.capabilities().pixel_exact {
            return Err(KinetixError::NotPixelExact(
                "vp9: decoder is not validated pixel-exact yet; see capabilities()".to_string(),
            ));
        }
        let mut last_out = None;
        for chunk in split_superframes(&packet.data) {
            if let Some(frame) = self.decode_frame_chunk(&chunk)? {
                last_out = Some(frame);
            }
        }
        Ok(last_out)
    }

    fn ref_dims(&self) -> [Option<(u32, u32)>; 8] {
        let mut dims = [None; 8];
        for (i, slot) in self.refs.iter().enumerate() {
            if let Some(f) = slot {
                dims[i] = Some((f.width, f.height));
            }
        }
        dims
    }

    fn decode_frame_chunk(&mut self, data: &[u8]) -> Result<Option<VideoFrame>, KinetixError> {
        let mut h = parse_uncompressed_header(data, &self.ref_dims(), &self.prev_lf)?;
        self.prev_lf = h.loop_filter.clone();

        if h.show_existing_frame {
            let f = self
                .refs
                .get(h.frame_to_show_map_idx)
                .cloned()
                .flatten()
                .ok_or_else(|| {
                    KinetixError::Parse("vp9: show_existing_frame references empty slot".into())
                })?;
            return Ok(Some(frame_to_video(&f)));
        }

        let keyframe = h.frame_type == FrameType::Key;

        // segmap / mvpair source bookkeeping (reference vp9_decode_frame)
        if keyframe || h.intra_only || !self.segmap_retained(&h) {
            self.segmap_src = if !keyframe && !h.intra_only && !h.error_resilient {
                self.prev.clone()
            } else {
                None
            };
        }
        let mvpair_src = if !keyframe && !h.intra_only && !h.error_resilient {
            self.prev.clone()
        } else {
            None
        };

        // use_last_frame_mvs: !error_resilient && last frame visible and
        // same-sized (reference decode_frame_header)
        if h.use_last_frame_mvs {
            h.use_last_frame_mvs = !self.prev_invisible;
            if let Some(p) = &self.prev {
                h.use_last_frame_mvs &= p.width == h.width && p.height == h.height;
            } else {
                h.use_last_frame_mvs = false;
            }
        }

        // probability context selection / reset
        let c = h.frame_context_idx;
        if keyframe || h.error_resilient || (h.intra_only && h.reset_frame_context == 3) {
            for ctx in &mut self.frame_ctxs {
                *ctx = FrameCtx::default_ctx();
            }
        } else if h.intra_only && h.reset_frame_context == 2 {
            self.frame_ctxs[c] = FrameCtx::default_ctx();
        }
        let mut probs = WorkingProbs::new(
            self.frame_ctxs[c].mode.clone(),
            self.frame_ctxs[c].coef_model.clone(),
        );

        // compressed header
        let ch_end = h.compressed_header_offset + h.compressed_header_size;
        let ch = &data[h.compressed_header_offset..ch_end];
        let mut bc = BoolDecoder::new(ch)?;
        parse_compressed_header(&mut bc, &mut h, &mut probs, &self.frame_ctxs[c])?;

        let seg = derive_segment_features(&h);

        let mut state = FrameState::new(&h);
        let mut counts = Counts::new();

        // per-reference MV scaling for resized references
        let mut mvscale = [[0u16; 2]; 3];
        let mut mvstep = [[0u8; 2]; 3];
        for i in 0..3 {
            let slot = h.ref_frame_idx[i];
            if let Some(r) = &self.refs[slot] {
                let (rw, rh) = (r.width, r.height);
                if rw == h.width && rh == h.height {
                    mvscale[i] = [0, 0];
                } else if h.width * 2 < rw
                    || h.height * 2 < rh
                    || h.width > rw * 16
                    || h.height > rh * 16
                {
                    mvscale[i] = [0xFFFF, 0xFFFF];
                } else {
                    mvscale[i] = [
                        ((rw << 14) / h.width) as u16,
                        ((rh << 14) / h.height) as u16,
                    ];
                    mvstep[i] = [
                        ((u32::from(mvscale[i][0]) * 16) >> 14) as u8,
                        ((u32::from(mvscale[i][1]) * 16) >> 14) as u8,
                    ];
                }
            }
        }

        // compound reference assignment from sign biases
        let (fixcompref, varcompref) =
            if h.sign_bias[0] != h.sign_bias[1] || h.sign_bias[0] != h.sign_bias[2] {
                if h.sign_bias[0] == h.sign_bias[1] {
                    (2, [0, 1])
                } else if h.sign_bias[0] == h.sign_bias[2] {
                    (1, [0, 2])
                } else {
                    (0, [1, 2])
                }
            } else {
                (0, [1, 2])
            };

        // frame-parallel mode saves the working probs before tile decode
        if h.refresh_frame_context && h.frame_parallel_decoding_mode {
            self.frame_ctxs[c].mode = probs.mode.clone();
            self.frame_ctxs[c].coef_model = probs.coef_model.clone();
        }

        // tile data
        let tile_data = &data[ch_end..];

        let tile_cols = h.tile.tile_cols();
        let tile_rows = h.tile.tile_rows();
        let mut offset = 0usize;
        for tr in 0..tile_rows {
            for tc in 0..tile_cols {
                let is_last = tr == tile_rows - 1 && tc == tile_cols - 1;
                let (size, chunk): (usize, &[u8]) = if is_last {
                    (tile_data.len() - offset, &tile_data[offset..])
                } else {
                    if offset + 4 > tile_data.len() {
                        return Err(KinetixError::Parse("vp9: truncated tile size field".into()));
                    }
                    let sz = u32::from_be_bytes([
                        tile_data[offset],
                        tile_data[offset + 1],
                        tile_data[offset + 2],
                        tile_data[offset + 3],
                    ]) as usize;
                    offset += 4;
                    if offset + sz > tile_data.len() {
                        return Err(KinetixError::Parse(
                            "vp9: tile data overruns the frame".into(),
                        ));
                    }
                    (sz, &tile_data[offset..offset + sz])
                };
                let _ = size;
                offset += chunk.len();

                // Tile boundaries are superblock-granular (reference
                // `set_tile_offset`): (idx * sb_count) >> log2, in SB units.
                let sb_cols = h.mi_cols.div_ceil(8);
                let sb_rows = h.mi_rows.div_ceil(8);
                let sb_col_start = (tc * sb_cols) >> h.tile.log2_tile_cols;
                let sb_col_end = ((tc + 1) * sb_cols) >> h.tile.log2_tile_cols;
                let sb_row_start = (tr * sb_rows) >> h.tile.log2_tile_rows;
                let sb_row_end = ((tr + 1) * sb_rows) >> h.tile.log2_tile_rows;
                let col_start = sb_col_start * 8;
                let col_end = sb_col_end * 8;
                let row_start = sb_row_start * 8;
                let row_end = sb_row_end * 8;

                let mut tbc = BoolDecoder::new(chunk)?;
                if tbc.read_bool(128) {
                    return Err(KinetixError::Parse("vp9: tile marker bit set".into()));
                }

                let fctx = FrameDecodeCtx {
                    refs: &self.refs,
                    mvpair: mvpair_src.as_deref().map(|f| f.mvrefs.as_slice()),
                    mvpair_w: mvpair_src.as_ref().map_or(0, |f| f.seg_stride),
                    prev_segmap: self.segmap_src.as_deref().map(|f| f.segmap.as_slice()),
                    prev_segmap_w: self.segmap_src.as_ref().map_or(0, |f| f.seg_stride),
                    mvscale,
                    mvstep,
                    fixcompref,
                    varcompref,
                };
                {
                    let mut tile = TileDecoder::new(
                        &h,
                        &probs,
                        &seg,
                        fctx,
                        &mut state,
                        &mut counts,
                        col_start,
                        col_end,
                    );
                    tile.tile_row_start = row_start;
                    tile.tile_row_end = row_end;
                    tile.decode_tile(&mut tbc)?;
                }
            }
        }

        // probability adaptation
        if h.refresh_frame_context && !h.frame_parallel_decoding_mode {
            adapt_probs(
                &mut self.frame_ctxs[c],
                &counts,
                &probs,
                keyframe || h.intra_only,
                self.prev_was_keyframe,
                &h,
            );
        }

        // loop filter
        if std::env::var_os("TPT_VP9_TRACE").is_some() {
            eprintln!("FRAMEMARK");
        }
        if let Some(spec) = std::env::var_os("TPT_VP9_BUF") {
            // debug: dump raw strided buffer rows, e.g. TPT_VP9_BUF=60:84:56:104
            // (y0:y1:x0:x1, exclusive row end), before the loop filter
            let s = spec.to_string_lossy().to_string();
            let v: Vec<usize> = s.split(':').filter_map(|t| t.parse().ok()).collect();
            if v.len() == 4 {
                let stride = state.frame.stride;
                for r in v[0]..v[1] {
                    let row: Vec<String> = (v[2]..v[3])
                        .map(|c| format!("{:02x}", state.frame.y[r * stride + c]))
                        .collect();
                    eprintln!("BUF r={r} {}", row.concat());
                }
            }
        }
        if h.loop_filter.level != 0 && std::env::var_os("TPT_VP9_NO_LF").is_none() {
            let luts = FilterLut::new(h.loop_filter.sharpness);
            for sb_row in 0..state.frame.sb64_rows() {
                for sb_col in 0..state.frame.sb64_cols {
                    let sf = state.lflvl[sb_row * state.frame.sb64_cols + sb_col].clone();
                    loopfilter_sb(&mut state.frame, &sf, &luts, sb_row, sb_col);
                }
            }
        }
        if let Some(spec) = std::env::var_os("TPT_VP9_BUF_POST") {
            // same dump syntax, after the loop filter
            let s = spec.to_string_lossy().to_string();
            let v: Vec<usize> = s.split(':').filter_map(|t| t.parse().ok()).collect();
            if v.len() == 4 {
                let stride = state.frame.stride;
                for r in v[0]..v[1] {
                    let row: Vec<String> = (v[2]..v[3])
                        .map(|c| format!("{:02x}", state.frame.y[r * stride + c]))
                        .collect();
                    eprintln!("BUFP r={r} {}", row.concat());
                }
            }
        }

        let frame_rc = Rc::new(std::mem::replace(
            &mut state.frame,
            FrameData::new(h.width, h.height),
        ));

        // ref frame setup
        for i in 0..8 {
            if h.refresh_frame_flags & (1 << i) != 0 {
                self.refs[i] = Some(frame_rc.clone());
            }
        }
        self.prev = Some(frame_rc.clone());
        self.prev_invisible = !h.show_frame;
        self.prev_was_keyframe = keyframe;

        if h.show_frame {
            return Ok(Some(frame_to_video(&frame_rc)));
        }
        Ok(None)
    }

    fn segmap_retained(&self, h: &FrameHeader) -> bool {
        // reference: retain_segmap_ref = REF_FRAME_SEGMAP exists &&
        // (!segmentation.enabled || !update_map)
        self.segmap_src.is_some() && (!h.segmentation.enabled || !h.segmentation.update_map)
    }
}

fn frame_to_video(f: &FrameData) -> VideoFrame {
    let w = f.width as usize;
    let h = f.height as usize;
    let mut data = Vec::with_capacity(w * h + 2 * (w / 2) * (h / 2));
    for r in 0..h {
        data.extend_from_slice(&f.y[r * f.stride..r * f.stride + w]);
    }
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let cs = f.stride >> 1;
    for r in 0..ch {
        data.extend_from_slice(&f.u[r * cs..r * cs + cw]);
    }
    for r in 0..ch {
        data.extend_from_slice(&f.v[r * cs..r * cs + cw]);
    }
    VideoFrame {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data,
        width: f.width,
        height: f.height,
        pixel_format: PixelFormat::Yuv420p,
        is_key_frame: false,
    }
}

/// VP9 superframe parsing: frames concatenated with a trailing index whose
/// last byte carries the marker bits `0b11`.
fn split_superframes(data: &[u8]) -> Vec<Vec<u8>> {
    if data.is_empty() {
        return Vec::new();
    }
    let marker = data[data.len() - 1];
    if (marker >> 6) != 0b11 {
        return vec![data.to_vec()];
    }
    let frames = usize::from((marker >> 3) & 0b111) + 1;
    let mag = usize::from(marker & 0b111);
    let index_size = 2 + mag * frames;
    if data.len() < index_size || frames > 8 || mag == 0 || mag > 4 {
        return vec![data.to_vec()];
    }
    let idx_start = data.len() - index_size;
    if (data[idx_start] >> 6) != 0b11 {
        return vec![data.to_vec()];
    }
    let mut out = Vec::with_capacity(frames);
    let mut pos = 0usize;
    for i in 0..frames {
        let ib = idx_start + 2 + i * mag;
        let mut sz = 0usize;
        for b in 0..mag {
            sz |= usize::from(data[ib + b]) << (8 * b);
        }
        if pos + sz > idx_start {
            return vec![data.to_vec()];
        }
        out.push(data[pos..pos + sz].to_vec());
        pos += sz;
    }
    if pos == idx_start {
        out
    } else {
        vec![data.to_vec()]
    }
}

/// Probability adaptation (reference `ff_vp9_adapt_probs`).
fn adapt_probs(
    ctx: &mut FrameCtx,
    counts: &Counts,
    working: &WorkingProbs,
    key_or_intra: bool,
    last_was_key: bool,
    h: &FrameHeader,
) {
    let uf: u32 = if key_or_intra || !last_was_key {
        112
    } else {
        128
    };
    let p = &mut ctx.mode;

    let adapt = |pp: &mut u8, ct0: u32, ct1: u32, max_count: u32, update_factor: u32| {
        let ct = ct0 + ct1;
        if ct == 0 {
            return;
        }
        let uf2 = update_factor * ct.min(max_count) / max_count;
        let p1 = u32::from(*pp);
        let p2 = ((((ct0 as u64) << 8) + u64::from(ct >> 1)) / u64::from(ct)) as u32;
        let p2 = p2.clamp(1, 255);
        *pp = (p1 as i32 + (((p2 as i32 - p1 as i32) * uf2 as i32 + 128) >> 8)) as u8;
    };

    // coefficients
    for tx in 0..4 {
        for bt in 0..2 {
            for pt in 0..2 {
                for band in 0..6 {
                    for c in 0..6 {
                        if band == 0 && c >= 3 {
                            break;
                        }
                        let b = Counts::coef_bin(tx, bt, pt, band, c);
                        let e = counts.eob[b];
                        let cc = counts.coef[b];
                        adapt(&mut ctx.coef_model[b], e[0], e[1], 24, uf);
                        adapt(&mut ctx.coef_model[b + 1], cc[0], cc[1] + cc[2], 24, uf);
                        adapt(&mut ctx.coef_model[b + 2], cc[1], cc[2], 24, uf);
                    }
                }
            }
        }
    }

    if key_or_intra {
        // the reference copies the frame's (post-compressed-header) skip and
        // tx probs into the context here
        p.skip = working.mode.skip;
        p.tx8p = working.mode.tx8p;
        p.tx16p = working.mode.tx16p;
        p.tx32p = working.mode.tx32p;
        return;
    }

    for i in 0..3 {
        adapt(
            &mut p.skip[i],
            counts.skip[i][0],
            counts.skip[i][1],
            20,
            128,
        );
    }
    for i in 0..4 {
        adapt(
            &mut p.intra[i],
            counts.intra[i][0],
            counts.intra[i][1],
            20,
            128,
        );
    }
    if h.comp_pred_mode == 2 {
        for i in 0..5 {
            adapt(
                &mut p.comp[i],
                counts.comp[i][0],
                counts.comp[i][1],
                20,
                128,
            );
        }
    }
    if h.comp_pred_mode != 1 {
        for i in 0..5 {
            adapt(
                &mut p.comp_ref[i],
                counts.comp_ref[i][0],
                counts.comp_ref[i][1],
                20,
                128,
            );
        }
    }
    if h.comp_pred_mode != 0 {
        for i in 0..5 {
            adapt(
                &mut p.single_ref[i][0],
                counts.single_ref[i][0][0],
                counts.single_ref[i][0][1],
                20,
                128,
            );
            adapt(
                &mut p.single_ref[i][1],
                counts.single_ref[i][1][0],
                counts.single_ref[i][1][1],
                20,
                128,
            );
        }
    }
    for i in 0..4 {
        for j in 0..4 {
            let c = counts.partition[i][j];
            adapt(&mut p.partition[i][j][0], c[0], c[1] + c[2] + c[3], 20, 128);
            adapt(&mut p.partition[i][j][1], c[1], c[2] + c[3], 20, 128);
            adapt(&mut p.partition[i][j][2], c[2], c[3], 20, 128);
        }
    }
    if h.txfm_mode == crate::header::TxfmMode::Switchable {
        for i in 0..2 {
            adapt(
                &mut p.tx8p[i],
                counts.tx8p[i][0],
                counts.tx8p[i][1],
                20,
                128,
            );
            let c16 = counts.tx16p[i];
            adapt(&mut p.tx16p[i][0], c16[0], c16[1] + c16[2], 20, 128);
            adapt(&mut p.tx16p[i][1], c16[1], c16[2], 20, 128);
            let c32 = counts.tx32p[i];
            adapt(
                &mut p.tx32p[i][0],
                c32[0],
                c32[1] + c32[2] + c32[3],
                20,
                128,
            );
            adapt(&mut p.tx32p[i][1], c32[1], c32[2] + c32[3], 20, 128);
            adapt(&mut p.tx32p[i][2], c32[2], c32[3], 20, 128);
        }
    }
    if h.filter_mode == 3 {
        for i in 0..4 {
            let c = counts.filter[i];
            adapt(&mut p.filter[i][0], c[0], c[1] + c[2], 20, 128);
            adapt(&mut p.filter[i][1], c[1], c[2], 20, 128);
        }
    }
    for i in 0..7 {
        let c = counts.mv_mode[i];
        // tree node counts for NEARESTMV | NEARMV | ZEROMV | NEWMV
        adapt(&mut p.mv_mode[i][0], c[0], c[1] + c[2] + c[3], 20, 128);
        adapt(&mut p.mv_mode[i][1], c[1], c[2] + c[3], 20, 128);
        adapt(&mut p.mv_mode[i][2], c[2], c[3], 20, 128);
    }
    {
        let c = counts.mv_joint;
        adapt(&mut p.mv_joint[0], c[0], c[1] + c[2] + c[3], 20, 128);
        adapt(&mut p.mv_joint[1], c[1], c[2] + c[3], 20, 128);
        adapt(&mut p.mv_joint[2], c[2], c[3], 20, 128);
    }
    for i in 0..2 {
        let mc = &counts.mv_comp[i];
        let mp = &mut p.mv_comp[i];
        adapt(&mut mp.sign, mc.sign[0], mc.sign[1], 20, 128);
        let sum0: u32 = mc.classes[1..].iter().sum();
        adapt(&mut mp.classes[0], mc.classes[0], sum0, 20, 128);
        let sum1 = sum0 - mc.classes[1];
        adapt(&mut mp.classes[1], mc.classes[1], sum1, 20, 128);
        let sum2 = sum1 - mc.classes[2] - mc.classes[3];
        adapt(
            &mut mp.classes[2],
            mc.classes[2] + mc.classes[3],
            sum2,
            20,
            128,
        );
        adapt(&mut mp.classes[3], mc.classes[2], mc.classes[3], 20, 128);
        let sum3 = sum2 - mc.classes[4] - mc.classes[5];
        adapt(
            &mut mp.classes[4],
            mc.classes[4] + mc.classes[5],
            sum3,
            20,
            128,
        );
        adapt(&mut mp.classes[5], mc.classes[4], mc.classes[5], 20, 128);
        let sum4 = sum3 - mc.classes[6];
        adapt(&mut mp.classes[6], mc.classes[6], sum4, 20, 128);
        adapt(
            &mut mp.classes[7],
            mc.classes[7] + mc.classes[8],
            mc.classes[9] + mc.classes[10],
            20,
            128,
        );
        adapt(&mut mp.classes[8], mc.classes[7], mc.classes[8], 20, 128);
        adapt(&mut mp.classes[9], mc.classes[9], mc.classes[10], 20, 128);

        adapt(&mut mp.class0, mc.class0[0], mc.class0[1], 20, 128);
        for j in 0..10 {
            adapt(&mut mp.bits[j], mc.bits[j][0], mc.bits[j][1], 20, 128);
        }
        for j in 0..2 {
            let c = mc.class0_fp[j];
            adapt(&mut mp.class0_fp[j][0], c[0], c[1] + c[2] + c[3], 20, 128);
            adapt(&mut mp.class0_fp[j][1], c[1], c[2] + c[3], 20, 128);
            adapt(&mut mp.class0_fp[j][2], c[2], c[3], 20, 128);
        }
        let c = mc.fp;
        adapt(&mut mp.fp[0], c[0], c[1] + c[2] + c[3], 20, 128);
        adapt(&mut mp.fp[1], c[1], c[2] + c[3], 20, 128);
        adapt(&mut mp.fp[2], c[2], c[3], 20, 128);
        if h.allow_high_precision_mv {
            adapt(&mut mp.class0_hp, mc.class0_hp[0], mc.class0_hp[1], 20, 128);
            adapt(&mut mp.hp, mc.hp[0], mc.hp[1], 20, 128);
        }
    }

    // y intra modes
    for i in 0..4 {
        let c = counts.y_mode[i];
        adapt_y_mode(&mut p.y_mode[i], &c);
    }
    for i in 0..10 {
        let c = counts.uv_mode[i];
        adapt_y_mode(&mut p.uv_mode[i], &c);
    }
}

/// The reference's count-tree walk for y/uv intra mode probabilities (the
/// tree order differs from the mode value order).
fn adapt_y_mode(pp: &mut [u8; 9], c: &[u32; 10]) {
    const DC: usize = 0;
    const TM: usize = 9;
    const V: usize = 1;
    const H: usize = 2;
    const D45: usize = 3;
    const D135: usize = 4;
    const D113: usize = 5;
    const D157: usize = 6;
    const D203: usize = 7;
    const D67: usize = 8;

    let adapt = |pp: &mut u8, ct0: u32, ct1: u32| {
        let ct = ct0 + ct1;
        if ct == 0 {
            return;
        }
        let uf2 = 128u32 * ct.min(20) / 20;
        let p1 = u32::from(*pp);
        let p2 = ((((ct0 as u64) << 8) + u64::from(ct >> 1)) / u64::from(ct)) as u32;
        let p2 = p2.clamp(1, 255);
        *pp = (p1 as i32 + (((p2 as i32 - p1 as i32) * uf2 as i32 + 128) >> 8)) as u8;
    };

    let mut sum = c[DC] + c[TM] + c[V] + c[H] + c[D45] + c[D113] + c[D135] + c[D203] + c[D67];
    adapt(&mut pp[0], c[DC], sum);
    sum -= c[TM];
    adapt(&mut pp[1], c[TM], sum);
    sum -= c[V];
    adapt(&mut pp[2], c[V], sum);
    let mut s2 = c[H] + c[D135] + c[D113];
    sum -= s2;
    adapt(&mut pp[3], s2, sum);
    s2 -= c[H];
    adapt(&mut pp[4], c[H], s2);
    adapt(&mut pp[5], c[D135], c[D113]);
    sum -= c[D45];
    adapt(&mut pp[6], c[D45], sum);
    sum -= c[D203];
    adapt(&mut pp[7], c[D203], sum);
    adapt(&mut pp[8], c[D157], c[D67]);
}
