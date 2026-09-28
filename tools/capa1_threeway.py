"""Three-way sample analysis: ITU ref vs our post vs ffmpeg-nolf vs our pre.

For a given (poc, parity) field, classify our post-deblock error samples:
  at samples where ours != ITU:
    n_ff_match  -> ff_nolf == ITU there  (deblock is a no-op there: our value
                   came out wrong => a RECON-side error, not a deblock miss)
    n_unfilt    -> ours == ff_nolf != ITU (our sample equals the UNFILTERED
                   value => our deblock failed to filter that edge)
    n_other     -> ours != ff_nolf != ITU (mixed / cascade differences)
And the control: samples where ours == ITU but ff_nolf != ITU (edges where we
filter correctly and ffmpeg's unfiltered decode drifted).
"""
import os
import sys
import re
from collections import defaultdict

TEMP = os.environ["TEMP"]
DUMP = os.path.join(TEMP, "capa1f")
ITU = "tpt-kinetix-h264/tests/fixtures/itu/CAPA1_TOSHIBA_B/CAPA1_TOSHIBA_B_dec.yuv"
FFN = os.path.join(TEMP, "capa1_nolf.yuv")
W, H = 352, 288
FW, FH = W, H // 2
FRAME_LEN = W * H * 3 // 2
FIELD_LEN = FW * FH

itu = open(ITU, "rb").read()
ffn = open(FFN, "rb").read()
nframes = min(len(itu), len(ffn)) // FRAME_LEN


def field(buf, base, bottom):
    out = bytearray(FIELD_LEN)
    for fy in range(FH):
        y = 2 * fy + (1 if bottom else 0)
        out[fy * FW:(fy + 1) * FW] = buf[base + y * W: base + y * W + FW]
    return out


def load(kind):
    out = {}
    pat = re.compile(r"f_%s_poc(-?\d+)_bottom(true|false)\.gray$" % kind)
    for fn in os.listdir(DUMP):
        m = pat.match(fn)
        if not m:
            continue
        out[(int(m.group(1)), m.group(2) == "true")] = bytearray(
            open(os.path.join(DUMP, fn), "rb").read())
    return out


def sad(a, b):
    return sum(abs(x - y) for x, y in zip(a, b))


post = load("bpost")
pre = load("bpre")

# map (poc,parity) -> itu frame idx via min-SAD of our post vs itu fields
ITU_F = {}
for i in range(nframes):
    for b in (False, True):
        ITU_F[(i, b)] = field(itu, i * FRAME_LEN, b)
FFN_F = {}
for i in range(nframes):
    for b in (False, True):
        FFN_F[(i, b)] = field(ffn, i * FRAME_LEN, b)

targets = [(int(x), None) for x in sys.argv[1:]] or sorted(
    {p for p, _ in post})
used = set()
mapping = {}
for poc in sorted({p for p, _ in post}):
    for b in (False, True):
        if (poc, b) not in post:
            continue
        if targets and (int(poc), None) not in targets:
            continue
        best = None
        for i in range(nframes):
            if (i, b) in used:
                continue
            s = sad(post[(poc, b)], ITU_F[(i, b)])
            if best is None or s < best[0]:
                best = (s, i)
        _, i = best
        used.add((i, b))
        mapping[(poc, b)] = i

print("poc par ff  | ours!=ITU: n / (ff==ITU, ours==ff_unfiltered, other)")
for (poc, b), i in sorted(mapping.items(), key=lambda kv: kv[0][0]):
    B = post[(poc, b)]
    A = ITU_F[(i, b)]
    C = FFN_F[(i, b)]
    D = pre.get((poc, b))
    n_err = n_ffmatch = n_unfilt = n_other = 0
    pre_eq_ffn = pre_ne = 0
    for s in range(FIELD_LEN):
        if B[s] != A[s]:
            n_err += 1
            if C[s] == A[s]:
                n_ffmatch += 1
            elif B[s] == C[s]:
                n_unfilt += 1
            else:
                n_other += 1
            if D is not None:
                if D[s] == C[s]:
                    pre_eq_ffn += 1
                else:
                    pre_ne += 1
    n_ctl = sum(1 for s in range(FIELD_LEN) if B[s] == A[s] and C[s] != A[s])
    print("%4d %s %2d | err=%-5d ffmatch=%-5d unfilt=%-5d other=%-5d | pre==ffn@err: %d, pre!=ffn@err: %d | ctl(we-ok,ff-drift)=%d"
          % (poc, "BOT" if b else "TOP", i, n_err, n_ffmatch, n_unfilt,
             n_other, pre_eq_ffn, pre_ne, n_ctl))
