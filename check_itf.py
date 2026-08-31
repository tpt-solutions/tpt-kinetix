import sys

# Exact AV1 integer inverse transform, ported from
# tpt-kinetix-av1/src/reconstruct/transform.rs

COS128_LOOKUP = [
    4096, 4095, 4091, 4085, 4076, 4065, 4052, 4036, 4017, 3996, 3973, 3948, 3920, 3889, 3857, 3822,
    3784, 3745, 3703, 3659, 3612, 3564, 3513, 3461, 3406, 3349, 3290, 3229, 3166, 3102, 3035, 2967,
    2896, 2824, 2751, 2675, 2598, 2520, 2440, 2359, 2276, 2191, 2106, 2019, 1931, 1842, 1751, 1660,
    1567, 1474, 1380, 1285, 1189, 1092, 995, 897, 799, 700, 601, 501, 401, 301, 201, 101, 0,
]

SINPI_1_9 = 1321
SINPI_2_9 = 2482
SINPI_3_9 = 3344
SINPI_4_9 = 3803

def cos128(angle):
    angle2 = angle % 256
    if angle2 <= 64:
        v = COS128_LOOKUP[angle2]
    elif angle2 <= 128:
        v = -COS128_LOOKUP[128 - angle2]
    elif angle2 <= 192:
        v = -COS128_LOOKUP[angle2 - 128]
    else:
        v = COS128_LOOKUP[256 - angle2]
    return v

def sin128(angle):
    return cos128(angle - 64)

def brev(num_bits, x):
    t = 0
    for i in range(num_bits):
        bit = (x >> i) & 1
        t += bit << (num_bits - 1 - i)
    return t

def round2(x, n):
    if n == 0:
        return x
    return (x + (1 << (n - 1))) >> n

def butterfly(t, a, b, angle, flip):
    ta = t[a]
    tb = t[b]
    x = ta * cos128(angle) - tb * sin128(angle)
    y = ta * sin128(angle) + tb * cos128(angle)
    t[a] = round2(x, 12)
    t[b] = round2(y, 12)
    if flip:
        t[a], t[b] = t[b], t[a]

def hadamard(t, a, b, flip, r):
    if flip:
        a, b = b, a
    x = t[a]
    y = t[b]
    lo = -(1 << (r - 1))
    hi = (1 << (r - 1)) - 1
    t[a] = (x + y)
    t[b] = (x - y)
    t[a] = max(lo, min(hi, t[a]))
    t[b] = max(lo, min(hi, t[b]))

def inverse_dct_permute(t, n):
    copy = t[:]
    for i in range(1 << n):
        t[i] = copy[brev(n, i)]

