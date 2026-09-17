//! VP9 frame header parsing: the uncompressed header (§6.2), the compressed
//! header (§6.3, bool-coded probability updates), and the derived per-segment
//! quantizer / loop-filter-level tables.

use tpt_kinetix_core::error::KinetixError;

use crate::bitreader::BitReader;
use crate::booldec::BoolDecoder;
use crate::tables::{
    BWH_TAB, DEFAULT_COEF_PROBS, DEFAULT_KF_PARTITION_PROBS, DEFAULT_KF_UVMODE_PROBS,
    DEFAULT_KF_YMODE_PROBS, INV_MAP_TABLE,
};

pub const REFS_PER_FRAME: usize = 3;
pub const MAX_SEGMENTS: usize = 8;
pub const SEGMENT_DELTAS: usize = MAX_SEGMENTS;

/// `frame_marker` value every frame starts with.
const FRAME_MARKER: u32 = 0b10;
/// 24-bit sync code for key frames and intra-only frames.
const SYNC_CODE: u32 = 0x49_83_42;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Key,
    Inter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TxfmMode {
    Only4x4 = 0,
    Allow8x8 = 1,
    Allow16x16 = 2,
    Allow32x32 = 3,
    Switchable = 4,
}

#[derive(Debug, Clone, Default)]
pub struct LoopFilterHeader {
    pub level: u8,
    pub sharpness: u8,
    pub delta_enabled: bool,
    pub delta_updated: bool,
    pub ref_deltas: [i32; 4],
    pub mode_deltas: [i32; 2],
}

#[derive(Debug, Clone, Default)]
pub struct SegmentationHeader {
    pub enabled: bool,
    pub update_map: bool,
    pub temporal_update: bool,
    pub update_data: bool,
    pub absolute_vals: bool,
    pub tree_probs: [u8; 7],
    pub pred_probs: [u8; 3],
    pub feat_enabled: [[bool; 4]; MAX_SEGMENTS], // q, lf, ref, skip
    pub feat: [[i32; 4]; MAX_SEGMENTS],          // q_val, lf_val, ref_val, unused
}

/// Per-segment derived quantizers and loop-filter levels (spec §6.2 / §8.7),
/// precomputed once per frame exactly as the reference does.
#[derive(Debug, Clone)]
pub struct SegFeatures {
    /// `[seg][plane]` -> (dc, ac) quantizer multipliers.
    pub qmul: [[[i16; 2]; 2]; MAX_SEGMENTS],
    /// `[seg][ref + 1 (0 = intra)][mode != ZEROMV]` loop filter level.
    pub lflvl: [[[u8; 2]; 4]; MAX_SEGMENTS],
}

#[derive(Debug, Clone)]
pub struct TileInfo {
    pub log2_tile_cols: u32,
    pub log2_tile_rows: u32,
}

impl TileInfo {
    pub fn tile_cols(&self) -> usize {
        1 << self.log2_tile_cols
    }
    pub fn tile_rows(&self) -> usize {
        1 << self.log2_tile_rows
    }
}

/// Parsed uncompressed + compressed header for one frame.
#[derive(Debug, Clone)]
pub struct FrameHeader {
    pub profile: u8,
    pub show_existing_frame: bool,
    pub frame_to_show_map_idx: usize,
    pub frame_type: FrameType,
    pub show_frame: bool,
    pub error_resilient: bool,
    pub intra_only: bool,
    pub reset_frame_context: usize,
    pub refresh_frame_flags: u8,
    pub ref_frame_idx: [usize; REFS_PER_FRAME],
    pub sign_bias: [bool; REFS_PER_FRAME],
    pub use_last_frame_mvs: bool,
    pub allow_high_precision_mv: bool,
    /// 0..=2 fixed filter, 3 = switchable.
    pub filter_mode: u8,
    /// Compound prediction mode for inter frames (`PRED_*`).
    pub comp_pred_mode: u8,
    pub refresh_frame_context: bool,
    pub frame_parallel_decoding_mode: bool,
    pub frame_context_idx: usize,

    pub width: u32,
    pub height: u32,
    pub render_width: u32,
    pub render_height: u32,

    /// Macroblock (8px) columns/rows, rounding the frame size up.
    pub mi_cols: usize,
    pub mi_rows: usize,
    pub sb64_cols: usize,
    pub sb64_rows: usize,
    pub subsampling_x: u32,
    pub subsampling_y: u32,

    pub base_q_idx: u8,
    pub delta_q_y_dc: i32,
    pub delta_q_uv_dc: i32,
    pub delta_q_uv_ac: i32,
    pub lossless: bool,

    pub loop_filter: LoopFilterHeader,
    pub segmentation: SegmentationHeader,
    pub tile: TileInfo,

    pub txfm_mode: TxfmMode,

    /// Bytes of the bool-coded compressed header.
    pub compressed_header_size: usize,
    /// Byte offset (from the start of the frame) where the compressed header
    /// begins — the uncompressed header is byte-aligned here.
    pub compressed_header_offset: usize,
}

