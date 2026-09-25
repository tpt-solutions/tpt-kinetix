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

## Session 2026-09-17 (#v2 — four decode bugs fixed, first pixel-exact clips)

**Method that worked.** ffmpeg-gated micro-clip ladder: 16x16 `color=0xRRGGBB`
lossless clips whose Y is exactly 128/127/129/125, chosen so DC_128/V-fill
prediction makes specific clips pure-skip and others pure-±1/±2 DC residual.
Each rung isolates one pipeline stage; PSNR 99 = pixel-exact.

**Bugs found & fixed this session (all verified by the ladder).**
1. **Tile offsets were MI-granular, must be SB-granular** — reference
   `set_tile_offset`: `sb_start = (idx * sb_count) >> log2`, then `<< 3`.
   Our old `(mi * idx) >> log2` misrouted every multi-tile/multi-SB-range
   frame (fixed three clips decoding garbage).
2. **FFmpeg's kf ymode/uvmode probability tables are in FFmpeg's
   `IntraPredMode` enum order `[V, H, DC, D45, D113, D157, D203, D67, ..]`,
   NOT the spec order `[DC, V, H, ..]`** (proved by diffing
   `vp9_kf_y_mode_prob[dc][dc] = {137,30,42,...}` in libvpx against
   FFmpeg `[0][0] = {43,46,168,...}` = libvpx `[v][v]`). Added
   `SPEC_TO_FFMPEG_MODE` permutation in `header.rs` for the kf ymode/uvmode
   lookups and permuted `DEFAULT_PROBS.uv_mode` rows 0-2.
   (Same 1-in-3 rotation presumably applies to other FFmpeg VP9 tables that
   are mode-indexed — `INTER_MODE_CTX_LUT` and `INTRA_SIZE_GROUP` verified
   unaffected.)
3. **`have_top`/`have_left` were swapped** at the `substitute_mode` call in
   `gather_intra_edges` (chroma sub-blocks on frame edges substituted the
   wrong DC fill).
4. **D135/D113/D157 must NOT edge-substitute**: the reference `mode_conv`
   keeps them as-is without neighbors — they predict from the FILLED edges
   (top 127, left 129, top-left 127/129). Our wrong DC_127 substitution broke
   every clip the encoder coded with an angle mode on a frame corner (which
   is most of them — libvpx loves D135 on keyframe corners). Also TM with no
   neighbors substitutes to **DC_129** (we had DC_128).
5. **itxfm pass B processed tmp ROWS; the reference processes tmp COLUMNS**
   (`type_b_1d(tmp + i, sz, ...)` reads `tmp[i + x*sz]`). This was THE major
   reconstruction bug: every residual-bearing block got a transposed-second-
   pass transform. After the fix: **solid_black_lossy luma is pixel-exact
   (99 dB)** — the whole lossy intra pipeline (dequant, IDCT, prediction,
   residual add, sub-block walk) is verified on that clip.
6. MV-clamp `max_mv` underflowed (usize) for blocks overhanging odd frame
   sizes (125x67) — now signed.

**Ladder status after fixes.** micro128_skip / micro127_dc1 / micro129_dc1 /
micro129_dc2 / micro125_dc2 = **99/99/99 pixel-exact**; solid_black_lossy =
**Y 99.00** (chroma U/V ≈ 50 — chroma-only bug remains, likely LF or chroma
tx path); solid_black_lossless U = 99. Content-rich clips (testsrc etc.) still
7-12 dB — the coefficient parse still desyncs somewhere mid-tile (consumption
overshoots the tile size by ~5-20%), exact spot not yet found.

## Session 2026-09-17 (#v3 — THE coefficient-table dim swap found; 7 clips exact)

**The big one.** FFmpeg's coefficient probability table is
`coef[4][2][2][6][6][3]` indexed **[tx][plane][intra/inter]** — proven by the
runtime access `s->prob.coef[b->tx][0 /* y */][!b->intra]` (vp9block.c) — but
our index math assumed **[tx][intra/inter][plane]**. The C initializer's
comments ("block Type 0", "Intra") are actively misleading. Intra-Y
coordinates (0,0) happen to coincide, which is why keyframe luma ever worked
at all; **intra-UV was silently reading the INTER-Y table**. Fixed in all
three index functions (`coef_model_idx` / `coef_full_idx` / `Counts::coef_bin`):
`(tx*4 + pt*2 + bt)`.

**Results of the fix + session #v2's left-ctx reset:**
- **solid_black_lossless 16/32/48/64/96: ALL 99/99/99 — including the 96x96
  2-SB-row clip** (the #v2 left-context reset was found via size bisection:
  16/32/48/64 exact, 96 failing from luma row 64 = SB row 1 exactly).
- **solid_black_lossy: 99/99/99** (was Y=99 chroma approx 50).
- solid64_lossy gray: 99/99/99.
- All 6 micro clips: still exact.
- Content clips (testsrc/smptebars at 96-256px) remain 7-12 dB: the
  coefficient walk still desyncs on them (consumes about a third of the tile
  bits — reading many cheap EOB/zero symbols where true values exist). Solid
  clips never exercise ZERO tokens or multiple values per sub-block; that is
  the remaining suspect space. The walk structure was re-verified
  line-by-line against FFmpeg (skip_eob semantics, band advance placement,
  cache/nnz ordering) with no discrepancy found — the next suspect is the
  ZERO-run behavior across band boundaries or an EOB-count adaptation detail.

**Also this session:** u_bad instrumentation in the conformance harness
(removed); size-bisection helper for solid lossless clips (kept in the
test); verified `set_tile_offset` + partition None/SPLIT forced paths at
16x16.

**#v3 addendum — the replay-tool lesson + block-coverage finding.**
1. A (prob,bit) re-encode replay test is VACUOUS for sync checking: a range
   decoder's recorded symbol sequence ALWAYS re-encodes to the original bytes
   by construction (the decoder reads the unique bit sequence that reproduces
   the code). Don't rebuild that tool.
