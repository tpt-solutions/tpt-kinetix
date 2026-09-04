"""Independent AV1 `coeffs()` re-implementation — AV1 spec §5.11.39.

Control flow (context derivation + symbol read order) is ported by hand from
tpt-kinetix-av1/src/coeff.rs::read_coeffs. Table values come from
cdf_tables_gen (exported from the Rust crate). See that module's header for
the independence caveat: this catches context-derivation / read-order bugs,
not table-value transcription errors (a separate spec-PDF pass is tracked for
the latter).
"""

import copy
import os
import sys

from cdf_tables_gen import (
    TX_WIDTH, TX_HEIGHT, TX_WIDTH_LOG2, TX_HEIGHT_LOG2, TX_SIZE_SQR,
    TX_SIZE_SQR_UP, ADJUSTED_TX_SIZE, DCT_DCT, TX_CLASS_HORIZ,
    TX_CLASS_VERT, TX_CLASS_2D, TX_SET_DCTONLY, TX_SET_INTRA_1, TX_SET_INTRA_2,
    NUM_BASE_LEVELS, COEFF_BASE_RANGE, BR_CDF_SIZE, SIG_COEF_CONTEXTS,
    SIG_COEF_CONTEXTS_EOB, SIG_REF_DIFF_OFFSET, MAG_REF_OFFSET_WITH_TX_CLASS,
    COEFF_BASE_CTX_OFFSET, COEFF_BASE_POS_CTX_OFFSET, MODE_TO_TXFM,
    TX_TYPE_IN_SET_INTRA, TX_TYPE_INTRA_INV_SET1, TX_TYPE_INTRA_INV_SET2,
    SCANS, DEFAULT_TXB_SKIP_CDF, DEFAULT_EOB_PT_16_CDF, DEFAULT_EOB_PT_32_CDF,
    DEFAULT_EOB_PT_64_CDF, DEFAULT_EOB_PT_128_CDF, DEFAULT_EOB_PT_256_CDF,
    DEFAULT_EOB_PT_512_CDF, DEFAULT_EOB_PT_1024_CDF, DEFAULT_EOB_EXTRA_CDF,
    DEFAULT_COEFF_BASE_EOB_CDF, DEFAULT_COEFF_BASE_CDF, DEFAULT_COEFF_BR_CDF,
    DEFAULT_DC_SIGN_CDF, DEFAULT_INTRA_TX_TYPE_SET1_CDF,
    DEFAULT_INTRA_TX_TYPE_SET2_CDF,
)
from symbol_decoder import SymbolDecoder

# AV1 TxType enum (spec "Intra/Inter prediction mode / transform type").
DCT_DCT, ADST_DCT, DCT_ADST, ADST_ADST = 0, 1, 2, 3
FLIPADST_DCT, DCT_FLIPADST, FLIPADST_FLIPADST = 4, 5, 6
ADST_FLIPADST, FLIPADST_ADST = 7, 8
IDTX, V_DCT, H_DCT = 9, 10, 11
V_ADST, H_ADST = 12, 13
V_FLIPADST, H_FLIPADST = 14, 15

# AV1 TxSize enum (spec).
TX_4X4, TX_8X8, TX_16X16, TX_32X32, TX_64X64 = 0, 1, 2, 3, 4
TX_4X8, TX_8X4, TX_8X16, TX_16X8, TX_16X32, TX_32X16 = 5, 6, 7, 8, 9, 10
TX_4X16, TX_16X4, TX_8X32, TX_32X8, TX_16X64, TX_64X16 = 11, 12, 13, 14, 15, 16
TX_32X64, TX_64X32 = 17, 18

NUM_PLANES = 3
MAX_GOLOMB_LENGTH = 24
TX_16X64 = 17
TX_64X16 = 16
TX_32X32 = 3


def q_context(base_q_idx: int) -> int:
    if base_q_idx <= 20:
        return 0
    if base_q_idx <= 60:
        return 1
    if base_q_idx <= 120:
        return 2
    return 3


