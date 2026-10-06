# CLOSED 2026-10-06: mid-stream key frame divergence — n>4 D153/D117 intra predictors

`keyframe128.ivf` is frame 128 of a 300-frame `testsrc` 320x240 clip encoded by
libvpx (`-deadline good -cpu-used 4 -lag-in-frames 0`, the verified conformance
envelope). It is a **key frame** (self-contained; decodes standalone).
`keyframe128.reference.yuv` is its pixel-exact decode per ffmpeg's native
decoder, cross-checked identical to ffmpeg's libvpx decoder.

**Kinetix now decodes it byte-exact**; the conformance suite asserts it. The
same fix closed the previously-`#[ignore]`d 640x360 and 1080p real-content
cases. Keep the fixture: it is the only pinned decode of an n>4 D153 block in
the repo (the conformance corpus's encoder-parameter envelope never selects
one).

## What the bug actually was

The historical notes at the bottom (the 2026-10-03 isolation) blamed a
"per-edge loop-filter level/skip-rule difference". That diagnosis was **wrong**,
in two steps:

1. **The instrumented libvpx oracle itself was corrupted.** The previous
   session's dump instrumentation inside `filter_selectively_horiz` created a
   dangling-`else`:

   ```c
   if (mask_4x4_int & 1)
     if (pt_ops()) { /* IN dump */ }
   vpx_lpf_horizontal_4(s + 4 * pitch, ...);   /* ran UNCONDITIONALLY */
   if (pt_ops()) { /* OUT dump */ }            /* also unconditional */
   else if (mask_4x4_int & 2)                  /* bound to if (pt_ops())! */
   ```

   so the oracle filtered 4x4-int edges libvpx should have skipped (389 ops
   per frame too many) and skipped others. That is where the oracle's
   "9-sample quirk vs the ffmpeg reference" came from — an oracle that
   disagrees with the reference is not an oracle. The 2026-10-03 token/dequant/
   prediction traces were taken through `vp9_reconintra.c`/`vp9_detokenize.c`
   and remain valid; the loop-filter-level conclusions drawn from the
   corrupted `vp9_loopfilter.c` do not.

2. **With the oracle repaired (byte-exact vs ffmpeg again), the loop filter
   exonerated cleanly**: the per-superblock op streams (position, width,
   level, and every vertical kernel's in/out windows) are identical between
   the two decoders, and with both decoders' loop filters disabled the frames
   still diverge from pixel (240, 208) — i.e. the difference is in
   **reconstruction**, not filtering.

Root cause: `tpt-kinetix-vp9/src/predict.rs`'s n>4 (`TX_8X8` and up)
intra-prediction paths for **D153 (`HOR_DOWN_PRED`)** and **D117
(`VERT_RIGHT_PRED`)** indexed the left column bottom-up where the reference
(`vpx_d153_predictor`/`vpx_d117_predictor`) is top-down, and D153's interior
shift loop wrote rows 2..n instead of rows 1..n-1 — leaving row 1's tail as
stale zeros (the "vertically smeared" corruption) and bleeding one row past
the block. One D153 8x8 block at (232, 208) on this keyframe was enough; the
error then spread through neighbouring predictions and (on inter frames) the
motion-compensated copies of them.

The 4x4 unrolled branches were always correct (the byte-exact conformance
corpus pins those); the generic branches are now pinned by
`predict.rs::tests::diagonal_predictors_generic_sizes_match_the_reference`,
which transcribes all six diagonal predictors' C source with pseudo-random
edges at n = 8/16/32 — flat edges would mask index-order bugs, and the
corpus's encoder envelope never selects these paths at all.

## Rebuilding the oracle (historical, and a warning)

`/tmp/libvpx` (v1.13.1) + `vpxsym.c` harness, dumps gated on `VP9OPS`
(per-kernel in/out windows with plane-relative offsets and levels; `OPS in=`
/ `OPSOUT out=` line pairs, `s=<seq> [tag]` on dual calls) and `VP9PT`
(token/prediction traces). Ours: `TPT_VP9_OPS=1` (`OPD` lines) /
`TPT_VP9_TRACE=1` (`KEDGE`/`PREDP`) / `VP9PT=1` (`COEF`) via
`tpt-kinetix-vp9 --example dbg_trace`; strides and border layout match
(320-wide frame → 384 both sides), so dumped offsets compare directly.

Build: edit `vp9/common/vp9_loopfilter.c`, then

    cd /tmp/libvpx/build
    gcc -I.. -I. -fno-common -m64 -O2 -c ../vp9/common/vp9_loopfilter.c -o /tmp/lf.o
    cp /tmp/lf.o vp9_loopfilter.c.o && ar r libvpx.a vp9_loopfilter.c.o
    gcc -O2 -I.. -I. ../vpxsym.c libvpx.a -o /tmp/vpxsym.exe -lm

(`make` fails on `-Werror` unused-variable warnings; compile manually.)

**Before trusting any dump: verify the rebuilt oracle still decodes the clip
byte-exact vs `keyframe128.reference.yuv` (or ffmpeg).** The dangling-else
above survived at least two sessions of "verified" oracle work because this
check was only run against the oracle's own earlier output. Dump instrumentation
that wraps single statements in `if (...)` without braces is exactly how this
class of bug enters — brace everything you touch.

## Historical isolation notes (2026-10-03 — conclusions superseded, method kept)

`keyframe128.reference.yuv` is pixel-exact per ffmpeg. Kinetix differed in
luma only, first at pixel (235, 208) post-filter; 2573 samples total (solo
decode), all x >= 234, y >= 208.

The 2026-10-03 oracle work proved bit-exact (against the *then*-instrumented
libvpx v1.13.1, and still true today): the entire coefficient token stream
(every `PTE` (band, context) and `PTV` (token value); every dequantized
coefficient (`PTVV`)); every intra prediction up to the first bad block —
all 867 luma transform blocks keyed by (mi_row, mi_col, ao, lo, tx) matched
until the D153 block at (232, 208), whose prediction wrote a zero staircase
into rows 1-3. The claim that "the remaining divergence is in the deblocking
loop filter ... a per-edge level/skip-rule difference" was an artifact of the
corrupted oracle described above.
