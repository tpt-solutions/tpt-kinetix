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

#### AV1 Phase G.0 — tooling: reusable symbol-trace oracle (NEW, 2026-08-20)

> Decided 2026-08-20 after 7 straight debugging sessions (2026-08-17→19, see
> Phase G session notes below) each found real bugs but via slow, ad hoc,
> one-off methods: (a) manual spec-PDF cross-checking of one syntax element
> at a time, and (b) four separate throwaway debug harnesses
> (`dbg_av1_smptebars.rs`/`dbg_av1_testsrc128.rs`/`dbg_av1_mandelbrot128.rs`/
> `dbg_av1_mandelbrot_diffmap.rs` in `tpt-kinetix-test-utils/tests/`), each
> hand-rolling the same "decode with ffmpeg reference + decode with Kinetix +
> compare a crop" pattern and then getting abandoned once that specific crop
> stopped being useful. Several session notes above independently flagged the
> same missing capability as their proposed next step ("an independent
> from-scratch bit-level re-decode... to catch a numerically-wrong default
> CDF table entry that spot-checks miss") without anyone actually building it.
> This phase is that build-it-once investment, so future sessions stop paying
> the "rebuild diagnostic infra" tax before they can even start on the next
> bug.

- [ ] **Not completed (2026-08-20).** Build a **symbol-level oracle**: an
      independent AV1 entropy-decode trace. Option (a) — `dav1d`/`ffmpeg`
      trace mode — was investigated and ruled out: `ffmpeg -h decoder=av1`
      exposes only `operating_point` as an AVOption, no verbose/trace flag
      reaches per-symbol `libdav1d` internals, and building `dav1d` from
      source with a debug/trace feature flag was judged impractical in this
      session's time budget. Option (b) — extending `coeff.rs`'s existing
      Python-cross-checked `coeffs()`-only oracle to real captured bitstream
      bytes and the full `intra_frame_mode_info()` sequence — was *not*
      attempted this session (deliberately: see session note below for why
      the harness took priority). That existing oracle still only runs
      against synthetic `ramp()` buffers today. Open for a future session.
- [x] Build a **generic differential-trace harness** (2026-08-20): done, see
      `tpt-kinetix-test-utils/examples/av1_symbol_trace_diff.rs` (run via
      `just av1-trace-diff [label|--all]`). Decodes a corpus entry with both
      `dav1d` (reference) and `Av1Decoder` (with a new structured symbol
      trace enabled), finds the first diverging pixel, brackets whether the
      post-filter chain (deblock/CDEF) is implicated via a
      `KINETIX_AV1_NOFILTER` re-decode, and prints the symbol-trace entries
      (source location, alphabet size, decoded value, bit position) around
      the nearest preceding block marker. **Not fully done**: only the
      *final* decoded frame is diffed (plus the NOFILTER bracket) — there is
      no public API yet to snapshot pre-filter/post-deblock/post-CDEF/
      post-restoration buffers separately, so the "walk every pipeline
      stage" part of the spec is partial, not complete (see the harness's
      own doc comment for the full limitations list). Does not yet replace
      the four `dbg_av1_*.rs` files (they still exist, uncommitted, as
      historical reference) since it doesn't do everything they each did
      ad hoc (e.g. `dbg_av1_mandelbrot_diffmap.rs`'s per-8×8-block heatmap).
- [x] Document how to invoke it (2026-08-20): `just av1-trace-diff` recipe
      added to `justfile`, following the `conformance`/`corpus-check`
      pattern; the example binary's own module doc comment is the primary
      reference.
- [~] Once built, re-point the still-open AV1 Phase G root-cause work at the
      new oracle instead of continuing manual tracing (2026-08-20): the
      *harness* was run against the current `mandelbrot` corpus entry and
      immediately (no manual instrumentation) found a first divergence at
      `plane=Y px=(64,0)` — see session note below for the actual output and
      why this isn't the same location as the previously-reported
      `mi=(0,8)`/`px=(32,8)` (a different `mandelbrot` encode). The
      *oracle* half (independent symbol-level verification of that
      divergence) was not built this session, so this is validated as
      "the harness works and finds real divergences fast" but not yet as
      "the harness pinpoints the exact wrong symbol independently".

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

> **2026-08-18 session note (root-caused the `144`-artifact lead — it was
> deblocking, not CDEF).** Picked up the previous session's CDEF lead on
> `dbg_av1_smptebars` (samples 14–17 of the top-left row decode `144` instead
> of the reference's `162`). Reproduced with `cargo test -p
> tpt-kinetix-test-utils --test dbg_av1_smptebars -- --nocapture` (no env var
> needed to reproduce; `KINETIX_AV1_DBG=1` only gates the temporary trace
> instrumentation this session added and removed).
>
> **First, two real (but not the primary) bugs found and fixed while
> checking the CDEF lead against the spec/`dav1d`, in `loop_filter.rs`:**
> 1. `cdef_plane_luma`/`cdef_plane_chroma`'s variance-based primary-strength
>    adjustment (`var_str`) was clamped `.min(31)` — a leftover from
>    `floor_log2`'s natural `u32` range — instead of the spec's `Min(
>    FloorLog2(var >> 6), 12)` (§7.15.2's `cdef_block`, confirmed against
>    `dav1d`'s `adjust_strength`: `i = imin(ulog2(var >> 6), 12)`). High-
>    variance blocks (real edges, not noise) could receive a far-too-strong
>    effective primary strength.
> 2. `cdef_constrain` used `sign(diff) · (abs(diff) − (abs(diff) >> shift))`
>    clamped to `±threshold` — structurally different from, and more
>    aggressive than, the spec's actual `constrain()`. Confirmed against
>    `dav1d`'s `cdef_tmpl.c`: `imin(adiff, imax(0, threshold − (adiff >>
>    shift)))`. Concretely, `cdef_constrain(100, 10, 7)` returned `10` before
>    the fix, `4` after — for a `diff` well past `threshold`, the old formula
>    let through nearly the full threshold instead of the correct, much more
>    conservative value.
>
> Both are real correctness bugs (now fixed, with regression tests
> `cdef_variance_strength_adjustment_caps_at_twelve` and
> `cdef_constrain_matches_dav1d_formula_for_a_large_diff` in
> `loop_filter.rs`), but **neither was the actual source of the `144`
> artifact** — confirmed directly (not by hand-walking) by adding temporary
> instrumentation that dumped `cdef_plane_luma`'s `src` snapshot (the
> post-deblock, pre-CDEF plane state) for the two affected 8×8 blocks: the
> `144` values were *already present* in that snapshot, before CDEF ever
> touched the plane. This directly contradicts the previous session's manual
> §7.14.6 hand-walk, which concluded deblocking couldn't be the source
> because "the samples immediately adjacent to the checked edges are equal on
> both sides" — true, but that hand-walk didn't account for the deblock
> filter's own internal clamp bug (below), which corrupts flat regions
> regardless of whether the immediately-adjacent samples are equal.
>
> **Root cause, found by instrumenting `filter_line_1d` directly at the
> `edge = 16` vertical deblock edge**: the narrow (4-tap) filter branch
> (§7.14.6.3) clamped its intermediate `filter`/`filter1`/`filter2` values,
> and the final output pixel, to `[-blimit, blimit]` — but `blimit` (here
> `16`) is a *filter-mask threshold* (§7.14.6.2, gates whether to filter at
> all), not a value-clamp range. The spec's actual intermediate clamp is the
> full signed-sample range `[-128, 127]` (8-bit), and the final pixel clamp
> is `[0, 255]` — cross-checked against `dav1d`'s `loopfilter_tmpl.c`
> (`iclip_diff` = `iclip(v, -128, 127)`, `iclip_pixel` = `iclip(v, 0, 255)`).
> For flat content whose value is far from 128 — which is the *common* case,
> not an edge case (e.g. `smptebars`'s color-bar value `162`, `qs0 = 162 -
> 128 = 34`) — `clip3(34, -16, 16)` clamped to `16`, giving `16 + 128 = 144`
> regardless of the true (correctly-zero) filter delta. This reproduces the
> exact reported artifact value. The bug fires even when `filter`/`f1`/`f2`
> are genuinely `0` (i.e. even on perfectly flat input), since the bug is in
> the *final* clamp, not the filter computation — any narrow-filter
> application to flat content whose sample value deviates from 128 by more
> than `blimit` corrupts it. The existing unit tests didn't catch this
> because they happened to use sample values close to 128 (`100`) or a large
> enough `blimit` (`80`) that the deviation never exceeded the (wrong) clamp
> range.
>
> **Fix**: in `filter_line_1d`'s narrow-filter branch, changed the four
> `clip3(..., -blimit, blimit)` calls on `filter`/`f1`/`f2` to `clip3(...,
> -128, 127)`, and the four final-pixel `clip3(..., -blimit, blimit) + 128`
> expressions to `clip3(... + 128, 0, 255)` (matching `dav1d`'s `iclip_pixel`
> form directly, rather than clamping-then-adding-128).
>
> **Regression tests** added in `loop_filter.rs`:
> `narrow_filter_leaves_flat_content_far_from_mid_gray_unchanged` (flat `162`
> content, `blimit = 16`, asserts a no-op — this is the direct repro of the
> bug) and `narrow_filter_ignores_a_real_edge_beyond_its_four_tap_reach` (the
> exact `smptebars` shape: flat `162` taps at the edge with a real `131`
> transition just outside the narrow filter's reach, asserts the edge doesn't
> perturb the flat samples next to it).
>
> **Impact measured**: `dbg_av1_smptebars`'s per-16×16-block max-abs-diff map
> improved sharply in the top-left region — row 1 went from `[18, 18, 27,
> 89]` to `[2, 2, 50, 78]` (blocks 3 and 4, further right, still have other,
> unrelated errors). The top-left `32×8` dump's first ~10 columns are now
> off by at most 1–2 (residual CDEF smoothing right at the true color-bar
> boundary, plausible given a real nonzero `cdef_y_strength`) instead of the
> `144` artifact 4 samples wide. `cargo test -p tpt-kinetix-av1 --lib` is
> green, now 78/78 (was 74/74; +4 new regression tests). Whole-corpus
> `av1_psnr_check` barely moved: `testsrc_128x96` 10.78→10.78 dB,
> `mandelbrot_128x96` 15.87→15.74 dB (very slightly down — noise-level),
> `smptebars_256x144` 11.60→11.55 dB (very slightly down, also noise-level —
> the 64×64 `dbg_av1_smptebars` crop and the 256×144 corpus entry are
> different source frames), `testsrc2_320x180` 12.28→12.28 dB, `solid_red_32`/
> `_64` unaffected (99.00 dB, as expected — solid content never engages the
> narrow filter's flat-far-from-128 case in a way that mattered before,
> since flat *and* already-correct). **This is the same pattern as the
> previous session's `all_zero_ctx` fix: a real, spec-verified, unit-tested
> bug fix with a clean before/after repro on the small debug crop, but the
> larger multi-superblock corpus PSNR is dominated by other, still-open
> sources of error that this fix doesn't touch.**
>
> **What's still open for the next session**:
> - The wide-filter branch (§7.14.6.4) was checked for the same
>   `blimit`-vs-`[0,255]` clamp mistake and does *not* have it — its final
>   `clip3(f, 0, 255)` already clamps the tap-weighted average directly to
>   the pixel range, not to `blimit`. Not a bug, but worth being aware of as
>   the "why didn't this also need fixing" answer if it comes up again.
> - `smptebars_256x144`'s corpus-level PSNR (11.55 dB) and the other three
>   non-solid corpus entries are still far from pixel-exact. The per-block
>   diff map's blocks 3–4 (`[50, 78]`) in the `dbg_av1_smptebars` crop are
>   still very wrong and haven't been root-caused this session — that's the
>   next concrete lead: dump the same pre-filter vs post-deblock vs post-CDEF
>   breakdown for those blocks (columns roughly 32–63 of the top row) the way
>   this session did for the `144` artifact, to see whether it's another
>   deblock/CDEF bug, a residual coefficient desync, or a predictor bug.
> - The horizontal-edge deblock pass and chroma planes were not specifically
>   re-verified against real nonzero content after this fix (the fix is in
>   shared code (`filter_line_1d`) so it should apply uniformly, but no
>   chroma-specific regression test was added this session).
> - `cdef_plane_chroma`'s "re-derive direction from the chroma block itself
>   instead of reusing the co-located luma direction" simplification (see its
>   own doc comment) is still unvalidated against real chroma content — flagged
>   by a previous session's doc comment, still open.
> - Loop restoration (§7.17) remains an explicit no-op passthrough; not
>   revisited this session.
>
> Uncommitted working-tree files this session: `tpt-kinetix-av1/src/loop_filter.rs`
> (the two CDEF fixes, the narrow-filter clamp fix, and 4 new regression
> tests). No `KINETIX_AV1_DBG`-gated instrumentation was left behind this
> time — the temporary trace prints used to isolate the bug were added and
> removed within this session, since a targeted `#[test]` reproduces the bug
> directly and permanently instead.

> **2026-08-18 session note (cont'd) — root-caused a real `Intra_Mode_Context`
> transcription bug via the "blocks 3-4" lead.** Picked up the explicit next
> lead from the note above: `dbg_av1_smptebars`'s per-16×16-block max-abs-diff
> map, blocks 3-4 of row 1 (`[50, 78]`), unexamined. Extended
> `tpt-kinetix-test-utils/tests/dbg_av1_smptebars.rs` with column-32-63 /
> row-0-15 dumps and widened the existing `KINETIX_AV1_DBG`-gated traces in
> `reconstruct/intra_block.rs` (`mi_row==0&&mi_col==0` → `mi_row<4`) and
> `reconstruct/reconstruct_block.rs` (`px_x<32` → `px_x<64`, `px_y==0` →
> `px_y<16`) to cover the previously-unexamined region (both left in the tree,
> opt-in only).
>
> First finding: the region's first *palette-free, AC-coefficient* block
> (`mi=(0,12)`, `BLOCK_16X4`, luma tx `TX_8X4`, `y_mode=DC_PRED`,
> `tx_type=V_DCT`, `eob=13`) reconstructed a plausible-looking but wrong
> residual (`[84,84,84,70,98,84,73,84]` vs the reference's flat step
> `[84,84,65,65,65,65,65,65]`) even though the DC prediction feeding it was
> already provably correct (`pred=84`, matching `have_left=true, left=84`).
> Crucially, every block *after* this one still decoded structurally
> plausible mode/tx syntax (no panic, no wildly-desynced garbage) — the
> signature of a context-selection bug picking the wrong CDF (still a valid,
> self-terminating range-decoder symbol read) rather than a bit-count/desync
> bug like the two fixed in the last two sessions.
>
> Spent most of the session ruling out the coefficient-context machinery in
> `coeff.rs`/`coeff_tables.rs` this block actually exercises
> (`coeff_base_ctx`, `coeff_br_ctx`, `dc_sign_ctx`, `get_scan`/`get_tx_class`
> row-vs-column dispatch, `row_axis_transform`/`col_axis_transform` in
> `reconstruct/transform.rs`, `TRANSFORM_ROW_SHIFT`, the rectangular
> `needs_rescale` 2896/4096 correction) by transcribing the actual spec text
> from a locally-downloaded copy of the PDF (`pdftotext -layout`, since
> `WebFetch`-mediated summarization of large numeric C tables from GitHub/
> googlesource mirrors proved unreliable — it silently relabeled and
> mis-transcribed table names/values across two separate attempts and should
> not be trusted for exact numeric spec data going forward; download the PDF
> and grep/read it directly instead). `COEFF_BASE_CTX_OFFSET` (475 values)
> was checked in full against the spec's `Coeff_Base_Ctx_Offset` table
> (§ Parsing process, page 374-376) and is transcribed correctly. All of the
> above turned out fine.
>
> **Root cause, found while checking `intra_frame_y_mode`'s CDF-selection
> context (spec §8.3.2) instead**: `reconstruct/mod.rs`'s `INTRA_MODE_CONTEXT`
> table — `Intra_Mode_Context[INTRA_MODES]`, which maps a neighbour's decoded
> intra mode to a 0..4 bucket used to index
> `TileIntraFrameYModeCdf[abovemode][leftmode]` — read `[0, 1, 2, 3, 4, 4, 4,
> 3, 3, 1, 1, 2, 0]`. The spec's actual table (confirmed directly from the
> PDF, page 361): `{0, 1, 2, 3, 4, 4, 4, 4, 3, 0, 1, 2, 0}`. Two entries were
> wrong: index 7 (`D207_PRED`) read `3` instead of `4`, and index 9
> (`SMOOTH_PRED`) read `1` instead of `0`. Both are common real-encoder mode
> choices — `SMOOTH_PRED` especially, on flat/gradient content exactly like
> `smptebars`'s color bars — so any block whose above or left neighbour used
> either mode got the *wrong* 2-D CDF context for its own
> `intra_frame_y_mode` read. Per the reasoning above, this doesn't desync the
> bitstream (arithmetic/range coding is self-terminating regardless of
> whether the context matched the true encoder's), it just decodes a
> plausible-but-wrong `y_mode` for that one block — which then cascades into
> that block's own `tx_type`/coefficient reads picking the wrong CDFs too
> (`read_transform_type`'s `dir = blk.intra_dir` is the block's *own*
> `y_mode`, correctly propagated, so a wrong `y_mode` here means an
> internally-consistent but still wrong transform-type CDF downstream) —
> matching the exact "locally garbage, globally still-plausible" symptom
> observed. `mi=(0,8)` (the block immediately above/left of the corrupted
> `mi=(0,12)`) decoded `y_mode=9` = `SMOOTH_PRED`, confirming this is the
> exact trigger for the observed corruption.
>
> **Fix**: `reconstruct/mod.rs`'s `INTRA_MODE_CONTEXT` corrected to `[0, 1, 2,
> 3, 4, 4, 4, 4, 3, 0, 1, 2, 0]`.
>
> **Regression tests**: updated the existing
> `intra_y_mode_context_uses_above_left_as_independent_axes` (its illustrative
> "old wrong formula" numbers used the *old, buggy* `D207_PRED` context value
> as if it were correct — switched to `D157_PRED`, an unaffected index, so
> the test still demonstrates the *original* 2026-08-16 sum-vs-independent-
> axes bug without asserting the newly-fixed-away wrong value) and added
> `intra_mode_context_table_matches_spec_at_the_two_previously_wrong_indices`
> in `reconstruct/tests.rs`, which pins the full 13-entry table against the
> spec text and specifically checks both previously-wrong indices.
>
> **Impact measured**: `dbg_av1_smptebars`'s per-16×16-block max-abs-diff map
> went from `[2,0,25,91] / [2,2,50,78] / [160,220,163,95] / [123,135,154,109]`
> to `[2,0,1,2] / [2,0,1,2] / [3,2,2,20] / [3,1,1,5]` — every block in the
> crop improved, most dramatically (blocks that were off by 91/78/160/220/163
> /95/123/135/154/109 are now off by at most 20, most ≤5). `cargo test -p
> tpt-kinetix-av1 --lib` is green, 79/79 (was 78/78; +1 net new test, one
> existing test's illustrative constants corrected). Whole-corpus
> `av1_psnr_check` moved in mixed directions, all still noise-level:
> `testsrc_128x96` 10.78→11.03 dB, `mandelbrot_128x96` 15.74→16.62 dB (both
> improved), `smptebars_256x144` 11.55→10.15 dB, `testsrc2_320x180`
> 12.28→11.13 dB (both slightly worse), `solid_red_32`/`_64` unaffected
> (99.00 dB). **Same pattern as every fix this bug hunt has found: a real,
> spec-verified bug with a dramatic, clean before/after win on the small
> debug crop, but the larger multi-superblock corpus PSNR is still dominated
> by other, stacked, still-open sources of error.**
>
> **New lead for the next session, found while re-checking the crop after
> this fix (not yet root-caused)**: the crop's remaining worst block (max
> diff 20, at columns 48-63/rows 32-47) is a *different* bug from anything
> fixed so far. Reference content there is flat `19` across columns 50-59,
> rows 32-41, with a genuine content transition at row 42 (not 8-pixel-grid-
> aligned) to a different flat region (`131`/`19`/`180` bands). Traced with
> the same pre-filter/post-deblock/post-cdef instrumentation (now covering
> rows 32-47 too, left in `loop_filter.rs`, opt-in via `KINETIX_AV1_DBG=1`):
> at column x=57, rows 42-47, the **pre-filter** value is correctly flat `19`
> for every row, but **post-deblock** it becomes a smooth gradient (`39, 33,
> 31, 28, 25, 22`) that CDEF then only mildly perturbs further — i.e. this is
> a *deblocking* bug, not CDEF, and it is **not** the already-fixed
> `blimit`-as-clamp-range bug (that fix is confirmed still in place and
> correct; this is a new, separate defect). The smooth multi-row gradient
> shape strongly suggests the *horizontal*-edge (row-boundary) wide filter
> (§7.14.6.4, the 13-tap/`log2=4` branch) is blending across the real row-42
> content transition despite it being well outside a genuinely flat region —
> i.e. either the `flat`/`flat2` masks (`reach up to 6` for `filter_size>=16`)
> are misclassifying this window as flat when the real content jumps `65→19`
> within that reach, or `filter_size` itself is being computed larger than it
> should be for this edge (the coded blocks here are small — `TX_8X4`/
> `TX_16X4` — so `filter_size` should plausibly be capped at 4 or 8, not 16,
> which would take the wide path's `log2=4`/13-tap branch out of consideration
> entirely). **Concretely worth checking next**: (1) how `filter_size` is
> computed per-edge in `deblock_plane` (the `tx_grid`/neighbour-transform-size
> lookup feeding `filter_line_1d`'s `filter_size` parameter) for a horizontal
> edge adjacent to small transform blocks, cross-checked against spec
> §7.14.2's `Filter_Size` derivation; (2) whether `flat`/`flat2`'s per-line
> masks are being computed on the correct axis (column-of-samples for a
> horizontal edge, not a row) with the correct absolute-position taps: add
> row/col-aware `KINETIX_AV1_DBG` instrumentation directly inside
> `filter_line_1d` (it currently only receives a 1-D `line` slice + relative
> `edge` offset, no absolute frame coordinates, so pinpointing exactly which
> call this is requires either passing coordinates through or temporarily
> hardcoding a value-based trigger like "if edge output at this offset
> changes by more than N, print `mask7`/`flat`/`flat2`/`filter_size`").
>
> Uncommitted working-tree files this session: `tpt-kinetix-av1/src/reconstruct/mod.rs`
> (the `INTRA_MODE_CONTEXT` fix), `tpt-kinetix-av1/src/reconstruct/tests.rs`
> (corrected + new regression tests), `tpt-kinetix-av1/src/reconstruct/intra_block.rs`
> and `tpt-kinetix-av1/src/reconstruct/reconstruct_block.rs` (widened
> `KINETIX_AV1_DBG` trace conditions, opt-in only, kept for future sessions),
> `tpt-kinetix-av1/src/loop_filter.rs` (widened debug dump row range to also
> cover rows 32-47, opt-in only, kept), `tpt-kinetix-test-utils/tests/dbg_av1_smptebars.rs`
> (added columns-32-63 and columns-48-63/rows-32-47 dumps, kept — still not
> part of the permanent suite). No `git commit` calls were made this session.

> **2026-08-18 session note (cont'd again) — root-caused the row-42
> horizontal-deblock lead (wrong tx axis + max-instead-of-min), and found
> three further spec-divergences in `filter_line_1d` while re-checking
> §7.14.6 for the assigned task.** Picked up the explicit next lead: the
> `dbg_av1_smptebars` crop's worst remaining block (max diff 20, columns
> 48-63/rows 32-47), diagnosed by the previous note as a horizontal-edge
> deblock bug where flat pre-filter content became a smooth gradient
> post-deblock.
>
> **Root cause #1 (the assigned lead itself), in `loop_filter.rs`'s
> `FrameMeta`/`deblock_plane`**: §7.14.3's `filterSize` derivation computes
> `baseSize` from `Tx_Width` for vertical edges (pass 0) but `Tx_Height` for
> *horizontal* edges (pass 1) — confirmed directly from the spec PDF
> (`pdftotext -layout`, downloaded fresh this session since no local copy
> survived from prior sessions; per house style, table-shaped spec text is
> transcribed from this PDF directly, not from a fetched/summarized page).
> `FrameMeta` only ever stored one tx-size value per 8×8 cell — the
> transform's *width* (`luma_tx_w as u8` in `intra_block.rs`) — reused for
> both passes. For a `TX_16X4` block (wide but only 4 samples tall), the
> horizontal-edge `filterSize` was therefore derived from `16` instead of
> the transform's real `4`, wrongly qualifying the 13-tap wide filter
> (`reach = 6`) for an edge a short transform never actually spans that far
> across — exactly the mechanism the previous session's note predicted.
> Separately, `deblock_plane` also combined the two straddling transform
> sizes with `.max(left, right)` — spec says `baseSize = Min(...)`, the
> *smaller* of the two, not the larger; using `max` compounds the same
> failure mode (a large neighbouring transform inflating a small transform's
> edge to a bigger filter than it should ever get). Also fixed
> `filter_size_from_tx_samples` itself: it bucketed into `{4, 8, 16}`
> ignoring `plane`, but §7.14.3 caps chroma at `Min(8, baseSize)` vs luma's
> `Min(16, baseSize)` — a `>=16`-sample chroma transform could wrongly reach
> `filterSize = 16`.
>
> **Fix**: `FrameMeta` now tracks `luma_tx_w`/`luma_tx_h` (and
> `u_tx_w`/`u_tx_h`, `v_tx_w`/`v_tx_h`) independently instead of one shared
> field; `intra_block.rs`'s `record_luma`/`record_chroma` call sites pass
> both axes (`luma_tx_h as u8`, `av1::TX_HEIGHT[c_tx] as u8`, previously only
> the width was ever recorded for chroma too). `deblock_plane`'s vertical
> pass now reads `tx_w_grid` with `.min(left, right)`; the horizontal pass
> now reads a separate `tx_h_grid`, also with `.min`. `filter_size_from_tx_samples`
> now takes `plane` and does a direct `tx_samples.min(cap)` (16 luma / 8
> chroma) instead of an un-plane-aware bucket.
>
> **Further findings while re-checking §7.14.6.2/6.4 against the code (per
> the task's explicit instruction to check flat/flat2 mask derivation and
> filter-size decision) — three more real, independent bugs, all in
> `filter_line_1d`:**
>
> 1. **`filterMask` was a summed-difference heuristic, not the spec's
>    per-tap formula.** The code computed `mask7 = Σ|adjacent taps| <=
>    blimit && mask4 = |p1-p0|+|q1-q0| <= limit` — a structurally different,
>    VP9-style approximation. The spec (§7.14.6.2) computes `Abs(p1-p0) >
>    limit`, `Abs(q1-q0) > limit`, `Abs(p0-q0)*2 + Abs(p1-q1)/2 > blimit`,
>    plus `Abs(p2-p1)`/`Abs(q2-q1)` checks gated on `filterLen >= 6` and
>    `Abs(p3-p2)`/`Abs(q3-q2)` gated on `filterLen >= 8` — where `filterLen`
>    itself depends on `plane` (chroma caps at 6 regardless of `filterSize`).
>    The old formula also completely ignored `plane`/`filterLen`, applying
>    identical tap-count logic to an 8-bit chroma edge and a 16-tap luma
>    edge. Concretely: a 72-magnitude step (`128 -> 200`) at `blimit = 80`
>    passed the old formula's threshold but fails the spec's real combined
>    term (`72*2 + 72/2 = 180 > 80`) — real AV1 would never filter an edge
>    that steep at that `blimit`, but this decoder did.
> 2. **`flat`/`flat2` compared the wrong things.** The code checked
>    `|p_k - q_k| <= bd_flat` for `k` in the relevant reach — i.e. whether
>    the two sides of the edge are close *to each other*. The spec
>    (§7.14.6.2) checks `|p_k - p0| <= threshold` and `|q_k - q0| <=
>    threshold` — i.e. whether each side is close *to its own boundary
>    sample*, entirely independent of what the other side's values are.
>    These are different conditions: a symmetric "notch" shape (values
>    `200,200,100,100 | 100,100,200,200`, edge in the middle) is wrongly
>    called flat by the old cross-boundary formula (every `p_k` happens to
>    equal the mirrored `q_k`) even though neither side is anywhere near its
>    own boundary sample — the old formula would wide-filter genuinely
>    non-flat content whenever it happened to be edge-symmetric, and could
>    just as easily reject genuinely flat-but-asymmetric content the other
>    way. (This specific formula error did not turn out to be what produced
>    the row-42 gradient artifact — that was root cause #1 above, confirmed
>    by testing the `filterSize` fix in isolation first and rerunning the
>    crop before touching this code — but it's a real, independently
>    spec-verified defect the task explicitly asked to check for.)
> 3. **The wide filter's chroma tap count (`n`) was wrong.** §7.14.6.4: `n =
>    6` when `log2Size == 4`; otherwise `n = 3` for luma but `n = 2` for
>    chroma (`log2Size == 3, plane > 0`). The code used `n = 3` for every
>    `log2 != 4` case regardless of `plane`, so chroma's 8-tap wide filter
>    read and wrote one tap farther (`p2`/`q2`) than the spec allows.
>
> Also hardened `filter_line_1d`'s `get()` tap accessor: it used to return a
> hardcoded `0` for any out-of-range offset (relevant now that the wide
> filter's `log2Size == 4` branch reaches out to `p6`/`q6`, 7 samples from
> the edge, which routinely runs past a small crop's plane boundary); it now
> clamps to the plane's own edge sample, matching how `CurrFrame` is only
> ever indexed within the real frame extent in the spec's process — an
> injected `0` at a plane edge would fabricate a fake hard-black edge that
> doesn't exist in the real frame.
>
> **Regression tests added** in `loop_filter.rs`:
> `filter_size_from_tx_samples_caps_by_plane_not_by_bucket`,
> `frame_meta_tracks_tx_width_and_height_independently`,
> `filter_mask_matches_spec_per_tap_formula_not_summed_heuristic`,
> `flat_mask_checks_each_sides_own_flatness_not_cross_boundary_equality`,
> `chroma_wide_filter_uses_two_taps_not_three` — all with hand-computed
> expected values checked against the spec formulas directly (shown in each
> test's comments), following the existing `narrow_filter_*` test pattern.
> Two pre-existing tests (`filter_line_smooths_a_step_edge`,
> `narrow_filter_reduces_single_discontinuity`) needed their input
> magnitudes reduced — they used step/discontinuity sizes that only passed
> the old, wrong `filterMask` formula and correctly fail the new spec-exact
> one; adjusted to a smaller, `filterMask`-passing step while keeping the
> same test intent (verify the edge gets smoothed, not amplified).
>
> **Impact measured**: `cargo test -p tpt-kinetix-av1 --lib` is green,
> 84/84 (was 79/79; +5 net new regression tests, 2 adjusted). The
> `dbg_av1_smptebars` per-16×16-block max-abs-diff map went from
> `[2,0,1,2] / [2,0,1,2] / [3,2,2,20] / [3,1,1,5]` to `[2,0,1,2] / [2,0,1,2]
> / [3,2,2,2] / [3,1,1,4]` after fixing root cause #1 (`filter_size` tx-axis
> bug) — the targeted block (row 2, col 3 — columns 48-63/rows 32-47) went
> from a max diff of 20 down to 2, confirming the lead's diagnosis was
> correct and the fix directly addresses it; the adjacent block (row 3, col
> 3) also improved 5→4. Fixing the three further `filter_line_1d` spec
> divergences (`filterMask`, `flat`/`flat2`, chroma `n`) afterward moved the
> crop only marginally further (no visible change in the printed diff
> grid — these bugs are real but apparently don't trigger detectably
> differently on this specific 64×64 crop's content) but did move
> whole-corpus chroma PSNR: `testsrc_128x96` U/V 9.72/10.00 → 11.22/10.89 dB,
> `mandelbrot_128x96` U/V 16.84/16.06 → 17.96/16.57 dB (both improved
> noticeably). Luma PSNR across the whole corpus barely moved (as with
> every fix this bug hunt has found so far): `testsrc_128x96` 11.03→11.03 dB,
> `mandelbrot_128x96` 16.63→16.64 dB, `smptebars_256x144` 10.15→10.15 dB,
> `testsrc2_320x180` 11.13→11.13 dB; `solid_red_32`/`_64` unaffected (99.00
> dB, as always — solid content never has a real transform-size or
> mask-shape edge case to expose). **Same pattern as every fix this bug hunt
> has found: real, spec-verified bugs with clean, hand-verified before/after
> wins (a targeted crop block improving 20→2, meaningful chroma PSNR gains),
> but luma PSNR on the larger multi-superblock corpus is still dominated by
> other, stacked, still-open sources of error.**
>
> **What's still open for the next session**:
> - Luma PSNR is still noise-level on every non-solid corpus entry despite
>   five consecutive sessions each finding and fixing a real, independently
>   confirmed bug. This strongly suggests there is at least one more
>   systemic bug (likely in prediction, coefficient decode, or transform,
>   given deblock/CDEF have now had two dedicated sessions each finding real
>   defects with only modest aggregate impact) still undiscovered. The
>   productive method continues to be picking one specific still-wrong
>   pixel/block in a small crop and tracing it through every stage — this
>   session's per-block diff map (`[2,0,1,2] / [2,0,1,2] / [3,2,2,2] /
>   [3,1,1,4]`) shows the 64×64 `dbg_av1_smptebars` crop itself is now
>   nearly clean (worst block off by 4), so the next productive crop is
>   likely a *larger* or *different* test source (the corpus's actual
>   256×144/128×96/320×180 entries, not the 64×64 debug crop, which may no
>   longer be representative of the corpus's dominant remaining error now
>   that its own worst blocks are fixed).
> - `flat`/`flat2`'s cross-boundary-vs-own-side fix (finding #2 above) is
>   spec-verified via hand computation and a dedicated unit test, but its
>   *aggregate* impact wasn't isolated separately from the other two
>   `filter_line_1d` fixes in the same commit — if a future session wants to
>   bisect which of the three contributed how much to the chroma PSNR gain,
>   they weren't measured independently this session (all three were fixed
>   together before the next PSNR check, per the task's per-fix
>   measure-then-iterate guidance being interpreted at the "root cause"
>   granularity rather than "every individual formula term" granularity —
>   worth reconsidering if isolating exact attribution matters later).
> - Chroma direction re-derivation in `cdef_plane_chroma` (flagged unvalidated
>   by an earlier session) still hasn't been checked.
> - Loop restoration (§7.17) remains an explicit no-op passthrough.
> - Inter blocks (`reconstruct/inter_block.rs`) still never call
>   `meta.record_luma`/`record_chroma` at all (confirmed by grep this
>   session) — every inter-coded block's 8×8 cells keep `FrameMeta::new`'s
>   default `tx_w = tx_h = 0`, which `filter_size_from_tx_samples(0, plane)`
>   maps to `filterSize = 0`, not a sane default — this decoder's corpus is
>   keyframe-only so far so it hasn't mattered yet, but will need fixing
>   before inter-frame deblocking can be trusted.
>
> Uncommitted working-tree files this session: `tpt-kinetix-av1/src/loop_filter.rs`
> (all the fixes and new/adjusted tests above), `tpt-kinetix-av1/src/reconstruct/intra_block.rs`
> (`record_luma`/`record_chroma` call sites now pass both tx axes). No
> `git commit` calls were made this session; `tpt-kinetix-h264/src/slice_data/ctx.rs`
> appearing modified in `git status` alongside this session's files is the
> other concurrent automated process mentioned in prior notes, not this
> session's work.

> **2026-08-18 session note (new crop, two more real bugs: a missing
> partition sub-block that desynced the entropy decoder, and a wrong
> transform-type CDF context for filter-intra blocks).** Per the previous
> session's explicit handoff, retired the exhausted `dbg_av1_smptebars` 64x64
> crop (worst block now off by only 4) and built a new harness,
> `tpt-kinetix-test-utils/tests/dbg_av1_testsrc128.rs`, targeting
> `testsrc_128x96` — one of the four corpus entries still stuck at
> noise-level luma PSNR (`av1_intra_corpus`'s `"testsrc"` entry, 128x96,
> reference via `decode_av1_obu_with_dav1d`, which on this machine falls back
> to `ffmpeg`'s built-in `libdav1d` since no standalone `dav1d` binary is
> installed — confirmed working via `ffmpeg -decoders | grep dav1d`).
>
> **First finding, via the new crop's per-16x16-block diff map**: the
> top-left 16x16 region's rows 8-15 decoded as flat `128` — the exact
> "neutral fill" value every plane buffer is initialized to
> (`reconstruct/mod.rs`'s `vec![128u8; ...]`) before any block writes its
> pixels. This is the signature of a region the partition tree never visited
> at all, not a wrong-value bug.
>
> **Root cause #1, in `reconstruct/partition.rs`'s `split_into_subblocks`**:
> confirmed via a temporary `KINETIX_AV1_DBG_PART`-gated trace of every
> `decode_partition` node (kept in the tree, opt-in) that a `BLOCK_16X16`
> node at `mi=(0,0)` decoded `partition = PARTITION_HORZ_B` (5) but only
> produced **two** sub-blocks (`subs = [(BLOCK_8X8, 0, 0), (BLOCK_16X4, 3, 0)]`)
> instead of the three the AV1 spec's real content requires. Downloaded the
> spec PDF fresh this session (`pdftotext -layout`, per house style — no
> local copy survived from prior sessions) and confirmed directly from
> `decode_partition()`'s pseudocode (spec page 62): `PARTITION_HORZ_A`/
> `_HORZ_B`/`_VERT_A`/`_VERT_B` each call `decode_block()` **three** times,
> using `subSize = Partition_Subsize[partition][bSize]` (the plain HORZ/VERT
> half-shape, `bw x hh` or `hw x bh`) for the "whole" piece and
> `splitSize = Partition_Subsize[PARTITION_SPLIT][bSize]` (the `hw x hh`
> quarter-area shape) for the two "split" pieces — **not** the `qh`/`3*qh`
> (`qw`/`3*qw`) quarter/three-quarter split the code used. Concretely, the
> old `PARTITION_HORZ_B` implementation pushed only 2 entries: `(bw, 3*qh)`
> at `(0,0)` and `(bw, qh)` at `(3*qh, 0)`. For a 16x16 node, `(bw, 3*qh) =
> (16, 12)` — **not a real `BLOCK_SIZES` entry** (AV1 has no 16x12 block) —
> so `bsize_from_wh`'s linear search silently fell through to its
> not-found default, `BLOCK_8X8`. This exactly matches the observed trace
> (`subs = [(BLOCK_8X8, 0, 0), ...]`, the fallback value). Reading only 2
> `decode_block()` calls where the real bitstream encoded 3 blocks' worth of
> mode/residual syntax **desyncs the entropy decoder for the rest of the
> tile** — a strictly more severe failure mode than the "plausible but
> wrong" corruption prior sessions found (`INTRA_MODE_CONTEXT`, `all_zero_ctx`),
> since here decode doesn't even stay self-consistent afterward, it just
> silently drops real bitstream content.
>
> **Fix**: rewrote `PARTITION_HORZ_A`/`_HORZ_B`/`_VERT_A`/`_VERT_B` in
> `split_into_subblocks` to emit exactly 3 sub-blocks each, using only
> `hw`/`hh` (never `qh`/`qw`, which remain correct and unchanged for
> `HORZ_4`/`VERT_4`'s genuinely-quarter shapes): `HORZ_A` = `(hw,hh)@(0,0)`,
> `(hw,hh)@(0,hw)`, `(bw,hh)@(hh,0)`; `HORZ_B` = `(bw,hh)@(0,0)`,
> `(hw,hh)@(hh,0)`, `(hw,hh)@(hh,hw)`; `VERT_A`/`VERT_B` are the transpose.
>
> **Regression tests** added in `reconstruct/partition.rs`'s new
> `#[cfg(test)] mod tests`: one per A/B partition type at a 16x16 node
> (`horz_a_produces_three_subblocks_matching_spec_split_and_horz_shapes` etc.,
> hand-computed against the spec pseudocode) plus
> `horz_a_scales_to_a_32x32_node` confirming the shapes scale correctly at a
> different node size (not just the one size happened to test right).
>
> **Impact measured**: the `testsrc_128x96` crop's top-left 16x16 region no
> longer shows the `128`-neutral-fill gap (every pixel in the region is now
> some real decoded value, confirmed by re-dumping the crop) — the missing-
> sub-block desync is gone. Re-running the same `KINETIX_AV1_DBG_PART` trace
> after the fix shows the same `mi=(0,0)` node's `PARTITION_HORZ_B` now
> correctly producing `subs = [(BLOCK_16X8, 0, 0), (BLOCK_8X8, 2, 0),
> (BLOCK_8X8, 2, 2)]`. Confirmed no regression on the now-nearly-clean
> `dbg_av1_smptebars` crop (unchanged `[2,0,1,2]/[2,0,1,2]/[3,2,2,2]/[3,1,1,4]`
> — that content apparently never selects a HORZ_A/HORZ_B/VERT_A/VERT_B
> partition, so the bug was invisible there, consistent with why 5 prior
> sessions mining that crop never found it). `cargo test -p tpt-kinetix-av1
> --lib` green, 89/89 (was 84/84; +5 new tests). Whole-corpus
> `av1_psnr_check` barely moved and even regressed slightly on some entries
> (`testsrc_128x96` 11.03→10.46, `mandelbrot_128x96` 16.64→14.55,
> `smptebars_256x144` 10.15→8.38, `testsrc2_320x180` 11.13→10.92 dB) —
> **this is expected and does not indicate the fix is wrong**: previously,
> hitting this bug meant the rest of the tile's entropy stream was
> completely desynced (silently reading whichever symbols happened to fall
> next, producing a *different* kind of garbage), so "fixing the desync"
> doesn't move decode from garbage to correct in one step, it moves decode
> from *one flavor* of garbage to *another*, correctly-synced-but-still-
> affected-by-other-bugs flavor. The fix is unambiguously correct per the
> spec pseudocode and structurally necessary (a decoder that drops 1 of every
> 3 sub-blocks for a whole partition family cannot ever be pixel-exact on
> content that uses it), independent of which direction any one corpus
> entry's PSNR moved.
>
> **Root cause #2, found while tracing the `testsrc_128x96` crop's very
> first transform block after fix #1** (`mi=(0,0)`, `BLOCK_16X8`,
> `y_mode=DC_PRED`, `filter_intra_mode=Some(2)` — i.e. `FILTER_H_PRED`), **in
> `reconstruct/intra_block.rs`'s `reconstruct_intra_subblock`**: the luma
> `TxBlockCtx.intra_dir` (used to select the `intra_tx_type` CDF context, AV1
> spec "Parsing process" §`intra_tx_type`) was set to `y_mode` unconditionally.
> The spec's actual derivation (page 381, confirmed via the same
> freshly-downloaded PDF): `intraDir = Filter_Intra_Mode_To_Intra_Dir[
> filter_intra_mode ]` when `use_filter_intra` is set — table `{DC_PRED,
> V_PRED, H_PRED, D157_PRED, DC_PRED}` — and only falls back to `YMode`
> directly when filter-intra is *not* used. Since `filter_intra_mode_info()`
> is only ever read when `YMode == DC_PRED` (spec-gated), `y_mode` is always
> `DC_PRED` (0) for every filter-intra block — so using it directly instead
> of mapping through the table meant **every filter-intra block in every
> frame** read `intra_tx_type` from the `DC_PRED`-indexed CDF bucket
> regardless of which of the 5 real filter-intra modes was actually in use
> (e.g. this crop's first block, `filter_intra_mode = FILTER_H_PRED` (2),
> should have used the `H_PRED`-indexed bucket). This crate's `testsrc`/
> `mandelbrot` corpus decodes filter-intra for most of its low-detail
> top-left blocks (confirmed via the `KINETIX_AV1_DBG` trace: `filter_intra
> = Some(0/2/3/4)` on the majority of the first ~10 blocks of
> `testsrc_128x96`), so this is a high-frequency real-content bug, not an
> edge case — the same "plausible-symbol, wrong-CDF-context" corruption
> signature as the `INTRA_MODE_CONTEXT` bug two sessions ago, just in a
> different table.
>
> **Fix**: added `FILTER_INTRA_MODE_TO_INTRA_DIR` (`[DC_PRED, V_PRED,
> H_PRED, D157_PRED, DC_PRED]`) in `reconstruct/intra_block.rs` and compute
> `luma_intra_dir` from it whenever `filter_intra_mode.is_some()`, falling
> back to `y_mode` otherwise; `TxBlockCtx.intra_dir` now uses
> `luma_intra_dir` instead of `y_mode` directly. (Chroma's `intra_dir` —
> actually `uv_mode` at `intra_block.rs:407` — is unaffected: chroma's
> `compute_tx_type` doesn't consult `filter_intra_mode` at all per spec,
> filter-intra is a luma-only feature.)
>
> **Regression coverage**: not yet added as a dedicated unit test this
> session (open item below) — the fix was confirmed via the
> `KINETIX_AV1_DBG` trace directly (`tx_type` for the crop's first block
> changed from `0` (`DCT_DCT`, the `DC_PRED`-bucket outcome) to `11`
> (`H_DCT`) after the fix, and `eob` changed `30→23`, confirming the CDF
> context genuinely changed which symbol got decoded) rather than a
> hand-computed `#[test]`; `cargo test -p tpt-kinetix-av1 --lib` stayed
> green at 89/89 throughout (no test exercised this path before or after).
>
> **Impact measured**: `av1_psnr_check` after both fixes:
> `testsrc_128x96` Y/U/V 10.58/11.37/9.78 dB, `mandelbrot_128x96`
> 15.80/17.68/15.53 dB, `smptebars_256x144` 10.03/13.98/10.41 dB,
> `testsrc2_320x180` 10.64/10.44/9.81 dB, `solid_red_32`/`_64` unchanged
> (99.00 dB, as always). Still noise-level luma across the board, same
> "real fix, no aggregate corpus win yet" pattern as every session in this
> hunt.
>
> **Deep-dived the crop's first transform block further to understand why
> the fix didn't move the needle (not fully resolved — the concrete next
> lead)**: post-fix, `mi=(0,0)`'s first 8x8 luma tx block decodes
> `tx_type=11` (`H_DCT`), `eob=23`, but the *actual* dequantized coefficients
> are sparse and small (`quant = [...]` mostly zero with a handful of `±1`
> entries at scan positions 26/50/56/57, dumped via a new
> `KINETIX_AV1_DBG_FULL`-gated print in `reconstruct_block.rs`, opt-in,
> kept) — producing a residual in the range of roughly `±20` at most. The
> reference decoder's true pixel value for this entire 16x16 region is a
> flat `16`; this decoder's prediction for the block (no left/above
> neighbour — first block in the tile) is `129` (matches spec: `DC_PRED`/
> filter-intra with no neighbours predicts near the 8-bit neutral value,
> which the *reference* decoder would also compute, since neither decoder
> has real edge pixels to work from here). Getting from a `~129` prediction
> to a true `16` value requires a residual with mean around `-113` — far
> larger than what this block's decoded coefficients produce. **This means
> either (a) the coefficient magnitudes for this block are still being
> decoded wrong (a further coefficient-context or CDF bug, not yet
> isolated), or (b) the block's mode/skip/filter-intra syntax itself is
> still being misdecoded earlier than the coefficients — i.e. there is at
> least one more real bug upstream of this point that the two fixes above
> didn't touch.** Not root-caused this session; concretely worth checking
> next: (1) whether `filter_intra_mode_info()`'s own read (gated on
> `enable_filter_intra && y_mode == DC_PRED && max(w,h) <= 32`, spec
> §5.11.24) is being read at the exact right bitstream position — i.e.
> whether everything *before* it in `intra_frame_mode_info()`'s syntax order
> (segment_id, skip, `intra_frame_y_mode`, `angle_delta_y` gating, `uv_mode`,
> `cfl_alpha`, `angle_delta_uv`, `palette_mode_info`) is correct for this
> specific block, by cross-checking against a from-scratch symbol-by-symbol
> spec walk of the raw bits (the `KINETIX_AV1_DBG_PART`/`KINETIX_AV1_DBG`/
> `KINETIX_AV1_DBG_FULL` traces added this session, all still in the tree
> opt-in, should make this tractable); (2) whether `H_DCT`'s scan table
> (`get_scan(TX_8X8, H_DCT)`) and `get_tx_class`/row-vs-column dispatch in
> `coeff_tables.rs`/`transform.rs` are correct — this is the *first* real
> content this bug hunt has exercised a 1D (`H_DCT`/`V_DCT`) transform type
> with actual nonzero coefficients on (`dbg_av1_smptebars`'s worked example
> was `V_DCT` but flat/`eob=0`), so it's plausible but unconfirmed that the
> 1D-transform coefficient/scan path has its own undiscovered bug; (3)
> whether `qindex=128`'s dequant scale itself (`dequant[]` printed as mostly
> `0`/`176` in the trace) matches the spec's `Dc_Qlookup`/`Ac_Qlookup[128]`
> table entries exactly — not cross-checked against the spec table this
> session.
>
> Uncommitted working-tree files this session (in addition to the
> already-uncommitted files from prior sessions listed above, which this
> session did not touch): `tpt-kinetix-av1/src/reconstruct/partition.rs`
> (the HORZ_A/B/VERT_A/B fix + 5 new regression tests + the
> `KINETIX_AV1_DBG_PART` trace, opt-in, kept), `tpt-kinetix-av1/src/reconstruct/intra_block.rs`
> (the `Filter_Intra_Mode_To_Intra_Dir` fix), `tpt-kinetix-av1/src/reconstruct/mod.rs`
> (widened the frame-header `KINETIX_AV1_DBG` trace to also print
> `delta_q_present`/`delta_lf_present`/`segmentation_enabled`, opt-in, kept —
> used to rule out missing `read_cdef()`/`read_delta_qindex()`/
> `read_delta_lf()` calls as the cause of the still-open lead above; confirmed
> all three are `false`/`0`-bit for this specific corpus entry, so their
> absence is currently harmless, but they are still genuinely unimplemented
> and will matter for any content that enables them — not fixed this
> session), `tpt-kinetix-av1/src/reconstruct/reconstruct_block.rs` (the
> `KINETIX_AV1_DBG_FULL` full quant/residual dump for the crop's first
> block, opt-in, kept), `tpt-kinetix-test-utils/tests/dbg_av1_testsrc128.rs`
> (new, this session's debug harness — kept for the next session to continue
> the still-open lead above). No `git commit` calls were made this session.