2. The useful probe was PER-BLOCK trace + COVERAGE: intra128 (128x128,
   content) decoded only **43 blocks** ending at tile bit 2612 = byte 327 of
   1982, with MI cols 13-15 and row 15 never touched — the partition walk
   desyncs early and "covers" the frame with false large partitions. The
   first blocks (16x16/8x8, top-left) look plausible for testsrc; the first
   wrong read is at or before the second-sub-block EOB (p=84 -> read EOB,
   33% branch, where true content blocks need not-EOB).
3. Since ALL pre-token reads were verified semantically forced and the
   coefficient walk matches FFmpeg structurally, the remaining suspects are
   (a) an ABOVE-CONTEXT value feeding a prob row that solid clips never
   vary (e.g. above_partition_ctx propagation across SB columns), or (b)
   the probability ADAPTATION state (refreshctx) — single-frame clips still
   read frame_context_idx probabilities; confirm our frame_ctxs[c] seed for
   content clips equals FFmpeg's at tile start (add a hash of the working
   prob set and compare against a libvpx/FFmpeg instrumented dump).
4. The definitive oracle remains an instrumented libvpx build (patch
   vp9_decodeframe/vp9block to dump per-symbol band/ctx/prob/bit), which is
   a clean next-session task: libvpx builds standalone in minutes.

**Debug tooling built this session (removed from the tree, recreate as
needed):** env-gated read/coef/block traces (`TPT_VP9_READS/COEF/TRACE`) —
the read-level trace plus a 30-line python bool-decoder reimplementation
(`crosscheck.py` pattern) pinned the pass-B bug in one evening; a
`dbg_micro_dump` test printing our raw Y/U rows for one IVF. The
`bits_consumed()` accessor on `BoolDecoder` is worth RE-ADDING when debugging
resumes: comparing consumed-vs-tile-size per sub-block found the desync
point immediately.

**Next steps (revised priority).**
1. solid_black_lossless: still desyncs INSIDE the coefficient tokens (val
   448 CAT6 read verified table-exact; every pre-token read verified
   semantically forced). Next probe: dump per-sub-block consumed bits for
   sub-blocks 0-3 and compare against the tile-byte budget (13 bytes cannot
   hold 16 CAT6 tokens — so the TRUE first token is smaller than ours, i.e.
   the desync is at or before the tp[3] cat-branch read).
2. Chroma on lossy clips (U/V ≈ 50 dB while Y = 99): check uvtx derivation
   and the chroma nnz ctx stride for 4:2:0.
3. Then scale the ladder: 32x32/64x64 blocks, tx 8/16/32, multi-block
   testsrc, then inter frames.
4. NOTE: `ffmpeg -c:v libvpx-vp9` output is NOT run-to-run deterministic
   (encodes with `-cpu-used 4` produce different tiles per invocation) —
   conformance clips must be written once and reused, or traces must be
   captured in the same process as the decode.

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

---

## Session #v4 — oracle-driven debugging (libvpx instrumented build)

**Setup that now exists (keep for future sessions).**
- Instrumented libvpx at `C:/Users/phill/AppData/Local/Temp/libvpx2/`
  (BSD license; provenance-only extraction — no code copied into the crate).
  - `vpxsym.exe <clip.ivf> [out.yuv]`, env `VP9SYM=1` emits, per decoded
    symbol: `SYMP p=<prob> bit=<bit> b=<bits consumed> range=<range>` for every
    bool read (both the detokenizer's local `read_bool` and `vpx_read` in
    bitreader.h — i.e. mode/partition reads too), plus `EOBCHK` per coefficient
    position (tx/type/band/ctx/prob/outcome/pos/raster/neighbors/full 11-prob
    row), `TOK` per decoded token, `CP` (adapted coef table after the
    compressed header) and `TILEOFF` (first 32 tile bytes) once per frame.
  - `gcc harness.c libvpx.a -o vpxsym.exe` rebuilds the harness after `make`.
  - Also dumps decoded YUV (argv[2]) — **verified ffmpeg == libvpx pixel-exact
    on intra128**, so both references agree and either is a valid target.
- Ours: `cargo run -p tpt-kinetix-vp9 --example dbg_trace -- <clip.ivf>` with
  `TPT_VP9_COEF=1` emits the same `SYMP`/`EOBCHK`/`TOK`/`CP`/`TILEOFF` lines
  (note: libvpx counts `b` from its own tile-start marker; our compressed
  header + tile are separate `BoolDecoder`s — filter to post-`TILEOFF` lines
  and compare `(p, bit, range)`; do NOT compare the `v=` window fields, the
  64-bit vs 16-bit residual windows are representationally different).
- Clips: pin bytes once (libvpx encode is nondeterministic); the intra128 clip
  lives at `/tmp/vp9dbg.ivf` (copy of `tpt_vp9_conf_ref_intra128.ivf`).

**Bugs found & fixed this session (verified via exact symbol-stream equality
against the oracle over thousands of symbols).**
1. Compressed-header coefficient updates: the reference streams updates as
   `[plane][intra/inter]` per tx size; we indexed our `(bt, pt)`-parameterized
   layout with the loops swapped, transposing every adapted table for any
   stream carrying coef updates. Fixed in `header.rs` (`coef_model_idx(i, k, j, l, m)`).
   Verified: `CP` dump now diffs **0 lines** vs libvpx.
2. Scan/neighbor tables: FFmpeg's `ff_vp9_*_scan_*` tables are the **transpose**
   of libvpx's (FFmpeg's inverse-transform pass consumes the block transposed;
   ours follows the spec/libvpx convention). All 20 scan + NB tables regenerated
   from libvpx `vp9_scan.c` (commit 5e680f3..., provenance markers updated), and
   `scans()` now maps tx_type 1 (ADST_DCT) → ROW, 2 (DCT_ADST) → COL per
   libvpx `vp9_scan_orders`.
3. `INTRA_TXFM_TYPE` was laid out in FFmpeg enum order but indexed by spec mode
   values — every intra block picked the wrong transform/scan family. Now
   spec-ordered `[0,1,2,0,3,1,2,2,1,3]`.
