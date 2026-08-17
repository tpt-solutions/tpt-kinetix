# TPT Kinetix — AV1 Decoder Todo

> Active work. See [todo.md](todo.md) for the project index.

### AV1 — from-scratch reconstruction

> Status corrected 2026-08-06 (previous wording was stale): `decoder.rs`
> already calls into `reconstruct.rs::reconstruct_av1_frame` for intra
> keyframes, and `reconstruct.rs` has real inverse-transform (`dct_4x4`,
> `dct_8x8`, `dct_16x16`, `adst_4x4`, `wht_4x4`) and intra-prediction
> (DC/vertical/horizontal/paeth/smooth/directional) code — this is
> substantially more than "grey placeholder frames." **However**,
> `decode_tile_group` (`reconstruct.rs:664`) previously read coefficients with a
> plain `BitReader` using an invented exp-golomb-like scheme (trailing-ones +
> level-prefix/suffix, modeled on H.264 CAVLC). Phase A landed the symbol
> decoder engine in `entropy.rs`/`entropy_cdf.rs`, and **Phase B (2026-08-09)
> rewired `decode_tile_group`/`decode_chroma_tx` onto real `coeffs()` syntax
> (`coeff.rs::read_coeffs`)** read via that symbol decoder — the reconstruction
> math (`inverse_transform`/`predict_intra_block`) is unchanged, and the
> lossless WHT path is now wired. Intra keyframe coefficients now decode through
> the real arithmetic decoder (cross-checked against an independent Python
> `coeffs()` oracle); output is still not pixel-exact because inter prediction
> (Phase E) is not yet implemented — the in-loop post-filters (deblock + CDEF +
> restoration passthrough, Phase D) are now wired and run after tile-group
> reconstruction. **Phase C (superblock partition tree + per-block intra mode /
> `tx_size` syntax) is now done** — the "fixed placeholder grid" gap is closed.
>
> **Bitstream-ingest status (2026-08-12)**: the OBU splitter
> (`obu::parse_obu_sequence`) and **Sequence Header OBU parser
> (`obu::SequenceHeaderObu::parse`) now decode correctly against real
> `ffmpeg`-generated keyframes** — verified by the new ffmpeg-gated
> conformance test `tpt-kinetix-test-utils/tests/conformance.rs::
> av1_vs_ffmpeg_reference_when_available`, which asserts the declared
> frame geometry (`128x96`) matches. The earlier truncation at
> `matrix_coefficients` was a missing `frame_id_numbers_present_flag` /
> `enable_*` feature-flag block in the non-reduced Sequence Header path
> (§5.5.2), now filled in; `read_ns` was also hardened against `n<=1`.
> The **uncompressed frame-header parser (`frame.rs::FrameHeader::parse`,
> AV1 §5.9) still drifts on real keyframes** (it produces wrong
> dimensions), so the decoder cannot yet reconstruct real frames — that
> parser, then the superblock partition tree + per-block intra mode + tx
> size (Phase C), then CDEF + deblock + restoration (Phase D), are the
> remaining blockers to a pixel-exact intra decode. The conformance
> harness already prints the Kinetix-vs-reference PSNR/diff so progress on
> those phases is measurable.

#### AV1 Phase A — real entropy: the symbol decoder (blocker for everything below)

> Done 2026-08-07. Landed in `tpt-kinetix-av1/src/entropy.rs`
> (`SymbolDecoder`: `init_symbol`/`read_symbol`/`read_bool`/`read_literal`,
> spec §8.2.2/8.2.3/8.2.5/8.2.6 — not §8.2.2/§8.2.4 as originally cited
> above, which are actually "Initialization" and "Exit process"
> respectively) and `entropy_cdf.rs` (mechanically extracted `Default_*_Cdf`
> tables for `txb_skip`/`cbf`, `tx_type` (intra sets 1/2, inter sets 1/2/3,
> full), and coefficient levels (`eob_pt_*`, `coeff_base_eob`,
> `coeff_base`, `coeff_br`, `dc_sign`, full)). The AV1 spec has no literal
> worked numeric example for the symbol decoder (confirmed by fetching
> `09.parsing.process.md` directly) — substituted an independent Python
> transcription of the same §8.2 pseudocode as a differential oracle;
> golden vectors from it are embedded in `entropy.rs`'s tests. `exit_symbol`
> (§8.2.4) is intentionally not implemented yet — it needs per-tile
> bookkeeping (`context_update_tile_id`, the full named `Tile*`/`Saved*`
> CDF set) that only exists once Phase B/C wire up real tile parsing.
> `decode_tile_group` itself is untouched — that rewiring is Phase B.