class TileCdfs:
    """Working (adapting) coefficient CDF set for one tile, seeded from q."""

    def __init__(self, base_q_idx: int):
        idx = q_context(base_q_idx)
        self.txb_skip = copy.deepcopy(DEFAULT_TXB_SKIP_CDF[idx])
        self.eob_pt_16 = copy.deepcopy(DEFAULT_EOB_PT_16_CDF[idx])
        self.eob_pt_32 = copy.deepcopy(DEFAULT_EOB_PT_32_CDF[idx])
        self.eob_pt_64 = copy.deepcopy(DEFAULT_EOB_PT_64_CDF[idx])
        self.eob_pt_128 = copy.deepcopy(DEFAULT_EOB_PT_128_CDF[idx])
        self.eob_pt_256 = copy.deepcopy(DEFAULT_EOB_PT_256_CDF[idx])
        self.eob_pt_512 = copy.deepcopy(DEFAULT_EOB_PT_512_CDF[idx])
        self.eob_pt_1024 = copy.deepcopy(DEFAULT_EOB_PT_1024_CDF[idx])
        self.eob_extra = copy.deepcopy(DEFAULT_EOB_EXTRA_CDF[idx])
        self.coeff_base_eob = copy.deepcopy(DEFAULT_COEFF_BASE_EOB_CDF[idx])
        self.coeff_base = copy.deepcopy(DEFAULT_COEFF_BASE_CDF[idx])
        self.coeff_br = copy.deepcopy(DEFAULT_COEFF_BR_CDF[idx])
        self.dc_sign = copy.deepcopy(DEFAULT_DC_SIGN_CDF[idx])
        self.intra_tx_type_set1 = copy.deepcopy(DEFAULT_INTRA_TX_TYPE_SET1_CDF)
        self.intra_tx_type_set2 = copy.deepcopy(DEFAULT_INTRA_TX_TYPE_SET2_CDF)

    def load_snapshot(self, snap: dict) -> None:
        """Overwrite the working tables with Kinetix's captured (adapted) CDF
        state, so an independent re-decode starts from the same probability
        tables as the real decoder at the captured block (removes the CDF-
        adaptation confound from the symbol diff)."""
        self.txb_skip = copy.deepcopy(snap["txb_skip"])
        self.eob_pt_16 = copy.deepcopy(snap["eob_pt_16"])
        self.eob_pt_32 = copy.deepcopy(snap["eob_pt_32"])
        self.eob_pt_64 = copy.deepcopy(snap["eob_pt_64"])
        self.eob_pt_128 = copy.deepcopy(snap["eob_pt_128"])
        self.eob_pt_256 = copy.deepcopy(snap["eob_pt_256"])
        self.eob_pt_512 = copy.deepcopy(snap["eob_pt_512"])
        self.eob_pt_1024 = copy.deepcopy(snap["eob_pt_1024"])
        self.eob_extra = copy.deepcopy(snap["eob_extra"])
        self.coeff_base_eob = copy.deepcopy(snap["coeff_base_eob"])
        self.coeff_base = copy.deepcopy(snap["coeff_base"])
        self.coeff_br = copy.deepcopy(snap["coeff_br"])
        self.dc_sign = copy.deepcopy(snap["dc_sign"])
        self.intra_tx_type_set1 = copy.deepcopy(snap["intra_tx_type_set1"])
        self.intra_tx_type_set2 = copy.deepcopy(snap["intra_tx_type_set2"])


