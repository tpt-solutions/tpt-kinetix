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
    reconstruct::ReconstructedFrame,
    ref_pic::{Dpb, DpbEntry, PocState},
    slice_data::ParsedSlice,
    sps::SeqParameterSet,
    trace::{DecodeTracer, NoopTracer},
};

/// Stateful H.264 / AVC decoder.
///
/// Feed compressed [`Packet`]s via [`H264Decoder::decode`] and receive decoded [`VideoFrame`]s.
pub struct H264Decoder {
    sps_store: HashMap<u32, SeqParameterSet>,
    pps_store: HashMap<u32, PicParameterSet>,
    /// Decoded Picture Buffer — stores reference frames for inter prediction.
    dpb: Dpb,
    /// POC derivation state carried between pictures (§8.2.1).
    poc_state: PocState,
    frame_count: u64,
    /// Pending complementary field of an interlaced (PAFF / MBAFF) frame that
    /// has not yet been paired with its other field for output interleaving.
    /// A field picture decodes to a half-height buffer; the full frame is only
    /// emitted once both its top and bottom fields have been reconstructed.
    field_accum: Option<FieldAccum>,
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

/// Accumulated fields of one interlaced frame awaiting output interleaving.
///
/// In PAFF / MBAFF the two fields of a coded frame are decoded as separate
/// access units. `pending` holds the first field to arrive; `paired` becomes
/// `Some` once both the top and bottom field have been reconstructed, at which
/// point they are interleaved into the full output frame.
#[derive(Debug, Default)]
struct FieldAccum {
    /// Pairing key — the field's `frame_num` (PAFF) or frame `PicOrderCnt`
    /// rounded to the frame (MBAFF). Two fields sharing a key belong to the
    /// same display frame.
    key: u32,
    /// The reconstructed top field (half-height YUV420p plane set).
    top: Option<VideoFrame>,
    /// The reconstructed bottom field (half-height YUV420p plane set).
    bottom: Option<VideoFrame>,
    /// Whether the most recently stored field was the bottom field (used to
    /// detect a key change when the second field of a new frame appears before
    /// the first was paired).
    #[allow(dead_code)]
    last_bottom: bool,
}

/// Result of attempting to decode an interlaced (PAFF / MBAFF) slice.
enum InterlacedOutcome {
    /// A full interlaced frame is ready (both of its fields were reconstructed
    /// and interleaved into a progressive frame buffer).
    Frame(VideoFrame),
    /// A field was reconstructed and buffered; the paired field has not arrived
    /// yet, so no frame is emitted. The slice is fully handled (do not fall
    /// through to the scaffold / strict path).
    Handled,
    /// This interlaced flavour is not yet supported; behave like any other
    /// unsupported slice (the caller falls through to the strict / scaffold
    /// path).
    Fallback,
}

impl H264Decoder {
    pub fn new() -> Self {
        Self {
            sps_store: HashMap::new(),
            pps_store: HashMap::new(),
            dpb: Dpb::new(),
            poc_state: PocState::default(),
            frame_count: 0,
            parallel: true,
            strict: false,
            field_accum: None,
        }
    }