impl Default for FrameHeader {
    fn default() -> Self {
        Self {
            profile: 0,
            show_existing_frame: false,
            frame_to_show_map_idx: 0,
            frame_type: FrameType::Key,
            show_frame: true,
            error_resilient: false,
            intra_only: false,
            reset_frame_context: 0,
            refresh_frame_flags: 0xff,
            ref_frame_idx: [0; REFS_PER_FRAME],
            sign_bias: [false; REFS_PER_FRAME],
            use_last_frame_mvs: false,
            allow_high_precision_mv: false,
            filter_mode: 3,
            comp_pred_mode: 0,
            refresh_frame_context: false,
            frame_parallel_decoding_mode: false,
            frame_context_idx: 0,
            width: 0,
            height: 0,
            render_width: 0,
            render_height: 0,
            mi_cols: 0,
            mi_rows: 0,
            sb64_cols: 0,
            sb64_rows: 0,
            subsampling_x: 1,
            subsampling_y: 1,
            base_q_idx: 0,
            delta_q_y_dc: 0,
            delta_q_uv_dc: 0,
            delta_q_uv_ac: 0,
            lossless: false,
            loop_filter: LoopFilterHeader::default(),
            segmentation: SegmentationHeader::default(),
            tile: TileInfo {
                log2_tile_cols: 0,
                log2_tile_rows: 0,
            },
            txfm_mode: TxfmMode::Allow32x32,
            compressed_header_size: 0,
            compressed_header_offset: 0,
        }
    }
}

/// Magitude-then-sign field used by several uncompressed header sections
/// (`get_sbits_inv` in the reference decoder).
fn get_sbits_inv(br: &mut BitReader, n: u32, what: &str) -> Result<i32, KinetixError> {
    let v = br.try_f(n, what)?;
    let sign = br.try_f(1, what)?;
    Ok(if sign == 1 { -(v as i32) } else { v as i32 })
}

