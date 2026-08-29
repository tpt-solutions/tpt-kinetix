//! Interlaced (PAFF / MBAFF) decode path for the H.264 decoder.
//!
//! Holds the field accumulator types and the `impl H264Decoder` block for
//! interlaced streams: PAFF field pictures, MBAFF frame pictures, and the
//! field pairing logic that interleaves half-height fields into full frames.

use tpt_kinetix_core::{
    error::KinetixError, frame::VideoFrame, packet::Packet, pixel_format::PixelFormat,
};

use crate::{
    nal::{NalUnit, NalUnitType},
    pps::PicParameterSet,
    reconstruct::ReconstructedFrame,
    slice_data::ParsedSlice,
    sps::SeqParameterSet,
    trace::DecodeTracer,
};

use super::H264Decoder;

/// Accumulated fields of one interlaced frame awaiting output interleaving.
///
/// In PAFF / MBAFF the two fields of a coded frame are decoded as separate
/// access units. `pending` holds the first field to arrive; `paired` becomes
/// `Some` once both the top and bottom field have been reconstructed, at which
/// point they are interleaved into the full output frame.
#[derive(Debug, Default)]
pub(super) struct FieldAccum {
    /// Pairing key — the field's `frame_num` (PAFF) or frame `PicOrderCnt`
    /// rounded to the frame (MBAFF). Two fields sharing a key belong to the
    /// same display frame.
    pub(super) key: u32,
    /// The reconstructed top field (half-height YUV420p plane set).
    pub(super) top: Option<VideoFrame>,
    /// The reconstructed bottom field (half-height YUV420p plane set).
    pub(super) bottom: Option<VideoFrame>,
    /// Whether the most recently stored field was the bottom field (used to
    /// detect a key change when the second field of a new frame appears before
    /// the first was paired).
    #[allow(dead_code)]
    pub(super) last_bottom: bool,
}

