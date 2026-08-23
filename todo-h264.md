# TPT Kinetix — H.264 Decoder Todo

> Active work. See [todo.md](todo.md) for the project index.

## Phase 12 — Full From-Scratch Conformant Decoders (2026-07-20)

> Goal: genuine pixel-exact, conformant decode for H.264 (and AV1), built
> from scratch. Every normative table is transcribed from an authoritative
> source (ITU-T H.264 spec; cross-checked against permissively-licensed
> references) with citations — no guessed/approximated tables. Each phase must
> compile, be unit-tested, and validated bit-exact against `ffmpeg`/`dav1d`
> before its box is checked. Do NOT flip `capabilities().pixel_exact = true`
> for a codec until its conformance harness passes.

### Foundations & correctness fixes (blockers)
- [x] Replace the approximated CAVLC tables in `slice.rs` with spec-exact
      `coeff_token` (Table 9-5), `level_prefix` (Table 9-6), `total_zeros`
      (Tables 9-7/9-8), chroma-DC `total_zeros` (Table 9-9), and `run_before`
      (Table 9-10) tables, with unit tests per table — done in
      `src/cavlc_tables.rs` (exhaustive prefix-code roundtrip tests pass)
- [x] Replace the simplified single-scale inverse-quant/IDCT in `macroblock.rs`
      with the spec `LevelScale4x4` weighting + correct 4×4 residual transform
      (§8.5.12), and add the Intra_16×16 luma DC Hadamard transform (§8.5.10)
      and chroma DC transform (§8.5.11) — done in `src/transform.rs` (unit-tested)
- [x] Extend SPS/PPS/slice-header parsers to retain all fields needed for
      reconstruction (chroma_format_idc, transform_8x8_mode_flag,
      chroma_qp_index_offset, num_ref_idx overrides, ref_pic_list_modification,
      pred_weight_table, dec_ref_pic_marking) — SPS/PPS extended; slice header
      fully rewritten (§7.3.3) exposing `data_bit_offset`

### H.264 — Phase A: I-frame / baseline pixel-exact
- [x] Implement the real slice-data parsing loop (§7.3.4): mb_type,
      coded_block_pattern, mb_qp_delta, CAVLC residual parsing dispatch —
      I-slice parser done in `src/slice_data.rs` (mb_type Table 7-11, CBP
      Table 9-4, nC neighbour derivation, spec §9.2.2 level decoding, unit
      tested). Wired into `decoder.rs::decode_slice()`; the fallback path now
      produces spec-exact CAVLC I-frames via `parse_i_slice` +
      `reconstruct_intra_frame` + deblocking, instead of the all-skip grey stub.
      I_PCM + Intra_4×4 MPM neighbour tracking are also implemented in
      `slice_data.rs`.
- [x] Neighbour-availability + Intra_4×4/16×16 mode signalling
      (prev_intra4x4_pred_mode / rem_intra4x4_pred_mode, §8.3.1.1) — full MPM
      derivation with left/top neighbour tracking implemented in
      `slice_data.rs::parse_i_macroblock`
- [x] Validate bit-exact I-frame baseline decode vs `ffmpeg` on a generated corpus —
      found and fixed a real bug: `Intra4x4Mode::DiagonalDownRight` in
      `prediction.rs` used the wrong sample weighting (only left/top-left
      samples, mismapped to the wrong output positions) instead of the spec
      §8.3.1.2.5 formula (cross-checked against ffmpeg's
      `pred4x4_down_right_c`); the other 7 Intra_4×4 modes were individually
      re-verified against the same ffmpeg reference and are correct. Also
      fixed an OOB panic in `deblock.rs` for non-16-aligned picture
      dimensions (missing per-sample x/y bounds checks in the last partial
      row/column of macroblocks). `cavlc_iframe_no_deblock_is_bitexact` and
      `cavlc_iframe_with_deblock_tracks_progress` are now both bit-exact
      (max_diff=0, not just <=20), and an ad hoc corpus of 8 MB-aligned
      clips (`tpt-kinetix-h264/examples/corpus_check.rs`, varied resolution
      48x32..128x96, testsrc/smptebars content) all decode bit-exact.
      Remaining known gap: non-16-aligned picture dimensions still show
      small (≤53) pixel diffs clustered at the partial right/bottom
      macroblock edges (deblocking/prediction edge-sample handling for
      cropped pictures) — tracked as a follow-up, not yet root-caused.

### H.264 — Phase B: complete CAVLC
- [x] Wire the spec-exact CAVLC tables into residual parsing; correct nC
      derivation from left/top neighbour TotalCoeff; validate on P/I CAVLC clips —
      `slice_data.rs` already drove the spec-exact `cavlc_tables.rs` tables
      (coeff_token/total_zeros/run_before) with real left/top-neighbour nC
      derivation for I-slices as of Phase A, validated bit-exact there. Removed
      the last user of the old approximated hand-rolled VLC tables in
      `slice.rs` (`parse_cavlc_residual` and its private VLC0/1/2/3,
      total_zeros, run_before helpers) — it was dead code (only its own unit
      test called it) left over from before `cavlc_tables.rs` existed, and its
      `total_zeros`/`run_before` tables were explicitly approximated per their
      own doc comments. P-slice CAVLC validation is blocked on Phase C (inter
      prediction) since P slices need motion compensation to reconstruct, but
      the residual-parsing path itself (coeff_token/nC/total_zeros/run_before)
      is slice-type-agnostic and already spec-exact.

### H.264 — Phase C: inter prediction (P-frames)
- [x] DPB + POC derivation (§8.2.1), reference-picture-list construction (§8.2.4)
- [x] Motion-vector prediction (§8.4.1) and mb_type/sub_mb partition parsing
- [x] Luma 6-tap + chroma bilinear sub-pel interpolation (§8.4.2.2)
- [x] Validate bit-exact P-frame decode vs `ffmpeg` — **DONE (2026-08-08)**

  #### Phase C.1 — unblock P-slice parsing (RESOLVED)

  **Status (2026-08-07):** The CAVLC bit-position desync is **resolved**.
  `p_slice_cavlc_invariant::p_slice_cavlc_parse_succeeds` now passes (the
  slice parses to completion without a `run_before > zeros_left` error), and the
  P-slice path (`parse_p_slice` → `parse_p_macroblock`) runs end-to-end. The
  prior desync was fixed by the `0ee3386` line of work (inter CBP table,
  `coeff_token` FLC codes, `dec_ref_pic_marking` gating). The `level_code`
  assembly in `parse_cavlc_block`/`parse_cavlc_chroma_dc` is the spec form
  (`base = (level_prefix << suffixLength)`, escape `+15` for `prefix>=14 &&
  sl==0`, and `+(15 - (1<<(prefix-3)) + 4096)` for `prefix>=15`); this was
  cross-checked against the IJERT reference algorithm and against I-frame
  bit-exactness (changing it to a `level_prefix.min(15)` form regressed the
  I-frame tests, confirming the committed form is correct).

  - [x] Reproduce deterministically (was `max_diff=127`, now resolved).
  - [x] Static 2-frame clip (identical frames → zero residual, cbp=0) is
        BIT-EXACT (`inter_skip_copies_reference` + live decode).
  - [x] CAVLC tables cross-checked against FFmpeg `h264data.c`; chroma-AC
        `total_zeros` "bug" confirmed NOT a bug (single combined table is
        spec-correct).
  - [x] `parse_p_slice` / `parse_p_macroblock` run end-to-end without panic.

  #### Phase C.2 — P-frame reconstruction correctness — **DONE (2026-08-08)**

  The earlier "max_diff=2 over 49/4608 samples" gap was **not** a CAVLC
  residual bug — it was a false premise in the test harness. `-x264-params
  deblock=0` only zeroes x264's alpha/beta filter *offset*; it does not set
  `disable_deblocking_filter_idc=1`, so every P-frame conformance test was
  unknowingly comparing against an ffmpeg reference with deblocking **on**
  while assuming it was off (`no-deblock=1` is the key that actually disables
  it). Two independent lines of evidence closed this out:
  1. A fixed differential CAVLC oracle (`p_slice_oracle2.rs` — an independent
     re-implementation of the §9.2.2 level-assembly walk, sharing only the
     already-verified VLC tables) found **0 mismatches** across all 104
     luma/chroma-AC/chroma-DC residual blocks of the 64×48 clip. (The oracle
     itself had a bug — it discarded computed chroma-AC `TotalCoeff` back into
     the nC neighbour grid as all-zero — which is what caused the earlier
     apparent desync; fixed alongside.)
  2. With deblocking genuinely disabled (`no-deblock=1`), P-frame decode is
     **bit-exact (max_diff=0)** against ffmpeg — proving CAVLC, MV
     prediction, motion compensation, and dequant/IDCT are all already
     correct.
  3. That pointed the real gap at the deblocking filter: `deblock.rs` derived
     one boundary-strength (`bS`) value per *whole macroblock edge* from a
     whole-MB `has_coeffs` flag, and had no motion-vector/ref-index rule at
     all (documented as a known gap). Spec §8.7.2.1 requires `bS` per
     4-sample segment (i.e. per pair of 4×4 blocks straddling the edge), with
     a coefficient-OR rule (bS=2 if *either* side's block has nonzero coeffs
     — the old code's "both vs exactly one coded" distinction, giving bS=1,
     was itself non-spec) and a fallback bS=1 rule for differing ref_idx or
     MV components differing by ≥4 quarter-pel. Rewired `DeblockMbInfo` to
     carry the per-4×4-block `nz` grid and `MvStore` cells, and restructured
     `deblock_luma_edge`/`deblock_chroma_edge` to filter each 4-sample (luma)
     / 2-sample (chroma) segment with its own bS. Result: bit-exact
     (max_diff=0) with deblocking **enabled** too (`p_frame_conformance.rs`
     now covers both variants; `cavlc_conformance.rs`'s I-frame equivalent
     tightened from a loosened `<=20` bound to exact 0 as well).

  - [x] Confirm P-slice `parse_p_macroblock` runs end-to-end without panic.
  - [x] Motion-compensated reconstruction (MV prediction + 6-tap/bilinear
        interp) produces correct reference-block fetches.
  - [x] Validate bit-exact P-frame decode vs `ffmpeg` — **max_diff=0**, both
        with deblocking disabled and with deblocking enabled.
  - [x] Independent CAVLC oracle confirms residual decode was never the bug.
  - [x] Per-4×4-block deblocking `bS` (coefficient-OR + MV/ref rule) fixes the
        real remaining gap; chroma reuses the co-located luma `bS` per spec.

   #### Phase C.3 — multi-P-frame chaining — **RESOLVED (2026-08-13)**

   - [x] `tests/multi_frame_dpb.rs::ipppp_clip_decodes_bitexact_frame_by_frame`
         now passes bit-exact (max_diff=0) for **all five** frames on the
         64×48 IPPPP clip. The original frame-2 divergence (max_diff=33 over
         124 samples, the first P picture predicting from another P picture)
         is gone. Root cause was the reference-list / POC-ordering logic that
         later landed in Phase E.1 (`modify_ref_pic_list` §8.2.4.3), E.2
         (`mark_decoded_picture` §8.2.5), and E.5 (`build_ref_list_l0_b_slice`
         POC-based ordering, plus the 2-partition B MVD interleave fix) — all
         of which make second-and-later P pictures predict from the correct
         reference. The decoder's `MAX_MB_COUNT`/`MAX_DIMENSION` guards
         (decoder.rs) also bound the picture allocation that the original
         report worried about. Verified by re-running the test directly.
   - [x] `tests/fuzz_from_seed.rs::fuzz_structured_seeds` is now a reliable CI
         gate. Two fixes landed:
         1. **Real fuzz-crash fixed:** a structured-seed mutation survey
            (800 k iters) found an actual panic — `attempt to shift left with
            overflow` at `slice.rs::WeightEntry::default_for`, triggered by an
            attacker-controlled `luma_log2_weight_denom`/`chroma_log2_weight_denom`
            (`ue(v)`, unbounded) fed into `1 << denom`. Fixed by bounding both
            denoms to ≤30 at parse time in `parse_pred_weight_table`
            (§7.4.3.2), and making `default_for` clamp defensively. (The
            `>> (denom + 1)` in `reconstruct.rs::weighted_bi` also needs
            denom ≤30 to stay panic-free, so 30 is the safe ceiling.) Other
            unbounded shifts were already safe: `log2_max_frame_num_minus4` is
            bounded ≤12 (sps.rs) and `transform.rs` `shift = qp/6` is always
            in range.
         2. **Machine-relative timeout:** replaced the fixed 300 ms constant
            with a budget calibrated from the worst-case *valid* decode on the
            runner (a 36864-MB IDR, capped by `MAX_MB_COUNT`), set to
            `max(2s, min(30s, 3 × worst_valid))`. Added a 60 s overall
            wall-clock deadline so the test can't run for hours on fast
            runners, and silenced the panic hook so caught-panic backtraces
            don't spam CI logs. Re-running the survey now shows **0 panics /
            800 k iters**; the test itself completes ~204 k iters in 60 s with
            no crash and slowest decode ~97 ms (well under the calibrated
            timeout).

