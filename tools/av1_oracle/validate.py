"""Validate the independent Python AV1 oracle against the Rust crate's own
golden vectors (tpt-kinetix-av1/src/coeff.rs and src/entropy.rs unit tests).

Run: python3 tools/av1_oracle/validate.py
"""

from symbol_decoder import SymbolDecoder
from coeffs import TileCdfs, CoeffContexts, read_coeffs

# AV1 TxSize enum order (matches spec / coeff_tables.rs).
TX_4X4, TX_8X8, TX_16X16, TX_32X32, TX_64X64 = 0, 1, 2, 3, 4
TX_4X8, TX_8X4, TX_8X16, TX_16X8, TX_16X32, TX_32X16 = 5, 6, 7, 8, 9, 10
TX_4X16, TX_16X4, TX_8X32, TX_32X8, TX_16X64, TX_64X16 = 11, 12, 13, 14, 15, 16

# Intra modes (av1 spec).
DC_PRED, V_PRED, H_PRED, PAETH_PRED = 0, 1, 2, 12


def ramp(length, mul, add):
    return bytes(((i * mul + add) & 0xFF) for i in range(length))


def blk(plane, tx_size, x4, y4, bw, bh, intra_dir=0, uv_mode=0,
        qindex_positive=True, reduced=False, lossless=False):
    return {
        "plane": plane, "tx_size": tx_size, "x4": x4, "y4": y4,
        "max_x4": 16, "max_y4": 16, "block_w": bw, "block_h": bh,
        "intra_dir": intra_dir, "uv_mode": uv_mode,
        "qindex_positive": qindex_positive, "reduced_tx_set": reduced,
        "lossless": lossless,
    }


def decode_all(data, base_q, blocks):
    dec = SymbolDecoder(data)
    cdfs = TileCdfs(base_q)
    ctxs = CoeffContexts(16, 16)
    return [read_coeffs(dec, cdfs, ctxs, b) for b in blocks]


def assert_matches(got, expected):
    assert len(got) == len(expected), f"block count {len(got)} != {len(expected)}"
    for i, (g, (eob, tx_type, nonzero)) in enumerate(zip(got, expected)):
        assert g["eob"] == eob, f"block {i} eob {g['eob']} != {eob}"
        assert g["tx_type"] == tx_type, f"block {i} tx_type {g['tx_type']} != {tx_type}"
        nz = [(p, v) for p, v in enumerate(g["quant"]) if v != 0]
        assert nz == nonzero, f"block {i} coeffs {nz} != {nonzero}"


# ── Scenario A (mirrors coeff.rs::coeffs_match_independent_oracle_lossy) ──
EXPECTED_A = [
    (41, 9, [(0, 15), (1, -1), (2, 11), (3, 8), (5, -1), (7, 1), (8, -9), (9, 3),
             (10, -5), (11, 3), (17, -1), (18, 1), (19, -3), (21, 1), (26, -1),
             (27, 1), (28, 1), (29, 1), (32, 1), (48, 1)]),
    (0, 0, []),
    (0, 0, []),
    (45, 1, [(0, 5), (8, -2), (10, -1), (11, -1), (13, -1), (16, 2), (17, 1),
             (19, -1), (20, -1), (23, -1), (24, 2), (25, -1), (29, 1), (30, -1),
             (33, -1), (34, -1), (40, -1), (42, 1)]),
    (0, 0, []),
    (0, 0, []),
    (5, 2, [(8, 1), (9, 1)]),
    (0, 0, []),
    (105, 1, [(0, 2), (2, -1), (5, 1), (13, 1), (16, 1), (17, 2), (18, 1), (20, -1),
              (22, 1), (24, 1), (25, 1), (28, 1), (32, 1), (37, -1), (43, 1),
              (48, -1), (51, -1), (68, 1), (87, -1), (115, -1), (116, 1), (129, -1),
              (178, -1), (208, 1)]),
    (5, 2, [(0, -2), (1, 1), (5, -1), (8, -2)]),
]

# ── Scenario B (mirrors coeff.rs::coeffs_match_independent_oracle_reduced_and_lossless) ──
EXPECTED_B = [
    (1, 9, [(0, -1)]),
    (0, 0, []),
    (0, 0, []),
    (1, 0, [(0, -1)]),
    (0, 0, []),
    (4, 1, [(0, -1), (32, -1)]),
    (0, 0, []),
    (0, 0, []),
]


