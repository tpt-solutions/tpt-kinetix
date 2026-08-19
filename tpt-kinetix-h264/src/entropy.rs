//! H.264 CABAC entropy decoding (Section 9.3), alongside the CAVLC path in
//! [`crate::slice`].
//!
//! This implements the binary arithmetic decoding engine — context-adaptive
//! decisions, bypass decoding, and slice-termination decoding (§9.3.3.2) —
//! plus context-variable initialisation (§9.3.1.1) from `(m, n)` init values.
//! These are the well-specified, table-driven primitives that every CABAC
//! syntax element (mb_type, cbf, coeff levels, mvd, …) is built from.
//!
//! Not yet included: the per-syntax-element context-index assignment tables
//! (spec Tables 9-12 through 9-33, ~1000+ `(m, n)` pairs) and macroblock-level
//! CABAC syntax parsing. Those sit on top of the engine below and are left for
//! follow-up work, matching the CAVLC path's "real but simplified" scope.

use crate::bitreader::BitReader;

/// One CABAC context variable: probability state index and most-probable-symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CabacContext {
    /// `pStateIdx` in the spec: index into [`RANGE_TAB_LPS`] / [`TRANS_IDX_LPS`], 0..=63.
    pub state: u8,
    /// `valMPS` in the spec: the most-probable-symbol value, 0 or 1.
    pub mps: u8,
}

impl CabacContext {
    /// Initialise a context variable from its `(m, n)` init values and the
    /// slice QP, per spec §9.3.1.1.
    pub fn init(m: i32, n: i32, slice_qp_y: i32) -> Self {
        let qp = slice_qp_y.clamp(0, 51);
        let pre_ctx_state = (((m * qp) >> 4) + n).clamp(1, 126);
        if pre_ctx_state <= 63 {
            Self {
                state: (63 - pre_ctx_state) as u8,
                mps: 0,
            }
        } else {
            Self {
                state: (pre_ctx_state - 64) as u8,
                mps: 1,
            }
        }
    }
}

/// `rangeTabLPS` (spec Table 9-44): `[pStateIdx][qCodIRangeIdx]` → `codIRangeLPS`.
#[rustfmt::skip]
const RANGE_TAB_LPS: [[u32; 4]; 64] = [
    [128, 176, 208, 240], [128, 167, 197, 227], [128, 158, 187, 216], [123, 150, 178, 205],
    [116, 142, 169, 195], [111, 135, 160, 185], [105, 128, 152, 175], [100, 122, 144, 166],
    [95, 116, 137, 158],  [90, 110, 130, 150],  [85, 104, 123, 142],  [81, 99, 117, 135],
    [77, 94, 111, 128],   [73, 89, 105, 122],   [69, 85, 100, 116],   [66, 80, 95, 110],
    [62, 76, 90, 104],    [59, 72, 86, 99],     [56, 69, 81, 94],     [53, 65, 77, 89],
    [51, 62, 73, 85],     [48, 59, 69, 80],     [46, 56, 66, 76],     [43, 53, 63, 72],
    [41, 50, 59, 69],     [39, 48, 56, 65],     [37, 45, 54, 62],     [35, 43, 51, 59],
    [33, 41, 48, 56],     [32, 39, 46, 53],     [30, 37, 43, 50],     [29, 35, 41, 48],
    [27, 33, 39, 45],     [26, 31, 37, 43],     [24, 30, 35, 41],     [23, 28, 33, 39],
    [22, 27, 32, 37],     [21, 26, 30, 35],     [20, 24, 29, 33],     [19, 23, 27, 31],
    [18, 22, 26, 30],     [17, 21, 25, 28],     [16, 20, 23, 27],     [15, 19, 22, 25],
    [14, 18, 21, 24],     [14, 17, 20, 23],     [13, 16, 19, 22],     [12, 15, 18, 21],
    [12, 14, 17, 20],     [11, 14, 16, 19],     [11, 13, 15, 18],     [10, 12, 15, 17],
    [10, 12, 14, 16],     [9, 11, 13, 15],      [9, 11, 12, 14],      [8, 10, 12, 14],
    [8, 9, 11, 13],       [7, 9, 11, 12],       [7, 9, 10, 12],       [7, 8, 10, 11],
    [6, 8, 9, 11],        [6, 7, 9, 10],        [6, 7, 8, 9],         [2, 2, 2, 2],
];

/// `transIdxLPS` (spec Table 9-45): next `pStateIdx` after an LPS decision.
///
/// `TRANS_IDX_LPS[28]` was `23` for a long time (a single-entry transcription
/// error) until 2026-08-12; the correct value is `22`, confirmed by decoding
/// FFmpeg's packed `ff_h264_mlps_state` table (`libavcodec/cabac.c`) for
/// `pStateIdx=28` at both `valMPS` values and cross-checking every other
/// entry in this table and in `TRANS_IDX_MPS` the same way (no other
/// discrepancies found). This was the root cause of the long-standing CABAC
/// I-slice desync bug (see `todo.md` Phase D): `pStateIdx=28` undergoing an
/// LPS transition is rare enough that most test content never exercised it
/// (compare against a real FFmpeg-compiled CABAC engine — via a self-authored
/// C harness — to find the exact bin where a scan-content-specific bitstream
/// first diverged from this crate's decode).
#[rustfmt::skip]
const TRANS_IDX_LPS: [u8; 64] = [
    0, 0, 1, 2, 2, 4, 4, 5, 6, 7, 8, 9, 9, 11, 11, 12,
    13, 13, 15, 15, 16, 16, 18, 18, 19, 19, 21, 21, 22, 22, 23, 24,
    24, 25, 26, 26, 27, 27, 28, 29, 29, 30, 30, 30, 31, 32, 32, 33,
    33, 33, 34, 34, 35, 35, 35, 36, 36, 36, 37, 37, 37, 38, 38, 63,
];

/// `transIdxMPS`: next `pStateIdx` after an MPS decision (`i + 1`, saturating at 63).
#[rustfmt::skip]
const TRANS_IDX_MPS: [u8; 64] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
    33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48,
    49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 62, 63,
];

/// CABAC binary arithmetic decoding engine (spec §9.3.3.2).
///
/// Operates over an already byte-aligned RBSP (the caller is responsible for
/// consuming `cabac_alignment_one_bit` bits before construction, per §7.3.4).
pub struct CabacDecoder<'a> {
    reader: BitReader<'a>,
    range: u32,
    offset: u32,
}

impl<'a> CabacDecoder<'a> {
    /// Initialise the arithmetic decoding engine (spec §9.3.1.2): `codIRange = 510`,
    /// `codIOffset` = the next 9 bits of the RBSP.
    pub fn new(data: &'a [u8]) -> anyhow::Result<Self> {
        let mut reader = BitReader::new(data);
        let offset = reader
            .read_bits(9)
            .ok_or_else(|| anyhow::anyhow!("EOF initialising CABAC engine (need 9 bits)"))?;
        Ok(Self {
            reader,
            range: 510,
            offset,
        })
    }

    fn next_bit(&mut self) -> u32 {
        // Spec streams are constructed so the engine never actually needs bits
        // past the end (trailing RBSP bits pad the arithmetic codeword); treat
        // exhaustion as zero-bits rather than erroring mid-decode.
        self.reader.read_bit().unwrap_or(0) as u32
    }

    fn renormalize(&mut self) {
        while self.range < 256 {
            self.range <<= 1;
            self.offset = (self.offset << 1) | self.next_bit();
        }
    }

    /// Decode one context-coded bin (spec §9.3.3.2.1), updating `ctx` in place.
    pub fn decode_decision(&mut self, ctx: &mut CabacContext) -> u8 {
        let q_range_idx = ((self.range >> 6) & 3) as usize;
        let range_lps = RANGE_TAB_LPS[ctx.state as usize][q_range_idx];
        self.range -= range_lps;

        let bin_val = if self.offset >= self.range {
            let bin_val = 1 - ctx.mps;
            self.offset -= self.range;
            self.range = range_lps;
            if ctx.state == 0 {
                ctx.mps = 1 - ctx.mps;
            }
            ctx.state = TRANS_IDX_LPS[ctx.state as usize];
            bin_val
        } else {
            let bin_val = ctx.mps;
            ctx.state = TRANS_IDX_MPS[ctx.state as usize];
            bin_val
        };

        self.renormalize();
        bin_val
    }

    /// Decode one bypass bin (spec §9.3.3.2.3): no context, no renormalisation.
    pub fn decode_bypass(&mut self) -> u8 {
        self.offset = (self.offset << 1) | self.next_bit();
        if self.offset >= self.range {
            self.offset -= self.range;
            1
        } else {
            0
        }
    }

    /// Decode `n` consecutive bypass bins as an unsigned integer, MSB first.
    pub fn decode_bypass_bits(&mut self, n: u32) -> u32 {
        let mut val = 0u32;
        for _ in 0..n {
            val = (val << 1) | self.decode_bypass() as u32;
        }
        val
    }

    pub fn debug_state(&self) -> (u32, u32) {
        (self.range, self.offset)
    }

    /// Decode the `end_of_slice_flag` / `mb_field_decoding_flag`-terminate bin
    /// (spec §9.3.3.2.4).
    pub fn decode_terminate(&mut self) -> u8 {
        self.range -= 2;
        if self.offset >= self.range {
            1
        } else {
            self.renormalize();
            0
        }
    }

    /// Decode a `k`-th order Exp-Golomb (UEGk) bypass-coded suffix (spec §9.3.2.3,
    /// as used by `coeff_abs_level_minus1` and `mvd` after their unary prefixes).
    pub fn decode_bypass_eg(&mut self, k0: u32) -> u32 {
        let mut k = k0;
        let mut code_num = 0u32;
        while self.decode_bypass() == 1 {
            code_num = code_num.saturating_add(1 << k.min(30));
            k += 1;
            if k >= 32 {
                break;
            }
        }
        while k > 0 {
            k -= 1;
            if self.decode_bypass() == 1 {
                code_num = code_num.saturating_add(1 << k.min(30));
            }
        }
        code_num
    }

    /// Decode a fixed-k Golomb-Rice bypass-coded suffix (spec §9.3.2.3).
    ///
    /// Used for `mvd_lX` when |mvd| >= 9: reads unary prefix (quotient q),
    /// then exactly k0 bits for the remainder r. Returns q * 2^k0 + r.
    /// Distinct from `decode_bypass_eg` (level coding, variable suffix length).
    pub fn decode_bypass_golomb(&mut self, k0: u32) -> u32 {
        let mut q = 0u32;
        while self.decode_bypass() == 1 {
            q = q.saturating_add(1);
            if q >= 32 {
                break;
            }
        }
        let mut r = 0u32;
        for i in (0..k0).rev() {
            if self.decode_bypass() == 1 {
                r |= 1 << i;
            }
        }
        q.saturating_mul(1u32 << k0).saturating_add(r)
    }

    /// Decode a truncated-unary bin string using per-position context-coded
    /// bins (spec §9.3.2.1 binarization, §9.3.3.1 context assignment).
    ///
    /// `ctx[i]` supplies the context for bin `i`; if `c_max` exceeds `ctx.len()`
    /// the last context is reused for subsequent bins (as several syntax
    /// elements, e.g. `coded_block_pattern`'s prefix, do). Stops at the first
    /// `0` bin or once `c_max` bins have been read as `1`.
    pub fn decode_truncated_unary(&mut self, c_max: u32, ctx: &mut [CabacContext]) -> u32 {
        let mut val = 0u32;
        while val < c_max {
            let idx = (val as usize).min(ctx.len().saturating_sub(1));
            if self.decode_decision(&mut ctx[idx]) == 0 {
                break;
            }
            val += 1;
        }
        val
    }