/// Result of attempting to decode an interlaced (PAFF / MBAFF) slice.
pub(super) enum InterlacedOutcome {
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
    pub(super) fn decode_interlaced<T: DecodeTracer>(
        &mut self,
        nal: &NalUnit,
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

        let chroma_qp_index_offset = pps.map(|p| p.chroma_qp_index_offset).unwrap_or(0);

        let header = match SliceHeader::parse_with_context(
            &nal.rbsp,
            nal.nal_unit_type,
            nal.nal_ref_idc,
            &ctx,
        ) {
            Ok(h) => h,
            Err(_) => return Ok(InterlacedOutcome::Fallback),
        };
        // MBAFF frames (SPS enables `mb_adaptive_frame_field_flag`, slice is a frame
        // picture) are handled per macroblock pair below (Phase G.4). PAFF frame
        // pictures remain unsupported and fall back.
        if mb_adaptive {
            return self.decode_interlaced_mbaff(
                nal,
                sps,
                &header,
                entropy_coding_mode_flag,
                pps,
                width,
                height,
                chroma_qp_index_offset,
                tracer,
                packet,
            );
        }
        if !header.field_pic_flag {
            return Ok(InterlacedOutcome::Fallback);
        }
        if header.first_mb_in_slice != 0 {
            return Ok(InterlacedOutcome::Fallback);
        }

        let coded_width = sps.coded_width_pixels();
        let coded_height = sps.coded_height_pixels();
        let mb_cols = coded_width / 16;
        let mb_rows_field = coded_height / 32;
        let field_height = coded_height / 2;

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

        if matches!(header.slice_type, SliceType::B) {
            return self.decode_interlaced_b_field(
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
            coded_width,
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

    /// Reconstruct an **MBAFF** I-slice frame (§6.4.10.1, Phase G.4).
    ///
    /// An MBAFF frame picture is a single access unit that decodes to a full
    /// interlaced frame (unlike PAFF, which splits the two fields across two
    /// access units). The parser reads `mb_field_decoding_flag` once per macroblock
    /// pair; [`crate::reconstruct::reconstruct_mbaff_intra_frame`] then places each
    /// pair into the interlaced frame using the pair's field/frame coding. The
    /// reconstructed frame is stored in the DPB as a frame reference and emitted
    /// directly as `InterlacedOutcome::Frame` (no field interleaving accumulator is
    /// needed).
    ///
    /// P/B MBAFF slices are parsed to completion (MBAFF pair-scan addressing fixed
    /// in #32q, ref-list construction in B1) but reconstruction is deferred to B3,
    /// so they return `InterlacedOutcome::Fallback` and the caller applies the
    /// strict / scaffold path; per [`crate::H264Decoder::capabilities`] interlaced
    /// decode is not yet pixel-exact.
    #[allow(clippy::too_many_arguments)]
    fn decode_interlaced_mbaff<T: DecodeTracer>(
        &mut self,
        nal: &NalUnit,
        sps: &SeqParameterSet,
        header: &crate::slice::SliceHeader,
        entropy_coding_mode_flag: bool,
        pps: Option<&PicParameterSet>,
        width: u32,
        height: u32,
        chroma_qp_index_offset: i32,
        tracer: &mut T,
        packet: &Packet,
    ) -> Result<InterlacedOutcome, KinetixError> {
        use crate::slice::SliceType;

        if header.first_mb_in_slice != 0 {
            return Ok(InterlacedOutcome::Fallback);
        }

        // Common setup for both P/B and I paths: slice QP and coded dimensions.
        let pic_init_qp = 26 + pps.map(|p| p.pic_init_qp_minus26).unwrap_or(0);
        let slice_qp = pic_init_qp + header.slice_qp_delta;
        let coded_width = sps.coded_width_pixels();
        let coded_height = sps.coded_height_pixels();
        let mb_cols = coded_width / 16;
        let mb_rows = coded_height / 16;

        if !matches!(header.slice_type, SliceType::I | SliceType::Si) {
            // P/B MBAFF slices (B5): the inter decode path is the default.
            // Field-coded pairs reuse the parity-plane convention from the
            // frame-coded path (the field-MC gate in reconstruct.rs stays).

            let num_ref_idx_l0_active = header.num_ref_idx_l0_active_minus1 + 1;
            let pic_num_ctx = crate::ref_pic::PicNumContext::new(
                sps,
                header.frame_num,
                header.field_pic_flag,
                header.bottom_field_flag,
            );

            // B1: build the frame reference lists (§8.2.4). MBAFF frames are
            // field_pic_flag=0 → ordinary frame ref lists, the same builders
            // decoder/mod.rs uses for progressive P/B — NOT the PAFF
            // build_field_ref_list_l0 path.
            let is_idr = matches!(nal.nal_unit_type, NalUnitType::IdrSlice);
            let current_poc = {
                let mut scratch = self.poc_state.clone();
                crate::ref_pic::derive_pic_order_cnt(
                    sps,
                    is_idr,
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

            let ref_list_l0 = if header.slice_type == SliceType::P {
                let list = crate::ref_pic::build_ref_list_l0(
                    &self.dpb,
                    num_ref_idx_l0_active as usize,
                    pic_num_ctx,
                    &header.ref_pic_list_modification_l0,
                );
                if let Some(ref_list) = &list {
                    crate::ref_pic::trace_ref_list("MBAFF P L0", ref_list, pic_num_ctx);
                }
                list
            } else {
                let list = crate::ref_pic::build_ref_list_l0_b_slice(
                    &self.dpb,
                    num_ref_idx_l0_active as usize,
                    current_poc,
                    pic_num_ctx,
                    &header.ref_pic_list_modification_l0,
                );
                if let Some(ref_list) = &list {
                    crate::ref_pic::trace_ref_list("MBAFF B L0", ref_list, pic_num_ctx);
                }
                list
            };
            let ref_list_l1 = if header.slice_type == SliceType::B {
                let num_ref_idx_l1_active = header.num_ref_idx_l1_active_minus1 + 1;
                let list = crate::ref_pic::build_ref_list_l1(
                    &self.dpb,
                    num_ref_idx_l1_active as usize,
                    current_poc,
                    pic_num_ctx,
                    &header.ref_pic_list_modification_l1,
                );
                if let Some(ref_list) = &list {
                    crate::ref_pic::trace_ref_list("MBAFF B L1", ref_list, pic_num_ctx);
                }
                list
            } else {
                None
            };

            let ref_frames_l0: Vec<tpt_kinetix_core::frame::VideoFrame> = ref_list_l0
                .as_ref()
                .map(|l| l.iter().map(|e| e.frame.clone()).collect())
                .unwrap_or_default();
            let ref_frames_l1: Vec<tpt_kinetix_core::frame::VideoFrame> = ref_list_l1
                .as_ref()
                .map(|l| l.iter().map(|e| e.frame.clone()).collect())
                .unwrap_or_default();

            // Co-located MV grid for direct-mode B prediction: reference 0 of
            // list 1 (§8.4.1.2.2/8.4.1.2.3).
            let colocated_mv: Option<Vec<[crate::mv::MvCell; 16]>> = ref_list_l1
                .as_ref()
                .and_then(|l| l.first())
                .and_then(|e| e.mv_grid.clone())
                .map(|g| (*g).clone());

            // B2: parse the P/B slice to completion. MBAFF pair-scan
            // addressing is fixed (#32q).
            let mut reader = crate::bitreader::BitReader::new(&nal.rbsp);
            reader.seek_to_bit(header.data_bit_offset);
            let num_ref_idx_l0_active = header.num_ref_idx_l0_active_minus1 + 1;
            let parsed = match header.slice_type {
                SliceType::P | SliceType::Sp => {
                    if entropy_coding_mode_flag {
                        reader.byte_align();
                        crate::slice_data::parse_p_slice_cabac(
                            reader.remaining_bytes(),
                            mb_cols,
                            mb_rows,
                            slice_qp,
                            sps.mb_adaptive_frame_field_flag,
                            header.field_pic_flag,
                            header.cabac_init_idc as usize,
                            num_ref_idx_l0_active,
                            chroma_qp_index_offset,
                            pps.map(|p| p.transform_8x8_mode_flag).unwrap_or(false),
                            tracer,
                        )
                    } else {
                        crate::slice_data::parse_p_slice(
                            &mut reader,
                            mb_cols,
                            mb_rows,
                            slice_qp,
                            num_ref_idx_l0_active,
                            chroma_qp_index_offset,
                            pps.map(|p| p.transform_8x8_mode_flag).unwrap_or(false),
                            sps.mb_adaptive_frame_field_flag,
                            header.field_pic_flag,
                            tracer,
                        )
                    }
                }
                SliceType::B => {
                    let num_ref_idx_l1_active = header.num_ref_idx_l1_active_minus1 + 1;
                    if entropy_coding_mode_flag {
                        reader.byte_align();
                        crate::slice_data::parse_b_slice_cabac(
                            reader.remaining_bytes(),
                            mb_cols,
                            mb_rows,
                            slice_qp,
                            sps.mb_adaptive_frame_field_flag,
                            header.field_pic_flag,
                            header.cabac_init_idc as usize,
                            num_ref_idx_l0_active,
                            num_ref_idx_l1_active,
                            chroma_qp_index_offset,
                            pps.map(|p| p.transform_8x8_mode_flag).unwrap_or(false),
                            colocated_mv.as_deref(),
                            tracer,
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
                            pps.map(|p| p.transform_8x8_mode_flag).unwrap_or(false),
                            colocated_mv.as_deref(),
                            tracer,
                        )
                    }
                }
                _ => unreachable!(),
            };
            let parsed = match parsed {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("interlaced P/B slice parse failed: {e}");
                    return Ok(InterlacedOutcome::Fallback);
                }
            };

            // B3: MC + reconstruction. For the all-frame-coded case
            // (mbaff_ip/mbaff_ibp) every macroblock has mb_field_flag == false,
            // so reconstruct_inter_frame_ex / reconstruct_b_frame_mbaff
            // collapse to progressive inter into contiguous halves. Field-coded
            // pairs exercise the parity-aware path behind the same gate.
            let scaling = pps.map(|p| &p.scaling).unwrap_or(&sps.scaling);
            let weighted_pred = if header.slice_type == SliceType::B {
                let weighted_bipred_idc = pps.map(|p| p.weighted_bipred_idc).unwrap_or(0);
                match (weighted_bipred_idc, &header.pred_weight_table) {
                    (1, Some(pwt)) => crate::reconstruct::WeightedPred::Explicit {
                        luma_log2_wd: pwt.luma_log2_weight_denom,
                        chroma_log2_wd: pwt.chroma_log2_weight_denom,
                        l0: pwt.l0.clone(),
                        l1: pwt.l1.clone(),
                    },
                    (2, _) => crate::reconstruct::WeightedPred::Implicit {
                        l0_poc: ref_list_l0
                            .as_ref()
                            .map(|l| l.iter().map(|e| e.pic_order_cnt).collect())
                            .unwrap_or_default(),
                        l1_poc: ref_list_l1
                            .as_ref()
                            .map(|l| l.iter().map(|e| e.pic_order_cnt).collect())
                            .unwrap_or_default(),
                        cur_poc: current_poc,
                    },
                    _ => crate::reconstruct::WeightedPred::Default,
                }
            } else {
                match (
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
                }
            };
            let mut recon = if header.slice_type == SliceType::B {
                crate::reconstruct::reconstruct_b_frame_mbaff(
                    &parsed.macroblocks,
                    &parsed.mv_store,
                    &ref_frames_l0,
                    &ref_frames_l1,
                    mb_cols,
                    mb_rows,
                    coded_width,
                    coded_height,
                    sps.mb_adaptive_frame_field_flag && !header.field_pic_flag,
                    chroma_qp_index_offset,
                    scaling,
                    &weighted_pred,
                    tracer,
                )
            } else {
                crate::reconstruct::reconstruct_inter_frame_ex(
                    &parsed.macroblocks,
                    &parsed.mv_store,
                    &ref_frames_l0,
                    mb_cols,
                    mb_rows,
                    coded_width,
                    coded_height,
                    sps.mb_adaptive_frame_field_flag && !header.field_pic_flag,
                    chroma_qp_index_offset,
                    scaling,
                    &weighted_pred,
                    tracer,
                )
            };

            // B4: inter deblock. The MBAFF orchestrator derives bS for inter
            // MBs (MV/ref-difference cases); the I-path already exists.
            let deblock_params = crate::deblock::DeblockParams {
                disable_idc: header.disable_deblocking_filter_idc as u8,
                alpha_offset_div2: header.slice_alpha_c0_offset_div2,
                beta_offset_div2: header.slice_beta_offset_div2,
                chroma_qp_index_offset,
            };
            if let Some(mcaff_infos) = Self::mbaff_deblock_infos(&parsed, true) {
                Self::run_mbaff_deblock(&mut recon, &mcaff_infos, mb_cols, mb_rows, deblock_params);
            }

            // Assemble the full interlaced frame and store it in the DPB as a
            // frame reference. Crop from coded (MB-aligned) dimensions to the
            // visible rectangle.
            let data = recon.crop_yuv420p(width, height);
            let frame = VideoFrame {
                pts: packet.pts,
                dts: packet.dts,
                data,
                width,
                height,
                pixel_format: PixelFormat::Yuv420p,
                is_key_frame: matches!(nal.nal_unit_type, NalUnitType::IdrSlice),
            };
            self.store_reference_picture(
                nal,
                sps,
                header,
                &frame,
                Some(std::sync::Arc::new(parsed.mv_store.to_grid_vec())),
            );

            return Ok(InterlacedOutcome::Frame(frame));
        }

        let scaling = pps.map(|p| &p.scaling).unwrap_or(&sps.scaling);

        let mut reader = crate::bitreader::BitReader::new(&nal.rbsp);
        reader.seek_to_bit(header.data_bit_offset);
        let parsed = if entropy_coding_mode_flag {
            reader.byte_align();
            crate::slice_data::parse_i_slice_cabac(
                reader.remaining_bytes(),
                mb_cols,
                mb_rows,
                slice_qp,
                sps.mb_adaptive_frame_field_flag,
                header.field_pic_flag,
                pps.map(|p| p.transform_8x8_mode_flag).unwrap_or(false),
                tracer,
            )
        } else {
            crate::slice_data::parse_i_slice(
                &mut reader,
                mb_cols,
                mb_rows,
                slice_qp,
                chroma_qp_index_offset,
                pps.map(|p| p.transform_8x8_mode_flag).unwrap_or(false),
                sps.mb_adaptive_frame_field_flag,
                header.field_pic_flag,
                tracer,
            )
        };
        let parsed = match parsed {
            Ok(p) => p,
            Err(e) => {
                // Surface WHY the MBAFF/PAFF parse failed instead of silently
                // falling back (session #32c: the silent swallow hid a
                // desync-class failure for days).
                eprintln!("interlaced slice parse failed: {e}");
                return Ok(InterlacedOutcome::Fallback);
            }
        };

        let mut recon = crate::reconstruct::reconstruct_mbaff_intra_frame(
            &parsed.macroblocks,
            mb_cols,
            mb_rows,
            coded_width,
            coded_height,
            chroma_qp_index_offset,
            scaling,
            &crate::reconstruct::WeightedPred::Default,
            tracer,
        );

        // MBAFF in-loop deblocking (default-on for MBAFF frame pictures; the
        // field-MC path remains separately gated). Frame-convention behaviour is
        // unchanged for progressive / PAFF pictures.
        let deblock_params = crate::deblock::DeblockParams {
            disable_idc: header.disable_deblocking_filter_idc as u8,
            alpha_offset_div2: header.slice_alpha_c0_offset_div2,
            beta_offset_div2: header.slice_beta_offset_div2,
            chroma_qp_index_offset,
        };
        if let Some(mcaff_infos) = Self::mbaff_deblock_infos(&parsed, true) {
            Self::run_mbaff_deblock(&mut recon, &mcaff_infos, mb_cols, mb_rows, deblock_params);
        }

        // Assemble the full interlaced frame and store it in the DPB as a frame
        // reference. MBAFF frames are coded as frame pictures (field_pic_flag is
        // false), so no field interleaving accumulator is required. Crop from coded
        // (MB-aligned) dimensions to the visible rectangle.
        let data = recon.crop_yuv420p(width, height);
        let frame = VideoFrame {
            pts: packet.pts,
            dts: packet.dts,
            data,
            width,
            height,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: matches!(nal.nal_unit_type, NalUnitType::IdrSlice),
        };
        self.store_reference_picture(nal, sps, header, &frame, None);

        Ok(InterlacedOutcome::Frame(frame))
    }

    /// Build a half-height grey skip field and pair it with its complementary
    /// field in the field accumulator.
    ///
    /// When the field reference list cannot be built (e.g. empty DPB), the field
    /// cannot be properly decoded. Instead of falling through to the progressive
    /// scaffold path (which misparses field slices as frame slices and bypasses
    /// `field_accum`), emit a grey skip field here so the field-pairing logic
    /// stays consistent and the complementary field can still be interleaved
    /// into a full output frame.
    fn emit_skip_field(
        &mut self,
        coded_width: u32,
        field_height: u32,
        packet: &Packet,
        nal: &NalUnit,
        sps: &SeqParameterSet,
        header: &crate::slice::SliceHeader,
    ) -> Result<InterlacedOutcome, KinetixError> {
        let w = coded_width as usize;
        let h = field_height as usize;
        let luma_size = w * h;
        let chroma_size = (w / 2) * (h / 2);

        let field_frame = VideoFrame {
            pts: packet.pts,
            dts: packet.dts,
            data: vec![128u8; luma_size + 2 * chroma_size],
            width: coded_width,
            height: field_height,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: matches!(nal.nal_unit_type, NalUnitType::IdrSlice),
        };

        let visible_width = sps.pic_width_pixels();
        let visible_height = sps.pic_height_pixels();
        match self.accumulate_field(field_frame, header.bottom_field_flag, header.frame_num) {
            Some(full) => Ok(InterlacedOutcome::Frame(VideoFrame {
                data: crate::reconstruct::crop_yuv420p(
                    &full.data,
                    full.width,
                    full.height,
                    visible_width,
                    visible_height,
                ),
                width: visible_width,
                height: visible_height,
                ..full
            })),
            None => Ok(InterlacedOutcome::Handled),
        }
    }

    /// PAFF P-field picture decode: build the field reference list (§8.2.4.2.5),
    /// parse the field P-slice, motion-compensate each field macroblock at field
    /// parity into a half-height buffer, deblock, and interleave with its
    /// complementary field for output.
    #[allow(clippy::too_many_arguments)]
    fn decode_interlaced_p_field<T: DecodeTracer>(
        &mut self,
        nal: &NalUnit,
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
            self.dpb(),
            header.bottom_field_flag,
            num_ref_idx_l0_active as usize,
            pic_num_ctx,
        ) {
            Some(l) => l,
            None => {
                return self.emit_skip_field(
                    sps.coded_width_pixels(),
                    field_height,
                    packet,
                    nal,
                    sps,
                    header,
                );
            }
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
                sps.mb_adaptive_frame_field_flag,
                header.field_pic_flag,
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
                sps.mb_adaptive_frame_field_flag,
                header.field_pic_flag,
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

    /// PAFF B-field picture decode: build both field reference lists
    /// (§8.2.4.2.5), parse the field B-slice, motion-compensate each field
    /// macroblock with bi-prediction into a half-height buffer, deblock, and
    /// interleave with its complementary field for output.
    #[allow(clippy::too_many_arguments)]
    fn decode_interlaced_b_field<T: DecodeTracer>(
        &mut self,
        nal: &NalUnit,
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
        use crate::ref_pic::{build_field_ref_list_l0, build_field_ref_list_l1, PicNumContext};

        let num_ref_idx_l0_active = header.num_ref_idx_l0_active_minus1 + 1;
        let num_ref_idx_l1_active = header.num_ref_idx_l1_active_minus1 + 1;
        let pic_num_ctx = PicNumContext::new(sps, header.frame_num, true, header.bottom_field_flag);

        // Field pictures use the same field-ordering rule for both reference
        // lists (§8.2.4.2.5); build L0 and L1 independently.
        let ref_l0 = match build_field_ref_list_l0(
            self.dpb(),
            header.bottom_field_flag,
            num_ref_idx_l0_active as usize,
            pic_num_ctx,
        ) {
            Some(l) => l,
            None => {
                return self.emit_skip_field(
                    sps.coded_width_pixels(),
                    field_height,
                    packet,
                    nal,
                    sps,
                    header,
                );
            }
        };
        let ref_l1 = match build_field_ref_list_l1(
            self.dpb(),
            header.bottom_field_flag,
            num_ref_idx_l1_active as usize,
            pic_num_ctx,
        ) {
            Some(l) => l,
            None => {
                return self.emit_skip_field(
                    sps.coded_width_pixels(),
                    field_height,
                    packet,
                    nal,
                    sps,
                    header,
                );
            }
        };

        // Current field's POC (§8.2.1) — needed for implicit bi-prediction
        // weights (weighted_bipred_idc == 2). Derived against a scratch state so
        // we don't advance the real poc_state (store_reference_picture handles
        // that later).
        let is_idr = matches!(nal.nal_unit_type, NalUnitType::IdrSlice);
        let current_poc = {
            let mut scratch = self.poc_state.clone();
            crate::ref_pic::derive_pic_order_cnt(
                sps,
                is_idr,
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

        // Weighted bi-prediction (§8.4.2.3.2): explicit when
        // `weighted_bipred_idc == 1` (parsed `pred_weight_table` carries both
        // l0 and l1), implicit when `== 2` (weights derived per-block from
        // each reference field's POC distance to the current field), default
        // otherwise.
        let weighted_bipred_idc = pps.map(|p| p.weighted_bipred_idc).unwrap_or(0);
        let weighted_pred = match (weighted_bipred_idc, &header.pred_weight_table) {
            (1, Some(pwt)) => crate::reconstruct::WeightedPred::Explicit {
                luma_log2_wd: pwt.luma_log2_weight_denom,
                chroma_log2_wd: pwt.chroma_log2_weight_denom,
                l0: pwt.l0.clone(),
                l1: pwt.l1.clone(),
            },
            (2, _) => crate::reconstruct::WeightedPred::Implicit {
                l0_poc: ref_l0.iter().map(|f| f.pic_order_cnt).collect(),
                l1_poc: ref_l1.iter().map(|f| f.pic_order_cnt).collect(),
                cur_poc: current_poc,
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
            crate::slice_data::parse_b_slice_cabac(
                reader.remaining_bytes(),
                mb_cols,
                mb_rows_field,
                slice_qp,
                sps.mb_adaptive_frame_field_flag,
                header.field_pic_flag,
                header.cabac_init_idc as usize,
                num_ref_idx_l0_active,
                num_ref_idx_l1_active,
                chroma_qp_index_offset,
                transform_8x8,
                None,
                tracer,
            )
        } else {
            crate::slice_data::parse_b_slice(
                &mut reader,
                mb_cols,
                mb_rows_field,
                slice_qp,
                num_ref_idx_l0_active,
                num_ref_idx_l1_active,
                chroma_qp_index_offset,
                transform_8x8,
                None,
                tracer,
            )
        };
        let parsed = match parsed {
            Ok(p) => p,
            Err(_) => return Ok(InterlacedOutcome::Fallback),
        };

        let mut recon = crate::reconstruct::reconstruct_inter_b_field_frame(
            &parsed.macroblocks,
            &parsed.mv_store,
            &ref_l0,
            &ref_l1,
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
                        crate::deblock::DeblockMbInfo {
                            transform_8x8: mb.transform_size_8x8,
                            ..crate::deblock::DeblockMbInfo::new(mb.mb_type, nz, cells, mb.qp)
                        }
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
        nal: &NalUnit,
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
        self.store_reference_picture(nal, sps, header, &field_frame, None);

        // Buffer the field and emit the interleaved frame once the pair is complete.
        let visible_width = sps.pic_width_pixels();
        let visible_height = sps.pic_height_pixels();
        match self.accumulate_field(field_frame, header.bottom_field_flag, header.frame_num) {
            Some(full) => Ok(InterlacedOutcome::Frame(VideoFrame {
                data: crate::reconstruct::crop_yuv420p(
                    &full.data,
                    full.width,
                    full.height,
                    visible_width,
                    visible_height,
                ),
                width: visible_width,
                height: visible_height,
                ..full
            })),
            None => Ok(InterlacedOutcome::Handled),
        }
    }

    /// Buffer one reconstructed field and, when its complementary field has also
    /// been decoded, interleave the two half-height fields into a full (progressive)
    /// frame suitable for output.
    ///
    /// On a key change (a new frame's first field arrived before the previous
    /// pair completed), the unpaired field from the previous frame is paired with
    /// a grey field so it is still emitted as a full-height frame rather than
    /// being silently dropped.
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
            // before the previous pair completed): pair the unpaired field with
            // a grey field so it is still emitted as a full-height frame.
            Some(accum) => {
                let mut discarded = None;
                if let Some(top) = &accum.top {
                    let grey_bottom = Self::grey_field(top);
                    discarded = Some(Self::interleave_fields(top, &grey_bottom));
                } else if let Some(bottom) = &accum.bottom {
                    let grey_top = Self::grey_field(bottom);
                    discarded = Some(Self::interleave_fields(&grey_top, bottom));
                }
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
                discarded
            }
            None => {
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

    /// Create a grey (flat 128) field frame matching the dimensions of
    /// `template`. Used to pair with an unpaired field on a key change so the
    /// frame is still emitted at full height.
    pub(crate) fn grey_field(template: &VideoFrame) -> VideoFrame {
        let w = template.width as usize;
        let h = template.height as usize;
        let luma_size = w * h;
        let chroma_size = (w / 2) * (h / 2);
        VideoFrame {
            pts: template.pts,
            dts: template.dts,
            data: vec![128u8; luma_size + 2 * chroma_size],
            width: template.width,
            height: template.height,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: template.is_key_frame,
        }
    }

    /// Interleave two complementary half-height fields into a full (progressive)
    /// frame: the top field's rows occupy the even scanlines, the bottom field's
    /// rows the odd scanlines (§6.4.10.1 / §8.4.2.2.1).
    pub(crate) fn interleave_fields(top: &VideoFrame, bottom: &VideoFrame) -> VideoFrame {
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
                data[ro..ro + chroma_w]
                    .copy_from_slice(&src_bottom[y * chroma_w..(y + 1) * chroma_w]);
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
    use super::*;

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
