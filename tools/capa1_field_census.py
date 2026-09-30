"""Per-(poc, parity) field census for CAPA1_TOSHIBA_B.

Compares our post-deblock / pre-deblock field luma dumps (KINETIX_FIELD_BUF_OUT)
against the ITU reference frames split into top/bottom fields, matching by SAD.
Prints one row per coded field so the residual can be localised to a poc+parity
rather than to a display frame.
"""
import os
import re
import sys
from collections import defaultdict

TEMP = os.environ["TEMP"]
DUMP = os.path.join(TEMP, "capa1f")
REF = "out-kinetix-h264/tests/fixtures/itu/CAPA1_TOSHIBA_B/CAPA1_TOSHIBA_B_dec.yuv"
W, H = 352, 288
# A coded field is FULL width, HALF height. (Getting this wrong as (W//2, H//2)
# silently compares the left half of our field against a half-width reference
# slice and makes every field look ~50% wrong.)
FW, FH = W, H // 2
FRAME_LEN = W * H * 3 // 2
FIELD_LEN = FW * FH

ref = open(REF, "rb").read()
nref = len(ref) // FRAME_LEN


def ref_field(frame_idx, bottom):
    """Luma of one field of a reference frame: bottom=True -> odd rows."""
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


def sad(a, b):
    return sum(abs(x - y) for x, y in zip(a, b))


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


def load(kind):
    """kind in {'bpost','bpre'} -> {(poc, bottom): bytes}"""
    out = {}
    pat = re.compile(r"f_%s_poc(-?\d+)_bottom(true|false)\.gray$" % kind)
    for fn in os.listdir(DUMP):
        m = pat.match(fn)
        if not m:
            continue
        out[(int(m.group(1)), m.group(2) == "true")] = open(
            os.path.join(DUMP, fn), "rb").read()
    return out


post = load("bpost")
pre = load("bpre")
print("coded B fields: post=%d pre=%d" % (len(post), len(pre)))

used = {}
rows = []
for poc in sorted({p for p, _ in post}):
    for b in (False, True):
        if (poc, b) not in post:
            continue
        # best reference field, excluding ones already claimed
        best = None
        for i in range(nref):
            if (i, b) in used:
                continue
            s = sad(post[(poc, b)], REFS[(i, b)])
            if best is None or s < best[0]:
                best = (s, i)
        s, i = best
        used[(i, b)] = (poc, b)
        n, mx = stats(post[(poc, b)], REFS[(i, b)])
        if n == 0:
            continue
        # does the error already exist before deblocking?
        np_, mxp = stats(pre[(poc, b)], REFS[(i, b)])
        # and how much does our own deblocking step itself change?
        nd, mxd = stats(post[(poc, b)], pre[(poc, b)])
        rows.append((poc, b, i, n, mx, np_, mxp, nd, mxd))

print()
print("poc  parity  ref_frm  post(ndiff/max)  pre(ndiff/max)*  our_deblock(ndiff/max)")
print("* the 'pre' column is NOT a correctness test: the reference YUV is")
print("  POST-deblock, so a pre-deblock dump is EXPECTED to differ from it by")
print("  roughly what deblocking changes (~4000 samples, max<=6). Read")
print("  'our_deblock' (our own pre->post footprint) instead.")
for poc, b, i, n, mx, np_, mxp, nd, mxd in rows:
    parity = "BOT" if b else "TOP"
    print(
        "%5d  %s  %5d  %6d/%4d  %6d/%4d  %6d/%4d"
        % (poc, parity, i, n, mx, np_, mxp, nd, mxd)
    )

nrec = sum(1 for r in rows if r[5] > 0)
ndb = sum(1 for r in rows if r[5] == 0 and r[7] > 0)
print()
print("non-exact coded B fields: %d of %d" % (len(rows), len(post)))
print("  pre-deblock already differs from reference: %d" % nrec)
print("  pre-deblock exact, deblock introduced err:  %d" % ndb)

big = [r for r in rows if r[4] > 10]
print()
print("fields with post max > 10: %d  (TOP %d / BOT %d)"
      % (len(big), sum(1 for r in big if not r[1]), sum(1 for r in big if r[1])))
for r in big:
    print("   poc %-4d %s  ndiff=%-4d max=%-4d  our_deblock_max=%d"
          % (r[0], "BOT" if r[1] else "TOP", r[3], r[4], r[8]))

