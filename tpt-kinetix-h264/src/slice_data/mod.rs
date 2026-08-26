//! H.264 slice-data / macroblock-layer parsing for I-slices (ITU-T §7.3.4,
//! §7.3.5, §9.2).
//!
//! This is the real bitstream-driven parser that produces [`Macroblock`]s from
//! CAVLC-coded I-slice data. It replaces the previous all-skip placeholder path
//! in the decoder. Only the I-slice (intra) macroblock types are handled here;
//! P/B (inter) parsing is added in a later phase.
//!
//! The parser drives the spec-exact CAVLC tables in [`crate::cavlc_tables`] and
//! stores parsed residuals as zigzag-order coefficient arrays on the
//! [`Macroblock`], ready for the inverse-quant/transform reconstruction path.

use crate::{
    bitreader::BitReader,
    cavlc_tables,
    macroblock::{Macroblock, MbType},
    mv::MvStore,
    prediction::Intra4x4Mode,
};

/// Table 7-11 (I-slice `mb_type`) I_16×16 decomposition, per-row values.
/// Index by `mb_type - 1` (0..=23) -> (Intra16x16PredMode, CBPChroma, CBPLuma).
#[rustfmt::skip]
const I16X16_TABLE: [(u8, u8, u8); 24] = [
    (0,0,0),(1,0,0),(2,0,0),(3,0,0),
    (0,1,0),(1,1,0),(2,1,0),(3,1,0),
    (0,2,0),(1,2,0),(2,2,0),(3,2,0),
    (0,0,15),(1,0,15),(2,0,15),(3,0,15),
    (0,1,15),(1,1,15),(2,1,15),(3,1,15),
    (0,2,15),(1,2,15),(2,2,15),(3,2,15),
];

/// coded_block_pattern (Table 9-4) — codeNum -> CBP for Intra_4×4 macroblocks.
#[rustfmt::skip]
const GOLOMB_TO_INTRA4X4_CBP: [u8; 48] = [
    47, 31, 15,  0, 23, 27, 29, 30,  7, 11, 13, 14, 39, 43, 45, 46,
    16,  3,  5, 10, 12, 19, 21, 26, 28, 35, 37, 42, 44,  1,  2,  4,
     8, 17, 18, 20, 24,  6,  9, 22, 25, 32, 33, 34, 36, 40, 38, 41,
];

/// coded_block_pattern (Table 9-4) — codeNum -> CBP for **inter** macroblocks
/// (§7.4.5). CBP layout is chroma in bits 4-5 and luma (4×4 8×8 groups) in bits
/// 0-3. Values match FFmpeg's `golomb_to_inter_cbp` (the spec Table 9-4
/// mapping — NOT the `golomb_to_inter_cbp_gray` permutation, which is a
/// different table and was previously used here in error).
#[rustfmt::skip]
const GOLOMB_TO_INTER_CBP: [u8; 48] = [
     0, 16,  1,  2,  4,  8, 32,  3,  5, 10, 12, 15, 47,  7, 11, 13,
    14,  6,  9, 31, 35, 37, 42, 44, 33, 34, 36, 40, 39, 43, 45, 46,
    17, 18, 20, 24, 19, 21, 26, 28, 23, 27, 29, 30, 22, 25, 38, 41,
];

/// sub_macroblock types for P_8x8 (Table 7-13) — number of sub-partitions.
const P_SUB_MB_PARTS: [usize; 4] = [1, 2, 2, 4];

/// B-slice mb_type 4..=21 table: (is_16x8, dir0, dir1).
/// Index = mb_type_raw − 4.
const B_2PART_TABLE: [(
    bool,
    crate::macroblock::BPredDir,
    crate::macroblock::BPredDir,
); 18] = {
    use crate::macroblock::BPredDir;
    [
        (true, BPredDir::L0, BPredDir::L0),  // 4
        (false, BPredDir::L0, BPredDir::L0), // 5
        (true, BPredDir::L1, BPredDir::L1),  // 6
        (false, BPredDir::L1, BPredDir::L1), // 7
        (true, BPredDir::L0, BPredDir::L1),  // 8
        (false, BPredDir::L0, BPredDir::L1), // 9
        (true, BPredDir::L1, BPredDir::L0),  // 10
        (false, BPredDir::L1, BPredDir::L0), // 11
        (true, BPredDir::L0, BPredDir::Bi),  // 12
        (false, BPredDir::L0, BPredDir::Bi), // 13
        (true, BPredDir::L1, BPredDir::Bi),  // 14
        (false, BPredDir::L1, BPredDir::Bi), // 15
        (true, BPredDir::Bi, BPredDir::L0),  // 16
        (false, BPredDir::Bi, BPredDir::L0), // 17
        (true, BPredDir::Bi, BPredDir::L1),  // 18
        (false, BPredDir::Bi, BPredDir::L1), // 19
        (true, BPredDir::Bi, BPredDir::Bi),  // 20
        (false, BPredDir::Bi, BPredDir::Bi), // 21
    ]
};