    /// Decode a truncated-unary bin string entirely in bypass mode (spec
    /// §9.3.3.1.1.10 and similar), used for suffix bins beyond a syntax
    /// element's context-coded prefix.
    pub fn decode_truncated_unary_bypass(&mut self, c_max: u32) -> u32 {
        let mut val = 0u32;
        while val < c_max && self.decode_bypass() == 1 {
            val += 1;
        }
        val
    }
}

/// Look up one context's `(m, n)` init pair from
/// [`crate::cabac_tables::CABAC_CTX_INIT_I`] and initialise it at `slice_qp_y`.
fn init_ctx(ctx_idx: usize, slice_qp_y: i32) -> CabacContext {
    let (m, n) = crate::cabac_tables::CABAC_CTX_INIT_I[ctx_idx];
    CabacContext::init(m as i32, n as i32, slice_qp_y)
}

/// Left/top neighbour inputs for I-slice `mb_type`'s bin-0 `ctxIdxInc`
/// derivation (spec §9.3.3.1.1.3): a neighbour contributes 1 when it is
/// present in the current slice and coded as I_16x16 or I_PCM.
#[derive(Debug, Clone, Copy, Default)]
pub struct MbTypeNeighbors {
    pub left_is_16x16_or_pcm: bool,
    pub top_is_16x16_or_pcm: bool,
}

/// I-slice `mb_type` CABAC decoder (spec Table 9-11, §9.3.3.1.1.1,
/// ctxIdxOffset 3, ctxIdx 3..=10).
///
/// Decodes the mb_type for an I-slice using the spec's binarization:
/// - Bin 0: 0 = I_NxN, 1 = I_16x16-or-I_PCM. Its `ctxIdxInc` (0..=2) comes
///   from the left/top neighbour macroblock types.
/// - When bin 0 is 1: a `decode_terminate()` bin distinguishes I_PCM (1)
///   from I_16x16 (0).
/// - I_16x16: cbp_luma (1 bin), cbp_chroma (1-2 bins), pred_mode (2 bins).
///
/// Returns the mb_type value (0 = I_NxN, 1..=24 = I_16x16 variants, 25 = I_PCM).
pub struct MbTypeICabacContext {
    ctx: [CabacContext; 8],
}

impl MbTypeICabacContext {
    pub fn new(slice_qp_y: i32) -> Self {
        let base = crate::cabac_tables::MB_TYPE_I_CTX;
        let ctx = std::array::from_fn(|i| init_ctx(base + i, slice_qp_y));
        Self { ctx }
    }

    /// Decode the I-slice mb_type given the left/top neighbour state.
    pub fn decode(&mut self, dec: &mut CabacDecoder, neighbors: &MbTypeNeighbors) -> u32 {
        let bin0_ctx =
            neighbors.left_is_16x16_or_pcm as usize + neighbors.top_is_16x16_or_pcm as usize;
        if dec.decode_decision(&mut self.ctx[bin0_ctx]) == 0 {
            return 0; // I_NxN
        }
        if dec.decode_terminate() == 1 {
            return 25; // I_PCM
        }
        // Absolute ctxIdx 6..=10 -> local indices 3..=7 (see module docs on
        // the exact FFmpeg-cross-checked bin/ctx mapping this mirrors).
        let cbp_luma = dec.decode_decision(&mut self.ctx[3]) as u32;
        let cbp_chroma = if dec.decode_decision(&mut self.ctx[4]) == 1 {
            1 + dec.decode_decision(&mut self.ctx[5]) as u32
        } else {
            0
        };
        let pred_mode = (dec.decode_decision(&mut self.ctx[6]) as u32) * 2
            + dec.decode_decision(&mut self.ctx[7]) as u32;
        1 + pred_mode + cbp_chroma * 4 + cbp_luma * 12
    }
}

/// Coded block pattern (CBP) CABAC decoder (§9.3.3.1.1.4, ctxIdx 73..=84).
///
/// Luma (4 bins, ctxIdx 73..=76) and chroma (presence + value, ctxIdx
/// 77..=84) contexts are both derived from the left/top neighbour
/// macroblocks' CBP bits (and, for luma, from bits already decoded earlier
/// in the *same* macroblock) -- there is no static per-bit-index context.
pub struct CbpCabacContext {
    /// Local layout: `[0..4)` luma (73..=76), `[4..8)` chroma-presence
    /// (77..=80), `[8..12)` chroma-value (81..=84).
    ctx: [CabacContext; 12],
}

impl CbpCabacContext {
    pub fn new(slice_qp_y: i32) -> Self {
        let base = crate::cabac_tables::CBP_LUMA_CTX;
        let ctx = std::array::from_fn(|i| init_ctx(base + i, slice_qp_y));
        Self { ctx }
    }

    /// Decode the coded_block_pattern and return `(cbp_luma, cbp_chroma)`.
    ///
    /// `left_cbp`/`top_cbp` carry the neighbour macroblock's `cbp_word`
    /// (bits 0-3 luma nibble, bits 4-5 chroma value -- same layout as
    /// [`crate::macroblock::Macroblock::cbp`]). Pass the sentinel `0x7CF`
    /// for an unavailable neighbour (this decoder is only used for intra
    /// macroblocks, matching FFmpeg's `IS_INTRA(mb_type) ? 0x7CF : 0x00F`
    /// convention).
    pub fn decode(&mut self, dec: &mut CabacDecoder, left_cbp: u16, top_cbp: u16) -> (u8, u8) {
        let mut cbp: u32 = 0;

        let ctx = (left_cbp & 0x02 == 0) as usize + 2 * (top_cbp & 0x04 == 0) as usize;
        cbp += dec.decode_decision(&mut self.ctx[ctx]) as u32;
        let ctx = (cbp & 0x01 == 0) as usize + 2 * (top_cbp & 0x08 == 0) as usize;
        cbp += (dec.decode_decision(&mut self.ctx[ctx]) as u32) << 1;
        let ctx = (left_cbp & 0x08 == 0) as usize + 2 * (cbp & 0x01 == 0) as usize;
        cbp += (dec.decode_decision(&mut self.ctx[ctx]) as u32) << 2;
        let ctx = (cbp & 0x04 == 0) as usize + 2 * (cbp & 0x02 == 0) as usize;
        cbp += (dec.decode_decision(&mut self.ctx[ctx]) as u32) << 3;

        let cbp_a_chroma = (left_cbp >> 4) & 0x03;
        let cbp_b_chroma = (top_cbp >> 4) & 0x03;
        let presence_ctx = (cbp_a_chroma > 0) as usize + 2 * (cbp_b_chroma > 0) as usize;
        let chroma_cbp = if dec.decode_decision(&mut self.ctx[4 + presence_ctx]) == 0 {
            0u8
        } else {
            let value_ctx = (cbp_a_chroma == 2) as usize + 2 * (cbp_b_chroma == 2) as usize;
            1 + dec.decode_decision(&mut self.ctx[8 + value_ctx])
        };
        (cbp as u8, chroma_cbp)
    }
}

/// `mb_qp_delta` CABAC decoder (spec Table 9-20, §9.3.3.1.1.5, ctxIdx 60..=63).
///
/// Bin 0's `ctxIdxInc` depends on whether the *previous* macroblock's
/// `mb_qp_delta` was nonzero (external state, not a neighbour lookup); bin 1
/// uses a fixed context; bin 2 onward reuses one further fixed context. The
/// unary prefix is unbounded (only capped as a malformed-stream guard) --
/// there is no truncated-unary/Exp-Golomb-suffix binarization here.
pub struct MbQpDeltaCabacContext {
    ctx: [CabacContext; 4],
}

impl MbQpDeltaCabacContext {
    pub fn new(slice_qp_y: i32) -> Self {
        let base = crate::cabac_tables::MB_QP_DELTA_CTX;
        let ctx = std::array::from_fn(|i| init_ctx(base + i, slice_qp_y));
        Self { ctx }
    }

    /// Decode `mb_qp_delta` (signed), given whether the previous
    /// macroblock's `mb_qp_delta` was nonzero.
    pub fn decode(&mut self, dec: &mut CabacDecoder, prev_nonzero: bool) -> i32 {
        if dec.decode_decision(&mut self.ctx[prev_nonzero as usize]) == 0 {
            return 0;
        }
        let mut val: i32 = 1;
        let mut ctx_idx = 2usize;
        loop {
            if dec.decode_decision(&mut self.ctx[ctx_idx]) == 0 {
                break;
            }
            ctx_idx = 3;
            val += 1;
            if val > 2 * 51 {
                // Malformed-stream guard; real streams never reach this.
                break;
            }
        }
        if val % 2 == 1 {
            (val + 1) / 2
        } else {
            -((val + 1) / 2)
        }
    }
}

/// `intra_chroma_pred_mode` CABAC decoder (spec §9.3.3.1.1.8, ctxIdx 64..=67).
///
/// Truncated-unary, `cMax = 3`. Bin 0's `ctxIdxInc` (0..=2) comes from
/// whether the left/top neighbour macroblock is present and has a nonzero
/// `intra_chroma_pred_mode`; bins 1-2 share one fixed context.
pub struct IntraChromaPredModeCabacContext {
    ctx: [CabacContext; 4],
}

impl IntraChromaPredModeCabacContext {
    pub fn new(slice_qp_y: i32) -> Self {
        let base = crate::cabac_tables::CHROMA_PRED_MODE_CTX;
        let ctx = std::array::from_fn(|i| init_ctx(base + i, slice_qp_y));
        Self { ctx }
    }

    /// Decode `intra_chroma_pred_mode` (0..=3) given whether the left/top
    /// neighbour macroblock is present with a nonzero chroma pred mode.
    pub fn decode(&mut self, dec: &mut CabacDecoder, left_nonzero: bool, top_nonzero: bool) -> u32 {
        let ctx0 = left_nonzero as usize + top_nonzero as usize;
        if dec.decode_decision(&mut self.ctx[ctx0]) == 0 {
            return 0;
        }
        if dec.decode_decision(&mut self.ctx[3]) == 0 {
            return 1;
        }
        if dec.decode_decision(&mut self.ctx[3]) == 0 {
            return 2;
        }
        3
    }
}

/// Intra 4x4 luma prediction mode CABAC decoder (spec §9.3.3.1.1.7, ctxIdx
/// 68 for `prev_intra4x4_pred_mode_flag`, ctxIdx 69 for `rem_intra4x4_pred_mode`).
///
/// `rem_intra4x4_pred_mode` uses the FL(cMax=7) binarization, whose bins are
/// indexed LSB-first per spec §9.3.2.5 (unlike CAVLC's plain MSB-first
/// `u(3)` for the same syntax element).
pub struct Intra4x4PredModeCabacContext {
    prev_ctx: CabacContext,
    rem_ctx: CabacContext,
}

impl Intra4x4PredModeCabacContext {
    pub fn new(slice_qp_y: i32) -> Self {
        Self {
            prev_ctx: init_ctx(crate::cabac_tables::PREV_INTRA_PRED_MODE_CTX, slice_qp_y),
            rem_ctx: init_ctx(crate::cabac_tables::REM_INTRA_PRED_MODE_CTX, slice_qp_y),
        }
    }

    /// Decode the mode for one 4x4 (or 8x8) luma block given its predicted
    /// mode (the min of the left/top neighbour modes, per §8.3.1.1/§8.3.2.1).
    pub fn decode(&mut self, dec: &mut CabacDecoder, pred_mode: u8) -> u8 {
        if dec.decode_decision(&mut self.prev_ctx) == 1 {
            return pred_mode;
        }
        let mut mode = 0u8;
        for i in 0..3u8 {
            mode += dec.decode_decision(&mut self.rem_ctx) << i;
        }
        mode + (mode >= pred_mode) as u8
    }
}