### H.264 — Phase D: CABAC

> Updated 2026-08-09: the engine + I-slice context tables/binarizations from
> the previous entry were re-verified against FFmpeg's `libavcodec/h264_cabac.c`
> (cross-checked the same way this repo's CAVLC tables already are) and found
> to have real bugs, not just missing coverage — `MbTypeICabacContext`'s bin-0
> context was static instead of neighbor-derived and was missing the I_PCM
> `decode_terminate()` check; `CbpCabacContext` had no neighbor input at all;
> `MbQpDeltaCabacContext` used a truncated-unary+EG0 binarization instead of
> the real unbounded-unary one. All three were rewritten, and the previously
> "outstanding" I-slice-relevant tables (chroma pred mode, intra4x4 pred mode,
> coded_block_flag, significant/last_significant_coeff_flag, coeff_abs_level)
> were implemented — see Phase D.1/D.3 below for what's actually done now.

- [x] Context-index tables + binarizations for **I-slice** syntax elements
       (mb_type, intra_chroma_pred_mode, prev/rem_intra4x4_pred_mode,
       coded_block_pattern, mb_qp_delta, coded_block_flag,
       significant_coeff_flag/last_significant_coeff_flag/coeff_abs_level_minus1)
       implemented in `entropy.rs`/`cabac_tables.rs` and unit-tested. P/B-slice
       tables (mb_skip_flag refinement, mb_type P/B, sub_mb_type, ref_idx, mvd)
       also implemented (Phase D.1/D.2) and now exercised by the P/B-slice
       parsing landed in Phase D.4.
- [x] CABAC macroblock/residual syntax parsing wired into the slice loop
       (`slice_data.rs::parse_i_slice_cabac`/`parse_intra_macroblock_cabac`,
       `parse_p_slice_cabac`/`parse_p_macroblock_cabac`,
       `parse_b_slice_cabac`/`parse_b_macroblock_cabac`, all wired into
       `decoder.rs`) for I/P/B slices, reusing the existing CAVLC-path
       reconstruction/dequant/IDCT/deblock code unchanged. The CABAC 8×8-transform
       (High-profile) path is now wired too (2026-08-15, see Phase F.4) but not
       yet bit-exact.
- [x] **CABAC I-slice desync bug — RESOLVED 2026-08-12.** Root cause:
      `entropy.rs::TRANS_IDX_LPS[28]` was `23`; the correct value is `22` — a
      single-entry transcription error in the `transIdxLPS` (spec Table 9-45)
      state-transition table, present since the table was first added. It
      only manifested when `pStateIdx` reached exactly `28` *and* underwent
      an LPS (least-probable-symbol) transition while decoding
      `coeff_abs_level_minus1`'s truncated-unary continuation bins — a
      specific (state, branch) combination most test content never hit,
      which is why CAVLC conformance, the I-slice mb_type/cbp/residual unit
      tests, and even several new bespoke CABAC repros (flat/checkerboard/
      random-noise/gradient content, including ones exercising the
      significant-coefficient-count-16/16 edge case and the Exp-Golomb escape
      path) all passed while `ffmpeg`'s `testsrc`/`testsrc2`/`rgbtestsrc`/
      `smptebars` filters at `size=16x16` reliably triggered it.
      \
      Found by building a self-contained C harness (MSVC via
      `vcvars64.bat`+`cl.exe`; no mingw/gcc available on this box) that
      copies FFmpeg's actual `libavcodec/cabac.c` engine and
      `ff_h264_cabac_tables` verbatim (fetched fresh from
      github.com/FFmpeg/FFmpeg — not reimplemented from memory), then running
      it against the real 290-byte CABAC payload from the `testsrc` repro
      side-by-side with the Rust decoder's own per-bin trace
      (`entropy.rs::CabacDecoder::debug_state()`, temporary, removed after).
      The two engines' `range` value (directly comparable — it isn't
      rescaled differently between representations) matched at every single
      call through the significance map and the first several level
      decodes, then diverged at one specific `coeff_abs_level_minus1`
      continuation bin. Isolating that one call and decoding FFmpeg's packed
      `ff_h264_mlps_state` table by hand for the same `(pStateIdx, valMPS)`
      pair going in — cross-checked programmatically for *all* 64 states in
      both `TRANS_IDX_LPS` and `TRANS_IDX_MPS`, not just the one that
      failed — found exactly one mismatch: index 28 of `TRANS_IDX_LPS`.
      \
      This means the extensive engine/context-table verification from the
      previous investigation pass (re-checking against real FFmpeg source,
      writing an independent from-scratch Python reimplementation) was
      thorough but insufficient: two implementations built from the *same*
      transcribed table inevitably agree with each other while both being
      wrong, so the only way this surfaced was comparing against the actual
      *compiled* reference engine rather than another reimplementation from
      the same source reading. Worth remembering as a lesson if a similar
      "two independent implementations agree but still don't match ffmpeg"
      situation comes up elsewhere (AV1 entropy decoder, P/B-slice CABAC).
      \
      `cabac_conformance.rs`'s two tests (Main-profile CABAC I-frame,
      deblocking on/off, real `testsrc` content at 64×48) are un-`#[ignore]`d
      and pass bit-exact; all 191 `tpt-kinetix-h264` unit tests still pass.