- [x] Implement the AV1 boolean/multi-symbol arithmetic decoder (§8.2.6,
      `read_symbol`) operating over an adaptive CDF table, distinct from both
      H.264 CABAC and the current ad hoc `BitReader` usage in
      `decode_tile_group`
- [x] Implement default CDF tables + adaptation/update rule (§8.2.6) for at
      minimum the symbols `decode_tile_group` currently reads with raw bits
      (coefficient levels, `cbf`, `tx_type`)
- [x] Unit test the symbol decoder against the spec's worked arithmetic-coding
      example, independent of any real tile bitstream — see note above on
      why this is a cross-validated synthetic vector set rather than a
      literal spec example

#### AV1 Phase B — rewire coefficient decode onto the symbol decoder
- [x] Replace the `BitReader`-based level/trailing-ones/total-zeros decode in
      `decode_tile_group` (`reconstruct.rs:664`) and `decode_chroma_tx`
      (`reconstruct.rs:902`) with real coefficient syntax (`all_zero`,
      `eob_pt*`, `eob_extra`, `coeff_base(_eob)`, `coeff_br`, `dc_sign`,
      sign, Exp-Golomb tail) read via the Phase A symbol decoder (§5.11.39),
      in `coeff.rs::read_coeffs` — landed and cross-checked against an
      independent Python `coeffs()` oracle for golden vectors
- [x] Keep the existing `inverse_transform`/`predict_intra_block` reconstruction
      math — only the bits-in path changes (also wired the previously-dead
      `wht_4x4` lossless WHT via `internal_tx_type`/`TX_TYPE_WHT`)

#### AV1 Phase C — partition + mode syntax — **DONE (2026-08-13)**

> `reconstruct.rs` now walks the real superblock partition tree: `decode_tile_group`
> calls `decode_superblock` → `decode_partition` (recursive §5.11.4 walk with the
> neighbour-context partition CDFs), and each leaf block reads its per-block
> `intra_y_mode` (`read_intra_y_mode`), `uv_mode` (`read_uv_mode`), and `tx_size`
> (`read_tx_size` → `read_selected_tx_size`) via the symbol decoder. The fixed
> 8×8-per-superblock placeholder loop in `decode_tile_group` is replaced. The
> reconstruction math (`inverse_transform`/`predict_intra_block`) is unchanged
> from Phase B. Output is still **not** pixel-exact because the loop filter /
> CDEF / loop restoration (Phase D) and inter prediction (Phase E) are not yet
> implemented — so this resolves the "placeholder grid" gap noted below, not the
> overall pixel-exactness gap.

- [x] Parse superblock partition tree (§5.11.4) instead of the current fixed
      8×8-block-per-superblock loop in `decode_tile_group`
      (`reconstruct.rs:697`) — `decode_superblock`/`decode_partition` land it
- [x] Parse per-block intra mode / `tx_size` selection via the symbol decoder
      rather than assuming a fixed DC-predicted 8×8 transform block