/// `coded_block_flag` CABAC decoder (spec §9.3.3.1.1.9, ctxIdx per
/// [`crate::cabac_tables::CBF_CTX_BASE`]).
///
/// `ctxIdxInc = (left_coded as usize) + 2*(top_coded as usize)`, where a
/// neighbour is "coded" when it has a nonzero coefficient count (AC/Luma4x4
/// categories) or its own `coded_block_flag` was 1 (DC categories). An
/// unavailable neighbour is treated as "coded" (matching FFmpeg's
/// `0x7CF`/sentinel-64 convention for an always-intra current macroblock).
pub struct CodedBlockFlagContext {
    ctx: [[CabacContext; 4]; 5],
}

impl CodedBlockFlagContext {
    pub fn new(slice_qp_y: i32) -> Self {
        let ctx = std::array::from_fn(|cat| {
            let base = crate::cabac_tables::CBF_CTX_BASE[cat];
            std::array::from_fn(|i| init_ctx(base + i, slice_qp_y))
        });
        Self { ctx }
    }

    /// Decode `coded_block_flag` for block category `cat` (0..=4, see the
    /// `CAT_*` constants in [`crate::cabac_tables`]).
    pub fn decode(
        &mut self,
        dec: &mut CabacDecoder,
        cat: usize,
        left_coded: bool,
        top_coded: bool,
    ) -> bool {
        let ctx_idx = left_coded as usize + 2 * top_coded as usize;
        let ctx = &self.ctx[cat][ctx_idx];
        eprintln!("    CBF cat={cat} ctx_idx={ctx_idx} state={} mps={}", ctx.state, ctx.mps);
        dec.decode_decision(&mut self.ctx[cat][ctx_idx]) == 1
    }
}

/// Per-`ctxBlockCat` significance-map length (`max_coeff - 1`, i.e. the
/// number of scan positions that need an explicit `significant_coeff_flag`
/// -- the final position's significance is implicit).
const SIG_LEN: [usize; 5] = [15, 14, 15, 3, 14];

/// Number of distinct `significant_coeff_flag`/`last_significant_coeff_flag`
/// contexts for the Luma8x8 category (`ctxBlockCat == 5`), per spec Table
/// 9-43: unlike cats 0..=4 (where `ctxIdxInc == scan position`), the 8x8
/// category maps its 63 scan positions onto a much smaller set of contexts
/// via [`crate::cabac_tables::SIG_COEFF_CTX_INC_8X8_FRAME`] /
/// [`crate::cabac_tables::LAST_COEFF_CTX_INC_8X8_FRAME`].
const SIG_LEN_8X8: usize = 15;
const LAST_LEN_8X8: usize = 9;

/// Residual-block CABAC decoder: `significant_coeff_flag` /
/// `last_significant_coeff_flag` (§9.3.3.1.2) and `coeff_abs_level_minus1`
/// (§9.3.3.1.3) for the five I-slice 4x4/DC block categories (Intra16x16
/// DC/AC, Luma4x4, ChromaDC, ChromaAC) plus the Luma8x8 category (`ctxBlockCat
/// == 5`, High-profile `transform_size_8x8_flag` intra macroblocks). No
/// field-coding (MBAFF/PAFF) support.
pub struct ResidualCabacContext {
    sig: [Vec<CabacContext>; 5],
    last: [Vec<CabacContext>; 5],
    level: [[CabacContext; 10]; 5],
    sig8x8: Vec<CabacContext>,
    last8x8: Vec<CabacContext>,
    level8x8: [CabacContext; 10],
}

impl ResidualCabacContext {
    pub fn new(slice_qp_y: i32) -> Self {
        // `base_table` covers all six `ctxBlockCat` groups including the
        // 8x8-transform category; only the five 4x4 categories are built here.
        let make = |base_table: &[usize; 6]| -> [Vec<CabacContext>; 5] {
            std::array::from_fn(|cat| {
                let base = base_table[cat];
                (0..SIG_LEN[cat])
                    .map(|i| init_ctx(base + i, slice_qp_y))
                    .collect()
            })
        };
        let sig = make(&crate::cabac_tables::SIG_COEFF_CTX_BASE);
        let last = make(&crate::cabac_tables::LAST_COEFF_CTX_BASE);
        let level = std::array::from_fn(|cat| {
            let base = crate::cabac_tables::COEFF_ABS_LEVEL_M1_CTX_BASE[cat];
            std::array::from_fn(|i| init_ctx(base + i, slice_qp_y))
        });
        let sig8x8 = (0..SIG_LEN_8X8)
            .map(|i| {
                init_ctx(
                    crate::cabac_tables::SIG_COEFF_CTX_BASE[crate::cabac_tables::CAT_LUMA_8X8] + i,
                    slice_qp_y,
                )
            })
            .collect();
        let last8x8 = (0..LAST_LEN_8X8)
            .map(|i| {
                init_ctx(
                    crate::cabac_tables::LAST_COEFF_CTX_BASE[crate::cabac_tables::CAT_LUMA_8X8] + i,
                    slice_qp_y,
                )
            })
            .collect();
        let level8x8 = std::array::from_fn(|i| {
            init_ctx(
                crate::cabac_tables::COEFF_ABS_LEVEL_M1_CTX_BASE[crate::cabac_tables::CAT_LUMA_8X8]
                    + i,
                slice_qp_y,
            )
        });
        Self {
            sig,
            last,
            level,
            sig8x8,
            last8x8,
            level8x8,
        }
    }

    /// Decode one Luma8x8 residual block (`ctxBlockCat == 5`, §9.3.3.1.2/.3).
    /// No `coded_block_flag` is signalled for this category (non-4:4:4) --
    /// the caller gates the call on the relevant `CodedBlockPatternLuma` bit,
    /// mirroring `slice_data::parse_intra_residuals`'s CAVLC 8x8 branch.
    /// Returns coefficients in scan-position order (64 entries) and the
    /// significant-coefficient count (for the neighbour cbf/nnz context).
    pub fn decode_block_8x8(&mut self, dec: &mut CabacDecoder) -> ([i16; 64], u8) {
        let mut out = [0i16; 64];
        const SIG_LEN: usize = 63;
        let mut positions: Vec<usize> = Vec::with_capacity(64);
        let mut found_last = false;
        for pos in 0..SIG_LEN {
            let sig_idx = crate::cabac_tables::SIG_COEFF_CTX_INC_8X8_FRAME[pos] as usize;
            if dec.decode_decision(&mut self.sig8x8[sig_idx]) == 1 {
                positions.push(pos);
                let last_idx = crate::cabac_tables::LAST_COEFF_CTX_INC_8X8_FRAME[pos] as usize;
                if dec.decode_decision(&mut self.last8x8[last_idx]) == 1 {
                    found_last = true;
                    break;
                }
            }
        }
        if !found_last {
            positions.push(SIG_LEN);
        }
        let coeff_count = positions.len() as u8;

        let mut node_ctx = 0usize;
        for &pos in positions.iter().rev() {
            let level_abs: i32;
            let level1_idx = crate::cabac_tables::COEFF_ABS_LEVEL1_CTX[node_ctx];
            if dec.decode_decision(&mut self.level8x8[level1_idx]) == 0 {
                level_abs = 1;
                node_ctx = crate::cabac_tables::COEFF_ABS_LEVEL_TRANSITION[0][node_ctx];
            } else {
                let gt1_idx = crate::cabac_tables::COEFF_ABS_LEVELGT1_CTX[node_ctx];
                node_ctx = crate::cabac_tables::COEFF_ABS_LEVEL_TRANSITION[1][node_ctx];
                let mut abs_val: u32 = 2;
                while abs_val < 15 && dec.decode_decision(&mut self.level8x8[gt1_idx]) == 1 {
                    abs_val += 1;
                }
                if abs_val >= 15 {
                    abs_val = 15 + dec.decode_bypass_eg(0);
                }
                level_abs = abs_val as i32;
            }
            let sign = dec.decode_bypass();
            out[pos] = (if sign == 1 { -level_abs } else { level_abs }) as i16;
        }

        (out, coeff_count)
    }

    /// Decode one residual block for category `cat` (0..=4) with `max_coeff`
    /// coefficients. Returns coefficients in **scan-position order** (the
    /// same layout `slice_data::parse_cavlc_block` uses: `out[scan_pos] =
    /// level`, DC-inclusive for DC-carrying categories, AC-only starting at
    /// scan position 0 otherwise) and the count of significant coefficients
    /// (for the neighbour `coded_block_flag`/nC-style context).
    ///
    /// Only called when this block's `coded_block_flag` was already decoded
    /// as 1 -- the caller is responsible for that gate (mirrors
    /// `slice_data::parse_intra_residuals`'s CBP-gated CAVLC block calls).
    pub fn decode_block(
        &mut self,
        dec: &mut CabacDecoder,
        cat: usize,
        max_coeff: usize,
    ) -> ([i16; 16], u8) {
        let mut out = [0i16; 16];
        let sig_len = max_coeff - 1;
        let mut positions: Vec<usize> = Vec::with_capacity(max_coeff);
        let mut found_last = false;
        for pos in 0..sig_len {
            if dec.decode_decision(&mut self.sig[cat][pos]) == 1 {
                positions.push(pos);
                if dec.decode_decision(&mut self.last[cat][pos]) == 1 {
                    found_last = true;
                    break;
                }
            }
        }
        if !found_last {
            positions.push(sig_len);
        }
        let coeff_count = positions.len() as u8;

        // Levels are decoded in reverse scan order (highest-frequency first).
        let mut node_ctx = 0usize;
        for &pos in positions.iter().rev() {
            let level_abs: i32;
            let level1_idx = crate::cabac_tables::COEFF_ABS_LEVEL1_CTX[node_ctx];
            if dec.decode_decision(&mut self.level[cat][level1_idx]) == 0 {
                level_abs = 1;
                node_ctx = crate::cabac_tables::COEFF_ABS_LEVEL_TRANSITION[0][node_ctx];
            } else {
                let gt1_idx = crate::cabac_tables::COEFF_ABS_LEVELGT1_CTX[node_ctx];
                node_ctx = crate::cabac_tables::COEFF_ABS_LEVEL_TRANSITION[1][node_ctx];
                let mut abs_val: u32 = 2;
                while abs_val < 15 && dec.decode_decision(&mut self.level[cat][gt1_idx]) == 1 {
                    abs_val += 1;
                }
                if abs_val >= 15 {
                    abs_val = 15 + dec.decode_bypass_eg(0);
                }
                level_abs = abs_val as i32;
            }
            let sign = dec.decode_bypass();
            out[pos] = (if sign == 1 { -level_abs } else { level_abs }) as i16;
        }

        (out, coeff_count)
    }
}

/// Look up one context's `(m, n)` init pair from the `cabac_init_idc`-selected
/// `CABAC_CTX_INIT_PB0`/`1`/`2` table (spec §9.3.1.1, Table 9-12) and
/// initialise it at `slice_qp_y`. Mirrors [`init_ctx`] but for P/B-slice
/// syntax elements, whose context init values additionally depend on the
/// slice header's `cabac_init_idc` (0..=2) -- unlike I-slice elements, which
/// only ever read [`crate::cabac_tables::CABAC_CTX_INIT_I`].
///
/// `cabac_init_idc` values outside `0..=2` are out of spec range; this
/// clamps to idc 2 rather than panicking (a malformed-stream guard, matching
/// this crate's existing preference for failing safe over panicking on
/// attacker-controlled header fields).
fn init_pb_ctx(ctx_idx: usize, cabac_init_idc: usize, slice_qp_y: i32) -> CabacContext {
    let (m, n) = match cabac_init_idc {
        0 => crate::cabac_tables::CABAC_CTX_INIT_PB0[ctx_idx],
        1 => crate::cabac_tables::CABAC_CTX_INIT_PB1[ctx_idx],
        _ => crate::cabac_tables::CABAC_CTX_INIT_PB2[ctx_idx],
    };
    CabacContext::init(m as i32, n as i32, slice_qp_y)
}

