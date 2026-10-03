# Pending-conformance repro: mid-stream key frame, loop-filter edge divergence

`keyframe128.ivf` is frame 128 of a 300-frame `testsrc` 320x240 clip encoded by
libvpx (`-deadline good -cpu-used 4 -lag-in-frames 0`, the verified conformance
envelope). It is a **key frame** (self-contained; decodes standalone).
`keyframe128.reference.yuv` is its pixel-exact decode per ffmpeg's native
decoder, cross-checked identical to ffmpeg's libvpx decoder.

## Symptom

Kinetix's `Vp9Decoder` output differs in **luma only**, first at pixel
(235, 208) in the full clip; 2573 samples total (solo decode).

## Isolation status (2026-10-03, oracle-driven — instrumented libvpx v1.13.1)

The instrumented libvpx oracle was **rebuilt** this session (see "Rebuilding
the oracle" below) and the following are now **proven bit-exact** against it
on the solo keyframe:

- the entire coefficient token stream: every `PTE` (band, context) and `PTV`
  (token value) — 9109 lines identical;
- every dequantized coefficient value (`PTVV`) — 12574 lines identical;
- every intra prediction: all 867 luma transform blocks, keyed by
  (mi_row, mi_col, ao, lo, tx) — pixel-identical.

The remaining divergence is in the **deblocking loop filter**: with both
decoders' loop filters disabled (`VP9NOLF=1` / `TPT_VP9_NO_LF=1`), the frames
agree until pixel (240, 208) — the first corrupt *prediction-derived* sample.
The first corrupt sample overall (post-LF, full clip: (235, 208)) sits one
sample left of the vertical block edge at x=232 between:

- mi(0,28)-of-the-solo-layout — a **skip** 8x8 DC block (flat 145), and
- mi(0,29) — a 4x4-partitioned intra block (D153/TM sub-modes).

At that edge our filter modified sample (231, 2) by −1 where libvpx leaves it
untouched: a per-edge level/skip-rule difference in `tpt-kinetix-vp9/src/
loop_filter.rs` (likely the skip-block or mode-delta level derivation for
edges between a skip 8x8 and a 4x4-partitioned neighbour). One wrong filtered
sample then cascades through neighbouring edges.

## Rebuilding the oracle

/tmp/libvpx (v1.13.1, provenance-only BSD extraction) + `vpxsym.c` harness.
Patches: `PTE/PTV/PTVV` token traces in vp9_detokenize.c, `PRED`/`EDGE` dumps
in vp9_reconintra.c, `VP9NOLF` env gate on the loop filter invocations in
vp9_decodeframe.c. Build: `../configure --disable-unit-tests --disable-vp8
--disable-examples --disable-tools --disable-docs --disable-vp9-encoder &&
make`, then `gcc -O2 -I.. -I. ../vpxsym.c libvpx.a -o /tmp/vpxsym.exe -lm`.
Env: `VP9PT=1` (token traces), `VP9NOLF=1` (filter off). Ours: the same env
vars via `tpt-kinetix-vp9 --example dbg_trace` (`TPT_VP9_TRACE=1
TPT_VP9_NO_LF=1 TPT_VP9_YUV=...`, plus the `KEDGE`/`COEF` dumps gated on
`VP9PT` added to frame_recon.rs this session).

Until fixed, libvpx-encoded clips longer than ~2s of content diverge and are
reported `UNVERIFIED` by `just bench-ffmpeg` (the perf corpus stays pinned to
clips that verify).
