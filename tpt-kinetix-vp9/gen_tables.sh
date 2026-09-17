#!/bin/bash
# One-shot generator for src/tables.rs — big arrays come mechanically from
# `tpt-kinetix-kg extract-tables` on the pinned FFmpeg commit; trees and the
# defaults struct are transcribed from the same pinned source by hand.
set -euo pipefail
cd "$(dirname "$0")"
COMMIT=c3ff71680805267bc8f3fff86c1cf917f810c0d9
DATA="../tpt-kinetix-kg/.cache/ffmpeg/$COMMIT/libavcodec/vp9data.c"
DSP="../tpt-kinetix-kg/.cache/ffmpeg/$COMMIT/libavcodec/vp9dsp.c"
V9="../tpt-kinetix-kg/.cache/ffmpeg/$COMMIT/libavcodec/vp9.c"

emit() { # emit <rust_name> <type> <symbol> <file> <tmpfile> [per_line]
    local name=$1 ty=$2 sym=$3 file=$4 tmp=$5 per=${6:-16}
    if [ -n "${NOTABLE:-}" ]; then echo "// (verify-tables not applicable: extracted textually; see doc comment)"; else echo "// verify-tables: rust=$name symbol=$sym commit=$COMMIT file=$file"; fi
    echo "pub const $name: [$ty; $(tail -n +2 < "$tmp" | grep -oE -- '-?[0-9]+' | wc -l)] = ["
    tail -n +2 < "$tmp" | grep -oE -- '-?[0-9]+' | awk -v per="$per" '{ s = s ((NR%per==1)?"":", ") $1; if (NR%per==0) { print "    " s ","; s="" } } END { if (s!="") print "    " s "," }'
    echo "];"
    echo
}

mkdir -p src
cat > src/tables.rs <<'HEADER'
//! VP9 normative tables — mode/partition/MV probabilities, coefficient
//! probabilities, scan orders, dequantization lookups and sub-pel
//! interpolation filters.
//!
//! Numeric arrays are extracted mechanically from the pinned FFmpeg commit
//! via `tpt-kinetix-kg extract-tables` and re-checked by
//! `cargo run -p tpt-kinetix-kg -- verify-tables src/tables.rs` (the
//! `verify-tables:` markers below). Nothing from FFmpeg is committed —
//! only the extracted raw numbers, as in `tpt-kinetix-h264`.

/// Block sizes, spec order: index = `block_level * 3 + block_partition`.
pub const N_BS_SIZES: usize = 13;