- [x] Validate bit-exact Main/High CABAC decode vs `ffmpeg` — done, see above.

  #### Phase D.1 — remaining context-index tables

  > Updated 2026-08-09: the three remaining Phase D.1 checkboxes are now
  > done, but this is **context-index-table work only** (init values +
  > ctxIdxOffset layout + ctxIdxInc derivation *data*), not P/B-slice syntax
  > parsing — Phase D.4 below (mb_type-P/B/sub_mb_type/ref_idx/mvd
  > *binarization and decode-loop wiring*) remains not started. Added
  > `CABAC_CTX_INIT_PB0`/`1`/`2` (1024-entry `(m,n)` tables per
  > `cabac_init_idc`) plus ctxIdxOffset constants for mb_skip_flag/mb_type/
  > sub_mb_type (P/SP and B) and mvd_x/mvd_y/ref_idx, all fetched and
  > cross-checked from FFmpeg's `libavcodec/h264_cabac.c` two independent
  > ways (the source's own `/* lo - hi */` block comments plus the literal
  > ctxIdx arithmetic in `decode_cabac_mb_skip`/`decode_cabac_p_mb_sub_type`/
  > `decode_cabac_b_mb_sub_type`/`decode_cabac_mb_ref`/`decode_cabac_mb_mvd`).
  > Found and fixed a real bug in the process: the pre-existing
  > `MB_SKIP_FLAG_P_INIT` stub's three `(m,n)` pairs turned out to be ctxIdx
  > 11's value from *each of the three* `cabac_init_idc` tables, not ctxIdx
  > 11/12/13 from one table — `MbSkipFlagContext::new_p_slice` now takes a
  > `cabac_init_idc` parameter and reads the verified `CABAC_CTX_INIT_PB*`
  > tables directly (see `entropy.rs`'s `MbSkipFlagContext` doc comment for
  > the full story); a `new_b_slice` constructor (ctxIdx 24..=26) was added
  > alongside it, confirmed from source to reuse the same condTermFlag
  > derivation as P/SP. Also added `ctxBlockCat` 5 (Luma8x8) residual
  > contexts: extended `SIG_COEFF_CTX_BASE`/`LAST_COEFF_CTX_BASE`/
  > `COEFF_ABS_LEVEL_M1_CTX_BASE` to 6 entries, and confirmed from FFmpeg's
  > `decode_cabac_residual_nondc` that `coded_block_flag` is *not* separately
  > signalled for Luma8x8 in the non-4:4:4 case this crate targets (so
  > `CBF_CTX_BASE` deliberately stays at 5 entries, documented on
  > `CAT_LUMA_8X8`); added the `significant_coeff_flag`/
  > `last_significant_coeff_flag` many-to-one ctxIdxInc indirection tables
  > (`SIG_COEFF_CTX_INC_8X8_FRAME`/`LAST_COEFF_CTX_INC_8X8_FRAME`, 63 entries
  > each) as standalone consts — **not** wired into
  > `ResidualCabacContext::decode_block`, which still assumes ctxIdxInc ==
  > scan position (only valid for cats 0..=4); that restructuring, plus all
  > actual P/B mb_type/sub_mb_type/ref_idx/mvd binarization, is Phase D.4.
  > All new tables/consts are unit-tested in `cabac_tables.rs`.

  - [x] mb_type I-slice, coded_block_pattern, mb_qp_delta, intra_chroma_pred_mode,
        prev/rem_intra4x4_pred_mode, coded_block_flag, significant_coeff_flag,
        last_significant_coeff_flag, coeff_abs_level_minus1 — all I-slice-only,
        frame coding (no MBAFF/field), no 8x8 transform.
  - [x] mb_type P/B-slice, sub_mb_type, ref_idx, mvd context init (needed for
        Phase D.4 P/B-slice CABAC) — tables + ctxIdxOffset constants only,
        see `cabac_tables.rs`; no binarization/parsing implemented yet.
  - [x] mb_skip_flag refinement for P/B — old `MB_SKIP_FLAG_P_INIT` stub was
        wrong (see note above), fixed and extended with a B-slice
        constructor; both now `cabac_init_idc`-dependent per source.
  - [x] 8x8-transform-specific residual contexts (ctxBlockCat 5, Luma8x8) —
        context-index tables + ctxIdxInc indirection LUTs, now wired into
        `decode_block_8x8` (2026-08-15, see Phase F.4); not yet bit-exact.

  #### Phase D.2 — binarizations (§9.3.2)
  - [x] Truncated-unary, FL (LSB-first per §9.3.2.5, distinct from CAVLC's
        MSB-first `u(v)`), and UEG0 (via `decode_bypass_eg`) binarizations
        used by the I-slice tables above are implemented; see `entropy.rs`.
  - [x] Binarizations specific to P/B-slice elements (mvd's UEGk suffix beyond
        what `decode_bypass_eg` already covers, ref_idx's truncated unary) —
        **already implemented**, found while starting this phase: commit
        `a8e4b56` (labeled "MMCO/ref-list wiring") landed `RefIdxCabacContext`
        and `MvdCabacContext` in `entropy.rs` alongside the ref-pic-list work,
        but never got its own checkbox here. `RefIdxCabacContext::decode` is a
        1:1 transliteration of FFmpeg's `decode_cabac_mb_ref` ctxIdx recurrence
        (truncated unary, no separate bin-0 special case). `MvdCabacContext::decode`
        does the context-coded truncated-unary prefix (saturating at 9) then
        falls through to the existing `decode_bypass_eg(3)` for the UEGk
        suffix — confirming no new bypass primitive was needed. Both are
        unit-tested (`ref_idx_decode_*`, `mvd_decode_*`, 6 tests, all passing).
        Neither is wired into `slice_data.rs`'s parser yet — that's still
        Phase D.4, unchanged below.

  #### Phase D.3 — CABAC syntax parsing in the slice loop
  - [x] I-slice mb_type / intra pred modes / coded_block_pattern / mb_qp_delta
        / residual wired into `slice_data.rs`'s CABAC-specific parser,
        reusing existing CAVLC reconstruction (transform/prediction/deblock)
        unchanged (see `parse_i_slice_cabac`).
  - [x] Fix the known desync bug above (`TRANS_IDX_LPS[28]` fix, 2026-08-12).
  - [x] Add a Main/High-profile CABAC clip to the corpus and validate bit-exact
        vs `ffmpeg` — `cabac_conformance.rs`'s two tests are un-`#[ignore]`d
        and passing (64×48 `testsrc`, deblocking on/off).

#### Phase D.4 — P/B-slice CABAC — **DONE (2026-08-13)**

> `slice_data.rs` gained `parse_p_slice_cabac`/`parse_p_macroblock_cabac` and
> `parse_b_slice_cabac`/`parse_b_macroblock_cabac`, and `decoder.rs` now
> dispatches CABAC P/B slices through them (reusing the CAVLC-path
> reconstruction/dequant/IDCT/deblock unchanged). The context tables/parsing
> for mb_skip_flag, mb_type P/B, sub_mb_type, ref_idx, and mvd were already in
> place (Phase D.1/D.2), and the mb_type/CBP binarization + context logic was
> fixed in commit `0f9e0a3`. Conformance is bit-exact:
> `tests/cabac_conformance.rs` has live (un-`#[ignore]`d) `cabac_pframe_*_is_bitexact`
> and `cabac_bframe_*_is_bitexact` tests (deblocking on/off), and
> `tests/cabac_pframe_conformance.rs` additionally pins the inter-MB CABAC parse
> path bit-exact vs ffmpeg. NOTE: the CABAC **8×8-transform** path (High
> profile, `transform_8x8_mode_flag`) is now wired too (2026-08-15) but not
> yet bit-exact — see Phase F.4.

- [x] mb_skip_flag, mb_type P/B, sub_mb_type, ref_idx, mvd context tables +
      parsing, following the same I-slice-first-then-P-slice pattern used
      for CAVLC

### H.264 — Phase E/F/G: advanced tools

> Broken down 2026-08-06: each of the three items below previously bundled
> several independent features into one checkbox. Grounded against the
> actual code state: `parse_ref_pic_list_modification` (`slice.rs:305`),
> `parse_dec_ref_pic_marking` (`slice.rs:371`), and `parse_pred_weight_table`
> (`slice.rs:334`) all already parse their syntax but only to advance the bit
> position — every decoded value (`_long_term_pic_num`, `_lw`, `_cw`, etc.) is
> discarded, per their own doc comments. `slice_data.rs` has no B-slice
> `mb_type`/`sub_mb_type` handling at all yet. `transform_8x8_mode_flag` is
> parsed into `pps.rs` but `transform.rs` has no 8×8 transform path, and the
> SPS/PPS `scaling_list` values are parsed but never applied at dequant.
> `field_pic_flag` is parsed in `slice.rs` but `bottom_field_flag` is
> discarded and there is no field/MBAFF decode logic anywhere.
>
> Updated 2026-08-09: Phase E.1 is done —
> `parse_ref_pic_list_modification`'s values are no longer discarded; they now
> drive `ref_pic::modify_ref_pic_list` (§8.2.4.3). Phase E.2 is done too —
> `parse_dec_ref_pic_marking`'s values now drive `ref_pic::Dpb::mark_decoded_picture`
> (§8.2.5). The rest of the paragraph above still holds:
> `parse_pred_weight_table` (E.4) remains parse-only.

#### Phase E.1 — ref_pic_list_modification (wire the existing parse-only stub)
- [x] Thread `modification_of_pic_nums_idc` + `abs_diff_pic_num_minus1` /
      `long_term_pic_num` from `parse_ref_pic_list_modification` (`slice.rs:305`)
      into `ref_pic.rs`'s reference-list construction so it actually reorders
      `RefPicList0`/`RefPicList1` per §8.2.4.3, instead of discarding the values
      — **DONE (2026-08-09)**. `ref_pic.rs` gained `modify_ref_pic_list`
      (§8.2.4.3.1 short-term `picNumLXPred`/`picNumLXNoWrap`/`picNumLX`
      derivation with `MaxPicNum` wrap, §8.2.4.3.2 long-term selection, and the
      shared 8-38/8-39 insert-shift-dedupe splice via `splice_into_list`, using
      `PicNumF`/`LongTermPicNumF` as `Option<i64>` "never matches" sentinels).
      `build_ref_list_l0` now takes `PicNumContext` + the header's
      `ref_pic_list_modification_l0` and applies §8.2.4.2.1 initialisation then
      §8.2.4.3 modification; `decoder.rs` passes them through. Two adjacent
      correctness fixes landed with it: (a) §8.2.4.2.1 P-slice initialisation
      ordered short-term refs by descending **PicOrderCnt**, which is the
      B-slice rule — it now orders by descending **PicNum** (`FrameNumWrap`),
      so `frame_num`-wrapped references sort correctly; (b) `decoder.rs` sized
      RefPicList0 from the raw PPS `num_ref_idx_l0_default_active_minus1`,
      ignoring the slice header's `num_ref_idx_active_override_flag` — it now
      uses the header's effective value (§7.4.3). Malformed streams fail safe:
      a command naming a picture absent from the DPB yields
      `RefPicListError`/`None` so the caller falls back rather than decoding
      against a wrong reference list, and the parser now enforces §7.4.3.1's
      cap of `num_ref_idx_lX_active_minus1 + 1` commands (previously an
      unbounded loop, harmless only because the values were discarded).
      **Note: L1 is parsed and stored but not yet applied — B-slice decode
      does not exist (Phase E.3), so there is no `RefPicList1` to modify.**
- [x] Unit test: a P-slice with an explicit reorder command produces a
      different `RefPicList0` order than default construction (§8.2.4.2) —
      `ref_pic.rs::tests::modification_reorders_p_slice_list_away_from_default`
      plus 7 sibling unit tests (pred carried across commands, `MaxPicNum`
      wrap on `ShortTermAdd`, long-term promotion, list length invariance
      across `num_active` 1..=4, pulling in a picture the truncation dropped,
      absent-picture error, empty-list no-op), and the end-to-end
      `tests/ref_pic_list_modification.rs` which drives the reorder from real
      slice-header bitstream syntax (3 tests) plus 3 new `slice.rs` header
      round-trip/§7.4.3.1-rejection tests. Existing bit-exact I/P-frame
      conformance (`cavlc_conformance.rs`, `p_frame_conformance.rs`,
      max_diff=0) is unaffected.

#### Phase E.2 — MMCO / dec_ref_pic_marking (wire the existing parse-only stub)
- [x] Thread `memory_management_control_operation` values 1–6 from
      `parse_dec_ref_pic_marking` (`slice.rs:371`) into real DPB marking in
      `ref_pic.rs` (mark-unused-for-reference, long-term conversion, sliding
      window override), instead of discarding the values — **DONE (2026-08-09)**.
      `parse_dec_ref_pic_marking` now returns a typed `DecRefPicMarking`
      (`Idr { no_output_of_prior_pics_flag, long_term_reference_flag }` /
      `SlidingWindow` / `Adaptive(Vec<MmcoOp>)`) stored on
      `SliceHeader::dec_ref_pic_marking`, and `ref_pic.rs` gained
      `Dpb::mark_decoded_picture`, the §8.2.5 decoded reference picture marking
      process, which `decoder.rs::store_reference_picture` runs for every
      reference picture on both decode paths. All six operations are
      implemented: MMCO 1 (§8.2.5.4.1, `picNumX = CurrPicNum −
      (difference_of_pic_nums_minus1 + 1)`, equation 8-40, matched against
      `PicNum`/`FrameNumWrap` so pre-wrap negatives work), MMCO 2 (§8.2.5.4.2),
      MMCO 3 (§8.2.5.4.3, short-term → long-term, evicting whichever picture
      already held that `LongTermFrameIdx`), MMCO 4 (§8.2.5.4.4,
      `MaxLongTermFrameIdx`, dropping every long-term above the new maximum,
      `plus1 == 0` meaning "no long-term frame indices"), MMCO 5 (§8.2.5.4.5,
      empty the DPB, reset `MaxLongTermFrameIdx`, and rebase the current
      picture to `frame_num == 0` / `PicOrderCnt == 0` per §7.4.3/§8.2.1.1 —
      reported back through `MarkingOutcome::mmco5` so `decoder.rs` also runs
      `PocState::reset_after_mmco5`), and MMCO 6 (§8.2.5.4.6, current picture →
      long-term). §8.2.5.1's "adaptive marking replaces sliding-window marking"
      rule is honoured: `Adaptive` never runs `apply_sliding_window`, and the
      current picture is marked short-term afterwards unless MMCO 6 claimed it.
      IDR marking (including `long_term_reference_flag`) goes through the same
      entry point. Malformed streams fail safe rather than half-marking: a
      command naming a picture that is not in the DPB with the required marking
      returns `MmcoError` **and empties the DPB**, so the next inter slice
      cannot predict from a wrongly-marked reference; the parser additionally
      rejects out-of-range operands at parse time (§7.4.3.3: `long_term_frame_idx`
      / `long_term_pic_num` > 15, `max_long_term_frame_idx_plus1` > 16, unknown
      MMCO values) and caps the command list at FFmpeg's `MAX_MMCO_COUNT` (66)
      so a `0`-terminated loop cannot be made unbounded. A defensive
      post-marking capacity clamp mirrors FFmpeg's
      `ff_h264_execute_ref_pic_marking` "reference frames exceeds max (probably
      corrupt input)" behaviour, since each DPB entry owns a full decoded frame
      and is therefore a memory-exhaustion vector for the fuzzers.
- [x] Unit test: MMCO 5 (reset) and MMCO 1 (mark short-term unused) each
      produce the expected DPB state — the two headline cases are
      `ref_pic.rs::tests::mmco1_marks_the_selected_short_term_picture_unused`
      and `::mmco5_resets_the_dpb_and_rebases_the_current_picture`, alongside 12
      sibling unit tests (MMCO 2/3/4/6, `LongTermFrameIdx` reuse eviction,
      adaptive-overrides-sliding-window, in-order application, absent-picture
      fail-safe, overfull-DPB clamp, IDR with/without `long_term_reference_flag`,
      the MMCO-5 POC-state reset, and an MMCO 3 → §8.2.4.3.2 hand-off proving
      Phase E.1 and E.2 compose). Two further layers were added on top:
      `tests/dec_ref_pic_marking.rs` drives the same operations from **real
      slice-header bitstream syntax** (7 tests), and — new this session — from
      **whole Annex B access units through the public `H264Decoder::decode`
      API** (6 tests), which is the only layer that covers
      `decoder.rs::store_reference_picture` itself: POC derivation, the
      `nal_ref_idc == 0` "non-reference pictures never enter the DPB" gate, the
      `PocState::reset_after_mmco5` rebase, and the fail-safe error branch. The
      decoder-level tests were mutation-checked (severing the header→marking
      wiring fails 5 of the 6; deleting the `reset_after_mmco5()` call fails the
      MMCO 5 one, which needed `pic_order_cnt_lsb` values chosen so the missing
      reset actually changes the derived POC — 12 → 2 reads as an MSB wrap and
      yields 18). `H264Decoder::dpb()` was added as a read-only accessor so the
      marking result is observable without inferring it from pixels. Existing
      bit-exact I/P-frame conformance (`cavlc_conformance.rs`,
      `p_frame_conformance.rs`, max_diff=0) is unaffected.

#### Phase E.3 — B-slice parsing + direct mode — **DONE (2026-08-12)**
- [x] Parse B-slice `mb_type`/`sub_mb_type` (Tables 7-14..7-18) in
      `slice_data.rs` — `parse_b_slice`/`parse_b_macroblock` added; all 23
      inter mb_types (Direct/L0/L1/Bi 16×16, eighteen 16×8+8×16 variants,
      B_8x8) and 13 B sub_mb_types (Table 7-15) are parsed; intra fall-through
      subtracts 23 from raw mb_type per spec §7.4.5; ref_idx and MVD reading
      follows the spec's all-L0-refs/all-L1-refs/per-part-MVDs order for
      multi-partition types and the B_8x8 sub-partition loop. Added
      `BPredDir` enum and new `MbType` variants (`BL016x16`/`BL116x16`/
      `BBi16x16`/`B16x8`/`B8x16`/`BB8x8`) to `macroblock.rs`; added L1 fields
      (`ref_idx_l1`, `mvd_l1`, `pred_dirs`, `sub_mb_type_b`) to `InterMotion`.
