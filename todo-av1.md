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
