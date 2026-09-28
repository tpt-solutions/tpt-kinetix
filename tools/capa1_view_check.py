"""Compare dumped field views (KINETIX_DUMP_FIELD_VIEWS) against ITU reference fields.

Each extracted reference view should be byte-identical to SOME field (top/bottom)
of SOME reference frame — the view is a pure copy of already-reconstructed data.
Prints one row per distinct view: best-matching ref field + ndiff/max.
"""
import os
import re
import sys
from collections import defaultdict

TEMP = os.environ["TEMP"]
VIEWS = os.path.join(TEMP, "capa1views")
REF = "tpt-kinetix-h264/tests/fixtures/itu/CAPA1_TOSHIBA_B/CAPA1_TOSHIBA_B_dec.yuv"
W, H = 352, 288
FW, FH = W, H // 2
FRAME_LEN = W * H * 3 // 2
FIELD_LEN = FW * FH

ref = open(REF, "rb").read()
nref = len(ref) // FRAME_LEN


def ref_field(frame_idx, bottom):
    base = frame_idx * FRAME_LEN
    out = bytearray(FIELD_LEN)
    for fy in range(FH):
        y = 2 * fy + (1 if bottom else 0)
        out[fy * FW:(fy + 1) * FW] = ref[base + y * W: base + y * W + FW]
    return bytes(out)


REFS = {}
for i in range(nref):
    for b in (False, True):
        REFS[(i, b)] = ref_field(i, b)


def stats(a, b):
    n = 0
    mx = 0
    for x, y in zip(a, b):
        if x != y:
            n += 1
            d = abs(x - y)
            if d > mx:
                mx = d
    return n, mx


pat = re.compile(
    r"view_(l[01])(\d+)_poc(-?\d+)_bottom(true|false)_frame(true|false)\.gray$")
views = defaultdict(dict)
for fn in os.listdir(VIEWS):
    m = pat.match(fn)
    if not m:
        continue
    lst, idx, poc, bot, isf = (
        m.group(1), int(m.group(2)), int(m.group(3)),
        m.group(4) == "true", m.group(5) == "true",
    )
    views[(lst, idx, poc, bot, isf)][fn] = open(os.path.join(VIEWS, fn), "rb").read()

print("distinct view keys: %d  files: %d" % (len(views), sum(len(v) for v in views.values())))

bad = 0
mislabel = 0
for key in sorted(views, key=lambda k: (k[2], k[0], k[1], k[3])):
    lst, idx, poc, bot, isf = key
    # All dumps for one key should agree (one slice per (poc,parity) here).
    data = list(views[key].values())[0]
    best = None
    for i in range(nref):
        for b in (False, True):
            n, mx = stats(data, REFS[(i, b)])
            if best is None or (n, mx) < best[:2]:
                best = (n, mx, i, b)
    n, mx, i, b = best
    tag = "%s[%d] poc=%d bot=%s frame=%s" % (lst, idx, poc, bot, isf)
    # The poc<->frame mapping for this clip (from the field census): coded
    # TOP field poc p lives in ref frame (p+4)//2, BOT field poc p in (p+3)//2.
    exp_i = (poc + 4) // 2 if not bot else (poc + 3) // 2
    if n != 0:
        bad += 1
        print("BAD      %-42s best=(frm %d %s) ndiff=%6d max=%4d" %
              (tag, i, "BOT" if b else "TOP", n, mx))
    if (i, b) != (exp_i, bot):
        mislabel += 1
        print("MISLABEL %-42s label implies (frm %d %s) but content is (frm %d %s) ndiff=%d" %
              (tag, exp_i, "BOT" if bot else "TOP", i, "BOT" if b else "TOP", n))
print("views not byte-identical to any reference field: %d / %d" % (bad, len(views)))
print("views whose content contradicts their poc/parity label: %d / %d" % (mislabel, len(views)))