def inverse_dct(t, n, r):
    inverse_dct_permute(t, n)
    if n == 6:
        for i in range(16):
            butterfly(t, 32 + i, 63 - i, 63 - 4 * brev(4, i), False)
    if n >= 5:
        for i in range(8):
            butterfly(t, 16 + i, 31 - i, 6 + ((brev(3, 7 - i)) << 3), False)
    if n == 6:
        for i in range(16):
            hadamard(t, 32 + i * 2, 33 + i * 2, (i & 1) != 0, r)
    if n >= 4:
        for i in range(4):
            butterfly(t, 8 + i, 15 - i, 12 + ((brev(2, 3 - i)) << 4), False)
    if n >= 5:
        for i in range(8):
            hadamard(t, 16 + 2 * i, 17 + 2 * i, (i & 1) != 0, r)
    if n == 6:
        for i in range(4):
            for j in range(2):
                butterfly(t, 62 - i * 4 - j, 33 + i * 4 + j,
                          60 - 16 * brev(2, i) + 64 * j, True)
    if n >= 3:
        for i in range(2):
            butterfly(t, 4 + i, 7 - i, 56 - 32 * i, False)
    if n >= 4:
        for i in range(4):
            hadamard(t, 8 + 2 * i, 9 + 2 * i, (i & 1) != 0, r)
    if n >= 5:
        for i in range(2):
            for j in range(2):
                butterfly(t, 30 - 4 * i - j, 17 + 4 * i + j,
                          24 + (j << 6) + (((1 - i) << 5)), True)
    if n == 6:
        for i in range(8):
            for j in range(2):
                hadamard(t, 32 + i * 4 + j, 35 + i * 4 - j, (i & 1) != 0, r)
    for i in range(2):
        butterfly(t, 2 * i, 2 * i + 1, 32 + 16 * i, i == 0)
    if n >= 3:
        for i in range(2):
            hadamard(t, 4 + 2 * i, 5 + 2 * i, i != 0, r)
    if n >= 4:
        for i in range(2):
            butterfly(t, 14 - i, 9 + i, 48 + 64 * i, True)
    if n >= 5:
        for i in range(4):
            for j in range(2):
                hadamard(t, 16 + 4 * i + j, 19 + 4 * i - j, (i & 1) != 0, r)
    if n == 6:
        for i in range(2):
            for j in range(4):
                butterfly(t, 61 - i * 8 - j, 34 + i * 8 + j,
                          56 - i * 32 + (j >> 1) * 64, True)
    for i in range(2):
        hadamard(t, i, 3 - i, False, r)
    if n >= 3:
        butterfly(t, 6, 5, 32, True)
    if n >= 4:
        for i in range(2):
            for j in range(2):
                hadamard(t, 8 + 4 * i + j, 11 + 4 * i - j, i != 0, r)
    if n >= 5:
        for i in range(4):
            butterfly(t, 29 - i, 18 + i, 48 + (i >> 1) * 64, True)
    if n == 6:
        for i in range(4):
            for j in range(4):
                hadamard(t, 32 + 8 * i + j, 39 + 8 * i - j, (i & 1) != 0, r)
    if n >= 3:
        for i in range(4):
            hadamard(t, i, 7 - i, False, r)
    if n >= 4:
        for i in range(2):
            butterfly(t, 13 - i, 10 + i, 32, True)
    if n >= 5:
        for i in range(2):
            for j in range(4):
                hadamard(t, 16 + i * 8 + j, 23 + i * 8 - j, i != 0, r)
    if n == 6:
        for i in range(8):
            butterfly(t, 59 - i, 36 + i, 48 if i < 4 else 112, True)
    if n >= 4:
        for i in range(8):
            hadamard(t, i, 15 - i, False, r)
    if n >= 5:
        for i in range(4):
            butterfly(t, 27 - i, 20 + i, 32, True)
    if n == 6:
        for i in range(8):
            hadamard(t, 32 + i, 47 - i, False, r)
            hadamard(t, 48 + i, 63 - i, True, r)
    if n >= 5:
        for i in range(16):
            hadamard(t, i, 31 - i, False, r)
    if n == 6:
        for i in range(8):
            butterfly(t, 55 - i, 40 + i, 32, True)
    if n == 6:
        for i in range(32):
            hadamard(t, i, 63 - i, False, r)

def adst_input_permute(t, n):
    n0 = 1 << n
    copy = t[:n0]
    for i in range(n0):
        idx = i - 1 if i & 1 else n0 - i - 1
        t[i] = copy[idx]

def adst_output_permute(t, n):
    n0 = 1 << n
    copy = t[:n0]
    for i in range(n0):
        a = (i >> 3) & 1
        b = ((i >> 2) & 1) ^ ((i >> 3) & 1)
        c = ((i >> 1) & 1) ^ ((i >> 2) & 1)
        d = (i & 1) ^ ((i >> 1) & 1)
        idx = ((d << 3) | (c << 2) | (b << 1) | a) >> (4 - n)
        t[i] = -copy[idx] if i & 1 else copy[idx]

def inverse_adst4(t):
    s = [0] * 7
    s[0] = SINPI_1_9 * t[0]
    s[1] = SINPI_2_9 * t[0]
    s[2] = SINPI_3_9 * t[1]
    s[3] = SINPI_4_9 * t[2]
    s[4] = SINPI_1_9 * t[2]
    s[5] = SINPI_2_9 * t[3]
    s[6] = SINPI_4_9 * t[3]
    a7 = t[0] - t[2]
    b7 = a7 + t[3]

    s[0] += s[3]
    s[1] -= s[4]
    s[3] = s[2]
    s[2] = SINPI_3_9 * b7

    s[0] += s[5]
    s[1] -= s[6]

    x0 = s[0] + s[3]
    x1 = s[1] + s[3]
    x2 = s[2]
    x3 = s[0] + s[1] - s[3]

    t[0] = round2(x0, 12)
    t[1] = round2(x1, 12)
    t[2] = round2(x2, 12)
    t[3] = round2(x3, 12)

def inverse_adst8(t, r):
    adst_input_permute(t, 3)
    for i in range(4):
        butterfly(t, 2 * i, 2 * i + 1, 60 - 16 * i, True)
    for i in range(4):
        hadamard(t, i, 4 + i, False, r)
    for i in range(2):
        butterfly(t, 4 + 3 * i, 5 + i, 48 - 32 * i, True)
    for j in range(2):
        for i in range(2):
            hadamard(t, 4 * j + i, 2 + 4 * j + i, False, r)
    for i in range(2):
        butterfly(t, 2 + 4 * i, 3 + 4 * i, 32, True)
    adst_output_permute(t, 3)

