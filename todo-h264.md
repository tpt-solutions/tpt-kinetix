# TPT Kinetix — H.264 Decoder Todo

> Active work. See [todo.md](todo.md) for the project index.

## SESSION #32af (2026-08-29) — BUG 3 DONE; `mbaff_ibp` P frame BIT-EXACT; remaining divergence is the **B frame** (was mislabelled "P")

**BUG 3 (`get_dct8x8_allowed`) — DONE.** `sps.direct_8x8_inference_flag` is now
parsed (was discarded) and threaded through `parse_p_slice_cabac` /
`parse_b_slice_cabac` → `parse_p/b_macroblock_cabac`. `transform_size_8x8_flag`
is now gated exactly as ffmpeg's `dct8x8_allowed` (`h264_cabac_ref.c` L2347):
- P: `shape` 0/1/2 (16×16/16×8/8×16) always read; `shape` 3 (P_8x8) read iff
  every `sub_mb_type` is ≥8×8 (raw 0; raw 3 too when `!direct_8x8_inference`).
- B: `b_type_raw` 1..=21 (16×16 + 16×8/8×16) always read; raw 0 (B_Direct_16x16)
  gated on `direct_8x8_inference_flag`; raw 22 (B_8x8) gated on sub-types
  (raw ≤3, or ≤3/10..12 when `!direct_8x8_inference`).
- A concurrent-process WIP had gated P on `shape == 0` only and B on
  `matches!(1..=3)` only — **both wrong** (dropped the 16×8/8×16 case), which
  regressed `g6_cabac_ip` P (SAD 0→25335) and mis-parsed `mbaff_ibp`. Fixed here.
- 8 diagnostic tests/examples that call `parse_[pb]_slice_cabac` with the old
  arity were updated (added the `direct_8x8_inference_flag` arg).

**`mbaff_ibp` P frame — BIT-EXACT.** `tests/dbg_ibp_p_grid.rs` full-decodes the
clip and diffs our P output against ffmpeg's `select=pict_type,P` frame:
per-4×4 luma SAD grid is **all zero**. The parse grid + every absolute MV
already matched ffmpeg; BUG 3's fix closed the residual/recon gap.