4. Nonzero-context off-by-one: the next-position context was computed with the
   *previous* scan index (correct only against FFmpeg's row-shifted NB tables).
   Now `i += 1` happens before the `nb[2*i]` lookup, matching libvpx's
   `get_coef_context(nb, cache, c)`.
5. 16x16/32x32 always use the default scan (tx_type clamp `tx < 2` in
   `frame_recon.rs`) — matches libvpx `vp9_scan_orders` filling those rows with
   the default order.

**Verified-equal infrastructure (do not re-suspect).** Bool decoder (16-bit
window) is symbol-exact vs libvpx including 0-bit renorm reads; default coef
tables (all 1584), MODEL_PARETO8 (2048), BAND_COUNTS, the token tree, the
prob-update subexp (`update_prob`), band advancement, and the coefficient walk
itself are all bit-exact vs the oracle on the common prefix.

**Current state.** Solid/micro clips: 11/13 at 99 dB (unchanged). Content
clips: still desync — the unified symbol stream now agrees for the compressed
header + first ~2905 tile symbols (b≈0..978) and first breaks at a 4x4 block
whose sub-block scan the oracle picks as ROW (mode 8 = D67) while we pick COL
(mode 7 = D203) for the *same* partition slot. Everything up to that point,
including all prior 4x4 mode reads, is bit-identical, so the divergence is in
the *kf intra mode context* feeding that specific sub-block read (suspect:
left/above context slot granularity for sub-8x8 partitions, or the leaf-mapping
of one of the mode-tree walks under a stale context). The `KFMI` instrumentation
(libvpx `vp9_decodemv.c` + our `frame_mode.rs`) prints per-sub-block read rows
(`p0`, `p7`) — diff those to pinpoint the first context divergence.
NOTE: FFmpeg's `BlockSize` enum is size-ordered (64x64=0 … 4x4=12) — `bs > 9`
means "smaller than 8x8"; our constants mirror that.

**Next steps (priority order).**
1. Diff `KFMI` streams (ignore `blk=`; libvpx only prints for 4x4 blocks) to
   find the first (above,left) context mismatch; fix the context storage.
2. Re-diff the unified stream to the end of the frame; expect residual issues
   only in prediction (arms 7/8 semantics) and the loop filter.
3. Then: inter frames (the 4-frame clip), loop filter on, strip debug
   instrumentation (`TPT_VP9_COEF` in booldec/coef/frame_recon/decoder,
   `KFMI`, the `dbg_trace` example), full `just check`, and flip
   `capabilities().pixel_exact`.
## Session #v4 continued — bit decode now 100% symbol-exact

Running the oracle-vs-ours methodology to ground produced five more fixes:

6. **`INTRAMODE_TREE` leaves (D203/D67 swapped).** The reference tree reads
   `'111110' -> D63(8)` and `'111111x' -> D153(6) / D207(7)`; our copy had
   `-7 / [-6, -8]`. Fixed to `[-8, 8] / [-6, -7]` in `tables.rs`. (The
   FFmpeg-enum leaf names VERT_LEFT/HOR_UP map to spec D67/D203 at positions
   7/8 — a plain identity translation of the leaves is wrong.)
7. **`SPEC_TO_FFMPEG_MODE` = `[2,0,1,3,4,5,6,8,7,9]`.** The kf y/uv mode
   probability tables are FFmpeg-enum-ordered; the identity mapping beyond
   D45 swapped the last two angular rows. (An earlier "fix" and revert of
   this same line were both wrong; the tree-leaf bug masked it.)
8. **Directional scans apply to 16x16 too.** libvpx derives
   `tx_type = intra_mode_to_tx_type_lookup[mode]` for ALL intra luma tx
   sizes and indexes `vp9_scan_orders`, which has real row/col entries for
   TX_16X16 (only TX_32X32 is all-default). Our `tx < 2` clamp became
   `tx < 3`.
9. **Inverse-transform pass order.** The reference transforms ROW vectors
   first, then columns, with rounding between passes. Our pass A ran over
   columns. Fixed in `transform.rs` (pass A = rows; a_kind/b_kind comments
   updated — the original a/b mapping was correct after all).
10. **dqcoeff clearing.** libvpx clears dqcoeff around every block; our
    scratch buffer is reused, so stale coefficients past the EOB polluted
    the transform. `decode_coeffs_b` now zeroes the slice on entry.

11. **d207/d63 predictors rewritten** as faithful ports of the reference
    generic bodies (left[] bottom-up mapping documented in the code).

Result: the unified SYMP/EOBCHK/TOK stream matches libvpx for **all 25226
lines** of the intra128 frame, `lossless96x64` is now pixel-exact (99 dB),
and every content clip improved (odd 9.3->16.5, tiled 11.3->15.6, inter
11.4->13.9, testsrc_lossless 8.7->12.2). Remaining: 12/13... clips exact
except the five content clips, whose first residual difference now traces
to prediction-edge propagation inside sub-8x8 partitions (the (0,4)/(4,0)
recon-order vs above-right edge availability question). The comparison
tooling (16-value COEF/PSTR dumps on both sides) is in place to finish it.
## Session #v4 final addendum — have_right + edge semantics

11. **`have_right` was never honored.** Our edge gather extended the above
    row with real frame pixels up to `n + 4` unconditionally; the reference
    replicates the last pixel past the block width unless the right
    neighbour sub-block exists (`(aoff + txw) < bw`). `gather_intra_edges`
    now takes `have_right` (Y: `x*4 + (4 << tx) < bw4 * 8`; UV mirrors it
    with the uvtx width) and only fills the above-right half from the frame
    when it is true. The D67/D203 arms read the full 2n edge row.

State at end of session: bit decode symbol-exact end-to-end; 7/13 conformance
clips pixel-exact at 99 dB (all micro/solid clips + `lossless96x64`, the
first content clip to pass); the other clips improved to 15-20 dB
(intra128 10.9->16.6, testsrc_lossless 12.3->20.0, odd125x67 16.5->19.2,
tiled 15.6->16.3, inter 13.9->14.9).

**Session #v4b addendum (D117/D153/D45 rewrites + mode-context hunt).**
After the d207/d63 fixes, three more predictor bugs were found by full-stream
PRED/PSTR comparison and fixed:
- **D117 (spec 5) rewritten** as a faithful port of the reference
  d117_predictor (first row = AVG2(tl-shifted above), second row = AVG3
  above-window, first column = AVG3 down the left edge, then the
  copy-up-left propagation). The old arm had wrong orientations.
- **D153 (spec 6) rewritten** as a faithful port of d153_predictor (first
  two columns from left/above, then copy-down-left propagation). The old
  arm read edges in the wrong direction.
- **D45 4x4 corner fixed**: the reference vpx_d45_predictor_4x4_c reads the
  REAL above-right pixels up to above[7] and the corner px(3,3) = above[7]
  (not above[6] as the old arm had). n=4 uses the unrolled form; n>4 pads
  with above[n-1] per the reference generic.
Also: `have_right` was extended to chroma (`x*4 + (4 << uvtx) < bw4*4`).

**Next session (precise).** The remaining divergence is the per-4px MODE
CONTEXT for split-8x8 partitions: ours decoded (0,4) LL sub-block mode 7
(D203) where libvpx decoded mode 8 (D63), because the (above, left) mode
context fed to `kf_ymode_probs_for` differed at that sub-block. Compare the
`above_mode_ctx`/`left.mode` 4px-slot writes between FFmpeg's
`above_partition_ctx`-style tracking (vp9block.c reads `a[0] = a_mode_ctx[col*2]`
etc.) and ours for split-8x8 partitions — suspect the write-back slot mapping
(`col * 2` vs 4px col) or the bottom-row write-back (l0/l1 slots). mid-session a bad sed
truncated `frame_recon.rs` (recovered from git; the tx<3 clamp, have_right
args and PRED/PSTR prints were reapplied) and a speculative full rewrite of
the directional arms regressed pixels (reverted; predict.rs restored from
git + normalized to LF). Final applied state in `predict.rs`: gather takes
`have_right` (above-right half = real frame pixels only if true, else
replicate; fill extended to 2n), the D45 4x4 exact-form stride bug fixed
(`r * 4` -> `r * stride`), and modes 7/8 swapped onto the faithful
d207/d63 ports. Verified end state: intra128 recon mismatches down to **2
blocks** (a 4x4 D67 whose column 3 reads a stale above-right pixel, and a
4x4 D135 whose (3,3) corner pixel differs — both ADST-transformed blocks);
PSNRs above.

**Next session (precise).** The first prediction divergence in intra128 is
luma block #19: a 4x4 D67 (spec mode 8) at intra-block (x=0, y=4), whose
pixel (0,3) = AVG2(above[3], above[4]) reads 16 (ours) vs 124 (libvpx) —
i.e. our above-right pixel at (py+3, px+4) differs. That pixel belongs to
the upper-right 4x4's bottom row; both sides predict (4,0) before (0,4), and
both (0,0) coefficient blocks are identical, so compare (a) the (4,0)
block's bottom-row reconstruction and (b) the exact `have_right` value each
side used for the (0,4) block (libvpx: `(aoff + txw) < bw` with bw = the
partition width in 4x4 units — verify our `x*4 + (4 << tx) < bw4 * 8`
matches per partition type). The dumps to use: PRED (16 px), PSTR (16 recon
px + 16 coeffs, both sides, 140 aligned eob>0 luma blocks) — beware the
field offsets: ours n[3:19]=px, n[19:35]=c; oracle n[3:19]=px (c always 0,
printed post-transform).


---

## Session 2026-09-19/20 — recon bit-exact vs libvpx (pre-LF); 12/17 clips pixel-exact vs ffmpeg

**Headline.** All five content clips jumped from 21-25 dB to 62-70 dB;
`testsrc_lossless` is now pixel-exact (99 dB). Full conformance: 13/13 pass,
12 clips at 99 dB. Remaining: intra128 67.7, inter128x96 70.4, tiled256x144
70.1, odd125x67 62.5 — all in the loop filter now (see below).

**Root causes found and fixed this session (in order):**

1. **Two-pass theory disproved.** libvpx `predict_and_reconstruct_intra_block`
   interleaves predict→tokens→recon per sub-block exactly like ours. The old
   "libvpx predicts all sub-blocks first" note in this file was wrong (it came
   from misreading a stale-buffer EDGE print; see 2).
2. **Oracle instrumentation traps.** The oracle's `KFMI mode=` field prints the
   STALE pre-decode value (real mode is in `KFM2`); `EDGE above=` prints the
   above_row buffer that the 4x4+hr+la direct-frame path BYPASSES (contents are
   stale garbage); `PRED px=` prints 16 LINEAR pixels (row 0 only, incl.
   neighbour pixels) not a 4x4. And crucially: the harness's `SKIP_LF` was
   inside the init-**failure** branch, so every "pre-LF" oracle dump ever taken
   was actually post-LF. After fixing, pre-LF YUVs are bit-identical.
3. **Above-right availability (predict.rs gather):** real above-right pixels
   are only read for **4x4** blocks (`bs == 4 && right_available`); 8x8/16x16
   always replicate `above[n-1]`. We copied real pixels for all sizes.
4. **Directional predictors are NOT one generic family:** libvpx compiles
   `intra_pred_no_4x4(d207/d63/d45/d117/d135/d153)` — hand-unrolled 4x4s that
   DIFFER from the generic (which is used only for n>4):
   - d63 4x4 unrolled reads real above-right (E,F,G): rows 2-3 are
     AVG2/AVG3 of above shifted by one, NOT memcpy-shifted.
   - d117 4x4: DST(1,0)=AVG2(A,B) (generic has AVG3(X,A,B)); several cells.
   - d153 4x4: DST(2,1)/(2,2) follow the left AVG2 sequence, not the copy.
   - d135 4x4 unrolled ≡ generic; d207 4x4 unrolled ≡ generic **plus the
     interior copy loop** (`dst[r][c+2] = dst[r+1][c]`, rows bottom-up) that we
     previously MISSED ENTIRELY (rows 0..n-2 cols 2+ were never written).
   - d45 generic (n>4): rows are row0 shifted right by r (we copied row0[0..]).
   - d63 generic (n>4): rows r/r+1 = rows 0/1 advanced by (r>>1) pixels
     (`memcpy(dst + r*stride, dst + (r>>1), size)`), padded with above[bs-1].
5. **DC-only substitution removed.** libvpx substitutes ONLY DC_PRED
   (dc_pred[left][up] tables); every other mode runs on the 127/129-filled
   border. We substituted V/H/D45/D135/D117/D153/D67/D203/TM — FFmpeg
   behaviour, not libvpx. needs_top/needs_left now key off the TRUE mode
   (extend_modes) with substitution applied afterwards.
6. **iadst8_1d rewritten as a literal port** of libvpx `iadst8_c`: its input
   permutation (x0=in[7], x1=in[0], x2=in[5], x3=in[2], ...), per-stage
   rounding, cospi_8/24 on the stage-2 s6/s7 terms (we had them swapped), and
   the output mapping out[1]=-x4, out[2]=x6, out[5]=-x7, out[6]=x5.
   Verified bit-exact vs `iadst8_c` probes.
7. **iadst16_1d rewritten** as a literal port of `iadst16_c` (same story;
   stage-4 output mapping out[1]=-x8, out[2]=x12, out[3]=-x4, out[13]=-x13,
   out[15]=-x1). Probes match `iadst16_c` exactly.
8. **Loop-filter driver rewritten** (`loop_filter.rs`): the old FFmpeg-style
   8px-segment/class masks were structurally wrong. Now a faithful port of
   `vp9_setup_mask` (partition-hierarchy walk over the per-unit grid),
   `build_masks`/`build_y_mask`, `vp9_adjust_mask`,
   `filter_selectively_vert_row2` / `_horiz`, and the ss00/ss11 plane drivers.
   Per-unit (bs, tx, uvtx, skip_inter, lvl) recorded in `record_filter_edges`
   — note units must be recorded EVEN WHEN lvl==0 (the walk needs sb_type).

**Method that cracked it:** full-coverage per-block traces on both sides
(EDGE/PREDP/BLK/COEF/PSTR with identical formats), normalize semantic fields,
diff line-by-line; C probes (`iht_probe.c`, `iadst8_probe.c`, `txfm_probe.c`
in the libvpx tree) to test single transforms against the oracle .a; LF ops
compared as (k,row,col,wd) using the per-line pitch. Beware: `make` in the
libvpx tree fails silently after some edits (run `make 2>&1 | tail`), the
oracle file redirects lose buffered output (use pipes), and the vp9 source
has INACTIVE duplicate functions (4-arg ss00 is dead code; the live one takes
3 args) — check `nm *.o` after every oracle edit.

**Remaining (precise).** Loop-filter mask-class details; recon is bit-exact
pre-LF on vp9dbg.ivf, so all remaining diffs are LF-only:
- ours-only H wd16 at luma rows 16/32 (cols 32-72 @16) — we mark
  above_y[TX_16X16] bits the oracle doesn't (16x16-TX blocks at unit row 2/4,
  cols 4-9: oracle emits NO edge there at all — verify per-unit bs/tx vs
  oracle mi grid via its BLK lines).