def test_symbol_decoder():
    # multi_symbol_uniform4
    cdf = [8192, 16384, 24576, 32768, 0]
    dec = SymbolDecoder(bytes([0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC]))
    exp_syms = [0, 1, 0, 0, 1, 3]
    exp_cdf = [
        [8960, 16896, 24832, 32768, 1],
        [8680, 17392, 25080, 32768, 2],
        [9432, 17872, 25320, 32768, 3],
        [10161, 18337, 25552, 32768, 4],
        [9844, 18787, 25777, 32768, 5],
        [9537, 18200, 24972, 32768, 6],
    ]
    for i in range(6):
        sym = dec.read_symbol(cdf)
        assert sym == exp_syms[i], f"sym {i}: {sym} != {exp_syms[i]}"
        assert cdf == exp_cdf[i], f"cdf {i}: {cdf} != {exp_cdf[i]}"

    # txb_skip_context0
    cdf = [31671, 32768, 0]
    dec = SymbolDecoder(bytes([0x7E, 0x91, 0x2D, 0x44, 0xC3, 0x0F, 0xAA, 0x55]))
    exp_cdf = [
        [31739, 32768, 1], [31803, 32768, 2], [31863, 32768, 3], [31919, 32768, 4],
        [31972, 32768, 5], [32021, 32768, 6], [32067, 32768, 7], [32110, 32768, 8],
        [32151, 32768, 9], [32189, 32768, 10],
    ]
    for i in range(10):
        sym = dec.read_symbol(cdf)
        assert sym == 0, f"sym {i}: {sym}"
        assert cdf == exp_cdf[i], f"cdf {i}: {cdf} != {exp_cdf[i]}"

    # read_literal_matches_reference_trace
    dec = SymbolDecoder(bytes([0xA5, 0x3C, 0x81, 0xF0]))
    lits = [dec.read_literal(4) for _ in range(4)]
    assert lits == [10, 5, 4, 2], f"lits {lits}"


def test_scenario_a():
    data = ramp(96, 37, 11)
    blocks = [
        blk(0, TX_8X8, 0, 0, 8, 8),
        blk(1, TX_4X4, 0, 0, 4, 4),
        blk(2, TX_4X4, 0, 0, 4, 4),
        blk(0, TX_8X8, 2, 0, 8, 8),
        blk(1, TX_4X4, 1, 0, 4, 4),
        blk(2, TX_4X4, 1, 0, 4, 4),
        blk(0, TX_8X8, 0, 2, 8, 8),
        blk(0, TX_4X4, 4, 4, 8, 8, intra_dir=V_PRED),
        blk(0, TX_16X16, 8, 8, 16, 16, intra_dir=PAETH_PRED),
        blk(1, TX_4X4, 4, 4, 8, 8, uv_mode=H_PRED),
    ]
    assert_matches(decode_all(data, 100, blocks), EXPECTED_A)


def test_scenario_b():
    data = ramp(80, 197, 3)
    lossless = blk(0, TX_4X4, 3, 0, 4, 4, lossless=True, qindex_positive=False)
    no_qindex = blk(0, TX_8X8, 1, 0, 8, 8, qindex_positive=False)
    reduced_4x4 = blk(0, TX_4X4, 0, 0, 4, 4, reduced=True)
    reduced_16x16 = blk(0, TX_16X16, 4, 0, 16, 16, intra_dir=H_PRED, reduced=True)
    blocks = [
        reduced_4x4,
        blk(1, TX_4X4, 0, 0, 4, 4, uv_mode=PAETH_PRED),
        blk(2, TX_4X4, 0, 0, 4, 4, uv_mode=V_PRED),
        no_qindex,
        lossless,
        reduced_16x16,
        blk(0, TX_8X8, 0, 2, 16, 16, intra_dir=V_PRED),
        blk(1, TX_4X4, 2, 2, 4, 4, uv_mode=H_PRED),
    ]
    assert_matches(decode_all(data, 200, blocks), EXPECTED_B)


if __name__ == "__main__":
    test_symbol_decoder()
    test_scenario_a()
    test_scenario_b()
    print("ALL ORACLE VALIDATION TESTS PASSED")