**★ The remaining divergence is the B FRAME, not the P frame. ★**
Prior notes (#32ac/#32ad/#32ae "BUG 1") call it "mbaff_ibp P" — that label is
wrong. `dbg_g5_interlaced` emits ffmpeg frames in display order (I, B, P =
ff0, ff1, ff2); the SAD-43815 cell is `ff1` = the **B** frame, SAD-523 `ff2` =
the P frame (near-exact). `dbg_ibp_p_grid` (unambiguous `select=pict_type`)
confirms: our P = SAD 0.

**B_SUB_MB table bug FIXED** (`mv.rs`): `B_SUB_MB_PARTS` / `B_SUB_MB_DIR` /
`b8x8_sub_rect` / `cabac_b.rs::b8x8_sub_dims` assumed a grouped index layout
`[Direct; L0×4; L1×4; Bi×4]` with parts `[1;1,2,2,4;…]`. The actual spec
Table 7-18 / ffmpeg `ff_h264_b_sub_mb_type_info` order (which both the CABAC
`decode_cabac_b_mb_sub_type` return value and CAVLC `ue(v)` index directly) is
`[Direct; {L0,L1,Bi}_8x8; {L0,L1}×{8x4,4x8}, {Bi}×{8x4,4x8}; {L0,L1,Bi}_4x4]`
with parts `[1;1,1,1;2,2,2,2,2,2;4,4,4]`. Latent because progressive
`b_frame_conformance`'s fixture never uses a B_8x8 sub-type ≥2. After the fix
`mbaff_ibp` B: **`dbg_ibp_p_grid` gB(1,3) now parses exactly like ffmpeg**
(sub_types→dirs [L1,L1,L0,Direct], MVs L0{(0,0)} L1{(0,0),(-42,0)} match
export_mvs); B-slice SAD **43815 → 13959** (skipLF harness).

**B_8x8 mvd decode ORDER bug FIXED** (`cabac_b.rs`): the B_8x8 sub-partition
mvd loop was part-outer/list-inner; ffmpeg (`h264_cabac_ref.c` L2140) is
**list-outer/part-inner**. The order changes which `l0_mvd_abs`/`l1_mvd_abs`
within-MB cells `amvd_sum` sees while decoding later partitions → a real CABAC
engine desync on any B_8x8 with both lists active. After the fix `mbaff_ibp` B:
**every MB parses bit-identical to ffmpeg's `-debug mb_type` grid**
(`d d d d / < d d d / d d d d / X- X+ X- D`) and the diff collapses to a single
MB: **gB(3,3) = B_Direct_16x16 luma only** (chroma bit-exact). B-slice g6 SAD
**43815 → 6272**, max 77.

**gB(3,3) FIXED — `mbaff_ibp` B frame BIT-EXACT.** Not a direct-MV or
col_zero_flag issue: the MVs were already (0,0)/(0,0). Root cause was a missing
**8×8-transform branch in `reconstruct_b_inter_luma`** (`reconstruct.rs`). That
MB is B_Direct_16x16 with `transform_size_8x8_flag=1` (cbp_luma=0xf); the B
inter-luma recon only ever did the 4×4 path, reading the all-zero `luma_coeffs`
array instead of `luma_coeffs_8x8` → whole-MB luma residual dropped (chroma has
no 8×8 transform, so it stayed exact). Added a bi-pred 8×8 branch mirroring the
P-slice `reconstruct_inter_luma` path (per-8×8 MC of both lists with the
top-left cell MV, per-quadrant `combine_weighted`, `dequant_idct_8x8_scan` with
inter scaling slot 1 / ZIGZAG_8X8). `dbg_g6_mbaff_deblock` frame#2 (B) gate-ON
luma **SAD 6272 → 0, max 0**. Regression: b_frame / cabac / cabac_pframe /
conformance_matrix / high_profile_8x8 (+cabac) / dbg_ibp_p_grid all green.

NOTE: a concurrent process left `decoder/interlaced.rs:256` referencing
non-existent fields `coded_block_pattern_luma` / `intra_pred_mode` (in a
`KINETIX_PAFF_DBG` block, not under cfg(test)) — breaks the lib build; not
touched here.

**A5 status:** committed in `fd77230` (g6 clips + assertions + `cabac_p.rs`
8×8 scan-perm fix). `dbg_g6_mbaff_deblock` `g6_cabac_ip`/`g6_cabac_ibp` I+P+B
pins all green. Full `tpt-kinetix-h264` test suite green (incl. the 8 repaired
diagnostic tests).

### REMAINING WORK (supersedes #32ae's BUG 1/BUG 3 lists)

**BUG 3 — DONE** (`get_dct8x8_allowed` / inter `transform_size_8x8_flag`).
**`mbaff_ibp` P frame — DONE** (bit-exact, `dbg_ibp_p_grid` all-zero SAD).

- [ ] **1. `mbaff_ibp` B frame — one MB left.** SAD 43815→6272; diff is now a
      single MB `gB(3,3)` = `B_Direct_16x16` spatial-direct, luma only (chroma
      bit-exact). Dump gB(3,3)'s final `mv_store` + `colocated` cells in the full
      decoder vs ffmpeg export_mvs (0,0)/(0,0). Check `apply_spatial_direct`
      indexes `colocated.get(mb_idx)` on the frame grid (not pair-scan) and that
      the stored P-frame `mv_grid` is frame-grid order. Then `dbg_ibp_p_grid`
      gB → assert all-zero; pin.
- [~] **2. PAFF B-field** — 2026-08-30, mostly done. Root cause was NOT B-frames
      (fixtures are I/P) and NOT entropy. Three bugs, all committed:
  - [x] **2a/2b.** `FIELD_SCAN_4X4` was mis-transcribed (scan pos 6/7/9/11/13) —
        zero coverage since the only field fixture is all Intra_8×8. Fixed
        (`transform.rs`, commit 4835979); lib test added. → PAFF CAVLC I-field
        bit-exact.
  - [x] **2b'.** CABAC PAFF path never selected the field residual contexts
        (`cur_pair_field` hard-`false` outside MBAFF). Fixed in cabac_i/p/b
        (`= field_pic_flag`). → CABAC I/P/B field residuals now match CAVLC.
  - [x] **2c.** `output_frame` clobber: a completed PAFF pair emitted, then a
        later undecodable field in the same packet overwrote it with the grey
        scaffold. Guarded with `interlaced_frame_emitted` (`decoder/mod.rs`).
  - [x] **2c'.** DPB sliding window counted field entries not frames → 2nd field
        of a pair evicted the 1st. `Dpb::num_ref_frames()` (commit 19d888e). →
        `paff_b_field` frame#0 P-field max_diff 246→~20.
  - [x] `dbg_paff_i_fields` now hard-asserts bit-exact (4 frames, deblock off).
  - [ ] **2d.** Remaining: PAFF **field deblocking** (~6–20 residual on ~800px,
        `Self::deblock_field`); `paff_b_field` frame_num=1 P-top field still
        Fallbacks (parse_p_slice_cabac Err — investigate ref-list / field MC).
  - [ ] **2e.** flip `dbg_paff_b_field` to a hard bit-exact assertion once 2d done.
- [ ] **3. G.5a.** Pin every currently-bit-exact MBAFF frame in
      `dbg_g5_interlaced` / `dbg_g6_mbaff_deblock` as hard assertions.
- [ ] **4. G.5b.** Add one real PAFF corpus clip + one real MBAFF corpus clip;
      assert bit-exact vs ffmpeg.
- [ ] **5. G.5c.** non-16 crop: one `crop_right=10` clip through `dbg_g6`;
      assert bit-exact (finishes #32s).
- [ ] **6. H — `pixel_exact` flip.** Flip `capabilities().pixel_exact` for the
      covered subset; update README status table; `just conformance` second run
      (`--strict`) passes.

## SESSION #32ae (2026-08-29) — REMAINING WORK BROKEN DOWN: every step is one run with a binary pass/fail

Current state after #32ac/#32ad: **CABAC MBAFF I/P/B bit-exact** vs fully-filtered
ffmpeg (`dbg_g6_mbaff_deblock` `g6_cabac_ip` P, `g6_cabac_ibp` I+B all maxdiff 0,
pinned). Three things left before `pixel_exact`: (1) `mbaff_ibp` P frame CABAC
(SAD ≈18300 g6 / 43815 skipLF), (2) PAFF B-field (max_diff 126, CAVLC≡CABAC),
(3) latent inter `transform_size_8x8_flag`. Then G.5 + flip.

Method that worked for #32ac (do not deviate): ffmpeg-engine oracle
(`tests/dbg_mbaff_p_ffengine_oracle.rs`, has `bypass()`, agrees bin-for-bin
through MB9) → first divergent bin → one context/table fix. No open-ended audits.

### BUG 1 — CABAC `mbaff_ibp` P frame (SAD ≈18300)

- [ ] **1a. Diff map.** Run `dbg_g5_i1_diffmap` on `mbaff_ibp` P frame. Deliverable:
      list of MBs with maxdiff > 4.
- [ ] **1b. Type vs recon split.** For the first bad MB: `KINETIX_BINTRACE` crate
      parse + `ffmpeg -debug mb_type` same grid pos. Compare mb_type only.
      → misparse (branch 1c-type) or residual/recon error (branch 1c-recon).
- [ ] **1c-type.** Extend `dbg_mbaff_p_ffengine_oracle` to replay to that MB's
      `mb_type` bins; diff ctxIdx + value bin-for-bin. First mismatch = the bug.
- [ ] **1c-recon.** Check `transform_size_8x8_flag` for that MB vs ffmpeg. crate
      `true` + `cbp&15 != 0` ⇒ this is BUG 3, go there. `t8` matches ⇒ dump parsed
      residual coeffs vs ffmpeg residual trace for that one MB.
- [ ] **1d. Fix + pin.** Apply the one-line fix. `dbg_g6_mbaff_deblock`
      `g6_cabac_ibp` P frame → assert SAD 0, add hard assertion.
- [ ] **1e. Regression.** `conformance_matrix`, `cabac_conformance`,
      `b_frame_conformance`, lib all green.

### BUG 2 — PAFF B-field (max_diff 126, CAVLC ≡ CABAC ⇒ not entropy)

- [ ] **2a. Bisect: intra-only PAFF vector.** Build an IDR-only PAFF field-pair
      stream (no P/B). Decode. Fails ⇒ field reconstruction/pairing bug (2b).
      Passes ⇒ ref-list / DPB bug (2c).
- [ ] **2b-recon.** In `decode_interlaced` I-field path: assert both fields decode
      to `Frame` not `Fallback`; assert `field_accum` holds exactly one field when
      the second arrives; assert `finalize_field` interleaves at the right parity.
      One assertion trips = the bug.
- [ ] **2c-reflist.** Log DPB size + entry POC/parity inside `build_field_ref_list_l0`
      at the P-field call. Empty ⇒ fix field `store_reference_picture`. Non-empty
      wrong order ⇒ fix §8.2.4.2.5 ordering.
- [ ] **2d. Re-measure.** P-field decodes without `Fallback` → re-check max_diff.
      Still off ⇒ normal field-MC bug, per-MB diff map (as 1a).
- [ ] **2e. Pin.** `dbg_paff_b_field` gets a hard bit-exact assertion (currently
      only captures the failing state).

### BUG 3 — latent inter `transform_size_8x8_flag` (CABAC P/B path never reads it)

- [ ] **3a. Oracle clip.** One High-profile CABAC P clip whose first coded inter
      MB has `cbp&15 != 0`. Get ffmpeg's `t8` value for that MB via trace.
- [ ] **3b. Re-land prototype.** Read bin after CBP gated by `get_dct8x8_allowed`
      + 8×8 residual branch in `decode_inter_residual_cabac`. 3a's oracle → assert
      `t8` bin matches.
- [ ] **3c. B-slice thread.** Thread `direct_8x8_inference_flag` into the B-slice
      `get_dct8x8_allowed` sub-type check (hypothesised cause of the earlier
      `mbaff_ibp` regression). Re-test.
- [ ] **3d. Regression.** Progressive `cabac_conformance` / `high8x8` / `b_frame`
      stay bit-exact.

### THEN — G.5 + `pixel_exact` flip (each is one clip + one assertion)

- [ ] **G.5a.** Pin every currently-bit-exact MBAFF frame in `dbg_g5_interlaced` /
      `dbg_g6_mbaff_deblock` as a hard assertion (lock in #32ac/#32ad).
- [ ] **G.5b.** Add one real PAFF corpus clip + one MBAFF corpus clip; assert
      bit-exact vs ffmpeg.
- [ ] **G.5c.** non-16 crop: one `crop_right=10` clip through `dbg_g6`; assert
      bit-exact (finishes #32s).
- [ ] **H.** Flip `capabilities().pixel_exact` for the covered subset; update
      README status table; `just conformance` second run (`--strict`) passes.

## SESSION #32ad (2026-08-29) — A5: fully-filtered CABAC MBAFF P/B regression lock + intra-8×8 scan-perm fix

**A5 regression lock landed.** `dbg_g6_mbaff_deblock` gained two CABAC MBAFF
clips (`g6_cabac_ip`, `g6_cabac_ibp`) decoded against ffmpeg's FULLY-FILTERED
reference (previously the only CABAC MBAFF P/B signal was the `-skip_loop_filter`
`dbg_g5_interlaced` harness). Result: **`g6_cabac_ip` P frame and `g6_cabac_ibp`
I+B frames are BIT-EXACT** (luma+chroma maxdiff 0) — the SAD ≈493/523 seen on
the skipLF harness was a harness artefact, confirmed. New hard assertions pin
those frames. `g6_cabac_ibp` P frame (emitted last) still diverges (best luma
SAD ≈18300, = the `mbaff_ibp` P 43815 bug) — left un-pinned, tracked separately.

**Fix:** `cabac_p.rs` intra-8×8 residual store dropped the stray
`INVERSE_ZIGZAG_8X8[scan_pos]` remap (double-permutation), mirroring the inter
path in `cabac_b.rs`. `decode_block_8x8` already returns scan-position order,
which every `dequant_idct_8x8_scan(&luma_coeffs_8x8[..], .., &ZIGZAG_8X8)` recon
path expects. `high_profile_8x8_cabac_conformance` never caught it (fixture 8×8
blocks are DC-dominant → permutation ≈ identity there). 262 lib tests,
`conformance_matrix`, `cabac_conformance`, `b_frame_conformance`,
`high_profile_8x8_cabac_conformance`, `dbg_paff_b_field`, `dbg_g6_mbaff_deblock`
all green. No commit (concurrent process active on the same files).

REMAINING: `mbaff_ibp` P (SAD 43815 / g6 ≈18300) — still open, separate bug.
**Localized this session (read-only):** `g6_cabac_ibp` P frame diff lives in the
bottom MB-row — grid MBs (1,3),(2,3),(3,3) catastrophic (~220/256 samples, max
113), (0,3)/(3,2) near-clean. ffmpeg `-debug mb_type` P grid:
`row0 S S S S / row1 > S S S / row2 > I > > / row3 >- >- >- >+`. In MBAFF
pair-scan order the desync starts right after the **intra `I_16x16` MB at grid
(1,2)** — its pair-bottom (1,3) is the first broken MB. This is the **first clip
to exercise an intra MB inside an MBAFF P slice** (`mbaff_ip` P was all-inter),
so the bug is in `parse_p_macroblock_cabac`'s `None` branch →
`parse_intra_mb_cabac_pb` (cabac_b.rs:1404): a bin miscount or an unpopulated
neighbour-context field for I_16x16 under MBAFF.

**Narrowed further (`tests/dbg_ibp_p_grid.rs`, new):** the crate's CABAC-parsed
P-slice `mb_type` grid **matches ffmpeg exactly** — incl. g(3,3)=P_8x8 (ffmpeg
`>+`), g(1,2)=Intra16x16{mode0,cbpC2,cbpL0}, row3 = 3×P16x8 + P8x8. So it is
NOT a mb_type misparse (unlike the #32ac `mbaff_ip` case). The residual element
sequence in `parse_intra_mb_cabac_pb` is **byte-identical** to the proven
I-slice `parse_intra_macroblock_cabac` (diffed line-by-line: same cats, order,
neighbour calls). ⇒ the desync is a wrong **bin VALUE / ctxIdxInc** inside the
I_16x16 parse of g(1,2), most likely: (a) the intra-suffix `mb_type` binariz-
ation/ctxIdxInc (ctxIdxOffset 17, shared-ctx-17 sync) — exercised by progressive
CABAC-P so proven there, but MBAFF changes nothing in it → less likely; (b) a
`coded_block_flag` ctxIdxInc where the `None` (unavailable) neighbour + intra-
current ⇒ 1 rule, or a skipped-MB neighbour, is mishandled by `dc_cbf_neighbor`/
`luma_cbf_neighbors`/`chroma_cbf_neighbors` under the MBAFF `nctx`; (c)
`nctx.is_field()` fed to `decode_block` (should be false for this frame pair).
NEXT: extend `dbg_mbaff_p_ffengine_oracle` past MB9 through MB10 (g(1,2)) element
by element, diff post-MB engine `range`/`low` vs the crate; or add per-element
engine-state BINTRACE to `parse_intra_mb_cabac_pb` and bisect.

**Deeper diagnosis (this session, vs commit `fd77230`):** parse is 100% correct —
`tests/dbg_ibp_p_grid.rs` confirms crate's P-slice `mb_type`/`cbp`/`sub_mb_type`
grid AND **absolute MVs** all match ffmpeg exactly (ffmpeg `-flags2 +export_mvs`
via PyAV: g(0,3)=P16x8 {(0,0),(86,0)}, g(1,3)=P16x8 {(0,2),(85,0)},
g(2,3)=P16x8 {(0,1),(85,0)}, g(3,3)=P8x8 {(-32,52),(0,1),(0,2),(9,56)} — crate
reproduces all). No persistent CABAC engine desync: grid MBs g(2,2)/g(3,2)
(decode order AFTER the intra MB and after g(1,3)) are BIT-EXACT; only
g(1,3)/g(2,3)/g(3,3) (coded-inter pair-bottom, cols 1-3) are wrong, root =
g(1,3), cascading left via g(2,3)/g(3,3)'s broken left-neighbour. Deblock
ablations don't move the SAD ⇒ pre-deblock reconstruction. g(0,3) [clean] top
MV is half-pel (86); g(1,3)/g(2,3) [broken] top MVs are quarter-pel (85). ⇒
bug is in **MC/residual for coded-inter pair-bottom MBs in the frame-coded
MBAFF P reconstruction path** (`reconstruct_inter_frame_ex` → `reconstruct_inter_luma`,
same progressive fns, mb_field_flag=false), NOT the parser. `parse_intra_mb_cabac_pb`
residual structure verified byte-identical to the proven I-slice path;
suffix `mb_type` contexts (ctxIdx 17-20) verified vs `h264_cabac_ref.c`
`decode_cabac_intra_mb_type(_,17,0)`.

**⚠️ 2026-08-29: a concurrent process's UNCOMMITTED edits to `cabac_p.rs` /
`cabac_b.rs` / `interlaced.rs` (threading a new `direct_8x8_inference_flag` param
into `parse_p_slice_cabac`, BUG 3c) have REGRESSED the CABAC MBAFF P path** —
`g6_cabac_ip` P frame SAD 0 → 25335, `dbg_ibp_p_grid` now mis-decodes MB10 as
skip instead of I_16x16, `dbg_g6_mbaff_deblock` A5 assertion fails. Progressive
conformance stays green. The A5 regression lock is doing its job. `mbaff_ibp` P
work is blocked until that lands / stabilises.

## SESSION #32ac (2026-08-29) — ★ ROOT CAUSE FOUND & FIXED: `mb_field_decoding_flag` CABAC context init used the I-slice table for P/B slices ★

**Committed d1c5c53.** The CABAC MBAFF P/B desync (#32aa: MB9 `mb_type` ctxIdx
15 misdecodes) is `MbFieldDecodingFlagContext::new` initialising ctxIdx 70..=72
from `CABAC_CTX_INIT_I` **regardless of slice type**. Spec §9.3.1.2 keys these
from the `cabac_init_idc` table for P/B slices — `I[70] = (0,11)` vs
`PB0[70] = (0,45)`, genuinely different. Only MBAFF frames ever decode
`mb_field_decoding_flag`, so the wrong init silently drifted the arithmetic
engine's `range` (offset stayed synced) on **every MBAFF P/B pair** — which is
exactly why progressive CABAC P/B conformance was bit-exact while MBAFF P/B was
broken, and why #32y/#32z's CBP/ref_idx/t8 fixes (all *downstream* of the
`mb_field` decode) couldn't help.

**Proof — `tests/dbg_mbaff_p_ffengine_oracle.rs`:** drives an independent
from-scratch port of ffmpeg's `get_cabac`/`get_cabac_terminate` (tables parsed
from `cabac_ref.c`, **with the u8-wrap fix** — ffmpeg stores RangeLPS ≥ 128 as
negative `int8` literals in a `uint8_t` table; `dbg_engine_diff.rs` parses them
as `i32` and *guards around* them, so its `FfEngine` had never validated a
large-range low-pStateIdx decode = MB0's first skip bin here) + the crate
`CabacDecoder`, shared context model, replaying ffmpeg's exact P-MBAFF element
sequence (10 skip bins + 4 terminates + `mb_field_decoding_flag` + `mb_type`).
Both engines agree bin-for-bin. With ctxIdx 70 from the **PB** table →
`ctx15 = 1` → 16x8 (matches ffmpeg's `export_mvs`). With ctxIdx 70 from the
**I** table (`ORACLE_FIELD_I_INIT=1`) → `ctx15 = 0` → P_8x8 (reproduces the
pre-fix crate output). Engine offset identical in both cases; only `range`
drifts.

**FIX:** added `MbFieldDecodingFlagContext::new_pb(slice_qp, cabac_init_idc)`
(uses `init_pb_ctx`); `PbCabacSliceContexts::new_p`/`new_b` now call it.
`CabacSliceContexts::new` (MBAFF **I**-slice) keeps `::new` (I-init, correct —
`g6_cabac_i` stays bit-exact). New lib test
`mb_field_context_pb_init_differs_from_i_init`.

**RESULT** (`dbg_g5_interlaced`, `-skip_loop_filter` ref):
- `mbaff_ip` P: SAD **48461 → 9842**
- `mbaff_ibp` B: SAD **73651 → 523** (≈ skip-loop-filter harness artefact —
  near bit-exact)
- `mbaff_ibp` P: 43815 (still off — more bugs remain for the P path)
- lib 262/262, `conformance_matrix` 15/15, `cabac_conformance`,
  `b_frame_conformance`, `dbg_g6_mbaff_deblock` all green — no progressive
  regression.

REMAINING for CABAC MBAFF P/B: `mbaff_ip` P still 9842 (not 0) and `mbaff_ibp`
P 43815 — a second gap past the field-flag fix. The remaining error is in
MB11/MB15 (both PL016x16, cbp=0x2f, t8=true) — MVs are correct, so the issue
is in the inter 8×8 residual parse or dequant/idct path.

**2026-08-29 (this session):** Added `bypass()` method to the `FfEngine` in
`dbg_mbaff_p_ffengine_oracle.rs` (matching the validated implementation in
`dbg_engine_diff.rs`) to enable extending the oracle past `mb_type` into the
MVD/residual. The oracle confirms the engine agrees with ffmpeg bin-for-bin
through MB9's `mb_type` (ctx15=1, P_L0_L0_16x8). Next step: extend the oracle
to decode MB9's MVD + CBP + residual + MB10 skip + MB11 `mb_type`/MVD/CBP to
its `transform_size_8x8` bin, diff vs the crate parser. 262 lib tests pass.
No `git commit` calls.

**Localized (same session):** `dbg_g5_i1_diffmap` after the fix — every MB is
now small-diff (max ≤4, ≈ skip-loop-filter harness artefact) EXCEPT
**MB(1,3)=MB11 and MB(3,3)=MB15** (both ~240/256 differ, max ~113). Both are
`PL016x16`, `cbp=0x2f` (full), and **`transform_size_8x8_flag` (inter) decodes
`true`** → the 8×8 residual path. The other coded MBs (MB9/MB13 = P16x8,
`t8=false`) are now fine.
- **MVs are CORRECT** (`dbg_mbaff_cabac_vs_cavlc` MV-grid dump vs ffmpeg
  `export_mvs`): MB11 = (0,0), MB15 = (0,0), MB9/MB13 = 16x8 top (+43,0) —
  all match. So the remaining error is NOT motion.
- Disabling the inter-8×8 residual recon (`KINETIX_DBG_NO_INTER8X8_RECON`,
  temp) barely changes MB11/MB15 — expected either way (a wrong heavy residual
  and a zero residual both differ from ffmpeg's correct heavy residual by
  similar magnitude), so it doesn't discriminate.
- Removing the inter t8 bin entirely (`KINETIX_NO_INTER_T8`, temp) makes SAD
  **worse** (9842→32632) ⇒ ffmpeg DOES read the bin; the concurrent #32y
  read is right to be present.

⇒ Open question: does ffmpeg decode `t8 = true` for MB11/MB15 (then the inter
8×8 residual **parse or dequant/idct** is wrong), or `false` (then the crate's
t8 bin *value* is wrong — context or a preceding desync in MB9's residual)?
The intra 8×8 path shares `decode_block_8x8` + `dequant_idct_8x8_scan(...,
ZIGZAG_8X8)` and is bit-exact (`high8x8_i`), so if it's a value bug it's
upstream. NEXT: extend `dbg_mbaff_p_ffengine_oracle` past MB9's `mb_type`
through MB9's MVD + CBP + **residual** + MB10 skip + MB11 `mb_type`/MVD/CBP to
its `transform_size_8x8` bin, diff vs the crate parser.

**RESOLVED for MB11 (2026-08-29, later): CABAC 8×8 residual was stored with a
double-permutation.** `decode_block_8x8` returns coefficients in
**scan-position order** (`out[scan_pos] = level`) — exactly what
`dequant_idct_8x8_scan(coeffs, …, ZIGZAG_8X8)` expects
(`block[ZIGZAG_8X8[z]] = dequant(coeffs[z])`). But both the intra
(`cabac_p.rs`) and inter (`cabac_b.rs`) parse paths ran
`coeffs_zz[INVERSE_ZIGZAG_8X8[scan_pos]] = level` first — treating a scan index
as a raster index, scrambling every non-DC coefficient. (CAVLC is fine — its
`INVERSE_ZIGZAG_8X8[cavlc_raster]` input genuinely *is* raster-order.)
FIX (`cabac_b.rs` only so far): `mb.luma_coeffs_8x8[blk8] = coeffs_scan`
directly. `mbaff_ip` MB(1,3): **236/256 differ → 60/256** (max 113 → 2);
`mbaff_ip` P SAD **9842 → 7269**. `conformance_matrix` (incl. `high8x8_i`),
`high_profile_8x8_cabac_conformance`, `cabac_conformance`,
`b_frame_conformance` all still bit-exact; 262 lib tests pass.
- The **intra** path (`cabac_p.rs:190`) has the identical bug. Applying the
  same fix there kept all conformance green BUT broke the concurrent
  `dbg_paff_b_field` harness (a size-assumption OOB, since fixed with a guard)
  — reverted the intra change pending a closer look at why no intra 8×8
  conformance clip catches it (likely the mandelbrot fixture's 8×8 blocks are
  near-diagonal so the permutation is close to identity for the significant
  low-frequency coeffs).
- **MB15 RESOLVED (2026-08-29): skip MBs didn't clear `prev_dqp_nonzero`.**
  §9.3.3.1.1.5 — ctxIdxInc for the next MB's `mb_qp_delta` is 0 when the
  previous MB is skipped. The P/B CABAC loops threaded `dqp_nz` from the last
  *coded* MB across intervening skips, so MB15 (preceded by MB14 skip) decoded
  `mb_qp_delta` with ctxIdxInc=1 → wrong value → qp wrong + residual desync.
  FIX: `prev_dqp_nonzero = false;` in both skip branches (`cabac_p.rs`,
  `cabac_b.rs`). `mbaff_ip` P SAD **7269 → 493** (≈ skip-loop-filter artefact,
  matches CAVLC 551); MB(3,3) 250/256 max 118 → 77/256 max 4. 262 lib tests,
  `conformance_matrix`, `cabac_conformance`, `b_frame_conformance`,
  `high_profile_8x8_cabac_conformance`, `dbg_g6_mbaff_deblock` all green.
  REMAINING: `mbaff_ibp` P still SAD 43815 — separate bug; MB11 intra-8×8
  scan permutation still un-fixed (cabac_p.rs:190).

## SESSION #32ab (2026-08-29) — A4: `amvd_sum` mvd-context cell geometry verified correct

Verified the `amvd_sum` MVD CABAC context cell geometry is correct for the
all-frame-coded MBAFF case (`mbaff_ip`), mirroring A3's `ref_idx_gt0_neighbors`
verification. The ffmpeg `scan8[n]-1/-8` convention is properly translated:
left neighbor reads `by*4+(bx-1)` (same MB) or `by*4+3` (left MB's rightmost
column); top neighbor reads `(by-1)*4+bx` (same MB) or `3*4+bx` (top MB's
bottom row). The `l1_mvd_abs` array is selected when `list==1`.

**New unit tests in `ctx.rs::tests` (5 new, 12 total with A3's 7):**
- `amvd_top_neighbor_cross_mb_reads_bottom_row` — cross-MB top reads row 3
  (`3*4 + bx`) of the neighbor at the correct column.
- `amvd_within_mb_top_reads_current_inter_context` — within-MB top reads pull
  from `cur_inter` at `(by-1)*4 + bx`.
- `amvd_l1_list_uses_l1_mvd_abs` — L1 list selects `l1_mvd_abs`, not `l0_mvd_abs`.
- `amvd_off_picture_neighbor_returns_zero` — off-picture ⇒ 0.
- `amvd_sum_caps_at_70` — 70 + 70 = 140 (storage-time cap respected).

**Conclusion:** the `amvd_sum` geometry is unambiguously correct for
`mbaff_ip` (all pairs frame-coded ⇒ `mbaff::derive_neighbours` degenerates to
plain raster). Combined with A3's ref_idx verification, the MVD/ref CABAC
context-cell picks are proven correct — the `mbaff_ip` P desync root cause is
NOT in these cells (it's upstream at `mb_type` ctxIdx 15 per #32aa). A4 needs
no fix. VALIDATION: lib 261/261 (12 ctx tests), clippy `-D warnings` clean.

## SESSION #32aa (2026-08-29) — ★ CABAC MBAFF P desync is at MB9's `mb_type` (ctxIdx 15), BEFORE any CBP/mvd/ref context ★

**This contradicts #32y/#32z's "CBP context is the root cause".** The CBP fix
(#32y) is real but downstream — CABAC full-decode SAD on `mbaff_ip` P actually
went **30204 → 48461 (worse)** after #32y/#32z + the transform_8x8 landing, and
the first coded MB is still mis-typed.

**Oracle built:** `tests/dbg_mbaff_cabac_vs_cavlc.rs` (parses both entropy
variants directly + full-decode SAD probe) + ffmpeg ground truth via
`-debug mb_type` and `-flags2 +export_mvs` (PyAV). NOTE `cabac=1` vs `cabac=0`
give *different* x264 partitioning — not directly comparable; the value is the
CABAC grid vs ffmpeg's own decode of the CABAC stream.

**ffmpeg's `cabac=1` `mbaff_ip` P grid (decode order):**
`MB9=P_L0_L0_16x8` (grid (0,3), top-partition mv ≈ (+43,0) qpel, bottom (0,0)),
`MB11,MB12,MB15 = 16x16 mv (0,0)`, `MB13 = 16x8 top (+43,0)`, `MB14 = SKIP`.
**Crate decodes `MB9 = P_8x8`** with sub_mb_types `[2,1,1,0]` and small mvds —
a `mb_type` misparse at the *first coded MB*. Its 3 `mb_type` bins:
ctxIdx 14 → 0 (inter, matches ffmpeg), **ctxIdx 15 → 0** (crate: "16x16/P_8x8"
branch; ffmpeg needs **1** → "16x8/8x16" branch), ctxIdx 16 → 1 (⇒ P_8x8).

**Everything upstream of ctxIdx 15 is hand-verified correct** against
`h264_cabac_ref.c` (`KINETIX_BINTRACE=1` per-bin trace, `BIN n D ctx=…`):
the 8 skip bins (ctxIdx 11, all MPS), the pair-4 `mb_field_decoding_flag`
(ctxIdx 70 → 0), and `mb_type` bin-0 (ctxIdx 14) all match ffmpeg's derivation
AND the bit *count* matches (2 skip + 1 terminate per fully-skipped pair;
pair 4 = MB8-skip + MB9-skip + field-flag). The CABAC engine is proven
bin-for-bin vs ffmpeg (`dbg_engine_diff`), and progressive CABAC P (which
exercises ctxIdx 15/16/17) is bit-exact.

⇒ **The arithmetic engine (`low`/`range`) is desynced entering ctxIdx 15**
despite every hand-checkable bin matching. Remaining suspects, in order:
1. a wrong bin *value* somewhere in MB0–MB8's skip/field decode that only the
   real ffmpeg engine can catch (the hand-oracle shares the crate's engine —
   same blind spot as TRANS_IDX_LPS[28] / the amvd convention);
2. the CABAC init byte offset — `data_bit_offset=30` → `byte_align` → byte 4;
   verify against ffmpeg's `cabac_alignment_one_bit` consumption (the CAVLC
   twin's `data_bit_offset=29` is confirmed right — its parse is bit-exact);
3. an MBAFF-specific `decode_terminate` count bug (the #32p fix guards
   pair-top terminates — re-audit whether a *skipped* pair still gets exactly
   one, and whether the skip-run pre-read of the bottom MB interacts).

**MB9 = 16x8 is CONFIRMED** (not a `-debug mb_type` glyph misread):
`-flags2 +export_mvs` reports MB(0,3) as two `w=16,h=8` partitions, top mv
≈(+43,0) qpel, bottom (0,0). P_8x8 would report `8x8`/`8x4`/`4x8`/`4x4`.

**FfEngine-lockstep attempt (`tests/dbg_mbaff_p_ffengine_oracle.rs`, deleted):**
tried to drive `dbg_engine_diff.rs`'s `FfEngine` (ffmpeg engine port, tables
parsed from `cabac_ref.c`) + the crate `CabacDecoder` through the MB0→MB9
sequence, shared context model. **Blocked:** ffmpeg's `ff_h264_lps_range`
lookup `table[2*(range&0xC0) + s]` returns a *negative* padding value (`-51`)
for `(range=0x1FE, s=7)` — i.e. qRangeIdx 3 + low pStateIdx. `dbg_engine_diff`
*guards around* exactly these (`if lps_range <= 0 { continue }`) and so has
**never validated the engines against each other for a first-bin decode at
range 0x1FE with a low-pStateIdx context** — which is precisely MB0's skip bin
here. The crate→ffmpeg packed-state mapping (`(pi<<1)|mps`) or the table slice
needs re-deriving for this regime; the C-table row spacing suggests
`table[qbucket + 2*pi + mps]` maps to spec `RANGE_TAB_LPS` at a *different*
pStateIdx than assumed (table[6,7]=123 ↔ spec `RANGE_TAB_LPS[6][0]=123`, not
`[3][0]=143`).

NEXT (two options):
1. Fix the ff-table indexing / packed-state mapping in a fresh `FfEngine`
   replay so the MB0→MB9 lockstep runs, OR
2. compile the real `cabac_ref.c` (self-contained: engine + tables, stub
   `libavutil/error.h`+`mem_internal.h`) — `clang` 22.x on PATH — init at
   `mbaff_ip`'s P-CABAC offset (payload starts `04 E7 5F AC 3E C9 …`,
   `data_bit_offset=30` → byte 4, slice_qp for context init from the header),
   replay ffmpeg's `decode_cabac_mb_skip`×10 + `get_cabac_terminate`×4 +
   `decode_cabac_field_decoding_flag` + `decode_cabac_mb_type` P, and diff
   each bin against the crate parser's `KINETIX_BINTRACE` (`BIN 20443…20460`).

## SESSION #32z (2026-08-29) — A3: `ref_idx_gt0_neighbors` cell geometry verified correct

Verified the `ref_idx_gt0_neighbors` and `amvd_sum` cell geometry is correct
for the all-frame-coded MBAFF case (`mbaff_ip`). The ffmpeg `scan8[n]-1/-8`
convention is properly translated: left neighbor reads `by*4+(bx-1)` (same MB)
or `by*4+3` (left MB's rightmost column); top neighbor reads `(by-1)*4+bx` (same
MB) or `3*4+bx` (top MB's bottom row). For `mbaff_ip` (all pairs frame-coded),
`mbaff::derive_neighbours` degenerates to plain raster, so the geometry is
unambiguously correct.

**New unit tests in `ctx.rs::tests` (7 tests, all pass):**
- `ref_idx_left_neighbor_cross_mb_reads_rightmost_column` — cross-MB left reads
  column 3 of the neighbor at the correct row.
- `ref_idx_top_neighbor_cross_mb_reads_bottom_row` — cross-MB top reads row 3
  of the neighbor at the correct column.
- `ref_idx_within_mb_reads_current_inter_context` — within-MB reads pull from
  `cur_inter` at the correct raster block.
- `amvd_left_neighbor_cross_mb_reads_rightmost_column` /
  `amvd_within_mb_reads_current_inter_context` — same geometry for MVD.
- `ref_idx_off_picture_neighbor_returns_false` — off-picture ⇒ false.
- `ref_idx_l1_list_uses_l1_ref_gt0` — L1 list selects `l1_ref_gt0`.

**Conclusion:** the `mbaff_ip` desync root cause is the **CBP context** (wrong
cbp-context from MBAFF neighbour cbp derivation, fixed in #32y), NOT ref_idx.
Confirmed by MB12's MVDs `(0,0)`/`(-1,0)` matching ffmpeg in the #32v trace.
A3 needs no fix — geometry is correct. VALIDATION: lib 256/256 (7 new), clippy
`-D warnings` clean.

## SESSION #32y (2026-08-29) — A2: inter `coded_block_pattern` CABAC context fixed for MBAFF frame pairs

Fixed the inter/intra `coded_block_pattern` CABAC neighbour context under MBAFF
frame pairs. Root cause: `NeighbourCtx::left_top()` discarded `left_bottom` from
`mbaff::derive_neighbours`, and both `decode_inter_cbp_cabac` (inter) and
`cabac_cbp_neighbors` (intra) copied `cbp_word` **wholesale** from `left_top`.
For a frame-coded current MB next to a field-coded left pair, FFmpeg's
`decode_cabac_mb_cbp_luma` reads `left_cbp` bits 1 (top-right 8×8) and 3
(bottom-right 8×8) — which in a mixed pair come from the pair-top and
pair-bottom MBs respectively. The wholesale copy always used the pair-top
neighbour, so the bottom-half luma context bit was wrong.

**New in `slice_data/ctx.rs`:**
- `NeighbourCtx::left_top_with_bottom()` — like `left_top()` but also returns
  `left_bottom` (the pair-bottom neighbour address) for MBAFF frame pairs.
- `cabac_cbp_neighbors_inter()` — MBAFF-aware CBP lookup for inter MBs:
  rebuilds `left_cbp` as `(left_top.cbp & 0x02) | (left_bottom.cbp & 0x08)`
  for luma, chroma from `left_top`. Non-MBAFF and all-frame-coded pairs
  degenerate to the wholesale copy.
- `cabac_cbp_neighbors()` (intra path) — now also MBAFF-aware via the same
  rebuild logic, using `CABAC_CBP_UNAVAILABLE` (0x7CF) as the off-picture
  sentinel.

**Fixed in `slice_data/cabac_b.rs`:**
- `decode_inter_cbp_cabac` — now calls `cabac_cbp_neighbors_inter` with the
  inter sentinel `0x00F`.
- `parse_intra_mb_cabac_pb` — the CABAC intra-in-P/B path now passes the
  real `nctx` to `cabac_cbp_neighbors` instead of `NeighbourCtx::NONE`, so
  intra MBs in an MBAFF frame also get the rebuilt left_cbp.

**VALIDATION:** `cargo build` clean, `cargo clippy --all-targets -- -D warnings`
clean, `cargo fmt --check` clean, lib 249/249, full integration suite green
(0 failures). The `mbaff_ip` P-frame SAD improvement is expected but not yet
measured — that requires re-running `dbg_g5_interlaced` with the fix.

## SESSION #32x (2026-08-29) — PAFF B-field decode path implemented, corpus validation SURFACES BUGS

Implemented the PAFF **B-field** decode path (Track G.2). Previously
`decode_interlaced` returned `InterlacedOutcome::Fallback` for every B-field
picture; now it decodes both fields, motion-compensates each field macroblock
with bi-prediction into a half-height buffer, deblocks, and interleaves the
pair for output — mirroring the existing PAFF P-field path.

**New in `reconstruct.rs`:**
- `reconstruct_inter_b_field_frame` — field-coordinate bi-predictive
  reconstruction: pre-extracts the contiguous half-height L0/L1 field planes
  via `FieldRef::planes()`, then per 4×4 block dispatches L0-only / L1-only /
  bi-prediction from the committed `MvCell` (`ref_idx` / `ref_idx_l1`).
- `reconstruct_field_b_inter_luma` / `_chroma` — field-coordinate bi-predictive
  MC helpers (mirror `reconstruct_field_inter_luma`/`_chroma`, dual-list).

**New in `decoder/interlaced.rs`:**
- `decode_interlaced_b_field` — builds both field reference lists
  (`build_field_ref_list_l0` / `_l1`, §8.2.4.2.5), derives the current field's
  POC (scratch `poc_state`), parses the field B-slice (`parse_b_slice_cabac` /
  `parse_b_slice`), reconstructs via `reconstruct_inter_b_field_frame`, deblocks
  (`deblock_field`), and interleaves (`finalize_field`).
- Weighted bi-prediction: explicit (`weighted_bipred_idc == 1`, both l0/l1
  weight tables), implicit (`== 2`, POC-distance weights), default otherwise.
- Dispatch: `decode_interlaced` now routes `SliceType::B` to the new path
  before the I/SI intra path.

**VALIDATION (2026-08-29):** `cargo build` clean, `cargo clippy --all-targets
-- -D warnings` clean, lib 249/249 green.

**CORPUS VALIDATION — BUGS FOUND:** Attempted validation against ffmpeg's
reference decode using a PAFF stream generated by the JM reference encoder
(`PicInterlace=1`, 80×64, IP sequence). Two issues had to be resolved to get a
compliant test vector:

1. **JM produces non-compliant Annex B** — zero emulation prevention bytes,
   causing false `00000001` start codes within slice payloads that split NALs.
   Patched `WriteAnnexbNALU` (`lencod/src/annexb.c`) to insert EPBs via an
   `insert_epb` helper. After the patch, the stream parses to the correct 6
   NALs (SPS/PPS/IDR + 3 field slices). Fixture committed at
   `tests/fixtures/paff_b_field.264` (CABAC variant).

2. **PAFF field decode produces catastrophic output** — the Rust decoder emits
   1 full frame (80×64) + 1 unpaired half-height field (80×32) instead of 2
   full frames, and the full frame has **max_diff=126** (7437/7680 samples
   differ) vs ffmpeg. The bug is **NOT entropy-coding-specific**: both CAVLC
   and CABAC streams fail identically with max_diff=126, pointing at the field
   reconstruction / reference-list / field-pairing path rather than the
   entropy decoder.

**Diagnosis:**
- PPS correctly parsed as `entropy_coding_mode_flag=false` (CAVLC). The PAFF
  path returns `Fallback` for most fields, causing the main loop to fall
  through to the progressive `try_decode_real_slice` path, which then fails
  because it expects progressive (non-field) input.
- The decoder emits a half-height frame on `flush`, confirming the
  `field_accum` pairing logic is not completing for all fields.

**Root cause (suspected):** The field reference list construction
(`build_field_ref_list_l0`) or the DPB storage of the IDR reference field is
broken — the P-field can't find its reference, returns `Fallback`, and the
progressive fallback path misparses the field-coded slice. Unit test
`dbg_paff_b_field.rs` captures the current (failing) state.

**NEXT:** Debug why `build_field_ref_list_l0` returns `None` (empty DPB) or
why the PAFF P-field reconstruction fails. Isolate with an intra-only PAFF
stream (no references needed) to separate field-reconstruction bugs from
reference-list bugs.

## SESSION #32w (2026-08-29) — B3: MC + reconstruction wiring for MBAFF P/B

Wired the inter reconstruction path for MBAFF frame P/B slices (Track B3).
`decode_interlaced_mbaff` now returns `Frame` (not `Fallback`) for P/B slices
when `KINETIX_CABAC_FIELD_MC=1`.

**New in `reconstruct.rs`:**
- `reconstruct_b_frame_mbaff` — MBAFF-aware twin of `reconstruct_inter_frame_ex`
  for B slices: dispatches each macroblock between the frame-coded path
  (`reconstruct_b_inter_luma`/`_chroma`) and the field-coded path based on
  `mb_field_flag`, behind the `KINETIX_MBAFF_FIELD_MC` gate.
- `reconstruct_mbaff_b_inter_luma` / `_chroma` — field-coordinate bi-predictive
  MC for field-coded B macroblocks (L0 + L1 against the half-height parity
  planes, stride-2 write-back).

**Wired in `decoder/interlaced.rs::decode_interlaced_mbaff`:**
- P slices: build L0 (`build_ref_list_l0`), parse, reconstruct via
  `reconstruct_inter_frame_ex`, deblock, store reference, return `Frame`.
- B slices: build L0 + L1 (`build_ref_list_l0_b_slice` / `build_ref_list_l1`),
  parse with colocated MV grid for direct mode, reconstruct via
  `reconstruct_b_frame_mbaff`, deblock, store reference, return `Frame`.
- Weighted prediction: explicit (P via `weighted_pred_flag`; B via
  `weighted_bipred_idc == 1`) and implicit (`weighted_bipred_idc == 2`).
- Whole path gated behind `KINETIX_MBAFF_FIELD_MC=1`; gate off ⇒ byte-identical
  `Fallback` to prior behaviour.

For the all-frame-coded case (`mbaff_ip`/`mbaff_ibp`) every macroblock has
`mb_field_flag == false`, so reconstruction collapses to progressive inter into
contiguous halves — the tractable first target. The #32f items 6-8 gaps in the
field-coded path are fixed only as far as the frame-coded path needs; the
field-coded B path reuses the parity-plane convention from the existing
`reconstruct_mbaff_inter_luma`/`_chroma`.

**VALIDATION:** `cargo build` clean, `cargo clippy --all-targets -- -D warnings`
clean, `cargo fmt --check` clean, lib 249/249, full integration suite green
(0 failures).

## SESSION #32v (2026-08-29) — CABAC MBAFF P desync localized to pair-6 TOP MB; inter `transform_size_8x8_flag` confirmed missing (latent)

Narrowed Track A with `ffmpeg -debug mb_type` + `KINETIX_BINTRACE` on `mbaff_ip`.

**The `mbaff_ip` P-frame mb_type grid (frame-MB raster):**
```
ffmpeg:            crate:
S S S S            S S S S
S S S S            S S S S
S S >  S           S S C  C     <- (3,2): ffmpeg SKIP, crate CODED
>- > >- >          C C S  C     <- (2,3): ffmpeg CODED, crate SKIP
```
**CORRECTION (later same session): the `ffmpeg -debug mb_type` legend reading
above is unreliable — `>-` / `> ` / `S` disambiguation is guesswork. The
decisive evidence is the per-MB diff map + the skip-MB behaviour, and it points
to a VALUE bug in the CABAC motion syntax, NOT a bin-count desync and NOT
reconstruction:**

`dbg_g5_i1_diffmap` per-MB luma diff (mbaff_ip P frame), in **decode order**:
```
MB8 (0,2) SKIP     13/256 max 2   FINE
MB9 (0,3) P8x8    227/256 max 144 CATASTROPHIC   <- first coded MB
MB10(1,2) SKIP     13/256 max 2   FINE           <- skip right after catastrophic
MB11(1,3) P8x16   243/256 max 114 CATASTROPHIC
MB12(2,2) P16x8    80/256 max 42  moderate
MB13(2,3) ...     205/256 max 126 CATASTROPHIC
MB14(3,2) ...     111/256 max 93
MB15(3,3) P8x8    235/256 max 122 CATASTROPHIC
```
- **Every coded inter MB is catastrophic; every skip MB (incl. MB10, right
  after catastrophic MB9) stays bit-close.** A bin-COUNT desync would make all
  MBs after the desync point garbage — skip flags included. They're not ⇒ the
  parse stays in bit-sync; only the decoded mvd / sub_mb_type / ref_idx VALUES
  are wrong for the inter-motion syntax.
- Diffs are **even/odd row symmetric** (MB9 117/110, MB12 40/40) ⇒ NOT a
  field/frame interleave mismatch, a uniform wrong-MV error across the MB.
- **`mbaff_ip` P SAD is byte-identical with `KINETIX_MBAFF_FIELD_MC=1` +
  `KINETIX_CABAC_FIELD_MC=1` (Track B's #32w path) ON vs OFF** ⇒ reconstruction
  path is not the variable; the MVs fed into it are already wrong.
- **`mbaff_cavlc_ip` P is BIT-EXACT** (#32t) and shares `mv.rs
  predict_slice_mvs_ex` + the whole reconstruction path with the CABAC route ⇒
  MV prediction + MC + inter reconstruction are PROVEN correct for
  all-frame-coded MBAFF P.

→ **The bug is wrong CABAC context selection in
`parse_p_macroblock_cabac`'s inter-motion path under MBAFF** (right bin count,
wrong value): `amvd_sum` / `cabac_decode_mvd_component` neighbour-cell geometry
(`ctx.rs`), `ref_idx_gt0_neighbors`, and/or `sub_mb_type` (`sub_mb_p`) — i.e.
Track A / #32q's original NEXT, for P_8x8 **and** P16x8/P8x16. My "it's the CBP
context" hypothesis above is NOT confirmed and looks wrong (cbp/skip flags stay
in sync per the diff map).

**Oracle harness built: `tests/dbg_mbaff_cabac_vs_cavlc.rs`** — encodes the
`mbaff_ip` source twice (`cabac=1` / `cabac=0`, else-identical x264 params),
parses both P slices directly via `parse_p_slice[_cabac](… mb_aff=true …)`, and
prints the per-MB `mb_type` / `cbp` / raw `mvd_l0` grid.

Findings:
- **The CABAC direct-parse reproduces the full decoder's parse exactly**
  (MB(0,3) P8x8[2,1,1,0] mvd `(-2,3),(2,1),(2,0),(0,0),(0,0),(-1,0),(1,0)` ==
  the `dbg_g5_interlaced` H264Decoder BINTRACE). So the harness is faithful and
  the CABAC parse output is deterministic + `transform_8x8_mode`-independent
  (no inter MB in this clip reads a t8 bin: every coded one is either
  `cbp&15==0` or a split P_8x8 that `get_dct8x8_allowed` excludes — so the
  missing-inter-t8-flag latent bug is a genuine no-op here, ruled out).
- **`num_ref_idx_l0_active` MUST come from the slice header, not the PPS
  default.** This clip: header override → 1, PPS default → 3. Feeding the PPS
  default (3) desyncs BOTH parsers immediately (spurious `ref_idx` reads):
  CAVLC hard-fails `mb_skip_run out of range`, CABAC emits absurd mvds
  (`-72`, `16`). `diag_cabac_p_localize.rs` / `diag_cabac_vs_cavlc.rs` use the
  PPS default — a latent harness bug there, only masked when the two happen to
  agree.
- **CAVLC direct-parse of this `deblock=0` clip ALSO produces garbage** (mvd
  `(43,0)`, `(-31,-35)`; skip pattern disagrees with the CABAC grid). NOTE the
  proven-bit-exact `g6_cavlc_ip` (#32t) uses **no `deblock=0`** and goes
  through the full `H264Decoder`, not `parse_p_slice` directly — so it is a
  *different stream* + path. Whether the CAVLC garbage here is a real
  CAVLC-MBAFF bug on `deblock=0` streams or a missing bit of slice context the
  direct call doesn't get (ref-list reordering etc.) is unresolved — the CAVLC
  oracle isn't trustworthy yet.

**Net:** the CABAC MBAFF P parse produces plausible small mvds that still drive
catastrophic reconstruction ⇒ the wrong value is subtle (a mis-selected mvd
context flipping a low-order bin) or it's MV-prediction (`mv.rs
predict_mv_sub` under MBAFF pair-scan). Next: get a trusted per-MB MV reference
(ffmpeg `-flags2 +export_mvs` side-data, or fix the CAVLC direct-parse harness)
and diff against the CABAC grid MB-by-MB.

**Latent bug found & confirmed (not the `mbaff_ip` root cause):** the CABAC P/B
inter path (`slice_data/cabac_b.rs`) **never reads `transform_size_8x8_flag`**
for coded inter MBs — the CABAC twin of the CAVLC bug fixed in #32j. ffmpeg
`ff_h264_decode_mb_cabac` reads it (ctxIdx `399 + neighbor_transform_size`)
when `dct8x8_allowed && (cbp & 15) && !IS_INTRA(mb_type)` (line ~2347), and
`trace_headers` confirms `transform_8x8_mode_flag = 1` in this High-profile
x264 PPS. It is genuinely latent for `mbaff_ip` (no coded inter MB there has
`cbp_l != 0` except a split P_8x8, which `get_dct8x8_allowed` excludes) but WILL
bite any High-profile CABAC P/B stream whose first coded inter MB has non-zero
luma CBP. A prototype fix (read the bin after CBP, gated by ffmpeg's
`get_dct8x8_allowed`, + an 8×8 residual branch in `decode_inter_residual_cabac`)
was written and **reverted**: no-op on `mbaff_ip`, regressed `mbaff_ibp`
(45764→69338 P) — the regression is inside already-broken output but signals
either a wrong `neighbor_transform_size` context or that the B-slice
`get_dct8x8_allowed` sub-type check needs `direct_8x8_inference_flag` threaded.
Re-land it with an independent oracle once Track A's cbp-context bug is fixed
(they share the CBP read path). Progressive CABAC P/B / high8x8 / b_frame
conformance all stay bit-exact with or without it.

## SESSION #32u (2026-08-29) — CABAC MBAFF P/B broken into trackable sub-tasks

The remaining `pixel_exact` blocker (CABAC/CAVLC MBAFF P/B — #32t) split into two
independent tracks. Track A (parse) and Track B (slice setup / reconstruction)
are independent up to B5: Track B can be built and unit-tested against the
already-bit-exact CAVLC MBAFF P parse output while Track A is still being
debugged.

### Track A — CABAC P_8x8 sub-partition parse under MBAFF frame-coded pairs

Desync is narrow: skip/coded grid matches ffmpeg through pair 5, diverges at
pair 6's first `P_8x8` MB. `terminate` never desyncs → a value error in one of a
few context-cell picks. Steps A1→A5 are strictly sequential.

- [ ] **A1. Oracle capture harness.** Extend the compiled-ffmpeg CABAC oracle
      (`clang` + vendored `h264_cabac_ref.c` at repo root) to dump engine state
      (`range`/`offset`) + context array + payload offset immediately before
      pair 6's `P_8x8` MB in `mbaff_ip`. Tooling only, no decoder change.
      Deliverable: a checked-in trace file.
- [ ] **A2. `sub_mb_type` decode audit.** Replay A1's state through
      `slice_data/cabac_b.rs::parse_p_macroblock_cabac`'s `sub_mb_type` path;
      diff each bin's `ctxIdx` + value vs the oracle. Fix the four sub-block
      `sub_mb_type` reads. Verify: bin-for-bin match up to the first `ref_idx`.
- [x] **A3. `ref_idx_gt0_neighbors` cell geometry.** COMMITTED fe15891: 7 unit
      tests in `ctx.rs::tests` (cross-MB left/top reads, within-MB reads,
      off-picture, L1 list). Geometry verified correct.
- [x] **A4. `amvd_sum` (mvd context) cell geometry.** 5 new unit tests in
      `ctx.rs::tests` (top-neighbor cross-MB reads bottom row, within-MB top
      reads, L1 list selects `l1_mvd_abs`, off-picture ⇒ 0, 70+70 cap). Geometry
      verified correct — mirrors the 7 ref_idx tests from A3.
- [ ] **A5. Regression lock.** Once `mbaff_ip` P SAD → 0 against a
      fully-filtered (`dbg_g6_mbaff_deblock`-class) reference: add a CABAC-P (and
      B) MBAFF clip to that harness — there is none today, current numbers come
      from the `-skip_loop_filter` harness. Then repeat A2/A4 for `mbaff_ibp`
      B slices (B-variant `sub_mb_type` + L1/bi `mvd`).

### Track B — interlaced-module inter decode path

`decode_interlaced_mbaff` (`decoder/interlaced.rs`) hard-returned `Fallback`
for every non-I slice. `mbaff_ip`/`mbaff_ibp` pairs are all frame-coded, so
reconstruction collapses to progressive inter into contiguous halves — the
setup plumbing is the real work. B1/B2 can start now in parallel with Track A.

- [x] **B1. Ref-list construction for MBAFF frame pairs.** MBAFF frames are
      `field_pic_flag=0` → ordinary *frame* ref lists (§8.2.4). Reuse
      `decoder/mod.rs`'s frame P/B list builders, NOT the PAFF
      `build_field_ref_list_l0` path. Deliverable: `decode_interlaced_mbaff`
      builds L0 (+ L1 for B) and logs them; still returns `Fallback` after.
- [x] **B2. Parse dispatch for P/B.** Replace the P/B early-return with
      `parse_p_slice_cabac` / `parse_b_slice_cabac` / CAVLC equivalents
      (pair-scan addressing already fixed #32q). Assert the parse runs to
      completion (terminate at last MB) on `mbaff_ip`/`mbaff_ibp`; no
      reconstruction yet.
- [x] **B3. MC + reconstruction wiring.** Call `reconstruct_mbaff_inter_luma`/
      `_chroma` (exist, opt-in with gaps #32f 6-8) for the all-frame-coded case,
      feeding B1's ref lists + DPB. Fix the #32f 6-8 gaps only as far as the
      frame-coded path needs. Keep behind `KINETIX_CABAC_FIELD_MC` initially.
      Implemented: `reconstruct_b_frame_mbaff` (new, MBAFF-aware twin of
      `reconstruct_inter_frame_ex`) + field-coded B inter helpers
      `reconstruct_mbaff_b_inter_luma`/`_chroma`; `decode_interlaced_mbaff` now
      builds ref lists, parses, reconstructs (P via `reconstruct_inter_frame_ex`,
      B via `reconstruct_b_frame_mbaff`), runs the MBAFF deblock orchestrator,
      stores the reference picture, and returns `Frame` — all gated behind
      `KINETIX_CABAC_FIELD_MC=1`.
- [x] **B4. Inter deblock.** Extend `mbaff_deblock_infos` / `run_mbaff_deblock`
      `bS` derivation for inter MBs (MV/ref-difference cases); I-path already
      exists. IMPLEMENTED: `mbaff_deblock_infos` reads MV cells from
      `mv_store.cells_of(idx)`; `filter_mbaff_mb` derives bS for inter MBs via
      `derive_bs_segments`/`derive_bs_pair` (MV ≥ mvy_limit / ref_idx difference
      → bS=1, nz → bS=2); MBAFF-specific edges (`first_vertical_edge_bs`,
      `fieldcoded_above_boundary_bs`) handle the non-intra path with nz-based
      bS; interlaced P/B path wires it in. Frame-coded MBAFF P/B is the
      supported target (field-coded pairs reuse the parity-plane convention).
      VALIDATED: 249/249 lib, full suite green, `dbg_g6_mbaff_deblock`
      bit-exact vs ffmpeg.
- [x] **B5. Flip on + integrate.** Remove the gate; `decode_interlaced_mbaff`
      returns `Frame` for P/B. Wire DPB store for B (non-ref handling).
      IMPLEMENTED: removed the `KINETIX_MBAFF_FIELD_MC` gate at the top of the
      P/B branch in `interlaced.rs` — the inter decode path is now the default
      for MBAFF P/B slices. DPB store for B was already correct:
      `store_reference_picture` returns early when `nal_ref_idc == 0`, so
      non-reference B slices never enter the DPB. The field-MC gate in
      `reconstruct.rs` stays (field-coded pairs are not yet validated; the
      supported target is frame-coded MBAFF P/B).
      VALIDATED: lib 256/256, full suite green, `dbg_g6_mbaff_deblock`
      bit-exact vs ffmpeg.

### G.5 BASELINE (2026-08-29) — `dbg_g5_interlaced` vs ffmpeg (-skip_loop_filter)

| variant | frame | luma SAD vs ffmpeg | status |
|---------|-------|--------------------|--------|
| mbaff_i1 | I | 499 | known deblock-vs-skipLF artefact |
| mbaff_ip | I | 381 | known deblock artefact |
| mbaff_ip | **P** | **48660** | **BROKEN — CABAC MBAFF P** |
| mbaff_ibp | I | 508 | known deblock artefact |
| mbaff_ibp | **P** | **98725** | **BROKEN** |
| mbaff_ibp | **B** | **189898** | **BROKEN** |
| mbaff_cavlc_ip | I | 482 | known deblock artefact |
| mbaff_cavlc_ip | P | 551 | known deblock artefact (near-exact) |
| mbaff_cavlc_ip2 | I | 703 | skipLF harness numbers |
| mbaff_cavlc_ip2 | P | 1154 | skipLF harness numbers |

**CAVLC MBAFF P is bit-exact** (reconstruction/MV-prediction/deblock all proven
correct via `dbg_g6_mbaff_deblock` `g6_cavlc_ip` sad=0). **CABAC MBAFF P/B is
the sole open inter gate.** The bug is in the CABAC entropy decode path
(`slice_data/cabac_p.rs`/`cabac_b.rs`): the full-decoder CABAC P-frame SAD is
30204-48660 while CAVLC P is 0 on identical params.

**First decode-order divergence** (crate CABAC direct-parse vs ffmpeg
`-debug mb_type`, same cabac bitstream): ffmpeg's P-frame raster grid is
`S S S S / S S S S / S S > S / >- > >- >` while the crate reads
`(2,2)P16x8 (3,2)P8x16 / (0,3)P8x8 (1,3)P8x16 (2,3)PSkip (3,3)P8x8`. First
mismatch at decode index 13 = g14 = MB(2,3): crate reads PSkip, ffmpeg reads
coded ⇒ engine state desync'd during an earlier coded MB (g12=P8x8, g13=P8x16,
or g10=P16x8). Likely a wrong CABAC ctxIdx in the MBAFF coded-MB path
(sub_mb_type / amvd_sum / cbp context), consuming the wrong number of bins.
→ **Build a compiled-ffmpeg per-bin oracle (vendored `h264_cabac_ref.c` + clang
22 at repo root) to pinpoint the exact divergent bin.** This is the highest-
leverage next action. G.5 stays gated until CABAC MBAFF P/B is bit-exact.

### Then (unchanged, gated on A + B)

- [ ] **G.5** — PAFF + MBAFF corpus bit-exact validation.
- [ ] **non-16 crop** — final check (mostly closed in #32s).
- [ ] **`pixel_exact` flip** — gated on G.5.

## SESSION #32t (2026-08-29) — Remaining-gate audit: CAVLC MBAFF P confirmed BIT-EXACT; CABAC MBAFF P/B is the sole open inter gate

Baseline re-verification pass over the `pixel_exact` gates from #32p/#32s.

**CAVLC MBAFF P — DONE / BIT-EXACT.** `dbg_g6_mbaff_deblock` (fully-filtered
ffmpeg reference, the real oracle — *not* `-skip_loop_filter`): `g6_cavlc_ip`
**frame#1 (P) gate-ON luma sad=0 max=0, cb=0, cr=0**. The `mbaff_cavlc_ip`
P-frame sad=551 / `mbaff_cavlc_ip2` sad=1154 seen in `dbg_g5_interlaced` are
purely the skip-loop-filter harness artefact (same class as the mbaff_i1
sad=499 I-frame noted in #32p), confirmed because `g5`'s own I-frames show
identical-magnitude residue (ff0 sad 381–703) against that mismatched
reference. #32q's decode-order + pair-scan-addressing fixes closed CAVLC MBAFF
P for real.

**Still open — CABAC MBAFF P/B only.** `dbg_g5_interlaced`: `mbaff_ip` P
sad≈30k, `mbaff_ibp` P 45764 / B 75300. `dbg_g6_mbaff_deblock` has no CABAC-P
or any-B clip, so those numbers are the best signal and they are genuine
decoder divergence (not a harness artefact). Root cause per #32q's
`ffmpeg -debug mb_type` cross-check: skip/coded grid matches ffmpeg through
pair 5, diverges at pairs 6–7 where the first coded MB is `P_8x8` — the
sub-partition CABAC parse (`sub_mb_type` / `mvd` amvd-sum contexts under an
MBAFF frame-coded pair) produces wrong values without desyncing `terminate`.

NEXT (unchanged from #32q, now the *only* remaining inter gate): audit
`slice_data/cabac_b.rs::parse_p_macroblock_cabac` P_8x8 path —
`p8x8_sub_dims` / `partition_dims` geometry, and the `amvd_sum` /
`ref_idx_gt0_neighbors` cell picks (`ctx.rs:230-338`, the ffmpeg
`mvd_cache[scan8[n]-1/-8]` convention) when `left_idx`/`top_idx` come from
`mbaff::derive_neighbours` rather than plain raster. The decisive tool is the
compiled-ffmpeg CABAC oracle (`clang` 22.x + vendored `h264_cabac_ref.c` at
repo root) the #32o/#32f notes already scoped: record engine state + payload
before pair 6's P_8x8 MB, replay, diff per-bin ctxIdx.

Then: PAFF P/B (B-field unimplemented), G.5 corpus, `pixel_exact` flip.

## SESSION #32s (2026-08-28) — Non-16-aligned crop-edge gap fixed; reconstruct + deblock at coded dimensions

**BUG**: `decode_slice` / `try_decode_real_slice` / `decode_interlaced` derived
`mb_cols = width.div_ceil(16)` / `mb_rows = height.div_ceil(16)` from the
*cropped* display dimensions, and `reconstruct_*_frame` allocated buffers at
cropped size with `stride = cropped_width`. Two consequences:

1. Edge macroblock samples past the visible region were silently dropped
   (`if px < stride` in deblock), so deblocking and inter-prediction into the
   padding near non-16-aligned right/bottom edges were wrong.
2. **Latent undercount**: `width.div_ceil(16)` undercounts `mb_cols` by 1 when a
   single-axis crop exceeds 8 px (e.g. `crop_right = 10` on a 12-MB-wide picture
   ⇒ display 172 ⇒ `172.div_ceil(16) = 11 ≠ 12`). x264's typical ≤8 px crops
   happened to work.

**FIX**:
- Added `SeqParameterSet::coded_width_pixels()` / `coded_height_pixels()`
  (`sps.rs`) — the MB-aligned dimensions per §7.4.2.1.1.
- `decode_impl` now computes `mb_cols = coded_width / 16` (exact, no
  `div_ceil`) for the MAX_MB_COUNT cap.
- `decode_slice`, `try_decode_real_slice`, and the MBAFF/PAFF paths in
  `interlaced.rs` now reconstruct and deblock at coded dimensions, then crop
  to the visible rectangle when building the output `VideoFrame`.
- Added `ReconstructedFrame::crop_yuv420p()` and a standalone
  `reconstruct::crop_yuv420p()` helper that tightly packs rows from coded stride
  to visible width.
- The skip-scaffold fallback (`emit_skip_frame` / `reconstruct_mb_rows`) is
  left at cropped dimensions — it produces flat-grey frames where exactness is
  not required.

**VALIDATION**: lib 249/249 (3 new unit tests for coded dims + crop), full
integration suite green (incl. `conformance_matrix`, `dbg_g6_mbaff_deblock`,
all `*_conformance` bit-exact tests), workspace `clippy -D warnings` clean,
`fmt` clean.

Remaining `pixel_exact` gates: CABAC/CAVLC MBAFF P/B (#32q item: P_8x8 path
pairs 6–7 still diverge), PAFF + MBAFF corpus bit-exact validation (G.5), then
the `pixel_exact` flip.

## SESSION #32r (2026-08-28) — cabac_b.rs debug lines gated behind KINETIX_BINTRACE

All 11 unconditional `eprintln!` debug lines in `slice_data/cabac_b.rs` (the P/B
CABAC inter parse path) are now gated behind
`if std::env::var("KINETIX_BINTRACE").is_ok() { ... }`, matching the convention
used in `cabac_i.rs`, `cabac_p.rs`, `cavlc.rs`, `mv.rs`, `deblock.rs`, and
`ctx.rs`. This was flagged in #32q as a prerequisite before MBAFF P/B is a
supported path. `cargo clippy -p tpt-kinetix-h264 --all-targets -- -D warnings`
clean; `cargo test -p tpt-kinetix-h264 --lib` 246/246 green.

## SESSION #32q (2026-08-28) — CABAC MBAFF P/B slice: pair-scan addressing bug fixed

**BUG** (`slice_data/cabac_p.rs` + `cabac_b.rs`): `parse_p_slice_cabac` /
`parse_b_slice_cabac` iterated macroblocks in **plain raster** order
(`mb_x = mb_idx % cols`, `mb_y = mb_idx / cols`) and committed every per-MB
array (`macroblocks` via `push`, `nz`/`pred_ctx`/`cabac_ctx`/`inter_ctx`/
`field_flags` by `[mb_idx]`) at the **decode-order** index — while the
neighbour lookups used frame-MB-grid positions. In an MBAFF frame the parse
visits pairs as (top, bottom) before advancing, so `mb_idx ≠ grid address` for
every bottom macroblock: neighbour context / nnz / cbp / mvd-cell / field-flag
reads all pulled from the wrong slots, and `mb_field_decoding_flag` /
`mb_skip_flag` were decoded for the wrong macroblocks. This is the P/B twin of
the CABAC I-slice `grid_idx` bug fixed in #32e / the CAVLC one in #32f — the P
and B CABAC paths never got it.

**FIX**: both parsers now derive `(mb_x, mb_y, grid_idx)` pair-aware when
`mbaff_frame` (identical formula to `cabac_i.rs`), pre-allocate `macroblocks`
and assign every array by `grid_idx`, and take `left_idx`/`top_idx` from the
frame grid. Progressive (`mb_aff=false`) is byte-identical (degenerate branch).

**FIX 2** (`mv.rs`): `predict_slice_mvs` processed macroblocks in **grid raster**
order, so `MvStore::is_available` reported a not-yet-decoded above-right
macroblock as an available MV predictor (spec §6.4.9 / §8.4.1.3.2 — C is
unavailable until decoded, and in an MBAFF frame the next pair's top MB is
decoded *after* the current pair's bottom). New `predict_slice_mvs_ex(…,
mbaff_frame)` iterates pair-scan **decode** order (committing by grid address);
`cabac_p.rs` / `cavlc.rs` pass `mbaff_frame`. Progressive unchanged.

**RESULT** (`dbg_g5_interlaced`, ffmpeg `-skip_loop_filter` reference):
- `mbaff_ip` P frame: SAD **83157 → ~30000**
- `mbaff_ibp` P frame **107208 → 45764**, B frame **127746 → 75300**
- `mbaff_cavlc_ip2` P frame **5748 → 1154** (decode-order fix)
- `mbaff_cavlc_ip` P frame stays **551** (near-exact, unaffected)
- `mbaff_i1` (CABAC MBAFF I) unchanged at 499 (deblock-vs-skipLF artefact, #32p)
- progressive CABAC P/B conformance, B-frame, CAVLC, lib (246): all green;
  clippy `-D warnings` clean.

`ffmpeg -debug mb_type` cross-check on `mbaff_ip`'s P frame: our skip/coded
grid now matches ffmpeg through pair 5; **pairs 6–7 still diverge** — the first
coded MB there is `P_8x8` (mb_type 3) and its sub-partition CABAC parse
(sub_mb_type / mvd contexts under MBAFF) produces wrong values without
desyncing the terminate, then the skip flags for the bottom MBs of pairs 6/7
flip. NEXT: audit `parse_p_macroblock_cabac`'s P_8x8 path — `amvd_sum` /
`ref_idx_gt0_neighbors` cell geometry and `sub_mb_type` decode for MBAFF
frame-coded pairs (the #32b amvd convention applied to the grid neighbours).
Also: `cabac_b.rs` has ~11 unconditional `eprintln!` debug lines in the inter
parse path (pre-existing) — gate behind `KINETIX_BINTRACE` before MBAFF P/B is
a supported path.

## SESSION #32p (2026-08-27) — CABAC MBAFF I-slice desync (#32e item 6) — ROOT-CAUSED AND FIXED

**BUG**: `end_of_slice_flag` was decoded after *every* macroblock in the CABAC
slice-data loop. Spec §7.3.4: in an MBAFF **frame**, when
`CurrMbAddr % 2 == 0` (the TOP macroblock of a pair) the loop sets
`moreDataFlag = 1` **unconditionally** — `end_of_slice_flag` is coded only
after the *bottom* macroblock of each pair. The decoder therefore consumed one
spurious `decode_terminate()` bin after each pair-top MB (8 phantom bins on the
16-MB `mbaff_i1` clip); each mid-slice terminate that returns 0 renormalises
the arithmetic engine, so `range`/`offset` drifted from ffmpeg's while the
context models stayed in lockstep — exactly the #32o signature (crate/oracle
agree bin-for-bin, both wrong; ffmpeg decodes the 4 centre MBs as I_16x16, the
crate as I_NxN; desync surfaces as a terminate=1 at MB14).

Found via `ffmpeg -bsf:v trace_headers` (confirmed FRAME_MBAFF: frame_mbs_only=0
mb_adaptive_frame_field=1 field_pic=0, no scaling matrix) plus re-reading the
spec slice_data() do/while, and confirmed by `KINETIX_NO_FIELD_BINS=1` letting
the parse run to completion (it removes a compensating number of bins).

**FIX** (`slice_data/cabac_i.rs`, `cabac_p.rs` ×2 sites, `cabac_b.rs` ×2 sites):
guard every `decode_terminate()` in the slice-data loop with
`!(mbaff_frame && mb_idx % 2 == 0)`. CAVLC is unaffected (`more_rbsp_data()`
consumes no bits). Also gated three unconditional `eprintln!` debug lines in
cabac_i/cabac_p behind `KINETIX_BINTRACE`.

**RESULT** — CABAC MBAFF I-slice is now **BIT-EXACT vs ffmpeg**:
- `dbg_g6_mbaff_deblock` `g6_cabac_i` (reference decoded WITH the in-loop
  filter): **gate-ON luma sad=0 max=0, cb/cr sad=0** — first pixel-exact CABAC
  MBAFF I frame. (Was "wholesale diffs / CABAC MBAFF parse desync" per #32i.)
- `dbg_g5_i1_diffmap` with `KINETIX_SKIP_DEBLOCK=1`: **0/512 differ, max=0** on
  every pair, chroma 0/1024.
- Parsed mb_type grid matches `ffmpeg -debug mb_type` exactly (MB3/5/10/12 =
  I_16x16, borders I_4x4, all `t8=false`).

The "max=3 residue" seen in bare `dbg_g5_i1_diffmap` is a **harness artefact**,
NOT a decoder bug: that diagnostic compares our (correctly in-loop-deblocked)
output against an `ffmpeg -skip_loop_filter all` reference. The `mbaff_i1`
stream has `disable_deblocking_filter_idc=0` with alpha/beta offsets 0:0 —
i.e. deblocking IS enabled (x264 `deblock=0` sets offsets `0:0`, it does NOT
disable the filter; `--no-deblock` would). ffmpeg's trace_headers confirms
`disable_deblocking_filter_idc = 0`. So our decode (bit-exact pre-deblock,
then the spec-mandated filter) is correct and the g6 harness — which uses a
matching filtered reference — proves it.

### Remaining h264 gates for `pixel_exact` — scoped 2026-08-28 (#32p)

1. **CABAC/CAVLC MBAFF P/B** — `decode_interlaced_mbaff` returns `Fallback` for
   every non-I slice (`interlaced.rs:280`), and the interlaced module has **no
   inter-decode path at all** (PAFF B-field also unimplemented; PAFF P-field
   has `decode_interlaced_p_field`). Wiring MBAFF P/B needs ref-list
   construction + DPB access + MC built in the interlaced module (or shared
   from `decoder/mod.rs`). `reconstruct_mbaff_inter_luma`/`_chroma` exist but
   are opt-in (`KINETIX_MBAFF_FIELD_MC`) with known gaps (#32f items 6–8).
   For `mbaff_ip`/`mbaff_ibp` (all-frame-coded P/B pairs per #32f) the
   reconstruction reduces to progressive inter into contiguous halves — the
   tractable first target — but the slice *setup* machinery is the real work.
   → biggest remaining chunk; a dedicated Phase G.2/G.4 effort.

2. **Non-16-aligned crop-edge gap** — DONE (#32s). Reconstruct + deblock at
   coded (MB-aligned) dimensions; crop to the display rect at each `VideoFrame`
   build site. `mb_cols`/`mb_rows` now derived exactly from
   `coded_width / 16` (no `div_ceil` undercount).

3. **Phase G.5** — PAFF + MBAFF corpus bit-exact validation (blocked on 1).

4. **`pixel_exact` flip** — gated on 1 and 3.

Note on `above_right_mb_decoded`: a spec-motivated reconstruction fix was
prototyped this session (MBAFF pair-scan — the *bottom* MB's above-right
neighbour is decoded later per §6.4.8, so its top-right prediction samples read
as stale zero) and **reverted** since the residue it was chasing turned out to
be the harness/deblock artefact above. Still a plausible latent bug for content
that uses a top-right-dependent Intra mode on a bottom MB's rightmost 4×4/8×8
block — revisit if a real diff is ever traced there.

## SESSION #32o (2026-08-27) — cont'd: CABAC MBAFF I-slice desync (#32e item 6) re-narrowed

The `mbaff_i1` clip (High profile, 4×4 MBs, x264 `--interlaced`; testsrc) still
desyncs: `parse_i_slice_cabac` reads MB0..MB14 then the terminate bin after
MB14 reads **1** ("end_of_slice_flag mismatch") → grey scaffold, wholesale
pixel divergence rows 16–63.

NEW FACTS this session:
- Every MB decodes as `Intra4x4` with `mb_field_decoding_flag=false` (all 8
  pairs frame-coded); MB2..MB14 mostly `transform_size_8x8_flag=true`
  (Intra_8x8, High profile). So the field-residual tables (#32e item 5) are
  never exercised — the trigger is a frame-coded MBAFF stream.
- `dbg_mbaff_oracle` (hand transcription of ffmpeg's I-slice path, residual
  walk through the crate's own `ResidualCabacContext`) is in **exact lockstep**
  with the crate parser: identical CABAC engine `state=range/offset` at EVERY
  MB boundary through MB14 (e.g. both `0x0158/0x00000157` at MB14), identical
  cbp / chroma_pred_mode / t8 / MPM modes. The only column that differs is the
  oracle's `qp` display — a KNOWN oracle bug (#32e: "oracle dqp ignores
  negative deltas", crate's qp is right; qp does not affect residual parsing).
  Both then hit "premature end_of_slice at MB14".
- => The hand-oracle route is **exhausted** for this bug: it shares the crate's
  residual coefficient code (`decode_block_8x8` sig/last/abs bin loop), so any
  bug there is invisible to it (circular calibration — same failure mode as
  TRANS_IDX_LPS[28] / the amvd convention). Progressive High/8×8 CABAC I
  (`conformance_matrix` `high8x8_i`) is bit-exact, so the bug is triggered by
  an MBAFF-specific INPUT into that shared code — prime suspects, in order:
  (a) `non_zero_count_cache` left/top nnz feeding `get_cabac_cbf_ctx` for the
  4×4 chroma-AC / luma blocks near MB14 (MBAFF pair-neighbour nnz derivation
  vs the plain `nz[grid-1]`/`nz[grid-mb_cols]` the crate uses in cabac_i.rs —
  verify against ffmpeg `fill_decode_caches` `left_block_options` + the
  `nnz = CABAC && !IS_INTRA ? 0 : 64` unavailable-fill);
  (b) the 8×8 significance-map `SIG_COEFF_CTX_INC_8X8` frame-row indices;
  (c) coeff visit order for the 8×8 groups.
- **NEW 2026-08-27 (#32o cont'd) — the desync is a MB_TYPE misparse that
  originates BEFORE MB3.** `ffmpeg -debug mb_type` on `mbaff_i1` prints:
  ```
  i  i  i  i
  i  I  I  i
  i  I  I  i
  i  i  i  i
  ```
  i.e. ffmpeg decodes the **4 centre MBs — frame-grid (1,1),(2,1),(1,2),(2,2)
  = crate MB3, MB5, MB10, MB12 — as I_16x16**; the 12 border MBs as I_4x4/8x8.
  The crate (and `dbg_mbaff_oracle`) decode ALL 16 as I_NxN. Since MB3's
  mb_type bin-0 context is 0 either way (both neighbours I_NxN) and the crate
  is in exact engine lockstep with the oracle, ffmpeg would decode the same
  bin-0 value from the same engine state — therefore **the arithmetic engine
  has already drifted from ffmpeg's before MB3**, i.e. the wrong-bin-count
  bug is inside MB0, MB1, the pair-1 `mb_field_decoding_flag` read, or MB2
  (MB2 = first `transform_size_8x8_flag=true` / Intra_8x8 MB).
  - MB0 bin trace is pristine and matches progressive behaviour exactly
    (field-flag ctx70=0, mb_type ctx3=0→I_NxN, t8 ctx399=0, 16×I4x4 MPM,
    chroma ctx64=0, cbp 0x2f, dqp 0, normal 4×4 residual).
  - Progressive High/8×8 CABAC I is bit-exact (`conformance_matrix high8x8_i`),
    so MB2's Intra_8x8 residual code itself is proven — suspect the MBAFF
    wrapper around it (field-flag interaction, or an off-by-one in the
    pair-scan commit of MB1's state that MB2 then reads).
  - ffmpeg decodes the whole frame with **0 errors**; SPS is High, 64×64,
    `mb_adaptive_frame_field_flag=1`, no scaling matrix.
  NEXT (focused): bisect MB0→MB1→field1→MB2 by dumping the crate's engine
  byte-position + a running bin count at each of those 4 boundaries and
  checking which one first disagrees with a from-scratch hand count of the
  spec syntax for that MB (MB0/MB1 are plain I_4x4 — fully hand-countable).
- DECISIVE NEXT STEP (now unblocked — `clang` 22.x is on PATH): build a
  compiled-ffmpeg oracle. Vendored sources already at repo root
  (`h264_cabac_ref.c`, `cabac_ref.c`/`.h`, `ff_cabac_functions.h`). Minimum
  viable: compile `get_cabac_cbf_ctx` + `decode_cabac_residual_internal` +
  the real cabac engine (`cabac.c` core) with hand-mocked minimal
  `H264SliceContext` (cabac_state[1024], non_zero_count_cache, left/top_cbp,
  intra4x4_pred_mode_cache is not needed), record the crate's engine state +
  `cabac_state` array + nnz cache immediately before MB14's residual, replay,
  and diff per-bin ctxIdx + coefficient outputs.

## SESSION #32o (2026-08-27) — CABAC P/B CONFORMANCE-MATRIX DESYNC IS RESOLVED (verification only)

Re-audit of the long-open "conformance_matrix.rs cabac_p / cabac_b cells fail
(max_abs_diff≈127, desync in parse_p_slice_cabac)" item (Phase H,
`todo-h264.md` "NEW (2026-08-22)"). It is **closed** — fixed by intervening
work (the #32b amvd-neighbour-convention fix and the #32j CAVLC/CABAC inter-MB
`transform_size_8x8_flag` fix, most likely):

- `cargo test -p tpt-kinetix-h264 --test conformance_matrix` → `[PASS] cabac_p`
  / `[PASS] cabac_b`, both deblock variants, `max_abs_diff=0
  differing_samples=0/4608`. `high8x8_i` (High/8×8 CABAC I) also `[PASS]`.
- `examples/dbg_cabac_p_matrix` — all 16 repro cases (incl. the qp18/qp21
  streams that straddled the preCtxState 63/64 boundary and used to hit
  "end_of_slice_flag mismatch (P-CABAC)") now decode bit-exact; every P slice
  reaches MB11 with `eos=true is_last=true`.
- Full `cargo test -p tpt-kinetix-h264` suite green (all integration binaries,
  0 failed); lib 246/246.

Toolchain note (invalidates the old "no C toolchain on the Windows dev box"
blocker): `clang` 22.x is on PATH (scoop llvm) and `ffmpeg` is present — a
verbatim-C CABAC oracle is now buildable here if a future desync needs one.

`pixel_exact` stays `false`: the remaining gates are Phase G interlaced
PAFF/MBAFF (CABAC MBAFF residual desync #32e item 6; PAFF/MBAFF corpus
validation G.5) and the non-16-aligned crop-edge gap — NOT CABAC P/B any more.

## SESSION #32n (2026-08-27) — sad=92 RESIDUE ROOT-CAUSED AND FIXED; MBAFF P FRAME NOW PIXEL-EXACT VS FFMPEG

1. **ROOT CAUSE of #32m item 8b's residual luma diffs** (`deblock.rs`): ffmpeg
   does NOT filter the ODD interior edges (`edge_index` 1 and 3, which cut
   through the middle of each 8×8 transform block) of ANY macroblock carrying
   `transform_size_8x8_flag`: its interior-edge loop computes
   `deblock_edge = !IS_8x8DCT(mb_type & (edge<<24))` with
   `MB_TYPE_8x8DCT = 0x01000000` (bit 24) and `continue`s the whole edge —
   luma AND chroma, both directions, intra or inter — when the bit is set
   (h264_loopfilter.c `filter_mb_dir`; interior edge 2, the 8×8-block
   boundary, is still filtered). Our decoder derived bS = 2 (nz rule) on those
   edges and filtered them, over-smoothing exactly the P16x8/P8x8ref0 MBs of
   row 3 flagged by #32m's single-edge bisect ((3,3,V,ei=1) skip → sad 68,
   (1,3,V,ei=1) → 87). The mv_store cell layout suspicion of #32m item 8c is
   CLOSED: committed MV grids were correct all along (consistent with
   dbg_qpel_brute's bit-exact MC validation); only the deblock consumed them
   on edges ffmpeg never touches.
2. **FIX**: new `DeblockMbInfo::transform_8x8` flag (default `false`, threaded
   from `Macroblock::transform_size_8x8` at every info-construction site:
   `mbaff_deblock_infos`, plain P/B + MBAFF-I sites in decoder/mod.rs,
   interlaced.rs); all four interior-edge loops (plain luma V/H,
   `filter_mbaff_mb` V/H) now skip `ei ∈ {1,3}` when set. Boundary edges are
   unaffected (ffmpeg only applies the skip inside the interior loop).
3. **RESULT (dbg_g6_mbaff_deblock)**: `g6_cavlc_ip` P frame **luma sad=0
   max=0 vs ffmpeg fully-filtered — first pixel-exact MBAFF P frame**;
   I frames remain bit-exact; chroma bit-exact. Determinism probes stable at
   p-frame sad=Some(0) ×5 reps.
4. VALIDATION: lib tests 246 passed / 0 failed; dbg_skip_lf, high_profile_8x8_conformance
   (3), p_slice_reference all green; clippy clean on changed code; fmt applied.

## SESSION #32l (2026-08-26) — MBAFF DEBLOCK DEFAULT-ON; OOB FIX; SUITE GREEN

1. **DEFAULT-ON**: `mbaff_deblock_infos` now returns `Some` for every MBAFF
   *frame* picture (P/B sites in `decoder/mod.rs`, MBAFF I path in
   `decoder/interlaced.rs`) — the full-frame orchestrator is the default
   deblocker there, justified by #32k's edge-set diff (orchestrator ⊇ plain,
   no contradictions) and its measured accuracy (I frames bit-exact vs
   ffmpeg fully-filtered; P frame sad=92 max=3 chroma-exact vs plain 472).
   Progressive / PAFF pictures keep the plain loop (bit-exact there).
   `KINETIX_MBAFF_DEBLOCK_PLAIN=1` restores the legacy pass for bisecting;
   the old `KINETIX_MBAFF_FIELD_MC` deblock gate is gone (the field-MC
   reconstruction path keeps its own separate gate in reconstruct.rs).
2. **OOB FIX** (`deblock_fieldcoded_above_boundary_mcaff`, exposed by the
   default-on flip on the CABAC `mbaff_ip` clip which has field pairs): the
   luma guard checked `y<2 || y+2>=height` while the filter touches y-4..y+3;
   chroma checked `y<1 || y+1>=cheight` while touching y-2..y+1. Both widened
   (`y<4 || y+3>=height`, `y<2 || y+1>=cheight`). Previously latent because
   the special case only fires under the (then opt-in) path.
3. VALIDATION: full crate suite green across all test binaries (0 failures),
   lib 246 tests green, workspace clippy `-D warnings` clean, fmt clean.
   G.6 harness confirms default behaviour: cavlc_i / cavlc_ip-I bit-exact vs
   ffmpeg fully-filtered, P frame sad=92 max=3 chroma-exact.
4. **CABAC MBAFF DESYNC DIAGNOSTIC DATA** (for #32e item 6): rerunning
   `g4_mbaff_i1_diffmap` shows the CABAC I-slice parse still fails at
   `cabac_i.rs` end_of_slice ("end_of_slice_flag mismatch") → grey scaffold,
   wholesale divergence. NEW FACTS: (a) ALL 8 pairs decode
   `mb_field_decoding_flag=false`, so the desync is NOT field-pair related;
   (b) it fires mid-slice (non-last MB reads terminate=1), meaning some earlier
   bin consumption drifted; (c) the CAVLC twin clip is bit-exact, so the bug
   is confined to the CABAC bin path (suspects: intra 8×8 bins under t8=true,
   cbp_ctx propagation across grid slots, or I16x16 CBP bin mapping).
   Instrumentation ready: `KINETIX_BINTRACE=1` on that test prints per-MB
   engine state (`TRC MBn ... state=0x…/0x…`) for replay comparison.


## SESSION #32k (2026-08-26) — FIELD PAIRS CONFIRMED IN TESTSRC P FRAME; MBAFF NEIGHBOUR RULES IMPLEMENTED

1. **FIELD-CODED PAIRS EXIST in `g6_cavlc_ip`'s P frame** (contradicts the
   earlier #32f item 8 note that x264 emits none for testsrc under CAVLC):
   per-MB diff clustering vs ffmpeg shows the residual divergence confined to
   MBs {(1,3):38, (3,2):2, (3,3):45} — bottom members / pair-top of
   field-coded pairs in columns x=1 and x=3.
2. **MBAFF deblock neighbour rules implemented** (`deblock.rs::filter_mbaff_mb`),
   port of h264_slice.c `fill_filter_caches` lines 2422–2437:
   - TOP: field-curr → 2 grid rows up (same parity); frame-curr → 1 row;
     field-coded pair-top steps back DOWN one row when the directly-above MB
     is frame-coded (`top_xy += stride & (INTERLACED(top)-1)`).
   - LEFT: LTOP/LBOT split shifts one grid row on coding-convention mismatch
     (bottom member: LTOP up; top member: LBOT down).
   - Vertical boundary bS now derives per-segment from LTOP (segments 0–1) /
     LBOT (segments 2–3) via `derive_bs_pair` directly.
   - Debug override `KINETIX_MBAFF_DEBLOCK_PLAIN=1` forces the plain pass for
     A/B bisecting.
3. **Pre-deblock isolation**: new harness stage proves our PRE-deblock P-frame
   pixels are bit-exact vs `ffmpeg -skip_loop_filter all` (sad=0 max=0) — the
   entire remaining gap is inside the deblock special cases.
4. **Flake fix** (`tests/dbg_mvp_trace.rs`): duplicate `use std::process::Command`
   removed (broke workspace clippy).
5. STATUS: full crate `--tests` green, lib 246 tests green, workspace clippy
   `-D warnings` clean, fmt clean. Remaining known diff: `g6_cavlc_ip` P frame
   luma sad=92 max=3 (85 samples, field-pair regions).
6. **ABLATION RESULT (#32k cont'd)**: new env-gated ablation matrix in
   dbg_g6_mbaff_deblock (`KINETIX_DBG_NO_MIXEDGE`, `KINETIX_DBG_NO_FIELDCODED_ABOVE`)
   proves the residue is NOT caused by either MBAFF special case — sad stays
   exactly 92 with either or both disabled.
7. **CORRECTION (#32k cont'd) — there are NO field-coded pairs** in
   `g6_cavlc_ip`: the `KINETIX_DBG_BS` per-edge trace shows every MB has
   `field=false`. The earlier per-MB-clustering "field pairs" reading was
   wrong. Yet orchestrator (sad=92) and plain loop (sad=472) still disagree on
   this all-frame-coded data — contradicting the synthetic equivalence unit
   test, so some REAL-data input (skip-MB nz/cells, I16x16, P8x8ref0 motion)
   exercises a divergence between the two implementations that the unit data
   does not. NOTE: forcing PLAIN also removes deblocking entirely from the
   interlaced.rs I-frame path (it has no plain loop), which is why the
   regression pin is skipped under that override.
8. **EDGE-SET DIFF (#32k cont'd)**: new `tests/dbg_edge_diff.rs` compares the
   two implementations' effective edge sets on the traced run. RESULT:
   **only-plain = 0** — every edge the plain loop filters, the orchestrator
   filters with the identical bS (no contradictions); the orchestrator applies
   136 ADDITIONAL nonzero-bS edges (interior bS=3 edges of intra MBs, intra
   bS=4 boundaries) that the plain loop derives as bS=0 or skips on this
   stream. Since pre-deblock pixels are bit-exact and the orchestrator lands
   at sad=92 (vs plain 472), those extra edges are the correct ones and the
   orchestrator supersedes the plain loop for MBAFF frames.
   TOOLING NOTE: PowerShell `Out-File` writes UTF-16LE — dbg_edge_diff decodes
   the BOM accordingly; regenerate the trace with
   `$env:KINETIX_DBG_BS='1'; $env:KINETIX_BINTRACE='1'; cargo test ... --test
   dbg_g6_mbaff_deblock` before running it.
8b. ABLATION MATRIX #2 (#32l): per-edge-class switches (`KINETIX_DBG_NO_VBOUND`,
   `KINETIX_DBG_NO_VINT`, `KINETIX_DBG_NO_HBOUND`, `KINETIX_DBG_NO_HINT`) —
   removing ANY edge class increases sad (vbound 121, vint 332, hbound 177,
   hint 232 vs baseline 92): EVERY edge class the orchestrator applies moves
   the output TOWARD ffmpeg. The residue is therefore per-edge strength /
   rounding differences on individual edges, not a wrong decision class.
8c. SINGLE-EDGE BISECT RESULT (#32m, decisive): fixed a slicing bug in the
   bisect harness (used W*H instead of FRAME=6144 stride for ff references —
   earlier ~321k readings were garbage). With correct comparisons: baseline
   sad=92; skipping the VERTICAL INTERIOR edge ei=1 of MB(3,3) drops sad to
   **68**, MB(1,3) ei=1 to 87; skipping MB(3,3) BOUNDARY raises to 110.
   => The residue localizes to the MV-rule bS on INTERIOR edges of inter MBs
   (P8x8ref0 / P16x8 partition boundaries): our committed sub-partition MV
   grid yields slightly different within-MB bS than ffmpeg's. NEXT: verify
   the mv_store cell layout for 8x8-partitioned inter MBs against ffmpeg's
   b_stride motion_val grid (mv.rs `predict_slice_mvs` / commit path).
9. NEXT: (a) root-cause WHY the plain loop under-derives on this stream (its
   inputs come from the same `parsed.nz`/mv_store, so suspect the skip-run /
   field-flag timing leaving some MBs' nz uncommitted in the CAVLC P path);
   (b) chase the residual sad=92 max=3 luma diffs (85 samples near interior
   edges of MB(3,2)/(3,3)/(1,3)) once (a) lands; (c) CABAC MBAFF desync
   (#32e item 6) remains the blocker for CABAC interlaced clips.

## SESSION #32j (2026-08-26) — CAVLC INTER-MB `transform_size_8x8_flag` BUG FIXED; MBAFF P FRAME NOW NEAR-EXACT

1. **ROOT CAUSE of the CAVLC "cbp code_num out of range" desync** (`slice_data/cavlc.rs`):
   the inter-MB paths (`parse_p_macroblock`, B-slice twin) never read
   `transform_size_8x8_flag` (§7.3.5.1: present between `coded_block_pattern`
   and `mb_qp_delta` when `transform_8x8_mode_flag && CodedBlockPatternLuma > 0`;
   inter MBs are never Intra_16×16). The intra path already handled it — only
   the inter paths were broken, so ANY High-profile stream (t8=true PPS) with a
   CAVLC P/B slice whose first coded inter MB has luma CBP ≠ 0 desynced
   immediately: the missing bit read silently consumed mb_qp_delta's first bit.
   Fix reads the flag in both inter paths (P + B), stores it on
   `Macroblock::transform_size_8x8`, and threads `is_8x8` into
   `parse_intra_residuals` so residuals parse via the 8×8 scan when set.

2. **Inter 8×8 reconstruction** (`reconstruct.rs::reconstruct_inter_luma`): new
   branch for `transform_size_8x8` inter MBs — motion-compensates each 8×8
   region with the committed MV of its top-left 4×4 cell and adds the 8×8
   inverse-transformed residual (`dequant_idct_8x8_scan` + progressive zigzag);
   explicit weighted prediction applied per-4×4 quadrant since
   `combine_weighted` is fixed at 16 samples.

3. **RESULT (dbg_g6_mbaff_deblock, gate ON vs ffmpeg fully-filtered)**:
   `g6_cavlc_ip` P frame went from PARSE FAILURE (skip-scaffold output,
   sad≈249k) to **luma SAD=92 max=3, chroma BIT-EXACT (max=0)**. I frames
   remain bit-exact. Remaining luma max=3 = small MC/rounding residue, next
   target.

4. **FLAKE FIX** (`tests/dbg_b_implied_pred.rs`): `p_header_manual_walk` /
   `b_implied_pred_oracle` raced other tests regenerating the shared
   `dbg_b_implied/b_boxmv.*` files (truncated reads → unwrap/empty-YUV panics).
   Both now generate into their own subdirectories. Full crate `--tests` suite
   green (0 failures across all binaries), workspace clippy `-D warnings`
   clean, fmt clean.

## SESSION #32i (2026-08-26) — MBAFF DEBLOCK VALIDATED BIT-EXACT VS FFMPEG ON REAL CONTENT

New ffmpeg-gated harness `tests/dbg_g6_mbaff_deblock.rs`: encodes interlaced
clips with deblocking **ENABLED** (x264 defaults, i.e. no `deblock=0` — the G.5
corpus never exercised the in-loop filter), decodes the reference WITHOUT
`-skip_loop_filter`, and compares our output with the
`KINETIX_MBAFF_FIELD_MC=1` gate on vs off.

RESULT: `g6_cavlc_i` (CAVLC MBAFF I frame, 64×64) with the gate ON is
**BIT-EXACT vs ffmpeg's fully-filtered decode — luma SAD=0 max=0, cb/cr max=0**
(gate OFF diverges: luma sad=519 max=3, proving the gate controls the path).
This is the first end-to-end pixel-exact validation of `deblock_frame_mbaff`
on real x264 content; pinned as a hard assertion in the harness (regression:
failure ⇒ deblock orchestrator, MBAFF I-frame recon, or CAVLC parse regressed).

Known-divergent (pre-existing, NOT deblock-related): `g6_cabac_i` wholesale
diffs = the CABAC MBAFF parse desync (#32e item 6); `g6_cavlc_ip` P frame
sad≈249k = MBAFF P reconstruction gaps (#32f item 8). Also observed again on
this clip: `P CABAC parse error: Unsupported("cbp code_num out of range")` on
the CABAC P slice — another face of that desync.

## SESSION #32h (2026-08-26) — FULL-FRAME MBAFF DEBLOCK ORCHESTRATOR LANDED + WIRED IN

1. **`deblock_frame_mbaff`** (`deblock.rs`): full-frame orchestrator walking every
   macroblock of a FRAME_MBAFF picture in raster order, port of ffmpeg
   `ff_h264_filter_mb`/`filter_mb_dir` MBAFF semantics:
   - mixed-interlace first VERTICAL edge via `deblock_first_vertical_edge_mcaff`
     (left-pair LTOP/LBOTTOM indexing per ffmpeg's `left_mb_xy`), marking the
     edge done;
   - fieldcoded-above pair-top HORIZONTAL boundary via
     `deblock_fieldcoded_above_boundary_mcaff`, once per above-pair member;
   - field-aware boundary rules: dir==0 either-intra → 4 always (FRAME_MBAFF
     clause); dir==1 either-intra → 4 unless either side field-coded → 3
     (`IS_INTERLACED(mb|mbm)` guard); forced bS = 1 without MV check across a
     horizontal field/frame mismatch; plain `derive_bs_segments` elsewhere with
     the current MB's field-aware `mvy_limit`.
2. **Parity-doubled addressing**: ffmpeg filters field MBs through a virtual
   contiguous field plane (doubled `linesize`, parity-shifted dest). Expressed
   in frame coords: new stepped edge helpers (`deblock_luma_edge_stepped`,
   `deblock_chroma_edge_stepped` over `filter_luma_at`/`filter_chroma_both_at`)
   take `(origin_y, y_step)` — a field MB occupies rows
   `(pair_top*16 | parity) + k*2`. Frame-coded MBs use step 1 and degenerate to
   plain addressing.
3. **Wired into the decoder behind `KINETIX_MBAFF_FIELD_MC=1`**:
   P-slice CABAC path and B-slice path in `decoder/mod.rs`, and the MBAFF I-frame
   path in `decoder/interlaced.rs`, each via new helpers `mbaff_deblock_infos`
   (flat raster infos carrying `mb_field_flag` via `DeblockMbInfo::new_field`) +
   `run_mbaff_deblock`; gate absent ⇒ byte-identical frame-convention behaviour.
4. **Correctness pins** (new tests): orchestrator ≡ plain per-MB pass for
   all-frame-coded frames (luma AND both chroma planes, varied QP/motion) — this
   caught two real bugs during development: chroma interior edges must derive
   their own bS from co-located chroma blocks AND fire only once per direction
   (chroma offset 4), not at every luma edge index. Plus: parity isolation of
   the stepped filter (y_step=2 touches only the member's parity rows),
   field-pair luma+chroma filtering smoke test, and the mixed-left-pair
   first-vertical-edge special case applying bS = 4 strong filtering despite
   zero coefficients. Test content note: purely linear ramps are fixed points
   of the strong filter — use the small-amplitude non-linear texture helper.
   248 lib tests green, workspace clippy `-D warnings` clean, fmt clean.

## SESSION #32g (2026-08-26) — MBAFF FIELD DEBLOCKING PRIMITIVES LANDED

1. **`DeblockMbInfo` gained a `field: bool` flag** (`new_field()` constructor;
   frame-convention callers via `new()` are unchanged). It selects the
   §8.7.2.1 motion-rule y-threshold: field-coded MBs flag a boundary at
   |Δmv_y| >= **2** quarter-samples instead of 4 (ffmpeg's
   `mvy_limit = IS_INTERLACED(mb_type) ? 2 : 4`). `derive_bs_pair`/
   `derive_bs_segments` now take an explicit `mvy_limit`; all existing call
   sites pass `mvy_limit(cur.field)` so plain-frame behavior is bit-identical.
2. **Mixed-interlace first VERTICAL edge** (`first_vertical_edge_bs` +
   `deblock_first_vertical_edge_mcaff`): mechanical port of ffmpeg
   `ff_h264_filter_mb`'s FRAME_MBAFF block (h264_loopfilter.c @master).
   bS[8]: current intra → all 4; neighbour intra → 4; else
   `1 + !!(cur.nz[(i>>1)*4] | left.nz[off[i]])` with ffmpeg's offset tables
   (`MBAFF_FIRST_EDGE_OFFSET_{FRAME_TOP,FRAME_BOTTOM,FIELD}`) and j-mapping
   (`i&1` when cur frame-coded, `i>>2` when field-coded). NO MV rule here —
   ffmpeg derives these from coefficients only. Filtering reproduces the
   two-call geometry (`filter_mbaff_call`: group-of-2 rows per bS, step-2 for
   the frame-cur case; parity-band addressing derived for the field-cur case
   from ffmpeg's band-start + doubled-stride + bottom-member `-= linesize*15`
   convention). ffmpeg's "strong iff `bS[0] < 4` fails, decided once per
   call" quirk is preserved deliberately.
3. **Fieldcoded-above pair-top boundary** (`fieldcoded_above_boundary_bs` +
   `deblock_fieldcoded_above_boundary_mcaff`): port of `filter_mb_dir`'s
   "filter twice, once per field" special case. bS is either-side-intra →
   **3, not 4** (ffmpeg passes `intra=0`, keeping the edge on the weak path);
   else `1 + !!(cur.nz[i] | above.nz[12+i])`. Applied once per above-pair
   member (ffmpeg's `j` loop); luma spans 16 every-other-row positions over
   the full 32-row band, chroma 8 over the chroma band.
9 new unit tests (mvy-limit halving incl. x-threshold NOT halved, both bS
   derivation tables against hand-derived ffmpeg values, parity-isolation +
   group-of-2 geometry of `filter_mbaff_call`). 242 lib tests green,
   clippy `-D warnings` clean, fmt clean.

REMAINING for this item: **DONE in session #32h** (see above) — the full-frame
MBAFF deblock orchestrator exists (`deblock_frame_mbaff`), implements the
mixed-edge special case, the field-aware boundary rules, and is wired into the
decoder behind `KINETIX_MBAFF_FIELD_MC=1`. Pixel-exactness vs ffmpeg on real
interlaced content **validated in session #32i** (CAVLC I frame bit-exact with
the filter enabled; see dbg_g6_mbaff_deblock.rs). Remaining for full MBAFF:
CABAC MBAFF residual desync (#32e item 6), MBAFF P reconstruction (#32f item
8), field-coded-pair coverage (no x264 CAVLC clip emits them yet).

## SESSION #32f (2026-08-26) — CAVLC MBAFF I-slice: pair-addressing bug fixed; I frame now PIXEL-EXACT

> Harness: existing `tests/dbg_g5_interlaced.rs` corpus (`mbaff_cavlc_ip`,
> 64×64, cabac=0, interlaced=1, threads=1). New env-gated
> `CAVLC-TRC` per-MB trace lines in `parse_i_slice` (same convention as the
> CABAC `TRC`/`BIN` traces, `KINETIX_BINTRACE=1`).

1. **BUG FIXED — CAVLC MBAFF pair addressing** (`slice_data/cavlc.rs`):
   `parse_i_slice` iterated macroblocks in PLAIN RASTER order
   (`mb_x = idx % mb_cols`, `mb_y = idx / mb_cols`) while an MBAFF frame's
   macroblock addresses enumerate each PAIR as (top, bottom) before advancing
   horizontally (§6.4.2 — addr 2k/2k+1 are the two MBs of pair k at frame-MB
   rows `2p`/`2p+1`). Every bottom MB therefore derived its neighbour contexts
   (`nC` for coeff_token, Intra_4x4 MPM left/top availability) from the WRONG
   grid slots; the parse drifted and died at MB6 with
   "non-intra mb_type in I-slice" (mb_type=79 garbage). This is the CAVLC twin
   of session #32e's CABAC `grid_idx` bug — same disease, different parser.
   Fix mirrors the CABAC loop exactly: pair-based `(mb_x, mb_y)` derivation +
   commit to the MB's own frame-MB grid address (`grid_idx =
   mb_row*mb_cols + px`) for `macroblocks[]`/`nz[]`/`pred_ctx[]`/
   `field_flags[]`. Also documented that ffmpeg reads `mb_field_decoding_flag`
   as one raw bit BEFORE mb_type for each pair-top MB (h264_cavlc.c @n5.1
   lines 728–731) — the crate already did this, now recorded so it can't be
   "reordered" by accident.
2. **RESULT:** `mbaff_cavlc_ip` I frame decodes **pixel-exact vs ffmpeg
   (SAD=0)** for the first time on a CAVLC MBAFF stream; the P frame still
   diverges (SAD≈5.4e4) because P-slice CAVLC MBAFF is not implemented (the
   P/B parsers don't read `mb_field_decoding_flag` yet — see the G-scope note
   below). All 231 lib tests green.
3. **P-SLICE CAVLC MBAFF PARSE IMPLEMENTED (same session):**
   `parse_p_slice` gained `mb_aff`/`field_pic_flag` parameters and full MBAFF
   awareness, ported from ffmpeg h264_cavlc.c @n5.1 `ff_h264_decode_mb_cavlc`
   lines 709–731 exactly:
   - `mb_skip_run` is now an i32 with ffmpeg's −1 sentinel; the coded-MB path
     resets it to −1 (replicating the `if (sl->mb_skip_run--)` post-decrement
     wrap-to-−1 trick), so fresh runs are re-read after every coded MB.
   - Field-flag timing: inside a skip run, when the run hits 0 on a pair-TOP
     skipped MB, one raw bit is read immediately (it is the pair flag of the
     pair whose bottom MB is about to be coded); otherwise the bit is read
     before mb_type of every coded pair-top MB.
   - Pair-based `(mb_x, mb_y)`/grid addressing (as in the I parser);
     `macroblocks[]`/`nz[]`/`pred_ctx[]` commit to frame-MB addresses so
     `predict_slice_mvs` sees each MB at its raster address. Intra-in-P and
     inter residuals now take a real MBAFF-aware `NeighbourCtx`.
   - All 10 stale `parse_p_slice` call sites (tests/examples) updated;
     clippy `-D warnings` clean; whole `--tests` suite green.
   STATUS: parse completes on `mbaff_cavlc_ip`'s P slice, but pixels are NOT
   pixel-exact yet — the residual gap is reconstruction-side: MVP lacks
   FIX_MV_MBAFF row-doubling/halving for field pairs (h264_mvpred_ref.h) and
   MC lacks field-parity reference sampling. That (plus B-slice MBAFF) remains
   the next G-phase work item.

6. **PARITY-AWARE RECON SCAFFOLDED (2026-08-26, later same day):**
   `reconstruct.rs` gained `reconstruct_inter_frame_ex` (MBAFF-aware twin of
   `reconstruct_inter_frame`, which now just forwards with `mb_aff=false`).
   When `mb_aff` is set and a macroblock carries
   `mb_field_decoding_flag`, new helpers `reconstruct_mbaff_inter_luma` /
   `reconstruct_mbaff_inter_chroma` run motion compensation in FIELD
   coordinates against the reference's contiguous half-height plane of the
   MB's own parity (pre-extracted once per ref via `FieldRef::planes`, both
   parities), and write predicted+residual rows back at stride-2 spacing with
   the MB's parity offset (`2*y_field + (mb_y & 1)`) — mirroring ffmpeg's
   doubled `mb_linesize`/`mb_uvlinesize` and the parity-shifted destination
   (h264_slice_ref.c @n5.1 lines 2591–2598; luma/chroma src rows read through
   the doubled stride exactly as `mc_dir_part` does). Decoder call site wired
   (`sps.mb_adaptive_frame_field_flag && !header.field_pic_flag`). New lib
   test `reconstruct::tests::mbaff_field_mb_samples_parity_rows` (a vertical
   field pair over a row-ramp reference reproduces `luma[y] == y` exactly);
   232 lib tests green, clippy `-D warnings` clean.
   STATUS: the path is **opt-in** (`KINETIX_MBAFF_FIELD_MC=1`) because it is
   not yet a win on real content: on `dbg_g5_interlaced`'s `mbaff_ip`
   (CABAC P, the only clip whose P slice contains field pairs — MBs 4/5 and
   14/15), enabling it moves that frame's best-match SAD from 257 554 to
   296 585. Root cause of the remaining gap: intra-in-P macroblocks inside a
   field pair are still reconstructed with contiguous frame addressing (they
   must also be parity-interleaved, and their intra prediction must sample
   parity-strided neighbours), and deblocking edge flags ignore
   `mb_field_decoding_flag`. Default output is byte-identical to the previous
   state (verified A/B via the env gate: all four corpus cells unchanged).
   Next: parity-aware intra recon inside P pairs, then flip the gate to
   default-on; B-slice CAVLC/CABAC MBAFF parse, CABAC MBAFF replay harness
   (#32e item 6), field deblocking flags, G.5 interlaced recon, H pixel_exact
   flip.

7. **INTRA-IN-P PARITY RECON + DIAGNOSIS (same day, cont'd):** under the same
   `KINETIX_MBAFF_FIELD_MC=1` gate, intra macroblocks inside a field-coded P
   pair now reconstruct via `reconstruct_luma_at`/`reconstruct_chroma_at`
   with base row `(pair*32|16) + parity` and `y_step = 2` — identical
   geometry to `reconstruct_mbaff_intra_frame`. New deterministic lib test
   `mbaff_field_intra_writes_interleaved_rows` (DC pair fills all 32 lines +
   chroma with 128; calls the helpers directly so it does not depend on the
   env gate). 233 lib tests green, clippy `-D warnings` clean.
   FINDING: on `mbaff_ip` the gate-on SAD is UNCHANGED (296 585) — that clip's
   P slice has no intra MBs, so the inter-MC-vs-ffmpeg divergence is inside
   the field-MC convention itself. Prime suspect for the next session: the
   reference-parity choice. ffmpeg's MBAFF ref lists are split per FIELD
   (`FIX_MV_MBAFF` does `refn <<= 1` / `>>= 1`, i.e. list entries alternate
   frame/field and each entry carries its own `reference-1` parity baked into
   `pic->data`); luma MC samples THAT entry's parity (no correction term),
   while chroma adds `my += 2*((mb_y & 1) - (reference - 1))`
   (h264_mb_ref.c @n5.1 line 290). Our decoder keeps plain frame lists and
   samples the CURRENT MB's parity for both planes, which matches neither
   ffmpeg convention when the bitstream ref_idx maps through the field-split
   list. Next step: decide the spec-correct mapping (§8.2.4.2.3 vs §8.4.2.2)
   for our frame-list ref_idx space — likely "sample the reference at the
   current MB's parity" is right but ref_idx→picture must go through the
   field-split list (idx>>1 picture, idx&1 src parity) — implement, re-run
   the A/B, and flip the gate to default-on once `mbaff_ip` improves.

8. **DIAGNOSIS CORRECTION + NEW COVERAGE (same day, cont'd):** wrote
   `tests/dbg_g5_i1_diffmap.rs::g5_mbaff_ip_pframe_diffmap` (per-MB luma diff
   map with even/odd row-parity breakdown; the original
   `g4_mbaff_i1_diffmap` is preserved alongside). RESULT: `mbaff_ip`
   diverges on **every MB** of the P frame (rows 16–63 fully, max diffs up to
   239) AND its I frame does not match ffmpeg either — the clip's problem is
   the upstream CABAC MBAFF parse desync (#32e item 6), NOT reconstruction.
   All parity A/B conclusions drawn from it (item 7) were therefore invalid;
   the ref-split experiment (`ref_idx>>1` picture / `&1` parity) was reverted.
   Also added corpus clip `mbaff_cavlc_ip2` (testsrc2, CAVLC MBAFF): I frame
   SAD=0, P frame 242 556 — but its P slice codes all pairs as FRAME, and
   env-traced field-MB counts confirm x264 emits field pairs under CAVLC for
   neither testsrc nor testsrc2 at these settings. NEXT (unblocking): obtain
   a CAVLC P slice that actually contains field-coded pairs — either hunt
   encoder settings/content (strong vertical motion, higher QP so inter
   loses to skip but field wins over frame), or hand-craft a synthetic CAVLC
   MBAFF stream with a known-good oracle. Only then can the field-MC /
   intra-parity paths be validated against ffmpeg and the gate flipped.
   Validation state: 233 lib tests green (incl. both new deterministic
   field-path unit tests), clippy `-D warnings` clean, fmt clean, default
   decoder output byte-identical to session #32f (gate off).

5. **FIX_MV_MBAFF IMPLEMENTED (same session)** (`mv.rs`): `MvStore` now
   records each committed macroblock's `mb_field_decoding_flag`
   (`set_mb_field`) plus a scoped "current field" (`set_cur_field`,
   interior-mutability scratch so neighbour fetches convert without threading
   a parameter through every helper). Neighbour extraction (`cell`/`cell_l1`)
   applies ffmpeg's exact conversion (h264_mvpred_ref.h @n5.1 lines 237–254):
   field-current + frame-neighbour → `refn <<= 1`, `mv_y /= 2` (C truncation);
   frame-current + interlaced-neighbour → `refn >>= 1`, `mv_y *= 2`;
   same-convention neighbours unchanged. Wired via `predict_slice_mvs`, which
   now records flags from `Macroblock::mb_field_flag` before predicting each
   MB. Progressive / PAFF paths are unaffected (flags all false → identity).
   clippy `-D warnings` clean; 231/231 lib tests green; corpus unchanged.
   NEXT (P-frame pixels): MBAFF P reconstruction — route
   `decode_interlaced_mbaff` P slices to a new frame-mode recon that (a) for
   FIELD-coded MBs samples reference frames at doubled row step with parity =
   mb_y&1 (equivalently: reuse `FieldRef::planes()` parity extraction) and
   applies the chroma parity correction `my += 2*((mb_y&1)-(reference-1))`
   (h264_mb_ref.c @n5.1 lines 288-292), and (b) for FRAME-coded pairs keeps
   the existing progressive MC into contiguous halves. Reference sources:
   `h264_mvpred_ref.h`, `h264_mb_ref.c`, `h264_mc_template.c` (fetched),
   `h264dec_ref.h`.

4. NEXT: carry over this session's evidence into the CABAC MBAFF residual
   desync (#32e item 6): the decisive real-C replay harness
   (compile ff_h264_cabac.c's decode_residual/get_cabac_cbf_ctx internals
   with MSVC, replay recorded engine state + payload, diff per-bin contexts).
   Reference sources saved at repo root (`h264_cabac_ref.c`, `cabac_ref.*`,
   `h264_mvpred_ref.h`, `h264_slice_ref.c`, `h264_cavlc_ref.c`).

## SESSION #32e (2026-08-25) — MBAFF I-slice: two real parse bugs fixed; field recon infrastructure landed


> Harnesses: `tests/dbg_mbaff_oracle.rs` (#32d oracle, now actually RUN) +
> a mechanical differ of its `BIN n ...` stream against the crate parser's
> `KINETIX_BINTRACE` `BIN` stream on the real `mbaff_i1` payload
> (`parse_i_slice_cabac` gained an env-gated per-MB `TRC` summary line).

1. **FALSE ALARM resolved — `intra_chroma_pred_mode` ctx weighting is
   `left + top` and the crate was ALREADY CORRECT.** The first BIN-stream
   divergence (BIN 2519, MB(1,1)'s chroma-mode bin, crate ctx 65 vs oracle 66)
   turned out to be a MIS-TRANSCRIPTION in the #32d oracle itself
   (`64 + lc + 2*tc`); FFmpeg's `decode_cabac_mb_chroma_pre_mode`
   (`h264_cabac_ref.c:1394-1399`) uses two plain `ctx++` branches. A "fix"
   following the oracle (`left + 2*top`) regressed the progressive CABAC
   I-frame conformance tests and was REVERTED; both `entropy.rs` and the
   oracle now carry doc comments recording this so it cannot recur.
2. **BUG FIXED — MBAFF commit addressing** (`slice_data/cabac_i.rs`): the
   CABAC I-slice loop computed `grid_idx = pair_row*mb_cols + px`, i.e. the
   *pair's* top slot, instead of the macroblock's own frame-MB address
   `mb_row*mb_cols + px`. Every bottom MB therefore committed its
   neighbour-context state (`MbCabacCtx`: cbp_word / chroma_pred_mode /
   transform_8x8 / is_intra16x16) OVER its top sibling's slot while leaving
   its own slot zeroed; from the second MB onward every context lookup read
   zeros and the engine drifted. Diagnosed with an env-gated `CBPNB` dump in
   `ctx.rs::cabac_cbp_neighbors` showing `left=Some(4)=0x0000` for MB(1,1)
   (should be MB1's word). With the oracle's chroma mis-transcription also
   corrected (item 1), the crate parse and the oracle are back in
   bin-for-bin lockstep over the whole payload.
3. **RESULT:** MB(0,0) reconstructs pixel-exact vs ffmpeg for the first time
   on this clip (`dbg_g5_i1_diffmap` forensics: flat 16/81 pattern matches).
4. **Phase G.4 field-reconstruction infrastructure landed** (uncommitted):
   - `transform.rs`: `FIELD_SCAN_4X4`/`FIELD_SCAN_8X8` transcribed verbatim
     from FFmpeg n5.1 `h264_slice.c` (`field_scan`/`field_scan8x8`, literal
     untransposed form per the `CAVLC_SCAN8X8` precedent);
     `dequant_idct_4x4_scan`/`dequant_idct_8x8_scan` take an explicit scan.
   - `reconstruct.rs`: `reconstruct_luma_at`/`reconstruct_chroma_at` carry a
     vertical geometry (`base_y_px`, `y_step`) + scan tables;
     `reconstruct_mbaff_intra_frame` now decodes DIRECTLY into the interlaced
     frame planes (frame-coded pairs = contiguous halves, field-coded pairs =
     every-other-line placement with doubled intra-prediction stride and the
     field scans), replacing the old progressive-then-rearrange pass.
5. **FIELD RESIDUAL CONTEXTS IMPLEMENTED (this session, after the table
   above):** FFmpeg selects its residual significance/last context *bases* by
   `MB_FIELD(sl)` — field-coded MBs read entirely different ctxIdx ranges
   (`significant_coeff_flag_offset[1] = {277+0,277+15,277+29,277+44,277+47,
   436}`, `last_coeff_flag_offset[1] = {338+0,...,451}`; the 8x8 sig-inc
   indirection also has a field row). The crate only had the frame tables.
   Added `SIG_COEFF_CTX_BASE_FIELD` / `LAST_COEFF_CTX_BASE_FIELD` /
   `SIG_COEFF_CTX_INC_8X8_FIELD` (`cabac_tables.rs`), dual (frame+field)
   context sets in `ResidualCabacContext` (both `new` and `new_pb`;
   coeff_abs contexts are shared — ffmpeg's `coeff_abs_level_m1_offset` has
   no field split), a `field` argument on
   `ResidualCabacContext::decode_block` / `::decode_block_8x8`, and
   `NeighbourCtx::is_field()`; all CABAC P/B/I call sites and the oracle now
   pass the current pair's field flag. Suite re-run green.
6. **REMAINING GAP (decisive next step):** on the deterministic `threads=1`
   corpus, crate parser and corrected oracle still agree bin-for-bin until
   BOTH hit `end_of_slice_flag=1` mid-slice at MB14. Everything above the
   residual walk is now verified against ffmpeg verbatim; the desync is
   therefore inside the SHARED residual internals (sig/last/abs context
   evolution or nnz-derived cbf under MBAFF), where this diff cannot see it
   (circular calibration). ALSO FIXED en route: oracle dqp ignored negative
   deltas (its qp column is wrong, crate's is right); x264 `threads=1`
   pinned in dbg_g5_interlaced because default threading made payloads vary
   run-to-run and poisoned earlier comparisons. NEXT: mechanically compile
   ff_h264_cabac.c's decode_residual/get_cabac_cbf_ctx internals with MSVC
   (the TRANS_IDX_LPS method) and replay the recorded engine state + payload,
   diffing per-bin contexts — this breaks the circularity definitively.
   Also still open: CAVLC MBAFF I-slice parse ("non-intra mb_type in
   I-slice", mbaff_cavlc_ip), MVP row-doubling/halving for inter pairs,
   deblocking edge flags for field MBs.

    REFERENCE MATERIAL saved to repo root for that work:
    `h264_mvpred_ref.h` (libavcodec/h264_mvpred.h @n5.1 - contains
    `fill_decode_neighbors`/`fill_decode_caches`; key subtleties:
    `left_block_options[0..3]` remap left/top cache rows for mixed
    field/frame pairs; unavailable-neighbour nnz is filled with
    `CABAC && !IS_INTRA ? 0 : 64`; `left_cbp` luma nibble is REBUILT from
    `(cbp_table[left_xy[LTOP/LBOT]] >> (left_block[k] & ~1)) & 2` rather than
    copied wholesale) and `h264_slice_ref.c` (field_scan tables).

## SESSION #32 (2026-08-24) — DECISIVE NARROWING of the c_p8x8 P/B gap

> New harness: `tpt-kinetix-h264/tests/dbg_qpel_brute.rs` (qpel SAD brute
> force + variant matrix + pixel forensics). All work compared PRE-deblock on
> both sides (`KINETIX_SKIP_DEBLOCK=1` + `ffmpeg -skip_loop_filter all`).

1. **MC/sub-pel interpolation EXONERATED (decisively, empirically).** The
   prescribed qpel brute force ran: for every diverging MB, exhaustive search
   over ALL quarter-pel MVs (±96 qpel) using OUR OWN
   `motion_comp::interpolate_luma` against the shared bit-exact I reference:
   - OUR pixels reproduce at our parsed MVs with SAD=0 exactly.
   - FFMPEG's pixels match NO MV at all (min SAD 743–2607 per quadrant).
   => Our MC is perfect; ffmpeg's diverging blocks were not produced by ANY
   motion-compensated prediction from the same reference.

2. **ffmpeg's diverging MBs are INTRA-IN-P (mb_type >= 5), not inter.**
   Pixel forensics for MB(1,2): bottom half == pure MC(mv=(0,1)) EXACTLY
   (SAD=0) while the top half shows flat, column-constant bands that match no
   MV — an intra prediction pattern. MB(3,2) row15 == I-reference AT MV=(0,0)
   sample-exact with small noise only near edges (MV=(0,0)+small residual),
   vs our parse P_L0_16x16 mvd=(-1,20) cbp=0. CONSEQUENCE: **the session #31
   "full-slice lockstep" verdict is UNSOUND** — the oracle was calibrated
   against the crate until it agreed (the exact TRANS_IDX_LPS[28] anti-pattern:
   two implementations sharing one source reading agree while both wrong).
   The engine must DRIFT during/around the P_8x8 MB(0,2) so that MB(1,2)'s
   ctx14 bin decodes as intra(1) in ffmpeg but inter(0) in ours.

3. **Variant matrix isolates the trigger: bframes=1 AND partitions=p8x8.**
   Pre-deblock whole-frame SAD vs ffmpeg across encode variants of the same
   clip (`variant_matrix` test):
   - base (bframes=1 + p8x8): FAILS (P and B frames both diverge).
   - bframes=1 + partitions=16x16 / p4x4 / p4x4+p8x8-mix: ALL BIT-EXACT.
   - bframes=0 + p8x8: BIT-EXACT.
   First divergence is always the first coded MB AFTER the P_8x8 macroblock
   (MB(0,2) itself stays pixel-exact). So the bug lives in state written by
   P_8x8 parsing that feeds the NEXT MB's context selection — prime suspect:
   `amvd_sum` neighbour mvd cells / `MbInterCabacCtx::set_partition_l0`
   geometry for P_8x8 sub-partitions (wrong cells -> different ctxIdxInc ->
   different bin counts -> engine drift with element values coincidentally
   still correct through MB(0,2)). Secondary suspects: ref_idx cell flags,
   cbp_word written for P_8x8, nnz grid.
   NOTE: luma residual visit order is NOT the issue — analysis shows cbf/cbf
   contexts see identical visited-neighbour sets under raster and group-by-
   group orders; the vendored ff_h264_cabac.c uses plain raster
   (index=4*i8x8+i4x4) and session #31's "raster regresses" experiment likely
   iterated uncoded groups too.

4. NEXT STEPS (in order):
   a. **Build a mechanical verbatim-C harness (MSVC, same method as the
      TRANS_IDX_LPS[28] fix)** that compiles the ACTUAL vendored
      ff_h264_cabac.c residual internals (lines 1591-1776: sig/last map +
      STORE_BLOCK) plus a real cabac engine copy, feed it the recorded engine
      state before MB(0,2)'s residual (`0x0184/0x0000014f`) + the real 406-byte
      payload, and diff per-bin ctx indices and per-block outputs against our
      walk. This breaks the circular-calibration loop definitively — every
      prior oracle was authored from the same source reading as the crate.
   b. Pin `amvd_sum` for P_8x8 sub-partitions with hand-computed spec tests.
   c. After the fix, re-run dbg_qpel_brute: target `base` variant SAD=0 on
      all 3 frames; then re-run the full conformance matrix + suite.

5. SESSION #32 ADDENDUM (same day) — further decisive facts from the extended
   variant matrix + chroma diff maps:
   - **CAVLC version of the IDENTICAL config (cabac=0, bframes=1,
     partitions=p8x8): BIT-EXACT.** Since CAVLC and CABAC share MV prediction,
     MC, deblocking and reconstruction, this PROVES the bug is in the CABAC
     P-slice parse path alone.
   - Content/resolution sweep: smptebars, rgbtestsrc, and testsrc at 128x96
     with the base config are ALL bit-exact. The trigger is a rare x264
     decision pattern around a CABAC P_8x8 MB, not a systematic config gap.
   - Chroma diff maps (U/V planes, previously never checked): through MB(0,2)
     chroma is EXACT too (its cbp_c=2 DC+AC residual parses correctly);
     divergence begins at MB(1,2) in BOTH luma and chroma wholesale. So the
     drift happens between the END of MB(0,2)'s residual and MB(1,2)'s first
     context-dependent element — i.e., inside MB(0,2)'s residual bin sequence
     tail, its terminate bin handling, or MB(1,2)'s skip-flag context inputs.
     Analytically verified NOT the cause on this payload: amvd sums (all
     zero-context lookups coincide under both ffmpeg's mvd_cache[-1/-8]
     convention and the spec sample rule), ref_idx gating (num_ref_idx=1),
     sub_mb_type order, chroma DC/AC ordering, cbp_table chroma-bit writeback.
   - CAVEAT discovered on the cavlc_base control: x264 makes DIFFERENT rate
     decisions under cabac=0, so cavlc_base passing does NOT prove the shared
     pipeline handles THE POISON PATTERN — it proves it handles ITS OWN
     cabac=0 stream. Still consistent with a CABAC-parse-only bug.
   - Fetched ffmpeg's REAL engine (cabac.c / cabac_functions.h @ n5.1,
     saved as repo-root cabac_ref.c/.h/cabac_funcs.h): ffmpeg uses a scaled
     16-bit-window rearrangement (refill/refill2, range<<(CABAC_BITS+1)
     comparisons, mlps_state+128 packed table); our CabacDecoder implements
     the SPEC algorithm literally (9-bit codIOffset, renorm loop, separate
     TRANS_IDX tables). Hand-audit finds them algebraically equivalent
     (decision/bypass/terminate) — BUT every oracle so far (sessions
     #28/#29/#31) ran BOTH sides through the CRATE engine, so a subtle
     engine-level divergence on real payloads has still never been
     independently excluded. A verbatim-C harness (compile ff_h264_cabac.c
     residual internals + the real cabac engine with MSVC, replay the
     recorded 0x0184/0x0000014f state into the 406-byte payload, diff
     per-bin ctx/bin/output against the crate) remains THE decisive next
     step; it would have caught TRANS_IDX_LPS[28]-class bugs by construction.
   - **BUG FOUND AND FIXED (session #32b finale): the amvd neighbour
     convention.** FFmpeg's literal `DECODE_CABAC_MB_MVD` reads the mvd
     context cells at `mvd_cache[scan8[n]-1]` / `[scan8[n]-8]` — i.e. the
     neighbours of the partition's TOP-LEFT 4x4 block (same top row / same
     left column) — while this crate implemented the spec 8.4.1.2-style
     bottom-row/top-right sample rule in `ctx.rs::amvd_sum`. The two disagree
     whenever a partition follows a neighbour with per-row/per-column
     differing mvds (16x8/8x16/P_8x8): exactly the c_p8x8 trigger. The wrong
     ctx flipped MB(1,2)'s mvd decode (0,1)->different value/bins, drifted
     the engine, and cascaded into intra-in-P misclassification for row 2.
     FIX: `amvd_sum` now reads the top-left-adjacent cells;
     `ref_idx_gt0_neighbors` updated to the same scan8-adjacent convention
     (`decode_cabac_mb_ref` uses ref_cache[scan8[n]-1/-8] likewise).
     RESULT: dbg_qpel_brute variant_matrix ALL 10 VARIANTS BIT-EXACT vs
     ffmpeg pixels including the previously-failing base (bframes=1+p8x8)
     configuration; qpel_brute per-MB diffs ALL ZERO; cabac I/P/B +
     conformance_matrix + cavlc suites all green; lib tests 231/231.
   - Session #31's oracle (`p_slice_full_walk_lockstep_vs_ffmpeg_transcription_c_p8x8`)
     updated: its ob_amvd transcription carried the same wrong convention
     (now fixed to match ffmpeg); its post-MB9 residual-walk internals still
   - **PHASE G.3 EXTENDED TO P/B CABAC SLICES (same day):** `parse_p_slice_cabac`
     / `parse_b_slice_cabac` now take `mb_aff` + `field_pic_flag` and implement
   - **PHASE G.4 PARTIAL (same day): `NeighbourCtx` threaded through the
     entire P/B CABAC parse stack.** `parse_p_macroblock_cabac`,
     `parse_b_macroblock_cabac`, `parse_intra_mb_cabac_pb`,
     `decode_inter_residual_cabac`, and `decode_inter_cbp_cabac` now take a
     per-MB `nctx` (built from the pair's field flag + the frame's
     `field_flags` grid) instead of computing plain-raster left/top indices;
     all internal cbf/chroma/cbp/MPM/amvd/ref_idx neighbour lookups resolve
     through §6.4.10.1 for mixed field/frame pairs. Frame-only streams are
     unaffected (`left_top` degenerates to the raster formula). Full suite
     re-validated green (231 lib + conformance_matrix + cabac 6 + qpel 2).
     STILL OPEN for full MBAFF decode: MVP row-doubling/halving
     (`MAP_F2F`-equivalent field MV scaling in mv.rs), field-aware intra
     prediction, PAFF/MBAFF corpus clips (G.5), deblocking edge flags for
     field MBs.
   - **MBAFF ADDRESSING BUG FIXED (session #32c):** the CABAC I-slice loop
     interpreted macroblock addresses as plain raster, but MBAFF addresses
     enumerate each PAIR as (top, bottom) before advancing horizontally
     (addr 2k/2k+1 = pair k at frame-MB col `pair%cols`, MB rows
     `2*(pair/cols)`/`+1` — spec §6.4.2/§7.4.4). `parse_i_slice_cabac` now
     derives (mb_x, mb_y, grid_idx) pair-aware when `mbaff_frame`, stores all
     per-MB state by grid address, and emits macroblocks in frame-grid order.
     Progressive streams unaffected (degenerate branch). Diagnostic:
     `tests/dbg_g5_i1_diffmap.rs`.
   - **G.5 FINDING:** after the addressing fix, x264 --interlaced I-frames
     STILL diverge wholesale because x264 chooses FIELD coding for pairs:
     field MBs need (a) field-scan / field_scan8x8 zigzag tables,
   - **G.5 I-FRAME DIAGNOSTIC NARROWING (session #32c):** for the mbaff_i1
     clip, `parse_i_slice_cabac` SUCCEEDS on the MBAFF payload (all 16 MBs,
     no end_of_slice desync; interlaced.rs previously swallowed parse errors
     silently — it now logs them via eprintln). The wholesale pixel
     divergence is therefore entirely in `reconstruct_mbaff_intra_frame` /
     the intra prediction path under MBAFF: our MB(0,0) outputs DC-128-grey +
     noise where ffmpeg decodes real content (testsrc black bg Y=16 + square
     edges). Prime suspects: intra prediction neighbour availability under
     pair addressing, and High-profile Intra_8x8 handling in
     reconstruct_mbaff_intra_frame. Diagnostic:
     `tests/dbg_g5_i1_diffmap.rs` (pair diff map + chroma + MB(0,0)
     forensics).
   - **#32c MB-LEVEL PARSE DATA (mbaff_i1):** first four MBs parse to
     plausible values — Intra4x4, cbp=0x2f, qp=24, MIXED transform flags
     (MB0/1 t8=false, MB2/3 t8=true) — yet luma AND chroma diverge wholesale
     (chroma-U 1016/1024 samples). Wholesale luma+chroma error with a
     plausible-looking parse points at a DEQUANT-level cause for this stream
     rather than prediction: prime suspect is SPS
     `seq_scaling_matrix_present_flag` (profile_idc=100 — does x264 write
     explicit scaling lists here, and does our SPS parse + dequant apply
     them?). Secondary: High-profile Intra_8x8 prediction under pair
     addressing. NEXT: dump `sps.scaling` presence for this clip; compare
     dequant tables vs ffmpeg; then field-coding support (field scans,
     field intra pred, field placement).
   - **#32c SPS VERIFIED:** the interlaced clip's SPS parses correctly
     (`mbaaf=true frame_mbs_only=false` via our own SeqParameterSet::parse),
   - **#32c EXPERIMENT RESULT (KINETIX_NO_FIELD_BINS probe):** skipping the
     field-flag reads changes nothing — divergence remains wholesale either
     way. Combined with clean end_of_slice termination across all 16 MBs,
     the parse failure mode is SELF-CONSISTENT-BUT-WRONG (same signature as
     the amvd bug): some context-selection or interpretation detail early in
     the slice differs from ffmpeg while staying internally aligned.
   - **DECISIVE NEXT STEP:** extend `dbg_engine_diff.rs`'s proven
     FfEngine into an MBAFF I-slice oracle walk — mechanically transcribe
     ffmpeg's I-slice path (decode_cabac_field_decoding_flag @ ctx70..72 +
     decode_cabac_intra_mb_type(ctx_base=3, intra_slice=1) + Intra_8x8/4x4
     pred-mode bins + chroma_pre_mode + cbp + dqp + residual walk with
     nnz-cache border rules) and diff per-element vs the crate ON THE REAL
     mbaff_i1 PAYLOAD. This technique found TRANS_IDX_LPS[28], ctx266, AND
     the amvd convention; it is the reliable instrument for this class.

     confirming the MBAFF signalling path end-to-end. The dequant-level
     suspicion (explicit scaling lists) and the reconstruction-stage field
     support (field scans / field intra pred / field placement for
     field-coded pairs) remain the two open threads for full interlaced
     pixel-exactness.



     (b) field intra prediction (half-height neighbour sampling),
     (c) interleaved row placement for inter pairs. This is precisely the
     remaining G.4 work; syntax layer is complete and correct.

   - **PHASE G.5 BASELINE ESTABLISHED:** new corpus harness
     `tpt-kinetix-h264/tests/dbg_g5_interlaced.rs` encodes genuinely
     interlaced x264 streams (`interlaced=1:tff=1`, i.e. MBAFF) across
     4 configurations (CABAC I-only / IP / IBP / CAVLC IP at 64x64) and
     measures per-frame SAD vs ffmpeg (`-skip_loop_filter all`).
     RESULT (post-G.3/G.4-partial): ALL configurations now PARSE end-to-end
     with the correct number of emitted frames (MBAFF I via
     reconstruct_mbaff_intra_frame; MBAFF P and B CABAC via the new pair-aware
     loops; CAVLC likewise) — no slice-data desync anywhere.
     RECONSTRUCTION is still wholesale-wrong on interlaced content
     (~250k-300k luma SAD/frame): expected, since field-coded macroblock
     pairs are reconstructed as progressive (no field placement for inter
     MBs, no MVP row-doubling/halving, no field-scan tables). These numbers
     are the G.4 completion baseline. NOTE: x264 --interlaced emits High
     profile (profile_idc=100) with transform_8x8 allowed — the decoder
     handles it on these clips.


     FFmpeg's exact MBAFF pairing (`ff_h264_decode_mb_cabac` lines 1932-1964):
     bottom-of-pair MB whose top was skipped reuses `next_mb_skipped` instead
     of reading a bin; a skipped TOP MB pre-reads the bottom's skip flag
     (ctx from left=(x-1,y+1), top=this-skip-MB) and decodes the pair's
     `mb_field_decoding_flag` (ctxIdx 70+left+top) when the bottom is coded;
     a coded TOP MB decodes the field flag directly. Flags stored on
     `Macroblock.mb_field_flag` / `MbCabacCtx.mb_field_flag` / per-frame-MB
     `field_flags` grid (G.4 wiring ready). Frame-only streams unchanged
     (`mbaff_frame == false` skips every new branch — full suite re-run green,
     231 lib tests + all conformance cells). Call sites updated: decoder
     mod.rs P/B, interlaced.rs PAFF-P (passes mb_adaptive flag +
     header.field_pic_flag), entropy.rs lockstep test, dbg examples/tests.

     diverge from the crate on this payload, so MB9-11 are pinned against
     values validated BIT-EXACT against ffmpeg's reconstructed pixels
     instead (documented in the test; oracle kept for MB0-8 differentials).

   - **DONE (same day) — engine-level differential BUILT and PASSED:**
     `tpt-kinetix-h264/tests/dbg_engine_diff.rs` mechanically ports ffmpeg's
     REAL engine arithmetic (cabac_functions.h @ n5.1: refill/refill2/
     get_cabac_inline/bypass/terminate) and parses `ff_h264_cabac_tables`
     OUT OF THE VENDORED SOURCE (`cabac_ref.c`, kept at repo root) at test
     runtime — zero transcription risk. Lockstep over random payloads with a
     shared 1024-context model: ALL payloads run in full bin-for-bin
     lockstep (terminate bin ends each payload, as expected). **The crate
     CABAC engine is EXONERATED definitively.** Correct ff packed-state
     mapping empirically confirmed as `2*pStateIdx + valMPS`
     (single_step_probe: 3156/3156 agreement; other mappings fail).
   - **Visit-order question RESOLVED.** A reorder experiment (scan8-style →
     raster-within-group placement in all three CABAC cat-2 walks) regressed
     every CABAC variant and was REVERTED. Root cause of the long-standing
     "vendored C ambiguity": ffmpeg's `index = 4*i8x8+i4x4` is consumed
     THROUGH its scan8[] table — scan8[0..3] = spatial raster blocks
     {0,1,4,5}, scan8[4..7] = {2,3,6,7} — IDENTICAL to this crate's
     raster_of_8x8_sub visit/placement. No conflict, no ordering bug;
     session #31's "plain raster regresses" is explained (raster genuinely
     changes placement and breaks decode).
   - NET RESULT of #32/#32b: engine EXONERATED (proven), MC EXONERATED
     (proven), element parse trees exonerated (#28/#29, now sound given the
     engine proof), visit order RESOLVED, trigger isolated to bframes+p8x8
     CABAC with first divergence one MB after a P_8x8. Remaining suspects:
     inter-MB glue around P_8x8 state (mvd cache cells / nnz cell semantics /
     cbp_word writeback) or cat-3/cat-4 chroma flow after fully-coded luma.
     NEXT: extend the proven ff-engine into a full P-slice MB loop walk and
     diff per-element against the crate ON THE REAL PAYLOAD.


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
- [x] **NEW (2026-08-22): `conformance_matrix` cabac_p / cabac_b cells fail**
      (max_abs_diff≈127, full-frame scaffold → a decode *error* fallback, both
      deblock variants). Verified pre-existing at origin/master (`96a4db9`) —
      not caused by the 2026-08-22 MPM/CAVLC work.
      **RESOLVED — verified 2026-08-27 (session #32o).** Both cells now decode
      bit-exact (`max_abs_diff=0`); the desync was cleared by the #32b amvd
      fix + #32j inter-MB transform_size_8x8_flag fix. See session #32o notes.
      Original root-cause diary retained below for reference.
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


      - **2026-08-23 session #12 — FFmpeg-exact P-slice oracle built; divergence
        narrowed to the MB(1,0) intra-in-P residual region.** One real (latent)
        fix landed plus the queued oracle harness:
        1. **Shared ctxIdx-17/32 context variable** (`entropy.rs`, `ctx.rs`,
           `cabac_b.rs`): FFmpeg adapts ONE physical `cabac_state[17]` (P) /
           `[32]` (B) for BOTH the mb_type partition/gate bit AND the
           intra-in-P/B suffix's bin 0; our split structs held two
           independently-adapting copies. Added `shared_ctx`/`set_shared_ctx`
           accessors + `PbCabacSliceContexts::sync_shared_mb_type_ctx_*` and
           call them after every decode of either. Latent bug (this clip never
           takes the 16x8/8x16 branch before failing, so it is not THE cell
           blocker), regression-tested in `entropy.rs`
           (`shared_ctx17/32_is_initialised_identically_*`).
        2. **New oracle harness** `tpt-kinetix-h264/tests/dbg_p_oracle_replay.rs`:
           FFmpeg-convention engine (`ff_init_cabac_decoder` + decision/
           terminate/bypass, validated bin-for-bin against the crate's own
           `CabacDecoder`) + ffmpeg-exact P-slice element walk over the exact
           payload bytes the crate parser read (hardcoded from the parser's
           own "P-CABAC bytes" trace). Prints per-bin context indices and
           engine states comparable with the parser trace.
        3. Oracle findings on the failing `b_boxmv` IBP P slice: MB0 skip=1,
           eos=0; MB1 skip=0 -> intra-in-P suffix -> I_16x16 variant 3,
           chroma DC, qp delta=0 -- ALL bins match the crate parse exactly.
           First mismatching element: the luma-DC `coded_block_flag` read
           (cat 0, ctxIdx 87, fresh state 31 in both): oracle bin=1 (DC block
           NONZERO), crate bin=0 (no block). Since engines/contexts are
           identical up to that point, the engine state entering the read must
           differ -- i.e. an uninstrumented extra/missing bin or a context-
           state difference somewhere between the chroma_pred/qp_delta reads
           and the DC cbf (candidates: crate reads an element the oracle walk
           does not model, or vice versa). NOTE: an earlier oracle run that
           started qpdelta at ctx62 produced garbage downstream -- ffmpeg's
           first dqp bin is at `60 + (last_qscale_diff != 0)` (crate already
           correct); keep this in mind when extending the replay past MB2.
        NEXT STEP: add per-element engine-state prints inside the crate's
        `parse_intra_mb_cabac_pb` (suffix/chroma/qp/dcbf boundaries) and diff
        against the oracle's states to expose the extra/missing bin; then
        extend the oracle past the DC-cbf read (significant-map transcription)
        to verify the full I16x16-in-P residual.

      - **2026-08-23 session #13 - PHANTOM BUG EXPOSED: the session-#12 oracle's
        ffmpeg-convention ENGINE was mis-reading the payload; the crate parser
        was right all along. Real remaining cabac_b gap re-localised to B_8x8 +
        direct-mode MBs + a +/-1 residue.**
        1. **Bin-level tracing added** (entropy.rs, env-gated KINETIX_BINTRACE=1):
           CabacContext now carries its global spec ctx_id (set by init_ctx /
           init_pb_ctx; 0xFFFF when built via CabacContext::init directly), and
           decode_decision / decode_bypass / decode_terminate emit one
           "BIN n D ctx=... st=... mps=... bin=..." line per bin under the flag.
           Zero cost when unset.
        2. **New harnesses**: tests/dbg_bintrace_replay.rs replays the exact
           hardcoded P-slice payload through the crate's own parse_p_slice_cabac;
           tests/dbg_p_oracle_replay.rs was rewritten so its ffmpeg element walk
           runs on THE CRATE'S OWN CabacDecoder + a flat 1024-entry context table
           (engine equivalence by construction). Result: the walk reproduces the
           crate parse bin-for-bin, including dc.cbf bin=0, matching the crate.
           The old hand-rolled Eng seeded low with 18 bits (bytes 0-1 plus byte
           2's top TWO bits via &0xC0) but resumed its bit reader at pos = 24,
           silently dropping byte 2's low 6 bits and desyncing every element
           after ~6 renormalisation shifts. The long-pursued "intra-in-P desync"
           (sessions #11/#12) never existed.
        3. **P frames confirmed bit-exact even in IBP streams**: with correct
           per-NAL pairing (x264 file order is IDR, P, B; ffprobe lists frames in
           DISPLAY order I,B,P which misled earlier pairing), every variant's P
           picture matches ffmpeg exactly (decoded[1] ref2:max=0 across all
           dbg_cabac_b variants).
        4. **dbg_cabac_b variant artefacts identified**: b_swap and b_forcel1
           clips contain NO B frames (ffprobe: I,P,P and I,I,P) -- x264 declined
           to place a B -- so their uniform diffs were harness pairing artefacts,
           not decoder bugs. Do not chase them.
        5. **Real remaining cabac_b cell gap** (B picture vs display-ref):
           - b_min (direct=none:partitions=none): max_abs_diff=1 over 21 samples
             -- a +/-1 rounding somewhere in bi-pred/MC; nearly closed.
           - b_nodirect: single failing MB(3,2) of type BB8x8 (partitioned B_8x8
             with bi-pred sub-partitions) at max=238 -- sub_mb_type /
             per-sub-partition MV path is the prime suspect (matches the earlier
             "audit B_8x8" note from sessions #5-#8).
           - b_default/b_temporal (direct spatial/temporal ON): bottom MB row
             wrong (~80-126) -- direct-mode derivation at frame edges or
             colocated-grid handling for the last row.
           - b_boxmv: MB(1,1) max=235 -- likely same B_8x8/bi-pred family.
        6. Debug instrumentation left in tree (matches existing style): BRECON
           per-MB type print in reconstruct_b_frame, "B-CABAC bytes[...]"
           payload dump in the decoder's B path (mirrors the P-path dump), CBF
           state prints in entropy.rs.
        NEXT STEPS (in order): (1) root-cause b_nodirect MB(3,2) BB8x8: dump
           sub_mb_type bins via KINETIX_BINTRACE against an extended crate-engine
           ffmpeg walk for B_8x8 (sub_mb_type contexts, per-sub-block
           ref_idx/mvd); (2) chase b_default/b_temporal bottom-row direct-mode
           errors (colocated MV grid for last row / boundary availability in
           spatial direct); (3) squeeze the b_min +/-1 residue; (4) only then
           revisit Phase G (MBAFF P/B parsing) and H.

      - **2026-08-23 session #14 - TWO MORE REAL BUGS FIXED; b_default B frame now
        BIT-EXACT; remaining failures narrowed to BBi16x16.**
        1. **B_Direct_16x16 dropped its CBP/qp_delta/residual bins**
           (cabac_b.rs): the old code returned early for b_type_raw==0 with the
           comment "B_Direct: no CBP/residual syntax" -- wrong per spec
           7.3.4/7.3.5.1: coded_block_pattern is signalled for ALL inter MBs,
           and a direct MB with cbp != 0 carries residuals. Every direct MB with
           cbp != 0 desynced all following MBs (the clean-until-bottom-row error
           pattern). Fix: route B_Direct through the generic CBP/residual tail.
           Result: b_default (direct=spatial, default partitions) B frame is now
           max_abs_diff=0 vs ffmpeg.
        2. **B sub_mb_type info tables were mis-transcribed** (mv.rs):
           B_SUB_MB_PARTS / B_SUB_MB_DIR / b8x8_sub_rect used an interleaved
           L0/L1/Bi order. Correct spec Table 9-16 layout:
           0=Direct, 1..4=L0(1/2/2/4 parts), 5..8=L1(1/2/2/4), 9..12=Bi(1/2/2/4).
           Fixed all three (b8x8_sub_dims in cabac_b.rs shares the fix path).
           Result: b_nodirect MB(3,2) error halved (238 -> 115).
        3. New isolation variants in dbg_cabac_b.rs: c_i16 / c_p8x8 / c_p4x4.
           Findings: i16x16-only and p4x4 clips sit at max=1/n=21 (a +/-1
           residue); c_p8x8 fails via its two BBi16x16 MBs -- so the REMAINING
           cabac_b gap is concentrated in the bi-pred 16x16 path (L1 predictor
           or bi-combination), not in B_8x8 sub-partitions per se.
        NEXT STEPS: (1) dump predicted-vs-ffmpeg MVs for a failing BBi16x16 MB
           (ffmpeg side: export_mvs side data) to decide whether predict_mv_l1
           availability/ref-matching or the bi-average rounding is at fault;
           (2) chase the +/-1 residue on b_min (21 samples, likely MC rounding);
           (3) re-run conformance_matrix; then Phase G/H as before.
        4. **RESULT: `h264_conformance_matrix` now PASSES** -- both cabac_b
           cells report bit-exact vs ffmpeg (the matrix clip is exactly the
           b_default configuration fixed by item 1). Full crate suite green
           (226 lib tests + all conformance tests, 0 failures).
           HONESTY CAVEAT: pixel_exact must STAY false -- the isolation harness
           still shows real gaps on other B configurations (BBi16x16-heavy
           content via c_p8x8, the b_min +/-1 residue, b_nodirect BB8x8 at
           max=115). The matrix clip simply does not exercise them. Next
           session should add a matrix cell (or a new gated test) that DOES
           exercise BBi16x16/B_8x8 before any capability flip is considered.

      - **2026-08-23 session #15 - BBi16x16 hypothesis cycle: L1-separate-context
        experiment DISPROVEN and reverted; shared mvd contexts confirmed.**
        1. Hypothesised (from spec Table 9-44 memory) that mvd_l1 uses separate
           contexts (L1-x 47 / L1-y 54). Added MVD_L1 contexts + rewired all B
           L1 mvd call sites: EVERY bi-pred clip regressed badly. REVERTED.
           Conclusion (empirical, matches the pre-existing ctx.rs comment):
           FFmpeg/spec share ONE pair of mvd context variables per component
           across both lists (ctxbase 40 x / 47 y, no list parameter).
        2. During the experiment a scripted bulk edit briefly corrupted the 20
           mvd call sites in cabac_b.rs; all were repaired deterministically
           against ground truth (x comp=0, y comp=1; list per arm) and the file
           re-formatted. Final state verified equal to the best-known config:
           b_default bit-exact; b_min/c_i16/c_p4x4/b_temporal at max=1;
           c_p8x8 bottom row 60-75; b_nodirect MB(3,2) max=115; b_boxmv
           MB(1,1) max=235.
        NEXT STEPS unchanged: dump ffmpeg MVs for a failing BBi16x16 MB via
           export_mvs to decide between predict_mv_l1 availability rules vs
           bi-average rounding; chase the +/-1 residue.

      - **2026-08-23 session #16 - DECISIVE LOCALISATION: the remaining B gap is
        CABAC-specific, not in shared MV prediction or MC.** Added CAVLC control
        variants to dbg_cabac_b.rs: cavlc_p8x8 (same config as the failing
        c_p8x8) decodes its B frame at max=1/n=25 through our CAVLC path, and
        cavlc_i16 likewise. Since CAVLC and CABAC share predict_b_slice_mvs /
        reconstruct_b_frame / motion_comp, the residual BBi16x16/BB8x8 failures
        under CABAC must originate in CABAC Bi-MB bin parsing: MbTypeBCabacContext
        tree bins, ref_idx neighbour-ctx derivation (ref_idx_gt0_neighbors with
        direct/L1-only neighbours), or mvd amvd sums -- NOT in mv.rs/reconstruct.rs.
        NEXT STEP: BINTRACE the failing MB(3,2)/MB(1,2) of c_p8x8 and replay an
        ffmpeg-element-walk (crate engine) for exactly those MBs to expose the
        first divergent bin; then extend the walk to ref_idx/mvd as needed.

      - **2026-08-23 session #17 - H1 mb_type-tree experiment DISPROVEN; b_boxmv
        identified as NON-DETERMINISTIC (nullsrc background varies per encode);
        original B tree restored as best-known.**
        1. Instrumented BBi/BL1 MV derivation prints (cabac_b.rs arm 3, mv.rs
           BL116x16 + BBi16x16). b_boxmv results vary BETWEEN RUNS because its
           nullsrc background is random per ffmpeg invocation -- all prior
           cross-run comparisons on that variant are unreliable. Use c_p8x8 /
           testsrc for analysis.
        2. c_p8x8 failing MBs parse as B_Bi_16x16 with mvd=(0,0) both lists,
           cbp=0x2f, qp=22 -- self-consistent but pixels differ from ffmpeg by
           ~20-30 with a smooth shift-like pattern.
        3. H1 tree variant (ctxIdx-31 single bin selecting Bi after [1,1])
           regressed ALL clips (c_p8x8 row2 60/71/75 -> 128/128/151). REVERTED;
           the 4-bin extension reading (ctx[4],ctx[5],ctx[5],ctx[5] ->
           bits<8 => type bits+3) is confirmed better.
        4. CAUTION recorded: x264 mode decisions DIFFER between cabac=0 and
           cabac=1 encodes of identical input (rate costs differ), so cavlc_p8x8
           passing does NOT prove c_p8x8 has identical modes -- it only bounds
           the shared pipeline. The remaining gap needs an authoritative
           re-check of MbTypeBCabacContext against real ffmpeg source; the
           fetch tooling truncates h264_cabac.c at 50k chars and the function
           sits past that point -- use a ranged fetch or vendored copy next time.
        State: conformance_matrix GREEN; suite green; remaining known gaps:
           c_p8x8 row2 (60-75), b_nodirect MB(3,2) (115), b_min +/-1 (21 samples).

      - **2026-08-23 session #18 - V-B tree experiment DISPROVEN; original
        MbTypeBCabacContext reading re-confirmed as best-known.** Tested the
        all-bins-at-ctxIdx-32 extension variant: regressed c_p8x8 row2
        (60/71/75 -> 135/104/162) and every other bi-pred clip. Reverted with a
        code comment. Two independent structural variants (H1, V-B) now both
        disproven -- the mb_type TREE is very likely correct, and the remaining
        cabac_b gap probably lies in what happens AROUND Bi MBs: either the
        ref_idx/mvd bins for the specific neighbour states of those MBs, or an
        interplay between cbp/residual and B-slice reconstruction ordering.
        Also noted: pixel evidence on c_p8x8 MB(3,2) shows ours vs ffmpeg
        differing by a smooth ~4px horizontal shift-like pattern -- consistent
        with ONE list using an MV off by ~16 quarter-pel, i.e. possibly a wrong
        mvd VALUE (not structure) for exactly these MBs, or a predictor
        difference from neighbour-state divergence earlier in row 2.
        NEXT STEP: obtain h264_cabac.c content past char 50k (vendored copy,
        ranged fetch, or GitHub .patch of an old mb_type-touching commit) and
        diff MbTypeBCabacContext + ref_idx/mvd call order against it line by
        line; alternatively hand-verify against the ITU spec Table 9-34/9-35.

      - **2026-08-23 session #19 - AUTHORITATIVE SOURCE OBTAINED: ff_h264_cabac.c
        was already vendored at the repo root (ff_h264_cabac.c /
        ff_cabac_functions.h, untracked). Line-by-line comparison DONE:**
        1. MbTypeBCabacContext tree is VERBATIM-CORRECT vs ffmpeg lines
           1977-1997 ([27+ctx], [27+3], [27+4]<<3|[27+5]<<2|[27+5]<<1|[27+5],
           bits<8=>+3, 13=intra, 14=>11, 15=>22, else <<1 +bin -4).
        2. MVD decoding CONFIRMED: ctxbase = (l==0)?40:47 -- component-based,
           SHARED across lists (session #15 conclusion re-confirmed by source);
           amvd threshold FFMIN(((amvd+28)*17)>>9,2) == crate <3/<33;
           sign via get_cabac_bypass_sign(&cabac, -mvd) == crate.
        3. Element ORDER for 16x16-type inter MBs CONFIRMED:
           ref(list0,list1) -> mvd(list0,list1) -> cbp(luma,chroma) ->
           [transform_size_8x8 if dct8x8_allowed && cbp&15 && !intra] ->
           mb_qp_delta -> residual.
        CONSEQUENCE: the CABAC parse of c_p8x8 MB(1,2)/MB(3,2) (Bi[0,0]) is
           CORRECT per ffmpeg semantics; the remaining pixel diffs must come
           from either (a) nz/cbf NEIGHBOUR-STATE tracking divergence inside the
           B inter-residual path (decode_inter_residual_cabac grids), or (b)
           reconstruction-side handling unique to Bi blocks -- despite
           cavlc_p8x8 passing, since x264 may pick different MVs there.
        NEXT STEP: instrument nz_grid/cbf-neighbour values for the row-2 MBs of
           c_p8x8 and verify against a hand ffmpeg-walk of the coded_block_flag
           reads; alternatively diff our parsed coefficients per block against
           implied coefficients (ref_pixels - pred) using ffmpeg reference YUV.

      - **2026-08-23 session #20 - DETERMINISTIC REPRO ACHIEVED; parse verified
        end-to-end against vendored source; suspicion narrowed to neighbour-state
        CONTEXT INPUTS.**
        1. dbg_cabac_b.rs now encodes with -threads:v 1 /
           threads=1:sliced-threads=0:non-deterministic=0. x264 default
           multithreading made streams vary BETWEEN RUNS -- the root cause of
           every earlier cross-run inconsistency. c_p8x8 now reproduces its
           failure EXACTLY (row2: 3,60,71,75) on every run.
        2. Verified against vendored ff_h264_cabac.c: mvd unary loop bounds,
           ctx advance, EGk bypass suffix, sign bit -- all identical to crate.
           dqp mapping (val&1 => +(val+1)>>1 else -((val+1)>>1)) identical.
        3. KEY EVIDENCE: under our parsed mode Bi[0,0], the implied residual
           (ref - avg(L0,L1)) for failing MB(3,2) is LARGE and structured
           (-38..+77 with horizontal gradient) -- implausible as a quantized
           residual at qp=22. Therefore x264 wrote a DIFFERENT mode/MVs than we
           decoded, even though the tree logic is verbatim-correct.
        CONCLUSION: the divergence is almost certainly in the CONTEXT INPUTS
           derived from neighbouring-macroblock state that feed the tree:
           non_direct_neighbours (IS_DIRECT of left/top incl. BSkip handling)
           and/or ref_idx_gt0 / cbf-neighbour grids. A single off-by-one ctx
           selection early in the slice would re-route bins into plausible-but-
           wrong elements without tripping end_of_slice checks.
        NEXT STEP: print per-MB non_direct_neighbours + left/top direct flags
           for the whole B slice and audit the BSkip/Direct classification
           against ffmpeg fill_decode_neighbors semantics; then BINTRACE the
           exact bins of MB(0,2)/MB(1,2) under corrected contexts.

      - **2026-08-23 session #21 - SLICE-QP HYPOTHESIS ELIMINATED; new debug
        infrastructure in place.** Added KINETIX_DUMP_B_PATH full-payload dump
        (decoder/mod.rs B path) + tests/dbg_b_qp_sweep.rs which regenerates the
        deterministic c_p8x8 clip, dumps the exact B CABAC payload + header
        params (qp=24 idc=0 nl0=1 nl1=1 t8=false, 274 bytes), and sweeps all
        52 qp values through parse_b_slice_cabac. RESULT: qp=24 is the UNIQUE
        value reproducing bi16=2 (the two B_Bi_16x16 MBs); every other qp gives
        bi16=0 (and extreme qps trip eos). Slice QP and context init are
        CORRECT. Also gated the leftover CBF/mvd/eprintln debug spam behind
        bin_trace_enabled() so sweeps and traces run fast.
        STATUS OF ELIMINATED HYPOTHESES FOR THE cabac_b ROW-2 GAP:
        slice qp X, context init X, mb_type tree X, mvd contexts/bases/sign X,
        element order X, dqp mapping X, neighbour ndc inputs X, shared MV
        prediction/MC X (cavlc control). REMAINING candidates: (a) our decoded
        RESIDUAL COEFFICIENT VALUES differ from x264s despite correct structure
        (would require an engine-state divergence entering row 2 -- but rows
        0-1 are clean...), or (b) something in reconstruct_b_inter_lumas
        Bi combination for exactly these blocks. Suggested next: dump our
        dequantised residual per 4x4 for MB(1,2)/MB(3,2) and compare against
        implied residual (ref - avg(L0,L1)) -- if they disagree beyond clipping,
        the coefficients are misdecoded; if they agree, reconstruction is at
        fault.

      - **2026-08-23 session #21 addendum - ERROR PROPAGATION PATTERN identified.**
        Row 2 diffs grow monotonically along the scan (3, 60, 71, 75) and row-1
        MBs carry small nonzero diffs (0,2,0,1). Since cbf context selection
        reads the LEFT and TOP neighbours coefficient counts (nz_grid), a single
        subtly-wrong coefficient or nz value early in the scan poisons the cbf
        context of every subsequent MB to its right/below -- producing exactly
        this growth pattern WITHOUT tripping end_of_slice (bin counts stay
        similar because only ctx INDICES shift, not the element structure).
        Working hypothesis for the final cabac_b gap: a +/-1-class error in an
        early row-1/row-2 MBs coefficient decode or nz bookkeeping that then
        propagates via cbf contexts. The +/-1 residue on b_min (21 samples) is
        probably THE primary bug, not a separate one.
        NEXT STEP: locate the FIRST sample-level divergence in scan order (not
        the largest), dump that MBs parsed coefficients + nz, and compare its
        cbf/significant-map ctx selection against a hand ffmpeg-walk.

      - **2026-08-23 session #22 - FIRST-DIVERGENCE MAP + instrumentation
        consolidated.** Added a per-MB scan-order divergence report and a
        small-diff sample dumper to dbg_cabac_b.rs; all debug prints (CBF, mvd,
        BL1MV, BBiMV) are now gated behind KINETIX_BINTRACE=1.
        FIRST-DIVERGENCE MAP for c_p8x8 (deterministic):
          MB(1,1) BL116x16: n=4, max=2 -- isolated samples at mb-local
            (x=5,y=14),(x=14,y=14),(x=5,y=15),(x=14,y=15), deltas +1/+2
          MB(3,1) BL016x16: n=2, max=1 -- mb-local x=9, y in {1,15}, delta -1
          MB(0,2) BL116x16: n=8, max=3 -- mb-local x in {14,15}, y 4..10
          MB(1,2)/(2,2)/(3,2): n=206/183/220, max 60/71/75 (Bi + L1)
        READING: the earliest errors are ISOLATED single samples with +/-1..2
          in otherwise-correct MBs -- the signature of a tiny coefficient
          difference (one level off by a small amount in one 4x4 block) rather
          than an MV or mode error; the later big row-2 errors grow out of the
          poisoned cbf-context chain these create. NOTE the row-1 MBs are all
          L1/L0 16x16 whose own pixels are ~correct -- so the primary defect is
          likely a single coefficient (or its dequant rounding) in MB(1,1)
          blk13-ish region, OR a subtle nz bookkeeping difference that shifts a
          later cbf ctx.
        NEXT STEP (unchanged in essence): hand-walk MB(1,1)s residual with the
          ffmpeg element order (all machinery now env-gated and fast) and check
          each cbf/significant/level decision; the first differing decision is
          the bug.

      - **2026-08-23 session #23 - RESIDUAL-SOURCE DISCRIMINATOR results (the
        strongest clues yet).** Added per-MB SAD comparison of (output-pred) vs
        (ref-pred) for candidates I / P / bi-avg:
        - MB(1,1) BL1[0,0]: OUR output == P frame EXACTLY (f-sad=0) where
          ffmpeg differs by r-sad=6 -> x264 used a small NONZERO mvd (likely
          fractional/+-1-2) that we decoded as 0. Same pattern for MB(3,1)
          BL0[0,0] vs I (f-sad=0, r-sad=2).
        - MB(0,2), MB(1,2): f-sad ~ r-sad (within 7-100) -> small residual
          coefficient differences.
        - MB(2,2): f-sad == r-sad EXACTLY for ALL THREE candidates (2953) while
          n=183 samples differ -> our residual there has the right MAGNITUDES
          but flipped signs and/or permuted placement. NOT a random decode
          error; systematic.
        INTERPRETATION: at least two distinct defects: (a) small mvd values
        decoded as zero somewhere (single-bin reads), and (b) a residual
        sign/arrangement issue in specific inter MBs. Candidate unifying cause:
        coeff_abs_level SIGN bypass polarity or the level->block mapping for
        inter MBs under specific significant-map shapes -- but P-slice
        bit-exactness constrains any theory hard.
        NEXT STEP: for MB(2,2), dump our per-4x4-block coefficient grids and
        compare against the implied residual pattern (r - P) block by block;
        check whether the mismatch is a sign flip, a scan-order permutation, or
        a block-placement offset.

      - **2026-08-23 session #24 - MB-level coefficient data extracted.** Parsed
        coefficient grids for all c_p8x8 B-slice MBs captured via
        KINETIX_BINTRACE (grouped by CODED skip_flag markers). MB(1,2)
        Bi[0,0]: blk0 cbf=false, blk1=[0,1,3], blk2=[1,0,-2,-6,0,0,11..],
        blk7 contains -15 at pos 11 -- real low-frequency residual, consistent
        with a genuine Bi[0,0] coding decision on moving content. Coefficient
        extraction pipeline is now trivially repeatable (grouped by
        CODED skip_flag markers in BINTRACE output).
        NEXT STEP remains: compare these parsed-coefficient reconstructions
        block-by-block against implied residual (r - pred) to pinpoint whether
        individual blocks or individual coefficients diverge, starting with
        MB(2,2)s equal-SAD signature (magnitudes right, signs/arrangement
        suspect).

      - **2026-08-23 session #25 - ROOT CAUSE LOCALIZED: the remaining cabac_b
        gap is a DEBLOCKING WEAK-FILTER difference, not CABAC.** Chain of proof:
        1. KINETIX_FORCE_MVD sweep on MB(1,1): forcing any nonzero mvd makes the
           whole MB wrong -> decoded mvd=(0,0) is correct; MVD path exonerated.
        2. The 4 diverging samples of MB(1,1) sit at local (5,14),(14,14),
           (5,15),(14,15) -- inside the modification zones of the interior
           v-edge x=20 (idx1) / h-edge y=28 (idx3) and the MB-boundary h-edge
           y=32 against Bi MB(1,2).
        3. bS trace for c_p8x8s B slice: MB(1,1) edges bs=[0]*4 (correct: no nz,
           identical mvs); boundary to Bi MB(1,2) bs=[0,2,2,2] (correct: MB(1,2)
           blocks 1-3 have nz 2/5/6); interior Bi edges bs=[2,2,2,2] (correct).
           bS DERIVATION IS CORRECT.
        4. c_p8x8_nd (no-deblock=1) decodes BIT-EXACT -> pre-deblock pixels are
           perfect.
        CONCLUSION: our weak-filter (bS<=2) execution differs from ffmpegs by
           +/-1-2 on certain sample patterns at qp~22-24 on B-slice inter edges
           (possibly also present-but-masked in P streams). The strong filter
           (bS=4) and bS derivation are fine. Suspects inside
           filter_luma_edge: dp/dq computation ((p2-p0)&(q2-q0) vs (p2-p0)
           variants), tC adjustment (`tc++` under specific delta conditions),
           or the delta-threshold comparisons (|p0-q0|<alpha, |p1-p0|<beta,
           |q1-q0|<beta).
        NEXT STEP: dump filter_luma_edge inputs/outputs (p0..p3,q0..q3,alpha,
           beta,tc,bs) for the failing edge and hand-compare against ffmpeg
           h264_loop_filter_luma line by line; fix the deviating branch.

      - **2026-08-23 session #26 - PRE-DEBLOCK ANALYSIS COMPLETE.** Pre-deblock
        pixel dump (KINETIX_DUMP_PREDEBLOCK env + .3 suffix for the B frame)
        shows pre == ours at EVERY diverging sample -> our deblocker is NOT
        the cause; the divergence exists BEFORE deblocking, i.e. in
        prediction or residual reconstruction itself.
        Refined understanding of c_p8x8 row-2 failures:
        - Parse is verbatim-correct vs ffmpeg source (sessions #19/#21).
        - References (I, P) are bit-exact.
        - Mode/MVD decode verified (Bi[0,0], forced-MVD sweep confirms
          (0,0) is right for MB(1,1)).
        => The remaining suspects: (a) our RESIDUAL COEFFICIENT VALUES for
           row-2 MBs differ from x264s (engine-state divergence entering the
           MB -- but rows 0-1 are clean...), or (b) the BI-PREDICTION COMBINE
           step in reconstruct_b_inter_luma differs subtly (e.g. weighted
           prediction handling, rounding), or (c) the colocated_mv grid fed
           into reconstruct/predict differs.
        NEXT STEP: dump our dequantised residual per 4x4 block for MB(1,2) and
           compare against implied residual r - avg(L0,L1) per sample; if they
           agree, the bug is in prediction; if they disagree, re-check
           coefficient->raster placement for inter MBs (scan order vs raster).

      - **2026-08-24 session #28 — QP init and skip-flag rule EXONERATED; the
         desync is an engine-state divergence invisible during the SKIP run.**
         Continued from #27 with new hard evidence:
         1. Parsed P-slice MB map (KINETIX_BINTRACE): MB0-7 SKIP, MB8=P8x8
            coded (PIXEL-EXACT vs ffmpeg), MB9=PL016x16 mvd=(0,1) cbp=0x03
            (FIRST pixel divergence @(20,32)=block1 which HAS residual),
            MB10=SKIP, MB11=PL0x16 mvd=(-1,20) cbp=0x00. The cbp=0x3 residual
            block set (blk0 cbf=false; blks 1-7 coded) IS self-consistent -
            the earlier "7 blocks with cbp=0x03" reading was wrong.
         2. Sharp MV oracle (dbg_skip_lf.rs): our MB(3,2)/MB(1,2) outputs are
            self-consistent MC at the decoded MVs (SAD 0..43). ffmpeg's
            corresponding blocks match NO MV from the I reference over
            +/-320 x +/-64 qpel, AND a full-picture integer-pel search finds
            nothing (min SAD 1517/64 samples). ffmpeg decodes with 0 errors.
            => ffmpeg's row-2 MBs are spatially predicted (intra-in-P) OR our
            engine state diverged before MB9 in a way pixels don't show.
         3. Slice-QP sweep (new KINETIX_FORCE_SLICE_QP[_B] debug overrides in
            decoder/mod.rs): forced qp 20..25 all still diverge (qp=24 ==
            baseline n=210/187/228). SliceQpY misinitialisation RULED OUT.
         4. mb_skip_flag contextIdxInc re-audited against ffmpeg's
            decode_cabac_mb_skip (h264_cabac.c:1336): both use "same-slice
            neighbour available AND not skipped" -> ctx 11+inc (B: +13).
            Identical. RULED OUT.
         KEY INSIGHT resolving the #27 engine-sync paradox: rows of SKIP MBs
            can decode identically under slightly-diverged engine state as
            long as the skip bins stay dominant, so the FIRST PIXEL divergence
            (MB9) need not be the first BIN divergence. Any state drift
            introduced earlier - e.g. during the 8 SKIP MBs or MB8 - flips the
            first low-probability decision. Candidates still open:
            (a) terminate-bin handling in the P path after SKIP MBs (the
                #12-era fix was applied to cabac_i; verify cabac_p/b read a
                terminate bin after EVERY MB incl. skips and match x264's
                exact write count);
            (b) the shared-ctxIdx-17 sync between MbTypePCabacContext and
                IntraMbTypeSuffixCabacContext (sync_shared_mb_type_ctx_*_p)
                corrupting state across the MB8 P8x8 decision;
            (c) MvStore/cells bookkeeping affecting nothing but bS (already
                fixed) - no longer suspect.
         NEXT STEP: add a per-bin engine-state hash to KINETIX_BINTRACE and
            diff OUR two decode passes? No - instead hand-walk the first 40
            bins of the P payload with ff_h264_cabac.c open, using
            tests/dbg_bintrace_replay.rs as the template, until the first
            decision differs from our trace. The payload is tiny (415-byte
            NAL, ~380 CABAC bytes); this is now a bounded mechanical task.
         ADDITIONAL ELIMINATIONS (same session):
         - Emulation-prevention bytes: P NAL contains ZERO 00-00-03 seqs
           (PowerShell scan); RBSP extraction cannot corrupt it. RULED OUT.
         - DPB store-vs-deblock ordering: ALL five decode paths in
           decoder/mod.rs (lines 547/828/1024/1287/1515) deblock BEFORE
           store_reference_picture (590/1068/1330); interlaced.rs likewise.
           References are post-deblock everywhere. RULED OUT.
         - P mb_type tree re-verified verbatim against fetched
           h264_cabac.c:2005-2020 (ctx14 intra gate; ctx15=0 -> mb_type=
           3*ctx16 i.e. 16x16/P8x8; else 2-ctx17 i.e. 8x16/16x8; intra ->
           decode_cabac_intra_mb_type(17, 0)). Identical.
         - Terminate-bin handling in cabac_p: read after EVERY MB incl.
           skips, early-eos errors, last-MB tolerated. Correct.
         - ffmpeg per-MB debug (-debug:v 32) prints nothing useful in release
           builds (ff_tlog compiled out); no oracle dump available from
           ffmpeg itself.
         FRAME-PAIRING NOTE for dbg_skip_lf users: our decoder emits decode
            order (nal#4=P emitted before nal#5=B, one frame per packet);
            ffmpeg rawvideo dumps display order. Verified empirically via
            swap-symmetric diff counts.
         INTRA-CONTINUITY PROBE result: ffmpeg's P row-2 pixels are NEITHER
            plain MC from I (MV oracles) NOR simple intra (flat 82 top rows
            but strong horizontal gradients mid-block; vertical/horizontal/
            DC candidates all fail) => most consistent with INTER MBs whose
            bins diverged inside MB9's RESIDUAL decode (ResidualCabacContext
            inter path - the one component never independently verified for
            inter blocks with this coefficient pattern), poisoning MB10's
            skip flag and MB11 wholesale. Sharpened next step: transcribe an
            independent oracle residual walker (sig-map + levels, ctx 105+,
            following ff_h264_cabac.c residual_coeff/coeff_token logic) into
            tests/dbg_p_oracle_replay.rs-style form, run it on the dumped
            c_p8x8 P payload from MB0, and diff bin-by-bin against our trace
            through MB9. First differing ORACLE line vs CRATE line is the bug.

      - **2026-08-24 session #27 — DEBLOCKING EXONERATED DEFINITIVELY; gap
         re-localized to intra-in-P / coded-inter MB parsing on the c_p8x8
         bitstream itself.** Method + results:
         1. `derive_bs_pair` rewritten as a verbatim transcription of ffmpeg's
            `check_mv` (h264_loopfilter.c): raw `LIST_NOT_USED` (-1) sentinel
            comparison implements the spec's "different number of motion
            vectors" clause (an L1-only block next to a Bi block now yields
            bS = 1), plus the mirrored-list equivalence check before returning.
            Applied unconditionally for P slices too (their cells always carry
            ref_idx_l1 == LIST_NOT_USED so it degenerates to the old result).
            Correct per spec and matches ffmpeg exactly (traced edge
            bs=[1,1,1,1] between BL116x16 MB(1,1) and BBi16x16 MB(1,2)).
         2. New `KINETIX_SKIP_DEBLOCK` env override in `deblock_luma_mb` /
            `deblock_chroma_mb` lets our pre-deblock pixels be compared against
            **`ffmpeg -skip_loop_filter all`** output on the SAME bitstream —
            something sessions #12-#26 could never do (they compared against
            ffmpeg's *deblocked* frames only). New harness:
            `tests/dbg_skip_lf.rs`.

      - **2026-08-24 session #28 — P-slice CABAC mb_type tree audit COMPLETE:
         tree EXONERATED (empirically, not just by eyeball).** Method:
         FFmpeg's exact `AV_PICTURE_TYPE_P` branch (`ff_h264_decode_mb_cabac`:
         ctx14 intra gate; ctx15=0 -> 3*ctx16; else 2-ctx17) AND
         `decode_cabac_intra_mb_type(ctx_base, intra_slice)` — including its
         pointer arithmetic (`state += 2` only on the intra_slice branch; the
         `state[2+intra_slice]` / `state[3+intra_slice]` /
         `state[3+2*intra_slice]` folds that make the P/B suffix REUSE ctx
         17+2 for the cbp_chroma *value* bin and ctx 17+3 twice for both
         pred_mode bins) — were transcribed verbatim onto a flat 1024-entry
         context array indexed by absolute spec ctxIdx, then run in LOCKSTEP
         against the crate's `MbTypePCabacContext` /
         `IntraMbTypeSuffixCabacContext` pair (with the same unconditional
         prefix->suffix / suffix->prefix shared-ctx17 syncs that
         `cabac_b.rs::parse_p_macroblock_cabac` performs) over pseudo-random
         payloads: 3 cabac_init_idc values x 4 QPs x 8 seeds x up to 200 MBs
         each. Two new tests in `entropy.rs::tests` assert BOTH per-element
         value equality AND final adapted-state equality of every touched
         context variable (ctxIdx 14..=20):
         `p_mbtype_tree_differential_vs_ffmpeg_transcription` and
         `i_slice_mbtype_differential_vs_ffmpeg_transcription` (the latter
         covers the I-slice variant, ctx_base=3/intra_slice=1, cycling all
         four bin-0 ctxIdxInc patterns).
         RESULT: all pass — the P mb_type tree, the intra-in-P suffix
         (including its context-reuse quirks), the shared ctxIdx-17 sync
         direction, and the I-slice mb_type tree are bit-for-bit identical to
         FFmpeg across every randomized stream tried.
         CONSEQUENCE: the "intra-in-P (mb_type>=5) misparse" theory from
         session #27 is now RULED OUT at the syntax-element level. The
         remaining row-2 gap on c_p8x8 must live in one of:
           (a) the inter residual walk (`decode_inter_residual_cabac` /
               `ResidualCabacContext`) — still the only major component never
               independently verified with this coefficient pattern (session
               #25's leading theory),
           (b) reconstruction of intra MBs inside P slices
               (`parse_intra_mb_cabac_pb`'s downstream neighbour-context/MPM
               handling vs ffmpeg's decode_intra_mb), or
           (c) sub_mb_type/ref_idx/mvd context derivation for P_8x8 MBs whose
               neighbours are intra.
         NEXT STEP: extend the same lockstep-oracle technique past mb_type
         into the residual path — transcribe ffmpeg's
         residual_coeff/coeff_token walk (sig-map + levels, ctx 105+,
         cat-specific bases incl. the 8x8 indirection tables) onto the flat
         oracle array and diff bin-by-bin against
         `decode_inter_residual_cabac` on the c_p8x8 P payload through MB9
         (session #26's prescription, now unblocked by this audit). The
         FlatOracle scaffolding in `entropy.rs::tests` is reusable for it.
         ALSO THIS SESSION: fixed 3 pre-existing clippy `-D warnings`
         failures in `tests/dbg_skip_lf.rs` (`map_or` -> `is_none_or`) that
         would have failed the CI clippy job.

      - **2026-08-24 session #29 — residual-path lockstep audit: REAL BUG
         FOUND AND FIXED (shared chroma level context).** Extended the
         FlatOracle lockstep technique into `decode_cabac_residual_internal`:
         new verbatim transcriptions of the significance-map walk
         (`DECODE_SIGNIFICANCE`), the STORE_BLOCK level loop, the node_ctx
         maps, and FFmpeg's absolute ctxIdx bases (sig {105,120,134,149,152,
         402}, last {166,181,195,210,213,417}, level {227,237,247,257,266,
         426}) now live in `entropy.rs::tests` alongside two permanent
         differential tests:
         `residual_block_differential_vs_ffmpeg_transcription` (cats 0..=4,
         4 QPs x 8 seeds x 300 blocks) and
         `residual_block_8x8_differential_vs_ffmpeg_transcription` (cat 5).
         **BUG**: `ResidualCabacContext` stored `coeff_abs_level_minus1`
         contexts as five per-category `[CabacContext; 10]` arrays — but spec
         Table 9-42 / ffmpeg's `coeff_abs_level_m1_offset` make ChromaDC's
         highest context (cat3 base 257 + inc 9 = ctxIdx 266) the SAME
         physical variable as ChromaAC's lowest (cat4 base 266 + inc 0). Our
         split arrays adapted two independent copies, so any CABAC slice
         whose chroma levels exercised both boundary contexts diverged from
         a conformant decoder (state drift found at ctxIdx 266 on random
         streams within one seed; bin values can stay equal for a while,
         which is why pixel-level symptoms look like tiny coefficient
         differences). **FIX**: `level` is now ONE flat Vec indexed by
         absolute ctxIdx - 227 (cats 0..=4 jointly occupy 227..=275), used by
         both `ResidualCabacContext::new` and `new_pb`; sig/last arrays have
         no overlaps and are unchanged.
         AUDIT NOTES: (a) the cats 0..=4 walk, node_ctx maps, level tables
         ({5,5,5,5,6,7,8,9} etc.), escape arithmetic (`15 + EG0 ==
         ffmpeg's `(1<<j)+bits+14`), and the 8x8 SIG indirection table are
         all bit-identical to FFmpeg; (b) the 8x8 differential initially
         failed due to a bug in MY oracle transcription (implicit-tail test
         written as `last == 62` instead of ffmpeg's `last == max_coeff-1`
         == 63), not in the crate — fixed in the oracle; (c) FFmpeg caps its
         escape prefix at 23 ones as a DoS guard while `decode_bypass_eg`
         caps at 32; the two differ only on non-conformant garbage (levels
         >= 2^23+14 cannot occur in valid streams) and the tests pin the
         crate's convention deliberately.
         All conformance suites re-run green after the fix (cabac I/P/B,
         high-profile 8x8 CABAC, CAVLC P/B — all bit-exact). NEXT STEPS:
         re-run `dbg_skip_lf` / c_p8x8 pixel comparisons to measure whether
         the shared-ctx266 fix closes part of the row-2 gap (it plausibly
         explains the "tiny coefficient difference"-class symptoms from
         sessions #22-#24); if the gap persists, continue with (b)/(c) from
         session #28's list.
         3. RESULT on c_p8x8 (deblocking enabled): I frame pre-deblock ==
            ffmpeg pre-deblock EXACTLY (and post-deblock bit-exact). But the P
            and B frames' PRE-deblock pixels diverge (n~1230/frame), confined
            to MB row 2. NOTE: our decoder emits decode order (I,P,B) while
            ffmpeg's rawvideo dump is display order (I,B,P) — pair
            ours[1]<->ff[2] (P) and ours[2]<->ff[1] (B) or the diffs look
            swapped. c_p8x8_nd remains fully bit-exact because x264 makes
            different MB choices when RD accounts for the loop filter.
         4. Brute-force MV oracle on P-frame MB(3,2) (cbp=0 => pure MC): OUR
            quadrants are reproduced by single MVs around the decoded mvd
            (-1,20)+predictor, but ffmpeg's quadrants match NO MV from the I
            reference (min SAD 1219-1583 over +/-128 qpel) => ffmpeg decoded
            that MB as something OTHER than plain L0-inter-with-cbp0 — almost
            certainly INTRA-IN-P (mb_type >= 5 -> I_16x16/I_4x4/I_PCM inside a
            P slice), which we misparse as inter (or as a different inter type,
            dropping its residual). First divergence: P-frame MB(1,2)
            @(20,32); MB(1..3,2) all diverge, MB(0,2) is fine.
         CONCLUSION: the long-standing "row-1/row-2 +/-1-2 residual gap" was
            TWO stacked issues: (a) a real bS=1 derivation bug (fixed this
            session via the check_mv transcription), and (b) misparsing of
            intra-in-P (probably also some coded-inter) MBs in streams where
            x264 actually uses them — invisible in every previous repro clip
            (b_swap/b_forcel1/b_default/c_p8x8_nd all avoid those mb_types).
         NEXT STEP: instrument the P-slice CABAC mb_type tree for MB(1,2) of
            c_p8x8 (KINETIX_BINTRACE already dumps per-MB context states);
            hand-walk the first bins against ff_h264_cabac.c
            `decode_cabac_mb_type`'s P branch (ctxIdx 14..17, intra suffix at
            ctxIdxOffset 32) and check whether our tree classifies mb_type>=5
            (intra-in-P) correctly, including the I_16x16 CBP/qp_delta handling
            that follows. Then re-run dbg_skip_lf: target is
            `PRE-DEBLOCK MATCH: true` on all 3 frames.

      - **2026-08-24 session #30 — post-fix re-run + REFINED DIAGNOSIS:
         evidence now points at MV-PREDICTOR / REFERENCE-LIST mismatch, not
         mb_type or residual parsing.**
         Re-ran `dbg_skip_lf` after the ctx266 fix: gap unchanged (I exact;
         P and B frames each diverge in MB row 2, n~187-228 per MB).
         NEW SYNTHESIS of all session evidence:
           - The CABAC mb_type trees (#28) and the whole residual walk
             (#29) are now PROVEN bit-identical to FFmpeg, and every
             element boundary through MB8 stays in lockstep (MB8 pixel-
             exact incl. its P8x8 sub_mb/ref_idx/mvd/cbp/dqp/residual).
           - MB(3,2) has cbp=0 (pure MC) yet is wholesale wrong, while OUR
             own reconstruction is perfectly consistent with OUR parsed
             MV (-1,20)-family (best-vs-ours SAD 0..43). ffmpeg's version
             matches NO integer MV into the deblocked I (SAD>=1219).
         KEY INSIGHT: mvd bins are context-selected by NEIGHBOUR mvd SUMS
           (amvd), NOT by the resulting MV. A wrong MV PREDICTOR (or wrong
           reference-list entry the MV points into) therefore changes the
           decoded MV VALUES while consuming IDENTICAL bins -- the parse
           never desyncs, later MBs stay 'consistent', and only pixels
           diverge. This fits every observation since session #11.
         PRIME SUSPECTS (in order):
           (a) MVP median-predictor inputs (§8.4.1.1): neighbour MV
               availability/scaling for non-reference or field pictures,
               especially across the row1->row2 boundary where diffs start;
           (b) RefPicList0 CONTENT (§8.2.4.2): with ref=1, x264 uses two
               refs; if our list ORDER differs from ffmpeg's (PicNum vs POC
               tie-breaks), identical mvds yield different reference
               pictures => wholesale pixel diffs with clean parsing;
           (c) B-slice L1 list + spatial-direct colZeroFlag derivation for
               the B frame's row 2.
         NEXT STEP: instrument mv.rs::predict_slice_mvs to dump the
           predictor + neighbours for each partition of MBs (1,2)/(3,2),
           and independently hand-compute §8.4.1 medians from the parsed
           neighbour MVs; separately print our RefPicList0 entries' buffer
           IDs/POCs vs ffmpeg's `-debug` ref info. First mismatched input
           is the bug. (The lockstep-oracle discipline cannot catch this
           class: it lives BETWEEN syntax elements, in derived data.)
         SESSION #30 ADDENDUM -- MVP TRACE INSTRUMENTATION LANDED:
           - mv.rs::predict_mv now prints A/B/C candidates + chosen
             predictor per partition when KINETIX_BINTRACE is set (all
             16x8/8x16 shortcut branches preserved; lib tests + clippy OK).
           - New harness tests/dbg_mvp_trace.rs regenerates the c_p8x8 IBP
             clip and dumps the trace.
           - First data point (P slice): mb9=(1,2) 16x16 ref0:
             A=Some((0,-20) ri0) [MB(0,2) top-right 8x8, mvd=(0,-20)],
             B=Some((0,0) ri0) [MB(1,1) SKIP], C=Some((0,0) ri0)
             [MB(2,1) SKIP] -> median (0,0); decoded mvd (0,1) => mv (0,1).
             match_count=3, no shortcut; derivation LOOKS spec-correct for
             those inputs. NEXT SESSION:
             (1) hand-verify MB(0,2)'s sub-MV chain from row-1 skips;
             (2) dump our RefPicList0 buffer/POC ids vs ffmpeg -debug to
                 rule out ref-list-order mismatch (suspect b);
             (3) CHECK A LIKELY REAL BUG: in the B-slice trace neighbours
                 appear as Some(mv=(0,0) ri=-1) -- L1-only neighbours must
                 be treated as UNAVAILABLE for L0 prediction per spec
                 8.4.1.1, i.e. they should NOT enter median3 at all (only
                 the special A-with-B,C-unavailable rule may use them).
                 Our median_pred currently feeds them into the median with
                 zero MVs, which can silently corrupt predictors in B
                 slices whenever a neighbour was coded L1-only or direct.

      - **2026-08-24 session #31 — REFLIST dump + P_8x8 MVP-SUB trace landed;
         suspects (a)/(b) narrowed; ri=-1 question resolved as spec-correct.**
         Landed:
           - `ref_pic.rs::trace_ref_list` + calls at both ref-list build sites
             in `decoder/mod.rs` (P path "P L0", B paths "B L0"/"B L1"),
             KINETIX_BINTRACE-gated: prints index/pic_num/frame_num/POC/
             short-long status per entry.
           - `mv.rs::predict_mv_sub` now prints the same A/B/C -> predictor
             trace as `predict_mv` ("MVP-SUB mb8 sub(px,py spww) ...").
           - `tests/dbg_mvp_trace.rs` extended: ffmpeg reference YUV decode +
             per-MB luma diff map for all three frames (decode/display order
             pairing ours[1]<->ff[2] (P), ours[2]<->ff[1] (B)).
         RESULTS:
           1. REFLIST: P RefPicList0 = [frame_num=0 poc=0] (single entry);
              B L0 = [I poc=0], B L1 = [P poc=4]. With b-pyramid=0 and one
              prior picture there is NO ordering freedom => SUSPECT (b)
              REF-LIST-ORDER MISMATCH IS RULED OUT for c_p8x8.
           2. MB(0,2)/mb8 sub-MV chain hand-verified from the MVP-SUB trace:
              sub(0,0)=(0,0) from row-1 skips; sub(8,0) final (0,-20) (its own
              mvd); sub(0,8): match_count=1 shortcut takes B=(0,0) (C is the
              intra-MB already-decoded block 6 = (0,-20), correctly read from
              `cur` per 6.4.11.7); sub(8,8): median((0,0),(0,-20),(0,0))=(0,0).
              Feeds mb9's A=(0,-20). Every step follows 8.4.1.3.1/.2 given its
              inputs => SUSPECT (a) WEAKENED (derivation correct; only input
              correctness via parse remains).
           3. ri=-1 question RESOLVED — NOT a bug: per 8.4.1.3.1 a neighbour is
              unavailable only if intra/unavailable; partitions predicted from
              the other list are available but never match refIdx, and their
              current-list MV is 0 by 8.4.1.2 — so Some((0,0) ri=-1) entering
              median3 with zeros matches the spec (and ffmpeg's ff_pred_motion
              convention of zero-filling non-matching candidates). No change.
           4. NEW per-MB diff map (post-deblock, deblock still ACTIVE on both
              sides since x264 deblock=0 does not set disable_deblocking_
              filter_idc): P frame diverges WHOLESALE at MB(1,2) n=209/256
              max=121, MB(2,2) n=181 max=71, MB(3,2) n=227 max=151 — NOT
              confined to residual-carrying blocks — plus small new row-1
              diffs MB(1,1) n=4 max=2 / MB(3,1) n=8 max=3 (most plausibly
              deblock propagation across the row1/row2 boundary). B frame has
              the same shape (MB(1..3,2) ~204-220 samples, small row-1/MB(0,2)
              n=8-11 diffs).
         REVISED INTERPRETATION: wholesale-MB divergence with a single-entry
            ref list kills BOTH the "wrong predictor" and "wrong ref picture"
            theories as the PRIMARY cause. Remaining live hypotheses:
              (i) mid-slice CABAC bin-consumption desync starting at/inside
                  mb8 (a desync still parses coherently — downstream sanity
                  proves nothing; c_p8x8_nd bit-exactness just means x264's
                  RD-with-deblock choices avoid the trigger),
              (ii) motion-compensation error (sub-pel interpolation or MV
                  application) on these specific partitions,
              (iii) residual application bug whose magnitude dominates whole
                  MBs (cbp=0x03 on mb9 makes blocks 1-2 suspect, but cbp=0
                  MB(3,2) diverging wholesale argues against this alone).
         NEXT STEPS (in order):
              1. Extend the FlatOracle lockstep walk over the FULL real
                 c_p8x8 P payload (mb_type/sub_mb_type/ref_idx/mvd/cbp/dqp per
                 MB through end-of-slice), diffing against KINETIX_BINTRACE on
                 identical bytes (reuse dbg_bintrace_replay scaffolding) — this
                 decides (i) definitively.
              2. If parse proves lockstep-clean: brute-force SAD over ALL qpel
                 MVs into the reconstructed I for cbp=0 MB(3,2) USING OUR OWN
                 MC code vs ffmpeg pixels — distinguishes (ii) from an mvd
      - **2026-08-24 session #31 part 2 — FULL-SLICE LOCKSTEP ORACLE LANDED:
         parse EXONERATED at syntax level; residual visit-order question
         framed; remaining gap is downstream.** Implemented the prescribed
         full-payload lockstep walk:
           - `KINETIX_DUMP_P_PATH` dumps the real c_p8x8 P CABAC payload
             (+ .meta) from decoder/mod.rs (mirrors the B-path dump).
           - `entropy.rs::tests::p_slice_full_walk_lockstep_vs_ffmpeg_
             transcription_c_p8x8` embeds the 406-byte real P payload
             (qp=24 idc=0 nl0=1 t8=off, 4x3 MBs) and replays it through a
             verbatim ff_h264_decode_mb_cabac P-branch transcription: skip
             flag (ctx 11+), p_branch/intra_mb_type(17,0), sub_mb_type
             (states 21-23), mvd (bases 40/47, amvd from |mvd| caches capped
             at 70 — ffmpeg's *mvda stores ABS magnitudes), cbp luma/chroma
             (states 73+/77+; off-picture neighbour sentinel 0x00F per
             FFmpeg fill_decode_caches for INTER MBs), dqp (60+ctx),
             coded_block_flag (base_ctx {85,89,93,97,101,...}) + the already
             differentially-verified residual transcriptions, terminate after
             every MB. Compares skip/cbp/qp/mvds/nnz per MB vs
             `parse_p_slice_cabac`.
           - RESULT AFTER ORACLE CALIBRATION: **full lockstep on all 12 MBs**.
             Element parsing, context selection and engine evolution of our
             CABAC P parser are bit-faithful to the ffmpeg transcription on
             the real failing payload => hypothesis (i) mid-slice desync is
             RULED OUT (for the P slice; B slice presumably follows).
           - Oracle calibration notes (bugs found in MY oracle, not crate):
             (a) amvd must sum ABS mvd magnitudes (ffmpeg's *mvda), signed
                 sums pick wrong ctx on negative sums;
             (b) amvd neighbours are the spec sample rule — left = partition
                 containing (xP-1,yP+hP-1), top = (xP+wP-1,yP-1) — not the
                 cells directly above/left of the partition origin;
             (c) off-picture cbp sentinel is 0x00F (chroma bits CLEAR) for
                 inter MBs, matching decode_inter_cbp_cabac's comment.
           - RESIDUAL VISIT ORDER EXPERIMENT: temporarily switched
             decode_inter_residual_cabac to plain raster block order
             ([0..15]); this REGRESSED the conformance matrix (cabac_p/b
             cells were bit-exact before!) and made c_p8x8 MB(0,2) pixel-
             wrong. REVERTED. So the group-by-group order ([0,1,4,5 |
             2,3,6,7 | ...]) is empirically correct for real streams even
             though the vendored ff_h264_cabac.c decode_cabac_luma_residual
             loop reads like plain raster (`index = 4*i8x8+i4x4`) — apparent
             conflict unresolved, recorded here. A permanent lockstep test
             now pins both sides to the group order.
         CONSEQUENCE: with element parsing exonerated, the c_p8x8 row-2 P/B
            pixel gap (wholesale diffs at MB(0..3,2)) must live in:
              (a) the residual LEVEL/scan semantics as applied to REAL
                  payloads (the random-stream differentials may miss a
                  real-payload-specific path, e.g. cat-5 8x8 or chroma DC
                  edge cases), or
              (b) reconstruction/MC/deblocking downstream of the parse.
            NOTE the diff map shows the gap is NOT confined to residual-
            carrying blocks, and mb11 (cbp=0, pure MC) diverges wholesale —
            keep (b) MC/sub-pel as prime suspect, or an MV-store divergence
            that only manifests with non-trivial mvds upstream (mb8's
            (0,-20)).
         NEXT STEPS: (1) run dbg_mvp_trace's qpel brute force (prescribed
            earlier) against the post-fix build to decide (b); (2) extend
            the same lockstep technique to the B slice payload.
         ALSO THIS SESSION: temporary `[profile.dev.package.tpt-kinetix-h264]
         codegen-units = 1` in root Cargo.toml works around a reproducible
         lld-link "undefined symbol: anon.*" cross-CGU link failure for the
         h264 lib-test binary on this machine; remove when toolchain fixed.


                 misparse (session #25's oracle was ad hoc and pre-dates the
                 bS/deblock fixes; re-run it against post-deblock references).


## SESSIONS #12-#26 SUMMARY — CABAC B-FRAME INVESTIGATION COMPLETE

### What was accomplished
Two real decoder bugs found and fixed:
1. B_Direct_16x16 dropped CBP/qp_delta/residual bins (cabac_b.rs) ->
   b_default B frame now BIT-EXACT; conformance matrix turned green.
2. B sub_mb_type tables were mis-transcribed (mv.rs) -> corrected to spec
   Table 9-16 layout (0=Direct, 1-4=L0(1/2/2/4), 5-8=L1(1/2/2/4), 9-12=Bi).

Six hypotheses conclusively disproven with evidence:
- L1-separate MVD contexts (session #15)
- H1 mb_type tree variant: ctxIdx-31 Bi shortcut (session #17)
- V-B mb_type tree variant: all-ext-bins at ctxIdx 32 (session #18)
- Slice-QP misdecode: qp=24 uniquely produces bi16=2 (session #21)
- Deblocking weak-filter difference: pre==ours at ALL diverging samples (#26)
- MVD misdecode on BL116x16 MB(1,1): forced sweep confirms (0,0) is correct (#25)

### Parse verified against vendored ffmpeg source (ff_h264_cabac.c)
- MbTypeBCabacContext tree: VERBATIM-CORRECT (lines 1977-1997)
- MvdCabacContext: ctxbase=(l==0)?40:47 shared across lists, thresholds,
  unary loop bounds, EGk bypass suffix, sign bit -- all identical
- MbQpDeltaCabacContext: val&1 mapping identical
- Element order for inter MBs: refs -> mvds -> cbp -> transform8x8 -> dqp ->
  residual -- confirmed

### Infrastructure added
- KINETIX_BINTRACE=1 per-bin tracing with global ctx indices
- KINETIX_DUMP_PREDEBLOCK pre-deblock pixel dump (frame-count suffixed)
- KINETIX_DUMP_B_PATH full B-slice CABAC payload dump
- KINETIX_FORCE_MVD debug override for specific MB mvd values
- tests/dbg_bintrace_replay.rs: crate-engine P-slice replay
- tests/dbg_b_qp_sweep.rs: 52-qp exhaustive parse validation
- dbg_cabac_b.rs: deterministic single-threaded encode + per-MB divergence
  report + residual-source discriminator + small-diff sample dumper

### Remaining known gaps (deterministic c_p8x8 repro available)
1. Isolated +/-1-2 sample diffs in row-1/row-2 MBs (4+2+8+206+183+220
   samples total). First divergence: MB(1,1) BL116x16 local (5,14) delta=+1.
   Root cause: subtle coefficient or nz/cbf context-state divergence.
2. Phase G: MBAFF P/B parsing (mb_field_decoding_flag for P/B slices,
   neighbour derivation for mixed field/frame pairs)
3. Phase G.5: PAFF/MBAFF corpus clips for interlaced validation
4. Phase H: pixel_exact flip (requires items 1-3 above plus ITU vectors)

### KEY INSIGHT FOR NEXT SESSION
MB(1,1) BL116x16 mv=[0,0]: our output == P frame exactly at ALL 256 samples;
ffmpeg differs from P by SAD=6 at 4 samples and matches I frame exactly.
This means ffmpeg predicted from L0 (=I) while we predicted from L1 (=P).
Either the reference lists are swapped/differently ordered, OR ffmpeg decoded
a different mb_type due to context-state divergence entering this MB.
Check build_ref_list_l0_b_slice and build_ref_list_l1 ordering for the
specific DPB state after decoding I(frame_num=0) and P(frame_num=1).
- PPS correctly parsed as ntropy_coding_mode_flag=false (CAVLC). The PAFF path returns Fallback for most fields, causing the main loop to fall through to the progressive try_decode_real_slice path, which then fails because it expects progressive (non-field) input.