class CoeffContexts:
    def __init__(self, width4: int, height4: int):
        w = max(width4, 1)
        h = max(height4, 1)
        self.above_level = [[0] * w for _ in range(NUM_PLANES)]
        self.above_dc = [[0] * w for _ in range(NUM_PLANES)]
        self.left_level = [[0] * h for _ in range(NUM_PLANES)]
        self.left_dc = [[0] * h for _ in range(NUM_PLANES)]

    def clear_left(self):
        for plane in range(NUM_PLANES):
            self.left_level[plane] = [0] * len(self.left_level[plane])
            self.left_dc[plane] = [0] * len(self.left_dc[plane])

    def clear_above(self):
        for plane in range(NUM_PLANES):
            self.above_level[plane] = [0] * len(self.above_level[plane])
            self.above_dc[plane] = [0] * len(self.above_dc[plane])

    @staticmethod
    def _get(store, plane, i):
        if 0 <= plane < len(store) and 0 <= i < len(store[plane]):
            return store[plane][i]
        return 0

    @staticmethod
    def _set(store, plane, i, v):
        if 0 <= plane < len(store) and 0 <= i < len(store[plane]):
            store[plane][i] = v


def get_tx_class(tx_type: int) -> int:
    if tx_type in (V_DCT, V_ADST, V_FLIPADST):
        return TX_CLASS_VERT
    if tx_type in (H_DCT, H_ADST, H_FLIPADST):
        return TX_CLASS_HORIZ
    return TX_CLASS_2D


def get_tx_set_intra(tx_size: int, reduced: bool) -> int:
    tx_sz_sqr = TX_SIZE_SQR[tx_size]
    tx_sz_sqr_up = TX_SIZE_SQR_UP[tx_size]
    if tx_sz_sqr_up > TX_32X32 or tx_sz_sqr_up == TX_32X32:
        return TX_SET_DCTONLY
    if reduced:
        return TX_SET_INTRA_2
    if tx_sz_sqr == TX_16X16:
        return TX_SET_INTRA_2
    return TX_SET_INTRA_1


def get_scan(tx_size: int, tx_type: int):
    return SCANS.get((tx_size, tx_type))


def _all_zero_ctx(blk, ctxs, w4, h4) -> int:
    plane = blk["plane"]
    w = TX_WIDTH[blk["tx_size"]]
    h = TX_HEIGHT[blk["tx_size"]]
    if plane == 0:
        top = 0
        left = 0
        for k in range(w4):
            if blk["x4"] + k < blk["max_x4"]:
                top = max(top, CoeffContexts._get(ctxs.above_level, plane, blk["x4"] + k))
        for k in range(h4):
            if blk["y4"] + k < blk["max_y4"]:
                left = max(left, CoeffContexts._get(ctxs.left_level, plane, blk["y4"] + k))
        top = min(top, 255)
        left = min(left, 255)
        if blk["block_w"] == w and blk["block_h"] == h:
            return 0
        if top == 0 and left == 0:
            return 1
        if top == 0 or left == 0:
            return 2 + (1 if max(top, left) > 3 else 0)
        if max(top, left) <= 3:
            return 4
        if min(top, left) <= 3:
            return 5
        return 6
    else:
        above = 0
        left = 0
        for i in range(w4):
            if blk["x4"] + i < blk["max_x4"]:
                above |= CoeffContexts._get(ctxs.above_level, plane, blk["x4"] + i)
                above |= CoeffContexts._get(ctxs.above_dc, plane, blk["x4"] + i)
        for i in range(h4):
            if blk["y4"] + i < blk["max_y4"]:
                left |= CoeffContexts._get(ctxs.left_level, plane, blk["y4"] + i)
                left |= CoeffContexts._get(ctxs.left_dc, plane, blk["y4"] + i)
        ctx = (1 if above != 0 else 0) + (1 if left != 0 else 0) + 7
        if blk["block_w"] * blk["block_h"] > w * h:
            ctx += 3
        return ctx