    /// Reports what this decoder can and cannot do.
    ///
    /// The H.264 decoder's **CAVLC I/P/B and CABAC I paths are bit-exact** (max
    /// abs diff == 0) against `ffmpeg` for 4:2:0, progressive, 16-px-aligned
    /// pictures without the 8×8 transform, validated continuously by
    /// `tests/conformance_matrix.rs` and the per-feature `*_conformance.rs`
    /// suites. These are guarded by `Expect::BitExact` assertions, so any
    /// regression fails CI.
    ///
    /// The **CABAC P/B paths are implemented but NOT yet bit-exact** (a Phase D.4
    /// regression): they decode without error but differ from `ffmpeg` by up to
    /// ~130 luma levels over roughly a quarter of the samples for P, and almost
    /// the entire frame for B. This is tracked and measured by the conformance
    /// harness (`Expect::NotConformant`) rather than hidden.
    ///
    /// The remaining hard gaps (which the decoder honestly rejects in strict
    /// mode) are the 8×8 transform / High profile (`transform_8x8_mode_flag`),
    /// interlaced coding (PAFF/MBAFF), and non-16-aligned picture dimensions.
    ///
    /// For any stream it cannot decode pixel-exactly, [`H264Decoder::with_strict`]
    /// makes [`H264Decoder::decode`] return [`tpt_kinetix_core::error::KinetixError::NotPixelExact`]
    /// instead of emitting approximate frames. Callers should check
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
    /// assert!(caps.supports_inter_prediction);
    /// ```
    pub fn capabilities(&self) -> DecoderCapabilities {
        DecoderCapabilities {
            codec: "H.264",
            pixel_exact: false,
            supports_cabac: true,
            supports_cavlc: true,
            supports_intra_prediction: true,
            supports_inter_prediction: true,
            supports_deblocking: true,
            notes: "CAVLC I/P/B and CABAC I slices (4:2:0, progressive, \
                    16-px-aligned, no 8x8 transform) are bit-exact vs ffmpeg; \
                    CABAC P/B slices decode but are NOT yet bit-exact (Phase D.4 \
                    regression); the 8x8 transform (High profile), interlaced \
                    (PAFF/MBAFF), and non-16-aligned dimensions are not yet \
                    pixel-exact (Phases F/G)",
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

    /// Read-only view of the Decoded Picture Buffer (§8.2.5).
    ///
    /// Holds the pictures the decoded reference picture marking process left
    /// marked "used for short-term reference" or "used for long-term
    /// reference" after the most recently decoded picture — i.e. exactly the
    /// pictures a following P/B slice may build its reference lists from
    /// (§8.2.4). Non-reference pictures (`nal_ref_idc == 0`) never enter it.
    ///
    /// Exposed for inspection and for tests that need to observe what the
    /// slice header's `dec_ref_pic_marking` (sliding-window or MMCO) actually
    /// did, rather than inferring it from decoded pixels.
    pub fn dpb(&self) -> &Dpb {
        &self.dpb
    }

    /// Decode a compressed bitstream [`Packet`] into a [`VideoFrame`].
    ///
    /// Returns `Ok(None)` when the decoder needs more data before a frame can
    /// be emitted. Returns `Ok(Some(frame))` when a frame is ready.
    ///
    /// NAL units are extracted from Annex B byte-stream format.
    /// Slice-level parallelism is applied via `rayon` at the macroblock-row boundary.
    pub fn decode(&mut self, packet: &Packet) -> Result<Option<VideoFrame>, KinetixError> {
        self.decode_impl(packet, &mut NoopTracer)
    }

    /// Like [`H264Decoder::decode`], but drives `tracer`'s hooks
    /// (see [`crate::trace::DecodeTracer`]) with per-macroblock intermediate
    /// values as the CAVLC I-slice path parses and reconstructs the frame.
    /// Only exercises the tracer for slices the real CAVLC path handles; the
    /// scaffold/placeholder fallback path never calls the tracer.
    pub fn decode_with_tracer<T: DecodeTracer>(
        &mut self,
        packet: &Packet,
        tracer: &mut T,
    ) -> Result<Option<VideoFrame>, KinetixError> {
        self.decode_impl(packet, tracer)
    }

    fn decode_impl<T: DecodeTracer>(
        &mut self,
        packet: &Packet,
        tracer: &mut T,
    ) -> Result<Option<VideoFrame>, KinetixError> {
        let nal_units = parse_nal_units_from_annexb(&packet.data);
        if nal_units.is_empty() {
            return Ok(None);
        }

        let mut output_frame: Option<VideoFrame> = None;

        for nal in &nal_units {
            match nal.nal_unit_type {
                NalUnitType::Sps => {
                    if let Ok(sps) = SeqParameterSet::parse(&nal.rbsp) {
                        eprintln!("SPS_NAL rbsp_len={} profile_idc={}", nal.rbsp.len(), sps.profile_idc);
                        self.sps_store.insert(sps.seq_parameter_set_id, sps);
                    } else {
                        eprintln!("SPS_NAL rbsp_len={} PARSE_FAIL", nal.rbsp.len());
                    }
                }
                NalUnitType::Pps => {
                    eprintln!("PPS_NAL rbsp_len={}", nal.rbsp.len());
                    // Probe the PPS first to discover its `seq_parameter_set_id`
                    // so the SPS scaling lists can be merged into the PPS set
                    // (§8.5.9: an absent PPS list falls back to the corresponding
                    // SPS list). Then parse again with the SPS scaling context.
                    match PicParameterSet::parse(&nal.rbsp, None) {
                        Ok(pps_probe) => {
                            let sps_scaling = self
                                .sps_store
                                .get(&pps_probe.seq_parameter_set_id)
                                .map(|s| &s.scaling);
                            match PicParameterSet::parse(&nal.rbsp, sps_scaling) {
                                Ok(pps) => {
                                    eprintln!(
                                        "PPS_OK t8={} pps_id={}",
                                        pps.transform_8x8_mode_flag, pps.pic_parameter_set_id
                                    );
                                    self.pps_store.insert(pps.pic_parameter_set_id, pps);
                                }
                                Err(e) => eprintln!("PPS_PARSE_ERR(2): {e:?}"),
                            }
                        }
                        Err(e) => eprintln!("PPS_PARSE_ERR(1): {e:?}"),
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
                    // Also cap total macroblock count so O(MB_count) paths (e.g.
                    // emit_skip_frame) can't be abused by a malformed SPS that
                    // encodes near-8192×8192 dimensions (≈262 k MBs → timeout).
                    const MAX_MB_COUNT: u32 = 36_864; // ≈3072×3072 (generous for 4K)
                    let mb_cols = width.div_ceil(16);
                    let mb_rows = height.div_ceil(16);
                    if mb_cols * mb_rows > MAX_MB_COUNT {
                        continue;
                    }

                    // Interlaced (PAFF / MBAFF) streams are decoded by a dedicated
                    // path. `Frame` means a full interlaced frame is ready (both
                    // its fields were reconstructed); `Handled` means a field was
                    // reconstructed and buffered but its pair has not arrived yet
                    // (no frame emitted — the slice is fully handled, so the
                    // caller must not fall through to the scaffold / strict
                    // path); `Fallback` means this interlaced flavour is not yet
                    // supported and the caller should treat it like any other
                    // unsupported slice.
                    if !sps.frame_mbs_only_flag {
                        match self.decode_interlaced(
                            nal, &sps, pps.as_ref(), width, height, packet, tracer,
                        ) {
                            Ok(InterlacedOutcome::Frame(frame)) => {
                                output_frame = Some(frame);
                                continue;
                            }
                            Ok(InterlacedOutcome::Handled) => {
                                continue;
                            }
                            Ok(InterlacedOutcome::Fallback) => {}
                            Err(_) => {}
                        }
                    }

                    // Attempt the real CAVLC I-slice decode path first.
                    match self.try_decode_real_slice(
                        nal,
                        &sps,
                        pps.as_ref(),
                        width,
                        height,
                        packet,
                        tracer,
                    ) {
                        Ok(Some(frame)) => {
                            output_frame = Some(frame);
                            continue;
                        }
                        Ok(None) => {
                            eprintln!("DBG try_decode_real_slice returned Ok(None)");
                        }
                        Err(_e) => {
                            eprintln!("DBG try_decode_real_slice ERR: {_e:?}");
                        }
                    }

                    if self.strict {
                        return Err(KinetixError::NotPixelExact(
                            "H.264: slice not decodable by the pixel-exact path yet \
                             (inter/CABAC/unsupported feature); see H264Decoder::capabilities"
                                .to_string(),
                        ));
                    }

                    let frame = self.decode_slice(nal, width, height, packet)?;
                    output_frame = Some(frame);
                }
                _ => {}
            }
        }

        Ok(output_frame)
    }

    /// Flush any buffered frames from the decoded picture buffer.
    pub fn flush(&mut self) -> Result<Vec<VideoFrame>, KinetixError> {
        let frames = self.dpb.take_frames();
        Ok(frames)
    }

    /// Attempt the real, spec-exact CAVLC I-slice decode path.
    ///
    /// Returns `Ok(Some(frame))` on success, `Ok(None)` if the slice is not a
    /// CAVLC I-slice this path handles yet, or `Err` on a parse failure.
    #[allow(clippy::too_many_arguments)]
    fn try_decode_real_slice<T: DecodeTracer>(
        &mut self,
        nal: &crate::nal::NalUnit,
        sps: &SeqParameterSet,
        pps: Option<&PicParameterSet>,
        width: u32,
        height: u32,
        packet: &Packet,
        tracer: &mut T,
    ) -> Result<Option<VideoFrame>, KinetixError> {
        use crate::slice::{SliceHeader, SliceHeaderContext, SliceType};

        let entropy_coding_mode_flag = pps.map(|p| p.entropy_coding_mode_flag).unwrap_or(false);
        // CABAC 8x8 transform (High profile) is supported for intra
        // macroblocks (Phase F.4); inter (P_8x8/B_Direct) 8x8 transform is
        // not yet implemented for either entropy mode, matching the CAVLC
        // path's existing scope, so no gate is needed here for that case.
        let transform_8x8_mode_flag = pps.map(|p| p.transform_8x8_mode_flag).unwrap_or(false);
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
            entropy_coding_mode_flag,
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

        let header = match SliceHeader::parse_with_context(&nal.rbsp, nal.nal_unit_type, nal.nal_ref_idc, &ctx) {
            Ok(h) => h,
            Err(e) => {
                let _ = e;
                return Ok(None);
            }
        };

        // Active scaling lists: the PPS override (if any) merged over the SPS set
        // (§8.5.9). When no PPS scaling is present `pps.scaling` already equals the
        // SPS set, so this is always the correct merged set.
        let scaling = pps.map(|p| &p.scaling).unwrap_or(&sps.scaling);

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

        let parsed = if entropy_coding_mode_flag {
            // §7.3.4: consume cabac_alignment_one_bit padding, then hand the
            // byte-aligned remainder to the CABAC arithmetic engine.
            reader.byte_align();
            match crate::slice_data::parse_i_slice_cabac(
                reader.remaining_bytes(),
                mb_cols,
                mb_rows,
                slice_qp,
                sps.mb_adaptive_frame_field_flag,
                header.field_pic_flag,
                transform_8x8_mode_flag,
                tracer,
            ) {
                Ok(p) => p,
                Err(e) => {
                    let _ = e;
                    return Ok(None);
                }
            }
        } else {
            match crate::slice_data::parse_i_slice(
                &mut reader,
                mb_cols,
                mb_rows,
                slice_qp,
                chroma_qp_index_offset,
                pps.map(|p| p.transform_8x8_mode_flag).unwrap_or(false),
                sps.mb_adaptive_frame_field_flag,
                header.field_pic_flag,
                tracer,
            ) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("DBG parse_i_slice ERR: {e:?}");
                    return Ok(None);
                }
            }
        };

        let mut recon = crate::reconstruct::reconstruct_intra_frame(
            &parsed.macroblocks,
            mb_cols,
            mb_rows,
            width,
            height,
            chroma_qp_index_offset,
            scaling,
            &crate::reconstruct::WeightedPred::Default,
            tracer,
        );

        // Apply the in-loop deblocking filter (spec §8.7).
        let deblock_params = crate::deblock::DeblockParams {
            disable_idc: header.disable_deblocking_filter_idc as u8,
            alpha_offset_div2: header.slice_alpha_c0_offset_div2,
            beta_offset_div2: header.slice_beta_offset_div2,
            chroma_qp_index_offset,
        };
        let mb_info: Vec<Vec<crate::deblock::DeblockMbInfo>> = parsed
            .macroblocks
            .chunks(mb_cols as usize)
            .enumerate()
            .map(|(row_idx, row)| {
                row.iter()
                    .enumerate()
                    .map(|(col_idx, mb)| {
                        let idx = row_idx * mb_cols as usize + col_idx;
                        let nz = parsed.nz[idx].luma;
                        let cells = parsed
                            .mv_store
                            .cells_of(idx)
                            .unwrap_or([crate::mv::MvCell::INTRA; 16]);
                        crate::deblock::DeblockMbInfo::new(mb.mb_type, nz, cells, mb.qp)
                    })
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
                    &mut recon.luma,
                    recon.luma_stride,
                    col_idx,
                    row_idx,
                    cur,
                    left,
                    top,
                    deblock_params,
                );
                crate::deblock::deblock_chroma_mb(
                    &mut recon.chroma_cb,
                    &mut recon.chroma_cr,
                    recon.chroma_stride,
                    col_idx,
                    row_idx,
                    cur,
                    left,
                    top,
                    deblock_params,
                );
            }
        }

        // Assemble the planar YUV420p frame.
        let mut data = recon.luma;
        data.extend(recon.chroma_cb);
        data.extend(recon.chroma_cr);

        self.frame_count += 1;
        let frame = VideoFrame {
            pts: packet.pts,
            dts: packet.dts,
            data,
            width,
            height,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: matches!(nal.nal_unit_type, NalUnitType::IdrSlice),
        };

        // Reference-picture management: derive POC (§8.2.1), advance the POC
        // state, and store reference pictures in the DPB (§8.2.5) so later
        // P/B slices can build reference lists (§8.2.4).
        self.store_reference_picture(nal, sps, &header, &frame);

        Ok(Some(frame))
    }

