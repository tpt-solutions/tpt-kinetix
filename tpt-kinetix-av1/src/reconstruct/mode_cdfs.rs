use super::*;

/// Default CDF state for the non-coefficient syntax elements (partition,
/// intra modes, transform size, skip, angle delta, interpolation filter).
/// Initialised from the exact spec default tables in `cdf_tables_gen`.
#[derive(Clone)]
pub(super) struct ModeCdfs {
    pub(super) partition_w8: [[u16; 5]; 4],
    pub(super) partition_w16: [[u16; 11]; 4],
    pub(super) partition_w32: [[u16; 11]; 4],
    pub(super) partition_w64: [[u16; 11]; 4],
    pub(super) partition_w128: [[u16; 9]; 4],
    pub(super) intra_y_mode: [[[u16; 14]; 5]; 5],
    pub(super) uv_mode_not_allowed: [[u16; 14]; 13],
    pub(super) uv_mode_allowed: [[u16; 15]; 13],
    pub(super) tx_8x8: [[u16; 3]; 3],
    pub(super) tx_16x16: [[u16; 4]; 3],
    pub(super) tx_32x32: [[u16; 4]; 3],
    pub(super) tx_64x64: [[u16; 4]; 3],
    #[allow(dead_code)]
    pub(super) txfm_split: [[u16; 3]; 21],
    pub(super) skip: [[u16; 3]; 3],
    pub(super) segment_id: [[u16; 9]; 3],
    /// `TileDeltaQCdf` (§8.3.2 `delta_q_abs`'s cdf selection) — one shared
    /// adaptive CDF for the whole tile, not indexed by context.
    pub(super) delta_q: [u16; 5],
    /// `TileDeltaLFCdf`, used when `delta_lf_multi == 0`.
    pub(super) delta_lf: [u16; 5],
    /// `TileDeltaLFMultiCdf[i]` for `i = 0..FRAME_LF_COUNT-1`, used when
    /// `delta_lf_multi == 1`.
    pub(super) delta_lf_multi: [[u16; 5]; 4],
    #[allow(dead_code)]
    pub(super) angle_delta: [[u16; 8]; 8],
    pub(super) interp_filter: [[u16; 4]; 16],
    pub(super) filter_intra: [[u16; 3]; 22],
    pub(super) filter_intra_mode: [u16; 6],
    pub(super) cfl_sign: [u16; 9],
    pub(super) cfl_alpha: [[u16; 17]; 6],
    pub(super) palette_y_mode: [[[u16; 3]; 3]; 7],
    pub(super) palette_uv_mode: [[u16; 3]; 2],
    pub(super) palette_y_size: [[u16; 8]; 7],
    pub(super) palette_uv_size: [[u16; 8]; 7],
    pub(super) palette_y_color_2: [[u16; 3]; 5],
    pub(super) palette_y_color_3: [[u16; 4]; 5],
    pub(super) palette_y_color_4: [[u16; 5]; 5],
    pub(super) palette_y_color_5: [[u16; 6]; 5],
    pub(super) palette_y_color_6: [[u16; 7]; 5],
    pub(super) palette_y_color_7: [[u16; 8]; 5],
    pub(super) palette_y_color_8: [[u16; 9]; 5],
    pub(super) palette_uv_color_2: [[u16; 3]; 5],
    pub(super) palette_uv_color_3: [[u16; 4]; 5],
    pub(super) palette_uv_color_4: [[u16; 5]; 5],
    pub(super) palette_uv_color_5: [[u16; 6]; 5],
    pub(super) palette_uv_color_6: [[u16; 7]; 5],
    pub(super) palette_uv_color_7: [[u16; 8]; 5],
    pub(super) palette_uv_color_8: [[u16; 9]; 5],
}

/// `UV_CFL_PRED` (AV1 spec `intra_mode` enumeration): the chroma-only
/// "chroma from luma" mode, `uv_mode_allowed`'s 14th (index 13) symbol.
pub(super) const UV_CFL_PRED: usize = 13;

/// `CFL_SIGN_ZERO`/`CFL_SIGN_NEG`/`CFL_SIGN_POS` (AV1 spec §6.10.36).
const CFL_SIGN_ZERO: i32 = 0;
const CFL_SIGN_NEG: i32 = 1;