def _dc_sign_ctx(blk, ctxs, w4, h4) -> int:
    plane = blk["plane"]
    dc_sign = 0
    for k in range(w4):
        if blk["x4"] + k < blk["max_x4"]:
            v = CoeffContexts._get(ctxs.above_dc, plane, blk["x4"] + k)
            if v == 1:
                dc_sign -= 1
            elif v == 2:
                dc_sign += 1
    for k in range(h4):
        if blk["y4"] + k < blk["max_y4"]:
            v = CoeffContexts._get(ctxs.left_dc, plane, blk["y4"] + k)
            if v == 1:
                dc_sign -= 1
            elif v == 2:
                dc_sign += 1
    if dc_sign < 0:
        return 1
    if dc_sign > 0:
        return 2
    return 0


def _coeff_base_ctx(tx_size, plane_tx_type, quant, pos, c, is_eob) -> int:
    adj = ADJUSTED_TX_SIZE[tx_size]
    bwl = TX_WIDTH_LOG2[adj]
    width = 1 << bwl
    height = TX_HEIGHT[adj]
    if is_eob:
        if c == 0:
            return SIG_COEF_CONTEXTS - 4
        if c <= (height << bwl) // 8:
            return SIG_COEF_CONTEXTS - 3
        if c <= (height << bwl) // 4:
            return SIG_COEF_CONTEXTS - 2
        return SIG_COEF_CONTEXTS - 1
    tx_class = get_tx_class(plane_tx_type)
    row = pos >> bwl
    col = pos - (row << bwl)
    mag = 0
    for ref_d_row, ref_d_col in SIG_REF_DIFF_OFFSET[tx_class]:
        ref_row = row + ref_d_row
        ref_col = col + ref_d_col
        if ref_row >= 0 and ref_col >= 0 and ref_row < height and ref_col < width:
            sample = abs(quant[(ref_row << bwl) + ref_col])
            mag += min(sample, 3)
    ctx = min((mag + 1) >> 1, 4)

    if tx_class == TX_CLASS_2D:
        if row == 0 and col == 0:
            return 0
        return ctx + COEFF_BASE_CTX_OFFSET[tx_size][min(row, 4)][min(col, 4)]
    idx = row if tx_class == TX_CLASS_VERT else col
    return ctx + COEFF_BASE_POS_CTX_OFFSET[min(idx, 2)]


def _coeff_br_ctx(tx_size, plane_tx_type, quant, pos) -> int:
    adj = ADJUSTED_TX_SIZE[tx_size]
    bwl = TX_WIDTH_LOG2[adj]
    txw = TX_WIDTH[adj]
    txh = TX_HEIGHT[adj]
    row = pos >> bwl
    col = pos - (row << bwl)
    tx_class = get_tx_class(plane_tx_type)
    limit = COEFF_BASE_RANGE + NUM_BASE_LEVELS + 1
    mag = 0
    for ref_d_row, ref_d_col in MAG_REF_OFFSET_WITH_TX_CLASS[tx_class]:
        ref_row = row + ref_d_row
        ref_col = col + ref_d_col
        if ref_row >= 0 and ref_col >= 0 and ref_row < txh and ref_col < (1 << bwl):
            mag += min(quant[ref_row * txw + ref_col], limit)
    mag = min((mag + 1) >> 1, 6)
    if pos == 0:
        return mag
    if tx_class == TX_CLASS_2D:
        if row < 2 and col < 2:
            return mag + 7
        return mag + 14
    if tx_class == TX_CLASS_HORIZ:
        if col == 0:
            return mag + 7
        return mag + 14
    if row == 0:
        return mag + 7
    return mag + 14


def _read_transform_type(dec, cdfs, blk, tx_size) -> int:
    set_ = get_tx_set_intra(tx_size, blk["reduced_tx_set"])
    if set_ == 0 or not blk["qindex_positive"]:
        return DCT_DCT
    sqr = TX_SIZE_SQR[tx_size]
    dire = blk["intra_dir"]
    if set_ == TX_SET_INTRA_1:
        cdf = cdfs.intra_tx_type_set1[sqr][dire]
        return TX_TYPE_INTRA_INV_SET1[dec.read_symbol(cdf)]
    cdf = cdfs.intra_tx_type_set2[sqr][dire]
    return TX_TYPE_INTRA_INV_SET2[dec.read_symbol(cdf)]


