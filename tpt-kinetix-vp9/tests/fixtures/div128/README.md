# Pending-conformance repro: mid-stream key frame, luma 4x4 coef desync

`keyframe128.ivf` is frame 128 of a 300-frame `testsrc` 320x240 clip encoded by
libvpx (`-deadline good -cpu-used 4 -lag-in-frames 0`, the verified conformance
envelope). It is a **key frame** (self-contained; decodes standalone with a
fresh decoder). `keyframe128.reference.yuv` is its pixel-exact decode per
ffmpeg's native decoder, cross-checked identical to ffmpeg's libvpx decoder.

## Symptom

Kinetix's `Vp9Decoder` output differs from the reference in **luma only**,
bottom-right region x 234-319, y 208-239 (2573 px, max diff 147). Chroma is
byte-exact. The divergence is present decoding the frame alone, so it is not
stream-state carryover.

## Localization (2026-10-02 session)

- Pre-loop-filter buffers already diverge (`TPT_VP9_BUF` vs reference), and
  post == pre: this is a **reconstruction** bug, not the loop filter.
- The first wrong samples sit on 4x4 boundaries inside one superblock;
  whole 4x4s decode black (1-2) or shifted (95 vs 150) while neighbours are
  correct — a **coefficient-decode desync** in a mixed-block-size
  neighbourhood (4x4s below a 16x16 and beside 8x8 skip blocks).
- This is the same bug class the oracle-driven sessions (todo-vp9.md #v4)
  fixed five of; the instrumented libvpx oracle build
  (`%TEMP%/libvpx2/vpxsym.exe`) no longer exists and needs rebuilding before
  the next push (diff `SYMP`/`EOBCHK`/`TOK`/`CP` streams per todo-vp9.md
  Session #v4 methodology).

Until fixed, libvpx-encoded clips longer than ~2s of content diverge and are
reported `UNVERIFIED` by `just bench-ffmpeg` (the perf corpus stays pinned to
clips that verify).