/// Parse the uncompressed header. `ref_dims` carries the (w, h) of the eight
/// reference slots, or `None` where a slot is empty, for
/// `frame_size_with_refs` and validity checks.
pub fn parse_uncompressed_header(
    data: &[u8],
    ref_dims: &[Option<(u32, u32)>; 8],
) -> Result<FrameHeader, KinetixError> {
    let mut h = FrameHeader::default();
    let mut br = BitReader::new(data);

    if br.try_f(2, "frame_marker")? != FRAME_MARKER {
        return Err(KinetixError::Parse("vp9: invalid frame marker".into()));
    }
    let profile_low = br.try_f(1, "profile_low_bit")?;
    let profile_high = br.try_f(1, "profile_high_bit")?;
    let mut profile = (profile_high << 1) | profile_low;
    if profile == 3 {
        let reserved = br.try_f(1, "profile_reserved")?;
        profile += reserved;
    }
    h.profile = profile as u8;
    if profile != 0 {
        return Err(KinetixError::Unsupported(format!(
            "vp9: only profile 0 (8-bit 4:2:0) is supported, stream is profile {profile}"
        )));
    }

    h.show_existing_frame = br.try_f(1, "show_existing_frame")? == 1;
    if h.show_existing_frame {
        h.frame_to_show_map_idx = br.try_f(3, "frame_to_show_map_idx")? as usize;
        h.compressed_header_offset = 0;
        h.compressed_header_size = 0;
        return Ok(h);
    }

    h.frame_type = if br.try_f(1, "frame_type")? == 0 {
        FrameType::Key
    } else {
        FrameType::Inter
    };
    h.show_frame = br.try_f(1, "show_frame")? == 1;
    h.error_resilient = br.try_f(1, "error_resilient_mode")? == 1;
    h.use_last_frame_mvs = !h.error_resilient;

    let keyframe = h.frame_type == FrameType::Key;
    if keyframe {
        h.refresh_frame_flags = if h.show_frame {
            0xff
        } else {
            br.try_f(8, "refresh_frame_flags")? as u8
        };
        if br.try_f(24, "sync_code")? != SYNC_CODE {
            return Err(KinetixError::Parse("vp9: invalid sync code".into()));
        }
        // Profile 0 colour config: color_space f(3) + color_range f(1).
        read_color_config(&mut br, &mut h)?;
        h.width = br.try_f(16, "frame_width_minus_1")? + 1;
        h.height = br.try_f(16, "frame_height_minus_1")? + 1;
        read_render_size(&mut br, &mut h)?;
        h.intra_only = false;
    } else {
        let last_invisible = !h.show_frame; // placeholder, replaced below
        let _ = last_invisible;
        h.intra_only = if !h.show_frame {
            br.try_f(1, "intra_only")? == 1
        } else {
            false
        };
        h.reset_frame_context = if h.error_resilient {
            0
        } else {
            br.try_f(2, "reset_frame_context")? as usize
        };
        if h.intra_only {
            if br.try_f(24, "sync_code")? != SYNC_CODE {
                return Err(KinetixError::Parse("vp9: invalid sync code".into()));
            }
            // Profile 0: colour config identical to keyframes.
            read_color_config(&mut br, &mut h)?;
            h.refresh_frame_flags = br.try_f(8, "refresh_frame_flags")? as u8;
            h.width = br.try_f(16, "frame_width_minus_1")? + 1;
            h.height = br.try_f(16, "frame_height_minus_1")? + 1;
            read_render_size(&mut br, &mut h)?;
        } else {
            h.refresh_frame_flags = br.try_f(8, "refresh_frame_flags")? as u8;
            for i in 0..REFS_PER_FRAME {
                h.ref_frame_idx[i] = br.try_f(3, "ref_frame_idx")? as usize;
                h.sign_bias[i] = br.try_f(1, "sign_bias")? == 1 && !h.error_resilient;
                if ref_dims[h.ref_frame_idx[i]].is_none() {
                    return Err(KinetixError::Parse(format!(
                        "vp9: reference frame {} is not available",
                        h.ref_frame_idx[i]
                    )));
                }
            }
            // frame_size_with_refs
            let mut found = false;
            for i in 0..REFS_PER_FRAME {
                if br.try_f(1, "found_ref")? == 1 {
                    let (w, hh) = ref_dims[h.ref_frame_idx[i]].expect("checked above");
                    h.width = w;
                    h.height = hh;
                    found = true;
                    break;
                }
            }
            if !found {
                h.width = br.try_f(16, "frame_width_minus_1")? + 1;
                h.height = br.try_f(16, "frame_height_minus_1")? + 1;
            }
            read_render_size(&mut br, &mut h)?;
            h.allow_high_precision_mv = br.try_f(1, "allow_high_precision_mv")? == 1;
            h.filter_mode = if br.try_f(1, "is_filter_switchable")? == 1 {
                3 // switchable
            } else {
                br.try_f(2, "raw_interpolation_filter")? as u8
            };
        }
    }

    if h.frame_type == FrameType::Key {
        // use_last_frame_mvs requires a same-sized previous frame; computed
        // by the decoder (needs the previous frame's dims), initialised false.
        h.use_last_frame_mvs = false;
    }

    h.refresh_frame_context = if !h.error_resilient {
        br.try_f(1, "refresh_frame_context")? == 1
    } else {
        false
    };
    h.frame_parallel_decoding_mode = if !h.error_resilient {
        br.try_f(1, "frame_parallel_decoding_mode")? == 1
    } else {
        true
    };
    h.frame_context_idx = br.try_f(2, "frame_context_idx")? as usize;

    // Loop filter header. On key/error-resilient/intra-only frames the ref/mode
    // deltas reset to the spec defaults.
    if keyframe || h.error_resilient || h.intra_only {
        h.loop_filter.ref_deltas = [1, 0, -1, -1];
        h.loop_filter.mode_deltas = [0, 0];
        h.segmentation = SegmentationHeader::default();
    }
    h.loop_filter.level = br.try_f(6, "loop_filter_level")? as u8;
    h.loop_filter.sharpness = br.try_f(3, "loop_filter_sharpness")? as u8;
    h.loop_filter.delta_enabled = br.try_f(1, "loop_filter_delta_enabled")? == 1;
    if h.loop_filter.delta_enabled {
        h.loop_filter.delta_updated = br.try_f(1, "loop_filter_delta_update")? == 1;
        if h.loop_filter.delta_updated {
            for i in 0..4 {
                if br.try_f(1, "update_ref_delta")? == 1 {
                    h.loop_filter.ref_deltas[i] = get_sbits_inv(&mut br, 6, "ref_delta")?;
                }
            }
            for i in 0..2 {
                if br.try_f(1, "update_mode_delta")? == 1 {
                    h.loop_filter.mode_deltas[i] = get_sbits_inv(&mut br, 6, "mode_delta")?;
                }
            }
        }
    }

    // Quantization header.
    h.base_q_idx = br.try_f(8, "base_q_idx")? as u8;
    h.delta_q_y_dc = if br.try_f(1, "delta_coded")? == 1 {
        get_sbits_inv(&mut br, 4, "delta_q_y_dc")?
    } else {
        0
    };
    h.delta_q_uv_dc = if br.try_f(1, "delta_coded")? == 1 {
        get_sbits_inv(&mut br, 4, "delta_q_uv_dc")?
    } else {
        0
    };
    h.delta_q_uv_ac = if br.try_f(1, "delta_coded")? == 1 {
        get_sbits_inv(&mut br, 4, "delta_q_uv_ac")?
    } else {
        0
    };
    h.lossless =
        h.base_q_idx == 0 && h.delta_q_y_dc == 0 && h.delta_q_uv_dc == 0 && h.delta_q_uv_ac == 0;

    // Segmentation header.
    if br.try_f(1, "segmentation_enabled")? == 1 {
        h.segmentation.enabled = true;
        if br.try_f(1, "segmentation_update_map")? == 1 {
            h.segmentation.update_map = true;
            for i in 0..7 {
                h.segmentation.tree_probs[i] = if br.try_f(1, "prob_coded")? == 1 {
                    br.try_f(8, "segment_tree_prob")? as u8
                } else {
                    255
                };
            }
            if br.try_f(1, "segmentation_temporal_update")? == 1 {
                h.segmentation.temporal_update = true;
                for i in 0..3 {
                    h.segmentation.pred_probs[i] = if br.try_f(1, "prob_coded")? == 1 {
                        br.try_f(8, "segment_pred_prob")? as u8
                    } else {
                        255
                    };
                }
            }
        }
        if br.try_f(1, "segmentation_update_data")? == 1 {
            h.segmentation.update_data = true;
            h.segmentation.absolute_vals = br.try_f(1, "segmentation_abs_or_delta")? == 1;
            for i in 0..MAX_SEGMENTS {
                if br.try_f(1, "feature_enabled")? == 1 {
                    h.segmentation.feat_enabled[i][0] = true;
                    h.segmentation.feat[i][0] = get_sbits_inv(&mut br, 8, "q_val")?;
                }
                if br.try_f(1, "feature_enabled")? == 1 {
                    h.segmentation.feat_enabled[i][1] = true;
                    h.segmentation.feat[i][1] = get_sbits_inv(&mut br, 6, "lf_val")?;
                }
                if br.try_f(1, "feature_enabled")? == 1 {
                    h.segmentation.feat_enabled[i][2] = true;
                    h.segmentation.feat[i][2] = br.try_f(2, "ref_val")? as i32;
                }
                h.segmentation.feat_enabled[i][3] = br.try_f(1, "skip_enabled")? == 1;
            }
        }
    }

    // Tile info.
    h.mi_cols = (h.width as usize).div_ceil(8);
    h.mi_rows = (h.height as usize).div_ceil(8);
    h.sb64_cols = h.mi_cols.div_ceil(8);
    h.sb64_rows = h.mi_rows.div_ceil(8);
    let mut log2_tile_cols: u32 = 0;
    while h.sb64_cols > (64usize << log2_tile_cols) {
        log2_tile_cols += 1;
    }
    let mut max: u32 = 0;
    while (h.sb64_cols >> max) >= 4 {
        max += 1;
    }
    let max = max.saturating_sub(1);
    while max > log2_tile_cols {
        if br.try_f(1, "increment_tile_cols_log2")? == 1 {
            log2_tile_cols += 1;
        } else {
            break;
        }
    }
    let mut log2_tile_rows: u32 = 0;
    if br.try_f(1, "tile_rows_log2")? == 1 {
        log2_tile_rows += br.try_f(1, "tile_rows_log2")?;
    }
    h.tile = TileInfo {
        log2_tile_cols,
        log2_tile_rows,
    };

    h.compressed_header_size = br.try_f(16, "header_size_in_bytes")? as usize;
    h.compressed_header_offset = br.aligned_byte_pos();
    if h.compressed_header_offset + h.compressed_header_size > data.len() {
        return Err(KinetixError::Parse(
            "vp9: compressed header extends past the frame".into(),
        ));
    }
    Ok(h)
}