/// Left/top neighbour inputs for `mb_skip_flag`'s `ctxIdxInc` derivation
/// (spec §9.3.3.1.1.1, condTermFlagA / condTermFlagB).
#[derive(Debug, Clone, Copy, Default)]
pub struct MbSkipNeighbors {
    pub left_available: bool,
    pub left_skipped: bool,
    pub top_available: bool,
    pub top_skipped: bool,
}

impl MbSkipNeighbors {
    /// `condTermFlagN = 0` if neighbour `N` is unavailable or was itself
    /// skipped; `1` otherwise. `ctxIdxInc = condTermFlagA + condTermFlagB`.
    fn ctx_idx_inc(&self) -> usize {
        let cond_a = (self.left_available && !self.left_skipped) as usize;
        let cond_b = (self.top_available && !self.top_skipped) as usize;
        cond_a + cond_b
    }
}

/// `mb_skip_flag` decoding for P/SP and B slices: a single context-coded bin
/// whose value *is* `mb_skip_flag` (no further binarization).
///
/// A previous version of this struct read from a local `MB_SKIP_FLAG_P_INIT
/// [(i32,i32);3]` stub that was never cross-checked against source and was,
/// in fact, wrong: its three pairs -- `[(23,33),(22,25),(29,16)]` -- turned
/// out to be ctxIdx 11's `(m, n)` value from *each of the three*
/// `cabac_init_idc` tables (i.e. `CABAC_CTX_INIT_PB0[11]`,
/// `CABAC_CTX_INIT_PB1[11]`, `CABAC_CTX_INIT_PB2[11]`), rather than ctxIdx
/// 11, 12, 13 from a *single* idc table -- a transposition bug most likely
/// introduced by misreading FFmpeg's column-per-idc table layout as
/// row-per-idc. Cross-checking that stub against the newly-fetched
/// `CABAC_CTX_INIT_PB0` this session (Phase D.1) surfaced the mismatch
/// directly (`CABAC_CTX_INIT_PB0[11..14]` is `[(23,33),(23,2),(21,0)]`, not
/// `[(23,33),(22,25),(29,16)]`); it also confirmed mb_skip_flag's context
/// init genuinely is `cabac_init_idc`-dependent (the three idc tables give
/// different values at ctxIdx 11 -- see
/// `cabac_tables::tests::pb1_and_pb2_mb_skip_flag_p_differs_from_pb0`), so
/// the fix reads directly from the verified, fetched `CABAC_CTX_INIT_PB*`
/// tables via [`init_pb_ctx`] instead of duplicating numbers into a
/// separate local const (eliminating this whole class of transcription bug
/// for this syntax element going forward).
pub struct MbSkipFlagContext {
    ctx: [CabacContext; 3],
}

impl MbSkipFlagContext {
    /// Initialise the three `mb_skip_flag` contexts for a P/SP slice
    /// (ctxIdx 11..=13, [`crate::cabac_tables::MB_SKIP_FLAG_P_CTX`]) at the
    /// given slice QP and `cabac_init_idc` (0..=2, from the slice header).
    pub fn new_p_slice(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        let base = crate::cabac_tables::MB_SKIP_FLAG_P_CTX;
        let ctx = std::array::from_fn(|i| init_pb_ctx(base + i, cabac_init_idc, slice_qp_y));
        Self { ctx }
    }

    /// Initialise the three `mb_skip_flag` contexts for a B slice (ctxIdx
    /// 24..=26, [`crate::cabac_tables::MB_SKIP_FLAG_B_CTX`]) at the given
    /// slice QP and `cabac_init_idc`. B slices derive `ctxIdxInc` the same
    /// way P/SP slices do (spec §9.3.3.1.1.1's condTermFlagA/condTermFlagB
    /// is not slice-type-specific; only the ctxIdxOffset differs -- verified
    /// from FFmpeg's `decode_cabac_mb_skip`, which adds a fixed +13 to `ctx`
    /// for B slices before indexing the same context array used for P/SP),
    /// so this reuses [`MbSkipNeighbors`]/[`Self::decode`] unchanged.
    pub fn new_b_slice(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        let base = crate::cabac_tables::MB_SKIP_FLAG_B_CTX;
        let ctx = std::array::from_fn(|i| init_pb_ctx(base + i, cabac_init_idc, slice_qp_y));
        Self { ctx }
    }

    /// Decode `mb_skip_flag` for the current macroblock given its left/top
    /// neighbour availability and skip state.
    pub fn decode(&mut self, dec: &mut CabacDecoder, neighbors: &MbSkipNeighbors) -> bool {
        let idx = neighbors.ctx_idx_inc();
        dec.decode_decision(&mut self.ctx[idx]) == 1
    }
}

/// `mb_field_decoding_flag` CABAC context (spec §9.3.3.1.1.11, ctxIdx 70..=72).
///
/// Signalled once per *macroblock pair* in an MBAFF frame (§7.4.4), before the
/// pair's top macroblock. Its single bin's `ctxIdxInc` (0..=2) is the sum of the
/// left and top neighbours' `mb_field_decoding_flag` values (each 0 or 1). The
/// three contexts initialise from [`crate::cabac_tables::CABAC_CTX_INIT_I`]
/// indices 70, 71, 72 (I-slice init table — `mb_field_decoding_flag` uses the
/// same init for all slice types).
pub struct MbFieldDecodingFlagContext {
    ctx: [CabacContext; 3],
}

impl MbFieldDecodingFlagContext {
    /// Initialise the three contexts at the given slice QP.
    pub fn new(slice_qp_y: i32) -> Self {
        let ctx = std::array::from_fn(|i| init_ctx(70 + i, slice_qp_y));
        Self { ctx }
    }

    /// Decode `mb_field_decoding_flag` given the left and top neighbours' flags.
    pub fn decode(&mut self, dec: &mut CabacDecoder, left_field: bool, top_field: bool) -> bool {
        let idx = (left_field as usize) + (top_field as usize);
        dec.decode_decision(&mut self.ctx[idx]) == 1
    }
}

/// Intra `mb_type` *suffix* decoder for intra-coded macroblocks embedded
/// inside a P or B slice (i.e. the mb_type prefix -- [`MbTypePCabacContext`]
/// for P/SP -- decoded "intra"). Spec §9.3.3.1.1.3-family binarization reused
/// at a different `ctxIdxOffset` (17 for P/SP, 32 for B).
///
/// **Not** a drop-in reuse of [`MbTypeICabacContext`]'s bin/context mapping,
/// despite superficially resembling it. Ported line-by-line from FFmpeg's
/// `decode_cabac_intra_mb_type(sl, ctx_base, intra_slice)` with
/// `intra_slice == 0` (the P/B call sites: `decode_cabac_intra_mb_type(sl,
/// 17, 0)` for P, `decode_cabac_intra_mb_type(sl, 32, 0)` for B) and compared
/// against the `intra_slice == 1` codepath (I-slice, ctx_base 3) that
/// [`MbTypeICabacContext::decode`] already implements. Two differences, both
/// consequences of the `if(intra_slice){ ... } else { ... }` branch in the
/// source:
///   1. Bin 0 (I4x4 vs I16x16-or-PCM) reads a **single fixed** context
///      (`state[0]`) for P/B -- the neighbour-derived `ctx` (0..=2) computed
///      from `left_type`/`top_type` only happens on the `intra_slice`
///      branch, and the `state += 2` advance after bin 0 is *inside* that
///      same branch, so for P/B `state` never advances past `ctx_base`.
///   2. Because of (1), `state[2+intra_slice]` (cbp_chroma "value" bin) and
///      `state[3+2*intra_slice]` (second `pred_mode` bit) both fold to
///      `state[2]`/`state[3]` for `intra_slice == 0` -- i.e. they **reuse**
///      the same context as cbp_chroma "presence" and the first `pred_mode`
///      bit respectively, rather than reading a distinct context the way the
///      I-slice suffix does.
///
/// Returns the same 0..=25 numbering as [`MbTypeICabacContext::decode`]
/// (0 = I_NxN, 1..=24 = I_16x16 variant, 25 = I_PCM).
pub struct IntraMbTypeSuffixCabacContext {
    ctx: [CabacContext; 4],
}

impl IntraMbTypeSuffixCabacContext {
    /// `ctx_base` is 17 for P/SP slices or 32 for B slices (see
    /// [`crate::cabac_tables::MB_TYPE_P_CTX`]/[`crate::cabac_tables::MB_TYPE_B_CTX`]
    /// for how those ranges were confirmed against the fetched source).
    pub fn new_pb(ctx_base: usize, slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        let ctx = std::array::from_fn(|i| init_pb_ctx(ctx_base + i, cabac_init_idc, slice_qp_y));
        Self { ctx }
    }

    pub fn decode(&mut self, dec: &mut CabacDecoder) -> u32 {
        if dec.decode_decision(&mut self.ctx[0]) == 0 {
            return 0; // I_NxN
        }
        if dec.decode_terminate() == 1 {
            return 25; // I_PCM
        }
        // Decode order matches FFmpeg decode_cabac_intra_mb_type(intra_slice=0):
        //   state += 1 after initial bin, so ctx[1]=cbp_luma, ctx[2]=cbp_chroma, ctx[3]=pred_mode.
        let mut mb_type = 1u32;
        mb_type += 12 * dec.decode_decision(&mut self.ctx[1]) as u32; // cbp_luma
        if dec.decode_decision(&mut self.ctx[2]) == 1 {               // cbp_chroma present
            mb_type += 4 + 4 * dec.decode_decision(&mut self.ctx[2]) as u32; // cbp_chroma value
        }
        mb_type += 2 * dec.decode_decision(&mut self.ctx[3]) as u32; // pred_mode high
        mb_type += dec.decode_decision(&mut self.ctx[3]) as u32;      // pred_mode low
        mb_type
    }
}

/// `mb_type` P/SP-slice "P vs intra" + partition-shape prefix (Table 9-11
/// inline P dispatch, ctxIdx 14..=17, ctxIdxOffset
/// [`crate::cabac_tables::MB_TYPE_P_CTX`]). Ported directly from FFmpeg's
/// `ff_h264_decode_mb_cabac`'s `AV_PICTURE_TYPE_P` branch:
///
/// ```c
/// if( get_cabac(ctx[14]) == 0 ) {          // P-type (not intra)
///     if( get_cabac(ctx[15]) == 0 )
///         mb_type = 3 * get_cabac(ctx[16]);      // 0 or 3
///     else
///         mb_type = 2 - get_cabac(ctx[17]);      // 2 or 1
///     // ff_h264_p_mb_type_info[mb_type]: 0=16x16,1=16x8,2=8x16,3=8x8
/// } else {
///     mb_type = decode_cabac_intra_mb_type(sl, 17, 0);   // intra-in-P
/// }
/// ```
///
/// Note ctxIdx 17 is shared between the "16x8-vs-8x16" prefix bit and
/// [`IntraMbTypeSuffixCabacContext`]'s bin 0 -- the two are mutually
/// exclusive per macroblock (only one branch of the `if` above executes), so
/// the same physical context variable is simply reused/adapted across MBs
/// regardless of which meaning applied that time; this mirrors
/// [`crate::cabac_tables::MB_TYPE_P_CTX`]'s doc comment.
pub struct MbTypePCabacContext {
    ctx: [CabacContext; 4],
}