#### AV1 Phase D — loop filter / CDEF / restoration — **WIRED (2026-08-14)**
- [x] Implement the deblocking loop filter (§7.14)
- [x] Implement CDEF (§7.15)
- [x] Implement loop restoration (§7.17) — no-op passthrough (spec-permitted
       when `enable_restoration` is false)

  **Done (2026-08-14):** `tpt-kinetix-av1/src/loop_filter.rs` (deblocking
  loop filter §7.14 + CDEF §7.15 from the normative algorithms, `apply_loop_restoration`
  no-op passthrough) is now declared (`pub mod loop_filter` in `lib.rs`) and
  invoked. `reconstruct.rs::reconstruct_av1_frame` runs `apply_post_filters`
  (deblock → CDEF → restoration) over each tile's reconstructed buffer after
  tile-group decode; per-8×8-block tx-size / skip metadata is collected during
  `decode_block` into a `FrameMeta` (threaded through `TileDecodeState` →
  `decode_tile_group`) and consumed by the filters. The CDEF strength packing
  was corrected (`pri = packed & 0x0F`, `sec = packed & 0x30`) and the
  variance-dependent strength clamp fixed. `Av1Decoder::capabilities()` reports
  `supports_deblocking = true`. Pixel-exact decode still awaits inter
  prediction (Phase E) + conformance (Phase G); `pixel_exact` stays `false`.

#### AV1 Phase E — inter prediction
- [x] Reference frame buffer management (§7.20, `RefFrameStore` equivalent) —
       `Av1Decoder` now holds a `RefFrameStore` of 8 slots; every reconstructed
       frame is stored into the slots selected by `refresh_frame_flags` (§7.20) in
       `decoder.rs::decode`. `ref_frames()` exposes it; `StoredFrame` carries the
       planar YUV. This is the storage inter prediction will read from once motion
       compensation lands (the reconstruction pipeline still returns `Ok(None)` for
       non-intra frames, so inter decode is not yet enabled).
- [ ] Motion vector prediction (§7.10) and inter block reconstruction (§7.11.3)

#### AV1 Phase F — parallel tile decode — **DONE (2026-08-14)**
- [x] Wire `rayon` over the tile groups now that Phase A–C produce real per-tile
       reconstruction. `reconstruct_av1_frame` computes each tile's superblock
       rectangle (uniform tile spacing from `tile_cols/tile_rows` + `tile_*_in_sb`),
       decodes every tile concurrently via `rayon`'s `par_iter` into a tile-local
       buffer (AV1 tiles are entropy-independent and write disjoint pixel
       rectangles), then blits the finished tiles back into the master planes.
       `decode_tile_group` now restricts its superblock walk to the tile's region
       (fixing a latent bug where every tile group re-decoded the whole frame),
       and writes at tile-local coordinates. Loop-filter passes (Phase D) run
       per-tile after reconstruction.

#### AV1 Phase G — conformance