    /// Run the decoded reference picture marking process (§8.2.5) for a
    /// just-decoded picture and store it in the DPB.
    ///
    /// Non-reference pictures (`nal_ref_idc == 0`) are not stored at all. For
    /// reference pictures the slice header's `dec_ref_pic_marking` (§7.3.3.3)
    /// selects sliding-window or adaptive (MMCO) marking; a header that omitted
    /// the syntax falls back to sliding-window marking.
    fn store_reference_picture(
        &mut self,
        nal: &crate::nal::NalUnit,
        sps: &SeqParameterSet,
        header: &crate::slice::SliceHeader,
        frame: &VideoFrame,
    ) {
        use crate::slice::DecRefPicMarking;

        if nal.nal_ref_idc == 0 {
            return;
        }
        let is_idr = matches!(nal.nal_unit_type, NalUnitType::IdrSlice);
        let Ok(poc) = crate::ref_pic::derive_pic_order_cnt(
            sps,
            is_idr,
            true,
            header.frame_num,
            header.pic_order_cnt_lsb,
            header.field_pic_flag,
            header.bottom_field_flag,
            header.delta_pic_order_cnt_bottom,
            &mut self.poc_state,
        ) else {
            return;
        };

        let marking = header
            .dec_ref_pic_marking
            .clone()
            .unwrap_or(DecRefPicMarking::SlidingWindow);
        let entry = DpbEntry {
            frame: frame.clone(),
            frame_num: header.frame_num,
            field_pic_flag: header.field_pic_flag,
            bottom_field_flag: header.bottom_field_flag,
            pic_order_cnt: poc,
            is_short_term: true,
            is_long_term: false,
            long_term_pic_num: -1,
            mv_grid: None,
        };
        let ctx = crate::ref_pic::PicNumContext::new(
            sps,
            header.frame_num,
            header.field_pic_flag,
            header.bottom_field_flag,
        );
        match self
            .dpb
            .mark_decoded_picture(entry, &marking, ctx, sps.num_ref_frames)
        {
            Ok(outcome) => {
                if outcome.mmco5 {
                    // §8.2.1.1/§7.4.3: MMCO 5 rebases POC and makes the
                    // current picture's frame_num 0 for everything that
                    // follows.
                    self.poc_state.reset_after_mmco5();
                }
            }
            Err(_e) => {
                // `mark_decoded_picture` already emptied the DPB, so the next
                // inter slice falls back rather than predicting from a
                // wrongly-marked reference.
                let _ = _e;
            }
        }
    }

    /// Fallback slice decode: attempt the real CAVLC I-slice path first,
    /// then I_PCM, otherwise emit an all-skip scaffold frame.
    fn decode_slice(
        &mut self,
        nal: &crate::nal::NalUnit,
        width: u32,
        height: u32,
        packet: &Packet,
    ) -> Result<VideoFrame, KinetixError> {
        let mb_cols = width.div_ceil(16);
        let mb_rows = height.div_ceil(16);

        let sps = match self.sps_store.values().next() {
            Some(s) => s.clone(),
            None => return self.emit_skip_frame(nal.nal_unit_type, width, height, packet),
        };
        let pps = self.pps_store.values().next().cloned();
        // Active scaling lists: PPS override merged over SPS set (§8.5.9).
        let scaling = pps.as_ref().map(|p| &p.scaling).unwrap_or(&sps.scaling);

        let ctx = crate::slice::SliceHeaderContext {
            log2_max_frame_num_minus4: sps.log2_max_frame_num_minus4,
            pic_order_cnt_type: sps.pic_order_cnt_type,
            log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_pic_order_cnt_lsb_minus4,
            frame_mbs_only_flag: sps.frame_mbs_only_flag,
            bottom_field_pic_order_in_frame_present_flag: pps
                .as_ref()
                .map(|p| p.bottom_field_pic_order_in_frame_present_flag)
                .unwrap_or(false),
            delta_pic_order_always_zero_flag: false,
            num_ref_idx_l0_default_active_minus1: pps
                .as_ref()
                .map(|p| p.num_ref_idx_l0_default_active_minus1)
                .unwrap_or(0),
            num_ref_idx_l1_default_active_minus1: pps
                .as_ref()
                .map(|p| p.num_ref_idx_l1_default_active_minus1)
                .unwrap_or(0),
            weighted_pred_flag: pps.as_ref().map(|p| p.weighted_pred_flag).unwrap_or(false),
            weighted_bipred_idc: pps.as_ref().map(|p| p.weighted_bipred_idc).unwrap_or(0),
            entropy_coding_mode_flag: pps
                .as_ref()
                .map(|p| p.entropy_coding_mode_flag)
                .unwrap_or(false),
            deblocking_filter_control_present_flag: pps
                .as_ref()
                .map(|p| p.deblocking_filter_control_present_flag)
                .unwrap_or(false),
            redundant_pic_cnt_present_flag: pps
                .as_ref()
                .map(|p| p.redundant_pic_cnt_present_flag)
                .unwrap_or(false),
            num_slice_groups_minus1: pps.as_ref().map(|p| p.num_slice_groups_minus1).unwrap_or(0),
            chroma_array_type: if sps.separate_colour_plane_flag {
                0
            } else {
                sps.chroma_format_idc
            },
        };

        let header =
            match crate::slice::SliceHeader::parse_with_context(&nal.rbsp, nal.nal_unit_type, nal.nal_ref_idc, &ctx)
            {
                Ok(h) => h,
                Err(_) => return self.emit_skip_frame(nal.nal_unit_type, width, height, packet),
            };

        let is_i_slice = header.slice_type == crate::slice::SliceType::I
            || header.slice_type == crate::slice::SliceType::Si;

        // Attempt the real CAVLC I-slice decode path.
        if is_i_slice && header.first_mb_in_slice == 0 {
            let slice_qp = 26
                + pps.as_ref().map(|p| p.pic_init_qp_minus26).unwrap_or(0)
                + header.slice_qp_delta;
            let chroma_qp_index_offset =
                pps.as_ref().map(|p| p.chroma_qp_index_offset).unwrap_or(0);

            let mut reader = crate::bitreader::BitReader::new(&nal.rbsp);
            reader.seek_to_bit(header.data_bit_offset);
            match crate::slice_data::parse_i_slice(
                &mut reader,
                mb_cols,
                mb_rows,
                slice_qp,
                chroma_qp_index_offset,
                pps.as_ref().map(|p| p.transform_8x8_mode_flag).unwrap_or(false),
                sps.mb_adaptive_frame_field_flag,
                header.field_pic_flag,
                &mut crate::trace::NoopTracer,
            ) {
                Ok(parsed) => {
                    let mut recon = crate::reconstruct::reconstruct_intra_frame(
                        &parsed.macroblocks,
                        mb_cols,
                        mb_rows,
                        width,
                        height,
                        chroma_qp_index_offset,
                        scaling,
                        &crate::reconstruct::WeightedPred::Default,
                        &mut crate::trace::NoopTracer,
                    );

                    let deblock_params = crate::deblock::DeblockParams {
                        disable_idc: header.disable_deblocking_filter_idc as u8,
                        alpha_offset_div2: header.slice_alpha_c0_offset_div2,
                        beta_offset_div2: header.slice_beta_offset_div2,
                        chroma_qp_index_offset,
                    };
                    let mb_info: Vec<Vec<crate::deblock::DeblockMbInfo>> = parsed
                        .macroblocks
                        .chunks(mb_cols as usize)
                        .enumerate()
                        .map(|(row_idx, row)| {
                            row.iter()
                                .enumerate()
                                .map(|(col_idx, mb)| {
                                    let idx = row_idx * mb_cols as usize + col_idx;
                                    let nz = parsed.nz[idx].luma;
                                    let cells = parsed
                                        .mv_store
                                        .cells_of(idx)
                                        .unwrap_or([crate::mv::MvCell::INTRA; 16]);
                                    crate::deblock::DeblockMbInfo::new(
                                        mb.mb_type, nz, cells, mb.qp,
                                    )
                                })
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
                                &mut recon.luma,
                                recon.luma_stride,
                                col_idx,
                                row_idx,
                                cur,
                                left,
                                top,
                                deblock_params,
                            );
                            crate::deblock::deblock_chroma_mb(
                                &mut recon.chroma_cb,
                                &mut recon.chroma_cr,
                                recon.chroma_stride,
                                col_idx,
                                row_idx,
                                cur,
                                left,
                                top,
                                deblock_params,
                            );
                        }
                    }

                    let mut data = recon.luma;
                    data.extend(recon.chroma_cb);
                    data.extend(recon.chroma_cr);

                    self.frame_count += 1;
                    return Ok(VideoFrame {
                        pts: packet.pts,
                        dts: packet.dts,
                        data,
                        width,
                        height,
                        pixel_format: PixelFormat::Yuv420p,
                        is_key_frame: matches!(nal.nal_unit_type, NalUnitType::IdrSlice),
                    });
                }
                Err(_) => {
                    // Fall through to I_PCM or skip.
                }
            }
        }