def inverse_adst16(t, r):
    adst_input_permute(t, 4)
    for i in range(8):
        butterfly(t, 2 * i, 2 * i + 1, 62 - 8 * i, True)
    for i in range(8):
        hadamard(t, i, 8 + i, False, r)
    for i in range(2):
        butterfly(t, 8 + 2 * i, 9 + 2 * i, 56 - 32 * i, True)
        butterfly(t, 13 + 2 * i, 12 + 2 * i, 8 + 32 * i, True)
    for j in range(2):
        for i in range(4):
            hadamard(t, 8 * j + i, 4 + 8 * j + i, False, r)
    for j in range(2):
        for i in range(2):
            butterfly(t, 4 + 8 * j + 3 * i, 5 + 8 * j + i, 48 - 32 * i, True)
    for j in range(4):
        for i in range(2):
            hadamard(t, 4 * j + i, 2 + 4 * j + i, False, r)
    for i in range(4):
        butterfly(t, 2 + 4 * i, 3 + 4 * i, 32, True)
    adst_output_permute(t, 4)

def inverse_adst(t, n, r):
    if n == 2:
        inverse_adst4(t)
    elif n == 3:
        inverse_adst8(t, r)
    else:
        inverse_adst16(t, r)

def inverse_identity(t, n):
    if n == 2:
        for i in range(4):
            t[i] = round2(t[i] * 5793, 12)
    elif n == 3:
        for i in range(8):
            t[i] *= 2
    elif n == 4:
        for i in range(16):
            t[i] = round2(t[i] * 11586, 12)
    else:
        for i in range(32):
            t[i] *= 4

def inverse_transform(dequant, tx_type, tx_size, lossless=False):
    if lossless and tx_size == 0:  # TX_4X4
        return dequant[:16]

    log2w = TX_WIDTH_LOG2[tx_size]
    log2h = TX_HEIGHT_LOG2[tx_size]
    w = 1 << log2w
    h = 1 << log2h

    if tx_type == 0:  # DCT_DCT
        row_kind = 0  # Dct
        col_kind = 0  # Dct
    elif tx_type == 1:  # ADST_DCT
        row_kind = 1  # Adst
        col_kind = 0  # Dct
    elif tx_type == 2:  # V_DCT
        row_kind = 0  # Dct
        col_kind = 1  # Adst
    elif tx_type == 3:  # H_DCT
        row_kind = 0  # Dct
        col_kind = 0  # Dct  (H_DCT = DCT on both axes)
    elif tx_type == 4:  # DCT_ADST
        row_kind = 0  # Dct
        col_kind = 1  # Adst
    elif tx_type == 5:  # ADST_ADST
        row_kind = 1  # Adst
        col_kind = 1  # Adst
    elif tx_type == 10:  # IDTX
        row_kind = 2  # Identity
        col_kind = 2  # Identity
    else:
        raise ValueError(f"Unsupported tx_type: {tx_type}")

    row_shift = TRANSFORM_ROW_SHIFT[tx_size]
    col_shift = 4
    row_clamp_range = 16
    col_clamp_range = 16

    adj = ADJUSTED_TX_SIZE[tx_size]
    adj_w = TX_WIDTH[adj]
    adj_h = TX_HEIGHT[adj]

    residual = [0] * (w * h)
    t = [0] * max(w, h)

    needs_rescale = abs(log2w - log2h) == 1

    for i in range(h):
        for j in range(w):
            t[j] = dequant[i * adj_w + j] if (i < adj_h and j < adj_w) else 0
        if needs_rescale:
            for j in range(w):
                t[j] = round2(t[j] * 2896, 12)
        if row_kind == 0:
            inverse_dct(t, log2w, row_clamp_range)
        elif row_kind == 1:
            inverse_adst(t, log2w, row_clamp_range)
        else:
            inverse_identity(t, log2w)
        for j in range(w):
            residual[i * w + j] = round2(t[j], row_shift)

    lo = -(1 << (col_clamp_range - 1))
    hi = (1 << (col_clamp_range - 1)) - 1
    for i in range(w * h):
        residual[i] = max(lo, min(hi, residual[i]))

    for j in range(w):
        for i in range(h):
            t[i] = residual[i * w + j]
        if col_kind == 0:
            inverse_dct(t, log2h, col_clamp_range)
        elif col_kind == 1:
            inverse_adst(t, log2h, col_clamp_range)
        else:
            inverse_identity(t, log2h)
        for i in range(h):
            residual[i * w + j] = round2(t[i], col_shift)

    return [residual[i] for i in range(w * h)]