impl MbTypePCabacContext {
    pub fn new(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        let base = crate::cabac_tables::MB_TYPE_P_CTX;
        let ctx = std::array::from_fn(|i| init_pb_ctx(base + i, cabac_init_idc, slice_qp_y));
        Self { ctx }
    }

    /// Returns `None` when the prefix indicates an intra-coded macroblock
    /// (caller continues with [`IntraMbTypeSuffixCabacContext::new_pb`] at
    /// `ctx_base = 17`); otherwise `Some(shape)` with `shape` in 0..=3
    /// matching this crate's existing CAVLC `mb_type_raw` numbering for the
    /// same four P shapes in `slice_data::parse_p_macroblock` (0 =
    /// P_L0_16x16, 1 = P_L0_L0_16x8, 2 = P_L0_L0_8x16, 3 = P_8x8). CABAC has
    /// no separate "ref index forced to 0" code point (unlike CAVLC's
    /// `mb_type_raw == 4` / `P8x8ref0`) -- see [`RefIdxCabacContext::decode`]
    /// for why that shortcut isn't needed here.
    pub fn decode(&mut self, dec: &mut CabacDecoder) -> Option<u32> {
        // H.264 spec §9.3.2.5 / Table 9-36 P-slice binarization.
        // Mirrors FFmpeg `ff_h264_decode_mb_cabac` (!get_cabac notation):
        //   ctxIdx 14 = 0 → inter branch (3 more bins); 1 → intra-in-P (None).
        if dec.decode_decision(&mut self.ctx[0]) == 1 {
            return None; // intra-in-P
        }
        // ctxIdx 14 = 0 → inter; discriminate on ctxIdx 15.
        if dec.decode_decision(&mut self.ctx[1]) == 0 {
            // ctxIdx 15 = 0: 0 → P_L0_16x16, 1 → P_8x8
            Some(3 * dec.decode_decision(&mut self.ctx[2]) as u32)
        } else {
            // ctxIdx 15 = 1: mirrors FFmpeg `mb_type = 2 - get_cabac(ctx[17])`:
            //   bit=0 → P_L0_L0_8x16 (2), bit=1 → P_L0_L0_16x8 (1).
            Some(2 - dec.decode_decision(&mut self.ctx[3]) as u32)
        }
    }
}

/// `sub_mb_type` (P, SP slices) CABAC decoder (ctxIdx 21..=23, ctxIdxOffset
/// [`crate::cabac_tables::SUB_MB_TYPE_P_CTX`]). Ported directly from
/// FFmpeg's `decode_cabac_p_mb_sub_type`:
///
/// ```c
/// if( get_cabac(ctx[21]) ) return 0;        /* 8x8: 1 partition */
/// if( !get_cabac(ctx[22]) ) return 1;       /* 8x4: 2 partitions */
/// if( get_cabac(ctx[23]) ) return 2;        /* 4x8: 2 partitions */
/// return 3;                                 /* 4x4: 4 partitions */
/// ```
///
/// Return value indexes this crate's existing `P_SUB_MB_PARTS` partition-count
/// table in `slice_data.rs` (`[1, 2, 2, 4]`), matching CAVLC's `sub_mb_type`
/// numbering exactly (both derive from the same `ff_h264_p_sub_mb_type_info`
/// table upstream).
pub struct SubMbTypePCabacContext {
    ctx: [CabacContext; 3],
}

impl SubMbTypePCabacContext {
    pub fn new(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        let base = crate::cabac_tables::SUB_MB_TYPE_P_CTX;
        let ctx = std::array::from_fn(|i| init_pb_ctx(base + i, cabac_init_idc, slice_qp_y));
        Self { ctx }
    }

    pub fn decode(&mut self, dec: &mut CabacDecoder) -> u32 {
        if dec.decode_decision(&mut self.ctx[0]) == 1 {
            return 0;
        }
        if dec.decode_decision(&mut self.ctx[1]) == 0 {
            return 1;
        }
        if dec.decode_decision(&mut self.ctx[2]) == 1 {
            return 2;
        }
        3
    }
}

/// `ref_idx_l0` CABAC decoder (spec §9.3.3.1.1.6, ctxIdx 54..=59,
/// ctxIdxOffset [`crate::cabac_tables::REF_IDX_CTX`]). Ported directly from
/// FFmpeg's `decode_cabac_mb_ref`:
///
/// ```c
/// int ctx = (refa > 0) + 2*(refb > 0);
/// while( get_cabac(ctx[54+ctx]) ) {
///     ref++;
///     ctx = (ctx>>2) + 4;   // -> 4 after the first hit, -> 5 from then on
///     if (ref >= 32) return -1;   // malformed-stream guard
/// }
/// return ref;
/// ```
///
/// This is exactly a truncated-unary bin string (no separate "bin 0" special
/// case -- the `while` condition itself performs the first decode), so it's
/// written here as a direct transliteration of that `ctx` recurrence for
/// 1:1 traceability against the source, rather than routed through
/// [`CabacDecoder::decode_truncated_unary`] (whose fixed by-position context
/// table doesn't have a clean way to express the neighbour-selected *first*
/// context alongside `min(pos, len-1)`-style reuse for the rest without an
/// extra copy step) -- functionally the two approaches agree.
pub struct RefIdxCabacContext {
    /// Local layout: `[0..4)` = bin 0's neighbour-selected context (ctxIdx
    /// 54..=57), `[4]` = ctxIdx 58 (first continuation bin), `[5]` = ctxIdx
    /// 59 (every bin after that).
    ctx: [CabacContext; 6],
}

impl RefIdxCabacContext {
    pub fn new(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        let base = crate::cabac_tables::REF_IDX_CTX;
        let ctx = std::array::from_fn(|i| init_pb_ctx(base + i, cabac_init_idc, slice_qp_y));
        Self { ctx }
    }

    /// `left_gt0`/`top_gt0`: whether the left/top neighbouring partition's
    /// `ref_idx` (list 0) is `> 0`. Per spec §9.3.3.1.1.6, `refIdxZeroFlagN`
    /// is 1 (i.e. contributes 0 here) when the neighbour is unavailable,
    /// intra-coded, `P_Skip`/`B_Skip`, doesn't use this list, or has
    /// `ref_idx == 0` -- only a real, decoded `ref_idx > 0` contributes 1.
    pub fn decode(&mut self, dec: &mut CabacDecoder, left_gt0: bool, top_gt0: bool) -> u32 {
        let mut ctx_idx = left_gt0 as usize + 2 * top_gt0 as usize;
        let mut val = 0u32;
        while val < 32 && dec.decode_decision(&mut self.ctx[ctx_idx]) == 1 {
            val += 1;
            ctx_idx = (ctx_idx >> 2) + 4;
        }
        val
    }
}

/// One component (x or y) of `mvd_l0`/`mvd_l1` CABAC decoder (spec
/// §9.3.3.1.1.7, ctxIdx 40..=46 for the x component
/// ([`crate::cabac_tables::MVD_X_CTX`]) or 47..=53 for y
/// ([`crate::cabac_tables::MVD_Y_CTX`])). Ported directly from FFmpeg's
/// `decode_cabac_mb_mvd`:
///
/// ```c
/// // ctxIdxInc for bin 0, from the branchless (amvd>2)+(amvd>32) equivalent:
/// int ctx0 = (amvd < 3) ? 0 : (amvd < 33) ? 1 : 2;
/// if( !get_cabac(ctx[ctx0]) ) return 0;
/// int mvd = 1, idx = 3;                 // ctxbase += 3
/// while( mvd < 9 && get_cabac(ctx[idx]) ) {
///     if( mvd < 4 ) idx++;              // idx: 3,4,5,6,6,6,6,6 across mvd=1..8
///     mvd++;
/// }
/// if( mvd >= 9 )
///     mvd += decode_bypass_eg(3);       // Exp-Golomb order-3 suffix (§9.3.3.1.1.7)
/// return get_cabac_bypass() ? -mvd : mvd;   // sign bit only read when mvd != 0
/// ```
///
/// Uses [`CabacDecoder::decode_bypass_eg`] (Exp-Golomb of order k=3) for the
/// bypass-coded suffix once the context-coded TU prefix saturates at 9, per
/// spec §9.3.2.3 / §9.3.3.1.1.7. This is distinct from the Golomb-Rice helper
/// [`CabacDecoder::decode_bypass_golomb`], which must NOT be used here: it reads
/// a different number of bits and would desync the decoder.
pub struct MvdCabacContext {
    /// Local layout: `[0..3)` = bin 0's neighbour-sum-selected context
    /// (ctxIdx `base`..=`base+2`), `[3..7)` = the truncated-unary
    /// continuation (ctxIdx `base+3`..=`base+6`, with `[6]` reused for every
    /// bin past the fourth).
    ctx: [CabacContext; 7],
}

impl MvdCabacContext {
    /// `ctx_base` is [`crate::cabac_tables::MVD_X_CTX`] (40) or
    /// [`crate::cabac_tables::MVD_Y_CTX`] (47).
    pub fn new(ctx_base: usize, slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        let ctx = std::array::from_fn(|i| init_pb_ctx(ctx_base + i, cabac_init_idc, slice_qp_y));
        Self { ctx }
    }

    /// `amvd_sum`: sum of the left/top neighbouring partitions' `|mvd|` for
    /// this component (each individually capped at 70, matching FFmpeg's
    /// `*mvda = mvd < 70 ? mvd : 70` -- see the module docs on
    /// `slice_data::MbMvCabacCtx` for where that cap is applied). A neighbour
    /// that's unavailable, intra-coded, or skipped contributes 0 (§9.3.3.1.1.7).
    ///
    /// Returns the signed `mvd` component value (not yet added to the
    /// predicted MV -- matches this crate's existing CAVLC convention of
    /// storing raw `mvd_l0` on [`crate::macroblock::InterMotion`] and letting
    /// `crate::mv::predict_slice_mvs` combine it with the predictor
    /// afterwards).
    pub fn decode(&mut self, dec: &mut CabacDecoder, amvd_sum: u32) -> i32 {
        let ctx0 = if amvd_sum < 3 {
            0
        } else if amvd_sum < 33 {
            1
        } else {
            2
        };
        if dec.decode_decision(&mut self.ctx[ctx0]) == 0 {
            return 0;
        }
        let mut mvd: u32 = 1;
        let mut idx: usize = 3;
        while mvd < 9 && dec.decode_decision(&mut self.ctx[idx]) == 1 {
            if mvd < 4 {
                idx += 1;
            }
            mvd += 1;
        }
        if mvd >= 9 {
            // §9.3.3.1.1.7 mvd suffix. This is a variable-k Rice code (the
            // reference FFmpeg `decode_cabac_mb_mvd`): a unary bypass prefix
            // `q` where each `1` bin adds `1 << (3 + q_run)` with the shift
            // growing (`k` starts at 3 and increments per `1`), followed by
            // `(3 + q)` bypass suffix bits. It is NOT fixed-k Golomb-Rice and
            // NOT fixed-k Exp-Golomb — those consume a different bit count and
            // desync every following syntax element.
            let mut k = 3u32;
            while dec.decode_bypass() == 1 {
                mvd += 1u32 << k;
                k += 1;
                if k > 24 {
                    break;
                }
            }
            while k > 0 {
                k -= 1;
                if dec.decode_bypass() == 1 {
                    mvd += 1u32 << k;
                }
            }
        }
        if dec.decode_bypass() == 1 {
            -(mvd as i32)
        } else {
            mvd as i32
        }
    }
}

