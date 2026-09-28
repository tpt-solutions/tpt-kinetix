"""Compare our pre/post-deblock B-field dumps against ffmpeg's pre-deblock decode.

ffmpeg -skip_loop_filter all gives PRE-deblock reference frames (display order).
For each of our dumped B fields (bpre/bpost), extract the matching field rows of
the ffmpeg pre-deblock frame (matched by min-SAD against our post dump, reusing
the census's one-to-one assignment) and diff. This splits the residual:
  our_post vs ff_nolf == our recon+deblock vs recon-only reference
  our_pre  vs ff_nolf == our recon vs recon-only reference  <-- the recon test
"""
import os
import re
from collections import defaultdict

TEMP = os.environ["TEMP"]
DUMP = os.path.join(TEMP, "capa1f")
FF = os.path.join(TEMP, "capa1_nolf.yuv")
W, H = 352, 288
FW, FH = W, H // 2
FRAME_LEN = W * H * 3 // 2
FIELD_LEN = FW * FH

ff = open(FF, "rb").read()
nframes = len(ff) // FRAME_LEN


def ff_field(frame_idx, bottom):
    base = frame_idx * FRAME_LEN
    out = bytearray(FIELD_LEN)
    for fy in range(FH):
        y = 2 * fy + (1 if bottom else 0)
        out[fy * FW:(fy + 1) * FW] = ff[base + y * W: base + y * W + FW]
    return bytes(out)


def load(kind):
    out = {}
    pat = re.compile(r"f_%s_poc(-?\d+)_bottom(true|false)\.gray$" % kind)
    for fn in os.listdir(DUMP):
        m = pat.match(fn)
        if not m:
            continue
        out[(int(m.group(1)), m.group(2) == "true")] = open(
            os.path.join(DUMP, fn), "rb").read()
    return out


def sad(a, b):
    return sum(abs(x - y) for x, y in zip(a, b))


def stats(a, b):
    n = 0
    mx = 0
    first = None
    for i, (x, y) in enumerate(zip(a, b)):
        if x != y:
            if first is None:
                first = (i % FW, i // FW)
            n += 1
            d = abs(x - y)
            if d > mx:
                mx = d
    return n, mx, first


post = load("bpost")
pre = load("bpre")
print("B fields: post=%d pre=%d; ffmpeg frames=%d" % (len(post), len(pre), nframes))

# One-to-one frame assignment by min-SAD of our post vs ffmpeg nolf fields.
used = set()
rows = []
tot_pre = tot_post = 0
for poc in sorted({p for p, _ in post}):
    for b in (False, True):
        if (poc, b) not in post:
            continue
        best = None
        for i in range(nframes):
            if (i, b) in used:
                continue
            s = sad(post[(poc, b)], ff_field(i, b))
            if best is None or s < best[0]:
                best = (s, i)
        _, i = best
        used.add((i, b))
        np_, mxp, fp = stats(pre[(poc, b)], ff_field(i, b))
        ns, mxs, fs = stats(post[(poc, b)], ff_field(i, b))
        tot_pre += np_
        tot_post += ns
        if np_ or ns:
            rows.append((poc, b, i, np_, mxp, fp, ns, mxs, fs))

print()
print("poc  par  ff_frm   pre: ndiff/max/first   post: ndiff/max/first")
for poc, b, i, np_, mxp, fp, ns, mxs, fs in rows:
    print("%4d %s %5d   %6d/%3d/%-12s %6d/%3d/%s"
          % (poc, "BOT" if b else "TOP", i, np_, mxp, str(fp), ns, mxs, fs))
print()
print("fields with any diff: %d" % len(rows))
print("total pre-deblock differing samples vs ffmpeg-nolf: %d" % tot_pre)
print("total post-deblock differing samples vs ffmpeg-nolf: %d" % tot_post)