        // Real CAVLC P-slice decode: motion-compensate against the DPB's
        // RefPicList0 (§8.4/§8.5). Falls through to the skip scaffold when the
        // slice references no available picture.
        let is_p_slice = header.slice_type == crate::slice::SliceType::P;
        if is_p_slice && header.first_mb_in_slice == 0 {
            let slice_qp = 26
                + pps.as_ref().map(|p| p.pic_init_qp_minus26).unwrap_or(0)
                + header.slice_qp_delta;
            let chroma_qp_index_offset =
                pps.as_ref().map(|p| p.chroma_qp_index_offset).unwrap_or(0);
            // §7.4.3: the slice header's effective value (which already folds
            // in any `num_ref_idx_active_override_flag`) governs both the
            // ref_idx binarisation and the RefPicList0 length, not the raw PPS
            // default.
            let num_ref_idx_l0_active = header.num_ref_idx_l0_active_minus1 + 1;
            let pic_num_ctx = crate::ref_pic::PicNumContext::new(
                &sps,
                header.frame_num,
                header.field_pic_flag,
                header.bottom_field_flag,
            );

            if let Some(ref_list) = crate::ref_pic::build_ref_list_l0(
                &self.dpb,
                num_ref_idx_l0_active as usize,
                pic_num_ctx,
                &header.ref_pic_list_modification_l0,
            ) {
                let ref_frames: Vec<tpt_kinetix_core::frame::VideoFrame> =
                    ref_list.iter().map(|e| e.frame.clone()).collect();
                let mut reader = crate::bitreader::BitReader::new(&nal.rbsp);
                reader.seek_to_bit(header.data_bit_offset);
                let entropy_coding_mode_flag =
                    pps.as_ref().map(|p| p.entropy_coding_mode_flag).unwrap_or(false);
                    let p_result = if entropy_coding_mode_flag {
                        reader.byte_align();
                        let cabac_data = reader.remaining_bytes();
                        let _preview: Vec<String> = cabac_data.iter().take(16).map(|b| format!("{b:02X}")).collect();
                        crate::slice_data::parse_p_slice_cabac(
                        cabac_data,
                        mb_cols,
                        mb_rows,
                        slice_qp,
                        header.cabac_init_idc as usize,
                        num_ref_idx_l0_active,
                        chroma_qp_index_offset,
                        pps.as_ref().map(|p| p.transform_8x8_mode_flag).unwrap_or(false),
                        &mut crate::trace::NoopTracer,
                    )
                } else {
                    crate::slice_data::parse_p_slice(
                        &mut reader,
                        mb_cols,
                        mb_rows,
                        slice_qp,
                        num_ref_idx_l0_active,
                        chroma_qp_index_offset,
                        pps.as_ref().map(|p| p.transform_8x8_mode_flag).unwrap_or(false),
                        &mut crate::trace::NoopTracer,
                    )
                };
                match p_result {
                    Ok(parsed) => {
                        // Explicit weighted prediction (§8.4.2.3.2): only P/SP
                        // slices with `weighted_pred_flag` carry a
                        // `pred_weight_table`; P-slices never use implicit
                        // weighting (that's a B-slice-only, bi-pred concept).
                        let weighted_pred = match (
                            pps.as_ref().map(|p| p.weighted_pred_flag).unwrap_or(false),
                            &header.pred_weight_table,
                        ) {
                            (true, Some(pwt)) => crate::reconstruct::WeightedPred::Explicit {
                                luma_log2_wd: pwt.luma_log2_weight_denom,
                                chroma_log2_wd: pwt.chroma_log2_weight_denom,
                                l0: pwt.l0.clone(),
                                l1: Vec::new(),
                            },
                            _ => crate::reconstruct::WeightedPred::Default,
                        };
                        let mut recon = crate::reconstruct::reconstruct_inter_frame(
                            &parsed.macroblocks,
                            &parsed.mv_store,
                            &ref_frames,
                            mb_cols,
                            mb_rows,
                            width,
                            height,
                            chroma_qp_index_offset,
                            scaling,
                            &weighted_pred,
                            &mut crate::trace::NoopTracer,
                        );

                        let deblock_params = crate::deblock::DeblockParams {
                            disable_idc: header.disable_deblocking_filter_idc as u8,
                            alpha_offset_div2: header.slice_alpha_c0_offset_div2,
                            beta_offset_div2: header.slice_beta_offset_div2,
                            chroma_qp_index_offset,
                        };
                        let mb_info: Vec<Vec<crate::deblock::DeblockMbInfo>> = parsed
                            .macroblocks
                            .chunks(mb_cols as usize)
                            .enumerate()
                            .map(|(row_idx, row)| {
                                row.iter()
                                    .enumerate()
                                    .map(|(col_idx, mb)| {
                                        let idx = row_idx * mb_cols as usize + col_idx;
                                        let nz = parsed.nz[idx].luma;
                                        let cells = parsed
                                            .mv_store
                                            .cells_of(idx)
                                            .unwrap_or([crate::mv::MvCell::INTRA; 16]);
                                        crate::deblock::DeblockMbInfo::new(
                                            mb.mb_type, nz, cells, mb.qp,
                                        )
                                    })
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
                                    &mut recon.luma,
                                    recon.luma_stride,
                                    col_idx,
                                    row_idx,
                                    cur,
                                    left,
                                    top,
                                    deblock_params,
                                );
                                crate::deblock::deblock_chroma_mb(
                                    &mut recon.chroma_cb,
                                    &mut recon.chroma_cr,
                                    recon.chroma_stride,
                                    col_idx,
                                    row_idx,
                                    cur,
                                    left,
                                    top,
                                    deblock_params,
                                );
                            }
                        }

                        let mut data = recon.luma;
                        data.extend(recon.chroma_cb);
                        data.extend(recon.chroma_cr);

                        self.frame_count += 1;
                        let frame = VideoFrame {
                            pts: packet.pts,
                            dts: packet.dts,
                            data,
                            width,
                            height,
                            pixel_format: PixelFormat::Yuv420p,
                            is_key_frame: matches!(nal.nal_unit_type, NalUnitType::IdrSlice),
                        };
                        // A reference P picture joins the DPB under the marking
                        // its own slice header requested (§8.2.5) — this is the
                        // path that carries MMCO commands, since only non-IDR
                        // slices have a `dec_ref_pic_marking` command list.
                        self.store_reference_picture(nal, &sps, &header, &frame);
                        return Ok(frame);
                    }
                    Err(e) => {
                        eprintln!("P CABAC parse error: {e:?}");
                        // Fall through to the skip scaffold.
                    }
                }
            }
        }

