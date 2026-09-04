"""Independent AV1 keyframe-intra tile decoder — the "Part 1 oracle".

Consumes `av1_tile_trace.json` (written by Kinetix when `KINETIX_AV1_CAPTURE_TILE`
is set — see `reconstruct/mod.rs::capture_tile_trace`), which carries:
  * the raw tile entropy payload + starting bit offset,
  * the *base* (un-adapted) mode + coefficient CDF tables,
  * frame/tile `params`,
  * Kinetix's full per-`read_symbol` trace `[n_symbols, value, bit_before, bit_after]`,
  * block markers `{seq, label}`.

This module re-implements the tile's syntax (`read_lr` §5.11.57 → `decode_partition`
§5.11.4 → `intra_frame_mode_info` §5.11.7 → `coeffs` §5.11.39) from scratch, driven
by an independent `SymbolDecoder`, and diffs its own per-symbol trace against
Kinetix's. The first `(n_symbols, value)` or bit-position mismatch, plus the
nearest preceding block marker, localizes a read-order / context-derivation bug.

LIMITATION: the CDF *tables* come from Kinetix's capture, so a numerically wrong
default CDF entry is invisible here (that needs a spec-PDF diff, done separately).
This catches context-selection and read-order bugs, which is the dominant open
hypothesis for the `mi=(8,18)` desync.

Run: `python tools/av1_oracle/intra_decode.py av1_tile_trace.json`
"""

import copy
import json
import os
import re
import sys

from symbol_decoder import SymbolDecoder
from coeffs import TileCdfs, CoeffContexts, read_coeffs
from cdf_tables_gen import TX_WIDTH, TX_HEIGHT

# ── constants (spec tables; stable since AV1 1.0) ───────────────────────────
MI_SIZE = 4
BW = [4, 4, 8, 8, 8, 16, 16, 16, 32, 32, 32, 64, 64, 64, 128, 128, 4, 16, 8, 32, 16, 64]
BH = [4, 8, 4, 8, 16, 8, 16, 32, 16, 32, 64, 32, 64, 128, 64, 128, 16, 4, 32, 8, 64, 16]
BLOCK_8X8 = 3
BLOCK_128X128 = 15
BLOCK_INVALID = -1

# NB: Kinetix's TxSize enum ordering (coeff_tables.rs) differs from the AV1
# spec's — TX_32X64/TX_64X32 sit at 11/12 where the spec puts TX_4X16/TX_16X4.
# The oracle must use Kinetix's ordering to diff against Kinetix's decode.
TX_4X4, TX_8X8, TX_16X16, TX_32X32, TX_64X64 = 0, 1, 2, 3, 4
TX_4X8, TX_8X4, TX_8X16, TX_16X8, TX_16X32, TX_32X16 = 5, 6, 7, 8, 9, 10
TX_32X64, TX_64X32, TX_4X16, TX_16X4, TX_8X32, TX_32X8 = 11, 12, 13, 14, 15, 16
TX_16X64, TX_64X16 = 17, 18
# Max_Tx_Size_Rect[bsize] -> Kinetix TxSize
MAX_TX_SIZE_RECT = [0, 5, 6, 1, 7, 8, 2, 9, 10, 3, 11, 12, 4, 4, 4, 4, 13, 14, 15, 16, 17, 18]
SPLIT_TX_SIZE = [0, 0, 1, 2, 3, 0, 0, 1, 1, 2, 2, 3, 3, 5, 6, 7, 8, 9, 10]
MAX_TX_DEPTH_TABLE = [0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4, 4, 4, 4, 2, 2, 3, 3, 4, 4]

DC_PRED, V_PRED, H_PRED = 0, 1, 2
D157_PRED, D67_PRED = 6, 8
UV_CFL_PRED = 13
CFL_SIGN_ZERO = 0
MAX_ANGLE_DELTA = 3

INTRA_MODE_CONTEXT = [0, 1, 2, 3, 4, 4, 4, 4, 3, 0, 1, 2, 0]
PARTITION_CDF_LOOKUP = [0, 0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4, 0, 0, 1, 1, 2, 2]
FILTER_INTRA_MODE_TO_INTRA_DIR = [DC_PRED, V_PRED, H_PRED, D157_PRED, DC_PRED]

(PART_NONE, PART_HORZ, PART_VERT, PART_SPLIT, PART_HORZ_A, PART_HORZ_B,
 PART_VERT_A, PART_VERT_B, PART_HORZ_4, PART_VERT_4) = range(10)

# Subsampled_Size[bsize][subX][subY] -> bsize
_I = BLOCK_INVALID
SUBSAMPLED_SIZE = [
    [[0, 0], [0, 0]], [[1, 0], [_I, 0]], [[2, _I], [0, 0]], [[3, 2], [1, 0]],
    [[4, 3], [_I, 1]], [[5, _I], [3, 2]], [[6, 5], [4, 3]], [[7, 6], [_I, 4]],
    [[8, _I], [6, 5]], [[9, 8], [7, 6]], [[10, 9], [_I, 7]], [[11, _I], [9, 8]],
    [[12, 11], [10, 9]], [[13, 12], [_I, 10]], [[14, _I], [12, 11]],
    [[15, 14], [13, 12]], [[16, 1], [_I, 1]], [[17, _I], [2, 2]],
    [[18, 4], [_I, 16]], [[19, _I], [5, 17]], [[20, 7], [_I, 18]],
    [[21, _I], [8, 19]],
]