HEADER
{
echo "/// [above_idx][left_idx][9] keyframe intra mode probs (spec section 13.4)."
emit DEFAULT_KF_YMODE_PROBS u8 ff_vp9_default_kf_ymode_probs libavcodec/vp9data.c /tmp/vp9tab_default_kf_ymode_probs.txt 27
echo "/// [y_mode][9] keyframe chroma intra mode probs (spec section 13.4)."
emit DEFAULT_KF_UVMODE_PROBS u8 ff_vp9_default_kf_uvmode_probs libavcodec/vp9data.c /tmp/vp9tab_default_kf_uvmode_probs.txt 9
echo "/// [block_level][partition_ctx][3] keyframe partition probs (spec section 13.3)."
emit DEFAULT_KF_PARTITION_PROBS u8 ff_vp9_default_kf_partition_probs libavcodec/vp9data.c /tmp/vp9tab_default_kf_partition_probs.txt 9
echo "/// [2][13][2] flattened: [half][bs][0] = width in 8px units, [1] = height."
emit BWH_TAB u8 ff_vp9_bwh_tab libavcodec/vp9data.c /tmp/vp9tab_bwh_tab.txt 6
cat <<'TREES'
/// Partition tree: 4 leaves (NONE, H, V, SPLIT), 3 nodes.
pub const PARTITION_TREE: [[i8; 2]; 3] = [[-0, 1], [-1, 2], [-2, -3]];
/// Segmentation id tree: 8 leaves, 7 nodes.
pub const SEGMENTATION_TREE: [[i8; 2]; 7] = [[1, 2], [3, 4], [5, 6], [-0, -1], [-2, -3], [-4, -5], [-6, -7]];
/// Intra mode tree: 10 leaves, 9 nodes. Negative leaves are spec mode values.
pub const INTRAMODE_TREE: [[i8; 2]; 9] = [
    [-0, 1],   // '0'       -> DC
    [-9, 2],   // '10'      -> TM
    [-1, 3],   // '110'     -> V
    [4, 6],    //
    [-2, 5],   // '11100'   -> H
    [-4, -5],  // '11101x'  -> D135, D113
    [-3, 7],   // '11110'   -> D45
    [-7, 8],   // '111110'  -> D203
    [-6, -8],  // '111111x' -> D157, D67
];
/// Inter mode tree: ZEROMV, NEARESTMV, NEARMV, NEWMV.
pub const INTER_MODE_TREE: [[i8; 2]; 3] = [[-0, 1], [-1, 2], [-2, -3]];
/// Interpolation filter tree (switchable ids; map through [`FILTER_LUT`]).
pub const FILTER_TREE: [[i8; 2]; 2] = [[-0, 1], [-1, -2]];
/// MV joint tree: ZERO, H, V, HV.
pub const MV_JOINT_TREE: [[i8; 2]; 3] = [[-0, 1], [-1, 2], [-2, -3]];
/// MV class tree: 11 leaves (class 0..10), 10 nodes.
pub const MV_CLASS_TREE: [[i8; 2]; 10] = [[-0, 1], [-1, 2], [3, 4], [-2, -3], [5, 6], [-4, -5], [-6, 7], [8, 9], [-7, -8], [-9, -10]];
/// MV fractional-position tree: 4 leaves, 3 nodes.
pub const MV_FP_TREE: [[i8; 2]; 3] = [[-0, 1], [-1, 2], [-2, -3]];

/// Switchable filter-tree leaf id -> filter type row in [`SUBPEL_FILTERS`]
/// (FFmpeg rows: 0 = REGULAR, 1 = SHARP, 2 = SMOOTH).
pub const FILTER_LUT: [usize; 3] = [0, 2, 1];

/// Intra mode (spec values 0..9) -> transform type: 0=DCT_DCT, 1=ADST_DCT,
/// 2=DCT_ADST, 3=ADST_ADST.
pub const INTRA_TXFM_TYPE: [u8; 14] = [
    1, // V    -> ADST_DCT
    2, // H    -> DCT_ADST
    0, // DC   -> DCT_DCT
    0, // D45  -> DCT_DCT
    3, // D135 -> ADST_ADST
    1, // D113 -> ADST_DCT
    2, // D157 -> DCT_ADST
    1, // D203 -> ADST_DCT
    2, // D67  -> DCT_ADST
    3, // TM   -> ADST_ADST
    0, 0, 0, 0, // inter modes: DCT_DCT
];

TREES
echo "/// DC dequantization lookup, 8-bit index (spec section 13.7)."
emit DC_QLOOKUP i16 ff_vp9_dc_qlookup libavcodec/vp9data.c /tmp/vp9tab_dcq.txt 16
echo "/// AC dequantization lookup, 8-bit index (spec section 13.7)."
emit AC_QLOOKUP i16 ff_vp9_ac_qlookup libavcodec/vp9data.c /tmp/vp9tab_acq.txt 16
for s in default_scan_4x4 col_scan_4x4 row_scan_4x4 default_scan_8x8 col_scan_8x8 row_scan_8x8 default_scan_16x16 col_scan_16x16 row_scan_16x16 default_scan_32x32; do
    up=$(echo "$s" | tr a-z A-Z)
    echo "/// Spec scan order: $s."
    emit "${up^^}" i16 "ff_vp9_$s" libavcodec/vp9data.c "/tmp/vp9tab_$s.txt" 16
done
for s in default_scan_4x4_nb col_scan_4x4_nb row_scan_4x4_nb default_scan_8x8_nb col_scan_8x8_nb row_scan_8x8_nb default_scan_16x16_nb col_scan_16x16_nb row_scan_16x16_nb default_scan_32x32_nb; do
    up=$(echo "$s" | tr a-z A-Z)
    echo "/// Neighbours-in-scan for $s, flattened [n][2] (0 = none)."
    emit "${up^^}" i16 "ff_vp9_$s" libavcodec/vp9data.c "/tmp/vp9tab_$s.txt" 16
done
echo "/// Sub-pel interpolation filters [type][phase][8] flattened"
echo "/// (rows: 0 = REGULAR, 1 = SHARP, 2 = SMOOTH)."
# The extractor cannot parse the DECLARE_ALIGNED declaration style, so grab
# the initializer directly from the pinned vp9dsp.c text.
NOTABLE=1; sed -n "/ff_vp9_subpel_filters).* = {/,/^};/p" "$DSP" | grep -v "FILTER_8TAP" > /tmp/vp9tab_subpel.txt
emit SUBPEL_FILTERS i16 ff_vp9_subpel_filters libavcodec/vp9dsp.c /tmp/vp9tab_subpel.txt 8
echo "/// Differential probability update inverse map (spec section 13.2)."
emit INV_MAP_TABLE u8 inv_map_table libavcodec/vp9.c /tmp/vp9tab_invmap.txt 14
cat <<'TAIL'
/// Coefficient probability model, compact FFmpeg layout: per
/// (tx group, block type, plane type) band 0 has 3 contexts and bands 1..5
/// have 6 (band-0 contexts 3..5 are never coded). Index with
/// [`coef_prob_idx`].
TAIL
emit DEFAULT_COEF_PROBS u8 ff_vp9_default_coef_probs libavcodec/vp9data.c /tmp/vp9tab_default_coef_probs.txt 15
echo "/// Pareto-8 expansion of the 'ONE vs higher' model prob into tree nodes 3..10."
emit MODEL_PARETO8 u8 ff_vp9_model_pareto8 libavcodec/vp9data.c /tmp/vp9tab_model_pareto8.txt 16
} >> src/tables.rs
echo "wrote src/tables.rs ($(wc -l < src/tables.rs) lines)"
