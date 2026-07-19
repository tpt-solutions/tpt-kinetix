//! H.264 / AVC stateful decoder.
//!
//! Wires together NAL parsing, SPS/PPS stores, and macroblock reconstruction.
//! Slice-level parallelism is injected via `rayon` at the macroblock-row level.

use std::collections::HashMap;

use rayon::prelude::*;
use tpt_kinetix_core::{
    capabilities::DecoderCapabilities, error::KinetixError, frame::VideoFrame, packet::Packet,
    pixel_format::PixelFormat,
};

use crate::{
    macroblock::{MbType, Macroblock},
    nal::{parse_nal_units_from_annexb, NalUnitType},
    pps::PicParameterSet,
    sps::SeqParameterSet,
};

/// Stateful H.264 / AVC decoder.
///
/// Feed compressed [`Packet`]s via [`H264Decoder::decode`] and receive decoded [`VideoFrame`]s.
pub struct H264Decoder {
    sps_store: HashMap<u32, SeqParameterSet>,
    pps_store: HashMap<u32, PicParameterSet>,
    /// Decoded Picture Buffer — stores reference frames for inter prediction.
    dpb: Vec<VideoFrame>,
    frame_count: u64,
    /// When `true` (the default), macroblock rows are reconstructed with `rayon`
    /// parallel iterators. Set to `false` to force serial reconstruction, which
    /// is useful for benchmarking the parallel speedup.
    parallel: bool,
    /// When `true`, [`H264Decoder::decode`] returns
    /// [`KinetixError::NotPixelExact`] instead of emitting placeholder frames.
    /// Off by default so existing pipelines keep working; opt in when callers
    /// need correctness guarantees.
    strict: bool,
}

impl H264Decoder {
    pub fn new() -> Self {
        Self {
            sps_store: HashMap::new(),
            pps_store: HashMap::new(),
            dpb: Vec::new(),
            frame_count: 0,
            parallel: true,
            strict: false,
        }
    }

    /// Reports what this decoder can and cannot do.
    ///
    /// The H.264 decoder is **not yet pixel-exact**: it parses the bitstream and
    /// runs a scaffold reconstruction (CAVLC scaffold only, no CABAC, no
    /// intra/inter prediction, no deblocking). Callers should check
    /// [`DecoderCapabilities::pixel_exact`] before trusting output frames.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use tpt_kinetix_h264::H264Decoder;
    ///
    /// let caps = H264Decoder::new().capabilities();
    /// assert!(!caps.pixel_exact);
    /// assert!(caps.is_incomplete());
    /// ```
    pub fn capabilities(&self) -> DecoderCapabilities {
        DecoderCapabilities {
            codec: "H.264",
            pixel_exact: false,
            supports_cabac: false,
            supports_cavlc: true,
            supports_intra_prediction: false,
            supports_inter_prediction: false,
            supports_deblocking: false,
            notes: "bitstream + CAVLC scaffold only; reconstruction emits \
                    placeholder pixels (no CABAC/prediction/deblocking)",
        }
    }

    /// Enable strict mode.
    ///
    /// In strict mode, [`H264Decoder::decode`] returns
    /// [`KinetixError::NotPixelExact`] for any slice it cannot decode
    /// pixel-exactly, rather than returning placeholder frames.
    pub fn set_strict(&mut self, strict: bool) {
        self.strict = strict;
    }

    /// Builder-style variant of [`H264Decoder::set_strict`].
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Enable or disable `rayon` parallel macroblock-row reconstruction.
    ///
    /// Parallel reconstruction is enabled by default. Disabling it is primarily
    /// useful for benchmarks that compare single-threaded vs. parallel throughput.
    pub fn set_parallel(&mut self, parallel: bool) {
        self.parallel = parallel;
    }

    /// Builder-style variant of [`H264Decoder::set_parallel`].
    pub fn with_parallel(mut self, parallel: bool) -> Self {
        self.parallel = parallel;
        self
    }

    /// Directly insert a parsed SPS into the decoder's parameter-set store.
    ///
    /// This is primarily intended for tests and benchmarks that need to drive
    /// slice reconstruction at a chosen resolution without hand-crafting a
    /// byte-exact SPS bitstream.
    #[doc(hidden)]
    pub fn insert_sps(&mut self, sps: SeqParameterSet) {
        self.sps_store.insert(sps.seq_parameter_set_id, sps);
    }