/// `Default_Cfl_Sign_Cdf[CFL_JOINT_SIGNS + 1]` (AV1 spec "Additional tables").
const DEFAULT_CFL_SIGN_CDF: [u16; 9] = [1418, 2123, 13340, 18405, 26972, 28343, 32294, 32768, 0];

/// `Default_Cfl_Alpha_Cdf[CFL_ALPHA_CONTEXTS][CFL_ALPHABET_SIZE + 1]` (AV1
/// spec "Additional tables").
const DEFAULT_CFL_ALPHA_CDF: [[u16; 17]; 6] = [
    [
        7637, 20719, 31401, 32481, 32657, 32688, 32692, 32696, 32700, 32704, 32708, 32712, 32716,
        32720, 32724, 32768, 0,
    ],
    [
        14365, 23603, 28135, 31168, 32167, 32395, 32487, 32573, 32620, 32647, 32668, 32672, 32676,
        32680, 32684, 32768, 0,
    ],
    [
        11532, 22380, 28445, 31360, 32349, 32523, 32584, 32649, 32673, 32677, 32681, 32685, 32689,
        32693, 32697, 32768, 0,
    ],
    [
        26990, 31402, 32282, 32571, 32692, 32696, 32700, 32704, 32708, 32712, 32716, 32720, 32724,
        32728, 32732, 32768, 0,
    ],
    [
        17248, 26058, 28904, 30608, 31305, 31877, 32126, 32321, 32394, 32464, 32516, 32560, 32576,
        32593, 32622, 32768, 0,
    ],
    [
        14738, 21678, 25779, 27901, 29024, 30302, 30980, 31843, 32144, 32413, 32520, 32594, 32622,
        32656, 32660, 32768, 0,
    ],
];

/// `Default_Palette_Y_Mode_Cdf[PALETTE_BLOCK_SIZE_CONTEXTS][PALETTE_Y_MODE_CONTEXTS][3]`
/// (AV1 spec "Additional tables").
const DEFAULT_PALETTE_Y_MODE_CDF: [[[u16; 3]; 3]; 7] = [
    [[31676, 32768, 0], [3419, 32768, 0], [1261, 32768, 0]],
    [[31912, 32768, 0], [2859, 32768, 0], [980, 32768, 0]],
    [[31823, 32768, 0], [3400, 32768, 0], [781, 32768, 0]],
    [[32030, 32768, 0], [3561, 32768, 0], [904, 32768, 0]],
    [[32309, 32768, 0], [7337, 32768, 0], [1462, 32768, 0]],
    [[32265, 32768, 0], [4015, 32768, 0], [1521, 32768, 0]],
    [[32450, 32768, 0], [7946, 32768, 0], [129, 32768, 0]],
];

/// `Default_Palette_Uv_Mode_Cdf[PALETTE_UV_MODE_CONTEXTS][3]` (AV1 spec
/// "Additional tables").
const DEFAULT_PALETTE_UV_MODE_CDF: [[u16; 3]; 2] = [[32461, 32768, 0], [21488, 32768, 0]];

/// `Default_Palette_Y_Size_Cdf[PALETTE_BLOCK_SIZE_CONTEXTS][PALETTE_SIZES + 1]`
/// (AV1 spec "Additional tables").
const DEFAULT_PALETTE_Y_SIZE_CDF: [[u16; 8]; 7] = [
    [7952, 13000, 18149, 21478, 25527, 29241, 32768, 0],
    [7139, 11421, 16195, 19544, 23666, 28073, 32768, 0],
    [7788, 12741, 17325, 20500, 24315, 28530, 32768, 0],
    [8271, 14064, 18246, 21564, 25071, 28533, 32768, 0],
    [12725, 19180, 21863, 24839, 27535, 30120, 32768, 0],
    [9711, 14888, 16923, 21052, 25661, 27875, 32768, 0],
    [14940, 20797, 21678, 24186, 27033, 28999, 32768, 0],
];

