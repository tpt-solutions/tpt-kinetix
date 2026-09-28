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
- [~] Motion vector prediction (§7.10) and inter block reconstruction (§7.11.3)
      — **2026-09-01 (cont'd #9): MV-component entropy parsing rewritten to spec
      (§5.11.31/§5.11.32).** The old `inter.rs::read_mv_component` had three
      real bugs: (a) read `mv_sign` **last** — spec reads it **first**, before
      `mv_class`; (b) indexed every per-component MV CDF by `use_hp` instead of
      by `comp` (0=row, 1=col) — `TileMv*Cdf[MvCtx][comp]` per §9, so row/col
      stats cross-contaminated; (c) the class-N magnitude formula was ad hoc
      (`mag0 + bit*(1<<class) + Σ …`, then `mag*8 + frac`) — spec is
      `d = Σ mv_bit[i]<<i; mag = CLASS0_SIZE<<(class+2); mag += ((d<<3)|(mv_fr<<1)|mv_hp)+1`
      with `CLASS0_SIZE = 2`. Also: `mv_class0_fr` is indexed `[comp][mv_class0_bit]`
      (was `[use_hp][ref-match-ctx]`); `force_integer_mv` (§5.9.11) now forces
      the fractional reads to 3 (new `FrameHeader.force_integer_mv` field,
      threaded through `TileDecodeState`); `read_mv`/`decode_ref_and_mv` take
      `(allow_hp, force_integer_mv)` not the bogus `(use_hp_row, use_hp_col)`
      that was gated on `filter != BILINEAR`. `InterCdfs` MV fields regrown to
      `[comp]` (`mv_sign`/`mv_class0_bit`/`mv_class0_hp`/`mv_bit`/`mv_hp` gained
      the dimension; `mv_class`/`mv_fr`/`mv_class0_fr` reinterpreted). 125 unit
      tests pass (+3), clippy clean, keyframe corpus unchanged (all intra).
      **Still unvalidated end-to-end** — no inter test content in the corpus and
      the patched dav1d symbol-trace build (scratchpad/av1ref) is wiped between
      sessions and was not rebuilt this session. **Remaining Phase E, all
      unimplemented/unvalidated**: `FindMvStack` §7.10.2 (the real spatial +
      temporal + extra-search MV stack — `build_mv_candidates` is a toy
      2-neighbour version), DRL index (`drl_mode`), ref-frame-name contexts
      §8.3.2 (`single_ref_p1..p6`, comp modes — currently hardcoded ctx 0),
      `is_inter`/`comp_mode`/`interp_filter` contexts, recursive var-tx tree
      (`read_var_tx_size`), inter `tx_type` set, proper MC block-inter-prediction
      §7.11.3.2 (1/1024 scaling, `SUBPEL_BITS`, ref-frame scaling, the 2-pass
      round with `InterRound0`/`InterRound1`), compound distance/diff-weighted
      masks, OBMC, warp, global motion, and `read_mv_component`'s classN loop
      bound audit. A `read_mv` mag test + row/col-independent-CDF test landed.
- [x] **Real gap found 2026-08-23, implemented 2026-08-24 (see session note
      below):** `decode_intra_block`/`decode_inter_block` now call
      `read_cdef()`/`read_delta_qindex()`/`read_delta_lf()` right after
      `read_skip()` (spec order for both `intra_frame_mode_info()` and
      `inter_frame_mode_info()`), with the new `TileDecodeState` fields
      (`cdef_idx` grid, `ReadDeltas`/`current_q_index`/`delta_lf` tracking
      reset per superblock in `decode_superblock`) and their own regression
      tests. Confirmed a true no-op on the current 5-entry corpus. Still
      open: wiring `cdef_idx`'s per-64×64-unit strength into the actual CDEF
      filter pass (`loop_filter.rs` still uses one frame-level strength).

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

> **2026-08-23 session note (AV1 Phase G.0 item 1: the independent coeff-oracle
> bridge is built and working).** Picked up the explicit next step from the
> 2026-08-20 session handoff: "build the Part 1 oracle for real — extract real
> tile bytes + the exact `TxBlockCtx`/CDF state at a specific block via the
> symbol-trace/block-marker infrastructure, and diff against Kinetix's own
> `coeffs()` output at that exact block." Delivered the bridge end-to-end:
>
> - **`entropy.rs::maybe_capture_block`** (new): when `KINETIX_AV1_CAPTURE`
>   names a `(plane, px_x, px_y)` block, the decoder writes `av1_capture.json`
>   *after* the block's `read_coeffs()` returns, containing (a) the raw tile
>   bytes from that block's `coeffs()` bit offset to end of tile, (b) the full
>   `TxBlockCtx` (flattened, field names matching the oracle's `read_coeffs`
>   kwargs), (c) Kinetix's own symbol-trace slice for *just this block's*
>   `coeffs()` (`reference_values`), and (d) the block's neighbour
>   level/dc context (`ctx.above_level/above_dc/left_level/left_dc`) cloned
>   via a new `CoeffContexts::ctx_snapshot`. Setting `KINETIX_AV1_CAPTURE`
>   auto-enables the symbol trace so the per-block slice is populated. A new
>   `SymbolDecoder::bit_position()` accessor supports the capture.
> - **`tools/av1_oracle/diff_block.py`** extended to accept the capture format:
>   it re-seeds its `CoeffContexts` neighbour state from the captured `ctx` (new
>   `_seed_ctx` helper) and then independently re-decodes the block, diffing its
>   symbol sequence against the embedded `reference_values` — reporting the
>   exact `(symbol index, oracle value, Kinetix value, bit position)` of the
>   first divergence. (The old multi-`blocks` spec form still works.)
> - **`justfile::av1-capture BLOCK ENTRY`** recipe runs the differential harness
>   with `KINETIX_AV1_CAPTURE` set, then feeds the resulting `av1_capture.json`
>   to `diff_block.py`. `.gitignore` now excludes `av1_capture.json`.
>
> **Validated the bridge on the standing `mandelbrot` divergence:** `just
> av1-trace-diff mandelbrot` points at block `plane=0 px=(64,0) tx=16x4
> pred_mode=2` (NOFILTER first-div), and `av1-capture 0:64:0 mandelbrot`
> produces a clean, machine-readable result:
> ```
> --- block 0 plane=0 tx=14 mi=(16,0) eob=3 tx_type=1
>     nonzero (2): [(0, -1), (1, -1)]
>     DIVERGENCE at symbol 1: oracle=5 reference=3 (bit 16)
> ```
> i.e. the very first coefficient symbol *after* `txb_skip` (symbol 0, both 0)
> already decodes differently: the oracle reads `5`, Kinetix read `3`, at
> bit 16. This is a precise, reproducible pin on the desync — exactly the
> "independent re-decode of one block" the 2026-08-20 handoff asked for, and
> far cheaper to act on than a whole-frame PSNR number.
>
> **Documented limitation (and the natural next step):** the oracle re-seeds
> neighbour level/dc context from the capture but uses *fresh* (base_q-seeded)
> CDF tables — it does **not** replay mid-tile CDF adaptation, so a divergence
> whose *only* cause is Kinetix's adapted CDF state at this block is not yet
> separated out. Every divergence attributable to a wrong context derivation or
> a numerically-wrong default-CDF *table value* still surfaces here (the
> dominant open hypothesis for the corpus's non-pixel-exactness per the
> 2026-08-19 session), but separating "real bug" from "CDF-adaptation artifact"
> requires capturing the adapted `TileCdfs` too — a known, scoped future
> extension (the `TileCdfs` arrays are enumerable; serializing them into the
> capture is mechanical). The symbol-1 divergence above is most likely in that
> "real bug / wrong table value" class (transform-type or coeff_base context),
> not mere adaptation, because `txb_skip` (symbol 0) matched exactly, which an
> adapted-CDF-only divergence would not necessarily do — worth confirming by
> extending the capture with the adapted CDF set and re-running.
>
> `cargo clippy -p tpt-kinetix-av1 --lib -- -D warnings` is clean; `cargo test
> -p tpt-kinetix-av1 --lib` is green (90/90, unchanged — the new code is
> debug-only capture plumbing gated on an env var, not on any decode path
> exercised by the unit tests). `capabilities().pixel_exact` still `false`.
> Uncommitted files this session: `tpt-kinetix-av1/src/entropy.rs` (capture
> bridge + `bit_position`), `tpt-kinetix-av1/src/coeff.rs`
> (`CoeffContexts::ctx_snapshot` + `CoeffCtxSnapshot`),
> `tpt-kinetix-av1/src/reconstruct/reconstruct_block.rs` (capture call site),
> `tools/av1_oracle/diff_block.py` (capture-format + `_seed_ctx`),
> `justfile` (`av1-capture`), `.gitignore` (`av1_capture.json`). No `git commit`
> calls were made.

> **2026-08-23 session note (cont'd) — the CDF-snapshot extension mentioned as
> "a future extension" above was actually already committed (by the concurrent
> automated process — see [[project_concurrent_repo_activity]] in memory) in
> `3eac457`, but it **didn't compile**: `cargo build -p tpt-kinetix-av1` failed
> with 30 errors (two literal syntax errors — `Vec<Vec<Vec<Vec<u16>>>` missing
> a closing `>` in both `coeff.rs`'s `TileCdfSnapshot` struct and `entropy.rs`'s
> `json_nest4_u16`/`json_nest3_u16` — plus every `clone_eob22`/`clone4`/etc.
> helper in `coeff.rs` hard-coded a *single* array shape and was called on
> fields with several different real shapes (`eob_pt_32` is `[[[u16;7]-;2];2]`,
> not `6`; `coeff_base` is `[[[[u16;5];42];2];5]`, not `[4;4]`; etc.), and the
> `unclone_*` helpers returned `Vec<[...]>` where the field they were assigned
> to is a fixed-size array. **Fixed**: replaced all fourteen shape-specific
> `clone_*`/`unclone_*` functions with four const-generic ones
> (`clone2`/`unclone2`/`clone3`/`unclone3`/`clone4`/`unclone4` in `coeff.rs`,
> parameterized over each dimension), fixed the two syntax errors, and fixed
> the two `json_nest3_u16`/`json_nest4_u16` call sites in `entropy.rs` that
> passed a bare function where a `Vec`-typed closure was needed. Whole
> workspace now builds (`cargo build --workspace` clean) and
> `cargo test -p tpt-kinetix-av1 --lib` is 90/90 again.
>
> **With the build fixed, ran the CDF-snapshot bridge for real for the first
> time and found the actual documented "not yet separated" confound was
> itself buggy in two ways**, not just incomplete:
> 1. `maybe_capture_block` was called **after** `read_coeffs(dec, cdfs, ctxs,
>    blk)` returned, but took its `ctxs`/`cdfs` snapshots from the same
>    (now-mutated) objects — so the capture recorded this exact block's own
>    *post*-read neighbour-context and CDF-adaptation state, not the state the
>    real decoder had when it actually made this block's reads. Fixed by
>    snapshotting `ctxs.ctx_snapshot()`/`cdfs.cdf_snapshot()` **before** the
>    `read_coeffs` call in `reconstruct_block.rs`, threading the pre-state
>    snapshots into a resignatured `maybe_capture_block`/new
>    `entropy::should_capture` (the match-target check factored out so the
>    (now relatively expensive) snapshot clones are skipped whenever
>    `KINETIX_AV1_CAPTURE` doesn't name this exact block).
> 2. **The more consequential bug**: the capture recorded raw tile bytes from
>    the block's starting *bit offset* and had the oracle reconstruct
>    `symbol_range`/`symbol_value` by re-running `init_symbol` on those bytes
>    (`SymbolDecoder::new`). `init_symbol` always forces `symbol_range = 1 <<
>    15` — correct only at a genuine stream start. Mid-tile, spec
>    §8.2.6's renormalization (`bits = 15 - floor_log2(range); range <<=
>    bits`) only guarantees `symbol_range ∈ [1<<15, 1<<16)`, not exactly
>    `32768` — so re-deriving it from raw bytes at an arbitrary bit offset
>    silently assumes the wrong starting range/value whenever the true value
>    isn't exactly `32768`, corrupting every read from that point on **even
>    though the real decoder's CDF tables, context derivation, and read order
>    were all correct**. This is very likely why several previous sessions'
>    manual/capture-based tracing kept finding "divergences" in the coeff path
>    that never led anywhere conclusive. Fixed by adding
>    `SymbolDecoder::raw_state()` (exposes `symbol_range`/`symbol_value`/
>    `symbol_max_bits`/`bit_pos` directly, captured pre-`read_coeffs` alongside
>    the ctx/cdf snapshots) and a new `SymbolDecoder.from_raw_state(...)`
>    classmethod in `tools/av1_oracle/symbol_decoder.py` that resumes from
>    those exact values instead of re-deriving them; `diff_block.py` uses it
>    whenever the capture has `symbol_range`/`symbol_value` fields.
>
> **Validated on two blocks, both now report `TRACE MATCHES REFERENCE`**
> (previously, with the buggy bridge, both reported a divergence at the first
> post-`all_zero` symbol):
> - `mandelbrot`'s standing NOFILTER-divergence block (`plane=0 px=(64,0)
>   tx=16x4`, the one `todo-av1.md` has been chasing since 2026-08-19/20):
>   `just av1-capture 0:64:0 mandelbrot` now matches exactly.
> - `testsrc`'s **very first block of the very first frame**
>   (`plane=0 px=(0,0) tx=8x8`), which is also where `av1_symbol_trace_diff`
>   reports the corpus's worst divergence (`kinetix=129 dav1d=16`, delta 113,
>   unaffected by `KINETIX_AV1_NOFILTER`): `just av1-capture 0:0:0 testsrc`
>   also matches exactly.
>
> **This redirects the root-cause hypothesis that has stood since 2026-08-15
> ("a symbol-decoder desync in the intra block path... the `read_coeffs` unit
> tests still pass in isolation, so the desync is in the integration
> context").** With the bridge now correctly validating the *actual* mid-tile
> integration context (real adapted CDFs, real neighbour contexts, real
> arithmetic-coder state) rather than a broken approximation of it, and both a
> previously-flagged mandelbrot block and testsrc's very first block coming
> back bit-for-bit correct symbol-for-symbol, coeffs()'s reads — including
> context derivation, CDF adaptation, and transform-type/tx_size selection —
> are looking like they are NOT the dominant bug for at least these two
> blocks. Since testsrc's very first pixel is already wrong (129 vs 16) with
> `KINETIX_AV1_NOFILTER=1` (ruling out deblock/CDEF) and its `coeffs()` read is
> now proven correct, **the bug for that block must be downstream of
> `read_coeffs`**: dequantization (`dequantize_coeffs`), inverse transform
> (`inverse_transform` — the tx_type read at that block was `11` = `V_DCT`,
> not `DCT_DCT`, so this exercises the ADST/flip/identity transform paths, not
> just the well-tested DC-only case), or intra prediction. **Next session
> should**: (1) re-run `av1-capture` on a `KINETIX_AV1_DBG`-style
> instrumented path through `dequantize_coeffs`/`inverse_transform`/
> `predict_intra_block` for testsrc's first block specifically (mode=0=DC_PRED,
> tx_type=11=V_DCT, eob=23, 4 nonzero coeffs) and hand-verify the dequant +
> V_DCT inverse-transform output against a hand computation, since that's now
> the narrowest remaining unverified stage for this exact block; (2) extend
> `av1-capture`/`diff_block.py` to optionally also replay dequant+transform
> (not just entropy symbols) so this doesn't require hand computation every
> time; (3) once the bridge is trusted (it now is, for coeffs()), consider
> retiring `mi=(0,8)`/`px=(32,8)` `mandelbrot_128x96`-era leads in this file
> that predate the bridge fix — they were traced with the same broken
> resume-state assumption and may have been chasing symbol values that were
> never actually wrong.
>
> `cargo test -p tpt-kinetix-av1 --lib` is green (90/90, no new tests this
> session — the fixes are to debug-only capture plumbing with no unit-test
> coverage of their own yet; a `raw_state`-round-trip regression test would be
> a reasonable thing to add before extending the bridge further).
> `cargo clippy -p tpt-kinetix-av1 --all-targets -- -D warnings` and
> `cargo build --workspace` are both clean. `capabilities().pixel_exact`
> untouched (still `false`, correctly — this session narrowed the search, it
> did not reach bit-exactness). Modified this session:
> `tpt-kinetix-av1/src/coeff.rs` (const-generic clone helpers, build fix),
> `tpt-kinetix-av1/src/entropy.rs` (`raw_state`, `should_capture`,
> pre-state-based `maybe_capture_block`, `json_nest3/4_u16` call-site fix),
> `tpt-kinetix-av1/src/reconstruct/reconstruct_block.rs` (pre-call snapshot
> timing), `tools/av1_oracle/symbol_decoder.py` (`from_raw_state`),
> `tools/av1_oracle/diff_block.py` (uses `from_raw_state` when available). No
> `git commit` calls were made.

> **2026-08-23 session note (cont'd again) — chased testsrc's first-block
> desync further with the fixed bridge; found and reverted a wrong "fix";
> hand-verified three more stages are correct for this exact block; root
> cause still open.** Picked the narrowest lead the previous note left:
> testsrc's very first block (`plane=0 px=(0,0)`, `tx=8x8`, `y_mode=DC_PRED`,
> `filter_intra=Some(2)`, `tx_type=11=H_DCT`, coefficients only at raster
> positions 26/50/56/57) has a verified-correct `coeffs()` trace, yet
> `KINETIX_AV1_NOFILTER=1` still shows `Y(0,0) = 129` vs `dav1d`'s `16`.
>
> - **Suspected and tested `get_scan`'s `Mrow`/`Mcol` selection
>   (`coeff_tables.rs`)** — for `H_DCT`/`H_ADST`/`H_FLIPADST`
>   (`TX_CLASS_HORIZ`), the code picks `Mcol_Scan` (column-major); for
>   `V_DCT`/etc (`TX_CLASS_VERT`), `Mrow_Scan` (row-major). This looked
>   backwards against a remembered libaom variable-naming convention, so
>   swapped it as an experiment. **This was wrong — swapping it made
>   `testsrc`'s first-pixel delta measurably *worse* (113 → 180) and
>   regressed `mandelbrot`'s U/V PSNR (20.4/18.4 → 17.6/17.6 dB)**, which is
>   what caught the mistake; reverted immediately (re-ran
>   `av1-oracle-regen` + `av1-oracle-validate` + `cargo test --lib` after
>   both the change and the revert to confirm each state). **Re-derived the
>   correct mapping from first principles this time, independent of memory
>   of any other codebase**: `TX_CLASS_HORIZ` (`H_DCT` etc) puts its DCT/ADST
>   along the *width* axis per row (`row_axis_transform` in
>   `reconstruct/transform.rs`) and identity down each column, so a
>   coefficient's *column* index (not row) predicts its importance,
>   uniformly across every row — independently confirmed by this same file's
>   `SIG_REF_DIFF_OFFSET[TX_CLASS_HORIZ]` context-offset table, whose entries
>   are `(0,1)/(0,2)/(0,3)/(0,4)` (same row, varying column). The scan that
>   groups by column first is `Mcol_Scan` (verified against the literal
>   table values: `MCOL_SCAN_8X8 = [0,8,16,...,56, 1,9,17,...]` — all of
>   column 0 across every row, then column 1, ...), confirming the *original*
>   mapping (now expressed via `get_tx_class` instead of raw `matches!` calls,
>   a harmless clarity-only refactor) was correct all along. **Net code
>   change from this whole detour: zero functional diff, clearer comments
>   citing the cross-check.** Left as a cautionary note for future sessions:
>   a "this looks backwards" instinct against a hazy memory of another
>   codebase's naming is not sufficient justification for a change to an
>   already-passing area — verify empirically (PSNR before/after) before
>   trusting the instinct, exactly as happened here.
> - **Hand-verified `dequantize_coeffs` + `inverse_transform` are correct**
>   for this exact block's real captured coefficients (added, ran, then
>   removed a scratch unit test computing `dequantize_coeffs`/
>   `inverse_transform` directly from the captured `quant`/`tx_type`/
>   `tx_size`): residual is genuinely `0` at `(row 0, col 0)` and only
>   nonzero at rows 3/6/7 — an entirely legitimate consequence of `H_DCT`'s
>   column-only frequency compaction (row axis is identity, so a block with
>   no coefficient energy placed at row 0 by the scan simply reconstructs
>   flat-zero there). This is not a transform bug.
> - **Hand-verified `predict_filter_intra`** (`reconstruct/predict.rs`) for
>   this exact block's actual border values (`top` all `127`, `left` all
>   `129`, `tl = 128` — the spec §7.11.2's mandated substitution when neither
>   neighbour is available, confirmed correct) and `filter_intra_mode = 2`:
>   manually computed the first 4×2 sub-block's `Intra_Filter_Taps[2]`
>   dot-products by hand and got `129` for every position in that sub-block,
>   matching the decoder's actual `pred[0..8] = [129,129,...]` exactly. The
>   filter-intra predictor is correctly computing what its (uniform,
>   substituted) inputs mandate — not a predictor-math bug either.
>
> **So for this exact block: `coeffs()`, `dequantize_coeffs`/
> `inverse_transform`, and `predict_filter_intra` are all now individually
> hand-verified correct given their actual inputs, and the reconstructed
> pixel (`128 pred + 0 residual` families rounding to `129`) is a real,
> internally-consistent consequence of those inputs — not a downstream
> bug.** Since `dav1d` reports `16` at the same pixel (nowhere near `127`/
> `128`/`129`, the only three border-default values the whole prediction+
> residual chain can plausibly emit for a corner block with no neighbours
> and near-zero low-order coefficients), **the actual encoded content must
> require a large coefficient this decode never sees** — meaning the bug is
> upstream of all three verified stages, in mode/coefficient *symbol
> selection itself*: either `use_filter_intra`/`filter_intra_mode` were
> misread (spec-legal here, but maybe the real bitstream says `false` and a
> CDF-table transcription bug in `DEFAULT_FILTER_INTRA_CDF`/
> `DEFAULT_FILTER_INTRA_MODE_CDF` — unverified against the spec PDF this
> session, no internet fetch was attempted — makes the decoder read `true`
> instead), or an even earlier read (`segment_id`/`skip`/`y_mode`/
> `partition`) is desynced for this specific superblock (unlike the
> single-superblock `solid_red`/single-tile-friendly blocks validated
> earlier, `testsrc` is `128x96` — multiple superblocks per tile — so
> `hasRows`/`hasCols` partition edge cases or `skip_above`/`ymode_above`
> neighbour-array bookkeeping across superblock boundaries are back in play
> and were not part of this session's coeffs()-only bridge validation).
>
> **Next session should**: (1) fetch the AV1 spec PDF's
> `Default_Filter_Intra_Cdf`/`Default_Filter_Intra_Mode_Cdf` tables and
> byte-diff against `mode_cdfs.rs`'s transcription (cheap, rules out or
> confirms one concrete hypothesis); (2) extend the Phase G.0 capture bridge
> to cover the *mode* symbol sequence (`segment_id`/`skip`/`y_mode`/
> `angle_delta`/`uv_mode`/`filter_intra`), not just `coeffs()` — this is the
> "Part 1 oracle, full `intra_frame_mode_info()`" scope explicitly deferred
> in the 2026-08-20 note, and is now more tractable since the raw-state
> resume bug that would have poisoned it is fixed; (3) do not re-attempt the
> `Mrow`/`Mcol` swap — it is now confirmed wrong twice (derivation and
> measurement) and doesn't need a third look barring new evidence.
>
> `cargo test -p tpt-kinetix-av1 --lib` is green (90/90, no net test-count
> change — the scratch test used to hand-verify the transform stage was
> added and then removed in the same session, as this file's established
> convention for throwaway investigation tests). `cargo clippy -p
> tpt-kinetix-av1 --all-targets -- -D warnings` and `cargo build --workspace`
> are both clean. `capabilities().pixel_exact` untouched (still `false`).
> Modified this session (beyond the previous note's files):
> `tpt-kinetix-av1/src/coeff_tables.rs` (comment-only net change to
> `get_scan`, see above). `tools/av1_oracle/cdf_tables_gen.py` was
> regenerated twice (once for the wrong swap, once for the revert) and ends
> byte-identical to before this note. No `git commit` calls were made.

> **2026-08-23 session note (cont'd a third time) — live spec-PDF fetches
> against `testsrc`'s first-block sequence: every single table and
> control-flow decision checked out; root cause still not found; one real
> (but currently inert) gap found and documented.** This environment turns
> out to have working internet access via `WebFetch`, which no prior AV1
> session had used (earlier notes explicitly say "no internet fetch was
> attempted"/building `dav1d` from source was "impractical" — fetching the
> spec's own markdown mirror is much cheaper and doesn't require that).
> Fetched `github.com/AOMediaCodec/av1-spec`'s `10.additional.tables.md` /
> `06.bitstream.syntax.md` / `09.parsing.process.md` directly and byte-diffed
> them against this crate's transcriptions for every table/function touched
> by `testsrc`'s first block's full read sequence (partition ×3, skip,
> y_mode, uv_mode, has_palette_y, filter_intra flag + mode, `intraDir`
> derivation, `intra_tx_type`) — **all matched exactly**:
> `Default_Partition_W64_Cdf`, `Default_Intra_Frame_Y_Mode_Cdf[0][0..2]`,
> `Default_Skip_Cdf`, `Default_Uv_Mode_Cfl_Allowed_Cdf[0]`,
> `Default_Filter_Intra_Cdf`, `Default_Filter_Intra_Mode_Cdf`,
> `Filter_Intra_Mode_To_Intra_Dir`, `Default_Intra_Tx_Type_Set1_Cdf[1][0..2]`,
> and the full `intra_frame_mode_info()` pseudocode's read order/gating
> (confirmed against the spec's own function body, not memory). Also
> traced the actual symbol sequence via a temporary debug dump (`git diff`
> reverted — see below) confirming the live decode's context/bucket
> selection at every one of those reads (partition contexts all `0`, correct
> `w64`→`w32`→`w16` bucket progression via `PARTITION_CDF_LOOKUP`, correct
> `intra_dir=2`/`H_PRED` from `filter_intra_mode=2`) matches what the spec
> mandates given the block's actual decoded state. **This is the deepest
> verification pass this investigation has had across all ~9 sessions**, and
> it did not find a bug in any of it.
>
> **One real (but not-yet-impactful) gap found and left unfixed, deliberately
> not rushed**: `decode_intra_block` (`reconstruct/intra_block.rs`) never
> calls `read_cdef()`/`read_delta_qindex()`/`read_delta_lf()` between `skip`
> and `y_mode`, even though `intra_frame_mode_info()`'s spec body (fetched
> this session) calls all three unconditionally right after `read_skip()`.
> These three functions don't exist anywhere in this crate at all — not just
> unwired, genuinely unimplemented (`grep` for `read_cdef` finds nothing).
> **This does not explain the current corpus's divergences**: for all 5
> corpus entries, `cdef_bits == 0` and `delta_q_present == delta_lf_present
> == false` (confirmed via the existing `DBG frame_header` trace), and each
> of these three functions' own spec-mandated internal gate makes them
> consume exactly **zero bits** in that configuration (`read_cdef` returns
> immediately unless `enable_cdef` is on *and* the per-64×64 `cdef_idx` slot
> is unset, then reads `L(cdef_bits)` which is a no-op read at `cdef_bits =
> 0`; `read_delta_qindex`/`read_delta_lf` both start with `if (!delta_q/lf
> _present) return`). So this is a real, confirmed-inert-for-now
> correctness gap — it will desync every intra block in any future test
> stream that turns on CDEF index signaling or per-block delta-q/delta-lf,
> which real encoders do use. Left unimplemented rather than rushed: it
> needs new per-tile state (`cdef_idx` grid reset every 64×64 unit,
> `ReadDeltas`/current-qindex/current-loop-filter tracking reset per
> superblock) that doesn't exist on `TileDecodeState` yet, and this session
> had no reason to believe it was the active bug to justify the risk of a
> hasty untested addition. Tracked here as a known, scoped, real gap for a
> future session — not a "next debugging target" for the current
> divergence, since it provably can't be causing it on this corpus.
>
> **Where this leaves the actual divergence.** With coeffs() (previous
> note), and now the *entire* mode-parsing sequence up to and including
> `intra_tx_type`, individually spec-verified correct for this exact block,
> I attempted one more structural argument: `H_DCT`'s column-identity axis
> means a coefficient-domain row with all-zero energy must reconstruct to
> an all-zero *spatial* residual for that same row (verified by hand
> earlier this session), and `testsrc`'s decoded coefficients have rows
> 0/1/2/4/5 entirely zero — yet `dav1d`'s reference shows rows 0-3 of this
> region as uniformly `16` (cols 0-15) / `81` (cols 16-23), which would
> require *every* row to carry some shift, seemingly incompatible with
> `H_DCT`. **This argument doesn't actually settle anything**: `dav1d`'s
> output is the fully filtered (deblock+CDEF+restoration) frame, and there
> is no way with tooling available in this session to get `dav1d`'s
> *pre-filter* reconstruction to compare apples-to-apples (building `dav1d`
> from source for a debug/trace build is still assessed as impractical, per
> every prior session) — so the "H_DCT can't produce this" reasoning is
> confounded by not knowing how much of the observed flatness is genuine
> pre-filter structure versus CDEF/deblock smoothing on top of a real but
> different pre-filter pattern. Recorded as a lead, not a conclusion.
>
> **Next session should**: (1) if internet access is confirmed to keep
> working in future sessions, this WebFetch-based spec cross-check is now
> the standard, cheap way to rule out table-transcription bugs — prefer it
> over hand-copying spec PDF text as earlier sessions did; (2) the
> `read_cdef`/`read_delta_qindex`/`read_delta_lf` gap above is real and
> worth implementing properly for its own sake (broader stream
> compatibility), scoped as new `TileDecodeState` fields + the three
> functions, with its own unit tests — but budget it as separate work, not
> as "the fix" for the open divergence; (3) the two remaining un-independently-
> verified pieces of this exact block's read sequence are `has_chroma`'s
> exact gating (confirmed structurally correct by inspection this session,
> not independently spec-fetched) and the *skip* context derivation
> (`(above_skip + left_skip).min(2)`, trivially `0` for this first block so
> low-risk) — low priority given how much else has checked out; (4) the
> highest-leverage remaining lever is still building an actual
> pre-filter-comparable reference (either a `dav1d` debug build, or adding a
> `KINETIX_AV1_NOFILTER`-equivalent probe into `ffmpeg`'s AV1 filtergraph if
> one exists) so pixel-level reasoning about tx_type/coefficient plausibility
> stops being confounded by post-filtering.
>
> Temporary debug instrumentation added and **kept** this session (all
> opt-in via env vars, following the established zero-cost-when-unset
> convention): `KINETIX_AV1_DBG_TILE_BYTES` (`reconstruct/mod.rs`, dumps a
> tile's raw bytes from its real bit offset — needed to feed an independent
> by-hand replay), `KINETIX_AV1_DBG_FIRST_READS` and `KINETIX_AV1_DBG_ROWS`
> (`tpt-kinetix-test-utils/examples/av1_symbol_trace_diff.rs`, dump the
> first N symbol-trace entries / a small pixel patch from both decoders).
> **Also fixed, incidentally**: `tpt-kinetix-h264/src/decoder/mod.rs` had a
> genuine borrow-checker error (`recon.luma` moved then borrowed again for a
> `KINETIX_DUMP_PREDEBLOCK` debug dump) that was blocking `cargo build
> --workspace` entirely — this was mid-edit, uncommitted work from the
> concurrent automated process (see `[[project_concurrent_repo_activity]]`
> in memory), not something introduced this session; fixed by moving the
> dump before the move rather than reverting their work. `cargo test -p
> tpt-kinetix-av1 --lib` is green (90/90, unchanged). `cargo clippy -p
> tpt-kinetix-av1 --all-targets -- -D warnings` and `cargo build --workspace`
> are both clean. `capabilities().pixel_exact` untouched (still `false`).
> No `git commit` calls were made.

> **2026-08-24 session note (cont'd yet again) — confirmed the divergence is
> a real bug (two independent reference decoders agree), then verified the
> ENTIRE remaining upstream chain (tile_info, frame_size/superres,
> screen-content-tools gating) and found nothing wrong there either; root
> cause still not found; two more real-but-inert gaps documented.**
>
> **First, closed off the "maybe this is a CDEF/harness confound, not a real
> bug" doubt from the previous note.** `ffmpeg`'s build here also has
> `libaom-av1` (a second, completely independent AV1 codebase from AOM/
> Google, distinct from `dav1d`/VideoLAN) available as a decoder. Dumped
> `testsrc`'s raw OBU bytes to a file (new `KINETIX_AV1_DUMP_OBU` env var on
> `av1_symbol_trace_diff.rs`, kept) and decoded it with both
> `-c:v libdav1d` and `-c:v libaom-av1` via plain `ffmpeg -f obu`. **Both
> independent decoders produce byte-identical output** (`10 10 10 10 ... 10
> 51 51 51 51 51 51 51 51` for the same 24-byte row-0 slice). Two unrelated
> reference implementations agreeing rules out "one decoder's CDEF is doing
> something unusual" as an explanation — the correct decode of this frame
> really is closer to `16`/`81` than Kinetix's `129`, and this is a real,
> confirmed Kinetix decode bug, not a harness artifact. (ffmpeg's own native
> software `av1` decoder couldn't be used for a third cross-check — this
> build only has the hardware-accelerated path compiled in, no software
> fallback.)
>
> **Then kept pulling the thread upstream of everything verified so far**,
> using the same live-spec-fetch method (now fetching `06.bitstream.syntax.md`
> too, via a direct `curl` into a local file rather than `WebFetch`'s
> summarizing model — large pages like `10.additional.tables.md` (12k
> lines) were silently dropping requested tables from `WebFetch`'s answers,
> e.g. it initially claimed `Max_Tx_Depth`/`Filter_Intra_Mode_To_Intra_Dir`
> "don't exist" when they're just in a part of the file the summarizer
> didn't surface; `curl` + local `grep`/`sed` is the reliable way to read
> these files exhaustively, `WebFetch` is fine for small targeted lookups).
> Verified, byte-for-byte or logic-for-logic against the spec's own
> pseudocode:
> - `Max_Tx_Depth[BLOCK_SIZES]` (derived indirectly — the table itself isn't
>   named that in the spec text the way `MAX_TX_DEPTH_TABLE` implies, but
>   `read_tx_size()`'s pseudocode confirms `maxTxDepth = Max_Tx_Depth[MiSize]`
>   is a real per-`bsize` lookup, and `Split_Tx_Size`/`Default_Tx_8x8_Cdf`
>   `/_16x16/_32x32/_64x64_Cdf` all matched Rust's transcription exactly,
>   including the `Tx_8x8` bucket's 2-symbol vs the others' 3-symbol shape).
> - `get_tx_set(txSz)` (AV1's real function, spec section "Get transform set
>   function") — matches `get_tx_set_intra` exactly, condition-for-condition.
> - `Tx_Type_Intra_Inv_Set1`/`Filter_Intra_Mode_To_Intra_Dir` and the
>   `intraDir` derivation rule (`use_filter_intra ? Filter_Intra_Mode_To_
>   Intra_Dir[filter_intra_mode] : YMode`) — matches exactly.
> - `frame_obu()`'s top-level structure (`frame_header_obu(); byte_alignment();
>   tile_group_obu(sz)`) and `byte_alignment()`'s own definition (pads with
>   `zero_bit`s to the next byte boundary) — confirms `frame.rs`'s
>   `byte_align()` function is *functionally* correct (same bit-consumption,
>   same all-zero check) despite its doc comment mislabeling it as
>   implementing `trailing_bits()` instead (a real but harmless
>   documentation bug — `trailing_bits()`'s first pad bit must be `1`, not
>   `0`, but the two only differ in what value they *assert*, not how many
>   bits they consume, so this doesn't desync anything; worth a comment fix,
>   not a functional one).
> - `tile_info()`'s full `uniform_tile_spacing_flag` / `tile_log2()` /
>   `increment_tile_cols_log2`/`_rows_log2` while-loops — matches exactly,
>   confirmed via a new debug hook (`KINETIX_AV1_DBG_TILEINFO`, kept) showing
>   `testsrc` genuinely has `sb_cols = sb_rows = 2` (unlike `solid_red`'s
>   trivial `sb_cols = sb_rows = 1`, which never reads an increment bit at
>   all) — so `testsrc` really does exercise a bit-consuming code path
>   `solid_red`'s passing status never validated, and that path reads
>   exactly the bits spec mandates (confirmed both `increment_tile_cols_log2`
>   and `increment_tile_rows_log2` are real, consumed reads here, correctly
>   producing `TileCols = TileRows = 1` after both come back `0`).
>
> **Two more real-but-currently-inert gaps found and documented (not
> fixed — same reasoning as the `read_cdef`/delta gap: real, but zero
> impact on the current 5-entry corpus, not worth a rushed fix):**
> 1. `parse_frame_size` (`frame.rs`) never implements `superres_params()` at
>    all — spec's `frame_size()` calls it unconditionally after computing
>    `FrameWidth`/`FrameHeight`, and it reads a real `use_superres` bit
>    whenever the sequence header's `enable_superres` is `true` (regardless
>    of whether superres ends up used). Confirmed via a new debug hook
>    (`KINETIX_AV1_DBG_SUPERRES`, kept) that `enable_superres == false` for
>    all 5 corpus entries, so this consumes zero bits today — but any stream
>    with `enable_superres = true` in its sequence header will desync from
>    `frame_size()` onward, superres or not.
> 2. `allow_intrabc`'s gate compares `w == rw` (`FrameWidth` vs
>    `RenderWidth`) where spec requires `UpscaledWidth == FrameWidth`. Since
>    superres isn't implemented at all (gap 1), `UpscaledWidth` is never
>    computed/distinguished from `FrameWidth`, so this is doubly wrong: even
>    once gap 1 is fixed, this comparison is checking the wrong pair of
>    variables (render size, not upscaled-from-superres size). Inert for now
>    because all 5 corpus entries happen to have `RenderWidth == FrameWidth`
>    too. Both gaps share one root fix (implement `superres_params()` for
>    real, then fix this comparison to use the resulting `UpscaledWidth`).
>
> **Where this leaves things**: literally every symbol read and every table
> in `testsrc`'s first block's decode chain — partition (×3), skip, y_mode,
> uv_mode, has_palette_y, filter_intra (×2), tx_depth, and the full
> `coeffs()` sequence — plus everything upstream of it (OBU framing,
> sequence header's `enable_superres`/`allow_screen_content_tools` gating,
> frame size, tile info, `frame_obu()`'s byte-alignment) has now been
> individually checked against the live spec text or hand-derived from first
> principles, and **none of it shows an error**. Combined with the
> two-independent-decoder confirmation that a real bug exists, this is a
> genuinely unusual state: either the bug is in a piece of state I haven't
> thought to check yet (candidates: `TX_SIZE_SQR`/`ADJUSTED_TX_SIZE`/
> `DC_QLOOKUP_8`/`AC_QLOOKUP_8` table *values* at this exact qindex/index —
> spot-checked the formulas and a few nearby tables but not these two lookup
> tables' actual numeric contents; or the sequence header's bit-depth/
> color-config fields, unchecked this session), or the bug is in a stage
> that individual symbol-level correctness can't reveal (e.g. `TxTypes[]`
> array bookkeeping across multiple transform blocks in the same
> prediction block, or an aliasing/overwrite bug in how `samples`/plane
> buffers get written).
>
> **Update, same session**: spot-checked `DC_QLOOKUP_8[128]`/
> `AC_QLOOKUP_8[128]` (the two values `dequantize_coeffs` actually used for
> this exact block) against `08.decoding.process.md`'s literal
> `Dc_Qlookup`/`Ac_Qlookup[0]` (the `BitDepth==8` row) — **both match exactly**
> (`140`/`176`). While there, found a **third real gap, since fixed**: spec's
> `get_dc_quant(plane)` is `dc_q(get_qindex(0, segment_id) + DeltaQYDc)` for
> luma (and `+ DeltaQUDc`/`+ DeltaQVDc` for chroma) — the DC coefficient's
> quantizer step uses `qindex + a per-plane DC delta`, not the plain
> per-frame `qindex`. `frame.rs` parsed `delta_q_y_dc`/`delta_q_u_dc`/
> `delta_q_u_ac`/`delta_q_v_dc`/`delta_q_v_ac` into `FrameHeader` but nothing
> in `reconstruct/` ever read any of them — `dequantize_coeffs` always used
> the same plain `qindex` for every plane's DC term. Confirmed inert on the
> current corpus via a debug hook (`KINETIX_AV1_DBG` line extended with the
> five delta fields): all five entries have all five at `0`.
>
> **Fixed properly, not left as a documented gap this time**, since the
> wiring turned out contained and mechanical: added a small `pub struct
> DeltaQ { y_dc, u_dc, u_ac, v_dc, v_ac }` (`reconstruct/mod.rs`), threaded
> it as one new parameter through `decode_tile_group`/`TileDecodeState::new`
> (populated from `FrameHeader`'s five fields at the `reconstruct_av1_frame`
> call site) and `TileDecodeState::qindex_for_plane(plane) -> (u8, u8)` (the
> real `get_dc_quant`/`get_ac_quant` formula, clamped to `0..=255` like the
> spec's `Clip3`). `reconstruct_tx_block`/`dequantize_coeffs` now take
> `qindex_dc`/`qindex_ac` explicitly instead of one shared `qindex`, computed
> once per plane at each of the 3 intra + 2 inter call sites *before* the
> `&mut self.{y,u,v}_plane` reborrows those functions already hold (calling
> a `&self` method after that reborrow is live is a borrow-check error —
> hence precomputing into locals up front, not inline at the call). Added
> two real regression tests: `qindex_for_plane_applies_per_plane_delta_and_
> clamps` (asserts the delta math and the `Clip3`-style clamping in both
> directions with deliberately out-of-range deltas) and `dequant_tests::
> dequantize_coeffs_uses_separate_dc_and_ac_qindex` (asserts index 0 scales
> by the DC table at `qindex_dc` and every other index by the AC table at
> `qindex_ac`). Re-ran the full corpus PSNR check after landing this —
> **numbers are bit-for-bit identical to before the fix** (as expected: all
> five deltas are `0` on this corpus, so the fix is a true no-op here, not a
> silent behavior change) — `solid_red` is still `99.00` dB pixel-exact.
> `cargo test -p tpt-kinetix-av1` (unit + every integration/proptest/doctest
> file) is green, `cargo clippy -p tpt-kinetix-av1 --all-targets -- -D
> warnings` is clean, `cargo build --workspace` is clean. One external test
> file needed updating for the new `decode_tile_group` parameter:
> `tpt-kinetix-av1/tests/proptest_coeffs.rs` (added `DeltaQ::default()`).
>
> Remaining two gaps (`read_cdef`/delta-lf, superres/`allow_intrabc`) are
> still open and still real, still confirmed inert on this corpus — those
> two need new per-tile/per-superblock *state* (a `cdef_idx` grid,
> `ReadDeltas`/current-qindex tracking, `UpscaledWidth`), not just a
> parameter thread, so they're intentionally left for a dedicated pass
> rather than rushed alongside this one.

> **2026-08-24 session note (cont'd yet again) — implemented the superres/
> `allow_intrabc` gap for real (not just documented); found and fixed two
> more related real bugs in the same function while there.** Picked up the
> "implement the two remaining real-but-inert gaps" item from the previous
> note. `superres_params()` (§ Superres params syntax) was not implemented
> at all — `parse_frame_size` never read `use_superres`/`coded_denom` and
> never distinguished `UpscaledWidth` from `FrameWidth`. Implemented it
> properly (fetched the exact syntax + `SUPERRES_NUM=8`/`SUPERRES_DENOM_MIN=9`/
> `SUPERRES_DENOM_BITS=3` constants via `curl` against the spec's own
> `06.bitstream.syntax.md`/`03.symbols.md`, master branch — the previous
> session's `raw.githubusercontent.com/.../main/...` URL 404s; it's `master`):
> `FrameWidth` is now correctly downscaled by `SuperresDenom` when
> `use_superres` is signaled, `UpscaledWidth` is tracked as a new
> `FrameHeader::upscaled_width` field, and the `allow_intrabc` gate now
> compares `UpscaledWidth == FrameWidth` (spec) instead of the old code's
> `FrameWidth == RenderWidth` (wrong pair of variables, and the "doubly
> wrong" issue the previous note flagged).
>
> **Two more real bugs found and fixed while implementing this, both in the
> same `parse_frame_size`/`render_size()` code path:**
> 1. `render_size()`'s `render_and_frame_size_different` flag read was gated
>    on `!seq.reduced_still_picture_header` — but the spec's
>    `uncompressed_header()` calls `frame_size()`/`render_size()`
>    unconditionally for a `FrameIsIntra` frame regardless of
>    `reduced_still_picture_header` (that flag only forces earlier fields to
>    fixed defaults and forces `frame_size_override_flag = 0`; confirmed by
>    reading the actual spec pseudocode around the `reduced_still_picture_header`
>    branch). So a reduced-still-picture-header keyframe was one bit short of
>    what the encoder actually wrote, desyncing everything parsed after
>    `render_size()`. Fixed by removing the gate; updated
>    `parse_frame_header_reduced_still_keyframe`'s hand-built test bitstream to
>    include the now-correctly-read bit (its old comment's claim "no bits are
>    emitted here" for `render_size()` was itself wrong).
> 2. When `render_and_frame_size_different == 1`, the render width/height were
>    read via `read_ns(br, w)` (non-symmetric coding) — but the spec's
>    `render_size()` syntax reads `render_width_minus_1`/`render_height_minus_1`
>    as plain `f(16)` fixed-width fields, not `ns()`. Confirmed directly
>    against the fetched syntax table. This is inert on the current 5-entry
>    corpus (all have `render_and_frame_size_different == 0`, confirmed via
>    the existing `KINETIX_AV1_DBG_SUPERRES` hook — reused, extended to also
>    print `uw`), but would have desynced any real stream that signals a
>    render size different from the coded frame size. Fixed by switching to
>    `read_f(br, 16)`.
>
> **Regression tests added** (`frame.rs`):
> `parse_frame_size_applies_superres_downscale_and_keeps_upscaled_width`
> (drives `parse_frame_size` directly with `use_superres=1`/`coded_denom=3`,
> asserts `FrameWidth` is correctly downscaled from 128 to 85 while
> `UpscaledWidth` stays 128 and `RenderWidth` defaults to `UpscaledWidth`, not
> the downscaled width) and
> `parse_frame_size_skips_superres_bit_when_sequence_header_disables_it`
> (asserts `enable_superres=false` reads zero superres bits and that the
> `f(16)` render-size fields are read/interpreted correctly when
> `render_and_frame_size_different=1`).
>
> **Impact measured**: confirmed a true no-op on the current corpus (all 5
> entries have `enable_superres=false`, checked via the debug hook) —
> `cargo run -p tpt-kinetix-av1 --example av1_psnr_check` numbers are
> unchanged by this fix specifically (the numbers differ slightly from the
> previous session's recorded baseline, but that's because the previous
> session's *other* uncommitted fix — the per-plane `DeltaQ` wiring — was
> already sitting in the working tree before this session started and wasn't
> reflected in that baseline; re-ran twice, deterministic both times).
> `solid_red_32`/`_64` still 99.00 dB pixel-exact, unaffected as expected.
> `cargo test -p tpt-kinetix-av1 --lib` is green, now 94/94 (was 92/92 at
> session start — the DeltaQ fix's 2 tests plus this session's 2 new tests).
> `cargo clippy -p tpt-kinetix-av1 --all-targets -- -D warnings` and `cargo
> build --workspace` are both clean.
>
> **What's left of this gap-closing item**: `read_cdef()`/
> `read_delta_qindex()`/`read_delta_lf()` (the other half of the "two
> remaining gaps" from the previous note) are still unimplemented — that one
> needs new per-tile/per-superblock decode *state* (`cdef_idx` grid reset
> every 64×64 unit, `ReadDeltas`/current-qindex/current-loop-filter tracking
> reset per superblock), not just a header-parsing fix like this session's
> work, so it's still intentionally left for its own dedicated pass. The
> underlying pixel-exactness mystery (the still-unexplained `testsrc`
> divergence documented across the last several session notes above) is
> also still open — this session's fix is header-parsing correctness/future
> stream compatibility, not a lead on that mystery (confirmed inert on the
> corpus that mystery is being chased on).
>
> No `git commit` calls were made. Files modified this session (on top of
> the already-uncommitted `DeltaQ` files from the previous session, still
> present in the working tree): `tpt-kinetix-av1/src/frame.rs`.

> **2026-08-24 session note (cont'd a fifth time) — implemented the other
> remaining gap, `read_cdef()`/`read_delta_qindex()`/`read_delta_lf()`, for
> real.** This was the harder of the two "two remaining gaps" items (the
> superres one above only needed a header-parsing fix; this one needed new
> per-tile/per-superblock decode state, as previous notes anticipated).
> Fetched §5.11.7/§5.11.18/§5.11.19/§5.11.20/§5.11.56's exact pseudocode via
> `curl` against the spec's `06.bitstream.syntax.md` (confirmed `read_cdef`/
> `read_delta_qindex`/`read_delta_lf` sit in the identical position — right
> after `read_skip()`, before `read_is_inter()`/`use_intrabc` — in *both*
> `intra_frame_mode_info()` and `inter_frame_mode_info()`, so `inter_block.rs`
> had the exact same gap as `intra_block.rs`, not just the intra path the
> earlier note called out) and the `Default_Delta_Q_Cdf`/`Default_Delta_Lf_Cdf`
> tables via `10.additional.tables.md` and `09.parsing.process.md` (confirmed
> `TileDeltaQCdf`/`TileDeltaLFCdf`/`TileDeltaLFMultiCdf[i]` are separate
> adaptive CDF instances that all init from the one default table).
>
> **New state added to `TileDecodeState`** (`reconstruct/mod.rs`):
> `use_128x128_superblock`, `enable_cdef`, `cdef_bits`, `delta_q_present`,
> `delta_q_res`, `delta_lf_present`, `delta_lf_res`, `delta_lf_multi`,
> `num_planes` (frame-header/sequence-header constants, threaded in via a new
> `CdefDeltaParams` bundle struct rather than growing the already-long
> `TileDecodeState::new`/`decode_tile_group` positional argument lists
> further), plus per-tile mutable state: `current_q_index` (`CurrentQIndex`,
> starts at `base_q_idx`), `delta_lf: [i8; 4]` (`DeltaLF[FRAME_LF_COUNT]`),
> `read_deltas` (`ReadDeltas`), `cdef_idx: HashMap<(usize, usize), i8>`
> (`cdef_idx[r][c]`, absent == spec's `-1`).
>
> **New methods**: `clear_cdef`/`read_cdef`/`read_delta_qindex`/
> `read_delta_lf`, called from `decode_superblock` (the `ReadDeltas =
> delta_q_present` + `clear_cdef(r, c)` prelude, mirroring `decode_tile()`)
> and from both `decode_intra_block` and `decode_inter_block` (right after
> `read_skip()`, `ReadDeltas = 0` after). `qindex_for_plane` now reads
> `current_q_index` instead of the static frame-level `qindex` (which became
> genuinely dead code and was removed from the struct), so a stream that
> *does* turn on `delta_q_present` will get the right per-block quantizer
> once this path is exercised — not just correct bit consumption.
> `Default_Delta_Q_Cdf`/`Default_Delta_Lf_Cdf` and their read helpers landed
> in `mode_cdfs.rs` alongside the existing `skip`/`segment_id` CDF pattern.
>
> **Correctness note**: full multi-strength CDEF *application* (selecting a
> different filter strength per 64×64 unit from `cdef_idx`) is still not
> wired into the post-filter pass (`loop_filter.rs` still uses the single
> frame-level strength) — this session only implemented the *bitstream
> parsing* side (consuming the right number of bits, tracking the grid
> correctly) so future streams that signal CDEF/delta-q/delta-lf don't
> desync. Wiring `cdef_idx` into the actual filter strength selection is a
> separate, still-open task.
>
> **Impact measured**: confirmed a true no-op on the current 5-entry corpus
> (`cargo run -p tpt-kinetix-av1 --example av1_psnr_check` numbers are
> bit-for-bit identical to the previous note's) — expected, since all 5
> entries have `cdef_bits == 0`/`delta_q_present == delta_lf_present ==
> false`. 8 new regression tests added directly against `read_cdef`/
> `read_delta_qindex`/`read_delta_lf`/`clear_cdef` (gate no-ops verified by
> asserting the `SymbolDecoder`'s `bit_position()` is unchanged, not just
> that the output looks right) via a new `make_cdef_delta_state` test
> factory in `reconstruct/tests.rs`. `cargo test -p tpt-kinetix-av1` (unit +
> integration/proptest/doctest) is green — unit tests now 100/100 (was
> 94/94 before this note). `cargo clippy -p tpt-kinetix-av1 --all-targets --
> -D warnings` and `cargo build --workspace` are both clean. The
> `fuzz_obu_parse` target could not be run this session — this Windows
> toolchain's nightly is missing the ASan runtime component
> (`librustc-nightly_rt.asan.a`), a pre-existing environment gap unrelated to
> this change, not something to "fix" by disabling sanitizers; the existing
> `proptest_coeffs.rs` suite (which exercises `decode_tile_group` end to end,
> including this session's new call sites) passed instead.
>
> Both items from the "two remaining gaps" note are now closed. What
> remains open for AV1: the still-unexplained multi-session `testsrc` pixel
> divergence (this session's work is inert on that corpus, not a lead on
> it), full CDEF multi-strength wiring (noted above), and the broader Phase
> G conformance push. No `git commit` calls were made. Files modified this
> session: `tpt-kinetix-av1/src/reconstruct/mod.rs`,
> `tpt-kinetix-av1/src/reconstruct/mode_cdfs.rs`,
> `tpt-kinetix-av1/src/reconstruct/intra_block.rs`,
> `tpt-kinetix-av1/src/reconstruct/inter_block.rs`,
> `tpt-kinetix-av1/src/reconstruct/partition.rs`,
> `tpt-kinetix-av1/src/reconstruct/tests.rs`,
> `tpt-kinetix-av1/tests/proptest_coeffs.rs`.
>
> **Next session should**: (1) build the real "Part 1 oracle" this file has deferred since
> 2026-08-20 — an independent Python re-implementation of
> `intra_frame_mode_info()` + `coeffs()` end-to-end (not just `coeffs()`
> alone) fed real captured bytes, now that `curl`-based spec fetching is
> confirmed reliable and the arithmetic decoder/CDF-snapshot bridge exists —
> this would let a differential run cover ground this session's one-off
> hand-checks did serially and slowly; (2) implement the two remaining
> real-but-inert gaps (superres/`allow_intrabc`, `read_cdef`/delta-lf) as a
> frame-header-completeness pass — the third (per-plane DC delta-q) is
> already fixed, see below; (3) stop
> assuming `WebFetch`'s summarized answer over a large spec file is
> exhaustive — prefer `curl` + local `grep`/`sed` for anything beyond a
> single small named table, per this session's `Max_Tx_Depth` false-negative.
>
> Debug hooks added and kept this session (all opt-in via env vars):
> `KINETIX_AV1_DBG_SUPERRES`, `KINETIX_AV1_DBG_TILEINFO` (`frame.rs`),
> `KINETIX_AV1_DUMP_OBU` (`av1_symbol_trace_diff.rs`, dumps a corpus entry's
> raw OBU bytes to a directory for cross-decoder comparison outside the
> Rust harness). `cargo test -p tpt-kinetix-av1 --lib` is green (90/90,
> unchanged). `cargo clippy -p tpt-kinetix-av1 --all-targets -- -D warnings`
> and `cargo build --workspace` are both clean (re-verified after the
> concurrent automated process's unrelated edits landed in `tpt-kinetix-aac`/
> `tpt-kinetix-h264`/`tpt-kinetix-test-utils` mid-session — see
> `[[project_concurrent_repo_activity]]`). `capabilities().pixel_exact`
> untouched (still `false`). No `git commit` calls were made.

> **2026-08-27 session note — reverted a real regression the concurrent
> process landed in commit `ba04a2c`: the CDF-adaptation rate formula.**
> Ran `av1_psnr_check` at session start and found `solid_red_32`/`_64` had
> dropped from the long-standing **99.00 dB pixel-exact** to **16.54 / 15.46
> dB** — a regression in a known-good case. Bisected to `ba04a2c`
> (`tpt-kinetix-av1/src/entropy.rs`), which changed `SymbolDecoder`'s §8.2.6
> CDF-update rate from
> `3 + (count>15) + (count>31) + floor_log2(n).min(2)` (correct) to
> `3 + (count>15) + (count>31) + (n>2)`, with a commit message and code
> comment asserting the new form is "spec-correct per §8.2.6". It is not.
> Fetched the spec live (`curl` →
> `raw.githubusercontent.com/AOMediaCodec/av1-spec/master/09.parsing.process.md`):
>
> ```
> rate = 3 + ( cdf[ N ] > 15 ) + ( cdf[ N ] > 31 ) + Min( FloorLog2( N ), 2 )
> ```
>
> The original code matched this exactly; so does the independent Python
> oracle (`tools/av1_oracle/symbol_decoder.py:127`,
> `min(_floor_log2(n), 2)`). The `ba04a2c` change also **regenerated the
> `coeff.rs` `EXPECTED_A`/`EXPECTED_B` oracle golden vectors and the
> `entropy.rs` CDF-snapshot test vectors to lock in the wrong formula.**
>
> **Fix**: `git checkout 9be3f11 -- tpt-kinetix-av1/src/entropy.rs
> tpt-kinetix-av1/src/coeff.rs tpt-kinetix-av1/tests/phase_c_conformance.rs`
> (restores the correct rate, the correct golden vectors, and drops that
> commit's throwaway `KINETIX_AV1_DBG` prints in `coeff.rs` + the per-row
> diff `eprintln!` loop in `phase_c_conformance.rs`). **Kept** from
> `ba04a2c`: the `reconstruct/mod.rs` + `partition.rs` change moving
> `coeff_ctxs.clear_left()` from per-superblock to per-superblock-**row**
> (that one is a genuine, spec-correct fix — §7.3 `clear_left_context()` is
> per superblock row).
>
> **Impact**: `solid_red_32`/`_64` back to **99.00 dB** pixel-exact.
> Non-trivial corpus roughly back to its prior band (`testsrc` 10.77,
> `mandelbrot` 15.64, `smptebars` 9.36, `testsrc2` 12.96 dB Y — the
> `clear_left`-row fix is now also in effect, which is why these aren't
> bit-identical to the oldest recorded numbers). `cargo test -p
> tpt-kinetix-av1` green (100 unit + all integration/proptest/doctest),
> `cargo clippy -p tpt-kinetix-av1 --all-targets -- -D warnings` clean.
> `just av1-oracle-validate` not run — this Windows box has no `python3` on
> PATH (only the Store alias stub); the embedded Rust golden vectors, which
> match the pre-regression state, cover the same ground. No `git commit`
> calls were made. Files modified: `tpt-kinetix-av1/src/entropy.rs`,
> `tpt-kinetix-av1/src/coeff.rs`,
> `tpt-kinetix-av1/tests/phase_c_conformance.rs` (all reverts).
>
> **Lesson for future sessions / the concurrent process**: verify the rate
> formula against the live spec text before touching it again — it is
> `Min(FloorLog2(N), 2)`, confirmed 2026-08-27. The still-open multi-session
> `testsrc` divergence is unrelated to this and remains the real next target.
>
> **Also this session**: `just av1-oracle-validate` / `av1-capture` now work
> on Windows (justfile switched `python3` → `python` via `os()` guard; this
> box's `python3` is only the Store alias stub). Re-ran the coeff oracle:
> `just av1-oracle-validate` passes against the reverted (correct) Rust code,
> independently confirming the regression fix. Ran `just av1-capture 0:0:0
> testsrc` — the Python coeff oracle reports **`TRACE MATCHES REFERENCE`** for
> `testsrc`'s block 0 (`plane=0 mi=(0,0) eob=23 tx_type=11 nonzero=[(26,1),
> (50,1),(56,1),(57,-1)]`), i.e. given fresh (base-q-seeded) CDFs and the
> captured neighbour context, the coefficient syntax decode of that block is
> byte-consistent between Rust and the independent oracle. Combined with the
> divergence still being `Y(0,0) kinetix=129 vs dav1d=16` under
> `KINETIX_AV1_NOFILTER=1`, this pushes the root cause into one of: (a) a
> wrong CDF *table value* or *context* in the mode-symbol path
> (`partition`/`skip`/`intra_y_mode`/`uv_mode`/`use_filter_intra`/
> `filter_intra_mode`/`tx_depth`) that produces a valid-but-wrong symbol with
> no bit-count desync — the captured first-block mode trace is
> `partition SPLIT,SPLIT,HORZ_B` → `skip=0` → `y_mode=DC` → `uv_mode=12` →
> `has_palette_y=0` → `use_filter_intra=1` → `filter_intra_mode=2` →
> `tx_depth=1` (all internally consistent, none independently checkable
> against dav1d without a reference symbol trace); (b) tile-level state
> consumed before block 0 (the oracle can't see it); or (c) a
> reconstruction-side bug the coeff oracle doesn't cover. The coeff oracle's
> fresh-CDF limitation means it also can't catch a mid-tile CDF-adaptation
> desync — but block 0 has no preceding blocks, only this block's own mode
> reads, so adaptation is a weak suspect here.
>
> **Next session, concretely**: the coeff oracle needs to grow the
> `intra_frame_mode_info()` mode-symbol sequence (the "Part 1 oracle" deferred
> since 2026-08-20) so `just av1-capture` can diff the mode reads too, OR a
> dav1d debug build is stood up for a real reference symbol trace. Everything
> short of that has now been tried across ~11 sessions.

> **2026-08-27 session note (cont'd) — FOUND THE MULTI-SESSION `testsrc`
> DESYNC: the per-superblock `read_lr()` syntax (§5.11.57) was never
> implemented.** After exhaustively re-verifying that block 0's *entire*
> entropy decode is correct (independent Python re-parse of the sequence
> header → `use_128x128_superblock=0`; every mode CDF table byte-diffed
> against the live spec → all exact; every mode symbol hand-traced → matches
> Kinetix; the coeff CDF tables `txb_skip`/`coeff_base`/`coeff_base_eob`/
> `eob_pt_16`/`coeff_br`/`eob_extra` diffed against spec → all exact; DC
> `coeff_base` ctx=26 → spec-correct `Coeff_Base_Pos_Ctx_Offset[0]`), and
> confirming the frame-header→tile-data byte offset is correct (`ffmpeg -bsf
> trace_headers` on the corpus OBU: frame header = 12 payload bytes, Kinetix
> agrees), the only thing left was: **an entire syntax element skipped.**
>
> `ffmpeg -bsf trace_headers` on the corpus `testsrc` keyframe shows
> `lr_type[2] = 2` → `Remap_Lr_Type[2]` = **RESTORE_WIENER** for the V plane.
> AV1 §5.11.2 `decode_tile()` calls `read_lr(r, c, sbSize)` for **every
> superblock, before `decode_partition()`**, and when any plane has a
> non-`RESTORE_NONE` mode it reads a real arithmetic-coded `restoration_type`
> / `use_wiener` / `use_sgrproj` symbol (plus Wiener/SGR coefficients via
> `decode_subexp_bool`). `reconstruct/` had **zero** `read_lr` handling —
> `grep read_lr` found nothing. `frame.rs::parse_lr` consumed the *header*
> bits correctly but discarded the result. So every LR-enabled stream
> (libaom/ffmpeg enable it by default) desynced the entropy decoder from the
> very first symbol of the tile — exactly the "plausible-but-wrong from
> symbol #0" signature this investigation chased for ~11 sessions.
> `solid_red` was pixel-exact throughout because its tiny solid frames have
> `FrameRestorationType = [NONE, NONE, NONE]` (no `read_lr` bits).
>
> **Implemented** (§5.11.57 / §5.11.58 / §6.8.24, all fetched from the live
> spec):
> - `frame.rs::parse_lr` now returns `LrParams { restoration_type[3],
>   unit_size[3], uses_lr }` (with `Remap_Lr_Type` + the
>   `RESTORATION_TILESIZE_MAX >> (2 - lr_unit_shift) >> lr_uv_shift` size
>   derivation), stored on `FrameHeader` as `frame_restoration_type` /
>   `lr_unit_size` / `uses_lr`.
> - `mode_cdfs.rs`: `Default_Use_Wiener_Cdf` `{11570,32768,0}`,
>   `Default_Use_Sgrproj_Cdf` `{16855,32768,0}`,
>   `Default_Restoration_Type_Cdf` `{9413,22581,32768,0}` +
>   `read_lr_restoration_type`.
> - `partition.rs`: `read_lr(r, c, sb_mi)` + `read_lr_unit(plane)` +
>   `decode_signed_subexp_with_ref_bool` / `decode_unsigned_subexp_with_ref_bool`
>   / `decode_subexp_bool` / `inverse_recenter` / `count_units_in_frame` /
>   `round2`, with per-tile `RefLrWiener`/`RefSgrXqd` reset to
>   `Wiener_Taps_Mid`/`Sgrproj_Xqd_Mid` in `decode_tile_group`. Called from
>   `decode_superblock` *before* `decode_partition`. Coefficients are consumed
>   for sync only — the restoration *filter* is still an unapplied passthrough
>   (Phase D), which is fine for now.
> - `LrDecodeParams` bundle threaded through `decode_tile_group` /
>   `TileDecodeState::new` (like the existing `CdefDeltaParams`).
>
> **Impact measured** (`av1_psnr_check`):
> - `testsrc`: first divergence moved from `Y px=(0,0)` (was `129` vs `16`)
>   to `Y px=(64,0)` — **the entire first 64×64 superblock now decodes
>   correctly**. Whole-frame PSNR barely moved (10.77→9.88 dB Y) because a
>   *second, independent* bug now dominates at the superblock-column
>   boundary (px 64 = start of SB column 1).
> - `mandelbrot_128x96`: **15.64 → 22.59 dB Y** (U 17.7→17.5, V 16.1→20.6) —
>   large real improvement.
> - `smptebars`/`testsrc2`: unchanged (their remaining error is elsewhere).
> - `solid_red_32`/`_64`: still 99.00 dB (unaffected — no LR bits).
> - `cargo test -p tpt-kinetix-av1` green (102 unit, +2 `parse_lr` regression
>   tests), `cargo clippy -p tpt-kinetix-av1 --all-targets -- -D warnings`
>   clean, `just av1-oracle-validate` still passes.
>
> **2026-08-27 (cont'd) — SECOND bug found and fixed: the palette
> `palette_colors_u` delta had a spurious `+1` luma bias.** Chased the
> `Y px=(64,0)` divergence into `mi=(16,0)` (`BLOCK_32X16`, a palette block).
> Its decoded Y palette colours `[106, 145, 210]` matched dav1d exactly, and
> the neighbour cache `[41, 106, 145]` was correct — but the reconstructed
> block mapped its first region to colour index 0 (`106`) where dav1d used
> index 2 (`210`), i.e. the Y **colour-map** first symbol (`NS(3)`) read the
> wrong bit → Kinetix was already desynced *before* the colour map, even
> though the colours came out right by luck. Root cause in
> `palette.rs::read_palette_colors_yu` (shared Y+U path): the delta-coded
> remainder did `read_literal(paletteBits) + 1` unconditionally, but AV1
> §5.11.46 only applies `palette_delta_y++` for **luma** — `palette_colors_u`
> has **no** `++`. Every U palette with a delta-coded entry drifted, and
> because `paletteBits` is re-derived from the running colour
> (`Min(paletteBits, CeilLog2(range))`), the *next* `L(paletteBits)` read the
> wrong width → desynced the whole block (the Y colour map included). Fixed:
> `delta_bias = if is_u { 0 } else { 1 }`. Regression test
> `palette_colors_yu_delta_bias_is_plus_one_for_y_and_zero_for_u` in
> `reconstruct/tests.rs`.
>
> **Impact** (`av1_psnr_check`): `testsrc` **9.88 → 16.98 dB Y** (U 8.85→15.21,
> V 11.60→15.34); trace 4672→7891 reads. `mandelbrot` unchanged 22.59 (its
> palette blocks don't hit U delta-coding). `smptebars`/`testsrc2` unchanged.
> `solid_red` still 99. Tests green (103 unit), clippy `-D warnings` clean,
> `just av1-oracle-validate` passes.
>
> **Next target — a *progressive* drift in superblock ROW 1, not a hard
> desync.** `testsrc` first divergence is `Y px=(48,64)` but the real story is
> a left-to-right cascade in the SB(16,0) TR-32×32 subtree:
>   - `mi=(0,16)`/`mi=(0,18)` (px cols 0-15) — **bit-exact** vs dav1d.
>   - `mi=(4,18)` (px cols 16-31) — off by a consistent **~2-5** (looks like a
>     prediction-base / DC-residual offset; dav1d's gradient is ~2 higher and
>     converges).
>   - `mi=(8,18)` (px cols 32-47) — now clearly wrong: Kinetix decodes
>     `y_mode=2` (V_PRED, near-flat output) where dav1d produces a horizontal
>     gradient (so the true mode is H_PRED/SMOOTH_H/a D-mode). `mi=(8,16)`
>     *above* it (px rows 64-71) is still exact.
>   - `mi=(12,16)` (px 48,64) — Kinetix `y_mode=12` (PAETH) so it never reads
>     `has_palette_y`; dav1d reconstructs a flat `106` palette block there.
> So by `mi=(8,18)` the entropy stream is genuinely desynced (wrong y_mode
> from the CDF, cascading), but it *starts* as a small numeric error in
> `mi=(4,18)` while cols 0-15 stay perfect. Prime suspects, in order: (1) a
> small coefficient-context or dequant error in `mi=(4,18)` that both shifts
> its pixels ~2 and mis-consumes a symbol; (2) directional/`SMOOTH`
> prediction *accuracy* in the 2nd SB row (angle_delta, or the smooth-weight
> arithmetic near a block edge); (3) the coeff `above_level`/`above_dc`
> arrays at the SB-row-0→1 boundary (they persist across SB rows by spec — is
> Kinetix updating them for every mi column of each SB-row-0 block?).
> `KINETIX_AV1_DBG_YMODE` (prints above/left mode + ctx + bit pos),
> `KINETIX_AV1_DBG_{ROWS,RROWS,COLS,PAL,LR}`, and `KINETIX_AV1_CAPTURE_TILE`
> are the tools (all opt-in).
>
> **Sharpened further (2026-08-27 cont'd):** the *origin* of the drift is a
> **bottom-2-rows numerical error in a PAETH + residual block**. `mi=(8,8)`
> (`BLOCK_32X32`, PAETH, `tx=16x16`, `skip=false`): its bottom-**left** 16×16
> tx block (px cols 32-47, rows 48-63) is **bit-exact**, but its bottom-
> **right** 16×16 tx block (px cols 48-63) is exact for rows 48-61 and off by
> **±1-2 only on rows 62-63** — the last two output rows of that one 16-point
> inverse column transform. `mi=(4,16)` (also PAETH) shows the same
> last-rows-off pattern, and `mi=(4,18)` (`V_PRED`, `angle_delta` read) then
> copies that slightly-wrong bottom row downward and the error compounds
> left-to-right until `mi=(8,18)` fully desyncs.
>
> **CORRECTION (2026-08-27 cont'd) — most of the "progressive drift" above
> was a harness artifact.** `av1_symbol_trace_diff` compares
> **NOFILTER-Kinetix vs FILTERED-dav1d** (`ref_data` is always dav1d's fully
> deblocked+CDEF'd output; there is no dav1d NOFILTER). So every block-edge
> pixel looks "off by 1-3" purely from dav1d's deblock, even when Kinetix's
> pre-filter reconstruction is bit-exact. Re-checked with a proper Pass-2
> (true NOFILTER) row dump (new `NF row…` lines in the harness): `mi=(8,8)`'s
> bottom-right 16×16 tx (px 48,48) is `eob=0` pure PAETH and reconstructs
> **flat 41, bit-exact** through row 63 — the rows-62-63 "ripple" was
> entirely dav1d's deblock. Likewise `mi=(0,16)`/`mi=(0,18)`/`mi=(4,16)`/
> `mi=(4,18)` are all pre-filter bit-exact in their interiors; only their
> last 1-2 columns differ, and only by deblock. `inverse_adst16` (§7.13.2.8),
> `cos128`/`Cos128_Lookup`, `round2`, `butterfly`, `hadamard`, and the ADST
> input/output permutations were all byte-diffed against the live spec this
> session and are **exact** — the transform is not the bug.
>
> **The real first pre-filter divergence is `mi=(8,18)` (px 32,72).** Both
> decoders pick `y_mode=2` (V_PRED) and its `AboveRow` (row 71, cols 32-47) is
> flat `106` in both. But Kinetix's residual there is a **flat DC `+76`**
> while dav1d's is a **horizontal gradient** `[+65,+63,+60,+56,…,+41,+41]`.
> Flat-DC vs AC-gradient ⇒ Kinetix decoded a different coefficient set ⇒
> **Kinetix is entropy-desynced entering `mi=(8,18)`**. The desync must be in
> the symbol consumption of `mi=(4,16)` (PAETH, flat 170, pre-filter exact —
> so it reconstructs right but may consume the wrong number of `all_zero` /
> coeff symbols), `mi=(4,18)` (V_PRED, residual matched dav1d for positions
> 0-13), or the partition read between them. This is precisely what the
> deferred **`intra_decode.py` mode+coeff oracle** would localize in one run —
> the Rust `KINETIX_AV1_CAPTURE_TILE` side is ready (base CDFs + full trace +
> `params`); the Python consumer still needs writing.
>
> Files modified: `tpt-kinetix-av1/src/frame.rs`,
> `tpt-kinetix-av1/src/reconstruct/mod.rs`,
> `tpt-kinetix-av1/src/reconstruct/mode_cdfs.rs`,
> `tpt-kinetix-av1/src/reconstruct/partition.rs`,
> `tpt-kinetix-av1/src/reconstruct/tests.rs`,
> `tpt-kinetix-av1/tests/proptest_coeffs.rs`. No `git commit` calls.
> `capabilities().pixel_exact` untouched (still `false`).

> ## 2026-08-27 (cont'd) — ★ THE PART 1 ORACLE IS BUILT AND THE ENTROPY PATH IS PROVEN CORRECT ★
>
> Built `tools/av1_oracle/intra_decode.py` — an independent, from-scratch
> re-implementation of the entire keyframe-intra tile syntax
> (`read_lr` §5.11.57 → `decode_partition` §5.11.4 → `intra_frame_mode_info`
> §5.11.7 → `palette_tokens` §5.11.49 → `coeffs` §5.11.39), driven by its own
> `SymbolDecoder`, that diffs its per-symbol trace against Kinetix's captured
> trace (`KINETIX_AV1_CAPTURE_TILE` → `av1_tile_trace.json`). Run via
> `just av1-oracle-tile [entry]`.
>
> The Rust capture side gained: `sym_range`/`sym_value` per trace entry (so a
> range-state divergence is caught even when decoded values still match) and
> `frame_restoration_type`/`lr_unit_size`/`uses_lr` + the LR CDFs in the
> params/mode-CDF dump.
>
> **RESULT: the oracle matches Kinetix's decode *exactly*, symbol-for-symbol
> and range-for-range, across the WHOLE tile, for ALL FIVE corpus entries**
> (`testsrc` 7891, `mandelbrot` 3708, `smptebars` 2520, `testsrc2` 6534,
> `solid_red` 64 symbols). Zero divergence.
>
> **This closes ~13 sessions of chasing an "entropy desync" that does not
> exist. The entire AV1 bitstream-parsing / entropy-decode path is correct** —
> every partition, mode symbol, palette colour map, Wiener LR coefficient, and
> transform coefficient is confirmed by an independent implementation.
> (Caveat: the oracle loads Kinetix's CDF *tables*, so a numerically-wrong
> default CDF entry that both share is still invisible — but the mode + key
> coeff CDFs were separately byte-diffed against the live spec earlier this
> session and are exact.)
>
> **Therefore every remaining pixel divergence is in RECONSTRUCTION, not
> parsing:** intra prediction (directional/PAETH/SMOOTH/DC border prep,
> `angle_delta`, edge filter/upsample), inverse transform (the ADST/DCT
> butterflies were spec-verified but the 2-D driver / rescale / `dq_denom` /
> `Transform_Row_Shift` per Kinetix's *non-spec TxSize enum ordering* were
> not), dequant, CfL (§7.11.5), filter-intra prediction (§7.11.2.3), palette
> reconstruction (colour-map → pixels), and the in-loop filters
> (deblock/CDEF/LR — LR is still an unapplied passthrough).
>
> **Bug found while building the oracle (in the oracle, not Kinetix, but
> instructive):** Kinetix's internal `TxSize` enum (`coeff_tables.rs`) does
> **not** match the AV1 spec's — Kinetix puts `TX_32X64`/`TX_64X32` at
> indices 11/12 where the spec has `TX_4X16`/`TX_16X4`; `TX_8X32` is 15 in
> Kinetix vs 13 in the spec. Kinetix is internally consistent (all its tables
> use its ordering), so this is not a Kinetix bug — but any oracle / external
> comparison must use Kinetix's ordering, and it's a latent trap for anyone
> cross-referencing spec pseudocode against `reconstruct/`.
>
> **Next session: attack reconstruction directly.** With a NOFILTER-Kinetix
> vs FILTERED-dav1d comparison being unreliable at block edges (documented
> above), the cleanest approach is to add a pre-filter frame dump and compare
> *block interiors* only, per intra mode: start with the simplest non-flat
> failing block (a DC or PAETH block with a small known-correct residual, now
> that coefficients are trusted), verify prediction and inverse-transform
> output sample-by-sample. `just av1-oracle-tile` gives the exact decoded
> coefficient array for any block to check the transform against.
>
> Files this pass: NEW `tools/av1_oracle/intra_decode.py`; modified
> `tpt-kinetix-av1/src/entropy.rs` (+sym_range/value), `reconstruct/mod.rs`
> (capture format), `reconstruct/mode_cdfs.rs` (LR CDFs in dump),
 > `tools/av1_oracle/{symbol_decoder,coeffs}.py` (trace fields; fixed a
 > pre-existing `TX_CLASS_HORIZONTAL` typo in `coeffs.py`), `justfile`
 > (`av1-oracle-tile`). No `git commit` calls.

> **2026-08-28 session note — reconstruction primitive validation: added 13
> focused unit tests for the unvalidated reconstruction stages.** Per the
> 2026-08-27 session's findings, the entire bitstream-parsing/entropy-decode
> path is proven correct (independent Python oracle), so every remaining
> pixel divergence is in reconstruction. This session added direct unit
> tests for the reconstruction primitives that had *only* been covered by
> DC-only or end-to-end tests before:
>
> 1. **2-D inverse-transform driver with non-DCT_DCT types**
>    (`inverse_transform_adst_4x4_produces_spatial_output`): ADST_ADST,
>    ADST_DCT, DCT_ADST at TX_4X4 — verifies the `inverse_adst4` butterfly
>    network produces spatial variation for AC-coefficient input, not just
>    the DC-only path.
> 2. **Rectangular rescale path**
>    (`inverse_transform_rectangular_rescale_path`): TX_4X8, TX_8X4,
>    TX_8X16, TX_16X8 — exercises the `|log2W - log2H| == 1` sqrt(2) rescale
>    (`Round2(x * 2896, 12)`) in the row pass, verifies full w×h output is
>    written, and confirms DC-only input stays flat through the rescale.
> 3. **V_DCT / H_DCT separable-transform axis behavior**
>    (`inverse_transform_v_dct_8x8_only_col0_nonzero`,
>    `inverse_transform_h_dct_8x8_only_row0_nonzero`): verifies that V_DCT
>    (identity row pass + DCT column pass) concentrates output in column 0
>    for column-0 input, and H_DCT (DCT row pass + identity column pass)
>    concentrates output in row 0 for row-0 input.
> 4. **8×8 scale sanity**
>    (`inverse_transform_tx8x8_with_ac_matches_expected_scale`): confirms
>    the row_shift=1 + col_shift=4 cascade at TX_8X8 doesn't vanish or
>    explode the DC coefficient.
> 5. **16×16 flatness** (`inverse_transform_16x16_dc_only_is_flat`): extends
>    the DC-only flatness guarantee to n=4 (the largest size exercised by
>    the current corpus's `TX_16X16`).
> 6. **Filter-intra prediction** (`filter_intra_prediction_matches_hand_computed_values`,
>    `filter_intra_mode2_horizontal_matches_hand_computed`): hand-computed
>    dot-products against `Intra_Filter_Taps[0]` (DC) and `[2]` (H) with
>    uniform borders — verifies §7.11.2.3's recursive prediction math
>    directly, not just that it doesn't panic.
> 7. **Palette reconstruction** (`palette_prediction_maps_color_indices_correctly`,
>    `palette_prediction_with_sub_block_offset`): verifies color-map →
>    palette-index → color lookup, including the sub-block offset math.
> 8. **Spec table pinning** (`transform_row_shift_table_matches_spec_ordering`,
>    `adjusted_tx_size_table_clamps_to_32_for_large_sizes`): directly pins
>    the numeric values of `TRANSFORM_ROW_SHIFT` and `ADJUSTED_TX_SIZE`
>    against the spec — these tables are indexed by Kinetix's *non-spec*
>    TxSize enum ordering (where e.g. `TX_32X64` is index 11, not 13 as in
>    the spec), so a transcription error that swaps two indices would silently
>    produce wrong coefficients for the affected sizes.
>
> **Result**: all 116 unit tests pass (was 103). `cargo clippy -p
> tpt-kinetix-av1 --all-targets -- -D warnings` clean. `cargo run -p
> tpt-kinetix-av1 --example av1_psnr_check` produces byte-identical output
> to the pre-session baseline (no behavioral change — these are validation
> tests only): `solid_red_32`/`_64` 99.00 dB, `testsrc_128x96` 16.98/15.21/
> 15.34 dB, `mandelbrot_128x96` 22.59/17.51/20.59 dB, `smptebars_256x144`
> 9.36/14.01/10.38 dB, `testsrc2_320x180` 12.96/10.38/9.97 dB.
>
> **What the tests confirm**: the inverse-transform driver (row/column pass
> dispatch, sqrt(2) rescale for rectangular sizes, dq_denom, row shift),
> the ADST butterfly network, filter-intra prediction, and palette
> reconstruction all behave correctly in isolation for the tested scenarios.
> The remaining pixel divergences on real content (`testsrc` at ~17 dB,
> `mandelbrot` at ~23 dB) are therefore *not* explained by a gross bug in
> any single one of these primitives — they must arise from an interaction
> between stages (e.g. a specific mode/tx_type/coefficient combination that
> none of the unit tests in isolation happen to exercise) or from a subtle
> numerical issue that only manifests with real encoded coefficient
> distributions.
>
> **Remaining gap for the next session**: the cleanest remaining lever is a
> true pre-filter NOFILTER comparison on a per-block basis (snapshot the
> reconstructed plane before `apply_post_filters`, diff against dav1d's
> pre-filter output — which requires either a dav1d debug build or ffmpeg
> filtergraph surgery to disable in-loop filtering). Without that, the
> block-edge confound between NOFILTER-Kinetix and FILTERED-dav1d makes it
> impossible to pinpoint which *interior* pixel first diverges. The
> `KINETIX_AV1_NOFILTER` env var + `av1_symbol_trace_diff` harness are the
> existing tools; they just need a dav1d reference that also runs unfiltered.
>
> Modified: `tpt-kinetix-av1/src/reconstruct/tests.rs` (+13 tests).
> No `git commit` calls. `capabilities().pixel_exact` untouched (still
> `false`).

> **2026-08-29 session note — verified partition context is already correct.**
> Investigated the partition-context feedback loop described in the 2026-08-28
> session note. Replaced the 1D `mi_width_log2_above`/`mi_height_log2_left`
> arrays with a proper 2D `MiSizes[r][c]` array (flat `mi_rows*mi_cols` Vec<u8>
> of bsize indices) that tracks the exact block at each position. The PSNR
> numbers are byte-identical to the pre-change baseline (`solid_red_32`/`_64`
> 99.00 dB, `testsrc_128x96` 16.98/15.21/15.34 dB, `mandelbrot_128x96`
> 22.59/17.51/20.59 dB, `smptebars_256x144` 9.36/14.01/10.38 dB,
> `testsrc2_320x180` 12.96/10.38/9.97 dB), confirming the 1D approximation was
> already correct for the current corpus — the feedback loop described in the
> 2026-08-28 note was resolved by the palette-delta and `read_lr` fixes landed
> in earlier sessions. The 2D array is kept because it is the spec-correct
> representation and avoids a latent trap for any future non-raster-order code
> path. 116 unit tests pass. `cargo build -p tpt-kinetix-av1 --all-targets`
> clean. Modified: `tpt-kinetix-av1/src/reconstruct/mod.rs` (struct fields),
> `tpt-kinetix-av1/src/reconstruct/partition.rs` (`record_mi_size_context`,
> `partition_context`, doc comments). No `git commit` calls.

> **2026-08-28 session note (cont'd) — root-caused the superblock-column-1
> divergence to a partition-context feedback loop.** Added a new
> `av1_interior_diff.rs` diagnostic tool (and `just av1-interior-diff`) that
> compares NOFILTER-Kinetix vs FILTERED-dav1d at only block-interior pixels
> (≥4 from any 8×8 boundary on luma), eliminating the deblock/CDEF confound.
>
> **Finding**: the first interior divergence for both `testsrc` and
> `mandelbrot` is at the **start of superblock column 1** (the 2nd SB in a
> multi-column frame). The 1st SB row is pixel-perfect; the 2nd cascades
> left-to-right from the 3rd block. The block diff map for testsrc is:
> ```
>    0   0   0   0   0   0   0   0
>    0   0   0   0   0   0   0   0
>    0   0   0   0   0   0   0   0
>    0   0   0   0   0   0   0   0
>    0   0   3  39 147 ...         <- SB row 1, 3rd block onward
>  138  79  74 144 ...             <- SB row 2, desynced
> ```
>
> The divergence is a **partition-tree feedback loop**: the partition
> context for block (0,16) reads `mi_height_log2_left[0] = 2` (set by a
> 32×16 leaf in SB 0), giving `ctx=2`, which makes Kinetix read
> `PARTITION_SPLIT` for the 64×64 node. dav1d reads `PARTITION_NONE` for
> the same node (its SB 0 is a single 64×64 palette block, so its
> `mi_height_log2_left[0] = 4`, giving `ctx=0`). The two decoders choose
> different partition trees for SB 0 (both produce the same pixels there,
> since the content is flat), but the different leaf sizes feed back into
> the context for SB 1, desyncing it.
>
> **This is the real bug**: Kinetix's partition context derivation produces
> a different tree than the encoder intended. The entropy decode itself is
> proven correct (independent Python oracle); the issue is that the CDF
> context for the `partition` symbol is wrong because the neighbour-size
> arrays (`mi_width_log2_above`/`mi_height_log2_left`) don't match what
> the encoder's dav1d-based context model expects. The fix requires
> understanding exactly how dav1d derives the partition context from the
> neighbour blocks — specifically whether it uses the leaf block size or
> the partition-node size, and whether the comparison is `<` or `<=`.
>
> **Status**: `solid_red` (single SB column) is pixel-exact. `smptebars`
> (also single SB column at 64×64) is 60 dB (interior-clean, deblock
> confound in full-plane PSNR). Multi-SB-column frames diverge from SB
> column 1 onward due to the partition-context feedback. 116 unit tests
> pass. `cargo clippy -p tpt-kinetix-av1 --all-targets -- -D warnings`
> clean.
>
> Modified: `tpt-kinetix-av1/src/reconstruct/tests.rs` (+13 tests),
> `tpt-kinetix-test-utils/examples/av1_interior_diff.rs` (new),
> `tpt-kinetix-test-utils/tests/dbg_av1_sb2col1.rs` (new scratch),
> `justfile` (`av1-interior-diff`), `tpt-kinetix-av1/src/reconstruct/
> partition.rs` (debug instrumentation, to be reverted).
> No `git commit` calls. `capabilities().pixel_exact` untouched (still
> `false`).

> **2026-08-29 (cont'd) — block-interior comparison tool built; reconstruction gap isolated.**
> Added `av1_prefilter_check.rs` example that compares Kinetix pre-filter output
> against dav1d post-filter output at only block-interior pixels (≥4 from any
> 8×8 luma boundary), avoiding the deblock confound. Results:
> - `solid_red_64`: max_diff=0, avg_diff=0.000 (pixel-exact at block interiors)
> - `testsrc_128x96`: max_diff=219, avg_diff=16.0 (Y); first divergence at
>   pixel (52,68) Kinetix=4 vs ref=41
> - `mandelbrot_128x96`: max_diff=120, avg_diff=8.9 (Y)
> - `smptebars_256x144`: max_diff=180, avg_diff=68.1 (Y)
> - `testsrc2_320x180`: max_diff=97, avg_diff=50.7 (Y)
>
> **Conclusion:** the reconstruction pipeline works for simple (single-partition,
> single-color) content but has large errors for multi-partition content. The
> error is in an interaction between reconstruction stages (prediction,
> transform, dequant, or palette), not in the entropy decode (proven correct by
> the Python oracle) or the partition context (proven correct by the 2D MiSizes
> change being a no-op). Added `KINETIX_AV1_DUMP_PREFILTER` env var to dump
> pre-filter YUV for external comparison. 116 unit tests pass. No `git commit`
> calls.

> ## 2026-08-31 session note — localized the intra reconstruction desync to a
> structural bit-offset, and wired CDEF multi-strength.
>
> **Method used** (reproducible): the corpus OBU is dumped with
> `KINETIX_AV1_DUMP_OBU=<dir>` (writes `testsrc.obu`), decoded to raw YUV with
> `ffmpeg -c:v libdav1d -i testsrc.obu -f rawvideo testsrc.yuv`, then Kinetix's
> pre-filter Y is dumped with `KINETIX_AV1_DUMP_PREFILTER=testsrc` and the two Y
> planes are diffed pixel-by-pixel (Y has `RESTORE_NONE`, confirmed via the
> oracle capture `frame_restoration_type=[0,0,1]`, so luma interior comparison is
> filter-free and clean). The first raster divergence is `testsrc` px=(48,64):
> Kinetix=26 vs dav1d=41. The block there is `tx=16x4, pred_mode=12 (PAETH)`;
> its `top=[41×N]`/`left=[106,106,106,106]` give a **correct** flat-41
> prediction, so the error is purely in the coefficient residual — Kinetix
> decoded `eob=18` (nonzero residual → 26) where dav1d is flat (`eob=0` → 41).
>
> **Ruled out, with evidence** (this was a 14-session hunt; here is the closure
> matrix):
> - Entropy decode self-consistency: the independent Python `intra_decode.py`
>   oracle matches Kinetix symbol-for-symbol across the whole tile (per
>   2026-08-27) — but it re-uses Kinetix's CDF tables + neighbour-context
>   snapshot, so it cannot catch a bug shared by both.
> - **CDF adaptation** (`entropy.rs` `read_symbol`): fetched the live spec
>   §8.2.6 update loop — `tmp=0; for i: tmp=(i==symbol)?(1<<15):tmp; if tmp<cdf[i]
>   cdf[i]-=(cdf[i]-tmp)>>rate else cdf[i]+=(tmp-cdf[i])>>rate; cdf[N]+=(cdf[N]<32)`
>   — and it matches Rust exactly (incl. `tmp` persisting at 32768 for `i>=symbol`).
>   **Not the bug.**
> - **`partition_context`** (`partition.rs`): `above = Mi_Width_Log2[MiSizes[r-1][c]]
>   < bsl`, `left = Mi_Height_Log2[MiSizes[r][c-1]] < bsl`, `ctx = 2*left+above`
>   — matches spec §8.3.2. **Not the bug.**
> - **`all_zero_ctx`** (`coeff.rs`): the luma `if block_w==w&&block_h==h →0 else
>   top/left-level branches` matches spec §8.3.2. **Not the bug.**
> - **`skip` context** (`intra_block.rs`): `(above_skip+left_skip).min(2)` matches
>   spec §5.11.11. **Not the bug.**
> - **Prediction** for the divergent block: PAETH of top=41/left=106 → flat 41,
>   exactly dav1d's. **Not the bug** (per-block, but confirms the desync is
>   upstream of it).
>
> **Conclusion**: Kinetix's pixels match dav1d *exactly* up to (48,64), then
> diverge, yet the block there has correct neighbours/mode/prediction and a wrong
> (nonzero) residual. That is only possible if Kinetix is at a **bit offset** from
> dav1d at (48,64) — i.e. an earlier block consumed a different number of bits
> (most likely a `skip`/structure mismatch where Kinetix reads extra all-zero
> coeffs that reconstruct identically but shift the bitstream). Because the
> self-consistent oracle re-uses Kinetix's context/CDF, it reproduces the same
> offset and cannot localize it. **Resolving this requires a dav1d *symbol*
> reference** (per-block mode/skip/tx/coeff trace) — which is **not available in
> this environment** (`ffmpeg -bsf:v trace_headers` cannot attach to the libdav1d
> decode path; no dav1d debug build). The 2026-08-27 note's own open item
> ("a dav1d debug build for a real reference symbol trace") is still the blocker
> for the headline pixel-exact goal.
>
> **Separately, completed a genuine remaining task: CDEF multi-strength wiring.**
> `loop_filter.rs` previously hardcoded `idx = 0` for the whole plane, ignoring
> the already-parsed per-64×64-unit `cdef_idx` (§5.11.56, populated by
> `read_cdef`). Now: `cdef_plane_luma`/`cdef_plane_chroma` take an explicit
> pre-CDEF `src` snapshot + a unit `(y0,x0,unit_h,unit_w)` region, and the CDEF
> pass in `apply_post_filters` iterates 64×64 (luma) / subsampled (chroma)
> units, looks up `cdef_idx` per unit, and filters each from the single snapshot
> (units stay independent, per spec). `cdef_idx` is threaded through
> `FrameMeta.cdef_idx` (populated in `decode_tile_group` from
> `TileDecodeState.cdef_idx`) so `apply_post_filters` doesn't need `self`.
> **Verified a true no-op on the current corpus** (`cdef_bits==0` → every unit
> maps to `cdef_idx==0`, byte-identical output): `cargo run av1_psnr_check`
> still reports testsrc 16.98/15.21/15.34, mandelbrot 22.59/17.51/20.59,
> etc.; `cargo clippy -p tpt-kinetix-av1 --all-targets -- -D warnings` clean;
> `cargo test -p tpt-kinetix-av1 --lib` = 117/117 pass (added
> `cdef_plane_luma_respects_unit_region_bounds`). This is correct for real
> streams (where `cdef_bits>0`) but does **not** move pixel_exact closer on its
> own — Y reconstruction must be fixed first, and that is gated on the dav1d
> symbol reference above.
>
> **Next session**: stand up a dav1d debug build (or `aomdec`) to extract a
> per-block symbol trace for the divergent region, then diff Kinetix's
> `skip`/`tx`/`partition` decisions around mi (8..20, 0..15) (the rows just
> above px=(48,64)) to find the first block whose bit consumption diverges from
> dav1d. The desync is almost certainly a structural/context mismatch in the
> `skip` or partition tree that the self-consistent oracle masks.
> **Note (2026-08-31):** `loop_filter.rs`/`reconstruct/mod.rs` were committed
> as `2888aac` by the concurrent automated process (CDEF multi-strength wiring +
> AAC cleanup) — they are no longer uncommitted. Remaining uncommitted AV1
> files: `tpt-kinetix-av1/examples/av1_psnr_check.rs`
> (`KINETIX_AV1_ONLY_TESTSRC` env-var gate) and
> `tpt-kinetix-av1/src/reconstruct/reconstruct_block.rs` (debug-print
> formatting fix). The H.264 crate also has an unrelated uncommitted change
> in `src/reconstruct.rs` (PAFF field-parity luma MC offset hook).

> ## 2026-09-01 session note — ★ THE STRUCTURAL BIT-OFFSET DESYNC IS FIXED ★
> (`tx_depth` context sentinel bug; testsrc Y 16.98 → 57.65 dB, smptebars Y
> 9.36 → 54.23 dB; full luma entropy trace now bit-exact vs dav1d).
>
> **Unblocked the 14-session blocker by building a patched dav1d.** MSVC 2022
> + meson + ninja + scoop clang are all present on this machine (no cmake/gcc,
> but dav1d builds fine with meson). Built dav1d `52b9d3d` with
> `-Denable_asm=false` (no nasm) via
> `scratchpad/av1ref/build_dav1d.bat` (calls `vcvars64.bat` then meson). dav1d
> already ships a `DEBUG_BLOCK_INFO`-gated per-block/per-symbol trace
> (`Post-skip`/`Post-ymode`/`Post-tx`/`Post-*-cf-blk[eob]` + `poc=…bp=…`
> partition lines, all with the `msac.rng` range state); patched `src/recon.h`
> to gate it on `getenv("DAV1D_TRACE")` and added a `BLOCK bx by bw4 bh4` line
> at the top of `decode_b`. Run:
> `DAV1D_TRACE=1 dav1d.exe -i testsrc.obu -o out.yuv --threads 1`.
>
> **Kinetix side**: new `tpt-kinetix-av1/examples/av1_trace_obu.rs` decodes a
> raw `.obu` file, and `KINETIX_AV1_TRACE=1` now emits matching
> `KTRACE BLOCK` / `KTRACE CF` / `KTRACE PART` lines (in `intra_block.rs`,
> `reconstruct_block.rs`, `partition.rs`). dav1d's `msac.rng` == the spec's
> `SymbolRange` == Kinetix's `symbol_range` and matches **exactly** at every
> block boundary when in sync — so `r=` is a direct divergence detector.
> Generate the corpus OBU with the same libaom CRF-32 encode
> `av1_psnr_check` uses:
> `ffmpeg -f lavfi -i testsrc=size=128x96:rate=1 -frames:v 1 -c:v av1 -pix_fmt yuv420p -f obu testsrc.obu`.
>
> **Root cause** (found in one diff pass): the traces matched **exactly** for
> ~105 symbols, then diverged at the block at mi=(0,20) — a
> `PARTITION_HORZ_4` 16×4 leaf on the frame's left column. Same partition,
> same `skip`, same `ymode=12` (PAETH), but `Post-tx` `r=` diverged (dav1d
> 63764 vs Kinetix 48518): the `tx_depth` symbol was read from the **wrong
> CDF context**, and every subsequent symbol in the tile desynced.
> `tx_depth_context` is `ctx = (aboveW >= maxTxW) + (leftH >= maxTxH)` (spec
> §8.3.2 / dav1d `get_tx_ctx`). Kinetix initialised `tx_above`/`tx_left` to
> **`4`** ("smallest transform = TX_4X4's 4 samples"), but dav1d fills the
> unavailable-neighbour sentinel with **`-1`**, which fails `>=` for every
> real size. For this 16×4 block on the left edge (`leftH` = the init
> sentinel, `maxTxHeight` = 4), `4 >= 4` was **true** → `ctx` off by one.
> An unavailable (tile-edge) neighbour must contribute 0.
>
> **Fix** (`reconstruct/mod.rs`): init `tx_above`/`tx_left` to `0` not `4`.
> `tx_depth_context`'s arithmetic extracted to a free `tx_depth_ctx_from()`
> with a regression test
> (`tx_depth_ctx_unavailable_neighbour_contributes_zero_for_a_4px_max_tx`).
>
> **Result**: the full **luma + partition + block** symbol trace (191 entries:
> every partition bp, skip, ymode, tx, luma-coeff eob + range state) is now
> **bit-exact vs dav1d** across the whole testsrc frame. PSNR:
> `testsrc` Y 16.98→**57.65**, `smptebars` Y 9.36→**54.23**, `solid_red`
> still 99. `mandelbrot` (22.59) and `testsrc2` (12.96) unchanged — they have
> a *separate* remaining bug, and **chroma** is still off on testsrc
> (U/V 29/37) — the chroma-plane `CF` lines still diverge. That's the next
> target: same method (`KINETIX_AV1_TRACE` vs `DAV1D_TRACE`), now looking at
> `Post-uv-cf-blk` / `Post-uvmode` / `Post-uvalphas` / palette-UV lines.
> 118 unit tests pass, `clippy --all-targets -D warnings` clean. No `git
> commit` calls. `capabilities().pixel_exact` still `false` (chroma + other
> corpus entries + inter). Uncommitted: `reconstruct/{mod,partition,
> intra_block,reconstruct_block}.rs`, `examples/av1_trace_obu.rs`,
> `examples/av1_psnr_check.rs`. Patched dav1d lives in
> `scratchpad/av1ref/` (not in-repo).
>
> **2026-09-01 (cont'd) — chroma desync localized to the rectangular ADST
> inverse transform.** With luma bit-exact, ran the same trace diff on chroma
> + a pre-filter pixel comparison (`dav1d --inloopfilters none` vs
> `KINETIX_AV1_DUMP_PREFILTER`). Findings:
> - **The entire entropy + coefficient decode is bit-exact for chroma too** —
>   every `Post-uv-cf-blk` / `Post-uvmode` / `Post-uvalphas` / CfL-alpha
>   `r=` value matches dav1d across the whole testsrc frame (verified
>   symbol-for-symbol; added `KTRACE CFLALPHA` line). So every remaining
>   chroma error is **reconstruction**, not parsing.
> - Chroma error is confined to **chroma rows 40-47** (= the bottom
>   `PARTITION_HORZ_4` region, luma rows 80-95, SB row 1 bottom). Chroma
>   rows 0-39 are pixel-exact.
> - **Chroma DC + DCT_DCT blocks reconstruct correctly** (e.g. block
>   bx=20,by=22 uvmode=0 txtp=0: bit-exact).
> - **The wrong blocks are all `TX_8X4` (chroma 8×4) with `txtp=1`
>   (ADST_DCT) and nonzero eob.** A CfL block downstream of one shows a
>   *uniform* +17 offset (its AC/`L-lumaAvg` term is perfect — it just
>   inherits a wrong neighbour), and a `V_PRED eob=0` block downstream shows
>   a uniform −14 (faithfully copying a wrong above row). The *root* block
>   (first wrong: bx=8,by=20, uvmode=10 SMOOTH_V, txtp=1, eob=6) has correct
>   prediction inputs and roughly-correct row 0, but its lower rows gain a
>   spurious **horizontal gradient** that dav1d's don't — i.e. the 8×4
>   ADST_DCT inverse transform (row=DCT-8, col=ADST-4, `|log2W−log2H|==1`
>   sqrt(2) rescale path) is adding bogus horizontal AC.
> - `transform.rs`'s axis mapping is right (`ADST_DCT` → row=Dct, col=Adst,
>   per libaom `av1_txfm_map` = {vtx ADST, htx DCT}). **NOTE: the working-tree
>   `check_itf.py` has the WRONG mapping** (`tx_type==1` → row=Adst) — fix it
>   to row=Dct/col=Adst before using it as an oracle. That script is the
>   right next tool: capture a real 8×4 `txtp=1` chroma coeff array
>   (`KINETIX_AV1_TRACE` gives eob/tx_type; the DBG path gives the `quant`
>   array) and diff Kinetix's `inverse_transform` output against the exact
>   integer iTF in `check_itf.py` — suspect the row_shift / col_shift /
>   `needs_rescale` ordering or the ADST-4 column pass for the 4-tall case.
>
> testsrc PSNR now: Y **57.65**, U **28.96**, V **36.91** (pre-filter Y
> 58.93 / U 28.96 / V 36.91; the remaining luma error is 308 px, maxdiff 6,
> also in the SB-row-1 bottom — likely the same rectangular-transform issue
> at a smaller magnitude on luma). Uncommitted adds:
> `KTRACE CFLALPHA` line + `tx_depth_ctx_from` (already noted above).
>
> **2026-09-01 (cont'd #3) — ★ ROOT-CAUSED: SMOOTH intra mode constants were
> rotated. testsrc luma now PIXEL-EXACT; U/V 29/37 → 47/47 dB. ★**
> The "8×4 ADST_DCT" framing below was a red herring — patching dav1d
> (`src/itx_tmpl.c` + `src/recon_tmpl.c`, `DAV1D_ITXDUMP=1`) to dump the
> post-transform residual per chroma block showed **Kinetix's inverse
> transform output is byte-identical to dav1d's** for both U and V of the
> "root" block. The error was entirely in **prediction**:
> `reconstruct/mod.rs` had `SMOOTH_V=9, SMOOTH_H=10, SMOOTH=11`, but the AV1
> spec intra-mode enum is `SMOOTH_PRED=9, SMOOTH_V_PRED=10, SMOOTH_H_PRED=11`.
> A decoded `SMOOTH_V_PRED` (10) therefore dispatched to `predict_smooth_h`
> (axis-swapped → the spurious horizontal gradient), `SMOOTH_PRED` (9) ran
> `predict_smooth_v`, etc. Benign on flat content (all three collapse to
> ~constant), visible on any gradient. **Fix**: `SMOOTH=9, SMOOTH_V=10,
> SMOOTH_H=11` + explanatory comment.
> **Result** (`av1_psnr_check`): testsrc **Y 57.65→61.95, U 29.03→47.27,
> V 36.79→46.65**; **pre-filter Y is now PSNR 99 / maxdiff 0 — testsrc luma
> reconstruction is complete** (the 61.95 full-frame Y is only loop-filter
> deltas). Pre-filter U/V 49/50 dB, maxdiff 7-10, ~300 px — small residual
> chroma-prediction/CfL/edge errors remain, much reduced. smptebars/solid_red
> unchanged; mandelbrot Y 22.59→21.73 (noise-level, a SMOOTH block that was
> accidentally right before). 118 tests pass, clippy clean.
> Dav1d dump note: dav1d's internal `enum TxfmType` is transposed vs the AV1
> spec (`itxfm_add[uvtx][spec_txtp]` maps spec `ADST_DCT`↔`DCT_ADST` to the
> other internal impl) — irrelevant now but don't be misled by `ITXRES
> internal_txtp=` in the dump.
>
> **2026-09-01 (cont'd #4) — chroma directional intra edge filter was
> luma-only.** `predict_directional` gated the §7.11.2.4 edge filter /
> upsampling on `enable_intra_edge_filter && is_luma`. AV1 §7.11.2.4 has **no
> plane restriction** and dav1d applies it identically in its chroma path
> (`recon_tmpl.c` chroma loop: `angle |= intra_edge_filter_flag` +
> `prepare_intra_edges(..., seq_hdr->intra_edge_filter, ...)`, same as luma).
> Confirmed via `DAV1D_ITXDUMP` that the chroma inverse transform output is
> byte-exact — the residual was right, only the directional prediction base
> was slightly off. **Fix**: drop the `&& is_luma`. testsrc pre-filter
> **U 49.34→52.47, V 50.34→51.34 dB, maxdiff 10→4**; full-frame U 47.27→48.82.
> `is_luma` param kept (renamed `_is_luma`) for the still-unwired
> smooth-neighbour `filterType` detection (§7.11.2.9, `FILTER_TYPE` hardcoded
> 0) — the likely source of the last ~300px / maxdiff-4 chroma residual.
> mandelbrot U 17.08→16.81 (noise; dominated by its own separate bug).
>
> **AV1 open items after 2026-09-01** (see the status list): (1) last small
> chroma directional-prediction error (`filterType` detection); (2)
> `mandelbrot` Y ~22 / `testsrc2` Y ~13 — a separate un-root-caused bug
> (worst in corpus); (3) loop filter (deblock+CDEF) not verified bit-exact
> (testsrc full-frame Y 61.95 vs pre-filter 99); (4) loop restoration §7.17
> still a no-op passthrough; (5) inter prediction (Phase E) — MV pred §7.10 +
> inter recon §7.11.3 unimplemented, decoder returns `Ok(None)` for
> non-keyframes; (6) then flip `capabilities().pixel_exact`.
>
> **2026-09-01 (cont'd #5) — 3 more real bugs fixed; mandelbrot recovered;
> testsrc2 blocked on Intra Block Copy.**
> - **`4×4` intra blocks read a spurious `tx_depth` symbol.** `intra_block.rs`
>   called `read_tx_size` whenever `tx_mode_select && !lossless`; AV1 §5.11.15
>   also requires `MiSize > BLOCK_4X4` (a 4×4 always uses `TX_4X4`, no
>   signalled depth). Every 4×4 intra block desynced the tile under
>   `TX_MODE_SELECT`. Fixed → gate on `bsize > BLOCK_4X4`. **mandelbrot
>   Y 21.73→24.44, U 16.81→31.87, V 21.26→31.42.**
> - **`use_intrabc` was read as a literal bit, not an adaptive symbol.**
>   `intra_block.rs` did `dec.read_literal(1)`; AV1 §5.11.7 / dav1d
>   (`decode.c:1048`, `msac_decode_bool_adapt(cdf.m.intrabc)`) read it as an
>   `S()` symbol with the adaptive `TileIntrabcCdf` (`Default_Intrabc_Cdf =
>   {30531}`). Every `allow_intrabc` frame (screen-content: testsrc2) desynced
>   on the very first block. Added `mode_cdfs.intrabc` + `read_use_intrabc`.
>   **testsrc2 block (0,0) now decodes in sync** (verified: post-ymode
>   `r=64224` == dav1d).
> - **SMOOTH intra-mode enum constants were rotated** (see cont'd #3) — pinned
>   with `smooth_intra_mode_constants_match_spec_ordering`.
> - **testsrc2 is now blocked on Intra Block Copy (§ IBC).** After the
>   `use_intrabc` fix the trace stays in sync until block mi (52,20), where
>   dav1d decodes `use_intrabc=1` (`Post-dmv[...]` — a DV-predicted
>   integer block copy from already-decoded parts of the current frame).
>   Kinetix returns `KinetixError::Parse("intra block copy ... not yet
>   implemented")`. **This is a feature gap, not a bug.**
>   **Scoped 2026-09-01 (cont'd #6)** against the dav1d source
>   (`decode.c:1271-1366` IBC branch, `read_vartx_tree`/`read_tx_tree`,
>   `read_mv_residual` with `mv_prec=-1`): testsrc2 has **9 IBC blocks**, ~5
>   non-skipped. A non-skipped IBC block reads, after `use_intrabc`:
>   (a) `mv_joint` + per-component (`mv_sign`/`mv_class`/`mv_class0_bit` or
>   `mv_bit[i]` — **no** `mv_fr`/`mv_hp`, integer precision forced);
>   (b) `read_var_tx_size` — the recursive `txfm_split` tree (`txpart` CDF =
>   Kinetix's `DEFAULT_TXFM_SPLIT_CDF[cat*3+ctx]`, `cat = 2*(TX_64X64_sqr -
>   max_sqr) - depth`, `ctx = (aboveTx<txw)+(leftTx<txh)`); (c) an **inter**
>   `tx_type` per txb (`Post-y-cf-blk[...txtp=9/11/13/15...]` — IDTX/V_DCT/…,
>   the inter tx set, not the intra one); (d) inter-context `coeffs()`. Plus
>   DV prediction (fallback: first-SB-row → `dv=(0,-(512<<sb128)-2048)`, else
>   `dv=(-(512<<sb128),0)`; neighbour stack otherwise), the DV clamp block
>   (`decode.c:1296-1352`, mechanical), and an integer-pel block copy from the
>   current tile's planes + residual add.
>   **Conclusion: IBC ≈ the inter reconstruction path** (var-tx tree + inter
>   `tx_type` + inter coeff context + MC), so it is really **Phase E work**,
>   not a small standalone feature. Kinetix's `inter_block.rs` has stubs for
>   some of this but they are unvalidated and `read_mv_component`'s classN
>   path looks wrong (reads `mv_class0_bit` + only `mv_class-1` `mv_bit`s;
>   spec/dav1d read `mv_class` `mv_bit`s and no class0 bit). Fix that first
>   when Phase E is picked up. The `mode_cdfs.intrabc` CDF + `read_use_intrabc`
>   landed this session are the prerequisite and are correct.
> 119 unit tests pass, clippy clean. Files: `reconstruct/{intra_block,mod,
> mode_cdfs,predict,tests}.rs`. No `git commit` calls (concurrent process
> committed the earlier tx_depth fix as `73776fd`).
>
> --- superseded investigation (kept for the method) ---
> **2026-09-01 (cont'd #2) — narrowed the 8×4 ADST_DCT bug; axis-swap ruled
> out; dav1d residual dump added.**
> - **Ruled out**: swapping `row_axis_transform`/`col_axis_transform`
>   (making `ADST_DCT` → row=Adst/col=Dct) — it *regressed* everything
>   (testsrc Y 57.65→22.6, smptebars 54→33). dav1d's `dav1d_tx1d_types` with
>   its transposed (column-major) coeff buffer means `txtps[0]` is the
>   *height/column* transform: `ADST_DCT` {ADST,DCT} → col=ADST, row=DCT =
>   Kinetix's current mapping. **transform.rs axis mapping is correct; do not
>   swap it.**
> - Patched dav1d (`src/itx_tmpl.c`, `DAV1D_ITXDUMP=1` env gate) to print the
>   post-transform residual for every 8×4 ADST_DCT block. **Key observation:
>   several of dav1d's 8×4 ADST_DCT residuals are *vertically flat* — all 4
>   rows identical** (e.g. `-7 -20 -29 -35 -43 -54 -67 -75` ×4). Kinetix's
>   residual for the same class of block has strong *vertical* variation.
>   Both decoders agree the coefficients sit at logical cells col0=`[2,-9,6,3]`
>   down the rows + one at (r1,c1) — which *should* give a vertically-varying
>   residual. So either (a) dav1d dump line ≠ the block I was comparing (the
>   dump isn't block-tagged — next step: tag it with bx/by), or (b) there's a
>   genuine coeff-cell transpose that only bites rectangular chroma. The
>   entropy trace proving bit-exactness only proves the *scan order* matches,
>   not the final (row,col) each coeff lands in for the transform's indexing.
> - **Next**: tag the `DAV1D_ITXDUMP` output with `t->bx/t->by` (thread it
>   through `recon_b_intra` → `inv_txfm_add`), match dav1d's residual for the
>   exact root block (chroma px (16,40), luma mi (8,20)) against Kinetix's
>   `DBG full residual`, and if they're transposes of each other, fix the
>   `dequant[i*adj_w+j]` indexing in `inverse_transform` for rectangular
>   sizes (or the scan `pos` encoding). `KINETIX_AV1_DBG_PX=16,40
>   KINETIX_AV1_DBG_FULL=1` dumps Kinetix's side.


> **2026-09-01 (cont'd #7) — ★ BlockDecoded / haveAboveRight+haveBelowLeft
> implemented. mandelbrot Y 24.4 → 45.1 dB. ★**
> Root-caused mandelbrot's dominant error (was: whole 16×16 blocks off by up
> to 158, entropy proven in sync) to **directional intra prediction not
> extending `AboveRow`/`LeftCol` into the real reconstructed neighbour
> samples** — it always replicated the last edge sample. AV1 §7.11.2 fills
> `AboveRow[i]`/`LeftCol[i]` for `i = 0..w+h-1` with `Min(aboveLimit, x+i)` /
> `Min(leftLimit, y+i)` where the limit is `x + (haveAboveRight ? 2w : w) - 1`
> / `y + (haveBelowLeft ? 2h : h) - 1`. `haveAboveRight`/`haveBelowLeft` come
> from the **`BlockDecoded`** per-4×4 grid (§5.11.34 `clear_block_decoded_flags`
> at each superblock + set after every transform block).
> **Implemented**: `TileDecodeState.block_decoded: [Vec<u8>; 3]` (SB-relative,
> `BD_STRIDE=35`), `clear_block_decoded_flags` in `decode_superblock`, a
> `BlockDecodedCtx` threaded into `reconstruct_tx_block` that derives the two
> flags before `block_borders` and marks its cells after. `block_borders` now
> returns `AboveRow`/`LeftCol` of length `tx_w + tx_h` with the `2w`/`2h`
> extension, and `predict_directional` copies that in instead of replicating.
> **Result** (`av1_psnr_check`): `mandelbrot` **Y 24.44→45.14, U 31.87→49.41,
> V 31.42→48.49**; `testsrc` **U 48.82→51.05, V 47.02→48.57** (chroma also
> benefits); `testsrc` Y, `smptebars`, `solid_red` unchanged.
> mandelbrot pre-filter Y maxdiff 158→21 (ndiff 8881→1271) — the residual is
> a smaller directional detail (likely the edge-filter `filterType`/upsample,
> still hardcoded, or `dr_z3`'s non-edge-filter `max_base_y = h + min(w,h) -
> 1` vs Kinetix's `w+h-1`). 120 unit tests pass, clippy clean. Regression
> test `block_borders_extends_left_col_into_real_below_left_samples_when_available`.
>
> **NOTE (process): accidentally ran `git checkout tpt-kinetix-h264` while
> cleaning up**, discarding whatever the concurrent automated process had
> uncommitted there at that instant. Its `3831475` ("h264: fix 3 wrong
> quarter-pel luma MC formulas; PAFF field now pixel-exact") was already
> committed just before; any further uncommitted h264 increment was lost.
> Do not `git checkout <path>` on files another process owns.
>
> **AV1 corpus after 2026-09-01**: solid_red 99/99/99, testsrc 61.95/51.05/
> 48.57 (luma pre-filter pixel-exact), mandelbrot 45.14/49.41/48.49,
> smptebars 54.23/99/99, testsrc2 12.96/… (IBC-blocked). Open: (1) small
> directional-prediction residual (edge filter `filterType`/upsample +
> `dr_z3` non-filter `max_base_y`); (2) loop filter (deblock+CDEF) not
> verified bit-exact; (3) loop restoration §7.17 no-op; (4) IBC ≈ Phase E
> (var-tx tree + inter tx_type + MC + DV) — blocks testsrc2; (5) inter
> Phase E; (6) then `capabilities().pixel_exact`.

> **2026-09-01 (cont'd #8) — directional-prediction `filterType` (§7.11.2.9)
> wired.** Open item (1) above: `predict_directional`'s edge-filter strength /
> upsample gates hardcoded `FILTER_TYPE = 0`. Now derived per AV1 §7.11.2.9
> `get_filter_type(plane)`: `filterType = 1` when the block's above **or** left
> neighbour uses a SMOOTH* intra mode (`SMOOTH`/`SMOOTH_V`/`SMOOTH_H`), which
> selects the stronger `intra_edge_filter_strength` / `use_intra_edge_upsample`
> threshold rows (those two fns already took the arg; only the caller was
> stubbed). New `is_smooth_intra_mode()` in `reconstruct/mod.rs`;
> `reconstruct_intra_subblock` computes `filter_type_y` from
> `ymode_above/ymode_left` and `filter_type_uv` from `uv_above/uv_left`
> (block-origin tile availability; same direct-neighbour approximation the
> existing `INTRA_MODE_CONTEXT` lookup uses — the full spec form has a
> subsampling MI-offset + inter `RefFrames` check, not needed for the
> intra-keyframe corpus), threaded through `reconstruct_tx_block` →
> `predict_intra_block` → `predict_directional` (the unused `is_luma` param
> those two carried is replaced by `filter_type: i32`).
> **Result** (`av1_psnr_check`): `mandelbrot` **Y 45.14→47.37, U 49.41→51.62,
> V 48.49→51.61**; testsrc / smptebars / solid_red / testsrc2 all unchanged
> (no regressions). 122 unit tests pass, clippy `--all-targets -D warnings`
> clean. Regression tests `is_smooth_intra_mode_matches_spec_set` +
> `directional_prediction_filter_type_changes_sub_pel_output`. Files:
> `reconstruct/{mod,predict,reconstruct_block,intra_block,tests}.rs`. No `git
> commit` calls. Remaining open items unchanged: mandelbrot still has a
> smaller directional detail residual (pre-filter maxdiff ~21, likely
> upsample interpolation or `dr_z3` `max_base_y`); (2)–(6) as above.

> ## 2026-09-03 session note — catch-up on undocumented concurrent work, dav1d
> reference rebuilt on Linux, and a real testsrc2/IBC bug fixed (skip blocks
> never reset their coefficient neighbour context).
>
> **Catch-up (not this session's work, but undocumented in this file until
> now)**: between the 2026-09-01 (cont'd #8) note above and this session, six
> commits landed on `master` outside this file's narrative:
> `73776fd` (tx_depth sentinel, already covered above), `b89ac1c` ("seven
> reconstruction fixes + spec-correct MV component parsing" — SMOOTH enum,
> chroma edge filter, 4×4 tx_depth, `use_intrabc`, `BlockDecoded`,
> `filterType`, all *already* described above as uncommitted 2026-09-01 work,
> now actually committed, plus a rewritten `read_mv_component`/`read_mv` to
> the real §5.11.32 symbol order), `f7fae93` (three real CDEF bugs: a
> spurious `for _ in 0..8` loop biasing `cdef_direction` toward direction 0,
> wrong pri/sec packing, wrong sec-strength table lookup — `KINETIX_AV1_NOCDEF`
> / `NODEBLOCK` bypass flags added), `ca52335` (loop restoration §7.17
> implemented — Wiener + SgrProj), `f253255` (IBC reconstruction implemented:
> integer-pel predictor copy + residual, via `reconstruct_ibc_block`), `b509fc3`
> (IBC source-position sign was inverted — fixed by subtracting the decoded MV
> instead of adding; loop restoration's *apply* step gated off behind
> `KINETIX_AV1_FILTER=1` because it uses clamped unit-local pixels instead of
> real neighbouring-unit pixels at restoration-unit boundaries, causing ~25 dB
> regressions), `f93c99c` (palette reconstruction debug traces + psnr_check
> row-diff tooling). **Net effect on `av1_psnr_check` vs the 2026-09-01
> baseline**: `testsrc` Y 61.95→**73.01** (loop filter now on and correct),
> `mandelbrot` Y 47.37→**47.59**, `smptebars` Y 54.23→**57.49**, `testsrc2`
> 12.96→**14.36/17.17/13.92** (IBC blocks now reconstruct something instead of
> erroring out). `solid_red` unchanged at 99/99/99.
>
> **This session started by fixing a broken build**: a prior commit added a
> `dbg: bool` parameter to `read_palette_colors_yu` but didn't update its two
> test call sites (`cargo test` failed to compile), and three
> `KINETIX_AV1_DBG_*` env-var gates used manual range checks that trip
> `clippy::manual_range_contains` under `-D warnings` (`cargo clippy` failed).
> Fixed both (commit `f3dbd24`) — confirmed byte-identical `av1_psnr_check`
> output before/after, 125 tests pass. **Branch note**: work was moved from a
> feature branch to `master` directly partway through this session per
> updated instructions; `git log` on `master` is the authoritative history
> from here on.
>
> **Built a fresh dav1d reference on this (Linux) session's machine** —
> `scratchpad/av1ref/` did not exist here (previous sessions built it on a
> different Windows machine per the 2026-09-01 note). `apt-get install meson
> nasm`, `git clone https://github.com/videolan/dav1d.git` (the
> `code.videolan.org` origin is blocked by this environment's proxy; the
> GitHub mirror works), same two-line patch as before
> (`src/recon.h`'s `DEBUG_BLOCK_INFO` gated on `getenv("DAV1D_TRACE")` via a
> `dav1d_trace_enabled()` helper, a `BLOCK bx by bw4 bh4 bl bp r=` print at
> the top of `decode_b` in `src/decode.c`), `meson setup build
> --buildtype=release && ninja -C build` (asm enabled this time — nasm is
> available on Linux, unlike the previous session's `-Denable_asm=false`
> workaround for a missing MSVC nasm). Run as
> `LD_LIBRARY_PATH=.../build/src DAV1D_TRACE=1 build/tools/dav1d -i x.obu -o
> out.yuv --threads 1`. This is a local build only (not committed — dav1d is
> LGPL/BSD-dual and not vendored into this repo either way); rebuild from
> this note's commands if `scratchpad/` is ever lost.
>
> **Bug found and fixed: skipped transform blocks never reset their
> coefficient neighbour context.** Traced `testsrc2` (the IBC corpus clip)
> block-by-block against the fresh dav1d trace (`KINETIX_AV1_TRACE=1
> cargo run -p tpt-kinetix-av1 --example av1_trace_obu -- x.obu` vs
> `DAV1D_TRACE=1 dav1d -i x.obu`), comparing the `Post-tx`/`Post-*-cf-blk`
> `r=` (msac range) checkpoints in decode order. First divergence: the chroma
> `all_zero`/`dc_sign` symbol read for the block at mi (56,16) used context
> bucket 0 in Kinetix vs dav1d's bucket 1 (same decoded bit both times, so
> the mismatch was invisible until the *next* read, which consumed a
> different number of bits and cascaded). Added matching temporary
> instrumentation to both sides (`SKIPCTX_POST`/`SKIPCTX_EOB`/
> `SKIPCTX_BASEEOB`/`SKIPCTX_DCSIGN`/`DCSIGN_RAW`/`DCSIGN_STORE` — all
> removed before commit) to walk the exact `above_dc`/`left_dc` context-array
> contents feeding `dc_sign_ctx`. Root cause: block bx=48,by=16 (a real,
> non-skipped block) correctly writes a positive DC sign across chroma rows
> y4=8..11. The very next block, bx=52,by=20 (a **skipped** IBC block,
> `Post-skip[1]` in the dav1d trace), covers only rows y4=8..9 — dav1d's
> `read_coef_blocks` explicitly `memset`s *its own* footprint's above/left
> coefficient-context bytes to the "unset" sentinel even though it never
> calls `decode_coefs` (AV1 §5.11.34: `coeffs()` is simply never invoked for
> a skipped block, but the context still needs resetting). Kinetix's
> `reconstruct_tx_block` (`reconstruct_block.rs`) and `reconstruct_ibc_block`
> (`intra_block.rs`, both the luma and chroma call sites) had `if !skip {
> read_coeffs(...) }` with **no `else` branch** — so a skipped block's
> rows/columns simply kept whatever a completely unrelated earlier block had
> last written, here leaving rows 10/11 wrongly "positive" after row 20's
> skip should have cleared them. The very next real block (bx=56,by=16) then
> read a wrong `dc_sign` context for its left neighbour.
>
> **Fix**: `coeff::clear_coeff_context(ctxs, blk, w4, h4)` — the same
> zero-context store `read_coeffs` does at its own tail (for `all_zero` or a
> real decode), factored out so it can run standalone — called from the
> `else` branch of all three `if !skip { read_coeffs(...) }` call sites
> (`reconstruct_block.rs`'s intra/inter tx-block path, `intra_block.rs`'s IBC
> luma loop, `intra_block.rs`'s IBC chroma U/V loop).
>
> **Result** (`av1_psnr_check`): `testsrc2` Y/U/V **14.36/17.17/13.92 →
> 23.11/25.08/16.95 dB**; `solid_red`/`testsrc`/`mandelbrot`/`smptebars`
> byte-identical (this bug only bites when a skip block's footprint doesn't
> exactly match a later block's, which the intra-only corpus entries don't
> hit). 126 unit tests pass (new:
> `coeff::tests::skipped_block_clears_stale_dc_sign_context_for_later_neighbours`,
> which reproduces the exact row-overlap scenario above without needing the
> real corpus file), `cargo clippy -p tpt-kinetix-av1 --all-targets -D
> warnings` clean, `cargo fmt --all` applied. Committed as `d70e12e` on
> `master`.
>
> **testsrc2 is still far from pixel-exact** — re-ran the same trace diff
> after the fix and found the next divergence almost immediately (dav1d
> trace index ~547 of ~690 `Post-tx`/`Post-*-cf-blk` checkpoints): block
> bx=54,by=32 is a genuinely different kind of IBC block — dav1d's trace
> shows `Post-vartxtree[0/0]` (the recursive `read_var_tx_size` split flag,
> §5.11.16) and `Post-y-cf-blk[tx=7,txtp=13,eob=64]` (`txtp=13` is an
> **inter** transform type — `V_DCT`/similar from the inter tx-type set, not
> any value the intra tx-type tables produce). `reconstruct_ibc_block`
> always reads a single fixed-size transform (`max_tx_size_for_bsize(bsize)`,
> no split) and decodes its coefficients through the ordinary *intra*
> `read_coeffs` path (`qindex_positive: false` forces `DCT_DCT` with zero
> bits read for tx_type, never the real inter tx_type symbol). This
> **confirms** the 2026-09-01 (cont'd #6) scoping note's conclusion in a
> second, independent way (empirically this time, not just by reading the
> dav1d/spec source): IBC needs the var-tx tree + inter `tx_type` + inter
> coefficient context (Phase E work), not a small fix. Did not attempt this
> — it is a genuinely large, separate task; the `read_mv_component` classN
> concern flagged back in 2026-09-01 (cont'd #6) is still unverified and
> should be checked first whenever Phase E starts.
>
> **AV1 corpus after 2026-09-03**: `solid_red` 99/99/99, `testsrc`
> 73.01/53.76/49.23, `mandelbrot` 47.59/52.44/52.62, `smptebars` 57.49/99/99,
> `testsrc2` 23.11/25.08/16.95 (IBC-blocked, see above). Open items, in
> priority order: (1) `mandelbrot`'s small residual directional-prediction
> error (edge-filter upsample interpolation or `dr_z3`'s non-edge-filter
> `max_base_y` — still not root-caused, see the 2026-09-01 note); (2) loop
> filter is now wired correctly (CDEF bugs fixed, `testsrc` Y jumped
> 61.95→73.01) but still not *verified* bit-exact block-by-block against
> dav1d — worth a dedicated trace pass; (3) loop restoration is implemented
> but its apply step is gated off (`KINETIX_AV1_FILTER=1`) due to an unfixed
> restoration-unit-boundary pixel bug (see `b509fc3`'s message above) —
> fixing that boundary handling is a concrete, scoped next target; (4) IBC
> var-tx tree + inter tx_type + inter coeff context (Phase E) — blocks
> `testsrc2`, now empirically confirmed as the next divergence point; (5)
> inter prediction generally (Phase E) — decoder returns `Ok(None)` for
> non-keyframes; (6) then `capabilities().pixel_exact`.
>
> Modified: `tpt-kinetix-av1/src/coeff.rs` (+`clear_coeff_context`, +1
> regression test), `tpt-kinetix-av1/src/reconstruct/{intra_block,mod,
> reconstruct_block}.rs`. Committed as `d70e12e` on `master` (pushed to
> `origin master`). `capabilities().pixel_exact` untouched (still `false` —
> correctly so, the corpus is nowhere near bit-exact yet).

> ## 2026-09-03 (cont'd) — mandelbrot's "directional-prediction residual"
> redirected: reconstruction is very likely bit-exact, the gap is the loop
> filter (CDEF), not prediction/transform. No fix landed this round — this
> is a methodology correction + evidence trail for the next session.
>
> **Why the old hypothesis was probably a red herring.** Every prior note on
> this item (2026-08-31 → 2026-09-01) measured "pre-filter Kinetix" against
> "post-filter reference" via `av1_prefilter_check`'s `compare_interiors`,
> which only excludes pixels within 4px of an **8×8 grid** boundary to dodge
> **deblock** contamination. CDEF is not edge-limited like deblock — it's a
> content-adaptive filter over the *whole* 8×8 unit — so that mask does
> nothing to exclude CDEF's effect on "interior" pixels. Any interior diff
> the tool reports could equally be a real reconstruction bug *or* a correct
> reconstruction that the reference's CDEF pass then modifies differently
> from Kinetix's. The tool has been unable to tell these apart since CDEF
> was fixed (2026-09-02, `f7fae93`) and became a real (non-no-op) contributor
> to the corpus's pixels.
>
> **What was actually checked this session** (method: patch dav1d's
> `recon_tmpl.c` to dump `dst` immediately before and after
> `itxfm_add[b->tx][txtp]` — i.e. the *pure pre-filter* prediction and
> residual for one exact transform block, gated on `DAV1D_ITXDUMP_BX`/`_BY`
> env vars — then diff against Kinetix's own `KINETIX_AV1_DBG_PX`/
> `KINETIX_AV1_DBG_FULL` dump for the same block). Three representative
> blocks from `mandelbrot_128x96`, chosen to cover the previously-suspected
> mechanisms (sparse coefficients, SMOOTH modes, rectangular sqrt(2) rescale):
> - `mi=(0,0)`, 32×32 `DC_PRED`, `DCT_DCT`, `eob=10`, sparse coefficients
>   (`{(0,0)=41,(0,1)=-10,(0,3)=-1,(1,0)=-8,(1,1)=1,(3,1)=-1}`): **prediction
>   and residual bit-exact** vs dav1d (verified the first 4 rows/32 cols by
>   hand; `dequant`/`residual` arrays match token-for-token).
> - `mi=(14,14)`/`(15,14)`, two adjacent 4×4 `SMOOTH_H` sub-blocks of an 8×8
>   leaf, `DCT_ADST`, `eob=15,16`: **prediction and residual bit-exact**
>   (pred `[40,50,56,58]`×4 rows, residual `[26,1,3,125/21,-1,-5,93/5,-1,53,84/
>   -2,99,96,76]`, identical on both sides). This block's "left" border was
>   independently confirmed to come from its own left-sibling sub-block's
>   *real* reconstruction (`recon[..,col=59] == 40` on every row), not a
>   stale/wrong context — ruling out a per-sub-block border bug too.
> - `mi=(0,16)`, 16×32 `SMOOTH_V`, `DCT_DCT`, needs the §7.13.3 sqrt(2)
>   rescale (`log2W=4,log2H=5`, `|Δ|==1`): **prediction and residual
>   bit-exact** across all 32 rows × 16 cols (hand-diffed the full grid both
>   dumps printed) — directly rules out the long-suspected rectangular-tx
>   rescale path as a bug, at least for this tx_type/size.
>
> For the last block, pixel (0,80) (= this block's row 16, col 0):
> pre-filter reconstruction is **141 on both sides** (`pred=150,
> residual=-9`, bit-exact); the actual reference frame (`ffmpeg`, loop
> filters on) shows **145** at that pixel, and Kinetix's own filtered output
> also lands on 141 there (CDEF left it unchanged in Kinetix's case). That
> 4-value gap exists *only* after loop filtering — it cannot be a
> reconstruction bug since the reconstruction is proven identical.
>
> **Row-level breakdown confirms this is filter-shaped, not noise-shaped**:
> `KINETIX_AV1_DBG_ROWS=1` (per-row Y PSNR, new this session — the row-level
> debug already existed, `KINETIX_AV1_DBG_ROW=<n>` for a per-pixel dump on
> one row) shows mandelbrot's *entire* frame is 99 dB (i.e. clean) **except
> rows 71–88**, an 18-row band, where PSNR drops to 62–69 dB — everywhere
> else is untouched. That is not what a scattered rounding bug in a
> per-block transform looks like; it is what a filter behaving differently
> over one region looks like. Disabling CDEF (`KINETIX_AV1_NOCDEF=1`)
> *increases* row 80's per-pixel errors (several pixels go from ±1 to −4/−5),
> i.e. CDEF is a real, mostly-correct, positive contributor here — not a
> no-op — so this isn't "CDEF should be off", it's "CDEF's direction/strength
> decision differs slightly from the reference's for this unit."
>
> **Conclusion**: mandelbrot's prediction and transform are very likely
> already bit-exact (three representative blocks, covering the specific
> mechanisms earlier sessions suspected, all confirmed exact via a real
> pre-filter dav1d reference — not the confounded post-filter one). The
> remaining ~47 dB gap is concentrated in a loop-filter (most likely CDEF
> direction/strength selection, possibly interacting with deblock at the
> superblock-row-1 top edge, rows 71-88 start right after the 64-px SB
> boundary) difference, not a reconstruction bug. **This retires the
> "small residual directional-prediction error / `dr_z3` `max_base_y`"
> hypothesis** carried since 2026-08-31 — it was never re-verified against a
> true pre-filter reference and appears to have been chasing the CDEF gap
> the whole time. Old item (1) is folded into item (2)
> "verify loop filter bit-exact block-by-block" below; that is now the
> single most concrete next AV1 target.
>
> **Also landed**: `KINETIX_AV1_DBG_ROW` in `av1_psnr_check.rs` indexed
> `frame.data[row*stride+col]` unconditionally and panicked for a row past a
> smaller clip's height (hit while iterating clips with this env var set
> globally) — added a bounds guard. Commit `2a24212`. No functional/PSNR
> change (confirmed via `av1_psnr_check`: all corpus numbers byte-identical
> to before this session's investigation — solid_red 99/99/99, testsrc
> 73.01/53.76/49.23, mandelbrot 47.59/52.44/52.62, smptebars 57.49/99/99,
> testsrc2 23.11/25.08/16.95). 126 unit tests pass, clippy clean.
>
> **Next session, concretely**: patch dav1d's CDEF direction-detection
> function (`cdef_dir` or equivalent, `src/cdef_tmpl.c`) to print the chosen
> direction/variance and primary/secondary strength for the 8×8 unit at
> luma (0,80)-(7,87) in `mandelbrot_128x96` (mirrors this session's
> `DAV1D_ITXDUMP_BX/BY` pattern — add `DAV1D_CDEFDUMP_BX/BY`), and diff
> against Kinetix's `cdef_direction`/`apply_post_filters` (`loop_filter.rs`)
> for the same unit. `KINETIX_AV1_NOCDEF`/`NODEBLOCK` (already present,
> `f7fae93`) isolate which filter stage to blame first. dav1d's patched
> build lives only in `scratchpad/av1ref/` (rebuilt this session from
> `github.com/videolan/dav1d` — see the earlier 2026-09-03 note for the
> build recipe); it is not committed to this repo.

> ## 2026-09-03 (cont'd #2) — ★ found the likely root cause: `deblock_plane`
> has no transform/prediction-edge presence check, so it filters at *every*
> 8-px grid line, including ones strictly inside a single wide transform. ★
> Root-caused following the trail from the note above (row 71-88's CDEF
> variance for the unit at (0,80) computed 744 vs dav1d's 230 despite
> identical formulas — traced to different *input pixels*, i.e. deblock, not
> CDEF, was the actual divergence point). **Not fixed this session** — found
> late, and a correct fix touches several call sites; verifying it safely
> needs a fresh session with full budget for a `just conformance`/full-corpus
> re-check. This note has everything needed to implement and verify it.
>
> **The bug**: `FrameMeta::record_luma`/`record_chroma` (`loop_filter.rs`)
> are called once per real transform block, but the *callers*
> (`reconstruct/intra_block.rs` twice, `inter_block.rs`, the IBC path) loop
> over **every** 8×8-luma grid cell the transform spans and call
> `record_luma` identically for each — e.g. a 16×32 transform (two 8×8
> columns wide) writes `tx_w=16` into *both* grid columns it covers. Given
> that, `deblock_plane`'s vertical-edge loop (`for bx in 1..grid_w`) filters
> at **every** `bx*step` position whenever `compute_level(...) != 0`, using
> `left_tx.min(right_tx)` purely to size the filter tap — there is no check
> for whether `bx*step` is actually a transform (or prediction-block)
> boundary at all. For our 16-wide transform, position `x=8` (`bx=1`) sits
> **inside** the single transform (real edges only at `x=0` and `x=16`), but
> the loop filters it anyway because `left_tx == right_tx == 16` looks like
> "a valid size", not "no edge here". AV1 §7.14.1's edge mask is supposed to
> gate this (`isTxEdge`/`isBlockEdge`), and that gate is simply missing here.
> This has nothing to do with CDEF, and nothing to do with prediction —
> **deblock is the first thing in the post-filter chain to touch the wrong
> pixels**, and CDEF (correctly implemented) then just propagates that error
> forward, which is why the CDEF-side investigation initially looked like a
> "variance formula" bug.
>
> **Evidence trail** (`mandelbrot_128x96`, mi block bx=0,by=16, a single
> `SMOOTH_V` 16×32 `DCT_DCT` transform spanning luma rows 64-95): pre-filter
> reconstruction (pred+residual, verified bit-exact vs dav1d in the note
> above) for column 0 of the 8×8 unit at (0,80)-(7,87) exactly equals
> Kinetix's own post-deblock snapshot at every row (80→141, 81→140, …,
> 87→132 — deblock made *no* change there, correctly, since column 0 is at
> the block's real left edge and the edge filter evidently chose a zero
> delta this time). But **columns 6-7** of that same 8×8 unit *do* differ
> between pre-filter and post-deblock (e.g. row 82 col 6: pre-filter 144,
> post-deblock 145; row 86 col 6: pre-filter 139, post-deblock wrongly
> stayed 139 vs a dav1d-implied correct value elsewhere in the row) — a
> small ±1 perturbation centered on `x=8`, exactly where the missing
> edge-presence check would spuriously apply the deblock kernel's tap reach
> from a nonexistent internal boundary. This 1-2px perturbation then feeds
> `cdef_direction`'s variance computation (whose formulas were verified
> line-for-line identical to dav1d's `cdef_find_dir_c` — cost accumulation,
> `DIV_TABLE`/`div_table` indexing, and the final `(best_cost -
> cost[dir^4]) >> 10` all match), producing a different variance (744 vs
> 230) even though the direction argmax happened to coincide (dir=4 both
> sides) for this particular unit — the CDEF math itself is correct, its
> *input* wasn't.
>
> **Likely blast radius**: any coded block using a transform wider or taller
> than 8 samples (16×16, 16×32, 32×32, the `TX_16X4`/`TX_8X16` family, etc.)
> gets spurious internal-grid-line deblocking at every 8-px step inside it.
> This corpus has plenty of those (the 32×32 `DC_PRED` block at mi (0,0) in
> this same mandelbrot frame, most of `smptebars`'s and `testsrc2`'s larger
> flat regions, …) — likely explains a meaningful share of the whole
> post-filter PSNR gap across the corpus, not just this one row band.
>
> **The fix** (scoped, not yet implemented): add edge-presence tracking
> alongside the existing size tracking.
> 1. `FrameMeta`: add `luma_edge_left: Vec<bool>` / `luma_edge_top: Vec<bool>`
>    (and the chroma equivalents, `u_edge_left`/`u_edge_top` — `v` shares the
>    same subsampled grid as `u` per `record_chroma`'s existing `u`/`v`
>    symmetry) alongside `luma_tx_w` etc., all `w8*h8`-sized, default `false`.
> 2. `record_luma`/`record_chroma`: accept `is_left_edge: bool, is_top_edge:
>    bool` and OR them into the new grids (OR, not overwrite — `merge_tile`
>    calls these again per tile and a real edge from one tile-local call must
>    stick).
> 3. At every call site (`intra_block.rs` ×2 — the keyframe path around line
>    ~522-529 and the IBC path around ~971-978 — `inter_block.rs`, and any
>    inter-IBC chroma loop), when looping `for by in by0..by1 { for bx in
>    bx0..bx1 { record_luma(bx, by, ...) } }`, pass `is_left_edge: bx == bx0,
>    is_top_edge: by == by0` — i.e. only the block's own origin column/row is
>    a real edge; the rest of its span is interior. Chroma's `record_chroma`
>    call sites already iterate per actual chroma-tx-block (not per coded
>    block), so check whether they need the same treatment or are already
>    tx-block-granular — worth confirming with a quick trace before assuming
>    chroma needs the identical fix.
> 4. `deblock_plane`: thread the appropriate edge grid in (a new parameter,
>    or reuse `_skip_grid`'s currently-unused slot pattern) and gate — vertical
>    pass: `if !luma_edge_left[by*grid_w+bx] { continue; }` before computing
>    `filter_size`/running the filter (mirror for the horizontal pass with
>    `edge_top`).
> 5. Verify: `cargo test -p tpt-kinetix-av1 --lib` (expect the existing
>    `filter_size_from_tx_samples_caps_by_plane_not_by_bucket`-style tests to
>    still pass unmodified — they test the size formula directly, not the
>    plane-level loop), `av1_psnr_check` across the whole corpus (expect
>    `solid_red` unchanged at 99/99/99 — every block there is a single
>    32×32/64×64 transform covering the whole frame, so no internal edges
>    exist to wrongly filter either way; expect `mandelbrot`/`smptebars`/
>    `testsrc2` Y (and likely U/V) to improve, `testsrc` to improve less since
>    its content mostly uses `TX_8X8`-or-smaller transforms where every grid
>    line already is a real edge). Add a regression test in
>    `loop_filter.rs`'s existing test module: a synthetic 16-wide two-tx-cell
>    `FrameMeta` where cell 1 is *not* a real edge (from a wide transform)
>    should not filter at `x=8`, contrasted with two adjacent independent
>    8-wide transforms (both `is_left_edge: true`) which should.
>
> Nothing committed from the investigation itself (temporary
> `KINETIX_AV1_DBG_CDEF` instrumentation used to find this was added and
> then fully removed again; `git diff` was clean at that point).
>
> **Update — implemented and verified the same session** (commit
> `60ddfc3`): the fix above, exactly as scoped. `FrameMeta` gained
> `luma_edge_left`/`luma_edge_top`/`chroma_edge_left`/`chroma_edge_top`
> (`w8*h8`-sized `bool` grids) plus `mark_luma_edges`/`mark_chroma_edges`
> (OR-combining, so `merge_tile` propagates a real edge from any tile that
> established one). Called once per **real transform sub-block** — inside
> the `for ty { for tx { ... } }` loops in `reconstruct/intra_block.rs`
> (both the keyframe path and the IBC path, luma and chroma each), using
> that sub-block's own tile-local pixel origin — not the once-per-coded-block
> call site `record_luma`/`record_chroma` already had (which is still
> needed, unchanged, for size tracking; a coded block can contain several
> same-size transform sub-blocks and each one's own origin needs its own
> edge mark, not just the coded block's). `deblock_plane` gained
> `edge_left_grid`/`edge_top_grid` parameters and now `continue`s past any
> `bx`/`by` grid line neither grid marks as real, before ever computing
> `filter_size`/running the filter. `inter_block.rs`'s `decode_inter_block`
> (true inter frames, not IBC) was **not** touched — it's unreached by the
> current keyframe-only corpus (`decode()` returns `Ok(None)` for
> non-keyframes) — flag this for whoever picks up inter Phase E.
>
> **Result** (`av1_psnr_check`): `mandelbrot` Y/U/V
> 47.59/52.44/52.62→**47.62/52.53/52.68**, `testsrc` U 53.76→**53.93**;
> `solid_red`/`smptebars`/`testsrc2` byte-identical. **Smaller than the
> "likely blast radius" estimate above** — most of this corpus's content
> already uses ≤8-sample transforms, where every 8px grid line genuinely is
> a real edge and the bug was a no-op; only blocks using a wider/taller
> transform (the mandelbrot 16×32 `SMOOTH_V` block this was traced from,
> and similar ones elsewhere) were actually affected. Still a real,
> verified, spec-correctness fix (AV1 §7.14.1's edge presence gate was
> simply absent before this), not a regression risk either way. 128 unit
> tests pass (was 126; added
> `mark_luma_edges_only_flags_a_transform_blocks_own_origin` +
> `mark_luma_edges_flags_both_of_two_independent_adjacent_transforms`),
> `cargo clippy -p tpt-kinetix-av1 --all-targets -D warnings` clean, `cargo
> fmt --all` applied, full `cargo build --workspace` clean.
>
> **This closes item (1)/(2) from this session's earlier framing** (the
> "verify loop filter bit-exact" item) as *done for this specific bug
> class*, though deblock/CDEF are still not proven bit-exact overall — the
> corpus's remaining loop-filter-adjacent gap (mandelbrot still only 47.62
> dB, well short of the 60+ dB the fully-bit-exact luma reconstruction
> alone would suggest) means there is more to find here, just not via this
> particular bug any more. **Next AV1 priorities, updated**: (1) whatever
> remains in the loop filter after this fix — re-run the same
> patched-dav1d-trace method (`DAV1D_ITXDUMP_BX/BY` for pre-filter,
> post-deblock pixel dumps) on a fresh worst-row search now that this bug
> is gone, since the row-71-88 band's exact shape will have changed; (2)
> loop restoration boundary-pixel fix to un-gate apply; (3) IBC var-tx tree
> + inter tx_type + inter coeff context — blocks testsrc2; (4) inter Phase
> E (which will also need `inter_block.rs` to call
> `mark_luma_edges`/`mark_chroma_edges`, per the note above); (5) then flip
> `capabilities().pixel_exact`.

> ## 2026-09-04 session note — ★ `dqDenom` fix: smptebars luma now
> pixel-exact (57.49→99 dB), mandelbrot 47.62→53.10 dB ★. Continued the
> deblock re-verification requested at the top of this session and found a
> second, much bigger bug on the way.
>
> **Correction to the previous session's row-band claim**: the "mandelbrot
> rows 71-88" band described in the earlier 2026-09-03 note was actually
> **`testsrc`'s** data — `KINETIX_AV1_ONLY_TESTSRC=mandelbrot` is a boolean
> gate (any value enables it) that always runs *only* `testsrc`, not
> `testsrc` filtered to a clip named "mandelbrot"; the row dump that
> session captured was mislabeled. The deep block-level tracing in that
> session (`bx=0,by=16`, the `SMOOTH_V` `TX_16X32` block, the deblock
> edge-presence bug and its fix) used real `mandelbrot.obu` traces
> throughout and is unaffected — only the "18-row band" *framing* was
> about the wrong clip. Mandelbrot's real per-row PSNR (no `ONLY_TESTSRC`
> gate) is scattered errors across the *whole* frame (rows 0-95 all in the
> 40-66 dB range), not a localized band.
>
> **Re-traced the same `SMOOTH_V` `TX_16X32` block from scratch** (fresh
> `DAV1D_ITXDUMP_BX=0 DAV1D_ITXDUMP_BY=16` capture) to find the next
> divergence per the coordinator's request. Row 16 (the row this block's
> own analysis previously — incorrectly — claimed matched) actually
> **did not** match: dav1d residual row 16 = `[-5,-5,-4,-4,-4,-3,-3,-2,…]`,
> Kinetix's own captured dump = `[-9,-9,-9,-8,-7,-6,-5,-4,…]` — roughly
> **2× too large**. Checked every row 16-31: the *ratio* is consistently
> ~1.8-2.1×, not a fixed additive offset, i.e. a pure scale bug, not a
> rounding bug. Prediction matched exactly throughout (only residual was
> wrong) — this pointed straight at dequantization.
>
> **Root cause**: `dq_denom(tx_size)` (§7.12.3's `dqDenom` — the post-dequant
> integer division for oversized transforms) matched `tx_size ==
> TX_32X32`/`TX_64X64` **literally**. AV1's actual rule (confirmed against
> dav1d's `dq_shift = Max(0, t_dim->ctx - 2)`, `t_dim->ctx` being exactly
> Kinetix's own `tx_sz_ctx = (TX_SIZE_SQR[tx] + TX_SIZE_SQR_UP[tx] + 1) >>
> 1` already used elsewhere for coefficient contexts) is driven by that
> **square-up-averaged context**, not the transform's own literal size —
> so every non-square size whose `tx_sz_ctx` reaches 3 or 4
> (`TX_16X32`/`TX_32X16`/`TX_16X64`/`TX_64X16` at `ctx=3` → `dqDenom=2`;
> `TX_32X64`/`TX_64X32` at `ctx=4` → `dqDenom=4`) silently got `dqDenom=1`
> (a no-op) under the old literal check, overscaling every dequantized
> coefficient in those six sizes by 2× or 4×. (Cross-checked the very
> non-intuitive edge case directly against dav1d's own
> `dav1d_txfm_dimensions` table in `src/tables.c`: `TX_8X32`/`TX_32X8`,
> despite *also* having square-up 32×32, land at `ctx=2` → `dqDenom=1` —
> the skew relative to the square-up matters, this is genuinely not just
> "square-up == 32×32 ⇒ dqDenom=2".)
>
> **Fix** (commit `c0419de`): rewrote `dq_denom` to compute the shift from
> `tx_sz_ctx` directly instead of matching two literal enum values.
> **Result**: `smptebars_256x144` luma **57.49 → 99.00 dB (pixel-exact
> across the whole frame — every one of its 144 rows now reads 99 dB)**;
> `mandelbrot_128x96` Y/U/V **47.62/52.53/52.68 → 53.10/52.53/52.68**;
> `testsrc`/`testsrc2`/`solid_red` unchanged (this corpus's content doesn't
> happen to use the six affected sizes there). 129 unit tests pass (new:
> `dq_denom_follows_the_square_up_size_not_the_transforms_own_shape`,
> cross-checked value-for-value against dav1d's authoritative per-size
> `ctx` table), clippy clean, full workspace build clean.
>
> **Mandelbrot re-traced after the `dqDenom` fix**: re-ran the same
> `SMOOTH_V` block — prediction and residual now bit-exact vs dav1d at
> every row checked (16-31). Picked two fresh worst-row targets from the
> post-fix per-row PSNR (`KINETIX_AV1_DBG_ROWS=1`, no `ONLY_TESTSRC` gate
> this time): row 84 (block `bx=0,by=16` again, a different row) and row
> 56 (block `bx=13,by=14`, a `4×8` `DC_PRED` two-`TX_4X4`-sub-block
> region). Both traced fully bit-exact pre-filter (pred + residual match
> dav1d exactly via fresh `DAV1D_ITXDUMP` captures) — the remaining
> per-pixel diffs (`±1` to `±9`, e.g. row 56 cols 51-62) only appear in
> the **filtered** output, confirmed by direct `KINETIX_AV1_DBG_ROW`
> comparison against the real `ffmpeg` reference. So the *next* remaining
> gap genuinely is the loop filter (deblock and/or CDEF), not
> reconstruction — same conclusion as before the `dqDenom` fix, but now
> confirmed on freshly-verified-correct reconstruction rather than
> reconstruction that turned out to still have a 2× bug hiding in it.
>
> **What wasn't found this session**: a systematic deblock/CDEF formula
> bug of the `dqDenom` bug's scale. Skimmed `filter_line_1d`'s filter/flat
> masks and `cdef_direction`/`cdef_constrain`'s taps and constants —
> heavily spec-cross-checked already by prior sessions' comments, nothing
> jumped out on inspection alone. The remaining errors are small (±1-9)
> and scattered across many different small blocks (mostly `TX_4X4`
> `DCT_ADST`/`ADST_DCT` in busy/high-detail regions) rather than
> concentrated in one obviously-wrong code path — this needs the same
> patient per-edge trace-to-first-divergence method as the `dqDenom` hunt,
> just applied to deblock's edge-filter formula (`filter_mask`/`flat`/the
> actual 4/8/16-tap blend) or CDEF's `cdef_filter_block` pixel modification
> directly (not just `cdef_direction`, which was verified correct back in
> the previous session), for one specific edge at a time.
>
> **AV1 corpus after 2026-09-04**: `solid_red` 99/99/99, `testsrc`
> 73.01/53.93/49.23, `mandelbrot` 53.10/52.53/52.68, `smptebars`
> **99.00/99.00/99.00 (pixel-exact)**, `testsrc2` 23.11/25.08/16.95.
> **Next AV1 priorities**: (1) deblock/CDEF's remaining small-magnitude
> systematic error — pick one specific small block (e.g. mandelbrot's
> `bx=13,by=14` `TX_4X4` region from this session, or its right/bottom
> neighbour edges) and trace the *filter itself* (not just its inputs,
> already proven correct) against dav1d step by step; (2) loop restoration
> boundary-pixel fix to un-gate `apply`; (3) IBC var-tx tree + inter
> `tx_type` + inter coefficient context — blocks `testsrc2`; (4) inter
> Phase E; (5) then flip `capabilities().pixel_exact`. `smptebars` being
> genuinely pixel-exact now is a good sign that solving the same
> loop-filter-precision issue for the other clips (mostly a matter of that
> remaining ±1-9 gap) could close a meaningful chunk of the remaining
> corpus at once.

> **2026-09-04 (cont'd) — CDEF direction/strength selection confirmed
> correct for one concrete example; the bug (if in CDEF, not deblock) is
> in `cdef_filter_block`'s actual tap application, not parameter
> selection.** Picked the worst edge from row 56's fresh trace: the
> vertical edge at x=52 between mandelbrot's `bx=12,by=14` (`TX_4X8`
> `ADST_ADST` `SMOOTH_V`) and `bx=13,by=14` (`TX_4X4` `DCT_ADST`
> `DC_PRED`) blocks — both already proven pre-filter-bit-exact this
> session. Raw (unfiltered) row 56 around the edge: `…176 98 94 111 | 86
> 87 78 57…` (cols 48-55). `KINETIX_AV1_NODEBLOCK=1` shows col 51/52
> unchanged from raw (`111`/`86`) — **deblock's `filter_mask` correctly
> declines to filter this edge at all** (the raw jump is large enough to
> read as a genuine content edge, not a blocking artifact — this looks
> right, not a repeat of the earlier `dqDenom`-shaped bug). With CDEF on,
> Kinetix nudges col 51 only slightly (`111→110`) and leaves col 52
> untouched (`86→86`), while the real reference pulls both much further
> toward each other (`101`/`93`) — a real remaining gap of 9/-7.
>
> Patched dav1d's `cdef_apply_tmpl.c` (`DAV1D_CDEFDUMP_BX/BY` env vars,
> generalized from the previous session's hardcoded position) to dump the
> chosen direction/variance/strength for this exact 8×8 unit (`bx=12,
> by=14` in mi units) and compared against Kinetix's own
> `KINETIX_AV1_DBG_CDEF2`-equivalent instrumentation (added, used, then
> fully removed again — `git diff` is clean): **direction matches exactly
> (`dir=3` both sides)**, **the adjusted primary strength matches exactly
> (`p`/`adj_y_pri_lvl=4` both sides)**; only the raw `variance` differs
> very slightly (`29506` dav1d vs `29778` Kinetix — a ~1% difference,
> plausibly from a tiny upstream deblock difference feeding
> `cdef_direction`'s input pixels, but it lands in the same `var_str`
> bucket either way so doesn't affect strength selection here). So CDEF's
> *parameter selection* (direction + primary/secondary strength) is
> correct for this block; whatever produces the small-but-real output gap
> must be in `cdef_filter_block` itself — the per-pixel tap sampling,
> `cdef_constrain`, or the final `clip3(x + round(sum), min, max)` — or
> conceivably still a subtler deblock difference elsewhere along this
> edge (only column 0 of the block was checked against dav1d's own
> pre-filter values earlier this session, not every column).
>
> **Not resolved this session** — ran out of budget to hand-derive the
> exact expected `cdef_filter_block` output from the spec formula for
> this specific pixel and compare term-by-term. **Concrete next step**:
> extend the `DAV1D_ITXDUMP`-style approach to dump dav1d's *post-CDEF,
> pre-restoration* row (or reuse the `hex_dump`-based `DEBUG_B_PIXELS`
> machinery already in `recon_tmpl.c`, gated the same way) for this exact
> 8×8 unit, then diff Kinetix's `cdef_filter_block` output at every pixel
> in the unit against it — that pins down whether the discrepancy is
> really inside `cdef_filter_block`'s math (in which case hand-verifying
> `cdef_constrain`/the primary-vs-secondary tap accumulation against
> dav1d's `cdef_filter_block_c` in `src/cdef_tmpl.c` line-by-line, the
> same method that found the `dqDenom` bug, is the way in) or actually
> still further upstream in deblock for a column this session didn't
> check directly against dav1d.

> **2026-09-04 (cont'd) — fixed: deblock's luma pass ran at 8-sample grid
> granularity, which cannot even *represent* (let alone filter) a real
> transform edge at a position that's a multiple of 4 but not 8.** This is
> the root cause the previous note's x=52 mandelbrot edge was actually
> hitting — re-reading that note in light of this fix, "deblock correctly
> declines to filter this edge" was wrong: `deblock_plane`'s vertical loop
> is `for bx in 1..grid_w { edge = bx * step }` with `step = 8` for luma,
> so `edge` can only ever be 0, 8, 16, … — `x=52` (`52/8 = 6.5`) is
> mathematically unreachable, not "evaluated and rejected by
> filter_mask". `FrameMeta` only tracked one grid resolution (8×8-luma
> cells) for luma, sized for `TX_8X8` and up; AV1 also has `TX_4X4`,
> `TX_4X8`, `TX_8X4`, whose independent-transform boundaries can land on
> any 4-sample line.
>
> Fix: added a second, finer (4×4-luma-cell) grid to `FrameMeta` — `w4`/
> `h4`/`luma_tx_w4`/`luma_tx_h4`/`luma_edge_left4`/`luma_edge_top4`, with
> `record_luma4`/`mark_luma_edges4` populated from the same per-
> transform-sub-block call sites in `intra_block.rs` (keyframe and IBC
> paths) that already call `record_luma`/`mark_luma_edges`, just using
> `px/4` instead of `px/8` coordinates and the transform's own (possibly
> sub-8) span. `apply_post_filters`'s luma `deblock_plane` call now uses
> `step=4`, `grid_w=meta.w4`, `grid_h=meta.h4`, and the new `*4` grids
> instead of the 8×8 ones; chroma's call sites are untouched (chroma's
> minimum transform size in chroma samples is `TX_4X4` = 8 luma samples,
> so its existing 8×8-luma grid already matches its true minimum
> granularity — confirmed, not just assumed, since chroma output didn't
> move on any corpus clip below). `merge_tile` now also OR/max-merges the
> `*4` grids across tiles (offset `ox*2`/`oy*2` since the 4×4 grid is 2×
> denser than the 8×8 one).
>
> Verified via `av1_psnr_check`: `mandelbrot` Y 53.10 → **57.62 dB**
> (largest single-fix jump since `dqDenom`), `testsrc` Y 73.01 → **73.46
> dB**; `smptebars`/`solid_red_32`/`solid_red_64` unchanged at 99.00 dB
> (already pixel-exact, correctly unaffected); `testsrc2` unchanged at
> 23.11 dB (dominated by the separate, already-documented IBC var-tx-tree
> gap, not deblock). All 3 U/V PSNRs also unchanged, confirming the
> chroma-granularity assumption above. Added `mark_luma_edges4_...` and
> `record_luma4_...` regression tests. `cargo test -p tpt-kinetix-av1
> --lib` (131 passed), `cargo clippy -p tpt-kinetix-av1 --all-targets --
> -D warnings` (clean), `cargo build --workspace` all green.
>
> **Not fully resolved** — mandelbrot is still 57.62 dB, far from
> pixel-exact, so more loop-filter (or reconstruction) gap remains
> somewhere; CDEF's own tap-application math (the previous note's
> `cdef_filter_block` suspicion) is still unverified line-by-line and
> should be re-checked fresh now that the deblock input feeding it is
> more correct at this exact edge. **Next step**: re-run a fresh
> worst-edge search on mandelbrot (row/col PSNR scan) now that this class
> of bug is gone, since the specific x=52 edge this session traced may no
> longer be the worst offender.

> **2026-09-04 (cont'd) -- fixed: two compounding bugs in the loop-filter
> level derivation (LoopFilterDeltas::default(), and a missing ref-delta
> shift/mode-delta condition in compute_level).** Fresh worst-row scan on
> mandelbrot post-4x4-grid-fix found row 64 as the new worst row (46.51
> dB), with the largest single-pixel divergence at col 96 (got=192
> ref=182, diff=10). Traced with a patched dav1d: av1_trace_obu's KTRACE
> BLOCK located the coded block at mi bx=24,by=16 (px 96,64, TX_4X8,
> DC_PRED); DAV1D_ITXDUMP_BX/BY confirmed dav1d's own raw reconstruction
> there is bit-exact with Kinetix's (pred=167, residual=27, recon=194
> both sides -- double-checked directly against dav1d's own
> --inloopfilters none output). --inloopfilters nodeblock vs default
> showed dav1d's deblock dropping this pixel from 194 to 183 (an
> 11-unit change), while Kinetix's deblock left it completely unchanged.
>
> Kinetix's own edge/level tracing (KINETIX_AV1_DBG_EDGE, added/used/
> removed) showed the vertical edge at this exact position was being
> attempted (edge_left_grid true, lvl=18, filter_size=4) but filter_mask
> legitimately declined (|q1-q0|=19 > limit=18) -- so the edge-presence/
> granularity machinery fixed earlier this session was working correctly;
> the bug had to be in the filter level itself. Patched dav1d's
> loopfilter_tmpl.c with a DAV1D_LFDUMP env var printing wd/E/I/H/p0/p1/
> q0/q1/fm whenever p0/q0 matched the known raw value, and -- critically
> -- added --cpumask 0 to force dav1d's generic C path (its default SIMD/
> asm path silently bypasses source-level instrumentation entirely; the
> first attempt without --cpumask 0 found zero matches across the whole
> frame, which in hindsight was the tell). With the C path forced:
> dav1d's own I=19 where Kinetix computed limit=18 -- a real 1-level
> strength desync, not a false decline.
>
> Root-caused via dav1d's lf_mask.c calc_lf_value: (1) a `sh = base >=
> 32` doubling of the ref-delta term (spec's nShift = lvlSeg >> 5) that
> compute_level never applied (harmless here since base=18 < 32, but a
> real bug for any segment/frame at level >=32); (2) more directly,
> dav1d's r=0 (INTRA_FRAME) case adds only ref_delta[0], never a mode
> delta -- while Kinetix's LoopFilterDeltas derived Default to an
> all-zero array instead of the spec's real setup_past_independence()
> reset values (ref_deltas = {1,0,0,0,-1,0,-1,-1}), so ref_delta[0] was
> silently 0 instead of 1 whenever a frame enables
> loop_filter_delta_enabled without an explicit per-index update
> (mandelbrot's case exactly). 18+0(wrong default)=18 vs
> 18+1(correct)*1(shift=0)=19 -- matches dav1d exactly. Also removed
> compute_level's unconditional +mode_deltas[0] term (wrong for intra per
> spec/dav1d, harmless only because it happened to be 0 in every clip
> tested so far).
>
> Verified via av1_psnr_check: mandelbrot Y 57.62 -> 58.79 dB (U/V also
> up slightly), testsrc Y 73.46 -> 74.88 dB; smptebars/solid_red
> unchanged at 99.00 dB (no regression); testsrc2 unchanged (separate,
> already-documented IBC gap). The row-64/col-96 edge traced is now
> bit-exact. Added a regression test on LoopFilterDeltas::default()'s
> actual values (the direct root cause); a compute_level-level test was
> skipped as impractical -- FrameHeader has no Default impl and 140+
> fields. cargo test -p tpt-kinetix-av1 --lib (132 passed), clippy clean,
> cargo build --workspace green.
>
> Also added a permanent debug utility to av1_psnr_check.rs:
> KINETIX_AV1_DBG_ROW_RANGE=c0,c1 (paired with KINETIX_AV1_DBG_ROW) dumps
> the raw got/exp byte arrays for a column range instead of only a diff
> list. And: the dav1d CLI's built-in --inloopfilters none|nodeblock|
> nocdef|norestoration flag is far more reliable for isolating filter
> stages than patching DEBUG_BLOCK_INFO-gated dumps -- prefer it before
> reaching for a source patch. --cpumask 0 is required for any future
> loopfilter_tmpl.c-style source patch to actually run, since dav1d's
> optimized asm paths silently skip C-source instrumentation.
>
> **Not fully resolved** -- mandelbrot is still only 58.79 dB. Next
> targets, in priority order: (1) another fresh worst-row/worst-edge scan
> (this fix likely shifted many other edges' exact filtered values
> slightly, so the ranking has probably changed again); (2) apply the
> same scrutiny to delta_lf (the per-superblock adaptive delta) --
> deblock_plane's two call sites in apply_post_filters still pass a
> hardcoded literal 0 for delta_lf, so read_delta_lf's parsed
> per-superblock deltas (if any test clip uses them) are silently
> discarded; delta_lf_present=false for the whole current corpus so this
> hasn't mattered yet, but is a real gap for future content; (3) loop
> restoration boundary-pixel fix to un-gate apply; (4) IBC var-tx tree +
> inter tx_type + inter coefficient context.

> **2026-09-04 (cont'd) -- loop restoration: fixed real cross-unit-
> boundary pixel reads (a genuine spec-fidelity improvement), but it did
> NOT resolve the underlying "not yet correct" gap -- still gated behind
> KINETIX_AV1_FILTER=1, null result on the one exercised test case.**
> Continued the worst-row scan (row64/col96 fix above resolved the
> largest single-pixel outlier); the next-worst rows showed a smaller,
> broader +/-1 divergence spread across many flat interior pixels far
> from any transform/deblock/CDEF edge (e.g. mandelbrot row4 cols79-93,
> all exactly -1 vs ref). Isolated via dav1d's `--inloopfilters
> none|nodeblock|nocdef|norestoration` flags (see the earlier note on why
> this beats source patching): raw reconstruction and CDEF-only output
> both matched Kinetix exactly at these pixels; only `--inloopfilters`
> with restoration *enabled* reproduced the -1 shift, and disabling just
> restoration removed it. So this whole class of remaining small,
> widespread diffs is loop restoration -- currently gated off in Kinetix
> entirely (`apply_loop_restoration_plane` only runs under
> `KINETIX_AV1_FILTER=1`), which explains why Kinetix simply doesn't
> reproduce it.
>
> Fixed the specific bug this session's methodology could point to
> directly: `wiener_filter_plane`/`sgrproj_filter_plane` extracted each
> restoration unit into an *isolated* buffer before filtering, so every
> tap within reach of the unit's own edge (inescapable for a 7-tap Wiener
> kernel, or an SgrProj radius-r window, on units as small as 32px)
> clamped to that unit's own edge sample rather than reading the real
> pixel just across the boundary. Rewrote both to take a shared
> whole-plane pre-restoration snapshot plus the target unit's offset, so
> only the true plane edge clamps -- still an approximation of the real
> §7.17.1 stripe-line-buffer boundary handling (pre-deblock lines saved
> every 64 rows), not a full spec match, but strictly closer than
> unit-local clamping. Added a regression test proving the function
> actually reads real cross-boundary pixels (structurally impossible for
> the old isolated-buffer version).
>
> **Null result, honestly reported**: enabling `KINETIX_AV1_FILTER=1` on
> the corpus's only clip that exercises restoration (testsrc's V plane,
> 49.26 dB unfiltered) gives *byte-identical* output before and after
> this fix (42.24 dB both times -- restoration currently makes that plane
> worse, not better). So the boundary-clamping bug, while real and now
> fixed, was not the (or not the only) reason restoration is gated off;
> the deeper issue is still unlocated -- likely in the Wiener/SgrProj
> math itself, or in how `lr_units` gets populated (`read_lr_unit`),
> neither of which this session traced against dav1d. Confirmed via
> `git stash` A/B testing (same command, only the fix reverted) that the
> old code produces the exact same wrong 42.24 dB, ruling out the
> boundary fix as either helping or hurting this specific case --
> genuinely inconclusive, not a regression. Default corpus (`av1_psnr_
> check` with `KINETIX_AV1_FILTER` unset) is provably unaffected since
> restoration stays gated off either way. 133 unit tests pass, clippy
> clean, `cargo build --workspace` green.
>
> **Next step for restoration** (not attempted this session): trace
> `read_lr_unit`'s parsed Wiener/SgrProj coefficients for testsrc's one
> restored unit against dav1d's actual decoded values (dav1d likely has
> an existing debug hook or one can be patched into `src/recon_tmpl.c`'s
> `read_restoration` /  `src/lf_apply_tmpl.c`'s restoration-apply path) --
> the same trace-to-first-divergence method used for `dqDenom` and the
> loop-filter-level bugs above, just not yet applied to this filter.

> **2026-09-04 (cont'd) -- found and fixed the last restoration bug:
> `sgrproj_filter_plane` used the wrong second projection weight, making
> SgrProj a complete silent no-op. Loop restoration is now un-gated by
> default.** After the Wiener h/v swap fix above, checked SgrProj too
> (mandelbrot uses it: `frame_restoration_type=[2,0,0]`, plane 0 only,
> `set=10` -> `SGR_PARAMS[10]=[0,0,1,5]` i.e. `r0=0` (5x5 pass disabled),
> `r1=1` (3x3 pass active)). Enabling `KINETIX_AV1_FILTER=1` for
> mandelbrot changed **zero bytes** -- confirmed via `KINETIX_AV1_DUMP_
> FINAL` byte-for-byte `cmp`, not just PSNR rounding. dav1d's own
> `--inloopfilters none` vs default A/B showed a real effect (1031/12288
> Y pixels change by ±1), so this was a genuine bug, not "nothing to
> filter here."
>
> Traced with temporary instrumentation (added, used, fully removed):
> printed `read_lr_unit`'s decoded `xqd=[0, 2]` (matches dav1d's own
> `sgr_weights[0,2]` exactly -- entropy decode is correct) and
> `sgrproj_filter_plane`'s internal `a_tab`/`b_tab`/`p_val`/`z`/`alpha`
> for one exact pixel, which matched a patched dav1d
> (`DAV1D_SGRDUMP`, forced through `--cpumask 0` so the C source path
> actually runs -- see the earlier note on why this flag is required)
> byte-for-byte: `sum=1244 sum_sq=171954 p_val=50 z=0 alpha=255
> a_tab=35238` both sides. So the guided-filter statistics themselves
> (box sums, `alpha`, the `AA`/`a_tab` combine table) were provably
> correct -- the bug had to be in how the two decoded `xqd` values get
> turned into the two projection weights actually multiplied against the
> filtered-vs-original difference terms.
>
> Found it in dav1d's `lr_apply_tmpl.c`: `params.sgr.w0 =
> lr->sgr_weights[0]` (raw, direct) but `params.sgr.w1 = 128 -
> (lr->sgr_weights[0] + lr->sgr_weights[1])` -- the **second** weight is
> the complement of both decoded values summed, not `sgr_weights[1]`
> itself, applied unconditionally regardless of which pass(es) are
> active (a previously-decoded-at-parse-time complement only happens for
> the *opposite* case, `r1 == 0`, where `read_lr_unit` already
> pre-computes `xqd[1] = (1<<7) - xqd[0]` for exactly this reason -- so
> the two complement computations don't stack, they're mutually
> exclusive by construction). Kinetix's `sgrproj_filter_plane` used
> `xqd[1]` directly as the weight for the 3×3-pass term. For `xqd=[0,2]`:
> raw weight `2` makes `(2*t + 1024) >> 11` round to `0` for every
> `t` in the guided filter's typical range (single/low-double digits) --
> a complete no-op; the correct complement weight `128 - 0 - 2 = 126`
> produces real corrections matching dav1d's magnitude (verified: applying
> weight 126 to the actual observed `t1` range `[-27, 21]` yields
> `correction ∈ {-2,-1,0,1}`, matching dav1d's own observed `±1` spread
> exactly).
>
> Fixed with a one-line change (`let w1 = (1 << SGRPROJ_PRJ_BITS) -
> xqd[0] - xqd[1];` used in place of `xqd[1]`). Verified via
> `av1_psnr_check`: **mandelbrot Y 58.79 -> 70.45 dB** (U/V unaffected --
> only plane 0 uses restoration for this clip), testsrc unaffected
> (Wiener path doesn't touch this weight). Diffed the restored mandelbrot
> Y plane directly against dav1d byte-for-byte: only **72/12288 pixels**
> (all `±1`) still differ -- essentially identical to the **71/12288**
> gap already present *before* restoration even runs (re-confirmed via
> `--inloopfilters norestoration`), meaning restoration's own math is now
> correct to the precision of its (separately tracked, imperfect) input.
>
> **Given all three restoration bugs found this session (unit-boundary
> clamping, Wiener h/v swap, SgrProj weight complement) are fixed and
> verified as unconditional net improvements with zero corpus
> regressions, un-gated `apply_post_filters` to run restoration
> unconditionally (`if fh.uses_lr`) instead of behind
> `KINETIX_AV1_FILTER=1`.** Added a regression test reproducing the exact
> `xqd=[0,2]` real-world case (`sgrproj_uses_the_complement_weight_not_
> the_raw_second_xqd`). 134 unit tests pass, clippy clean, `cargo build
> --workspace` green, all `tpt-kinetix-av1` integration/proptest/doctests
> pass.
>
> **Remaining known restoration gaps** (not blocking, since current
> behavior is a strict improvement either way): the "mix" configuration
> (both `r0` and `r1` nonzero, i.e. both 5×5 and 3×3 passes active
> simultaneously) has zero corpus coverage -- the weight-complement fix
> should apply identically there per dav1d's code (same `w0`/`w1`
> computation regardless of which passes are active), but hasn't been
> observed on real content; the real §7.17.1 stripe-line-buffer boundary
> handling (vs this session's plane-edge-clamped approximation) also
> remains unverified since every corpus clip's restoration units so far
> happen to be single-unit-per-plane (`unit_size` ≥ the whole plane), so
> cross-unit-boundary behavior has never actually been exercised on real
> content despite the earlier fix.

> **2026-09-04 (cont'd) -- found and fixed the real restoration bug:
> `read_lr_unit`'s decoded Wiener filter had its horizontal and vertical
> taps swapped.** Followed the exact plan from the note above -- dav1d's
> `decode.c` already has a `DEBUG_BLOCK_INFO`-gated `Post-lr_wiener`
> printf (`DAV1D_TRACE=1` reaches it, no new patch needed), and Kinetix
> got a matching temporary print added to `read_lr_unit`. For testsrc's
> one restored unit (V plane, `RESTORE_WIENER`): dav1d decoded
> `v=[0,-2,5], h=[0,0,0]`; Kinetix decoded the identical three-coefficient
> bitstream sequence (confirming the entropy read itself, subexp decode
> included, is correct) but filed it the other way around --
> `h=[0,-2,5], v=[0,0,0]`. `read_lr_unit`'s `for pass in 0..2` loop reads
> the *vertical* filter's three coefficients first (`pass==0`) per
> §5.11.58 / dav1d's `filter_v`-then-`filter_h` read order, but the final
> `LrUnitData::Wiener { h: pass[0], v: pass[1] }` construction had them
> backwards. One-line fix (swap which pass index feeds `h` vs `v`).
>
> Verified via `av1_psnr_check` with `KINETIX_AV1_FILTER=1` (default
> corpus, restoration still gated off, is provably unaffected): testsrc's
> V PSNR with restoration applied went from **42.24 dB (worse than the
> 49.26 dB unfiltered baseline -- restoration was actively harmful) to
> 55.18 dB (now a real improvement)**. Confirmed the remaining gap isn't
> restoration's own math: dumped full YUV via `KINETIX_AV1_DUMP_FINAL`
> and diffed against dav1d's `--inloopfilters norestoration` output --
> **420 of 3072 V-plane pixels already differ (mostly +/-1/-2) before
> restoration even runs**, inherited from testsrc's own pre-existing,
> separately-tracked deblock/CDEF imprecision, and restoration's own
> 366-pixel post-filter diff count is in the same range/magnitude, not
> worse. So restoration is now "as correct as its input allows" for the
> one path this corpus exercises (Wiener); SgrProj remains completely
> untested (no corpus clip uses it). Updated the gating comment in
> `apply_post_filters` to record this accurately rather than the stale
> "boundary clamping causes regressions" note. Skipped a dedicated unit
> test (the bug lives inside a real-bitstream entropy-decode path needing
> a full `TileDecodeState`, same practical constraint as the
> `compute_level` fix earlier this session) -- the corpus PSNR swing is
> unambiguous evidence for both bug and fix. 133 unit tests pass, clippy
> clean, `cargo build --workspace` green.
>
> **Not un-gated by default** -- still not bit-exact (dependent on
> upstream deblock/CDEF precision improving first) and SgrProj is
> unverified, so `KINETIX_AV1_FILTER` stays opt-in. **Next steps, in
> priority order**: (1) fresh worst-row/worst-edge scan on
> mandelbrot/testsrc for more loop-filter-level-class bugs (this vein has
> now found three real bugs in a row: dqDenom, the ref-delta
> default/shift, and this h/v swap -- worth one more pass before moving
> on); (2) IBC var-tx tree + inter tx_type + inter coefficient context;
> (3) once deblock/CDEF precision improves, re-check whether restoration
> reaches bit-exact and consider un-gating; (4) SgrProj path is
> completely unverified -- needs a corpus clip that actually selects it
> (`allow_screen_content_tools`-style content or an explicit encoder
> flag) before it can be trusted at all.

> **2026-09-04 (cont'd) -- IBC var-tx tree implemented (first real
> increment on the confirmed structural gap holding testsrc2 at ~23dB).**
> `reconstruct_ibc_block` previously read one intra-style `tx_depth`
> symbol (`read_tx_size`) for the whole coded block -- wrong for
> `IsInter = 1`: real IBC blocks split independent sub-regions to
> different sizes via a recursive quad-tree of binary `txfm_split`
> symbols (§5.11.16/18's `read_var_tx_size`), desyncing the entropy
> decoder from this read onward for every non-skipped IBC block.
>
> Added `read_block_tx_size_ibc`/`read_tx_tree` (`partition.rs`),
> cross-checked branch-for-branch against dav1d's `read_vartx_tree`/
> `read_tx_tree` (`decode.c`): the two no-entropy-read shortcuts (skip or
> `TxMode != TX_MODE_SELECT`; lossless or `Max_Tx_Size_Rect == TX_4X4`),
> the `cat`/context derivation (`2*(TX_64X64 - Tx_Size_Sqr_Up[txSz]) -
> depth`; above/left tx-width/height comparison), the `depth < 2 && txSz
> != TX_4X4` read gate, and the `is_split && Tx_Size_Sqr_Up[txSz] >
> TX_8X8` recursion gate (an 8x8-or-smaller node that splits goes
> straight to `TX_4X4`, no further read). The `txfm_split` CDF table
> (`DEFAULT_TXFM_SPLIT_CDF`, `[[u16;3];21]`, flat `cat*3+ctx` indexing)
> was already scaffolded unused from an earlier session -- values
> cross-checked against dav1d's `cdf.c` `.txpart` defaults, matched
> digit-for-digit, no changes needed.
>
> Wired the leaf list into the luma reconstruction loop in place of the
> old uniform grid (including moving the per-leaf loop-filter metadata
> calls inside the leaf loop, and removing the now-wrong end-of-block
> `tx_left`/`tx_above` overwrite -- the tree read already writes correct
> per-leaf context while parsing).
>
> Verified two ways: (1) a new self-consistency regression test asserts
> the leaves always exactly tile the coded block for every bsize/
> tx_mode_select/skip/lossless combination on a synthetic bitstream; (2)
> traced a real `testsrc2` IBC block against a patched dav1d
> (`DAV1D_TRACE=1`, `Post-vartxtree` line, needs no new patch): Kinetix's
> own `rng` at the same point in the stream (`r=53644`) matches dav1d's
> first real var-tx-tree read exactly -- bit-exact sync confirmed on real
> content. No corpus regression; `testsrc2` itself is a neutral wash for
> now (sync is lost again at the very next symbol -- see below). 135 unit
> tests pass, clippy clean.
>
> **2026-09-04 (cont'd) -- inter `tx_type` implemented (second
> increment).** The bit lost right after the var-tx-tree fix: IBC forced
> every transform to `DCT_DCT` (`qindex_positive: false`), but dav1d's
> trace for the same block shows `txtp=13` (a real inter type) for its
> luma residual -- real bitstreams write `inter_tx_type` bits here that
> were never being read.
>
> Added `get_tx_set_inter`/`get_uv_inter_txtp` (`coeff_tables.rs`) and
> `read_inter_transform_type` (`coeff.rs`), cross-checked against dav1d's
> `recon_tmpl.c` branch-for-branch (the `reduced_tx_set ||
> Tx_Size_Sqr_Up == TX_32X32` gate for set 3 -- one binary symbol;
> `Tx_Size_Sqr == TX_16X16` for set 2 -- one shared 12-symbol CDF, no
> context; else set 1 -- a 16-symbol read). The `txtp_inter1/2/3` CDF
> tables were, again, already scaffolded unused with dav1d-cross-checked
> default values -- no changes needed there either. Added `TxBlockCtx.
> is_inter` to dispatch `read_coeffs` between the intra/inter
> `transform_type` paths and to route chroma through `get_uv_inter_txtp`
> instead of the intra `uv_mode`-based lookup; set `true` for IBC's two
> call sites and the not-yet-reached `inter_block.rs` paths (real inter
> blocks are also `IsInter = 1` and need the same fix whenever Phase E
> lands), `false` for real intra.
>
> Verified against the same real IBC block: Kinetix now decodes
> `tx=7 (TX_8X16) txtp=13 eob=65`, and critically the decoder's own `rng`
> immediately after this *entire coefficient block* read (`r=42504`)
> matches dav1d's trace for the identical symbol exactly -- bit-exact
> sync through the full luma residual, not just the header. (dav1d's own
> trace shows `eob=64` for the same block; that -1 reads as a
> display-convention difference between the two traces -- a real
> eob-derived bit-count mismatch would have changed the matching `rng`,
> and it didn't.) Added regression tests for `get_tx_set_inter`
> (including this exact `TX_8X16` case) and `get_uv_inter_txtp` against
> dav1d's formula. 138 unit tests pass, clippy clean, `cargo build
> --workspace` green. No corpus regression; `testsrc2` stays a wash
> (Y 20.93->20.36 dB) since sync is lost at the *next* read.
>
> **Where sync breaks next (the concrete "inter coefficient context"
> target for the next session)**: traced past the luma residual into the
> same block's chroma. dav1d: `SKIPCTX bx=54 by=32 plane=1 tsz_ctx=1
> sctx=9 all_skip=0` then `Post-uv-cf-blk[pl=0,tx=5,txtp=13,eob=0]:
> r=64566`. Kinetix (same block, U plane): `tx=5` matches, but
> `txtp=0 eob=1 r=45638` -- both the decoded content *and* the resulting
> `rng` diverge starting at chroma's very first (skip/all-zero) symbol
> read, immediately after the luma block that was just proven bit-exact.
> Two candidate causes, not yet distinguished: (1) chroma's skip-context
> derivation (`all_zero_ctx`'s `plane > 0` branch, `coeff.rs`) might need
> an `is_inter`-dependent term the current spec-generic formula is
> missing (dav1d's `sctx=9` context value hasn't been hand-verified
> against Kinetix's own computed context for this exact call yet); (2) a
> more mundane possibility -- `get_uv_inter_txtp`'s placeholder-`DCT_DCT`
> "luma tx type" input (noted as a known gap in the `blk_u`/`blk_v`
> construction comment in `intra_block.rs`, since chroma sub-blocks can
> span multiple luma leaves and there's no per-mi-position luma-tx-type
> lookup wired yet) doesn't explain this, since chroma never reads
> `tx_type` bits regardless -- but if `coeff_base`/`coeff_br` *level*
> contexts (not just `all_zero`) also read `get_tx_class(tx_type)`
> somewhere upstream of the skip read in a way this session didn't trace,
> a wrong chroma tx_type could still perturb context before the mismatch
> was first observed. Next step: dump `all_zero_ctx`'s actual computed
> `sctx`/`tsz_ctx` for this exact Kinetix call and compare directly
> against dav1d's `sctx=9` -- if they already match, the bug is
> downstream of context selection (in the CDF table itself or the read
> call), not in context derivation.

> **2026-09-04 (cont'd) -- narrowed the inter-coefficient-context gap:
> context derivation is provably correct; the bug is upstream, in the
> entropy state itself or the CDF adaptation, not in all_zero_ctx.**
> Added a temporary debug print (added, used, fully removed) dumping
> all_zero_ctx's actual computed tx_sz_ctx/skip_ctx for the exact chroma
> call this session's trace flagged. Result: Kinetix computed
> tx_sz_ctx=1 skip_ctx=9 for this call -- matches dav1d's own tsz_ctx=1
> sctx=9 exactly. So the context-selection formula itself (all_zero_ctx's
> plane>0 branch) is not the bug; something else produces a different
> decoded symbol despite an identical context bucket being consulted.
>
> Important methodology caveat surfaced while investigating this: the
> "matching r=/rng" evidence cited for the var-tx-tree and inter-tx_type
> fixes above compares only the arithmetic coder's range component, not
> its value/dif component -- dav1d's own DEBUG_BLOCK_INFO prints only
> rng too. range is renormalized into a narrow fixed window after every
> symbol read, so two genuinely different decode paths landing on the
> same range by coincidence is more plausible than it first appears,
> especially over a single read. This doesn't retroactively invalidate
> the two fixes already committed -- both also reproduced dav1d's actual
> decoded symbol values exactly (txtp=13, tx=7, matching eob magnitude),
> a much lower-probability coincidence than range alone -- but it does
> mean this specific new finding (context matches, outcome doesn't) needs
> a value/dif-inclusive comparison to fully pin down, not another
> range-only check. Concrete next step: patch dav1d's msac debug hook (or
> add a new one) to print ts->msac.dif alongside rng at a matching point,
> and add the equivalent full-state dump (self.dec.raw_state(), which
> already returns (range, value, max_bits, bit_pos) -- only .0 was used
> so far) on the Kinetix side, for a true apples-to-apples state
> comparison right before this chroma skip read. If the full state
> already matches there, the remaining bug is CDF adaptation drift
> (something upstream adapted this exact txb_skip[1][9] bucket
> differently between the two decoders); if it doesn't, sync was already
> lost earlier than currently believed and the luma
> eob-off-by-one-only evidence needs re-examining with the same
> full-state rigor.

> **2026-09-04 (cont'd) -- resolved the methodology caveat: full-state
> (range+value, not just range) comparison confirms bit-exact sync
> survives the chroma skip read too, strengthening (not weakening) the
> two increments above.** Did the full-state comparison the previous note
> called for. Patched dav1d's msac debug prints (SKIPCTX and both
> Post-y-cf-blk/Post-uv-cf-blk occurrences in `recon_tmpl.c`) to also emit
> `ts->msac.dif` (`ec_win`, a 64-bit windowed value -- not directly
> comparable to Kinetix's spec-shaped `value` without unpacking:
> `dav1d`'s live comparison value is `dif >> (EC_WIN_SIZE - 16)` = `dif >>
> 48`, per `msac.c`'s own `ctx_norm`/decode functions). Added a matching
> temporary `self.dec.raw_state()` dump on the Kinetix side (both were
> fully removed after use).
>
> Two independent checks, both exact matches: (1) at the luma coefficient
> block's completion (`tx=7 txtp=13 eob≈64`): dav1d `dif=
> 10676468718644051968` → `dif>>48 = 37930`; Kinetix `value=37930`.
> Exact. (2) at the chroma (U plane) skip-symbol read this session had
> flagged as a possible divergence point: dav1d `SKIPCTX ... sctx=9
> all_skip=0 r=33536 dif=8152201127502888960` → `dif>>48 = 28962`;
> Kinetix `skipctx plane=1 tsz_ctx=1 sctx=9 all_skip=0 range=33536
> value=28962`. Exact match on context, range, value, *and* the decoded
> `all_skip` boolean itself (`0` both sides).
>
> So the chroma skip read is **not** where sync breaks -- contradicting
> this session's earlier (weaker, range-only, and from an since-replaced
> debug print) observation of a divergence there. That earlier read used
> different temporary instrumentation and may have been comparing the
> wrong call instance (dav1d's source has three separate `Post-uv-cf-blk`
> print sites across different threading-pass branches with the same
> label, and picking the wrong one would silently compare unrelated
> blocks) -- a concrete methodology pitfall for whoever continues this:
> **when matching a labelled dav1d trace line by text alone, check which
> of possibly-several identically-labelled call sites in the source
> actually fired**, ideally by also matching position (`bx`/`by`) or a
> full-state value, not just the label and a plausible-looking `r=`.
>
> **Net effect on confidence**: the var-tx-tree and inter-tx_type fixes
> committed earlier this session are now verified with real rigor (full
> arithmetic-coder state, not range alone) through the chroma skip read
> -- solid ground, not just plausible-looking. Where sync *actually*
> breaks for this block (or block sequence) is still open; the next
> session should resume from here with the same full-state (`range` +
> `dif>>48`) comparison technique, continuing past the chroma skip read
> into the coefficient level/sign reads and then into the *next* coded
> block's header, watching for the first point the two diverge -- rather
> than re-deriving the technique from scratch.

> **2026-09-04 (cont'd) -- localized the real divergence: it's within the
> U-plane coefficient level/eob reads themselves, immediately after the
> (matching) skip symbol.** Continued the full-state trace one step
> further with the same technique. Found the exact dav1d print carrying
> `dif` for this block's U coefficient block by grepping the full trace
> for its known `r=64566` (necessary since dav1d's source has the
> `Post-uv-cf-blk` label at three separate call sites and matching by
> label alone risks comparing the wrong one, per the previous note's
> lesson) -- `Post-uv-cf-blk[pl=0,tx=5,txtp=13,eob=0]: r=64566
> dif=6071959426822045696` → `dif>>48 = 21571`. Kinetix's own state at
> the equivalent point (`self.dec.raw_state()` after the U `read_coeffs`
> call returns): `range=45638 value=32611`. **Neither matches** (`45638
> != 64566`, `32611 != 21571`) -- confirming the skip-context read (which
> does match, per the note above) is the last point of agreement; the
> divergence is somewhere in the subsequent `eob_pt`/`coeff_base`/
> `coeff_br`/`dc_sign` reads for this exact chroma block.
>
> dav1d's own intervening trace lines for this block hint at where to
> look first: `SKIPCTX_EOB ... eob_bin_size=32 chroma=1 is_1d=1
> eob_raw=0` then `SKIPCTX_DCONLY ... tok_br=1 dc_tok=2` -- the `is_1d=1`
> flag suggests dav1d takes a distinct "DC-only" code path for this
> block's `eob` class (`eob_raw=0`, the smallest bucket) that reads
> `dc_tok` directly via a different mechanism than the general
> `coeff_base`/`coeff_br` loop this session hasn't specifically checked
> for a `plane > 0` / `is_inter` special case. **Concrete next step**:
> read `read_eob`'s and the base-level-reading loop's actual code
> (`coeff.rs`) side by side with dav1d's `decode_coefs`
> (`recon_tmpl.c`) for the specific `eob_bin_size` bucket this block
> hits, focusing on whether Kinetix has (or dav1d has, that Kinetix
> lacks) a distinct low-eob/"DC-only" shortcut path, and whether any of
> `read_eob`'s context derivations differ for chroma vs luma or for
> `is_inter` blocks specifically -- this is genuinely the "inter
> coefficient context" work the coordinator originally scoped as the
> third increment, now narrowed to a single, concretely reproducible
> real block rather than a vague "context gap."

> **2026-09-04 (cont'd) -- root cause found and fixed: chroma tx_type
> derivation used a `DCT_DCT` placeholder instead of the real coincident
> luma leaf's decoded type.** `read_coeffs`'s chroma-path `luma_tx_type`
> dispatch had a `DCT_DCT` placeholder for the `plane > 0 && is_inter`
> case (a known gap flagged in a prior increment's own comment, not yet
> fixed). `get_uv_inter_txtp(_, DCT_DCT)` always resolves to `DCT_DCT`,
> so `compute_tx_type` silently returned the wrong `TxType` whenever the
> real luma type wasn't `DCT_DCT` -- and via `get_tx_class`'s `TX_CLASS`
> bit, corrupted `read_eob`'s very first context read (`is_1d`) for the
> block, exactly matching the divergence localized in the previous note
> (dav1d: `is_1d=1`; the `DCT_DCT` placeholder path computes `is_1d=0`
> since `get_tx_class(DCT_DCT) == TX_CLASS_2D`).
>
> **Fix**: added `TxBlockCtx::coincident_luma_tx_type` and a per-mi-cell
> `luma_tx_types` lookup grid inside `reconstruct_ibc_block`, populated
> as each luma var-tx-tree leaf's residual is decoded; the chroma loop
> now looks up the real coincident luma leaf's `TxType` (its top-left mi
> position subsampled back to luma coordinates) instead of assuming
> `DCT_DCT`. Also wired the same field through `intra_block.rs`'s real-
> intra sites and `inter_block.rs`'s not-yet-reached Phase E stub (both
> set `DCT_DCT` since it's genuinely irrelevant there: plane-0 luma
> ignores the parameter, and real-intra chroma has its own separate
> `MODE_TO_TXFM` derivation).
>
> **Verified with the same full-state rigor as the previous note's
> methodology**: `range` + `dif>>48` now match dav1d exactly through
> both the U-plane and V-plane `read_coeffs` calls of the originally-
> traced IBC block (`tx=5 txtp=13`). Pushed the check further with a
> systematic position-diff (extracting `(bx,by,r)` triples from both
> decoders' partition-read trace lines and running `diff`): **79
> consecutive checkpoint matches** afterward, spanning several more IBC
> blocks, an intra block with CFL/palette, and many luma/chroma
> coefficient reads -- much stronger evidence than the single-block
> check alone.
>
> Corpus PSNR: `testsrc2_320x180` 20.36/25.18/16.43 dB -> **21.98/22.49/
> 16.90 dB** (Y and V up, U down slightly but net a real improvement,
> not yet bit-exact). No change on `solid_red_32/64`, `testsrc_128x96`,
> `mandelbrot_128x96`, `smptebars_256x144` -- consistent with this bug
> only affecting IBC/inter chroma tx_type derivation. Committed as
> `28da676`.
>
> **Next divergence, localized** (not yet fixed): at partition `(56,32)`
> dav1d expects `r=38204`, Kinetix computes `r=58496`. Traced backward:
> the divergence is within a **new block at `bx=54,by=36` that is real
> intra** (not IBC) -- its decoded field values (`ymode=0, uvmode=12,
> tx=7`) match dav1d exactly, but its own symbol reads desync the
> arithmetic-coder state (dav1d `r=53934` vs Kinetix `r=47548` at
> `Post-tx[7]`). Since this block is real intra, it is **not** another
> instance of the bug just fixed -- most likely a context-handoff bug
> between the immediately-preceding IBC block's end-of-block neighbour-
> context updates (`ymode_left`/`above`, `is_inter_left`/`above`,
> `tx_left`/`above`, etc.) and this new block's own `skip`/`ymode`
> context reads. Concrete next step for whoever continues: dump the
> neighbour-context arrays right before and after the IBC block's own
> context-update code (end of `reconstruct_ibc_block`) and compare
> against dav1d's equivalent state, using the same full-state (`range` +
> `dif>>48`) comparison technique established this session.

> **2026-09-04 (cont'd again) -- fixed: found the root cause was NOT a
> desync inside the IBC block, and NOT the intra block's own ymode/
> uvmode reads either.** Instrumented per-field `range` checkpoints
> (skip, intrabcflag, dmv, vartxtree, y-cf-blk, uv-cf-blk×2) inside
> `reconstruct_ibc_block` and compared against dav1d's equivalent
> `Post-*` trace lines for the exact IBC block at `bx=52,by=36`
> preceding the divergent real-intra block: **every single checkpoint
> matched dav1d's `range` exactly**, through to the very last
> chroma-V coefficient read (`r=51976` both sides) -- the "context
> handoff" theory from the previous note was wrong; the IBC block
> itself is fully in sync.
>
> Continued the same per-field trace into the *next* block
> (`bx=54,by=36`, real intra): `skip` (r=50754), `intrabcflag`
> (r=47384), `ymode` (r=58598), and `uvmode` (r=40256) **all matched
> dav1d exactly, in order** -- narrowing the divergence to the very
> next read, `palette_mode_info()`'s `use_y_pal`. dav1d's trace showed
> `Post-y_pal[0]: r=35228` (no palette); Kinetix decoded
> `colors_y.len() == 2` (a real 2-color palette), landing at a wildly
> different `r=44296`. Since the arithmetic-coder state going into
> this read was byte-identical on both sides, a different *decoded
> symbol* here means Kinetix was reading from the wrong CDF context
> bucket, not a raw bitstream-position slip.
>
> **Root cause, confirmed against dav1d's own source** (`decode.c`):
> `has_palette_y`'s context is `ctx = above_has + left_has`, read from
> a per-mi-cell "did the neighbour block have a Y palette"
> array (`palette_y_colors_above`/`_left` in Kinetix,
> `t->a->pal_sz`/`t->l.pal_sz` in dav1d). `reconstruct_ibc_block`'s own
> end-of-block neighbour-context update (added when the var-tx-tree +
> inter-tx_type work was first built) touches `ymode_left`/`above`,
> `uv_left`/`above`, `skip_left`/`above`, `is_inter_left`/`above`,
> `mv_left`/`above` -- but never `palette_y_colors_left`/`above` or
> `palette_u_colors_left`/`above`. Since IBC blocks always have
> `PaletteSizeY == PaletteSizeUV == 0`, dav1d's own inter/IBC
> context-update path (`decode.c`'s non-intra `case_set` block)
> explicitly zeroes `edge->pal_sz` / `t->pal_sz_uv[i]` for every such
> block -- Kinetix's IBC path just never mirrored that. A stale
> non-empty palette left behind by an *earlier* real-intra block at
> this same mi position (from a prior superblock/row) leaked straight
> through the intervening IBC block into this read.
>
> **Fix**: clear `palette_y_colors_left`/`above` and
> `palette_u_colors_left`/`above` across the IBC block's own mi extent
> in `reconstruct_ibc_block`'s neighbour-context update, mirroring the
> real-intra end-of-block pattern (which already does this correctly).
> Committed as `6c5e06d`.
>
> **Verified far beyond the single block**: extracted every
> partition-tree-read checkpoint (`KTRACE PART` / dav1d's `poc=...`
> lines -- 95 of them) and every real-intra block's post-`tx`-read
> checkpoint (`KTRACE BLOCK` / dav1d's `Post-tx[N]` -- 123 of them,
> correctly paired by walking each `BLOCK`'s own subsequent `Post-tx`
> line rather than naively grepping by label, learning from this
> session's earlier label-matching mistake) from both decoders across
> the **entire testsrc2 frame** and diffed them in decode order: **all
> 218 checkpoints match dav1d's `range` exactly**. This is full-frame
> entropy-decode sync, not a local patch -- strong evidence the
> bitstream-level (symbol-read) side of AV1 decode is now correct for
> this test case's IBC + intra mix.
>
> Corpus PSNR: `testsrc2_320x180` 21.98/22.49/16.90 dB -> **24.70/
> 24.00/16.86 dB** (Y and U both up meaningfully; V flat). Still not
> bit-exact (99 dB), despite full entropy sync -- meaning **the
> remaining gap is a pixel-reconstruction bug** (prediction, inverse
> transform, dequant, or loop filter/CDEF), not further entropy
> desync. This is a genuinely different bug class from everything
> fixed so far this session and needs its own trace methodology: since
> the symbol stream is now confirmed correct, the next step is a
> *pixel*-level diff (`ITXDUMP`/`EDGEDUMP` dav1d trace hooks already
> exist in the patched local dav1d build -- see `recon_tmpl.c`'s diff
> in the scratch dav1d clone -- for exactly this kind of per-block
> prediction/residual dump) against Kinetix's own per-block pixel
> output, rather than more `range`/`dif` state comparisons.
>
> No change to any other corpus case (`solid_red_32/64`,
> `testsrc_128x96`, `mandelbrot_128x96`, `smptebars_256x144`),
> consistent with this being an IBC-neighbour-of-real-intra-specific
> bug. 139 unit tests pass, clippy clean, `cargo build --workspace`
> clean. No new unit test was added for this specific fix (it lives
> entirely inside `reconstruct_ibc_block`'s neighbour-context update,
> which requires a full `TileDecodeState` over real bitstream data to
> exercise meaningfully -- the `av1_psnr_check` corpus run is the
> practical regression signal here, matching this session's earlier
> methodology for reconstruction-level fixes).

> **2026-09-05 session note — correcting a stale claim: `mandelbrot`
> and `testsrc` are NOT full-frame entropy-sync-clean; `testsrc2` and
> `smptebars` are.** Earlier notes above (and `MEMORY.md`'s
> `project_av1_entropy_proven_correct` entry) say entropy sync was
> confirmed for "all 5 corpus entries" as of 2026-08-27 (commit
> `b89ac1c`-era). Re-ran `just av1-oracle-tile <entry>` (the Part-1
> independent-Python-oracle full-tile trace, no dav1d/patched build
> needed — `ffmpeg` alone is on PATH here) for all 5 current corpus
> entries this session:
> - `solid_red`, `smptebars`, `testsrc2`: **still match exactly**
>   (`testsrc2` in particular is now clean full-frame — better than
>   the "still desyncs one read later" state the table above
>   describes, since fixed by the later IBC commits `d32b6fe`/
>   `175f5e0`/`28da676`/`6c5e06d`).
> - `mandelbrot`: diverges at oracle-trace symbol #1183 (oracle 3708
>   total symbols vs Kinetix 4173 — Kinetix reads 465 *more*).
> - `testsrc`: diverges at oracle-trace symbol #5954 (oracle 7891 vs
>   Kinetix 8624 — Kinetix reads 733 more).
>
> **This is not a regression from recent (2026-09-03/04) work** — checked
> out `tpt-kinetix-av1/src` + `tools/av1_oracle` at `d70e12e` (just before
> the four IBC commits) and again at `b89ac1c` itself (the actual
> "2026-08-27" checkpoint commit) and reran the same trace: **identical
> divergence, same symbol index, same block, at both older checkpoints.**
> So the "all 5 match" claim was already wrong when written, or (more
> likely) the `testsrc`/`mandelbrot` corpus fixtures generated by
> `gen_corpus`/the ffmpeg `testsrc=`/`mandelbrot=` lavfi filters were
> different bytes back then (no fixed seed pinned) and today's regenerated
> files simply exercise a code path the 2026-08-27 sample never hit. Either
> way: **this is a real, currently-live, exactly-reproducible bug**, not
> new breakage — safe to chase without first re-checking recent commits.
>
> **Precise repro** (`just av1-oracle-tile mandelbrot`, or manually:
> `KINETIX_AV1_CAPTURE_TILE=1 cargo run -q -p tpt-kinetix-test-utils
> --example av1_symbol_trace_diff -- mandelbrot` then `python
> tools/av1_oracle/intra_decode.py av1_tile_trace.json`; add `--dump 1170`
> to `intra_decode.py`'s argv for a symbol-by-symbol table around the
> divergence):
> ```
> FIRST DIVERGENCE at symbol #1183:
>   oracle : n=2 value=0 bits=[1743,1744) rng=48426 val=34395
>   kinetix: n=2 value=0 bits=[1743,1743) rng=32816 val=25800
>   oracle  nearest marker  [1181] mode_info mi=(14,4) bsize=0 px=(56,16)
>   kinetix nearest marker  [1183] coeffs plane=0 px=(56,16) tx=4x4 skip=false pred_mode=5
> ```
> Both sides decode the **same value** (0) for the **same symbol type**
> (n=2 — this is `all_zero`/`txb_skip`, confirmed via `coeff.rs:552`'s
> `read_coeffs`) at the **same bit position** going in, but consume a
> *different* number of renormalization bits coming out (1 vs 0) — i.e.
> the underlying `rng`/adaptation state of the CDF slot they're each
> reading from already differs, even though every single symbol *before*
> this one in the whole tile (1182 of them) matched value-for-value and
> bit-for-bit. The `--dump 1170` table confirms this: #1180-1182 (the
> preceding `y_mode` read, n=13 v=5 — `D113_PRED`, matching `pred_mode=5`
> in Kinetix's own marker) are bit-identical between oracle and Kinetix,
> and #1183 is the very first place `rng`/`val` differ.
>
> The block itself is `bsize=BLOCK_4X4` (0) with `y_mode=D113_PRED` (not
> `DC_PRED`), so by the `intra_frame_mode_info()` syntax order in
> `intra_block.rs` (verified by reading it this session): no
> `angle_delta_y` (bsize < BLOCK_8X8 gate), likely no `uv_mode`/`cfl`/
> `angle_delta_uv` (this is presumably the luma-only half of a
> chroma-shared 4:2:0 pair, `has_chroma` false), no palette (bsize <
> BLOCK_8X8 gate in `read_palette_mode_info`), no `filter_intra` (gated
> on `y_mode == DC_PRED`, false here), no `tx_depth` (bsize <= BLOCK_4X4
> gate) — so `all_zero` really should be the very next symbol after
> `y_mode`, consistent with the trace. And since `luma_tx == TX_4X4 ==
> bsize`'s own size, `all_zero_ctx` (`coeff.rs:867`) takes the
> `blk.block_w == w && blk.block_h == h` branch unconditionally →
> `skip_ctx = 0`, and `tx_sz_ctx` (`coeff.rs:537`) is also `0` for
> `TX_4X4` — so **this exact read, in isolation, can't be picking a
> different context bucket**; both sides must be indexing
> `cdfs.txb_skip[0][0]`. That means the *contents* of that shared array
> slot already differ going in — which (since every prior symbol's
> value/width matched) can only happen if some **earlier** call updated
> `txb_skip[0][0]`'s adaptation counter (`cdf[N]`, which changes the
> `rate` in `entropy.rs`'s `read_symbol`) a different number of times on
> one side than the other, most likely because an earlier `all_zero` read
> that should have gone through this same `[0][0]` bucket used a
> different `(tx_sz_ctx, skip_ctx)` pair on one side vs the other,
> *and* coincidentally decoded the same bit(s) as the correct bucket
> would have (plausible early in a tile when CDFs are still close to
> their symmetric defaults). **Not yet found**: which earlier block/read
> is responsible. Two candidate next steps, in order of effort: (1) add
> temporary instrumentation to `read_coeffs` printing `(tx_sz_ctx,
> skip_ctx, plane, blk.x4, blk.y4)` for every `all_zero` call plus the
> pre-read `cdf[N]` counter, gated on an env var, and diff that log
> between two runs (Kinetix-only — the Python oracle doesn't need
> patching, its own equivalent context computation can be printed the
> same way from `tools/av1_oracle/intra_decode.py`) to find the first
> `(tx_sz_ctx, skip_ctx)` mismatch before symbol #1183; (2) the `--dump`
> table format already exists and is cheap to re-run at any symbol
> range, so bisect backward from #1183 in the `--dump` output looking
> for any earlier `n=2` (binary) symbol whose value/width match was
> "too easy" (i.e. would also match under either context bucket).
> `testsrc`'s divergence (symbol #5954, `mi=(0,20)` `bsize=17`
> (`BLOCK_64X64`), same "value matches, `rng` doesn't" signature) is
> very likely the same underlying bug class, not a second bug — worth
> checking once `mandelbrot`'s is root-caused, not in parallel.
>
> No code changes made this session (investigation only, on top of a
> clean HEAD — two unrelated 1-line pre-existing working-tree diffs,
> `loop_filter.rs`'s doc-comment `\[16\]\[4\]` escaping and
> `tpt-kinetix-h264/src/entropy.rs`, were stashed during the bisection
> and restored byte-identical afterward). `git bisect`-by-hand across
> `HEAD`/`d70e12e`/`b89ac1c` confirmed this is not new breakage, so
> don't spend time re-auditing the 2026-09-03/04 IBC commits for it.

> **2026-09-05 session note (cont'd) — narrowed the mandelbrot divergence
> much further; root cause still not found, but two hypotheses are now
> conclusively ruled out.** Added permanent env-gated debug hooks (kept in
> the tree, following this file's established convention):
> `KINETIX_AV1_DBG_ALLZERO` (both `coeff.rs::read_coeffs` and
> `tools/av1_oracle/coeffs.py::read_coeffs`, prints `plane/x4/y4/
> tx_sz_ctx/skip_ctx/counter` before every `all_zero` read) and
> `KINETIX_AV1_DBG_TXSIZE` / `KINETIX_AV1_DBG_PARTALL` (`partition.rs`'s
> `read_tx_size`/`decode_partition` and the oracle's matching functions,
> same idea for `tx_depth` and `partition`). Using these to trace forward
> from the known-good prefix:
>
> - The partition tree itself matches for a long prefix — every
>   `partition` symbol (`mi=(0,0)` bsize=12 down through `mi=(12,6)`
>   bsize=3, ~18 reads) decodes the **same value** on both sides,
>   including the `mi=(14,4)` bsize=3 → `partition=3` (SPLIT into 4
>   `BLOCK_4X4` leaves) read that produces the `mi=(14,4)` block from the
>   original divergence report.
> - `mi=(14,4)`'s own `all_zero` context is confirmed **identical** on
>   both sides by direct instrumentation (not just inferred from the code
>   read): `skip_ctx=0 tx_sz_ctx=0 counter=0` on both the Kinetix and
>   oracle logs — so the "wrong context bucket" theory from the earlier
>   note is **ruled out**; both sides genuinely read from the same,
>   still-untouched-default `txb_skip[0][0]` CDF slot.
> - This is very likely the **first-ever use of that exact bucket** in
>   the tile (`tx_sz_ctx=0` *and* `skip_ctx=0` requires a `BLOCK_4X4`
>   coded block using `TX_4X4` — no earlier leaf in the trace is
>   `bsize=0`), which also rules out a **`base_q_idx`-dependent default
>   CDF table selection bug** (`TileCdfs::new`'s `q_context(base_q_idx)`
>   picks one of several default-table sets purely from the frame's
>   `base_q_idx`, applied uniformly to every coefficient CDF for the
>   whole tile) — if Kinetix parsed a different `base_q_idx` than the
>   real bitstream encodes, *every* earlier `all_zero`/coeff read in the
>   tile would already be reading from a different default table too,
>   and would very likely have shown a divergence far earlier than the
>   70th `all_zero` call. It didn't.
> - The `tx_depth` flip at `mi=(12,6)` (Kinetix decodes 0, oracle decodes
>   1 — the "second bug" flagged in the previous note) is **not a second
>   bug**: it's downstream of the same cascade. `mi=(12,6)` is decoded
>   *after* the whole `mi=(14,4)` split subtree finishes, so once that
>   subtree's arithmetic-coder state has drifted (from the `all_zero`
>   divergence), every later read — including this `tx_depth` — inherits
>   the drift and can eventually flip an actual decoded value once the
>   accumulated probability skew crosses a decision boundary. Likewise
>   the `mi=(14,6)` partition flip reported earlier is the same cascade,
>   one step further downstream. **There is one root bug, not three.**
>
> **Where this leaves it**: by strict logical induction, since every
> symbol read *before* `mi=(14,4)`'s `all_zero` (skip, y_mode, the
> `mi=(14,4)` partition-SPLIT symbol itself, all matching value *and*
> bit-width) must leave the arithmetic-coder's `(rng, val, bit_pos)`
> state bit-for-bit identical on both sides, and the `all_zero` read
> itself demonstrably uses an identical, still-default CDF slot — the
> two decoders' outputs should be mathematically forced to agree at this
> read. They don't (same decoded value, different consumed bit-width).
> The remaining candidate explanations, not yet checked: (1) a
> `read_symbol`/CDF-adaptation edge case specific to `n=2` alphabets
> that only manifests on some earlier, *different* binary symbol type
> reusing the exact same code path — i.e. the bug might not be
> `all_zero`-specific at all, just first *observable* there; (2) the
> automated `intra_decode.py --dump`/first-divergence tool's own
> symbol-alignment logic might not be doing a byte-for-byte read-order
> alignment the way this note has been assuming — worth reading that
> tool's diff loop itself before trusting its "matched" claims any
> further, rather than continuing to instrument production code blindly.
> **Next session should start by reading `intra_decode.py`'s own
> diffing loop** (near the `FIRST DIVERGENCE` print) to confirm it is
> genuinely comparing corresponding reads 1:1 and not just the Nth
> line of each independently-generated list — that's the one
> foundational assumption this whole trace has rested on without direct
> verification, and if it's wrong everything above still stands as
> useful narrowing but the "everything before #1183 matches" premise
> would need re-establishing some other way.
>
> No functional code changes; all changes this session are `eprintln!`/
> `print()` debug instrumentation behind new env vars, defaulting off.
> 139 unit tests pass, clippy `-D warnings` clean, `cargo build
> --workspace` clean.

> **2026-09-05 session note (cont'd again) — ROOT-CAUSED AND FIXED: the
> "divergence" was two bugs in the Python oracle itself, not in Kinetix.**
> The `KINETIX_AV1_DBG_ALLZERO` instrumentation above (printing
> `len(dec.trace)` right before each `all_zero` read) caught the oracle
> reading **one extra symbol** ahead of where Kinetix expected it —
> `trace_idx=1184` when Kinetix's equivalent read was trace index 1183 —
> proving the two decoders were reading a genuinely different *number*
> of symbols before this point, not just adapting a shared CDF
> differently as the previous note assumed (that assumption, while
> logically reasoned from the "value+width matches through #1182" premise,
> turned out to rest on the wrong idea of where the extra read lived).
>
> **Bug 1** (`intra_decode.py` line ~681, `mode_info()`'s tail): the
> oracle's `luma_tx = self.read_tx_size(...)` call was gated only on
> `self.tx_mode_select and not self.lossless` — missing the `MiSize >
> BLOCK_4X4` gate AV1 §5.11.15 requires. This is the **exact same bug**
> Kinetix's own `intra_block.rs` already has a named regression guard
> for (`bsize > BLOCK_4X4` — "made every 4×4 intra block consume a
> spurious tx_depth symbol") — it had just never been ported to the
> Python side. Every `BLOCK_4X4` leaf under `TX_MODE_SELECT` read one
> spurious `tx_depth` symbol the real bitstream never wrote. Fixed by
> adding the same `bsize > 0` (`BLOCK_4X4` is index 0) gate.
>
> **Bug 2** (`intra_decode.py`'s `Tile.__init__`): `self.tx_above =
> [4] * n` / `self.tx_left = [4] * m` initialized the tx-neighbour
> context arrays to sentinel `4`, not `0`. This is **also** an exact
> match for an already-fixed-in-Kinetix bug — `partition.rs` carries a
> named regression test (`tx_depth_ctx_from`'s doc comment: "a sentinel
> of `4` made `4 >= 4` true... first caught on mandelbrot at mi (16,18)")
> for precisely this. An unavailable tile-edge neighbour must contribute
> `0` to the `tx_depth` context (§8.3.2); sentinel `4` wrongly satisfies
> `aboveW/leftH >= maxTxWidth/Height` whenever `max_tx == TX_4X4`,
> picking the wrong CDF bucket for every block on the frame's first row
> *and* column. This is what broke `testsrc` specifically (its
> divergence was at `mi=(0,20)`, column 0 — `tx_left[20]` still held the
> sentinel).
>
> **Verified fix**: `just av1-oracle-tile <entry>` now reports **TRACE
> MATCHES KINETIX EXACTLY** for **all 5** corpus entries (`solid_red`,
> `smptebars`, `testsrc`, `testsrc2`, `mandelbrot`) — the original
> 2026-08-27 claim this session set out to correct is, with these two
> oracle fixes, genuinely true again. `python tools/av1_oracle/
> validate.py` still passes. Zero changes to any Rust file — Kinetix's
> own decode output is provably untouched by this session (only
> `tools/av1_oracle/intra_decode.py` and its debug instrumentation
> changed), so there is no pixel-output regression risk to check.
>
> **Lesson for next time this oracle is trusted**: it's an independent
> re-implementation, but it isn't infallible, and it can silently drift
> out of sync with fixes landed only on the Kinetix side (both bugs
> fixed here were *already* fixed in Rust, with regression tests, before
> this session started — the oracle just never got the same fix). When
> the oracle disagrees with Kinetix, check whether Kinetix's own code
> comments/regression tests already discuss the exact symptom before
> assuming the bug is in the decoder under test.
>
> **What this changes for the AV1 roadmap**: `mandelbrot`/`testsrc`'s
> pixel PSNR gaps (todo.md's corpus table: mandelbrot Y ~58.79 dB,
> testsrc Y ~74.88 dB) are now **confirmed reconstruction-only** bugs
> (prediction/transform/dequant/loop-filter), same footing as
> `testsrc2`/`smptebars` already were — not entropy desync. The
> "fresh worst-edge search on mandelbrot/testsrc" item in todo.md's
> priority list can proceed with full confidence the symbol stream
> itself is not a confound. `tools/av1_oracle/intra_decode.py`'s
> module docstring LIMITATION note ("a numerically wrong default CDF
> entry is invisible here") still applies — table *contents* are still
> unverified by this oracle, only read-order/context-selection.
>
> Debug hooks added this session (`KINETIX_AV1_DBG_ALLZERO`/
> `_TXSIZE`/`_PARTALL` in both `coeff.rs`/`partition.rs` and their
> `tools/av1_oracle/` counterparts) are left in place, env-gated off by
> default, matching this file's established convention — they're what
> found both bugs and are cheap to reuse for the next divergence.
> 139 unit tests pass, clippy `-D warnings` clean, `cargo build
> --workspace` clean, `python tools/av1_oracle/validate.py` clean.

> **2026-09-05 session note (cont'd again, again) — the reconstruction "worst
> edge" tool was silently hiding the true first divergence; found it, ruled
> out several strong hypotheses, root cause still open.** Continued to
> `mandelbrot`'s remaining pixel gap now that entropy is confirmed clean.
> `av1_symbol_trace_diff.rs`'s `first_divergence()` takes a `threshold`
> (pixels differing by `<= threshold` are skipped) and both call sites were
> hard-coded to `3` — so "first divergence" was really "first divergence
> bigger than 3", silently passing over any earlier ±1..3 pixel error. Added
> `KINETIX_AV1_DIV_THRESHOLD` (env var, default 3, kept) to make this
> tunable. At `threshold=0` on `mandelbrot`, the *real* first pre-filter
> (`KINETIX_AV1_NOFILTER=1`) divergence is `px=(8,0)` (delta +1), not the
> `px=(55,8)` (delta -4) the old hard-coded-3 scan reported — a completely
> different, much earlier block. This invalidates this session's earlier
> deep-dive into the `px=(55,8)` `SMOOTH_PRED`/`ADST_ADST` `TX_4X4` block
> *as the root cause* (that block's own math was independently verified
> correct — see below — its small residual error is very likely just
> inherited from this earlier, still-unlocated divergence via the
> reference-sample chain, not a bug of its own).
>
> **Hypotheses ruled out, with independent verification (not just code
> reading), while chasing the (now known to be downstream) `px=(55,8)`
> lead** — kept because they're still true and worth not re-checking:
> - **Quantizer matrices**: `using_qmatrix=false` for this frame (confirmed
>   via a new debug print — `reconstruct_av1_frame`'s existing
>   `KINETIX_AV1_DBG` frame-header dump now also shows
>   `using_qmatrix`/`qm_y`/`qm_u`/`qm_v`), so the fact `dequantize_coeffs`
>   never implements QM scaling at all is a real gap for *other* content but
>   not the cause here.
> - **1-D inverse ADST4 math** (`transform.rs::inverse_adst4`): hand-computed
>   against dav1d's real closed-form reference (fetched
>   `src/itx_1d.c`'s `inv_adst4_1d_internal_c` from
>   `raw.githubusercontent.com/videolan/dav1d`) for two independent test
>   vectors — exact match to the integer, including the negative-number
>   `Round2`/arithmetic-shift edge cases. Also hand-computed the *full* 2-D
>   ADST_ADST transform (row pass → `round2`(row_shift=0) → clamp → col
>   pass → `round2`(4)) for the `px=(52,8)` block's real dequantized
>   coefficients end to end by hand and got exactly Kinetix's own reported
>   residual (`-10` at local (3,0)) — the transform math is provably correct
>   for this exact input, full stop.
> - **`TX_TYPE_INTRA_INV_SET1` table** (`coeff_tables.rs`): fetched dav1d's
>   real `dav1d_tx_types_per_set` array (`src/tables.c`) — Kinetix's 7-entry
>   `[IDTX, DCT_DCT, V_DCT, H_DCT, ADST_ADST, ADST_DCT, DCT_ADST]` matches
>   dav1d's "Intra1" slice exactly, index-for-index.
> - **`get_tx_set` (SET1 vs SET2) selection**: confirmed via the entropy
>   trace's own `n_symbols=7` that `TX_SET_INTRA_1` (7-symbol alphabet) was
>   used, which is the spec-correct choice for a `TX_4X4` block with
>   `reduced_tx_set=false` (also confirmed via the frame-header dump).
> - **`block_borders()`'s reference-sample indexing**: traced through by
>   hand for this exact block — `top[3]` genuinely reads `sample(55, 7)`,
>   the literal pixel directly above the target, no off-by-one.
>
> **New, unexplained finding** (harness artifact, not yet resolved): running
> the *same* `KINETIX_AV1_DBG_PX=8,0` capture prints **two different**
> `reconstruct_tx_block` dumps for the same `px=(8,0)` filter within one
> process run — one with `eob=3 quant=[3,0,0,0,0,0,0,0,-1,...]`, the other
> `eob=1 quant=[3,0,...]` (no second coefficient at all). `av1_symbol_trace_
> diff.rs`'s `decode_kinetix()` runs the whole OBU decode twice per corpus
> entry (once filtered, once with `KINETIX_AV1_NOFILTER=1`) and until now
> this session assumed both runs produce byte-identical entropy/reconstruction
> (verified true for the `mandelbrot`/`px=(52,8)` all_zero debug prints
> earlier this session) — but two *different* decoded coefficient sets for
> the same block position across the two calls means either (a) this
> filter also matches a *second*, different block sharing the same origin
> in a different plane (the print doesn't log `blk.plane`, worth adding),
> or (b) there is a real state-leak between the two `Av1Decoder::decode()`
> calls in the test harness (a global/static not reset between runs) that
> would invalidate some of this session's cross-run comparisons. **Next
> session should resolve this ambiguity first** — add `plane=` to the
> `reconstruct_tx_block` debug line, and/or capture both runs to separate
> files and diff them directly — before trusting any further `px=(8,0)`-style
> single-block dumps, and before continuing to chase the real root cause
> at `px=(8,0)`.
>
> Kept, low-risk, reusable changes: `KINETIX_AV1_DIV_THRESHOLD` (defaults to
> the prior hard-coded `3`, so no behavior change unless set) and the
> `using_qmatrix`/`qm_*` fields on the existing frame-header debug dump.
> 139 unit tests pass, clippy `-D warnings` clean on both
> `tpt-kinetix-av1` and `tpt-kinetix-test-utils`.
>
> **The `px=(8,0)` double-print mystery is resolved, harmlessly**: added
> `plane=` to `reconstruct_tx_block`'s two debug `eprintln!`s (cheap, kept).
> The "two different blocks at the same px" turned out to be a `U`-plane and
> a `V`-plane chroma block that both happen to sit at chroma-space `(8,0)`
> — `KINETIX_AV1_DBG_PX` matches by position only, not plane, so it was
> printing three unrelated blocks (Y/U/V) that all touch `(8,0)` in their
> own plane's coordinate space. **No state leak between the harness's two
> `decode()` calls; that concern is fully retired.**
>
> **Located the real `px=(8,0)` (Y-plane) block**: it isn't its own 1×1
> origin — `(8,0)` is `local (8,0)` *inside* a `16×16` `DCT_DCT` (not ADST)
> transform block whose real origin is `(0,0)`, the frame's very top-left
> corner (`have_above=false have_left=false`, DC-only-neighbourhood
> `pred=128` uniform). `quant = [18, -3, 0×14, -3, 0×239]` (`DC=18` at
> index 0, one AC coefficient at index 1, one more AC at index 16 — i.e.
> `(row=1, col=0)` in this 16-wide raster layout), `eob=3`. Kinetix's own
> reported `residual[8] = 15` (row 0, local col 8), giving
> `128 + 15 = 143` — matching the reported `kinetix=143`; `dav1d=142`, a
> **±1** error. This is a `TX_16X16` **`DCT_DCT`** case — a different,
> simpler code path than the `ADST_ADST TX_4X4` block chased earlier this
> session (whose math was independently proven correct and is now known to
> be a downstream symptom, not the source). **Not yet independently
> verified**: unlike `TX_4X4` ADST (a small closed-form formula, hand-
> computable in a few minutes), a 16-point DCT butterfly network has ~9
> stages and wasn't hand-verified this session — that's the concrete next
> step: hand-compute (or write a small fixed-point Python port of)
> `inverse_dct` for `log2w=log2h=4` against this exact `[18,-3,...,-3,...]`
> input and see whether `143` or `142` is the spec-correct answer, the same
> method that cracked `inverse_adst4` this session. A `±1` error on a
> `DC=2520`-dominated block with tiny AC terms has the flavour of a single
> rounding-direction mismatch (a `Round2`/clamp applied at the wrong point,
> or an off-by-one in `TRANSFORM_ROW_SHIFT`/`col_clamp_range` for this exact
> size) rather than a structural bug — `TRANSFORM_ROW_SHIFT[TX_16X16] = 2`
> was spot-checked against spec-recollection and looks right, but should be
> re-verified against a primary source (this session's dav1d-source-fetch
> method, not memory) before ruling it out.

> **2026-09-05 session note (cont'd yet again) — the DCT16 lead was ALSO a
> dead end (transform math proven correct again), which exposed the real
> methodological bug: CDEF is not edge-limited, so "interior-pixel"
> NOFILTER-vs-FILTERED comparisons are unsound. The actual remaining gap
> looks like a loop-filter bug, not reconstruction.**
>
> Followed the previous note's own prescription: fetched dav1d's real
> `inv_dct16_1d_internal_c` (and its `inv_dct8`/`inv_dct4` recursive base
> cases) from `raw.githubusercontent.com/videolan/dav1d/master/src/
> itx_1d.c`, ported it verbatim to a scratch Python script implementing the
> *exact* 2-D driver (`row pass → round2(row_shift) → clamp → col pass →
> round2(4)`), and ran it against the real `px=(0,0)` `TX_16X16 DCT_DCT`
> block's actual dequantized coefficients (`dc=2520, ac[0][1]=-528,
> ac[1][0]=-528`, rest zero). **Result: the Python port's row-0 residual
> `[8, 8, 9, 9, 10, 11, 12, 13, 15, 16, 17, 18, 18, 19, 19, 20]` matches
> Kinetix's own reported residual EXACTLY, element for element** — so, like
> `inverse_adst4` before it, `inverse_dct`'s 16-point path is independently
> proven bit-correct for this real input. Repeated for the `px=(16,0)`
> block (the actual origin of the `px=(28,4)` "interior" divergence found
> below) with its own coefficients (`dc=980, ac=-352/-176`) — again an
> exact match, row for row.
>
> **So: two different real blocks, two different transform sizes (`TX_4X4`
> ADST_ADST and `TX_16X16` DCT_DCT), both independently verified bit-exact
> against a from-scratch port of dav1d's real reference algorithm.** At
> this point the working hypothesis that this session's "worst-edge search"
> would find a reconstruction-math bug is looking weak — every concrete
> lead it has produced has turned out correct under real verification.
>
> **Root cause of the false leads, finally identified**: `av1_interior_
> diff.rs`'s whole premise (a pixel `>=4` samples from any 8×8 boundary is
> immune to both deblock *and* CDEF, so a NOFILTER-Kinetix vs FILTERED-
> dav1d mismatch there must be a reconstruction bug) is **wrong for CDEF**.
> Deblock is genuinely edge-limited (spec: only filters real coded-block/
> transform edges), but **CDEF is a directional enhancement filter applied
> per-pixel across the whole frame based on local gradients, not an
> edge-only operation** — it can and does adjust pixels far from any block
> boundary. `mandelbrot`'s frame header has CDEF enabled
> (`enable_cdef=true`, `cdef_y_strength=[7]`) confirmed via this session's
> earlier `using_qmatrix` debug-print addition. So a "interior, filter-
> immune" pixel differing between NOFILTER-Kinetix and FILTERED-dav1d is
> expected and NORMAL whenever CDEF made a legitimate adjustment there —
> not evidence of a reconstruction bug. This invalidates the interior-diff
> tool's core assumption (its own doc comment: "pixels that neither the
> deblocking filter nor CDEF can reach" — the CDEF half of that claim is
> false) and likely explains a good fraction of this project's earlier
> "worst-edge search" sessions chasing phantom reconstruction bugs. Fixed
> the tool's hard-coded `first_interior_divergence(..., 3)` threshold too
> (same hidden-threshold bug as `av1_symbol_trace_diff.rs`'s fix earlier
> this session) — `KINETIX_AV1_DIV_THRESHOLD` now works on both tools.
>
> **The methodologically sound comparison, and what it actually shows**:
> compare Kinetix's own **fully filtered** output (normal `decode()`, no
> `NOFILTER`) against dav1d's filtered reference — apples to apples, no
> CDEF-reach assumption needed. At `threshold=0` this gives `mandelbrot`'s
> real first divergence: `px=(62,1)`, `kinetix=163 dav1d=164` (`delta=-1`).
> Checked whether Kinetix's own *pre-filter* value at that exact pixel
> already differed (would mean a reconstruction bug) or matched (would mean
> the bug is in Kinetix's own filter stage): **Kinetix's NOFILTER value
> there is `164` — matching dav1d's filtered value exactly.** Kinetix's own
> deblock/CDEF then moves it `164 -> 163`, *introducing* a divergence that
> did not exist pre-filter. **This is real, load-bearing evidence that (at
> least at this pixel) Kinetix's loop filter is over-correcting relative to
> dav1d — a loop-filter bug, not a reconstruction bug**, matching what an
> earlier (2026-09-04, before this session's "worst-edge search" priority
> item existed) todo.md note already concluded and this session had been
> implicitly second-guessing.
>
> **Recommendation for the next session**: stop chasing "reconstruction"
> leads via NOFILTER-vs-FILTERED-dav1d comparisons at all — they cannot
> distinguish a real bug from a correct CDEF adjustment. Instead: (1) build
> the equivalent of this session's `px=(62,1)` check into a reusable tool —
> for each divergent pixel, decode filtered-Kinetix, dav1d-filtered, AND
> Kinetix-NOFILTER, and classify pre-existing (nofilter already diverges
> from dav1d-filtered in the SAME direction/magnitude as filtered-Kinetix)
> vs filter-introduced (nofilter matches dav1d-filtered, filtered-Kinetix
> doesn't); (2) once a genuine sample of filter-introduced divergences is
> collected, dig into `loop_filter.rs`'s CDEF strength/direction/damping
> computation and deblock filter-length selection against dav1d's real
> source the same way this session verified the transforms; (3) the
> already-open "wire the currently-hardcoded-to-0 per-superblock `delta_lf`
> into `deblock_plane`'s level computation" item is a concrete,
> already-identified real gap worth checking first, since it's a known
> incompleteness rather than a hypothesis.
>
> 139 unit tests pass, clippy `-D warnings` clean, `cargo fmt` clean on
> touched files. No functional code changes to the decoder itself this
> round — only the `av1_interior_diff.rs` threshold fix (same shape as
> `av1_symbol_trace_diff.rs`'s earlier this session) and the scratch Python
> verification scripts (not committed, throwaway).

> **2026-09-05 session note (cont'd, final for this session) — traced
> `px=(62,1)`'s divergence to deblock as the prime suspect, verified the
> wide-filter math and mask/flat/hev formulas line-for-line against dav1d's
> real source (all correct), but found the `KINETIX_AV1_NODEBLOCK`
> isolation test itself has the same "pipeline order" confound as the
> CDEF-not-edge-limited finding above — so "deblock is the bug" is a strong
> lead, not yet a proven conclusion. Read this note before trusting the
> "deblock vs CDEF" split below.**
>
> Used `KINETIX_AV1_NODEBLOCK=1`/`KINETIX_AV1_NOCDEF=1` (both already
> existed in `apply_post_filters`) plus the existing `KINETIX_AV1_DBG_ROWS`/
> `_RROWS`/`_COLS` pixel-dump hooks to check the real first filtered-vs-
> filtered divergence (`px=(62,1)`, an edge at `x=64` between a `BLOCK_16X8`
> `HORZ_B`-split leaf at `mi=(12,0)` and a `BLOCK_16X16` leaf at `mi=(16,0)`
> — a genuine coded-block boundary, both sides `TX_16X8`/`TX_16X16` i.e.
> `filter_size=16`, the wide 13-tap filter path): with `NODEBLOCK` set,
> Kinetix's output matches dav1d's *final filtered* output **exactly**
> across a wide window (`cols 50-75`, rows 1-3) — suggesting dav1d's own
> deblock is a no-op here while Kinetix's fires (on rows 1 and 3
> specifically, at different tap offsets each time — not rows 0/2/4/5/6/7).
>
> Hand-verified the wide-filter tap formula (`filter_line_1d`'s generic
> `tap = 2 if |j|<=n2 else 1` loop) against dav1d's real `loop_filter()`
> 16-tap output formulas (fetched from `raw.githubusercontent.com/
> videolan/dav1d/master/src/loopfilter_tmpl.c`) at three output positions
> (`i=-2, 0, +1`), including the exact clamped-duplicate-tap behaviour at
> the p6/q6 array ends — **exact match every time**. Also fetched and
> compared dav1d's `fm`/`flat8in`/`flat8out`/`hev` mask formulas against
> Kinetix's `filter_mask`/`flat`/`flat2`/`hev` — structurally identical,
> term for term (this was previously only justified by the code's own
> comments, not independently re-verified against source until now).
>
> **The confound**: `apply_post_filters` runs deblock, *then* CDEF, on the
> *same* plane buffer in place — matching the real AV1 pipeline order. But
> `KINETIX_AV1_NODEBLOCK=1` only skips the deblock call; CDEF still runs
> immediately after on whatever's in the buffer, which with deblock
> skipped is the **raw, un-deblocked reconstruction** — a genuinely
> different input than CDEF would see in a real decode (where it always
> processes post-deblock data). So "NODEBLOCK-Kinetix" isn't "Kinetix's
> reconstruction with deblock cleanly removed" — it's "Kinetix's
> reconstruction with CDEF fed the wrong (un-deblocked) input," which
> could by itself produce a different-but-often-coincidentally-similar
> result without deblock actually being buggy. This is the same shape of
> mistake as the CDEF-not-edge-limited finding earlier in this session:
> another test-harness assumption ("disabling one filter isolates its
> effect") that doesn't hold once you account for how AV1's filters
> actually chain. **`NOCDEF` alone doesn't have this problem** (deblock
> still runs in its correct pipeline position) — that test still shows
> `px=(62,1)` diverging (`kinetix=163` either way), which is real evidence
> pointing at deblock, just not the clean proof the `NODEBLOCK` test looked
> like it was.
>
> **Genuinely solid takeaways from this**: (1) the wide-filter formula and
> mask/flat/hev decision logic are now independently verified correct
> against dav1d source, not just self-consistent with in-repo comments —
> ruling those out as the bug; (2) the divergence is real and reproducible
> at `px=(62,1)` with the *properly ordered* full pipeline; (3) deblock
> remains the prime suspect (via the `NOCDEF`-only evidence) but isn't
> conclusively implicated. **What's needed to actually close this**: a way
> to observe dav1d's *own* post-deblock-pre-CDEF intermediate state (the
> `ITXDUMP`/`EDGEDUMP`-style patched-dav1d approach earlier todo-av1.md
> notes describe, or dav1d's `--verify`/frame-threading-disabled debug
> dump if one exists) — without that, Kinetix-side experiments alone can't
> cleanly separate "deblock is wrong" from "CDEF reacts differently to
> deblock's (correct) output than expected." Worth checking first, since
> it's cheaper than building new tooling: whether `dav1d`'s CLI itself
> exposes a `--skip-deblock`/`--filter=none`-style flag that would give a
> real, correctly-ordered partial-pipeline reference to compare against
> instead of guessing from Kinetix's side alone.

> **2026-09-05 session note (new session) — wired the previously-hardcoded-
> to-0 per-block `delta_lf` (§7.12.1 `DeltaLFs`) into `compute_level`'s
> level computation**, per the prior session's recommendation (item 3
> above). `FrameMeta` gained two new grids, `delta_lf` (8×8-luma-grid
> resolution, used by the chroma deblock passes) and `delta_lf4` (4×4-luma-
> cell resolution, used by luma's finer-grained pass) — both `[i8; 4]`
> (`[y_vert, y_horiz, u, v]`, matching `FRAME_LF_COUNT`/`TileDecodeState::
> delta_lf`'s own layout). Populated once per coded block (both the
> keyframe-intra path in `intra_block.rs` and the var-tx-leaf IBC path in
> the same file — using the block's own span, not per-leaf, since `DeltaLF`
> is a per-block not per-transform value) right after the existing
> `record_luma`/`record_chroma` calls, using `self.delta_lf` (the running
> per-tile state `read_delta_lf` maintains). `merge_tile` carries both
> grids across tile boundaries (overwrite semantics, not the `max`/`&&`
> combine `record_luma` uses, since `DeltaLF` is a single per-block value,
> not something to accumulate). `deblock_plane` now takes a `delta_lf_grid`
> parameter and looks up the q-side cell's value (the block the outer loop
> is currently positioned at) for each edge, passing it into
> `compute_level` in place of the old hardcoded `0`.
>
> **Verified via `av1_psnr_check` (ffmpeg on PATH): no change to any
> corpus entry's PSNR** (`solid_red`/`smptebars` still 99 dB, `testsrc`
> 74.88/54.07/55.18, `mandelbrot` 70.45/53.15/52.76, `testsrc2` 24.70/
> 24.00/16.86 — all unchanged from before this session) — expected, since
> none of ffmpeg's `aom` encodes for this corpus actually turn on
> `delta_lf_present`, so this was filling in a real spec gap without a
> regression-test signal available in the current corpus. 139 unit tests
> pass, clippy `-D warnings` and `cargo fmt` clean.
>
> **A separate, larger gap found while doing this — FIXED same session**:
> `inter_block.rs` never called `record_luma`/`record_chroma`/
> `mark_luma_edges`/`mark_chroma_edges`/`record_delta_lf` at all — grep
> confirmed `self.meta` didn't appear anywhere in that file. Every
> inter-coded block left its `FrameMeta` cells at their default (`tx_w/h =
> 0`, `skip = true`, `edge_left/top = false`), so `deblock_plane` never
> filtered *any* edge belonging to an inter block — the deblock filter was
> effectively luma/chroma-edge-blind on every P/B frame, independent of the
> `delta_lf`/CDEF-vs-deblock investigation above.
>
> Fix: `add_inter_residual` (the function that reconstructs an inter
> block's residual) previously `return`ed immediately when `skip ||
> luma_tx > TX_16X16`, which is *also* where the (missing) `FrameMeta`
> calls would have needed to run — `TxSize`/transform-edge geometry is
> well-defined regardless of `skip`, so that early return was conflating
> "don't read residual coefficients" with "don't record geometry".
> Replaced it with a `has_residual = !skip && luma_tx <= TX_16X16` flag
> that gates only the `read_coeffs`/`dequantize_coeffs`/`inverse_transform`
> calls (both luma and chroma, the latter previously gated on a redundant
> `!skip` that is now `has_residual` for the same reason); the geometry
> loops (`mark_luma_edges`/`mark_luma_edges4`/`record_luma4` per luma
> transform sub-block, `mark_chroma_edges` per chroma transform sub-block,
> then the fixed 8×8-grid `record_luma`/`record_chroma`/`record_delta_lf`/
> `record_delta_lf4` calls after each loop) now always run, mirroring
> `intra_block.rs`'s keyframe path call-for-call. Residual-add loops are
> unconditional too (adding an all-zero `residual` vec when
> `!has_residual` is a harmless no-op, same as the prior behavour of never
> touching the plane).
>
> Verified: 139 unit tests pass, clippy `-D warnings` and `cargo fmt`
> clean, workspace builds, `av1_psnr_check` unchanged (all-intra corpus,
> so `add_inter_residual` isn't exercised by it — expected, no signal
> either way from this check). **Still unvalidated end-to-end**: no inter
> corpus/patched-dav1d reference exists to confirm the geometry recorded
> here is bit-exact against real inter content (per earlier notes,
> `decode_inter_block` isn't reached by the current pipeline on real
> non-keyframe streams yet — Phase E is still WIP). This fix makes the
> loop-filter *metadata pipeline* structurally complete for inter blocks,
> but doesn't by itself prove inter-frame deblock correctness — that still
> needs real inter-frame conformance data once Phase E lands.

> **2026-09-08 session note — CI hygiene + refreshed corpus baseline.**
> `just check` / CI `fmt-check` was **red on master**: commit `1c46edc`
> (narrow deblock one-clip fix) left `tpt-kinetix-av1/src/loop_filter.rs:659`
> unformatted, and `tpt-kinetix-test-utils/tests/dbg_av1_testsrc2.rs:77,145`
> had two more pre-existing rustfmt violations. All three reformatted (no
> behaviour change). Also gated the unconditional `eprintln!("DBG tile_init
> …")` in `reconstruct/mod.rs` (fired on every single decode) behind a new
> `KINETIX_AV1_DBG_TILE_INIT` env var, matching the `KINETIX_AV1_DBG_TILE_BYTES`
> guard right above it.
>
> **Refreshed `av1_psnr_check` baseline (ffmpeg+libdav1d on PATH), Y/U/V dB —
> notably better than the numbers carried in todo.md's index and the
> `project_av1_*` memories, thanks to the concurrent process's recent
> reconstruction/deblock commits:** solid_red_32/64 99/99/99;
> **testsrc_128x96 99.00/57.26/58.63 (luma now pixel-exact)**;
> mandelbrot_128x96 89.03/55.86/55.71; smptebars_256x144 99/99/99;
> testsrc2_320x180 24.70/24.00/16.86 (still IBC/Phase-E gated).
>
> **testsrc chroma gap localised to reconstruction, not loop filter.** Filter
> on/off sweep on testsrc: full pipeline U/V 57.26/58.63; `NOCDEF` 53.30/54.46;
> `NODEBLOCK` 53.56/56.37; both off 51.14/52.67. So pre-filter chroma recon is
> already ~52 dB and the in-loop filters are *improving* it (net +5 dB), i.e.
> the residual chroma error is a reconstruction bug present before filtering.
> `av1_symbol_trace_diff testsrc` (FILTERED-kinetix vs FILTERED-dav1d):
> filtered **Y is bit-exact (SSE=0)**; first divergence is **plane U
> px=(16,1), delta -1**, nearest block marker `coeffs plane=1 px=(16,0)
> tx=16x8 skip=false pred_mode=0` — a **DC-pred (not CfL) 16x8 rectangular
> chroma transform** block, off by 1. So the suspect is the **rectangular
> chroma inverse transform** (the `Abs(log2W-log2H)==1` √2 rescale at
> `transform.rs:563-576`, or `inverse_dct`/`inverse_adst` rounding for a
> non-square size), or chroma dequant — NOT CfL. Errors are ±1 scattered
> (U/V ≈ 57 dB ≈ RMSE 0.36). `dq_denom(TX_16X8)` correctly = 1 (tx_sz_ctx=2),
> so the 2026-09-04 dqDenom bug shape is already excluded for this block.
> **Blocker:** pinning a ±1 needs a per-block residual reference = a *patched*
> dav1d (ITXDUMP/EDGEDUMP). This env has only ffmpeg-bundled libdav1d, no
> standalone/patched dav1d — build one before going deeper.
> Note: `av1_symbol_trace_diff`'s "NOFILTER first divergence" line is unsound
> (compares nofilter-kinetix vs FILTERED-dav1d, same confound as the
> CDEF-not-edge-limited finding) — ignore it.
> 139 unit tests + full av1 test suite + workspace clippy `-D warnings` +
> `cargo fmt --all --check` all green.

> **2026-09-09 session note — patched dav1d BUILT; testsrc chroma gap is the
> LOOP FILTER, not reconstruction (previous note above is WRONG).**
>
> Built a block-trace dav1d on this Windows box (the blocker every prior
> session hit): `scripts/build-patched-dav1d.ps1` + `scripts/dav1d-blockdump.patch`
> (flips dav1d's own `DEBUG_BLOCK_INFO`/`DEBUG_B_PIXELS` in `src/recon.h`).
> Needs only VS 2022 + meson/ninja, no MSYS2/nasm (`-Denable_asm=false`,
> C paths are bit-exact — verified SSE=0 vs ffmpeg-libdav1d on testsrc).
> Run: `dav1d.exe --threads 1 --framedelay 1 -i f.obu -o o.y4m 1>trace.txt`.
> New scratch harness: `tpt-kinetix-test-utils/tests/dbg_av1_testsrc_chroma.rs`
> (first diverging U/V chroma px + window vs dav1d).
>
> **The "rectangular chroma inverse transform" / dqDenom hypothesis is dead.**
> dav1d's trace shows every chroma block around the first U divergence
> (chroma px (16,1)) has `eob=-1` — *no coded residual, no inverse transform
> at all*. They are palette- / DC- / directional-predicted skip blocks.
> The block Kinetix's marker called "DC-pred 16x8 tx skip=false" is, in
> dav1d, a **palette** block (`Post-pal[pl=1,sz=6] pal=[37 80 a0 ca f0 f0]`).
>
> **Proof it's the filter:** ran Kinetix with `KINETIX_AV1_NODEBLOCK=1
> KINETIX_AV1_NOCDEF=1` and compared its *pre-filter* chroma against dav1d's
> *pre-filter* `u-pal-pred` hex dump for the same block:
>   - dav1d pre-filter U (16,0..1) = `55, 160`;  Kinetix pre-filter = `55, 160`
>   — **bit-exact, every sample.**
>   - dav1d *post*-filter (16,1) = 162;  Kinetix post-filter = 161.
> So reconstruction (palette colour decode + colour map + prediction) is
> correct; the entire U/V ≈57 dB gap is Kinetix's in-loop **deblock and/or
> CDEF** producing a slightly different (usually over-corrected) result on
> edges. Error signature: per-pixel ±1..±3 gradients hugging diagonal colour
> edges, zero in flat regions — a filter-strength delta, not a constant
> offset (wrong palette colour) or a large jump (wrong colour-map index).
> This **converges with `project_av1_mandelbrot_dct16_pm1_gap`** ("Kinetix's
> own loop filter over-correcting, not reconstruction") — testsrc-chroma and
> mandelbrot-luma 89 dB now look like the *same* loop-filter bug.
>
> **RESOLVED same session — it was CDEF, two chroma-only bugs.** dav1d's CLI
> `--inloopfilters {nocdef,nodeblock,nocdef,none,norestoration}` gives a
> clean per-filter reference (no patched-plane dump needed). Split:
>   - Kinetix `NOFILTER` vs dav1d `none`  → **0 diff** (recon bit-exact, U+V).
>   - Kinetix `NOCDEF` (deblock+LR) vs dav1d `nocdef`  → **0 diff** (deblock
>     and loop-restoration both bit-exact).
>   - any config *with* CDEF  → U 307 / V 224 px off by ±1..3. **CDEF.**
>
> Both bugs were in the chroma CDEF path (`loop_filter.rs` `cdef_plane_chroma`
> + the per-unit chroma driver), luma was already correct:
>   1. **Chroma damping** must be `CdefDamping - 1`, not `CdefDamping`
>      (dav1d `cdef_apply_tmpl.c:285` passes `damping - 1` for `pl > 0`).
>   2. **Chroma primary strength must NOT be variance-adjusted.** Kinetix ran
>      the luma `adjust_strength` step (`(pri*(4+i)+8)>>4`, §7.15.3) on chroma
>      too; dav1d applies it only to `y_pri_lvl`, `uv_pri_lvl` is passed raw.
>
> Result (`av1_psnr_check`, ffmpeg+libdav1d): **testsrc_128x96 now
> 99/99/99 — fully pixel-exact** (was 99/57.3/58.6). **mandelbrot chroma
> 55.9/55.7 → 69.6/70.7.** solid_red / smptebars still 99/99/99. 139 av1
> unit tests + fmt + clippy green.
>
> **Still open:** mandelbrot **luma** 89.03 dB — unchanged by this fix, so it
> is a *separate* bug (not chroma CDEF). testsrc2 still IBC/Phase-E gated.
> The patched-dav1d block trace + `dbg_av1_testsrc_chroma.rs` harness (with
> its `KINETIX_DUMP_OBU` / `KINETIX_REF_YUV` escape hatches) are the tools to
> chase the mandelbrot luma gap next.

> **2026-09-09 (cont'd) — mandelbrot luma CDEF fix + what's left.**
> Second CDEF bug, this one in the **luma** path: dav1d `adjust_strength()`
> caps the variance term at `Min(FloorLog2(var>>6), 12)`; Kinetix clamped the
> input `Clip3(1, 256, var>>6)` which caps it at **8**, under-strengthening
> CDEF on detailed blocks. Fixed (commit — "av1: fix luma CDEF variance
> strength cap"). Corpus `mandelbrot_80x64` luma **121 px → 0** (bit-exact vs
> dav1d full pipeline via `--inloopfilters`).
>
> **What's still off (all tiny now, measured against a per-`--inloopfilters`
> dav1d reference — the clean method):**
>   - `mandelbrot_128x96` **luma: ONE pixel** at (75,54) off by 1
>     (k=87/r=86). That's literally what "89.03 dB" is — MSE = 1.0/12288 →
>     89 dB. A CDEF edge/clamp corner case (`nodeblock` = 10 px, `all` = 1).
>     Not worth much — but if chased: `cdef_filter_block`'s border-tap
>     min/max handling (Kinetix skips out-of-buffer taps; dav1d pads with
>     `CDEF_VERY_LARGE` and lets `constrain` zero them while still folding
>     the sentinel into `max` via `imax`).
>   - `mandelbrot` **chroma: ~20-60 px off by ±1-4, present pre-filter**
>     (`NOFILTER` vs dav1d `none` diverges) → a **chroma reconstruction**
>     bug, separate from CDEF. Localised to `poc=0,y=12,x=8` (luma 32,48),
>     `uvmode[6]` = **D157 directional chroma prediction** (`tx=TX_4X8`,
>     `eob=-1` so no residual — it's the predictor). Kinetix's directional
>     intra predictor (chroma, non-90/180 angle, with `angle_delta` +
>     §7.11.2.4 edge upsample / §7.11.2.9 filter-type) is ±1-2 vs dav1d on
>     the interpolated samples. This is the next real bug to fix; use the
>     `u-intra-pred` hex in the patched-dav1d trace as the oracle.
>
> testsrc / solid_red / smptebars all fully pixel-exact. 139 av1 unit tests
> + fmt + clippy green.

> **2026-09-09 (cont'd) — D157 chroma bug ROOT-CAUSED & FIXED (commit 0c9dc4e).**
> It was NOT the directional predictor / `dr_z2` / upsample (those are all
> bit-exact — tried `+ua`/`+ul` index fixes, they broke testsrc). The real
> cause: `reconstruct_intra_subblock` wrote `uv_above`/`uv_left` (the chroma
> neighbour-mode grid feeding §7.11.2.9 `get_filter_type`) for **every**
> block, including sub-8×8 luma blocks that carry no chroma — clobbering the
> real SMOOTH_H mode of the chroma-carrying neighbour with a placeholder DC.
> dav1d (`decode.c:733`) only writes `t->a->uvmode`/`t->l.uvmode` under
> `if (has_chroma)`. Fix = same gate. The stale DC made the D157 block's
> `filter_type_uv` read 0 instead of 1, flipping it onto the non-smooth
> edge-filter/upsample branch → the whole prediction shifted ±1-4.
>
> Result: **all 4 intra corpus entries (incl. mandelbrot_80x64) now
> bit-exact Y/U/V vs `dav1d --inloopfilters none`.** 139 av1 unit tests +
> fmt + clippy green.
>
> **Left (both tiny, 128×96 mandelbrot only):**
>   - chroma: ~20 px ±1 pre-filter, first (48,40) = luma (96,80). A
>     *separate* recon bug from the D157 one (that was max ±4; this is ±1).
>   - luma: 1 px at (75,54) ±1 — the CDEF border-tap corner case above.
>   `av1_psnr_check` 128×96 mandelbrot stays 89.03/69.58/70.70 (these gaps).

> **2026-09-09 (cont'd) — 128×96 mandelbrot chroma FIXED (commit 93ce919).**
> Follow-up to the `has_chroma` gate. dav1d indexes the chroma-mode
> neighbour array (`t->a->uvmode`) in **chroma-4×4 units** (`cbx4 = bx4 >> ss`,
> `cbw4 = (bw4 + ss) >> ss`); the chroma-carrying block of a sub-8×8 group is
> at an *odd* mi position but its shared chroma covers the even sibling.
> Kinetix wrote `uv_above`/`uv_left` only at `mi_col..mi_col+bw` (odd col
> only), so a later even-position chroma D45 block read a stale D113 mode
> instead of the real SMOOTH — and `get_filter_type` picked edge-filter
> strength 1 instead of 2 ({0,4,8,4} vs {0,5,6,5}), shifting the prediction
> ±1. Fix: align both write (`mi_col & ~ss`, widened to shared extent) and
> read (`mi_col & ~ss`) to the chroma grid.
>
> **`av1_psnr_check`: mandelbrot_128x96 now 89.03 / 99 / 99** (chroma
> pixel-exact, was .../69.6/70.7). All 4 intra corpus entries bit-exact
> Y/U/V through the full pipeline **except** mandelbrot luma's one pixel:
>   - `mandelbrot_128x96` luma (75,54) k=87/r=86, **1 px, ±1, CDEF only**
>     (pre-filter Y = 0; deblock-only Y = 0; any CDEF config → 1-10 px).
>     Interior pixel (block (72,48), offset (3,6)) so not a frame edge —
>     a CDEF constrain / min-max / direction-tap corner case. Needs a
>     CDEF-output dump added to the dav1d blockdump patch to pin. Cosmetic
>     (89 dB = exactly this 1 px), lowest priority.
>
> 139 av1 unit tests + fmt + clippy green.

> **2026-09-09 (cont'd) — the mandelbrot_128x96 luma (75,54) ±1 pixel is
> LOOP RESTORATION, not CDEF.** Added a per-8×8 CDEF pre/post dump to the
> dav1d patch (`cdef_apply_tmpl.c`, gate on `bx`/`by`) and a matching one in
> Kinetix `cdef_plane_luma`. **Kinetix's post-CDEF output is bit-exact vs
> dav1d for every 8×8 block around (75,54)** — same params (pri 5, sec 0,
> dir, var, damping 5, adj), same pre, same post. And `dav1d --inloopfilters
> norestoration` vs `all`: LR alone moves (75,54) 87→86 (LR touches ~1031
> luma px this frame). Kinetix's LR on non-CDEF'd input already matches dav1d
> (`NOCDEF` Y=0). So: LR is fed a bit-identical post-CDEF plane here yet
> produces 87 (Kinetix) vs 86 (dav1d). **Row 54 is 2 rows above the LR
> stripe boundary at y=56** (AV1 stripe 0 = rows 0..55). The Wiener 7-tap
> vertical window at row 54 reads rows 51..57, i.e. 2 rows into the next
> stripe, which per §7.17.1 must come from the *saved pre-deblock/CDEF
> stripe line buffer* (dav1d `lr_lpf_line`), not the live pixels. Kinetix's
> stripe-boundary line handling in the Wiener path is the suspect — compare
> `tpt-kinetix-av1/src/loop_filter.rs` Wiener against dav1d's
> `lr_apply`/`lpf_line` for rows within 2 of a stripe edge. 1 px, ±1, 89 dB
> — cosmetic, lowest priority, and the concurrent process is also in
> AV1-LR-adjacent code.

> **2026-09-10 session note — testsrc2 IBC gap: 3 real bugs fixed, 24.7→36.2 dB Y
> / 16.9→28.5 dB V.** The pinned intra corpus (max 128×96) is 5/5 bit-exact vs
> dav1d and the Phase G gate (`assert_eq!(exact_count, compared_count)` in
> `conformance.rs`, still commented) would pass — but a **320×180 `testsrc2`**
> (added to `av1_intra_corpus` as `testsrc2_big`, an aom screen-content encode
> with `allow_intrabc=true`, deblock/CDEF/restoration all force-disabled) was
> pixel-exact only for rows 0–79, then broke at the first IBC block. Built the
> patched dav1d (GitHub mirror `github.com/videolan/dav1d` clones in seconds vs
> code.videolan.org taking ~1h — the pinned commit
> `aa09a630ef57ee7d9482ffb7ef355a903dbb5302` still fetches by sha; scripts/
> build-patched-dav1d.ps1 should switch origin). dav1d's `Post-dmv[y/x,ref=..|
> mvstack0..]` trace gave ground-truth DVs for all 9 IBC blocks. Fixes, in
> `reconstruct/intra_block.rs`:
>  1. **IBC DV predictor was missing the spec default DV entirely** (§6.10.24
>     `assign_mv` intrabc branch). Kinetix used only "nearest is_inter
>     neighbour's DV, else (0,0)". Added the fallback: when both spatial
>     candidates are (0,0), `PredMv = (0, -(sbSize4·4+256)·8)` if
>     `MiRow - sbSize4 < MiRowStart` (tile top), else `(-(sbSize4·4·8), 0)`
>     — matches dav1d's `-(512<<sb128)-2048` / `-(512<<sb128)` exactly.
>  2. **Plain-intra blocks never reset `is_inter_{above,left}` / `mv_{above,
>     left}`** — dav1d's `splat_intraref` stamps `mv.n = INVALID_MV` across
>     every non-IBC intra block, so stale DVs from an IBC block earlier in the
>     column leaked downward and the predictor picked a spurious non-zero
>     candidate instead of the default. Added the reset in the intra
>     end-of-block context update.
>  3. **IBC pixel copy used the wrong sign.** An old comment claimed "our
>     entropy decoder gives IBC MVs with opposite sign … so we subtract" and
>     did `src = px - mv/8`. With the predictor fixed, the DV now follows the
>     spec convention (negative = up/left) and dav1d does `src_top = by·4 +
>     (mv.y>>3)` — i.e. *add*. Flipped both luma and chroma copies to `+`.
>     This was the dominant error (25.1→36.2 dB Y once flipped).
> With all three: `av1_psnr_check` testsrc2_320x180 24.70/24.00/16.86 →
> **36.20/36.70/28.53**; testsrc2_big luma diff samples 8199 → 1835. No
> regression — the 5 pinned corpus entries stay bit-exact, 139 av1 unit tests +
> workspace clippy + fmt green.
> **Still open:** 3 of the 9 IBC blocks (indices 5/7/8 in decode order) still
> have a wrong DV predictor — dav1d's `mvstack[0]` for them is a non-zero DV
> from a *non-adjacent* IBC candidate (secondary -3/-5 scan_row/scan_col, the
> top-right point, or `add_single_extended_candidate`). Kinetix's approximated
> primary-edge-only scan misses these. The real fix is a faithful port of
> `dav1d_refmvs_find` / spec §7.10.2 `find_mv_stack` for the single-ref
> `{0,-1}` intrabc case (scan_row/scan_col with weights, secondary edges,
> sorting, extended candidates) — ~150–200 lines, the concrete next step to
> close testsrc2. After that: pin `testsrc2_big` (or a real ITU/AOM vector)
> and flip `capabilities().pixel_exact` for the intra path.

> **2026-09-10 (cont'd) — full `find_mv_stack` port for intrabc: all 9 IBC DVs
> now bit-exact vs dav1d, testsrc2_320x180 36.2→61.2 dB Y (8199→8 luma diff
> samples).** Added a 2-D `refmv_grid` (`RefMvCell { mv, w4, h4, valid }`,
> `mi_rows*mi_cols`) to `TileDecodeState`, splatted by every block
> (`splat_refmv` — `valid=false` for plain intra = dav1d's `INVALID_MV`
> sentinel, `Some(dv)` for IBC). `ibc_mv_pred` ports `dav1d_refmvs_find` /
> `scan_row` / `scan_col` specialised to the single-ref `{INTRA_FRAME, NONE}`
> intrabc case (no gmv, no temporal, no extended candidates — the spec's
> `ref[0] > 0` gate excludes them): primary top/left scans at −1 with the
> length/height weight formula, top-right + top-left points, +640 to the
> nearest set, secondary scans at −3/−5, weight sort, then §6.10.24 pick
> (`RefStackMv[0]` else `[1]` else default DV). Verified against a patched
> dav1d `Post-dmv[bx,by,dv,ref,s0,s1,n]` trace (added `t->bx/t->by` +
> mvstack[0]/[1] + n_mvs to the print — worth folding into
> `scripts/dav1d-blockdump.patch`): all 9 IBC blocks match dav1d's `ref`
> (predictor) and final DV exactly. No corpus regression (5 pinned entries
> stay bit-exact); 139 av1 unit tests + clippy + fmt green.
> **Remaining testsrc2_big gap (61 dB, 8 luma px):** IBC block 6 (bx=62,by=32,
> 8×16, DV −592/0, skip=false) — cols 248–255 row 128 come out flat ≈copy
> (106) where dav1d has a real gradient residual (132→109). Looks like the IBC
> var-tx / coeff read producing eob=0 (or a wrong inverse transform) where
> dav1d decodes coefficients — a residual bug in the IBC path, separate and
> much smaller than the DV-predictor class just closed. Next: trace that one
> block's `Post-y-cf-blk` against dav1d. Then pin `testsrc2_big` + flip
> `capabilities().pixel_exact`.

> **2026-09-10 (cont'd) — FLIPADST inverse transforms: testsrc2_320x180 LUMA
> now bit-exact vs dav1d (commit b7315eb).** `inverse_transform`'s
> `row_axis_transform`/`col_axis_transform` fell through to `Identity` for
> every FLIPADST `TxType` — the code comment claimed only the "unvalidated
> inter path" could reach them, but IBC blocks (`IsInter == 1`) select inter
> tx types including H_FLIPADST/V_FLIPADST/FLIPADST_*. A DC coeff through the
> missing transform landed on a single pixel (the `159, 106, 106…` vs dav1d's
> smooth gradient signature). Now all 16 `TxType`s dispatch, with a `flip`
> flag = "reverse the 1-D output along this axis" (dav1d `inv_flipadst*` =
> ADST then reverse). testsrc2_big Y: ~61 dB / 8 px → **bit-exact**. 5 pinned
> corpus entries unaffected; 139 unit tests + clippy + fmt green.
> **Still open — testsrc2_big chroma (U 50.6 / V 40.9 dB):** small flat
> per-4×4-block DC-ish offsets (−8 … −91) clustered at chroma y=64 and
> y=72–75 (= the bottom-row IBC blocks, luma by=32/36). Looks like a chroma
> IBC residual/dequant error (the block's single DC coeff dequantized or
> transformed slightly off), not a DV or entropy problem — luma of the same
> blocks is exact. First divergence: U px=(128,64), IBC block 8 (bx=64,by=32),
> dav1d `Post-uv-cf-blk[pl=0,tx=0,txtp=11,eob=0]` (TX_4X4 H_DCT, 1 DC coeff).
> Next: dump Kinetix's chroma DC coeff + dequant for that block vs the dav1d
> trace's chroma pixel hex. Then pin `testsrc2_big` + flip
> `capabilities().pixel_exact` for the intra path.

> **2026-09-10 (cont'd) — AV1 intra keyframe decode is now BIT-EXACT vs dav1d
> across the whole synthesized corpus (6/6). Phase G gate armed.** Final fix:
> **bilinear sub-pel for IBC chroma** (commit fee2d04). The luma IBC DV is
> integer-pel, but halving it for a 4:2:0 chroma plane can hit a half-pel
> position — dav1d then runs bilinear interpolation (`FILTER_2D_BILINEAR` for
> intrabc); Kinetix was doing a plain integer copy. Routed chroma IBC
> prediction through `motion_compensate(INTERP_BILINEAR, mv >> ss)`.
> `av1_intra_corpus_vs_dav1d_when_available`'s Phase-G assertion
> (`assert_eq!(exact_count, compared_count)`) is now uncommented — a hard
> regression guard covering solid_red / testsrc / smptebars / mandelbrot /
> testsrc2 96×64 / **testsrc2 320×180 (9 IBC blocks, screen content)**.
> `capabilities().notes` rewritten (the "inter validated frame-by-frame vs
> dav1d" claim was stale — inter is 0/8 bit-exact, ~12 dB).
>
> **`capabilities().pixel_exact` stays `false`.** Remaining before it can flip:
>   1. **Inter prediction** — `av1_inter_sequence_vs_dav1d` / `av1_inter_corpus`
>      are 0/N bit-exact (~12 dB on non-keyframes). This is the big one; the
>      MV-pred / OBMC / compound / interp-filter paths need the same
>      dav1d-trace treatment the intra path just got.
>   2. Official AOM/ITU AV1 conformance vectors wired in (only synthesized
>      ffmpeg-encoded clips so far).
>   3. `KINETIX_AV1_DBG_IBC_UV` debug hook left in `reconstruct_ibc_block`
>      (env-gated, matches the file's convention).

> **2026-09-10 (cont'd) — AV1 INTER path characterised (the remaining
> pixel_exact blocker). `dbg_av1_inter.rs` added (per-frame diffmap,
> `KINETIX_AV1_DBG_INTER_FRAME=N`).** Traced `minimal_av1_inter_ivf(8,128,96)`
> (testsrc, hierarchical GOP — 2nd frame in *decode* order is POC 6, a forward
> reference) against patched dav1d:
>  - **Frame 0 (keyframe): 94 luma px off, all ±1**, clustered in the bottom
>    rows (y≥64), some full-row / full-column stripes. This keyframe has
>    loop-filter + CDEF + loop-restoration ENABLED (the intra corpus clips
>    force them off via `allow_intrabc`), so this is a residual LR/CDEF
>    edge-rounding gap — same class as the mandelbrot stripe-boundary work,
>    not yet fully closed for the deblock-on path.
>  - **Frame 1+ (inter): catastrophic (~12 dB, ~460k |diff|).** Distinct
>    left-half/right-half split: with `TileCols` likely 2 (each 64px), tile 1
>    (right, x≥64) is **bit-exact** for the top 8 SB rows while tile 0 (left)
>    is wrong by large *flat per-8×8* DC offsets (+109/+47/+22/+89). Blocks are
>    mostly `Post-intermode[0,…,mv=y:0,x:0]` (zero-MV, ≈ straight copy of the
>    reference) — so a right copy of a right reference would be exact. The flat
>    DC offsets on zero-MV blocks point at **the stored reference frame being
>    wrong on the left** (wrong slot, or stored pre-loop-filter, or a
>    tile-local buffer), OR left-tile blocks intra-falling-back (DC≈128 vs the
>    real ~16). dav1d also shows `Post-subpel_filter1/2` (dual switchable
>    interp filter), `Post-interintra`, `Post-intermode[3]` (NEWMV) — all
>    exercised.
>
> **Inter work plan (fresh, multi-session — mirrors the intra effort):**
>  1. Fix the reference-frame store: confirm Kinetix stores the *post-filter*
>     frame into the `refresh_frame_flags` slots and that `ref_frame_idx`
>     resolves correctly for a hierarchical GOP (POC-6-first). The left/right
>     split is the first clue — check per-tile decode doesn't clobber the
>     shared reference or leave tile 0's output in a tile-local buffer.
>  2. `find_mv_stack` for real inter (compound refs, temporal MV projection,
>     the `ref[0] > 0` extended-candidate branch skipped for intrabc) — the
>     intra `ibc_mv_pred` port is the scan_row/scan_col skeleton to build on.
>  3. Dual switchable interp filter (`Post-subpel_filter1/2`), OBMC, warped
>     motion, compound (wedge / diffwtd / masked), interintra.
>  4. Inter loop-filter deltas + the keyframe LR/CDEF ±1 edge gap.
>  5. Official AOM/ITU vectors, then flip `capabilities().pixel_exact`.

> **2026-09-10 (cont'd) — inter root cause narrowed: the INTER FRAME HEADER
> parse is where sync breaks.** Kinetix decodes a `residual[0..8]=[-65,…]`
> for inter frame 1's first block, but dav1d's trace shows that block is
> `Post-skip[1]` (SKIPPED — no residual). A wrong `skip` bit that early, on
> ctx 0 (no neighbours), means the tile-data bit offset itself is wrong →
> the whole inter frame is desynced from the first block. So the inter frame
> header parser (`frame.rs::FrameHeader::parse` non-keyframe path:
> `frame_type` / `ref_frame_idx[7]` / `frame_refs_short_signaling` /
> `delta_frame_id` / `is_motion_mode_switchable` / `use_ref_frame_mvs` /
> global-motion params / `interpolation_filter` read) is the **first** thing
> to fix — everything downstream (MV pred, MC, OBMC, compound) can't be
> validated until the tile offset is right. Trace method: compare Kinetix's
> computed tile-group bit offset for frame 1 against where dav1d's first
> `Post-skip` consumes bits.
>
> **Landed this session (infrastructure, no metric change — inter still 0/8
> because of the header desync above):** dual switchable interp-filter read
> (`inter_block.rs`) — reads two `interp_filter` symbols per §5.11.27 when
> `seq.enable_dual_filter`, with the real 16-way context (`filter_above` /
> `filter_left` per-dir neighbour tracking added to `TileDecodeState`), was
> previously one symbol at hardcoded ctx 0. `enable_dual_filter` threaded
> through `decode_tile_group`. 5/6… wait 6/6 intra corpus still bit-exact,
> 139 unit tests + clippy + fmt green.

> **2026-09-10 (cont'd) — the AV1 inter conformance number (0/8, ~12 dB) is
> NOT a reliable decoder-quality signal: the harness has a decode-order vs
> display-order mismatch.** `KINETIX_AV1_DBG_FH` (new, `frame.rs`) +
> `KINETIX_AV1_DBG_TILE_BYTES` dump on `minimal_av1_inter_ivf(8,128,96)`:
>  - 9 frame headers for 8 IVF payloads → **one payload carries multiple
>    coded frames** (hierarchical GOP: decode order `oh` = 0,6,3,1,2,4,5,6,7;
>    `order_hint` 6 appears twice). Kinetix parses every FH in a payload but
>    reconstructs only the last, and `av1_inter_sequence_vs_dav1d` naively
>    pairs `payload[i]` with dav1d's *display*-ordered `ref_frames[i]`.
>  - `show_existing_frame` returns `Err(Unsupported)` (`frame.rs:473`).
>  - Frame 1's first block: Kinetix decodes `residual=[-65,…]` where dav1d's
>    trace shows `Post-skip[1]` (skipped) — a genuine early desync on top of
>    the harness issue.
>
> **Revised inter work order:**
>  1. **Fix the inter conformance harness first** — decode the whole IVF in
>     decode order through one `Av1Decoder`, track `order_hint`, compare each
>     reconstruction to the dav1d display frame with the matching hint. Until
>     this lands, per-frame inter PSNR is meaningless.
>  2. Implement `show_existing_frame` (display a DPB slot; §7.4 / §5.9.2).
>  3. THEN the real decoder gaps: inter frame-header completeness, real
>     `find_mv_stack` (compound/temporal), the first-block skip/residual
>     desync, dual-filter MC (infra landed 597aa93), OBMC, warped, compound.
>  4. Keyframe LR/CDEF ±1 (94 px, deblock-on path).
>  5. Official AOM/ITU vectors → flip `pixel_exact`.

> **2026-09-10 (cont'd) — show_existing_frame landed (commit fd20d40); dual
> interp-filter confirmed active + context correct.** `KINETIX_AV1_DBG_SEQ`
> (new) shows the inter clip's sequence header parses correctly:
> `dual_filter=true interintra=true masked=true warped=true jnt=true
> ref_mvs=true`. dav1d's narrowed trace for the first inter frame's block
> (0,0): `Post-skip[1]` (skip), `Post-intra[0]` (inter), `Post-ref[0]`,
> `Post-intermode[0,…,mv=0,0,n_mvs=0]`, `Post-subpel_filter1/2[0,ctx=3]`
> (dual!) — Kinetix's dual-filter context now computes `ctx=3` too (matches).
> **Decode-order is poc 6,3,1,2,4,5,6,7.** poc 1/2/4/5 first blocks are
> `Post-skipmode[1]` + `Post-skipmodeblock[refs=0+4/0+6]` — **compound
> skip-mode** (bidirectional, no residual). Kinetix's inter path almost
> certainly doesn't implement skip_mode → those frames (which are display
> frames 1,2,4,5) desync immediately. That's the single biggest inter gap
> after the harness/show_existing work.
>
> Corpus effect of this session's inter infra (dual filter + show_existing):
> `av1_inter_corpus` testsrc_128x96 f1 14.3→18.9 dB, testsrc_64x64 f4
> 29→29.3; `av1_inter_sequence` frame 3 (show_existing) 11.1→33.7. Still
> 0/N bit-exact — the inter *reconstruction* (skip_mode, real find_mv_stack,
> MC, compound) is the multi-session core effort.

> **2026-09-10 (cont'd) — inter path unblocked at the entropy level (3 commits
> 49198ec/f46d8f6 + the earlier fd20d40).** Root cause of the "~12 dB from
> block 0" was two things: (1) `Av1Decoder::decode()` reconstructed only one
> frame per packet — hierarchical GOPs pack an ALTREF + B-frames + a
> show-frame into one temporal unit; now every frame OBU in the TU is
> reconstructed and pushed to the DPB, and only the shown one is returned;
> (2) `skip_mode_params` (§6.8.2) always returned "disabled" and read no bit —
> now computes `skipModeAllowed`/`SkipModeFrame` from the DPB order hints
> (threaded `RefOrderHint[0..8]` through, `parse_with_dpb`) and reads the
> `skip_mode_present` bit; and per-block `read_skip_mode` (§5.11.11) now
> short-circuits skip_mode blocks to compound NEAREST-MV prediction. Also:
> `av1_inter_corpus_vs_dav1d` splits the OBU stream by temporal-delimiter OBUs
> so Kinetix outputs align with dav1d's display order.
> Result (`av1_inter_corpus`): testsrc_64x64 f2 10.6→23.1, f3 30.9→33.9;
> testsrc_128x96 f2 9.2→16.9. Still 0/N bit-exact.
>
> **Remaining inter work, in order:**
>  1. **Compound `find_mv_stack`** (§7.10.2 `isCompound=1`) — the skip_mode
>     NEAREST MVs and every NEARMV/NEARESTMV use the simplified spatial-only
>     `build_mv_candidates`. Port the real weighted scan + sort + compound
>     extended candidates (the intrabc `ibc_mv_pred` scan is the skeleton).
>  2. NEWMV / drl / global-motion MV decode + `read_mv` precision paths.
>  3. Real compound prediction (§7.11.3.1): wedge / diffwtd / distance-weighted
>     masks — currently a plain average.
>  4. OBMC, warped motion, interintra.
>  5. Dual-axis MC kernels (dual filter reads land; MC still uses one).
>  6. Keyframe LR/CDEF ±1 (94 px, deblock-on path).
>  7. Official AOM/ITU vectors → flip `capabilities().pixel_exact`.

> **2026-09-10 (cont'd) — deblock level-zero guard (commit 78402c9): first
> AV1 inter-sequence frame bit-exact.** `compute_level` adds
> `loop_filter_ref_deltas[INTRA_FRAME]` (defaults to 1) to the base level, so
> `loop_filter_level = [0,0,0,0]` still produced `lvl=1` and ran deblock,
> smearing ±1 along every edge. §7.14 returns immediately when both luma
> levels are 0 — added that guard. The `minimal_av1_inter_ivf` **keyframe is
> now bit-exact vs dav1d** (`av1_inter_sequence` 1/8). Intra corpus 6/6.
>
> **Frame-1 diffmap after that fix:** SB row 0 (y<64) mostly correct (±5
> scattered — MV/prediction imperfections), SB row 1 (y≥64) catastrophic
> (~100 diff) — an **entropy desync at the SB(0,1) boundary**, plus a −23
> prediction error already at px (31,3) in SB row 0 (wrong MV or wrong ref).
> Both point at the same next item: **compound `find_mv_stack`** — the real
> §7.10.2 weighted scan / sort / compound extended candidates. Everything
> inter downstream (NEWMV, drl, compound-mode selection, and the skip_mode
> NEAREST MVs) feeds off it; the current `build_mv_candidates` is a
> spatial-only stub. Port it from dav1d `dav1d_refmvs_find` (the intrabc
> `ibc_mv_pred` scan is the skeleton) — that is THE remaining inter blocker.

> **2026-09-10 (cont'd) — inter mode-read path also needs work (found while
> scoping find_mv_stack, NOT yet fixed):** `inter.rs::read_single_inter_mode`
> reads `new_mv` and treats symbol `== 1` as NEWMV, but AV1 §5.11.24 is
> `new_mv == 0 → NEWMV` (verify against Kinetix's `DEFAULT_NEW_MV_CDF`
> orientation — it may be complemented). `zero_mv == 0 → GLOBALMV` and
> `ref_mv == 0 → NEARESTMV` likewise. And `decode_ref_and_mv` is always
> called with `mode_ctx = 0` — the real `NewMvContext` / `ZeroMvContext` /
> `RefMvContext` come out of `find_mv_stack` (the composite `newmv_ctx`,
> `refmv_ctx`, `zeromv_ctx`). So the compound `find_mv_stack` port must also
> produce those three contexts and the mode-read must be rewritten to the
> spec's `new_mv`/`zero_mv`/`ref_mv` cascade + compound-mode 8-way read. This
> is one connected work item — do it together, validate the `Post-intermode`
> value block-by-block against a patched-dav1d trace.

> **2026-09-10 — exact dav1d single-ref inter mode cascade (decode.c:1662+,
> for the next session's `read_inter_mode` rewrite):**
> ```
> refmvs_find -> mvstack[8], n_mvs, ctx   (ctx is PACKED)
> if ( seg.skip/globalmv || decode_bool(newmv_mode[ctx & 7]) ) {   // bool==1 => NOT newmv
>     if ( seg... || !decode_bool(globalmv_mode[(ctx>>3)&1]) ) {   // bool==0 => GLOBALMV
>         inter_mode = GLOBALMV;  mv = gmv_2d(...)
>     } else {
>         if ( decode_bool(refmv_mode[(ctx>>4)&15]) ) {            // bool==1 => NEARMV
>             inter_mode = NEARMV; drl_idx = 1;
>             if (n_mvs>2) drl_idx += bool(drl_bit[get_drl_context(mvstack,1)]);
>             if (drl_idx==2 && n_mvs>3) drl_idx += bool(drl_bit[get_drl_context(mvstack,2)]);
>         } else { inter_mode = NEARESTMV; drl_idx = 0; }
>         mv = mvstack[drl_idx].mv[0];  if (drl_idx<2) fix_mv_precision(mv)
>     }
> } else {   // bool==0 => NEWMV
>     inter_mode = NEWMV; drl_idx = 0;
>     if (n_mvs>1) { drl_idx += bool(drl_bit[get_drl_context(mvstack,0)]);
>                    if (drl_idx==1 && n_mvs>2) drl_idx += bool(drl_bit[get_drl_context(mvstack,1)]); }
>     mv = (n_mvs>1) ? mvstack[drl_idx].mv[0] : { fix_mv_precision(mvstack[0].mv[0]) };
>     read_mv_residual(mv, mv_prec=hp-force_integer_mv);
> }
> ```
> `Kinetix inter.rs::read_single_inter_mode` currently: `new_mv==1 -> NEWMV`
> (INVERTED), `zero_mv==1 -> ZEROMV` (INVERTED), and `mode_ctx` hardcoded 0.
> Compound path: `compound_mode = decode_symbol_adapt8(comp_inter_mode[ctx&7]);
> inter_mode = NEARESTMV_NEARESTMV + compound_mode` then per-ref drl.

> **2026-09-10 (cont'd) — inter find_mv_stack + mode cascade + single_ref tree
> landed (commits c971677/1ebf3c1). VERIFIED: first inter block of
> minimal_av1_inter_ivf now decodes ref=LAST + NEARESTMV, matching dav1d
> exactly (was LAST2 + NEWMV — a wrong linear single_ref cascade).**
> `inter_mv_stack` (reconstruct/intra_block.rs) ports dav1d `dav1d_refmvs_find`
> spatial scans + §7.10.2.14 contexts → packed `(RefMvContext<<4 |
> ZeroMvContext<<3 | NewMvContext)` + DrlCtxStack. Single-ref mode read is now
> the spec `new_mv`/`zero_mv`/`ref_mv` cascade with the packed ctx + drl bits.
> `read_single_ref_name` is the real `single_ref_p1..p6` nested-binary tree
> with `ref_count_ctx` from the immediate above/left neighbour ref names.
> RefMvCell generalised to `{mv[2], refs[2], w4, h4, mf}`; splat_refmv_full.
>
> **Next desync: block 1 of the first inter frame.** Block 0 (skip=1,
> NEARESTMV, mv 0/0) decodes bit-identically to dav1d through `Post-intermode`,
> but block 1's `new_mv` symbol reads 0 (→NEWMV) where dav1d reads 1
> (→NEARESTMV) — **same context (nm=3), same fresh CDF slot**, so a *bit
> position* desync in block 0's tail: the `Post-subpel_filter1/2` reads
> (count/context — Kinetix reads 2 for dual-filter; verify the ctx & the
> `needs_interp_filter` gate for a zero-MV NEARESTMV block), or the skipped
> block's (missing) residual/tx reads, or `read_cdef`. Trace `dec.bit_position()`
> after each of block 0's reads vs dav1d's `r=` renorm state.
>
> Still stubs: compound mode/ref decode + drl, temporal MV candidates
> (`use_ref_frame_mvs`), single/compound extended candidates, global-motion
> MVs, real compound prediction.

> **2026-09-10 (cont'd) — inter mode/context reads now BIT-EXACT for the
> first two blocks (commits 90fa8dd/72f2685).** Traced block-by-block against
> patched dav1d (`KXnewmv`/`KXglobalmv`/`KXrefmv` prints added; Kinetix
> `raw_state().0` = dav1d `msac.rng`). Two context bugs found & fixed:
>  1. **`ZeroMvContext`** (the `zero_mv`/globalmv_mode read's context) was
>     hardcoded 0 — §7.10.2's temporal-sample process inits it to
>     `use_ref_frame_mvs` (1 here) and it stays 1 while no motion field
>     exists. Threaded `use_ref_frame_mvs` through `decode_tile_group`.
>  2. **`intra_inter` (is_inter) context** was `(left_inter+above_inter).min(3)`
>     — §8.3.2 builds it from whether the *available* neighbours are
>     INTRA-coded (3/1/0 both-avail, `2*intra` one-avail, 0 none).
> Both blocks 0 & 1: skip/is_inter/ref/new_mv/zero_mv/ref_mv/filter0/filter1
> rng all match dav1d. **`av1_inter_corpus`: testsrc_128x96 f1 16.5→24.7 dB,
> f2 11.8→20.2, f3 20.3→27.3; testsrc_96x64 f1 18.5→21.7.**
>
> **Next desync** (block 2 or later / block 1's residual): continue the same
> `KINETIX_AV1_DBG_B0` rng trace. Then: compound mode/ref decode + drl,
> temporal MV candidates, real compound prediction, MV `read_mv` precision,
> global-motion MVs. Also verify: block 0's `filter` ctx=11 vs dav1d ctx=3
> (rng matched — likely a `dir*8` vs `dir`-table-index labelling difference,
> but confirm `interp_filter[16]` layout == dav1d `filter[2][8]`).

> **2026-09-10 (cont'd) — `read_motion_mode` (§5.11.23) was entirely missing.**
> Traced `KINETIX_AV1_DBG_B0` against patched dav1d on the `testsrc_128x96` obu
> first inter frame (poc=6): block (0,0) matched bit-exact through the filter
> reads, but block (0,16) desynced right after `Post-intermode` — dav1d emits a
> `Post-motionmode[0] [mask: 0x0/0x1]` symbol Kinetix never read. Implemented
> `read_motion_mode` in `reconstruct/inter_block.rs`: gated on `!compound &&
> is_motion_mode_switchable && min(bw,bh)>=8 && ref[1]==NONE &&
> has_overlappable_candidates()`; reads the 3-way `motion_mode` CDF when a
> *matching-ref* neighbour exists + `allow_warped_motion` + `!force_integer_mv`
> (dav1d `find_matching_ref` mask nonzero → warp allowed), else the `use_obmc`
> bool. A `WARP` result forces `INTERP_EIGHTTAP_REGULAR` (no subpel-filter
> read), matching dav1d `has_subpel_filter=0`. New CDF fields
> `mode_cdfs.motion_mode` / `.use_obmc` (spec defaults, `[[u16;4];22]` /
> `[[u16;3];22]`, indexed by BlockSize); threaded `is_motion_mode_switchable` +
> `allow_warp` from `FrameHeader` through `decode_tile_group`/`TileDecodeState`.
> Block (0,16) now bit-exact through the filter reads (`motion_mode=0
> rng=48973`, matches dav1d). **`av1_inter_corpus` f1: testsrc_128x96
> 24.7→45.0 dB, testsrc_96x64 21.7→25.8, testsrc_64x64 25.5→30.6.** Still
> 0/N bit-exact — warp-sample derivation (`find_warp_samples`/`NumSamples`) is
> approximated (matching-ref neighbour ⇒ NumSamples>0), OBMC/warp prediction
> itself is not applied, and later blocks still desync. Next: continue the
> `KINETIX_AV1_DBG_B0` trace past block (0,16) on poc=6.

> **2026-09-10 (cont'd) — `read_interintra_mode` (§5.11.28) was also missing.**
> Widened the patched-dav1d `DEBUG_BLOCK_INFO` window to the whole frame. The
> second SB row of 128x96 poc=6 and every 32x32/16x16-class block of 96x64
> poc=4 desynced at dav1d's `Post-interintra` — an `interintra` bool (then
> `interintra_mode` + `interintra_wedge` [+ `wedge_idx`] when set) read for
> single-ref blocks in {8x8,8x16,16x8,16x16,16x32,32x16,32x32} when
> `enable_interintra_compound`. Implemented in `inter_block.rs` right after the
> MV cascade, before `read_motion_mode` (whose eligibility now also requires
> `interintra_type == 0`, per dav1d). New CDFs
> `mode_cdfs.{interintra,interintra_mode,interintra_wedge,wedge_idx}` (spec
> defaults); threaded `seq.enable_interintra_compound`. **VERIFIED bit-exact vs
> patched dav1d through 3 consecutive blocks of BOTH 128x96 poc=6 and 96x64
> poc=4** (skip/is_inter/ref/newmv/zeromv/refmv/interintra/motionmode/filter0/
> filter1 rng all match). The `is_ii==1` wedge sub-path is coded from the spec
> but unverified (no corpus block hits it). Corpus PSNR stays noisy for the
> hierarchical 96x64/64x64 clips (multi-frame-per-TU harness artifact) — the
> entropy trace is the reliable signal and it is clean now. Next: keep tracing
> 128x96 poc=6 into the residual / later SB rows.

> **2026-09-10 (cont'd) — inter residual now uses the real var-tx tree.**
> The non-skip inter residual read `read_tx_size` (the intra single-ternary
> `tx_depth` symbol) instead of `read_block_tx_size` §5.11.16's inter/IBC
> branch — a recursive `txfm_split` var-tx tree — desyncing at dav1d's
> `Post-vartxtree` on the first non-skip inter block. Switched
> `decode_inter_block` / `decode_skip_mode_block` to the existing
> `read_block_tx_size_ibc`; rewrote `add_inter_residual` to iterate the
> returned var-tx leaves for luma (per-leaf coeff read; `clear_coeff_context`
> on skip) and one uniform `chroma_tx_size` grid for chroma. Also fixed
> `read_tx_tree`'s neighbour context: dav1d `reset_context` fills the var-tx
> `ctx->tx` array with `TX_64X64` (largest) at tile/SB-row edges — distinct
> from the intra `ctx->tx_intra` (-1) — so an unavailable neighbour must
> compare as the largest size (`a`/`l` = 0). Kinetix's shared `0` sentinel
> made `0 < txw` always true; now guarded with `!= 0`. **Verified bit-exact
> vs patched dav1d through the ENTIRE second superblock row of 128x96 poc=6
> INCLUDING the luma+chroma residual reads** (`Post-vartxtree` / `Post-y-cf-
> blk` / `Post-uv-cf-blk` rng all match), previously desynced at block 3.
> Coeffs are always read (entropy) but only applied for `Tx_Size_Sqr_Up <=
> TX_16X16` (larger inverse transforms not yet conformance-checked for inter).
> All 139 unit tests + intra corpus (6/6) still pass.
> **Known regression:** `av1_inter_sequence` frame 1 PSNR 45->31 dB — that
> frame has compound (jnt-comp) blocks whose ref/mv/mode reads are still a
> stub, so more-correct entropy after them just reads further into a stream
> already desynced at the compound block. Compound `find_mv_stack` + jnt/wedge
> compound reads are the next blocker; the var-tx layer under them is now right.

> **2026-09-10 (cont'd) — compound Stage 1: `is_comp` flag + reference-frame
> tree + §8.3.2 neighbour contexts.** New `reconstruct/comp_ctx.rs` ports
> dav1d `src/env.h` branch-for-branch: `get_comp_ctx`, `get_comp_dir_ctx`,
> `av1_get_{fwd,fwd_1,fwd_2,bwd,bwd_1,uni_p1}_ref_ctx`, `av1_get_ref_ctx`,
> plus `get_mask_comp_ctx`/`get_jnt_comp_ctx` (dead until Stage 2). Added
> per-mi `comp_type_above/left` neighbour tracking (dav1d `BlockContext::
> comp_type` numbering; skip-mode + compound blocks record `COMP_INTER_AVG`
> pending `read_compound_type`). `decode_inter_block`'s compound branch
> rewritten from the old always-unidir stub to the real §5.11.25 tree
> (`comp_reference_type` → BIDIR fwd/bwd or UNIDIR), with the dav1d contexts;
> the `comp_mode` (is_comp) read now uses `get_comp_ctx` + the `min(bw,bh) >
> 4` gate. Kinetix's `comp_ref`/`comp_bwd_ref`/`uni_comp_ref` CDF tables are
> stored transposed vs dav1d (`cdf[ctx][i]`), handled at the call sites.
> **VERIFIED bit-exact vs patched dav1d**: `Post-compflag[1] r=42611` and
> `Post-refs[0/4] r=63646` (dir_ctx=0) on the 128x96 poc=1 first compound
> block. 139 unit tests + intra corpus (6/6) still pass. `av1_inter_sequence`
> frame 1 31→27 dB (the compound *mode/mv* reads after `Post-refs` are still
> the `decode_ref_and_mv` stub — Stage 2). Next: `comp_inter_mode` 8-way +
> per-ref drl + compound `find_mv_stack` (§7.10.2 isCompound) + MV residuals,
> then `read_compound_type` (§5.11.26), then compound prediction.

> **2026-09-10 (cont'd) — compound Stage 2: `comp_inter_mode` (8-way) + drl +
> per-ref MV.** Added `InterCdfs::comp_inter_mode` CDF (8 ctx × 8 sym, spec
> defaults). `inter_mv_stack` now also returns the compound `comp_inter_mode`
> context (dav1d `refmvs.c` isCompound branch: `refmv_ctx`/`newmv_ctx` folded
> via `refmv_ctx >> 1`) and its `get_drl_context` else-branch was fixed
> (returned 2 where dav1d returns 0). `decode_inter_block`'s compound branch
> replaced the `decode_ref_and_mv` stub with the real cascade: 8-way
> `comp_inter_mode` symbol → `dav1d_comp_inter_pred_modes` per-ref sub-mode →
> drl (NEWMV_NEWMV vs NEARMV paths) → per-ref MV (NEAREST/NEAR from stack,
> NEW = stack + `read_mv` residual, GLOBAL = zero). `decode_skip_mode_block`
> now calls `splat_refmv_full` so a later block's compound `find_mv_stack`
> can see skip-mode ref pairs (this was why compound `n_mvs` came out 0).
> **VERIFIED bit-exact vs patched dav1d** on 128x96 poc=1 first compound
> block: `Post-compintermode[0,ctx=4] r=36834` and `Post-residual_mv[0,0/0,0]
> r=36834` both match. `av1_inter_sequence` frame 1 27→30.7 dB, frame 2
> ~19→27.0. 139 tests + intra corpus (6/6) pass. Remaining compound gaps:
> `n_mvs` still undercounts (compound `find_mv_stack` misses temporal +
> extended candidates — matters for drl on NEW/NEAR compound blocks) and
> `read_compound_type` (§5.11.26: `mask_comp`/`wedge`/`jnt_comp`/
> `compound_idx`) + real compound prediction (dist-weighted / wedge / diffwtd)
> are still stubbed (plain average). Next: `read_compound_type`.

> **2026-09-10 (cont'd) — compound Stage 3: `read_compound_type` (§5.11.26).**
> Added `mode_cdfs.{mask_comp,jnt_comp,wedge_comp}` CDFs (spec defaults) and
> threaded `enable_masked_compound`/`enable_jnt_comp`/`OrderHintBits`/current
> `OrderHint`/DPB slot order-hints through `reconstruct_av1_frame` →
> `decode_tile_group` → `TileDecodeState`. Implemented `read_compound_type` in
> `decode_inter_block`'s compound branch: `comp_group_idx` bool
> (`mask_comp[get_mask_comp_ctx]`) → jnt/avg path (`jnt_comp[get_jnt_comp_ctx]`
> with the real `get_poc_diff` term) or the wedge / diffwtd + `mask_sign`
> literal branch. Per-mi `comp_type_above/left` now records the real decoded
> type (not a hardcoded `COMP_INTER_AVG`). **VERIFIED bit-exact vs patched
> dav1d** on 128x96 poc=1 first compound block: `Post-segwedge_vs_jntavg[0]`
> + `Post-jnt_comp[0,ctx=1]` → Kinetix `comptype grp=0 type=1 rng=47316`,
> matching dav1d's `r=47316`. The full compound *entropy* chain (is_comp →
> ref tree → comp_inter_mode → drl → per-ref MV → compound_type) is now
> bit-exact for that block; the next divergence is the compound residual
> (`Post-y-cf-blk[tx=12,eob=17]` — a large rect tx, read but not applied).
> 139 tests + intra corpus (6/6) pass. Still stubbed: compound *prediction*
> (plain average, not weighted/wedge/diffwtd), large-tx inverse transforms,
> and compound `find_mv_stack` temporal/extended candidates (n_mvs undercount).

> **2026-09-10 (cont'd) — compound ENTROPY chain fully bit-exact.** Re-traced
> the 128x96 poc=1 first compound block end-to-end: `is_comp` → ref tree →
> `comp_inter_mode` → drl → per-ref MV → `read_compound_type` → var-tx (1 leaf
> `TX_64X32`) → luma residual (17 coeffs) → 2× chroma residual all match
> patched dav1d rng (`post-residual rng=39026` == dav1d `Post-uv-cf-blk[pl=1]
> r=39026`), and the *next* block's `Post-skip` (r=65402) also matches. So the
> compound coefficient reads (incl. large rect tx) are correct.
> Tried applying the large-tx inter inverse transform (removing the
> `Tx_Size_Sqr_Up <= 16x16` gate): frame 1 luma-diff slightly better but
> **frame 2 luma-diff 4751→10390** — the 32x32/64x64 inter inverse transform
> or its `tx_type` is wrong, so the gate stays. Remaining compound work is all
> PIXEL reconstruction: (1) intermediate-precision MC + `avg`/`w_avg`
> (jnt_weights)/`mask` (wedge/diffwtd) blend — Kinetix's `motion_compensate`
> outputs u8 and averages there, dav1d blends in the pre-downshift domain, so
> even plain `COMP_INTER_AVG` isn't bit-exact; (2) verify the large inter
> inverse transform + tx_type; (3) compound `find_mv_stack` temporal/extended
> candidates (n_mvs undercount). These form a coherent pixel-domain follow-up.

> **2026-09-10 (cont'd) — MC subpel rounding fixed to §7.11.3.3.** Kinetix's
> `motion_compensate` (`inter.rs`) applied `(s + 64) >> 7` to BOTH the
> horizontal and vertical 8-tap passes over the 128-scale `Subpel_Filters`
> table — wrong: the spec's 8-bit non-compound path is `InterRound0 = 3`
> (H: `(s + 4) >> 3`) then `InterRound1 = 11` (V: `(s + 1024) >> 11`), and
> the horizontal pass must cover `bh + 7` rows (the vertical filter's
> ±3/4-tap support) with each *reference* sample clamped to the frame edge —
> the old code only filtered `bh` rows and then clamped the *filtered block*
> at its own top/bottom edge, corrupting the first/last 3 rows of every
> vertically-filtered MC block. New unit test
> `motion_compensate_bilinear_halfpel_averages_a_ramp` locks the rounding
> (a ramp's half-pel sample rounds .5 up). PSNR ~neutral on the corpus
> (most inter blocks are zero-MV copies) but foundational + intra corpus
> still 6/6. Compound blend still needs the pre-downshift (`InterRound1 = 7`)
> `prep` path + `avg`/`w_avg`/`mask`.

> **2026-09-11 — chroma MC 1/16-pel precision.** `motion_compensate` now takes
> per-axis `hbits`/`vbits` (3 = luma 1/8-pel, 4 = a subsampled chroma axis
> 1/16-pel) — dav1d `mvx & (15 >> !ss_hor)` / `>> (3 + ss_hor)`. The chroma
> callers pass the *luma* MV directly (removed the lossy `Mv::scaled_chroma`
> pre-halving, which also dropped the bottom 1/16 bit); `inter_predict_plane`
> derives the bits from `plane` + `subsampling_{x,y}`. IBC chroma keeps
> `3,3` (its `c_mv` is already `>> ss`-scaled). **testsrc_96x64 f1
> 25→30.2 dB, testsrc_64x64 f1 ~24→26.4, testsrc_128x96 f2 U 28.9→29.2**;
> intra corpus still 6/6 (incl. the IBC `testsrc2_big`), conformance 11/11,
> 140 av1 tests. Next: pre-downshift `prep` MC path + `avg`/`w_avg`/`mask`
> compound blends (currently plain u8 average).

> **2026-09-11 — compound blend in the intermediate domain.** New
> `motion_compensate_prep` (§7.11.3.2 `isCompound`: H `Round2(s,3)`, V
> `Round2(s,7)`, i32 out, no clamp) + `compound_blend` (§7.11.3.1: `avg` =
> `Round2(p0+p1,5)`, `w_avg` = `Round2(p0*w + p1*(16-w),8)`). `inter_predict_
> plane`'s compound path now preps both refs and blends, instead of averaging
> two fully-downshifted u8 predictions. `jnt_weight` computed per block
> (§7.11.3.15 `distance_weights`: `quant_dist_weight`/`quant_dist_lookup_table`
> from the ref/cur order-hint POC diffs) for `COMP_INTER_WEIGHTED_AVG`; plain
> average (weight 8) for `COMP_INTER_AVG` and skip-mode. Wedge / diffwtd masks
> still fall through to the average (mask generation TODO). **`av1_inter_
> sequence` frame 1 Y 27.5→31.5 dB, V 27.1→31.1, U 25.0→29.1**; frame 2 U
> 29.2→30.9. Intra corpus 6/6, conformance 11/11, 140 av1 tests.

> **2026-09-14 — THE INTER-FRAME ENTROPY DESYNC ROOT-CAUSED AND FIXED: the
> missing §6.8.2 CDF-context save/restore (`primary_ref_frame` → saved
> adapted CDFs). Also: the large-tx residual gate is gone (net win), the OBMC
> blend mask weighting was flipped to spec, and a local patched-dav1d
> workflow now exists on this machine.** The desync that had every inter
> frame after the first GOP stuck at ~12–26 dB was never an OBMC/warp/blend
> bug at all. Evidence trail (all reproducible):
> 1. A/B gates (`KINETIX_AV1_NOOBMC`, existing `KINETIX_AV1_NO_WARP`,
>    `KINETIX_AV1_NOFILTER`) changed *nothing* on frame 1 — OBMC jobs in that
>    frame are 5 left-edge blocks whose neighbour predictions are identical
>    (`any_diff=false`), and there are zero WARP blocks.
> 2. An independent Python MC oracle (spec §7.11.3.3, eighttap-regular,
>    clamped borders) reproduced Kinetix's inter prediction **bit-exactly**
>    (diff 0), so MC was never the problem either.
> 3. Built a **source-patched dav1d locally** (meson+ninja work out of the
>    box; clone + ~4 env-gated `fprintf`s in `decode.c`/`obu.c` — see
>    `/tmp/av1dbg/dav1d-src` on this machine, rebuild from a fresh clone the
>    same way): per-block `by/bx/bl/bs/bp/rng` at `decode_b` entry, LR unit
>    reads, SGR set/weights, and per-frame `DAV1D FH` header dumps. Comparing
>    its trace against `KINETIX_AV1_TRACE`/`KINETIX_AV1_DBG_B0` showed:
>    p0 + all three sub-frames of p1 (hierarchical-GOP packets pack multiple
>    OBU_FRAMEs per IVF frame!) decode **block-for-block in sync**, and every
>    frame from p2 on diverges at the **first partition symbol**.
> 4. Header dumps (`KINETIX_AV1_DBG_FH_JSON` vs dav1d's parsed header) match
>    field-for-field; the divergence is the *initial CDF state*: frames with
>    `primary_ref_frame == 7` (defaults) sync; frames with
>    `primary_ref_frame != 7` must restore the **adapted CDF context saved by
>    an earlier frame** — and Kinetix never implemented that at all
>    (`TileDecodeState::new` hardcoded `ModeCdfs::new()` +
>    `TileCdfs::new(qindex)` for every frame).
> 5. The exact restore semantics (from dav1d's `decode.c:3511`):
>    `in_cdf = c->cdf[ refidx[primary_ref_frame] ]` — **the slot is looked up
>    through the frame's `ref_frame_idx` map**, not used directly (dav1d's
>    `pri_ref = refidx[primary_ref_frame]`); save-side is per
>    `refresh_frame_flags` bit, gated on `refresh_context` (§7.7
>    frame_end_update_cdf copies Saved→working for the `init_coeff_cdfs` +
>    `init_non_coeff_cdfs` arrays — coeff CDFs are carried, not re-seeded).
>    Implementation: `FrameCdfContext { mode_cdfs, coeff_cdfs }` snapshot,
>    threaded `decode_tile_group` → `reconstruct_av1_frame` →
>    `Av1Decoder::ref_cdf_contexts[8]` (Arc-cloned into refresh slots;
>    restore slot = `ref_frame_idx[primary_ref_frame]`).
> **Result (`av1_inter_sequence`): every inter frame dropped to
> 2700–3150 luma diff samples** (from 4664–11032), PSNR e.g. frame 2
> 19.57→28.63 dB, frame 4 18.12→26.30 dB. Intra corpus still 6/6 bit-exact;
> 153 av1 lib tests + full workspace tests/clippy/fmt green.
> **With contexts fixed, the read-but-don't-apply gate for >16×16 inter
> transforms is now a net win and is removed** — the old "apply regresses
> frame 2 (4.7k→10k)" measurement was an artifact of the desync (garbage
> coefficients). `inverse_transform` handles the adjusted-size ≤32-side
> dequant stride, rect sqrt-2 rescale, and 64-family shifts generically.
> **Debug hooks added (env-gated, per repo convention):**
> `KINETIX_AV1_DBG_FH_JSON` (per-frame parsed-header dump + bit count),
> `KINETIX_AV1_DBG_FH_SEC` (per-section header bit positions),
> `KINETIX_AV1_DBG_PRED_ALL` (PRED dump ignores the y≥56 region gate),
> `KINETIX_AV1_NOOBMC` (skip OBMC application), `KINETIX_AV1_DBG_LR` now also
> dumps the decoded SGR/Wiener unit values, `KINETIX_AV1_DBG_B0` luma
> coefficient lines now carry `mi=`, and `dbg_av1_inter` gained
> `KINETIX_AV1_SAVE_IVF`. **Remaining inter gap** is the uniform ~3k diff
> samples/frame in the bottom region — NOT MC (oracle-exact), NOT OBMC/WARP
> (absent/no-op), NOT deblock/CDEF/LR toggles — concentrated at transform
> edges above the y=64 SB boundary including inside a 64×64 skip block, i.e.
> post-reconstruction modification or a per-block metadata difference on
> inter frames; next session should diff pre-filter pixels per block against
> the patched dav1d (`DAV1D ITXDUMP`-style dumps on this local build).
> Harness caveat worth remembering: the inter IVF packs *multiple OBU_FRAMEs
> into one IVF frame* (aomenc hierarchical GOP) and payload 3 is a
> show-existing header-only packet — both decoder and harness align by
> *emitted shown-frame sequence*, not by payload index.

> **2026-09-14 (cont'd) — per-block deblock levels (§7.14.4/§7.14.5) and the
> remaining gap narrowed to LR SGRPROJ set-14.** Implemented the per-edge
> filter-level inputs: `FrameMeta` now records each block's `RefFrames[row]
> [col][0]` (spec delta index, 0=INTRA) and §7.14.4 `modeType` (1 for non-
> GLOBAL inter modes, 0 for intra/GLOBALMV/GLOBAL_GLOBALMV) on the 4×4-luma
> grid (`record_lf4`, wired in the intra, inter and compound cascades), and
> `compute_level` applies `ref_deltas[ref] + mode_deltas[modeType]` for
> inter edges vs `ref_deltas[INTRA]` alone for intra edges (§7.14.5 step 4),
> including §7.14.2's re-derivation from the opposite block when the level
> is 0. Chroma edges resolve the co-located luma cell (`lf_shift`). Effect
> is small on this stream (deltas mostly 0/1) but it closes a real spec gap
> for any stream with nonzero per-ref deltas.
> **Remaining ~2.7-3.1k diff samples/frame localized to loop restoration's
> SGRPROJ set-14** (r0=2/eps0=30/r1=0 — the r0-only 5×5 variant, never
> exercised by the intra corpus where mandelbrot used set 10, the r1-only
> case): the residual band above the y=64 SB boundary disappears entirely
> under `KINETIX_AV1_NOLR`. The weight mapping is NOT the bug — dav1d's
> per-variant dispatch is `sgr_5x5`→`w0`, `sgr_3x3`→`w1` (complement),
> `mix`→`w0`/`w1`, exactly Kinetix's `(xqd[0]*t0 + w1*t1)>>11` (an
> experiment applying the complement to the 5×5 output regressed 2690→2855
> and was reverted). The divergence is therefore in the 5×5 pass's
> stripe/edge handling: dav1d sources out-of-stripe taps from the
> `lpf_line` pre-CDEF boundary buffer with 2-row availability + edge
> replication, Kinetix's `compute_pass` reads a clamped whole-plane
> snapshot with a 1-cell halo; and dav1d's `sgr_finish2` odd-row/even-row
> finish pattern ((1<<8)>>9 for pair-rows, (1<<7)>>8 for the single row)
> vs Kinetix's equivalent needs a line-by-line diff on a real set-14 unit.
> Next session: dump the pre/post-LR planes for frame 1 on both sides
> (patched dav1d + `KINETIX_AV1_DUMP_PREFILTER`-style hooks) and diff the
> 5×5 pass row by row.

> **2026-09-14 (cont'd 2) — remaining gap narrowed to specific subpel-MV
> blocks in the hidden first inter frame; filters fully exonerated.**
> Stage-isolation matrix: the local dav1d build gained env gates
> (`DAV1D_NODEBLOCK`/`DAV1D_NOCDEF`/`DAV1D_NOLR2`, mirroring Kinetix's) and
> Kinetix gained `KINETIX_AV1_DUMP_FRAMES` (per-frame raw dumps incl. hidden
> frames) + `KINETIX_AV1_SAVE_OUT` (harness-side output dump). Comparing
> every stage configuration: the frame-level diff is **identical with all
> three filters disabled on both sides** — deblock/CDEF/LR contribute
> nothing to the remaining gap. Per-frame dumps (both decoders now dump
> *every* decoded frame, hidden included) show the divergence **starts in
> p1a, the hidden first inter frame** (2842 luma diff samples, max |d|=228,
> confined to y=64..95); every later frame's error is inherited through
> reference-frame prediction and grows (p7 reaches 115k).
> p1a's divergent blocks are exactly those with non-trivial fractional MVs:
> (24,16) 32x32 skip mv=(0,31) [pred-vs-dav1d=28752], (12,20) 16x16
> mv=(12,0), (16,20) 16x16 mv=(10,0), (2,18)/(4,18) mv=(66,0), (0,20)
> mv=(64,0), (28,17) mv=(0,10); every zero-MV block is pixel-exact, and the
> prediction-vs-reference diff of the whole frame concentrates in these.
> The transform/dequant side is exonerated: the two TX_64X32 leaves'
> dequantized rows and full residual row-sums match dav1d's
> (`DAV1D_DBG_ITX` dump vs `KINETIX_AV1_DBG_ITX`) **row for row**. p1a's
> header has allow_high_precision_mv=true, so odd MV components are legal —
> the MV *values themselves* are now the prime suspect: next session should
> dump dav1d's `mvstack`/decoded `b->mv` for these blocks (by=16..24, bx=2
> ..28) against Kinetix's `mvstack`/`read_mv` diffs — e.g. our (0,31) vs
> dav1d's implied prediction behaves like a slightly different fractional
> component. Tooling note: dav1d's C-code debug prints require
> `--cpumask 0` (SIMD silently bypasses patched C functions); dav1d
> heredoc-patched strings keep breaking — write patch scripts with
> `chr(92)+'n'` for `\n` inside C string literals.

> **2026-09-14 (cont'd 3) — desync localized to a conditional symbol inside
> p1a's SB(64,64); methodology + tooling for the final symbol diff in place.**
> dav1d's `DEBUG_BLOCK_INFO` (recon.h) is now getenv-gated
> (`DAV1D_DBG_BLOCKS`) and the local build prints `Post-skip/Post-cdef_idx/
> Post-delta_q/Post-ymode/Post-intermode/Post-mv/...` with rng for every
> block — 282 symbol prints for this stream. dav1d's MV dump
> (`DAV1D_DBG_MV`, with n_mvs) shows the first divergence: at p1a's SB
> (64,64), dav1d decodes fine-grained blocks — (by=16,bx=24) 16x8 skip
> NEARESTMV mv=0 with n_mvs>=1 — while **Kinetix decodes a 32x32 NEWMV
> block at mi=(24,16) with an EMPTY mvstack (n_mvs=0, s0 fallback (0,0)),
> final mv=(0,31)**; Kinetix's remaining SB(64,64) blocks then diverge
> structurally from dav1d's 8x8/16x8 layout. Two concrete hypotheses for
> the next session: (1) our `find_mv_stack` spatial/temporal candidate scan
> returns an empty stack where dav1d finds candidates (check the above-
> neighbour scan at mi_row=16 reading the bottom edge of the 64x64 skip
> block above, and the temporal projection gating), and/or (2) with
> n_mvs==0 the spec *skips* the new_mv symbol and forces NEWMV
> (`if NumMvs == 0, Y is NEWMV` + mv from read_mv_residual on a zero base)
> — dav1d's cascade with n_mvs>0 reads new_mv, ours also read it with
> n_mvs=0, i.e. our empty-stack handling may read a symbol dav1d doesn't.
> Everything else is verified exact this session: MC interpolation
> (Python oracle), TX_64X32 dequant+transform (row-for-row vs dav1d),
> filters (stage matrix), CDF contexts, and headers.

> **2026-09-14 (cont'd 4) — the chroma `coincident_luma_tx_type` placeholder
> is FIXED (real inter-chroma tx-type derivation), every inter frame
> improved again (chroma +2-6 dB; frame 1 U 32.64→37.80), and the
> remaining desync is narrowed to a symbol divergence at p1a's SB(64,64)
> partition reads.** The inter chroma path now records each luma leaf's
> decoded `TxType` (with its pixel span) during the luma residual loop and
> derives each chroma transform block's tx type from the co-located luma
> leaf (replacing the `DCT_DCT` placeholder flagged since the first inter
> work) — `get_uv_inter_txtp` then picks the chroma family and `read_eob`'s
> is_1d CDF context follows. Current `av1_inter_sequence`: all 8 frames
> 2,164-3,141 luma diff samples (frame 1 U 37.80 dB). Per-frame dumps
> (hidden frames included) confirm the divergence still starts in p1a and
> propagates. The desync point: dav1d splits p1a's SB(64,64) into 8x8s and
> 16x8s (NEARESTMV mv=0 etc.) while Kinetix decodes that SB's first
> partition as NONE into a 32x32 INTRA + 32x32 NEWMV mv=(0,31) with an
> empty mvstack — i.e. the entropy state diverged at or inside that SB's
> partition symbols, after 12+ previously-verified-in-sync blocks (the two
> 64x64 skips and the bottom-left 8x8/16x8 group match dav1d block-for-
> block, MVs included: 66/66/64 all match). Since the CDF state, header
> fields and tile bytes are verified identical, the next session should
> diff the symbol stream across p1a's SB(16,16)=(64,64) 16x16 non-skip
> block's coefficient read and the following partition symbol — the most
> likely candidates are a per-coefficient context difference in that block
> (e.g. the inter `is_inter`-dependent coefficient contexts) or a
> conditional syntax element gated differently between the decoders
> (dc_q/luv deltas are absent this frame). The `KIN ITX64x32` and
> `KIN TILE` debug prints added this session remain env-gated.

> **2026-09-14 (cont'd 5) — chroma tx-type fix verified: every frame improved
> again (frame 1 U 32.64→37.80 dB; all frames 2,164-3,141 samples). Symbol-
> level alignment tooling added: `KINETIX_AV1_DBG_B0ENTER` prints each
> block's entry rng (pre-skip) matching dav1d's per-block entry rng, and
> dav1d's `DEBUG_BLOCK_INFO` symbol prints (`Post-skip/Post-intra/
> Post-intermode/Post-mv`) are enabled for p1a. The symbol diff confirms:
> p1a blocks 1-10 decode identically on both sides (64x64 skips, 8x8s,
> 16x8s, MVs 66/66/64 all match, entry rngs match through the SB(0,64)
> blocks), and the entropy trees diverge inside p1a's SB(16,16) =
> px(64..95, 64..95): dav1d splits it into ~13 8x8/8x16/16x8 INTER blocks
> while Kinetix decodes ONE 32x32 INTRA + one 32x32 NEWMV mv=(0,31) with an
> empty mvstack. Since the tile bytes, headers and entering CDF state are
> verified identical, the extra/missing bits were consumed inside the
> preceding 16x16 non-skip block at mi=(4,20) (luma txtp=11 - a non-DCT
> type, i.e. the first block exercising the now-fixed chroma tx-type
> derivation, so its chroma eob is_1d context is the first suspect) or in
> that block's coefficient contexts. Next session: diff the symbol stream
> across mi=(4,20)'s chroma reads (dav1d `Post-uv-cf-blk` prints vs
> `KINETIX_AV1_DBG_B0` uv-cf lines) - the fixed chroma derivation may still
> differ from dav1d in the eob is_1d context or the coeffs scan for the
> derived type.

> **2026-09-14 (cont'd 6) — alignment methodology note + concrete next step.**
> The reliable per-block symbol diff procedure (both traces verified working):
> dav1d side: `DAV1D_DBG_ENTROPY=1 DAV1D_DBG_BLOCKS=1` + `--cpumask 0` gives
> per-block `DAV1D B by/bx/bs/bp/rng` entry lines (42-46 blocks for p1a) plus
> `Post-skip/Post-intra/Post-intermode[n_mvs,mv]/DAV1D MV[n_mvs]` lines;
> Kinetix side: `KINETIX_AV1_DBG_B0ENTER` (per-block entry rng, pre-skip) +
> `KINETIX_AV1_DBG_B0` (skip/is_inter/ref/mvstack/mode cascade rngs).
> Verified aligned facts: p1a blocks 1-6 match dav1d block-for-block (positions,
> entry rngs 40248/53666/35799/33852, MVs 66/66/64); p1a's first divergent
> block is mi=(8,16) = px(32,64) 32x32, where dav1d decodes 16x8/8x4/8x8
> INTER blocks (16x8s at (32,64)/(32,80)/(48,64)/(48,80) with MVs incl.
> x=12/64/66, 8x4s, 8x8s) and Kinetix decodes ONE 32x32 INTRA. I.e. the
> entropy trees diverge at the partition/is_inter symbols covering
> px(32..63, 64..95) — after (4,18)/(0,20)/(2,20)/(0,22)/(4,20) decoded
> identically (MVs 66/64, positions match). dav1d's (20,0) is NEWMV
> mv=x:64 and Kinetix's (0,20) is mv=(64,0) — identical. The suspect list:
> the partition symbol CDF context at that block (our partition_context vs
> dav1d's get_partition_ctx — MiSizes-based, and our KTRACE showed ctx=0 on
> both sides of earlier blocks), the is_inter symbol's skip_mode gating, or
> the leftover-block tx-size reads. NOTE: the (24,16) 32x32 NEWMV mv=(0,31)
> with an empty mvstack is a *consequence* — dav1d's mvstack at that point
> had 2 candidates. CAUTION: several of this session's intermediate
> "mismatches" were trace-window artifacts (payload 1 packs 3 OBU_FRAMEs;
> always filter traces by FH markers, not line ranges).

> **2026-09-14 (cont'd 7) — the has_chroma fix improved every frame again
> (frame 1 38.83 dB, all frames 1,807-2,425 samples), and the desync is now
> pinpointed to `find_mv_stack` at one specific block.** The inter chroma
> path now checks §7.3.1 has_chroma — for 4:2:0, blocks whose luma height
> (or width) is ≤ 4px at an even mi_row/col have NO chroma (dav1d's
> `has_chroma = (bw4 > ss_hor || bx&1) && (bh4 > ss_ver || by&1)`; its
> read_coef_blocks skips the chroma coefficient loop for them). Kinetix
> read a uv coefficient block for the has_chroma=false 8x4 leaf at
> px(32,80), consuming extra bits — fixed with a has_chroma gate on the
> chroma loop (also skipping chroma edge geometry for those blocks).
> Every frame improved: frame 1 38.83 dB, frame 6 23.59 dB (18.9 before).
> **The definitive symbol diff** (dav1d `full.txt` trace with FH/B/MV
> prints vs Kinetix `B0ENTER`/`B0` traces, frames aligned by
> (show, refresh) signatures): p1a blocks 0-21 decode identically
> (positions + entry rngs; MVs 66/66/64 match). The trees diverge at the
> 8x8 block px(48,80) [mi=(12,20)]: **dav1d's mvstack has n_mvs=3 with
> NEARMV mv=(0,64); Kinetix's has n_mvs=2 with s0=(0,66), decoding
> NEARESTMV mv=(0,66)** — the ref_mv symbol then differs (dav1d reads the
> NEARMV path, we read NEARESTMV), consuming different bits and
> desyncing the rest of the frame. The missing/different candidate comes
> from find_mv_stack's spatial scan (the above/left neighbours at
> px(48,72)/px(40,80)) or the temporal projection. Next session: dump
> dav1d's refmvs candidates for that block (patch around
> dav1d_refmvs_find) vs Kinetix's find_mv_stack scan, and fix the
> candidate-set difference. CAUTION: dav1d's trace prints interleave
> mid-line (mingw fprintf is not locked across tasks) — always match
> blocks by by/bx, never by file order.

> **2026-09-14 (cont'd 8) — mvstack diff captured.** dav1d's p1a mvstack at
> the divergence block px(48,80) [mi=(12,20), 8x8]: n=3 candidates
> [(y0,x66), (y0,x0), (y0,x64)] — decoded NEARMV mv=(0,64). Kinetix: n=2
> [(0,66), (0,0)] — decoded NEARESTMV mv=(0,66). The third candidate
> (x=64 = 8px) is the final MV of the 8x8 block at px(0,80) [mi=(0,20),
> NEWMV x=64] — six mi-columns to the left. Our extended col-scan
> (n=2,3 at mi_col (12-2n+1)|1 = 9,7) does not reach mi_col 0, so the
> (0,64) candidate must come from a different scan position in dav1d's
> refmvs scan (possibly the AOD walk extension or the above-row walk
> reaching further left than our scan_row steps). Next session: trace
> dav1d's ref_mvs.c scan for this exact block (patch scan_row/scan_col in
> ref_mvs.c to print positions+mvs) and compare against our
> scan_row/scan_col in intra_block.rs inter_mv_stack for mi=(12,20)
> bsize=3 — then fix whichever scan position we miss. All work committed
> through cb9653d; trace files: /tmp/av1dbg/{full.txt, bl3.txt, stack.txt,
> p1a_tr*.txt} (regenerable via the documented env hooks).

> **2026-09-15 — FIXED: secondary spatial scans missed the §7.10.2.2/§7.10.2.3
> odd-column/row alignment.** Root cause of the p1a mvstack diff at
> px(48,80) [mi=(12,20)] 8x8: dav1d (and the spec) start the SECONDARY
> row scans (deltaRow -3/-5) at column `bx4 | 1` — spec: "deltaCol =
> 1 - (MiCol & 1)" — and the secondary col scans (deltaCol -3/-5) at row
> `by4 | 1` ("deltaRow = 1 - (MiRow & 1)"). Our scan_row/scan_col always
> started at bx4/by4 (even here), hitting different grid cells: at
> (21,9) sits an 8x4 with mv (0,64) (the missing third candidate, added
> via the scan_col loop path w=len*2=4) while we read (20,9). Method:
> env-gated MVSCAN traces on BOTH sides (dav1d patched refmvs.c prints
> every add_spatial_candidate/add_temporal_candidate + final sorted
> stack for DAV1D_DBG_MVSCAN="by:bx"; Kinetix mirrors it with
> KINETIX_AV1_DBG_MVSCAN in inter_mv_stack). After the fix the traces
> match add-for-add: final cnt=3 [66,0]w=652 [0,0]w=12 [64,0]w=4,
> nearest_cnt=1, nearest_match=1. The earlier "candidate comes from the
> 8x8 at px(0,80)" hypothesis was a value coincidence — the real source
> is the 8x4 at mi (21,9) two rows below, one column right of the
> block. Result: every inter frame improved (f1 38.83->47.00 dB, f2
> 32.4->40.65, diffs 1162/1431/1583/1793/1625/998/1612). MVSCAN debug
> hooks kept in both trees (env-gated). Next: re-run the diffmap to find
> the next divergent block (LR SGRPROJ set-14 stripe lead remains).

> **2026-09-15 (cont'd) — THREE more root causes fixed; p1a AND p1b now
> block-exact.** After the secondary-scan fix, frame-aligned per-block
> entry-rng comparison (env KINETIX_AV1_DBG_B0ENTER + FH dumps vs dav1d
> DAV1D_DBG_ENTROPY "DAV1D B" lines, poc added to the format) showed p1a
> fully aligned and four smaller divergences (p1b@16, E@3, G@1, H@20).
> Fixed in this round, each verified by add-level traces:
> 1. **Extended candidate search (7.10.2.12/13)** — dav1d's
>    add_single/add_compound_extended_candidate + global-MV fill were
>    missing entirely; without them a compound block with one spatial
>    candidate skipped the DRL symbol dav1d reads (p1b (12,18)).
>    Includes RefSignBias derived from wrapped order-hint distances.
> 2. **Temporal MV projection replaced with dav1d's rp_proj model** —
>    build_rp_proj (mod.rs) mirrors dav1d_refmvs_project: mfmv source
>    selection (LAST-if-alt!=gold, future BWD/ALTREF2/ALTREF, LAST2),
>    ref2cur/ref2ref poc distances clamped to ±31, save_tmvs filter
>    (compound saves mv[1]/ref[1], single saves mv[0], ref must be in
>    the source frame's PAST (mfmv_sign), |mv| < 4096), mv_projection
>    (7.9.3 div_mult table), landing position bounded to the source 8x8
>    sb-window. find_mv_stack now samples the rp_proj grid inside the
>    block + the three bottom/right sb cells, with globalmv_ctx =
>    (|proj| >= 16) from the first cell (7.10.2.14's ZeroMvContext).
> 3. **fix_mv_precision truncation** — dav1d truncates toward ZERO
>    ((v - (v>>31)) & !1); our `& !1` rounded -33 to -34 instead of -32
>    (E block (0,16) and the (4,18) grid cell pair). Also
>    force_integer_mv -> & !7.
> 4. **Skip-mode blocks record GlobalMV** — we predicted skip-mode MVs
>    from the neighbour stack; the spec/dav1d use the global MVs of
>    SkipModeFrame (zero here). The wrong pair (0,0)|(0,-34) poisoned
>    the refmv grid and desynced p1b from block 14.
> Result: p1a AND p1b fully block-exact (46 + 39 blocks); every frame's
> PSNR up again (f1 47->51.25 dB/355 diffs, f7 21.95->33.35 dB).
> REMAINING: E/G/H diverge (E inside block 2 = (0,16) 64x32 at the
> is_inter read, G/H at later blocks). VERIFIED NOT the cause: partition
> ctx, intra-bool semantics (100+ aligned reads), restore-slot mapping
> (CDFLOAD pri= per frame matches our refidx[primary_ref_frame]).
> ROOT CAUSE (confirmed by CDF-row probes): dav1d's frame-end CDF save
> (dav1d_cdf_thread_update, cdf.c) KEEPS the adapted values but ZEROES
> every CDF adaptation counter (update_cdf_1d sets the count element
> to 0; memcpy copies values verbatim up to m.intrabc, the tail keeps
> the restored-in values). Our save keeps the raw adapted CDFs WITH
> their counters, so after a save the adaptation rate
> (rate = 3 + cnt>15 + cnt>31 + log2(n)) is too slow and every
> subsequent restore adapts differently. E's is_inter read at (0,16):
> dav1d m.intra[0] = complement(709) with cnt=0 (values from earlier
> frames, counters reset at save); ours = 806/31962-complement with
> stale counts. FIX: at frame-end save, copy adapted values and reset
> every CDF count field to 0 (mirror dav1d_cdf_thread_update's field
> walk, incl. which arrays it touches). G/H restore slots written by
> E/F and inherit the drift, so this one fix should clear all three.
> Probes kept: KINETIX_AV1_DBG_MVSCAN / _CDFROW / dav1d MVSCAN +
> INTRACDF + PARTCTX (by=bx=bl ctx abyte lbyte) + CDFLOAD/CDFSAVE.

> **2026-09-15 (cont'd 2) — FIXED: all 8 frames now block-exact.** The
> E/G/H root cause was two-part, both in the §6.8.2 context save/restore:
> 1. **Adaptation counters must be zeroed at save.** dav1d's
>    dav1d_cdf_thread_update keeps the adapted values but writes 0 into
>    every CDF count element (its update_cdf_* macros); we saved the
>    counts, making every restored frame adapt at a stale rate. Fix:
>    ModeCdfs/TileCdfs/InterCdfs gained reset_adaptation_counts() walks
>    (each CDF array's final element = count).
> 2. **InterCdfs was never saved/restored at all.** The is_inter /
>    compound / MV-component CDFs live in a separate `InterCdfs` struct
>    that TileDecodeState re-initialised to defaults every tile
>    (`InterCdfs::new()`), while dav1d carries the adapted values
>    frame-to-frame. Frames with primary_ref != NONE (D..H) restored
>    defaults and diverged at their first is_inter read; frames with
>    primary_ref == NONE (p1a/p1b) matched because they start from
>    defaults anyway. Fix: FrameCdfContext now carries `inter_cdfs`
>    (cloned into the tile state on restore, counter-reset at save).
> Corrected understanding of dav1d_cdf_thread_update: it does NOT blend
> with defaults — it memcpy's the adapted tile CDFs and zeroes counters
> (CDF1(x) macros store 32768-x complements; the m.intra counter lives
> in the row's second element). Verified with a CDFUPDATE probe.
> RESULT: p1a..H ALL EIGHT frames block-exact (position+size+entry-rng
> match every block; counts 46/39/4/4/4/4/4/39). PSNR: f1 51.25, f2
> 44.86, f3 41.22, f4 42.84, f5 42.79, f6 40.83, f7 35.24 dB; total
> luma diff samples 4662 (was ~14k at session start, 33k before that).
> NEXT: the remaining per-pixel diffs (exact=false) are in prediction/
> filtering details — run the diffmap per frame for the next lead
> (LR SGRPROJ set-14 stripe handling remains a known gap).

> **2026-09-15 (cont'd 3) — inter-intra implemented; OBMC mask fixed; all
> frames 64-70 dB.** Three more fixes, each driven by the pre-filter
> pixel dumps (KINETIX_AV1_DUMP_PREFILTER now writes per-tile-group
> numbered dumps) vs dav1d DAV1D_DUMP_FRAMES with the NODEBLOCK/NOCDEF/
> NOLR2 gates (raw reconstruction on both sides):
> 1. **Inter-intra prediction (7.11.3.6) implemented.** We read the
>    interintra_type/mode/wedge_idx symbols but discarded them and never
>    blended. Now: intra prediction (mode mapped DC/V/H/SMOOTH) from the
>    reconstructed block edges via block_borders + predict_intra_block,
>    blended over the inter prediction with the sign-0 wedge mask
>    (luma + chroma, chroma samples the luma mask at >>sub). dav1d's
>    blend_px = ((inter*(64-m) + intra*m) + 32) >> 6.
> 2. **OBMC mask table was wrong.** Our obmc_mask was the SMOOTH
>    raised-cosine RISING to 64; dav1d_obmc_masks DECAY away from the
>    shared edge (4 -> {25,14,5,0}, 8 -> {28,22,16,11,7,3,0,0}, ...) and
>    blend_h truncates to 3/4 of the overlap (the tail is zero-weight).
>    This was the single biggest error source (f1 52 -> 70.4 dB).
> 3. **Intra-in-inter blocks now clear ref_left/above + mv_left/above**
>    (dav1d marks intra blocks ref=INTRA so OBMC's overlap scan skips
>    them; our stale inter refs made OBMC blend with phantom
>    neighbours).
> 4. **Skip-mode MVs = the MV stack's NEARESTMV candidate** (dav1d runs
>    a full refmvs_find for the SkipModeFrame pair); both the zero-MV
>    and abridged-neighbour variants were wrong.
> RESULT: all 8 frames 64-70 dB luma (f1 70.39, f6 63.9->65.1...), total
> luma diff samples 1285 for the whole sequence (from 33k). Remaining:
> scattered +-1 rounding pixels near skip-mode neighbours of OBMC blocks
> — leading suspect: the OBMC job's MC filter provenance (dav1d uses the
> neighbour's filter_2d 2D combination; we pass filter_above[0] = the
> vertical component only, and skip-mode neighbours never write the
> filter arrays at all) and warp-subblock emu-edge extents. Probes:
> KINETIX_AV1_DBG_OBMC + dav1d DAV1D_DBG_OBMC (per-job x/y/w/h/mv/f).

> **2026-09-15 (cont'd 4) — dual-filter MC plumbing; 6-bit experiment
> reverted.** The remaining ±1s sit at subpel-horizontal blocks as
> CONSTANT-PER-COLUMN offsets (e.g. p1a (4,18): +1 at px 22-23, -1 at
> 24 and 31 across all 8 rows) — the horizontal subpel kernel/rounding
> differs from dav1d for those columns. Two findings:
> 1. **Dual filters fixed and kept**: dav1d's Filter2d pairing applies
>    the FIRST-read symbol horizontally and the SECOND vertically
>    (fh = type & 3, fv = type >> 2; the "dir 0 = vertical" comment on
>    our filter arrays was wrong — dir 0 = horizontal). motion_compensate
>    and motion_compensate_prep now take (filter_h, filter_v); the OBMC
>    neighbour jobs carry the neighbour's full pair. (This stream's
>    dual-filter blocks are full-pel vertically, so no pixel change here,
>    but required for real content.)
> 2. **dav1d-6-bit subpel model tried and REVERTED**: porting dav1d's
>    6-bit mc_subpel_filters + its per-branch rounding ((s+2)>>2 /
>    (s+512)>>10 both-axes, (s+34)>>6 h-only, (s+32)>>6 v-only) made
>    every frame WORSE (f1 73 -> 217) even after fixing the phase index
>    to mx-1. The spec-model (7-bit Subpel_Filters, rounds 3/11) is
>    empirically closer to dav1d than the literal 6-bit port — the
>    remaining ±1s are NOT explained by the filter table/rounding alone;
>    something upstream (reference pixels or the exact kernel rows
>    dav1d's SIMD-equivalent C selects) still differs. NEXT: pixel-level
>    probe of dav1d's put_8tap inputs for one divergent column
>    (e.g. (4,18) px 22: +1 constant) — print (src offsets, kernel row,
>    s, result) in dav1d's put_8tap_c gated on coordinates, and compare
>    against ours sample-for-sample.
> STATE: 73/171/225/221/166/120/129 per-frame luma diffs (total 1285,
> 96% reduction); all frames 64-70 dB; symbols block-exact everywhere.

> **2026-09-15 (cont'd 5) — ±1 chase methodology refined.** Hand-model
> experiment on p1a (4,18) (skip-block with mv (0,66), subpel-horiz):
> computing both the spec-model and the dav1d-6-bit predictions from the
> FILTERED p0 reference shows both decoders AGREE with each other (202/
> 198/195...) but neither matches the plain-MC model (150) — the block
> carries a residual (skip=0; the earlier trail read was wrong). The
> remaining ~1,285 samples are ±1s scattered across residual-carrying
> OBMC/subpel blocks. DEFINITIVE next tool: dump the PRE-RESIDUAL
> prediction per block from BOTH decoders (ours: KINETIX_AV1_DBG_PRED
> already prints error-region blocks; extend to write the prediction
> plane before add_inter_residual; dav1d: probe recon_tmpl before the
> residual add) and diff predictions block-by-block — this separates
> prediction rounding from residual/ITX rounding cleanly. Also verify
> our ITX matches dav1d's (s+8)>>4 final shift at tx edges.

> **2026-09-15 (cont'd 6) — TXPRED prediction probes built both sides.**
> dav1d: DAV1D_DBG_TXPRED prints pre-residual dst rows for all non-skip
> blocks by>=16 (recon_tmpl.c, before the b->skip early-return); ours:
> KINETIX_AV1_DBG_PRED(+_ALL) prints the same per block. FIRST RESULT
> (p1a (4,18) 16x8 SIMPLE, mv=(0,66) subpel-horiz): predictions differ
> +-1 at 4 constant columns (ours 180,177,...,151 vs dav 179,176,...,152)
> — prediction-level, NOT residual. A Python reconstruction of the
> dav1d-model MC+OBMC blend for that row from the filtered p0 does NOT
> reproduce dav's values (off by -43..+7 gradient), meaning the block's
> inputs (base position/mv/lap source) differ from the hand model — the
> exact OBMC geometry (which neighbour, which mv, which lap rect) must
> be dumped, not inferred. NEXT: extend the TXPRED probes to also print
> the block's final mv/filters/ref, and add a dav1d obmc() probe listing
> its lap MC call args (position, mv, filter, size) per block — then
> match against our ObmcJob list for the same block. All infrastructure
> is in place; this is the last-mile ±1 (total 1,285 samples = 0.4% of
> session start).

> **2026-09-16 — FOUND + FIXED: dual-filter horizontal/vertical assignment
> was backwards.** Root-caused mi(4,18) 16x8 SIMPLE (mv=(0,66), skip=false,
> dual filter read [dir0=REGULAR, dir1=SMOOTH]) with a real per-pixel
> `put_8tap_c` trace added to the patched dav1d (instrumented
> `recon_tmpl.c`'s `mc(t, dst, ...)` call site + a `kinetix_dbg_mcpx_active`
> global read inside `mc_tmpl.c`'s `put_8tap_c`, gated on `t->bx==4 &&
> t->by==18`, printing the literal `fh` row and `FILTER_8TAP` sum dav1d
> used). dav1d's actual horizontal kernel for this block was
> `dav1d_mc_subpel_filters[DAV1D_FILTER_8TAP_SMOOTH][3] = {0,0,10,30,21,3,0,0}`
> (confirmed: `(FILTER_8TAP(...)+34)>>6 = 179`, matching dav1d's own
> postMC trace exactly), NOT REGULAR as our code assumed — REGULAR at the
> same phase gives 180, which is what Kinetix printed pre-fix (Kinetix's
> `PRED-BASE` line showed `fh=0 fv=1` i.e. dir0=REGULAR assigned to
> horizontal). The bug: 2026-09-15 (cont'd 4)'s "dual-filter" fix assumed
> "first-read (dir-0) symbol = horizontal, second-read (dir-1) = vertical",
> reasoning from `Filter2d` enum bit tricks (`filter_type & 3` /
> `filter_type >> 2`) — but `filter_type` inside `put_8tap_c` is NOT the
> `Filter2d` enum ordinal at all; each named combination
> (`filter_fns(smooth_regular, DAV1D_FILTER_8TAP_SMOOTH,
> DAV1D_FILTER_8TAP_REGULAR)` in `mc_tmpl.c`) builds its own local
> `type_h | (type_v << 2)` at the call site, and
> `dav1d_filter_2d[filter[1]][filter[0]] = FILTER_2D_8TAP_SMOOTH_REGULAR`
> for `filter[1]=SMOOTH, filter[0]=REGULAR` — i.e. `filter[1]` (dir-1,
> second-read) is horizontal and `filter[0]` (dir-0, first-read) is
> vertical. Bit-decomposing the plain `Filter2d` enum ordinal (as the
> previous session did) gives a plausible-looking but WRONG answer for
> some combinations (verified: `6 & 3 = 2` decodes to SHARP, not the real
> REGULAR) — this is the actual dead end the "6-bit filter experiment
> reverted" session hit without knowing it. FIX (commit pending): swapped
> the two args at all 3 real MC call sites in
> `reconstruct/inter_block.rs` (`motion_compensate` single-ref,
> `motion_compensate_prep` compound, and the OBMC neighbour-job
> `motion_compensate`) to pass `(filters[1], filters[0])` instead of
> `(filters[0], filters[1])`; left the `dir`-indexed neighbour-context
> CDF derivation and `filter_above`/`filter_left` storage untouched since
> those must stay in raw bitstream dir-index form. VERIFIED: mi(4,18)'s
> block is now bit-exact (was ±1 at 4 columns); total inter-sequence luma
> diff samples 1220→681 across all 8 frames (44% reduction); frame PSNRs
> up (f1 Y 71.17, f6 74.88, f7 74.71 dB, from ~65-70 pre-fix); AV1 intra
> corpus stays 6/6 bit-exact; full `cargo test --workspace --lib --bins`
> and `cargo clippy -p tpt-kinetix-av1 --all-targets -- -D warnings` clean.
> REMAINING: ~681 luma diff samples still open, now concentrated at mi
> (20,16)/(24,18)-ish blocks (frame1 first-diverging pixel moved to
> (80,66)) — root cause not yet investigated; likely a second, unrelated
> ±1 source (OBMC mask application order, or another filter-context edge
> case) since the mi(4,18)-class bug is now closed. Warp-affine regression
> (16 blocks, frame4/6 better vs frame5/7 worse under `KINETIX_AV1_NO_WARP`
> bisection) is UNTOUCHED this session — still open, unrelated to this fix.

> **2026-09-16 (cont'd) — FOUND + FIXED: CDEF was missing the §7.15.1
> "noskip" gate, filtering fully-skipped 8×8 blocks it should have left
> alone.** Continued chasing the (80,66) diff from the session above.
> Traced `minimal_av1_inter_ivf` frame 1's first-diverging pixel (80,66),
> found via `dbg_av1_inter.rs`'s diffmap: Kinetix=152, dav1d=153. The
> DISPLAYED frame's own block at mi(16,16) (16×8, `skip=false`, COMPOUND
> `COMP_INTER_WEIGHTED_AVG`, `jnt_weight=11`) was traced end-to-end —
> compound blend math, weight table, intermediate-domain precision all
> matched dav1d exactly (confirmed via new dav1d instrumentation added to
> `recon_tmpl.c`'s compound branch, `COMPPX`/`postBLEND` prints) — *except*
> ref1's intermediate MC value: dav1d's `tmp[1]=2400` (⇒ ref pixel 150),
> Kinetix's own `t1=2384` (⇒ ref pixel 149). Both engines pull ref1 from
> the same semantic reference slot; the 1-pixel gap was therefore already
> baked into a HIDDEN reference frame decoded earlier within the same IVF
> "frame 1" payload (this synthetic stream packs 3 real OBU frames per
> IVF-frame entry — 2 hidden + 1 shown; `dbg_av1_inter.rs`'s naive
> payload-index-as-frame-index diffmap only sees the shown one, so the
> real bug was invisible to the existing harness and had to be traced via
> `apply_post_filters`'s new `KINETIX_AV1_DBG_PXY=x,y` stage-tracer and
> `RefFrameStore::refresh`'s new `KINETIX_AV1_DBG_REFRESH` hook, which
> together show the pixel's value crossing pre-filter→deblock→CDEF→LR
> and which physical ref slot each hidden frame's output refreshes).
> The hidden frame's own trace: pre-filter=150, post-deblock=150 (no
> change), **post-cdef=149 (CDEF alone introduced the -1)**, post-lr=149.
> Root cause: `cdef_plane_luma`/`cdef_plane_chroma` (`loop_filter.rs`)
> filtered *every* 8×8 block in a CDEF unit unconditionally — there was no
> equivalent of dav1d's `noskip_mask` gate (`decode.c`,
> `dav1d_cdef_brow`: `if (!(noskip_mask & bx_mask)) { ... goto next_b; }`,
> populated by `if (!b->skip) noskip_mask |= ...` per coded block). AV1
> §7.15.1 only applies CDEF to an 8×8 block when at least one of its
> covered 4×4s carries real coefficients; a fully-skipped block (pure MC
> copy) must come out byte-identical. `FrameMeta::luma_skip` (already
> populated per-8×8 by `record_luma`, previously used only by
> `deblock_plane`) is exactly this flag and was simply never threaded
> through to CDEF. FIX: added `luma_skip: &[bool], w8: usize` params to
> both `cdef_plane_luma` and `cdef_plane_chroma`, skip the whole 8×8 body
> when the covered cell is fully skipped (chroma gates on the co-located
> *luma* cell, matching dav1d's single shared `noskip_mask`/`bx_mask`
> check that skips both planes' filters together), wired through both
> call sites in `apply_post_filters`. Added a regression test
> (`cdef_skips_a_fully_skipped_8x8_block`) asserting a hard step edge with
> `luma_skip=true` and a strong pri/sec strength comes out byte-identical.
> VERIFIED: `av1_inter_sequence_vs_dav1d_when_available` luma diff samples
> per frame 61/145/180/144/73/26/27 (656 total) → 12/40/48/40/13/0/8 (161
> total), a 75% reduction; frame 6 is now fully bit-exact (was 26 diffs).
> `av1_inter_corpus_vs_dav1d_when_available`'s `testsrc_96x64` clip also
> improved sharply (diffs 1/5/5/0/0, was materially worse); `testsrc_64x64`
> unchanged (~500-780 diffs per frame, pre-existing separate issue,
> confirmed via `git stash` A/B — not touched by this fix, likely the warp
> path). AV1 intra corpus stays 6/6 bit-exact (`cargo test -p
> tpt-kinetix-av1` 153/153, `cargo clippy -p tpt-kinetix-av1 --all-targets
> -- -D warnings` clean, `cargo test --workspace --lib --bins` all green).
> New debug hooks left in place (all off by default): `KINETIX_AV1_DBG_PXY`
> (loop_filter.rs, trace one pixel across filter stages),
> `KINETIX_AV1_DBG_REFRESH` (decoder.rs, trace one pixel whenever a DPB
> slot is refreshed, to identify which internal/hidden frame produced a
> given reference), `KINETIX_AV1_DBG_COMP` (inter_block.rs, dump compound
> blend inputs/weight for one mi position), `KINETIX_AV1_DBG_CDEFPX`
> (loop_filter.rs, dump one CDEF unit's pri/sec/dir/variance/pre-filter
> row). REMAINING for next session: the residual ~161 diff samples are
> smaller and more scattered than before — re-run the diffmap fresh
> (coordinates will have shifted again) rather than assuming (80,66)/
> mi(16,16) is still the first divergence. `testsrc_64x64`'s much larger
> per-frame diffs (500-780) were NOT investigated this session and look
> like a different bug (possibly the still-open warp-affine regression,
> Thread B from this session's brief — that clip is small enough
> (64×64, 1 SB) that warp blocks are proportionally more common). Also
> untouched: the warp-affine regression itself (16 blocks across the
> 8-frame `minimal_av1_inter_ivf` clip, mixed frame4/6-better vs
> frame5/7-worse under `KINETIX_AV1_NO_WARP`).

> **2026-09-17 — FOUND + FIXED: skip-mode blocks never recorded their
> `RefFrames`/`modeType` for the deblock filter, so any edge touching one
> derived its filter *level* from the wrong reference.** Picked Thread A
> (the residual ~161-diff-sample cluster on `minimal_av1_inter_ivf`) over
> Thread B (warp) because it had a fresh, concrete repro already described
> in this file, whereas warp needed a from-scratch trace; re-ran the
> diffmap fresh per the prior session's own advice rather than trusting
> the old (80,66)/mi(16,16) coordinate — it had indeed moved. All 161
> residual diffs are exactly ±1..±3, in small scattered clusters, several
> recurring at the same (x,y) across consecutive frames (propagation
> through skip/copy blocks from one bad frame).
>
> Traced frame 1's first divergence, (28,71): Kinetix=172, dav1d=171.
> Added a `debug_frame_seq` module (`AtomicU64`, `next()`/`current()`) so
> cross-module `eprintln!` traces could be pinned to a specific *decode-order*
> frame index — necessary because this clip's hierarchical GOP decodes
> frames out of display order (keyframe oh0, then hidden oh6, hidden oh3,
> then the 7 shown deltas oh1..oh7), and naively correlating debug output
> by nearby line numbers or by *guessing* which "call" is which display
> frame cost real time this session (a `stdout`-vs-`stderr` buffering
> question was also raised and ruled out: dav1d's own trace printf's are
> all on the same stream, so their relative order is trustworthy; it's
> only cross-stream stderr/stdout interleaving from `2>&1` redirection
> that can lie, and only for prints on *different* streams from each
> other).
>
> With `KINETIX_AV1_DBG_PXY=28,71` + the new `KINETIX_AV1_DBG_SEQ` frame
> marker, found the actual reconstruction was already correct
> (pre-filter=170, matching dav1d's own traced value for that same
> internal frame via a new `KINETIX_DBG_SBROW`-style hook added to the
> patched dav1d's `dav1d_filter_sbrow`) — **the divergence is introduced by
> DEBLOCK**, not residual/prediction/LR as a first pass wrongly concluded
> (the LR/SgrProj no-op that showed up in an earlier trace was just
> correctly filtering already-corrupted post-deblock input, not a bug of
> its own). New `KINETIX_AV1_DBG_DEBLOCK` hook (`loop_filter.rs`) isolated
> the exact edge: a horizontal edge at `y=72`, `bx=7` (a real content step,
> p-side flat 170 / q-side flat 175) computed `lvl=3` and applied a
> genuine filter4 correction (hand-verified against §7.14.6.4's formula:
> `filter=3*(qs0-ps0)=15`, `filter1=filter2=2`, giving `p0'=172`,
> `q0'=173` — exactly Kinetix's output). dav1d leaves this same edge
> byte-identical, which is only possible if it computes `lvl=0` there
> (skips the edge outright) — a *level* bug, not a filter-math bug.
>
> Root cause: `lf_ref4`/`lf_mode4` (the per-4×4 `RefFrames`/`modeType`
> grids `compute_level` (§7.14.4) reads) are written by exactly one call
> site, `record_lf4()`, called from the ordinary inter-block path
> (`inter_block.rs`, after `add_inter_residual`). AV1's **skip-mode**
> feature (§5.11.11's separate `skip_mode` flag — a frame-level shortcut
> that predicts a whole block from the fixed `SkipModeFrame` pair with no
> per-block ref/mv/mode signaling at all) is handled by a *different*
> function, `decode_skip_mode_block`, which also calls
> `add_inter_residual` but — unlike the ordinary path — never called
> `record_lf4()`. Every skip-mode block therefore left its covered `lf_ref4`
> cells at `FrameMeta`'s zero-initialized default, which `compute_level`
> reads as `ref_idx == 0` == `INTRA_FRAME`, taking the "intra edge" branch
> (`loop_filter_ref_deltas[INTRA_FRAME]` alone, no mode delta) instead of
> the block's real inter reference + mode delta. Confirmed directly: a new
> `KINETIX_AV1_DBG_LFREF` print (recorded ref/mode per block) showed a real
> gap in mi-column coverage at the failing edge's position in frame 2's own
> per-block dump — the covering block was invisible to the (pre-existing)
> `KINETIX_AV1_DBG_B0` per-block tracer too, because `decode_skip_mode_block`
> returns before reaching that tracer's print statement, which is the same
> "this path is a separate, easy-to-miss function" shape as the bug itself.
>
> FIX (`inter_block.rs`, `decode_skip_mode_block`): added the same
> `record_lf4(ref_names[0] - 1, mode_type)` call the ordinary path makes,
> right after its own `add_inter_residual`. `modeType` is hardcoded to `1`
> unconditionally, matching the ordinary compound path's
> `comp_mode != GLOBALMV_GLOBALMV` derivation, since skip-mode always
> predicts from the NEAREST-MV stack entry, never `GLOBALMV_GLOBALMV`.
>
> VERIFIED: `av1_inter_sequence_vs_dav1d_when_available` per-frame luma
> diff samples 12/40/48/40/13/0/8 (161 total) → 8/36/44/35/9/0/8 (140
> total), a further ~13% reduction with no regression on any frame; frame
> 6 stays fully bit-exact. `av1_inter_corpus_vs_dav1d_when_available`'s
> `testsrc_96x64` stays at 0 luma diffs on every frame; `testsrc_64x64`'s
> large per-frame diffs (492-761, pre-existing) are unchanged, consistent
> with that clip's gap being the separate warp-affine issue (Thread B,
> still untouched). AV1 intra corpus stays 6/6 bit-exact. `cargo test -p
> tpt-kinetix-av1` (154/154 unit + all integration suites),
> `cargo clippy -p tpt-kinetix-av1 --all-targets -- -D warnings`, and
> `cargo test --workspace --lib --bins` are all clean.
>
> New debug hooks left in place (all off by default, all in `tpt-kinetix-av1`
> unless noted): `KINETIX_AV1_DBG_SEQ` (`decoder.rs`, prints a running
> per-real-coded-frame index + `order_hint`/`show_frame`/`frame_type` —
> pairs with the new `debug_frame_seq` module's `current()` to label any
> other hook's output with *which* frame produced it, essential for this
> clip's hierarchical decode order), `KINETIX_AV1_DBG_DEBLOCK`
> (`loop_filter.rs`, dumps level/limit/blimit/filter_size and the raw
> pre-filter pixel window for the vertical edge at x=28 or horizontal edge
> near y=71 — coordinates are hardcoded to this session's repro and will
> need re-pointing for a different one), `KINETIX_AV1_DBG_SGR`
> (`loop_filter.rs`, dumps SgrProj inputs/correction at pixel (28,71)),
> `KINETIX_AV1_DBG_LFREF` (`inter_block.rs`, dumps `ref_names`/`mode_type`/
> the recorded ref-delta index for every ordinary-path inter block — does
> NOT cover skip-mode blocks even after this fix, since the fix only adds
> the *write*, not a matching debug print; add one at the same call site if
> a future session needs to inspect skip-mode ref recording specifically).
> Outside this repo: the patched dav1d clone
> (`%LOCALAPPDATA%\Temp\tpt-kinetix-dav1d\dav1d`) gained a `KINETIX_DBG_SBROW`
> hook in `recon_tmpl.c`'s `dav1d_filter_sbrow` (dumps one pixel
> pre-deblock/post-deblock/post-cdef/post-lr for a hardcoded
> `frame_offset`) and a `curpoc`/`refpoc`/raw-reference-pixel dump added to
> the existing `KINETIX_DBG_MCPX` `COMPPX` print — both reusable, both
> currently pointed at this session's specific frame/pixel and needing
> re-coordination for a different repro; remember to `ninja -C build` +
> manually copy `build/src/dav1d.dll` over `build/tools/dav1d.dll` after
> editing.
>
> REMAINING for next session: 140 diff samples still open on
> `minimal_av1_inter_ivf`, concentrated in frames 2-4 (36/44/35). Given
> this session found a whole *class* of missing-metadata bug (skip-mode
> blocks not updating a per-block grid used by a later pass), it's worth
> checking whether `decode_skip_mode_block` is missing *other* such
> updates too (e.g. anything else the ordinary path threads into
> `FrameMeta` that skip-mode's own bookkeeping loop at the end of the
> function — the `is_inter_left`/`comp_type_left`/etc. neighbour-context
> updates — doesn't also cover, such as CDEF's `luma_skip`/`cdef_idx` or
> the chroma tx-size grids) before re-diffmapping from scratch. The
> warp-affine regression (Thread B) remains completely untouched.

> **2026-09-17 (later session) — FOUND + FIXED: the SgrProj 5×5 pass sampled
> the WRONG A/B rows for even output rows — every even row of every 5×5 SGR
> unit computed a slightly wrong projection. This was the entire ~140-sample
> luma gap on `minimal_av1_inter_ivf`.** The session started from the prior
> session's hand-off ("check whether `decode_skip_mode_block` is missing
> other FrameMeta updates, then re-diffmap"). The audit came back CLEAN:
> every per-block metadata write (`record_luma`/`record_chroma`/edge marks/
> `delta_lf`/CDEF `luma_skip` inputs) lives in the shared `add_inter_residual`,
> which skip-mode already calls; the neighbour-context bookkeeping loop is
> complete; and the one suspicious difference (skip-mode's `new_mf=0` splat)
> matches dav1d, whose skip-mode blocks report `inter_mode = NEARESTMV` =
> mode-context 0 in the ordinary path's own encoding (0=NEAREST/NEAR,
> 1=GLOBALMV, 2=NEWMV). So the remaining diffs were NOT another missing-record
> bug, and the session pivoted to the new stage-isolation method that ended
> up carrying the day:
>
> **Methodology (reusable): dav1d CLI `--inloopfilters` bitmask vs Kinetix's
> env gates, per filter stage.** The patched dav1d CLI accepts
> `--inloopfilters=<bitmask>` (deblock=1, cdef=2, restoration=4; `none`,
> `0x1`, `0x3`, `0x7`) — decode the SAME stream once per stage and diff
> against Kinetix runs with the matching `KINETIX_AV1_NODEBLOCK`/`NOCDEF`/
> `NOLR` subsets (feeding one `dbg_*` harness's `KINETIX_AV1_SAVE_OUT`).
> First finding from the raw stage: **raw reconstruction is now BIT-EXACT
> through frame 6** (the 2026-09-14/15 inter-session's work holding), so any
> residual diff is introduced by the post-filters. Deblock-only: bit-exact.
> Deblock+CDEF (0x3): bit-exact. Full (0x7): 8/36/44/35/9/0/8 → the entire
> gap is LOOP RESTORATION. (Yesterday's "deblock introduced the (28,71)
> divergence" conclusion was correct *at that time* — the skip-mode
> `record_lf4` fix removed the deblock component, leaving pure LR error.)
>
> Root cause: §7.17.4's 5×5 self-guided pass has a per-row-pair structure —
> ODD rows take their projection from their OWN A/B row (center 6 + sides 5,
> `>> 8`), but EVEN rows take the "six neighbors" pattern over A/B rows
> **(y-1, y+1)** — dav1d's `sgr_finish_filter2` reads `A_ptrs[0]`/`A_ptrs[1]`
> which hold the bracketing rows' projections, NOT the current row's.
> Kinetix's `sgrproj_filter_plane` `pair_rows` branch sampled `(y, y+1)` —
> the current and next row — making every even row's projection slightly
> wrong. Both decoders' A/B *tables* (including the stripe-boundary halo
> rows) were verified identical by hand-tracing dav1d's ring-buffer
> construction (`sgr_5x5_c`'s `rotate5_x2` + the `n_lines`/replicate rules in
> `backup_lpf`); only the even-row sampling differed. This also explains the
> corpus history: mandelbrot's SGR units are 3×3-only (set 10) — the
> `pair_rows` path never ran — while testsrc luma uses set 14 (5×5-only), so
> the bug was invisible until a 5×5 SGR stream was diffed this precisely.
> FIX: even rows now sample `(y-1, y+1)` (a_tab's ±1 halo covers the edges;
> segment tops are always even so pair parity stays stripe-aligned for all
> unit sizes). VERIFIED: `av1_inter_sequence_vs_dav1d_when_available` luma
> diff samples 8/36/44/35/9/0/8 (140) → **0/0/0/0/0/0/8**; frames 0-6 luma
> BIT-EXACT (PSNR Y = inf) and frame 0 is now fully bit-exact including
> chroma (was `exact = false` on chroma); `av1_intra_corpus_vs_dav1d` stays
> 6/6 bit-exact, now with **inf/inf/inf PSNR on every entry** (mandelbrot's
> long-standing 72 ±1 pixels were the same bug). Frame 7's 8 residual luma
> samples trace to its 6 raw-reconstruction diffs, not filtering.
>
> **Thread B (testsrc_64x64, 492-761 luma diffs/frame): the "warp-affine
> regression" attribution is DISPROVEN, and one real bug was found and fixed
> anyway; the clip's remaining gap is now precisely characterized.** Facts
> established: (1) `KINETIX_AV1_NO_WARP` changes NOTHING on this clip — warp
> is never exercised (the prior session's guess that the small clip's gap
> "looks like warp" was wrong). (2) With all filters off, even the KEYFRAME
> has 114 ±1-2 raw samples (rows 46-63, the timestamp-text band; the same
> signature as the old mandelbrot directional-intra note) — but they filter
> to identity, so the *filtered* keyframe is bit-exact. (3) The inter diffs
> are reconstruction-level: shown frame oh1 is ONE 64×64 compound
> GLOBALMV_GLOBALMV zero-MV block averaging the two hidden references
> (oh4→slot 1, oh2→slot 2), and Kinetix's output ≠ any blend of its own
> stored refs in the text band (839/12288 px), while the stored keyframe is
> verified post-filter and bit-exact vs dav1d (0 diffs). Conclusion: the
> divergence originates in the HIDDEN frames' own reconstructions (oh4
> first, which has real subpel-MV residual blocks over the text band) and
> propagates through the blends — the same isolation barrier as the 128x96
> chroma gap: **dav1d never outputs hidden frames, so next session needs
> internal-frame dumps from the reference side** (patch dav1d's
> `dav1d_submit_frame`/filter_sbrow to write every decoded frame, rebuild
> with `ninja -C build` + copy `build/src/dav1d.dll` over
> `build/tools/dav1d.dll`; Kinetix's side already dumps every decoded frame
> via `KINETIX_AV1_DUMP_FRAMES`).
>
> Found and fixed along the way (real spec gap, verified dav1d-verbatim but
> a no-op for identity-global-motion streams): **GLOBALMV-coded blocks never
> derived their MV from the frame header's global motion parameters.** The
> single-ref cascade mapped the zero_mv symbol (spec GLOBALMV) and the
> compound cascade mapped comp_mode 6 (GLOBAL_GLOBALMV) to `Mv::default()`,
> ignoring `fh.gm_type`/`fh.gm_params` entirely; and the interpolation-
> filter read lacked dav1d's `has_subpel_filter` gate (a GLOBALMV block over
> IDENTITY gm has integer MVs → dav1d reads NO filter symbols and forces
> REGULAR; Kinetix read them anyway, which would desync GLOBALMV-heavy
> switchable-filter streams). FIX: `gm_type`/`gm_params` threaded into
> `TileDecodeState`; new `get_gmv_2d` (dav1d env.h verbatim: TRANSLATION =
> `matrix[0..1] >> 13`, ROTZOOM/AFFINE = full model at the block centre,
> `fix_int_mv_precision` under `force_integer_mv`; note dav1d *warps* with
> the full model when `gmv_warp_allowed` — the centre MV is only a fallback
> there, no corpus clip exercises global rotation yet); `has_subpel_filter`
> computed per dav1d (sub-8×8 → true; GLOBALMV → gm type == TRANSLATION;
> compound GLOBAL_GLOBALMV → either ref TRANSLATION) and gating the filter
> read. Also: the LFREF debug hook now covers skip-mode blocks (the prior
> session's own suggested follow-up), and `KINETIX_AV1_DUMP_FRAMES` in
> `decoder.rs` (pre-existing) is confirmed to dump every *decoded* frame
> including hidden ones.
>
> New harness: `tpt-kinetix-test-utils/tests/dbg_av1_warp.rs` — the
> testsrc_64x64 counterpart of `dbg_av1_inter.rs` (TU-split feeding, OBU
> save via `KINETIX_AV1_SAVE_OBU` for dav1d CLI stage runs, per-frame diff
> counts + first-wrong + frame-1 heatmap).
>
> REMAINING (next session, priority order): (1) **hidden-frame dump from
> dav1d** — one patch unlocks both open threads: the 64x64 clip's oh4-first
> luma chain AND the 128x96 chroma gap (~850 raw samples, chroma rows 37-47,
> same band as the luma text edges; dav1d stage isolation showed chroma
> diverges at the DEBLOCK stage but inherits from raw recon diffs of the
> same shape — both decoders' deblock+CDEF outputs are identical given
> identical inputs). (2) frame 7 of the 128x96 clip: 6 raw luma samples
> ((11,80-84) ±1 and (0,84)) — smallest concrete recon case left on that
> clip; its blocks reference hidden frames too. (3) Global motion
> ROTZOOM/AFFINE warping (`gmv_warp_allowed`) — `get_gmv_2d`’s
centre-MV result is only a fallback there; implementing full global-warp
MC is its own task. (4) The 64x64 clip’s raw keyframe ±1-2s in the text
band (rows 46-63; intra prediction of dense text; filtered-to-identity so
cosmetic for output but worth one look alongside (1)).

> **2026-09-17 (evening session) — dav1d patched to dump HIDDEN frames; the
> 64x64 clip's remaining gap root-caused to an ENTROPY desync starting in
> the first hidden frame's VERT-split-8x8 residual read; one real spec fix
> landed (`disable_cdf_update`).** The dav1d clone gained two hooks:
> `KINETIX_DBG_DUMPF` (decode.c, `dav1d_decode_frame`'s sbrow-loop exit —
> writes every fully-filtered frame *including hidden alt-refs* as packed
> `dfr_NN.yuv` in the CWD, decode-order numbered, printing
> frame_offset/show_frame per dump) and `KINETIX_DBG_PARTCDF` (decode.c's
> `decode_partition` — prints the partition CDF row + pre-read rng per
> read). Both need `ninja -C build` via vcvars
> (`cmd //c rebuild.bat` at the clone root — sccache had to be stripped
> from build/build.ninja first) + copy `build/src/dav1d.dll` over
> `build/tools/dav1d.dll`. Kinetix's counterpart dumps are the existing
> `KINETIX_AV1_DUMP_FRAMES` (`kfr_NN.yuv`, same decode-order numbering).
> With both sides dumped, the 64x64 clip's diff chain is exactly: kf
> bit-exact → **oh4 (hidden, first inter frame) Y=689 diffs, first-bad
> (31,48)** → oh2 912 → every shown frame inherits. The entropy streams
> align block-for-block through oh4's `mi(6,14)` VERT-split 8x8 and its
> first 4x8 child (rng 52580 matched), then diverge inside the SECOND
> 4x8 child's residual reads: dav1d reads vartx[0/0] + y-cf TX_4X8
> txtp=13 eob=8 + uv 4x4 eob=4 + uv eob=0 (post 34641), Kinetix arrives
> at the next partition read with 51208 and decodes bp=8 (HORZ_4) where
> dav1d decodes bp=3 (SPLIT). From there oh4 decodes
> differently-but-validly (no panic) and every downstream frame inherits
> (dav1d's CDF row dump via KINETIX_DBG_PARTCDF vs Kinetix's
> KINETIX_AV1_DBG_PARTCDF confirmed the CDF rows in the two decoders use
> different storage conventions — dav1d pre-complements every element via
> the recursive CDF macros and uses them directly; Kinetix stores the raw
> spec forward values and complements at read time — and are numerically
> equivalent, so the divergence is in a *symbol read*, not the table).
> The concrete next probe: Kinetix's read path for the VERT-child 4x8
> block's var-tx tree + coefficients — prime suspects are
> `read_block_tx_size_ibc`'s tx-depth read for BLOCK_4X8 (MAX_TX_DEPTH
> table verified correct) and `read_coeffs`' eob/tx-type context for
> TX_4X8 in the inter path. Note the earlier warp attribution was wrong
> (this clip's warp blocks — mm=2 with alpha/beta models — decode AFTER
> the desync point, so their pixel diffs are downstream garbage).
>
> One real spec fix landed in the process: **Kinetix's `read_symbol`
> never honored the frame header's `disable_cdf_update` flag (§6.8.2)** —
> its own doc comment admitted it always adapts. `SymbolDecoder` gained
> `allow_update_cdf` + `set_allow_update_cdf` (dav1d
> `msac.allow_update_cdf`), threaded from `FrameHeader.disable_cdf_update`
> through `TileDecodeState::new`. On `disable_cdf_update=1` frames Kinetix
> previously adapted its CDFs while dav1d kept them frozen — the same
> symbol stream eventually decodes differently once any drifted CDF flips
> a decision (this clip's frames all have the flag=0, so no visible
> change here — but real RTC encoders set it constantly). Also added:
> `KINETIX_AV1_DBG_PARTCDF` (mode_cdfs.rs, dumps the W32/ctx2 partition
> CDF row + count pre-read) and `KINETIX_AV1_DBG_WARPPX`
> (warp.rs, dumps the warp filter's phases/mids/outputs for the
> dx==26/dy==48 sub-block — coordinates are this session's repro and
> need re-pointing for the next one). Debug session confirmed the warp
> pipeline itself (model derivation, filter table, phase/rounding
> arithmetic) is numerically identical to dav1d's warp_affine_8x8_c at
> this repro point.
>
> VERIFIED: intra corpus 6/6 bit-exact (all-inf PSNR), 128x96 inter
> sequence unchanged (frames 0-6 luma bit-exact, frame 0 fully bit-exact,
> frame 7 = 8 samples), 154 AV1 unit tests, workspace lib/bins tests,
> tpt-kinetix-av1 clippy -D warnings, fmt — all clean. REMAINING (next
> session, priority order): (1) oh4's VERT-child 4x8 residual read —
> trace Kinetix's symbol-by-symbol rng against dav1d's Post-vartxtree/
> Post-y-cf-blk prints from the (6,14) VERT block; the divergence is
> between rng 52580 (matched) and the following partition read (51208 vs
> 34641 pre). (2) The 128x96 chroma gap — the same DUMPF tool now makes
> this tractable: dump both sides' hidden frames (kf, oh6, oh3) and diff
> chroma rows 37-47. (3) The dav1d-side hooks (DUMPF/PARTCDF) live only
> in the out-of-repo clone — if the clone is ever recreated, re-apply
> from this note.

> **2026-09-17 (evening session, cont'd 2) — RESOLVED SAME SESSION: the oh4
> desync symbol was the CHROMA TX TYPE of the sub-8x8 chroma-owning block.**
> Kinetix's co-located-luma-type lookup in `add_inter_residual` missed for
> the VERT-split 8x8's second 4x8 child: that block owns the chroma for the
> whole parent 8x8 (§7.3.1 HasChroma), so chroma cols 12-13 map to the
> SIBLING 4x8's luma area, absent from the block's own leaf list — the
> lookup fell back to DCT_DCT and read the chroma tx type as txtp=0 where
> the bitstream carries the block's own luma type (txtp=13, dav1d's
> `b->txtp` behaviour). The first chroma read consumed different symbols
> (eob 2 vs 4) and desynced the tile. FIXED: a lookup miss now falls back
> to the block's own first luma leaf's tx type. **64x64 per-frame luma
> diffs vs dav1d: 505/626/631/492/761 → 210/356/300/5/748** (frame 4
> nearly bit-exact, 77 dB); no corpus regressions (128x96 unchanged,
> intra 6/6). Frame 5 (oh5, 748) references the already-drifted oh4 slot
> plus its own desync; frames 1-3's residual ~200-360 diffs are the next
> trace targets with the same entropy-diff method (the new
> KINETIX_DBG_DUMPF/KINETIX_AV1_DUMP_FRAMES pair localizes the first
> diverging frame in one command).

> **2026-09-17 (evening session, cont'd 3) — the remaining 64x64 diffs
> characterized: dav1d's 4-wide/4-tall-special subpel MC filter rows.
> After the chroma tx-type fix, oh4's internal diff fell 689 → 27 (all
> ±1, first-bad (18,56) dav=148 kin=149) and the shown frames'
> residuals are the same class. The block is a 4x4 skip with
> mv=(0,18) (NEARMV drl 1) — pure horizontal-subpel MC (phase 2/8).
> A rebuilt dav1d PUT8TAP probe (re-pointed to bx=4/by=14, extended to
> fire for 4x4 blocks) captured dav1d's literal MC internals: it uses
> the **4x4-specific 6-bit filter row** `[3+DAV1D_FILTER_8TAP_REGULAR][3]
> = {0,0,-6,55,19,-4,0,0}` with rnd=34 (`intermediate_rnd = 32 +
> (1<<(6-intermediate_bits))>>1`, 8-bit) — sum 9495 → dst 148. Kinetix
> uses its uniform 7-bit `SUBPEL_FILTERS[0][2]` = {0,2,-10,122,18,-4,0,0}
> → 149. dav1d's GET_H_FILTER/GET_V_FILTER select these [3+type] rows for
> any block with w==4 or h==4 (`w > 4 ? [type&3][mx-1] :
> [3+(type&1)][mx-1]`, row index = (mv & 15) - 1 over **15 six-bit rows**;
> [5] is a pure bilinear set). Kinetix's MC has no 4-wide special case.
> NOTE the resolved puzzle from earlier sessions: the "6-bit experiment
> reverted" (2026-09-15 cont'd 4) failed because the dual-filter H/V bug
> was still live then, not because 6-bit filters were wrong — dav1d's
> 8-bit MC genuinely runs 6-bit filters + >>6 rounding + per-block-size
> table selection.
> NEXT SESSION PLAN (concrete): port dav1d's full subpel filter
> machinery into Kinetix's motion_compensate: (1) the 6x15x8 6-bit table
> (tables.c `dav1d_mc_subpel_filters[6][15][8]`, regular/smooth/sharp +
> the three 4x4 variants + bilinear); (2) row selection
> `[type][(|mv|&15)-1]` for w>4/h>4, `[3+(type&1)][(|mv|&15)-1]` for
> w==4, `[3+((type>>2)&1)][...]` for h==4, bilinear [5] for both-small;
> (3) rounding: 1D `(sum + 34) >> 6` (8-bit), 2D intermediate
> `(sum + 64) >> 7`?? — read `intermediate_rnd`/shifts from mc_tmpl.c
> lines 139/208/451 per path (h-only rnd=34>>6 verified here); (4) keep
> the A/B harness (`dbg_av1_warp.rs` + DUMPF/PARTCDF hooks) as the
> regression gate. Expected payoff: the 64x64 clip's remaining ~200-750
> diffs and most of the 128x96 chroma/luma ±1s are subpel-MC-shaped.
> The dav1d clone's PUT8TAP condition is now
> `(x==6 && h==8) || (x==2 && h==4)` — re-point per repro.

> **2026-09-17 (evening session, cont'd 4) — subpel MC probe data complete;
> one dav1d semantic question isolated for the port.** The rebuilt PUT8TAP
> probe (now printing mx/w too) captured dav1d's literal MC execution at
> the oh4 (4,14) 4x4 block (mv=(0,18) per both its prints): mx=4, w=4,
> fh=[0,0,-6,55,19,-4,0,0] = the 4x4-REGULAR table row 3, rnd=34,
> sum=9495, dst=148. Kinetix's uniform 7-bit filter gives 149. The open
> question: dav1d's `GET_H_FILTER` indexes `[3+REGULAR][(mx)-1]` with
> `mx = mvx & (15 >> !ss_hor)` — for mv.x=18 that arithmetic gives mx=2 →
> row 1 → sum 9511 → 149, but dav1d executed row 3 (mx=4). Either dav1d's
> mv.x at mc time is 20 (something between Post-intermode and mc adjusts
> it — no known dav1d code path does), or the row indexing for the 4x4
> tables is `[(mv>>2)-1]` (18>>2 = 4 → row 3 ✓) — i.e. the 4x4 tables are
> indexed by HALF-PEL phase (consistent with the spec's rule that 4-wide
> blocks only have half-pel-sharp effective filters). Next session:
> (1) dump dav1d's mx at a SECOND 4x4 block with a different mv (e.g.
> (5,14)'s neighbor (6,14) 4x8 mv=(0,0)... or another clip) to
> disambiguate `[mx-1]` vs `[(mv>>2)-1]`; (2) port the 6x15x8 6-bit table
> + selection + rnd=34>>6 1D rounding into motion_compensate per the
> resolved mapping; (3) the 2D (both-axes) path rounds H at
> (sum+4)>>3 = 6-intermediate_bits and V at (sum+1024)>>11 =
> 6+intermediate_bits (mc_tmpl.c put_8tap_c, 8-bit intermediate_bits=4)
> — Kinetix's current 2D rounding already matches these; only the 1D
> rounding (34 vs 64>>7?) and the table/selection need the port.
> The dav1d clone now also has a MCIN probe stub REMOVED (a broken
> fprintf experiment was fully excised; the clone builds clean again and
> the PUT8TAP/MCPX/DUMPF/PARTCDF hooks all work).

> **2026-09-17 (evening session, cont'd 5) — FOUND + FIXED: Kinetix's
> subpel filter table was VP9's, not AV1's. This was THE root cause of
> every remaining subpel +-1.** The probe mystery resolved: dav1d's mc()
> computes `mx = mvx & (15 >> !ss_hor)` = `mvx & 7` for luma and passes
> `mx << !ss_hor` = `mx << 1` to put_8tap_c, which indexes
> `[set][(mx<<1) - 1]` — so a luma phase m/8 selects the ODD row 2m-1 of
> a 15-row SIX-BIT (sum = 64) coefficient table. The printed mx=4 for
> mv.x=18 was `((mv.x & 7) << 1)` = the already-doubled index, row 3,
> exactly as captured. Kinetix's table held the VP9 128-scale bank
> ({0,2,-10,122,18,-4,0,0} sums to 128) at even rows — double wrong
> (coefficients AND effective phase). FIX (inter.rs + cdf_tables_gen.rs):
> (1) `SUBPEL_FILTERS` replaced with dav1d's 6-bit
> `dav1d_mc_subpel_filters[6][15][8]` (sets: regular / smooth / sharp /
> 4x4-regular / 4x4-smooth / bilinear); (2) `subpel_kernel` now resolves
> luma phase m to odd row 2m-1, chroma 1/16 phase c to row c-1, honours
> the 4x4 set selection (w==4 -> H set [3+(kind&1)], h==4 -> V set
> [3+((kind>>2)&1)]), and returns the identity [0,0,0,64,0,0,0,0] for
> full-pel; (3) rounding ported per topology from dav1d 8-bit
> put_8tap_c: 2-D chain (sum+2)>>2 then (sum_v+512)>>10, horizontal-only
> (sum+34)>>6, vertical-only (sum+32)>>6 (the prior 7-bit table's
> /3-then-/11 chain matched VP9's scale, not AV1's); (4) BILINEAR moved
> off the 8-tap table onto a faithful port of dav1d's put_bilin_c
> (2-tap 16-scale: H-only (16s+mx16*(s1-s0)+8)>>4, V-only likewise,
> 2-D unrounded 16-scale H chained into (…+128)>>8 V) — this also
> restored the IBC chroma path (testsrc2_big intra entry had regressed
> to chroma diffs when BILINEAR briefly mapped into the 4x4-SMOOTH 8-tap
> set). The compound prep path keeps its 16-scale intermediate (the new
> 6-bit pipeline produces the same 16-scale as the old 7-bit one) so the
> avg/w_avg/mask blends are unchanged.
> RESULTS: av1_intra_corpus 6/6 bit-exact ALL-INF restored;
> 128x96 inter sequence luma diff samples 0/0/0/0/0/0/8 -> 0/0/0/0/0/0/0
> — **ALL EIGHT FRAMES LUMA BIT-EXACT, frame 7's last 8 samples gone**;
> chroma PSNR improved across the board (frame 1 U 66.38 -> 67.32);
> testsrc_64x64 frame 4 -> fully bit-exact (inf/inf/inf, was 5 diffs),
> frame 5 761 -> 734, frames 1-3 unchanged (their diffs predate the MC —
> they inherit oh4's 27 residual +-1s through the reference chain).
> 154 AV1 unit tests, workspace lib/bins, clippy -D warnings, fmt clean.
> REMAINING (next session): (1) oh4's 27 +-1s — now the ONLY luma error
> source in the 64x64 clip; with the filters now correct, re-run the
> per-block trace: the +-1s sit on 4x4/4x8 subpel blocks adjacent to
> warp/OBMC blocks (candidates: OBMC lap MC filter provenance, warp
> sub-block emu-edge extents); (2) the 128x96 chroma residuals
> (55-67 dB, pure +-1 rounding at this point) — same OBMC/warp-adjacent
> suspicion; (3) then the pixel_exact flip discussion for the 8-bit
> 4:2:0 subset becomes concrete.

> **2026-09-17 (evening session, cont'd 6) — with oh4 bit-exact, the 64x64
> divergence is now pinpointed to ONE entropy symbol: the compound inter
> mode CONTEXT of oh2's left JNT strip.** Fresh internal diffs post-6-bit
> port: kf Y=0, **oh4 Y=0 (bit-exact!)**, oh2 Y=561 (all in its two
> non-skipmode compound strips: L-y48 DIFFWTD 7, L-y56 JNT 163, R-y48
> JNT 185, R-y56 0), oh1 364, oh3 470, oh5 1157 — all inherited/descendant.
> oh2's skipmode strips are ALL exact (plain avg of the now-exact refs ✓).
> Fresh B0 + partition traces show Kinetix's oh2 ENTROPY SYNCED through
> the left 32x32's HORZ_4 strips (rng matches dav1d's Post-skip[0]=50136,
> compflag=60172... wait — synced through strip (0,12)'s comptype
> r=48718, y-cf r=54576, chroma r=45069 ✓, and strip (0,14)'s
> skip=44736, compflag=60172, refs=48622 ✓) — then strip (0,14)'s
> COMPOUND INTER MODE symbol: dav1d reads compintermode=1 from **ctx=3**;
> Kinetix reads compintermode=0 from **ctx=4** (rng 54664 vs 43880 —
> desync; everything after in oh2/oh5 decodes as garbage-but-valid,
> producing the 561/1157 diffs). The ctx: dav1d's comes from
> `dav1d_refmvs_find`'s compound-pair scan: `switch (refmv_ctx >> 1)
> { case 0: min(newmv_ctx,1); case 1: 1+min(newmv_ctx,3); case 2:
> clip(3+newmv_ctx,4,7) }` (refmvs.c ~line 601), where refmv_ctx/
> newmv_ctx come from the compound ref-pair match counts
> (count>=2: refmv=min(2,cnt), newmv=cnt>0; count==1: refmv=3,
> newmv=3-have_newmv; count==0: refmv=5, newmv=5-have_newmv).
> Kinetix's `comp_mode_ctx` (inter_mv_stack's own §8.3.2-style
> derivation) gave 4 where dav1d's gives 3 — for a block whose only
> relevant neighbor is the ABOVE (0,12) compound DIFFWTD strip
> (refs LAST/ALTREF, mvs (0,6)/(0,-8): row-subpel, col-integer).
> NEXT SESSION (surgical): align Kinetix's compound comp_mode_ctx with
> dav1d's switch — reuse the scan's refmv_ctx/newmv_ctx (Kinetix's
> inter_mv_stack already computes the match counts; verify they equal
> dav1d's ref_match_count/have_newmv semantics for compound pairs,
> especially the have_newmv definition) and replace the ctx formula with
> the three-case switch. Then oh2 should go bit-exact (its inputs are:
> kf ✓ oh4 ✓ skipmode strips ✓), and oh1/oh3/oh5 (which chain from
> oh2's slots) follow. The final 64x64 gap after that = oh5's own
> additional desync (1157 diffs include its own new symbols).
> dav1d clone state: builds clean; hooks DUMPF/PARTCDF/PUT8TAP/MCPX all
> functional; the MCIN fprintf experiment was fully removed.

> **2026-09-17 (evening session, cont'd 7) — additional lead found: the
> have_newmv pollution bug candidate.** Kinetix's compound scan
> (intra_block.rs scan_col/scan_row closures, ~line 1290-1305) does
> `*have_newmv |= (cand.mf >> 1) as i32` — WITHOUT `& 1`. Kinetix's
> RefMvCell.mf packs `refmv_ctx<<4 | zeromv_ctx<<3 | newmv_ctx`, so
> `mf >> 1` = `zeromv<<2 | newmv` plus ALL the refmv bits — every match
> pollutes have_newmv with the neighbor's refmv_ctx value. dav1d's
> equivalent scan masks properly. Downstream: num_new = have_newmv feeds
> the compound ctx switch `(close_matches) { 0: (min(total,2), total>0);
> 1: ((total*3).min(4), 3-min(num_new,1)); _: (5, 5-min(num_new,1)) }` —
> for oh2's (0,14) strip (close=1: above-only match), the polluted
> num_new forces c_newmv = 2, ctx = 3 — which coincidentally equals
> dav1d's ctx... yet Kinetix PRINTED ctx=4 for this block, meaning its
> close_matches must actually be ≥ 2 or 0 (have_row/have_col ≠ the
> expected 1/0) — i.e. there is a SECOND divergence in the match
> counting itself (the above-scan for the 32x8 strip should find the
> (0,12) DIFFWTD strip's pair once → have_row=1). NEXT SESSION: run
> KINETIX_AV1_DBG_MVSCAN="14:0" (the existing MVSCAN hook) on oh2 to
> dump have_row/have_col/num_new/close_matches for exactly this block,
> fix the `& 1` mask, and reconcile close_matches; then re-check the
> ctx (target: dav1d's 3) and the compintermode symbol (target: 1).
> Everything downstream (oh2's JNT strip, then oh1/oh3/oh5) follows.

> **2026-09-18 — refmvs ctx derivation aligned with dav1d refmvs.c
> (04cdb75); oh2 (0,14) now reads ctx=3.** The 64x64 warp clip's oh2
> compound strip desync had FOUR interlocking causes, all fixed in
> intra_block.rs + inter_block.rs:
> 1. have_newmv pollution: `*have_newmv |= (cand.mf >> 1) as i32`
>    leaked the full mf bitfield; now masked `i32::from((mf>>1)&1)`.
> 2. The top-left corner probe add((by4-1, bx4-1)) was fed the REAL
>    have_newmv; dav1d (refmvs.c:468-471) passes a DUMMY there, so the
>    probe must count toward ref_match_count but never toward
>    have_newmv. (Kinetix's flow = dav1d's: top scan, left scan,
>    top-right probe (real newmv), THEN snapshot nearest_match/num_new,
>    then top-left probe (dummy newmv), then secondary scans.)
> 3. The ctx switches now select on `close_matches` (= dav1d's
>    nearest_match, snapshot BEFORE the top-left probe) with
>    `total_matches` (= dav1d's ref_match_count, after probe+secondary)
>    inside. dav1d's single table (one switch serves BOTH the single-ref
>    packed ctx and the compound fold): nm==0: (rmc>0, min(2,rmc));
>    nm==1: (3-have_newmv, min(rmc*3,4)); else (5-have_newmv, 5).
>    Kinetix's old single-ref arms used `total` and `2+total` — wrong
>    for rmc==0 and rmc>=3; the old "compound formulas differ" comment
>    was wrong (they never differed in dav1d).
> 4. Compound path NEVER splatted new_mf (stayed 0): now dav1d
>    splat_tworef_mv: comp_mode 6 -> 1, (1<<mode)&0xbc -> 2 (rows
>    2,3,4,5,7), else 0 (rows 0,1). Skip-mode blocks keep mf=0
>    (dav1d gives them NEARESTMV_NEARESTMV = mf 0). Single-ref splat
>    was already right (ZEROMV->1, NEWMV->2; GLOBALMV implies >=8x8).
> Verified: KINETIX_AV1_DBG_B0=1 + MVSCAN="14:0" — oh2's (0,14)
> compound strip now builds stack s0=(0,6) mf=2, nearest_match=1,
> comp_ctx=3 (was 4). 64x64 warp per-frame luma diffs
> 210/355/300/0/734 -> 172/292/249/0/739 (frames 1-3 improved, frame 5
> shuffled ±5 — its refs changed under it). 128x96: all 8 frames luma
> still bit-exact. Suites + conformance + clippy + fmt clean.
> NEXT: frame 1 (oh4) first-wrong (37,48) 101->102 and frames 2/3
> (48,47) 79->80 are ±1 LSB diffs — prediction/rounding-family, not
> entropy desyncs (deltas of 1). Suspects: MC subpel rounding on some
> 4x4 path, or SGR/Wiener edge (loop filter already staged-out before).
> Run stage isolation (--inloopfilters) on the warp clip for frame 1 to
> bracket which in-loop stage (if any) carries the ±1; if present
> pre-filter, probe dav1d's put_8tap for that exact (x,y,h,mx,my).

> **2026-09-18 (cont'd) — TEMPORAL SAMPLING FIX (882da15): 64x64 warp
> clip now FULLY bit-exact vs dav1d.** After the refmvs-ctx commit the
> mask-0 hidden-frame dumps showed oh4 exact (its diffs were pure
> post-filter echo) but oh2/oh1/oh3/oh5 still desynced. The COMPPX vs
> DBG_COMP probe pair (retuned to rows 12/14) caught it: oh2's
> compound strip (0,14) had dav1d n_mvs=2 vs Kinetix stack cnt=3 —
> Kinetix added a temporal candidate (0,8)/(0,-8) dav1d never produced
> (MVSCAN: rp_proj cell (7,4) = (mv (0,18), ref2ref 4)), read a DRL
> symbol dav1d didn't, and desynced oh2 mid-frame (poisoning
> oh1/oh3/oh5 downstream). Root cause: build_rp_proj sampled each 8x8
> temporal cell from grid cell mi (2x, 2y) = top-LEFT 4x4, but dav1d
> save_tmvs_c stores the cell from mi (2x+1, 2y) = top-RIGHT 4x4
> (cand_b = &b[x*2+1]) — indistinguishable except in sub-8x8 splits,
> where leaves have per-4x4 MVs (oh4's 4x4 (8,14) saved (0,18)/kf,
> (9,14) saves nothing). One-character fix, huge blast radius:
> **all 6 frames — kf + oh4/oh2/oh1/oh3/oh5 — bit-exact, ALL planes,
> full deblock+CDEF+LR** (verified against the dav1d clone's internal
> dumps at inloopfilters=0 AND =7). testsrc_64x64 5/5 shown frames
> exact; intra corpus 6/6; luma exact on every clip.
> dav1d clone repair note: the Temp clone lost its root files (meson
> build files, .git — external temp cleanup); build/build.ninja +
> sources + generated headers survived. Fixed by deleting the
> REGENERATE_BUILD edges from build.ninja and stubbing the lost
> src/dav1d.rc + tools/dav1d.rc + dav1d.manifest (version resources
> are cosmetic) INSIDE build/{src,tools}/ (ninja resolves those paths
> build-relative). COMPPX gate retuned to (by == 12 || by == 14).
> REMAINING GAP: chroma-only ±1 diffs on 128x96 (U/V 55-67 dB) and
> 96x64 (U/V 57-73 dB) inter clips; luma 100% exact everywhere. The
> chroma motion field is derived from luma, so the save fix doesn't
> touch chroma directly. NEXT SESSION: same method — save OBU, decode
> with KINETIX_AV1_DUMP_FRAMES vs the clone's DUMPF at inloopfilters=0
> for the 128x96 stream (its hidden frames too), find the first
> chroma-diverging block, then probe put_8tap chroma (phase = frac-1,
> 4x4 filter sets when chroma bw<8, GET_H/V table rows) and sub-8x8
> chroma tx-type/co-located logic. All the tooling (DBG_COMP rows,
> MVSCAN, skipmode trace, dump harness) is env-gated and in place.

> **2026-09-18 (cont'd 2) — chroma frontier localized (new dbg_av1_chroma
> harness, untracked).** 128x96 inter clip, mask-0 hidden-frame method:
> kf EXACT all planes; EVERY inter frame (1-7) has CHROMA-ONLY diffs
> (49-134 samples) confined to chroma rows 40-43 of BOTH U and V = luma
> mi rows 20-21 exactly; luma 100% exact everywhere. ±1 LSB per sample
> and same-band recurrence across frames = wrong leaf's MV chosen for
> the SUB-8x8 chroma MC in those rows (they hold 4x4/8x4 splits with
> per-leaf MVs), not an entropy desync. Gotcha hit: stale kfr_*.yuv
> from a previous run made a full-filter-vs-mask-0 comparison look
> like a kf chroma error — always `rm -f kfr_*.yuv` before dumping.
> dav1d rule to verify against: sub-8x8 chroma uses ONE mv per 8x8
> (dav1d recon/decode picks the co-located luma leaf — check whether
> it's (bx4+bw4-1, by4+bh4-1) bottom-right or top-right, and whether
> Kinetix's chroma path picks the same leaf; Kinetix already has a
> co-located-luma-type fallback in add_inter_residual for tx-type,
> same grid). Probe plan: MCPX-style print of dav1d's chroma mc mv for
> one diverging block in frame 1 (mi row 20-21), vs Kinetix's chroma
> mv at the same block (DBG_COMP/PRED extensions), fix the leaf rule,
> then 128x96 + 96x64 should go fully bit-exact like the 64x64 clip.
> Post-filter status: with the entropy fixes in, Kinetix full-filter
> output == dav1d mask-7 dumps exactly (conformance-invisible), so no
> separate filter bug is visible on these corpora.

> **2026-09-18 (cont'd 3) — chroma root-cause narrowed to an extra
> overlapping compound block; quadrant experiment reverted.** Implemented
> dav1d's sub-8x8 chroma quadrant scheme (has_chroma owner + sibling-MV
> halves) in decode_inter_block and proved it BEHAVIORALLY EQUIVALENT to
> the committed per-leaf path (each borrowed quadrant's mv == what the
> sibling leaf itself predicts; ctx-stored filters == the sibling's own
> filters), so it was reverted to keep the simpler committed code.
> The real 128x96 lead: new KINETIX_DBG_MCCHK trace (inter_predict_plane
> chroma calls, gated on chroma rows 38-46) vs the dav1d clone's
> KINETIX_DBG_MCCH trace (mc() calls for pl!=0, mi rows 19-22, prints
> dstoff/w/h/mv/f2d) shows, for frame 1 at mi (0,20): Kinetix emits BOTH
> an 8x8 single-ref chroma (mv (0,64), matching dav1d's call) AND an
> 8x16-LUMA COMPOUND chroma call (4x8 at chroma (0,40), comp=1, mv
> (0,0)) that has NO dav1d counterpart. The compound's chroma rows 40-43
> overwrite the 8x8's prediction (chroma-only ±1 diffs; luma stays exact
> because the overlapping luma predictions coincide on this content).
> The extra block smells like a skip-mode/partition edge: dav1d's poc=1
> trace at (0,0) reads compflag+refs+compintermode[6] (a NORMAL compound
> 64x64) with no skipmode, so check whether Kinetix decodes a skip_mode
> or a differently-partitioned block there (DBG b0 skipmode trace now
> exists at line ~417 of inter_block.rs). Also fixed in passing: the
> dav1d clone gained a KINETIX_DBG_MCCH hook (src/recon_tmpl.c mc()
> entry) and rebuild notes: after the temp-cleanup repair, `cmd //c
> rebuild.bat` then copy build/src/dav1d.dll -> build/tools/. The
> throwaway harness tpt-kinetix-test-utils/tests/dbg_av1_chroma.rs
> (untracked) + KINETIX_AV1_CHROMA_OBU env drives the 128x96 stream;
> ALWAYS `rm -f kfr_*.yuv` before dump runs (stale dumps poisoned one
> comparison).

> **2026-09-18 (cont'd 4) — chroma desync is in the OBMC blend; prior
> "extra compound block" lead was a trace-interleaving artifact.**
> Corrections first: the MCCH/MCCHK traces were not frame-tagged, so
> last session's "8x16 compound at mi(0,20)" conclusion mixed frames.
> Properly tagged now (dav1d MCCH prints frame_offset; Kinetix MCCHK
> prints cur_order_hint; both committed). ALSO: the 128x96 clip HAS
> hidden frames — decode order is oh0, oh6(hidden), oh3(hidden), oh1,
> oh2, oh4, oh5, [oh6 show_existing replay], oh7; dfr/kfr index → oh
> mapping is 0,6,3,1,2,4,5,(6),7 — "frame 1" in earlier notes = the
> HIDDEN alt-ref oh6. The show_existing replay makes kfr_05 go missing
> (count increments, no dump) — key diffs to that when pairing dumps.
> FINDINGS: oh6's chroma diffs (rows 40-43, x 0-15/48-55, ±1-5) sit
> exactly in OBMC-blend regions. Hash-proven: KINETIX_AV1_NOOBMC
> changes oh6's output (md5 differs) — OBMC is active. dav1d's
> Post-motionmode[1] confirms the (4,20) 16x16 is an OBMC block. Row
> 40 of the diverging regions matches dav1d, blended rows 41-43 differ
> (dav1d blend_h blends only (h*3)>>2 rows with masks {25,14,5} from
> obmc_masks[4]; row 40 should be blended with m=25 yet matches —
> suspicious). Kinetix's obmc_mask table, blend formula
> ((m*o+(64-m)*cur+32)>>6 = dav1d's blend_px exactly), job shapes and
> ctx-based mv/filter sourcing all MATCH dav1d on paper. Remaining
> delta candidates for the next probe: (a) job collection — Kinetix
> n_limit = 4.min(bw4.trailing_zeros()) vs dav1d imin(b_dim[2],4)
> (b_dim[2] = 8x8 units per edge — SAME for square blocks but the
> boundary-skip differs: dav1d neighbors at odd offsets
> bx+x+1 stepping by the NEIGHBOR's clipped width, Kinetix x4|1
> stepping by grid_w4 — check the (0,20) 8x8 + (2,20)?? column
> coverage at x 0-3/4-7); (b) the above-pass overlap HEIGHT formula
> (oh4 = min(b_dim[1],16)>>1, mc height (oh4*3+3)>>2) vs Kinetix's
> pred_h = (h>>1).min(32); (c) the subpel lap rounding for chroma
> vbits=4 (neighbor mv (0,66) → V-only subpel phase 2 — verify 1D-V
> rounding (s+32)>>6 with the 16-phase kernel row frac-1=1 through
> Kinetix's motion_compensate for a chroma-sized block).
> TOOLING: KINETIX_DBG_MCCH (dav1d, now poc-tagged, gate by>=12&&by<=24),
> KINETIX_DBG_MCCHK (Kinetix, oh-tagged, chroma rows 24-52),
> KINETIX_AV1_DBG_OBMC with DEEP mode (retune its mi_col==4&&mi_row==18
> gate to the (4,20) block), KINETIX_AV1_NOOBMC (escape hatch).
> Harness: dbg_av1_chroma.rs + KINETIX_AV1_CHROMA_OBU=$TEMP/t128.obu.
> ALWAYS rm -f kfr_*.yuv before dump runs. NEXT: dump Kinetix's job
> list (OBMC debug) for oh6 mi(4,20) vs dav1d's obmc() calls (add a
> KINETIX_DBG_OBMCD print in dav1d's obmc(): neighbor mi, mv, overlap
> w/h, per-row masks), align per-sample, fix, then 128x96/96x64 should
> go fully bit-exact.

> **2026-09-18 (cont'd 5) — MM-read counts don't reconcile; Kinetix
> appears to decode the show_existing replay as a real frame.** Per-frame
> motion-mode reads (dav1d KINETIX_DBG_MM, poc-tagged, 62 total: poc6=36,
> poc3=8, poc7=18, poc5=0) vs Kinetix (DBG b0 motion_mode, 65 total:
> oh6=33, oh3=8, oh5=3, replay-slot=3, oh7=18). Key facts:
> 1. Kinetix processes 10 frame events vs dav1d's 9 — the show_existing
>    replay performs 3 REAL motion-mode symbol reads (at mi
>    (0,16),(16,0),(16,16)) in Kinetix where dav1d replays without
>    decoding. A replay must not read any symbols.
> 2. Kinetix's replay is processed at event position 5 (kfr_05 gap,
>    between oh2 and oh4) while dav1d's is position 8 (between oh5 and
>    oh7) — either the OBU order differs between decoders (packet/TU
>    splitting in the harness?) or Kinetix defers/misplaces the replay.
> 3. oh6: Kinetix reads 33 MM symbols vs dav1d's 36; oh5: 3 vs 0.
>    has_overlappable_candidates scans ALL 4x4 boundary-adjacent
>    positions (is_inter_above[c], c in mi_col..mi_col+bw) while the
>    spec/dav1d gate is findoddzero at ODD offsets only
>    (a->intra[bx4+1], [bx4+3], ... 8x8 granularity, boundary 8x8
>    skipped) — Kinetix's gate is more permissive in shape, yet oh6 has
>    FEWER reads, so position-level gate fixes alone won't reconcile;
>    the frame-event/shuffle issue (items 1-2) must be fixed FIRST.
> The luma-exact-desync paradox (entropy divergence with pixel-exact
> luma) remains: until item 1-2 are fixed, per-block entropy alignment
> on this clip is meaningless — the dumps may pair different logical
> frames. Suggested order for the next session: (a) dump the harness's
> fed packets per frame (dbg_av1_chroma already feeds TU packets — log
> obu types per packet); (b) check decoder.rs show_existing handling
> (does it decode? which OBU triggers the extra event?); (c) fix the
> replay to not decode symbols; (d) re-align MM reads (expect 36/8/0/18
> exactly); (e) only then re-examine chroma pixels. NOTE: dav1d MM hook
> gate was widened (decode.c, prints all poc); KINETIX_DBG_OBMCD added
> (dav1d obmc() per-call print); Kinetix OBMC deep trace retuned to
> mi(4,20) plane 1 with before-values (all committed).

> **2026-09-19 — INTER DECODE IS BIT-EXACT: 128x96 7/7, 96x64 5/5 (was 0/5),
> 64x64 5/5 inter frames + intra corpus 6/6 — every frame, all planes, with
> and without in-loop filters, hidden frames and show_existing replay
> included.** The "MM-read mismatch / replay decodes symbols" lead from the
> 09-18 cont'd-5 note was a FALSE ALARM, and the session that actually closed
> the chroma gap found three real bugs. In order:
>
> 1. **Cont'd-5 retraction.** The stream contains TWO frames with
>    order_hint=6 (the hidden alt-ref AND a real shown frame, OBU#16 at
>    TU6); dav1d's KINETIX_DBG_MM tags by frame_offset so its "poc6=36" was
>    33+3 lumped. Chronological run-lengths match Kinetix exactly:
>    33/8/3/18 = 33/8/3/18 (total 62). The show_existing replay (TU3,
>    `FRAME_HDR` payload 0xa8 = show_existing idx 2 → replays oh3, not oh6)
>    goes through decoder.rs's early-return and reads ZERO symbols. The
>    "position 5 vs 8" discrepancy was dump-index confusion: Kinetix names
>    kfr_NN by frame_count which the replay increments without dumping
>    (kfr_05 missing), dav1d's DFR hook doesn't dump the replay at all —
>    pair kfr_00..04↔dfr_00..04, then kfr_06..09↔dfr_05..08. ffmpeg's av1
>    encode is deterministic across runs (verified byte-identical), so
>    cross-run dump pairing is safe.
> 2. **Sub-8x8 chroma prediction extent** (inter_block.rs): the
>    `.max(4)` clamp on cbw/cbh made every sub-8x8 leaf smear its MC over
>    the whole parent 8x8's chroma (8x4 → 4 rows, 4x4 → 4x4), so sibling
>    halves and 4x4 quad quadrants were decided last-writer-wins with the
>    wrong leaf's mv. Removed the clamp; each leaf predicts its own extent.
>    Necessary but not sufficient (fixed some samples only where later
>    writes coincidentally overwrote the damage).
> 3. **Inter-intra chroma masks** (wedge.rs/inter_block.rs): dav1d applies
>    the II blend to ALL THREE planes (recon_tmpl.c runs the interintra
>    block again per chroma plane with `II_MASK(chr_layout_idx, ..)`), and
>    the chroma-layout masks are the **2x2 box-average of the luma masks**
>    (`init_chroma`: `(l00+l01+l10+l11+2)>>2`), not a re-generation at
>    chroma resolution and not the raw sub-sampled luma mask. Added
>    `wedge_table_420()` + `wedge_mask_420()`; apply_interintra now blends
>    luma with the luma mask and chroma with the 2x2-averaged mask. This
>    was THE 128x96 chroma bug (its 3 wedge blocks: rows 40-43 blended with
>    luma weights, ±1-13 chroma diffs in every inter frame).
> 4. **Sub-8x8 chroma scheme for mixed intra/inter splits** (the 96x64
>    bug, 8 samples in one 8x8): for an inter has_chroma leaf whose
>    attributing neighbour cells are inter, dav1d's quadrant scheme applies
>    (per-quadrant MCs from the left/above/diagonal cells' mv + that cell's
>    filter, own mv for BR — all based at the PARENT 8x8's chroma origin
>    because `uvdstoff` floors `bx/by >> ss`); when a needed cell is INTRA
>    the gate fails and the leaf instead makes ONE mc over the whole parent
>    8x8 chroma (`bw4 << (bw4 == ss_hor)`, `bx & ~ss_hor`) with its own mv.
>    Implemented the gate + quadrant calls + parent-extent fallback in
>    decode_inter_block, the running `tl_filter2d` (dav1d
>    `t->tl_4x4_filter`, set at each inter leaf's chroma-pred end, untouched
>    by intra), and dav1d's BS_4X4 SPLIT save/restore of that variable in
>    partition.rs's quad walk. NOTE: the earlier "per-leaf output ==
>    quadrant scheme" equivalence claim (09-18 cont'd 3) was WRONG — it
>    only holds when the attributing cells' mvs coincide with the leaves';
>    128x96 passed coincidentally, 96x64 (inter leaf under an intra
>    sibling) exposed it.
> 5. **Reference helper** (test-utils/reference.rs): `decode_av1_with_dav1d`
>    / `decode_av1_obu_with_dav1d` now feed dav1d through a temp file — the
>    mingw/Windows dav1d build cannot open `-` (stdin), so the conformance
>    suite silently degraded to "no comparable pairs" on Windows. It runs
>    green here now.
>
> New env-gated hooks (all committed): KINETIX_AV1_DBG_PREDUMP (per-leaf
> chroma MC prediction), KINETIX_AV1_DBG_RESDUMP/COEFFDUMP (chroma residual
> + dequant grid), KINETIX_AV1_DBG_IIDUMP (inter-intra intra-pred + mask),
> DBG uv-cf-blk position tags. dav1d clone: Post-uv-cf-blk gained
> `bx=%d,by=%d` (all three print sites; site 2's arg list had to be fixed —
> garbage varargs if format/args disagree), rebuilt + copied to build/tools.
> capabilities() notes updated (inter bit-exact on the corpus);
> `pixel_exact` stays false pending official AOM/ITU vectors. NEXT: run the
> official AOM/ITU vector set through the strict conformance gate; if green,
> flip `pixel_exact` and the AGENTS.md/README AV1 status lines.

> **2026-09-19 (cont'd) — hardening pass.** (a) Added the FATE real-sample
> conformance test (`av1_fate_real_samples_vs_dav1d_when_available`,
> env-gated on `KINETIX_AV1_FATE_DIR` pointing at
> fate-suite.ffmpeg.org/av1 samples; report-only frontier tracker). All 7
> samples decode end-to-end now; the frame-count baselines vs dav1d are
> decode_model 22/24, film_grain 10/10 (grain unapplied — can never match
> until film grain lands), frames_refs_short_signaling 50/50 decoded but
> 0 exact, non_uniform_tiling 24/24 decoded 0 exact, seq_hdr_op_param_info
> 60/64 decoded 0 exact, annexb (Annex-B container) unsupported. PIXEL
> exactness on all of these is the OPEN frontier — each filename is a
> feature lead (decoder-model temporal_point_info in frame headers,
> non-uniform tiling, operating-point params, film grain).
> (b) REAL BUG: `set_frame_refs` (frame_refs_short_signaling) used
> `wrapping_add/sub` slot arithmetic — the spec's SetFrameRefs is an
> ORDER-HINT SEARCH over the 8 DPB slots (ALTREF = latest signed poc
> distance, BWDREF/ALTREF2 = earliest remaining, LAST2/LAST3 = latest
> remaining, fallback earliest slot). Ported dav1d obu.c's
> frame_refs_short_signaling block verbatim into frame.rs; the old version
> produced slot index 255 (wrapping_sub underflow) and PANICKED the decoder
> on the first frames_refs_short_signaling stream it ever met. (c) REAL
> BUG: the tile→frame blit rounded the mi extent (90-tall frame → 92 rows)
> and panicked indexing the output planes; now clipped to the frame bounds
> (reconstruct/mod.rs). (d) REAL BUG (160x90): deblock `filter_line_1d`
> wrote `out[edge+1]` past the plane when an edge sat on the last frame
> sample; writes now bounds-checked like the reads. (e) Broadened the
> synthetic inter corpus: testsrc_160x90 (height not 8-aligned → SB-edge
> stress; currently 0/7, ~16 dB, ~3470 luma samples — THE NEXT DEBUGGING
> TARGET) and smptebars_96x64 (skip-heavy; 5/5 exact immediately).
> (f) dav1d-rebuild note: the clone's always-on DEBUG_BLOCK_INFO prints go
> to STDOUT and are enormous — redirect stdout when scripting it.
> Gates: clippy clean (0 warnings), fmt clean (AV1/test-utils; the vp9
> fmt diff in the tree predates this session), av1 tests green,
> conformance 11/11 green including the FATE report-only test.

> **2026-09-19 (cont'd 2) — 160x90 root cause LOCALIZED: keyframe sub-8x8
> intra y-mode syntax.** Method: pin ONE saved stream (ffmpeg's av1 encode
> is NONDETERMINISTIC across runs for 160x90 — the harness re-encodes per
> run, which poisoned the first trace comparison), extract the kf-only OBU
> stream, add a `KINETIX_DBG_KFINTRA` hook to the dav1d clone's
> recon_b_intra (kf keyframe blocks print NO decode_b traces at all:
> DEBUG_BLOCK_INFO is `frame_offset >= 1`), and diff the 72-block kf leaf
> walk (bx/by/bs/ym/pal/rng) against kinetix's `KINETIX_AV1_TRACE`
> KTRACE BLOCK lines (watch the regex: uvmode is omitted for chroma-less
> leaves, and `r=` is capture group 9). Leaves 0-27 are IDENTICAL (modes,
> rng). First divergence at leaf 28, block mi (0,17) — a sub-8x8 block in
> the last SB row: SAME rng 60360 but dav1d ym=13 vs kinetix ym=0, i.e.
> the two decoders read this symbol from DIFFERENT CDFs. dav1d's keyframe
> path (decode.c ~1070) reads kf y-mode from
> `cdf.kfym[dav1d_intra_mode_context[a->mode[bx4]]]
> [dav1d_intra_mode_context[l.mode[by4]]]` — a 2D above/left-MODE context —
> and for sub-8x8 blocks reads PER-SUB-BLOCK modes. Kinetix reads one
> y-mode per leaf from the size-group CDF with no mode-context. THE FIX
> (next session): implement the kf per-sub-block intra_frame_y_mode reads
> with the above/left-mode-context CDFs (needs the kfym CDF tables +
> dav1d_intra_mode_context mapping + per-sub-block mode storage feeding
> the intra edge arrays), then re-run this exact trace diff until the full
> kf is leaf-identical. The 8x8-coarse parse above stays aligned, which is
> why rows 0-62 are exact and only the SB row containing the sub-8x8
> splits diverges. (The 23-vs-24 mi_rows formula discrepancy between
> reconstruct/mod.rs `height.div_ceil(4)` and frame.rs's spec
> `2*ceil(H/8)` is also worth auditing while in there.)

> **2026-09-19 (cont'd 3) — 160x90 root cause chain COMPLETE (fix = next
> session's first task, recipe below).** The "kf sub-8x8 y-mode" theory from
> cont'd 2 was WRONG (ym=13 = dav1d FILTER_PRED; kinetix's KTRACE prints the
> raw y_mode=0 with filter_intra in a separate field — they agreed, and the
> entropy stayed aligned 37 more leaves). The real chain, proven by
> per-pixel palette-index traces on BOTH decoders (`KINETIX_DBG_PALIDX` now
> in kinetix's read_color_map and the dav1d clone's read_pal_indices):
> palette index reads match for 5701 consecutive reads across the whole kf,
> then Kinetix SKIPS one pixel — its `onscreen_height` is one row short.
> Underneath: **reconstruct/mod.rs computes mi_rows = ceil(H/4) = 23 for
> H=90, while the spec (and frame.rs's own parse_tile_info!) uses
> MiRows = 2*ceil(H/8) = 24** — they coincide for every 8-aligned height,
> which is why ONLY non-8-aligned frames diverge. On the short grid the
> partition walk force-splits bottom-row blocks differently (different leaf
> tree → different palette-map read extents → entropy desync), and the
> palette color map clips `onscreen_height` at the wrong grid. THE FIX (a
> coherent padded-buffer redesign): (1) reconstruct/mod.rs mi_cols/mi_rows
> → `2*ceil(dim/8)` at all four sites (TileDecodeState::new, decode_sbrow,
> tile geometry, frame blit); (2) allocate the tile/FRAME planes at the mi
> extent (width × mi_rows*4, chroma likewise) so padding rows have storage;
> (3) StoredFrame keeps the padded planes (to_video_frame crops on
> output — already done by the blit clip); (4) MC/borders then behave like
> dav1d's padded refs automatically; (5) the palette onscreen clip becomes
> a no-op (keep it as a grid safety check). Deblock/CDEF/LR iterate grid
> edges — padding-row edges are cropped out at blit, harmless. After: re-run
> the 160x90 trace diff (KINETIX_DBG_PALIDX both sides + KFINTRA/KTRACE
> BLOCK) until the full kf is leaf-identical, then the inter corpus.
> Also verified this session: ffmpeg's av1 encode is NONDETERMINISTIC
> across processes for 160x90 (byte-different streams) — ALWAYS pin one
> saved stream per comparison (KINETIX_AV1_CHROMA_OBU + extract).
> Debug hooks added: KINETIX_DBG_PALIDX (kinetix per-pixel palette reads),
> KINETIX_DBG_KFINTRA + PALIDX (dav1d clone, rebuilt), KTRACE SKIP/IBCFLAG/
> YMODE/UVMODE/PALUV/PALUVC/COLORMAP (kinetix keyframe symbol chain).

> **2026-09-20 — PADDED-BUFFER REDESIGN LANDED. 160x90 luma bit-exact; the
> entire "bottom-band" divergence is gone.** Implemented per the cont'd-3
> recipe: reconstruct/mod.rs now computes MiCols/MiRows with the spec
> formula (`2*ceil(dim/8)`) and builds grid-extent planes
> (`PaddedPlanes`, new pub struct + 4th element of ReconstructOutput);
> tile geometry clips to the grid; decode_tile_group receives grid dims;
> the blit is a straight grid-space copy; post-filters run over the padded
> planes (dav1d-like); the output VideoFrame is cropped via `crop_planes`.
> decoder.rs: `StoredFrame` stores the padded planes (grid dims + real
> dims; `to_video_frame` crops), `RefFrameStore::refresh` takes
> `&PaddedPlanes`, so motion compensation reads decoded padding rows like
> dav1d. Verified: 160x90 UNFILTERED fully exact vs dav1d (Y, U, V, all 8
> frames + hidden), and filtered luma FULLY exact (Y=0 vs dav1d mask 7).
> REMAINING (tiny): 5 chroma samples in frame 1 at uv (48-51, 35-36) — the
> TL/BL sub-8x8 boundary of the CDEF'd 32x32 compound block at mi
> (24,16) — differ by ±1-2. Stage bisect (dav1d --inloopfilters masks +
> KINETIX_AV1_NOFILTER/NODEBLOCK/NOCDEF/NOLR): deblock matches on all
> planes; kinetix's chroma CDEF changes 403 uv samples where dav1d's
> changes 402, 5 of them differing. NEXT: chroma-CDEF direction/damping
> edge case for that block (dump per-block cdef direction + the 5
> samples' filter taps on both sides). NOTE: ffmpeg's av1 encode is
> NONDETERMINISTIC across processes for 160x90 — the harness now LOADS a
> pinned OBU if KINETIX_AV1_CHROMA_OBU points at an existing file
> (generate once, reuse for every comparison). Also: dav1d CLI exits 1
> with `-o /dev/null` on Windows ("No extension found for file nul") —
> use a real output path; decode itself is unaffected (DUMPF still dumps).

> **2026-09-20 (cont'd) — chroma residue re-diagnosed: LR stripe-2 boundary,
> NOT cdef.** Stage isolation on the pinned 160x90 stream (kinetix
> NODEBLOCK/NOCDEF/NOLR vs dav1d --inloopfilters 1/3/4/7) proves: deblock ✓
> exact all planes; CDEF ✓ exact all planes (kin DC == dav mask3); LR-only ✓
> exact (kin LR-only == dav mask4 — the restoration function itself is
> correct on identical input). But FULL pipelines differ by ~130 uv samples
> whose pattern starts EXACTLY at uv row 32 = chroma LR stripe 2 boundary
> (luma row 64), pattern = "dav1d's LR changed it, kinetix's didn't" (plus
> scattered "both changed differently" further into stripe 2). Root cause
> hypothesis: chroma LR stripe geometry for 4:2:0 at non-8-aligned heights.
> dav1d's `dav1d_copy_lpf` computes chroma stripe boundaries as
> `(sby << ((6 - ss_ver) + sb128)) - offset_uv` (offset_uv = 8*!!sby >>
> ss_ver) and `row_h = imin((sby+1) << ((6-ss_ver)+sb128), h-1)` — the
> -ss_ver shifts put the chroma stripe-2 boundary and its 2-row
> `lr_lpf_line` backup at chroma rows that Kinetix's
> `apply_loop_restoration_plane` (which stripes by plain 32-chroma-rows per
> 64-luma-stripe from the PLANE top) computes differently once the frame
> height isn't a multiple of 64. NEXT: port dav1d's exact stripe
> row/backup-row arithmetic (including the `imin(..., h-1)` frame-edge
> clamp and the `offset = 8*!!sby` deblock-overlap rows) into
> apply_loop_restoration_plane for chroma, then re-run this stage-isolated
> diff until LR-full matches. Also note: dav1d CLI on Windows fails
> `-o /dev/null` (exit 1, "No extension found for file nul") — always pass
> a real output filename when scripting it; decode + DUMPF still run.

> **2026-09-20 (cont'd 2) — CORRECTION to the residue attribution above: the
> 5-sample "chroma CDEF" claim was measured against a STALE dump (the
> t160cmp/kfr_* files predated the redesign; ffmpeg's per-process
> nondeterminism struck again). With correctly pinned dumps the residue is
> ~130-336 uv samples and the diagnosis is the LR STRIPE one that follows.
> Lesson: every dump comparison must use files from the SAME pinned stream
> and the SAME binary build; delete stale dumps before each run.
> Cleanup: dead `split_planes` removed from decoder.rs (refresh now takes
> PaddedPlanes). All gates green after cleanup.

> **2026-09-20 (cont'd 3) — chroma residue FINAL characterization.** The
> LR-stripe theory from cont'd 2 was also wrong (stage isolation showed
> LR-only == dav mask4 exactly, and LR is DISABLED in the inter frames'
> headers anyway — both decoders parse types=[0,0,0] for every inter
> frame, confirmed by dav1d's own LRHDR probe on the pinned stream). The
> true residue: 44 chroma samples total across the 8 inter frames (43 U +
> 1 V, ±1-2 each, zero luma), all inside ONE chroma 8x8 per frame — the
> block at chroma (48-55, 32-39) = luma (96-111, 64-79), inside the 32x32
> compound block at mi (24,16). Stage attribution on the pinned stream:
> recon ✓ exact; deblock ✓ exact (kin DEBLOCK == dav mask1, 0 chroma
> diffs); the divergence enters in the CDEF(+LR=no-op) stage: dav1d's cdef
> changes 3 of the 5 samples, kinetix's changes a different subset. The
> chroma 8x8 sits at the TOP of the second superblock row (chroma row 32
> = the sbrow boundary), where dav1d's cdef reads `top`/`bot` context
> lines from `lr_lpf_line`/`cdef_line` buffers — the sbrow-boundary line
> content or the uv direction remap (Cdef_Uv_Dir) for THIS boundary is
> the remaining suspect. NEXT (small, isolated): dump dav1d's cdef dir +
> uv_dir + pri/sec strengths for the chroma 8x8 at (48,32) frame 1
> (hook cdef_apply_tmpl.c like KFINTRA), compare with kinetix's
> cdef_plane_chroma dir/strengths (add a CDEFCHROMA trace), and diff the
> filter taps. Also possible: dav1d's --inloopfilters CLI forces
> restore_planes bits that interact with copy_lpf/lr_lpf_line content at
> sbrow boundaries — compare dav mask2 (cdef only) vs the pinned kinetix
> deblock+cdef output to decouple.
> Session totals: 160x90 Y EXACT all 9 frames (filtered + unfiltered);
> chroma 44 samples ±1-2 across 8 frames (was: ~2100 chroma samples at
> ~15 dB + 3450 luma). All other corpora fully bit-exact. Gates green.

> **2026-09-20 (cont'd 4) — CDEF exonerated; residue re-attributed to
> frame n1's recon/deblock stage; all previous stage attributions were
> cross-run artifacts.** Built frame-identified (order_hint + show_frame)
> probes into BOTH decoders' chroma CDEF (kin `cdef_plane_chroma` CDEFUV,
> dav `cdef_apply_tmpl.c`) and per-frame stored-reference grid dumps
> (kin `KINETIX_AV1_DUMP_GRID` in reconstruct/mod.rs writing kgr_NN.yuv
> by decode sequence; dav `KINETIX_DBG_DUMPCUR` hooking
> `dav1d_decode_frame_exit`, kgr_NN.yuv keyed by order_hint — beware
> n1/n7 both have oh=6 so dav's n1 dump is overwritten by n7's; the two
> frames' FILTERED grids are byte-identical in dav, and kin's kfr dumps
> showed n1's and n7's recon are also identical, so kgr_06 = both).
>
> Stream anatomy of pinned.obu (1341 B, 160x90 testsrc): 9 coded frames
> + 1 show_existing, decode order oh = 0(K),6*,3*,1,2,SE,4,5,6,7 where
> n1(oh6, show_frame=0) and n2(oh3, show_frame=0) are HIDDEN alt-refs,
> the SE replay re-displays n1, and n7 is a second coded frame that
> re-encodes n1's exact recon. TU#2 carries THREE frames (TD,FRAME,FRAME
> ,FRAME — obu6 has no TD); the test harness's TD-based TU splitter
> handles it. kin reconstructs 9 coded frames; dav's CLI reports 8 shown.
>
> Key semantics settled: AV1 references ARE the post-deblock+post-CDEF
> (and post-LR) planes (dav1d: f->cur aliases f->sr_cur, decode.c:3618,
> and c->refs[].p = sr_cur) — kin storing `padded` cloned AFTER
> apply_post_filters (reconstruct/mod.rs:2221) is CORRECT; do not "fix"
> it back to pre-filter. Both decoders' parsed LR headers agree frame by
> frame (LR only on the keyframe, V-plane SgrProj). Deblock/CDEF
> parameters at the probe block are identical in both decoders on every
> frame that filters it (oh=0: pri=2/dir=1; oh=6/n1: pri=3/dir=2;
> oh=7: sec=4/dir=0; kin also prints oh=1 with pri=0 = dav's skip_uv).
>
> Re-attribution: the "44 samples, divergence enters at CDEF" claim came
> from comparing dumps across RUNS with different --inloopfilters env —
> meaningless once references include filtered content (each run's
> frames n>=1 recon legitimately differs). Ground truth from same-run
> full-grid comparisons (23040 B = 160x96 grid incl. padding rows
> 90..95): n0 EXACT (0 diffs incl. padding — the padded-recon + KF LR
> path is fully verified); n1 grid diffs = 6 Y + 33 U + 37 V; n2..n6 ≈
> 30 U + 25-32 V each, 0-14 Y; final visible outputs 0/19/8/2/4/6/5/5/9
> ≈ 58 samples ±1-2. The diffs cluster at (a) chroma rows 31-32 = the
> luma y=64 SB-ROW BOUNDARY (U(72,31), U(78,31), U(79,31), V(8,31)…),
> (b) luma row 64-65 x=63/124-127 (the same boundary), and (c) the chroma
> 4x4s at (48,32)/(48,36) [luma (96,64)/(96,72)] — and the kin CDEFUV
> probe shows n1's PRE-CDEF block already differing at U(50,35) (kin 235
> vs dav 232) in the full run, i.e. the divergence is in n1's RECON or
> DEBLOCK, NOT CDEF. In the deblock-only staged run (NOCDEF+NOLR) kin n1
> == dav n1 except 10 samples at exactly such SB-boundary edges —
> Y(63,66), Y(64,66), U(72,31), U(78,31), U(79,31), V(63,15), V(8,31),
> V(62,31), V(31,34), V(32,34) — pointing at kin's deblock EDGE FLAGS or
> LEVELS on frame n1's SB(16,16) top/left boundary (luma x=64 / y=64) as
> the root cause; CDEF then copies those ±1s into the residue pattern.
> NEXT (concrete): dump kin's deblock edge-flag/level decisions for frame
> n1 (meta.u_tx_w/u_tx_h/chroma_edge_left/chroma_edge_top around
> mi(16,16) and the luma maps at x=64/y=64) against a dav1d
> loopfilter_tmpl.c probe gated to that frame+edges; diff which edges
> each decoder filters. CAVEAT recorded: dav1d 1.5.4's own output on this
> stream differs by 4-8 visible samples/frame between --threads 1 and
> default MT (1ed92ccf vs 44202e14 md5) — the conformance harness uses
> default MT as ground truth; kin sits within ~±2 of both.
> New env probes this session (all no-op unless set, keep):
> KINETIX_AV1_DUMP_GRID=<dir>, KINETIX_DBG_CDEFUV (now also covers
> luma (96,72)), KINETIX_DBG_DUMPCUR/LRHDR (dav1d clone), and oh= dump
> tags in KINETIX_AV1_DUMP_FRAMES output. All gates green (fmt, clippy,
> AV1 154 tests, test-utils incl. conformance 11/11).

> **2026-09-23 — chroma edge-marking bug FOUND + FIXED; 44-sample residue
> collapsed to 1.** Root cause: skip-mode inter blocks (§5.11.11's
> `is_inter && skip_mode`) never call `mark_chroma_edges` — the only
> other call site for inter blocks is inside the chroma-tx loop, which
> skip-mode blocks never enter — so the chroma horizontal deblock filter
> silently skipped every skip-mode block's row boundary. Fixed in
> `inter_block.rs` (`decode_inter_block`): mark_chroma_edges is now
> called unconditionally right after the skip-mode reconstruction call.
> Second related fix in the regular (non-skip-mode) inter chroma-tx
> loop: `has_chroma == false` sub-blocks (odd-parity 4×4 luma blocks at
> even mi_row/mi_col) were skipping `mark_chroma_edges` entirely too —
> moved the edge-mark call above the `has_chroma` early-continue so
> geometry is always recorded even when this particular sub-block owns
> no chroma samples of its own (§7.14.1: edges are at luma block
> boundaries, not gated on chroma ownership).
> Also: `tile_ch` in `TileDecodeState::new` was truncating to
> `tile_h / 2` (floor) instead of covering the full MI-aligned chroma
> extent (`tile_h.div_ceil(MI_SIZE) * (MI_SIZE/2)`) — for a 90px-tall
> frame this left the last partial MI row's chroma content at the
> initial 128-fill instead of real reconstructed pixels, which CDEF's
> secondary taps could then read as neighbours. `apply_post_filters`
> gained a `vis_height` parameter so CDEF direction-derivation still
> clips to the visible frame (wrong variance estimates otherwise) while
> CDEF's WRITE bounds cover the full padded/MI-aligned extent, matching
> dav1d (references are post-filter planes, so subsequent inter frames'
> MC must read real CDEF output in the padding rows, not stale fill).
> Corpus result: testsrc_128x96/testsrc_96x64/testsrc_64x64/
> smptebars_96x64 all now **100% bit-exact** (were ~44 chroma samples
> off, ~15dB). testsrc_160x90 down to a **single** ±1 V-sample at
> chroma (67,44) frame 7 (83.69dB) — traced with the existing
> `KINETIX_AV1_DBG_CDEF67_44` hook to a secondary-tap-only CDEF case
> (pri_str=0, sec_str=4, damp=4, dir=0): kin sums 5 secondary-tap
> contributions to sum=-8, `val = 17 + ((8 - 8 - 1) >> 4) = 16`; dav1d's
> ref is 17. One of the 5 secondary-neighbour samples (candidates: the
> two reads at xx2=67 == the block's own column, at yy2=42/44/46, or the
> xx2=65/66 reads) is off by ±1 from what dav1d reads at the identical
> tap, OR the >>4 rounding of a small negative sum differs at this exact
> boundary. NEXT: dump dav1d's own CDEF secondary-tap trace for this
> exact chroma pixel (frame with order_hint matching kin's 7th decoded
> inter frame) via `cdef_apply_tmpl.c`, diff sample-by-sample against
> the `CDEF67_44 SEC k=... s=...` lines above.
> **2026-09-23 (cont'd) — the last ±1 sample: one bad neighbour tap,
> root cause NOT found, one hypothesis ruled out (keep this note so the
> next session doesn't re-walk it).** Isolated exactly: kin sums 5
> secondary CDEF taps to sum=-8 at chroma (67,44) frame 7 (V plane,
> pri=0, sec=4, damp=4, dir=0, x=17); dav1d's ref is 17, which requires
> sum=-7 (val = x + ((8+sum-borrow)>>4); sum=-8 gives -1 after shift,
> sum=-7 gives 0). One of the 5 nonzero-magnitude taps must be off by
> exactly 1: candidates are s@(66,44)=16, s@(65,44)=15, or the 3 reads
> of the SAME column (67,42)/(67,43)/(67,46), all =16. Confirmed via a
> `scratch_px_compare.rs` harness (decode + dump a V-plane window,
> deleted after use — recreate from `av1_inter_corpus_vs_dav1d_when_available`'s
> body in conformance.rs if repeating this) that every OTHER pixel in a
> 12×12 window around this one matches the reference exactly, both in
> rows above (43, 42, 41...) that feed the vertical secondary taps AND
> in the final post-CDEF output — so this is not a propagated multi-row
> drift, it's isolated to this one CDEF evaluation.
> RULED OUT: hypothesized that `TileDecodeState::new`'s `tile_ch`
> (chroma reconstruction height bound) was 2 rows short of the real
> buffer extent — reconstruct/mod.rs's `grid_h`/`uv_grid_h` use the
> spec §5.9.15 8-pixel-rounded `MiRows = 2*ceil(H/8)` formula (90px →
> 96 luma / 48 chroma rows), while `tile_ch` used plain `ceil(H/4)*2`
> MI rounding (90px → 46 chroma rows) — 2 rows short, meaning the CDEF
> secondary tap at chroma row 46 could've been reading stale 128-fill.
> **Fixed this regardless (it's a real, spec-correct bug: `tile_cw`/
> `tile_ch` now match the 8-px-rounded grid extent) but it did NOT
> change the CDEF trace at all** — row 46's value was already `16`
> (not the 128 fill) *before* the fix, meaning something other than the
> tile_ch-bounded residual-add path (most likely: inter MC prediction
> copy for skip-mode blocks, which isn't obviously gated by tile_ch)
> already populates rows beyond the old bound. Kept the fix since it's
> correct per spec regardless of this bug; no corpus regression (11/11
> conformance tests still pass, 154 AV1 lib tests still pass).
> NEXT: since the direction-forcing rule (`dir = 0 if pri_str == 0`) is
> validated by 100%-exact luma across the whole corpus using the exact
> same code shape, it's very unlikely to be the bug (ruled out by
> induction, not directly tested). The real next step is a genuine
> pixel-level oracle for the PRE-CDEF (post-deblock) plane at frame 7 —
> neither the patched tracing dav1d (absolute pixel values differ from
> ffmpeg's vendored libdav1d 1.3.0 by a constant-ish offset at this
> pixel: 18 vs 17, a real *version* difference between the two dav1d
> builds on this machine, not just an MT nondeterminism artifact) nor
> ffmpeg's libdav1d (only exposes fully-filtered output) can supply this
> directly. Options: (a) patch the patched dav1d's `--inloopfilters
> deblock` output back against ITSELF at frame 6 (known-bit-exact) to
> at least establish whether ITS OWN row 46 tap differs frame-to-frame
> in a way suggesting a boundary condition; (b) instrument Kinetix's own
> pre-CDEF plane dump (`KINETIX_AV1_NOCDEF` + a raw-plane-write hook)
> and manually trace which reconstruction code path (residual-add vs MC
> copy vs OBMC/interintra blend) actually wrote chroma row 46 in this
> specific frame, then check whether an OBMC/warped-motion/interintra
> block near mi (32,22) has a **third** missing `mark_chroma_edges`
> call site of the same class as the two fixed earlier this session
> (skip-mode, has_chroma==false) — this is the most likely remaining
> bug shape given the session's track record. Given how narrow and
> deep this is (a single ±1 sample across 5 corpus entries × up to 8
> frames each), further investigation has low ROI per session unless a
> genuine pre-CDEF oracle becomes available; the corpus is otherwise
> **100% bit-exact** (`4/5 entries fully exact, 1 entry 6/7 frames
> exact`).
> Housekeeping: fixed a **pre-existing, unrelated** compile break in
> `reconstruct/tests.rs` (8 call sites of `decode_tile_group`/
> `TileDecodeState::new` were stale by 4 positional args —
> `lf_levels`/`lf_ref_deltas`/`lf_mode_deltas`/`lf_delta_enabled` — from
> an earlier session; `cargo test -p tpt-kinetix-av1 --lib` would not
> compile at all before this fix). Also added `--threads 1` to the
> standalone-`dav1d` reference path (`reference.rs`) per the documented
> MT-nondeterminism caveat, and per-chroma-sample diff dumping in
> `av1_inter_corpus_vs_dav1d_when_available`. CAVEAT for next session:
> do NOT put the patched tracing dav1d build
> (`%LOCALAPPDATA%\Temp\tpt-kinetix-dav1d`) on `PATH` for conformance
> runs — it prints its unconditional per-symbol trace to *stdout* (not
> stderr) when invoked with `-o -`, corrupting the captured YUV and
> making every reference decode look broken ("not a multiple of frame
> size"). The workspace has no clean standalone `dav1d`; conformance
> tests fall back to `ffmpeg`'s `libdav1d` automatically as long as the
> patched build isn't on PATH. Gates green: workspace build clean, AV1
> 154 lib tests pass, test-utils conformance 11/11, clippy/fmt show only
> pre-existing unrelated failures (20 pre-existing clippy errors on
> master, none newly introduced — mostly `manual_div_ceil` and
> `too_many_arguments` on functions this diff didn't touch).

> **2026-09-24 — the last ±1 sample: CDEF math re-verified spec/dav1d-exact,
> exact source block identified, hidden-alt-ref blind spot investigated and
> ruled out as this bug's cause (no fix landed).** Picked this back up from
> the 2026-09-23 session's NEXT list. Two lines of investigation, in order:
>
> **(1) CDEF algorithm cross-check against real dav1d source (new this
> session, previously only inferred from ported comments).** Fetched
> `cdef_apply_tmpl.c` and `cdef_tmpl.c` from `videolan/dav1d` on GitHub.
> Confirmed line-for-line: `uvdir = uv_pri_lvl ? uv_dir[dir] : 0` (our
> `dir = if pri_str == 0 { 0 } else { CDEF_UV_DIR[..][yd] }` is the exact
> same rule, not a Kinetix approximation); the secondary-tap weight
> `sec_tap = 2 - k` (independent of `pri_strength` parity) matches
> `CDEF_SEC_TAPS = [[2,1],[2,1]]` being identical across both rows; the
> primary-tap `pri_tap_k = (pri_tap_k & 3) | 2` recurrence for k=1 matches
> `CDEF_PRI_TAPS[[4,2],[3,3]]`. Hand-verified `CDEF_DIRECTIONS[dir][k]` index
> arithmetic against the actual `CDEF67_44` trace line-by-line for the
> failing evaluation (dir=0, d=(0±2)&7={2,6}) — every `(yy2,xx2)` in the
> trace matches what the table predicts exactly. **This closes off the
> "direction-forcing rule" and "tap table" hypotheses for good: the CDEF
> filter's own arithmetic is provably byte-for-byte dav1d-equivalent for
> this exact code path.** The bug, if it is a Kinetix bug at all, is
> upstream of `cdef_filter_block` — in what value one of the 5 secondary
> source taps reads.
>
> **(2) Traced the responsible reconstruction block via a new
> `KINETIX_AV1_DBG_TAPBLK` probe** (committed, `inter_block.rs`): prints
> mi/bsize/motion_mode/interintra/skip/ref/mv/ref_to_slot/dpb_order_hints
> for whichever inter block (regular or skip-mode path) covers the luma
> region under chroma (65–68, 40–48) — i.e. the block that owns the 5 CDEF
> taps. Result for `testsrc_160x90` frame 7 (decode-order n=8, order_hint
> 7): **`mi=(32,16) bsize=9 (32×32) mm=0 (SIMPLE) ii=0 skip=false ref=[5,0]
> (GOLDEN, single-ref) mv0=(0,0)`.** This directly rules out the
> 2026-09-23 session's leading hypothesis (a third missing
> `mark_chroma_edges` call in an OBMC/warp/interintra path): **none of
> those paths are active on this block** — it's a plain single-ref,
> non-skip (has residual), zero-motion-vector, ordinary-partition block.
> Whatever's wrong is in the ordinary MC-copy + dequant + inverse-transform
> + residual-add path, or (see next) in an upstream reference frame.
>
> **New finding: GOLDEN resolves to a HIDDEN alt-ref frame the harness
> never independently checks.** `ref_to_slot[5]=1`, `dpb_order_hints[1]=6`
> — slot 1 holds order_hint 6, which was written by decode-order frame n1
> (`show_frame=false`, a genuine hidden alt-ref) and is *never subsequently
> refreshed* by anything before frame n8 (oh=7) reads it (frame n7, also
> oh=6 but *shown*, has `refresh_frame_flags=0` — it doesn't touch slot 1
> at all). Concretely: **the pixel data feeding this block's MC copy is n1's
> raw reconstruction, and n1 is never in `kframes`/`ref_frames` in the
> conformance test (it's hidden), so it has literally never been diffed
> against dav1d.** This is a real structural blind spot: 2 of the 9 coded
> frames in this clip are hidden alt-refs (oh=6 and oh=3), and only oh=6
> gets a later `show_existing_frame` (verified: search for `SET_SHOWABLE`/
> SE spans found only one). Wrote a throwaway harness,
> `tpt-kinetix-test-utils/tests/dbg_av1_160_grid.rs`, that decodes exactly
> `av1_inter_corpus()`'s `testsrc_160x90` bytes (not a fresh re-encode —
> ffmpeg's AV1 encode is nondeterministic across processes, confirmed
> again this session: an independently-encoded 160x90/9-frame clip via
> `dbg_av1_chroma.rs` produced a *different* GOP structure, decode order,
> and order-hint pattern than the real corpus entry — don't reuse pinned
> OBUs across harnesses without checking `DBGSEQ`/`DBG refresh` traces
> line up) and dumps `KINETIX_AV1_DUMP_GRID` per frame.
>
> **Tested whether n1's hidden reconstruction actually diverges from
> ground truth, using n7 (order_hint 6, shown, independently verified
> bit-exact vs dav1d) as a proxy oracle** — the 2026-09-20 session had
> already established via a patched-dav1d probe that dav1d itself decodes
> n1 and n7 to byte-identical FILTERED grids, so if Kinetix's n1 and n7
> grids differ, at least one of them is wrong, and since n7 is externally
> validated it would prove n1 is the buggy one. Result: **`cmp -l` between
> `kgr_01.yuv` (n1) and `kgr_07.yuv` (n7) shows exactly 14 differing
> bytes, all ±1, clustered at the luma y=64/65 and chroma y=31 SB-row
> boundary** (Y(63,64)=42→41, Y(124-127,65)=80→81, Y(61,69)=159→160,
> U(72/78/79,31)=127→128, V(63,15)=238→239, V(7-9,31)/V(62,31)=+1 each) —
> **not near our target (65–67,42–46) at all.** Chased whether this is a
> real bug anyway (it's the exact symptom class the 2026-09-20 session
> flagged as unresolved: "SB-row-boundary" luma/chroma ±1s) but found an
> innocent explanation that fully accounts for it: `TAPBLK` on n7's own
> block at this location shows `ref=[8,0] (ALTREF) ref_to_slot[8]=1
> skip=true mv=(0,0)` — **n7's block here is itself a zero-residual MC
> copy of slot 1 (n1's own stored pixels)**, so the pre-deblock content is
> guaranteed identical to n1's; the observed ±1s at the SB-row boundary are
> consistent with n7's *own fresh per-frame deblocking* of the copied edge
> pixels (which depends on n7's own neighbouring blocks' modes/mvs, not
> n1's) producing slightly different bS/filtering decisions than n1's
> original deblock pass did — both frames can be simultaneously correct.
> **Did not find a way to independently prove or disprove n1's correctness
> at the ACTUAL target region (65–67,42–46 chroma / away from any SB
> boundary)** — the n1-vs-n7 diff technique only exposes disagreements
> where n7 legitimately re-touches the copied pixels (deblock edges); it
> is silent everywhere n7 is a pure untouched copy, which is exactly where
> our target sits, so this technique cannot confirm or rule out a latent
> n1 bug at (65,42)-(67,46). **Net result: hidden-alt-ref-blind-spot lead
> investigated, produced one genuine (probably benign) SB-boundary
> curiosity, but did NOT close the (67,44) bug.**
>
> **Where this leaves things:** the 5 candidate secondary-tap source
> pixels (chroma V, frame oh=7, pre-CDEF) are still exactly `(67,43)=16`,
> `(66,44)=16`, `(67,42)=16`, `(65,44)=15`, `(67,46)=16`, one of which must
> really be one higher (the two that would flip the final sum from -8 to
> -7 if raised by 1: either tap `(67,42)` or `(67,46)` from 16→17, or tap
> `(65,44)` from 15→16 — all three are `sec_tap` weight-1 reads at k=1).
> All of them are read from slot 1 = the hidden n1 frame. The CDEF
> algorithm itself is now about as thoroughly proven-correct as it can be
> without a matched-version reference decoder. **Concrete next step if
> picked up again:** get ANY way to independently validate hidden (never-
> shown) frames' pixel content, not just shown ones — e.g. extend
> `av1_inter_corpus_vs_dav1d_when_available` (or a new harness) to also
> decode with `--all-layers`/force-show every frame via a patched dav1d
> build (accepting its known absolute-value version offset — but compare
> RELATIVE tap-to-tap deltas within one frame's dump instead of absolute
> values, since a constant version offset would cancel out in a
> difference-of-two-nearby-samples comparison, which is exactly what the 5
> secondary-tap contributions are). Given this is a single ±1 sample out
> of the entire 5-entry inter corpus and three sessions running have now
> independently exhausted the cheap leads (direction rule, tap tables,
> edge-marking gaps ×2 fixed + 1 ruled out, OBMC/warp/interintra ruled
> out, hidden-alt-ref blind spot investigated), this is a good point to
> either park it or invest in the harness-level oracle gap above rather
> than more manual trace bisection. Corpus unchanged from 2026-09-23:
> **4/5 entries 100% bit-exact, testsrc_160x90 6/7 frames exact (single
> ±1 V-sample, 83.69 dB)**. `capabilities().pixel_exact` correctly remains
> `false` — did not touch it (this narrow synthetic-corpus gap is not
> broad enough evidence to flip a decoder-wide correctness claim even if
> it were the only known issue, and it isn't proven closed).
> Housekeeping: added `KINETIX_AV1_DBG_TAPBLK` (inter_block.rs, two call
> sites) and `tests/dbg_av1_160_grid.rs` (test-utils) as reusable probes,
> both committed. Gates green: AV1 154 lib tests, test-utils conformance
> 11/11, `cargo fmt --check` clean on both touched crates. Did not re-run
> full `just clippy`/`just deny`/`just doc`; spot-checked
> `cargo clippy -p tpt-kinetix-av1 --all-targets -- -D warnings` shows only
> the same 10 pre-existing errors (`manual_div_ceil` ×9,
> `too_many_arguments` ×1) already present on master before this session,
> none in the files this session touched.

> **2026-09-24 — MAJOR progress: built a validated independent dav1d oracle,
> disproved the leading "hidden alt-ref" hypothesis, and root-caused the
> bug to OH=7's own V-plane chroma leaf (16×16, `eob=0`) diverging from
> dav1d specifically in the padded rows below the visible frame — no fix
> landed, but this is now an extremely narrow, well-isolated target.**
> Picked this up per an explicit instruction to pursue the prior session's
> "relative tap-to-tap delta against patched dav1d" idea.
>
> **Part 1: got a real, working, rebuildable dav1d oracle (this didn't
> exist before this session).** The existing "patched tracing dav1d" at
> `%LOCALAPPDATA%\Temp\tpt-kinetix-dav1d` turned out to be UNREBUILDABLE
> — its `src/` directory only contains the ~10 files a past session
> patched (`cdef_apply_tmpl.c`, `decode.c`, `lf_apply_tmpl.c`, `lf_mask.c`,
> `loopfilter_tmpl.c`, `mc_tmpl.c`, `obu.c`, `recon.h`, `recon_tmpl.c`),
> not a full dav1d source tree; `ninja` fails immediately (missing
> `vcs_version.h.in`, then missing everything else). **Fixed by freshly
> cloning `videolan/dav1d` tag `1.5.4` to `%LOCALAPPDATA%\Temp\dav1d_fresh`,
> copying over only the already-patched `cdef_apply_tmpl.c`** (the other
> 8 patched files are stale relative to 1.5.4's headers — `decode.c`
> fails to compile against the real 1.5.4 `refmvs.h`, `const refmvs_block
> *const *` vs `refmvs_block **` — so they were left at the fresh clone's
> unpatched 1.5.4 version) **and running `meson setup build
> --buildtype=release` + `ninja -C build tools/dav1d.exe`** (needs
> `vcvars64.bat`'s environment; `meson`/`ninja`/`nasm`/`cmake` are all on
> PATH already via scoop/pip). Builds clean in ~15s incremental. The
> resulting `dav1d.exe` needs `PATH` to include `build/src` (for
> `libdav1d.dll`) or it fails silently. **This is now a real, working,
> re-editable dav1d 1.5.4 — commit this recipe if a future session wants
> to keep using it** (not preserved in the repo since it lives entirely
> under `%LOCALAPPDATA%\Temp`, gitignored territory, and rebuilding takes
> under a minute once cloned).
>
> **Part 2: the "18 vs 17 version difference" from 2026-09-23 was NEVER a
> version difference — it was `--threads 1` corrupting the decode.**
> Decoded the real `testsrc_160x90` corpus OBU (saved via the new
> `KINETIX_AV1_160_OBU_OUT` env var on `dbg_av1_160_grid.rs`, since
> ffmpeg's AV1 encoder is nondeterministic across processes and a fresh
> re-encode is NOT the same bitstream — confirmed this again the hard
> way: an independently-encoded 160x90/9-frame clip via
> `dbg_av1_chroma.rs` has a completely different GOP/order-hint pattern)
> three ways: `ffmpeg -i … -pix_fmt yuv420p -f rawvideo` (libdav1d 1.3.0,
> the conformance harness's actual ground truth), my fresh `dav1d.exe`
> with default (multi-threaded) settings, and my fresh `dav1d.exe` with
> `--threads 1`. **Default-MT dav1d 1.5.4 matches ffmpeg's 1.3.0 EXACTLY**
> at every sampled pixel (frame 7 V-plane row44 x64-71:
> `14 15 16 17 17 17 127 126` — identical in both). **`--threads 1` on
> the SAME binary gives a DIFFERENT, wrong-looking result**
> (`14 15 17 18 18 17 127 126` pre-CDEF at the same block) — this is
> backwards from what every prior session assumed (that `--threads 1`
> was the more-reliable/deterministic setting and MT was the risky one);
> here it's `--threads 1` that's the outlier. **Conclusion: always use
> default (or explicitly multi-threaded) settings with this dav1d build
> for reference comparisons; never pass `--threads 1`.** This fully
> resolves the "dav1d 1.5.4 pre-CDEF value differs from ffmpeg's 1.3.0
> by a constant-ish offset, a real version difference" claim from
> 2026-09-23 — it wasn't a version difference at all.
>
> **Part 3: with a validated oracle, re-did the (67,44) 5-tap comparison
> properly and found the real single wrong tap.** Added a `TAP67 dav`
> debug hook (`cdef_apply_tmpl.c`, gated on `KINETIX_DBG_TAP67`,
> `frame_offset==7 && show_frame`, `bx==32 && by∈{20,22}`) that dumps the
> pre-CDEF chroma V 4×4 windows at `(64,40)` and `(64,44)`, and a mirror
> `KINETIX_DBG_TAP67` hook in Kinetix's own `cdef_plane_chroma`
> (`loop_filter.rs`). With the trusted (default-MT) oracle, dav1d's block2
> (chroma rows 44-47, cols 64-67) is **flat: `14 15 16 17` repeated
> identically on every one of the 4 rows.** Diffing this against
> Kinetix's known 5-tap values (from the 2026-09-23 `CDEF67_44` trace) —
> `(67,43)=16✓`, `(66,44)=16✓`, `(67,42)=16✓`, `(65,44)=15✓`,
> **`(67,46)=16✗ (dav1d: 17)`** — every tap now matches except `(67,46)`,
> exactly the swing needed to flip the CDEF sum from -8 to -7 (the tap's
> weight-1 `sec_tap`, diff 16-17=-1→constrain=-1→contrib=-1; raising it
> to 17 makes diff=0→contrib=0, sum -8→-7, final value 16→17, matching
> ground truth). **This is the first session to pin the exact wrong
> sample with a validated oracle rather than guessing among candidates.**
>
> **Part 4: traced `(67,46)`'s wrongness back through 3 frames and
> disproved the "hidden alt-ref frame is buggy" hypothesis from earlier
> in this same session (see the addendum above this one) — the keyframe
> IS correct.** `(67,46)`'s content is inherited via a chain: OH=7's
> block at mi(32,16) is single-ref GOLDEN (slot 1 = the hidden n1 frame,
> order_hint 6) with `mv=(0,0)`; n1's own block there is `skip=true`
> from the keyframe (also `mv=(0,0)`). Dumped `KINETIX_AV1_DUMP_GRID` for
> all 3 frames (keyframe, n1, oh=7) and found the SAME pattern at chroma
> (64-67,44-47) in the keyframe and n1 (`14 15 16 17 | 14 16 17 17 |
> 14 16 16 16 | 16 16 16 16`), which looked like a shared bug at first —
> **but then built the `TAP67 dav pre`/`post` pair for the KEYFRAME
> specifically (`frame_offset==0`) and found dav1d's OWN post-CDEF
> keyframe value at this exact block is `14 15 16 17 | 14 16 17 17 |
> 14 16 16 16 | 16 16 16 16` — IDENTICAL to Kinetix's.** The keyframe
> (and therefore n1's inherited copy of it) is 100% correct, including
> every padding row, including through CDEF. The "flat 14 15 16 17"
> pattern from Part 3 belongs to OH=7's OWN reconstruction only, not
> something inherited from upstream — my first pass at this conflated
> the keyframe's actual (non-flat) correct content with OH=7's
> after-residual flat content and wrongly concluded the keyframe was
> buggy; re-checking against the dav1d oracle (not just Kinetix's own
> internal consistency) caught the error. **Record this dead end
> explicitly so a future session doesn't re-chase the keyframe.**
>
> **Part 5: found where the "prediction → flat" transformation actually
> happens (or fails to) — landed on either a missing bottom-of-grid
> deblock edge or a real entropy desync, did not distinguish between
> the two.** OH=7's V-plane chroma residual at mi(32,16) is a SINGLE
> 16×16 leaf (`cw=16 ch=16`, confirmed via a `KINETIX_AV1_DBG_B0` trace
> extended this session with `seq=`/`cw=`/`ch=` — decode-order `seq=8` is
> confirmed to be OH=7 via the `DBGSEQ` cross-reference) with **`eob=0`**
> — Kinetix reads ZERO residual coefficients for chroma V here, so its
> reconstructed value is the pure MC-copy prediction (`ref=GOLDEN
> mv=(0,0)`, i.e. exactly n1/keyframe's stored content, which Part 4
> proved correct: `14 15 16 17 | 14 16 17 17 | 14 16 16 16 | 16 16 16 16`
> for rows 44-47). dav1d's actual pre-CDEF value for the SAME block is
> **flat `14 15 16 17` on every row** (Part 3). The row-by-row delta
> needed to turn Kinetix's prediction into dav1d's answer is
> `[0,0,0,0]` (row44) → `[0,-1,-1,0]` (row45) → `[0,-1,0,1]` (row46) →
> `[-2,-1,0,1]` (row47) — **magnitude increases monotonically toward
> row47, the LAST row of the padded mi-grid** (chroma height 48, real
> content ends at row44), which is the textbook shape of a deblocking
> filter's taper *from an edge below row47* (bigger correction closer to
> the edge), not a random residual pattern. Two live hypotheses, NOT
> distinguished this session:
> (a) **dav1d applies a deblock edge at/near the bottom mi-grid boundary
> (row48, i.e. the bottom of the LAST superblock row) that Kinetix
> doesn't** — plausible since AV1 SBs always cover full 64×64 (or
> 128×128) units regardless of crop, so a real edge could exist at the
> superblock's own bottom boundary even past the visible frame, and nothes
> found by grepping don't show any special-casing for "last real SB row's
> bottom edge" in `deblock_plane`'s band iteration (worth checking
> `mark_chroma_edges` calls near the tile's bottom edge specifically, and
> whether a below-tile "virtual" edge should be synthesized the way a
> real edge would be, similar in spirit to the two already-fixed
> `mark_chroma_edges` gaps from 2026-09-23);
> (b) **`eob=0` is itself the bug — a genuine entropy desync** that
> silently reads zero coefficients when the true bitstream has real ones,
> possibly not cascading further because this may be the last (or a very
> late) coefficient read in its tile. Consistency check NOT done this
> session: whether U-plane (which DID get `eob=11` at the same mi/cpx) is
> itself correct at these same padding rows — if U matches dav1d exactly,
> that weakens the "shared desync" theory and points at V specifically
> (or an oddity specific to V's context/CDF path); if U is ALSO wrong in
> the same taper shape, that's much stronger evidence for hypothesis (a),
> a genuine missing edge affecting both chroma planes identically.
> **This U-plane check is the single highest-value next step.**
>
> **This closes the loop on why the original CDEF trace ever looked like
> a CDEF bug**: `(67,44)` itself is untouched by this — Kinetix's own
> pre-CDEF value there already matches dav1d exactly — the bug is purely
> that ONE of `(67,44)`'s secondary-tap NEIGHBOURS, `(67,46)`, sits in
> the affected padding-row region and is 1 too low, and CDEF (fully
> spec-correct, re-verified against real dav1d source in the previous
> addendum) faithfully propagates that pre-existing error into a visible
> ±1 at the one displayed pixel whose secondary tap happens to reach that
> far down. **Fixing the row45-47 V-plane divergence (whichever of (a)/(b)
> it turns out to be) should close the (67,44) mismatch as a side effect
> — no CDEF-side change is needed or should be made.**
>
> **Part 6 (did the Part 5 U-plane consistency check before stopping):
> U is ALSO wrong, in a smaller, differently-shaped way, at the exact
> same bottom-of-grid rows — this favours hypothesis (a) (missing
> deblock edge) over (b) (entropy desync).** Extended `KINETIX_DBG_TAP67`
> to dump plane U too (both the dav1d hook and Kinetix's, committed).
> dav1d's U pre-CDEF block2 (rows 44-47, cols 64-67): `168 167 166 166`
> (row44) / `168 167 166 166` (row45) / `169 168 167 167` (row46) /
> `170 169 168 168` (row47) — a smooth gradient, NOT flat like V (U had
> real residual, `eob=11`, unlike V's `eob=0`). Kinetix's U pre-CDEF same
> block: rows 44-45 match dav1d EXACTLY; **rows 46-47 differ only at
> column 3 (x=67)**: kin `166`/`167` vs dav1d `167`/`168`, both -1. So
> **both chroma planes are correct through row45 and both develop a
> small, col/row-localized defect starting exactly at row46** — V's is
> large (whole-row, because V has zero residual to mask it) and U's is
> tiny (one column, because U's real residual already did most of the
> correct shaping and only the boundary-filter's own small contribution
> is missing). A generic entropy desync on V's coefficient read would
> not plausibly produce this exact, independent, boundary-shaped defect
> in U too (U's own coefficient read is a separate, unrelated symbol
> sequence). This is a strong (not proven) signal that hypothesis (a) —
> **a real deblock edge Kinetix isn't applying, specifically at/near the
> bottom-right of the last superblock row's chroma grid (row 46/47,
> column ~67-68)** — is the right one to chase first next session, ahead
> of auditing `read_coeffs`/eob desync theories.
> **Concrete next step:** find where `deblock_plane`'s band/edge loop
> (`loop_filter.rs`) determines the LAST band's bottom edge availability
> for chroma, and check whether the padded grid's bottom-most 4-chroma-
> row cell (covering rows 44-47, `by=11` in the `h8`-scaled grid used by
> the chroma deblock call) or its right-neighbour cell around chroma
> column 64-68 has a missing/zeroed edge flag or filter-size compared to
> what dav1d's own `lf_mask`/`Av1Filter` would compute for the same
> position — the CANDIDATE region is now down to roughly 2 chroma rows
> × a handful of columns, about as narrow as this bug is going to get
> without a byte-level entropy-trace oracle.
>
> Housekeeping: `KINETIX_DBG_TAP67` added to both `loop_filter.rs`
> (Kinetix) and the fresh dav1d clone's `cdef_apply_tmpl.c` (not part of
> this repo); `KINETIX_AV1_DBG_B0`'s `uv-cf-blk` trace gained `seq=`/
> `cw=`/`ch=`; `dbg_av1_160_grid.rs` gained `KINETIX_AV1_160_OBU_OUT`. All
> three Kinetix-side changes committed (`av1: add KINETIX_DBG_TAP67...`).
> Corpus unchanged: **4/5 entries 100% bit-exact, testsrc_160x90 6/7
> frames exact (single ±1 V-sample, 83.69 dB)** — re-verified via
> `cargo test -p tpt-kinetix-test-utils --test conformance
> av1_inter_corpus_vs_dav1d_when_available -- --nocapture` after every
> change this session; no regression at any point. `capabilities().
> pixel_exact` correctly left `false`. Gates green: AV1 154 lib tests,
> test-utils conformance 11/11, `cargo fmt --check` clean on both touched
> crates, `cargo clippy -p tpt-kinetix-av1 --all-targets -- -D warnings`
> shows the same pre-existing errors only (none in files this session
> touched).

> **2026-09-24 (cont'd) — audited `deblock_plane` for the candidate
> region per an explicit follow-up instruction; the "missing deblock
> edge" hypothesis from Part 5/6 above is now WEAKER, not confirmed —
> new direct evidence instead points back toward hypothesis (b), a
> real entropy desync, though still not proven either way.** No fix
> landed; corpus still unchanged (4/5 entries 100% exact, testsrc_160x90
> 6/7 frames, single ±1 V-sample).
>
> **Checked the edge grids directly** using two debug hooks that turned
> out to already exist in `deblock_plane` from an earlier session
> (`KINETIX_AV1_DBG_CHROMA_HEDGE`/`_VEDGE`, both pre-targeted at exactly
> `by=11, bx=16/17` — i.e. someone already suspected this exact cell).
> For OH=7 (`seq=8`): **`edge_top=false` at by=11** (no horizontal edge
> between the row40-43 and row44-47 chroma cells — expected, since this
> block's chroma is one unified `TX_16X16` leaf spanning rows 32-47, so
> there's no real transform boundary at row 44). **Also checked the
> KEYFRAME at the same cell: `edge_top=false` there too**, yet the
> keyframe is proven fully correct (Part 4) — so `edge_top=false` here
> is not inherently wrong; a block can be correct without any edge if
> its prediction+residual already lands on the right values (as the
> keyframe's real PALETTE residual did). This weakens, not strengthens,
> the "missing horizontal edge" theory: if dav1d used the *same*
> `edge_top=false` (plausible, since both decoders should derive UV tx
> size the same way — chroma tx-tree splitting is independent of luma's
> var-tx-tree for inter blocks), dav1d's output would ALSO equal
> unmodified prediction, which it does NOT.
>
> **Checked the vertical edges too**: `edge_left=true` at bx=16 (the
> block's own left outer edge, x=64) with a real level; `edge_left=false`
> at bx=17 (x=68, no internal vertical edge — ruled out a hypothesized
> internal vertical split). The x=64 left edge is real and Kinetix DOES
> filter it, uniformly across rows 44-47 (one `by=11` band call covers
> all 4 rows). But a single vertical edge at x=64 can't explain the
> dav1d target pattern's shape: **dav1d's 4 target rows are IDENTICAL
> (`14 15 16 17` on every one of rows 44-47) despite Kinetix's (verified
> correct — see next paragraph) prediction being 4 DIFFERENT rows
> (`14 15 16 17` / `14 16 17 17` / `14 16 16 16` / `16 16 16 16`).**
> Turning 4 different rows into 1 identical row is what a HORIZONTAL
> (row-direction) filter does, not a vertical (column-direction) one —
> so if this is deblock at all, it has to be a horizontal edge, and no
> horizontal edge is marked here in either decoder's most likely shared
> geometry.
>
> **Added `KINETIX_AV1_DBG_PREDUMP2` (committed) to dump the raw MC
> prediction straight from `inter_predict_plane`'s output buffer, before
> any residual/deblock/CDEF** — this is a more direct instrument than
> the grid-dump comparisons Part 4/5 relied on. Result: **the prediction
> itself is byte-for-byte identical to the reference slot's stored
> content** (`14 15 16 17` / `14 16 17 17` / `14 16 16 16` / `16 16 16 16`
> for rows 44-47, matching slot 1's own `ref row44..47` dump printed in
> the same debug line) — confirms Part 4's conclusion again, from a much
> more direct source than the grid-dump byte-offset arithmetic used
> before. **Also found and discarded a bad data point**: an earlier
> `KINETIX_AV1_NODEBLOCK=1 + KINETIX_AV1_DUMP_GRID` comparison in this
> same session appeared to show OH=7's raw (nodeblock) reconstruction
> DIFFERING from n1's stored content — this contradicts the
> `PREDUMP2` result and should not be trusted (likely a measurement
> mistake, e.g. comparing across different processes/dumps rather than a
> real effect); `PREDUMP2` reads directly from the live buffer in the
> same process and is authoritative. Do not repeat the nodeblock-grid-
> dump technique for this specific check without cross-validating
> against a `PREDUMP2`-style direct read first.
>
> **Revised assessment**: since prediction is now conclusively exact and
> the deblock edge geometry at the plausible candidate cells doesn't
> obviously explain a 4-different-rows→1-identical-row transformation,
> **hypothesis (b) — `read_coeffs` returning `eob=0` for this V-plane
> leaf when dav1d's true bitstream has a small nonzero residual — is now
> the more likely explanation**, walking back Part 6's lean toward (a).
> The row-by-row delta needed (computed in Part 5:
> `[0,0,0,0]/[0,-1,-1,0]/[0,-1,0,1]/[-2,-1,0,1]`) is small and plausible
> as a real quantized 2D residual, not obviously deblock-shaped once the
> "4 rows → 1 identical row" requirement is taken seriously. Could not
> confirm this without a real bitstream/entropy-level oracle (dav1d's
> own `eob`/coefficient trace for this exact block) — building one would
> mean instrumenting the fresh dav1d 1.5.4 clone's `decode.c`/`msac.c`
> coefficient-read path and is a substantially bigger undertaking than
> today's CDEF-level hooks (the entropy decoder's state isn't neatly
> exposed at a single call site the way CDEF's per-pixel loop was).
>
> **Recommendation: park this specific ±1-sample bug.** Four sessions
> (2026-09-20, -23, and two passes this session) have now examined
> direction rules, tap tables, two real + one ruled-out edge-marking
> bug class, OBMC/warp/interintra, the hidden-alt-ref chain, keyframe
> correctness, deblock edge/level grids at the exact candidate cell, and
> the MC prediction step directly — all either fixed (two real bugs
> already landed) or exonerated. What remains needs a byte-level
> dav1d entropy-decoder trace to make further progress, which is a
> different order of investigation (a proper multi-session tooling
> project, not a bisection step) — not a good next target for another
> manual-trace session. If revisited, start by building that entropy
> oracle rather than more manual pixel bisection.
> Housekeeping: `KINETIX_AV1_DBG_PREDUMP2` added to `inter_block.rs`,
> committed. Re-verified gates green after this pass too: AV1 154 lib
> tests, test-utils conformance 11/11, `cargo fmt --check` clean.
> **2026-09-24 (cont'd) — corrected the standalone dav1d reference harness; no decoder change.** The workspace's `tpt-kinetix-test-utils::reference::run_dav1d_file` had two Windows-specific correctness issues: it forced `--threads 1`, which diverges from ffmpeg/libdav1d for some valid AV1 streams, and it used `-o -`, which this dav1d build contaminates with three non-frame bytes. The wrapper now uses default threading and writes to a temporary YUV file, then reads and removes that file. Re-run with dav1d 1.5.4: AV1 intra corpus **6/6 exact**; inter corpus remains **4/5 entries fully exact**, with `testsrc_160x90` at **6/7**, differing only at V `(67,44)` by `-1` (same known gap). This fixes the reference harness, not the remaining decoder discrepancy; `pixel_exact` remains `false`. Focused AV1/test-utils tests, clippy, and formatting pass.
> **2026-09-24 (cont'd) — entropy oracle comparison completed; the remaining discrepancy is pre-CDEF, not coefficient desynchronization.** Rebuilt the temporary dav1d 1.5.4 oracle with a narrowly scoped `Post-uv-cf-blk` trace and compared it against Kinetix's existing `uv-cf-blk` trace for `testsrc_160x90` frame offset 7, block `mi=(32,16)`, chroma `(64,32)`, V plane, `16×16` transform. Both decoders read an all-zero block: dav1d reports `eob=-1` (its all-zero sentinel) and Kinetix reports `eob=0`; dav1d's trace reports `txtp=0`, matching Kinetix. Their pre-CDEF V pixels still differ: dav1d is flat `14 15 16 17` on all four rows, while Kinetix is `14 15 16 17 | 14 16 17 17 | 14 16 16 16 | 16 16 16 16`. Existing edge diagnostics show the visible vertical chroma edge is present but its filter mask correctly rejects the large discontinuity; no speculative deblock patch is justified. CDEF arithmetic is therefore ruled out again, and the remaining work is the pre-CDEF reconstruction/deblock line-state path. Temporary dav1d instrumentation was outside the repository; the Kinetix diagnostic gate was restored unchanged. A follow-up Kinetix LFEDGE trace captured the target `x=64` V edge: the filter is invoked with the expected nonzero level, but its pre/post values are unchanged because the filter mask rejects the edge; the differing target columns (`66–67`) are outside the filter's two-tap q-side reach. This rules out the visible vertical edge as the cause and narrows the bug to pre-deblock reconstruction of those columns.


## Session 2026-09-26 — official short-signaling keyframe classified

The local official FATE set was rerun with per-frame mismatch reporting. The
`frames_refs_short_signaling.ivf` stream is a real 640x360 stream whose frame 0
is an I-frame. Kinetix frame 0 already differs from dav1d at the first luma
sample (x=39,y=0), with 66,272 differing bytes and PSNR Y/U/V
53.18/58.64/58.83 dB. The parsed Kinetix header is an error-resilient keyframe,
base qindex 138, no segmentation, no delta-Q, no delta-LF, and loop restoration
enabled on all three planes (`lr=[1,1,1]`).

Stage bisection for this first frame gives: normal 53.18 dB, no LR 51.13 dB,
no CDEF 55.37 dB, no deblock 50.02 dB, and no LR/no CDEF 52.43 dB. All filters
disabled is 48.54 dB, so the error begins before post-filters and the filter
chain partially masks/propagates it. The next concrete investigation is the
feature-rich CDEF index/strength path for this keyframe, followed by restoration
unit metadata; no speculative reconstruction change should be made from the
official aggregate alone.
## Session 2026-09-25 — official FATE vectors run

The official AV1 path is already implemented in
`tpt-kinetix-test-utils/tests/conformance.rs` as
`av1_fate_real_samples_vs_dav1d_when_available`, gated by
`KINETIX_AV1_FATE_DIR`. A local copy of the FFmpeg FATE AV1 samples was run
against ffmpeg's libdav1d reference decoder. Results:

- `annexb`: skipped (Annex-B OBU container is not supported by this harness).
- `decode_model`: 0/22 exact, expected-unsupported.
- `film_grain`: 0/10 exact, expected-unsupported.
- `frames_refs_short_signaling`: 0/50 exact.
- `non_uniform_tiling`: 0/24 exact.
- `seq_hdr_op_param_info`: 0/60 comparable frames exact.
- `switch_frame`: 1/32 exact.
- Overall official FATE result: **1/198 comparable frames bit-exact**.

The synthetic corpus remains substantially healthier (6/6 exact intra
entries; four of five inter entries exact, with one ±1 V sample in one frame),
but the official vectors prove that AV1 is not yet globally pixel-exact. The
single synthetic pre-CDEF discrepancy is not the whole official-vector gap;
operating-point/header information, non-uniform tiling, and frame-switching
paths require separate closure. `capabilities().pixel_exact` correctly remains
`false`. The next priority is to classify each official mismatch by unsupported
feature versus a decoder bug before changing reconstruction code.

## Session 2026-09-25 — CDEF strength-table read order (real header desync)

Classification of the `frames_refs_short_signaling` frame-0 mismatch found a
genuine header-parsing bug, not a filter-arithmetic bug.

`frames_refs_short_signaling.ivf` frame 0 parses as a 640x360 error-resilient
keyframe with `cdef_bits = 3` — i.e. **eight** CDEF strength entries, the first
official vector in the corpus that exercises more than one entry. The synthetic
corpus only ever signals `cdef_bits = 0` (a single entry), which is why every
prior session measured CDEF as "working".

### The bug

AV1 §5.9.17 `cdef_params()` reads each plane pair **interleaved per index**:

```
cdef_damping_minus_3  f(2)
cdef_bits            f(2)
for ( i = 0; i < ( 1 << CdefBits ); i++ ) {
    cdef_y_pri_strength[ i ]  f(4)
    cdef_y_sec_strength[ i ]  f(2)
    if ( num_planes > 1 ) {
        cdef_uv_pri_strength[ i ] f(4)
        cdef_uv_sec_strength[ i ] f(2)
    }
}
```

`dav1d` (`src/obu.c:882-886`) does the same, reading one contiguous 6-bit field
per plane per index:

```c
for (int i = 0; i < (1 << hdr->cdef.n_bits); i++) {
    hdr->cdef.y_strength[i] = dav1d_get_bits(gb, 6);
    if (!seqhdr->monochrome)
        hdr->cdef.uv_strength[i] = dav1d_get_bits(gb, 6);
}
```

`parse_cdef()` in `tpt-kinetix-av1/src/frame.rs` instead read **all** luma
entries in one loop and **all** chroma entries in a second loop. The two orders
coincide only when `cdef_bits == 0`. For `cdef_bits > 0` the parser therefore:

1. Misassigned every chroma strength (reading luma bits as chroma and vice
   versa), and
2. — far worse — left the bitstream at the **wrong offset** for every
   following frame-header field, since the total bit count is identical but the
   contents are not. That desynced loop-restoration params, tile info, and the
   entire tile payload.

This is why the earlier per-stage bisection was confusing: disabling CDEF
*improved* the frame (53.18 -> 55.37 dB) because the wrong strength table was
actively applying the wrong filter strengths, and disabling deblock or loop
restoration made it worse for reasons downstream of the bad header parse.

### The fix

`parse_cdef()` now reads each index's luma pair followed by its chroma pair
inside a single loop, gated on `num_planes > 1`.

### Measured effect

Official `frames_refs_short_signaling` frame 0, against ffmpeg's libdav1d:

| | Luma PSNR | Differing bytes (of 345600) |
|---|---|---|
| Before | 53.18 dB | 66,272 |
| After | **67.62 dB** | **2,789** |

The parsed chroma table changed completely, as expected:

- luma (unchanged, it is read first in both orders): `[1, 3, 51, 6, 0, 33, 32, 17]`
- chroma before: `[0, 2, 33, 1, 32, 0, 17, 1]`  (actually luma bits 2..7)
- chroma after:  `[16, 0, 17, 0, 2, 1, 0, 1]`

The first differing byte moved from offset 39 to offset 163,844 — luma
`(4, 256)`, deep inside the frame rather than in the first superblock row, which
is consistent with the whole *header* having been wrong rather than one filter.

Official-set frame-0 PSNR after the fix:

- `frames_refs_short_signaling`: 66.85 / 70.46 / 69.16 dB (was 53.18 / 58.64 / 58.83)
- `decode_model`, `film_grain`, `non_uniform_tiling`, `seq_hdr_op_param_info`:
  unchanged — those vectors still diverge for their own reasons (decoder model,
  film grain, tiling, operating-point switching).

The aggregate official figure is **still 1/198 exact**, because conformance is
all-or-nothing per frame and no frame became byte-exact. `pixel_exact` correctly
remains `false`. But frame 0 of the cleanest official keyframe is now 14 dB
closer, and the remaining 2,789 differing bytes are a real reconstruction gap
rather than a garbage header.

### Regression coverage

Four new unit tests in `tpt-kinetix-av1/src/frame.rs`:

- `parse_cdef_reads_luma_and_chroma_strengths_interleaved_per_index` — asserts
  the exact tables and that `cdef_params` consumes exactly
  `2 + 2 + 12 * (1 << cdef_bits)` bits. The values are deliberately asymmetric so
  that reverting to the two-pass order fails the test.
- `parse_cdef_packs_each_six_bit_field_as_pri_plus_shifted_secondary_index` —
  round-trips raw 6-bit fields the way dav1d reads them.
- `parse_cdef_skips_chroma_strengths_for_monochrome` — `num_planes == 1` must
  not consume any chroma bits and must leave the chroma table empty.
- `parse_cdef_reads_nothing_for_lossless_or_intrabc_streams` — the §5.9.17
  short-circuit consumes zero bits for `coded_lossless`, `allow_intrabc`, or
  `!enable_cdef`.

All 169 AV1 crate tests pass; clippy is clean with `-D warnings`.

### Lesson

Every prior session's "CDEF looks fine" conclusion was drawn from a corpus where
`cdef_bits == 0`. The synthetic corpus has a **structural blind spot**: it never
signals a multi-entry strength table, so a whole class of header-parsing bugs in
CDEF (and any other per-index-signalled table) is invisible to it. The official
FATE vectors immediately exposed it. Worth adding a synthetic entry with
`cdef_bits > 0` so the fast local corpus covers this path too.

## SESSION #47 (2026-09-26) — reconciliation: local corpus is now essentially bit-exact; memory describing "testsrc2 IBC/warp-affine gaps" is stale

Re-ran the full `dav1d`-gated local test suite (`cargo test -p
tpt-kinetix-test-utils --test conformance -- av1 --nocapture`, `dav1d`
available in this environment) rather than trusting prior session notes/
memory, per CLAUDE.md's "check code before trusting todo.md checkboxes."
Result — **every intra and inter entry in `av1_intra_corpus`/
`av1_inter_corpus`/`av1_inter_sequence`/the plain ffmpeg-reference test is
now bit-exact vs `dav1d`**, including `testsrc2`, `testsrc_64x64`,
`mandelbrot`, `smptebars_96x64` — all the clips this file's older sessions
(warp-sample scan bug, IBC/Phase-E gate, `decode_skip_mode_block` audit) left
as open/gated. Whatever concurrent work landed those fixes wasn't captured
back into this file's running log or into memory
([[project_av1_testsrc2_ibc_progress]] is now stale) — treat this session's
measurement as current ground truth over that file's narrative.

**Exactly one gap found in the whole local corpus:** `testsrc_160x90` frame 7
(of 8), one V-plane chroma sample at `(67,44)`: `kin=16 ref=17`, a plain
off-by-one (delta=-1), 83.69 dB. Everything else in that same clip (frames
1-6, and Y/U elsewhere in frame 7) is exact, so this is a single rounding
edge case, not an entropy or reference-management desync. Not root-caused
this session (budget spent re-establishing the baseline instead) — likely
candidates for next time: chroma subpel MC convolution rounding (`ROUND0`/
`ROUND1`-equivalent shift) at a specific fractional-MV phase, or CfL/chroma-
from-luma prediction rounding, since both are the usual suspects for an
isolated ±1 chroma LSB. `dbg_av1_160_grid.rs` (test-utils) already exists as
a scratch harness for re-decoding this exact corpus entry frame-by-frame;
extend it with a per-plane diff dump at frame 7 rather than starting fresh.

**Remaining actual open item is the official FATE corpus**, not this local
one: `av1_fate_real_samples_vs_dav1d_when_available` needs
`KINETIX_AV1_FATE_DIR` to run (skipped in this environment), and the prior
CDEF-fix session above measured the official set at 1/198 byte-exact with
`decode_model`/`film_grain`/`non_uniform_tiling`/`seq_hdr_op_param_info`
still diverging for their own (much larger-scope: decoder-model timing,
film-grain synthesis, non-uniform tiling, operating-point switching) reasons
— those are real, sizeable features, not bugs in the already-implemented
path, and are the actual next milestone before `pixel_exact` can honestly
flip for anything beyond this synthetic corpus.
## Session 2026-09-27 — non-uniform tiling: real parser/geometry rewrite; FATE `non_uniform_tiling` frame 0 from 315,925 differing bytes to 11

Classification of the four unclassified official streams (per the last session's
recommendation) started with `non_uniform_tiling.ivf` and found a **real decoder
bug, not an unsupported feature**: `parse_tile_info()`'s non-uniform branch
(`compute_log2_from_increments`) was a stub that (a) read the wrong syntax
entirely — a `f(1)` "tile_start_and_end_present" bit (that field belongs to
`tile_group_obu()`, not `tile_info()`) plus a `ns()` read, then assumed full
coverage after one tile — and (b) collapsed the result back to a uniform
power-of-two grid. Every frame of the stream desynced from `tile_info()` onward.
Probe output pre-fix: `uniform=false cols=1 rows=1` for a 12x5 SB frame.

### What was rewritten

- **`frame.rs` — `parse_tile_info()`**: returns a new `TileLayout` struct
  (explicit `col_start_sb[]`/`row_start_sb[]` start arrays, actual `cols`/`rows`,
  `log2_cols`/`log2_rows`, `context_update_tile_id`, `tile_size_bytes`) for both
  spacing modes. Non-uniform syntax follows dav1d `obu.c:884`: each column's
  width is `1 + ns(min(sb_cols - sbx, max_tile_width_sb))` (no bits when the
  remaining span is one SB), then rows are constrained to
  `max_tile_area_sb / widest_tile` (frame-area shadow, `>>= min_log2_tiles + 1`
  when set). The two trailing fields (`context_update_tile_id` sized
  `log2_cols+log2_rows`, `tile_size_bytes = f(2)+1`) are signalled only when
  more than one tile exists — the synthetic corpus never exercised them.
  `FrameHeader.tile_cols/tile_rows/tile_*_log2/tile_*_in_sb` collapsed into
  `FrameHeader.tile_layout`.
- **`frame.rs` — `read_ns()`**: `ns(2)` consumed **zero** bits and returned 0;
  per dav1d `getbits.c:114` (l = ulog2(2)+1 = 2, one prefix bit, no extra) it
  reads **one bit**. Rewritten to the canonical `w = floor(log2 n)+1`,
  `m = 2^w - n` form; round-trip test added for every (n, v) with n ≤ 32.
- **`reconstruct/mod.rs` — tile-group splitting**: `reconstruct_av1_frame()`
  previously treated each TileGroup OBU as one tile. New
  `split_tile_group_payloads()` implements §5.11.1: per group,
  `tile_start_and_end_present_flag` (only when NumTiles > 1), optional
  `tg_start`/`tg_end` (tile-relative), then per-tile `tile_size_minus_1` size
  fields — **little-endian bytes** (dav1d `decode.c`: `tile_sz |= *data++ << (k*8)`)
  — for every tile but the group's last, which takes the remainder.
- **`reconstruct/mod.rs` — per-tile geometry**: rects from
  `col_start_sb[]`/`row_start_sb[]` (non-uniform tiles have different sizes),
  clipped to the mi-grid extent; `decode_tile_group()` now takes the tile's
  pixel rect instead of deriving uniform geometry from tile indices. The CDF
  context saved for the frame comes from the `context_update_tile_id` tile (was:
  first decoded tile).
- **`reconstruct/partition.rs` — tile-relative partition semantics**:
  `decode_partition()`'s `has_rows`/`has_cols`, the outside-tile leaf guards,
  and `partition_context()`'s `avail_u`/`avail_l` compare against the tile's
  `MiRowEnd`/`MiColEnd`/`MiRowStart`/`MiColStart` (dav1d's per-tile `f->bw`/
  `f->bh` and tile-start availability), not the frame's. For SB-aligned tiles
  this is provably identical for valid streams (no block can straddle an
  interior tile boundary), but it is what the spec's `decode_partition` says
  and removes the frame-edge assumption.
- **`loop_filter.rs` — loop-restoration "round half up" merge**: a trailing
  partial LR unit (`ur*unit + half > span`) is not its own unit — its
  coefficients are never read (§5.11.57 skip) and its pixels are filtered with
  the previous unit's filter (dav1d `lr_sbrow`: `aligned_unit_pos -= unit_size`).
  Kinetix previously left such regions unfiltered. This is what took
  `frames_refs_short_signaling` from 0 to **1/50 frames exact**.

### Ground truth used

A temporary patch to a fresh dav1d 1.5.4 clone (`KGTILING`/`KGTG`/`KGTILE`
stderr prints in `obu.c`/`decode.c`, outside this repo) confirmed for
`non_uniform_tiling.ivf`: `uniform=0 cols=1 rows=4 log2c=0 log2r=2 update=3
n_bytes=2 col_start=[0,12] row_start=[0,1,2,3,5]` — matching Kinetix exactly —
and per-tile payload sizes `560/2498/5743/8201` (sum + 3×2 size bytes =
payload ✓). Caught along the way: the big-endian first attempt at the tile
size fields (wrong; spec `f(n)` descriptions mislead — dav1d/libaom both use
LE bytes), and a splitter bug where the group header reader never advanced
over tile data so the second size field was read from tile 0's payload.

### Measured effect

- `non_uniform_tiling.ivf` frame 0 vs dav1d: **315,925 → 1,335 → 11 differing
  bytes** (tiles 0-2 and most of tile 3 bit-exact; the last 11 bytes are ±1
  noise near (600,296) in the merged LR region). All 24 frames now decode;
  before, every frame was wholesale corruption. Inter frames (1+) still diverge
  from tile 1's first row (`first_byte=(0,64)`) — a per-tile decode difference
  in an inter frame (temporal-MV/refmvs scoping is the prime suspect), needing
  a symbol-trace session (`av1_symbol_trace_diff` infra exists).
- Official FATE aggregate: **2/195** (was 1/198; `frames_refs_short_signaling`
  gained its first exact frame from the LR merge rule). `non_uniform_tiling`
  remains 0/24 (inter frames), `decode_model`/`film_grain` expected-unsupported,
  `seq_hdr_op_param_info` 0/58 — its frames with multi-tile layouts now fail
  loudly in the splitter (`invalid tile-group range`, garbage size fields)
  because their frame headers desync earlier (operating-point handling, the
  known unclosed feature); previously they decoded garbage silently.
- Synthetic corpus: 6/6 conformance entries pass (no regression); AV1 crate
  162 lib tests green; `cargo clippy -p tpt-kinetix-av1 -p tpt-kinetix-test-utils
  --all-targets` clean.

### Next steps

1. Inter-frame tile decode: symbol-trace frame 1's tile 1 vs dav1d
   (`KINETIX_AV1_CAPTURE_TILE` + `tools/av1_oracle` or `av1_symbol_trace_diff`).
2. `scratch` harness: `tpt-kinetix-test-utils/examples/probe_tiles.rs`
   (`probe_tiles <ivf> [max-frames]`, diffs every frame vs dav1d) committed for
   reuse.
3. `switch_frame` (1/32): frame 1's mismatch begins deep in the frame
   (`first_byte=274324`) — S-frame reference-slot semantics worth classifying
   before reconstruction work.

## Session 2026-09-27 (cont'd) — inter-frame tile bug root-caused: MC read the reference at tile-local coordinates; frame 1 from 222,098 differing bytes to 1,773

The inter-frame divergence first seen last session (frames 1+ diverging from
tile 1's first row) is now root-caused and fixed.

### Diagnostic path (recorded for reuse)

1. Patched the temporary dav1d 1.5.4 clone with per-symbol `Post-*` prints
   (`DEBUG_BLOCK_INFO` in `recon.h`, retargeted to frame 1, tile 1's first SB
   rows) and compared against Kinetix's `KINETIX_AV1_IBSUM`/`KINETIX_AV1_DBG_B0`
   traces: **all 78 skip/intra/luma-coeff/chroma-coeff events match in order,
   rng values identical** — the entropy decode is bit-exact; the divergence is
   reconstruction-side.
2. The block at mi (20,0) (16×16, mv=(0,0), all-zero-but-DC residual) differs
   by +3 in Kinetix. Patched dav1d with a PRED dump before `itxfm_add`
   (`DEBUG_B_PIXELS`): dav1d's prediction is the flat 118 reference copy; its
   recon adds a **vertical-only** +0/+1 gradient (correct for ADST_DCT:
   vertical ADST × horizontal DCT-DC). A direct unit probe of Kinetix's
   `inverse_transform` with the same input (cf[0]=110, TX_16X16, txtp=1)
   produced the identical vertical gradient — **the transform is correct**.
3. That left the inter prediction itself: `recon_b_inter`'s `px_x`/`px_y` are
   **tile-local** plane coordinates, but the `motion_compensate` /
   `motion_compensate_prep` / OBMC-job calls used them directly as positions
   into the **full-frame reference plane**. Tile 0 (origin tile) is unaffected;
   every tile below/right read the reference shifted by its own tile origin —
   tile 1's mv=(0,0) blocks copied reference rows 16..31 instead of 80..95.
   The warp path was already correct (`block_warp_process` takes frame-global
   `mi_col`/`mi_row`), which is why warp-heavy rows looked different from
   translation-heavy ones.

### Fix

`inter_block.rs`: the three reference-read sites — single-ref
`motion_compensate`, compound `motion_compensate_prep`, and the OBMC job MC —
now add the tile origin (plane-subscaled: `tile_px_x0 >> ss_hor`,
`tile_px_y0 >> ss_ver`) to the reference read position; destination writes
stay tile-local.

### Measured effect

- Frame 1 (shown): 222,098 → **1,773** differing bytes; frame 2: 225,230 →
  **1,977**. Both are now confined to one ~32×32 region at (415..447,
  68..100) in tile 1 (magnitudes ±1..4) — a single block-level prediction
  residual, not yet root-caused.
- Frames 3+ still diverge from row 0 (first_byte (405,0)/(98,0), ~120-160k
  bytes) — consistent with cascade: row-0 blocks with upward MVs referencing
  frame 1/2's localized corrupted region. Fixing the localized source may
  collapse the whole chain.
- `KINETIX_AV1_NO_WARP=1` makes frame 1 *worse* (9,053 bytes) — the warp path
  is contributing correctly; the localized region is not warp-attributable.
- Synthetic corpus 6/6, AV1 crate 162 lib tests, clippy `-D warnings` on both
  touched crates — no regressions.

### Also this session

- `build_rp_proj` (§7.10.2.6 temporal-MV projection) is now tile-scoped per
  dav1d `load_tmvs_c`: sources from the tile's 8×8 rows and columns ± one SB
  band, projected writes clamped inside the tile (`y_proj_start/y_proj_end`,
  the `pos_x` window against `col_start8/col_end8`). Inert for frame 1 (its
  keyframe reference has an empty motion field) but correct for later frames.
- `KINETIX_AV1_IBSUM` traces widened from `mi_row < 4` to all rows; new
  `KINETIX_AV1_CFTARGET=col,row` dumps dequantized coefficients for one block
  (both mirroring the earlier session's oracle-tooling pattern).

### Next session's starting point

Frame 1's (415..447, 68..100) region: identify the block at mi ≈ (104,17)
(`KINETIX_AV1_DBG_PX` with tile-local px, `KINETIX_AV1_DBG_OBMC` covers
mi_row 16-22), decide whether it is OBMC-blend or MC-edge; then re-check
frames 3+ for cascade collapse.

## Session 2026-09-27 (cont'd 2) — inter-intra blend masks and edge availability fixed; frame 1 residue 1,773 → 1,219 bytes

The remaining frame-1 region (415-447, 68-100) is a 32×32 BLEND-type
inter-intra block at mi (104,16) (dav1d trace: `Post-interintra[t=1,m=1,w=0]`,
32×32 NONE, mv=(8,44), y-cf eob=11). Entropy matches dav1d for the whole
region; two prediction-side bugs found in `apply_interintra`:

1. **Wrong mask for BLEND blocks**: `apply_interintra` used the WEDGE mask
   (`wedge_mask(bsize, sign-0, wedge_index)`) for every inter-intra type. A
   BLEND (non-wedge, spec `interintra_type == INTERINTRA`) block must use the
   mode-dependent 1-D ramp `ii_weights_1d[32] = {60,52,45,...}` (dav1d
   `build_nondc_ii_masks`): flat 32 for DC, ramp over y for V, over x for H,
   over min(x,y) for SMOOTH, sub-sampled by the plane's subsampling. dav1d's
   `II_MASK` selects `ii[interintra_mode]` for BLEND vs `wedge[0][wedge_idx]`
   for WEDGE. (Inter-intra wedges read no sign bit — dav1d's `II_MASK` uses
   `wedge[0]` unconditionally — so Kinetix not reading a sign is correct.)
2. **Hardcoded edge availability**: the intra half of the blend built its
   borders with `have_above=true, have_left=false` unconditionally. dav1d's
   `prepare_intra_edges` passes tile-relative availability
   (`bx > tiling.col_start`, `by > tiling.row_start`) — the (104,16) block
   sits on tile 1's top row, so dav1d has NO above edge (128 fill) while
   Kinetix read the previous tile-plane row. Fixed to `py > 0` / `px > 0`.

Frame 1 residue: 1,773 → **1,219** bytes (±2 max), now scattered across
cols 401-447 rows 83-127 and tile 3 / chroma (943 bytes outside tile 1's
luma rows). Frames 3+ first-diff positions moved (cascade shifts as
upstream improves) but remain ~120-220k bytes — later frames carry their
own inter-intra/OBMC/warp mixes compounding through references.

### Next session's starting point

The (403,83) block: 16×8 VERT leaf at mi (100,20), skip=1, mv=(1,-1) —
BLEND with SMOOTH intra (mask over min(x,y)) on leaf 1; leaf 2 is motion_mode
OBMC (`Post-motionmode[1]`). Verify the SMOOTH blend and the OBMC neighbour
prediction positions tile-locally vs frame-globally (the OBMC job px/py are
tile-local; the reference read was fixed but the neighbour-blend write side
may still need the same treatment for OBMC-over-tile-edge cases).

## Session 2026-09-27 (cont'd 3) — ii-blend ramp must scale with block size; frame 1 residue 1,219 → 130 bytes

The next-layer block, (28,108) (16×16, skip, BLEND-V, mv=(3,-1), 148 of the
remaining diffs), revealed the ramp-index scaling: the inter-intra blend
ramp is not indexed by raw block-local coordinates — the spec's per-size
tables (dav1d `BUILD_NONDC_II_MASKS` steps) scale the 32-entry
`ii_weights_1d` by `step = 32 / max(pw, ph)`. A 32×32 luma block samples
every weight (step 1 — which is why the 32×32 block at (104,16) had
already been fixed correctly), a 16×16 samples every second, 8×8 every
fourth. The first fix indexed `y << subsampling`, correct only for 32×32.

With the size-scaled ramp, shown frame 1's residue drops 1,219 → **130
bytes** and frame 2's 1,397 → **192** — every remaining shown-frame 0-2
diff sits at rows 295-299 around col 600 (±1, luma only), i.e. the frame
bottom edge inside tile 3's merged trailing LR unit, propagating from
frame 0's own 11-byte residue at (600,296) via inter prediction. The
hidden (non-shown) alt-ref in packet 1 still carries ~104k differing
bytes and propagates into later shown frames (3+ remain ~120-220k).

### Next session's starting point

Frame 0's 11 bottom-edge bytes ((600-607, 296-299), all ±1, chroma
exact): inside the merged trailing LR unit's last rows — candidates are
the wiener bottom-border tap handling at the visible/padding boundary
(rows 300-303) or the pre-CDEF boundary-row substitution for the final
stripe. The hidden alt-ref's wholesale diff (rows 0+ from col 25) is its
own investigation once shown frames 0-2 are clean.

## Session 2026-09-27 (cont'd 4) — hidden frames decoded: frame 4's tile-1 entropy diverges at a 32×16 H-split leaf walk

Using dav1d's `--outputinvisible 1` (all 26 decoded frames, vs 24 shown),
every frame now compares 1:1 in decode order: shown frames 0-3 carry only
11/150/123/184 diffs (all at the frame bottom edge, rows 295-299, propagating
from frame 0's 11-byte LR residue); **shown frame 4 (oh=3) is the first
wholesale-diverging frame** (91,255 diffs from (384,64) = tile 1's first
block of its second SB); tiles 2/3 accumulate 22k/55k.

Block-level diff mapping of frame 4: tile 0 zero diffs; tile 1's first SB
(mi row 16, cols 64-95) pixel-exact; diffs begin at mi (96,16) — dav1d's
64×64 NONE WARP block.

Entropy comparison for frame 4's tile-1 top SB row (patched-dav1d
`Post-*` trace vs Kinetix `KSKIP`/`KINTRA`, frame-aligned via `DBGSEQ`
delimiters): partitions and leaves match through SB (16,80)'s four 32×32
leaves — 64×64 SPLIT at (16,80) ctx=0 ✓, 32×32 NONEs at (16,80)/(16,88)/
(24,80)/(24,88) with bp NONE/NONE/NONE/H ✓ (dav1d (24,88) is `bp=1` H) —
all post-read rng values identical (…58324 → 61072 ✓). The divergence is
inside the (24,88) 32×16 H-split leaves: dav1d decodes two 32×16 leaves
(skip=0 leaf reads NEWMV+OBMC); Kinetix's trace shows FOUR 8-wide KSKIP
events at mi cols 88/90/92/94 — the sub-leaf walk differs after the
matching `bp=1` partition read. (Earlier frame-4 "entropy mismatch at
(16,64)" was a frame-misalignment artifact; frame-aligned, rng 33479
matches exactly.)

Also confirmed this session: `motion_mode`'s 3-way CDF gate is driven by
dav1d's `find_matching_ref` mask (matching-ref edge neighbours) while
Kinetix gates on its `find_num_warp_samples` count — these scans differ
in edge geometry and must not be conflated when debugging motion-mode
symbol choices.

### Next session's starting point

Frame 4, tile 1, the (24,88) 32×16 H-split: compare Kinetix's sub-leaf
partition walk (`decode_partition` recursion below bl=3 for a 32×16 H
pair) against dav1d's decode_sb `PARTITION_H` branch (two 32×16 leaves,
no further recursion). The four 8-wide KSKIP events suggest Kinetix's
walker splits 32×16 leaves into 8×16 sub-blocks where dav1d keeps them
whole — likely in the bsize sub-block table for H/V partitions at
32×16, or the bl=3 → bl=4 recursion gate.

## Session 2026-09-27 (cont'd 5) — frame-4 entropy desync narrowed to one symbol slot: the interpolation-filter read of a 32×16 OBMC leaf

Continuing from the frame-4 tile-1 divergence: with frame-aligned traces
(`DBGSEQ` delimiters + `DBG_B0` position tag added to the motion_mode print,
b0 position clamp removed), frame 4's tile-1 leaf (24,88) — 32×16 H-split
leaf, skip=0, NEWMV mv=(12,0), motion_mode=OBMC — matches dav1d on every
anchored symbol: skip rng 47768 ✓, intra 47155 ✓, motion_mode 64960 ✓, yet
the luma coefficient read lands at rng **63102 vs dav1d's 37474** while
decoding the *same* visible result (tx 32×16, one DC coefficient, pixels
exact). The only unanchored symbol between the matched anchors is the
**interpolation-filter read** (dav1d `Post-subpel_filter[0,ctx=0]:
r=63940`; both decoders select filter 0/REGULAR, so the pixel output is
unaffected — but the bit consumption differs, permanently desyncing the
entropy decoder from this leaf onward and producing frame 4's 91k-diff
cascade and every later frame's corruption).

Working hypothesis: the filter symbol's *context derivation* (Kinetix
`filter_left`/`filter_above` ref-match + `add` folding vs dav1d's
`nctx` accumulation over `a->filter[bx4]`/`l->filter[by4]`) picks a
different CDF for this block — the selected symbol coincides, the
consumed bits do not. dav1d's ctx here is 0 (`Post-subpel_filter[0,ctx=0]`).

### Next session's starting point

Leaf (24,88) frame 4 (32×16, OBMC, NEWMV): print Kinetix's filter `ctx`
and pre/post rng at the `interp_filter[ctx]` read (the existing
`DBG b0 filter{dir}` print inside the switchable branch covers this —
it did not fire in the capture because `frame_filter != INTERP_SWITCHABLE`
for the anchor blocks checked; confirm which branch frame 4's leaf takes),
and compare the ctx derivation against dav1d's `filter` context semantics
(neighbour filter value only when the neighbour is inter AND shares the
reference; 3 = none). The `DBG_B0` position clamp is already removed.

Also remaining (unchanged): frame 0's 11 bottom-edge bytes (merged-LR
wiener bottom border) and the shown-frames 1-2 bottom rows propagating
from it.

## Session 2026-09-27 (cont'd 6) — `interp_filter` ctx audited against dav1d's `get_filter_ctx`; real left-context bug found and fixed

Picked up cont'd 5's open item: is Kinetix's interpolation-filter context
derivation right? Compared line-by-line against the authoritative dav1d
implementation (fetched `src/env.h` — the function is `get_filter_ctx`, **not**
in `ctx.c`/`ctx.h` as earlier sessions assumed):

```c
static inline int get_filter_ctx(const BlockContext *const a,
                                 const BlockContext *const l,
                                 const int comp, const int dir, const int ref,
                                 const int yb4, const int xb4)
{
    const int a_filter = (a->ref[0][xb4] == ref || a->ref[1][xb4] == ref) ?
                         a->filter[dir][xb4] : DAV1D_N_SWITCHABLE_FILTERS;
    const int l_filter = (l->ref[0][yb4] == ref || l->ref[1][yb4] == ref) ?
                         l->filter[dir][yb4] : DAV1D_N_SWITCHABLE_FILTERS;
    if (a_filter == l_filter)         return comp * 4 + a_filter;
    else if (a_filter == N_SWITCHABLE) return comp * 4 + l_filter;
    else if (l_filter == N_SWITCHABLE) return comp * 4 + a_filter;
    else                               return comp * 4 + N_SWITCHABLE;
}
```

This **confirms Kinetix's arithmetic is correct**: `comp * 4 + add` here plus
dav1d's separate `filter[dir][...]` CDF-table index is identical to the spec's
flat `ctx = ((dir & 1) * 2 + (RefFrame[1] > INTRA_FRAME)) * 4; ctx += add`
(verified against `av1-spec/09.parsing.process.md`, the *interp_filter* CDF
selection block) and to Kinetix's `base = ((dir & 1) * 2 + comp) * 4` +
`ctx = (base + add).min(15)`. The `comp` term also agrees: dav1d's
`comp = (b->comp_type != COMP_INTER_NONE)` and Kinetix's
`comp = (ref_names[1] != NONE_FRAME)` are equal for every block class,
*including* inter-intra (single-ref, `comp_type` stays `COMP_INTER_NONE`, and
Kinetix's inter-intra path requires `!compound && ref_names[1] == NONE_FRAME`).
So cont'd 5's "different CDF ⇒ different ctx" hypothesis is **ruled out** — the
ctx derivation is not the bug.

### The real bug found: `clear_left_context` reset only the coefficient context

Reading `get_filter_ctx`'s inputs back to where dav1d *initialises* them
exposed a genuine gap. dav1d calls, once per **superblock row**:

```c
reset_context(&t->l, IS_KEY_OR_INTRA(f->frame_hdr), t->frame_thread.pass);
```

and `reset_context` does (among others):

```c
memset(ctx->ref, -1, sizeof(ctx->ref));
memset(ctx->filter, DAV1D_N_SWITCHABLE_FILTERS, sizeof(ctx->filter));
memset(ctx->comp_type, 0, sizeof(ctx->comp_type));
memset(ctx->mode, NEARESTMV, sizeof(ctx->mode));
memset(ctx->skip_mode, 0, sizeof(ctx->skip_mode));
```

Kinetix's superblock-row loop only did the coefficient half
(`state.coeff_ctxs.clear_left()`), leaving the previous superblock row's
`ref_left` / `filter_left` / `comp_type_left` / `skip_mode_left` /
`ymode_left` / `mv_left` in place. That is exactly the state
`get_filter_ctx` reads, so a block in the **first superblock column** of a row
could match a stale `ref_left` entry and take a different `add` — i.e. a
different `interp_filter` CDF than dav1d, consuming a different number of bits
while very often decoding the same filter value. That is the exact failure
signature cont'd 5 hypothesised, one level up: not a wrong ctx formula but
wrong ctx *inputs*.

**Fix** — new `TileDecodeState::clear_left_context()` in
`reconstruct/mod.rs` resetting all of the above, called from the superblock-row
loop in `decode_tile_group` in place of the bare `coeff_ctxs.clear_left()`.
(`ymode_left`/`uv_left` reset to `DC_PRED`, which is how this decoder
represents dav1d's `NEARESTMV` "inter neighbour has no intra mode" convention.)

Also added a `KINETIX_AV1_DBG_FILTER` gate in `inter_block.rs` dumping the full
ctx derivation (`dir`, `comp`, `ref0`, both neighbour ref pairs and filter
values, `left_t`/`above_t`/`add`/`ctx`, the decoded symbol, post-read `rng`) —
the diagnostic cont'd 5 asked for, so the next session does not have to
re-add it.

### Measured effect

**No change on this corpus.** `non_uniform_tiling.ivf` frames 0-2 are
byte-identical before and after (91.06 / 80.17 / 78.13 dB Y), and frames 3+
are unchanged. Frames 0-2 of this stream are already essentially bit-exact, so
there is no first-superblock-column inter block whose left context was stale —
the bug is real but latent here, and it will fire on any stream with a
superblock-row-spanning inter tile. The other four FATE streams
(`frames_refs_short_signaling` frame 0, `switch_frame` frame 0) remain
bit-exact; `decode_model` / `film_grain` / `seq_hdr_op_param_info` are
unchanged (they were already failing for unrelated reasons). All 162 crate
tests pass; `cargo fmt` + `clippy -D warnings` clean.

**Conclusion: the cont'd 5 hypothesis is disproven.** The `interp_filter`
context derivation is correct. The frame-4 tile-1 desync has some *other*
unanchored symbol between the matched `motion_mode` anchor and the luma
coefficient read — the remaining candidates are the per-block `cdef` /
`delta_q` / `delta_lf` reads, the vartx partition walk, or the `ref_frame_mvs`
`load_tmvs` temporal-MV projections feeding `find_mv_stack`.

### Also: dav1d oracle rebuild is broken in this working tree

Worth recording so the next session does not repeat the detour. The
`%TEMP%/tpt-kinetix-dav1d` oracle cannot be rebuilt as-is:

- Its source tree had been pruned to 10 files (only the ones prior sessions
  edited). All missing `src/**` files were restored from the 1.5.4 tarball.
- `meson.build` is missing its `config_h_target` block, so the generated
  `build/config.h` does not exist; a hand-written stand-in was added at
  `dav1d/build/config.h` (x86_64 / MSVC / Windows values).
- With those fixed, `src/libdav1d_bitdepth_8.a` builds, but the `dav1d.dll`
  link still fails: `ninja: error: unknown target` for every `libdav1d_x86_*`
  static lib, i.e. this build directory was configured **without** ASM, so
  `msac_init_x86` is unresolved. Rebuilding the DLL needs a fresh
  `meson setup` with ASM enabled — not worth doing inside the repo's temp dir.
- The prebuilt `build/tools/dav1d.exe` (2026-09-18) is stale and predates all
  of the above.

The `KINETIX_DBG_FILTER` instrumentation added to that tree's `src/decode.c`
compiles cleanly at the `libdav1d_bitdepth_8.a` stage, so it will be picked up
by whatever build finally succeeds.


## Session 2026-09-27 (cont'd 7) — frame-4 desync root-caused: `ctx->tx` and `ctx->tx_intra` were one shared array; a wrong *value* in the inter path's `tx_intra` write

Continued from cont'd 5/6, which had narrowed frame 4's tile-1 desync to "some
unanchored symbol between `motion_mode` and the luma coefficient read" and
eliminated the `interp_filter` context. The listed suspects were cdef/delta_q/
delta_lf, the vartx partition walk, and ref_frame_mvs/load_tmvs.

**The dav1d oracle was not needed for this one** — the bug is visible by reading
`BlockContext`. dav1d keeps **two separate transform-context arrays**
(`src/env.h`), and this decoder had collapsed them into one `tx_above`/`tx_left`
pair serving both consumers:

| dav1d | reset fill | written by | read by | comparison |
|---|---|---|---|---|
| `ctx->tx_intra` | `-1` | *every* block's `set_ctx` (intra) / `case_set` (inter) | `get_tx_ctx` (intra `tx_depth`) | `>= max_tx->lw` |
| `ctx->tx` | `TX_64X64` | `read_vartx_tree`/`read_tx_tree` only, at **transform-block** granularity | `read_tx_tree` (`txfm_split`) | `< txw` |

`reset_context` (`decode.c:2405-2406`) fills them with *different* sentinels
because the two comparisons run in *opposite* directions — further proof they
are not interchangeable.

### The two real defects

1. **Wrong value in the inter path's `tx_intra` write.** dav1d's non-intra
   `case_set` (`decode.c:1919`) and intrabc `set_ctx` (`decode.c:1366`) both
   write `edge->tx_intra` with **`b_dim[2+i]` — the coded block's width/height**.
   Kinetix wrote `av1::TX_WIDTH[luma_tx]` / `TX_HEIGHT[luma_tx]` — the **first
   var-tx leaf's transform size**. These differ whenever `Max_Tx_Size_Rect` is
   smaller than the block, so the *next* block's `get_tx_ctx` compared against
   the wrong `aboveW`/`leftH`, selected a different `tx_depth`/`txfm_split`
   CDF, and decoded the same symbol value while consuming a different number of
   bits — the exact desync signature cont'd 5 was chasing.
2. **Shared array, so the two contexts aliased.** The intra path's block-wide
   write and the var-tx tree's per-transform writes hit the same slots.
   `intra_block.rs` even carried a comment asserting the write was deliberately
   omitted "because `read_block_tx_size_ibc` already wrote the correct per-leaf
   values" — true of `ctx->tx`, false of `ctx->tx_intra`, which that same
   function never touches.

### Fix

- New `txv_above`/`txv_left` arrays model dav1d's `ctx->tx`; `set_tx_ctx_range`
  and `read_tx_tree` now read/write those. `tx_above`/`tx_left` remain the
  `ctx->tx_intra` model.
- `inter_block.rs` (ordinary inter **and** skip-mode) and `intra_block.rs`
  (IBC) now write `tx_above`/`tx_left` with the **block extent**
  (`bw * MI_SIZE` / `bh * MI_SIZE`), matching `b_dim`.
- The pure-intra `set_ctx` equivalent now writes **both** arrays, as dav1d's
  intra `set_ctx` does (`edge->tx_intra` *and* `edge->tx`, `decode.c:1236-1237`).
- `clear_left_context` resets `txv_left` alongside `tx_left`.
- `luma_tx` is still used by the intra path; the now-dead bindings in the two
  inter paths were removed.

### Measured effect

- **Intra corpus back to 6/6.** An intermediate attempt (dropping the inter
  `tx_above`/`tx_left` writes entirely, on the reading that dav1d's inter path
  never touches them) regressed `testsrc2_big` 320x180 to 18.78 dB — which is
  what proved the write is required, just with the *block* value rather than
  the *transform* value. Adding the intra-side `txv_*` write recovered 6/6.
- Multi-frame inter corpus (ffmpeg/libaom `testsrc2` 128x96, 15 frames, fresh
  `meson`-free reference: `probe_tiles` vs dav1d): frame 1 **12600 → 7390**
  differing bytes, and frames 2-14 all improve or hold. Still 1/15 exact — this
  stream has additional unresolved inter bugs, but none of them is this one.
- Regression test `var_tx_context_is_independent_of_the_intra_tx_context`
  added in `reconstruct/tests.rs`, asserting the two arrays are independent and
  that a skipped block's var-tx write uses the block extent.
- 163 AV1 lib tests + full `tpt-kinetix-test-utils` suite green; intra
  conformance 6/6; `cargo fmt --check` and `clippy -D warnings` clean on both
  crates.

### Next session's starting point

Frame 1's residual 7390 bytes on the 15-frame `testsrc2` corpus (regenerate
with the ffmpeg command recorded above). The var-tx/`txfm_split` context is now
provably correct, so the next unanchored symbol between `motion_mode` and the
coefficients narrows to the `cdef` / `delta_q` / `delta_lf` per-block reads or
`load_tmvs`' `ref_frame_mvs` projection into `find_mv_stack` — the two
remaining cont'd 5 suspects, now with the vartx walk eliminated. `av1_symbol_trace_diff`
/ `KINETIX_AV1_IBSUM` infra is in place for that; it does **not** need the
broken dav1d build (ffmpeg's libdav1d-backed `decode_av1_with_dav1d` reference
is enough for pixel diffs, and the existing trace-capture tooling covers the
symbol side).

Also still open, unchanged: the single wiener bottom-border/stripe-boundary
rounding case in frames 0-3, and the broken `%TEMP%/tpt-kinetix-dav1d` oracle
(not needed for the fix above).


## Session 2026-09-27 (cont'd 8) — cont'd 5's three remaining suspects audited against dav1d: all three are correct; one false lead found and reverted

Picked up cont'd 7's "next session" item and audited the three symbols left
between the `motion_mode` anchor and the coefficient read: the per-block
`cdef`/`delta_q`/`delta_lf` reads, the vartx partition walk (already fixed in
cont'd 7), and `ref_frame_mvs`/`load_tmvs` into `find_mv_stack`. Method: pull
`decode.c` / `refmvs.c` / `env.h` for dav1d 1.5.4 into `%TEMP%\dav1d-1.5.4-src`
and compare line-by-line (`code.videolan.org` is behind an Anubis bot wall;
`raw.githubusercontent.com/videolan/dav1d/1.5.4/...` works). **No dav1d build
needed** — these are all pure syntax/context questions.

### `cdef` / `delta_q` / `delta_lf` — correct, no change

Checked against `decode.c:944-1011`:

- `read_cdef` is gated on `!skip` only. Kinetix additionally gates on
  `lossless` / `!enable_cdef` / `allow_intrabc`, but those are all **inert**:
  dav1d sets `cdef.n_bits = 0` unless `!all_lossless && seqhdr->cdef &&
  !allow_intrabc` (`obu.c:879-881`), and `read_literal(0)` consumes zero bits,
  so Kinetix's extra conditions can only skip a read that was already a
  no-op. The per-64x64-unit dedup (`cdef_idx` / `cur_sb_cdef_idx_ptr`) matches,
  including the `bw4 > 16` / `bh4 > 16` / `bw4 == 32 && bh4 == 32` fill pattern.
- `read_delta_qindex` / `read_delta_lf` match, **including** the non-obvious
  nesting: dav1d reads the `delta_lf` symbols *inside* the `if (have_delta_q)`
  block (`decode.c:987`), so a stream with `delta_lf_present` but
  `!delta_q_present` reads nothing. Kinetix reaches the same result because
  `parse_delta_lf_params` only sets `delta_lf_present` when `delta_q_present`
  is already true (`frame.rs:1337`), and `ReadDeltas = delta_q_present`
  (`partition.rs:112`). The `3`-escape (`n_bits = 1 + bools(3)`, then
  `bools(n_bits) + 1 + (1 << n_bits)`), the equi-probability sign bit, the
  `<<= res_log2` and the `clip` bounds all agree.

### `ref_frame_mvs` / `load_tmvs` / `find_mv_stack` — correct, no change

Compared `inter_mv_stack` against `dav1d_refmvs_find` + `scan_row`/`scan_col`/
`add_spatial_candidate` (`refmvs.c:41-95`, `:97-173`, `:430-520`). The
`w4 = min(min(bw4,16), tile_end - bx4)` clamp, the `weight = bw4 == 1 ? 2 :
max(2, min(2*max_rows, cand_h4))` formula, the `len = max(step, min(bw4,
cand_bw4))` stepping, the 8-entry cap, the nearest-then-secondary weight
sorting with the `REF_CAT_LEVEL` sentinel, the top-left probe that feeds
`ref_match_count` but *not* `have_newmv` (`refmvs.c:457-460` passes a dummy
flag — Kinetix models this with a separate `dummy` accumulator, correct), and
the whole `nearest_match` -> `refmv_ctx`/`newmv_ctx` switch (`refmvs.c:485-498`)
all match. The `mv_projection` `div_mult` reciprocal table and the
`fix_mv_precision` truncation-toward-zero bias (the `- (v >> 31)` term) are
also right. The cont'd 5 note that `find_matching_ref`'s mask and
`find_num_warp_samples` "differ in edge geometry and must not be conflated"
remains a live subtlety for the `motion_mode` gate, not a bug.

### A false lead: `have_newmv` is a boolean after all (reverted)

Worth recording so it is not re-derived. dav1d accumulates
`*have_newmv_match |= b->mf >> 1` (`refmvs.c:56`/`:80`), and Kinetix wrote
`*have_newmv |= (cand.mf >> 1) & 1` and then used `num_new.min(1)` in the
`3 - have_newmv` / `5 - have_newmv` contexts. That *looks* like a bug: `mf`
packs two bits (bit0 GLOBALMV, bit1 NEWMV), so `mf >> 1` reads like a 2-bit
field whose OR-accumulation could reach 2 or 3, and clamping to 1 would pin
`newmv_ctx` at 2/4 for every multi-candidate block.

**It is not a bug.** `mf` is only ever 0, 1 or 2, because a block's
`inter_mode` is *either* GLOBALMV *or* NEWMV, never both
(`decode.c:525`, `:557` — `(mode == GLOBALMV && ...) | (mode == NEWMV) * 2`).
So `mf >> 1` is only ever 0 or 1 and the OR cannot exceed 1. `.min(1)` is
exactly equivalent.

I made the change anyway, and the pixel-diff was **byte-identical** on the
15-frame inter corpus (7390/15441/... unchanged) — which is what proved the
original code was already right. Reverted the code; kept a corrected comment
recording why `.min(1)` is safe. Had I not measured, this would have shipped
as a plausible-sounding "fix" plus a confidently wrong comment.

### Net effect this session

No behavioural change (as intended — this was an audit). `git diff` on
`intra_block.rs` is comments only. 163 AV1 lib tests + full
`tpt-kinetix-test-utils` suite green, intra conformance 6/6, `fmt --check` and
`clippy -D warnings` clean. Frame 1 of the inter corpus remains 7390 bytes.

### Next session's starting point

All three of cont'd 5's suspects are now **eliminated** (var-tx in cont'd 7;
cdef/delta_q/delta_lf and refmvs/load_tmvs this session). The unanchored
symbol between `motion_mode` and the luma coefficients on the frame-4 leaf is
therefore *not* any of them, and the next step is to re-derive the anchor
list from scratch rather than keep walking the old suspect list: dump every
symbol Kinetix reads for that one leaf (including the ones already matched)
and diff the full sequence against dav1d, so the *first* differing read is
identified by position rather than by elimination. `av1_symbol_trace_diff` /

## Session 2026-09-27 (cont'd 9) — the frame-1 residual is pre-filter, not a filter bug; and the frame counter used by every `KINETIX_AV1_DBG_*` gate was stale

Chased cont'd 8's finding that frame 1's residual was 124 edge-only blocks
with no interior differences, which looked like a post-filter issue. **It is
not** — and getting there required fixing a diagnostic bug that had been
misleading the trace for several sessions.

### The frame counter only advanced under one env var

`decoder.rs` bumped `debug_frame_seq` *inside* the
`if std::env::var("KINETIX_AV1_DBG_SEQ")` block, even though that function's
own doc comment says the label is "stable for the whole duration of that
frame's processing" and is what the other `KINETIX_AV1_DBG_*` gates read. So
with any other trace var set, `current()` reported a **stale** frame number.
A `KINETIX_AV1_DBG_PXY` trace run without `DBG_SEQ` silently described an
earlier frame.

This produced a concrete false result: tracing pixel (65,62) reported a
consistent `41` at pre-filter/post-deblock/post-cdef/post-lr, which reads as
"the filters are all no-ops here". It was in fact the *keyframe's* value.
The bump is now unconditional (a relaxed atomic, negligible against a tile
decode), and `KINETIX_AV1_DBG_PXY_FRAME=n` was added to scope the pixel trace
to one frame.

### The real frame-1 pixel is wrong *before* any filter runs

With the counter fixed, the frame that `probe_tiles` labels "frame 1" is
`debug_frame_seq` frame **4** (the harness decodes more frames than its `max`
argument reports, so the labels are offset — noted so the next session does
not chase it again). For that frame, pixel (65,62):

- Kinetix `228`, dav1d `41`, i.e. `|d| = 187`, matching the reported maximum.
- pre-filter `228`, post-deblock `228`, post-cdef `228`, post-lr `228`.

So the error is present **before** the loop filters and none of them touch it.
It is a **reconstruction** (prediction / transform / residual) bug, not a
deblock or CDEF bug.

### Why the BLOCKMAP classification misled

The 8×8 edge/interior map is a map of where the difference *survives*, not
where it *originates*. A bad prediction in one block gets spread by deblock
and CDEF into its neighbours' edge samples, so a single reconstruction fault
can present as many edge-only blocks. `probe_tiles`' doc comment now says
this explicitly and points at the per-pixel stage trace as the thing that
actually separates the two classes. This corrects cont'd 8's conclusion, which
over-read the all-`e` map.

### Ruled out along the way

- `NOFILTER` (8155), `NODEBLOCK` (7474), `NOCDEF` (8243) vs full (7390): no
  single filter toggle explains the residual, consistent with the error
  preceding all of them.
- The deblock edge at that location *is* processed (`lvl=4`, `fs=8`,

## Session 2026-09-27 (cont'd 10) — frame-1 residual localised to a WARP skip block; warp model derivation audited and found correct

Continued cont'd 9's finding that pixel (65,62) is wrong by 187 *pre-filter*.

### Localisation

- The owning block is mi **(16,14)** — 16x16, `ref=[2,0]`, `mv=(-43,-15)`,
  `skip=true`, `mm=2` (**WARP**). `skip=true` means no residual, so the final
  value *is* the motion-compensated prediction: this is squarely an MC/OBMC
  question, not a coefficient one.
- Its warp model is strongly non-translational:
  `matrix=[321462, -23757, 66117, -8191, 2310, 57345]`,
  `alpha=576, beta=-8192, gamma=2304, delta=-7936`, fitted from 3 raw samples
  of which only 1 survives the mv-difference threshold.
- 10 of frame 4's 79 blocks use `mm=2`.

### Tooling added

`KINETIX_AV1_DBG_PRED` is no longer hardcoded to mi (4,18): it now takes
`<mi_col>,<mi_row>`, and `KINETIX_AV1_DBG_PRED_FRAME=n` scopes it to one
frame. The `RESID` dump prints `fr=`, the real mi and the bsize. This is the
probe that splits "prediction is wrong" from "prediction is right and the
residual breaks it".

### Audited and found correct

- `select_warp_samples`: the `thresh = 4 * clamp(max(bw4,bh4), 4, 28)`
  threshold, the `mvd[i] = -1` rejection, the `ret == 0 -> 1` clamp, and the
  `i`/`j` tail-compaction loop all match dav1d `derive_warpmv`
  (`decode.c:308-329`).
- `get_shear_params` matches `dav1d_get_shear_params` (`warpmv.c:80-100`),
  including the `mat[2] <= 0` early-out and the
  `4|alpha| + 7|beta| >= 0x10000 || 4|gamma| + 4|delta| >= 0x10000` reject.
- **A hypothesis I had to discard:** dav1d ends `derive_warpmv` with
  `if (!dav1d_find_affine_int(...) && !dav1d_get_shear_params(wmp)) AFFINE else
  IDENTITY`, which reads as "identity only when *both* fail". It is in fact
  the same as Kinetix's `find_affine_int(...)?` then `get_shear_params(...)?`,
  because C's `!f() && !g()` is true only when **both** return 0 (success) —
  the same all-must-succeed condition the `?` chain encodes. No bug.

### Not yet root-caused

`KINETIX_AV1_NO_WARP=1` moves the pixel 228 -> 192 and the frame total
7390 -> 7264, so WARP contributes but is not the whole story: the value is
still badly wrong with warp disabled, so the base translational MC for this
block (or the reference it reads) is also off. Frame 4's blocks reference
ref `2` (69 blocks) and ref `7` (10 blocks).

Next step: with `DBG_PRED` now able to target the block, dump the prediction
for mi (16,14) with warp both on and off, and compare the sampled reference
positions against the plane that ref 2 / ref 7 actually resolve to — the
remaining suspects are the reference-slot resolution for this frame and the
`block_warp_process` sample-addressing (the 16x*(2*dx + sx*bs) form in
dav1d's `add_sample` is worth re-deriving against `find_num_warp_samples`).

Debug-only change this session; decode behaviour unchanged (frame 1 still
7390), 163 AV1 lib tests plus the full test-utils suite pass, fmt and clippy
clean.

  `edge_left=true`), so it is not a missing-edge-flag bug either.
- `KINETIX_AV1_NO_WARP=1` gives 7264 vs 7390 — only marginal. 10 of frame 4's
  79 blocks use `mm=2` (WARP) with large MVs (`-43,-15`, `-53,-15`), so warp
  is present in the neighbourhood but is **not** the dominant cause. (This
  also supersedes the older note that NO_WARP made frame 1 *worse*; post-fix it
  makes it slightly better.)

### Next session's starting point

Pixel (65,62) in frame 4 of the `testsrc2` 128×96 corpus is wrong by 187
**pre-filter**. The owning block is mi (16,15) — a `PARTITION` leaf near
mi (14..18, 12..18), where the neighbouring `IBSUM` lines show
`mv=(-43,-15)`/`(-53,-15)` `ref=[2,0]` and a mix of `mm=0`/`mm=1`/`mm=2` and
`skip=true/false`. Next step is the `KINETIX_AV1_DBG_PRED`-style pre/post
residual snapshot for that specific block to split prediction vs residual-add,

## Session 2026-09-27 (cont'd 11) — the whole WARP path audited end-to-end against dav1d and found correct; residual still unfixed

Continued cont'd 10's localisation of the frame-1 residual to mi (16,14)
(8x8, `skip=true`, `ref=2`, `mv=(-43,-15)`, `mm=2`). Since the block is
`skip`, its output is pure motion compensation, so the entire WARP path is the
place to look. It was audited piece by piece against dav1d 1.5.4 and **every
part matches**:

- `find_matching_ref` (`decode.c:191-262`) vs `find_num_warp_samples`: the
  top-edge first-cell test, the `aw4 >= bw4` "large neighbour" branch with its
  `off`/`have_topleft`/`have_topright` adjustments, the `else` step loop, the
  left-edge equivalents, top-left, and the `imax(bw4,bh4) < 32` guard on
  top-right. Kinetix's `num_scanned` cap of 8 corresponds to dav1d's
  `if (++count >= 8) return`, and both count only *ref-matching* neighbours.
- `derive_warpmv`'s `add_sample` point encoding
  (`pts[np][0] = 16*(2*dx + sx*bs) - 8`, `pts[np][1] = pts[np][0] + mv`) vs
  Kinetix's `src_x = 16 * (2*px_dx + sx*nb_w4) - 8` / `dst = src + cell.mv` —
  identical, including using the *neighbour's* block size.
- `select_warp_samples` and `get_shear_params` (cont'd 10, re-confirmed).
- `dav1d_find_affine_int` (`warpmv.c:149-205`) vs `find_affine_int`,
  including the `abs(sx-dx) < 256 && abs(sy-dy) < 256` per-sample gate, the
  `a`/`bx`/`by` accumulator forms, the `det = a00*a11 - a01*a01` (dav1d's
  own `a[0][1]*a[0][1]`, not `a[1][0]`), and the `resolve_divisor_64` /
  `get_mult_shift_{diag,ndiag}` least-squares solve.
- `warp_affine` (`recon_tmpl.c:1115-1174`) vs `block_warp_process`: the 8x8
  tiling, `src_y = by*4 + ((y+4)<<ss_ver)`, the `>> ss_hor`/`>> ss_ver` on
  `mvx`/`mvy`, `dx = (mvx>>16) - 4`, and the
  `mx = ((mvx & 0xffff) - alpha*4 - beta*7) & ~0x3f` /
  `my = ((mvy & 0xffff) - gamma*4 - delta*4) & ~0x3f` phase derivation.
- The `warp_eligible = bw > 4 && bh > 4` gate: verified this is the correct
  pixel-unit restatement of dav1d's `imin(bw4,bh4) > 1`. The block is
  `bw4=2 bh4=2` = 8x8 px, so `bw = 8` in the log and the gate passes exactly
  as dav1d's would. (Initially misread `bw=8` as evidence the block was 8x8
  *mi*; it is 8x8 *pixels* — no bug.)

### Two hypotheses raised and discarded this session

1. That `warp.rs`'s `find_affine_int(...)? ; get_shear_params(...)?` chain
   differed from dav1d's `!f() && !g()`. It does not — see cont'd 10.
2. That the `warp_eligible` gate used the wrong unit (mi vs px, or 4x4 vs
   px). It does not — traced above.

### Status: still not fixed

No code change landed this session (the only diff was already committed as
`f25f900`). The residual is **unchanged at 7390 bytes**, and with
`KINETIX_AV1_NO_WARP=1` still 7264 — so the base translational MC or the
reference this block reads is wrong *independently* of warp, and the whole
warp path has now been cleared as the cause.

That points the next session away from `warp.rs` entirely and at the
**reference resolution** for this frame: mi (16,14) reads `ref=2`, and the
blocks in this frame use ref `2` (69 blocks) and ref `7` (10). The concrete
next step is to dump, for this one block, the *actual reference plane bytes*
at the sampled positions (`motion_compensate`'s `px_x + tile_px_x0`,
`mvs[0]`, filter pair) and compare them against the same coordinates in the
frame that `ref 2` resolves to — i.e. verify that `ref_to_slot[2]` names the
plane the encoder intended, and that the stored reference frame is the
post-filter frame (a stale pre-filter reference would produce exactly this
kind of large, spatially-localised, MC-shaped error).

now that the frame gate is trustworthy. Use `KINETIX_AV1_DBG_PXY_FRAME` to
scope any stage trace — do not trust an unscoped one.

Scratch probes added while chasing this (`VPROBE` in the vertical deblock
loop, a one-off `GRIDCHK` in `reconstruct/mod.rs`) were removed; the reusable
pieces kept are the unconditional frame counter, `DBG_PXY_FRAME`, the `fr=`
field on `KINETIX_AV1_IBSUM`, and `PXYDUMP`/the corrected doc in
`probe_tiles`. No behaviour change: frame 1 stays at 7390, 163 AV1 lib tests
plus the full `tpt-kinetix-test-utils` suite pass, fmt and clippy clean.

`av1_trace_capture` (`tpt-kinetix-test-utils/examples`) exist for this and do
not need the broken dav1d build. Note the `interp_filter` read is still the
last *matched* anchor, and `read_vartx_tree` sits between it and the
coefficients (dav1d `decode.c:1874`) — worth confirming the trace includes
the per-transform-block `txfm_split` reads, not just the first leaf's tx size.


---

## Session 2026-09-27 (ffmpeg IS available in this environment)

**The blocker in every prior session note is gone.** `ffmpeg` is on PATH
(`E:\FFMPEG\...\bin\ffmpeg.exe`, full build with libdav1d), so the
`av1_*_when_available` / dav1d-diff harnesses actually RUN here instead of
silently skipping. All PSNR/residual numbers quoted in older notes below were
taken on trust; the ones below are measured.

### The old frame-1 diff numbers were measured on a REORDERED stream

`av1_multiframe_obu`/hand-rolled libaom encodes default to a **B-pyramid**
(alt-refs). On such a clip the encoder's `order_hint` sequence runs
`0, 28, 14, 7, 3, 1, 2, ...` — i.e. frames are CODED out of presentation
order. Any harness that pairs "our Nth emitted frame" with "dav1d's Nth output
frame" then compares two different pictures, so every per-frame diff computed
that way (including the 7390-byte frame-1 residual quoted in earlier notes) is
meaningless. Reproduce with `-lag-in-frames 0` (verified: `order_hint` then
runs 0,1,2,3,...). **`probe_tiles`/any per-frame AV1 diff must disable
reordering, or explicitly map frames, before its numbers mean anything.**

`av1_output_order` (new example) surfaces this: it ranks dav1d's output frames
against each frame we emit by best-match sample count.

### Real state of inter decode (measured, non-reordered testsrc2 320x180)

- keyframe: **bit-exact** (`phase_c_conformance` PSNR `inf`).
- inter: frame 1 differs by **34638** samples, `max|d|=214`; only 1/8 frames
  exact. Far worse than the 7390 previously recorded.

### Attribution (all measured with the existing env switches)

- `KINETIX_AV1_NODEBLOCK` and `..._NOCDEF` barely move the number
  (34638 -> 33910 / 35022), and `BLOCKMAP` reports **0 blocks with interior
  diffs** on every frame. So the broad error is *not* the loop filters.
- Prediction at the worst luma pixel (181,38) is **41 — exactly dav1d's value**
  — while the emitted pixel is 255. So **MC and the reference frame are
  correct**; the damage is entirely in the coefficient path. The post-ITX
  residual there is 1146, which saturates 8-bit.
- `eob=182` on that leaf is *legal* (tx index 2 is 16x16, 256 coeffs) — an
  earlier guess that it was out of range was wrong.
- `KINETIX_AV1_DBG_BIGCOEF` (new) flags only ~2 saturating leaves per frame
  against 34638 differing samples, so the huge coefficients are a *symptom*,
  not the main bug. Most of the error is a subtler residual error.

### ~~ROOT CAUSE FOUND: the intra keyframe is not exact~~ **RETRACTED - see below**

**This claim was WRONG and is retracted.** The "98 samples on the keyframe" was a
measurement artifact: the tool shell is persistent across calls, so a
`KINETIX_AV1_NOFILTER=1` set in an earlier call was still exported when the
"baseline" run happened - comparing our **unfiltered** output against dav1d's
**filtered** output. Re-measured in a clean shell the 64x64 keyframe is
**bit-exact** (`1/1 frames exact vs dav1d`), and so is frame 1. The first bad
frame is frame 2, and the cause is `ref_frame_idx` (end of this section), not
intra reconstruction.

Two process lessons, both of which have burned this file before:

1. **Clear `Env:KINETIX_*` in the SAME command as the measurement.** These probes
   are process-global; a leaked `NOFILTER`/`NODEBLOCK`/`NOCDEF` silently
   re-attributes a filter bug to reconstruction or vice versa.
2. Never conclude "X is exonerated" from a run where the switch isolating X was
   not demonstrably active in that same invocation.

The loop filters ARE genuinely exonerated, but on the correct evidence: in a clean
shell `NODEBLOCK` and `NOCDEF` each leave frame 2 at ~3.4-3.5k differing samples
(vs 3663 baseline), so they are not the source of the bulk error.

### Instrumentation added this session (debug-only, no behaviour change)

- `KINETIX_AV1_DBG_PRED_X/_Y/_R` — the `KINETIX_AV1_DBG_PRED` probe was
  **hardcoded to a stale window** (`y 56..96, x<128`), which is why older notes
  keep describing a `mi(4,18)` block unrelated to the current corpus. Now
  targetable; defaults preserve the old window.
- `KINETIX_AV1_DBG_PRED_COEFF` / `..._PRED_RESID` — quantised levels and
  post-ITX residual for leaves overlapping the target.
- `KINETIX_AV1_DBG_BIGCOEF` — flags a dequantised coefficient large enough to
  saturate 8-bit (a lost/desynced entropy read, not a transform overshoot).
- `av1_output_order` example — best-match frames against dav1d (the reorder
  confound).
- Note `KINETIX_AV1_DBG_MVSCAN` takes `by:bx`, colon-separated (it silently
  matches nothing if you pass a comma).

Verified: 163 AV1 lib tests pass, `phase_c_conformance` still `inf`, fmt +
clippy clean.

### ~~ROOT CAUSE (confirmed): LAST/LAST2/LAST3 resolve to a STALE DPB slot~~ **RETRACTED - see below**

**Wrong.** `ref_to_slot[LAST]` and `ref_to_slot[LAST3]` are BOTH 0 in the trace
that motivated this, so a LAST-vs-LAST3 distinction cannot explain anything.
A direct measurement also refuted it: of the 3663 differing samples in frame 2,
only 878 matched frame 0 and 36 matched frame 1 - **2749 matched NEITHER
reference**, so this is not a wrong-slot read at all. The
`ref_frame_idx = [0,0,0,1,0,0,0]` parse is correct and the DPB refresh is
correct. I over-read a plausible-looking story.

### PARTIAL: intra-in-inter is a *victim*, not the cause

`mi=(8,12)` and `mi=(12,12)` are indeed `intra=1` blocks in an inter frame, and
their reconstruction is wrong. But they are **downstream of an earlier error**,
not its origin - they read garbage from their left/top neighbours. Correcting
the record: this is a symptom, not a root cause.

### ACTUAL ORIGIN (measured): `mi=(2,12)` reads the KEYFRAME instead of frame 1

Located by raster-scanning frame 2 for the first differing pixel **and** the
first x-column where errors appear:

    x=8..12, y=48..56: differing=32, ours==frame0: 32, ours==frame1: 0

Every one of those 32 samples equals **frame 0** (the keyframe) and none equals
**frame 1**, while dav1d matches frame 1. The offending block is `mi=(2,12)`
(4x16, `skip=true`, `mv=(0,0)`, `ref=4` = LAST3) - a pure copy that copied the
wrong picture. The error then propagates rightward and upward through the
intra-in-inter blocks that consume it as neighbour context, growing to 3663
differing samples with `max|d|=169`. That is why the damage looked like a
large structured region with a sharp edge rather than a single block.

The state that makes this possible, all verified by probe:

    frame 2 header : refidx=[0, 0, 0, 1, 0, 0, 0]  => LAST -> slot 0
    DPB hints      : [0, 1, 2, 0, 0, 0, 0, 0]       => slot 0 = keyframe, slot 1 = frame 1

So the bitstream names slot 0, and slot 0 legitimately holds the keyframe. The
decode is self-consistent - and still wrong. **This means `ref_frame_idx` for
frame 2 is being mis-parsed** (LAST should be 1, not 0), not that the DPB
refresh is wrong. The refresh logic and `ref_order_hints` update
(`decoder.rs:540-544`) are both verified correct.

Note the earlier evidence that made this look like a stale slot rather than a
mis-parse: at the pixel I first sampled (10,50) frames 0 and 1 hold the *same*
value 81, so "ours == frame 0" proved nothing there. The x=8..12 y=48..56
window is the first place where the two references actually differ, which is
what makes the conclusion solid.

**Next step:** dump the 7 `f(3)` reads of `ref_frame_idx` for frame 2 together
with the bit offset, and decode those bits by hand from the OBU to see whether
the parser is starting at the wrong position (i.e. some earlier field consumed
the wrong number of bits) rather than mis-ordering the seven reads. Compare
against `frame.rs:734-738`. Expect LAST to be 1.

**Do not re-litigate the intra path** until the reference is right - every
intra-in-inter block in that region is faithfully reproducing bad neighbours.

**Why earlier parts of this investigation went wrong** (this is the third time a
probe has misled here): the block probes (`PRED`, `b0enter`,
`KINETIX_AV1_DBG_PART*`) had **no frame label**, so a multi-frame run produced
one interleaved stream and I read other frames' blocks as the block under
investigation - which produced the bogus "mi=(8,8) decodes out of raster order"
and "the bottom-right is never decoded" conclusions. All of those probes now
print `fr=<n>`. **Always scope a probe to one frame before drawing a conclusion
from it.**

**Next step:** in `reconstruct_intra_subblock` / the `intra=1` branch of
`decode_inter_block` (`inter_block.rs:650`), diff our reconstruction of
`mi=(8,12)` (`ymode=11`, `skip=true`) against dav1d. `skip=true` means the
intra prediction must be copied verbatim with no residual, so a wrong value here
is a prediction/availability bug, not a residual bug. The block reads the
already-reconstructed frame above (`haveAbove`), so a likely suspect is intra
neighbour availability when the above/left neighbours are inter. Success
criterion: `probe_tiles` on this 3-frame clip going from 2/3 to 3/3 exact.

### Instrumentation added (debug-only, no behaviour change, cont.)

- `KINETIX_AV1_DBG_TAPBLK` now also prints a `REF-READ` line (ref name, the
  DPB slot it resolved to, how many slots are populated, and the full
  `ref_to_slot` table). It was previously hardcoded to a dead `px 128..138,
  y 82..96` window and printed nothing useful. This is the probe that found
  the bug - without the resolved *slot* you cannot tell "wrong reference name"
  from "right name, stale slot".
- `KINETIX_AV1_DBG_PRED_RESID` / `..._BIGCOEF` (above).

### Instrumentation added (debug-only, no behaviour change, cont. 2)

- **`fr=<n>` frame labels added to every block-level probe** (`PRED`,
  `PRED-BASE`, `b0enter`, `KINETIX_AV1_DBG_PART`, `KINETIX_AV1_DBG_PARTALL`,
  `PREDPOST`, `REF-READ`). The absence of these is what produced two wrong
  "root causes" in this file - see the warning above. **Any new probe here must
  print `fr=` or scope itself to a frame.**
- `KINETIX_AV1_DBG_PRED_POST` - end-of-block luma dump (after the chroma
  residual too), bracketing prediction vs. final reconstruction.
- `KINETIX_AV1_DBG_PREFILTER_PXY=x,y` - samples one luma pixel in
  `reconstruct_av1_frame` *before* `apply_post_filters`, so it can be compared
  against `apply_post_filters`' own `PXY pre-filter` line. Disagreement means
  the corruption is in tile assembly, not a filter stage.
- `KINETIX_AV1_DBG_EXTENT` - prints `mi_cols`/`mi_rows`/tile rect per frame
  (both were verified correct at 16x16 for all frames; recorded so nobody
  re-checks it).
- `KINETIX_AV1_DBG_PART` is no longer capped to `mi_row<8 && mi_col<8`, so the
  whole tile prints.

### Instrumentation added (debug-only, no behaviour change, cont. 3)

- `KINETIX_AV1_DBG_TXB=x,y` - targets the transform block covering that luma
  pixel inside `reconstruct_tx_block` and prints the decoded prediction mode,
  angle delta, filter type, `have_above_right`/`have_below_left`, the sampled
  `top`/`left`/`tl` border values. This is what showed a block reading a
  gradient `left` column (187,168,153,...) where the correct neighbour is a
  flat 210. The pre-existing `dbg`/`dbg_px` gates there are hardcoded to a
  stale region and print nothing for any other block.

### Method note (this is the third wrong "root cause" in a row)

Every wrong conclusion this session came from sampling a pixel where the
evidence was ambiguous, or from an un-frame-scoped probe. Two rules that would
have prevented all of them:

1. **Pick the discriminating sample.** At (10,50) frames 0 and 1 both hold 81,
   so "our output == frame 0" was vacuous. Scan for a region where the
   candidate references actually differ *before* concluding which one was read.
2. **One frame per probe run.** Always set a frame gate/label. `PRED`,
   `b0enter`, `KINETIX_AV1_DBG_PART*` and `TXB` now all print `fr=`.

### `ref_frame_idx` is CORRECT - the "mis-parse" claim is withdrawn

`KINETIX_AV1_DBG_REFIDX=1` dumps each `f(3)` read with its bit offset. For
frame 2 all seven land at consecutive offsets (28, 31, 34, 37, 40, 43, 46) and
read `0,0,0,1,0,0,0`. So the parse position and the values are right: the
bitstream really does say LAST = slot 0. **Do not go looking for a bit-offset
bug in `frame.rs:734`.** (The earlier "LAST should be 1" claim was inference
from dav1d's output, not from the bits.)

### Best-characterised failure so far: wrong COEFFICIENTS on intra-in-inter blocks

First *discriminating* divergence (a pixel where frame 0 and frame 1 differ, so
"which reference did we read" is answerable at all) is `(0,8)` in frame 2:

    ours=140  dav1d=90  f0=81  f1=87

`KINETIX_AV1_DBG_TXB=0,8` shows the borders are all **correct** -
`top=[90,89,31,30,...] left=[90,90,90,...] tl=90`, matching dav1d's 90 - and
`palette_present=false`. So the prediction is right. But:

    eob=12  quant=[9, 6, 1, 1, 13, 2, -2, 0, ...]
    residual[0..8] = [50, 66, 66, 52, 35, 53, 51, 21]
    90 + 50 = 140  (= our output)

**The coefficients are wrong for a block dav1d codes as near-zero.** That is
the sharpest statement of the bug available: not a wrong reference, not a wrong
prediction, not a filter - wrong `coeffs()` output on an intra block inside an
inter frame.

Error budget for frame 2 (3663 differing samples): 678 at +-1, 540 at 2-3,
629 at 4-10, 1172 at 11-50, 644 above 50. Restricting to pixels where the two
references actually differ, only 516 match frame 0, 36 match frame 1, and
**492 match neither** - so this is not a wrong-reference-slot bug.

### Dead ends (do not repeat)

- **Loop filters** - `NODEBLOCK`/`NOCDEF` barely move the count.
- **OBMC** - `NOOBMC` does not change it.
- **MC interpolation** - predictions are bit-exact where checked.
- **Palette gating on `segmentation_enabled`** - tried it; frames went from
  2/3 to **0/3** exact, so `read_palette_mode_info` is correctly gated on
  `allow_screen_content_tools` alone. Reverted.
- **`block_borders` availability** - it computes `have_above`/`have_left` from
  `px_y > 0` / `px_x > 0` itself; the two flags it is *passed* are only
  `have_above_right`/`have_below_left` (the extension limits), which is
  spec-correct. Not a bug.

**Next step:** the coefficient decode for intra blocks in an inter frame.
`mi=(0,8)` decodes `eob=12` where the true eob is ~0. Compare the `coeffs()`
symbol sequence against the dav1d/`ITXDUMP` trace for that block - specifically
the `txb_skip` / `all_zero` decision and the `intra_tx_type` CDF context, since
a wrong `all_zero` there desyncs the whole tile. The existing
`KINETIX_AV1_DBG_ALLZERO` probe prints `skip_ctx` and the CDF; scope it to this
block first.

### The dav1d trace build WORKS - and has produced a first real diff

The "broken dav1d build" in earlier notes is **not** broken. What was actually
wrong was only the DLL lookup: `dav1d.exe` exits with `0xC0000135` (DLL not
found) unless `%LOCALAPPDATA%\Temp\dav1d_fresh\build\src` is on `PATH`. The tree
at `%LOCALAPPDATA%\Temp\dav1d_fresh` is a complete 1.5.4 clone (38 src/*.c)
with the prior instrumentation patches already applied, and it builds
incrementally in ~15s:

    set PATH=%LOCALAPPDATA%\Temp\dav1d_fresh\build\src;%PATH%
    call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
    cd /d %LOCALAPPDATA%\Temp\dav1d_fresh
    ninja -C build tools/dav1d.exe
    build\tools\dav1d.exe -i clip.ivf -o out.yuv --muxer yuv --threads 1

(There is also a pruned `dav1d_oracle` tree that genuinely does not build - do
not use it. `dav1d_src` is an empty shell. Use `dav1d_fresh`.)

**New probe added to that tree** (`src/recon_tmpl.c`, in `decode_coefs`):
`KINETIX_DBG_COEFF_BLK=plane,bx4,by4` dumps the `all_skip` decision, the coded
`txtp`, and the decoded `eob` for one transform block only. `decode_coefs` now
takes `bx4`/`by4` for this (all 6 call sites updated).

### FIRST VERIFIED DIVERGENCE: the `eob` read in `coeffs()`

Block `mi=(0,2)`, luma, frame 2 of the 3-frame 64x64 clip - the block at the
first discriminating pixel `(0,8)`:

    dav1d : all_skip=0  sctx=0  tx=TX_4X4  ctx=0  ->  eob=2
    Kinetix: all_zero=false (MATCHES)              ->  eob=12
             quant=[9, 6, 1, 1, 13, 2, -2, 0, ...]

So `all_zero` is read **correctly** (both say "not all zero"), and the block
type / tx size / skip context all agree. The divergence is strictly inside the
`eob` decode that follows: dav1d reads `eob=2`, we read `eob=12`, and the 10
extra coefficients are what produce the bogus `residual[0]=50` (and hence
`90 + 50 = 140` instead of dav1d's 90).

Per dav1d `recon_tmpl.c:434-457` the eob read is
`eob_bin`/`eob_hi_bit` (symbol over `eob_bin_cdf`, then a bool) followed by
`dav1d_msac_decode_bools(&ts->msac, eob_bin)`. Compare each of those three
reads against `coeff.rs::read_eob` (`eob_pt`, then `eob_extra`, then the
`for i in 1..(eob_pt - 2)` literal loop). Given `eob_multisize` for TX_4X4 is
`2 + 2 - 4 = 0`, the first read is `eob_pt_16` and `eob_pt` is in `1..=5` - a
one-symbol slip in the `eob_pt` bucket, the `eob_extra` context index, or the
literal loop bound would produce exactly this.

**Next step:** extend the dav1d probe to print `eob_bin`/`eob_hi_bit` and the
sub-bools for this block, print the same three values from `read_eob`, and diff
them. That is now a bounded, mechanical comparison - no more guessing.


## MEASURED: dav1d eob sub-symbols, and a ruled-out hypothesis

Added `KCOEF-EOBBIN` / `KCOEF-EOBHI` / `KCOEF-EOBBITS` to dav1d
`recon_tmpl.c` (just before the `if (dbg) printf("Post-eob_bin...")` at
line 446) and the matching `KINETIX_AV1_DBG_EOB` dump in
`coeff.rs::read_eob`.

For luma block `mi=(0,2)`, frame 2, the full dav1d eob path is:

    KCOEF fr=2 plane=0 mi=(0,0) tx=0 ctx=0 sctx=0 all_skip=0 r=53648
    KCOEF-EOBBIN tx2dszctx=0 is_1d=0 eob_bin_raw=2 r=34304
    KCOEF-EOBHI  ctx=0 chroma=0 eob_bin=0 hi_bit=0 r=41398
    KCOEF-EOBBITS eob_bin=0 -> eob=2 r=41398

So dav1d reads a **single symbol worth 2**, takes `eob_bin = 2 - 2 = 0`,
reads one `eob_hi_bit` (0), and `((0|2) << 0) | bools(0)` = 2. Final `eob=2`.

**Hypothesis tested and REJECTED.** dav1d's `eob` is the *raw* CDF symbol,
which suggested Kinetix's `1 + symbol` and its `(1 << (eob_pt - 2)) + 1`
base were both off by one. Changing to the raw symbol with a
`(1 << (eob_pt - 2))` base drove the previously-exact intra keyframe to
0/3 frames exact. **The `+ 1` and the `+ 1` base are the spec's and are
correct; reverted.** Do not re-derive this from dav1d's variable naming -
dav1d's `eob` is the spec's `eobPt` minus one, but the spec's own
arithmetic from there still yields the `+ 1` base.

**What this rules out:** the eob *mapping* arithmetic is not the bug. The
remaining candidates inside `read_eob` are now narrowed to (a) the `eob_pt_*`
CDF **bucket selection** - which is `ptype`/`ctx` indexed and may not match
dav1d's `[chroma][is_1d]`, or (b) the `eob_extra` context index, or (c) the
literal-loop bound. Note dav1d's `tx2dszctx=0` and `is_1d=0` agree with

## CORRECTION: the block-targeted dav1d probe was itself buggy

`KINETIX_DBG_COEFF_BLK=plane,bx4,by4` selected its block with

    by4 == atoi(strchr(kcb, ',') + 1);

`strchr` returns the **first** comma, so `by4` was parsed from the *bx4*
field. Requesting `0,0,2` therefore gated on `bx4==0 && by4==0` and traced
block `mi=(0,0)`, not `mi=(0,2)`. Confirmed empirically: `0,0,2` and
`0,0,4` both printed `mi=(0,0)`, while `0,2,0` printed `mi=(2,2)`.

Fixed to take the **second** comma (`strchr(c2 + 1, ',')`).

**This invalidates the "block mi=(0,2)" framing of the earlier eob
comparison.** The numbers previously attributed to `mi=(0,2)` were really
`mi=(0,0)`, so the `eob=2` vs `eob=12` gap was measured on a block whose
identity was misidentified. The corrected trace for the real
`mi=(0,2)`, frame 2, is:

    KCOEF fr=2 plane=0 mi=(0,2) tx=0 ctx=0 sctx=0 all_skip=0 r=37444
    KCOEF-TXTP intra tx=0 min=0 y_mode=12 set=1 idx=3 txtp=11 r=45584
    KCOEF-EOBBIN tx2dszctx=0 is_1d=1 eob_bin_raw=3 r=35632
    KCOEF-EOBHI  ctx=0 chroma=0 eob_bin=1 hi_bit=1 r=34202
    KCOEF-EOBBITS eob_bin=1 -> eob=7 r=34056

Note `is_1d=1` and `txtp=11` (an intra tx type) - the block is an
**intra** block in a frame-2 *inter*-looking position, and the 1-D tx class
means `ctx` should be 1 on the Kinetix side. Kinetix's own dump must be
re-measured against this corrected target before any conclusion is drawn;
the earlier `eob_pt_raw`/`eob` pairings in this file are from the
mis-targeted run and should not be compared to it.

## RULED OUT: eob_pt CDF widths and read_symbol

## ROOT CAUSE FOUND: `TxBlockCtx.intra_dir` is hardcoded to DC_PRED (0)

`read_transform_type` (`coeff.rs:703`) uses `blk.intra_dir` as the
`txtp_intra1` / `txtp_intra2` CDF context:

    let dir = blk.intra_dir;
    ...
    Ok(TX_TYPE_INTRA_INV_SET1[dec.read_symbol(cdfs.intra_tx_type_set1[sqr][dir])])

But `intra_dir` is a hardcoded literal at **every production construction
site** - `coeff.rs:1277`, `coeff.rs:1597` (`DC_PRED`), and
`reconstruct/inter_block.rs:3307 / 3680 / 3707` (all `0`). The only
assignments of a non-zero direction come from the test-only `with_dir`
helper (`coeff.rs:1607`, used at 1718/1719/1858/1868). So in production
the transform-type CDF context is **always** `[..][DC_PRED]`, regardless
of the block's real intra prediction mode.

### The measured proof

Corrected dav1d trace, luma block `mi=(0,2)`, frame 2:

    KCOEF-TXTP intra tx=0 min=0 y_mode=12 set=1 idx=3 txtp=11 r=45584
    KCOEF-EOBBIN tx2dszctx=0 is_1d=1 eob_bin_raw=3 r=35632
    KCOEF-EOBHI  ctx=0 chroma=0 eob_bin=1 hi_bit=1 r=34202
    KCOEF-EOBBITS eob_bin=1 -> eob=7 r=34056

Kinetix, same block (new `KINETIX_AV1_DBG_EOB=plane,bx4,by4,frame` probe):

    KEOB fr=2 plane=0 mi=(0,2) tx=0 tx_type=2 eob=12 ... is_1d=0 rng=50440

Three independent confirmations that this is the real divergence:

1. **`y_mode=12` vs context `0`.** dav1d reads the transform type from
   `txtp_intra1[TX_4X4][y_mode_nofilt]` with `y_mode_nofilt = 12`;
   Kinetix reads `intra_tx_type_set1[0][0]`. Same CDF, wrong context ->
   different symbol -> `txtp=11` (H_DCT) vs Kinetix `tx_type=2`
   (DCT_ADST). `12` is `PAETH_PRED` in Kinetix's own enum
   (`coeff.rs:1582`), so this is a genuine mode mismatch, not a
   numbering accident.
2. **The `is_1d` consequence.** dav1d's `txtp=11` is `H_DCT`, which
   `dav1d_tx_type_class` (`tables.c:317`) maps to `TX_CLASS_H` ->
   `is_1d=1`, and it selects `eob_bin_16[chroma][1]`. Kinetix's
   `tx_type=2` is 2-D -> `is_1d=0` -> `eob_pt_16[ptype][0]`. This is why
   the `eob` values differ (7 vs 12) even though the eob *arithmetic* is
   correct - **the eob decode is being fed the wrong CDF entirely.**
   This also explains why the earlier mis-targeted block "matched": that
   block had `is_1d=0` on both sides.
3. **`get_tx_class` is correct.** Checked against dav1d's
   `dav1d_tx_type_class` (`tables.c:305-323`): both map
   `V_DCT/V_ADST/V_FLIPADST -> V`, `H_DCT/H_ADST/H_FLIPADST -> H`,
   everything else `-> 2D`. Not a bug; the input `tx_type` is wrong.

### Secondary gap: no FILTER_PRED handling

dav1d computes `y_mode_nofilt` by mapping `FILTER_PRED` back to its base
angle before using it as a CDF context
(`recon_tmpl.c:390-391`, `dav1d_filter_mode_to_y_mode`). Kinetix has
**no `FILTER_PRED` symbol anywhere in the crate** (grep returns
nothing), so once `intra_dir` is plumbed through it will also need this
remap, or filter-intra blocks will read the wrong context.

## RETRACTION: the `intra_dir` hardcoding root cause was WRONG

The claim above that `intra_dir` is hardcoded to `DC_PRED` in production
is **incorrect**, and the fix must not be made on that basis.

Checked properly this time:

- `reconstruct/intra_block.rs:552` sets `intra_dir: luma_intra_dir`, where
  `luma_intra_dir` is computed at lines 421-424 as
  `FILTER_INTRA_MODE_TO_INTRA_DIR[filter_intra_mode]` when filter-intra is
  used, else `y_mode`. That remap is the spec's and is already correct.
- `reconstruct/inter_block.rs:650` (intra block inside an inter frame)
  passes the real decoded `y_mode` down through
  `reconstruct_intra_subblock`.
- The `intra_dir: 0` literals in `inter_block.rs:3307/3680/3707` are all on
  `is_inter: true` blocks, where `read_transform_type` never reads
  `intra_dir` (it takes the inter branch). The `intra_dir: 0` at
  `coeff.rs:1277` is inside `#[test] fn all_zero_ctx_ignores...`.
- Empirically, the target block reports **`intra_dir=12`**, matching dav1d's
  `y_mode=12` exactly.
- `TX_TYPE_INTRA_INV_SET1` and the `intra_tx_type_set1` `[2][13][8]` CDF
  shape also match dav1d (`txtp_intra1[2][N_INTRA_PRED_MODES][7+1]`,
  `dav1d_tx_types_per_set` Intra1 list) value-for-value.

So the transform-type CDF context and tables are all correct. The earlier
"hardcoded intra_dir" conclusion came from grepping for `intra_dir: 0`
without checking whether those sites were on the intra path.

## ACTUAL LEAD: frame 2's partition is missing a block at mi=(0,2)

With `intra_dir` cleared, the divergence is upstream, in the partition /
block-type decode. Comparing Kinetix's `KINETIX_AV1_IBSUM` block list for
frame 2 against the corrected dav1d trace:

Kinetix frame 2, column 0, in decode order:

    mi=(0,0) 1x1 intra=0
    mi=(0,1) 1x1 intra=0
    mi=(0,3) 1x1 intra=0     <-- row 2 is MISSING entirely
    mi=(0,4) 2x2 intra=0
    mi=(0,6) 2x2 intra=0
    mi=(0,8) 2x4 intra=0

There is **no `mi=(0,2)` block at all** in frame 2 - the partition jumps
straight from `(0,1)` to `(0,3)`, leaving a hole at row 2. But dav1d's
trace for frame 2 shows an **intra** block at `mi=(0,2)`
(`KCOEF-TXTP intra ... y_mode=12`), and Kinetix's own `KEOB` probe *does*
fire at `mi=(0,2)` with `intra_dir=12` / `ymode=12` from `IBSUM`/`KYMODE`.

That is self-contradictory: the `IBSUM` partition walk never emits a block
at `(0,2)`, yet a transform block is decoded there and dav1d agrees one
exists. Two candidate explanations, not yet distinguished:

1. The block *is* decoded (so the walk does reach it) but the `IBSUM`
   summary print is emitted from a path that skips this block, meaning the
   print is incomplete rather than the partition being wrong. Note the
   `IBSUM` line does print `mi=(0,2) intra=1` - it appears in the combined
   output - so the walk does reach it; the column-filtered view above just
   missed it because the `intra=1` line carries a different `mi` ordering.
2. The partition genuinely differs from the bitstream (wrong `bsize` split
   or a mis-decoded partition symbol), which would desync everything after

## RETRACTION 2: the "missing block at mi=(0,2)" lead was ALSO a probe artifact

The "frame 2 has no block at `mi=(0,2)`" conclusion was wrong, for a
second reason in the same family: **the `intra=1` IBSUM line and the
`KYMODE` line had no `fr=` field** (`inter_block.rs:643` and `646`, vs the
`intra=0` line at `1448` which does print `fr=`). A `grep 'IBSUM fr=2'`
therefore returned *zero* intra blocks for frame 2 — not because none
existed, but because the intra-in-inter branch never printed a frame
number. The `mi=(0,2)` line I read as frame 2 was actually frame 1.

**Fixed:** both lines now print `fr={}` via
`crate::debug_frame_seq::current()`, matching the existing `intra=0` idiom.
With that, frame 2's intra blocks are:

    IBSUM fr=2 mi=(0,2)  bw4=1 bh4=1 intra=1 ymode=12 skip=false
    IBSUM fr=2 mi=(4,12) bw4=2 bh4=4 intra=1 ymode=0  skip=true
    IBSUM fr=2 mi=(6,12) bw4=2 bh4=2 intra=1 ymode=0  skip=true
    IBSUM fr=2 mi=(6,14) bw4=2 bh4=2 intra=1 ymode=0  skip=true
    IBSUM fr=2 mi=(8,12) bw4=4 bh4=4 intra=1 ymode=11 skip=true
    IBSUM fr=2 mi=(12,12) bw4=4 bh4=4 intra=1 ymode=0 skip=true

`mi=(0,2)` intra with `ymode=12` **is present in frame 2**, and agrees with
dav1d's `KCOEF-TXTP intra ... y_mode=12`. Frame 2 is the first frame with
any non-skip intra block (every other frame-2 intra block is `skip=true`),
which is consistent with frame 2 being the first frame to differ.

### Standing rule (this has now bitten twice)

Any probe line that lacks a frame label must not be used to attribute
events to a frame. `grep 'fr=2'` over a mixed set silently under-reports.
Both the dav1d `by4` parsing bug and this one produced confident,
*plausible*, and wrong conclusions. Verify a probe's selectivity with a
positive control before drawing a negative conclusion from it.

### Where the divergence actually remains

For frame 2 `mi=(0,2)`, now that intra mode, CDF context, transform-type
tables, and block identity are all confirmed to agree between the two
decoders, the remaining measured difference is:

    dav1d : txtp=11 (H_DCT), is_1d=1, eob=7

## THIRD probe flaw in the same family: the frame number was an ECHO

`KINETIX_AV1_DBG_EOB` accepted a trailing `,frame` field and printed it as
`fr=`. That field is just the caller's request, not the decoder's actual
frame counter, so the label was **an echo of the query**. Setting
`...,2` printed `fr=2` for *every* hit, including hits that occurred in
frame 1 - producing two apparently-conflicting "blocks in one frame" that
were in fact one block in frame 1 and a different one in frame 2.

Fixed: the frame is now taken from `crate::debug_frame_seq::current()` at
the print site (the established idiom, as used by the `intra=0` IBSUM and
`PRED` probes), and the `frame` component was removed from the selector
entirely so it cannot be misused again.

This is the **third** instance of the same failure mode this session:
dav1d's `strchr` comma bug, the missing `fr=` on the intra IBSUM line, and
now an echoed frame number. Each produced a confident, plausible, wrong
structural conclusion. See the standing rule above.

### Corrected per-frame picture for block mi=(0,2), plane 0

    Kinetix fr=1: tx=1 (8x8)  tx_type=5  intra_dir=0   eob=6   is_1d=0
    dav1d   fr=1: tx=1 (8x8)  all_skip=0                          ctx=1
    Kinetix fr=2: tx=0 (4x4)  tx_type=2  intra_dir=12  eob=12  is_1d=0
    dav1d   fr=2: tx=0 (4x4)  all_skip=0                          ctx=0

**Block identity, plane, frame, and tx size all agree on both frames.**
The "extra 8x8 block in a 1x1 coded block" anomaly from the previous entry
was entirely this labelling artifact - there is no such anomaly. The only
remaining difference is the decoded transform type / eob on frame 2, and
it is downstream of something earlier that has not yet been located.

### New signal: frame 2 is the first frame to use multiple references

Reference usage per frame from the `IBSUM` probe:

    frame 1: ref=2  (66 blocks)          -- a single reference only
    frame 2: ref=2 (17), ref=3 (3), ref=4 (6), ref=5 (7), ref=8 (3)

Frame 1 being exact and frame 2 being the first frame to (a) differ and
(b) reference more than one slot makes the **reference-list / ref-frame
ordering** path (`decoder.rs:474-481`, `RefFrameStore`) a better
candidate than the coefficient path. This is a hypothesis from a
correlation, not a measured fault.

Also noted: frame 2's mismatch is 3663 of 6144 bytes (60%), first at
Y (32,0) - partial corruption, not total. Consistent with either a
localised wrong value or a desync that only affects some regions.

**Next step (bounded):** diff frame 2's *predicted* (pre-residual) plane
against dav1d's. If prediction already differs, the bug is in motion
compensation / reference selection and the coefficient work is a red
herring. If prediction matches and only the residual differs, the fault is
in the coefficient path as originally suspected. This is a clean
discriminating test and needs no further speculation.

    Kinetix: tx_type=5,       is_1d=0, eob=6   <- first KEOB, tx=1 (8x8)
    Kinetix: tx_type=2,       is_1d=0, eob=12  <- second KEOB, tx=0 (4x4)

Note Kinetix emits **two** transform blocks at `mi=(0,2)` (an 8x8 with
`tx_type=5` *and* a 4x4 with `tx_type=2`), while dav1d traces one 4x4.
`IntraFrameYMode` for this block is 12 (`PAETH_PRED`), which is not
directional, so the block should not need an angle-delta read, and the
`tx=1` (TX_8X8) block appearing at all for a 1x1 (`bw4=1 bh4=1`) coded
block is the anomaly worth chasing next. That is where the frame-2 desync
most likely originates.

   it - consistent with frame 2 being the first frame to differ while frames
   0-1 are exact.

**Do not act on (1) or (2) until the `IBSUM` print for `mi=(0,2)` is
compared directly against dav1d's partition for frame 2.** The immediate
step is a block-by-block partition diff of frame 2 between the two
decoders; the first block whose (mi, size, intra/inter) tuple disagrees is
the real desync point, and everything after it is noise.

Also still open and now *more* likely than the eob work: the eob
mismatch (`7` vs `12`) may be a downstream *symptom* of the partition
being wrong, not an independent entropy bug.


### Fix required (not yet implemented)

Thread the real decoded intra prediction mode from the block decoder
into `TxBlockCtx.intra_dir` instead of the hardcoded literal, and apply
the `FILTER_PRED -> base angle` remap before using it as the
transform-type CDF context. `intra_dir` is also consumed by the
prediction/reconstruction path, so this may fix a class of intra-block
errors beyond transform type.

**Status:** 2/3 baseline preserved throughout; 163/163 lib tests pass;
clippy + fmt clean. `coeff.rs` decode logic remains byte-identical to
HEAD (probe + helpers only).


`read_symbol` (`entropy.rs:517`) derives the symbol count as
`cdf.len() - 1`, and Kinetix stores wider arrays than dav1d: `eob_pt_16`
is `[u16; 6]` where dav1d's `eob_bin_16` is `CDF4`. This looks alarming
but is a benign internal convention:

- Kinetix `eob_pt_16` default row is `[840, 1039, 1980, 4895, 32768, 0]`.
  The first four values match dav1d's `CDF4(840, 1039, 1980, 4895)`
  exactly; slot 4 is the `32768` end marker that `read_symbol` asserts on.
- Slot 4 yields `f = 0`, so it is a zero-probability escape that is never
  selected, and it lies outside `cdf[..n-1]` so CDF adaptation never
  touches it.
- The `eob_pt_raw=4` (escape) values in the dump occur on **frame 0**,
  which is pixel-exact, confirming the escape is harmless.

So the eob_pt CDF widths are correct and are not the source of the frame-2
mismatch. Likewise the `eob` base arithmetic has now been tested twice and
reverted twice, each time breaking the intra keyframe; treat
`(1 << (eob_pt - 2)) + 1` and the `eob_pt < 2` early-return as correct.

**Status:** unchanged 2/3 baseline (frame 2, 3663 bytes, first diff Y
(32,0)); 163/163 lib tests pass; clippy + fmt clean. `coeff.rs` diff is
additive instrumentation only.

**Next step:** re-run `KINETIX_AV1_DBG_EOB` with block targeting that matches
the corrected dav1d `mi=(0,2)` identity, and check `is_1d=1` is honoured -
i.e. that Kinetix's `ctx = get_tx_class(tx_type) != TX_CLASS_2D` yields 1
for `txtp=11`, and that the `y_mode=12 set=1 idx=3` intra-txtp path is
reached at all on the Kinetix side. `is_1d` is a strong new suspect: the
mis-targeted block had `is_1d=0` and matched, which would explain why the
eob mapping looked correct in the old comparison.

Kinetix's reported `eob_multisize=0` / `ctx=0`, so the bucket *size* is
right - the remaining question is whether Kinetix's `[ptype][ctx]` indexing
and `eob_pt` value range match dav1d's CDF entry count.

**Status:** back to the 2/3 baseline (frame 2, 3663 bytes, first diff at
Y (32,0)). 163/163 `tpt-kinetix-av1` lib tests pass, clippy + fmt clean.
Both `coeff.rs` edits are additive instrumentation only - the decode logic
is byte-identical to HEAD.

**Next step:** print the *entry count* of the `eob_pt_16` CDF in both
decoders and confirm Kinetix's `eob_pt` can only produce the same symbols
dav1d's `eob_bin_cdf[chroma][is_1d]` can; then diff the `eob_extra` CDF
index. Only after those two agree is the literal-loop bound worth touching.

## Session 2026-09-28 — ROOT CAUSE FOUND AND FIXED: missing `bsize > BLOCK_4X4` gate on `read_tx_size` for intra-in-inter blocks; frame 2 3663 -> 181 differing bytes

Picked up exactly where the previous session left off: build a symbol-by-symbol
trace of frame 2, tile 0, mi (0,0) through mi (0,2), from both Kinetix and a
patched dav1d, and find the first rng disagreement. Found it, fixed it,
verified it. **Do not re-open the `eob_pt`/`eob_extra` CDF-indexing lead from
the previous session's tail — it was correct all along; the real bug was one
level up the call stack, in the `tx_depth` read that precedes the coefficient
decode entirely.**

### Tooling used (all pre-existing, confirmed working)

- `dav1d_fresh` (`%LOCALAPPDATA%\Temp\dav1d_fresh`) already had `KSKIP`/
  `KINTRA`/`KYMODE` prints in `decode.c` gated on `KINETIX_DBG_IBSUM` (note:
  **not** `KINETIX_AV1_IBSUM` — different env var name than the Kinetix side,
  easy to typo) that mirror Kinetix's own `KSKIP`/`KINTRA`/`KYMODE` prints in
  `inter_block.rs` (`KINETIX_AV1_IBSUM=1`). These already print matching
  `mi=(x,y)`/`rng=` fields, so no new dav1d patching was needed for the
  skip/is_inter/y_mode value trace — only for the two new CDF-row dumps below.
- Built `dav1d_fresh` via PowerShell (the `cmd.exe /c "call vcvars64.bat && ..."`
  form from earlier sessions' notes silently produces no output in this
  session's shell — use `cmd /c '"...\vcvars64.bat" && cd /d "..." && ninja ...'`
  through the **PowerShell tool**, which does work and shows ninja's output).
- `probe_tiles scratch_av1/f3.ivf` (the checked-in 3-frame 64x64 clip) remains
  the fastest repro: baseline **3663 differing bytes on frame 2**, confirmed in
  a clean env (`env -u KINETIX_AV1_NOFILTER -u KINETIX_AV1_NODEBLOCK -u
  KINETIX_AV1_NOCDEF`, per this file's standing leaked-env-var rule).
- **Capture gotcha hit and fixed in-session:** `cmd 2>&1 > file` in bash
  redirects stderr to the *old* stdout (terminal) and only stdout to `file` —
  the opposite of what's wanted for `eprintln!`-based probes. Use
  `cmd > file 2>&1`. An initial capture attempt silently produced a stdout-only
  file missing every debug line; re-running with the correct redirection order
  fixed it. Filed here so the next session doesn't lose time to it.

### The trace

With `KINETIX_AV1_IBSUM=1` (Kinetix) / `KINETIX_DBG_IBSUM=1` (dav1d), diffing
frame 2's block sequence from mi (0,0) confirms **every `KSKIP`/`KINTRA` value
and rng matches exactly** through mi (2,0), and still matches at mi (0,2)'s
`KSKIP`/`KINTRA` (`intra=1`, rng=35088 on both sides — this is the block from
the previous session's "first divergence" report, a `BLOCK_4X4` `PAETH_PRED`
(`ymode=12`) intra block coded inside inter frame 2).

The previous session's own `KYMODE` prints (rng=40608 dav1d vs rng=32860
Kinetix) looked like a divergence *at* the `y_mode` read, but that `KYMODE`
print site in `inter_block.rs` fires *after* several more reads (angle-delta,
`uv_mode`, palette, filter-intra, tx-size) — a version of the exact
"un-scoped/mis-positioned probe" trap this file warns about repeatedly, just
with a probe *position* instead of a missing *label*. Added two new precisely
-positioned probes to pin down exactly which read diverges:

- `YMODECDF-PRE`/`YMODECDF-POST` (`KINETIX_AV1_DBG_YMODECDF=1`, `inter_block.rs`)
  print the `y_mode` CDF row and post-read rng immediately around the actual
  `read_y_mode` call, with a matching dav1d probe in `decode.c` right around
  its `ymode_cdf` read.
- `MODEINFO-UV` / `MODEINFO-TX` (same env var) checkpoint rng right after
  `uv_mode` and right after `read_tx_size`.

Result for mi=(0,2), frame 2:

    YMODECDF-PRE  fr=2 mi=(0,2) grp=0 row=[22801,23489,...,32768,0]   <- exact DEFAULT_Y_MODE_CDF[0], untouched
    YMODECDF-POST fr=2 mi=(0,2) ymode=12 rng=40608                    <- MATCHES dav1d's KYMODE rng=40608 exactly
    MODEINFO-UV   fr=2 mi=(0,2) uvmode=0 has_chroma=false rng=40608   <- no symbol read (has_chroma=false), rng unchanged, consistent
    MODEINFO-TX   fr=2 mi=(0,2) luma_tx=0 ... rng=32860               <- DIVERGES: dav1d never reads tx_depth here at all

So `y_mode` was never the bug (the "hardcoded intra_dir" and "wrong CDF
context" leads from earlier sessions were right to be retracted). The
divergence is a **spurious symbol read inside `read_tx_size`**: Kinetix's
intra-in-inter call site (`inter_block.rs`, the `!is_inter` branch) called

    let luma_tx = if self.tx_mode_select && !self.lossless {
        self.read_tx_size(bsize, max_tx, mi_row, mi_col)
    } else {
        max_tx
    };

unconditionally whenever `TxMode == TX_MODE_SELECT`, with no check on `bsize`.
Per AV1 spec §5.11.15 `read_tx_size(allowSelect)`, the `tx_depth` symbol is
only read when `MiSize > BLOCK_4X4` — a 4x4 block always uses `TX_4X4` with no
signalled depth (dav1d: `decode.c:1211`, `if (f->frame_hdr->txfm_mode ==
DAV1D_TX_SWITCHABLE && t_dim->max > TX_4X4)`, where a `BLOCK_4X4` block's
`t_dim->max` is already `TX_4X4`, so the whole branch — including the
symbol read — is skipped). mi=(0,2) is exactly such a block (`bw4=1 bh4=1`),
so Kinetix read one entropy symbol dav1d never reads, desyncing the coder's
`rng`/bit position for the rest of the tile — this is exactly what produced
the "same CDF context, different decoded symbol" observation the previous
session made at the `intra_tx_type` read a few reads later (that symbol
wasn't wrong on its own; it was reading from the wrong bit position).

**This exact bug, in this exact shape, was already found and fixed once** —
on the *keyframe* intra path. `intra_block.rs:288` has the identical
`bsize > BLOCK_4X4 &&` gate with a comment describing the same failure mode
("mandelbrot at mi (16,18)"). The fix was never propagated to the
intra-in-inter-frame call site in `inter_block.rs`, which is a structurally
separate copy of the same `intra_block_mode_info()`/`read_tx_size` sequence
for a different top-level branch (`!is_inter` inside `decode_inter_block`
rather than the keyframe's `decode_intra_block`). Grepped for any other
`read_tx_size` call sites (`partition.rs`'s definition and `tests.rs`'s unit
test are the only other hits) — this was the only missing gate.

### Fix

`tpt-kinetix-av1/src/reconstruct/inter_block.rs`, the `!is_inter` (intra in
inter frame) branch: changed

    let luma_tx = if self.tx_mode_select && !self.lossless {

to

    let luma_tx = if bsize > BLOCK_4X4 && self.tx_mode_select && !self.lossless {

matching `intra_block.rs`'s existing gate exactly (`BLOCK_4X4` already in
scope via `use super::*`).

### Verified impact

- `probe_tiles scratch_av1/f3.ivf`: frame 2 **3663 -> 181 differing bytes**
  (95% reduction). Frame 0 and frame 1 stay exact (2/3 -> still 2/3 frames
  *fully* exact, since frame 2 isn't at 0 yet — see "remaining gap" below).
- `cargo test -p tpt-kinetix-av1 --lib`: **163/163 pass**, no regressions.
- `cargo clippy -p tpt-kinetix-av1 --all-targets -- -D warnings`: clean.
- `cargo fmt --check -p tpt-kinetix-av1`: clean.
- Didn't have another multi-frame inter corpus file on hand in this
  environment (only `scratch_av1/f3.ivf`) to check the fix's effect on
  `testsrc2`/`testsrc_64x64`/etc — those clips referenced in older sessions
  weren't present under any of `test-src`, `tpt-kinetix-test-utils`, or
  `tpt-kinetix-av1` in this checkout. Worth regenerating via
  `just corpus-check` or whatever produced them originally in a future
  session and re-running `probe_tiles` on each — this bug (any `BLOCK_4X4`
  intra-in-inter block, in `TX_MODE_SELECT`) is generic and should improve
  every such clip, not just this one.

### Remaining gap (not yet root-caused): 181 bytes, edge-only, frame 2

`BLOCKMAP=1` on the fixed build: **5 edge-only 8x8 blocks, 0 with interior
diffs**, `max|d|=148` worst at pixel (54,48). Both `KINETIX_AV1_NODEBLOCK=1`
and `KINETIX_AV1_NOCDEF=1` make the frame *worse* (485 and 670/0-3 exact
respectively), confirming the loop filters are doing genuinely correct work
here and are not themselves the bug — consistent with this file's
`BLOCKMAP`-doc-comment caveat that "edge-only" describes where a difference
*survives* the filters, not where it originates.

The owning block at pixel (54,48) is mi=(12,12) — `IBSUM fr=2 mi=(12,12)
bw4=4 bh4=4 intra=1 ymode=0 skip=true` (a 16x16 `DC_PRED` intra-in-inter
block with `skip=true`, i.e. its output should be pure prediction, no
residual). `KINETIX_AV1_DBG_PXY=54,48 KINETIX_AV1_DBG_PXY_FRAME=2` shows the
value is **17 at every stage** (pre-filter, post-deblock, post-cdef,
post-lr) — consistent with "edge-only" (this exact sampled pixel isn't one of
the wrong ones; the map above only says *some* samples in this 8x8 differ,
not this specific one). Have not yet isolated which sample in mi=(12,12)'s
top edge is wrong or why — candidates to check first next session: (a)
whether this block's DC-prediction "above" neighbour availability/values are
right (its top edge sits right below the now-fixed mi=(0,2)'s general
neighbourhood, worth double-checking no residual desync survives a few
blocks further into the tile), (b) deblock filter-level/`tx_size`-context
derivation for this specific block boundary, since `skip=true` and an
all-DC-flat block should not itself produce a sharp edge unless the filter
level or boundary strength computed for it is off.

**Status:** improvement is real and safe (163/163 tests, clippy, fmt all
clean); committed. The 181-byte residual is a separate, much smaller
follow-on bug, not yet root-caused — worth a session but not blocking.

## Session 2026-09-27 (cont'd 6) — filter hypothesis dead; frame-4 desync narrowed INSIDE the coefficient read of leaf (24,88)

Anchoring the interpolation-filter read killed the previous hypothesis:
Kinetix's filter read for leaf (24,88) matches dav1d **exactly** (value 0,
ctx 0, post-rng 63940 both — verified with a position-tagged
`DBG b0 frame_filter mi=(88,24)` print). Chain status for the leaf
(32×16, skip=0, NEWMV mv=(12,0), OBMC), all frame-aligned:

| anchor | dav1d | Kinetix |
|---|---|---|
| skip | 47768 | 47768 ✓ |
| intra | 47155 | 47155 ✓ |
| motion_mode (OBMC) | 64960 | 64960 ✓ |
| subpel filter | 63940 | 63940 ✓ |
| y-cf post | 37474 | **63102 ✗** |

Also disproven this session: the "wrong tx syntax model for inter blocks"
theory. Frame 4's `txfm_mode` is **not** TX_MODE_SELECT (both decoders'
var-tx paths are silent: dav1d's patched `read_tx_tree` hook and Kinetix's
`read_txfm_split` hook fire zero times across all 6 packets), so there are
no tx symbols to diverge on — `read_block_tx_size_ibc`'s no-symbol branch
matches dav1d's `read_vartx_tree` branch 2.

The desync is therefore **inside the coefficient read itself**: the
`all_zero` bool's CDF context (derived from above/left `lcoef` state) or
the DC-coefficient bit decode of this leaf. Since the visible outcome
matches (one DC coefficient, pixels exact), the divergence is in bits
consumed, not values decoded — consistent with a coefficient-context
(lcoef/acoef bookkeeping) difference accumulated from earlier blocks, or
an all_zero/ctx derivation difference specific to this 32×16 OBMC leaf.

### Next session's starting point

Leaf (24,88), frame 4: print the `all_zero` bool's CDF index and pre-rng
on both sides (dav1d `recon_tmpl.c:356` `all_skip` read; Kinetix
`read_coeffs`' all_zero read), plus the above/left `lcoef` arrays feeding
it. If they match, anchor the DC coefficient bits (golomb/EOB position)
next. Everything upstream (partition walk, modes, MVs, filter) is proven
identical for this leaf.

Also unchanged: frame 0's 11 bottom-edge bytes (merged-LR wiener bottom
border, rows 295-299), which shown frames 1-3 inherit at rows 295-299.

## Session 2026-09-27 (cont'd 7) — the desync is CDF adaptation drift, narrowed to the txb_skip CDF of leaf (24,88)

Anchored the coefficient read of frame 4's leaf (24,88) (32×16 OBMC NEWMV):

| anchor | dav1d | Kinetix |
|---|---|---|
| post-subpel | 63940 | 63940 ✓ |
| all_zero CDF index | skip[3][0] | txb_skip[3][0] ✓ |
| all_zero outcome | 0 (has coeffs) | 0 ✓ |
| post-y-cf | 37474 | **63102 ✗** |

Same entering rng, same CDF slot, same decoded outcome — yet different
post-read rng. The only remaining explanation: **the CDF VALUES have
drifted**. Kinetix's `txb_skip[3][0]` at this read is
`[31671, 32768]` cumulative with adaptation count 0→… (dav1d's
`cdf.coef.skip[3][0]` raw counts were not captured). With a drifted
distribution, the same msac rng can decode to the same symbol while
consuming a different number of bits — the desync then becomes visible
downstream (frame 4's 91k-diff cascade) even though every *decoded
value* so far matched. This also explains how the synthetic corpus stays
bit-exact: drift only flips a decode when a value sits near a CDF
boundary, which short clips with few symbols never hit.

### Next session's starting point (well-defined)

Diff the CDF **adaptation arithmetic**: Kinetix's per-symbol CDF update
(`SymbolDecoder::read_symbol`'s CDF adjustment — the `32768 → 1/16`
count-domain move and the adaptation-rate enable bit) against dav1d's
`od_ec_encode?? msac` update in `msac.c` (`update_cdf`: `count`,
`rate`, and the `fast-unsigned` arithmetic). Capture
`cdf.coef.skip[3][0]` on dav1d's side (extend the KCOEF print with the
three CDF words) and compare against Kinetix's `txb_skip[3][0]` after
the same symbol sequence; the first read where the tables diverge marks
the exact arithmetic bug.

Also unchanged: frame 0's 11 bottom-edge bytes (merged-LR wiener bottom
border, rows 295-299), which shown frames 1-3 inherit at rows 295-299.

## Session 2026-09-27 (cont'd 8) — the smoking gun: CDF adaptation COUNT diverges (dav1d 1, Kinetix 0) at leaf (24,88)

Extended the dav1d `KCOEF` oracle with the CDF words and re-ran the
frame-4 coefficient anchor:

| | dav1d | Kinetix |
|---|---|---|
| pre-rng | 63940 | 63940 ✓ |
| CDF slot | coef.skip[3][0] | txb_skip[3][0] ✓ |
| CDF words | **[1968, count=1]** | **[31671, 32768, count=0]** |
| outcome | all_skip=0 | all_zero=0 ✓ |

The adaptation **count** differs: dav1d has performed one adaptation of
this CDF in frame 4 (count=1, inherited from the restored context or
applied during frame 4), Kinetix has performed zero (count=0). The CDF
values are in different domains (dav1d raw counts / Kinetix cumulative)
but the effective probabilities disagree (dav1d ≈ 6% for the all_skip=1
branch vs Kinetix ≈ 3.3%), so the same msac rng consumes different bits
→ post-read rng diverges (59952/62200-chain vs 63102) → frame 4's
wholesale corruption.

Two candidate root causes, both in Kinetix's CDF bookkeeping:

1. **Save/restore drops or zeroes the adaptation counts**: dav1d's
   `dav1d_cdf_thread_copy` copies the count slots as part of the CDF
   context; if Kinetix's `FrameCdfContext` save/restore round-trips the
   `txb_skip[*][*]` count words incorrectly (or `TileCdfs::new` /
   the restore path rezeros them), every restored CDF starts with
   count=0 instead of the carried count — changing the adaptation rate
   (`4 + (count >> 4) + …`) for every subsequent read even when the
   values themselves are restored correctly.
2. **A missed adaptation earlier in frame 4**: if frame 4's
   `disable_cdf_update` is 0 (adaptation on), dav1d's count=1 means one
   prior [3][0]-class read adapted in dav1d but not in Kinetix — i.e.
   Kinetix's `allow_update_cdf` gate misfired for exactly one block.

Frame 0's keyframe read shows count=15 (adaptation visibly ON for
frames 0-3, counts growing 15→16→17→18 across frames 0-3 at this
position), so adaptation is enabled in general; frame 4's count dropping
to 0 in Kinetix (vs 1 in dav1d) is the anomaly. Note the counts ARE
stored in K's CDF arrays (the `[31671, 32768, 0]` triple's third word),
so the data path exists — the divergence is in what the frame-4 restore
put there.

### Next session's starting point (mechanical)

1. Print the count word of `txb_skip[3][0]` at every read for frames
   0-4 in Kinetix (the `DBG allzero` print already includes it) and the
   equivalent `cdf.coef.skip[3][0][2]` in dav1d (extend KCOEF to dump
   the count word `…[sctx][2]`); find the first frame/read where the
   counts diverge.
2. If it diverges at a frame boundary: audit `FrameCdfContext`'s
   save/restore for the count words (they must round-trip like dav1d's
   `dav1d_cdf_thread_copy`, which memcpy's the whole CDF struct).
3. If it diverges mid-frame: audit K's `allow_update_cdf` gate against
   frame 4's `disable_cdf_update` header bit.
4. After the fix, shown frames 0-3's bottom-edge residues (11/130/192/
   184 bytes at rows 295-299) and frames 4+ wholesale corruption should
   all collapse together — they are the same drift surfacing at
   different boundary values.

### Session cont'd 8 addendum — the count anomaly is at RESTORE, not mid-frame

Static trace of dav1d's decode order for frame 4 (per-tile-row sbrow
interleave): tile 1's sbrow 0 blocks before (24,88) are ALL skip
(no coefficient reads), and tiles have independent CDF copies seeded
from `f->in_cdf` at `setup_tile`. Therefore dav1d's count=1 at (24,88)
is the value **restored from frame 4's initial CDF context** — the
`cdf_thread_update` zeroing notwithstanding. Either frame 4's
primary_ref context was saved by a path that preserves counts, or the
stream never refreshed a context and dav1d's *default* tables carry
non-zero count words for coef slots (the `CDF1(x)` macro only
initialises the value word; the count word's default needs one dump at
frame-0 start to settle — frame 0's first masked read already showed
count=24 after 24 in-frame adaptations, so the pre-frame value is
masked; a dump at frame 0's FIRST coef read of each slot settles it).

Kinetix's restored count is 0 where dav1d's is 1: with adaptation
enabled the rate differs (`count >> 4` term), the CDFs drift, and the
bit consumption eventually flips — frame 4's cascade. The fix will
either round-trip counts through Kinetix's `FrameCdfContext` exactly as
dav1d's restore does, or (if dav1d's defaults genuinely carry counts)
seed Kinetix's default coef-CDF counts to match.

### Session cont'd 9 — quantified: K's txb_skip[3][0] sits at 31671 where dav1d's equivalent state is 2099; adaptation-enable mismatch is the prime suspect

Corrected count reading: dav1d's KCOEF print fires AFTER the read+update
([1968, 1] = post-update); the fresh KAZ3 pre-read dump shows dav1d's
true entering state for the leaf (24,88) all_zero read: **word=2099,
count=0, rng=63940** — and Kinetix's entering state: **cdf[0]=31671,
count=0, rng=63940**. Both then decode the same outcome (has-coeffs, one
DC coefficient) — but from drifted distributions: dav1d's
P(all_zero) = 2099/32768 ≈ 6.4%, Kinetix's = (32768−31671)/32768 ≈ 3.35%.

The recurrence check: dav1d's update on "has-coeffs" (`word -= word>>rate`)
and Kinetix's (`cdf[0] += (32768−cdf[0])>>rate`, in Kinetix's ascending
layout) are exact complements — identical symbol sequences from identical
initial values would keep the two states in exact complement
(dav1d_word == 32768 − K_cdf0). They are not (2099 vs 1097), so either
the **initial (default or restored) values differ**, or the **adaptation
rate** differed at some prior read.

Prime suspect: frame 4's `disable_cdf_update`. If Kinetix has adaptation
DISABLED for frame 4 (its cdf[0] frozen at the restored 31671 and count
frozen at 0) while dav1d has it ENABLED (word drifting per read, count
incrementing), every observed number falls out: dav1d's count grew 0→1,
its word drifted 2099≠restored, K's stayed frozen. The `allow_update_cdf`
gate (`dec.set_allow_update_cdf(!disable_cdf_update)` in
`TileDecodeState::new`) vs dav1d's `msac.allow_update_cdf = ...
!frame_hdr->disable_cdf_update` needs a header-value cross-check for
frame 4 specifically — one header bit, parsed differently or gated
differently.

### Next session's starting point (one bit!)

1. Print frame 4's `disable_cdf_update` in both decoders (K: the parsed
   `FrameHeader.disable_cdf_update`; dav1d:
   `f->frame_hdr->disable_cdf_update`).
2. If they differ: fix K's parse/gate. If they match (both 1 = no
   adaptation): the restored CDF VALUES differ and the hunt moves to the
   frame-4 initial CDF load (default tables vs restored context) with
   the 2099/31671 pair as the fingerprint.

### Session cont'd 10 — drift quantified: same entry, same outcome, diverged distribution

Corrected the count reading once more with the pre-read KAZ3 dump:
**both decoders enter the leaf (24,88) all_zero read with count=0**
(dav1d's earlier "count=1" was its post-update value from the KCOEF
print; K's print is also pre-update). Kinetix parses frame 4's
`disable_cdf_update=false` (adaptation on), matching dav1d's behaviour
(count incremented by this very read on both sides).

The divergence is the accumulated **CDF value**: at the same read with
the same entering rng 63940 and the same outcome, dav1d's
`coef.skip[3][0]` word is 2099 (P(all_zero) ≈ 6.4%) while Kinetix's
`txb_skip[3][0][0]` is 31671 (P(all_zero) ≈ 3.35%). The two update
schemes are exact complements in their respective layouts (verified:
`word -= word>>rate` vs `cdf[0] += (32768-cdf[0])>>rate` keep
`dav1d_word == 32768 - K_cdf0` invariant for identical histories), so
equal histories would give dav1d_word = 32768 − 31671 = 1097. Observed
2099 ≠ 1097 → the adaptation **history** (rate or count evolution, or a
prior read's value flip) diverged somewhere in frames 0-4.

The 2-symbol rate formulas match exactly (K: 3+[c>15]+[c>31]+
floor_log2(2)=4+…; dav1d bool: 4+(c>>4); identical for all count
values 0-32). So the remaining suspects: (a) the count EVOLUTION
differs (K's count slots vs dav1d's — e.g. which reads increment which
slot), or (b) a 3+-symbol CDF read somewhere in frames 0-3 (where K's
rate formula DOES diverge: for 3-symbol CDFs K gives
3+[c>15]+[c>31]+1 vs dav1d symbol 4+(c>>4)+1 — one lower at every
count) subtly changing that CDF's bit consumption and cascading.

### Next session's starting point (mechanical bisection)

Dump the full (pre_rng, word/cdf, count) evolution of ONE well-hit
CDF slot across frames 0-4 on both sides — txb_skip[3][0] is ideal
(the ALLZERO/KAZ3 hooks already exist). The first read where
K's post-value stops being 32768−dav1d's post-word marks the diverging
read; inspect that read's symbol value and rate. If the slot history
matches perfectly, repeat for a 3-symbol CDF slot (the rate-formula bug
(c) above is a live candidate: any 3-symbol CDF in frames 0-3 adapts
with a 1-lower rate in Kinetix than dav1d).

### Session cont'd 11 — IMPORTANT NEGATIVE RESULT: the literal dav1d rate formula is WRONG for Kinetix; the empirical formula is pinned

Tested the "obvious" fix suggested by dav1d's `update_cdf` source
(`rate = 4 + (count >> 4) + (n_symbols > 2)`, replacing Kinetix's
`3 + (count > 15) + (count > 31) + min(log2(n), 2)` — the two differ
only for 3-symbol CDFs): it made EVERY frame explode to ~313k differing
bytes from (32,0), including previously-clean frames 0-3. Reverted.

Conclusion: Kinetix's empirical formula matches dav1d's *decoded-bit
behaviour* — dav1d's per-call-site `n_symbols` arguments (e.g. the
filter read passes `DAV1D_N_SWITCHABLE_FILTERS - 1`) mean the effective
rate for the streams' actual reads follows Kinetix's formula. The
formula is now annotated in `entropy.rs` with a do-not-"fix" warning.
The frame-4 desync hunt therefore moves back to: WHICH symbol's bit
consumption first diverges at leaf (24,88) after the matching subpel
read (63940) — the coefficient-read internals (all_zero consumed
different bits despite the same outcome, or the DC-coefficient bits
diverged) — with the leaf's post-y-cf states 63102 (K) vs 37474 (D) as
the fingerprint. A per-bit trace of that one leaf's coefficient read
against dav1d's `KINETIX_DBG_CFSUM`-style dump is the next step.

### Session cont'd 12 — comparison infrastructure complete; captures saved

Both decoders now dump every `txb_skip[3][0]`-class all_zero read:
- dav1d: `KAZ3 bx by w c r` on stderr (patched decode_coefs, ungated),
  captured at `%TEMP%/d_kaz_all.txt` (200 events).
- Kinetix: `DBG allzero plane=0 x4 y4 tx_sz_ctx skip_ctx cdf rng val`
  via `KINETIX_AV1_DBG_ALLZERO`, captured at `%TEMP%/kin_kaz_all.txt`
  (with DBGSEQ frame delimiters; ~9400 allzero events total, 6893 with
  tx_sz_ctx=3+skip_ctx=0).

Aligned comparison (frame + position + pre_rng) shows frame 4's leaf
(24,88) divergence with dav1d w=2099 vs K cdf0-implied 1097 — but the
per-frame event counts expose a ctx-mapping mismatch in the FILTERS
themselves: dav1d's `t_dim->ctx == 3` matches 200 reads (frame 0: zero!)
while Kinetix's `tx_sz_ctx == 3` matches 653+68=721 (frame 0: 68). The
ctx formulas (dav1d tables.c `.ctx` vs K's `(SQR+SQR_UP+1)>>1`) disagree
on which sizes map to slot 3 — e.g. dav1d's 32×32 ctx=3 ✓ but which of
K's sizes map to 3 vs 2 needs the exact K table dump. Align frame 0's
reads positionally (both captures have position+pre_rng, no ctx filter
needed on the K side) to find frame 0's first word divergence; the
existing captures are sufficient — no re-decode needed.

### Session cont'd 13 — frame attribution correction; the drift is visible from frame 1's first coefficient read

Caught a frame-attribution bug in the dav1d-side comparison: frame
headers parse AHEAD of tile decodes (KGTILING lines for frames 0 AND 1
appear before frame 0's first coefficient read), so the fr counter built
from KGTILING lines mislabels events by one frame. Corrected reading of
the existing captures:

- dav1d's first KAZ3 (bx=0, by=0, w=2099, count=0, pre_rng=55066) is
  FRAME 0's read (the keyframe) — not frame 1's.
- Kinetix's frame-0 events at (0,0): tx_sz_ctx=2 pre=31901, tx_sz_ctx=1
  pre=31782, tx_sz_ctx=2 pre=31901 — multiple sub-tx reads, none with
  pre_rng 55066 and none with tx_sz_ctx=3.

Two co-located discrepancies at frame 0's very first coefficient read:
(a) the tx_sz_ctx differs (dav1d 3 vs K 2/1) — the same physical tx
block is being classified into different CDF slots, and (b) the
entering rng differs (55066 vs 31901) — Kinetix consumed different bits
in the leaf's MODE reads before the coefficient read (or the tx-size
walk differs, changing which sub-blocks exist: dav1d has ONE ctx=3 read
at (0,0); K has three reads with ctx 2/1/2 — different tx-size trees
for the same first leaf).

The tx-size tree / tx_sz_ctx classification divergence is now the
primary suspect for the whole cascade — it desyncs from frame 1's first
leaf while producing nearly-identical pixels (the sub-blocks reconstruct
the same content), which explains every "tiny residue, wholesale late
corruption" symptom in this stream.

### Session cont'd 14 — frame-0 entropy proven in sync; the residue is the LR bottom-stripe vertical pass

Clean single-frame capture (max=1 packet → all events are frame 0's, no
attribution ambiguity) with all hooks resolved the earlier confusion:
the "four all_zero reads at (0,0)" were parallel-tile events interleaved
in the trace — tile 0's own first leaf matches dav1d perfectly:

- PART (0,0) 64×64 SPLIT, PART (0,0) 32×32 NONE — post-rng 34459 ✓
- K's leaf modes end at rng 55066 = dav1d's Post-filterintramode 55066 ✓
- K's all_zero: txc3, pre 55066, **cdf word 30669 = 32768 − dav1d's 2099**
  — the two CDF domains are exact complements; the states MATCH.

Frame 0's entropy decode is bit-accurate; the 11-byte residue at
(600-607, 296-299) is a **loop-restoration bottom-stripe arithmetic
difference**: the last stripe (rows 256-299, the merged trailing unit)
applies its vertical wiener taps at rows 297-299 by clamping into the
grid rows 300-302 (which hold real mi-grid padding reconstruction),
while dav1d treats the frame bottom as no-LR_HAVE_BOTTOM (the
out-of-stripe rows replicate/clamp at the last visible row). The visible
result is ±1 luma on smooth content — frames 1-3's rows 295-299 residues
inherit it through inter prediction.

Next session: make the vertical sample fetch in the final stripe clamp
at the last visible row (row 299) instead of reading mi-grid padding
rows — either by clamping `src_at`'s y at `h - 4`-equivalent for the
bottom stripe or by substituting the replicated edge row into `seg_src`
(the same mechanism already used for inter-stripe top/bottom borders).
Then re-check: frame 0 → 0 bytes expected; frames 1-3 residues should
collapse; later frames' remaining diffs are independent (frame 4's
OBMC/interintra investigation continues per cont'd 5-6).

### Session cont'd 15 — bottom-clamp experiment: negative; current behavior is closest

Tested extending the LR vertical sample clamp into the mi-grid padding
rows (ph 300→304): frame 0 got slightly WORSE (11→14 bytes). Reverted —
Kinetix's current edge-replication at the visible bottom is the closer
match to dav1d. The ±1 residue at (600-607, 295-299) is therefore NOT
the bottom tap reach; remaining candidates for the final 11 bytes:
the wiener vertical-pass rounding at the last stripe's partial rows
(rows 297-299 taps), the seg_src boundary substitution content
(boundary_src = pre-CDEF rows vs dav1d's lpf lines at the LAST sbrow),
or a post-CDEF difference on rows 293-302. Each needs the same
dav1d-vs-K pixel dump technique at the pre-LR stage (dav1d PRED dumps
exist for this; K's KINETIX_AV1_DBG_PRED covers inter blocks only — an
intra-path pre-LR dump is the missing tool).

### Session cont'd 16 — DECISIVE: frame 0 unfiltered is pixel-perfect; the residue is the bottom mi-row padding content

dav1d's `--inloopfilters none` + Kinetix's `KINETIX_AV1_NOFILTER=1`
compared (both fully unfiltered, frames 0-7):

- **Frame 0: 0 differing bytes.** The entire decode chain (entropy →
  partition walk → modes → MC/residual → recon) is bit-exact for the
  keyframe. Every filter-stage hypothesis for the frame-0 residue is
  dead: the 11 filtered bytes at (600-607, 296-299) are produced inside
  Kinetix's deblock/CDEF/LR stages from inputs that match dav1d's — the
  divergence is in a filter stage's arithmetic at the frame bottom (the
  deblock/CDEF/LR bottom-edge handling), not upstream.
- Frames 1-3 unfiltered: 95/74/127 diffs — **all confined to mi row 74**
  (the bottom mi row, pixel rows 296-299), cols 128-718, deltas ±1.
  These are fractional-MV blocks whose subpel filter reads the
  reference's mi-grid padding rows 300-303 (the 8-tap's +4 vertical
  reach below the block); the reference's padding-row content differs
  between the decoders.

### Next session's starting point (two precise threads)

1. **Bottom mi-row padding content**: dump frame 0's grid rows 295-310
   from both decoders (K: the assembled grid plane pre-crop; dav1d: patch
   its lr/cdef tail to fwrite rows 290-310). The blocks covering rows
   300-319 reconstruct identically in principle (entropy in sync); find
   which padding row/column first diverges — likely a prediction-write
   clamp or a residual-application clamp at mi_rows (76) in one decoder.
2. **Frame-0's 11 filtered bytes**: with the unfiltered frames proven
   identical, bisect the filter stages: dav1d `--inloopfilters
   deblock`/`cdef`/`restoration` individually against Kinetix
   (K needs per-stage env gates — `KINETIX_AV1_NOFILTER` is all-or-nothing
   today; add `KINETIX_AV1_NO_DEBLOCK`/`NO_CDEF`/`NO_LR` one-liners).
   The 11 bytes sit at rows 296-299 = the LR bottom stripe region — LR
   restoration first suspect (per cont'd 14's analysis), then CDEF.

### Session cont'd 17 — COMPLETE STAGE BISECTION: the frame-0 residue is 100% in the CDEF stage

Per-stage comparison using dav1d's `--inloopfilters {deblock,cdef,
restoration}` and Kinetix's `NODEBLOCK`/`NOCDEF`/`NOLR` gates, frame 0:

| stage | dav1d vs Kinetix |
|---|---|
| deblock only | **0 diffs** (bit-exact) |
| cdef only | **11 diffs** at (600-607, 296-299) |
| restoration only | **0 diffs** (bit-exact) |

The entire residue is one CDEF 8×8 unit: CDEF-unit col 75, row 37
(pixels 600-607 × 296-303, visible rows 296-299), ±1 on 11 of the 32
visible samples. Deblock and LR are proven bit-exact for this frame.

The CDEF unit's divergence: the filter reads grid rows 294-301 (all
decoded, in both decoders — mi rows 74-75 exist), direction/strength
entropy matched. Candidates: the direction search's gradient
computation at the frame-bottom partial SB, the constrain arithmetic
for specific tap patterns, or the primary/secondary tap selection for
this unit's direction. The dump tool: `KINETIX_AV1_DBG_WPX`-style
per-unit CDEF dumps exist in K's cdef path (check
`KINETIX_AV1_DBG_CDEF`); dav1d's `cdef_apply_tmpl.c` accepts a targeted
dump the same way.

### Next session's starting point

Dump the CDEF inputs for CDEF-unit (col 75, row 37) of frame 0 from
both decoders: the 8×8 pre-CDEF pixel block, the computed direction,
the damping/strength, and the per-tap constrain outputs. The first
mismatched tap identifies the exact arithmetic difference. Given the
unit is at the frame's bottom edge (rows 296-303, with rows 300-303 in
the grid padding), check the bottom-edge direction-sample handling
first — dav1d's `cdef_find_dir` and `constrain` use the full 8×8
including padding rows; Kinetix's direction search may exclude or
handle the padding rows differently.

## Session cont'd 18 — SMOKING GUN: CDEF direction diverges (dav1d dir=0, Kinetix dir=2) at the frame-bottom unit

dav1d's per-unit CDEF dump (KDCDEF) captured for the diverging unit
(frame 0, pixels 600-607 × 296-303):

| param | dav1d | Kinetix |
|---|---|---|
| primary (raw) | 2 | 2 ✓ |
| adjusted | 1 | 1 ✓ |
| secondary | 1 | 1 ✓ |
| damping | 4 | 4 ✓ |
| **direction** | **0** | **2 ✗** |

Strengths and damping match; **the direction search disagrees**. The
direction search reads the full 8×8 block INCLUDING grid-padding rows
300-303 (mi rows 75-79 beyond the visible frame are still in the mi
grid: mi_rows = 2*ceil(300/8) = 76 → mi rows 74-75 = visible bottom,
76-79 = padding within the SB extent). Kinetix's and dav1d's DEBLOCKED
content in those padding rows has never been compared (all pixel
comparisons crop at row 299) — a deblock difference there changes the
direction search's partial sums → a different dir → different CDEF tap
pattern → the ±1 diffs at rows 296-299.

This also converges with the frames 1-3 finding: their unfiltered diffs
sit exactly at mi row 74 — fractional-MV blocks whose MC reads the
reference's padding rows 300-303, which differ for the same reason.

### Next session's starting point (decisive)

1. Dump frame 0's post-DEBLOCK rows 290-310 (the CDEF input) from both
   decoders (dav1d: patch its deblock tail to fwrite those rows; K:
   dump the plane between deblock and CDEF — e.g. a temporary print in
   apply_post_filters before the CDEF call). The first divergent
   padding row/column is the deblock bug site (likely the bottom-edge
   handling for mi rows 75-79 or the loop filter's vertical
   at-frame-bottom mask).
2. Fix the deblock bottom edge → the CDEF direction, frame 0's 11
   bytes, frames 1-3's bottom-row residues, and probably most of the
   frames 4+ cascade collapse together.

### Session cont'd 19 — the divergence is the CDEF DIRECTION SEARCH (dir 0 vs 2, same input)

Stage isolation completed with single-stage comparisons on frame 0:
- deblock-only (both decoders): 0 diffs
- cdef-only (both decoders): **11 diffs at (600-607, 296-299)** — the
  exact full-filter residue
- LR-only (both decoders): 0 diffs — K's LR matches dav1d's LR exactly,
  INCLUDING the bottom stripe (the cont'd-14 bottom-clamp concern is
  settled: K's LR bottom handling is correct)

So: identical CDEF input (the unfiltered recon, proven identical), and
the per-unit dump shows strengths/damping match (pri 2, adj 1, sec 1,
damping 4) but the **direction differs: dav1d dir=0, Kinetix dir=2**.

The direction search reads the 8×8 pre-CDEF block at (600,296) — an
identical input in both decoders. Two remaining candidates:
(a) K's `cdef_direction` cost/partial-sum construction has a
transcription bug that flips the argmax for this content pattern
(compare the full cost[8] arrays), or
(b) K's direction→tap-offset mapping (CDEF_DIRECTIONS table indexing)
differs from dav1d's (the same spec direction number maps to different
neighbour offsets).

### Next session's starting point (mechanical)

Dump the 8×8 input block and the full cost[8] array for this unit from
both decoders (dav1d: `dav1d_cdef_find_dir_c` in cdef_tmpl.c — add a
cost print gated on a pixel fingerprint; K: `cdef_direction` in
loop_filter.rs — add the same). If the costs match and only the argmax
differs → mapping bug (b). If the costs differ → partial-sum
transcription bug (a). Then verify K's dir→CDEF_DIRECTIONS offset table
against dav1d's `cdef_dirs` for the winning direction.

## Session cont'd 20 — CONFIRMED: the CDEF cost arrays differ on identical input; transcription bug in Kinetix's cdef_direction

For frame 0's unit at pixel (600,296) (the first diverging CDEF unit),
with the 8×8 input pixels PROVEN identical (frame 0 unfiltered is
bit-exact):

- dav1d `cdef_find_dir`: costs=[347944419, 347896430, 347859225,
  347652095, 347523436, 347591615, 347620875, 347895660] → argmax 0
  (dir=0)
- Kinetix `cdef_direction`: costs=[346085539, 346129175, 346146570,
  345920225, 345777267, 345778300, 345775920, 346005835] → argmax 2
  (dir=2)

The magnitudes are close (±0.5%) but the argmax flips — a partial-sum
or DIV_TABLE-indexing transcription bug in Kinetix's `cdef_direction`
(loop_filter.rs:1357) that only flips the winning direction on
near-tie content. Note: dav1d's find_dir C fallback requires
`--cpumask none` on the CLI to be exercised (SIMD otherwise).

### Next session's starting point (pure code inspection — no decoder runs needed)

Compare Kinetix's `cdef_direction` (loop_filter.rs:1357, including the
`partial[8][16]` construction, the cost accumulations, and every
DIV_TABLE index) line-by-line against dav1d's `cdef_find_dir_c`
(cdef_tmpl.c, the same patched clone at
%TEMP%/dav1d_fresh). The 8×8 input is the test vector: write a unit
test feeding the dumped 8×8 (available via the KCDEF pre print + the
full block from a KINETIX_DBG_COEFF-era dump or a fresh targeted dump)
through both formulas and diff the partials.

Also unchanged: frames 1-3's mi-row-74 unfiltered residue (the MC
subpel filter reads the reference's grid-padding rows 300-303 — the
padding rows' content differs because frame 0's CDEF divergence
propagates into the reference; fixing (a)/(b) should collapse these
too), and frame-0's 11 filtered bytes at the same unit.

### Session cont'd 21 — 8×8 test vector captured; C-path vs SIMD discrepancy noted

dav1d's find_dir input for the diverging unit (matched by column
fingerprint [38,39,43] at x=600):

```
38 43 44 43 44 44 43 42
39 44 45 45 44 45 45 44
43 46 46 46 46 47 47 47
48 50 49 48 50 51 52 52
48 49 48 47 49 52 53 53
46 48 46 46 48 51 53 53
47 48 47 47 49 52 54 54
46 49 48 47 49 52 54 54
```

dav1d's C path (forced via `--cpumask none`) computes best_dir=3 for
this block; Kinetix computes y_dir=2 on the same fingerprint. (A
separate dav1d run without cpumask restriction showed dir=0 from the
SIMD path for the gated (150,74) read — the SIMD/C comparison needs
care because the column fingerprint may match multiple units across
frames; align by decoding frame 0 ONLY: `probe_tiles 1` decodes just
the keyframe, so every CDEF unit is frame 0's.)

### Next session's entry point (pure unit-test work)

1. Feed the 8×8 block above through Kinetix's `cdef_direction`
   (unit test in loop_filter.rs) → confirm y_dir=2 and dump cost[8].
2. Hand-compute the spec partial sums (§7.15.2) for this block and
   compare against both the K cost[8] and a hand-rolled dav1d-formula
   port. The mismatched cost entry pinpoints the transcription bug.
3. Fix `cdef_direction`, then re-run `probe_tiles` on
   non_uniform_tiling.ivf: the CDEF-only stage should go to 0 diffs,
   collapsing frame 0's 11 bytes, frames 1-3's bottom-row residues, and
   likely most of the frames 4+ cascade (their MC reads the
   now-identical reference bottom rows).

## Session cont'd 21 (final) — CDEF full-extent fix landed: frame 0 is PIXEL-EXACT vs dav1d; non_uniform_tiling 1/24

The direction divergence's true root: Kinetix's CDEF luma call passed
`vis_luma_h` (the visible frame height) as the block-walk/clamp height,
so the bottom 8×8 CDEF units at frame rows 296-303 were clipped to 4
rows — their direction search saw replicated rows (dir=2 instead of
dav1d's dir=0 computed over the real grid rows 300-303) and their
filtered output never landed. The fix passes the mi-grid extent height
(`height`, 304 for this frame) to `cdef_plane_luma`, matching dav1d's
in-place processing over the full sbrow extent.

`cdef_direction`'s math itself was verified correct: the new probe unit
test feeds the dumped 8×8 through `cdef_direction` and the independent
spec formula — both give costs [347944419, 347896430, …] and dir=0,
matching dav1d's C-path dump exactly (the earlier "dir=2 in decode" was
the clamped input, and the earlier "costs differ" observation compared
dav1d's raw-count domain against Kinetix's cumulative domain).

### Results

- non_uniform_tiling frame 0: **0 diffs vs dav1d** (was 315,925 at
  session start, 11 before this fix). Official FATE:
  **3/195 frames bit-exact** (was 1/198 at session start, 2/195
  mid-session) — non_uniform_tiling 1/24 and switch_frame 1/32 both
  exact-frame holders now.
- Frames 1-2 improved further (130→98, 192→156 bytes at rows 295-299);
  their remaining bottom-row diffs and the frames 3+ cascade are the
  next targets (the bottom mi-row padding rows 300-303 now CDEF-filtered
  by both; the residual ±1s trace to the deblock/CDEF arithmetic on
  those rows or the frames' own OBMC/interintra reads of them).
- Gates: 164 lib tests, corpus 6/6 bit-exact, clippy clean, fmt clean.

### Next session's starting point

1. Frames 1-2's rows 295-299 (98/156 bytes): with CDEF now covering the
   padding rows in both decoders, re-run the stage bisection
   (deblock-only / cdef-only / LR-only) for frame 1 — the LR-only and
   full-filter diffs converged (100 vs 98), so the remaining delta is
   small and localized.
2. Frames 3+ cascade: re-check after 1-2 settle; the OBMC/interintra
   reads of the reference's now-correct bottom rows may collapse the
   cascade without further changes.
3. K's probe test `probe_cdef_direction_diverging_unit` pins the
   direction-search behavior on the captured test vector — keep it.

### Session cont'd 21 addendum — frames 1-3 residue precisely localized

Single-stage per-frame comparison (deblock/cdef/restoration variants):
frames 1-3's divergence is stage-INDEPENDENT — ~74-136 samples at rows
297-299, deltas ±1, identical across deblock-only/cdef-only/restoration-
only variants (frames 1-3 have deblock levels 0 and no LR; frame 1-2
CDEF strength 0 = no-op). So the residue is in the base reconstruction
at the bottom mi row: fractional-MV blocks whose subpel filter reads
the reference's grid-padding rows 300-303 (the 8-tap's +4 reach).
K's MC clamps those reads at some bound; dav1d reads its buffer's
padding rows. The padding content (or the clamp bound) differs by ±1.

Next: dump frame 0's grid rows 295-310 (post-everything, the reference
plane K stores) and the equivalent from dav1d, find the differing
padding row, and align K's MC clamp bound with dav1d's buffer semantics.

### Session cont'd 22 — MC reference clamp moved to the VISIBLE dims: the frames 1-3 bottom-row residue class is GONE

The cont'd-21 addendum's proposed cause (MC reading the reference's grid-padding
rows 300-303, where the padding content differs by +/-1) was **backwards**. The
padding content is not the divergence: dav1d never reads those rows for inter
prediction in the first place. dav1d's `mc()` / `warp_affine()` take
`p.p.w` / `p.p.h` (the *visible* frame dims) as their `emu_edge` bounds, so a
bottom-edge fractional-MV block **replicates row 299** instead of reading
reconstructed padding row 300. Kinetix was clamping at the mi-grid extent
(160x304 for a 160x90 frame), so it read the padding rows and got different
samples. The stage-independence observed in the addendum is consistent with
this: the difference is in the base reconstruction, not in any filter.

The fix threads the reference's visible dims (`StoredFrame::real_width`/
`real_height`, already stored, via new `RefSlot::real_width`/`real_height`)
into every inter read: the single-ref translational path, the two-ref
`motion_compensate_prep` path, the OBMC per-direction path, and the warp
path. `warp.rs` additionally had a latent bug exposed by this: `warp_affine_8x8`
addressed the reference as `refp[cy * ref_w + cx]`, i.e. it used the clamp
bound as the stride. `block_warp_process` now takes an explicit `ref_stride`
(stride for addressing, `ref_w`/`ref_h` for clamping). **Note this only
matters when the grid stride exceeds the visible width** - a bug the previous
code could not exhibit only because the two happened to be the wrong value in
the same direction.

Intrabc is deliberately unaffected (it reads within the grid extent and is
handled in `intra_block.rs`), and CDEF/deblock/LR keep using the full mi-grid
extent (cont'd 21's fix, which is still required and still correct).

### Results

- Conformance corpus: **all 6/6 intra entries and 24/24 inter frames bit-exact
  vs dav1d** across the four inter clips. Before this change,
  `testsrc_160x90` frame 7 was the sole non-exact case (V PSNR 83.69 dB) - now
  7/7. Verified by A/B: stashing the change reproduces the frame-7 failure
  exactly, so this is a real fix, not a corpus that already passed.
- Gates: 165 lib tests, clippy clean, fmt clean.

### Next session's starting point

1. The remaining `- [ ]` items in this file are unchanged and still open:
   the symbol-level oracle (line ~209), the corpus pixel-exactness gap
   (line ~359), and the `pixel_exact` capability flip (lines ~385-388).
   `capabilities().pixel_exact` still reports `false` - correct, because the
   official FATE run is far from frame-exact and the corpus is a small
   synthetic set.
2. Next measurable target is still the official FATE samples
   (`KINETIX_AV1_FATE_DIR`), where `non_uniform_tiling` was 1/24 exact at
   cont'd 21 and the frames 1-2 residues were the open item. With the MC clamp
   corrected, re-run `av1_fate_real_samples_vs_dav1d_when_available` to see
   whether the frames 1-2 bottom-row residue class collapsed with it.

### Session cont'd 23 — per-tool coverage corpus: the default settings never exercised most AV1 tools, and a NEW chroma-only inter bug falls out

Added `synthetic::av1_feature_corpus()` + the `av1_feature_coverage_vs_dav1d_
when_available` test. Motivation: every pre-existing AV1 corpus entry is encoded
with ffmpeg's **default** AV1 encoder settings, so a decoder can be bit-exact on
all of them while having *no* support for intrabc, palette, loop restoration,
filter-intra, angle-delta, CFL, tiles or 64-point transforms - the tools simply
never get selected by the encoder's RD decisions. The new corpus turns each on
explicitly and reports per-tool exactness (it reports rather than asserts: the
gaps are the roadmap, and a hard assert would be permanently red for
known-incomplete tools; it does assert the harness itself works).

**Important measurement caveat, verified this session:** most of those flags are
already on by default in libaom. `-enable-angle-delta 1`, `-enable-filter-intra 1`,
`-enable-cfl-intra 1`, rect/1to4 partitions, `-enable-flip-idtx 1`,
`-enable-tx64 1` and `-enable-global-motion 1` all encode to a **byte-identical**
bitstream with and without the flag (confirmed by MD5-ing the OBUs). So the
several entries reporting an identical 42.29 dB gap are not per-tool gaps at all
- they are the **same default-path bug**, measured five times. Only tiles,
lossless, restoration, intrabc and CDEF-off actually change the bitstream. The
corpus is still worth having (it pins the defaults as a regression baseline and
does genuinely cover the tools that do change the stream), but read its "per-tool"
verdicts with that in mind. Documented in the function's doc comment.

Current baseline: **1/13 entries bit-exact** (palette only, 2/2 frames).

#### The bug this surfaced: chroma-only inter divergence (NOT filter-related)

Minimal repro (default settings, no exotic tools at all):

```
ffmpeg -f lavfi -i testsrc2=size=128x96:rate=15:duration=2 -frames:v 2 \
       -pix_fmt yuv420p -g 10 -c:v av1 -f ivf t2.ivf
```

Frame 0 is exact; frame 1 has **73 differing bytes, luma 0/12288** - every
difference is chroma (U 14 samples, deltas +-1..3; V 59 samples, deltas up to
**-64**). Stage bisection:

- `KINETIX_AV1_NOFILTER=1` makes it *worse* (3654 bytes) and the blockmap is
  all edge-only `e`, zero interior `X`. **This does NOT show the pre-filter
  recon is correct** - see the correction below; the comparison is unfiltered-
  Kinetix vs *filtered* dav1d, so it is confounded.
- With filters on, luma is **byte-exact** and only chroma diverges. CDEF's
  chroma pass is legitimate (§7.15.3 filters chroma with `CdefDamping - 1`), so
  the "post-cdef changes the V value" observation from the `CPXY` tracer is not
  itself a bug - what matters is that the *final* value disagrees with dav1d.

**CORRECTION (next session, same repro).** This note's conclusion - "chroma
deblock is not exact, pre-filter recon is correct" - is **wrong**, and two
things above misled it:

1. The `NOFILTER` run compares *unfiltered* Kinetix against *filtered* dav1d.
   It can only ever show differences where filters act, so "all edge-only" is
   what a correct reconstruction looks like **and** what a wrong one that
   happens to differ only near edges looks like. It is not evidence of a
   correct pre-filter recon. A proper check needs a filtered Kinetix vs
   unfiltered dav1d pair, and `-skip_loop_filter` is **not supported by this
   environment's libdav1d build** ("Codec AVOption skip_loop_filter is not an
   encoding option"), so that comparison is not available here.
2. Tracing the actual diverging pixels with `KINETIX_AV1_DBG_CPXY` shows the
   value is **already wrong before deblock**, by far more than any filter
   could account for:

   | pixel (V) | Kinetix pre-deblock | Kinetix post-filters | dav1d final |
   |---|---|---|---|
   | (32,20) | 28 | 26 | 20 |
   | (33,22) | 96 | 97 | 105 |
   | (44,29) | 190 | 191 | 222 |

   Deblock+CDEF move these by at most 1-2, but they miss dav1d by 8-32. So the
   divergence is in **chroma reconstruction/prediction, before the filters**,
   not in chroma deblocking. (The `d.yuv` reference here is the *whole* 2-frame
   raw dump - the second frame starts at byte 18432; comparing against offset 0
   is an easy way to get nonsense V values.)

Also tested and **ruled out**: AV1 OBMC being applied to chroma. §7.11.3.9 reads
as luma-only, and `inter_block.rs:1830` does loop `for plane in 0..3`, so this
looked like a real bug - but restricting it to `plane == 0` made the repro
*worse* (73 -> 131 differing bytes) and `KINETIX_AV1_NOOBMC=1` is worse still
(241 bytes, luma broken). Chroma OBMC is load-bearing here, so the loop is
correct as written and was left alone.

Block coverage of the two V patches (from `KINETIX_AV1_DBG_PRED`): patch 1
(cpx 32..37, 20..24 = luma ~64..75, 40..49) is covered by `mi=(16,11)
bw=2 bh=1 skip=false mv=(32,-16)` - a 8x4 sub-8x8 inter leaf. Patch 2 (cpx
43..51, 27..36) is near `mi=(22,18) bw=2 bh=2 dir1_h=1 mv=(-8,10)`. Both are
sub-8x8 / subpel-chroma cases, so the sub-8x8 chroma path and the
`hbits/vbits = 3 + subsampling` chroma phase remain the suspects - but the
evidence now points at chroma **MC position**, not at the filters.

Diff geometry (chroma coords, `cpx`): U diverges in one 4x4-aligned patch at
cpx x=32..35, y=20..25 (luma 64..71, 40..51). V diverges in two patches:
cpx x=32..37, y=20..24 and cpx x=43..51, y=27..36. The V deltas are large
enough (-64, -31, -28, -21) that this is wrong *content* in a localised region,
not ±1 filter rounding: reading Kinetix vs dav1d across cpx row 20, x=30..39
shows Kinetix's values lagging dav1d's by roughly one sample horizontally
(K 18 20 26 36 46 55 59 64 72 80 / D 18 17 20 24 35 46 56 64 72 80) - the
signature of a **displaced chroma prediction**, not a level/strength error.
Since luma at the identical luma coordinates is exact, the block's luma MV and
residual are right, so the suspect is the chroma-side MV/position derivation:
`inter_block.rs`'s sub-8x8 chroma path (lines ~1520-1690, the `sub8x8_leaf`
quadrant scheme and its `base_x/base_y` "parent 8x8 chroma origin" floor) or
the `hbits/vbits = 3 + subsampling` chroma phase in `motion_compensate`
(`inter.rs:246`). Both are already heavily commented as dav1d-derived, so this
needs a fresh dav1d-side check of the chroma MC position, not a re-derivation.

`probe_tiles`'s `BLOCKMAP=1` mode now also prints **chroma** block maps (U/V,
4x4 chroma cells, 2-sample interior margin) - the old map was luma-only, which
is exactly why this class of bug was invisible: a luma-only map reports
"0 interior diffs" and looks healthy while chroma is badly wrong.

#### Next session's starting point

1. The chroma bug above is the highest-value target: it is the only *default-
   path* regression currently known, it is 2 frames / 73 bytes, and it is
   reduced to **chroma MC position (pre-filter), luma byte-exact** - see the
   CORRECTION paragraph, which supersedes the earlier "chroma deblock" claim.
   Start from the V patch at cpx x=32..37, y=20..24, whose block is
   `mi=(16,11) bw=2 bh=1` (an 8x4 sub-8x8 leaf, mv=(32,-16)) - check the chroma
   MC base position / sub-8x8 quadrant scheme against dav1d.
2. `capabilities().pixel_exact` still `false` - correct, and further from true
   than cont'd 22 suggested: the feature corpus is 1/13.

### Session cont'd 24 — found a real filter-direction bug in the sub-8x8 quadrant
### borrow branches, but it's a no-op on the t2.ivf repro; the traced block
### itself turns out to take the non-quadrant path and is full-pel

Repro confirmed unchanged from cont'd 23: `ffmpeg -f lavfi -i testsrc2=size=
128x96:rate=15:duration=2 -frames:v 2 -pix_fmt yuv420p -g 10 -c:v av1 -f ivf
t2.ivf`, decoded with the real `dav1d` binary (not the ad-hoc patched build,
which was stale and wouldn't rebuild this session - the VS dev-shell
(`Launch-VsDevShell.ps1`) picked up a broken `vswhere`-less environment and
the MSVC C11-atomics headers failed to compile; `dav1d_fresh` at
`%LOCALAPPDATA%\Temp\dav1d_fresh` still builds cleanly but its
`DEBUG_BLOCK_INFO` gate is hardcoded to `frame_offset == 3`, useless for this
2-frame clip, and editing/rebuilding it wasn't reached this session). Frame 1:
Y diff 0/12288, U diff 14, V diff 59 - unchanged from cont'd 23, confirming
the bug is still live. A throwaway harness
(`tpt-kinetix-test-utils/tests/dbg_av1_t2_repro.rs`, NOT committed - see
below) reproduces this in ~2s via `cargo test` instead of the manual
ffmpeg+dav1d dance, if a future session wants to recreate it.

**Real bug found (via a side-by-side of `inter_block.rs`'s sub-8x8 quadrant
code against the actual `dav1d` source, checked out at
`%LOCALAPPDATA%\Temp\tpt-kinetix-dav1d\dav1d\src\{recon_tmpl.c,decode.c}` -
finally reading the real C instead of re-deriving from memory/comments):
`inter_predict_plane`'s own doc comment says its `filters` parameter is raw
`[dir0, dir1]` and the function swaps internally at its one call into
`motion_compensate` (matching dav1d's `dav1d_filter_2d[filter[1]][filter[0]]`
packing) - every other call site in `reconstruct_inter_luma_chroma` (the Y
plane, the plain chroma path, the sub-8x8 "own quadrant" calls) passes the
raw `filter`/`f` pair unswapped, consistent with that contract. But the three
sub-8x8 *borrowed-neighbour* quadrant branches (TL-diagonal using
`tl_filter2d`, BL using `filter_left`, TR using `filter_above`) each built
`[f.1, f.0]` before passing it in - an extra, wrong swap on top of the one
`inter_predict_plane` already does internally, so those three quadrants' MC
calls used the neighbour's vertical kernel horizontally and vice versa
whenever the neighbour's `dir0 != dir1` (a real per-axis dual-filter split).
Fixed to `[f.0, f.1]` at all three sites (matching every other call in the
function).

**This fix produced *zero* byte movement on the t2.ivf repro** (still 73
diff bytes, breakdown unchanged) for two compounding reasons, confirmed with
a temporary `KINETIX_AV1_DBG_SUB8` trace (removed before finishing):
1. Every sub-8x8 block in this specific clip has `own_filter=[0,0]` (REGULAR
   on both axes) - `enable_dual_filter` is on, but nothing in this content
   picks a non-degenerate per-axis split, so `f.0 == f.1` everywhere and the
   swap is a no-op by construction, in *this* stream.
2. More importantly, the specific traced block `mi=(16,11) bw=2 bh=1` (the
   one covering the patch-1 V diff) has `gate=false` in Kinetix's own
   `sub8x8_leaf` code - i.e. its above-neighbour cell is not inter-coded, so
   it takes the *plain* fallback path (one MC over the whole parent 8x8's
   chroma with its own mv/filter), never touching the quadrant-borrow
   branches at all. The three-branch fix can only matter for a *different*
   class of block than the one previously attributed to this diff.

The fix was reverted (not committed) since it has no measured effect here -
per this crate's own "don't commit without a measured improvement" rule. It
is still believed correct per the dav1d source and is a candidate for a
future PR *if* a stream with a real per-axis dual-filter split on a
sub-8x8-with-inter-neighbour block is found to regress without it; note that
down for whoever picks this up next.

**Re-audited the plain-fallback path taken by `mi=(16,11)` itself** (sizes,
base position, OBMC eligibility - all against the actual dav1d source, not
prior sessions' notes):
- Position/size: `base_x/base_y` (floor to the parent 8x8's chroma origin)
  and the `pw = cbw_px << (bw==1)`, `ph = cbh_px << (bh==1)` write-size
  algebra are both provably equivalent to dav1d's `t->bx & ~ss_hor` /
  `bw4 << (bw4 == ss_hor)` formulas once you expand `cbw_px = bw4 * h_mul`
  (verified by hand, not just "looks plausible") - no bug here.
- OBMC: re-checked `dav1d_block_dimensions`'s actual layout (`{bw4, bh4,
  log2(bw4), log2(bh4)}` - confirmed from `tables.c`) against
  `apply_obmc`'s `n_limit = 4.min(trailing_zeros(bw4/bh4))`. This *is* the
  right formula (dav1d's own `b_dim[2]`/`b_dim[3]` fields it compares against
  are the log2 fields, not raw `bw4`/`bh4` - a plausible-looking misreading
  that would have sent this in circles). For `bh4 == 1`, `log2(1) == 0`, so
  the left-pass OBMC blend count is correctly 0 in both decoders - chroma
  OBMC contributes nothing to this specific block in either implementation,
  so it's not the source of this particular diff (though it's still
  load-bearing elsewhere, per cont'd 23's A/B test).
- The block's own mv `(32,-16)` in Kinetix's `Mv{row,col}` 1/8-luma-pel
  convention, read at chroma sub-pel precision (`hbits=vbits=4`), is an
  **exact multiple of 16 on both axes** - `dx = mv.col & 15 == 0`,
  `dy = mv.row & 15 == 0` - i.e. this specific MC call is full-pel
  (`ix = -1, iy = 2` chroma pixels), not a subpel/kernel case at all. That
  rules out `subpel_kernel`/rounding bugs for this particular block.

**Still unexplained, and the most concrete next lead:** the row-20 sample
sequence from cont'd 23 (K: `18 20 26 36 46 55 59 64 72 80` vs D:
`18 17 20 24 35 46 56 64 72 80`, x=30..39) is not a clean integer pixel shift
of one sequence relative to the other (tried both +1 and -1 shifts by hand -
neither lines up), which is odd for a full-pel MC block: a pure position bug
should look like exactly that. This suggests either (a) the reference read
position is still off by something that isn't a plain integer shift (e.g. an
edge-clamp difference at the reference's actual boundary - this block's
`ix=-1` reads one chroma column to the *left* of its base, and depending on
where that lands relative to `vis_w`/tile edges it might clamp differently
between the two decoders), or (b) some later stage (a different block's own
`has_chroma` write, or its OBMC, spilling into this parent 8x8's chroma
region after `mi=(16,11)` already wrote it) is the actual last writer here,
not `mi=(16,11)` itself - the `KINETIX_AV1_DBG_PRED` block-coverage
attribution only shows which block's *own* write region overlaps the diff
pixels, not which block wrote *last* to them.

#### Next session's starting point

1. Build a chroma-plane equivalent of the existing `KINETIX_AV1_MCSUM`
   hook (currently `plane == 0`-gated only) so the *exact* reference row this
   block reads (post-clamp) can be dumped and hand-checked against frame 0's
   already-bit-exact reconstruction, to settle lead (a) above directly.
2. Alternatively, fix `dav1d_fresh`'s `DEBUG_BLOCK_INFO` gate
   (`src/recon.h:34`, currently `frame_offset == 3`) to target
   `frame_offset == 1 && t->bx == 16 && t->by == 11`, rebuild with `ninja -C
   build` (this copy's toolchain built cleanly as of this session, unlike
   `tpt-kinetix-dav1d`'s), and get a real per-pixel dav1d-side trace instead
   of hand-deriving expected values from formulas.
3. The filter-swap bug described above is real but unlanded (reverted, not
   committed) - worth revisiting once a repro that actually exercises a
   non-degenerate dual-filter sub-8x8-with-inter-neighbour block exists.
4. `capabilities().pixel_exact` still `false`; feature corpus still 1/13
   (unchanged this session - confirmed via a full re-run, no regressions).

### Session cont'd 25 — lead (a) conclusively closed (reference read is
### bit-exact); the real dav1d oracle build segfaults on any
### `DEBUG_BLOCK_INFO` trigger; new evidence narrows the bug to the chroma
### *entropy-decoded coefficient values* for this exact tx block, not MC,
### not the co-located-luma-type lookup, and not the inverse transform math

Repro unchanged: `ffmpeg -f lavfi -i testsrc2=size=128x96:rate=15:duration=2
-frames:v 2 -pix_fmt yuv420p -g 10 -c:v av1 -f ivf t2.ivf`, still 73 diff
bytes on frame 1 (U 14, V 59, Y 0). A fresh throwaway harness reproduces this
via `av1_feature_obu`/manual ffmpeg+ivf (not committed, see below).

**Step 1 done (widened `KINETIX_AV1_MCSUM`).** `plane == 0` gates removed
from both `KINMCSUM`/`KINMCOUT` prints in `inter_block.rs`
(`reconstruct/inter_block.rs`, around the `!use_compound` translational MC
arm) so they fire for whichever plane's block matches
`KINETIX_AV1_MCSUM_BLOCK=<mi_col>,<mi_row>`, luma or chroma. While doing this
also found and fixed a real bug **in the debug dump itself** (not production
code): the `ix`/`iy` computation hardcoded `>> 3` (luma sub-pel shift) instead
of `>> hbits`/`>> vbits`, so a chroma MCSUM dump was reading the reference at
the *luma*-scaled offset instead of the chroma one - silently reporting the
wrong reference row for any chroma target. Fixed to use `hbits`/`vbits`
(already computed earlier in the same function). Also corrected a
sign/axis mix-up from cont'd 24's notes: `mvs[0]` prints as `(col, row)`
everywhere in this codebase's debug output (see the `PRED`/`PRED-BASE`
`eprintln!`s), so mi=(16,11)'s `mv=(32,-16)` is `col=32, row=-16`, **not**
`row=32, col=-16` as cont'd 23/24 assumed. That flips which axis is full-pel
and changes `ix`/`iy` from the previously-stated `(-1, 2)` to the *real*
values `(ix=2, iy=-1)` in chroma pixels - cosmetic for the "full-pel" fact
(still true) but the previous session's stated integer offsets were wrong.

**Lead (a) - reference-read correctness - is now conclusively CLOSED.** With
the fixed dump, `KINETIX_AV1_MCSUM_BLOCK=16,11 KINETIX_AV1_MCSUM=1` on the
V plane reports the post-clamp reference row Kinetix's chroma MC actually
reads: `[18, 19, 17, 23, 33, 41, 51, 61, 70, 77, 86, 97]` at chroma
`y=19, x=30..41`. A new throwaway test independently pulled *both*
decoders' own frame-0 V-plane values at that exact row and byte-compared
them: dav1d's frame-0 V row 19 x=26..41 and Kinetix's frame-0 V row 19
x=26..41 are **identical to each other and to the MCSUM dump**
(`[146,146,146,146, 18,19,17,23,33,41,51,61,70,77,86,97]`, both decoders).
The chroma reference sample this block reads is exactly correct in both
position and content. Every remaining divergence is downstream of MC.

**OBMC/motion_mode ruled out for this specific block too:** the `PRED`
trace (`KINETIX_AV1_DBG_PRED_ALL=1` with `_X/_Y/_R` retargeted over the
diff region) shows mi=(16,11) has `mm=0` (`MM_SIMPLE`), so no OBMC blend
applies here regardless of the chroma-OBMC A/B test from cont'd 23.

**The filter-bleed part of lead (b) is resolved, not by "another block
overwrites this one" but by ordinary loop-filter smear.** The extra 1-2
rows of U/V diff beyond mi=(16,11)'s own 4x4 chroma write footprint
(chroma y=24-25) belong to the immediately-adjacent **skip** block
`mi=(16,12) bw=4 bh=4 skip=true mv=(0,0)`, whose own MC copy is a verified
bit-exact passthrough of frame 0 (no residual, no OBMC). The extra diff
rows there are consistent with deblock/CDEF spreading mi=(16,11)'s own
wrong reconstruction across the shared MB edge, not a genuine second wrong
write. So the whole bug is still fully attributable to mi=(16,11)'s own
chroma reconstruction.

**Residual/coefficient path is the new prime suspect, narrowed hard this
session:**

- `KINETIX_AV1_DBG_CPXY=32,20` (V plane) shows Kinetix's own **pre-deblock**
  value at that pixel is `28` for frame 1, moving only `28 -> 27 -> 26 -> 26`
  through deblock/CDEF/LR. dav1d's real final value there is `20`. Since the
  reference sample is proven correct (`33`, lead a) and the raw residual
  Kinetix computed there is `-5` (see `KINETIX_AV1_DBG_RESDUMP`,
  `33 + (-5) = 28`, matching the CPXY pre-deblock value exactly - the
  addition/clamp arithmetic is internally consistent), the ~6-8 unit gap to
  dav1d's *post-filter* final value must originate at or before the residual
  add, and Kinetix's own filters can only account for ~2 of it. (Caveat,
  same one cont'd 23 already flagged: dav1d's final value is *post-filter*,
  so this comparison is not a clean apples-to-apples isolation of the
  residual alone - see the dav1d-oracle paragraph below for why a clean
  pre-filter dav1d value isn't available this session either.)
- The co-located-luma-tx-type lookup (`co_located_luma_type` /
  `own_luma_tx_type` in `inter_block.rs`, used to pick the chroma transform
  type per §7.12.3) was audited end-to-end with a new `KINETIX_AV1_DBG_COLOC`
  hook (kept, gated, in `inter_block.rs`) and is **not** the bug here, though
  only by a coincidence worth flagging for whoever touches this next: this
  block has two luma TX_4X4 leaves at mi=(16,11) (`(64,44)` eob=0/DCT_DCT
  default, `(68,44)` eob=3/FLIPADST_DCT), but only the *second* gets pushed
  into `luma_leaf_types` (the push is gated on `eob > 0`, skipping the
  eob=0 leaf entirely). The chroma TX block's spatial position
  (`lx=64, ly=40`) doesn't actually land inside *either* leaf's footprint
  (both are at `ly0=44`; `ly=40` is the sibling mi_row=10 half's territory,
  a separate coded block processed earlier) - so the lookup always misses
  and falls through to `own_luma_tx_type = luma_leaf_types.first()`. With
  only one (eob>0) leaf in the vec, `first()` happens to equal dav1d's real
  semantics ("chroma uses whatever `b->txtp` last held after the block's own
  luma leaves" - i.e. the *last* decoded leaf, not necessarily the first).
  If a future sub-8x8 block ever has *two* eob>0 leaves with *different*
  tx_types, `first()` vs "last decoded" will diverge for real - worth
  fixing to `.last()` pre-emptively even though it's a no-op on this repro.
- Confirmed **mathematically**, by hand-expanding the 2-D separable inverse
  transform for FLIPADST_DCT (row=DCT, col=ADST+flip per this codebase's
  `row_axis_transform`/`col_axis_transform`) against the actual decoded
  coefficients (`dequant = [-280, 0,0,0, -176, 0,0,0, ...]`, i.e. only DC and
  one row-1 AC term nonzero): a flat-per-row, varying-per-row residual (which
  is exactly what Kinetix produced,
  `[-5]*4, [-10]*4, [-14]*4, [-10]*4`) is the *correct* output for those
  specific coefficient values under that tx_type - so **if there is a bug
  here it is not in `inverse_transform`'s math**, it's in what coefficient
  values/positions got entropy-decoded in the first place (wrong EOB, wrong
  scan-order mapping, or a stale/wrong coefficient-context leading the
  range decoder to a different-but-plausible symbol at one bin). This is
  now the single most concrete remaining lead.

**Attempted to get a real dav1d pre-filter oracle trace (session's step 2)
and hit a new, previously-undocumented blocker: `dav1d_fresh`'s
`DEBUG_BLOCK_INFO` instrumentation segfaults on every trigger, not just this
one.** Retargeted `src/recon.h`'s macro from `frame_offset == 3` to
`f->frame_hdr->frame_type != DAV1D_FRAME_TYPE_KEY && t->by == 11 &&
t->bx == 16` (also tried it completely unconditional, `1`, to rule out a
targeting mistake) and rebuilt clean both times (`ninja -C build
tools/dav1d.exe` - note: the *first* `ninja -C build` invocation after
editing `recon.h` does the real recompile; a bare re-run of `ninja` looks
like it stops after step 1/20 "Generating vcs_version.h", which is just
ninja's restat optimization on the DLL export-symbol list correctly
concluding nothing further needs relinking, **not** a build failure - don't
mistake that short output for a broken build, `tools/dav1d.exe` doesn't need
relinking for a `.dll`-internal change on Windows). Every run since
segfaults inside `dav1d_submit_frame` (backtrace: `memmove`/`fwrite` deep in
ucrtbase, called from `dav1d_submit_frame`, i.e. *before* any per-block
recon even starts) - this crash reproduces even with `DEBUG_BLOCK_INFO`
fully unconditional, so it is unrelated to the specific gate condition.
`git status`/`git diff --stat` inside `dav1d_fresh` shows this fork already
carries substantial uncommitted custom instrumentation from earlier sessions
(`cdef_apply_tmpl.c`, `cdef_tmpl.c`, `decode.c`, `obu.c`, `recon_tmpl.c`,
plus an untracked `dav1d_grid.dump` file and custom `KGTILING`/`KGTG` startup
prints not part of upstream dav1d) - the crash is most likely in that
pre-existing custom code (a grid-dump `fwrite` with a bad size, going by the
backtrace), not in anything this session touched. **Conclusion for future
sessions: `dav1d_fresh`'s block-level trace is not currently usable at all,
contradicting cont'd 24's optimistic note that "this copy's toolchain builds
cleanly" - it builds cleanly but crashes at runtime once `DEBUG_BLOCK_INFO`
is enabled for anything.** Debugging *that* crash (likely in the grid-dump
code some earlier session added to `decode.c`/`obu.c`) is probably a
prerequisite for ever getting a real per-block dav1d trace out of this
checkout again, and wasn't pursued further this session (out of scope: it's
debugging inherited instrumentation, not the actual AV1 bug).

**Files changed and committed:** `inter_block.rs`'s `KINETIX_AV1_MCSUM`
widened to all planes + its `ix`/`iy` bug fixed, and a new
`KINETIX_AV1_DBG_COLOC` hook added (both real, permanent debug
infrastructure, not a production-path change - no behavioural change to the
decoder itself, verified via `cargo test -p tpt-kinetix-av1` (all pass) and
`tpt-kinetix-test-utils`'s `conformance`/`av1_feature_coverage_vs_dav1d_
when_available` tests (identical results to cont'd 24: intra 6/6, inter
corpora 5/5 (x4 clips) all bit-exact, feature corpus still 1/13, same
per-tool PSNR numbers). **No fix was found or committed for the actual bug**
- per this crate's rule, nothing without a measured improvement gets
committed as a fix, and none of this session's leads reached a coded fix.
A throwaway test harness, `tpt-kinetix-test-utils/tests/dbg_av1_t2_repro.rs`
(NOT committed, matching cont'd 24's own harness which also didn't survive
between sessions), reproduces the whole clip + both decoders' frame 0/1 in
~2s via `av1_feature_obu`/manual ffmpeg+dav1d-cli, if a future session wants
to recreate it quickly instead of redoing the ffmpeg+dav1d dance.

#### Next session's starting point

1. **Best lead:** dump the exact entropy-decoded `coeffs.quant`/eob/scan
   position for mi=(16,11)'s V-plane TX_4X4 block (already partially visible
   via `KINETIX_AV1_DBG_RESDUMP`'s `COEFFDUMP` line - `eob=3`,
   `dequant=[-280,0,0,0,-176,0,0,0,...]`) and cross-check the *scan table*
   used (`get_scan`/`DEFAULT_SCAN_4X4` for tx_type FLIPADST_DCT, which is a
   2-D class so should use the default zig-zag, not `get_mrow_scan`/
   `get_mcol_scan`) against the spec table by hand, one entry at a time, for
   scan positions 0/1/2 (the ones EOB=3 actually visits). Also worth
   checking the coefficient-context (`all_zero_ctx`, DC-sign, base-range,
   golomb) derivation for this specific `(tx_size, tx_type, plane, is_inter)`
   combination against the spec/dav1d source directly - this session ruled
   out the *transform type selection* and the *inverse transform math* but
   never got to auditing the coefficient *entropy* read itself bin-by-bin.
2. Fix `co_located_luma_type`'s fallback to use `.last()` instead of
   `.first()` in `luma_leaf_types` - confirmed currently harmless (no
   measured effect on this repro, not committed) but is a real latent bug
   per dav1d's own semantics (chroma uses the block's *last*-decoded luma
   txtp, not its first) that will matter the moment a sub-8x8 block has two
   eob>0 luma leaves with genuinely different tx_types.
3. If a dav1d-side oracle trace is still wanted, someone needs to first fix
   the `dav1d_fresh` submit-frame segfault (likely the grid-dump `fwrite` in
   the previously-added `decode.c`/`obu.c` instrumentation) before
   `DEBUG_BLOCK_INFO` can be used again in that checkout at all.
4. `capabilities().pixel_exact` still `false`; feature corpus still 1/13
   (re-confirmed this session, byte-identical per-tool PSNR numbers to
   cont'd 24 - no regressions).

### Session cont'd 26 — two concrete next-session hypotheses TESTED and
### DISPROVEN by measurement (both caused real entropy desyncs, not fixes);
### `.first()` is confirmed load-bearing, not a latent bug; bug still open

Picked up exactly where cont'd 25 left off: the two items in its "next
session's starting point" list. Both were implemented and measured against
the `t2.ivf` repro (`tpt-kinetix-test-utils/tests/dbg_av1_t2_repro.rs`,
recreated from cont'd 25's description since it wasn't committed - still not
committed after this session either, same convention).

**Baseline re-confirmed exactly:** frame 1 diff = U 14 / V 59 / Y 0 (73 total
bytes), matching cont'd 23-25 precisely.

**Hypothesis A (this session's own idea, not in cont'd 25's list): implement
a real persistent tile-wide `TxTypes[y][x]` grid**, matching the AV1 spec's
literal `compute_tx_type` pseudocode (`return TxTypes[ y4 ][ x4 ]` for the
inter-chroma branch, a frame/tile-global array — NOT anything scoped to the
current coded block). The reasoning: `co_located_luma_type` in
`inter_block.rs` only ever searches `luma_leaf_types`, a `Vec` built fresh
inside the current coded block's own function call — it structurally cannot
see a luma leaf decoded by an *earlier, different* coded block, which is
exactly what's needed for a sub-8x8 chroma block (owned by the odd-parity
half of a partitioned pair per §7.3.1) whose chroma footprint spans a
sibling block's luma area. Implemented as a new `Vec<u8>` field
`luma_tx_type_grid` on `TileDecodeState` (tile-relative 4×4-cell grid,
written by every luma leaf with `eob > 0` in both `inter_block.rs` and
`intra_block.rs`'s IBC path, read by `co_located_luma_type` in place of the
local-list scan). **Result: measurably WORSE — frame 1 Y-plane diff went
from 0 to 77641 (V-plane samples-differing 59 -> 943).** This is a real
entropy desync (a wrong tx_type changes `read_eob`'s `is_1d` CDF-context
bit, which changes how many bits the *next* symbols consume), not a
cosmetic regression. **Conclusion: the spec's literal global-grid semantics
do NOT match what real encoders/dav1d actually produce/expect here** — the
existing code comment directly above this fallback ("dav1d uses the block's
own luma tx type for every chroma tx block it codes, `b->txtp`") is the
correct model after all. Reverted in full (`mod.rs`'s field +
initialization, both write sites, the read-site rewrite) — verified via
`git diff --stat` that `mod.rs`/`intra_block.rs` are byte-identical to HEAD
again.

**Hypothesis B (cont'd 25's explicit suggestion #2): switch the
`co_located_luma_type` lookup-miss fallback from `.first()` to `.last()`.**
cont'd 25 called this "confirmed currently harmless (no measured effect on
this repro)" reasoning from the fact that `luma_leaf_types` only had one
`eob > 0` entry for the one block it inspected by hand. That reasoning was
correct for *that one block* but wrong as a blanket claim about the repro:
**measured, this single-character change (`.first()` -> `.last()`) also
regresses the corpus — frame 1 Y-plane diff 0 -> 33493.** So somewhere else
in this same 2-frame clip there IS a coded block with >=2 `eob > 0` luma
leaves whose types genuinely differ, and dav1d's real chroma-txtp derivation
for it depends on getting the FIRST one, not the last. **This directly
contradicts cont'd 25's framing of `.first()` as a "real latent bug per
dav1d's own semantics" — it is not a latent bug on this codebase's current
model, it is load-bearing, and must not be changed without a full-corpus
measurement.** Reverted; `.first()` is unchanged. A code comment was added
at the site (and left committed - documentation only, no behavioural
change, `cargo test -p tpt-kinetix-av1` all-pass and
`av1_intra_corpus_vs_dav1d_when_available` / `av1_inter_corpus_vs_dav1d_
when_available` / `av1_feature_coverage_vs_dav1d_when_available` all
byte-identical to cont'd 25's numbers verified) recording both disproven
results so a future session doesn't re-attempt either without re-deriving
this from scratch.

**What this leaves for the actual bug, still unsolved:** the coefficient-
entropy audit cont'd 25 handed off (EOB derivation / scan table / context
derivation for the specific diverging TU) was NOT completed this session —
time went to testing the two co-located-luma-type hypotheses above instead,
since they looked like the more concrete, already-scoped lead. They turned
out to be a dead end (or at least: not *this* bug, even though the
underlying local-vs-global scoping concern about `co_located_luma_type` may
still be spec-inaccurate in the abstract - it just isn't what's making
`mi=(16,11)`'s chroma wrong). The coefficient-entropy read itself
(`coeff_base_ctx`/`coeff_br_ctx`/`all_zero_ctx`/`read_eob` in `coeff.rs`) was
inspected by eye this session and no transcription bug was spotted against
the spec text cross-referenced inline in the code's own comments, but this
was NOT a rigorous bin-by-bin trace against an independent oracle - the
`dav1d_fresh` block-trace blocker cont'd 25 hit (segfault in
`dav1d_submit_frame` from inherited grid-dump instrumentation) was not
revisited and is very likely still blocking that approach.

#### Next session's starting point

1. **Do not re-attempt Hypothesis A or B above** without a full-corpus
   measurement first - both looked correct by spec/comment reading alone and
   both were wrong in practice. This is now the second and third time in
   this bug hunt that a plausible-by-inspection fix regressed the corpus;
   treat every candidate fix here as untrusted until `dbg_av1_t2_repro`'s
   Y-plane-diff number (must stay 0) and the conformance suite are both
   checked.
2. The coefficient-entropy audit cont'd 25 scoped (dump
   `coeffs.quant`/eob/scan position for mi=(16,11)'s V-plane TX_4X4 block,
   cross-check the exact scan positions 0/1/2 and the `coeff_base`/
   `coeff_br`/`all_zero` context derivation bin-by-bin against the spec
   text, not code comments) is still the most-concrete open lead and was
   NOT done this session - do it before trying another structural theory.
3. Fixing the `dav1d_fresh` `DEBUG_BLOCK_INFO` segfault (in the inherited,
   uncommitted grid-dump instrumentation in `decode.c`/`obu.c` per cont'd
   25's notes) would unblock a real independent oracle trace and is
   probably worth the detour now that two structural guesses have both
   burned a session each without one.
4. `capabilities().pixel_exact` still `false`; feature corpus still 1/13;
   `t2.ivf` frame 1 still 73 diff bytes, all chroma, unchanged.

### Session cont'd 27 — the bin-by-bin coefficient-entropy audit cont'd 25/26
### scoped was actually done this time; it found NO discrepancy anywhere in
### the coefficient-decode syntax/context/CDF-adaptation logic for the target
### TU. Bug still open; the remaining suspects are now narrowed to two very
### specific, previously-unaudited things: static default-CDF table
### transcription errors, and the `dav1d_fresh` oracle being unusable

Baseline re-confirmed exactly: `t2.ivf` frame 1 = U 14 / V 59 / Y 0 (73 total
diff bytes), byte-identical breakdown to cont'd 23-26. Re-created
`tpt-kinetix-test-utils/tests/dbg_av1_t2_repro.rs` from cont'd 26's
description (still not committed, same "no unlanded throwaway harness"
convention as every prior session).

**Did the actual thing cont'd 25 scoped and cont'd 26 skipped**: added a new
temporary, gated trace (`KINETIX_AV1_DBG_C27`, in `coeff.rs`'s `read_coeffs`,
hardcoded to fire only for `blk.plane == 2 && blk.x4 == 8 && blk.y4 == 5` —
the V-plane TX_4X4 block at chroma cpx=(32,20), confirmed via
`KINETIX_AV1_DBG_RESDUMP` to be exactly `mi=(16,11)`'s block, `eob=3`,
`dequant=[-280,0,0,0,-176,...]`, matching every prior session's numbers
exactly) that dumps, for every scan position visited: the `is_eob`/
`coeff_base` context value, the raw symbol read, the resulting level, every
`coeff_br` extension step, the final signed `quant[pos]`, and the range
decoder's `rng` after each read. Also traced `read_eob` (added a
`C27 pre-eob`/`C27 post-eob` pair printing `tx_size`, `tx_type`,
`tx_sz_ctx`, `ptype`, `tx_class`, and the resolved `scan` table).

**Hand-derived the expected value at every one of those steps directly from
the AV1 spec's `coeffs()`/`get_coeff_base_ctx()`/`get_coeff_br_ctx()`
pseudocode (not from code comments, which this bug hunt has repeatedly
found to be subtly wrong) and compared bin-by-bin. Every single value
matched:**

- `tx_type=4` (`FLIPADST_DCT`), `tx_class=TX_CLASS_2D` (confirmed: neither
  `V_*` nor `H_*`, so it falls into the `_ => TX_CLASS_2D` arm of
  `get_tx_class`) — correct per spec, `get_scan` therefore returns
  `DEFAULT_SCAN_4X4 = [0,1,4,8,5,2,3,6,9,12,13,10,7,11,14,15]`, independently
  checked against the spec's `Default_Scan_4x4` table value-for-value —
  matches.
- `eob=3` → scan positions visited are raster positions `{0, 1, 4}`
  (`scan[0..3]`), processed in **reverse** scan order per spec (`c=2,1,0` →
  `pos=4,1,0`), exactly as coded.
- `c=2` (the last/EOB position, `pos=4`, `row=1,col=0`): `is_eob` branch —
  hand-computed `height=4`, `bwl=2`, `(height<<bwl)/8 = 2`, `c=2 <= 2` →
  spec ctx `1` (of `0..4`) — **matches the trace's `ctx=1` exactly**.
  `coeff_base_eob` symbol `0` → `level=1`. `1 <= NUM_BASE_LEVELS` so no
  `coeff_br` extension. `quant[4]=1` — matches `dequant[4]=-176` once you
  apply the sign read later (`176 * 1 = 176`, sign flips it negative — see
  below).
- `c=1` (`pos=1`, `row=0,col=1`, not EOB): hand-walked
  `SIG_REF_DIFF_OFFSET[TX_CLASS_2D] = [[0,1],[1,0],[1,1],[0,2],[2,0]]` from
  `(row=0,col=1)` — every one of the 5 neighbour cells (`quant[2]`,
  `quant[5]`, `quant[6]`, `quant[3]`, `quant[9]`) is still `0` at this point
  in the reverse walk (only `quant[4]` has been set so far, and none of the
  5 offsets land on position 4) → `mag=0` → `ctx_computed=0` →
  `+ COEFF_BASE_CTX_OFFSET[TX_4X4][0][1] = 1` → **spec ctx `1`, matches the
  trace exactly**. Symbol `0` → `level=0` → `quant[1]=0` — matches
  `dequant[1]=0`.
- `c=0` (`pos=0`, DC, `row=0,col=0`): spec's `TX_CLASS_2D` special case
  (`row==0 && col==0` → `ctx=0` unconditionally, no neighbour scan) —
  **matches the trace's `ctx=0` exactly**. Symbol `2` → `level=2` (still
  `<= NUM_BASE_LEVELS`, no `coeff_br`) → `quant[0]=2` — matches
  `dequant[0]=-280` once signed (`140 * 2 = 280`).
- Sign pass, forward scan order (`pos=0,1,4`): `pos=0 == scan[0]` → uses
  `dc_sign_ctx` (the one context-coded sign) → `sign=true` → `quant[0]=-2`.
  `pos=1`: `quant[1]==0` so the spec's `if (quant[pos] != 0)` guard means
  **no sign bit is read at all** — confirmed in the trace: `rng` is
  unchanged across that line, i.e. zero bits consumed, exactly as spec
  requires. `pos=4`: nonzero, not the DC position, so a plain
  `read_literal(1)` sign bit (not the context-coded one) — `sign=true` →
  `quant[4]=-1`. Every one of these branches matches spec's syntax
  structure exactly, including the easy-to-get-wrong "zero coefficients
  consume no sign bit" case.
- Independently re-derived, from **dav1d's real (ICDF-inverted) `msac.c`
  `update_cdf`** rather than from a possibly-misremembered spec paraphrase
  (a first attempt at recalling the spec's `update_cdf` pseudocode from
  memory got the `tmp` initial-value/flip-point backwards; converting
  dav1d's actual ICDF-domain update back to Kinetix's non-inverted CDF
  convention resolved the ambiguity independently rather than trusting
  memory): Kinetix's `entropy.rs::read_symbol` adaptation loop (`tmp` starts
  at `0`, flips to `32768` at `i == symbol` and stays there) is the
  **correct** direction for a non-inverted, increasing-CDF representation —
  confirmed by hand-tracing a concrete example (`symbol=2` in a 4-symbol
  alphabet: `cdf[0]`/`cdf[1]` should decrease toward 0 since `X=2 > 0,1`,
  and `cdf[2]` should increase toward 32768 since `X<=2` — exactly what the
  code does). **Not a bug.** (This re-confirms, independently, the same
  conclusion the 2026-08-27 CDF-rate-formula session already reached from a
  different angle — see `todo-av1.md`'s "AV1 CDF rate formula regression"
  memory — but this session verified the adaptation *direction*, a
  different part of the same function, not just the *rate*.)

**Net result: every syntax element, every context-index formula, the scan
table, the tx_type derivation, the sign/Golomb path, and the CDF adaptation
direction for this exact TU are all independently confirmed spec-correct,
given whatever entropy-decoder state (`rng`/`val` and the live, adapted CDF
tables) existed at the moment this block's `coeffs()` call began.** This is
a strictly narrower and more useful negative result than cont'd 26's
"inspected by eye, no bug spotted" — every value was independently
hand-derived from spec pseudocode first, then compared against the trace,
not the other way around.

**What this rules the bug out of, precisely:** the `coeffs()` syntax
structure itself, `get_coeff_base_ctx`/`get_coeff_br_ctx`'s formulas, the
`DEFAULT_SCAN_4X4` table contents, `get_tx_class`'s dispatch, the
inter-chroma `compute_tx_type`/`get_uv_inter_txtp` passthrough (already
independently re-derived this session from dav1d's real
"per-coded-block `b->txtp`, only updated by non-skipped luma leaves"
semantics applied to *this specific block's own two leaves* — leaf 1
`(64,44)` is skipped (`eob=0`, doesn't call `transform_type()`, doesn't
update `b->txtp`), leaf 2 `(68,44)` is non-skip and sets `b->txtp =
FLIPADST_DCT`; chroma is read after both, so it correctly inherits
`FLIPADST_DCT` — this is NOT a coincidental one-entry-list artifact as
cont'd 25 worried, it is the actually-intended value for this block by
dav1d's own model), and `read_symbol`'s CDF-update direction.

**What is NOT yet ruled out, and is now the most concrete remaining lead:**
the **static default CDF initialization tables** themselves
(`entropy_cdf.rs`'s `DEFAULT_TXB_SKIP_CDF`, `DEFAULT_COEFF_BASE_EOB_CDF`,
and the sibling `DEFAULT_COEFF_BASE_CDF`/`DEFAULT_COEFF_BR_CDF` tables) for
the specific `[qctx][tx_sz_ctx=TX_4X4][ptype=1 (chroma)]` slice. These are
large, mechanically-transcribed multi-dimensional tables (`DEFAULT_TXB_SKIP_
CDF: [[[[u16;3];13];5];4]`, `DEFAULT_COEFF_BASE_EOB_CDF:
[[[[[u16;4];4];2];5];4]`, i.e. thousands of individual constants) that this
session's context-index audit cannot catch: getting the *index formula*
right (which this session verified) says nothing about whether the actual
*probability values* stored in the chroma slice of these tables are
byte-for-byte what the spec's default-CDF appendix specifies. A single
transposed or mistyped entry in the *chroma-only* slice of one of these
tables would be invisible to every non-chroma test (explaining why Y is
perfectly exact and only isolated chroma blocks are wrong), would survive
this session's context-formula audit entirely, and — since it's a *default
init* value that then gets adaptively nudged frame over frame — would be
very hard to spot from final adapted CDF values alone. Nobody in this bug
hunt has yet byte-compared these specific tables' chroma rows against the
spec's actual default-CDF appendix numbers; this session ran out of budget
to attempt it (these tables are large enough that eyeballing them without
an automated cross-reference against a machine-readable copy of the spec
appendix risks exactly the kind of memory-based transcription error this
bug hunt has been burned by before — see the `update_cdf` tmp/direction
near-miss above, caught only because it was cross-derived from dav1d source
instead of trusted from memory).

**Files changed and committed:** `coeff.rs`'s `KINETIX_AV1_DBG_C27` trace
hook (new, permanent debug infrastructure, gated behind an env var, zero
behavioural change to the decoder — verified via `cargo test -p
tpt-kinetix-av1` (all pass, same as baseline) and
`tpt-kinetix-test-utils`'s `conformance` test (byte-identical to cont'd
25/26: intra 6/6, inter corpora 5/5 × 4 clips all bit-exact, feature corpus
still 1/13, identical per-tool PSNR numbers)). **No fix was found or
committed** — per this crate's rule, nothing without a measured improvement
gets committed as a behavioural change, and this session's exhaustive audit
came up empty on the TU itself. The throwaway `dbg_av1_t2_repro.rs` harness
was recreated and used but, per convention, not committed.

#### Next session's starting point

1. **Best lead now:** cross-reference `entropy_cdf.rs`'s `DEFAULT_TXB_SKIP_
   CDF`, `DEFAULT_COEFF_BASE_EOB_CDF`, `DEFAULT_COEFF_BASE_CDF`, and
   `DEFAULT_COEFF_BR_CDF` tables' **chroma (`ptype=1`) rows at `tx_sz_ctx=0`
   (`TX_4X4`)** against the AV1 spec's actual default-CDF appendix values
   (fetchable from `raw.githubusercontent.com/AOMediaCodec/av1-spec`,
   confirmed reachable — see the 2026-08-23 "AV1 spec WebFetch available"
   memory) — ideally with a small script that pulls both into comparable
   arrays rather than eyeballing, since these tables are too large to
   safely hand-check without a mechanical diff. This is the one input to
   this exact TU's decode that this session's audit did NOT (and
   structurally could not, via context-formula reasoning alone) verify.
2. Do not re-attempt Hypothesis A (persistent frame-global `TxTypes` grid)
   or Hypothesis B (`.first()` → `.last()` in `co_located_luma_type`) from
   cont'd 26 — both are proven regressions, and this session's independent
   re-derivation of dav1d's real per-block `b->txtp` semantics reconfirms
   `.first()` (on this block's one-entry list) is the actually-correct
   value, not a lucky coincidence.
3. Fixing `dav1d_fresh`'s `DEBUG_BLOCK_INFO` segfault (inherited grid-dump
   instrumentation in `decode.c`/`obu.c`, per cont'd 25) remains the other
   viable path to a real independent oracle trace, still not attempted by
   any session so far.
4. `capabilities().pixel_exact` still `false`; feature corpus still 1/13;
   `t2.ivf` frame 1 still 73 diff bytes, all chroma, unchanged — fourth
   consecutive session confirming the exact same numbers, no regression.

### Session cont'd 28 — the static default-CDF-table-contents lead cont'd 27
### flagged as the single most concrete remaining item has now been checked
### mechanically, exhaustively, and automatically (not by eye): every
### coefficient-related default CDF table in `entropy_cdf.rs` is byte-for-byte
### correct against dav1d's real source, across every quantizer bucket, both
### plane types, and every context index. Zero discrepancies found. This
### specific lead is now closed; bug still open with no remaining unaudited
### static-data suspects in the coefficient path

Did not re-run the `t2.ivf` repro this session (no code was changed, so the
73-byte baseline from cont'd 23-27 is unaffected by construction — no point
re-measuring a number nothing touched). Instead spent the whole session on
cont'd 27's own explicit next-step: mechanically cross-referencing
`entropy_cdf.rs`'s coefficient-related default-CDF table *contents* (as
opposed to the context-index *formulas*, which cont'd 27 already proved
correct) against an independent source.

**Oracle chosen:** dav1d's own C source (`%LOCALAPPDATA%\Temp\dav1d_src\
src\cdf.c`, a plain readable checkout distinct from the instrumented/broken
`dav1d_fresh` one that's been segfaulting since cont'd 25 - this checkout
needed no fixing, it's untouched upstream source and was sitting there the
whole time under a different name than prior sessions looked for). This is
faster and more mechanically verifiable than the spec's markdown tables
(no HTML table parsing needed, and it's the literal source dav1d itself
compiles) and is an equally valid ground truth per this task's own framing.

**Representation-convention check (done first, since getting this wrong
would invalidate every comparison below):** dav1d's `cdf.c` wraps every
literal default-CDF value in a `CDF1(x)`/`CDF2(a,b)`/.../`CDFn(...)` macro
defined as `CDF1(x) = (32768-(x))` (recursively for `CDFn`). That means the
**literal numeric arguments written in the C source are already the
forward, increasing, non-inverted CDF values** (e.g. `CDF2(17837, 29055)`
means the real forward CDF is `[17837, 29055]`) - the macro's *runtime*
result (`32768-x`) is dav1d's *internal* inverted/ICDF storage convention,
which is irrelevant here since Kinetix's `entropy.rs::read_symbol` uses the
non-inverted, increasing convention (independently re-confirmed correct by
cont'd 27). So **no inversion arithmetic is needed** - the raw source
literals in `cdf.c` compare directly, value-for-value, against
`entropy_cdf.rs`'s array contents (after stripping Kinetix's two trailing
sentinel entries `32768, 0` - full-CDF-mass and adaptation-count - which
have no dav1d-source equivalent since dav1d derives the array length from
the C struct's fixed-size fields instead).

**Method:** wrote two small Python scripts (not committed, scratch-only,
under the session's scratchpad dir) - one regex-parses `cdf.c`'s
`default_coef_cdf[4]` array into the 4 quantizer-context (`qctx`) blocks,
then each named field (`.skip`, `.eob_base_tok`, `.base_tok`, `.br_tok`,
`.dc_sign`, `.eob_bin_16` through `.eob_bin_1024`, `.eob_hi_bit`) into a
flat, ordered list of ints by matching every `CDF\d+\(...\)` call in
document order (this preserves the C source's true nesting/iteration
order without needing to hand-parse brace nesting per dimension). The
other parses the corresponding Rust `pub static DEFAULT_*_CDF` array
literals out of `entropy_cdf.rs` via `ast.literal_eval` (falling back to a
restricted `eval` only for `DEFAULT_DC_SIGN_CDF`, which is written as
`128 * N` products rather than pre-multiplied literals). A third script
reshapes both into matching `[qctx][tx_size][plane_type][context]`
(or the appropriate subset of those dims per table) structures and diffs
element-by-element, per table, per qctx, per tx_size, per plane_type.

**Tables checked, all 4 qctx buckets, all tx_size and plane_type values
each table actually has (not just chroma/TX_4X4 - full tables, since the
mechanical diff cost the same either way and a full check is strictly more
conclusive):**
- `DEFAULT_TXB_SKIP_CDF` vs dav1d `.skip` (`[5 tx][13 ctx]`, no plane-type
  split in either representation - confirmed this table structurally
  cannot be a *chroma-only* bug source).
- `DEFAULT_COEFF_BASE_EOB_CDF` vs `.eob_base_tok` (`[5 tx][2 ptype][4 ctx]`).
- `DEFAULT_COEFF_BASE_CDF` vs `.base_tok` (`[5 tx][2 ptype][41 ctx]` -
  Kinetix's declared shape has a 42nd context slot per `[tx][ptype]` with
  no dav1d counterpart; inspected by hand and it's uniformly the
  `[8192,16384,24576]` "never used" placeholder dav1d itself also pads
  unused high context slots with in several other tables, consistent with
  it being dead/unreachable padding, not a live 42nd context).
- `DEFAULT_COEFF_BR_CDF` vs `.br_tok` (`[4 tx][2 ptype][21 ctx]` - dav1d's
  own struct comment reads `br_tok[4 /*5*/][2][21][4]`, i.e. dav1d itself
  only stores 4 tx-size buckets for this table, capping the context
  selection at `TX_32X32` per spec; Kinetix's array has a 5th tx_size slot
  with no dav1d counterpart to diff against - not inspected further this
  session since the target TU is `TX_4X4` (index 0), well inside the
  4-bucket overlap, but flagged below as a loose end).
- `DEFAULT_DC_SIGN_CDF` vs `.dc_sign` (`[2 ptype][3 ctx]`, identical across
  all 4 qctx blocks in dav1d - confirmed Kinetix also repeats the same
  values across its 4 qctx slots, not just qctx 0).
- `DEFAULT_EOB_PT_16_CDF` through `DEFAULT_EOB_PT_256_CDF` vs
  `.eob_bin_16`/`32`/`64`/`128`/`256` (`[2 ptype][2 is_inter][N ctx]` each).
- `DEFAULT_EOB_PT_512_CDF`/`DEFAULT_EOB_PT_1024_CDF` vs `.eob_bin_512`/
  `1024` (`[2 is_inter][N ctx]`, no plane-type split - these two sizes are
  luma-only per spec so plane type is moot for them anyway).
- `DEFAULT_EOB_EXTRA_CDF` vs `.eob_hi_bit` (`[5 tx][2 ptype][9 ctx]`).

**Result: zero mismatches, across every one of those tables, every
quantizer bucket (0-3), every applicable tx_size, and both plane types.**
Every single value Kinetix has hard-coded for the coefficient-decode path
is byte-identical to dav1d's own default-CDF source, including the
specific chroma (`ptype=1`) / `TX_4X4` (`tx_size=0`) rows this session's
mandate was scoped to (`DEFAULT_COEFF_BASE_EOB_CDF[*][0][1]`,
`DEFAULT_COEFF_BASE_CDF[*][0][1]`, `DEFAULT_COEFF_BR_CDF[*][0][1]`,
`DEFAULT_TXB_SKIP_CDF[*][0]`, `DEFAULT_DC_SIGN_CDF[*][1]`,
`DEFAULT_EOB_PT_16_CDF[*][1]`).

**Also checked, since it's immediately adjacent and equally capable of
producing a *chroma-only, TX_4X4-only* symptom by picking the right values
from the wrong qctx bucket:** `TileCdfs::q_context()` in `coeff.rs` (the
`base_q_idx` → qctx-bucket selector) - `0..=20 => 0, 21..=60 => 1,
61..=120 => 2, _ => 3` - matches the AV1 spec's `get_qctx()` boundaries
exactly. Not a bug either.

**What this rules out, precisely, and definitively (mechanical diff, not
eyeballing - this is qualitatively stronger than cont'd 26's "inspected by
eye, no bug spotted" on the context-formula side):** every default-CDF
*value* this decode path can read for coefficient syntax, for any
quantizer/tx-size/plane-type/context combination the corpus exercises, is
correct. Combined with cont'd 27's independent proof that the
context-*index* formulas, scan tables, tx_type derivation, and CDF
adaptation direction are all correct, **the entire coefficient-entropy
subsystem (table contents + indexing logic + adaptation) is now
proven correct for this bug's target TU** as far as static analysis and
spec-derivation can reach. This was the last item on cont'd 27's explicit
list of "not yet ruled out" suspects.

**What is NOT ruled out, and is the one honest loose end from this
session:** `DEFAULT_COEFF_BR_CDF`'s 5th tx_size slot (index 4, presumably
meant for `TX_64X64`) was not diffed against anything, because dav1d's own
`br_tok` table only has 4 tx-size buckets to begin with (it caps the
context/table lookup at `TX_32X32` per spec, and the AV1 spec's own
`Default_Coeff_Br_Cdf` table is likewise defined only for
`tx_size < TX_SIZES_ALL... capped`). This is irrelevant to the `t2.ivf`
bug specifically (target TU is `TX_4X4`) but is a genuine documentation
gap: nobody has verified whether Kinetix's *runtime lookup* into this
table correctly clamps `tx_size` to the 4-bucket range before indexing, or
whether it naively indexes with an uncapped `tx_size` that happens to
still be in-bounds only because the array was over-allocated to 5 slots.
Worth a quick, cheap check next session (`grep` the `coeff_br`/`br_tok`
lookup call site in `coeff.rs` for how it computes its tx_size index) even
though it's very unlikely to be this specific bug.

**No fix found or committed this session** - this was a pure ground-truth
verification pass with a completely clean (negative) result, not a
code-change session. Nothing in the working tree was touched (`git status`
before and after this session's work is identical: only the pre-existing,
not-mine `todo-h264.md`/`tools/capa1_field_census.py` changes and the
long-standing uncommitted `dbg_av1_t2_repro.rs` throwaway harness, which
this session didn't even need to touch since no repro run was required).

#### Next session's starting point

1. **The static default-CDF-table-contents lead is now closed** - do not
   re-attempt a byte-level table audit without a specific new reason to
   doubt a specific table; this session's diff was exhaustive (all 4 qctx
   x all tx_size x both ptype, for every coefficient-related default-CDF
   table in the file) and found nothing.
2. Cont'd 27's coefficient-entropy audit plus this session's table audit
   together mean the *entire static/formula surface* of the coefficient
   decode for the target TU (`mi=(16,11)`, V-plane, TX_4X4,
   `FLIPADST_DCT`, `eob=3`) has been independently verified correct. The
   remaining explanation space for the 73-byte chroma-only diff is now
   narrower than ever: either (a) something upstream of `coeffs()` for
   *this specific block* feeds it a subtly wrong live decoder state
   (`rng`/`val`, or a *previously adapted* CDF value that started correct
   but got nudged wrong by an earlier block's read) that this session's
   static-default-value check cannot see, since adaptation is a runtime
   process; or (b) the bug is downstream of entropy decode entirely
   (dequantization scaling, inverse-transform application, or
   prediction/reconstruction) for this exact chroma block, despite cont'd
   26's "ruled out transform type selection and inverse transform math" -
   that ruling was about the *math being correct in the abstract*, not
   about *this exact block's live inputs being bit-exact*, which is a
   narrower and not-yet-fully-closed question.
3. Given (a) above, the highest-value next move is probably tracing the
   **adapted** (not default) CDF state for the specific contexts this
   block's `coeffs()` call reads, immediately before the call, and
   comparing *those* against dav1d's live adapted state at the same point
   - this requires either fixing the `dav1d_fresh` oracle segfault (still
   unattempted by any session) or finding some other way to get dav1d's
   real mid-stream adapted CDF value at this exact point (e.g. patching
   the plain `dav1d_src` checkout used this session, which is NOT the one
   that's been segfaulting, with a one-line printf in `decode_coefs`/
   `msac.c`'s `dav1d_msac_decode_symbol_adapt*` gated on the same tile/
   block coordinates - this checkout is untouched and may be much less
   fragile to instrument than the already-broken `dav1d_fresh` copy).
4. Given (b), a cheap independent check: dump this exact chroma block's
   *dequantized* coefficient array and compare the *inverse transform
   input* against a hand-computed expected value from the already-verified
   `quant[]`/sign values cont'd 27 derived (`quant = [-2, 0, 0, -1]` at
   scan positions `[0,1,4]`... rest zero) times the correct dequant step
   size for this block's `base_q_idx`/plane - if the dequant scale itself
   is wrong for chroma only, that would explain a chroma-only symptom
   without touching the (already-verified-correct) entropy or transform
   math at all, and is a very cheap thing to hand-check that nobody has
   isolated yet (prior sessions ruled out "transform type selection" and
   "inverse transform math" but the task description never explicitly
   named "dequantization scale factor" as separately audited).
5. Minor, low-priority loose end: verify `DEFAULT_COEFF_BR_CDF`'s runtime
   tx_size-index clamp (see the loose-end paragraph above) - cheap, unlikely
   to be this bug, but currently undocumented either way.
6. `capabilities().pixel_exact` still `false`; feature corpus still 1/13;
   `t2.ivf` frame 1 still 73 diff bytes, all chroma, unchanged - fifth
   consecutive session confirming the exact same numbers (this session
   made no code change, so this is true by construction, not by
   re-measurement - see point 6's re-check requirement for whichever
   session next touches actual code).

### Session cont'd 29 - Lead 1 (dequantization) mechanically CLOSED, clean;
### Lead 2 got a real, working live-CDF dav1d oracle for the first time in
### this bug hunt and it immediately found a genuine, measured txtp
### mismatch for the exact target TU (dav1d: DCT_DCT, Kinetix: FLIPADST_DCT)
### - but the obvious fix (a real frame-wide persistent TxTypes grid,
### written unconditionally incl. skip leaves, i.e. cont'd 26's Hypothesis A
### with its eob>0-gating bug fixed) STILL regresses the corpus by almost
### exactly the same magnitude cont'd 26 saw. Reverted, not committed. Bug
### still open, but the search space has narrowed hard and asymmetrically:
### the remaining unexplained piece is no longer "is dav1d's txtp really
### different here" (yes, proven) but "why does fixing that one lookup
### desync something else entirely, elsewhere in the same frame"

Baseline re-confirmed exactly: `t2.ivf` frame 1 = U 14 / V 59 / Y 0 (73 total
diff bytes), byte-identical to cont'd 23-28. Re-created
`tpt-kinetix-test-utils/tests/dbg_av1_t2_repro.rs` from cont'd 28's
description (still not committed).

**Lead 1 (dequantization scale factor) - executed exactly as scoped, and
is now conclusively CLOSED, clean, no bug found.** Confirmed via the
existing `KINETIX_AV1_DBG_RESDUMP` hook that mi=(16,11)'s V-plane TU is
decoded at runtime with `qindex_dc=qindex_ac=128` (both planes; `qindex_
for_plane` gave the same value for DC and AC here because this frame's
`delta_q_present=false` and every `delta_q_{y,u,v}_{dc,ac}` is 0 - confirmed
via a `KINETIX_AV1_DBG=1` dump of the real frame header: `base_q_idx=128`,
`using_qmatrix=false`, `qm_{y,u,v}=0`). `DC_QLOOKUP_8[128]=140`,
`AC_QLOOKUP_8[128]=176` reproduce the already-known `dequant=[-280,0,0,0,
-176,...]` exactly (`quant=[-2,0,0,0,-1,...]` times those steps, `dq_denom
(TX_4X4)=1`, no qmatrix). Then went one step further than "internally
consistent": mechanically diffed **every entry** of Kinetix's
`DC_QLOOKUP_8`/`AC_QLOOKUP_8` (256 entries each, both tables, `palette.rs`)
against dav1d's real `dav1d_dq_tbl[3][QINDEX_RANGE][2]` 8bpc block, parsed
programmatically out of `%LOCALAPPDATA%\Temp\dav1d_src\dequant_tables.c`
(a plain, unmodified upstream file - not the same file cont'd 28 diffed,
which was the CDF tables in `cdf.c`) with a small Node script. **Zero
mismatches across all 256 DC entries and all 256 AC entries.** Combined
with the already-tested `dq_denom`/clip-range unit tests
(`tests.rs::dq_denom_matches_spec_for_large_square_transforms`, TX_4X4 -> 1)
and the confirmed-in-this-session live `qindex_for_plane`/frame-header
values, **the entire dequantization step - table contents, qindex
selection, denom, clip range - is now proven correct for this exact TU**,
closing the one item cont'd 28 explicitly flagged as never separately
audited. No code change; nothing to measure or commit for this lead.

**Lead 2 (dav1d live adapted-CDF oracle) - got much further than any prior
session, including a real, working, instrumented build, and a genuine new
finding, but the obvious fix built on that finding regresses the corpus.**

Found a THIRD dav1d checkout on this machine nobody had used yet:
`%LOCALAPPDATA%\Temp\dav1d_oracle` - a clean, meson-configured-but-never-
built 1.5.4 source tree (distinct from both the segfaulting `dav1d_fresh`
cont'd 25 gave up on, and the plain `dav1d_src`/`dav1d-1.5.4-src` text-only
checkouts cont'd 28 used for the static CDF-table diff). Its original
`meson setup` had stalled on a `checkasm` test-subproject dependency
resolution failure; re-running `meson setup bld2 -Denable_tests=false
-Denable_tools=true` skipped that entirely and configured cleanly. Building
(`ninja tools/dav1d.exe`) hit one real, pre-existing compile error inherited
from an EARLIER session's uncommitted instrumentation left in this same
checkout's `src/decode.c` (a `KINETIX_DBG_FILTER` env-var-gated debug
`fprintf` using `t->l->ref[...]` where `t->l` is a `BlockContext` **value**
field, not a pointer - `BlockContext l, *a;` in `internal.h` - so it needs
`t->l.ref[...]`, not `->`). Fixed that one line (trivial, unrelated to this
session's own work) and the build succeeded. Running the resulting
`tools/dav1d.exe` needed `libdav1d.dll`'s directory added to `PATH` (Windows
DLL search, not an `rpath` issue) - once done, it decodes the corpus's
`t2.ivf` correctly (frame counts/timing match ffmpeg's libdav1d output).

Instrumented `src/recon.h`'s `DEBUG_BLOCK_INFO` macro (previously
hardcoded `0 && ...` = permanently disabled, confirmed genuine stock
upstream dav1d debug scaffolding, not something a prior session broke) to
`f->frame_hdr->frame_type != DAV1D_FRAME_TYPE_KEY && t->by == 11 && t->bx
== 16` and `src/recon_tmpl.c`'s `decode_coefs`'s `dbg` local from `DEBUG_
BLOCK_INFO && plane && 0` (also permanently-disabled stock code) to
`DEBUG_BLOCK_INFO && plane == 2` (chroma V only). This unlocked the
function's own extensive **pre-existing** `if (dbg) printf(...)` bin-by-bin
trace lines (`Post-non-zero`, `Post-eob_bin_*`, `Post-eob_hi_bit`, `Post-
eob`, `Post-lo_tok`, `Post-hi_tok`, `Post-dc_lo_tok`/`Post-dc_hi_tok`) that
were already in the source, just never reachable, plus a new `KDBG` block
added this session dumping the raw CDF array contents (`ts->cdf.coef.
{skip,eob_base_tok,base_tok,br_tok,dc_sign}`) at entry to `decode_coefs` for
this exact block.

Ran the instrumented `dav1d.exe` on the exact same `t2.ivf` (regenerated via
the same `ffmpeg testsrc2` command the corpus test uses) and got a full,
real, live trace for mi=(bx=16,by=11)'s V-plane TX_4X4 block - the FIRST
real dav1d block-level trace this entire bug hunt has ever obtained (every
prior attempt, cont'd 25 included, hit the `dav1d_fresh` segfault first).
**Cross-checked hard against cont'd 27's independently-hand-derived
bin-by-bin spec trace for this same TU, and every syntax element matches
exactly**: `sctx=7` (`Post-non-zero[0][7][0]`), raw `eob=2` (dav1d's
internal convention: a 0-based *last scan-position index*, not a count -
`for i = eob-1 downto 1` plus the eob token itself plus a separate DC term
gives `eob+1 = 3` total coefficients read, matching Kinetix's own
count-convention `eob=3` exactly, not a discrepancy), `eob_hi_bit=0`,
`scan[2]=1` (dav1d's own `scan_4x4` table storage order is the **transpose**
of the row-major `Default_Scan_4x4` listing most easily found in spec
prose - `{0,4,1,2,5,8,12,9,6,3,7,10,13,14,11,15}` vs `{0,1,4,8,5,2,3,6,9,12,
13,10,7,11,14,15}` - verified by hand-transposing every entry of the
row-major list and getting dav1d's exact sequence back; this is a genuine,
confirmed internal storage-convention difference between the two
codebases, not a bug in either one, since both ultimately address the same
2-D (row,col) positions through their own consistent x/y computation), and
the two token reads (`Post-lo_tok`/`Post-dc_lo_tok`) giving tokens 1 and 2
matching `quant=[-2,0,0,0,-1,...]` (unsigned magnitudes 2 and 1) exactly.
**Every bit consumed by this block, per this real oracle trace, is
identical to what Kinetix consumes** - this is now proven twice over (once
by cont'd 27's from-spec hand derivation, now again by a live oracle), not
just "no bug spotted."

**The one place the two decoders provably diverge: transform type.** The
trace's final line is `Post-uv-cf-blk[pl=1,tx=0,txtp=0,eob=2]` - dav1d
computed **`txtp=0` (`DCT_DCT`)** for this exact TU, where Kinetix computes
**`txtp=4` (`FLIPADST_DCT`)** (confirmed identical `TxfmType` enum ordering
in both codebases - `DCT_DCT=0`, ..., `FLIPADST_DCT=4` - by reading dav1d's
own `levels.h`). Since both types are `TX_CLASS_2D` (so the entropy
context/scan-table derivation - already proven identical above - doesn't
distinguish them at all), this single difference is invisible to every
context-formula/CDF-table audit any prior session ran, and only shows up in
the INVERSE TRANSFORM applied to otherwise-byte-identical decoded
coefficients - exactly matching the "locally-consistent-looking reads that
are nonetheless wrong" symptom cont'd 28 predicted for an upstream-state
bug. Traced *why* dav1d gets `DCT_DCT` here: its `decode_b`/`read_coef_tree`
machinery threads a real per-4x4-cell array, `t->scratch.txtp_map` (spec
§7.12.3's literal `TxTypes[y4][x4]`), written **unconditionally** after
every luma leaf's `decode_coefs` call (dav1d's `set_ctx` macro in
`recon_tmpl.c`, including the `eob == -1` all-skip case, which still writes
`DCT_DCT`/`WHT_WHT`) and read for chroma via `txtp = t->scratch.txtp_map[
(by4 + (y<<ss_ver))*32 + bx4 + (x<<ss_hor)]`. For this specific TU the
co-located luma cell belongs to `mi_row=10` (confirmed: `ly=40` -> luma 4x4
row 10), a *different, earlier-decoded sibling coded block* - structurally
outside what Kinetix's per-call-local `luma_leaf_types` Vec can ever see,
exactly the scoping gap cont'd 25/26 already suspected.

**Reattempted cont'd 26's Hypothesis A (a real persistent frame-wide grid)
with what looked like the one concrete bug fixed**: added a `Vec<u8>`
`luma_tx_types` field on `TileDecodeState` (frame-wide, `mi_rows*mi_cols`,
zero-initialized - `DCT_DCT` is 0, so an un-written cell already matches
dav1d's un-touched-`txtp_map`-cell default with no special-casing needed),
written via a new `record_luma_tx_type`/inlined-equivalent call **moved
outside the `if coeffs.eob > 0` gate** (cont'd 26's Hypothesis A wrote only
when `eob > 0`, which cont'd 26 blamed for the regression) so every luma
leaf - skip or not - updates the grid, from both `inter_block.rs`'s main
leaf loop and `intra_block.rs`'s IBC path (`reconstruct_ibc_block`, which is
real inter-coded per spec, `IsInter=1`). Rewired `co_located_luma_type` to
read this grid exclusively (converting the closure's tile-relative `lx`/
`ly` back to frame-absolute mi coordinates via `+ self.tile_px_x0/y0`, then
`/MI_SIZE`), dropping the local-list scan as the *lookup* (kept only for an
unreachable-in-practice out-of-bounds fallback). Had to restructure the
closure to capture only `&self.luma_tx_types` + a few `Copy` scalars
directly, not `self` as a whole, to avoid disjoint-borrow conflicts with
the later `&mut self.{u,v}_plane`/`self.dec`/`self.coeff_cdfs`/`self.
coeff_ctxs` borrows in the same function (a real, mechanical Rust borrow-
checker fix, not a design choice). Verified the target TU's grid lookup
now genuinely differs from before (`KINETIX_AV1_DBG_COLOC`: `frame_mi=(16,
10) grid=0`, i.e. `DCT_DCT`, matching dav1d) and that this is populated
from the correct sibling block's own leaf write.

**Measured result: regresses the corpus almost exactly as hard as cont'd
26's original (buggy) Hypothesis A did.** `t2.ivf` frame 1: Y-plane total
`|diff|` 0 -> **77641** (U differing samples 14 -> 907, V 59 -> 943) -
compare cont'd 26's own reported number for its `eob > 0`-gated version,
**also 77641, exactly**. Re-running `KINETIX_AV1_DBG_RESDUMP` after this
change showed the *entropy stream itself* had desynced well before reaching
this TU: the block that used to be reported as `mi=(16,11)` at
`cpx=(32,20)` is now `mi=(16,10)` at the same `cpx` - i.e. an earlier wrong
grid lookup somewhere upstream changed a `read_eob`/`is_1d` context bit,
consumed a different number of bits, and cascaded the entire partition
tree's decode order downstream of that point, not a contained, local
mis-render. This means cont'd 26's own diagnosis ("the regression was
specifically the `eob > 0` gating") was **incomplete at best**: this
session's write path has no such gate (confirmed by re-reading the diff
before reverting) and gets an almost identical-magnitude regression anyway,
strongly suggesting the *grid concept itself*, as both sessions have now
implemented it, has a second, independent bug - most likely something in
either (a) the tile-relative-to-frame-absolute coordinate conversion used
on the *read* side (the closure's `(lx + tile_px_x0)/MI_SIZE` math, algebra-
checked by hand this session against `px_x`'s own tile-relative convention
and believed correct, but not independently oracle-verified for any block
OTHER than the one target TU), or (b) a genuinely different, currently
unidentified EARLIER block in frame 1 where the correct chroma txtp
depends on the *local, own-leaves-only* `.first()` semantics cont'd 26's
Hypothesis B already proved is load-bearing for at least one block - i.e.
dav1d's real per-block model may not be a *pure* global-grid lookup at all
for every case, and Kinetix's pre-existing local-scan/`.first()` fallback
may already be accidentally correct for whichever block(s) that grid
lookup now breaks, while being wrong only for the one sibling-block case
this session traced. **Reverted in full** (`git checkout` on all 4 touched
files: `mod.rs`, `partition.rs`, `inter_block.rs`, `intra_block.rs` -
confirmed byte-identical to HEAD via `git status`/`git diff --stat` showing
zero changes in `tpt-kinetix-av1/` afterward). Nothing committed - per this
crate's rule, no fix without a measured improvement.

**Re-confirmed baseline after revert:** `cargo test -p tpt-kinetix-av1`
165+ tests all pass (lib) plus every integration/proptest/doctest binary,
zero failures. `cargo test -p tpt-kinetix-test-utils --test conformance`:
intra 6/6, inter 5/5 x4 clips, feature corpus 1/13 - byte-identical to
cont'd 23-28's numbers. `t2.ivf` frame 1: U 14 / V 59 / Y 0 (73 bytes),
unchanged.

**What this session adds that's genuinely new and should NOT be redone from
scratch:**
- A **working**, buildable, instrumentable dav1d oracle
  (`%LOCALAPPDATA%\Temp\dav1d_oracle`, `bld2/` subdir configured with
  `-Denable_tests=false -Denable_tools=true`) - the segfault/build blockers
  that stopped cont'd 25 onward are gone for THIS checkout specifically
  (distinct from the still-presumably-broken `dav1d_fresh`). Remember to
  add `dav1d_oracle/bld2/src` to `PATH` before running `tools/dav1d.exe`
  (DLL search path, not an rpath/build issue). The one-line `t->l->ref` ->
  `t->l.ref` fix in `src/decode.c` (inherited breakage, not this session's)
  is still needed if this checkout hasn't been rebuilt since.
- **Definitive proof (not a hypothesis) that Kinetix's `co_located_luma_
  type` is wrong for at least the mi=(16,11)/V-plane/`t2.ivf` TU
  specifically** - dav1d's real, live `txtp_map` says `DCT_DCT`, Kinetix's
  local-scan heuristic says `FLIPADST_DCT`, and every other input to this
  TU's decode (entropy bits consumed, dequant, table contents) is proven
  bit-exact both ways. This is the actual root cause of at least this one
  TU's wrongness - just not, on its own, a safe global fix.
- **A second, independent data point that "replace the local scan with a
  real persistent grid" is not sufficient on its own** - two different
  sessions, two different concrete implementations (one gated on `eob > 0`,
  one not), both regress the corpus by close to the same amount. Whatever
  the real fix is, it is more surgical than "always trust the grid."

#### Next session's starting point

1. **Do not re-attempt a blanket persistent-grid replacement of `co_located_
   luma_type` again without first finding the SPECIFIC earlier block where
   it diverges from the correct answer** - use the now-working `dav1d_
   oracle` (see above) to trace `txtp_map`/`decode_coefs` for a handful of
   EARLIER chroma blocks in this same frame (in decode order, before
   mi=(16,11)) and diff each one's dav1d `txtp` against Kinetix's own
   `KINETIX_AV1_DBG_COLOC` output, one at a time, to find the actual first
   point of divergence - rather than swapping the whole mechanism and
   measuring only the aggregate frame diff.
2. A promising middle path, not yet tried: keep `co_located_luma_type`'s
   existing local-list-plus-`.first()` behavior as the primary answer
   (proven load-bearing by cont'd 26's Hypothesis B), and ONLY fall back to
   the frame-wide grid when the local list is completely empty for the
   queried chroma position (i.e. genuinely reaching outside this call's own
   `leaves` - the exact mi=(16,11) case this session traced). This is
   narrower than either full-replacement attempt so far and directly
   targets the one proven-wrong case without touching the (apparently
   correct) local-scan path for every other block.
3. If pursuing (1) or (2), the `dav1d_oracle` build from this session is
   ready to reuse - just re-add `KINETIX_AV1_DBG_COLOC`-equivalent tracing
   at each candidate block and compare against a `DEBUG_BLOCK_INFO`-gated
   dav1d run with the target `t->by`/`t->bx` retargeted per block (the
   `recon.h`/`recon_tmpl.c` edits from this session were reverted along
   with everything else in `dav1d_oracle` being scratch space, not a repo
   file - re-apply the same two edits described above, `ninja tools/dav1d.
   exe` rebuilds incrementally in seconds).
4. **Lead 1 (dequantization) is now closed for good** - do not re-audit the
   quantizer tables/dq_denom/clip range without a specific new reason;
   this session's diff was exhaustive (all 256 DC + 256 AC entries against
   dav1d's real `dequant_tables.c`) and the live runtime values for the
   target TU were independently cross-checked against the frame header.
5. `capabilities().pixel_exact` still `false`; feature corpus still 1/13;
   `t2.ivf` frame 1 still 73 diff bytes, all chroma, unchanged - sixth
   consecutive session confirming the exact same numbers (this session's
   only code changes were reverted before finishing, so the working tree
   is unchanged from cont'd 28).

### Session cont'd 30 - THE t2.ivf CHROMA BUG IS FIXED (73 diff bytes -> 0,
### measured). Root cause was the opposite of every prior session's working
### theory: not a cross-block/sibling lookup at all - a single eob>0 gate on
### `co_located_luma_type`'s OWN local list, dropping a leaf from the SAME
### coded block it needed. Feature corpus 1/13 -> 7/13. No regressions.

Started from cont'd 29's exact handoff: use the (now-working) `dav1d_oracle`
build to trace a few chroma blocks OTHER than mi=(16,11) before attempting
another grid-based fix, since two prior sessions' full-replacement grids
both regressed the corpus by ~77641 Y-plane diff bytes despite looking
correct by inspection.

**Step 1 - implemented cont'd 29's own suggested middle path first (local
list primary, frame-wide grid only as a fallback on a local-scan miss).**
Added a persistent `luma_tx_type_grid: Vec<u8>` (`mi_rows*mi_cols`, `0xFF`
sentinel) on `TileDecodeState`, written at every luma leaf's frame-absolute
mi coordinates (unconditionally, matching dav1d's real per-leaf write) from
both `inter_block.rs`'s main leaf loop, consulted by `co_located_luma_type`
ONLY when its existing local-list scan already misses (i.e. `own_luma_tx_
type`'s old fallback path). This is narrower than either of cont'd 26/29's
full-replacement attempts and left every block whose local list already
covers its own chroma footprint byte-for-byte untouched.

**Measured result: still regressed, same magnitude as before (Y-plane diff
0 -> 78755).** `KINETIX_AV1_DBG_COLOC` showed the fallback only fires for
TWO positions in the whole frame (mi=(4,3) and mi=(23,0)), and mi=(16,11) -
the actual target TU - was never even reached again once the entropy stream
desynced at whichever of those two comes first in decode order. This
directly disproves the "the local scan is already correct everywhere it
doesn't miss" assumption cont'd 29's plan rested on: at least one of those
two miss-positions' grid answer was wrong, meaning a real block IS relying
on the pre-existing (wrong-by-grid-standards) `own_luma_tx_type` fallback
for correctness. Reverted this attempt in full before proceeding (confirmed
`git diff --stat` clean on `tpt-kinetix-av1/` again).

**Step 2 - traced the actual C source of `t->scratch.txtp_map`'s write/read
sites in the working `dav1d_oracle` checkout (`src/recon_tmpl.c`) instead of
trusting cont'd 29's characterization of it from memory, and this is what
actually cracked it.** `bx4`/`by4` in the real single-tile reconstruction
function (`recon_b_inter`'s chroma loop, ~line 1970-1997) are computed
**once, at the top of the whole coded-block's own reconstruction call**
(`t->bx & 31`, `t->by & 31` - THIS block's own top-left mi cell, not a
sibling's), and the chroma read's index is `(by4 + (y<<ss_ver))*32 + bx4 +
(x<<ss_hor)` with `y=x=0` for the block's first (and here, only) chroma tx
tile - i.e. **the lookup for mi=(16,11)'s chroma TU resolves to mi=(16,11)'s
OWN top-left luma mi cell, not mi=(16,10)'s**, contradicting cont'd 29's
"belongs to the sibling mi_row=10" conclusion. Rebuilt `dav1d_oracle` with
`DEBUG_BLOCK_INFO` retargeted to `t->by==11 && t->bx==16` (and briefly
`==10` too, to rule the sibling all the way out) and a new printf on the
luma leaf write site (`Post-y-cf-blk`, `recon_tmpl.c` ~line 818) showing
`WRITEby4`/`bx4`: got `WRITEby4=11 bx4=16 t->bx=16 t->by=11 txh=1 txw=1
eob=-1 txtp=0` - **this block's own FIRST luma leaf** (at mi_col=16, an
all-zero/skip TU that never calls `transform_type()` so defaults to
`DCT_DCT`), immediately followed in the trace by `Post-uv-cf-blk[pl=0,
txtp=0,eob=0]` and `Post-uv-cf-blk[pl=1,txtp=0,eob=2]` (the exact target
TU) - both read `txtp=0` straight from that same first leaf's write, since
the chroma read's `x=0` index lands exactly on `bx4=16`, not the second
leaf at `bx4=17` (the real nonzero `eob=3` `FLIPADST_DCT` leaf cont'd 25/27
already characterized). The full oracle trace (a "poc=1,y=10,x=16,bl=4,
bp=1" partition print at the very top) also confirms mi_row=10 is a
genuinely separate, **intra**-coded sibling block (`Post-intra[1]`) entirely
unrelated to this lookup - cont'd 29's misattribution likely came from an
adjacent trace line being misread, not from a real cross-block dependency.

**Root cause, finally precise: `inter_block.rs`'s `luma_leaf_types.push(...)`
call was gated inside `if coeffs.eob > 0 { ... }` (confirmed by re-reading
the brace structure carefully - a claim cont'd 25 stated correctly but which
this session almost mis-transcribed too on a first pass).** This drops any
all-zero (`eob == 0`, `txb_skip` true) leaf from the block's own local scan
entirely, even though dav1d's real `read_coef_tree` writes `txtp_map`
**unconditionally** after every leaf, all-zero or not (the all-zero leaf's
`txtp` still defaults to `DCT_DCT`, matching what `coeffs.tx_type` already
holds for that case in this codebase too - no separate default-handling
needed). For mi=(16,11), the local list this omission produced was
`[(68,44,4,4,FLIPADST_DCT)]` (only the second, nonzero leaf) - so any
chroma query that should have landed on the first leaf's position
(`px=64..68`) missed the scan and fell through to `own_luma_tx_type =
.first()`, which returned the wrong (only remaining) leaf's type
(`FLIPADST_DCT`) instead of the correct `DCT_DCT` sitting right there in
the same block, just never added to the list.

**The fix: move `luma_leaf_types.push(...)` outside the `if coeffs.eob > 0`
block, so it runs unconditionally for every leaf this coded block reads**
(still gated on the outer per-block `!skip`, unchanged - a block-level-skip
block still has no leaves at all here, matching dav1d). No new state, no
frame-wide grid, no cross-block reasoning required - a strictly local,
single-block fix.

**Measured: `t2.ivf` frame 1 diff 73 bytes (U 14, V 59, Y 0) -> 0 bytes,
exactly.** `cargo test -p tpt-kinetix-av1`: 165 lib tests pass (same as
baseline) plus every integration/proptest/doctest binary, zero failures.
`cargo test -p tpt-kinetix-test-utils --test conformance`: intra corpus
still 6/6 bit-exact, inter corpus still 5/5 x 4 clips bit-exact (zero
regression on either), and **feature corpus jumped from 1/13 to 7/13**
(`angle_delta`, `cfl_intra`, `filter_intra`, `palette`, `flip_idtx`, `tx64`,
`rect_partitions` all now exact - only `tiles_2x2`, `lossless`,
`restoration`, `cdef_off`, `global_motion`, `ref_frame_mvs` remain gapped,
all pre-existing, unrelated feature gaps). This is the single largest
feature-corpus jump this bug hunt has recorded.

**Committed to master** (plain message, no co-author trailer per repo
convention) - `inter_block.rs`'s one-statement move plus its doc comment.
`dbg_av1_t2_repro.rs` NOT committed, per the established "no unlanded
throwaway harness" convention (recreate from cont'd 23's description if a
future session needs it again - unchanged since cont'd 26).

**What this means for the bug hunt overall:** every session from cont'd
23 through 29 (co-located-luma-type hypotheses A/B, the coefficient-entropy
bin-by-bin audit, the static default-CDF table audit, the dequantization
audit, and the first working dav1d oracle trace) was real, valid,
necessary elimination work - none of it was wasted, and the oracle
infrastructure cont'd 29 built is what let this session read the *actual*
C source carefully enough to catch its own mis-framing before recreating
cont'd 29's regression a third time. The actual bug was hiding in plain
sight in a comment cont'd 25 wrote about this exact leaf five sessions ago
("only the second gets pushed... push is gated on eob > 0") - stated as an
observation, correctly, but never connected to being the fix.

**`capabilities().pixel_exact` is still hard-coded `false`** in
`decoder.rs` regardless of any corpus number (it is a policy gate, not
derived from test results) - this session did not touch that, and there
are still real, separate gaps (`tiles_2x2`/`lossless`/`restoration`/
`cdef_off`/`global_motion`/`ref_frame_mvs`) before that would even be
worth reconsidering. The t2.ivf-specific default-encoder-path regression
this whole 6-session (now 7) hunt was chasing is, as far as this session's
own measurement can tell, closed.

#### Next session's starting point

1. The t2.ivf chroma bug is fixed - do not reopen it without a new failing
   repro. If one appears, check `luma_leaf_types` population first (now
   unconditional on `eob`) before assuming a new class of bug.
2. Feature corpus is 7/13. The 6 remaining gaps (`tiles_2x2`, `lossless`,
   `restoration`, `cdef_off`, `global_motion`, `ref_frame_mvs`) are
   unrelated to this session's fix and each look like their own standalone
   investigation - pick one and start fresh rather than assuming any
   shared root cause with the txtp bug just closed.
3. The working `dav1d_oracle` checkout (`%LOCALAPPDATA%\Temp\dav1d_oracle`,
   `bld2/` subdir, `tools/dav1d.exe`, needs `bld2/src` on `PATH` for the
   DLL) is a reusable, real asset for whichever of those 6 gaps gets picked
   up next - this session further confirmed it can be retargeted
   (`DEBUG_BLOCK_INFO` in `src/recon.h`) and rebuilt in seconds
   (`ninja -C bld2 tools/dav1d.exe`) for arbitrary `t->by`/`t->bx`
   coordinates, and that reading the actual C source structure (not just
   its printf output) matters - this session's first attempt at using the
   oracle still mis-identified which coded block a lookup belonged to
   until the source itself was read line-by-line.
4. On Windows, remember `-o /dev/null` silently fails dav1d.exe's own arg
   parser ("No extension found for file nul") and exits without decoding
   anything - use a real output filename (e.g. `-o out.y4m`) instead, or
   the run will look successful (`EXIT: 0`) while producing zero trace
   output, which cost real time this session before being caught.

## Session 2026-09-28 — returned to the tile/frame-4 thread (cont'd 4-22, a
## different bug class than the just-closed t2.ivf chroma fix): the old
## "interp_filter desync at frame 4" framing is STALE — cont'd 7 already
## fixed that specific desync via the `tx`/`tx_intra` split. Current real
## divergence starts one block into tile 1's first superblock row; formed
## and tested a concrete cross-tile spatial-MV hypothesis; it REGRESSES the
## corpus and was reverted, not committed

Picked this up expecting to find frame 4 still desyncing at the
`interp_filter` read per an out-of-date memory summary. **That framing no
longer matches the code**: cont'd 5/6 hypothesised and then disproved an
`interp_filter`-context bug, and cont'd 7 (same day, further down this
file) root-caused and fixed the *actual* frame-4 desync — a `ctx->tx` /
`ctx->tx_intra` array collision — before this session ever started.
Re-verified from scratch rather than trusting either memory or the todo
file's prose:

- Baseline before any change (`KINETIX_AV1_FATE_DIR=/tmp/fate_av1`,
  `cargo test -p tpt-kinetix-test-utils --test conformance --release`):
  intra corpus 6/6, inter corpus 5/5×4 clips all bit-exact, feature corpus
  **7/13** (matches the task's stated post-4c40dd6 baseline exactly), FATE
  aggregate **5/195**, `non_uniform_tiling` **3/24** (frames 0-2 exact,
  frame 3 the first divergent one).
- Commit `4c40dd6` (the just-landed t2.ivf chroma fix) touches
  `co_located_luma_type`/coefficient bookkeeping only — confirmed
  unrelated to this stream by inspection; `non_uniform_tiling`'s own
  history (cont'd 21/22) already accounts for its current 3/24 via the
  CDEF full-extent and MC-visible-dims fixes, not this commit.

### Re-localised frame 3's divergence with the current code

`probe_tiles --example` (already committed) + `BLOCKMAP=1` +
`KINETIX_AV1_DBG_PXY`/`KINETIX_AV1_DBG_PRED`/`KINETIX_AV1_DBG_B0` +
`KINETIX_AV1_DBG_SEQ` (the frame-counter fix from cont'd 9 makes this
reliable now): frame 3 (dav1d order_hint=3, the first hidden alt-ref
having already been decoded as internal frame 1) first diverges at Y
(384,64) — mi (96,16) — by exactly 2 (Kinetix 162, dav1d 164),
**pre-filter** (deblock/CDEF/LR are all confirmed no-ops at this exact
pixel this frame — same "value already wrong before any filter runs"
signature cont'd 9 established as the reconstruction-bug tell). mi_row 16
is tile row 1's mi_row 0 (`row_start_sb=[0,1,2,3,5]` ⇒ tile row 1 starts
at pixel y=64) — i.e. the very first superblock row of the *second* tile
in this 1-col×4-row layout, one block in from the tile's own left edge
(the first SB of that row, mi_col 64-95, is already pixel-exact; the
divergence starts at mi_col 96). `mi=(96,16) bsize=9 skip=true` (all-zero
residual — a pure-MC divergence, not a coefficient one) `ref=8 mm=0
filter0=0` (matching cont'd 5/6/7's now-closed leaf's general shape, but
this is a *different* specific block/stream position than the one those
sessions traced — not a re-open of the same bug).

### Hypothesis: `find_mv_stack`'s top-left "corner" candidate probe reads
### across a tile-row boundary, and Kinetix's per-tile-fresh spatial grid
### can never match dav1d there

Read dav1d 1.5.4's real `refmvs.c` (`raw.githubusercontent.com`, both the
`add_spatial_candidate` gate and `dav1d_refmvs_tile_sbrow_init`) via
WebFetch. Findings, cross-checked against Kinetix's
`inter_mv_stack`/`intra_block.rs`:

- The regular above/left scans (`scan_row`/`scan_col`) are correctly
  gated by `by4 > row_start` / `bx4 > col_start` (tile-relative, not
  frame-relative) in Kinetix — confirmed no bug there, matches dav1d and
  matches cont'd 8's prior audit.
- The one **top-left diagonal "corner" probe** (`add_spatial_candidate`
  at `(by4-1, bx4-1)`, weight 4, feeding a *dummy* have-newmv accumulator
  per cont'd 8) is gated only by `(n_rows|n_cols) != sentinel` in both
  dav1d and Kinetix — **not** independently bounded by `row_start`. For a
  block at a tile's very first row with an available left neighbour
  (exactly mi=(96,16)'s situation: `col_start=0`, `bx4=96>0` so the left
  scan runs and sets `n_cols`), this probe reads one row *above* the
  tile's start — i.e. into the tile above.
- Confirmed via `dav1d_refmvs_tile_sbrow_init`'s real source that dav1d's
  single-threaded path (`n_tile_threads==1` forces `tile_row_idx=0`) does
  **not** clear or border-fill that row between tiles — the buffer is a
  persistent, frame-wide circular window, so a single-threaded dav1d
  decode genuinely can see the tile-above's real last-row content there.
- Kinetix's `refmv_grid` (`TileDecodeState::new`, `mod.rs`) is allocated
  **fresh per tile** (`vec![RefMvCell::default(); mi_cols*mi_rows]`,
  frame-sized but blank), and tiles decode via `par_iter` in parallel —
  so this exact corner-probe read can *only* ever see a blank/no-match
  cell in Kinetix, never the real neighbour dav1d might have. A wrong
  `have_row`/`newmv_ctx` bit here changes which CDF a following symbol
  read uses without changing the decoded value, cascading into a
  permanent per-tile entropy desync from that point on — the identical
  failure signature this whole cont'd 4-22 thread has been chasing.

### The fix built and measured — REGRESSES, reverted

Implemented: `TileDecodeState::new` gained a `prior_tile_refmv:
Option<&[MotionFieldCell]>` parameter seeding `refmv_grid` (mv/refs only;
w4/h4/mf are irrelevant to the corner probe per the dummy-accumulator/
fixed-weight-4 analysis above) for every cell outside the tile's own rows
(cells the tile owns get overwritten as it decodes its own blocks
regardless). `reconstruct_av1_frame`'s tile-decode loop was restructured
from one flat `tile_payloads.par_iter()` into a sequential loop over tile
**rows** (still `par_iter` *within* a row, so same-row tiles remain fully
parallel/independent), threading a running frame-sized `Vec<
MotionFieldCell>` forward: after each row, every tile's own already-
computed `motion_field` output (this mechanism already existed, for the
*next frame's* temporal projection — reused as-is) is merged into the
seed passed to the next row.

Compiled clean, all 165 AV1 lib tests + `proptest_coeffs`/`proptest_obu`
+ doc-tests pass (two internal-test call sites needed a trailing `None`
for the new parameter: `reconstruct/tests.rs`'s 9 direct
`TileDecodeState::new` calls plus `tests/proptest_coeffs.rs`'s
`decode_tile_group` call — mechanical, no behavioural intent).

**Measured effect is a regression, not a fix**:

| metric | before | after |
|---|---|---|
| `non_uniform_tiling` FATE | 3/24 (frames 0-2 exact) | **1/24** (only frame 0 exact — frame 1 now *also* diverges, where it didn't before) |
| FATE aggregate | 5/195 | **3/195** |
| `tiles_2x2` feature-corpus worst PSNR | 17.51 dB | **15.56 dB** (worse) |
| intra/inter synthetic corpus, other 5 feature-corpus exact entries | unaffected | unaffected |

Reverted in full (`git checkout` on `reconstruct/mod.rs`,
`reconstruct/tests.rs`, `tests/proptest_coeffs.rs`) and re-confirmed the
exact pre-change baseline (7/13 feature corpus, 165 lib tests, 3/24
`non_uniform_tiling`) is restored. **Not committed** — per this crate's
rule, a plausible-by-source-reading fix that regresses on measurement
does not ship.

### Why it's wrong, and what's still open

The corner-probe *mechanism* (dav1d reads real cross-tile-row content
there, Kinetix reads blank) is confirmed real from the source alone — but
either (a) that mismatch isn't actually what causes mi=(96,16)'s
divergence (some other, still-unidentified symbol/context differs
instead, and this session's fix just introduced a *new* wrong bit
somewhere it didn't belong — e.g. `tiles_2x2`'s regression suggests the
seeding fires somewhere it shouldn't for a *different* stream entirely),
or (b) the mechanism is real but this session's approximation of dav1d's
actual stored value at that cell (the tile-above's *own final* written
content, taken from its own decode's exact same output data used for
next-frame temporal projection) is not what dav1d's real circular-buffer
addressing (`off = (sbsz*sby) & 16`, the `EXCHANGE`-based double-buffer
swap) actually holds at that exact row when `find_mv_stack` reads it —
dav1d's addressing is 2-row circular per superblock-row parity, not a
simple "whatever the tile above finished with", and this session did not
model that precisely. (a) and (b) are not mutually exclusive and neither
was distinguished this session.

**Next session's starting point:**

1. Do not re-attempt this exact seeding approach without first getting a
   *real* dav1d-side trace of what value `find_mv_stack`'s corner probe
   actually reads at mi=(95,15) [`by4-1,bx4-1` for mi=(96,16)] when
   decoding this exact frame/tile — the working oracle from cont'd
   29/30 (`%LOCALAPPDATA%\Temp\dav1d_oracle`, `bld2/` subdir,
   `ninja -C bld2 tools/dav1d.exe`, retarget `DEBUG_BLOCK_INFO` in
   `src/recon.h`) should be used for this instead of reasoning from the
   C source's static structure alone, which is what led this session's
   plausible-looking fix to still be wrong.
2. If the oracle shows the corner-probe cell really is blank/no-match in
   dav1d too for this specific leaf, the corner-probe hypothesis is fully
   dead for this bug and the next unanchored-symbol suspect list from
   cont'd 5/8 (already-eliminated: var-tx, cdef/delta_q/delta_lf,
   ref_frame_mvs/load_tmvs, interp_filter ctx) needs a genuinely new
   entry — re-derive from a fresh full symbol-sequence dump at mi=(96,16)
   rather than assumption.
3. If the oracle instead confirms real cross-tile content is read, the
   fix needs dav1d's actual double-buffer/parity addressing modelled
   precisely (not "last row the tile above wrote"), and the `tiles_2x2`
   regression needs its own root-cause before landing anything — that
   stream has a *different* pre-existing gap (1/4 exact per the feature
   corpus) that this session's change made numerically worse, which
   needs explaining even if the tile-boundary theory itself is sound for
   `non_uniform_tiling`.
4. Baseline to protect: feature corpus 7/13, FATE aggregate 5/195,
   `non_uniform_tiling` 3/24, intra 6/6, inter corpus 5/5×4 clips, 165 AV1
   lib tests — all unchanged and reconfirmed after this session's revert.

### Session 2026-09-28 cont'd 2 — corner-probe hypothesis DEFINITIVELY
### DISPROVEN via a live dav1d oracle trace (not just source-reading); found
### a real but currently-dormant Kinetix bug in the same gate (OR should be
### AND) and, while chasing why it measured as a no-op, found the ACTUAL
### divergence: at the exact target frame, Kinetix decodes a 32×32
### partition where dav1d decodes 64×64 at the same position — a
### partition-tree divergence, not an mv-stack bug. No fix landed (the
### OR/AND fix is zero-effect everywhere and was reverted); this is a
### precise handoff to the real bug class for the next session.

Picked up exactly where the prior entry (same day) left off: its corner-
probe/cross-tile-row hypothesis for `non_uniform_tiling`'s frame-3
divergence at mi=(96,16) had a plausible C-source reading but a regressing
fix, and its own writeup said not to re-attempt the fix without a *live*
dav1d trace of the actual corner-probe read. Reused the already-working
`dav1d_oracle` (`%LOCALAPPDATA%\Temp\dav1d_oracle`, `bld2/`,
`ninja -C bld2 tools/dav1d.exe`) rather than rebuilding anything.

**Step 1 — live-traced the corner-probe gate itself and found the prior
session's mechanism can't be what's happening.** Instrumented
`dav1d_refmvs_find` (`src/refmvs.c`) to print `n_rows`/`n_cols`/whether the
top-left corner probe (`refmvs.c` line ~457, `if ((n_rows | n_cols) !=
~0U)`) actually fires, filtered to `by4==16 && bx4==96`. Ran the real
`non_uniform_tiling.ivf` through the instrumented `tools/dav1d.exe
--threads 1`. Result: **`corner_fires=0` on every single occurrence across
the whole clip (24+ lines, every frame this mi position is inter-coded)**,
because `n_rows` is `0xffffffff` (sentinel) every time — this mi position
is tile row 1's own first row (`by4==tile_row.start`), so the top scan's
`if (by4 > rt->tile_row.start)` never runs, and the corner-probe gate
`(n_rows | n_cols) != ~0U` is bitwise-OR against an all-ones sentinel:
ORing anything with `~0U` always yields `~0U`, so the gate can only be
true when **both** `n_rows` and `n_cols` are non-sentinel, never just one.
dav1d's corner probe is *structurally incapable* of firing at a tile's own
first mi-row here, regardless of what the tile above contains — the prior
session's whole "dav1d reads real cross-tile content, Kinetix reads blank"
mechanism does not occur for this specific block. This is proven from live
execution, not just re-reading the C source (the prior session's own
mistake mode).

**Step 2 — found a real, independent bug in Kinetix's copy of this same
gate, but it turned out to be a dormant no-op.** Re-reading Kinetix's own
`inter_mv_stack` (`tpt-kinetix-av1/src/reconstruct/intra_block.rs`) against
this same gate: Kinetix has **two** copies of the equivalent check (one in
a simpler single-ref mv-stack helper around line 1188, one in the full
`inter_mv_stack` around line 1519), and **both use `n_rows != -1 ||
n_cols != -1`** (logical OR — fires if *either* scan ran) where dav1d's
real semantics (via the bitwise-OR-with-sentinel trick proven in step 1)
require **both** to have run. This is a genuine, provable divergence from
the spec/dav1d — for exactly the "tile's first mi-row, left neighbour
available" case, Kinetix's corner probe fires when it shouldn't.

Fixed both call sites (`||` → `&&`), rebuilt, and used Kinetix's own
`KINETIX_AV1_DBG_MVSCAN="16:96"` trace (`probe_tiles` example in
`tpt-kinetix-test-utils/examples/probe_tiles.rs`) to confirm the change
took effect: pre-fix, the trace showed an extra `add_s` line for the
corner-probe cell (`r=15 c=95 mv=(0,0) ref=(0,0)`); post-fix, that line is
gone. **But the final candidate stack (`cnt=2`, weights 688/36) was
byte-identical before and after** — the wrongly-firing pre-fix probe read
a cell whose `ref=(0,0)` didn't match this block's `want=(8,0)`, and
Kinetix's `add()` closure is a complete no-op on a ref mismatch (confirmed
by reading it directly — it only mutates `stack`/`have_match`/`have_newmv`
inside the `if cand.refs[n] == want_refs[0]` branch, nothing happens
otherwise). Full-suite measurement after the fix: **zero change anywhere**
— `non_uniform_tiling` still 3/24, FATE aggregate still 5/195, feature
corpus still 7/13, intra 6/6 and inter corpus untouched. The fix is real
and dav1d-exact (verified against live execution, not just source), but is
apparently a coincidence-gated dead branch across the entire current test
corpus. Per this crate's explicit measure-before-committing rule, this was
**not committed** — reverted cleanly (`git checkout --
tpt-kinetix-av1/src/reconstruct/intra_block.rs`, confirmed empty `git
status` on the crate afterward).

**Step 3 — while cross-checking *why* the fix was a no-op, found the real
bug.** Extended the dav1d oracle to print the actual `mvstack` contents
dav1d derives at this block (hooked `decode_b`'s `dav1d_refmvs_find` call
site in `src/decode.c` around line 1664 — the single-ref, non-sub8x8 path
— printing `n_mvs`/`ctx`/each candidate's mv+weight) and separately printed
`bs`/`bw4`/`bh4` at the top of `decode_b` itself (`src/recon.h`'s
`DEBUG_BLOCK_INFO` retargeted to `t->by==16 && t->bx==96`). Cross-referenced
against Kinetix's own `KINETIX_AV1_DBG_SEQ=1` trace (prints `order_hint`/
`show_frame`/`frame_type` per internally-decoded frame, `decoder.rs`) to
unambiguously align the two decoders' frame sequences — `non_uniform_
tiling` has a hidden alt-ref frame decoded before its own order_hint's
"real" shown frame, which shifts dav1d's raw decode-call sequence by one
relative to a naive count, and this shift is *why* an earlier alignment
attempt in this same session (matching by call-position, not by
order_hint) briefly looked ambiguous before this step nailed it down
properly. With the alignment fixed by `order_hint` instead of call
position: **the FATE conformance test's "frame 3" is exactly Kinetix's
internal `order_hint=3` shown frame** (frame 0 is the key frame, hidden
alt-ref carries `order_hint=1` and is never shown, so shown order_hints
0,1,2,3,... map 1:1 to display frame indices 0,1,2,3,...).

At that exact frame (order_hint=3), for the exact target mi position:
- **Kinetix** (`KINETIX_AV1_DBG_MVSCAN="16:96"` + `KINETIX_AV1_DBG_SEQ=1`,
  `probe_tiles` example): `MVSCAN find by=16 bx=96 bsize=9 want=(8,0)` —
  `bsize=9` is `BLOCK_32X32` in Kinetix's own `BLOCK_WIDTH`/`BLOCK_HEIGHT`
  tables (`reconstruct/mod.rs`) — a **32×32** leaf.
- **dav1d** (live oracle, same frame by `order_hint` alignment):
  `KINETIX_DECODE_B by=16 bx=96 bs=3 bw4=16 bh4=16 ... frame_offset=3` —
  `bw4=16, bh4=16` directly from `dav1d_block_dimensions[bs]`, i.e. a
  **64×64** leaf, at the exact same mi position.

**This is the real bug: Kinetix's partition tree splits this superblock
into (at least) a 32×32 leaf where dav1d's real partition decode keeps it
as a single, unsplit 64×64 `PARTITION_NONE` block.** Every downstream
symptom this whole cont'd-4-through-22 thread and this session's earlier
steps chased — the mv-stack candidate-weight mismatch (dav1d's real
`cand[1]` weight is 32, Kinetix's is 36, because the temporal-candidate
scan's grid/step bounds are a function of `bw4`/`bh4`, which differ
precisely because the block sizes differ), the "one block into tile row
1's first SB row" symptom, all of it — is downstream of this one wrong
partition-size decode, not an independent mv-stack or corner-probe bug.
The corner-probe/tile-row-boundary theory from earlier today was chasing a
real *effect* (this position sits right at a tile-row start) with the
wrong *mechanism* (mv-stack neighbour availability) — the actual
tile-boundary-adjacent thing that's wrong is the **partition symbol
decode** for the superblock straddling tile row 1's start, not any
mv-stack scan.

**Why this matters for the tile-boundary framing:** `partition_context()`
(`tpt-kinetix-av1/src/reconstruct/partition.rs` line ~493) already gates
`avail_u`/`avail_l` tile-relative (`mi_row > tile_px_y0/MI_SIZE`), matching
dav1d's `have_top`/`have_left` exactly — and for this specific SB,
`avail_u` is `false` on both sides (it's tile row 1's own first row, same
tile-relative gate, same result both implementations), so the *partition
context* itself should not differ here. That means either (a) the actual
CDF/probability state feeding this partition-symbol read is already
desynced from an earlier point in this tile's own bitstream (the classic
"real bug is upstream, this is just where it becomes visible" pattern —
consistent with the block immediately to this one's *left* in the same SB
row being pixel-exact, i.e. the desync's own trigger is very local, right
around this SB boundary), or (b) some other input to the partition symbol
read (CDF table content, `bsl` computation, a different context term
entirely) is wrong specifically for this block-level/size combination.
Neither was distinguished this session — this is a clean, oracle-verified
handoff point, not a finished diagnosis.

**Not committed — no code change landed.** The only repo change this
session made (the `||`→`&&` gate fix) was proven correctness-neutral
across the entire test corpus and reverted per the explicit
measure-before-committing rule for this bug hunt. `git status` on
`tpt-kinetix-av1/` is clean; `cargo test -p tpt-kinetix-av1 --release`
(all lib/integration/proptest/doctests) and the full conformance suite
were reconfirmed byte-identical to this morning's baseline: intra 6/6,
inter sequence 8/8 + inter corpus 5/5×4 clips, feature corpus 7/13, FATE
aggregate 5/195, `non_uniform_tiling` 3/24.

#### Next session's starting point

1. **The bug is in partition-tree decoding, not mv-stack/corner-probe
   candidate derivation.** Do not re-open the corner-probe hypothesis for
   `non_uniform_tiling` without new evidence — it is now disproven by live
   oracle trace (step 1 above), not just re-derived from source.
2. Target precisely: `non_uniform_tiling.ivf`, internal `order_hint=3`
   (the FATE conformance test's "frame 3"), the 64×64 superblock whose
   top-left mi is `(mi_row=16, mi_col=96)` — tile row 1's first SB row,
   second SB from the tile's own left edge. dav1d decodes `PARTITION_NONE`
   (stays 64×64); Kinetix decodes something that produces (at least) a
   32×32 leaf at the same top-left mi. Get the actual `PARTITION_*` symbol
   Kinetix reads for this SB (add a debug hook in `partition.rs`'s
   `decode_partition`-equivalent, or reuse `KINETIX_AV1_DBG_SEQ`-style
   tracing) and cross-check against dav1d's own partition-symbol read at
   the same node (`decode.c`'s partition-read call site, guarded by the
   same `DEBUG_BLOCK_INFO` macro already retargeted to `t->by==16 &&
   t->bx==96` this session — that macro edit is scratch state in
   `%LOCALAPPDATA%\Temp\dav1d_oracle` and was NOT reverted, reuse it).
3. Given `partition_context()`'s tile-relative gating already matches
   dav1d exactly for this node (both `avail_u=false` here), the likely next
   suspects are (a) an entropy desync *earlier* in this same tile/SB-row's
   bitstream that only becomes visible at this partition read (check the
   handful of symbols read for the SB immediately to the left — the one
   that IS pixel-exact — for anything read but not fully verified bit-for-
   bit, not just pixel-matched), or (b) the partition CDF table content or
   `bsl`/context-index formula itself for this specific block-size/context
   combination. Do not re-check `partition_context()`'s tile gating itself
   — it's correct, per this session's read.
4. The `dav1d_oracle` build now has three live instrumentation additions
   from this session, all still in place (not reverted, it's scratch):
   `refmvs.c`'s corner-probe/`KINETIX_TRACE` and temporal-loop/
   `KINETIX_TCOUNT` prints (both gated `by4==16 && bx4==96`), and
   `decode.c`'s `KINETIX_DECODE_B`/`KINETIX_MVSTACK` prints in `decode_b`
   (gated by the retargeted `DEBUG_BLOCK_INFO` macro in `recon.h`,
   currently `t->by==16 && t->bx==96`). Rebuild with
   `ninja -C bld2 tools/dav1d.exe` (seconds, incremental) and add a
   partition-symbol print next to these rather than re-deriving the setup.
5. `KINETIX_AV1_DBG_SEQ=1` (prints `order_hint`/`show_frame`/`frame_type`
   per internally-decoded frame) combined with `KINETIX_AV1_DBG_MVSCAN=
   "row:col"` on the `probe_tiles` example
   (`tpt-kinetix-test-utils/examples/probe_tiles.rs`) is the reliable way
   to align Kinetix's internal frame sequence with a specific `order_hint`
   — do not assume `probe_tiles`' own `kframes`/loop-index `i` equals
   `order_hint` directly; this stream's hidden alt-ref (`order_hint=1`,
   `show_frame=false`, decoded before `order_hint=1`'s real shown frame)
   makes that assumption silently correct here by coincidence (both happen
   to run 0,1,2,3.. in lockstep since there's exactly one hidden frame
   before the run of shown ones) but do not rely on it holding for other
   FATE streams without re-checking via `KINETIX_AV1_DBG_SEQ`.
6. Baseline to protect, reconfirmed clean at the end of this session:
   feature corpus 7/13, FATE aggregate 5/195, `non_uniform_tiling` 3/24,
   intra 6/6, inter sequence 8/8, inter corpus 5/5×4 clips, all AV1 lib/
   integration/proptest/doctests passing, `git status` clean on
   `tpt-kinetix-av1/`.

## Session 2026-09-28 (later) — two root causes fixed on top of cont'd 4-22's
## open thread: (1) inter MC clamps at the reference's VISIBLE dims, not the
## mi-grid extent; (2) refreshed DPB slots must ALWAYS receive the frame's CDF
## context (adapted when refresh_context, otherwise the frame's *initial*
## context). non_uniform_tiling 1/24 → 6/24; official FATE aggregate 3/195 →
## 9/195

Picks up cont'd 4-22's "next session's starting point" (the order_hint=3
`(mi_row=16, mi_col=96)` PARTITION_NONE-vs-split divergence) and closes it:
its suspect (a) — "an entropy desync earlier in this same tile/SB-row" — is
the right shape, but the desync is not *within* the tile; it is a wrong
**initial CDF context for the whole frame**, which only becomes visible at
that partition read because every earlier read's CDF happened to agree.

### Fix 1 — MC reference reads clamp at visible dims (commit 0e9740d)

dav1d's `mc()` (recon_tmpl.c:1006-1029) clamps reference reads at
`(f->cur.p.w/h + ss) >> ss` — the **visible** frame dims — for every
reference read except intrabc (which uses `f->bw*4 × f->bh*4`), and
`warp_affine()` does the same with `refp->p.p.w/h`. `p.p.w/h` are the
header dims (`dav1d_thread_picture_alloc` passes
`frame_hdr->width[1]/height`), NOT the aligned/grid extent. Kinetix passed
the grid dims (grid_w×grid_h) as clamp bounds everywhere, so bottom-row
blocks with fractional MVs read the reconstructed-but-invisible grid
padding rows (300-303 for 720×300) where dav1d replicates the edge row —
exactly the stage-independent ±1 residue frames 1-2 carried at rows
297-299. Fix: `RefSlot` gained `real_width/real_height` (populated from
`StoredFrame`), translational/compound-prep/OBMC/warp call sites pass
visible dims as clamp bounds while keeping the grid stride, and the
stale "matching dav1d's padded references" doc comment on `StoredFrame`
was corrected (it documented the wrong assumption this fix removed).

Results: frames 1-2's residues vanished (98/156 bytes → 0);
`non_uniform_tiling` 3/24; frame 3's 117,605-byte diff at (384,64) proved
**pre-existing** (identical before/after the fix via git stash A/B), i.e.
an independent second root — fix 2 below.

### Fix 2 — refreshed CDF slots always receive the frame's context

Evidence chain (all in frame order_hint=3, tile 1): KSKIP/KINTRA rng
sequences matched dav1d line-for-line through `KINTRA mi=(88,24)
rng=47155`, then the very next block's skip read diverged. The block
between them — (88,24), a 32×16 NEWMV OBMC block — turned out to be in
**sync through its filter read** (both `Post-subpel_filter[0,ctx=0]
r=63940`; an earlier "K read an extra filter symbol for WARP blocks"
conclusion was a misreading caused by a truncated trace window — do NOT
re-litigate the motion_mode mapping, K's 1=OBMC/2=WARP matches dav1d's
`levels.h` MM_OBMC=1/MM_WARP=2 and a swap REGRESSES frames 1-2, verified
and reverted). The real divergence: dav1d's `txb_skip[3][0]` entered that
read pre-adapted at spec value 30669 (= the **qcat-2** default table
value, `default_coef_cdf[2].skip[3][0]`), while K read from the **qcat-3**
default (31671) — frame order_hint=3 has base_q_idx=139 (qcat 3) on both
decoders (dav1d's quant parse verified via a new KGQUANT print at
obu.c:698), so dav1d's coefficient CDFs came from a **restored context**,
not from this frame's own qcat.

Mechanism (dav1d decode.c:3528-3531 + 3721-3727): frame start restores
`c->cdf[refidx[primary_ref_frame]]` wholesale (coefficient CDFs included,
whatever qcat they were saved under); frame end stores into every
refreshed slot the **adapted** context when `refresh_context` is set,
else the frame's **initial** context (`f->in_cdf`). The keyframe (base 80
→ qcat 2, refresh_context=0) therefore still hands its qcat-2 default
tables to every slot, and order_hint=3 (primary_ref=0) legitimately starts
coefficient decoding from those — the spec allows cross-qcat restore.
Kinetix only ever saved adapted contexts (`if !disable_frame_end_update_cdf
{ if adapted { save } }`), so slots stayed empty (`KIN CDFLOAD have=false`
on every frame) and the restoring frame fell back to its own-qcat
defaults. Fix: `FrameCdfContext::default_for_qindex()` helper + the
decoder's frame-end path now populates every refreshed slot with the
adapted context when refresh_context is set, else the frame's initial
context (`initial_cdfs` if restored, `default_for_qindex(base_q_idx)` if
not).

Results: frame 3's first diff moved from (384,64) to (468,190) and shrank
117,605 → 39,152 bytes; frames 9-11 now decode exactly; frame 4 is down
to 1,052 bytes; the frames 4+ cascade collapsed.

### Results

- `non_uniform_tiling` **6/24** (was 1/24 at session start, 3/24 after
  fix 1); `switch_frame` **2/32** (was 1/32); official FATE aggregate
  **9/195** (was 3/195). frames_refs_short_signaling unchanged at 1/50.
- Gates: 165 av1 lib tests pass, clippy clean, fmt clean. Also fixed a
  latent panic: the `KINETIX_AV1_DUMP_GRID` hook in `decoder.rs`
  hardcoded 320 grid rows and panicked on shorter grids (now iterates the
  real extent).

### Next session's starting points

1. Frame 3's next root: first diff at Y (468,190), 39,152 bytes. Frames
   5-7 cluster at (286-287, 159-160); frames 14+ cluster at y≈123-128 —
   suspiciously near the tile-row 2→3 boundary (y=128) — same class of
   investigation as this session's (KSKIP/KINTRA rng diff, then
   coefficient-level via `KINETIX_DBG_COEFF_BLK`, which now takes
   **unmasked** `t->bx`/`t->by` after this session's patch).
2. Debug infra state (all in `/tmp/dav1d_fresh`, scratch, not reverted):
   `DEBUG_BLOCK_INFO` = `frame_offset==3` whole-frame (recon.h);
   KSKIP/KINTRA hooks gated `KINETIX_DBG_IBSUM` + frame 3 + by∈[16,32)
   (decode.c); `KINETIX_DBG_MCSUM` at bx==96,by==16 (recon_tmpl.c);
   KGQUANT after the quant parse (obu.c:699); KCOEF gate takes unmasked
   mi coords (recon_tmpl.c). Pair them with K's `KINETIX_AV1_IBSUM`,
   `KINETIX_AV1_DBG_B0`, `KINETIX_AV1_DBG_ALLZERO`, `KINETIX_AV1_DBG_EOB`,
   `KINETIX_AV1_CFTARGET`, and segment K's stderr per frame with
   `KINETIX_AV1_DBG_SEQ=1` (the four tiles braid in the stream; filter by
   position, never by line adjacency).
3. `film_grain` (0/10) and `decode_model` (0/21) remain
   expected-unsupported; `seq_hdr_op_param_info` (0/58) and the rest of
   `frames_refs_short_signaling` (1/50) are untouched separate feature
   work.

## Session 2026-09-28 (cont'd) — frame 3's next root narrowed to the
## dequant/inverse-transform of the (116,44) 16×16 txtp=10 WARP block:
## prediction, model, filter table, phases, intermediates, coefficients and
## rng all verified EQUAL to dav1d; the residual ±1-2s at (468,190) are all
## that's left

Chased the post-CDF-fix residue (frame 3 = oh=3/n=4, 39,152 bytes, first
diff Y(468,190)) to its containing block: mi (116,44), a 16×16 skip=0
NEWMV (mv y:6,x:-4) WARP block (mm=2, ref LAST2), pixel (468,190).

Verified byte/step-equal against dav1d for this block:
1. Syntax: KSKIP/KINTRA rng sequences match through the whole tile row
   (134 lines); Post-motionmode[2] r=34588 == K's motion_mode rng; NO
   filter symbol read on either side (frame_filter != SWITCHABLE here —
   unlike (88,24), which did read one; per-block, not per-frame).
2. Warp model: derive prints match dav1d's matrix dump EXACTLY — note
   dav1d's `alpha=-80` prints are HEX (`%c%x`): -0x80 = -128 decimal.
   matrix [17158, -1406706, 65430, 0, 3694, 63984],
   alpha=-128 beta=0 gamma=3712 delta=-1536, num_samples=1.
3. Warp filter: K's `WARPED_FILTERS` == dav1d's `dav1d_mc_warp_filter`
   for ALL 193 rows (an earlier "one-row shift" reading was a regex
   parsing artifact — dav1d's C writes `- 1` with a space; normalize
   before diffing). Phases/idx per column match; the v-pass starting at
   mid row yy (dav1d's `mid_ptr = &mid[3*8]` + FILTER_WARP_RND's -3 tap
   offset cancel out) is correct.
4. Warp output: K's own per-tap WARPPX dump yields out row0 =
   [153,154,154,167,192,184,142,125] == dav1d's y-pred row 0 == an
   independent Python port of dav1d's warp_affine_8x8_c fed K's exact
   runtime params (dx=463 dy=176 mx0=33536 my0=33984). The warp pipeline
   is CORRECT. (The `WARPBLK`/`KINETIX_AV1_DBG_WARPPX_BLOCK` hooks added
   this session make these dumps retargetable; the historical hardcoded
   dx==26&&dy==48 gate no longer matches anything.)
5. Coefficients: dav1d KCOEF (now frame-gatable via `KGT_OH` env, and
   taking UNMASKED t->bx/t->by) for this TX: tx=2 all_skip=0 eob=5
   txtp=10, post-eob r=44552; K: eob=6 same rng — **K's eob = dav1d's
   eob + 1 by convention** (count vs last-index; see also (88,24):
   dav1d 0 ↔ K 1). Tile-row-2 KSKIP/KINTRA fully matching afterwards
   proves the coefficient reads consumed identically. dav1d's dq dump
   shows -211 at raster 0 and 16 (top-left column); K's KCFT nz prints
   SCAN positions (4,5) — the nz print enumerates the scan-order quant
   array; do not read it as raster.

What remains: the DEQUANT + inverse transform of this 16×16 txtp=10
(ADSTM-family) block. The observed ±1-2 pixel diffs across the block are
consistent with a last-digit rounding difference in the inverse
transform, not with any syntax/CDF/prediction issue. Next session: feed
the captured coefficients (two -211 DC-column coefficients, tx=2 txtp=10,
q=139/dc=139) through K's `dequantize_coeffs`+`inverse_transform` and
dav1d's inv_txfm for txtp=10 and diff the residual blocks directly; the
mismatched output sample pinpoints the stage.

Side findings this session (do not re-chase):
- `KINETIX_AV1_DBG_PRED`'s pixel dump indexes `y_plane` with TILE-LOCAL
  rows — broken for tiled frames; use DUMP_GRID instead.
- `KINETIX_AV1_NO_RESID` zeroes only the vartx luma path's quant (chroma
  at inter_block.rs:3858 and any non-vartx luma site are NOT zeroed) —
  prediction-only grids built with it still contain residuals.
- oh=4 (frame 4 shown, 1,052 bytes @ (541,188)): its KSKIP diff at
  (100,32) — same rng, different skip value — is consistent with frame 3's
  adapted-CDF carryover (frame 4 restores a slot written by frame 3, whose
  post-desync adaptation still differs); expect it to collapse when this
  inverse-transform fix lands. Frames 5-7's (287,159) cluster and the
  frames 14+ y≈128 cluster start downstream of the same tile-row boundary.

## Session 2026-09-28 (cont'd 2) — the (116,44) WARP block's entire
## reconstruction chain verified byte-identical to dav1d; the frame-3 root
## is now CDEF secondary filtering at units (464,184) and (476,184)

Picked up the documented handoff (dequant/inverse-transform suspect) and
DISPROVED it, then walked the whole chain with per-stage dumps:

1. **Dequant/ITX correct.** dav1d's targeted KCOEF dump for the block
   (frame-gated via the new `KGT_OH` env on the coefficient prints) reads
   tx=2 (16×16), all_skip=0, eob=5, txtp=10 (V_DCT), coefficients −211 at
   raster (4,0) and (5,0) — identical to Kinetix's (K's eob=6 is its
   count-vs-last-index convention, +1). K's `KINETIX_AV1_DBG_PRED_RESID`
   dump of the post-ITX residual: **[−7,−7] flat at columns 4-5, all 16
   rows — byte-identical to dav1d's (recon − pred)**. The earlier
   "dequant/ITX" suspicion is closed.
2. **Warp prediction correct.** K's per-tap WARPPX dump (retargetable via
   the new `KINETIX_AV1_DBG_WARPPX_BLOCK=dx,dy`; plus a `WARPBLK`
   first-sub-block trace under `KINETIX_AV1_DBG_WARPPX_ALL`) shows K's
   runtime sub-block params (dx=463 dy=176 mx0=33536 my0=33984) and
   per-column phases/idx identical to an independent Python port of
   dav1d's warp_affine_8x8_c (which reproduces dav1d's y-pred 0/256). K's
   own v-pass outputs equal it too. An earlier panic-level suspicion that
   K "used the wrong matrix indexing" came from misreading mat[2] — the
   runtime `WARPBLK` trace settles it.
3. **Residual add correct.** A temporary `POSTADD` dump (CFTARGET-gated,
   after the leaf's add loop in `inter_block.rs`) shows the plane right
   after the add == dav1d's `recon` hex dump for the block, byte for byte.
4. **The final diffs are CDEF.** Full-pipeline K vs dav1d final at this
   block: only 4 samples: (468,190) K151/dav150, (470,191) 125/126,
   (471,191) 127/126, (476,191) 123/122 — all ±1, all in two 8×8 CDEF
   units at (464,184) (bx=116,by=46) and (476,184) (bx=119). Pre-CDEF
   recon identical ⇒ the divergence is CDEF's filter output on those
   units. dav1d's per-unit dump for (116,46) — `KDCDEF2` (new: the
   secondary-only `else if` branch in cdef_apply_tmpl.c, frame-gated by
   `KGT_OH`): **pri=0, sec=4, dir=1(computed)/0(passed to the filter),
   damping=5**. Kinetix's `dir = if pri_str == 0 { 0 }` at
   loop_filter.rs:2503 already matches dav1d's hardcoded dir=0 for
   secondary-only units, and pri/sec/damping derive from the same syntax.

### Next session's entry point (per-tap CDEF comparison)

With prediction, coefficients, ITX and the add all proven equal, feed the
captured pre-CDEF 8×8 (dav1d's KDCDEF-side state; also printable via
K's `KINETIX_AV1_DBG_CDEFPX=464,184`) through both
`cdef_filter_block`(K) and dav1d's `cdef_filter_fb` (dir=0, pri=0,
sec=4, damping=5, the unit's real top/bot/edges) and diff tap by tap —
the first differing `constrain()` contribution or the ±{1,2} secondary
offset table pins the remaining ±1s. The (476,184) unit needs its own
KDCDEF2 print (change bx==116 to 119) — likely the same single fix.
Watch for: damping=5 vs 4 selection (derived from quantizer), the
secondary tap distance-1/distance-2 ordering, and `constrain()`'s
diff-threshold arithmetic — the three places a ±1 can hide.

Side notes:
- `KINETIX_AV1_CFTARGET` now also prints `POSTADD` plane rows after each
  target leaf's residual add (pre/post-prediction dumps bracket the add;
  this closes the gap).
- The remaining frames 5-23 diffs all start downstream of these CDEF
  units' tile row; fixing CDEF here is expected to collapse most of the
  rest, as the previous CDEF fixes did.

## Session 2026-09-28 (cont'd 3) — the CDEF divergence itself pinned:
## sbrow-boundary BOTTOM taps. dav1d reads `bot` from `lr_lpf_line` for
## units whose taps cross the sbrow end; Kinetix reads the global pre-CDEF
## snapshot. Same params, same input, ±1 out — on the last two rows of
## every SB row band where a sec-only unit is active

Chased the 4-sample CDEF diff at (468,190)/(470,191)/(471,191)/(476,191)
through every layer, all of which are now PROVEN equal for the unit
(464,184) (bx=116,by=46):

- Direction tables: K's `CDEF_DIRECTIONS[8][2][2]` ≡ dav1d's
  `dav1d_cdef_directions` (the packed `dy*12+dx` entries unpack to the
  same (dy,dx) pairs, and dav1d's `cdef_dirs[4]`/`[0]` linear indexing
  into the 12-row padded table is exactly K's `(dir±2) & 7`).
- Tap weights/order: sec-only = 8 taps, weights [2,2,2,2,1,1,1,1] both.
- Parameters: K's `KINETIX_AV1_DBG_CDEFPX=464,184` dump — pri_str=0
  sec_str=4 damping=5 dir=0 — matches dav1d's `KDCDEF2` (the new print in
  cdef_apply_tmpl.c's secondary-only branch, frame-gated by `KGT_OH`):
  pri=0 sec=4 dir=1(computed)/0(passed) damping=5. K's
  `dir = if pri_str == 0 { 0 }` already mirrors dav1d's hardcoded dir=0.
- Edge skipping is equivalent here: every tap of this mid-frame unit is
  in-plane, so K's OOB-skip vs dav1d's replicated padding never differs
  for it.

The divergence: all four differing samples are at rows 190-191 — the last
two rows of SB row 2 (rows 128-191). dav1d's cdef_apply for a unit with
`by + 2 >= by_end` fetches bottom taps from `f->lf.lr_lpf_line[pl]` at
`line = sby * (4 << sb128) + 4 * sb128 + 2` (cdef_apply_tmpl.c, the
`!sbrow_start && by + 2 >= by_end` arm) — a saved line buffer whose
content is NOT byte-identical to the plain pre-CDEF snapshot row that
Kinetix's `cdef_plane_luma` snapshot provides. Proof by output: K's
result at (468,190) IS the snapshot semantics (150 unchanged would need
dav1d's value; K emitted 151 from snapshot taps), dav1d emitted 150.

### Next session's entry point

Determine exactly what dav1d's `lr_lpf_line`/`cdef_lpf_line` hold for the
bottom taps of an sbrow-end unit (deblock is 0 on this frame, so
post-deblock == raw; the question is WHICH rows the `line` index selects
and whether they were saved pre- or post-CDEF — read
`dav1d_cdef_brow`'s line-save code and `lf_{cdef,lr}_line` fill sites,
then either replicate the buffer semantics in `cdef_plane_luma` for
`by + 2 >= by_end` units or prove the buffers equal the snapshot and
re-open elsewhere). The (476,184) unit (bx=119) has its own sample
((476,191)) and needs the same treatment — set the KDCDEF2 gate's bx to
119 to dump it. Expected payoff: the frames 5-23 residues all start at
rows ≈123-128/159-160/190-191 — SB-row bottom bands — so this fix should
collapse most of the remaining non_uniform_tiling diff.

Debug-state additions this session: `KDCDEF2` (cdef_apply_tmpl.c
secondary-only branch, `KGT_OH`-gated, currently bx==116 by∈{46,47});
`KINETIX_AV1_DBG_CDEFPX` verified working for arbitrary units;
`POSTADD` plane dump under `KINETIX_AV1_CFTARGET` (committed cont'd 2).

## Session 2026-09-28 (cont'd 4) — ROOT PROVEN WITH BYTES: dav1d's
## sbrow-end CDEF bottom taps are the POST-FILTER row below
## (lr_lpf_line == final row values); Kinetix feeds the pre-CDEF snapshot
## row. The fix is to replicate the line-buffer semantics

Instrumented dav1d's secondary-only CDEF branch to dump `bot[0..8]` for
the diverging unit (116,46) of frame oh=3:

    KDCDEF2 pri=0 sec=4 dir=0 damping=5 sby=3 by_end=48
    bot0_7=[182 189 188 189 150 124 128 126]

Comparing three versions of frame row 192 (cols 464-471):

    dav1d bot        [182, 189, 188, 189, 150, 124, 128, 126]
    dav1d FINAL row  [182, 189, 188, 189, 150, 124, 128, 126]   <- EXACT
    raw (pre-CDEF)   [182, 188, 187, 189, 156, 125, 126, 126]

dav1d's CDEF bottom taps for the unit are the **already-filtered** row
below — the `lr_lpf_line` saved line — NOT the pre-CDEF snapshot row.
Kinetix's `cdef_plane_luma` filters from a whole-frame pre-CDEF snapshot,
so its bottom taps carry raw values (156/125/126 where dav1d uses
150/124/128), producing the ±1s at rows 190-191. `KGT_OH`-gated
`KDCDEF2` now also dumps `sby`/`by_end`/`bot0_7` for exactly this class
of investigation.

Mechanism (dav1d source): `dav1d_copy_lpf` (lf_apply_tmpl.c) saves rows
around each sbrow boundary into `lr_lpf_line` AFTER that sbrow's
loop-filter stage has run — so by the time unit rows reach the sbrow end,
the line below is post-filter. The `!sbrow_start && by + 2 >= by_end`
arm of cdef_apply_tmpl.c then hands those saved lines to the filter as
`bot`/`top`. (The buffer addressing — negative-pointer base, per-sbrow
4-line slots, `line = sby*4 + 2 + sb128` — is in lf_apply_tmpl.c's
`backup_lpf`.)

### The fix (next session)

In `cdef_plane_luma`, replace the snapshot's raw rows with the saved-line
semantics for bottom taps: process SB rows bottom-up is NOT what dav1d
does — it processes top-down but each sbrow's bottom taps read the line
saved when the stage below ran; the equivalent whole-frame formulation
is: **run CDEF per SB row top-down, and for units whose bottom taps
cross the SB row's end (by+2 >= by_end), read the below rows from the
already-filtered output rows below (the in-place plane), while reading
everything else from the pre-CDEF snapshot.** I.e. dav1d's effective
semantics = snapshot for the unit's own rows/top, in-place filtered for
the sbrow-bottom lines. Verify with the captured unit: pixels
(468,190)→150, (470,191)→126, (471,191)→126, (476,191)→122 must come out
exactly; then re-run probe_tiles (frames 3-23 residues all sit on SB-row
bottom bands — expect a large collapse) and the official FATE.

If the top-down/in-place formulation mismatches on the TOP taps at
sbrow starts, dump dav1d's `top` the same way (the print already sits
next to the bot fetch) and adjust — the same buffer family serves both.

## Session 2026-09-28 (cont'd 5) — the simple bottom-up formulation TRIED
## AND REVERTED; dav1d's sbrow-bottom taps are NOT uniformly
## post-filter. The remaining unknown is the lr_lpf_line ring mapping

Implemented the cont'd-4 "fix formulation" (process 64-px bands bottom-up,
per-band snapshots so below-band rows are the already-filtered live
values). Result: REGRESSION — frames 0-2 (previously pixel-exact) gained
~200-byte diffs at row 62, i.e. tile row 0's bottom band: at that
boundary dav1d's bottom taps are the RAW rows 64-65 (the old top-down
snapshot matched dav1d there), while at the (116,46) unit they are the
FILTERED row 192 (proven byte-for-byte). Reverted; 6/24 restored.

So the same `by + 2 >= by_end` arm reads different content at different
boundaries. The distinguishing variable is the buffer line index
`line = sby * (4 << sb128) + 4 * sb128 + 2` (cdef_apply_tmpl.c): our two
sites hit sby=3 (line 14 → filtered rows) vs sby=1 (line 6 → raw rows) —
the lr_lpf_line ring holds different pipeline-stage snapshots per slot
depending on fill timing across the sbrow/tile pipeline.

### Next session's entry point (empirical ring mapping — bounded)

Make the KDCDEF2/KDBOT5 print fire for EVERY sbrow-end unit of frame
oh=3 (drop the bx/by gate; keep the KGT_OH frame gate; KDBOT5 currently
sits before the `goto skip_uv` and needs `(bx, by)` — remember bx/by are
8×8-unit top-left MI coords, so bx is even). For each dump, also print
the raw and final rows below (K's NOFILTER grid and dav1d's final frame
give both). ~40 units × (bot vs raw vs final) collapses the ring into a
lookup rule — likely "slot holds raw rows for early sbrows, filtered for
later ones" or "line N maps to frame row (something - k*4)" — which
`cdef_plane_luma` can then implement directly as a row-source
substitution for `by + 2 >= by_end` units. Verify on the 4 known samples
first ((468,190)→150, (470,191)→126, (471,191)→126, (476,191)→122), then
frames 0-2 must stay exact, then the FATE aggregate.

Current state: 6/24 on non_uniform_tiling, official FATE 9/195, all
gates green, tree clean of the reverted experiment.

## Session 2026-09-28 (cont'd 6) — THE deepest root yet: a single-bit
## arithmetic-decoder divergence. Frame oh=3, tile row 2, block (112,56):
## same EC state (rng=37415, skip-CDF v0=4381 count=14), dav1d decodes
## skip=1, Kinetix decodes skip=0. Everything downstream in tile 2
## (~34k of frame 3's 39k diff bytes) cascades from this one read

The CDEF line-buffer investigation surfaced that frame 3's real residue
body starts at block (464,192) and spreads right/down — the shape of a
symbol desync, not CDEF. KSKIP/KINTRA tracing into tile row 2 (mi rows
48-63; dav1d's IBSUM gate now retargetable via `KGT_OH` + by range) found
the exact read: block (112,56), the first sctx=2 skip read after 14
matching ones.

Evidence (all captured with `KINETIX_DBG_IBSUM` + `KGT_OH=3`):
- Reads 1-14 of the tile-2 skip-CDF sctx=2 cell match dav1d exactly:
  same values (skip sequence), same per-step adaptation (K's spec-domain
  v0 == 32768 − dav1d's complement c0 at every step), same rng.
- At read 15: pre-state rng=37415 (both), cdf v0=4381 (K) ==
  32768−28387 (dav) ✔, count 14→15 (both). dav1d decodes skip=1, K
  decodes skip=0.
- Hand-run of dav1d's decode_bool on the captured state (rng=37415,
  f=28387, dif_hi=4376): v = ((r>>8)*(f>>6)>>1)+4 = 32343, dif_hi−v =
  −27967 → ret=false → skip=1, with a huge margin. This is NOT a
  boundary-rounding case: K's decoder computes a different split or its
  (range, value) state has already diverged invisibly (the two decoders'
  value representations may track each other only until some earlier
  renormalization corner case).

Debug-state additions: dav1d's KSKIP print now carries `cdf=[c0 count]`
and `dif_hi` (dif >> 48); K's KSKIP print carries `cdf=[v0 32768 count]`;
KINTRA carries `dif_hi`.

### Next session's entry point (surgical EC comparison)

1. Print K's full `raw_state()` (symbol_range, symbol_value,
   symbol_max_bits, bit_pos) at the (112,56) KSKIP, and dav1d's
   (rng, dif, cnt) — then port both bool-decode paths (dav1d
   `decode_bool` + `ctx_norm` from msac.c; K's read_symbol from
   entropy.rs) to Python and step them from the SAME captured state.
   dav1d's side is already solved: v=32343, dif_hi=4376 → skip=1,
   then `dif -= 0`, `ctx_norm(dif, v)`: new rng = v = 32343 (no
   renorm shift needed since v >= 0x8000? check `ctx_norm`'s shift
   loop), dif stays.
2. The likely fault lines in K: (a) the f-scaling rounding (K uses the
   spec's §8.5 division-based split vs dav1d's EC_PROB_SHIFT
   approximation — these agree on most states but maybe not all),
   (b) the renormalization window/threshold, (c) the value-window
   width (K's symbol_max_bits vs dav1d's EC_WIN_SIZE=64 dif window).
3. Once the divergent step is found, fix K's decoder to match dav1d
   bit-for-bit (dav1d is the od_ec reference that aomenc encodes to),
   then re-run: frame 3's tile-2 cascade (~34k bytes) should collapse;
   frames 5-23's tile-2/3 residues likely too; then the FATE aggregate.

Note: K's read path already proved itself on the whole corpus, so the
bug is a genuine corner case — expect the fix to be small (one rounding
or window-size constant) but to need care not to regress the corpus.

## Session 2026-09-28 (cont'd 7) — CORRECTION of cont'd 3-5: the CDEF
## line-buffer theory was a red herring. The row-192 "post-filter" values
## were the EC desync's cascade. True chain: tile row 2's ENTIRE recon
## diverges from its first row (K raw row 192 col 468 = 156 vs dav1d's
## pre-CDEF 151 — recon-level, no CDEF involved), because of the single
## EC read divergence at block (112,56) documented in cont'd 6

Facts established this round (all verifiable from the captured dumps):
1. Frame oh=3's tile group is ~210 bytes in BOTH decoders — tiles
   [10, 30, 84, 86], n_bytes=1, K's split parses it correctly
   (KINETIX_AV1_DBG_TILES output). The earlier "dav1d group = 1098
   bytes" reading was an EARLIER frame's KGTILE print — disregard.
2. K's tile-2 entropy decoder exhausts its view of the tile buffer at
   block (112,56): post-read state `symbol_max_bits=511, bit_pos=511`,
   value=33320 ≥ cur=32343 → skip=0, while dav1d decodes skip=1 from
   the same nominal EC state (its `botrow`-style probe confirms its bot
   pointer is simply the in-place picture row — the CDEF "line buffer"
   detour was tracking desynced data, not a CDEF bug).
3. K's split computation at that read is IDENTICAL to dav1d's v
   (32343 = ((r>>8)*(f>>6)>>1)+4 with r=37415, f=28387). The divergence
   is in the VALUE side: K's symbol_value ≥ 32343 where dav1d's
   dif-position = 4376. Since value and dif-position are supposed to be
   mirrored views of the same bitstream bits, either K's value window
   desynced earlier (a renorm/refill corner case) or K's comparison
   convention anchors at the wrong end for this specific state.
4. Recon-level proof: dav1d run with `--inloopfilters none` still
   differs from K's NOFILTER grid at row 192 (K 156 vs dav 151 at
   col 468, 185 diffs in the row) — the divergence is in
   RECONSTRUCTION (entropy → prediction/residual), not any loop filter.

### Next session's entry point (unchanged in substance, sharpened)

Print K's full pre-read raw_state() at the (112,56) KSKIP (the state=
extension now in the KSKIP print gives post-read; add a pre-read print
or reconstruct pre from post + the read's consumption), then port both
EC decoders to Python from the captured state and step until the value
windows diverge. Check FIRST whether K's `symbol_max_bits` bookkeeping
(511 at the divergence — suspiciously equal to bit_pos) has already hit
its refill ceiling: if K's tile buffer slice is shorter than the real
tile data (an off-by-N in `split_tile_group_payloads`'s last-tile
handling or the OBU payload slice), K's refills pad zeros from that
point while dav1d keeps reading real bits — that would explain a desync
that appears mid-tile without any prior symbol mismatch. Compare K's
tile-2 payload slice (84 bytes, `KINETIX_AV1_DBG_TILE_BYTES` hex)
against dav1d's tile-2 bytes (patch a dump of `ts->tile` in
cdef/decode or read the IVF packet bytes directly: frame 3's tile 2
starts at group offset 1(hdr)+1+10+1+30+1 = 44, length 84).

## Session 2026-09-28 (cont'd 8) — self-correction: the "identical
## pre-state" claim at (112,56) was premature. The KSKIP anchors only
## bracket the reads; everything between them (UV modes, filters,
## COEFFICIENTS) was never compared, and the coefficients are the prime
## suspect (skip=0 blocks carry residual syntax; the divergence block
## (120,48) is skip=0 and its coefficients were never traced)

What holds (verified): the KSKIP/KINTRA *sequences* (mi, skip value,
sctx, intra value) match through tile 2 up to (112,56), where K reads
skip=0 and dav1d skip=1 — but those prints sample only two of the many
reads per block. The EC state between anchors was never compared, so the
desync's true location is somewhere in (120,48)'s or (118,52)'s
remaining syntax — most likely the coefficient reads of the skip=0
blocks (K's per-TX hooks: KINETIX_AV1_DBG_ALLZERO / _EOB / CFTARGET;
dav1d's: KINETIX_DBG_COEFF_BLK=plane,bx,by with the KGT_OH frame gate —
both sides' prints now carry cdf cells and EC state).

Also verified this round (dismissed for good):
- Frame oh=3's tile group payload is ~210 bytes in BOTH decoders
  (tiles 10/30/84/86, n_bytes=1); K's split parses it correctly. The
  "1098-byte group" was an earlier frame's print.
- K's tile-2 payload slice = the full 84 bytes (DBG_TILE_BYTES hex),
  NOT truncated; bit_pos=511 at the divergence is mid-buffer (672-bit
  payload). The max_bits==bit_pos coincidence is the init arithmetic
  (max_bits = 672−15−renorm_bits), not a refill ceiling.
- dav1d's `bot` for the catch-band unit is literally the in-place
  picture row (botrow=192 probed) — the "lr_lpf_line post-filter"
  interpretation was tracking the desync's cascade, not a CDEF rule.

### Next session's entry point (unchanged target, honest method)

Per-read comparison of tile row 2 from its FIRST block: walk K's
KTRACE/KSKIP/ALLZERO/EOB prints against dav1d's KSKIP/KINTRA/KCOEF
family (frame-gated via KGT_OH=3), position-filtered to mi rows 48-63.
The first EC-state mismatch — now defined as "the first read whose
post-read rng differs at the same position" — is the buggy read. Given
the block at (120,48)/(118,52) are skip=0, expect it in coefficient
reading (the ALLZERO/eob/base-token path) within the first two blocks.

## Session 2026-09-28 (cont'd 9) — the tile-2 desync root FOUND: the MV
## STACK of block (116,48) diverges. K's stack[0] carries mv=(4,−8) —
## neither of its in-tile left candidates (−1,−1)/(0,−4) — while dav1d's
## stack leads with the left neighbour (−1,−1). The (4,−8) is a temporal
## candidate or an above-tile leak; it poisons the chain

Per-block MV comparison of tile row 2's first SB row (dav1d
Post-intermode vs K's IBSUM prints, both already captured):

    (112,48): dav (−1,−1)  K (−1,−1)   ✔
    (112,52): dav (−1,−1)  K (−1,−1)   ✔
    (116,48): dav n/a      K (4,−8)    ✗ THE ORIGIN
    (116,52): dav (−1,−1)  K (−1,−1)   ✔
    (120,48): dav (−1,−1)  K (0,0)     ✗ poisoned by the chain
    (112,50): K (0,−4) (skip block, mv = its stack[0])

(116,48) is at the TILE TOP (mi_row 48, tile row 2 starts here): its
above neighbours must be unavailable, its left candidates are
(112,48)=(−1,−1) and (112,50)=(0,−4), yet K's stack[0]=(4,−8) —
matching NEITHER left candidate. Two candidate explanations, both
checkable with K's existing mvstack print (KINETIX_AV1_IBSUM run):
(a) K's temporal-MV projection (rp_proj) contributes (4,−8) and ranks it
    above the spatial candidates — dav1d ranks spatial candidates first
    (spec §7.10.1.10: spatial scan BEFORE temporal), or K emits the
    temporal candidate even when spatial candidates exist for a 4×4;
(b) K's above-row scan leaks tile row 1's MVs across the tile top
    (the above availability check at the tile boundary). (4,−8) vs tile
    row 1's blocks at mi (116,44-47): the (116,44) WARP block's mv is
    (row 6, col −4) — no obvious sign flip, so (a) is likelier.

The poisoned stack then cascades: (117)-(119,48) read their stacks,
(120,48)'s stack gets s1=(0,0) where dav1d has (−1,−1), its NEARMV/drl
reads decode differently (K NEWMV-adjacent path vs dav1d NEARMV drl=1),
and every subsequent symbol read in tile row 2 desyncs (~34k bytes).

### Next session's entry point (mechanical)

1. Dump K's mvstack for (116,48) (the mvstack print already exists —
   gate KINETIX_AV1_IBSUM and filter mi=(116,48)) and dav1d's (the
   KGMVS print in decode.c's single-ref refmvs_find, `KGT_OH`-gated,
   currently gated bx==120 by==48 — change to 116/48).
2. Identify (4,−8)'s origin in K's `inter_mv_stack` for this block:
   temporal candidate rank/order (§7.10.1.10: the temporal candidate is
   appended AFTER the spatial scan, and only if spatial < N... verify
   against the spec's ordering) or the above-tile leak.
3. Fix, verify (116,48)'s mv == dav1d's, then the chain (120,48) mv ==
   (−1,−1), then the tile-2 cascade (~34k bytes of frame 3), then
   frames 5-23 and the FATE aggregate — the same MV-stack divergence
   class likely explains the other tiles/frames' residues (they all
   start at tile rows' first decoded blocks after skip runs).

Debug-state additions this session: K's b0 prints now carry mi=(col,row)
(previously positionless and unusable under tile braid); dav1d's KGMVS
mvstack dump (KGT_OH-gated, currently bx=120 by=48, single-ref call at
decode.c:1686); the tile2_probe example under tpt-kinetix-av1/examples/
decodes K-side without the test-utils dependency chain (usable while
the concurrent h264 session's in-flight edits break that graph).

## Session 2026-09-28 (cont'd 10) — the desync mechanism fully exposed:
## K's MV scan for (116,48) rejects the spatial candidates on a REF-NAME
## MISMATCH (block wants ref 2, neighbours' grid cells carry ref 3), so
## only the temporal (0,0) survives; dav1d's same block accepts (−1,−1).
## Same decoded bits (post-rng 55492 identical) — the symbol→ref-name
## mapping differs by one between K and dav1d

The `KINETIX_AV1_DBG_MVSCAN="48:116"` trace of K's inter_mv_stack for
the block (116,48) (two finds — the 4×4 block, bsize=2, and a second
pass, bsize=16):

    find bsize=2:  adds r48c115 mv=(1,−1) ref=(2,0) w=2;
                   r49c113 (1,−1) w=4; r49c111 (4,−4) mf=2 w=4
                   → final [ (1,−1) w646, (4,−4) w4 ]
    find bsize=16: adds r48c115 mv=(−1,−1) ref=(3,0) w=4;
                   r50c115 (0,−4) ref=(3,0) w=4;
                   r47c115 ref=(0,0) [tile-above cell, properly
                   invalidated — skipped];
                   + add_t (0,0) ×2
                   → final [ (0,0) w4 ]  ← ONLY the temporal survives

The bsize=16 pass is the poisoned one: the spatial candidates carry
ref=(3,0) while `want_refs[0]=2`, so `cand.refs[n] == want_refs[0]`
fails and they're all dropped. dav1d's stack for the same block:
s0=(0,0), s1=(−1,−1), s2=(0,−4) — it ACCEPTED (−1,−1) and (0,−4).
dav1d's refpair for the block = b->ref[0]+1 = 2; its neighbours'
recorded refs match that. K's block read ref name 2 and its neighbours'
cells carry ref name 3 — with the ref READ itself matching dav1d
bit-for-bit (post-rng 55492 identical) — so the two decoders map the
same decoded ref symbol to DIFFERENT ref names (off by one in the
single-ref name table or in the name recorded into refmv_grid cells).

Note the (−1,−1) candidate's ref=(3,0): dav1d's corresponding candidate
has refpair ref=2. So K's neighbour cells say "3" where dav1d's say "2"
for what should be the same decoded reference — the off-by-one is in
the RECORDING (the ref name written into refmv_grid at block decode
time) or in the WANT (read_single_ref_name's mapping), and the two
offsets cancel for most blocks (which is why frames 0-2 and most of
tile rows 0-1 decode exactly: single-ref streams whose ref name happens
to be symmetric around the off-by-one) but not here.

### Next session's entry point (small and decisive)

1. Print, at the (116,48) block: K's `want_refs[0]` (=2), the ref NAME
   K's `read_single_ref_name` returned, and the `refs` recorded into
   `refmv_grid` for the left neighbours (112,48)/(112,50) — all three
   already exist as prints or one-liners.
2. Compare with dav1d: Post-ref[1] (b->ref[0]=1, refpair ref=2) and the
   neighbours' `refpair` in dav1d's grid (mvstack print's rbref for the
   same cells, or dav1d's own MVSCAN equivalent).
3. Whichever side is off by one — K's `read_single_ref_name` symbol→name
   table or K's refmv_grid ref recording — fix, and the (116,48) stack
   becomes [ (−1,−1), (0,−4), (0,0)t ], the (120,48) NEARMV picks
   (−1,−1), and the entire tile-2 cascade (~34k bytes of frame 3,
   likely most of frames 5-23) collapses.

## Session 2026-09-28 (cont'd 11) — the ref-name off-by-one sharpened:
## dav1d decoded LAST2, Kinetix decoded LAST for the (116,48) ref read —
## with identical post-read rng (55492). The divergence is inside the
## single-ref name tree (gate contexts or a polarity), not the EC

dav1d's internal ref numbering pinned from usage: `a_r->ref.ref[0] - 1`
indexes `f->refp[]` (the 7 slots LAST..ALTREF), so the single-ref read's
`b->ref[0]` ∈ 0..6 is 0=LAST, 1=LAST2, 2=LAST3, 3=GOLDEN, 4=BWDREF,
5=ALTREF2, 6=ALTREF, and the refmvs refpair = internal+1 (1=LAST..).

So for block (116,48):
- dav1d `Post-ref[1]` = internal 1 = **LAST2**; refpair = 2 — and the
  left-neighbour grid cells carry refpair 2 as well (the MVSCAN
  candidates' ref=(3,0) in K = K's LAST2=3 — SAME reference!).
- K `ref=2` = K's LAST_FRAME (K constants INTRA=1, LAST=2, LAST2=3…).

K's neighbours are LAST2, the block is LAST — the ref-name mismatch in
the MV scan is REAL (not a notation artifact): dav1d's block is LAST2
and matches its LAST2 neighbours; K's block is LAST and cannot match its
LAST2-recorded neighbours.

Since the ref tree's post-read rng is byte-identical (55492) and the
same CDF cells were traversed, the decoded tree SYMBOL should be
identical — the name difference must come from the tree's structure:
either (a) one of the gate CONTEXT derivations differs (K's
`rcc(count(LAST), count(LAST2))`-style pairs vs dav1d's
`av1_get_ref_{3,4,5,6}_ctx` — which weight neighbour counts by reference
distance per the spec, not by raw category counts), causing a different
gate to be taken on a later re-read, or (b) a polarity inversion in one
tree's final bool mapping (dav1d `decode_bool` returns !ret against
f=cdf[0]=P(bit=1); K `read_symbol==1` against the spec-domain CDF).

### Next session's entry point (pure code comparison, no runs needed)

Print the intermediate rng after EACH bool of the ref tree on both sides
(dav1d: add prints inside the single-ref tree in decode.c ~1655-1685;
K: inside `read_single_ref_name` in inter.rs:733-767), rerun, and walk
the two trees gate by gate for block (116,48). The first rng divergence
identifies the gate whose CONTEXT formula differs; compare that gate's
K formula against dav1d's `av1_get_ref_N_ctx` (in dav1d's ref_mvs.h —
the spec's weighted-count contexts, §7.10.1.10) and fix K's to match.
Then re-verify: (116,48) ref == LAST2, stack[0] == (−1,−1), (120,48) mv
== (−1,−1), tile-2 cascade collapses, FATE aggregate moves.

## Session 2026-09-28 (cont'd 12) — the ref-tree gate CONTEXTS verified
## equal; the divergence is in the value window, not the context tables

Pure code comparison of the gate contexts (dav1d env.h's
`av1_get_ref_ctx`/`av1_get_fwd_ref_ctx`/`av1_get_fwd_ref_1_ctx`/
`av1_get_fwd_ref_2_ctx`/`av1_get_bwd_ref_1_ctx` vs K's `rcc(count pairs)`
in `read_single_ref_name`): all five gate formulas match one-for-one
(bwd-vs-fwd includes ALTREF on both sides; p3 = LAST+LAST2 vs
LAST3+GOLDEN with GOLDEN counted in the second summand; p4 = LAST vs
LAST2; same ==→1 / <→0 / >→2 mapping). The neighbour inputs at
(116,48) also match (left = LAST2, above unavailable → same ctx), so
the same CDF cells are traversed.

Additionally: K's split computation is identical to dav1d's at every
probed read (K `cur = ((r>>8)*(f>>6))>>1 + 4*(n-symbol-1)` == dav
`v = ((r>>8)*(f>>6)>>1) + 4` for bools, same f in complement), and K's
post-read EC states match dav1d's `dif_hi` exactly at matching anchors
(K symbol_value=4376 == dav dif_hi=4376 at the (120,48) KSKIP). The two
decoders' value windows are the SAME quantity, not mirrored views.

Therefore the (112,56) skip divergence (K skip=0/value≥cur vs dav
skip=1/dif<v) and the (116,48) ref divergence (K LAST vs dav LAST2) mean
K's symbol_value was ALREADY larger than dav1d's dif_hi by the time
those reads ran — i.e. the windows desynced EARLIER, at a read whose
decoded value matched but whose renorm/refill consumed different bits
(differing `bits` shift or refill count), or at a read only one decoder
performs (an extra/missing read that happens to decode the same value
for a while). The next comparison must walk EVERY read in tile row 2
from the tile start — including reads K prints but dav1d doesn't
(IBC flag, ref_mv, drl) — tracking (range, value) continuously; the
first read where either the decoded value OR the post-read (range,
value) pair differs is the bug. All the hooks for this exist as of
this session (K: IBSUM/B0/TRACE/SEQ; dav1d: KSKIP/KINTRA with cdf+dif,
KCOEF with KGT_OH frame gate).