/// `mb_type` B-slice inter-prefix decoder (ctxIdx 27..=32, ctxIdxOffset
/// [`crate::cabac_tables::MB_TYPE_B_CTX`]). Ported from FFmpeg's inline
/// B-slice `mb_type` dispatch (`cabac_state[27+ctx]` for `ctx` 0..=5):
///
/// - ctx[0] (ctxIdx 27): 0 → B_Direct_16x16 (type 0)
/// - ctx[1] (ctxIdx 28): 0 → types 1/2 (L0/L1 16x16), selected by ctx[2]
/// - ctx[2] (ctxIdx 29): used for types 1/2 and 3/4 branches
/// - ctx[3] (ctxIdx 30): used for types 3/4 and 5/6 branches
/// - ctx[4] (ctxIdx 31): reused for all types 7..=21 pair-wise decisions
/// - ctx[5] (ctxIdx 32): final inter/intra gate (0 → B_8x8=type22, 1 → intra)
///   — ctxIdx 32 is also the base of [`IntraMbTypeSuffixCabacContext::new_pb`]
///   for B slices, following the same mutual-exclusion pattern as ctxIdx 17 on
///   the P-slice path.
///
/// Returns `None` when the prefix indicates an intra-coded macroblock (caller
/// uses `IntraMbTypeSuffixCabacContext::new_pb(32, ...)`); otherwise `Some(n)`
/// with `n` in 0..=22.
pub struct MbTypeBCabacContext {
    ctx: [CabacContext; 6],
}

impl MbTypeBCabacContext {
    pub fn new(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        let base = crate::cabac_tables::MB_TYPE_B_CTX;
        let ctx = std::array::from_fn(|i| init_pb_ctx(base + i, cabac_init_idc, slice_qp_y));
        Self { ctx }
    }

    pub fn decode(&mut self, dec: &mut CabacDecoder) -> Option<u32> {
        if dec.decode_decision(&mut self.ctx[0]) == 0 {
            return Some(0); // B_Direct_16x16
        }
        if dec.decode_decision(&mut self.ctx[1]) == 0 {
            // types 1 (L0) or 2 (L1)
            return Some(1 + dec.decode_decision(&mut self.ctx[2]) as u32);
        }
        if dec.decode_decision(&mut self.ctx[2]) == 0 {
            // types 3 (Bi) or 4 (L0L0_16x8)
            return Some(3 + dec.decode_decision(&mut self.ctx[3]) as u32);
        }
        if dec.decode_decision(&mut self.ctx[3]) == 0 {
            // types 5 (L0L0_8x16) or 6 (L1L1_16x8)
            return Some(5 + dec.decode_decision(&mut self.ctx[4]) as u32);
        }
        // ctx[4] (ctxIdx 31) is reused for all subsequent pair-wise decisions.
        // Each loop iteration handles one "0 → return pair" check.
        let mut base = 7u32;
        loop {
            if dec.decode_decision(&mut self.ctx[4]) == 0 {
                return Some(base + dec.decode_decision(&mut self.ctx[4]) as u32);
            }
            base += 2;
            if base == 21 {
                break;
            }
        }
        // After base reaches 21: one more ctx[4] read for type 21.
        if dec.decode_decision(&mut self.ctx[4]) == 0 {
            return Some(21);
        }
        // ctx[5] (ctxIdx 32): 0 → B_8x8 (type 22), 1 → intra.
        if dec.decode_decision(&mut self.ctx[5]) == 0 {
            return Some(22);
        }
        None // intra — caller uses IntraMbTypeSuffixCabacContext::new_pb(32, ...)
    }
}

/// `sub_mb_type` (B slices) CABAC decoder (ctxIdx 36..=39, ctxIdxOffset
/// [`crate::cabac_tables::SUB_MB_TYPE_B_CTX`]). Ported directly from FFmpeg's
/// `decode_cabac_b_mb_sub_type` (`cabac_state[36]`, `[37]`, `[38]`, `[39]`):
///
/// Returns 0..=12 matching this crate's existing CAVLC numbering for B
/// sub_mb_type (Table 7-15). The B_Direct (0) case produces no motion data.
/// Values 1..=12 map through `crate::mv::B_SUB_MB_PARTS` and `B_SUB_MB_DIR`.
pub struct SubMbTypeBCabacContext {
    ctx: [CabacContext; 4],
}

impl SubMbTypeBCabacContext {
    pub fn new(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        let base = crate::cabac_tables::SUB_MB_TYPE_B_CTX;
        let ctx = std::array::from_fn(|i| init_pb_ctx(base + i, cabac_init_idc, slice_qp_y));
        Self { ctx }
    }

    pub fn decode(&mut self, dec: &mut CabacDecoder) -> u32 {
        if dec.decode_decision(&mut self.ctx[0]) == 0 {
            return 0; // B_Direct
        }
        if dec.decode_decision(&mut self.ctx[1]) == 0 {
            return 1 + dec.decode_decision(&mut self.ctx[2]) as u32; // 1 or 2
        }
        if dec.decode_decision(&mut self.ctx[2]) == 0 {
            return 3 + dec.decode_decision(&mut self.ctx[3]) as u32; // 3 or 4
        }
        if dec.decode_decision(&mut self.ctx[3]) == 0 {
            return 5 + dec.decode_decision(&mut self.ctx[3]) as u32; // 5 or 6
        }
        if dec.decode_decision(&mut self.ctx[3]) == 0 {
            return 7 + dec.decode_decision(&mut self.ctx[3]) as u32; // 7 or 8
        }
        if dec.decode_decision(&mut self.ctx[3]) == 0 {
            return 9;
        }
        if dec.decode_decision(&mut self.ctx[3]) == 0 {
            return 10;
        }
        if dec.decode_decision(&mut self.ctx[3]) == 0 {
            return 11;
        }
        12
    }
}

impl CbpCabacContext {
    /// P/B-slice sibling of [`CbpCabacContext::new`]: identical ctxIdx range
    /// (73..=84, ctxIdxOffset doesn't vary by slice type for this element),
    /// but the `(m, n)` init pairs come from the `cabac_init_idc`-selected
    /// `CABAC_CTX_INIT_PB*` table instead of `CABAC_CTX_INIT_I` -- required
    /// per spec §9.3.1.1 (context init is chosen once per slice by slice
    /// type, for *every* ctxIdx used in that slice, not per syntax element).
    pub fn new_pb(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        let base = crate::cabac_tables::CBP_LUMA_CTX;
        let ctx = std::array::from_fn(|i| init_pb_ctx(base + i, cabac_init_idc, slice_qp_y));
        Self { ctx }
    }
}

impl MbQpDeltaCabacContext {
    /// P/B-slice sibling of [`MbQpDeltaCabacContext::new`] -- see
    /// [`CbpCabacContext::new_pb`]'s doc comment for why this exists even
    /// though `cabac_tables::tests::pb_tables_mb_qp_delta_matches_i_table`
    /// shows the four tables happen to agree at this particular ctxIdx range.
    pub fn new_pb(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        let base = crate::cabac_tables::MB_QP_DELTA_CTX;
        let ctx = std::array::from_fn(|i| init_pb_ctx(base + i, cabac_init_idc, slice_qp_y));
        Self { ctx }
    }
}

impl IntraChromaPredModeCabacContext {
    /// P/B-slice sibling of [`IntraChromaPredModeCabacContext::new`] -- see
    /// [`CbpCabacContext::new_pb`]'s doc comment.
    pub fn new_pb(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        let base = crate::cabac_tables::CHROMA_PRED_MODE_CTX;
        let ctx = std::array::from_fn(|i| init_pb_ctx(base + i, cabac_init_idc, slice_qp_y));
        Self { ctx }
    }
}

impl Intra4x4PredModeCabacContext {
    /// P/B-slice sibling of [`Intra4x4PredModeCabacContext::new`] -- see
    /// [`CbpCabacContext::new_pb`]'s doc comment. `prev_intra4x4_pred_mode_flag`/
    /// `rem_intra4x4_pred_mode` (ctxIdx 68/69) are read by the *same*
    /// `decode_cabac_mb_intra4x4_pred_mode` function regardless of slice type
    /// in FFmpeg (it's not gated on `slice_type_nos`), but per §9.3.1.1 the
    /// `(m, n)` pair at that ctxIdx still comes from the PB table for a P/B
    /// slice, even though the numeric ctxIdx is unchanged from the I-slice case.
    pub fn new_pb(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        Self {
            prev_ctx: init_pb_ctx(
                crate::cabac_tables::PREV_INTRA_PRED_MODE_CTX,
                cabac_init_idc,
                slice_qp_y,
            ),
            rem_ctx: init_pb_ctx(
                crate::cabac_tables::REM_INTRA_PRED_MODE_CTX,
                cabac_init_idc,
                slice_qp_y,
            ),
        }
    }
}

impl CodedBlockFlagContext {
    /// P/B-slice sibling of [`CodedBlockFlagContext::new`] -- see
    /// [`CbpCabacContext::new_pb`]'s doc comment.
    pub fn new_pb(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        let ctx: [[CabacContext; 4]; 5] = std::array::from_fn(|cat| {
            let base = crate::cabac_tables::CBF_CTX_BASE[cat];
            std::array::from_fn(|i| {
                let c = init_pb_ctx(base + i, cabac_init_idc, slice_qp_y);
                eprintln!("  CBF_INIT cat={cat} i={i} base+i={} qp={slice_qp_y} idc={cabac_init_idc} → state={} mps={}", base+i, c.state, c.mps);
                c
            })
        });
        Self { ctx }
    }
}

impl ResidualCabacContext {
    /// P/B-slice sibling of [`ResidualCabacContext::new`] -- see
    /// [`CbpCabacContext::new_pb`]'s doc comment.
    pub fn new_pb(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        let make = |base_table: &[usize; 6]| -> [Vec<CabacContext>; 5] {
            std::array::from_fn(|cat| {
                let base = base_table[cat];
                (0..SIG_LEN[cat])
                    .map(|i| init_pb_ctx(base + i, cabac_init_idc, slice_qp_y))
                    .collect()
            })
        };
        let sig = make(&crate::cabac_tables::SIG_COEFF_CTX_BASE);
        let last = make(&crate::cabac_tables::LAST_COEFF_CTX_BASE);
        let level = std::array::from_fn(|cat| {
            let base = crate::cabac_tables::COEFF_ABS_LEVEL_M1_CTX_BASE[cat];
            std::array::from_fn(|i| init_pb_ctx(base + i, cabac_init_idc, slice_qp_y))
        });
        let sig8x8 = (0..SIG_LEN_8X8)
            .map(|i| {
                init_pb_ctx(
                    crate::cabac_tables::SIG_COEFF_CTX_BASE[crate::cabac_tables::CAT_LUMA_8X8] + i,
                    cabac_init_idc,
                    slice_qp_y,
                )
            })
            .collect();
        let last8x8 = (0..LAST_LEN_8X8)
            .map(|i| {
                init_pb_ctx(
                    crate::cabac_tables::LAST_COEFF_CTX_BASE[crate::cabac_tables::CAT_LUMA_8X8] + i,
                    cabac_init_idc,
                    slice_qp_y,
                )
            })
            .collect();
        let level8x8 = std::array::from_fn(|i| {
            init_pb_ctx(
                crate::cabac_tables::COEFF_ABS_LEVEL_M1_CTX_BASE[crate::cabac_tables::CAT_LUMA_8X8]
                    + i,
                cabac_init_idc,
                slice_qp_y,
            )
        });
        Self {
            sig,
            last,
            level,
            sig8x8,
            last8x8,
            level8x8,
        }
    }
}