    /// Decode a compressed bitstream [`Packet`] into a [`VideoFrame`].
    ///
    /// Returns `Ok(None)` when the decoder needs more data before a frame can
    /// be emitted. Returns `Ok(Some(frame))` when a frame is ready.
    ///
    /// NAL units are extracted from Annex B byte-stream format.
    /// Slice-level parallelism is applied via `rayon` at the macroblock-row boundary.
    pub fn decode(&mut self, packet: &Packet) -> Result<Option<VideoFrame>, KinetixError> {
        let nal_units = parse_nal_units_from_annexb(&packet.data);
        if nal_units.is_empty() {
            return Ok(None);
        }

        let mut output_frame: Option<VideoFrame> = None;

        for nal in &nal_units {
            match nal.nal_unit_type {
                NalUnitType::Sps => {
                    if let Ok(sps) = SeqParameterSet::parse(&nal.rbsp) {
                        self.sps_store.insert(sps.seq_parameter_set_id, sps);
                    }
                }
                NalUnitType::Pps => {
                    if let Ok(pps) = PicParameterSet::parse(&nal.rbsp) {
                        self.pps_store.insert(pps.pic_parameter_set_id, pps);
                    }
                }
                NalUnitType::IdrSlice | NalUnitType::NonIdrSlice => {
                    if self.strict {
                        return Err(KinetixError::NotPixelExact(
                            "H.264: no CABAC/intra/inter prediction or deblocking implemented \
                             (see H264Decoder::capabilities)"
                                .to_string(),
                        ));
                    }

                    // Look up the active SPS/PPS (use the first available as a fallback).
                    let sps = match self.sps_store.values().next() {
                        Some(s) => s,
                        None => continue,
                    };

                    let width = sps.pic_width_pixels();
                    let height = sps.pic_height_pixels();
                    if width == 0 || height == 0 {
                        continue;
                    }

                    let frame = self.decode_slice(nal.nal_unit_type, width, height, packet)?;
                    output_frame = Some(frame);
                }
                _ => {}
            }
        }

        Ok(output_frame)
    }

    /// Flush any buffered frames from the decoded picture buffer.
    pub fn flush(&mut self) -> Result<Vec<VideoFrame>, KinetixError> {
        let frames = self.dpb.drain(..).collect();
        Ok(frames)
    }