print()
print("MB map of wrong-sample counts (fields with ndiff <= 120).")
print("  '#' =>=20 wrong samples, '.' 1-19, '-' exact")
for poc, b, i, n, mx, np_, mxp, nd, mxd in rows:
    if n > 120:
        continue
    a = post[(poc, b)]
    r = REFS[(i, b)]
    print("  poc %-4d %s total=%d" % (poc, "BOT" if b else "TOP", n))
    for my in range(FH // 16):
        line = ""
        for mcol in range(W // 16):
            c = 0
            for yy in range(my * 16, my * 16 + 16):
                for xx in range(mcol * 16, mcol * 16 + 16):
                    if a[yy * FW + xx] != r[yy * FW + xx]:
                        c += 1
            line += "-" if c == 0 else ("#" if c >= 20 else ".")
        print("    " + line)

# Aggregate: how many FIELDS have at least one wrong sample in each MB row /
# column, and the total wrong samples per MB row / column.
MBY, MBX = FH // 16, W // 16
row_fields = [0] * MBY
col_fields = [0] * MBX
row_samples = [0] * MBY
col_samples = [0] * MBX
mb_hits = 0
for poc, b, i, n, mx, np_, mxp, nd, mxd in rows:
    a = post[(poc, b)]
    r = REFS[(i, b)]
    seen_row = set()
    seen_col = set()
    for my in range(MBY):
        for mcol in range(MBX):
            c = 0
            for yy in range(my * 16, my * 16 + 16):
                for xx in range(mcol * 16, mcol * 16 + 16):
                    if a[yy * FW + xx] != r[yy * FW + xx]:
                        c += 1
            if c:
                mb_hits += 1
                seen_row.add(my)
                seen_col.add(mcol)
                row_samples[my] += c
                col_samples[mcol] += c
    for my in seen_row:
        row_fields[my] += 1
    for mcol in seen_col:
        col_fields[mcol] += 1

print()
print("aggregate over %d non-exact fields (%d MBs each = %d total)"
      % (len(rows), MBY * MBX, len(rows) * MBY * MBX))
print("  MBs with >=1 wrong sample: %d (%.1f%%)"
      % (mb_hits, 100.0 * mb_hits / (len(rows) * MBY * MBX)))
print()
print("  MB row : fields_touched  wrong_samples")
for my in range(MBY):
    print("     %2d   :  %2d/%2d  %6d  %s"
          % (my, row_fields[my], len(rows), row_samples[my], "#" * row_fields[my]))
print()
print("  MB col : fields_touched  wrong_samples")
for mcol in range(MBX):
    print("     %2d   :  %2d/%2d  %6d  %s"
          % (mcol, col_fields[mcol], len(rows), col_samples[mcol], "#" * col_fields[mcol]))

# For the cleanest targets (few affected MBs), print the wrong samples at 4x4
# block resolution plus exact coordinates. The shape of the dirty region inside
# one macroblock identifies the partition, which is what discriminates between
# the MV-derivation paths.
print()
print("=" * 70)
print("FINE TARGETS: 4x4 block map of wrong samples (fields with ndiff <= 40)")
for poc, b, i, n, mx, np_, mxp, nd, mxd in rows:
    if n > 40:
        continue
    a = post[(poc, b)]
    r = REFS[(i, b)]
    aff = []
    for my in range(MBY):
        for mcol in range(MBX):
            c = 0
            for yy in range(my * 16, my * 16 + 16):
                for xx in range(mcol * 16, mcol * 16 + 16):
                    if a[yy * FW + xx] != r[yy * FW + xx]:
                        c += 1
            if c:
                aff.append((mcol, my, c))
    if not aff:
        continue
    print()
    print("poc %-4d %s total=%d max=%d affected MBs=%s"
          % (poc, "BOT" if b else "TOP", n, mx, aff))
    for mcol, my, c in aff:
        print("  MB(col=%d,row=%d) %d wrong; 4x4 block counts:" % (mcol, my, c))
        for by in range(4):
            cells = []
            for bx in range(4):
                cnt = 0
                for yy in range(my * 16 + by * 4, my * 16 + by * 4 + 4):
                    for xx in range(mcol * 16 + bx * 4, mcol * 16 + bx * 4 + 4):
                        if a[yy * FW + xx] != r[yy * FW + xx]:
                            cnt += 1
                cells.append(cnt)
            print("      " + " ".join("%2d" % v for v in cells))
        pts = [(xx, yy, a[yy * FW + xx] - r[yy * FW + xx])
               for yy in range(my * 16, my * 16 + 16)
               for xx in range(mcol * 16, mcol * 16 + 16)
               if a[yy * FW + xx] != r[yy * FW + xx]]
        print("      (x,y,delta) rel MB origin: "
              + " ".join("(%d,%d)%+d" % (x - mcol * 16, y - my * 16, d)
                        for x, y, d in pts))

# The P-field pictures (the f_post_/f_pre_ dumps) get the same treatment. If
# the P fields are bit-exact while every B field is not, the defect is confined
# to B-field-specific logic (temporal direct / colocated reads), NOT to the
# shared MC, residual, or deblocking paths.
pat_p = re.compile(r"f_post_poc(-?\d+)_bottom(true|false)\.gray$")
pfields = {}
for fn in os.listdir(DUMP):
    m = pat_p.match(fn)
    if m:
        pfields[(int(m.group(1)), m.group(2) == "true")] = open(
            os.path.join(DUMP, fn), "rb").read()

used_p = {}
p_exact = 0
p_bad = []
for poc in sorted({p for p, _ in pfields}):
    for b in (False, True):
        if (poc, b) not in pfields:
            continue
        best = None
        for i in range(nref):
            if (i, b) in used_p:
                continue
            s = sad(pfields[(poc, b)], REFS[(i, b)])
            if best is None or s < best[0]:
                best = (s, i)
        s, i = best
        used_p[(i, b)] = (poc, b)
        n, mx = stats(pfields[(poc, b)], REFS[(i, b)])
        if n == 0:
            p_exact += 1
        else:
            p_bad.append((poc, b, i, n, mx))

print()
print("=" * 70)
print("P-FIELD pictures (f_post_ dumps): %d exact, %d non-exact"
      % (p_exact, len(p_bad)))
for poc, b, i, n, mx in p_bad:
    print("   poc %-4d %s ref=%d ndiff=%d max=%d"
          % (poc, "BOT" if b else "TOP", i, n, mx))