- oracle-only V wd4 at 4px cols (36,44,52... row 0) — int_4x4_y bits we miss.
- chroma H rows 8/16 (oracle) vs 6/12 (ours) — uv row alignment in ss11.
Tooling to resume: oracle prints LFP k/wd/off/pitch (luma pitch=192, chroma
96) and LFM masks were attempted but the build fights instrumentation — the
working comparison is LFP (pitch==192) rows/cols vs ours (add pitch print
back temporarily). Ours emits ~844 raw ops vs oracle ~1180; common 358.

---

## Session 2026-09-20 (cont.) — inter decode fixed; 13/13 pass; LF ops bit-identical

**Root causes fixed:**

1. **Inter-mode tree leaf mapping (THE big inter bug).** libvpx
   `vp9_inter_mode_tree = { -ZEROMV, 2, -NEARESTMV, 4, -NEARMV, -NEWMV }` —
   leaves in order **ZEROMV, NEARESTMV, NEARMV, NEWMV** (NOT sequential
   NEARESTMV..NEWMV). We stored the raw leaf index (0..3) while every
   consumer compares against the mode constants 10..13 — so NEARMV/NEWMV
   blocks skipped their MV reads, seg-ZEROMV checks never fired, etc. Fixed
   by mapping leaves through `[ZEROMV, NEARESTMV, NEARMV, NEWMV]` at the
   three read sites. (`parse_inter_mode_info` and its sub-8x8 closure.)