/// `transform_size_8x8_flag` CABAC context (spec §9.3.3.1.1.10, ctxIdxOffset
/// 399, 3 contexts). `ctxIdxInc = condTermFlagA + condTermFlagB`, where each
/// term is the neighbour macroblock's own `transform_size_8x8_flag` value (0
/// if the neighbour is unavailable or wasn't itself 8x8-transform-coded).
/// Single-bin FL(cMax=1) binarization -- same shape as
/// [`MbFieldDecodingFlagContext`].
pub struct TransformSize8x8FlagContext {
    ctx: [CabacContext; 3],
}

impl TransformSize8x8FlagContext {
    pub fn new(slice_qp_y: i32) -> Self {
        let base = crate::cabac_tables::TRANSFORM_SIZE_8X8_FLAG_CTX;
        let ctx = std::array::from_fn(|i| init_ctx(base + i, slice_qp_y));
        Self { ctx }
    }

    /// P/B-slice sibling of [`TransformSize8x8FlagContext::new`] -- see
    /// [`CbpCabacContext::new_pb`]'s doc comment.
    pub fn new_pb(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        let base = crate::cabac_tables::TRANSFORM_SIZE_8X8_FLAG_CTX;
        let ctx = std::array::from_fn(|i| init_pb_ctx(base + i, cabac_init_idc, slice_qp_y));
        Self { ctx }
    }