/// Profile 0 colour config:  then .
fn read_color_config(br: &mut BitReader, h: &mut FrameHeader) -> Result<(), KinetixError> {
    let color_space = br.try_f(3, "color_space")?;
    if color_space == 7 {
        return Err(KinetixError::Unsupported(
            "vp9: RGB (4:4:4) color space requires profile 1, which is unsupported".into(),
        ));
    }
    let _color_range = br.try_f(1, "color_range")?;
    h.subsampling_x = 1;
    h.subsampling_y = 1;
    Ok(())
}

fn read_render_size(br: &mut BitReader, h: &mut FrameHeader) -> Result<(), KinetixError> {
    if br.try_f(1, "render_and_frame_size_different")? == 1 {
        h.render_width = br.try_f(16, "render_width_minus_1")? + 1;
        h.render_height = br.try_f(16, "render_height_minus_1")? + 1;
    } else {
        h.render_width = h.width;
        h.render_height = h.height;
    }
    Ok(())
}

/// Mode/probability context (mirrors the reference `ProbContext`).
#[derive(Debug, Clone)]
pub struct ProbsCtx {
    pub y_mode: [[u8; 9]; 4],
    pub uv_mode: [[u8; 9]; 10],
    pub filter: [[u8; 2]; 4],
    pub mv_mode: [[u8; 3]; 7],
    pub intra: [u8; 4],
    pub comp: [u8; 5],
    pub single_ref: [[u8; 2]; 5],
    pub comp_ref: [u8; 5],
    pub tx32p: [[u8; 3]; 2],
    pub tx16p: [[u8; 2]; 2],
    pub tx8p: [u8; 2],
    pub skip: [u8; 3],
    pub mv_joint: [u8; 3],
    pub mv_comp: [MvCompProbs; 2],
    pub partition: [[[u8; 3]; 4]; 4],
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MvCompProbs {
    pub sign: u8,
    pub classes: [u8; 10],
    pub class0: u8,
    pub bits: [u8; 10],
    pub class0_fp: [[u8; 3]; 2],
    pub fp: [u8; 3],
    pub class0_hp: u8,
    pub hp: u8,
}

/// Spec default probability context (§13.3/13.4), transcribed from the pinned
/// reference source (`ff_vp9_default_probs`, commit
/// c3ff71680805267bc8f3fff86c1cf917f810c0d9, libavcodec/vp9data.c — a struct,
/// so not machine-extractable via `verify-tables`).
pub const DEFAULT_PROBS: ProbsCtx = ProbsCtx {
    y_mode: [
        [65, 32, 18, 144, 162, 194, 41, 51, 98],
        [132, 68, 18, 165, 217, 196, 45, 40, 78],
        [173, 80, 19, 176, 240, 193, 64, 35, 46],
        [221, 135, 38, 194, 248, 121, 96, 85, 29],
    ],
    // NB: rows permuted from FFmpeg's enum order (its comments "y = v, y = h,
    // y = dc") into spec mode order (dc, v, h) — see SPEC_TO_FFMPEG_MODE.
    uv_mode: [
        [120, 7, 76, 176, 208, 126, 28, 54, 103],
        [48, 12, 154, 155, 139, 90, 34, 117, 119],
        [67, 6, 25, 204, 243, 158, 13, 21, 96],
        [97, 5, 44, 131, 176, 139, 48, 68, 97],
        [83, 5, 42, 156, 111, 152, 26, 49, 152],
        [80, 5, 58, 178, 74, 83, 33, 62, 145],
        [86, 5, 32, 154, 192, 168, 14, 22, 163],
        [77, 7, 64, 116, 132, 122, 37, 126, 120],
        [85, 5, 32, 156, 216, 148, 19, 29, 73],
        [101, 21, 107, 181, 192, 103, 19, 67, 125],
    ],
    filter: [[235, 162], [36, 255], [34, 3], [149, 144]],
    mv_mode: [
        [2, 173, 34],
        [7, 145, 85],
        [7, 166, 63],
        [7, 94, 66],
        [8, 64, 46],
        [17, 81, 31],
        [25, 29, 30],
    ],
    intra: [9, 102, 187, 225],
    comp: [239, 183, 119, 96, 41],
    single_ref: [[33, 16], [77, 74], [142, 142], [172, 170], [238, 247]],
    comp_ref: [50, 126, 123, 221, 226],
    tx32p: [[3, 136, 37], [5, 52, 13]],
    tx16p: [[20, 152], [15, 101]],
    tx8p: [100, 66],
    skip: [192, 128, 64],
    mv_joint: [32, 64, 96],
    mv_comp: [
        MvCompProbs {
            sign: 128,
            classes: [224, 144, 192, 168, 192, 176, 192, 198, 198, 245],
            class0: 216,
            bits: [136, 140, 148, 160, 176, 192, 224, 234, 234, 240],
            class0_fp: [[128, 128, 64], [96, 112, 64]],
            fp: [64, 96, 64],
            class0_hp: 160,
            hp: 128,
        },
        MvCompProbs {
            sign: 128,
            classes: [216, 128, 176, 160, 176, 176, 192, 198, 198, 208],
            class0: 208,
            bits: [136, 140, 148, 160, 176, 192, 224, 234, 234, 240],
            class0_fp: [[128, 128, 64], [96, 112, 64]],
            fp: [64, 96, 64],
            class0_hp: 160,
            hp: 128,
        },
    ],
    partition: [
        [[222, 34, 30], [72, 16, 44], [58, 32, 12], [10, 7, 6]],
        [[177, 58, 59], [68, 26, 63], [52, 79, 25], [17, 14, 12]],
        [[174, 73, 87], [92, 41, 83], [82, 99, 50], [53, 39, 39]],
        [
            [199, 122, 141],
            [147, 63, 159],
            [148, 133, 118],
            [121, 104, 114],
        ],
    ],
};

/// Flat index into the compact coefficient-probability model layout:
/// per (tx group, block type, plane type), band 0 stores 3 contexts and bands
/// 1..5 store 6 — exactly the layout of the extracted
/// [`DEFAULT_COEF_PROBS`] table (band-0 contexts 3..5 are never coded).
#[inline]
pub fn coef_model_idx(tx: usize, bt: usize, pt: usize, band: usize, ctx: usize) -> usize {
    debug_assert!(!(band == 0 && ctx >= 3), "band 0 has only 3 contexts");
    let base = (tx * 2 * 2 + bt * 2 + pt) * 99;
    let off = if band == 0 {
        ctx * 3
    } else {
        9 + (band - 1) * 18 + ctx * 3
    };
    base + off
}

/// Flat index into the expanded 11-node full-probability tree, laid out as
/// `[tx][bt][pt][band][ctx][node]` (band 0 carries all 6 contexts here; the
/// unused ones are never read).
#[inline]
pub fn coef_full_idx(tx: usize, bt: usize, pt: usize, band: usize, ctx: usize) -> usize {
    let base = (tx * 2 * 2 + bt * 2 + pt) * (6 * 6 * 11);
    base + (band * 6 + ctx) * 11
}

pub const COEF_FULL_LEN: usize = 4 * 2 * 2 * 6 * 6 * 11;
pub const COEF_MODEL_LEN: usize = 4 * 2 * 2 * 99;

/// Working probability set for the frame being decoded: the mode context and
/// both coefficient representations (compact model + expanded tree).
#[derive(Debug, Clone)]
pub struct WorkingProbs {
    pub mode: ProbsCtx,
    pub coef_model: Box<[u8; COEF_MODEL_LEN]>,
    pub coef_full: Box<[u8; COEF_FULL_LEN]>,
}

impl WorkingProbs {
    pub fn new(mode: ProbsCtx, coef_model: Box<[u8; COEF_MODEL_LEN]>) -> Self {
        let mut w = Self {
            mode,
            coef_model,
            coef_full: Box::new([0; COEF_FULL_LEN]),
        };
        w.expand_coef_model();
        w
    }