# AV1 spec constants (from coeff_tables.rs)
TX_WIDTH_LOG2 = [2, 2, 3, 3, 4, 4, 4, 5, 5, 5, 5, 5, 5, 5, 5, 5,
                 3, 4, 5, 5, 4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
                 3, 4, 5, 5, 4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
                 3, 4, 5, 5, 4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5]
TX_HEIGHT_LOG2 = [2, 3, 2, 3, 2, 3, 4, 2, 3, 4, 5, 5, 5, 5, 5, 5,
                  2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
                  3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
                  4, 4, 4, 4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5]
TRANSFORM_ROW_SHIFT = [0, 1, 2, 2, 2, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 2,
                       2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
                       2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
                       2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2]
TX_WIDTH = [4, 8, 16, 32, 64, 4, 8, 16, 32, 64, 4, 8, 16, 32, 64, 4,
            8, 16, 32, 64, 8, 16, 32, 64, 8, 16, 32, 64, 8, 16, 32, 64,
            16, 32, 64, 16, 32, 64, 16, 32, 64, 16, 32, 64, 16, 32, 64, 16,
            32, 64, 32, 64, 32, 64, 32, 64, 32, 64, 32, 64, 32, 64, 32, 64]
TX_HEIGHT = [4, 4, 4, 4, 4, 8, 8, 8, 8, 8, 16, 16, 16, 16, 16, 16,
             4, 4, 4, 4, 8, 8, 8, 8, 16, 16, 16, 16, 32, 32, 32, 32,
             8, 8, 8, 8, 16, 16, 16, 16, 32, 32, 32, 32, 64, 64, 64, 64,
             16, 16, 16, 16, 32, 32, 32, 32, 64, 64, 64, 64, 32, 32, 32, 32]
ADJUSTED_TX_SIZE = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 10, 10, 10, 10, 10,
                   5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
                   6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                   7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22]

# TX size indices
TX_4X4 = 0
TX_8X4 = 10
TX_16X4 = 18  # approximate; actual index depends on enum ordering

# Verify TX_16X4 index from dbg output: tx_w=16, tx_h=4
# Looking at TX_WIDTH/TX_HEIGHT arrays:
# Index 18: TX_WIDTH[18]=16, TX_HEIGHT[18]=4 -> TX_16X4 = 18
TX_16X4 = 18

# Block at px=(48,64) from current DBG: 16x4, ADST_DCT (tx_type=1)
# quant from DBG line 129 (first 64 values):
quant = [
    -2, 1, 0, 0, 0, -1, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, -1, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    -1, 0, -1, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
]

# dequant from DBG line 130 (qindex_dc=128 qindex_ac=128)
# dequantize_coeffs applies DC_QLOOKUP_8[128]=140 and AC_QLOOKUP_8[128]=176
dequant = [
    -280, 176, 0, 0, 0, -176, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, -176, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    -176, 0, -176, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
]

w, h = 16, 4
tx_type = 1  # ADST_DCT
tx_size = TX_16X4

# Run exact integer iTF
residual = inverse_transform(dequant, tx_type, tx_size)

print("Row 0 residual (ref):", residual[:8])
print("Row 0 residual (Kinetix):", [-15, -12, -8, -4, -2, -1, 1, 3])
print("Diff:", [residual[i] - kinetix for i, kinetix in enumerate([-15, -12, -8, -4, -2, -1, 1, 3])])

# Print all rows for comparison
print("\nFull residual from integer iTF:")
for r in range(h):
    print(f"  row {r}: {residual[r*w:(r+1)*w]}")

print("\nKinetix residual from DBG:")
kinetix_rows = [
    [-15, -12, -8, -4, -2, -1, 1, 3, 5, 4, 0, -5, -10, -14, -15, -15],
    [-5, -2, 2, 3, 1, -1, -3, -1, 1, 2, 0, -5, -9, -9, -8, -6],
    [3, 6, 8, 7, 1, -5, -9, -8, -5, -2, -4, -7, -9, -7, -2, 2],
    [-6, -3, 1, 1, -4, -10, -13, -11, -7, -5, -8, -13, -17, -16, -11, -8],
]
for r in range(h):
    print(f"  row {r}: {kinetix_rows[r]}")

# Compare
print("\nPer-row diffs:")
for r in range(h):
    ref_row = residual[r*w:(r+1)*w]
    kin_row = kinetix_rows[r]
    diffs = [ref_row[i] - kin_row[i] for i in range(w)]
    print(f"  row {r} diff: {diffs}")