/// `Default_Palette_Uv_Size_Cdf[PALETTE_BLOCK_SIZE_CONTEXTS][PALETTE_SIZES + 1]`
/// (AV1 spec "Additional tables").
const DEFAULT_PALETTE_UV_SIZE_CDF: [[u16; 8]; 7] = [
    [8713, 19979, 27128, 29609, 31331, 32272, 32768, 0],
    [5839, 15573, 23581, 26947, 29848, 31700, 32768, 0],
    [4426, 11260, 17999, 21483, 25863, 29430, 32768, 0],
    [3228, 9464, 14993, 18089, 22523, 27420, 32768, 0],
    [3768, 8886, 13091, 17852, 22495, 27207, 32768, 0],
    [2464, 8451, 12861, 21632, 25525, 28555, 32768, 0],
    [1269, 5435, 10433, 18963, 21700, 25865, 32768, 0],
];

/// `Default_Palette_Size_{2..8}_Y_Color_Cdf[PALETTE_COLOR_CONTEXTS][size + 1]`
/// (AV1 spec "Additional tables").
const DEFAULT_PALETTE_Y_COLOR_2_CDF: [[u16; 3]; 5] = [
    [28710, 32768, 0],
    [16384, 32768, 0],
    [10553, 32768, 0],
    [27036, 32768, 0],
    [31603, 32768, 0],
];
const DEFAULT_PALETTE_Y_COLOR_3_CDF: [[u16; 4]; 5] = [
    [27877, 30490, 32768, 0],
    [11532, 25697, 32768, 0],
    [6544, 30234, 32768, 0],
    [23018, 28072, 32768, 0],
    [31915, 32385, 32768, 0],
];
const DEFAULT_PALETTE_Y_COLOR_4_CDF: [[u16; 5]; 5] = [
    [25572, 28046, 30045, 32768, 0],
    [9478, 21590, 27256, 32768, 0],
    [7248, 26837, 29824, 32768, 0],
    [19167, 24486, 28349, 32768, 0],
    [31400, 31825, 32250, 32768, 0],
];
const DEFAULT_PALETTE_Y_COLOR_5_CDF: [[u16; 6]; 5] = [
    [24779, 26955, 28576, 30282, 32768, 0],
    [8669, 20364, 24073, 28093, 32768, 0],
    [4255, 27565, 29377, 31067, 32768, 0],
    [19864, 23674, 26716, 29530, 32768, 0],
    [31646, 31893, 32147, 32426, 32768, 0],
];
const DEFAULT_PALETTE_Y_COLOR_6_CDF: [[u16; 7]; 5] = [
    [23132, 25407, 26970, 28435, 30073, 32768, 0],
    [7443, 17242, 20717, 24762, 27982, 32768, 0],
    [6300, 24862, 26944, 28784, 30671, 32768, 0],
    [18916, 22895, 25267, 27435, 29652, 32768, 0],
    [31270, 31550, 31808, 32059, 32353, 32768, 0],
];
const DEFAULT_PALETTE_Y_COLOR_7_CDF: [[u16; 8]; 5] = [
    [23105, 25199, 26464, 27684, 28931, 30318, 32768, 0],
    [6950, 15447, 18952, 22681, 25567, 28563, 32768, 0],
    [7560, 23474, 25490, 27203, 28921, 30708, 32768, 0],
    [18544, 22373, 24457, 26195, 28119, 30045, 32768, 0],
    [31198, 31451, 31670, 31882, 32123, 32391, 32768, 0],
];
const DEFAULT_PALETTE_Y_COLOR_8_CDF: [[u16; 9]; 5] = [
    [21689, 23883, 25163, 26352, 27506, 28827, 30195, 32768, 0],
    [6892, 15385, 17840, 21606, 24287, 26753, 29204, 32768, 0],
    [5651, 23182, 25042, 26518, 27982, 29392, 30900, 32768, 0],
    [19349, 22578, 24418, 25994, 27524, 29031, 30448, 32768, 0],
    [31028, 31270, 31504, 31705, 31927, 32153, 32392, 32768, 0],
];