    fn decode_slice(
        &mut self,
        nal_type: NalUnitType,
        width: u32,
        height: u32,
        packet: &Packet,
    ) -> Result<VideoFrame, KinetixError> {
        let mb_cols = width.div_ceil(16);
        let mb_rows = height.div_ceil(16);
        let total_mbs = (mb_cols * mb_rows) as usize;

        // Build a row-indexed list of macroblock stubs.
        let mb_rows_data: Vec<Vec<Macroblock>> = (0..mb_rows)
            .map(|_row| (0..mb_cols).map(|_col| Macroblock::new_skip()).collect())
            .collect();

        let any_intra = mb_rows_data
            .iter()
            .flatten()
            .any(|mb| matches!(mb.mb_type, MbType::Intra4x4 | MbType::Intra16x16 { .. }));

        let luma_stride = width as usize;
        let chroma_stride = (width / 2) as usize;
        let luma_size = luma_stride * height as usize;
        let chroma_size = chroma_stride * (height as usize / 2);

        let mut luma = vec![128u8; luma_size];
        let mut chroma_cb = vec![128u8; chroma_size];
        let mut chroma_cr = vec![128u8; chroma_size];

        if any_intra {
            // Intra prediction needs already-reconstructed top/left neighbours, so
            // reconstruction is strictly top-to-bottom, left-to-right (the H.264
            // decode order). `rayon` row-parallelism is not safe here because the
            // row above must be fully committed before the row below is predicted.
            for (row_idx, row_mbs) in mb_rows_data.iter().enumerate() {
                for (col_idx, mb) in row_mbs.iter().enumerate() {
                    reconstruct_mb(
                        mb,
                        &mut luma,
                        &mut chroma_cb,
                        &mut chroma_cr,
                        col_idx as u32,
                        row_idx as u32,
                        luma_stride,
                        chroma_stride,
                    );
                }
            }
        } else {
            // All-skip scaffold: rows are independent, so reconstruct them with
            // `rayon` (respecting the `parallel` toggle) and copy into the planes.
            let reconstruct_row = |row_idx: usize, row_mbs: &Vec<Macroblock>| {
                let row_height = if (row_idx + 1) * 16 > height as usize {
                    height as usize - row_idx * 16
                } else {
                    16
                };
                let luma_row_size = luma_stride * row_height;
                let chroma_row_size = chroma_stride * (row_height / 2).max(1);
                let mut luma_row = vec![128u8; luma_row_size];
                let mut cb_row = vec![128u8; chroma_row_size];
                let mut cr_row = vec![128u8; chroma_row_size];
                for (col_idx, mb) in row_mbs.iter().enumerate() {
                    mb.reconstruct_luma(&mut luma_row, col_idx as u32, 0, luma_stride);
                    mb.reconstruct_chroma(
                        &mut cb_row,
                        &mut cr_row,
                        col_idx as u32,
                        0,
                        chroma_stride,
                    );
                }
                (luma_row, cb_row, cr_row)
            };

            let row_results: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> = if self.parallel {
                mb_rows_data
                    .par_iter()
                    .enumerate()
                    .map(|(row_idx, row_mbs)| reconstruct_row(row_idx, row_mbs))
                    .collect()
            } else {
                mb_rows_data
                    .iter()
                    .enumerate()
                    .map(|(row_idx, row_mbs)| reconstruct_row(row_idx, row_mbs))
                    .collect()
            };

            for (row_idx, (luma_row, cb_row, cr_row)) in row_results.iter().enumerate() {
                let y_off = row_idx * 16 * luma_stride;
                let copy_len = luma_row.len().min(luma.len() - y_off);
                luma[y_off..y_off + copy_len].copy_from_slice(&luma_row[..copy_len]);

                let c_off = row_idx * 8 * chroma_stride;
                let cc_len = cb_row.len().min(chroma_cb.len().saturating_sub(c_off));
                if cc_len > 0 {
                    chroma_cb[c_off..c_off + cc_len].copy_from_slice(&cb_row[..cc_len]);
                    chroma_cr[c_off..c_off + cc_len].copy_from_slice(&cr_row[..cc_len]);
                }
            }
        }

        let _ = total_mbs;

        let mut data = luma;
        data.extend(chroma_cb);
        data.extend(chroma_cr);

        self.frame_count += 1;
        Ok(VideoFrame {
            pts: packet.pts,
            dts: packet.dts,
            data,
            width,
            height,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: matches!(nal_type, NalUnitType::IdrSlice),
        })
    }
}