def _compute_tx_type(blk, tx_size, luma_tx_type) -> int:
    if blk["lossless"] or TX_SIZE_SQR_UP[tx_size] > TX_32X32:
        return DCT_DCT
    if blk["plane"] == 0:
        return luma_tx_type
    tx_set = get_tx_set_intra(tx_size, blk["reduced_tx_set"])
    tx_type = MODE_TO_TXFM[blk["uv_mode"]]
    if TX_TYPE_IN_SET_INTRA[tx_set][tx_type] == 0:
        return DCT_DCT
    return tx_type


def _read_eob(dec, cdfs, tx_size, tx_sz_ctx, ptype, tx_type) -> int:
    eob_multisize = min(TX_WIDTH_LOG2[tx_size], 5) + min(TX_HEIGHT_LOG2[tx_size], 5) - 4
    ctx = 1 if get_tx_class(tx_type) != TX_CLASS_2D else 0
    if eob_multisize == 0:
        eob_pt = 1 + dec.read_symbol(cdfs.eob_pt_16[ptype][ctx])
    elif eob_multisize == 1:
        eob_pt = 1 + dec.read_symbol(cdfs.eob_pt_32[ptype][ctx])
    elif eob_multisize == 2:
        eob_pt = 1 + dec.read_symbol(cdfs.eob_pt_64[ptype][ctx])
    elif eob_multisize == 3:
        eob_pt = 1 + dec.read_symbol(cdfs.eob_pt_128[ptype][ctx])
    elif eob_multisize == 4:
        eob_pt = 1 + dec.read_symbol(cdfs.eob_pt_256[ptype][ctx])
    elif eob_multisize == 5:
        eob_pt = 1 + dec.read_symbol(cdfs.eob_pt_512[ptype])
    else:
        eob_pt = 1 + dec.read_symbol(cdfs.eob_pt_1024[ptype])

    if eob_pt < 2:
        eob = eob_pt
    else:
        eob = (1 << (eob_pt - 2)) + 1

    if eob_pt >= 3:
        eob_shift = eob_pt - 3
        extra_cdf = cdfs.eob_extra[tx_sz_ctx][ptype][eob_shift]
        if dec.read_symbol(extra_cdf) == 1:
            eob += 1 << eob_shift
        for i in range(1, eob_pt - 2):
            shift = (eob_pt - 2) - 1 - i
            if dec.read_literal(1) == 1:
                eob += 1 << shift
    return eob


def _read_golomb_tail(dec) -> int:
    length = 0
    while True:
        length += 1
        if dec.read_literal(1) == 1:
            break
        if length > MAX_GOLOMB_LENGTH:
            raise ValueError("Exp-Golomb prefix too long")
    x = 1
    for _ in range(length - 1):
        x = (x << 1) | dec.read_literal(1)
    return x + COEFF_BASE_RANGE + NUM_BASE_LEVELS