> **2026-08-19 session note (exhaustive spec audit of the still-open
> `testsrc_128x96` first-block lead — no new bug found, but a real new
> empirical clue narrows the search space considerably).** Picked up the
> exact handoff from the previous session: `testsrc_128x96`'s first luma tx
> block (`mi=(0,0)`, `BLOCK_16X8`, `filter_intra_mode=Some(2)`/`FILTER_H_PRED`,
> `tx_type=H_DCT`, `eob=23`) decodes coefficients too small/sparse (`quant`
> nonzero only at raster 26/50/56/57, all `±1`) to explain the ~`-113`
> residual needed to turn the ~129 no-neighbour prediction into the
> reference's true flat `16`. Re-confirmed this is still exactly the
> situation via the existing `dbg_av1_testsrc128.rs` harness before doing
> anything else.
>
> **What was checked this session, line-by-line against a freshly
> `pdftotext -layout`'d spec PDF (no cached copy survived from prior
> sessions' scratch dirs), all found to already match spec exactly (i.e.
> ruled out, not just "looked fine"):** the `H_DCT`/`V_DCT` row/column
> transform-kind dispatch and `Transform_Row_Shift` table in
> `reconstruct/transform.rs` (spec §7.13.3 — `H_DCT` is DCT-along-rows +
> identity-along-columns, confirmed against the spec's literal
> `PlaneTxType` membership lists, not just naming intuition); `get_scan`'s
> `mrow`/`mcol` selection and the `Mcol_Scan_8x8`/`Mrow_Scan_8x8` tables
> (`coeff_tables.rs`, spec §5.11.41); `get_coeff_base_ctx`/`coeff_base_ctx`
> including `Coeff_Base_Ctx_Offset`/`Sig_Ref_Diff_Offset` neighbour-offset
> tables (spec §8.3.2); `coeff_br_ctx`/`Mag_Ref_Offset_With_Tx_Class`; the
> full `read_eob`/`eob_pt_*`/`eob_extra` bit-exact reconstruction against
> spec's `coeffs()` pseudocode; the `dc_sign`/Exp-Golomb tail/`& 0xFFFFF`
> masking order in `read_coeffs`; the core arithmetic/CDF-adaptation engine
> in `entropy.rs::read_symbol` (spec §8.2.6, including the `rate`/`tmp`
> adaptation formula and renormalization steps) — the same engine
> `solid_red`'s bit-exact decode already indirectly validates for simple
> cases, but this checked it against the *general* N-ary path coefficient
> reads exercise; `partition_context`/`tx_depth_context`'s CDF-context
> derivations (both evaluate to `ctx=0` for this specific first-in-tile
> block since `AvailU`/`AvailL` are both false, so a bug there couldn't
> explain *this* block regardless, but the formulas were confirmed correct
> anyway for future blocks); `palette_mode_info`'s block-size/`bsizeCtx`
> gating and `has_palette_y`/`has_palette_uv` CDF selection (spec §5.11.46);
> `filter_intra_mode_info`'s gate condition and the
> `Filter_Intra_Mode_To_Intra_Dir` mapping fixed last session (re-verified,
> still correct); `predict_filter_intra`'s recursive-prediction math (spec
> §7.11.2.3) against the `Intra_Filter_Taps` application and `AboveRow`/
> `LeftCol`-via-`tl` edge defaults; `intra_frame_mode_info()`'s full syntax
> order (segment_id → skip → y_mode → angle_delta_y → uv_mode → cfl_alpha →
> angle_delta_uv → palette_mode_info → filter_intra_mode_info, spec
> §5.11.7); `TX_SIZE_SQR`/`TX_SIZE_SQR_UP`/`ADJUSTED_TX_SIZE` table
> transcriptions; the `DEFAULT_INTRA_TX_TYPE_SET1_CDF`/
> `DEFAULT_FILTER_INTRA_CDF`/`DEFAULT_FILTER_INTRA_MODE_CDF`/
> `DEFAULT_PALETTE_Y_MODE_CDF` numeric default-table entries actually used
> by this block's specific context indices (spot-checked against the
> spec's literal array text, not just dimension counts); `AC_QLOOKUP_8[128]`
> (`=176`, matches the trace's dequant value exactly). Also confirmed
> `allow_intrabc=false` for this content (so the `use_intrabc` symbol read —
> present in `intra_frame_mode_info()`'s spec pseudocode but never
> implemented in `decode_intra_block` — is correctly never reached here;
> still a real gap for any content that *does* set `allow_intrabc`, not
> fixed this session, added to open items below).
>
> **New empirical finding (the actual contribution this session, in lieu of
> a fix): built a second debug harness,
> `tpt-kinetix-test-utils/tests/dbg_av1_mandelbrot128.rs`, and compared
> `mandelbrot_128x96`'s first 8x8 luma block against its dav1d reference.**
> Unlike `testsrc`'s catastrophically-wrong first block, `mandelbrot`'s
> first block (`BLOCK_16X16`, plain `DC_PRED`, `filter_intra=None`, `skip`-
> like/`eob=0`) decodes **essentially pixel-exact** — every sample matches
> the reference to within 0-2/255, e.g. row 6: kinetix
> `[140,140,141,142,143,144,145,145]` vs ref
> `[140,140,141,142,142,143,144,146]`. This rules out several standing
> hypotheses at a stroke: it can't be a universal arithmetic-decoder or
> coefficient-engine bug (both blocks go through the identical
> `read_symbol`/`read_coeffs` machinery), and — since `mandelbrot`'s
> `frame_header` has `allow_screen_content_tools=false` (confirmed via a
> widened `KINETIX_AV1_DBG` frame-header trace that now also prints
> `allow_screen_content_tools`/`allow_intrabc`/`enable_filter_intra`/
> `reduced_tx_set`) while `testsrc`'s has it `true` — it also weakens (but
> doesn't fully kill) the "palette-adjacent bug" theory, since `mandelbrot`
> never even reaches `palette_mode_info()`'s body while `testsrc` does (one
> extra `has_palette_y` symbol read, which decoded the highly-probable
> `false` outcome and looks self-consistent). Both corpus entries *do*
> eventually diverge from the reference (`mandelbrot`'s own trace shows
> implausible-looking sample jumps by `mi=(0,8)`/`px=(32,8)`, e.g. `top=
> [48,64,81,141]` where a smooth fractal gradient should never jump like
> that) — just `testsrc` diverges from literally the very first block while
> `mandelbrot` survives several clean blocks first. This "eventually
> desyncs on real content, but not instantly and not universally" signature
> matches the same family as the `INTRA_MODE_CONTEXT`/
> `Filter_Intra_Mode_To_Intra_Dir` bugs fixed in earlier sessions (a wrong-
> but-plausible CDF context or default-table entry for *some* specific
> context combination, not a structural missing/extra symbol read) more
> than it matches a hard desync — but the specific trigger (does it need
> `filter_intra=true`? a specific `y_mode`/`tx_type` combination? something
> about non-square/non-`TX_8X8` sizes?) is not yet isolated, since
> `mandelbrot`'s first few blocks happen not to exercise `filter_intra`
> at all (checked: none of its `mi_row<4` blocks show `filter_intra=Some`),
> so a direct side-by-side of "same feature, one clean one broken" wasn't
> available this session.
>
> **Debug instrumentation added and kept (all opt-in, zero cost when the
> env vars are unset):** `entropy.rs::SymbolDecoder::dbg_bit_pos()` (returns
> `(bit_pos, symbol_max_bits, data_len_bits)` — used to confirm the decoder
> is nowhere near running out of real tile data by the time it reaches the
> first coefficient read, `bit_pos=35` out of `4168` bits available, ruling
> out gross bitstream exhaustion as the desync mechanism); a
> `KINETIX_AV1_DBG` print of `dec.dbg_bit_pos()` immediately before every
> `read_coeffs()` call in `reconstruct_block.rs`; the widened frame-header
> trace in `reconstruct/mod.rs` noted above; the new
> `dbg_av1_mandelbrot128.rs` harness.
>
> **Impact measured**: no functional code changed this session (only
> diagnostics), so both `cargo test -p tpt-kinetix-av1 --lib` (89/89, same
> as before) and `av1_psnr_check` (`testsrc_128x96` 10.58/11.37/9.78,
> `mandelbrot_128x96` 15.80/17.68/15.53, `smptebars_256x144`
> 10.03/13.98/10.41, `testsrc2_320x180` 10.64/10.44/9.81,
> `solid_red_32`/`_64` 99.00 dB — all bit-for-bit identical to the previous
> session's numbers, as expected).
>
> **Concretely worth trying next**, in priority order: (1) find or engineer
> a corpus entry/crop where the *same* coded block decodes once with
> `filter_intra=true` and once with `filter_intra=false` (or otherwise
> isolate the feature axis) to get a clean "broken vs clean" pair the way
> `mandelbrot`'s block 1 vs `testsrc`'s block 1 almost gives us, but without
> the confound of `testsrc` also using palette syntax and a different
> `bsize`/`tx_size`; (2) instrument `mandelbrot`'s own first real desync
> point (somewhere around `mi=(0,8)`/`px=(32,8)`, bit_pos ~581-588 per this
> session's trace) with the same before/after-symbol tracing method that
> worked for the HORZ_A/B and `Filter_Intra_Mode_To_Intra_Dir` bugs in
> prior sessions — since it's a *later*, more-isolated-looking desync than
> `testsrc`'s instant one, it may be easier to root-cause and could well be
> the same underlying bug; (3) the `use_intrabc` gap noted above (never
> read even when `allow_intrabc` is true) is a confirmed, if not-yet-hit-by-
> this-corpus, real missing-symbol-read bug — worth fixing on principle
> even before it's confirmed to explain any corpus entry's PSNR, since a
> missing read is exactly the failure class every desync bug in this hunt
> has turned out to be; (4) not re-attempted this session:
> hand-decoding the raw OBU bits for `testsrc`'s first block via an
> independent from-scratch reimplementation (e.g. a Python re-transcription
> of `entropy.rs` + the relevant CDF tables) to get a ground-truth symbol
> sequence to diff against — every other avenue this session tried was a
> *static* spec-conformance check, which exhaustively confirmed the code
> matches the spec text but cannot rule out a bug in a spec-conformant-
> looking default CDF *table value* the session didn't happen to spot-check
> numerically (only a handful of the ~30 default CDF tables touched by this
> block's decode path were spot-checked against literal spec numbers this
> session, not all of them).
>
> No `git commit` calls were made this session; the only uncommitted
> changes beyond prior sessions' are the debug instrumentation listed above
> (`tpt-kinetix-av1/src/entropy.rs`, `tpt-kinetix-av1/src/reconstruct/mod.rs`,
> `tpt-kinetix-av1/src/reconstruct/reconstruct_block.rs`,
> `tpt-kinetix-test-utils/tests/dbg_av1_mandelbrot128.rs` (new),
> `tpt-kinetix-test-utils/tests/dbg_av1_mandelbrot_diffmap.rs` (new — full-plane
> per-8×8-block mean-abs-diff heatmap + first-failing-pixel finder for
> `mandelbrot_128x96`, companion to `dbg_av1_mandelbrot128.rs`)) — no
> functional/behavioral code was touched.

> **2026-08-19 session note (cont'd — picked up the mi=(0,8)/px=(32,8) lead;
> found and fixed a real, spec-verified `predict_directional` gating bug;
> mandelbrot's own root cause is still open).** Started from the previous
> session's exact handoff: `dbg_av1_mandelbrot128.rs`'s trace showed
> `mandelbrot_128x96`'s block 3 (`mi=(0,8)`, `BLOCK_16X16`, `D207_PRED`
> a.k.a. nominal-angle-203, `eob=1`, `quant=[-1]`) as the first block whose
> own reconstruction visibly diverges from the dav1d reference, following
> two essentially-pixel-exact blocks before it.
>
> **New instrumentation used to localize the divergence precisely:** built
> `tpt-kinetix-test-utils/tests/dbg_av1_mandelbrot_diffmap.rs` (new), which
> prints (1) a per-8×8-block mean-abs-diff heatmap over the whole
> `80×64` luma plane, (2) a per-pixel diff zoom over a chosen region, and
> (3) actual kinetix-vs-ref sample rows. This pinned the real breakdown far
> more precisely than the block-level heatmap alone: `mi=(0,8)`'s own
> `16×16` region (`px` cols 32-47, rows 0-15) starts with only a small,
> smoothly-growing offset at row 0 (kinetix `[158,158,158,158,159,...]` vs
> ref `[160,160,161,161,161,...]`, both monotonic ramps, just offset by
> ~2) but by row 13 kinetix is pinned flat at ~172 while the reference
> actually *declines* sharply across the row (`[172,172,171,170,169,168,
> 167,165,163,161,159,156,152,146,141,134]`) — a real image feature
> (`quant`'s `eob=1` genuinely cannot represent a declining ramp; the
> decoded coefficient set is categorically too sparse for the true content,
> which reads as either a desynced/mis-contexted `eob`/`coeff_base` read
> somewhere at or before this block, or a wrong CDF context feeding into
> it). Also confirmed via a `KINETIX_AV1_NOFILTER` env-var gate added around
> `apply_post_filters` in `reconstruct/mod.rs` that disabling the in-loop
> deblock/CDEF filters leaves the diff heatmap and first-bad-pixel location
> unchanged (block-level heatmap values moved by at most 1, e.g. row16/bx48
> `67→68`) — ruling out loop-filter/CDEF as the desync's cause or even a
> significant contributor, confirming this is a reconstruction-path bug.
>
> **Extensive spec audit performed against a freshly `pdftotext -layout`'d
> spec PDF, all confirmed correct (ruled out) for this block's own decode
> path:** `row_axis_transform`/`col_axis_transform`'s `DCT_ADST` dispatch
> (`transform.rs`) re-checked line-by-line against spec §7.13.3's literal
> row/column transform-kind membership lists (`transform.rs:14664-14700` in
> the extracted text) — confirmed correct, including for `DCT_ADST`
> specifically (not just the previously-checked `H_DCT`); `Dr_Intra_
> Derivative[90]` table transcription (bit-for-bit against spec's flattened
> table); `dr_z3`'s (`predict.rs`) zone-3 `idx`/`base`/`shift`/`maxBaseY`
> formulas against spec §7.11.2.4 step 9, including the non-obvious
> `(X & 0x3F) >> 1 == (X >> 1) & 0x1F` bit-shuffle-order equivalence between
> the code's and spec's shift computation; `MODE_TO_TXFM` (`coeff_tables.rs`)
> transcribed and checked against spec's literal `Mode_To_Txfm` table for
> all 14 entries (previously only spot-checked); `all_zero`/`txb_skip`
> context derivation (`coeff.rs::all_zero_ctx`) against spec's literal
> pseudocode (`ctx = 0` whole-block special case, the `top`/`left`
> `Max`/`Min` clamps, the plane>0 OR-based context) — bit-for-bit match;
> `read_eob`'s `eob_pt` context (`get_tx_class(txType) != TX_CLASS_2D`)
> and the `eob_extra`/literal-bit reconstruction loop; `read_cfl_alphas()`
> (`mode_cdfs.rs`) — signs/magnitude/context formulas (`ctx = (signU-1)*3+
> signV` for U, `ctx = (signV-1)*3+signU` for V) matched spec's
> §8.3.2 CDF-selection tables exactly, including cross-checking the
> `DEFAULT_ANGLE_DELTA_CDF`/`Default_Angle_Delta_Cdf` table verbatim; the
> `intra_frame_mode_info()` read order including where `read_cdef()`/
> `read_delta_qindex()`/`read_delta_lf()` sit in spec's pseudocode — noted
> `read_cdef()` is never called anywhere in this crate (a real gap), but
> confirmed inert for this corpus since `cdef_bits=0` (its literal-bit read
> would consume zero bits regardless of whether it's called); `angle_delta_y`
> gating (`bsize >= BLOCK_8X8 && is_directional_mode`) and its binarization/
> context (`mode - V_PRED`); `palette_mode_info()`'s full gate (confirmed
> `allow_screen_content_tools=false` for `mandelbrot`, so it's provably
> never entered, zero bits either way).
>
> **The one real, concrete bug found and fixed this session** (in
> `predict_directional`, `tpt-kinetix-av1/src/reconstruct/predict.rs`):
> spec §7.11.2.4 step 4 gates the above-edge and left-edge intra-edge-filter
> sub-steps on `haveAbove`/`haveLeft` (actual sample availability at this
> block's position) — quote: "If haveAbove is equal to 1, the following
> steps apply: [strength selection + numPx + edge filter]" and symmetrically
> for `haveLeft`. The code instead gated both sub-steps on `need_above`/
> `need_left` — whether the block's *prediction zone* (1/2/3, from `pAngle`)
> structurally reads that edge at all — which is a different condition
> whenever a block's real availability diverges from its zone's edge
> requirement (e.g. any zone-2, i.e. `90 < pAngle < 180`, block — which
> always needs *both* edges structurally — sitting at the frame's top row,
> where `haveAbove` is actually `false`). Also missing: spec's `numPx` for
> each edge is `Min(w, maxX - x + 1) + …` / `Min(h, maxY - y + 1) + …` —
> clamped to the samples actually remaining before the frame/tile edge —
> not the unclamped `w`/`h` the code used, which could over-read past the
> frame edge for a transform block whose size doesn't evenly divide the
> remaining plane extent. Fixed by threading `have_above`/`have_left` (from
> the already-computed `BlockBorders`) and `avail_w`/`avail_h` (`plane_w -
> px_x`/`plane_h - px_y` from the caller in `reconstruct_block.rs`) through
> `predict_intra_block` into `predict_directional`, and using them for the
> edge-filter gate and the `n_px` clamp respectively (the *upsample*
> sub-step's gate and `numPx` were re-checked against spec and are correct
> as-is — spec doesn't clamp or re-gate `numPx` there).
>
> Added a hand-verified regression test,
> `reconstruct::tests::directional_edge_filter_gates_on_have_above_left_not_zone_need`
> (`tpt-kinetix-av1/src/reconstruct/tests.rs`): `D135_PRED` (zone-2, so
> `need_above == need_left == true` structurally) with `have_above ==
> have_left == false` and a deliberately jagged `top`/`left` (alternating
> `0`/`255`) must predict *bit-identically* whether `enable_intra_edge_filter`
> is on or off, since spec says neither edge-filter sub-step ever runs when
> neither side has real samples. `w + h < 24` (size 8) was chosen
> specifically to keep `filter_intra_edge_corner` (correctly gated on
> `need_above && need_left` per spec, *not* `haveAbove`/`haveLeft` — a
> separate, already-correct piece of the same spec step) out of play, so
> the test isolates only the bug this session fixed. Verified by hand: with
> the code reverted to the old `need_above`/`need_left` gate the test fails
> (`filtered != unfiltered`, e.g. first samples `128,96,128,...` vs
> `128,0,255,0,255,...`); with the fix in place it passes.
>
> **Investigated but ruled out as an explanation for *this specific*
> fix's real-world impact:** re-ran both `cargo test -p tpt-kinetix-av1
> --lib` (90/90, up from 89/89 — the one new test) and `av1_psnr_check`
> after the fix — every PSNR number is bit-for-bit identical to the
> pre-fix baseline (`solid_red_32`/`_64` 99.00/99.00, `testsrc_128x96`
> 10.58/11.37/9.78, `mandelbrot_128x96` 15.80/17.68/15.53,
> `smptebars_256x144` 10.03/13.98/10.41, `testsrc2_320x180`
> 10.64/10.44/9.81). Traced why: `mandelbrot`'s own `mi=(0,8)` block is
> zone-3 (`D207_PRED`, `pAngle > 180`), which only reads `have_left`
> (`need_left` and `have_left` are both `true` for this block — they only
> *coincide*, they don't diverge), and `have_above`/`need_above` are both
> `false` too (first frame row) — so for this one specific block the old
> and new gates happen to agree. The fix is real and spec-verified (proven
> by the regression test above), but it doesn't explain *this* corpus
> entry's desync; it would only visibly change output for a block whose
> `have_above`/`have_left` genuinely diverges from its zone's structural
> need (e.g. a zone-2 block at a frame edge, or a block whose transform
> size doesn't evenly divide the remaining plane extent for the `numPx`
> clamp) — not yet confirmed to occur anywhere in the current 5-entry
> corpus, but a real, previously-undetected conformance gap regardless.
>
> **`mandelbrot`'s real root cause is still open.** The most concrete
> remaining lead: `mi=(0,8)`'s `eob=1` is very likely genuinely wrong (too
> sparse to reconstruct the reference's real declining-gradient feature by
> row 13), meaning either an `eob_pt`/`coeff_base` symbol read earlier in
> the stream desynced (context or CDF-table numeric error not yet spotted
> despite the audit above), or a context feeding `all_zero`/`eob_pt` for
> *this specific block* is wrong in a way the spec-text audit didn't catch
> (e.g. a stale/wrong value in the `above_level`/`left_level` neighbour-
> context arrays carried over from block 1 or 2's own coefficient write-back,
> which was checked structurally but not numerically hand-traced against a
> ground truth). The from-scratch independent re-decode (session 6's open
> item 4) still hasn't been attempted and remains the most likely way to
> catch a numerically-wrong default-CDF-table entry or context-array bug
> that spec-text-only spot-checks keep missing across multiple sessions now.
>
> Instrumentation added and kept this session (all opt-in via env vars,
> zero cost when unset): `tpt-kinetix-test-utils/tests/
> dbg_av1_mandelbrot_diffmap.rs` (new); `KINETIX_AV1_NOFILTER` gate around
> `apply_post_filters` in `reconstruct/mod.rs`; `KINETIX_AV1_DBG_UV` trace
> in `reconstruct_block.rs` for chroma transform blocks (previously luma-
> only); widened the existing `KINETIX_AV1_DBG` luma trace's pixel-range
> gate from `px_x < 64` to `16..64` combined with `px_y < 32` to cover the
> `mi=(0,8)`-through-`mi=(0,16)` region this session focused on.
>
> No `git commit` calls were made this session. Functional changes:
> `tpt-kinetix-av1/src/reconstruct/predict.rs` (the `predict_directional`
> fix), `tpt-kinetix-av1/src/reconstruct/reconstruct_block.rs` (threading
> `avail_w`/`avail_h` through, plus the new UV debug trace),
> `tpt-kinetix-av1/src/reconstruct/tests.rs` (new regression test, plus
> updated call sites for `predict_intra_block`'s two new parameters, plus
> an unrelated pre-existing `clippy::unnecessary_min_or_max`/
> `clippy::manual_div_ceil` fix in an older test spotted while running
> `cargo clippy -p tpt-kinetix-av1 --all-targets -- -D warnings`, which now
> passes clean). `capabilities().pixel_exact` was not touched — still
> `false`, correctly, since none of this corpus is bit-exact yet.

> **2026-08-20 session note (Phase G.0 tooling: built the differential-trace
> harness; oracle deliberately deferred).** Scoped per the task brief:
> 7 straight sessions each burning 30-50 min on manual spec-PDF cross-checks
> plus 4 abandoned one-off `dbg_av1_*.rs` harnesses was the problem; this
> session's job was to build the reusable replacement, not chase the next
> bug. Delivered:
>
> **Structured symbol trace (`tpt-kinetix-av1/src/entropy.rs`).** Added a
> `thread_local`-backed trace: `SymbolTraceEntry { seq, n_symbols, value,
> bit_pos_before, bit_pos_after, location }`, captured automatically inside
> `SymbolDecoder::read_symbol` (the one place all reads funnel through) via
> `#[track_caller]`. `read_bool`/`read_literal` are *also* `#[track_caller]`,
> so `Location::caller()` propagates transparently through the call chain —
> a `dec.read_literal(4)` call in `reconstruct/mode_cdfs.rs` shows up in the
> trace tagged with *that* call site, not `read_bool`'s internal line, with
> zero edits needed at any of the dozens of call sites in `coeff.rs`/
> `reconstruct/*.rs`. `enable_symbol_trace()`/`take_symbol_trace()` start/
> drain a session; when no session is active the only per-call cost is one
> thread-local check (`symbol_trace_enabled()`), so normal decode paths pay
> nothing extra by default. A companion `BlockMarker { trace_seq, label }`
> facility (`mark_block()`/`take_block_markers()`) is pushed from two call
> sites — `decode_intra_block` (`reconstruct/intra_block.rs`, one marker per
> `mi_row`/`mi_col`/`bsize`) and `reconstruct_tx_block`
> (`reconstruct/reconstruct_block.rs`, one marker per transform block with
> plane/px/tx-size/skip/pred_mode) — so a trace index can be mapped back to
> "which block was this" without re-deriving it from the partition tree by
> hand.
>
> **Differential harness
> (`tpt-kinetix-test-utils/examples/av1_symbol_trace_diff.rs`, wired to `just
> av1-trace-diff [label|--all]`).** For a given corpus entry: decodes with
> `dav1d`/`ffmpeg -c:v libdav1d` (reference) and with `Av1Decoder` (trace
> enabled), computes per-plane PSNR, finds the first pixel (raster order,
> Y then U then V) whose absolute diff exceeds 3, re-decodes with
> `KINETIX_AV1_NOFILTER=1` to report whether the divergence survives with
> deblock/CDEF disabled (pinning blame to `reconstruct/` vs
> `loop_filter.rs`/CDEF), finds the nearest-preceding block marker for the
> divergent pixel, and prints ~26 trace entries around it (source location,
> alphabet size, decoded value, bit-position range). All of this without a
> human placing a single `eprintln!` or re-deriving mi/px coordinates by
> hand.
>
> **Validation run against the corpus (this is real output, not
> illustrative):**
> ```
> === mandelbrot (80x64) ===
>   PSNR Y/U/V = 16.37/20.45/18.39 dB  (symbol trace: 3405 reads, 280 block markers)
>   First divergence: plane Y px=(64,0) kinetix=171 dav1d=161 (delta=10)
>   With KINETIX_AV1_NOFILTER=1: same first-divergence pixel (Y,64,0) kinetix=178 dav1d=161
>     -> deblock/CDEF is NOT the cause; look in reconstruct/ (prediction/transform/coeffs).
>   Nearest preceding block marker: [2904] "coeffs plane=2 px=(32,0) tx=8x4 skip=false pred_mode=13"
>   Symbol trace around that marker (seq 2902..2928): [26 lines, source:line + value per read]
> ```
> This **is not the same divergence** as the previously-reported
> `mi=(0,8)`/`px=(32,8)` `mandelbrot_128x96` desync — that finding came from
> `av1_psnr_check.rs`'s own separately-encoded 128×96 `mandelbrot` clip,
> whereas `av1_intra_corpus()`'s `mandelbrot` entry (which this harness and
> the existing `dbg_av1_mandelbrot128.rs`/`dbg_av1_mandelbrot_diffmap.rs`
> both actually use) is 80×64 — different encoder output, different bits,
> not directly comparable pixel-for-pixel despite the shared label. This
> discrepancy in prior session notes (calling an 80×64 corpus entry
> "`mandelbrot_128x96`") is itself worth fixing (either rename the dbg files
> or regenerate them against the real 128×96 clip) before further root-cause
> work on "the mandelbrot bug", so a future session doesn't keep chasing two
> different bitstreams under one name. Ran `--all` across the full 5-entry
> corpus too: `testsrc` first-diverges at Y (0,0) (delta 113, i.e. still
> badly broken from the very first pixel), `testsrc2` at Y (80,0), `smptebars`
> at Y (50,48) with PSNR 52.30/42.59/99.00 dB (a large improvement over the
> 10.03 dB recorded in the 2026-08-15 session note — likely downstream of
> the 2026-08-19 `predict_directional` fix landing since, not verified
> further this session), and `solid_red` reports no divergence above the
> threshold (99.00/99.00/99.00, matching prior runs). This confirms the
> harness generalizes across the corpus, not just the one entry named in
> the task brief.
>
> **What was deliberately *not* built this session, and why:** the Part 1
> symbol-level oracle. Investigated option (a) first — `ffmpeg -h
> decoder=av1` lists only `operating_point` as an AVOption; no verbose/trace
> flag surfaces per-symbol `libdav1d` state, and this environment has no
> `dav1d` source checkout, so a debug-build-flag investigation would mean
> cloning+building a C project from scratch, assessed as not fitting this
> session's remaining budget after the harness. Fallback option (b) —
> extending `coeff.rs`'s existing Python `coeffs()` oracle test (currently
> synthetic-`ramp()`-only) to real captured bytes and the full
> `intra_frame_mode_info()` sequence — is real, substantial work (the
> control-flow alone covers segment_id, skip, y_mode+angle_delta, uv_mode,
> cfl_alphas, palette, filter_intra, tx_size, each with its own context
> derivation) that risks either being rushed into something numerically
> wrong (worse than not having it, since a broken "oracle" actively
> misleads) or eating the whole session with nothing shippable. Chose to
> ship a solid, tested, actually-useful Part 2 instead of a half-verified
> Part 1 plus a half-built Part 2. This is honestly a partial completion of
> the two-part spec, not a full one — flagged as the clear next step below.
>
> **What a future session should do next:** (1) build the Part 1 oracle for
> real — start from `coeff.rs`'s existing synthetic-buffer Python oracle,
> extract real tile bytes + the exact `TxBlockCtx`/CDF state at a specific
> block via the new symbol-trace/block-marker infrastructure (bit position
> and block index are now directly available from the trace, removing the
> "where do I even start" cost that made this hard before), and diff against
> Kinetix's own `coeffs()` output at that exact block — this validates the
> coeffs()-stage independently even without a full mode_info transcription;
> (2) reconcile the `mandelbrot`/`mandelbrot_128x96` naming split above
> before trusting any cross-session comparison of "the mandelbrot bug";
> (3) extend `Av1Decoder`/`TileDecodeState` with an optional per-stage
> snapshot hook (pre-filter/post-deblock/post-CDEF), mirroring
> `tpt-kinetix-test-utils::trace_dump::MapTracer`'s existing `DecodeTracer`
> pattern for H.264, so the harness's NOFILTER bracket becomes a real
> stage-by-stage walk instead of a binary before/after; (4) once (1)-(3)
> land, retire the four `dbg_av1_*.rs` one-offs for real (they're still
> present, uncommitted, and still occasionally useful today, so left alone
> this session rather than deleted prematurely).
>
> `cargo test -p tpt-kinetix-av1 --lib` stays green (90/90, unchanged count —
> no new unit tests added this session; the harness is validated by actually
> running it, not a unit test, since it shells out to `ffmpeg`/`dav1d`).
> `cargo build --workspace` is green. `cargo clippy -p tpt-kinetix-av1 --lib
> -- -D warnings` and the same for the new example are both clean.
> `capabilities().pixel_exact` untouched (still `false`). Uncommitted files
> this session: `tpt-kinetix-av1/src/entropy.rs` (trace infra),
> `tpt-kinetix-av1/src/reconstruct/intra_block.rs` +
> `reconstruct/reconstruct_block.rs` (marker call sites),
> `tpt-kinetix-test-utils/examples/av1_symbol_trace_diff.rs` (new), `justfile`
> (`av1-trace-diff` recipe). No `git commit` calls were made.

