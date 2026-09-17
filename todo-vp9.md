# todo-vp9.md — VP9 decoder worklog

Sibling of `todo-h264.md` / `todo-av1.md`. Tracks the native VP9 decoder in
`tpt-kinetix-vp9` (Phase 9 royalty-free codec roadmap, see `todo.md` and
`docs/codec-backlog.md`).

## Session 2026-09-17 (#v1 — crate bootstrap, full pipeline, not pixel-exact)

**Goal.** Start the VP9 decode crate planned in todo.md Phase 9: "new
`tpt-kinetix-vp9` crate via the cargo-generate template + kg ingest of
libvpx/FFmpeg `vp9*.c` … bit-exact vs `ffmpeg -c:v vp9`".

**What exists now.**

- Crate scaffolded (`tpt-kinetix-vp9`), registered in `[workspace] members`,
  fuzz dir excluded like the other codecs. Template + `docs/adding-a-codec.md`
  process followed (hand-completion from a pinned C reference).
- **Tables (`src/tables.rs`) are mechanically extracted** from FFmpeg commit
  `c3ff71680805267bc8f3fff86c1cf917f810c0d9` via
  `cargo run -p tpt-kinetix-kg -- extract-tables`, with `verify-tables:`
  markers: **26/26 verified OK** (`cargo run -p tpt-kinetix-kg -- verify-tables
  tpt-kinetix-vp9/src/tables.rs`). Covers coefficient probs (compact band-0
  layout, `coef_model_idx`/`Counts::coef_bin` index pairs), model→full pareto8
  expansion, kf/inter mode/partition/MV probs, dequant lookups, all 12 scan
  orders + neighbours, sub-pel filter kernels, `inv_map_table`. The
  `default_probs` *struct* and the DECLARE_ALIGNED subpel filters are
  hand-transcribed with provenance comments (verify-tables cannot parse a
  struct const / DECLARE_ALIGNED declarations — same precedent as H.264's
  packed tables). `tpt-kinetix-vp9/gen_tables.sh` regenerates the file.
- **Bool decoder** (`booldec.rs`) round-trip tested against the canonical
  RFC 6386 bool *encoder* re-implemented in the test module (2000 random
  symbols + literals, exact). Found & fixed a real refill bug this way
  (normalize pulled the same top bit repeatedly instead of advancing a bit
  cursor).
- **Full decode pipeline** (~4.5k lines): uncompressed header (incl. colour
  config `f(3)+f(1)` after the sync code, `frame_context_idx`), compressed
  header (tx mode, coef prob subexp updates + pareto8 expansion, mode/MV
  updates incl. the 7-bit-literal MV quirk), superframe splitting, tiles
  (4-byte BE size fields, per-tile marker bit, row/col band ranges),
  partition trees with the FFmpeg-style above/left partition-ctx bit caches,
  keyframe intra modes (per-sub-block for sub-8x8 with the kf spatial
  derivation), inter mode/ref/comp ladders (ported branch-for-branch incl.
  `fixcompref`/`varcompref`), MV prediction (`find_ref_mvs` port with the
  sub-8x8 `mem`/`mem_sub8x8` diff quirks and the zero-MV fallback),
  `fill_mv`/`read_mv_component` + counts, coefficient token decoding
  (dequant-at-read, tx32/2, `cache[]` nnz context, band counts), inverse
  DCT/ADST 4/8/16/32 + lossless WHT with the reference's exact
  unsigned-shift rounding, intra prediction (all 10 modes + derived DC
  variants + edge padding), MC (8-tap luma×2 phases / chroma×16, border
  replication, compound `+1>>1` avg, scaled-reference path with progressive
  phase stepping), segmentation (incl. temporal prediction from the retained
  segmap ref), loop filter (level LUTs, `mask_edges` port, per-SB driver),
  frame-context probability adaptation (both refreshctx/parallelmode save
  semantics), `show_existing_frame`, `Vp9Decoder::decode(&Packet)` public API.
- **Bug already fixed via conformance:** `read_tree` treated leaf payload 0
  (`-0`, e.g. DC_PRED/ZEROMV) as "jump to node 0" → infinite walk; FFmpeg's
  `while (i > 0)` semantics adopted (`next <= 0` terminates). Also fixed: a
  tile-row range double-`/8` that made every decode a no-op, lossless scan-row
  selection, counts array sizing (528 contexts vs 1584 model values),
  64x64-block scratch sizes.

**State of correctness.** All 8 conformance clips (ffmpeg-generated
`libvpx-vp9`, profile 0 forced via `-pix_fmt yuv420p`: solid black
lossless/lossy, testsrc lossless, testsrc intra, inter, odd size 125x67,
2-tile) decode **without panics and with correct frame geometry**, but PSNR
vs `ffmpeg -c:v vp9` is ~5–50 dB → **not pixel-exact**;
`capabilities().pixel_exact == false`, strict mode returns `NotPixelExact`.
Lib tests (6) + conformance (8, ffmpeg-gated skip) + proptest `*_never_panics`
(20k-case sweep in release) green; clippy `-D warnings` clean;
`verify-tables` 26/26.

**Leading hypothesis for the remaining pixel gap (next session):** on the
solid-black lossless clip the output looks like *pure prediction with no
residual applied* (≈128 flat vs reference 16) while our parse reads
`skip=true` for blocks that must carry residual → suspect skip-flag context/
prob bookkeeping or a desync introduced earlier in the compressed-header
update walk. Recommended method: single-frame clip, compare our bool-decoder
bit position after the compressed header against `header_size`, then trace
block (0,0) symbol-by-symbol. Then bisect coefficients → transforms → intra
edges → loop filter (AV1 playbook).

**Also noted this session.**
- ffmpeg's `libvpx-vp9` defaults to profile 1 (4:4:4/gbrp) for RGB sources —
  conformance clips must pass `-pix_fmt yuv420p`. Our profile-0-only rejection
  caught this immediately.
- `cargo +nightly fuzz run` cannot link on this machine (missing
  `librustc-nightly_rt.asan.a` / libFuzzer C++ runtime in the local nightly);
  same for the existing AV1 fuzz crate — CI's Ubuntu `fuzz-check` job is the
  compile gate. Local deep-fuzz substitute: `PROPTEST_CASES=20000 cargo test
  --release -p tpt-kinetix-vp9 --test proptest_vp9` (passes).
- Parallel `cargo test` runs in this workspace can block on the target-dir
  lock while another session builds — a hung-looking test may just be waiting.

**Open work (priority order).**
1. Pixel-exact keyframes: skip-flag/mode desync hunt on `testsrc_lossless`
   (single frame), then intra128.
2. Lossless path WHT + coefficient verification once intra is exact.
3. Inter frames: MV ref ladders are ported but unexercised until intra works;
   conformance has a 4-frame clip ready.
4. Loop filter verification (enable after recon is exact; clips have
   `lf.level > 0`).
5. Scaled-reference MC exists but has no corpus clip yet (needs an
   encode-time resolution change clip).
6. Flip `capabilities().pixel_exact` only when the whole conformance set is
   bit-exact, then wire VP9 into `tpt-kinetix-pipeline`/CLI decode paths and
   the README status table.