2. **Inter-mode context.** The reference `get_mode_context` scans the two
   nearest candidates (above, left) weighting intra=9, NEAREST/NEAR=0,
   ZEROMV=3, NEW=1, then `counter_to_context[sum]` (0→2/BOTH_PREDICTED,
   1→3, 2→4, 3→1, 9+→5, 18→6). Out-of-frame neighbours are SKIPPED
   (weight 0) — NOT intra. Implemented as the precomputed 15×15
   `INTER_MODE_CTX_LUT` with index 14 = "no neighbour", the context arrays
   initialised/reset to 14 (`NO_NEIGHBOUR_MODE`), and inter neighbours
   stored as mode constants. The old 14×14 LUT was missing that row/col.
3. **mv_mode prob adaptation.** Counts per tree node are
   [c0, c1+c2+c3] / [c1, c2+c3] / [c2, c3] (was garbled).
4. **tx-size read order.** libvpx reads tx_size for INTER blocks AFTER the
   inter mode/MV info (`read_tx_size(…, !skip || !inter, …)`), only for
   intra blocks at the top. We read it before everything → whole-frame
   bool-decoder misalignment from the first inter block. Restructured
   `decode_mode` + `parse_tx_size`.
5. **UV_TXSIZE table was transposed** (tx axis reversed): correct rows e.g.
   64X64 = [0,1,2,3], 32X32 = [0,1,2,2], 16X8 = [0,0,1,1]. Was [3,2,1,0]-
   style garbage → wrong uv mask classes for inter frames.