> **Status update 2026-08-15.** Ran the existing dav1d/ffmpeg-gated corpus
> tests for the first time with `--nocapture` to actually read the PSNR
> numbers (previously only pass/fail on the geometry assertion was checked).
> Result: every entry, including trivial single-color intra keyframes, was
> decoding to ~9–17 dB PSNR — noise level, not "missing a feature." Root
> caused and fixed two real bugs in this session (both still leave the corpus
> far from pixel-exact — see below — but are necessary, not sufficient,
> fixes; do not re-introduce either):
> 1. **`frame.rs::parse_tile_info` bitstream desync.** `MiCols`/`MiRows` used
>    `width.div_ceil(8)` instead of the spec's `2 * width.div_ceil(8)`, and —
>    the more serious bug — `tile_cols_log2`/`tile_rows_log2` were read by
>    looping "read a bit, stop at 0" with no bound, instead of implementing
>    §5.9.15's `minLog2TileCols`/`maxLog2TileCols`-gated
>    `while (TileColsLog2 < maxLog2TileCols) { increment_tile_cols_log2 f(1); ... }`.
>    Any real frame where the superblock grid is already at
>    `maxLog2TileCols` (i.e. most single-superblock-row/column frames) had
>    the old code read 1-2 phantom bits the encoder never wrote, desyncing
>    every field parsed afterward (`base_q_idx`, loop filter, CDEF, tx mode,
>    ...). Confirmed via a debug harness on a real ffmpeg-encoded 32×32
>    keyframe: `base_q_idx` came out `0`/`lossless=true`/`tile_cols=2`
>    before the fix, `128`/`false`/`1` (correct) after. Fixed by implementing
>    the real spec algorithm (`tile_log2_calc` + the bounded while-loops).
> 2. **`reconstruct.rs::inverse_transform` transform-size bug.** `let n = 1usize
>    << tx_size` computed the DCT/DST basis-matrix edge length from the
>    transform-size *index* (`TX_4X4=0, TX_8X8=1, TX_16X16=2, ...`) directly,
>    giving `n=1,2,4` instead of the correct `n=4,8,16`. Every non-WHT,
>    non-identity (i.e. every ordinary `DCT_DCT`) transform block above the
>    degenerate 1-coefficient case was built from the wrong-size basis
>    matrix and produced the wrong number of output samples. This is the
>    dominant correctness bug: it affects effectively every DCT-coded block
>    in every frame, not just >16×16 blocks (which separately still hit the
>    "not yet reconstructable" skip below). Fixed by using `4usize <<
>    tx_size` instead.
> 3. **Still open, found but not fixed this session:** `reconstruct_intra_subblock`
>    silently skips writing *any* pixels for a block whose selected luma
>    `tx_size` is `TX_32X32`/`TX_64X64` (`if luma_tx <= TX_16X16 { ... }`,
>    `reconstruct.rs` ~line 1816) — the block is left at the 128-gray neutral
>    fill. Confirmed via the debug harness that this is exactly what happens
>    to the `solid_red` 32×32 corpus entry (encoder picks one 64×64
>    `PARTITION_NONE` block with `TX_64X64`; decoder reconstructs nothing).
>    `inverse_transform` itself already refuses `n > 16` for the same reason
>    (no 32-point/64-point DCT-IV basis implemented yet). This is very likely
>    high-impact (large flat/low-detail regions routinely pick large
>    transforms) and is the natural next debugging target.
> 4. Even after fixes 1–2, PSNR across the corpus did **not** recover to
>    anything close to pixel-exact (still ~6–11 dB on most entries) — so
>    there is at least one more substantial bug beyond tx-size skip #3,
>    somewhere in coefficient scan/context, dequant, or intra prediction for
>    non-4×4 blocks. Not yet isolated. A scratch debug harness for this
>    investigation lives at
>    `tpt-kinetix-test-utils/tests/dbg_av1_solid_red.rs` (dumps top-left 8×8
>    luma samples + `FrameHeader`/`SequenceHeaderObu` fields for the
>    `solid_red` corpus entry against dav1d) — reuse or delete once
>    root-caused, following the same "keep debug scratch files uncommitted
>    until resolved" convention as `tpt-kinetix-h264/examples/dbg_*.rs`.
> 5. Also fixed, unrelated to the above: `frame::tests::
>    parse_frame_header_reduced_still_keyframe` was itself failing (a stale
>    hand-built synthetic bitstream that didn't match the fields the parser
>    actually reads for the seq-header flags it set), which meant `cargo
>    test -p tpt-kinetix-av1` was red before this session. Fixed by
>    correcting the test's bit sequence; all 47 `tpt-kinetix-av1` unit tests
>    pass now.
>
> Conformance harness itself (`tpt-kinetix-test-utils/tests/conformance.rs`)
> was already in place and working correctly *as a harness* — it correctly
> reported bad PSNR the whole time; the gap was that nobody had read its
> `--nocapture` output closely before this session.

- [~] Conformance harness in place: `tpt-kinetix-test-utils/tests/conformance.rs::
       av1_vs_ffmpeg_reference_when_available` synthesizes an AV1 keyframe OBU with
       `ffmpeg`, decodes it with both `Av1Decoder` and `ffmpeg`'s AV1 decoder, and
       prints the per-plane PSNR/diff (gated on `ffmpeg`, asserts the sequence-header
       geometry contract today; the pixel-exact `within_tolerance(.., 0)` assertion is
       commented out until Phase C/D land). Sequence Header OBU parsing is exercised
       and passes against real `ffmpeg` keyframes. With Phase D + F wired, the
       harness now exercises the full intra decode → loop-filter → diff path.
       A standalone, h264-free validation harness was also added:
       `tpt-kinetix-av1/examples/av1_psnr_check.rs` (generates a keyframe per
       `lavfi` source, decodes with both decoders, prints per-plane PSNR) so AV1
       progress is measurable without building the (currently-broken-in-working-tree)
       H.264 crate.