/// Reconstruct a single macroblock into the luma/chroma planes, applying the
/// correct prediction path for its type.
///
/// Neighbour samples (top row, left column, and the above-left corner) are read
/// back from `luma`/`chroma_cb`/`chroma_cr`, which must already hold the
/// reconstructed output of the macroblocks above and to the left.
fn reconstruct_mb(
    mb: &Macroblock,
    luma: &mut [u8],
    chroma_cb: &mut [u8],
    chroma_cr: &mut [u8],
    mb_x: u32,
    mb_y: u32,
    luma_stride: usize,
    chroma_stride: usize,
) {
    use crate::prediction::{Intra16x16Mode, Intra4x4Mode, IntraChromaMode};

    let base_x = (mb_x * 16) as usize;
    let base_y = (mb_y * 16) as usize;

    match mb.mb_type {
        MbType::Intra4x4 => {
            // Neighbour sample extraction per 4×4 block.
            let mut top = [None; 16];
            let mut left = [None; 16];
            let mut top_left = [None; 16];
            for b in 0..16usize {
                let bx = (b % 4) * 4;
                let by = (b / 4) * 4;
                for i in 0..4 {
                    // Top sample (above the block).
                    let tx = base_x + bx + i;
                    let ty = base_y + by - 1;
                    top[b * 4 + i] = if ty < base_y || ty >= luma.len() / luma_stride {
                        None
                    } else {
                        luma.get(ty * luma_stride + tx).copied()
                    };
                    // Left sample.
                    let lx = base_x + bx - 1;
                    let ly = base_y + by + i;
                    left[b * 4 + i] = if lx < base_x {
                        None
                    } else {
                        luma.get(ly * luma_stride + lx).copied()
                    };
                }
                // Above-left corner.
                let cx = base_x + bx - 1;
                let cy = base_y + by - 1;
                top_left[b] = if cx < base_x || cy < base_y {
                    None
                } else {
                    luma.get(cy * luma_stride + cx).copied()
                };
            }
            let modes = [Intra4x4Mode::Dc; 16];
            mb.reconstruct_luma_intra_4x4(
                luma, mb_x, mb_y, luma_stride, &modes, &top, &left, &top_left,
            );
        }
        MbType::Intra16x16 {
            pred_mode,
            cbp_chroma: _,
            cbp_luma: _,
        } => {
            let mut top = [None; 16];
            let mut left = [None; 16];
            for i in 0..16 {
                // Top row above the macroblock.
                top[i] = luma
                    .get((base_y as isize - 1).max(0) as usize * luma_stride + base_x + i)
                    .copied();
                // Left column to the left of the macroblock.
                left[i] = if (base_x as isize - 1) >= 0 {
                    luma.get(base_y * luma_stride + (base_x - 1) + i * luma_stride)
                        .copied()
                } else {
                    None
                };
            }
            let tl = if base_x > 0 && base_y > 0 {
                luma.get((base_y - 1) * luma_stride + (base_x - 1)).copied()
            } else {
                None
            };
            mb.reconstruct_luma_intra_16x16(
                luma,
                mb_x,
                mb_y,
                luma_stride,
                Intra16x16Mode::from_u8(pred_mode),
                &top,
                &left,
                tl,
            );
        }
        _ => {
            mb.reconstruct_luma(luma, mb_x, mb_y, luma_stride);
            mb.reconstruct_chroma(chroma_cb, chroma_cr, mb_x, mb_y, chroma_stride);
        }
    }

    // Chroma prediction (DC) for intra macroblocks.
    if matches!(mb.mb_type, MbType::Intra4x4 | MbType::Intra16x16 { .. }) {
        let cbx = (mb_x * 8) as usize;
        let cby = (mb_y * 8) as usize;
        let mut ctop = [None; 8];
        let mut cleft = [None; 8];
        for i in 0..8 {
            ctop[i] = chroma_cb
                .get((cby as isize - 1).max(0) as usize * chroma_stride + cbx + i)
                .copied();
            cleft[i] = if (cbx as isize - 1) >= 0 {
                chroma_cb
                    .get(cby * chroma_stride + (cbx - 1) + i * chroma_stride)
                    .copied()
            } else {
                None
            };
        }
        let ctl = if cbx > 0 && cby > 0 {
            chroma_cb.get((cby - 1) * chroma_stride + (cbx - 1)).copied()
        } else {
            None
        };
        let mut cbp = [0u8; 64];
        crate::prediction::predict_chroma(
            IntraChromaMode::Dc,
            &ctop,
            &cleft,
            ctl,
            &mut cbp,
        );
        // Add chroma residual on top of the chroma prediction.
        for row in 0..8usize {
            for col in 0..8usize {
                let x = cbx + col;
                let y = cby + row;
                let off = y * chroma_stride + x;
                if off < chroma_cb.len() {
                    let res = 0i32; // residual handled by Macroblock when coeffs present
                    let _ = res;
                    chroma_cb[off] = (chroma_cb[off] as i32 + cbp[row * 8 + col] as i32
                        - 128)
                        .clamp(0, 255) as u8;
                    chroma_cr[off] = (chroma_cr[off] as i32 + cbp[row * 8 + col] as i32
                        - 128)
                        .clamp(0, 255) as u8;
                }
            }
        }
    }
}

impl Default for H264Decoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use tpt_kinetix_core::Timestamp;

    use super::*;

    #[test]
    fn empty_packet_returns_none() {
        let mut dec = H264Decoder::new();
        let pkt = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: vec![],
            stream_index: 0,
            is_key_frame: false,
        };
        assert!(matches!(dec.decode(&pkt), Ok(None)));
    }

    #[test]
    fn flush_on_empty_dpb_returns_empty() {
        let mut dec = H264Decoder::new();
        let frames = dec.flush().unwrap();
        assert!(frames.is_empty());
    }
}