/// `Default_Palette_Size_{2..8}_Uv_Color_Cdf[PALETTE_COLOR_CONTEXTS][size + 1]`
/// (AV1 spec "Additional tables").
const DEFAULT_PALETTE_UV_COLOR_2_CDF: [[u16; 3]; 5] = [
    [29089, 32768, 0],
    [16384, 32768, 0],
    [8713, 32768, 0],
    [29257, 32768, 0],
    [31610, 32768, 0],
];
const DEFAULT_PALETTE_UV_COLOR_3_CDF: [[u16; 4]; 5] = [
    [25257, 29145, 32768, 0],
    [12287, 27293, 32768, 0],
    [7033, 27960, 32768, 0],
    [20145, 25405, 32768, 0],
    [30608, 31639, 32768, 0],
];
const DEFAULT_PALETTE_UV_COLOR_4_CDF: [[u16; 5]; 5] = [
    [24210, 27175, 29903, 32768, 0],
    [9888, 22386, 27214, 32768, 0],
    [5901, 26053, 29293, 32768, 0],
    [18318, 22152, 28333, 32768, 0],
    [30459, 31136, 31926, 32768, 0],
];
const DEFAULT_PALETTE_UV_COLOR_5_CDF: [[u16; 6]; 5] = [
    [22980, 25479, 27781, 29986, 32768, 0],
    [8413, 21408, 24859, 28874, 32768, 0],
    [2257, 29449, 30594, 31598, 32768, 0],
    [19189, 21202, 25915, 28620, 32768, 0],
    [31844, 32044, 32281, 32518, 32768, 0],
];
const DEFAULT_PALETTE_UV_COLOR_6_CDF: [[u16; 7]; 5] = [
    [22217, 24567, 26637, 28683, 30548, 32768, 0],
    [7307, 16406, 19636, 24632, 28424, 32768, 0],
    [4441, 25064, 26879, 28942, 30919, 32768, 0],
    [17210, 20528, 23319, 26750, 29582, 32768, 0],
    [30674, 30953, 31396, 31735, 32207, 32768, 0],
];
const DEFAULT_PALETTE_UV_COLOR_7_CDF: [[u16; 8]; 5] = [
    [21239, 23168, 25044, 26962, 28705, 30506, 32768, 0],
    [6545, 15012, 18004, 21817, 25503, 28701, 32768, 0],
    [3448, 26295, 27437, 28704, 30126, 31442, 32768, 0],
    [15889, 18323, 21704, 24698, 26976, 29690, 32768, 0],
    [30988, 31204, 31479, 31734, 31983, 32325, 32768, 0],
];
const DEFAULT_PALETTE_UV_COLOR_8_CDF: [[u16; 9]; 5] = [
    [21442, 23288, 24758, 26246, 27649, 28980, 30563, 32768, 0],
    [5863, 14933, 17552, 20668, 23683, 26411, 29273, 32768, 0],
    [3415, 25810, 26877, 27990, 29223, 30394, 31618, 32768, 0],
    [17965, 20084, 22232, 23974, 26274, 28402, 30390, 32768, 0],
    [31190, 31329, 31516, 31679, 31825, 32026, 32322, 32768, 0],
];