- [x] **Item 3 (TX_32X32 / TX_64X64 reconstruction) — implemented (2026-08-15):**
       `reconstruct.rs::inverse_transform` now builds the full DCT-IV / DST-VII basis
       for `n = 32` and `n = 64` (the previous `n > 16` copy-through guard is gone),
       `reconstruct_intra_subblock` no longer skips `luma_tx > TX_16X16` (luma or
       chroma), and the chroma `tx_size` selection reaches `TX_32X32`. The required
       `get_scan` tables for 32×32 / 64×64 were added in
       `coeff_tables.rs` (sub-block scans per AV1 §7.11: 4×4 sub-blocks ordered by
       the 8×8 / 16×16 scan) so `read_coeffs` no longer errors on large blocks.
- [x] **Inverse-transform scaling (the dominant non-4×4 correctness bug) — fixed
       (2026-08-15):** the unnormalized 2-D DCT-IV / DST-VII basis gives
       `M·Mᵀ = (n/2)·I`, so the 2-D inverse transform produces `(n/2)·residual`; the
       final shift must therefore be `log2(n) − 1` (8×8 → `>> 2`, 16×16 → `>> 3`,
       32×32 → `>> 4`, 64×64 → `>> 5`), **not** the previous hard-coded `>> 1` that
       only matched the 4×4 case. `reconstruct.rs::round_shift` / `inverse_transform`
       now apply the `n`-dependent shift. This is item 4's largest contributor
       (every 8×8/16×16 block was reconstructed 2× / 4× too large before).
- [x] **Loop-filter OOB panic — fixed (2026-08-15):** `loop_filter.rs`'s wide
       deblock filter read/wrote outside the line buffer at frame/tile edges
       (`line[(edge + pidx) as usize]` with `pidx` reaching negative), panicking the
       whole decode on e.g. 128×96 `testsrc`. Reads/writes are now clamped to the
       buffer; the loop filter remains non-pixel-exact (already acknowledged) but no
       longer crashes.
- [ ] **Remaining gap (item 4):** the corpus is still far from pixel-exact
       (~7–22 dB PSNR after the fixes above; a solid 32×32 red keyframe reaches
       ~21 dB Y while a 64×64 red keyframe is still ~8 dB, with most pixels left at
       the 128 neutral fill). Diagnostics show blocks decoding as `all_zero`/skip when
       they are not, i.e. a **symbol-decoder desync in the intra block path**
       (the mode/skip/`tx_size`/`txb_skip`/`coeffs` reads share one `SymbolDecoder`
       and any mis-contexted read desyncs every subsequent coefficient). The
       `read_coeffs` unit tests (cross-checked against a Python oracle) still pass in
       isolation, so the desync is in the *integration* context, not the coeff syntax
       itself. Root-causing this is the next debugging target before inter prediction
       (Phase E) is worth landing. **Updated 2026-08-15 (further session,
       uncommitted):** a plausible root cause was fixed — `decode_tile_group`
       started the `SymbolDecoder` at bit offset 0 instead of past the
       mandatory `tile_group_header()` syntax (§5.11.1), which for multi-tile
       and/or inter frames desyncs every bit read thereafter; `obu.rs`'s
       `BitReader` gained `byte_align()`/`bit_position()` so the header can
       now be parsed for real before handing off the `tile_data` offset.
       **Ruled out as the root cause (2026-08-16):** re-ran both
       `av1_psnr_check` and `av1_vs_ffmpeg_reference_when_available` — PSNR
       is essentially unchanged/still poor across the corpus (7–13 dB Y on
       testsrc/mandelbrot/smptebars/testsrc2/solid_red_64), and
       `solid_red_32` actually regressed (~21 → 16.10 dB Y). The fix itself
       is correct per spec and should stay, but the intra-block
       symbol-decoder desync is still unexplained — next debugging target is
       still open. The temporary `eprintln!("DBG ...")` lines from that
       investigation are already gone from the working tree.
- [ ] Validate decode vs `dav1d` reference output on a generated intra-only
       corpus first (Phases A–C only), then again once inter prediction
       (Phase E) lands
- [ ] Flip `Av1Decoder::capabilities().pixel_exact` only after the conformance
       harness passes