6. **Loop-filter vertical int_4x4 edges** are applied 4 COLUMNS right of the
   segment (`vpx_lpf_vertical_4(ss + 4, …)`), we used +4*pitch (4 rows
   down) — this was the source of the "oracle-only V wd4" ops. After the
   fix the LF op stream (k, row, col, wd) is **bit-identical** to the
   oracle on vp9dbg.ivf: luma 468/468, chroma 176/176, zero diff.
7. **MC patch margins.** The 8-tap kernels read forward +0..+7 from each
   sample; edge MC patch/clamp windows now cover +7 (was +4/+5) and the 2-D
   tmp walk no longer reads past its buffer (the odd125x67/multitile
   panics).

**Oracle traps discovered:** `make` in the libvpx tree silently uses `cc`
which fails → stale .o; compile instrumentation manually with gcc and `ar r`
the single member. `vpx_lpf_horizontal_16(_dual)` dispatch is RTCD_EXTERN
(runtime function pointer, not a #define) — forcing C requires patching the
init lines in vpx_dsp_rtcd.h too. VP9 `vp9_inter_mode_tree` leaf order and
`dec_partition_plane_context`'s `bsl * PARTITION_PLOFFSET` level offset.

**State:** 13/13 conformance pass (odd_size & multitile no longer crash/fail
thresholds). Remaining non-exact clips: intra128 69.0, inter128x96 71.7
(V 32.7), tiled256x144 71.2 (V 46.4), odd125x67 64.5 (V 43.5) — all ±1-class
diffs now, mostly at frame/block edges in subpel-MC and LF paths. Env-gated
traces remain in place for the next session: TPT_VP9_TRACE (SYMP/BLK2/IMCTX/
PART/ISINTER/LFP/LFM2/EDGE/PREDP/PSTR), plus the oracle's LFP/LFM2/HSS00/
IMCTX/BLK2 prints (rebuild the oracle manually with gcc: `gcc -I. -c <file>`
+ `ar r libvpx_g.a <member>` + relink; make is unreliable there).

## Session addendum — MC centered-filter alignment + LF mask tables

- **THE remaining MC bug (huge):** libvpx's 8-tap convolution pre-offsets the
  source by -3 per filtered dimension (taps read sample-3 .. sample+4); we
  applied the taps forward (sample .. sample+7). Fixed in `mc_block`: sx0/sy0
  subtract 3 when the dimension is subpel-filtered, the patch covers
  (bw+8)x(bh+8) from (x-3,y-3), and the 2-D vertical pass indexes
  `t = r * tmp_stride` (the +3 was double-counted). After this the odd125x67
  inter frame is pre-LF **bit-exact** (0/16384 diffs; was 3774).
- **LF mask tables fixed for real:** LEFT_PREDICTION_MASK[1]/[3] were
  0x101 (should be 0x1010101 — 64X32/32X32 span 4 unit rows), [4] 0x1 →
  0x101 (8X16 spans 2 rows), ABOVE_PREDICTION_MASK[1] 0xf → 0xff,
  [4] 0x3 → 0xf, [7] 0x1 → 0x3. All six BLOCK_SIZES tables now verified
  programmatically against the reversed libvpx tables (see verify script in
  this session log). intra128 + odd125x67 LFM masks now match the oracle
  per-SB exactly for both frames.
- **LF status:** luma LF op stream bit-identical (468/468) on intra128;
  intra128 pre-LF bit-exact; post-LF 116 px remain — filter math re-verified
  line-by-line against vpx_dsp/loopfilter.c (filter4/filter8/filter16
  formulas and offsets all match; flat-8 writes at -3..+2 confirmed correct —
  an attempted "fix" to -2..+3 regressed and was reverted).
- **Chroma LF ops on intra128: oracle 548 vs ours 352.** The u/v op
  comparison needs the y→uv plane-base offset (oracle LFP offsets are
  y-buffer-relative); next session: set g_frame_base per plane in the oracle
  (ss00/ss11 entry: g_frame_base = plane base) so chroma ops compare directly,
  then fix the ss11/ss00 chroma mask or level derivation.
- **Oracle rebuild recipe (reliable):** `gcc -I. -c -o <obj> <src.c>` then
  `ar r libvpx_g.a <obj>` then `cp libvpx_g.a libvpx.a` then relink vpxsym.
  NEVER use make (its `cc` silently fails → stale .o). RTCD: lpf entries
  using `RTCD_EXTERN` (horizontal_16) need the init lines in vpx_dsp_rtcd.h
  patched as well.

## Session addendum 2 — chroma LF ops verified bit-identical; per-op instrumentation

- Oracle instrumentation upgraded: per-plane `g_frame_base` (ss00/ss11 entry,
  plane origin = dst->buf − SB offset using a stashed `g_sb_mi_col`), so the
  oracle's chroma LFP offsets are now plane-relative and directly comparable
  to ours. **Chroma LF ops on intra128: 176/176 exact, zero diff. Luma
  468/468 exact.** Level grid (lfl_y) verified equal (all 4s).
- Remaining intra128 post-LF diff: 116 luma px (±1) despite identical op sets,
  identical levels, identical pre-LF pixels. Multiset analysis shows the
  ORACLE applies certain H ops at rows 8/12 **twice as often** as we do
  ((H,8,c,4) ×4 oracle vs ×2 ours; (H,12,c,4) ×4 vs ×2 — the vert/horiz dual
  call structure in ss00's horizontal pass processes unit-row pairs and
  applies dual filters where we apply singles). Next: replicate the exact
  vert_row2 dual/single emission in our horiz pass (or accept: ±1 px).
- MC centered alignment fixed → **all tested clips pre-LF bit-exact** vs
  libvpx (odd125x67 both frames 0/16384).
- Current failing-clip PSNRs: intra128 69.0, inter128x96 71.7 (V 63.3),
  tiled256x144 71.2 (V 65.1), odd125x67 64.5 (V 60.7). 13/13 tests pass.
- Oracle instrumentation available: LFP k/wd/off/pitch(/lvl ours), LFM2 masks
  per SB, LFL lfl grids, BLK2/IMCTX/PART/ISINTER parse traces. Oracle rebuild:
  manual gcc + ar per object (make is unreliable), RTCD_EXTERN entries need
  vpx_dsp_rtcd.h init-line patches (horizontal_16 family).

## Session addendum 3 — remaining-diff isolation complete

- **intra128**: pre-LF bit-exact; LUMA LF ops 468/468 exact; CHROMA LF ops
  176/176 exact (after per-plane g_frame_base fix); lfl level grids equal.
  Yet post-LF: 116 luma px ±1. Multiset diff: oracle applies H wd4/wd8 ops at
  rows 8/12 (unit row 1 block+int edges) **×2** vs ours ×1 — the doubling
  survives all mask-level comparison (HMASK2 per-row masks are IDENTICAL).
  Hypothesis: the oracle's filter_selectively_horiz emits a dual at cols
  (2,3) twice because the 4px-int edges (m4i=0x1c at cols 2,3,4) interact
  with the m4 dual (m4=0x1c) via the count=2 advancement — trace sequence:
  (8,16),(8,24),(8,16),(8,24),(12,16),(12,24),(12,16),(12,24) i.e. m4-dual,
  m4-dual again, m4i-dual, m4i-dual again. Re-read
  filter_selectively_horiz's m4 branch with (mask_4x4&3)==3 and the s/lfl
  advancement for the double-dual pattern, then replicate in
  filter_selectively_horiz (frame_mode.rs loop_filter.rs).
- **odd125x67**: pre-LF bit-exact both frames. Post-LF frame-2: 218 px vs
  libvpx. LF op comparison (after per-plane g_frame_base) pending — the
  oracle LFP chroma offsets are now plane-relative (ss11 sets g_frame_base
  from dst->buf - mi_row*4*ustride - g_sb_mi_col*4), same for luma (ss00).
- **Chroma LF ops intra128 verified**: 176/176 exact. Y LF ops: 468/468.
- **intra128 clip = 1 frame, 1 LF pass** (ROWSRUN ×1) — the doubling is NOT
  a double-LF run.

## Session addendum 4 — filter-math verification complete

- The ±1 post-LF diffs (e.g. pixel (56,2): ours 40 vs libvpx 39) were traced
  to the V wd4 edge at col 56, row 2: inputs (p1=42, p0=39, q0=40, q1=41 —
  after the col-52 edge modifies col 52→43, col 53→41), hev=true, level 4.
  Hand-simulation of BOTH our code and libvpx's filter4 on these inputs gives
  **39** (f1v=1, oq0=40-1); our decoder produces 40 — meaning the edge ran
  with a different mask/level at runtime or the write landed elsewhere.
  Tooling now in place: `TPT_VP9_DBG56=1` dumps loop_filter_edge inputs for
  (row 2, col 56); `TPT_VP9_TRACE` dumps all LF ops with levels
  (LFP k/wd/off/pitch/lvl), lfl grids, per-SB masks, and parse symbols.
- **Do NOT "fix" the flat-8 offsets to -2..+3**: libvpx writes flat-8 at
  -3..+2 (op2=s-3*pitch confirmed from vpx_lpf_horizontal_8_c); an attempted
  shift regressed 116→209 and was reverted.
- Oracle chroma LF op comparison now works: per-plane g_frame_base
  (ss00/ss11 entry, g_sb_mi_col stashed from loop_filter_rows) → chroma ops
  176/176 exact on intra128.

## Session addendum 5 — debugging infrastructure

- `loop_filter_edge` now has `TPT_VP9_DBG56=1` gated per-row input dumps for
  the (row 0-7, col 56) edge: shows p3..p0|q0..q3 per step. Confirmed: our
  decoder DOES call loop_filter_edge for this edge but the eprintln! output
  goes to a different code path than expected (filter applies at row level
  but the final pixel value doesn't match hand-simulation).
- Oracle chroma LF ops now plane-relative: ss00/ss11 entry sets g_frame_base
  from dst->buf - SB_offset (using g_sb_mi_col global stashed per SB).
  Chroma LF ops: 176/176 exact on intra128.
- All LF mask tables programmatically verified against reversed libvpx
  source. All filter math verified line-by-line.
- The remaining ±1 px diffs (116 luma on intra128, 87 on odd125x67 frame 2,
  etc.) are concentrated at 4x4-internal boundaries (60/116) and 8px block
  edges. They require a per-op input/output diff at the loop_filter_edge
  level, which the current tooling supports but needs a fresh session.

## Session 2026-09-25 — HEV scalar fix; vertical-mask gap isolated

The earlier per-op conclusions above are superseded by a byte-level differential
against an untouched libvpx v1.17.0 tree with all VP9 loop-filter RTCD entries
forced to the scalar C kernels. The first concrete kernel bug was in
`filter4`'s HEV branch: after computing the combined signed delta
`f1 = clamp(p1 - q1) + 3 * (q0 - p0)`, the Rust port used the outer flatness
constant (`f = 1`) for `f1v` instead of the combined `f1`. Fixing
`(f1 + 4) >> 3` removed the bulk of the loop-filter error. All U/V planes and
the single-frame `intra128` vector are now byte-exact, and all 13 ffmpeg-gated
VP9 tests pass under the diagnostic gate.

The conformance harness itself had a reporting bug: it treated concatenated
output as planar-all-Y, then planar-all-U, then planar-all-V. It now splits each
frame into contiguous Y/U/V planes, accumulates PSNR across those planes, and
reports exact mismatch counts and first coordinates for every plane. The test
remains diagnostic rather than asserting all-zero counts until the remaining
luma gap is closed.

The remaining discrepancy is in **vertical mask construction**, not the scalar
kernel. For frame 1 of `odd125x67`, pre-filter output is byte-exact to libvpx,
and the luma horizontal masks and levels match. However, the first frame-1 luma
vertical entry differs: Kinetix's adjusted left masks are TX_16=`0x10`,
TX_8=`0xe`, TX_4=`0`, while the clean libvpx driver reports TX_16=`0x10101010`,
TX_8=`0`, TX_4=`0`. That vertical state causes the later horizontal x=32 pass
to receive different p-side taps and leaves one wrong luma sample. Corrected
full-plane counts are: `intra128` 0/0/0, `odd125x67` 1/0/0,
`tiled256x144` 25/0/0, and `inter128x96` 42/0/0.

**Next step:** compare the `SbUnit`/mask-building records for frame 1 against a
clean libvpx `MODE_INFO` trace, starting with the superblock containing
`mi=(8,4)`, where Kinetix records `BLOCK_32X16`, TX_16x16. Do not change the
scalar filter or flip `capabilities().pixel_exact` until all three failing
vectors have zero luma mismatches.

## Session 2026-09-26 — VP9 byte-exact end-to-end; pixel_exact flipped

**State.** All 13 conformance clips now decode byte-exact vs
`ffmpeg -c:v vp9` (y/u/v_bad = 0 on every frame). The conformance harness
asserts it, and `capabilities().pixel_exact` is flipped to `true` (the bar
this file set). The odd125x67/tiled256x144/inter128x96 luma gaps were TWO
independent bugs, neither in the mask walk:

1. **MC source-border clamp used the MI-aligned extent, not the crop extent**
   (`frame_recon.rs` `mc_luma`/`mc_chroma`). libvpx extends reference-frame
   borders from the *visible* (cropped) dimensions, so MC reads beyond the
   visible edge replicate the last visible row/column. We clamped at
   `mi_cols*8 / mi_rows*8` and read real reconstructed overhang content
   (rows 67-71 of a 67-tall frame). This fixed odd125x67 completely (its
   failing pixel was an mv=(0,0) copy of a row-67 sample). Change: pass
   `frame.width/height` (chroma: `(w+1)/2, (h+1)/2`) as `src_w/src_h`.
2. **Loop-filter ref/mode deltas were not carried between frames**
   (`header.rs` + `decoder.rs`). The deltas are persistent header state:
   libvpx keeps them in `cm->lf` and resets to the defaults only on key /
   error-resilient / intra-only frames. We started every frame from
   `Default` ([0,0,0,0]), so a non-key frame with `delta_updated == 0`
   derived per-block levels from zeros instead of the inherited deltas
   (ours lvl=7 vs libvpx 9/8 = base 7 + inherited +2/-1). Fix: pass the
   previous frame's `LoopFilterHeader` into `parse_uncompressed_header`
   and store the parsed state back after each frame.

This supersedes the "vertical mask construction" hypothesis in the previous
session note: the per-SB masks, levels grids and op streams were verified
**identical** to a cleanly rebuilt instrumented libvpx v1.17.0
(`harness_kd.exe` in `C:/Users/phill/AppData/Local/Temp/libvpx2/`; per-kernel
`KI/KO wd.. off=..` dumps of every luma filter call, matched positionally
against ours via `TPT_VP9_OPS=1`). The last SB-row "mask divergence" recorded
earlier was an artifact of comparing a stale instrumented exe against a
mislabelled frame. Debug tooling added this session (env-gated, keep):
`TPT_VP9_OPS` (per-op input/output hex dump of every `loop_filter_edge`
call), `TPT_VP9_BUF` / `TPT_VP9_BUF_POST` (raw strided-buffer row dumps
before/after the LF, `y0:y1:x0:x1`), and `dbg_trace` now writes per-frame
YUV files (`out.fN`) for frame-by-frame diffs.

**Oracle rebuild recipe that finally worked (for next time):** edit
`vp9/common/vp9_loopfilter.c` (driver prints: `STORED` pre-adjust masks,
`OBUF`/`OPOST` buffer dumps, per-plane kernel-dump gate
`g_lpf_dump_plane`) and/or `vpx_dsp/loopfilter.c` (KI/KO dumps inside the
six scalar kernels; note `mb_lpf_*_edge_w` steps = `count` for vertical,
`8*count` for horizontal, and `g_lpf_frame_base`-relative offsets need
`extern`), then
`gcc -I. -fno-common -m64 -O2 -c <file.c> -o <obj>`, copy the object over
the archive member name (`loopfilter.c.o` / `vp9_loopfilter.c.o`) —
`ar r libvpx.a <member>` — delete stale members (`loopfilter.o`,
`loopfilter_trace.o`) which otherwise shadow yours, and relink
`gcc -I. -O2 harness.c -o harness_kd.exe libvpx.a`. Verify the rebuilt
oracle still decodes the clip byte-exact vs ffmpeg BEFORE trusting its
dumps, and remember the file is CRLF (patch scripts must preserve it).

**Remaining (from the old list, now unblocked):**
1. ~~Wire VP9 into `tpt-kinetix-pipeline`/CLI decode paths~~ **Done
   (2026-09-26):** new `codec-vp9` pipeline feature (on by default,
   royalty-free) with `Vp9DecodeStage`; the CLI `probe` reports VP9 decoder
   capabilities and `transcode --vcodec av1` dispatches on the probed input
   codec — VP9 input takes the `Vp9DecodeStage` path, H.264 input the
   existing `codec-h264` path. An ffmpeg-gated pipeline test
   (`tpt-kinetix-pipeline/tests/vp9_pipeline.rs`) covers VP9-in-MP4 end to
   end. Two pre-existing CLI rough edges noticed on the way (not VP9):
   rav1e's AV1 IVF output is not dav1d-decodable yet, and the H.264-input
   transcode path produced "no packets" on the test inputs.
2. Optional hardening: fuzz the superframe splitter and odd-size paths
   (CI `fuzz-check` compiles; local fuzzing still lacks the ASAN runtime).
3. Profile-1/4:4:4 and 10/12-bit remain out of scope (rejected in strict
   mode with `KinetixError::Unsupported`).