/// `Default_Filter_Intra_Cdf[BLOCK_SIZES][3]` (AV1 spec "Additional tables").
/// Indices 10-15 and 20-21 are never used (spec note) but are transcribed
/// verbatim anyway rather than left as gaps.
const DEFAULT_FILTER_INTRA_CDF: [[u16; 3]; 22] = [
    [4621, 32768, 0],
    [6743, 32768, 0],
    [5893, 32768, 0],
    [7866, 32768, 0],
    [12551, 32768, 0],
    [9394, 32768, 0],
    [12408, 32768, 0],
    [14301, 32768, 0],
    [12756, 32768, 0],
    [22343, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
    [12770, 32768, 0],
    [10368, 32768, 0],
    [20229, 32768, 0],
    [18101, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
];

/// `Default_Filter_Intra_Mode_Cdf[6]` (AV1 spec "Additional tables").
const DEFAULT_FILTER_INTRA_MODE_CDF: [u16; 6] = [8949, 12776, 17211, 29558, 32768, 0];

/// `Default_Delta_Q_Cdf[DELTA_Q_SMALL + 2]` (AV1 spec "Additional tables").
/// `Default_Delta_Lf_Cdf` has the identical values and is reused for both
/// `TileDeltaLFCdf` and every `TileDeltaLFMultiCdf[i]` instance (the spec
/// only lists one default table for both the single and multi forms).
const DEFAULT_DELTA_Q_CDF: [u16; 5] = [28160, 32120, 32677, 32768, 0];

impl ModeCdfs {
    pub(super) fn new() -> Self {
        ModeCdfs {
            partition_w8: DEFAULT_PARTITION_W8_CDF,
            partition_w16: DEFAULT_PARTITION_W16_CDF,
            partition_w32: DEFAULT_PARTITION_W32_CDF,
            partition_w64: DEFAULT_PARTITION_W64_CDF,
            partition_w128: DEFAULT_PARTITION_W128_CDF,
            intra_y_mode: DEFAULT_INTRA_FRAME_Y_MODE_CDF,
            uv_mode_not_allowed: DEFAULT_UV_MODE_CFL_NOT_ALLOWED_CDF,
            uv_mode_allowed: DEFAULT_UV_MODE_CFL_ALLOWED_CDF,
            tx_8x8: DEFAULT_TX_8X8_CDF,
            tx_16x16: DEFAULT_TX_16X16_CDF,
            tx_32x32: DEFAULT_TX_32X32_CDF,
            tx_64x64: DEFAULT_TX_64X64_CDF,
            txfm_split: DEFAULT_TXFM_SPLIT_CDF,
            skip: DEFAULT_SKIP_CDF,
            // AV1 default `segment_id_cdf` is not yet transcribed into
            // `cdf_tables_gen`; a uniform 8-way CDF is used as a placeholder so
            // segmentation-enabled frames stay bit-aligned. Replace with the
            // exact spec table before relying on pixel-exact seg decode.
            segment_id: [[4096, 8192, 12288, 16384, 20480, 24576, 28672, 32768, 0]; 3],
            delta_q: DEFAULT_DELTA_Q_CDF,
            delta_lf: DEFAULT_DELTA_Q_CDF,
            delta_lf_multi: [DEFAULT_DELTA_Q_CDF; 4],
            angle_delta: DEFAULT_ANGLE_DELTA_CDF,
            interp_filter: DEFAULT_INTERP_FILTER_CDF,
            filter_intra: DEFAULT_FILTER_INTRA_CDF,
            filter_intra_mode: DEFAULT_FILTER_INTRA_MODE_CDF,
            cfl_sign: DEFAULT_CFL_SIGN_CDF,
            cfl_alpha: DEFAULT_CFL_ALPHA_CDF,
            palette_y_mode: DEFAULT_PALETTE_Y_MODE_CDF,
            palette_uv_mode: DEFAULT_PALETTE_UV_MODE_CDF,
            palette_y_size: DEFAULT_PALETTE_Y_SIZE_CDF,
            palette_uv_size: DEFAULT_PALETTE_UV_SIZE_CDF,
            palette_y_color_2: DEFAULT_PALETTE_Y_COLOR_2_CDF,
            palette_y_color_3: DEFAULT_PALETTE_Y_COLOR_3_CDF,
            palette_y_color_4: DEFAULT_PALETTE_Y_COLOR_4_CDF,
            palette_y_color_5: DEFAULT_PALETTE_Y_COLOR_5_CDF,
            palette_y_color_6: DEFAULT_PALETTE_Y_COLOR_6_CDF,
            palette_y_color_7: DEFAULT_PALETTE_Y_COLOR_7_CDF,
            palette_y_color_8: DEFAULT_PALETTE_Y_COLOR_8_CDF,
            palette_uv_color_2: DEFAULT_PALETTE_UV_COLOR_2_CDF,
            palette_uv_color_3: DEFAULT_PALETTE_UV_COLOR_3_CDF,
            palette_uv_color_4: DEFAULT_PALETTE_UV_COLOR_4_CDF,
            palette_uv_color_5: DEFAULT_PALETTE_UV_COLOR_5_CDF,
            palette_uv_color_6: DEFAULT_PALETTE_UV_COLOR_6_CDF,
            palette_uv_color_7: DEFAULT_PALETTE_UV_COLOR_7_CDF,
            palette_uv_color_8: DEFAULT_PALETTE_UV_COLOR_8_CDF,
        }
    }

    /// `has_palette_y` (AV1 spec §8.3.2): cdf `TilePaletteYModeCdf[bsizeCtx][ctx]`.
    pub(super) fn read_has_palette_y(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        bsize_ctx: usize,
        ctx: usize,
    ) -> bool {
        dec.read_symbol(&mut self.palette_y_mode[bsize_ctx][ctx]) == 1
    }

    /// `has_palette_uv` (AV1 spec §8.3.2): cdf `TilePaletteUVModeCdf[ctx]`.
    pub(super) fn read_has_palette_uv(&mut self, dec: &mut SymbolDecoder<'_>, ctx: usize) -> bool {
        dec.read_symbol(&mut self.palette_uv_mode[ctx]) == 1
    }

    /// `palette_size_y_minus_2` (AV1 spec §8.3.2): cdf `TilePaletteYSizeCdf[bsizeCtx]`.
    pub(super) fn read_palette_size_y(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        bsize_ctx: usize,
    ) -> usize {
        dec.read_symbol(&mut self.palette_y_size[bsize_ctx])
    }

    /// `palette_size_uv_minus_2` (AV1 spec §8.3.2): cdf `TilePaletteUVSizeCdf[bsizeCtx]`.
    pub(super) fn read_palette_size_uv(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        bsize_ctx: usize,
    ) -> usize {
        dec.read_symbol(&mut self.palette_uv_size[bsize_ctx])
    }

    /// `palette_color_idx_y` (AV1 spec §8.3.2): cdf `TilePaletteSize{n}YColorCdf[ctx]`.
    pub(super) fn read_palette_color_idx_y(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        size: usize,
        ctx: usize,
    ) -> usize {
        match size {
            2 => dec.read_symbol(&mut self.palette_y_color_2[ctx]),
            3 => dec.read_symbol(&mut self.palette_y_color_3[ctx]),
            4 => dec.read_symbol(&mut self.palette_y_color_4[ctx]),
            5 => dec.read_symbol(&mut self.palette_y_color_5[ctx]),
            6 => dec.read_symbol(&mut self.palette_y_color_6[ctx]),
            7 => dec.read_symbol(&mut self.palette_y_color_7[ctx]),
            _ => dec.read_symbol(&mut self.palette_y_color_8[ctx]),
        }
    }

    /// `palette_color_idx_uv` (AV1 spec §8.3.2): cdf `TilePaletteSize{n}UvColorCdf[ctx]`.
    pub(super) fn read_palette_color_idx_uv(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        size: usize,
        ctx: usize,
    ) -> usize {
        match size {
            2 => dec.read_symbol(&mut self.palette_uv_color_2[ctx]),
            3 => dec.read_symbol(&mut self.palette_uv_color_3[ctx]),
            4 => dec.read_symbol(&mut self.palette_uv_color_4[ctx]),
            5 => dec.read_symbol(&mut self.palette_uv_color_5[ctx]),
            6 => dec.read_symbol(&mut self.palette_uv_color_6[ctx]),
            7 => dec.read_symbol(&mut self.palette_uv_color_7[ctx]),
            _ => dec.read_symbol(&mut self.palette_uv_color_8[ctx]),
        }
    }

    /// `read_cfl_alphas()` (AV1 spec §5.11.45): returns `(CflAlphaU,
    /// CflAlphaV)`. Only called when `UVMode == UV_CFL_PRED`.
    pub(super) fn read_cfl_alphas(&mut self, dec: &mut SymbolDecoder<'_>) -> (i32, i32) {
        let cfl_alpha_signs = dec.read_symbol(&mut self.cfl_sign) as i32;
        let sign_u = (cfl_alpha_signs + 1) / 3;
        let sign_v = (cfl_alpha_signs + 1) % 3;
        let alpha_u = if sign_u != CFL_SIGN_ZERO {
            let ctx = ((sign_u - 1) * 3 + sign_v) as usize;
            let mag = 1 + dec.read_symbol(&mut self.cfl_alpha[ctx]) as i32;
            if sign_u == CFL_SIGN_NEG {
                -mag
            } else {
                mag
            }
        } else {
            0
        };
        let alpha_v = if sign_v != CFL_SIGN_ZERO {
            let ctx = ((sign_v - 1) * 3 + sign_u) as usize;
            let mag = 1 + dec.read_symbol(&mut self.cfl_alpha[ctx]) as i32;
            if sign_v == CFL_SIGN_NEG {
                -mag
            } else {
                mag
            }
        } else {
            0
        };
        (alpha_u, alpha_v)
    }

    /// `filter_intra_mode_info()` (AV1 spec §5.11.24). Returns `Some(mode)`
    /// (`filter_intra_mode`, 0-4) when `use_filter_intra` was signalled and
    /// read as 1, `None` otherwise (including when the leading condition
    /// means no symbol is read at all — `use_filter_intra` implicitly stays
    /// 0 in that case, per spec, with no bitstream cost).
    pub(super) fn read_filter_intra_mode_info(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        enable_filter_intra: bool,
        y_mode: usize,
        bsize: usize,
    ) -> Option<usize> {
        const DC_PRED: usize = 0;
        if !(enable_filter_intra
            && y_mode == DC_PRED
            && BLOCK_WIDTH[bsize].max(BLOCK_HEIGHT[bsize]) <= 32)
        {
            return None;
        }
        let use_filter_intra = dec.read_symbol(&mut self.filter_intra[bsize]) == 1;
        if !use_filter_intra {
            return None;
        }
        Some(dec.read_symbol(&mut self.filter_intra_mode))
    }

    pub(super) fn read_partition(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        bucket: usize,
        ctx: usize,
    ) -> usize {
        let cdf: &mut [u16] = match bucket {
            0 => &mut self.partition_w8[ctx],
            1 => &mut self.partition_w16[ctx],
            2 => &mut self.partition_w32[ctx],
            3 => &mut self.partition_w64[ctx],
            _ => &mut self.partition_w128[ctx],
        };
        dec.read_symbol(cdf)
    }

    #[inline]
    pub(super) fn base_partition_cdf(&self, bucket: usize, ctx: usize) -> &[u16] {
        match bucket {
            0 => &self.partition_w8[ctx],
            1 => &self.partition_w16[ctx],
            2 => &self.partition_w32[ctx],
            3 => &self.partition_w64[ctx],
            _ => &self.partition_w128[ctx],
        }
    }

    /// `split_or_horz` (AV1 spec §8.3.2): a synthetic 2-symbol CDF folded
    /// from the full `partition` CDF, read-only w.r.t. the underlying
    /// `partition_w*` tables (they are never adapted by this path — the
    /// synthetic array is rebuilt fresh from their *current* values on every
    /// call, per spec). Returns `true` for `PARTITION_SPLIT`, `false` for
    /// `PARTITION_HORZ`.
    pub(super) fn read_split_or_horz(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        bucket: usize,
        ctx: usize,
        bsize: usize,
    ) -> bool {
        let cdf = self.base_partition_cdf(bucket, ctx);
        // Spec: "bsl is never equal to 1 when decoding split_or_horz/vert",
        // i.e. the `PARTITION_W8` bucket (4 symbols: NONE/HORZ/VERT/SPLIT,
        // no extended partitions) never legitimately reaches this function.
        // Clamp instead of indexing out of bounds so any bitstream that
        // violates this stays a decode error elsewhere rather than a panic:
        // an index past the last real cumulative entry (`cdf.len() - 2`)
        // names a partition type this bucket doesn't have, which
        // mathematically carries zero probability mass.
        let max_valid = cdf.len() - 2;
        let mass = |hi: usize| -> i32 {
            if hi > max_valid {
                return 0;
            }
            let prev = if hi == 0 { 0 } else { cdf[hi - 1] as i32 };
            cdf[hi] as i32 - prev
        };
        let mut psum = mass(PARTITION_VERT as usize)
            + mass(PARTITION_SPLIT as usize)
            + mass(PARTITION_HORZ_A as usize)
            + mass(PARTITION_VERT_A as usize)
            + mass(PARTITION_VERT_B as usize);
        if bsize != BLOCK_128X128 {
            psum += mass(PARTITION_VERT_4 as usize);
        }
        let mut synthetic = [(32768 - psum) as u16, 32768u16, 0u16];
        dec.read_symbol(&mut synthetic) == 1
    }

    /// `split_or_vert` (AV1 spec §8.3.2): the `split_or_horz` counterpart.
    /// Returns `true` for `PARTITION_SPLIT`, `false` for `PARTITION_VERT`.
    pub(super) fn read_split_or_vert(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        bucket: usize,
        ctx: usize,
        bsize: usize,
    ) -> bool {
        let cdf = self.base_partition_cdf(bucket, ctx);
        // Spec: "bsl is never equal to 1 when decoding split_or_horz/vert",
        // i.e. the `PARTITION_W8` bucket (4 symbols: NONE/HORZ/VERT/SPLIT,
        // no extended partitions) never legitimately reaches this function.
        // Clamp instead of indexing out of bounds so any bitstream that
        // violates this stays a decode error elsewhere rather than a panic:
        // an index past the last real cumulative entry (`cdf.len() - 2`)
        // names a partition type this bucket doesn't have, which
        // mathematically carries zero probability mass.
        let max_valid = cdf.len() - 2;
        let mass = |hi: usize| -> i32 {
            if hi > max_valid {
                return 0;
            }
            let prev = if hi == 0 { 0 } else { cdf[hi - 1] as i32 };
            cdf[hi] as i32 - prev
        };
        let mut psum = mass(PARTITION_HORZ as usize)
            + mass(PARTITION_SPLIT as usize)
            + mass(PARTITION_HORZ_A as usize)
            + mass(PARTITION_HORZ_B as usize)
            + mass(PARTITION_VERT_A as usize);
        if bsize != BLOCK_128X128 {
            psum += mass(PARTITION_HORZ_4 as usize);
        }
        let mut synthetic = [(32768 - psum) as u16, 32768u16, 0u16];
        dec.read_symbol(&mut synthetic) == 1
    }

    pub(super) fn read_intra_y_mode(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        above_ctx: usize,
        left_ctx: usize,
    ) -> usize {
        dec.read_symbol(&mut self.intra_y_mode[above_ctx][left_ctx])
    }

    /// `intra_angle_info_y()`/`intra_angle_info_uv()` (AV1 spec §5.11.42/43):
    /// reads `angle_delta_y`/`angle_delta_uv` (cdf `TileAngleDeltaCdf[mode -
    /// V_PRED]`, §8.3.2) and returns the biased-out `AngleDeltaY`/
    /// `AngleDeltaUV` (`-MAX_ANGLE_DELTA..=MAX_ANGLE_DELTA`). Only called
    /// when `is_directional_mode(mode) && MiSize >= BLOCK_8X8` — this
    /// function assumes that gate has already been checked by the caller.
    pub(super) fn read_angle_delta(&mut self, dec: &mut SymbolDecoder<'_>, mode: usize) -> i32 {
        let ctx = mode - V_PRED as usize;
        dec.read_symbol(&mut self.angle_delta[ctx]) as i32 - MAX_ANGLE_DELTA
    }

    pub(super) fn read_uv_mode(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        cfl_allowed: bool,
        y_mode: usize,
    ) -> usize {
        if cfl_allowed {
            dec.read_symbol(&mut self.uv_mode_allowed[y_mode])
        } else {
            dec.read_symbol(&mut self.uv_mode_not_allowed[y_mode])
        }
    }

    pub(super) fn read_tx_level(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        depth: usize,
        ctx: usize,
    ) -> usize {
        let cdf: &mut [u16] = match depth {
            0 => &mut self.tx_8x8[ctx],
            1 => &mut self.tx_16x16[ctx],
            2 => &mut self.tx_32x32[ctx],
            _ => &mut self.tx_64x64[ctx],
        };
        dec.read_symbol(cdf)
    }

    pub(super) fn read_skip(&mut self, dec: &mut SymbolDecoder<'_>, ctx: usize) -> usize {
        dec.read_symbol(&mut self.skip[ctx])
    }

    pub(super) fn read_segment_id(&mut self, dec: &mut SymbolDecoder<'_>, ctx: usize) -> usize {
        dec.read_symbol(&mut self.segment_id[ctx])
    }

    pub(super) fn read_delta_q_abs(&mut self, dec: &mut SymbolDecoder<'_>) -> usize {
        dec.read_symbol(&mut self.delta_q)
    }

    pub(super) fn read_delta_lf_abs(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        multi_index: Option<usize>,
    ) -> usize {
        match multi_index {
            Some(i) => dec.read_symbol(&mut self.delta_lf_multi[i]),
            None => dec.read_symbol(&mut self.delta_lf),
        }
    }
}