def read_coeffs(dec, cdfs, ctxs, blk):
    tx_size = blk["tx_size"]
    plane = blk["plane"]
    tx_w = TX_WIDTH[tx_size]
    tx_h = TX_HEIGHT[tx_size]
    w4 = tx_w >> 2
    h4 = tx_h >> 2
    num_coeffs = tx_w * tx_h
    tx_sz_ctx = (TX_SIZE_SQR[tx_size] + TX_SIZE_SQR_UP[tx_size] + 1) >> 1
    ptype = 1 if plane > 0 else 0
    seg_eob = 512 if (tx_size == TX_16X64 or tx_size == TX_64X16) else min(num_coeffs, 1024)

    quant = [0] * num_coeffs
    eob = 0
    cul_level = 0
    dc_category = 0
    tx_type = DCT_DCT

    skip_ctx = _all_zero_ctx(blk, ctxs, w4, h4)
    if os.environ.get("KINETIX_AV1_DBG_ALLZERO"):
        print(
            f"DBG allzero plane={plane} x4={blk['x4']} y4={blk['y4']} "
            f"tx_sz_ctx={tx_sz_ctx} skip_ctx={skip_ctx} "
            f"cdf={cdfs.txb_skip[tx_sz_ctx][skip_ctx]} "
            f"rng={dec.symbol_range} val={dec.symbol_value} "
            f"trace_idx={len(dec.trace)} bit_pos={dec.bit_pos}",
            file=sys.stderr,
        )
    all_zero = dec.read_symbol(cdfs.txb_skip[tx_sz_ctx][skip_ctx]) == 1

    if not all_zero:
        luma_tx_type = _read_transform_type(dec, cdfs, blk, tx_size) if plane == 0 else DCT_DCT
        tx_type = _compute_tx_type(blk, tx_size, luma_tx_type)

        scan = get_scan(tx_size, tx_type)
        if scan is None:
            raise ValueError(f"no scan for tx_size={tx_size} tx_type={tx_type}")
        eob = _read_eob(dec, cdfs, tx_size, tx_sz_ctx, ptype, tx_type)
        if eob > seg_eob or eob > len(scan):
            raise ValueError(f"eob {eob} exceeds seg limit {min(seg_eob, len(scan))}")

        for c in range(eob - 1, -1, -1):
            pos = scan[c]
            if c == eob - 1:
                ctx = _coeff_base_ctx(tx_size, tx_type, quant, pos, c, True) + SIG_COEF_CONTEXTS_EOB - SIG_COEF_CONTEXTS
                level = dec.read_symbol(cdfs.coeff_base_eob[tx_sz_ctx][ptype][ctx]) + 1
            else:
                ctx = _coeff_base_ctx(tx_size, tx_type, quant, pos, c, False)
                level = dec.read_symbol(cdfs.coeff_base[tx_sz_ctx][ptype][ctx])

            if level > NUM_BASE_LEVELS:
                br_ctx = _coeff_br_ctx(tx_size, tx_type, quant, pos)
                br_tx_ctx = min(tx_sz_ctx, TX_32X32)
                for _ in range(COEFF_BASE_RANGE // (BR_CDF_SIZE - 1)):
                    coeff_br = dec.read_symbol(cdfs.coeff_br[br_tx_ctx][ptype][br_ctx])
                    level += coeff_br
                    if coeff_br < BR_CDF_SIZE - 1:
                        break

            quant[pos] = level

        for raw_pos in scan[:eob]:
            pos = raw_pos
            sign = False
            if quant[pos] != 0:
                if pos == scan[0] and eob > 0:
                    ctx = _dc_sign_ctx(blk, ctxs, w4, h4)
                    sign = dec.read_symbol(cdfs.dc_sign[ptype][ctx]) == 1
                else:
                    sign = dec.read_literal(1) == 1

            if quant[pos] > NUM_BASE_LEVELS + COEFF_BASE_RANGE:
                quant[pos] = _read_golomb_tail(dec)

            if pos == 0 and quant[pos] > 0:
                dc_category = 1 if sign else 2
            quant[pos] &= 0xFFFFF
            cul_level += quant[pos]
            if sign:
                quant[pos] = -quant[pos]

        cul_level = min(cul_level, 63)

    cul_level = cul_level
    for i in range(w4):
        CoeffContexts._set(ctxs.above_level, plane, blk["x4"] + i, cul_level)
        CoeffContexts._set(ctxs.above_dc, plane, blk["x4"] + i, dc_category)
    for i in range(h4):
        CoeffContexts._set(ctxs.left_level, plane, blk["y4"] + i, cul_level)
        CoeffContexts._set(ctxs.left_dc, plane, blk["y4"] + i, dc_category)

    return {"quant": quant, "eob": eob, "tx_type": tx_type}