        // Real CAVLC B-slice decode: bi-predictive motion compensation against
        // RefPicList0 and RefPicList1 (§8.4/§8.5).
        let is_b_slice = header.slice_type == crate::slice::SliceType::B;
        if is_b_slice && header.first_mb_in_slice == 0 {
            let slice_qp = 26
                + pps.as_ref().map(|p| p.pic_init_qp_minus26).unwrap_or(0)
                + header.slice_qp_delta;
            let chroma_qp_index_offset =
                pps.as_ref().map(|p| p.chroma_qp_index_offset).unwrap_or(0);
            let num_ref_idx_l0_active = header.num_ref_idx_l0_active_minus1 + 1;
            let num_ref_idx_l1_active = header.num_ref_idx_l1_active_minus1 + 1;
            let pic_num_ctx = crate::ref_pic::PicNumContext::new(
                &sps,
                header.frame_num,
                header.field_pic_flag,
                header.bottom_field_flag,
            );
            // Compute current POC using a scratch state so we don't advance the
            // real poc_state (store_reference_picture handles that later).
            let is_idr_b = matches!(nal.nal_unit_type, NalUnitType::IdrSlice);
            let current_poc = {
                let mut scratch = self.poc_state.clone();
                crate::ref_pic::derive_pic_order_cnt(
                    &sps,
                    is_idr_b,
                    nal.nal_ref_idc != 0,
                    header.frame_num,
                    header.pic_order_cnt_lsb,
                    header.field_pic_flag,
                    header.bottom_field_flag,
                    header.delta_pic_order_cnt_bottom,
                    &mut scratch,
                )
                .unwrap_or(0)
            };

            let l0_list = crate::ref_pic::build_ref_list_l0_b_slice(
                &self.dpb,
                num_ref_idx_l0_active as usize,
                current_poc,
                pic_num_ctx,
                &header.ref_pic_list_modification_l0,
            );
            let l1_list = crate::ref_pic::build_ref_list_l1(
                &self.dpb,
                num_ref_idx_l1_active as usize,
                current_poc,
                pic_num_ctx,
                &header.ref_pic_list_modification_l1,
            );

            if let (Some(ref_l0), Some(ref_l1)) = (l0_list, l1_list) {
                let ref_frames_l0: Vec<tpt_kinetix_core::frame::VideoFrame> =
                    ref_l0.iter().map(|e| e.frame.clone()).collect();
                let ref_frames_l1: Vec<tpt_kinetix_core::frame::VideoFrame> =
                    ref_l1.iter().map(|e| e.frame.clone()).collect();
                let mut reader = crate::bitreader::BitReader::new(&nal.rbsp);
                reader.seek_to_bit(header.data_bit_offset);
                let entropy_coding_mode_flag =
                    pps.as_ref().map(|p| p.entropy_coding_mode_flag).unwrap_or(false);
                let b_result = if entropy_coding_mode_flag {
                    reader.byte_align();
                    crate::slice_data::parse_b_slice_cabac(
                        reader.remaining_bytes(),
                        mb_cols,
                        mb_rows,
                        slice_qp,
                        header.cabac_init_idc as usize,
                        num_ref_idx_l0_active,
                        num_ref_idx_l1_active,
                        chroma_qp_index_offset,
                        pps.as_ref().map(|p| p.transform_8x8_mode_flag).unwrap_or(false),
                        &mut crate::trace::NoopTracer,
                    )
                } else {
                    crate::slice_data::parse_b_slice(
                        &mut reader,
                        mb_cols,
                        mb_rows,
                        slice_qp,
                        num_ref_idx_l0_active,
                        num_ref_idx_l1_active,
                        chroma_qp_index_offset,
                        pps.as_ref().map(|p| p.transform_8x8_mode_flag).unwrap_or(false),
                        &mut crate::trace::NoopTracer,
                    )
                };
                match b_result {
                    Ok(parsed) => {
                        // Weighted bi-prediction (§8.4.2.3.2): explicit when
                        // `weighted_bipred_idc == 1` (uses the parsed
                        // `pred_weight_table`), implicit when `== 2` (weights
                        // derived per-block from each reference's POC
                        // distance to the current picture).
                        let weighted_bipred_idc =
                            pps.as_ref().map(|p| p.weighted_bipred_idc).unwrap_or(0);
                        let weighted_pred = match (weighted_bipred_idc, &header.pred_weight_table)
                        {
                            (1, Some(pwt)) => crate::reconstruct::WeightedPred::Explicit {
                                luma_log2_wd: pwt.luma_log2_weight_denom,
                                chroma_log2_wd: pwt.chroma_log2_weight_denom,
                                l0: pwt.l0.clone(),
                                l1: pwt.l1.clone(),
                            },
                            (2, _) => crate::reconstruct::WeightedPred::Implicit {
                                l0_poc: ref_l0.iter().map(|e| e.pic_order_cnt).collect(),
                                l1_poc: ref_l1.iter().map(|e| e.pic_order_cnt).collect(),
                                cur_poc: current_poc,
                            },
                            _ => crate::reconstruct::WeightedPred::Default,
                        };
                        let mut recon = crate::reconstruct::reconstruct_b_frame(
                            &parsed.macroblocks,
                            &parsed.mv_store,
                            &ref_frames_l0,
                            &ref_frames_l1,
                            mb_cols,
                            mb_rows,
                            width,
                            height,
                            chroma_qp_index_offset,
                            scaling,
                            &weighted_pred,
                            &mut crate::trace::NoopTracer,
                        );

                        let deblock_params = crate::deblock::DeblockParams {
                            disable_idc: header.disable_deblocking_filter_idc as u8,
                            alpha_offset_div2: header.slice_alpha_c0_offset_div2,
                            beta_offset_div2: header.slice_beta_offset_div2,
                            chroma_qp_index_offset,
                        };
                        let mb_info: Vec<Vec<crate::deblock::DeblockMbInfo>> = parsed
                            .macroblocks
                            .chunks(mb_cols as usize)
                            .enumerate()
                            .map(|(row_idx, row)| {
                                row.iter()
                                    .enumerate()
                                    .map(|(col_idx, mb)| {
                                        let idx = row_idx * mb_cols as usize + col_idx;
                                        let nz = parsed.nz[idx].luma;
                                        let cells = parsed
                                            .mv_store
                                            .cells_of(idx)
                                            .unwrap_or([crate::mv::MvCell::INTRA; 16]);
                                        crate::deblock::DeblockMbInfo::new(
                                            mb.mb_type, nz, cells, mb.qp,
                                        )
                                    })
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
                                    &mut recon.luma,
                                    recon.luma_stride,
                                    col_idx,
                                    row_idx,
                                    cur,
                                    left,
                                    top,
                                    deblock_params,
                                );
                                crate::deblock::deblock_chroma_mb(
                                    &mut recon.chroma_cb,
                                    &mut recon.chroma_cr,
                                    recon.chroma_stride,
                                    col_idx,
                                    row_idx,
                                    cur,
                                    left,
                                    top,
                                    deblock_params,
                                );
                            }
                        }

                        let mut data = recon.luma;
                        data.extend(recon.chroma_cb);
                        data.extend(recon.chroma_cr);

                        self.frame_count += 1;
                        let frame = VideoFrame {
                            pts: packet.pts,
                            dts: packet.dts,
                            data,
                            width,
                            height,
                            pixel_format: PixelFormat::Yuv420p,
                            is_key_frame: false,
                        };
                        self.store_reference_picture(nal, &sps, &header, &frame);
                        return Ok(frame);
                    }
                    Err(e) => {
                        let _ = e;
                        // Fall through to the skip scaffold.
                    }
                }
            }
        }

        // Attempt I_PCM path for I/SI slices.
        if is_i_slice && header.first_mb_in_slice == 0 {
            let slice_qp = 26
                + pps.as_ref().map(|p| p.pic_init_qp_minus26).unwrap_or(0)
                + header.slice_qp_delta;
            let mut reader = crate::bitreader::BitReader::new(&nal.rbsp);
            reader.seek_to_bit(header.data_bit_offset);
            if let Ok(mb_rows_data) =
                self.parse_i_pcm_slice(&mut reader, mb_cols, mb_rows, slice_qp)
            {
                return self.reconstruct_mb_rows(
                    nal.nal_unit_type,
                    width,
                    height,
                    packet,
                    mb_rows_data,
                );
            }
        }

        // Fallback: all-skip scaffold.
        self.emit_skip_frame(nal.nal_unit_type, width, height, packet)
    }

    /// Emit a flat-grey skip frame as the scaffold fallback.
    fn emit_skip_frame(
        &mut self,
        nal_type: NalUnitType,
        width: u32,
        height: u32,
        packet: &Packet,
    ) -> Result<VideoFrame, KinetixError> {
        let mb_cols = width.div_ceil(16);
        let mb_rows = height.div_ceil(16);
        let mb_rows_data = self.build_skip_mb_rows(mb_cols, mb_rows, 26);
        self.reconstruct_mb_rows(nal_type, width, height, packet, mb_rows_data)
    }

    /// Reconstruct a grid of macroblock rows into a `VideoFrame`.
    fn reconstruct_mb_rows(
        &mut self,
        nal_type: NalUnitType,
        width: u32,
        height: u32,
        packet: &Packet,
        mb_rows_data: Vec<Vec<Macroblock>>,
    ) -> Result<VideoFrame, KinetixError> {
        let _mb_cols = width.div_ceil(16);
        let _mb_rows = height.div_ceil(16);
        let luma_stride = width as usize;
        let chroma_stride = (width / 2) as usize;
        let luma_size = luma_stride * height as usize;
        let chroma_size = chroma_stride * (height as usize / 2).max(1);

        let mut luma = vec![128u8; luma_size];
        let mut chroma_cb = vec![128u8; chroma_size];
        let mut chroma_cr = vec![128u8; chroma_size];

        let any_intra = mb_rows_data.iter().flatten().any(|mb| {
            matches!(
                mb.mb_type,
                MbType::Intra4x4 | MbType::Intra16x16 { .. } | MbType::IPcm
            )
        });

        if any_intra {
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
                let copy_len = luma_row.len().min(luma.len().saturating_sub(y_off));
                if copy_len > 0 {
                    luma[y_off..y_off + copy_len].copy_from_slice(&luma_row[..copy_len]);
                }
                let c_off = row_idx * 8 * chroma_stride;
                let cc_len = cb_row.len().min(chroma_cb.len().saturating_sub(c_off));
                if cc_len > 0 {
                    chroma_cb[c_off..c_off + cc_len].copy_from_slice(&cb_row[..cc_len]);
                    chroma_cr[c_off..c_off + cc_len].copy_from_slice(&cr_row[..cc_len]);
                }
            }
        }

        let deblock_params = crate::deblock::DeblockParams::default();
        let mb_info: Vec<Vec<crate::deblock::DeblockMbInfo>> = mb_rows_data
            .iter()
            .map(|row| {
                row.iter()
                    .map(|mb| {
                        // Scaffold fallback path: no real per-block coefficient/motion
                        // data is available, so approximate with the old whole-MB
                        // proxy broadcast uniformly (matches prior behaviour).
                        let nz = if mb.skip { [0u8; 16] } else { [1u8; 16] };
                        crate::deblock::DeblockMbInfo::new(
                            mb.mb_type,
                            nz,
                            [crate::mv::MvCell::default(); 16],
                            mb.qp,
                        )
                    })
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

    /// Build a `mb_rows × mb_cols` grid of skip macroblocks at `qp`.
    fn build_skip_mb_rows(&self, mb_cols: u32, mb_rows: u32, qp: i32) -> Vec<Vec<Macroblock>> {
        (0..mb_rows)
            .map(|_| {
                (0..mb_cols)
                    .map(|_| {
                        let mut mb = Macroblock::new_skip();
                        mb.qp = qp;
                        mb
                    })
                    .collect()
            })
            .collect()
    }

    /// Parse an I-slice where every macroblock is I_PCM (§7.4.4).
    ///
    /// Each I_PCM macroblock carries 384 raw bytes (256 luma + 64 Cb + 64 Cr
    /// for 4:2:0). Returns `Err` on EOF.
    fn parse_i_pcm_slice(
        &self,
        r: &mut crate::bitreader::BitReader,
        mb_cols: u32,
        mb_rows: u32,
        qp: i32,
    ) -> Result<Vec<Vec<Macroblock>>, crate::slice_data::SliceDataError> {
        let mut rows: Vec<Vec<Macroblock>> = Vec::with_capacity(mb_rows as usize);
        for _mb_y in 0..mb_rows {
            let mut row = Vec::with_capacity(mb_cols as usize);
            for _mb_x in 0..mb_cols {
                let mut mb = Macroblock::new_skip();
                mb.mb_type = MbType::IPcm;
                mb.skip = false;
                mb.qp = qp;
                let mut samples = Vec::with_capacity(384);
                for _ in 0..384 {
                    let byte = r
                        .read_u8()
                        .ok_or(crate::slice_data::SliceDataError::Eof("I_PCM byte"))?;
                    samples.push(byte);
                }
                mb.pcm_samples = samples;
                row.push(mb);
            }
            rows.push(row);
        }
        Ok(rows)
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
        MbType::IPcm => {
            let y_size = 16 * 16;
            let c_size = 8 * 8;
            let samples = &mb.pcm_samples;
            if samples.len() >= y_size {
                for row in 0..16usize {
                    let src_off = row * 16;
                    let dst_off = (base_y + row) * luma_stride + base_x;
                    let copy_len = 16.min(planes.luma.len().saturating_sub(dst_off));
                    if copy_len > 0 {
                        planes.luma[dst_off..dst_off + copy_len]
                            .copy_from_slice(&samples[src_off..src_off + copy_len]);
                    }
                }
            }
            if samples.len() >= y_size + c_size * 2 {
                for comp in 0..2usize {
                    let (dst_off, dst) = if comp == 0 {
                        let off = base_y * chroma_stride + base_x;
                        (off, &mut *chroma_cb)
                    } else {
                        let off = base_y * chroma_stride + base_x;
                        (off, &mut *chroma_cr)
                    };
                    let src_off = y_size + comp * c_size;
                    for row in 0..8usize {
                        let off = dst_off + row * chroma_stride;
                        let copy_len = 8.min(dst.len().saturating_sub(off));
                        if copy_len > 0 {
                            dst[off..off + copy_len].copy_from_slice(
                                &samples[src_off + row * 8..src_off + row * 8 + copy_len],
                            );
                        }
                    }
                }
            }
        }
        MbType::Intra4x4 => {
            let mut top = [None; 64];
            let mut left = [None; 64];
            let mut top_left = [None; 64];
            let height = luma.len() / luma_stride.max(1);
            for b in 0..16usize {
                let bx = (b % 4) * 4;
                let by = (b / 4) * 4;
                for i in 0..4usize {
                    let tx = base_x + bx + i;
                    let ty = base_y as isize + by as isize - 1;
                    top[b * 4 + i] = if ty >= 0 && (ty as usize) < height {
                        luma.get(ty as usize * luma_stride + tx).copied()
                    } else {
                        None
                    };
                    let lx = base_x as isize + bx as isize - 1;
                    let ly = base_y + by + i;
                    left[b * 4 + i] = if lx >= 0 {
                        luma.get(ly * luma_stride + lx as usize).copied()
                    } else {
                        None
                    };
                }
                let cx = base_x as isize + bx as isize - 1;
                let cy = base_y as isize + by as isize - 1;
                top_left[b] = if cx >= 0 && cy >= 0 {
                    luma.get(cy as usize * luma_stride + cx as usize).copied()
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
            for i in 0..16usize {
                top[i] = luma
                    .get((base_y as isize - 1).max(0) as usize * luma_stride + base_x + i)
                    .copied();
                left[i] = if (base_x as isize - 1) >= 0 {
                    luma.get(base_y * luma_stride + base_x - 1 + i * luma_stride)
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
        for i in 0..8usize {
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

impl H264Decoder {
/// Reconstruct an interlaced (PAFF / MBAFF) slice.
///
/// Currently handles **PAFF field pictures** (`field_pic_flag == true`):
/// each field is decoded into a half-height YUV420p buffer, stored in the DPB
/// as a field, and paired with its complementary field for output interleaving
/// once both have been reconstructed (see `decode_interlaced` / `accumulate_field`).
///
/// PAFF frame pictures and MBAFF (which require the macroblock-pair decode
/// ordering and neighbour derivation of §6.4.10.1) return [`InterlacedOutcome::Fallback`]
/// so the caller treats them as unsupported and applies the strict / scaffold
/// path.
#[allow(clippy::too_many_arguments)]
fn decode_interlaced<T: DecodeTracer>(
    &mut self,
    nal: &crate::nal::NalUnit,
    sps: &SeqParameterSet,
    pps: Option<&PicParameterSet>,
    width: u32,
    height: u32,
    packet: &Packet,
    tracer: &mut T,
) -> Result<InterlacedOutcome, KinetixError> {
    use crate::slice::{SliceHeader, SliceHeaderContext, SliceType};

    let entropy_coding_mode_flag = pps.map(|p| p.entropy_coding_mode_flag).unwrap_or(false);
    let mb_adaptive = sps.mb_adaptive_frame_field_flag;

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
        entropy_coding_mode_flag,
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

    let header = match SliceHeader::parse_with_context(
        &nal.rbsp,
        nal.nal_unit_type,
        nal.nal_ref_idc,
        &ctx,
    ) {
        Ok(h) => h,
        Err(_) => return Ok(InterlacedOutcome::Fallback),
    };

    // Only PAFF field pictures are handled in this phase.
    if mb_adaptive || !header.field_pic_flag {
        return Ok(InterlacedOutcome::Fallback);
    }
    if header.first_mb_in_slice != 0 {
        return Ok(InterlacedOutcome::Fallback);
    }

    let chroma_qp_index_offset = pps.map(|p| p.chroma_qp_index_offset).unwrap_or(0);
    let mb_cols = width.div_ceil(16);
    let mb_rows_field = height.div_ceil(32);
    let field_height = height / 2;

    // PAFF P-field picture: field-based inter prediction (§8.2.4.2.5 +
    // §8.4.2.2.1). The reference list is built from field references and each
    // field macroblock is motion-compensated at field parity into a half-height
    // buffer, which is then interleaved with its complementary field.
    if matches!(header.slice_type, SliceType::P) {
        return self.decode_interlaced_p_field(
            nal,
            sps,
            &header,
            entropy_coding_mode_flag,
            pps,
            mb_cols,
            mb_rows_field,
            field_height,
            width,
            chroma_qp_index_offset,
            tracer,
            packet,
        );
    }

    if !matches!(header.slice_type, SliceType::I | SliceType::Si) {
        // Inter (B) field pictures: not yet handled (G.2). Fall back so they
        // are treated as unsupported slices by the caller.
        return Ok(InterlacedOutcome::Fallback);
    }

    let scaling = pps.map(|p| &p.scaling).unwrap_or(&sps.scaling);
    let pic_init_qp = 26 + pps.map(|p| p.pic_init_qp_minus26).unwrap_or(0);
    let slice_qp = pic_init_qp + header.slice_qp_delta;

    let mut reader = crate::bitreader::BitReader::new(&nal.rbsp);
    reader.seek_to_bit(header.data_bit_offset);

    let parsed = if entropy_coding_mode_flag {
        reader.byte_align();
        crate::slice_data::parse_i_slice_cabac(
            reader.remaining_bytes(),
            mb_cols,
            mb_rows_field,
            slice_qp,
            mb_adaptive,
            header.field_pic_flag,
            pps.map(|p| p.transform_8x8_mode_flag).unwrap_or(false),
            tracer,
        )
    } else {
        crate::slice_data::parse_i_slice(
            &mut reader,
            mb_cols,
            mb_rows_field,
            slice_qp,
            chroma_qp_index_offset,
            pps.map(|p| p.transform_8x8_mode_flag).unwrap_or(false),
            mb_adaptive,
            header.field_pic_flag,
            tracer,
        )
    };
    let parsed = match parsed {
        Ok(p) => p,
        Err(_) => return Ok(InterlacedOutcome::Fallback),
    };

    let mut recon = crate::reconstruct::reconstruct_intra_frame(
        &parsed.macroblocks,
        mb_cols,
        mb_rows_field,
        width,
        field_height,
        chroma_qp_index_offset,
        scaling,
        &crate::reconstruct::WeightedPred::Default,
        tracer,
    );

    let deblock_params = crate::deblock::DeblockParams {
        disable_idc: header.disable_deblocking_filter_idc as u8,
        alpha_offset_div2: header.slice_alpha_c0_offset_div2,
        beta_offset_div2: header.slice_beta_offset_div2,
        chroma_qp_index_offset,
    };
    Self::deblock_field(&mut recon, &parsed, mb_cols, mb_rows_field, deblock_params);
    self.finalize_field(recon, nal, sps, &header, packet)
}

/// PAFF P-field picture decode: build the field reference list (§8.2.4.2.5),
/// parse the field P-slice, motion-compensate each field macroblock at field
/// parity into a half-height buffer, deblock, and interleave with its
/// complementary field for output.
#[allow(clippy::too_many_arguments)]
fn decode_interlaced_p_field<T: DecodeTracer>(
    &mut self,
    nal: &crate::nal::NalUnit,
    sps: &SeqParameterSet,
    header: &crate::slice::SliceHeader,
    entropy_coding_mode_flag: bool,
    pps: Option<&PicParameterSet>,
    mb_cols: u32,
    mb_rows_field: u32,
    field_height: u32,
    width: u32,
    chroma_qp_index_offset: i32,
    tracer: &mut T,
    packet: &Packet,
) -> Result<InterlacedOutcome, KinetixError> {
    use crate::ref_pic::{build_field_ref_list_l0, PicNumContext};

    let num_ref_idx_l0_active = header.num_ref_idx_l0_active_minus1 + 1;
    let pic_num_ctx = PicNumContext::new(sps, header.frame_num, true, header.bottom_field_flag);
    let ref_list = match build_field_ref_list_l0(
        &self.dpb,
        header.bottom_field_flag,
        num_ref_idx_l0_active as usize,
        pic_num_ctx,
    ) {
        Some(l) => l,
        None => return Ok(InterlacedOutcome::Fallback),
    };

    let weighted_pred = match (
        pps.map(|p| p.weighted_pred_flag).unwrap_or(false),
        &header.pred_weight_table,
    ) {
        (true, Some(pwt)) => crate::reconstruct::WeightedPred::Explicit {
            luma_log2_wd: pwt.luma_log2_weight_denom,
            chroma_log2_wd: pwt.chroma_log2_weight_denom,
            l0: pwt.l0.clone(),
            l1: Vec::new(),
        },
        _ => crate::reconstruct::WeightedPred::Default,
    };

    let pic_init_qp = 26 + pps.map(|p| p.pic_init_qp_minus26).unwrap_or(0);
    let slice_qp = pic_init_qp + header.slice_qp_delta;
    let scaling = pps.map(|p| &p.scaling).unwrap_or(&sps.scaling);

    let mut reader = crate::bitreader::BitReader::new(&nal.rbsp);
    reader.seek_to_bit(header.data_bit_offset);

    let transform_8x8 = pps.map(|p| p.transform_8x8_mode_flag).unwrap_or(false);
    let parsed = if entropy_coding_mode_flag {
        reader.byte_align();
        crate::slice_data::parse_p_slice_cabac(
            reader.remaining_bytes(),
            mb_cols,
            mb_rows_field,
            slice_qp,
            header.cabac_init_idc as usize,
            num_ref_idx_l0_active,
            chroma_qp_index_offset,
            transform_8x8,
            tracer,
        )
    } else {
        crate::slice_data::parse_p_slice(
            &mut reader,
            mb_cols,
            mb_rows_field,
            slice_qp,
            num_ref_idx_l0_active,
            chroma_qp_index_offset,
            transform_8x8,
            tracer,
        )
    };
    let parsed = match parsed {
        Ok(p) => p,
        Err(_) => return Ok(InterlacedOutcome::Fallback),
    };

    let mut recon = crate::reconstruct::reconstruct_inter_field_frame(
        &parsed.macroblocks,
        &parsed.mv_store,
        &ref_list,
        mb_cols,
        mb_rows_field,
        width,
        field_height,
        chroma_qp_index_offset,
        scaling,
        &weighted_pred,
        tracer,
    );

    let deblock_params = crate::deblock::DeblockParams {
        disable_idc: header.disable_deblocking_filter_idc as u8,
        alpha_offset_div2: header.slice_alpha_c0_offset_div2,
        beta_offset_div2: header.slice_beta_offset_div2,
        chroma_qp_index_offset,
    };
    Self::deblock_field(&mut recon, &parsed, mb_cols, mb_rows_field, deblock_params);
    self.finalize_field(recon, nal, sps, header, packet)
}

/// Apply the in-loop deblocking filter to a half-height field buffer
/// (`ReconstructedFrame`). The deblocking runs per-field, so it never crosses
/// the field boundary (the complementary field is not in this buffer). `parsed`
/// supplies the per-block non-zero / motion state needed for boundary strength.
fn deblock_field(
    recon: &mut ReconstructedFrame,
    parsed: &ParsedSlice,
    mb_cols: u32,
    _mb_rows_field: u32,
    params: crate::deblock::DeblockParams,
) {
    let mb_info: Vec<Vec<crate::deblock::DeblockMbInfo>> = parsed
        .macroblocks
        .chunks(mb_cols as usize)
        .enumerate()
        .map(|(row_idx, row)| {
            row.iter()
                .enumerate()
                .map(|(col_idx, mb)| {
                    let idx = row_idx * mb_cols as usize + col_idx;
                    let nz = parsed.nz[idx].luma;
                    let cells = parsed
                        .mv_store
                        .cells_of(idx)
                        .unwrap_or([crate::mv::MvCell::INTRA; 16]);
                    crate::deblock::DeblockMbInfo::new(mb.mb_type, nz, cells, mb.qp)
                })
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
                &mut recon.luma,
                recon.luma_stride,
                col_idx,
                row_idx,
                cur,
                left,
                top,
                params,
            );
            crate::deblock::deblock_chroma_mb(
                &mut recon.chroma_cb,
                &mut recon.chroma_cr,
                recon.chroma_stride,
                col_idx,
                row_idx,
                cur,
                left,
                top,
                params,
            );
        }
    }
}

/// Assemble a half-height `ReconstructedFrame` into a `VideoFrame`, store it in
/// the DPB as a field reference (so later inter slices can build their field
/// reference lists per §8.2.4.2.5), and pair it with its complementary field
/// for output interleaving.
fn finalize_field(
    &mut self,
    recon: ReconstructedFrame,
    nal: &crate::nal::NalUnit,
    sps: &SeqParameterSet,
    header: &crate::slice::SliceHeader,
    packet: &Packet,
) -> Result<InterlacedOutcome, KinetixError> {
    let field_height = (recon.luma.len() / recon.luma_stride) as u32;
    let mut data = recon.luma;
    data.extend(recon.chroma_cb);
    data.extend(recon.chroma_cr);

    let field_frame = VideoFrame {
        pts: packet.pts,
        dts: packet.dts,
        data,
        width: recon.luma_stride as u32,
        height: field_height,
        pixel_format: PixelFormat::Yuv420p,
        is_key_frame: matches!(nal.nal_unit_type, NalUnitType::IdrSlice),
    };

    // Store the half-height field in the DPB so later inter slices can build
    // their field reference lists from it (§8.2.4.2.5).
    self.store_reference_picture(nal, sps, header, &field_frame);

    // Buffer the field and emit the interleaved frame once the pair is complete.
    match self.accumulate_field(field_frame, header.bottom_field_flag, header.frame_num) {
        Some(full) => Ok(InterlacedOutcome::Frame(full)),
        None => Ok(InterlacedOutcome::Handled),
    }
}

/// Buffer one reconstructed field and, when its complementary field has also
/// been decoded, interleave the two half-height fields into a full (progressive)
/// frame suitable for output.
fn accumulate_field(
    &mut self,
    field: VideoFrame,
    bottom: bool,
    key: u32,
) -> Option<VideoFrame> {
    match self.field_accum.take() {
        Some(mut accum) if accum.key == key => {
            if bottom {
                accum.bottom = Some(field);
            } else {
                accum.top = Some(field);
            }
            if let (Some(top), Some(bottom)) = (&accum.top, &accum.bottom) {
                let full = Self::interleave_fields(top, bottom);
                self.field_accum = None;
                Some(full)
            } else {
                self.field_accum = Some(accum);
                None
            }
        }
        // Either empty, or the key changed (a new frame's first field arrived
        // before the previous pair completed): start a fresh accumulator.
        _ => {
            let mut accum = FieldAccum {
                key,
                top: None,
                bottom: None,
                last_bottom: bottom,
            };
            if bottom {
                accum.bottom = Some(field);
            } else {
                accum.top = Some(field);
            }
            self.field_accum = Some(accum);
            None
        }
    }
}

/// Interleave two complementary half-height fields into a full (progressive)
/// frame: the top field's rows occupy the even scanlines, the bottom field's
/// rows the odd scanlines (§6.4.10.1 / §8.4.2.2.1).
fn interleave_fields(top: &VideoFrame, bottom: &VideoFrame) -> VideoFrame {
    let w = top.width as usize;
    let field_h = top.height as usize; // half height
    let full_h = field_h * 2;
    let luma_len = w * field_h;
    let chroma_w = w / 2;
    let chroma_field_h = field_h / 2;
    let chroma_len = chroma_w * chroma_field_h;

    let mut data = vec![0u8; w * full_h + 2 * chroma_w * chroma_field_h * 2];

    // Luma.
    for y in 0..field_h {
        let src = &top.data[y * w..(y + 1) * w];
        data[(2 * y) * w..(2 * y + 1) * w].copy_from_slice(src);
        let src = &bottom.data[y * w..(y + 1) * w];
        data[((2 * y + 1) * w)..((2 * y + 2) * w)].copy_from_slice(src);
    }
    // Chroma Cb then Cr, each half-height plane interleaved the same way.
    let mut dst_off = w * full_h;
    for comp in 0..2 {
        let (src_top, src_bottom) = if comp == 0 {
            (
                &top.data[luma_len..luma_len + chroma_len],
                &bottom.data[luma_len..luma_len + chroma_len],
            )
        } else {
            (
                &top.data[luma_len + chroma_len..luma_len + 2 * chroma_len],
                &bottom.data[luma_len + chroma_len..luma_len + 2 * chroma_len],
            )
        };
        for y in 0..chroma_field_h {
            let ro = dst_off + 2 * y * chroma_w;
            data[ro..ro + chroma_w].copy_from_slice(&src_top[y * chroma_w..(y + 1) * chroma_w]);
            let ro = dst_off + (2 * y + 1) * chroma_w;
            data[ro..ro + chroma_w].copy_from_slice(&src_bottom[y * chroma_w..(y + 1) * chroma_w]);
        }
        dst_off += 2 * chroma_w * chroma_field_h;
    }

    VideoFrame {
        pts: top.pts,
        dts: top.dts,
        data,
        width: top.width,
        height: full_h as u32,
        pixel_format: PixelFormat::Yuv420p,
        is_key_frame: top.is_key_frame,
    }
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
            33, 31, 0, 0, 1, 255, 243, 0, 0, 1, 39, 255, 0, 1, 105, 164, 0, 0, 0, 105, 105, 105, 3,
            3, 3, 255, 255, 255, 255, 255, 255, 255, 15, 0, 0, 1, 33, 5, 4, 1, 33, 5, 4, 217,
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

    #[test]
    fn fuzz_regression_panic_7beba6be278af7cb1bf0e7065b6a81860bb29716() {
        // An SPS with an out-of-range log2_max_frame_num_minus4 (63, spec
        // limits it to 0..=12) used to make the slice header parser request
        // a 67-bit `frame_num` read, tripping `read_bits`'s `n <= 32` guard.
        let data = vec![
            0, 0, 1, 103, 0, 129, 129, 129, 1, 5, 255, 255, 255, 0, 0, 0, 1, 5, 255, 255, 255, 254,
            255, 255, 255, 1, 1, 129, 129, 129, 126, 129, 43,
        ];
        let mut dec = H264Decoder::new();
        let pkt = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data,
            stream_index: 0,
            is_key_frame: false,
        };
        let _ = dec.decode(&pkt);
    }

    /// Interleaving two complementary half-height fields reproduces an interlaced
    /// frame: the top field's samples land on even scanlines, the bottom field's
    /// on odd scanlines (§6.4.10.1 / §8.4.2.2.1).
    #[test]
    fn interleave_fields_places_top_and_bottom_parity() {
        let w = 16u32;
        let field_h = 8u32;
        let full_h = field_h * 2;

        let mut top = crate::macroblock::new_video_frame(w, field_h).unwrap();
        for px in top.data.iter_mut() {
            *px = 10;
        }
        let mut bottom = crate::macroblock::new_video_frame(w, field_h).unwrap();
        for px in bottom.data.iter_mut() {
            *px = 20;
        }

        let full = H264Decoder::interleave_fields(&top, &bottom);
        assert_eq!(full.width, w);
        assert_eq!(full.height, full_h);
        // Luma: even rows from top (10), odd rows from bottom (20).
        for y in 0..full_h {
            for x in 0..w {
                let v = full.data[(y * w + x) as usize];
                if y % 2 == 0 {
                    assert_eq!(v, 10, "row {y} should be top field");
                } else {
                    assert_eq!(v, 20, "row {y} should be bottom field");
                }
            }
        }
    }
}