> **2026-08-15 session note (AV1 continues).** Implemented the AV1 Phase G item-3
> blockers and the dominant inverse-transform scaling bug (above): `inverse_transform`
> now applies the `log2(n) − 1` final shift (was a hard-coded `>> 1`), supports
> `n = 32`/`64`, the 32×32 / 64×64 scan tables are wired in `coeff_tables.rs`, and
> `reconstruct_intra_subblock` no longer skips large `tx_size` blocks. The loop-filter
> wide-filter OOB read/write panic (frame/tile-edge) is clamped. A standalone
> `av1_psnr_check` example measures per-plane PSNR vs `ffmpeg` without the H.264
> crate. Post-fix PSNR is still ~7–22 dB (solid 32×32 red ≈21 dB Y; 64×64 red ≈8 dB
> with most pixels at the 128 neutral fill), indicating a residual symbol-decoder
> desync in the intra block path (mode/skip/`tx_size`/`txb_skip`/`coeffs` share one
> `SymbolDecoder` and a mis-contexted read desyncs later coefficients). `cargo test
> -p tpt-kinetix-av1 --lib` is green (47 tests, including an extended
> `scan_is_valid_permutation` that now also validates the 32/64 large scans).
> Uncommitted working-tree files: `tpt-kinetix-av1/examples/av1_psnr_check.rs`
> (validation harness; keep — useful standalone regardless). Note: as of the
> later-in-session AAC/symphonia work (see the AAC session note above), the
> H.264 crate is no longer broken in the working tree, so this example is no
> longer the *only* way to validate AV1, but it's still a faster one.