PALETTE_COLOR_HASH_MULTIPLIERS = [1, 2, 2]
PALETTE_COLOR_CONTEXT = [-1, -1, 0, -1, -1, 4, 3, 2, 1]
PALETTE_NUM_NEIGHBORS = 3
PALETTE_COLORS = 8

WIENER_TAPS_MIN = [-5, -23, -17]
WIENER_TAPS_MAX = [10, 8, 46]
WIENER_TAPS_K = [1, 2, 3]
SGRPROJ_XQD_MIN = [-96, -32]
SGRPROJ_XQD_MAX = [31, 95]
SGRPROJ_PRJ_SUBEXP_K = 4
SGRPROJ_PARAMS_BITS = 4
SGRPROJ_PRJ_BITS = 7
SGR_PARAMS = [[2, 12, 1, 4], [2, 15, 1, 6], [2, 18, 1, 8], [2, 21, 1, 9],
              [2, 24, 1, 10], [2, 29, 1, 11], [2, 36, 1, 12], [2, 45, 1, 13],
              [2, 56, 1, 14], [2, 68, 1, 15], [0, 0, 1, 5], [0, 0, 1, 8],
              [0, 0, 1, 11], [0, 0, 1, 14], [2, 30, 0, 0], [2, 75, 0, 0]]
BIT_DEPTH = 8


def ilog2(x):
    return x.bit_length() - 1


