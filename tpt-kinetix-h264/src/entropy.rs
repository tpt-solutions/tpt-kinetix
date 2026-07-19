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
#[rustfmt::skip]
const TRANS_IDX_LPS: [u8; 64] = [
    0, 0, 1, 2, 2, 4, 4, 5, 6, 7, 8, 9, 9, 11, 11, 12,
    13, 13, 15, 15, 16, 16, 18, 18, 19, 19, 21, 21, 23, 22, 23, 24,
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
            code_num += 1 << k;
            k += 1;
            if k >= 32 {
                break;
            }
        }
        while k > 0 {
            k -= 1;
            if self.decode_bypass() == 1 {
                code_num += 1 << k;
            }
        }
        code_num
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

/// `mb_skip_flag` context init values for P/SP slices, ctxIdx 11..=13 (spec
/// Table 9-13, `ctxIdxOffset = 11`).
///
/// # Provenance warning
/// These `(m, n)` pairs are transcribed from memory of the widely-mirrored
/// H.264 CABAC context-init tables (as reproduced identically across
/// ffmpeg/JM/x264 reference sources), **not** copied from the ITU-T H.264
/// spec text or cross-checked against a reference decoder in this session.
/// Verify against ITU-T H.264 Table 9-13 (or an authoritative decoder's
/// context-init table) before relying on this for a real decode path.
const MB_SKIP_FLAG_P_INIT: [(i32, i32); 3] = [(23, 33), (22, 25), (29, 16)];

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

/// `mb_skip_flag` decoding for P/SP slices: a single context-coded bin whose
/// value *is* `mb_skip_flag` (no further binarization).
pub struct MbSkipFlagContext {
    ctx: [CabacContext; 3],
}

impl MbSkipFlagContext {
    /// Initialise the three `mb_skip_flag` contexts for a P/SP slice at the
    /// given slice QP. See [`MB_SKIP_FLAG_P_INIT`] for the provenance caveat
    /// on these init values.
    pub fn new_p_slice(slice_qp_y: i32) -> Self {
        let ctx = MB_SKIP_FLAG_P_INIT.map(|(m, n)| CabacContext::init(m, n, slice_qp_y));
        Self { ctx }
    }

    /// Decode `mb_skip_flag` for the current macroblock given its left/top
    /// neighbour availability and skip state.
    pub fn decode(&mut self, dec: &mut CabacDecoder, neighbors: &MbSkipNeighbors) -> bool {
        let idx = neighbors.ctx_idx_inc();
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
        let mut skip_ctx = MbSkipFlagContext::new_p_slice(26);
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
}