- [x] Implement spatial direct mode MV derivation (§8.4.1.2.2) —
      `predict_b_slice_mvs` in `mv.rs`: for B_Direct/B_Skip blocks,
      `refIdxL0` is the min non-negative L0 ref among spatial neighbors A/B/C
      (default 0), `refIdxL1 = 0`; `mvL0`/`mvL1` from the standard
      §8.4.1.3.1 median predictor applied to each list's neighbor fields.
      `MvCell` extended with `mv_l1`/`ref_idx_l1`; `build_ref_list_l1` added
      to `ref_pic.rs` (ascending POC > current, then descending POC ≤
      current, then long-term ascending). `direct_spatial_mv_pred_flag` stored
      on `SliceHeader` (was discarded).
- [x] Implement temporal direct mode MV derivation (§8.4.1.2.3) — when
      `direct_spatial_mv_pred_flag == 0`: scales `mvCol` from the co-located
      4×4 block in `RefPicList1[0]` by `tb/td` (L0) and `(tb−td)/td` (L1)
      per spec §8.4.1.2.3; falls back to (0,0)/ref0 if co-located MV grid
      unavailable. `DpbEntry` gains `mv_grid: Option<Arc<Vec<[MvCell;16]>>>`;
      `decoder.rs` passes the decoded P/B MV grid through `store_reference_picture`.
- [x] Implement bi-predictive motion compensation: average two
      motion-compensated blocks (§8.4.2.3) — `reconstruct_b_frame` in
      `reconstruct.rs`: for each 4×4 block, L0-only/L1-only/bi-pred selected
      by `cell.ref_idx`/`cell.ref_idx_l1` sentinels;
      `pred[i] = (l0[i] + l1[i] + 1) >> 1` for bi-pred; B-slice dispatch
      added to `decoder.rs`. All 190 existing tests pass.

#### Phase E.4 — Weighted prediction (wire the existing parse-only stub)
- [x] Thread `luma_weight`/`luma_offset`/`chroma_weight`/`chroma_offset` from
      `parse_pred_weight_table` (`slice.rs:334`) into explicit weighted
      prediction (§8.4.2.3.2) for P and B slices, instead of discarding them
- [x] Implement implicit weighted prediction (§8.4.2.3.2, B-slices only,
      distance-based weight derivation)
- [x] Unit test: explicit weighted P-slice reconstruction matches hand-computed
      weight/offset for a synthetic block

  **Completed 2026-08-12.** `parse_pred_weight_table` now returns a
  `PredWeightTable` (`slice.rs`) instead of just advancing the bit position;
  `SliceHeader::pred_weight_table` carries it. `reconstruct.rs` gained a
  `WeightedPred` enum (`Default`/`Explicit`/`Implicit`) threaded through
  `reconstruct_inter_frame`/`reconstruct_b_frame` down to the per-4×4-block
  `combine_weighted` helper, implementing the explicit uni/bi-pred formulas
  and the POC-distance-based implicit-weight derivation (§8.4.2.3.2), both
  per FFmpeg-cross-checked spec formulas. `decoder.rs` selects the mode from
  `pps.weighted_pred_flag`/`weighted_bipred_idc`. Caught and fixed a real bug
  along the way via the existing fuzz harness: the first cut of
  `parse_pred_weight_table` preallocated `Vec::with_capacity` directly from
  the attacker-controlled `num_ref_idx_lX_active_minus1` `ue(v)`, which
  OOM'd `fuzz_structured_seeds` on a malformed seed (64GB alloc); fixed by
  dropping the capacity hint and adding an explicit 32-entry bound (§7.4.3),
  matching the pattern `parse_ref_pic_list_modification` already uses.

#### Phase E.5 — Validate B-frame decode
- [x] Generate an IBP-structured corpus clip with `ffmpeg`
- [x] Validate bit-exact B-frame decode vs `ffmpeg` on that corpus

  **Completed 2026-08-12.** Root cause was `build_ref_list_l0` using P-slice
  PicNum ordering for B-slices; B-slices need POC-based ordering (§8.2.4.2.3).
  Added `build_ref_list_l0_b_slice` with the correct ordering. Also fixed the
  2-partition B-type MVD interleave bug (all L0 MVDs before all L1 MVDs per
  §7.3.5.1). Tests `tests/b_frame_conformance.rs` now pass bit-exact
  (max_abs_diff=0) for both deblock-enabled and deblock-disabled variants.

#### Phase F.1 — 8×8 transform: parsing
- [x] Parse `transform_size_8x8_flag` per-macroblock in `slice_data.rs` when
      `pps.transform_8x8_mode_flag` is set (stored on `Macroblock::transform_size_8x8`)
- [x] Parse the 8×8 residual block CAVLC syntax (distinct coeff scan/context
      from the 4×4 path, §7.3.5.3.3) — `luma_coeffs_8x8` populated in `slice_data.rs`

#### Phase F.2 — 8×8 transform: reconstruction
- [x] Implement the 8×8 inverse transform (§8.5.12.3) in `transform.rs` —
      `dequant_idct_8x8` now uses a faithful port of FFmpeg's `ff_h264_idct8_add`
      core (the previous hand-rolled butterfly had the wrong `a4`/`a6` pairing and
      omitted the `a1`/`a3`/`a5`/`a7` + `b1`/`b3`/`b5`/`b7` cross-terms, which
      zeroed DC-only blocks). Unit tests `eight_by_eight_dc_only_is_flat` /
      `eight_by_eight_flat_scaling_*` updated to assert correct (FFmpeg-matching)
      values.
- [x] Implement the four 8×8 intra prediction modes (§8.3.2.2) in
      `prediction.rs` — **done (2026-08-17, verified by reading source)**.
      `predict_8x8` takes `top: &[Option<u8>; 16]` + `left: &[Option<u8>; 8]`
      and computes the 3-tap filtered `t0..t15` / `l0..l7` / `lt` values using
      the `has_topright` / `has_topleft` availability flags per §8.3.2.2, then
      dispatches all 9 modes using those filtered values. The earlier "clamped
      at 7" note is stale — the Vertical/Horizontal modes were fixed (2026-08-16)
      to use filtered `t0..t7`/`l0..l7`, and the diagonal/VerticalRight/
      HorizontalDown/VerticalLeft/HorizontalUp modes all reference `t8..t15`
      through the `has_topright` branch. Unit tests `predict_8x8_vertical` and
      `field_mv_scaling_same_parity_doubles` both pass (confirmed 2026-08-16).
      **F.2 is no longer the prime suspect for the Phase F.4 gap** — subsequent
      investigation found the failure is a whole-frame state-propagation bug
      (not a per-block prediction-math error), inconsistent with a neighbour
      sample calculation issue.

#### Phase F.3 — High-profile scaling matrices

- [x] Apply the already-parsed SPS/PPS `scaling_list` values (Table 7-... /
      §8.5.9) to 4×4 dequant in `transform.rs` — `dequant_idct_4x4` derives
      `LevelScale4x4` from the active `ScalingLists` (§8.5.9). `decoder.rs` now
      merges the PPS list over the SPS list (§8.5.9 fallback) and passes the
      merged set into reconstruction; `pps.rs` defaults the PPS list to the SPS
      list so the active set is always correct.
- [x] Apply the same scaling lists to the 8×8 dequant path from Phase F.2 —
      `dequant_idct_8x8` reads `scaling.scaling_8x8(scale_list)` per coefficient
      (§8.5.9); same merged active set is threaded through `decoder.rs`.

#### Phase F.4 — Validate High-profile 8×8-transform decode

> Updated 2026-08-15: 8×8 reconstruction (CAVLC **and** CABAC) is wired end to
> end and the early-return gate is gone — both `high_profile_8x8_conformance.rs`
> (CAVLC) and the new `high_profile_8x8_cabac_conformance.rs` (CABAC,
> `TransformSize8x8FlagContext` + `ResidualCabacContext::decode_block_8x8`)
> exercise real 8×8 macroblocks and decode without error. **But neither is
> bit-exact**: found while fixing the corpus generator, not the decoder. The
> existing `testsrc=...` clip generator never actually made x264 pick the 8×8
> transform for any macroblock (confirmed via `ffmpeg -loglevel debug`'s "8x8
> transform intra: NN%" line reporting 0%), so both conformance tests were
> passing *vacuously* — a `..._clip_exercises_8x8_transform` tracer-based test
> was added per generator to catch this class of false-pass in the future.
> Swapping the generator to `mandelbrot=...` (enough high-frequency texture to
> make x264 actually choose 8×8 for some macroblocks — verified 12 8×8 luma
> blocks decoded by the tracer in both variants) makes the real bit-exactness
> gap visible: CAVLC `max_abs_diff=160` (3053-3055/4608 samples), CABAC
> `max_abs_diff=161` (3069-3070/4608 samples), both with deblocking on and
> off. The near-identical magnitude/sample-count between CAVLC and CABAC
> suggests a shared bug downstream of entropy decode — most likely
> `predict_8x8`'s already-documented 7-sample-clamped-neighbour gap (Phase
> F.2) or something in the 8×8 dequant/IDCT/reconstruction wiring itself,
> rather than two independent entropy bugs. Not yet root-caused.
- [x] **Wire 8×8 reconstruction into `reconstruct.rs`** (`MbType::Intra4x4` +
      `transform_size_8x8` → `dequant_idct_8x8` + `predict_8x8` per 8×8 block),
      then remove the `entropy_coding_mode_flag && transform_8x8_mode_flag`
      early-return gate in `decoder.rs::try_decode_real_slice` (keep the gate for
      inter 8×8 / non-intra until inter 8×8 is implemented) — done for both the
      CAVLC and CABAC entropy paths.
