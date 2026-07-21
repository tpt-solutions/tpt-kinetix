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
    macroblock::{Macroblock, MbPos, MbType},
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
            supports_deblocking: true,
            notes: "bitstream + CAVLC scaffold only; reconstruction emits \
                    placeholder pixels (no CABAC/prediction); in-loop deblocking \
                    filter implemented",
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
                    // Look up the active SPS/PPS (use the first available as a fallback).
                    let sps = match self.sps_store.values().next() {
                        Some(s) => s.clone(),
                        None => continue,
                    };
                    let pps = self.pps_store.values().next().cloned();

                    let width = sps.pic_width_pixels();
                    let height = sps.pic_height_pixels();
                    // Reject implausible dimensions from a malformed/adversarial SPS
                    // before allocating frame buffers or macroblock grids sized from
                    // them (an attacker-controlled `pic_width_in_mbs_minus1` can be
                    // close to `u32::MAX`, which would otherwise attempt a
                    // multi-gigabyte allocation). 8192 covers H.264 level 6.2 (the
                    // highest defined level, up to 8192x4320).
                    const MAX_DIMENSION: u32 = 8192;
                    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION
                    {
                        continue;
                    }

                    // Attempt the real CAVLC I-slice decode path first.
                    match self.try_decode_real_slice(nal, &sps, pps.as_ref(), width, height, packet)
                    {
                        Ok(Some(frame)) => {
                            output_frame = Some(frame);
                            continue;
                        }
                        Ok(None) => { /* not an I/CAVLC slice we can fully decode */ }
                        Err(_e) => { /* fall through to scaffold / strict handling */ }
                    }

                    if self.strict {
                        return Err(KinetixError::NotPixelExact(
                            "H.264: slice not decodable by the pixel-exact path yet \
                             (inter/CABAC/unsupported feature); see H264Decoder::capabilities"
                                .to_string(),
                        ));
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

    /// Attempt the real, spec-exact CAVLC I-slice decode path.
    ///
    /// Returns `Ok(Some(frame))` on success, `Ok(None)` if the slice is not a
    /// CAVLC I-slice this path handles yet, or `Err` on a parse failure.
    fn try_decode_real_slice(
        &mut self,
        nal: &crate::nal::NalUnit,
        sps: &SeqParameterSet,
        pps: Option<&PicParameterSet>,
        width: u32,
        height: u32,
        packet: &Packet,
    ) -> Result<Option<VideoFrame>, KinetixError> {
        use crate::slice::{SliceHeader, SliceHeaderContext, SliceType};

        // CABAC is not handled by this path yet.
        if pps.map(|p| p.entropy_coding_mode_flag).unwrap_or(false) {
            return Ok(None);
        }
        // Interlaced not handled.
        if !sps.frame_mbs_only_flag {
            return Ok(None);
        }

        let ctx = SliceHeaderContext {
            log2_max_frame_num_minus4: sps.log2_max_frame_num_minus4,
            pic_order_cnt_type: sps.pic_order_cnt_type,
            log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_pic_order_cnt_lsb_minus4,
            frame_mbs_only_flag: sps.frame_mbs_only_flag,
            bottom_field_pic_order_in_frame_present_flag: pps
                .map(|p| p.bottom_field_pic_order_in_frame_present_flag)
                .unwrap_or(false),
            delta_pic_order_always_zero_flag: false,
            num_ref_idx_l0_default_active_minus1: pps
                .map(|p| p.num_ref_idx_l0_default_active_minus1)
                .unwrap_or(0),
            num_ref_idx_l1_default_active_minus1: pps
                .map(|p| p.num_ref_idx_l1_default_active_minus1)
                .unwrap_or(0),
            weighted_pred_flag: pps.map(|p| p.weighted_pred_flag).unwrap_or(false),
            weighted_bipred_idc: pps.map(|p| p.weighted_bipred_idc).unwrap_or(0),
            entropy_coding_mode_flag: false,
            deblocking_filter_control_present_flag: pps
                .map(|p| p.deblocking_filter_control_present_flag)
                .unwrap_or(false),
            redundant_pic_cnt_present_flag: pps
                .map(|p| p.redundant_pic_cnt_present_flag)
                .unwrap_or(false),
            num_slice_groups_minus1: pps.map(|p| p.num_slice_groups_minus1).unwrap_or(0),
            chroma_array_type: if sps.separate_colour_plane_flag {
                0
            } else {
                sps.chroma_format_idc
            },
        };

        let header = match SliceHeader::parse_with_context(&nal.rbsp, nal.nal_unit_type, &ctx) {
            Ok(h) => h,
            Err(_) => return Ok(None),
        };

        // Only fully-intra slices are handled by this path.
        if !matches!(header.slice_type, SliceType::I | SliceType::Si) {
            return Ok(None);
        }
        // Only single-slice pictures starting at MB 0.
        if header.first_mb_in_slice != 0 {
            return Ok(None);
        }

        let pic_init_qp = 26 + pps.map(|p| p.pic_init_qp_minus26).unwrap_or(0);
        let slice_qp = pic_init_qp + header.slice_qp_delta;
        let chroma_qp_index_offset = pps.map(|p| p.chroma_qp_index_offset).unwrap_or(0);

        let mb_cols = width.div_ceil(16);
        let mb_rows = height.div_ceil(16);

        let mut reader = crate::bitreader::BitReader::new(&nal.rbsp);
        reader.seek_to_bit(header.data_bit_offset);

        let parsed = match crate::slice_data::parse_i_slice(
            &mut reader,
            mb_cols,
            mb_rows,
            slice_qp,
            chroma_qp_index_offset,
        ) {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };

        let recon = crate::reconstruct::reconstruct_intra_frame(
            &parsed.macroblocks,
            mb_cols,
            mb_rows,
            width,
            height,
            chroma_qp_index_offset,
        );

        // Assemble the planar YUV420p frame.
        let mut data = recon.luma;
        data.extend(recon.chroma_cb);
        data.extend(recon.chroma_cr);

        self.frame_count += 1;
        Ok(Some(VideoFrame {
            pts: packet.pts,
            dts: packet.dts,
            data,
            width,
            height,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: matches!(nal.nal_unit_type, NalUnitType::IdrSlice),
        }))
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
            let mut planes = FramePlanes {
                luma: &mut luma,
                chroma_cb: &mut chroma_cb,
                chroma_cr: &mut chroma_cr,
                luma_stride,
                chroma_stride,
            };
            for (row_idx, row_mbs) in mb_rows_data.iter().enumerate() {
                for (col_idx, mb) in row_mbs.iter().enumerate() {
                    reconstruct_mb(mb, &mut planes, col_idx as u32, row_idx as u32);
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

        // In-loop deblocking pass (spec §8.7). We derive a per-MB
        // [`DeblockMbInfo`] from the reconstructed macroblock grid and filter
        // every macroblock edge against its left/top neighbour. With the current
        // skip-only reconstruction the planes are flat, so this is a no-op here,
        // but the code path is real and unit-tested in `deblock.rs`; it becomes
        // active once CABAC/CAVLC reconstruction emits non-flat blocks.
        let deblock_params = crate::deblock::DeblockParams::default();
        let mb_info: Vec<Vec<crate::deblock::DeblockMbInfo>> = mb_rows_data
            .iter()
            .map(|row| {
                row.iter()
                    .map(|mb| crate::deblock::DeblockMbInfo::new(mb.mb_type, !mb.skip, mb.qp))
                    .collect()
            })
            .collect();
        for (row_idx, row_info) in mb_info.iter().enumerate() {
            for (col_idx, cur) in row_info.iter().enumerate() {
                let left = if col_idx > 0 {
                    Some(&row_info[col_idx - 1])
                } else {
                    None
                };
                let top = if row_idx > 0 {
                    Some(&mb_info[row_idx - 1][col_idx])
                } else {
                    None
                };
                crate::deblock::deblock_luma_mb(
                    &mut luma,
                    luma_stride,
                    col_idx,
                    row_idx,
                    cur,
                    left,
                    top,
                    deblock_params,
                );
                crate::deblock::deblock_chroma_mb(
                    &mut chroma_cb,
                    &mut chroma_cr,
                    chroma_stride,
                    col_idx,
                    row_idx,
                    cur,
                    left,
                    top,
                    deblock_params,
                );
            }
        }

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

/// The frame's luma/chroma output planes, borrowed together for the duration
/// of macroblock reconstruction.
struct FramePlanes<'a> {
    luma: &'a mut [u8],
    chroma_cb: &'a mut [u8],
    chroma_cr: &'a mut [u8],
    luma_stride: usize,
    chroma_stride: usize,
}

/// Reconstruct a single macroblock into the luma/chroma planes, applying the
/// correct prediction path for its type.
///
/// Neighbour samples (top row, left column, and the above-left corner) are read
/// back from `planes`, which must already hold the reconstructed output of the
/// macroblocks above and to the left.
fn reconstruct_mb(mb: &Macroblock, planes: &mut FramePlanes<'_>, mb_x: u32, mb_y: u32) {
    use crate::prediction::{Intra16x16Mode, IntraChromaMode};

    let luma: &mut [u8] = &mut *planes.luma;
    let chroma_cb: &mut [u8] = &mut *planes.chroma_cb;
    let chroma_cr: &mut [u8] = &mut *planes.chroma_cr;
    let luma_stride = planes.luma_stride;
    let chroma_stride = planes.chroma_stride;

    let base_x = (mb_x * 16) as usize;
    let base_y = (mb_y * 16) as usize;

    match mb.mb_type {
        MbType::Intra4x4 => {
            // Neighbour sample extraction per 4×4 block (64 neighbour slots:
            // 4 top/left/corner samples per block × 16 blocks).
            let mut top = [None; 64];
            let mut left = [None; 64];
            let mut top_left = [None; 64];
            let height = luma.len() / luma_stride.max(1);
            for b in 0..16usize {
                let bx = (b % 4) * 4;
                let by = (b / 4) * 4;
                for i in 0..4 {
                    // Top sample (directly above the block).
                    let tx = base_x + bx + i;
                    let ty = base_y as isize + by as isize - 1;
                    top[b * 4 + i] = if ty >= 0 && (ty as usize) < height {
                        luma.get(ty as usize * luma_stride + tx).copied()
                    } else {
                        None
                    };
                    // Left sample (directly left of the block).
                    let lx = base_x as isize + bx as isize - 1;
                    let ly = base_y + by + i;
                    left[b * 4 + i] = if lx >= 0 {
                        luma.get(ly * luma_stride + lx as usize).copied()
                    } else {
                        None
                    };
                }
                // Above-left corner sample.
                let cx = base_x as isize + bx as isize - 1;
                let cy = base_y as isize + by as isize - 1;
                top_left[b] = if cx >= 0 && cy >= 0 {
                    luma
                        .get(cy as usize * luma_stride + cx as usize)
                        .copied()
                } else {
                    None
                };
            }
            let pos = MbPos {
                mb_x,
                mb_y,
                stride: luma_stride,
            };
            mb.reconstruct_luma_intra_4x4(luma, pos, &mb.pred_modes_4x4, &top, &left, &top_left);
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
            let pos = MbPos {
                mb_x,
                mb_y,
                stride: luma_stride,
            };
            mb.reconstruct_luma_intra_16x16(
                luma,
                pos,
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
            chroma_cb
                .get((cby - 1) * chroma_stride + (cbx - 1))
                .copied()
        } else {
            None
        };
        let mut cbp = [0u8; 64];
        crate::prediction::predict_chroma(IntraChromaMode::Dc, &ctop, &cleft, ctl, &mut cbp);
        // The chroma prediction (`cbp`, already in 8-bit sample range) is written
        // into the plane, and the per-block residual (from the CAVLC/CABAC
        // decoded coefficients) is added on top when present.
        for row in 0..8usize {
            for col in 0..8usize {
                let x = cbx + col;
                let y = cby + row;
                let off = y * chroma_stride + x;
                if off < chroma_cb.len() {
                    let cb_res = mb.chroma_cb_coeffs[(row >> 2) * 2 + (col >> 2)];
                    let cr_res = mb.chroma_cr_coeffs[(row >> 2) * 2 + (col >> 2)];
                    let cb_idct = crate::macroblock::iquant_idct_4x4_public(&cb_res, mb.qp);
                    let cr_idct = crate::macroblock::iquant_idct_4x4_public(&cr_res, mb.qp);
                    let br = (row % 4) * 4 + (col % 4);
                    chroma_cb[off] = (cbp[row * 8 + col] as i32 + cb_idct[br]).clamp(0, 255) as u8;
                    chroma_cr[off] = (cbp[row * 8 + col] as i32 + cr_idct[br]).clamp(0, 255) as u8;
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

    #[test]
    fn fuzz_regression_oom_fd9f0adb2952389dda8d5ad0feab8c75168ee1b0() {
        // An SPS claiming an implausible resolution used to make `decode_slice`
        // build a macroblock grid and frame buffers sized from the raw
        // (attacker-controlled) width/height, exhausting memory instead of
        // being rejected.
        let data = vec![
            33, 31, 0, 0, 1, 255, 243, 0, 0, 1, 39, 255, 0, 1, 105, 164, 0, 0, 0, 105, 105, 105,
            3, 3, 3, 255, 255, 255, 255, 255, 255, 255, 15, 0, 0, 1, 33, 5, 4, 1, 33, 5, 4, 217,
        ];
        let mut dec = H264Decoder::new();
        let pkt = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data,
            stream_index: 0,
            is_key_frame: false,
        };
        // Must return promptly without attempting a huge allocation.
        let _ = dec.decode(&pkt);
    }
}