> **2026-08-17 session note (root-caused the multi-block intra desync).**
> Picked up exactly where the previous session left off: `dbg_av1_smptebars`
> (a 64×64 crop of ffmpeg's `smptebars` test pattern — plain flat color bars,
> no texture) decodes with the top-left 8×8 luma block ranging 158–223
> instead of the reference's flat 180, even though the same crate is
> bit-exact on `solid_red` (a single 64×64 block, no downstream reads to
> desync). Root cause and fix:
>
> **Bug: `TxBlockCtx::block_w`/`block_h` were wired to the *transform*
> block's own size, not the *coded* block's plane-residual size, at every
> real call site.** The struct's own doc comment is explicit that these
> fields mean `Block_Width[bsize]`/`Block_Height[bsize]` (i.e. the spec's
> `bw`/`bh` in `all_zero`'s §8.3.2 context derivation) — but
> `reconstruct/intra_block.rs`'s and `reconstruct/inter_block.rs`'s luma call
> sites passed `luma_tx_w`/`luma_tx_h` (the *tx block's* `Tx_Width`/
> `Tx_Height`, spec's `w`/`h`) and the chroma call sites passed `cw`/`ch`
> (same mistake, chroma-tx-sized). Since `blk.block_w`/`block_h` are always
> equal to `w`/`h` by construction under that bug, `all_zero_ctx`'s first
> branch (`if blk.block_w == w && blk.block_h == h { ctx = 0 }` — the spec's
> "this transform *is* the whole coded block, ignore neighbour state"
> special case) fired unconditionally for every transform block, real
> neighbour `AboveLevelContext`/`LeftLevelContext` state included. That reads
> `all_zero` (`txb_skip`) from the wrong CDF context whenever the true
> context should have been nonzero, silently decoding the wrong boolean —
> and, whenever it wrongly decoded `false` for a block that should have
> been `all_zero == true`, spuriously consumed an entire `coeffs()` (eob,
> level, sign, Exp-Golomb) read that the real bitstream never wrote,
> desyncing every subsequent symbol in the tile.
>
> This exactly matches the previous session's own diagnosis ("the
> `read_coeffs` unit tests still pass in isolation ... so the desync is in
> the integration context, not the coeff syntax itself") — confirmed here:
> `coeff.rs`'s existing `coeffs()` oracle tests already parametrize
> `block_w`/`block_h` correctly per-scenario (see `fn blk(...)` in its test
> module), so they never exercised the buggy wiring; the bug lived entirely
> in the three real call sites, not in `coeff.rs` itself.
>
> Why `solid_red` never caught this: it decodes as one `PARTITION_NONE`
> 64×64 block with one `TX_64X64` luma transform — `block_w == w` is
> genuinely true there, so the buggy branch and the correct one agree.
> `smptebars`'s first leaf block is a `BLOCK_32X8` (chosen because its
> content is 4 flat color-bar palette colors — `colors_y = [112, 131, 162,
> 180]`, which are exactly the four bar luma values dav1d also decodes) with
> `tx_size = TX_16X8`, i.e. **two** luma transform blocks — the second one is
> where `block_w (16) == w (16)` accidentally still held (single tx-depth
> split of a 32-wide block into two 16-wide halves happens to make `bw`
> ambiguous at that specific size), but any block whose tx split produces
> more sub-blocks, or whose coded-block width isn't exactly `2×` its tx
> width, hits the real bug. Confirmed via the `KINETIX_AV1_DBG=1` env-gated
> trace this session added to `intra_block.rs`/`reconstruct_block.rs`/
> `reconstruct/mod.rs` (kept in the working tree, opt-in only): before the
> fix the first `16×8` tx block decoded `eob=57` (garbage) where the
> reference is provably flat; after the fix it decodes `eob=0` and the
> reconstructed pixels match the reference exactly for that block.
>
> **Fix**: in `reconstruct/intra_block.rs` and `reconstruct/inter_block.rs`,
> every `TxBlockCtx { block_w, block_h, .. }` construction now uses the coded
> block's own plane-residual size (`bw * MI_SIZE`/`bh * MI_SIZE` for luma —
> `bw`/`bh` are already `BLOCK_WIDTH[bsize]/MI_SIZE` etc. in scope; the
> already-computed `chroma_bw`/`chroma_bh` — themselves
> `Block_Width`/`Height[get_plane_residual_size(MiSize, plane)]` — for intra
> chroma; the equivalent `(bw * MI_SIZE) >> subsampling_{x,y}` approximation
> for the simplified inter chroma path, consistent with that path's existing
> `c_tx` heuristic) instead of the transform block's own `luma_tx_w`/`_h` or
> `cw`/`ch`.
>
> **Regression test**: `tpt-kinetix-av1/src/coeff.rs`'s
> `all_zero_ctx_ignores_neighbour_levels_only_when_tx_covers_the_whole_coded_block`
> directly exercises `all_zero_ctx` with a "whole coded block" `TxBlockCtx`
> (asserts `ctx == 0`, matching the spec's ignore-neighbours special case)
> and a "coded block split into two tx blocks" `TxBlockCtx` with a hot left
> neighbour (asserts `ctx == 3`, i.e. the neighbour state is *not* ignored) —
> this would have caught the bug had it existed at the `all_zero_ctx` call
> boundary, which is exactly where the real call sites went wrong.
>
> **Impact measured**: `dbg_av1_smptebars`'s top-left 16×8 luma block goes
> from garbage (`eob=57`, max abs diff vs reference up to 220 in the
> top-left 16×16 region) to bit-exact (`eob=0`, matches reference exactly).
> `cargo test -p tpt-kinetix-av1 --lib` is green, now 74/74 (was 73/73).
> Whole-corpus `av1_psnr_check` (a *different*, larger 256×144/320×180/etc.
> corpus, not the 64×64 `smptebars` crop the debug harness uses) moved only
> marginally: `testsrc_128x96` 11.18→10.78 dB (slightly worse — noise-level
> either way), `mandelbrot_128x96` 15.15→15.87 dB, `smptebars_256x144`
> 11.43→11.60 dB, `testsrc2_320x180` 11.45→12.28 dB; `solid_red_32`/`_64`
> stay 99.00 dB (unaffected, as expected — see above for why). **This
> confirms the bug was real and is now fixed, but it is not the only
> remaining source of error** — a full multi-superblock frame still has
> plenty of PSNR left on the table beyond this one fix.
>
> **New lead for the next session, found while root-causing the above (not
> yet fixed): CDEF is very likely over-smoothing genuine hard content edges
> that the reference decoder leaves untouched.** After the `all_zero_ctx` fix,
> `dbg_av1_smptebars`'s reconstructed top row is *exactly* correct through
> the palette-driven color-bar boundaries at samples 0–9 (180), 10–19 (162),
> 20–29 (131), 30–31 (112) — except samples 14–17, which come out `144`
> (not even one of the four real palette colors) instead of `162`. Traced
> with the same `KINETIX_AV1_DBG` instrumentation: the *pre-filter*
> reconstruction at that position is already correct (`pred[0..8] =
> [162,162,162,162,131,131,131,131]` for the second tx block, `eob = 0`,
> i.e. no residual) — the corruption is introduced entirely by
> `apply_post_filters` (deblock → CDEF) afterward. This frame's real
> `loop_filter_level = [4, 4, 0, 0]` and `cdef_y_strength = [12]` (both
> genuinely nonzero, so the reference decoder does run both filters too, and
> still produces a razor-sharp edge). Manually walked `loop_filter.rs`'s
> `filter_line_1d` mask/flatness math for the checked 8-sample-grid edges
> near this position (`x = 8`, `x = 16`) by hand against §7.14.6 — at both,
> the *immediately adjacent* samples across the edge are equal (both 180 at
> `x=8`, both 162 at `x=16`, since the real color change is at `x=10`/`x=20`,
> not aligned to the 8-sample deblock grid at all), and the narrow-filter
> branch (`!flat`, triggered because the *wide* flatness check's far taps
> cross the real `x=10` boundary) only touches `p1,p0,q0,q1` — which are all
> equal, so deblocking alone can't be the source of the `144`. That leaves
> CDEF (`cdef_y_strength = [12]`, i.e. primary strength 12 / secondary 0 —
> not a no-op) as the prime suspect: this crate's `cdef_plane_luma` has not
> been validated against real nonzero-strength content the way the
> deblocking filter's `filter_line_identity_when_flat` unit test validates
> the flat case. **Concretely worth checking next**: whether
> `cdef_plane_luma`'s direction search / primary-tap threshold logic
> correctly refuses to blend across a real (non-quantization-noise) edge the
> way AV1's CDEF is specified to (§7.15.3's `cdef_get_dir` /
> `constrain()`), or whether it's applying an unconditional/under-thresholded
> blend. (Verified this is CDEF and not deblocking by temporarily flipping
> `apply_post_filters`'s `subsampling_x`/`subsampling_y` bool arguments to
> `false` to try to no-op the filters — **do not repeat this**, those two
> booleans are the real 4:2:0 subsampling flags, not filter-enable toggles,
> and setting them `false` panics in `loop_filter.rs:520` on a UV-plane
> index-out-of-bounds; reverted immediately. There is currently no clean way
> to independently disable just CDEF or just deblocking from outside
> `loop_filter.rs` for this kind of diagnosis — adding one would help future
> sessions isolate filter-stage bugs faster.)
>
> Uncommitted working-tree files this session: `tpt-kinetix-av1/src/coeff.rs`
> (the fix + new regression test), `tpt-kinetix-av1/src/reconstruct/mod.rs`
> (a `KINETIX_AV1_DBG`-gated frame-header trace print, harmless/opt-in).
> **Note**: the concurrent automated process mentioned elsewhere in this
> repo's notes (see the memory entry on concurrent repo activity) committed
> `tpt-kinetix-av1/src/reconstruct/intra_block.rs`,
> `reconstruct/inter_block.rs`, `reconstruct/reconstruct_block.rs`, and
> `tpt-kinetix-test-utils/tests/dbg_av1_smptebars.rs` mid-session as part of
> its own unrelated "Modularize AV1 reconstruct and H.264 decoder" commit
> (`b4a2870`) — which happened to sweep up this session's in-progress edits
> to those files (the `block_w`/`block_h` fix among them) since they were
> sitting in the working tree at commit time. That commit was made by the
> other process, not by this session (this session made no `git commit`
> calls), so the letter of "leave changes uncommitted" was respected, but
> the practical result is that most of this session's fix already landed in
> git history under an unrelated commit message. Only `coeff.rs` and
> `reconstruct/mod.rs` remain uncommitted in the working tree as of this
> note.