- [x] Generate a High-profile corpus clip (`transform_8x8_mode_flag=1`) with
      `ffmpeg` that actually exercises the 8×8 path (done — `mandelbrot=...`)
      **and get it to bit-exact decode — CLOSED (verified 2026-08-23).** The
      previously-noted ±1 DC-rounding gap on the 352×288 `mandelbrot` clip is
      gone: `dbg_hp352_localize.rs` now reports luma/cb/cr `max_diff=0`
      (bit-exact, no deblocking), and both `high_profile_8x8_conformance.rs`
      (CAVLC) and `high_profile_8x8_cabac_conformance.rs` matrix cells report
      `max_abs_diff=0`. The fix had already landed as the chroma-DC dequant
      rounding correction in `transform.rs::chroma_dc_transform` (flat
      `(f*ls) >> (5-qP/6)`, no rounding constant — see that function's doc
      comment, which references this exact todo item).
      - 64×48 `mandelbrot` clip: **bit-exact** without deblocking (asserted,
        `max_abs_diff=0`) and ≤2 residual error with default deblocking
        (pre-existing deblocking gap, not 8×8-specific).
      - 352×288 `mandelbrot` clip: improved from `max_abs_diff=84`
        (89137/152064 differing) to `max_abs_diff=79` (72373/152064).
      - **Fixed this session — Intra_8×8 MPM derivation.** The old
        `mpm_pred_mode_8x8` guessed cross-MB neighbour 8×8 blocks with the
        wrong indices. Rewritten to FFmpeg's exact semantics, transcribed
        from `fill_decode_caches` + `write_back_intra_pred_mode` +
        `pred_intra_mode` (h264_mvpred.h / h264dec.h): each quadrant's
        most-probable-mode reads the 4×4 cache cell immediately left of /
        above its top-left sub-block over the *physical* scan8 layout.
        Final mapping (neighbour MB quadrant k-sub-block):
        q0: A=left q1.k1, B=top q2.k2; q1: A=own q0.k1, B=top q3.k2;
        q2: A=left q3.k1, B=own q0.k2; q3: A=own q2.k1, B=own q1.k2.
        (The stored 8-byte per-MB array is [bottom-row k2,k3 of q2/q3,
        right-col k1/k3/k1 of q1,q3,q1] — an unintuitive permutation that
        is easy to get wrong; verified against x264's cache load/save,
        which uses the identical physical scan8 layout.)
      - **Method (reusable): implied-prediction oracle.** For a diverging
        8×8 block, `residual = ours - our_traced_pred` (residuals parse
        byte-exact), then `implied_ffmpeg_pred = ref - residual` is matched
        against all 9 Intra_8×8 mode predictions computed from the
        *reference frame's* neighbours (reusing the crate's own
        `predict_8x8`). A 64/64 exact match identifies ffmpeg's mode
        unambiguously; comparing it with the mode our prediction matches
        separates mode-selection bugs from residual bugs. Implemented in
        `tests/dbg_hp352_localize.rs`.
      - **Remaining gap (narrowed to a single ±1 rounding issue):** after the
        MPM fix, a frame-wide implied-prediction sweep reports **zero mode
        mismatches** across all 440 8×8 blocks. The 352×288 clip is now at
        `max_abs_diff=1` with only **423/152064 samples** differing (99.72%
        exact), concentrated around MB(8,11)/(9,11): two DC-mode quadrants
        decode with a uniform ±1 shift (identical neighbour samples on both
        sides — so it is a DC-average or DC-dequant rounding divergence, not
        a mode/parse issue). Verified-not-the-cause this session: the IDCT
        pass order (FFmpeg runs columns-first in `ff_h264_idct8_add`; our
        rows-first empirically matches better because FFmpeg's `sl->mb`
        8×8 blocks are stored transposed relative to ours — the transposed
        dequant table + transposed CAVLC scan + columns-first order all
        compensate to the same arithmetic as our literal scan + rows-first),
        the dequant rounding algebra (FFmpeg's folded `(l·qmul+32)>>6` is
        algebraically identical to the spec's `(l·ls + 2^(5-s))>>(6-s)` for
        all s), and the nC context derivation (both are physical-adjacency).
        Next step: dump the DC coefficient level and qP for the diverging
        MB(8,11) quadrants and compare the two rounding expressions
        numerically.
      - Also ruled out this session: CAVLC 8×8 scan transposition (FFmpeg's
        `TRANSPOSE` at init is compensated by its own transposed `sl->mb`
        layout — the literal table is correct here, empirically verified),
        8×8 dequant position classes (transpose-symmetric), and the
        `predict_8x8` filtered-neighbour formulas (verbatim ffmpeg port).
        (Earlier sessions also fixed, independently: the `idct_8x8` pass-2
        axis/transpose bug — regression test
        (`transform::tests::eight_by_eight_horizontal_ac_varies_along_columns_not_rows`);
        a real bug worth fixing even though it did not change the
        conformance numbers of the time.)

      **Ruled out a second candidate, found via a real bug, then discovered the
      failure isn't 8×8-specific at all.** The CAVLC 8×8 residual interleave
      (`slice_data.rs`'s old `block64[4*k+sub]` mapping) was indeed wrong —
      fetched the real `libavcodec/h264_slice.c`/`h264_cavlc.c` at the pinned
      commit (`tpt-kinetix-kg fetch-source`) and found CAVLC's actual 8×8 scan
      is `zigzag_scan8x8_cavlc[i] = zigzag_scan8x8[(i/4) + 16*(i%4)]`, a
      genuinely different permutation from the naive interleave. Transcribed
      it verbatim as `CAVLC_SCAN8X8` in `transform.rs` (plus a new
      `INVERSE_ZIGZAG_8X8` table) and rewired `parse_intra_residuals`'s 8×8
      branch to use it — a real, FFmpeg-verified fix, kept. **But it also did
      not change the conformance numbers**, because the specific coefficients
      in the failing test block are all DC-only per CAVLC sub-stream (`k=0`),
      and the old and new formulas happen to agree exactly at `k=0` — so this
      test never actually exercised the part of the mapping that was wrong.

      Chasing this further with a per-macroblock trace (dumping raw CAVLC
      `nc`/`total_coeff`/coefficient values and the scaling list in use)
      showed MB(0,0)'s very first 8×8 block — flat DC-128 prediction, no
      neighbours, residual math independently re-verified by hand — decoding
      *correctly* per the (small) coefficients it parsed. The coefficients
      themselves just don't carry enough energy to explain ffmpeg's reference
      (residual ~0-1 vs. an actual +4..+20 gradient). That pointed at CAVLC
      parsing being wrong, not the transform.

      Then the actually-important test: **regenerate the exact same
      `mandelbrot` clip with `8x8dct=0` (plain CAVLC 4×4, no 8×8 transform
      involved at all) and it is *also* badly wrong** — `max_abs_diff=100`,
      4592/4608 samples differ, i.e. nearly the whole frame. This proves the
      root cause has **nothing to do with 8×8 transform, CABAC, or Phase F.4**
      — it's a pre-existing, more general CAVLC intra-decode bug that only
      manifests on real/high-frequency image content (`mandelbrot`); every
      other conformance test in this suite uses flat `testsrc` content that
      never triggers it. `predict_8x8`'s neighbour-clamping gap (Phase F.2)
      is therefore **not** the cause (it's 8×8-specific code; the bug
      reproduces with pure 4×4 prediction).

      **RESOLVED (2026-08-15).** Root cause: `parse_intra_macroblock`
      (`slice_data.rs`) read the `transform_size_8x8_flag` bit
      *unconditionally* for every `Intra_4x4` macroblock instead of gating it
      on the PPS's `transform_8x8_mode_flag` (§7.3.5.1 — that bit is only
      present in the bitstream at all when the PPS enables the 8×8
      transform). Baseline/Main-profile PPS always has that flag `false`, so
      every real `Intra_4x4` macroblock consumed one phantom bit too many,
      desyncing the rest of the CAVLC residual parse for that macroblock (and
      usually the whole slice) — surfacing as a bitstream-level `Cavlc` parse
      error, silently caught by `decode_impl` and falling back to the flat
      mid-grey scaffold frame (the "`max_abs_diff=100`, ~99% of samples
      differ" numbers above were the scaffold-vs-content diff, not a fine-grained
      pixel bug). Every prior CAVLC conformance test used flat `testsrc`
      content, which x264 always codes as `Intra_16x16` — the buggy branch was
      simply never exercised until `mandelbrot`'s high-frequency detail forced
      x264 to choose `Intra_4x4`. Found via a per-macroblock/per-block CAVLC
      trace (`DecodeTracer`, temporary `eprintln!` instrumentation) that
      localized the first divergence to MB(0,0)'s chroma-AC parse producing an
      out-of-range `total_zeros`/position — traced back through the whole
      macroblock to the unconditional bit read right after `mb_type`. Fixed by
      gating the read on `transform_8x8_mode` (matches the already-correct
      CABAC path in `parse_intra_macroblock_cabac`, which was never affected).
      Regression test: `tests/cavlc_intra4x4_conformance.rs` (mandelbrot,
      baseline profile, asserts ≥1 real `Intra_4x4` macroblock decoded and
      bit-exact vs ffmpeg, both deblock variants) — now bit-exact
      (`max_abs_diff=0`). `high_profile_8x8_conformance.rs` /
      `high_profile_conformance.rs` still fail (`max_abs_diff≈160-171`) —
      that's the distinct, still-open 8×8-transform-specific bug from Phase
      F.4 above (`predict_8x8` neighbour-clamping / dequant-IDCT wiring),
      unaffected by this fix and confirmed via `git stash` to pre-date it.

      **Further localization (uncommitted scratch harness, `tpt-kinetix-h264/
      examples/dbg_8x8_localize.rs`, per-macroblock max/avg diff dump — not
      committed, recreate similarly if needed):** on the same 64×48
      `mandelbrot` clip at `8x8dct=1`, **every** macroblock in the frame shows
      a nonzero diff (max 39-43 for 11 of the 12 macroblocks, one outlier —
      MB(0,2) — at max=160, matching the conformance test's headline number),
      not just the macroblocks that actually select the 8×8 transform. The
      matching `8x8dct=0` run of the *same* clip/generator is confirmed
      bit-exact (`max_abs_diff=0`), isolating the bug to the 8×8-specific
      code path (as expected) but showing it corrupts the whole frame rather
      than only the 8×8-coded blocks — consistent with a neighbour/prediction
      state bug that propagates from one macroblock into the next (e.g. a
      wrongly-updated "last mb was 8×8" neighbour-availability or MPM-context
      flag) rather than a per-block dequant/IDCT arithmetic bug, which would
      be expected to stay localized to the 8×8-coded blocks themselves. Not
      yet root-caused; worth checking `predict_8x8`'s neighbour bookkeeping
      and whatever in `slice_data.rs`/`reconstruct.rs` threads
      `transform_size_8x8_flag` state between consecutive macroblocks next.

#### Phase G.1 — PAFF: field-picture parsing
- [x] Thread the already-parsed `bottom_field_flag` (`slice.rs:169`, previously
      discarded as `_bottom_field_flag`) through slice/header state instead of
      dropping it — `SliceHeader` now carries `field_pic_flag`, `bottom_field_flag`,
      and `delta_pic_order_cnt_bottom` (§7.3.3); the slice header parser reads and
      stores them, and round-trip unit tests assert both field and frame pictures
      parse correctly
- [x] Implement field-picture POC derivation (§8.2.1.2/8.2.1.3, distinct from the
      existing frame-picture path) — `derive_pic_order_cnt` now takes
      `field_pic_flag`/`bottom_field_flag`/`delta_pic_order_cnt_bottom`; `PocState`
      tracks per-field `prev_top_field_order_cnt`/`prev_bottom_field_order_cnt`
      (the MSB/LSB predictor is derived from their max, per §8.2.1.1) so type-0
      (separate per-field `pic_order_cnt_lsb`) and type-2 (`base + 1` for the
      bottom field) field POC both derive correctly and are unit-tested

#### Phase G.2 — PAFF: field-picture reconstruction
- [x] Field-picture reference list construction (§8.2.4.2.5)
- [x] Field-based (odd/even scanline) macroblock reconstruction and output
       interleaving back into a full frame

  **Implemented (working tree, 2026-08-15):** the PAFF field-picture decode
  path in `decoder.rs::decode_interlaced` now handles both **I-field** and
  **P-field** pictures (the "`build_field_ref_list_l0` wired" half of this was
  the open item). Concretely:
  - §8.2.4.2.5 field reference lists: `ref_pic.rs::build_field_ref_list_l0`/
    `build_field_ref_list_l1` (which unfold each stored frame into its two
    field references, or pass through genuine field references) are now invoked
    from the new `decoder.rs::decode_interlaced_p_field` via `PicNumContext::
    new(..., field_pic_flag=true, ...)`. `FieldRef::planes` extracts the
    contiguous half-height luma/Cb/Cr planes for a referenced field (every-other-
    row sampling for frame references, identity for genuine field references).
  - Field-based reconstruction: `reconstruct.rs::reconstruct_inter_field_frame`
    reconstructs each field macroblock into a **half-height** buffer. The MB grid
    addresses field scanlines; inter MBs are motion-compensated at field parity
    by sampling the reference field planes with the (already field-unit) motion
    vector (`reconstruct_field_inter_luma`/`reconstruct_field_inter_chroma`),
    then the residual IDCT is added per 4×4 block as in the frame path.
  - Output interleaving: the half-height field is stored as a DPB field entry
    (`store_reference_picture` already carried `field_pic_flag`/`bottom_field_flag`
    since G.1), then `accumulate_field` pairs it with its complementary field and
    `interleave_fields` merges the two half-height planes into the full
    interlaced frame (top field → even scanlines, bottom → odd, §6.4.10.1).
  - Deblocking runs per-field on the half-height buffer (`deblock_field` helper),
    so it never crosses the field boundary.

  Unit tests added (no `ffmpeg` needed): `reconstruct::tests::
  field_ref_planes_extract_parity`, `field_p_skip_copies_reference_field`
  (a skip P-field MB with zero MV copies the reference field verbatim into the
  half-height output — the field analogue of `inter_skip_copies_reference`), and
  `decoder::tests::interleave_fields_places_top_and_bottom_parity`. All three
  pass; the rest of the `tpt-kinetix-h264` lib suite is unaffected (the only two
  failures are the pre-existing `field_mv_scaling_same_parity_doubles` and
  `predict_8x8_vertical`, which fail on `master` unmodified). `cargo clippy -p
  tpt-kinetix-h264` is clean.

  **Remaining gaps (not yet done):**
  - B-field pictures still `Fallback` (same structure as P-field — add
    `decode_interlaced_b_field` once B-field ref lists + temporal direct mode
    are wanted).
  - Field-intra 16×16 (and 8×8) DC Hadamard is applied with the frame ordering,
    not the field transform ordering (§8.4.2.2.1); pure-field I-slices with
    Intra_16×16 MBs are therefore not yet pixel-exact.
  - Field MV scaling (§8.4.1.3 `scale_field_mv_y`, already implemented in
    `mv.rs`) is not yet applied during field prediction — the dominant
    same-parity / field-from-field case (no scaling) is correct, but
    cross-parity or frame-from-field scaling is skipped.
  - No `ffmpeg` bit-exact conformance run (ffmpeg is unavailable in this
    environment); gated behind Phase G.5's PAFF corpus clip.

#### Phase G.3 — MBAFF: parsing
- [x] Parse `mb_field_decoding_flag` and macroblock-pair decode ordering
       (§7.3.4, §7.4.4) when `mb_adaptive_frame_field_flag` is set — SPS gained
       `mb_adaptive_frame_field_flag` (parsed, round-trip tested in
       `sps.rs::tests::sps_mb_adaptive_frame_field_flag_round_trips`); in MBAFF
       frames (`mb_adaptive_frame_field_flag && !field_pic_flag`)
       `slice_data.rs` reads `mb_field_decoding_flag` once per macroblock pair
       (CAVLC `parse_i_slice` and CABAC `parse_i_slice_cabac`, via the new
       `MbFieldDecodingFlagContext` in `entropy.rs`) and stores it on
       `Macroblock::mb_field_flag`. Reconstruction-side macroblock-pair ordering
       (neighbour derivation / output interleave) is still Phase G.4.

#### Phase G.4 — MBAFF: neighbour derivation + reconstruction

> **Updated 2026-08-15 (uncommitted):** new `src/mbaff.rs` (431 lines) adds
> both pieces per §6.4.10.1, cross-checked against FFmpeg's
> `fill_decode_neighbors`/`hl_decode_mb`. `place_mbaff_luma_pair`/
> `place_mbaff_chroma_pair` (field/frame-adaptive pair placement) are wired
> into `reconstruct.rs` and run for real MBAFF frames. `derive_neighbours`
> is now wired into the I-slice CAVLC/CABAC parsers (see below) via
> `slice_data.rs::NeighbourCtx`; P/B slices remain non-MBAFF-aware since they
> don't parse `mb_field_decoding_flag` yet (separate, larger gap, see below).
> Not re-validated against `ffmpeg` (blocked on G.5 corpus generation below).

- [x] Field/frame-adaptive reconstruction per macroblock pair — `mbaff.rs`'s
      `place_mbaff_luma_pair`/`place_mbaff_chroma_pair`, wired into
      `reconstruct.rs`
- [x] Adjust neighbour derivation (nC, MPM) for mixed field/frame macroblock
      pairs (§6.4.10.1) — **done for the I-slice CAVLC and CABAC parsers**
      (2026-08-15). New `slice_data.rs::NeighbourCtx` bundles the MBAFF state
      (`mb_aff`, `mb_rows`, the current pair's `mb_field_decoding_flag`, and a
      per-frame-MB `field_flags` array populated as each pair is decoded) and
      exposes `left_top()`, which calls `mbaff::derive_neighbours` when
      `mb_aff` is set and otherwise degenerates to the exact plain
      `mb_xy - 1` / `mb_xy - mb_cols` formula every call site used before this
      change — so every already-bit-exact non-MBAFF conformance path
      (CAVLC/CABAC I/P/B, `high_profile_8x8_*`, `p_frame_conformance`, etc.)
      is provably unaffected. Threaded through `mpm_pred_mode`,
      `mpm_pred_mode_8x8`, `luma_nc`, `chroma_nc`, `luma_cbf_neighbors`,
      `chroma_cbf_neighbors`, `cabac_cbp_neighbors`, `parse_intra_macroblock`,
      `parse_intra_macroblock_cabac`, and `parse_intra_residuals`;
      `parse_i_slice`/`parse_i_slice_cabac` build the per-pair `field_flags`
      array and construct a real `NeighbourCtx` per macroblock.
      **Scope note / remaining gap:** this only covers the I-slice parsers.
      Discovered while wiring this in: `parse_p_slice`/`parse_p_slice_cabac`/
      `parse_b_slice`/`parse_b_slice_cabac` don't read `mb_field_decoding_flag`
      at all yet (only the I-slice parsers do), so a real MBAFF P/B slice
      would already desync at the bitstream level before neighbour derivation
      even matters — P/B intra-macroblock and inter (motion-vector-
      prediction) neighbour lookups in those four parsers now take a
      `NeighbourCtx` parameter too (for the shared `parse_intra_macroblock`/
      `parse_intra_residuals`/neighbour-helper functions) but are passed
      `NeighbourCtx::NONE`, i.e. still non-MBAFF-aware — correct/honest given
      P/B slices don't parse the pair flag, but real work for a future P/B
      MBAFF phase: (1) add `mb_field_decoding_flag` parsing to all four P/B
      slice parsers (mirroring the I-slice CAVLC/CABAC read), (2) thread a
      real per-pair `NeighbourCtx` through them the same way, (3) extend
      `derive_neighbours`-style addressing to the `ref_idx_gt0_neighbors`/
      `amvd_sum` motion-vector-prediction helpers (currently still plain
      `mb_xy-1`/`mb_xy-mb_cols` inline arithmetic, untouched by this pass).

#### Phase G.5 — Validate interlaced decode
- [ ] Generate a PAFF corpus clip and a separate MBAFF corpus clip with
      `ffmpeg`; validate bit-exact decode vs `ffmpeg` for each independently

### H.264 — Phase H: conformance & capability flip

- [x] Cross-codec conformance harness vs `ffmpeg`: `tpt-kinetix-h264/tests/conformance_matrix.rs`
      enumerates a profile × entropy × frame-structure × deblock × resolution
      matrix (CAVLC/CABAC I/P/B, 4:2:0, progressive, 16-px-aligned, no 8×8) and
      asserts **bit-exact** (`max_abs_diff == 0`) decode vs `ffmpeg` for every
      supported cell; the already-present `*_conformance.rs` suites are the
      per-feature gated pixel-exact assertions. The harness additionally asserts
      the **honesty contract** for the unsupported subset (8×8 transform / High
      profile, interlaced PAFF): under `with_strict(true)` the decoder returns
      `KinetixError::NotPixelExact` rather than emitting wrong pixels. Gated on
      `ffmpeg` presence (skips on runners without it).
- [x] Update `H264Decoder::capabilities()` to the *actual* achieved state:
      `supports_inter_prediction = true` (P/B + B-frames are bit-exact),
      accurate `notes` (CAVLC/CABAC I/P/B bit-exact; 8×8 / interlaced /
      non-16-aligned still open). Stale claim that B-frames, CABAC P/B, and
      weighted prediction were unimplemented — contradicting the passing
      conformance suites — has been corrected. `tpt-kinetix-core` `capabilities.rs`
      and `tpt-kinetix-h264/README.md` status sections updated to match.
- [~] **Global `pixel_exact` flip — gated (NOT flipped).** The decoder is
      bit-exact for its supported subset, but `pixel_exact` is a *global* honesty
      flag, and genuine gaps remain: the 8×8 transform / High profile (Phase F),
      interlaced PAFF/MBAFF (Phase G), and a non-16-aligned-dimension crop-edge
      gap (Phase 12 A follow-up). Flipping the flag while those exist would make
      callers/CLI trust approximate output, directly contradicting the project's
      `NotPixelExact` honesty design. The flip stays `false` until Phases F/G and
      the crop-edge gap land; the `conformance_matrix.rs` gate asserts
      `!capabilities().pixel_exact` so the constraint is enforced in CI.
- [~] **NEW (2026-08-22): `conformance_matrix` cabac_p / cabac_b cells fail**
      (max_abs_diff≈127, full-frame scaffold → a decode *error* fallback, both
      deblock variants). Verified pre-existing at origin/master (`96a4db9`) —
      not caused by the 2026-08-22 MPM/CAVLC work. Needs its own root-cause
      pass before any `pixel_exact` flip discussion.
      - **2026-08-23 session — root cause narrowed substantially; the failure
        is a real CABAC *desync* inside `parse_p_slice_cabac`, not an
        unimplemented live-decoder path.** Reproduced minimally with
        `tpt-kinetix-h264/examples/dbg_cabac_p_matrix.rs` (generates the exact
        matrix-cell clip via ffmpeg, decodes, reports): the P slice fails with
        `Unsupported("end_of_slice_flag mismatch (P-CABAC)")`, i.e. the parser
        reaches MB11 but its terminate bin reads 0 instead of 1 → the
        arithmetic decode desynced somewhere upstream, and the whole P frame
        falls back to the grey scaffold (max_abs_diff=127).
      - **The standalone `cabac_pframe_conformance.rs` suites do NOT actually
        assert bit-exactness** — they print `[GAP]` and pass on any diff
        (`max_diff != 0` branch only logs). The same desync fires there
        ("P CABAC parse error" + max_abs_diff=127); "standalone passes" was
        vacuous for this path. The matrix cell is the first hard assertion.
      - **A static-content clip reproduces it with the minimal syntax**:
        `color=c=gray` IP clip (all-skip P frame — confirmed: the CAVLC encode
        of the same content is a bare `mb_skip_run`=12; ffmpeg's decoded frames
        are identical) fails identically, so the desync is exercisable with
        *nothing but* `mb_skip_flag` decisions + terminate bins. This rules out
        every inter-only element (sub_mb_type, ref_idx, mvd contexts, cbp,
        residual) as the *sole* cause for that repro and points at the
        `mb_skip_flag` context/init/engine-state evolution itself.
      - Verified-correct this session (do not re-audit): CABAC engine
        `decode_decision`/`decode_terminate` match a hand-computed spec §9.3.3.2
        trace on the failing payload byte-for-byte; PB0 init table entries
        11..13 = [(23,33),(23,2),(21,0)] match ffmpeg's
        `cabac_context_init_PB[0]`; mb_skip_flag ctxIdxInc (= condL+condU,
        cond = neighbour available && !skip) matches ffmpeg's
        `decode_cabac_mb_skip`; P mb_type ctx 14..17, sub_mb_type ctx 21..23,
        ref_idx ctx 54..59, mvd ctx 40/47 all match ffmpeg's h264_cabac.c;
        cbp/cbf neighbour conventions (incl. left_cbp low-nibble masking being
        irrelevant because ffmpeg's `decode_cabac_mb_cbp_luma` only ever reads
        bits 1/3 of left, and the 0x7CF-vs-0x00F unavailable sentinels being
        equivalent under those masks) verified against ffmpeg source fetched to
        repo-root scratch (`h264_cabac.c`, `h264_mvpred.h` — delete when done).
      - **2026-08-23 session #2 — DECISIVE BISECTION: the failure flips
        exactly at the CABAC context-init `preCtxState` 63/64 boundary.**
        Forced-QP static clips through the live decoder
        (`dbg_cabac_p_matrix.rs`, cases qp18/qp21/qp22/qp25/qp30):
        qp22, qp25, qp30 decode **bit-exact** (max_abs_diff=0); qp21, qp18,
        qp2 hit the end-of-slice mismatch. For mb_skip_flag-P ctxIdx11
        (m,n)=(23,33): raw=((23·qp)>>4)+33 = 64 at qp22 (>63 branch) vs 63 at
        qp21 (≤63 branch). Every other syntax element is identical between
        those clips (all-skip content ⇒ only mb_skip_flag bins + terminate),
        so the misbehaving component is the context-init `preCtxState ≤ 63`
        branch (our `CabacContext::init`: idx=63−raw, mps=0) — or something
        tightly coupled to it.
      - Equivalences PROVEN this session (do not re-audit): (a) engine
        head-to-head — our `CabacDecoder::decode_decision` and a faithful
        ffmpeg packed-engine transcription produce IDENTICAL bin sequences and
        identical range/offset trajectories on the same payload+init;
        (b) ffmpeg's `ff_h264_cabac_tables` mlps_state section unpacks to
        exactly our TL/TM incl. the pStateIdx==0 mps-flip rule (MPS:
        sec[128+s]=s+2; LPS: sec[127−s]; reached via the negative-index trick
        `s ^= lps_mask` with `ff_h264_mlps_state = tables+1024`);
        (c) init formula: ffmpeg's `pre = 2*(((m*qp)>>4)+n)-127;
        pre ^= pre>>31` is **bitwise-NOT for negatives** (= −pre−1, NOT abs!),
        giving packed = 126−2raw = 2(63−raw) for raw≤63 → unpacks to
        (63−raw, mps=0) — textually identical to ours; (d) slice-header parse
        verified field-by-field against an independent bit-reader replication
        (ends at bit 19/29 resp.; alignment/payload start correct — an earlier
        "payload might start earlier" hypothesis is DEAD: no start position
        makes complex-content clips parse).
      - Remaining paradox (precisely stated): with engines, tables, init,
        bytes, and positions all provably identical, ffmpeg nonetheless
        decodes the qp≤21 streams error-free while our parser desyncs. One
        concrete unexplored lead: `ff_init_cabac_decoder` (cabac.c:162) is
        **buffer-alignment dependent** — when `(uintptr_t)(buf+2)` is even it
        adds a constant `1<<9` WITHOUT consuming byte 3, else it adds
        `(byte3<<2)+2` and consumes it. Whether (and how) that changes
        decisions near the tolerance boundary of a mostly-LPS run is the next
        thing to model exactly (the probe transcription used the unconditional
        three-byte form). Also queued: build a self-authored C harness
        against `get_cabac_inline` for a per-bin oracle (no C toolchain on the
        Windows dev box — needs CI/Linux or an installed gcc).
      - New reusable harnesses left in-tree:
        `examples/dbg_cabac_p_matrix.rs` (matrix-cell repro + controlled
        clip variants), `examples/dbg_cabac_skip_probe.rs` (header field /
        bit-position dump, mb_skip_flag-only probe, start-shift sweep,
        splice-into-stream differential test vs ffmpeg, mini spec-exact CABAC
        encoder oracle — round-trip validated).
      - **2026-08-23 session #3 — RESOLVED for cabac_p (bit-exact); cabac_b
        now parses cleanly, residual gap is B-direct/bi-pred semantics.**
        Two real bugs fixed in the slice-data drivers (engine/tables/init were
        never the problem — the "63/64 bisection" was a red herring: at
        qp>=22 the desynced skip-flag reads still all returned 1, so all-skip
        *output* was coincidentally correct):
        1. **`end_of_slice_flag` was not decoded after skipped macroblocks**
           (`cabac_p.rs`/`cabac_b.rs`). Per §7.3.4 `slice_data()` it sits
           OUTSIDE `macroblock_layer()`, gated only on `mb_type != I_PCM` —
           x264 writes exactly `total-1` terminate bins (one before each MB
           except the first, none after the last MB; verified in
           x264 `encoder/encoder.c`), and ffmpeg reads one after every MB but
           exits on `eos || mb_y >= mb_height` (`h264_slice.c:2644-2678`).
           Fix: decode terminate after skip MBs too, and accept either value
           on the LAST MB (applied to cabac_i as well, whose final-MB check
           was silently relying on flush-bit luck).
        2. **Chroma-DC `coded_block_flag` context for an off-picture neighbour
           used the intra sentinel** (`dc_cbf_neighbor` → `None => true`).
           For INTER macroblocks FFmpeg fills unavailable-neighbour cbp with
           **0x00F** (`fill_decode_caches`: `CABAC && !IS_INTRA(mb_type) ?
           0 : 0x40404040`, top/left_cbp = `IS_INTRA ? 0x7CF : 0x00F`), i.e.
           chroma DC counts as NOT coded. The wrong ctx flipped the first
           coded inter MB's chroma-DC decision, which skipped 4 coefficient
           reads → bin-count desync for the rest of the slice (MB9+ garbage,
           MB8 off-by-small). Fixed in `decode_inter_residual_cabac`
           (intra paths keep the 0x7CF/"coded" convention).
        Also verified-identical to FFmpeg source this session (do not
        re-audit): full 1024-entry `CABAC_CTX_INIT_PB0` table (regex diff vs
        `cabac_context_init_PB[0]`: 0 mismatches), P mb_type bins (14..17),
        sub_mb_type (21..23), CBP luma/chroma ctx sequences (73..76/77..84
        incl. same-MB bit feedback), mb_qp_delta (60..63 + map), MVD prefix/
        suffix contexts and sign polarity (`get_cabac_bypass_sign`:
        bin 1 = NEGATIVE), amvd_sum neighbour selection.
        New synthetic repro clips added to `dbg_cabac_p_matrix.rs`
        (boxmove/colorswap/colorswap-with-partitions, forced-QP twins): all
        16 variants now decode `max_abs_diff=0`. Matrix state: cabac_p both
        deblock variants PASS bit-exact.
        Next step is the same twin/oracle
        method against `tests/dbg_cabac_twin.rs`.
      - **2026-08-23 session #4 — cabac_b progress: skip/direct MBs fixed,
        coded-B-MB path still open.** Three real fixes landed:
        1. **B `mb_type` CABAC tree rewritten** (`MbTypeBCabacContext::decode`):
           the old tree was an invented structure. It is now FFmpeg's exact
           `decode_cabac_mb_type` B branch — first bin ctxIdxInc = count of
           available left/top neighbours that are NOT B_Direct/B_Skip
           (`non_direct_neighbours`, threaded from `parse_b_slice_cabac`),
           then `27+3`/`27+5` L0/L1 pair, then the `27+4`/`27+5`×3 "bits"
           nibble (<8 → types 3..10; ==13 → intra-in-B; ==14 → type 11;
           ==15 → B_8x8; else `bits<<1|extra − 4`).
        2. **`ref_idx` gating in every coded-B arm**: ref_idx is only coded
           when `num_ref_idx_lX_active_minus1 > 0`; with a single reference it
           is implicitly 0 (§7.3.5.2). All seven sites in
           `parse_b_macroblock_cabac` now gate (BL0/BL1/Bi 16x16, 16x8/8x16
           per-list loops, B_8x8 per-quadrant).
        3. **Real spatial direct mode** (`mv.rs::derive_spatial_direct` +
           `apply_spatial_direct`, transcribed from FFmpeg
           `pred_spatial_direct_motion`): per-list min-ref/MV-selection over
           A/B/C(D) neighbours, list dropping when no neighbour uses a list,
           whole-MB zero fast path, and the colocated `col_zero_flag`
           adjustment. Colocated motion data is now persisted: `DpbEntry`
           `.mv_grid` (previously always `None`) is populated from
           `MvStore::to_grid_vec()` when reference P pictures are stored, and
           threaded through `parse_b_slice_cabac`/`parse_b_slice`/
           `predict_b_slice_mvs`.
        Result: the b_default clip's B frame rows 0–1 (all BSkip/direct MBs)
        are now **bit-exact vs ffmpeg** — spatial derivation + col_zero_flag
        verified working. Remaining gap: row 2's *coded* B macroblocks
        (bi-pred / intra-in-B / coded-direct-with-residual) still diverge
        (~977 samples). With `direct=none` x264 clips the entire frame is
        wrong → the bug is in the generic coded-B-MB element order or a
        residual-context issue specific to B slices, not in direct mode.
        Next steps: (a) dump our parsed per-element sequence for the first
        coded B MB and diff against FFmpeg's element order for e.g.
        B_L0_16x16 (suspects: intra-in-B suffix at ctxIdxOffset 32 semantics,
        and B-specific nC/cbf neighbour rules); (b) verify bi-pred
        reconstruction weighting against ffmpeg for BBi (combine_weighted
        Default average); (c) re-check `direct_spatial_mv_pred_flag`
        handling — temporal direct is still unimplemented (treated as
        spatial). New harness: `examples/dbg_cabac_b.rs` (IBP clip variants
        with direct=none/spatial/temporal + per-NAL feeding and per-MB diff).
      - **2026-08-23 session #5 — isolation matrix narrows the cabac_b bug to
        nonzero-MVD/intra-in-B coded MBs.** Built a variant matrix in
        `dbg_cabac_b.rs` (each = IBP testsrc/solid-colour clip, CABAC, main,
        deblock-offsets-0, per-NAL feeding):
        - `b_swap` (solid green→blue→red, no MVDs): **bit-exact** ✓ — B mb_type
          tree, cbp/qp/residual machinery, list plumbing, and B-frame output
          ordering all proven correct.
        - `b_forcel1` (past≠B=future solid colours, forces pure L1-coded MBs
          with mv=0): **bit-exact** ✓ — L1 reference selection + MC correct.
        - `b_default` (testsrc): rows of BSkip/direct MBs **bit-exact** ✓
          (spatial derivation + col_zero_flag working); only the row containing
          *coded* MBs diverges.
        - `b_nodirect` / `b_min` / `b_temporal` (all-coded B slices with
          NONZERO MVDs / intra-in-B / 16x8-B8x8 partitions): whole frame wrong.
        Conclusion: the residual bug tracks **nonzero MVD decoding or
        intra-in-B parsing** in the CABAC path. Fixes applied this session
        that are correct-and-kept regardless: (1) `mvd_l0_*`/`mvd_l1_*`
        contexts merged into one shared pair per component (FFmpeg
        `DECODE_CABAC_MB_MVD` passes ctxbase 40/47 with NO list parameter);
        (2) deblocking `derive_bs_pair` now applies the §8.7.2.1 bS=1 motion
        rule per prediction list (`ref_idx_l1`/`mv_l1` differences between
        neighbours also force bS=1 — previously only list 0 was compared, so
        B-slice edges with differing L1 MVs were left unfiltered);
        (3) `ref_idx` gating (num_ref_idx_lX_active == 1 → implicit 0) across
        all seven coded-B sites.
        Verified-unchanged (do not re-audit): `MbTypeBCabacContext::decode`
        now transcribes FFmpeg's `decode_cabac_mb_type` B branch verbatim
        (first-bin ctx = non-direct-neighbour count; 27+3/27+5 L0L1 pair;
        27+4/27+5×3 bits nibble; 13→intra@32, 14→11, 15→22); B_2PART_TABLE
        matches `ff_h264_b_mb_type_info[4..=21]`; intra-in-B suffix
        (`IntraMbTypeSuffixCabacContext`, ctxIdxOffset 32, intra_slice=0
        semantics incl. terminate-bin PCM check and folded ctx reuse)
        matches `decode_cabac_intra_mb_type`.
        NEXT STEP (queued): build the standalone C oracle harness with clang
        (toolchain now present on the dev box) — compile FFmpeg's actual
        `ff_init_cabac_decoder` + `get_cabac_inline` engine (cabac.c +
        cabac_functions.h, CABAC_BITS=16) with stub headers, initialize states
        via the verbatim `ff_h264_init_cabac_states` formula over
        `cabac_context_init_PB[0]`, transcribe the B-slice syntax loop
        line-by-line from `ff_h264_decode_mb_cabac`, and print per-element
        decisions + engine state; diff against our parser's traces on the
        failing `b_nodirect` payload to pinpoint the first divergent bin.
      - **2026-08-23 session #6 — ROOT CAUSE FOUND AND FIXED: missing
        `mb.motion` assignment in B_L0/B_L1/B_Bi 16x16 arms.** The B slice was
        silently erroring with `Unsupported("inter macroblock without
        motion")` and falling back to scaffold for every variant containing
        coded 16x16 B MBs (b_default row 2, b_nodirect/b_min/b_temporal whole
        frames). The error was invisible because `decoder/mod.rs`'s B-slice
        Err arm did `let _ = e;` before falling through. Three fixes:
        1. **`mb.motion = Some(motion)` added to arms 1 (B_L0_16x16),
           2 (B_L1_16x16), and 3 (B_Bi_16x16)** of `parse_b_macroblock_cabac`
           — previously only the 4..=21 and 22 arms attached motion data, so
           every plain 16×16 inter B MB parsed successfully but carried
           `motion: None`, crashing MV prediction at `mv.rs::inter_motion`.
        2. **Error surfaced**: replaced `let _ = e` with an eprintln in the
           B-slice Err arm so future parse failures aren't silent.
        3. **MVD context sharing** (from earlier in this session): L0/L1 MVDs
           share one context pair per component per FFmpeg
           `DECODE_CABAC_MB_MVD` (no list param on ctxbase).
        Results after all session #5+#6 fixes:
        - `b_min` (16x16-only L0/L1 B MBs): **bit-exact** ✓✓
        - `b_forcel1`: **bit-exact** ✓ (unchanged)
        - `b_swap`: **bit-exact** ✓ (unchanged)
        - `b_nodirect`: n=4355→393 samples wrong; only MB(3,2)=B_8x8 (diff
          146) + tiny MB(2,2) residual noise remain
        - `b_boxmv`: improved but nonzero-MVD sub-partition cases remain
        - `b_default`/`b_temporal`: skip/direct rows exact ✓; remaining diffs
          concentrated in intra-in-B / partitioned / bi-pred MBs
        Remaining work for full cabac_b bit-exactness: audit B_8x8 direct-
        sub-partition handling inside `apply_spatial_direct` (per-quadrant
        col_zero_flag uses colocated quadrant block, but derivation is shared
        from MB top-left — verify this matches FFmpeg's is_b8x8 branch);
        verify bi-pred combine_weighted Default average matches spec §8.4.3
        ((p0+p1+1)>>1 rounding); verify intra-in-B reconstruction paths.
      - **2026-08-23 session #7 — SubMbTypeBCabacContext tree rewritten.**
        The old implementation was a flat chain that didn't match FFmpeg's
        `decode_cabac_b_mb_sub_type` at all. Three bugs fixed:
        1. **L0/L1 discriminator read wrong context**: after ctx[1]=0, FFmpeg
           reads `state[39]` for the L0-vs-L1 decision, ours read `state[38]`.
        2. **Missing double state[39] read**: after the `state[38]` branch,
           FFmpeg reads `state[39]` TWICE sequentially (`type += 2*get(39);
           type += get(39)`) — our chain only read once per level.
        3. **Wrong tree shape**: FFmpeg has a nested structure where
           `state[38]=1 && state[39]=1` returns `11 + get(39)` (reading 39 a
           third time), not a chain of pairwise decisions.
        The new implementation is a verbatim transcription of the FFmpeg C
        code, including the multiple sequential `state[39]` reads.
        Isolation matrix after this fix:
        - `b_min` (16x16-only): still **bit-exact** ✓ (sub_mb_type not used)
        - `b_forcel1`/`b_swap`: still **bit-exact** ✓
        - `b_nodirect`: n=393→323 (improved); max=146→238 (mixed)
        - `b_default`/`b_temporal`/`b_boxmv`: similar or slightly changed
        The remaining failures are in partitioned B MB types (B_16x8/B_8x16/
        B_8x8) and/or bi-pred/intra-in-B MBs. All entropy-layer elements have
        now been audited verbatim against FFmpeg source and corrected. The
        next step is to investigate non-entropy semantics: MV prediction for
        partitioned B MBs (16x8/8x16 use directional shortcuts per §8.4.1.3.1),
        bi-pred MC averaging, and intra-in-B reconstruction.
      - **2026-08-23 session #8 — deblocking bS list-1 rule fixed; debug
        instrumentation added.** `derive_bs_pair` in deblock.rs now correctly
        evaluates the §8.7.2.1 bS=1 motion condition per prediction list:
        for each list LX, the MV/ref difference check applies only when BOTH
        the P and Q blocks actually use that list (`ref_idx_lX >= 0`). The
        previous naive comparison included LIST_NOT_USED sentinels, causing
        false bS=1 triggers between direct MBs (ref_l1=0) and L0-only MBs
        (ref_l1=-1). Also added `examples/dbg_cabac_b.rs` with 7 isolation
        clip variants + per-NAL feeding + per-MB luma diff maps.
      - **Current cabac_b status after sessions #5–#8:** The root cause of
        whole-frame scaffold fallback was found and fixed (missing mb.motion).
        B slices now parse without errors and produce real reconstruction.
        Isolation results: solid-colour clips, forced-L1 clips, and pure
        16x16-L0/L1 clips all decode bit-exact vs ffmpeg ✓. Remaining diffs
        are concentrated in testsrc clips with partitioned MB types
        (B_16x8/B_8x16/B_8x8), intra-in-B MBs, and/or nonzero-MVD bi-pred
        combinations — these need further investigation of the MC/reconstruction
        semantics for those specific MB types.
      - **2026-08-23 session #9 — F.4 confirmed CLOSED; cabac_b isolation
        sharpened; tracer instrumentation fixed; new oracle harnesses.**
        1. **Phase F.4 is closed**: the 352×288 mandelbrot clip decodes
           bit-exact (`dbg_hp352_localize.rs`: luma/cb/cr max_diff=0) and both
           high-profile matrix cells report `max_abs_diff=0`. The residual ±1
           DC issue was already fixed by the chroma-DC rounding correction in
           `transform.rs::chroma_dc_transform`. Item flipped to `[x]`.
        2. **Live-decoder P/B reconstruction ignored the caller's DecodeTracer**
           — `decoder/mod.rs::decode_slice` hardcoded `NoopTracer` for the
           parse + `reconstruct_inter_frame`/`reconstruct_b_frame` calls.
           Fixed: `decode_slice` is now generic over `T: DecodeTracer` and
           threads the caller's tracer through all slice parsing and B/P
           reconstruction, so `on_motion_comp`/`on_mb_parsed`/coefficient hooks
           now fire on real CABAC P/B streams via `decode_with_tracer`.
        3. **New harnesses** in `tpt-kinetix-h264/tests/dbg_b_implied_pred.rs`:
           (a) `p_boxmv_minimal` — a pure-IP CABAC clip with a moving box
           (nonzero MVDs over a static background) asserted BIT-EXACT vs
           ffmpeg. This is a new regression guard for the mvd path that every
           historical all-skip forced-QP cabac_p repro never exercised.
           (b) `b_implied_pred_oracle` — implied-prediction MV search for the
           failing IBP isolation clips.
        4. **Isolation findings for the remaining conformance_matrix cabac_b
           cell failure** (`max_abs_diff=104`, ~977/4608 samples):
           - Per-variant SAD-vs-reference pairing shows I frame bit-exact,
             P frame wrong (sad≈36026) and B frame wrong (sad≈29237) in the
             `b_boxmv` IBP clip — i.e. the failure already appears in *coded*
             (non-skip) inter MBs with nonzero MVDs, not only in direct/bi-pred
             or partitioned-B syntax.
           - The same moving-box content as a pure-IP stream decodes
             bit-exact, so the base CABAC P machinery (mvd contexts, cbp,
             residuals, MC) is sound; whatever breaks appears only when the
             stream also carries a B slice / B-slice state (e.g. DPB/ref-list
             setup, colocated grid construction feeding back into P, or x264
             choosing different mb modes under bframes=1).
           - Caveat recorded for future sessions: the implied-prediction
             oracle (residual = recon − pred; implied = ref − residual) is
             UNRELIABLE on black/white synthetic content because sample
             clamping at 0/255 destroys the residual estimate — run it on
             mid-range content (e.g. `testsrc`) instead of `color=c=black`.
           - NEXT STEPS (in order): (1) feed SPS+PPS+I+P only from the failing
             IBP clip and check whether the P frame alone still fails (separates
             "this particular P payload" from "B-slice state contamination");
             (2) if it fails, dump our parsed per-MB (type, ref_idx, mvd, cbp,
             qp_delta, total_coeff) for that slice and hand-verify against an
             independent decode of the same NAL; (3) clamp-aware implied-pred
             oracle on testsrc-based bframes=1 clips.
      - **2026-08-23 session #10 — DECISIVE ISOLATION: the cabac_b cell root
        cause is a CABAC MVD misparse in P slices carrying NONZERO MVDs, not
        B-slice semantics at all.** New experiments in
        `tests/dbg_b_implied_pred.rs` (all reproducible, ffmpeg-gated):
        1. `p_from_ibp_without_b`: feeding SPS+PPS+IDR+P only (no B NAL ever)
           still reproduces the failure (luma max=235, 1826/3072 wrong) ⇒ NOT
           B-slice-state contamination; this specific P payload misparses.
        2. `ibp_boxmv_cavlc`: identical content/settings with `cabac=0` decodes
           ALL THREE FRAMES bit-exact (sad=0 each) ⇒ base syntax/reconstruction
           (incl. intra-in-P under CAVLC) is correct.
        3. `ibp_testsrc_cabac`: static content IBP+CABAC decodes I and P
           bit-exact (P contains Intra16x16-in-P ⇒ CABAC intra-in-P parsing
           works) and B at sad=7 (near-exact; separate tiny residual).
        4. Failure signature in the failing P (`per-MB diff grid`
           `[0,0,3,1]/[2,6,234,235]/[10×4]`): everything through MB(1,0)
           exact; the FIRST divergence is MB(2,0), the first CODED inter MB
           with a NONZERO MVD. Our parse reads `mvd_l0=(16,0)` where the true
           motion (box moved 24 px from the only reference, predictor (0,0))
           requires mvd≈±96 quarter-pel ⇒ the MVD bin consumption diverges
           exactly there, and every later MB is garbage (phantom P8x8
           sub_types [2,3,0,1] on flat background, final terminate bin = 0 —
           previously masked by the lenient last-MB eos check from session #3).
        5. Contradiction to resolve next: the pure-IP moving-box clip ALSO has
           nonzero MVDs (val=48) and decodes bit-exact, so plain large-MVD
           bypass decoding works. Differences to probe: mvd magnitude (48 vs
           96 — different EG3 unary-prefix depth), the preceding-MB state
           (intra-in-P immediately before the first coded MB), or the amvd
           neighbour-sum inputs differing between the two streams. Suggested
           next tool: hand-trace the raw NAL bytes through ffmpeg's
           `decode_cabac_mb_mvd` (fetched via `tpt-kinetix-kg fetch-source`)
           for MB(2,0) of the failing slice, starting from the printed engine
           state `0x013e/0x000000dc` (post-mb_type), and compare bin-for-bin
           with our `MvdCabacContext::decode`.
      - Once this single desync is fixed, re-run `conformance_matrix`: the
        cabac_b cell failures likely collapse, since the B frames of the
        failing clips are otherwise near-exact (testsrc-IBP B sad=7 with
        BBi16x16 MBs decoded).
      - **2026-08-23 session #11 — MVD primitive PROVEN verbatim-identical to
        FFmpeg; trigger narrowed to intra-in-P → coded-inter-MB interaction.**
        1. Fetched `libavcodec/h264_cabac.c` + `cabac_functions.h` to repo root
           (`ff_h264_cabac.c`, `ff_cabac_functions.h` — keep until resolved).
           Line-by-line comparison against our `MvdCabacContext::decode` +
           `cabac_decode_mvd_component`: first-bin context selection
           (`(amvd-3)>>31`/`(amvd-33)>>31` trick ≡ our `<3/<33` branches),
           continuation loop (`idx=base+3`, `if(mvd<4) idx++`, cap at 9),
           EG3 bypass tail (unary `1<<k` with growing k, then k suffix bits),
           the 70-cap on stored amvd, AND sign polarity
           (`get_cabac_bypass_sign(c,-mvd)` ⇒ bit0=+val/bit1=−val) are ALL
           identical. The mvd primitive is NOT the bug.
        2. New experiments in `dbg_b_implied_pred.rs`:
           - `ibp_boxmv_smallmv` (6 px/frame ⇒ mvd≈48, no intra-in-P, all
             PL016x16): ALL THREE FRAMES BIT-EXACT including the B frame.
           - `ibp_bigmv_nointra` (same big motion, crf=10): x264 STILL codes
             intra-in-P (+ P8x8/P16x8) in the P slice and it still fails
             (P sad=30623); B frame sad=256 (near-exact).
        3. Conclusion: the failure needs the combination "INTRA-IN-P macroblock
           followed by a CODED inter MB whose MVD is large enough to sit near a
           decision threshold" — consistent with a CONTEXT-STATE divergence
           (wrong context variable evolved during the intra-in-P parse) rather
           than a bin-count error, because pixels through MB(1,0) stay exact
           and MB(2,0)'s structure still looks coherent while its mvd decodes
           as 16 instead of ~96. Candidate contexts to audit for the
           intra-in-P path (compare against ffmpeg's flat cabac_state indices):
           the IntraMbTypeSuffixCabacContext (spec ctxIdxOffset 17 for P), the
           luma-DC cbf read (always present for Intra16x16, even at cbp=0),
           chroma_pred_mode neighbour conditions (ffmpeg: left/top
           chroma_pred_mode_table != 0, ctx base 64), and whether any of these
           accidentally share context variables with the inter elements
           (mb_type/ref_idx/mvd/cbp/qp_delta) in `PbCabacSliceContexts`.
        4. Also useful: the failing slice ends with the engine running OUT of
           bytes (terminate read at offset exhausted ⇒ we consumed MORE bits
           than x264 wrote somewhere, i.e. an EXTRA bin is being consumed
           relative to the encoder — look for a missing gate that skips an
           element x264 did not write, most plausibly inside the intra-in-P
           branch).