    /// Expand the 3-prob model into the full 11-node tree by appending the
    /// Pareto-8 table row selected by the 'ONE vs higher' probability, exactly
    /// as the reference `vp9_model_to_full_probs` does.
    pub fn expand_coef_model(&mut self) {
        for tx in 0..4 {
            for bt in 0..2 {
                for pt in 0..2 {
                    for band in 0..6 {
                        for ctx in 0..6 {
                            if band == 0 && ctx >= 3 {
                                continue;
                            }
                            let mi = coef_model_idx(tx, bt, pt, band, ctx);
                            let fi = coef_full_idx(tx, bt, pt, band, ctx);
                            let one_prob = self.coef_model[mi + 2] as usize;
                            for n in 0..3 {
                                self.coef_full[fi + n] = self.coef_model[mi + n];
                            }
                            for n in 0..8 {
                                self.coef_full[fi + 3 + n] =
                                    crate::tables::MODEL_PARETO8[one_prob * 8 + n];
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Coefficient probability context slots (the four saveable frame contexts'
/// compact models), bundled with their mode-probability contexts.
#[derive(Debug, Clone)]
pub struct FrameCtx {
    pub mode: ProbsCtx,
    pub coef_model: Box<[u8; COEF_MODEL_LEN]>,
}

impl FrameCtx {
    pub fn default_ctx() -> Self {
        let mut model = Box::new([0u8; COEF_MODEL_LEN]);
        model.copy_from_slice(&DEFAULT_COEF_PROBS[..]);
        Self {
            mode: DEFAULT_PROBS.clone(),
            coef_model: model,
        }
    }
}

/// `inv_recenter_nonneg` (§13.2).
fn inv_recenter_nonneg(v: u32, m: u32) -> u32 {
    if v > 2 * m {
        v
    } else if v & 1 != 0 {
        m - ((v + 1) >> 1)
    } else {
        m + (v >> 1)
    }
}

/// Differential probability update subexp reader (§13.2).
fn update_prob(bc: &mut BoolDecoder, p: u8) -> u8 {
    let d: u32 = if !bc.read_bool(128) {
        bc.read_literal(4)
    } else if !bc.read_bool(128) {
        bc.read_literal(4) + 16
    } else if !bc.read_bool(128) {
        bc.read_literal(5) + 32
    } else {
        let mut d = bc.read_literal(7);
        if d >= 65 {
            d = (d << 1) - 65 + bc.read_bool_u32(128);
        }
        d + 64
    };
    let val = u32::from(INV_MAP_TABLE[d as usize]);
    if p <= 128 {
        (1 + inv_recenter_nonneg(val, u32::from(p) - 1)) as u8
    } else {
        (255 - inv_recenter_nonneg(val, 255 - u32::from(p))) as u8
    }
}

/// Compound-prediction mode for inter frames (reference `PRED_*` values).
pub const PRED_SINGLEREF: u8 = 0;
pub const PRED_COMPREF: u8 = 1;
pub const PRED_SWITCHABLE: u8 = 2;

/// Parse the compressed header into `probs` (already seeded from the correct
/// frame context) and set `h.txfm_mode` / `h.comp_pred_mode`. Mirrors the
/// reference decoder's compressed-header section exactly, including the
/// band-0 "dc only has 3 contexts" break and the literal-7 MV update style.
pub fn parse_compressed_header(
    bc: &mut BoolDecoder,
    h: &mut FrameHeader,
    probs: &mut WorkingProbs,
    ctx: &FrameCtx,
) -> Result<(), KinetixError> {
    // Marker bit must be 0.
    if bc.read_bool(128) {
        return Err(KinetixError::Parse(
            "vp9: compressed header marker bit set".into(),
        ));
    }

    // Transform mode.
    if h.lossless {
        h.txfm_mode = TxfmMode::Only4x4;
    } else {
        let mut txfm = bc.read_literal(2);
        if txfm == 3 {
            txfm += u32::from(bc.read_bool(128));
        }
        h.txfm_mode = match txfm {
            0 => TxfmMode::Only4x4,
            1 => TxfmMode::Allow8x8,
            2 => TxfmMode::Allow16x16,
            3 => TxfmMode::Allow32x32,
            _ => TxfmMode::Switchable,
        };
        if h.txfm_mode == TxfmMode::Switchable {
            for i in 0..2 {
                if bc.read_bool(252) {
                    probs.mode.tx8p[i] = update_prob(bc, probs.mode.tx8p[i]);
                }
            }
            for i in 0..2 {
                for j in 0..2 {
                    if bc.read_bool(252) {
                        probs.mode.tx16p[i][j] = update_prob(bc, probs.mode.tx16p[i][j]);
                    }
                }
            }
            for i in 0..2 {
                for j in 0..3 {
                    if bc.read_bool(252) {
                        probs.mode.tx32p[i][j] = update_prob(bc, probs.mode.tx32p[i][j]);
                    }
                }
            }
        }
    }

    // Coefficient probability updates, per tx-size group (§6.3.2). The
    // reference source stores the model in a full [6][6] grid where band 0
    // has only 3 coded contexts; our compact layout skips the (never used)
    // band-0 contexts 3..5 in both branches.
    for i in 0..4 {
        if bc.read_bool(128) {
            for j in 0..2 {
                for k in 0..2 {
                    for l in 0..6 {
                        for m in 0..6 {
                            if m >= 3 && l == 0 {
                                break; // dc band has only 3 contexts
                            }
                            for n in 0..3 {
                                let idx = coef_model_idx(i, j, k, l, m);
                                probs.coef_model[idx + n] = if bc.read_bool(252) {
                                    update_prob(bc, ctx.coef_model[idx + n])
                                } else {
                                    ctx.coef_model[idx + n]
                                };
                            }
                        }
                    }
                }
            }
        } else {
            for j in 0..2 {
                for k in 0..2 {
                    for l in 0..6 {
                        for m in 0..6 {
                            if m >= 3 && l == 0 {
                                break;
                            }
                            let idx = coef_model_idx(i, j, k, l, m);
                            probs.coef_model[idx..idx + 3]
                                .copy_from_slice(&ctx.coef_model[idx..idx + 3]);
                        }
                    }
                }
            }
        }
        probs.expand_coef_model();
        if h.txfm_mode as u8 == i as u8 {
            break;
        }
    }

    // Skip flag.
    for i in 0..3 {
        if bc.read_bool(252) {
            probs.mode.skip[i] = update_prob(bc, probs.mode.skip[i]);
        }
    }

    if h.frame_type != FrameType::Key && !h.intra_only {
        // Inter mode probs.
        for i in 0..7 {
            for j in 0..3 {
                if bc.read_bool(252) {
                    probs.mode.mv_mode[i][j] = update_prob(bc, probs.mode.mv_mode[i][j]);
                }
            }
        }
        if h.filter_mode == 3 {
            for i in 0..4 {
                for j in 0..2 {
                    if bc.read_bool(252) {
                        probs.mode.filter[i][j] = update_prob(bc, probs.mode.filter[i][j]);
                    }
                }
            }
        }
        for i in 0..4 {
            if bc.read_bool(252) {
                probs.mode.intra[i] = update_prob(bc, probs.mode.intra[i]);
            }
        }

        // Compound prediction mode + comp probs.
        let allow_comp_inter = h.sign_bias[0] != h.sign_bias[1] || h.sign_bias[0] != h.sign_bias[2];
        if allow_comp_inter {
            let mut mode = bc.read_bool_u32(128);
            if mode != 0 {
                mode += bc.read_bool_u32(128);
            }
            h.comp_pred_mode = mode as u8;
            if h.comp_pred_mode == PRED_SWITCHABLE {
                for i in 0..5 {
                    if bc.read_bool(252) {
                        probs.mode.comp[i] = update_prob(bc, probs.mode.comp[i]);
                    }
                }
            }
        } else {
            h.comp_pred_mode = PRED_SINGLEREF;
        }

        if h.comp_pred_mode != PRED_COMPREF {
            for i in 0..5 {
                if bc.read_bool(252) {
                    probs.mode.single_ref[i][0] = update_prob(bc, probs.mode.single_ref[i][0]);
                }
                if bc.read_bool(252) {
                    probs.mode.single_ref[i][1] = update_prob(bc, probs.mode.single_ref[i][1]);
                }
            }
        }
        if h.comp_pred_mode != PRED_SINGLEREF {
            for i in 0..5 {
                if bc.read_bool(252) {
                    probs.mode.comp_ref[i] = update_prob(bc, probs.mode.comp_ref[i]);
                }
            }
        }

        for i in 0..4 {
            for j in 0..9 {
                if bc.read_bool(252) {
                    probs.mode.y_mode[i][j] = update_prob(bc, probs.mode.y_mode[i][j]);
                }
            }
        }
        for i in 0..4 {
            for j in 0..4 {
                for k in 0..3 {
                    if bc.read_bool(252) {
                        probs.mode.partition[3 - i][j][k] =
                            update_prob(bc, probs.mode.partition[3 - i][j][k]);
                    }
                }
            }
        }

        // MV fields use plain 7-bit literals, not the subexp model.
        for i in 0..3 {
            if bc.read_bool(252) {
                probs.mode.mv_joint[i] = (bc.read_literal(7) as u8) << 1 | 1;
            }
        }
        for i in 0..2 {
            if bc.read_bool(252) {
                probs.mode.mv_comp[i].sign = (bc.read_literal(7) as u8) << 1 | 1;
            }
            for j in 0..10 {
                if bc.read_bool(252) {
                    probs.mode.mv_comp[i].classes[j] = (bc.read_literal(7) as u8) << 1 | 1;
                }
            }
            if bc.read_bool(252) {
                probs.mode.mv_comp[i].class0 = (bc.read_literal(7) as u8) << 1 | 1;
            }
            for j in 0..10 {
                if bc.read_bool(252) {
                    probs.mode.mv_comp[i].bits[j] = (bc.read_literal(7) as u8) << 1 | 1;
                }
            }
        }
        for i in 0..2 {
            for j in 0..2 {
                for k in 0..3 {
                    if bc.read_bool(252) {
                        probs.mode.mv_comp[i].class0_fp[j][k] = (bc.read_literal(7) as u8) << 1 | 1;
                    }
                }
            }
            for j in 0..3 {
                if bc.read_bool(252) {
                    probs.mode.mv_comp[i].fp[j] = (bc.read_literal(7) as u8) << 1 | 1;
                }
            }
        }
        if h.allow_high_precision_mv {
            for i in 0..2 {
                if bc.read_bool(252) {
                    probs.mode.mv_comp[i].class0_hp = (bc.read_literal(7) as u8) << 1 | 1;
                }
                if bc.read_bool(252) {
                    probs.mode.mv_comp[i].hp = (bc.read_literal(7) as u8) << 1 | 1;
                }
            }
        }
    }

    Ok(())
}

/// Derive the per-segment quantizer multipliers and loop-filter levels for
/// the current frame (reference: end of `decode_frame_header`).
pub fn derive_segment_features(h: &FrameHeader) -> SegFeatures {
    let mut seg = SegFeatures {
        qmul: [[[0; 2]; 2]; MAX_SEGMENTS],
        lflvl: [[[0; 2]; 4]; MAX_SEGMENTS],
    };
    let n_segments = if h.segmentation.enabled {
        MAX_SEGMENTS
    } else {
        1
    };
    let sh = u32::from(h.loop_filter.level >= 32);
    for i in 0..n_segments {
        let qyac = if h.segmentation.enabled && h.segmentation.feat_enabled[i][0] {
            if h.segmentation.absolute_vals {
                h.segmentation.feat[i][0].clamp(0, 255)
            } else {
                (h.base_q_idx as i32 + h.segmentation.feat[i][0]).clamp(0, 255)
            }
        } else {
            h.base_q_idx as i32
        };
        let qydc = (qyac + h.delta_q_y_dc).clamp(0, 255);
        let quvdc = (qyac + h.delta_q_uv_dc).clamp(0, 255);
        let quvac = (qyac + h.delta_q_uv_ac).clamp(0, 255);
        let dc = &crate::tables::DC_QLOOKUP;
        let ac = &crate::tables::AC_QLOOKUP;
        seg.qmul[i][0] = [dc[qydc as usize], ac[qyac as usize]];
        seg.qmul[i][1] = [dc[quvdc as usize], ac[quvac as usize]];

        let lflvl_base = if h.segmentation.enabled && h.segmentation.feat_enabled[i][1] {
            if h.segmentation.absolute_vals {
                h.segmentation.feat[i][1].clamp(0, 63)
            } else {
                (h.loop_filter.level as i32 + h.segmentation.feat[i][1]).clamp(0, 63)
            }
        } else {
            h.loop_filter.level as i32
        };
        if h.loop_filter.delta_enabled {
            let shift = 1i32 << sh;
            let clip = |v: i32| (v).clamp(0, 63) as u8;
            seg.lflvl[i][0][0] = clip(lflvl_base + h.loop_filter.ref_deltas[0] * shift);
            seg.lflvl[i][0][1] = seg.lflvl[i][0][0];
            for j in 1..4 {
                seg.lflvl[i][j][0] = clip(
                    lflvl_base
                        + (h.loop_filter.ref_deltas[j] + h.loop_filter.mode_deltas[0]) * shift,
                );
                seg.lflvl[i][j][1] = clip(
                    lflvl_base
                        + (h.loop_filter.ref_deltas[j] + h.loop_filter.mode_deltas[1]) * shift,
                );
            }
        } else {
            for j in 0..4 {
                seg.lflvl[i][j][0] = lflvl_base as u8;
                seg.lflvl[i][j][1] = lflvl_base as u8;
            }
        }
    }
    seg
}

/// The extracted FFmpeg tables are in *FFmpeg* `IntraPredMode` order
/// `[V, H, DC, D45, D113, D157, D203, D67, ..]` (enum: VERT=0, HOR=1,
/// DC=2), while this decoder uses the spec order `[DC, V, H, D45, ...]`.
/// Map spec mode value -> FFmpeg table row.
const SPEC_TO_FFMPEG_MODE: [usize; 10] = [2, 0, 1, 3, 4, 5, 6, 7, 8, 9];

/// Keyframe y-mode probs for one (above, left) context, spec mode values.
#[inline]
pub fn kf_ymode_probs_for(above: u8, left: u8) -> [u8; 9] {
    let a = SPEC_TO_FFMPEG_MODE[above as usize];
    let l = SPEC_TO_FFMPEG_MODE[left as usize];
    let base = (a * 10 + l) * 9;
    let mut out = [0u8; 9];
    out.copy_from_slice(&DEFAULT_KF_YMODE_PROBS[base..base + 9]);
    out
}

/// Keyframe chroma mode probs for one y mode, spec mode values.
#[inline]
pub fn kf_uvmode_probs_for(y_mode: usize) -> [u8; 9] {
    let base = SPEC_TO_FFMPEG_MODE[y_mode] * 9;
    let mut out = [0u8; 9];
    out.copy_from_slice(&DEFAULT_KF_UVMODE_PROBS[base..base + 9]);
    out
}

#[inline]
pub fn kf_partition_probs_for(level: usize, ctx: usize) -> [u8; 3] {
    let base = (level * 4 + ctx) * 3;
    let mut out = [0u8; 3];
    out.copy_from_slice(&DEFAULT_KF_PARTITION_PROBS[base..base + 3]);
    out
}

/// `[half][block_size][axis]` block width/height in 8px units.
#[inline]
pub fn bwh(half: usize, bs: usize) -> (usize, usize) {
    let base = (half * crate::tables::N_BS_SIZES + bs) * 2;
    (BWH_TAB[base] as usize, BWH_TAB[base + 1] as usize)
}
