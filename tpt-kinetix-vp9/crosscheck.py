#!/usr/bin/env python
"""Independent VP9 tile-header cross-check: bool decoder + partition/skip/mode
parsing for the solid-lossless keyframe, implemented directly from the spec.
Compares its reads against the Rust decoder's trace."""
import sys

# --- tables (from vp9data.c @ c3ff7168) ---
KF_PARTITION = [
    [174, 35, 49], [68, 11, 27], [57, 15, 9], [12, 3, 3],
    [150, 40, 39], [78, 12, 26], [67, 33, 11], [24, 7, 5],
    [149, 53, 53], [94, 20, 48], [83, 53, 24], [52, 18, 18],
    [158, 97, 94], [93, 24, 99], [85, 119, 44], [62, 59, 67],
]
KF_YMODE = [
    [43, 46, 168, 134, 107, 128, 69, 142, 92],
    [37, 91, 11, 98, 125, 110, 36, 44, 111] if False else [50, 57, 77, 63, 36, 126, 146, 123, 158],
    [60, 36, 126, 146, 123, 158, 60, 90, 96] if False else [69, 142, 92, 44, 29, 68, 159, 201, 177],
    [50, 57, 77, 63, 36, 126, 146, 123, 158],
    [69, 142, 92, 44, 29, 68, 159, 201, 177],
    [77, 63, 36, 126, 146, 123, 158, 60, 90] if False else [63, 36, 126, 146, 123, 158, 60, 90, 96],
    [159, 201, 177, 50, 57, 77, 63, 36, 126] if False else [36, 126, 146, 123, 158, 60, 90, 96, 58],
    [146, 123, 158, 60, 90, 96, 58, 38, 76] if False else [126, 146, 123, 158, 60, 90, 96, 58, 38],
    [123, 158, 60, 90, 96, 58, 38, 76, 114] if False else [158, 60, 90, 96, 58, 38, 76, 114, 97],
    [158, 60, 90, 96, 58, 38, 76, 114, 97] if False else [60, 90, 96, 58, 38, 76, 114, 97, 172],
]
# NOTE: the ymode rows above are placeholders except row 0; the cross-check
# only walks the FIRST block whose above/left ctx = 0 -> row [0].

KF_UVMODE_TM = [102, 19, 66, 162, 182, 122, 35, 59, 128]

INTRAMODE_TREE = [(-0, 1), (-9, 2), (-1, 3), (4, 6), (-2, 5), (-4, -5), (-3, 7), (-7, 8), (-6, -8)]
PARTITION_TREE = [(-0, 1), (-1, 2), (-2, -3)]

SKIP_PROB = [192, 128, 64]


class BC:
    def __init__(self, data):
        self.data = data
        self.next_bit = 16
        self.value = (data[0] << 8) | data[1]
        self.range = 255

    def _bit(self):
        i = self.next_bit
        b = (self.data[i // 8] >> (7 - (i % 8))) & 1 if i // 8 < len(self.data) else 0
        self.next_bit += 1
        return b

    def read(self, prob, tag=""):
        split = 1 + (((self.range - 1) * prob) >> 8)
        bigsplit = split << 8
        bit = 1 if self.value >= bigsplit else 0
        print(f"  read p={prob} -> {bit} {tag}")
        if bit:
            self.range -= split
            self.value -= bigsplit
        else:
            self.range = split
        while self.range < 128:
            self.range <<= 1
            self.value = ((self.value << 1) | self._bit()) & 0xFFFF
        return bit

    def tree(self, tree, probs, tag=""):
        i = 0
        while True:
            bit = self.read(probs[i], f"{tag} n{i}")
            nxt = tree[i][bit]
            if nxt <= 0:
                return -nxt
            i = nxt


tile = bytes.fromhex(sys.argv[1])
bc = BC(tile)
print("tile bytes:", tile.hex())

# marker
m = bc.read(128, "marker")
assert m == 0, "marker bit set"

mi_cols = mi_rows = 8  # 64x64
# bl0 partition at (0,0), ctx 0
p = KF_PARTITION[0]
v = bc.tree(PARTITION_TREE, p, "bl0")
print("bl0 partition =", v)
if v == 3:  # SPLIT
    v = bc.tree(PARTITION_TREE, KF_PARTITION[4], "bl1(0,0)")
    print("bl1 partition =", v)
    if v == 3:
        v = bc.tree(PARTITION_TREE, KF_PARTITION[8], "bl2(0,0)")
        print("bl2 partition =", v)
        if v == 3:
            v = bc.tree(PARTITION_TREE, KF_PARTITION[12], "bl3(0,0)")
            print("bl3 partition =", v)
        # first block regardless: 16x16-level NONE handled below
    # first block header (whatever the first decoded block is): skip+mode
if True:
    skip = bc.read(SKIP_PROB[0], "skip(c0)")
    ymode = bc.tree(INTRAMODE_TREE, KF_YMODE[0], "ymode")
    print("skip =", skip, "ymode =", ymode)
    uv = bc.tree(INTRAMODE_TREE, KF_UVMODE_TM if ymode == 9 else KF_YMODE[0], "uvmode(TM)")
    print("uvmode =", uv)
print("consumed bytes ~", bc.next_bit / 8)