mod cabac_b;
mod cabac_i;
mod cabac_p;
mod cavlc;
mod ctx;

pub use cabac_b::parse_b_slice_cabac;
pub use cabac_i::parse_i_slice_cabac;
pub use cabac_p::parse_p_slice_cabac;
pub use cavlc::{
    parse_b_slice, parse_cavlc_block, parse_i_slice, parse_p_slice, raster_of_8x8_sub,
};
pub use ctx::{
    CabacSliceContexts, MbCabacCtx, MbInterCabacCtx, MbNz, MbPredCtx, NeighbourCtx, ParsedSlice,
    PbCabacSliceContexts, SliceDataError,
};

// Cross-module private helpers/types (defined in `ctx`) re-exported so sibling
// submodules can reach them via `use super::*;`.
pub(crate) use ctx::{
    cabac_cbp_neighbors, cabac_decode_mvd_component, chroma_cbf_neighbors, dc_cbf_neighbor,
    luma_cbf_neighbors, partition_blocks, partition_dims, ref_idx_gt0_neighbors, MAX_LEVEL_PREFIX,
    R,
};

pub(crate) use cabac_b::parse_p_macroblock_cabac;
pub(crate) use cabac_p::parse_intra_macroblock_cabac;
pub(crate) use cavlc::{mpm_pred_mode, mpm_pred_mode_8x8};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raster_mapping() {
        // 8×8 group 0 -> blocks 0,1,4,5
        assert_eq!(raster_of_8x8_sub(0, 0), 0);
        assert_eq!(raster_of_8x8_sub(0, 1), 1);
        assert_eq!(raster_of_8x8_sub(0, 2), 4);
        assert_eq!(raster_of_8x8_sub(0, 3), 5);
        // group 3 -> blocks 10,11,14,15
        assert_eq!(raster_of_8x8_sub(3, 0), 10);
        assert_eq!(raster_of_8x8_sub(3, 3), 15);
    }

    #[test]
    fn empty_block_parses_to_zero() {
        // coeff_token for nC=0, TotalCoeff=0 is the single bit "1".
        let data = [0b1000_0000u8];
        let mut r = BitReader::new(&data);
        let (coeffs, tc, _t1) = parse_cavlc_block(&mut r, 0, 16).unwrap();
        assert_eq!(tc, 0);
        assert_eq!(coeffs, [0i16; 16]);
    }

    #[test]
    fn p_slice_single_16x16_mb() {
        // 1×1 picture, one P_L0_16x16 MB with no residual:
        //   mb_skip_run=0 → "1", mb_type=0 → "1", ref_idx(rc=1) no bits,
        //   mvd(0,0) → "1" "1", cbp=0 → "1" → 111111, then rbsp stop bit.
        let data = [0xFFu8, 0x80];
        let mut r = BitReader::new(&data);
        let parsed = parse_p_slice(
            &mut r,
            1,
            1,
            26,
            1,
            0,
            false,
            false,
            false,
            &mut crate::trace::NoopTracer,
        )
        .unwrap();
        assert_eq!(parsed.macroblocks.len(), 1);
        let mb = &parsed.macroblocks[0];
        assert_eq!(mb.mb_type, MbType::PL016x16);
        assert!(!mb.skip);
        assert_eq!(mb.cbp, 0);
        let motion = mb.motion.as_ref().unwrap();
        assert_eq!(motion.ref_idx_l0, vec![0]);
        assert_eq!(motion.mvd_l0, vec![(0, 0)]);
        assert!(motion.sub_mb_type.is_none());
        // MV store: predicted (0,0) ref 0 for the whole 16×16 grid.
        let grid = parsed.mv_store.cells_of(0).unwrap();
        assert_eq!(grid[0].mv, [0, 0]);
        assert_eq!(grid[0].ref_idx, 0);
        assert_eq!(grid[15].mv, [0, 0]);
    }

    #[test]
    fn p_slice_skip_run_then_coded_mb() {
        // 1×4 picture: mb_skip_run=3 ("00100") skips MB0-2, then a coded
        // P_L0_16x16 MB3 (mb_skip_run=0 "1", mb_type=0 "1", ref no bits,
        // mvd(0,0) "1" "1", cbp=0 "1") + rbsp stop bit.
        // bits: 00100 11111 1(pad) → 0x27 0xE0.
        let data = [0x27u8, 0xE0];
        let mut r = BitReader::new(&data);
        let parsed = parse_p_slice(
            &mut r,
            1,
            4,
            26,
            1,
            0,
            false,
            false,
            false,
            &mut crate::trace::NoopTracer,
        )
        .unwrap();
        assert_eq!(parsed.macroblocks.len(), 4);
        for mb in &parsed.macroblocks[..3] {
            assert!(mb.skip, "MB should be skip");
            assert_eq!(mb.mb_type, MbType::PSkip);
            assert!(mb.motion.is_none());
        }
        let last = &parsed.macroblocks[3];
        assert!(!last.skip);
        assert_eq!(last.mb_type, MbType::PL016x16);
        // All four MBs committed to the MV store; skip MBs get (0,0) ref 0.
        for mb_idx in 0..4 {
            let grid = parsed.mv_store.cells_of(mb_idx).unwrap();
            assert_eq!(grid[0].mv, [0, 0]);
            assert_eq!(grid[0].ref_idx, 0);
        }
    }

    #[test]
    fn p_slice_16x8_partitions() {
        // 1×1 picture, P_L0_L0_16x8 (mb_type 1): two partitions, each with
        // ref=0 (rc=1, no bits) and mvd(0,0) ("1" "1" each).
        // bits: run=0 "1", mb_type=1 → ue(1)="010", refs no bits,
        //       mvd0 "1" "1", mvd1 "1" "1", cbp=0 "1"
        //   = 1 010 11111 → 0b1010_1111 = 0xAF, then rbsp stop.
        let data = [0xAFu8, 0x80];
        let mut r = BitReader::new(&data);
        let parsed = parse_p_slice(
            &mut r,
            1,
            1,
            26,
            1,
            0,
            false,
            false,
            false,
            &mut crate::trace::NoopTracer,
        )
        .unwrap();
        let mb = &parsed.macroblocks[0];
        assert_eq!(mb.mb_type, MbType::P16x8);
        let motion = mb.motion.as_ref().unwrap();
        assert_eq!(motion.ref_idx_l0, vec![0, 0]);
        assert_eq!(motion.mvd_l0, vec![(0, 0), (0, 0)]);
    }

    #[test]
    fn p_slice_8x8_with_sub_mb_types() {
        // 1×1 picture, P_8x8 (mb_type 3): four sub_mb_types (all 0 = 8×8 →
        // 1 sub-partition each), refs = 0 (rc=1), mvd(0,0) per sub-partition.
        // bits: run=0 "1", mb_type=3 → ue(3)="00100",
        //       sub_types: ue(0) "1" ×4,
        //       4× [ref no bits + mvd "1" "1"],
        //       cbp=0 "1"
        //   = 1,00100,1,1,1,1,1,1,1,1,1,1,1,1,1,1 → count: 1+5+4+8+1 = 19 bits
        //   byte0 = 10010011 = 0x93, byte1 = 11111111 = 0xFF,
        //   byte2 = 1(data)+1(stop)+pad = 11100000 = 0xE0
        let data = [0x93u8, 0xFF, 0xE0];
        let mut r = BitReader::new(&data);
        let parsed = parse_p_slice(
            &mut r,
            1,
            1,
            26,
            1,
            0,
            false,
            false,
            false,
            &mut crate::trace::NoopTracer,
        )
        .unwrap();
        let mb = &parsed.macroblocks[0];
        assert_eq!(mb.mb_type, MbType::P8x8);
        let motion = mb.motion.as_ref().unwrap();
        assert_eq!(motion.sub_mb_type, Some([0, 0, 0, 0]));
        assert_eq!(motion.ref_idx_l0, vec![0, 0, 0, 0]);
        assert_eq!(motion.mvd_l0.len(), 4);
        assert!(motion.mvd_l0.iter().all(|&m| m == (0, 0)));
    }
}
