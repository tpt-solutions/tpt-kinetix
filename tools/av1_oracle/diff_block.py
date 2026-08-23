"""Decode AV1 coefficient blocks with the independent oracle and emit a
per-symbol trace that can be diffed against Kinetix's Rust `SymbolDecoder`
trace (produced by `just av1-trace-diff` / `enable_symbol_trace`).

This is the differential tool that makes `coeffs.py` hunt the real AV1
decode desync: feed it the capture written by `KINETIX_AV1_CAPTURE` (see
`maybe_capture_block` in entropy.rs / `just av1-capture`) — the raw tile
bytes starting at the exact bit offset where a target block's `coeffs()`
begins, plus that block's `TxBlockCtx`, its neighbour level/dc context, and
Kinetix's own symbol slice — and compare the oracle's independent re-decode
against Kinetix's symbols. A mismatch at a given (value, bit_pos) is a
control-flow / context / CDF-table bug in whichever side differs.

Capture format (from `maybe_capture_block`):
{
  "data_hex": "<raw tile bytes from the block's coeffs() bit offset onward>",
  "bit_offset": 0,
  "base_q_idx": 128,
  "reference_values": [v0, v1, ...],   # Kinetix's own symbols for this block
  "ctx": { "above_level": [...], "above_dc": [...], "left_level": [...], "left_dc": [...] },
  "blk": { plane, tx_size, x4, y4, max_x4, max_y4, block_w, block_h,
           intra_dir, uv_mode, qindex_positive, reduced_tx_set, lossless }
}

LIMITATION: the oracle re-seeds neighbour level/dc context from `ctx` but uses
fresh (base_q-seeded) *CDF tables*; it does not replay mid-tile CDF adaptation.
So a divergence that is purely an artifact of Kinetix's adapted CDF state at
this block is not yet separated out — but any divergence caused by a wrong
context derivation or a numerically-wrong default CDF *table value* still shows
up here, which is the dominant open hypothesis for the corpus's non-pixel-
exactness. Capturing the adapted CDF set is a future extension.
"""

import json
import sys

from symbol_decoder import SymbolDecoder
from coeffs import TileCdfs, CoeffContexts, read_coeffs

# TxSize enum (kept local so a block spec can echo the validate.py ordering).
(TX_4X4, TX_8X8, TX_16X16, TX_32X32, TX_64X64, TX_4X8, TX_8X4, TX_8X16,
 TX_16X8, TX_16X32, TX_32X16, TX_4X16, TX_16X4, TX_8X32, TX_32X8,
 TX_16X64, TX_64X16, TX_32X64, TX_64X32) = range(19)


def _seed_ctx(ctxs, plane, cap_ctx):
    """Copy Kinetix's captured neighbour context into the oracle's
    CoeffContexts for `plane`."""
    def _assign(store_name, values):
        store = getattr(ctxs, store_name)
        for i, v in enumerate(values):
            if 0 <= i < len(store[plane]):
                store[plane][i] = v
    _assign("above_level", cap_ctx["above_level"])
    _assign("above_dc", cap_ctx["above_dc"])
    _assign("left_level", cap_ctx["left_level"])
    _assign("left_dc", cap_ctx["left_dc"])


def main():
    if len(sys.argv) != 2:
        print(__doc__)
        sys.exit(1)
    spec = json.load(open(sys.argv[1]))
    # Accept either a capture file (block fields at top level) or a multi-block
    # spec (list under "blocks").
    if "blocks" in spec:
        blocks = spec["blocks"]
        data_hex = spec["data_hex"]
        bit_offset = int(spec.get("bit_offset", 0))
        base_q = int(spec["base_q_idx"])
        max_x4 = spec.get("max_x4", 16)
        max_y4 = spec.get("max_y4", 16)
    else:
        blk = spec["blk"]
        blocks = [blk]
        data_hex = spec["data_hex"]
        bit_offset = int(spec.get("bit_offset", 0))
        base_q = int(spec["base_q_idx"])
        max_x4 = blk.get("max_x4", 16)
        max_y4 = blk.get("max_y4", 16)

    data = bytes.fromhex(data_hex)
    if "symbol_range" in spec:
        # Resume from Kinetix's exact arithmetic-coder state rather than
        # re-deriving symbol_range/symbol_value from raw bytes — see
        # SymbolDecoder.from_raw_state's docstring for why a fresh
        # init_symbol()-style reinit is wrong mid-tile.
        dec = SymbolDecoder.from_raw_state(
            data, bit_offset, spec["symbol_range"], spec["symbol_value"],
            spec["symbol_max_bits"],
        )
    else:
        dec = SymbolDecoder(data, bit_offset)
    cdfs = TileCdfs(base_q)
    ctxs = CoeffContexts(max(max_x4, 1), max(max_y4, 1))
    # Re-seed the neighbour level/dc context from Kinetix's exact state at
    # this block, so the independent re-decode starts from the same
    # above_level/left_level as the real decoder (otherwise every context
    # derived from the neighbour levels diverges immediately and the diff is
    # meaningless).
    cap_ctx = spec.get("ctx")
    if cap_ctx is not None:
        plane = blocks[0]["plane"]
        _seed_ctx(ctxs, plane, cap_ctx)
    # Re-seed the adapted CDF tables from Kinetix's exact mid-tile state so
    # the independent re-decode starts from the same probabilities (removes
    # the CDF-adaptation confound entirely — a remaining divergence is then a
    # genuine context-derivation or table-value bug, not an adaptation artifact).
    cap_cdfs = spec.get("cdfs")
    if cap_cdfs is not None:
        cdfs.load_snapshot(cap_cdfs)
    ref = spec.get("reference_values")

    for i, blk in enumerate(blocks):
        dec.trace = []
        cb = read_coeffs(dec, cdfs, ctxs, blk)
        print(f"--- block {i} plane={blk['plane']} tx={blk['tx_size']} "
              f"mi=({blk['x4']},{blk['y4']}) eob={cb['eob']} tx_type={cb['tx_type']}")
        nz = [(p, v) for p, v in enumerate(cb["quant"]) if v != 0]
        print(f"    nonzero ({len(nz)}): {nz}")
        if ref is not None:
            vals = [t["value"] for t in dec.trace]
            for j, (v, rv) in enumerate(zip(vals, ref)):
                if v != rv:
                    print(f"    DIVERGENCE at symbol {j}: oracle={v} reference={rv} "
                          f"(bit {dec.trace[j]['before']})")
                    break
            else:
                if len(vals) == len(ref):
                    print("    TRACE MATCHES REFERENCE")
                else:
                    print(f"    trace length {len(vals)} != reference {len(ref)}")
        else:
            for t in dec.trace:
                print(f"    n={t['n']} value={t['value']} before={t['before']} after={t['after']}")


if __name__ == "__main__":
    main()