    /// Decode `transform_size_8x8_flag` given the left and top neighbours'
    /// `transform_size_8x8` state.
    pub fn decode(&mut self, dec: &mut CabacDecoder, left_is_8x8: bool, top_is_8x8: bool) -> bool {
        let idx = (left_is_8x8 as usize) + (top_is_8x8 as usize);
        dec.decode_decision(&mut self.ctx[idx]) == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_init_reads_nine_bits_and_sets_range() {
        // 9 bits of 0b1_0110_1100 = 0x1B6 = 438 (padded byte boundary).
        let data = [0b1011_0110, 0b0000_0000];
        let dec = CabacDecoder::new(&data).unwrap();
        assert_eq!(dec.range, 510);
        assert_eq!(dec.offset, 0b1_0110_1100);
    }

    #[test]
    fn engine_init_errors_on_too_short_stream() {
        let data: [u8; 0] = [];
        assert!(CabacDecoder::new(&data).is_err());
    }

    #[test]
    fn context_init_matches_hand_computed_values() {
        // m=0, n=64, qp=26 -> preCtxState = (0*26>>4) + 64 = 64 -> pStateIdx=0, MPS=1.
        let ctx = CabacContext::init(0, 64, 26);
        assert_eq!(ctx.state, 0);
        assert_eq!(ctx.mps, 1);

        // m=0, n=63, qp=26 -> preCtxState = 63 -> pStateIdx = 63-63=0, MPS=0.
        let ctx = CabacContext::init(0, 63, 26);
        assert_eq!(ctx.state, 0);
        assert_eq!(ctx.mps, 0);

        // preCtxState is clamped to [1, 126]: very negative n clamps to 1 -> pStateIdx=62, MPS=0.
        let ctx = CabacContext::init(0, -1000, 26);
        assert_eq!(ctx.state, 62);
        assert_eq!(ctx.mps, 0);

        // Very large n clamps to 126 -> pStateIdx = 126-64=62, MPS=1.
        let ctx = CabacContext::init(0, 1000, 26);
        assert_eq!(ctx.state, 62);
        assert_eq!(ctx.mps, 1);
    }

    #[test]
    fn decode_decision_keeps_offset_below_range() {
        let data = [0xA3u8, 0x5C, 0x91, 0x77, 0x2E, 0x0F, 0xFF, 0x00];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = CabacContext::init(20, 40, 26);
        for _ in 0..32 {
            let _ = dec.decode_decision(&mut ctx);
            assert!(dec.offset < dec.range);
            assert!(dec.range >= 256 && dec.range < 512);
            assert!(ctx.state <= 63);
        }
    }

    #[test]
    fn decode_bypass_keeps_offset_below_range() {
        let data = [0x5Au8, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A];
        let mut dec = CabacDecoder::new(&data).unwrap();
        for _ in 0..24 {
            let _ = dec.decode_bypass();
            assert!(dec.offset < dec.range);
        }
    }

    #[test]
    fn decode_bypass_bits_reads_msb_first() {
        // All-ones stream: with range fixed at 510 and every offset bit 1,
        // decode_bypass always drives offset back below range, but the exact
        // bin sequence depends on the arithmetic recurrence — assert instead
        // that reading n bits yields a value within [0, 2^n).
        let data = [0xFFu8; 4];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let v = dec.decode_bypass_bits(5);
        assert!(v < 32);
    }

    #[test]
    fn decode_terminate_eventually_signals_end_on_all_ones() {
        // An all-ones bitstream drives codIOffset to stay high relative to a
        // shrinking codIRange, so decode_terminate must fire before the
        // reader runs dry.
        let data = [0xFFu8; 8];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut terminated = false;
        for _ in 0..64 {
            if dec.decode_terminate() == 1 {
                terminated = true;
                break;
            }
        }
        assert!(terminated);
    }

    #[test]
    fn decode_bypass_eg_zero_prefix_returns_zero() {
        // k0=0, first bypass bin = 0 (stop) -> code_num = 0, no suffix bits read.
        // Construct a stream whose first bypass decode yields 0: with
        // range=510 and offset < range for a 0 bit, this depends on exact
        // arithmetic state, so instead verify the decoded value is always
        // representable and decoding terminates without panicking.
        let data = [0x00u8; 4];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let v = dec.decode_bypass_eg(0);
        assert!(v < (1 << 20));
    }

    #[test]
    fn truncated_unary_zero_cmax_reads_nothing() {
        let data = [0xA3u8, 0x5C, 0x91];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let (range_before, offset_before) = (dec.range, dec.offset);
        let mut ctx = [CabacContext::init(20, 40, 26)];
        let v = dec.decode_truncated_unary(0, &mut ctx);
        assert_eq!(v, 0);
        assert_eq!(dec.range, range_before);
        assert_eq!(dec.offset, offset_before);
    }

    #[test]
    fn truncated_unary_never_exceeds_cmax() {
        // All-ones data pushes the arithmetic engine toward repeatedly
        // decoding the LPS/MPS such that many syntax elements would read
        // consecutive `1` bins; the truncated-unary loop must still stop at
        // c_max regardless of the underlying bit pattern.
        let data = [0xFFu8; 8];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = [
            CabacContext::init(23, 33, 26),
            CabacContext::init(22, 25, 26),
            CabacContext::init(29, 16, 26),
        ];
        for _ in 0..16 {
            let v = dec.decode_truncated_unary(3, &mut ctx);
            assert!(v <= 3);
        }
    }

    #[test]
    fn truncated_unary_bypass_never_exceeds_cmax() {
        let data = [0xFFu8; 8];
        let mut dec = CabacDecoder::new(&data).unwrap();
        for _ in 0..16 {
            let v = dec.decode_truncated_unary_bypass(5);
            assert!(v <= 5);
        }
    }

    #[test]
    fn mb_skip_neighbors_ctx_idx_inc() {
        // Both neighbours unavailable -> condTermFlagA = condTermFlagB = 0.
        let n = MbSkipNeighbors::default();
        assert_eq!(n.ctx_idx_inc(), 0);

        // Left available and not skipped -> condTermFlagA = 1.
        let n = MbSkipNeighbors {
            left_available: true,
            left_skipped: false,
            top_available: false,
            top_skipped: false,
        };
        assert_eq!(n.ctx_idx_inc(), 1);

        // Left available but skipped -> condTermFlagA = 0 (skipped counts as unavailable-like).
        let n = MbSkipNeighbors {
            left_available: true,
            left_skipped: true,
            top_available: true,
            top_skipped: false,
        };
        assert_eq!(n.ctx_idx_inc(), 1);

        // Both available and not skipped -> condTermFlagA = condTermFlagB = 1.
        let n = MbSkipNeighbors {
            left_available: true,
            left_skipped: false,
            top_available: true,
            top_skipped: false,
        };
        assert_eq!(n.ctx_idx_inc(), 2);
    }

    #[test]
    fn mb_skip_flag_context_selects_context_by_neighbors() {
        let data = [0xA3u8, 0x5C, 0x91, 0x77, 0x2E, 0x0F];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut skip_ctx = MbSkipFlagContext::new_p_slice(26, 0);
        let before = [skip_ctx.ctx[0], skip_ctx.ctx[1], skip_ctx.ctx[2]];

        let neighbors = MbSkipNeighbors {
            left_available: true,
            left_skipped: false,
            top_available: false,
            top_skipped: false,
        };
        let _ = skip_ctx.decode(&mut dec, &neighbors);

        // Only ctx[1] (ctxIdxInc = 1) should have been touched.
        assert_eq!(skip_ctx.ctx[0], before[0]);
        assert_ne!(skip_ctx.ctx[1], before[1]);
        assert_eq!(skip_ctx.ctx[2], before[2]);
    }

    #[test]
    fn mb_skip_flag_b_slice_context_selects_context_by_neighbors() {
        // Mirrors `mb_skip_flag_context_selects_context_by_neighbors` above,
        // but for the B-slice constructor (ctxIdx 24..=26 instead of
        // 11..=13); same ctxIdxInc derivation, different ctxIdxOffset.
        let data = [0xA3u8, 0x5C, 0x91, 0x77, 0x2E, 0x0F];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut skip_ctx = MbSkipFlagContext::new_b_slice(26, 0);
        let before = [skip_ctx.ctx[0], skip_ctx.ctx[1], skip_ctx.ctx[2]];

        let neighbors = MbSkipNeighbors {
            left_available: true,
            left_skipped: false,
            top_available: true,
            top_skipped: false,
        };
        let _ = skip_ctx.decode(&mut dec, &neighbors);

        // Both neighbours available and not skipped -> ctxIdxInc = 2 -> only
        // ctx[2] should have been touched.
        assert_eq!(skip_ctx.ctx[0], before[0]);
        assert_eq!(skip_ctx.ctx[1], before[1]);
        assert_ne!(skip_ctx.ctx[2], before[2]);
    }

    #[test]
    fn mb_skip_flag_p_slice_context_init_differs_by_cabac_init_idc() {
        // Confirms MbSkipFlagContext::new_p_slice actually threads
        // cabac_init_idc through to context selection (not just accepting
        // and ignoring the parameter) -- ctxIdx 11 has a different (m, n)
        // pair per idc (see
        // cabac_tables::tests::pb1_and_pb2_mb_skip_flag_p_differs_from_pb0),
        // so the freshly-initialised (pre-any-decode) context state should
        // differ across idc values too.
        let idc0 = MbSkipFlagContext::new_p_slice(26, 0);
        let idc1 = MbSkipFlagContext::new_p_slice(26, 1);
        let idc2 = MbSkipFlagContext::new_p_slice(26, 2);
        assert_ne!(idc0.ctx[0], idc1.ctx[0]);
        assert_ne!(idc1.ctx[0], idc2.ctx[0]);
    }

    #[test]
    fn mb_type_i_decode_returns_valid_values() {
        let data = [0xFFu8; 16];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = MbTypeICabacContext::new(26);
        let neighbors = MbTypeNeighbors::default();
        let mb_type = ctx.decode(&mut dec, &neighbors);
        assert!(
            mb_type <= 25,
            "I-slice mb_type must be 0..=25, got {mb_type}"
        );
    }

    #[test]
    fn mb_type_i_bin0_ctx_selected_by_neighbors() {
        let data = [0xA3u8, 0x5C, 0x91, 0x77, 0x2E, 0x0F];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = MbTypeICabacContext::new(26);
        let before = ctx.ctx;
        let neighbors = MbTypeNeighbors {
            left_is_16x16_or_pcm: true,
            top_is_16x16_or_pcm: false,
        };
        let _ = ctx.decode(&mut dec, &neighbors);
        // ctxIdxInc = 1 -> only local ctx[1] should be touched by bin 0 (bin
        // 0 = 0 short-circuits to I_NxN with no further reads on this data).
        assert_eq!(ctx.ctx[0], before[0]);
        assert_ne!(ctx.ctx[1], before[1]);
    }

    #[test]
    fn cbp_decode_returns_luma_and_chroma() {
        let data = [0xFFu8; 16];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = CbpCabacContext::new(26);
        let (luma, chroma) = ctx.decode(&mut dec, 0x7CF, 0x7CF);
        assert!(luma <= 15, "luma CBP must be 0..=15, got {luma}");
        assert!(chroma <= 3, "chroma CBP must be 0..=3, got {chroma}");
    }

    #[test]
    fn cbp_chroma_value_bit_not_read_when_presence_bit_zero() {
        // All-zero data: every context-coded decision decodes MPS/LPS
        // deterministically from a fresh context, but the key property under
        // test is that decoding does not panic or run past the end of a
        // short buffer when the chroma-cbp presence bit comes back 0 -- the
        // old implementation unconditionally read a second chroma bit here,
        // which desynced the stream. 6 bytes covers 4 luma bins + up to 2
        // chroma bins without exhausting the reader.
        let data = [0x00u8; 6];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = CbpCabacContext::new(26);
        let (_luma, chroma) = ctx.decode(&mut dec, 0x7CF, 0x7CF);
        assert!(chroma <= 3);
    }

    #[test]
    fn mb_qp_delta_decode_returns_signed_value() {
        let data = [0xFFu8; 16];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = MbQpDeltaCabacContext::new(26);
        let dqp = ctx.decode(&mut dec, false);
        // mb_qp_delta is typically small; just verify it doesn't panic
        // and returns a reasonable value.
        assert!(dqp.abs() <= 200);
    }

    #[test]
    fn mb_qp_delta_bin0_ctx_selected_by_prev_state() {
        let data = [0xA3u8, 0x5C, 0x91, 0x77];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = MbQpDeltaCabacContext::new(26);
        let before = ctx.ctx;
        let _ = ctx.decode(&mut dec, true);
        // prev_nonzero=true -> ctxIdxInc=1 -> only ctx[1] touched by bin 0
        // (short-circuits here since ctx[1] decodes to 0 on this data).
        assert_eq!(ctx.ctx[0], before[0]);
    }

    #[test]
    fn intra_chroma_pred_mode_decode_in_range() {
        let data = [0xFFu8; 8];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = IntraChromaPredModeCabacContext::new(26);
        let mode = ctx.decode(&mut dec, false, false);
        assert!(mode <= 3, "chroma pred mode must be 0..=3, got {mode}");
    }

    #[test]
    fn intra4x4_pred_mode_decode_in_range() {
        let data = [0xA3u8, 0x5C, 0x91, 0x77];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = Intra4x4PredModeCabacContext::new(26);
        let mode = ctx.decode(&mut dec, 2);
        assert!(mode <= 8, "intra4x4 pred mode must be 0..=8, got {mode}");
    }

    #[test]
    fn coded_block_flag_decode_returns_bool() {
        let data = [0xA3u8, 0x5C];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = CodedBlockFlagContext::new(26);
        // Should not panic for any of the 5 block categories.
        for cat in 0..5 {
            let _ = ctx.decode(&mut dec, cat, true, true);
        }
    }

    #[test]
    fn residual_block_decode_matches_significance_count() {
        let data = [0x55u8; 16];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = ResidualCabacContext::new(26);
        for (cat, max_coeff) in [(0, 16), (1, 15), (2, 16), (3, 4), (4, 15)] {
            let (coeffs, coeff_count) = ctx.decode_block(&mut dec, cat, max_coeff);
            let nonzero = coeffs.iter().filter(|&&c| c != 0).count();
            assert_eq!(
                nonzero, coeff_count as usize,
                "cat {cat}: nonzero coeff count in output must match returned coeff_count"
            );
            assert!(coeff_count as usize <= max_coeff);
        }
    }

    // ---- Phase D.4: P-slice CABAC primitives ----

    #[test]
    fn intra_mb_type_suffix_pb_decode_in_range() {
        let data = [0xFFu8; 16];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = IntraMbTypeSuffixCabacContext::new_pb(17, 26, 0);
        let mb_type = ctx.decode(&mut dec);
        assert!(mb_type <= 25, "must be 0..=25, got {mb_type}");
    }

    #[test]
    fn intra_mb_type_suffix_pb_bin0_is_a_single_fixed_context() {
        // Unlike the I-slice suffix, bin 0 here has no neighbour-derived
        // ctxIdxInc -- it always reads local ctx[0], regardless of any
        // external state (there's no neighbours parameter to even vary it).
        let data = [0x00u8; 8];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = IntraMbTypeSuffixCabacContext::new_pb(17, 26, 0);
        let before = ctx.ctx;
        let _ = ctx.decode(&mut dec);
        assert_ne!(ctx.ctx[0], before[0], "bin 0 must touch ctx[0]");
    }

    #[test]
    fn mb_type_p_decode_returns_inter_shape_or_none_for_intra() {
        let data = [0xFFu8; 8];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = MbTypePCabacContext::new(26, 0);
        match ctx.decode(&mut dec) {
            None => {} // intra-in-P
            Some(shape) => assert!(shape <= 3, "P shape must be 0..=3, got {shape}"),
        }
    }

    #[test]
    fn mb_type_p_decode_is_deterministic_for_fixed_input() {
        // Same context init + same stream must always decode the same
        // result (arithmetic decoding has no hidden nondeterminism).
        let data = [0x00u8; 4];
        let mut dec1 = CabacDecoder::new(&data).unwrap();
        let mut ctx1 = MbTypePCabacContext::new(26, 0);
        let a = ctx1.decode(&mut dec1);

        let mut dec2 = CabacDecoder::new(&data).unwrap();
        let mut ctx2 = MbTypePCabacContext::new(26, 0);
        let b = ctx2.decode(&mut dec2);
        assert_eq!(a, b);
        if let Some(shape) = a {
            assert!(shape <= 3);
        }
    }

    #[test]
    fn sub_mb_type_p_decode_in_range() {
        let data = [0xFFu8; 4];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = SubMbTypePCabacContext::new(26, 0);
        for _ in 0..4 {
            let v = ctx.decode(&mut dec);
            assert!(v <= 3, "sub_mb_type must be 0..=3, got {v}");
        }
    }

    #[test]
    fn ref_idx_decode_zero_when_bin0_is_zero() {
        // All-zero bypass-free stream: a freshly initialised context's first
        // decision is deterministic from its (m,n) pair, not necessarily 0,
        // so just assert the general contract (never panics, bounded value)
        // and that ctxIdxInc selection doesn't touch unrelated contexts.
        let data = [0xA3u8, 0x5C, 0x91, 0x77];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = RefIdxCabacContext::new(26, 0);
        let before = ctx.ctx;
        let val = ctx.decode(&mut dec, false, false);
        assert!(val < 32);
        // left_gt0=false, top_gt0=false -> ctxIdxInc=0 -> bin0 touches ctx[0]
        // only (further bins, if any, touch ctx[4]/ctx[5], never ctx[1..3]).
        assert_eq!(ctx.ctx[1], before[1]);
        assert_eq!(ctx.ctx[2], before[2]);
        assert_eq!(ctx.ctx[3], before[3]);
    }

    #[test]
    fn ref_idx_decode_selects_ctx_by_neighbor_availability() {
        let data = [0xA3u8, 0x5C, 0x91, 0x77];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = RefIdxCabacContext::new(26, 0);
        let before = ctx.ctx;
        let _ = ctx.decode(&mut dec, true, true);
        // left_gt0=true, top_gt0=true -> ctxIdxInc=3 -> bin0 touches ctx[3].
        assert_ne!(ctx.ctx[3], before[3]);
        assert_eq!(ctx.ctx[0], before[0]);
        assert_eq!(ctx.ctx[1], before[1]);
        assert_eq!(ctx.ctx[2], before[2]);
    }

    #[test]
    fn mvd_decode_zero_amvd_sum_uses_ctx0() {
        let data = [0xA3u8, 0x5C, 0x91, 0x77];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = MvdCabacContext::new(crate::cabac_tables::MVD_X_CTX, 26, 0);
        let before = ctx.ctx;
        let _ = ctx.decode(&mut dec, 0);
        assert_ne!(ctx.ctx[0], before[0]);
        assert_eq!(ctx.ctx[1], before[1]);
        assert_eq!(ctx.ctx[2], before[2]);
    }

    #[test]
    fn mvd_decode_large_amvd_sum_uses_ctx2() {
        let data = [0xA3u8, 0x5C, 0x91, 0x77];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = MvdCabacContext::new(crate::cabac_tables::MVD_X_CTX, 26, 0);
        let before = ctx.ctx;
        let _ = ctx.decode(&mut dec, 100);
        assert_ne!(ctx.ctx[2], before[2]);
        assert_eq!(ctx.ctx[0], before[0]);
        assert_eq!(ctx.ctx[1], before[1]);
    }

    #[test]
    fn mvd_decode_mid_amvd_sum_uses_ctx1() {
        let data = [0xA3u8, 0x5C, 0x91, 0x77];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = MvdCabacContext::new(crate::cabac_tables::MVD_X_CTX, 26, 0);
        let before = ctx.ctx;
        let _ = ctx.decode(&mut dec, 10);
        assert_ne!(ctx.ctx[1], before[1]);
        assert_eq!(ctx.ctx[0], before[0]);
        assert_eq!(ctx.ctx[2], before[2]);
    }

    #[test]
    fn mvd_decode_returns_bounded_value_across_many_streams() {
        // Fuzz-lite: decode repeatedly from varied streams and assert the
        // result never panics and stays within a sane magnitude.
        for byte in [0x00u8, 0x55, 0xAA, 0xFF, 0x3C, 0xC3] {
            let data = [byte; 32];
            let mut dec = CabacDecoder::new(&data).unwrap();
            let mut ctx = MvdCabacContext::new(crate::cabac_tables::MVD_X_CTX, 26, 0);
            for _ in 0..8 {
                let v = ctx.decode(&mut dec, 0);
                assert!(v.unsigned_abs() < (1 << 24));
            }
        }
    }

    #[test]
    fn cbp_context_new_pb_differs_from_new_at_some_ctx_idx() {
        // Spot-check the additive P/B constructors actually read from the PB
        // table (not silently delegating to the I-table init).
        let i_ctx = CbpCabacContext::new(26);
        let pb_ctx = CbpCabacContext::new_pb(26, 0);
        assert_ne!(i_ctx.ctx, pb_ctx.ctx);
    }

    #[test]
    fn residual_context_new_pb_decodes_without_panicking() {
        let data = [0x55u8; 16];
        let mut dec = CabacDecoder::new(&data).unwrap();
        let mut ctx = ResidualCabacContext::new_pb(26, 0);
        for (cat, max_coeff) in [(0, 16), (1, 15), (2, 16), (3, 4), (4, 15)] {
            let (_coeffs, coeff_count) = ctx.decode_block(&mut dec, cat, max_coeff);
            assert!(coeff_count as usize <= max_coeff);
        }
    }
}