def mi_log2_w(bs):
    return ilog2(BW[bs] // MI_SIZE)


def mi_log2_h(bs):
    return ilog2(BH[bs] // MI_SIZE)


def is_directional(m):
    return V_PRED <= m <= D67_PRED


def clip1(x):
    return max(0, min((1 << BIT_DEPTH) - 1, x))


def clip3(lo, hi, x):
    return max(lo, min(hi, x))


def ceil_log2(x):
    if x < 2:
        return 0
    i, p = 1, 2
    while p < x:
        i += 1
        p <<= 1
    return i


def read_ns(dec, n):
    if n <= 1:
        return 0
    w = ilog2(n) + 1
    m = (1 << w) - n
    v = dec.read_literal(w - 1)
    if v < m:
        return v
    return (v << 1) - m + dec.read_literal(1)


def inverse_recenter(r, v):
    if v > 2 * r:
        return v
    if v & 1:
        return r - ((v + 1) >> 1)
    return r + (v >> 1)


def bsize_from_wh(w, h):
    for i in range(22):
        if BW[i] == w and BH[i] == h:
            return i
    return BLOCK_8X8


def get_plane_residual_size(bsize, sx, sy):
    return SUBSAMPLED_SIZE[bsize][sx][sy]


def chroma_tx_size(bsize, sx, sy):
    ps = get_plane_residual_size(bsize, sx, sy)
    if ps == BLOCK_INVALID:
        ps = bsize
    uv = MAX_TX_SIZE_RECT[ps]
    tw, th = TX_WIDTH[uv], TX_HEIGHT[uv]
    if tw == 64 or th == 64:
        if tw == 16:
            return TX_16X32
        if th == 16:
            return TX_32X16
        return TX_32X32
    return uv


def has_chroma(bsize, mi_row, mi_col, sx, sy):
    bw4 = BW[bsize] // MI_SIZE
    bh4 = BH[bsize] // MI_SIZE
    luma_only = (bh4 == 1 and sy and (mi_row & 1) == 0) or \
                (bw4 == 1 and sx and (mi_col & 1) == 0)
    return not luma_only


def cfl_allowed(bsize):
    return BW[bsize] <= 32 and BH[bsize] <= 32


# ── mode CDF wrapper ───────────────────────────────────────────────────────
class ModeCdfs:
    def __init__(self, snap):
        for k, v in snap.items():
            setattr(self, k, copy.deepcopy(v))

    def base_partition(self, bucket, ctx):
        return [self.partition_w8, self.partition_w16, self.partition_w32,
                self.partition_w64, self.partition_w128][bucket][ctx]


def split_into_subblocks(bw, bh, part):
    hw, hh = bw // 2, bh // 2
    qw, qh = bw // 4, bh // 4
    def b(w, h):
        return bsize_from_wh(w * 4, h * 4)
    if part == PART_NONE:
        return [(b(bw, bh), 0, 0)]
    if part == PART_HORZ:
        return [(b(bw, hh), 0, 0), (b(bw, hh), hh, 0)]
    if part == PART_VERT:
        return [(b(hw, bh), 0, 0), (b(hw, bh), 0, hw)]
    if part == PART_SPLIT:
        return [(b(hw, hh), 0, 0), (b(hw, hh), 0, hw),
                (b(hw, hh), hh, 0), (b(hw, hh), hh, hw)]
    if part == PART_HORZ_A:
        return [(b(hw, hh), 0, 0), (b(hw, hh), 0, hw), (b(bw, hh), hh, 0)]
    if part == PART_HORZ_B:
        return [(b(bw, hh), 0, 0), (b(hw, hh), hh, 0), (b(hw, hh), hh, hw)]
    if part == PART_VERT_A:
        return [(b(hw, hh), 0, 0), (b(hw, hh), hh, 0), (b(hw, bh), 0, hw)]
    if part == PART_VERT_B:
        return [(b(hw, bh), 0, 0), (b(hw, hh), 0, hw), (b(hw, hh), hh, hw)]
    if part == PART_HORZ_4:
        return [(b(bw, qh), i * qh, 0) for i in range(4)]
    if part == PART_VERT_4:
        return [(b(qw, bh), 0, i * qw) for i in range(4)]
    return [(b(bw, bh), 0, 0)]


class Tile:
    def __init__(self, spec):
        p = spec["params"]
        self.p = p
        self.mi_cols = p["mi_cols"]
        self.mi_rows = p["mi_rows"]
        self.sx = 1 if p["subsampling_x"] else 0
        self.sy = 1 if p["subsampling_y"] else 0
        self.sb_mi = p["sb_size"] // MI_SIZE
        self.sb_bsize = BLOCK_128X128 if p["use_128"] else 12  # BLOCK_64X64
        self.tx_mode_select = p["tx_mode_select"]
        self.lossless = p["lossless"]
        self.reduced_tx_set = p["reduced_tx_set"]
        self.enable_filter_intra = p["enable_filter_intra"]
        self.allow_screen = p["allow_screen_content_tools"]
        self.allow_intrabc = p["allow_intrabc"]
        self.seg = p["segmentation_enabled"]
        self.base_q = spec["base_q_idx"]
        # loop restoration (from params, emitted by capture_tile_trace)
        self.frt = p.get("frame_restoration_type", [0, 0, 0])
        self.lr_size = p.get("lr_unit_size", [256, 256, 256])
        self.lr_uses = p.get("uses_lr", any(self.frt))
        self.delta_q_present = p.get("delta_q_present", False)

        data = bytes.fromhex(spec["data_hex"])
        self.dec = SymbolDecoder(data, spec["bit_offset"])
        self.mc = ModeCdfs(spec["mode_cdfs"])
        self.cc = TileCdfs(self.base_q)
        self.cc.load_snapshot(spec["coeff_cdfs"])
        self.ctxs = CoeffContexts((p["width"] + 3) // 4, (p["height"] + 3) // 4)

        n = self.mi_cols
        m = self.mi_rows
        self.mi_w_log2_above = [0] * n
        self.mi_h_log2_left = [0] * m
        self.ymode_above = [DC_PRED] * n
        self.ymode_left = [DC_PRED] * m
        self.uv_above = [DC_PRED] * n
        self.uv_left = [DC_PRED] * m
        self.skip_above = [0] * n
        self.skip_left = [0] * m
        # An initial / tile-edge-unavailable tx neighbour must contribute 0 to
        # the tx_depth context (AV1 spec section 8.3.2's Get_Tx_Skip_Ctx-
        # style availability rule): a sentinel of 4 wrongly satisfies
        # `>= TX_WIDTH[max_tx]`/`>= TX_HEIGHT[max_tx]` for max_tx==TX_4X4 and
        # reads tx_depth from the wrong CDF bucket for every block on the
        # frame's first row/column (Kinetix hit and fixed this same bug --
        # see partition.rs's `tx_depth_ctx_from` regression test).
        self.tx_above = [0] * n
        self.tx_left = [0] * m
        self.pal_y_above = [[] for _ in range(n)]
        self.pal_y_left = [[] for _ in range(m)]
        self.pal_u_above = [[] for _ in range(n)]
        self.pal_u_left = [[] for _ in range(m)]

        self.luma_max_x4 = (p["width"] + 3) // 4
        self.luma_max_y4 = (p["height"] + 3) // 4
        self.uv_max_x4 = (p["width"] // 2 + 3) // 4
        self.uv_max_y4 = (p["height"] // 2 + 3) // 4

        self.ref_wiener = [[[3, -7, 15] for _ in range(2)] for _ in range(3)]
        self.ref_sgr = [[-32, 31] for _ in range(3)]

    # ── read_lr (§5.11.57) ────────────────────────────────────────────────
    def read_lr(self, r, c):
        if self.allow_intrabc or not self.lr_uses:
            return
        w = h = self.sb_mi
        for plane in range(3):
            frt = self.frt[plane]
            if frt == 0:
                continue
            sub_x = 0 if plane == 0 else self.sx
            sub_y = 0 if plane == 0 else self.sy
            unit_size = max(1, self.lr_size[plane])
            step_y = MI_SIZE >> sub_y
            step_x = MI_SIZE >> sub_x

            def cuif(us, fs):
                return max((fs + (us >> 1)) // us, 1)

            def round2(x, nn):
                return x if nn == 0 else (x + (1 << (nn - 1))) >> nn

            unit_rows = cuif(unit_size, round2(self.p["height"], sub_y))
            unit_cols = cuif(unit_size, round2(self.p["width"], sub_x))
            row_start = -(-(r * step_y) // unit_size)
            row_end = min(unit_rows, -(-((r + h) * step_y) // unit_size))
            col_start = -(-(c * step_x) // unit_size)
            col_end = min(unit_cols, -(-((c + w) * step_x) // unit_size))
            for _ur in range(row_start, row_end):
                for _uc in range(col_start, col_end):
                    self.read_lr_unit(plane)

    def read_lr_unit(self, plane):
        frt = self.frt[plane]
        if frt == 1:
            rt = 1 if self.dec.read_symbol(self.mc.use_wiener) == 1 else 0
        elif frt == 2:
            rt = 2 if self.dec.read_symbol(self.mc.use_sgrproj) == 1 else 0
        else:
            rt = self.dec.read_symbol(self.mc.restoration_type)
        if rt == 1:
            for p in range(2):
                first = 1 if plane != 0 else 0
                if plane != 0:
                    self.ref_wiener[plane][p][0] = 0
                for j in range(first, 3):
                    ref = self.ref_wiener[plane][p][j]
                    v = self._subexp_bool_signed(
                        WIENER_TAPS_MIN[j], WIENER_TAPS_MAX[j] + 1,
                        WIENER_TAPS_K[j], ref)
                    self.ref_wiener[plane][p][j] = v
        elif rt == 2:
            lr_set = self.dec.read_literal(SGRPROJ_PARAMS_BITS) & 15
            for i in range(2):
                radius = SGR_PARAMS[lr_set][i * 2]
                mn, mx = SGRPROJ_XQD_MIN[i], SGRPROJ_XQD_MAX[i]
                if radius:
                    v = self._subexp_bool_signed(mn, mx + 1, SGRPROJ_PRJ_SUBEXP_K,
                                                 self.ref_sgr[plane][i])
                elif i == 1:
                    v = clip3(mn, mx, (1 << SGRPROJ_PRJ_BITS) - self.ref_sgr[plane][0])
                else:
                    v = 0
                self.ref_sgr[plane][i] = v

    def _subexp_bool_signed(self, low, high, k, r):
        return self._subexp_bool_unsigned(high - low, k, r - low) + low

    def _subexp_bool_unsigned(self, mx, k, r):
        v = self._subexp_bool(mx, k)
        if (r << 1) <= mx:
            return inverse_recenter(r, v)
        return mx - 1 - inverse_recenter(mx - 1 - r, v)

    def _subexp_bool(self, num_syms, k):
        i = 0
        mk = 0
        while True:
            b2 = k + i - 1 if i != 0 else k
            a = 1 << b2
            if num_syms <= mk + 3 * a:
                return read_ns(self.dec, num_syms - mk) + mk
            if self.dec.read_literal(1) != 0:
                i += 1
                mk += a
            else:
                return self.dec.read_literal(b2) + mk

    # ── partition (§5.11.4) ───────────────────────────────────────────────
    def partition_context(self, mi_row, mi_col, bsize):
        bsl = mi_log2_w(bsize)
        au = mi_row > 0
        al = mi_col > 0
        above = au and self.mi_w_log2_above[min(mi_col, self.mi_cols - 1)] < bsl
        left = al and self.mi_h_log2_left[min(mi_row, self.mi_rows - 1)] < bsl
        return (1 if left else 0) * 2 + (1 if above else 0)

    def _split_or(self, bucket, ctx, bsize, is_horz):
        cdf = self.mc.base_partition(bucket, ctx)
        max_valid = len(cdf) - 2

        def mass(hi):
            if hi > max_valid:
                return 0
            prev = 0 if hi == 0 else cdf[hi - 1]
            return cdf[hi] - prev

        if is_horz:
            psum = (mass(PART_VERT) + mass(PART_SPLIT) + mass(PART_HORZ_A) +
                    mass(PART_VERT_A) + mass(PART_VERT_B))
            if bsize != BLOCK_128X128:
                psum += mass(PART_VERT_4)
        else:
            psum = (mass(PART_HORZ) + mass(PART_SPLIT) + mass(PART_HORZ_A) +
                    mass(PART_HORZ_B) + mass(PART_VERT_A))
            if bsize != BLOCK_128X128:
                psum += mass(PART_HORZ_4)
        synthetic = [32768 - psum, 32768, 0]
        return self.dec.read_symbol(synthetic) == 1

    def decode_partition(self, mi_row, mi_col, bsize):
        if mi_row >= self.mi_rows or mi_col >= self.mi_cols:
            return
        if bsize < BLOCK_8X8:
            self.decode_block(mi_row, mi_col, bsize)
            return
        bw4 = BW[bsize] // MI_SIZE
        bh4 = BH[bsize] // MI_SIZE
        half4 = bw4 >> 1
        has_rows = (mi_row + half4) < self.mi_rows
        has_cols = (mi_col + half4) < self.mi_cols
        ctx = self.partition_context(mi_row, mi_col, bsize)
        bucket = PARTITION_CDF_LOOKUP[bsize]
        if has_rows and has_cols:
            partition = self.mc.read_partition_sym(self.dec, bucket, ctx) \
                if hasattr(self.mc, "read_partition_sym") else \
                self.dec.read_symbol(self.mc.base_partition(bucket, ctx))
        elif has_cols:
            partition = PART_SPLIT if self._split_or(bucket, ctx, bsize, True) else PART_HORZ
        elif has_rows:
            partition = PART_SPLIT if self._split_or(bucket, ctx, bsize, False) else PART_VERT
        else:
            partition = PART_SPLIT
        if os.environ.get("KINETIX_AV1_DBG_PARTALL"):
            print(
                f"DBG partition mi=({mi_col},{mi_row}) bsize={bsize} "
                f"has_rows={has_rows} has_cols={has_cols} ctx={ctx} partition={partition}",
                file=sys.stderr,
            )
        subs = split_into_subblocks(bw4, bh4, partition)
        if partition == PART_SPLIT and bsize > BLOCK_8X8:
            for sub_bs, ro, co in subs:
                self.decode_partition(mi_row + ro, mi_col + co, sub_bs)
        else:
            for sub_bs, ro, co in subs:
                sr, sc = mi_row + ro, mi_col + co
                if sr < self.mi_rows and sc < self.mi_cols:
                    self.decode_block(sr, sc, sub_bs)

    def record_mi_size(self, mi_row, mi_col, bsize):
        bw4 = BW[bsize] // MI_SIZE
        bh4 = BH[bsize] // MI_SIZE
        for c in range(mi_col, min(mi_col + bw4, self.mi_cols)):
            self.mi_w_log2_above[c] = mi_log2_w(bsize)
        for r in range(mi_row, min(mi_row + bh4, self.mi_rows)):
            self.mi_h_log2_left[r] = mi_log2_h(bsize)

    def decode_block(self, mi_row, mi_col, bsize):
        self.record_mi_size(mi_row, mi_col, bsize)
        self.decode_intra_block(mi_row, mi_col, bsize)

    # ── palette ───────────────────────────────────────────────────────────
    def get_palette_cache(self, plane, mi_row, mi_col):
        above_store, left_store = (self.pal_y_above, self.pal_y_left) if plane == 0 \
            else (self.pal_u_above, self.pal_u_left)
        above = above_store[mi_col] if (mi_row * MI_SIZE) % 64 != 0 else []
        left = left_store[mi_row] if mi_col > 0 else []
        cache = []
        ai = li = 0
        while ai < len(above) and li < len(left):
            a, l = above[ai], left[li]
            if l < a:
                if not cache or cache[-1] != l:
                    cache.append(l)
                li += 1
            else:
                if not cache or cache[-1] != a:
                    cache.append(a)
                ai += 1
                if l == a:
                    li += 1
        for v in above[ai:]:
            if not cache or cache[-1] != v:
                cache.append(v)
        for v in left[li:]:
            if not cache or cache[-1] != v:
                cache.append(v)
        return cache

    def read_palette_colors_yu(self, size, cache, is_u):
        colors = []
        idx = 0
        i = 0
        while i < len(cache) and idx < size:
            if self.dec.read_literal(1) == 1:
                colors.append(cache[i])
                idx += 1
            i += 1
        if idx < size:
            colors.append(self.dec.read_literal(BIT_DEPTH))
            idx += 1
        palette_bits = 0
        if idx < size:
            palette_bits = (BIT_DEPTH - 3) + self.dec.read_literal(2)
        while idx < size:
            bias = 0 if is_u else 1
            delta = self.dec.read_literal(palette_bits) + bias
            val = clip1(colors[idx - 1] + delta)
            colors.append(val)
            sub = 0 if is_u else 1
            rng = max(0, (1 << BIT_DEPTH) - val - sub)
            palette_bits = min(palette_bits, ceil_log2(rng))
            idx += 1
        colors.sort()
        return colors

    def read_palette_colors_v(self, size):
        colors = [0] * size
        if self.dec.read_literal(1) == 1:
            max_val = 1 << BIT_DEPTH
            palette_bits = (BIT_DEPTH - 4) + self.dec.read_literal(2)
            colors[0] = self.dec.read_literal(BIT_DEPTH)
            for idx in range(1, size):
                delta = self.dec.read_literal(palette_bits)
                if delta != 0 and self.dec.read_literal(1) == 1:
                    delta = -delta
                val = colors[idx - 1] + delta
                if val < 0:
                    val += max_val
                if val >= max_val:
                    val -= max_val
                colors[idx] = clip1(val)
        else:
            for idx in range(size):
                colors[idx] = self.dec.read_literal(BIT_DEPTH)
        return colors

    def read_palette_mode_info(self, mi_row, mi_col, bsize, y_mode, uv_mode, hchroma):
        colors_y, colors_u, colors_v = [], [], []
        if (bsize < BLOCK_8X8 or BW[bsize] > 64 or BH[bsize] > 64
                or not self.allow_screen):
            return colors_y, colors_u, colors_v
        bsize_ctx = mi_log2_w(bsize) + mi_log2_h(bsize) - 2
        if y_mode == DC_PRED:
            above_has = mi_row > 0 and len(self.pal_y_above[mi_col]) > 0
            left_has = mi_col > 0 and len(self.pal_y_left[mi_row]) > 0
            ctx = (1 if above_has else 0) + (1 if left_has else 0)
            if self.dec.read_symbol(self.mc.palette_y_mode[bsize_ctx][ctx]) == 1:
                size = 2 + self.dec.read_symbol(self.mc.palette_y_size[bsize_ctx])
                cache = self.get_palette_cache(0, mi_row, mi_col)
                colors_y = self.read_palette_colors_yu(size, cache, False)
        if hchroma and uv_mode == DC_PRED:
            ctx = 1 if colors_y else 0
            if self.dec.read_symbol(self.mc.palette_uv_mode[ctx]) == 1:
                size = 2 + self.dec.read_symbol(self.mc.palette_uv_size[bsize_ctx])
                cache = self.get_palette_cache(1, mi_row, mi_col)
                colors_u = self.read_palette_colors_yu(size, cache, True)
                colors_v = self.read_palette_colors_v(size)
        return colors_y, colors_u, colors_v

    def _palette_color_ctx(self, cmap, stride, r, c, n):
        scores = [0] * PALETTE_COLORS
        order = list(range(PALETTE_COLORS))
        if c > 0:
            scores[cmap[r * stride + c - 1]] += 2
        if r > 0 and c > 0:
            scores[cmap[(r - 1) * stride + c - 1]] += 1
        if r > 0:
            scores[cmap[(r - 1) * stride + c]] += 2
        for i in range(PALETTE_NUM_NEIGHBORS):
            max_score = scores[i]
            max_idx = i
            for j in range(i + 1, n):
                if scores[j] > max_score:
                    max_score = scores[j]
                    max_idx = j
            if max_idx != i:
                ms = scores[max_idx]
                mo = order[max_idx]
                k = max_idx
                while k > i:
                    scores[k] = scores[k - 1]
                    order[k] = order[k - 1]
                    k -= 1
                scores[i] = ms
                order[i] = mo
        h = sum(scores[i] * PALETTE_COLOR_HASH_MULTIPLIERS[i]
                for i in range(PALETTE_NUM_NEIGHBORS))
        ctx = PALETTE_COLOR_CONTEXT[h]
        return max(0, ctx), order

    def read_color_map(self, bsize, mi_row, mi_col, n, is_uv):
        if n == 0:
            return
        block_w = BW[bsize]
        block_h = BH[bsize]
        on_w = min(block_w, (self.mi_cols - mi_col) * MI_SIZE)
        on_h = min(block_h, (self.mi_rows - mi_row) * MI_SIZE)
        if is_uv:
            block_w >>= self.sx
            block_h >>= self.sy
            on_w >>= self.sx
            on_h >>= self.sy
            if block_w < 4:
                block_w += 2
                on_w += 2
            if block_h < 4:
                block_h += 2
                on_h += 2
        stride = block_w
        cmap = [0] * (block_w * block_h)
        cmap[0] = read_ns(self.dec, n)
        if on_w > 0 and on_h > 0:
            for i in range(1, on_h + on_w - 1):
                j_hi = min(i, on_w - 1)
                j_lo = max(0, i - on_h + 1)
                for j in range(j_hi, j_lo - 1, -1):
                    r, c = i - j, j
                    ctx, order = self._palette_color_ctx(cmap, stride, r, c, n)
                    if is_uv:
                        sym = self._pal_idx(self.mc, "palette_uv_color_", n, ctx)
                    else:
                        sym = self._pal_idx(self.mc, "palette_y_color_", n, ctx)
                    cmap[r * stride + c] = order[sym]

    def _pal_idx(self, mc, prefix, size, ctx):
        s = size if size <= 8 else 8
        table = getattr(mc, prefix + str(max(2, min(8, s))))
        return self.dec.read_symbol(table[ctx])

    # ── intra_frame_mode_info (§5.11.7) + reconstruct-order coeffs ─────────
    def decode_intra_block(self, mi_row, mi_col, bsize):
        self.mark(f"mode_info mi=({mi_col},{mi_row}) bsize={bsize} "
                  f"px=({mi_col * MI_SIZE},{mi_row * MI_SIZE})")
        bw4 = BW[bsize] // MI_SIZE
        bh4 = BH[bsize] // MI_SIZE

        # skip
        if self.seg:
            raise NotImplementedError("segmentation")
        above_skip = self.skip_above[mi_col]
        left_skip = self.skip_left[mi_row]
        skip = self.dec.read_symbol(self.mc.skip[min(above_skip + left_skip, 2)]) == 1
        # read_cdef / read_delta_qindex / read_delta_lf: no-ops on this corpus

        if self.allow_intrabc:
            raise NotImplementedError("intrabc")

        above_mode = self.ymode_above[mi_col]
        left_mode = self.ymode_left[mi_row]
        y_mode = self.dec.read_symbol(
            self.mc.intra_y_mode[INTRA_MODE_CONTEXT[above_mode]][INTRA_MODE_CONTEXT[left_mode]])

        angle_delta_y = 0
        if bsize >= BLOCK_8X8 and is_directional(y_mode):
            angle_delta_y = self.dec.read_symbol(
                self.mc.angle_delta[y_mode - V_PRED]) - MAX_ANGLE_DELTA

        hchroma = has_chroma(bsize, mi_row, mi_col, self.sx, self.sy)
        uv_mode = DC_PRED
        if hchroma:
            table = self.mc.uv_mode_allowed if cfl_allowed(bsize) else self.mc.uv_mode_not_allowed
            uv_mode = self.dec.read_symbol(table[y_mode])

        if hchroma and uv_mode == UV_CFL_PRED:
            self.read_cfl_alphas()

        if hchroma and bsize >= BLOCK_8X8 and is_directional(uv_mode):
            self.dec.read_symbol(self.mc.angle_delta[uv_mode - V_PRED])

        colors_y, colors_u, colors_v = self.read_palette_mode_info(
            mi_row, mi_col, bsize, y_mode, uv_mode, hchroma)

        filter_intra_mode = None
        if not colors_y:
            if (self.enable_filter_intra and y_mode == DC_PRED
                    and max(BW[bsize], BH[bsize]) <= 32):
                if self.dec.read_symbol(self.mc.filter_intra[bsize]) == 1:
                    filter_intra_mode = self.dec.read_symbol(self.mc.filter_intra_mode)

        self.read_color_map(bsize, mi_row, mi_col, len(colors_y), False)
        self.read_color_map(bsize, mi_row, mi_col, len(colors_u), True)

        max_tx = MAX_TX_SIZE_RECT[bsize]
        luma_tx = max_tx
        # AV1 §5.11.15 read_tx_size: the tx_depth symbol is only read when
        # MiSize > BLOCK_4X4 (bsize 0) -- a 4x4 block always uses TX_4X4 with
        # no signalled depth. Missing this gate reads a spurious tx_depth
        # symbol for every BLOCK_4X4 leaf under TX_MODE_SELECT, desyncing
        # everything after it (Kinetix hit and fixed this same bug -- see
        # intra_block.rs's `bsize > BLOCK_4X4` comment).
        if bsize > 0 and self.tx_mode_select and not self.lossless:
            luma_tx = self.read_tx_size(bsize, max_tx, mi_row, mi_col)

        # reconstruct-order coeffs (mirrors reconstruct_intra_subblock)
        luma_dir = FILTER_INTRA_MODE_TO_INTRA_DIR[filter_intra_mode] \
            if filter_intra_mode is not None else y_mode
        self._coeffs_luma(mi_row, mi_col, bsize, luma_tx, luma_dir, uv_mode,
                          bool(colors_y))
        if hchroma:
            self._coeffs_chroma(mi_row, mi_col, bsize, uv_mode, bool(colors_u))

        # neighbour updates
        for r in range(mi_row, min(mi_row + bh4, self.mi_rows)):
            self.ymode_left[r] = y_mode
            self.uv_left[r] = uv_mode
            self.tx_left[r] = TX_HEIGHT[luma_tx]
            self.skip_left[r] = 1 if skip else 0
            self.pal_y_left[r] = list(colors_y)
            self.pal_u_left[r] = list(colors_u)
        for c in range(mi_col, min(mi_col + bw4, self.mi_cols)):
            self.ymode_above[c] = y_mode
            self.uv_above[c] = uv_mode
            self.tx_above[c] = TX_WIDTH[luma_tx]
            self.skip_above[c] = 1 if skip else 0
            self.pal_y_above[c] = list(colors_y)
            self.pal_u_above[c] = list(colors_u)

    def read_cfl_alphas(self):
        signs = self.dec.read_symbol(self.mc.cfl_sign)
        su = (signs + 1) // 3
        sv = (signs + 1) % 3
        if su != CFL_SIGN_ZERO:
            ctx = (su - 1) * 3 + sv
            self.dec.read_symbol(self.mc.cfl_alpha[ctx])
        if sv != CFL_SIGN_ZERO:
            ctx = (sv - 1) * 3 + su
            self.dec.read_symbol(self.mc.cfl_alpha[ctx])

    def read_tx_size(self, bsize, max_tx, mi_row, mi_col):
        depth = MAX_TX_DEPTH_TABLE[bsize]
        bucket = {4: 3, 3: 2, 2: 1}.get(depth, 0)
        aw = self.tx_above[mi_col]
        lh = self.tx_left[mi_row]
        ctx = (1 if aw >= TX_WIDTH[max_tx] else 0) + (1 if lh >= TX_HEIGHT[max_tx] else 0)
        table = [self.mc.tx_8x8, self.mc.tx_16x16, self.mc.tx_32x32, self.mc.tx_64x64][bucket]
        tx_depth = self.dec.read_symbol(table[ctx])
        if os.environ.get("KINETIX_AV1_DBG_TXSIZE"):
            print(
                f"DBG txsize mi=({mi_col},{mi_row}) bsize={bsize} max_tx={max_tx} "
                f"ctx={ctx} above_w={aw} left_h={lh} tx_depth={tx_depth}",
                file=sys.stderr,
            )
        tx = max_tx
        for _ in range(tx_depth):
            tx = SPLIT_TX_SIZE[tx]
        return tx

    def _blk(self, plane, tx_size, x4, y4, mx4, my4, bw_s, bh_s, intra_dir, uv_mode):
        return {"plane": plane, "tx_size": tx_size, "x4": x4, "y4": y4,
                "max_x4": mx4, "max_y4": my4, "block_w": bw_s, "block_h": bh_s,
                "intra_dir": intra_dir, "uv_mode": uv_mode,
                "qindex_positive": not self.lossless,
                "reduced_tx_set": self.reduced_tx_set, "lossless": self.lossless}

    def _coeffs_luma(self, mi_row, mi_col, bsize, luma_tx, intra_dir, uv_mode, has_pal):
        bw4 = BW[bsize] // MI_SIZE
        bh4 = BH[bsize] // MI_SIZE
        tw, th = TX_WIDTH[luma_tx], TX_HEIGHT[luma_tx]
        for ty in range(0, bh4 * MI_SIZE, th):
            for tx in range(0, bw4 * MI_SIZE, tw):
                px = mi_col * MI_SIZE + tx
                py = mi_row * MI_SIZE + ty
                blk = self._blk(0, luma_tx, px // 4, py // 4, self.luma_max_x4,
                                self.luma_max_y4, bw4 * MI_SIZE, bh4 * MI_SIZE,
                                intra_dir, uv_mode)
                read_coeffs(self.dec, self.cc, self.ctxs, blk)

    def _coeffs_chroma(self, mi_row, mi_col, bsize, uv_mode, has_pal):
        c_tx = chroma_tx_size(bsize, self.sx, self.sy)
        cw, ch = TX_WIDTH[c_tx], TX_HEIGHT[c_tx]
        ps = get_plane_residual_size(bsize, self.sx, self.sy)
        if ps == BLOCK_INVALID:
            ps = bsize
        cbw, cbh = BW[ps], BH[ps]
        base_x = (mi_col >> self.sx) * MI_SIZE
        base_y = (mi_row >> self.sy) * MI_SIZE
        for ty in range(0, cbh, ch):
            for tx in range(0, cbw, cw):
                cx, cy = base_x + tx, base_y + ty
                if cx >= self.p["width"] // 2 or cy >= self.p["height"] // 2:
                    continue
                for plane in (1, 2):
                    blk = self._blk(plane, c_tx, cx // 4, cy // 4, self.uv_max_x4,
                                    self.uv_max_y4, cbw, cbh, uv_mode, uv_mode)
                    read_coeffs(self.dec, self.cc, self.ctxs, blk)

    # ── driver ────────────────────────────────────────────────────────────
    def mark(self, label):
        self.markers.append((len(self.dec.trace), label))

    def run(self):
        self.markers = []
        for mi_row in range(self.p["sb_row_start"] * self.sb_mi,
                            self.p["sb_row_end"] * self.sb_mi, self.sb_mi):
            self.ctxs.clear_left()
            for mi_col in range(self.p["sb_col_start"] * self.sb_mi,
                                self.p["sb_col_end"] * self.sb_mi, self.sb_mi):
                self.read_deltas = self.p.get("delta_q_present", False)
                self.read_lr(mi_row, mi_col)
                self.decode_partition(mi_row, mi_col, self.sb_bsize)


def main():
    spec = json.load(open(sys.argv[1] if len(sys.argv) > 1 else "av1_tile_trace.json"))
    t = Tile(spec)
    print(f"params: frt={t.frt} lr_size={t.lr_size} uses_lr={t.lr_uses}")
    try:
        t.run()
    except Exception as e:
        print(f"oracle stopped: {type(e).__name__}: {e}")

    ktrace = spec["trace"]
    otrace = t.dec.trace
    kmarkers = {m["seq"]: m["label"] for m in spec["markers"]}
    omarkers = dict(t.markers)

    print(f"oracle produced {len(otrace)} symbols; Kinetix trace has {len(ktrace)}")
    if "--markers" in sys.argv:
        for s, lab in t.markers[:12]:
            kseq = next((k for k in sorted(kmarkers) if kmarkers[k] == lab), "?")
            print(f"  oracle marker seq={s:5}  kinetix seq={kseq:>5}  {lab}")
    if "--dump" in sys.argv:
        a = int(sys.argv[sys.argv.index("--dump") + 1])
        b = a + 24
        for i in range(a, min(b, min(len(otrace), len(ktrace)))):
            o = otrace[i]
            k = ktrace[i]
            flag = "" if (o["n"], o["value"], o["after"]) == (k[0], k[1], k[3]) else "  <<<"
            print(f"  #{i:5} oracle n={o['n']} v={o['value']} [{o['before']},{o['after']})   "
                  f"kinetix n={k[0]} v={k[1]} [{k[2]},{k[3]}){flag}")
        return
    n = min(len(otrace), len(ktrace))
    have_rng = len(ktrace[0]) >= 6
    for i in range(n):
        o = otrace[i]
        k = ktrace[i]
        mismatch = o["n"] != k[0] or o["value"] != k[1] or o["after"] != k[3]
        if have_rng and (o["rng"] != k[4] or o["val"] != k[5]):
            mismatch = True
        if mismatch:
            # nearest preceding markers
            om = max((s for s in omarkers if s <= i), default=None)
            km = max((s for s in kmarkers if s <= i), default=None)
            print(f"\nFIRST DIVERGENCE at symbol #{i}:")
            print(f"  oracle : n={o['n']} value={o['value']} bits=[{o['before']},{o['after']}) "
                  f"rng={o['rng']} val={o['val']}")
            if have_rng:
                print(f"  kinetix: n={k[0]} value={k[1]} bits=[{k[2]},{k[3]}) rng={k[4]} val={k[5]}")
            else:
                print(f"  kinetix: n={k[0]} value={k[1]} bits=[{k[2]},{k[3]})")
            if om is not None:
                print(f"  oracle  nearest marker  [{om}] {omarkers[om]}")
            if km is not None:
                print(f"  kinetix nearest marker  [{km}] {kmarkers[km]}")
            # context: previous 3 markers on each side
            print("  last oracle markers:", [(s, omarkers[s]) for s in sorted(omarkers)[-6:] if s <= i][-3:])
            return
    if len(otrace) == len(ktrace):
        print("\nTRACE MATCHES KINETIX EXACTLY across the whole tile.")
        print("=> the entropy path is correct; the divergence is in reconstruction.")
    else:
        print(f"\nprefix of {n} symbols matches; lengths differ "
              f"(oracle {len(otrace)} vs kinetix {len(ktrace)}).")


if __name__ == "__main__":
    main()
